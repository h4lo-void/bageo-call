// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Text messages the SMS forwarder hands to POST /calls/api/notify/send with
//! "kind": "sms", and the ones the owner sends from the console, kept on disk
//! so the console's SMS page can show them as conversations (one per number),
//! search and delete them.
//!
//! One JSON file (sms/messages.json under the calls data folder), written
//! whole on every change: to a temporary file first, then renamed over the old
//! one. Messages older than the retention_days setting are dropped, and at
//! most [MAX_MESSAGES] are kept.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

/// How many messages are kept; the oldest go first.
pub const MAX_MESSAGES: usize = 5000;
/// Longest text, in characters (an ellipsis is added after cutting).
pub const MAX_TEXT_CHARS: usize = 2000;
/// Longest sender, receiving number, phone name and serial, in characters.
pub const MAX_LABEL_CHARS: usize = 80;
/// Messages on one page of the list unless the request asks otherwise.
pub const PAGE_DEFAULT: usize = 50;
/// The most messages one page of the list can hold.
pub const PAGE_MAX: usize = 200;
const MAX_REF_CHARS: usize = 200;
const DAY_MS: u64 = 86_400_000;
const FILE_NAME: &str = "messages.json";
/// The conversation of messages without a sender.
pub const NO_SENDER_KEY: &str = "~";
/// Directions of a message.
pub const DIR_IN: &str = "in";
pub const DIR_OUT: &str = "out";
/// States of a message sent from the console.
pub const STATUS_SENDING: &str = "sending";
pub const STATUS_SENT: &str = "sent";
pub const STATUS_FAILED: &str = "failed";
pub const STATUS_TIMEOUT: &str = "timeout";

fn dir_in() -> String {
    DIR_IN.to_string()
}

/// One stored text message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmsMessage {
    pub id: u64,
    /// When the phone received it (Unix milliseconds).
    pub at_ms: u64,
    #[serde(default)]
    pub from: String,
    /// The number of the SIM that received it.
    #[serde(default)]
    pub to: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub serial: String,
    #[serde(default)]
    pub device: String,
    /// The forwarder's own key for this message: a second request with the
    /// same key is not stored again.
    #[serde(default, rename = "ref")]
    pub reference: String,
    /// "in" (received by a phone) or "out" (sent from the console). A sent
    /// message keeps the owner's line in from and the other party in to.
    #[serde(default = "dir_in")]
    pub dir: String,
    /// The SIM (Android subscription id) that received or sent it, when known.
    #[serde(default)]
    pub sub_id: Option<i64>,
    /// Sent messages: "sending", "sent", "failed" or "timeout"; "" for received ones.
    #[serde(default)]
    pub status: String,
    /// Why a sent message failed (the phone's code), else "".
    #[serde(default)]
    pub error: String,
}

impl SmsMessage {
    pub fn is_out(&self) -> bool {
        self.dir == DIR_OUT
    }

    /// The other party: the sender of a received message, the recipient of a sent one.
    pub fn peer(&self) -> &str {
        if self.is_out() {
            &self.to
        } else {
            &self.from
        }
    }
}

/// A message as a request carries it, before it is cleaned up and stored.
#[derive(Clone, Copy, Debug, Default)]
pub struct NewSms<'a> {
    pub received_ms: u64,
    pub from: &'a str,
    pub to: &'a str,
    pub text: &'a str,
    pub serial: &'a str,
    pub device: &'a str,
    pub reference: &'a str,
    pub sub_id: Option<i64>,
}

/// A message the owner sends from the console, before it is stored.
#[derive(Clone, Copy, Debug, Default)]
pub struct NewOut<'a> {
    /// The other party's number.
    pub to: &'a str,
    pub text: &'a str,
    /// The number of the SIM it goes out from (may be empty).
    pub from: &'a str,
    pub serial: &'a str,
    pub device: &'a str,
    pub sub_id: Option<i64>,
}

/// What became of a pushed message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pushed {
    /// Stored under this id.
    Stored(u64),
    /// A message with the same ref is already there; nothing was stored.
    Duplicate,
}

/// One page of the list, newest first.
#[derive(Clone, Debug)]
pub struct Page {
    /// Every stored message, whatever the search.
    pub total: usize,
    pub messages: Vec<SmsMessage>,
    /// Older messages that match are left after this page.
    pub has_more: bool,
}

/// One conversation: every message with the same other party.
#[derive(Clone, Debug)]
pub struct Thread {
    pub key: String,
    /// The other party as the newest message writes it.
    pub number: String,
    /// The newest message.
    pub last: SmsMessage,
    pub count: usize,
    /// When the newest received message arrived (0: none received).
    pub last_in_at: u64,
}

/// The messages in memory, oldest first. Nothing here reads the clock or the
/// disk: the caller passes the time, so tests can pick it.
#[derive(Debug, Default)]
pub struct SmsBook {
    messages: Vec<SmsMessage>,
    last_id: u64,
}

