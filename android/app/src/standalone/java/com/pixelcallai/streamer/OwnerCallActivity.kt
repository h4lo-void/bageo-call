// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.telecom.Call
import android.telecom.PhoneAccountHandle
import android.telecom.TelecomManager
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.style.ForegroundColorSpan
import android.text.style.RelativeSizeSpan
import android.view.Gravity
import android.view.HapticFeedbackConstants
import android.view.View
import android.view.WindowInsets
import android.widget.Button
import android.widget.EditText
import android.widget.GridLayout
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast

/**
 * Lockscreen-capable native call & dialpad UI for Android Standalone edition,
 * styled to match Bageo's warm paper/ink design system:
 * - `incoming_whitelist`: direct ring on THIS phone ([接聽] / [交給 AI 接] / [掛斷])
 * - `incoming`: a call that rings for the owner through [OwnerRing] ([接聽] / [掛斷]); works
 *   without CallAudioService
 * - `urgent_prompt`: urgent AI escalation popup ([立刻接手通話] / [讓 AI 請他留話])
 * - `in_call`: owner active call controls ([擴音] / [靜音] / [鍵盤] / [掛斷])
 * - `dialer`: clean native dialpad with dual-SIM selector ([撥出])
 */
class OwnerCallActivity : Activity() {
    private lateinit var rootLayout: View
    private lateinit var badgeText: TextView
    private lateinit var avatarWrap: View
    private lateinit var avatarText: TextView
    private lateinit var titleText: TextView
    private lateinit var subtitleText: TextView
    private lateinit var reasonCard: View
    private lateinit var reasonText: TextView

    private lateinit var incomingWhitelistPanel: View
    private lateinit var urgentPromptPanel: View
    private lateinit var inCallPanel: View
    private lateinit var dialerPanel: View
    private lateinit var inCallDtmfGrid: GridLayout

    private lateinit var btnSpeaker: Button
    private lateinit var btnMute: Button
    private lateinit var btnKeypad: Button
    private lateinit var dialInput: EditText
    private lateinit var dialSimContainer: LinearLayout
    private lateinit var btnDialPlace: Button

    private var currentMode = MODE_IN_CALL
    private var callerId = ""
    private var callerName = ""
    private var reason = ""
    private var urgentId = ""
    /** The AI could not take this ringing call and gave it back to the owner. */
    private var aiMissed = false
    private var selectedSubId: Int? = null
    private var showDtmfPad = false

