// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Runs a short system command with a time limit, collecting its output on a thread of
//! its own while it runs, so neither a slow command nor a full pipe can hold up the
//! caller: the command is killed when its time is up, and output that did not arrive
//! whole by then (or was cut at the size limit, or failed to read) is marked as such.

use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{mpsc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

pub struct Output {
    /// `None`: it did not end in time and was killed.
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

/// The command a run waits for, so that stopping can end it and know when it has gone.
/// The command is reaped (waited for) only while this is locked, and the slot is
/// cleared in the same step; an unreaped child keeps its pid, so a pid kept here always
/// still is that command, never another process that got the number later.
pub struct ChildSlot {
    pid: Mutex<Option<u32>>,
}

impl ChildSlot {
    pub const fn new() -> ChildSlot {
        ChildSlot { pid: Mutex::new(None) }
    }

    fn lock(&self) -> MutexGuard<'_, Option<u32>> {
        self.pid.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Kills the command if one runs (its run then reaps it and returns at once).
    pub fn kill(&self) {
        if let Some(pid) = *self.lock() {
            // SAFETY: kill on our own child that has not been reaped (see above).
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }

    /// Whether a command still runs (or was not reaped yet).
    pub fn busy(&self) -> bool {
        self.lock().is_some()
    }
}

impl Default for ChildSlot {
    fn default() -> Self {
        ChildSlot::new()
    }
}

/// How long the output may still take once the command has ended or was killed (a
/// process it left behind can keep the pipe open).
const OUTPUT_GRACE: Duration = Duration::from_millis(500);

/// Runs `cmd` (its stdin and stderr go nowhere) for at most `timeout`, keeping at most
/// `max` bytes of its output. `slot`: where the running command is kept, so that a stop
/// can kill it at once (see [`ChildSlot`]).
pub fn run(cmd: &mut Command, timeout: Duration, max: usize, slot: Option<&ChildSlot>) -> Result<Output, &'static str> {
    let deadline = Instant::now() + timeout;
    let held = slot.map(ChildSlot::lock);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "cannot start the command")?;
    if let Some(mut h) = held {
        *h = Some(child.id());
    }
    // Reaps the command (try_wait or wait) with the slot locked, clearing it in the same
    // step. `wait`: block until it has ended (after a kill).
    let reap = |child: &mut std::process::Child, wait: bool| {
        let mut h = slot.map(ChildSlot::lock);
        let r = if wait { child.wait().map(Some) } else { child.try_wait() };
        if !matches!(r, Ok(None)) {
            if let Some(h) = h.as_mut() {
                **h = None;
            }
        }
        r
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
                    // Past the limit: keep reading (so the command is not stuck on a full
                    // pipe) but keep nothing more.
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
            let _ = child.kill();
            let _ = reap(&mut child, true);
            return Err("cannot read the command's output");
        }
    }
    let status = loop {
        match reap(&mut child, false) {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = reap(&mut child, true);
                break None;
            }
        }
    };
    let wait = deadline.saturating_duration_since(Instant::now()).max(OUTPUT_GRACE);
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
        // 1 MiB of output: far more than a pipe holds.
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
    fn a_stop_kills_the_running_command_and_knows_when_it_has_gone() {
        static SLOT: ChildSlot = ChildSlot::new();
        let t = thread::spawn(|| run(&mut sh("exec sleep 30"), Duration::from_secs(20), 1000, Some(&SLOT)).unwrap());
        let start = Instant::now();
        while !SLOT.busy() && start.elapsed() < Duration::from_secs(5) {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(SLOT.busy(), "the running command is known");
        SLOT.kill();
        let out = t.join().unwrap();
        assert!(start.elapsed() < Duration::from_secs(5), "ended at once, not after its time");
        assert!(!out.ok());
        assert!(!SLOT.busy(), "reaped, and the slot cleared with it");
        SLOT.kill();
    }

    #[test]
    fn a_failed_command_is_not_ok() {
        let out = run(&mut sh("echo x; exit 3"), Duration::from_secs(5), 1000, None).unwrap();
        assert!(out.whole && !out.ok());
        assert!(run(&mut Command::new("/nonexistent/command"), Duration::from_secs(1), 10, None).is_err());
    }
}
