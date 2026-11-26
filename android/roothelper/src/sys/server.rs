// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! `serve`: the helper's life. One copy at a time (a lock file). It first puts back a
//! session a previous run left behind, then connects to the app's socket every 200 ms.
//! Each connection the app accepts is one control connection; when it ends, the
//! session is put back and the helper connects again while the app still listens. With
//! no connection for a minute it quits. A stop signal puts the session back first.

use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::alsa::{AlsaMixer, Pcm};
use super::params::AppProcessParams;
use super::probe::SysProber;
use super::{log, net, APP_DATA_DIR, DATA_DIR, LOCK_FILE, PACKAGES_LIST, SOCKET_NAME, STATE_FILE};
use crate::control::{Direction, Dispatcher, Hardware, PcmHost, PcmRequest};
use crate::json::Json;
use crate::peer;
use crate::proto::{self, Kind};
use crate::session::{self, Report, Session};

/// No connection for this long: the helper quits.
const IDLE: Duration = Duration::from_secs(60);
const RETRY: Duration = Duration::from_millis(200);
/// The app must send its first request this soon after a connection.
const FIRST_REQUEST: Duration = Duration::from_secs(30);
const MAX_STREAMS: usize = 4;

pub type SysDispatcher = Dispatcher<AlsaMixer, AppProcessParams, StreamHost, SysProber>;

pub fn hardware() -> Hardware<AlsaMixer, AppProcessParams> {
    Hardware { mixer: AlsaMixer::new(), params: AppProcessParams::next_to_helper() }
}

pub fn dispatcher(guard: Arc<Mutex<()>>) -> SysDispatcher {
    Dispatcher::new(hardware(), StreamHost::default(), SysProber, Session::new(STATE_FILE), guard)
}

/// The app's user ID right now.
pub fn app_uid() -> Option<u32> {
    let list = fs::read_to_string(PACKAGES_LIST)
        .ok()
        .and_then(|t| peer::uid_from_packages_list(&t, peer::APP_PACKAGE));
    let dir = fs::metadata(APP_DATA_DIR).ok().map(|m| m.uid());
    peer::resolve_app_uid(list, dir)
}

/// Connects to the app; `None` when it does not listen or the listener is not the app.
fn connect_to_app() -> Option<UnixStream> {
    let s = net::connect_abstract(SOCKET_NAME).ok()?;
    let peer_uid = net::peer_uid(&s).ok();
    if peer_uid.is_some_and(|u| peer::peer_allowed(u, app_uid())) {
        Some(s)
    } else {
        log::warn(&format!("refused: the socket belongs to user {peer_uid:?}, not the Bageo app"));
        // Another app holding the name: try again slowly, without flooding the log.
        thread::sleep(Duration::from_secs(2));
        None
    }
}

/// Takes the one-copy lock; `Ok(None)`: another helper holds it.
pub fn take_lock() -> io::Result<Option<fs::File>> {
    if !Path::new(DATA_DIR).is_dir() {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(DATA_DIR)?;
    }
    let f = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).mode(0o600).open(LOCK_FILE)?;
    // SAFETY: flock on a descriptor we own.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        Ok(Some(f))
    } else {
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Ok(None)
        } else {
            Err(e)
        }
    }
}

/// Blocks the stop signals in this thread and every thread started after it; the
/// signal thread waits for them instead.
pub fn block_stop_signals() -> libc::sigset_t {
    // SAFETY: plain signal-set calls on a local set.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for s in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGQUIT] {
            libc::sigaddset(&mut set, s);
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        set
    }
}

/// On a stop signal: put back whatever session is on disk, then quit.
pub fn spawn_signal_thread(set: libc::sigset_t, guard: Arc<Mutex<()>>) {
    thread::spawn(move || {
        let mut sig: libc::c_int = 0;
        // SAFETY: `set` holds the blocked stop signals.
        while unsafe { libc::sigwait(&set, &mut sig) } != 0 {}
        let _held = guard.lock().unwrap_or_else(|e| e.into_inner());
        log::info(&format!("signal {sig}: putting the audio settings back"));
        if let Some(r) = restore_state_file() {
            log_report("stop", &r);
        }
        std::process::exit(0);
    });
}

