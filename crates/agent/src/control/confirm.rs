// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

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

/// 128 random bits as 32 hex digits: too many to guess. Two v4 UUIDs, the
/// second turned half-way round, so the fixed version and variant bits of
/// one fall on random bits of the other. A button's data ("k:" + code +
/// ":y", 36 bytes) stays within BUTTON_DATA_MAX.
pub fn new_code() -> String {
    let bits = uuid::Uuid::new_v4().as_u128() ^ uuid::Uuid::new_v4().as_u128().rotate_left(64);
    format!("{bits:032X}")
}

/// How many digits of a code the chat text shows (the button carries all).
pub const CODE_SHOWN: usize = 6;

/// The sender of a confirmation no one in the chat asked for (an MCP
/// client's call): anyone allowed to give commands there may answer it.
pub const ANY_SENDER: &str = "*";

impl Confirmations {
    pub fn new(ttl: Duration) -> Self {
        Self { ttl, open: Mutex::new(HashMap::new()) }
    }

    fn open(&self) -> std::sync::MutexGuard<'_, HashMap<String, Pending>> {
        self.open.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Waits for the owner's yes; its code.
    pub fn ask(&self, chat: &str, sender: &str, job: Job) -> String {
        self.ask_many(&[chat.to_string()], sender, job)
    }

    /// The same job waiting in several chats under one code: the first
    /// answer in any of them settles it everywhere.
    pub fn ask_many(&self, chats: &[String], sender: &str, job: Job) -> String {
        let code = new_code();
        let mut open = self.open();
        for chat in chats {
            let pending = Pending { code: code.clone(), sender: sender.trim().to_string(), job: job.clone(), at: Instant::now() };
            open.insert(chat.clone(), pending);
        }
        code
    }

    /// Something waits in this chat (it may have expired: taking it says so).
    pub fn waiting(&self, chat: &str) -> bool {
        self.open().contains_key(chat)
    }

    /// The job waiting in this chat is an errand that asked questions.
    pub fn has_gapped_errand(&self, chat: &str) -> bool {
        self.open().get(chat).is_some_and(|p| {
            matches!(&p.job, Job::Errand { body } if body.get("preflight_gaps").and_then(Value::as_bool) == Some(true))
        })
    }

    /// The errand waiting in this chat that asked questions (its body has
    /// `preflight_gaps`), taken like [Self::take] without a code: the owner's
    /// reply is the answer and the call goes ahead.
    pub fn take_gapped_errand(&self, chat: &str, sender: &str) -> Option<Job> {
        let mut open = self.open();
        let p = open.get(chat)?;
        let gapped = matches!(&p.job, Job::Errand { body } if body.get("preflight_gaps").and_then(Value::as_bool) == Some(true));
        if !gapped || p.at.elapsed() > self.ttl || (p.sender != ANY_SENDER && p.sender != sender.trim()) {
            return None;
        }
        let taken = open.remove(chat)?;
        open.retain(|_, q| q.code != taken.code);
        Some(taken.job)
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
        if code.is_some_and(|c| !c.trim().eq_ignore_ascii_case(&p.code)) || (p.sender != ANY_SENDER && p.sender != sender.trim()) {
            return None;
        }
        let taken = open.remove(chat)?;
        open.retain(|_, q| q.code != taken.code);
        Some(taken.job)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_code_in_many_chats_is_settled_by_the_first_answer_from_anyone_there() {
        let c = Confirmations::new(Duration::from_secs(60));
        let chats = vec!["tg:1".to_string(), "tg:2".to_string()];
        let code = c.ask_many(&chats, ANY_SENDER, Job::Decide { task_id: "t1".into(), confirm: true });
        assert!(c.waiting("tg:1") && c.waiting("tg:2"));
        assert!(c.take("tg:2", "someone", Some("WRONG")).is_none());
        assert!(c.take("tg:2", "someone", Some(&code)).is_some());
        assert!(!c.waiting("tg:1"), "answered in one chat: gone from the other");
        assert!(c.take("tg:1", "someone", Some(&code)).is_none());
    }

    fn job() -> Job {
        Job::Decide { task_id: "t1".into(), confirm: true }
    }

    #[test]
    fn a_reply_settles_only_an_errand_card_that_asked_questions() {
        let c = Confirmations::new(Duration::from_secs(60));
        let plain = Job::Errand { body: serde_json::json!({"number": "0912345678", "task": "x"}) };
        c.ask("chat", "42", plain.clone());
        assert_eq!(c.take_gapped_errand("chat", "42"), None);
        assert!(c.waiting("chat"), "a card without questions stays for 1 / 2");
        let gapped = Job::Errand { body: serde_json::json!({"number": "0912345678", "task": "x", "preflight_gaps": true}) };
        c.ask("chat", "42", gapped.clone());
        assert_eq!(c.take_gapped_errand("chat", "7"), None, "another member cannot answer it");
        assert_eq!(c.take_gapped_errand("chat", "42"), Some(gapped));
        assert_eq!(c.take_gapped_errand("chat", "42"), None, "it is used once");
        let other = c.ask("chat", "42", job());
        assert_eq!(c.take_gapped_errand("chat", "42"), None);
        assert!(c.take("chat", "42", Some(&other)).is_some());
    }

    #[test]
    fn a_confirmation_works_once_for_the_one_who_asked() {
        let c = Confirmations::new(Duration::from_secs(120));
        let code = c.ask("chat", "42", job());
        assert_eq!(code.len(), 32);
        assert!(code.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert!(format!("k:{code}:y").len() <= super::super::BUTTON_DATA_MAX);
        assert_eq!(c.take("chat", "43", None), None, "another member of the chat");
        assert_eq!(c.take("other", "42", None), None, "another chat");
        assert_eq!(c.take("chat", "42", Some("ZZZZZZ")), None, "a stale button");
        assert_eq!(c.take("chat", "42", Some(&code[..CODE_SHOWN])), None, "the digits the text shows are not enough");
        assert_eq!(c.take("chat", "42", Some(&code.to_lowercase())), Some(job()));
        assert_eq!(c.take("chat", "42", Some(&code)), None, "used already");
        assert!(!c.waiting("chat"));
    }

    #[test]
    fn a_newer_request_replaces_the_older_one() {
        // Codes do not repeat and use every bit position.
        let codes: std::collections::HashSet<String> = (0..2000).map(|_| new_code()).collect();
        assert_eq!(codes.len(), 2000);
        let ored = codes.iter().fold(0u128, |acc, c| acc | u128::from_str_radix(c, 16).unwrap());
        let anded = codes.iter().fold(u128::MAX, |acc, c| acc & u128::from_str_radix(c, 16).unwrap());
        assert_eq!((ored, anded), (u128::MAX, 0), "a fixed bit in the code");
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

