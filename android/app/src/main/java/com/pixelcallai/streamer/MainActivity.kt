package com.pixelcallai.streamer

import android.Manifest
import android.app.Activity
import android.app.ActivityManager
import android.app.AlertDialog
import android.app.NotificationManager
import android.app.role.RoleManager
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.os.SystemClock
import android.provider.Settings
import android.telephony.TelephonyManager
import android.util.Log
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.Switch
import android.widget.TextView
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import org.java_websocket.client.WebSocketClient
import org.java_websocket.drafts.Draft_6455
import org.java_websocket.handshake.ServerHandshake
import java.net.ConnectException
import java.net.NoRouteToHostException
import java.net.SocketTimeoutException
import java.net.URI
import java.net.UnknownHostException
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import javax.net.ssl.SSLException

/**
 * Setup / status screen. Framework Activity and plain views (no AppCompat) so the
 * app stays single-dex and loads reliably as a privileged system app.
 *
 * At the top, one plain sentence about what the phone does right now and the switch
 * between "the AI answers my calls" and "normal phone" ([PhoneModeManager]). Below it
 * the setup steps the AI mode needs, and under "Advanced" the connection settings and
 * the technical checks. Nothing here is on the call path.
 */
class MainActivity : Activity() {
    private lateinit var statusHeadline: TextView
    private lateinit var statusDetail: TextView
    private lateinit var legacyHint: TextView
    private lateinit var btnStatusAction: Button
    private lateinit var switchAi: Switch
    private lateinit var switchProgress: View
    private lateinit var modeError: TextView
    private lateinit var btnConsole: Button
    private lateinit var status: TextView
    private lateinit var stepsSection: View
    private lateinit var stepsHeader: TextView
    private lateinit var stepsBody: View
    private lateinit var advancedHeader: TextView
    private lateinit var advancedBody: View
    private lateinit var serviceStatus: TextView
    private lateinit var agentUrlField: EditText
    private lateinit var urlMessage: TextView
    private lateinit var btnTest: Button
    private lateinit var testResult: TextView
    private lateinit var testDetailToggle: TextView
    private lateinit var testDetail: TextView

    private val handler = Handler(Looper.getMainLooper())
    private val delayedRefresh = Runnable { refresh() }

    /** Re-reads the state every few seconds while the screen is visible (connection, switch). */
    private val ticker = object : Runnable {
        override fun run() {
            refresh()
            handler.postDelayed(this, TICK_MS)
        }
    }

    private val modeListener = PhoneModeManager.Listener { runOnUiThread { refresh() } }

    /** True while [refresh] updates the switch, so the listener ignores it. */
    private var bindingSwitch = false

    /** Invalidates the result of an older connection test. */
    private var testSequence = 0

    private var grantRequestedAt = 0L
    private var grantRationaleBefore: Map<String, Boolean> = emptyMap()

    /** Null = open while a step is missing, closed when all are done. Tapping the title decides. */
    private var stepsOpen: Boolean? = null
    private var advancedOpen = false

    /** Error code of the last switch made on this screen, shown until the next one. */
    private var modeErrorCode: String? = null

    private enum class CheckState { OK, MISSING, CHECK }

    private class StatusLine(
        val state: CheckState,
        val headline: String,
        val detail: String,
        val action: Int = 0,
    )

    private class CheckRow(root: View) {
        val root: View = root
        val icon: TextView = root.findViewById(R.id.checkIcon)
        val title: TextView = root.findViewById(R.id.checkTitle)
        val status: TextView = root.findViewById(R.id.checkStatus)
        val detail: TextView = root.findViewById(R.id.checkDetail)
        val action: Button = root.findViewById(R.id.checkAction)
    }

    private class PermissionItem(val permission: String, val rowId: Int, val labelRes: Int)

