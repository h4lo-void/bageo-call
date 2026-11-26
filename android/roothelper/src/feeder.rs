// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! What the playback stream writes next. The device sets the pace: the stream writes
//! one period at a time with a blocking write, which returns once the device has room.
//! Every write is either the oldest whole period the app sent, or a period of silence
//! when the app has not sent one; so the device is never left without data, however
//! long the app is quiet, and never waits for the app.
//!
//! The stream takes from the app's connection only up to a few periods ahead
//! ([`Feeder::room`]); the rest stays in the connection, and the app's own blocking
//! writes slow down to the device's pace by themselves. So audio sent faster than real
//! time is played late, never dropped. Only a flush drops audio. The app should send at
//! the device's pace (or keep its socket's send buffer small): whatever it sends ahead
//! is delay.

pub struct Feeder {
    period: usize,
    max_pending: usize,
    /// Periods of the app's audio written.
    pub played_bytes: u64,
    pub silences: u64,
    /// Dropped by a flush (or left over, less than a period, at the end).
    pub dropped_bytes: u64,
}

impl Feeder {
    /// `period`: bytes; at most `max_periods` periods are taken ahead of the device.
    pub fn new(period: usize, max_periods: usize) -> Feeder {
        Feeder { period, max_pending: period * max_periods.max(2), played_bytes: 0, silences: 0, dropped_bytes: 0 }
    }

    /// How many more bytes may be taken from the app before the next write.
    pub fn room(&self, pending: usize) -> usize {
        self.max_pending.saturating_sub(pending)
    }

    /// Fills `out` (one period) with what to write next and takes it out of `pending`.
    /// True: the app's audio; false: silence.
    pub fn next(&mut self, pending: &mut Vec<u8>, out: &mut [u8]) -> bool {
        debug_assert_eq!(out.len(), self.period);
        if pending.len() >= self.period {
            out.copy_from_slice(&pending[..self.period]);
            pending.drain(..self.period);
            self.played_bytes += self.period as u64;
            true
        } else {
            out.fill(0);
            self.silences += 1;
            false
        }
    }

    /// Drops everything waiting.
    pub fn flush(&mut self, pending: &mut Vec<u8>) {
        self.dropped_bytes += pending.len() as u64;
        pending.clear();
    }

    /// Counts bytes thrown away straight from the connection during a flush.
    pub fn discarded(&mut self, n: usize) {
        self.dropped_bytes += n as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    const PERIOD: usize = 640;
    const DEVICE_PERIODS: usize = 4;
    const SOCKET_PERIODS: usize = 16;

    /// What came out of a run.
    struct Run {
        sent: u64,
        played: u64,
        dropped: u64,
        underruns: usize,
        worst_pending: usize,
        ticks: usize,
    }

    /// One device period per tick from a buffer of DEVICE_PERIODS periods; the playback
    /// loop writes whenever the device has room, taking from a connection that holds
    /// SOCKET_PERIODS periods; the app's writes block (wait for a later tick) while the
    /// connection is full. `app(t)`: how many periods the app tries to send at tick `t`,
    /// until it has sent `total`. Runs until everything sent was played.
    fn run(total: usize, mut app: impl FnMut(usize) -> usize) -> Run {
        let mut feeder = Feeder::new(PERIOD, 8);
        let mut socket: VecDeque<u8> = VecDeque::new();
        let mut pending = Vec::new();
        let mut out = vec![0u8; PERIOD];
        let (mut queued, mut underruns, mut worst, mut outbox, mut sent_periods) = (0usize, 0, 0, 0usize, 0usize);
        let mut t = 0;
        while (sent_periods < total || outbox > 0 || !socket.is_empty() || pending.len() >= PERIOD) && t < 100_000 {
            if sent_periods < total {
                let n = app(t).min(total - sent_periods);
                outbox += n;
                sent_periods += n;
            }
            // The app's blocking write: as much as the connection holds.
            while outbox > 0 && socket.len() + PERIOD <= SOCKET_PERIODS * PERIOD {
                socket.extend(std::iter::repeat_n(1u8, PERIOD));
                outbox -= 1;
            }
            while queued < DEVICE_PERIODS {
                let room = feeder.room(pending.len()).min(socket.len());
                pending.extend(socket.drain(..room));
                feeder.next(&mut pending, &mut out);
                queued += 1;
            }
            worst = worst.max(pending.len() / PERIOD);
            if queued == 0 {
                underruns += 1;
            } else {
                queued -= 1;
            }
            t += 1;
        }
        Run {
            sent: (total * PERIOD) as u64,
            played: feeder.played_bytes,
            dropped: feeder.dropped_bytes,
            underruns,
            worst_pending: worst,
            ticks: t,
        }
    }

    fn check(r: &Run) {
        assert_eq!(r.dropped, 0, "nothing is dropped without a flush");
        assert_eq!(r.played, r.sent - r.dropped, "everything sent is played");
        assert_eq!(r.underruns, 0);
        assert!(r.worst_pending <= 8, "at most 8 periods taken ahead, got {}", r.worst_pending);
    }

    #[test]
    fn real_time_with_jitter_drops_nothing() {
        // One period per tick on average: 0, 2, 1, 1, 0, 2, ... with every seventh tick late.
        let r = run(600, |t| match t % 7 {
            0 => 0,
            1 => 2,
            _ => 1,
        });
        check(&r);
    }

    #[test]
    fn three_times_real_time_is_played_late_not_dropped() {
        let r = run(300, |_| 3);
        check(&r);
        assert!(r.ticks + DEVICE_PERIODS >= 300, "played at the device's pace: {} ticks", r.ticks);
    }

    #[test]
    fn fifty_periods_at_once_are_played_slowly() {
        let r = run(50, |t| if t == 0 { 50 } else { 0 });
        check(&r);
        assert!(r.ticks + DEVICE_PERIODS >= 50, "{} ticks", r.ticks);
    }

    #[test]
    fn ten_quiet_seconds_never_run_dry() {
        // 20 ms periods: 500 ticks of nothing, then a little audio.
        let r = run(5, |t| usize::from(t >= 500));
        check(&r);
        assert!(r.ticks >= 500);
    }

    #[test]
    fn whole_periods_or_silence_and_a_flush() {
        let mut f = Feeder::new(4, 2);
        let mut pending = vec![1, 2, 3];
        let mut out = [9u8; 4];
        assert!(!f.next(&mut pending, &mut out));
        assert_eq!(out, [0, 0, 0, 0], "a partial period waits; silence goes out");
        assert_eq!(pending, vec![1, 2, 3]);
        pending.extend([4, 5]);
        assert!(f.next(&mut pending, &mut out));
        assert_eq!(out, [1, 2, 3, 4]);
        assert_eq!(pending, vec![5]);
        assert_eq!((f.silences, f.played_bytes), (1, 4));
        assert_eq!(f.room(0), 8);
        assert_eq!(f.room(5), 3);
        assert_eq!(f.room(30), 0);
        let mut big: Vec<u8> = (0..30).collect();
        assert!(f.next(&mut big, &mut out), "a long queue is played, not cut");
        assert_eq!(out, [0, 1, 2, 3]);
        assert_eq!(f.dropped_bytes, 0);
        f.flush(&mut big);
        f.discarded(10);
        assert!(big.is_empty());
        assert_eq!(f.dropped_bytes, 36);
    }
}
