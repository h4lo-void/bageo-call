package com.pixelcallai.streamer

import android.util.Log
import org.java_websocket.client.WebSocketClient
import org.java_websocket.handshake.ServerHandshake
import org.json.JSONObject
import java.net.URI
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/**
 * Idle control WebSocket — separate from the in-call [BridgeClient] PCM relay.
 *
 * Connects to the agent URL ([Prefs.agentUrl]) while [CallAudioService] is armed,
 * so the agent can send text JSON to the phone even when no call is active.
 * In-call audio uses a second WebSocket via [BridgeClient] on OFFHOOK.
 *
 * Handles agent → phone commands such as `{"type":"place_call",...}` and the
 * `answer_plan` reply to a ringing call's `incoming`, `set_mode` (AI phone / normal
 * phone) and `agent_info` (the web console's address).
 * Sends a periodic `ping` so the idle registration stays alive; reconnects quickly on drop.
 *
 * [describe] adds the phone's current state to the `hello` (`phone_mode`, `in_call`).
 * The phone mode is not called `mode`: that field says which connection this is.
 */
class AgentControlClient(
    private val url: String,
    private val onCommand: (JSONObject) -> Unit,
    private val describe: (JSONObject) -> Unit = {},
) {
    private var client: WebSocketClient? = null
    private val open = AtomicBoolean(false)
    private val shouldRun = AtomicBoolean(false)
    private val lastPingAt = AtomicLong(0L)
    @Volatile private var serialHeld: String = ""
    private var pingThread: Thread? = null

    fun isOpen(): Boolean = open.get() && client?.isOpen == true

    fun connect(serial: String) {
        shouldRun.set(true)
        serialHeld = serial
        if (client != null) return
        val ws = object : WebSocketClient(URI(url)) {
            override fun onOpen(handshakedata: ServerHandshake?) {
                open.set(true)
                current = this@AgentControlClient
                val hello = JSONObject()
                    .put("type", "hello")
                    .put("serial", serial)
                    .put("mode", "CONTROL_IDLE")
                    .put("play_tts_locally", false)
                    .put("model", android.os.Build.MODEL)
                    .put("app_version", BuildConfig.VERSION_NAME)
                try {
                    describe(hello)
                } catch (e: Exception) {
                    Log.w(TAG, "hello state failed: ${e.message}")
                }
                send(hello.toString())
                Log.i(TAG, "control WS open ${Prefs.redactUrl(url)} serial=$serial mode=CONTROL_IDLE")
                ensurePingLoop()
            }

            override fun onMessage(message: String?) {
                if (message == null) return
                try {
                    val obj = JSONObject(message)
                    when (obj.optString("type")) {
                        "hello_ack", "hello_ok", "ack", "pong" -> {
                            Log.i(TAG, "control TEXT ${obj.optString("type")}")
                        }
                        "place_call", "blocklist_sync", "answer_plan", "set_mode", "agent_info" -> onCommand(obj)
                        else -> Log.i(TAG, "control TEXT type=${obj.optString("type")} len=${message.length}")
                    }
                } catch (e: Exception) {
                    Log.w(TAG, "control TEXT parse failed: ${e.message}")
                }
            }

            override fun onClose(code: Int, reason: String?, remote: Boolean) {
                open.set(false)
                if (current === this@AgentControlClient) current = null
                client = null
                Log.w(TAG, "control WS closed code=$code reason=$reason remote=$remote")
                scheduleReconnect(serial)
            }

            override fun onError(ex: Exception?) {
                Log.e(TAG, "control WS error", ex)
                // Drop socket only — keep shouldRun so onClose → reconnect.
                try {
                    client?.close()
                } catch (_: Exception) {
                }
            }
        }
        client = ws
        try {
            ws.connectionLostTimeout = 30
        } catch (_: Exception) {
        }
        ws.connect()
    }

    fun sendJson(obj: JSONObject) {
        val c = client ?: return
        if (!open.get() || !c.isOpen) return
        try {
            c.send(obj.toString())
        } catch (e: Exception) {
            Log.w(TAG, "control sendJson failed: ${e.message}")
        }
    }

    fun close() {
        shouldRun.set(false)
        open.set(false)
        if (current === this) current = null
        try {
            client?.close()
        } catch (_: Exception) {
        }
        client = null
    }

    private fun ensurePingLoop() {
        if (pingThread?.isAlive == true) return
        pingThread = Thread {
            while (shouldRun.get()) {
                try {
                    Thread.sleep(PING_INTERVAL_MS)
                } catch (_: InterruptedException) {
                    break
                }
                if (!shouldRun.get()) break
                val c = client
                if (c == null || !open.get() || !c.isOpen) {
                    if (shouldRun.get() && client == null && serialHeld.isNotEmpty()) {
                        Log.i(TAG, "control WS ping-loop saw null client — reconnect")
                        scheduleReconnect(serialHeld)
                    }
                    continue
                }
                try {
                    val now = System.currentTimeMillis()
                    lastPingAt.set(now)
                    c.send(JSONObject().put("type", "ping").put("t", now).toString())
                } catch (e: Exception) {
                    Log.w(TAG, "control ping failed: ${e.message}")
                    try {
                        c.close()
                    } catch (_: Exception) {
                    }
                }
            }
        }.also {
            it.isDaemon = true
            it.name = "AgentControlPing"
            it.start()
        }
    }

    private fun scheduleReconnect(serial: String) {
        if (!shouldRun.get()) return
        Thread {
            try {
                Thread.sleep(RECONNECT_MS)
            } catch (_: InterruptedException) {
                return@Thread
            }
            if (shouldRun.get() && client == null) {
                Log.i(TAG, "control WS reconnecting…")
                connect(serial)
            }
        }.start()
    }

    companion object {
        private const val TAG = "AgentControlClient"
        /** Keep the agent's CONTROL_IDLE registration warm. */
        private const val PING_INTERVAL_MS = 15_000L
        /** Faster than 3s — idle drops show up as missing callbacks. */
        private const val RECONNECT_MS = 1_500L

        /** The client whose connection is open (one per running service). */
        @Volatile
        private var current: AgentControlClient? = null

        /** True while the service's control connection to the agent is open (setup screen). */
        fun isAgentConnected(): Boolean = current?.isOpen() == true
    }
}