    /** Runtime permissions the user can grant, each with what it is used for. */
    private val runtimePermissions: List<PermissionItem> by lazy {
        val items = mutableListOf(
            PermissionItem(Manifest.permission.RECORD_AUDIO, R.id.rowPermRecordAudio, R.string.perm_record_audio),
            PermissionItem(Manifest.permission.READ_PHONE_STATE, R.id.rowPermPhoneState, R.string.perm_read_phone_state),
            PermissionItem(Manifest.permission.READ_CALL_LOG, R.id.rowPermCallLog, R.string.perm_read_call_log),
            PermissionItem(Manifest.permission.ANSWER_PHONE_CALLS, R.id.rowPermAnswerCalls, R.string.perm_answer_calls),
            PermissionItem(Manifest.permission.CALL_PHONE, R.id.rowPermCallPhone, R.string.perm_call_phone),
        )
        if (Build.VERSION.SDK_INT >= 33) {
            items += PermissionItem(
                Manifest.permission.POST_NOTIFICATIONS,
                R.id.rowPermNotifications,
                R.string.perm_post_notifications,
            )
        }
        items
    }

    /** Privileged permissions: only the module can grant them (informational rows). */
    private val privilegedPermissions = listOf(
        PermissionItem("android.permission.CAPTURE_AUDIO_OUTPUT", R.id.rowPermCapture, R.string.perm_capture_audio_output),
        PermissionItem("android.permission.MODIFY_PHONE_STATE", R.id.rowPermModifyPhone, R.string.perm_modify_phone_state),
        PermissionItem("android.permission.CONTROL_INCALL_EXPERIENCE", R.id.rowPermInCall, R.string.perm_control_incall),
    )

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        applySystemBarInsets(findViewById(R.id.root))

        statusHeadline = findViewById(R.id.statusHeadline)
        statusDetail = findViewById(R.id.statusDetail)
        legacyHint = findViewById(R.id.legacyHint)
        btnStatusAction = findViewById(R.id.btnStatusAction)
        switchAi = findViewById(R.id.switchAi)
        switchProgress = findViewById(R.id.switchProgress)
        modeError = findViewById(R.id.modeError)
        btnConsole = findViewById(R.id.btnConsole)
        status = findViewById(R.id.status)
        stepsSection = findViewById(R.id.stepsSection)
        stepsHeader = findViewById(R.id.stepsHeader)
        stepsBody = findViewById(R.id.stepsBody)
        advancedHeader = findViewById(R.id.advancedHeader)
        advancedBody = findViewById(R.id.advancedBody)
        serviceStatus = findViewById(R.id.serviceStatus)
        agentUrlField = findViewById(R.id.agentUrl)
        urlMessage = findViewById(R.id.urlMessage)
        btnTest = findViewById(R.id.btnTest)
        testResult = findViewById(R.id.testResult)
        testDetailToggle = findViewById(R.id.testDetailToggle)
        testDetail = findViewById(R.id.testDetail)

        findViewById<TextView>(R.id.headerVersion).text =
            getString(R.string.header_version, BuildConfig.VERSION_NAME, BuildConfig.VERSION_CODE)
        findViewById<TextView>(R.id.headerDevice).text = getString(
            R.string.header_device,
            deviceName(),
            Build.VERSION.RELEASE,
            Build.VERSION.SDK_INT,
        )
        if (Build.VERSION.SDK_INT < 33) {
            findViewById<View>(R.id.rowPermNotifications).visibility = View.GONE
        }

        switchAi.setOnCheckedChangeListener { _, isChecked ->
            if (bindingSwitch) return@setOnCheckedChangeListener
            // The switch always shows the phone's real mode: put it back until the
            // switch is confirmed and done.
            bindingSwitch = true
            switchAi.isChecked = !isChecked
            bindingSwitch = false
            confirmModeSwitch(if (isChecked) Prefs.PHONE_MODE_AI else Prefs.PHONE_MODE_NORMAL)
        }
        btnStatusAction.setOnClickListener {
            if (ensurePerms()) armService(CallAudioService.ACTION_START)
        }
        btnConsole.setOnClickListener { openConsole() }
        stepsHeader.setOnClickListener {
            stepsOpen = stepsBody.visibility != View.VISIBLE
            refresh()
        }
        advancedHeader.setOnClickListener {
            advancedOpen = !advancedOpen
            refresh()
        }

