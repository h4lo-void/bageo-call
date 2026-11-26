// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! One PCM stream: its own connection to the app and its own thread, moving raw s16le
//! audio between the connection and a sound device ([`Device`]).
//!
//! - Capture (device to app): one period at a time, as the device delivers it.
//! - Playback (app to device): paced by the device, see [`crate::feeder`]. The app must
//!   send at the device's pace (or keep its socket's send buffer small); audio it sends
//!   ahead waits in the connection and is played late, never dropped.
//! - Flush ([`Shared::flush`]): the stream's own thread drops what waits for the device,
//!   throws away whatever the connection holds at that moment (playback), and drops and
//!   restarts the device; the flush returns only once that is done. The app sends a
//!   flush after it stopped sending the old audio.
//! - Stop ([`Shared::halt`]): ends the thread within a period or so; a read or write
//!   blocked in the device is cut short with [`Device::drop_now`].

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::control::Direction;
use crate::feeder::Feeder;

/// How many periods playback takes from the app ahead of the device.
pub const AHEAD_PERIODS: usize = 8;
/// Most bytes a flush throws away from the connection (an app that keeps sending
/// during a flush cannot keep it going).
const MAX_DRAIN: usize = 4 << 20;
/// Most time a period of captured audio may take to go out to the app, whole.
const CAPTURE_WRITE_TIME: Duration = Duration::from_secs(2);

pub trait Device: Send + Sync {
    fn direction(&self) -> Direction;
    fn period_bytes(&self) -> usize;
    /// Reads whole frames into `buf`; gives up once `stop` is set.
    fn read_frames(&self, buf: &mut [u8], stop: &AtomicBool) -> io::Result<()>;
    /// Writes whole frames from `buf`; blocks until the device has room; gives up once
    /// `stop` is set.
    fn write_frames(&self, buf: &[u8], stop: &AtomicBool) -> io::Result<()>;
    /// Drops what the device holds and makes it ready to start again.
    fn reset(&self) -> io::Result<()>;
    /// Stops it at once; a read or write blocked in another thread returns.
    fn drop_now(&self);
}

#[derive(Default)]
struct Flushes {
    asked: u64,
    done: u64,
    failed: bool,
    ended: bool,
}

/// What the stream's thread and whoever controls it share.
pub struct Shared<D: Device> {
    pub stop: AtomicBool,
    pub dev: D,
    sock: UnixStream,
    flushes: Mutex<Flushes>,
    flushed: Condvar,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub played: u64,
    pub silences: u64,
    pub dropped: u64,
    pub captured: u64,
}

impl<D: Device> Shared<D> {
    /// `sock`: the stream's connection (a copy is kept to cut it off on a stop).
    pub fn new(dev: D, sock: &UnixStream) -> io::Result<Shared<D>> {
        Ok(Shared {
            stop: AtomicBool::new(false),
            dev,
            sock: sock.try_clone()?,
            flushes: Mutex::new(Flushes::default()),
            flushed: Condvar::new(),
        })
    }

    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Asks the thread to stop: the stop flag and the connection.
    pub fn halt(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.sock.shutdown(std::net::Shutdown::Both);
    }

