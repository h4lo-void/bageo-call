// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! What the playback stream writes next. The device sets the pace: the stream writes
//! one period at a time with a blocking write, which returns once the device has room.
//! Every write is either the oldest whole period the app sent, or a period of silence
//! when the app has not sent one; so the device is never left without data, however
//! long the app is quiet, and never waits for the app. A backlog from an app that sends
//! faster than the device plays (or catches up after a gap) is cut back to a few
//! periods, so the delay cannot grow.

pub struct Feeder {
    period: usize,
    frame: usize,
    max_backlog: usize,
    keep: usize,
    pub silences: u64,
    pub dropped_bytes: u64,
}

impl Feeder {
    /// `period`, `frame`: bytes; the backlog is cut to `keep_periods` once it is over
    /// `max_periods`.
    pub fn new(period: usize, frame: usize, max_periods: usize, keep_periods: usize) -> Feeder {
        let keep_periods = keep_periods.max(1);
        Feeder {
            period,
            frame: frame.max(1),
            max_backlog: period * max_periods.max(keep_periods + 1),
            keep: period * keep_periods,
            silences: 0,
            dropped_bytes: 0,
        }
    }

    /// The most bytes worth reading from the app before the next write.
    pub fn room(&self, pending: usize) -> usize {
        (self.max_backlog + self.period).saturating_sub(pending)
    }

    /// Fills `out` (one period) with what to write next and takes it out of `pending`.
    pub fn next(&mut self, pending: &mut Vec<u8>, out: &mut [u8]) {
        debug_assert_eq!(out.len(), self.period);
        if pending.len() > self.max_backlog {
            let mut cut = pending.len() - self.keep;
            cut -= cut % self.frame;
            pending.drain(..cut);
            self.dropped_bytes += cut as u64;
        }
        if pending.len() >= self.period {
            out.copy_from_slice(&pending[..self.period]);
            pending.drain(..self.period);
        } else {
            out.fill(0);
            self.silences += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device playing one period every tick from a buffer of `periods` periods, and the
    /// playback loop: it writes whenever the device has room (a blocking write returns
    /// as soon as there is room). Returns (underruns, largest backlog in periods).
    fn run(ticks: usize, mut app: impl FnMut(usize) -> usize) -> (usize, usize) {
        let (period, periods) = (640usize, 4usize);
        let mut feeder = Feeder::new(period, 2, 8, 2);
        let mut pending = Vec::new();
        let mut out = vec![0u8; period];
        let mut queued = 0usize;
        let (mut underruns, mut worst) = (0, 0);
        for t in 0..ticks {
            pending.extend(std::iter::repeat_n(1u8, app(t) * period));
            while queued < periods {
                feeder.next(&mut pending, &mut out);
                queued += 1;
            }
            worst = worst.max(pending.len() / period);
            if queued == 0 {
                underruns += 1;
            } else {
                queued -= 1;
            }
        }
        (underruns, worst)
    }

    #[test]
    fn ten_quiet_seconds_never_run_dry() {
        // 20 ms periods: 500 ticks are 10 seconds with nothing from the app.
        let (underruns, worst) = run(500, |_| 0);
        assert_eq!(underruns, 0);
        assert_eq!(worst, 0);
    }

    #[test]
    fn bursty_and_late_app_audio_neither_runs_dry_nor_piles_up() {
        // Three periods every three ticks, every seventh burst a tick late, then quiet.
        let (underruns, worst) = run(1500, |t| {
            if t >= 1000 || t % 21 == 1 {
                0
            } else if t % 21 == 2 {
                6
            } else if t % 3 == 0 {
                3
            } else {
                0
            }
        });
        assert_eq!(underruns, 0);
        assert!(worst <= 9, "backlog grew to {worst} periods");
    }

    #[test]
    fn a_flood_is_cut_back() {
        let (underruns, worst) = run(200, |_| 5);
        assert_eq!(underruns, 0);
        assert!(worst <= 9, "backlog grew to {worst} periods");
    }

    #[test]
    fn whole_periods_or_silence() {
        let mut f = Feeder::new(4, 2, 4, 2);
        let mut pending = vec![1, 2, 3];
        let mut out = [9u8; 4];
        f.next(&mut pending, &mut out);
        assert_eq!(out, [0, 0, 0, 0], "a partial period waits; silence goes out");
        assert_eq!(pending, vec![1, 2, 3]);
        pending.extend([4, 5]);
        f.next(&mut pending, &mut out);
        assert_eq!(out, [1, 2, 3, 4]);
        assert_eq!(pending, vec![5]);
        assert_eq!(f.silences, 1);
        let mut big: Vec<u8> = (0..30).collect();
        f.next(&mut big, &mut out);
        assert_eq!(f.dropped_bytes, 22, "cut to two periods, whole frames");
        assert_eq!(out, [22, 23, 24, 25]);
        assert_eq!(f.room(0), 20);
    }
}
