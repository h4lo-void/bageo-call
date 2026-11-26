// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! `serve`: the helper's life.
//!
//! 1. One copy at a time (a lock file); not at all while the owner turned it off
//!    (`/data/adb/pixelcallai/audio_helper.disabled`), and not unless the app passes the
//!    checks in [`crate::peer`] (installed once, main user, own user ID, not debuggable).
//! 2. Puts back what a previous run left in the state file.
//! 3. Draws new keys and writes them for the app ([`crate::auth`]).
//! 4. Connects to the app's socket every 200 ms. Each connection whose listener is the
//!    app (user ID and SELinux context) and that presents the app key is served (see
//!    [`crate::control`]); when it ends, its streams are closed and its session put back.
//! 5. Quits after a minute with no connection, when turned off, when the app changed,
//!    or after three connections that failed to present the key; on a stop signal it
//!    puts the session back first. Quitting removes the key file.

use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::alsa::{AlsaMixer, Pcm};
use super::appdir::AppDir;
use super::calls::{in_call_now, DumpsysCalls};
use super::net::{self, AppConn};
use super::params::{AppProcessParams, CHILD};
use super::{log, props, APP_DATA_DIR, DATA_DIR, DISABLE_FILE, LOCK_FILE, PACKAGES_LIST, PID_FILE, SOCKET_NAME, STATE_FILE};
use crate::allow::{Allowlist, RULES};
use crate::auth::{Keys, KEY_FILE};
use crate::control::{Direction, Dispatcher, Hardware, PcmHost, PcmRequest, Policy, AUTH_TIME};
use crate::feeder::Feeder;
use crate::json::Json;
use crate::peer::{self, Refusal};
use crate::proto::{self, Kind};
use crate::session::{self, Origin, Report, Session};

/// No connection for this long: the helper quits.
const IDLE: Duration = Duration::from_secs(60);
const RETRY: Duration = Duration::from_millis(200);
const MAX_STREAMS: usize = 4;
const MAX_BAD_AUTH: u32 = 3;
/// How often a running stream checks that the call goes on.
const CALL_CHECK: Duration = Duration::from_secs(2);
/// How long a stop signal waits for work in progress before it puts things back anyway.
const SIGNAL_WAIT: Duration = Duration::from_secs(3);

pub type SysDispatcher<H, C> = Dispatcher<AlsaMixer, AppProcessParams, H, C>;

pub fn allowlist() -> Allowlist {
    let platform = props::get("ro.board.platform").unwrap_or_default();
    let device = props::get("ro.product.device").unwrap_or_default();
    Allowlist::for_phone(RULES, &platform, &device)
}

pub fn hardware() -> Hardware<AlsaMixer, AppProcessParams> {
    Hardware { mixer: AlsaMixer::new(), params: AppProcessParams::next_to_helper(), allow: allowlist() }
}

/// The app's user ID right now, or why it must not be served.
pub fn app_uid() -> Result<u32, Refusal> {
    let list = read_small(Path::new(PACKAGES_LIST), 8 << 20);
    let owner = fs::symlink_metadata(APP_DATA_DIR).ok().filter(|m| m.file_type().is_dir()).map(|m| m.uid());
    peer::app_uid(list.as_deref(), owner)
}

fn read_small(path: &Path, max: u64) -> Option<String> {
    let f = fs::File::open(path).ok()?;
    let mut s = String::new();
    f.take(max).read_to_string(&mut s).ok()?;
    Some(s)
}

/// Connects to the app; `None` when it does not listen or the listener is not the app.
fn connect_to_app(app_uid: u32) -> Option<UnixStream> {
    let s = net::connect_abstract(SOCKET_NAME).ok()?;
    let uid_ok = net::peer_uid(&s).ok().is_some_and(|u| peer::peer_allowed(u, app_uid));
    let ctx_ok = net::peer_context(&s).ok().is_some_and(|c| peer::context_allowed(&c));
    if uid_ok && ctx_ok {
        Some(s)
    } else {
        log::warn("refused a listener that is not the Bageo app");
        // Something else holds the name: try again slowly, without flooding the log.
        thread::sleep(Duration::from_secs(2));
        None
    }
}

