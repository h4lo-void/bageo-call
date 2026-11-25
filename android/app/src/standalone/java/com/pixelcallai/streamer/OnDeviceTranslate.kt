// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.content.Context
import android.icu.util.ULocale
import android.os.Build
import android.os.CancellationSignal
import android.util.Log
import android.view.textclassifier.TextClassificationManager
import android.view.textclassifier.TextClassifier
import android.view.textclassifier.TextLanguage
import android.view.translation.TranslationCapability
import android.view.translation.TranslationContext
import android.view.translation.TranslationManager
import android.view.translation.TranslationRequest
import android.view.translation.TranslationRequestValue
import android.view.translation.TranslationSpec
import android.view.translation.Translator
import androidx.annotation.RequiresApi
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

/**
 * Translating a call's lines on the phone itself (Android 12+ on-device
 * translation, the one Live Translate uses): the words never leave the phone.
 * Each line's language is told by the system's text classifier; lines already in
 * the owner's language, in a language pair the phone cannot do, or that come back
 * empty are left out, and the page asks the summary's service for the rest.
 * Blocking: call it off the main thread.
 */
object OnDeviceTranslate {
    private const val TAG = "OnDeviceTranslate"
    private const val MIN_CONFIDENCE = 0.5f
    private val executor = Executors.newSingleThreadExecutor()

    fun run(ctx: Context, target: String, lines: List<Pair<Int, String>>): List<Pair<Int, String>> {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S || lines.isEmpty() || target.isBlank()) return emptyList()
        return try {
            translateAll(ctx, ULocale.forLanguageTag(target), lines)
        } catch (e: Exception) {
            Log.w(TAG, "on-device translation failed: ${e.javaClass.simpleName}")
            emptyList()
        }
    }

    @RequiresApi(Build.VERSION_CODES.S)
    private fun translateAll(ctx: Context, target: ULocale, lines: List<Pair<Int, String>>): List<Pair<Int, String>> {
        val tm = ctx.getSystemService(TranslationManager::class.java) ?: return emptyList()
        val caps = tm.getOnDeviceTranslationCapabilities(TranslationSpec.DATA_FORMAT_TEXT, TranslationSpec.DATA_FORMAT_TEXT)
        val classifier = ctx.getSystemService(TextClassificationManager::class.java)?.textClassifier
        val bySource = linkedMapOf<String, MutableList<Pair<Int, String>>>()
        for ((i, text) in lines) {
            val src = detect(classifier, text) ?: continue
            if (src.language == target.language) continue
            bySource.getOrPut(src.language) { mutableListOf() }.add(i to text)
        }
        val out = mutableListOf<Pair<Int, String>>()
        for ((language, group) in bySource) {
            val cap = pick(caps, language, target)
            if (cap == null) {
                Log.i(TAG, "no on-device pair $language->${target.toLanguageTag()}")
                continue
            }
            val translator = create(tm, cap) ?: continue
            try {
                for ((i, text) in group) translate(translator, text)?.let { out += i to it }
            } finally {
                translator.destroy()
            }
        }
        Log.i(TAG, "on-device translation lines=${out.size} asked=${lines.size}")
        return out
    }

    private fun detect(classifier: TextClassifier?, text: String): ULocale? {
        classifier ?: return null
        return try {
            val r = classifier.detectLanguage(TextLanguage.Request.Builder(text).build())
            if (r.localeHypothesisCount == 0) return null
            val best = r.getLocale(0)
            best.takeIf { r.getConfidenceScore(it) >= MIN_CONFIDENCE }
        } catch (_: Exception) {
            null
        }
    }

    /** A pair the phone has or can fetch, the owner's region first (zh-TW: Traditional). */
    @RequiresApi(Build.VERSION_CODES.S)
    private fun pick(caps: Set<TranslationCapability>, source: String, target: ULocale): TranslationCapability? {
        val ok = caps.filter {
            it.state != TranslationCapability.STATE_NOT_AVAILABLE &&
                it.sourceSpec.locale.language == source &&
                it.targetSpec.locale.language == target.language
        }
        return ok.firstOrNull { it.targetSpec.locale.country == target.country && it.state == TranslationCapability.STATE_ON_DEVICE }
            ?: ok.firstOrNull { it.state == TranslationCapability.STATE_ON_DEVICE }
            ?: ok.firstOrNull()
    }

    @RequiresApi(Build.VERSION_CODES.S)
    private fun create(tm: TranslationManager, cap: TranslationCapability): Translator? {
        val done = CountDownLatch(1)
        var made: Translator? = null
        val context = TranslationContext.Builder(cap.sourceSpec, cap.targetSpec).build()
        tm.createOnDeviceTranslator(context, executor) {
            made = it
            done.countDown()
        }
        // A language pack the phone still has to fetch can take a while.
        done.await(30, TimeUnit.SECONDS)
        return made
    }

    @RequiresApi(Build.VERSION_CODES.S)
    private fun translate(translator: Translator, text: String): String? {
        val done = CountDownLatch(1)
        var result: String? = null
        val request = TranslationRequest.Builder()
            .setTranslationRequestValues(listOf(TranslationRequestValue.forText(text)))
            .build()
        translator.translate(request, CancellationSignal(), executor) { response ->
            val values = response.translationResponseValues
            if (values.size() > 0) result = values.valueAt(0).text?.toString()
            done.countDown()
        }
        done.await(15, TimeUnit.SECONDS)
        return result?.trim()?.takeIf { it.isNotEmpty() && it != text.trim() }
    }
}

