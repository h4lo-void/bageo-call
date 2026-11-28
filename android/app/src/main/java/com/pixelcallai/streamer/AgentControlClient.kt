// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.util.Log
import org.java_websocket.client.WebSocketClient
import org.java_websocket.handshake.ServerHandshake
import org.json.JSONObject
import org.json.JSONArray
import org.java_websocket.drafts.Draft_6455
import java.net.InetSocketAddress
import java.net.Socket
import java.net.URI
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/**
 * Idle control WebSocket — separate from the in-call [BridgeClient] PCM relay.
 *
 * Connects to the agent while [CallAudioService] is armed, so the agent can send text
 * JSON to the phone even when no call is active. The address comes from [candidates]
 * (USB first, then the Wi-Fi addresses the agent sent; a hand-typed address stands
 * alone): a failed attempt moves on to the next one, and the address that worked is
 * remembered ([activeUrl]) so the in-call connection uses the same one. While on Wi-Fi
 * and not in a call ([isBusy] false), the client checks every [USB_CHECK_MS] whether
 * the USB link is back and then moves back to it. It never switches during a call.
 * In-call audio uses a second WebSocket via [BridgeClient] on OFFHOOK.
 *
 * Handles agent → phone commands such as `{"type":"place_call",...}` and the
 * `answer_plan` reply to a ringing call's `incoming`, `set_mode` (AI phone / normal
 * phone), `agent_info` (the web console's address), `uplink_fit` (the owner says the
 * caller did not hear the AI; the hello carries `audio_fit`) and `send_sms` (the hello says
 * `supports_sms`). The hello's `supports_reject` tells the agent that this app declines a
 * ringing call when `answer_plan` says `reject` (older apps would answer it).
 * Sends a periodic `ping` so the idle registration stays alive; reconnects quickly on drop.
 *
 * [describe] adds the phone's current state to the `hello` (`phone_mode`, `in_call`).
 * The phone mode is not called `mode`: that field says which connection this is.
 * [sims] gives the phone's SIMs for the hello's `sims` list (null = leave it out), so the
 * agent knows every SIM at once, not only the ones a call came in on.
 * [onOpened] runs right after the hello was sent (on the socket's thread).
 *
 * Each connection runs the link check ([PhoneLinkAuth], key from [linkKey] or the in-app
 * agent): when Bageo does not prove itself, the socket is closed before the hello (the
 * phone's serial and SIMs never go out) and the attempt counts as failed. `agent_info`
 * is only taken from a connection Bageo proved itself on ([isVerified]); on others its
 * addresses are dropped.
 */
