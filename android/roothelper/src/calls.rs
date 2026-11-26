// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Whether the phone is in a call, as the helper sees it for itself (never from the
//! app): the call state the telephony registry keeps per SIM, from
//! `dumpsys telephony.registry`.
//!
//! The dump (`TelephonyRegistry.dump`, the same in Android 10 to 16) starts like this,
//! indented by two spaces per level:
//!
//! ```text
//! last known state:
//!   Phone Id=0
//!     mCallState=0
//!     mRingingCallState=0
//!     ...
//!   Phone Id=1
//!     mCallState=2
//!     ...
//!   mCarrierNetworkChangeState=...      (Android 10; later versions: another line)
//! ```
//!
//! `mCallState` is 0 idle, 1 ringing, 2 off the hook. Only that layout is read: the first
//! line, then one block per SIM numbered from 0 whose first line is its call state, up
//! to the first line back at the block level. Anything else (another first line, a SIM
//! out of order, a state that is not 0, 1 or 2, no SIM, no end of the SIM blocks) is
//! [`CallStatus::Unknown`]; so is output that could not be read whole. A call counts only
//! while some SIM is off the hook.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallStatus {
    InCall,
    NotInCall,
    /// The state could not be read or understood.
    Unknown,
}

const HEADER: &str = "last known state:";
const PHONE: &str = "  Phone Id=";
const STATE: &str = "    mCallState=";

pub fn status_from_registry(text: &str) -> CallStatus {
    let mut lines = text.lines();
    if lines.next() != Some(HEADER) {
        return CallStatus::Unknown;
    }
    let mut phones = 0u32;
    let mut in_call = false;
    let mut ended = false;
    while let Some(line) = lines.next() {
        if line.is_empty() || line.starts_with("    ") {
            // Inside a SIM's block (or a line of one of its values).
            if phones == 0 {
                return CallStatus::Unknown;
            }
            continue;
        }
        if let Some(id) = line.strip_prefix(PHONE) {
            if ended || id != phones.to_string() {
                return CallStatus::Unknown;
            }
            match lines.next().and_then(|l| l.strip_prefix(STATE)) {
                Some("0") | Some("1") => {}
                Some("2") => in_call = true,
                _ => return CallStatus::Unknown,
            }
            phones += 1;
            continue;
        }
        if !ended {
            ended = true;
            if phones == 0 {
                return CallStatus::Unknown;
            }
        }
        // After the SIM blocks only another SIM block would matter; none may follow.
    }
    if !ended || phones == 0 {
        CallStatus::Unknown
    } else if in_call {
        CallStatus::InCall
    } else {
        CallStatus::NotInCall
    }
}

/// How many checks in a row must find no call before a call counts as over. A check that
/// could not tell changes nothing.
pub const END_AFTER: u32 = 2;

#[derive(Default)]
pub struct CallEnd {
    misses: u32,
}

impl CallEnd {
    /// Takes one check; true once the call counts as over.
    pub fn observe(&mut self, s: CallStatus) -> bool {
        match s {
            CallStatus::InCall => self.misses = 0,
            CallStatus::NotInCall => self.misses += 1,
            CallStatus::Unknown => {}
        }
        self.misses >= END_AFTER
    }
}