        findViewById<Button>(R.id.btnStart).setOnClickListener {
            if (!ensurePerms()) return@setOnClickListener
            if (Prefs.isAiCallEnabled(this)) {
                DialerRoleHelper.ensureRoles(this)
                DialerRoleHelper.requestDialerRole(this)
            }
            armService(CallAudioService.ACTION_START)
        }
        findViewById<Button>(R.id.btnStop).setOnClickListener {
            startService(Intent(this, CallAudioService::class.java).setAction(CallAudioService.ACTION_STOP))
            showStatus(getString(R.string.msg_service_stopped))
            refresh()
            refreshSoon()
        }

        agentUrlField.hint = BuildConfig.BRIDGE_WS_URL
        agentUrlField.setText(Prefs.agentUrl(this))
        findViewById<Button>(R.id.btnSaveUrl).setOnClickListener {
            saveAgentUrl(agentUrlField.text.toString())
        }
        findViewById<Button>(R.id.btnResetUrl).setOnClickListener {
            saveAgentUrl(BuildConfig.BRIDGE_WS_URL)
        }
        btnTest.setOnClickListener { testConnection() }
        testDetailToggle.setOnClickListener {
            val show = testDetail.visibility != View.VISIBLE
            testDetail.visibility = if (show) View.VISIBLE else View.GONE
            testDetailToggle.text = foldTitle(getString(R.string.test_details), show)
        }

