// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * A ring must not be taken for a `place_call` that has not started, and the
 * media hello must say which call it is.
 */
class OutboundVersusIncomingTest {
    @Test
    fun a_ring_cancels_a_place_call_that_has_not_dialed() {
        assertTrue(CallAudioService.shouldCancelPendingForRing("call-1", false))
    }

    @Test
    fun a_place_call_already_dialing_is_not_cancelled_when_a_call_rings() {
        assertFalse(CallAudioService.shouldCancelPendingForRing("call-1", true))
        assertFalse(CallAudioService.shouldCancelPendingForRing(null, false))
        assertFalse(CallAudioService.shouldCancelPendingForRing("", false))
        assertFalse(CallAudioService.shouldCancelPendingForRing("  ", false))
    }

    @Test
    fun an_answered_ring_is_not_the_outbound_call() {
        assertFalse(
            CallAudioService.isAgentOutboundCall(
                answeredRing = true,
                pendingOutbound = true,
                systemOutgoing = true,
                sdkInt = 29,
            ),
        )
        assertEquals(
            CallAudioService.OffhookKind.AI,
            CallAudioService.classifyOffhook(agentCallback = true, answeredRing = true, ownCall = true),
        )
        assertEquals(
            CallAudioService.OffhookKind.AI,
            CallAudioService.classifyOffhook(agentCallback = false, answeredRing = true, ownCall = true),
        )
    }

    @Test
    fun android_10_outbound_needs_the_system_direction() {
        // API 29 is Android 10, the first release with Call.Details.getCallDirection().
        assertTrue(
            CallAudioService.isAgentOutboundCall(
                answeredRing = false,
                pendingOutbound = true,
                systemOutgoing = true,
                sdkInt = 29,
            ),
        )
        assertFalse(
            CallAudioService.isAgentOutboundCall(
                answeredRing = false,
                pendingOutbound = true,
                systemOutgoing = false,
                sdkInt = 29,
            ),
        )
        assertFalse(
            CallAudioService.isAgentOutboundCall(
                answeredRing = false,
                pendingOutbound = true,
                systemOutgoing = null,
                sdkInt = 29,
            ),
        )
        assertFalse(
            CallAudioService.isAgentOutboundCall(
                answeredRing = false,
                pendingOutbound = false,
                systemOutgoing = true,
                sdkInt = 29,
            ),
        )
    }

    @Test
    fun older_android_keeps_the_pending_dial_unless_the_ring_was_just_answered() {
        assertTrue(
            CallAudioService.isAgentOutboundCall(
                answeredRing = false,
                pendingOutbound = true,
                systemOutgoing = null,
                sdkInt = 28,
            ),
        )
        assertTrue(
            CallAudioService.isAgentOutboundCall(
                answeredRing = false,
                pendingOutbound = true,
                systemOutgoing = false,
                sdkInt = 28,
            ),
        )
        assertFalse(
            CallAudioService.isAgentOutboundCall(
                answeredRing = true,
                pendingOutbound = true,
                systemOutgoing = true,
                sdkInt = 28,
            ),
        )
        assertFalse(
            CallAudioService.isAgentOutboundCall(
                answeredRing = false,
                pendingOutbound = false,
                systemOutgoing = true,
                sdkInt = 28,
            ),
        )
    }

    @Test
    fun the_media_hello_carries_direction_and_call_id() {
        val outbound = BridgeClient.mediaHelloFields(
            serial = "phone-h6",
            callerId = "",
            sampleRate = 16000,
            simCountry = "tw",
            simSubId = null,
            direction = BridgeClient.HELLO_DIRECTION_OUTBOUND,
            callId = "call-9",
            localMenu = false,
            model = "test",
            appVersion = "0",
        )
        assertEquals("hello", outbound["type"])
        assertEquals(BridgeClient.HELLO_DIRECTION_OUTBOUND, outbound["direction"])
        assertEquals("call-9", outbound["call_id"])
        assertFalse(outbound.containsKey("sim_sub_id"))
        val incoming = BridgeClient.mediaHelloFields(
            serial = "phone-h6",
            callerId = "",
            sampleRate = 16000,
            simCountry = "tw",
            simSubId = 1,
            direction = BridgeClient.HELLO_DIRECTION_INCOMING,
            callId = "",
            localMenu = false,
            model = "test",
            appVersion = "0",
        )
        assertTrue(incoming.containsKey("direction"))
        assertTrue(incoming.containsKey("call_id"))
        assertEquals(BridgeClient.HELLO_DIRECTION_INCOMING, incoming["direction"])
        assertEquals("", incoming["call_id"])
        assertEquals(1, incoming["sim_sub_id"])
    }
}
