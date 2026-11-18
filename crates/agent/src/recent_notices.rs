// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The last few messages sent through `POST /calls/api/notify/send`, kept in
//! memory only so a desktop status app can poll for new ones and pop up a
//! notification. Nothing is written to disk; a restart empties the list.

use serde::Serialize;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// How many events are kept; older ones are dropped.
pub const MAX_EVENTS: usize = 50;
/// Longest `text`, in characters (an ellipsis is added after cutting).
pub const MAX_TEXT_CHARS: usize = 80;
/// Longest `title` and `from`, in characters.
pub const MAX_LABEL_CHARS: usize = 60;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RecentNotice {
    pub id: u64,
    pub at_ms: u64,
    /// `"sms"` or `"message"`.
    pub kind: String,
    pub title: String,
    pub from: String,
    pub text: String,
}

/// A bounded list of events with strictly increasing ids.
#[derive(Debug)]
pub struct RecentNotices {
    events: VecDeque<RecentNotice>,
    last_id: u64,
}

impl RecentNotices {
    pub const fn new() -> Self {
        Self { events: VecDeque::new(), last_id: 0 }
    }

    /// Add an event and return its id. The id is the larger of "previous id
    /// + 1" and the clock in Unix milliseconds, so it never goes backwards
    /// (not even across a restart, as long as the clock moves forward).
    pub fn push(&mut self, now_ms: u64, kind: &str, title: &str, from: &str, text: &str, body: &str) -> u64 {
        let id = self.last_id.saturating_add(1).max(now_ms);
        self.last_id = id;
        let kind = if kind.trim().eq_ignore_ascii_case("sms") { "sms" } else { "message" };
        let text_source = if kind == "sms" && !text.trim().is_empty() {
            text
        } else {
            body.lines().find(|l| !l.trim().is_empty()).unwrap_or("")
        };
        self.events.push_back(RecentNotice {
            id,
            at_ms: now_ms,
            kind: kind.to_string(),
            title: squash(title, MAX_LABEL_CHARS),
            from: squash(from, MAX_LABEL_CHARS),
            text: squash(text_source, MAX_TEXT_CHARS),
        });
        while self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
        id
    }

    /// The largest id so far, or 0 when nothing was recorded.
    pub fn last_id(&self) -> u64 {
        self.last_id
    }

    /// Events with an id greater than `after`, oldest first.
    pub fn after(&self, after: u64) -> Vec<RecentNotice> {
        self.events.iter().filter(|e| e.id > after).cloned().collect()
    }
}

/// Collapse runs of whitespace (newlines included) into one space, trim, and
/// cut to `max` characters (not bytes) with a trailing ellipsis.
fn squash(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        let mut cut: String = flat.chars().take(max).collect();
        cut.push('…');
        cut
    } else {
        flat
    }
}

static RECENT: Mutex<RecentNotices> = Mutex::new(RecentNotices::new());

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Record one event in the process-wide list.
pub fn record(kind: &str, title: &str, from: &str, text: &str, body: &str) -> u64 {
    let mut list = RECENT.lock().unwrap_or_else(|e| e.into_inner());
    list.push(now_ms(), kind, title, from, text, body)
}

