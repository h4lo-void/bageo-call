// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import java.net.HttpURLConnection
import java.net.URL
import java.net.URLEncoder
import org.json.JSONObject

/**
 * Pairing by IP address, for a Bageo the phone cannot be plugged into (in Docker or on
 * another computer). The owner types Bageo's address and the 6-digit code the web
 * console made; [claim] trades the code for the pairing key, and the phone then connects
 * to `ws://<the address typed><path>?token=<key>` (see [Prefs.setIpUrl]).
 *
 * The parsing rules are plain Kotlin (no Android classes) so they are easy to test;
 * [claim] does network work and must not run on the main thread.
 */
object IpPairing {
    /** Bageo's port when none is typed. */
    const val DEFAULT_PORT = 8787
    const val CLAIM_PATH = "/api/phone-pairing/claim"
    private const val CODE_DIGITS = 6
    private val SCHEME = Regex("^[A-Za-z][A-Za-z0-9+.-]*://")
    private val NAME = Regex("^[A-Za-z0-9.-]+$")
    private val IPV6 = Regex("^[0-9A-Fa-f:.]+$")

    data class Address(val host: String, val port: Int) {
        /** The host as written in a URL: an IPv6 address goes in brackets. */
        val urlHost: String get() = if (host.contains(':')) "[$host]" else host
    }

    /** Why pairing failed, for the message shown. */
    enum class Failure { UNREACHABLE, BAD_CODE, TOO_MANY, WIFI_OFF }

    sealed class Outcome {
        data class Paired(val wsUrl: String) : Outcome()
        data class Failed(val failure: Failure) : Outcome()
    }

    /**
     * `192.168.1.20`, `192.168.1.20:9000`, `http://192.168.1.20:8787/`, a host name or
     * `[IPv6]:port`. Blanks around it, a scheme and a path are dropped; no port means
     * [DEFAULT_PORT]. Null when it is not an address.
     */
    fun parseAddress(raw: String): Address? {
        val s = raw.trim().replaceFirst(SCHEME, "")
            .substringBefore('/').substringBefore('?').substringBefore('#').trim()
        if (s.isEmpty()) return null
        val host: String
        val portText: String?
        when {
            s.startsWith("[") -> {
                val end = s.indexOf(']')
                if (end < 2) return null
                host = s.substring(1, end)
                val rest = s.substring(end + 1)
                portText = when {
                    rest.isEmpty() -> null
                    rest.startsWith(":") -> rest.substring(1)
                    else -> return null
                }
                if (!IPV6.matches(host)) return null
            }
            s.count { it == ':' } > 1 -> {
                host = s
                portText = null
                if (!IPV6.matches(host)) return null
            }
            else -> {
                host = s.substringBefore(':')
                portText = if (s.contains(':')) s.substringAfter(':') else null
                if (!NAME.matches(host)) return null
            }
        }
        val port = if (portText == null) {
            DEFAULT_PORT
        } else {
            portText.toIntOrNull()?.takeIf { it in 1..65535 } ?: return null
        }
        return Address(host, port)
    }

    /** The code without blanks or dashes; null unless it is 6 digits. */
    fun normalizeCode(raw: String): String? =
        raw.filterNot { it.isWhitespace() || it == '-' }.takeIf { it.length == CODE_DIGITS && it.all { c -> c in '0'..'9' } }

    fun claimUrl(address: Address): String = "http://${address.urlHost}:${address.port}$CLAIM_PATH"

    /**
     * Where the phone connects: the address typed, the path Bageo gave (`/ws` when it gave
     * none or an odd one) and the pairing key.
     */
    fun wsUrl(address: Address, path: String?, token: String): String {
        val p = path?.trim()?.takeIf { it.startsWith("/") && !it.contains('?') && !it.contains('#') } ?: "/ws"
        return "ws://${address.urlHost}:${address.port}$p?token=${URLEncoder.encode(token.trim(), "UTF-8")}"
    }

    /** What an error answer means: Bageo's `code`, else the HTTP status. */
    fun failureFor(status: Int, code: String?): Failure = when {
        code == "too_many_tries" || status == 429 -> Failure.TOO_MANY
        code == "phone_wifi_off" -> Failure.WIFI_OFF
        code == "wrong_code" || code == "no_code" || code == "bad_request" -> Failure.BAD_CODE
        status == 403 -> Failure.BAD_CODE
        else -> Failure.UNREACHABLE
    }

    /** Trade [code] for the pairing key at [address]. Blocks: not on the main thread. */
    fun claim(address: Address, code: String): Outcome = try {
        val conn = URL(claimUrl(address)).openConnection() as HttpURLConnection
        try {
            conn.requestMethod = "POST"
            conn.connectTimeout = 6_000
            conn.readTimeout = 10_000
            conn.instanceFollowRedirects = false
            conn.doOutput = true
            conn.setRequestProperty("Content-Type", "application/json")
            conn.setRequestProperty("Accept", "application/json")
            val body = JSONObject().put("code", code).toString().toByteArray(Charsets.UTF_8)
            conn.outputStream.use { it.write(body) }
            val status = conn.responseCode
            val stream = if (status in 200..299) conn.inputStream else conn.errorStream
            val text = stream?.bufferedReader(Charsets.UTF_8)?.use { it.readText() }.orEmpty()
            val json = try {
                JSONObject(text)
            } catch (_: Exception) {
                null
            }
            val token = json?.optString("token").orEmpty()
            if (status == 200 && json != null && json.optBoolean("ok") && token.isNotBlank()) {
                Outcome.Paired(wsUrl(address, json.optString("path"), token))
            } else {
                Outcome.Failed(failureFor(status, json?.optString("code")))
            }
        } finally {
            conn.disconnect()
        }
    } catch (_: Exception) {
        Outcome.Failed(Failure.UNREACHABLE)
    }
}