impl SmsBook {
    pub fn from_messages(mut messages: Vec<SmsMessage>) -> Self {
        messages.sort_by_key(|m| m.id);
        let last_id = messages.last().map(|m| m.id).unwrap_or(0);
        Self { messages, last_id }
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Add messages from elsewhere (a backup): the same time, direction,
    /// numbers and text (or the same ref) count as one already here. An id
    /// taken here gets the next free one; one still "sending" has timed out.
    /// How many were added.
    pub fn merge(&mut self, incoming: Vec<SmsMessage>) -> usize {
        let mut ids: std::collections::HashSet<u64> = self.messages.iter().map(|m| m.id).collect();
        let mut added = 0;
        for mut m in incoming {
            let known = self.messages.iter().any(|x| {
                (x.at_ms == m.at_ms && x.dir == m.dir && x.from == m.from && x.to == m.to && x.text == m.text)
                    || (!m.reference.is_empty() && x.reference == m.reference)
            });
            if known {
                continue;
            }
            if m.id == 0 || ids.contains(&m.id) {
                m.id = self.last_id.saturating_add(1);
            }
            if m.status == STATUS_SENDING {
                m.status = STATUS_TIMEOUT.to_string();
            }
            ids.insert(m.id);
            self.last_id = self.last_id.max(m.id);
            self.messages.push(m);
            added += 1;
        }
        if added > 0 {
            self.messages.sort_by_key(|m| m.id);
            if self.messages.len() > MAX_MESSAGES {
                let extra = self.messages.len() - MAX_MESSAGES;
                self.messages.drain(..extra);
            }
        }
        added
    }

    pub fn messages(&self) -> &[SmsMessage] {
        &self.messages
    }

    pub fn get(&self, id: u64) -> Option<&SmsMessage> {
        self.messages.iter().find(|m| m.id == id)
    }

    fn next_id(&mut self, now_ms: u64) -> u64 {
        let id = self.last_id.saturating_add(1).max(now_ms);
        self.last_id = id;
        id
    }

    /// Drop what is older than keep_days and what is over [MAX_MESSAGES].
    /// True when something was dropped.
    pub fn prune(&mut self, now_ms: u64, keep_days: u32) -> bool {
        let before = self.messages.len();
        let keep_ms = u64::from(keep_days.clamp(1, 365)) * DAY_MS;
        self.messages.retain(|m| m.at_ms.saturating_add(keep_ms) >= now_ms);
        if self.messages.len() > MAX_MESSAGES {
            let extra = self.messages.len() - MAX_MESSAGES;
            self.messages.drain(..extra);
        }
        self.messages.len() != before
    }

    /// Store a message unless its ref is already there. The id is the larger
    /// of "previous id + 1" and the clock, so it never goes backwards.
    pub fn push(&mut self, now_ms: u64, keep_days: u32, new: NewSms<'_>) -> Pushed {
        self.prune(now_ms, keep_days);
        let reference: String = new.reference.trim().chars().take(MAX_REF_CHARS).collect();
        if !reference.is_empty() && self.messages.iter().any(|m| m.reference == reference) {
            return Pushed::Duplicate;
        }
        let id = self.next_id(now_ms);
        let at_ms = if new.received_ms > 0 && new.received_ms <= now_ms.saturating_add(DAY_MS) {
            new.received_ms
        } else {
            now_ms
        };
        self.messages.push(SmsMessage {
            id,
            at_ms,
            from: clean_label(new.from),
            to: clean_label(new.to),
            text: clean_text(new.text),
            serial: clean_label(new.serial),
            device: clean_label(new.device),
            reference,
            dir: dir_in(),
            sub_id: new.sub_id,
            status: String::new(),
            error: String::new(),
        });
        self.prune(now_ms, keep_days);
        Pushed::Stored(id)
    }

    /// Store a message sent from the console, as "sending"; its id.
    pub fn push_out(&mut self, now_ms: u64, keep_days: u32, new: NewOut<'_>) -> u64 {
        self.prune(now_ms, keep_days);
        let id = self.next_id(now_ms);
        self.messages.push(SmsMessage {
            id,
            at_ms: now_ms,
            from: clean_label(new.from),
            to: clean_label(new.to),
            text: clean_text(new.text),
            serial: clean_label(new.serial),
            device: clean_label(new.device),
            reference: String::new(),
            dir: DIR_OUT.to_string(),
            sub_id: new.sub_id,
            status: STATUS_SENDING.to_string(),
            error: String::new(),
        });
        self.prune(now_ms, keep_days);
        id
    }

    /// Record what became of a sent message; the message as it is now.
    pub fn set_status(&mut self, id: u64, status: &str, error: &str) -> Option<SmsMessage> {
        let m = self.messages.iter_mut().find(|m| m.id == id && m.is_out())?;
        m.status = status.to_string();
        m.error = error.chars().take(MAX_LABEL_CHARS).collect();
        Some(m.clone())
    }

    /// Sent messages still marked "sending" become "timeout": after a restart
    /// nobody waits for their result any more. True when one changed.
    fn settle_sending(&mut self) -> bool {
        let mut changed = false;
        for m in self.messages.iter_mut().filter(|m| m.is_out() && m.status == STATUS_SENDING) {
            m.status = STATUS_TIMEOUT.to_string();
            changed = true;
        }
        changed
    }

    /// The conversations, newest first. key_of gives a message's conversation
    /// ([thread_key] with the country of its SIM); name_of the contact name of
    /// a conversation's newest message. q (case is ignored) matches the name,
    /// the number or any message's text; an empty q matches everything.
    pub fn threads(&self, q: &str, key_of: &dyn Fn(&SmsMessage) -> String, name_of: &dyn Fn(&SmsMessage) -> String) -> Vec<Thread> {
        let needle = fold(q);
        let mut by_key: HashMap<String, (Thread, bool)> = HashMap::new();
        for m in &self.messages {
            let key = key_of(m);
            let hit = !needle.is_empty() && (fold(&m.text).contains(&needle) || fold(m.peer()).contains(&needle));
            let entry = by_key.entry(key.clone()).or_insert_with(|| {
                (Thread { key, number: String::new(), last: m.clone(), count: 0, last_in_at: 0 }, false)
            });
            let (t, found) = entry;
            t.count += 1;
            if m.id >= t.last.id {
                t.last = m.clone();
            }
            if !m.is_out() {
                t.last_in_at = t.last_in_at.max(m.at_ms);
            }
            *found |= hit;
        }
        let mut out: Vec<Thread> = by_key
            .into_values()
            .filter_map(|(mut t, found)| {
                t.number = t.last.peer().to_string();
                let keep = needle.is_empty() || found || fold(&t.key).contains(&needle) || fold(&name_of(&t.last)).contains(&needle);
                keep.then_some(t)
            })
            .collect();
        out.sort_by(|a, b| (b.last.at_ms, b.last.id).cmp(&(a.last.at_ms, a.last.id)));
        out
    }

    /// Every message of the conversation key, oldest first.
    pub fn thread(&self, key: &str, key_of: &dyn Fn(&SmsMessage) -> String) -> Vec<SmsMessage> {
        self.messages.iter().filter(|m| key_of(m) == key).cloned().collect()
    }

    /// Remove the conversation key; how many messages that was.
    pub fn delete_thread(&mut self, key: &str, key_of: &dyn Fn(&SmsMessage) -> String) -> usize {
        let before = self.messages.len();
        self.messages.retain(|m| key_of(m) != key);
        before - self.messages.len()
    }

    /// A page of messages, newest first: those with an id below before_id (all
    /// when None) whose sender, text or contact name holds q (case is ignored;
    /// an empty q matches everything). name_of gives the contact name of a
    /// sender, or an empty string.
    pub fn page(&self, q: &str, before_id: Option<u64>, limit: usize, name_of: &dyn Fn(&str) -> String) -> Page {
        let limit = limit.clamp(1, PAGE_MAX);
        let needle = fold(q);
        let mut names: HashMap<&str, String> = HashMap::new();
        let mut hits = self
            .messages
            .iter()
            .rev()
            .filter(|m| before_id.map_or(true, |b| m.id < b))
            .filter(|m| {
                needle.is_empty()
                    || fold(&m.from).contains(&needle)
                    || fold(&m.text).contains(&needle)
                    || names
                        .entry(m.from.as_str())
                        .or_insert_with(|| fold(&name_of(&m.from)))
                        .contains(&needle)
            });
        let messages: Vec<SmsMessage> = hits.by_ref().take(limit).cloned().collect();
        let has_more = hits.next().is_some();
        Page { total: self.messages.len(), messages, has_more }
    }

    /// True when a message with this id was there.
    pub fn delete(&mut self, id: u64) -> bool {
        let before = self.messages.len();
        self.messages.retain(|m| m.id != id);
        self.messages.len() != before
    }

    /// Remove everything; how many messages that was.
    pub fn clear(&mut self) -> usize {
        let n = self.messages.len();
        self.messages.clear();
        n
    }
}

/// Lower case with runs of whitespace as one space, for comparing.
fn fold(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// One line, at most [MAX_LABEL_CHARS] characters.
fn clean_label(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_LABEL_CHARS).collect()
}

/// The text with its line breaks (as \n), trimmed, cut to [MAX_TEXT_CHARS]
/// characters (not bytes) with a trailing ellipsis.
fn clean_text(s: &str) -> String {
    let unified = s.replace("\r\n", "\n").replace('\r', "\n");
    let text = unified.trim();
    if text.chars().count() > MAX_TEXT_CHARS {
        let mut cut: String = text.chars().take(MAX_TEXT_CHARS).collect();
        cut.push('…');
        cut
    } else {
        text.to_string()
    }
}

/// The clock in Unix milliseconds.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Full-width digits and signs as plain ones ("０９１２", "＋886").
fn ascii_form(c: char) -> char {
    match c {
        '０'..='９' => char::from_u32(c as u32 - '０' as u32 + '0' as u32).unwrap_or(c),
        '＋' => '+',
        '－' | '‐' | '‑' | '–' | '—' => '-',
        '（' => '(',
        '）' => ')',
        '．' => '.',
        '\u{3000}' => ' ',
        _ => c,
    }
}

/// A number a text can be sent to: the digits (3 to 20), with a leading + when
/// written so, once spaces, -, (, ) and . are taken out. None for anything else
/// (a sender name like "BankInfo", a short code with a #).
pub fn sendable_number(raw: &str) -> Option<String> {
    let plain: String = raw
        .trim()
        .chars()
        .map(ascii_form)
        .filter(|c| !c.is_whitespace() && !matches!(c, '-' | '(' | ')' | '.'))
        .collect();
    let digits = plain.strip_prefix('+').unwrap_or(&plain);
    ((3..=20).contains(&digits.len()) && digits.chars().all(|c| c.is_ascii_digit())).then_some(plain)
}

/// Validate a recipient number before sending a text or offering a reply:
/// rejects short codes (`<= 7` digits such as emergency `110`/`119`/`911`/`112`)
/// and, via [`crate::errands::check_number`], premium-rate or malformed numbers.
///
/// `+...` numbers are checked against their destination country (`phone_region::normalize`)
/// so international texts remain allowed; local numbers are checked against `region`.
pub fn check_send_number(raw: &str, region: Option<&str>) -> Result<String, &'static str> {
    let to = sendable_number(raw).ok_or("bad_number")?;
    if to.trim_start_matches('+').len() <= 7 {
        return Err("not_allowed");
    }
    let dest_region = if to.starts_with('+') {
        crate::phone_region::normalize(&to, None)
            .map(|p| p.country.to_string())
            .ok_or("bad_number")?
    } else {
        match region.map(str::trim).filter(|r| !r.is_empty()) {
            Some(r) => r.to_ascii_lowercase(),
            None => return Ok(to),
        }
    };
    crate::errands::check_number(&to, &dest_region).map(|_| to)
}

