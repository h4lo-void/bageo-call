// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.content.Context
import android.content.pm.PackageManager
import android.database.ContentObserver
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.Telephony
import android.util.Log
import androidx.core.content.ContextCompat
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.security.MessageDigest
import java.util.concurrent.Executors
import java.util.concurrent.ScheduledExecutorService
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit

/**
 * Pure JVM-testable core of the standalone incoming SMS forwarder:
 * - Initializes baseline `_id` / pre-existing RIL files on first run so historical SMS are never re-sent.
 * - Deduplicates across broadcast, `content://sms/inbox`, and `SmsRilHook` within 24 hours.
 * - Persists unacknowledged messages until `incoming_sms_ack` (`ok == true`).
 */
internal class LocalSmsEngine(
    private val storage: Storage,
    private val inbox: InboxReader,
    private val rilDir: File?,
    private val deviceName: String,
    private val sender: (PendingSms) -> Boolean,
    private val clockMs: () -> Long = { System.currentTimeMillis() },
    private val log: (String) -> Unit = { msg -> Log.i(TAG, msg) },
) {
    data class PendingSms(
        val id: String,
        val from: String,
        val to: String,
        val text: String,
        val receivedMs: Long,
        val subId: Int?,
        val device: String,
    )

    interface Storage {
        fun lastInboxId(): Long
        fun setLastInboxId(id: Long)
        fun loadPending(): List<PendingSms>
        fun savePending(items: List<PendingSms>)
        fun loadDedup(): Map<String, Long>
        fun saveDedup(items: Map<String, Long>)
    }

    data class InboxMessage(
        val id: Long,
        val from: String,
        val body: String,
        val receivedMs: Long,
        val subId: Int?,
    )

    interface InboxReader {
        fun maxId(): Long
        fun querySince(lastId: Long): List<InboxMessage>
    }

    private val lock = Any()
    private val pending = mutableListOf<PendingSms>()
    private val dedup = linkedMapOf<String, Long>()
    private val lastSendAttemptMs = mutableMapOf<String, Long>()
    private var loaded = false

    fun pendingCount(): Int = synchronized(lock) {
        ensureLoaded()
        pending.size
    }

    fun initializeBaselineIfNeeded() {
        synchronized(lock) {
            ensureLoaded()
            if (storage.lastInboxId() < 0L) {
                val maxId = inbox.maxId().coerceAtLeast(0L)
                storage.setLastInboxId(maxId)
                // Also drop any pre-existing RIL hook drops from before standalone first run.
                rilDir?.takeIf { it.isDirectory }?.listFiles()?.forEach { f ->
                    if (f.name.endsWith(".json")) f.delete()
                }
                log("SMS forwarder baseline initialized at inbox_id=$maxId")
            }
        }
    }

    fun enqueueFromBroadcast(
        from: String,
        body: String,
        receivedMs: Long,
        subId: Int?,
        refId: String,
    ) {
        synchronized(lock) {
            ensureLoaded()
            if (storage.lastInboxId() < 0L) {
                // Establish inbox baseline first without sweeping old inbox rows.
                val maxId = inbox.maxId().coerceAtLeast(0L)
                storage.setLastInboxId(maxId)
            }
            if (maybeQueue(refId, from, body, receivedMs, subId)) {
                flushLocked()
            }
        }
    }

    fun sweepAndFlush() {
        synchronized(lock) {
            ensureLoaded()
            if (storage.lastInboxId() < 0L) {
                val maxId = inbox.maxId().coerceAtLeast(0L)
                storage.setLastInboxId(maxId)
                rilDir?.takeIf { it.isDirectory }?.listFiles()?.forEach { f ->
                    if (f.name.endsWith(".json")) f.delete()
                }
                return
            }

            // 1. Poll content://sms/inbox rows newer than lastInboxId
            val lastId = storage.lastInboxId()
            val rows = inbox.querySince(lastId)
            var newestId = lastId
            for (row in rows) {
                if (row.id > newestId) newestId = row.id
                maybeQueue(
                    id = "inbox:${row.id}",
                    from = row.from,
                    body = row.body,
                    receivedMs = row.receivedMs,
                    subId = row.subId,
                )
            }
            if (newestId > lastId) {
                storage.setLastInboxId(newestId)
            }

            // 2. Drain Xposed SmsRilHook cache files
            drainRilFilesLocked()

            // 3. Send any unacknowledged messages
            flushLocked()
        }
    }

    fun flushPending() {
        synchronized(lock) {
            ensureLoaded()
            flushLocked()
        }
    }

    fun onAckResult(id: String, ok: Boolean) {
        val cleanId = id.trim()
        if (cleanId.isEmpty() || !ok) return
        synchronized(lock) {
            ensureLoaded()
            val before = pending.size
            pending.removeAll { it.id == cleanId }
            lastSendAttemptMs.remove(cleanId)
            if (pending.size != before) {
                storage.savePending(pending.toList())
                log("incoming_sms_ack id=$cleanId ok=true remaining=${pending.size}")
            }
        }
    }

    private fun drainRilFilesLocked() {
        val dir = rilDir ?: return
        if (!dir.isDirectory) return
        val files = dir.listFiles { f -> f.isFile && f.name.endsWith(".json") }?.sortedBy { it.lastModified() }
            ?: return
        for (file in files) {
            try {
                val raw = file.readText(Charsets.UTF_8)
                val obj = JSONObject(raw)
                val addr = obj.optString("addr").trim()
                val body = obj.optString("body")
                val ts = obj.optLong("ts", clockMs())
                val sub = if (obj.has("sub")) obj.optInt("sub", -1).takeIf { it >= 0 } else null
                val refId = "ril:${file.nameWithoutExtension}"
                maybeQueue(refId, addr, body, ts, sub)
                file.delete()
            } catch (_: Exception) {
                // Leave incomplete tmp/unreadable files alone briefly, ignore malformed JSON.
                if (clockMs() - file.lastModified() > 60_000L) {
                    file.delete()
                }
            }
        }
    }

    private fun maybeQueue(
        id: String,
        from: String,
        body: String,
        receivedMs: Long,
        subId: Int?,
    ): Boolean {
        val cleanId = id.trim()
        val cleanFrom = from.trim()
        if (cleanId.isEmpty() || body.isEmpty()) return false
        if (pending.any { it.id == cleanId }) return false

        val now = clockMs()
        pruneDedupLocked(now)
        val dedupKey = "${normalizeSender(cleanFrom)}|${sha256Hex(body)}"
        val seenAt = dedup[dedupKey]
        if (seenAt != null && now - seenAt < DEDUP_WINDOW_MS) {
            return false
        }
        dedup[dedupKey] = now
        while (dedup.size > MAX_DEDUP_ENTRIES) {
            val oldest = dedup.keys.firstOrNull() ?: break
            dedup.remove(oldest)
        }
        storage.saveDedup(dedup.toMap())

        val payload = PendingSms(
            id = cleanId,
            from = cleanFrom,
            to = "",
            text = body,
            receivedMs = if (receivedMs > 0L) receivedMs else now,
            subId = subId?.takeIf { it >= 0 },
            device = deviceName,
        )

        while (pending.size >= MAX_PENDING_SMS) {
            pending.removeAt(0)
        }
        pending.add(payload)
        storage.savePending(pending.toList())
        log("queued incoming_sms id=$cleanId len=${body.length} pending=${pending.size}")
        return true
    }

    private fun flushLocked() {
        if (pending.isEmpty()) return
        val now = clockMs()
        for (item in pending.toList()) {
            val last = lastSendAttemptMs[item.id] ?: 0L
            if (now - last < RETRY_COOLDOWN_MS) continue
            lastSendAttemptMs[item.id] = now
            val sent = try {
                sender(item)
            } catch (_: Exception) {
                false
            }
            if (!sent) break
        }
    }

    private fun ensureLoaded() {
        if (loaded) return
        loaded = true
        pending.addAll(storage.loadPending())
        val now = clockMs()
        for ((k, ts) in storage.loadDedup()) {
            if (ts > 0L && now - ts < DEDUP_WINDOW_MS) {
                dedup[k] = ts
            }
        }
    }

    private fun pruneDedupLocked(now: Long) {
        val it = dedup.entries.iterator()
        while (it.hasNext()) {
            val e = it.next()
            if (now - e.value >= DEDUP_WINDOW_MS) {
                it.remove()
            }
        }
    }

    companion object {
        private const val TAG = "LocalSmsForwarder"
        const val DEDUP_WINDOW_MS = 24 * 60 * 60 * 1000L
        const val RETRY_COOLDOWN_MS = 5_000L
        const val MAX_PENDING_SMS = 200
        const val MAX_DEDUP_ENTRIES = 500

        fun normalizeSender(raw: String): String =
            raw.trim().filter { !it.isWhitespace() && it != '-' && it != '(' && it != ')' }

        fun sha256Hex(text: String): String {
            val dig = MessageDigest.getInstance("SHA-256").digest(text.toByteArray(Charsets.UTF_8))
            return dig.joinToString("") { "%02x".format(it) }
        }
    }
}