pub fn restore_state_file() -> Option<Report> {
    session::restore_leftover(Path::new(STATE_FILE), &mut hardware())
}

pub fn log_report(when: &str, r: &Report) {
    let text = format!("{when}: put back {} value(s), {} failed {:?}", r.restored, r.failed.len(), r.failed);
    if r.failed.is_empty() {
        log::info(&text);
    } else {
        log::warn(&text);
    }
}

fn hello(fields: &[&str]) -> Vec<u8> {
    let mut all = vec!["bageo-audio-helper", "1"];
    all.extend_from_slice(fields);
    all.join("\n").into_bytes()
}

/// The control connection: the read timeout is lifted after the first request.
struct ControlConn {
    s: UnixStream,
    first: bool,
}

impl Read for ControlConn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.s.read(buf)?;
        if self.first && n > 0 {
            self.first = false;
            self.s.set_read_timeout(None)?;
        }
        Ok(n)
    }
}

impl Write for ControlConn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.s.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.s.flush()
    }
}

pub fn serve() -> i32 {
    let set = block_stop_signals();
    let guard = Arc::new(Mutex::new(()));
    let _lock = match take_lock() {
        Ok(Some(f)) => f,
        Ok(None) => {
            log::info("already running");
            return 0;
        }
        Err(e) => {
            log::warn(&format!("cannot take the lock: {e}"));
            return 1;
        }
    };
    spawn_signal_thread(set, Arc::clone(&guard));
    {
        let _held = guard.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = restore_state_file() {
            log_report("left over from the last run", &r);
        }
    }
    log::info("started; waiting for the app");
    let mut d = dispatcher(Arc::clone(&guard));
    let mut last = Instant::now();
    loop {
        if last.elapsed() >= IDLE {
            log::info("no app for a minute; quitting");
            return 0;
        }
        let Some(s) = connect_to_app() else {
            thread::sleep(RETRY);
            continue;
        };
        let _ = s.set_read_timeout(Some(FIRST_REQUEST));
        let mut conn = ControlConn { s, first: true };
        if proto::write_frame(&mut conn, Kind::Hello, &hello(&["control"])).is_err() {
            thread::sleep(RETRY);
            continue;
        }
        log::info("serving the app");
        match panic::catch_unwind(AssertUnwindSafe(|| d.serve(&mut conn))) {
            Ok(Some(r)) => log_report("connection ended", &r),
            Ok(None) => {}
            Err(_) => {
                let _held = guard.lock().unwrap_or_else(|e| e.into_inner());
                log::warn("internal error; putting the audio settings back and quitting");
                if let Some(r) = restore_state_file() {
                    log_report("internal error", &r);
                }
                return 70;
            }
        }
        let _ = conn.s.shutdown(std::net::Shutdown::Both);
        last = Instant::now();
        thread::sleep(RETRY);
    }
}

// ------------------------------------------------------------------ PCM streams

struct Stream {
    id: String,
    stop: Arc<AtomicBool>,
    sock: UnixStream,
    thread: Option<JoinHandle<()>>,
}

