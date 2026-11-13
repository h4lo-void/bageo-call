package com.pixelcallai.streamer

import android.content.Context
import android.media.AudioManager
import android.os.SystemClock
import android.telephony.TelephonyManager
import android.util.Log
import java.io.File
import java.util.UUID
import java.util.concurrent.CopyOnWriteArraySet
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean

/**
 * The phone's two modes.
 *
 * - `ai`: the module's takeover. This app is the phone app and the call screener,
 *   Google Phone is disabled, Do Not Disturb is on and the ringer is at zero. The AI
 *   answers every call.
 * - `normal`: an ordinary phone. Google Phone is the phone app again, calls ring and
 *   the owner answers them. This app leaves every call alone; its service only keeps
 *   the connection to the agent so the phone can be switched back from the web console.
 *
 * The mode is stored in `/data/adb/pixelcallai/mode`, outside the module folder so it
 * survives module updates, and `service.sh` reads it at boot. No file = `ai`: that is
 * how every phone ran before this switch existed. [Prefs.phoneMode] mirrors it for code
 * that cannot use root.
 *
 * The system settings are changed by a root script (`assets/apply_mode.sh`, copied to
 * `/data/adb/pixelcallai/`) that runs in its own session: Android may stop this app when
 * it loses the phone-app role, and the script still finishes. It records every step in
 * `last_apply` and keeps `pending` until all of them worked; the next start of the
 * service runs it again ([syncFromRoot]). A switch is refused during a call.
 */
object PhoneModeManager {
    const val ERR_IN_CALL = "in_call"
    const val ERR_BUSY = "busy"
    const val ERR_INVALID_MODE = "invalid_mode"
    const val ERR_NO_ROOT = "no_root"
    const val ERR_SAVE = "save_failed"
    const val ERR_DIALER = "dialer_failed"
    const val ERR_SCREENING = "screening_failed"
    const val ERR_DND = "dnd_failed"
    const val ERR_VOLUME = "volume_failed"
    const val ERR_FAILED = "failed"

    /** Ids of switches started on the phone itself; their results are not sent to the agent. */
    const val LOCAL_ID_PREFIX = "app-"

    /** [mode] is the phone's mode after the attempt; [error] is empty when [ok]. */
    class Result(val ok: Boolean, val mode: String, val error: String)

    /** One run of the root script, read from `last_apply`. */
    class ApplyLog(val id: String, val mode: String, val done: Boolean, val error: String) {
        val ok: Boolean get() = done && error.isEmpty()
    }

    fun interface Listener {
        /** The mode was (re)applied. Called on the switch thread. */
        fun onModeChanged(mode: String)
    }

    private const val TAG = "PhoneMode"
    private const val STATE_DIR = "/data/adb/pixelcallai"
    private const val MODE_FILE = "$STATE_DIR/mode"
    private const val PENDING_FILE = "$STATE_DIR/pending"
    private const val LOG_FILE = "$STATE_DIR/last_apply"
    private const val LOCK_DIR = "$STATE_DIR/apply.lock"
    private const val SCRIPT = "$STATE_DIR/apply_mode.sh"
    /** Ring / notification / … volumes the phone had before it last became an AI phone. */
    private const val AUDIO_FILE = "$STATE_DIR/normal_audio"
    private const val SCRIPT_ASSET = "apply_mode.sh"
    /** The same characters service.sh and the script drop before comparing the mode. */
    private const val STRIP = "' \\t\\r\\n\\v\\f\\357\\273\\277'"
    /** No `last_apply` line from the script by then: it did not start. */
    private const val SCRIPT_START_MS = 10_000L
    private const val SCRIPT_MAX_MS = 90_000L
    private const val POLL_MS = 400L

    private class Volume(val stream: Int, val key: String)

    /** Restored in normal mode, in the order of the script's arguments. */
    private val VOLUMES = listOf(
        Volume(AudioManager.STREAM_RING, "ring"),
        Volume(AudioManager.STREAM_NOTIFICATION, "notification"),
        Volume(AudioManager.STREAM_VOICE_CALL, "voice_call"),
        Volume(AudioManager.STREAM_MUSIC, "music"),
        Volume(AudioManager.STREAM_ALARM, "alarm"),
        Volume(AudioManager.STREAM_SYSTEM, "system"),
    )

    private val executor = Executors.newSingleThreadExecutor { r ->
        Thread(r, "phone-mode").apply { isDaemon = true }
    }
    private val busy = AtomicBoolean(false)
    private val listeners = CopyOnWriteArraySet<Listener>()

    fun addListener(listener: Listener) {
        listeners.add(listener)
    }

    fun removeListener(listener: Listener) {
        listeners.remove(listener)
    }