    fn state(&self) -> std::sync::MutexGuard<'_, Flushes> {
        self.flushes.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Drops the stream's queued audio (see the top of this file); returns once the
    /// stream's thread has done it, or an error after `wait`.
    pub fn flush(&self, wait: Duration) -> Result<(), String> {
        let deadline = Instant::now() + wait;
        let mut st = self.state();
        if st.ended || self.stopped() {
            return Err("the stream has ended".into());
        }
        st.asked += 1;
        let want = st.asked;
        loop {
            if st.done >= want {
                return if st.failed { Err("the device could not be restarted".into()) } else { Ok(()) };
            }
            if st.ended {
                return Err("the stream has ended".into());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("the stream did not drop its audio in time".into());
            }
            st = self.flushed.wait_timeout(st, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    fn flush_asked(&self) -> Option<u64> {
        let st = self.state();
        (st.asked > st.done).then_some(st.asked)
    }

    fn flush_done(&self, upto: u64, ok: bool) {
        let mut st = self.state();
        st.done = upto;
        st.failed = !ok;
        self.flushed.notify_all();
    }

    fn ended(&self) {
        self.state().ended = true;
        self.flushed.notify_all();
    }

    /// Marks the stream as over, whether or not [`run`] ran (its thread is done): a
    /// flush waiting for it gets an error at once.
    pub fn finish(&self) {
        self.halt();
        self.ended();
    }
}

/// Moves audio until the stream is stopped, the app closes its end (playback: after
/// what it sent was played) or the device fails. Run on the stream's own thread, after
/// the app proved who it is on this connection.
pub fn run<D: Device>(shared: &Shared<D>, sock: UnixStream) -> io::Result<Stats> {
    let r = match shared.dev.direction() {
        Direction::Capture => capture(shared, sock),
        Direction::Playback => playback(shared, sock),
    };
    shared.ended();
    r
}

fn capture<D: Device>(shared: &Shared<D>, mut sock: UnixStream) -> io::Result<Stats> {
    let dev = &shared.dev;
    let mut stats = Stats::default();
    let mut buf = vec![0u8; dev.period_bytes()];
    while !shared.stopped() {
        if let Some(asked) = shared.flush_asked() {
            let ok = dev.reset().is_ok();
            shared.flush_done(asked, ok);
            if !ok {
                return Err(io::Error::other("the device could not be restarted"));
            }
        }
        if let Err(e) = dev.read_frames(&mut buf, &shared.stop) {
            return if e.kind() == io::ErrorKind::Interrupted { Ok(stats) } else { Err(e) };
        }
        if write_all_within(&mut sock, &buf, CAPTURE_WRITE_TIME).is_err() {
            break;
        }
        stats.captured += buf.len() as u64;
    }
    Ok(stats)
}

/// Writes all of uf within limit, however slowly the other end reads.
fn write_all_within(sock: &mut UnixStream, buf: &[u8], limit: Duration) -> io::Result<()> {
    let by = Instant::now() + limit;
    let mut sent = 0;
    while sent < buf.len() {
        let left = by.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        sock.set_write_timeout(Some(left))?;
        match sock.write(&buf[sent..]) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
            Ok(n) => sent += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Reads and throws away what the connection holds now. (bytes, closed)
fn drain(sock: &mut UnixStream, buf: &mut [u8]) -> io::Result<(usize, bool)> {
    let mut n = 0;
    while n < MAX_DRAIN {
        match sock.read(buf) {
            Ok(0) => return Ok((n, true)),
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok((n, false)),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok((n, false))
}

fn playback<D: Device>(shared: &Shared<D>, mut sock: UnixStream) -> io::Result<Stats> {
    let dev = &shared.dev;
    let period = dev.period_bytes();
    let mut feeder = Feeder::new(period, AHEAD_PERIODS);
    let mut pending: Vec<u8> = Vec::with_capacity(period * AHEAD_PERIODS);
    let mut chunk = vec![0u8; period * AHEAD_PERIODS];
    let mut out = vec![0u8; period];
    let mut closed = false;
    sock.set_nonblocking(true)?;
    let result = loop {
        if shared.stopped() {
            break Ok(());
        }
        if let Some(asked) = shared.flush_asked() {
            feeder.flush(&mut pending);
            let drained = drain(&mut sock, &mut chunk);
            if let Ok((n, end)) = drained {
                feeder.discarded(n);
                closed |= end;
            }
            let ok = drained.is_ok() && dev.reset().is_ok();
            shared.flush_done(asked, ok);
            if !ok {
                break Err(io::Error::other("the stream could not be flushed"));
            }
        }
        // Only up to a few periods ahead; the rest stays in the connection.
        while !closed {
            let room = feeder.room(pending.len()).min(chunk.len());
            if room == 0 {
                break;
            }
            match sock.read(&mut chunk[..room]) {
                Ok(0) => closed = true,
                Ok(n) => pending.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => closed = true,
            }
        }
        if closed && pending.len() < period {
            break Ok(());
        }
        feeder.next(&mut pending, &mut out);
        if let Err(e) = dev.write_frames(&out, &shared.stop) {
            break if e.kind() == io::ErrorKind::Interrupted { Ok(()) } else { Err(e) };
        }
    };
    feeder.flush(&mut pending);
    let stats = Stats {
        played: feeder.played_bytes,
        silences: feeder.silences,
        dropped: feeder.dropped_bytes,
        captured: 0,
    };
    result.map(|()| stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    const PERIOD: usize = 128;
    const PACE: Duration = Duration::from_millis(3);

    /// A device that takes (or gives) one period per PACE and keeps what was written;
    /// `resets` holds how much had been written at each reset.
    struct FakeDev {
        dir: Direction,
        written: Mutex<Vec<u8>>,
        resets: Mutex<Vec<usize>>,
    }

    impl FakeDev {
        fn new(dir: Direction) -> FakeDev {
            FakeDev { dir, written: Mutex::new(Vec::new()), resets: Mutex::new(Vec::new()) }
        }
    }

    impl Device for FakeDev {
        fn direction(&self) -> Direction {
            self.dir
        }
        fn period_bytes(&self) -> usize {
            PERIOD
        }
        fn read_frames(&self, buf: &mut [u8], stop: &AtomicBool) -> io::Result<()> {
            thread::sleep(PACE);
            if stop.load(Ordering::SeqCst) {
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            buf.fill(7);
            Ok(())
        }
        fn write_frames(&self, buf: &[u8], stop: &AtomicBool) -> io::Result<()> {
            thread::sleep(PACE);
            if stop.load(Ordering::SeqCst) {
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            self.written.lock().unwrap().extend_from_slice(buf);
            Ok(())
        }
        fn reset(&self) -> io::Result<()> {
            let n = self.written.lock().unwrap().len();
            self.resets.lock().unwrap().push(n);
            Ok(())
        }
        fn drop_now(&self) {}
    }

    fn start(dir: Direction) -> (Arc<Shared<FakeDev>>, UnixStream, thread::JoinHandle<io::Result<Stats>>) {
        let (app, helper) = UnixStream::pair().unwrap();
        let shared = Arc::new(Shared::new(FakeDev::new(dir), &helper).unwrap());
        let s2 = Arc::clone(&shared);
        let t = thread::spawn(move || run(&s2, helper));
        (shared, app, t)
    }

    fn count(v: &[u8], b: u8) -> usize {
        v.iter().filter(|&&x| x == b).count()
    }

    /// The app sends `periods` periods of `value`, `per_tick` at a time, waiting
    /// `wait(i)` after the i-th send, then closes.
    fn app_sends(mut app: UnixStream, periods: usize, per_tick: usize, wait: impl Fn(usize) -> Duration) {
        let mut sent = 0;
        let mut i = 0;
        while sent < periods {
            let n = per_tick.min(periods - sent);
            app.write_all(&vec![1u8; n * PERIOD]).unwrap();
            sent += n;
            thread::sleep(wait(i));
            i += 1;
        }
    }

    fn played_all(shared: &Shared<FakeDev>, stats: Stats, periods: usize) {
        let sent = (periods * PERIOD) as u64;
        assert_eq!(stats.dropped, 0, "nothing dropped");
        assert_eq!(stats.played, sent - stats.dropped, "played = sent - dropped");
        assert_eq!(count(&shared.dev.written.lock().unwrap(), 1), periods * PERIOD);
    }

    #[test]
    fn real_time_with_jitter_drops_nothing() {
        let (shared, app, t) = start(Direction::Playback);
        // One period per PACE on average, early and late by turns.
        app_sends(app, 80, 1, |i| match i % 4 {
            0 => Duration::from_millis(1),
            1 => Duration::from_millis(5),
            2 => Duration::from_millis(2),
            _ => Duration::from_millis(4),
        });
        let stats = t.join().unwrap().unwrap();
        played_all(&shared, stats, 80);
    }

    #[test]
    fn three_times_real_time_is_played_late_not_dropped() {
        let (shared, app, t) = start(Direction::Playback);
        let start = Instant::now();
        app_sends(app, 90, 3, |_| PACE);
        let stats = t.join().unwrap().unwrap();
        played_all(&shared, stats, 90);
        assert!(start.elapsed() >= PACE * 80, "at the device's pace");
    }

    #[test]
    fn fifty_periods_at_once_are_played_slowly() {
        let (shared, app, t) = start(Direction::Playback);
        app_sends(app, 50, 50, |_| Duration::ZERO);
        let stats = t.join().unwrap().unwrap();
        played_all(&shared, stats, 50);
    }

    #[test]
    fn a_flush_drops_the_old_audio_before_it_answers() {
        let (shared, mut app, t) = start(Direction::Playback);
        // 30 periods of old audio, far more than plays before the flush.
        app.write_all(&vec![1u8; 30 * PERIOD]).unwrap();
        thread::sleep(PACE * 4);
        shared.flush(Duration::from_secs(2)).unwrap();
        let resets = shared.dev.resets.lock().unwrap().clone();
        assert_eq!(resets.len(), 1, "the device was dropped and restarted before the answer");
        // New audio after the flush.
        app.write_all(&vec![2u8; 5 * PERIOD]).unwrap();
        drop(app);
        let stats = t.join().unwrap().unwrap();
        let written = shared.dev.written.lock().unwrap().clone();
        let after = &written[resets[0]..];
        assert_eq!(count(after, 1), 0, "no old audio after the flush");
        assert_eq!(count(after, 2), 5 * PERIOD, "all of the new audio");
        assert!(stats.dropped > 0);
        assert_eq!(stats.played + stats.dropped, (35 * PERIOD) as u64, "played + dropped = sent");
    }

    #[test]
    fn a_flush_of_an_ended_stream_is_an_error() {
        let (shared, app, t) = start(Direction::Playback);
        drop(app);
        t.join().unwrap().unwrap();
        assert!(shared.flush(Duration::from_millis(100)).is_err());
    }

    #[test]
    fn capture_streams_and_flushes() {
        let (shared, mut app, t) = start(Direction::Capture);
        let mut buf = vec![0u8; PERIOD * 3];
        app.read_exact(&mut buf).unwrap();
        assert_eq!(count(&buf, 7), buf.len());
        shared.flush(Duration::from_secs(2)).unwrap();
        assert_eq!(shared.dev.resets.lock().unwrap().len(), 1);
        shared.halt();
        let stats = t.join().unwrap().unwrap();
        assert!(stats.captured >= (3 * PERIOD) as u64);
    }

    #[test]
    fn captured_audio_the_app_does_not_read_cannot_hold_the_stream() {
        let (mut helper, _app) = UnixStream::pair().unwrap();
        helper.set_nonblocking(true).unwrap();
        let chunk = [0u8; 4096];
        while helper.write(&chunk).is_ok() {}
        helper.set_nonblocking(false).unwrap();
        let start = Instant::now();
        let e = write_all_within(&mut helper, &chunk, Duration::from_millis(100)).unwrap_err();
        assert!(matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock), "{e:?}");
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_stop_ends_the_stream_quickly() {
        let (shared, _app, t) = start(Direction::Playback);
        thread::sleep(PACE * 3);
        let start = Instant::now();
        shared.halt();
        t.join().unwrap().unwrap();
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(shared.flush(Duration::from_millis(50)).is_err());
    }
}
