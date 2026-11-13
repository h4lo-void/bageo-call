package com.pixelcallai.streamer

import android.content.Context
import android.media.AudioManager
import android.os.SystemClock
import android.telecom.TelecomManager
import android.telephony.TelephonyManager
import android.util.Log
import java.io.File
import java.util.UUID
import java.util.concurrent.CopyOnWriteArraySet
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.math.abs

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
 * how every phone ran before this switch existed. [Prefs.phoneMode] mirrors it.
 *
 * The app needs no root for a switch. The module starts `mode_daemon.sh` as root at boot,
 * and it watches `files/mode/` in this app's device-protected storage: the app writes
 * `request` (one line, `<ai|normal> <id> [volumes]`), the helper takes it within two
 * seconds, runs the module's `apply_mode.sh` and writes `state` (key=value lines: the
 * mode, `pending`, `running`, the last request and its exit code, and the script's
 * `last_apply` as `log.*`). The script runs outside this app, so it finishes even when
 * Android stops the app on the way (losing the phone-app role can do that). It keeps
 * `pending` until every step worked; the next start of the service asks for the switch
 * again ([syncFromRoot]). A switch is refused during a call, and fails with
 * [ERR_NO_ROOT] when nobody takes the request (the module is not running).
 */
object PhoneModeManager {
    const val ERR_IN_CALL = "in_call"
    const val ERR_BUSY = "busy"
    const val ERR_INVALID_MODE = "invalid_mode"
    /** The module's helper did not take the request: the module is not running. */
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
    /** Folder in device-protected storage that the module's mode_daemon.sh watches. */
    private const val DIR_NAME = "mode"
    private const val REQUEST_FILE = "request"
    private const val STATE_FILE = "state"
    /** Ring / notification / … volumes the phone had before it last became an AI phone. */
    private const val AUDIO_FILE = "normal_audio"
    /** apply_mode.sh's exit code when another run holds its lock. */
    private const val EXIT_LOCKED = 4
    /** The helper looks every 2 seconds: a request still there by then means it is not running. */
    private const val SCRIPT_START_MS = 10_000L
    private const val SCRIPT_MAX_MS = 90_000L
    /** The module's helper starts about this long after boot at the latest. */
    private const val BOOT_GRACE_MS = 180_000L
    private const val POLL_MS = 400L
    /**
     * Service start: how long to wait for a state written since this boot. At boot the
     * module starts its helper after this service (once the boot is complete).
     */
    private const val SYNC_WAIT_MS = 300_000L
    private const val SYNC_POLL_MS = 2_000L
    /** The state's boot time and this app's may differ this much and still be one boot. */
    private const val SAME_BOOT_SLACK_SEC = 30L

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

