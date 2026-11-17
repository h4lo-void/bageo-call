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
        let hit = classify(heard).filter(|h| !h.ambiguous || self.greeting_bye_counts(h));
        // Opening greetings never close: 수고하십니다, Servus, สวัสดีค่ะ at the start.
        let old_rules = old_rules && !opening_only;
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
    /// closing cue / a short "ok" right before it.
    fn greeting_bye_counts(&self, h: &Hit) -> bool {
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
        "短訊覆你", "短讯覆你",
    ]),
    ("ja", &[
        "以上となりますが", "以上となります", "それではよろしくお願いいたします",
        "それではよろしくお願いします", "そろそろ切るね", "そろそろ切ります", "そろそろ失礼します",
        "折り返しご連絡いたします", "折り返しご連絡します", "折り返しお電話いたします", "折り返しお電話します",
    ]),
    ("ko", &["나중에다시연락드릴게요", "다시연락드릴게요", "나끊는다", "그럼이만", "이만줄일게요"]),
    ("es", &[
        "bueno te dejo", "te dejo", "le dejo", "nada era solo eso", "era solo eso", "hablamos luego",
        "hablamos después", "hablamos despues",
    ]),
    ("fr", &[
        "je ne vous retiens pas plus longtemps", "je ne vous retiens pas", "je ne te retiens pas",
        "je vous laisse", "je te laisse", "c'est noté", "c'est note",
    ]),
    ("de", &[
        "gut dann verbleiben wir so", "dann verbleiben wir so", "verbleiben wir so",
        "alles klar dann weiß ich bescheid", "dann weiß ich bescheid", "dann weiss ich bescheid",
    ]),
    ("pt", &["então tá bom", "entao ta bom", "combinado então", "combinado entao", "a gente se fala"]),
    ("th", &["งั้นแค่นี้", "แค่นี้ก่อน", "แค่นี้"]),
    ("vi", &["dạ vậy nhé", "vậy nhé", "vay nhe", "thế nhé", "the nhe", "thôi nhé"]),
    ("id", &["oke sudah dulu ya", "sudah dulu ya", "sudah dulu", "udah dulu ya", "udah dulu", "segitu dulu ya", "itu saja"]),
];

/// Goodbyes said only at the end.
const FAREWELLS: Lexicon = &[
    ("en", &[
        "bye", "bye bye", "goodbye", "good bye", "talk to you soon", "talk soon", "take care",
        "have a great rest of your day", "have a good rest of your day", "enjoy the rest of your day",
        "cheers mate", "lovely ta", "ta ta", "see you", "see ya", "catch you later",
    ]),
    ("zh-TW", &[
        "掰掰", "掰", "拜拜", "再見", "再會", "掰囉", "掰掰囉", "掛囉", "掛啦", "先掛了", "我掛了", "我先掛了", "那我掛了",
        "那掛了",
    ]),
    ("zh-CN", &["再见", "再会", "挂了啊", "先挂了", "我挂了", "那挂了", "好嘞拜拜", "挂啰", "挂咯", "挂啦", "那我挂了"]),
    ("yue", &["得啦唔該晒", "得啦唔该晒", "得啦唔該曬", "先掛啦", "拜拜啦", "byebye"]),
    ("ja", &[
        // A bare 「ありがとうございました」 is thanks, like 謝謝 elsewhere; with 本日は／今日は it closes.
        "失礼いたします", "失礼致します", "失礼します", "本日はありがとうございました", "今日はありがとうございました", "じゃあね", "じゃあまた",
        "またね", "さようなら", "お疲れ様でした", "お疲れさまでした", "ではまた", "それではまた", "バイバイ",
        "ばいばい",
    ]),
    ("ko", &[
        "들어가세요", "수고하세요", "끊겠습니다", "끊을게요", "응끊어", "어끊어", "그래끊어", "안녕히계세요",
        "안녕히가세요", "다음에봬요",
    ]),
    ("es", &[
        "chao", "chau", "hasta luego", "hasta pronto", "cuídate", "cuidate", "cuídese", "cuidese",
        "un abrazo", "un beso", "besos", "adiós", "adios", "nos vemos",
    ]),
    ("fr", &[
        "au revoir", "bonne journée", "bonne journee", "bonne soirée", "bonne soiree", "à bientôt",
        "a bientot", "bisous", "bises", "à plus",
    ]),
    ("de", &[
        "auf wiederhören", "auf wiederhoren", "wiederhören", "schönen tag noch", "schönen tag", "tschüss",
        "tschüs", "tschuss", "baba", "ciao", "pfiat di", "pfiat eich",
    ]),
    ("pt", &[
        "tchau tchau", "tchau", "até logo", "ate logo", "até mais", "um abraço", "um abraco", "beijos",
        "beijo", "fica com deus", "vai com deus",
    ]),
    ("th", &["บ๊ายบาย", "บายบาย", "ไว้คุยกันใหม่", "แล้วคุยกันใหม่"]),
    ("vi", &["tạm biệt", "tam biet", "hẹn gặp lại", "hen gap lai", "chào nhé", "chao nhe"]),
    ("id", &["sampai jumpa", "sampai ketemu", "dadah"]),
];