impl Stream {
    fn finish(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.sock.shutdown(std::net::Shutdown::Both);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The PCM streams of the current connection, one thread and one connection each.
#[derive(Default)]
pub struct StreamHost {
    streams: Vec<Stream>,
    next: u32,
}

impl StreamHost {
    fn reap(&mut self) {
        let (done, live): (Vec<Stream>, Vec<Stream>) =
            self.streams.drain(..).partition(|s| s.thread.as_ref().is_none_or(|t| t.is_finished()));
        self.streams = live;
        for s in done {
            s.finish();
        }
    }
}

impl PcmHost for StreamHost {
    fn open(&mut self, req: &PcmRequest) -> Result<Json, String> {
        self.reap();
        if self.streams.len() >= MAX_STREAMS {
            return Err("too many streams".into());
        }
        let pcm = Pcm::open(req).map_err(|e| format!("cannot open the PCM device: {e}"))?;
        let mut sock = connect_to_app().ok_or("cannot connect to the app for the stream")?;
        self.next += 1;
        let id = format!("s{}", self.next);
        let (rate, ch, period, periods) =
            (pcm.rate.to_string(), pcm.channels.to_string(), pcm.period_frames.to_string(), pcm.periods.to_string());
        let hello = hello(&["pcm", &id, req.dir.name(), &rate, &ch, &period, &periods]);
        proto::write_frame(&mut sock, Kind::Hello, &hello).map_err(|e| format!("cannot reach the app: {e}"))?;
        let reply = Json::obj()
            .with("id", Json::str(id.clone()))
            .with("direction", Json::str(req.dir.name()))
            .with("rate", Json::Num(pcm.rate as i64))
            .with("channels", Json::Num(pcm.channels as i64))
            .with("period_frames", Json::Num(pcm.period_frames as i64))
            .with("periods", Json::Num(pcm.periods as i64))
            .with("buffer_frames", Json::Num(pcm.buffer_frames as i64));
        let stop = Arc::new(AtomicBool::new(false));
        let data = sock.try_clone().map_err(|e| e.to_string())?;
        let flag = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name(format!("pcm-{id}"))
            .spawn(move || match pcm.dir {
                Direction::Capture => capture_loop(pcm, data, flag),
                Direction::Playback => playback_loop(pcm, data, flag),
            })
            .map_err(|e| e.to_string())?;
        log::info(&format!("stream {id}: {} card {} device {}", req.dir.name(), req.card, req.device));
        self.streams.push(Stream { id, stop, sock, thread: Some(thread) });
        Ok(reply)
    }

    fn close(&mut self, id: &str) -> Result<(), String> {
        let i = self.streams.iter().position(|s| s.id == id).ok_or("no such stream")?;
        self.streams.remove(i).finish();
        Ok(())
    }

    fn close_all(&mut self) {
        for s in self.streams.drain(..) {
            s.finish();
        }
    }
}

/// Device to app: one period at a time, as it comes.
fn capture_loop(pcm: Pcm, mut sock: UnixStream, stop: Arc<AtomicBool>) {
    let mut buf = vec![0u8; pcm.period_bytes()];
    let _ = sock.set_write_timeout(Some(Duration::from_secs(2)));
    while !stop.load(Ordering::Relaxed) {
        if let Err(e) = pcm.read_frames(&mut buf) {
            log::warn(&format!("capture stopped: {e}"));
            break;
        }
        if sock.write_all(&buf).is_err() {
            break;
        }
    }
    let _ = sock.shutdown(std::net::Shutdown::Both);
}

/// App to device: one period at a time. When the app is late enough that the device
/// would run dry, a period of silence keeps the stream going (an underrun stops it,
/// and some phones then stop the call's own audio path too).
fn playback_loop(pcm: Pcm, mut sock: UnixStream, stop: Arc<AtomicBool>) {
    let period = pcm.period_bytes();
    let period_ms = (pcm.period_frames as u64 * 1000 / pcm.rate.max(1) as u64).max(1);
    // After a write the device holds about `periods - 1` periods; fill in before they run out.
    let slack = Duration::from_millis((period_ms * (pcm.periods as u64).saturating_sub(1)).saturating_sub(period_ms / 2).max(1));
    let mut buf = vec![0u8; period];
    let mut have = 0usize;
    let mut last_write: Option<Instant> = None;
    while !stop.load(Ordering::Relaxed) {
        let wait = match last_write {
            None => Duration::from_millis(100),
            Some(t) => slack.saturating_sub(t.elapsed()).min(Duration::from_millis(100)),
        };
        match net::wait_readable(&sock, wait) {
            Ok(true) => match sock.read(&mut buf[have..]) {
                Ok(0) => break,
                Ok(n) => have += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            },
            Ok(false) => {
                if last_write.is_some_and(|t| t.elapsed() >= slack) {
                    buf[have..].fill(0);
                    have = period;
                }
            }
            Err(_) => break,
        }
        if have == period {
            if let Err(e) = pcm.write_frames(&buf) {
                log::warn(&format!("playback stopped: {e}"));
                break;
            }
            have = 0;
            last_write = Some(Instant::now());
        }
    }
    let _ = sock.shutdown(std::net::Shutdown::Both);
}