class AgentControlClient(
    private val candidates: () -> List<String>,
    private val onCommand: (JSONObject) -> Unit,
    private val isBusy: () -> Boolean = { false },
    private val usbUrl: String = BuildConfig.BRIDGE_WS_URL,
    private val describe: (JSONObject) -> Unit = {},
    private val sims: () -> JSONArray? = { null },
    private val onOpened: () -> Unit = {},
    private val linkKey: () -> String? = { null },
) {
    @Volatile private var client: WebSocketClient? = null
    private val stateLock = Any()
    private val reconnecting = AtomicBoolean(false)
    private val open = AtomicBoolean(false)
    private val shouldRun = AtomicBoolean(false)
    private val lastPingAt = AtomicLong(0L)
    @Volatile private var serialHeld: String = ""
    private var pingThread: Thread? = null
    private var usbThread: Thread? = null
    /** The address the next attempt uses (null = the first candidate). */
    @Volatile private var target: String? = null
    /** The address of the current or latest attempt. */
    @Volatile private var currentUrl: String = ""
    /** The open connection is one Bageo proved itself on ([PhoneLinkAuth.verified]). */
    @Volatile private var verified = false

    fun isOpen(): Boolean = open.get() && client?.isOpen == true

    /** Connected, and Bageo proved itself on this connection. */
    fun isVerified(): Boolean = isOpen() && verified

    fun connect(serial: String) {
        val ws: WebSocketClient
        synchronized(stateLock) {
            shouldRun.set(true)
            serialHeld = serial
            if (client != null) return
            val list = candidates().ifEmpty { listOf(usbUrl) }
            val url = target?.takeIf { it in list } ?: list.first()
            currentUrl = url
            val openedHere = AtomicBoolean(false)
            val session = when (val plan = EmbeddedAgent.linkPlan(linkKey())) {
                is PhoneLinkAuth.Plan.Check -> plan.session
                PhoneLinkAuth.Plan.Legacy -> null
                PhoneLinkAuth.Plan.Refuse -> {
                    // Standalone before its agent's bridge listens: nothing to connect to yet.
                    Log.i(TAG, "control WS waits: no link key yet")
                    scheduleReconnect(serial)
                    return
                }
            }
            ws = object : WebSocketClient(URI(url), Draft_6455(), session?.headers(), CONNECT_TIMEOUT_MS) {
                override fun onOpen(handshakedata: ServerHandshake?) {
                    val checked = session != null && session.verifyHost(
                        handshakedata?.getFieldValue(PhoneLinkAuth.HEADER_SNONCE),
                        handshakedata?.getFieldValue(PhoneLinkAuth.HEADER_PROOF),
                    )
                    if (session != null && !checked) {
                        // Not the Bageo that holds the key: say nothing, try the next address.
                        Log.w(TAG, "control WS ${Prefs.redactUrl(url)}: the link check failed; closing before hello")
                        try {
                            this.close()
                        } catch (_: Exception) {
                        }
                        return
                    }
                    synchronized(stateLock) {
                        if (!shouldRun.get() || client !== this) {
                            try {
                                this.close()
                            } catch (_: Exception) {
                            }
                            return
                        }
                        open.set(true)
                        openedHere.set(true)
                        verified = PhoneLinkAuth.verified(checked, url)
                        target = url
                        lastGoodUrl = url
                        current = this@AgentControlClient
                    }
                    val hello = JSONObject()
                        .put("type", "hello")
                        .put("serial", serial)
                        .put("mode", "CONTROL_IDLE")
                        .put("play_tts_locally", false)
                        .put("model", DeviceName.display)
                        .put("app_version", BuildConfig.VERSION_NAME)
                        .put("supports_sms", true)
                        .put("supports_reject", true)
                    session?.helloProof()?.let { hello.put("link_proof", it) }
                    try {
                        describe(hello)
                    } catch (e: Exception) {
                        Log.w(TAG, "hello state failed: ${e.message}")
                    }
                    try {
                        sims()?.let { hello.put("sims", it) }
                    } catch (e: Exception) {
                        Log.w(TAG, "hello sims failed: ${e.message}")
                    }
                    send(hello.toString())
                    Log.i(TAG, "control WS open ${Prefs.redactUrl(url)} usb=${url == usbUrl} checked=$verified serial=$serial mode=CONTROL_IDLE")
                    try {
                        onOpened()
                    } catch (e: Exception) {
                        Log.w(TAG, "after-hello step failed: ${e.message}")
                    }
                    ensurePingLoop()
                    if (!BuildConfig.STANDALONE) {
                        ensureUsbWatch()
                    }
                }

                override fun onMessage(message: String?) {
                    if (message == null) return
                    try {
                        val obj = JSONObject(message)
                        when (obj.optString("type")) {
                            "hello_ack", "hello_ok", "ack", "pong" -> {
                                Log.i(TAG, "control TEXT ${obj.optString("type")}")
                            }
                            "agent_info" -> {
                                if (!verified) {
                                    // Unproven Bageo: its version only, never where to connect or browse.
                                    obj.remove("wifi")
                                    obj.remove("console_url")
                                    Log.i(TAG, "control agent_info on an unchecked connection: addresses ignored")
                                }
                                onCommand(obj)
                            }
                            "place_call", "blocklist_sync", "answer_plan", "set_mode", "send_sms", "local_notice", "uplink_fit" -> onCommand(obj)
                            else -> Log.i(TAG, "control TEXT type=${obj.optString("type")} len=${message.length}")
                        }
                    } catch (e: Exception) {
                        Log.w(TAG, "control TEXT parse failed: ${e.message}")
                    }
                }

                override fun onClose(code: Int, reason: String?, remote: Boolean) {
                    var cleared = false
                    synchronized(stateLock) {
                        if (client === this) {
                            client = null
                            open.set(false)
                            verified = false
                            if (current === this@AgentControlClient) current = null
                            cleared = true
                        }
                    }
                    if (!cleared) return
                    Log.w(TAG, "control WS closed code=$code reason=$reason remote=$remote")
                    if (!openedHere.get()) {
                        // Never got through: the next try uses the next address.
                        target = ConnectionPlan.nextAfterFailure(candidates().ifEmpty { listOf(usbUrl) }, url)
                    }
                    scheduleReconnect(serial)
                }

                override fun onError(ex: Exception?) {
                    Log.e(TAG, "control WS error", ex)
                    // Drop socket only — keep shouldRun so onClose → reconnect.
                    try {
                        this.close()
                    } catch (_: Exception) {
                    }
                }

                override fun onSetSSLParameters(sslParameters: javax.net.ssl.SSLParameters) {
                    TlsPinning.configureSslParameters(sslParameters, url)
                }
            }
            client = ws
        }
        try {
            ws.connectionLostTimeout = 30
        } catch (_: Exception) {
        }
        TlsPinning.attachIfPinned(ws, ws.uri.toString())
        ws.connect()
    }

    fun sendJson(obj: JSONObject) {
        sendEvent(obj)
    }

    fun sendEvent(obj: JSONObject): Boolean {
        val c = client ?: return false
        if (!open.get() || !c.isOpen) return false
        return try {
            c.send(obj.toString())
            true
        } catch (e: Exception) {
            Log.w(TAG, "control sendJson failed: ${e.message}")
            false
        }
    }

    fun close() {
        val toClose: WebSocketClient?
        synchronized(stateLock) {
            shouldRun.set(false)
            open.set(false)
            verified = false
            if (current === this) current = null
            toClose = client
            client = null
            pingThread?.interrupt()
            pingThread = null
            usbThread?.interrupt()
            usbThread = null
        }
        try {
            toClose?.close()
        } catch (_: Exception) {
        }
    }

    private fun ensurePingLoop() {
        if (pingThread?.isAlive == true) return
        pingThread = Thread {
            while (shouldRun.get()) {
                try {
                    Thread.sleep(if (BuildConfig.STANDALONE) STANDALONE_PING_INTERVAL_MS else PING_INTERVAL_MS)
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
        if (!shouldRun.get() || !reconnecting.compareAndSet(false, true)) return
        val t = Thread {
            val retry = try {
                Thread.sleep(RECONNECT_MS)
                shouldRun.get() && client == null
            } catch (_: InterruptedException) {
                false
            } finally {
                // Release before connect(): connect() is serialized by stateLock, and an
                // immediate async failure on ws.connect() may invoke onClose right away.
                reconnecting.set(false)
            }
            if (retry) {
                Log.i(TAG, "control WS reconnecting…")
                connect(serial)
            }
        }
        t.isDaemon = true
        t.name = "AgentControlReconnect"
        try {
            t.start()
        } catch (e: Throwable) {
            reconnecting.set(false)
            throw e
        }
    }

    /** While on Wi-Fi and idle, look for the USB link again and move back to it. */
    private fun ensureUsbWatch() {
        if (usbThread?.isAlive == true) return
        usbThread = Thread {
            while (shouldRun.get()) {
                try {
                    Thread.sleep(USB_CHECK_MS)
                } catch (_: InterruptedException) {
                    break
                }
                if (!shouldRun.get()) break
                try {
                    val c = client
                    if (c == null || !open.get() || !c.isOpen) continue
                    val list = candidates()
                    if (currentUrl == usbUrl || usbUrl !in list || currentUrl !in list) continue
                    if (isBusy() || !usbProbe()) continue
                    // Look again right before the switch: a call may have just come in.
                    if (isBusy()) continue
                    Log.i(TAG, "USB link is back: leaving ${Prefs.redactUrl(currentUrl)} for USB")
                    target = usbUrl
                    c.close()
                } catch (e: Exception) {
                    Log.w(TAG, "USB check failed: ${e.message}")
                }
            }
        }.also {
            it.isDaemon = true
            it.name = "AgentControlUsbWatch"
            it.start()
        }
    }

    /**
     * The link key changed: drop the open connection, so the next one (USB first) runs
     * the check with the new key.
     */
    fun reconnectNow() {
        val c = synchronized(stateLock) { if (shouldRun.get()) client else null } ?: return
        target = null
        try {
            c.close()
        } catch (_: Exception) {
        }
    }

    /**
     * True when the USB address answers as the Bageo that holds the link key: the check
     * only, no hello, so nothing about the phone goes out; at most [USB_LINK_PROBE_MS].
     * Without a key (this Bageo never handed one) an open port counts, as before.
     */
    private fun usbProbe(): Boolean {
        val session = (EmbeddedAgent.linkPlan(linkKey()) as? PhoneLinkAuth.Plan.Check)?.session
            ?: return usbPortOpen()
        val done = CountDownLatch(1)
        val ok = AtomicBoolean(false)
        val probe = object : WebSocketClient(URI(usbUrl), Draft_6455(), session.headers(), USB_LINK_PROBE_MS) {
            override fun onOpen(handshakedata: ServerHandshake?) {
                ok.set(
                    session.verifyHost(
                        handshakedata?.getFieldValue(PhoneLinkAuth.HEADER_SNONCE),
                        handshakedata?.getFieldValue(PhoneLinkAuth.HEADER_PROOF),
                    ),
                )
                done.countDown()
                try {
                    close()
                } catch (_: Exception) {
                }
            }

            override fun onMessage(message: String?) {}

            override fun onClose(code: Int, reason: String?, remote: Boolean) {
                done.countDown()
            }

            override fun onError(ex: Exception?) {
                done.countDown()
            }
        }
        return try {
            probe.connect()
            done.await(USB_LINK_PROBE_MS.toLong(), TimeUnit.MILLISECONDS) && ok.get()
        } catch (_: Exception) {
            false
        } finally {
            try {
                probe.close()
            } catch (_: Exception) {
            }
        }
    }

    /** True when the phone's USB port (adb reverse) accepts a connection. Sends nothing. */
    private fun usbPortOpen(): Boolean {
        val hp = ConnectionPlan.hostPort(usbUrl) ?: return false
        return try {
            Socket().use { it.connect(InetSocketAddress(hp.first, hp.second), USB_PROBE_TIMEOUT_MS) }
            true
        } catch (_: Exception) {
            false
        }
    }

    companion object {
        private const val TAG = "AgentControlClient"
        /** Keep the agent's CONTROL_IDLE registration warm. */
        private const val PING_INTERVAL_MS = 15_000L
        /** Loopback standalone control needs infrequent keepalive to stay zero-drain while idle. */
        private const val STANDALONE_PING_INTERVAL_MS = 300_000L
        /** Faster than 3s — idle drops show up as missing callbacks. */
        private const val RECONNECT_MS = 1_500L
        /** A silent address (Wi-Fi off, wrong network) must not hold up the next one for long. */
        private const val CONNECT_TIMEOUT_MS = 5_000
        /** How often to look for the USB link while on Wi-Fi. */
        private const val USB_CHECK_MS = 30_000L
        private const val USB_PROBE_TIMEOUT_MS = 400
        /** The link-checked look at the USB address waits at most this long. */
        private const val USB_LINK_PROBE_MS = 3_000

        /** The client whose connection is open (one per running service). */
        @Volatile
        private var current: AgentControlClient? = null

        /** The address that last got through; the in-call connection uses it too. */
        @Volatile
        private var lastGoodUrl: String? = null

        /** True while the service's control connection to the agent is open (setup screen). */
        fun isAgentConnected(): Boolean = current?.isOpen() == true

        /** Connected, and Bageo proved itself on that connection (false: "unverified"). */
        fun isAgentVerified(): Boolean = current?.isVerified() == true

        /** The address the open control connection uses, or null when not connected. */
        fun connectedUrl(): String? = current?.takeIf { it.isOpen() }?.currentUrl

        /** The address for a new connection (the in-call media one): [candidates] decides. */
        fun activeUrl(candidates: List<String>): String =
            ConnectionPlan.pickActive(lastGoodUrl, candidates) ?: BuildConfig.BRIDGE_WS_URL
    }
}