/// Greetings that also say goodbye: farewells only once the business is done
/// ([`ClosingMachine::greeting_bye_counts`]).
const GREETING_BYES: Lexicon = &[
    ("ko", &["수고하십니다", "수고하셨습니다"]),
    ("th", &["สวัสดีค่ะ", "สวัสดีครับ", "สวัสดี"]),
    ("vi", &["chào anh ạ", "chào chị ạ", "chào anh", "chào chị", "chào em", "chào bạn", "chao anh"]),
    ("id", &["selamat siang", "selamat pagi", "selamat sore", "selamat malam"]),
    ("de", &["servus"]),
    ("fr", &["salut", "bonsoir"]),
    ("es", &["buenas noches", "buenas tardes"]),
    ("pt", &["boa noite", "boa tarde"]),
];

/// How calls are answered and opened; never a goodbye by themselves.
const OPENING_GREETINGS: &[&str] = &[
    "喂", "你好", "您好", "哈囉", "哈啰", "もしもし", "お世話になっております", "お世話になります",
    "お疲れ様です", "こんにちは", "여보세요", "안녕하세요", "hello", "hi", "hey", "hiya", "good morning",
    "good afternoon", "good evening", "hola", "diga", "bueno", "aló", "alo", "allo", "allô", "buenos días",
    "buenos dias", "buenas", "bonjour", "hallo", "guten tag", "guten morgen", "grüß gott", "grüss gott",
    "gruss gott", "grüezi", "moin", "alô", "oi", "olá", "ola", "bom dia", "tudo bem", "ฮัลโหล", "a lô",
    "xin chào", "halo",
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
    ("ko", &["네", "네네", "예", "어", "응", "맞아요", "그렇죠", "네에"]),
    ("en", &[
        "uh huh", "mhm", "mm", "hmm", "right", "gotcha", "yeah", "yep", "yes", "okay", "ok", "sure", "i see",
        "got it",
    ]),
    ("es", &["vale", "claro", "dale", "sí", "si", "ajá", "aja", "ya"]),
    ("fr", &["d'accord", "tout à fait", "tout a fait", "oui", "ouais", "entendu", "bien sûr"]),
    ("de", &["genau", "alles klar", "ja", "jawohl", "stimmt"]),
    ("pt", &["entendi", "tá bom", "ta bom", "tá", "pois", "pois é", "sim", "uhum", "certo", "beleza"]),
    ("th", &["ค่ะ", "ครับ", "อืม", "ใช่"]),
    ("vi", &["vâng", "dạ", "ừ", "ờ", "dạ vâng"]),
    ("id", &["iya", "oke"]),
];

/// Thanks the shared list ([`crate::locale::strip_trailing_thanks`]) does not have.
const EXTRA_THANKS: &[&str] = &[
    "ขอบคุณมาก", "ขอบคุณ", "唔該晒", "唔该晒", "唔該曬", "唔該哂", "唔该哂", "唔該", "唔该", "多謝晒", "多谢晒", "多謝曬",
    "多謝哂", "多谢哂", "多謝", "多谢", "cảm ơn anh", "cảm ơn chị", "cám ơn anh", "terima kasih banyak",
];