/// `(last_id, events after the given id)` from the process-wide list.
pub fn snapshot(after: u64) -> (u64, Vec<RecentNotice>) {
    let list = RECENT.lock().unwrap_or_else(|e| e.into_inner());
    (list.last_id(), list.after(after))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_plain(list: &mut RecentNotices, now_ms: u64) -> u64 {
        list.push(now_ms, "message", "Bageo", "", "", "hello")
    }

    #[test]
    fn recent_ids_increase_and_never_go_back() {
        let mut list = RecentNotices::new();
        assert_eq!(list.last_id(), 0);
        assert_eq!(push_plain(&mut list, 1000), 1000);
        assert_eq!(push_plain(&mut list, 1000), 1001);
        // The clock stepped back: ids keep climbing.
        assert_eq!(push_plain(&mut list, 500), 1002);
        assert_eq!(push_plain(&mut list, 5000), 5000);
        assert_eq!(list.last_id(), 5000);
    }

    #[test]
    fn recent_keeps_only_the_last_fifty() {
        let mut list = RecentNotices::new();
        for i in 0..80u64 {
            list.push(1000 + i, "message", "t", "", "", &format!("m{i}"));
        }
        let all = list.after(0);
        assert_eq!(all.len(), MAX_EVENTS);
        assert_eq!(all.first().unwrap().text, "m30");
        assert_eq!(all.last().unwrap().text, "m79");
        assert!(all.windows(2).all(|w| w[0].id < w[1].id));
    }

    #[test]
    fn recent_kind_is_sms_or_message() {
        let mut list = RecentNotices::new();
        for (asked, want) in [("sms", "sms"), ("  SMS ", "sms"), ("Sms", "sms"), ("", "message"), ("sms2", "message"), ("call", "message")] {
            list.push(1, asked, "t", "", "", "b");
            assert_eq!(list.after(0).last().unwrap().kind, want, "asked {asked:?}");
        }
    }

    #[test]
    fn recent_text_uses_request_text_only_for_sms() {
        let mut list = RecentNotices::new();
        list.push(1, "sms", "Texts", "+15555550123", "Your code is 123456", "Texts\nbody line");
        list.push(2, "message", "Texts", "+15555550123", "ignored", "first line\nsecond line");
        list.push(3, "sms", "Texts", "+15555550123", "   ", "\n\n  body first line \nnext");
        let events = list.after(0);
        assert_eq!(events[0].text, "Your code is 123456");
        assert_eq!(events[0].from, "+15555550123");
        assert_eq!(events[1].text, "first line");
        assert_eq!(events[2].text, "body first line");
    }

    #[test]
    fn recent_text_squashes_whitespace() {
        let mut list = RecentNotices::new();
        list.push(1, "sms", "  A \t B\n", " x\n y ", " one\n\n  two \t three ", "");
        let e = &list.after(0)[0];
        assert_eq!(e.title, "A B");
        assert_eq!(e.from, "x y");
        assert_eq!(e.text, "one two three");
    }

    #[test]
    fn recent_text_cuts_by_characters() {
        let samples = [
            "這是一則很長的簡訊內容".repeat(20),
            "これは長いメッセージです。".repeat(20),
            "مرحبا بكم في خدمتنا ".repeat(20),
            "😀".repeat(100),
            "Привет, это длинное сообщение ".repeat(10),
        ];
        for s in samples {
            let mut list = RecentNotices::new();
            list.push(1, "sms", &s, &s, &s, "");
            let e = &list.after(0)[0];
            assert_eq!(e.text.chars().count(), MAX_TEXT_CHARS + 1, "{}", e.text);
            assert!(e.text.ends_with('…'));
            assert_eq!(e.from.chars().count(), MAX_LABEL_CHARS + 1);
            assert_eq!(e.title.chars().count(), MAX_LABEL_CHARS + 1);
            // Still valid text that starts like the input.
            assert!(s.trim().starts_with(e.text.trim_end_matches('…').trim_end()));
        }
        // Exactly at the limit is left alone.
        let exact = "字".repeat(MAX_TEXT_CHARS);
        let mut list = RecentNotices::new();
        list.push(1, "sms", "t", "", &exact, "");
        assert_eq!(list.after(0)[0].text, exact);
    }

    #[test]
    fn recent_after_filters_and_keeps_order() {
        let mut list = RecentNotices::new();
        let a = push_plain(&mut list, 100);
        let b = push_plain(&mut list, 200);
        let c = push_plain(&mut list, 300);
        assert_eq!(list.after(0).iter().map(|e| e.id).collect::<Vec<_>>(), vec![a, b, c]);
        assert_eq!(list.after(a).iter().map(|e| e.id).collect::<Vec<_>>(), vec![b, c]);
        assert!(list.after(c).is_empty());
        assert!(list.after(u64::MAX).is_empty());
    }

    #[test]
    fn recent_global_list_records_and_reads_back() {
        let marker = format!("recent-global-{}", now_ms());
        let id = record("sms", "Texts", "+15555550123", &marker, "");
        let (last, events) = snapshot(id - 1);
        assert!(last >= id);
        assert!(events.iter().any(|e| e.id == id && e.text == marker));
    }
}