fn data_dir() -> io::Result<()> {
    if !Path::new(DATA_DIR).is_dir() {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(DATA_DIR)?;
    }
    Ok(())
}

/// Takes the one-copy lock; `Ok(None)`: another helper holds it.
pub fn take_lock() -> io::Result<Option<fs::File>> {
    data_dir()?;
    let f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(LOCK_FILE)?;
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

pub fn disabled() -> bool {
    fs::symlink_metadata(DISABLE_FILE).is_ok()
}

fn write_pid() {
    let _ = fs::remove_file(PID_FILE);
    if let Ok(mut f) = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(PID_FILE) {
        let _ = write!(f, "{}", std::process::id());
    }
}

/// The pid of a running `serve`, if any.
pub fn running_pid() -> Option<i32> {
    let pid = read_small(Path::new(PID_FILE), 32)?.trim().parse::<i32>().ok().filter(|&p| p > 0)?;
    let cmdline = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let text = String::from_utf8_lossy(&cmdline);
    (text.contains("bageo-audio-helper") && text.contains("serve")).then_some(pid)
}

fn random_keys() -> io::Result<Keys> {
    let mut r = [0u8; 64];
    fs::File::open("/dev/urandom")?.read_exact(&mut r)?;
    Ok(Keys::from_random(r))
}

fn app_dir(uid: u32) -> io::Result<AppDir> {
    AppDir::open(Path::new(APP_DATA_DIR), &["files", "mode"], uid)
}

/// What every exit does: the key file and the pid file go.
fn clean_up(uid: Option<u32>) {
    if let Some(uid) = uid {
        if let Ok(d) = app_dir(uid) {
            d.remove_file(KEY_FILE);
        }
    }
    if running_pid() == Some(std::process::id() as i32) {
        let _ = fs::remove_file(PID_FILE);
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

/// On a stop signal: end a running parameter call, wait a little for work in progress,
/// put back whatever session is on disk, clean up and quit.
pub fn spawn_signal_thread(set: libc::sigset_t, guard: Arc<Mutex<()>>, uid: Option<u32>) {
    thread::spawn(move || {
        let mut sig: libc::c_int = 0;
        // SAFETY: `set` holds the blocked stop signals.
        while unsafe { libc::sigwait(&set, &mut sig) } != 0 {}
        let child = CHILD.load(Ordering::SeqCst);
        if child > 0 {
            // SAFETY: kill on a pid we started.
            unsafe {
                libc::kill(child, libc::SIGKILL);
            }
        }
        let start = Instant::now();
        let held = loop {
            match guard.try_lock() {
                Ok(g) => break Some(g),
                Err(std::sync::TryLockError::Poisoned(p)) => break Some(p.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) if start.elapsed() < SIGNAL_WAIT => {
                    thread::sleep(Duration::from_millis(50))
                }
                Err(std::sync::TryLockError::WouldBlock) => break None,
            }
        };
        log::info("stop signal: putting the audio settings back");
        if let Some(r) = restore_state_file(false) {
            log_report("stop", &r);
        }
        drop(held);
        clean_up(uid);
        std::process::exit(0);
    });
}

pub fn restore_state_file(force: bool) -> Option<Report> {
    session::restore_leftover(Path::new(STATE_FILE), &mut hardware(), force)
}

/// Counts and fixed kinds only.
pub fn log_report(when: &str, r: &Report) {
    let text = format!(
        "{when}: put back {}, failed {}, not on the list {}, out of tries {}, left {}{} {:?}",
        r.restored,
        r.failed,
        r.not_allowed,
        r.given_up,
        r.left,
        if r.unreadable { ", state file not usable" } else { "" },
        r.kinds
    );
    if r.clean() {
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

pub fn serve() -> i32 {
    let set = block_stop_signals();
    let guard = Arc::new(Mutex::new(()));
    let _lock = match take_lock() {
        Ok(Some(f)) => f,
        Ok(None) => {
            log::info("already running");
            return 0;
        }
        Err(_) => {
            log::warn("cannot take the lock");
            return 1;
        }
    };
    if let Some(r) = restore_state_file(false) {
        log_report("left over from the last run", &r);
    }
    if disabled() {
        log::info("turned off; not serving");
        return 0;
    }
    let uid = match app_uid() {
        Ok(u) => u,
        Err(r) => {
            log::warn(&format!("not serving: {}", r.name()));
            return 0;
        }
    };
    let Ok(keys) = random_keys() else {
        log::warn("no random numbers; not serving");
        return 1;
    };
    match app_dir(uid).and_then(|d| d.write_file(KEY_FILE, keys.file_text().as_bytes())) {
        Ok(()) => {}
        Err(_) => {
            log::warn("cannot write the key for the app; not serving");
            return 1;
        }
    }
    write_pid();
    spawn_signal_thread(set, Arc::clone(&guard), Some(uid));
    log::info("started; waiting for the app");

    let mut d = Dispatcher::new(
        hardware(),
        StreamHost::new(keys.clone(), uid),
        DumpsysCalls,
        Session::new(STATE_FILE, Origin::App),
        keys.clone(),
        Policy::App,
        Arc::clone(&guard),
    );
    let mut last = Instant::now();
    let mut bad_auth = 0;
    let code = loop {
        if disabled() {
            log::info("turned off; quitting");
            break 0;
        }
        if last.elapsed() >= IDLE {
            log::info("no app for a minute; quitting");
            break 0;
        }
        if app_uid() != Ok(uid) {
            log::warn("the app changed; quitting");
            break 0;
        }
        let Some(s) = connect_to_app(uid) else {
            thread::sleep(RETRY);
            continue;
        };
        {
            let _held = guard.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(r) = restore_state_file(false) {
                log_report("left over", &r);
            }
        }
        let mut conn = AppConn(s);
        if proto::write_frame(&mut conn, Kind::Hello, &hello(&["control", &keys.helper_hex()])).is_err() {
            thread::sleep(RETRY);
            continue;
        }
        d.new_connection();
        match panic::catch_unwind(AssertUnwindSafe(|| d.serve(&mut conn))) {
            Ok(Some(r)) => log_report("session ended", &r),
            Ok(None) => {}
            Err(_) => {
                let _held = guard.lock().unwrap_or_else(|e| e.into_inner());
                log::warn("internal error; putting the audio settings back and quitting");
                if let Some(r) = restore_state_file(false) {
                    log_report("internal error", &r);
                }
                break 70;
            }
        }
        if d.is_authed() {
            bad_auth = 0;
        } else {
            bad_auth += 1;
            log::warn("a connection did not present the key");
            if bad_auth >= MAX_BAD_AUTH {
                break 0;
            }
        }
        let _ = conn.0.shutdown(std::net::Shutdown::Both);
        last = Instant::now();
        thread::sleep(RETRY);
    };
    {
        let _held = guard.lock().unwrap_or_else(|e| e.into_inner());
        d.on_disconnect();
    }
    clean_up(Some(uid));
    code
}

// ------------------------------------------------------------------ PCM streams

struct Shared {
    stop: AtomicBool,
    flush: AtomicBool,
    pcm: Arc<Pcm>,
    sock: UnixStream,
}

impl Shared {
    fn halt(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.sock.shutdown(std::net::Shutdown::Both);
    }
}

struct Stream {
    id: String,
    shared: Arc<Shared>,
    done: mpsc::Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

impl Stream {
    /// Stops the stream within about two seconds: the stop flag and the socket first,
    /// then, if the thread still sits in a device read or write, a DROP on the device.
    fn finish(mut self) {
        self.shared.halt();
        let mut ended = self.done.recv_timeout(Duration::from_millis(300)).is_ok();
        if !ended {
            self.shared.pcm.drop_now();
            ended = self.done.recv_timeout(Duration::from_secs(2)).is_ok();
        }
        if ended {
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        } else {
            log::warn("a stream did not stop in time; left to end on its own");
        }
    }
}

/// The PCM streams of the current connection, one thread and one connection each, and
/// one watcher that closes them all when the call ends.
pub struct StreamHost {
    keys: Keys,
    app_uid: u32,
    streams: Vec<Stream>,
    next: u32,
    live: Arc<Mutex<Vec<Arc<Shared>>>>,
    watcher: Option<JoinHandle<()>>,
}

impl StreamHost {
    pub fn new(keys: Keys, app_uid: u32) -> StreamHost {
        StreamHost { keys, app_uid, streams: Vec::new(), next: 0, live: Arc::default(), watcher: None }
    }

    fn reap(&mut self) {
        let (done, live): (Vec<Stream>, Vec<Stream>) =
            self.streams.drain(..).partition(|s| s.shared.stop.load(Ordering::Relaxed));
        self.streams = live;
        for s in done {
            s.finish();
        }
    }

    fn watch_call(&mut self) {
        if self.watcher.as_ref().is_some_and(|w| !w.is_finished()) {
            return;
        }
        let live = Arc::clone(&self.live);
        self.watcher = Some(thread::spawn(move || loop {
            thread::sleep(CALL_CHECK);
            let streams: Vec<Arc<Shared>> = {
                let mut l = live.lock().unwrap_or_else(|e| e.into_inner());
                l.retain(|s| !s.stop.load(Ordering::Relaxed));
                l.clone()
            };
            if streams.is_empty() {
                return;
            }
            if !in_call_now() {
                log::info("the call ended; closing its streams");
                for s in &streams {
                    s.halt();
                    s.pcm.drop_now();
                }
            }
        }));
    }
}

impl PcmHost for StreamHost {
    fn open(&mut self, req: &PcmRequest) -> Result<Json, String> {
        self.reap();
        if self.streams.len() >= MAX_STREAMS {
            return Err("too many streams".into());
        }
        let pcm = Arc::new(Pcm::open(req).map_err(|e| format!("cannot open the PCM device ({})", e.kind()))?);
        let mut sock = connect_to_app(self.app_uid).ok_or("cannot connect to the app for the stream")?;
        self.next += 1;
        let id = format!("s{}", self.next);
        let (rate, ch, period, periods) =
            (pcm.rate.to_string(), pcm.channels.to_string(), pcm.period_frames.to_string(), pcm.periods.to_string());
        let helper_key = self.keys.helper_hex();
        let hello = hello(&["pcm", &helper_key, &id, req.dir.name(), &rate, &ch, &period, &periods]);
        proto::write_frame(&mut sock, Kind::Hello, &hello).map_err(|_| "cannot reach the app".to_string())?;
        let reply = Json::obj()
            .with("id", Json::str(id.clone()))
            .with("direction", Json::str(req.dir.name()))
            .with("rate", Json::Num(pcm.rate as i64))
            .with("channels", Json::Num(pcm.channels as i64))
            .with("period_frames", Json::Num(pcm.period_frames as i64))
            .with("periods", Json::Num(pcm.periods as i64))
            .with("buffer_frames", Json::Num(pcm.buffer_frames as i64));
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            flush: AtomicBool::new(false),
            pcm,
            sock: sock.try_clone().map_err(|_| "cannot set up the stream".to_string())?,
        });
        let (tx, rx) = mpsc::channel();
        let (s2, keys) = (Arc::clone(&shared), self.keys.clone());
        let thread = thread::Builder::new()
            .name(format!("pcm-{id}"))
            .spawn(move || {
                run_stream(&s2, sock, &keys);
                s2.halt();
                let _ = tx.send(());
            })
            .map_err(|_| "cannot start the stream".to_string())?;
        self.live.lock().unwrap_or_else(|e| e.into_inner()).push(Arc::clone(&shared));
        self.streams.push(Stream { id, shared, done: rx, thread: Some(thread) });
        self.watch_call();
        Ok(reply)
    }

    fn flush(&mut self, id: &str) -> Result<(), String> {
        let s = self.streams.iter().find(|s| s.id == id).ok_or("no such stream")?;
        s.shared.flush.store(true, Ordering::Relaxed);
        Ok(())
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
        self.live.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

/// One stream's connection: the app key first, then the audio.
fn run_stream(shared: &Shared, sock: UnixStream, keys: &Keys) {
    let mut conn = AppConn(sock);
    let authed = match proto::read_frame_timed(&mut conn, proto::MAX_REQUEST, Some(AUTH_TIME)) {
        Ok(Some((Kind::Request, p))) => {
            proto::parse_request(&p).is_ok_and(|f| f.len() == 2 && f[0] == "auth" && keys.app_key_ok(&f[1]))
        }
        _ => false,
    };
    if !authed {
        log::warn("a stream connection did not present the key");
        return;
    }
    let sock = conn.0;
    if sock.set_read_timeout(None).is_err() {
        return;
    }
    match shared.pcm.dir {
        Direction::Capture => capture_loop(shared, sock),
        Direction::Playback => playback_loop(shared, sock),
    }
    log::info(&format!(
        "stream ended: {} under/overruns, {} device timeouts",
        shared.pcm.xruns.load(Ordering::Relaxed),
        shared.pcm.io_errors.load(Ordering::Relaxed)
    ));
}

/// Device to app: one period at a time, as the device delivers it.
fn capture_loop(shared: &Shared, mut sock: UnixStream) {
    let pcm = &shared.pcm;
    let mut buf = vec![0u8; pcm.period_bytes()];
    let _ = sock.set_write_timeout(Some(Duration::from_secs(2)));
    while !shared.stop.load(Ordering::Relaxed) {
        if shared.flush.swap(false, Ordering::Relaxed) && pcm.reset().is_err() {
            break;
        }
        if let Err(e) = pcm.read_frames(&mut buf, &shared.stop) {
            if e.kind() != io::ErrorKind::Interrupted {
                log::warn(&format!("capture stopped ({})", e.kind()));
            }
            break;
        }
        if sock.write_all(&buf).is_err() {
            break;
        }
    }
}

/// App to device, paced by the device (see [`crate::feeder`]): each round takes what the
/// app has sent without waiting, then writes one period (the app's or silence) with a
/// write that blocks until the device has room.
fn playback_loop(shared: &Shared, mut sock: UnixStream) {
    let pcm = &shared.pcm;
    let period = pcm.period_bytes();
    let mut feeder = Feeder::new(period, pcm.frame_bytes(), 8, 2);
    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; period * 4];
    let mut out = vec![0u8; period];
    if sock.set_nonblocking(true).is_err() {
        return;
    }
    'stream: while !shared.stop.load(Ordering::Relaxed) {
        if shared.flush.swap(false, Ordering::Relaxed) {
            pending.clear();
            if pcm.reset().is_err() {
                break;
            }
        }
        loop {
            let room = feeder.room(pending.len()).min(chunk.len());
            if room == 0 {
                break;
            }
            match sock.read(&mut chunk[..room]) {
                Ok(0) => break 'stream,
                Ok(n) => pending.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break 'stream,
            }
        }
        feeder.next(&mut pending, &mut out);
        if let Err(e) = pcm.write_frames(&out, &shared.stop) {
            if e.kind() != io::ErrorKind::Interrupted {
                log::warn(&format!("playback stopped ({})", e.kind()));
            }
            break;
        }
    }
    log::info(&format!("playback: {} silent periods, {} bytes dropped", feeder.silences, feeder.dropped_bytes));
}
