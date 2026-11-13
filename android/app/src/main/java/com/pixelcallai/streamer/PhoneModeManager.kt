package com.pixelcallai.streamer

import android.Manifest
import android.app.NotificationManager
import android.app.role.RoleManager
import android.content.Context
import android.media.AudioManager
import android.os.SystemClock
import android.telephony.TelephonyManager
import android.util.Log
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
 * A switch is refused during a call. It runs every step even when the phone is already
 * in the target mode, so it can simply be repeated, and it reports the first step that
 * did not take effect.
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

    /** [mode] is the phone's mode after the attempt; [error] is empty when [ok]. */
    class Result(val ok: Boolean, val mode: String, val error: String)

    fun interface Listener {
        /** The mode was (re)applied. Called on the switch thread. */
        fun onModeChanged(mode: String)
    }

    private const val TAG = "PhoneMode"
    private const val STATE_DIR = "/data/adb/pixelcallai"
    private const val MODE_FILE = "$STATE_DIR/mode"
    /** Ring / notification / … volumes the phone had before it last became an AI phone. */
    private const val AUDIO_FILE = "$STATE_DIR/normal_audio"
    private const val GOOGLE_DIALER = "com.google.android.dialer"
    private const val VERIFY_MS = 4_000L

    private class Volume(val stream: Int, val key: String, val setting: String?)

    /** Restored in normal mode; the AI takeover sets them to zero (earpiece to minimum). */
    private val VOLUMES = listOf(
        Volume(AudioManager.STREAM_RING, "ring", "volume_ring"),
        Volume(AudioManager.STREAM_NOTIFICATION, "notification", "volume_notification"),
        Volume(AudioManager.STREAM_VOICE_CALL, "voice_call", null),
        Volume(AudioManager.STREAM_MUSIC, "music", "volume_music"),
        Volume(AudioManager.STREAM_ALARM, "alarm", "volume_alarm"),
        Volume(AudioManager.STREAM_SYSTEM, "system", "volume_system"),
    )

    /** Undo the AI mode's root microphone mutes. Controls a phone does not have are ignored. */
    private val MIC_UNMUTE = listOf(
        "cmd media.audio_flinger mute mic 0",
        "tinymix 'Mic Mute' 0",
        "tinymix 'Microphone Mute' 0",
        "tinymix 'ADC1 Mute' 0",
        "tinymix 'ADC2 Mute' 0",
        "tinymix 'IN1 Mute' 0",
        "tinymix 'IN2 Mute' 0",
    )

    private val GRANTS = listOf(
        Manifest.permission.RECORD_AUDIO,
        Manifest.permission.READ_PHONE_STATE,
        Manifest.permission.ANSWER_PHONE_CALLS,
        Manifest.permission.ACCESS_NOTIFICATION_POLICY,
        "android.permission.POST_NOTIFICATIONS",
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

    /** A switch (or the boot-time check) is running. */
    fun isSwitching(): Boolean = busy.get()

    fun isValidMode(mode: String): Boolean =
        mode == Prefs.PHONE_MODE_AI || mode == Prefs.PHONE_MODE_NORMAL

    /**
     * Switch to [target] in the background. [callBusy] = the caller knows a call is
     * ringing or active. [onDone] runs on the switch thread, or right away when the
     * switch cannot start.
     */
    fun requestSwitch(context: Context, target: String, callBusy: Boolean, onDone: (Result) -> Unit) {
        val ctx = context.applicationContext
        val current = Prefs.phoneMode(ctx)
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
                switchBlocking(ctx, target)
            } catch (t: Throwable) {
                Log.e(TAG, "switch to $target crashed", t)
                Result(false, Prefs.phoneMode(ctx), ERR_FAILED)
            } finally {
                busy.set(false)
            }
            Log.i(TAG, "switch to $target ok=${result.ok} mode=${result.mode} error=${result.error}")
            try {
                onDone(result)
            } catch (t: Throwable) {
                Log.e(TAG, "switch callback failed", t)
            }
        }
    }

    /**
     * The module's file is what the phone boots with. When the app's copy differs
     * (the file was changed by hand, or the app's data was cleared), apply the file's
     * mode fully. Does nothing without root or during a call (the next start retries).
     */
    fun syncFromRoot(context: Context) {
        val ctx = context.applicationContext
        if (!busy.compareAndSet(false, true)) return
        executor.execute {
            try {
                val root = readRootMode()
                val mirror = Prefs.phoneMode(ctx)
                if (root != null && root != mirror && isPhoneIdle(ctx)) {
                    Log.i(TAG, "module file says $root, app had $mirror: applying $root")
                    val r = switchBlocking(ctx, root)
                    Log.i(TAG, "sync to $root ok=${r.ok} error=${r.error}")
                }
            } catch (t: Throwable) {
                Log.e(TAG, "sync from module file failed", t)
            } finally {
                busy.set(false)
            }
        }
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
        val out = RootShell.run("if [ -f $MODE_FILE ]; then cat $MODE_FILE; else echo ${Prefs.PHONE_MODE_AI}; fi")
        if (!out.ok) return null
        return if (out.text.trim() == Prefs.PHONE_MODE_NORMAL) Prefs.PHONE_MODE_NORMAL else Prefs.PHONE_MODE_AI
    }

    private fun switchBlocking(ctx: Context, target: String): Result {
        val before = Prefs.phoneMode(ctx)
        if (!RootShell.available()) return Result(false, before, ERR_NO_ROOT)
        if (!isPhoneIdle(ctx)) return Result(false, before, ERR_IN_CALL)
        if (target == Prefs.PHONE_MODE_AI && before == Prefs.PHONE_MODE_NORMAL) saveNormalVolumes(ctx)
        if (!writeModeFile(target)) return Result(false, before, ERR_SAVE)
        if (!Prefs.setPhoneMode(ctx, target)) return Result(false, before, ERR_SAVE)
        val error = if (target == Prefs.PHONE_MODE_NORMAL) applyNormal(ctx) else applyAi(ctx)
        for (listener in listeners) {
            try {
                listener.onModeChanged(target)
            } catch (t: Throwable) {
                Log.e(TAG, "mode listener failed", t)
            }
        }
        return if (error == null) Result(true, target, "") else Result(false, target, error)
    }

    private fun writeModeFile(mode: String): Boolean {
        val out = RootShell.run("mkdir -p $STATE_DIR && echo $mode > $MODE_FILE && cat $MODE_FILE")
        return out.ok && out.text.trim() == mode
    }

    // ------------------------------------------------------------- normal phone

    /** Returns the error code of the first step that did not take effect, or null. */
    private fun applyNormal(ctx: Context): String? {
        val pkg = ctx.packageName
        // Stop holding the microphone mute and the earpiece minimum.
        LocalAudioGuard(ctx).release()

        // 1. Google Phone is the phone app again (on other phones: Android's own default).
        val google = RootShell.run("pm path $GOOGLE_DIALER").let { it.ok && it.text.contains("package:") }
        if (google) {
            DialerRoleHelper.restoreCompetingDialerUi()
        } else {
            RootShell.run("cmd role remove-role-holder android.app.role.DIALER $pkg")
        }
        if (!waitFor { !DialerRoleHelper.isDefaultDialer(ctx) }) return ERR_DIALER

        // 2. This app no longer screens (silences) incoming calls.
        RootShell.run("cmd role remove-role-holder android.app.role.CALL_SCREENING $pkg")
        if (!waitFor { !holdsScreeningRole(ctx) }) return ERR_SCREENING

        // 3. Do Not Disturb off.
        val nm = ctx.getSystemService(NotificationManager::class.java)
        RootShell.run("cmd notification set_dnd off")
        RootShell.run("settings put global zen_mode 0")
        try {
            nm?.setInterruptionFilter(NotificationManager.INTERRUPTION_FILTER_ALL)
        } catch (e: Exception) {
            Log.w(TAG, "interruption filter ALL: ${e.message}")
        }
        if (!waitFor { nm?.currentInterruptionFilter == NotificationManager.INTERRUPTION_FILTER_ALL }) {
            return ERR_DND
        }

        // 4. Ringer on, volumes back.
        if (!restoreVolumes(ctx)) return ERR_VOLUME

        // 5. Microphone no longer muted.
        try {
            ctx.getSystemService(AudioManager::class.java)?.isMicrophoneMute = false
        } catch (e: Exception) {
            Log.w(TAG, "microphone unmute: ${e.message}")
        }
        for (cmd in MIC_UNMUTE) RootShell.run(cmd, 3)
        return null
    }

    private fun restoreVolumes(ctx: Context): Boolean {
        val am = ctx.getSystemService(AudioManager::class.java) ?: return false
        val saved = readSavedVolumes()
        try {
            am.ringerMode = AudioManager.RINGER_MODE_NORMAL
        } catch (e: Exception) {
            Log.w(TAG, "ringer normal: ${e.message}")
        }
        for (v in VOLUMES) {
            val max = try {
                am.getStreamMaxVolume(v.stream)
            } catch (_: Exception) {
                0
            }
            if (max <= 0) continue
            // A saved zero would leave the phone silent: a normal phone rings.
            val level = (saved[v.key]?.takeIf { it > 0 } ?: defaultVolume(max)).coerceIn(1, max)
            try {
                am.setStreamVolume(v.stream, level, 0)
            } catch (e: Exception) {
                Log.w(TAG, "volume ${v.key}=$level: ${e.message}")
            }
            v.setting?.let { RootShell.run("settings put system $it $level") }
        }
        try {
            if (am.ringerMode != AudioManager.RINGER_MODE_NORMAL) am.ringerMode = AudioManager.RINGER_MODE_NORMAL
        } catch (_: Exception) {
        }
        return waitFor {
            am.ringerMode == AudioManager.RINGER_MODE_NORMAL && am.getStreamVolume(AudioManager.STREAM_RING) > 0
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
        val out = RootShell.run("cat $AUDIO_FILE")
        return if (out.ok) parseSavedVolumes(out.text) else emptyMap()
    }

    /** `key=value` lines; anything else is skipped. */
    fun parseSavedVolumes(text: String): Map<String, Int> {
        val map = mutableMapOf<String, Int>()
        for (line in text.lineSequence()) {
            val i = line.indexOf('=')
            if (i <= 0) continue
            val value = line.substring(i + 1).trim().toIntOrNull() ?: continue
            map[line.substring(0, i).trim()] = value
        }
        return map
    }

    // ------------------------------------------------------------- AI phone

    /** The same takeover as `service.sh`, checked step by step. */
    private fun applyAi(ctx: Context): String? {
        val pkg = ctx.packageName
        for (permission in GRANTS) RootShell.run("pm grant $pkg $permission")
        RootShell.run("cmd appops set $pkg ACCESS_NOTIFICATION_POLICY allow")

        // 1. This app is the phone app and the call screener.
        RootShell.run("cmd role add-role-holder android.app.role.DIALER $pkg")
        RootShell.run("cmd role add-role-holder android.app.role.CALL_SCREENING $pkg")
        RootShell.run("telecom set-default-dialer $pkg")
        if (!waitFor { DialerRoleHelper.isDefaultDialer(ctx) }) return ERR_DIALER
        if (!waitFor { holdsScreeningRole(ctx) }) return ERR_SCREENING
        // Only now that this app answers calls: Google Phone's call screen off.
        DialerRoleHelper.suspendCompetingDialerUi()

        // 2. Do Not Disturb on, ringer and notification volume settings zero.
        val nm = ctx.getSystemService(NotificationManager::class.java)
        RootShell.run("settings put global zen_mode 2")
        RootShell.run("cmd notification set_dnd on")
        try {
            nm?.setInterruptionFilter(NotificationManager.INTERRUPTION_FILTER_NONE)
        } catch (e: Exception) {
            Log.w(TAG, "interruption filter NONE: ${e.message}")
        }
        if (!waitFor { nm?.currentInterruptionFilter.let { it != null && it != NotificationManager.INTERRUPTION_FILTER_ALL } }) {
            return ERR_DND
        }
        RootShell.run("settings put system volume_ring 0")
        RootShell.run("settings put system volume_notification 0")
        // Local volumes to zero, microphone muted, earpiece to minimum (as when the service arms).
        LocalAudioGuard(ctx).applyHardMute()
        return null
    }

    // ------------------------------------------------------------- helpers

    private fun holdsScreeningRole(ctx: Context): Boolean =
        ctx.getSystemService(RoleManager::class.java)?.isRoleHeld(RoleManager.ROLE_CALL_SCREENING) == true

    /** Role and settings changes can take a moment to show. */
    private fun waitFor(timeoutMs: Long = VERIFY_MS, check: () -> Boolean): Boolean {
        val deadline = SystemClock.elapsedRealtime() + timeoutMs
        while (true) {
            val ok = try {
                check()
            } catch (_: Exception) {
                false
            }
            if (ok) return true
            if (SystemClock.elapsedRealtime() >= deadline) return false
            try {
                Thread.sleep(250)
            } catch (_: InterruptedException) {
                return false
            }
        }
    }
}
