package com.pixelcallai.streamer

import android.app.Activity
import android.os.Bundle
import android.util.Log

/**
 * ROLE_DIALER requires a dialer Activity. This one is invisible: finishes immediately
 * so DIAL / CALL_BUTTON intents never show a dial pad on the dedicated phone.
 */
class TransparentDialerActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Log.i(TAG, "dialer stub finish action=${intent?.action}")
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

    companion object {
        private const val TAG = "TransparentDialer"
    }
}
