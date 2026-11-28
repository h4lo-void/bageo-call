// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Confirmation of what has side effects (a call, a phone's mode, a to-do).
//!
//! The chat gets a summary and a code; only the person who asked, in the same
//! chat, can go ahead, within the time limit and once. Several can wait in
//! one chat, each under its own code. A card nobody in the chat asked for
//! (an MCP client's) is settled only by the button, which carries the whole
//! code.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
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
    sender: String,
    job: Job,
    at: Instant,
    /// Insertion order, so "oldest" stays the one put in first.
    seq: u64,
}

/// How many cards one chat keeps. A newer one drops the oldest card the
/// chat itself asked for. A chat that is already full of MCP cards takes
/// no further MCP card.
pub const MAX_PER_CHAT: usize = 20;

/// chat → code → the card. One code may sit in several chats ([`Confirmations::ask_many`]);
/// the first answer removes it from all of them.
type Open = HashMap<String, HashMap<String, Pending>>;

pub struct Confirmations {
    ttl: Duration,
    open: Mutex<Open>,
    seq: AtomicU64,
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
/// client's call): anyone allowed to give commands there may answer it,
/// and only with the button.
pub const ANY_SENDER: &str = "*";

/// What a typed 1 / 2 (no code) may do in this chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypedReply {
    /// Nothing is waiting.
    Absent,
    /// Exactly one card, and it is not from an MCP client.
    One,
    /// Several cards, or the only one came from an MCP client.
    UseButton,
}

impl Confirmations {
    pub fn new(ttl: Duration) -> Self {
        Self { ttl, open: Mutex::new(HashMap::new()), seq: AtomicU64::new(1) }
    }

    fn open(&self) -> std::sync::MutexGuard<'_, Open> {
        self.open.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Waits for the owner's yes; its code. Other cards in the chat stay.
    /// None when this chat is already full of MCP cards and this one is too.
    pub fn ask(&self, chat: &str, sender: &str, job: Job) -> Option<String> {
        self.ask_many(&[chat.to_string()], sender, job)
    }

    /// The same job waiting in several chats under one code: the first
    /// answer in any of them settles it everywhere. Cards already waiting
    /// in those chats stay. Expired cards are dropped first. Past
    /// [MAX_PER_CHAT], the oldest card the chat itself asked for is dropped
    /// to make room. A chat already full of MCP cards is skipped when this
    /// card is one too. None when every chat was skipped.
    pub fn ask_many(&self, chats: &[String], sender: &str, job: Job) -> Option<String> {
        let sender = sender.trim().to_string();
        let mcp = sender == ANY_SENDER;
        let mut open = self.open();
        let mut kept = Vec::new();
        for chat in chats {
            if room_for(&mut open, chat, self.ttl, mcp) {
                kept.push(chat.clone());
            }
        }
        if kept.is_empty() {
            return None;
        }
        let code = new_code();
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let at = Instant::now();
        for chat in kept {
            open.entry(chat).or_default().insert(
                code.clone(),
                Pending { sender: sender.clone(), job: job.clone(), at, seq },
            );
        }
        Some(code)
    }

    /// A card this chat asked for is still inside its time limit.
    pub fn has_live_owner(&self, chat: &str) -> bool {
        let open = self.open();
        open.get(chat).is_some_and(|cards| cards.values().any(|p| !is_mcp(p) && p.at.elapsed() <= self.ttl))
    }

    /// A typed 1 / 2 on a chat that has no buttons, when several of this
    /// sender's own cards are waiting and none of the live cards is from an
    /// MCP client: the oldest of this sender's. None otherwise.
    pub fn take_oldest_owner(&self, chat: &str, sender: &str) -> Option<Job> {
        let mut open = self.open();
        purge_expired(&mut open, chat, self.ttl);
        let pick = {
            let cards = open.get(chat)?;
            let live: Vec<&Pending> = cards.values().filter(|p| p.at.elapsed() <= self.ttl).collect();
            if live.len() < 2 || live.iter().any(|p| is_mcp(p)) {
                return None;
            }
            let sender = sender.trim();
            cards
                .iter()
                .filter(|(_, p)| !is_mcp(p) && p.sender == sender)
                .min_by_key(|(_, p)| p.seq)
                .map(|(code, p)| (code.clone(), p.job.clone()))
        };
        let (code, job) = pick?;
        remove_code(&mut open, &code);
        Some(job)
    }

    /// Something waits in this chat (it may have expired: taking it says so).
    pub fn waiting(&self, chat: &str) -> bool {
        self.open().get(chat).is_some_and(|cards| !cards.is_empty())
    }

