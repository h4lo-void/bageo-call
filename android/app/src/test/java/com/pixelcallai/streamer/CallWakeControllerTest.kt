// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

package com.pixelcallai.streamer

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CallWakeControllerTest {
    private class FakeLocks : CallWakeController.WakeLocks {
        var held = false
        var acquireCalls = 0
        var releaseCalls = 0
        var lastTimeout = 0L

        override fun acquire(timeoutMs: Long) {
            held = true
            acquireCalls++
            lastTimeout = timeoutMs
        }

        override fun release() {
            held = false
            releaseCalls++
        }
    }

    private class FakeScheduler : CallWakeController.DelayScheduler {
        data class Entry(val delayMs: Long, val task: Runnable, var cancelled: Boolean = false)
        val entries = mutableListOf<Entry>()

        override fun postDelayed(delayMs: Long, task: Runnable): Any {
            val e = Entry(delayMs, task)
            entries.add(e)
            return e
        }

        override fun cancel(token: Any) {
            (token as? Entry)?.cancelled = true
        }

        fun runPending() {
            val snapshot = entries.toList()
            for (e in snapshot) {
                if (!e.cancelled) {
                    e.cancelled = true
                    e.task.run()
                }
            }
        }
    }

    @Test
    fun acquiresOnRingAndReleasesAfterIdleGrace() {
        val locks = FakeLocks()
        val sched = FakeScheduler()
        val ctrl = CallWakeController(locks, sched, idleDelayMs = 45_000L, lockTimeoutMs = 2_400_000L, log = {})

        ctrl.onCallActive("ringing")
        assertTrue(ctrl.isHeld())
        assertTrue(locks.held)
        assertEquals(1, locks.acquireCalls)
        assertEquals(2_400_000L, locks.lastTimeout)

        // Second call-state update must not re-acquire redundantly.
        ctrl.onCallActive("offhook")
        assertEquals(1, locks.acquireCalls)

        // Idle schedules release 45s later, does not drop immediately.
        ctrl.onCallIdle()
        assertTrue(ctrl.isHeld())
        assertEquals(0, locks.releaseCalls)
        assertEquals(45_000L, sched.entries.last().delayMs)

        // Grace timer fires -> released.
        sched.runPending()
        assertFalse(ctrl.isHeld())
        assertFalse(locks.held)
        assertEquals(1, locks.releaseCalls)
    }

    @Test
    fun newCallBeforeGraceEndsCancelsPendingRelease() {
        val locks = FakeLocks()
        val sched = FakeScheduler()
        val ctrl = CallWakeController(locks, sched, log = {})

        ctrl.onCallActive("call1")
        ctrl.onCallIdle()
        ctrl.onCallActive("call2")

        // Stale grace timer run must be a no-op because it was cancelled.
        sched.runPending()
        assertTrue(ctrl.isHeld())
        assertEquals(0, locks.releaseCalls)

        ctrl.releaseNow()
        assertFalse(ctrl.isHeld())
        assertEquals(1, locks.releaseCalls)
    }
}
