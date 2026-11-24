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
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.GridLayout
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast

/**
 * Lockscreen-capable native call UI for Android Standalone edition:
 * - `incoming_whitelist`: direct ring on THIS phone ([接聽] / [交給 AI 接] / [掛斷])
 * - `urgent_prompt`: urgent AI escalation popup ([立刻接手通話] / [讓 AI 請他留話])
 * - `in_call`: owner active call controls ([擴音] / [靜音] / [鍵盤] / [掛斷])
 * - `dialer`: clean native dialpad with dual-SIM selector ([撥出])
 */
class OwnerCallActivity : Activity() {
    private lateinit var badgeText: TextView
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

        badgeText = findViewById(R.id.ocBadge)
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

        wireButtons()
        setupDialpads()
        applyIntent(intent)
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
        val dialPrefill = i.getStringExtra("dial_number").orEmpty()
        if (currentMode == MODE_DIALER && dialPrefill.isNotBlank()) {
            dialInput.setText(dialPrefill)
            dialInput.setSelection(dialPrefill.length)
        }
        renderMode()
    }

    private fun wireButtons() {
        findViewById<Button>(R.id.ocBtnWlAnswer).setOnClickListener {
            CallAudioService.answerWhitelistForOwner(this)
            currentMode = MODE_IN_CALL
            renderMode()
        }
        findViewById<Button>(R.id.ocBtnWlHandAi).setOnClickListener {
            CallAudioService.handWhitelistToAi(this)
            finish()
        }
        findViewById<Button>(R.id.ocBtnWlReject).setOnClickListener {
            CallAudioService.rejectWhitelistForOwner(this)
            finish()
        }
        findViewById<Button>(R.id.ocBtnUrgentTake).setOnClickListener {
            CallAudioService.requestOwnerTakeover(this, urgentId)
            currentMode = MODE_IN_CALL
            renderMode()
        }
        findViewById<Button>(R.id.ocBtnUrgentDecline).setOnClickListener {
            CallAudioService.declineUrgentPrompt(this, urgentId)
            finish()
        }
        findViewById<Button>(R.id.ocBtnInCallHangup).setOnClickListener {
            CallAudioService.hangupActiveCall(this)
            finish()
        }
        btnSpeaker.setOnClickListener {
            val now = SilentInCallService.ownerCallSnapshot()?.speakerOn ?: false
            SilentInCallService.setSpeakerphone(!now)
            updateLiveState()
        }
        btnMute.setOnClickListener {
            val now = SilentInCallService.ownerCallSnapshot()?.micMuted ?: false
            SilentInCallService.setOwnerMuted(!now)
            updateLiveState()
        }
        btnKeypad.setOnClickListener {
            showDtmfPad = !showDtmfPad
            inCallDtmfGrid.visibility = if (showDtmfPad) View.VISIBLE else View.GONE
        }
        findViewById<Button>(R.id.ocBtnDialBack).setOnClickListener {
            val s = dialInput.text.toString()
            if (s.isNotEmpty()) {
                dialInput.setText(s.dropLast(1))
                dialInput.setSelection(dialInput.text.length)
            }
        }
        findViewById<Button>(R.id.ocBtnDialClose).setOnClickListener {
            finish()
        }
        btnDialPlace.setOnClickListener {
            placeManualCall()
        }
    }

    private fun setupDialpads() {
        val keys = listOf("1", "2", "3", "4", "5", "6", "7", "8", "9", "*", "0", "#")
        val dialGrid = findViewById<GridLayout>(R.id.ocDialGrid)
        dialGrid.removeAllViews()
        inCallDtmfGrid.removeAllViews()
        for (k in keys) {
            dialGrid.addView(makeKeyButton(k) {
                dialInput.append(k)
            })
            inCallDtmfGrid.addView(makeKeyButton(k) {
                SilentInCallService.sendDtmfBurst(k[0])
            })
        }
        populateSimChips()
    }

    private fun makeKeyButton(label: String, onTap: () -> Unit): Button =
        Button(this).apply {
            text = label
            textSize = 20f
            layoutParams = GridLayout.LayoutParams().apply {
                width = 0
                height = GridLayout.LayoutParams.WRAP_CONTENT
                columnSpec = GridLayout.spec(GridLayout.UNDEFINED, 1f)
                setMargins(6, 6, 6, 6)
            }
            setOnClickListener { onTap() }
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
            val btn = Button(this).apply {
                text = if (sub == selectedSubId) "✓ $label" else label
                textSize = 12f
                setOnClickListener {
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

    private fun renderMode() {
        incomingWhitelistPanel.visibility = if (currentMode == MODE_INCOMING_WL) View.VISIBLE else View.GONE
        urgentPromptPanel.visibility = if (currentMode == MODE_URGENT) View.VISIBLE else View.GONE
        inCallPanel.visibility = if (currentMode == MODE_IN_CALL) View.VISIBLE else View.GONE
        dialerPanel.visibility = if (currentMode == MODE_DIALER) View.VISIBLE else View.GONE
        reasonCard.visibility = if (currentMode == MODE_URGENT && reason.isNotBlank()) View.VISIBLE else View.GONE
        reasonText.text = reason

        val displayTitle = callerName.ifBlank { callerId }.ifBlank { getString(R.string.sa_call_unknown) }
        when (currentMode) {
            MODE_INCOMING_WL -> {
                badgeText.text = getString(R.string.sa_badge_whitelist)
                titleText.text = displayTitle
                subtitleText.text = callerId.takeIf { callerName.isNotBlank() }.orEmpty()
            }
            MODE_URGENT -> {
                badgeText.text = getString(R.string.sa_badge_urgent)
                titleText.text = displayTitle
                subtitleText.text = callerId.takeIf { callerName.isNotBlank() }.orEmpty()
            }
            MODE_IN_CALL -> {
                badgeText.text = getString(R.string.sa_badge_in_call)
                titleText.text = displayTitle
                inCallDtmfGrid.visibility = if (showDtmfPad) View.VISIBLE else View.GONE
            }
            MODE_DIALER -> {
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
            if (currentMode == MODE_IN_CALL) {
                finish()
            }
            return
        }
        if (snap.state == Call.STATE_DISCONNECTED || snap.state == Call.STATE_DISCONNECTING) {
            finish()
            return
        }
        if (currentMode == MODE_IN_CALL) {
            if (snap.number.isNotBlank() && callerName.isBlank()) {
                titleText.text = snap.number
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
            btnMute.text = getString(
                if (snap.micMuted) R.string.sa_btn_unmute else R.string.sa_btn_mute,
            )
        }
    }

    private fun formatDuration(connectTimeMs: Long): String {
        if (connectTimeMs <= 0L) return getString(R.string.sa_state_connected)
        val elapsedSec = ((System.currentTimeMillis() - connectTimeMs) / 1000L).coerceAtLeast(0L)
        val mins = elapsedSec / 60L
        val secs = elapsedSec % 60L
        return String.format("%02d:%02d", mins, secs)
    }

    companion object {
        const val MODE_INCOMING_WL = "incoming_whitelist"
        const val MODE_URGENT = "urgent_prompt"
        const val MODE_IN_CALL = "in_call"
        const val MODE_DIALER = "dialer"
    }
}