/**
 * Process-wide coordinator for incoming SMS forwarding in the standalone build.
 * Uses event-driven [ContentObserver] + [SmsBroadcastReceiver] instead of periodic polling
 * so idle standby battery drain stays minimal.
 */
object LocalSmsForwarder {
    private const val TAG = "LocalSmsForwarder"
    private const val OBSERVER_DEBOUNCE_MS = 1500L
    private const val RIL_CACHE_DIR = "/data/user_de/0/com.android.phone/cache/pixel-sms-push"

    private val lock = Any()
    @Volatile private var engine: LocalSmsEngine? = null
    @Volatile private var executor: ScheduledExecutorService? = null
    @Volatile private var pendingSweep: ScheduledFuture<*>? = null
    @Volatile private var smsObserver: ContentObserver? = null
    @Volatile private var observedAppContext: Context? = null
    @Volatile private var senderCallback: ((JSONObject) -> Boolean)? = null

    fun start(ctx: Context, sendEvent: (JSONObject) -> Boolean) {
        if (!BuildConfig.STANDALONE) return
        val app = ctx.applicationContext
        val eng = ensureEngine(app, sendEvent)
        synchronized(lock) {
            senderCallback = sendEvent
            if (executor == null) {
                val exec = Executors.newSingleThreadScheduledExecutor { r ->
                    Thread(r, "local-sms-forwarder").apply { isDaemon = true }
                }
                executor = exec
                exec.execute {
                    eng.initializeBaselineIfNeeded()
                    eng.sweepAndFlush()
                }
                registerObserverLocked(app, eng)
            }
        }
    }

