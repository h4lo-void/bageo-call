//! Reply latency: how long a caller waits between the end of their sentence and
//! the first sound of the AI's answer. Measurement only. The functions here are
//! pure arithmetic; `ReplyClock` keeps a few numbers per call and never waits.

use std::time::Duration;
use tokio::time::Instant;

/// One measured reply, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplyTiming {
    /// The caller's last voiced frame → the first audio of the model's turn.
    pub reply_ms: u64,
    /// Part of `reply_ms` spent waiting to be sure the caller was done (the silence window).
    pub wait_ms: u64,
    /// Part of `reply_ms` after the end was decided: model thinking plus network.
    pub model_ms: u64,
}

/// Who decided the caller's turn was over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndBy {
    /// The local VAD (also the only one when the server VAD is off).
    Local,
    /// Gemini's server VAD, ahead of the local one.
    Server,
    /// Gemini's server VAD, while the local VAD still heard the caller less than
    /// the server's window ago (noise it took for speech): the number is a lower bound.
    ServerEstimate,
}

impl EndBy {
    pub fn as_str(self) -> &'static str {
        match self {
            EndBy::Local => "local",
            EndBy::Server => "server",
            EndBy::ServerEstimate => "server_est",
        }
    }
}

/// A reply that was measured: its number in the call and its timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Measured {
    pub turn: usize,
    pub timing: ReplyTiming,
    pub end_by: EndBy,
}

/// The local VAD ended the turn after `silence_ms` of quiet (counted back from
/// that decision, so the caller stopped `silence_ms` before it), and the first
/// audio came `decided_to_audio_ms` after the decision.
pub fn timing_after_local_end(silence_ms: u32, decided_to_audio_ms: u64) -> ReplyTiming {
    let wait_ms = u64::from(silence_ms);
    ReplyTiming {
        reply_ms: wait_ms.saturating_add(decided_to_audio_ms),
        wait_ms,
        model_ms: decided_to_audio_ms,
    }
}

/// The first audio arrived while the local turn was still open, so the server
/// VAD ended the turn: the caller stopped `silence_ms` ago (the local count) and
/// the server waited `window_ms` of that before deciding; the rest is the model.
/// `None` when the local silence is shorter than the server's window: the two
/// disagree about when the caller stopped (see [`estimate_after_server_end`]).
pub fn timing_after_server_end(silence_ms: u32, window_ms: u32) -> Option<ReplyTiming> {
    if silence_ms < window_ms {
        return None;
    }
    let reply_ms = u64::from(silence_ms);
    let wait_ms = u64::from(window_ms);
    Some(ReplyTiming { reply_ms, wait_ms, model_ms: reply_ms - wait_ms })
}

/// The server VAD ended the turn but the local VAD heard the caller less than the
/// window ago, so the local count cannot say when the caller stopped. The server
/// needed a full window of silence to decide: the caller waited at least that long
/// (a lower bound; the model's own time is not known).
pub fn estimate_after_server_end(window_ms: u32) -> ReplyTiming {
    let wait_ms = u64::from(window_ms);
    ReplyTiming { reply_ms: wait_ms, wait_ms, model_ms: 0 }
}

/// Median of the samples (the mean of the two middle ones for an even count); 0 when empty.
pub fn median(samples: &[u64]) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let mut s = samples.to_vec();
    s.sort_unstable();
    let mid = s.len() / 2;
    if s.len() % 2 == 1 {
        s[mid]
    } else {
        (s[mid - 1] / 2) + (s[mid] / 2) + ((s[mid - 1] % 2 + s[mid] % 2) / 2)
    }
}

/// What a call's replies add up to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    pub count: usize,
    pub avg_ms: u64,
    pub median_ms: u64,
    pub max_ms: u64,
}

/// Count, mean, median and maximum of `reply_ms` samples (all 0 when there are none).
pub fn summarize(samples: &[u64]) -> Summary {
    let count = samples.len();
    let sum: u64 = samples.iter().fold(0u64, |a, &x| a.saturating_add(x));
    Summary {
        count,
        avg_ms: if count == 0 { 0 } else { sum / count as u64 },
        median_ms: median(samples),
        max_ms: samples.iter().copied().max().unwrap_or(0),
    }
}

