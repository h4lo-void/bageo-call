// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import java.net.URI
import java.net.URLDecoder
import java.security.MessageDigest
import java.security.SecureRandom
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import java.util.concurrent.atomic.AtomicReference
import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLParameters
import javax.net.ssl.SSLSocketFactory
import javax.net.ssl.TrustManager
import javax.net.ssl.X509TrustManager
import org.java_websocket.client.WebSocketClient

/**
 * SHA-256 leaf-certificate pinning and channel-bound pairing MAC helpers for
 * `wss://` WebSocket connections and `https://` IP pairing on a local network.
 *
 * Pure JVM / `javax.net.ssl` (no `android.*` imports) so JUnit tests exercise
 * the exact production pinning and HMAC logic.
 */
object TlsPinning {
    private val HEX_64 = Regex("^[0-9a-fA-F]{64}$")
    private val rng = SecureRandom()

    /** Extract a validated lowercase 64-hex `?pin=` query parameter from [url], or null. */
    fun extractPin(url: String): String? {
        val u = url.trim()
        if (!u.startsWith("wss://", ignoreCase = true)) return null
        val query = try {
            URI(u).rawQuery
        } catch (_: Exception) {
            u.substringAfter('?', "").substringBefore('#')
        } ?: return null
        for (part in query.split('&')) {
            val eq = part.indexOf('=')
            if (eq <= 0) continue
            val k = part.substring(0, eq)
            if (k != "pin") continue
            val v = try {
                URLDecoder.decode(part.substring(eq + 1), "UTF-8").trim()
            } catch (_: Exception) {
                part.substring(eq + 1).trim()
            }
            if (isValidPin(v)) return v.lowercase()
        }
        return null
    }

    fun isValidPin(pin: String?): Boolean = pin != null && HEX_64.matches(pin.trim())

    fun sha256Hex(bytes: ByteArray): String =
        toHex(MessageDigest.getInstance("SHA-256").digest(bytes))

    fun randomNonceHex(numBytes: Int = 16): String {
        val buf = ByteArray(numBytes)
        rng.nextBytes(buf)
        return toHex(buf)
    }

    fun hmacSha256Hex(keyUtf8: String, messageUtf8: String): String {
        val mac = Mac.getInstance("HmacSHA256")
        mac.init(SecretKeySpec(keyUtf8.toByteArray(Charsets.UTF_8), "HmacSHA256"))
        return toHex(mac.doFinal(messageUtf8.toByteArray(Charsets.UTF_8)))
    }

    /** Constant-time case-insensitive hex equality check. */
    fun constantTimeEquals(a: String?, b: String?): Boolean {
        if (a == null || b == null) return false
        val ba = a.trim().lowercase().toByteArray(Charsets.US_ASCII)
        val bb = b.trim().lowercase().toByteArray(Charsets.US_ASCII)
        return ba.isNotEmpty() && MessageDigest.isEqual(ba, bb)
    }

    /** Client -> server channel-bound pairing MAC (`bageo-claim-req-v1`). */
    fun claimRequestMac(code: String, cnonce: String, certPinSha256: String): String =
        hmacSha256Hex(
            code.trim(),
            "bageo-claim-req-v1:${cnonce.trim()}:${certPinSha256.trim().lowercase()}",
        )

    /** Verify server -> client pairing proof (`bageo-claim-ack-v1`). */
    fun verifyClaimProof(
        code: String,
        cnonce: String,
        certPinSha256: String,
        token: String,
        proof: String?,
    ): Boolean {
        if (proof.isNullOrBlank() || !isValidPin(certPinSha256) || token.isBlank()) return false
        val expected = hmacSha256Hex(
            code.trim(),
            "bageo-claim-ack-v1:${cnonce.trim()}:${certPinSha256.trim().lowercase()}:${token.trim()}",
        )
        return constantTimeEquals(proof, expected)
    }

    /** Build an [SSLSocketFactory] that accepts ONLY the leaf certificate matching [expectedPinHex]. */
    fun pinnedSocketFactory(expectedPinHex: String): SSLSocketFactory {
        val expectedBytes = hexToBytes(expectedPinHex.trim().lowercase())
        val tm = object : X509TrustManager {
            override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?) {
                throw CertificateException("client certificates not used")
            }

            override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?) {
                if (chain.isNullOrEmpty()) {
                    throw CertificateException("empty server certificate chain")
                }
                val leaf = chain[0]
                leaf.checkValidity()
                val actual = MessageDigest.getInstance("SHA-256").digest(leaf.encoded)
                if (!MessageDigest.isEqual(actual, expectedBytes)) {
                    throw CertificateException("server certificate SHA-256 pin mismatch")
                }
            }

            override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
        }
        val ctx = SSLContext.getInstance("TLS")
        ctx.init(null, arrayOf<TrustManager>(tm), rng)
        return ctx.socketFactory
    }

    /**
     * Attach a pinned [SSLSocketFactory] when [url] is `wss://…&pin=<64-hex>`.
     * Standard `wss://` URLs without `?pin=` keep the platform CA trust store.
     */
    fun attachIfPinned(ws: WebSocketClient, url: String) {
        val pin = extractPin(url) ?: return
        ws.setSocketFactory(pinnedSocketFactory(pin))
    }

    /**
     * Configure [SSLParameters] for [WebSocketClient]: when `?pin=<64-hex>` pins the
     * exact leaf certificate on a dynamic LAN IP, disable CA hostname matching;
     * otherwise enforce standard HTTPS endpoint identification.
     */
    fun configureSslParameters(params: SSLParameters, url: String) {
        if (extractPin(url) != null) {
            params.endpointIdentificationAlgorithm = null
        } else {
            params.endpointIdentificationAlgorithm = "HTTPS"
        }
    }

    /**
     * Build an [SSLContext] for the initial HTTPS pairing handshake (`IpPairing.claim`)
     * that records the server's leaf certificate SHA-256 digest in [observedPin] so the
     * caller can cryptographically bind `code_mac` and verify `proof` against that exact
     * certificate before trusting any token.
     */
    fun pairingHandshakeContext(observedPin: AtomicReference<String?>): SSLContext {
        val tm = object : X509TrustManager {
            override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?) {
                throw CertificateException("client certificates not used")
            }

            override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?) {
                if (chain.isNullOrEmpty()) {
                    throw CertificateException("empty server certificate chain")
                }
                val leaf = chain[0]
                leaf.checkValidity()
                observedPin.set(sha256Hex(leaf.encoded))
            }

            override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
        }
        val ctx = SSLContext.getInstance("TLS")
        ctx.init(null, arrayOf<TrustManager>(tm), rng)
        return ctx
    }

    private fun toHex(bytes: ByteArray): String {
        val chars = CharArray(bytes.size * 2)
        val hex = "0123456789abcdef"
        for (i in bytes.indices) {
            val v = bytes[i].toInt() and 0xff
            chars[i * 2] = hex[v ushr 4]
            chars[i * 2 + 1] = hex[v and 0x0f]
        }
        return String(chars)
    }

    private fun hexToBytes(hex: String): ByteArray {
        require(hex.length % 2 == 0) { "hex length must be even" }
        return ByteArray(hex.length / 2) { i ->
            hex.substring(i * 2, i * 2 + 2).toInt(16).toByte()
        }
    }
}
