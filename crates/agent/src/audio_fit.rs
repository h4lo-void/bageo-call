// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! What the owner is told and asked about a phone's call audio (`audio_fit`, see
//! [`crate::uplink_health`]): one notification when a new phone needs a test call, one with
//! the result, one when it later turns bad, and the "did you hear the AI?" question of the
//! console. The rules are pure functions ([`on_fit`], [`on_call`], [`owner_heard`],
//! [`owner_did_not_hear`]); [`FitBook`] keeps each phone's state on disk, so a restart of
//! Bageo or a reconnecting phone never repeats a message.

use crate::uplink_health::AudioFit;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use tracing::warn;

/// A message for the owner. `Result` follows the first test (untested to anything else);
/// `Failed` is a later change to problem or unsupported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    AskTest,
    Result(AudioFit),
    Failed(AudioFit),
}

/// What is kept of one phone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// The last `audio_fit` the phone reported.
    pub fit: AudioFit,
    /// The owner said they heard the AI, in this round (a round starts when the phone is
    /// untested).
    #[serde(default)]
    pub answered: bool,
    /// A call ended with the fit ok or adjusted and the owner has not answered yet.
    #[serde(default)]
    pub ask: bool,
    /// The owner said they heard nothing: the phone tries other settings on its next call.
    #[serde(default)]
    pub retest: bool,
}

impl Record {
    fn new(fit: AudioFit) -> Self {
        Self { fit, answered: false, ask: false, retest: false }
    }
}

fn is_bad(fit: AudioFit) -> bool {
    matches!(fit, AudioFit::Problem | AudioFit::Unsupported)
}

/// The phone reported `now`. Returns what is kept afterwards and what the owner is told:
/// - never seen: a phone that is untested gets "please test it"; one already ok or adjusted
///   is a known model and gets nothing; one that is bad is told once;
/// - back to untested (a system update): a new round, "please test it" again, and the
///   owner's earlier answer is forgotten;
/// - untested to anything else: the result of the test;
/// - anything else to problem or unsupported: that it went bad;
/// - everything else (ok and adjusted in either direction, a bad phone that recovers,
///   the same fit again): nothing.
pub fn on_fit(prev: Option<&Record>, now: AudioFit) -> (Record, Option<Notice>) {
    let Some(prev) = prev else {
        let notice = match now {
            AudioFit::Untested => Some(Notice::AskTest),
            AudioFit::Problem | AudioFit::Unsupported => Some(Notice::Failed(now)),
            AudioFit::Ok | AudioFit::Adjusted => None,
        };
        return (Record::new(now), notice);
    };
    let mut rec = *prev;
    rec.fit = now;
    if prev.fit == now {
        return (rec, None);
    }
    if now == AudioFit::Untested {
        return (Record::new(now), Some(Notice::AskTest));
    }
    if now != AudioFit::Ok && now != AudioFit::Adjusted {
        rec.ask = false;
    }
    let notice = if prev.fit == AudioFit::Untested {
        Some(Notice::Result(now))
    } else if is_bad(now) {
        Some(Notice::Failed(now))
    } else {
        None
    };
    (rec, notice)
}

/// A call ended and the phone reported (`live`: the AI's voice really played). The owner is
/// asked once per round, after a call with the fit ok or adjusted; a pending retest is over.
pub fn on_call(rec: &Record, live: bool) -> Record {
    if !live {
        return *rec;
    }
    let mut r = *rec;
    r.retest = false;
    r.ask = matches!(r.fit, AudioFit::Ok | AudioFit::Adjusted) && !r.answered;
    r
}

/// The owner heard the AI: not asked again in this round.
pub fn owner_heard(rec: &Record) -> Record {
    Record { answered: true, ask: false, ..*rec }
}

/// The owner heard nothing: the fit is problem until the next test call says otherwise, and
/// the owner is asked again after it.
pub fn owner_did_not_hear(_prev: &Record) -> Record {
    Record { fit: AudioFit::Problem, answered: false, ask: false, retest: true }
}