    private fun registerObserverLocked(app: Context, eng: LocalSmsEngine) {
        if (smsObserver != null) return
        val hasPerm = ContextCompat.checkSelfPermission(app, android.Manifest.permission.READ_SMS) ==
            PackageManager.PERMISSION_GRANTED
        if (!hasPerm) return
        val obs = object : ContentObserver(Handler(Looper.getMainLooper())) {
            override fun onChange(selfChange: Boolean) {
                scheduleSweep(eng)
            }
        }
        try {
            app.contentResolver.registerContentObserver(Telephony.Sms.CONTENT_URI, true, obs)
            smsObserver = obs
            observedAppContext = app
        } catch (e: Exception) {
            Log.w(TAG, "registerContentObserver failed: ${e.message}")
        }
    }

    private fun scheduleSweep(eng: LocalSmsEngine) {
        val exec = executor ?: return
        synchronized(lock) {
            pendingSweep?.cancel(false)
            pendingSweep = exec.schedule(
                {
                    try {
                        eng.sweepAndFlush()
                    } catch (e: Exception) {
                        Log.w(TAG, "sweep failed: ${e.message}")
                    }
                },
                OBSERVER_DEBOUNCE_MS,
                TimeUnit.MILLISECONDS,
            )
        }
    }

    fun stop() {
        synchronized(lock) {
            senderCallback = null
            pendingSweep?.cancel(false)
            pendingSweep = null
            smsObserver?.let { obs ->
                try {
                    observedAppContext?.contentResolver?.unregisterContentObserver(obs)
                } catch (_: Exception) {
                }
            }
            smsObserver = null
            observedAppContext = null
            executor?.shutdownNow()
            executor = null
        }
    }

    fun onControlOpened() {
        if (!BuildConfig.STANDALONE) return
        val eng = engine ?: return
        val exec = executor
        if (exec != null) {
            exec.execute {
                eng.sweepAndFlush()
                eng.flushPending()
            }
        } else {
            eng.sweepAndFlush()
            eng.flushPending()
        }
    }

    fun onAck(ack: JSONObject) {
        if (!BuildConfig.STANDALONE) return
        engine?.onAckResult(ack.optString("id"), ack.optBoolean("ok", false))
    }

    fun enqueueFromBroadcast(
        ctx: Context,
        from: String,
        body: String,
        receivedMs: Long,
        subId: Int?,
        refId: String,
    ) {
        if (!BuildConfig.STANDALONE) return
        val eng = ensureEngine(ctx.applicationContext, senderCallback)
        val exec = executor
        if (exec != null) {
            exec.execute { eng.enqueueFromBroadcast(from, body, receivedMs, subId, refId) }
        } else {
            Thread({ eng.enqueueFromBroadcast(from, body, receivedMs, subId, refId) }, "sms-bcast").start()
        }
    }

