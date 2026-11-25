// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.media.AudioManager
import android.media.Ringtone
import android.media.RingtoneManager
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.VibrationEffect
import android.os.Vibrator
import android.os.VibratorManager
import android.provider.Settings
import android.util.Log
import androidx.core.app.NotificationCompat

/**
 * Standalone: a ringing call goes back to the owner. Bageo is the phone app and Android
 * leaves the ring to it (IN_CALL_SERVICE_RINGING), so a call the AI does not take would
 * otherwise ring unseen and unheard. This rings and vibrates as the phone's ringer and Do
 * Not Disturb allow, opens the call screen (also over the lock screen), posts a call
 * notification with answer and decline, and after a call the owner did not take, a
 * missed-call notice.
 *
 * It needs neither [CallAudioService] nor the agent, and keeps nothing on disk (it works
 * before the first unlock). [SilentInCallService] hands calls over when the call service
 * never took them; [CallAudioService] when the AI could not answer. Both go through here,
 * so a call rings once. Main thread.
 */
object OwnerRing {
    const val ACTION_ANSWER = "com.pixelcallai.streamer.OWNER_RING_ANSWER"
    const val ACTION_DECLINE = "com.pixelcallai.streamer.OWNER_RING_DECLINE"

    /** OwnerCallActivity's screen for a call that rings for the owner. */
    const val SCREEN_INCOMING = "incoming"
    private const val SCREEN_IN_CALL = "in_call"
    private const val SCREEN_DIALER = "dialer"

    private const val TAG = "OwnerRing"
    private const val RING_CHANNEL_ID = "bageo_incoming_ring"
    private const val MISSED_CHANNEL_ID = "bageo_missed_calls"
    /** The channel a call the AI could not answer always used. */
    private const val AI_MISSED_CHANNEL_ID = "bageo_ai_missed"
    private const val NOTIF_MISSED_ID = 45
    private const val NOTIF_RING_ID = 46
    private val VIBRATION = longArrayOf(0L, 1_000L, 1_000L)

    private val main = Handler(Looper.getMainLooper())

    /** Call instance handed to the owner (0 = instance unknown); null when there is none. */
    @Volatile
    private var handed: Long? = null
    private var caller = ""
    private var aiMissed = false
    /** The owner answered or declined: no missed-call notice. */
    private var settled = false
    /** [CallAudioService]'s owner flag was set here; cleared once no call is left. */
    private var ownerFlagSet = false
    private var ringtone: Ringtone? = null
    private var vibrator: Vibrator? = null

    /** A ringing call was handed to the owner and has not ended yet. */
    fun isHandedToOwner(): Boolean = handed != null

    /**
     * Ring the owner for the ringing call [instanceId]. [aiMissed]: the phone is in AI mode
     * and the AI did not take the call (the screen and notices say so). A second hand-over
     * of the same ring does nothing.
     */
    fun handToOwner(context: Context, instanceId: Long, callerId: String, aiMissed: Boolean, why: String) {
        if (!BuildConfig.STANDALONE) return
        val ctx = context.applicationContext
        if (Looper.myLooper() != Looper.getMainLooper()) {
            main.post { handToOwner(ctx, instanceId, callerId, aiMissed, why) }
            return
        }
        if (handed != null) {
            Log.i(TAG, "call already rings for the owner ($why) instance=$instanceId")
            return
        }
        handed = instanceId
        caller = callerId
        this.aiMissed = aiMissed
        settled = false
        // The AI stays off this call, also when the owner answers it.
        ownerFlagSet = true
        CallAudioService.setOwnerHandlingCall(true)
        if (Prefs.isAiCallEnabled(ctx)) {
            // The ring was silenced for the AI: give the owner the phone's sound back.
            val guard = LocalAudioGuard(ctx)
            guard.stopHoldLoop()
            guard.restoreForOwnCall()
        }
        SilentInCallService.unmuteOwnCall()
        startAlert(ctx, callerId)
        postRingNotification(ctx)
        openCallScreen(ctx, SCREEN_INCOMING)
        Log.w(TAG, "ringing call handed to the owner ($why) instance=$instanceId ai_missed=$aiMissed")
    }

