// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.content.Context
import android.util.Log
import org.json.JSONObject
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * JNI bridge to the in-process Rust Bageo agent (`libbageo_agent.so`, standalone flavor only).
 *
 * The agent opens no console port: the app hands it each console request with [http].
 * Its only socket is the phone bridge on loopback ([BuildConfig.BRIDGE_WS_URL]), which
 * takes a connection only with the link check ([PhoneLinkAuth]) on [bridgeKey]; the key
 * itself never goes over the socket ([linkPlan]). Text crosses JNI as UTF-8 bytes.
 *
 * Keeps all calls guarded by [BuildConfig.STANDALONE] so the lightweight `bridge` flavor
 * never attempts to load a native library.
 */
object EmbeddedAgent {
    private const val TAG = "EmbeddedAgent"
    private const val PRIV_APP_SO = "/system/priv-app/com.pixelcallai.streamer/lib/arm64/libbageo_agent.so"
    private const val PRIV_APP_ASSETS_DIR = "/system/priv-app/com.pixelcallai.streamer/bageo_assets"
    private val MODULE_ASSETS_DIRS = listOf(
        "/data/adb/modules/bageo_ai_call_agent/bageo_assets",
        "/data/adb/modules/pixelcallai_uplink/bageo_assets",
    )

    data class Status(
        val running: Boolean,
        val ready: Boolean,
        val activeCalls: Int,
    ) {
        companion object {
            fun empty(): Status = Status(running = false, ready = false, activeCalls = 0)

            fun fromJson(raw: String): Status {
                return try {
                    val obj = JSONObject(raw)
                    Status(
                        running = obj.optBoolean("running", false),
                        ready = obj.optBoolean("ready", false),
                        activeCalls = obj.optInt("active_calls", 0),
                    )
                } catch (_: Exception) {
                    empty()
                }
            }
        }
    }

    /** The agent's answer to one [http] request. Header names are lower case. */
    class HttpResult(
        val status: Int,
        val headers: List<Pair<String, String>>,
        val body: ByteArray,
    ) {
        val ok: Boolean get() = status in 200..299

        fun header(name: String): String? =
            headers.firstOrNull { it.first.equals(name, ignoreCase = true) }?.second
    }

    private val lock = Any()
    @Volatile private var libraryLoaded = false

    private fun ensureLoaded(): Boolean {
        if (!BuildConfig.STANDALONE) return false
        if (libraryLoaded) return true
        return synchronized(lock) {
            if (libraryLoaded) return@synchronized true
            try {
                System.loadLibrary("bageo_agent")
                libraryLoaded = true
                return@synchronized true
            } catch (t: Throwable) {
                Log.w(TAG, "loadLibrary(bageo_agent) failed, trying priv-app path: ${t.message}")
            }
            try {
                val privSo = File(PRIV_APP_SO)
                if (privSo.isFile) {
                    System.load(privSo.absolutePath)
                    libraryLoaded = true
                    true
                } else {
                    Log.e(TAG, "libbageo_agent.so missing at $PRIV_APP_SO")
                    false
                }
            } catch (t: Throwable) {
                Log.e(TAG, "failed to load libbageo_agent.so: ${t.message}")
                false
            }
        }
    }

    /** Device-protected data directory so calls/settings work before lockscreen unlock. */
    fun dataDir(ctx: Context): File {
        val de = ctx.applicationContext.createDeviceProtectedStorageContext()
        return File(de.filesDir, "calls_data").apply { mkdirs() }
    }

    /** Priv-app or module assets directory when present; fallback to app-private directory. */
    fun assetsDir(ctx: Context): File {
        val priv = File(PRIV_APP_ASSETS_DIR)
        if (priv.isDirectory) return priv
        MODULE_ASSETS_DIRS.map(::File).firstOrNull { it.isDirectory }?.let { return it }
        val de = ctx.applicationContext.createDeviceProtectedStorageContext()
        return File(de.filesDir, "assets").apply { mkdirs() }
    }

    /**
     * Starts the agent, or finds it running. An agent that stopped (it could not start,
     * or its phone bridge closed) is started again, so calling this again is the retry.
     */
    fun ensureStarted(ctx: Context): Boolean {
        if (!BuildConfig.STANDALONE) return false
        if (!ensureLoaded()) return false
        return synchronized(lock) {
            val data = dataDir(ctx).absolutePath
            val assets = assetsDir(ctx).absolutePath
            try {
                nativeStart(data.toByteArray(Charsets.UTF_8), assets.toByteArray(Charsets.UTF_8))
            } catch (t: Throwable) {
                Log.e(TAG, "nativeStart failed: ${t.message}")
                false
            }
        }
    }

