// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! `serve`: the helper's life.
//!
//! 1. One copy at a time (a lock file); not at all while the owner turned it off
//!    (`/data/adb/pixelcallai/audio_helper.disabled`), and not unless the app passes the
//!    checks in [`crate::peer`] (installed once, main user, own user ID, not debuggable).
//! 2. Puts back what a previous run left in the state file (once at the start, then at
//!    most every 30 seconds while it runs).
//! 3. Draws new keys and publishes them for the app ([`crate::auth`],
//!    [`super::appdir`]); without a trusted SELinux label on the app's folder it does
//!    not serve.
//! 4. Connects to the app's socket every 200 ms. Each connection whose listener is the
//!    app (user ID and SELinux context) and that presents the app key is served (see
//!    [`crate::control`]); when it ends, its streams are closed and its session put back.
//! 5. Quits a minute after the last connection ended, when turned off (it looks every
//!    second), when the app changed, after three connections in a row that failed to
//!    present the key, or on a stop signal. Every way out goes the one way described in
//!    [`crate::life`]: nothing new is changed, the session is put back by whoever holds
//!    the lock, and the process ends while holding it; if a change does not finish in
//!    time, the saved values are kept and `restore --wait` puts them back once this
//!    process has gone. Quitting removes the key file it published (only that file) and
//!    its pid file.

use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::alsa::{AlsaMixer, Pcm};
use super::appdir::{AppDir, Labels, Published};
use super::calls::{call_status, DumpsysCalls};
use super::net::{self, AppConn};
use super::params::{AppProcessParams, CHILD};
use super::{log, props, APP_DATA_DIR, DATA_DIR, DISABLE_FILE, LOCK_FILE, PACKAGES_LIST, PID_FILE, SOCKET_NAME, STATE_FILE};
use crate::allow::{Allowlist, RULES};
use crate::auth::{Keys, KEY_FILE};
use crate::calls::{CallStatus, Watch};
use crate::control::{Dispatcher, Hardware, PcmHost, PcmRequest, Policy, AUTH_TIME};
use crate::json::Json;
use crate::life::Life;
use crate::peer::{self, Refusal};
use crate::proto::{self, Kind};
use crate::session::{self, Leftover, Origin, Report, Session};
use crate::stream::{self, Shared};

/// No connection for this long: the helper quits.
const IDLE: Duration = Duration::from_secs(60);
const RETRY: Duration = Duration::from_millis(200);
const MAX_STREAMS: usize = 4;
const MAX_BAD_AUTH: u32 = 3;
/// How often the call is checked while a session has streams.
const CALL_CHECK: Duration = Duration::from_secs(2);
/// How long stopping waits for a change in progress before it leaves the saved values
/// to `restore --wait`.
const STOP_WAIT: Duration = Duration::from_secs(3);
/// How long closing streams waits for their threads, all of them together.
const STREAM_STOP: Duration = Duration::from_secs(2);
const FLUSH_WAIT: Duration = Duration::from_secs(2);
/// Leftover values are tried again at most this often while the helper runs.
const LEFTOVER_GAP: Duration = Duration::from_secs(30);
/// `packages.list` larger than this is not read at all.
const MAX_PACKAGES_LIST: u64 = 8 << 20;
/// `restore --wait`: how long it waits for the stopped helper to be gone.
const AFTER_EXIT_WAIT: Duration = Duration::from_secs(10);

const SYS_PIDFD_SEND_SIGNAL: libc::c_long = 424;
const SYS_PIDFD_OPEN: libc::c_long = 434;

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
    let list = read_small(Path::new(PACKAGES_LIST), MAX_PACKAGES_LIST);
    let owner = fs::symlink_metadata(APP_DATA_DIR).ok().filter(|m| m.file_type().is_dir()).map(|m| m.uid());
    peer::app_uid(list.as_deref(), owner)
}

