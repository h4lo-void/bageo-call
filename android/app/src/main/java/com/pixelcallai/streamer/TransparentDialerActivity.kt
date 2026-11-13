package com.pixelcallai.streamer

import android.app.Activity
import android.content.Intent
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
        Log.i(TAG, "dialer stub finish action=${intent?.action}")
        if (!Prefs.isAiCallEnabled(this)) {
            // Normal phone mode: hand the dial request to the real phone app and
            // never take the phone-app role back from here.
            forwardToPhoneApp()
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
        val source = intent ?: return
        if (dialer.isNullOrBlank() || dialer == packageName) return
        try {
            startActivity(
                Intent(source)
                    .setComponent(null)
                    .setPackage(dialer)
                    .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
            )
        } catch (e: Exception) {
            Log.w(TAG, "forward to $dialer failed: ${e.message}")
        }
    }

    companion object {
        private const val TAG = "TransparentDialer"
    }
}
