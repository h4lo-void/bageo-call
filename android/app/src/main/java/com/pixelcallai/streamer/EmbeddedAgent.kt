// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.content.Context
import android.util.Log
import org.json.JSONObject
import java.io.File

/**
 * JNI bridge to the in-process Rust Bageo agent (`libbageo_agent.so`, standalone flavor only).
 *
 * Keeps all calls guarded by [BuildConfig.STANDALONE] so the lightweight `bridge` flavor
 * never attempts to load a native library.
 */
object EmbeddedAgent {
    private const val TAG = "EmbeddedAgent"
    private const val PRIV_APP_SO = "/system/priv-app/com.pixelcallai.streamer/lib/arm64/libbageo_agent.so"
    private const val PRIV_APP_ASSETS_DIR = "/system/priv-app/com.pixelcallai.streamer/bageo_assets"
    private const val MODULE_ASSETS_DIR = "/data/adb/modules/pixelcallai_uplink/bageo_assets"

    data class Status(
        val running: Boolean,
        val ready: Boolean,
        val lanEnabled: Boolean,
        val lanRequested: Boolean,
        val passwordSet: Boolean,
        val uiToken: String,
        val port: Int,
        val lanIp: String,
        val lanUrl: String,
        val mdnsUrl: String,
        val activeCalls: Int,
    ) {
        companion object {
            fun empty(port: Int = BuildConfig.CONSOLE_PORT): Status = Status(
                running = false,
                ready = false,
                lanEnabled = false,
                lanRequested = false,
                passwordSet = false,
                uiToken = "",
                port = port,
                lanIp = "",
                lanUrl = "",
                mdnsUrl = "",
                activeCalls = 0,
            )

            fun fromJson(raw: String): Status {
                return try {
                    val obj = JSONObject(raw)
                    Status(
                        running = obj.optBoolean("running", false),
                        ready = obj.optBoolean("ready", false),
                        lanEnabled = obj.optBoolean("lan_enabled", false),
                        lanRequested = obj.optBoolean("lan_requested", false),
                        passwordSet = obj.optBoolean("password_set", false),
                        uiToken = obj.optString("ui_token", ""),
                        port = obj.optInt("port", BuildConfig.CONSOLE_PORT),
                        lanIp = obj.optString("lan_ip", ""),
                        lanUrl = obj.optString("lan_url", ""),
                        mdnsUrl = obj.optString("mdns_url", ""),
                        activeCalls = obj.optInt("active_calls", 0),
                    )
                } catch (_: Exception) {
                    empty()
                }
            }
        }
    }

    private val lock = Any()
    @Volatile private var libraryLoaded = false
    @Volatile private var started = false

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
        val mod = File(MODULE_ASSETS_DIR)
        if (mod.isDirectory) return mod
        val de = ctx.applicationContext.createDeviceProtectedStorageContext()
        return File(de.filesDir, "assets").apply { mkdirs() }
    }

    fun ensureStarted(ctx: Context): Boolean {
        if (!BuildConfig.STANDALONE) return false
        if (started) return true
        if (!ensureLoaded()) return false
        return synchronized(lock) {
            if (started) return@synchronized true
            val data = dataDir(ctx).absolutePath
            val assets = assetsDir(ctx).absolutePath
            val lan = Prefs.isStandaloneLanEnabled(ctx)
            val ok = try {
                nativeStart(data, assets, BuildConfig.CONSOLE_PORT, lan)
            } catch (t: Throwable) {
                Log.e(TAG, "nativeStart failed: ${t.message}")
                false
            }
            if (ok) {
                started = true
                Log.i(TAG, "embedded agent started port=${BuildConfig.CONSOLE_PORT} lan=$lan")
            }
            ok
        }
    }

    fun setLanEnabled(ctx: Context, enabled: Boolean): Boolean {
        if (!ensureStarted(ctx)) return false
        Prefs.setStandaloneLanEnabled(ctx, enabled)
        return try {
            nativeSetLanEnabled(enabled)
        } catch (t: Throwable) {
            Log.w(TAG, "nativeSetLanEnabled failed: ${t.message}")
            false
        }
    }

    fun setConsolePassword(password: String): Boolean {
        if (!BuildConfig.STANDALONE || !ensureLoaded()) return false
        return try {
            nativeSetUiPassword(password)
        } catch (t: Throwable) {
            Log.w(TAG, "nativeSetUiPassword failed: ${t.message}")
            false
        }
    }

    fun patchSettings(ctx: Context, patchJson: String): Boolean {
        if (!ensureStarted(ctx)) return false
        return try {
            nativePatchSettings(patchJson)
        } catch (t: Throwable) {
            Log.w(TAG, "nativePatchSettings failed: ${t.message}")
            false
        }
    }

    fun status(): Status {
        if (!BuildConfig.STANDALONE || !libraryLoaded) return Status.empty()
        return try {
            Status.fromJson(nativeStatusJson())
        } catch (t: Throwable) {
            Log.w(TAG, "nativeStatusJson failed: ${t.message}")
            Status.empty()
        }
    }

    fun localConsoleUrl(token: String = ""): String {
        val base = "http://127.0.0.1:${BuildConfig.CONSOLE_PORT}/calls/?embed=android"
        return if (token.isNotBlank()) {
            "$base&token=${android.net.Uri.encode(token)}"
        } else {
            base
        }
    }

    fun localMobileUrl(token: String = "", lang: String = ""): String {
        val params = mutableListOf<String>()
        if (token.isNotBlank()) params.add("token=${android.net.Uri.encode(token)}")
        if (lang.isNotBlank()) params.add("lang=${android.net.Uri.encode(lang)}")
        val qs = if (params.isEmpty()) "" else "?" + params.joinToString("&")
        return "http://127.0.0.1:${BuildConfig.CONSOLE_PORT}/calls/mobile$qs"
    }

    @JvmStatic
    private external fun nativeStart(
        dataDir: String,
        assetsDir: String,
        port: Int,
        lanEnabled: Boolean,
    ): Boolean

    @JvmStatic
    private external fun nativeSetLanEnabled(enabled: Boolean): Boolean

    @JvmStatic
    private external fun nativeSetUiPassword(password: String): Boolean

    @JvmStatic
    private external fun nativePatchSettings(patchJson: String): Boolean

    @JvmStatic
    private external fun nativeStatusJson(): String
}