/// A plain file of at most `max` bytes, read whole through one descriptor opened without
/// following a link; `None` for anything else, a larger file included.
fn read_small(path: &Path, max: u64) -> Option<String> {
    let f = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    if !f.metadata().ok()?.file_type().is_file() {
        return None;
    }
    let mut s = String::new();
    f.take(max + 1).read_to_string(&mut s).ok()?;
    (s.len() as u64 <= max).then_some(s)
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

/// [`take_lock`], waiting up to `wait` for another holder to let go.
pub fn wait_for_lock(wait: Duration) -> io::Result<Option<fs::File>> {
    let start = Instant::now();
    loop {
        match take_lock()? {
            Some(l) => return Ok(Some(l)),
            None if start.elapsed() < wait => thread::sleep(Duration::from_millis(100)),
            None => return Ok(None),
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

fn pid_from_file() -> Option<i32> {
    read_small(Path::new(PID_FILE), 32)?.trim().parse::<i32>().ok().filter(|&p| p > 0)
}

/// Whether `pid` is this program running `serve`: the same program file as this one,
/// with `serve` as its only argument.
fn is_serving_helper(pid: i32) -> bool {
    let (Ok(theirs), Ok(mine)) = (fs::read_link(format!("/proc/{pid}/exe")), fs::read_link("/proc/self/exe")) else {
        return false;
    };
    let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) else { return false };
    let args: Vec<&[u8]> = cmdline.split(|&b| b == 0).filter(|a| !a.is_empty()).collect();
    theirs == mine && args.len() == 2 && args[1] == b"serve"
}

/// The pid of a running `serve`, if any (for `status`).
pub fn running_pid() -> Option<i32> {
    pid_from_file().filter(|&p| is_serving_helper(p))
}

/// Sends the running `serve` a stop signal, only once it is sure who receives it: the
/// process is pinned with a pidfd first (its pid cannot go to another process while the
/// pidfd is open), then checked to be the helper, then signalled through the pidfd.
/// Without pidfds (kernels before 5.3) nothing is sent: the helper sees the disable file
/// itself within a second.
// c_long is 32 bits on 32-bit Android.
#[allow(clippy::unnecessary_cast)]
pub fn stop_running() -> bool {
    let Some(pid) = pid_from_file() else { return false };
    // SAFETY: a plain system call; a negative result is an error.
    let fd = unsafe { libc::syscall(SYS_PIDFD_OPEN, pid, 0) };
    if fd < 0 {
        return false;
    }
    // SAFETY: the pidfd was just opened and belongs to nobody else.
    let fd = unsafe { OwnedFd::from_raw_fd(fd as RawFd) };
    if !is_serving_helper(pid) {
        return false;
    }
    // SAFETY: a system call on a pidfd we hold, without extra signal information.
    unsafe {
        libc::syscall(SYS_PIDFD_SEND_SIGNAL, fd.as_raw_fd(), libc::SIGTERM, std::ptr::null::<libc::siginfo_t>(), 0) == 0
    }
}

fn random_keys() -> io::Result<Keys> {
    let mut r = [0u8; 64];
    fs::File::open("/dev/urandom")?.read_exact(&mut r)?;
    Ok(Keys::from_random(r))
}

fn app_dir(uid: u32) -> io::Result<AppDir> {
    AppDir::open(Path::new(APP_DATA_DIR), &["files", "mode"], uid, Labels::Required)
}

/// What quitting removes: the key file this run published, and the pid file.
#[derive(Clone, Default)]
pub struct Exit {
    dir: Option<Arc<AppDir>>,
    key: Option<Arc<Published>>,
}

fn clean_up(exit: &Exit) {
    if let (Some(d), Some(k)) = (&exit.dir, &exit.key) {
        d.remove_published(KEY_FILE, k);
    }
    if pid_from_file() == Some(std::process::id() as i32) {
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

/// Waits for a stop signal (and, with `watch_disable`, for the disable file, checked
/// every second), then stops the one way: [`exit_now`].
pub fn spawn_signal_thread(set: libc::sigset_t, life: Arc<Life>, exit: Exit, watch_disable: bool) {
    thread::spawn(move || {
        let second = libc::timespec { tv_sec: 1, tv_nsec: 0 };
        loop {
            // SAFETY: `set` holds the blocked stop signals; no signal information wanted.
            let sig = unsafe { libc::sigtimedwait(&set, std::ptr::null_mut(), &second) };
            if sig > 0 {
                log::info("stop signal");
                break;
            }
            if watch_disable && disabled() {
                log::info("turned off");
                break;
            }
        }
        exit_now(&life, &exit, 0)
    });
}

/// The way out (see [`crate::life`]): stops new changes first, then ends a running
/// parameter call (through [`CHILD`], which knows the call for sure until it has been
/// reaped), then either puts back the saved values while holding the lock, once that call
/// is known to have gone, and ends the process still holding it; or, when a change does
/// not finish in time, keeps them for `restore --wait` and ends the process (which ends
/// that change).
pub fn exit_now(life: &Life, exit: &Exit, code: i32) -> ! {
    life.stop();
    CHILD.kill();
    let held = life.hold_for_exit(STOP_WAIT).filter(|_| child_gone(Duration::from_secs(1)));
    match held {
        Some(_held) => {
            if let Some(r) = restore_state_file(Leftover::Normal) {
                log_report("stopping", &r);
            }
            clean_up(exit);
            std::process::exit(code)
        }
        None => {
            log::warn("stopping: a change is still in progress; the saved settings are kept and put back once this process has ended");
            clean_up(exit);
            spawn_restore_after_exit();
            std::process::exit(code)
        }
    }
}

/// Whether no parameter call runs any more, killing one that still does, within wait.
fn child_gone(wait: Duration) -> bool {
    let start = Instant::now();
    while CHILD.busy() {
        if start.elapsed() >= wait {
            return false;
        }
        CHILD.kill();
        thread::sleep(Duration::from_millis(20));
    }
    true
}

/// Starts `restore --wait` on its own, to put back the saved values once this process
/// (and its hold on the one-copy lock) has gone.
fn spawn_restore_after_exit() {
    let Ok(exe) = std::env::current_exe() else { return };
    let mut cmd = Command::new(exe);
    cmd.args(["restore", "--wait"])
        .env_clear()
        .env("PATH", "/system/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    if cmd.spawn().is_err() {
        log::warn("cannot start the restore; the settings are put back at the next start or boot");
    }
}

/// `restore --wait`: waits for the stopped helper to be gone, then puts back. `Err`: it
/// did not go in time, so nothing was put back.
pub fn restore_after_exit() -> Result<Option<Report>, &'static str> {
    match wait_for_lock(AFTER_EXIT_WAIT) {
        Ok(Some(_lock)) => Ok(restore_state_file(Leftover::Normal)),
        Ok(None) => {
            log::warn("restore after a stop: the helper did not go; nothing put back yet (next start or boot)");
            Err("the stopped helper did not go in time; nothing was put back yet")
        }
        Err(_) => Err("cannot take the lock; nothing was put back"),
    }
}

pub fn restore_state_file(how: Leftover) -> Option<Report> {
    session::restore_leftover(Path::new(STATE_FILE), &mut hardware(), how)
}

/// Counts and fixed kinds only.
pub fn log_report(when: &str, r: &Report) {
    let text = format!(
        "{when}: put back {}, failed {}, not on the list {}, out of tries {}, given up {}, left {}{} {:?}",
        r.restored,
        r.failed,
        r.not_allowed,
        r.given_up,
        r.discarded,
        r.left,
        if r.unreadable { ", state file not usable" } else { "" },
        r.kinds
    );
    if r.clean() && r.discarded == 0 {
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
    let life = Arc::new(Life::new());
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
    if let Some(r) = restore_state_file(Leftover::Normal) {
        log_report("left over from the last run", &r);
    }
    let mut leftover_tried = Instant::now();
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
    let dir = match app_dir(uid) {
        Ok(d) => Arc::new(d),
        Err(_) => {
            log::warn("the app's folder cannot be used safely (owner or SELinux label); not serving");
            return 1;
        }
    };
    let key = match dir.publish(KEY_FILE, keys.file_text().as_bytes()) {
        Ok(k) => k,
        Err(_) => {
            log::warn("cannot hand the key to the app; not serving");
            return 1;
        }
    };
    let exit = Exit { dir: Some(dir), key: Some(Arc::new(key)) };
    write_pid();
    spawn_signal_thread(set, Arc::clone(&life), exit.clone(), true);
    log::info("started; waiting for the app");

    let mut d = Dispatcher::new(
        hardware(),
        StreamHost::new(keys.clone(), uid),
        DumpsysCalls,
        Session::new(STATE_FILE, Origin::App),
        keys.clone(),
        Policy::App,
        Arc::clone(&life),
    );
    let mut last = Instant::now();
    let mut bad_auth = 0;
    let code = loop {
        if life.stopping() {
            break 0;
        }
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
        if fs::symlink_metadata(STATE_FILE).is_ok() && leftover_tried.elapsed() >= LEFTOVER_GAP {
            let _held = life.hold();
            if let Some(r) = restore_state_file(Leftover::Normal) {
                log_report("left over", &r);
            }
            leftover_tried = Instant::now();
        }
        let mut conn = AppConn(s);
        let by = Instant::now() + proto::WRITE_TIME;
        if proto::write_frame_by(&mut conn, Kind::Hello, &hello(&["control", &keys.helper_hex()]), by).is_err() {
            thread::sleep(RETRY);
            continue;
        }
        d.new_connection();
        d.pcm.set_control(conn.0.try_clone().ok());
        let served = panic::catch_unwind(AssertUnwindSafe(|| d.serve(&mut conn)));
        d.pcm.set_control(None);
        match served {
            Ok(Some(r)) => log_report("session ended", &r),
            Ok(None) => {}
            Err(_) => {
                log::warn("internal error; putting the audio settings back and quitting");
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
    // The way out: the session is put back under the lock (which shut_down takes and
    // lets go itself), then the process ends holding the lock.
    match panic::catch_unwind(AssertUnwindSafe(|| d.shut_down())) {
        Ok(Some(r)) => log_report("quitting", &r),
        Ok(None) => {}
        Err(_) => log::warn("internal error while quitting"),
    }
    exit_now(&life, &exit, code)
}

// ------------------------------------------------------------------ PCM streams

struct Stream {
    id: String,
    shared: Arc<Shared<Pcm>>,
    done: mpsc::Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

impl Stream {
    fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }

    fn join(mut self) {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The PCM streams of the current connection, one thread and one connection each, and
/// one watch for the whole session that, once the call is over, closes the streams and
/// the control connection (whose end puts the session back).
pub struct StreamHost {
    keys: Keys,
    app_uid: u32,
    streams: Vec<Stream>,
    /// Streams told to stop that had not stopped in time; they still count.
    stragglers: Vec<Stream>,
    next: u32,
    live: Arc<Mutex<Vec<Arc<Shared<Pcm>>>>>,
    control: Arc<Mutex<Option<UnixStream>>>,
    watch: Option<Watch>,
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl StreamHost {
    pub fn new(keys: Keys, app_uid: u32) -> StreamHost {
        StreamHost {
            keys,
            app_uid,
            streams: Vec::new(),
            stragglers: Vec::new(),
            next: 0,
            live: Arc::default(),
            control: Arc::default(),
            watch: None,
        }
    }

    /// The current control connection (a copy), so the call watch can end it.
    pub fn set_control(&mut self, s: Option<UnixStream>) {
        *locked(&self.control) = s;
    }

    /// Stops `list` together (stop flag, connection, device drop all at once), then
    /// waits for them all within one time limit; those still running are kept as
    /// stragglers.
    fn finish(&mut self, list: Vec<Stream>) {
        if list.is_empty() {
            return;
        }
        for s in &list {
            s.shared.halt();
            s.shared.dev.drop_now();
        }
        let deadline = Instant::now() + STREAM_STOP;
        for s in list {
            let left = deadline.saturating_duration_since(Instant::now());
            if s.done.recv_timeout(left).is_ok() || s.finished() {
                s.join();
            } else {
                log::warn("a stream did not stop in time; it is left to end on its own");
                self.stragglers.push(s);
            }
        }
        locked(&self.live).retain(|s| !s.stopped());
    }

    fn reap(&mut self) {
        let (done, running): (Vec<Stream>, Vec<Stream>) =
            self.streams.drain(..).partition(|s| s.shared.stopped() || s.finished());
        self.streams = running;
        self.finish(done);
        let (gone, left): (Vec<Stream>, Vec<Stream>) = self.stragglers.drain(..).partition(Stream::finished);
        self.stragglers = left;
        for s in gone {
            s.join();
        }
    }

    /// Starts the session's call watch unless it runs.
    fn start_watch(&mut self) -> Result<(), String> {
        if self.watch.as_ref().is_some_and(Watch::is_running) {
            return Ok(());
        }
        if let Some(old) = self.watch.take() {
            old.stop(Duration::ZERO);
        }
        let (live, control) = (Arc::clone(&self.live), Arc::clone(&self.control));
        let w = Watch::start(CALL_CHECK, call_status, move || {
            log::info("the call ended; closing its streams and putting its settings back");
            for s in locked(&live).iter() {
                s.halt();
                s.dev.drop_now();
            }
            if let Some(c) = locked(&control).as_ref() {
                let _ = c.shutdown(std::net::Shutdown::Read);
            }
        })
        .map_err(|_| "cannot watch the call".to_string())?;
        self.watch = Some(w);
        Ok(())
    }
}

impl PcmHost for StreamHost {
    fn open(&mut self, req: &PcmRequest) -> Result<Json, String> {
        self.reap();
        if self.streams.len() + self.stragglers.len() >= MAX_STREAMS {
            return Err("too many streams".into());
        }
        self.start_watch()?;
        let pcm = Pcm::open(req).map_err(|e| format!("cannot open the PCM device ({})", e.kind()))?;
        let sock = connect_to_app(self.app_uid).ok_or("cannot connect to the app for the stream")?;
        self.next += 1;
        let id = format!("s{}", self.next);
        let (rate, ch, period, periods) =
            (pcm.rate.to_string(), pcm.channels.to_string(), pcm.period_frames.to_string(), pcm.periods.to_string());
        let helper_key = self.keys.helper_hex();
        let hello = hello(&["pcm", &helper_key, &id, req.dir.name(), &rate, &ch, &period, &periods]);
        let mut conn = AppConn(sock);
        proto::write_frame_by(&mut conn, Kind::Hello, &hello, Instant::now() + proto::WRITE_TIME)
            .map_err(|_| "cannot reach the app".to_string())?;
        let sock = conn.0;
        let reply = Json::obj()
            .with("id", Json::str(id.clone()))
            .with("direction", Json::str(req.dir.name()))
            .with("rate", Json::Num(pcm.rate as i64))
            .with("channels", Json::Num(pcm.channels as i64))
            .with("period_frames", Json::Num(pcm.period_frames as i64))
            .with("periods", Json::Num(pcm.periods as i64))
            .with("buffer_frames", Json::Num(pcm.buffer_frames as i64));
        let shared = Arc::new(Shared::new(pcm, &sock).map_err(|_| "cannot set up the stream".to_string())?);
        let (tx, rx) = mpsc::channel();
        let (s2, keys) = (Arc::clone(&shared), self.keys.clone());
        let thread = thread::Builder::new()
            .name(format!("pcm-{id}"))
            .spawn(move || {
                run_stream(&s2, sock, &keys);
                s2.finish();
                let _ = tx.send(());
            })
            .map_err(|_| "cannot start the stream".to_string())?;
        locked(&self.live).push(Arc::clone(&shared));
        self.streams.push(Stream { id, shared, done: rx, thread: Some(thread) });
        Ok(reply)
    }

    fn watch_call(&mut self) -> Result<(), String> {
        self.start_watch()
    }

    fn flush(&mut self, id: &str) -> Result<(), String> {
        let s = self.streams.iter().find(|s| s.id == id).ok_or("no such stream")?;
        s.shared.flush(FLUSH_WAIT)
    }

    fn close(&mut self, id: &str) -> Result<(), String> {
        let i = self.streams.iter().position(|s| s.id == id).ok_or("no such stream")?;
        let s = self.streams.remove(i);
        self.finish(vec![s]);
        Ok(())
    }

    fn close_all(&mut self) {
        if let Some(w) = self.watch.take() {
            // A check in progress ends on its own and does nothing more.
            w.stop(Duration::from_millis(100));
        }
        let all: Vec<Stream> = self.streams.drain(..).collect();
        self.finish(all);
        locked(&self.live).clear();
        self.reap();
    }
}

/// One stream's connection: the app key first, the call checked again, then the audio.
fn run_stream(shared: &Shared<Pcm>, sock: UnixStream, keys: &Keys) {
    let mut conn = AppConn(sock);
    let by = Instant::now() + AUTH_TIME;
    let authed = match proto::read_frame_by(&mut conn, proto::MAX_REQUEST, Some(by), Some(by)) {
        Ok(Some((Kind::Request, p))) => {
            proto::parse_request(&p).is_ok_and(|f| f.len() == 2 && f[0] == "auth" && keys.app_key_ok(&f[1]))
        }
        _ => false,
    };
    if !authed {
        log::warn("a stream connection did not present the key");
        return;
    }
    if call_status() != CallStatus::InCall {
        log::warn("no call (or the call state is unknown) as the stream was to start; closed");
        return;
    }
    let sock = conn.0;
    if sock.set_read_timeout(None).is_err() {
        return;
    }
    match stream::run(shared, sock) {
        Ok(st) => log::info(&format!(
            "stream ended: {} bytes played, {} silent periods, {} bytes dropped, {} bytes captured; {} under/overruns, {} device timeouts",
            st.played,
            st.silences,
            st.dropped,
            st.captured,
            shared.dev.xruns.load(Ordering::Relaxed),
            shared.dev.io_errors.load(Ordering::Relaxed)
        )),
        Err(e) => log::warn(&format!("stream stopped ({})", e.kind())),
    }
}
