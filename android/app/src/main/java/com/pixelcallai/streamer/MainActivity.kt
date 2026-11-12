package com.pixelcallai.streamer

import android.Manifest
import android.app.Activity
import android.app.ActivityManager
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
import java.net.URI
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference

/**
 * Setup / status dashboard. Framework Activity and plain views (no AppCompat) so the
 * app stays single-dex and loads reliably as a privileged system app.
 *
 * Nothing here is on the call path: it only shows what is missing, opens the matching
 * system dialogs, and sends the same START / STOP / SET_ENABLED intents as before.
 */
class MainActivity : Activity() {
    private lateinit var status: TextView
    private lateinit var serviceStatus: TextView
    private lateinit var switchAi: Switch
    private lateinit var btnGrant: Button
    private lateinit var agentUrlField: EditText
    private lateinit var urlMessage: TextView
    private lateinit var btnTest: Button
    private lateinit var testResult: TextView

    private val handler = Handler(Looper.getMainLooper())
    private val delayedRefresh = Runnable { refresh() }

    /** True while [refresh] updates the switch, so the listener ignores it. */
    private var bindingSwitch = false

    /** Invalidates the result of an older connection test. */
    private var testSequence = 0

    private var grantRequestedAt = 0L
    private var grantRationaleBefore: Map<String, Boolean> = emptyMap()

    private enum class CheckState { OK, MISSING, CHECK }

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

        status = findViewById(R.id.status)
        serviceStatus = findViewById(R.id.serviceStatus)
        switchAi = findViewById(R.id.switchAi)
        btnGrant = findViewById(R.id.btnGrant)
        agentUrlField = findViewById(R.id.agentUrl)
        urlMessage = findViewById(R.id.urlMessage)
        btnTest = findViewById(R.id.btnTest)
        testResult = findViewById(R.id.testResult)

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

        findViewById<Button>(R.id.btnStart).setOnClickListener {
            if (!ensurePerms()) return@setOnClickListener
            DialerRoleHelper.ensureRoles(this)
            DialerRoleHelper.requestDialerRole(this)
            armService(CallAudioService.ACTION_START)
        }
        findViewById<Button>(R.id.btnStop).setOnClickListener {
            startService(Intent(this, CallAudioService::class.java).setAction(CallAudioService.ACTION_STOP))
            showStatus(getString(R.string.msg_service_stopped))
            refresh()
            refreshSoon()
        }
        switchAi.setOnCheckedChangeListener { _, isChecked ->
            if (!bindingSwitch) onAiSwitchChanged(isChecked)
        }
        btnGrant.setOnClickListener { requestMissingRuntimePermissions() }

        agentUrlField.hint = BuildConfig.BRIDGE_WS_URL
        agentUrlField.setText(Prefs.agentUrl(this))
        findViewById<Button>(R.id.btnSaveUrl).setOnClickListener {
            saveAgentUrl(agentUrlField.text.toString())
        }
        findViewById<Button>(R.id.btnResetUrl).setOnClickListener {
            saveAgentUrl(BuildConfig.BRIDGE_WS_URL)
        }
        btnTest.setOnClickListener { testConnection() }

        handleLaunchIntent(intent)
        if (Prefs.isAiCallEnabled(this) && ensurePerms()) {
            DialerRoleHelper.ensureRoles(this)
            armService(CallAudioService.ACTION_START)
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
        Prefs.setAiCallEnabled(this, true)
        val i = Intent(this, CallAudioService::class.java).setAction(action)
        ContextCompat.startForegroundService(this, i)
        showStatus(getString(R.string.msg_service_started))
        refresh()
        refreshSoon()
    }

    override fun onResume() {
        super.onResume()
        refresh()
    }

