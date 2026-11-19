// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import java.net.URI

/**
 * Which agent address the phone tries, and how the Wi-Fi addresses the agent sends are
 * kept. Plain Kotlin on purpose (no Android classes), so the rules are easy to test.
 */
object ConnectionPlan {
    /** Enough for "LAN IP, then name.local"; more would only slow down a failed round. */
    const val MAX_WIFI_URLS = 4

    /**
     * Addresses to try, in order. An address typed by hand ([agentUrl] differs from
     * [usbUrl]) is the only one used. Otherwise: USB first, then the Wi-Fi addresses.
     */
    fun candidates(usbUrl: String, agentUrl: String, wifiUrls: List<String>): List<String> =
        if (agentUrl != usbUrl) listOf(agentUrl) else (listOf(usbUrl) + wifiUrls).distinct()

    /**
     * The addresses tried after USB: the one paired by IP address ([ipUrl], typed by the
     * owner) first, then the Wi-Fi addresses Bageo sent over USB.
     */
    fun networkUrls(ipUrl: String?, wifiUrls: List<String>): List<String> =
        (listOfNotNull(ipUrl) + wifiUrls).distinct()

    /**
     * The stored Wi-Fi addresses after an `agent_info`. [incoming] null means the message
     * had no `wifi` field (an older agent): keep what is stored. An empty list clears it.
     * Entries that are not valid addresses are dropped.
     */
    fun nextWifiUrls(
        current: List<String>,
        incoming: List<String>?,
        isValid: (String) -> Boolean,
    ): List<String> =
        if (incoming == null) {
            current
        } else {
            incoming.map { it.trim() }.filter(isValid).distinct().take(MAX_WIFI_URLS)
        }

    /** The address after a failed attempt on [failed]: the next one, wrapping around. */
    fun nextAfterFailure(candidates: List<String>, failed: String): String {
        if (candidates.isEmpty()) return failed
        val i = candidates.indexOf(failed)
        return if (i < 0) candidates[0] else candidates[(i + 1) % candidates.size]
    }

    /** The address a new connection should use: the one that last worked, else the first. */
    fun pickActive(lastGood: String?, candidates: List<String>): String? =
        lastGood?.takeIf { it in candidates } ?: candidates.firstOrNull()

    fun hostPort(url: String): Pair<String, Int>? = try {
        val u = URI(url.trim())
        val host = u.host
        if (host.isNullOrBlank()) {
            null
        } else {
            val port = if (u.port >= 0) u.port else if (u.scheme == "wss") 443 else 80
            host to port
        }
    } catch (_: Exception) {
        null
    }
}