/// Checks the call every `every` until it is over (then runs `on_end` once) or it is
/// told to stop. It does not end by itself while the call goes on, so a stream opened
/// later in the same session is watched too.
pub struct Watch {
    stop: Arc<AtomicBool>,
    done: mpsc::Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

impl Watch {
    pub fn start<C, E>(every: Duration, mut check: C, on_end: E) -> std::io::Result<Watch>
    where
        C: FnMut() -> CallStatus + Send + 'static,
        E: FnOnce() + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, done) = mpsc::channel();
        let s2 = Arc::clone(&stop);
        let thread = thread::Builder::new().name("call-watch".into()).spawn(move || {
            let mut end = CallEnd::default();
            'watch: loop {
                let next = Instant::now() + every;
                while Instant::now() < next {
                    if s2.load(Ordering::SeqCst) {
                        break 'watch;
                    }
                    thread::sleep(Duration::from_millis(20).min(every));
                }
                let status = check();
                if s2.load(Ordering::SeqCst) {
                    break;
                }
                if end.observe(status) {
                    on_end();
                    break;
                }
            }
            let _ = tx.send(());
        })?;
        Ok(Watch { stop, done, thread: Some(thread) })
    }

    /// Whether it still watches (false once the call ended or it was stopped).
    pub fn is_running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    /// Stops it and waits at most `wait` (a check in progress can take a few seconds);
    /// false: it had not finished by then and is left to end on its own.
    pub fn stop(mut self, wait: Duration) -> bool {
        self.stop.store(true, Ordering::SeqCst);
        if self.done.recv_timeout(wait).is_ok() || self.thread.as_ref().is_some_and(|t| t.is_finished()) {
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn dump(states: &[&str]) -> String {
        let mut s = String::from("last known state:\n");
        for (i, st) in states.iter().enumerate() {
            s.push_str(&format!(
                "  Phone Id={i}\n    mCallState={st}\n    mRingingCallState=0\n    mCallIncomingNumber=\n    \
                 mServiceState={{mVoiceRegState=0(IN_SERVICE), mCallState=2 says nothing}}\n"
            ));
        }
        s.push_str("  mCarrierNetworkChangeState=false\n  mPhoneCapability=null\nlocal logs:\n  notifyCallState: mCallState=2\n");
        s
    }

    #[test]
    fn reads_each_sim_in_its_place() {
        assert_eq!(status_from_registry(&dump(&["0"])), CallStatus::NotInCall);
        assert_eq!(status_from_registry(&dump(&["0", "0"])), CallStatus::NotInCall);
        assert_eq!(status_from_registry(&dump(&["1", "0"])), CallStatus::NotInCall, "ringing is not a call yet");
        assert_eq!(status_from_registry(&dump(&["0", "2"])), CallStatus::InCall);
        assert_eq!(status_from_registry(&dump(&["2"])), CallStatus::InCall);
    }

    #[test]
    fn values_of_other_fields_do_not_count() {
        // "mCallState=2" inside another value, a later section or the log is not a state.
        let idle = dump(&["0", "0"]);
        assert!(idle.matches("mCallState=2").count() >= 2);
        assert_eq!(status_from_registry(&idle), CallStatus::NotInCall);
        // A value spread over several lines (each one indented by the dump) is skipped.
        let multi = idle.replace("    mRingingCallState=0\n", "    mCellInfo=[a,\n    b]\n\n    mRingingCallState=0\n");
        assert_eq!(status_from_registry(&multi), CallStatus::NotInCall);
    }

    #[test]
    fn anything_unclear_is_unknown() {
        for bad in [
            String::new(),
            "Can't find service: telephony.registry\n".to_string(),
            "Permission Denial: can't dump TelephonyRegistry\n".to_string(),
            "  mCallState=2\n".to_string(),
            dump(&["0"]).replacen("last known state:\n", "", 1),
            dump(&["0", "2"]).replace("Phone Id=1", "Phone Id=2"),
            dump(&["0", "0"]).replace("Phone Id=1", "Phone Id=0"),
            dump(&["20"]),
            dump(&["2 "]),
            dump(&["x"]),
            "last known state:\n  mCarrierNetworkChangeState=false\n".to_string(),
            "last known state:\n    mCallState=2\n  Phone Id=0\n    mCallState=0\n  x=1\n".to_string(),
            // The state is not the first line of its block.
            dump(&["0"]).replace("    mCallState=0\n    mRingingCallState=0\n", "    mRingingCallState=0\n    mCallState=0\n"),
            // Cut off inside the SIM blocks: their end was never seen.
            "last known state:\n  Phone Id=0\n    mCallState=0\n    mRingingCallState=0\n".to_string(),
            // A SIM block after the end of the SIM blocks.
            format!("{}  Phone Id=1\n    mCallState=2\n", dump(&["0"])),
        ] {
            assert_eq!(status_from_registry(&bad), CallStatus::Unknown, "{bad:?}");
        }
    }

    #[test]
    fn a_call_ends_after_two_clear_misses_in_a_row() {
        use CallStatus::*;
        let mut e = CallEnd::default();
        assert!(!e.observe(InCall));
        assert!(!e.observe(NotInCall));
        assert!(!e.observe(Unknown), "a failed check changes nothing");
        assert!(!e.observe(InCall), "the call went on");
        assert!(!e.observe(NotInCall));
        assert!(!e.observe(Unknown));
        assert!(e.observe(NotInCall));
        let mut e = CallEnd::default();
        for _ in 0..20 {
            assert!(!e.observe(Unknown), "never knowing is not an end");
        }
    }

    fn scripted(seq: Vec<CallStatus>) -> (impl FnMut() -> CallStatus + Send + 'static, Arc<Mutex<usize>>) {
        let n = Arc::new(Mutex::new(0usize));
        let n2 = Arc::clone(&n);
        let f = move || {
            let mut i = n2.lock().unwrap();
            let s = seq.get(*i).copied().unwrap_or(CallStatus::InCall);
            *i += 1;
            s
        };
        (f, n)
    }

    #[test]
    fn the_watch_ends_the_call_once_and_only_when_it_is_over() {
        use CallStatus::*;
        let (check, count) = scripted(vec![InCall, Unknown, NotInCall, Unknown, InCall, NotInCall, NotInCall, InCall]);
        let (tx, rx) = mpsc::channel();
        let w = Watch::start(Duration::from_millis(5), check, move || tx.send(()).unwrap()).unwrap();
        rx.recv_timeout(Duration::from_secs(5)).expect("the call end is seen");
        assert_eq!(*count.lock().unwrap(), 7, "ended at the second miss in a row");
        let start = Instant::now();
        while w.is_running() && start.elapsed() < Duration::from_secs(2) {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(!w.is_running());
        assert!(rx.try_recv().is_err(), "once");
        assert!(w.stop(Duration::from_millis(100)));
    }

    #[test]
    fn a_stopped_watch_does_nothing_more() {
        let (check, count) = scripted(vec![]);
        let (tx, rx) = mpsc::channel::<()>();
        let w = Watch::start(Duration::from_millis(5), check, move || tx.send(()).unwrap()).unwrap();
        thread::sleep(Duration::from_millis(40));
        assert!(w.is_running(), "it keeps watching while the call goes on");
        assert!(w.stop(Duration::from_secs(1)));
        let seen = *count.lock().unwrap();
        thread::sleep(Duration::from_millis(30));
        assert_eq!(*count.lock().unwrap(), seen);
        assert!(rx.try_recv().is_err());
    }
}