    override fun onDestroy() {
        handler.removeCallbacks(delayedRefresh)
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

    // ---------------------------------------------------------------- actions

    private fun onAiSwitchChanged(enabled: Boolean) {
        Prefs.setAiCallEnabled(this, enabled)
        val i = Intent(this, CallAudioService::class.java)
            .setAction(CallAudioService.ACTION_SET_ENABLED)
            .putExtra(CallAudioService.EXTRA_ENABLED, enabled)
        startService(i)
        if (enabled && ensurePerms()) {
            DialerRoleHelper.ensureRoles(this)
            armService(CallAudioService.ACTION_START)
        }
        refresh()
        refreshSoon()
    }

    /** The permissions the service cannot start without (same set as before). */
    private fun ensurePerms(): Boolean {
        val need = mutableListOf(
            Manifest.permission.RECORD_AUDIO,
            Manifest.permission.READ_PHONE_STATE,
            Manifest.permission.ANSWER_PHONE_CALLS,
        )
        if (Build.VERSION.SDK_INT >= 33) {
            need += Manifest.permission.POST_NOTIFICATIONS
        }
        val missing = need.filter { !isGranted(it) }
        if (missing.isNotEmpty()) {
            ActivityCompat.requestPermissions(this, missing.toTypedArray(), REQ_REQUIRED_PERMS)
            showStatus(getString(R.string.grant_perms))
            return false
        }
        return true
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
                // STOP / START intents as the buttons (START only while AI answering is on).
                val enabled = Prefs.isAiCallEnabled(this)
                startService(
                    Intent(this, CallAudioService::class.java).setAction(CallAudioService.ACTION_STOP),
                )
                if (enabled) {
                    ContextCompat.startForegroundService(
                        this,
                        Intent(this, CallAudioService::class.java).setAction(CallAudioService.ACTION_START),
                    )
                }
                refreshSoon()
                if (enabled) R.string.msg_url_saved_restarted else R.string.msg_url_saved
            }
        }
        showUrlMessage(getString(message), CheckState.OK)
    }

    /** Opens a WebSocket to the address in the field, reports the result, sends nothing. */
    private fun testConnection() {
        val url = agentUrlField.text.toString().trim()
        if (!Prefs.isValidAgentUrl(url)) {
            showTestResult(getString(R.string.msg_url_invalid), CheckState.MISSING)
            return
        }
        val sequence = ++testSequence
        btnTest.isEnabled = false
        showTestResult(getString(R.string.test_running), null)
        Thread({
            val reason = AtomicReference<String?>(null)
            var connected = false
            var client: WebSocketClient? = null
            try {
                val ws = object : WebSocketClient(URI(url), Draft_6455(), null, TEST_TIMEOUT_MS) {
                    override fun onOpen(handshakedata: ServerHandshake?) {}

                    override fun onMessage(message: String?) {}

                    override fun onClose(code: Int, closeReason: String?, remote: Boolean) {
                        reason.compareAndSet(
                            null,
                            closeReason?.takeIf { it.isNotBlank() }
                                ?: getString(R.string.test_closed, code),
                        )
                    }

                    override fun onError(ex: Exception?) {
                        reason.compareAndSet(null, ex?.let { it.message ?: it.javaClass.simpleName })
                    }
                }
                client = ws
                connected = ws.connectBlocking(TEST_TIMEOUT_MS.toLong(), TimeUnit.MILLISECONDS)
            } catch (e: Exception) {
                reason.compareAndSet(null, e.message ?: e.javaClass.simpleName)
            } finally {
                try {
                    client?.close()
                } catch (_: Exception) {
                }
            }
            val failure = reason.get() ?: getString(R.string.test_timeout)
            runOnUiThread {
                if (isDestroyed || sequence != testSequence) return@runOnUiThread
                btnTest.isEnabled = true
                if (connected) {
                    showTestResult(getString(R.string.test_ok), CheckState.OK)
                } else {
                    showTestResult(getString(R.string.test_failed, failure), CheckState.MISSING)
                }
            }
        }, "agent-connection-test").apply { isDaemon = true }.start()
    }

    // ----------------------------------------------------------------- status

    private fun refresh() {
        val enabled = Prefs.isAiCallEnabled(this)
        bindingSwitch = true
        switchAi.isChecked = enabled
        bindingSwitch = false
        serviceStatus.setText(
            if (isServiceRunning()) R.string.service_running else R.string.service_stopped,
        )

        // 1. Privileged system app (installed by the Magisk / KernelSU module).
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

        // 2. Privileged permissions (informational).
        for (item in privilegedPermissions) {
            row(item.rowId).bind(
                state = if (isGranted(item.permission)) CheckState.OK else CheckState.MISSING,
                title = getString(item.labelRes),
                detail = item.permission.substringAfterLast('.'),
            )
        }

        // 3. Runtime permissions, one row each, one button for all missing ones.
        var runtimeMissing = false
        for (item in runtimePermissions) {
            val granted = isGranted(item.permission)
            if (!granted) runtimeMissing = true
            row(item.rowId).bind(
                state = if (granted) CheckState.OK else CheckState.MISSING,
                title = getString(item.labelRes),
                detail = item.permission.substringAfterLast('.'),
            )
        }
        btnGrant.visibility = if (runtimeMissing) View.VISIBLE else View.GONE

        // 4. Roles.
        val roleManager = getSystemService(RoleManager::class.java)
        val dialer = DialerRoleHelper.isDefaultDialer(this) ||
            roleManager?.isRoleHeld(RoleManager.ROLE_DIALER) == true
        row(R.id.rowDialer).bind(
            state = if (dialer) CheckState.OK else CheckState.MISSING,
            title = getString(R.string.check_dialer_title),
            detail = getString(R.string.check_dialer_desc),
            actionLabel = if (dialer) 0 else R.string.btn_set,
        ) {
            DialerRoleHelper.requestDialerRole(this)
            refresh()
        }
        val screening = roleManager?.isRoleHeld(RoleManager.ROLE_CALL_SCREENING) == true
        row(R.id.rowScreening).bind(
            state = if (screening) CheckState.OK else CheckState.MISSING,
            title = getString(R.string.check_screening_title),
            detail = getString(R.string.check_screening_desc),
            actionLabel = if (screening) 0 else R.string.btn_set,
        ) { requestScreeningRole() }

        // 5. Do Not Disturb access.
        val dnd = getSystemService(NotificationManager::class.java)
            ?.isNotificationPolicyAccessGranted == true
        row(R.id.rowDnd).bind(
            state = if (dnd) CheckState.OK else CheckState.MISSING,
            title = getString(R.string.check_dnd_title),
            detail = getString(R.string.check_dnd_desc),
            actionLabel = if (dnd) 0 else R.string.btn_open_settings,
        ) { launch(Intent(Settings.ACTION_NOTIFICATION_POLICY_ACCESS_SETTINGS)) }

        // 6. Battery optimization exemption.
        val battery = getSystemService(PowerManager::class.java)
            ?.isIgnoringBatteryOptimizations(packageName) == true
        row(R.id.rowBattery).bind(
            state = if (battery) CheckState.OK else CheckState.MISSING,
            title = getString(R.string.check_battery_title),
            detail = getString(R.string.check_battery_desc),
            actionLabel = if (battery) 0 else R.string.btn_allow,
        ) { requestBatteryExemption() }

        // 7. Uplink injection: the same TYPE_TELEPHONY device check the call path uses.
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

    private fun showTestResult(message: String, state: CheckState?) {
        testResult.text = message
        // No state yet (still connecting): use the same neutral colour as the other body text.
        testResult.setTextColor(
            if (state != null) stateColor(state) else serviceStatus.currentTextColor,
        )
        testResult.visibility = View.VISIBLE
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
        private const val INSTANT_DENIAL_MS = 350L
    }
}
