// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Who may write to the phone, and how the helper stops.
//!
//! Every change and every putting back happens while holding one lock. Stopping first
//! marks the helper as stopping, after which no request may change anything, then waits
//! a bounded time for the lock. Whoever holds it then is the only one left that may
//! write: it puts back what the state file holds, and the process ends while it still
//! holds the lock, so nothing can be written after the values were put back.
//!
//! When the lock stays busy (a write that does not return in time), nothing is put back
//! from there and the state file is kept as it is. The process ends anyway, which ends
//! that write, and a restore that takes the helper's one-copy file lock once the process
//! has gone puts the values back (see `restore --wait`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Life {
    lock: Mutex<()>,
    stopping: AtomicBool,
}

impl Life {
    pub fn new() -> Life {
        Life::default()
    }

    /// Held while anything is changed or put back.
    pub fn hold(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// No request may change anything any more.
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
    }

    pub fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// Stops new changes, then waits at most `wait` for the lock. `Some`: the caller is
    /// the only one left that may write. `None`: the lock is still busy; the caller must
    /// not put anything back.
    pub fn hold_for_exit(&self, wait: Duration) -> Option<MutexGuard<'_, ()>> {
        self.stop();
        let start = Instant::now();
        loop {
            match self.lock.try_lock() {
                Ok(g) => return Some(g),
                Err(TryLockError::Poisoned(p)) => return Some(p.into_inner()),
                Err(TryLockError::WouldBlock) if start.elapsed() < wait => thread::sleep(Duration::from_millis(10)),
                Err(TryLockError::WouldBlock) => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Arc};

    #[test]
    fn a_free_lock_is_taken_and_new_changes_stop() {
        let life = Life::new();
        assert!(!life.stopping());
        let g = life.hold_for_exit(Duration::from_millis(50));
        assert!(g.is_some());
        assert!(life.stopping());
    }

    #[test]
    fn a_write_that_does_not_return_keeps_the_exit_from_putting_back() {
        let life = Arc::new(Life::new());
        let (held, release) = (mpsc::channel(), mpsc::channel::<()>());
        let l2 = Arc::clone(&life);
        let writer = thread::spawn(move || {
            let _g = l2.hold();
            held.0.send(()).unwrap();
            let _ = release.1.recv();
        });
        held.1.recv().unwrap();
        let start = Instant::now();
        assert!(life.hold_for_exit(Duration::from_millis(100)).is_none(), "busy: nothing may be put back");
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(life.stopping());
        release.0.send(()).unwrap();
        writer.join().unwrap();
        assert!(life.hold_for_exit(Duration::from_millis(100)).is_some(), "free once the write ended");
    }

    #[test]
    fn a_poisoned_lock_still_lets_the_exit_put_back() {
        let life = Arc::new(Life::new());
        let l2 = Arc::clone(&life);
        let _ = thread::spawn(move || {
            let _g = l2.hold();
            panic!("a request failed");
        })
        .join();
        assert!(life.hold_for_exit(Duration::from_millis(50)).is_some());
    }
}
