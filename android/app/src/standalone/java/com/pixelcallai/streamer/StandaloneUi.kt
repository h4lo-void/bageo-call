// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.app.ActivityManager
import android.app.NotificationManager
import android.app.role.RoleManager
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.graphics.Color
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.os.Process
import android.os.SystemClock
import android.provider.Settings
import android.util.Base64
import android.util.Log
import android.view.HapticFeedbackConstants
import android.view.View
import android.webkit.DownloadListener
import android.webkit.JavascriptInterface
import android.webkit.URLUtil
import android.webkit.ValueCallback
import android.webkit.WebChromeClient
import android.webkit.WebResourceRequest
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.Toast
import android.window.OnBackInvokedCallback
import android.window.OnBackInvokedDispatcher
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.net.HttpURLConnection
import java.net.URI
import java.net.URL

/**
 * Pure mobile App host for Android Standalone edition:
 * - Full-screen WebView serving `/calls/mobile` (`standalone.html`) with zero desktop/hybrid chrome.
 * - Direct `@JavascriptInterface` (`window.BageoAndroid`) bridging single-phone AI mode switching,
 *   Google Phone / native dialpad launch, permission & role setup, native contacts sync,
 *   local mobile preferences, LAN sharing, haptics, file picker/export, and dynamic system bars.
 */
class StandaloneUi(private val activity: Activity) {
    private lateinit var rootView: View
    private lateinit var webView: WebView
    private lateinit var loadingOverlay: View

    private val handler = Handler(Looper.getMainLooper())
    private var pendingOpenCallId: String? = null
    private var pendingAutoAiAfterPerms = false
    private var pendingStarredToWhitelist = true
    private var embeddedSettingsSynced = false
    private var consoleLoaded = false
    private var pageReady = false
    private var modeErrorCode: String? = null
    private var modeErrorDndMode = ""
    private var grantRequestedAt = 0L
    private var grantRationaleBefore: Map<String, Boolean> = emptyMap()
    private var backCallback: Any? = null

    private var fileUploadCallback: ValueCallback<Array<Uri>>? = null
    private var pendingExport: PendingExport? = null

    private data class PendingExport(
        val stagedFile: File?,
        val downloadUrl: String?,
        val filename: String,
        val mimeType: String,
    )

    private val modeListener = PhoneModeManager.Listener {
        activity.runOnUiThread {
            if (!activity.isDestroyed) pushNativeState()
        }
    }

    private val pollRunnable = object : Runnable {
        override fun run() {
            if (activity.isDestroyed) return
            checkEmbeddedReady()
            pushNativeState()
            handler.postDelayed(this, if (consoleLoaded) TICK_MS else POLL_READY_MS)
        }
    }

    private val runtimePermissions: List<String> by lazy {
        val items = mutableListOf(
            Manifest.permission.RECORD_AUDIO,
            Manifest.permission.READ_PHONE_STATE,
            Manifest.permission.READ_PHONE_NUMBERS,
            Manifest.permission.READ_CALL_LOG,
            Manifest.permission.ANSWER_PHONE_CALLS,
            Manifest.permission.CALL_PHONE,
        )
        if (Build.VERSION.SDK_INT >= 33) {
            items += Manifest.permission.POST_NOTIFICATIONS
        }
        items
    }

    private val privilegedPermissions = listOf(
        "android.permission.CAPTURE_AUDIO_OUTPUT",
        "android.permission.MODIFY_PHONE_STATE",
        "android.permission.CONTROL_INCALL_EXPERIENCE",
    )

    @SuppressLint("SetJavaScriptEnabled")
    fun onCreate(savedInstanceState: Bundle?) {
        activity.setContentView(R.layout.activity_standalone)
        rootView = activity.findViewById(R.id.saRoot)
        webView = activity.findViewById(R.id.saWebView)
        loadingOverlay = activity.findViewById(R.id.saLoading)

        applySystemBarInsets(rootView)
        syncSystemBarsWithSystemNightMode()
        registerBackNavigation()
        configureWebView()

        PhoneModeManager.reconcileLocalMirror(activity)
        PhoneModeManager.syncFromRoot(activity) {}
        EmbeddedAgent.ensureStarted(activity)
        handleLaunchIntent(activity.intent)

        if (Prefs.isAiCallEnabled(activity)) {
            if (hasRequiredPerms()) {
                DialerRoleHelper.ensureRoles(activity)
                CallAudioService.ensureArmed(activity)
            }
        } else if (Prefs.isStandaloneLanEnabled(activity) && hasRequiredPerms()) {
            CallAudioService.ensureArmed(activity)
        }

        checkEmbeddedReady()
    }

    fun onNewIntent(intent: Intent) {
        handleLaunchIntent(intent)
    }

    fun onStart() {
        LocalAudioGuard(activity).setConsolePreviewAllowed(true)
        if (::webView.isInitialized) {
            webView.onResume()
            webView.resumeTimers()
        }
    }

    fun onStop() {
        LocalAudioGuard(activity).setConsolePreviewAllowed(false)
        if (::webView.isInitialized) {
            webView.onPause()
            webView.pauseTimers()
        }
    }