/// The conversation a message with the other party peer belongs to. A phone
/// number in international form when it reads as one (its digits taken in
/// region, the country of the SIM, when it has no country code), so 0912…
/// and +886912… meet; else its digits. A sender name (letters) in lower case;
/// no sender at all is [NO_SENDER_KEY].
pub fn thread_key(peer: &str, region: Option<&str>) -> String {
    let plain: String = peer.trim().chars().map(ascii_form).collect();
    if plain.trim().is_empty() {
        return NO_SENDER_KEY.to_string();
    }
    let digits: String = plain.chars().filter(char::is_ascii_digit).collect();
    if plain.chars().any(char::is_alphabetic) || !(3..=20).contains(&digits.len()) {
        return fold(&plain);
    }
    if let Some(n) = crate::phone_region::normalize(&plain, region) {
        // "+886 0912…": the trunk 0 written after the country code (Italy keeps its 0).
        let split = crate::phone_region::calling_code(n.country)
            .and_then(|code| n.e164.strip_prefix('+')?.strip_prefix(code).map(|national| (code, national)));
        return match split {
            Some((code, national)) if code != "39" && national.starts_with('0') => {
                format!("+{code}{}", national.trim_start_matches('0'))
            }
            _ => n.e164,
        };
    }
    let intl = plain.chars().find(|c| c.is_ascii_digit() || *c == '+') == Some('+');
    match (intl, digits.strip_prefix("00")) {
        (true, _) => format!("+{digits}"),
        (false, Some(rest)) if rest.len() >= 3 => format!("+{rest}"),
        _ => digits,
    }
}

#[derive(Deserialize)]
struct SmsFile {
    #[serde(default)]
    messages: Vec<SmsMessage>,
}

#[derive(Serialize)]
struct SmsFileRef<'a> {
    messages: &'a [SmsMessage],
}

/// The book and its file. Every call that changes the book writes the file
/// before it returns; the time and the retention come from the caller.
pub struct SmsStore {
    path: PathBuf,
    book: Mutex<SmsBook>,
}

impl SmsStore {
    /// The messages kept in dir (none while the file does not exist), without
    /// those that are too old by now.
    pub fn open(dir: PathBuf, now_ms: u64, keep_days: u32) -> Arc<Self> {
        let path = dir.join(FILE_NAME);
        drop_old_unreadable(&dir, now_ms, keep_days);
        let mut book = SmsBook::from_messages(load_file(&path));
        let settled = book.settle_sending();
        let pruned = book.prune(now_ms, keep_days);
        let store = Self { path, book: Mutex::new(book) };
        if pruned || settled {
            store.save(&store.lock());
        }
        Arc::new(store)
    }

    fn lock(&self) -> MutexGuard<'_, SmsBook> {
        self.book.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The folder the messages are kept in.
    pub fn dir(&self) -> PathBuf {
        self.path.parent().map(Path::to_path_buf).unwrap_or_default()
    }

    /// Add the messages of a backup that are not here yet; how many.
    pub fn merge(&self, messages: Vec<SmsMessage>) -> usize {
        let mut book = self.lock();
        let added = book.merge(messages);
        if added > 0 {
            self.save(&book);
        }
        added
    }