    /** A switch (or the start-up check) is running. */
    fun isSwitching(): Boolean = busy.get()

    fun isValidMode(mode: String): Boolean =
        mode == Prefs.PHONE_MODE_AI || mode == Prefs.PHONE_MODE_NORMAL

    fun newLocalId(): String = LOCAL_ID_PREFIX + UUID.randomUUID()

    /** The id goes on the script's command line: letters, digits and dashes only. */
    fun safeId(id: String): String =
        id.filter { (it.isLetterOrDigit() && it.code < 128) || it == '-' }.take(64)

    /**
     * Switch to [target] in the background. [callBusy] = the caller knows a call is
     * ringing or active. [onDone] runs on the switch thread, or right away when the
     * switch cannot start.
     */
    fun requestSwitch(
        context: Context,
        target: String,
        callBusy: Boolean,
        id: String = newLocalId(),
        onDone: (Result) -> Unit,
    ) {
        val ctx = context.applicationContext
        val current = Prefs.phoneMode(ctx)
        val runId = safeId(id).ifEmpty { newLocalId() }
        val refused = when {
            !isValidMode(target) -> ERR_INVALID_MODE
            callBusy || !isPhoneIdle(ctx) -> ERR_IN_CALL
            !busy.compareAndSet(false, true) -> ERR_BUSY
            else -> null
        }
        if (refused != null) {
            Log.i(TAG, "switch to $target refused: $refused")
            onDone(Result(false, current, refused))
            return
        }
        executor.execute {
            val result = try {
                switchBlocking(ctx, target, runId)
            } catch (t: Throwable) {
                Log.e(TAG, "switch to $target crashed", t)
                Result(false, Prefs.phoneMode(ctx), ERR_FAILED)
            } finally {
                busy.set(false)
            }
            Log.i(TAG, "switch to $target id=$runId ok=${result.ok} mode=${result.mode} error=${result.error}")
            try {
                onDone(result)
            } catch (t: Throwable) {
                Log.e(TAG, "switch callback failed", t)
            }
        }
    }

    /**
     * Service start: bring the phone in line with the module's mode file. Runs the
     * script again when the last switch did not finish (`pending`), when the mode is
     * normal but Myna is still the phone app (an interrupted switch, or an older
     * version that took the phone over again), or when the app's copy of the mode is
     * different. [onUnreported] gets the result of a switch the agent asked for whose
     * answer was never sent (the app was stopped while it ran).
     */
    fun syncFromRoot(context: Context, onUnreported: (ApplyLog) -> Unit) {
        val ctx = context.applicationContext
        if (!busy.compareAndSet(false, true)) return
        executor.execute {
            try {
                syncBlocking(ctx, onUnreported)
            } catch (t: Throwable) {
                Log.e(TAG, "start-up check failed", t)
            } finally {
                busy.set(false)
            }
        }
    }

    private class RootState(
        val mode: String,
        val pending: Boolean,
        val running: Boolean,
        val dialer: String,
        val log: ApplyLog?,
    )

    private fun syncBlocking(ctx: Context, onUnreported: (ApplyLog) -> Unit) {
        var state = readRootState() ?: return
        if (state.running) {
            // A run started before the app was stopped: let it finish.
            state.log?.let { waitForScript(it.id) }
            state = readRootState() ?: return
        }
        val unreported = state.log?.takeIf {
            it.done && !it.id.startsWith(LOCAL_ID_PREFIX) && it.id != "invalid" &&
                it.id != Prefs.reportedApplyId(ctx)
        }
        val mirror = Prefs.phoneMode(ctx)
        val why = when {
            state.pending -> "the last switch did not finish"
            state.mode == Prefs.PHONE_MODE_NORMAL && state.dialer == ctx.packageName ->
                "normal mode, but Myna is still the phone app"
            state.mode != mirror -> "the module file says ${state.mode}, the app had $mirror"
            else -> null
        }
        if (why == null) {
            Log.i(TAG, "start-up check: ${state.mode}, nothing to do")
        } else if (!isPhoneIdle(ctx)) {
            Log.w(TAG, "start-up check: $why; a call is on, try again at the next start")
        } else {
            Log.i(TAG, "start-up check: $why; applying ${state.mode} again")
            val r = switchBlocking(ctx, state.mode, newLocalId())
            Log.i(TAG, "start-up re-apply ok=${r.ok} error=${r.error}")
        }
        unreported?.let(onUnreported)
    }

