// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Runs a short system command with a time limit, collecting its output on a thread of
//! its own while it runs, so neither a slow command nor a full pipe can hold up the
//! caller: the command is killed when its time is up, and output that did not arrive
//! whole by then (or was cut at the size limit, or failed to read) is marked as such.
//!
//! `ChildSlot` never performs a blocking `wait()` inside its mutex, and once
//! [`ChildSlot::close_and_kill`] has been called no further command may be started
//! through that slot.

use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{mpsc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

pub struct Output {
    /// `None`: it did not end in time (or was stopped) and was killed.
    pub status: Option<ExitStatus>,
    pub stdout: Vec<u8>,
    /// The whole output was read: no read error, not cut at the limit, not left
    /// unread when the time was up.
    pub whole: bool,
}

impl Output {
    /// Ended by itself, successfully, with its whole output read.
    pub fn ok(&self) -> bool {
        self.whole && self.status.is_some_and(|s| s.success())
    }
}

struct SlotInner {
    closed: bool,
    running: Option<u32>,
    /// Children that did not exit within the post-SIGKILL grace window (e.g. stuck in an
    /// uninterruptible kernel wait). Kept unreaped here until non-blocking `try_wait`
    /// succeeds so their PIDs are never reused and `busy()` continues reporting `true`.
    lingering: Vec<Child>,
}

impl SlotInner {
    const fn new() -> SlotInner {
        SlotInner { closed: false, running: None, lingering: Vec::new() }
    }

    fn poll_lingering(&mut self) {
        self.lingering.retain_mut(|c| {
            let _ = c.kill();
            matches!(c.try_wait(), Ok(None))
        });
    }
}

/// Tracks the running parameter command without ever blocking inside its mutex.
pub struct ChildSlot {
    inner: Mutex<SlotInner>,
}

impl ChildSlot {
    pub const fn new() -> ChildSlot {
        ChildSlot { inner: Mutex::new(SlotInner::new()) }
    }

    fn lock(&self) -> MutexGuard<'_, SlotInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Permanently closes this slot against any future `run()` spawns and sends `SIGKILL`
    /// to any currently running or lingering child. Never blocks on `wait()`.
    pub fn close_and_kill(&self) {
        let mut g = self.lock();
        g.closed = true;
        if let Some(pid) = g.running {
            // SAFETY: kill on our own unreaped child.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
        g.poll_lingering();
    }

    /// Kills the running command if any (without closing the slot for future runs).
    pub fn kill(&self) {
        let mut g = self.lock();
        if let Some(pid) = g.running {
            // SAFETY: kill on our own unreaped child.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
        g.poll_lingering();
    }

    /// Whether a command still runs or has not finished kernel exit yet (non-blocking).
    pub fn busy(&self) -> bool {
        let mut g = self.lock();
        g.poll_lingering();
        g.running.is_some() || !g.lingering.is_empty()
    }

    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }
}

impl Default for ChildSlot {
    fn default() -> Self {
        ChildSlot::new()
    }
}

/// How long the output may still take once the command has ended or was killed.
const OUTPUT_GRACE: Duration = Duration::from_millis(500);
/// Bounded non-blocking poll window after `SIGKILL` before parking a stuck child in
/// `lingering` and returning.
const KILL_REAP_WAIT: Duration = Duration::from_millis(500);

/// Runs `cmd` (its stdin and stderr go nowhere) for at most `timeout`, keeping at most
/// `max` bytes of its output. When `slot` is closed, `cmd` is never spawned.
pub fn run(cmd: &mut Command, timeout: Duration, max: usize, slot: Option<&ChildSlot>) -> Result<Output, &'static str> {
    let deadline = Instant::now() + timeout;
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = if let Some(s) = slot {
        let mut g = s.lock();
        if g.closed {
            return Err("the helper is stopping");
        }
        g.poll_lingering();
        let c = cmd.spawn().map_err(|_| "cannot start the command")?;
        g.running = Some(c.id());
        c
    } else {
        cmd.spawn().map_err(|_| "cannot start the command")?
    };

    let try_reap = |child: &mut Child| -> std::io::Result<Option<ExitStatus>> {
        let mut g = slot.map(ChildSlot::lock);
        let r = child.try_wait();
        if !matches!(r, Ok(None)) {
            if let Some(g) = g.as_mut() {
                g.running = None;
            }
        }
        r
    };

    let kill_and_finish = |mut child: Child| {
        let _ = child.kill();
        let kill_by = Instant::now() + KILL_REAP_WAIT;
        loop {
            match try_reap(&mut child) {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) if Instant::now() < kill_by => thread::sleep(Duration::from_millis(10)),
                Ok(None) => {
                    if let Some(s) = slot {
                        let mut g = s.lock();
                        g.running = None;
                        g.lingering.push(child);
                    }
                    return;
                }
            }
        }
    };

    let (tx, rx) = mpsc::channel();
    if let Some(mut out) = child.stdout.take() {
        let spawned = thread::Builder::new().name("child-output".into()).spawn(move || {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            let (mut ok, mut cut) = (true, false);
            loop {
                match out.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) if buf.len() + n <= max => buf.extend_from_slice(&chunk[..n]),
                    Ok(_) => cut = true,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => {
                        ok = false;
                        break;
                    }
                }
            }
            let _ = tx.send((buf, ok && !cut));
        });
        if spawned.is_err() {
            kill_and_finish(child);
            return Err("cannot read the command's output");
        }
    }

    let status = loop {
        match try_reap(&mut child) {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline && !slot.is_some_and(ChildSlot::is_closed) => {
                thread::sleep(Duration::from_millis(10));
            }
            Ok(None) | Err(_) => {
                kill_and_finish(child);
                break None;
            }
        }
    };
    let wait = if status.is_some() {
        deadline.saturating_duration_since(Instant::now()).max(OUTPUT_GRACE)
    } else {
        OUTPUT_GRACE
    };
    let (stdout, whole) = match rx.recv_timeout(wait) {
        Ok((buf, whole)) => (buf, whole && status.is_some()),
        Err(_) => (Vec::new(), false),
    };
    Ok(Output { status, stdout, whole })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(script);
        c
    }

    #[test]
    fn output_larger_than_a_pipe_does_not_block_the_command() {
        let out = run(&mut sh("head -c 1048576 /dev/zero"), Duration::from_secs(10), 2 << 20, None).unwrap();
        assert!(out.ok());
        assert_eq!(out.stdout.len(), 1 << 20);
    }

    #[test]
    fn output_past_the_limit_is_not_whole() {
        let out = run(&mut sh("head -c 100000 /dev/zero"), Duration::from_secs(10), 1000, None).unwrap();
        assert!(out.status.is_some_and(|s| s.success()));
        assert!(!out.whole && !out.ok());
        assert!(out.stdout.len() <= 1000);
    }

    #[test]
    fn a_slow_command_is_killed_on_time() {
        let slot = ChildSlot::new();
        let start = Instant::now();
        let out = run(&mut sh("echo partial; exec sleep 30"), Duration::from_millis(200), 1000, Some(&slot)).unwrap();
        assert!(start.elapsed() < Duration::from_secs(3));
        assert!(out.status.is_none() && !out.ok(), "partial output of a killed command is not whole");
        assert!(!slot.busy());
    }

    #[test]
    fn a_stop_kills_the_running_command_and_forbids_new_spawns() {
        static SLOT: ChildSlot = ChildSlot::new();
        let t = thread::spawn(|| run(&mut sh("exec sleep 30"), Duration::from_secs(20), 1000, Some(&SLOT)).unwrap());
        let start = Instant::now();
        while !SLOT.busy() && start.elapsed() < Duration::from_secs(5) {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(SLOT.busy(), "the running command is known");
        SLOT.close_and_kill();
        let out = t.join().unwrap();
        assert!(start.elapsed() < Duration::from_secs(5), "ended at once, not after its time");
        assert!(!out.ok());
        assert!(!SLOT.busy(), "reaped, and the slot cleared with it");
        assert!(SLOT.is_closed());
        assert!(run(&mut sh("echo should-not-run"), Duration::from_secs(5), 1000, Some(&SLOT)).is_err());
    }

    #[test]
    fn a_failed_command_is_not_ok() {
        let out = run(&mut sh("echo x; exit 3"), Duration::from_secs(5), 1000, None).unwrap();
        assert!(out.whole && !out.ok());
        assert!(run(&mut Command::new("/nonexistent/command"), Duration::from_secs(1), 10, None).is_err());
    }
}
