// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The phone's call state, read by the helper itself with
//! `/system/bin/dumpsys telephony.registry` (see [`crate::calls`]). Any failure (the
//! command missing, slow or unreadable) counts as "not in a call".

use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::calls::in_call_from_registry;
use crate::control::CallState;

const TIMEOUT: Duration = Duration::from_secs(3);
const MAX_OUTPUT: u64 = 1 << 20;

pub fn in_call_now() -> bool {
    let Ok(mut child) = Command::new("/system/bin/dumpsys")
        .arg("telephony.registry")
        .env_clear()
        .env("PATH", "/system/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    // Read while it runs (its output can be larger than a pipe holds), within the time.
    let mut out = Vec::new();
    let reader = child.stdout.take().map(|s| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = s.take(MAX_OUTPUT).read_to_end(&mut buf);
            buf
        })
    });
    let start = Instant::now();
    let ok = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s.success(),
            Ok(None) if start.elapsed() < TIMEOUT => thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    if let Some(r) = reader {
        if let Ok(buf) = r.join() {
            out = buf;
        }
    }
    ok && in_call_from_registry(&String::from_utf8_lossy(&out))
}

pub struct DumpsysCalls;

impl CallState for DumpsysCalls {
    fn in_call(&mut self) -> bool {
        in_call_now()
    }
}
