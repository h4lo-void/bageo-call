// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.app.ActivityManager
import android.app.AlertDialog
import android.app.NotificationManager
import android.app.role.RoleManager
import android.content.ClipData
import android.content.ClipboardManager
import android.content.DialogInterface
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.graphics.drawable.GradientDrawable
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.os.SystemClock
import android.provider.Settings
import android.text.InputType
import android.util.Base64
import android.util.Log
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
import android.widget.Button
import android.widget.CheckBox
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.RadioGroup
import android.widget.Switch
import android.widget.TextView
import android.widget.Toast
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import org.json.JSONObject
import java.io.File
import java.net.HttpURLConnection
import java.net.URI
import java.net.URL

/**
 * Standalone hybrid UI: compact native header + collapsible phone setup drawer +
 * full-screen embedded WebView hosting Bageo's full web console on `127.0.0.1:8787`.
 */
class StandaloneUi(private val activity: Activity) {
    private lateinit var dotView: View
    private lateinit var headline: TextView
    private lateinit var switchAi: Switch
    private lateinit var btnDialer: Button
    private lateinit var btnSetup: Button
    private lateinit var btnLiveTakeover: Button
    private lateinit var modeBanner: TextView
    private lateinit var webView: WebView
    private lateinit var loadingOverlay: View
    private lateinit var drawer: View
    private lateinit var statusDetail: TextView
    private lateinit var stepsSection: View
    private lateinit var stepsTitle: TextView
    private lateinit var switchDailySound: Switch
    private lateinit var switchPersistentNotif: Switch
    private lateinit var switchSummaryNotif: Switch
    private lateinit var switchDndAuto: Switch
    private lateinit var chkDndAny: CheckBox
    private lateinit var btnDndSettings: Button
    private lateinit var whitelistGroup: RadioGroup
    private lateinit var chkStarredWhitelist: CheckBox
    private lateinit var btnSyncContacts: Button
    private lateinit var syncStatusText: TextView
    private lateinit var switchLan: Switch
    private lateinit var lanBox: View
    private lateinit var lanUrlText: TextView
    private lateinit var lanMdnsText: TextView
    private lateinit var btnCopyUrl: Button
    private lateinit var advHeader: TextView
    private lateinit var advBody: View
    private lateinit var serviceStatus: TextView

    private val handler = Handler(Looper.getMainLooper())
    private var bindingAiSwitch = false
    private var bindingLanSwitch = false
    private var bindingMobileControls = false
    private var pendingOpenCallId: String? = null
    private var embeddedSettingsSynced = false
    private var drawerOpen: Boolean? = null
    private var advOpen = false
    private var consoleLoaded = false
    private var modeErrorCode: String? = null
    private var modeErrorDndMode = ""
    private var grantRequestedAt = 0L
    private var grantRationaleBefore: Map<String, Boolean> = emptyMap()

    private var fileUploadCallback: ValueCallback<Array<Uri>>? = null
    private var pendingExport: PendingExport? = null

    private data class PendingExport(
        val stagedFile: File?,
        val downloadUrl: String?,
        val filename: String,
        val mimeType: String,
    )

    private val modeListener = PhoneModeManager.Listener {
        activity.runOnUiThread { if (!activity.isDestroyed) refresh() }
    }

    private val pollRunnable = object : Runnable {
        override fun run() {
            if (activity.isDestroyed) return
            checkEmbeddedReady()
            refresh()
            handler.postDelayed(this, if (consoleLoaded) TICK_MS else POLL_READY_MS)
        }
    }

    private enum class CheckState { OK, MISSING, CHECK }

    private class CheckRow(root: View) {
        val icon: TextView = root.findViewById(R.id.checkIcon)
        val title: TextView = root.findViewById(R.id.checkTitle)
        val status: TextView = root.findViewById(R.id.checkStatus)
        val detail: TextView = root.findViewById(R.id.checkDetail)
        val action: Button = root.findViewById(R.id.checkAction)
    }

    private class PermissionItem(val permission: String, val rowId: Int, val labelRes: Int)