/// What the console shows next to a phone besides its fit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct View {
    pub ask: bool,
    pub retest: bool,
}

/// Every phone's [`Record`], kept in a file.
#[derive(Default)]
pub struct FitBook {
    path: Option<PathBuf>,
    map: Mutex<BTreeMap<String, Record>>,
}

impl FitBook {
    /// The book in `path` (empty when there is no file yet). A file that cannot be read is
    /// moved aside and the book starts empty.
    pub fn open(path: PathBuf) -> Self {
        let (map, writable) = crate::errands::read_json::<BTreeMap<String, Record>>(&path, "phone call audio");
        Self { path: writable.then_some(path), map: Mutex::new(map.unwrap_or_default()) }
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Record>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, map: &BTreeMap<String, Record>) {
        let Some(path) = &self.path else {
            return;
        };
        match serde_json::to_vec_pretty(map) {
            Ok(body) => {
                if let Err(e) = crate::errands::write_atomic(path, &body) {
                    warn!("phone call audio not saved: {e:#}");
                }
            }
            Err(e) => warn!("phone call audio not encoded: {e}"),
        }
    }

    /// The phone reported its fit: in its hello (`call` None), or after a call (`call` is
    /// whether the AI's voice played). Returns the message for the owner, if any.
    pub fn observe(&self, serial: &str, now: AudioFit, call: Option<bool>) -> Option<Notice> {
        let mut map = self.guard();
        let prev = map.get(serial).copied();
        let (mut rec, notice) = on_fit(prev.as_ref(), now);
        if let Some(live) = call {
            rec = on_call(&rec, live);
        }
        if prev != Some(rec) {
            map.insert(serial.to_string(), rec);
            let snapshot = map.clone();
            drop(map);
            self.save(&snapshot);
        }
        notice
    }

    /// The owner heard the AI. False when the phone is not in the book.
    pub fn heard(&self, serial: &str) -> bool {
        self.change(serial, owner_heard)
    }

    /// The owner heard nothing. False when the phone is not in the book.
    pub fn did_not_hear(&self, serial: &str) -> bool {
        self.change(serial, owner_did_not_hear)
    }

    fn change(&self, serial: &str, f: impl FnOnce(&Record) -> Record) -> bool {
        let mut map = self.guard();
        let Some(prev) = map.get(serial).copied() else {
            return false;
        };
        let rec = f(&prev);
        if rec != prev {
            map.insert(serial.to_string(), rec);
            let snapshot = map.clone();
            drop(map);
            self.save(&snapshot);
        }
        true
    }

    pub fn view(&self, serial: &str) -> View {
        self.guard().get(serial).map(|r| View { ask: r.ask, retest: r.retest }).unwrap_or_default()
    }