    /** The id goes into the request line: letters, digits and dashes only. */
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
     * switch again when the last one did not finish (`pending`), when the mode is
     * normal but Myna is still the phone app (an interrupted switch, or an older
     * version that took the phone over again), or when the app's copy of the mode is
     * different. [onUnreported] gets the result of a switch the agent asked for whose
     * answer was never sent (the app was stopped while it ran).
     *
     * At boot this service starts before the module's helper, and the state file can
     * still be the one from before the reboot, so this first waits (on its own thread)
     * for a state written since this boot.
     */
    fun syncFromRoot(context: Context, onUnreported: (ApplyLog) -> Unit) {
        val ctx = context.applicationContext
        appDir(ctx)
        Thread({
            if (!waitForCurrentState(ctx)) {
                Log.w(TAG, "start-up check skipped: no state from the Myna module since boot")
            } else if (busy.compareAndSet(false, true)) {
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
        }, "phone-mode-sync").apply { isDaemon = true }.start()
    }

    /** What the module's helper last reported (`state`). */
    private class RootState(
        val mode: String,
        val pending: Boolean,
        /** A run is going; never true for a state written before this boot. */
        val running: Boolean,
        /** Written since this boot. */
        val current: Boolean,
        /** The last request the helper took, and the script's exit code once that run ended. */
        val request: String,
        val exit: Int?,
        val error: String,
        val log: ApplyLog?,
    )

    private fun syncBlocking(ctx: Context, onUnreported: (ApplyLog) -> Unit) {
        var state = readRootState(ctx) ?: return
        if (state.running) {
            // A run started before the app was stopped: let it finish.
            if (state.request.isNotEmpty()) waitForRun(ctx, state.request)
            state = readRootState(ctx) ?: return
        }
        val unreported = state.log?.takeIf {
            it.done && !it.id.startsWith(LOCAL_ID_PREFIX) && it.id != "invalid" &&
                it.id != Prefs.reportedApplyId(ctx)
        }
        val mirror = Prefs.phoneMode(ctx)
        val why = when {
            state.pending -> "the last switch did not finish"
            state.mode == Prefs.PHONE_MODE_NORMAL && defaultDialer(ctx) == ctx.packageName ->
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

    /** The default phone app's package; empty when unknown. */
    private fun defaultDialer(ctx: Context): String = try {
        ctx.getSystemService(TelecomManager::class.java)?.defaultDialerPackage.orEmpty()
    } catch (_: Exception) {
        ""
    }

    /** The module's mode file (no file = ai) as the helper last reported it; null without a state. */
    private fun readRootMode(ctx: Context): String? = readRootState(ctx)?.mode

    private fun switchBlocking(ctx: Context, target: String, id: String): Result {
        val before = Prefs.phoneMode(ctx)
        if (!isPhoneIdle(ctx)) return Result(false, before, ERR_IN_CALL)
        if (scriptRunning(ctx)) return Result(false, before, ERR_BUSY)
        if (target == Prefs.PHONE_MODE_AI && before == Prefs.PHONE_MODE_NORMAL) saveNormalVolumes(ctx)
        val volumes = if (target == Prefs.PHONE_MODE_NORMAL) normalVolumes(ctx) else emptyList()
        val request = File(appDir(ctx), REQUEST_FILE)
        val line = (listOf(target, id) + volumes.map { it.toString() }).joinToString(" ")
        if (!writeRequest(request, line)) return Result(false, before, ERR_FAILED)
        if (!waitForPickup(request)) {
            // Right after a boot the module's helper starts late (boot completed + ~10 s):
            // "busy, try again" then, not "the module is not running".
            val early = SystemClock.elapsedRealtime() < BOOT_GRACE_MS
            Log.e(TAG, "nobody took the mode request (id=$id, early=$early)")
            return Result(false, before, if (early) ERR_BUSY else ERR_NO_ROOT)
        }
        if (target == Prefs.PHONE_MODE_NORMAL) {
            // Stop muting at once; the module's script changes the system settings.
            Prefs.setPhoneMode(ctx, Prefs.PHONE_MODE_NORMAL)
            LocalAudioGuard(ctx).release()
            unmuteMicrophone(ctx)
            notifyListeners(Prefs.PHONE_MODE_NORMAL)
        }
        val log = waitForRun(ctx, id)
        // No result in time: the script may still be running, so the state file still has
        // the old mode. Keep the app's copy; the next service start brings it in line.
        if (log == null) return Result(false, Prefs.phoneMode(ctx), ERR_FAILED)
        // The mode file is the truth (a failed "save" step leaves the old mode).
        val mode = readRootMode(ctx) ?: Prefs.phoneMode(ctx)
        Prefs.setPhoneMode(ctx, mode)
        notifyListeners(mode)
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

    // ------------------------------------------------------------- the module's helper

    /** `files/mode/` in device-protected storage (usable before the first unlock); made when missing. */
    private fun appDir(ctx: Context): File {
        val dir = File(ctx.createDeviceProtectedStorageContext().filesDir, DIR_NAME)
        if (!dir.isDirectory && !dir.mkdirs()) Log.w(TAG, "cannot create $dir")
        return dir
    }

    /** Written to a temporary file and renamed, so the helper never reads half a request. */
    private fun writeRequest(request: File, line: String): Boolean {
        val tmp = File(request.parentFile, "$REQUEST_FILE.tmp")
        return try {
            tmp.writeText(line + "\n")
            if (tmp.renameTo(request)) {
                true
            } else {
                Log.e(TAG, "cannot hand the mode request over")
                tmp.delete()
                false
            }
        } catch (e: Exception) {
            Log.e(TAG, "cannot write the mode request", e)
            tmp.delete()
            false
        }
    }

    /**
     * True once the helper took [request] (it renames the file). Still there after
     * [SCRIPT_START_MS]: withdrawn, false. A failed delete means it was taken just then.
     */
    private fun waitForPickup(request: File): Boolean {
        val start = SystemClock.elapsedRealtime()
        while (request.exists()) {
            if (SystemClock.elapsedRealtime() - start > SCRIPT_START_MS || !pause(POLL_MS)) {
                return !request.delete()
            }
        }
        return true
    }

    /**
     * The script's log for request [id] once the helper is done with it. A run that ended
     * without one stopped before its first step ([EXIT_LOCKED]: another run held the lock).
     * Null when nothing came back in time.
     */
    private fun waitForRun(ctx: Context, id: String): ApplyLog? {
        val start = SystemClock.elapsedRealtime()
        while (true) {
            val state = readRootState(ctx)
            val log = state?.log
            if (log != null && log.id == id && log.done) return log
            if (state != null && state.request == id && state.exit != null) {
                Log.e(TAG, "the mode script ended without a result (id=$id exit=${state.exit} ${state.error})")
                return ApplyLog(id, "", true, if (state.exit == EXIT_LOCKED) ERR_BUSY else ERR_FAILED)
            }
            if (SystemClock.elapsedRealtime() - start > SCRIPT_MAX_MS) {
                Log.e(TAG, "the mode script did not finish in time (id=$id)")
                return null
            }
            if (!pause(POLL_MS)) return null
        }
    }

    /** Poll until the helper has written a state since this boot; false after [SYNC_WAIT_MS]. */
    private fun waitForCurrentState(ctx: Context): Boolean {
        val start = SystemClock.elapsedRealtime()
        while (true) {
            if (readRootState(ctx)?.current == true) return true
            if (SystemClock.elapsedRealtime() - start > SYNC_WAIT_MS || !pause(SYNC_POLL_MS)) return false
        }
    }

    private fun scriptRunning(ctx: Context): Boolean = readRootState(ctx)?.running == true

    /** The helper's `state` file; null when there is none (the module has not run yet). */
    private fun readRootState(ctx: Context): RootState? {
        val text = try {
            val file = File(appDir(ctx), STATE_FILE)
            if (!file.isFile) return null
            file.readText()
        } catch (e: Exception) {
            Log.w(TAG, "cannot read the mode state: ${e.message}")
            return null
        }
        val kv = keyValues(text)
        if (kv["daemon"] != "1") return null
        val log = parseLog(
            kv.filterKeys { it.startsWith("log.") }.mapKeys { it.key.removePrefix("log.") },
        )
        // Same boot: the boot time the state implies matches this app's.
        val updated = kv["updated"]?.toLongOrNull()
        val uptime = kv["uptime"]?.toLongOrNull()
        val bootSec = (System.currentTimeMillis() - SystemClock.elapsedRealtime()) / 1000
        val current = updated != null && uptime != null &&
            abs(updated - uptime - bootSec) <= SAME_BOOT_SLACK_SEC
        return RootState(
            mode = Prefs.normalizeMode(kv["mode"].orEmpty()),
            pending = kv["pending"] == "1",
            running = current && kv["running"] == "1",
            current = current,
            request = kv["request"].orEmpty(),
            exit = kv["exit"]?.toIntOrNull(),
            error = kv["error"].orEmpty(),
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

    /** False when the thread was interrupted. */
    private fun pause(ms: Long): Boolean = try {
        Thread.sleep(ms)
        true
    } catch (_: InterruptedException) {
        false
    }

    // ------------------------------------------------------------- volumes

    /** Normal-mode volumes for the script: saved value, or half of the maximum (0 = skip). */
    private fun normalVolumes(ctx: Context): List<Int> {
        val am = ctx.getSystemService(AudioManager::class.java) ?: return emptyList()
        val saved = readSavedVolumes(ctx)
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

    /** `files/mode/normal_audio`: one `key=value` line per stream. */
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
        val file = File(appDir(ctx), AUDIO_FILE)
        val tmp = File(file.parentFile, "$AUDIO_FILE.tmp")
        try {
            tmp.writeText(lines.joinToString("\n", postfix = "\n"))
            if (!tmp.renameTo(file)) {
                tmp.delete()
                Log.w(TAG, "cannot save the normal-phone volumes")
            }
        } catch (e: Exception) {
            tmp.delete()
            Log.w(TAG, "cannot save the normal-phone volumes: ${e.message}")
        }
    }

    private fun readSavedVolumes(ctx: Context): Map<String, Int> = try {
        val file = File(appDir(ctx), AUDIO_FILE)
        if (file.isFile) parseSavedVolumes(file.readText()) else emptyMap()
    } catch (_: Exception) {
        emptyMap()
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