    /** The owner answers the call that rings for them. [openScreen]: show the call screen. */
    fun answer(context: Context, openScreen: Boolean = true) {
        val ctx = context.applicationContext
        onMain {
            stopAlert(ctx)
            if (handed != null) settled = true
            val ok = SilentInCallService.answerForOwner()
            Log.i(TAG, "owner answered ok=$ok")
            if (openScreen) openCallScreen(ctx, SCREEN_IN_CALL)
        }
    }

    /** The owner declines the call that rings for them. */
    fun decline(context: Context) {
        val ctx = context.applicationContext
        onMain {
            stopAlert(ctx)
            if (handed != null) settled = true
            val ok = SilentInCallService.rejectRinging()
            Log.i(TAG, "owner declined ok=$ok")
        }
    }

    /** Call [instanceId] is up (answered here, on a headset, or by Telecom): stop ringing. */
    fun onCallActive(context: Context, instanceId: Long) {
        val h = handed ?: return
        if (h != instanceId && h != 0L) return
        stopAlert(context.applicationContext)
        settled = true
    }

    /**
     * Call [instanceId] ended. The one that rang for the owner and was neither answered
     * nor declined gets a missed-call notice. With [noCallsLeft] the owner flag goes, and
     * an AI phone gets its call service back (it mutes the phone again).
     */
    fun onCallRemoved(context: Context, instanceId: Long, noCallsLeft: Boolean) {
        if (!BuildConfig.STANDALONE) return
        val ctx = context.applicationContext
        val h = handed
        if (h != null && (h == instanceId || h == 0L || noCallsLeft)) {
            stopAlert(ctx)
            if (!settled) {
                showMissed(ctx, caller, aiMissed)
                Log.i(TAG, "call that rang for the owner was not taken instance=$h")
            }
            handed = null
            caller = ""
            aiMissed = false
            settled = false
        }
        if (noCallsLeft && ownerFlagSet) {
            ownerFlagSet = false
            CallAudioService.setOwnerHandlingCall(false)
            if (Prefs.isAiCallEnabled(ctx) && !CallAudioService.isRunning()) CallAudioService.ensureArmed(ctx)
        }
    }

    private fun onMain(block: () -> Unit) {
        if (Looper.myLooper() == Looper.getMainLooper()) block() else main.post(block)
    }

    // ------------------------------------------------------------- ring and vibrate

    private fun startAlert(ctx: Context, callerId: String) {
        stopAlert(ctx, keepNotification = true)
        val ringer = when (ringerMode(ctx)) {
            AudioManager.RINGER_MODE_SILENT -> RingHandoffRules.Ringer.SILENT
            AudioManager.RINGER_MODE_VIBRATE -> RingHandoffRules.Ringer.VIBRATE
            else -> RingHandoffRules.Ringer.NORMAL
        }
        val vibrateForCalls = try {
            Settings.System.getInt(ctx.contentResolver, "vibrate_when_ringing", 1) != 0
        } catch (_: Exception) {
            true
        }
        val dndAllows = dndAllowsCall(ctx, callerId)
        val alert = RingHandoffRules.alertFor(ringer, dndAllows, vibrateForCalls)
        Log.i(TAG, "owner ring: ringer=$ringer dnd_allows=$dndAllows sound=${alert.sound} vibrate=${alert.vibrate}")
        if (alert.sound) playRingtone(ctx)
        if (alert.vibrate) vibrate(ctx)
    }

    private fun stopAlert(ctx: Context, keepNotification: Boolean = false) {
        ringtone?.let { r ->
            try {
                if (r.isPlaying) r.stop()
            } catch (_: Exception) {
            }
        }
        ringtone = null
        vibrator?.let { v ->
            try {
                v.cancel()
            } catch (_: Exception) {
            }
        }
        vibrator = null
        if (!keepNotification) {
            try {
                ctx.getSystemService(NotificationManager::class.java)?.cancel(NOTIF_RING_ID)
            } catch (_: Exception) {
            }
        }
    }

    private fun ringerMode(ctx: Context): Int = try {
        ctx.getSystemService(AudioManager::class.java)?.ringerMode ?: AudioManager.RINGER_MODE_NORMAL
    } catch (_: Exception) {
        AudioManager.RINGER_MODE_NORMAL
    }

