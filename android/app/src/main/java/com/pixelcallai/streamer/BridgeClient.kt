package com.pixelcallai.streamer

import android.util.Log
import org.java_websocket.client.WebSocketClient
import org.java_websocket.handshake.ServerHandshake
import org.json.JSONObject
import java.io.File
import java.io.FileOutputStream
import java.net.URI
import java.nio.ByteBuffer
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

class BridgeClient(
    private val url: String,
    private val onBinary: (ByteArray) -> Unit,
    private val onText: (String) -> Unit,
    private val pcmDumpDir: File? = null,
    /** When true, only wait for agent `ready` — no hello/media session / no IVR start. */
    private val probeOnly: Boolean = false,
    private val onAgentReady: (() -> Unit)? = null,
    /**
     * The connection closed or failed without [close] being called (agent gone, network
     * lost, no pong within [CONNECTION_LOST_TIMEOUT_S]). Runs on the socket's thread.
     */
    private val onDisconnected: ((String) -> Unit)? = null,
) {
    private var client: WebSocketClient? = null
    private val open = AtomicBoolean(false)
    /** [close] was called: the close that follows is ours, not a lost connection. */
    private val closedLocally = AtomicBoolean(false)
    /** Call audio frames dropped while the connection was not open (logged once). */
    private val droppedPcm = AtomicLong(0)
    private val agentReady = AtomicBoolean(false)
    private val ttsBytes = AtomicLong(0)
    private var ttsFrames = 0
    private var dumpStream: FileOutputStream? = null
    private var debugSerial: String = "unknown"
    /** JSON queued before WS open (local menu events). */
    private val pendingJson = java.util.concurrent.ConcurrentLinkedQueue<JSONObject>()

    fun isOpen(): Boolean = open.get() && client?.isOpen == true
    fun isAgentReady(): Boolean = agentReady.get()

    fun connect(
        serial: String,
        callerId: String,
        sampleRate: Int,
        simCountry: String = "",
        /** Subscription id of the SIM the call uses; not sent when unknown. */
        simSubId: Int? = null,
    ) {
        debugSerial = serial
        agentReady.set(false)
        val ws = object : WebSocketClient(URI(url)) {
            override fun onOpen(handshakedata: ServerHandshake?) {
                open.set(true)
                droppedPcm.set(0)
                if (probeOnly) {
                    Log.i(TAG, "bridge probe open ${Prefs.redactUrl(url)} serial=$serial (wait ready, no hello)")
                    return
                }
                val hello = JSONObject()
                    .put("type", "hello")
                    .put("serial", serial)
                    .put("caller_id", callerId)
                    .put("sample_rate", sampleRate)
                    .put("mode", "SILENT_LOCAL")
                    .put("sim_country", simCountry)
                    .put("play_tts_locally", false)
                    .put("local_menu", BuildConfig.LOCAL_MENU_ENABLED)
                    .put("model", android.os.Build.MODEL)
                    .put("app_version", BuildConfig.VERSION_NAME)
                if (simSubId != null) hello.put("sim_sub_id", simSubId)
                send(hello.toString())
                val ringing = JSONObject()
                    .put("type", "ringing")
                    .put("serial", serial)
                    .put("caller_id", callerId)
                send(ringing.toString())
                flushPending()
                Log.i(
                    TAG,
                    "bridge open ${Prefs.redactUrl(url)}; sent hello+ringing local_menu=${BuildConfig.LOCAL_MENU_ENABLED} serial=$serial caller=$callerId",
                )
            }

            override fun onMessage(message: String?) {
                if (message == null) return
                try {
                    val obj = JSONObject(message)
                    val t = obj.optString("type")
                    if (t == "ready" || t == "hello_ack" || t == "hello_ok") {
                        if (!agentReady.getAndSet(true)) {
                            Log.i(TAG, "agent ready type=$t serial=$debugSerial probe=$probeOnly")
                            onAgentReady?.invoke()
                        }
                    }
                } catch (_: Exception) {
                }
                if (!probeOnly) {
                    onText(message)
                }
                if (probeOnly) return
                try {
                    val obj = JSONObject(message)
                    when (obj.optString("type")) {
                        "hello_ack", "hello_ok", "ack", "ready" -> {
                            Log.i(TAG, "TEXT ${obj.optString("type")}: $message")
                        }
                        "tts_start" -> {
                            ttsBytes.set(0)
                            ttsFrames = 0
                            openDump()
                            Log.i(TAG, "TEXT tts_start (PLAY_TTS_LOCALLY=false; uplink/file only)")
                        }
                        "tts_end" -> {
                            closeDump()
                            Log.i(TAG, "TEXT tts_end frames=$ttsFrames bytes=${ttsBytes.get()}")
                        }
                        "hangup" -> {
                            Log.i(TAG, "TEXT hangup from agent: $message")
                        }
                        "dtmf" -> {
                            Log.i(
                                TAG,
                                "TEXT dtmf digit=${obj.optString("digit")} conf=${obj.optDouble("confidence")}",
                            )
                        }
                        else -> Log.i(TAG, "TEXT type=${obj.optString("type")} len=${message.length}")
                    }
                } catch (_: Exception) {
                    Log.i(TAG, "TEXT raw len=${message.length}")
                }
            }

            override fun onMessage(bytes: ByteBuffer?) {
                if (probeOnly) return
                if (bytes == null) return
                val arr = ByteArray(bytes.remaining())
                bytes.get(arr)
                ttsFrames++
                ttsBytes.addAndGet(arr.size.toLong())
                // Rate-limit logs — high-volume binary relay must not stall the USB path.
                if (ttsFrames <= 3 || ttsFrames % 50 == 0) {
                    Log.i(TAG, "PCM frame#$ttsFrames bytes=${arr.size} total=${ttsBytes.get()} serial=$debugSerial")
                }
                try {
                    dumpStream?.write(arr)
                } catch (e: Exception) {
                    Log.w(TAG, "pcm dump write failed: ${e.message}")
                }
                onBinary(arr)
            }

            override fun onClose(code: Int, reason: String?, remote: Boolean) {
                open.set(false)
                closeDump()
                Log.w(
                    TAG,
                    "bridge closed code=$code reason=$reason remote=$remote probe=$probeOnly " +
                        "droppedPcmFrames=${droppedPcm.get()}",
                )
                if (!closedLocally.get()) onDisconnected?.invoke("closed code=$code remote=$remote")
            }

            override fun onError(ex: Exception?) {
                Log.e(TAG, "bridge error", ex)
                if (!closedLocally.get()) onDisconnected?.invoke("error ${ex?.javaClass?.simpleName}")
            }
        }
        client = ws
        try {
            // Notice a silently dead connection (no pong) instead of streaming into it.
            ws.connectionLostTimeout = CONNECTION_LOST_TIMEOUT_S
        } catch (_: Exception) {
        }
        ws.connect()
    }

    private fun openDump() {
        val dir = pcmDumpDir ?: return
        try {
            if (!dir.exists() && !dir.mkdirs()) {
                Log.w(TAG, "pcm dump mkdirs failed: ${dir.absolutePath}")
                return
            }
            val f = File(dir, "tts_dump_${System.currentTimeMillis()}.pcm")
            dumpStream = FileOutputStream(f)
            Log.i(TAG, "pcm dump -> ${f.absolutePath}")
        } catch (e: Exception) {
            Log.w(TAG, "pcm dump open failed: ${e.message}")
        }
    }

    private fun closeDump() {
        try {
            dumpStream?.flush()
            dumpStream?.close()
        } catch (_: Exception) {
        }
        dumpStream = null
    }

    fun sendPcm(pcm: ByteArray) {
        val c = client ?: return
        if (!open.get() || !c.isOpen) {
            // One line per outage, not one per 60 ms frame; the count goes into the close log.
            if (droppedPcm.getAndIncrement() == 0L) {
                Log.w(TAG, "PCM send dropped: media connection not open (serial=$debugSerial); further drops only counted")
            }
            return
        }
        // Always forward downlink PCM during OFFHOOK so the agent's tone detector can
        // hear in-band DTMF while it is playing audio (barge-in) and while it waits
        // for a key. VOICE_DOWNLINK should not carry our injected uplink audio.
        c.send(pcm)
    }

    /** Best-effort JSON control frame (ringing / call_end / local_menu_*). Queues if WS not open. */
    fun sendJson(obj: JSONObject) {
        val c = client
        if (c == null || !open.get() || !c.isOpen) {
            pendingJson.offer(obj)
            Log.i(
                TAG,
                "sendJson queued type=${obj.optString("type")} pending=${pendingJson.size} serial=$debugSerial",
            )
            return
        }
        try {
            c.send(obj.toString())
        } catch (e: Exception) {
            pendingJson.offer(obj)
            Log.w(TAG, "sendJson failed (queued): ${e.message}")
        }
    }

    private fun flushPending() {
        val c = client ?: return
        if (!open.get() || !c.isOpen) return
        while (true) {
            val o = pendingJson.poll() ?: break
            try {
                c.send(o.toString())
                Log.i(TAG, "sendJson flushed type=${o.optString("type")}")
            } catch (e: Exception) {
                pendingJson.offer(o)
                Log.w(TAG, "flushPending failed: ${e.message}")
                break
            }
        }
    }

    fun sendRinging(serial: String, callerId: String) {
        sendJson(
            JSONObject()
                .put("type", "ringing")
                .put("serial", serial)
                .put("caller_id", callerId),
        )
    }

    fun sendCallEnd(serial: String, callerId: String) {
        sendJson(
            JSONObject()
                .put("type", "call_end")
                .put("serial", serial)
                .put("caller_id", callerId),
        )
    }

    fun close() {
        closedLocally.set(true)
        open.set(false)
        closeDump()
        try {
            client?.close()
        } catch (_: Exception) {
        }
        client = null
    }

    companion object {
        private const val TAG = "BridgeClient"
        /** Seconds without a pong before the media connection counts as lost. */
        private const val CONNECTION_LOST_TIMEOUT_S = 10
    }
}
