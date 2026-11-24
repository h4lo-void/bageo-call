// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.content.Context
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.util.Log

/**
 * Keeps CPU and low-latency Wi-Fi awake across incoming/outbound calls and for a
 * short cool-down after hangup so the embedded Gemini Live / summary / notification
 * pipeline never stalls under Doze.
 */
class CallWakeController internal constructor(
    private val locks: WakeLocks,
    private val scheduler: DelayScheduler,
    private val idleDelayMs: Long = IDLE_RELEASE_DELAY_MS,
    private val lockTimeoutMs: Long = MAX_HOLD_MS,
    private val log: (String) -> Unit = { msg -> Log.i(TAG, msg) },
) {
    interface WakeLocks {
        fun acquire(timeoutMs: Long)
        fun release()
    }

    interface DelayScheduler {
        fun postDelayed(delayMs: Long, task: Runnable): Any
        fun cancel(token: Any)
    }

    private val stateLock = Any()
    private var held = false
    private var pendingReleaseToken: Any? = null

    fun isHeld(): Boolean = synchronized(stateLock) { held }

    fun onCallActive(reason: String) {
        var acquired = false
        synchronized(stateLock) {
            pendingReleaseToken?.let {
                scheduler.cancel(it)
                pendingReleaseToken = null
            }
            if (!held) {
                locks.acquire(lockTimeoutMs)
                held = true
                acquired = true
            }
        }
        if (acquired) {
            log("call wake + wifi lock acquired reason=$reason")
        }
    }

    fun onCallIdle() {
        synchronized(stateLock) {
            if (!held) return
            pendingReleaseToken?.let { scheduler.cancel(it) }
            lateinit var token: Any
            val task = Runnable { releaseOnTimeout(token) }
            token = scheduler.postDelayed(idleDelayMs, task)
            pendingReleaseToken = token
        }
    }

    private fun releaseOnTimeout(expectedToken: Any) {
        var released = false
        synchronized(stateLock) {
            if (pendingReleaseToken !== expectedToken) return
            pendingReleaseToken = null
            if (held) {
                locks.release()
                held = false
                released = true
            }
        }
        if (released) {
            log("call wake + wifi lock released after idle grace=${idleDelayMs}ms")
        }
    }

    fun releaseNow() {
        var released = false
        synchronized(stateLock) {
            pendingReleaseToken?.let {
                scheduler.cancel(it)
                pendingReleaseToken = null
            }
            if (held) {
                locks.release()
                held = false
                released = true
            }
        }
        if (released) {
            log("call wake + wifi lock released immediately")
        }
    }

    companion object {
        private const val TAG = "CallWakeController"
        const val IDLE_RELEASE_DELAY_MS = 45_000L
        const val MAX_HOLD_MS = 40 * 60 * 1000L

        fun create(context: Context): CallWakeController {
            val app = context.applicationContext
            val pm = app.getSystemService(Context.POWER_SERVICE) as? PowerManager
            val wm = app.getSystemService(Context.WIFI_SERVICE) as? WifiManager
            val cpuLock = pm?.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "bageo:call")?.apply {
                setReferenceCounted(false)
            }
            @Suppress("DEPRECATION")
            val wifiMode = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                WifiManager.WIFI_MODE_FULL_LOW_LATENCY
            } else {
                WifiManager.WIFI_MODE_FULL_HIGH_PERF
            }
            val wifiLock = wm?.createWifiLock(wifiMode, "bageo:call_wifi")?.apply {
                setReferenceCounted(false)
            }
            val handler = Handler(Looper.getMainLooper())

            val locks = object : WakeLocks {
                override fun acquire(timeoutMs: Long) {
                    try {
                        cpuLock?.acquire(timeoutMs)
                    } catch (e: Exception) {
                        Log.w(TAG, "cpu wakelock acquire failed: ${e.message}")
                    }
                    try {
                        if (wifiLock?.isHeld == false) wifiLock.acquire()
                    } catch (e: Exception) {
                        Log.w(TAG, "wifi lock acquire failed: ${e.message}")
                    }
                }

                override fun release() {
                    try {
                        if (cpuLock?.isHeld == true) cpuLock.release()
                    } catch (e: Exception) {
                        Log.w(TAG, "cpu wakelock release failed: ${e.message}")
                    }
                    try {
                        if (wifiLock?.isHeld == true) wifiLock.release()
                    } catch (e: Exception) {
                        Log.w(TAG, "wifi lock release failed: ${e.message}")
                    }
                }
            }

            val scheduler = object : DelayScheduler {
                override fun postDelayed(delayMs: Long, task: Runnable): Any {
                    handler.postDelayed(task, delayMs)
                    return task
                }

                override fun cancel(token: Any) {
                    (token as? Runnable)?.let { handler.removeCallbacks(it) }
                }
            }
            return CallWakeController(locks, scheduler)
        }
    }
}
