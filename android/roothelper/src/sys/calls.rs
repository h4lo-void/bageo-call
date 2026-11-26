// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The phone's call state, read by the helper itself with
//! `/system/bin/dumpsys telephony.registry` (see [`crate::calls`]). A command that is
//! missing, fails, is slow, or whose output is not read whole gives
//! [`CallStatus::Unknown`], never a guess.

use std::process::Command;
use std::time::Duration;

use super::child;
use crate::calls::{status_from_registry, CallStatus};
use crate::control::CallState;

const TIMEOUT: Duration = Duration::from_secs(3);
const MAX_OUTPUT: usize = 1 << 20;

pub fn call_status() -> CallStatus {
    let mut cmd = Command::new("/system/bin/dumpsys");
    cmd.arg("telephony.registry").env_clear().env("PATH", "/system/bin");
    match child::run(&mut cmd, TIMEOUT, MAX_OUTPUT, None) {
        Ok(out) if out.ok() => match std::str::from_utf8(&out.stdout) {
            Ok(text) => status_from_registry(text),
            Err(_) => CallStatus::Unknown,
        },
        _ => CallStatus::Unknown,
    }
}

pub struct DumpsysCalls;

impl CallState for DumpsysCalls {
    fn status(&mut self) -> CallStatus {
        call_status()
    }
}