    /// Write the file. A failure is logged and the messages stay in memory.
    fn save(&self, book: &SmsBook) {
        let body = match serde_json::to_vec(&SmsFileRef { messages: book.messages() }) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("sms store: encode failed: {e}");
                return;
            }
        };
        if let Err(e) = write_atomic(&self.path, &body) {
            tracing::warn!("sms store: write {} failed: {e}", self.path.display());
        }
    }

    pub fn push(&self, now_ms: u64, keep_days: u32, new: NewSms<'_>) -> Pushed {
        let mut book = self.lock();
        let before = book.len();
        let pushed = book.push(now_ms, keep_days, new);
        if pushed != Pushed::Duplicate || book.len() != before {
            self.save(&book);
        }
        pushed
    }

    pub fn list(&self, now_ms: u64, keep_days: u32, q: &str, before_id: Option<u64>, limit: usize, name_of: &dyn Fn(&str) -> String) -> Page {
        let mut book = self.lock();
        if book.prune(now_ms, keep_days) {
            self.save(&book);
        }
        book.page(q, before_id, limit, name_of)
    }

    /// Store a message sent from the console ("sending"); the message.
    pub fn push_out(&self, now_ms: u64, keep_days: u32, new: NewOut<'_>) -> SmsMessage {
        let mut book = self.lock();
        let id = book.push_out(now_ms, keep_days, new);
        self.save(&book);
        book.get(id).cloned().expect("just stored")
    }

    pub fn get(&self, id: u64) -> Option<SmsMessage> {
        self.lock().get(id).cloned()
    }

    /// Drop the messages older than keep_days (the setting as it is now), and
    /// the old copies of a file that could not be read; how many messages went.
    pub fn purge(&self, now_ms: u64, keep_days: u32) -> usize {
        let mut book = self.lock();
        let before = book.len();
        if book.prune(now_ms, keep_days) {
            self.save(&book);
        }
        if let Some(dir) = self.path.parent() {
            drop_old_unreadable(dir, now_ms, keep_days);
        }
        before - book.len()
    }

    /// Record what became of a sent message; None when it is gone.
    pub fn set_status(&self, id: u64, status: &str, error: &str) -> Option<SmsMessage> {
        let mut book = self.lock();
        let m = book.set_status(id, status, error)?;
        self.save(&book);
        Some(m)
    }

    /// The conversations, newest first, and how many messages there are in all
    /// ([SmsBook::threads]).
    pub fn threads(
        &self,
        now_ms: u64,
        keep_days: u32,
        q: &str,
        key_of: &dyn Fn(&SmsMessage) -> String,
        name_of: &dyn Fn(&SmsMessage) -> String,
    ) -> (usize, Vec<Thread>) {
        let mut book = self.lock();
        if book.prune(now_ms, keep_days) {
            self.save(&book);
        }
        (book.len(), book.threads(q, key_of, name_of))
    }

    /// Every message of the conversation key, oldest first.
    pub fn thread(&self, now_ms: u64, keep_days: u32, key: &str, key_of: &dyn Fn(&SmsMessage) -> String) -> Vec<SmsMessage> {
        let mut book = self.lock();
        if book.prune(now_ms, keep_days) {
            self.save(&book);
        }
        book.thread(key, key_of)
    }

    /// Remove a conversation; how many messages that was.
    pub fn delete_thread(&self, key: &str, key_of: &dyn Fn(&SmsMessage) -> String) -> usize {
        let mut book = self.lock();
        let removed = book.delete_thread(key, key_of);
        if removed > 0 {
            self.save(&book);
        }
        removed
    }

    pub fn delete(&self, id: u64) -> bool {
        let mut book = self.lock();
        let found = book.delete(id);
        if found {
            self.save(&book);
        }
        found
    }

    pub fn clear(&self) -> usize {
        let mut book = self.lock();
        let removed = book.clear();
        if removed > 0 {
            self.save(&book);
        }
        // "Delete all" means all: also the copies of a file that could not be read.
        if let Some(dir) = self.path.parent() {
            drop_unreadable(dir, None);
        }
        removed
    }
}

/// The messages of a `messages.json` (a backup's).
pub fn messages_from_json(bytes: &[u8]) -> serde_json::Result<Vec<SmsMessage>> {
    serde_json::from_slice::<SmsFile>(bytes).map(|f| f.messages)
}

/// The messages of the file. A file that cannot be read is moved aside (so the
/// next save does not overwrite it) and the list starts empty.
/// Files moved aside hold text messages too: they go once they are older than
/// the days messages are kept ([drop_old_unreadable]).
fn load_file(path: &Path) -> Vec<SmsMessage> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!("sms store: read {} failed: {e}", path.display());
            return Vec::new();
        }
    };
    match serde_json::from_slice::<SmsFile>(&raw) {
        Ok(file) => file.messages,
        Err(e) => {
            let aside = path.with_file_name(format!("{FILE_NAME}.unreadable-{}", now_ms()));
            tracing::warn!("sms store: {} is not readable ({e}); moved to {}", path.display(), aside.display());
            let _ = std::fs::rename(path, &aside);
            Vec::new()
        }
    }
}

/// Remove the files [load_file] moved aside more than keep_days ago (their
/// name ends with the time they were moved, in Unix milliseconds).
fn drop_old_unreadable(dir: &Path, now_ms: u64, keep_days: u32) {
    let keep_ms = u64::from(keep_days.clamp(1, 365)) * DAY_MS;
    drop_unreadable(dir, Some((now_ms, keep_ms)));
}

