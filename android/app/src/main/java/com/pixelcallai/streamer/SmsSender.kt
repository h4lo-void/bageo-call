package com.pixelcallai.streamer

import android.Manifest
import android.annotation.SuppressLint
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
import android.telephony.SmsMessage
import android.telephony.SubscriptionInfo
import android.telephony.SubscriptionManager
import android.util.Log
import androidx.core.content.ContextCompat
import org.json.JSONObject
import java.util.concurrent.atomic.AtomicInteger
import kotlin.random.Random

/**
 * Agent `send_sms` (control connection): sends a text message from the phone's own
 * number — the SIM named by `sim` (carrier or display name, or the SIM's own number;
 * exactly one active SIM must match, see [SimSubscriptions.matching]), else the SIM with
 * subscription id `sub_id`, else the default SMS SIM — and answers with
 * `send_sms_result` once every part was reported sent, one part failed, or [TIMEOUT_MS]
 * passed. A part reported after that timeout still answers again with the real result.
 * When the network reports delivery, `send_sms_delivered` follows (`ok` false when the
 * message failed for good).
 *
 * The answers for the last [REPLAY_LIMIT] ids are kept; [replay] sends them again after
 * the control connection reconnects (the agent drops repeats by id). A `send_sms` whose
 * id was already sent is answered from there instead of texting twice.
 *
 * Error codes: no_permission, bad_request, no_service, radio_off, generic_failure,
 * timeout, exception, no_sim (no SIM or more than one matched `sim`; nothing is sent).
 * A `send_sms` whose id is still being sent is ignored.
 * All state lives on the main thread. Logs show only the last 3 digits of the number
 * and never the text; for `sim`, only how many SIMs matched and the one used.
 */
