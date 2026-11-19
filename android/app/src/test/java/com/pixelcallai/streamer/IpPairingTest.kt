// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import com.pixelcallai.streamer.IpPairing.Address
import com.pixelcallai.streamer.IpPairing.Failure
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class IpPairingTest {
    @Test
    fun address_plain_ip_uses_the_default_port() {
        assertEquals(Address("192.168.1.20", 8787), IpPairing.parseAddress("192.168.1.20"))
    }

    @Test
    fun address_with_a_port() {
        assertEquals(Address("192.168.1.20", 9000), IpPairing.parseAddress("192.168.1.20:9000"))
        assertEquals(Address("bageo-host", 8787), IpPairing.parseAddress("bageo-host"))
    }

    @Test
    fun address_drops_blanks_scheme_and_path() {
        assertEquals(Address("192.168.1.20", 8787), IpPairing.parseAddress("  192.168.1.20 \n"))
        assertEquals(Address("192.168.1.20", 8787), IpPairing.parseAddress("http://192.168.1.20"))
        assertEquals(Address("192.168.1.20", 9000), IpPairing.parseAddress("http://192.168.1.20:9000/calls/"))
        assertEquals(Address("192.168.1.20", 8787), IpPairing.parseAddress("HTTPS://192.168.1.20/"))
        assertEquals(Address("192.168.1.20", 8787), IpPairing.parseAddress("ws://192.168.1.20:8787/ws?token=x"))
    }

    @Test
    fun address_ipv6() {
        assertEquals(Address("fd00::20", 9000), IpPairing.parseAddress("[fd00::20]:9000"))
        assertEquals(Address("fd00::20", 8787), IpPairing.parseAddress("fd00::20"))
        assertEquals("[fd00::20]", Address("fd00::20", 8787).urlHost)
    }

    @Test
    fun address_rejects_what_is_not_an_address() {
        assertNull(IpPairing.parseAddress(""))
        assertNull(IpPairing.parseAddress("   "))
        assertNull(IpPairing.parseAddress("http://"))
        assertNull(IpPairing.parseAddress("192.168.1.20:"))
        assertNull(IpPairing.parseAddress("192.168.1.20:0"))
        assertNull(IpPairing.parseAddress("192.168.1.20:70000"))
        assertNull(IpPairing.parseAddress("192.168.1.20:abc"))
        assertNull(IpPairing.parseAddress("192.168 .1.20"))
        assertNull(IpPairing.parseAddress("user@192.168.1.20"))
        assertNull(IpPairing.parseAddress("[fd00::20"))
    }

    @Test
    fun code_is_six_digits() {
        assertEquals("123456", IpPairing.normalizeCode("123456"))
        assertEquals("123456", IpPairing.normalizeCode(" 123 456 "))
        assertEquals("012345", IpPairing.normalizeCode("012-345"))
        assertNull(IpPairing.normalizeCode("12345"))
        assertNull(IpPairing.normalizeCode("1234567"))
        assertNull(IpPairing.normalizeCode("12a456"))
        assertNull(IpPairing.normalizeCode(""))
    }

    @Test
    fun claim_and_connection_urls_use_the_address_typed() {
        val a = Address("192.168.1.20", 9000)
        assertEquals("http://192.168.1.20:9000/api/phone-pairing/claim", IpPairing.claimUrl(a))
        assertEquals("ws://192.168.1.20:9000/ws?token=k3y-_x", IpPairing.wsUrl(a, "/ws", "k3y-_x"))
        assertEquals("ws://192.168.1.20:9000/ws?token=k3y", IpPairing.wsUrl(a, null, "k3y"))
        assertEquals("ws://192.168.1.20:9000/ws?token=k3y", IpPairing.wsUrl(a, "", "k3y"))
        assertEquals("ws://192.168.1.20:9000/ws?token=k3y", IpPairing.wsUrl(a, "ws?x=1", "k3y"))
        assertEquals("ws://192.168.1.20:9000/phone/ws?token=k3y", IpPairing.wsUrl(a, "/phone/ws", "k3y"))
        assertEquals("ws://192.168.1.20:9000/ws?token=a%2Bb%3D", IpPairing.wsUrl(a, "/ws", "a+b="))
        assertEquals(
            "ws://[fd00::20]:8787/ws?token=k3y",
            IpPairing.wsUrl(Address("fd00::20", 8787), "/ws", "k3y"),
        )
    }

    @Test
    fun error_answers() {
        assertEquals(Failure.BAD_CODE, IpPairing.failureFor(403, "wrong_code"))
        assertEquals(Failure.BAD_CODE, IpPairing.failureFor(403, "no_code"))
        assertEquals(Failure.BAD_CODE, IpPairing.failureFor(400, "bad_request"))
        assertEquals(Failure.TOO_MANY, IpPairing.failureFor(429, "too_many_tries"))
        assertEquals(Failure.TOO_MANY, IpPairing.failureFor(429, null))
        assertEquals(Failure.WIFI_OFF, IpPairing.failureFor(409, "phone_wifi_off"))
        // An older Bageo, or something else at that address.
        assertEquals(Failure.UNREACHABLE, IpPairing.failureFor(404, null))
        assertEquals(Failure.UNREACHABLE, IpPairing.failureFor(502, ""))
    }

    @Test
    fun the_ip_address_is_tried_after_usb_before_the_usb_wifi_addresses() {
        val usb = "ws://127.0.0.1:8765"
        val ip = "ws://192.168.1.20:9000/ws?token=k1"
        val wifi = listOf("ws://192.168.1.20:8788/ws?token=k1")
        assertEquals(listOf(ip) + wifi, ConnectionPlan.networkUrls(ip, wifi))
        assertEquals(wifi, ConnectionPlan.networkUrls(null, wifi))
        assertEquals(listOf(ip), ConnectionPlan.networkUrls(ip, listOf(ip)))
        assertEquals(listOf(usb, ip) + wifi, ConnectionPlan.candidates(usb, usb, ConnectionPlan.networkUrls(ip, wifi)))
    }
}
