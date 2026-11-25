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
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.os.Process
import android.os.SystemClock
import android.provider.OpenableColumns
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
import android.webkit.WebResourceResponse
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
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.net.URLDecoder
import java.net.URLEncoder
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

/**
 * Pure mobile App host for Android Standalone edition:
 * - Full-screen WebView showing `/calls/mobile` (`standalone.html`) with zero desktop/hybrid chrome.
 *   The console never goes over the network: the page lives at [CONSOLE_ORIGIN], a reserved
 *   address that never resolves, and every request for it is answered by the agent inside this
 *   app ([EmbeddedAgent.http]): GET and HEAD in [WebViewClient.shouldInterceptRequest], requests
 *   with a body through [MobileAppJsBridge.httpRequest] (a web view cannot pass a body on).
 * - Direct `@JavascriptInterface` (`window.BageoAndroid`) bridging single-phone AI mode switching,
 *   Google Phone / native dialpad launch, permission & role setup, native contacts sync,
 *   local mobile preferences, haptics, file picker/export, and dynamic system bars.
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
    private var lastStartAttempt = 0L

    /** Runs the page's console requests off the main thread. */
    private val httpExecutor: ExecutorService = Executors.newCachedThreadPool()

    private var fileUploadCallback: ValueCallback<Array<Uri>>? = null
    private var pendingExport: PendingExport? = null

    /** Headers (the console password) for the backup file being picked; null: none asked. */
    private var pendingImportHeaders: String? = null

    /** The console's colours (the phone's own palette when it has one) for the bars and the web view. */
    private val theme by lazy { ThemeBridge(activity) }

    private data class PendingExport(
        val stagedFile: File?,
        /** Console path to fetch from the agent (instead of [stagedFile]). */
        val downloadTarget: String?,
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
        }

        checkEmbeddedReady()
    }

    fun onNewIntent(intent: Intent) {
        handleLaunchIntent(intent)
    }

    fun onStart() {
        LocalAudioGuard(activity).setConsolePreviewAllowed(true)
        // In normal-phone mode the app's call link is up only while the console shows (AI errands).
        CallAudioService.onConsoleVisible(activity)
        if (::webView.isInitialized) {
            webView.onResume()
            webView.resumeTimers()
        }
    }

    fun onStop() {
        LocalAudioGuard(activity).setConsolePreviewAllowed(false)
        CallAudioService.onConsoleHidden(activity)
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
        // Back in the app (say, from downloading a translation language): the page
        // does not get a visibility change from the WebView, so tell it.
        if (::webView.isInitialized) webView.evaluateJavascript("if(window.__bageoResumed)window.__bageoResumed();", null)
    }

    fun onPause() {
        PhoneModeManager.removeListener(modeListener)
        handler.removeCallbacks(pollRunnable)
    }

    fun onDestroy() {
        unregisterBackNavigation()
        handler.removeCallbacks(pollRunnable)
        httpExecutor.shutdown()
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
        if (requestCode == REQ_BACKUP_PICK) {
            val headers = pendingImportHeaders ?: "{}"
            pendingImportHeaders = null
            val uri = if (resultCode == Activity.RESULT_OK) data?.data else null
            if (uri == null) {
                backupPicked(JSONObject().put("error", "cancel"))
                return
            }
            try {
                httpExecutor.execute { backupPicked(importBackup(uri, headers)) }
            } catch (e: Exception) {
                Log.w(TAG, "backup import not queued: ${e.message}")
                backupPicked(JSONObject().put("error", "read_failed"))
            }
            return
        }
        pushNativeState()
    }

    // ---------------------------------------------------------------- WebView & JS Bridge

    @SuppressLint("SetJavaScriptEnabled")
    private fun configureWebView() {
        // The page's own background before it paints: no white flash, same colour as the page.
        webView.setBackgroundColor(theme.pageColor(isSystemNight()))
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
        webView.addJavascriptInterface(theme, "BageoTheme")
        webView.webViewClient = object : WebViewClient() {
            override fun shouldInterceptRequest(
                view: WebView?,
                request: WebResourceRequest?,
            ): WebResourceResponse? {
                val req = request ?: return null
                if (!isLocalConsoleUri(req.url)) return null
                return serveConsoleRequest(req)
            }

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
        if (!st.running) {
            // Not started yet, or it stopped: try again, at most every few seconds.
            val now = SystemClock.elapsedRealtime()
            if (now - lastStartAttempt >= RESTART_EVERY_MS) {
                lastStartAttempt = now
                EmbeddedAgent.ensureStarted(activity)
            }
        }
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
            val openCall = pendingOpenCallId
            pendingOpenCallId = null
            val url = if (!openCall.isNullOrBlank()) {
                "$CONSOLE_URL#call=${Uri.encode(openCall)}"
            } else {
                CONSOLE_URL
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
        return scheme == "https" && host == CONSOLE_HOST && (port == -1 || port == 443)
    }

    /**
     * Path and query of a console address ([CONSOLE_HOST]) or of a bare path such as
     * `/calls/api/list`. Null for any other place.
     */
    private fun consoleTarget(uri: Uri): String? {
        if (uri.scheme == null) {
            if (uri.encodedAuthority != null) return null
        } else if (!isLocalConsoleUri(uri)) {
            return null
        }
        val path = uri.encodedPath.orEmpty()
        if (!path.startsWith("/") || path.startsWith("//")) return null
        val query = uri.encodedQuery
        return if (query.isNullOrEmpty()) path else "$path?$query"
    }

    /** A page request for [CONSOLE_HOST], answered by the agent in this app (runs off the main thread). */
    private fun serveConsoleRequest(req: WebResourceRequest): WebResourceResponse {
        val method = req.method.orEmpty().uppercase()
        if (method != "GET" && method != "HEAD") {
            // The page sends these through BageoAndroid.httpRequest: the body cannot be read here.
            return emptyResponse(405)
        }
        var target = consoleTarget(req.url) ?: return emptyResponse(400)
        val headers = withSystemLanguage(req.requestHeaders ?: emptyMap())
        var hops = 0
        while (true) {
            val res = EmbeddedAgent.http(method, target, headers) ?: return emptyResponse(503)
            if (res.status in 300..399) {
                // A web view cannot take a redirect from here: follow it inside the app.
                val next = res.header("location")?.let { consoleTarget(Uri.parse(it)) }
                if (next != null && hops < MAX_REDIRECTS) {
                    target = next
                    hops++
                    continue
                }
                return emptyResponse(502)
            }
            return toWebResponse(res, method == "HEAD")
        }
    }

    /**
     * The phone's languages when the page did not send any, so the console opens in the
     * phone's language the first time (before the owner picks one).
     */
    private fun withSystemLanguage(given: Map<String, String>): Map<String, String> {
        if (given.keys.any { it.equals("accept-language", ignoreCase = true) }) return given
        val tags = try {
            activity.resources.configuration.locales.toLanguageTags()
        } catch (_: Exception) {
            ""
        }
        if (tags.isBlank()) return given
        return LinkedHashMap(given).apply { put("Accept-Language", tags) }
    }

    private fun toWebResponse(res: EmbeddedAgent.HttpResult, headOnly: Boolean): WebResourceResponse {
        val contentType = res.header("content-type").orEmpty()
        val mime = contentType.substringBefore(';').trim().ifEmpty { "application/octet-stream" }
        val charset = CHARSET_RE.find(contentType)?.groupValues?.get(1)?.trim()?.trim('"')?.ifEmpty { null }
        val headers = LinkedHashMap<String, String>()
        for ((name, value) in res.headers) {
            if (name in SKIPPED_RESPONSE_HEADERS) continue
            headers[name] = headers[name]?.let { "$it, $value" } ?: value
        }
        val status = if (res.status in 200..299 || res.status in 400..599) res.status else 502
        val body = if (headOnly) ByteArray(0) else res.body
        return WebResourceResponse(mime, charset, status, reasonPhrase(status), headers, ByteArrayInputStream(body))
    }

    private fun emptyResponse(status: Int): WebResourceResponse =
        WebResourceResponse(
            "text/plain",
            "utf-8",
            status,
            reasonPhrase(status),
            mutableMapOf("cache-control" to "no-store"),
            ByteArrayInputStream(ByteArray(0)),
        )

    /** WebResourceResponse needs a non-empty reason phrase. */
    private fun reasonPhrase(status: Int): String = when (status) {
        200 -> "OK"
        201 -> "Created"
        202 -> "Accepted"
        204 -> "No Content"
        206 -> "Partial Content"
        400 -> "Bad Request"
        401 -> "Unauthorized"
        403 -> "Forbidden"
        404 -> "Not Found"
        405 -> "Method Not Allowed"
        409 -> "Conflict"
        413 -> "Payload Too Large"
        416 -> "Range Not Satisfiable"
        422 -> "Unprocessable Entity"
        429 -> "Too Many Requests"
        500 -> "Internal Server Error"
        502 -> "Bad Gateway"
        503 -> "Service Unavailable"
        else -> if (status < 400) "OK" else "Error"
    }

    private fun buildNativeStateJson(): JSONObject {
        val mode = PhoneModeManager.reconcileLocalMirror(activity)
        val ai = mode == Prefs.PHONE_MODE_AI
        val switching = PhoneModeManager.isSwitching()

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
            .put("sms_ok", isGranted(Manifest.permission.SEND_SMS))

        val prefs = JSONObject()
            .put("daily_sound", Prefs.isStandaloneDailySound(activity))
            .put("persistent_notif", Prefs.isStandalonePersistentNotif(activity))
            .put("summary_notif", Prefs.isStandaloneCallSummaryNotif(activity))
            .put("dnd_auto", Prefs.isStandaloneDndAutoAi(activity))
            .put("dnd_any", Prefs.isStandaloneDndAnyMode(activity))
            .put("whitelist_action", Prefs.standaloneWhitelistAction(activity))

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

        /**
         * A console request with a body (POST, PUT, DELETE…), answered by the agent in this app
         * on a background thread. The answer comes back as
         * `window.__bageoHttpDone(id, {status, headers, body})` with the body in base64;
         * `status` 0 means it could not be sent.
         */
        @JavascriptInterface
        fun httpRequest(id: String, method: String, url: String, headersJson: String, bodyBase64: String) {
            val cleanId = id.take(64)
            try {
                httpExecutor.execute {
                    val reply = runPageRequest(method, url, headersJson, bodyBase64)
                    val js = "if(window.__bageoHttpDone)window.__bageoHttpDone(${JSONObject.quote(cleanId)},$reply);"
                    activity.runOnUiThread {
                        if (!activity.isDestroyed && ::webView.isInitialized) webView.evaluateJavascript(js, null)
                    }
                }
            } catch (e: Exception) {
                Log.w(TAG, "console request not queued: ${e.message}")
                val js = "if(window.__bageoHttpDone)window.__bageoHttpDone(${JSONObject.quote(cleanId)},{\"status\":0});"
                activity.runOnUiThread {
                    if (!activity.isDestroyed && ::webView.isInitialized) webView.evaluateJavascript(js, null)
                }
            }
        }

        /**
         * Translate a call's lines on the phone ([OnDeviceTranslate]): `json` is
         * `{"req":"…","target":"zh-TW","lines":[{"i":1,"text":"…"}]}`. The answer comes back
         * as `window.__bageoTranslated(req, "{\"lines\":[{\"i\":1,\"text\":\"…\"}],\"need_download\":[\"ja\"]}")`:
         * what the phone translated, and the languages it could do once their pack is downloaded.
         */
        @JavascriptInterface
        fun translateLines(json: String) {
            val parsed = try {
                val o = JSONObject(json)
                val arr = o.optJSONArray("lines") ?: org.json.JSONArray()
                val list = (0 until minOf(arr.length(), 80)).mapNotNull { k ->
                    arr.optJSONObject(k)?.let { l -> l.optInt("i", -1) to l.optString("text").take(2000) }
                }.filter { it.first >= 0 && it.second.isNotBlank() }
                Triple(o.optString("req").take(64), o.optString("target").take(16), list)
            } catch (_: Exception) {
                return
            }
            val (req, target, lines) = parsed
            httpExecutor.execute {
                val outcome = OnDeviceTranslate.run(activity, target, lines)
                val arr = org.json.JSONArray()
                for ((i, t) in outcome.lines) arr.put(JSONObject().put("i", i).put("text", t))
                val out = JSONObject().put("lines", arr).put("need_download", org.json.JSONArray(outcome.needDownload))
                val js = "if(window.__bageoTranslated)window.__bageoTranslated(${JSONObject.quote(req)},${JSONObject.quote(out.toString())});"
                activity.runOnUiThread {
                    if (!activity.isDestroyed && ::webView.isInitialized) webView.evaluateJavascript(js, null)
                }
            }
        }

        /** Open the phone's translation languages (Pixel: Live Translate) to download a language. */
        @JavascriptInterface
        fun openTranslateLanguages() {
            activity.runOnUiThread {
                val tries = listOf(
                    Intent("com.android.settings.action.live_translate").setPackage("com.google.android.as"),
                    Intent("com.android.settings.action.live_translate"),
                    Intent(android.provider.Settings.ACTION_LOCALE_SETTINGS),
                )
                for (intent in tries) {
                    try {
                        activity.startActivity(intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
                        return@runOnUiThread
                    } catch (_: Exception) {
                    }
                }
            }
        }

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
                    "sms" -> requestSmsPermission()
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
        fun syncTheme(theme: String) {
            activity.runOnUiThread {
                val dark = theme == "dark"
                applySystemBarTheme(dark)
                if (::webView.isInitialized) webView.setBackgroundColor(this@StandaloneUi.theme.pageColor(dark))
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

        /**
         * A one-time download (a backup ticket): fetched from the agent right away, before the
         * owner picks where to save it (the ticket would run out meanwhile), and offered under
         * the name the agent gives it ([filename] when it gives none).
         */
        @JavascriptInterface
        fun downloadUrl(url: String, filename: String) {
            val fallback = filename.trim().ifEmpty { "bageo-backup" }.replace('/', '_')
            val target = resolveLocalUrl(url)
            if (target == null) {
                activity.runOnUiThread { Toast.makeText(activity, R.string.sa_save_fail, Toast.LENGTH_SHORT).show() }
                return
            }
            try {
                httpExecutor.execute { stageDownload(target, fallback) }
            } catch (e: Exception) {
                Log.w(TAG, "download not queued: ${e.message}")
                activity.runOnUiThread { Toast.makeText(activity, R.string.sa_save_fail, Toast.LENGTH_SHORT).show() }
            }
        }

        /**
         * Pick a backup file with the system picker and send it to the agent from here (no
         * base64 copy through the page). [headersJson]: the page's headers (its password).
         * The answer comes as `window.__bageoBackupPicked({status, body} | {error})`.
         */
        @JavascriptInterface
        fun importBackupFile(headersJson: String) {
            activity.runOnUiThread {
                pendingImportHeaders = headersJson
                val pick = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
                    addCategory(Intent.CATEGORY_OPENABLE)
                    type = "*/*"
                }
                if (!launch(pick, REQ_BACKUP_PICK)) {
                    pendingImportHeaders = null
                    backupPicked(JSONObject().put("error", "read_failed"))
                }
            }
        }

        /** A web address (calendar, update page) opened in the phone's browser. */
        @JavascriptInterface
        fun openExternal(url: String) {
            val uri = try {
                Uri.parse(url.trim())
            } catch (_: Exception) {
                null
            } ?: return
            val scheme = uri.scheme?.lowercase().orEmpty()
            if ((scheme != "https" && scheme != "http") || isLocalConsoleUri(uri)) return
            activity.runOnUiThread { launch(Intent(Intent.ACTION_VIEW, uri)) }
        }

        @JavascriptInterface
        fun copyText(text: String) {
            activity.runOnUiThread { copyToClipboard(text) }
        }
    }

    /** One request from the page's JS bridge; the reply as JSON for `__bageoHttpDone`. */
    private fun runPageRequest(method: String, url: String, headersJson: String, bodyBase64: String): JSONObject {
        val reply = JSONObject()
        try {
            val target = consoleTarget(Uri.parse(url.trim()))
            if (target == null) {
                reply.put("status", 0)
                return reply
            }
            val headers = LinkedHashMap<String, String>()
            val given = try {
                JSONObject(headersJson)
            } catch (_: Exception) {
                JSONObject()
            }
            for (key in given.keys()) headers[key] = given.optString(key)
            val body = if (bodyBase64.isEmpty()) null else Base64.decode(bodyBase64, Base64.DEFAULT)
            val verb = method.trim().uppercase().ifEmpty { "GET" }
            val res = EmbeddedAgent.http(verb, target, headers, body)
            if (res == null) {
                reply.put("status", 0)
                return reply
            }
            val outHeaders = JSONObject()
            for ((name, value) in res.headers) {
                if (!outHeaders.has(name)) outHeaders.put(name, value)
            }
            reply.put("status", res.status)
                .put("headers", outHeaders)
                .put("body", Base64.encodeToString(res.body, Base64.NO_WRAP))
        } catch (t: Throwable) {
            Log.w(TAG, "console request failed: ${t.message}")
            reply.put("status", 0)
        }
        return reply
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
        val raw = rawUrl.trim()
        if (raw.isEmpty()) return null
        return try {
            val resolved = java.net.URI("$CONSOLE_ORIGIN/calls/").resolve(raw)
            consoleTarget(Uri.parse(resolved.toString()))
        } catch (_: Exception) {
            null
        }
    }

    /** Fetch [target] into a cache file now (background thread), then ask where to save it. */
    private fun stageDownload(target: String, fallbackName: String) {
        var staged: File? = null
        var name = fallbackName
        var mime = "application/octet-stream"
        try {
            val res = EmbeddedAgent.http("GET", target)
            if (res != null && res.ok) {
                name = attachmentName(res.header("content-disposition")) ?: fallbackName
                mime = res.header("content-type")?.substringBefore(';')?.trim()?.ifEmpty { null } ?: mime
                val dir = File(activity.cacheDir, "exports").apply { mkdirs() }
                val file = File.createTempFile("dl-", ".tmp", dir)
                staged = file
                file.writeBytes(res.body)
            } else {
                Log.w(TAG, "download answered ${res?.status}")
            }
        } catch (e: Exception) {
            Log.w(TAG, "download failed: ${e.message}")
            staged?.delete()
            staged = null
        }
        val file = staged
        activity.runOnUiThread {
            if (activity.isDestroyed) {
                file?.delete()
            } else if (file == null) {
                Toast.makeText(activity, R.string.sa_save_fail, Toast.LENGTH_SHORT).show()
            } else {
                launchCreateDocument(PendingExport(file, null, name, mime))
            }
        }
    }

    /** The file name of a `Content-Disposition` header (`filename*=UTF-8''…` or `filename="…"`). */
    private fun attachmentName(disposition: String?): String? {
        if (disposition.isNullOrBlank()) return null
        val encoded = STAR_NAME_RE.find(disposition)?.groupValues?.get(1)?.trim()
        val raw = encoded?.let {
            try {
                URLDecoder.decode(it.replace("+", "%2B"), "UTF-8")
            } catch (_: Exception) {
                null
            }
        } ?: PLAIN_NAME_RE.find(disposition)?.groupValues?.get(1)
        return raw?.trim()?.replace('/', '_')?.replace('\\', '_')?.ifEmpty { null }
    }

    /** Hand the result of a backup import to the page. */
    private fun backupPicked(reply: JSONObject) {
        val js = "if(window.__bageoBackupPicked)window.__bageoBackupPicked($reply);"
        activity.runOnUiThread {
            if (!activity.isDestroyed && ::webView.isInitialized) webView.evaluateJavascript(js, null)
        }
    }

    /**
     * Read the picked backup (at most [IMPORT_MAX_BYTES], the agent's limit) and send it to the
     * agent's import (background thread): `{status, body}` with the agent's answer, or
     * `{error}`: `too_large`, `read_failed`, `no_answer`.
     */
    private fun importBackup(uri: Uri, headersJson: String): JSONObject {
        val reply = JSONObject()
        var name = "backup"
        var size = -1L
        try {
            activity.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE), null, null, null)?.use { c ->
                if (c.moveToFirst()) {
                    val ni = c.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                    val si = c.getColumnIndex(OpenableColumns.SIZE)
                    if (ni >= 0 && !c.isNull(ni)) name = c.getString(ni).orEmpty().ifBlank { name }
                    if (si >= 0 && !c.isNull(si)) size = c.getLong(si)
                }
            }
        } catch (e: Exception) {
            Log.w(TAG, "backup file details: ${e.message}")
        }
        if (size > IMPORT_MAX_BYTES) return reply.put("error", "too_large")
        val body: ByteArray = try {
            activity.contentResolver.openInputStream(uri)?.use { readCapped(it, size, IMPORT_MAX_BYTES) }
                ?: return reply.put("error", "read_failed")
        } catch (_: TooLargeException) {
            return reply.put("error", "too_large")
        } catch (_: OutOfMemoryError) {
            return reply.put("error", "too_large")
        } catch (e: Exception) {
            Log.w(TAG, "backup file read failed: ${e.message}")
            return reply.put("error", "read_failed")
        }
        val headers = LinkedHashMap<String, String>()
        try {
            val given = JSONObject(headersJson)
            for (key in given.keys()) headers[key] = given.optString(key)
        } catch (_: Exception) {
        }
        headers["content-type"] = activity.contentResolver.getType(uri) ?: "application/octet-stream"
        headers["x-filename"] = URLEncoder.encode(name, "UTF-8").replace("+", "%20")
        var res: EmbeddedAgent.HttpResult? = null
        try {
            res = EmbeddedAgent.http("POST", "/calls/api/backup/import", headers, body)
        } catch (_: OutOfMemoryError) {
            return reply.put("error", "too_large")
        }
        val answer = res ?: return reply.put("error", "no_answer")
        return reply.put("status", answer.status).put("body", String(answer.body, Charsets.UTF_8))
    }

    private class TooLargeException : IOException("larger than the import limit")

    /** All of [input], at most [max] bytes ([TooLargeException] past that). */
    private fun readCapped(input: InputStream, knownSize: Long, max: Long): ByteArray {
        if (knownSize in 0..max) {
            val bytes = ByteArray(knownSize.toInt())
            var off = 0
            while (off < bytes.size) {
                val n = input.read(bytes, off, bytes.size - off)
                if (n < 0) break
                off += n
            }
            if (off < bytes.size) return bytes.copyOf(off)
            if (input.read() < 0) return bytes
            throw IOException("the file grew while it was read")
        }
        val out = ByteArrayOutputStream(64 * 1024)
        val buf = ByteArray(64 * 1024)
        var total = 0L
        while (true) {
            val n = input.read(buf)
            if (n < 0) break
            total += n
            if (total > max) throw TooLargeException()
            out.write(buf, 0, n)
        }
        return out.toByteArray()
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
                    } else if (job.downloadTarget != null) {
                        val res = EmbeddedAgent.http("GET", job.downloadTarget)
                        if (res != null && res.ok) {
                            out.write(res.body)
                            ok = true
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

    /** Sending texts (the reply to a caller's booking); settings when it was turned down for good. */
    private fun requestSmsPermission() {
        val perm = Manifest.permission.SEND_SMS
        if (isGranted(perm)) {
            pushNativeState()
            return
        }
        grantRationaleBefore = mapOf(perm to activity.shouldShowRequestPermissionRationale(perm))
        grantRequestedAt = SystemClock.elapsedRealtime()
        ActivityCompat.requestPermissions(activity, arrayOf(perm), REQ_GRANT_ALL)
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
        applySystemBarTheme(isSystemNight())
    }

    private fun isSystemNight(): Boolean =
        (activity.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK) ==
            Configuration.UI_MODE_NIGHT_YES

    @Suppress("DEPRECATION")
    private fun applySystemBarTheme(isDark: Boolean) {
        val bgColor = theme.pageColor(isDark)
        val navColor = theme.navColor(isDark)
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
                    WindowInsetsCompat.Type.displayCutout(),
            )
            val ime = insets.getInsets(WindowInsetsCompat.Type.ime())
            // The page draws under the status bar and the gesture bar itself
            // (env(safe-area-inset-top / -bottom)), like the system apps: its header
            // and tab bar run to the screen edges and a sheet's dimming covers the
            // status bar too. Padding here as well left empty strips. Only the
            // keyboard pushes the page up.
            view.setPadding(bars.left, 0, bars.right, (ime.bottom - bars.bottom).coerceAtLeast(0))
            insets
        }
    }

    companion object {
        const val ENABLED = true
        private const val TAG = "StandaloneUi"

        /**
         * Where the page lives: Android's reserved address for app content, which never
         * reaches the network. Every request for it is answered inside this app.
         */
        private const val CONSOLE_HOST = "appassets.androidplatform.net"
        private const val CONSOLE_ORIGIN = "https://$CONSOLE_HOST"
        private const val CONSOLE_URL = "$CONSOLE_ORIGIN/calls/mobile"
        private const val MAX_REDIRECTS = 3
        private const val RESTART_EVERY_MS = 5_000L
        private val CHARSET_RE = Regex("charset=([^;]+)", RegexOption.IGNORE_CASE)
        private val STAR_NAME_RE = Regex("filename\\*\\s*=\\s*UTF-8''([^;]+)", RegexOption.IGNORE_CASE)
        private val PLAIN_NAME_RE = Regex("filename\\s*=\\s*\"?([^\";]+)\"?", RegexOption.IGNORE_CASE)

        /** The agent's largest backup import (backup.rs IMPORT_MAX_BYTES). */
        private const val IMPORT_MAX_BYTES = 200L * 1024 * 1024

        /** Set from the parameters of WebResourceResponse, or not for a web view. */
        private val SKIPPED_RESPONSE_HEADERS = setOf("content-type", "content-encoding", "transfer-encoding", "connection")

        private const val REQ_GRANT_ALL = 101
        private const val REQ_CONTACTS = 102
        private const val REQ_SCREENING_ROLE = 7102
        private const val REQ_FILE_CHOOSER = 7103
        private const val REQ_CREATE_DOC = 7104
        private const val REQ_BACKUP_PICK = 7105
        private const val POLL_READY_MS = 200L
        private const val TICK_MS = 2_500L
        private const val INSTANT_DENIAL_MS = 350L
    }
}