/// The caller's end-of-turn decision that no reply has answered yet.
#[derive(Clone, Copy)]
struct Armed {
    decided_at: Instant,
    silence_ms: u32,
    /// The caller's turn began by cutting in on the AI.
    cut_in: bool,
}

/// Per-call state of the measurement. A reply counts only when it answers a
/// caller turn that ended: an opening, or anything Bageo prompts the model for
/// by itself, has no such turn and is never measured.
#[derive(Default)]
pub struct ReplyClock {
    armed: Option<Armed>,
    cut_in: bool,
    samples: Vec<u64>,
    /// Model turns that began with no caller turn to answer or after a cut-in
    /// (openings, Bageo's own prompts), so were not measured.
    unmeasured: usize,
}

impl ReplyClock {
    /// The local VAD ended the caller's turn now, after `silence_ms` of quiet.
    pub fn caller_turn_ended(&mut self, now: Instant, silence_ms: u32) {
        let cut_in = std::mem::take(&mut self.cut_in);
        self.armed = Some(Armed { decided_at: now, silence_ms, cut_in });
    }

    /// Nothing is owed any more as a reply to the caller: Bageo prompted the model
    /// itself, the connection was switched, or the caller's turn went unmeasurable.
    pub fn disarm(&mut self) {
        self.armed = None;
    }

    /// The caller cut in on the AI: the turn they are starting is not a plain reply.
    pub fn cut_in(&mut self) {
        self.armed = None;
        self.cut_in = true;
    }

    /// First audio of a model turn, the local VAD having ended the caller's turn.
    pub fn audio_after_local_end(&mut self, now: Instant) -> Option<Measured> {
        let armed = self.armed.take()?;
        if armed.cut_in {
            return None;
        }
        let gap: Duration = now.saturating_duration_since(armed.decided_at);
        let gap_ms = u64::try_from(gap.as_millis()).unwrap_or(u64::MAX);
        Some(self.keep(timing_after_local_end(armed.silence_ms, gap_ms), EndBy::Local))
    }

    /// First audio of a model turn while the local turn is still open: the server
    /// VAD ended it. `silence_ms` is the local silence so far; when it is shorter
    /// than the server's window the reply is still counted, as an estimate.
    pub fn audio_after_server_end(&mut self, silence_ms: u32, window_ms: u32) -> Option<Measured> {
        self.armed = None;
        if std::mem::take(&mut self.cut_in) {
            return None;
        }
        Some(match timing_after_server_end(silence_ms, window_ms) {
            Some(timing) => self.keep(timing, EndBy::Server),
            None => self.keep(estimate_after_server_end(window_ms), EndBy::ServerEstimate),
        })
    }

    /// A model turn began and no reply was measured for it.
    pub fn note_unmeasured(&mut self) {
        self.unmeasured += 1;
    }

    /// How many model turns were not measured (see [`Self::note_unmeasured`]).
    pub fn unmeasured(&self) -> usize {
        self.unmeasured
    }

    fn keep(&mut self, timing: ReplyTiming, end_by: EndBy) -> Measured {
        self.samples.push(timing.reply_ms);
        Measured { turn: self.samples.len(), timing, end_by }
    }

