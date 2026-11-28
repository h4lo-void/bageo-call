// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PhoneLinkAuthTest {
    private val key = "00".repeat(32)
    private val cnonce = "11".repeat(32)
    private val snonce = "22".repeat(32)

    @Test
    fun proofs_match_bageo_known_answer() {
        // The same values as phone_link.rs `link_handshake_proofs_match_both_ways`.
        assertEquals(
            "edae13eb159e4215e1f2e7735b5445e0b748df7a1abc3f26f2f713eee7d9bb58",
            PhoneLinkAuth.hostProof(key, cnonce, snonce),
        )
        assertEquals(
            "0cd686cff360ad2b0db8c08f5ebb6d0116f1dfb408c127af87f1d7725ac73b8a",
            PhoneLinkAuth.phoneProof(key, cnonce, snonce),
        )
    }

    @Test
    fun handshake_accepts_only_the_right_host_proof() {
        val ok = PhoneLinkAuth.Session(key, cnonce)
        assertEquals(mapOf("x-bageo-link" to "1", "x-bageo-link-cnonce" to cnonce), ok.headers())
        assertNull("no proof before the host proved itself", ok.helloProof())
        assertTrue(ok.verifyHost(snonce, PhoneLinkAuth.hostProof(key, cnonce, snonce)))
        assertEquals(PhoneLinkAuth.phoneProof(key, cnonce, snonce), ok.helloProof())

        val s = PhoneLinkAuth.Session(key, cnonce)
        assertFalse("missing", s.verifyHost(null, null))
        assertFalse("no proof", s.verifyHost(snonce, ""))
        assertFalse("bad nonce", s.verifyHost("xyz", PhoneLinkAuth.hostProof(key, cnonce, snonce)))
        assertFalse("the phone proof is not the host proof", s.verifyHost(snonce, PhoneLinkAuth.phoneProof(key, cnonce, snonce)))
        assertFalse("another key", s.verifyHost(snonce, PhoneLinkAuth.hostProof("ff".repeat(32), cnonce, snonce)))
        assertNull(s.helloProof())
    }

    @Test
    fun empty_key_never_connects_in_standalone() {
        assertTrue(PhoneLinkAuth.plan(standalone = true, bridgeKey = "", storedKey = null) is PhoneLinkAuth.Plan.Refuse)
        assertTrue(PhoneLinkAuth.plan(standalone = true, bridgeKey = "short", storedKey = key) is PhoneLinkAuth.Plan.Refuse)
        assertTrue(PhoneLinkAuth.plan(standalone = true, bridgeKey = key, storedKey = null) is PhoneLinkAuth.Plan.Check)
        // Bridge flavor: as before until Bageo handed a key, then always the check.
        assertTrue(PhoneLinkAuth.plan(standalone = false, bridgeKey = null, storedKey = null) is PhoneLinkAuth.Plan.Legacy)
        assertTrue(PhoneLinkAuth.plan(standalone = false, bridgeKey = null, storedKey = key) is PhoneLinkAuth.Plan.Check)
        assertTrue(PhoneLinkAuth.plan(standalone = false, bridgeKey = key, storedKey = null) is PhoneLinkAuth.Plan.Legacy)
    }

    @Test
    fun agent_info_wifi_urls_must_be_pinned_wss_on_the_local_network() {
        val pin = "a1".repeat(32)
        assertTrue(PhoneLinkAuth.isAcceptableWifiUrl("wss://192.168.1.20:8788/ws?token=k&pin=$pin"))
        assertTrue(PhoneLinkAuth.isAcceptableWifiUrl("wss://10.0.0.5:8788/ws?token=k&pin=$pin"))
        assertTrue(PhoneLinkAuth.isAcceptableWifiUrl("wss://172.20.1.2:8788/ws?token=k&pin=$pin"))
        assertTrue(PhoneLinkAuth.isAcceptableWifiUrl("wss://169.254.3.4:8788/ws?token=k&pin=$pin"))
        assertTrue(PhoneLinkAuth.isAcceptableWifiUrl("wss://bageo.local:8788/ws?token=k&pin=$pin"))
        assertTrue(PhoneLinkAuth.isAcceptableWifiUrl("wss://[fd00::20]:8788/ws?token=k&pin=$pin"))
        assertTrue(PhoneLinkAuth.isAcceptableWifiUrl("wss://[fe80::1]:8788/ws?token=k&pin=$pin"))
        assertFalse("plain", PhoneLinkAuth.isAcceptableWifiUrl("ws://192.168.1.20:8788/ws?token=k"))
        assertFalse("no pin", PhoneLinkAuth.isAcceptableWifiUrl("wss://192.168.1.20:8788/ws?token=k"))
        assertFalse("public address", PhoneLinkAuth.isAcceptableWifiUrl("wss://203.0.113.9:8788/ws?token=k&pin=$pin"))
        assertFalse("a name", PhoneLinkAuth.isAcceptableWifiUrl("wss://evil.example:8788/ws?token=k&pin=$pin"))
        assertFalse("not quite private", PhoneLinkAuth.isAcceptableWifiUrl("wss://172.32.0.1:8788/ws?token=k&pin=$pin"))
        assertFalse("loopback", PhoneLinkAuth.isAcceptableWifiUrl("wss://127.0.0.1:8788/ws?token=k&pin=$pin"))
        assertFalse("global IPv6", PhoneLinkAuth.isAcceptableWifiUrl("wss://[2001:db8::1]:8788/ws?token=k&pin=$pin"))

        assertTrue(PhoneLinkAuth.isAcceptableConsoleUrl("http://192.168.1.20:8787/calls/"))
        assertTrue(PhoneLinkAuth.isAcceptableConsoleUrl("https://bageo.local/calls/"))
        assertFalse(PhoneLinkAuth.isAcceptableConsoleUrl("https://app.example/calls/"))
        assertFalse(PhoneLinkAuth.isAcceptableConsoleUrl("javascript:alert(1)"))
    }

    @Test
    fun stored_list_drops_plain_ws_when_a_pinned_one_exists() {
        val pin = "a1".repeat(32)
        val pinned = "wss://192.168.1.20:8788/ws?token=k&pin=$pin"
        val plain = "ws://192.168.1.20:8788/ws?token=k"
        assertEquals(listOf(pinned), PhoneLinkAuth.dropPlainWhenPinned(listOf(pinned, plain)))
        assertEquals(listOf(plain), PhoneLinkAuth.dropPlainWhenPinned(listOf(plain)))
    }

    @Test
    fun verified_means_checked_or_pinned() {
        val pin = "a1".repeat(32)
        assertTrue(PhoneLinkAuth.verified(true, "ws://127.0.0.1:8765"))
        assertFalse(PhoneLinkAuth.verified(false, "ws://127.0.0.1:8765"))
        assertTrue(PhoneLinkAuth.verified(false, "wss://192.168.1.20:8788/ws?token=k&pin=$pin"))
    }
}
