//! Confirmation of what has side effects (a call, a phone's mode, a to-do).
//!
//! The chat gets a summary and a code; only the person who asked, in the same
//! chat, can go ahead, within the time limit and once. A newer request in the
//! chat replaces an older one still open.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::calls_http::PhoneMode;

/// What runs once the owner says yes: already resolved (the number or contact
/// id, the phone's serial, the to-do's id), so it does exactly what the
/// summary said.
#[derive(Debug, Clone, PartialEq)]
pub enum Job {
    /// The body for errands::create (number or contact_id, task).
    Errand { body: Value },
    Mode { serial: String, mode: PhoneMode },
    Decide { task_id: String, confirm: bool },
    /// A line for the AI on a call or an answer to its question, read from
    /// free text: done once the owner says yes.
    Act { action: super::command::Action },
}

struct Pending {
    code: String,
    sender: String,
    job: Job,
    at: Instant,
}

pub struct Confirmations {
    ttl: Duration,
    open: Mutex<HashMap<String, Pending>>,
}

fn new_code() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..6].to_ascii_uppercase()
}

impl Confirmations {
    pub fn new(ttl: Duration) -> Self {
        Self { ttl, open: Mutex::new(HashMap::new()) }
    }

    fn open(&self) -> std::sync::MutexGuard<'_, HashMap<String, Pending>> {
        self.open.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Waits for the owner's yes; its code.
    pub fn ask(&self, chat: &str, sender: &str, job: Job) -> String {
        let code = new_code();
        let pending = Pending { code: code.clone(), sender: sender.trim().to_string(), job, at: Instant::now() };
        self.open().insert(chat.to_string(), pending);
        code
    }

    /// Something waits in this chat (it may have expired: taking it says so).
    pub fn waiting(&self, chat: &str) -> bool {
        self.open().contains_key(chat)
    }

    /// The job waiting in this chat, taken (it is used once). code: the one a
    /// button carries; None for a typed 1 / 2 (the chat's newest). None when
    /// there is none, it expired, it is another one (a stale button), or the
    /// sender is not the one who asked. An expired one is dropped.
    pub fn take(&self, chat: &str, sender: &str, code: Option<&str>) -> Option<Job> {
        let mut open = self.open();
        let p = open.get(chat)?;
        if p.at.elapsed() > self.ttl {
            open.remove(chat);
            return None;
        }
        if code.is_some_and(|c| !c.trim().eq_ignore_ascii_case(&p.code)) || p.sender != sender.trim() {
            return None;
        }
        open.remove(chat).map(|p| p.job)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> Job {
        Job::Decide { task_id: "t1".into(), confirm: true }
    }

    #[test]
    fn a_confirmation_works_once_for_the_one_who_asked() {
        let c = Confirmations::new(Duration::from_secs(120));
        let code = c.ask("chat", "42", job());
        assert_eq!(code.len(), 6);
        assert_eq!(c.take("chat", "43", None), None, "another member of the chat");
        assert_eq!(c.take("other", "42", None), None, "another chat");
        assert_eq!(c.take("chat", "42", Some("ZZZZZZ")), None, "a stale button");
        assert_eq!(c.take("chat", "42", Some(&code.to_lowercase())), Some(job()));
        assert_eq!(c.take("chat", "42", Some(&code)), None, "used already");
        assert!(!c.waiting("chat"));
    }

    #[test]
    fn a_newer_request_replaces_the_older_one() {
        let c = Confirmations::new(Duration::from_secs(120));
        let old = c.ask("chat", "42", job());
        let new = c.ask("chat", "42", Job::Decide { task_id: "t2".into(), confirm: false });
        assert_eq!(c.take("chat", "42", Some(&old)), None);
        assert_eq!(c.take("chat", "42", Some(&new)), Some(Job::Decide { task_id: "t2".into(), confirm: false }));
    }

    #[test]
    fn an_expired_confirmation_does_nothing() {
        let c = Confirmations::new(Duration::ZERO);
        let code = c.ask("chat", "42", job());
        std::thread::sleep(Duration::from_millis(5));
        assert!(c.waiting("chat"));
        assert_eq!(c.take("chat", "42", Some(&code)), None);
        assert!(!c.waiting("chat"), "dropped once seen expired");
    }
}

