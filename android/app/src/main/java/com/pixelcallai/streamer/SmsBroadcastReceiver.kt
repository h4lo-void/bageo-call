// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.provider.Telephony
import android.telephony.SubscriptionManager
import android.util.Log
import androidx.core.content.ContextCompat

/**
 * Receives incoming SMS broadcasts in the standalone build, reassembles multi-part
 * messages, ensures the embedded Bageo engine & service are running, and queues the
 * text into [LocalSmsForwarder].
 */
class SmsBroadcastReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent?) {
        if (!BuildConfig.STANDALONE) return
        if (intent?.action != Telephony.Sms.Intents.SMS_RECEIVED_ACTION) return

        val msgs = Telephony.Sms.Intents.getMessagesFromIntent(intent) ?: return
        if (msgs.isEmpty()) return

        val subId = extractSubId(intent)
        // Group multi-part SMS slices by (sender, 5s bucket)
        val grouped = linkedMapOf<String, MutableList<Pair<String, Long>>>()
        for (msg in msgs) {
            if (msg.isStatusReportMessage) continue
            val addr = msg.displayOriginatingAddress?.trim().orEmpty()
            val text = msg.displayMessageBody ?: msg.messageBody ?: ""
            if (text.isEmpty()) continue
            val ts = if (msg.timestampMillis > 0L) msg.timestampMillis else System.currentTimeMillis()
            val bucket = ts / 5_000L
            grouped.getOrPut("$addr|$bucket") { mutableListOf() }.add(text to ts)
        }
        if (grouped.isEmpty()) return

        EmbeddedAgent.ensureStarted(context)
        try {
            ContextCompat.startForegroundService(
                context,
                Intent(context, CallAudioService::class.java).setAction(CallAudioService.ACTION_START),
            )
        } catch (e: Exception) {
            Log.w(TAG, "ensure CallAudioService on SMS failed: ${e.message}")
        }

        for ((key, parts) in grouped) {
            val sender = key.substringBefore('|')
            val fullText = parts.joinToString("") { it.first }
            val ts = parts.maxOf { it.second }
            val refHash = LocalSmsEngine.sha256Hex("${LocalSmsEngine.normalizeSender(sender)}|$ts|$fullText")
                .take(16)
            LocalSmsForwarder.enqueueFromBroadcast(
                ctx = context,
                from = sender,
                body = fullText,
                receivedMs = ts,
                subId = subId,
                refId = "bcast:$refHash",
            )
        }
    }

    private fun extractSubId(intent: Intent): Int? {
        for (key in arrayOf(SubscriptionManager.EXTRA_SUBSCRIPTION_INDEX, "subscription")) {
            val v = intent.getIntExtra(key, -1)
            if (v >= 0) return v
        }
        return null
    }

    companion object {
        private const val TAG = "SmsBroadcastReceiver"
    }
}
