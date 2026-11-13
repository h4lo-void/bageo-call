package com.pixelcallai.streamer

import android.util.Log
import java.util.concurrent.TimeUnit

/** `su -c` with a timeout, returning the exit code and the output. Never call it on the main thread. */
object RootShell {
    class Out(val exit: Int, val text: String) {
        val ok: Boolean get() = exit == 0
    }

    fun run(cmd: String, timeoutSec: Long = DEFAULT_TIMEOUT_SEC): Out {
        return try {
            val p = ProcessBuilder("su", "-c", cmd).redirectErrorStream(true).start()
            try {
                p.outputStream.close()
            } catch (_: Exception) {
            }
            // Outputs here are a few lines: they fit in the pipe, so waiting first is safe.
            if (!p.waitFor(timeoutSec, TimeUnit.SECONDS)) {
                p.destroyForcibly()
                Log.w(TAG, "timeout after ${timeoutSec}s: $cmd")
                return Out(-1, "")
            }
            val text = try {
                p.inputStream.bufferedReader().use { it.readText() }
            } catch (_: Exception) {
                ""
            }
            val exit = p.exitValue()
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

    private const val TAG = "RootShell"
    private const val DEFAULT_TIMEOUT_SEC = 8L
    private const val FIRST_TIMEOUT_SEC = 20L
}
