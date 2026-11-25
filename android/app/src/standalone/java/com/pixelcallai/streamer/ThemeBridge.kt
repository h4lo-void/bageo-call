// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import android.annotation.TargetApi
import android.content.Context
import android.content.res.Configuration
import android.os.Build
import android.webkit.JavascriptInterface
import org.json.JSONObject

/**
 * Hands the phone's colours to the console page (window.BageoTheme).
 *
 * Android 12 and newer: the system palette (the shades of "accent1", "accent2",
 * "neutral1" and "neutral2" that follow the wallpaper). Older phones get Bageo's own
 * teal in the same shape, so the page has one way to read both. The page turns the
 * shades into Material 3 colours for light and dark by itself.
 */
class ThemeBridge(private val ctx: Context) {

    /** JSON: {"dynamic":Boolean,"dark":Boolean,"palette":{"a1":{"0":"#RRGGBB",...},"a2":...,"n1":...,"n2":...}} */
    @JavascriptInterface
    fun colors(): String {
        val night = ctx.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK
        var dynamic = Build.VERSION.SDK_INT >= Build.VERSION_CODES.S
        var palette: JSONObject? = null
        if (dynamic) {
            palette = try {
                systemPalette()
            } catch (_: Exception) {
                null
            }
            if (palette == null) dynamic = false
        }
        return JSONObject()
            .put("dynamic", dynamic)
            .put("dark", night == Configuration.UI_MODE_NIGHT_YES)
            .put("palette", palette ?: bageoPalette())
            .toString()
    }

    /** Page background of the native bars for the given theme: Material surface (tone 99 light, tone 10 dark). */
    fun pageColor(dark: Boolean): Int = shade("n1", if (dark) 900 else 10)

    /** Colour of the bottom navigation bar for the given theme (tone 95 light, tone 20 dark). */
    fun navColor(dark: Boolean): Int = shade("n1", if (dark) 800 else 50)

    private fun shade(name: String, step: Int): Int {
        val hex = try {
            JSONObject(colors()).getJSONObject("palette").getJSONObject(name).getString(step.toString())
        } catch (_: Exception) {
            bageoPalette().getJSONObject(name).getString(step.toString())
        }
        return android.graphics.Color.parseColor(hex)
    }

    @TargetApi(Build.VERSION_CODES.S)
    private fun systemPalette(): JSONObject {
        fun pick(vararg ids: Int): JSONObject {
            val o = JSONObject()
            for (i in SHADES.indices) o.put(SHADES[i].toString(), hex(ctx.getColor(ids[i])))
            return o
        }
        return JSONObject()
            .put(
                "a1",
                pick(
                    android.R.color.system_accent1_0, android.R.color.system_accent1_10, android.R.color.system_accent1_50,
                    android.R.color.system_accent1_100, android.R.color.system_accent1_200, android.R.color.system_accent1_300,
                    android.R.color.system_accent1_400, android.R.color.system_accent1_500, android.R.color.system_accent1_600,
                    android.R.color.system_accent1_700, android.R.color.system_accent1_800, android.R.color.system_accent1_900,
                    android.R.color.system_accent1_1000,
                ),
            )
            .put(
                "a2",
                pick(
                    android.R.color.system_accent2_0, android.R.color.system_accent2_10, android.R.color.system_accent2_50,
                    android.R.color.system_accent2_100, android.R.color.system_accent2_200, android.R.color.system_accent2_300,
                    android.R.color.system_accent2_400, android.R.color.system_accent2_500, android.R.color.system_accent2_600,
                    android.R.color.system_accent2_700, android.R.color.system_accent2_800, android.R.color.system_accent2_900,
                    android.R.color.system_accent2_1000,
                ),
            )
            .put(
                "n1",
                pick(
                    android.R.color.system_neutral1_0, android.R.color.system_neutral1_10, android.R.color.system_neutral1_50,
                    android.R.color.system_neutral1_100, android.R.color.system_neutral1_200, android.R.color.system_neutral1_300,
                    android.R.color.system_neutral1_400, android.R.color.system_neutral1_500, android.R.color.system_neutral1_600,
                    android.R.color.system_neutral1_700, android.R.color.system_neutral1_800, android.R.color.system_neutral1_900,
                    android.R.color.system_neutral1_1000,
                ),
            )
            .put(
                "n2",
                pick(
                    android.R.color.system_neutral2_0, android.R.color.system_neutral2_10, android.R.color.system_neutral2_50,
                    android.R.color.system_neutral2_100, android.R.color.system_neutral2_200, android.R.color.system_neutral2_300,
                    android.R.color.system_neutral2_400, android.R.color.system_neutral2_500, android.R.color.system_neutral2_600,
                    android.R.color.system_neutral2_700, android.R.color.system_neutral2_800, android.R.color.system_neutral2_900,
                    android.R.color.system_neutral2_1000,
                ),
            )
    }

    private fun bageoPalette(): JSONObject {
        fun tones(vararg hex: String): JSONObject {
            val o = JSONObject()
            for (i in SHADES.indices) o.put(SHADES[i].toString(), hex[i])
            return o
        }
        return JSONObject()
            .put("a1", tones("#FFFFFF", "#FCFCFC", "#EEF1F1", "#D5E6E5", "#81D5CF", "#3FBDB5", "#35A099", "#2C847E", "#236864", "#1A4E4B", "#123633", "#121E1D", "#000000"))
            .put("a2", tones("#FFFFFF", "#FCFCFC", "#EFF1F1", "#DEE4E4", "#B3CBCC", "#8DB2B3", "#69999B", "#557E7F", "#436465", "#334B4C", "#233334", "#171C1D", "#000000"))
            .put("n1", tones("#FFFFFF", "#FCFCFC", "#F0F1F1", "#E1E3E3", "#C3C7C7", "#A6ACAC", "#899292", "#707979", "#586060", "#424848", "#2D3131", "#1B1C1C", "#000000"))
            .put("n2", tones("#FFFFFF", "#FCFCFC", "#F0F1F1", "#E0E3E3", "#BFC8C8", "#A0AEAE", "#829494", "#687A7A", "#536161", "#3E4949", "#2A3232", "#1A1C1C", "#000000"))
    }

    private fun hex(color: Int): String = String.format("#%06X", color and 0xFFFFFF)

    private companion object {
        /** Android shade names, light to dark: 0 is white, 1000 is black. */
        val SHADES = intArrayOf(0, 10, 50, 100, 200, 300, 400, 500, 600, 700, 800, 900, 1000)
    }
}