    private val runtimePermissions: List<PermissionItem> by lazy {
        val items = mutableListOf(
            PermissionItem(Manifest.permission.RECORD_AUDIO, R.id.saRowPermRecordAudio, R.string.perm_record_audio),
            PermissionItem(Manifest.permission.READ_PHONE_STATE, R.id.saRowPermPhoneState, R.string.perm_read_phone_state),
            PermissionItem(Manifest.permission.READ_CALL_LOG, R.id.saRowPermCallLog, R.string.perm_read_call_log),
            PermissionItem(Manifest.permission.ANSWER_PHONE_CALLS, R.id.saRowPermAnswerCalls, R.string.perm_answer_calls),
            PermissionItem(Manifest.permission.CALL_PHONE, R.id.saRowPermCallPhone, R.string.perm_call_phone),
            PermissionItem(Manifest.permission.READ_SMS, R.id.saRowPermSms, R.string.sa_perms_sms),
        )
        if (Build.VERSION.SDK_INT >= 33) {
            items += PermissionItem(
                Manifest.permission.POST_NOTIFICATIONS,
                R.id.saRowPermNotifications,
                R.string.perm_post_notifications,
            )
        }
        items
    }

    private val privilegedPermissions = listOf(
        PermissionItem("android.permission.CAPTURE_AUDIO_OUTPUT", R.id.saRowPermCapture, R.string.perm_capture_audio_output),
        PermissionItem("android.permission.MODIFY_PHONE_STATE", R.id.saRowPermModifyPhone, R.string.perm_modify_phone_state),
        PermissionItem("android.permission.CONTROL_INCALL_EXPERIENCE", R.id.saRowPermInCall, R.string.perm_control_incall),
    )

    @SuppressLint("SetJavaScriptEnabled")
    fun onCreate(savedInstanceState: Bundle?) {
        activity.setContentView(R.layout.activity_standalone)
        applySystemBarInsets(activity.findViewById(R.id.saRoot))

        dotView = activity.findViewById(R.id.saDot)
        headline = activity.findViewById(R.id.saHeadline)
        switchAi = activity.findViewById(R.id.saSwitchAi)
        btnDialer = activity.findViewById(R.id.saBtnDialer)
        btnSetup = activity.findViewById(R.id.saBtnSetup)
        btnLiveTakeover = activity.findViewById(R.id.saBtnLiveTakeover)
        modeBanner = activity.findViewById(R.id.saModeBanner)
        webView = activity.findViewById(R.id.saWebView)
        loadingOverlay = activity.findViewById(R.id.saLoading)
        drawer = activity.findViewById(R.id.saDrawer)
        statusDetail = activity.findViewById(R.id.saStatusDetail)
        stepsSection = activity.findViewById(R.id.saStepsSection)
        stepsTitle = activity.findViewById(R.id.saStepsTitle)
        switchDailySound = activity.findViewById(R.id.saSwitchDailySound)
        switchPersistentNotif = activity.findViewById(R.id.saSwitchPersistentNotif)
        switchSummaryNotif = activity.findViewById(R.id.saSwitchSummaryNotif)
        switchDndAuto = activity.findViewById(R.id.saSwitchDndAuto)
        chkDndAny = activity.findViewById(R.id.saChkDndAny)
        btnDndSettings = activity.findViewById(R.id.saBtnDndSettings)
        whitelistGroup = activity.findViewById(R.id.saWhitelistGroup)
        chkStarredWhitelist = activity.findViewById(R.id.saChkStarredWhitelist)
        btnSyncContacts = activity.findViewById(R.id.saBtnSyncContacts)
        syncStatusText = activity.findViewById(R.id.saSyncStatus)
        switchLan = activity.findViewById(R.id.saSwitchLan)
        lanBox = activity.findViewById(R.id.saLanBox)
        lanUrlText = activity.findViewById(R.id.saLanUrl)
        lanMdnsText = activity.findViewById(R.id.saLanMdns)
        btnCopyUrl = activity.findViewById(R.id.saBtnCopyUrl)
        advHeader = activity.findViewById(R.id.saAdvHeader)
        advBody = activity.findViewById(R.id.saAdvBody)
        serviceStatus = activity.findViewById(R.id.saServiceStatus)

        activity.findViewById<TextView>(R.id.saVersion).text =
            activity.getString(R.string.header_version, BuildConfig.VERSION_NAME, BuildConfig.VERSION_CODE)
        activity.findViewById<TextView>(R.id.saDevice).text = activity.getString(
            R.string.header_device,
            deviceName(),
            Build.VERSION.RELEASE,
            Build.VERSION.SDK_INT,
        )
        if (Build.VERSION.SDK_INT < 33) {
            activity.findViewById<View>(R.id.saRowPermNotifications).visibility = View.GONE
        }

        switchAi.setOnCheckedChangeListener { _, isChecked ->
            if (bindingAiSwitch) return@setOnCheckedChangeListener
            bindingAiSwitch = true
            switchAi.isChecked = !isChecked
            bindingAiSwitch = false
            confirmModeSwitch(if (isChecked) Prefs.PHONE_MODE_AI else Prefs.PHONE_MODE_NORMAL)
        }

        btnSetup.setOnClickListener {
            drawerOpen = drawer.visibility != View.VISIBLE
            refresh()
        }

        btnDialer.setOnClickListener {
            activity.startActivity(
                Intent(activity, OwnerCallActivity::class.java)
                    .putExtra("mode", OwnerCallActivity.MODE_DIALER),
            )
        }

        btnLiveTakeover.setOnClickListener {
            CallAudioService.requestOwnerTakeover(activity)
            refresh()
        }

        wireMobileControls()

        switchLan.setOnCheckedChangeListener { _, isChecked ->
            if (bindingLanSwitch) return@setOnCheckedChangeListener
            onLanSwitchToggled(isChecked)
        }

        btnCopyUrl.setOnClickListener {
            val st = EmbeddedAgent.status()
            val url = st.lanUrl.ifBlank { st.mdnsUrl }
            if (url.isNotBlank()) copyToClipboard(url)
        }

        advHeader.setOnClickListener {
            advOpen = !advOpen
            refresh()
        }

        activity.findViewById<Button>(R.id.saBtnStart).setOnClickListener {
            if (!ensurePerms()) return@setOnClickListener
            if (Prefs.isAiCallEnabled(activity)) {
                DialerRoleHelper.ensureRoles(activity)
                DialerRoleHelper.requestDialerRole(activity)
            }
            startCallService(CallAudioService.ACTION_START)
            Toast.makeText(activity, R.string.msg_service_started, Toast.LENGTH_SHORT).show()
            refreshSoon()
        }

        activity.findViewById<Button>(R.id.saBtnStop).setOnClickListener {
            activity.startService(
                Intent(activity, CallAudioService::class.java).setAction(CallAudioService.ACTION_STOP),
            )
            Toast.makeText(activity, R.string.msg_service_stopped, Toast.LENGTH_SHORT).show()
            refreshSoon()
        }

        configureWebView()

        EmbeddedAgent.ensureStarted(activity)
        handleLaunchIntent(activity.intent)
        if (Prefs.isAiCallEnabled(activity)) {
            if (ensurePerms()) {
                DialerRoleHelper.ensureRoles(activity)
                startCallService(CallAudioService.ACTION_START)
            }
        } else if (hasRequiredPerms()) {
            startCallService(CallAudioService.ACTION_START)
        }
        checkEmbeddedReady()
        refresh()
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
        PhoneModeManager.addListener(modeListener)
        handler.removeCallbacks(pollRunnable)
        handler.post(pollRunnable)
    }

