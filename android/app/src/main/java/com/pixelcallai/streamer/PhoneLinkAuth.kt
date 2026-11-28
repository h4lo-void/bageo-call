// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import java.net.URI

/**
 * The phone link check: before the app says anything, Bageo must show it holds the same
 * link key, and the app's `hello` shows the same back (Bageo's `phone_link.rs`).
 *
 * The key is 64 lowercase hex characters (32 random bytes); the HMAC key is that text.
 * The `bridge` flavor gets it from Bageo over USB with the start of [CallAudioService]
 * and keeps it ([Prefs.linkKey]); the standalone flavor uses the key its in-app Bageo
 * makes at each start ([EmbeddedAgent.bridgeKey]).
 *
 * 1. Request headers [HEADER_LINK] `1` and [HEADER_CNONCE] (32 random bytes, hex).
 * 2. Bageo's 101 answer: [HEADER_SNONCE] and [HEADER_PROOF] =
 *    `HMAC-SHA256(K, "bageo-link-v1|host|" + cnonce + "|" + snonce)`.
 * 3. A wrong or missing proof: close without a word ([Session.verifyHost]).
 * 4. The hello carries `link_proof` = `HMAC-SHA256(K, "bageo-link-v1|phone|" + cnonce + "|" + snonce)`.
 *
 * Plain Kotlin (no Android classes), so the rules are easy to test.
 */
object PhoneLinkAuth {
    const val HEADER_LINK = "x-bageo-link"
    const val HEADER_CNONCE = "x-bageo-link-cnonce"
    const val HEADER_SNONCE = "x-bageo-link-snonce"
    const val HEADER_PROOF = "x-bageo-link-proof"

    /** The intent extra Bageo hands the key in (`usb_bridge.rs`). */
    const val EXTRA_LINK_KEY = "link_key"

    private val HEX_64 = Regex("^[0-9a-f]{64}$")

    fun isValidKey(key: String?): Boolean = key != null && HEX_64.matches(key)

    fun normalizeKey(raw: String?): String? = raw?.trim()?.lowercase()?.takeIf { isValidKey(it) }

    fun hostProof(key: String, cnonce: String, snonce: String): String =
        TlsPinning.hmacSha256Hex(key, "bageo-link-v1|host|$cnonce|$snonce")

    fun phoneProof(key: String, cnonce: String, snonce: String): String =
        TlsPinning.hmacSha256Hex(key, "bageo-link-v1|phone|$cnonce|$snonce")

    /** One connection's half of the check. */
    class Session(private val key: String, val cnonce: String = TlsPinning.randomNonceHex(32)) {
        @Volatile private var snonce: String? = null

        fun headers(): Map<String, String> = mapOf(HEADER_LINK to "1", HEADER_CNONCE to cnonce)

        /** True when Bageo's 101 answer proves it holds the key (constant-time compare). */
        fun verifyHost(snonce: String?, proof: String?): Boolean {
            val s = snonce?.trim()?.lowercase()
            if (s == null || !HEX_64.matches(s) || proof.isNullOrBlank()) return false
            val ok = TlsPinning.constantTimeEquals(proof, hostProof(key, cnonce, s))
            if (ok) this.snonce = s
            return ok
        }

        /** The hello's `link_proof`; null before [verifyHost] succeeded. */
        fun helloProof(): String? = snonce?.let { phoneProof(key, cnonce, it) }
    }

    /** What the app does on one connection. */
    sealed class Plan {
        /** Do not connect at all (standalone without its key yet). */
        object Refuse : Plan()

        /** No key known: connect as before, unverified. */
        object Legacy : Plan()

        /** Run the check; Bageo must prove itself. */
        class Check(val session: Session) : Plan()
    }

    /**
     * Standalone: always the check with the in-app key, and no connection while there is
     * none. Bridge flavor: once a key was received, the check on every connection (until
     * the owner forgets the pairing); before that, as before.
     */
    fun plan(standalone: Boolean, bridgeKey: String?, storedKey: String?): Plan {
        val key = if (standalone) normalizeKey(bridgeKey) else normalizeKey(storedKey)
        return when {
            key != null -> Plan.Check(Session(key))
            standalone -> Plan.Refuse
            else -> Plan.Legacy
        }
    }

    /**
     * A connection Bageo's word can be taken on (for `agent_info`): it passed the check,
     * or it is `wss://` pinned to a certificate the app got from a checked Bageo.
     */
    fun verified(checkedOk: Boolean, url: String): Boolean =
        checkedOk || TlsPinning.extractPin(url) != null

    /**
     * A host on the local network: a private or link-local IP address written out, or a
     * `.local` name. No name is looked up.
     */
    fun isLocalHost(rawHost: String?): Boolean {
        val host = rawHost?.trim()?.lowercase()?.removePrefix("[")?.removeSuffix("]")?.trimEnd('.') ?: return false
        if (host.isEmpty()) return false
        if (host.endsWith(".local") && host.length > ".local".length) {
            return host.removeSuffix(".local").all { it.isLetterOrDigit() || it == '-' || it == '.' }
        }
        val v4 = host.split('.')
        if (v4.size == 4 && v4.all { p -> p.isNotEmpty() && p.length <= 3 && p.all { it in '0'..'9' } }) {
            val b = v4.map { it.toInt() }
            if (b.any { it > 255 }) return false
            return b[0] == 10 ||
                (b[0] == 172 && b[1] in 16..31) ||
                (b[0] == 192 && b[1] == 168) ||
                (b[0] == 169 && b[1] == 254)
        }
        if (host.contains(':') && host.all { it in '0'..'9' || it in 'a'..'f' || it == ':' || it == '.' || it == '%' }) {
            val first = host.substringBefore(':')
            val v = first.toIntOrNull(16) ?: return false
            return (v and 0xfe00) == 0xfc00 || (v and 0xffc0) == 0xfe80
        }
        return false
    }

    /** A Wi-Fi address from `agent_info`: `wss://` with a certificate pin, on the local network. */
    fun isAcceptableWifiUrl(url: String): Boolean {
        val u = url.trim()
        if (!u.startsWith("wss://") || TlsPinning.extractPin(u) == null) return false
        return try {
            isLocalHost(URI(u).host)
        } catch (_: Exception) {
            false
        }
    }

    /** The console address from `agent_info`: `http(s)://` on the local network. */
    fun isAcceptableConsoleUrl(url: String): Boolean {
        val u = url.trim()
        if (!u.startsWith("http://") && !u.startsWith("https://")) return false
        return try {
            isLocalHost(URI(u).host)
        } catch (_: Exception) {
            false
        }
    }

    /** `wss://` with a certificate pin. */
    fun isPinned(url: String): Boolean = TlsPinning.extractPin(url) != null

    /** Stored network addresses: only the pinned `wss://` ones are kept. */
    fun pinnedOnly(urls: List<String>): List<String> = urls.filter { isPinned(it) }

    /**
     * May an unchecked connection to [url] go ahead (say hello, take commands)? Not while
     * the phone has a pinned network pairing ([candidates]) and [url] itself is not pinned:
     * the phone then has a Bageo it can check and uses that one instead.
     */
    fun uncheckedAllowed(url: String, candidates: List<String>): Boolean =
        isPinned(url) || candidates.none { isPinned(it) }
}
