// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class LocalSmsForwarderTest {
    private class MemoryStorage : LocalSmsEngine.Storage {
        private var lastId = -1L
        private var pending = emptyList<LocalSmsEngine.PendingSms>()
        private var dedup = emptyMap<String, Long>()

        override fun lastInboxId(): Long = lastId
        override fun setLastInboxId(id: Long) { lastId = id }
        override fun loadPending(): List<LocalSmsEngine.PendingSms> = pending
        override fun savePending(items: List<LocalSmsEngine.PendingSms>) { pending = items }
        override fun loadDedup(): Map<String, Long> = dedup
        override fun saveDedup(items: Map<String, Long>) { dedup = items }
    }

    private class MemoryInbox : LocalSmsEngine.InboxReader {
        var rows = mutableListOf<LocalSmsEngine.InboxMessage>()
        override fun maxId(): Long = rows.maxOfOrNull { it.id } ?: 0L
        override fun querySince(lastId: Long): List<LocalSmsEngine.InboxMessage> =
            rows.filter { it.id > lastId }
    }

    @Test
    fun firstRunInitializesBaselineWithoutForwardingOldMessages() {
        val storage = MemoryStorage()
        val inbox = MemoryInbox().apply {
            rows += LocalSmsEngine.InboxMessage(10L, "0912345678", "Old SMS", 1_000L, 1)
            rows += LocalSmsEngine.InboxMessage(15L, "0912345678", "Also Old", 2_000L, 1)
        }
        val sent = mutableListOf<LocalSmsEngine.PendingSms>()
        val engine = LocalSmsEngine(storage, inbox, null, "Pixel Test", { sent.add(it); true }, { 5_000L }, {})

        engine.initializeBaselineIfNeeded()
        engine.sweepAndFlush()

        assertEquals(15L, storage.lastInboxId())
        assertTrue(sent.isEmpty())
        assertEquals(0, engine.pendingCount())
    }

    @Test
    fun deduplicatesBroadcastAndInboxCopyWithin24Hours() {
        val storage = MemoryStorage()
        storage.setLastInboxId(10L)
        val inbox = MemoryInbox().apply {
            rows += LocalSmsEngine.InboxMessage(11L, "+886 912-345-678", "Code 654321", 10_000L, 2)
        }
        val sent = mutableListOf<LocalSmsEngine.PendingSms>()
        val engine = LocalSmsEngine(storage, inbox, null, "Pixel Test", { sent.add(it); true }, { 20_000L }, {})

        engine.enqueueFromBroadcast("+886912345678", "Code 654321", 10_000L, 2, "bcast:abc")
        assertEquals(1, sent.size)

        // Same message later observed via content://sms/inbox sweep must be deduplicated.
        engine.sweepAndFlush()
        assertEquals(1, sent.size)
        assertEquals(11L, storage.lastInboxId())
    }

    @Test
    fun retriesUntilAckedWithOkTrue() {
        var now = 1_000L
        val storage = MemoryStorage()
        storage.setLastInboxId(1L)
        val inbox = MemoryInbox()
        var allowSend = false
        val sent = mutableListOf<LocalSmsEngine.PendingSms>()
        val engine = LocalSmsEngine(
            storage,
            inbox,
            null,
            "Pixel Test",
            { obj ->
                if (!allowSend) false else {
                    sent.add(obj)
                    true
                }
            },
            { now },
            {},
        )

        engine.enqueueFromBroadcast("0912345678", "Hi there", 1_000L, null, "bcast:retry1")
        assertEquals(1, engine.pendingCount())
        assertTrue(sent.isEmpty())

        // Retry after cooldown succeeds cleanly.
        now += LocalSmsEngine.RETRY_COOLDOWN_MS + 100
        allowSend = true
        engine.flushPending()
        assertEquals(1, sent.size)
        // Still pending until server acknowledges.
        assertEquals(1, engine.pendingCount())

        // Failed ack leaves item queued.
        engine.onAckResult("bcast:retry1", false)
        assertEquals(1, engine.pendingCount())

        // Successful ack removes it.
        engine.onAckResult("bcast:retry1", true)
        assertEquals(0, engine.pendingCount())
        assertFalse(sent.isEmpty())
    }
}
