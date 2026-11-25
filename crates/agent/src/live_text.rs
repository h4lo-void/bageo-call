// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The words of a call going on now, for the owner's page while the AI talks
//! (`GET /api/call/{id}/live`): each phone's running transcript, the one the
//! record is made from when the call ends. Kept in memory only; a phone's entry
//! is replaced when its next call starts, and read only for a call still live
//! that began before the entry was made (never the last call's words).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use chrono::{DateTime, Utc};

struct Entry {
    words: Arc<Mutex<String>>,
    since: DateTime<Utc>,
}

fn entries() -> &'static Mutex<HashMap<String, Entry>> {
    static ENTRIES: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    ENTRIES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The transcript a phone's call now writes into.
pub fn started(serial: &str, words: &Arc<Mutex<String>>) {
    if let Ok(mut m) = entries().lock() {
        m.insert(serial.to_string(), Entry { words: words.clone(), since: Utc::now() });
    }
}

/// What has been said so far on the call that started at `call_started`
/// (RFC 3339) on `serial`, one line per speaker run as the record has it;
/// None when that phone's transcript belongs to an earlier call.
pub fn words(serial: &str, call_started: &str) -> Option<String> {
    let started = DateTime::parse_from_rfc3339(call_started).ok()?.with_timezone(&Utc);
    let words = {
        let m = entries().lock().ok()?;
        let e = m.get(serial)?;
        if e.since < started {
            return None;
        }
        e.words.clone()
    };
    let raw = words.lock().ok()?.clone();
    Some(crate::locale::fix_names(&crate::gemini_live::merge_dialogue_fragments(raw.trim())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_call_after_the_transcript_started_reads_it() {
        let serial = format!("live-text-{}", uuid::Uuid::new_v4().simple());
        let before = (Utc::now() - chrono::Duration::seconds(5)).to_rfc3339();
        assert!(words(&serial, &before).is_none());
        let buf = Arc::new(Mutex::new(String::new()));
        started(&serial, &buf);
        buf.lock().unwrap().push_str("對方：Hello,\n對方： is anyone there?\n接聽：Yes, this is the assistant.");
        let got = words(&serial, &before).unwrap();
        assert!(got.contains("對方：Hello") && got.contains("接聽：Yes"), "{got}");
        assert_eq!(got.lines().count(), 2, "{got}");
        // A call that began after the transcript did: the words are not its own.
        let later = (Utc::now() + chrono::Duration::seconds(5)).to_rfc3339();
        assert!(words(&serial, &later).is_none());
    }
}