    /// An errand card that asked questions is waiting, and it is not from an
    /// MCP client (it may have expired: taking it says so).
    pub fn has_gapped_errand(&self, chat: &str) -> bool {
        self.open().get(chat).is_some_and(|cards| cards.values().any(|p| !is_mcp(p) && is_gapped(&p.job)))
    }

    /// How many of this sender's cards asked questions and can still be
    /// answered: not from an MCP client, and not expired.
    pub fn live_gapped(&self, chat: &str, sender: &str) -> usize {
        let open = self.open();
        let Some(cards) = open.get(chat) else { return 0 };
        collect_gapped(cards, sender, self.ttl).live.len()
    }

    /// What a typed 1 / 2 may do. When every card has expired, the answer is
    /// still [TypedReply::One] so taking it can say so. Expired cards sitting
    /// beside a live one are dropped here.
    pub fn typed(&self, chat: &str) -> TypedReply {
        let mut open = self.open();
        let (live_n, expired) = {
            let Some(cards) = open.get(chat) else { return TypedReply::Absent };
            if cards.is_empty() {
                return TypedReply::Absent;
            }
            let live_n = cards.values().filter(|p| p.at.elapsed() <= self.ttl).count();
            (live_n, cards.len() - live_n)
        };
        if live_n > 0 && expired > 0 {
            purge_expired(&mut open, chat, self.ttl);
        }
        let Some(cards) = open.get(chat) else { return TypedReply::Absent };
        let live: Vec<&Pending> = cards.values().filter(|p| p.at.elapsed() <= self.ttl).collect();
        if live.is_empty() {
            return TypedReply::One;
        }
        if live.len() == 1 && !is_mcp(live[0]) {
            TypedReply::One
        } else {
            TypedReply::UseButton
        }
    }

    /// The errand waiting in this chat that asked questions (its body has
    /// `preflight_gaps`), taken like [Self::take] without a code: the owner's
    /// reply is the answer and the call goes ahead. An MCP client's card is
    /// left where it is. None when there is not exactly one of this sender's.
    pub fn take_gapped_errand(&self, chat: &str, sender: &str) -> Option<Job> {
        let mut open = self.open();
        let set = {
            let cards = open.get(chat)?;
            collect_gapped(cards, sender, self.ttl)
        };
        for code in &set.expired {
            remove_code(&mut open, code);
        }
        if set.live.len() != 1 {
            return None;
        }
        let (code, job) = set.live.into_iter().next()?;
        remove_code(&mut open, &code);
        Some(job)
    }

    /// The job for this code, taken (it is used once). `code`: the one a
    /// button carries; None for a typed 1 / 2, and only when this chat has
    /// exactly one card and it is not from an MCP client. None when there
    /// is none, it expired, it is another one (a stale button), the sender
    /// is not the one who asked, or a typed answer must not choose. An
    /// expired one is dropped.
    pub fn take(&self, chat: &str, sender: &str, code: Option<&str>) -> Option<Job> {
        let mut open = self.open();
        match code {
            Some(code) => take_code(&mut open, chat, sender, code, self.ttl),
            None => take_typed(&mut open, chat, sender, self.ttl),
        }
    }
}

fn is_mcp(p: &Pending) -> bool {
    p.sender == ANY_SENDER
}

fn is_gapped(job: &Job) -> bool {
    matches!(job, Job::Errand { body } if body.get("preflight_gaps").and_then(Value::as_bool) == Some(true))
}

struct GappedSet {
    expired: Vec<String>,
    live: Vec<(String, Job)>,
}

fn collect_gapped(cards: &HashMap<String, Pending>, sender: &str, ttl: Duration) -> GappedSet {
    let sender = sender.trim();
    let mut expired = Vec::new();
    let mut live = Vec::new();
    for (code, p) in cards {
        if is_mcp(p) || p.sender != sender || !is_gapped(&p.job) {
            continue;
        }
        if p.at.elapsed() > ttl {
            expired.push(code.clone());
        } else {
            live.push((code.clone(), p.job.clone()));
        }
    }
    GappedSet { expired, live }
}

fn remove_code(open: &mut Open, code: &str) {
    for cards in open.values_mut() {
        cards.remove(code);
    }
    open.retain(|_, cards| !cards.is_empty());
}

fn code_key(cards: &HashMap<String, Pending>, code: &str) -> Option<String> {
    let want = code.trim();
    cards.keys().find(|k| k.eq_ignore_ascii_case(want)).cloned()
}