    fun patchSettings(ctx: Context, patchJson: String): Boolean {
        if (!ensureStarted(ctx)) return false
        return try {
            nativePatchSettings(patchJson.toByteArray(Charsets.UTF_8))
        } catch (t: Throwable) {
            Log.w(TAG, "nativePatchSettings failed: ${t.message}")
            false
        }
    }

    fun status(): Status {
        if (!BuildConfig.STANDALONE || !libraryLoaded) return Status.empty()
        return try {
            Status.fromJson(String(nativeStatusJson() ?: return Status.empty(), Charsets.UTF_8))
        } catch (t: Throwable) {
            Log.w(TAG, "nativeStatusJson failed: ${t.message}")
            Status.empty()
        }
    }

    /**
     * The phone bridge key of the running agent: empty until its bridge listens (so the
     * app never talks to whatever holds the port before that). Changes on each start.
     */
    fun bridgeKey(): String {
        if (!BuildConfig.STANDALONE || !libraryLoaded) return ""
        return try {
            String(nativeBridgeKey() ?: return "", Charsets.UTF_8).trim()
        } catch (t: Throwable) {
            Log.w(TAG, "nativeBridgeKey failed: ${t.message}")
            ""
        }
    }

    /**
     * How a WebSocket to Bageo runs the link check ([PhoneLinkAuth.plan]): standalone with
     * this start's [bridgeKey] (no connection while it is empty), the `bridge` flavor with
     * [storedKey], the key Bageo handed over USB.
     */
    fun linkPlan(storedKey: String?): PhoneLinkAuth.Plan =
        PhoneLinkAuth.plan(
            standalone = BuildConfig.STANDALONE,
            bridgeKey = if (BuildConfig.STANDALONE) bridgeKey() else null,
            storedKey = storedKey,
        )

    /**
     * One console request handled inside the app, with no network: [target] is the path
     * with its query (e.g. `/calls/api/list?limit=20`). Blocks until the agent answers,
     * so call it from a background thread. Null when the agent library is not loaded.
     */
    fun http(
        method: String,
        target: String,
        headers: Map<String, String> = emptyMap(),
        body: ByteArray? = null,
    ): HttpResult? {
        if (!BuildConfig.STANDALONE || !libraryLoaded) return null
        val headerLines = buildString {
            for ((name, value) in headers) {
                val n = name.trim()
                if (n.isEmpty() || n.contains(':') || n.contains('\n')) continue
                append(n).append(": ").append(value.replace('\r', ' ').replace('\n', ' ')).append('\n')
            }
        }
        val raw = try {
            nativeHttp(
                method.toByteArray(Charsets.UTF_8),
                target.toByteArray(Charsets.UTF_8),
                headerLines.toByteArray(Charsets.UTF_8),
                body ?: ByteArray(0),
            )
        } catch (t: Throwable) {
            Log.w(TAG, "nativeHttp failed: ${t.message}")
            null
        } ?: return null
        return decode(raw)
    }

    /** `[head length: 4 bytes big-endian][head: "status\nname: value\n…"][body]`. */
    private fun decode(raw: ByteArray): HttpResult? {
        if (raw.size < 4) return null
        val headLen = ByteBuffer.wrap(raw, 0, 4).order(ByteOrder.BIG_ENDIAN).int
        if (headLen < 0 || headLen > raw.size - 4) return null
        val lines = String(raw, 4, headLen, Charsets.UTF_8).split('\n')
        val status = lines.firstOrNull()?.trim()?.toIntOrNull() ?: return null
        val headers = lines.drop(1).mapNotNull { line ->
            val idx = line.indexOf(':')
            if (idx <= 0) null else line.substring(0, idx).trim().lowercase() to line.substring(idx + 1).trim()
        }
        return HttpResult(status, headers, raw.copyOfRange(4 + headLen, raw.size))
    }

    @JvmStatic
    private external fun nativeStart(dataDir: ByteArray, assetsDir: ByteArray): Boolean

    @JvmStatic
    private external fun nativePatchSettings(patchJson: ByteArray): Boolean

    @JvmStatic
    private external fun nativeStatusJson(): ByteArray?

    @JvmStatic
    private external fun nativeBridgeKey(): ByteArray?

    @JvmStatic
    private external fun nativeHttp(
        method: ByteArray,
        target: ByteArray,
        headers: ByteArray,
        body: ByteArray,
    ): ByteArray?
}
