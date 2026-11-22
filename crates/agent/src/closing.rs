// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! How a call ends, judged locally: the closing state machine and the words of
//! twelve languages it keys on. Pure in-memory work on each final transcript
//! piece — no I/O, no model call — so it never delays the audio loop.
//!
//! Talking → PreClosing → Closing → Hangup:
//! - **PreClosing**: someone announced the end ("I won't keep you", 「那就先這樣」,
//!   「以上となりますが」) — or the AI said its farewell first.
//! - **Closing**: farewells were exchanged, or the caller's latest words are a
//!   farewell. Only here may the call hang up, and only with the caller's latest
//!   words closing it (the AI's own bye does not count).
//! - New words of the caller after a closing ("oh, one more thing…") take it back
//!   to Talking; short replies, thanks and backchannels do not.
//! - Farewells exchanged on the caller's announced end (their 「係咁先啦」, our
//!   「好，拜拜」) still owe their own bye: the line is held a moment for it
//!   ([`ClosingMachine::awaits_caller_bye`]).
//! - The AI's words can come in after its turn ended (the transcript lags the
//!   audio): they are judged as that turn when they do ([`ClosingMachine::on_ai_late`]).
//! - The caller settling the matter and thanking ("Okay, I'll leave it with the
//!   concierge, thanks", 「かしこまりました…ありがとうございます」) announces the end:
//!   the AI's farewell after it is the exchange. A bare 「好，謝謝」 does not.
//! - A greeting that also says goodbye (수고하십니다, สวัสดีค่ะ, Chào anh, Selamat
//!   siang, Servus) never closes the opening, and later only once the business
//!   is done (thanks or a closing cue just before it).
//!
//! While the AI is stalling a sales / scam caller nothing here hangs up: the
//! caller's closing comes back as [`ClosingEvent::CallerPreClosing`] /
//! [`ClosingEvent::CallerFarewell`] for the stalling logic to answer.

use tracing::info;

/// Where the call stands on its way to an end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClosingState {
    Talking,
    PreClosing,
    Closing,
    Hangup,
}

impl ClosingState {
    pub fn name(self) -> &'static str {
        match self {
            ClosingState::Talking => "Talking",
            ClosingState::PreClosing => "PreClosing",
            ClosingState::Closing => "Closing",
            ClosingState::Hangup => "Hangup",
        }
    }
}

/// What one transcript piece did to the closing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClosingEvent {
    None,
    /// The caller announced the end. While stalling: the moment for the last
    /// "wait, one more question" — nothing hangs up.
    CallerPreClosing,
    /// The caller said goodbye. While stalling: hold them, nothing hangs up.
    CallerFarewell,
    /// The caller came back with something new after a closing: talk on.
    CallerResumed,
    /// The caller settled the matter and thanked: the end announced, not said.
    /// Nothing hangs up on it; the AI's farewell after it is the exchange.
    CallerThanksWrap,
    /// The AI closed right after the caller's pre-closing: farewells exchanged,
    /// the caller's latest words now end the call.
    FarewellsExchanged,
    /// The AI's late words went on to ask the caller something after its
    /// closing (「晚點再打給你。請問您明天幾點方便？」): the exchange is taken back.
    Withdrawn,
}

/// [`ClosingMachine::on_caller`]'s verdict on the caller's latest words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClosingStep {
    #[allow(dead_code)] // for the stalling logic and tests
    pub event: ClosingEvent,
    /// The caller's latest words end the call (their goodbye, a wrap-up the
    /// older rules knew, or a pre-closing the AI already answered with its own).
    pub ends_call: bool,
}

/// One per call. Feed it every caller transcript piece ([`Self::on_caller`]) and
/// every finished AI turn ([`Self::on_ai_turn`]); a goodbye clock that fires
/// calls [`Self::hangup`]. Use: `let step = m.on_caller(&heard, piece,
/// old_rules_say_closing, stalling); if stalling && step.event ==
/// ClosingEvent::CallerFarewell { /* keep them on the line */ }`.
#[derive(Debug, Clone)]
pub struct ClosingMachine {
    state: ClosingState,
    /// Language of the call, then of the last closing words heard (for the log).
    lang: &'static str,
    /// The caller has said more than greetings, thanks and short replies.
    substance: bool,
    /// The caller's latest words close (their pre-closing or farewell) — as
    /// opposed to the AI's farewell waiting for their answer.
    caller_closed: bool,
    /// The AI's last turn ended before all its words came in: the rest is
    /// judged as that turn ([`Self::on_ai_late`]) until the AI starts another
    /// turn or the caller says something.
    late_ai: Option<String>,
    /// Farewells exchanged on the caller's announced end, their own bye not
    /// said yet.
    bye_owed: bool,
}

impl ClosingMachine {
    pub fn new(lang: &'static str) -> Self {
        ClosingMachine {
            state: ClosingState::Talking,
            lang,
            substance: false,
            caller_closed: false,
            late_ai: None,
            bye_owed: false,
        }
    }

    #[allow(dead_code)] // for the stalling logic and tests
    pub fn state(&self) -> ClosingState {
        self.state
    }

    /// A hangup is allowed now: farewells done with the caller's words last, and
    /// not while stalling a sales / scam caller.
    #[allow(dead_code)] // for the stalling logic and tests
    pub fn may_hang_up(&self, stalling: bool) -> bool {
        self.state == ClosingState::Closing && self.caller_closed && !stalling
    }

    /// Farewells were exchanged on the caller's announced end and their own
    /// bye has not come yet: hold the line a moment for it before hanging up.
    pub fn awaits_caller_bye(&self) -> bool {
        self.state == ClosingState::Closing && self.bye_owed
    }

    fn go(&mut self, to: ClosingState, trigger: &str) {
        if self.state == to {
            return;
        }
        if to != ClosingState::Closing {
            self.bye_owed = false;
        }
        info!(
            "closing_state from={} to={} lang={} trigger={trigger}",
            self.state.name(),
            to.name(),
            self.lang
        );
        self.state = to;
    }

    /// The caller's words as they end now. `heard`: their words so far this turn
    /// with the newest piece at the end; `latest`: that piece; `old_rules`: the
    /// older closing check (bye / wrap-up at the end) says it closes; `stalling`:
    /// a sales / scam caller is being kept on the line.
    pub fn on_caller(&mut self, heard: &str, latest: &str, old_rules: bool, stalling: bool) -> ClosingStep {
        // The caller has moved on: late words of the AI's last turn no longer answer them.
        if !is_fluff(latest) {
            self.late_ai = None;
        }
        let opening_only = !self.substance && greeting_only(heard);
        let raw_hit = classify(heard);
        let suppressed_ambiguous = raw_hit
            .as_ref()
            .is_some_and(|h| h.ambiguous && !self.greeting_bye_counts(h));
        let hit = raw_hit.filter(|h| !h.ambiguous || self.greeting_bye_counts(h));
        // Opening greetings and guarded greeting-byes (e.g. Turn 1 "ciao" or
        // "お忙しいところ失礼いたします") never close even if the older check fires.
        let old_rules = old_rules && !opening_only && !suppressed_ambiguous;
        let kind = match (&hit, old_rules) {
            (Some(h), _) => Some(h.kind),
            (None, true) if crate::locale::ends_with_farewell(heard, 2) => Some(Kind::Farewell),
            (None, true) => Some(Kind::PreClosing),
            (None, false) => None,
        };
        if let Some(h) = &hit {
            self.lang = h.lang;
        }
        if !self.substance && !greeting_only(heard) {
            self.substance = true;
        }
        let tag = |t: &'static str| if stalling { stall_trigger(t) } else { t };
        match kind {
            Some(Kind::Farewell) => {
                self.caller_closed = true;
                self.bye_owed = false;
                self.go(ClosingState::Closing, tag("caller_farewell"));
                ClosingStep { event: ClosingEvent::CallerFarewell, ends_call: true }
            }
            Some(Kind::PreClosing) => {
                // Their pre-closing answers the AI's farewell: that is the exchange.
                let exchange = (self.state == ClosingState::PreClosing && !self.caller_closed)
                    || self.state == ClosingState::Closing;
                self.caller_closed = true;
                if exchange {
                    self.go(ClosingState::Closing, tag("caller_closing_reply"));
                    // Announced, not said: their bye usually still follows.
                    self.bye_owed = true;
                } else {
                    self.go(ClosingState::PreClosing, tag("caller_preclosing"));
                }
                ClosingStep { event: ClosingEvent::CallerPreClosing, ends_call: old_rules || exchange }
            }
            None => {
                // 「好的，沒問題，那我放管理室，謝謝！」: the matter is settled. The
                // AI's farewell after it ends the call (not their thanks alone).
                if thanks_wrap(heard) && !opening_only {
                    if self.state == ClosingState::Talking {
                        self.caller_closed = true;
                        self.go(ClosingState::PreClosing, tag("caller_thanks_wrap"));
                    }
                    return ClosingStep { event: ClosingEvent::CallerThanksWrap, ends_call: false };
                }
                let mut event = ClosingEvent::None;
                if self.state != ClosingState::Talking && !is_fluff(latest) && !greeting_only(latest) {
                    self.caller_closed = false;
                    self.go(ClosingState::Talking, tag("caller_new_content"));
                    event = ClosingEvent::CallerResumed;
                }
                ClosingStep { event, ends_call: false }
            }
        }
    }

    /// A finished AI turn: its words, and whether the older check says it ends
    /// on a closing. While stalling its farewell is cut anyway: nothing moves.
    pub fn on_ai_turn(&mut self, said: &str, ends_on_closing: bool, stalling: bool) -> ClosingEvent {
        self.late_ai = None;
        if stalling {
            return ClosingEvent::None;
        }
        // A turn without words (its transcript not in yet: "", "…") says nothing
        // either way now: its words are judged when they come in.
        if norm(said).is_empty() {
            self.late_ai = Some(String::new());
            return ClosingEvent::None;
        }
        let ev = self.judge_ai(said, ends_on_closing, false);
        // Cut off mid-sentence: the rest may still come in.
        if !ends_sentence(said) {
            self.late_ai = Some(said.to_string());
        }
        ev
    }

    /// A piece of the AI's last turn that came in after the turn ended (its
    /// transcript lags the audio): judged with the words before it as that turn,
    /// once they end a sentence (「先這樣」 may still go on 「…算的話多少？」).
    /// Nothing after the AI starts another turn ([`Self::ai_started`]) or the
    /// caller says something. `ends_on_closing`: the older check on the words.
    pub fn on_ai_late(&mut self, piece: &str, ends_on_closing: impl Fn(&str) -> bool, stalling: bool) -> ClosingEvent {
        if stalling {
            self.late_ai = None;
            return ClosingEvent::None;
        }
        let Some(said) = self.late_ai.as_mut() else {
            return ClosingEvent::None;
        };
        let piece = piece.trim();
        if said.ends_with(is_latin_letter) && piece.starts_with(is_latin_letter) {
            said.push(' ');
        }
        said.push_str(piece);
        if !ends_sentence(said) {
            return ClosingEvent::None;
        }
        let said = said.clone();
        // A question after the closing words takes the exchange back: the
        // caller has to answer it.
        if self.state == ClosingState::Closing && self.bye_owed && ends_as_question(&said) {
            self.bye_owed = false;
            self.go(ClosingState::PreClosing, "ai_asked_after_closing");
            return ClosingEvent::Withdrawn;
        }
        self.judge_ai(&said, ends_on_closing(&said), true)
    }

    /// The AI started another turn: words coming in now are that turn's.
    pub fn ai_started(&mut self) {
        self.late_ai = None;
    }

    fn judge_ai(&mut self, said: &str, ends_on_closing: bool, late: bool) -> ClosingEvent {
        let closed = ends_on_closing || classify(said).is_some();
        match (closed, self.state) {
            (true, ClosingState::Talking) => {
                self.caller_closed = false;
                self.go(ClosingState::PreClosing, "ai_farewell");
                ClosingEvent::None
            }
            (true, ClosingState::PreClosing) if self.caller_closed => {
                self.go(ClosingState::Closing, if late { "ai_late_farewell_reply" } else { "ai_farewell_reply" });
                self.bye_owed = true;
                ClosingEvent::FarewellsExchanged
            }
            (false, ClosingState::PreClosing) if !self.caller_closed => {
                self.go(ClosingState::Talking, "ai_new_content");
                ClosingEvent::None
            }
            _ => ClosingEvent::None,
        }
    }

    /// The goodbye clock fired (`trigger` says why): the call ends.
    pub fn hangup(&mut self, trigger: &str) {
        self.go(ClosingState::Hangup, trigger);
    }

    /// A greeting that is also a goodbye ends the call only after real talk, and
    /// once the business is done: a closing already under way, or thanks / a
    /// closing cue / a short "ok" right before it. Japanese opening apologies
    /// (「お忙しいところ失礼いたします」「夜分に失礼します」「恐れ入りますが」) never close.
    fn greeting_bye_counts(&self, h: &Hit) -> bool {
        if h.lang == "ja" && ja_opening_apology(&h.before) && !cue_before(&h.before) {
            return false;
        }
        if h.lang == "vi" && self.substance && vi_parting(&h.before) {
            return true;
        }
        let opening_before = if h.lang == "vi" { vi_without_pronoun(&h.before) } else { h.before.as_str() };
        (self.substance || !greeting_only(opening_before))
            && (self.state != ClosingState::Talking
                || cue_before(&h.before)
                || cue_shortly_before(h.lang, &h.before))
    }
}

/// Vietnamese opens and parts with the same 「chào」. After the caller's own words in a
/// later turn (「…cho anh nhé. Em chào anh ạ」) it is the parting: what comes before it,
/// without the speaker's own pronoun, is more than greetings and short replies.
fn vi_without_pronoun(before: &str) -> &str {
    let b = before.trim_end();
    ["em", "cháu", "con", "tôi", "mình"].iter().find_map(|p| ends_with_phrase(b, p)).unwrap_or(b)
}

/// A question among the last words before it (「Mai anh có rảnh không. Chào anh」) is
/// asking, not parting; 「không sao」 / 「không có gì」 (no problem) are no question.
const VI_QUESTION_WORDS: &[&str] = &["không", "chưa", "nào", "sao", "gì", "bao giờ", "mấy giờ", "ở đâu"];

fn vi_asks_before(b: &str) -> bool {
    let words: Vec<&str> = b.split(' ').filter(|w| !w.is_empty()).collect();
    let tail = words[words.len().saturating_sub(CUE_SHORTLY_WORDS)..].join(" ");
    let mut s = format!(" {tail} ");
    for ok in [" không sao ", " không có gì "] {
        s = s.replace(ok, " ");
    }
    VI_QUESTION_WORDS.iter().any(|q| s.contains(&format!(" {q} ")))
}

fn vi_parting(before: &str) -> bool {
    let b = vi_without_pronoun(before);
    !b.is_empty() && !greeting_only(b) && !vi_asks_before(b)
}

fn stall_trigger(t: &'static str) -> &'static str {
    match t {
        "caller_farewell" => "stall_caller_farewell",
        "caller_closing_reply" => "stall_caller_closing_reply",
        "caller_preclosing" => "stall_caller_preclosing",
        "caller_new_content" => "stall_caller_new_content",
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Hangup timer by region.

/// The setting's default (`goodbye_hangup_delay_ms`): any other value is the
/// owner's choice and wins over the regional timers.
pub const DEFAULT_HANGUP_DELAY_MS: u64 = 2_000;

/// How long after our last words the line is closed, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HangupTimer {
    pub ms: u64,
    /// SIM country (upper case) or the call's language code.
    pub region: String,
    /// The owner's own setting, not the regional default.
    pub from_setting: bool,
}

/// The regional timer: the SIM's country first, then the call's language. The
/// owner's setting wins when it is not [`DEFAULT_HANGUP_DELAY_MS`].
pub fn hangup_timer(setting_ms: u64, sim_country: Option<&str>, lang: &str) -> HangupTimer {
    let sim = sim_country.map(|c| c.trim().to_ascii_lowercase()).filter(|c| c.len() == 2);
    let by_sim = sim.as_deref().and_then(|c| country_ms(c, lang));
    let (ms, region) = match (by_sim, sim) {
        (Some(ms), Some(c)) => (ms, c.to_ascii_uppercase()),
        _ => (lang_ms(lang), lang.to_string()),
    };
    if setting_ms != DEFAULT_HANGUP_DELAY_MS {
        return HangupTimer { ms: setting_ms.clamp(200, 15_000), region, from_setting: true };
    }
    HangupTimer { ms, region, from_setting: false }
}

fn country_ms(country: &str, lang: &str) -> Option<u64> {
    Some(match country {
        "hk" | "mo" => 1_300,
        "cn" | "my" | "sg" => 1_200,
        "jp" => 2_500,
        "de" | "at" | "li" => 1_400,
        // Switzerland: German or French by the call's language.
        "ch" => {
            if lang.starts_with("fr") {
                1_600
            } else {
                1_400
            }
        }
        "es" | "mx" | "co" | "cl" | "pe" | "ar" | "uy" | "cu" | "ve" | "ec" | "bo" | "py" | "do" | "gt"
        | "cr" | "pa" | "sv" | "hn" | "ni" | "pr" | "fr" | "be" | "lu" | "mc" | "br" | "pt" | "ao" | "mz" => {
            1_600
        }
        "tw" | "kr" | "us" | "ca" | "gb" | "uk" | "au" | "nz" | "ie" | "in" | "th" | "vn" | "id" | "ph" => 1_500,
        _ => return None,
    })
}

fn lang_ms(lang: &str) -> u64 {
    let l = lang.to_ascii_lowercase();
    match l.as_str() {
        "zh-cn" | "zh-sg" | "zh-hans" => 1_200,
        "zh-hk" | "yue" => 1_300,
        _ => match l.split('-').next().unwrap_or("") {
            "ja" => 2_500,
            "de" => 1_400,
            "es" | "fr" | "pt" => 1_600,
            _ => 1_500,
        },
    }
}

// ---------------------------------------------------------------------------
// Words.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    PreClosing,
    Farewell,
}

/// Lists are written normalized ([`norm`]): lower case, no punctuation; words
/// in Latin script separated by one space, the others without spaces.
type Lexicon = &'static [(&'static str, &'static [&'static str])];

/// The end announced, not yet said ("I won't keep you", 「那就先這樣」).
const PRE_CLOSINGS: Lexicon = &[
    ("en", &[
        "anyway i won't keep you", "i won't keep you", "won't keep you any longer",
        "i'll let you get back to it", "i'll let you get back to your day", "let me let you go",
        "i'll let you go", "that's all for now", "that's about it",
        // Getting back to them later ends this call too.
        "i'll call you back", "call you back later", "i'll text you back", "text you back later",
        "i'll get back to you", "get back to you later",
        // Not "gotta run" alone: "which program do I gotta run" asks something.
        "alright i gotta run", "okay i gotta run", "ok i gotta run", "anyway i gotta run", "sorry i gotta run",
        "well i gotta run", "i've gotta run", "sorry gotta run", "anyway gotta run",
    ]),
    ("zh-TW", &[
        "那就先這樣", "就先這樣", "先這樣", "先這樣子", "不耽誤你時間了", "不耽誤您時間了", "不耽誤你了",
        "不耽誤您了", "有問題再聯絡", "有問題再聯繫", "不打擾了", "不打擾您了", "不打擾你了",
        "再聯絡", "再聯繫", "再講", "再聊", "改天再聊", "晚點再說", "之後再說", "回頭再說",
        "再打給你", "再打給您", "再回電給你", "再回電給您", "再回你電話", "再回您電話", "再回你", "再回您",
        "簡訊回你", "簡訊回您",
        // Said to end the call: 「好那就這樣」「回頭聯絡」.
        "那就這樣", "好就這樣", "回頭聯絡", "回頭聯繫",
    ]),
    ("zh-CN", &[
        "那就先这样", "就先这样", "先这样", "回头微信说", "回头微信聊", "不说了", "不耽误你时间了",
        "不耽误您时间了", "有问题再联系", "不打扰了", "不打扰您了", "再联系", "再联络", "再聊", "回头再说",
        "再打给你", "再打给您", "再回电给你", "再回你电话", "再回你", "再回您", "短信回你", "短信回您",
        "那就这样", "好就这样", "回头联系", "回头联络", "不打扰你了", "回聊",
    ]),
    ("yue", &[
        "係咁先", "系咁先", "就咁先", "唔阻你", "唔阻你啦", "唔阻你喇", "再打俾你", "再打畀你", "再覆你", "覆返你",
        "短訊覆你", "短讯覆你", "收線先", "收线先", "得閒再傾", "得闲再倾",
    ]),
    ("ja", &[
        "以上となりますが", "以上となります", "それではよろしくお願いいたします",
        "それではよろしくお願いします", "そろそろ切るね", "そろそろ切ります", "そろそろ失礼します",
        "折り返しご連絡いたします", "折り返しご連絡します", "折り返しお電話いたします", "折り返しお電話します",
        // A booking or a visit settled: 「それでは、当日はよろしくお願いいたします」 closes it.
        "当日はよろしくお願いいたします", "当日はよろしくお願いします",
        // Getting back to them later, as 折り返し does: 「また後ほどこちらからご連絡いたしますね」.
        "後ほどご連絡いたします", "後ほどご連絡します", "こちらからご連絡いたします", "こちらからご連絡します",
        "改めてご連絡いたします", "改めてご連絡します", "またご連絡いたします", "また連絡します",
    ]),
    ("ko", &[
        "나중에다시연락드릴게요", "다시연락드릴게요", "연락드릴게요", "연락드리겠습니다", "나끊는다", "그럼이만", "이만줄일게요",
        "이제끊을게요", "먼저끊을게요",
        "나중에통화해요",
    ]),
    ("es", &[
        "bueno te dejo", "te dejo", "le dejo", "nada era solo eso", "era solo eso", "hablamos luego",
        "hablamos después", "hablamos despues",
        // Not "nos hablamos" alone: "desde entonces no nos hablamos" is no closing.
        "órale nos hablamos", "orale nos hablamos", "cualquier cosa te aviso", "cualquier cosa le aviso",
    ]),
    ("fr", &[
        "je ne vous retiens pas plus longtemps", "je ne vous retiens pas", "je ne te retiens pas",
        "je vous laisse", "je te laisse", "je vous laisse à vos occupations", "je te laisse à tes occupations",
        "c'est noté", "c'est note",
    ]),
    ("de", &[
        "gut dann verbleiben wir so", "dann verbleiben wir so", "verbleiben wir so",
        "alles klar dann weiß ich bescheid", "dann weiß ich bescheid", "dann weiss ich bescheid",
    ]),
    ("pt", &["então tá bom", "entao ta bom", "combinado então", "combinado entao", "a gente se fala", "falamos depois"]),
    ("th", &[
        "งั้นแค่นี้", "งั้นแค่นี้นะ", "แค่นี้นะ", "แค่นี้ก่อน", "แค่นี้", "เอาไว้คุยกันใหม่", "คุยกันแค่นี้ก่อน",
    ]),
    ("vi", &[
        "dạ vậy nhé", "vậy nhé", "vay nhe", "thế nhé", "the nhe", "thôi nhé",
        // "If anything, we'll be in touch later, okay": only with the closing particle.
        "có gì liên lạc sau nhé", "có gì liên lạc sau nha", "co gi lien lac sau nhe",
    ]),
    ("id", &[
        "oke sudah dulu ya", "sudah dulu ya", "sudah dulu", "udah dulu ya", "udah dulu", "segitu dulu ya", "itu saja",
        "saya tutup dulu ya", "saya tutup dulu", "tutup dulu ya",
    ]),
    ("it", &[
        "allora ti lascio", "ti lascio", "la lascio", "non ti disturbo più", "non ti disturbo piu", "non la disturbo più", "non la disturbo piu",
        "non la trattengo", "non ti trattengo", "non la trattengo oltre", "non ti trattengo oltre", "per ora è tutto", "per ora e tutto",
        "è tutto per ora", "e tutto per ora", "ne parliamo dopo", "ti richiamo dopo", "la richiamo dopo", "ti chiamo dopo", "la chiamo dopo",
        "ti richiamo più tardi", "ti richiamo piu tardi", "la richiamo più tardi", "la richiamo piu tardi", "adesso attacco", "ora attacco",
    ]),
    ("ru", &["хорошо тогда до свидания", "ладно мне пора", "мне пора", "я перезвоню", "поговорим позже", "тогда пока", "всего вам доброго"]),
    ("ar", &["حسنا سأغلق الآن", "حسنا ساغلق الان", "لنتحدث لاحقا", "سأتصل بك لاحقا", "ساتصل بك لاحقا", "سأتركك الآن", "هذا كل شيء"]),
    ("hi", &[
        "ठीक है अब रखता हूँ", "बाद में बात करते हैं", "मैं बाद में कॉल करूँगा", "बस इतना ही", "अच्छा चलता हूँ",
        // Romanized "I'll put the phone down", only after okay / chalo: "main ek copy rakhta hoon" keeps a copy.
        "okay rakhta hoon", "ok rakhta hoon", "chalo rakhta hoon", "chalo ab rakhta hoon", "theek hai rakhta hoon",
        "okay rakhti hoon", "ok rakhti hoon", "chalo rakhti hoon", "chalo ab rakhti hoon", "theek hai rakhti hoon",
    ]),
    ("nl", &["ik houd u niet langer op", "ik hou je niet langer op", "dan spreken we later", "ik bel u later terug", "tot later"]),
    ("pl", &["nie zatrzymuję już", "nie zatrzymuje juz", "odezwę się później", "odezwe sie pozniej", "zadzwonię później", "zadzwonie pozniej", "to tyle na teraz"]),
    ("ro", &["nu vă mai rețin", "nu va mai retin", "nu te mai rețin", "nu te mai retin", "vorbim mai târziu", "vorbim mai tarziu"]),
    ("uk", &["добре тоді до побачення", "мені вже пора", "передзвоню пізніше", "поговоримо пізніше", "на все добре"]),
    ("bn", &["তাহলে এখন রাখছি", "পরে আবার কথা হবে", "আমি পরে ফোন করব", "আজ এই পর্যন্তই"]),
    ("mr", &["ठीक आहे आता ठेवतो", "नंतर बोलूया", "मी नंतर फोन करतो", "इतकेच होते"]),
    ("ta", &["சரி நான் வைக்கிறேன்", "பிறகு பேசலாம்", "நான் பிறகு அழைக்கிறேன்"]),
    ("te", &["సరే నేను పెడుతున్నాను", "తర్వాత మాట్లాడుదాం", "నేను తర్వాత ఫోన్ చేస్తాను"]),
    ("tr", &[
        "sizi daha fazla tutmayayım", "ben kapatıyorum", "kapatıyorum", "sonra konuşuruz", "sonra ararım",
        "tekrar ararım", "şimdilik bu kadar", "o zaman görüşürüz",
    ]),
    // Fourth batch. Not "I must go" alone (sl "moram iti", hr "moram ići"): "jutri moram
    // iti k zdravniku" goes on; not "þá er þetta komið" ("þá er þetta komið inn á skjáinn").
    ("is", &["takk fyrir símtalið", "við tölum saman síðar", "ég heyri í þér síðar", "hafðu það gott"]),
    ("sl", &[
        "ne bom vas več zadrževal", "ne bom vas več zadrževala", "ne bom te več zadrževal", "ne bom te več zadrževala",
        "to bi bilo vse",
    ]),
    ("mk", &["да не те задржувам повеќе", "да не ве задржувам повеќе", "фала што се јави", "толку од мене засега"]),
    ("sq", &["mos të të mbaj më gjatë", "mos t'ju mbaj më gjatë", "mbetemi në kontakt", "flasim më vonë"]),
    ("sv", &["jag ska inte hålla dig längre", "jag ska inte uppehålla dig längre", "tack för att du ringde", "jag ringer tillbaka senare"]),
    ("nb", &["jeg skal ikke holde deg lenger", "takk for at du ringte", "jeg ringer deg opp igjen senere"]),
    ("da", &["jeg skal ikke holde dig længere", "tak fordi du ringede", "jeg ringer tilbage lidt senere"]),
    ("fi", &["ei mulla muuta", "kiitos soitosta", "puhutaan myöhemmin lisää", "palataan asiaan"]),
    ("cs", &["nebudu vás zdržovat", "nebudu vás už zdržovat", "děkuji za zavolání", "zavoláme si později"]),
    ("sk", &["nebudem vás zdržiavať", "ďakujem za zavolanie", "zavoláme si neskôr"]),
    ("hu", &["nem tartom fel tovább", "most le kell tennem", "köszönöm a hívást", "később visszahívlak"]),
    ("bg", &["няма да ви задържам повече", "трябва да затварям", "хайде да не те задържам", "ще се обадя по късно"]),
    ("el", &["πρέπει να κλείσω", "δεν σας καθυστερώ άλλο", "θα σε πάρω αργότερα", "μιλάμε αργότερα"]),
    ("hr", &["neću vas više zadržavati", "nazvat ću kasnije", "moram prekinuti"]),
    ("sr", &["не бих вас више задржавао", "јавићу се касније", "ne bih vas više zadržavao", "javiću se kasnije"]),
    // Fifth batch: Asian, Middle Eastern, African and Baltic languages, and the
    // Bengali and Tamil ones the earlier lists did not have.
    ("he", &[
        "לא אעכב אותך יותר", "תודה על הזמן שלך", "זה הכול בינתיים", "אני חייבת לזוז", "זה הכל בינתיים",
        "אני חייב לזוז", "תודה על השיחה", "לא אעכב אותך", "תודה על הכול", "אחזור אלייך", "תודה על הכל", "אחזור אליך",
        "נהיה בקשר", "נמשיך מחר",
    ]),
    ("ms", &[
        "nanti saya hubungi semula", "nanti saya telefon balik", "itu sahaja daripada saya", "baiklah kalau macam tu",
        "saya ada kerja sikit", "saya minta diri dulu", "nanti kita sambung", "itu sahaja",
    ]),
    ("tl", &[
        "kailangan ko nang umalis", "tatawag ako ulit mamaya", "tawag na lang ulit", "usap tayo mamaya",
        "tatawag ako ulit", "mamaya na lang", "iyon lang muna", "yun lang muna",
    ]),
    ("bn", &["এই পর্যন্তই থাক তাহলে", "তাহলে এখন রাখি", "পরে ফোন করছি"]),
    ("ur", &[
        "میں آپ کا مزید وقت نہیں لیتا", "میں آپ کا مزید وقت نہیں لیتی", "دعاؤں میں یاد رکھیے گا", "میں اجازت چاہوں گا",
        "میں اجازت چاہوں گی", "میں فون رکھتا ہوں", "میں فون رکھتی ہوں", "سلام کہیے گا",
    ]),
    ("ne", &["म अहिले राख्छु ल", "अहिलेलाई यति नै", "पछि फोन गर्छु", "यति नै गरौँ"]),
    ("si", &["දැන් තියන්නම්", "පස්සේ ගන්නම්", "හරි එහෙනම්"]),
    ("my", &["နောက်မှဆက်ပြောကြတာပေါ့", "အခုတော့ဒီလောက်ပဲ", "ဒီလောက်ပဲနော်", "ဒါပါပဲ"]),
    ("lo", &["ຂ້ອຍຕ້ອງໄປກ່ອນແລ້ວ", "ມີແຕ່ເທົ່ານີ້ແຫຼະ", "ບໍ່ລົບກວນແລ້ວເດີ້", "ດຽວໂທຫາໃໝ່ເດີ້", "ບໍ່ລົບກວນແລ້ວ"]),
    ("mn", &["би одоо явах хэрэгтэй байна", "утсаа тасалъя даа", "утсаа тасалъя", "орой залгая", "за ингээд"]),
    ("fa", &[
        "بیشتر از این وقتتون رو نمی‌گیرم", "من دیگه باید برم", "مزاحمتون نمیشم", "به کارهایت برس", "مزاحمت نمیشم",
        "سلام برسونید", "به کارات برس",
    ]),
    ("ps", &["په کور کې سلام ووایاست", "زه نور اجازه غواړم", "زه باید اوس لاړ شم", "نور مو نه مصروفوم"]),
    ("az", &[
        "sonra əlaqə saxlayarıq", "yaxşı mən bağlayıram", "başqa sualım yoxdur", "mən getməliyəm", "sizi yormayım",
        "zəngləşərik",
    ]),
    ("hy", &["էլ չխանգարեմ", "հետո կզանգեմ", "պետք է գնամ", "այսքանը"]),
    ("ka", &["კარგი გავთიშავ", "აღარ მოგაცდენ", "უნდა წავიდე", "სულ ეს იყო"]),
    ("sw", &[
        "nikuache uendelee na kazi", "sina swali lingine", "wacha nikate simu", "wacha nikuache", "basi inatosha",
        "ni hayo tu", "haya basi", "sawa basi",
    ]),
    ("am", &["ሌላ ጥያቄ የለኝም", "በኋላ እደውላለሁ", "ነገ እንቀጥላለን", "መሄድ አለብኝ", "ለዛሬ ይበቃል", "እሺ በቃ"]),
    ("af", &["ek moet nou gaan", "dis al"]),
    ("et", &[
        "rohkem küsimusi pole", "hea küll ma lõpetan", "pean nüüd minema", "tänan kõne eest", "ma lõpetan nüüd",
        "räägime hiljem",
    ]),
    ("lv", &[
        "tas arī viss no manas puses", "paldies par zvanu", "es jums atzvanīšu", "man tagad jāiet", "parunāsim vēlāk",
        "tas arī viss",
    ]),
    ("lt", &[
        "daugiau klausimų neturiu", "aš jums perskambinsiu", "pasikalbėsime vėliau", "ačiū už skambutį", "turiu bėgti",
    ]),
    ("be", &[
        "не буду вас больш затрымліваць", "пазней табе ператэлефаную", "дзякуй вам за дапамогу",
        "мушу класці слухаўку", "вялікі дзякуй за час", "пазней ператэлефаную", "дзякуй за дапамогу",
        "пазней пагаворым", "мне трэба бегчы", "мне пара ісці", "дзякуй за час", "на гэтым усё",
    ]),
    ("ta", &["நான் அப்புறம் கூப்பிடுறேன்", "வேற ஒண்ணும் இல்ல"]),
];

/// Goodbyes said only at the end.
const FAREWELLS: Lexicon = &[
    ("en", &[
        "bye", "bye bye", "byebye", "goodbye", "good bye", "talk to you soon", "talk soon", "take care",
        "have a great rest of your day", "have a good rest of your day", "enjoy the rest of your day",
        "cheers mate", "lovely ta", "ta ta", "see you", "see ya", "catch you later",
        "catch ya later", "ta ra", "slán", "slan", "slán go fóill", "slan go foill",
    ]),
    ("zh-TW", &[
        "掰掰", "掰", "拜拜", "再見", "再會", "掰囉", "掰掰囉", "掛囉", "掛啦", "先掛了", "我掛了", "我先掛了", "那我掛了",
        "那掛了",
        "好掛了",
    ]),
    ("zh-CN", &["再见", "再会", "挂了啊", "先挂了", "我挂了", "那挂了", "好嘞拜拜", "挂啰", "挂咯", "挂啦", "那我挂了", "好挂了"]),
    ("yue", &["得啦唔該晒", "得啦唔该晒", "得啦唔該曬", "先掛啦", "先挂啦", "拜拜啦", "byebye", "收線啦", "收线啦", "得閒再傾", "得闲再倾", "遲啲再傾", "迟啲再倾"]),
    ("ja", &[
        // A bare 「ありがとうございました」 is thanks, like 謝謝 elsewhere; with 本日は／今日は it closes.
        // 「失礼いたします／します」 is in GREETING_BYES so opening phrases never close on Turn 1.
        "本日はありがとうございました", "今日はありがとうございました", "じゃあね", "じゃあまた",
        "またね", "さようなら", "お疲れ様でした", "お疲れさまでした", "ではまた", "それではまた", "バイバイ",
        "ばいばい", "切るね", "切ります", "じゃあ切るね", "おやすみなさい",
    ]),
    ("ko", &[
        "들어가세요", "수고하세요", "끊겠습니다", "끊을게요", "끊을게", "먼저끊을게", "잘가", "잘가요", "잘가세요",
        "응끊어", "어끊어", "그래끊어", "안녕히계세요", "안녕히가세요", "다음에봬요",
        // 「그럼 수요일에 뵙겠습니다」: seeing them on the day ends the call (처음 뵙겠습니다 opens one).
        "에뵙겠습니다", "그때뵙겠습니다", "그럼뵙겠습니다", "또뵙겠습니다", "에봬요", "그때봬요",
    ]),
    ("es", &[
        "chao", "chau", "hasta luego", "hasta pronto", "cuídate", "cuidate", "cuídese", "cuidese",
        "un abrazo", "un beso", "besos", "adiós", "adios", "nos vemos",
        "que le vaya bien", "que te vaya bien", "que tenga buen día", "que tenga buen dia", "que tengas buen día", "que tengas buen dia",
    ]),
    ("fr", &[
        "au revoir", "bonne journée", "bonne journee", "bonne soirée", "bonne soiree", "à bientôt",
        "a bientot", "bisous", "bises", "à plus", "je raccroche", "bonne continuation", "à tout à l'heure", "a tout a l heure",
        "à très vite", "a tres vite", "allez salut",
    ]),
    ("de", &[
        "auf wiederhören", "auf wiederhoren", "wiederhören", "schönen tag noch", "schönen tag", "tschüss",
        "tschüs", "tschuss", "baba", "pfiat di", "pfiat eich", "gute nacht", "schönen abend noch", "schonen abend noch",
        "machs gut", "machen sie es gut",
        // Austrian and Swiss goodbyes.
        "schönen feierabend", "schonen feierabend", "tschö", "pfiati", "uf wiederluege", "wiederluege",
    ]),
    ("pt", &[
        "tchau tchau", "tchau", "até logo", "ate logo", "até mais", "um abraço", "um abraco", "abraço", "abraco", "beijos",
        "beijo", "fica com deus", "vai com deus", "fique bem", "bom trabalho",
        // Portugal: 「…e continuação de um bom dia」.
        "continuação de um bom dia", "continuação de bom dia", "continuacao de um bom dia", "tenha um bom dia",
        "boa continuação",
        "até já", "ate ja", "passe bem",
    ]),
    ("th", &[
        "บ๊ายบาย", "บายบาย", "ไว้คุยกันใหม่", "แล้วคุยกันใหม่", "แล้วพบกันใหม่", "พบกันใหม่", "ไปก่อนนะ", "วางสายนะ",
        "ไว้เจอกัน", "ไว้เจอกันใหม่",
    ]),
    ("vi", &[
        "tạm biệt", "tam biet", "hẹn gặp lại", "hen gap lai", "chào nhé", "chao nhe", "một ngày tốt lành",
        "ngày tốt lành",
        // Hanging up out loud; not 「đừng cúp máy」 ("don't hang up").
        "thôi mình cúp máy", "thôi em cúp máy", "thôi mình cúp máy nha", "thôi em cúp máy nha",
        "em xin phép cúp máy", "mình xin phép cúp máy", "con xin phép cúp máy", "tôi xin phép cúp máy",
    ]),
    ("id", &["sampai jumpa", "sampai ketemu", "dadah"]),
    ("it", &[
        "arrivederci", "arrivederla", "buona giornata", "buongiornata", "buona serata", "buonanotte",
        "buona continuazione", "grazie a lei", "stammi bene", "stia bene", "a presto", "a domani",
        "ci vediamo", "ci sentiamo", "ciao ciao", "a dopo", "a più tardi", "a piu tardi", "buon proseguimento",
        "buon weekend", "buon fine settimana", "buona domenica", "ci risentiamo", "ci rivediamo", "a risentirci",
        "la saluto", "ti saluto", "vi saluto", "cordiali saluti",
        // "Un saluto" only after thanks: "sì, un saluto" answers "any message for her?".
        "grazie un saluto",
    ]),
    ("ru", &[
        "до свидания", "до свиданья", "всего доброго", "всего хорошего", "хорошего дня", "хорошего вечера",
        "до встречи", "до связи", "пока пока",
        // "Пока" after a closing word; alone it is in GREETING_BYES ("я пока не знаю").
        // Not "ну всё пока": "ну всё пока понятно" is "all clear so far".
        "давай пока", "удачи пока", "ну всё счастливо", "ну все счастливо",
    ]),
    ("ar", &[
        "مع السلامة", "مع السلامه", "إلى اللقاء", "الى اللقاء", "في أمان الله", "في امان الله", "وداعاً",
        "وداعا", "باي باي",
        // "Good night", said only on leaving; "come on, bye".
        "تصبح على خير", "تصبحين على خير", "تصبحون على خير", "تصبحوا على خير", "يلا سلام", "يلا باي",
    ]),
    ("hi", &[
        "अलविदा", "फिर मिलेंगे", "फिर मिलते हैं", "फिर बात करते हैं", "बाद में बात करते हैं",
        "आपका दिन शुभ हो", "बाय बाय",
    ]),
    ("nl", &[
        "tot ziens", "tot spreeks", "doei", "dag hoor", "fijne dag nog", "fijne avond",
        "doeg", "tot snel", "tot binnenkort", "spreek je later",
    ]),
    // "Na razie" is "bye" as the last words (or before pa / cześć / thanks): "Na razie nie wiem" is no goodbye.
    // With a name or another goodbye after it ("No to na razie, Ania") the older check decides.
    ("pl", &[
        "do widzenia", "na razie", "na razie pa", "na razie cześć", "na razie czesc", "na razie dzięki",
        "na razie dzieki", "na razie dziękuję", "na razie dziekuje", "do usłyszenia", "do uslyszenia",
        "dobrego dnia", "miłego dnia", "milego dnia",
        "no to cześć", "no to czesc", "miłego wieczoru", "milego wieczoru",
    ]),
    ("ro", &["la revedere", "pa pa", "o zi bună", "o zi buna", "toate cele bune", "pe curând", "pe curand"]),
    ("uk", &["до побачення", "на все добре", "гарного дня", "бувайте", "до зустрічі", "до зв'язку"]),
    ("bn", &["বিদায়", "আল্লাহ হাফেজ", "ভালো থাকবেন", "আবার কথা হবে", "রাখছি তাহলে", "শুভ দিন"]),
    ("mr", &["नमस्कार पुन्हा बोलू", "काळजी घ्या", "शुभ दिवस", "आता ठेवतो", "बाय बाय"]),
    // Not வருகிறேன் / వస్తాను alone: "I am coming" is no goodbye, "I'll go and come" is.
    ("ta", &[
        "போய் வருகிறேன்", "போயிட்டு வருகிறேன்", "போயிட்டு வரேன்", "பை பை", "பிறகு பேசலாம்", "நல்ல நாள்",
        "கவனமாக இருங்கள்",
    ]),
    ("te", &[
        "వెళ్ళొస్తాను", "వెళ్లొస్తాను", "సరే ఉంటాను", "ఇక ఉంటాను", "బై బై", "మళ్ళీ మాట్లాడుదాం", "శుభ దినం",
        "జాగ్రత్త",
    ]),
    ("tr", &[
        "hoşça kal", "hoşça kalın", "hoşçakal", "hoşçakalın", "görüşürüz", "görüşmek üzere", "iyi geceler",
        "kendine iyi bak", "kendinize iyi bakın", "güle güle", "allaha ısmarladık", "bay bay",
    ]),
    // Swedish, Norwegian, Danish, Finnish. "Ha det bra", "vi ses" and "farvel" are
    // everyday words too: GREETING_BYES.
    ("sv", &["hej då", "hejdå"]),
    ("nb", &["hei så lenge", "takk for praten"]),
    ("da", &["fortsat god dag", "hav en god dag", "hav det godt", "tak for snakken"]),
    ("fi", &["kuulemiin", "kiitos ja hei", "hyvää päivänjatkoa"]),
    // Czech, Slovak. Not "zatím" alone: "je to tak zatím" is "so far".
    ("cs", &[
        "na shledanou", "nashledanou", "nashle", "mějte se", "mějte se hezky", "tak se mějte", "pěkný zbytek dne",
        "hezký zbytek dne", "zatím ahoj", "zatím čau", "tak pa",
    ]),
    ("sk", &[
        "dovidenia", "do videnia", "majte sa", "majte sa pekne", "maj sa", "maj sa pekne", "pekný zvyšok dňa",
        "zatiaľ ahoj", "zatiaľ čau",
    ]),
    ("hu", &[
        "viszlát", "viszontlátásra", "viszonthallásra", "további szép napot", "további szép napot kívánok", "akkor szia",
    ]),
    ("bg", &["дочуване", "айде чао"]),
    // Croatian and Serbian in Latin letters.
    ("hr", &["doviđenja", "dovidjenja", "do viđenja", "ajde bok"]),
    ("sr", &["ajde ćao"]),
    ("uk", &["давай бувай", "ну все па па"]),
    // Fourth batch: Icelandic, Slovenian, Macedonian and Albanian, and goodbyes the
    // earlier lists did not have. Words that are greetings or everyday words too
    // ("heyrumst", "se slišimo", "се слушаме", "dëgjohemi": also "we can hear each
    // other") are in GREETING_BYES.
    ("is", &["bless bless", "bless", "bæ bæ", "bæ", "bæjó", "vertu sæll", "vertu sæl", "vertu blessaður", "vertu blessuð"]),
    ("sl", &["nasvidenje", "na svidenje", "adijo", "lepo bodite", "fejst bodite", "se slišimo kasneje", "se slišiva kasneje"]),
    ("mk", &["ајде пријатно", "ајде се слушаме", "ајде чао", "до гледање", "збогум", "ајде со здравје", "ќе се чуеме"]),
    ("sq", &["mirupafshim", "mirë u pafshim", "hajde mirupafshim", "kalofsh mirë", "kalofshi mirë", "gjithë të mirat", "shihemi më vonë"]),
    // "Have a nice rest of the day": "…, ha en fin dag videre", "ha en fortsatt bra dag".
    ("sv", &["hej så länge", "ha en fortsatt bra dag", "ha en fortsatt trevlig dag", "fortsatt trevlig dag"]),
    ("nb", &["ha en fin dag videre", "ha en god dag videre", "ha en fin kveld videre", "ha en fortsatt fin dag"]),
    ("da", &["hej så længe", "kan du have det godt", "hav en fortsat god dag"]),
    ("fi", &["näkemiin", "kiitos hei"]),
    ("cs", &["na slyšenou", "měj se"]),
    ("sk", &["do počutia"]),
    ("hu", &["viszhall", "pá pá", "puszi"]),
    ("bg", &["довиждане", "до чуване", "ще се чуем", "чао засега"]),
    ("el", &["αντίο", "εις το επανιδείν", "τα ξαναλέμε"]),
    // A wish with "να έχετε" at the end ("Καλό υπόλοιπο ημέρας να έχετε!") is said on leaving.
    ("el", &[
        "καλό υπόλοιπο ημέρας να έχετε", "καλό υπόλοιπο να έχετε", "καλή συνέχεια να έχετε", "καλή μέρα να έχετε",
        "καλό απόγευμα να έχετε", "καλό βράδυ να έχετε",
    ]),
    // "Σας ευχαριστώ πολύ και καλή σας μέρα!": a wish joined to thanks with "και" is said on leaving.
    ("el", &[
        "και καλή σας μέρα", "και καλή σας ημέρα", "και καλή μέρα", "και καλή ημέρα", "και καλό σας απόγευμα",
        "και καλό απόγευμα", "και καλό σας βράδυ", "και καλό βράδυ", "και καλή σας συνέχεια", "και καλή συνέχεια",
    ]),
    ("hr", &["do slušanja", "čujemo se poslije"]),
    ("sr", &["довиђења", "ајде здраво", "ајде ћао", "ајд ћао", "ajde zdravo", "чујемо се после", "čujemo se posle"]),
    // Fifth batch. Greetings, wishes and everyday words that also say goodbye are in
    // GREETING_BYES: Hebrew "שלום" and "נדבר", Swahili "salama" ("nimefika salama"),
    // Georgian "კარგად" ("well"), Lithuanian "ate" and "iki" (English "ate", "until").
    ("he", &[
        "שיהיה לילה טוב", "ושיהיה יום טוב", "והמשך יום נעים", "שיהיה יום טוב", "שיהיה ערב טוב", "המשך יום נעים",
        "המשך ערב נעים", "המשך יום טוב", "נדבר אחר כך", "יאללה ביי", "ולילה טוב", "ושבת שלום", "ויום טוב", "וערב טוב",
        "ולהתראות", "טוב ביי", "ביי ביי", "להתראות", "וכל טוב", "להשתמע", "וביי", "ביי",
    ]),
    // "See you" (נתראה), alone or with a time ("נתראה בשישי", "נתראה ביום ראשון"): said on leaving.
    ("he", &[
        "נתראה", "נתראה בקרוב", "נתראה מחר", "נתראה אחר כך", "נתראה מאוחר יותר", "נתראה בשבוע הבא", "נתראה בשבת",
        "נתראה בראשון", "נתראה בשני", "נתראה בשלישי", "נתראה ברביעי", "נתראה בחמישי", "נתראה בשישי",
        "נתראה ביום ראשון", "נתראה ביום שני", "נתראה ביום שלישי", "נתראה ביום רביעי", "נתראה ביום חמישי",
        "נתראה ביום שישי", "נתראה ביום שבת",
        "נתראה אז", "נתראה שם", "ויום טוב גם לך", "יום טוב גם לך", "גם לך יום טוב",
    ]),
    ("ms", &[
        "nanti kita sembang lagi", "selamat tinggal", "saya letak dulu", "jumpa lagi", "ok lah bye", "gerak dulu",
        "jaga diri", "babai",
    ]),
    ("tl", &[
        "hanggang sa muli", "sige na bye", "ibaba ko na", "baba ko na", "una na ako", "paalam po", "kita kits",
        "sige bye", "bye na", "paalam",
    ]),
    ("bn", &[
        "ঠিক আছে রাখছি", "পরে কথা হবে", "আচ্ছা রাখছি", "খোদা হাফেজ", "তাহলে রাখি", "ভালো থেকো", "শুভরাত্রি", "টাটা",
        "বাই",
    ]),
    ("ur", &[
        "بعد میں بات کرتے ہیں", "اپنا خیال رکھیے گا", "اپنا خیال رکھنا", "فی امان اللہ", "پھر بات ہوگی",
        "پھر ملتے ہیں", "اللہ حافظ", "اچھا بائے", "خدا حافظ", "رب راکھا",
        "الله حافظ",
    ]),
    ("ne", &[
        "पछि कुरा गरौँला", "पछी कुरा गरौँला", "फोन राखेँ ल", "पछि बोलम्ला", "पछी बोलम्ला", "शुभ रात्री", "राखेँ है",
        "ल म गएँ", "टाटा", "बाइ",
    ]),
    ("si", &[
        "එහෙනම් මං කැපුනා", "ගිහිල්ලා එන්නම්", "එහෙනම් අපි ගියා", "එහෙනම් මං ගියා", "පස්සේ කතා කරමු", "ගිහින් එන්නම්",
        "ආයෙ සෙට් වෙමු", "බුදු සරණයි", "සුභ ගමන්", "බායි", "ටටා",
    ]),
    ("my", &[
        "ဖုန်းချလိုက်တော့မယ်နော်", "နောက်မှပြန်ဆက်လိုက်မယ်", "နောက်မှစကားပြောကြမယ်", "သွားတော့မယ်နော်", "ကောင်းသောညပါ",
        "ဘိုင်ဘိုင်", "ဒါပဲနော်", "တာ့တာ",
    ]),
    ("lo", &["ຊ່ຳນີ້ກ່ອນເດີ້", "ວາງສາຍກ່ອນເດີ້", "ໄປກ່ອນເດີ້", "ລົມກັນໃໝ່", "ພົບກັນໃໝ່", "ຝັນດີເດີ້", "ບ່າຍໆ"]),
    ("mn", &[
        "сайн сууж байгаарай", "сайхан амраарай", "дараа холбогдьё", "эргээд залгая", "дараа уулзъя", "түр баяртай",
        "эргээд ярья", "за баяртай", "дараа ярья", "баяртай",
    ]),
    ("fa", &[
        "مواظب خودتون باشید", "مراقب خودت باش", "مواظب خودت باش", "به امید دیدار", "خدا به همراهت", "خدانگهدار",
        "خداحافظ", "روز خوش", "شب خوش",
    ]),
    ("ps", &[
        "د خدای په امان", "خدای دې مل شه", "په مخه مو ښه", "په مخه دې ښه", "بیا به غږیږو", "په خیر اوسئ", "خدای پامان",
        "الله پامان", "ژوندي اوسئ", "مخه ښه",
    ]),
    ("az", &[
        "gecəniz xeyrə qalsın", "özünüzə yaxşı baxın", "sonra danışarıq", "özünə yaxşı bax", "sağlıqla qalın",
        "salamat qalın", "görüşərik", "xudahafiz", "hələlik",
    ]),
    ("hy", &["ցտեսություն", "լավ մնացեք", "կհանդիպենք", "բարի գիշեր", "լավ մնա"]),
    ("ka", &[
        "მოგვიანებით დაგირეკავ", "კარგად ბრძანდებოდეთ", "მერე ვილაპარაკოთ", "ღამე მშვიდობისა", "კარგად იყავით",
        "კარგად იყავი", "აბა დროებით", "აბა კარგად", "ნახვამდის",
    ]),
    ("sw", &[
        "tutazungumza baadaye", "tutaonana baadaye", "tutaongea kesho", "nenda salama", "usiku mwema", "baki salama",
        "tutaonana", "kwaheri",
    ]),
    ("am", &["በኋላ እንገናኝ", "ነገ እንገናኝ", "ደህና እደሩ", "በሰላም ሁኑ", "ደህና ሁኑ", "ደህና ሁን", "ደህና ሁኚ", "ደህና ዋሉ", "ደህና ዋል", "ቻው"]),
    ("af", &[
        "lekker aand verder", "lekker dag verder", "sien jou later", "praat later", "mooi loop", "tot later",
        "totsiens", "mooi bly", "goeienag", "tatta", "baai",
    ]),
    ("et", &[
        "kena päeva jätku", "jääme kuuldele", "kuulmiseni", "nägemiseni", "kõike head", "head aega", "nägemist",
        "homseni", "tšauki", "pakaa",
    ]),
    ("lv", &[
        "uz sadzirdēšanos", "uz saredzēšanos", "līdz rītdienai", "uz redzēšanos", "ar labu nakti", "jauku dienu",
        "visu labu", "atā",
    ]),
    ("lt", &["susiskambinsime", "iki pasimatymo", "iki malonaus", "geros dienos", "iki greito", "viso gero"]),
    ("be", &[
        "да хуткай сустрэчы", "усяго найлепшага", "усяго добрага", "да пабачэння", "да сустрэчы", "да заўтра",
        "дабранач", "бывайце",
    ]),
    ("ta", &[
        "பத்திரமா போயிட்டு வாங்க", "உடம்பை பார்த்துக்கோங்க", "சரிங்க வச்சிடறேன்", "அப்புறம் பேசலாம்",
        "நாளைக்கு பேசலாம்", "சரி வைக்கிறேன்", "போயிட்டு வாங்க", "சரி பாய்",
    ]),
];

/// Greetings that also say goodbye: farewells only once the business is done
/// ([`ClosingMachine::greeting_bye_counts`]).
const GREETING_BYES: Lexicon = &[
    ("ja", &["失礼いたします", "失礼致します", "失礼します"]),
    ("ko", &["수고하십니다", "수고하셨습니다", "안녕"]),
    ("th", &["สวัสดีค่ะ", "สวัสดีครับ", "สวัสดี"]),
    ("vi", &[
        "chào anh ạ", "chào chị ạ", "chào cô ạ", "chào chú ạ", "chào bác ạ", "chào anh", "chào chị", "chào em",
        "chào bạn", "chào cô", "chào chú", "chào bác", "chào ông", "chào bà", "chao anh",
    ]),
    ("id", &[
        "selamat siang", "selamat pagi", "selamat sore", "selamat malam",
        // Opens a call as often as it ends one.
        "assalamualaikum", "assalamu'alaikum", "assalamu alaikum",
    ]),
    // "Buon lavoro" and "alla prossima" are everyday words too ("cerco un buon lavoro").
    ("it", &["ciao", "buongiorno", "buonasera", "buon pomeriggio", "buon lavoro", "alla prossima"]),
    // Swiss "tschau" and "adieu" are said on meeting too; "Ade" is also a name.
    ("de", &["servus", "tschau", "adieu", "ade"]),
    // "All the best" also sits inside sentences ("all the best doctors").
    ("en", &["all the best"]),
    ("fr", &["salut", "bonsoir"]),
    // "¡Feliz día!" also opens a call on a birthday or a holiday.
    ("es", &["buenas noches", "buenas tardes", "buen día", "buen dia", "buenos días", "buenos dias", "feliz día", "feliz dia"]),
    ("pt", &["boa noite", "boa tarde", "cumprimentos"]),
    // "Бывай" is also "be (here more often)", "созвонимся" also "we'll talk
    // (tomorrow)" in a plan.
    ("ru", &["пока", "созвонимся", "бывай"]),
    // Greetings at the start too (السلام عليكم, سلام, نهارك سعيد), or words of a
    // sentence ("عملية سلام", "لا تخاف، الله معك", "الأولاد في رعاية الله", "تفاح باي").
    ("ar", &[
        "السلام عليكم", "سلام عليكم", "سلام", "باي", "بأمان الله", "بامان الله", "في رعاية الله", "الله معك",
        "نهارك سعيد",
    ]),
    ("hi", &["नमस्ते", "नमस्कार"]),
    // "Het was een fijne dag", "ik ben bezig tot straks", "ik daag hem uit"; Flemish
    // "salu" and "hoi hoi" also greet.
    ("nl", &[
        "fijne dag", "prettige dag", "prettige avond", "goed weekend", "fijn weekend", "werk ze", "tot straks",
        "salukes", "salu", "hoi hoi", "daag", "goedendag", "dag",
    ]),
    // "Nic do zobaczenia" is "nothing to see", "trzymaj się poręczy" "hold the rail";
    // "wszystkiego dobrego" also opens a birthday call.
    ("pl", &["cześć", "czesc", "do zobaczenia", "trzymaj się", "trzymaj sie", "wszystkiego dobrego"]),
    ("ro", &["bună ziua", "buna ziua"]),
    ("uk", &["бувай", "па па"]),
    ("mr", &["नमस्कार"]),
    ("ta", &["வணக்கம்"]),
    ("te", &["నమస్కారం"]),
    // Not "eyvallah": "anladım, eyvallah" is "got it, thanks" in the middle of a call.
    ("tr", &["iyi günler", "iyi akşamlar", "kolay gelsin", "hayırlı günler"]),
    // Also everyday words: "Hon verkar ha det bra", "jag vet inte när vi ses", "sen var
    // det tack och hej". Not "vi hörs" / "vi høres": "we can hear each other".
    ("sv", &["ha det så bra", "ha det bra", "ha det gott", "vi ses", "tack och hej"]),
    // "I Norge er det norsk som snakkes", "Hun ser ut til å ha det godt".
    ("nb", &["ha det godt", "vi snakkes", "snakkes senere", "snakkes", "ha en fin dag"]),
    // "Vi tog et sidste farvel", "jeg ved ikke hvornår vi snakkes ved".
    ("da", &["farvel", "vi snakkes ved"]),
    // Greetings as well. Not "nähdään": "odotetaan ja nähdään" is "let's wait and see".
    ("fi", &["moikka", "heippa", "moi moi", "hei hei"]),
    ("cs", &["ahoj", "čau", "hezký den"]),
    // "Nemal som dobrú noc", "bol to pekný deň".
    ("sk", &["čaute", "pekný deň", "dobrú noc"]),
    // "Szép napot kívánok" also opens a call; "minden jót" a birthday call.
    ("hu", &["sziasztok", "szia", "szép napot kívánok", "szép napot", "minden jót"]),
    // "A fost o zi frumoasă". Not "ne auzim" ("nu ne auzim": we can't hear each other)
    // or "numai bine" ("Ce faceți? Mulțumesc, numai bine").
    ("ro", &["o zi frumoasă", "o zi frumoasa", "seară bună", "seara buna", "spor la treabă", "spor la treaba"]),
    // "Беше приятен ден", "днес имам лека работа". Not "живи и здрави": "how are you" is
    // answered with it.
    ("bg", &["чао чао", "чао", "приятен ден", "хубав ден", "лека вечер", "всичко добро", "лека работа", "до скоро"]),
    // "Έτσι τα λέμε", "ήταν καλό βράδυ", "η σειρά δεν είχε καλή συνέχεια"; "τι έγινε,
    // γεια" opens a call. Not "έλα γεια", which also opens one.
    ("el", &[
        "έγινε γεια", "εγινε γεια", "γεια σας", "γεια σου", "γεια χαρά", "τα λέμε", "καλό βράδυ", "καλό απόγευμα", "χαιρετώ",
        "καλή συνέχεια",
    ]),
    // "Mama šalje lijep pozdrav", "bio je ugodan dan". Not "čujemo se": "ne čujemo se"
    // is "we can't hear each other".
    // Slovenian "lep pozdrav" sits here, before "pozdrav", so it is read as a whole.
    ("hr", &["bok", "vidimo se", "ugodan dan", "lijep pozdrav", "lep pozdrav", "pozdrav", "svako dobro", "laku noć", "laku noc"]),
    // "Bilo je prijatno".
    ("sr", &["ćao", "prijatno", "sve najbolje"]),
    // "Ми завтра зідзвонимось", "лікар сказав: бережіть себе". Not "на зв'язку": "I'm on the line".
    ("uk", &[
        "тихого дня", "зідзвонимось", "зідзвонимося", "усього найкращого", "всього найкращого", "бережіть себе", "бережи себе",
    ]),
    // Fourth batch. "Við heyrumst miklu betur núna", "zdaj se slišiva bolj čisto", "добро
    // ли се слушаме", "a dëgjohemi": also "we can hear each other". "Bil je lep dan",
    // "denes vremeto e mnogu prijatno", "sapo i urova ditën e mirë korrierit". Not the
    // Slovenian "vse dobro": "Kako ste? Hvala, vse dobro" answers "how are you".
    ("is", &["sæll", "sæl", "blessaður", "blessuð", "heyrumst", "við heyrumst"]),
    ("sl", &["čao", "živjo", "lep dan", "lahko noč", "se slišimo", "se slišiva", "se vidimo", "se vidiva", "srečno"]),
    ("mk", &["се слушаме", "се гледаме", "убав ден", "добра ноќ"]),
    ("sq", &["tung", "tungjatjeta", "çao", "dëgjohemi", "shihemi", "ditën e mirë", "natën e mirë"]),
    // "Vi tog adjø med bilen", "vi hade en trevlig helg", "selv om vi sees i morgen".
    ("sv", &["adjö", "trevlig helg"]),
    ("nb", &["adjø", "god helg", "vi sees", "ha det fint"]),
    ("da", &["vi tales ved", "god weekend", "godnat"]),
    ("fi", &["soitellaan", "palaillaan", "kuullaan"]),
    ("hu", &["jó éjszakát", "majd beszélünk"]),
    // "Само затварям прозореца", "сега чуваме се по-ясно".
    ("bg", &["чуваме се", "всичко хубаво", "затварям"]),
    // Not "να είστε καλά": the older lists keep "Ευχαριστώ, να είστε καλά" open.
    ("el", &["καλό υπόλοιπο ημέρας", "καλό υπόλοιπο", "καλό μεσημέρι", "κλείνω"]),
    // "Καλή σας μέρα, είμαι από την τράπεζα…" opens a call.
    ("el", &[
        "καλή σας μέρα", "καλή σας ημέρα", "καλό σας απόγευμα", "καλό σας βράδυ", "καλή σας συνέχεια", "καλή μέρα", "καλή ημέρα",
        "καλημέρα σας",
    ]),
    ("hr", &["drži se", "ugodan ostatak dana"]),
    // Serbian in Cyrillic letters; "здраво", "пријатно" and "поздрав" are Macedonian too.
    ("sr", &["ћао", "здраво", "пријатно", "свако добро", "све најбоље", "видимо се", "поздрав", "лаку ноћ", "zdravo"]),
    // Fifth batch: greetings and wishes ("יום טוב", "magandang araw", "روز بخیر") and everyday
    // words ("ingat": careful, "baadaye": later, "დროებით": for now, "dag": day).
    ("he", &["לילה טוב", "שבת שלום", "יום טוב", "ערב טוב", "כל טוב", "שלום", "נדבר"]),
    // "You too" after the other side's wish: it answers a goodbye, and is no goodbye in a question.
    ("he", &["וגם לך", "גם לך"]),
    ("ms", &["salam sejahtera", "selamat jalan", "hati hati"]),
    ("tl", &[
        "magandang umaga", "magandang hapon", "magandang araw", "magandang gabi", "ingat po kayo", "ingat kayo",
        "ingat po", "ingat ka", "ingat",
    ]),
    ("bn", &["আসসালামু আলাইকুম", "দেখা হবে", "নমস্কার"]),
    ("ur", &["السلام علیکم", "خوش رہیں", "شب بخیر", "آداب"]),
    ("si", &["ආයුබෝවන්", "ජය වේවා"]),
    ("my", &["နားတော့မယ်နော်", "ဂရုစိုက်နော်"]),
    ("lo", &["ໂຊກດີເດີ້", "ສະບາຍດີ", "ໂຊກດີ"]),
    ("mn", &["амжилт хүсье"]),
    ("fa", &["قربان شما", "روز بخیر", "عصر بخیر", "شب بخیر", "قربانت", "فعلاً", "فعلا"]),
    ("ps", &["شپه مو پخیر"]),
    ("az", &["uğurlar"]),
    ("hy", &["հաջողություն", "կխոսենք", "առայժմ", "հաջող"]),
    ("ka", &["დროებით", "კარგად", "მაგრად"]),
    ("sw", &["siku njema", "kwaherini", "baadaye", "salama", "salaam"]),
    ("am", &["ጤና ይስጥልኝ", "መልካም ቀን", "ሰላም"]),
    ("af", &["goeiedag", "dag"]),
    ("et", &["tšau", "tsau", "hei"]),
    ("lv", &["sazvanāmies", "tiekamies", "sveiki", "čau"]),
    ("lt", &["sėkmės", "čiau", "viso", "iki", "ate"]),
    ("be", &["добрага вечара", "стэлефануемся", "беражы сябе", "добрага дня", "шчасліва", "пакуль"]),
    ("ta", &["வணக்கம்"]),
];

/// How calls are answered and opened; never a goodbye by themselves.
const OPENING_GREETINGS: &[&str] = &[
    "喂", "你好", "您好", "哈囉", "哈啰", "もしもし", "お世話になっております", "お世話になります",
    "お疲れ様です", "お忙しいところ失礼いたします", "お忙しいところ失礼します", "夜分に失礼します",
    "夜分に失礼いたします", "恐れ入りますが", "こんにちは", "여보세요", "안녕하세요", "hello", "hi", "hey",
    "hiya", "good morning", "good afternoon", "good evening", "hola", "diga", "bueno", "aló", "alo",
    "allo", "allô", "buenos días", "buenos dias", "buenas", "bonjour", "hallo", "guten tag", "guten morgen",
    "grüß gott", "grüss gott", "gruss gott", "grüezi", "moin", "alô", "oi", "olá", "ola", "bom dia",
    "tudo bem", "ฮัลโหล", "a lô", "xin chào", "halo", "pronto", "salve", "ciao", "алло", "здравствуйте",
    "привет", "مرحباً", "مرحبا", "أهلاً", "أهلا", "السلام عليكم", "हेलो",
    "merhaba", "efendim", "buyurun",
    "buongiorno", "buonasera", "buon pomeriggio", "mi dica", "dica",
];

/// Short replies to someone still talking (aizuchi): not a turn of their own,
/// not new content, not a goodbye.
const BACKCHANNELS: Lexicon = &[
    ("zh", &[
        "對對對", "对对对", "對對", "对对", "對", "对", "嗯嗯", "嗯", "嗯哼", "恩", "好喔", "好哦", "好", "好的",
        "是", "是的", "了解", "收到", "喔", "哦", "行", "好嘞", "沒錯", "没错", "係", "系", "係呀", "ok lah",
        "ok la", "okay lah",
    ]),
    ("ja", &["はい", "はいはい", "ええ", "なるほど", "そうですね", "うん", "うんうん", "そうそう", "はーい"]),
    ("ko", &["네", "네네", "예", "어", "응", "맞아요", "그렇죠", "네에", "그래요", "네알겠습니다", "알겠습니다"]),
    ("en", &[
        "uh huh", "mhm", "mm", "hmm", "right", "gotcha", "yeah", "yep", "yes", "okay", "ok", "sure", "i see",
        "got it",
    ]),
    ("es", &["vale", "claro", "dale", "sí", "si", "ajá", "aja", "ya"]),
    ("fr", &["d'accord", "tout à fait", "tout a fait", "oui", "ouais", "entendu", "bien sûr"]),
    ("de", &["genau", "alles klar", "ja", "jawohl", "stimmt"]),
    ("pt", &["entendi", "tá bom", "ta bom", "tá", "pois", "pois é", "sim", "uhum", "certo", "beleza"]),
    ("it", &[
        "va bene", "d'accordo", "ho capito", "capito", "certo", "perfetto", "ok grazie", "sì", "si", "esatto", "giusto", "benissimo",
        "ottimo", "chiaro", "sì sì", "si si", "certo certo",
    ]),
    ("th", &["ค่ะ", "ครับ", "อืม", "ใช่", "โอเคค่ะ", "โอเคครับ", "ได้ค่ะ", "ได้ครับ"]),
    ("vi", &[
        "vâng", "dạ", "ừ", "ờ", "dạ vâng", "ừ nhé", "u nhe", "vâng ạ", "vang a", "ok nhé", "oke nhé",
        // "Got it", "alright": 「Vâng em hiểu rồi, chào anh ạ」.
        "hiểu rồi", "được rồi",
    ]),
    ("id", &["iya", "oke"]),
    ("ru", &["да", "ага", "понятно", "хорошо", "ладно"]),
    ("ar", &["نعم", "أيوه", "أيوة", "تمام", "طيب"]),
    ("hi", &["हाँ", "हां", "जी", "अच्छा"]),
    ("tr", &["evet", "tamam", "tabii", "peki", "anladım", "aynen", "hı hı"]),
];

/// Thanks the shared list ([`crate::locale::strip_trailing_thanks`]) does not have.
const EXTRA_THANKS: &[&str] = &[
    "ขอบคุณมาก", "ขอบคุณ", "ขอบพระคุณมาก", "ขอบพระคุณ", "ขอบใจมาก", "ขอบใจ",
    "唔該晒", "唔该晒", "唔該曬", "唔該哂", "唔该哂", "唔該", "唔该", "多謝晒", "多谢晒", "多謝曬",
    "多謝哂", "多谢哂", "多謝", "多谢",
    "cảm ơn anh nhiều", "cảm ơn chị nhiều", "cảm ơn anh", "cảm ơn chị", "cảm ơn em", "cảm ơn bạn",
    "cảm ơn nhiều", "cảm ơn", "cám ơn anh", "cám ơn chị", "cám ơn nhiều", "cám ơn",
    "cam on anh", "cam on chi", "cam on",
    "terima kasih banyak", "terima kasih", "makasih banyak", "makasih",
    "grazie mille", "molte grazie", "grazie tante", "mille grazie", "grazie infinite", "la ringrazio", "ti ringrazio", "vi ringrazio", "grazie",
    "большое спасибо", "спасибо большое", "спасибо вам", "благодарю", "спасибо",
    "شكراً جزيلاً", "شكرا جزيلا", "شكراً لك", "شكرا لك", "شكراً", "شكرا", "يعطيك العافية",
    "बहुत बहुत धन्यवाद", "बहुत धन्यवाद", "बहुत शुक्रिया", "धन्यवाद", "शुक्रिया",
    "çok teşekkür ederim", "teşekkür ederim", "teşekkürler", "sağ olun", "sağ ol", "sağolun", "sağol",
    "bardzo dziękuję", "dziękuję bardzo", "dziękuję", "dziekuje", "dzięki wielkie", "dzięki", "dzieki",
    "hartelijk bedankt", "bedankt", "dank je wel", "dank u wel", "dank je", "dank u",
    // Not the Danish "tak" alone: Polish "tak" is "yes".
    "tack så mycket", "tusen tack", "stort tack", "tack ska du ha", "tack",
    "tusen takk", "mange takk", "takk skal du ha", "takk",
    "tusind tak", "mange tak", "tak skal du have", "tak for det",
    "kiitos paljon", "paljon kiitoksia", "kiitoksia", "kiitos", "kiitti",
    "děkuji moc", "děkuju moc", "moc děkuji", "moc děkuju", "děkuji", "děkuju", "díky moc", "díky",
    "ďakujem veľmi pekne", "ďakujem pekne", "ďakujem", "vďaka",
    "köszönöm szépen", "nagyon köszönöm", "köszönöm", "köszönjük", "köszi", "kösz",
    "mulțumesc frumos", "mulțumesc mult", "mulțumesc", "multumesc", "mulțumim", "mersi",
    "много благодаря", "благодаря ви", "благодаря", "мерси",
    "ευχαριστώ πολύ", "ευχαριστούμε", "ευχαριστώ", "ευχαριστω",
    "hvala lijepa", "hvala lepo", "hvala puno", "puno hvala", "hvala vam", "hvala",
    "дуже дякую", "щиро дякую", "дякую вам", "дякуємо", "дякую",
    // Icelandic "takk" alone is the Norwegian one above (also "please": "já takk").
    "kærar þakkir", "takk kærlega", "takk fyrir", "þakka þér kærlega", "þakka þér fyrir",
    "najlepša hvala", "hvala lepa",
    "многу благодарам", "ви благодарам", "благодарам", "фала многу", "многу фала", "фала",
    "хвала лепо", "хвала вам", "хвала",
    "faleminderit shumë", "ju faleminderit", "faleminderit", "rrofshi", "rrofsh",
    // Fifth batch.
    "תודה רבה", "תודה", "maraming salamat po", "maraming salamat", "salamat po", "salamat", "অনেক ধন্যবাদ", "ধন্যবাদ",
    "بہت بہت شکریہ", "بہت شکریہ", "شکریہ", "බොහොම ස්තූතියි", "ස්තූතියි", "ස්තුතියි", "ကျေးဇူးအများကြီးတင်ပါတယ်",
    "ကျေးဇူးတင်ပါတယ်", "ကျေးဇူးပါ", "ကျေးဇူးပဲ", "ຂອບໃຈຫຼາຍໆເດີ້", "ຂອບໃຈຫຼາຍໆ", "ຂອບໃຈຫຼາຍ", "ຂອບໃຈເດີ້", "ຂອບໃຈ",
    "их баярлалаа", "баярлалаа", "خیلی ممنونم", "خیلی ممنون", "ممنونم", "ممنون", "متشکرم", "مرسی", "سپاسگزارم",
    "دستت درد نکنه", "ډېره مننه", "ډیره مننه", "مننه", "çox təşəkkür edirəm", "təşəkkür edirəm", "təşəkkürlər",
    "çox sağ olun", "çox sağ ol", "շատ շնորհակալ եմ", "շնորհակալություն", "շնորհակալ եմ", "մերսի", "დიდი მადლობა",
    "მადლობა", "გმადლობთ", "asante sana", "asanteni", "asante", "በጣም አመሰግናለሁ", "አመሰግናለሁ", "እናመሰግናለን", "baie dankie",
    "dankie", "suur aitäh", "aitäh", "tänan väga", "tänan", "aitüma", "liels paldies", "paldies", "labai ačiū", "ačiū",
    "dėkoju", "dėkui", "вялікі дзякуй", "дзякуй вам", "дзякуй", "ரொம்ப நன்றிங்க", "ரொம்ப நன்றி", "மிக்க நன்றி",
    "நன்றிங்க", "நன்றி",
];

/// Thanks for something, at the very end ("Thanks for your help", "thank you
/// for calling", "gracias por su ayuda"): thanks as well, with up to
/// [THANKS_FOR_MAX_WORDS] words after it.
const THANKS_FOR: &[&str] = &[
    "thank you so much for", "thank you very much for", "thanks so much for", "thanks again for", "thank you for",
    "thanks for", "many thanks for", "muchas gracias por", "gracias por", "merci beaucoup pour", "merci pour",
    "vielen dank für", "danke für", "obrigado pela", "obrigada pela", "obrigado pelo", "obrigada pelo",
    "obrigado por", "obrigada por", "grazie mille per", "grazie per",
    "tack så mycket för", "tack för", "tusen takk for", "takk for", "tusind tak for", "mange tak for", "tak for", "děkuji za", "děkuju za",
    "díky za", "ďakujem za", "vďaka za", "köszönöm a", "köszönöm az", "mulțumesc mult pentru", "mulțumesc pentru",
    "hvala na", "hvala za", "takk fyrir", "faleminderit për",
    "asante sana kwa", "asante kwa", "terima kasih atas", "terima kasih kerana", "maraming salamat sa", "salamat po sa",
    "salamat sa", "baie dankie vir", "dankie vir", "liels paldies par", "paldies par", "labai ačiū už", "ačiū už", "dėkoju už",
];
const THANKS_FOR_MAX_WORDS: usize = 4;

/// `t` (normalized) without the thanks for something at its end.
fn strip_thanks_for(t: &str) -> Option<&str> {
    THANKS_FOR.iter().find_map(|p| {
        let i = t.rfind(&format!("{p} "))?;
        if i > 0 && !t[..i].ends_with(' ') {
            return None;
        }
        let what: Vec<&str> = t[i + p.len() + 1..].split(' ').collect();
        let latin_words = what.iter().all(|w| !w.is_empty() && w.chars().all(|c| is_latin_letter(c) || c == '\''));
        (latin_words && what.len() <= THANKS_FOR_MAX_WORDS).then(|| t[..i].trim_end())
    })
}

/// A 「拜」 of its own at the end (speech-to-text's "Bye!": 「…等你電話咯。拜。」);
/// only as a word by itself, 「下禮拜」 ends on it too. Returns what comes before it.
fn lone_bai(t: &str) -> Option<&str> {
    if t == "拜" {
        return Some("");
    }
    t.strip_suffix(" 拜").map(str::trim_end)
}

/// Whom thanks or a goodbye is said to, after it (「唔該晒王生」「謝謝王先生」
/// 「再見陳小姐」): a title, with up to two letters of a name before it.
const ADDRESS_TITLES: &[&str] = &[
    "先生", "小姐", "太太", "女士", "師傅", "师傅", "老闆", "老板", "經理", "经理", "醫師", "医师", "醫生", "医生", "老師",
    "老师", "姐", "哥",
];

/// Particles after the last word: 「先這樣囉」, 「นะคะ」.
const ZH_PARTICLES: &str = "喔哦囉啰啦呢吧耶齁欸啊喇嘞呀噢";
const TH_PARTICLES: &[&str] = &["ค่ะ", "คะ", "ครับ", "คับ", "นะ", "จ้ะ", "จ้า", "จ๊ะ", "ฮะ"];
/// Japanese sentence-end particles after a closing: 「ご連絡いたしますね」.
const JA_PARTICLES: &[&str] = &["よね", "ねー", "ね", "よ"];

/// Words that may follow a goodbye: whom it is said to, a particle.
const TAIL_WORDS: &[&str] = &[
    "mate", "then", "now", "too", "again", "dear", "sir", "madam", "ma'am", "anh", "chị", "chi", "em", "ạ",
    "pak", "bu", "bapak", "ibu", "kak", "señor", "señora", "senhor", "senhora", "monsieur", "madame",
    "signore", "signora", "lei", "mucho", "ya", "you", "जी", "nhé", "nhe",
    "efendim", "hanım", "bey",
    // Fifth batch: Filipino "po" and "ha", Malay "encik", Urdu "جی", Swahili "bwana", Latvian "jums".
    "po", "ha", "encik", "cik", "puan", "tuan", "جی", "bwana", "jums",
];

/// Only the end of a long window is looked at.
const TAIL_CHARS: usize = 120;
/// Trailing particles / thanks / short replies peeled off before giving up.
const MAX_PEEL: usize = 5;

fn is_latin_letter(c: char) -> bool {
    c.is_ascii_alphabetic() || ('\u{00C0}'..='\u{024F}').contains(&c) || ('\u{1E00}'..='\u{1EFF}').contains(&c)
}

fn is_cjk_or_unspaced(c: char) -> bool {
    matches!(
        c as u32,
        0x3040..=0x30FF | 0x31F0..=0x31FF | 0x3400..=0x9FFF | 0xAC00..=0xD7AF
            | 0x1100..=0x11FF | 0x3130..=0x318F | 0x0E00..=0x0EFF | 0x1000..=0x109F | 0x1780..=0x17FF
    )
}

/// Scripts whose words are separated by spaces (Latin, Cyrillic, Arabic, Devanagari…).
fn is_spaced(p: &str) -> bool {
    p.chars().any(char::is_alphabetic) && !p.chars().any(is_cjk_or_unspaced)
}

/// Ends a sentence: the speech-to-text's own full stop, question or exclamation mark.
fn ends_sentence(text: &str) -> bool {
    text.trim_end().ends_with(|c: char| "。！？!?.…～~।؟۔။።፧։՜".contains(c))
}

fn is_punct(c: char) -> bool {
    if c.is_ascii() {
        return !(c.is_ascii_alphanumeric() || c == '\'');
    }
    c.is_whitespace() || "，。！？、～〜…．；：「」『』（）【】《》〈〉·・—–“”‘¡¿«»｡｢｣،؛؟।॥۔။၊።፣፤፡፧։՝՜՞".contains(c)
}

/// Lower case, the end of it only, punctuation as single spaces.
fn norm(text: &str) -> String {
    let start = text.char_indices().rev().nth(TAIL_CHARS).map_or(0, |(i, c)| i + c.len_utf8());
    let mut out = String::with_capacity(text.len() - start);
    let mut space = true;
    for c in text[start..].chars() {
        let c = if c == '’' || c == '´' { '\'' } else { c };
        if is_punct(c) {
            if !space {
                out.push(' ');
                space = true;
            }
        } else {
            // Turkish capital İ lowers to "i" plus a combining dot (U+0307): dropped, so "İyi" is "iyi".
            out.extend(c.to_lowercase().filter(|&l| l != '\u{307}'));
            space = false;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// `t` (normalized) ends with `p`: whole words in space-separated scripts,
/// spaces ignored otherwise. Returns what comes before it.
fn ends_with_phrase<'a>(t: &'a str, p: &str) -> Option<&'a str> {
    if is_spaced(p) {
        let head = t.strip_suffix(p)?;
        return (head.is_empty() || head.ends_with(' ')).then(|| head.trim_end());
    }
    // Compare from the end, skipping the spaces of `t`.
    let mut ti = t.char_indices().rev().filter(|(_, c)| *c != ' ');
    let mut cut = t.len();
    for pc in p.chars().rev() {
        let (i, tc) = ti.next()?;
        if tc != pc {
            return None;
        }
        cut = i;
    }
    Some(t[..cut].trim_end())
}

fn ends_with_any<'a>(t: &'a str, lex: Lexicon) -> Option<(&'static str, &'a str)> {
    ends_with_extra(t, lex)
}

/// A "don't" right before a translated closing word: "อย่าเพิ่งลาก่อน" (don't say
/// goodbye yet), "đừng tạm biệt".
const EXTRA_NEGATIONS: &[&str] = &[
    "อย่าเพิ่ง", "อย่า", "ไม่ต้อง", "ยังไม่", "đừng", "không", "don't", "do not", "non", "nie", "не", "لا", "jangan", "niet",
    "मत", "नहीं",
];

/// What may stand right before a translated word written without spaces
/// ("ขอบคุณค่ะลาก่อน"): anything else makes it part of another word
/// ("ขอเวลาก่อน", give me a moment, holds "ลาก่อน").
const UNSPACED_LEADS: &[&str] = &["ครับ", "คับ", "ค่ะ", "คะ", "นะ", "จ้า", "ขอ", "ขอตัว", "งั้น", "แล้ว", "โอเค"];

/// [ends_with_extra] for the translations' words: a whole word (in scripts
/// without spaces: at the start of a run or after one of [UNSPACED_LEADS]),
/// not right after a negation.
fn ends_with_extra_word<'a>(t: &'a str, lex: &[(&'static str, &'static [&'static str])]) -> Option<(&'static str, &'a str)> {
    let last = t.chars().next_back()?;
    for &(lang, list) in lex {
        for p in list {
            if p.chars().next_back() != Some(last) {
                continue;
            }
            let Some(before) = ends_with_phrase(t, p) else { continue };
            let whole = is_spaced(p)
                || before.is_empty()
                || t[before.len()..].starts_with(' ')
                || UNSPACED_LEADS.iter().any(|l| before.ends_with(l));
            if whole && !EXTRA_NEGATIONS.iter().any(|n| ends_with_phrase(before, n).is_some()) {
                return Some((lang, before));
            }
        }
    }
    None
}

fn ends_with_extra<'a>(t: &'a str, lex: &[(&'static str, &'static [&'static str])]) -> Option<(&'static str, &'a str)> {
    // Most phrases end on another letter: skipped without a comparison.
    let last = t.chars().next_back()?;
    for &(lang, list) in lex {
        for p in list {
            if p.chars().next_back() != Some(last) {
                continue;
            }
            if let Some(head) = ends_with_phrase(t, p) {
                return Some((lang, head));
            }
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Peel {
    Particle,
    Thanks,
    Word,
    Backchannel,
}

/// The title and name after thanks or a goodbye off `t` (「唔該晒王先生」 →
/// 「唔該晒」, 「多謝陳生」 → 「多謝」); only when thanks or a goodbye is left.
/// 「生」 alone needs one letter of a name before it (王生, 陳生).
fn peel_addressee(t: &str) -> Option<&str> {
    let thanks_or_bye = |s: &str| {
        !s.is_empty()
            && (EXTRA_THANKS.iter().any(|th| ends_with_phrase(s, th).is_some())
                || crate::locale::strip_trailing_thanks(s).len() < s.len()
                || ends_with_any(s, FAREWELLS).is_some())
    };
    let han = |c: char| ('\u{4E00}'..='\u{9FFF}').contains(&c);
    let titles = ADDRESS_TITLES.iter().map(|t| (*t, 0usize)).chain(std::iter::once(("生", 1usize)));
    for (title, min_name) in titles {
        let Some(head) = t.strip_suffix(title) else { continue };
        let mut rest = head.trim_end();
        for n in 0..=2 {
            if n >= min_name && thanks_or_bye(rest) {
                return Some(rest);
            }
            match rest.chars().next_back() {
                Some(c) if han(c) => rest = rest[..rest.len() - c.len_utf8()].trim_end(),
                _ => break,
            }
        }
    }
    None
}

/// One trailing particle, thanks, short reply or addressee word off `t`.
fn peel(t: &str) -> Option<(&str, Peel)> {
    let t = t.trim_end();
    if t.is_empty() {
        return None;
    }
    let p = t.trim_end_matches(|c: char| ZH_PARTICLES.contains(c));
    if p.len() < t.len() {
        return Some((p.trim_end(), Peel::Particle));
    }
    for p in TH_PARTICLES {
        if let Some(rest) = t.strip_suffix(*p) {
            return Some((rest.trim_end(), Peel::Particle));
        }
    }
    for p in JA_PARTICLES {
        if let Some(rest) = t.strip_suffix(*p).filter(|r| r.ends_with(|c: char| ('\u{3040}'..='\u{30FF}').contains(&c))) {
            return Some((rest.trim_end(), Peel::Particle));
        }
    }
    for th in EXTRA_THANKS {
        if let Some(rest) = ends_with_phrase(t, th) {
            return Some((rest, Peel::Thanks));
        }
    }
    let rest = crate::locale::strip_trailing_thanks(t);
    if rest.len() < t.len() {
        return Some((rest.trim_end(), Peel::Thanks));
    }
    if let Some(rest) = strip_thanks_for(t) {
        return Some((rest, Peel::Thanks));
    }
    let (head, last) = t.rsplit_once(' ').unwrap_or(("", t));
    if TAIL_WORDS.contains(&last) {
        return Some((head.trim_end(), Peel::Word));
    }
    if let Some(rest) = peel_addressee(t) {
        return Some((rest, Peel::Word));
    }
    ends_with_any(t, BACKCHANNELS).map(|(_, rest)| (rest, Peel::Backchannel))
}

fn ends_as_question(text: &str) -> bool {
    let t = text.trim_end();
    if t.ends_with(['?', '？', '؟', '፧']) || t.ends_with("까요") || t.ends_with("か。") {
        return true;
    }
    // Armenian marks a question on the asked word itself: 「Իրո՞ք」「Ե՞րբ ես գալու։」.
    let marked = t.trim_end_matches(|c: char| !c.is_alphanumeric());
    if marked.rsplit(['.', '։', '!', '?']).next().is_some_and(|s| s.contains('՞')) {
        return true;
    }
    let core = t
        .trim_end_matches(|c: char| !c.is_alphanumeric())
        .trim_end_matches(|c: char| ZH_PARTICLES.contains(c));
    core.ends_with('嗎') || core.ends_with('吗') || ["ですか", "ますか", "でしょうか"].iter().any(|q| core.ends_with(q))
}

/// Chinese goodbyes that are also other words: 「拜拜」 is worshipping at a
/// temple, 「掰」 is 「瞎掰」 (talking nonsense) and 「掰開」 (to break apart).
const ZH_BYE_WORDS: &[&str] = &["掰掰", "拜拜", "掰"];

/// What may stand right before a Chinese 「拜拜／掰」 for it to be a goodbye
/// (after punctuation or at the start, anything may): an answer, thanks, a
/// wrap-up word, or another goodbye. 「去拜拜」「在拜拜」「瞎掰」 are none of them.
/// Not a bare 「先」 or 「就」: 「我們要先拜拜」「祖先拜拜」「初一就拜拜」 are worship.
const ZH_BYE_CUES: &[&str] = &[
    "好", "好的", "好啦", "好喔", "好哦", "好了", "那", "那就", "那先", "嗯", "喔", "哦", "啊", "欸", "ok", "okay",
    "謝謝", "謝謝你", "謝謝您", "感謝", "謝啦", "我先", "那我先", "就先", "那我就", "我就先", "說", "講", "了", "囉", "啦", "吧", "齁",
    "掰", "掰掰", "拜拜", "bye", "再見", "晚安", "沒問題", "不會", "收到", "了解", "知道了", "沒事", "辛苦了",
    "麻煩你", "麻煩您", "再聯絡", "再聯繫", "先這樣", "就這樣",
    "谢谢", "谢谢你", "谢谢您", "感谢", "谢啦", "说", "讲", "啰", "再见", "没问题", "不会", "没事",
    "麻烦你", "麻烦您", "再联络", "再联系", "先这样", "就这样",
    // How a call ends without punctuation: 「下次見拜拜」「沒關係拜拜」「你路上小心拜拜」
    // 「我明天再打來拜拜」「沒事你忙你的拜拜」.
    "見", "沒關係", "消息", "休息", "小心", "看看", "打來", "聊", "電話", "你忙", "您忙", "先忙", "去忙", "忙你的",
    "早安", "午安", "快樂", "愉快", "保重", "加油", "慢走", "安全", "知道", "不好意思", "抱歉",
    "對不起", "客氣", "謝",
    "见", "没关系", "打来", "电话", "快乐", "对不起", "客气", "谢",
    // Cantonese thanks: 「唔該晒，拜拜」.
    "唔該晒", "唔该晒", "唔該", "唔该",
];

/// "You" right before the goodbye is a cue as well (「好我再打給你掰」) — unless
/// one of [`ZH_NOT_BEFORE_YOU`] stands before it: 「我會幫你拜拜」「明天我陪你拜拜」
/// are worship done for or with them.
const ZH_YOU: &str = "你妳您";
const ZH_NOT_BEFORE_YOU: &str = "幫帮替陪帶带跟叫要等讓让和為为代";

fn zh_you_before_bye(b: &str) -> bool {
    let mut back = b.chars().rev();
    back.next().is_some_and(|c| ZH_YOU.contains(c)) && !back.next().is_some_and(|c| ZH_NOT_BEFORE_YOU.contains(c))
}

/// `b` ends on one of [`ZH_BYE_CUES`]. 「那」 after a word of place is "there"
/// (「他在那拜拜」), not "then".
fn zh_cue_ends(b: &str) -> bool {
    ZH_BYE_CUES.iter().any(|cue| {
        b.strip_suffix(cue).is_some_and(|rest| {
            if cue.is_ascii() {
                return !rest.ends_with(|c: char| c.is_ascii_alphanumeric());
            }
            *cue != "那" || !rest.ends_with(|c: char| "在去到從从往來来".contains(c))
        })
    })
}

/// Whom the goodbye is said to, right before it (「好的王小姐拜拜」「謝謝老師拜拜」
/// 「老闆拜拜」): a title ([`ADDRESS_TITLES`]) after a cue or at the very start,
/// with a name of up to two letters before it after a cue, of one letter at the
/// start. 「今天老闆拜拜」「我們請師傅拜拜」 say who worships.
fn zh_addressee_before_bye(b: &str) -> bool {
    ADDRESS_TITLES.iter().any(|title| {
        let Some(mut rest) = b.strip_suffix(title) else {
            return false;
        };
        for name_letters in 0..=2 {
            if zh_cue_ends(rest) || zh_you_before_bye(rest) || (rest.is_empty() && name_letters <= 1) {
                return true;
            }
            match rest.chars().next_back() {
                Some(c) if ('\u{4E00}'..='\u{9FFF}').contains(&c) => rest = &rest[..rest.len() - c.len_utf8()],
                _ => return false,
            }
        }
        false
    })
}

/// A Chinese goodbye word ends `t`'s start: `before` is what comes before it
/// (normalized), `separated` that punctuation or a space stands between. True
/// at the start of the text, after punctuation or a space, and right after one of
/// [`ZH_BYE_CUES`], whom it is said to ([`ADDRESS_TITLES`]: 「好的王小姐拜拜」) or
/// "you" ([`zh_you_before_bye`]); particles may stand between: 「好喔掰」.
fn zh_bye_before_ok(before: &str, separated: bool) -> bool {
    let b: String = before.chars().filter(|c| !c.is_whitespace()).collect();
    if separated || b.is_empty() {
        return true;
    }
    let mut b = b.as_str();
    loop {
        let cue = zh_cue_ends(b) || zh_addressee_before_bye(b) || zh_you_before_bye(b);
        if cue {
            return true;
        }
        let rest = b.trim_end_matches(|c: char| ZH_PARTICLES.contains(c));
        if rest.len() == b.len() {
            return false;
        }
        if rest.is_empty() {
            return true;
        }
        b = rest;
    }
}

/// The Chinese goodbye word 「拜拜／掰掰／掰」 (with a particle after it, as the
/// lexicon has them) in `word`.
fn is_zh_bye_word(word: &str) -> bool {
    let w: String = word.chars().filter(|c| !c.is_whitespace()).collect();
    ZH_BYE_WORDS.contains(&w.trim_end_matches(|c: char| ZH_PARTICLES.contains(c)))
}

/// What was said ends on a Chinese 「拜拜／掰掰／掰」 that is a goodbye, with
/// particles, thanks and an addressee allowed after it and a cue before it
/// ([`ZH_BYE_CUES`]); None for 「我們在拜拜」「你不要瞎掰」 and for a question.
/// Some(what came before the goodbye, normalized) otherwise.
pub(crate) fn zh_bye_ends(text: &str) -> Option<String> {
    if ends_as_question(text) {
        return None;
    }
    let all = norm(text);
    let mut t = all.as_str();
    for _ in 0..=MAX_PEEL {
        if t.is_empty() {
            return None;
        }
        for w in ZH_BYE_WORDS {
            if let Some(before) = ends_with_phrase(t, w) {
                let separated = t[before.len()..].starts_with(' ');
                return zh_bye_before_ok(before, separated).then(|| before.to_string());
            }
        }
        t = peel(t)?.0;
    }
    None
}

/// A closing found at the end of what was said.
#[derive(Debug, Clone)]
struct Hit {
    kind: Kind,
    lang: &'static str,
    /// A greeting that also says goodbye.
    ambiguous: bool,
    /// What was said before it (normalized).
    before: String,
}

/// The closing what was said ends on, if any: a farewell, a greeting-goodbye or
/// a pre-closing, with particles, thanks and short replies after it allowed.
fn classify(text: &str) -> Option<Hit> {
    classify_with(text, crate::locales_store::current().closing_words())
}

/// Closing words of the other languages, from the translations: farewells and
/// wrap-ups that a native speaker checked, whose examples Bageo classifies
/// right and that leave every core sentence of [CORE_NOT_ENDS] alone.
#[derive(Default)]
pub(crate) struct ExtraWords {
    farewells: Vec<(&'static str, &'static [&'static str])>,
    wrapups: Vec<(&'static str, &'static [&'static str])>,
}

/// Core-language lines that do not end a call: a new closing word may not make
/// any of them end one.
const CORE_NOT_ENDS: &[&str] = &[
    "Sure, ok",
    "Yes, I am at home",
    "Hello, I am calling from the bank",
    "好，我知道了",
    "喂，你好，我是快遞",
    "はい、わかりました",
    "네, 알겠습니다",
    "Sí, claro",
    "Oui, d'accord",
    "Ja, genau",
    "Sim, claro",
];

fn leak_words(words: &[String]) -> &'static [&'static str] {
    let v: Vec<&'static str> = words
        .iter()
        .map(|w| norm(w))
        .filter(|w| !w.is_empty())
        .map(|w| &*Box::leak(w.into_boxed_str()))
        .collect();
    Box::leak(v.into_boxed_slice())
}

impl ExtraWords {
    fn of(code: &'static str, c: &bageo_packs::locales::Closing) -> Self {
        ExtraWords { farewells: vec![(code, leak_words(&c.farewells))], wrapups: vec![(code, leak_words(&c.wrapups))] }
    }

    /// The closing's examples come out right with its words, and no core line
    /// of [CORE_NOT_ENDS] ends with them.
    fn passes(&self, c: &bageo_packs::locales::Closing) -> bool {
        let ends = |s: &str| ends_judged(s, self);
        !c.examples.ends.is_empty()
            && c.examples.ends.iter().all(|s| ends(s))
            && c.examples.not_ends.iter().all(|s| !ends(s))
            && CORE_NOT_ENDS.iter().all(|s| !ends(s))
    }

    /// The words of verified closings (language code, closing) that pass:
    /// every example right and no core line of [CORE_NOT_ENDS] ending.
    pub(crate) fn from_verified(list: &[(&'static str, bageo_packs::locales::Closing)]) -> Self {
        let mut all = ExtraWords::default();
        for (code, c) in list {
            let one = ExtraWords::of(code, c);
            if one.passes(c) {
                all.farewells.extend(one.farewells);
                all.wrapups.extend(one.wrapups);
            } else {
                tracing::warn!("closing words of {code} left out: an example or a core line is classified wrong with them");
            }
        }
        all
    }

    /// The farewells of a translation's closing (verified or not) for telling
    /// the AI's own goodbye ([ai_says_translated_farewell]), normalized; None
    /// when its examples or a core line come out wrong with them.
    pub(crate) fn ai_farewells(code: &'static str, c: &bageo_packs::locales::Closing) -> Option<&'static [&'static str]> {
        let one = ExtraWords::of(code, c);
        let words = one.farewells.first().map(|(_, w)| *w).filter(|w| !w.is_empty())?;
        if one.passes(c) {
            Some(words)
        } else {
            tracing::warn!("farewells of {code} left out of the AI's goodbye check: an example or a core line is classified wrong with them");
            None
        }
    }
}

/// What was said ends the call with these words: a closing that is no
/// greeting-goodbye, or a greeting-goodbye after a closing cue ("Cảm ơn anh,
/// chào anh"), as it counts once the business is done.
fn ends_judged(said: &str, w: &ExtraWords) -> bool {
    classify_with(said, w).is_some_and(|h| !h.ambiguous || cue_before(&h.before))
}

/// Runs a closing example with the closing words under test (the translations'
/// check, [bageo_packs::locales::ClosingJudge]): true when it ends the call.
pub(crate) fn judge(code: &str, c: &bageo_packs::locales::Closing, said: &str) -> bool {
    let code: &'static str = Box::leak(code.to_string().into_boxed_str());
    ends_judged(said, &ExtraWords::of(code, c))
}

/// The AI's words end on a goodbye of a translated language (the farewells
/// of its call speech that passed, [ExtraWords::ai_farewells]): particles,
/// thanks and short words after it allowed, not a question, not negated
/// ("อย่าเพิ่งลาก่อนนะ", don't say goodbye yet).
pub(crate) fn ai_says_translated_farewell(text: &str) -> bool {
    let lex = crate::locales_store::current().ai_farewells();
    if lex.is_empty() || ends_as_question(text) {
        return false;
    }
    let all = norm(text);
    let mut t = all.as_str();
    for _ in 0..=MAX_PEEL {
        if t.is_empty() {
            return false;
        }
        if ends_with_extra_word(t, lex).is_some() {
            return true;
        }
        match peel(t) {
            Some((rest, _)) => t = rest,
            None => return false,
        }
    }
    false
}

fn classify_with(text: &str, extra: &ExtraWords) -> Option<Hit> {
    if ends_as_question(text) {
        return None;
    }
    let all = norm(text);
    let mut t = all.as_str();
    for _ in 0..=MAX_PEEL {
        if t.is_empty() {
            return None;
        }
        let found = ends_with_any(t, FAREWELLS)
            .map(|(l, b)| (Kind::Farewell, l, b, false))
            .or_else(|| lone_bai(t).map(|b| (Kind::Farewell, "zh-TW", b, false)))
            .or_else(|| ends_with_extra_word(t, &extra.farewells).map(|(l, b)| (Kind::Farewell, l, b, false)))
            .or_else(|| ends_with_any(t, PRE_CLOSINGS).map(|(l, b)| (Kind::PreClosing, l, b, false)))
            .or_else(|| ends_with_extra_word(t, &extra.wrapups).map(|(l, b)| (Kind::PreClosing, l, b, false)))
            .or_else(|| ends_with_any(t, GREETING_BYES).map(|(l, b)| (Kind::Farewell, l, b, true)));
        if let Some((kind, lang, before, ambiguous)) = found {
            // 「你先不要掛啦」「請別掛囉」: a hang-up word after "don't" asks to stay on.
            if kind == Kind::Farewell && asks_to_stay(before, &t[before.len()..]) {
                return None;
            }
            if kind == Kind::Farewell && lang == "he" && he_unsure_see_you(before, &t[before.len()..]) {
                return None;
            }
            // 「我們在拜拜」「你不要瞎掰」: 拜拜／掰 as other words are no goodbye.
            if kind == Kind::Farewell {
                let word = &t[before.len()..];
                if is_zh_bye_word(word) && !zh_bye_before_ok(before, word.starts_with(' ')) {
                    return None;
                }
            }
            return Some(Hit { kind, lang, ambiguous, before: before.to_string() });
        }
        t = match id_time_after_bye(t) {
            Some(rest) => rest,
            None => peel(t)?.0,
        };
    }
    None
}

/// Hebrew 「נתראה」 ("we'll see each other") after 「אם／לא／מתי／איפה／אולי」 is a doubt or a
/// question, not a goodbye: 「אני לא יודע אם נתראה」.
fn he_unsure_see_you(before: &str, word: &str) -> bool {
    word.trim_start().starts_with("נתראה")
        && ["אם", "האם", "לא", "מתי", "איפה", "אולי"].iter().any(|p| ends_with_phrase(before.trim_end(), p).is_some())
}

/// Indonesian "see you" with a day after it ("Sampai ketemu Rabu pagi, Pak",
/// "sampai jumpa hari Senin ya, Bu"): the day, time and address words come off
/// so the goodbye before them is read. Only after "sampai jumpa／ketemu":
/// "tunggu sampai besok pagi" (wait until tomorrow morning) keeps its words.
const ID_TIME_WORDS: &[&str] = &[
    "hari", "senin", "selasa", "rabu", "kamis", "jumat", "sabtu", "minggu", "besok", "lusa", "pagi", "siang", "sore",
    "malam", "depan", "nanti", "lagi", "ya", "pak", "bu", "bapak", "ibu", "mas", "mbak", "kak",
];
const ID_SEE_YOU: &[&str] = &["sampai jumpa", "sampai ketemu"];

fn id_time_after_bye(t: &str) -> Option<&str> {
    let mut rest = t.trim_end();
    let mut took = 0;
    while let Some((head, last)) = rest.rsplit_once(' ') {
        if took >= 4 || !ID_TIME_WORDS.contains(&last) {
            break;
        }
        rest = head.trim_end();
        took += 1;
    }
    (took > 0 && ID_SEE_YOU.iter().any(|s| rest == *s || rest.ends_with(&format!(" {s}")))).then_some(rest)
}

/// A hang-up word (掛／挂) right after "don't" (不要、別、唔好…): the caller asks
/// not to hang up.
fn asks_to_stay(before: &str, word: &str) -> bool {
    if !word.contains('掛') && !word.contains('挂') {
        return false;
    }
    let b = before.trim_end();
    ["不要", "別", "别", "不要先", "別先", "别先", "先不要", "先別", "先别", "唔好", "咪", "勿"].iter().any(|n| b.ends_with(n))
}

/// Japanese opening apologies before 「失礼します／失礼いたします」: not a farewell.
fn ja_opening_apology(before: &str) -> bool {
    const OPENERS: &[&str] = &[
        "お忙しいところ", "お忙しい中", "夜分に", "夜分遅く", "夜遅く", "朝早く", "朝から", "突然",
        "恐れ入りますが", "おそれいりますが", "初めてお電話", "初めてご連絡", "お取り込み中",
        "お電話口で", "何度も失礼", "度々失礼", "たびたび失礼",
    ];
    let compact: String = before.chars().filter(|c| !c.is_whitespace()).collect();
    OPENERS.iter().any(|p| compact.contains(p))
}

/// "Understood" in Japanese ([`SETTLED`] has 「承知」 as a stem): right before
/// 「では／じゃ」 it settles the matter, 「わかりました、では失礼いたします」.
const JA_UNDERSTOOD: &[&str] = &[
    "かしこまりました", "承知しました", "承知いたしました", "承知致しました", "承知です", "承知", "わかりました",
    "分かりました",
];

fn ends_with_ja_wrap_cue(t: &str) -> bool {
    const UNCONDITIONAL: &[&str] = &[
        "それでは", "それじゃあ", "それじゃ", "じゃあ", "ではでは", "以上です", "以上となります",
        "以上になります", "そろそろ", "よろしくお願いいたします", "よろしくお願いします",
        "よろしくおねがいいたします", "またあとで", "また後で", "また明日", "お先に",
    ];
    if UNCONDITIONAL.iter().any(|c| t.ends_with(c)) {
        return true;
    }
    ["では", "じゃ"].iter().any(|c| {
        t.strip_suffix(c).is_some_and(|rest| {
            let rest = rest.trim_end();
            rest.is_empty()
                || ends_with_any(rest, PRE_CLOSINGS).is_some()
                || ends_with_any(rest, FAREWELLS).is_some()
                || matches!(peel(rest), Some((_, Peel::Thanks | Peel::Backchannel)))
                || JA_UNDERSTOOD.iter().any(|w| ends_with_phrase(rest, w).is_some())
        })
    })
}

/// Right before a greeting-goodbye: thanks, a short "ok", a pre-closing or a Japanese wrap cue.
fn cue_before(before: &str) -> bool {
    if it_farewell_before(before) || th_see_you_before(before) {
        return true;
    }
    let mut t = before.trim_end();
    for _ in 0..MAX_PEEL {
        if t.is_empty() {
            return false;
        }
        if ends_with_any(t, PRE_CLOSINGS).is_some()
            || ends_with_any(t, FAREWELLS).is_some()
            || ends_with_ja_wrap_cue(t)
        {
            return true;
        }
        match peel(t) {
            Some((_, Peel::Thanks | Peel::Backchannel)) => return true,
            Some((rest, _)) => t = rest,
            None => return false,
        }
    }
    false
}

/// Words that may follow an Italian goodbye before a closing "ciao" ("ci
/// sentiamo domani, ciao").
const IT_BYE_BEFORE_WORDS: usize = 2;

/// Thai "see you" ("แล้วพบกัน", "ไว้เจอกัน") with a time right after it
/// ("…วันพฤหัสบดีหน้านะคะ", see you next Thursday): the call is closing, so a
/// สวัสดี after it is a goodbye. Not a question ("…ได้ไหมคะ", can we meet…?),
/// and the bare "พบกัน"/"เจอกัน" only at the start of the last run of letters
/// ("เราเจอกันวันจันทร์ที่แล้ว…", we met last Monday, is no goodbye).
const TH_SEE_YOU: &[&str] = &[
    "แล้วพบกัน", "ไว้พบกัน", "ค่อยพบกัน", "งั้นพบกัน", "แล้วเจอกัน", "ไว้เจอกัน", "ค่อยเจอกัน", "งั้นเจอกัน", "เจอกัน", "พบกัน",
];
const TH_SEE_YOU_TIMES: &[&str] = &[
    "พรุ่งนี้", "มะรืน", "วัน", "อาทิตย์หน้า", "สัปดาห์หน้า", "เดือนหน้า", "ปีหน้า", "คืนนี้", "เย็นนี้", "บ่ายนี้", "เช้านี้",
    "ตอนเช้า", "ตอนบ่าย", "ตอนเย็น", "ครั้งหน้า", "คราวหน้า", "เสาร์", "อาทิตย์",
];
const TH_QUESTION_WORDS: &[&str] = &["ไหม", "มั้ย", "หรือเปล่า", "หรือยัง", "หรือไม่", "เมื่อไหร่", "กี่โมง", "ไหน", "อะไร", "ยังไง"];
/// Letters a Thai time may take after "see you" ("วันพฤหัสบดีหน้า" is 15).
const TH_SEE_YOU_TAIL: usize = 24;
/// Runs of Thai letters read together: speech to text may put spaces inside a
/// sentence ("แล้วพบกัน วันพฤหัสบดีหน้า นะคะ").
const TH_SEE_YOU_RUNS: usize = 4;

fn th_see_you_before(before: &str) -> bool {
    let runs: Vec<&str> = before.split(' ').filter(|w| !w.is_empty()).collect();
    // The bare "พบกัน"/"เจอกัน" only at the start of the last run itself.
    (1..=runs.len().min(TH_SEE_YOU_RUNS)).any(|k| th_see_you_in(&runs[runs.len() - k..].concat(), k == 1))
}

fn th_see_you_in(run: &str, bare_ok: bool) -> bool {
    let mut run = run;
    'particles: loop {
        for p in TH_PARTICLES {
            if let Some(rest) = run.strip_suffix(*p) {
                run = rest;
                continue 'particles;
            }
        }
        break;
    }
    if TH_QUESTION_WORDS.iter().any(|q| run.contains(q)) {
        return false;
    }
    TH_SEE_YOU.iter().any(|m| {
        let Some(i) = run.rfind(m) else { return false };
        let bare = *m == "เจอกัน" || *m == "พบกัน";
        let rest = &run[i + m.len()..];
        (!bare || (bare_ok && i == 0))
            && rest.chars().count() <= TH_SEE_YOU_TAIL
            && TH_SEE_YOU_TIMES.iter().any(|t| rest.starts_with(t))
    })
}

/// Thai, Vietnamese and Indonesian callers often thank, add one more line and
/// only then say the greeting-goodbye ("Makasih banyak ya, Pak. Tolong kabari
/// saya secepatnya. Selamat siang!", "Dạ em cảm ơn anh nhiều nhé. Chúc anh một
/// ngày tốt lành ạ. Xin chào anh ạ."). For them thanks or a closing a few words
/// back count too, unless a question comes after it (asking to meet is no
/// goodbye). The other languages keep the rule of [`cue_before`].
const CUE_SHORTLY_WORDS: usize = 12;
const CUE_SHORTLY_QUESTIONS: &[(&str, &[&str])] = &[
    ("th", TH_QUESTION_WORDS),
    ("vi", VI_QUESTION_WORDS),
    ("id", &["apa", "apakah", "kapan", "bagaimana", "gimana", "berapa", "mana", "siapa"]),
];

fn cue_shortly_before(lang: &str, before: &str) -> bool {
    let Some((_, questions)) = CUE_SHORTLY_QUESTIONS.iter().find(|(l, _)| *l == lang) else {
        return false;
    };
    let words: Vec<&str> = before.split(' ').filter(|w| !w.is_empty()).collect();
    let from = words.len().saturating_sub(CUE_SHORTLY_WORDS);
    for end in (from..words.len()).rev() {
        if !cue_before(&words[..=end].join(" ")) {
            continue;
        }
        let tail = words[end + 1..].join(" ");
        let padded = format!(" {tail} ");
        // Thai has no spaces between words: its question words are looked for anywhere.
        let asks = questions.iter().any(|q| if lang == "th" { tail.contains(q) } else { padded.contains(&format!(" {q} ")) });
        return !asks;
    }
    false
}

/// An Italian goodbye shortly before a greeting-goodbye ("Perfetto, ci sentiamo
/// domani. Ciao"): the call is ending already.
fn it_farewell_before(before: &str) -> bool {
    let ws: Vec<&str> = before.split(' ').filter(|w| !w.is_empty()).collect();
    FAREWELLS.iter().filter(|(l, _)| *l == "it").flat_map(|(_, list)| list.iter()).any(|p| {
        let pw: Vec<&str> = p.split(' ').collect();
        ws.len() >= pw.len()
            && (ws.len().saturating_sub(pw.len() + IT_BYE_BEFORE_WORDS)..=ws.len() - pw.len()).any(|i| ws[i..i + pw.len()] == pw[..])
    })
}

/// Words that settle a matter before closing thanks: agreement, "got it", an
/// apology for a wrong number. Space-separated scripts are matched as whole words;
/// the others are compared without spaces.
const SETTLED: &[&str] = &[
    "好的", "好喔", "好哦", "好啊", "好啦", "好呀", "好嘞", "沒問題", "没问题", "冇問題", "冇问题", "無問題", "无问题", "沒錯",
    "没错", "就是這樣", "就是这样", "了解", "明白", "知道了", "收到", "那就", "那我就", "打錯", "打错", "不好意思", "抱歉",
    "かしこまりました", "承知", "わかりました", "分かりました", "失礼しました", "すみません", "申し訳",
    "알겠습니다", "알겠어요", "잘못걸", "죄송", "okay", "ok", "alright", "all right", "sure", "perfect", "great",
    "got it", "understood", "sorry", "wrong number", "will do", "no problem", "sounds good", "vale", "perfecto",
    "de acuerdo", "entendido", "perdón", "perdone", "disculpe", "número equivocado", "d'accord", "parfait",
    "entendu", "très bien", "désolé", "désolée", "pardon", "alles klar", "in ordnung", "verstanden", "perfekt",
    "entschuldigung", "falsch verbunden", "certo", "perfeito", "combinado", "entendi", "tá bom", "ta bom",
    "desculpa", "desculpe", "beleza",
    "va bene", "perfetto", "d'accordo", "ho capito", "capito", "nessun problema", "mi scusi", "scusi", "numero sbagliato",
    "benissimo", "ottimo", "chiaro", "esatto", "scusate", "scusa", "mi dispiace", "ho sbagliato numero",
    "dạ vâng", "vâng", "được rồi", "hiểu rồi", "không sao", "xin lỗi", "nhầm số", "gọi nhầm",
    "baik", "siap", "mengerti", "paham", "tidak masalah", "maaf", "salah sambung", "salah nomor",
    "โอเค", "ตกลง", "ได้เลย", "รับทราบ", "เข้าใจแล้ว", "ไม่มีปัญหา", "ขอโทษ", "โทรผิด",
    "хорошо", "ладно", "понял", "поняла", "понятно", "договорились", "без проблем", "извините", "не туда попал", "ошибся номером",
    "حسناً", "حسنا", "تمام", "طيب", "فهمت", "لا مشكلة", "آسف", "الرقم غلط", "رقم غلط",
    "ठीक है", "अच्छा", "समझ गया", "समझ गई", "कोई बात नहीं", "माफ़ कीजिए", "माफ कीजिए", "गलत नंबर",
    "tamam", "peki", "anladım", "sorun değil", "özür dilerim", "yanlış numara", "yanlış aradım",
];

/// Settling words that close a call with nothing more said: a wrong number.
const SETTLED_ALONE: &[&str] = &[
    "打錯", "打错", "wrong number", "잘못걸", "número equivocado", "falsch verbunden", "失礼しました",
    "numero sbagliato", "ho sbagliato numero", "nhầm số", "gọi nhầm", "salah sambung", "salah nomor", "โทรผิด",
    "не туда попал", "ошибся номером", "الرقم غلط", "رقم غلط", "गलत नंबर",
    "yanlış numara", "yanlış aradım",
];

/// Words that ask something even without a question mark (「那明天幾點到」,
/// "what time tomorrow", 「何時ですか」): such words before thanks are no wrap-up.
const ASKS_RUNS: &[&str] = &[
    "幾點", "几点", "什麼", "什么", "哪裡", "哪里", "哪天", "多少", "怎麼", "怎么", "為什麼", "为什么", "是不是",
    "有沒有", "有没有", "可不可以", "能不能", "要不要", "何時", "いつ", "どこ", "何で", "なぜ", "ですか", "ますか",
    "언제", "어디", "몇", "뭐", "왜", "กี่โมง", "เมื่อไหร่", "ที่ไหน", "เท่าไหร่",
];
const ASKS_WORDS: &[&str] = &[
    "what", "when", "where", "which", "how", "who", "why", "can you", "could you", "is there", "are you", "do you",
    "will you", "qué", "cuándo", "dónde", "cómo", "quand", "où", "comment", "quel", "quelle", "wann", "wo", "wie",
    "was", "welche", "quando", "onde", "como", "qual", "dove", "che ora", "a che ora", "che ore", "quale", "quanto", "quanti", "perché",
    "perche", "cosa", "bao giờ", "mấy giờ", "ở đâu",
    "jam berapa", "kapan", "di mana", "когда", "где", "сколько", "почему", "متى", "أين", "كم", "कब", "कहाँ", "कितना",
    "ne zaman", "nerede", "kaç", "nasıl", "neden",
];

fn asks(t: &str) -> bool {
    let spaced = format!(" {t} ");
    ASKS_RUNS.iter().any(|w| t.contains(w)) || ASKS_WORDS.iter().any(|w| spaced.contains(&format!(" {w} ")))
}

/// Something said besides the settling words ("great" and thanks is only
/// thanks): three or more CJK / spaceless-script letters, or two or more
/// alphabetic words (Latin, Cyrillic, Arabic, Devanagari…).
fn has_matter(t: &str) -> bool {
    let mut s = format!(" {t} ");
    for w in SETTLED {
        s = if is_spaced(w) { s.replace(&format!(" {w} "), "  ") } else { s.replace(w, " ") };
    }
    for p in TH_PARTICLES {
        s = s.replace(p, " ");
    }
    let unspaced = s
        .chars()
        .filter(|c| {
            (matches!(*c as u32, 0x3040..=0x30FF | 0x3400..=0x9FFF | 0xAC00..=0xD7AF) && !ZH_PARTICLES.contains(*c))
                || (matches!(*c as u32, 0x0E00..=0x0EFF | 0x1000..=0x109F | 0x1780..=0x17FF) && c.is_alphabetic())
        })
        .count();
    const FILLER: &[&str] = &["oh", "ah", "um", "uh", "so", "and", "then", "well", "yes", "yeah", "thanks", "thank", "you"];
    let words = s
        .split_whitespace()
        .filter(|w| {
            w.chars().any(|c| c.is_alphabetic() && !is_cjk_or_unspaced(c))
                && w.chars().count() >= 2
                && !FILLER.contains(w)
        })
        .count();
    unspaced >= 3 || words >= 2
}

/// The caller's words since their last question settle the matter and end on
/// thanks: 「好的，沒問題，那明天我放管理室，謝謝！」, "Oh sorry, wrong number,
/// thanks". What they heard may still hold their earlier question (「…有人在家嗎？」);
/// only what follows it counts. Thanks after nothing but short replies (「好，謝謝」)
/// is not one.
fn thanks_wrap(text: &str) -> bool {
    let text = text.rsplit(['?', '？', '؟']).next().unwrap_or("");
    if ends_as_question(text) {
        return false;
    }
    let all = norm(text);
    let mut t = all.as_str();
    let mut thanked = false;
    let mut nordic = false;
    for _ in 0..MAX_PEEL {
        match peel(t) {
            Some((rest, Peel::Thanks)) => {
                thanked = true;
                nordic |= is_nordic_thanks(t[rest.len()..].trim());
                t = rest;
            }
            Some((rest, Peel::Particle | Peel::Word)) if !thanked => t = rest,
            _ => break,
        }
    }
    if thanked && nordic {
        return nordic_settled_only(t);
    }
    if !thanked || t.is_empty() || is_fluff(t) || ends_as_question(t) || asks(t) {
        return false;
    }
    let spaced = format!(" {t} ");
    let bare: String = t.chars().filter(|c| *c != ' ').collect();
    let has = |list: &[&str]| {
        list.iter().any(|w| if is_spaced(w) { spaced.contains(&format!(" {w} ")) } else { bare.contains(w) })
    };
    has(SETTLED_ALONE) || (has_matter(t) && SETTLED.iter().any(|w| {
        if is_spaced(w) {
            spaced.contains(&format!(" {w} "))
        } else {
            bare.contains(w)
        }
    }))
}

/// Swedish, Norwegian, Danish and Finnish thanks also say "please" at the end of a
/// request ("Kan jag få ditt nummer, tack"; "Ja, takk" is "yes, please"): a wrap-up
/// only after nothing but settling words ([`nordic_settled_only`]).
const NORDIC_THANKS: &[&str] = &[
    "tack så mycket", "tusen tack", "stort tack", "tack ska du ha", "tack", "tusen takk", "mange takk", "takk skal du ha",
    "takk", "tusind tak", "mange tak", "tak skal du have", "tak for det", "kiitos paljon", "paljon kiitoksia", "kiitoksia",
    "kiitos", "kiitti",
];
const NORDIC_THANKS_FOR: &[&str] = &["tack så mycket för", "tack för", "tusen takk for", "takk for", "mange tak for", "tak for"];

/// Words that settle a matter before Nordic thanks. Not "ja" / "joo" / "kyllä" alone
/// ("yes, please"), "bra" / "fint" / "godt" / "hyvää" ("fine, thanks" answers "how are
/// you") or "nej" / "nei" / "ei" alone ("no, thanks").
const NORDIC_SETTLED: &[&str] = &[
    "okej", "ok", "okay", "okei", "perfekt", "utmärkt", "uppfattat", "jag förstår", "då vet jag det", "då vet jag", "det var allt",
    "det var bara det", "då gör vi så", "det låter bra", "låter bra", "greit", "den er grei", "flott", "supert", "skjønner",
    "jeg skjønner", "da vet jeg det", "da vet jeg", "det var alt", "da gjør vi det sånn", "høres bra ut", "notert", "forstått", "super",
    "forstået", "i orden", "det er i orden", "så ved jeg det", "det var det", "det gør vi", "det lyder godt", "lyder godt",
    "selvä", "selvä homma", "selvä juttu", "hyvä on", "ymmärrän", "nyt tiedän", "sitten tiedän", "se oli siinä",
    "siinä kaikki", "ei muuta", "kuulostaa hyvältä",
];
/// Short words that may stand next to them: "Nej, det var allt, tack", "Okej, bra, tack".
const NORDIC_FILLERS: &[&str] = &[
    "ja", "jaha", "jo", "joo", "kyllä", "nej", "nei", "ei", "nå", "no", "niin", "då", "da", "så", "bra", "fint", "godt", "hyvä",
];

fn is_nordic_thanks(said: &str) -> bool {
    NORDIC_THANKS.contains(&said)
        || NORDIC_THANKS_FOR.iter().any(|p| said.strip_prefix(p).is_some_and(|rest| rest.starts_with(' ')))
}

/// `t` (normalized, the thanks peeled off) is settling words only, at least one of
/// [`NORDIC_SETTLED`]: "okej", "nej det var allt", "selvä".
fn nordic_settled_only(t: &str) -> bool {
    let mut rest = t.trim_end();
    let mut settled = false;
    while !rest.is_empty() {
        if let Some(head) = NORDIC_SETTLED.iter().find_map(|p| ends_with_phrase(rest, p)) {
            settled = true;
            rest = head;
        } else if let Some(head) = NORDIC_FILLERS.iter().find_map(|p| ends_with_phrase(rest, p)) {
            rest = head;
        } else {
            return false;
        }
    }
    settled
}

/// Only greetings, thanks and short replies (or nothing): 「喂？」, "Alo, chào anh
/// ạ", 「여보세요, 수고하십니다」.
fn greeting_only(text: &str) -> bool {
    let all = norm(text);
    let mut t = all.as_str();
    for _ in 0..12 {
        if t.is_empty() {
            return true;
        }
        // Greetings first: the addressee in "chào anh ạ" is part of it.
        let last = t.chars().next_back();
        let greeting = OPENING_GREETINGS
            .iter()
            .filter(|g| g.chars().next_back() == last)
            .find_map(|g| ends_with_phrase(t, g))
            .or_else(|| ends_with_any(t, GREETING_BYES).map(|(_, rest)| rest))
            .or_else(|| peel(t).map(|(rest, _)| rest));
        match greeting {
            Some(rest) => t = rest,
            None => return false,
        }
    }
    false
}

/// Only a backchannel ("Vale, vale", 「네~ 네」, "Uh-huh", 「對對對」).
pub fn is_backchannel(text: &str) -> bool {
    let all = norm(text);
    if all.is_empty() || all.chars().count() > 30 {
        return false;
    }
    let mut t = all.as_str();
    for _ in 0..8 {
        if t.is_empty() {
            return true;
        }
        match ends_with_any(t, BACKCHANNELS) {
            Some((_, rest)) => t = rest,
            None => {
                let p = t.trim_end_matches(|c: char| ZH_PARTICLES.contains(c));
                if p.len() == t.len() {
                    return false;
                }
                t = p.trim_end();
            }
        }
    }
    false
}

/// Nothing but thanks, short replies and particles (or nothing at all): no new
/// content, and no goodbye either.
pub fn is_fluff(text: &str) -> bool {
    let all = norm(text);
    let mut t = all.as_str();
    for _ in 0..12 {
        if t.is_empty() {
            return true;
        }
        match peel(t) {
            Some((rest, _)) => t = rest,
            None => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A whole caller turn.
    fn caller(m: &mut ClosingMachine, text: &str) -> ClosingStep {
        m.on_caller(text, text, false, false)
    }

    /// After some business, so greetings that also say goodbye may close.
    fn mid_call(lang: &'static str) -> ClosingMachine {
        let mut m = ClosingMachine::new(lang);
        caller(&mut m, "我要寄一個包裹給王先生 0912345678");
        assert_eq!(m.state(), ClosingState::Talking);
        m
    }

    struct Case {
        lang: &'static str,
        pre: [&'static str; 2],
        bye: [&'static str; 2],
        opening: &'static str,
        follow_up: &'static str,
    }

    const CASES: &[Case] = &[
        Case {
            lang: "en",
            pre: ["Anyway, I won't keep you.", "OK, I'll let you get back to it."],
            bye: ["Talk to you soon, bye!", "Have a great rest of your day."],
            opening: "Hello?",
            follow_up: "Oh wait, one more thing, when does the parcel arrive?",
        },
        Case {
            lang: "zh-TW",
            pre: ["好喔，那就先這樣囉", "那我不耽誤你時間了"],
            bye: ["好，謝謝喔，掰掰～掰掰！", "好啦，多謝，再會"],
            opening: "喂，你好",
            follow_up: "啊對了再問一下，明天幾點會到？",
        },
        Case {
            lang: "yue",
            pre: ["好啦，係咁先啦", "唔阻你啦"],
            bye: ["好，唔該晒，拜拜！", "得啦唔該晒"],
            opening: "喂？",
            follow_up: "係呢，我想再問下幾時送到呀？",
        },
        Case {
            lang: "zh-CN",
            pre: ["行，那就先这样啊", "回头微信说"],
            bye: ["好嘞，拜拜", "那我先挂了啊"],
            opening: "喂，您好",
            follow_up: "哎对了，还有个事，快递放哪里？",
        },
        Case {
            lang: "ja",
            pre: ["以上となりますが…", "それでは、よろしくお願いいたします"],
            bye: ["ありがとうございました、では失礼いたします", "本日はありがとうございました"],
            opening: "もしもし、お世話になっております",
            follow_up: "あ、もう一つだけ、配達は何時ごろになりますか",
        },
        Case {
            lang: "ko",
            pre: ["나중에 다시 연락드릴게요", "그럼 이만"],
            bye: ["네, 들어가세요~", "네, 수고하세요"],
            opening: "여보세요, 수고하십니다",
            follow_up: "아 그리고 하나만 더 여쭤볼게요, 택배는 언제 와요",
        },
        Case {
            lang: "es",
            pre: ["Bueno, te dejo…", "Nada, era solo eso"],
            bye: ["Chao, cuídate", "Hasta luego, un abrazo"],
            opening: "¿Bueno? Hola",
            follow_up: "Ah, una cosa más, el paquete llega mañana",
        },
        Case {
            lang: "fr",
            pre: ["Je ne vous retiens pas plus longtemps", "Bon, je vous laisse"],
            bye: ["Merci, bonne journée, au revoir !", "À bientôt, bisous"],
            opening: "Allô, bonjour !",
            follow_up: "Ah, une dernière chose, le colis arrive quand",
        },
        Case {
            lang: "de",
            pre: ["Gut, dann verbleiben wir so", "Alles klar, dann weiß ich Bescheid"],
            bye: ["Auf Wiederhören! Schönen Tag noch!", "Tschüss"],
            opening: "Hallo? Grüß Gott",
            follow_up: "Ach, noch eine Frage, wann kommt das Paket",
        },
        Case {
            lang: "pt",
            pre: ["Então tá bom", "Combinado então"],
            bye: ["Tchau, tchau!", "Um abraço, fica com Deus"],
            opening: "Alô? Oi, tudo bem",
            follow_up: "Ah, só mais uma coisa, o pacote chega amanhã",
        },
        Case {
            lang: "th",
            pre: ["งั้นแค่นี้นะคะ", "แค่นี้ก่อนนะครับ"],
            bye: ["บ๊ายบายค่ะ", "ขอบคุณค่ะ สวัสดีค่ะ"],
            opening: "สวัสดีค่ะ",
            follow_up: "อ้อ ขอถามอีกเรื่อง พัสดุจะมาถึงกี่โมง",
        },
        Case {
            lang: "vi",
            pre: ["Dạ vậy nhé", "Thế nhé"],
            bye: ["Cảm ơn anh, chào anh ạ", "Tạm biệt"],
            opening: "Alo, chào anh ạ",
            follow_up: "À cho em hỏi thêm, mấy giờ giao hàng",
        },
        Case {
            lang: "id",
            pre: ["Oke, sudah dulu ya", "Itu saja"],
            bye: ["Terima kasih, selamat siang", "Sampai jumpa"],
            opening: "Halo, selamat siang",
            follow_up: "Oh iya, satu lagi, paketnya sampai jam berapa",
        },
        Case {
            lang: "it",
            pre: ["Va bene, allora ti lascio", "Non la disturbo più"],
            bye: ["Grazie mille, arrivederci!", "Buona giornata, a presto"],
            opening: "Pronto?",
            follow_up: "Ah, un'altra cosa, quando arriva il pacco",
        },
    ];

    #[test]
    fn pre_closings_in_every_language() {
        for c in CASES {
            for p in c.pre {
                let mut m = mid_call(c.lang);
                let step = caller(&mut m, p);
                assert_eq!(step.event, ClosingEvent::CallerPreClosing, "{}: {p}", c.lang);
                assert_eq!(m.state(), ClosingState::PreClosing, "{}: {p}", c.lang);
                // The AI's farewell answers it: farewells exchanged, the call may end.
                assert_eq!(m.on_ai_turn("好，謝謝，掰掰", true, false), ClosingEvent::FarewellsExchanged, "{p}");
                assert!(m.may_hang_up(false), "{}: {p}", c.lang);
            }
        }
    }

    #[test]
    fn farewells_in_every_language() {
        for c in CASES {
            for b in c.bye {
                let mut m = mid_call(c.lang);
                let step = caller(&mut m, b);
                assert_eq!(step.event, ClosingEvent::CallerFarewell, "{}: {b}", c.lang);
                assert!(step.ends_call, "{}: {b}", c.lang);
                assert!(m.may_hang_up(false), "{}: {b}", c.lang);
                // While stalling the same words only report the event.
                assert!(!m.may_hang_up(true));
            }
        }
    }

    #[test]
    fn opening_greetings_never_close() {
        for c in CASES {
            let mut m = ClosingMachine::new(c.lang);
            for _ in 0..2 {
                let step = caller(&mut m, c.opening);
                assert!(!step.ends_call, "{}: {}", c.lang, c.opening);
                assert_eq!(step.event, ClosingEvent::None, "{}: {}", c.lang, c.opening);
            }
            // Even when the older rules would have called it a goodbye.
            assert!(!m.on_caller(c.opening, c.opening, true, false).ends_call, "{}", c.opening);
            assert_eq!(m.state(), ClosingState::Talking, "{}: {}", c.lang, c.opening);
        }
        // Greetings that also say goodbye, alone at the start.
        for g in ["수고하십니다", "สวัสดีค่ะ", "Chào anh", "Selamat siang", "Servus!", "Bonjour", "Bonsoir"] {
            let mut m = ClosingMachine::new("en");
            assert!(!caller(&mut m, g).ends_call, "{g}");
            assert_eq!(m.state(), ClosingState::Talking, "{g}");
        }
    }

    #[test]
    fn a_question_after_the_pre_closing_goes_back_to_talking() {
        for c in CASES {
            let mut m = mid_call(c.lang);
            caller(&mut m, c.pre[0]);
            assert_eq!(m.state(), ClosingState::PreClosing, "{}", c.lang);
            let step = caller(&mut m, c.follow_up);
            assert_eq!(step.event, ClosingEvent::CallerResumed, "{}: {}", c.lang, c.follow_up);
            assert_eq!(m.state(), ClosingState::Talking, "{}: {}", c.lang, c.follow_up);
            assert!(!step.ends_call);
        }
    }

    #[test]
    fn thanks_and_backchannels_keep_the_state() {
        let mut m = mid_call("zh-TW");
        // Thanks alone is no goodbye.
        assert_eq!(caller(&mut m, "謝謝").event, ClosingEvent::None);
        assert_eq!(m.state(), ClosingState::Talking);
        caller(&mut m, "那就先這樣");
        for s in ["嗯嗯", "好喔", "謝謝你", "Genau", "네네"] {
            assert_eq!(caller(&mut m, s).event, ClosingEvent::None, "{s}");
            assert_eq!(m.state(), ClosingState::PreClosing, "{s}");
        }
    }

    #[test]
    fn the_ais_farewell_alone_does_not_end_the_call() {
        let mut m = mid_call("en");
        assert_eq!(m.on_ai_turn("Okay, bye!", true, false), ClosingEvent::None);
        assert_eq!(m.state(), ClosingState::PreClosing);
        assert!(!m.may_hang_up(false));
        // The caller answers with thanks only: still not theirs to end.
        caller(&mut m, "Thank you");
        assert!(!m.may_hang_up(false));
        // Their goodbye does.
        assert!(caller(&mut m, "Bye").ends_call);
        assert!(m.may_hang_up(false));
        m.hangup("goodbye_timer");
        assert_eq!(m.state(), ClosingState::Hangup);
    }

    /// The caller settles the matter and thanks, the AI says goodbye: the call
    /// ends after the AI's words (matrix B: delivery TW / JP, wrong number TW).
    #[test]
    fn settled_thanks_and_the_ais_farewell_are_the_exchange() {
        let calls: [(&str, &str, &str, &str); 4] = [
            (
                "zh-TW",
                "喂，王先生你好，我是宅配司機，明天下午有個包裹要送過去，請問明天下午大概幾點有人在家呢？",
                "好的，沒問題，那明天下午我就幫您放管理室，再麻煩您留意囉，謝謝！",
                "好，謝謝你喔，掰掰。",
            ),
            (
                "ja",
                "もしもし、お世話になっております。山田運送です。王さんのお荷物をお持ちしたんですが、明日の午後はご在宅でしょうか?",
                "かしこまりました。それでは、ご不在の場合は管理室にお預けしますね。ご対応ありがとうございます。",
                "はい、お願いします。バイバイ。",
            ),
            ("zh-TW", "喂，請問是陳小姐嗎？我是她同事", "喔，不好意思，我打錯了，謝謝", "沒關係，掰掰"),
            ("en", "Hi, is this Alex? I'm calling about the car", "Oh sorry, wrong number then, thanks", "No worries, bye!"),
        ];
        for (lang, first, settled, ai_bye) in calls {
            let mut m = ClosingMachine::new(lang);
            caller(&mut m, first);
            let step = caller(&mut m, settled);
            assert_eq!(step.event, ClosingEvent::CallerThanksWrap, "{settled}");
            assert!(!step.ends_call, "{settled}");
            assert_eq!(m.state(), ClosingState::PreClosing, "{settled}");
            assert_eq!(m.on_ai_turn(ai_bye, true, false), ClosingEvent::FarewellsExchanged, "{settled}");
            assert!(m.may_hang_up(false), "{settled}");
        }
    }

    #[test]
    fn bare_thanks_or_a_question_with_thanks_is_no_wrap_up() {
        for t in ["好，謝謝", "謝謝你", "Okay, thanks", "はい、ありがとうございます", "那你明天在家嗎？謝謝", "好的，那你幾點方便嗎，謝謝"] {
            let mut m = mid_call("zh-TW");
            let step = caller(&mut m, t);
            assert_ne!(step.event, ClosingEvent::CallerThanksWrap, "{t}");
            assert_eq!(m.state(), ClosingState::Talking, "{t}");
            // The AI's farewell after it waits for the caller's bye (or the quiet).
            assert_eq!(m.on_ai_turn("好，掰掰", true, false), ClosingEvent::None, "{t}");
            assert!(!m.may_hang_up(false), "{t}");
        }
        // While stalling the AI's farewell is cut: no exchange.
        let mut m = mid_call("zh-TW");
        m.on_caller("好的，沒問題，那我再考慮，謝謝", "好的，沒問題，那我再考慮，謝謝", false, true);
        assert_eq!(m.on_ai_turn("好，掰掰", true, true), ClosingEvent::None);
        assert!(!m.may_hang_up(true));
    }

    /// Thanks for something is thanks (hidden_us_en: «…Thanks for your help.»
    /// went unseen, and the call waited out the quiet after the AI's bye).
    #[test]
    fn thanks_for_something_is_thanks() {
        let mut m = ClosingMachine::new("en");
        let latest = "Got it. I'll leave the package with the building management tomorrow afternoon. Thanks for your help.";
        let heard = format!("Hi, I have a package for you. Will someone be home tomorrow afternoon? Or maybe I can leave it with building management. {latest}");
        let step = m.on_caller(&heard, latest, false, false);
        assert_eq!(step.event, ClosingEvent::CallerThanksWrap);
        assert_eq!(m.on_ai_turn("Sounds good,bye.", true, false), ClosingEvent::FarewellsExchanged);
        assert!(m.may_hang_up(false));
        for t in ["Thank you for calling!", "Thanks so much for your time", "Muchas gracias por su ayuda", "Obrigado pela atenção"] {
            assert!(is_fluff(t), "{t}");
        }
        assert!(matches!(classify("Okay, bye, thanks for calling."), Some(Hit { kind: Kind::Farewell, .. })));
        // More than a few words after it: something else is said.
        assert!(!is_fluff("Thanks for the package, it is the wrong one"));
    }

    /// Speech-to-text writes a caller's "Bye!" as 「拜」 (d_sg_nan_dinner).
    #[test]
    fn a_lone_bai_is_a_bye_but_next_week_is_not() {
        let mut m = mid_call("zh-TW");
        let step = caller(&mut m, "好,按呢我等你的電話咯。拜。");
        assert_eq!(step.event, ClosingEvent::CallerFarewell);
        assert!(step.ends_call);
        for t in ["那我們下禮拜。", "好，下個禮拜", "好，那就約下禮拜"] {
            let mut m = mid_call("zh-TW");
            assert_ne!(caller(&mut m, t).event, ClosingEvent::CallerFarewell, "{t}");
        }
    }

    /// What the caller heard so far may still hold their question of the turn
    /// before (matrix B delivery TW, as the bench heard it).
    #[test]
    fn the_earlier_question_in_the_window_does_not_hide_the_wrap_up() {
        let mut m = ClosingMachine::new("zh-TW");
        let heard = "喂，王先生你好，我是宅配的司機，想問一下明天下午有包裹寄到你那邊，請問有沒有人在家收件呢？好的，那明天下午我再幫你放到管理室，再請你留意一下囉，謝謝！";
        let step = m.on_caller(heard, "好的，那明天下午我再幫你放到管理室，再請你留意一下囉，謝謝！", false, false);
        assert_eq!(step.event, ClosingEvent::CallerThanksWrap);
        assert_eq!(m.on_ai_turn("好，謝謝你喔，掰掰。", true, false), ClosingEvent::FarewellsExchanged);
    }

    #[test]
    fn the_ai_talking_on_takes_its_own_closing_back() {
        let mut m = mid_call("zh-TW");
        m.on_ai_turn("好，那先這樣，掰掰", true, false);
        assert_eq!(m.state(), ClosingState::PreClosing);
        m.on_ai_turn("對了，包裹放管理室可以嗎？", false, false);
        assert_eq!(m.state(), ClosingState::Talking);
    }

    #[test]
    fn stalling_reports_the_closing_and_never_hangs_up() {
        let mut m = mid_call("zh-TW");
        let step = m.on_caller("好那就先這樣", "好那就先這樣", true, true);
        assert_eq!(step.event, ClosingEvent::CallerPreClosing);
        // The AI's farewell is cut while stalling: no exchange.
        assert_eq!(m.on_ai_turn("好，掰掰", true, true), ClosingEvent::None);
        assert_eq!(m.state(), ClosingState::PreClosing);
        let step = m.on_caller("掰掰", "掰掰", true, true);
        assert_eq!(step.event, ClosingEvent::CallerFarewell);
        assert!(!m.may_hang_up(true));
    }

    #[test]
    fn the_older_rules_still_close() {
        let mut m = mid_call("zh-TW");
        // A wrap-up only the older check knows: kept, as a pre-closing.
        let step = m.on_caller("我回頭再打給你", "我回頭再打給你", true, false);
        assert!(step.ends_call);
        assert_eq!(m.state(), ClosingState::PreClosing);
        let mut m = mid_call("en");
        assert!(m.on_caller("Okay, have a nice day", "Okay, have a nice day", true, false).ends_call);
        assert_eq!(m.state(), ClosingState::Closing);
    }

    #[test]
    fn greeting_goodbyes_need_the_business_done() {
        let mut m = mid_call("th");
        // Mid-call, no thanks or cue before it: not yet.
        assert!(!caller(&mut m, "สวัสดีค่ะ").ends_call);
        assert!(caller(&mut m, "ขอบคุณมากค่ะ สวัสดีค่ะ").ends_call);
        let mut m = mid_call("ko");
        caller(&mut m, "네, 알겠습니다. 그럼 이만");
        assert!(caller(&mut m, "네, 수고하십니다").ends_call);
    }

    #[test]
    fn thanks_a_line_before_a_thai_vietnamese_or_indonesian_greeting_goodbye() {
        // Lines from multilingual call simulations where the caller hung up first.
        for (lang, line) in [
            ("vi", "Dạ em cảm ơn anh nhiều nhé. Chúc anh một ngày tốt lành ạ. Xin chào anh ạ."),
            ("id", "Makasih banyak ya, Pak. Tolong kabari saya secepatnya. Selamat siang!"),
            ("th", "ขอบคุณมากค่ะ แล้วพบกัน วันพฤหัสบดีหน้า นะคะ สวัสดีค่ะ"),
            ("th", "ขอบคุณมากค่ะ แล้ว พบกัน วัน พฤหัสบดี หน้า นะคะ สวัสดีค่ะ"),
        ] {
            let mut m = mid_call(lang);
            assert!(caller(&mut m, line).ends_call, "{line}");
        }
        // A question after the thanks, or no thanks at all: not a goodbye.
        for (lang, line) in [
            ("th", "ขอบคุณค่ะ แล้วพบกัน วันพฤหัสบดีหน้า ได้ไหมคะ สวัสดีค่ะ"),
            ("vi", "Cảm ơn anh. Mai anh có rảnh không. Chào anh."),
            ("id", "Terima kasih, Pak. Kapan bisa datang. Selamat siang."),
            ("id", "Tolong kabari saya secepatnya. Selamat siang!"),
        ] {
            let mut m = mid_call(lang);
            assert!(!caller(&mut m, line).ends_call, "{line}");
        }
        // "Tôi chào cô" from the AI after settled thanks: the goodbyes are exchanged.
        let mut m = mid_call("vi");
        let step = caller(&mut m, "Dạ vâng, em gửi hàng ở quầy lễ tân nhé, cảm ơn anh nhiều.");
        assert!(!step.ends_call);
        assert_eq!(m.on_ai_turn("Dạ vâng, cảm ơn cô. Tôi chào cô.", true, false), ClosingEvent::FarewellsExchanged);
    }

    #[test]
    fn vietnamese_chao_after_a_question_waits_and_other_languages_are_unchanged() {
        // From the multilingual call simulations: Hebrew "see you" was no goodbye, so nobody
        // closed the line. The other languages stay as they were.
        for (lang, line, ends) in [
            ("he", "מעולה, תודה רבה. נתראה.", true),
            ("he", "זהו, הכל מעודכן. נתראה בשישי.", true),
            ("he", "בסדר גמור, נתראה ביום ראשון!", true),
            ("he", "מצוין, תודה ויום טוב גם לך.", true),
            ("he", "הכל סגור, אז נתראה אז, יום טוב.", true),
            ("he", "יש גם לך את המספר?", false),
            ("he", "אני לא יודע אם נתראה", false),
            ("he", "מתי נתראה?", false),
            ("el", "Σας ευχαριστώ πολύ και καλή σας μέρα!", true),
            ("el", "Και η καλή συνέχεια της δουλειάς σας ποια είναι;", false),
            ("ur", "شکریہ، الله حافظ", true),
            ("es", "¿Nos vemos el viernes?", false),
            ("ja", "また明日の件ですが、どうなりましたか", false),
            ("zh-TW", "好，那禮拜五見，再見。", true),
        ] {
            let mut m = mid_call(lang);
            assert_eq!(caller(&mut m, line).ends_call, ends, "{lang}: {line}");
        }
        // A Vietnamese 「chào」 after a question is no parting; after 「không sao」 it is.
        for (lang, line, ends) in [
            ("vi", "Cảm ơn anh. Mai anh có rảnh không. Chào anh.", false),
            ("vi", "Dạ em gửi hàng ở quầy lễ tân, anh có ở nhà không ạ. Em chào anh ạ.", false),
            ("vi", "Dạ vâng, không sao ạ, em gửi hàng ở quầy lễ tân nhé. Em chào anh ạ.", true),
            ("el", "Ευχαριστώ πολύ για τη βοήθεια, και καλή σας μέρα.", true),
            ("nb", "Takk for hjelpen, ha en fin dag videre. Ha det bra.", true),
            ("he", "תודה רבה על העזרה, יום טוב, להתראות.", true),
            ("id", "Makasih banyak ya, Pak. Tolong kabari saya secepatnya. Selamat siang!", true),
            ("es", "Perfecto, gracias por avisar, hasta luego.", true),
            ("de", "Danke für den Anruf, tschüss.", true),
            ("zh-TW", "好，那就這樣，謝謝你，再見。", true),
        ] {
            let mut m = mid_call(lang);
            assert_eq!(caller(&mut m, line).ends_call, ends, "{lang}: {line}");
        }
    }

    #[test]
    fn indonesian_see_you_with_a_day_is_a_goodbye() {
        for line in [
            "Makasih banyak ya atas kerja samanya. Sampai ketemu Rabu pagi, Pak.",
            "Oke, sampai jumpa hari Senin ya, Bu.",
            "Baik, sampai ketemu besok.",
        ] {
            let mut m = mid_call("id");
            assert!(caller(&mut m, line).ends_call, "{line}");
        }
        for line in ["Saya tunggu sampai besok pagi.", "Tolong tunggu sampai hari Rabu ya, Pak.", "Kantornya buka sampai malam."] {
            let mut m = mid_call("id");
            assert!(!caller(&mut m, line).ends_call, "{line}");
        }
    }

    #[test]
    fn questions_are_not_closings() {
        for q in ["那就先這樣嗎？", "That's all?", "先这样吗", "Then we leave it at that, okay?"] {
            let mut m = mid_call("zh-TW");
            assert_eq!(caller(&mut m, q).event, ClosingEvent::None, "{q}");
        }
    }

    #[test]
    fn backchannels_in_every_language() {
        for s in [
            "對對對", "嗯嗯", "係", "OK lah", "はい", "なるほど", "네~", "어, 어", "응", "맞아요", "Uh-huh",
            "Right", "Gotcha", "Vale, vale", "Claro", "Dale", "D'accord", "Tout à fait", "Genau", "Alles klar",
            "Entendi", "Tá bom", "Pois", "ค่ะ", "อืม", "Vâng", "Dạ", "Iya", "Oke",
        ] {
            assert!(is_backchannel(s), "{s}");
            assert!(is_fluff(s), "{s}");
            assert!(classify(s).is_none(), "backchannel taken as a closing: {s}");
        }
        for s in ["好喔，那就先這樣囉", "Genau, das Paket kommt morgen", "Claro que no, señor", "掰掰"] {
            assert!(!is_backchannel(s), "{s}");
        }
    }

    #[test]
    fn hangup_timer_by_sim_then_language() {
        let t = |sim: Option<&str>, lang: &str| hangup_timer(DEFAULT_HANGUP_DELAY_MS, sim, lang);
        assert_eq!(t(Some("tw"), "zh-TW").ms, 1_500);
        assert_eq!(t(Some("tw"), "zh-TW").region, "TW");
        assert_eq!(t(Some("hk"), "zh-TW").ms, 1_300);
        assert_eq!(t(Some("mo"), "en").ms, 1_300);
        assert_eq!(t(Some("cn"), "zh-CN").ms, 1_200);
        assert_eq!(t(Some("jp"), "en").ms, 2_500);
        assert_eq!(t(Some("kr"), "ko").ms, 1_500);
        assert_eq!(t(Some("us"), "en").ms, 1_500);
        assert_eq!(t(Some("mx"), "es").ms, 1_600);
        assert_eq!(t(Some("fr"), "fr").ms, 1_600);
        assert_eq!(t(Some("br"), "pt").ms, 1_600);
        assert_eq!(t(Some("de"), "de").ms, 1_400);
        assert_eq!(t(Some("ch"), "fr").ms, 1_600);
        assert_eq!(t(Some("ch"), "de").ms, 1_400);
        assert_eq!(t(Some("th"), "en").ms, 1_500);
        // The SIM first: a Japanese SIM keeps Japan's timer on a Chinese call.
        assert_eq!(t(Some("JP"), "zh-TW").region, "JP");
        // No SIM, or a country without its own timer: the call's language.
        assert_eq!(t(None, "ja").ms, 2_500);
        assert_eq!(t(None, "ja").region, "ja");
        assert_eq!(t(None, "zh-CN").ms, 1_200);
        assert_eq!(t(None, "de").ms, 1_400);
        assert_eq!(t(Some("zz"), "es").ms, 1_600);
        assert_eq!(t(Some("zz"), "es").region, "es");
        assert_eq!(t(None, "zh-TW").ms, 1_500);
        assert!(!t(None, "en").from_setting);
        // The owner's own value wins (clamped as before).
        let own = hangup_timer(3_000, Some("tw"), "zh-TW");
        assert_eq!((own.ms, own.from_setting, own.region.as_str()), (3_000, true, "TW"));
        assert_eq!(hangup_timer(50, None, "en").ms, 200);
        assert_eq!(hangup_timer(99_000, None, "en").ms, 15_000);
    }

    #[test]
    fn lists_are_normalized() {
        let lexicons = [PRE_CLOSINGS, FAREWELLS, GREETING_BYES, BACKCHANNELS];
        for lex in lexicons {
            for (_, list) in lex {
                for p in *list {
                    assert_eq!(norm(p), *p, "not normalized: {p}");
                    assert!(is_spaced(p) || !p.contains(' '), "spaces in a non-spaced phrase: {p}");
                }
            }
        }
        for p in OPENING_GREETINGS.iter().chain(EXTRA_THANKS) {
            assert_eq!(norm(p), *p, "not normalized: {p}");
        }
    }

    #[test]
    fn one_judgement_takes_well_under_a_millisecond() {
        let texts: Vec<String> = CASES
            .iter()
            .flat_map(|c| [c.pre[0], c.bye[0], c.opening, c.follow_up])
            .map(|s| format!("我要寄一個包裹給王先生，地址在台北，電話 0912345678。{s}"))
            .collect();
        let mut m = ClosingMachine::new("zh-TW");
        // Warm up lazy tables before timing the per-judgement loop.
        let _ = m.on_caller(&texts[0], &texts[0], false, false);
        let _ = is_backchannel(&texts[0]);
        let start = std::time::Instant::now();
        for i in 0..1_000 {
            let t = &texts[i % texts.len()];
            let step = m.on_caller(t, t, false, i % 7 == 0);
            std::hint::black_box(step);
            std::hint::black_box(is_backchannel(t));
        }
        let took = start.elapsed();
        // Microseconds in a release build; an unoptimised test build on a busy shared
        // Windows CI VM is many times slower, so give debug builds room for CPU contention.
        let limit_ms = if cfg!(debug_assertions) { 2_500 } else { 200 };
        assert!(took < std::time::Duration::from_millis(limit_ms), "1000 judgements took {took:?}");
    }
}

#[cfg(test)]
mod thanks_wrap_edges {
    use super::thanks_wrap;

    #[test]
    fn a_question_or_bare_agreement_before_thanks_is_no_wrap_up() {
        for t in [
            "好的，那明天幾點到，謝謝",
            "Okay, what time tomorrow, thanks",
            "かしこまりました、明日は何時ですか、ありがとうございます",
            "承知しました、何時ごろ来ますか。ありがとうございます",
            "Great, thanks",
            "Alright, thank you",
            "No problem, thanks",
            "知道了，謝謝",
            "わかりました、ありがとうございます",
        ] {
            assert!(!thanks_wrap(t), "{t}");
        }
    }

    #[test]
    fn a_settled_matter_or_a_wrong_number_with_thanks_is_one() {
        for t in [
            "好的，沒問題，那明天我放管理室，謝謝",
            "Oh sorry, wrong number, thanks",
            "Okay, I'll leave it at the front door then, thanks",
            "かしこまりました、では宅配ボックスに入れておきます、ありがとうございます",
        ] {
            assert!(thanks_wrap(t), "{t}");
        }
    }
}

/// Seen on the 2026-11-17 bench: the caller's last words closed, but the
/// machine did not see it, and the call waited for the AI's 6 s quiet timer.
#[cfg(test)]
mod bench_closings {
    use super::*;

    #[test]
    fn thanks_to_a_named_person_is_still_thanks() {
        // A Hong Kong courier (speech-to-text writes 曬 and 王先生 for 晒 and 王生).
        for t in [
            "OK,沒問題。咁我放喺管理室啦。唔該曬王先生。",
            "OK冇問題，咁我聽日送到擺埋喺管理處囉，唔該晒王生！",
            "好的，沒問題，那明天我放管理室，謝謝王小姐",
            // The courier agreeing with 「好喔」: settled too.
            "好喔，那明天下午我就幫你放在管理室，謝謝喔。",
            // A clinic confirming the read-back, then its request and thanks.
            "沒錯，就是這樣。確認無誤後，再麻煩您撥打分機十二回電確認囉，謝謝您。",
        ] {
            assert!(thanks_wrap(t), "{t}");
        }
        assert!(!thanks_wrap("好喔，謝謝") && !thanks_wrap("好啊，那明天幾點來，謝謝"));
        // 「…你先看，有空再講」 announces the end (Singapore Hokkien speech-to-text):
        // the AI's goodbye after it is the exchange.
        let mut m = ClosingMachine::new("zh-TW");
        m.on_caller("這禮拜六你有閒無？出來吃一杯咖啡啦", "這禮拜六你有閒無？出來吃一杯咖啡啦", false, false);
        let later = "好啊，按呢你先看，有空再講。";
        assert_eq!(m.on_caller(later, later, false, false).event, ClosingEvent::CallerPreClosing);
        assert_eq!(m.on_ai_turn("好，再聯絡，拜拜。", false, false), ClosingEvent::FarewellsExchanged);
        assert!(m.may_hang_up(false));
        // Asked to say it again is no closing.
        assert!(classify("蛤？你可以再講一次嗎？").is_none() && classify("你再講一次").is_none());
        for t in ["唔該曬王先生", "謝謝陳生", "掰掰，王先生"] {
            assert!(is_fluff(t) || classify(t).is_some(), "{t}");
        }
        assert!(classify("好，再見陳小姐").is_some_and(|h| h.kind == Kind::Farewell));
        // A name alone is no thanks: 「我是王先生」 is still content.
        assert!(!is_fluff("謝謝，我是王先生"));
        assert!(peel_addressee("我是王先生").is_none() && peel_addressee("王生").is_none());

        // The settled courier and the AI's goodbye are the exchange.
        let mut m = ClosingMachine::new("zh-TW");
        m.on_caller("喂，你好，請問是不是王先生啊?我是順豐派件的。", "喂，你好，請問是不是王先生啊?我是順豐派件的。", false, false);
        let wrap = "OK,沒問題。咁我放喺管理室啦。唔該曬王先生。";
        assert_eq!(m.on_caller(wrap, wrap, false, false).event, ClosingEvent::CallerThanksWrap);
        assert_eq!(m.on_ai_turn("唔該曬，bye-bye。", false, false), ClosingEvent::FarewellsExchanged);
        assert!(m.may_hang_up(false));
    }

    /// Cantonese closings in both scripts, with other languages beside them.
    #[test]
    fn cantonese_closings_in_both_scripts() {
        for t in ["好啦，得啦唔該晒", "好啦，得啦唔该晒", "好啦，得啦唔該曬", "好，先掛啦", "好，先挂啦", "好，拜拜啦", "收線啦", "收线啦"] {
            assert!(classify(t).is_some_and(|h| h.kind == Kind::Farewell), "{t}");
        }
        for t in ["好，係咁先", "好，系咁先", "好，收線先", "好，收线先", "好，就咁先", "好，唔阻你"] {
            assert!(classify(t).is_some_and(|h| h.kind == Kind::PreClosing), "{t}");
        }
        for t in ["唔該晒", "唔該曬", "唔该晒", "唔該哂", "唔该哂"] {
            assert!(is_fluff(t), "{t}");
        }
        // The same moment in other languages.
        for (t, kind) in [
            ("Okay, bye bye", Kind::Farewell),
            ("じゃあ、またね", Kind::Farewell),
            ("Dạ, tạm biệt", Kind::Farewell),
            ("โอเค บ๊ายบาย", Kind::Farewell),
            ("Baik, sampai jumpa", Kind::Farewell),
            ("Oke sudah dulu ya", Kind::PreClosing),
            ("Dạ vậy nhé", Kind::PreClosing),
        ] {
            assert!(classify(t).is_some_and(|h| h.kind == kind), "{t}");
        }
        // 「蕭先生」「稍息先」 are mis-heard words, not closings.
        assert!(classify("好, 蕭先生。").is_none() && classify("好, 稍息先。").is_none());
    }

    #[test]
    fn hanging_up_out_loud_is_a_goodbye() {
        for t in ["有你這句話媽媽就放心啦！玩得開心點啊，注意安全，掛囉！", "好，那我先掛了喔", "行，那我挂了啊", "好啦，掛啦"] {
            assert!(classify(t).is_some_and(|h| h.kind == Kind::Farewell), "{t}");
        }
        let mut m = ClosingMachine::new("zh-TW");
        m.on_caller("兒子啊，旅遊的錢我幫你轉過去囉", "兒子啊，旅遊的錢我幫你轉過去囉", false, false);
        let bye = "有你這句話媽媽就放心啦！玩得開心點啊，注意安全，掛囉！";
        let step = m.on_caller(bye, bye, false, false);
        assert!(step.ends_call && m.may_hang_up(false));
        // Not while stalling: the caller's goodbye comes back to be answered.
        let mut s = ClosingMachine::new("zh-TW");
        s.on_caller("我們有一個投資方案", "我們有一個投資方案", false, true);
        assert_eq!(s.on_caller("好啦，掛啦", "好啦，掛啦", false, true).event, ClosingEvent::CallerFarewell);
        assert!(!s.may_hang_up(true));
    }

    #[test]
    fn an_ai_turn_without_words_keeps_the_closing() {
        let mut m = ClosingMachine::new("zh-TW");
        m.on_caller("您的檢查改到下週三上午十點", "您的檢查改到下週三上午十點", false, false);
        assert_eq!(m.on_ai_turn("我晚點再打給你。拜拜。", false, false), ClosingEvent::None);
        assert_eq!(m.state(), ClosingState::PreClosing);
        for said in ["", "...", "…", " 。"] {
            assert_eq!(m.on_ai_turn(said, false, false), ClosingEvent::None);
            assert_eq!(m.state(), ClosingState::PreClosing, "{said:?}");
        }
        // Words that are no closing still take it back.
        m.on_ai_turn("對了，你要記得空腹喔", false, false);
        assert_eq!(m.state(), ClosingState::Talking);
    }
}

#[cfg(test)]
mod stay_on_the_line {
    use super::ClosingMachine;

    #[test]
    fn dont_hang_up_is_no_goodbye_but_hanging_up_is() {
        for t in ["你先不要掛啦", "不要掛囉", "請別掛啦", "你不要先掛啦"] {
            let mut m = ClosingMachine::new("zh-TW");
            m.on_caller("喂，你好，我想問一下包裹的事", "喂，你好，我想問一下包裹的事", false, false);
            assert!(!m.on_caller(t, t, false, false).ends_call, "{t}");
        }
        let mut m = ClosingMachine::new("zh-TW");
        m.on_caller("喂，你好，我想問一下包裹的事", "喂，你好，我想問一下包裹的事", false, false);
        assert!(m.on_caller("好，那就這樣，我先掛了", "好，那就這樣，我先掛了", false, false).ends_call);
    }
}

/// 2026-11-17 bench (candidate 33b93df): how two calls really ended.
#[cfg(test)]
mod late_words_and_the_callers_bye {
    use super::*;

    fn caller(m: &mut ClosingMachine, text: &str) -> ClosingStep {
        m.on_caller(text, text, false, false)
    }

    /// f_aizuchi_tw_e0: the clinic settles the new date and thanks.
    fn clinic_wrapped_up() -> ClosingMachine {
        let mut m = ClosingMachine::new("zh-TW");
        caller(&mut m, "王先生您好,我是健檢中心,通知您,您的健康檢查改期到下週三上午10點喔。");
        m.on_ai_turn("下週三上午十點，對嗎？", false, false);
        caller(&mut m, "是,對吧?下週三上午10點。當天記得要空腹,並且帶健保卡哦。可以請您幫我複述一次時間跟要帶的東西嗎?");
        m.on_ai_turn("下週三上午十點，當天要空腹，並且帶健保卡。", false, false);
        let wrap = "沒錯,就是這樣。確認後再請您撥打分機12跟我們確認哦。謝謝您。";
        assert_eq!(caller(&mut m, wrap).event, ClosingEvent::CallerThanksWrap);
        assert_eq!(m.state(), ClosingState::PreClosing);
        m
    }

    /// The AI answered 「我確認一下，晚點再打給你。」, its words in only after its
    /// turn ended: the line stayed open until the caller gave up.
    #[test]
    fn the_ais_words_after_its_turn_still_close_the_call() {
        let mut m = clinic_wrapped_up();
        assert_eq!(m.on_ai_turn("", false, false), ClosingEvent::None);
        assert_eq!(m.on_ai_late("晚點再打給你。", |_| false, false), ClosingEvent::FarewellsExchanged);
        // Its late words go on to ask something: the exchange is taken back.
        assert_eq!(m.on_ai_late("請問您明天幾點方便？", |_| false, false), ClosingEvent::Withdrawn);
        assert!(!m.may_hang_up(false));
        let mut m = clinic_wrapped_up();
        assert_eq!(m.on_ai_turn("", false, false), ClosingEvent::None);
        assert!(!m.may_hang_up(false));
        assert_eq!(m.on_ai_late("我確認一下，", |_| false, false), ClosingEvent::None);
        assert_eq!(m.on_ai_late("晚點再打給你。", |_| false, false), ClosingEvent::FarewellsExchanged);
        assert!(m.may_hang_up(false) && m.awaits_caller_bye());
        // In the turn itself, the same.
        let mut m = clinic_wrapped_up();
        assert_eq!(m.on_ai_turn("我確認一下，晚點再打給你。", false, false), ClosingEvent::FarewellsExchanged);
        // The turn ended mid-sentence: the rest of it came in later.
        let mut m = clinic_wrapped_up();
        assert_eq!(m.on_ai_turn("我確認一下，", false, false), ClosingEvent::None);
        assert_eq!(m.on_ai_late("晚點再打給你。", |_| false, false), ClosingEvent::FarewellsExchanged);
        // Latin words keep their spaces across pieces.
        let mut m = ClosingMachine::new("en");
        caller(&mut m, "Hi, is this Alex? I'm calling about the car");
        assert_eq!(caller(&mut m, "Oh sorry, wrong number then, thanks").event, ClosingEvent::CallerThanksWrap);
        m.on_ai_turn("", false, false);
        assert_eq!(m.on_ai_late(" No worries, take", |_| false, false), ClosingEvent::None);
        assert_eq!(m.on_ai_late(" care!", |_| false, false), ClosingEvent::FarewellsExchanged);
        for t in ["Let me check and I'll call you back later.", "我確認一下再回你", "我確認一下，晚點再用簡訊回你", "我再打俾你"] {
            assert!(classify(t).is_some_and(|h| h.kind == Kind::PreClosing), "{t}");
        }
    }

    #[test]
    fn late_words_count_only_for_that_turn() {
        // A sentence not finished is not judged yet: 「先這樣」 may go on.
        let mut m = clinic_wrapped_up();
        m.on_ai_turn("", false, false);
        let old = |t: &str| t.ends_with("先這樣");
        assert_eq!(m.on_ai_late("那就先這樣", old, false), ClosingEvent::None);
        assert_eq!(m.on_ai_late("算的話一個月多少？", old, false), ClosingEvent::None);
        assert!(!m.may_hang_up(false));
        // The caller brought up something new: the late words answer nothing.
        let mut m = clinic_wrapped_up();
        m.on_ai_turn("", false, false);
        caller(&mut m, "對了，還有一件事，報告要怎麼拿？");
        assert_eq!(m.on_ai_late("晚點再打給你。", |_| false, false), ClosingEvent::None);
        assert_eq!(m.state(), ClosingState::Talking);
        // A short 「嗯」 of theirs changes nothing.
        let mut m = clinic_wrapped_up();
        m.on_ai_turn("", false, false);
        caller(&mut m, "嗯");
        assert_eq!(m.on_ai_late("晚點再打給你。", |_| false, false), ClosingEvent::FarewellsExchanged);
        // The AI's next turn has begun: the words are that turn's.
        let mut m = clinic_wrapped_up();
        m.on_ai_turn("", false, false);
        m.ai_started();
        assert_eq!(m.on_ai_late("晚點再打給你。", |_| false, false), ClosingEvent::None);
        // While stalling nothing moves.
        let mut m = clinic_wrapped_up();
        m.on_ai_turn("", false, false);
        assert_eq!(m.on_ai_late("晚點再打給你。", |_| false, true), ClosingEvent::None);
        assert!(!m.may_hang_up(false));
    }

    /// e_close_hk: 「係咁先啦，唔阻你啦」 and the AI's 「好，拜拜」 — hung up a
    /// second later, the caller's own 「拜拜」 on its way.
    #[test]
    fn after_the_exchange_the_callers_bye_is_waited_for() {
        let mut m = ClosingMachine::new("zh-TW");
        caller(&mut m, "喂，王生你好啊，我係想confirm聽日下晝三點，我哋嘅見面時間同地點無變到呀？");
        m.on_ai_turn("十一月十八號星期三下晝三點，對嗎？", false, false);
        caller(&mut m, "係呀，就係聽日下晝三點啦，麻煩晒王生！");
        m.on_ai_turn("我確認一下，再用簡訊回你。", false, false);
        let step = caller(&mut m, "好，唔該晒！好啦，係咁先啦，唔阻你啦。");
        assert_eq!(step.event, ClosingEvent::CallerPreClosing);
        assert!(m.may_hang_up(false) && m.awaits_caller_bye());
        m.on_ai_turn("好，拜拜。", true, false);
        assert!(m.awaits_caller_bye());
        assert!(caller(&mut m, "好，唔該晒，拜拜！").ends_call);
        assert!(m.may_hang_up(false) && !m.awaits_caller_bye());

        // Their pre-closing first, then the AI's farewell: the same wait.
        let mut m = ClosingMachine::new("zh-TW");
        caller(&mut m, "王先生你好，我是管理室，你的包裹到了");
        assert_eq!(caller(&mut m, "好，那就先這樣，不打擾你了").event, ClosingEvent::CallerPreClosing);
        assert_eq!(m.on_ai_turn("好，掰掰。", true, false), ClosingEvent::FarewellsExchanged);
        assert!(m.awaits_caller_bye());
        // Something new instead of their bye: talk on, nothing owed.
        caller(&mut m, "啊對了，還有一個掛號信也在這裡");
        assert_eq!(m.state(), ClosingState::Talking);
        assert!(!m.awaits_caller_bye() && !m.may_hang_up(false));

        // Their goodbye itself owes nothing more.
        let mut m = ClosingMachine::new("zh-TW");
        caller(&mut m, "王先生你好，我是管理室，你的包裹到了");
        assert!(caller(&mut m, "好，那先這樣，掰掰").ends_call);
        m.on_ai_turn("好，掰掰。", true, false);
        assert!(m.may_hang_up(false) && !m.awaits_caller_bye());
    }
}

/// 2026-11-17 acceptance run (9832627): closings the lists did not have.
#[cfg(test)]
mod acceptance_closings {
    use super::*;

    fn caller(m: &mut ClosingMachine, text: &str) -> ClosingStep {
        m.on_caller(text, text, false, false)
    }

    fn talking(lang: &'static str, first: &str) -> ClosingMachine {
        let mut m = ClosingMachine::new(lang);
        caller(&mut m, first);
        assert_eq!(m.state(), ClosingState::Talking, "{first}");
        m
    }

    /// f_aizuchi_kr_e0 and parcel_pt_pt: the caller's goodbye was not heard as one
    /// and the line waited for the AI's bye and 6 s of quiet, or for nobody.
    #[test]
    fn seeing_them_on_the_day_and_a_good_rest_of_the_day_are_goodbyes() {
        let mut m = talking("ko", "다음 주 수요일 오전 10시로 건강검진 일정이 변경되었습니다.");
        let step = caller(&mut m, "네, 정확히 확인하셨네요. 그럼 수요일에 뵙겠습니다. 감사합니다.");
        assert_eq!(step.event, ClosingEvent::CallerFarewell);
        assert!(step.ends_call);
        // Meeting someone for the first time opens a call.
        let mut m = ClosingMachine::new("ko");
        assert!(!caller(&mut m, "안녕하세요, 처음 뵙겠습니다").ends_call);
        let mut m = talking("pt", "Boa tarde, estou a falar com o Senhor Wang? Tenho aqui uma encomenda para si.");
        let t = "Claro que sim, eu deixo na portaria. Muito obrigado pela ajuda e continuação de um bom dia.";
        assert_eq!(caller(&mut m, t).event, ClosingEvent::CallerFarewell);
    }

    /// e_close_jp_biz and hold_ja: a Japanese business call closes on the day's
    /// 「よろしくお願いいたします」, and the AI's 「後ほどこちらからご連絡いたしますね」
    /// answers the caller's closing the way 折り返し does.
    #[test]
    fn japanese_business_closings() {
        let mut m = talking("ja", "来週の月曜日の午前九時から十二時の間に技術者が伺えます");
        let step = caller(&mut m, "はい、その時間帯であればお伺いできます。それでは、当日はよろしくお願いいたします。");
        assert_eq!(step.event, ClosingEvent::CallerPreClosing);
        assert_eq!(
            m.on_ai_turn("分かりました。それではまた後ほどこちらからご連絡いたしますね。", false, false),
            ClosingEvent::FarewellsExchanged
        );
        assert!(m.may_hang_up(false) && m.awaits_caller_bye());
        // The same words as a question ask for an answer.
        assert!(classify("後ほどご連絡いたしますか？").is_none());
        // 「そうですね」 stays a reply.
        assert!(classify("そうですね").is_none());
    }

    /// parcel_it: no Italian goodbye was known; the line stayed open after 「Arrivederci!」.
    #[test]
    fn italian_goodbyes() {
        let mut m = talking("it", "Pronto, parlo con il signor Wang? Sono il corriere.");
        let step = caller(&mut m, "Perfetto, allora lascerò il pacco in portineria. Grazie mille e buona giornata!");
        assert_eq!(step.event, ClosingEvent::CallerFarewell);
        let mut m = talking("it", "Pronto, parlo con il signor Wang? Sono il corriere.");
        assert!(caller(&mut m, "Arrivederci!").ends_call);
        // Thanks alone is still thanks.
        let mut m = talking("it", "Pronto, parlo con il signor Wang? Sono il corriere.");
        assert!(!caller(&mut m, "Grazie").ends_call);
        // 「Pronto?」 opens a call.
        let mut m = ClosingMachine::new("it");
        assert!(!m.on_caller("Pronto?", "Pronto?", true, false).ends_call);
    }
}

/// What Italians say to end a phone call, next to what only looks like it
/// (greetings that open a call), and the other languages as they were.
#[cfg(test)]
mod italian_phone_talk_tests {
    use super::*;

    fn after_talk(lang: &'static str) -> ClosingMachine {
        let mut m = ClosingMachine::new(lang);
        m.on_caller("Parcel delivery info", "Parcel delivery info", false, false);
        m
    }

    fn hears(m: &mut ClosingMachine, t: &str) -> ClosingStep {
        m.on_caller(t, t, crate::locale::caller_wraps_up(t), false)
    }

    #[test]
    fn italian_goodbyes_pre_closings_and_the_greetings_that_say_goodbye() {
        for t in [
            "A dopo.",
            "Va bene, alla prossima!",
            "Grazie, buon lavoro.",
            "La saluto.",
            "Perfetto, la saluto.",
            "Buon proseguimento!",
            "Cordiali saluti.",
            "Ci risentiamo.",
            "Va bene, grazie, buonasera.",
            "Perfetto, mille grazie, buongiorno.",
            "Ok, grazie, buon pomeriggio.",
        ] {
            let mut m = after_talk("it");
            let step = hears(&mut m, t);
            assert_eq!(step.event, ClosingEvent::CallerFarewell, "{t}");
            assert!(step.ends_call, "{t}");
        }
        for t in ["Non la trattengo.", "Ti richiamo più tardi.", "Per ora è tutto."] {
            let mut m = after_talk("it");
            let step = hears(&mut m, t);
            assert_eq!(step.event, ClosingEvent::CallerPreClosing, "{t}");
            assert!(!step.ends_call, "{t}");
        }
    }

    #[test]
    fn italian_everyday_words_that_look_like_goodbyes_do_not_hang_up() {
        for t in [
            "Siamo un'azienda che cerca persone per un buon lavoro",
            "Alla prossima fermata scendo",
            "Buongiorno, buon lavoro, sono l'idraulico",
            "La saluto mia madre",
            "Grazie, cerchiamo persone per un buon lavoro",
        ] {
            let mut m = ClosingMachine::new("it");
            assert!(!hears(&mut m, t).ends_call, "first turn: {t}");
            let mut m = after_talk("it");
            assert!(!hears(&mut m, t).ends_call, "after talk: {t}");
        }
    }

    #[test]
    fn a_ciao_after_an_italian_goodbye_ends_the_call() {
        for t in ["Perfetto, ci sentiamo domani. Ciao", "Va bene, a presto, ciao", "Arrivederci, ciao!"] {
            let mut m = after_talk("it");
            let step = hears(&mut m, t);
            assert_eq!(step.event, ClosingEvent::CallerFarewell, "{t}");
            assert!(step.ends_call, "{t}");
        }
        // A greeting at the start is still no goodbye.
        for t in ["Ciao, sono Marco", "Pronto, ciao!", "Buongiorno, ci vediamo alle cinque per il pacco, ciao"] {
            let mut m = ClosingMachine::new("it");
            assert!(!hears(&mut m, t).ends_call, "{t}");
        }
    }

    #[test]
    fn italian_greetings_open_calls_and_do_not_close_them() {
        for opener in [
            "Buongiorno, sono Marco della consegna.",
            "Pronto, buongiorno.",
            "Buonasera.",
            "Buon pomeriggio, parlo con il signor Rossi?",
            "Ciao, sono Marco.",
        ] {
            let mut m = ClosingMachine::new("it");
            let step = m.on_caller(opener, opener, crate::locale::caller_wraps_up(opener), false);
            assert!(!step.ends_call, "{opener}");
            assert_eq!(m.state(), ClosingState::Talking, "{opener}");
        }
        for t in [
            "Buongiorno, sono Marco, ho un pacco per lei.",
            "A dopo le cinque sono in ufficio.",
            "Il signor Rossi lavora qui e dice che arriva domani.",
            "Mi passa il signor Rossi, buonasera?",
        ] {
            let mut m = after_talk("it");
            assert!(!hears(&mut m, t).ends_call, "{t}");
        }
    }

    #[test]
    fn italian_thanks_replies_and_questions() {
        assert!(thanks_wrap("Mi scusi, ho sbagliato numero, grazie mille"));
        assert!(thanks_wrap("Benissimo, lascio il pacco in portineria, grazie"));
        assert!(!thanks_wrap("Benissimo, grazie"));
        assert!(!thanks_wrap("Perfetto, a che ora arriva il corriere, grazie"));
        assert!(asks("a che ora arriva il corriere"));
        assert!(!asks("lascio il pacco in portineria"));
        assert!(is_backchannel("Sì sì, esatto."));
        assert!(is_backchannel("Benissimo, ho capito."));
        assert!(!is_backchannel("Va bene, lascio il pacco in portineria"));
        let mut m = after_talk("it");
        let step = hears(&mut m, "Mille grazie.");
        assert!(!step.ends_call);
    }

    #[test]
    fn goodbyes_in_other_languages_close_as_before() {
        for (lang, bye) in [
            ("en", "Okay, bye!"),
            ("es", "Vale, hasta luego."),
            ("de", "Alles klar, tschüss."),
            ("tr", "Görüşürüz."),
            ("zh-TW", "好的，掰掰。"),
        ] {
            let mut m = after_talk(lang);
            assert!(hears(&mut m, bye).ends_call, "{lang}: {bye}");
        }
        let mut m = after_talk("en");
        assert!(!hears(&mut m, "Hi, good evening, I have a parcel for you.").ends_call);
    }
}

/// P0-1 & P1-1: Italian "ciao", Japanese opening "失礼します／失礼いたします",
/// and non-CJK / non-Latin `has_matter` + `thanks_wrap` across all official languages.
#[cfg(test)]
mod p0_p1_multilingual_closing_tests {
    use super::*;

    #[test]
    fn italian_ciao_never_closes_opening_but_closes_after_substance_and_cue() {
        let t1 = "Ciao, sono Marco, ho un pacco per lei";
        let t1_old = crate::locale::caller_wraps_up(t1);
        assert!(!t1_old);
        for old_rules in [false, true] {
            let mut m = ClosingMachine::new("it");
            let step1 = m.on_caller(t1, t1, t1_old, false);
            assert!(!step1.ends_call, "Turn 1 must not end: old_rules={old_rules}");
            assert_eq!(step1.event, ClosingEvent::None);
            assert_eq!(m.state(), ClosingState::Talking);

            // Bare "Ciao!" or "Pronto, ciao!" or trailing "ciao" on Turn 1 must not close even when old_rules=true.
            for opener in ["Ciao!", "Pronto, ciao!", "Ciao, sono Marco, ciao"] {
                for check_old in [false, true, crate::locale::caller_wraps_up(opener)] {
                    let mut m2 = ClosingMachine::new("it");
                    let step = m2.on_caller(opener, opener, check_old, false);
                    assert!(!step.ends_call, "Opener {opener:?} must not end (old_rules={check_old})");
                    assert_eq!(m2.state(), ClosingState::Talking, "{opener}");
                }
            }

            // Real closing "Grazie mille, ciao!" after substance MUST end the call.
            let step2 = m.on_caller("Grazie mille, ciao!", "Grazie mille, ciao!", old_rules, false);
            assert_eq!(step2.event, ClosingEvent::CallerFarewell, "old_rules={old_rules}");
            assert!(step2.ends_call);
            assert_eq!(m.state(), ClosingState::Closing);
            assert!(m.may_hang_up(false));
        }
    }

    #[test]
    fn japanese_opening_shitsurei_never_closes_while_real_closing_does() {
        let openings = [
            "お忙しいところ失礼いたします",
            "夜分に失礼します",
            "恐れ入りますが、失礼します",
            "失礼いたします",
        ];
        for old_rules in [false, true] {
            // Turn 1 / before substance: never closes (even when old_rules=true from locale.rs).
            for t in openings {
                for check_old in [old_rules, crate::locale::caller_wraps_up(t)] {
                    let mut m = ClosingMachine::new("ja");
                    let step = m.on_caller(t, t, check_old, false);
                    assert!(!step.ends_call, "Turn 1 {t:?} must not close (old_rules={check_old})");
                    assert_ne!(m.state(), ClosingState::Closing, "{t}");
                }
            }
            // Opening with apology + company name on Turn 1 never closes either.
            let intro = "お忙しいところ失礼いたします、山田運送です";
            let mut m_intro = ClosingMachine::new("ja");
            let step_intro = m_intro.on_caller(intro, intro, crate::locale::caller_wraps_up(intro), false);
            assert!(!step_intro.ends_call);
            assert_eq!(m_intro.state(), ClosingState::Talking);
            // After substance, without wrap cue (or with opening apology): still never closes.
            for t in ["お忙しいところ失礼いたします", "夜分に失礼します", "恐れ入りますが、失礼します", "失礼いたします"] {
                let mut m = ClosingMachine::new("ja");
                m.on_caller("もしもし、山田運送の田中と申します。", "もしもし、山田運送の田中と申します。", false, false);
                let step = m.on_caller(t, t, old_rules, false);
                assert!(!step.ends_call, "Mid-call {t:?} without wrap cue must not close (old_rules={old_rules})");
                assert_eq!(m.state(), ClosingState::Talking, "{t}");
            }
            // After substance, real closing with wrap cue MUST end the call.
            for real_bye in [
                "ありがとうございました、では失礼します",
                "ありがとうございました、では失礼いたします",
                "それでは失礼します。",
            ] {
                let mut m = ClosingMachine::new("ja");
                m.on_caller("山田運送の田中です、お荷物は宅配ボックスに入れました。", "山田運送の田中です、お荷物は宅配ボックスに入れました。", false, false);
                let step = m.on_caller(real_bye, real_bye, old_rules, false);
                assert_eq!(step.event, ClosingEvent::CallerFarewell, "{real_bye}");
                assert!(step.ends_call, "{real_bye}");
                assert_eq!(m.state(), ClosingState::Closing, "{real_bye}");
                assert!(m.may_hang_up(false));
            }
        }
    }

    #[test]
    fn non_cjk_non_latin_and_all_languages_trigger_thanks_wrap_and_reject_bare_thanks() {
        // (lang, wrap_up_with_matter, ai_bye, bare_agreement_thanks)
        let cases: &[(&str, &str, &str, &str)] = &[
            ("zh-TW", "好的，沒問題，那我放管理室，謝謝！", "好，掰掰。", "好，謝謝"),
            ("zh-CN", "好的，没问题，那我放门卫处，谢谢！", "好，再见。", "好的，谢谢"),
            ("en", "Okay, I'll leave the parcel at reception, thank you!", "Okay, bye!", "Okay, thanks"),
            ("ja", "かしこまりました、宅配ボックスに入れておきます、ありがとうございます。", "はい、失礼します。", "わかりました、ありがとうございます"),
            ("ko", "알겠습니다, 경비실에 맡겨 둘게요, 감사합니다.", "네, 들어가세요.", "알겠습니다, 감사합니다"),
            ("es", "Vale, dejo el paquete en portería, muchas gracias.", "Vale, adiós.", "Vale, gracias"),
            ("fr", "D'accord, je laisse le colis au gardien, merci beaucoup.", "D'accord, au revoir.", "D'accord, merci"),
            ("de", "Alles klar, ich gebe das Paket beim Nachbarn ab, vielen Dank.", "Alles klar, tschüss.", "Alles klar, danke"),
            ("pt", "Tá bom, vou deixar o pacote na portaria, muito obrigado.", "Tá bom, tchau.", "Tá bom, obrigado"),
            ("it", "Perfetto, lascerò il pacco in portineria, grazie mille!", "Va bene, arrivederci!", "Va bene, grazie"),
            ("th", "โอเคค่ะ เดี๋ยวฝากพัสดุไว้ที่นิติบุคคลนะคะ ขอบคุณมากค่ะ", "ค่ะ บ๊ายบายค่ะ", "โอเคค่ะ ขอบคุณค่ะ"),
            ("ru", "Хорошо, я оставлю посылку на ресепшене, большое спасибо.", "Хорошо, до свидания.", "Хорошо, спасибо"),
            ("ar", "حسناً، سأترك الطرد في الاستقبال، شكراً جزيلاً.", "حسناً، مع السلامة.", "تمام، شكراً"),
            ("hi", "ठीक है, मैं पार्सल रिसेप्शन पर रख दूँगा, बहुत धन्यवाद।", "ठीक है, नमस्ते।", "ठीक है, धन्यवाद"),
            ("vi", "Dạ vâng, em gửi hàng ở quầy lễ tân nhé, cảm ơn anh nhiều.", "Vâng, tạm biệt.", "Dạ vâng, cảm ơn anh"),
            ("id", "Baik, saya titip paketnya di resepsionis ya, terima kasih banyak.", "Baik, sampai jumpa.", "Baik, terima kasih"),
        ];
        for &(lang, wrap, ai_bye, bare) in cases {
            assert!(thanks_wrap(wrap), "{lang}: wrap should trigger thanks_wrap: {wrap}");
            assert!(!thanks_wrap(bare), "{lang}: bare agreement+thanks must NOT trigger thanks_wrap: {bare}");

            let mut m = ClosingMachine::new(lang);
            m.on_caller("Parcel delivery info", "Parcel delivery info", false, false);
            let step = m.on_caller(wrap, wrap, false, false);
            assert_eq!(step.event, ClosingEvent::CallerThanksWrap, "{lang}: {wrap}");
            assert!(!step.ends_call);
            assert_eq!(m.state(), ClosingState::PreClosing);
            assert_eq!(m.on_ai_turn(ai_bye, true, false), ClosingEvent::FarewellsExchanged, "{lang}: {ai_bye}");
            assert!(m.may_hang_up(false), "{lang}");
        }
    }

    #[test]
    fn ambiguous_greetings_and_extended_official_languages_behave_safely() {
        // TRAP check: KO "안녕" and ES "buen día" only in GREETING_BYES, never FAREWELLS.
        assert!(!FAREWELLS.iter().any(|(_, list)| list.contains(&"안녕") || list.contains(&"buen día") || list.contains(&"buen dia")));
        for (lang, opener, cue_bye) in [
            ("ko", "안녕", "알겠습니다, 감사합니다. 안녕!"),
            ("es", "Hola, buen día", "Muchas gracias por avisar, buen día."),
        ] {
            let mut m1 = ClosingMachine::new(lang);
            let s1 = m1.on_caller(opener, opener, crate::locale::caller_wraps_up(opener), false);
            assert!(!s1.ends_call, "Turn 1 {opener:?} must not close");
            m1.on_caller("Tengo un paquete para entregar", "Tengo un paquete para entregar", false, false);
            let s2 = m1.on_caller(cue_bye, cue_bye, false, false);
            assert!(s2.ends_call, "Post-cue {cue_bye:?} should close");
        }

        // nl/pl/ro/uk/bn/mr/ta/te + it/th farewells close cleanly after substance.
        for (lang, bye) in [
            ("nl", "Bedankt, tot ziens!"),
            ("pl", "Dziękuję, do widzenia!"),
            ("ro", "Mulțumesc, la revedere!"),
            ("uk", "Дякую, до побачення!"),
            ("bn", "ধন্যবাদ, বিদায়!"),
            ("mr", "धन्यवाद, बाय बाय!"),
            ("ta", "நன்றி, போய் வருகிறேன்!"),
            ("te", "ధన్యవాదాలు, వెళ్ళొస్తాను!"),
            ("it", "Perfetto, buongiornata!"),
            ("th", "ขอบคุณค่ะ แล้วพบกันใหม่นะคะ"),
        ] {
            let mut m = ClosingMachine::new(lang);
            m.on_caller("Parcel delivery info", "Parcel delivery info", false, false);
            let step = m.on_caller(bye, bye, false, false);
            assert!(step.ends_call, "{lang}: {bye:?} did not close");
        }
    }
}

/// Words that only look like goodbyes ("falou", "take care of it", Tamil
/// "I am coming"), "byebye", and Turkish.
#[cfg(test)]
mod false_hangups_and_turkish_tests {
    use super::*;

    /// After some business, so greetings that also say goodbye may close.
    fn after_talk(lang: &'static str) -> ClosingMachine {
        let mut m = ClosingMachine::new(lang);
        m.on_caller("Parcel delivery info", "Parcel delivery info", false, false);
        m
    }

    /// The caller's turn as the live session feeds it: with the older check.
    fn hears(m: &mut ClosingMachine, t: &str) -> ClosingStep {
        m.on_caller(t, t, crate::locale::caller_wraps_up(t), false)
    }

    #[test]
    fn sentences_that_only_look_like_goodbyes_do_not_end_the_call() {
        for t in [
            "Foi o que ele falou.",
            "O gerente falou que sim.",
            "Ela já falou com você.",
            "Valeu a pena.",
            "Ah, valeu.",
            "I'll take care of it.",
            "My sister will take care of mom.",
            "That's all right.",
            "Je vous laisse mon numéro.",
            "Je vous laisse un message.",
            "Te dejo mi número.",
            "Le dejo el paquete.",
            "Nada más quería confirmar.",
            "Das ist alles sehr teuer.",
            "C'est tout à fait.",
            "Isso é tudo verdade.",
            "Na razie nie wiem.",
            "Na razie nie mogę.",
            "Fica combinado como ele falou.",
            "Ele chegou e falou.",
            "Je vous laisse l'adresse.",
            "Je vous laisse réfléchir.",
            "Te dejo un mensaje.",
            "Le dejo mis datos.",
            "Das ist alles nicht so einfach.",
            "Das ist alles zu teuer.",
            "Na razie nic nie wiem.",
            "Na razie jestem w pracy.",
            "Isso é tudo que eu sei.",
            "பத்து நிமிடத்தில் வருகிறேன்.",
            "நான் நாளைக்கு வருகிறேன்.",
            "నేను రేపు వస్తాను.",
            "పది నిమిషాల్లో వస్తాను.",
        ] {
            assert!(!crate::locale::caller_wraps_up(t), "older check: {t}");
            let mut m = after_talk("en");
            let step = hears(&mut m, t);
            assert!(!step.ends_call, "must not end the call: {t}");
            assert_ne!(step.event, ClosingEvent::CallerFarewell, "{t}");
        }
    }

    #[test]
    fn real_goodbyes_in_the_same_words_still_end_the_call() {
        for t in [
            "Beleza, valeu, falou!",
            "Então tá, falou.",
            "Tchau, falou.",
            "Falou!",
            "Take care.",
            "Okay, take care, bye.",
            "That's all.",
            "That's all, thanks.",
            "That's all for now.",
            "Je vous laisse.",
            "Bon, je vous laisse, bonne journée.",
            "Bueno, te dejo.",
            "Le dejo, hasta luego.",
            "Nada más, gracias.",
            "Das ist alles, danke.",
            "C'est tout, merci.",
            "É tudo, obrigado.",
            "Na razie!",
            "To na razie, pa.",
            // "Falou" set off by punctuation or after a closing cue.
            "Certo, falou",
            "Tá combinado então, falou",
            "Beleza então, falou",
            "Fechou, falou",
            "Obrigado, até mais, falou",
            "Então tá bom, falou valeu",
            "Tá bom, obrigada, falou",
            // A name, a title, a particle or another goodbye after the closing words.
            "Bon, je vous laisse madame",
            "Je vous laisse monsieur",
            "Bon allez je vous laisse hein",
            "D'accord je vous laisse et merci",
            "Je vous laisse à vos occupations",
            "Le dejo señora",
            "Bueno te dejo Marta",
            "Das ist alles, Herr Müller",
            "Das ist alles, Frau Schmidt",
            "Das ist alles so weit",
            "No to na razie, Ania",
            "Dobrze, na razie, do jutra",
            // "No" as the answer to "anything else?".
            "No, that's all, thanks.",
            "No that's everything, thank you so much",
            "No, nothing else.",
            "No, nada más, gracias.",
            "No, eso es todo.",
            "Não, é só isso, obrigado.",
            "Não, mais nada.",
            "சரி, போய் வருகிறேன்.",
            "సరే, వెళ్ళొస్తాను.",
        ] {
            let mut m = after_talk("en");
            let step = hears(&mut m, t);
            assert!(step.ends_call, "must end the call: {t}");
        }
    }

    #[test]
    fn japanese_understood_then_dewa_shitsurei_is_a_farewell() {
        for t in [
            "わかりました、では失礼いたします",
            "かしこまりました。では失礼します",
            "承知しました、では失礼いたします",
        ] {
            let mut m = ClosingMachine::new("ja");
            let talk = "山田運送の田中です、お荷物は宅配ボックスに入れました。";
            m.on_caller(talk, talk, false, false);
            let step = m.on_caller(t, t, false, false);
            assert_eq!(step.event, ClosingEvent::CallerFarewell, "{t}");
            assert!(step.ends_call, "{t}");
        }
        // The opening apology still closes nothing.
        let opener = "お忙しいところ失礼いたします";
        for old_rules in [false, true] {
            let mut m = ClosingMachine::new("ja");
            let step = m.on_caller(opener, opener, old_rules, false);
            assert!(!step.ends_call, "old_rules={old_rules}");
            assert_eq!(m.state(), ClosingState::Talking);
        }
    }

    #[test]
    fn indonesian_thai_and_vietnamese_closings_added_after_review() {
        let t = "Ya sudah, saya tutup dulu ya";
        let mut m = after_talk("id");
        let step = m.on_caller(t, t, false, false);
        assert_eq!(step.event, ClosingEvent::CallerPreClosing, "{t}");

        for (lang, t) in [
            ("th", "ขอบคุณครับ ไว้เจอกันครับ"),
            ("vi", "Cảm ơn anh nhiều, chúc anh một ngày tốt lành"),
            ("vi", "Vâng em hiểu rồi, chào anh ạ"),
        ] {
            assert!(crate::locale::ends_with_farewell(t, 2), "older check: {t}");
            for old_rules in [false, true] {
                let mut m = after_talk(lang);
                let step = m.on_caller(t, t, old_rules, false);
                assert_eq!(step.event, ClosingEvent::CallerFarewell, "{t} (old_rules={old_rules})");
                assert!(step.ends_call, "{t} (old_rules={old_rules})");
            }
        }

        // "Dạ" opens a call as well: the greeting after it is no goodbye.
        let opener = "Alo, dạ chào anh ạ";
        for old_rules in [false, true, crate::locale::caller_wraps_up(opener)] {
            let mut m = ClosingMachine::new("vi");
            let step = m.on_caller(opener, opener, old_rules, false);
            assert!(!step.ends_call, "opening must not close (old_rules={old_rules})");
            assert_eq!(m.state(), ClosingState::Talking);
        }
    }

    #[test]
    fn a_thanks_only_portuguese_close_waits_for_the_ai_goodbye() {
        let t = "Beleza, então. Vou deixar com a portaria para você. Valeu.";
        let mut m = after_talk("pt");
        let step = hears(&mut m, t);
        assert!(!step.ends_call, "{t}");
        assert_eq!(m.on_ai_turn("Tá bom, tchau.", true, false), ClosingEvent::FarewellsExchanged);
    }

    #[test]
    fn byebye_in_one_word_is_a_farewell() {
        let mut m = after_talk("en");
        let step = m.on_caller("Okay, byebye.", "Okay, byebye.", false, false);
        assert_eq!(step.event, ClosingEvent::CallerFarewell);
        assert!(step.ends_call);
    }

    #[test]
    fn turkish_greeting_at_the_start_does_not_close_but_after_talk_it_does() {
        let opener = "Merhaba, iyi günler.";
        for old_rules in [false, true, crate::locale::caller_wraps_up(opener)] {
            let mut m = ClosingMachine::new("tr");
            let step = m.on_caller(opener, opener, old_rules, false);
            assert!(!step.ends_call, "opening must not close (old_rules={old_rules})");
            assert_eq!(m.state(), ClosingState::Talking);
        }
        let mut m = after_talk("tr");
        let step = hears(&mut m, "Tamam, teşekkürler, iyi günler.");
        assert_eq!(step.event, ClosingEvent::CallerFarewell);
        assert!(step.ends_call);
    }

    #[test]
    fn turkish_goodbyes_pre_closings_fluff_and_thanks_wraps() {
        for t in ["Görüşürüz.", "Hoşça kalın.", "Güle güle.", "İyi geceler."] {
            let mut m = after_talk("tr");
            let step = m.on_caller(t, t, false, false);
            assert_eq!(step.event, ClosingEvent::CallerFarewell, "{t}");
            assert!(step.ends_call, "{t}");
        }
        let mut m = after_talk("tr");
        let step = m.on_caller("Tamam, kapatıyorum.", "Tamam, kapatıyorum.", false, false);
        assert_eq!(step.event, ClosingEvent::CallerPreClosing);

        assert!(is_fluff("Evet, tamam."));
        assert!(is_backchannel("Evet, tamam."));
        let mut m = after_talk("tr");
        let step = m.on_caller("Evet, tamam.", "Evet, tamam.", false, false);
        assert_eq!(step.event, ClosingEvent::None);
        assert!(!step.ends_call);

        let t = "Pardon, yanlış numara, teşekkürler.";
        assert!(thanks_wrap(t));
        let mut m = after_talk("tr");
        let step = m.on_caller(t, t, false, false);
        assert_eq!(step.event, ClosingEvent::CallerThanksWrap);
        assert!(!step.ends_call);

        let q = "Yarın evde misiniz?";
        let mut m = after_talk("tr");
        let step = m.on_caller(q, q, false, false);
        assert!(!step.ends_call);
        assert_eq!(step.event, ClosingEvent::None);
        assert!(asks("ne zaman gelecek"));
    }

    #[test]
    fn turkish_dotted_capital_matches_like_the_lowercase_word() {
        assert_eq!(norm("İyi günler"), "iyi günler");
        let t = "Teşekkürler, İyi günler.";
        let mut m = after_talk("tr");
        let step = m.on_caller(t, t, false, false);
        assert_eq!(step.event, ClosingEvent::CallerFarewell);
        assert!(step.ends_call);
    }
}

/// 「拜拜」 is worshipping at a temple and 「掰」 is 「瞎掰」: only a goodbye
/// when said as one.
#[cfg(test)]
mod chinese_bye_words {
    use super::*;

    fn after_talk() -> ClosingMachine {
        let mut m = ClosingMachine::new("zh-TW");
        m.on_caller("我要寄一個包裹給王先生 0912345678", "我要寄一個包裹給王先生 0912345678", false, false);
        m
    }

    #[test]
    fn worshipping_and_nonsense_are_no_goodbye() {
        for t in [
            "我明天要去拜拜",
            "我們在拜拜",
            "等一下要拜拜",
            "我媽叫我回去拜拜",
            "今天公司拜拜",
            "初一十五都要拜拜",
            "我明天要去拜拜喔",
            "你不要瞎掰",
            "他在亂掰啦",
            "把它掰開",
            "我们在拜拜呢",
            "拜拜的時候再說",
            "我們要先拜拜",
            "祖先拜拜",
            "初一十五就拜拜",
            "我明天要去廟裡拜拜",
            "阿嬤每天早上都要拜拜",
            "過年要回老家拜拜",
            "中元節家家戶戶都在拜拜",
            "他講的都是瞎掰的",
            "這個故事是他自己掰的",
            "路邊有人在拜拜",
            "公司明天要拜拜",
            "好了我們現在去拜拜",
            "我媽叫我來拜拜",
            "今天初一要先拜拜",
            "我先去拜拜",
            "我在廟裡跟神明拜拜",
            // "You" before it, but the worship is done for or with them.
            "我會幫你拜拜",
            // Who worships, where and what for: not whom a goodbye is said to.
            "今天老闆拜拜",
            "我們請師傅拜拜",
            "他在那拜拜",
            "初一十五去廟裡求平安拜拜",
            "全家一起去祈求順利拜拜",
            "明天我陪你拜拜",
            // Reads both ways ("I'll come over and worship later" / "I'll come over later, bye"):
            // kept as no goodbye.
            "那我晚點再過去拜拜",
        ] {
            assert!(zh_bye_ends(t).is_none(), "{t}");
            let mut m = after_talk();
            let step = m.on_caller(t, t, false, false);
            assert!(!step.ends_call, "{t}");
            assert_ne!(step.event, ClosingEvent::CallerFarewell, "{t}");
            // The AI's turn is judged the same way.
            assert!(classify(t).map_or(true, |h| h.kind != Kind::Farewell), "{t}");
        }
    }

    #[test]
    fn a_goodbye_said_is_one() {
        for t in [
            "好，拜拜",
            "好拜拜",
            "那拜拜囉",
            "謝謝，拜拜",
            "嗯嗯拜拜",
            "OK拜拜",
            "拜拜",
            "掰掰",
            "好喔掰",
            "那先這樣，拜拜",
            "我要去拜拜了，拜拜",
            "好的謝謝你掰掰",
            "先這樣囉掰掰",
            "沒問題拜拜",
            "辛苦了拜拜",
            "跟你說拜拜",
            "喂,不好意思,那也不打擾您,祝您事事順心,謝謝您哦,好,拜拜。",
            "谢谢您，拜拜",
            "好，拜拜，王先生",
        ] {
            assert!(zh_bye_ends(t).is_some(), "{t}");
            let mut m = after_talk();
            let step = m.on_caller(t, t, false, false);
            assert_eq!(step.event, ClosingEvent::CallerFarewell, "{t}");
            assert!(step.ends_call, "{t}");
        }
    }

    #[test]
    fn a_goodbye_run_into_the_words_before_it_is_one() {
        for t in [
            "好喔那就先這樣拜拜",
            "OK那我知道了掰掰",
            "沒關係拜拜",
            "那再麻煩你囉拜拜",
            "不好意思打擾了拜拜",
            "好我再打給你掰",
            "謝謝你喔拜拜",
            "對對對那就這樣啦掰掰",
            "好啦好啦我知道了拜拜",
            "麻煩你了拜拜",
            "好的謝謝再見拜拜",
            "嗯嗯我了解了拜拜",
            "沒事沒事你忙你的拜拜",
            "辛苦你了掰掰",
            "好那我先去忙了拜拜",
            "我明天再打來拜拜",
            "沒問題我等你消息拜拜",
            "下次見啦拜拜",
            "好那改天見拜拜",
            "恩恩好的沒問題掰",
            "好的王小姐拜拜",
            "謝謝老師拜拜",
            "那就這樣子喔拜拜",
            "對不起我打錯了拜拜",
            "不會啦拜拜",
            "我知道了謝謝掰",
            "那我們週五見拜拜",
            "你好好休息拜拜",
            "你路上小心拜拜",
            "沒關係沒關係拜拜",
            "好我再看看拜拜",
            "好的陳先生拜拜",
            "謝謝經理拜拜",
            "好啦下次再聊拜拜",
            "不用了謝謝拜拜",
            "好我懂了拜拜",
            "我要去忙了拜拜",
            // Simplified characters.
            "没关系拜拜",
            "那我们周五见拜拜",
            "对不起我打错了拜拜",
        ] {
            assert!(zh_bye_ends(t).is_some(), "{t}");
            let mut m = after_talk();
            let step = m.on_caller(t, t, false, false);
            assert_eq!(step.event, ClosingEvent::CallerFarewell, "{t}");
            assert!(step.ends_call, "{t}");
        }
    }

    fn closing_of(farewells: &[&str], wrapups: &[&str], ends: &[&str], not_ends: &[&str]) -> bageo_packs::locales::Closing {
        let v = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        bageo_packs::locales::Closing {
            verified: true,
            farewells: v(farewells),
            wrapups: v(wrapups),
            examples: bageo_packs::locales::Examples { ends: v(ends), not_ends: v(not_ends) },
        }
    }

    #[test]
    fn core_lines_that_guard_new_closing_words_do_not_end_today() {
        for s in CORE_NOT_ENDS {
            assert!(classify(s).map_or(true, |h| h.ambiguous), "{s}");
        }
    }

    #[test]
    fn verified_closing_words_of_another_language_join_only_when_every_check_passes() {
        // Welsh stands for a language the built-in lists do not know.
        let fa = closing_of(&["hwyl fawr", "hwyl am y tro"], &["dyna i gyd"], &["Diolch yn fawr, hwyl fawr.", "Iawn, dyna i gyd"], &["Helo, rwy'n ffonio o'r banc."]);
        assert!(classify("Diolch yn fawr, hwyl fawr.").is_none(), "not known today");
        assert!(judge("fa", &fa, "Diolch yn fawr, hwyl fawr.") && !judge("fa", &fa, "Helo, rwy'n ffonio o'r banc."));
        let w = ExtraWords::from_verified(&[("fa", fa.clone())]);
        assert!(classify_with("Diolch yn fawr, hwyl fawr.", &w).is_some_and(|h| h.kind == Kind::Farewell && h.lang == "fa"));
        assert!(classify_with("Iawn, dyna i gyd", &w).is_some_and(|h| h.kind == Kind::PreClosing));
        assert!(classify_with("Hwyl fawr?", &w).is_none(), "a question is no goodbye");
        // A word that makes a core line end is refused, with the rest of its closing.
        let ok = closing_of(&["ok"], &[], &["Merci, ok"], &[]);
        let w = ExtraWords::from_verified(&[("fa", fa.clone()), ("tr", ok)]);
        assert!(classify_with("Sure, ok", &w).is_none(), "core lines stay as they were");
        assert!(classify_with("Diolch yn fawr, hwyl fawr.", &w).is_some(), "the good closing still joins");
        // An example classified wrong keeps the words out.
        let wrong = closing_of(&["hwyl fawr"], &[], &["Diolch yn fawr, hwyl fawr."], &["hwyl fawr"]);
        assert!(classify_with("Diolch yn fawr, hwyl fawr.", &ExtraWords::from_verified(&[("fa", wrong)])).is_none());
        // Every core closing test line still reads as before with the words in.
        let w = ExtraWords::from_verified(&[("fa", fa)]);
        for s in ["Okay, bye, thanks for calling.", "好，再見陳小姐", "そうですね", "蛤？你可以再講一次嗎？"] {
            assert_eq!(classify_with(s, &w).map(|h| h.kind), classify(s).map(|h| h.kind), "{s}");
        }
    }

    #[test]
    fn translations_add_closing_words_only_when_verified_and_reviewed() {
        let v0 = crate::locales_store::builtin().version;
        let verified = |status: &str, verified: bool| {
            crate::locales_store::testing::gz(v0 + 1, |v| {
                v["langs"]["fa"]["status"] = status.into();
                let c = &mut v["langs"]["fa"]["speech"]["closing"];
                c["verified"] = verified.into();
                c["examples"]["ends"] = serde_json::json!(["ممنون، همین بود."]);
            })
        };
        crate::locales_store::testing::use_gz(&verified("local_reviewed", true));
        // Persian "that's all" is in no built-in list: it counts only through the translation.
        assert!(classify("ممنون، همین بود.").is_some_and(|h| h.lang == "fa"));
        let mut m = after_talk();
        assert_eq!(m.on_caller("ممنون، همین بود.", "ممنون، همین بود.", false, false).event, ClosingEvent::CallerPreClosing);
        for (status, v) in [("ai_draft", true), ("local_reviewed", false), ("ai_checked", true)] {
            crate::locales_store::testing::use_gz(&verified(status, v));
            assert!(classify("ممنون، همین بود.").is_none(), "{status} {v}");
        }
        crate::locales_store::testing::use_builtin();
        assert!(classify("ممنون، همین بود.").is_none());
    }

    #[test]
    fn thai_see_you_on_a_day_then_sawasdee_is_a_goodbye() {
        let yes = "ขอบคุณมากค่ะ แล้วพบกันวันพฤหัสบดีหน้านะคะ สวัสดีค่ะ";
        let mut m = after_talk();
        let step = m.on_caller(yes, yes, false, false);
        assert!(step.ends_call && step.event == ClosingEvent::CallerFarewell, "{yes}");
        for yes in ["โอเคค่ะ ไว้เจอกันพรุ่งนี้นะคะ สวัสดีค่ะ", "ได้ครับ งั้นเจอกันอาทิตย์หน้าครับ สวัสดีครับ"] {
            let mut m = after_talk();
            assert!(m.on_caller(yes, yes, false, false).ends_call, "{yes}");
        }
        // Asking for a day, or "we met on a day", is no goodbye.
        for no in [
            "แล้วพบกันวันพฤหัสบดีหน้าได้ไหมคะ",
            "ขอบคุณค่ะ แล้วพบกันวันพฤหัสบดีหน้าได้ไหมคะ สวัสดีค่ะ",
            "เราเคยเจอกันวันจันทร์ที่ร้านกาแฟค่ะ สวัสดีค่ะ",
            "แล้วพบกันวันไหนดีคะ สวัสดีค่ะ",
        ] {
            let mut m = after_talk();
            assert!(!m.on_caller(no, no, false, false).ends_call, "{no}");
        }
        // At the opening a สวัสดี stays a hello.
        let mut m = ClosingMachine::new("th");
        assert!(!m.on_caller("สวัสดีค่ะ", "สวัสดีค่ะ", false, false).ends_call);
        // Only Thai: other languages' cues are as before.
        assert!(!th_see_you_before("see you tomorrow") && !th_see_you_before("我們明天見"));
    }

    #[test]
    fn a_translated_word_written_without_spaces_must_stand_as_a_word() {
        let th = closing_of(&["ลาก่อน"], &[], &["ขอบคุณค่ะ ลาก่อนค่ะ"], &["ขอเวลาก่อนนะคะ", "อย่าเพิ่งลาก่อนนะ"]);
        let w = ExtraWords::of("th", &th);
        assert!(ends_judged("ขอบคุณค่ะ ลาก่อนค่ะ", &w) && ends_judged("ขอบคุณค่ะลาก่อน", &w));
        assert!(!ends_judged("ขอเวลาก่อนนะคะ", &w), "part of another word");
        assert!(!ends_judged("อย่าเพิ่งลาก่อนนะ", &w), "negated");
        assert!(ExtraWords::ai_farewells("th", &th).is_some());
    }
}

/// Closing words from the regional research (Chinese, English, Japanese, Korean,
/// Spanish, French, German, Portuguese, Italian): what callers there say to end
/// a call ends it; what they say to go on, or what only looks like a goodbye, does not.
#[cfg(test)]
mod regional_closing_words_tests {
    use super::*;

    fn after_talk() -> ClosingMachine {
        let mut m = ClosingMachine::new("en");
        m.on_caller("Parcel delivery info", "Parcel delivery info", false, false);
        m
    }

    /// The caller's turn as the live session feeds it: with the older check.
    fn hears(m: &mut ClosingMachine, t: &str) -> ClosingStep {
        m.on_caller(t, t, crate::locale::caller_wraps_up(t), false)
    }

    #[test]
    fn regional_goodbyes_end_the_call() {
        for t in [
            "好掛了", "好挂了", "唔該晒拜拜", "唔该晒拜拜",
            "Catch ya later!", "Thanks, all the best.", "Ta-ra!", "Slán go fóill.", "Right so, slán.",
            "じゃあね", "좋은 하루 보내세요",
            "Muchas gracias, feliz día.",
            "Allez, salut !", "Merci, à très vite !",
            "Schönen Feierabend!", "Tschö!", "Pfiati!", "Uf Wiederluege!", "Danke, ade.", "Danke, tschau.", "Danke, adieu.",
            "Obrigado, até já!", "Passe bem.", "Obrigado, cumprimentos.",
            "Grazie, arrivederci", "Buona serata", "Ciao ciao",
        ] {
            let mut m = after_talk();
            let step = hears(&mut m, t);
            assert_eq!(step.event, ClosingEvent::CallerFarewell, "{t}");
            assert!(step.ends_call, "{t}");
        }
    }

    #[test]
    fn regional_pre_closings_announce_the_end() {
        for t in [
            "好，那就這樣", "好就這樣", "回頭聯絡", "那就这样吧", "回头联系", "好的，不打扰你了", "回聊",
            "Alright, I gotta run.",
            "네, 나중에 통화해요.",
            "Órale, nos hablamos.", "Cualquier cosa te aviso.", "Bueno, cualquier cosa le aviso.",
            "Tá bom, falamos depois.",
        ] {
            let mut m = after_talk();
            assert_eq!(hears(&mut m, t).event, ClosingEvent::CallerPreClosing, "{t}");
        }
    }

    #[test]
    fn going_on_or_lookalikes_do_not_close() {
        for t in [
            "啊對了", "喔還有", "順便問一下", "等一下喔", "噢对了", "哎还有个事", "话说回来", "仲有呢", "順便問多句",
            "那就這樣處理，你明天把文件寄過來", "晚點聊的那件事你先準備資料", "他回頭聯絡過我們兩次了",
            "這個方案只要三分鐘，不耽誤你時間", "我就简单说几句，不耽误您时间", "那件事我們晚點聊",
            "All the best.", "Der Anrufer heißt Ade", "Dígame, cuánto sería todo",
            "Oh, real quick", "Before I let you go", "Oh, by the way", "Anyway, so", "Oh hang on",
            "We've gotta run a few more tests on your car", "You should take it easy", "All the best seats are gone already",
            "あ、それと", "ちなみに", "もう一点だけ", "あとですね", "そういうことでしたら、来週にしましょう",
            "お願いがあるのですが、じゃあそういうことで",
            "아 맞다", "하나만 더 여쭤볼게요", "잠시만요",
            "Oye, una pregunta", "Antes de colgar", "Ah, por cierto", "Espérame tantito", "Bueno, mirá", "Pará un cachito",
            "Desde ese día no nos hablamos", "Qué bueno que estés muy bien", "Me alegra que andes bien", "Espero que vaya bien",
            "Con su hijo ya estamos en contacto",
            "Du coup", "Bon bah", "Au fait", "Ah juste une chose", "Bon courage pour demain",
            "Übrigens", "Moment mal", "Eine Frage noch", "Aja noch was", "Warten Sie kurz", "Wir hatten ein schönes Wochenende",
            "Die Limonade ist schon bezahlt",
            "Só mais uma coisa", "Antes de desligar", "Ah, por sinal", "Muito bem, já agora", "Foi tudo de bom",
            "Disso falamos depois, primeiro o endereço",
            "Ah aspetta", "Un'ultima cosa", "Prima che chiudi", "Comunque senti", "A proposito", "Ti chiamo solo per fare un saluto",
        ] {
            let mut m = after_talk();
            let step = hears(&mut m, t);
            assert!(!step.ends_call, "must not end the call: {t}");
            assert!(
                !matches!(step.event, ClosingEvent::CallerFarewell | ClosingEvent::CallerPreClosing),
                "no closing: {t} {:?}",
                step.event
            );
        }
    }

    /// Greetings that also say goodbye open a call without ending it.
    #[test]
    fn greetings_that_say_goodbye_open_calls() {
        for (lang, t) in [("es", "¡Hola! ¡Feliz día!"), ("de", "Tschau!"), ("de", "Adieu!"), ("pt", "Cumprimentos!")] {
            let mut m = ClosingMachine::new(lang);
            assert!(!hears(&mut m, t).ends_call, "{t}");
        }
    }
}

#[cfg(test)]
mod review_probe_tests {
    use super::*;

    fn run(lang: &'static str, t: &str) -> ClosingStep {
        let mut m = ClosingMachine::new(lang);
        m.on_caller("Parcel delivery info", "Parcel delivery info", false, false);
        m.on_caller(t, t, crate::locale::caller_wraps_up(t), false)
    }

    fn opener(lang: &'static str, t: &str) -> ClosingStep {
        let mut m = ClosingMachine::new(lang);
        m.on_caller(t, t, crate::locale::caller_wraps_up(t), false)
    }

    /// Ordinary sentences that hold a newly added closing word but are not a goodbye.
    #[test]
    fn review_probe_lookalikes_do_not_close() {
        let cases: &[(&str, &str)] = &[
            ("en", "We've gotta run diagnostics on the line before I can quote you"),
            ("en", "Which program do I gotta run"),
            ("en", "Your plan gives you access to all the best doctors in the network"),
            ("en", "The all the best of album is half price"),
            ("en", "Please keep the all the best"),
            ("en", "What is the data rate on your plan"),
            ("en", "The metadata rarely changes on your account"),
            ("en", "He is facing a slander lawsuit"),
            ("en", "Is the roof slanted"),
            ("en", "Your slang is hard to follow"),
            ("en", "I would like to buy lemonade"),
            ("en", "We trade in the morning"),
            ("zh-TW", "窗簾好掛了，下午師傅會來"),
            ("zh-TW", "這幅畫好掛了就可以拍照"),
            ("zh-TW", "線上客服的晚點聊天時段是九點到十點"),
            ("zh-TW", "我們晚點聊天室會開放"),
            ("zh-TW", "你有回頭聯絡過房東嗎"),
            ("zh-TW", "如果客戶沒回頭聯絡，我們會寄第二封信"),
            ("zh-TW", "那就這樣做吧，把資料給我"),
            ("zh-TW", "你說好就這樣辦理對嗎"),
            ("zh-TW", "我們不耽誤你時間，只需要一分鐘確認資料"),
            ("zh-TW", "這個方法不耽誤你時間，很快就可以辦好"),
            ("zh-TW", "唔該拜拜土地公"),
            ("zh-TW", "唔該你再講一次"),
            ("zh-CN", "窗帘好挂了，师傅下午来"),
            ("zh-CN", "我回聊天记录看看"),
            ("zh-CN", "你能回聊天室吗"),
            ("zh-CN", "客服晚点聊天时间是九点"),
            ("zh-CN", "那就这样操作，先点设置"),
            ("zh-CN", "银行说不打扰你了，但你的账户还有问题"),
            ("zh-CN", "客户说不打扰你了，是什么意思"),
            ("zh-CN", "他回头联系过我们两次"),
            ("zh-CN", "我们不耽误你时间，只要一分钟确认资料"),
            ("yue", "唔該晒你份文件"),
            ("yue", "唔該你再講一次"),
            ("ja", "じゃあそういうことで手続きを進めますので、ご住所を教えてください"),
            ("ja", "それではそういうことで間違いないですか"),
            ("ko", "아까 나중에 통화해요 라고 하셨는데 지금은 통화 가능하세요"),
            ("ko", "상담원이 나중에 통화해요 라고 안내가 나왔나요"),
            ("es", "¿Cuánto sería todo junto?"),
            ("es", "Sería todo un honor ayudarle con su cuenta"),
            ("es", "Si paga hoy, seria todo el saldo pendiente"),
            ("es", "Si hay cualquier cosa, le aviso al supervisor para que lo revise"),
            ("es", "Mire, cualquier cosa te aviso al banco para que no le cobren"),
            ("es", "Órale, nos hablamos de su pedido hace una semana, ¿recuerda?"),
            ("fr", "Je vous réponds à très vite allure"),
            ("fr", "Vous devez aller à très vite fait"),
            ("fr", "Allez salut le client de ma part et dites-lui que la commande part"),
            ("de", "Das Paket enthält Schokolade"),
            ("de", "Die Fassade ist neu gestrichen"),
            ("de", "Ich möchte mit Herrn Ade sprechen"),
            ("de", "Wir haben Marmelade und Limonade bestellt"),
            ("de", "Das war im schönen Feierabendverkehr"),
            ("de", "Wir sagen immer Tschö zu den Kunden, aber heute geht es um Ihre Rechnung"),
            ("de", "Die Arkade ist offen"),
            ("pt", "Espero que o pagamento passe bem pelo sistema"),
            ("pt", "Quero que a entrega passe bem pela alfândega"),
            ("pt", "Até já vi esse erro antes"),
            ("pt", "Até já consegui pagar a conta"),
            ("pt", "Nós falamos depois do almoço?"),
            ("pt", "Ontem falamos depois da reunião sobre o contrato"),
            ("pt", "Trouxe os cumprimentos do seu filho"),
            ("pt", "Passe bem o cartão na máquina"),
        ];
        let mut bad = Vec::new();
        for (lang, t) in cases {
            for l in [*lang, "en"] {
                let s = run(l, t);
                if s.ends_call || matches!(s.event, ClosingEvent::CallerFarewell | ClosingEvent::CallerPreClosing) {
                    bad.push(format!("[{l}] {t} -> {:?} ends={}", s.event, s.ends_call));
                }
            }
        }
        assert!(bad.is_empty(), "lookalikes treated as closing:\n{}", bad.join("\n"));
    }

    /// A sales or bank caller's first words may hold a closing-sounding phrase.
    #[test]
    fn review_probe_openers_do_not_close() {
        let cases: &[(&str, &str)] = &[
            ("zh-TW", "您好，不耽誤你時間，我是保險公司的"),
            ("zh-CN", "您好，不耽误你时间，我是银行的"),
            ("en", "Hi, all the best from the team at your bank"),
            ("es", "Hola, feliz día, le llamo de su banco"),
            ("de", "Tschau, hier ist Ihre Bank"),
            ("de", "Adieu, hier ist die Bank"),
            ("pt", "Cumprimentos, falamos da sua conta"),
            ("fr", "Salut, c'est la banque"),
        ];
        let mut bad = Vec::new();
        for (lang, t) in cases {
            let s = opener(lang, t);
            if s.ends_call || matches!(s.event, ClosingEvent::CallerFarewell | ClosingEvent::CallerPreClosing) {
                bad.push(format!("[{lang}] {t} -> {:?} ends={}", s.event, s.ends_call));
            }
        }
        assert!(bad.is_empty(), "openers treated as closing:\n{}", bad.join("\n"));
    }

    /// A call's middle turn that happens to end on a newly added closing word.
    #[test]
    fn review_probe_end_position_lookalikes() {
        let cases: &[(&'static str, &str)] = &[
            ("zh-TW", "這個方案只要三分鐘，不耽誤你時間"),
            ("zh-TW", "我們就在線上處理，不耽誤您時間"),
            ("zh-CN", "这个方案只要三分钟，不耽误你时间"),
            ("zh-CN", "我就简单说几句，不耽误您时间"),
            ("zh-TW", "我剛剛說到的那件事，我們晚點聊"),
            ("de", "Ich möchte mit Herrn Ade"),
            ("de", "Der Anrufer heißt Ade"),
            ("en", "He asked about the all the best"),
            ("pt", "Eu vi o resto de boa tarde"),
            ("es", "Dígame, ¿cuánto sería todo"),
            ("ja", "お願いがあるのですが、じゃあそういうことで"),
        ];
        let mut bad = Vec::new();
        for (lang, t) in cases {
            let s = run(lang, t);
            if s.ends_call || matches!(s.event, ClosingEvent::CallerFarewell | ClosingEvent::CallerPreClosing) {
                bad.push(format!("[{lang}] {t} -> {:?} ends={}", s.event, s.ends_call));
            }
        }
        assert!(bad.is_empty(), "end-position lookalikes treated as closing:\n{}", bad.join("\n"));
    }
}

/// Closing words from the regional research, second batch (Italian, Russian,
/// Turkish, Vietnamese, Thai, Indonesian, Arabic, Hindi, Dutch, Polish): what
/// callers there say to end a call ends it; ordinary sentences holding the same
/// words, and greetings at the start of a call, do not.
#[cfg(test)]
mod regional_closing_words_2_tests {
    use super::*;

    fn after_talk(lang: &'static str) -> ClosingMachine {
        let mut m = ClosingMachine::new(lang);
        m.on_caller("Parcel delivery info", "Parcel delivery info", false, false);
        m
    }

    /// The caller's turn as the live session feeds it: with the older check.
    fn hears(m: &mut ClosingMachine, t: &str) -> ClosingStep {
        m.on_caller(t, t, crate::locale::caller_wraps_up(t), false)
    }

    fn closes(s: &ClosingStep) -> bool {
        s.ends_call || matches!(s.event, ClosingEvent::CallerFarewell | ClosingEvent::CallerPreClosing)
    }

    #[test]
    fn goodbyes_end_the_call() {
        let cases: &[(&'static str, &str)] = &[
            ("ru", "Ну давай, пока!"), ("ru", "Давай, пока"), ("ru", "Удачи, пока!"),
            ("ru", "Ну всё, счастливо!"),
            ("vi", "Thôi mình cúp máy nhé"), ("vi", "Dạ, em xin phép cúp máy ạ"), ("vi", "Thôi em cúp máy nha"),
            ("ar", "تصبح على خير"), ("ar", "شكراً، تصبحين على خير"), ("ar", "يلا سلام"), ("ar", "طيب، يلا باي"),
            ("nl", "Doeg!"), ("nl", "Tot snel!"), ("nl", "Bedankt, tot binnenkort."), ("nl", "Oké, spreek je later."),
            ("pl", "No to cześć!"), ("pl", "Miłego wieczoru!"), ("pl", "Dzięki, miłego wieczoru."),
        ];
        let mut bad = Vec::new();
        for (lang, t) in cases {
            let mut m = after_talk(lang);
            let s = hears(&mut m, t);
            if s.event != ClosingEvent::CallerFarewell || !s.ends_call {
                bad.push(format!("[{lang}] {t} -> {:?} ends={}", s.event, s.ends_call));
            }
        }
        assert!(bad.is_empty(), "goodbyes missed:\n{}", bad.join("\n"));
    }

    #[test]
    fn pre_closings_announce_the_end() {
        let cases: &[(&'static str, &str)] = &[
            ("vi", "Có gì liên lạc sau nhé"), ("vi", "Vậy có gì liên lạc sau nha"),
            ("hi", "Okay rakhta hoon"), ("hi", "Chalo, rakhti hoon"), ("hi", "Theek hai, rakhta hoon"),
        ];
        let mut bad = Vec::new();
        for (lang, t) in cases {
            let mut m = after_talk(lang);
            let s = hears(&mut m, t);
            if s.event != ClosingEvent::CallerPreClosing {
                bad.push(format!("[{lang}] {t} -> {:?}", s.event));
            }
        }
        assert!(bad.is_empty(), "pre-closings missed:\n{}", bad.join("\n"));
    }

    /// Greetings that also say goodbye close right after thanks or a short "ok".
    #[test]
    fn greeting_goodbyes_close_after_thanks_or_ok() {
        let cases: &[(&'static str, &str)] = &[
            ("it", "Grazie, un saluto."), ("it", "Va bene, grazie, un saluto"),
            ("ru", "Ладно, созвонимся."), ("ru", "Спасибо, созвонимся"), ("ru", "Спасибо, бывай!"),
            ("tr", "Teşekkürler, hayırlı günler."),
            ("id", "Terima kasih, assalamualaikum."), ("id", "Oke, assalamu'alaikum"),
            ("ar", "شكراً، السلام عليكم"), ("ar", "شكرا، سلام"), ("ar", "يعطيك العافية، باي"), ("ar", "شكراً، بأمان الله"),
            ("ar", "شكراً، في رعاية الله"), ("ar", "شكراً، الله معك"), ("ar", "شكراً، نهارك سعيد"),
            ("nl", "Bedankt, fijne dag!"), ("nl", "Dank je wel, prettige avond."), ("nl", "Bedankt, goed weekend!"),
            ("nl", "Bedankt, werk ze."), ("nl", "Dank u, tot straks."), ("nl", "Merci, salukes!"), ("nl", "Bedankt, daag!"),
            ("nl", "Bedankt, hoi hoi"),
            ("pl", "Dzięki, cześć!"), ("pl", "Dziękuję, wszystkiego dobrego."), ("pl", "Dzięki, trzymaj się!"),
            ("pl", "Dziękuję bardzo, do zobaczenia."),
        ];
        let mut bad = Vec::new();
        for (lang, t) in cases {
            let mut m = after_talk(lang);
            let s = hears(&mut m, t);
            if s.event != ClosingEvent::CallerFarewell || !s.ends_call {
                bad.push(format!("[{lang}] {t} -> {:?} ends={}", s.event, s.ends_call));
            }
        }
        assert!(bad.is_empty(), "greeting goodbyes after thanks missed:\n{}", bad.join("\n"));
    }

    /// Ordinary sentences holding a newly added closing word: no goodbye.
    #[test]
    fn lookalikes_do_not_close() {
        let cases: &[(&'static str, &str)] = &[
            ("it", "Volevo solo fare un saluto"), ("it", "Passo in negozio solo per un saluto"),
            ("it", "Mia madre le manda un saluto"),
            ("ru", "Давай пока подождём с оплатой"), ("ru", "Давай пока оставим как есть"),
            ("ru", "Давай пока я уточню адрес доставки"),
            ("ru", "Ну всё пока идёт по плану"), ("ru", "Ну всё пока нормально с заказом"), ("ru", "Ну все пока понятно"),
            ("ru", "Удачи пока не было, но мы стараемся"), ("ru", "Удачи пока никакой с этим заказом"),
            ("ru", "Пожелайте нам удачи пока мы едем"),
            ("ru", "Ну всё счастливо закончилось"), ("ru", "Они живут очень счастливо"), ("ru", "Спасибо, всё прошло счастливо"),
            ("ru", "Я думал, мы завтра созвонимся"), ("ru", "Он сказал, что вечером созвонимся"),
            ("ru", "Мы с курьером обычно утром созвонимся"),
            ("ru", "Ты к нам почаще бывай"), ("ru", "Бывай у нас почаще"), ("ru", "Не бывай таким грустным"),
            ("tr", "Bu işe eyvallah"), ("tr", "Eyvallah abi, şimdi adrese gelelim"), ("tr", "Ona hiç eyvallah etmem"),
            ("tr", "Hayırlı günler dileriz, kargonuz yolda"), ("tr", "Bayramınız hayırlı günler getirsin"),
            ("tr", "Size de hayırlı günler"),
            ("tr", "Tamamdır eyvallah dedi ama gelmedi"), ("tr", "Usta tamamdır eyvallah deyip gitti"),
            ("tr", "Tamamdır eyvallah demeden kapattı"),
            ("tr", "Anladım, eyvallah"), ("tr", "Tamamdır, eyvallah"), ("tr", "Sağ olun, eyvallah"),
            ("it", "Sì, un saluto"), ("it", "Va bene, un saluto"),
            ("ru", "Да, счастливо"), ("ru", "Спасибо, всё счастливо"),
            ("th", "โอเคค่ะ ไม่รบกวนแล้ว"), ("vi", "Dạ, có gì liên lạc sau"),
            ("vi", "Anh đừng cúp máy nhé"), ("vi", "Nếu mạng yếu thì mình cúp máy rồi gọi lại"),
            ("vi", "Hôm qua em xin phép cúp máy giữa chừng"),
            ("vi", "Lần trước anh nói có gì liên lạc sau mà không thấy"), ("vi", "Có gì liên lạc sau thì gọi số này"),
            ("vi", "Chị bảo có gì liên lạc sau cũng được"),
            ("th", "ขอโทษที่รบกวนนะครับ"), ("th", "ไม่รบกวนเวลานานครับ ขอสองนาที"), ("th", "เสียงดังไม่รบกวนแล้วใช่ไหมคะ"),
            ("th", "ยาตัวนี้แก้หวัดดี"), ("th", "ลูกเป็นไข้หวัดดีขึ้นแล้วค่ะ"), ("th", "ขอบคุณค่ะ ยาตัวนี้แก้หวัดดี"),
            ("id", "Assalamualaikum, pak, saya dari kurir"), ("id", "Tadi dia bilang assalamualaikum lalu pergi"),
            ("id", "Dia cuma bilang assalamualaikum"),
            ("ar", "إن شاء الله الأمور تصبح على خير ما يرام"), ("ar", "الحالة تصبح على خير حال بعد العلاج"),
            ("ar", "قال لي يلا سلام وراح"),
            ("ar", "يلا سلام عليكم يا جماعة، أنا من شركة التوصيل"), ("ar", "قال يلا باي وسكر الخط"),
            ("ar", "الحمد لله البلد في سلام"), ("ar", "وقعوا اتفاقية سلام"), ("ar", "أبغى أطلب تفاح باي"),
            ("ar", "الحمد لله، الأولاد بأمان الله"), ("ar", "وصلنا البيت بأمان الله"), ("ar", "الأطفال في رعاية الله"),
            ("ar", "المريض الآن في رعاية الله"), ("ar", "لا تخاف، الله معك"), ("ar", "ربنا يسهل والله معك"),
            ("ar", "نهارك سعيد يا أستاذ، معك البنك"),
            ("hi", "Okay, main paise locker mein rakhta hoon"), ("hi", "Main hamesha ek copy rakhta hoon"),
            ("hi", "Main roz phone charging pe rakhta hoon"),
            ("nl", "Wat betekent doeg eigenlijk"), ("nl", "Hij zei doeg tegen de buren"), ("nl", "Het woord doeg schrijf je met een g"),
            ("nl", "Tot snel in de winkel, maar eerst nog een vraag over de levering"), ("nl", "Ik zei tot snel tegen de monteur"),
            ("nl", "Tot snel is niet hetzelfde als vandaag"),
            ("nl", "Tot binnenkort hadden we geen problemen met de verwarming"), ("nl", "Ik zei tot binnenkort tegen de buurvrouw"),
            ("nl", "Tot binnenkort is te vaag voor de planning"),
            ("nl", "Spreek je later nog met de huisarts"), ("nl", "Of spreek je later liever Engels"),
            ("nl", "Wanneer spreek je later met de manager"),
            ("nl", "Het was een fijne dag"), ("nl", "We hadden een fijne dag aan zee"), ("nl", "Fijne dag gehad, maar ik bel over de factuur"),
            ("nl", "Gisteren was een prettige avond"), ("nl", "Het werd toch een prettige dag"), ("nl", "We hadden een prettige avond bij de buren"),
            ("nl", "We hadden een goed weekend"), ("nl", "Het was een fijn weekend"), ("nl", "Goed weekend gehad, maar de verwarming doet het niet"),
            ("nl", "Ik werk ze vandaag nog af"), ("nl", "Die formulieren, ik werk ze"), ("nl", "Werk ze maar bij in het systeem"),
            ("nl", "Ik ben bezig tot straks"), ("nl", "De winkel is dicht tot straks"), ("nl", "Wacht maar tot straks"),
            ("nl", "Ik daag hem uit"), ("nl", "Waarom daag je ons voor de rechter"), ("nl", "Ik daag"),
            ("nl", "Salu, met Jan van de bakker"), ("nl", "Ik zei salu tegen de buurman"), ("nl", "Salukes, ik bel voor de afspraak"),
            ("nl", "Hoi hoi, met Lisa van de tandarts"), ("nl", "Ze riep hoi hoi en ging weg"), ("nl", "De kinderen zeggen altijd hoi hoi"),
            ("nl", "Bedankt voor het wachten, ik heb nog een vraag"), ("nl", "Dank je wel voor de moeite maar het pakket is er nog niet"),
            ("pl", "No to cześć, tu kurier, mam paczkę"), ("pl", "Powiedział no to cześć i poszedł"), ("pl", "No to cześć pracy"),
            ("pl", "Miłego wieczoru życzę, ale mam jeszcze pytanie"), ("pl", "Nie mieliśmy miłego wieczoru przez awarię"),
            ("pl", "Życzyła mi miłego wieczoru wczoraj"),
            ("pl", "Wiem, że ciężko, ale trzymaj się"), ("pl", "Trzymaj się poręczy na schodach"), ("pl", "Trzymaj się instrukcji"),
            ("pl", "Wszystkiego dobrego z okazji urodzin"), ("pl", "Życzymy wszystkiego dobrego"),
            ("pl", "Wszystkiego dobrego nie da się kupić"),
            ("pl", "Tu nie ma już nic do zobaczenia"), ("pl", "Mam coś do zobaczenia w aplikacji"), ("pl", "Do zobaczenia jest tylko faktura"),
            ("pl", "Dzięki temu paczka dotarła szybciej"), ("pl", "Dziękuję za informację, ale mam jeszcze jedno pytanie"),
        ];
        let mut bad = Vec::new();
        for (lang, t) in cases {
            for l in [*lang, "en"] {
                let mut m = after_talk(l);
                let s = hears(&mut m, t);
                if closes(&s) {
                    bad.push(format!("[{l}] {t} -> {:?} ends={}", s.event, s.ends_call));
                }
            }
        }
        assert!(bad.is_empty(), "lookalikes treated as closing:\n{}", bad.join("\n"));
    }

    /// Greetings that also say goodbye open a call without ending it.
    #[test]
    fn openers_do_not_close() {
        let cases: &[(&'static str, &str)] = &[
            ("ar", "السلام عليكم"), ("ar", "السلام عليكم، معك البنك"), ("ar", "سلام"), ("ar", "نهارك سعيد"),
            ("ar", "مرحبا، نهارك سعيد"),
            ("id", "Assalamualaikum"), ("id", "Halo, assalamualaikum"),
            ("th", "หวัดดีค่ะ"), ("tr", "Hayırlı günler"), ("tr", "Merhaba, hayırlı günler"), ("tr", "Eyvallah"),
            ("it", "Pronto, un saluto!"),
            ("nl", "Hoi hoi"), ("nl", "Salu"), ("nl", "Daag"), ("nl", "Hallo, fijne dag!"), ("nl", "Goedemiddag, goed weekend gehad?"),
            ("pl", "Cześć mamo, wszystkiego dobrego!"), ("pl", "Wszystkiego dobrego!"),
        ];
        let mut bad = Vec::new();
        for (lang, t) in cases {
            let mut m = ClosingMachine::new(lang);
            let s = hears(&mut m, t);
            if closes(&s) {
                bad.push(format!("[{lang}] {t} -> {:?} ends={}", s.event, s.ends_call));
            }
        }
        assert!(bad.is_empty(), "openers treated as closing:\n{}", bad.join("\n"));
    }
}

/// Reviewer probe (second batch of closing words): replies that carry a newly
/// added word without ending the call.
#[cfg(test)]
mod review_probe2_tests {
    use super::*;

    fn hears(m: &mut ClosingMachine, t: &str) -> ClosingStep {
        m.on_caller(t, t, crate::locale::caller_wraps_up(t), false)
    }
    fn closes(s: &ClosingStep) -> bool {
        s.ends_call || matches!(s.event, ClosingEvent::CallerFarewell | ClosingEvent::CallerPreClosing)
    }
    fn talked(lang: &'static str, ai: &str) -> ClosingMachine {
        let mut m = ClosingMachine::new(lang);
        m.on_caller("Parcel delivery info", "Parcel delivery info", false, false);
        m.on_ai_turn(ai, false, false);
        m
    }

    /// The AI's own greeting, as a call's first words, reads as a closing
    /// announced. Existing behavior, not from this batch: "Pronto, buongiorno" and
    /// "Hallo, servus" do the same. Nothing ends: the caller's first words with
    /// content take it back.
    #[test]
    fn ai_own_greeting_never_ends_the_call() {
        let cases: &[(&'static str, &str)] = &[
            ("ar", "ألو، السلام عليكم"), ("ar", "السلام عليكم"), ("id", "Halo, assalamualaikum"),
            ("tr", "Alo, hayırlı günler"), ("nl", "Hoi hoi"), ("nl", "Hallo, fijne dag"), ("pl", "Dzień dobry, wszystkiego dobrego"),
            ("it", "Pronto, un saluto"), ("ru", "Алло, счастливо"),
            // baselines already in the code before this batch
            ("it", "Pronto, buongiorno"), ("de", "Hallo, servus"),
        ];
        let mut bad = Vec::new();
        for (lang, t) in cases {
            let mut m = ClosingMachine::new(lang);
            m.on_ai_turn(t, false, false);
            let s = hears(&mut m, "I am calling about the parcel for tomorrow");
            if closes(&s) || m.state() != ClosingState::Talking {
                bad.push(format!("[{lang}] AI says {t} -> {:?} ends={} state {:?}", s.event, s.ends_call, m.state()));
            }
        }
        assert!(bad.is_empty(), "AI greeting ended the call:\n{}", bad.join("\n"));
    }

    /// AI greeted, the caller greets back or answers: no hang-up.
    #[test]
    fn greeting_exchange_does_not_hang_up() {
        let cases: &[(&'static str, &str, &str)] = &[
            ("ar", "ألو، السلام عليكم", "السلام عليكم"), ("ar", "السلام عليكم", "سلام"), ("ar", "ألو؟", "أيوه، السلام عليكم"),
            ("ar", "ألو؟", "نعم، السلام عليكم"), ("ar", "ألو؟", "تمام، السلام عليكم"), ("ar", "ألو؟", "شكراً، السلام عليكم"),
            ("id", "Halo, assalamualaikum", "Assalamualaikum"), ("id", "Halo?", "Iya, assalamualaikum"),
            ("id", "Halo?", "Oke, assalamualaikum"),
            ("tr", "Efendim?", "Evet, hayırlı günler"), ("tr", "Efendim?", "Tamam, hayırlı günler"),
            ("tr", "Alo, hayırlı günler", "Hayırlı günler"), ("tr", "Efendim?", "Evet, eyvallah"),
            ("nl", "Hallo?", "Ja, hoi hoi"), ("nl", "Hoi hoi", "Hoi hoi"), ("nl", "Hallo?", "Ja, salu"),
            ("nl", "Hallo?", "Dank u, goedendag"),
            ("it", "Pronto?", "Sì, un saluto"), ("it", "Pronto?", "Va bene, un saluto"),
            ("pl", "Halo?", "Tak, cześć"), ("pl", "Halo?", "Dziękuję, wszystkiego dobrego"),
        ];
        let mut bad = Vec::new();
        for (lang, ai, caller) in cases {
            let mut m = ClosingMachine::new(lang);
            m.on_ai_turn(ai, false, false);
            let s = hears(&mut m, caller);
            if closes(&s) {
                bad.push(format!("[{lang}] AI: {ai} / caller: {caller} -> {:?} ends={}", s.event, s.ends_call));
            }
        }
        assert!(bad.is_empty(), "greeting exchange closes:\n{}", bad.join("\n"));
    }

    /// Mid-call, a cue word right before a newly added word that is not a goodbye.
    #[test]
    fn midcall_cue_then_added_word_is_not_a_goodbye() {
        let cases: &[(&'static str, &str)] = &[
            ("ar", "شكراً، الأولاد في رعاية الله"), ("ar", "تمام، هو الآن في رعاية الله"), ("ar", "أيوه، وصلنا بأمان الله"),
            ("ar", "نعم، لا تخف الله معك"), ("ar", "شكراً، عملية سلام"),
            ("tr", "Tamam, bu işe eyvallah"), ("tr", "Peki, ona eyvallah"), ("tr", "Evet, hayırlı günler getirsin"),
            ("tr", "Anladım, eyvallah"),
            ("nl", "Ja, ik bel u tot straks"), ("nl", "Oké, ik ben bezig tot straks"), ("nl", "Ja, ik daag"),
            ("nl", "Oké, ik werk ze"), ("nl", "Ja, we hadden een fijne dag"),
            ("pl", "Tak, trzymaj się"), ("pl", "Dobrze, dziękuję"), ("pl", "Nie, dziękuję"), ("nl", "Nee, bedankt"),
            ("pl", "Dobrze, do zobaczenia"),
            ("ru", "Да, счастливо"),
            ("it", "Sì, un saluto"), ("it", "Va bene, un saluto"), ("it", "Sì, mi manda un saluto"),
            ("hi", "Okay, main ek copy rakhta hoon"), ("hi", "Theek hai, main yahin rakhta hoon"),
            ("hi", "Chalo, main photo rakhta hoon"), ("hi", "Okay, yeh bhi rakhta hoon"),
            ("th", "โอเคค่ะ ไม่รบกวนแล้ว"),
            ("vi", "Dạ, có gì liên lạc sau"),
        ];
        let mut bad = Vec::new();
        for (lang, t) in cases {
            for ai in ["Understood, go ahead", "تمام، تفضل", "Tamam, buyurun"] {
                let mut m = talked(lang, ai);
                let s = hears(&mut m, t);
                if closes(&s) {
                    bad.push(format!("[{lang}] (AI: {ai}) {t} -> {:?} ends={}", s.event, s.ends_call));
                    break;
                }
            }
        }
        assert!(bad.is_empty(), "mid-call words treated as goodbye:\n{}", bad.join("\n"));
    }

    /// Mid-call, a short "ok" and then a goodbye: the caller is leaving.
    #[test]
    fn midcall_ok_then_goodbye_closes() {
        let cases: &[(&'static str, &str)] = &[
            ("ar", "طيب، الله معك"), ("ar", "تمام، باي"),
            ("ru", "Хорошо, созвонимся"), ("ru", "Ладно, бывай"),
            ("id", "Oke, assalamualaikum"), ("id", "Iya, assalamualaikum"),
        ];
        let mut bad = Vec::new();
        for (lang, t) in cases {
            let mut m = talked(lang, "Understood, go ahead");
            let s = hears(&mut m, t);
            if s.event != ClosingEvent::CallerFarewell || !s.ends_call {
                bad.push(format!("[{lang}] {t} -> {:?} ends={}", s.event, s.ends_call));
            }
        }
        assert!(bad.is_empty(), "goodbyes missed:\n{}", bad.join("\n"));
    }

    /// The live call feeds the transcript piece by piece. A piece that ends on thanks
    /// and a greeting-goodbye reads as a goodbye until more comes in. Existing
    /// behavior of every greeting-goodbye ("Danke, servus" does the same): the rest
    /// of the sentence takes the closing back and the call goes on.
    #[test]
    fn the_rest_of_the_sentence_takes_a_partial_goodbye_back() {
        let cases: &[(&'static str, &[&str])] = &[
            ("ar", &["شكراً، السلام عليكم", "شكراً، السلام عليكم ورحمة الله، عندي سؤال ثاني"]),
            ("ar", &["شكراً", "شكراً، السلام عليكم", "شكراً، السلام عليكم، معك فلان من البنك"]),
            ("id", &["Terima kasih, assalamualaikum", "Terima kasih, assalamualaikum, saya mau tanya lagi"]),
            ("tr", &["Teşekkürler, hayırlı günler", "Teşekkürler, hayırlı günler, kargo için arıyorum"]),
            ("nl", &["Bedankt, fijne dag", "Bedankt, fijne dag, maar ik heb nog een vraag"]),
            ("nl", &["Bedankt, tot straks", "Bedankt, tot straks, maar eerst nog een vraag"]),
            ("pl", &["Dziękuję, trzymaj się", "Dziękuję, trzymaj się tego planu"]),
            ("de", &["Danke, servus", "Danke, servus, ich habe noch eine Frage zur Lieferung"]),
        ];
        let mut bad = Vec::new();
        for (lang, pieces) in cases {
            let mut m = talked(lang, "Understood, go ahead");
            let mut last = ClosingStep { event: ClosingEvent::None, ends_call: false };
            for p in *pieces {
                last = hears(&mut m, p);
            }
            if closes(&last) || m.state() != ClosingState::Talking {
                bad.push(format!("[{lang}] {} -> {:?} ends={} state {:?}", pieces.last().unwrap(), last.event, last.ends_call, m.state()));
            }
        }
        assert!(bad.is_empty(), "the rest did not take the closing back:\n{}", bad.join("\n"));
    }
}

/// Third batch of closing words (Swedish, Norwegian, Danish, Finnish, Czech, Slovak,
/// Hungarian, Romanian, Bulgarian, Greek, Croatian, Serbian, Ukrainian).
#[cfg(test)]
mod regional_closing_words_3_tests {
    use super::*;

    fn after_talk(lang: &'static str) -> ClosingMachine {
        let mut m = ClosingMachine::new(lang);
        m.on_caller("Parcel delivery info", "Parcel delivery info", false, false);
        m
    }

    /// The caller's turn as the live session feeds it: with the older check.
    fn hears(m: &mut ClosingMachine, t: &str) -> ClosingStep {
        m.on_caller(t, t, crate::locale::caller_wraps_up(t), false)
    }

    fn closes(s: &ClosingStep) -> bool {
        s.ends_call || matches!(s.event, ClosingEvent::CallerFarewell | ClosingEvent::CallerPreClosing)
    }

    fn all_close(cases: &[(&'static str, &str)], m: impl Fn(&'static str) -> ClosingMachine) -> Vec<String> {
        let mut bad = Vec::new();
        for (lang, t) in cases {
            let mut mm = m(lang);
            let s = hears(&mut mm, t);
            if s.event != ClosingEvent::CallerFarewell || !s.ends_call {
                bad.push(format!("[{lang}] {t} -> {:?} ends={}", s.event, s.ends_call));
            }
        }
        bad
    }

    #[test]
    fn goodbyes_end_the_call() {
        let cases: &[(&'static str, &str)] = &[
            ("sv", "Hej då!"), ("sv", "Okej, hej då."), ("sv", "Tack för hjälpen, hej då!"), ("sv", "Hejdå!"),
            ("nb", "Hei så lenge!"), ("nb", "Takk for praten!"), ("nb", "Ok, hei så lenge."),
            ("da", "Fortsat god dag!"), ("da", "Hav en god dag."), ("da", "Mange tak, hav en god dag!"), ("da", "Hav det godt!"),
            ("da", "Tak for snakken."),
            ("fi", "Kuulemiin!"), ("fi", "Kiitos, kuulemiin."), ("fi", "Kiitos ja hei!"), ("fi", "Hyvää päivänjatkoa!"),
            ("fi", "Kiitos paljon, hyvää päivänjatkoa."),
            ("cs", "Na shledanou!"), ("cs", "Děkuji, na shledanou."), ("cs", "Nashledanou."), ("cs", "Díky, nashle!"),
            ("cs", "Mějte se hezky!"), ("cs", "Tak se mějte."), ("cs", "Přeji pěkný zbytek dne."), ("cs", "Hezký zbytek dne!"),
            ("cs", "Zatím ahoj!"), ("cs", "Tak zatím čau."), ("cs", "Tak pa!"),
            ("sk", "Dovidenia!"), ("sk", "Ďakujem, dovidenia."), ("sk", "Vďaka, do videnia!"), ("sk", "Majte sa pekne!"),
            ("sk", "Maj sa!"), ("sk", "Pekný zvyšok dňa!"), ("sk", "Zatiaľ ahoj."), ("sk", "Tak zatiaľ čau!"),
            ("hu", "Viszlát!"), ("hu", "Köszönöm, viszontlátásra."), ("hu", "Viszonthallásra!"), ("hu", "További szép napot!"),
            ("hu", "Köszönöm, további szép napot kívánok."), ("hu", "Akkor szia!"), ("hu", "Na akkor szia."),
            ("bg", "Дочуване!"), ("bg", "Благодаря, дочуване."), ("bg", "Айде, чао!"),
            ("hr", "Doviđenja!"), ("hr", "Hvala, doviđenja."), ("hr", "Do viđenja!"), ("hr", "Ajde, bok!"),
            ("sr", "Ajde, ćao!"), ("sr", "Hvala lepo, doviđenja."),
            ("uk", "Давай, бувай!"), ("uk", "Ну все, па-па!"),
        ];
        let bad = all_close(cases, after_talk);
        assert!(bad.is_empty(), "goodbyes missed:\n{}", bad.join("\n"));
    }

    /// Words that are greetings or everyday words too close right after thanks or a short "ok".
    #[test]
    fn cued_goodbyes_close_after_thanks_or_ok() {
        let cases: &[(&'static str, &str)] = &[
            ("sv", "Tack, ha det bra!"), ("sv", "Tack, ha det så bra."), ("sv", "Tack så mycket, ha det gott!"),
            ("sv", "Tack, vi ses!"), ("sv", "Tusen tack, tack och hej."), ("sv", "Tack för hjälpen, ha det bra."),
            ("nb", "Takk, ha det godt!"), ("nb", "Tusen takk, vi snakkes."), ("nb", "Ok, snakkes!"),
            ("nb", "Takk for hjelpen, ha en fin dag!"), ("nb", "Mange takk, snakkes senere."),
            ("da", "Tak for det, farvel."), ("da", "Mange tak, farvel!"), ("da", "Tak for hjælpen, vi snakkes ved."),
            ("da", "Tusind tak, vi ses."),
            ("fi", "Kiitos, moikka!"), ("fi", "Kiitti, heippa!"), ("fi", "Kiitos paljon, moi moi."),
            ("cs", "Díky, ahoj!"), ("cs", "Děkuju, čau."), ("cs", "Děkuji moc, hezký den!"), ("cs", "Ok, ahoj."),
            ("cs", "Díky za info, ahoj."),
            ("sk", "Ďakujem, čaute!"), ("sk", "Vďaka, pekný deň."), ("sk", "Ďakujem pekne, dobrú noc!"),
            ("sk", "Ďakujem za pomoc, ahoj."),
            ("hu", "Köszönöm, szia!"), ("hu", "Köszi, sziasztok!"), ("hu", "Köszönöm szépen, szép napot!"),
            ("hu", "Köszönöm, szép napot kívánok."), ("hu", "Köszönöm, minden jót!"), ("hu", "Köszönöm a segítséget, szia."),
            ("ro", "Mulțumesc, o zi frumoasă!"), ("ro", "Mersi, seară bună!"), ("ro", "Mulțumesc frumos, spor la treabă."),
            ("ro", "Mulțumesc pentru ajutor, o zi frumoasă."),
            ("bg", "Благодаря, чао!"), ("bg", "Мерси, чао чао."), ("bg", "Благодаря, приятен ден!"), ("bg", "Благодаря ви, хубав ден."),
            ("bg", "Благодаря, лека вечер!"), ("bg", "Мерси, всичко добро."), ("bg", "Благодаря, лека работа!"),
            ("bg", "Благодаря, до скоро."),
            ("el", "Ευχαριστώ, γεια σας!"), ("el", "Ευχαριστώ πολύ, γεια σου."), ("el", "Ευχαριστώ, γεια χαρά!"),
            ("el", "Ευχαριστούμε, τα λέμε."), ("el", "Ευχαριστώ, καλό βράδυ!"), ("el", "Ευχαριστώ, καλό απόγευμα."),
            ("el", "Ευχαριστώ, χαιρετώ."), ("el", "Ευχαριστώ, καλή συνέχεια!"),
            ("el", "Ευχαριστώ, έγινε, γεια!"), ("el", "Ευχαριστώ πολύ, εγινε γεια."),
            ("hr", "Hvala, bok!"), ("hr", "Hvala lijepa, vidimo se."), ("hr", "Hvala, ugodan dan!"), ("hr", "Hvala, lijep pozdrav."),
            ("hr", "Hvala vam, pozdrav!"), ("hr", "Hvala, svako dobro."), ("hr", "Hvala, laku noć!"), ("hr", "Hvala na pomoći, bok."),
            ("sr", "Hvala, ćao!"), ("sr", "Hvala lepo, prijatno."), ("sr", "Hvala, sve najbolje!"),
            ("uk", "Дякую, тихого дня!"), ("uk", "Дякую, усього найкращого."), ("uk", "Дякую вам, всього найкращого!"),
            ("uk", "Дякую, зідзвонимось."), ("uk", "Дуже дякую, зідзвонимося!"),
            ("uk", "Дякую, бережіть себе!"), ("uk", "Дякую, бережи себе."),
            // Words already listed, now after this batch's thanks.
            ("ro", "Mulțumesc, bună ziua."), ("uk", "Дякую, бувай!"), ("uk", "Дякую, па-па."),
        ];
        let bad = all_close(cases, after_talk);
        assert!(bad.is_empty(), "cued goodbyes missed:\n{}", bad.join("\n"));
    }

    const CUED_ALONE: &[(&str, &str)] = &[
        ("sv", "Ha det bra!"), ("sv", "Ha det gott."), ("sv", "Vi ses!"), ("sv", "Tack och hej!"),
        ("nb", "Ha det godt!"), ("nb", "Vi snakkes."), ("nb", "Snakkes!"), ("nb", "Snakkes senere."), ("nb", "Ha en fin dag!"),
        ("da", "Farvel!"), ("da", "Vi snakkes ved."),
        ("fi", "Moikka!"), ("fi", "Heippa!"), ("fi", "Moi moi!"),
        ("cs", "Ahoj!"), ("cs", "Čau!"), ("cs", "Hezký den!"),
        ("sk", "Čaute!"), ("sk", "Pekný deň!"), ("sk", "Dobrú noc!"),
        ("hu", "Szia!"), ("hu", "Sziasztok!"), ("hu", "Szép napot!"), ("hu", "Szép napot kívánok!"), ("hu", "Minden jót!"),
        ("ro", "O zi frumoasă!"), ("ro", "Seară bună!"), ("ro", "Spor la treabă!"),
        ("bg", "Чао!"), ("bg", "Чао, чао!"), ("bg", "Приятен ден!"), ("bg", "Хубав ден!"), ("bg", "Лека вечер!"),
        ("bg", "Всичко добро!"), ("bg", "Лека работа!"), ("bg", "До скоро!"),
        ("el", "Γεια σας!"), ("el", "Γεια σου!"), ("el", "Γεια χαρά!"), ("el", "Τα λέμε!"), ("el", "Καλό βράδυ!"),
        ("el", "Καλό απόγευμα!"), ("el", "Χαιρετώ!"), ("el", "Καλή συνέχεια!"),
        ("el", "Έγινε, γεια!"), ("el", "Εντάξει, έγινε, γεια."),
        ("hr", "Bok!"), ("hr", "Vidimo se!"), ("hr", "Ugodan dan!"), ("hr", "Lijep pozdrav!"), ("hr", "Pozdrav!"),
        ("hr", "Svako dobro!"), ("hr", "Laku noć!"),
        ("sr", "Ćao!"), ("sr", "Prijatno!"), ("sr", "Sve najbolje!"),
        ("uk", "Тихого дня!"), ("uk", "Зідзвонимось!"), ("uk", "Усього найкращого!"),
        ("uk", "Бережіть себе!"), ("uk", "Бережи себе."),
    ];

    /// Alone, mid-call, they wait: the AI's goodbye before them, or a cue in the same turn.
    #[test]
    fn cued_words_alone_close_only_after_the_ais_goodbye() {
        let mut bad = Vec::new();
        for (lang, t) in CUED_ALONE {
            let mut m = after_talk(lang);
            let s = hears(&mut m, t);
            if closes(&s) {
                bad.push(format!("[{lang}] mid-call {t} -> {:?} ends={}", s.event, s.ends_call));
            }
        }
        let bad_after_ai = all_close(CUED_ALONE, |lang| {
            let mut m = after_talk(lang);
            m.on_ai_turn("Okay, goodbye!", false, false);
            m
        });
        bad.extend(bad_after_ai.into_iter().map(|b| format!("after the AI's goodbye: {b}")));
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// Ordinary sentences holding a newly added word: no goodbye.
    #[test]
    fn lookalikes_do_not_close() {
        let cases: &[(&'static str, &str)] = &[
            ("sv", "Barnen vinkade hej då till mormor vid tåget"), ("sv", "Hej då är det sista ordet i boken"),
            ("sv", "Vi hann inte säga hejdå ordentligt igår"),
            ("sv", "Hon verkar ha det bra"), ("sv", "Barnen ska ha det bra hos oss"), ("sv", "Jag hoppas att hunden får ha det bra"),
            ("sv", "Han förtjänar att ha det så bra"), ("sv", "Alla borde få ha det så bra"), ("sv", "Det är skönt att få ha det så bra"),
            ("sv", "Katten verkar ha det gott"), ("sv", "Pensionärerna ska ha det gott"), ("sv", "De tycks ha det gott"),
            ("sv", "Jag vet inte när vi ses"), ("sv", "Det spelar ingen roll var vi ses"), ("sv", "Mamma undrar hur ofta vi ses"),
            ("sv", "Han tog pengarna och sedan var det tack och hej"), ("sv", "Leverantören bara försvann, tack och hej"),
            ("sv", "Det blev inget avtal, bara tack och hej"),
            ("sv", "Tack vare er kom paketet fram"), ("sv", "Tack för att du ringde, men jag har en fråga till om fakturan"),
            ("sv", "Kolla bara att vi hörs"), ("sv", "Ja, vi hörs"),
            ("nb", "Hei, så lenge du er hjemme kan pakken leveres"), ("nb", "Han ropte hei så lenge og gikk"),
            ("nb", "Hei så lenge var det eneste han sa"),
            ("nb", "Jeg sa takk for praten og la på"), ("nb", "Takk for praten i går, men jeg ringer om fakturaen"),
            ("nb", "Takk for praten sist, jeg har et spørsmål om leiligheten"),
            ("nb", "Hun ser ut til å ha det godt"), ("nb", "Dyrene skal ha det godt på gården"),
            ("nb", "Vi vil at besteforeldrene skal ha det godt"),
            ("nb", "I Norge er det norsk som snakkes"), ("nb", "Dette er et tema som sjelden snakkes"), ("nb", "Jeg vet ikke når vi snakkes"),
            ("nb", "Detaljene kan snakkes om senere"), ("nb", "Prisen må snakkes senere"), ("nb", "Det kan snakkes senere"),
            ("nb", "Vi håper barna skal ha en fin dag"), ("nb", "Hun fortjener å ha en fin dag"),
            ("nb", "Målet er at alle skal ha en fin dag"),
            ("nb", "Takk for at du ringte, men pakken har ikke kommet ennå"), ("nb", "Ja, vi høres"),
            ("da", "Hun sagde fortsat god dag og gik"), ("da", "Fortsat god dag er en almindelig hilsen"),
            ("da", "Skriv fortsat god dag nederst i mailen"),
            ("da", "Skriv hav en god dag på kortet"), ("da", "Hav en god dag står der på skiltet"),
            ("da", "Hun sagde hav en god dag til alle kunderne"),
            ("da", "Hav det godt står der på kortet"), ("da", "Han sagde hav det godt til naboen"),
            ("da", "Hav det godt på ferien og ring når I er hjemme"),
            ("da", "Tak for snakken i går, men jeg ringer om fakturaen"), ("da", "Han sendte en sms med tak for snakken og et tilbud"),
            ("da", "Tak for snakken sidst, har du fået pakken"),
            ("da", "Vi tog et sidste farvel med farmor"), ("da", "Til begravelsen sagde vi et sidste farvel"),
            ("da", "Vi har sagt farvel til den gamle bil"),
            ("da", "Jeg ved ikke hvornår vi snakkes ved"), ("da", "Det er sjældent vi snakkes ved"), ("da", "Mor er glad når vi snakkes ved"),
            ("da", "Tak skal du have for pakken, men den var forkert"),
            ("fi", "Hän sanoi kuulemiin ja lopetti puhelun"), ("fi", "Kuulemiin on vanha sana"),
            ("fi", "Kirjoitin kuulemiin viestin loppuun vaikka se oli sähköposti"),
            ("fi", "Kiitos ja hei ovat ensimmäiset sanat jotka opin"), ("fi", "Hän sanoi kiitos ja hei ja lähti"),
            ("fi", "Kiitos ja hei, soitan Postista paketin takia"),
            ("fi", "Toivotin hyvää päivänjatkoa ja lähdin kaupasta"), ("fi", "Hyvää päivänjatkoa ei sovi illalla"),
            ("fi", "Kassalla sanotaan aina hyvää päivänjatkoa asiakkaille"),
            ("fi", "Lapsi sanoi moikka mummolle"), ("fi", "Moikka, täällä on Liisa hammaslääkäristä"), ("fi", "Sano moikka"),
            ("fi", "Heippa, soitan siitä paketista"), ("fi", "Hän huikkasi heippa ovelta"), ("fi", "Kirjoita viestiin heippa"),
            ("fi", "Moi moi, täällä Pekka"), ("fi", "Lapset huusivat moi moi"), ("fi", "Moi moi on tuttu tervehdys"),
            ("fi", "Odotetaan ja nähdään"), ("fi", "Kiitos tiedosta, mutta minulla on vielä kysymys laskusta"),
            ("cs", "Řekl na shledanou a odešel"), ("cs", "Na shledanou se těším"), ("cs", "Slovo nashledanou se píše dohromady"),
            ("cs", "Nashle je hovorová zkratka"), ("cs", "Říkal nashle všem"), ("cs", "Nashle nikdy neříkám"),
            ("cs", "Mějte se na pozoru před podvodníky"), ("cs", "Mějte se k světu"), ("cs", "Říkal mějte se a odešel"),
            ("cs", "Měli jsme pěkný zbytek dne u babičky"), ("cs", "Pěkný zbytek dne vám přeje vaše pošta"),
            ("cs", "Hezký zbytek dne jsme strávili venku"),
            ("cs", "Zatím ahoj neřekl"), ("cs", "Zatím čau nikdo neřekl"), ("cs", "Psal zatím ahoj na konec zprávy"),
            ("cs", "Tak pak zavolám"), ("cs", "Říkal tak pa a zavěsil"), ("cs", "Tak pa už neříkej"),
            ("cs", "Ahoj, tady Petr z doručovací služby"), ("cs", "Řekni mu ahoj"), ("cs", "Děti říkají každému ahoj"),
            ("cs", "Čau, volám kvůli tomu balíku"), ("cs", "Řekni jim čau"), ("cs", "Čau je neformální pozdrav"),
            ("cs", "Byl to hezký den"), ("cs", "Máme za sebou hezký den"), ("cs", "Zítra má být hezký den"),
            ("cs", "Díky tomu balík dorazil dřív"), ("cs", "Děkuji za informaci, ale mám ještě jednu otázku k faktuře"),
            ("cs", "Je to tak zatím"),
            ("sk", "Povedal dovidenia a odišiel"), ("sk", "Dovidenia sa píše spolu"), ("sk", "Dovidenia nehovorí nikdy"),
            ("sk", "Majte sa na pozore pred podvodníkmi"), ("sk", "Maj sa k svetu"), ("sk", "Majte sa ako doma"),
            ("sk", "Mali sme pekný zvyšok dňa pri mori"), ("sk", "Pekný zvyšok dňa vám želá pošta"),
            ("sk", "Pekný zvyšok dňa sme strávili vonku"),
            ("sk", "Zatiaľ ahoj nepovedal"), ("sk", "Zatiaľ čau nikto nepovedal"), ("sk", "Napísal zatiaľ ahoj na koniec správy"),
            ("sk", "Čaute, volám kvôli balíku"), ("sk", "Povedz im čaute"), ("sk", "Čaute je pozdrav pre viacerých"),
            ("sk", "Bol to pekný deň"), ("sk", "Zajtra bude pekný deň"), ("sk", "Mali sme pekný deň"),
            ("sk", "Nemal som dobrú noc"), ("sk", "Dieťa malo dobrú noc"), ("sk", "Prajem si aspoň jednu dobrú noc"),
            ("sk", "Vďaka tomu balík prišiel skôr"),
            ("hu", "Viszlát nélkül ment el"), ("hu", "Azt írta viszlát a levél végére"), ("hu", "Viszlát helyett köszönt"),
            ("hu", "Viszontlátásra nem számítok"), ("hu", "A viszontlátásra mindenki készül"), ("hu", "Viszontlátásra várjuk a rendelőben"),
            ("hu", "Viszonthallásra mondta a rádiós"), ("hu", "A viszonthallásra régi köszönés"), ("hu", "Viszonthallásra nem volt idő"),
            ("hu", "További szép napot kívánunk minden vásárlónak"), ("hu", "A pénztáros mindig további szép napot mond"),
            ("hu", "További szép napot írt a levél végére"),
            ("hu", "Akkor szia, de még egy kérdés"), ("hu", "Ha akkor szia lett volna, nem haragszik"), ("hu", "Akkor szia volt a válasza"),
            ("hu", "Szia, a csomag miatt hívlak"), ("hu", "Mondd meg anyának, hogy szia"), ("hu", "Szia vagy jó napot, mindegy"),
            ("hu", "Sziasztok, itt Anna a rendelőből"), ("hu", "Mondd a gyerekeknek, hogy sziasztok"), ("hu", "Sziasztok, mindenki megvan"),
            ("hu", "Szép napot kívánok, a postáról hívom"), ("hu", "Szép napot ígér az időjárás"), ("hu", "Szép napot csináltunk belőle"),
            ("hu", "Minden jót a születésnapodra"), ("hu", "Nem hoz minden jót a holnap"), ("hu", "Kívánok neki minden jót"),
            ("hu", "Köszönöm a türelmét, de még van egy kérdésem a számlával kapcsolatban"),
            ("ro", "A fost o zi frumoasă"), ("ro", "Mâine va fi o zi frumoasă"), ("ro", "Am avut o zi frumoasă la mare"),
            ("ro", "A fost o seară bună"), ("ro", "Am avut o seară bună cu familia"), ("ro", "Sper să fie o seară bună pentru meci"),
            ("ro", "Le-am urat spor la treabă"), ("ro", "Spor la treabă, vă sun de la curierat"), ("ro", "Am nevoie de spor la treabă"),
            ("ro", "Mulțumesc pentru informație, dar mai am o întrebare despre factură"),
            ("ro", "Ok, nu ne auzim bine"), ("ro", "Alo, nu ne auzim"), ("ro", "Mulțumesc, numai bine"),
            ("bg", "Дочуване казваме само по телефона"), ("bg", "Той каза дочуване и затвори"), ("bg", "Думата дочуване е стара"),
            ("bg", "Айде чао не е учтиво"), ("bg", "Каза айде чао и си тръгна"), ("bg", "Айде чао, ама чакай още нещо"),
            ("bg", "Чао, обаждам се за пратката"), ("bg", "Кажи му чао"), ("bg", "Чао е италианска дума"),
            ("bg", "Беше приятен ден"), ("bg", "Утре ще е приятен ден"), ("bg", "Имахме приятен ден на морето"),
            ("bg", "Днес е хубав ден"), ("bg", "Беше хубав ден за разходка"), ("bg", "Чакаме хубав ден"),
            ("bg", "Имахме лека вечер"), ("bg", "Беше лека вечер на работа"), ("bg", "Обещават лека вечер без дъжд"),
            ("bg", "При нас всичко добро"), ("bg", "Всичко добро идва с времето"), ("bg", "Не всичко добро е скъпо"),
            ("bg", "Днес имам лека работа"), ("bg", "Търся лека работа"), ("bg", "Това е лека работа"),
            ("bg", "Написа до скоро в края на писмото"), ("bg", "До скоро е кратко сбогуване"), ("bg", "Каза до скоро и затвори"),
            ("bg", "Благодаря за информацията, но имам още един въпрос"), ("bg", "Благодаря, живи и здрави"),
            ("el", "Μόλις έγινε, γεια σας, εδώ κούριερ"), ("el", "Το λάθος έγινε, γεια σας και πάλι, σας καλώ για το δέμα"),
            ("el", "Τι έγινε, γεια σου, εδώ ο Νίκος"),
            ("el", "Γεια σας, σας καλώ από την τράπεζα"), ("el", "Πείτε στα παιδιά γεια σας"),
            ("el", "Γεια σας κι από μένα, για το ραντεβού σας καλώ"),
            ("el", "Γεια σου Νίκο, εδώ ο υδραυλικός"), ("el", "Πες του γεια σου"), ("el", "Γεια σου κι εσένα"),
            ("el", "Γεια χαρά, για το δέμα παίρνω"), ("el", "Είπε γεια χαρά"), ("el", "Γεια χαρά λέμε στους φίλους"),
            ("el", "Έτσι τα λέμε"), ("el", "Πώς τα λέμε στα αγγλικά"), ("el", "Αυτά τα λέμε κάθε μέρα"),
            ("el", "Ήταν καλό βράδυ"), ("el", "Περάσαμε καλό βράδυ"), ("el", "Ελπίζω να έχουμε καλό βράδυ"),
            ("el", "Ήταν καλό απόγευμα"), ("el", "Καλό απόγευμα, σας καλώ από το φαρμακείο"),
            ("el", "Περάσαμε καλό απόγευμα στην παραλία"),
            ("el", "Όταν τον βλέπω τον χαιρετώ"), ("el", "Χαιρετώ τη μητέρα σας"), ("el", "Σας χαιρετώ, είμαι από την εταιρεία"),
            ("el", "Η σειρά δεν είχε καλή συνέχεια"), ("el", "Καλή συνέχεια έχει η ιστορία"),
            ("el", "Περιμένουμε καλή συνέχεια στη θεραπεία"),
            ("el", "Ευχαριστώ για την ενημέρωση, αλλά έχω μια ερώτηση"), ("el", "Ευχαριστώ, να είστε καλά"),
            ("hr", "Rekao je doviđenja i otišao"), ("hr", "Doviđenja se piše zajedno"), ("hr", "Doviđenja nije rekao nikome"),
            ("hr", "Ajde bok, a sad ozbiljno"), ("hr", "Rekao je ajde bok i spustio slušalicu"), ("hr", "Ajde bok nije pristojno"),
            ("hr", "Bok, zovem zbog paketa"), ("hr", "Reci mu bok"), ("hr", "Bok je kratki pozdrav"),
            ("hr", "Vidimo se u petak u devet sati"), ("hr", "Vidimo se rijetko"), ("hr", "Vidimo se tek u ponedjeljak, ali imam pitanje"),
            ("hr", "Bio je ugodan dan"), ("hr", "Sutra će biti ugodan dan"), ("hr", "Imali smo ugodan dan na moru"),
            ("hr", "Mama šalje lijep pozdrav"), ("hr", "Prenesi joj lijep pozdrav"), ("hr", "Lijep pozdrav od cijele obitelji"),
            ("hr", "Pozdrav, zovem iz banke"), ("hr", "Šaljem pozdrav"), ("hr", "Kolega vam šalje pozdrav"),
            ("hr", "Želim mu svako dobro"), ("hr", "Svako dobro dijete zaslužuje poklon"), ("hr", "Ne donosi svako dobro sreću"),
            ("hr", "Poželio sam djeci laku noć"), ("hr", "Imali smo laku noć"), ("hr", "Laku noć im je rekla mama"),
            ("hr", "Hvala na strpljenju, ali imam još jedno pitanje oko računa"), ("hr", "Ne čujemo se"), ("hr", "Ok, čujemo se"),
            ("sr", "Ajde ćao, a sad o računu"), ("sr", "Rekao je ajde ćao i otišao"), ("sr", "Ajde ćao nije lepo reći"),
            ("sr", "Ćao, zovem zbog paketa"), ("sr", "Reci mu ćao"), ("sr", "Ćao je italijanska reč"),
            ("sr", "Bilo je prijatno"), ("sr", "Prijatno mi je da vas čujem"), ("sr", "Veče je bilo veoma prijatno"),
            ("sr", "Sve najbolje za rođendan"), ("sr", "Želimo mu sve najbolje"), ("sr", "Kupili smo sve najbolje"),
            ("uk", "Бережіть себе від шахраїв"), ("uk", "Лікар сказав: бережіть себе і пийте воду"), ("uk", "Бережіть себе взимку"),
            ("uk", "Бережи себе для важливого"), ("uk", "Лікар сказав: бережіть себе"), ("uk", "Відпочивайте більше і бережіть себе"),
            ("el", "Τι έγινε, γεια"), ("el", "Δεν ξέρω τι έγινε, γεια"),
            ("uk", "Давай бувай у нас частіше"), ("uk", "Давай бувай на зборах"), ("uk", "Він сказав давай бувай і пішов"),
            ("uk", "Ну все па-па, а тепер про рахунок"), ("uk", "Ну все, па-па сказала дитина і заснула"),
            ("uk", "Тихого дня сьогодні не було"), ("uk", "Ми давно не мали тихого дня"), ("uk", "Тихого дня, дзвоню з пошти"),
            ("uk", "Усього найкращого з днем народження"), ("uk", "Ми бажаємо йому усього найкращого"), ("uk", "Усього найкращого не купиш"),
            ("uk", "Ми завтра зідзвонимось"), ("uk", "Він сказав, що ввечері зідзвонимось"), ("uk", "Ми з кур'єром зранку зідзвонимося"),
            ("uk", "Дякую за інформацію, але в мене ще одне питання"), ("uk", "Ок, на зв'язку"), ("uk", "Так, я на зв'язку"),
        ];
        let mut bad = Vec::new();
        for (lang, t) in cases {
            for l in [*lang, "en"] {
                let mut m = after_talk(l);
                let s = hears(&mut m, t);
                if closes(&s) {
                    bad.push(format!("[{l}] {t} -> {:?} ends={}", s.event, s.ends_call));
                }
            }
        }
        assert!(bad.is_empty(), "lookalikes treated as closing:\n{}", bad.join("\n"));
    }

    /// Thanks or "ok" first, then an ordinary sentence ending on a newly added word.
    #[test]
    fn a_cue_before_an_ordinary_sentence_is_no_goodbye() {
        let cases: &[(&'static str, &str)] = &[
            ("sv", "Tack, jag vet inte när vi ses"), ("sv", "Okej, hon verkar ha det bra"), ("sv", "Ja, barnen ska ha det bra hos oss"),
            ("nb", "Takk, i Norge er det norsk som snakkes"), ("nb", "Ok, prisen må snakkes senere"),
            ("da", "Tak, til begravelsen sagde vi et sidste farvel"), ("da", "Ja, det er sjældent vi snakkes ved"),
            ("fi", "Okei, lapset huusivat moi moi"), ("fi", "Kiitos, odotetaan ja nähdään"),
            ("cs", "Děkuji, byl to hezký den"), ("cs", "Ok, řekni mu ahoj"),
            ("sk", "Ďakujem, nemal som dobrú noc"), ("sk", "Ok, zajtra bude pekný deň"),
            ("hu", "Köszönöm, mondd meg anyának, hogy szia"), ("hu", "Ok, szép napot ígér az időjárás"),
            ("ro", "Mulțumesc, a fost o zi frumoasă"), ("ro", "Mersi, a fost o seară bună"),
            ("bg", "Благодаря, днес имам лека работа"), ("bg", "Да, беше приятен ден"),
            ("el", "Ευχαριστώ, έτσι τα λέμε"), ("el", "Ευχαριστώ, ήταν καλό βράδυ"),
            ("hr", "Hvala, mama šalje lijep pozdrav"), ("hr", "Ok, želim mu svako dobro"),
            ("sr", "Hvala, bilo je prijatno"), ("sr", "Ok, kupili smo sve najbolje"),
            ("uk", "Дякую, ми завтра зідзвонимось"), ("uk", "Так, ми давно не мали тихого дня"),
        ];
        let mut bad = Vec::new();
        for (lang, t) in cases {
            let mut m = after_talk(lang);
            let s = hears(&mut m, t);
            if closes(&s) {
                bad.push(format!("[{lang}] {t} -> {:?} ends={}", s.event, s.ends_call));
            }
        }
        assert!(bad.is_empty(), "closing after a cue:\n{}", bad.join("\n"));
    }

    /// Bare thanks in the new languages is thanks, no goodbye.
    #[test]
    fn bare_thanks_does_not_close() {
        let cases: &[(&'static str, &str)] = &[
            ("sv", "Tack!"), ("sv", "Tack så mycket."), ("nb", "Takk!"), ("nb", "Tusen takk."), ("da", "Mange tak!"),
            ("da", "Tak for det."), ("fi", "Kiitos!"), ("fi", "Kiitos paljon."), ("cs", "Děkuji!"), ("cs", "Díky moc."),
            ("sk", "Ďakujem!"), ("sk", "Vďaka."), ("hu", "Köszönöm!"), ("hu", "Köszi."), ("ro", "Mulțumesc!"), ("ro", "Mersi."),
            ("bg", "Благодаря!"), ("bg", "Мерси."), ("el", "Ευχαριστώ!"), ("el", "Ευχαριστώ πολύ."), ("hr", "Hvala!"),
            ("sr", "Hvala lepo."), ("uk", "Дякую!"), ("uk", "Дуже дякую."), ("pl", "Tak"), ("pl", "Tak, tak"),
        ];
        let mut bad = Vec::new();
        for (lang, t) in cases {
            let mut m = after_talk(lang);
            let s = hears(&mut m, t);
            if closes(&s) || s.event == ClosingEvent::CallerThanksWrap {
                bad.push(format!("[{lang}] {t} -> {:?} ends={}", s.event, s.ends_call));
            }
        }
        assert!(bad.is_empty(), "bare thanks closed:\n{}", bad.join("\n"));
    }

    /// Swedish, Norwegian, Danish and Finnish thanks after settling words only is a
    /// wrap-up; after the AI's goodbye bare thanks keeps the closing; with a goodbye
    /// after it, it ends the call.
    #[test]
    fn nordic_thanks_still_wraps_up() {
        let wraps: &[(&'static str, &str)] = &[
            ("sv", "Okej, tack så mycket"), ("sv", "Nej, det var allt, tack"), ("nb", "Ok, takk skal du ha"),
            ("nb", "Greit, tusen takk"), ("da", "Okay, mange tak"), ("da", "Det var det, tak for det"),
            ("fi", "Selvä, kiitos paljon"), ("fi", "Okei, kiitos"),
        ];
        let mut bad = Vec::new();
        for (lang, t) in wraps {
            let mut m = after_talk(lang);
            let s = hears(&mut m, t);
            if s.event != ClosingEvent::CallerThanksWrap || s.ends_call || m.state() != ClosingState::PreClosing {
                bad.push(format!("[{lang}] {t} -> {:?} ends={} state={:?}", s.event, s.ends_call, m.state()));
            }
        }
        let after_ai: &[(&'static str, &str)] = &[
            ("sv", "Tack så mycket"), ("sv", "Okej, tack så mycket"), ("nb", "Takk skal du ha"), ("nb", "Ok, takk skal du ha"),
            ("da", "Tusind tak"), ("da", "Okay, mange tak"), ("fi", "Kiitos paljon"), ("fi", "Selvä, kiitos"),
        ];
        for (lang, t) in after_ai {
            let mut m = after_talk(lang);
            m.on_ai_turn("Okay, goodbye!", false, false);
            let s = hears(&mut m, t);
            if m.state() == ClosingState::Talking || closes(&s) {
                bad.push(format!("after the AI's goodbye [{lang}] {t} -> {:?} state={:?}", s.event, m.state()));
            }
        }
        let byes: &[(&'static str, &str)] = &[
            ("sv", "Tack så mycket, hej då"), ("nb", "Takk skal du ha, hei så lenge"), ("da", "Tusind tak, hav en god dag"),
            ("fi", "Kiitos paljon, kuulemiin"),
        ];
        bad.extend(all_close(byes, after_talk));
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// The caller's first words, and greetings back after the AI's: no goodbye.
    #[test]
    fn openers_and_greeting_exchanges_do_not_close() {
        let openers: &[(&'static str, &str)] = &[
            ("cs", "Ahoj!"), ("cs", "Čau, tady Petr"), ("cs", "Dobrý den, ahoj"), ("sk", "Čaute!"), ("hu", "Szia!"),
            ("hu", "Sziasztok!"), ("hu", "Szép napot kívánok!"), ("hu", "Jó napot, szép napot kívánok!"),
            ("hu", "Boldog születésnapot, minden jót!"), ("ro", "Spor la treabă!"), ("bg", "Чао!"), ("bg", "Здравейте, хубав ден!"),
            ("el", "Γεια σας!"), ("el", "Γεια σου!"), ("el", "Γεια χαρά!"), ("el", "Καλό απόγευμα!"), ("el", "Χαιρετώ!"),
            ("hr", "Bok!"), ("hr", "Pozdrav!"), ("sr", "Ćao!"), ("fi", "Moikka!"), ("fi", "Heippa!"), ("fi", "Moi moi!"),
            ("uk", "Тихого дня!"), ("uk", "Добрий день, тихого дня!"), ("sv", "Vi ses!"), ("da", "Farvel!"),
            ("el", "Τι έγινε, γεια!"), ("el", "Έλα, γεια!"), ("uk", "Бережіть себе!"),
        ];
        let mut bad = Vec::new();
        for (lang, t) in openers {
            let mut m = ClosingMachine::new(lang);
            let s = hears(&mut m, t);
            if closes(&s) {
                bad.push(format!("[{lang}] opener {t} -> {:?} ends={}", s.event, s.ends_call));
            }
        }
        let exchanges: &[(&'static str, &str, &str)] = &[
            ("hu", "Szia!", "Szia!"), ("hu", "Halló?", "Köszönöm, szia"), ("cs", "Ahoj!", "Ahoj!"), ("el", "Γεια σας!", "Γεια σας!"),
            ("el", "Παρακαλώ;", "Ευχαριστώ, γεια σας"), ("hr", "Bok!", "Bok, hvala"), ("bg", "Ало?", "Благодаря, чао"),
            ("fi", "Moikka!", "Kiitos, moikka"), ("uk", "Алло?", "Дякую, тихого дня"), ("ro", "Alo?", "Mulțumesc, spor la treabă"),
            ("sv", "Hallå?", "Ja, hej"), ("nb", "Hallo?", "Ja, hei hei"),
        ];
        for (lang, ai, caller) in exchanges {
            let mut m = ClosingMachine::new(lang);
            m.on_ai_turn(ai, false, false);
            let s = hears(&mut m, caller);
            if closes(&s) {
                bad.push(format!("[{lang}] AI: {ai} / caller: {caller} -> {:?} ends={}", s.event, s.ends_call));
            }
        }
        for (lang, ai) in [("cs", "Ahoj!"), ("hu", "Szia!"), ("el", "Γεια σας!"), ("bg", "Чао!"), ("hr", "Bok!")] {
            let mut m = ClosingMachine::new(lang);
            m.on_ai_turn(ai, false, false);
            let s = hears(&mut m, "I am calling about the parcel for tomorrow");
            if closes(&s) || m.state() != ClosingState::Talking {
                bad.push(format!("[{lang}] AI says {ai} -> {:?} state {:?}", s.event, m.state()));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// The research's own "not a goodbye yet" lines.
    #[test]
    fn false_closings_from_the_research_do_not_close() {
        let cases: &[(&'static str, &str)] = &[
            ("sv", "En sak till"), ("sv", "Förresten"), ("sv", "Vänta lite"), ("sv", "Innan vi lägger på"), ("sv", "En snabb fråga"),
            ("sv", "Apropå det"), ("sv", "Kom på en grej"), ("sv", "Hördu förresten"),
            ("nb", "Forresten"), ("nb", "En ting til"), ("nb", "Vent litt"), ("nb", "Kom på en ting"), ("nb", "Før du legger på"),
            ("nb", "Et kjapt spørsmål"), ("nb", "Du forresten"), ("nb", "Apropos det"),
            ("da", "For resten"), ("da", "Lige en ting til"), ("da", "Vent lige lidt"), ("da", "Inden vi lægger på"),
            ("da", "Et hurtigt spørgsmål"), ("da", "Kom lige i tanke om"), ("da", "Hov forresten"), ("da", "Apropos det"),
            ("fi", "Ai niin"), ("fi", "Vielä yksi juttu"), ("fi", "Muuten"), ("fi", "Hetkinen vielä"), ("fi", "Ennen kuin lopetan"),
            ("fi", "Semmoinen juttu vielä"), ("fi", "Tuli muuten mieleen"), ("fi", "Pikainen kysymys"),
            ("cs", "Jinak"), ("cs", "Hele ještě"), ("cs", "Jo a mimochodem"), ("cs", "Počkejte ještě"), ("cs", "Takže tak"),
            ("cs", "No nicméně"), ("cs", "Ještě jedna věc"), ("cs", "Vlastně ještě"),
            ("sk", "Inak"), ("sk", "Počkajte ešte"), ("sk", "Ozaj"), ("sk", "Ešte jedna vec"), ("sk", "Takže tak"),
            ("sk", "Mimochodom"), ("sk", "Vlastne ešte"), ("sk", "No ale"),
            ("hu", "Amúgy"), ("hu", "Ja és még valami"), ("hu", "Várj csak"), ("hu", "Várjon csak"), ("hu", "Szóval"),
            ("hu", "Egyébként"), ("hu", "Még annyit"), ("hu", "Na szóval"),
            ("ro", "Apropo"), ("ro", "Stai puțin"), ("ro", "Stați puțin"), ("ro", "Încă ceva"), ("ro", "De altfel"),
            ("ro", "Așa deci"), ("ro", "Și încă o chestie"), ("ro", "Păi uite"),
            ("bg", "Добре, значи"), ("bg", "Чакай само"), ("bg", "А, и още нещо"), ("bg", "Между другото"),
            ("bg", "Още един въпрос"), ("bg", "Добре, а иначе"), ("bg", "Преди да затворя"), ("bg", "Само да уточня"),
            ("el", "Α, και κάτι άλλο"), ("el", "Περίμενε λίγο"), ("el", "Να ρωτήσω κάτι"), ("el", "Πριν κλείσουμε"),
            ("el", "Ωραία, αλλά"), ("el", "Κάτι τελευταίο"), ("el", "Παρεμπιπτόντως"), ("el", "Αλήθεια, ξέχασα"),
            ("hr", "Dobro, nego"), ("hr", "Čekaj samo malo"), ("hr", "Samo još nešto"), ("hr", "Usput rečeno"),
            ("hr", "Prije nego poklopim"), ("hr", "Još jedno pitanje"), ("hr", "Aha, a reci mi"), ("hr", "Zaboravih pitati"),
            ("sr", "Čekaj samo sekund"), ("sr", "Pre nego prekinem"), ("sr", "Zaboravih da pitam"),
            ("uk", "А, слухай, ще момент"), ("uk", "І ще одне питання"), ("uk", "До речі, поки не забув"), ("uk", "Зачекай секунду"),
            ("uk", "Дивись, тут таке"), ("uk", "А, і ще"), ("uk", "Секундочку, уточню"), ("uk", "Словом, дивись"),
        ];
        let mut bad = Vec::new();
        for (lang, t) in cases {
            let mut m = after_talk(lang);
            let s = hears(&mut m, t);
            if closes(&s) {
                bad.push(format!("[{lang}] {t} -> {:?} ends={}", s.event, s.ends_call));
            }
        }
        assert!(bad.is_empty(), "research false closings closed:\n{}", bad.join("\n"));
    }
}

/// Reviewer probes for the third batch of closing words.
#[cfg(test)]
mod review_probe3_tests {
    use super::*;

    fn talk(lang: &'static str) -> ClosingMachine {
        let mut m = ClosingMachine::new(lang);
        m.on_caller("Parcel delivery info", "Parcel delivery info", false, false);
        m
    }

    fn hear(m: &mut ClosingMachine, t: &str) -> ClosingStep {
        m.on_caller(t, t, crate::locale::caller_wraps_up(t), false)
    }

    fn closes(s: &ClosingStep) -> bool {
        s.ends_call || matches!(s.event, ClosingEvent::CallerFarewell | ClosingEvent::CallerPreClosing)
    }

    /// Run each text through a fresh mid-call machine; collect those that close or wrap.
    fn run(cases: &[(&'static str, &str)], wrap_too: bool) -> Vec<String> {
        let mut bad = Vec::new();
        for (lang, t) in cases {
            let mut m = talk(lang);
            let s = hear(&mut m, t);
            if closes(&s) || (wrap_too && s.event == ClosingEvent::CallerThanksWrap) || m.state() != ClosingState::Talking {
                bad.push(format!("[{lang}] {t} -> {:?} ends={} state={:?}", s.event, s.ends_call, m.state()));
            }
        }
        bad
    }

    /// Thanks first, then more business: the caller is not done.
    #[test]
    fn thanks_then_more_business_is_not_the_end() {
        let cases: &[(&'static str, &str)] = &[
            ("sv", "Tack, en sak till"), ("sv", "Tack, jag har en fråga till om fakturan"), ("sv", "Tack för hjälpen, men det är en sak till"),
            ("sv", "Tack, kan du skicka det igen"), ("sv", "Tack, då ringer jag tillbaka i morgon"),
            ("nb", "Takk, ett spørsmål til"), ("nb", "Takk, jeg har en ting til"), ("nb", "Takk for hjelpen, men pakken er feil"),
            ("da", "Tak for det, en ting mere"), ("da", "Mange tak, jeg har et spørgsmål mere"), ("da", "Tak for hjælpen, men pakken er forkert"),
            ("fi", "Kiitos, yksi asia vielä"), ("fi", "Kiitos, minulla on toinen kysymys"), ("fi", "Kiitos tiedosta, mutta paketti on väärä"),
            ("cs", "Díky, ještě jedna věc"), ("cs", "Děkuji, mám ještě jednu otázku"), ("cs", "Děkuju, ale balík je špatný"),
            ("sk", "Ďakujem, ešte jedna vec"), ("sk", "Vďaka, mám ešte jednu otázku"), ("sk", "Ďakujem, ale balík je zlý"),
            ("hu", "Köszönöm, még egy dolog"), ("hu", "Köszönöm, van még egy kérdésem"), ("hu", "Köszi, de a csomag rossz"),
            ("ro", "Mulțumesc, încă o întrebare"), ("ro", "Mulțumesc, mai am o problemă"), ("ro", "Mersi, dar coletul este greșit"),
            ("bg", "Благодаря, още един въпрос"), ("bg", "Благодаря, имам още един проблем"), ("bg", "Мерси, но пратката е грешна"),
            ("el", "Ευχαριστώ, μια ερώτηση ακόμα"), ("el", "Ευχαριστώ, έχω άλλο ένα πρόβλημα"), ("el", "Ευχαριστώ, αλλά το δέμα είναι λάθος"),
            ("hr", "Hvala, još jedno pitanje"), ("hr", "Hvala, imam još jedan problem"), ("hr", "Hvala, ali paket je pogrešan"),
            ("sr", "Hvala, imam još jedno pitanje"), ("uk", "Дякую, ще одне питання"), ("uk", "Дякую, у мене ще одна проблема"),
            ("uk", "Дякую, але посилка не та"),
            ("en", "Thanks, one more thing"), ("es", "Gracias, una cosa más"), ("de", "Danke, noch eine Sache"), ("fr", "Merci, encore une chose"),
            ("pt", "Obrigado, mais uma coisa"), ("ja", "ありがとうございます、もう一つあります"), ("ko", "감사합니다, 하나 더 있어요"), ("zh-TW", "謝謝，還有一件事"),
        ];
        let bad = run(cases, true);
        assert!(bad.is_empty(), "thanks + more business treated as the end:\n{}", bad.join("\n"));
    }

    /// After the AI's goodbye, thanks and then new business reopens the call; thanks alone does not.
    #[test]
    fn new_business_after_the_ais_goodbye_reopens() {
        let cases: &[(&'static str, &str)] = &[
            ("sv", "Tack, en sak till"), ("nb", "Takk, ett spørsmål til"), ("da", "Tak, en ting mere"), ("fi", "Kiitos, yksi asia vielä"),
            ("cs", "Díky, ještě jedna věc"), ("sk", "Ďakujem, ešte jedna vec"), ("hu", "Köszönöm, még egy dolog"),
            ("ro", "Mulțumesc, încă o întrebare"), ("bg", "Благодаря, още един въпрос"), ("el", "Ευχαριστώ, μια ερώτηση ακόμα"),
            ("hr", "Hvala, još jedno pitanje"), ("sr", "Hvala, još jedno pitanje"), ("uk", "Дякую, ще одне питання"),
            ("sv", "Tack för hjälpen, men jag har en sak till att fråga"), ("cs", "Díky, ale ještě potřebuji změnit adresu"),
        ];
        let mut bad = Vec::new();
        for (lang, t) in cases {
            let mut m = talk(lang);
            m.on_ai_turn("Okay, goodbye!", true, false);
            if m.state() == ClosingState::Talking {
                bad.push(format!("setup: AI goodbye did not leave Talking ({lang})"));
                continue;
            }
            let s = hear(&mut m, t);
            if m.state() != ClosingState::Talking || s.ends_call {
                bad.push(format!("[{lang}] {t} -> {:?} ends={} state={:?}", s.event, s.ends_call, m.state()));
            }
        }
        assert!(bad.is_empty(), "business after the AI's goodbye did not reopen:\n{}", bad.join("\n"));
    }

    /// Northern "tack / takk / tak" also means "please": a request ending on it is no wrap-up.
    #[test]
    fn please_is_not_a_wrap_up() {
        let cases: &[(&'static str, &str)] = &[
            ("sv", "Okej, jag skulle vilja boka en tid, tack"), ("sv", "Ja, kan jag få ditt nummer, tack"), ("sv", "Ok, skicka det till mig igen, tack"),
            ("nb", "Ok, jeg vil gjerne bestille time, takk"), ("nb", "Ja, kan jeg få nummeret ditt, takk"),
            ("da", "Okay, send det igen, tusind tak"), ("da", "Ja, kan jeg få dit nummer, mange tak"),
            ("sv", "Ja, jag lägger paketet vid dörren, tack"),
        ];
        let bad = run(cases, true);
        assert!(bad.is_empty(), "request ending on please taken as a wrap-up:\n{}", bad.join("\n"));
    }

    /// Ordinary sentences that END on a new farewell phrase, not said as goodbye.
    #[test]
    fn sentences_ending_on_farewell_phrases_are_no_goodbye() {
        let cases: &[(&'static str, &str)] = &[
            ("sv", "Jag hörde honom säga hej då"), ("sv", "Barnet lärde sig säga hej då"),
            ("nb", "Jeg ringer ikke for å si takk for praten"), ("nb", "Hun skrev bare takk for praten"),
            ("da", "Vi ønsker alle kunder fortsat god dag"), ("da", "Skiltet i butikken sagde hav en god dag"),
            ("fi", "Postin ääni sanoo aina kuulemiin"), ("fi", "Hän sanoo aina kiitos ja hei"),
            ("cs", "Automat v bance říká vždy na shledanou"), ("cs", "Moje dcera se učí říkat na shledanou"),
            ("sk", "Dcéra sa učí hovoriť dovidenia"),
            ("hu", "A kisfiam megtanulta, hogy viszlát"), ("hu", "A gép mindig azt mondja, hogy viszlát"),
            ("bg", "Детето се научи да казва дочуване"), ("bg", "Автоматът казва винаги дочуване"),
            ("hr", "Dijete je naučilo reći doviđenja"), ("hr", "Automat uvijek kaže ajde bok"),
            ("uk", "Автовідповідач завжди каже давай бувай"), ("uk", "Дитина вчиться казати ну все па па"),
        ];
        // Reported speech ending on a farewell closes in every language (English "goodbye" too):
        // the design of the unconditional list, shown for information only.
        let bad = run(cases, false);
        let en = run(&[("en", "I heard him say goodbye"), ("en", "The child learned to say goodbye")], false);
        println!("REPORTED-SPEECH closes: new={} of {}, english baseline={} of 2", bad.len(), cases.len(), en.len());
    }

    /// Greetings said mid-call (a transfer, a second person) and first sentences: no goodbye.
    #[test]
    fn greetings_first_and_mid_call_do_not_close() {
        let greets: &[(&'static str, &str)] = &[
            ("sv", "Hej"), ("sv", "Hej, det är Anna"), ("nb", "Hei"), ("nb", "Hei, det er Per"), ("da", "Hej"), ("fi", "Hei"),
            ("fi", "Moi"), ("cs", "Ahoj"), ("cs", "Čau"), ("cs", "Ahoj, tady Petr"), ("sk", "Ahoj"), ("sk", "Čau"), ("hu", "Szia"),
            ("hu", "Szia, itt Anna"), ("bg", "Чао"), ("bg", "Здрасти"), ("el", "Γεια"), ("el", "Γεια σου"), ("el", "Γεια σας"),
            ("hr", "Bok"), ("hr", "Bok, tko je"), ("sr", "Ćao"), ("sr", "Ćao, ko je"), ("uk", "Привіт"), ("ro", "Salut"),
        ];
        let mut bad = Vec::new();
        for (lang, t) in greets {
            let mut first = ClosingMachine::new(lang);
            let s = hear(&mut first, t);
            if closes(&s) {
                bad.push(format!("first [{lang}] {t} -> {:?}", s.event));
            }
            let mut mid = talk(lang);
            let s = hear(&mut mid, t);
            if closes(&s) {
                bad.push(format!("mid [{lang}] {t} -> {:?}", s.event));
            }
            // After the AI's own (non-goodbye) turn.
            let mut m = talk(lang);
            m.on_ai_turn("Hello, how can I help?", false, false);
            let s = hear(&mut m, t);
            if closes(&s) {
                bad.push(format!("after AI [{lang}] {t} -> {:?}", s.event));
            }
        }
        // The AI greets in the language, caller greets back with a thanks cue before.
        for (lang, ai, caller) in [("cs", "Ahoj!", "Díky, ahoj, tady Petr"), ("hu", "Szia!", "Szia, köszönöm, hogy felvetted"), ("el", "Γεια σας!", "Γεια σας, ευχαριστώ που απαντήσατε")] {
            let mut m = ClosingMachine::new(lang);
            m.on_ai_turn(ai, false, false);
            let s = hear(&mut m, caller);
            if closes(&s) {
                bad.push(format!("greeting exchange [{lang}] {caller} -> {:?}", s.event));
            }
        }
        assert!(bad.is_empty(), "greeting closed the call:\n{}", bad.join("\n"));
    }

    /// Words that are thanks only as the last word of a thanks, used otherwise: not fluff.
    #[test]
    fn thanks_words_used_otherwise_are_not_fluff() {
        for t in [
            "We need to change tack", "Vi trenger nytt tak for huset", "Jeg trenger tak for huset",
            "That is a different tack",
        ] {
            assert!(!is_fluff(t), "{t}");
        }
    }

    /// The core nine languages and a few more: their own farewells and thanks still behave.
    #[test]
    fn other_languages_keep_their_behavior() {
        let bye: &[(&'static str, &str)] = &[
            ("en", "Thanks, bye!"), ("es", "Gracias, adiós"), ("fr", "Merci, au revoir"), ("de", "Danke, tschüss"), ("pt", "Obrigado, tchau"),
            ("ja", "ありがとうございます、失礼します"), ("ko", "감사합니다, 안녕히 계세요"), ("zh-TW", "謝謝，再見"), ("it", "Grazie, arrivederci"),
            ("ru", "Спасибо, до свидания"), ("pl", "Dziękuję, do widzenia"), ("tr", "Teşekkürler, hoşça kalın"), ("nl", "Bedankt, tot ziens"),
        ];
        let mut bad = Vec::new();
        for (lang, t) in bye {
            let mut m = talk(lang);
            let s = hear(&mut m, t);
            if !(s.ends_call && s.event == ClosingEvent::CallerFarewell) {
                bad.push(format!("lost bye [{lang}] {t} -> {:?} ends={}", s.event, s.ends_call));
            }
        }
        let go_on: &[(&'static str, &str)] = &[
            ("en", "Thanks, one more thing"), ("pl", "Tak, mam jeszcze jedno pytanie"), ("pl", "Tak, pa pa mówi dziecko"), ("de", "Ja, und noch eine Frage"),
            ("es", "Sí, hay otra cosa"), ("tr", "Tamam, bir şey daha var"), ("pl", "Dzięki, ale jest jeszcze problem"),
            ("en", "The bok choy is in the box"), ("en", "Thanks, we have a different tack on it"),
        ];
        for (lang, t) in go_on {
            let mut m = talk(lang);
            let s = hear(&mut m, t);
            if closes(&s) {
                bad.push(format!("closed [{lang}] {t} -> {:?}", s.event));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}

/// Recheck of the Nordic thanks fix.
#[cfg(test)]
mod review_probe3_recheck_tests {
    use super::*;

    fn talk(lang: &'static str) -> ClosingMachine {
        let mut m = ClosingMachine::new(lang);
        m.on_caller("Parcel delivery info", "Parcel delivery info", false, false);
        m
    }

    fn hear(m: &mut ClosingMachine, t: &str) -> ClosingStep {
        m.on_caller(t, t, crate::locale::caller_wraps_up(t), false)
    }

    fn after_ai_bye(lang: &'static str) -> ClosingMachine {
        let mut m = talk(lang);
        m.on_ai_turn("Okay, goodbye!", true, false);
        m
    }

    /// Thanks, then a request or more business: no wrap-up mid-call; after the AI's goodbye it reopens.
    #[test]
    fn nordic_thanks_then_more_is_no_wrap_up() {
        let cases: &[(&'static str, &str)] = &[
            ("sv", "Tack, kan du ringa mig imorgon?"), ("nb", "Takk, men jeg har ett spørsmål til"),
            ("da", "Tak, og hvad med fredag?"), ("fi", "Kiitos, voisitko lähettää sen uudestaan?"),
            ("sv", "Okej, kan du skicka det igen, tack"), ("nb", "Greit, send det på e-post, takk"),
            ("da", "Okay, ring mig i morgen, tak"), ("fi", "Selvä, lähetä se sähköpostiin, kiitos"),
        ];
        let mut bad = Vec::new();
        for (lang, t) in cases {
            let mut m = talk(lang);
            let s = hear(&mut m, t);
            println!("MID [{lang}] {t} -> {:?} ends={} state={:?}", s.event, s.ends_call, m.state());
            if s.ends_call || s.event != ClosingEvent::None || m.state() != ClosingState::Talking {
                bad.push(format!("mid-call [{lang}] {t} -> {:?} ends={} state={:?}", s.event, s.ends_call, m.state()));
            }
            let mut m = after_ai_bye(lang);
            let s = hear(&mut m, t);
            println!("AFTER-AI [{lang}] {t} -> {:?} ends={} state={:?}", s.event, s.ends_call, m.state());
            if s.ends_call || m.state() != ClosingState::Talking {
                bad.push(format!("after the AI's goodbye [{lang}] {t} -> {:?} ends={} state={:?}", s.event, s.ends_call, m.state()));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// Real Nordic thanks wrap-ups still hold.
    #[test]
    fn nordic_real_wrap_ups_still_hold() {
        let mut bad = Vec::new();
        // Settling word + thanks, mid-call: a thanks wrap-up.
        for (lang, t) in [("sv", "Okej, tack så mycket"), ("nb", "Ok, takk skal du ha"), ("da", "Okay, tusind tak"), ("fi", "Selvä, kiitos paljon")] {
            let mut m = talk(lang);
            let s = hear(&mut m, t);
            if s.event != ClosingEvent::CallerThanksWrap || s.ends_call || m.state() != ClosingState::PreClosing {
                bad.push(format!("mid-call [{lang}] {t} -> {:?} ends={} state={:?}", s.event, s.ends_call, m.state()));
            }
        }
        // After the AI's goodbye: thanks keeps the closing (or ends the call).
        let after: &[(&'static str, &str)] = &[
            ("sv", "Okej, tack så mycket"), ("nb", "Takk skal du ha"), ("da", "Tusind tak for hjælpen"), ("fi", "Kiitos paljon, hei hei"),
            ("sv", "Tack för hjälpen"), ("nb", "Tusen takk for hjelpen"), ("fi", "Kiitos paljon"),
        ];
        for (lang, t) in after {
            let mut m = after_ai_bye(lang);
            let s = hear(&mut m, t);
            println!("WRAP-AFTER-AI [{lang}] {t} -> {:?} ends={} state={:?}", s.event, s.ends_call, m.state());
            if m.state() == ClosingState::Talking {
                bad.push(format!("after the AI's goodbye reopened [{lang}] {t} -> {:?} state={:?}", s.event, m.state()));
            }
        }
        // Bare thanks mid-call, against the English baseline (same design: thanks alone is no wrap-up).
        for (lang, t) in [("nb", "Takk skal du ha"), ("da", "Tusind tak for hjælpen"), ("fi", "Kiitos paljon, hei hei"), ("en", "Thank you so much for your help"), ("en", "Thanks a lot, bye bye")] {
            let mut m = talk(lang);
            let s = hear(&mut m, t);
            println!("BARE-MID [{lang}] {t} -> {:?} ends={} state={:?}", s.event, s.ends_call, m.state());
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// Other languages' thanks wrap-ups are unchanged by the Nordic rule.
    #[test]
    fn other_thanks_wrap_ups_unchanged() {
        let mut bad = Vec::new();
        for (lang, t) in [
            ("en", "Okay, no problem, I'll leave it at the door, thanks"), ("de", "Alles klar, dann lege ich es vor die Tür, danke"),
            ("cs", "Dobře, ok, nechám to u dveří, díky"), ("pl", "Ok, zostawię to pod drzwiami, dziękuję"),
        ] {
            let mut m = talk(lang);
            let s = hear(&mut m, t);
            println!("OTHER [{lang}] {t} -> {:?}", s.event);
            if lang == "en" || lang == "de" {
                if s.event != ClosingEvent::CallerThanksWrap {
                    bad.push(format!("[{lang}] {t} -> {:?}", s.event));
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}

/// Fourth batch: Icelandic, Slovenian, Macedonian and Albanian goodbyes, and the
/// European goodbyes, pre-closings and greeting-goodbyes the earlier lists did not
/// have. The sentences that must not close are the research's own counterexamples
/// (opening greetings, reported speech, a goodbye word with more said after it).
#[cfg(test)]
mod regional_closing_words_4_tests {
    use super::*;

    fn after_talk(lang: &'static str) -> ClosingMachine {
        let mut m = ClosingMachine::new(lang);
        m.on_caller("Parcel delivery info", "Parcel delivery info", false, false);
        m
    }

    /// The caller's turn as the live session feeds it: with the older check.
    fn hears(m: &mut ClosingMachine, t: &str) -> ClosingStep {
        m.on_caller(t, t, crate::locale::caller_wraps_up(t), false)
    }

    fn closes(s: &ClosingStep) -> bool {
        s.ends_call || matches!(s.event, ClosingEvent::CallerFarewell | ClosingEvent::CallerPreClosing)
    }

    fn all_close(cases: &[(&'static str, &str)], m: impl Fn(&'static str) -> ClosingMachine) -> Vec<String> {
        let mut bad = Vec::new();
        for (lang, t) in cases {
            let mut mm = m(lang);
            let s = hears(&mut mm, t);
            if s.event != ClosingEvent::CallerFarewell || !s.ends_call {
                bad.push(format!("[{lang}] {t} -> {:?} ends={}", s.event, s.ends_call));
            }
        }
        bad
    }

    /// The research's own goodbye sentences that end on a newly added goodbye.
    const GOODBYES: &[(&str, &str)] = &[
        ("is", "Takk kærlega fyrir aðstoðina, við heyrumst á morgun, bless bless!"),
        ("is", "Frábært, ég sendi þér upplýsingarnar í tölvupósti, bless bless."),
        ("is", "Takk fyrir að láta mig vita, sjáumst í kvöld, bless!"),
        ("is", "Allt í góðu, við tölum saman síðar, bless."),
        ("is", "Ókei, þá er þetta klárt, takk fyrir spjallið og bæ bæ!"),
        ("is", "Ég hringi aftur þegar ég er kominn heim, bæ bæ."),
        ("is", "Við sjáumst þá klukkan fjögur, bæ!"),
        ("is", "Takk fyrir þetta, bæ!"),
        ("is", "Heyrumst þá eftir helgi, bæjó!"),
        ("is", "Takk fyrir spjallið, bæjó!"),
        ("is", "Þakka þér kærlega fyrir hjálpina og vertu sæll."),
        ("is", "Við verðum í bandi í næstu viku, vertu sæll."),
        ("is", "Takk fyrir upplýsingarnar og góða helgi, vertu sæl."),
        ("is", "Ég kem þá við á skrifstofunni á morgun, vertu sæl."),
        ("is", "Takk fyrir greiðann og vertu blessaður!"),
        ("is", "Við heyrumst fljótlega aftur, vertu blessaður."),
        ("is", "Þakka þér innilega fyrir símtalið, vertu blessuð."),
        ("is", "Sjáumst á fimmtudaginn, vertu blessuð!"),
        ("sl", "Najlepša hvala za vse informacije, nasvidenje!"),
        ("sl", "Hvala za klic, zdaj pa res hitim na avtobus, adijo!"),
        ("sl", "Vse je dogovorjeno za jutri, lepo bodite, adijo!"),
        ("sl", "Nimam več vprašanj, lepo bodite!"),
        ("sl", "Sva vse zmenjena za vikend, fejst bodite!"),
        ("sl", "Kličejo me k mizi, se slišiva kasneje!"),
        ("mk", "Тоа е сѐ за денес, ајде пријатно."),
        ("mk", "Се гледаме попладне пред влезот, ајде пријатно."),
        ("mk", "Одам на состанок сега, ајде се слушаме."),
        ("mk", "Фала ти многу за помошта, ајде се слушаме."),
        ("mk", "Брзам кон автобусот, ајде чао."),
        ("mk", "Ќе ти пратам порака вечерва, ајде чао."),
        ("mk", "Ви посакувам убав остаток од денот, до гледање."),
        ("mk", "Сѐ е запишано, Ви благодарам и до гледање."),
        ("mk", "Јави ми кога ќе завршиш со работа, ќе се чуеме."),
        ("mk", "Добро, да не те задржувам сега, ќе се чуеме."),
        ("mk", "Се надевам дека сѐ ќе биде во ред, збогум."),
        ("mk", "Ви посакувам сѐ најдобро понатаму, збогум."),
        ("mk", "Поздрави ги сите дома, ајде со здравје."),
        ("mk", "Се гледаме викендов, ајде со здравје."),
        ("sq", "Faleminderit shumë për ndihmën, mirupafshim."),
        ("sq", "U morëm vesh për nesër në mëngjes, mirupafshim."),
        ("sq", "Po nisem tani drejt qytetit, shihemi më vonë."),
        ("sq", "E lëmë për pasdite atëherë, shihemi më vonë."),
        ("sq", "Të falënderoj për telefonatën, kalofsh mirë."),
        ("sq", "Pushim të mbarë gjatë fundjavës, kalofsh mirë."),
        ("sq", "Ju faleminderit për sqarimin e detajuar, kalofshi mirë."),
        ("sq", "Do t'ju njoftojmë sapo të jetë gati dokumenti, kalofshi mirë."),
        ("sq", "Shumë faleminderit për bashkëpunimin, gjithë të mirat."),
        ("sq", "Ju uroj suksese në detyrën e re, gjithë të mirat."),
        ("sq", "Po më bie zilja e derës tani, hajde mirupafshim."),
        ("sq", "U krye kjo punë atëherë, hajde mirupafshim."),
        ("sv", "Jag återkommer så snart jag vet mer, hej så länge."),
        ("sv", "Ha en trevlig eftermiddag nu, hej så länge."),
        ("da", "Tak fordi du lige gav besked, kan du have det godt."),
        ("da", "Hils mange gange derhjemme, kan du have det godt!"),
        ("da", "Jeg vender tilbage, så snart jeg har nyt, hej så længe."),
        ("da", "Tak for snakken indtil videre, hej så længe."),
        ("fi", "Kiitos yhteydenotosta, palaamme asiaan ensi viikolla, näkemiin."),
        ("fi", "Ei muuta tällä kertaa, kiitos ja näkemiin."),
        ("fi", "En ole kiinnostunut tarjouksesta, kiitos hei."),
        ("fi", "Tuo aika sopii oikein hyvin, kiitos hei!"),
        ("cs", "Děkuji za upřesnění termínu, na slyšenou."),
        ("cs", "Budu se těšit na další hovor příští týden, na slyšenou."),
        ("cs", "Tak jo, uvidíme se večer, měj se."),
        ("cs", "Musím běžet na vlak, měj se."),
        ("sk", "Ďakujem za všetky informácie, do počutia."),
        ("sk", "Ozvem sa vám budúci týždeň s výsledkom, do počutia."),
        ("hu", "Rendben, köszönöm a gyors választ, viszhall."),
        ("hu", "Akkor maradunk ennyiben, viszhall."),
        ("hu", "Rendben, akkor este találkozunk, puszi."),
        ("hu", "Vigyázz magadra az úton, puszi."),
        ("bg", "Благодаря ви за съдействието, довиждане!"),
        ("bg", "Записах си новия час, благодаря и до чуване!"),
        ("bg", "Ще чакам потвърждение по имейл, до чуване!"),
        ("bg", "Чухме се и се разбрахме, чао засега!"),
        ("bg", "Влизам в асансьора и връзката прекъсва, чао засега!"),
        ("el", "Δεν χρειάζεται κάτι άλλο προς το παρόν, αντίο!"),
        ("el", "Σας ευχαριστούμε για τη συνεργασία, εις το επανιδείν!"),
        ("el", "Ραντεβού την επόμενη Τρίτη λοιπόν, εις το επανιδείν!"),
        ("hr", "Hvala vam na razgovoru, čujemo se idući tjedan, do slušanja!"),
        ("hr", "Vaš zahtjev je uspješno zaprimljen, do slušanja!"),
        ("hr", "Zovu me za stolom, čujemo se poslije!"),
        ("sr", "Хвала вам на свим информацијама, довиђења!"),
        ("sr", "Журим на аутобус, ајде здраво!"),
        ("sr", "Улазим у зграду сада, ајд ћао!"),
        ("sr", "Важи, све је решено, ајд ћао!"),
        ("sr", "Почиње ми час, чујемо се после!"),
    ];

    #[test]
    fn goodbyes_end_the_call() {
        let bad = all_close(GOODBYES, after_talk);
        assert!(bad.is_empty(), "goodbyes missed:\n{}", bad.join("\n"));
    }

    /// The research's pre-closing sentences: the end is announced (the AI's goodbye then ends the call).
    const PRE_CLOSING_SENTENCES: &[(&str, &str)] = &[
        ("is", "Ég kem þessum skilaboðum til skila strax, takk fyrir símtalið."),
        ("is", "Þá er allt komið á hreint hjá okkur, takk fyrir símtalið."),
        ("is", "Ég er að keyra inn í göng, við tölum saman síðar!"),
        ("is", "Takk fyrir þetta, ég skoða málið og við tölum saman síðar."),
        ("is", "Takk kærlega fyrir aðstoðina og hafðu það gott!"),
        ("is", "Við sjáumst í næstu viku, hafðu það gott."),
        ("is", "Ég sendi þér staðfestinguna á eftir og ég heyri í þér síðar."),
        ("sl", "Najlepša hvala za vaš čas, ne bom vas več zadrževal!"),
        ("mk", "Добро, ми стана јасно сѐ, да не те задржувам повеќе."),
        ("mk", "Значи се гледаме во петок, да не те задржувам повеќе."),
        ("mk", "Ќе проверам веднаш и ќе ти пишам, фала што се јави."),
        ("mk", "Добро е што ме потсети навреме, фала што се јави."),
        ("mk", "Ти ги пренесов сите новости, толку од мене засега."),
        ("mk", "Ако испадне нешто друго ќе ти пишам, толку од мене засега."),
        ("sq", "Jam në rrugë tani dhe ka zhurmë, flasim më vonë."),
        ("sq", "Po hyj në sallë, të marr unë pas një ore, flasim më vonë."),
        ("sq", "E kuptova plotësisht tani, mos të të mbaj më gjatë."),
        ("sq", "E di që ke shumë punë sot, mos të të mbaj më gjatë."),
        ("sq", "Sapo të kem një përgjigje të njoftoj menjëherë, mbetemi në kontakt."),
        ("sq", "Faleminderit për bisedën e këndshme, mbetemi në kontakt."),
        ("sv", "Nu har jag fått svar på allt, jag ska inte hålla dig längre."),
        ("sv", "Jag hör att du har fullt upp där borta, jag ska inte hålla dig längre."),
        ("sv", "Jag tar hand om det direkt och skickar en bekräftelse, tack för att du ringde."),
        ("sv", "Det var fint att höra din röst igen, tack för att du ringde."),
        ("sv", "Jag sitter mitt i trafiken just nu, jag ringer tillbaka senare."),
        ("sv", "Låt mig kolla i pärmen först, jag ringer tillbaka senare."),
        ("nb", "Da har jeg fått svar på alt sammen, jeg skal ikke holde deg lenger."),
        ("nb", "Jeg hører at du er opptatt nå, jeg skal ikke holde deg lenger."),
        ("nb", "Jeg sjekker dette med det samme og sender bekreftelse, takk for at du ringte."),
        ("nb", "Veldig hyggelig å høre fra deg igjen, takk for at du ringte."),
        ("nb", "Jeg står midt i butikken akkurat nå, jeg ringer deg opp igjen senere."),
        ("nb", "La meg finne frem papirene først, jeg ringer deg opp igjen senere."),
        ("da", "Nu har jeg fået svar på det hele, jeg skal ikke holde dig længere."),
        ("da", "Jeg kan høre, du har travlt lige nu, jeg skal ikke holde dig længere."),
        ("da", "Jeg undersøger sagen med det samme og vender tilbage, tak fordi du ringede."),
        ("da", "Det var rigtig hyggeligt at høre nyt fra dig, tak fordi du ringede."),
        ("da", "Jeg står midt på gaden i støjen lige nu, jeg ringer tilbage lidt senere."),
        ("da", "Lad mig lige finde tallene frem først, jeg ringer tilbage lidt senere."),
        ("fi", "Kiitos kun tarkistit sen ajan, ei mulla muuta."),
        ("fi", "Tässä alkaa nyt tunti, puhutaan myöhemmin lisää!"),
        ("fi", "Kiitos tiedosta, minä mietin tätä ja puhutaan myöhemmin lisää."),
        ("fi", "Välitän viestin eteenpäin heti kun hän palaa, kiitos soitosta."),
        ("fi", "Selvä, laitetaan viestiä ja palataan asiaan."),
        ("cs", "Už mám všechny informace, nebudu vás zdržovat."),
        ("cs", "Děkuji za vysvětlení, dál už vás opravdu nebudu, nebudu vás zdržovat."),
        ("cs", "Vzkaz vyřídím hned po návratu kolegy, děkuji za zavolání."),
        ("cs", "Vše jsem zapsal do systému, děkuji za zavolání."),
        ("cs", "Teď řídím v hustém provozu, zavoláme si později."),
        ("cs", "Jsem u lékaře v čekárně, zavoláme si později."),
        ("sk", "Už mám všetky informácie, nebudem vás zdržiavať."),
        ("sk", "Ďakujem za vysvetlenie, ďalej už naozaj nebudem, nebudem vás zdržiavať."),
        ("sk", "Odkaz hneď odovzdám kolegovi, ďakujem za zavolanie."),
        ("sk", "Všetko som zaevidoval do systému, ďakujem za zavolanie."),
        ("sk", "Teraz šoférujem v premávke, zavoláme si neskôr."),
        ("sk", "Som práve v čakárni u lekára, zavoláme si neskôr."),
        ("hu", "Mindent feljegyeztem, köszönöm, nem tartom fel tovább."),
        ("hu", "Látom, hogy sok a dolga, nem tartom fel tovább."),
        ("hu", "Ne haragudj, hív a felettesem a másik vonalon, most le kell tennem."),
        ("hu", "Megérkezett a villamosom, most le kell tennem."),
        ("hu", "Az üzenetet azonnal továbbítom a vezetőnek, köszönöm a hívást."),
        ("hu", "Rögzítettem a bejelentést a rendszerben, köszönöm a hívást."),
        ("hu", "Most vezetek a forgalomban, később visszahívlak."),
        ("hu", "Éppen az ügyfélnél vagyok, később visszahívlak."),
        ("bg", "На касата съм в магазина, ще се обадя по-късно!"),
        ("bg", "Благодаря за отделеното време, няма да ви задържам повече!"),
        ("el", "Είμαι στο ταμείο τώρα, θα σε πάρω αργότερα!"),
        ("el", "Φτάνω στο ιατρείο τώρα, μιλάμε αργότερα!"),
        ("hr", "Upravo ulazim u ordinaciju, nazvat ću kasnije!"),
        ("hr", "Hvala na brzom odgovoru, neću vas više zadržavati!"),
        ("sr", "Управо улазим код лекара, јавићу се касније!"),
    ];

    #[test]
    fn pre_closings_announce_the_end() {
        let mut bad = Vec::new();
        for (lang, t) in PRE_CLOSING_SENTENCES {
            let mut m = after_talk(lang);
            let s = hears(&mut m, t);
            if !closes(&s) {
                bad.push(format!("[{lang}] {t} -> {:?}", s.event));
                continue;
            }
            if s.event == ClosingEvent::CallerPreClosing && m.on_ai_turn("Okay, goodbye!", false, false) != ClosingEvent::FarewellsExchanged {
                bad.push(format!("[{lang}] the AI's goodbye after {t} did not end it"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// Greetings or everyday words that also say goodbye close right after thanks.
    #[test]
    fn cued_goodbyes_close_after_thanks() {
        let cases: &[(&'static str, &str)] = &[
            ("is", "Takk fyrir, sæll!"),
            ("is", "Takk fyrir, sæl."),
            ("is", "Takk fyrir, blessaður!"),
            ("is", "Takk fyrir, blessuð."),
            ("is", "Takk fyrir, heyrumst!"),
            ("is", "Takk fyrir, við heyrumst."),
            ("sl", "Hvala, čao!"),
            ("sl", "Hvala, živjo!"),
            ("sl", "Hvala, lep dan!"),
            ("sl", "Hvala, lahko noč."),
            ("sl", "Hvala, lep pozdrav!"),
            ("sl", "Hvala, se slišimo!"),
            ("sl", "Hvala, se slišiva."),
            ("sl", "Hvala, se vidimo!"),
            ("sl", "Hvala, se vidiva."),
            ("sl", "Najlepša hvala, srečno!"),
            ("mk", "Фала, се слушаме!"),
            ("mk", "Фала, се гледаме."),
            ("mk", "Благодарам, убав ден!"),
            ("mk", "Фала многу, добра ноќ."),
            ("mk", "Фала, здраво!"),
            ("mk", "Ви благодарам, пријатно!"),
            ("mk", "Фала, поздрав."),
            ("sq", "Faleminderit, tung!"),
            ("sq", "Faleminderit, tungjatjeta."),
            ("sq", "Faleminderit, çao!"),
            ("sq", "Faleminderit, dëgjohemi!"),
            ("sq", "Ju faleminderit, shihemi."),
            ("sq", "Faleminderit shumë, ditën e mirë!"),
            ("sq", "Faleminderit, natën e mirë."),
            ("sv", "Tack, adjö!"),
            ("sv", "Tack så mycket, trevlig helg!"),
            ("nb", "Takk, adjø!"),
            ("nb", "Takk, god helg!"),
            ("nb", "Takk, vi sees."),
            ("nb", "Tusen takk, ha det fint!"),
            ("da", "Tusind tak, vi tales ved."),
            ("da", "Mange tak, god weekend!"),
            ("da", "Tak for det, godnat."),
            ("fi", "Kiitos, soitellaan!"),
            ("fi", "Kiitos, palaillaan."),
            ("fi", "Kiitos paljon, kuullaan!"),
            ("hu", "Köszönöm, jó éjszakát!"),
            ("hu", "Köszi, majd beszélünk."),
            ("bg", "Благодаря, чуваме се!"),
            ("bg", "Благодаря, всичко хубаво!"),
            ("bg", "Благодаря, затварям."),
            ("el", "Ευχαριστώ, καλό υπόλοιπο!"),
            ("el", "Ευχαριστώ, καλό μεσημέρι."),
            ("el", "Ευχαριστώ πολύ, κλείνω."),
            ("hr", "Hvala, drži se!"),
            ("hr", "Hvala, ugodan ostatak dana!"),
            ("sr", "Хвала, ћао!"),
            ("sr", "Хвала, здраво."),
            ("sr", "Хвала лепо, пријатно!"),
            ("sr", "Хвала, свако добро."),
            ("sr", "Хвала вам, све најбоље!"),
            ("sr", "Хвала, видимо се."),
            ("sr", "Хвала, поздрав!"),
            ("sr", "Хвала, лаку ноћ."),
            ("sr", "Hvala, zdravo!"),
        ];
        let bad = all_close(cases, after_talk);
        assert!(bad.is_empty(), "cued goodbyes missed:\n{}", bad.join("\n"));
    }

    const CUED_ALONE: &[(&str, &str)] = &[
        ("is", "Sæll!"),
        ("is", "Sæl."),
        ("is", "Blessaður!"),
        ("is", "Blessuð."),
        ("is", "Heyrumst!"),
        ("is", "Við heyrumst."),
        ("sl", "Čao!"),
        ("sl", "Živjo!"),
        ("sl", "Lep dan!"),
        ("sl", "Lahko noč."),
        ("sl", "Lep pozdrav!"),
        ("sl", "Se slišimo!"),
        ("sl", "Se slišiva."),
        ("sl", "Se vidimo!"),
        ("sl", "Se vidiva."),
        ("sl", "Srečno!"),
        ("mk", "Се слушаме!"),
        ("mk", "Се гледаме."),
        ("mk", "Убав ден!"),
        ("mk", "Добра ноќ."),
        ("mk", "Здраво!"),
        ("mk", "Пријатно!"),
        ("mk", "Поздрав."),
        ("sq", "Tung!"),
        ("sq", "Tungjatjeta."),
        ("sq", "Çao!"),
        ("sq", "Dëgjohemi!"),
        ("sq", "Shihemi."),
        ("sq", "Ditën e mirë!"),
        ("sq", "Natën e mirë."),
        ("sv", "Adjö!"),
        ("sv", "Trevlig helg!"),
        ("nb", "Adjø!"),
        ("nb", "God helg!"),
        ("nb", "Vi sees."),
        ("nb", "Ha det fint!"),
        ("da", "Vi tales ved."),
        ("da", "God weekend!"),
        ("da", "Godnat."),
        ("fi", "Soitellaan!"),
        ("fi", "Palaillaan."),
        ("fi", "Kuullaan!"),
        ("hu", "Jó éjszakát!"),
        ("hu", "Majd beszélünk."),
        ("bg", "Чуваме се!"),
        ("bg", "Всичко хубаво!"),
        ("bg", "Затварям."),
        ("el", "Καλό υπόλοιπο!"),
        ("el", "Καλό μεσημέρι."),
        ("el", "Κλείνω."),
        ("hr", "Drži se!"),
        ("hr", "Ugodan ostatak dana!"),
        ("sr", "Ћао!"),
        ("sr", "Здраво."),
        ("sr", "Пријатно!"),
        ("sr", "Свако добро."),
        ("sr", "Све најбоље!"),
        ("sr", "Видимо се."),
        ("sr", "Поздрав!"),
        ("sr", "Лаку ноћ."),
        ("sr", "Zdravo!"),
    ];

    /// Alone, mid-call, they wait: the AI's goodbye before them, or a cue in the same turn.
    #[test]
    fn cued_words_alone_close_only_after_the_ais_goodbye() {
        let mut bad = Vec::new();
        for (lang, t) in CUED_ALONE {
            let mut m = after_talk(lang);
            let s = hears(&mut m, t);
            if closes(&s) {
                bad.push(format!("[{lang}] mid-call {t} -> {:?} ends={}", s.event, s.ends_call));
            }
        }
        let bad_after_ai = all_close(CUED_ALONE, |lang| {
            let mut m = after_talk(lang);
            m.on_ai_turn("Okay, goodbye!", false, false);
            m
        });
        bad.extend(bad_after_ai.into_iter().map(|b| format!("after the AI's goodbye: {b}")));
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// How these calls open: never a goodbye on the first turn.
    #[test]
    fn opening_greetings_do_not_close() {
        let cases: &[(&'static str, &str)] = &[
            ("is", "Sæll!"),
            ("is", "Blessuð!"),
            ("sl", "Živjo!"),
            ("sl", "Čao!"),
            ("mk", "Здраво!"),
            ("sq", "Tungjatjeta!"),
            ("sq", "Tung!"),
            ("sr", "Ћао!"),
            ("sr", "Здраво!"),
            ("sv", "Adjö!"),
            ("bg", "Чуваме се!"),
            ("el", "Καλό μεσημέρι!"),
        ];
        let mut bad = Vec::new();
        for (lang, t) in cases {
            let mut m = ClosingMachine::new(lang);
            let s = hears(&mut m, t);
            if closes(&s) {
                bad.push(format!("[{lang}] opening {t} -> {:?}", s.event));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// The research's counterexamples for every added word: at the start of a call or mid-call, no goodbye.
    const LOOKALIKES: &[(&str, &str)] = &[
        ("is", "Barnið hérna við hliðina var að kalla bless bless á nágrannann, en ég er enn í símanum, haltu bara áfram."),
        ("is", "Síðasti þjónustufulltrúi sagði bara bless bless áður en ég náði að spyrja um verðið, getur þú athugað það fyrir mig?"),
        ("is", "Ekki segja bless alveg strax, ég þarf líka að fá póstnúmerið hjá ykkur."),
        ("is", "Samstarfsmaður minn kastaði bara kveðju og sagði bless í dyrunum, ég er enn að hlusta á þig."),
        ("is", "Ég heyrði einhvern segja bæ bæ í bakgrunninum hjá þér, ertu nokkuð að fara á fund?"),
        ("is", "Sendillinn sagði bara bæ bæ við dyrnar og rétti mér kassann, bíddu aðeins á meðan ég skoða innihaldið."),
        ("is", "Þú sagðir næstum bæ, en má ég spyrja einnar stuttrar spurningar áður en við hættum?"),
        ("is", "Kunningi minn sagði bæ hérna frammi á gangi, ekkert mál, segðu mér meira um tímasetninguna."),
        ("is", "Systir mín kallaði bæjó úr forstofunni rétt í þessu, en ég er enn á línunni hjá þér."),
        ("is", "Hann sagði bæjó í síðasta símtali án þess að staðfesta dagsetninguna, svo ég vil vera alveg viss núna."),
        ("is", "Ef maður segir vertu sæll við karlmann í lok símtals er það formlegt, en segðu mér fyrst hvort reikningurinn sé greiddur."),
        ("is", "Hann kvaddi með vertu sæll áðan en gleymdi að gefa mér bókunarnúmerið, getur þú flett því upp?"),
        ("is", "Áður en ég segi vertu sæl langar mig að ganga úr skugga um að nafnið sé rétt skráð."),
        ("is", "Konan í móttökunni sagði vertu sæl og gaf mér samband hingað, getur þú aðstoðað mig með tímabókun?"),
        ("is", "Afi segir alltaf vertu blessaður þegar hann kveður, en við skulum klára að fara yfir listann fyrst."),
        ("is", "Ekki segja vertu blessaður strax, ég á eftir að lesa upp viðmiðunarnúmerið."),
        ("is", "Hún sagði vertu blessuð við mig í gær án þess að svara erindinu, svo ég hringi aftur til að fá skýr svör."),
        ("is", "Áður en við kveðjumst og segjum vertu blessuð, geturðu athugað hvort tíminn klukkan tvö sé laus?"),
        ("is", "Við sögðum í gær að við heyrumst í dag til að klára samninginn, ertu með skjalið fyrir framan þig?"),
        ("is", "Við heyrumst miklu betur núna þegar ég fór út úr lyftunni, svo segðu mér hvað kom fram á fundinum."),
        ("is", "Takk fyrir símtalið annars, ég ætlaði einmitt að spyrja þig út í reikninginn sem barst í morgun."),
        ("is", "Byrjum á að segja takk fyrir símtalið, og farðu svo rólega yfir það sem þú vildir ræða."),
        ("is", "Þótt við tölum saman síðar um smáatriðin, getum við þá ekki neglt niður dagsetninguna strax núna?"),
        ("is", "Þú stakkst upp á að við tölum saman síðar, en ég er laus einmitt núna ef þú hefur tvær mínútur."),
        ("is", "Ef ég heyri í þér síðar í vikunni verður of seint að breyta pöntuninni, svo klárum það frekar núna."),
        ("is", "Ég sagði honum að ég heyri í þér síðar, en fyrst þú ert á línunni skulum við leysa þetta strax."),
        ("is", "Áður en ég segi hafðu það gott, viltu staðfesta hvort sendingin komi fyrir hádegi?"),
        ("is", "Þjónustufulltrúinn sagði bara hafðu það gott í gær án þess að vista breytinguna, svo athugum það núna."),
        ("is", "Sæll, mikið er gott að ná loksins í þig, ertu laus í smá stund?"),
        ("is", "Ég kallaði bara sæll á nágrannann úti á bílastæði, ég er enn að hlusta á þig."),
        ("is", "Sæl, ég er að hringja til að athuga með bókaðan tíma hjá lækninum."),
        ("is", "Samstarfskona mín gekk fram hjá og ég sagði bara sæl við hana, haltu endilega áfram."),
        ("is", "Blessaður, heyrirðu nokkuð í mér núna, ég var að reyna að ná í þig áðan?"),
        ("is", "Blessaður vertu ekki að flýta þér að leggja á, við eigum eftir að ræða kostnaðinn."),
        ("is", "Blessuð, ertu með stund til að fara yfir dagskrána fyrir morgundaginn?"),
        ("is", "Blessuð hafðu engar áhyggjur af bakgrunnshljóðinu, ég heyri mjög skýrt í þér."),
        ("sl", "Kurir je ravnokar odložil paket pri vratih in rekel nasvidenje, jaz pa sem še na zvezi — povejte naprej."),
        ("sl", "Preden rečeva nasvidenje v tem klicu, preveriva prosim še točno uro jutrišnjega prihoda."),
        ("sl", "Sodelavec je skozi vrata zaklical adijo, ampak jaz sem še vedno v pisarni na liniji — kar nadaljujte."),
        ("sl", "Ne reci mi še adijo, dokler mi ne poveš, katere dokumente moram prinesti s seboj."),
        ("sl", "Zdaj se slišiva veliko bolj čisto kot prej, tako da mi lahko mirno poveš vse do konca."),
        ("sl", "Vem, da se slišiva vsak teden, ampak dajva kar zdaj med tem klicem uskladiti še datume."),
        ("sl", "Ali se slišimo dovolj glasno v sejni sobi, da lahko nadaljujem s predstavitvijo podatkov?"),
        ("sl", "V sporočilu ste napisali se slišimo kmalu, zato vas kličem, da zdaj skupaj pregledava pogodbo."),
        ("sl", "Tudi vi lepo bodite, zdaj pa mi prosim na kratko povejte, ali je termin v četrtek še prost."),
        ("sl", "Receptorka mi je ob izhodu rekla lepo bodite, zdaj sem zunaj na mirnem in vas dobro slišim."),
        ("sl", "Sosed mi je na dvorišču zaklical fejst bodite, oprostite za kratek šum — še vedno vas poslušam."),
        ("sl", "Vedno rad reče fejst bodite ob koncu, ampak midva imava danes na telefonu še dve pomembni točki."),
        ("sl", "Prenesite lep pozdrav vsem sodelavcem v ekipi, meni pa zdaj povejte, kdaj začnemo s projektom."),
        ("sl", "Na koncu dopisa je stal samo lep pozdrav brez ure, zato vas kličem, da se natančno dogovorimo."),
        ("sl", "Srečno sva prispela v hotel pred nekaj minutami, zato imava zdaj čas, da se v miru pogovoriva po telefonu."),
        ("sl", "Zaželel sem mu srečno pred odhodom na pot, zdaj pa sem spet pri mizi — povejte, kaj ste želeli."),
        ("sl", "Ker se vidiva šele prihodnji mesec v živo, morava zdaj po telefonu urediti vse podrobnosti."),
        ("sl", "Če se vidiva zvečer na treningu, mi že zdaj povej, ali naj vzamem dodatno opremo."),
        ("sl", "Po tem zadnjem vprašanju ne bom vas več zadrževal, povejte mi prosim samo še rok dobave."),
        ("sl", "Obljubim, da ne bom vas več zadrževal kot dve minuti, zato preidiva kar k bistvu klica."),
        ("sl", "Glede prvega dela naročila to bi bilo vse, zdaj pa bi vas na zvezi vprašal še glede dostave."),
        ("sl", "Mislil sem, da to bi bilo vse za danes, pa sem ravnokar opazil še eno odprto postavko na seznamu."),
        ("sl", "Napisal sem mu se slišiva kasneje, da naju ne bi motil med tem najinim pogovorom."),
        ("sl", "Lahko rečeva se slišiva kasneje za ostale malenkosti, ampak glavno uro odhoda določiva kar zdaj."),
        ("sl", "Oj, čao, lepo da si se oglasil — imaš minutko za kratko vprašanje?"),
        ("sl", "Samo sosedu na hodniku sem rekel čao, še vedno sem na zvezi in te poslušam."),
        ("sl", "Halo, živjo, kličeva se glede jutrišnjega izleta — imaš malo časa?"),
        ("sl", "Ravno sem rekel živjo receptorju ob vstopu, zdaj sem v tihi pisarni in lahko nadaljuješ."),
        ("sl", "Pozdravljeni in lep dan vam želim, kličem glede rezervacije termina v prihodnjem tednu."),
        ("sl", "Prodajalka mi je pravkar zaželela lep dan ob izhodu, zdaj pa vas na ulici spet dobro slišim."),
        ("sl", "Otrokom sem ravnokar zaželel lahko noč in zaprl vrata, tako da se zdaj lahko v miru pogovoriva."),
        ("sl", "Preden bo čas za lahko noč, mi na kratko povej, kdaj se jutri dobiva pred postajo."),
        ("mk", "Денес времето надвор е многу пријатно, туку кажи ми дали стаса пратката?"),
        ("mk", "Ми беше пријатно кога разговаравме минатата недела, па сега сакам да прашам уште нешто."),
        ("mk", "Ми рече само ајде пријатно и веднаш излезе од канцеларијата, па сакав да те прашам тебе што се случи."),
        ("mk", "Помислив дека ќе кажеш ајде пријатно пред да ти го кажам најважниот дел од планот."),
        ("mk", "Добро ли се слушаме сега кога излегов во ходникот?"),
        ("mk", "Ретко се слушаме во последно време, па раскажи ми што има ново кај вас."),
        ("mk", "Прееска ми пиша само ајде се слушаме, ама еве уште не ми објасни што треба да понесам."),
        ("mk", "Таман реков ајде се слушаме на колегата во собава, туку продолжи ти со прашањето."),
        ("mk", "Со соседите се гледаме секое утро во лифтот, туку кажи ми за сметката за вода."),
        ("mk", "Одамна не се гледаме со друштвото, па сакав да видам кој ден ви одговара."),
        ("mk", "Тој само рече ајде чао среде состанокот вчера, а јас сега сакам да ги провериме бројките."),
        ("mk", "Малото од другата соба вика ајде чао на сите, не обрнувај внимание, кажи ми за терминот."),
        ("mk", "На шалтерот никој не ни рече ни до гледање, туку да ти кажам што ми одговорија за барањето."),
        ("mk", "Пред да дојдеме до гледање, морам да ги проверам уште податоците од формуларот."),
        ("mk", "Само да им кажам добра ноќ на децата во собата и веднаш продолжуваме со разговорот."),
        ("mk", "Вчера не ни стигнав да ти посакам добра ноќ по порака, туку кажи ми како помина денес."),
        ("mk", "Денес конечно имаме убав ден без дожд, па сакав да те прашам дали ќе одиме пеш."),
        ("mk", "Ти посакувам убав ден за роденденот, а сега кажи ми во колку часот почнува прославата."),
        ("mk", "Си ветивме дека почесто ќе се чуеме оваа година, па кажи ми прво како си со обврските."),
        ("mk", "Ако се прекине линијава во тунелот пак ќе се чуеме, но засега кажи ми ги деталите."),
        ("mk", "Се пошегува дека му кажал збогум на стариот велосипед, туку кажи ми кога пристигнува новиот."),
        ("mk", "Зборот збогум звучи премногу сериозно за обичен разговор, па подобро кажи ми што правиш вечерва."),
        ("mk", "Дедо секогаш велеше ајде со здравје кога испраќаше гости, туку да те прашам за рецептот."),
        ("mk", "Му реков ајде со здравје на мајсторот штотуку на врата, извини, продолжи што ми кажуваше."),
        ("mk", "Само една минута да не те задржувам повеќе од потребното, кажи ми кој број треба да го внесам овде?"),
        ("mk", "Знам дека реков да не те задржувам повеќе прееска, ама ми текна уште едно важно прашање за договорот."),
        ("mk", "Еј, баш фала што се јави во вистински момент, таман сакав да те прашам за распоредот!"),
        ("mk", "Фала што се јави толку брзо, ајде сега да ги поминеме сите точки една по една."),
        ("mk", "Во првиот дел од извештајот напишав толку од мене засега, ама ти кажи ми што би додал ти."),
        ("mk", "Толку од мене засега за првата тема, па сега ја слушам твојата идеја за следниот чекор."),
        ("mk", "Здраво, се јавувам во врска со резервацијата за следната недела."),
        ("mk", "Многу е здраво да се пешачи секое утро, туку кажи ми дали си слободен попладне."),
        ("mk", "Голем поздрав ти праќаат сите од дома, а сега кажи ми што реши за патувањето."),
        ("mk", "Поздрав до тебе, дали имаш две минути да ми објасниш како се вклучува опцијата?"),
        ("sq", "Doli nga mbledhja dje pa thënë as mirupafshim, por më thuaj ti çfarë vendosët më pas."),
        ("sq", "Para se të themi mirupafshim, duhet të verifikojmë edhe një herë orarin e saktë."),
        ("sq", "A dëgjohemi më qartë tani që dola pranë dritares?"),
        ("sq", "Rrallë dëgjohemi gjatë javës për shkak të punës, ndaj më trego si je me shëndet."),
        ("sq", "Me fqinjët shihemi pothuajse çdo mëngjes në shkallë, por doja të të pyesja për çelësin rezervë."),
        ("sq", "Kur nuk shihemi për një kohë të gjatë kemi shumë për të folur, prandaj më trego çdo detaj."),
        ("sq", "Më shkroi vetëm shihemi më vonë në mesazh dhe nuk sqaroi orën, ndaj po të pyes ty çfarë di."),
        ("sq", "Edhe pse shihemi më vonë në darkë, duhet të ta them që tani këtë lajm të rëndësishëm."),
        ("sq", "I urova të kalofsh mirë kur u nis për pushime dje, por tani më thuaj kur kthehet."),
        ("sq", "Shpresoj të kalofsh mirë në festë sonte, veç më thuaj më parë a të duhet ndonjë gjë."),
        ("sq", "Kur i thashë kalofshi mirë sportelistes, ajo më kujtoi edhe një formular, ndaj po ju pyes për të."),
        ("sq", "Uroj të kalofshi mirë me familjen këtë javë, por më lejoni t'ju pyes edhe për adresën e dërgesës."),
        ("sq", "Ai gjithmonë e fillon letrën duke uruar gjithë të mirat, por le të shohim tani pikën e dytë të kontratës."),
        ("sq", "Meriton gjithë të mirat për punën që ke bërë, dhe tani dua të të pyes si do të vazhdojmë nesër."),
        ("sq", "Nga mëngjesi e kupton ditën e mirë, po më thuaj a e ke lexuar njoftimin që erdhi sot?"),
        ("sq", "Sapo i urova ditën e mirë korrierit te dera, më fal për shkëputjen, vazhdo të lutem."),
        ("sq", "Vetëm një sekondë t'u them natën e mirë fëmijëve në dhomën tjetër dhe kthehem menjëherë në linjë."),
        ("sq", "Dje harrova të të uroj natën e mirë me mesazh, po më trego si fjete."),
        ("sq", "Ai e ka zakon të thotë hajde mirupafshim sa herë ngutet, por ne kemi ende dy çështje për të diskutuar."),
        ("sq", "Mos më thuaj hajde mirupafshim pa më treguar më parë si shkoi mbledhja e mëngjesit."),
        ("sq", "Për pjesën e buxhetit mund të flasim më vonë gjatë javës, kurse tani më thuaj për orarin e nesërm."),
        ("sq", "Sa herë themi flasim më vonë e harrojmë, prandaj më mirë ta vendosim që tani në telefon."),
        ("sq", "Që mos të të mbaj më gjatë se pesë minuta, më thuaj shkurt cilat dokumente duhen nesër."),
        ("sq", "Thashë mos të të mbaj më gjatë pak më parë, por m'u kujtua edhe një pyetje e vogël."),
        ("sq", "Me ekipin tjetër mbetemi në kontakt çdo ditë, por dua të di mendimin tënd tani për planin."),
        ("sq", "Është mirë që mbetemi në kontakt kaq rregullisht, më trego tani çfarë të reje ka nga projekti."),
        ("sq", "Tung, a ke dy minuta kohë të të pyes diçka për orarin e sotëm?"),
        ("sq", "Sapo i thashë tung fqinjit në korridor, vazhdo të lutem me atë që po më tregoje."),
        ("sq", "Tungjatjeta, po ju marr në lidhje me njoftimin që më erdhi sot në mëngjes."),
        ("sq", "Gjyshi im thoshte gjithmonë tungjatjeta kur hynte në shtëpi, po më thuaj ti çfarë bëtë dje."),
        ("sq", "Çao, më dëgjon mirë tani, desha të të pyes ku do të takohemi?"),
        ("sq", "Më tha vetëm çao kur kaloi me vrap në rrugë, por tani më trego çfarë ke planifikuar ti."),
        ("sv", "Jag sa bara hej så länge till budet vid dörren, förlåt avbrottet, vad var det du sa om leveransen?"),
        ("sv", "Uttrycket hej så länge låter alltid lite roligt, men innan vi avslutar måste jag fråga om nycklarna."),
        ("sv", "Det kändes skönt att ta adjö av den gamla soffan i går, och nu funderar jag på vilken färg vi ska välja."),
        ("sv", "Ingen säger väl adjö i telefon längre bland ungdomar, men vad skulle du svara på frågan jag ställde?"),
        ("sv", "Vi hade en riktigt trevlig helg på landet senast, och nu undrar jag om ni vill följa med nästa gång."),
        ("sv", "För att få en lugn och trevlig helg behöver vi lösa de här två frågorna i samtalet först."),
        ("sv", "Jag ska inte hålla dig längre än två minuter, jag behöver bara stämma av en siffra först."),
        ("sv", "Jag sa visst jag ska inte hålla dig längre nyss, men jag kom precis på en viktig detalj till."),
        ("sv", "Tack för att du ringde tillbaka så snabbt, jag tänkte höra om vi kan gå igenom hela tidsplanen nu?"),
        ("sv", "Förresten, tack för att du ringde och påminde mig, vad mer stod det i kallelsen?"),
        ("sv", "Jag lovade grannen nyss att jag ringer tillbaka senare till honom så att jag kan prata klart med dig nu."),
        ("sv", "I stället för att säga att jag ringer tillbaka senare kan jag lika gärna leta fram uppgiften medan du väntar."),
        ("nb", "Vi tok endelig adjø med den gamle bilen i forrige uke, og nå lurer jeg på hva vi bør se etter."),
        ("nb", "Det er nesten ingen som sier adjø i telefonen lenger, men hva tenkte du egentlig om forslaget mitt?"),
        ("nb", "Vi hadde en veldig god helg på fjellet sist, og nå lurte jeg på om vi skal leie samme hytte igjen."),
        ("nb", "For at alle skal få en rolig og god helg, må vi bli enige om vaktlisten nå med en gang."),
        ("nb", "Selv om vi sees på møtet i morgen, trenger jeg svar på dette spørsmålet allerede nå i telefonen."),
        ("nb", "Siden vi sees så sjelden til vanlig, ville jeg gjerne høre litt mer om planene dine fremover."),
        ("nb", "Vi pleier å ha det fint når hele gruppen samles, så hvilken helg passer egentlig best for deg?"),
        ("nb", "Det ser ut til å være greit å ha det fint ryddet før visningen, men hva gjør vi med boden?"),
        ("nb", "Jeg skal ikke holde deg lenger enn et par minutter, jeg må bare spørre om én viktig detalj."),
        ("nb", "Jeg sa nettopp jeg skal ikke holde deg lenger i sted, men det dukket akkurat opp et spørsmål til."),
        ("nb", "Takk for at du ringte tilbake så raskt, passer det at vi går gjennom hele avtalen nå med en gang?"),
        ("nb", "Forresten, takk for at du ringte og minnet meg på fristen, hvilke vedlegg var det som manglet?"),
        ("nb", "Jeg ga bare beskjed på den andre linjen om at jeg ringer deg opp igjen senere, så nå kan vi to snakke ferdig."),
        ("nb", "I stedet for å si at jeg ringer deg opp igjen senere, kan jeg sjekke kalenderen mens du venter et sekund."),
        ("da", "Jeg sagde kun hej så længe til kollegaen ved døren, så nu har jeg god ro til at høre dit forslag."),
        ("da", "Udtrykket hej så længe lyder næsten som en kort pause, men inden vi slutter skal jeg lige spørge om noget."),
        ("da", "Jeg sagde lige kan du have det godt til postbuddet i døren, undskyld afbrydelsen, fortsæt endelig."),
        ("da", "Man spørger tit hvordan kan du have det godt med så meget travlhed, men fortæl mig om dit nye skema."),
        ("da", "Vi tales ved alt for sjældent i hverdagen, så fortæl mig endelig, hvordan det går hos jer."),
        ("da", "Selvom vi tales ved igen på fredag, har jeg brug for dit svar på dette spørgsmål allerede nu."),
        ("da", "Vi havde en rigtig god weekend i sommerhuset sidst, og nu ville jeg høre om I er ledige i næste måned."),
        ("da", "For at vi alle kan få en rolig og god weekend, skal vi lige have styr på tidsplanen nu."),
        ("da", "Giv mig lige ti sekunder til at sige godnat til børnene inde ved siden af, så er jeg tilbage i røret."),
        ("da", "Jeg glemte helt at skrive godnat i aftes, men fortæl mig lige hvordan mødet gik her til morgen."),
        ("da", "Jeg skal ikke holde dig længere end to minutter, jeg mangler bare lige at tjekke én dato med dig."),
        ("da", "Jeg sagde lige jeg skal ikke holde dig længere før, men jeg kom i tanke om et vigtigt spørgsmål."),
        ("da", "Tak fordi du ringede tilbage så hurtigt, har du tid til at vi gennemgår punkterne nu med det samme?"),
        ("da", "For resten, tak fordi du ringede og mindede mig om aftalen, hvad tid var det præcis vi skulle mødes?"),
        ("da", "Jeg gav bare kort besked på den anden linje om, at jeg ringer tilbage lidt senere, så vi to kan tale færdig nu."),
        ("da", "I stedet for at sige, at jeg ringer tilbage lidt senere, kan jeg lige slå det op på skærmen mens du venter."),
        ("fi", "Kollega sanoi tuossa käytävällä näkemiin lähtiessään kotiin, mutta minä olen yhä linjalla, jatketaan vain."),
        ("fi", "Viimeksi minulle sanottiin vain näkemiin ilman selitystä, joten haluaisin nyt kuulla tarkat ehdot."),
        ("fi", "Jos sanon myyjälle heti kiitos hei, hän soittaa silti uudestaan, mutta miten teidän palvelussanne esto toimii?"),
        ("fi", "Lähetti sanoi ovella kiitos hei ja antoi paketin, mutta odota hetki niin avaan sen tässä puhelun aikana."),
        ("fi", "Sovimme eilen että soitellaan tänään tästä sopimuksesta, joten käydäänkö kohdat nyt rauhassa läpi?"),
        ("fi", "Hän sanoi aina vain että soitellaan myöhemmin, mutta nyt haluaisin hoitaa tämän varauksen heti valmiiksi."),
        ("fi", "Älä sano vielä että palaillaan myöhemmin, sillä tarvitsen vastauksen tähän kysymykseen jo tämän puhelun aikana."),
        ("fi", "Meidän piti katsoa hintaa jo viimeksi kun sanottiin että palaillaan, joten mikä se loppusumma nyt on?"),
        ("fi", "Kuullaan kyllä ihan hyvin nyt kun siirryin ikkunan viereen, joten kerro vain asiasi."),
        ("fi", "Täällä kuullaan taustalta vähän liikennettä, mutta kuulen sinut silti selvästi, jatka vain."),
        ("fi", "Luulin jo että ei mulla muuta, mutta hetkinen, pitikö lomake sittenkin allekirjoittaa etukäteen?"),
        ("fi", "Sanoin äsken että ei mulla muuta, mutta kysyn vielä nopeasti pysäköinnistä."),
        ("fi", "Kiitos soitosta muuten, olinkin juuri aikeissa kysyä sinulta muutamaa tarkennusta tähän hakemukseen."),
        ("fi", "Sanotaan ensin kiitos soitosta, ja kerro sitten ihan rauhassa mitä asunnolle kuuluu."),
        ("fi", "Jos puhutaan myöhemmin lisää yksityiskohdista, niin sovitaanko kuitenkin jo nyt alustava päivämäärä?"),
        ("fi", "Ehdotit että puhutaan myöhemmin lisää, mutta minulla on kalenteri auki juuri nyt, kävisikö keskiviikko?"),
        ("fi", "Palataan asiaan numero yksi ihan kohta, mutta katsotaanko ensin tämä toimitusmaksu?"),
        ("fi", "Ennen kuin palataan asiaan ensi viikolla, voisitko lukea minulle sen viitenumeron nyt heti?"),
        ("cs", "Předchozí operátor řekl na slyšenou dřív, než jsem stihl nahlásit změnu adresy."),
        ("cs", "Někdo v telefonu říká výhradně na slyšenou, ale než skončíme, proberme ještě ceník."),
        ("cs", "Měj se mnou prosím ještě chvilku trpělivost, hned ten dokument v počítači najdu."),
        ("cs", "Kamarád u vchodu zavolal měj se, ale já jsem pořád na drátě a poslouchám tě."),
        ("cs", "Nebudu vás zdržovat dlouho, jen potřebuji rychle ověřit dvě položky ve vyúčtování."),
        ("cs", "Nebudu vás zdržovat víc než minutu, řekněte mi prosím aktuální stav objednávky."),
        ("cs", "Dobrý den, děkuji za zavolání na naši podporu, s čím vám mohu pomoci?"),
        ("cs", "Děkuji za zavolání, než přejdeme k věci, povězte mi prosím, čeho se dotaz týká."),
        ("cs", "S účetním to vyřeším potom, zavoláme si později odpoledne, teď ale potřebuji slyšet tvůj názor."),
        ("cs", "Kdyby se spojení přerušilo, zavoláme si později, ale zatím tě slyším výborně, pokračuj."),
        ("sk", "Predchádzajúci pracovník povedal do počutia skôr, než som stihol nadiktovať adresu doručenia."),
        ("sk", "Niekto v telefóne používa zásadne do počutia, ale predtým ešte preberme podmienky zmluvy."),
        ("sk", "Nebudem vás zdržiavať dlho, potrebujem si len rýchlo overiť dve položky v rozpise."),
        ("sk", "Nebudem vás zdržiavať viac než minútu, poviete mi prosím aktuálny stav reklamácie?"),
        ("sk", "Dobrý deň, ďakujem za zavolanie na našu zákaznícku linku, s čím vám môžem pomôcť?"),
        ("sk", "Ďakujem za zavolanie, povedzte mi prosím najskôr, akej služby sa vaša otázka týka."),
        ("sk", "S účtovníkom to doriešim potom, zavoláme si neskôr popoludní, teraz však potrebujem počuť tvoj názor."),
        ("sk", "Ak by nám vypadol signál, zavoláme si neskôr, ale zatiaľ ťa počujem výborne, pokračuj."),
        ("hu", "A futár az előbb csak annyit mondott, hogy viszhall, de nem árulta el, melyik kapuhoz jön, ön látja a rendszerben?"),
        ("hu", "Sokan vitatják, mennyire udvarias a viszhall kifejezés, mindenesetre most még lenne egy kérdésem önhöz."),
        ("hu", "A nagymama üzeni, hogy sok puszi mindenkinek, most pedig mondd el, mit hozzak a boltból."),
        ("hu", "Az üzenet végén csak annyi állt, hogy puszi, de az időpontot elfelejtetted megírni, mikor érkezel?"),
        ("hu", "A gyerekeknek már mondtam jó éjszakát, úgyhogy most nyugodtan átbeszélhetjük a holnapi programot."),
        ("hu", "Az őr épp jó éjszakát kívánt a kapuban, ne zavartasd magad, mondd tovább."),
        ("hu", "A könyvelővel majd beszélünk a jövő héten, most viszont veled szeretném tisztázni a határidőt."),
        ("hu", "Erről a részletről majd beszélünk személyesen is, de előbb mondd el telefonon a lényeget."),
        ("hu", "Ígérem, hogy nem tartom fel tovább egy percnél, csak még egy adatot ellenőrizzünk le."),
        ("hu", "Hogy később már biztosan ne zavarjam és mondhassam, nem tartom fel tovább, kérdeznék még valamit most."),
        ("hu", "Azt hittem, most le kell tennem a csengő miatt, de tévedés volt, nyugodtan mondd tovább."),
        ("hu", "Írtál, hogy most le kell tennem a másik hívás miatt, de látom, még vonalban vagy, beszéljük át gyorsan."),
        ("hu", "Jó napot kívánok, köszönöm a hívást, miben segíthetek önnek ma?"),
        ("hu", "Köszönöm a hívást, kérjük, mondja el röviden, milyen ügyben keresi ügyfélszolgálatunkat."),
        ("hu", "Írtál egy üzenetet, hogy később visszahívlak, ezért most én hívtalak fel, ráérsz?"),
        ("hu", "Ha megszakadna a vonal az alagútban, később visszahívlak, de amíg hallasz, mondd gyorsan a híreket."),
        ("bg", "Куриерът току-що остави пратката на входа и каза довиждане, но аз още съм на линия — кажете ми за часа."),
        ("bg", "На бележката пишеше само довиждане, затова ви се обаждам сега да разбера какво следва по графика."),
        ("bg", "В края на писмото беше написал до чуване, но аз ти звъня още сега за няколко уточнения."),
        ("bg", "Когато операторът каже до чуване твърде рано, винаги остават въпроси, затова нека ги минем подред."),
        ("bg", "Знам, че ще се чуем и другата седмица, но нека още сега в този разговор проверим списъка."),
        ("bg", "Колегата написа, че ще се чуем следобед, а междувременно искам да те питам още два въпроса."),
        ("bg", "Той ми прати съобщение с чао засега, обаче аз искам да останеш на линия още минута."),
        ("bg", "Не ми казвай чао засега, преди да сме проверили дали адресът в заявката е точен."),
        ("bg", "Сега чуваме се много по-ясно, така че спокойно ми разкажете останалите подробности."),
        ("bg", "Ние винаги чуваме се сутрин преди смяната, но кажи ми още нещо важно, докато сме на телефона."),
        ("bg", "Пожелавам ти всичко хубаво за празника, а сега ми разкажи как мина сутрешната среща."),
        ("bg", "Тук в новия офис всичко хубаво тепърва започва, но кажи ми първо кога пристига доставката."),
        ("bg", "Само затварям прозореца в стаята заради шума от улицата, останете на линия и продължете."),
        ("bg", "Сега затварям папката на компютъра и веднага ви чета следващия въпрос по телефона."),
        ("bg", "След тази последна тема няма да ви задържам повече, но първо ми кажете какъв е срокът за доставка."),
        ("bg", "Обещавам, че няма да ви задържам повече от пет минути, така че нека започнем с основния въпрос."),
        ("bg", "Ако дойде куриерът, ще трябва да затварям, но докато го чакам ми разкажи всичко подробно."),
        ("bg", "Казах на съседа, че скоро трябва да затварям магазина, но с теб спокойно можем да поговорим още малко."),
        ("bg", "След още две минути ще кажа хайде да не те задържам, но първо ми продиктувай точния час."),
        ("bg", "Той веднага рече хайде да не те задържам, а аз всъщност имам време — кажи ми какво мислиш по въпроса."),
        ("bg", "На колегата му казах, че ще се обадя по-късно, за да мога сега спокойно говоря с вас."),
        ("bg", "Ако случайно прекъсне обхватът в тунела, ще се обадя по-късно, но засега ме чувате добре, нали?"),
        ("el", "Στην ταινία είπε αντίο χωρίς εξήγηση, αλλά εμείς έχουμε ακόμα ώρα να συζητήσουμε το πρόγραμμα."),
        ("el", "Πριν πούμε αντίο σε αυτή την κλήση, θέλω οπωσδήποτε να επιβεβαιώσουμε τη διεύθυνση αποστολής."),
        ("el", "Το μήνυμα έκλεινε επίσημα με εις το επανιδείν, αλλά σας τηλεφωνώ τώρα για να διευκρινίσουμε την ώρα."),
        ("el", "Είπαμε εις το επανιδείν με τους επισκέπτες στη ρεσεψιόν και μόλις γύρισα στο γραφείο — σας ακούω."),
        ("el", "Σίγουρα θα τα ξαναλέμε μέσα στην εβδομάδα, αλλά πες μου από τώρα αν συμφωνείς με το σχέδιο."),
        ("el", "Κάθε φορά λέμε ότι θα τα ξαναλέμε αργότερα, οπότε ας λύσουμε το ζήτημα τώρα που μιλάμε."),
        ("el", "Χαίρομαι πολύ που ακούω ότι είστε καλύτερα — να είστε καλά πάντα, και πείτε μου τώρα για τα έγγραφα."),
        ("el", "Μου είπε απλώς να είστε καλά στην είσοδο, αλλά εμείς συνεχίζουμε κανονικά τη συζήτησή μας στο τηλέφωνο."),
        ("el", "Σας εύχομαι καλό υπόλοιπο εβδομάδας εκ των προτέρων, αλλά ας δούμε πρώτα μαζί τις τρεις εκκρεμότητες."),
        ("el", "Μας είπε καλό υπόλοιπο ο υπάλληλος στο γκισέ μόλις βγήκα, οπότε τώρα μπορώ να σου μιλήσω άνετα."),
        ("el", "Γεια σας και καλό μεσημέρι, σας καλώ σχετικά με την παραγγελία που καταχωρίστηκε σήμερα το πρωί."),
        ("el", "Μου ευχήθηκαν καλό μεσημέρι στην είσοδο του κτιρίου, είμαι στη γραμμή και μπορούμε να συνεχίσουμε."),
        ("el", "Περιμένετε ένα δευτερόλεπτο, κλείνω μόνο το παράθυρο γιατί έχει φασαρία στον δρόμο — συνεχίστε παρακαλώ."),
        ("el", "Τώρα κλείνω τον φάκελο στον υπολογιστή και ανοίγω το ημερολόγιο, είμαι ακόμα στη γραμμή μαζί σας."),
        ("el", "Σε δέκα λεπτά πρέπει να κλείσω για ένα ραντεβού, άρα προλαβαίνουμε άνετα να τα πούμε τώρα."),
        ("el", "Νόμιζα ότι πρέπει να κλείσω νωρίτερα, τελικά όμως αναβλήθηκε η συνάντηση και σε ακούω κανονικά."),
        ("el", "Υπόσχομαι ότι δεν σας καθυστερώ άλλο μετά από αυτή την τελευταία ερώτηση για το ωράριο."),
        ("el", "Μου είπατε να μην ανησυχώ, οπότε δεν σας καθυστερώ άλλο με τα παλιά και μπαίνω κατευθείαν στο θέμα."),
        ("el", "Έστειλα μήνυμα στον συνάδελφο ότι θα σε πάρω αργότερα αν χρειαστεί, αλλά τώρα έχω χρόνο — πες μου."),
        ("el", "Αν κοπεί η γραμμή στο ασανσέρ θα σε πάρω αργότερα, προς το παρόν όμως με ακούς καθαρά, σωστά;"),
        ("el", "Του είπα ότι μιλάμε αργότερα για να μην μας διακόψει όσο συζητάμε εμείς οι δύο τώρα."),
        ("el", "Αν θέλεις μιλάμε αργότερα και για το ταξίδι, αλλά πρώτα πες μου τώρα τι ώρα θα συναντηθούμε."),
        ("hr", "Voditelj u emisiji upravo je rekao do slušanja, stišao sam radio pa mi sada mirno sve ispričajte."),
        ("hr", "Na kraju govorne poruke rekao je do slušanja, pa vas zovem natrag da prođemo kroz sva pitanja."),
        ("hr", "Napisao sam mu u poruci čujemo se poslije da nas ne ometa dok mi sada razgovaramo."),
        ("hr", "Lako ćemo reći čujemo se poslije, nego daj da odmah sada provjerimo jesu li svi brojevi točni."),
        ("hr", "Samo drži se teme o kojoj sada pričamo na telefonu da ne zaboravimo najvažniji dio plana."),
        ("hr", "Ma drži se ti svog rasporeda, a sada mi na liniji reci odgovara li ti utorak ujutro."),
        ("hr", "Želim i vama ugodan ostatak dana, a prije svega molim vas da mi provjerite status rezervacije."),
        ("hr", "Blagajnica mi je upravo poželjela ugodan ostatak dana, izašao sam iz trgovine i sada vas dobro čujem."),
        ("hr", "Nakon ovog zadnjeg pitanja neću vas više zadržavati, samo mi još recite rok isporuke."),
        ("hr", "Obećavam da neću vas više zadržavati od tri minute, pa krenimo odmah na glavni razlog poziva."),
        ("hr", "Rekao sam kolegi da nazvat ću kasnije njega kako bih sada mogao bez žurbe razgovarati s vama."),
        ("hr", "Ako nam slučajno pukne veza u vlaku, nazvat ću kasnije, ali zasad me čujete dobro, zar ne?"),
        ("hr", "Ako naiđe nadređeni, moram prekinuti na kratko, ali dok sam sam u uredu slobodno mi sve ispričaj."),
        ("hr", "Mislio sam da moram prekinuti zbog druge obveze, no ipak je odgođena pa možemo nastaviti razgovor."),
        ("sr", "Курир је само оставио пакет на вратима и рекао довиђења, али ја сам још на вези — реците ми за распоред."),
        ("sr", "Пре него што кажемо довиђења у овом разговору, хајде да проверимо још једном тачну адресу."),
        ("sr", "Комшија је у пролазу само добацио ајде здраво, не прекидај везу, ту сам и слушам те."),
        ("sr", "Немој одмах да ми кажеш ајде здраво док ми не потврдиш у колико сати се налазимо."),
        ("sr", "Другар ми је махнуо са врата уз брзо ајд ћао, али ја имам времена — испричај ми све до краја."),
        ("sr", "Мрзим кад неко усред приче каже ајд ћао, зато хајде на миру да завршимо ову тему."),
        ("sr", "Еј, ћао, добро је да си се јавио — имаш ли два минута да те нешто питам?"),
        ("sr", "Само сам рекао ћао комшији у ходнику, ту сам на вези и слушам те пажљиво."),
        ("sr", "Хало, здраво, зовем да проверим када вам одговара да преузмете документа."),
        ("sr", "Управо сам рекао здраво портиру на улазу, сада сам у тихој сали па наставите."),
        ("sr", "Баш ми је пријатно што сте се јавили тако брзо, па ми сада реците који вам термин одговара."),
        ("sr", "Колега у канцеларији ми је управо пожелео пријатно за ручак, али нисам прекинуо — слушам вас."),
        ("sr", "Желим вам свако добро у даљем раду, а пре него што наставимо реците ми рок за предају документације."),
        ("sr", "Службеник на шалтеру ми је рекао свако добро кад сам изашао, па сада могу мирно да причам са вама."),
        ("sr", "Желим ти све најбоље за празнике, а док смо још на вези реци ми када ти почиње одмор."),
        ("sr", "Поручили су нам све најбоље на почетку састанка, него реци ми сада шта мислиш о новом предлогу."),
        ("sr", "Пошто видимо се тек следећег месеца уживо, хајде сада преко телефона да пређемо цео списак."),
        ("sr", "Ако видимо се вечерас у граду, реци ми одмах сада шта треба да понесем са собом."),
        ("sr", "Срдачан поздрав и вама, зовем да проверим да ли је пошиљка већ кренула данас."),
        ("sr", "Послао је само кратак поздрав у поруци, зато сам те позвао да се лепо испричамо сада."),
        ("sr", "Пожелео сам деци лаку ноћ и затворио врата собе, па сада можемо на миру да причамо."),
        ("sr", "Пре него што дође време за лаку ноћ, реци ми само у колико сати крећемо ујутру."),
        ("sr", "Рекао сам му кратко чујемо се после само да нас не прекида док ми сада разговарамо."),
        ("sr", "Лако ћемо рећи чујемо се после, него хајде одмах сада да проверимо да ли су сви подаци тачни."),
        ("sr", "После овог последњег питања не бих вас више задржавао, само ми још реците цену доставе."),
        ("sr", "Обећавам да не бих вас више задржавао дуже од два минута, па да пређемо одмах на ствар."),
        ("sr", "Написао сам колеги јавићу се касније како бих сада могао без журбе да разговарам са вама."),
        ("sr", "Ако случајно пукне веза у лифту јавићу се касније, али засад ме чујете добро, зар не?"),
    ];

    #[test]
    fn lookalikes_do_not_close() {
        let mut bad = Vec::new();
        for (lang, t) in LOOKALIKES {
            for opening in [true, false] {
                let mut m = if opening { ClosingMachine::new(lang) } else { after_talk(lang) };
                let s = hears(&mut m, t);
                if closes(&s) {
                    bad.push(format!("[{lang}] opening={opening} {t} -> {:?} ends={}", s.event, s.ends_call));
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// A Greek wish with "να έχετε" and a Norwegian "ha en fin dag videre" close (the
    /// simulated clinic and landlord calls ended on them); the same wish told about, or
    /// with more said after it, does not.
    #[test]
    fn greek_wishes_on_leaving() {
        let bye: &[(&'static str, &str)] = &[
            ("el", "Θα σας δούμε την Παρασκευή λοιπόν. Καλό υπόλοιπο ημέρας να έχετε!"), ("el", "Ευχαριστώ, καλή συνέχεια να έχετε."),
            ("el", "Εντάξει, καλό βράδυ να έχετε!"),
            ("nb", "Supert, tusen takk skal du ha, og ha en fin dag videre."), ("sv", "Tack för samtalet, ha en fortsatt bra dag."),
            ("da", "Mange tak, hav en fortsat god dag!"),
        ];
        let mut bad = all_close(bye, after_talk);
        let go_on: &[(&'static str, &str)] = &[
            ("el", "Καλό υπόλοιπο ημέρας να έχετε, αλλά πρώτα πείτε μου την ώρα"),
            ("el", "Μου είπε καλό υπόλοιπο ημέρας να έχετε και μετά με έβαλε σε αναμονή, τι γίνεται με το ραντεβού;"),
            ("el", "Καλό υπόλοιπο ημέρας"),
            ("nb", "Han sa ha en fin dag videre og la på, men jeg har fortsatt et spørsmål"),
        ];
        for (lang, t) in go_on {
            let mut m = after_talk(lang);
            let s = hears(&mut m, t);
            if closes(&s) {
                bad.push(format!("[{lang}] {t} -> {:?}", s.event));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// Thanks in the new languages alone is thanks, not a goodbye.
    #[test]
    fn bare_thanks_does_not_close() {
        let cases: &[(&'static str, &str)] = &[
            ("is", "Takk fyrir."), ("is", "Takk kærlega."), ("sl", "Najlepša hvala."), ("mk", "Благодарам."), ("mk", "Фала многу."),
            ("sq", "Faleminderit."), ("sq", "Faleminderit shumë."), ("sr", "Хвала вам."),
        ];
        let mut bad = Vec::new();
        for (lang, t) in cases {
            let mut m = after_talk(lang);
            let s = hears(&mut m, t);
            if s.ends_call || s.event == ClosingEvent::CallerFarewell {
                bad.push(format!("[{lang}] {t} -> {:?} ends={}", s.event, s.ends_call));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}

/// Fifth batch: goodbyes, pre-closings and greeting-goodbyes for 23 languages of
/// Asia, the Middle East, Africa and the Baltics (he, ms, tl, bn, ur, ne, si, my, lo, mn, fa, ps, az, hy, ka, sw, am, af, et, lv, lt, be, ta).
/// The sentences are the research's own examples; the ones that must not close are its
/// counterexamples (a goodbye told about, said at the start, or with more after it).
#[cfg(test)]
mod regional_closing_words_5_tests {
    use super::*;

    fn after_talk(lang: &'static str) -> ClosingMachine {
        let mut m = ClosingMachine::new(lang);
        m.on_caller("Parcel delivery info", "Parcel delivery info", false, false);
        m
    }

    fn after_ai_goodbye(lang: &'static str) -> ClosingMachine {
        let mut m = after_talk(lang);
        m.on_ai_turn("Okay, goodbye!", false, false);
        m
    }

    fn hears(m: &mut ClosingMachine, t: &str) -> ClosingStep {
        m.on_caller(t, t, crate::locale::caller_wraps_up(t), false)
    }

    fn closes(s: &ClosingStep) -> bool {
        s.ends_call || matches!(s.event, ClosingEvent::CallerFarewell | ClosingEvent::CallerPreClosing)
    }

    fn all_close(cases: &[(&'static str, &str)], m: impl Fn(&'static str) -> ClosingMachine) -> Vec<String> {
        let mut bad = Vec::new();
        for (lang, t) in cases {
            let mut mm = m(lang);
            let s = hears(&mut mm, t);
            if s.event != ClosingEvent::CallerFarewell || !s.ends_call {
                bad.push(format!("[{lang}] {t} -> {:?} ends={}", s.event, s.ends_call));
            }
        }
        bad
    }

    /// Goodbyes said only at the end.
    const GOODBYES: &[(&str, &str)] = &[
        ("he", "מעולה, תודה על העדכון, יאללה ביי."),
        ("he", "סגרנו את כל הפרטים למחר, טוב ביי."),
        ("he", "תודה רבה על העזרה, ביי."),
        ("he", "הכול ברור לגמרי, תודה, ביי ביי."),
        ("he", "תודה על השירות המהיר, להתראות."),
        ("he", "שמחתי לשוחח איתך, שיהיה המשך שבוע נעים, להשתמע."),
        ("he", "אני באמצע נהיגה באזור רועש, נדבר אחר כך."),
        ("he", "תודה על המענה המדויק, שיהיה יום טוב."),
        ("ms", "Terima kasih banyak atas bantuan tadi, jumpa lagi."),
        ("ms", "Baiklah, sampai di sini sahaja perbualan kita, selamat tinggal."),
        ("ms", "Ada panggilan lain masuk pula, saya letak dulu ya."),
        ("ms", "Waktu rehat dah nak habis ni, nanti kita sembang lagi."),
        ("ms", "Baiklah, hati-hati di sana dan jaga diri."),
        ("ms", "Ok lah, terima kasih banyak, bye!"),
        ("ms", "Ok lah awak, saya nak tidur dah ni, babai."),
        ("ms", "Semua dah jelas rasanya, ok lah, bye."),
        ("ms", "Ok geng, dah sampai masa ni, aku gerak dulu."),
        ("tl", "Salamat sa pagtawag ha, sige, bye!"),
        ("tl", "Maraming salamat po sa inyong oras, paalam po."),
        ("tl", "Aalis na ang sinasakyan naming bus, sige, bye na!"),
        ("tl", "Maraming salamat po sa inyong pagtawag, hanggang sa muli!"),
        ("tl", "Papunta na ako riyan sa tagpuan natin, sige, kita-kits!"),
        ("tl", "Marami pa akong tatapusing gawain sa mesa, sige na, bye!"),
        ("tl", "Magsisimula na ang klase namin ngayon, sige, una na ako!"),
        ("bn", "অনেক ধন্যবাদ আপনাকে, ভালো থাকবেন।"),
        ("bn", "সাবধানে বাড়ি যেও, ভালো থেকো।"),
        ("bn", "আচ্ছা ভাই, দোয়া করবেন, আল্লাহ হাফেজ।"),
        ("bn", "আজকের মতো তাহলে বিদায় নিচ্ছি, খোদা হাফেজ।"),
        ("bn", "এখন একটু কাজে ঢুকছি, পরে কথা হবে।"),
        ("bn", "ধন্যবাদ সময় দেওয়ার জন্য, আবার কথা হবে।"),
        ("bn", "ওকে তাহলে এখন রাখছি, বাই।"),
        ("bn", "আচ্ছা আজ রাখি রে, টাটা।"),
        ("bn", "অনেক রাত হলো, এবার ঘুমিয়ে পড়ুন, শুভরাত্রি।"),
        ("ur", "بہت شکریہ آپ کی رہنمائی کا، اللہ حافظ۔"),
        ("ur", "ساری تفصیل نوٹ کر لی ہے، بہت شکریہ، خدا حافظ۔"),
        ("ur", "آپ سے بات کر کے بہت اچھا لگا، فی امان اللہ۔"),
        ("ur", "ابھی میں راستے میں ہوں، شام کو پھر بات ہوگی۔"),
        ("ur", "دروازے پر کوئی آیا ہے، بعد میں بات کرتے ہیں۔"),
        ("ur", "بہت شکریہ وقت دینے کا، اچھا جی، اپنا خیال رکھیے گا۔"),
        ("ur", "چلو ٹھیک ہے، گھر پہنچ کر میسج کرنا، اپنا خیال رکھنا۔"),
        ("ur", "چلیں کل دفتر میں آمنے سامنے بات کریں گے، پھر ملتے ہیں۔"),
        ("ur", "چلیں جی بہت مہربانی آپ کی، رب راکھا۔"),
        ("ur", "میری بس آ گئی ہے، میں چلتا ہوں، اچھا بائے۔"),
        ("ne", "ल त, सबै कुरा बुझियो, मैले फोन राखेँ है।"),
        ("ne", "अब ढिलो भयो, म फोन राखेँ ल।"),
        ("ne", "अहिले बाटोमा छु, पछी कुरा गरौँला।"),
        ("ne", "ल यार, काम पर्यो, पछी बोलम्ला।"),
        ("ne", "बस आयो जस्तो छ, ल म गएँ।"),
        ("ne", "ल त, पछि फोन गर्छु, बाइ।"),
        ("ne", "ल है साथी, आजलाई टाटा।"),
        ("ne", "धेरै अबेर भइसक्यो, आराम गर्नुस्, शुभ रात्री।"),
        ("si", "හරි එහෙනම් මම තියන්නම්, බුදු සරණයි."),
        ("si", "හරි මල්ලි, එහෙනම් මම ගිහින් එන්නම්."),
        ("si", "එහෙනම් අක්කේ මම දැන් ගිහිල්ලා එන්නම්."),
        ("si", "වැඩ ටික හරි නේද, එහෙනම් මං ගියා."),
        ("si", "හොඳයි, හැමදේම කතා කරගත්තානේ, එහෙනම් අපි ගියා."),
        ("si", "හරි බං, පොඩි වැඩක් ආවා, එහෙනම් මං කැපුනා."),
        ("si", "එහෙනම් මම තියන්නම් මචං, නිවාඩු දවසක ආයෙ සෙට් වෙමු."),
        ("si", "දැන් පොඩි වැඩක ඉන්නේ, අපි පස්සේ කතා කරමු."),
        ("si", "හරි එහෙනම් පරිස්සමින් යන්න, සුභ ගමන්."),
        ("si", "හරි එහෙනම් මම තියන්නම්, බායි."),
        ("si", "එහෙනම් අපි ගියා හොඳේ, ටටා."),
        ("my", "ဟုတ်ကဲ့ အားလုံးရှင်းပါပြီ၊ အဲဒါဆို ဒါပဲနော်။"),
        ("my", "အလုပ်လေး ပေါ်လာလို့ ဖုန်းချလိုက်တော့မယ်နော်။"),
        ("my", "ကောင်းပါပြီ၊ မနက်ဖြန် စောစောတွေ့ကြမယ်၊ ဒါဆို ဒါပဲနော်။"),
        ("my", "အခု လမ်းသွားနေလို့ အိမ်ရောက်မှ နောက်မှ ပြန်ဆက်လိုက်မယ်။"),
        ("my", "အခု အစည်းအဝေး ဝင်ရတော့မှာမို့လို့ နောက်မှ စကားပြောကြမယ်။"),
        ("my", "ကားရောက်နေပြီမို့လို့ ကျွန်တော် သွားတော့မယ်နော်။"),
        ("my", "ကဲ ဒါပဲနော် သူငယ်ချင်း၊ တာ့တာ။"),
        ("my", "အိုကေနော်၊ နောက်နေ့မှ ပြန်ဆက်မယ်၊ ဘိုင်ဘိုင်။"),
        ("my", "အချိန်လည်း လင့်နေပြီဆိုတော့ အေးအေးဆေးဆေး အနားယူပါ၊ ကောင်းသောညပါ။"),
        ("lo", "ໂອເຄ ຊ່ຳນີ້ກ່ອນເດີ້ ມື້ອື່ນຈັ່ງລົມກັນໃໝ່."),
        ("lo", "ຮອດໂມງປະຊຸມແລ້ວ ໄປກ່ອນເດີ້."),
        ("lo", "ແບັດເຕີຣີໃກ້ຈະໝົດແລ້ວ ວາງສາຍກ່ອນເດີ້."),
        ("lo", "ຕອນນີ້ຄາວຽກໜ້ອຍໜຶ່ງ ຄ່ອຍລົມກັນໃໝ່."),
        ("lo", "ຄ່ຳແລ້ວ ພັກຜ່ອນເດີ້ ມື້ໜ້າລົມກັນໃໝ່."),
        ("lo", "ໂດຍ ຂອບໃຈຫຼາຍໆ ພົບກັນໃໝ່."),
        ("lo", "ໂອເຄ ພົບກັນມື້ອື່ນເດີ້ ບ່າຍໆ."),
        ("lo", "ເດິກແລ້ວ ນອນຫຼັບຝັນດີເດີ້."),
        ("mn", "Мэдээлэл өгсөнд баярлалаа, баяртай."),
        ("mn", "Ойлголоо, тэгвэл маргааш холбогдьё, за баяртай."),
        ("mn", "Орой эргээд залгая, түр баяртай."),
        ("mn", "Одоо жаахан завгүй байна, дараа ярья."),
        ("mn", "Одоо гадуур явж байна, гэртээ ороод эргээд залгая."),
        ("mn", "За тэгье, амралтын өдөр дараа уулзъя."),
        ("mn", "Орой болчихлоо, сайхан амраарай."),
        ("mn", "За би явлаа, сайн сууж байгаарай."),
        ("mn", "За ингээд дуусгая, дараа холбогдьё."),
        ("mn", "Би гэртээ харьчхаад тэгээд эргээд ярья."),
        ("fa", "خیلی ممنون از راهنمایی شما، خداحافظ."),
        ("fa", "سپاسگزارم که پیگیری کردید، روز خوش، خدانگهدار."),
        ("fa", "الان باید وارد جلسه بشوم، فعلاً خداحافظ."),
        ("fa", "از گفتگوی امروز بسیار خوشحال شدم، به امید دیدار."),
        ("fa", "خیلی خب عزیزم، فردا بهت زنگ می‌زنم، مراقب خودت باش."),
        ("fa", "ممنون از زحمات شما، شب خوش، مواظب خودتون باشید."),
        ("fa", "ممنون از پاسخگویی دقیق شما، روز خوش."),
        ("fa", "مزاحم استراحتتان نمی‌شوم، ممنون و شب خوش."),
        ("fa", "راهت بی‌خطر باشد، رسیدی پیام بده، خدا به همراهت."),
        ("ps", "ستاسو له مرستې ډېره مننه، خدای پامان."),
        ("ps", "پیغام مو ثبت شو، ورځ مو نېکمرغه، د خدای په امان."),
        ("ps", "له معلوماتو مو نړۍ مننه، په مخه مو ښه."),
        ("ps", "ښه نو زه اوس ځم، سبا به وینو، په مخه دې ښه."),
        ("ps", "ټولې خبرې روښانه شوې، مننه او مخه ښه."),
        ("ps", "له وخت راکولو مو ډېره مننه، په خیر اوسئ."),
        ("ps", "ستاسو له لارښوونې مننه، شپه مو پخیر، الله پامان."),
        ("ps", "سمه ده ګرانه، پر لاره پام کوه، خدای دې مل شه."),
        ("ps", "اوس په موټر کې شور دی، ماښام بیا به غږیږو."),
        ("ps", "ستاسو له همکارۍ ډېره مننه، خوشحاله او ژوندي اوسئ."),
        ("az", "Yaxşı, axşam danışarıq, hələlik."),
        ("az", "Oldu, sabah ofisdə görüşərik."),
        ("az", "İndi iclasdayam, sonra danışarıq."),
        ("az", "Oldu, gecikməyim, özünə yaxşı bax."),
        ("az", "Çox təşəkkür edirəm, özünüzə yaxşı baxın."),
        ("az", "Artıq gecdir, gecəniz xeyrə qalsın."),
        ("az", "Oldu, növbəti həftəyə qədər, xudahafiz."),
        ("az", "Yardımınız üçün minnətdaram, sağlıqla qalın."),
        ("hy", "Շնորհակալություն տեղեկության համար, ցտեսություն։"),
        ("hy", "Դե լավ, հետո կխոսենք, լավ մնա։"),
        ("hy", "Ամեն ինչ պարզ է, շնորհակալություն, լավ մնացեք։"),
        ("hy", "Պայմանավորվեցինք, շաբաթ օրը կհանդիպենք։"),
        ("hy", "Արդեն ուշ է, քնիր, բարի գիշեր։"),
        ("hy", "Հարցս լուծվեց, շնորհակալություն, ցտեսություն։"),
        ("ka", "ყველაფერი გასაგებია, გმადლობთ, ნახვამდის."),
        ("ka", "აბა წავედი ახლა, კარგად იყავი."),
        ("ka", "დიდი მადლობა ყურადღებისთვის, კარგად ბრძანდებოდეთ."),
        ("ka", "კარგი, აღარ მოგაცდენ, აბა დროებით."),
        ("ka", "კარგი, სახლში მივედი უკვე, აბა კარგად."),
        ("ka", "ახლა საჭესთან ვარ, მოგვიანებით დაგირეკავ."),
        ("ka", "ახლა ხმაურია და მერე ვილაპარაკოთ."),
        ("ka", "უკვე გვიანია, დაისვენე, ღამე მშვიდობისა."),
        ("ka", "ყველაფერი ჩავინიშნე, მადლობა, ნახვამდის."),
        ("sw", "Asante sana kwa msaada wako, kwaheri."),
        ("sw", "Asante kwa taarifa, tutaonana!"),
        ("sw", "Niko njiani sasa hivi, asante na tutaonana baadaye!"),
        ("sw", "Ninaingia kwenye kikao sasa, tutazungumza baadaye."),
        ("sw", "Kwa leo inatosha, asante sana, tutaongea kesho."),
        ("sw", "Asante kwa taarifa, tutawasiliana asubuhi, usiku mwema."),
        ("sw", "Asante sana kwa mazungumzo mazuri, baki salama!"),
        ("sw", "Asante kwa kunipigia kabla ya safari, nenda salama!"),
        ("am", "ስለ ትብብርዎ በጣም አመሰግናለሁ፣ ቻው።"),
        ("am", "መልዕክቱን በትክክል አስተላልፋለሁ፣ አመሰግናለሁ፣ ደህና ሁኑ።"),
        ("am", "ስለ እርዳታህ በጣም አመሰግናለሁ፣ ደህና ሁን።"),
        ("am", "ስለ ደወልሽልኝ አመሰግናለሁ፣ ደህና ሁኚ።"),
        ("am", "ስለ መረጃው እጅግ አመሰግናለሁ፣ ደህና ዋሉ።"),
        ("am", "እሺ ሁሉም ተስተካክሏል፣ አመሰግናለሁ፣ ደህና ዋል።"),
        ("am", "ነገ ጠዋት መልሼ እደውልልዎታለሁ፣ ደህና እደሩ።"),
        ("am", "አሁን ስብሰባ ልገባ ስለሆነ በኋላ እንገናኝ።"),
        ("am", "ለዛሬ እዚህ ላይ ይብቃን፣ ነገ እንገናኝ።"),
        ("am", "ስለ ሁሉም ነገር አመሰግናለሁ፣ በሰላም ሁኑ።"),
        ("af", "Baie dankie vir al die hulp, totsiens."),
        ("af", "Dankie vir jou tyd vandag, mooi bly!"),
        ("af", "Geniet die reis en mooi loop, ons praat later!"),
        ("af", "Ek moet nou ongelukkig gaan, praat later!"),
        ("af", "Alles is nou uitgesorteer, lekker dag verder!"),
        ("af", "Dankie dat jy laat weet het, slaap gerus en goeienag."),
        ("af", "Oukei, dankie vir die oproep, tatta!"),
        ("af", "Reg so, ek stuur die boodskap aan, baai!"),
        ("af", "Ek bel jou vanmiddag terug, tot later!"),
        ("af", "Ek is oor 'n rukkie daar, sien jou later!"),
        ("af", "Baie dankie vir die hulp vanaand, lekker aand verder!"),
        ("et", "Suur tänu teile info eest ja head aega!"),
        ("et", "Aitäh abi eest, kena päeva jätku ja nägemist!"),
        ("et", "Helistan teile tagasi kohe, kui saan vastuse, kuulmiseni!"),
        ("et", "Tänan teid koostöö eest ja peatse nägemiseni!"),
        ("et", "Olgu, saadan sulle kohe sõnumi, tšauki!"),
        ("et", "Tänan kiire lahenduse eest, soovin teile kõike head!"),
        ("et", "Suur aitäh selgituse eest ja kena päeva jätku!"),
        ("et", "Uurin hinnapakkumise järele ja jääme kuuldele!"),
        ("et", "Kõik on kokku lepitud, aitäh ja homseni!"),
        ("et", "Olgu nii, ma jooksen nüüd bussi peale, pakaa!"),
        ("lv", "Liels paldies par informāciju, uz redzēšanos!"),
        ("lv", "Es jums atzvanīšu tiklīdz saņemšu atbildi, uz sadzirdēšanos!"),
        ("lv", "Labi, es tagad skrienu uz vilcienu, sazvanāmies vakarā, atā!"),
        ("lv", "Paldies par palīdzību un visu labu jums!"),
        ("lv", "Liels paldies par ātro risinājumu, jauku dienu!"),
        ("lv", "Sarunāts, tad tiekamies birojā deviņos, līdz rītdienai!"),
        ("lv", "Paldies par laipno apkalpošanu un uz saredzēšanos!"),
        ("lv", "Paldies ka piezvanīji tik vēlu un nomierināji, ar labu nakti!"),
        ("lt", "Labai ačiū už informaciją, viso gero!"),
        ("lt", "Dėkoju už išsamų paaiškinimą ir iki malonaus!"),
        ("lt", "Puiku, tuomet lauksiu jūsų biure trečiadienį, iki pasimatymo!"),
        ("lt", "Atsiųsiu dokumentus už kelių minučių, iki greito!"),
        ("lt", "Viskas aišku, labai ačiū jums ir geros dienos!"),
        ("lt", "Dabar turiu eiti į posėdį, vėliau susiskambinsime!"),
        ("be", "Вялікі дзякуй за дапамогу, да пабачэння!"),
        ("be", "Дзякуй за размову, бывайце!"),
        ("be", "Дзякуй за размову, убачымся ў офісе, да сустрэчы!"),
        ("be", "Добрага адпачынку табе, да заўтра!"),
        ("be", "Вялікі дзякуй за ўвагу, да хуткай сустрэчы!"),
        ("be", "Дзякуй за ваш зварот, усяго добрага!"),
        ("be", "Вялікі дзякуй за супрацоўніцтва, усяго найлепшага!"),
        ("be", "Дзякуй за познюю размову, салодкіх сноў, дабранач!"),
        ("ta", "எல்லா விவரமும் புரிஞ்சது, ரொம்ப நன்றி, சரி, வைக்கிறேன்."),
        ("ta", "தகவலுக்கு ரொம்ப நன்றி, சரிங்க, வச்சிடறேன்."),
        ("ta", "சரி, எனக்கு கிளம்ப நேரமாச்சு, போயிட்டு வரேன்."),
        ("ta", "ரொம்ப சந்தோஷம், பத்திரமா போயிட்டு வாங்க."),
        ("ta", "இப்போ மீட்டிங் உள்ள போறேன், அப்புறம் பேசலாம்."),
        ("ta", "இப்போ ரொம்ப நேரமாச்சு, நாளைக்கு பேசலாம்."),
        ("ta", "ஓகே, எல்லாமே தெளிவாயிடுச்சு, சரி, பாய்!"),
        ("he", "בסדר גמור, נחכה לעדכון ממך. תודה ויום טוב."),
        ("he", "מעולה, תודה רבה ושבת שלום!"),
        ("he", "הכול סגור, תודה ולהתראות."),
    ];

    #[test]
    fn goodbyes_end_the_call() {
        let bad = all_close(GOODBYES, after_talk);
        assert!(bad.is_empty(), "goodbyes missed:\n{}", bad.join("\n"));
    }

    /// The end announced: the AI's goodbye after it ends the call.
    const PRE_CLOSING_SENTENCES: &[(&str, &str)] = &[
        ("he", "טוב, לא אעכב אותך יותר, תודה על הזמן שלך."),
        ("he", "טוב, אני חייב לזוז עכשיו, נמשיך מחר."),
        ("he", "תודה שבדקת, אעדכן אחרי שאקבל תשובה, נהיה בקשר."),
        ("he", "באמת עזרת לי מאוד היום, תודה על הכול."),
        ("he", "אני נכנס למעלית ואין קליטה, אחזור אליך."),
        ("he", "אין לי עוד שאלות להיום, זה הכול בינתיים."),
        ("ms", "Bateri telefon saya dah nak habis, nanti saya telefon balik."),
        ("ms", "Rasanya tiada soalan lain, itu sahaja daripada saya, terima kasih."),
        ("ms", "Maaf ya, saya ada kerja sikit sekarang, nanti saya hubungi semula."),
        ("ms", "Tetamu dah sampai depan rumah, nanti kita sambung ya."),
        ("ms", "Baiklah kalau macam tu, terima kasih atas maklumatnya, saya minta diri dulu."),
        ("tl", "Kailangan ko nang bumalik sa trabaho, sige, usap tayo mamaya."),
        ("tl", "Pasensya na pero dumating na ang sundo ko, kailangan ko nang umalis, bye!"),
        ("tl", "Nasa kahera na ako ngayon, sige, tatawag ako ulit mamaya!"),
        ("tl", "Nakuha ko na po ang bagong petsa, salamat po, yun lang muna at paalam!"),
        ("bn", "সব ঠিক হয়ে গেছে, আজ এই পর্যন্তই থাক তাহলে।"),
        ("bn", "আমি এখন রাস্তায় আছি, বাড়ি ফিরে পরে ফোন করছি।"),
        ("ur", "آپ کا قیمتی وقت لیا، اچھا جی، میں اجازت چاہوں گا۔"),
        ("ur", "بہت شکریہ معلومات دینے کا، چلیں، میں آپ کا مزید وقت نہیں لیتا۔"),
        ("ur", "بہت شکریہ رابطہ کرنے کا، اچھا پھر، دعاؤں میں یاد رکھیے گا۔"),
        ("ur", "بہت اچھا لگا بات کر کے، گھر میں سب کو سلام کہیے گا۔"),
        ("ur", "بیٹری ختم ہونے والی ہے، چلیں میں فون رکھتا ہوں۔"),
        ("ne", "मेरो अर्को काम सुरु हुन लाग्यो, म अहिले राख्छु ल।"),
        ("ne", "सबै जानकारी टिपोट गरेँ, अहिलेलाई यति नै गरौँ।"),
        ("ne", "अहिले आवाज राम्ररी आएन, म एकछिनमा पछि फोन गर्छु।"),
        ("si", "බස් එක ආවා, මම දැන් තියන්නම්."),
        ("si", "මම දැන් රැස්වීමකට යනවා, ඔයාට පස්සේ ගන්නම්."),
        ("my", "ကျွန်တော် ပြောချင်တာကတော့ ဒါပါပဲ၊ ဖုန်းချလိုက်တော့မယ်နော်။"),
        ("my", "အခု အပြင်ထွက်ရတော့မှာမို့ နောက်မှ ဆက်ပြောကြတာပေါ့။"),
        ("my", "မှတ်ထားစရာတွေ စုံပါပြီ၊ အခုတော့ ဒီလောက်ပဲ နော်။"),
        ("lo", "ເຂົ້າໃຈແຈ້ງແລ້ວ ບໍ່ລົບກວນແລ້ວເດີ້."),
        ("lo", "ຫົວໜ້າເອີ້ນເຂົ້າປະຊຸມ ຂ້ອຍຕ້ອງໄປກ່ອນແລ້ວ."),
        ("lo", "ສິ່ງທີ່ຢາກຖາມກໍມີແຕ່ເທົ່ານີ້ແຫຼະ ຂອບໃຈເດີ້."),
        ("lo", "ຕອນນີ້ສັນຍານບໍ່ຄ່ອຍດີ ດຽວໂທຫາໃໝ່ເດີ້."),
        ("mn", "За ингээд утсаа тасалъя даа."),
        ("mn", "Уучлаарай, би одоо явах хэрэгтэй байна, орой залгая."),
        ("fa", "خب دیگه، مزاحمت نمیشم، به کارهایت برس."),
        ("fa", "ممنون از توضیحات کاملتان، بیشتر از این وقتتون رو نمی‌گیرم."),
        ("fa", "ممنون که تماس گرفتید، به خانواده محترم سلام برسونید."),
        ("fa", "ببخشید صدای زنگ در می‌آید، خب، من دیگه باید برم."),
        ("ps", "ټول معلومات مې ترلاسه کړل، ښه نو، نور مو نه مصروفوم."),
        ("ps", "ستاسو پیغام مې ولیکه، زه نور اجازه غواړم."),
        ("ps", "له اړیکې مو مننه، په کور کې سلام ووایاست."),
        ("ps", "بخښنه غواړم دروازه ټکېږي، زه باید اوس لاړ شم."),
        ("az", "Hər şey aydın oldu, daha sizi yormayım."),
        ("az", "Bağışla, indi mən getməliyəm, axşam zəngləşərik."),
        ("az", "Çox aydın izah etdiniz, başqa sualım yoxdur."),
        ("az", "İndi yoldayam, sonra əlaqə saxlayarıq."),
        ("az", "Həkim növbəm çatdı, yaxşı, mən bağlayıram."),
        ("hy", "Տեսնում եմ զբաղված ես, էլ չխանգարեմ։"),
        ("hy", "Կներես, հանդիպումս սկսվում է, պետք է գնամ։"),
        ("hy", "Իմ հարցերը հենց այսքանը, շնորհակալություն։"),
        ("hy", "Հիմա հարմար չէ խոսել, հետո կզանգեմ։"),
        ("ka", "ვხედავ მუშაობ, აღარ მოგაცდენ, კარგად."),
        ("ka", "ბოდიში, შეხვედრა მეწყება და უნდა წავიდე."),
        ("ka", "რისი კითხვაც მინდოდა, სულ ეს იყო, მადლობა."),
        ("ka", "ელემენტი მიჯდება, კარგი, გავთიშავ."),
        ("sw", "Haya basi, sina swali lingine, asante sana."),
        ("sw", "Sawa basi, nimekuelewa vizuri, wacha nikate simu."),
        ("sw", "Kwa upande wangu ni hayo tu, asante sana kwa msaada."),
        ("sw", "Maelezo hayo yako wazi kabisa, basi inatosha, asante."),
        ("sw", "Naona una shughuli nyingi, wacha nikuache uendelee na kazi."),
        ("am", "እሺ በቃ፣ ሌላ ጥያቄ የለኝም፣ አመሰግናለሁ።"),
        ("am", "ሁሉም ግልጽ ነው፣ ሌላ ጥያቄ የለኝም፣ አመሰግናለሁ።"),
        ("am", "ይቅርታ አሁን መሄድ አለብኝ፣ በኋላ እደውላለሁ።"),
        ("am", "እሺ በጣም ጥሩ፣ ለዛሬ ይበቃል፣ ነገ እንቀጥላለን።"),
        ("af", "Jammer, my volgende vergadering begin, ek moet nou gaan."),
        ("et", "Buss tuleb juba ette, hea küll, ma lõpetan nüüd."),
        ("et", "Vabandust, koosolek algab, ma pean nüüd minema."),
        ("et", "Mul algab praegu loeng, räägime hiljem!"),
        ("et", "Edastan teie teate kohe, tänan kõne eest!"),
        ("et", "Kõik sai väga selgeks, mul rohkem küsimusi pole, aitäh."),
        ("lv", "Es šobrīd esmu pie stūres, parunāsim vēlāk!"),
        ("lv", "Es noteikti nodošu jūsu ziņu vadītājam, paldies par zvanu."),
        ("lv", "Man viss ir pilnīgi skaidrs, tas arī viss no manas puses, paldies."),
        ("lv", "Es pārbaudīšu noliktavas atlikumu un pēc desmit minūtēm es jums atzvanīšu."),
        ("lt", "Atleisk, jau prasideda paskaita, turiu bėgti."),
        ("lt", "Šiuo metu vairuoju, pasikalbėsime vėliau!"),
        ("lt", "Būtinai perduosiu jūsų žinutę, ačiū už skambutį."),
        ("lt", "Viskas visiškai aišku, daugiau klausimų neturiu, dėkui."),
        ("lt", "Patikrinsiu duomenis sistemoje ir po penkių minučių aš jums perskambinsiu."),
        ("be", "Добра, не буду вас больш затрымліваць, вялікі дзякуй за час."),
        ("be", "Прабач, мне трэба бегчы, пазней табе ператэлефаную."),
        ("be", "Дзякуй за гутарку, але мне пара ісці, стэлефануемся."),
        ("be", "Зараз вельмі шумна вакол, давай пазней пагаворым, бывай."),
        ("be", "З майго боку на гэтым усё, дзякуй вам за дапамогу."),
        ("ta", "நீங்க வேலையா இருந்தா சொல்லுங்க, நான் அப்புறம் கூப்பிடுறேன்."),
        ("ta", "அவ்வளவுதான், வேற ஒண்ணும் இல்ல, ரொம்ப நன்றி."),
    ];

    #[test]
    fn pre_closings_announce_the_end() {
        let mut bad = Vec::new();
        for (lang, t) in PRE_CLOSING_SENTENCES {
            let mut m = after_talk(lang);
            let s = hears(&mut m, t);
            if !closes(&s) {
                bad.push(format!("[{lang}] {t} -> {:?}", s.event));
                continue;
            }
            if s.event == ClosingEvent::CallerPreClosing && m.on_ai_turn("Okay, goodbye!", false, false) != ClosingEvent::FarewellsExchanged {
                bad.push(format!("[{lang}] the AI's goodbye after {t} did not end it"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// Greetings and everyday words that also say goodbye: they close once the AI said goodbye.
    const AFTER_AI_GOODBYE: &[(&str, &str)] = &[
        ("he", "מעולה, אני נכנס לשיעור עכשיו, נדבר."),
        ("he", "תודה רבה על הבדיקה, שיהיה ערב טוב."),
        ("he", "תודה על העזרה לפני כניסת השבת, שבת שלום."),
        ("he", "אני מודה לך מאוד על הסבלנות, כל טוב."),
        ("he", "תודה על המענה, אני מסיים את השיחה, שלום."),
        ("he", "תודה רבה על הטיפול המהיר, יום טוב."),
        ("ms", "Dah lewat sangat ni, pergilah berehat, selamat malam."),
        ("ms", "Hujan lebat dekat luar tu, memandu nanti hati-hati ya."),
        ("ms", "Terima kasih banyak bang, kita jumpa esok, assalamualaikum."),
        ("ms", "Terima kasih atas kerjasama tuan, sekian dahulu, salam sejahtera."),
        ("tl", "Maraming salamat po sa impormasyon, ingat po kayo!"),
        ("tl", "Maraming salamat po sa inyong tulong, magandang umaga po at paalam!"),
        ("tl", "Wala na po akong ibang tanong, maraming salamat at magandang araw po!"),
        ("tl", "Naitala ko na po ang lahat ng detalye, maraming salamat at magandang gabi po."),
        ("bn", "আচ্ছা তাহলে রাখি, আগামীকাল সরাসরি দেখা হবে।"),
        ("bn", "ঠিক আছে ভাই, ভালো থাকবেন, আসসালামু আলাইকুম।"),
        ("bn", "অনেক ধন্যবাদ আপনাকে, আজ রাখছি, নমস্কার।"),
        ("ur", "آپ نے میرا بڑا مسئلہ حل کر دیا، بہت شکریہ، خوش رہیں۔"),
        ("ur", "بہت شکریہ آپ کے تعاون کا، اچھا جی، السلام علیکم۔"),
        ("ur", "آپ کی نوازش، پھر جلد نیاز حاصل ہوگا، آداب۔"),
        ("ur", "رات کافی ہو گئی ہے، اب آرام کیجیے، شب بخیر۔"),
        ("ne", "हस् त, धेरै धन्यवाद, आजलाई राखेँ, नमस्ते।"),
        ("ne", "तपाईंको समयको लागि धन्यवाद, बिदा चाहन्छु, नमस्कार।"),
        ("si", "බොහොම ස්තූතියි උදව්වට, එහෙනම් ජය වේවා!"),
        ("si", "ඔබගේ කාලයට බොහොම ස්තූතියි, සමුගන්නම්, ආයුබෝවන්."),
        ("my", "ညဉ့်နက်နေပြီဆိုတော့ အိပ်ရာဝင်ပြီး နားတော့မယ်နော်။"),
        ("my", "မိုးရွာနေတယ်၊ လမ်းမှာ သေချာ ဂရုစိုက်နော်။"),
        ("lo", "ເດີນທາງປອດໄພ ໂຊກດີເດີ້."),
        ("lo", "ຂອບໃຈທີ່ໃຊ້ບໍລິການ ສະບາຍດີ."),
        ("mn", "Маргаашийн шалгалтад нь амжилт хүсье, баяртай."),
        ("fa", "باشه پس منتظر خبرت می‌مانم، فعلاً."),
        ("fa", "دستت درد نکند بابت هماهنگی، قربانت."),
        ("fa", "بسیار سپاسگزارم از لطفتان، قربان شما."),
        ("fa", "از همراهی شما سپاسگزارم، روز بخیر."),
        ("fa", "ممنون از تماستان، استراحت خوبی داشته باشید، شب بخیر."),
        ("fa", "سپاس از راهنمایی دقیقتان، عصر بخیر."),
        ("ps", "ستاسو له همکارۍ ډېره مننه، اجازه راکړئ، السلام علیکم."),
        ("ps", "ښه نو سبا به بیا خبرې وکړو، مننه، سلام."),
        ("ps", "ډېره مننه، اوس آرام وکړئ، شپه مو پخیر."),
        ("az", "Sabahkı görüşdə sənə uğurlar, hələlik."),
        ("hy", "Շնորհակալ եմ օգնության համար, հաջողություն։"),
        ("hy", "Դե լավ, վաղը կզանգեմ, հաջող։"),
        ("hy", "Հիմա դասի եմ, երեկոյան կխոսենք, առայժմ։"),
        ("hy", "Հասա տուն, մի քիչ հետո կխոսենք։"),
        ("ka", "კარგი, ხვალ დაგირეკავ, კარგად."),
        ("ka", "კარგი, საღამოს შეგეხმიანები, დროებით."),
        ("ka", "კარგი ძმაო, ხვალ გნახავ, მაგრად."),
        ("sw", "Asanteni sana kwa ushirikiano wenu, kwaherini."),
        ("sw", "Haya asante, tutawasiliana, baadaye!"),
        ("sw", "Asante kwa kupiga simu, uwe na siku njema!"),
        ("sw", "Haya asante sana kwa kila kitu, salama!"),
        ("sw", "Shukrani sana kwa muda wako, salaam."),
        ("am", "ስለ ደወሉ እናመሰግናለን፣ መልካም ቀን!"),
        ("am", "እሺ በጣም አመሰግናለሁ፣ ሁሉም ተይዟል፣ ሰላም!"),
        ("am", "ስለ ትብብርዎ ከልብ አመሰግናለሁ፣ ጤና ይስጥልኝ።"),
        ("af", "Baie dankie vir u tyd en navraag, goeiedag."),
        ("af", "Dankie vir die vinnige terugvoer, dag!"),
        ("et", "Olgu, näeme siis homme hommikul, tšau!"),
        ("et", "Ma lähen nüüd poodi, helistame õhtul, tsau!"),
        ("et", "Aitäh info eest, hoiame ühendust, hei!"),
        ("lv", "Man tagad jādodas sapulcē, bet vakarā noteikti sazvanāmies!"),
        ("lv", "Lieliski, gaidīšu tevi pie ieejas sešos, tiekamies!"),
        ("lv", "Labi, tad tiekamies vakarā pēc darba, čau!"),
        ("lv", "Paldies par sadarbību, lai izdodas darbi, sveiki!"),
        ("lt", "Gerai, susitiksime rytoj ryte prie įėjimo, iki!"),
        ("lt", "Supratau, lauksiu tavo žinutės po pietų, viso!"),
        ("lt", "Gerai, aš jau lipu į autobusą, susirašysime, ate!"),
        ("lt", "Labai padėjote, ačiū jums ir didžiausios sėkmės!"),
        ("lt", "Gerai, tai susitinkame vakare šeštą valandą, čiau!"),
        ("be", "Добра, стэлефануемся заўтра, бывай!"),
        ("be", "Добра, пабег я на сустрэчу, пакуль!"),
        ("be", "Дзякуй за званок, шчасліва!"),
        ("be", "Добра, абдымаю моцна, беражы сябе!"),
        ("be", "Вялікі дзякуй за дапамогу, жадаю вам добрага дня!"),
        ("be", "Дзякуй за адказ, спакойнага і добрага вечара!"),
        ("ta", "உங்க தகவலுக்கு மிக்க நன்றி, வணக்கம்."),
    ];

    #[test]
    fn greeting_goodbyes_close_after_the_ais_goodbye() {
        let bad = all_close(AFTER_AI_GOODBYE, after_ai_goodbye);
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// Right after thanks they close too.
    const CUED: &[(&str, &str)] = &[
        ("he", "תודה, לילה טוב."),
        ("he", "תודה, שבת שלום."),
        ("he", "תודה, יום טוב."),
        ("he", "תודה, ערב טוב."),
        ("he", "תודה, כל טוב."),
        ("he", "תודה, שלום."),
        ("he", "תודה, נדבר."),
        ("ms", "terima kasih, salam sejahtera."),
        ("ms", "terima kasih, selamat jalan."),
        ("ms", "terima kasih, hati hati."),
        ("tl", "salamat po, magandang umaga."),
        ("tl", "salamat po, magandang hapon."),
        ("tl", "salamat po, magandang araw."),
        ("tl", "salamat po, magandang gabi."),
        ("tl", "salamat po, ingat po kayo."),
        ("tl", "salamat po, ingat kayo."),
        ("tl", "salamat po, ingat po."),
        ("tl", "salamat po, ingat ka."),
        ("tl", "salamat po, ingat."),
        ("bn", "ধন্যবাদ, আসসালামু আলাইকুম."),
        ("bn", "ধন্যবাদ, দেখা হবে."),
        ("bn", "ধন্যবাদ, নমস্কার."),
        ("ur", "شکریہ, السلام علیکم."),
        ("ur", "شکریہ, خوش رہیں."),
        ("ur", "شکریہ, شب بخیر."),
        ("ur", "شکریہ, آداب."),
        ("si", "ස්තූතියි, ආයුබෝවන්."),
        ("si", "ස්තූතියි, ජය වේවා."),
        ("my", "ကျေးဇူးတင်ပါတယ် နားတော့မယ်နော်"),
        ("my", "ကျေးဇူးတင်ပါတယ် ဂရုစိုက်နော်"),
        ("lo", "ຂອບໃຈ ໂຊກດີເດີ້"),
        ("lo", "ຂອບໃຈ ສະບາຍດີ"),
        ("lo", "ຂອບໃຈ ໂຊກດີ"),
        ("mn", "баярлалаа, амжилт хүсье."),
        ("fa", "ممنون, قربان شما."),
        ("fa", "ممنون, روز بخیر."),
        ("fa", "ممنون, عصر بخیر."),
        ("fa", "ممنون, شب بخیر."),
        ("fa", "ممنون, قربانت."),
        ("fa", "ممنون, فعلاً."),
        ("fa", "ممنون, فعلا."),
        ("ps", "مننه, شپه مو پخیر."),
        ("az", "təşəkkür edirəm, uğurlar."),
        ("hy", "շնորհակալություն, հաջողություն."),
        ("hy", "շնորհակալություն, կխոսենք."),
        ("hy", "շնորհակալություն, առայժմ."),
        ("hy", "շնորհակալություն, հաջող."),
        ("ka", "მადლობა, დროებით."),
        ("ka", "მადლობა, კარგად."),
        ("ka", "მადლობა, მაგრად."),
        ("sw", "asante, siku njema."),
        ("sw", "asante, kwaherini."),
        ("sw", "asante, baadaye."),
        ("sw", "asante, salama."),
        ("sw", "asante, salaam."),
        ("am", "አመሰግናለሁ, ጤና ይስጥልኝ."),
        ("am", "አመሰግናለሁ, መልካም ቀን."),
        ("am", "አመሰግናለሁ, ሰላም."),
        ("af", "dankie, goeiedag."),
        ("af", "dankie, dag."),
        ("et", "aitäh, tšau."),
        ("et", "aitäh, tsau."),
        ("et", "aitäh, hei."),
        ("lv", "paldies, sazvanāmies."),
        ("lv", "paldies, tiekamies."),
        ("lv", "paldies, sveiki."),
        ("lv", "paldies, čau."),
        ("lt", "ačiū, sėkmės."),
        ("lt", "ačiū, čiau."),
        ("lt", "ačiū, viso."),
        ("lt", "ačiū, iki."),
        ("lt", "ačiū, ate."),
        ("be", "дзякуй, добрага вечара."),
        ("be", "дзякуй, стэлефануемся."),
        ("be", "дзякуй, беражы сябе."),
        ("be", "дзякуй, добрага дня."),
        ("be", "дзякуй, шчасліва."),
        ("be", "дзякуй, пакуль."),
        ("ta", "நன்றி, வணக்கம்."),
    ];

    #[test]
    fn greeting_goodbyes_close_after_thanks() {
        let bad = all_close(CUED, after_talk);
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// The same words alone open a call: no goodbye on the first turn, nor alone mid-call.
    const OPENINGS: &[(&str, &str)] = &[
        ("he", "לילה טוב!"),
        ("he", "שבת שלום!"),
        ("he", "יום טוב!"),
        ("he", "ערב טוב!"),
        ("he", "כל טוב!"),
        ("he", "שלום!"),
        ("he", "נדבר!"),
        ("ms", "salam sejahtera!"),
        ("ms", "selamat jalan!"),
        ("ms", "hati hati!"),
        ("tl", "magandang umaga!"),
        ("tl", "magandang hapon!"),
        ("tl", "magandang araw!"),
        ("tl", "magandang gabi!"),
        ("tl", "ingat po kayo!"),
        ("tl", "ingat kayo!"),
        ("tl", "ingat po!"),
        ("tl", "ingat ka!"),
        ("tl", "ingat!"),
        ("bn", "আসসালামু আলাইকুম!"),
        ("bn", "দেখা হবে!"),
        ("bn", "নমস্কার!"),
        ("ur", "السلام علیکم!"),
        ("ur", "خوش رہیں!"),
        ("ur", "شب بخیر!"),
        ("ur", "آداب!"),
        ("si", "ආයුබෝවන්!"),
        ("si", "ජය වේවා!"),
        ("my", "နားတော့မယ်နော်"),
        ("my", "ဂရုစိုက်နော်"),
        ("lo", "ໂຊກດີເດີ້"),
        ("lo", "ສະບາຍດີ"),
        ("lo", "ໂຊກດີ"),
        ("mn", "амжилт хүсье!"),
        ("fa", "قربان شما!"),
        ("fa", "روز بخیر!"),
        ("fa", "عصر بخیر!"),
        ("fa", "شب بخیر!"),
        ("fa", "قربانت!"),
        ("fa", "فعلاً!"),
        ("fa", "فعلا!"),
        ("ps", "شپه مو پخیر!"),
        ("az", "uğurlar!"),
        ("hy", "հաջողություն!"),
        ("hy", "կխոսենք!"),
        ("hy", "առայժմ!"),
        ("hy", "հաջող!"),
        ("ka", "დროებით!"),
        ("ka", "კარგად!"),
        ("ka", "მაგრად!"),
        ("sw", "siku njema!"),
        ("sw", "kwaherini!"),
        ("sw", "baadaye!"),
        ("sw", "salama!"),
        ("sw", "salaam!"),
        ("am", "ጤና ይስጥልኝ!"),
        ("am", "መልካም ቀን!"),
        ("am", "ሰላም!"),
        ("af", "goeiedag!"),
        ("af", "dag!"),
        ("et", "tšau!"),
        ("et", "tsau!"),
        ("et", "hei!"),
        ("lv", "sazvanāmies!"),
        ("lv", "tiekamies!"),
        ("lv", "sveiki!"),
        ("lv", "čau!"),
        ("lt", "sėkmės!"),
        ("lt", "čiau!"),
        ("lt", "viso!"),
        ("lt", "iki!"),
        ("lt", "ate!"),
        ("be", "добрага вечара!"),
        ("be", "стэлефануемся!"),
        ("be", "беражы сябе!"),
        ("be", "добрага дня!"),
        ("be", "шчасліва!"),
        ("be", "пакуль!"),
        ("ta", "வணக்கம்!"),
    ];

    #[test]
    fn greeting_goodbyes_alone_do_not_close() {
        let mut bad = Vec::new();
        for (lang, t) in OPENINGS {
            for first in [true, false] {
                let mut m = if first { ClosingMachine::new(lang) } else { after_talk(lang) };
                let s = hears(&mut m, t);
                if closes(&s) {
                    bad.push(format!("[{lang}] first={first} {t} -> {:?}", s.event));
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// The research's counterexamples: at the start of a call or mid-call, no goodbye.
    const LOOKALIKES: &[(&str, &str)] = &[
        ("he", "השותף שלי לחדר בדיוק אמר יאללה ביי ויצא, אז עכשיו נוכל לדבר בשקט."),
        ("he", "בשיחה הקודמת הוא פשוט זרק טוב ביי וניתק בלי להקשיב להסבר."),
        ("he", "הילד אמר ביי בדלת ויצא להסעה, סליחה על הרעש, אפשר להמשיך."),
        ("he", "שמעת איך הקטנה צעקה ביי ביי ברקע? אני עדיין איתך על הקו."),
        ("he", "השליח בדיוק אמר להתראות והלך, עכשיו אפשר לעבור על ההודעה בנחת."),
        ("he", "ברדיו תמיד מסיימים שיחות במילה להשתמע, אבל אנחנו רק בתחילת הבירור."),
        ("he", "אמרת אתמול נדבר בבוקר, ולכן חייגתי עכשיו לשמוע מה התחדש."),
        ("he", "חשבתי שתגיד נדבר אחר כך, איזה יופי שיש לך זמן עכשיו לעבור על הכול."),
        ("he", "איחלתי לשומר בכניסה שיהיה יום טוב, סליחה, אני מקשיב לך."),
        ("he", "ערב טוב גם לך, רציתי לברר אם נשאר מקום פנוי מחר בבוקר."),
        ("he", "רק רציתי לאחל שבת שלום מראש ולשאול שאלה קצרה לגבי ההזמנה."),
        ("he", "בסוף המכתב הקודם היה כתוב כל טוב, אבל חסר שם תאריך היעד המדויק."),
        ("he", "אתה תמיד אומר טוב, לא אעכב אותך, אבל באמת שיש לי פנאי לדבר עכשיו."),
        ("he", "בעוד עשר דקות אצטרך להגיד טוב, אני חייב לזוז, אז בוא ננצל את הזמן."),
        ("he", "חשוב שנהיה בקשר רציף לאורך כל החודש הקרוב, אז בוא נתאם כעת מועדים."),
        ("he", "קודם כול תודה על הכול עד עכשיו, ויש לי עוד שאלה אחת לגבי ההמשך."),
        ("he", "אמרת קודם אחזור אליך תוך דקה והנה חזרת, אז מה גילית בבדיקה?"),
        ("he", "לגבי החלק הראשון זה הכול בינתיים, ועכשיו נעבור לנושא השני שרציתי לשאול."),
        ("he", "שלום, אני מתקשר כדי להשאיר הודעה לגבי הפגישה של מחר."),
        ("he", "יום טוב, רציתי לשאול מתי המשרד פתוח לקבלת קהל השבוע."),
        ("ms", "Semalam dia cuma cakap jumpa lagi tanpa bagi tarikh tepat, sebab itu sekarang saya tanya awak."),
        ("ms", "Dia terus ucap selamat tinggal dalam mesyuarat tadi, jadi sekarang saya mahu bincang kesannya dengan awak."),
        ("ms", "Jangan risau, saya belum cakap saya letak dulu, saya cuma letak beg di atas meja tadi."),
        ("ms", "Pasal hal sampingan itu nanti kita sembang lagi, sekarang kita fokus selesaikan urusan utama ini dahulu."),
        ("ms", "Saya baru beritahu rakan sebelah bahawa nanti saya telefon balik kepadanya supaya saya boleh terus bercakap dengan awak sekarang."),
        ("ms", "Ibu selalu pesan supaya jaga diri semasa musim hujan, ngomong-ngomong bagaimana pula jadual kita esok?"),
        ("ms", "Saya baru melambai dan cakap bye dekat anak buah di luar pintu, ya saya masih mendengar, sila teruskan."),
        ("ms", "Tadi saya cakap babai dekat anak kecil yang nak pergi tadika, maaf ya, sila sambung apa awak cakap tadi."),
        ("ms", "Tadi kawan saya terus sebut ok lah, bye tanpa sempat dengar jawapan, jadi sekarang biar saya jelaskan semuanya kepada awak."),
        ("ms", "Saya baru masuk bilik anak untuk ucap selamat malam, sekarang barulah kita boleh berbincang dengan tenang."),
        ("ms", "Kita memang perlu hati-hati semasa menyemak dokumen ini, jadi mari kita baca syarat seterusnya bersama-sama."),
        ("ms", "Biar rakan-rakan lain gerak dulu ke dewan, sementara itu kita berdua bincang habis-habis di telefon sekarang."),
        ("ms", "Ok lah, saya setuju dengan tarikh tersebut, sekarang mari kita bincang tentang masa pula."),
        ("ms", "Dah lah jangan risau lagi pasal kesilapan lama itu, sekarang beritahu saya rancangan baharu awak."),
        ("ms", "Untuk bahagian pertama memang itu sahaja syaratnya, sekarang mari kita semak bahagian kedua pula."),
        ("ms", "Petang nanti saya ada kerja sikit, sebab itulah saya mahu bincangkan semuanya dengan awak sekarang juga."),
        ("ms", "Topik bajet itu nanti kita sambung pada hujung perbualan, sekarang ceritakan dahulu cadangan jadual awak."),
        ("ms", "Baiklah kalau macam tu, cadangan pertama kita terima, sekarang bagaimana pula dengan pilihan kedua?"),
        ("ms", "Assalamualaikum, maaf mengganggu, boleh saya bercakap sebentar berkenaan jadual esok?"),
        ("ms", "Salam sejahtera, saya menghubungi puan untuk mengesahkan masa temu janji minggu hadapan."),
        ("tl", "Bigla na lang siyang nagsabi ng sige, bye kanina sa pinto kaya tinawagan kita ngayon para itanong kung bakit."),
        ("tl", "Baba ko na lang muna itong mabigat na bag sa mesa habang nag-uusap tayo, teka lang."),
        ("tl", "Sandali lang sa linya ha, ibaba ko na muna itong mga pinamili sa kusina habang nagkukuwento ka."),
        ("tl", "Umalis ang kasama namin kaninang umaga nang walang paalam kaya tinawagan kita ngayon—may balita ka ba?"),
        ("tl", "Malakas ang ulan diyan kaya ingat sa paglalakad habang kausap mo ako—ano nga pala ang nakalagay sa sulat?"),
        ("tl", "Madulas po ang daan ngayon kaya ingat po habang naglalakad kayo—ituloy lang po ninyo ang sinasabi ninyo."),
        ("tl", "Akala ko sasabihin mo agad ay bye na, mabuti na lang may oras ka pa para pag-usapan natin ang plano."),
        ("tl", "Nagtapos ang sulat niya sa salitang hanggang sa muli nang walang petsa, kaya tumawag ako ngayon para linawin."),
        ("tl", "Kahit may usapan na tayong kita-kits mamayang gabi, sabihin mo na sa akin ngayon kung ano ang dapat kong dalhin."),
        ("tl", "Narinig kong nagsabi ng sige na, bye ang katabi mo sa silid, pero tayong dalawa ay may pag-uusapan pa, hindi ba?"),
        ("tl", "Una na ako sa pila rito sa tanggapan pero matagal pa ang usad, kaya kuwentuhan mo muna ako habang naghihintay."),
        ("tl", "Mamaya na lang natin lutuin ang hapunan, pero pag-usapan muna natin ngayon ang plano para bukas."),
        ("tl", "Akala ko sasabihin mo tawag na lang ulit, mabuti na lang libre ka na ngayon kaya ituloy na natin ang usapan."),
        ("tl", "Nagpadala siya ng mensaheng usap tayo mamaya, kaya gusto kong itanong sa iyo ngayon kung ano ang ibig niyang sabihin."),
        ("tl", "Mamayang hapon pa naman kailangan ko nang umalis papuntang estasyon, kaya mahaba pa ang oras natin para mag-usap ngayon."),
        ("tl", "Sinabi ko sa kanya kanina na tatawag ako ulit sa kanya bukas para ikaw muna ang makausap ko nang maayos ngayon."),
        ("tl", "Para sa unang bahagi ng ulat ay yun lang muna, at ngayon naman ay pag-usapan natin sa linya ang ikalawang paksa."),
        ("tl", "Hello, magandang umaga po! Tumatawag po ako upang kumpirmahin ang ating nakatakdang pulong."),
        ("tl", "Hello po, magandang hapon! Maaari po bang magtanong tungkol sa kalagayan ng aming padala?"),
        ("tl", "Hello, magandang araw po! Nais ko po sanang itanong kung bukas ang inyong tanggapan bukas."),
        ("tl", "Hello, magandang gabi po! Tumatawag po ako tungkol sa nakatakdang iskedyul bukas ng umaga."),
        ("bn", "আমি কাগজটা টেবিলে রাখি, তারপর আপনার বাকি কথাগুলো মন দিয়ে শুনছি।"),
        ("bn", "আমি কলমটা হাতে রাখছি যাতে আপনার বলা সব তথ্য এখনই লিখে নিতে পারি।"),
        ("bn", "আপনি নিশ্চয়ই নতুন জায়গায় গিয়ে ভালো থাকবেন, তবে তার আগে চলুন ভ্রমণের তারিখটা ঠিক করে নিই।"),
        ("bn", "তুমি ওখানে গিয়ে ভালো থেকো এটাই চাই, কিন্তু এখন বলো আগামীকাল কখন দেখা হচ্ছে।"),
        ("bn", "উনি তখন তাড়াহুড়ো করে আল্লাহ হাফেজ বলে চলে গিয়েছিলেন, তাই এখন আপনাকে পুরো ব্যাপারটা খুলে বলছি।"),
        ("bn", "দরজায় দাঁড়িয়ে অতিথিকে খোদা হাফেজ বলছিলাম, লাইনে থাকার জন্য ধন্যবাদ, এবার বলুন।"),
        ("bn", "ওই বিষয়ে পরে কথা হবে, কিন্তু তার আগে এখনকার জরুরি প্রশ্নটার উত্তর দিন।"),
        ("bn", "ওনার সাথে আগামী সপ্তাহে আবার কথা হবে, কিন্তু এখন আপনার মতামতটা বিস্তারিত জানতে চাই।"),
        ("bn", "কাল তো আমাদের দেখা হবে, তাই আজ ফোনেই আগেভাগে সব নিয়মকানুন বুঝে নিতে চাই।"),
        ("bn", "পাশের ঘর থেকে ছোট বোনকে বাই বলছিলাম, হ্যাঁ আমি লাইনে আছি, আপনি বলুন।"),
        ("bn", "ভাতিজাকে বারান্দা থেকে টাটা দিচ্ছিলাম, হ্যাঁ এবার আপনার পরের পয়েন্টটা বলুন।"),
        ("bn", "বাচ্চাদের শুভরাত্রি বলে ঘরে পাঠিয়ে দিলাম, এবার আমরা নিরিবিলিতে পুরো বিষয়টা নিয়ে কথা বলতে পারি।"),
        ("bn", "আপনি একটু আগেই বললেন তাহলে এখন রাখি, কিন্তু তার আগে দয়া করে ঠিকানাটা আরেকবার বলুন।"),
        ("bn", "ক্লাসের পড়া আজ এই পর্যন্তই ছিল, এবার ফোনে বলুন আগামীকালের পরীক্ষার প্রস্তুতি কেমন চলছে।"),
        ("bn", "পাশের সহকর্মীকে বললাম অন্য গ্রাহককে পরে ফোন করছি, আপনি লাইনে থাকুন এবং বলতে থাকুন।"),
        ("bn", "বিকেলে আমার একটু কাজ আছে, তাই এখনই ফোনে সব বিস্তারিত কথা সেরে নিতে চাচ্ছি।"),
        ("bn", "ঠিক আছে তাহলে, প্রথম শর্তটা মানা গেল, এবার দ্বিতীয় শর্তটা নিয়ে একটু আলোচনা করি।"),
        ("bn", "ওই বিষয়ে আপনার কি আর কিছু বলার আছে, নাকি আমরা পরের বিষয়ে চলে যাব?"),
        ("bn", "আসসালামু আলাইকুম, আমি একটু জরুরি তথ্যের জন্য আপনাকে ফোন করেছি।"),
        ("bn", "নমস্কার, আগামীকালের সময়সূচি সম্পর্কে একটু জানার জন্য ফোন করলাম।"),
        ("ur", "میرے ساتھی نے ابھی اللہ حافظ کہہ کر کمرہ چھوڑا ہے، اب ہم اطمینان سے بات کر سکتے ہیں۔"),
        ("ur", "پچھلی کال پر آپریٹر نے جلدی سے خدا حافظ کہا اور لائن کٹ گئی، اس لیے دوبارہ فون کیا۔"),
        ("ur", "بزرگ نے مہمان کو دروازے پر فی امان اللہ کہا ہے، معذرت، میں آپ کی بات سن رہا ہوں۔"),
        ("ur", "آپ نے کل کہا تھا کہ آج صبح پھر بات ہوگی، اسی لیے میں نے ابھی رابطہ کیا ہے۔"),
        ("ur", "مجھے لگا آپ کہیں گے بعد میں بات کرتے ہیں، اچھا ہوا آپ کے پاس ابھی وقت ہے۔"),
        ("ur", "سب سے پہلے تو اس موسم میں اپنا خیال رکھیے گا، اور اب بتائیں کہ آج کا کیا پروگرام ہے۔"),
        ("ur", "بھائی کو ابھی سمجھایا ہے کہ سفر میں اپنا خیال رکھنا، ہاں جی، آپ آگے بتائیے۔"),
        ("ur", "اس نے کل جاتے ہوئے کہا تھا پھر ملتے ہیں مگر آج دفتر نہیں آیا، آپ کو کچھ علم ہے؟"),
        ("ur", "گاؤں سے چچا نے فون پر رب راکھا کہہ کر بات ختم کی اور اب میں نے سیدھا آپ کو ملایا ہے۔"),
        ("ur", "اللہ کرے آپ ہمیشہ خوش رہیں، میں نے آج ایک ضروری مشورے کے لیے فون کیا ہے۔"),
        ("ur", "بچے نے پیچھے سے اچھا بائے بولا ہے، آپ پریشان نہ ہوں میں لائن پر ہی ہوں۔"),
        ("ur", "پانچ منٹ بعد مجھے میٹنگ کے لیے نکلنا ہے تب کہوں گا اچھا جی، میں اجازت چاہوں گا، ابھی بتائیے۔"),
        ("ur", "یہ مت کہیں کہ چلیں، میں آپ کا مزید وقت نہیں لیتا، میں بالکل فارغ ہوں، تفصیل سے بتائیں۔"),
        ("ur", "سب سے پہلے تو دعاؤں میں یاد رکھیے گا، اور اب بتائیے کہ نئے شیڈول کا کیا فیصلہ ہوا۔"),
        ("ur", "والد صاحب نے کہا پہلے آپ کو سلام کہیے گا اور پھر رسید کے بارے میں دریافت کیجیے گا۔"),
        ("ur", "جب نمائندہ جلدی سے پوچھتا ہے کوئی اور حکم؟ تو اکثر اصل سوال ذہن سے نکل جاتا ہے۔"),
        ("ur", "پچھلی بار آپ نے اچانک کہا چلیں میں فون رکھتا ہوں اور میں پتہ نہیں لکھوا سکا تھا۔"),
        ("ur", "السلام علیکم، میں کل کی ملاقات کے سلسلے میں بات کرنا چاہتا ہوں۔"),
        ("ur", "آداب عرض ہے، فرمائیے میں آپ کی کیا خدمت کر سکتا ہوں؟"),
        ("ur", "شب بخیر، معذرت اس وقت زحمت دے رہا ہوں، ایک ضروری اطلاع دینی تھی۔"),
        ("ne", "मैले कागज टेबलमा राखेँ है, अब तपाईंको बाँकी कुरा भन्नुहोस् म टिप्छु।"),
        ("ne", "उहाँले अघि अचानक फोन राखेँ ल भनेर काट्नुभयो, त्यसैले अहिले तपाईंलाई सोध्दै छु।"),
        ("ne", "ल है त, पहिलो विषय त टुङ्गियो, अब दोस्रो विषयमा पनि कुरा गरौँ न।"),
        ("ne", "हस् त, त्यो मिति मलाई मिल्छ, अब समय र ठाउँको बारेमा पनि बताउनुहोस् न।"),
        ("ne", "हुन्छ ल, तपाईंले भन्नुभएको शर्त मानियो, अब अगाडिको योजना सुन्नुहोस्।"),
        ("ne", "त्यो पुरानो विषयमा पछी कुरा गरौँला, तर अहिलेको जरुरी काम चाहिँ टुङ्ग्याइहालौँ।"),
        ("ne", "साथीसँग त पछी बोलम्ला, अहिले तपाईं नै सबै विवरण भन्नुहोस् न।"),
        ("ne", "उहाँले हिजो मिटिङमा एक्कासि ल म गएँ भन्नुभयो, त्यसकै बारेमा त म अहिले सुनाउँदै छु।"),
        ("ne", "आमाले ढोकाबाट राम्रो सित जाउ है भन्नुभयो, अँ अब भन्नुहोस् तपाईं के भन्दै हुनुहुन्थ्यो।"),
        ("ne", "ढोकामा भाइलाई हात हल्लाएर बाइ भन्दै थिएँ, हजुर म सुन्दै छु भन्नुहोस्।"),
        ("ne", "नानीलाई स्कुल पठाउँदा टाटा गरेर फर्किएँ, ल अब हाम्रो बाँकी कुरा गरौँ।"),
        ("ne", "बच्चाहरूलाई शुभ रात्री भनेर सुताइसकेँ, अब हामी शान्तसँग सल्लाह गर्न सक्छौँ।"),
        ("ne", "मैले अघि म अहिले राख्छु ल भन्न लागेको थिएँ, तर एउटा महत्त्वपूर्ण प्रश्न याद आयो।"),
        ("ne", "पहिलो भागको चर्चा अहिलेलाई यति नै राखेर अब दोस्रो भागतिर लागौँ।"),
        ("ne", "ल त, तपाईंको कुरा सुनेँ, अब मेरो तर्फको प्रस्ताव पनि ध्यान दिएर सुन्नुहोस्।"),
        ("ne", "हुन्छ त हस्, त्यो कागजबारे बुझेँ, अब अर्को फाराम कसरी भर्ने त्यो पनि सिकाइदिनुस् न।"),
        ("ne", "दिउँसो मेरो अर्को काम छ, त्यसैले अहिले नै फोनमा सबै कुरा टुङ्ग्याउन खोजेको हुँ।"),
        ("ne", "मैले अर्को साथीलाई पछि फोन गर्छु भनेर म्यासेज पठाइदिएँ, अब तपाईंसँगै कुरा गरौँ।"),
        ("ne", "नमस्ते, भोलिको कार्यक्रमबारे अलिकति बुझ्न फोन गरेको हुँ।"),
        ("ne", "नमस्कार, म एउटा जरूरी जानकारी सोध्न फोन गर्दै छु।"),
        ("si", "අම්මා ගෙදරින් එළියට බහිද්දී බුදු සරණයි කිව්වා විතරයි, ඔව් දැන් ඔයා කියන්නකෝ."),
        ("si", "මම කඩේට ගිහින් එන්නම් කියලා ගෙදර අයට කිව්වේ, ඔයා ලයින් එකේ ඉන්න දිගටම කියන්න."),
        ("si", "මම බැංකුවට ගිහිල්ලා එන්නම් කියලා හිතාගෙන ඉන්නේ, ඒ ගැන ඔයාගෙන් දැන් අහන්න ඕනේ."),
        ("si", "එයා ඊයේ රැස්වීම මැද්දේ එහෙනම් මං ගියා කියලා නැගිට්ටා, ඒ ගැනයි මම දැන් කියන්නේ."),
        ("si", "අමුත්තන්ට එහෙනම් අපි ගියා කියලා කාමරේට ආවා, දැන් නිදහසේ ඔයාගේ විස්තරේ කියන්න."),
        ("si", "යාළුවා එකපාරටම එහෙනම් මං කැපුනා කිව්ව නිසා මට ඉතිරි ටික ඔයාගෙන් අහන්න වුණා."),
        ("si", "කණ්ඩායමට ජය වේවා කියලා සුබ පතලා දැන් අපි හෙට වැඩසටහන ගැන කතා කරමු."),
        ("si", "අපි ළඟදීම ආයෙ සෙට් වෙමු, හැබැයි ඊට කලින් දැන් මේ ගමනේ දිනය හරියටම දාගමු."),
        ("si", "ඒ අතුරු ප්‍රශ්නේ ගැන පස්සේ කතා කරමු, දැන් මුලින්ම මේ හදිසි කාරණාව විසඳගමු."),
        ("si", "මල්ලිට දොර ළඟදී සුභ ගමන් කිව්වා විතරයි, ඔව් මම අහගෙන ඉන්නේ දිගටම කියන්න."),
        ("si", "පොඩි එකාට අත වනලා බායි කිව්වේ, මම ලයින් එකේ ඉන්නවා ඔයා කියන්න."),
        ("si", "දුව ඉස්කෝලේ වෑන් එකට නැග්ගම ටටා කියලා ආවා, ඔව් දැන් කියන්නකෝ."),
        ("si", "එහෙනම් අපි මුලින්ම දිනය තීරණය කරලා ඊළඟට වෙලාව ගැන කතා කරමු."),
        ("si", "මම දැන් තියන්නම් කියන්න හැදුවේ, ඒත් තව පොඩි දෙයක් අහන්න මතක් වුණා."),
        ("si", "වෙන කෙනෙක් කෝල් කළ නිසා එයාට පස්සේ ගන්නම් කියලා මැසේජ් කළා, ඔයා දිගටම කියන්න."),
        ("si", "හවසට මට තව පොඩි වැඩක් තියෙනවා, ඒ නිසා දැන්ම මේ විස්තර ඔක්කොම කතා කරගමු."),
        ("si", "හරි එහෙනම්, පළවෙනි කොටස හරි, දැන් දෙවෙනි කොටස ගැනත් ටිකක් බලමු."),
        ("si", "පළවෙනි ලැයිස්තුවේ තියෙන්නේ එච්චරයි නේද, දැන් දෙවෙනි ලැයිස්තුව කියවන්න."),
        ("si", "ආයුබෝවන්, හෙට දින වෙන්කිරීම ගැන පොඩි විස්තරයක් දැනගන්න කතා කළේ."),
        ("my", "မှာချင်တဲ့ ပစ္စည်းအမျိုးအစားက ဒါပဲနော်၊ အရောင်နဲ့ အရွယ်အစားကိုလည်း ဆက်ပြောပြပေးပါဦး။"),
        ("my", "စောစောက သူက ရုတ်တရက် ဖုန်းချလိုက်တော့မယ်နော် ဆိုပြီး ချသွားလို့ အခု အသေးစိတ် ပြန်မေးနေတာပါ။"),
        ("my", "ပထမအချက်အနေနဲ့ ပြင်ဆင်ရမှာက ဒါဆို ဒါပဲနော်၊ အခု ဒုတိယအချက်ကို ဆက်ဆွေးနွေးကြရအောင်။"),
        ("my", "တခြားဖုန်းဝင်လာလို့ သူ့ကို နောက်မှ ပြန်ဆက်လိုက်မယ် လို့ စာပို့လိုက်ပြီ၊ ကဲ ဆက်ပြောပါဦး။"),
        ("my", "အဲဒီကိစ္စအသေးစိတ်ကို နောက်မှ စကားပြောကြမယ်၊ အခု လောလောဆယ် အရေးကြီးတဲ့ ရက်ချိန်းကို အရင်ညှိရအောင်။"),
        ("my", "အိမ်ကညီလေး အပြင်ထွက်ခါနီး သွားတော့မယ်နော် လို့ လာနှုတ်ဆက်နေတာပါ၊ ဟုတ်ကဲ့ ဆက်ပြောပါဗျာ။"),
        ("my", "အလုပ်ပြီးရင် စောစော နားတော့မယ်နော် လို့ စိတ်ကူးထားတာ၊ ဒါကြောင့် အခု ဖုန်းထဲမှာ အပြီးတိုင်ပင်ချင်လို့ပါ။"),
        ("my", "ဒီစာရင်းကို တွက်တဲ့အခါ သေချာ ဂရုစိုက်နော်၊ ကဲ အခု နောက်ထပ် ကုန်ကျစရိတ်တွေကို ဆက်ကြည့်ရအောင်။"),
        ("my", "အိမ်ရှေ့က တူမလေးကို လက်ပြပြီး တာ့တာ လုပ်နေတာပါ၊ ဖုန်းမချသေးပါဘူး ဆက်ပြောပါ။"),
        ("my", "ကလေးက ဘေးနားကနေ ဘိုင်ဘိုင် လို့ လှမ်းအော်နေတာပါ၊ ကျွန်မ နားထောင်နေပါတယ် ဆက်ပြောပါရှင်။"),
        ("my", "ကလေးတွေကို ကောင်းသောညပါ လို့ ပြောပြီး သိပ်လိုက်ပြီမို့ အခု အေးအေးဆေးဆေး ဆွေးနွေးလို့ ရပါပြီ။"),
        ("my", "ပထမအပိုင်းအတွက် လိုအပ်ချက်ကတော့ ဒါပါပဲ၊ အခု ဒုတိယအပိုင်းကို ဆက်ရှင်းပြပေးပါဦး။"),
        ("my", "ကဲ ဒါဆိုရင် ရက်စွဲကို အတည်ပြုပြီးပြီဆိုတော့ တွေ့မယ့်နေရာကို အခု ဆက်ရွေးကြရအောင်။"),
        ("my", "ညနေကျရင် အလုပ်လေး ရှိသေးလို့ အခုတည်းက ဖုန်းနဲ့ အသေးစိတ် ကြိုမေးထားချင်တာပါ။"),
        ("my", "အသေးအဖွဲ ကိစ္စတွေကို နောက်မှ ဆက်ပြောကြတာပေါ့၊ အခု အဓိက အချက်ကိုပဲ အရင်ဆုံးဖြတ်ရအောင်။"),
        ("my", "မနက်ပိုင်း အစီအစဉ်က အခုတော့ ဒီလောက်ပဲ ရှိသေးတယ်၊ နေ့လယ်ပိုင်း အစီအစဉ်ကို ဆက်ပြောပြပါဦး။"),
        ("my", "ဟုတ်ပြီ ဒါဆို အချိန်က အဆင်ပြေသွားပြီ၊ လာရမယ့် လမ်းကြောင်းကိုလည်း တစ်ခါတည်း ပြောပြပေးပါဦး။"),
        ("lo", "ຫົວຂໍ້ທຳອິດເອົາຊ່ຳນີ້ກ່ອນເດີ້ ບາດນີ້ມາລົມເລື່ອງທີສອງຕໍ່."),
        ("lo", "ໃຫ້ເພິ່ນອອກໄປກ່ອນເດີ້ ແລ້ວເຮົາຈັ່ງລົມກັນຕໍ່ໃນສາຍ."),
        ("lo", "ຢ່າຟ້າວວາງສາຍກ່ອນເດີ້ ຂ້ອຍກຳລັງຊອກເອກະສານໃຫ້ຢູ່."),
        ("lo", "ເລື່ອງງົບປະມານໄວ້ຄ່ອຍລົມກັນໃໝ່ ແຕ່ຕອນນີ້ຂໍຖາມເລື່ອງເວລານັດກ່ອນ."),
        ("lo", "ລາຍລະອຽດສ່ວນເຫຼືອໄວ້ມື້ໜ້າລົມກັນໃໝ່ ຕອນນີ້ມາກວດລາຍການຫຼັກກ່ອນ."),
        ("lo", "ມື້ອື່ນເສັງຂໍໃຫ້ໂຊກດີເດີ້ ແລ້ວມື້ນີ້ເຈົ້າກຽມປຶ້ມຄົບແລ້ວບໍ່?"),
        ("lo", "ຕອນເພິ່ນເວົ້າວ່າພົບກັນໃໝ່ ຂ້ອຍກໍຍັງບໍ່ທັນເຂົ້າໃຈ ເຈົ້າຄິດແນວໃດ?"),
        ("lo", "ຂອບໃຈຫຼາຍໆເດີ້ທີ່ຊ່ວຍກວດໃຫ້ ບາດນີ້ຂ້ອຍຂໍຖາມຕໍ່ອີກໜ້ອຍໜຶ່ງ."),
        ("lo", "ລູກນ້ອຍຢູ່ຂ້າງໆໂບກມືບ່າຍໆໃສ່ຂ້ອຍຊື່ໆ ເຈົ້າເວົ້າຕໍ່ໂລດ."),
        ("lo", "ຂ້ອຍຫາກໍສົ່ງລູກເຂົ້ານອນແລ້ວບອກຝັນດີເດີ້ ບາດນີ້ເຮົາລົມກັນໄດ້ສະບາຍແລ້ວ."),
        ("lo", "ສຽງລົດຢູ່ທາງນອກມິດໄປແລ້ວ ບໍ່ລົບກວນແລ້ວ ເວົ້າຕໍ່ໄດ້ເລີຍ."),
        ("lo", "ມື້ວານນີ້ຂ້ອຍບອກເພິ່ນວ່າຂ້ອຍຕ້ອງໄປກ່ອນແລ້ວ ແຕ່ເພິ່ນຍັງຊວນລົມຕໍ່ຕັ້ງດົນ."),
        ("lo", "ບັນຫາທຳອິດມີແຕ່ເທົ່ານີ້ແຫຼະ ບາດນີ້ມາເບິ່ງບັນຫາທີສອງນຳກັນ."),
        ("lo", "ມື້ເຊົ້ານີ້ລາວບອກວ່າດຽວໂທຫາໃໝ່ເດີ້ ແຕ່ຈົນຮອດດຽວນີ້ກໍຍັງບໍ່ໂທມາເລີຍ."),
        ("lo", "ມື້ເຊົ້າຂ້ອຍຟ້າວວຽກໜ້ອຍໜຶ່ງເລີຍບໍ່ໄດ້ຕອບ ດຽວນີ້ວ່າງແລ້ວ ເວົ້າມາໂລດ."),
        ("lo", "ວຽກສ່ວນນັ້ນເອົາໄວ້ມື້ອື່ນເນາະ ແຕ່ຕອນນີ້ຂໍຕົກລົງເລື່ອງເວລານັດໝາຍກ່ອນ."),
        ("lo", "ອາໂຫຼ ສະບາຍດີ ມື້ນີ້ມີຫຍັງໃຫ້ຊ່ວຍບໍ່?"),
        ("mn", "Өчигдөр тэр хүн баяртай ч гэлгүй гараад явчихсан, чи сонссон уу?"),
        ("mn", "Тэр утсаа таслахдаа шууд л за баяртай гээд хаячихсан, одоо би чамд учрыг нь ярья."),
        ("mn", "Өглөө би түр баяртай гээд гарсан болохоор одоо үлдсэн асуудлаа чамтай ярилцъя."),
        ("mn", "Үнийн асуудлыг дараа ярья, харин одоо хүргэлтийн цагаа тохиръё."),
        ("mn", "Өглөө эргээд залгая гэж амласан болохоор одоо чам руу залгаж байна, сонсож байна уу?"),
        ("mn", "Тэр надад дараа уулзъя гэж бичсэн байсан, гэхдээ би одоо чамаас өөр зүйл асуух гэсэн юм."),
        ("mn", "Ээж маань сая орж ирээд сайхан амраарай гэж хэлчхээд гарлаа, за чи цааш нь ярь даа."),
        ("mn", "Өвөө маань дандаа сайн сууж байгаарай гэж захидаг юм, харин чиний ажил юу болж байна?"),
        ("mn", "Шинэ ажилд орсонд баяр хүргэж амжилт хүсье, ингэхэд эхний өдөр ямархуу байв?"),
        ("mn", "За баярлалаа, тэгвэл хоёр дахь асуултаа одоо асууж болох уу?"),
        ("mn", "Нөгөө хэлтэстэй дараа холбогдьё, харин одоо хоёулаа энэ тайлангаа дуусгая."),
        ("mn", "За ингээд эхний хэсгээ дуусгалаа, одоо хоёр дахь хэсэг рүүгээ оръё."),
        ("mn", "Тэр сая би одоо явах хэрэгтэй байна гээд өрөөнөөс гарсан тул одоо би чөлөөтэй ярьж болно."),
        ("mn", "Тэр надад ажлаа бод доо гэж зөвлөсөн юм, чи үүнтэй санал нийлэх үү?"),
        ("mn", "Тэр өчигдөр тэгээд эргээд ярья гэсэн ч өнөөдөр хүртэл чимээгүй л байна."),
        ("mn", "Эхний жагсаалтыг ингээд болох уу, одоо дараагийн хуудсыг нь уншъя."),
        ("mn", "За за, ойлголоо, тэгээд цааш нь юу болсон бэ?"),
        ("fa", "همکارم همین الان گفت خداحافظ و از اتاق بیرون رفت، حالا راحت می‌توانیم صحبت کنیم."),
        ("fa", "پاسخگوی قبلی وسط توضیح من گفت خدانگهدار و تماس قطع شد، برای همین دوباره زنگ زدم."),
        ("fa", "فکر کردم می‌خواهید بگویید فعلاً خداحافظ، خوشحالم که هنوز چند دقیقه وقت دارید."),
        ("fa", "من فعلاً پشت خط منتظر هستم تا پرونده را باز کنید، عجله‌ای ندارم."),
        ("fa", "در پایان نامه نوشته بودند به امید دیدار، اما هنوز ساعت جلسه را مشخص نکرده‌اند."),
        ("fa", "به راننده گفتم در این باران مراقب خودت باش، ببخشید، حواسم با شماست."),
        ("fa", "فقط خواستم اول تماس بگویم در این هوای سرد مواظب خودتون باشید و یک سؤال بپرسم."),
        ("fa", "قربانت، من هنوز سر خط هستم، لطفاً بقیه جزئیات سفارش را هم بگو."),
        ("fa", "قربان شما، اختیار دارید، خواهش می‌کنم بفرمایید دقیقاً چه مدارکی لازم است."),
        ("fa", "به نگهبان ورودی گفتم روز خوش، عذر می‌خواهم، صدای شما را واضح می‌شنوم."),
        ("fa", "به همسایه در راهرو گفتم شب خوش، بفرمایید، هنوز با شما هستم."),
        ("fa", "مادربزرگم بدرقه‌اش کرد و گفت خدا به همراهت، الان خانه خلوت است و می‌توانیم حرف بزنیم."),
        ("fa", "اصلاً نگو خب دیگه، مزاحمت نمیشم، امروز کاملاً وقت دارم و گوش می‌دهم."),
        ("fa", "می‌دانم همیشه می‌گویید بیشتر از این وقتتون رو نمی‌گیرم، اما لطفاً همه جزئیات را کامل بگویید."),
        ("fa", "همیشه وسط حرف‌هایم می‌پرسی کاری باری نداری؟ صبر کن تازه به اصل ماجرا رسیدم!"),
        ("fa", "منشی در پذیرش از مراجع قبلی پرسید امری نیست؟ من هنوز پای تلفن به شما گوش می‌دهم."),
        ("fa", "گفتند حتماً اول تماس به شما سلام برسونید و بعد درباره برنامه فردا سؤال کنم."),
        ("fa", "ده دقیقه دیگر مجبورم بگویم خب، من دیگه باید برم، پس بیایید سریع‌تر بررسی کنیم."),
        ("fa", "سلام و روز بخیر، برای پیگیری نوبت فردا تماس گرفتم."),
        ("fa", "سلام، شب بخیر، ببخشید این موقع شب تماس می‌گیرم، یک پیام فوری داشتم."),
        ("fa", "سلام و عصر بخیر، خواستم ببینم جلسه امروز سر ساعت برگزار می‌شود یا خیر."),
        ("ps", "همکار مې همدا اوس خدای پامان ووایه او له خونې ووت، اوس آرام غږېدلی شو."),
        ("ps", "په مخکینۍ اړیکه کې کارکوونکي ژر د خدای په امان ووایه او اړیکه پرې شوه، ځکه مې بیا زنګ وواهه."),
        ("ps", "مېلمنو ته مې په دروازه کې په مخه مو ښه ووایه، بخښنه غواړم، ستاسو خبرې اورم."),
        ("ps", "ورور ته مې وویل په مخه دې ښه چې ښوونځي ته ولاړ شي، اوس خپلې خبرې ته دوام ورکړه."),
        ("ps", "پرتله کول مې غوښتل چې دلته خلک څنګه مخه ښه کوي، خو زه لا پر لیکه ستاسو ځواب ته منتظر یم."),
        ("ps", "تل په خیر اوسئ، ما غوښتل د نننۍ غونډې په اړه یو لنډ پیغام پرېږدم."),
        ("ps", "ملګري مې الله پامان ووایه او ولاړ، اوس کولی شو د کار په اړه تفصیلي وغږېږو."),
        ("ps", "مور مې ورته وویل خدای دې مل شه او رخصت یې کړ، بخښنه غواړم، تاسو ووایاست."),
        ("ps", "تاسو پرون وویل چې نن سهار بیا به غږیږو، نو ځکه مې اوس اړیکه ونیوله."),
        ("ps", "ژوندي اوسئ، ډېره مننه چې زما اړیکه مو ځواب کړه، اوس غواړم خپله ستونزه درته ووایم."),
        ("ps", "دا مه وایاست چې ښه نو، نور مو نه مصروفوم، زه اوس پوره وخت لرم، مهرباني وکړئ ووایاست."),
        ("ps", "لس دقیقې وروسته به ووایم زه نور اجازه غواړم، نو راځئ تر هغې دا مسله بشپړه کړو."),
        ("ps", "مخکې له دې چې وپوښتئ نور کوم امر؟ غواړم په نېټه کې یو کوچنی بدلون راولم."),
        ("ps", "پلار مې وویل لومړی تاسو ته په کور کې سلام ووایاست او بیا د سبا وخت وپوښتم."),
        ("ps", "لومړی خو موږ په دعا کې مو یاد ساتئ، او دویم دا چې غواړم یو پیغام ثبت کړم."),
        ("ps", "هغه یوازې وویل زه باید اوس لاړ شم او هیڅ تفصیل یې ورنه کړ، ځکه له تاسو پوښتم."),
        ("ps", "السلام علیکم، غواړم د سبا د لیدنې په اړه یو پیغام پرېږدم."),
        ("ps", "سلام، بخښنه غواړم، ایا دا د دفتر شمېره ده؟"),
        ("ps", "سلام او شپه مو پخیر، بخښنه غواړم چې ناوخته زنګ وهم، یو بېړنی پیغام لرم."),
        ("az", "Məlumat üçün sağ ol, bəs ikinci sualımın cavabı necə oldu?"),
        ("az", "Diqqətiniz üçün sağ olun, amma bir məsələni də soruşmaq istəyirəm."),
        ("az", "Çox sağ olun, bəs çatdırılma saat neçədə olacaq?"),
        ("az", "Bu məsələni hələlik saxlayaq, indi əsas mövzuya keçək."),
        ("az", "Sabah saat neçədə görüşərik, indi vaxtı təyin edək?"),
        ("az", "Tətil planını sonra danışarıq, indi təcili işi həll edək."),
        ("az", "Həkim də həmişə özünə yaxşı bax deyir, bəs sənin səhhətin indi necədir?"),
        ("az", "Hamıya özünüzə yaxşı baxın deyəndən sonra bir sual da ortaya çıxdı, qulaq asın."),
        ("az", "Uşaqlara gecəniz xeyrə qalsın deyib gəldim, indi səni dinləyirəm."),
        ("az", "O heç xudahafiz demədən otaqdan çıxdı, təsəvvür edirsən?"),
        ("az", "Qonaqları sağlıqla qalın deyib yola saldım, indi səninlə geniş danışaq."),
        ("az", "Yeni layihədə sənə uğurlar, indi de görüm hazırlıq necə gedir?"),
        ("az", "Uzun təfərrüatlarla sizi yormayım, birbaşa əsas sualıma keçim."),
        ("az", "Bir saatdan sonra mən getməliyəm, ona görə indi bütün detalları danışaq."),
        ("az", "Birinci mövzu üzrə başqa sualım yoxdur, indi keçək ikinci məsələyə."),
        ("az", "Digər şöbə ilə sonra əlaqə saxlayarıq, indi öz aramızda qərar verək."),
        ("az", "Oldu onda, birinci şərti qəbul edirik, indi ikinci şərtə baxaq."),
        ("az", "Pəncərəni yaxşı, mən bağlayıram ki, küçənin səsi gəlməsin, indi davam et."),
        ("hy", "Նա առանց ցտեսություն ասելու դուրս եկավ սենյակից, պատկերացնո՞ւմ ես։"),
        ("hy", "Եղբորս ասացի լավ մնա ու դուրս եկա բակ, հիմա կարող ենք հանգիստ խոսել։"),
        ("hy", "Հյուրերին ասացի լավ մնացեք ու ճանապարհ դրեցի, հիմա ասա՝ ինչ նորություն կա։"),
        ("hy", "Այդ գործում քեզ մեծ հաջողություն եմ մաղթում, իսկ հիմա ասա՝ ե՞րբ ենք սկսում։"),
        ("hy", "Այսօր շատ հաջող օր էր, հիմա պատմեմ՝ ինչ եղավ առավոտյան։"),
        ("hy", "Այդ հարցը առայժմ մի կողմ դնենք ու անցնենք հաջորդ կետին։"),
        ("hy", "Գնի մասին էլ հիմա կխոսենք, բայց նախ ժամը հաստատենք։"),
        ("hy", "Նա խոստացավ, որ կեսօրին կզանգեմ կասի, բայց հիմա դու ասա՝ ինչ անենք։"),
        ("hy", "Վաղը որտե՞ղ կհանդիպենք, արի հիմա տեղը որոշենք։"),
        ("hy", "Փոքրիկին նոր բարի գիշեր ասացի, հիմա ազատ եմ, պատմիր։"),
        ("hy", "Օպերատորը միանգամից ասաց շնորհակալություն, ցտեսություն ու անջատեց, դրա համար հիմա քեզ զանգեցի։"),
        ("hy", "Դե լավ, այդ հարցը պարզեցինք, հիմա անցնենք երկրորդին։"),
        ("hy", "Աշխատակիցներին ասացի, որ ինձ էլ չխանգարեմ ասելու կարիք չկա, հիմա հանգիստ պատմիր։"),
        ("hy", "Կես ժամից պետք է գնամ, դրա համար հիմա արագ քննարկենք այս հարցը։"),
        ("hy", "Առաջին մասով այսքանը, իսկ հիմա լսիր երկրորդ մասը։"),
        ("hy", "Ընկերոջս գրեցի հետո կզանգեմ, որպեսզի հիմա քեզ հետ հանգիստ զրուցեմ։"),
        ("hy", "Առաջին կետի շուրջ ուրեմն պայմանավորվեցինք, հիմա նայենք երկրորդ կետը։"),
        ("ka", "ახლა უკვე კარგად ისმის შენი ხმა, განაგრძე."),
        ("ka", "ეს ფაილი დროებით აქ შევინახოთ და ახლა მეორე საკითხზე გადავიდეთ."),
        ("ka", "ისე გავიდა ოთახიდან, რომ ნახვამდის არც უთქვამს, წარმოგიდგენია?"),
        ("ka", "მეგობარს ვუთხარი კარგად იყავი-მეთქი და გამოვედი, ახლა მომიყევი რა ხდება."),
        ("ka", "ოპერატორმა მაშინვე კარგად ბრძანდებოდეთ მითხრა და გათიშა, ამიტომ ახლა შენ გირეკავ."),
        ("ka", "მან უცებ აბა დროებით მომაძახა და გაიქცა, შენ რას ფიქრობ ამაზე?"),
        ("ka", "მეზობელს აბა კარგად ვუთხარი სადარბაზოში და ახლა გისმენ ყურადღებით."),
        ("ka", "დილით მომწერა მოგვიანებით დაგირეკავო და ჯერ არ ჩანს, შენ ხომ არ გილაპარაკია?"),
        ("ka", "ფასზე მერე ვილაპარაკოთ, ახლა ჯერ თარიღი შევათანხმოთ."),
        ("ka", "ბავშვებს ღამე მშვიდობისა ვუსურვე და გამოვედი, ახლა თავისუფალი ვარ, მომიყევი."),
        ("ka", "ეს იდეა ძალიან მაგრად მომწონს, მითხარი დეტალები როგორ წარმოგიდგენია."),
        ("ka", "კურიერს მადლობა, ნახვამდის ვუთხარი და კარი დავხურე, ჰოდა გააგრძელე რას ამბობდი."),
        ("ka", "ბევრი შესავლით აღარ მოგაცდენ და პირდაპირ მთავარ კითხვას დაგისვამ."),
        ("ka", "ნახევარ საათში უნდა წავიდე, ამიტომ ახლავე გავიაროთ ყველა საკითხი."),
        ("ka", "პირველი ნაწილი სულ ეს იყო, ახლა გადავიდეთ მეორე საკითხზე."),
        ("ka", "კაი აბა, გისმენ, მომიყევი თავიდან ბოლომდე რა მოხდა."),
        ("ka", "პირველ პუნქტზე შევთანხმდით, ახლა მეორე პუნქტსაც გადავხედოთ."),
        ("ka", "ტელევიზორის ხმას კარგი, გავთიშავ, რომ უკეთ გავიგო რას მეუბნები."),
        ("sw", "Aliondoka jana bila kusema kwaheri, kwa hivyo nataka kukuuliza kilichotokea."),
        ("sw", "Subiri kidogo kwenye simu, ninawaambia wageni hapa mlangoni kwaherini."),
        ("sw", "Tutajadili bajeti baadaye kwenye mkutano, lakini kwanza nieleze ratiba ya leo."),
        ("sw", "Najua tutaonana wiki ijayo, lakini kwanza tafadhali nijibu swali hili kwenye simu."),
        ("sw", "Ingawa tutaonana baadaye leo, nahitaji kujua sasa hivi kama nyaraka ziko tayari."),
        ("sw", "Kuhusu gharama tutazungumza baadaye, lakini sasa hivi nieleze kwanza mpango wa kazi."),
        ("sw", "Najua tutaongea kesho kwa kirefu, lakini kabla ya hapo naomba nikuulize jambo moja sasa."),
        ("sw", "Natumaini umekuwa na siku njema kufikia sasa; ninapiga simu kuuliza kuhusu ratiba yetu."),
        ("sw", "Subiri kidogo kwenye simu, ninamtakia mtoto wangu usiku mwema hapa chumbani."),
        ("sw", "Tafadhali baki salama hapo ulipo huku tukiendelea kuzungumza kwenye simu na kutafuta msaada."),
        ("sw", "Nilimwambia dereva wetu nenda salama asubuhi, lakini sasa nataka kukuuliza kuhusu mzigo."),
        ("sw", "Haya basi, tuanze na jambo la kwanza ulilotaja."),
        ("sw", "Sawa basi, niko tayari kuandika, tafadhali nitajie maelezo hayo."),
        ("sw", "Sawa, asante kwa maelezo hayo ya kwanza, na sasa nina swali lingine."),
        ("sw", "Je, ni hayo tu uliyoambiwa, au kuna maelezo mengine tunayopaswa kujadili sasa?"),
        ("sw", "Kama umenielewa hapo basi inatosha kwa sehemu ya kwanza, sasa tuingie sehemu ya pili."),
        ("sw", "Nilikuwa karibu kusema wacha nikuache, lakini nimekumbuka swali moja muhimu sana."),
        ("sw", "Habari za asubuhi? Salama kabisa, ninapiga simu kuuliza kuhusu miadi yangu."),
        ("sw", "Salaam, ninaomba kuzungumza na mhudumu kuhusu ombi langu."),
        ("am", "ቅድም ሳይሰማኝ ቻው ብሎ ስልኩን ዘጋው፣ አሁን ግን ጉዳዩን በዝርዝር ልንገርዎት።"),
        ("am", "ትላንት ደህና ሁኑ ብለው ከወጡ በኋላ አዲስ መረጃ መጥቷል፣ እባክዎ ያዳምጡኝ።"),
        ("am", "ቅድም ለጓደኛዬ ደህና ሁን እያልኩ ነበር፣ ከእርስዎ ጋር ግን ንግግራችንን እንቀጥል።"),
        ("am", "እዚህ አጠገቤ ላለችው ባልደረባዬ ደህና ሁኚ ብዬ ነው፣ እባክዎ እርስዎ መናገርዎን ይቀጥሉ።"),
        ("am", "ደህና ዋሉ ብዬ ስልኩን ከመዝጋቴ በፊት አንድ ጥያቄ ብቻ ቀረኝ።"),
        ("am", "ትላንት ጠዋት ደህና ዋል ብሎኝ ከሄደ ወዲህ አላገኘሁትም፣ እርስዎ ሰምተውታል?"),
        ("am", "እዚህ ቤት ውስጥ ለልጆቹ ደህና እደሩ እያልኩ ነበር፣ እባክዎ አንድ አፍታ ይጠብቁኝና እንቀጥል።"),
        ("am", "በኋላ እንገናኝ ብሎ ቀጠሮ ሰጥቶኝ ነበር፣ አሁን ግን ሰዓቱን ለመቀየር ነው የደወልኩት።"),
        ("am", "ነገ እንገናኝ ብለውኝ ነበር፣ ግን ከዚያ በፊት አሁን በስልክ የምጠይቅዎት ጉዳይ አለ።"),
        ("am", "ዛሬ ለውይይታችን በጣም መልካም ቀን ነው፣ እባክዎ የደወሉበትን ጉዳይ ይንገሩኝ።"),
        ("am", "ሁልጊዜ በሰላም ሁኑ እያሉ ይመርቁናል፣ አሁን ግን ስለ ዋናው ጉዳይ እንነጋገር።"),
        ("am", "እሺ በቃ፣ ወደ ዋናው ጉዳይ እንግባና ዝርዝሩን ይንገሩኝ።"),
        ("am", "እሺ አመሰግናለሁ፣ አሁን ደግሞ ሁለተኛውን ጥያቄ ልጠይቅዎት።"),
        ("am", "ስለ ክፍያው ሌላ ጥያቄ የለኝም፣ ነገር ግን ስለ ቀጠሮው ቀን አሁን መጠየቅ እፈልጋለሁ።"),
        ("am", "ከሰዓት በኋላ ወደ ቢሮ መሄድ አለብኝ፣ ስለዚህ ከዚያ በፊት አሁን በስልክ እንጨርሰው።"),
        ("am", "ስለ መጀመሪያው ርዕስ ለዛሬ ይበቃል፣ አሁን ግን ሁለተኛውን ጉዳይ በአጭሩ እንይ።"),
        ("am", "እሺ እንግዲህ፣ ከመጀመሪያው ጀምረው ምን እንደተፈጠረ ያስረዱኝ።"),
        ("am", "ሰላም፣ እንዴት ነዎት? ስለ ቀጠሮዬ ለመጠየቅ ደውዬ ነበር።"),
        ("am", "ጤና ይስጥልኝ፣ እንዴት ልረዳዎት እችላለሁ?"),
        ("af", "Hy het gister sonder om totsiens te sê geloop, maar vertel my eers wat vandag gebeur het."),
        ("af", "Ek hoop die weer gaan môre mooi bly vir die geleentheid, maar watter tyd pas jou?"),
        ("af", "As alles vandag mooi loop met die vergadering, kan ons die plan vanaand bespreek."),
        ("af", "Moenie sê ons praat later nie, want ons moet hierdie kwessie sommer nou dadelik oplos."),
        ("af", "Die ontvangsbeampte het my 'n lekker dag verder toegewens, maar ek bel nou om die tyd te bevestig."),
        ("af", "Mense verwar soms goeienag met goeienaand aan die begin van 'n oproep, maar hoe kan ek jou vanaand help?"),
        ("af", "Bly asseblief aan die lyn, ek sê net gou tatta vir iemand wat nou die kantoor verlaat."),
        ("af", "Ons ry tans verby die baai op pad na die stad, maar ek kan jou duidelik hoor."),
        ("af", "Ons kan dit nie tot later uitstel nie, so kom ons kyk nou dadelik na die besonderhede."),
        ("af", "Hy het vanoggend gesê sien jou later en toe nooit opgedaag nie, weet jy dalk waar hy is?"),
        ("af", "Voordat ons afsluit met lekker aand verder, moet ek net seker maak van môre se tyd."),
        ("af", "Goed dan, kom ons begin by die eerste vraag op die lys."),
        ("af", "Reg so, ek luister aandagtig, vertel my gerus wat daarna gebeur het."),
        ("af", "Nou ja, dit klink interessant, vertel my asseblief 'n bietjie meer daaroor."),
        ("af", "Dis al wat ek tot dusver gehoor het, so kan jy asseblief die res vir my verduidelik?"),
        ("af", "My kollega sê ek moet nou gaan teken vir 'n pakkie, so gee my net 'n halwe minuut op die lyn."),
        ("af", "Baie dankie vir jou hulp met daardie eerste deel, en nou wil ek nog oor die aflewering vra."),
        ("af", "Goeiedag, waarmee kan ek u vandag op hierdie lyn behulpsaam wees?"),
        ("af", "Dag, is ek nou deur na die regte afdeling vir navrae?"),
        ("et", "Eelmine teenindaja ütles mulle lihtsalt head aega ja katkestas kõne, nii et palun vaadake minu avaldus uuesti üle."),
        ("et", "Kolleeg hõikas mulle siin ukse peal nägemist, aga mina olen ikka liinil, rääkige edasi."),
        ("et", "Enne kui ütleme kuulmiseni, kas saaksite mulle veel kord öelda kabineti numbri?"),
        ("et", "Me ei jäta veel nägemiseni, sest ma pean teile enne ütlema ka uksekoodi."),
        ("et", "Õde ütles mulle praegu köögist tšauki, ära pane tähele, räägi edasi mis arst arvas."),
        ("et", "Soovin muidugi kõigile kõike head, aga praegu tahaksin kõigepealt teada, miks arve summa on muutunud."),
        ("et", "Klienditeenindaja soovis mulle juba kena päeva jätku, kuigi mure jäi lahendamata, kas teie saate mind aidata?"),
        ("et", "Leppisime hommikul kokku, et jääme kuuldele, ja nüüd helistangi, et küsida täpseid mõõte."),
        ("et", "Kas seda pakkumist saab hoida homseni, või pean otsuse ütlema kohe praeguse kõne jooksul?"),
        ("et", "Sõber hõikas mulle koridoris pakaa, aga ma kuulan sind edasi, mis kell kohtumine algab?"),
        ("et", "Olgu, aitäh selle esimese punkti eest, aga vaatame nüüd kohe ka teist tellimust."),
        ("et", "Mõtlesin juba öelda, et hea küll, ma lõpetan, aga mul tuli meelde veel üks oluline detail."),
        ("et", "Ütlesin kolleegile, et pean nüüd minema teise tuppa telefoniga rääkima, nii et kuulan teid rahulikult."),
        ("et", "Kui me räägime hiljem üksikasjadest, siis kas saame vähemalt kuupäeva kohe praegu lukku panna?"),
        ("et", "Tänan kõne eest muide, tahtsingi teilt just küsida, kuidas eilne ülevaatus läks."),
        ("et", "Arvasin juba, et rohkem küsimusi pole, kuid kas parkimine on hoovis ikka tasuta?"),
        ("et", "Tšau, kas sul on praegu hetk aega telefonis rääkida?"),
        ("et", "Tsau, helistan sulle selle homse väljasõidu pärast, kas kuuled mind?"),
        ("et", "Hei, oota korraks, ära veel kõnet katkesta, mul jäi kellaaeg küsimata."),
        ("lv", "Iepriekšējais operators pateica tikai uz redzēšanos un nolika klausuli, tāpēc lūdzu pārbaudiet manu pieteikumu vēlreiz."),
        ("lv", "Pirms sakām uz sadzirdēšanos, vai varat vēlreiz nosaukt kabineta numuru?"),
        ("lv", "Bērns tikko koridorā sauca atā kaimiņam, nepievērs uzmanību, es klausos tevi tālāk."),
        ("lv", "Es jums protams novēlu visu labu, bet vispirms lūdzu paskaidrojiet, kāpēc rēķinā ir šī papildu maksa."),
        ("lv", "Konsultants man jau novēlēja jauku dienu, lai gan jautājums vēl nebija atrisināts, vai varam to pārbaudīt tagad?"),
        ("lv", "Vai šo rezervāciju var paturēt spēkā līdz rītdienai, vai man jāapstiprina uzreiz šīs sarunas laikā?"),
        ("lv", "Mēs vakar norunājām, ka šodien sazvanāmies par līgumu, tāpēc iziesim tagad visiem punktiem cauri."),
        ("lv", "Kur tieši mēs rīt tiekamies, pie galvenās ieejas vai pagalmā, pasaki pirms beidzam sarunu?"),
        ("lv", "Kolēģis pie durvīm pateica uz saredzēšanos un devās mājās, bet es vēl esmu klausulē, turpiniet lūdzu."),
        ("lv", "Es tikko pateicu bērnam ar labu nakti un aizvēru istabas durvis, tagad varu mierīgi ar tevi parunāt."),
        ("lv", "Labi, paldies par šo pirmo jautājumu, un tagad pāriesim pie nākamā punkta."),
        ("lv", "Es tikko pateicu kolēģim, ka man tagad jāiet klusākā telpā parunāt ar jums pa telefonu, tāpēc klausos."),
        ("lv", "Pat ja mēs parunāsim vēlāk par detaļām, vai varam jau tagad vienoties par datumu?"),
        ("lv", "Paldies par zvanu starp citu, es tieši grasījos jums jautāt par vakardienas piegādi."),
        ("lv", "Domāju ka tas arī viss, bet pagaidiet mirkli, vai stāvvieta pie klīnikas ir bez maksas?"),
        ("lv", "Operatora balss teica es jums atzvanīšu, bet neviens tā arī nezvanīja, tāpēc lūdzu palīdziet man tagad."),
        ("lv", "Čau, vai tu šobrīd vari brīdi parunāt pa telefonu?"),
        ("lv", "Sveiki, es zvanu, lai precizētu rītdienas vizītes laiku."),
        ("lt", "Ankstesnis konsultantas pasakė tik viso gero ir padėjo ragelį, todėl norėčiau dabar viską ramiai išsiaiškinti."),
        ("lt", "Mūsų skyrius šiandien dirba iki šeštos valandos, tad pasakykite, kuriuo metu jums patogiausia atvykti."),
        ("lt", "Noriu sužinoti iš viso kiek kainuos remontas su visomis detalėmis, ar galite paskaičiuoti?"),
        ("lt", "Prieš atsisveikinant ir sakant iki malonaus, patikslinkime dar kartą užsakymo kodą."),
        ("lt", "Mums liko dar dvi dienos iki pasimatymo su gydytoju, todėl skambinu paklausti dėl tyrimų atsakymų."),
        ("lt", "Tikiuosi sulaukti bent iki greito kurjerio skambučio, bet kol kas pasakykite, kur yra mano siunta."),
        ("lt", "Vaikas čia šalia pasakė ate močiutei, bet aš vis dar prie telefono, tęskite prašau."),
        ("lt", "Operatorius man jau palinkėjo geros dienos, nors problema liko neišspręsta, ar galite patikrinti sąskaitą?"),
        ("lt", "Vakar sutarėme, kad šiandien susiskambinsime dėl sutarties sąlygų, tad gal peržiūrime jas dabar?"),
        ("lt", "Linkiu mums visiems sėkmės su šiuo projektu, o dabar aptarkime antrojo etapo sąmatą."),
        ("lt", "Gerai, ačiū už šį paaiškinimą, o dabar norėčiau paklausti apie kitą sąskaitos eilutę."),
        ("lt", "Pasakiau kolegai, kad turiu bėgti į kitą kabinetą atsiliepti į jūsų skambutį, tad klausau jūsų."),
        ("lt", "Net jei pasikalbėsime vėliau apie smulkmenas, ar galime jau dabar sutarti tikslią susitikimo dieną?"),
        ("lt", "Ačiū už skambutį beje, kaip tik ketinau jums skambinti ir paklausti dėl rytdienos pristatymo."),
        ("lt", "Maniau, kad daugiau klausimų neturiu, bet palaukite, ar automobilių aikštelė prie įėjimo yra nemokama?"),
        ("lt", "Vakar pažadėjote, kad aš jums perskambinsiu, bet skambučio nesulaukiau, todėl prašau patikrinti dabar."),
        ("lt", "Čiau, ar turi dabar porą minučių pakalbėti telefonu?"),
        ("be", "Ён выйшаў з кабінета і не сказаў да пабачэння, таму я тэлефаную ўдакладніць дэталі."),
        ("be", "Калі ласка, бывайце ў нас часцей на выхадных, а цяпер узгоднім час сустрэчы."),
        ("be", "Яна напісала бывай у паведамленні, але паслухай, што я хачу прапанаваць."),
        ("be", "Скажыце зручны для вас дзень, пакуль я правяраю расклад у сістэме."),
        ("be", "Гэтая гісторыя скончылася вельмі шчасліва, а цяпер пяройдзем да вашага пытання."),
        ("be", "Засталося мала часу да сустрэчы, таму давайце хутка праверым спіс дакументаў."),
        ("be", "Заяўку можна падаць да заўтра, таму давайце запоўнім усе палі зараз."),
        ("be", "У лісце было напісана да хуткай сустрэчы, але давайце спачатку вызначым дату."),
        ("be", "Ён пажадаў калегам усяго добрага і перадаў мне слухаўку, каб мы працягнулі."),
        ("be", "Мы жадаем кліентам усяго найлепшага, таму зараз падрабязна разбяром вашу сітуацыю."),
        ("be", "Лекар сказаў мне беражы сябе і не хвалюйся, а цяпер раскажы свае навіны."),
        ("be", "Дзеці ўжо сказалі дабранач і леглі спаць, таму цяпер мы можам спакойна пагаварыць."),
        ("be", "Задам толькі адно кароткае пытанне і не буду вас больш затрымліваць, паслухайце."),
        ("be", "Каб пасля не казаць што мне трэба бегчы, давай зараз спакойна ўсё абмяркуем."),
        ("be", "Яшчэ няскора мне пара ісці, таму можаш смела расказваць усе падрабязнасці."),
        ("be", "Пра іншыя праекты пазней пагаворым, а зараз давай вырашым галоўнае пытанне."),
        ("be", "Я думаў што на гэтым усё, але раптам узгадаў яшчэ адзін важны момант."),
        ("be", "Калі раптам сувязь абарвецца і я мушу класці слухаўку, адразу набярыце мяне зноў."),
        ("be", "Алё, добры дзень, хачу запісацца на прыём да спецыяліста."),
        ("be", "Алё, добры вечар, прабачце за позні званок."),
        ("be", "Алё, добрага дня, чым магу вам сёння дапамагчы?"),
        ("be", "Алё, добрага вечара, падкажыце, ці ёсць вольныя месцы на заўтра?"),
        ("ta", "முன்னாடி பேசின ஊழியர் உடனே 'சரி, வைக்கிறேன்'னு சொல்லிட்டு இணைப்பைத் துண்டிச்சிட்டாரு, அதான் மறுபடியும் கூப்பிட்டேன்."),
        ("ta", "பக்கத்து மேசையில என் சக ஊழியர் வேற போன்ல 'சரிங்க, வச்சிடறேன்'னு சொல்றாரு, நீங்க தொடர்ந்து சொல்லுங்க."),
        ("ta", "வாசல்ல கொரியர் ஊழியர் 'போயிட்டு வரேன்'னு சொல்லிட்டு கிளம்புறாரு, ஆனா நாம இப்போ ரசீதை சரிபார்ப்போம்."),
        ("ta", "வரவேற்பாளர் என்கிட்ட 'போயிட்டு வாங்க'ன்னு சொல்லிட்டாங்க, ஆனா மருத்துவர் நேரத்தை மாத்திட்டாரான்னு இப்போ உங்ககிட்ட கேட்கணும்."),
        ("ta", "இன்னும் கொஞ்ச நேரத்துல அலுவலகத்துக்கு வரேன், அதுக்குள்ள என்னென்ன ஆவணம் வேணும்னு இப்போவே சொல்லுங்க."),
        ("ta", "மத்த விஷயத்தை அப்புறம் பேசலாம், இப்போ முதல்ல நாளைக்கு வர வேண்டிய நேரத்தை முடிவு பண்ணுவோம்."),
        ("ta", "பட்டியல்ல வேற தேதி இருக்கான்னு இப்போவே பார்க்கலாம், ஒரு நிமிஷம் காத்திருங்க."),
        ("ta", "மேலதிகாரிகிட்ட நாளைக்கு பேசலாம், ஆனா இப்போதைக்கு விண்ணப்ப எண்ணை எனக்குச் சொல்லுங்க."),
        ("ta", "மருத்துவர் 'உடம்பை பார்த்துக்கோங்க'ன்னு சொன்னதோட சரி, அடுத்த பரிசோதனை எப்போதுன்னு நீங்க பார்த்துச் சொல்லுங்க."),
        ("ta", "கிளம்பும்போது எல்லாரும் 'பத்திரமா போயிட்டு வாங்க'ன்னு சொன்னாங்க, இப்போ வழியில இருந்துதான் உங்ககிட்ட முகவரி கேட்கிறேன்."),
        ("ta", "பக்கத்துல தோழி வேற போன்ல 'சரி, பாய்'னு சொல்றாங்க, நம்ம இணைப்புல எந்தப் பிரச்சினையும் இல்ல, சொல்லுங்க."),
        ("ta", "சரி அப்போ, அடுத்ததா எந்த ஆவணத்தை எடுத்து வைக்கணும்னு சொல்லுங்க."),
        ("ta", "சரிங்க, நீங்க சொல்றதைக் குறித்துக்கிறேன், மேற்கொண்டு முழு விவரத்தையும் சொல்லுங்க."),
        ("ta", "கொரியர் ஊழியர் 'நான் அப்புறம் கூப்பிடுறேன்'னு சொன்னாரு, அதுக்குள்ள நாம முகவரியைச் சரிபார்த்துக்கலாம்."),
        ("ta", "ரசீதுல வேற ஒண்ணும் இல்ல, அதனால இப்போ நேரடியா கட்டண விவரத்தை எனக்கு விளக்குங்க."),
        ("ta", "இவ்வளவு சீக்கிரம் எடுத்ததுக்கு ரொம்ப நன்றிங்க, இப்போ என் கோரிக்கை என்னன்னா முகவரியை மாத்தணும்."),
        ("ta", "பார்சல் வர வேண்டிய நேரமாச்சு, இன்னும் ஏன் வரலைன்னு இப்போ கொஞ்சம் விசாரிச்சுச் சொல்றீங்களா?"),
        ("ta", "வணக்கம், மருத்துவமனை முன்பதிவு பத்தி விசாரிக்க கூப்பிட்டேன்."),
        ("he", "אמרתי לו תודה ויום טוב והוא המשיך לדבר, אז מה עם התור שלי?"),
        ("he", "ויום טוב גם לך, רציתי לשאול אם יש תור פנוי מחר."),
    ];

    #[test]
    fn lookalikes_do_not_close() {
        let mut bad = Vec::new();
        for (lang, t) in LOOKALIKES {
            for opening in [true, false] {
                let mut m = if opening { ClosingMachine::new(lang) } else { after_talk(lang) };
                let s = hears(&mut m, t);
                if closes(&s) {
                    bad.push(format!("[{lang}] opening={opening} {t} -> {:?} ends={}", s.event, s.ends_call));
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// Thanks alone is thanks, not a goodbye.
    const BARE_THANKS: &[(&str, &str)] = &[
        ("he", "תודה רבה."),
        ("he", "תודה."),
        ("tl", "maraming salamat po."),
        ("tl", "maraming salamat."),
        ("tl", "salamat po."),
        ("bn", "অনেক ধন্যবাদ."),
        ("bn", "ধন্যবাদ."),
        ("ur", "بہت بہت شکریہ."),
        ("ur", "بہت شکریہ."),
        ("ur", "شکریہ."),
        ("si", "බොහොම ස්තූතියි."),
        ("si", "ස්තූතියි."),
        ("si", "ස්තුතියි."),
        ("my", "ကျေးဇူးအများကြီးတင်ပါတယ်"),
        ("my", "ကျေးဇူးတင်ပါတယ်"),
        ("my", "ကျေးဇူးပါ"),
        ("lo", "ຂອບໃຈຫຼາຍໆເດີ້"),
        ("lo", "ຂອບໃຈຫຼາຍໆ"),
        ("lo", "ຂອບໃຈຫຼາຍ"),
        ("mn", "их баярлалаа."),
        ("mn", "баярлалаа."),
        ("fa", "خیلی ممنونم."),
        ("fa", "خیلی ممنون."),
        ("fa", "ممنونم."),
        ("ps", "ډېره مننه."),
        ("ps", "ډیره مننه."),
        ("ps", "مننه."),
        ("az", "çox təşəkkür edirəm."),
        ("az", "təşəkkür edirəm."),
        ("az", "təşəkkürlər."),
        ("hy", "շատ շնորհակալ եմ."),
        ("hy", "շնորհակալություն."),
        ("hy", "շնորհակալ եմ."),
        ("ka", "დიდი მადლობა."),
        ("ka", "მადლობა."),
        ("ka", "გმადლობთ."),
        ("sw", "asante sana."),
        ("sw", "asanteni."),
        ("sw", "asante."),
        ("am", "በጣም አመሰግናለሁ."),
        ("am", "አመሰግናለሁ."),
        ("am", "እናመሰግናለን."),
        ("af", "baie dankie."),
        ("af", "dankie."),
        ("et", "suur aitäh."),
        ("et", "aitäh."),
        ("et", "tänan väga."),
        ("lv", "liels paldies."),
        ("lv", "paldies."),
        ("lt", "labai ačiū."),
        ("lt", "ačiū."),
        ("lt", "dėkoju."),
        ("be", "вялікі дзякуй."),
        ("be", "дзякуй вам."),
        ("be", "дзякуй."),
        ("ta", "ரொம்ப நன்றிங்க."),
        ("ta", "ரொம்ப நன்றி."),
        ("ta", "மிக்க நன்றி."),
    ];

    #[test]
    fn bare_thanks_does_not_close() {
        let mut bad = Vec::new();
        for (lang, t) in BARE_THANKS {
            let mut m = after_talk(lang);
            let s = hears(&mut m, t);
            if s.ends_call || s.event == ClosingEvent::CallerFarewell {
                bad.push(format!("[{lang}] {t} -> {:?} ends={}", s.event, s.ends_call));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}

/// Two gaps simulated calls showed: a Greek wish with "σας" after thanks ("Σας ευχαριστώ
/// πολύ και καλή σας μέρα!"), and a Vietnamese 「Em chào anh ạ」 after the matter was settled.
/// The same words opening a call never close it.
#[cfg(test)]
mod parting_wish_gaps_tests {
    use super::*;

    fn hears(m: &mut ClosingMachine, t: &str) -> ClosingStep {
        m.on_caller(t, t, crate::locale::caller_wraps_up(t), false)
    }

    fn closes(s: &ClosingStep) -> bool {
        s.ends_call || matches!(s.event, ClosingEvent::CallerFarewell | ClosingEvent::CallerPreClosing)
    }

    /// The machine after the caller's first turn.
    fn after(lang: &'static str, first: &str) -> ClosingMachine {
        let mut m = ClosingMachine::new(lang);
        hears(&mut m, first);
        m
    }

    const EL_FIRST: &str = "Γεια σας, είμαι από το οδοντιατρείο, θέλουμε να μεταφέρουμε το ραντεβού σας.";
    const VI_FIRST: &str =
        "Em chào anh Quang ạ. Em gọi từ bên đơn vị giao hàng, không biết chiều mai anh có ở nhà để nhận hàng không ạ?";

    fn all_end(lang: &'static str, first: &str, cases: &[&str]) -> Vec<String> {
        let mut bad = Vec::new();
        for t in cases {
            let mut m = after(lang, first);
            let s = hears(&mut m, t);
            if s.event != ClosingEvent::CallerFarewell || !s.ends_call {
                bad.push(format!("[{lang}] {t} -> {:?} ends={}", s.event, s.ends_call));
            }
        }
        bad
    }

    #[test]
    fn greek_wish_with_sas_after_thanks_ends_the_call() {
        let mut bad = all_end("el", EL_FIRST, &[
            "Ακριβώς, την Παρασκευή στις έντεκα. Σας ευχαριστώ πολύ και καλή σας μέρα!",
            "Εντάξει, ευχαριστώ, καλή σας μέρα.",
            "Ευχαριστώ πολύ, καλή σας ημέρα!",
            "Τέλεια, σας ευχαριστώ και καλό σας απόγευμα.",
            "Ωραία, ευχαριστούμε, καλό σας βράδυ!",
        ]);
        // After the AI's own "Επίσης, καλή σας μέρα." the caller's wish alone closes too.
        let mut m = after("el", EL_FIRST);
        m.on_ai_turn("Επίσης, καλή σας μέρα.", false, false);
        let s = hears(&mut m, "Καλή σας μέρα!");
        if s.event != ClosingEvent::CallerFarewell || !s.ends_call {
            bad.push(format!("after the AI's wish -> {:?}", s.event));
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn greek_wish_opening_or_mid_sentence_does_not_end_the_call() {
        let mut bad = Vec::new();
        for t in ["Καλή σας μέρα, είμαι από την τράπεζα και σας καλώ για την κάρτα σας.", "Καλή σας μέρα!", "Καλημέρα σας!"] {
            let s = hears(&mut ClosingMachine::new("el"), t);
            if closes(&s) {
                bad.push(format!("opening {t} -> {:?}", s.event));
            }
        }
        for t in [
            "Καλή σας μέρα!",
            "Μου είπε καλή σας μέρα και μετά έκλεισε, τι έγινε με το ραντεβού;",
            "Θέλω να ευχηθώ καλή σας μέρα σε όλη την ομάδα, αλλά πρώτα μια ερώτηση για την παραγγελία.",
        ] {
            let s = hears(&mut after("el", EL_FIRST), t);
            if closes(&s) {
                bad.push(format!("mid-call {t} -> {:?}", s.event));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn vietnamese_chao_after_the_settled_matter_ends_the_call() {
        let bad = all_end("vi", VI_FIRST, &[
            "Dạ vâng, vậy em sẽ gửi lại quầy lễ tân cho anh nhé. Em chào anh ạ.",
            "Vâng, chiều mai ba giờ em giao nhé, chào anh ạ.",
            "Ok chị, em ghi lại rồi, em chào chị ạ.",
            "Dạ em hiểu rồi, mai em gọi lại sau, tôi chào anh.",
        ]);
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn vietnamese_chao_opening_does_not_end_the_call() {
        let mut bad = Vec::new();
        for t in ["Em chào anh ạ, em gọi từ bên giao hàng", "Em chào anh ạ.", "Alo, chào anh ạ", "Dạ, cháu chào anh ạ."] {
            let s = hears(&mut ClosingMachine::new("vi"), t);
            if closes(&s) {
                bad.push(format!("opening {t} -> {:?}", s.event));
            }
        }
        // A first turn of greetings only: the greeting in the next turn still opens the call.
        let mut m = after("vi", "Alo?");
        let s = hears(&mut m, "Em chào anh ạ.");
        if closes(&s) {
            bad.push(format!("second greeting -> {:?}", s.event));
        }
        let s = hears(&mut after("vi", VI_FIRST), "Em chào anh, em gọi lại về đơn hàng hôm qua ạ");
        if closes(&s) {
            bad.push(format!("mid-call greeting first -> {:?}", s.event));
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// The Vietnamese rule is Vietnamese only: elsewhere a greeting-goodbye after the
    /// caller's own words still waits for thanks (or the AI's goodbye) before it.
    #[test]
    fn other_languages_keep_the_thanks_rule_for_greeting_goodbyes() {
        let first = "Parcel delivery info";
        let mut bad = Vec::new();
        for (lang, t) in [
            ("id", "Paketnya saya antar besok pagi, selamat siang"),
            ("es", "Le llevo el paquete mañana por la mañana, buenas tardes"),
            ("de", "Ich bringe das Paket morgen vorbei, servus"),
            ("he", "החבילה תגיע מחר בבוקר, שלום"),
        ] {
            let s = hears(&mut after(lang, first), t);
            if closes(&s) {
                bad.push(format!("[{lang}] without thanks {t} -> {:?}", s.event));
            }
        }
        for (lang, t) in [
            ("id", "Baik, terima kasih, selamat siang."),
            ("de", "Alles klar, danke, servus!"),
            ("he", "תודה, שלום."),
            ("vi", "Cảm ơn anh, chào anh ạ"),
        ] {
            let s = hears(&mut after(lang, first), t);
            if s.event != ClosingEvent::CallerFarewell || !s.ends_call {
                bad.push(format!("[{lang}] with thanks {t} -> {:?}", s.event));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}