class SmsSender(
    private val context: Context,
    private val reply: (JSONObject) -> Unit,
) {
    private class Job(val token: Long, val parts: Int) {
        val sent = BooleanArray(parts)
        var sentCount = 0
        /** The `timeout` answer went out; a late part report still answers again. */
        var timedOut = false
        var timer: Runnable? = null
    }

    private class Delivery(val token: Long, val parts: Int) {
        val delivered = BooleanArray(parts)
        var deliveredCount = 0
        var expiry: Runnable? = null
    }

    /** The last answers for one id, sent again by [replay]. */
    private class Answers {
        var result: JSONObject? = null
        var delivered: JSONObject? = null
    }

    private val handler = Handler(Looper.getMainLooper())
    private val sentAction = "${context.packageName}.SMS_SENT"
    private val deliveredAction = "${context.packageName}.SMS_DELIVERED"
    private val jobs = HashMap<String, Job>()
    private val deliveries = HashMap<String, Delivery>()
    /** Oldest first; at most [REPLAY_LIMIT] ids. */
    private val answers = LinkedHashMap<String, Answers>()
    private val requestCodes = AtomicInteger(Random.nextInt(1, Int.MAX_VALUE / 2))
    private var receiverRegistered = false
    private var closed = false

    private val receiver = object : BroadcastReceiver() {
        override fun onReceive(ctx: Context, intent: Intent) {
            val id = intent.getStringExtra(EXTRA_ID) ?: return
            val token = intent.getLongExtra(EXTRA_TOKEN, 0L)
            val part = intent.getIntExtra(EXTRA_PART, -1)
            when (intent.action) {
                sentAction -> onPartSent(id, token, part, resultCode)
                deliveredAction -> onPartDelivered(id, token, part, deliveryOutcome(intent))
            }
        }
    }

    /** Any thread. */
    fun handle(obj: JSONObject) {
        handler.post { start(obj) }
    }

    /** Any thread: send the kept answers again (the control connection reconnected). */
    fun replay() {
        handler.post {
            if (closed || answers.isEmpty()) return@post
            Log.i(TAG, "send_sms: replaying the answers for ${answers.size} id(s) after reconnect")
            for (a in answers.values.toList()) {
                a.result?.let { send(it) }
                a.delivered?.let { send(it) }
            }
        }
    }

    /** Main thread: drop every message in flight without answering (the connection goes too). */
    fun close() {
        closed = true
        for (job in jobs.values) job.timer?.let { handler.removeCallbacks(it) }
        for (d in deliveries.values) d.expiry?.let { handler.removeCallbacks(it) }
        jobs.clear()
        deliveries.clear()
        answers.clear()
        unregisterReceiver()
    }

    private fun start(obj: JSONObject) {
        if (closed) return
        val id = obj.optString("id")
        val to = obj.optString("to")
        val text = obj.optString("text")
        val commandSubId = SimSubscriptions.fromCommand(obj)
        val simName = SimSubscriptions.simFromCommand(obj)
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
        val earlier = answers[id]
        val earlierResult = earlier?.result
        if (earlierResult != null && earlierResult.optBoolean("ok")) {
            Log.i(TAG, "send_sms id=$id was already sent; answering again instead of sending twice")
            send(earlierResult)
            earlier.delivered?.let { send(it) }
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
            val subId = if (simName != null) {
                val found = matchingSims(simName)
                if (found.size != 1) {
                    Log.w(TAG, "send_sms id=$id: ${found.size} SIMs match the named SIM; not sent")
                    answer(id, "no_sim")
                    return
                }
                Log.i(TAG, "send_sms id=$id: 1 SIM matches the named SIM, sub_id=${found[0]}")
                found[0]
            } else {
                commandSubId
            }
            val sms = smsManager(subId)
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
            val deliveryIntents = ArrayList<PendingIntent>(parts.size)
            for (i in parts.indices) {
                sentIntents.add(pendingIntent(sentAction, id, token, i, mutable = false))
                deliveryIntents.add(pendingIntent(deliveredAction, id, token, i, mutable = true))
            }
            val job = Job(token, parts.size)
            ensureReceiver()
            jobs[id] = job
            dropDelivery(id)
            val delivery = Delivery(token, parts.size)
            deliveries[id] = delivery
            delivery.expiry = Runnable { onDeliveryExpired(id, token) }.also {
                handler.postDelayed(it, DELIVERY_WAIT_MS)
            }
            schedule(job, TIMEOUT_MS) { onTimeout(id, token) }
            Log.i(TAG, "send_sms id=$id to=${mask(to)} parts=${parts.size} sub_id=${subId ?: "default"}")
            sms.sendMultipartTextMessage(to, null, parts, sentIntents, deliveryIntents)
        } catch (e: SecurityException) {
            Log.w(TAG, "send_sms id=$id refused: ${e.javaClass.simpleName}")
            drop(id)
            dropDelivery(id)
            answer(id, "no_permission")
        } catch (e: Exception) {
            Log.w(TAG, "send_sms id=$id failed: ${e.javaClass.simpleName}")
            drop(id)
            dropDelivery(id)
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
            Log.w(
                TAG,
                "send_sms id=$id part ${part + 1}/${job.parts} failed code=$code -> $error late=${job.timedOut}",
            )
            drop(id)
            dropDelivery(id)
            answer(id, error)
            return
        }
        if (!job.sent[part]) {
            job.sent[part] = true
            job.sentCount++
        }
        if (job.sentCount == job.parts) {
            Log.i(TAG, "send_sms id=$id sent (${job.parts} parts) late=${job.timedOut}")
            drop(id)
            answer(id, null)
        }
    }

    private fun onTimeout(id: String, token: Long) {
        val job = jobs[id] ?: return
        if (job.token != token || job.timedOut) return
        Log.w(TAG, "send_sms id=$id timed out (${job.sentCount}/${job.parts} parts sent); a later report still answers")
        job.timedOut = true
        answer(id, "timeout")
        // Keep listening: the radio can still report the parts, and then the real result goes out.
        schedule(job, LATE_RESULT_MS) { onLateWindowOver(id, token) }
    }

    private fun onLateWindowOver(id: String, token: Long) {
        val job = jobs[id] ?: return
        if (job.token != token) return
        Log.w(TAG, "send_sms id=$id: no report ${LATE_RESULT_MS / 60_000} min after the timeout; stop waiting")
        drop(id)
        dropDelivery(id)
    }

    private fun onPartDelivered(id: String, token: Long, part: Int, outcome: Boolean?) {
        val d = deliveries[id] ?: return
        if (d.token != token || part !in 0 until d.parts) return
        when (outcome) {
            null -> Log.i(TAG, "send_sms id=$id part ${part + 1}/${d.parts}: delivery still pending")
            false -> {
                Log.w(TAG, "send_sms id=$id part ${part + 1}/${d.parts} not delivered")
                dropDelivery(id)
                answerDelivered(id, false)
            }
            true -> {
                if (!d.delivered[part]) {
                    d.delivered[part] = true
                    d.deliveredCount++
                }
                if (d.deliveredCount == d.parts) {
                    Log.i(TAG, "send_sms id=$id delivered (${d.parts} parts)")
                    dropDelivery(id)
                    answerDelivered(id, true)
                }
            }
        }
    }

    private fun onDeliveryExpired(id: String, token: Long) {
        val d = deliveries[id] ?: return
        if (d.token != token) return
        Log.i(TAG, "send_sms id=$id: no delivery report within ${DELIVERY_WAIT_MS / 60_000} min; stop waiting")
        dropDelivery(id)
    }

    /** [error] null = sent. Kept for [replay]; dropped by the client while the connection is closed. */
    private fun answer(id: String, error: String?) {
        if (closed) return
        val msg = JSONObject()
            .put("type", "send_sms_result")
            .put("id", id)
            .put("ok", error == null)
        if (error != null) msg.put("error", error)
        if (id.isNotEmpty()) remember(id).result = msg
        send(msg)
    }

    private fun answerDelivered(id: String, ok: Boolean) {
        if (closed) return
        val msg = JSONObject()
            .put("type", "send_sms_delivered")
            .put("id", id)
            .put("ok", ok)
        remember(id).delivered = msg
        send(msg)
    }

    private fun send(msg: JSONObject) {
        try {
            reply(msg)
        } catch (e: Exception) {
            Log.w(TAG, "${msg.optString("type")} not sent: ${e.message}")
        }
    }

    /** The answers kept for [id], moved to the newest place; the oldest ids beyond the limit go. */
    private fun remember(id: String): Answers {
        val kept = answers.remove(id) ?: Answers()
        answers[id] = kept
        while (answers.size > REPLAY_LIMIT) answers.remove(answers.keys.first())
        return kept
    }

    private fun schedule(job: Job, delayMs: Long, block: () -> Unit) {
        job.timer?.let { handler.removeCallbacks(it) }
        val r = Runnable(block)
        job.timer = r
        handler.postDelayed(r, delayMs)
    }

    private fun drop(id: String) {
        val job = jobs.remove(id) ?: return
        job.timer?.let { handler.removeCallbacks(it) }
        unregisterIfIdle()
    }

    private fun dropDelivery(id: String) {
        val d = deliveries.remove(id) ?: return
        d.expiry?.let { handler.removeCallbacks(it) }
        unregisterIfIdle()
    }

    private fun unregisterIfIdle() {
        if (jobs.isEmpty() && deliveries.isEmpty()) unregisterReceiver()
    }

    private fun pendingIntent(action: String, id: String, token: Long, part: Int, mutable: Boolean): PendingIntent {
        val intent = Intent(action)
            .setPackage(context.packageName)
            .putExtra(EXTRA_ID, id)
            .putExtra(EXTRA_TOKEN, token)
            .putExtra(EXTRA_PART, part)
        val code = requestCodes.updateAndGet { if (it == Int.MAX_VALUE) 1 else it + 1 }
        // Android adds the status report (pdu, format) to the delivery intent when it sends
        // it; an immutable PendingIntent would drop those extras. The intent is explicit.
        val mutability = when {
            !mutable -> PendingIntent.FLAG_IMMUTABLE
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.S -> PendingIntent.FLAG_MUTABLE
            else -> 0
        }
        return PendingIntent.getBroadcast(
            context,
            code,
            intent,
            mutability or PendingIntent.FLAG_CANCEL_CURRENT,
        )
    }

    private fun ensureReceiver() {
        if (receiverRegistered) return
        val filter = IntentFilter(sentAction).apply { addAction(deliveredAction) }
        ContextCompat.registerReceiver(
            context,
            receiver,
            filter,
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

    /** The status report in a delivery intent: see [deliveryOutcome]. */
    private fun deliveryOutcome(intent: Intent): Boolean? {
        val pdu = intent.getByteArrayExtra("pdu") ?: return null
        val format = intent.getStringExtra("format")
        val status = try {
            SmsMessage.createFromPdu(pdu, format)?.status
        } catch (_: Exception) {
            null
        } ?: return null
        return deliveryOutcome(status, format)
    }

    /**
     * The active SIMs [wanted] names ([SimSubscriptions.matching]). Their numbers are read
     * only when [wanted] looks like a number; a number that cannot be read is not compared.
     */
    @SuppressLint("MissingPermission")
    private fun matchingSims(wanted: String): List<Int> {
        val sm = context.getSystemService(SubscriptionManager::class.java) ?: return emptyList()
        val withNumbers = SimSubscriptions.looksLikeNumber(wanted)
        val sims = (sm.activeSubscriptionInfoList ?: emptyList()).map { info ->
            SimSubscriptions.Sim(
                subId = info.subscriptionId,
                carrierName = info.carrierName?.toString(),
                displayName = info.displayName?.toString(),
                number = if (withNumbers) numberOf(sm, info) else null,
            )
        }
        return SimSubscriptions.matching(sims, wanted)
    }

    /** The SIM's own number (READ_PHONE_NUMBERS on Android 13+); null when it may not be read. */
    @SuppressLint("MissingPermission")
    @Suppress("DEPRECATION")
    private fun numberOf(sm: SubscriptionManager, info: SubscriptionInfo): String? =
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                sm.getPhoneNumber(info.subscriptionId)
            } else {
                info.number
            }
        } catch (_: SecurityException) {
            null
        }

    /** The SIM [subId] names, or the default SMS SIM. */
    @Suppress("DEPRECATION")
    private fun smsManager(subId: Int?): SmsManager? {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            val default = context.getSystemService(SmsManager::class.java) ?: return null
            return if (subId != null) default.createForSubscriptionId(subId) else default
        }
        return if (subId != null) SmsManager.getSmsManagerForSubscriptionId(subId) else SmsManager.getDefault()
    }

    companion object {
        private const val TAG = "SmsSender"
        private const val TIMEOUT_MS = 60_000L
        /** After the timeout answer, a late "sent" or "failed" report still counts this long. */
        private const val LATE_RESULT_MS = 10 * 60_000L
        /** How long a delivery report is waited for. */
        private const val DELIVERY_WAIT_MS = 60 * 60_000L
        /** Ids whose answers are sent again after a reconnect. */
        private const val REPLAY_LIMIT = 20
        private const val MAX_ID_LENGTH = 64
        private const val MAX_TEXT_CHARS = 500
        private val NUMBER = Regex("^\\+?[0-9]{3,20}$")
        private const val EXTRA_ID = "id"
        private const val EXTRA_TOKEN = "token"
        private const val EXTRA_PART = "part"
        private const val FORMAT_3GPP2 = "3gpp2"

        private fun mask(number: String): String = "***" + number.takeLast(3)

        /**
         * A status report: true delivered, false failed for good, null still pending or
         * unknown. 3GPP (TP-Status): below 0x20 done, 0x20–0x3F the centre still tries,
         * 0x40 and up failed. 3GPP2: error class in bits 24–25 (0 none, 2 temporary,
         * 3 permanent).
         */
        internal fun deliveryOutcome(status: Int, format: String?): Boolean? =
            if (format == FORMAT_3GPP2) {
                when ((status ushr 24) and 0x03) {
                    0 -> true
                    3 -> false
                    else -> null
                }
            } else {
                when {
                    status < 0 -> null
                    status >= 0x40 -> false
                    status >= 0x20 -> null
                    else -> true
                }
            }
    }
}
