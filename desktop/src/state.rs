// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Shared application state, managed by Tauri.

use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use tokio::sync::Notify;

use crate::config::Config;
use crate::local::{AdbJob, Local, Supervisor};
use crate::notify::ToastState;
use crate::status::Snapshot;
use crate::window::Anchor;

pub struct Shared {
    pub snapshot: Mutex<Snapshot>,
    pub config: Mutex<Config>,
    /// Console password, cached in memory after it is read from the system keychain.
    pub password: Mutex<Option<String>>,
    /// Wakes the poller before its 5 second timer ends.
    pub notify: Notify,
    /// Set when the poller must forget its address, failure counters and cached values.
    pub reset: AtomicBool,
    pub anchor: Mutex<Option<Anchor>>,
    /// Logical height of the popup window.
    pub height: Mutex<f64>,
    /// When the popup last hid itself because it lost focus.
    pub hidden_at: Mutex<Option<Instant>>,
    /// The notification window's current content.
    pub toast: Mutex<ToastState>,
    /// Bageo found next to this program, if any. Set once at start.
    pub local: Option<Local>,
    /// The Bageo process this program started.
    pub sup: Mutex<Supervisor>,
    /// The Android tools download.
    pub adb_job: Mutex<AdbJob>,
}

impl Shared {
    pub fn new(config: Config, snapshot: Snapshot, local: Option<Local>) -> Self {
        Shared {
            snapshot: Mutex::new(snapshot),
            config: Mutex::new(config),
            password: Mutex::new(None),
            notify: Notify::new(),
            reset: AtomicBool::new(false),
            anchor: Mutex::new(None),
            height: Mutex::new(crate::window::DEFAULT_HEIGHT),
            hidden_at: Mutex::new(None),
            toast: Mutex::new(ToastState::default()),
            local,
            sup: Mutex::new(Supervisor::new()),
            adb_job: Mutex::new(AdbJob::default()),
        }
    }
}

/// Locks a mutex and keeps going if another thread panicked while holding it.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