    private fun ensureEngine(ctx: Context, sendCb: ((JSONObject) -> Boolean)?): LocalSmsEngine {
        synchronized(lock) {
            if (sendCb != null) senderCallback = sendCb
            engine?.let { return it }
            val storage = object : LocalSmsEngine.Storage {
                override fun lastInboxId(): Long = Prefs.smsLastInboxId(ctx)
                override fun setLastInboxId(id: Long) = Prefs.setSmsLastInboxId(ctx, id)

                override fun loadPending(): List<LocalSmsEngine.PendingSms> {
                    val raw = Prefs.smsPendingQueue(ctx) ?: return emptyList()
                    val out = mutableListOf<LocalSmsEngine.PendingSms>()
                    try {
                        val arr = JSONArray(raw)
                        for (i in 0 until arr.length()) {
                            val obj = arr.optJSONObject(i) ?: continue
                            out += LocalSmsEngine.PendingSms(
                                id = obj.optString("id"),
                                from = obj.optString("from"),
                                to = obj.optString("to"),
                                text = obj.optString("text"),
                                receivedMs = obj.optLong("received_ms"),
                                subId = if (obj.has("sub_id")) obj.optInt("sub_id", -1).takeIf { it >= 0 } else null,
                                device = obj.optString("device"),
                            )
                        }
                    } catch (_: Exception) {
                    }
                    return out
                }

                override fun savePending(items: List<LocalSmsEngine.PendingSms>) {
                    val arr = JSONArray()
                    for (item in items) arr.put(toJson(item))
                    Prefs.setSmsPendingQueue(ctx, arr.toString())
                }

                override fun loadDedup(): Map<String, Long> {
                    val raw = Prefs.smsDedupCache(ctx) ?: return emptyMap()
                    val out = linkedMapOf<String, Long>()
                    try {
                        val obj = JSONObject(raw)
                        val keys = obj.keys()
                        while (keys.hasNext()) {
                            val k = keys.next()
                            out[k] = obj.optLong(k, 0L)
                        }
                    } catch (_: Exception) {
                    }
                    return out
                }

                override fun saveDedup(items: Map<String, Long>) {
                    val obj = JSONObject()
                    for ((k, v) in items) obj.put(k, v)
                    Prefs.setSmsDedupCache(ctx, obj.toString())
                }
            }
            val inbox = AndroidInboxReader(ctx)
            val rilDir = File(RIL_CACHE_DIR)
            val created = LocalSmsEngine(
                storage = storage,
                inbox = inbox,
                rilDir = rilDir,
                deviceName = Build.MODEL.orEmpty().ifBlank { "Pixel" },
                sender = { item -> senderCallback?.invoke(toJson(item)) ?: false },
            )
            engine = created
            return created
        }
    }

    private fun toJson(item: LocalSmsEngine.PendingSms): JSONObject {
        val obj = JSONObject()
            .put("type", "incoming_sms")
            .put("id", item.id)
            .put("from", item.from)
            .put("to", item.to)
            .put("text", item.text)
            .put("received_ms", item.receivedMs)
            .put("device", item.device)
        item.subId?.let { obj.put("sub_id", it) }
        return obj
    }

    private class AndroidInboxReader(private val ctx: Context) : LocalSmsEngine.InboxReader {
        private val inboxUri: Uri = Telephony.Sms.Inbox.CONTENT_URI

        private fun hasReadSms(): Boolean =
            ContextCompat.checkSelfPermission(ctx, android.Manifest.permission.READ_SMS) ==
                PackageManager.PERMISSION_GRANTED

        override fun maxId(): Long {
            if (!hasReadSms()) return 0L
            return try {
                ctx.contentResolver.query(
                    inboxUri,
                    arrayOf("_id"),
                    null,
                    null,
                    "_id DESC LIMIT 1",
                )?.use { cur ->
                    if (cur.moveToFirst()) cur.getLong(0) else 0L
                } ?: 0L
            } catch (e: Exception) {
                Log.w(TAG, "inbox maxId failed: ${e.message}")
                0L
            }
        }

        override fun querySince(lastId: Long): List<LocalSmsEngine.InboxMessage> {
            if (!hasReadSms()) return emptyList()
            val out = mutableListOf<LocalSmsEngine.InboxMessage>()
            try {
                ctx.contentResolver.query(
                    inboxUri,
                    arrayOf("_id", "address", "body", "date", "sub_id"),
                    "_id > ?",
                    arrayOf(lastId.toString()),
                    "_id ASC LIMIT 50",
                )?.use { cur ->
                    val idxId = cur.getColumnIndex("_id")
                    val idxAddr = cur.getColumnIndex("address")
                    val idxBody = cur.getColumnIndex("body")
                    val idxDate = cur.getColumnIndex("date")
                    val idxSub = cur.getColumnIndex("sub_id")
                    while (cur.moveToNext()) {
                        val id = if (idxId >= 0) cur.getLong(idxId) else continue
                        val from = if (idxAddr >= 0) cur.getString(idxAddr).orEmpty() else ""
                        val body = if (idxBody >= 0) cur.getString(idxBody).orEmpty() else ""
                        val date = if (idxDate >= 0) cur.getLong(idxDate) else 0L
                        val sub = if (idxSub >= 0 && !cur.isNull(idxSub)) {
                            cur.getInt(idxSub).takeIf { it >= 0 }
                        } else {
                            null
                        }
                        if (body.isNotEmpty()) {
                            out += LocalSmsEngine.InboxMessage(id, from, body, date, sub)
                        }
                    }
                }
            } catch (e: Exception) {
                Log.w(TAG, "inbox query failed: ${e.message}")
            }
            return out
        }
    }
}
