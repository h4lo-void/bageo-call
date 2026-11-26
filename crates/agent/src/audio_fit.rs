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
/// `Failed` is a later change from good to problem or unsupported.
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
    /// The owner said they heard nothing and no call has settled on new settings since.
    #[serde(default)]
    pub reported: bool,
    /// The last call's report said the phone only measures and cannot switch settings
    /// (Google Pixel): an owner's report is then only kept, not tested again.
    #[serde(default)]
    pub measure_only: bool,
    /// "Went bad" was already told in this round.
    #[serde(default)]
    pub failed_told: bool,
    /// "Please test" was due but no notification channel was set to take it.
    #[serde(default)]
    pub test_unsent: bool,
}

impl Record {
    fn new(fit: AudioFit) -> Self {
        Self { fit, answered: false, ask: false, reported: false, measure_only: false, failed_told: false, test_unsent: false }
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
/// - good (ok or adjusted) to problem or unsupported: that it went bad, once per round;
/// - everything else (ok and adjusted in either direction, problem and unsupported in either
///   direction, a bad phone that recovers, the same fit again): nothing.
///
/// "Please test it" that could not be sent (no channel) is due again while the phone is
/// still untested ([`mark_test_unsent`]).
pub fn on_fit(prev: Option<&Record>, now: AudioFit) -> (Record, Option<Notice>) {
    let Some(prev) = prev else {
        let mut rec = Record::new(now);
        let notice = match now {
            AudioFit::Untested => Some(Notice::AskTest),
            AudioFit::Problem | AudioFit::Unsupported => {
                rec.failed_told = true;
                Some(Notice::Failed(now))
            }
            AudioFit::Ok | AudioFit::Adjusted => None,
        };
        return (rec, notice);
    };
    let mut rec = *prev;
    rec.fit = now;
    if prev.fit == now {
        if now == AudioFit::Untested && prev.test_unsent {
            rec.test_unsent = false;
            return (rec, Some(Notice::AskTest));
        }
        return (rec, None);
    }
    if now == AudioFit::Untested {
        return (Record::new(now), Some(Notice::AskTest));
    }
    rec.test_unsent = false;
    if is_bad(now) {
        rec.ask = false;
    }
    let notice = if prev.fit == AudioFit::Untested {
        rec.failed_told |= is_bad(now);
        Some(Notice::Result(now))
    } else if is_bad(now) && !is_bad(prev.fit) && !prev.failed_told {
        rec.failed_told = true;
        Some(Notice::Failed(now))
    } else {
        None
    };
    (rec, notice)
}

/// "Please test it" was due but no channel took it: due again at the next report of the
/// phone while it is still untested.
pub fn mark_test_unsent(rec: &Record) -> Record {
    if rec.fit == AudioFit::Untested {
        Record { test_unsent: true, ..*rec }
    } else {
        *rec
    }
}

/// What a call's report (`uplink_health`) says besides the fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Call {
    /// The AI's voice really played in the call.
    pub live: bool,
    /// The report's reason is the owner's report: the phone is still where the owner left it.
    pub owner_report: bool,
    /// The phone only measures and cannot switch settings (Google Pixel).
    pub measure_only: bool,
}

/// A call ended and the phone reported. The owner is asked once per round, after a call
/// with the fit ok or adjusted. After the owner said no, the call that does not settle on
/// anything (too short, or a phone with nothing else to try) leaves that report standing.
pub fn on_call(rec: &Record, call: Call) -> Record {
    if !call.live {
        return *rec;
    }
    let mut r = *rec;
    r.measure_only = call.measure_only;
    if r.reported && r.fit == AudioFit::Problem && call.owner_report {
        r.ask = false;
        return r;
    }
    r.reported = false;
    r.ask = matches!(r.fit, AudioFit::Ok | AudioFit::Adjusted) && !r.answered;
    r
}

/// The owner heard the AI: not asked again in this round, and a later turn for the worse is
/// told again.
pub fn owner_heard(rec: &Record) -> Record {
    Record { answered: true, ask: false, failed_told: false, ..*rec }
}

/// The owner heard nothing: the fit is problem until a call settles on something else, and
/// the owner is asked again after it. The owner knows, so no message for this.
pub fn owner_did_not_hear(prev: &Record) -> Record {
    Record { fit: AudioFit::Problem, answered: false, ask: false, reported: true, failed_told: true, test_unsent: false, ..*prev }
}

/// What the console shows next to a phone besides its fit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct View {
    /// Ask "did you hear the AI?".
    pub ask: bool,
    /// The owner said no and the phone switched to other settings: the next call is the test.
    pub retest: bool,
    /// The owner said no on a phone that cannot switch settings: only noted.
    pub noted: bool,
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

