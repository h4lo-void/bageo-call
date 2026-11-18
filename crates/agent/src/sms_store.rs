//! Text messages the SMS forwarder hands to POST /calls/api/notify/send with
//! "kind": "sms", kept on disk so the console's SMS page can show, search and
//! delete them.
//!
//! One JSON file (sms/messages.json under the calls data folder), written
//! whole on every change: to a temporary file first, then renamed over the old
//! one. Messages older than the sms_retention_days setting are dropped, and at
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

    pub fn messages(&self) -> &[SmsMessage] {
        &self.messages
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
        let id = self.last_id.saturating_add(1).max(now_ms);
        self.last_id = id;
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
        });
        self.prune(now_ms, keep_days);
        Pushed::Stored(id)
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
        let pruned = book.prune(now_ms, keep_days);
        let store = Self { path, book: Mutex::new(book) };
        if pruned {
            store.save(&store.lock());
        }
        Arc::new(store)
    }

    fn lock(&self) -> MutexGuard<'_, SmsBook> {
        self.book.lock().unwrap_or_else(|e| e.into_inner())
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
