// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ConnectionPlanTest {
    @Test
    fun candidates_usb_first_then_wifi_or_manual_alone() {
        val usb = "ws://127.0.0.1:8765"
        val wifi = listOf("ws://192.168.1.20:8788/ws?token=k1", "ws://bageo.local:8788/ws?token=k1")
        assertEquals(listOf(usb) + wifi, ConnectionPlan.candidates(usb, usb, wifi))
        assertEquals(listOf(usb), ConnectionPlan.candidates(usb, usb, listOf(usb)))
        assertEquals(
            listOf("ws://192.168.1.50:8787/ws"),
            ConnectionPlan.candidates(usb, "ws://192.168.1.50:8787/ws", wifi),
        )
    }

    @Test
    fun nextWifiUrls_null_keeps_empty_clears_invalid_dropped_and_capped() {
        val kept = listOf("ws://192.168.1.20:8788/ws?token=k1")
        val valid = { u: String -> u.startsWith("ws://") || u.startsWith("wss://") }
        assertEquals(kept, ConnectionPlan.nextWifiUrls(kept, null, valid))
        assertEquals(emptyList<String>(), ConnectionPlan.nextWifiUrls(kept, emptyList(), valid))
        val mixed = listOf(
            "  ws://192.168.1.20:8788/ws?token=k2 ",
            "http://192.168.1.20:8788/ws",
            "ws://192.168.1.20:8788/ws?token=k2",
            "wss://app.example/ws?token=k2",
            "ws://192.0.2.10:8788/ws?token=k2",
            "ws://192.0.2.11:8788/ws?token=k2",
            "ws://192.0.2.12:8788/ws?token=k2",
        )
        assertEquals(
            listOf(
                "ws://192.168.1.20:8788/ws?token=k2",
                "wss://app.example/ws?token=k2",
                "ws://192.0.2.10:8788/ws?token=k2",
                "ws://192.0.2.11:8788/ws?token=k2",
            ),
            ConnectionPlan.nextWifiUrls(kept, mixed, valid),
        )
    }

    @Test
    fun nextAfterFailure_wraps_and_pickActive_prefers_lastGood_when_present() {
        val cands = listOf("ws://127.0.0.1:8765", "ws://192.168.1.20:8788/ws", "ws://bageo.local:8788/ws")
        assertEquals("ws://192.168.1.20:8788/ws", ConnectionPlan.nextAfterFailure(cands, "ws://127.0.0.1:8765"))
        assertEquals("ws://127.0.0.1:8765", ConnectionPlan.nextAfterFailure(cands, "ws://bageo.local:8788/ws"))
        assertEquals("ws://127.0.0.1:8765", ConnectionPlan.nextAfterFailure(cands, "ws://missing:80"))
        assertEquals("ws://missing:80", ConnectionPlan.nextAfterFailure(emptyList(), "ws://missing:80"))

        assertEquals("ws://192.168.1.20:8788/ws", ConnectionPlan.pickActive("ws://192.168.1.20:8788/ws", cands))
        assertEquals("ws://127.0.0.1:8765", ConnectionPlan.pickActive("ws://gone:8788/ws", cands))
        assertEquals("ws://127.0.0.1:8765", ConnectionPlan.pickActive(null, cands))
        assertNull(ConnectionPlan.pickActive("ws://127.0.0.1:8765", emptyList()))
    }

    @Test
    fun hostPort_parses_ports_and_rejects_bad_urls() {
        assertEquals("127.0.0.1" to 8765, ConnectionPlan.hostPort(" ws://127.0.0.1:8765 "))
        assertEquals("app.example" to 443, ConnectionPlan.hostPort("wss://app.example/ws?token=k"))
        assertEquals("app.example" to 80, ConnectionPlan.hostPort("ws://app.example/ws"))
        assertNull(ConnectionPlan.hostPort("not a url"))
        assertNull(ConnectionPlan.hostPort("ws:///nohost"))
    }
}
