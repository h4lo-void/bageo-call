// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.app.Activity
import android.content.Intent
import android.os.Bundle
import android.util.Log

/**
 * ROLE_DIALER requires a dialer Activity. This one is invisible: finishes immediately
 * so DIAL / CALL_BUTTON intents never show a dial pad on the dedicated phone.
 */
class TransparentDialerActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        PhoneModeManager.reconcileLocalMirror(this)
        Log.i(TAG, "dialer stub finish action=${intent?.action}")
        if (!Prefs.isAiCallEnabled(this)) {
            // Normal phone mode: hand the dial request to the real phone app and
            // never take the phone-app role back from here.
            forwardToPhoneApp()
            finish()
            overridePendingTransition(0, 0)
            return
        }
        if (BuildConfig.STANDALONE) {
            DialerRoleHelper.ensureRoles(this)
            try {
                val svc = Intent(this, CallAudioService::class.java)
                    .setAction(CallAudioService.ACTION_START)
                startForegroundService(svc)
            } catch (_: Exception) {
            }
            val rawNumber = intent?.data?.schemeSpecificPart
                ?: intent?.getStringExtra(Intent.EXTRA_PHONE_NUMBER)
            val number = dialableNumber(rawNumber)
            val dialerIntent = Intent().apply {
                setClassName(this@TransparentDialerActivity, "com.pixelcallai.streamer.OwnerCallActivity")
                putExtra("mode", "dialer")
                if (number.isNotBlank()) putExtra("dial_number", number)
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
            }
            try {
                startActivity(dialerIntent)
            } catch (e: Exception) {
                Log.w(TAG, "launch OwnerCallActivity failed: ${e.message}")
            }
            finish()
            overridePendingTransition(0, 0)
            return
        }
        DialerRoleHelper.ensureRoles(this)
        // Keep CallAudioService armed; never show UI.
        try {
            val i = android.content.Intent(this, CallAudioService::class.java)
                .setAction(CallAudioService.ACTION_START)
            startForegroundService(i)
        } catch (_: Exception) {
        }
        finish()
        overridePendingTransition(0, 0)
    }

    private fun forwardToPhoneApp() {
        val data = intent?.data
        val number = dialForwardNumber(
            intent?.action,
            data?.scheme,
            data?.schemeSpecificPart,
        ) ?: return
        PhoneModeManager.openSystemPhoneApp(this, number)
    }

    companion object {
        private const val TAG = "TransparentDialer"
    }
}

/**
 * Number for the real phone app, or null when [action] is not forwarded.
 * Only a `tel:` number is kept, and only after [dialableNumber].
 * [ignoredExtras] and [ignoredFlags] are not copied onto the phone-app intent.
 */
@Suppress("UNUSED_PARAMETER")
internal fun dialForwardNumber(
    action: String?,
    scheme: String?,
    schemeSpecificPart: String?,
    ignoredExtras: Map<String, String> = emptyMap(),
    ignoredFlags: Int = 0,
): String? {
    if (action != Intent.ACTION_DIAL &&
        action != Intent.ACTION_VIEW &&
        action != Intent.ACTION_CALL_BUTTON
    ) {
        return null
    }
    val raw = if (scheme.equals("tel", ignoreCase = true)) schemeSpecificPart else null
    return dialableNumber(raw)
}