    private val handler = Handler(Looper.getMainLooper())
    private val tickRunnable = object : Runnable {
        override fun run() {
            if (isFinishing || isDestroyed) return
            updateLiveState()
            handler.postDelayed(this, 500L)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O_MR1) {
            setShowWhenLocked(true)
            setTurnScreenOn(true)
        }
        setContentView(R.layout.activity_owner_call)

        rootLayout = findViewById(R.id.ocRoot)
        badgeText = findViewById(R.id.ocBadge)
        avatarWrap = findViewById(R.id.ocAvatarWrap)
        avatarText = findViewById(R.id.ocAvatarText)
        titleText = findViewById(R.id.ocTitle)
        subtitleText = findViewById(R.id.ocSubtitle)
        reasonCard = findViewById(R.id.ocReasonCard)
        reasonText = findViewById(R.id.ocReasonText)

        incomingWhitelistPanel = findViewById(R.id.ocPanelIncomingWhitelist)
        urgentPromptPanel = findViewById(R.id.ocPanelUrgentPrompt)
        inCallPanel = findViewById(R.id.ocPanelInCall)
        dialerPanel = findViewById(R.id.ocPanelDialer)
        inCallDtmfGrid = findViewById(R.id.ocDtmfGrid)

        btnSpeaker = findViewById(R.id.ocBtnSpeaker)
        btnMute = findViewById(R.id.ocBtnMute)
        btnKeypad = findViewById(R.id.ocBtnKeypad)
        dialInput = findViewById(R.id.ocDialInput)
        dialSimContainer = findViewById(R.id.ocSimRow)
        btnDialPlace = findViewById(R.id.ocBtnDialPlace)

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.LOLLIPOP) {
            dialInput.showSoftInputOnFocus = false
        }
        applyEdgeToEdgeInsets()
        wireButtons()
        setupDialpads()
        applyIntent(intent)
    }

    private fun applyEdgeToEdgeInsets() {
        val padH = dp(20)
        val baseTop = dp(18)
        val baseBottom = dp(20)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            window.setDecorFitsSystemWindows(false)
            rootLayout.setOnApplyWindowInsetsListener { v, insets ->
                val bars = insets.getInsets(WindowInsets.Type.systemBars())
                v.setPadding(padH + bars.left, baseTop + bars.top, padH + bars.right, baseBottom + bars.bottom)
                insets
            }
        }
    }

    override fun onNewIntent(intent: Intent?) {
        super.onNewIntent(intent)
        if (intent != null) applyIntent(intent)
    }

    override fun onResume() {
        super.onResume()
        handler.removeCallbacks(tickRunnable)
        handler.post(tickRunnable)
    }

    override fun onPause() {
        handler.removeCallbacks(tickRunnable)
        super.onPause()
    }

    private fun applyIntent(i: Intent) {
        currentMode = i.getStringExtra("mode")?.takeIf { it.isNotBlank() } ?: MODE_IN_CALL
        callerId = i.getStringExtra("caller_id").orEmpty()
        callerName = i.getStringExtra("caller_name").orEmpty()
        reason = i.getStringExtra("reason").orEmpty()
        urgentId = i.getStringExtra("urgent_id").orEmpty()
        aiMissed = i.getBooleanExtra("ai_missed", false)
        val dialPrefill = dialableNumber(i.getStringExtra("dial_number"))
        if (currentMode == MODE_DIALER && dialPrefill.isNotBlank()) {
            dialInput.setText(dialPrefill)
            dialInput.setSelection(dialPrefill.length)
        }
        renderMode()
    }

    private fun tapHaptic(v: View) {
        v.performHapticFeedback(HapticFeedbackConstants.KEYBOARD_TAP)
    }

    private fun wireButtons() {
        findViewById<Button>(R.id.ocBtnWlAnswer).setOnClickListener { v ->
            tapHaptic(v)
            if (currentMode == MODE_INCOMING) {
                OwnerRing.answer(this, openScreen = false)
            } else {
                CallAudioService.answerWhitelistForOwner(this)
            }
            currentMode = MODE_IN_CALL
            renderMode()
        }
        findViewById<Button>(R.id.ocBtnWlHandAi).setOnClickListener { v ->
            tapHaptic(v)
            CallAudioService.handWhitelistToAi(this)
            finish()
        }
        findViewById<Button>(R.id.ocBtnWlReject).setOnClickListener { v ->
            tapHaptic(v)
            if (currentMode == MODE_INCOMING) {
                OwnerRing.decline(this)
            } else {
                CallAudioService.rejectWhitelistForOwner(this)
            }
            finish()
        }
        findViewById<Button>(R.id.ocBtnUrgentTake).setOnClickListener { v ->
            tapHaptic(v)
            CallAudioService.requestOwnerTakeover(this, urgentId)
            currentMode = MODE_IN_CALL
            renderMode()
        }
        findViewById<Button>(R.id.ocBtnUrgentDecline).setOnClickListener { v ->
            tapHaptic(v)
            CallAudioService.declineUrgentPrompt(this, urgentId)
            finish()
        }
        findViewById<Button>(R.id.ocBtnInCallHangup).setOnClickListener { v ->
            tapHaptic(v)
            CallAudioService.hangupActiveCall(this)
            finish()
        }
        btnSpeaker.setOnClickListener { v ->
            tapHaptic(v)
            val now = SilentInCallService.ownerCallSnapshot()?.speakerOn ?: false
            SilentInCallService.setSpeakerphone(!now)
            updateLiveState()
        }
        btnMute.setOnClickListener { v ->
            tapHaptic(v)
            val now = SilentInCallService.ownerCallSnapshot()?.micMuted ?: false
            SilentInCallService.setOwnerMuted(!now)
            updateLiveState()
        }
        btnKeypad.setOnClickListener { v ->
            tapHaptic(v)
            showDtmfPad = !showDtmfPad
            inCallDtmfGrid.visibility = if (showDtmfPad) View.VISIBLE else View.GONE
            styleToggleButton(btnKeypad, showDtmfPad)
        }
        val backBtn = findViewById<Button>(R.id.ocBtnDialBack)
        backBtn.setOnClickListener { v ->
            tapHaptic(v)
            val s = dialInput.text.toString()
            if (s.isNotEmpty()) {
                dialInput.setText(s.dropLast(1))
                dialInput.setSelection(dialInput.text.length)
            }
        }
        backBtn.setOnLongClickListener { v ->
            v.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
            dialInput.setText("")
            true
        }
        findViewById<Button>(R.id.ocBtnDialClose).setOnClickListener { v ->
            tapHaptic(v)
            finish()
        }
        btnDialPlace.setOnClickListener { v ->
            tapHaptic(v)
            placeManualCall()
        }
    }

    private fun styleToggleButton(btn: Button, active: Boolean) {
        btn.setBackgroundResource(if (active) R.drawable.bg_btn_active else R.drawable.bg_btn_secondary)
        btn.setTextColor(getColor(if (active) R.color.bg_on_accent else R.color.bg_on_secondary))
    }

    private fun setupDialpads() {
        val keys = listOf(
            "1" to "",
            "2" to "ABC",
            "3" to "DEF",
            "4" to "GHI",
            "5" to "JKL",
            "6" to "MNO",
            "7" to "PQRS",
            "8" to "TUV",
            "9" to "WXYZ",
            "*" to "",
            "0" to "+",
            "#" to "",
        )
        val dialGrid = findViewById<GridLayout>(R.id.ocDialGrid)
        dialGrid.removeAllViews()
        inCallDtmfGrid.removeAllViews()
        for ((digit, sub) in keys) {
            val dialBtn = makeKeyButton(digit, sub, onTap = {
                dialInput.append(digit)
            }, onLongZero = if (digit == "0") {
                { dialInput.append("+") }
            } else {
                null
            })
            dialGrid.addView(dialBtn)
            inCallDtmfGrid.addView(makeKeyButton(digit, sub, onTap = {
                SilentInCallService.sendDtmfBurst(digit[0])
            }))
        }
        populateSimChips()
    }

    private fun makeKeyButton(
        digit: String,
        sub: String,
        onTap: () -> Unit,
        onLongZero: (() -> Unit)? = null,
    ): Button = Button(this).apply {
        isAllCaps = false
        stateListAnimator = null
        setBackgroundResource(R.drawable.bg_dial_key)
        gravity = Gravity.CENTER
        val ssb = SpannableStringBuilder(digit)
        if (sub.isNotEmpty()) {
            ssb.append("\n").append(sub)
            val start = digit.length + 1
            ssb.setSpan(RelativeSizeSpan(0.42f), start, ssb.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            ssb.setSpan(
                ForegroundColorSpan(getColor(R.color.bg_muted)),
                start,
                ssb.length,
                Spanned.SPAN_EXCLUSIVE_EXCLUSIVE,
            )
        }
        text = ssb
        textSize = 22f
        setTextColor(getColor(R.color.bg_text))
        setLineSpacing(0f, 0.85f)
        layoutParams = GridLayout.LayoutParams().apply {
            width = 0
            height = dp(58)
            columnSpec = GridLayout.spec(GridLayout.UNDEFINED, 1f)
            val m = dp(5)
            setMargins(m, m, m, m)
        }
        setOnClickListener { v ->
            tapHaptic(v)
            onTap()
        }
        if (onLongZero != null) {
            setOnLongClickListener { v ->
                v.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
                onLongZero()
                true
            }
        }
    }

    private fun populateSimChips() {
        dialSimContainer.removeAllViews()
        val sims = SimList.read(this)
        if (sims == null || sims.length() <= 1) {
            dialSimContainer.visibility = View.GONE
            if (sims != null && sims.length() == 1) {
                selectedSubId = sims.optJSONObject(0)?.optInt("sub_id")?.takeIf { it >= 0 }
            }
            return
        }
        dialSimContainer.visibility = View.VISIBLE
        if (selectedSubId == null) {
            selectedSubId = sims.optJSONObject(0)?.optInt("sub_id")?.takeIf { it >= 0 }
        }
        for (i in 0 until sims.length()) {
            val s = sims.optJSONObject(i) ?: continue
            val sub = s.optInt("sub_id", -1).takeIf { it >= 0 } ?: continue
            val label = s.optString("label").ifBlank { "SIM ${i + 1}" }
            val active = sub == selectedSubId
            val btn = Button(this).apply {
                isAllCaps = false
                stateListAnimator = null
                text = label
                textSize = 13f
                setPadding(dp(14), dp(6), dp(14), dp(6))
                setBackgroundResource(if (active) R.drawable.bg_btn_active else R.drawable.bg_btn_secondary)
                setTextColor(getColor(if (active) R.color.bg_on_accent else R.color.bg_on_secondary))
                layoutParams = LinearLayout.LayoutParams(
                    LinearLayout.LayoutParams.WRAP_CONTENT,
                    dp(48),
                ).apply {
                    if (i > 0) marginStart = dp(8)
                }
                setOnClickListener { v ->
                    tapHaptic(v)
                    selectedSubId = sub
                    populateSimChips()
                }
            }
            dialSimContainer.addView(btn)
        }
    }

    private fun placeManualCall() {
        val raw = dialInput.text.toString().trim()
        if (raw.isEmpty()) {
            Toast.makeText(this, R.string.sa_dial_empty, Toast.LENGTH_SHORT).show()
            return
        }
        if (!Prefs.isAiCallEnabled(this)) {
            if (PhoneModeManager.openSystemPhoneApp(this, raw)) {
                finish()
                return
            }
        }
        try {
            val tm = getSystemService(TelecomManager::class.java)
            val uri = Uri.fromParts("tel", raw, null)
            val extras = Bundle()
            selectedSubId?.let { sub ->
                findAccountForSub(tm, sub)?.let { handle ->
                    extras.putParcelable(TelecomManager.EXTRA_PHONE_ACCOUNT_HANDLE, handle)
                }
            }
            tm?.placeCall(uri, extras)
            callerId = raw
            callerName = ""
            currentMode = MODE_IN_CALL
            renderMode()
        } catch (e: SecurityException) {
            Toast.makeText(this, e.message ?: "Permission required", Toast.LENGTH_LONG).show()
        } catch (e: Exception) {
            Toast.makeText(this, e.message ?: "Dial failed", Toast.LENGTH_LONG).show()
        }
    }

    private fun findAccountForSub(tm: TelecomManager?, subId: Int): PhoneAccountHandle? {
        if (tm == null) return null
        val tel = getSystemService(android.telephony.TelephonyManager::class.java) ?: return null
        val accounts = try {
            tm.callCapablePhoneAccounts
        } catch (_: SecurityException) {
            return null
        }
        return accounts.firstOrNull { handle ->
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                try {
                    tel.getSubscriptionId(handle) == subId
                } catch (_: Exception) {
                    false
                }
            } else {
                false
            }
        }
    }

    private fun avatarGlyph(name: String, number: String): String {
        val cleanName = name.trim()
        if (cleanName.isNotEmpty()) {
            return cleanName.codePoints().findFirst().orElse('?'.code).let { String(Character.toChars(it)).uppercase() }
        }
        val cleanNum = number.trim().trimStart('+')
        if (cleanNum.isNotEmpty()) {
            return cleanNum.takeLast(2)
        }
        return "•"
    }

    private fun renderMode() {
        val ringing = currentMode == MODE_INCOMING_WL || currentMode == MODE_INCOMING
        incomingWhitelistPanel.visibility = if (ringing) View.VISIBLE else View.GONE
        urgentPromptPanel.visibility = if (currentMode == MODE_URGENT) View.VISIBLE else View.GONE
        inCallPanel.visibility = if (currentMode == MODE_IN_CALL) View.VISIBLE else View.GONE
        dialerPanel.visibility = if (currentMode == MODE_DIALER) View.VISIBLE else View.GONE
        reasonCard.visibility = if (currentMode == MODE_URGENT && reason.isNotBlank()) View.VISIBLE else View.GONE
        reasonText.text = reason

        val isUrgent = currentMode == MODE_URGENT
        badgeText.setBackgroundResource(if (isUrgent) R.drawable.bg_badge_warn else R.drawable.bg_badge_accent)
        badgeText.setTextColor(getColor(if (isUrgent) R.color.bg_warn else R.color.bg_on_accent_soft))

        val displayTitle = callerName.ifBlank { callerId }.ifBlank { getString(R.string.sa_call_unknown) }
        when (currentMode) {
            MODE_INCOMING_WL, MODE_INCOMING -> {
                avatarWrap.visibility = View.VISIBLE
                avatarText.text = avatarGlyph(callerName, callerId)
                // A call that rings for the owner (the AI could not take it, or a normal
                // phone) rings here the same way, without "hand to AI".
                badgeText.text = getString(
                    when {
                        aiMissed -> R.string.sa_ai_missed_title
                        currentMode == MODE_INCOMING -> R.string.sa_incoming_title
                        else -> R.string.sa_badge_whitelist
                    },
                )
                val handAi = currentMode == MODE_INCOMING_WL && !aiMissed
                findViewById<Button>(R.id.ocBtnWlHandAi).visibility = if (handAi) View.VISIBLE else View.GONE
                titleText.text = displayTitle
                subtitleText.text = callerId.takeIf { callerName.isNotBlank() }.orEmpty()
            }
            MODE_URGENT -> {
                avatarWrap.visibility = View.VISIBLE
                avatarText.text = avatarGlyph(callerName, callerId)
                badgeText.text = getString(R.string.sa_badge_urgent)
                titleText.text = displayTitle
                subtitleText.text = callerId.takeIf { callerName.isNotBlank() }.orEmpty()
            }
            MODE_IN_CALL -> {
                avatarWrap.visibility = View.VISIBLE
                avatarText.text = avatarGlyph(callerName, callerId)
                badgeText.text = getString(R.string.sa_badge_in_call)
                titleText.text = displayTitle
                inCallDtmfGrid.visibility = if (showDtmfPad) View.VISIBLE else View.GONE
                styleToggleButton(btnKeypad, showDtmfPad)
            }
            MODE_DIALER -> {
                avatarWrap.visibility = View.GONE
                badgeText.text = getString(R.string.sa_badge_dialer)
                titleText.text = getString(R.string.sa_dialer_title)
                subtitleText.text = ""
            }
        }
        updateLiveState()
    }

    private fun updateLiveState() {
        if (currentMode == MODE_DIALER) return
        val snap = SilentInCallService.ownerCallSnapshot()
        if (snap == null) {
            if (currentMode == MODE_IN_CALL || currentMode == MODE_INCOMING) {
                finish()
            }
            return
        }
        if (snap.state == Call.STATE_DISCONNECTED || snap.state == Call.STATE_DISCONNECTING) {
            finish()
            return
        }
        if (currentMode == MODE_INCOMING && snap.state == Call.STATE_ACTIVE) {
            // Answered elsewhere (a headset button, the notification): show the call controls.
            currentMode = MODE_IN_CALL
            renderMode()
            return
        }
        if (currentMode == MODE_IN_CALL) {
            if (snap.number.isNotBlank() && callerName.isBlank()) {
                titleText.text = snap.number
                avatarText.text = avatarGlyph("", snap.number)
            }
            subtitleText.text = when (snap.state) {
                Call.STATE_DIALING, Call.STATE_CONNECTING -> getString(R.string.sa_state_dialing)
                Call.STATE_ACTIVE -> formatDuration(snap.connectTimeMs)
                Call.STATE_HOLDING -> getString(R.string.sa_state_holding)
                else -> ""
            }
            btnSpeaker.text = getString(
                if (snap.speakerOn) R.string.sa_btn_speaker_on else R.string.sa_btn_speaker_off,
            )
            styleToggleButton(btnSpeaker, snap.speakerOn)
            btnMute.text = getString(
                if (snap.micMuted) R.string.sa_btn_unmute else R.string.sa_btn_mute,
            )
            styleToggleButton(btnMute, snap.micMuted)
        }
    }

    private fun formatDuration(connectTimeMs: Long): String {
        if (connectTimeMs <= 0L) return getString(R.string.sa_state_connected)
        val elapsedSec = ((System.currentTimeMillis() - connectTimeMs) / 1000L).coerceAtLeast(0L)
        val mins = elapsedSec / 60L
        val secs = elapsedSec % 60L
        return String.format("%02d:%02d", mins, secs)
    }

    private fun dp(v: Int): Int = (v * resources.displayMetrics.density + 0.5f).toInt()

    companion object {
        const val MODE_INCOMING_WL = "incoming_whitelist"
        const val MODE_INCOMING = OwnerRing.SCREEN_INCOMING
        const val MODE_URGENT = "urgent_prompt"
        const val MODE_IN_CALL = "in_call"
        const val MODE_DIALER = "dialer"
    }
}
