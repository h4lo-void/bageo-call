// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.content.Context
import android.content.SharedPreferences
import android.os.Build
import android.util.Log

/**
 * This phone's uplink tuning: the remembered profile and the fit state. Every key ends
 * with "@" + [Build.FINGERPRINT], so a system update starts over; keys of other
 * fingerprints are dropped on the next write. The fingerprint never leaves the phone.
 */
object UplinkTuneStore {
    private const val TAG = "UplinkTune"
    private const val NAME = "uplink_tune"

    /**
     * Google phones (Pixel) only measure and report: no switching, no remembered profile,
     * the same audio path as before. Turn on once real calls show the measurement does not
     * misjudge healthy Pixel tracks.
     */
    const val AUTO_TUNE_ON_GOOGLE = false

    /**
     * `adb shell setprop debug.bageo.uplink_profile <id>` picks the first profile (testing).
     * Google phones ignore it ([measureOnly]). A forced call stores nothing unless the forced
     * profile lost audio and another one took over ([decideTune]).
     */
    const val FORCE_PROPERTY = "debug.bageo.uplink_profile"

    private fun sp(ctx: Context): SharedPreferences =
        ctx.createDeviceProtectedStorageContext().getSharedPreferences(NAME, Context.MODE_PRIVATE)

    private val fingerprint: String by lazy { Build.FINGERPRINT.orEmpty() }

    internal fun key(name: String, fp: String): String = "$name@$fp"

    /** Keys that belong to another fingerprint. */
    internal fun staleKeys(keys: Collection<String>, fp: String): List<String> =
        keys.filter { !it.endsWith("@$fp") }

    val chip: ChipKind by lazy {
        val soc = if (Build.VERSION.SDK_INT >= 31) Build.SOC_MANUFACTURER else null
        chipKind(Build.MANUFACTURER, isMediaTekChip(Build.VERSION.SDK_INT, soc, Build.HARDWARE))
    }

    val measureOnly: Boolean get() = chip == ChipKind.GOOGLE && !AUTO_TUNE_ON_GOOGLE

    fun forcedProfile(): UplinkProfile? = UplinkProfile.parse(readSystemProperty(FORCE_PROPERTY))

    fun rememberedProfile(ctx: Context): UplinkProfile? =
        UplinkProfile.parse(read(ctx, "profile"))

    /** The profile the owner said did not work (the next call starts after it). */
    fun avoidedProfile(ctx: Context): UplinkProfile? = UplinkProfile.parse(read(ctx, "avoid"))

    /** Stored state; a Google phone with nothing stored is a known model and counts as ok. */
    fun fit(ctx: Context): AudioFit =
        AudioFit.parse(read(ctx, "fit")) ?: if (chip == ChipKind.GOOGLE) AudioFit.OK else AudioFit.UNTESTED

    fun reason(ctx: Context): String? = read(ctx, "reason")?.takeIf { it.isNotBlank() }

    /**
     * A measure-only phone whose owner said the caller heard nothing keeps that problem
     * until a system update (new fingerprint) or until Google phones may switch settings:
     * it has nothing else to try, so a later healthy measurement must not erase the report.
     */
    fun keepOwnerProblem(ctx: Context): Boolean =
        measureOnly && fit(ctx) == AudioFit.PROBLEM && reason(ctx) == OWNER_REPORT

    private const val OWNER_REPORT = "owner_report"

    /** Store a call's outcome. [last] is the profile the call ended on. */
    fun apply(ctx: Context, d: TuneDecision, last: UplinkProfile, live: Boolean) {
        try {
            val prefs = sp(ctx)
            val e = prefs.edit()
            staleKeys(prefs.all.keys, fingerprint).forEach { e.remove(it) }
            if (d.forget) e.remove(key("profile", fingerprint))
            d.remember?.let { e.putString(key("profile", fingerprint), it.id) }
            d.fit?.let {
                e.putString(key("fit", fingerprint), it.id)
                if (d.reason != null) {
                    e.putString(key("reason", fingerprint), d.reason)
                } else {
                    e.remove(key("reason", fingerprint))
                }
            }
            if (live) e.putString(key("last", fingerprint), last.id)
            // The owner's report applies until a call settles on settings, then it is used up.
            if (d.remember != null || d.fit != null) e.remove(key("avoid", fingerprint))
            e.apply()
        } catch (ex: Exception) {
            Log.w(TAG, "store failed: ${ex.message}")
        }
    }

    /**
     * The owner said the caller did not hear the AI: state problem, forget the remembered
     * profile, and start the next call after the profile the last call ended on.
     */
    fun markOwnerProblem(ctx: Context): AudioFit {
        try {
            val prefs = sp(ctx)
            val e = prefs.edit()
            staleKeys(prefs.all.keys, fingerprint).forEach { e.remove(it) }
            e.putString(key("fit", fingerprint), AudioFit.PROBLEM.id)
            e.putString(key("reason", fingerprint), OWNER_REPORT)
            if (!measureOnly) {
                val last = UplinkProfile.parse(prefs.getString(key("last", fingerprint), null))
                    ?: UplinkProfile.parse(prefs.getString(key("profile", fingerprint), null))
                e.remove(key("profile", fingerprint))
                if (last != null) e.putString(key("avoid", fingerprint), last.id)
            }
            e.apply()
        } catch (ex: Exception) {
            Log.w(TAG, "store failed: ${ex.message}")
        }
        return fit(ctx)
    }

    private fun read(ctx: Context, name: String): String? = try {
        sp(ctx).getString(key(name, fingerprint), null)
    } catch (_: Exception) {
        null
    }

    private fun readSystemProperty(key: String): String? = try {
        Class.forName("android.os.SystemProperties")
            .getMethod("get", String::class.java, String::class.java)
            .invoke(null, key, "") as? String
    } catch (_: Throwable) {
        null
    }
}