    /** False while a call rings or is active (unknown state counts as idle unless the audio mode says otherwise). */
    @Suppress("DEPRECATION")
    fun isPhoneIdle(ctx: Context): Boolean {
        val state = try {
            ctx.getSystemService(TelephonyManager::class.java)?.callState
        } catch (_: Exception) {
            null
        }
        if (state != null) return state == TelephonyManager.CALL_STATE_IDLE
        val audioMode = try {
            ctx.getSystemService(AudioManager::class.java)?.mode
        } catch (_: Exception) {
            null
        }
        return audioMode != AudioManager.MODE_IN_CALL && audioMode != AudioManager.MODE_RINGTONE
    }

    /** The module's mode file (no file = ai), or null when root is not available. */
    fun readRootMode(): String? {
        val out = RootShell.run("[ -f $MODE_FILE ] && tr -d $STRIP < $MODE_FILE; true", 5)
        if (!out.ok) return null
        return Prefs.normalizeMode(out.text)
    }

    private fun switchBlocking(ctx: Context, target: String, id: String): Result {
        val before = Prefs.phoneMode(ctx)
        if (!RootShell.available()) return Result(false, before, ERR_NO_ROOT)
        if (!isPhoneIdle(ctx)) return Result(false, before, ERR_IN_CALL)
        if (scriptRunning()) return Result(false, before, ERR_BUSY)
        if (target == Prefs.PHONE_MODE_AI && before == Prefs.PHONE_MODE_NORMAL) saveNormalVolumes(ctx)
        if (target == Prefs.PHONE_MODE_NORMAL) {
            // Stop muting at once; the script changes the system settings.
            Prefs.setPhoneMode(ctx, Prefs.PHONE_MODE_NORMAL)
            LocalAudioGuard(ctx).release()
            unmuteMicrophone(ctx)
            notifyListeners(Prefs.PHONE_MODE_NORMAL)
        }
        val volumes = if (target == Prefs.PHONE_MODE_NORMAL) normalVolumes(ctx) else emptyList()
        val log = if (installScript(ctx) && launchScript(target, id, volumes)) waitForScript(id) else null
        // The mode file is the truth (a failed "save" step leaves the old mode).
        val mode = readRootMode() ?: Prefs.phoneMode(ctx)
        Prefs.setPhoneMode(ctx, mode)
        notifyListeners(mode)
        if (log == null) return Result(false, mode, ERR_FAILED)
        if (!log.ok) return Result(false, mode, log.error.ifEmpty { ERR_FAILED })
        Prefs.clearLegacyAiSwitch(ctx)
        return Result(true, mode, "")
    }

    private fun notifyListeners(mode: String) {
        for (listener in listeners) {
            try {
                listener.onModeChanged(mode)
            } catch (t: Throwable) {
                Log.e(TAG, "mode listener failed", t)
            }
        }
    }

    // ------------------------------------------------------------- the root script

    /** Copy the script from the APK to the root-only state folder. */
    private fun installScript(ctx: Context): Boolean {
        val src = File(ctx.createDeviceProtectedStorageContext().filesDir, SCRIPT_ASSET)
        try {
            src.parentFile?.mkdirs()
            ctx.assets.open(SCRIPT_ASSET).use { input -> src.outputStream().use { input.copyTo(it) } }
        } catch (e: Exception) {
            Log.e(TAG, "cannot unpack $SCRIPT_ASSET", e)
            return false
        }
        val out = RootShell.run(
            "mkdir -p $STATE_DIR && cat ${RootShell.quote(src.absolutePath)} > $SCRIPT.new && " +
                "mv -f $SCRIPT.new $SCRIPT && chmod 700 $SCRIPT",
            10,
        )
        return out.ok
    }

    /** Start the script in its own session: it keeps running if this app is stopped. */
    private fun launchScript(target: String, id: String, volumes: List<Int>): Boolean {
        val args = (listOf(target, id) + volumes.map { it.toString() }).joinToString(" ")
        val out = RootShell.run("toybox setsid sh $SCRIPT $args </dev/null >/dev/null 2>&1 &", 10)
        return out.ok
    }

    /** The script's log for [id] once it is done; null when it never started or hangs. */
    private fun waitForScript(id: String): ApplyLog? {
        val start = SystemClock.elapsedRealtime()
        var started = false
        while (true) {
            val log = readLog()
            if (log != null && log.id == id) {
                if (log.done) return log
                started = true
            }
            val elapsed = SystemClock.elapsedRealtime() - start
            if (!started && elapsed > SCRIPT_START_MS && !scriptRunning()) {
                Log.e(TAG, "the mode script did not start (id=$id)")
                return null
            }
            if (elapsed > SCRIPT_MAX_MS) {
                Log.e(TAG, "the mode script did not finish in time (id=$id)")
                return null
            }
            try {
                Thread.sleep(POLL_MS)
            } catch (_: InterruptedException) {
                return null
            }
        }
    }