        handleLaunchIntent(intent)
        // The service runs in both modes (in normal mode it only keeps the agent connection).
        if (Prefs.isAiCallEnabled(this)) {
            if (ensurePerms()) {
                DialerRoleHelper.ensureRoles(this)
                startCallService(CallAudioService.ACTION_START)
            }
        } else if (hasRequiredPerms()) {
            startCallService(CallAudioService.ACTION_START)
        }
        refresh()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleLaunchIntent(intent)
    }

    private fun handleLaunchIntent(intent: Intent?) {
        val action = intent?.action ?: return
        when (action) {
            CallAudioService.ACTION_SMOKE_CONNECT,
            CallAudioService.ACTION_START,
            -> {
                if (ensurePerms()) {
                    armService(action)
                    Log.i(TAG, "launch intent -> service action=$action")
                }
            }
        }
    }

    private fun armService(action: String) {
        startCallService(action)
        showStatus(getString(R.string.msg_service_started))
        refresh()
        refreshSoon()
    }

    private fun startCallService(action: String) {
        try {
            ContextCompat.startForegroundService(
                this,
                Intent(this, CallAudioService::class.java).setAction(action),
            )
        } catch (e: Exception) {
            Log.w(TAG, "start service $action failed: ${e.message}")
        }
    }

    override fun onResume() {
        super.onResume()
        PhoneModeManager.addListener(modeListener)
        handler.removeCallbacks(ticker)
        handler.post(ticker)
    }

    override fun onPause() {
        PhoneModeManager.removeListener(modeListener)
        handler.removeCallbacks(ticker)
        super.onPause()
    }

    override fun onDestroy() {
        handler.removeCallbacks(delayedRefresh)
        handler.removeCallbacks(ticker)
        testSequence++
        super.onDestroy()
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray,
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == REQ_GRANT_ALL && permissions.isNotEmpty()) {
            // "Permanently denied": still denied, Android will not ask again, and either it
            // asked before or it answered without showing a dialog.
            val answeredInstantly =
                SystemClock.elapsedRealtime() - grantRequestedAt < INSTANT_DENIAL_MS
            val blocked = permissions.filterIndexed { index, permission ->
                grantResults.getOrNull(index) != PackageManager.PERMISSION_GRANTED &&
                    !shouldShowRequestPermissionRationale(permission) &&
                    (grantRationaleBefore[permission] == true || answeredInstantly)
            }
            if (blocked.isNotEmpty()) {
                showStatus(getString(R.string.msg_perm_settings))
                openAppSettings()
            }
        }
        refresh()
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        refresh()
    }

    // ---------------------------------------------------------------- mode switch

    private fun confirmModeSwitch(target: String) {
        val toAi = target == Prefs.PHONE_MODE_AI
        AlertDialog.Builder(this)
            .setTitle(if (toAi) R.string.confirm_ai_title else R.string.confirm_normal_title)
            .setMessage(if (toAi) R.string.confirm_ai_body else R.string.confirm_normal_body)
            .setPositiveButton(R.string.btn_switch) { _, _ -> startModeSwitch(target) }
            .setNegativeButton(android.R.string.cancel, null)
            .show()
    }

    private fun startModeSwitch(target: String) {
        modeErrorCode = null
        PhoneModeManager.requestSwitch(this, target, callBusy = false) { result ->
            runOnUiThread {
                if (isDestroyed) return@runOnUiThread
                modeErrorCode = if (result.ok) null else result.error
                // Same service in both modes: (re)start it so it runs the new one.
                if (result.mode == target && hasRequiredPerms()) {
                    startCallService(CallAudioService.ACTION_START)
                }
                refresh()
                refreshSoon()
            }
        }
        refresh()
    }

    private fun modeErrorText(code: String): String = getString(
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

    private fun openConsole() {
        val url = Prefs.consoleUrl(this) ?: return
        if (!launch(Intent(Intent.ACTION_VIEW, Uri.parse(url)))) {
            showStatus(getString(R.string.msg_no_browser))
        }
    }

    // ---------------------------------------------------------------- actions

    /** The permissions the service cannot start without. Asks for the missing ones. */
    private fun ensurePerms(): Boolean {
        val missing = requiredPermissions().filter { !isGranted(it) }
        if (missing.isNotEmpty()) {
            ActivityCompat.requestPermissions(this, missing.toTypedArray(), REQ_REQUIRED_PERMS)
            showStatus(getString(R.string.grant_perms))
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
        grantRationaleBefore = missing.associateWith { shouldShowRequestPermissionRationale(it) }
        grantRequestedAt = SystemClock.elapsedRealtime()
        ActivityCompat.requestPermissions(this, missing.toTypedArray(), REQ_GRANT_ALL)
    }

    private fun openAppSettings() {
        launch(
            Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.parse("package:$packageName")),
        )
    }

    private fun requestScreeningRole() {
        val rm = getSystemService(RoleManager::class.java) ?: return
        if (!rm.isRoleAvailable(RoleManager.ROLE_CALL_SCREENING) ||
            rm.isRoleHeld(RoleManager.ROLE_CALL_SCREENING)
        ) {
            refresh()
            return
        }
        try {
            startActivityForResult(
                rm.createRequestRoleIntent(RoleManager.ROLE_CALL_SCREENING),
                REQ_SCREENING_ROLE,
            )
        } catch (e: Exception) {
            Log.w(TAG, "ROLE_CALL_SCREENING request failed: ${e.message}")
        }
    }

    private fun requestBatteryExemption() {
        val ok = launch(
            Intent(
                Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
                Uri.parse("package:$packageName"),
            ),
        )
        if (!ok) launch(Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS))
    }

    private fun launch(intent: Intent): Boolean = try {
        startActivity(intent)
        true
    } catch (e: Exception) {
        Log.w(TAG, "startActivity ${intent.action} failed: ${e.message}")
        false
    }

    private fun saveAgentUrl(raw: String) {
        val url = raw.trim()
        if (!Prefs.isValidAgentUrl(url)) {
            showUrlMessage(getString(R.string.msg_url_invalid), CheckState.MISSING)
            return
        }
        val before = Prefs.agentUrl(this)
        Prefs.setAgentUrl(this, url)
        val after = Prefs.agentUrl(this)
        agentUrlField.setText(after)
        val message = when {
            after == before || !isServiceRunning() -> R.string.msg_url_saved
            !isPhoneIdle() -> R.string.msg_url_saved_pending
            else -> {
                // The service reads the address when it connects: restart it with the same
                // STOP / START intents as the buttons (it runs in both modes).
                startService(
                    Intent(this, CallAudioService::class.java).setAction(CallAudioService.ACTION_STOP),
                )
                startCallService(CallAudioService.ACTION_START)
                refreshSoon()
                R.string.msg_url_saved_restarted
            }
        }
        showUrlMessage(getString(message), CheckState.OK)
    }

    /** Opens a WebSocket to the address in the field, reports the result, sends nothing. */
    private fun testConnection() {
        val url = agentUrlField.text.toString().trim()
        if (!Prefs.isValidAgentUrl(url)) {
            showTestResult(getString(R.string.msg_url_invalid), CheckState.MISSING, null)
            return
        }
        val sequence = ++testSequence
        btnTest.isEnabled = false
        showTestResult(getString(R.string.test_running), null, null)
        Thread({
            val error = AtomicReference<Throwable?>(null)
            val closed = AtomicReference<String?>(null)
            var connected = false
            var client: WebSocketClient? = null
            try {
                val ws = object : WebSocketClient(URI(url), Draft_6455(), null, TEST_TIMEOUT_MS) {
                    override fun onOpen(handshakedata: ServerHandshake?) {}

                    override fun onMessage(message: String?) {}

                    override fun onClose(code: Int, closeReason: String?, remote: Boolean) {
                        val reason = closeReason?.trim().orEmpty()
                        closed.compareAndSet(null, if (reason.isEmpty()) "close $code" else "close $code: $reason")
                    }

                    override fun onError(ex: Exception?) {
                        if (ex != null) error.compareAndSet(null, ex)
                    }
                }
                client = ws
                connected = ws.connectBlocking(TEST_TIMEOUT_MS.toLong(), TimeUnit.MILLISECONDS)
            } catch (e: Exception) {
                error.compareAndSet(null, e)
            }
            // Read before closing: our own close below reports a close code too.
            val errorSeen = error.get()
            val closedSeen = closed.get()
            try {
                client?.close()
            } catch (_: Exception) {
            }
            val failure = if (connected) 0 else classifyFailure(errorSeen, closedSeen)
            val detail = listOfNotNull(
                errorSeen?.let { "${it.javaClass.simpleName}: ${it.message.orEmpty()}" },
                closedSeen,
            ).joinToString("\n")
            runOnUiThread {
                if (isDestroyed || sequence != testSequence) return@runOnUiThread
                btnTest.isEnabled = true
                if (connected) {
                    showTestResult(getString(R.string.test_ok), CheckState.OK, null)
                } else {
                    showTestResult(
                        getString(R.string.test_failed, getString(failure)),
                        CheckState.MISSING,
                        detail.ifBlank { null },
                    )
                }
            }
        }, "agent-connection-test").apply { isDaemon = true }.start()
    }

    /** A plain reason for a failed test; the raw error text goes to "Details". */
    private fun classifyFailure(error: Throwable?, closed: String?): Int {
        val chain = generateSequence(error) { it.cause }.take(6).toList()
        val text = (chain.joinToString(" ") { "${it.javaClass.name} ${it.message.orEmpty()}" } +
            " " + closed.orEmpty()).lowercase()
        return when {
            chain.any { it is UnknownHostException } -> R.string.test_fail_host
            chain.any { it is SSLException } -> R.string.test_fail_tls
            chain.any { it is SocketTimeoutException } -> R.string.test_fail_timeout
            chain.any { it is ConnectException } || "refused" in text -> R.string.test_fail_refused
            chain.any { it is NoRouteToHostException } || "unreachable" in text -> R.string.test_fail_network
            " 401" in text || " 403" in text -> R.string.test_fail_auth
            "invalid status code" in text || "handshake" in text -> R.string.test_fail_rejected
            error == null && closed == null -> R.string.test_fail_timeout
            else -> R.string.test_fail_other
        }
    }

    // ----------------------------------------------------------------- status

    private fun refresh() {
        val ai = Prefs.isAiCallEnabled(this)
        val missing = bindSteps(ai)
        bindStatus(ai, PhoneModeManager.isSwitching(), missing)
        bindAdvanced()
    }

    private fun bindStatus(ai: Boolean, switching: Boolean, missingSteps: Int) {
        bindingSwitch = true
        switchAi.isChecked = ai
        bindingSwitch = false
        switchAi.isEnabled = !switching
        switchProgress.visibility = if (switching) View.VISIBLE else View.GONE

        val line = when {
            switching -> StatusLine(
                CheckState.CHECK,
                getString(R.string.st_switching),
                getString(R.string.st_switching_d),
            )
            !ai -> StatusLine(CheckState.OK, getString(R.string.st_normal), getString(R.string.st_normal_d))
            missingSteps > 0 -> StatusLine(
                CheckState.MISSING,
                getString(R.string.st_steps, missingSteps),
                getString(R.string.st_steps_d),
            )
            !isServiceRunning() -> StatusLine(
                CheckState.MISSING,
                getString(R.string.st_service_off),
                getString(R.string.st_service_off_d),
                R.string.btn_start,
            )
            !AgentControlClient.isAgentConnected() -> StatusLine(
                CheckState.MISSING,
                getString(R.string.st_offline),
                getString(R.string.st_offline_d),
            )
            else -> StatusLine(CheckState.OK, getString(R.string.st_ai_on), getString(R.string.st_ai_on_d))
        }
        statusHeadline.text = line.headline
        statusHeadline.setTextColor(stateColor(line.state))
        statusDetail.text = line.detail
        // Upgraded from a version whose "AI answering" switch was off: it now answers.
        legacyHint.visibility = if (ai && !switching && Prefs.legacyAiSwitchOff(this)) View.VISIBLE else View.GONE
        if (line.action != 0) {
            btnStatusAction.setText(line.action)
            btnStatusAction.visibility = View.VISIBLE
        } else {
            btnStatusAction.visibility = View.GONE
        }

        val code = modeErrorCode
        if (code != null && !switching) {
            modeError.text = getString(R.string.mode_failed, modeErrorText(code))
            modeError.visibility = View.VISIBLE
        } else {
            modeError.visibility = View.GONE
        }
        btnConsole.visibility = if (Prefs.consoleUrl(this) != null) View.VISIBLE else View.GONE
    }

    /** The setup steps the AI mode needs. Returns how many are not done (0 in normal mode). */
    private fun bindSteps(ai: Boolean): Int {
        stepsSection.visibility = if (ai) View.VISIBLE else View.GONE
        if (!ai) return 0

        val roleManager = getSystemService(RoleManager::class.java)
        val module = isPrivilegedSystemApp() && privilegedPermissions.all { isGranted(it.permission) }
        val perms = runtimePermissions.all { isGranted(it.permission) }
        val dialer = DialerRoleHelper.isDefaultDialer(this) ||
            roleManager?.isRoleHeld(RoleManager.ROLE_DIALER) == true
        val screening = roleManager?.isRoleHeld(RoleManager.ROLE_CALL_SCREENING) == true
        val dnd = getSystemService(NotificationManager::class.java)
            ?.isNotificationPolicyAccessGranted == true
        val battery = getSystemService(PowerManager::class.java)
            ?.isIgnoringBatteryOptimizations(packageName) == true

        row(R.id.stepModule).bind(
            state = stepState(module),
            title = getString(R.string.step_module_title),
            detail = getString(if (module) R.string.step_module_ok else R.string.step_module_missing),
        )
        row(R.id.stepPerms).bind(
            state = stepState(perms),
            title = getString(R.string.step_perms_title),
            detail = getString(R.string.step_perms_desc),
            actionLabel = if (perms) 0 else R.string.btn_allow,
        ) { requestMissingRuntimePermissions() }
        row(R.id.stepDialer).bind(
            state = stepState(dialer),
            title = getString(R.string.step_dialer_title),
            detail = getString(R.string.step_dialer_desc),
            actionLabel = if (dialer) 0 else R.string.btn_set,
        ) {
            DialerRoleHelper.requestDialerRole(this)
            refresh()
        }
        row(R.id.stepScreening).bind(
            state = stepState(screening),
            title = getString(R.string.step_screening_title),
            detail = getString(R.string.step_screening_desc),
            actionLabel = if (screening) 0 else R.string.btn_set,
        ) { requestScreeningRole() }
        row(R.id.stepDnd).bind(
            state = stepState(dnd),
            title = getString(R.string.step_dnd_title),
            detail = getString(R.string.step_dnd_desc),
            actionLabel = if (dnd) 0 else R.string.btn_open_settings,
        ) { launch(Intent(Settings.ACTION_NOTIFICATION_POLICY_ACCESS_SETTINGS)) }
        row(R.id.stepBattery).bind(
            state = stepState(battery),
            title = getString(R.string.step_battery_title),
            detail = getString(R.string.step_battery_desc),
            actionLabel = if (battery) 0 else R.string.btn_allow,
        ) { requestBatteryExemption() }

        val steps = listOf(module, perms, dialer, screening, dnd, battery)
        val done = steps.count { it }
        val open = stepsOpen ?: (done < steps.size)
        stepsHeader.text = foldTitle(getString(R.string.steps_header, done, steps.size), open)
        stepsBody.visibility = if (open) View.VISIBLE else View.GONE
        return steps.size - done
    }

    private fun bindAdvanced() {
        advancedHeader.text = foldTitle(getString(R.string.advanced), advancedOpen)
        advancedBody.visibility = if (advancedOpen) View.VISIBLE else View.GONE
        if (!advancedOpen) return
        serviceStatus.setText(
            if (isServiceRunning()) R.string.service_running else R.string.service_stopped,
        )

        // Installed by the Magisk / KernelSU module as a privileged system app.
        val sourceDir = applicationInfo.sourceDir.orEmpty()
        val privileged = isPrivilegedSystemApp()
        row(R.id.rowPriv).bind(
            state = if (privileged) CheckState.OK else CheckState.MISSING,
            title = getString(R.string.check_priv_title),
            detail = getString(
                if (privileged) R.string.check_priv_ok else R.string.check_priv_missing,
                sourceDir,
            ),
        )
        for (item in privilegedPermissions) {
            row(item.rowId).bind(
                state = if (isGranted(item.permission)) CheckState.OK else CheckState.MISSING,
                title = getString(item.labelRes),
                detail = item.permission.substringAfterLast('.'),
            )
        }
        for (item in runtimePermissions) {
            row(item.rowId).bind(
                state = if (isGranted(item.permission)) CheckState.OK else CheckState.MISSING,
                title = getString(item.labelRes),
                detail = item.permission.substringAfterLast('.'),
            )
        }

        // Uplink injection: the same TYPE_TELEPHONY device check the call path uses.
        val canInject = isGranted("android.permission.MODIFY_PHONE_STATE")
        val telephonyOutput = try {
            TelephonyUplinkInjector(this).isAvailable()
        } catch (e: Exception) {
            Log.w(TAG, "uplink check failed: ${e.message}")
            false
        }
        row(R.id.rowUplink).bind(
            state = when {
                !canInject -> CheckState.MISSING
                telephonyOutput -> CheckState.OK
                else -> CheckState.CHECK
            },
            title = getString(R.string.check_uplink_title),
            detail = getString(
                when {
                    !canInject -> R.string.check_uplink_missing
                    telephonyOutput -> R.string.check_uplink_ok
                    else -> R.string.check_uplink_idle
                },
            ),
        )
    }

    private fun stepState(done: Boolean): CheckState = if (done) CheckState.OK else CheckState.MISSING

    /** Title of a section that opens and closes, with an arrow. */
    private fun foldTitle(title: String, open: Boolean): String {
        val arrow = if (open) "\u25BE" else "\u25B8"
        return "$title  $arrow"
    }

    private fun row(id: Int): CheckRow {
        val root = findViewById<View>(id)
        return (root.tag as? CheckRow) ?: CheckRow(root).also { root.tag = it }
    }

    private fun CheckRow.bind(
        state: CheckState,
        title: String,
        detail: String,
        actionLabel: Int = 0,
        onAction: (() -> Unit)? = null,
    ) {
        val color = stateColor(state)
        this.icon.text = when (state) {
            CheckState.OK -> "\u2713"
            CheckState.MISSING -> "\u2717"
            CheckState.CHECK -> "!"
        }
        this.icon.setTextColor(color)
        this.title.text = title
        this.status.setText(
            when (state) {
                CheckState.OK -> R.string.status_ok
                CheckState.MISSING -> R.string.status_missing
                CheckState.CHECK -> R.string.status_check
            },
        )
        this.status.setTextColor(color)
        this.detail.text = detail
        if (actionLabel != 0 && onAction != null) {
            this.action.setText(actionLabel)
            this.action.setOnClickListener { onAction() }
            this.action.visibility = View.VISIBLE
        } else {
            this.action.setOnClickListener(null)
            this.action.visibility = View.GONE
        }
    }

    private fun stateColor(state: CheckState): Int = getColor(
        when (state) {
            CheckState.OK -> R.color.pca_status_ok
            CheckState.MISSING -> R.color.pca_status_missing
            CheckState.CHECK -> R.color.pca_status_check
        },
    )

    private fun showStatus(message: String) {
        status.text = message
        status.visibility = View.VISIBLE
    }

    private fun showUrlMessage(message: String, state: CheckState) {
        urlMessage.text = message
        urlMessage.setTextColor(stateColor(state))
        urlMessage.visibility = View.VISIBLE
    }

    /** [detail]: the raw error text, shown only after tapping "Details". */
    private fun showTestResult(message: String, state: CheckState?, detail: String?) {
        testResult.text = message
        // No state yet (still connecting): use the same neutral colour as the other body text.
        testResult.setTextColor(
            if (state != null) stateColor(state) else serviceStatus.currentTextColor,
        )
        testResult.visibility = View.VISIBLE
        testDetail.text = detail.orEmpty()
        testDetail.visibility = View.GONE
        testDetailToggle.text = foldTitle(getString(R.string.test_details), false)
        testDetailToggle.visibility = if (detail.isNullOrBlank()) View.GONE else View.VISIBLE
    }

    /** The service starts / stops asynchronously; look again shortly afterwards. */
    private fun refreshSoon() {
        handler.removeCallbacks(delayedRefresh)
        handler.postDelayed(delayedRefresh, REFRESH_DELAY_MS)
    }

    private fun isGranted(permission: String): Boolean =
        ContextCompat.checkSelfPermission(this, permission) == PackageManager.PERMISSION_GRANTED

    private fun isPrivilegedSystemApp(): Boolean {
        val info = applicationInfo
        val system = (info.flags and ApplicationInfo.FLAG_SYSTEM) != 0
        return system && info.sourceDir.orEmpty().contains("/priv-app/")
    }

    private fun isServiceRunning(): Boolean = try {
        val am = getSystemService(ActivityManager::class.java)
        @Suppress("DEPRECATION")
        am?.getRunningServices(Int.MAX_VALUE)
            ?.any { it.service.className == CallAudioService::class.java.name } == true
    } catch (e: Exception) {
        Log.w(TAG, "service state unavailable: ${e.message}")
        false
    }

    /** False when a call is ringing / active, or when the state cannot be read. */
    private fun isPhoneIdle(): Boolean = try {
        val tm = getSystemService(TelephonyManager::class.java)
        @Suppress("DEPRECATION")
        tm?.callState == TelephonyManager.CALL_STATE_IDLE
    } catch (e: Exception) {
        Log.w(TAG, "call state unavailable: ${e.message}")
        false
    }

    private fun deviceName(): String {
        val manufacturer = Build.MANUFACTURER.orEmpty().trim()
        val model = Build.MODEL.orEmpty().trim()
        return if (manufacturer.isEmpty() || model.startsWith(manufacturer, ignoreCase = true)) {
            model
        } else {
            "$manufacturer $model"
        }
    }

    /** targetSdk 35 draws edge-to-edge on Android 15+: keep content clear of the bars and IME. */
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
        private const val TAG = "MainActivity"
        private const val REQ_REQUIRED_PERMS = 100
        private const val REQ_GRANT_ALL = 101
        private const val REQ_SCREENING_ROLE = 7102
        private const val TEST_TIMEOUT_MS = 5_000
        private const val REFRESH_DELAY_MS = 700L
        private const val TICK_MS = 3_000L
        private const val INSTANT_DENIAL_MS = 350L
    }
}
