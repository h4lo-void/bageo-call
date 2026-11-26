// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Runs a short system command with a time limit, collecting its output on a thread of
//! its own while it runs, so neither a slow command nor a full pipe can hold up the
//! caller: the command is killed when its time is up, and output that did not arrive
//! whole by then (or was cut at the size limit, or failed to read) is marked as such.

use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc;
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

/// How long the output may still take once the command has ended or was killed (a
/// process it left behind can keep the pipe open).
const OUTPUT_GRACE: Duration = Duration::from_millis(500);

/// Runs `cmd` (its stdin and stderr go nowhere) for at most `timeout`, keeping at most
/// `max` bytes of its output. `pid`: where the running command's pid is kept, so that a
/// stop signal can kill it at once.
pub fn run(cmd: &mut Command, timeout: Duration, max: usize, pid: Option<&AtomicI32>) -> Result<Output, &'static str> {
    let deadline = Instant::now() + timeout;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "cannot start the command")?;
    if let Some(p) = pid {
        p.store(child.id() as i32, Ordering::SeqCst);
    }
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
            let _ = child.wait();
            if let Some(p) = pid {
                p.store(0, Ordering::SeqCst);
            }
            return Err("cannot read the command's output");
        }
    }
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    if let Some(p) = pid {
        p.store(0, Ordering::SeqCst);
    }
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
        let pid = AtomicI32::new(0);
        let start = Instant::now();
        let out = run(&mut sh("echo partial; exec sleep 30"), Duration::from_millis(200), 1000, Some(&pid)).unwrap();
        assert!(start.elapsed() < Duration::from_secs(3));
        assert!(out.status.is_none() && !out.ok(), "partial output of a killed command is not whole");
        assert_eq!(pid.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_failed_command_is_not_ok() {
        let out = run(&mut sh("echo x; exit 3"), Duration::from_secs(5), 1000, None).unwrap();
        assert!(out.whole && !out.ok());
        assert!(run(&mut Command::new("/nonexistent/command"), Duration::from_secs(1), 10, None).is_err());
    }
}