/// Remove the files [load_file] moved aside: those older than `older_than`
/// (now, age in milliseconds), or all of them when it is None.
fn drop_unreadable(dir: &Path, older_than: Option<(u64, u64)>) {
    let prefix = format!("{FILE_NAME}.unreadable-");
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(moved_ms) = name.strip_prefix(&prefix).and_then(|t| t.parse::<u64>().ok()) else { continue };
        let old_enough = match older_than {
            Some((now_ms, keep_ms)) => now_ms.saturating_sub(moved_ms) > keep_ms,
            None => true,
        };
        if old_enough {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Write body to a new file next to path, then rename it over path: a crash
/// leaves the old file or the new one, never half of one.
fn write_atomic(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_file_name(format!(".{FILE_NAME}.tmp"));
    let written = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(body)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEEP: u32 = 15;
    const NOW: u64 = 1_800_000_000_000;

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("bageo-sms-{tag}-{}", uuid::Uuid::new_v4().simple()))
    }

    fn sms<'a>(from: &'a str, text: &'a str) -> NewSms<'a> {
        NewSms { from, text, ..Default::default() }
    }

    fn no_name(_: &str) -> String {
        String::new()
    }

    fn all(book: &SmsBook) -> Vec<SmsMessage> {
        book.page("", None, PAGE_MAX, &no_name).messages
    }

    fn key_tw(m: &SmsMessage) -> String {
        thread_key(m.peer(), Some("tw"))
    }

    fn out_to<'a>(to: &'a str, text: &'a str) -> NewOut<'a> {
        NewOut { to, text, from: "0912000111", serial: "TESTSERIAL01", device: "Pixel Test", sub_id: Some(1) }
    }

    #[test]
    fn sms_retention_purge_follows_the_setting() {
        let dir = temp_dir("retention");
        let store = SmsStore::open(dir.clone(), NOW, KEEP);
        store.push_out(NOW - 10 * DAY_MS, KEEP, out_to("+15555550123", "sent ten days ago"));
        for (received_ms, text) in [(NOW - 10 * DAY_MS, "ten days old"), (NOW - DAY_MS / 2, "today")] {
            store.push(NOW, KEEP, NewSms { received_ms, from: "+15555550123", text, ..Default::default() });
        }
        assert_eq!(store.purge(NOW, 15), 0);
        assert_eq!(store.purge(NOW, 5), 2, "received and sent ones alike");
        let left = |s: &SmsStore| s.list(NOW, 365, "", None, PAGE_MAX, &no_name).messages.into_iter().map(|m| m.text).collect::<Vec<_>>();
        assert_eq!(left(&store), vec!["today".to_string()]);
        assert_eq!(left(&SmsStore::open(dir.clone(), NOW, 365)), vec!["today".to_string()], "saved");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sms_thread_key_puts_local_and_international_numbers_together() {
        let same = |region: &str, a: &str, b: &str| {
            assert_eq!(thread_key(a, Some(region)), thread_key(b, Some(region)), "{a} / {b} in {region}");
        };
        // Taiwan
        same("tw", "0912345678", "+886912345678");
        same("tw", "0912-345-678", "+886 0912 345 678");
        same("tw", "０９１２３４５６７８", "00886912345678");
        assert_eq!(thread_key("0912345678", Some("tw")), "+886912345678");
        // United States
        same("us", "(650) 555-0142", "+1 650 555 0142");
        assert_eq!(thread_key("650.555.0142", Some("us")), "+16505550142");
        // Japan
        same("jp", "090-1234-5678", "+81 90 1234 5678");
        // A number with its own country code reads the same on any SIM.
        assert_eq!(thread_key("+16505550142", Some("tw")), thread_key("+16505550142", Some("jp")));
        // The same local digits are another number in another country.
        assert_ne!(thread_key("0912345678", Some("tw")), thread_key("0912345678", Some("jp")));
        // Without a country: the digits.
        assert_eq!(thread_key("0912345678", None), "0912345678");
        assert_eq!(thread_key("+886 912 345 678", None), "+886912345678");
        // Sender names, short codes, nobody.
        assert_eq!(thread_key(" BankInfo ", Some("tw")), "bankinfo");
        assert_eq!(thread_key("bankinfo", None), "bankinfo");
        assert_eq!(thread_key("中華電信", Some("tw")), "中華電信");
        assert_eq!(thread_key("", Some("tw")), NO_SENDER_KEY);
        assert_eq!(thread_key("   ", None), NO_SENDER_KEY);
        assert_eq!(thread_key("55", Some("tw")), "55");
    }

    #[test]
    fn sms_sendable_number_takes_digits_only() {
        assert_eq!(sendable_number(" +886 912-345-678 ").as_deref(), Some("+886912345678"));
        assert_eq!(sendable_number("(650) 555.0142").as_deref(), Some("6505550142"));
        assert_eq!(sendable_number("１２３").as_deref(), Some("123"));
        for bad in ["", "BankInfo", "12", "#123", "++886912", "0912 345 678 ext 5", "1".repeat(21).as_str()] {
            assert_eq!(sendable_number(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn sms_check_send_number_blocks_emergency_short_and_premium_numbers() {
        for bad in ["110", "119", "911", "112", "113", "165", "1999", "+110", "１１９", "1234567"] {
            assert_eq!(check_send_number(bad, Some("tw")), Err("not_allowed"), "{bad}");
            assert_eq!(check_send_number(bad, None), Err("not_allowed"), "{bad}");
        }
        assert_eq!(check_send_number("0204-123-456", Some("tw")), Err("not_allowed"));
        assert_eq!(check_send_number("+886204123456", Some("tw")), Err("not_allowed"));
        assert_eq!(check_send_number("0196834567", Some("tw")), Err("not_allowed"));
        assert_eq!(check_send_number("+1 900-555-0100", Some("tw")), Err("not_allowed"));
        assert_eq!(check_send_number("900-555-0100", Some("us")), Err("not_allowed"));
        assert_eq!(check_send_number("+81-990-123-456", Some("tw")), Err("not_allowed"));
        assert_eq!(check_send_number("00886912345678", Some("tw")), Err("not_allowed"));
        assert_eq!(check_send_number("BankInfo", Some("tw")), Err("bad_number"));
        assert_eq!(check_send_number("912345678", Some("tw")), Err("bad_number"));
        // Normal local, toll-free, and international numbers remain allowed.
        assert_eq!(check_send_number("0912-345-678", Some("tw")).as_deref(), Ok("0912345678"));
        assert_eq!(check_send_number("0800-123-456", Some("tw")).as_deref(), Ok("0800123456"));
        assert_eq!(check_send_number("+1 (650) 555-0142", Some("tw")).as_deref(), Ok("+16505550142"));
        assert_eq!(check_send_number("+81 90-1234-5678", Some("tw")).as_deref(), Ok("+819012345678"));
    }

    #[test]
    fn sms_threads_group_sort_and_search_in_many_languages() {
        let mut book = SmsBook::default();
        let rows: [(u64, &str, &str); 8] = [
            (1, "0912345678", "您的包裹已送達"),
            (2, "+81312345678", "明日の予定を確認してください"),
            (3, "+821012345678", "내일 회의는 오후 3시입니다"),
            (4, "+966500000000", "موعدك غدا الساعة العاشرة"),
            (5, "+66812345678", "พรุ่งนี้เจอกันที่ร้านกาแฟ"),
            (6, "+34600000000", "¿Nos vemos mañana en la estación?"),
            (7, "+886912345678", "第二則：明天見"),
            (8, "BankInfo", "Your statement is ready"),
        ];
        for (t, from, text) in rows {
            book.push(NOW + t, KEEP, NewSms { from, text, sub_id: Some(1), ..Default::default() });
        }
        book.push_out(NOW + 9, KEEP, out_to("+81 3-1234-5678", "了解です"));
        let names = |m: &SmsMessage| if key_tw(m) == "+886912345678" { "王小明".to_string() } else { String::new() };
        let all = book.threads("", &key_tw, &names);
        assert_eq!(all.len(), 7, "0912… and +886912… are one conversation");
        assert_eq!(all[0].key, "+81312345678", "newest first");
        assert_eq!((all[0].count, all[0].last.dir.as_str(), all[0].last.status.as_str()), (2, "out", "sending"));
        assert_eq!(all[0].last_in_at, NOW + 2);
        assert_eq!(all[0].number, "+81 3-1234-5678", "the newest message's number as written");
        let tw = all.iter().find(|t| t.key == "+886912345678").unwrap();
        assert_eq!((tw.count, tw.number.as_str(), tw.last.text.as_str(), tw.last_in_at), (2, "+886912345678", "第二則：明天見", NOW + 7));
        let bank = all.iter().find(|t| t.key == "bankinfo").unwrap();
        assert_eq!(bank.number, "BankInfo");
        let found = |q: &str| book.threads(q, &key_tw, &names).into_iter().map(|t| t.key).collect::<Vec<_>>();
        for (q, key) in [
            ("包裹", "+886912345678"),
            ("王小明", "+886912345678"),
            ("予定", "+81312345678"),
            ("了解", "+81312345678"),
            ("회의는", "+821012345678"),
            ("الساعة", "+966500000000"),
            ("ร้านกาแฟ", "+66812345678"),
            ("MAÑANA", "+34600000000"),
            ("bankinfo", "bankinfo"),
            ("statement", "bankinfo"),
            ("0912345678", "+886912345678"),
            ("886912345678", "+886912345678"),
        ] {
            assert_eq!(found(q), vec![key.to_string()], "q {q:?}");
        }
        assert!(found("nothing like this").is_empty());
        // One conversation, oldest first.
        let jp = book.thread("+81312345678", &key_tw);
        assert_eq!(jp.iter().map(|m| m.dir.as_str()).collect::<Vec<_>>(), vec!["in", "out"]);
        // Deleting it leaves the others.
        assert_eq!(book.delete_thread("+886912345678", &key_tw), 2);
        assert_eq!(book.threads("", &key_tw, &names).len(), 6);
        assert_eq!(book.delete_thread("+886912345678", &key_tw), 0);
    }

    #[test]
    fn sms_sent_messages_keep_their_status() {
        let dir = temp_dir("out");
        let store = SmsStore::open(dir.clone(), NOW, KEEP);
        let m = store.push_out(NOW, KEEP, out_to("+886912345678", "  回覆內容 \n第二行 "));
        assert_eq!((m.dir.as_str(), m.status.as_str(), m.error.as_str()), ("out", "sending", ""));
        assert_eq!((m.from.as_str(), m.to.as_str(), m.peer()), ("0912000111", "+886912345678", "+886912345678"));
        assert_eq!((m.text.as_str(), m.sub_id, m.serial.as_str()), ("回覆內容 \n第二行", Some(1), "TESTSERIAL01"));
        let failed = store.set_status(m.id, STATUS_FAILED, "no_service").unwrap();
        assert_eq!((failed.status.as_str(), failed.error.as_str()), ("failed", "no_service"));
        // A received message has no send status.
        let Pushed::Stored(in_id) = store.push(NOW + 1, KEEP, sms("+886912345678", "收到")) else { panic!() };
        assert_eq!(store.set_status(in_id, STATUS_SENT, ""), None);
        assert_eq!(store.get(in_id).unwrap().status, "");
        assert_eq!(store.set_status(42, STATUS_SENT, ""), None);
        // Still "sending" when Bageo stops: after a restart nobody waits for it.
        let pending = store.push_out(NOW + 2, KEEP, out_to("+886912345678", "pending"));
        drop(store);
        let again = SmsStore::open(dir.clone(), NOW + 3, KEEP);
        assert_eq!(again.get(pending.id).unwrap().status, STATUS_TIMEOUT);
        assert_eq!(again.get(m.id).unwrap().error, "no_service");
        // Sent messages go after the retention like the others.
        assert_eq!(again.threads(NOW + 20 * DAY_MS, KEEP, "", &key_tw, &|_: &SmsMessage| String::new()).0, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sms_old_file_without_direction_reads_as_received() {
        let dir = temp_dir("oldfile");
        std::fs::create_dir_all(&dir).unwrap();
        let old = format!(
            r#"{{"messages":[{{"id":{id},"at_ms":{id},"from":"0912345678","to":"+886912000111","text":"舊格式","serial":"TESTSERIAL01","device":"Pixel Test","ref":"k-1"}}]}}"#,
            id = NOW
        );
        std::fs::write(dir.join("messages.json"), old).unwrap();
        let store = SmsStore::open(dir.clone(), NOW, KEEP);
        let m = store.get(NOW).expect("read");
        assert_eq!((m.dir.as_str(), m.status.as_str(), m.error.as_str(), m.sub_id), ("in", "", "", None));
        assert_eq!((m.peer(), m.text.as_str(), m.reference.as_str()), ("0912345678", "舊格式", "k-1"));
        assert_eq!(store.push(NOW + 1, KEEP, NewSms { from: "x", text: "y", reference: "k-1", ..Default::default() }), Pushed::Duplicate);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sms_store_saves_and_reads_back_after_a_restart() {
        let dir = temp_dir("restart");
        let store = SmsStore::open(dir.clone(), NOW, KEEP);
        let first = store.push(NOW, KEEP, sms("+15555550123", "Your code is 123456"));
        let second = store.push(
            NOW + 5,
            KEEP,
            NewSms {
                received_ms: NOW - 2_000,
                from: "0912345678",
                to: "+15555550100",
                text: "第一行\n第二行",
                serial: "TESTSERIAL01",
                device: "Pixel Test",
                reference: "key-1",
                sub_id: Some(3),
            },
        );
        let (Pushed::Stored(a), Pushed::Stored(b)) = (first, second) else { panic!("{first:?} {second:?}") };
        assert!(a < b);
        let before = store.list(NOW + 10, KEEP, "", None, PAGE_DEFAULT, &no_name);
        assert_eq!((before.total, before.has_more), (2, false));
        assert_eq!(before.messages[0].id, b, "newest first");
        assert_eq!(before.messages[0].at_ms, NOW - 2_000);
        assert_eq!(before.messages[0].text, "第一行\n第二行");
        assert_eq!((before.messages[0].serial.as_str(), before.messages[0].device.as_str()), ("TESTSERIAL01", "Pixel Test"));
        assert_eq!((before.messages[0].to.as_str(), before.messages[0].reference.as_str()), ("+15555550100", "key-1"));
        drop(store);

        assert!(dir.join("messages.json").is_file());
        let left: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(left, vec!["messages.json".to_string()], "no temporary file stays");

        let again = SmsStore::open(dir.clone(), NOW + 20, KEEP);
        let after = again.list(NOW + 20, KEEP, "", None, PAGE_DEFAULT, &no_name);
        assert_eq!(after.messages, before.messages);
        // Ids keep climbing after a restart, even when the clock stepped back.
        let Pushed::Stored(c) = again.push(NOW - 50_000, KEEP, sms("+15555550123", "later")) else { panic!() };
        assert!(c > b);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sms_store_starts_empty_when_the_file_is_not_readable() {
        let dir = temp_dir("broken");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("messages.json"), b"{ not json").unwrap();
        let store = SmsStore::open(dir.clone(), NOW, KEEP);
        assert_eq!(store.list(NOW, KEEP, "", None, PAGE_DEFAULT, &no_name).total, 0);
        // The broken file is kept under another name.
        let kept = std::fs::read_dir(&dir).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().contains("unreadable")).count();
        assert_eq!(kept, 1);
        assert_eq!(store.push(NOW, KEEP, sms("+15555550123", "works again")), Pushed::Stored(NOW));
        assert_eq!(SmsStore::open(dir.clone(), NOW, KEEP).list(NOW, KEEP, "", None, PAGE_DEFAULT, &no_name).total, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sms_unreadable_copies_go_after_the_retention() {
        let dir = temp_dir("aside");
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join(format!("messages.json.unreadable-{}", NOW - 16 * DAY_MS));
        let recent = dir.join(format!("messages.json.unreadable-{}", NOW - 14 * DAY_MS));
        let other = dir.join("messages.json.unreadable-notatime");
        for f in [&old, &recent, &other] {
            std::fs::write(f, b"{ not json").unwrap();
        }
        let _ = SmsStore::open(dir.clone(), NOW, KEEP);
        assert!(!old.exists());
        assert!(recent.exists() && other.exists());
        // "Delete all" also removes the recent copy; a file that is not one of ours stays.
        let store = SmsStore::open(dir.clone(), NOW, KEEP);
        store.clear();
        assert!(!recent.exists() && other.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sms_ref_is_stored_only_once() {
        let dir = temp_dir("ref");
        let store = SmsStore::open(dir.clone(), NOW, KEEP);
        fn with_ref(r: &str) -> NewSms<'_> {
            NewSms { from: "+15555550123", text: "same text", reference: r, ..Default::default() }
        }
        assert!(matches!(store.push(NOW, KEEP, with_ref("abc")), Pushed::Stored(_)));
        assert_eq!(store.push(NOW + 1, KEEP, with_ref("abc")), Pushed::Duplicate);
        assert_eq!(store.push(NOW + 2, KEEP, with_ref("  abc ")), Pushed::Duplicate);
        assert!(matches!(store.push(NOW + 3, KEEP, with_ref("abd")), Pushed::Stored(_)));
        // Without a ref every request is its own message.
        assert!(matches!(store.push(NOW + 4, KEEP, with_ref("")), Pushed::Stored(_)));
        assert!(matches!(store.push(NOW + 5, KEEP, with_ref(" ")), Pushed::Stored(_)));
        assert_eq!(store.list(NOW + 6, KEEP, "", None, PAGE_DEFAULT, &no_name).total, 4);
        // Still known after a restart.
        let again = SmsStore::open(dir.clone(), NOW + 7, KEEP);
        assert_eq!(again.push(NOW + 8, KEEP, with_ref("abc")), Pushed::Duplicate);
        assert_eq!(again.list(NOW + 9, KEEP, "", None, PAGE_DEFAULT, &no_name).total, 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sms_older_than_the_retention_are_removed() {
        let mut book = SmsBook::default();
        book.push(NOW - 20 * DAY_MS, KEEP, sms("+15555550123", "twenty days old"));
        book.push(NOW - 10 * DAY_MS, KEEP, sms("+15555550123", "ten days old"));
        book.push(NOW - 6 * DAY_MS, KEEP, sms("+15555550123", "six days old"));
        assert_eq!(book.len(), 3, "each was new enough when it came");
        assert!(book.prune(NOW, KEEP));
        assert_eq!(all(&book).iter().map(|m| m.text.as_str()).collect::<Vec<_>>(), vec!["six days old", "ten days old"]);
        assert!(!book.prune(NOW, KEEP), "nothing more to drop");
        // A shorter retention drops more; a message exactly at the limit stays.
        assert!(!book.prune(NOW, 10));
        assert!(book.prune(NOW + 1, 10));
        assert_eq!(book.len(), 1);
        // A new message cleans up as well.
        book.push(NOW + 30 * DAY_MS, KEEP, sms("+15555550123", "much later"));
        assert_eq!(all(&book).iter().map(|m| m.text.as_str()).collect::<Vec<_>>(), vec!["much later"]);
        // The received time counts, not the time of storing.
        let mut late = SmsBook::default();
        late.push(NOW, KEEP, NewSms { received_ms: NOW - 16 * DAY_MS, from: "+15555550123", text: "forwarded late", ..Default::default() });
        assert_eq!(late.len(), 0);

        // The store cleans up when it opens and when it lists.
        let dir = temp_dir("keep");
        let store = SmsStore::open(dir.clone(), NOW, KEEP);
        store.push(NOW, KEEP, sms("+15555550123", "old"));
        store.push(NOW + 10 * DAY_MS, KEEP, sms("+15555550123", "newer"));
        assert_eq!(store.list(NOW + 14 * DAY_MS, KEEP, "", None, PAGE_DEFAULT, &no_name).total, 2);
        assert_eq!(store.list(NOW + 16 * DAY_MS, KEEP, "", None, PAGE_DEFAULT, &no_name).total, 1);
        assert_eq!(SmsStore::open(dir.clone(), NOW + 16 * DAY_MS, 365).list(NOW + 16 * DAY_MS, 365, "", None, PAGE_DEFAULT, &no_name).total, 1, "the file lost it too");
        drop(SmsStore::open(dir.clone(), NOW + 40 * DAY_MS, KEEP));
        assert_eq!(SmsStore::open(dir.clone(), NOW, 365).list(NOW, 365, "", None, PAGE_DEFAULT, &no_name).total, 0, "opening cleaned the file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sms_store_keeps_at_most_five_thousand() {
        let mut book = SmsBook::default();
        for i in 0..(MAX_MESSAGES as u64 + 5) {
            book.push(NOW + i, KEEP, sms("+15555550123", &format!("m{i}")));
        }
        assert_eq!(book.len(), MAX_MESSAGES);
        assert_eq!(book.messages().first().unwrap().text, "m5");
        assert_eq!(book.messages().last().unwrap().text, format!("m{}", MAX_MESSAGES + 4));
        assert_eq!(MAX_MESSAGES, 5000);
    }

    #[test]
    fn sms_text_is_cut_by_characters_and_keeps_line_breaks() {
        let samples = [
            "這是一則很長的簡訊內容，",
            "これは長いメッセージです。",
            "이것은 긴 문자 메시지입니다. ",
            "هذه رسالة نصية طويلة ",
            "นี่คือข้อความที่ยาวมาก ",
            "😀👍",
            "Это длинное сообщение. ",
        ];
        for sample in samples {
            let long = sample.repeat(MAX_TEXT_CHARS / sample.chars().count() + 2);
            let mut book = SmsBook::default();
            book.push(NOW, KEEP, sms(&long, &long));
            let m = &book.messages()[0];
            assert_eq!(m.text.chars().count(), MAX_TEXT_CHARS + 1, "{sample}");
            assert!(m.text.ends_with('…'));
            assert!(long.starts_with(m.text.trim_end_matches('…')), "{sample}");
            assert!(m.from.chars().count() <= MAX_LABEL_CHARS, "{sample}");
            assert!(long.starts_with(&m.from), "{sample}");
        }
        let mut book = SmsBook::default();
        // Exactly at the limit is left alone.
        let exact = "字".repeat(MAX_TEXT_CHARS);
        book.push(NOW, KEEP, sms("+15555550123", &exact));
        assert_eq!(book.messages()[0].text, exact);
        // Line breaks stay (as \n); the ends are trimmed.
        book.push(NOW, KEEP, sms("+15555550123", "\n  第一行\r\n二行目\n\n넷째 줄\rالسطر الأخير  \n"));
        assert_eq!(book.messages()[1].text, "第一行\n二行目\n\n넷째 줄\nالسطر الأخير");
        // Sender, receiving number, phone name and serial become one line.
        book.push(
            NOW,
            KEEP,
            NewSms { from: "  +1555 555\n0123 ", to: " +15555550100\t", serial: " TEST\nSERIAL ", device: "Pixel \n Test", text: "x", ..Default::default() },
        );
        let m = &book.messages()[2];
        assert_eq!((m.from.as_str(), m.to.as_str(), m.serial.as_str(), m.device.as_str()), ("+1555 555 0123", "+15555550100", "TEST SERIAL", "Pixel Test"));
        let wide = "機".repeat(MAX_LABEL_CHARS + 20);
        book.push(NOW, KEEP, NewSms { from: &wide, to: &wide, serial: &wide, device: &wide, text: "x", ..Default::default() });
        let m = &book.messages()[3];
        for label in [&m.from, &m.to, &m.serial, &m.device] {
            assert_eq!(label.chars().count(), MAX_LABEL_CHARS);
        }
    }

    #[test]
    fn sms_time_is_the_received_time_when_it_makes_sense() {
        let mut book = SmsBook::default();
        let at = |book: &mut SmsBook, received_ms: u64| {
            book.push(NOW, KEEP, NewSms { received_ms, from: "+15555550123", text: "x", ..Default::default() });
            book.messages().last().unwrap().at_ms
        };
        assert_eq!(at(&mut book, 0), NOW, "not given");
        assert_eq!(at(&mut book, NOW - 5_000), NOW - 5_000);
        assert_eq!(at(&mut book, NOW + 3_600_000), NOW + 3_600_000, "a phone clock an hour ahead");
        assert_eq!(at(&mut book, NOW + DAY_MS), NOW + DAY_MS);
        assert_eq!(at(&mut book, NOW + DAY_MS + 1), NOW, "too far ahead");
        assert_eq!(at(&mut book, u64::MAX), NOW);
    }

    #[test]
    fn sms_search_matches_sender_text_and_contact_name() {
        let mut book = SmsBook::default();
        let rows = [
            ("+15555550123", "Your code is 123456"),
            ("0912345678", "您的驗證碼是 654321"),
            ("+15555550125", "認証コードは 111222 です"),
            ("+15555550126", "인증번호는 333444 입니다"),
            ("+15555550127", "رمز التحقق هو 987666"),
            ("+15555550128", "รหัสยืนยันของคุณคือ 777888"),
            ("BANK", "Hello 😀 WORLD\nsecond line"),
        ];
        for (i, (from, text)) in rows.iter().enumerate() {
            book.push(NOW + i as u64, KEEP, sms(from, text));
        }
        let names = |from: &str| -> String {
            match from {
                "0912345678" => "王小明".into(),
                "+15555550123" => "Amy Lee".into(),
                _ => String::new(),
            }
        };
        let found = |q: &str| -> Vec<String> { book.page(q, None, PAGE_MAX, &names).messages.into_iter().map(|m| m.from).collect() };
        assert_eq!(found("").len(), rows.len());
        assert_eq!(found("   ").len(), rows.len());
        for (q, from) in [
            ("CODE", "+15555550123"),
            ("驗證碼", "0912345678"),
            ("認証コード", "+15555550125"),
            ("인증번호", "+15555550126"),
            ("التحقق", "+15555550127"),
            ("รหัสยืนยัน", "+15555550128"),
            ("😀", "BANK"),
            ("hello 😀 world second", "BANK"),
            ("bank", "BANK"),
            ("0912", "0912345678"),
            ("5550127", "+15555550127"),
            ("王小明", "0912345678"),
            ("amy", "+15555550123"),
            ("  Amy   LEE ", "+15555550123"),
        ] {
            assert_eq!(found(q), vec![from.to_string()], "q {q:?}");
        }
        assert_eq!(found("123456"), vec!["0912345678".to_string(), "+15555550123".to_string()], "a sender and a text");
        let none = book.page("nothing like this", None, PAGE_MAX, &names);
        assert!(none.messages.is_empty() && !none.has_more);
        assert_eq!(none.total, rows.len(), "the total does not follow the search");
    }

    #[test]
    fn sms_list_pages_newest_first_with_before_and_has_more() {
        let mut book = SmsBook::default();
        for i in 0..7u64 {
            book.push(NOW + i, KEEP, sms("+15555550123", if i % 2 == 0 { "even" } else { "odd" }));
        }
        let ids = |p: &Page| p.messages.iter().map(|m| m.id - NOW).collect::<Vec<_>>();
        let first = book.page("", None, 3, &no_name);
        assert_eq!((ids(&first), first.has_more, first.total), (vec![6, 5, 4], true, 7));
        let second = book.page("", first.messages.last().map(|m| m.id), 3, &no_name);
        assert_eq!((ids(&second), second.has_more), (vec![3, 2, 1], true));
        let third = book.page("", second.messages.last().map(|m| m.id), 3, &no_name);
        assert_eq!((ids(&third), third.has_more), (vec![0], false));
        let exact = book.page("", Some(NOW + 3), 3, &no_name);
        assert_eq!((ids(&exact), exact.has_more), (vec![2, 1, 0], false));
        // A search pages over its own matches.
        let even = book.page("even", None, 3, &no_name);
        assert_eq!((ids(&even), even.has_more), (vec![6, 4, 2], true));
        let even = book.page("even", Some(NOW + 2), 3, &no_name);
        assert_eq!((ids(&even), even.has_more), (vec![0], false));
        // The limit stays between 1 and PAGE_MAX.
        assert_eq!(book.page("", None, 0, &no_name).messages.len(), 1);
        assert_eq!(book.page("", None, 100_000, &no_name).messages.len(), 7);
        assert_eq!((PAGE_DEFAULT, PAGE_MAX), (50, 200));
    }

    #[test]
    fn sms_delete_and_clear() {
        let dir = temp_dir("delete");
        let store = SmsStore::open(dir.clone(), NOW, KEEP);
        let mut ids = Vec::new();
        for i in 0..3u64 {
            let Pushed::Stored(id) = store.push(NOW + i, KEEP, sms("+15555550123", &format!("m{i}"))) else { panic!() };
            ids.push(id);
        }
        assert!(store.delete(ids[1]));
        assert!(!store.delete(ids[1]), "already gone");
        assert!(!store.delete(42));
        let left = store.list(NOW + 5, KEEP, "", None, PAGE_DEFAULT, &no_name);
        assert_eq!(left.messages.iter().map(|m| m.id).collect::<Vec<_>>(), vec![ids[2], ids[0]]);
        assert_eq!(SmsStore::open(dir.clone(), NOW + 5, KEEP).list(NOW + 5, KEEP, "", None, PAGE_DEFAULT, &no_name).total, 2);
        assert_eq!(store.clear(), 2);
        assert_eq!(store.clear(), 0);
        assert_eq!(store.list(NOW + 6, KEEP, "", None, PAGE_DEFAULT, &no_name).total, 0);
        assert_eq!(SmsStore::open(dir.clone(), NOW + 6, KEEP).list(NOW + 6, KEEP, "", None, PAGE_DEFAULT, &no_name).total, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