/// Whom thanks or a goodbye is said to, after it (「唔該晒王生」「謝謝王先生」
/// 「再見陳小姐」): a title, with up to two letters of a name before it.
const ADDRESS_TITLES: &[&str] = &[
    "先生", "小姐", "太太", "女士", "師傅", "师傅", "老闆", "老板", "經理", "经理", "醫師", "医师", "醫生", "医生", "老師",
    "老师", "姐", "哥",
];

/// Particles after the last word: 「先這樣囉」, 「นะคะ」.
const ZH_PARTICLES: &str = "喔哦囉啰啦呢吧耶齁欸啊喇嘞呀噢";
const TH_PARTICLES: &[&str] = &["ค่ะ", "คะ", "ครับ", "นะ", "จ้ะ", "จ้า", "จ๊ะ", "ฮะ"];

/// Words that may follow a goodbye: whom it is said to, a particle.
const TAIL_WORDS: &[&str] = &[
    "mate", "then", "now", "too", "again", "dear", "sir", "madam", "ma'am", "anh", "chị", "chi", "em", "ạ",
    "pak", "bu", "bapak", "ibu", "kak", "señor", "señora", "senhor", "senhora", "monsieur", "madame",
    "mucho", "ya", "you",
];

/// Only the end of a long window is looked at.
const TAIL_CHARS: usize = 120;
/// Trailing particles / thanks / short replies peeled off before giving up.
const MAX_PEEL: usize = 5;

fn is_latin_letter(c: char) -> bool {
    c.is_ascii_alphabetic() || ('\u{00C0}'..='\u{024F}').contains(&c) || ('\u{1E00}'..='\u{1EFF}').contains(&c)
}

fn is_latin(p: &str) -> bool {
    p.chars().any(is_latin_letter)
}

/// Ends a sentence: the speech-to-text's own full stop, question or exclamation mark.
fn ends_sentence(text: &str) -> bool {
    text.trim_end().ends_with(|c: char| "。！？!?.…～~".contains(c))
}

