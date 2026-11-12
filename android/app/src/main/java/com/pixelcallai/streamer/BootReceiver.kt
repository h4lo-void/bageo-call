package com.pixelcallai.streamer

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Build
import android.util.Log

/** Auto-arm the silent AI call watcher after reboot (dedicated phone). */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent?) {
        val action = intent?.action ?: return
        if (action != Intent.ACTION_BOOT_COMPLETED &&
            action != Intent.ACTION_LOCKED_BOOT_COMPLETED &&
            action != Intent.ACTION_MY_PACKAGE_REPLACED
        ) {
            return
        }
        if (!Prefs.isAiCallEnabled(context)) {
            Log.i(TAG, "boot: AI call disabled; skip arm")
            return
        }
        val svc = Intent(context, CallAudioService::class.java).setAction(CallAudioService.ACTION_START)
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                context.startForegroundService(svc)
            } else {
                context.startService(svc)
            }
            Log.i(TAG, "boot: armed CallAudioService")
        } catch (e: Exception) {
            Log.e(TAG, "boot arm failed: ${e.message}")
        }
    }

    companion object {
        private const val TAG = "BootReceiver"
    }
}