    private fun scriptRunning(): Boolean {
        val out = RootShell.run(
            "p=\$(cat $LOCK_DIR/pid 2>/dev/null); [ -n \"\$p\" ] && kill -0 \"\$p\" 2>/dev/null && echo yes; true",
            5,
        )
        return out.text.trim() == "yes"
    }

    private fun readLog(): ApplyLog? {
        val out = RootShell.run("cat $LOG_FILE 2>/dev/null; true", 5)
        return if (out.ok) parseLog(keyValues(out.text)) else null
    }

    private fun readRootState(): RootState? {
        val out = RootShell.run(
            "echo \"mode=\$([ -f $MODE_FILE ] && tr -d $STRIP < $MODE_FILE)\"; " +
                "[ -f $PENDING_FILE ] && echo pending=1; " +
                "p=\$(cat $LOCK_DIR/pid 2>/dev/null); [ -n \"\$p\" ] && kill -0 \"\$p\" 2>/dev/null && echo running=1; " +
                "echo \"dialer=\$(telecom get-default-dialer 2>/dev/null)\"; " +
                "sed 's/^/log./' $LOG_FILE 2>/dev/null; true",
            15,
        )
        if (!out.ok) return null
        val kv = keyValues(out.text)
        val log = parseLog(
            kv.filterKeys { it.startsWith("log.") }.mapKeys { it.key.removePrefix("log.") },
        )
        return RootState(
            mode = Prefs.normalizeMode(kv["mode"].orEmpty()),
            pending = kv["pending"] == "1",
            running = kv["running"] == "1",
            dialer = kv["dialer"].orEmpty().trim(),
            log = log,
        )
    }

    /** `key=value` lines; the first `=` splits. */
    private fun keyValues(text: String): Map<String, String> {
        val map = mutableMapOf<String, String>()
        for (line in text.lineSequence()) {
            val i = line.indexOf('=')
            if (i <= 0) continue
            map[line.substring(0, i).trim()] = line.substring(i + 1).trim()
        }
        return map
    }

    private fun parseLog(kv: Map<String, String>): ApplyLog? {
        val id = kv["id"] ?: return null
        val done = kv["done"] == "1"
        val error = kv["error"].orEmpty().ifEmpty {
            if (done && kv["result"] != "ok") ERR_FAILED else ""
        }
        return ApplyLog(id, kv["mode"].orEmpty(), done, error)
    }

    // ------------------------------------------------------------- volumes

    /** Normal-mode volumes for the script: saved value, or half of the maximum (0 = skip). */
    private fun normalVolumes(ctx: Context): List<Int> {
        val am = ctx.getSystemService(AudioManager::class.java) ?: return emptyList()
        val saved = readSavedVolumes()
        return VOLUMES.map { v ->
            val max = try {
                am.getStreamMaxVolume(v.stream)
            } catch (_: Exception) {
                0
            }
            // A saved zero would leave the phone silent: a normal phone rings.
            if (max <= 0) 0 else (saved[v.key]?.takeIf { it > 0 } ?: defaultVolume(max)).coerceIn(1, max)
        }
    }

    /** Half of the maximum, when nothing was saved. */
    fun defaultVolume(max: Int): Int = ((max + 1) / 2).coerceAtLeast(1)

    private fun saveNormalVolumes(ctx: Context) {
        val am = ctx.getSystemService(AudioManager::class.java) ?: return
        val lines = VOLUMES.mapNotNull { v ->
            try {
                "${v.key}=${am.getStreamVolume(v.stream)}"
            } catch (_: Exception) {
                null
            }
        }
        if (lines.isEmpty()) return
        // Only digits and fixed keys: safe inside the single-quoted printf format.
        val body = lines.joinToString("\\n")
        RootShell.run("mkdir -p $STATE_DIR && printf '$body\\n' > $AUDIO_FILE")
    }

    private fun readSavedVolumes(): Map<String, Int> {
        val out = RootShell.run("cat $AUDIO_FILE 2>/dev/null; true", 5)
        return if (out.ok) parseSavedVolumes(out.text) else emptyMap()
    }

    /** `key=value` lines; anything else is skipped. */
    fun parseSavedVolumes(text: String): Map<String, Int> =
        keyValues(text).mapNotNull { (k, v) -> v.toIntOrNull()?.let { k to it } }.toMap()

    private fun unmuteMicrophone(ctx: Context) {
        try {
            ctx.getSystemService(AudioManager::class.java)?.isMicrophoneMute = false
        } catch (e: Exception) {
            Log.w(TAG, "microphone unmute: ${e.message}")
        }
    }
}