    fun onResume() {
        PhoneModeManager.reconcileLocalMirror(activity)
        PhoneModeManager.addListener(modeListener)
        if (Prefs.isAiCallEnabled(activity) && hasRequiredPerms()) {
            val screeningHeld =
                activity.getSystemService(RoleManager::class.java)?.isRoleHeld(RoleManager.ROLE_CALL_SCREENING) == true
            if (!DialerRoleHelper.isDefaultDialer(activity) || !screeningHeld) {
                PhoneModeManager.syncFromRoot(activity) {}
            }
            if (!isServiceRunning()) {
                CallAudioService.ensureArmed(activity)
            }
        }
        handler.removeCallbacks(pollRunnable)
        handler.post(pollRunnable)
    }

    fun onPause() {
        PhoneModeManager.removeListener(modeListener)
        handler.removeCallbacks(pollRunnable)
    }

    fun onDestroy() {
        unregisterBackNavigation()
        handler.removeCallbacks(pollRunnable)
        fileUploadCallback?.onReceiveValue(null)
        fileUploadCallback = null
        if (::webView.isInitialized) {
            webView.destroy()
        }
    }

    private fun registerBackNavigation() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            val cb = OnBackInvokedCallback {
                if (!handleBack()) {
                    activity.finish()
                }
            }
            activity.onBackInvokedDispatcher.registerOnBackInvokedCallback(
                OnBackInvokedDispatcher.PRIORITY_DEFAULT,
                cb,
            )
            backCallback = cb
        }
    }

    private fun unregisterBackNavigation() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            (backCallback as? OnBackInvokedCallback)?.let {
                try {
                    activity.onBackInvokedDispatcher.unregisterOnBackInvokedCallback(it)
                } catch (_: Exception) {
                }
            }
            backCallback = null
        }
    }

    fun onBackPressed(): Boolean = handleBack()

    private fun handleBack(): Boolean {
        if (!consoleLoaded || !::webView.isInitialized) return false
        webView.evaluateJavascript(
            "(function(){try{return window.__bageoHandleBack?!!window.__bageoHandleBack():false;}catch(e){return false;}})()",
        ) { raw ->
            if (raw != "true") {
                if (webView.canGoBack()) {
                    webView.goBack()
                } else {
                    activity.finish()
                }
            }
        }
        return true
    }

    fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray,
    ) {
        if (requestCode == REQ_GRANT_ALL && permissions.isNotEmpty()) {
            val answeredInstantly = SystemClock.elapsedRealtime() - grantRequestedAt < INSTANT_DENIAL_MS
            val blocked = permissions.filterIndexed { index, permission ->
                grantResults.getOrNull(index) != PackageManager.PERMISSION_GRANTED &&
                    !activity.shouldShowRequestPermissionRationale(permission) &&
                    (grantRationaleBefore[permission] == true || answeredInstantly)
            }
            if (blocked.isNotEmpty()) {
                Toast.makeText(activity, R.string.msg_perm_settings, Toast.LENGTH_LONG).show()
                launch(
                    Intent(
                        Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
                        Uri.parse("package:${activity.packageName}"),
                    ),
                )
            }
        }
        if (requestCode == REQ_CONTACTS) {
            if (PhoneContactsSync.hasPermission(activity)) {
                runContactsSync(pendingStarredToWhitelist)
            } else {
                Toast.makeText(activity, R.string.sa_contacts_perm_denied, Toast.LENGTH_SHORT).show()
                pushContactsSyncResult(PhoneContactsSync.SyncResult(ok = false, error = "no_permission"))
            }
            pushNativeState()
            return
        }
        if (pendingAutoAiAfterPerms) {
            pendingAutoAiAfterPerms = false
            if (hasRequiredPerms()) {
                startModeSwitch(Prefs.PHONE_MODE_AI)
                return
            }
        }
        if (Prefs.isAiCallEnabled(activity) && hasRequiredPerms() && !isServiceRunning()) {
            CallAudioService.ensureArmed(activity)
        }
        pushNativeState()
    }

    fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        if (requestCode == REQ_FILE_CHOOSER) {
            val cb = fileUploadCallback
            fileUploadCallback = null
            if (cb != null) {
                cb.onReceiveValue(parseChooserResult(resultCode, data))
            }
            return
        }
        if (requestCode == REQ_CREATE_DOC) {
            val job = pendingExport
            pendingExport = null
            val uri = if (resultCode == Activity.RESULT_OK) data?.data else null
            if (job == null || uri == null) {
                job?.stagedFile?.delete()
                return
            }
            finishPendingExport(job, uri)
            return
        }
        pushNativeState()
    }

    // ---------------------------------------------------------------- WebView & JS Bridge

    @SuppressLint("SetJavaScriptEnabled")
    private fun configureWebView() {
        webView.setBackgroundColor(activity.getColor(R.color.bg_page))
        webView.settings.apply {
            javaScriptEnabled = true
            domStorageEnabled = true
            mediaPlaybackRequiresUserGesture = false
            allowFileAccess = false
            allowContentAccess = false
            textZoom = 100
            mixedContentMode = WebSettings.MIXED_CONTENT_NEVER_ALLOW
        }
        webView.addJavascriptInterface(MobileAppJsBridge(), "BageoAndroid")
        webView.webViewClient = object : WebViewClient() {
            override fun onPageFinished(view: WebView?, url: String?) {
                super.onPageFinished(view, url)
                if (!pageReady && url != null && url.contains("/calls/mobile")) {
                    pageReady = true
                    hideSplash()
                    pushNativeState()
                }
            }

            override fun shouldOverrideUrlLoading(
                view: WebView?,
                request: WebResourceRequest?,
            ): Boolean {
                val uri = request?.url ?: return false
                if (isLocalConsoleUri(uri)) {
                    val path = uri.path.orEmpty()
                    if (path.endsWith(".ics") || path.endsWith(".csv") || path.endsWith(".vcf")) {
                        val name = uri.lastPathSegment ?: "download"
                        startUrlExport(uri.toString(), name, "application/octet-stream")
                        return true
                    }
                    return false
                }
                val scheme = uri.scheme?.lowercase().orEmpty()
                if (scheme == "tel") {
                    val num = uri.schemeSpecificPart.orEmpty()
                    openDialerOrPhoneApp(num)
                    return true
                }
                if (scheme in setOf("http", "https", "mailto")) {
                    launch(Intent(Intent.ACTION_VIEW, uri))
                    return true
                }
                return true
            }
        }
        webView.webChromeClient = object : WebChromeClient() {
            override fun onShowFileChooser(
                webView: WebView?,
                filePathCallback: ValueCallback<Array<Uri>>?,
                fileChooserParams: FileChooserParams?,
            ): Boolean {
                this@StandaloneUi.fileUploadCallback?.onReceiveValue(null)
                this@StandaloneUi.fileUploadCallback = filePathCallback
                val allowMultiple = fileChooserParams?.mode == FileChooserParams.MODE_OPEN_MULTIPLE
                val pick = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
                    addCategory(Intent.CATEGORY_OPENABLE)
                    type = "*/*"
                    if (allowMultiple) putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
                }
                return try {
                    activity.startActivityForResult(pick, REQ_FILE_CHOOSER)
                    true
                } catch (e: Exception) {
                    Log.w(TAG, "file chooser launch failed: ${e.message}")
                    this@StandaloneUi.fileUploadCallback?.onReceiveValue(null)
                    this@StandaloneUi.fileUploadCallback = null
                    false
                }
            }
        }
        webView.setDownloadListener(
            DownloadListener { url, _, contentDisposition, mimetype, _ ->
                if (url.isNullOrBlank()) return@DownloadListener
                val guessed = URLUtil.guessFileName(url, contentDisposition, mimetype)
                startUrlExport(url, guessed, mimetype ?: "application/octet-stream")
            },
        )
    }

    private fun hideSplash() {
        if (loadingOverlay.visibility != View.VISIBLE) return
        loadingOverlay.animate()
            .alpha(0f)
            .setDuration(180L)
            .withEndAction {
                loadingOverlay.visibility = View.GONE
                loadingOverlay.alpha = 1f
            }
            .start()
    }

    private fun checkEmbeddedReady() {
        val st = EmbeddedAgent.status()
        if (st.ready && !embeddedSettingsSynced) {
            embeddedSettingsSynced = true
            val action = Prefs.standaloneWhitelistAction(activity)
            EmbeddedAgent.patchSettings(
                activity,
                JSONObject()
                    .put("standalone_whitelist_action", action)
                    .put("gemini_live_enabled", true)
                    .toString(),
            )
        }
        if (st.ready && !consoleLoaded) {
            consoleLoaded = true
            val baseUrl = EmbeddedAgent.localMobileUrl(st.uiToken)
            val openCall = pendingOpenCallId
            pendingOpenCallId = null
            val url = if (!openCall.isNullOrBlank()) {
                "$baseUrl#call=${Uri.encode(openCall)}"
            } else {
                baseUrl
            }
            webView.loadUrl(url)
            // Fallback splash dismissal in case onPageFinished is delayed.
            handler.postDelayed({ if (!activity.isDestroyed) hideSplash() }, 1200L)
        } else if (consoleLoaded && !pendingOpenCallId.isNullOrBlank()) {
            val openCall = pendingOpenCallId!!
            pendingOpenCallId = null
            webView.evaluateJavascript("location.hash='#call=' + ${JSONObject.quote(openCall)};", null)
        }
    }

    private fun isLocalConsoleUri(uri: Uri): Boolean {
        val scheme = uri.scheme?.lowercase().orEmpty()
        val host = uri.host?.lowercase().orEmpty()
        val port = uri.port
        return scheme == "http" &&
            (host == "127.0.0.1" || host == "localhost") &&
            (port == -1 || port == BuildConfig.CONSOLE_PORT)
    }

    private fun buildNativeStateJson(): JSONObject {
        val mode = PhoneModeManager.reconcileLocalMirror(activity)
        val ai = mode == Prefs.PHONE_MODE_AI
        val switching = PhoneModeManager.isSwitching()
        val st = EmbeddedAgent.status()

        val roleManager = activity.getSystemService(RoleManager::class.java)
        val moduleOk = isPrivilegedSystemApp() && privilegedPermissions.all { isGranted(it) }
        val permsOk = runtimePermissions.all { isGranted(it) }
        val dialerOk = DialerRoleHelper.isDefaultDialer(activity) ||
            roleManager?.isRoleHeld(RoleManager.ROLE_DIALER) == true
        val screeningOk = roleManager?.isRoleHeld(RoleManager.ROLE_CALL_SCREENING) == true
        val dndOk = activity.getSystemService(NotificationManager::class.java)
            ?.isNotificationPolicyAccessGranted == true
        val batteryOk = activity.getSystemService(PowerManager::class.java)
            ?.isIgnoringBatteryOptimizations(activity.packageName) == true
        val contactsPerm = PhoneContactsSync.hasPermission(activity)
        val canInject = isGranted("android.permission.MODIFY_PHONE_STATE")
        val telephonyOut = try {
            TelephonyUplinkInjector(activity).isAvailable()
        } catch (_: Exception) {
            false
        }

        val checks = JSONObject()
            .put("module_ok", moduleOk)
            .put("perms_ok", permsOk)
            .put("dialer_ok", dialerOk)
            .put("screening_ok", screeningOk)
            .put("dnd_ok", dndOk)
            .put("battery_ok", batteryOk)
            .put("contacts_perm", contactsPerm)
            .put("uplink_ok", canInject && telephonyOut)

        val prefs = JSONObject()
            .put("daily_sound", Prefs.isStandaloneDailySound(activity))
            .put("persistent_notif", Prefs.isStandalonePersistentNotif(activity))
            .put("summary_notif", Prefs.isStandaloneCallSummaryNotif(activity))
            .put("dnd_auto", Prefs.isStandaloneDndAutoAi(activity))
            .put("dnd_any", Prefs.isStandaloneDndAnyMode(activity))
            .put("whitelist_action", Prefs.standaloneWhitelistAction(activity))

        val lan = JSONObject()
            .put("requested", Prefs.isStandaloneLanEnabled(activity))
            .put("enabled", st.lanEnabled)
            .put("password_set", st.passwordSet)
            .put("lan_ip", st.lanIp)
            .put("lan_url", st.lanUrl)
            .put("mdns_url", st.mdnsUrl)

        val errCode = modeErrorCode
        val errText = if (errCode != null && !switching) {
            activity.getString(R.string.mode_failed, modeErrorText(errCode))
        } else {
            ""
        }

        val sims = try {
            SimList.read(activity) ?: JSONArray()
        } catch (_: Exception) {
            JSONArray()
        }

        return JSONObject()
            .put("mode", mode)
            .put("ai_enabled", ai)
            .put("switching", switching)
            .put("mode_error", errText)
            .put("live_ai_call", CallAudioService.isLiveAiCallActive())
            .put("owner_in_call", CallAudioService.isOwnerHandlingCall())
            .put("call_busy", !PhoneModeManager.isPhoneIdle(activity))
            .put("service_running", isServiceRunning())
            .put("version_name", BuildConfig.VERSION_NAME)
            .put("version_code", BuildConfig.VERSION_CODE)
            .put("device_name", deviceName())
            .put("android_release", Build.VERSION.RELEASE)
            .put("serial", deviceSerial())
            .put("sims", sims)
            .put("checks", checks)
            .put("prefs", prefs)
            .put("lan", lan)
    }

    private fun pushNativeState() {
        if (!consoleLoaded || !pageReady || !::webView.isInitialized || activity.isDestroyed) return
        val json = buildNativeStateJson().toString()
        webView.evaluateJavascript(
            "if(window.__onBageoNativeState)window.__onBageoNativeState($json);",
            null,
        )
    }

    private fun pushContactsSyncResult(res: PhoneContactsSync.SyncResult) {
        if (!consoleLoaded || !::webView.isInitialized || activity.isDestroyed) return
        val json = JSONObject()
            .put("ok", res.ok)
            .put("added", res.added)
            .put("merged", res.merged)
            .put("skipped", res.skipped)
            .put("error", res.error)
            .toString()
        webView.evaluateJavascript(
            "if(window.__onBageoContactsSynced)window.__onBageoContactsSynced($json);",
            null,
        )
    }

    private inner class MobileAppJsBridge {
        @JavascriptInterface
        fun getNativeStateJson(): String = buildNativeStateJson().toString()

        @JavascriptInterface
        fun setAiMode(enable: Boolean) {
            activity.runOnUiThread {
                performHaptic("toggle")
                Prefs.markManualModeSwitch(activity)
                if (enable) {
                    if (!hasRequiredPerms()) {
                        pendingAutoAiAfterPerms = true
                        requestMissingRuntimePermissions()
                        return@runOnUiThread
                    }
                    startModeSwitch(Prefs.PHONE_MODE_AI)
                } else {
                    startModeSwitch(Prefs.PHONE_MODE_NORMAL)
                }
            }
        }

        @JavascriptInterface
        fun openPhoneApp(number: String) {
            activity.runOnUiThread {
                performHaptic("tap")
                openDialerOrPhoneApp(number)
            }
        }

        @JavascriptInterface
        fun takeoverLiveCall() {
            activity.runOnUiThread {
                performHaptic("confirm")
                CallAudioService.requestOwnerTakeover(activity)
                pushNativeState()
            }
        }

        @JavascriptInterface
        fun runSetupAction(kind: String) {
            activity.runOnUiThread {
                performHaptic("tap")
                when (kind.trim().lowercase()) {
                    "perms" -> requestMissingRuntimePermissions()
                    "dialer" -> {
                        DialerRoleHelper.requestDialerRole(activity)
                        refreshSoon()
                    }
                    "screening" -> requestScreeningRole()
                    "dnd" -> launch(Intent(Settings.ACTION_NOTIFICATION_POLICY_ACCESS_SETTINGS))
                    "dnd_rule" -> {
                        if (!DedicatedDndManager.hasPolicyAccess(activity)) {
                            launch(Intent(Settings.ACTION_NOTIFICATION_POLICY_ACCESS_SETTINGS))
                        } else {
                            DedicatedDndManager.openSystemDndSettings(activity)
                        }
                    }
                    "battery" -> requestBatteryExemption()
                    "app_settings" -> launch(
                        Intent(
                            Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
                            Uri.parse("package:${activity.packageName}"),
                        ),
                    )
                }
            }
        }

        @JavascriptInterface
        fun syncContacts(starredToWhitelist: Boolean) {
            activity.runOnUiThread {
                performHaptic("tap")
                pendingStarredToWhitelist = starredToWhitelist
                if (!PhoneContactsSync.hasPermission(activity)) {
                    ActivityCompat.requestPermissions(
                        activity,
                        arrayOf(Manifest.permission.READ_CONTACTS),
                        REQ_CONTACTS,
                    )
                    return@runOnUiThread
                }
                runContactsSync(starredToWhitelist)
            }
        }

        @JavascriptInterface
        fun setMobilePref(key: String, value: String) {
            val boolVal = value == "true" || value == "1"
            activity.runOnUiThread {
                performHaptic("toggle")
                when (key.trim()) {
                    "daily_sound" -> {
                        Prefs.setStandaloneDailySound(activity, boolVal)
                        if (Prefs.isAiCallEnabled(activity) && !PhoneModeManager.isSwitching()) {
                            startModeSwitch(Prefs.PHONE_MODE_AI)
                        }
                        if (Prefs.isAiCallEnabled(activity)) {
                            LocalAudioGuard(activity).applyHardMute()
                        }
                    }
                    "persistent_notif" -> {
                        Prefs.setStandalonePersistentNotif(activity, boolVal)
                        CallAudioService.refreshNotification(activity)
                    }
                    "summary_notif" -> {
                        Prefs.setStandaloneCallSummaryNotif(activity, boolVal)
                    }
                    "dnd_auto" -> {
                        if (boolVal && !DedicatedDndManager.hasPolicyAccess(activity)) {
                            launch(Intent(Settings.ACTION_NOTIFICATION_POLICY_ACCESS_SETTINGS))
                        } else {
                            Prefs.setStandaloneDndAutoAi(activity, boolVal)
                            if (boolVal) DedicatedDndManager.ensureRule(activity)
                        }
                    }
                    "dnd_any" -> {
                        Prefs.setStandaloneDndAnyMode(activity, boolVal)
                    }
                    "whitelist_action" -> {
                        val clean = Prefs.normalizeWhitelistAction(value)
                        Prefs.setStandaloneWhitelistAction(activity, clean)
                        EmbeddedAgent.patchSettings(
                            activity,
                            JSONObject().put("standalone_whitelist_action", clean).toString(),
                        )
                    }
                }
                pushNativeState()
            }
        }

        @JavascriptInterface
        fun setLanAccess(enabled: Boolean, password: String): String {
            if (!enabled) {
                EmbeddedAgent.setLanEnabled(activity, false)
                activity.runOnUiThread {
                    if (!Prefs.isAiCallEnabled(activity)) {
                        activity.startService(
                            Intent(activity, CallAudioService::class.java).setAction(CallAudioService.ACTION_STOP),
                        )
                    }
                    pushNativeState()
                    refreshSoon()
                }
                return JSONObject().put("ok", true).toString()
            }
            val pw = password.trim()
            val st = EmbeddedAgent.status()
            if (pw.isNotEmpty()) {
                if (pw.length < MIN_PASSWORD_LEN) {
                    return JSONObject().put("ok", false).put("error", "password_too_short").toString()
                }
                if (!EmbeddedAgent.setConsolePassword(pw)) {
                    return JSONObject().put("ok", false).put("error", "save_failed").toString()
                }
            } else if (!st.passwordSet) {
                return JSONObject().put("ok", false).put("error", "need_password").toString()
            }
            EmbeddedAgent.setLanEnabled(activity, true)
            activity.runOnUiThread {
                if (!Prefs.isAiCallEnabled(activity) && hasRequiredPerms()) {
                    CallAudioService.ensureArmed(activity)
                }
                pushNativeState()
                refreshSoon()
            }
            return JSONObject().put("ok", true).toString()
        }

        @JavascriptInterface
        fun syncTheme(theme: String) {
            activity.runOnUiThread {
                applySystemBarTheme(theme == "dark")
            }
        }

        @JavascriptInterface
        fun haptic(kind: String) {
            activity.runOnUiThread { performHaptic(kind) }
        }

        @JavascriptInterface
        fun saveBase64File(base64: String, filename: String, mimeType: String) {
            val cleanName = filename.trim().ifEmpty { "bageo-export.bin" }.replace('/', '_')
            val cleanMime = mimeType.trim().ifEmpty { "application/octet-stream" }
            try {
                val bytes = Base64.decode(base64, Base64.DEFAULT)
                val dir = File(activity.cacheDir, "exports").apply { mkdirs() }
                val tmp = File.createTempFile("exp-", ".tmp", dir)
                tmp.writeBytes(bytes)
                activity.runOnUiThread {
                    launchCreateDocument(PendingExport(tmp, null, cleanName, cleanMime))
                }
            } catch (e: Exception) {
                Log.w(TAG, "saveBase64File failed: ${e.message}")
                activity.runOnUiThread {
                    Toast.makeText(activity, R.string.sa_save_fail, Toast.LENGTH_SHORT).show()
                }
            }
        }

        @JavascriptInterface
        fun downloadUrl(url: String, filename: String) {
            val cleanName = filename.trim().ifEmpty { "bageo-backup" }.replace('/', '_')
            activity.runOnUiThread {
                startUrlExport(url, cleanName, "application/octet-stream")
            }
        }

        @JavascriptInterface
        fun copyText(text: String) {
            activity.runOnUiThread { copyToClipboard(text) }
        }
    }

    private fun openDialerOrPhoneApp(number: String) {
        val clean = number.trim()
        if (!Prefs.isAiCallEnabled(activity)) {
            if (PhoneModeManager.openSystemPhoneApp(activity, clean)) {
                return
            }
        }
        val intent = Intent(activity, OwnerCallActivity::class.java)
            .putExtra("mode", OwnerCallActivity.MODE_DIALER)
        if (clean.isNotEmpty()) intent.putExtra("dial_number", clean)
        launch(intent)
    }

    private fun runContactsSync(starredToWhitelist: Boolean) {
        Thread({
            val res = PhoneContactsSync.syncNow(activity, starredToWhitelist)
            activity.runOnUiThread {
                if (activity.isDestroyed) return@runOnUiThread
                pushContactsSyncResult(res)
                pushNativeState()
            }
        }, "bageo-contacts-sync").start()
    }

    private fun startModeSwitch(target: String) {
        modeErrorCode = null
        modeErrorDndMode = ""
        PhoneModeManager.requestSwitch(
            activity,
            target,
            callBusy = !PhoneModeManager.isPhoneIdle(activity),
        ) { result ->
            activity.runOnUiThread {
                if (activity.isDestroyed) return@runOnUiThread
                modeErrorCode = if (result.ok) null else result.error
                modeErrorDndMode = if (result.ok) "" else result.dndMode
                if (result.ok && result.mode == Prefs.PHONE_MODE_AI && hasRequiredPerms()) {
                    DialerRoleHelper.ensureRoles(activity)
                    CallAudioService.ensureArmed(activity)
                }
                pushNativeState()
                refreshSoon()
            }
        }
        pushNativeState()
    }

    private fun modeErrorText(code: String): String {
        if (code == PhoneModeManager.ERR_DND && modeErrorDndMode.isNotEmpty()) {
            return activity.getString(R.string.mode_err_dnd_mode, modeErrorDndMode)
        }
        return activity.getString(
            when (code) {
                PhoneModeManager.ERR_IN_CALL -> R.string.mode_err_in_call
                PhoneModeManager.ERR_BUSY -> R.string.mode_err_busy
                PhoneModeManager.ERR_NO_ROOT -> R.string.mode_err_no_root
                PhoneModeManager.ERR_SAVE -> R.string.mode_err_save
                PhoneModeManager.ERR_DIALER -> R.string.mode_err_dialer
                PhoneModeManager.ERR_SCREENING -> R.string.mode_err_screening
                PhoneModeManager.ERR_DND -> R.string.mode_err_dnd
                PhoneModeManager.ERR_VOLUME -> R.string.mode_err_volume
                else -> R.string.mode_err_other
            },
        )
    }

    // ---------------------------------------------------------------- Exports & File Picker

    private fun startUrlExport(rawUrl: String, filename: String, mimeType: String) {
        val resolved = resolveLocalUrl(rawUrl)
        if (resolved == null) {
            Toast.makeText(activity, R.string.sa_save_fail, Toast.LENGTH_SHORT).show()
            return
        }
        launchCreateDocument(
            PendingExport(null, resolved, filename, mimeType.ifBlank { "application/octet-stream" }),
        )
    }

    private fun resolveLocalUrl(rawUrl: String): String? {
        return try {
            val base = URI("http://127.0.0.1:${BuildConfig.CONSOLE_PORT}/calls/")
            val target = base.resolve(rawUrl.trim())
            val host = target.host?.lowercase().orEmpty()
            val port = if (target.port == -1) 80 else target.port
            if (target.scheme == "http" &&
                (host == "127.0.0.1" || host == "localhost") &&
                port == BuildConfig.CONSOLE_PORT
            ) {
                target.toString()
            } else {
                null
            }
        } catch (_: Exception) {
            null
        }
    }

    private fun launchCreateDocument(job: PendingExport) {
        pendingExport?.stagedFile?.delete()
        pendingExport = job
        val intent = Intent(Intent.ACTION_CREATE_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = job.mimeType
            putExtra(Intent.EXTRA_TITLE, job.filename)
        }
        if (!launch(intent, REQ_CREATE_DOC)) {
            job.stagedFile?.delete()
            pendingExport = null
            Toast.makeText(activity, R.string.sa_save_fail, Toast.LENGTH_SHORT).show()
        }
    }

    private fun finishPendingExport(job: PendingExport, destUri: Uri) {
        Thread({
            var ok = false
            try {
                activity.contentResolver.openOutputStream(destUri)?.use { out ->
                    if (job.stagedFile != null) {
                        job.stagedFile.inputStream().use { input -> input.copyTo(out) }
                        ok = true
                    } else if (job.downloadUrl != null) {
                        val conn = (URL(job.downloadUrl).openConnection() as HttpURLConnection).apply {
                            connectTimeout = 10_000
                            readTimeout = 60_000
                            val tok = EmbeddedAgent.status().uiToken
                            if (tok.isNotBlank()) setRequestProperty("x-calls-token", tok)
                        }
                        try {
                            if (conn.responseCode in 200..299) {
                                conn.inputStream.use { input -> input.copyTo(out) }
                                ok = true
                            }
                        } finally {
                            conn.disconnect()
                        }
                    }
                }
            } catch (e: Exception) {
                Log.w(TAG, "export copy failed: ${e.message}")
            } finally {
                job.stagedFile?.delete()
            }
            activity.runOnUiThread {
                Toast.makeText(
                    activity,
                    if (ok) R.string.sa_save_ok else R.string.sa_save_fail,
                    Toast.LENGTH_SHORT,
                ).show()
            }
        }, "bageo-export").start()
    }

    private fun parseChooserResult(resultCode: Int, data: Intent?): Array<Uri>? {
        if (resultCode != Activity.RESULT_OK || data == null) return null
        val single = data.data
        if (single != null) return arrayOf(single)
        val clip = data.clipData
        if (clip != null && clip.itemCount > 0) {
            return Array(clip.itemCount) { idx -> clip.getItemAt(idx).uri }
        }
        return WebChromeClient.FileChooserParams.parseResult(resultCode, data)
    }

    // ---------------------------------------------------------------- Permissions, System Bars & Helpers

    private fun handleLaunchIntent(intent: Intent?) {
        intent?.getStringExtra("open_call_id")?.trim()?.takeIf { it.isNotBlank() }?.let { callId ->
            pendingOpenCallId = callId
            checkEmbeddedReady()
        }
        when (intent?.action) {
            CallAudioService.ACTION_SMOKE_CONNECT,
            CallAudioService.ACTION_START,
            -> if (hasRequiredPerms()) CallAudioService.ensureArmed(activity)
        }
    }

    private fun hasRequiredPerms(): Boolean = requiredPermissions().all { isGranted(it) }

    private fun requiredPermissions(): List<String> {
        val need = mutableListOf(
            Manifest.permission.RECORD_AUDIO,
            Manifest.permission.READ_PHONE_STATE,
            Manifest.permission.ANSWER_PHONE_CALLS,
        )
        if (Build.VERSION.SDK_INT >= 33) {
            need += Manifest.permission.POST_NOTIFICATIONS
        }
        return need
    }

    private fun requestMissingRuntimePermissions() {
        val missing = runtimePermissions.filter { !isGranted(it) }
        if (missing.isEmpty()) {
            pushNativeState()
            return
        }
        grantRationaleBefore = missing.associateWith { activity.shouldShowRequestPermissionRationale(it) }
        grantRequestedAt = SystemClock.elapsedRealtime()
        ActivityCompat.requestPermissions(activity, missing.toTypedArray(), REQ_GRANT_ALL)
    }

    private fun requestScreeningRole() {
        val rm = activity.getSystemService(RoleManager::class.java) ?: return
        if (!rm.isRoleAvailable(RoleManager.ROLE_CALL_SCREENING) ||
            rm.isRoleHeld(RoleManager.ROLE_CALL_SCREENING)
        ) {
            pushNativeState()
            return
        }
        try {
            activity.startActivityForResult(
                rm.createRequestRoleIntent(RoleManager.ROLE_CALL_SCREENING),
                REQ_SCREENING_ROLE,
            )
        } catch (e: Exception) {
            Log.w(TAG, "ROLE_CALL_SCREENING failed: ${e.message}")
        }
    }

    private fun requestBatteryExemption() {
        val ok = launch(
            Intent(
                Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
                Uri.parse("package:${activity.packageName}"),
            ),
        )
        if (!ok) launch(Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS))
    }

    private fun copyToClipboard(text: String) {
        val cm = activity.getSystemService(ClipboardManager::class.java) ?: return
        cm.setPrimaryClip(ClipData.newPlainText("Bageo", text))
    }

    private fun launch(intent: Intent, reqCode: Int? = null): Boolean = try {
        if (reqCode != null) {
            activity.startActivityForResult(intent, reqCode)
        } else {
            activity.startActivity(intent)
        }
        true
    } catch (e: Exception) {
        Log.w(TAG, "launch ${intent.action} failed: ${e.message}")
        false
    }

    private fun refreshSoon() {
        handler.postDelayed({ if (!activity.isDestroyed) pushNativeState() }, 650L)
    }

    private fun isGranted(perm: String): Boolean =
        ContextCompat.checkSelfPermission(activity, perm) == PackageManager.PERMISSION_GRANTED

    private fun isPrivilegedSystemApp(): Boolean {
        val info = activity.applicationInfo
        return (info.flags and ApplicationInfo.FLAG_SYSTEM) != 0 &&
            info.sourceDir.orEmpty().contains("/priv-app/")
    }

    private fun isServiceRunning(): Boolean = try {
        val am = activity.getSystemService(ActivityManager::class.java)
        @Suppress("DEPRECATION")
        am?.getRunningServices(Int.MAX_VALUE)
            ?.any { it.service.className == CallAudioService::class.java.name } == true
    } catch (_: Exception) {
        false
    }

    private fun deviceName(): String {
        val m = Build.MANUFACTURER.orEmpty().trim()
        val model = Build.MODEL.orEmpty().trim()
        return if (m.isEmpty() || model.startsWith(m, ignoreCase = true)) model else "$m $model"
    }

    private fun deviceSerial(): String {
        return try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                Build.getSerial()
            } else {
                @Suppress("DEPRECATION")
                Build.SERIAL
            }
        } catch (_: SecurityException) {
            "pixel-${Process.myUid()}"
        }.ifBlank { "pixel-${Process.myUid()}" }
    }

    private fun performHaptic(kind: String) {
        val view = if (::webView.isInitialized) webView else rootView
        val code = when (kind) {
            "confirm" -> if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) HapticFeedbackConstants.CONFIRM else HapticFeedbackConstants.VIRTUAL_KEY
            "reject" -> if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) HapticFeedbackConstants.REJECT else HapticFeedbackConstants.LONG_PRESS
            "toggle" -> if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) HapticFeedbackConstants.TOGGLE_ON else HapticFeedbackConstants.CLOCK_TICK
            else -> HapticFeedbackConstants.KEYBOARD_TAP
        }
        try {
            view.performHapticFeedback(code)
        } catch (_: Exception) {
        }
    }

    private fun syncSystemBarsWithSystemNightMode() {
        val night = (activity.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK) ==
            Configuration.UI_MODE_NIGHT_YES
        applySystemBarTheme(night)
    }

    @Suppress("DEPRECATION")
    private fun applySystemBarTheme(isDark: Boolean) {
        val bgColor = Color.parseColor(if (isDark) "#141518" else "#F6F5F1")
        val navColor = Color.parseColor(if (isDark) "#1B1D21" else "#FFFFFF")
        try {
            activity.window.statusBarColor = bgColor
            activity.window.navigationBarColor = navColor
            val ctrl = WindowCompat.getInsetsController(activity.window, rootView)
            ctrl.isAppearanceLightStatusBars = !isDark
            ctrl.isAppearanceLightNavigationBars = !isDark
        } catch (_: Exception) {
        }
    }

    private fun applySystemBarInsets(root: View) {
        WindowCompat.setDecorFitsSystemWindows(activity.window, false)
        ViewCompat.setOnApplyWindowInsetsListener(root) { view, insets ->
            val bars = insets.getInsets(
                WindowInsetsCompat.Type.systemBars() or
                    WindowInsetsCompat.Type.displayCutout() or
                    WindowInsetsCompat.Type.ime(),
            )
            view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
            insets
        }
    }

    companion object {
        const val ENABLED = true
        private const val TAG = "StandaloneUi"
        private const val MIN_PASSWORD_LEN = 8
        private const val REQ_GRANT_ALL = 101
        private const val REQ_CONTACTS = 102
        private const val REQ_SCREENING_ROLE = 7102
        private const val REQ_FILE_CHOOSER = 7103
        private const val REQ_CREATE_DOC = 7104
        private const val POLL_READY_MS = 200L
        private const val TICK_MS = 2_500L
        private const val INSTANT_DENIAL_MS = 350L
    }
}
