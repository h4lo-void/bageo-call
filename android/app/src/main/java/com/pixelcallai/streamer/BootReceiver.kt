package com.pixelcallai.streamer

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Build
import android.util.Log

/**
 * Start the service after a reboot, in both modes. In AI mode it answers calls; in
 * normal phone mode it only keeps the connection to the agent (for the web console's
 * mode switch) and leaves calls alone.
 */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent?) {
        val action = intent?.action ?: return
        if (action != Intent.ACTION_BOOT_COMPLETED &&
            action != Intent.ACTION_LOCKED_BOOT_COMPLETED &&
            action != Intent.ACTION_MY_PACKAGE_REPLACED
        ) {
            return
        }
        val svc = Intent(context, CallAudioService::class.java).setAction(CallAudioService.ACTION_START)
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                context.startForegroundService(svc)
            } else {
                context.startService(svc)
            }
            Log.i(TAG, "boot: started CallAudioService mode=${Prefs.phoneMode(context)}")
        } catch (e: Exception) {
            Log.e(TAG, "boot arm failed: ${e.message}")
        }
    }

    companion object {
        private const val TAG = "BootReceiver"
    }
}