fn take_code(open: &mut Open, chat: &str, sender: &str, code: &str, ttl: Duration) -> Option<Job> {
    let key = {
        let cards = open.get(chat)?;
        code_key(cards, code)?
    };
    let expired = open.get(chat)?.get(&key)?.at.elapsed() > ttl;
    if expired {
        if let Some(cards) = open.get_mut(chat) {
            cards.remove(&key);
        }
        if open.get(chat).is_some_and(|cards| cards.is_empty()) {
            open.remove(chat);
        }
        return None;
    }
    let pending = open.get(chat)?.get(&key)?;
    if pending.sender != ANY_SENDER && pending.sender != sender.trim() {
        return None;
    }
    let job = pending.job.clone();
    remove_code(open, &key);
    Some(job)
}

fn purge_expired(open: &mut Open, chat: &str, ttl: Duration) {
    let Some(cards) = open.get_mut(chat) else { return };
    cards.retain(|_, p| p.at.elapsed() <= ttl);
    if cards.is_empty() {
        open.remove(chat);
    }
}

fn drop_one(open: &mut Open, chat: &str, code: &str) {
    let empty = if let Some(cards) = open.get_mut(chat) {
        cards.remove(code);
        cards.is_empty()
    } else {
        false
    };
    if empty {
        open.remove(chat);
    }
}

/// Expired cards in this chat are gone. True when one more card fits.
/// At the limit, a card from an MCP client gets no room (it never pushes
/// out a card the owner is waiting on); one the chat asked for pushes out
/// the oldest such card, else the oldest card.
fn room_for(open: &mut Open, chat: &str, ttl: Duration, mcp_new: bool) -> bool {
    purge_expired(open, chat, ttl);
    let len = open.get(chat).map(|cards| cards.len()).unwrap_or(0);
    if len < MAX_PER_CHAT {
        return true;
    }
    if mcp_new {
        return false;
    }
    let oldest_owner = open.get(chat).and_then(|cards| {
        cards.iter().filter(|(_, p)| !is_mcp(p)).min_by_key(|(_, p)| p.seq).map(|(code, _)| code.clone())
    });
    if let Some(code) = oldest_owner {
        drop_one(open, chat, &code);
        return true;
    }
    let oldest = open.get(chat).and_then(|cards| cards.iter().min_by_key(|(_, p)| p.seq).map(|(code, _)| code.clone()));
    if let Some(code) = oldest {
        drop_one(open, chat, &code);
    }
    true
}

