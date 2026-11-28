// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.telecom.TelecomManager
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
            val number = intent?.data?.schemeSpecificPart
                ?: intent?.getStringExtra(Intent.EXTRA_PHONE_NUMBER)
                ?: ""
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
        val dialer = try {
            getSystemService(TelecomManager::class.java)?.defaultDialerPackage
        } catch (_: Exception) {
            null
        }
        // A fresh ACTION_DIAL. The incoming intent's action, extras and flags stay here.
        val data = intent?.data
        val number = dialHandOffNumber(data?.scheme, data?.schemeSpecificPart)
        if (!dialer.isNullOrBlank() && dialer != packageName) {
            try {
                val out = Intent(DIAL_HAND_OFF_ACTION).setPackage(dialer)
                if (number != null) out.data = Uri.fromParts("tel", number, null)
                startActivity(out)
                return
            } catch (e: Exception) {
                Log.w(TAG, "forward to $dialer failed: ${e.message}")
            }
        }
        PhoneModeManager.openSystemPhoneApp(this, number.orEmpty())
    }

    companion object {
        private const val TAG = "TransparentDialer"
    }
}

/** The hand-off is always a dial, never a call. */
internal const val DIAL_HAND_OFF_ACTION = "android.intent.action.DIAL"

private const val DIAL_HAND_OFF_MAX = 40

/**
 * Number for the real phone app, taken only from a `tel:` URI.
 * Visual separators are dropped. Any other character drops the whole number,
 * and the dial pad opens empty. Null when there is nothing to prefill.
 */
internal fun dialHandOffNumber(scheme: String?, schemeSpecificPart: String?): String? {
    if (!scheme.equals("tel", ignoreCase = true)) return null
    if (schemeSpecificPart.isNullOrBlank()) return null
    val out = StringBuilder()
    for (ch in schemeSpecificPart.trim()) {
        when {
            ch in '0'..'9' || ch == '*' || ch == '#' || ch == ',' || ch == ';' -> out.append(ch)
            ch == '+' && out.isEmpty() -> out.append('+')
            ch == ' ' || ch == '-' || ch == '(' || ch == ')' || ch == '.' -> {}
            else -> return null
        }
    }
    if (out.isEmpty() || out.length > DIAL_HAND_OFF_MAX) return null
    return out.toString()
}