    fun onPause() {
        PhoneModeManager.removeListener(modeListener)
        handler.removeCallbacks(pollRunnable)
    }

    fun onDestroy() {
        handler.removeCallbacks(pollRunnable)
        fileUploadCallback?.onReceiveValue(null)
        fileUploadCallback = null
        if (::webView.isInitialized) {
            webView.destroy()
        }
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
                runContactsSync()
            } else {
                Toast.makeText(activity, R.string.sa_contacts_perm_denied, Toast.LENGTH_SHORT).show()
            }
            refresh()
            return
        }
        if (hasRequiredPerms() && !isServiceRunning()) {
            startCallService(CallAudioService.ACTION_START)
        }
        refresh()
    }

    fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        if (requestCode == REQ_FILE_CHOOSER) {
            val cb = fileUploadCallback
            fileUploadCallback = null
            if (cb != null) {
                val uris = parseChooserResult(resultCode, data)
                cb.onReceiveValue(uris)
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
        refresh()
    }

    fun onBackPressed(): Boolean {
        if (drawer.visibility == View.VISIBLE) {
            drawerOpen = false
            refresh()
            return true
        }
        if (consoleLoaded) {
            webView.evaluateJavascript(
                "(function(){try{return window.__bageoHandleBack?window.__bageoHandleBack():false;}catch(e){return false;}})()",
            ) { raw ->
                val handled = raw == "true"
                if (!handled) {
                    if (webView.canGoBack()) {
                        webView.goBack()
                    } else {
                        activity.finish()
                    }
                }
            }
            return true
        }
        return false
    }

    // ---------------------------------------------------------------- WebView & JS Bridge

    @SuppressLint("SetJavaScriptEnabled")
    private fun configureWebView() {
        webView.settings.apply {
            javaScriptEnabled = true
            domStorageEnabled = true
            mediaPlaybackRequiresUserGesture = false
            allowFileAccess = false
            allowContentAccess = false
            mixedContentMode = WebSettings.MIXED_CONTENT_NEVER_ALLOW
        }
        webView.addJavascriptInterface(ConsoleJsBridge(), "BageoAndroid")
        webView.webViewClient = object : WebViewClient() {
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
                if (scheme in setOf("http", "https", "mailto", "tel")) {
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

    private fun checkEmbeddedReady() {
        val st = EmbeddedAgent.status()
        if (st.ready && !embeddedSettingsSynced) {
            embeddedSettingsSynced = true
            val action = Prefs.standaloneWhitelistAction(activity)
            EmbeddedAgent.patchSettings(
                activity,
                JSONObject().put("standalone_whitelist_action", action).toString(),
            )
        }
        if (st.ready && !consoleLoaded) {
            consoleLoaded = true
            loadingOverlay.visibility = View.GONE
            val baseUrl = EmbeddedAgent.localConsoleUrl(st.uiToken)
            val openCall = pendingOpenCallId
            pendingOpenCallId = null
            val url = if (!openCall.isNullOrBlank()) {
                "$baseUrl#call=${Uri.encode(openCall)}"
            } else {
                baseUrl
            }
            webView.loadUrl(url)
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

    private inner class ConsoleJsBridge {
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
                    launchCreateDocument(
                        PendingExport(tmp, null, cleanName, cleanMime),
                    )
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

    // ---------------------------------------------------------------- LAN Toggle + Password Dialog

    private fun onLanSwitchToggled(wantOn: Boolean) {
        if (!wantOn) {
            EmbeddedAgent.setLanEnabled(activity, false)
            refresh()
            return
        }
        val st = EmbeddedAgent.status()
        if (st.passwordSet) {
            EmbeddedAgent.setLanEnabled(activity, true)
            refresh()
            return
        }
        // Revert visual state until password >= 8 chars is confirmed.
        bindingLanSwitch = true
        switchLan.isChecked = false
        bindingLanSwitch = false
        showSetPasswordDialog()
    }

    private fun showSetPasswordDialog() {
        val pad = (20 * activity.resources.displayMetrics.density).toInt()
        val box = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, pad / 2, pad, 0)
        }
        val body = TextView(activity).apply {
            setText(R.string.sa_lan_pw_body)
        }
        val input = EditText(activity).apply {
            setHint(R.string.sa_lan_pw_hint)
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_PASSWORD
            setSingleLine(true)
            importantForAutofill = View.IMPORTANT_FOR_AUTOFILL_NO
        }
        val err = TextView(activity).apply {
            setTextColor(activity.getColor(R.color.pca_status_missing))
            visibility = View.GONE
            setPadding(0, pad / 3, 0, 0)
        }
        box.addView(body)
        box.addView(input)
        box.addView(err)

        val dlg = AlertDialog.Builder(activity)
            .setTitle(R.string.sa_lan_pw_title)
            .setView(box)
            .setPositiveButton(R.string.sa_btn_ok, null)
            .setNegativeButton(android.R.string.cancel, null)
            .create()

        dlg.setOnShowListener {
            dlg.getButton(DialogInterface.BUTTON_POSITIVE).setOnClickListener {
                val pw = input.text.toString().trim()
                if (pw.length < MIN_PASSWORD_LEN) {
                    err.setText(R.string.sa_lan_pw_short)
                    err.visibility = View.VISIBLE
                    return@setOnClickListener
                }
                if (!EmbeddedAgent.setConsolePassword(pw)) {
                    err.setText(R.string.sa_lan_pw_failed)
                    err.visibility = View.VISIBLE
                    return@setOnClickListener
                }
                if (consoleLoaded) {
                    webView.evaluateJavascript(
                        "try{localStorage.setItem('pca.token',${JSONObject.quote(pw)});}catch(e){}",
                        null,
                    )
                }
                EmbeddedAgent.setLanEnabled(activity, true)
                dlg.dismiss()
                refresh()
            }
        }
        dlg.show()
    }

    // ---------------------------------------------------------------- Status & Setup Checks

    private fun refresh() {
        val ai = Prefs.isAiCallEnabled(activity)
        val switching = PhoneModeManager.isSwitching()
        val missingSteps = bindSteps(ai)
        val st = EmbeddedAgent.status()

        bindingAiSwitch = true
        switchAi.isChecked = ai
        bindingAiSwitch = false
        switchAi.isEnabled = !switching

        val (colorRes, headlineText, detailText) = when {
            switching -> Triple(
                R.color.pca_status_check,
                activity.getString(R.string.st_switching),
                activity.getString(R.string.st_switching_d),
            )
            !ai -> Triple(
                R.color.pca_status_ok,
                activity.getString(R.string.st_normal),
                activity.getString(R.string.st_normal_d),
            )
            missingSteps > 0 -> Triple(
                R.color.pca_status_missing,
                activity.getString(R.string.st_steps, missingSteps),
                activity.getString(R.string.st_steps_d),
            )
            !isServiceRunning() || !st.ready -> Triple(
                R.color.pca_status_check,
                activity.getString(R.string.sa_starting_title),
                activity.getString(R.string.sa_starting_desc),
            )
            else -> Triple(
                R.color.pca_status_ok,
                activity.getString(R.string.st_ai_on),
                activity.getString(R.string.st_ai_on_d),
            )
        }
        headline.text = headlineText
        statusDetail.text = detailText
        setDotColor(activity.getColor(colorRes))

        val code = modeErrorCode
        if (code != null && !switching) {
            modeBanner.text = activity.getString(R.string.mode_failed, modeErrorText(code))
            modeBanner.visibility = View.VISIBLE
        } else {
            modeBanner.visibility = View.GONE
        }

        val setupLabel = activity.getString(R.string.sa_phone_setup_btn)
        btnSetup.text = if (missingSteps > 0) "$setupLabel ($missingSteps)" else setupLabel

        val open = drawerOpen ?: (missingSteps > 0)
        drawer.visibility = if (open) View.VISIBLE else View.GONE
        btnLiveTakeover.visibility = if (CallAudioService.isLiveAiCallActive()) View.VISIBLE else View.GONE

        bindMobileControls()
        bindLan(st)
        bindAdvanced()
    }

    private fun wireMobileControls() {
        switchDailySound.setOnCheckedChangeListener { _, isChecked ->
            if (bindingMobileControls) return@setOnCheckedChangeListener
            Prefs.setStandaloneDailySound(activity, isChecked)
            if (Prefs.isAiCallEnabled(activity) && !PhoneModeManager.isSwitching()) {
                startModeSwitch(Prefs.PHONE_MODE_AI)
            }
            LocalAudioGuard(activity).applyHardMute()
        }
        switchPersistentNotif.setOnCheckedChangeListener { _, isChecked ->
            if (bindingMobileControls) return@setOnCheckedChangeListener
            Prefs.setStandalonePersistentNotif(activity, isChecked)
            CallAudioService.refreshNotification(activity)
        }
        switchSummaryNotif.setOnCheckedChangeListener { _, isChecked ->
            if (bindingMobileControls) return@setOnCheckedChangeListener
            Prefs.setStandaloneCallSummaryNotif(activity, isChecked)
        }
        switchDndAuto.setOnCheckedChangeListener { _, isChecked ->
            if (bindingMobileControls) return@setOnCheckedChangeListener
            if (isChecked && !DedicatedDndManager.hasPolicyAccess(activity)) {
                bindingMobileControls = true
                switchDndAuto.isChecked = false
                bindingMobileControls = false
                launch(Intent(Settings.ACTION_NOTIFICATION_POLICY_ACCESS_SETTINGS))
                return@setOnCheckedChangeListener
            }
            Prefs.setStandaloneDndAutoAi(activity, isChecked)
            if (isChecked) DedicatedDndManager.ensureRule(activity)
        }
        chkDndAny.setOnCheckedChangeListener { _, isChecked ->
            if (bindingMobileControls) return@setOnCheckedChangeListener
            Prefs.setStandaloneDndAnyMode(activity, isChecked)
        }
        btnDndSettings.setOnClickListener {
            if (!DedicatedDndManager.hasPolicyAccess(activity)) {
                launch(Intent(Settings.ACTION_NOTIFICATION_POLICY_ACCESS_SETTINGS))
            } else {
                DedicatedDndManager.openSystemDndSettings(activity)
            }
        }
        whitelistGroup.setOnCheckedChangeListener { _, checkedId ->
            if (bindingMobileControls) return@setOnCheckedChangeListener
            val action = when (checkedId) {
                R.id.saRbRingPhone -> Prefs.WHITELIST_ACTION_RING_PHONE
                R.id.saRbForwardNumber -> Prefs.WHITELIST_ACTION_FORWARD_NUMBER
                R.id.saRbAiOnly -> Prefs.WHITELIST_ACTION_AI_ONLY
                else -> Prefs.WHITELIST_ACTION_URGENT_PROMPT
            }
            Prefs.setStandaloneWhitelistAction(activity, action)
            EmbeddedAgent.patchSettings(
                activity,
                JSONObject().put("standalone_whitelist_action", action).toString(),
            )
        }
        btnSyncContacts.setOnClickListener {
            if (!PhoneContactsSync.hasPermission(activity)) {
                ActivityCompat.requestPermissions(
                    activity,
                    arrayOf(Manifest.permission.READ_CONTACTS),
                    REQ_CONTACTS,
                )
                return@setOnClickListener
            }
            runContactsSync()
        }
    }

    private fun bindMobileControls() {
        bindingMobileControls = true
        try {
            switchDailySound.isChecked = Prefs.isStandaloneDailySound(activity)
            switchPersistentNotif.isChecked = Prefs.isStandalonePersistentNotif(activity)
            switchSummaryNotif.isChecked = Prefs.isStandaloneCallSummaryNotif(activity)
            switchDndAuto.isChecked = Prefs.isStandaloneDndAutoAi(activity)
            chkDndAny.isChecked = Prefs.isStandaloneDndAnyMode(activity)
            val checkedId = when (Prefs.standaloneWhitelistAction(activity)) {
                Prefs.WHITELIST_ACTION_RING_PHONE -> R.id.saRbRingPhone
                Prefs.WHITELIST_ACTION_FORWARD_NUMBER -> R.id.saRbForwardNumber
                Prefs.WHITELIST_ACTION_AI_ONLY -> R.id.saRbAiOnly
                else -> R.id.saRbUrgentPrompt
            }
            if (whitelistGroup.checkedRadioButtonId != checkedId) {
                whitelistGroup.check(checkedId)
            }
        } finally {
            bindingMobileControls = false
        }
    }

    private fun runContactsSync() {
        btnSyncContacts.isEnabled = false
        syncStatusText.setText(R.string.sa_contacts_syncing)
        val starredToWhitelist = chkStarredWhitelist.isChecked
        Thread({
            val res = PhoneContactsSync.syncNow(activity, starredToWhitelist)
            activity.runOnUiThread {
                if (activity.isDestroyed) return@runOnUiThread
                btnSyncContacts.isEnabled = true
                if (res.ok) {
                    syncStatusText.text = activity.getString(
                        R.string.sa_contacts_sync_done,
                        res.added,
                        res.merged,
                    )
                    if (consoleLoaded) webView.reload()
                } else {
                    syncStatusText.text = activity.getString(
                        R.string.sa_contacts_sync_fail,
                        res.error,
                    )
                }
            }
        }, "bageo-contacts-sync").start()
    }

    private fun bindLan(st: EmbeddedAgent.Status) {
        val prefOn = Prefs.isStandaloneLanEnabled(activity)
        bindingLanSwitch = true
        switchLan.isChecked = prefOn && st.passwordSet
        bindingLanSwitch = false

        val showBox = prefOn && st.lanEnabled && st.lanUrl.isNotBlank()
        lanBox.visibility = if (showBox) View.VISIBLE else View.GONE
        if (showBox) {
            lanUrlText.text = st.lanUrl
            if (st.mdnsUrl.isNotBlank()) {
                lanMdnsText.text = activity.getString(R.string.sa_lan_mdns_hint, st.mdnsUrl)
                lanMdnsText.visibility = View.VISIBLE
            } else {
                lanMdnsText.visibility = View.GONE
            }
        }
    }

    private fun bindSteps(ai: Boolean): Int {
        stepsSection.visibility = if (ai) View.VISIBLE else View.GONE
        if (!ai) return 0

        val roleManager = activity.getSystemService(RoleManager::class.java)
        val module = isPrivilegedSystemApp() && privilegedPermissions.all { isGranted(it.permission) }
        val perms = runtimePermissions.all { isGranted(it.permission) }
        val dialer = DialerRoleHelper.isDefaultDialer(activity) ||
            roleManager?.isRoleHeld(RoleManager.ROLE_DIALER) == true
        val screening = roleManager?.isRoleHeld(RoleManager.ROLE_CALL_SCREENING) == true
        val dnd = activity.getSystemService(NotificationManager::class.java)
            ?.isNotificationPolicyAccessGranted == true
        val battery = activity.getSystemService(PowerManager::class.java)
            ?.isIgnoringBatteryOptimizations(activity.packageName) == true

        row(R.id.saStepModule).bind(
            state = stepState(module),
            title = activity.getString(R.string.step_module_title),
            detail = activity.getString(if (module) R.string.step_module_ok else R.string.step_module_missing),
        )
        row(R.id.saStepPerms).bind(
            state = stepState(perms),
            title = activity.getString(R.string.step_perms_title),
            detail = activity.getString(R.string.sa_perms_desc),
            actionLabel = if (perms) 0 else R.string.btn_allow,
        ) { requestMissingRuntimePermissions() }
        row(R.id.saStepDialer).bind(
            state = stepState(dialer),
            title = activity.getString(R.string.step_dialer_title),
            detail = activity.getString(R.string.step_dialer_desc),
            actionLabel = if (dialer) 0 else R.string.btn_set,
        ) {
            DialerRoleHelper.requestDialerRole(activity)
            refresh()
        }
        row(R.id.saStepScreening).bind(
            state = stepState(screening),
            title = activity.getString(R.string.step_screening_title),
            detail = activity.getString(R.string.step_screening_desc),
            actionLabel = if (screening) 0 else R.string.btn_set,
        ) { requestScreeningRole() }
        row(R.id.saStepDnd).bind(
            state = stepState(dnd),
            title = activity.getString(R.string.step_dnd_title),
            detail = activity.getString(R.string.step_dnd_desc),
            actionLabel = if (dnd) 0 else R.string.btn_open_settings,
        ) { launch(Intent(Settings.ACTION_NOTIFICATION_POLICY_ACCESS_SETTINGS)) }
        row(R.id.saStepBattery).bind(
            state = stepState(battery),
            title = activity.getString(R.string.step_battery_title),
            detail = activity.getString(R.string.step_battery_desc),
            actionLabel = if (battery) 0 else R.string.btn_allow,
        ) { requestBatteryExemption() }

        val steps = listOf(module, perms, dialer, screening, dnd, battery)
        val done = steps.count { it }
        stepsTitle.text = activity.getString(R.string.steps_header, done, steps.size)
        return steps.size - done
    }

    private fun bindAdvanced() {
        advHeader.text = foldTitle(activity.getString(R.string.advanced), advOpen)
        advBody.visibility = if (advOpen) View.VISIBLE else View.GONE
        if (!advOpen) return

        serviceStatus.setText(if (isServiceRunning()) R.string.service_running else R.string.service_stopped)

        val sourceDir = activity.applicationInfo.sourceDir.orEmpty()
        val privileged = isPrivilegedSystemApp()
        row(R.id.saRowPriv).bind(
            state = if (privileged) CheckState.OK else CheckState.MISSING,
            title = activity.getString(R.string.check_priv_title),
            detail = activity.getString(
                if (privileged) R.string.check_priv_ok else R.string.check_priv_missing,
                sourceDir,
            ),
        )
        for (item in privilegedPermissions) {
            row(item.rowId).bind(
                state = if (isGranted(item.permission)) CheckState.OK else CheckState.MISSING,
                title = activity.getString(item.labelRes),
                detail = item.permission.substringAfterLast('.'),
            )
        }
        for (item in runtimePermissions) {
            row(item.rowId).bind(
                state = if (isGranted(item.permission)) CheckState.OK else CheckState.MISSING,
                title = activity.getString(item.labelRes),
                detail = item.permission.substringAfterLast('.'),
            )
        }

        val canInject = isGranted("android.permission.MODIFY_PHONE_STATE")
        val telephonyOutput = try {
            TelephonyUplinkInjector(activity).isAvailable()
        } catch (_: Exception) {
            false
        }
        row(R.id.saRowUplink).bind(
            state = when {
                !canInject -> CheckState.MISSING
                telephonyOutput -> CheckState.OK
                else -> CheckState.CHECK
            },
            title = activity.getString(R.string.check_uplink_title),
            detail = activity.getString(
                when {
                    !canInject -> R.string.check_uplink_missing
                    telephonyOutput -> R.string.check_uplink_ok
                    else -> R.string.check_uplink_idle
                },
            ),
        )
    }

    // ---------------------------------------------------------------- Helpers

    private fun confirmModeSwitch(target: String) {
        val toAi = target == Prefs.PHONE_MODE_AI
        AlertDialog.Builder(activity)
            .setTitle(if (toAi) R.string.confirm_ai_title else R.string.confirm_normal_title)
            .setMessage(if (toAi) R.string.confirm_ai_body else R.string.confirm_normal_body)
            .setPositiveButton(R.string.btn_switch) { _, _ -> startModeSwitch(target) }
            .setNegativeButton(android.R.string.cancel, null)
            .show()
    }

    private fun startModeSwitch(target: String) {
        modeErrorCode = null
        modeErrorDndMode = ""
        PhoneModeManager.requestSwitch(activity, target, callBusy = false) { result ->
            activity.runOnUiThread {
                if (activity.isDestroyed) return@runOnUiThread
                modeErrorCode = if (result.ok) null else result.error
                modeErrorDndMode = if (result.ok) "" else result.dndMode
                if (result.mode == target && hasRequiredPerms()) {
                    startCallService(CallAudioService.ACTION_START)
                }
                refresh()
                refreshSoon()
            }
        }
        refresh()
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

    private fun handleLaunchIntent(intent: Intent?) {
        intent?.getStringExtra("open_call_id")?.trim()?.takeIf { it.isNotBlank() }?.let { callId ->
            pendingOpenCallId = callId
            drawerOpen = false
            checkEmbeddedReady()
        }
        when (intent?.action) {
            CallAudioService.ACTION_SMOKE_CONNECT,
            CallAudioService.ACTION_START,
            -> if (ensurePerms()) startCallService(intent.action!!)
        }
    }

    private fun startCallService(action: String) {
        try {
            ContextCompat.startForegroundService(
                activity,
                Intent(activity, CallAudioService::class.java).setAction(action),
            )
        } catch (e: Exception) {
            Log.w(TAG, "start service $action failed: ${e.message}")
        }
    }

    private fun ensurePerms(): Boolean {
        val missing = requiredPermissions().filter { !isGranted(it) }
        if (missing.isNotEmpty()) {
            ActivityCompat.requestPermissions(activity, missing.toTypedArray(), REQ_REQUIRED_PERMS)
            return false
        }
        return true
    }

    private fun hasRequiredPerms(): Boolean = requiredPermissions().all { isGranted(it) }

    private fun requiredPermissions(): List<String> {
        val need = mutableListOf(
            Manifest.permission.RECORD_AUDIO,
            Manifest.permission.READ_PHONE_STATE,
            Manifest.permission.ANSWER_PHONE_CALLS,
            Manifest.permission.READ_SMS,
            Manifest.permission.RECEIVE_SMS,
        )
        if (Build.VERSION.SDK_INT >= 33) {
            need += Manifest.permission.POST_NOTIFICATIONS
        }
        return need
    }

    private fun requestMissingRuntimePermissions() {
        val missing = runtimePermissions.map { it.permission }.filter { !isGranted(it) }
        if (missing.isEmpty()) {
            refresh()
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
            refresh()
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
        Toast.makeText(activity, R.string.sa_copied, Toast.LENGTH_SHORT).show()
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
        handler.postDelayed({ if (!activity.isDestroyed) refresh() }, 700L)
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

    private fun setDotColor(color: Int) {
        val gd = (dotView.background as? GradientDrawable) ?: GradientDrawable().apply {
            shape = GradientDrawable.OVAL
            dotView.background = this
        }
        gd.setColor(color)
    }

    private fun stepState(done: Boolean): CheckState = if (done) CheckState.OK else CheckState.MISSING

    private fun foldTitle(title: String, open: Boolean): String =
        "$title  ${if (open) "\u25BE" else "\u25B8"}"

    private fun row(id: Int): CheckRow {
        val root = activity.findViewById<View>(id)
        return (root.tag as? CheckRow) ?: CheckRow(root).also { root.tag = it }
    }

    private fun CheckRow.bind(
        state: CheckState,
        title: String,
        detail: String,
        actionLabel: Int = 0,
        onAction: (() -> Unit)? = null,
    ) {
        val color = activity.getColor(
            when (state) {
                CheckState.OK -> R.color.pca_status_ok
                CheckState.MISSING -> R.color.pca_status_missing
                CheckState.CHECK -> R.color.pca_status_check
            },
        )
        icon.text = when (state) {
            CheckState.OK -> "\u2713"
            CheckState.MISSING -> "\u2717"
            CheckState.CHECK -> "!"
        }
        icon.setTextColor(color)
        this.title.text = title
        status.setText(
            when (state) {
                CheckState.OK -> R.string.status_ok
                CheckState.MISSING -> R.string.status_missing
                CheckState.CHECK -> R.string.status_check
            },
        )
        status.setTextColor(color)
        this.detail.text = detail
        if (actionLabel != 0 && onAction != null) {
            action.setText(actionLabel)
            action.setOnClickListener { onAction() }
            action.visibility = View.VISIBLE
        } else {
            action.setOnClickListener(null)
            action.visibility = View.GONE
        }
    }

    private fun deviceName(): String {
        val m = Build.MANUFACTURER.orEmpty().trim()
        val model = Build.MODEL.orEmpty().trim()
        return if (m.isEmpty() || model.startsWith(m, ignoreCase = true)) model else "$m $model"
    }

    private fun applySystemBarInsets(root: View) {
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
        private const val REQ_REQUIRED_PERMS = 100
        private const val REQ_GRANT_ALL = 101
        private const val REQ_CONTACTS = 102
        private const val REQ_SCREENING_ROLE = 7102
        private const val REQ_FILE_CHOOSER = 7103
        private const val REQ_CREATE_DOC = 7104
        private const val POLL_READY_MS = 250L
        private const val TICK_MS = 3_000L
        private const val INSTANT_DENIAL_MS = 350L
    }
}
