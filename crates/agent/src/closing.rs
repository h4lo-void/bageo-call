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
        (self.substance || !greeting_only(&h.before))
            && (self.state != ClosingState::Talking || cue_before(&h.before))
    }
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
    ]),
    ("zh-TW", &[
        "那就先這樣", "就先這樣", "先這樣", "先這樣子", "不耽誤你時間了", "不耽誤您時間了", "不耽誤你了",
        "不耽誤您了", "有問題再聯絡", "有問題再聯繫", "不打擾了", "不打擾您了", "不打擾你了",
        "再聯絡", "再聯繫", "再講", "再聊", "改天再聊", "晚點再說", "之後再說", "回頭再說",
        "再打給你", "再打給您", "再回電給你", "再回電給您", "再回你電話", "再回您電話", "再回你", "再回您",
        "簡訊回你", "簡訊回您",
    ]),
    ("zh-CN", &[
        "那就先这样", "就先这样", "先这样", "回头微信说", "回头微信聊", "不说了", "不耽误你时间了",
        "不耽误您时间了", "有问题再联系", "不打扰了", "不打扰您了", "再联系", "再联络", "再聊", "回头再说",
        "再打给你", "再打给您", "再回电给你", "再回你电话", "再回你", "再回您", "短信回你", "短信回您",
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
    ]),
    ("es", &[
        "bueno te dejo", "te dejo", "le dejo", "nada era solo eso", "era solo eso", "hablamos luego",
        "hablamos después", "hablamos despues",
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
    ("pt", &["então tá bom", "entao ta bom", "combinado então", "combinado entao", "a gente se fala"]),
    ("th", &["งั้นแค่นี้", "งั้นแค่นี้นะ", "แค่นี้นะ", "แค่นี้ก่อน", "แค่นี้", "เอาไว้คุยกันใหม่", "คุยกันแค่นี้ก่อน"]),
    ("vi", &["dạ vậy nhé", "vậy nhé", "vay nhe", "thế nhé", "the nhe", "thôi nhé"]),
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
    ("hi", &["ठीक है अब रखता हूँ", "बाद में बात करते हैं", "मैं बाद में कॉल करूँगा", "बस इतना ही", "अच्छा चलता हूँ"]),
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
];

/// Goodbyes said only at the end.
const FAREWELLS: Lexicon = &[
    ("en", &[
        "bye", "bye bye", "byebye", "goodbye", "good bye", "talk to you soon", "talk soon", "take care",
        "have a great rest of your day", "have a good rest of your day", "enjoy the rest of your day",
        "cheers mate", "lovely ta", "ta ta", "see you", "see ya", "catch you later",
    ]),
    ("zh-TW", &[
        "掰掰", "掰", "拜拜", "再見", "再會", "掰囉", "掰掰囉", "掛囉", "掛啦", "先掛了", "我掛了", "我先掛了", "那我掛了",
        "那掛了",
    ]),
    ("zh-CN", &["再见", "再会", "挂了啊", "先挂了", "我挂了", "那挂了", "好嘞拜拜", "挂啰", "挂咯", "挂啦", "那我挂了"]),
    ("yue", &["得啦唔該晒", "得啦唔该晒", "得啦唔該曬", "先掛啦", "拜拜啦", "byebye", "收線啦", "收线啦", "得閒再傾", "得闲再倾", "遲啲再傾", "迟啲再倾"]),
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
    ]),
    ("de", &[
        "auf wiederhören", "auf wiederhoren", "wiederhören", "schönen tag noch", "schönen tag", "tschüss",
        "tschüs", "tschuss", "baba", "pfiat di", "pfiat eich", "gute nacht", "schönen abend noch", "schonen abend noch",
        "machs gut", "machen sie es gut",
    ]),
    ("pt", &[
        "tchau tchau", "tchau", "até logo", "ate logo", "até mais", "um abraço", "um abraco", "abraço", "abraco", "beijos",
        "beijo", "fica com deus", "vai com deus", "fique bem", "bom trabalho",
        // Portugal: 「…e continuação de um bom dia」.
        "continuação de um bom dia", "continuação de bom dia", "continuacao de um bom dia", "tenha um bom dia",
        "boa continuação",
    ]),
    ("th", &[
        "บ๊ายบาย", "บายบาย", "ไว้คุยกันใหม่", "แล้วคุยกันใหม่", "แล้วพบกันใหม่", "พบกันใหม่", "ไปก่อนนะ", "วางสายนะ",
        "ไว้เจอกัน", "ไว้เจอกันใหม่",
    ]),
    ("vi", &[
        "tạm biệt", "tam biet", "hẹn gặp lại", "hen gap lai", "chào nhé", "chao nhe", "một ngày tốt lành",
        "ngày tốt lành",
    ]),
    ("id", &["sampai jumpa", "sampai ketemu", "dadah"]),
    ("it", &[
        "arrivederci", "arrivederla", "buona giornata", "buongiornata", "buona serata", "buonanotte",
        "buona continuazione", "grazie a lei", "stammi bene", "stia bene", "a presto", "a domani",
        "ci vediamo", "ci sentiamo", "ciao ciao", "a dopo", "a più tardi", "a piu tardi", "buon proseguimento",
        "buon weekend", "buon fine settimana", "buona domenica", "ci risentiamo", "ci rivediamo", "a risentirci",
        "la saluto", "ti saluto", "vi saluto", "cordiali saluti",
    ]),
    ("ru", &[
        "до свидания", "до свиданья", "всего доброго", "всего хорошего", "хорошего дня", "хорошего вечера",
        "до встречи", "до связи", "пока пока",
    ]),
    ("ar", &[
        "مع السلامة", "مع السلامه", "إلى اللقاء", "الى اللقاء", "في أمان الله", "في امان الله", "وداعاً",
        "وداعا", "باي باي",
    ]),
    ("hi", &[
        "अलविदा", "फिर मिलेंगे", "फिर मिलते हैं", "फिर बात करते हैं", "बाद में बात करते हैं",
        "आपका दिन शुभ हो", "बाय बाय",
    ]),
    ("nl", &["tot ziens", "tot spreeks", "doei", "dag hoor", "fijne dag nog", "fijne avond"]),
    // "Na razie" is "bye" as the last words (or before pa / cześć / thanks): "Na razie nie wiem" is no goodbye.
    // With a name or another goodbye after it ("No to na razie, Ania") the older check decides.
    ("pl", &[
        "do widzenia", "na razie", "na razie pa", "na razie cześć", "na razie czesc", "na razie dzięki",
        "na razie dzieki", "na razie dziękuję", "na razie dziekuje", "do usłyszenia", "do uslyszenia",
        "dobrego dnia", "miłego dnia", "milego dnia",
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
];

/// Greetings that also say goodbye: farewells only once the business is done
/// ([`ClosingMachine::greeting_bye_counts`]).
const GREETING_BYES: Lexicon = &[
    ("ja", &["失礼いたします", "失礼致します", "失礼します"]),
    ("ko", &["수고하십니다", "수고하셨습니다", "안녕"]),
    ("th", &["สวัสดีค่ะ", "สวัสดีครับ", "สวัสดี"]),
    ("vi", &["chào anh ạ", "chào chị ạ", "chào anh", "chào chị", "chào em", "chào bạn", "chao anh"]),
    ("id", &["selamat siang", "selamat pagi", "selamat sore", "selamat malam"]),
    // "Buon lavoro" and "alla prossima" are everyday words too ("cerco un buon lavoro").
    ("it", &["ciao", "buongiorno", "buonasera", "buon pomeriggio", "buon lavoro", "alla prossima"]),
    ("de", &["servus"]),
    ("fr", &["salut", "bonsoir"]),
    ("es", &["buenas noches", "buenas tardes", "buen día", "buen dia", "buenos días", "buenos dias"]),
    ("pt", &["boa noite", "boa tarde"]),
    ("ru", &["пока"]),
    ("hi", &["नमस्ते", "नमस्कार"]),
    ("nl", &["goedendag", "dag"]),
    ("pl", &["cześć", "czesc"]),
    ("ro", &["bună ziua", "buna ziua"]),
    ("uk", &["бувай", "па па"]),
    ("mr", &["नमस्कार"]),
    ("ta", &["வணக்கம்"]),
    ("te", &["నమస్కారం"]),
    ("tr", &["iyi günler", "iyi akşamlar", "kolay gelsin"]),
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
];

/// Thanks for something, at the very end ("Thanks for your help", "thank you
/// for calling", "gracias por su ayuda"): thanks as well, with up to
/// [THANKS_FOR_MAX_WORDS] words after it.
const THANKS_FOR: &[&str] = &[
    "thank you so much for", "thank you very much for", "thanks so much for", "thanks again for", "thank you for",
    "thanks for", "many thanks for", "muchas gracias por", "gracias por", "merci beaucoup pour", "merci pour",
    "vielen dank für", "danke für", "obrigado pela", "obrigada pela", "obrigado pelo", "obrigada pelo",
    "obrigado por", "obrigada por", "grazie mille per", "grazie per",
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
    text.trim_end().ends_with(|c: char| "。！？!?.…～~।؟".contains(c))
}

fn is_punct(c: char) -> bool {
    if c.is_ascii() {
        return !(c.is_ascii_alphanumeric() || c == '\'');
    }
    c.is_whitespace() || "，。！？、～〜…．；：「」『』（）【】《》〈〉·・—–“”‘¡¿«»｡｢｣،؛؟।॥".contains(c)
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
    if t.ends_with(['?', '？', '؟']) || t.ends_with("까요") || t.ends_with("か。") {
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
            .or_else(|| ends_with_any(t, PRE_CLOSINGS).map(|(l, b)| (Kind::PreClosing, l, b, false)))
            .or_else(|| ends_with_any(t, GREETING_BYES).map(|(l, b)| (Kind::Farewell, l, b, true)));
        if let Some((kind, lang, before, ambiguous)) = found {
            // 「你先不要掛啦」「請別掛囉」: a hang-up word after "don't" asks to stay on.
            if kind == Kind::Farewell && asks_to_stay(before, &t[before.len()..]) {
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
        t = peel(t)?.0;
    }
    None
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
    if it_farewell_before(before) {
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
    for _ in 0..MAX_PEEL {
        match peel(t) {
            Some((rest, Peel::Thanks)) => {
                thanked = true;
                t = rest;
            }
            Some((rest, Peel::Particle | Peel::Word)) if !thanked => t = rest,
            _ => break,
        }
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
        let start = std::time::Instant::now();
        let mut m = ClosingMachine::new("zh-TW");
        for i in 0..1_000 {
            let t = &texts[i % texts.len()];
            let step = m.on_caller(t, t, false, i % 7 == 0);
            std::hint::black_box(step);
            std::hint::black_box(is_backchannel(t));
        }
        let took = start.elapsed();
        // Microseconds in a release build; an unoptimised test build on a slow CI
        // machine (Windows) is many times slower, so the bound here is loose:
        // still well under a millisecond a judgement.
        assert!(took < std::time::Duration::from_millis(800), "1000 judgements took {took:?}");
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
}
