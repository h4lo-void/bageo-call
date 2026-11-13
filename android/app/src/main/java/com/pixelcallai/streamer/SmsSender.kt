package com.pixelcallai.streamer

import android.Manifest
import android.app.Activity
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.telephony.SmsManager
import android.util.Log
import androidx.core.content.ContextCompat
import org.json.JSONObject
import java.util.concurrent.atomic.AtomicInteger
import kotlin.random.Random

/**
 * Agent `send_sms` (control connection): sends a text message from the phone's own
 * number (the default SMS SIM) and answers with `send_sms_result` once every part was
 * reported sent, one part failed, or [TIMEOUT_MS] passed.
 *
 * Error codes: no_permission, bad_request, no_service, radio_off, generic_failure,
 * timeout, exception. A `send_sms` whose id is still being sent is ignored.
 * All state lives on the main thread. Logs show only the last 3 digits of the number
 * and never the text.
 */
class SmsSender(
    private val context: Context,
    private val reply: (JSONObject) -> Unit,
) {
    private class Job(val token: Long, val parts: Int, val timeout: Runnable) {
        val sent = BooleanArray(parts)
        var sentCount = 0
    }

    private val handler = Handler(Looper.getMainLooper())
    private val action = "${context.packageName}.SMS_SENT"
    private val jobs = HashMap<String, Job>()
    private val requestCodes = AtomicInteger(Random.nextInt(1, Int.MAX_VALUE / 2))
    private var receiverRegistered = false
    private var closed = false

    private val receiver = object : BroadcastReceiver() {
        override fun onReceive(ctx: Context, intent: Intent) {
            if (intent.action != action) return
            val id = intent.getStringExtra(EXTRA_ID) ?: return
            onPartSent(id, intent.getLongExtra(EXTRA_TOKEN, 0L), intent.getIntExtra(EXTRA_PART, -1), resultCode)
        }
    }

    /** Any thread. */
    fun handle(obj: JSONObject) {
        handler.post { start(obj) }
    }

    /** Main thread: drop every message in flight without answering (the connection goes too). */
    fun close() {
        closed = true
        for (job in jobs.values) handler.removeCallbacks(job.timeout)
        jobs.clear()
        unregisterReceiver()
    }

    private fun start(obj: JSONObject) {
        if (closed) return
        val id = obj.optString("id")
        val to = obj.optString("to")
        val text = obj.optString("text")
        if (id.isEmpty() || id.length > MAX_ID_LENGTH || !NUMBER.matches(to) ||
            text.isBlank() || text.codePointCount(0, text.length) > MAX_TEXT_CHARS
        ) {
            Log.w(TAG, "send_sms bad request id_len=${id.length} to=${mask(to)} text_len=${text.length}")
            answer(id, "bad_request")
            return
        }
        if (jobs.containsKey(id)) {
            Log.i(TAG, "send_sms id=$id is already being sent; ignored")
            return
        }
        if (ContextCompat.checkSelfPermission(context, Manifest.permission.SEND_SMS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            Log.w(TAG, "send_sms id=$id: SEND_SMS not granted")
            answer(id, "no_permission")
            return
        }
        try {
            val sms = smsManager()
            if (sms == null) {
                Log.w(TAG, "send_sms id=$id: no SmsManager")
                answer(id, "generic_failure")
                return
            }
            val parts = sms.divideMessage(text)
            if (parts.isNullOrEmpty()) {
                answer(id, "bad_request")
                return
            }
            val token = Random.nextLong()
            val sentIntents = ArrayList<PendingIntent>(parts.size)
            for (i in parts.indices) sentIntents.add(sentIntent(id, token, i))
            val job = Job(token, parts.size, Runnable { onTimeout(id, token) })
            ensureReceiver()
            jobs[id] = job
            handler.postDelayed(job.timeout, TIMEOUT_MS)
            Log.i(TAG, "send_sms id=$id to=${mask(to)} parts=${parts.size}")
            sms.sendMultipartTextMessage(to, null, parts, sentIntents, null)
        } catch (e: SecurityException) {
            Log.w(TAG, "send_sms id=$id refused: ${e.javaClass.simpleName}")
            drop(id)
            answer(id, "no_permission")
        } catch (e: Exception) {
            Log.w(TAG, "send_sms id=$id failed: ${e.javaClass.simpleName}")
            drop(id)
            answer(id, "exception")
        }
    }

    private fun onPartSent(id: String, token: Long, part: Int, code: Int) {
        val job = jobs[id] ?: return
        if (job.token != token || part !in 0 until job.parts) return
        if (code != Activity.RESULT_OK) {
            val error = when (code) {
                SmsManager.RESULT_ERROR_RADIO_OFF -> "radio_off"
                SmsManager.RESULT_ERROR_NO_SERVICE -> "no_service"
                else -> "generic_failure"
            }
            Log.w(TAG, "send_sms id=$id part ${part + 1}/${job.parts} failed code=$code -> $error")
            drop(id)
            answer(id, error)
            return
        }
        if (!job.sent[part]) {
            job.sent[part] = true
            job.sentCount++
        }
        if (job.sentCount == job.parts) {
            Log.i(TAG, "send_sms id=$id sent (${job.parts} parts)")
            drop(id)
            answer(id, null)
        }
    }

    private fun onTimeout(id: String, token: Long) {
        val job = jobs[id] ?: return
        if (job.token != token) return
        Log.w(TAG, "send_sms id=$id timed out (${job.sentCount}/${job.parts} parts sent)")
        drop(id)
        answer(id, "timeout")
    }

    /** [error] null = sent. Dropped by the client when the control connection is closed. */
    private fun answer(id: String, error: String?) {
        if (closed) return
        val msg = JSONObject()
            .put("type", "send_sms_result")
            .put("id", id)
            .put("ok", error == null)
        if (error != null) msg.put("error", error)
        try {
            reply(msg)
        } catch (e: Exception) {
            Log.w(TAG, "send_sms_result not sent: ${e.message}")
        }
    }

    private fun drop(id: String) {
        val job = jobs.remove(id) ?: return
        handler.removeCallbacks(job.timeout)
        if (jobs.isEmpty()) unregisterReceiver()
    }

    private fun sentIntent(id: String, token: Long, part: Int): PendingIntent {
        val intent = Intent(action)
            .setPackage(context.packageName)
            .putExtra(EXTRA_ID, id)
            .putExtra(EXTRA_TOKEN, token)
            .putExtra(EXTRA_PART, part)
        val code = requestCodes.updateAndGet { if (it == Int.MAX_VALUE) 1 else it + 1 }
        return PendingIntent.getBroadcast(
            context,
            code,
            intent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_CANCEL_CURRENT,
        )
    }

    private fun ensureReceiver() {
        if (receiverRegistered) return
        ContextCompat.registerReceiver(
            context,
            receiver,
            IntentFilter(action),
            null,
            handler,
            ContextCompat.RECEIVER_NOT_EXPORTED,
        )
        receiverRegistered = true
    }

    private fun unregisterReceiver() {
        if (!receiverRegistered) return
        receiverRegistered = false
        try {
            context.unregisterReceiver(receiver)
        } catch (_: IllegalArgumentException) {
        }
    }

    @Suppress("DEPRECATION")
    private fun smsManager(): SmsManager? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            context.getSystemService(SmsManager::class.java)
        } else {
            SmsManager.getDefault()
        }

    companion object {
        private const val TAG = "SmsSender"
        private const val TIMEOUT_MS = 60_000L
        private const val MAX_ID_LENGTH = 64
        private const val MAX_TEXT_CHARS = 500
        private val NUMBER = Regex("^\\+?[0-9]{3,20}$")
        private const val EXTRA_ID = "id"
        private const val EXTRA_TOKEN = "token"
        private const val EXTRA_PART = "part"

        private fun mask(number: String): String = "***" + number.takeLast(3)
    }
}