    /// Write the book. Called with the lock held, so two updates reach the file in the order
    /// they were made and an older state never replaces a newer one.
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

    /// The phone reported its fit: in its hello (`call` None), or after a call. Returns the
    /// message for the owner, if any.
    pub fn observe(&self, serial: &str, now: AudioFit, call: Option<Call>) -> Option<Notice> {
        let mut map = self.guard();
        let prev = map.get(serial).copied();
        let (mut rec, notice) = on_fit(prev.as_ref(), now);
        if let Some(call) = call {
            rec = on_call(&rec, call);
        }
        if prev != Some(rec) {
            map.insert(serial.to_string(), rec);
            self.save(&map);
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

    /// "Please test" for this phone found no channel: due again later.
    pub fn test_unsent(&self, serial: &str) -> bool {
        self.change(serial, mark_test_unsent)
    }

    fn change(&self, serial: &str, f: impl FnOnce(&Record) -> Record) -> bool {
        let mut map = self.guard();
        let Some(prev) = map.get(serial).copied() else {
            return false;
        };
        let rec = f(&prev);
        if rec != prev {
            map.insert(serial.to_string(), rec);
            self.save(&map);
        }
        true
    }

    pub fn view(&self, serial: &str) -> View {
        self.guard()
            .get(serial)
            .map(|r| View { ask: r.ask, retest: r.reported && !r.measure_only, noted: r.reported && r.measure_only })
            .unwrap_or_default()
    }

    /// Forget a phone (it is removed from the console).
    pub fn forget(&self, serial: &str) -> bool {
        let mut map = self.guard();
        if map.remove(serial).is_none() {
            return false;
        }
        self.save(&map);
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

    fn live() -> Call {
        Call { live: true, owner_report: false, measure_only: false }
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
        let (rec, notice) = on_fit(None, Problem);
        assert_eq!(notice, Some(Notice::Failed(Problem)));
        assert_eq!(on_fit(Some(&rec), Problem).1, None);
        assert_eq!(on_fit(None, Unsupported).1, Some(Notice::Failed(Unsupported)));
    }

    #[test]
    fn the_first_test_gets_its_result_whatever_it_is() {
        for now in [Good, Adjusted, Problem, Unsupported] {
            assert_eq!(on_fit(Some(&seen(Untested)), now).1, Some(Notice::Result(now)), "{now:?}");
        }
    }

    #[test]
    fn after_the_test_only_a_turn_from_good_to_bad_is_told() {
        for from in [Good, Adjusted] {
            assert_eq!(on_fit(Some(&seen(from)), Problem).1, Some(Notice::Failed(Problem)), "{from:?}");
            assert_eq!(on_fit(Some(&seen(from)), Unsupported).1, Some(Notice::Failed(Unsupported)), "{from:?}");
        }
    }

    #[test]
    fn problem_and_unsupported_never_message_each_other() {
        assert_eq!(on_fit(Some(&seen(Problem)), Unsupported).1, None);
        assert_eq!(on_fit(Some(&seen(Unsupported)), Problem).1, None);
        // Not even when the owner was never told about the first one in this round.
        let told = Record { failed_told: true, ..seen(Problem) };
        assert_eq!(on_fit(Some(&told), Unsupported).1, None);
    }

    #[test]
    fn a_phone_that_flaps_is_told_once_in_a_round() {
        let mut rec = seen(Good);
        let mut told = 0;
        for now in [Problem, Good, Problem, Good, Problem] {
            let (next, notice) = on_fit(Some(&rec), now);
            told += usize::from(notice.is_some());
            rec = next;
        }
        assert_eq!(told, 1, "ok to problem, back and forth: only the first fall is told");
        // problem, ok, problem, ok, problem from a phone that was bad at first sight.
        let (mut rec, first) = on_fit(None, Problem);
        let mut told = usize::from(first.is_some());
        for now in [Good, Problem, Good, Problem] {
            let (next, notice) = on_fit(Some(&rec), now);
            told += usize::from(notice.is_some());
            rec = next;
        }
        assert_eq!(told, 1);
    }

    #[test]
    fn a_failed_test_counts_as_the_round_s_one_bad_message() {
        let (rec, notice) = on_fit(Some(&seen(Untested)), Problem);
        assert_eq!(notice, Some(Notice::Result(Problem)));
        let (rec, _) = on_fit(Some(&rec), Good);
        assert_eq!(on_fit(Some(&rec), Problem).1, None);
    }

    #[test]
    fn the_owner_saying_yes_or_a_new_round_allows_another_bad_message() {
        let (rec, _) = on_fit(Some(&seen(Good)), Problem);
        let (rec, _) = on_fit(Some(&rec), Good);
        assert_eq!(on_fit(Some(&rec), Problem).1, None);
        let heard = owner_heard(&rec);
        assert_eq!(on_fit(Some(&heard), Problem).1, Some(Notice::Failed(Problem)));
        // A new round (untested again) too.
        let (round, _) = on_fit(Some(&rec), Untested);
        let (round, _) = on_fit(Some(&round), Good);
        assert_eq!(on_fit(Some(&round), Problem).1, Some(Notice::Failed(Problem)));
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
        let answered = Record { answered: true, failed_told: true, ..seen(Good) };
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
    fn please_test_that_found_no_channel_is_due_again_while_untested() {
        let (rec, notice) = on_fit(None, Untested);
        assert_eq!(notice, Some(Notice::AskTest));
        let rec = mark_test_unsent(&rec);
        let (rec, again) = on_fit(Some(&rec), Untested);
        assert_eq!(again, Some(Notice::AskTest), "a channel may exist by now");
        assert_eq!(on_fit(Some(&rec), Untested).1, None, "once it was sent it is done");
        // A phone that has been tested meanwhile does not get it, and forgets it was due.
        let (rec, _) = on_fit(None, Untested);
        let rec = mark_test_unsent(&rec);
        let (rec, notice) = on_fit(Some(&rec), Good);
        assert_eq!(notice, Some(Notice::Result(Good)));
        assert!(!rec.test_unsent);
        assert_eq!(mark_test_unsent(&seen(Good)), seen(Good), "only an untested phone is waiting for it");
    }

    #[test]
    fn the_owner_is_asked_after_a_call_that_ended_ok_or_adjusted() {
        for fit in [Good, Adjusted] {
            let rec = on_call(&seen(fit), live());
            assert!(rec.ask, "{fit:?}");
        }
    }

    #[test]
    fn the_owner_is_not_asked_when_the_fit_is_not_ok_or_the_ai_did_not_play() {
        for fit in [Untested, Problem, Unsupported] {
            assert!(!on_call(&seen(fit), live()).ask, "{fit:?}");
        }
        let silent = Call { live: false, ..live() };
        assert!(!on_call(&seen(Good), silent).ask, "no AI voice in the call");
        let pending = Record { ask: true, ..seen(Good) };
        assert_eq!(on_call(&pending, silent), pending, "a call without the AI changes nothing");
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
        let rec = owner_heard(&on_call(&seen(Good), live()));
        assert!(rec.answered && !rec.ask);
        assert!(!on_call(&rec, live()).ask, "the next call does not ask");
        // A new round asks again.
        let (next, _) = on_fit(Some(&rec), Untested);
        let (next, _) = on_fit(Some(&next), Good);
        assert!(on_call(&next, live()).ask);
    }

    #[test]
    fn no_marks_a_problem_and_asks_again_after_the_next_test_call() {
        let asked = on_call(&seen(Adjusted), live());
        let rec = owner_did_not_hear(&asked);
        assert_eq!(rec, Record { fit: Problem, reported: true, failed_told: true, ..seen(Problem) });
        // The phone confirms the problem: nothing to tell, the owner knows.
        assert_eq!(on_fit(Some(&rec), Problem).1, None);
        // The next call plays with other settings and ends well: ask again.
        let (rec, notice) = on_fit(Some(&rec), Good);
        assert_eq!(notice, None);
        let rec = on_call(&rec, live());
        assert!(rec.ask && !rec.reported);
        // It ends bad again, on its own: no question, the problem line shows.
        let (bad, _) = on_fit(Some(&owner_did_not_hear(&asked)), Problem);
        let bad = on_call(&bad, live());
        assert!(!bad.ask && !bad.reported);
    }

    #[test]
    fn a_call_too_short_to_settle_leaves_the_owners_report_standing() {
        let rec = owner_did_not_hear(&on_call(&seen(Good), live()));
        // The phone still says problem for the owner's report: the test is still due.
        let short = Call { owner_report: true, ..live() };
        let (after, notice) = on_fit(Some(&rec), Problem);
        assert_eq!(notice, None);
        let after = on_call(&after, short);
        assert!(after.reported && !after.ask);
        let again = on_call(&after, short);
        assert_eq!(again, after);
        // A call without the AI changes nothing either.
        assert_eq!(on_call(&after, Call { live: false, ..short }), after);
        // Once a call settles (the reason is gone) the report is over.
        let (ok, _) = on_fit(Some(&after), Good);
        let settled = on_call(&ok, live());
        assert!(!settled.reported && settled.ask);
    }

    #[test]
    fn a_phone_that_cannot_switch_only_notes_the_owners_report() {
        let fixed = Call { live: true, owner_report: false, measure_only: true };
        let rec = on_call(&seen(Good), fixed);
        assert!(rec.measure_only);
        let rec = owner_did_not_hear(&rec);
        let book = FitBook::default();
        book.guard().insert("pixel-1".into(), rec);
        assert_eq!(book.view("pixel-1"), View { ask: false, retest: false, noted: true });
        // Its next calls keep reporting problem for the owner's report: still only noted.
        let still = Call { owner_report: true, ..fixed };
        for _ in 0..3 {
            book.observe("pixel-1", Problem, Some(still));
        }
        assert_eq!(book.view("pixel-1"), View { ask: false, retest: false, noted: true });
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
        assert_eq!(book.observe("pixel-1", Adjusted, Some(live())), Some(Notice::Result(Adjusted)));
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
    fn a_pending_please_test_survives_a_restart() {
        let path = temp_file();
        let book = FitBook::open(path.clone());
        assert_eq!(book.observe("pixel-1", Untested, None), Some(Notice::AskTest));
        assert!(book.test_unsent("pixel-1"));
        drop(book);
        let book = FitBook::open(path.clone());
        assert_eq!(book.observe("pixel-1", Untested, None), Some(Notice::AskTest));
        assert_eq!(book.observe("pixel-1", Untested, None), None);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn each_phone_has_its_own_round() {
        let book = FitBook::default();
        assert_eq!(book.observe("pixel-1", Untested, None), Some(Notice::AskTest));
        assert_eq!(book.observe("pixel-2", Untested, None), Some(Notice::AskTest));
        assert_eq!(book.observe("pixel-1", Untested, None), None);
        assert_eq!(book.observe("pixel-2", Good, Some(live())), Some(Notice::Result(Good)));
        assert!(!book.view("pixel-1").ask && book.view("pixel-2").ask);
    }

    #[test]
    fn answers_need_a_known_phone_and_forgetting_starts_over() {
        let book = FitBook::default();
        assert!(!book.heard("pixel-1") && !book.did_not_hear("pixel-1") && !book.test_unsent("pixel-1"));
        book.observe("pixel-1", Good, Some(live()));
        assert!(book.did_not_hear("pixel-1"));
        assert_eq!(book.view("pixel-1"), View { ask: false, retest: true, noted: false });
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

    #[test]
    fn updates_at_the_same_time_all_reach_the_file() {
        let path = temp_file();
        let book = FitBook::open(path.clone());
        let threads = 8;
        let per_thread = 25;
        std::thread::scope(|s| {
            for t in 0..threads {
                let book = &book;
                s.spawn(move || {
                    for i in 0..per_thread {
                        let serial = format!("pixel-{t}-{i}");
                        book.observe(&serial, Untested, None);
                        book.observe(&serial, Good, Some(live()));
                        if i % 2 == 0 {
                            book.heard(&serial);
                        }
                    }
                });
            }
        });
        let reread = FitBook::open(path.clone());
        for t in 0..threads {
            for i in 0..per_thread {
                let serial = format!("pixel-{t}-{i}");
                assert_eq!(reread.view(&serial), book.view(&serial), "{serial}: the file holds the latest state");
                assert_eq!(reread.view(&serial).ask, i % 2 == 1, "{serial}");
            }
        }
        assert_eq!(reread.guard().len(), threads * per_thread);
        assert_eq!(*reread.guard(), *book.guard(), "the file is exactly what is in memory");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