    /** Do Not Disturb lets this call ring (no Do Not Disturb, or its call rules allow it). */
    private fun dndAllowsCall(ctx: Context, callerId: String): Boolean {
        val nm = ctx.getSystemService(NotificationManager::class.java) ?: return true
        val filter = try {
            nm.currentInterruptionFilter
        } catch (_: Exception) {
            NotificationManager.INTERRUPTION_FILTER_UNKNOWN
        }
        return when (filter) {
            NotificationManager.INTERRUPTION_FILTER_ALL, NotificationManager.INTERRUPTION_FILTER_UNKNOWN -> true
            NotificationManager.INTERRUPTION_FILTER_PRIORITY -> priorityAllowsCall(nm, callerId)
            else -> false
        }
    }

    private fun priorityAllowsCall(nm: NotificationManager, callerId: String): Boolean {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU && callerId.isNotBlank()) {
            try {
                // Starred contacts, repeat callers and the rest of the phone's call rules.
                return nm.matchesCallFilter(Uri.fromParts("tel", callerId, null))
            } catch (e: Exception) {
                Log.w(TAG, "call filter check failed: ${e.message}")
            }
        }
        return try {
            val policy = nm.notificationPolicy
            (policy.priorityCategories and NotificationManager.Policy.PRIORITY_CATEGORY_CALLS) != 0 &&
                policy.priorityCallSenders == NotificationManager.Policy.PRIORITY_SENDERS_ANY
        } catch (_: Exception) {
            false
        }
    }

    private fun playRingtone(ctx: Context) {
        try {
            val uri = RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE) ?: return
            val r = RingtoneManager.getRingtone(ctx, uri) ?: return
            r.audioAttributes = AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_NOTIFICATION_RINGTONE)
                .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                .build()
            r.isLooping = true
            r.play()
            ringtone = r
        } catch (e: Exception) {
            Log.w(TAG, "ringtone failed: ${e.message}")
        }
    }

    private fun vibrate(ctx: Context) {
        try {
            val found = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                ctx.getSystemService(VibratorManager::class.java)?.defaultVibrator
            } else {
                @Suppress("DEPRECATION")
                ctx.getSystemService(Context.VIBRATOR_SERVICE) as? Vibrator
            }
            val v = found ?: return
            if (!v.hasVibrator()) return
            val attrs = AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_NOTIFICATION_RINGTONE)
                .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                .build()
            @Suppress("DEPRECATION")
            v.vibrate(VibrationEffect.createWaveform(VIBRATION, 0), attrs)
            vibrator = v
        } catch (e: Exception) {
            Log.w(TAG, "vibration failed: ${e.message}")
        }
    }

    // ------------------------------------------------------------- screen and notices

    private fun text(ctx: Context, name: String, fallback: String): String = try {
        val id = ctx.resources.getIdentifier(name, "string", ctx.packageName)
        if (id != 0) ctx.getString(id) else fallback
    } catch (_: Exception) {
        fallback
    }

    private fun callScreenIntent(ctx: Context, screen: String): Intent = Intent().apply {
        setClassName(ctx, "com.pixelcallai.streamer.OwnerCallActivity")
        putExtra("mode", screen)
        putExtra("caller_id", caller)
        putExtra("caller_name", "")
        putExtra("reason", "")
        putExtra("urgent_id", "")
        putExtra("ai_missed", aiMissed)
        addFlags(
            Intent.FLAG_ACTIVITY_NEW_TASK or
                Intent.FLAG_ACTIVITY_SINGLE_TOP or
                Intent.FLAG_ACTIVITY_CLEAR_TOP,
        )
    }

    private fun openCallScreen(ctx: Context, screen: String) {
        try {
            ctx.startActivity(callScreenIntent(ctx, screen))
        } catch (e: Exception) {
            // The notification's full-screen intent still shows the call.
            Log.w(TAG, "call screen failed: ${e.message}")
        }
    }

    private fun actionIntent(ctx: Context, action: String, code: Int): PendingIntent =
        PendingIntent.getBroadcast(
            ctx,
            code,
            Intent(ctx, OwnerRingActionReceiver::class.java).setAction(action),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )

    private fun postRingNotification(ctx: Context) {
        val nm = ctx.getSystemService(NotificationManager::class.java) ?: return
        val channel = NotificationChannel(
            RING_CHANNEL_ID,
            text(ctx, "sa_channel_incoming", "Incoming calls"),
            NotificationManager.IMPORTANCE_HIGH,
        )
        // The ring and vibration come from Bageo itself, as the phone's settings allow.
        channel.setSound(null, null)
        channel.enableVibration(false)
        nm.createNotificationChannel(channel)
        val screen = PendingIntent.getActivity(
            ctx,
            210,
            callScreenIntent(ctx, SCREEN_INCOMING),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val display = caller.ifBlank { text(ctx, "sa_call_unknown", "Unknown caller") }
        val body = if (aiMissed) {
            text(ctx, "sa_ai_missed_ringing_text", "Your phone is ringing so you can answer")
        } else {
            text(ctx, "sa_incoming_title", "Incoming call")
        }
        val b = NotificationCompat.Builder(ctx, RING_CHANNEL_ID)
            .setSmallIcon(android.R.drawable.sym_call_incoming)
            .setContentTitle(display)
            .setContentText(body)
            .setPriority(NotificationCompat.PRIORITY_MAX)
            .setCategory(NotificationCompat.CATEGORY_CALL)
            .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
            .setFullScreenIntent(screen, true)
            .setContentIntent(screen)
            .setOngoing(true)
            .setAutoCancel(false)
            .addAction(
                android.R.drawable.ic_menu_call,
                text(ctx, "sa_btn_answer", "Answer"),
                actionIntent(ctx, ACTION_ANSWER, 211),
            )
            .addAction(
                android.R.drawable.ic_menu_close_clear_cancel,
                text(ctx, "sa_btn_hangup", "Hang up"),
                actionIntent(ctx, ACTION_DECLINE, 212),
            )
        try {
            nm.notify(NOTIF_RING_ID, b.build())
        } catch (e: Exception) {
            Log.w(TAG, "call notification failed: ${e.message}")
        }
    }

    private fun showMissed(ctx: Context, number: String, aiMissed: Boolean) {
        val nm = ctx.getSystemService(NotificationManager::class.java) ?: return
        val channelId = if (aiMissed) AI_MISSED_CHANNEL_ID else MISSED_CHANNEL_ID
        nm.createNotificationChannel(
            NotificationChannel(
                channelId,
                if (aiMissed) {
                    text(ctx, "sa_channel_ai_missed", "Calls AI couldn't answer")
                } else {
                    text(ctx, "sa_channel_missed", "Missed calls")
                },
                NotificationManager.IMPORTANCE_DEFAULT,
            ),
        )
        val open = PendingIntent.getActivity(
            ctx,
            213,
            Intent(ctx, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val title = if (aiMissed) {
            text(ctx, "sa_ai_missed_title", "AI couldn't answer")
        } else {
            text(ctx, "sa_missed_title", "Missed call")
        }
        val b = NotificationCompat.Builder(ctx, channelId)
            .setSmallIcon(android.R.drawable.stat_notify_missed_call)
            .setContentTitle(title)
            .setContentText(number.ifBlank { text(ctx, "sa_call_unknown", "Unknown caller") })
            .setContentIntent(open)
            .setAutoCancel(true)
        if (number.isNotBlank()) {
            val dial = Intent().apply {
                setClassName(ctx, "com.pixelcallai.streamer.OwnerCallActivity")
                putExtra("mode", SCREEN_DIALER)
                putExtra("dial_number", number)
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
            }
            b.addAction(
                android.R.drawable.ic_menu_call,
                text(ctx, "sa_btn_callback", "Call back"),
                PendingIntent.getActivity(
                    ctx,
                    214,
                    dial,
                    PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
                ),
            )
        }
        try {
            nm.notify(NOTIF_MISSED_ID, b.build())
        } catch (e: Exception) {
            Log.w(TAG, "missed-call notice failed: ${e.message}")
        }
    }
}