    /// Forget a phone (it is removed from the console).
    pub fn forget(&self, serial: &str) -> bool {
        let mut map = self.guard();
        if map.remove(serial).is_none() {
            return false;
        }
        let snapshot = map.clone();
        drop(map);
        self.save(&snapshot);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use AudioFit::{Adjusted, Ok as Good, Problem, Unsupported, Untested};

    const ALL: [AudioFit; 5] = [Untested, Good, Adjusted, Problem, Unsupported];

    fn seen(fit: AudioFit) -> Record {
        Record::new(fit)
    }

    #[test]
    fn a_new_untested_phone_is_asked_to_test_once() {
        let (rec, notice) = on_fit(None, Untested);
        assert_eq!(notice, Some(Notice::AskTest));
        assert_eq!(on_fit(Some(&rec), Untested).1, None, "it reconnects untested: not asked again");
    }

    #[test]
    fn a_known_good_phone_gets_no_message() {
        assert_eq!(on_fit(None, Good).1, None);
        assert_eq!(on_fit(None, Adjusted).1, None);
    }

    #[test]
    fn a_new_phone_that_is_already_bad_is_told_once() {
        assert_eq!(on_fit(None, Problem).1, Some(Notice::Failed(Problem)));
        assert_eq!(on_fit(None, Unsupported).1, Some(Notice::Failed(Unsupported)));
        assert_eq!(on_fit(Some(&seen(Problem)), Problem).1, None);
    }

    #[test]
    fn the_first_test_gets_its_result_whatever_it_is() {
        for now in [Good, Adjusted, Problem, Unsupported] {
            assert_eq!(on_fit(Some(&seen(Untested)), now).1, Some(Notice::Result(now)), "{now:?}");
        }
    }

    #[test]
    fn after_the_test_only_a_turn_to_problem_or_unsupported_is_told() {
        for from in [Good, Adjusted] {
            assert_eq!(on_fit(Some(&seen(from)), Problem).1, Some(Notice::Failed(Problem)), "{from:?}");
            assert_eq!(on_fit(Some(&seen(from)), Unsupported).1, Some(Notice::Failed(Unsupported)), "{from:?}");
        }
        assert_eq!(on_fit(Some(&seen(Problem)), Unsupported).1, Some(Notice::Failed(Unsupported)));
        assert_eq!(on_fit(Some(&seen(Unsupported)), Problem).1, Some(Notice::Failed(Problem)));
    }

    #[test]
    fn ok_and_adjusted_never_message_each_other() {
        assert_eq!(on_fit(Some(&seen(Good)), Adjusted).1, None);
        assert_eq!(on_fit(Some(&seen(Adjusted)), Good).1, None);
    }

    #[test]
    fn recovering_from_a_problem_is_quiet() {
        assert_eq!(on_fit(Some(&seen(Problem)), Good).1, None);
        assert_eq!(on_fit(Some(&seen(Unsupported)), Adjusted).1, None);
    }

    #[test]
    fn the_same_fit_again_is_quiet() {
        for f in ALL {
            assert_eq!(on_fit(Some(&seen(f)), f).1, None, "{f:?}");
        }
    }

    #[test]
    fn back_to_untested_is_a_new_round() {
        let answered = Record { fit: Good, answered: true, ask: false, retest: false };
        let (rec, notice) = on_fit(Some(&answered), Untested);
        assert_eq!(notice, Some(Notice::AskTest));
        assert_eq!(rec, seen(Untested), "the earlier answer is forgotten");
        // The new round tells the result of its own test.
        assert_eq!(on_fit(Some(&rec), Adjusted).1, Some(Notice::Result(Adjusted)));
        for from in [Good, Adjusted, Problem, Unsupported] {
            assert_eq!(on_fit(Some(&seen(from)), Untested).1, Some(Notice::AskTest), "{from:?}");
        }
    }

    #[test]
    fn the_owner_is_asked_after_a_call_that_ended_ok_or_adjusted() {
        for fit in [Good, Adjusted] {
            let rec = on_call(&seen(fit), true);
            assert!(rec.ask, "{fit:?}");
        }
    }

    #[test]
    fn the_owner_is_not_asked_when_the_fit_is_not_ok_or_the_ai_did_not_play() {
        for fit in [Untested, Problem, Unsupported] {
            assert!(!on_call(&seen(fit), true).ask, "{fit:?}");
        }
        assert!(!on_call(&seen(Good), false).ask, "no AI voice in the call");
        let pending = Record { ask: true, ..seen(Good) };
        assert_eq!(on_call(&pending, false), pending, "a call without the AI changes nothing");
    }

    #[test]
    fn a_turn_to_a_bad_fit_withdraws_the_question() {
        let asking = Record { ask: true, ..seen(Good) };
        assert!(!on_fit(Some(&asking), Problem).0.ask);
        assert!(!on_fit(Some(&asking), Unsupported).0.ask);
        assert!(on_fit(Some(&asking), Adjusted).0.ask);
    }

    #[test]
    fn yes_is_remembered_and_not_asked_again_in_the_round() {
        let rec = owner_heard(&on_call(&seen(Good), true));
        assert!(rec.answered && !rec.ask);
        assert!(!on_call(&rec, true).ask, "the next call does not ask");
        // A new round asks again.
        let (next, _) = on_fit(Some(&rec), Untested);
        let (next, _) = on_fit(Some(&next), Good);
        assert!(on_call(&next, true).ask);
    }

    #[test]
    fn no_marks_a_problem_and_asks_again_after_the_next_test_call() {
        let asked = on_call(&seen(Adjusted), true);
        let rec = owner_did_not_hear(&asked);
        assert_eq!(rec, Record { fit: Problem, answered: false, ask: false, retest: true });
        // The phone confirms the problem: nothing to tell, the owner knows.
        assert_eq!(on_fit(Some(&rec), Problem).1, None);
        // The next call plays with other settings and ends well: ask again.
        let (rec, notice) = on_fit(Some(&rec), Good);
        assert_eq!(notice, None);
        let rec = on_call(&rec, true);
        assert!(rec.ask && !rec.retest);
        // It ends bad again: no question, the problem line shows.
        let (bad, _) = on_fit(Some(&owner_did_not_hear(&asked)), Problem);
        let bad = on_call(&bad, true);
        assert!(!bad.ask && !bad.retest);
    }

    fn temp_file() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bageo-audio-fit-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("audio_fit.json")
    }