/// A typed 1 / 2. Drops expired cards in this chat first. Takes the card
/// only when one remains and it is not from an MCP client.
fn take_typed(open: &mut Open, chat: &str, sender: &str, ttl: Duration) -> Option<Job> {
    purge_expired(open, chat, ttl);
    let cards = open.get(chat)?;
    if cards.len() != 1 {
        return None;
    }
    let (code, pending) = cards.iter().next()?;
    if is_mcp(pending) || pending.sender != sender.trim() {
        return None;
    }
    let code = code.clone();
    let job = pending.job.clone();
    remove_code(open, &code);
    Some(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_code_in_many_chats_is_settled_by_the_first_answer_from_anyone_there() {
        let c = Confirmations::new(Duration::from_secs(60));
        let chats = vec!["tg:1".to_string(), "tg:2".to_string()];
        let code = c.ask_many(&chats, ANY_SENDER, Job::Decide { task_id: "t1".into(), confirm: true }).unwrap();
        assert!(c.waiting("tg:1") && c.waiting("tg:2"));
        assert!(c.take("tg:2", "someone", Some("WRONG")).is_none());
        assert!(c.take("tg:2", "someone", Some(&code)).is_some());
        assert!(!c.waiting("tg:1"), "answered in one chat: gone from the other");
        assert!(c.take("tg:1", "someone", Some(&code)).is_none());
    }

    fn job() -> Job {
        Job::Decide { task_id: "t1".into(), confirm: true }
    }

    fn gapped(task: &str) -> Job {
        Job::Errand { body: serde_json::json!({"number": "0912345678", "task": task, "preflight_gaps": true}) }
    }

    #[test]
    fn a_reply_settles_only_an_errand_card_that_asked_questions() {
        let c = Confirmations::new(Duration::from_secs(60));
        let plain = Job::Errand { body: serde_json::json!({"number": "0912345678", "task": "x"}) };
        c.ask("chat", "42", plain.clone()).unwrap();
        assert_eq!(c.take_gapped_errand("chat", "42"), None);
        assert!(c.waiting("chat"), "a card without questions stays for 1 / 2");
        let gapped = gapped("x");
        c.ask("chat", "42", gapped.clone()).unwrap();
        assert_eq!(c.take_gapped_errand("chat", "7"), None, "another member cannot answer it");
        assert_eq!(c.take_gapped_errand("chat", "42"), Some(gapped));
        assert_eq!(c.take_gapped_errand("chat", "42"), None, "it is used once");
        let other = c.ask("chat", "42", job()).unwrap();
        assert_eq!(c.take_gapped_errand("chat", "42"), None);
        assert!(c.take("chat", "42", Some(&other)).is_some());
    }

    #[test]
    fn a_confirmation_works_once_for_the_one_who_asked() {
        let c = Confirmations::new(Duration::from_secs(120));
        let code = c.ask("chat", "42", job()).unwrap();
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
    fn two_cards_in_one_chat_do_not_replace_each_other() {
        // Codes do not repeat and use every bit position.
        let codes: std::collections::HashSet<String> = (0..2000).map(|_| new_code()).collect();
        assert_eq!(codes.len(), 2000);
        let ored = codes.iter().fold(0u128, |acc, c| acc | u128::from_str_radix(c, 16).unwrap());
        let anded = codes.iter().fold(u128::MAX, |acc, c| acc & u128::from_str_radix(c, 16).unwrap());
        assert_eq!((ored, anded), (u128::MAX, 0), "a fixed bit in the code");
        let c = Confirmations::new(Duration::from_secs(120));
        let old = c.ask("chat", "42", job()).unwrap();
        let new = c.ask("chat", "42", Job::Decide { task_id: "t2".into(), confirm: false }).unwrap();
        assert_ne!(old, new);
        assert_eq!(c.typed("chat"), TypedReply::UseButton, "two cards: a typed 1 must not choose");
        assert_eq!(c.take("chat", "42", None), None, "a typed 1 takes neither");
        assert!(c.waiting("chat"));
        assert_eq!(c.take("chat", "42", Some(&old)), Some(job()));
        assert_eq!(c.typed("chat"), TypedReply::One);
        assert_eq!(
            c.take("chat", "42", Some(&new)),
            Some(Job::Decide { task_id: "t2".into(), confirm: false })
        );
        assert!(!c.waiting("chat"));
    }

    #[test]
    fn a_typed_one_settles_only_the_single_owners_card() {
        let c = Confirmations::new(Duration::from_secs(120));
        let code = c.ask("chat", "42", job()).unwrap();
        assert_eq!(c.typed("chat"), TypedReply::One);
        assert_eq!(c.take("chat", "42", None), Some(job()));
        assert_eq!(c.take("chat", "42", Some(&code)), None, "the typed answer used it");
        let mcp = c.ask("chat", ANY_SENDER, Job::Decide { task_id: "mcp".into(), confirm: true }).unwrap();
        assert_eq!(c.typed("chat"), TypedReply::UseButton, "the only card is from MCP");
        assert_eq!(c.take("chat", "42", None), None, "typing does not settle an MCP card");
        assert_eq!(
            c.take("chat", "7", Some(&mcp)),
            Some(Job::Decide { task_id: "mcp".into(), confirm: true }),
            "the button's full code does"
        );
    }

    #[test]
    fn an_mcp_card_stays_when_the_owner_has_one_too_and_a_gapped_reply_skips_it() {
        let c = Confirmations::new(Duration::from_secs(60));
        let owner = c.ask("chat", "42", gapped("訂餐廳")).unwrap();
        let mcp = c.ask("chat", ANY_SENDER, gapped("問營業時間")).unwrap();
        let plain = c.ask("chat", "42", job()).unwrap();
        assert_eq!(c.typed("chat"), TypedReply::UseButton);
        assert_eq!(c.take("chat", "42", None), None);
        assert!(c.waiting("chat"));
        assert_eq!(c.live_gapped("chat", "42"), 1, "the MCP card is not one the reply may answer");
        assert_eq!(c.take_gapped_errand("chat", "42"), Some(gapped("訂餐廳")));
        assert_eq!(c.take("chat", "42", Some(&owner)), None, "the reply used the owner's card");
        assert_eq!(c.take("chat", "1", Some(&mcp)), Some(gapped("問營業時間")), "the MCP card is still there");
        assert_eq!(c.take("chat", "42", Some(&plain)), Some(job()), "the other owner card was not replaced");
    }

    #[test]
    fn an_expired_confirmation_does_nothing() {
        let c = Confirmations::new(Duration::ZERO);
        let code = c.ask("chat", "42", job()).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        assert!(c.waiting("chat"));
        assert_eq!(c.typed("chat"), TypedReply::One, "one expired card: taking it says so");
        assert_eq!(c.take("chat", "42", Some(&code)), None);
        assert!(!c.waiting("chat"), "dropped once seen expired");
    }

    #[test]
    fn an_expired_card_is_dropped_before_the_next_and_twenty_is_the_most() {
        let c = Confirmations::new(Duration::from_millis(40));
        let old = c.ask("chat", "42", job()).unwrap();
        std::thread::sleep(Duration::from_millis(80));
        let fresh = c.ask("chat", "42", Job::Decide { task_id: "fresh".into(), confirm: true }).unwrap();
        assert_eq!(c.take("chat", "42", Some(&old)), None, "expired before the new card");
        assert_eq!(c.take("chat", "42", Some(&fresh)).map(|j| matches!(j, Job::Decide { task_id, .. } if task_id == "fresh")), Some(true));

        let c = Confirmations::new(Duration::from_secs(60));
        let mut codes = Vec::new();
        for i in 0..MAX_PER_CHAT {
            codes.push(c.ask("chat", "42", Job::Decide { task_id: format!("t{i}"), confirm: true }).unwrap());
        }
        let extra = c.ask("chat", "42", Job::Decide { task_id: "new".into(), confirm: false }).unwrap();
        assert_eq!(c.take("chat", "42", Some(&codes[0])), None, "the oldest was dropped");
        assert!(c.take("chat", "42", Some(&codes[1])).is_some());
        assert_eq!(
            c.take("chat", "42", Some(&extra)).map(|j| matches!(j, Job::Decide { task_id, .. } if task_id == "new")),
            Some(true)
        );
    }

    #[test]
    fn a_full_chat_keeps_an_mcp_card_and_refuses_another_mcp_card() {
        let c = Confirmations::new(Duration::from_secs(60));
        let mut owners = Vec::new();
        for i in 0..MAX_PER_CHAT - 1 {
            owners.push(c.ask("chat", "42", Job::Decide { task_id: format!("o{i}"), confirm: true }).unwrap());
        }
        let mcp = c.ask("chat", ANY_SENDER, Job::Decide { task_id: "mcp".into(), confirm: true }).unwrap();
        let extra = c.ask("chat", "42", job()).unwrap();
        assert_eq!(c.take("chat", "42", Some(&owners[0])), None, "the oldest card the chat asked for");
        assert!(c.take("chat", "7", Some(&mcp)).is_some(), "the MCP card stays");
        assert!(c.take("chat", "42", Some(&extra)).is_some());

        let c = Confirmations::new(Duration::from_secs(60));
        let mut mcps = Vec::new();
        for i in 0..MAX_PER_CHAT {
            mcps.push(c.ask("full", ANY_SENDER, Job::Decide { task_id: format!("m{i}"), confirm: true }).unwrap());
        }
        assert!(c.ask("full", ANY_SENDER, job()).is_none(), "a chat of MCP cards takes no more");
        let shared = c.ask_many(&["full".into(), "open".into()], ANY_SENDER, job()).unwrap();
        assert_eq!(c.take("full", "1", Some(&shared)), None, "the full chat was skipped");
        assert!(c.take("open", "1", Some(&shared)).is_some());
        assert!(c.take("full", "1", Some(&mcps[0])).is_some());
        assert!(c.take("full", "1", Some(&mcps[MAX_PER_CHAT - 1])).is_some());

        let mut packed = Vec::new();
        for i in 0..MAX_PER_CHAT {
            packed.push(c.ask("packed", ANY_SENDER, Job::Decide { task_id: format!("p{i}"), confirm: true }).unwrap());
        }
        let owner = c.ask("packed", "42", job()).unwrap();
        assert_eq!(c.take("packed", "1", Some(&packed[0])), None, "the oldest MCP card made room for the chat's own");
        assert_eq!(c.take("packed", "42", Some(&owner)), Some(job()));
        assert!(c.take("packed", "1", Some(&packed[1])).is_some());

        // A full chat with one owner card: a new MCP card is refused and never pushes it out.
        let mut mixed = Vec::new();
        for i in 0..MAX_PER_CHAT - 1 {
            mixed.push(c.ask("mixed", ANY_SENDER, Job::Decide { task_id: format!("x{i}"), confirm: true }).unwrap());
        }
        let waiting = c.ask("mixed", "42", job()).unwrap();
        assert!(c.ask("mixed", ANY_SENDER, Job::Decide { task_id: "late".into(), confirm: true }).is_none());
        assert_eq!(c.take("mixed", "42", Some(&waiting)), Some(job()), "the owner's card is still there");
        assert!(c.take("mixed", "1", Some(&mixed[0])).is_some());
    }
}
