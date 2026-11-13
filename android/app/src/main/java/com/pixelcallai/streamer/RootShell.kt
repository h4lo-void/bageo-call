package com.pixelcallai.streamer

import android.util.Log
import java.util.concurrent.TimeUnit

/**
 * `su -c` with a time limit, returning the exit code and the output. Never call it on
 * the main thread. Every command runs under `toybox timeout`, so a command that hangs
 * is also stopped on the root side, not only abandoned here.
 */
object RootShell {
    class Out(val exit: Int, val text: String) {
        val ok: Boolean get() = exit == 0
    }

    fun run(cmd: String, timeoutSec: Long = DEFAULT_TIMEOUT_SEC): Out {
        val wrapped = "toybox timeout -k $KILL_AFTER_SEC $timeoutSec sh -c ${quote(cmd)}"
        return try {
            val p = ProcessBuilder("su", "-c", wrapped).redirectErrorStream(true).start()
            try {
                p.outputStream.close()
            } catch (_: Exception) {
            }
            val buf = StringBuilder()
            val reader = Thread {
                try {
                    val text = p.inputStream.bufferedReader().use { it.readText() }
                    synchronized(buf) { buf.append(text) }
                } catch (_: Exception) {
                }
            }.apply {
                isDaemon = true
                start()
            }
            // The root side gives up after timeoutSec (+ the kill grace); wait a bit longer.
            if (!p.waitFor(timeoutSec + KILL_AFTER_SEC + LOCAL_GRACE_SEC, TimeUnit.SECONDS)) {
                p.destroyForcibly()
                Log.w(TAG, "timeout after ${timeoutSec}s: $cmd")
                return Out(-1, "")
            }
            reader.join(READER_JOIN_MS)
            val exit = p.exitValue()
            val text = synchronized(buf) { buf.toString() }
            Log.i(TAG, "exit=$exit: $cmd")
            Out(exit, text)
        } catch (e: Exception) {
            Log.w(TAG, "failed ($cmd): ${e.message}")
            Out(-1, "")
        }
    }

    /** True when `su` works and runs as root. The root manager may ask the first time. */
    fun available(): Boolean {
        val out = run("id -u", FIRST_TIMEOUT_SEC)
        return out.ok && out.text.trim() == "0"
    }

    /** One shell word: single quotes, with any single quote inside escaped. */
    fun quote(s: String): String = "'" + s.replace("'", "'\\''") + "'"

    private const val TAG = "RootShell"
    private const val DEFAULT_TIMEOUT_SEC = 8L
    private const val FIRST_TIMEOUT_SEC = 20L
    private const val KILL_AFTER_SEC = 2L
    private const val LOCAL_GRACE_SEC = 3L
    private const val READER_JOIN_MS = 1_000L
}