    #[test]
    fn a_restart_does_not_repeat_a_message() {
        let path = temp_file();
        let book = FitBook::open(path.clone());
        assert_eq!(book.observe("pixel-1", Untested, None), Some(Notice::AskTest));
        drop(book);
        let book = FitBook::open(path.clone());
        assert_eq!(book.observe("pixel-1", Untested, None), None, "Bageo restarted, the phone reconnects");
        assert_eq!(book.observe("pixel-1", Adjusted, Some(true)), Some(Notice::Result(Adjusted)));
        assert!(book.view("pixel-1").ask);
        drop(book);
        let book = FitBook::open(path.clone());
        assert_eq!(book.observe("pixel-1", Adjusted, None), None);
        assert_eq!(book.observe("pixel-1", Good, None), None);
        assert!(book.view("pixel-1").ask, "the question survives a restart");
        assert!(book.heard("pixel-1"));
        drop(book);
        assert_eq!(FitBook::open(path.clone()).view("pixel-1"), View::default(), "and so does the answer");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn each_phone_has_its_own_round() {
        let book = FitBook::default();
        assert_eq!(book.observe("pixel-1", Untested, None), Some(Notice::AskTest));
        assert_eq!(book.observe("pixel-2", Untested, None), Some(Notice::AskTest));
        assert_eq!(book.observe("pixel-1", Untested, None), None);
        assert_eq!(book.observe("pixel-2", Good, Some(true)), Some(Notice::Result(Good)));
        assert!(!book.view("pixel-1").ask && book.view("pixel-2").ask);
    }

    #[test]
    fn answers_need_a_known_phone_and_forgetting_starts_over() {
        let book = FitBook::default();
        assert!(!book.heard("pixel-1") && !book.did_not_hear("pixel-1"));
        book.observe("pixel-1", Good, Some(true));
        assert!(book.did_not_hear("pixel-1"));
        assert_eq!(book.view("pixel-1"), View { ask: false, retest: true });
        assert!(book.forget("pixel-1") && !book.forget("pixel-1"));
        assert_eq!(book.observe("pixel-1", Untested, None), Some(Notice::AskTest), "a forgotten phone is new again");
    }

    #[test]
    fn a_book_file_that_is_not_a_book_is_moved_aside() {
        let path = temp_file();
        std::fs::write(&path, b"not json").unwrap();
        let book = FitBook::open(path.clone());
        assert_eq!(book.observe("pixel-1", Untested, None), Some(Notice::AskTest));
        assert_eq!(FitBook::open(path.clone()).view("pixel-1"), View::default());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