fn is_punct(c: char) -> bool {
    if c.is_ascii() {
        return !(c.is_ascii_alphanumeric() || c == '\'');
    }
    c.is_whitespace() || "，。！？、～〜…．；：「」『』（）【】《》〈〉·・—–“”‘¡¿«»｡｢｣".contains(c)
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
            out.extend(c.to_lowercase());
            space = false;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// `t` (normalized) ends with `p`: whole words in Latin script, spaces ignored
/// otherwise. Returns what comes before it.
fn ends_with_phrase<'a>(t: &'a str, p: &str) -> Option<&'a str> {
    if is_latin(p) {
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
    for th in EXTRA_THANKS {
        if let Some(rest) = ends_with_phrase(t, th) {
            return Some((rest, Peel::Thanks));
        }
    }
    let rest = crate::locale::strip_trailing_thanks(t);
    if rest.len() < t.len() {
        return Some((rest.trim_end(), Peel::Thanks));
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
    if t.ends_with(['?', '？']) || t.ends_with("까요") || t.ends_with("か。") {
        return true;
    }
    let core = t
        .trim_end_matches(|c: char| !c.is_alphanumeric())
        .trim_end_matches(|c: char| ZH_PARTICLES.contains(c));
    core.ends_with('嗎') || core.ends_with('吗') || ["ですか", "ますか", "でしょうか"].iter().any(|q| core.ends_with(q))
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
            .or_else(|| ends_with_any(t, GREETING_BYES).map(|(l, b)| (Kind::Farewell, l, b, true)))
            .or_else(|| ends_with_any(t, PRE_CLOSINGS).map(|(l, b)| (Kind::PreClosing, l, b, false)));
        if let Some((kind, lang, before, ambiguous)) = found {
            // 「你先不要掛啦」「請別掛囉」: a hang-up word after "don't" asks to stay on.
            if kind == Kind::Farewell && asks_to_stay(before, &t[before.len()..]) {
                return None;
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

/// Right before a greeting-goodbye: thanks, a short "ok" or a pre-closing.
fn cue_before(before: &str) -> bool {
    let mut t = before.trim_end();
    for _ in 0..MAX_PEEL {
        if t.is_empty() {
            return false;
        }
        if ends_with_any(t, PRE_CLOSINGS).is_some() {
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

/// Words that settle a matter before closing thanks: agreement, "got it", an
/// apology for a wrong number. Latin ones are whole words; the others are
/// compared without spaces.
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
];

/// Settling words that close a call with nothing more said: a wrong number.
const SETTLED_ALONE: &[&str] =
    &["打錯", "打错", "wrong number", "잘못걸", "número equivocado", "falsch verbunden", "失礼しました"];

/// Words that ask something even without a question mark (「那明天幾點到」,
/// "what time tomorrow", 「何時ですか」): such words before thanks are no wrap-up.
const ASKS_RUNS: &[&str] = &[
    "幾點", "几点", "什麼", "什么", "哪裡", "哪里", "哪天", "多少", "怎麼", "怎么", "為什麼", "为什么", "是不是",
    "有沒有", "有没有", "可不可以", "能不能", "要不要", "何時", "いつ", "どこ", "何で", "なぜ", "ですか", "ますか",
    "언제", "어디", "몇", "뭐", "왜",
];
const ASKS_WORDS: &[&str] = &[
    "what", "when", "where", "which", "how", "who", "why", "can you", "could you", "is there", "are you", "do you",
    "will you", "qué", "cuándo", "dónde", "cómo", "quand", "où", "comment", "quel", "quelle", "wann", "wo", "wie",
    "was", "welche", "quando", "onde", "como", "qual",
];

fn asks(t: &str) -> bool {
    let spaced = format!(" {t} ");
    ASKS_RUNS.iter().any(|w| t.contains(w)) || ASKS_WORDS.iter().any(|w| spaced.contains(&format!(" {w} ")))
}

/// Something said besides the settling words ("great" and thanks is only
/// thanks): three or more CJK letters, or two or more other words.
fn has_matter(t: &str) -> bool {
    let mut s = format!(" {t} ");
    for w in SETTLED {
        s = if is_latin(w) { s.replace(&format!(" {w} "), "  ") } else { s.replace(w, " ") };
    }
    let cjk = s
        .chars()
        .filter(|c| matches!(*c as u32, 0x3040..=0x30FF | 0x3400..=0x9FFF | 0xAC00..=0xD7AF) && !ZH_PARTICLES.contains(*c))
        .count();
    const FILLER: &[&str] = &["oh", "ah", "um", "uh", "so", "and", "then", "well", "yes", "yeah", "thanks", "thank", "you"];
    let words = s
        .split_whitespace()
        .filter(|w| w.chars().any(is_latin_letter) && w.chars().count() >= 2 && !FILLER.contains(w))
        .count();
    cjk >= 3 || words >= 2
}

/// The caller's words since their last question settle the matter and end on
/// thanks: 「好的，沒問題，那明天我放管理室，謝謝！」, "Oh sorry, wrong number,
/// thanks". What they heard may still hold their earlier question (「…有人在家嗎？」);
/// only what follows it counts. Thanks after nothing but short replies (「好，謝謝」)
/// is not one.
fn thanks_wrap(text: &str) -> bool {
    let text = text.rsplit(['?', '？']).next().unwrap_or("");
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
        list.iter().any(|w| if is_latin(w) { spaced.contains(&format!(" {w} ")) } else { bare.contains(w) })
    };
    has(SETTLED_ALONE) || (has_matter(t) && SETTLED.iter().any(|w| {
        if is_latin(w) {
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
            bye: ["失礼いたします", "本日はありがとうございました"],
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
                    assert!(is_latin(p) || !p.contains(' '), "spaces in a non-Latin phrase: {p}");
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