    /// The call's replies so far.
    pub fn summary(&self) -> Summary {
        summarize(&self.samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64, base: Instant) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn local_end_counts_the_silence_wait_back_from_the_decision() {
        let t = timing_after_local_end(720, 480);
        assert_eq!(t, ReplyTiming { reply_ms: 1200, wait_ms: 720, model_ms: 480 });
        assert_eq!(t.wait_ms + t.model_ms, t.reply_ms);
    }

    #[test]
    fn server_end_splits_the_local_silence_at_the_servers_window() {
        assert_eq!(
            timing_after_server_end(1100, 700),
            Some(ReplyTiming { reply_ms: 1100, wait_ms: 700, model_ms: 400 })
        );
        assert_eq!(
            timing_after_server_end(700, 700),
            Some(ReplyTiming { reply_ms: 700, wait_ms: 700, model_ms: 0 })
        );
        // The caller was heard again less than the window ago: no exact number.
        assert_eq!(timing_after_server_end(300, 700), None);
        assert_eq!(
            estimate_after_server_end(700),
            ReplyTiming { reply_ms: 700, wait_ms: 700, model_ms: 0 }
        );
    }

    #[test]
    fn median_and_summary() {
        assert_eq!(median(&[]), 0);
        assert_eq!(median(&[900]), 900);
        assert_eq!(median(&[1500, 900, 1200]), 1200);
        assert_eq!(median(&[1000, 2000, 4000, 1001]), 1500);
        assert_eq!(median(&[u64::MAX, u64::MAX]), u64::MAX);
        assert_eq!(
            summarize(&[1000, 2000, 6000]),
            Summary { count: 3, avg_ms: 3000, median_ms: 2000, max_ms: 6000 }
        );
        assert_eq!(
            summarize(&[]),
            Summary { count: 0, avg_ms: 0, median_ms: 0, max_ms: 0 }
        );
    }

    #[test]
    fn only_a_reply_to_a_caller_turn_is_measured() {
        let base = Instant::now();
        let mut c = ReplyClock::default();
        // An opening (or a prompt of our own): no caller turn ended, nothing measured.
        assert_eq!(c.audio_after_local_end(at(0, base)), None);
        c.caller_turn_ended(at(1000, base), 700);
        let m = c.audio_after_local_end(at(1600, base)).unwrap();
        assert_eq!(m.turn, 1);
        assert_eq!(m.end_by, EndBy::Local);
        assert_eq!(m.timing, ReplyTiming { reply_ms: 1300, wait_ms: 700, model_ms: 600 });
        // The rest of the same model turn is not a second reply.
        assert_eq!(c.audio_after_local_end(at(1700, base)), None);
        assert_eq!(c.summary().count, 1);
    }

    #[test]
    fn bageos_own_prompts_and_rotations_drop_the_pending_turn() {
        let base = Instant::now();
        let mut c = ReplyClock::default();
        c.caller_turn_ended(at(0, base), 700);
        c.disarm();
        assert_eq!(c.audio_after_local_end(at(16_000, base)), None);
        assert_eq!(c.summary().count, 0);
    }

    #[test]
    fn a_turn_that_began_by_cutting_in_is_not_measured_but_the_next_is() {
        let base = Instant::now();
        let mut c = ReplyClock::default();
        c.cut_in();
        c.caller_turn_ended(at(0, base), 700);
        assert_eq!(c.audio_after_local_end(at(900, base)), None);
        c.caller_turn_ended(at(5000, base), 700);
        assert_eq!(c.audio_after_local_end(at(5500, base)).unwrap().turn, 1);
        // Cutting in while a reply is owed drops that reply too.
        c.caller_turn_ended(at(8000, base), 700);
        c.cut_in();
        assert_eq!(c.audio_after_local_end(at(8500, base)), None);
    }

    #[test]
    fn a_server_ended_turn_is_measured_from_the_local_silence() {
        let base = Instant::now();
        let mut c = ReplyClock::default();
        c.caller_turn_ended(at(0, base), 700);
        // The turn is still open locally when audio comes: the pending local end is stale.
        // The local VAD heard noise inside the server's window: still counted, as an estimate.
        let m = c.audio_after_server_end(300, 700).unwrap();
        assert_eq!((m.turn, m.end_by), (1, EndBy::ServerEstimate));
        assert_eq!(m.timing, ReplyTiming { reply_ms: 700, wait_ms: 700, model_ms: 0 });
        assert_eq!(c.audio_after_local_end(at(900, base)), None);
        let m = c.audio_after_server_end(1000, 700).unwrap();
        assert_eq!((m.turn, m.end_by), (2, EndBy::Server));
        assert_eq!(m.timing.model_ms, 300);
        assert_eq!(c.summary().count, 2);
        c.cut_in();
        assert_eq!(c.audio_after_server_end(1000, 700), None);
    }

    #[test]
    fn turns_not_measured_are_counted_apart() {
        let mut c = ReplyClock::default();
        c.note_unmeasured();
        c.note_unmeasured();
        assert_eq!((c.unmeasured(), c.summary().count), (2, 0));
    }
}
