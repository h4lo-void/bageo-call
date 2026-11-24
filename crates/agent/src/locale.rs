// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Languages: which one the AI speaks, the phrases the call logic listens for,
//! and the words used in notifications.
//!
//! Traditional Chinese (Taiwan) is the language this project grew up in and
//! the only one exercised on real calls. Simplified Chinese reuses its prompts
//! (converted); every other language shares one set of English instructions
//! ([`crate::prompts`]) filled in with that language's phrases from [`phrases`].
//!
//! The settings value lives in a process-wide slot ([`set_current`]) because
//! the Live session's helpers are free functions. Unset (unit tests) = zh-TW.

use bageo_packs::data::{OfficialLang, Script, OFFICIAL_LANG_INFO};

mod notify_words;

pub(crate) use notify_words::words_over_english;
pub use notify_words::{notify_words, words, Words};

/// A Gemini Live language outside the nine core ones (Italian, Thai, Arabic,
/// ...): an index into [`OFFICIAL_LANG_INFO`] that only ever points at a
/// non-core entry. Until a language has its own phrases and prompts, every
/// part of the agent treats it the way it treats English.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ext(u8);

impl Ext {
    /// The non-core language whose primary code is `code` (`it`, `fil`,
    /// `nb`; lower case). Core codes (`en`, `ja`, ...) give None.
    pub fn from_code(code: &str) -> Option<Ext> {
        bageo_packs::data::official_ext_index(code).and_then(|i| u8::try_from(i).ok()).map(Ext)
    }

    /// Every non-core official language, in the order of the official list.
    pub fn all() -> impl Iterator<Item = Ext> {
        OFFICIAL_LANG_INFO.iter().enumerate().filter(|(_, l)| !l.core).filter_map(|(i, _)| u8::try_from(i).ok()).map(Ext)
    }

    fn info(self) -> &'static OfficialLang {
        &OFFICIAL_LANG_INFO[usize::from(self.0)]
    }

    /// Settings code: the primary code (`it`, `fil`, `nb`).
    pub fn code(self) -> &'static str {
        self.info().code
    }

    /// The code exactly as the official list writes it (`it-IT`, `fil`).
    pub fn official(self) -> &'static str {
        self.info().official
    }

    /// The script it is written in.
    pub fn script(self) -> Script {
        self.info().script
    }

    /// The country it is mainly spoken in (ISO 3166 alpha-2, lower case):
    /// where a call is taken to be, and where a number without a country
    /// code is dialled, when no SIM or setting says. English is a core
    /// language, so none of these is the United States.
    pub fn country(self) -> &'static str {
        match self.code() {
            "af" => "za",
            "am" => "et",
            "ar" => "eg",
            "az" => "az",
            "be" => "by",
            "bg" => "bg",
            "bn" => "bd",
            "ca" | "eu" | "gl" => "es",
            "ceb" | "fil" => "ph",
            "cs" => "cz",
            "da" => "dk",
            "el" => "gr",
            "et" => "ee",
            "fa" => "ir",
            "fi" => "fi",
            "gu" | "hi" | "kn" | "kok" | "mai" | "ml" | "mr" | "or" | "pa" | "ta" | "te" => "in",
            "he" => "il",
            "hr" => "hr",
            "ht" => "ht",
            "hu" => "hu",
            "hy" => "am",
            "id" | "jv" => "id",
            "is" => "is",
            "it" => "it",
            "ka" => "ge",
            "la" => "va",
            "lb" => "lu",
            "lo" => "la",
            "lt" => "lt",
            "lv" => "lv",
            "mg" => "mg",
            "mk" => "mk",
            "mn" => "mn",
            "ms" => "my",
            "my" => "mm",
            "nb" | "nn" => "no",
            "ne" => "np",
            "nl" => "nl",
            "pl" => "pl",
            "ps" => "af",
            "ro" => "ro",
            "ru" => "ru",
            "sd" | "ur" => "pk",
            "si" => "lk",
            "sk" => "sk",
            "sl" => "si",
            "sq" => "al",
            "sr" => "rs",
            "sv" => "se",
            "sw" => "ke",
            "th" => "th",
            "tr" => "tr",
            "uk" => "ua",
            "vi" => "vn",
            _ => "us",
        }
    }
}

impl std::fmt::Debug for Ext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    ZhTw,
    ZhCn,
    En,
    Ja,
    Ko,
    Es,
    Fr,
    De,
    Pt,
    /// A language outside the nine core ones. [`Lang::parse`] never returns
    /// it; only [`Lang::parse_any`] does.
    Ext(Ext),
}

/// The nine core languages: the ones settings accept and the UI lists.
pub const CORE: [Lang; 9] = [
    Lang::En,
    Lang::ZhTw,
    Lang::ZhCn,
    Lang::Ja,
    Lang::Ko,
    Lang::Es,
    Lang::Fr,
    Lang::De,
    Lang::Pt,
];

/// Same as [`CORE`]. Code that walks every language a user can pick today.
pub const ALL: [Lang; 9] = CORE;

impl Lang {
    /// Settings / UI code (`en`, `zh-TW`, …). Unknown → English. Only ever
    /// returns one of the nine core languages.
    pub fn parse(s: &str) -> Lang {
        let t = s.trim().to_ascii_lowercase().replace('_', "-");
        match t.as_str() {
            "zh-tw" | "zh-hant" | "zh-hk" | "zh" => Lang::ZhTw,
            "zh-cn" | "zh-hans" | "zh-sg" => Lang::ZhCn,
            _ => match t.split('-').next().unwrap_or("") {
                "ja" => Lang::Ja,
                "ko" => Lang::Ko,
                "es" => Lang::Es,
                "fr" => Lang::Fr,
                "de" => Lang::De,
                "pt" => Lang::Pt,
                _ => Lang::En,
            },
        }
    }

    /// Like [`Lang::parse`], but also knows the other official languages, and
    /// says None for a code it does not know instead of falling back to English.
    ///
    /// Order: a core language (whatever [`Lang::parse`] recognises, plus
    /// `cmn` read as `zh`), then a non-core official language by its official
    /// code (`it-IT`) or primary code (`it`, `it-CH`). Legacy and macro codes
    /// browsers and Android still send are mapped first: `no` → `nb`,
    /// `iw` → `he`, `in` → `id`, `tl` → `fil`.
    pub fn parse_any(s: &str) -> Option<Lang> {
        let t = s.trim().to_ascii_lowercase().replace('_', "-");
        let mut parts = t.splitn(2, '-');
        let primary = parts.next().unwrap_or("");
        let rest = parts.next();
        let primary = match primary {
            "cmn" => "zh",
            "no" => "nb",
            "iw" => "he",
            "in" => "id",
            "tl" => "fil",
            p => p,
        };
        if primary.is_empty() {
            return None;
        }
        let norm = match rest {
            Some(r) => format!("{primary}-{r}"),
            None => primary.to_string(),
        };
        let core = Lang::parse(&norm);
        if core != Lang::En || primary == "en" {
            return Some(core);
        }
        Ext::from_code(primary).map(Lang::Ext)
    }

    pub fn code(self) -> &'static str {
        match self {
            Lang::ZhTw => "zh-TW",
            Lang::ZhCn => "zh-CN",
            Lang::En => "en",
            Lang::Ja => "ja",
            Lang::Ko => "ko",
            Lang::Es => "es",
            Lang::Fr => "fr",
            Lang::De => "de",
            Lang::Pt => "pt",
            Lang::Ext(e) => e.code(),
        }
    }

    /// One of the nine core languages.
    pub fn is_core(self) -> bool {
        !matches!(self, Lang::Ext(_))
    }

    /// The language's name in English (`Italian`, `Traditional Chinese (Taiwan Mandarin)`).
    pub fn name_en(self) -> &'static str {
        match self {
            Lang::Ext(e) => e.info().name_en,
            core => phrases(core).name_en,
        }
    }

    /// Written right to left (Arabic, Hebrew, Persian, Urdu, ...).
    pub fn rtl(self) -> bool {
        match self {
            Lang::Ext(e) => e.info().rtl,
            _ => false,
        }
    }

    /// The language whose settings, phrases and prompts stand in for this one
    /// until it has its own: English for every non-core language.
    pub fn base(self) -> Lang {
        match self {
            Lang::Ext(_) => Lang::En,
            core => core,
        }
    }

    pub fn is_zh(self) -> bool {
        matches!(self, Lang::ZhTw | Lang::ZhCn)
    }

}

/// What the running agent is set to (settings `language`, `notify_language`,
/// `answer_style`, `owner_name`, `country_code`), for the Live session's free functions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Active {
    /// The language the AI speaks with callers.
    pub lang: Lang,
    /// The language of what the owner reads: notifications, call summaries,
    /// the notes in the web console's call list, appointment cards (settings
    /// `notify_language`; empty there = `lang`).
    pub notify_lang: Lang,
    /// Answer as the owner's AI assistant instead of as the owner.
    pub assistant: bool,
    pub owner: String,
    /// Other names callers use for the owner (settings `owner_aliases`).
    pub owner_aliases: Vec<String>,
    /// Country calling code of the phone's SIM, digits only (`886`, `1`, `81`).
    pub country_code: String,
    /// Names speech-to-text keeps getting wrong → how they should read
    /// (settings `name_corrections`).
    pub name_fixes: Vec<(String, String)>,
    /// The country the owner lives in (settings `home_country`, ISO 3166
    /// alpha-2, lowercase): a call's region when its SIM does not say. None =
    /// worked out automatically.
    pub home_country: Option<String>,
    /// Talk-style slider, 1 to 5 (settings `talk_style_level`); None when the
    /// owner's own prompt is in use, which replaces the talk style.
    pub talk_style: Option<u8>,
}

impl Default for Active {
    /// Unset (unit tests): Taiwan, answering as the owner — what the call
    /// logic was written and tested with.
    fn default() -> Self {
        Self {
            lang: Lang::ZhTw,
            notify_lang: Lang::ZhTw,
            assistant: false,
            owner: String::new(),
            owner_aliases: Vec::new(),
            country_code: "886".to_string(),
            name_fixes: Vec::new(),
            home_country: None,
            talk_style: None,
        }
    }
}

static ACTIVE: std::sync::RwLock<Option<Active>> = std::sync::RwLock::new(None);

/// Set at startup and on every settings save.
pub fn set_active(a: Active) {
    *ACTIVE.write().unwrap_or_else(|e| e.into_inner()) = Some(a);
}

pub fn active() -> Active {
    ACTIVE
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

pub fn current() -> Lang {
    active().lang
}

/// The language of what the owner reads ([`Active::notify_lang`]).
pub fn notify_lang() -> Lang {
    active().notify_lang
}

/// The call phrases in the language the owner reads (the blocked-number
/// announcement quoted in notifications); everything else the owner reads is
/// in [`notify_words`].
pub fn notify_phrases() -> &'static Phrases {
    phrases(notify_lang())
}

/// [`kv_sep`] for what the owner reads.
pub fn notify_kv_sep() -> &'static str {
    &notify_words().sep
}

/// [`fill_in`] for what the owner reads.
pub fn notify_fill(template: &str, slots: &[(&str, &str)]) -> String {
    fill_in(notify_lang(), template, slots)
}

/// A text of [`Words`] with its named `{slots}` filled in one pass; a slot the
/// text does not name is left out, a `{name}` that is no slot stays as it is.
/// For Simplified Chinese the filled text goes through the converter, so a
/// value written in Traditional characters (a contact's name, a phone's label)
/// reads as Simplified, as it always did.
pub fn fill_in(lang: Lang, template: &str, slots: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let slot = after
            .find('}')
            .and_then(|j| slots.iter().find(|s| s.0 == &after[..j]).map(|s| (j, s.1)));
        match slot {
            Some((j, value)) => {
                out.push_str(value);
                rest = &after[j + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    if lang == Lang::ZhCn && !slots.is_empty() {
        crate::zh_convert::to_cn(&out)
    } else {
        out
    }
}

/// Text the owner reads (summaries, notifications, Telegram replies): put into
/// Traditional Chinese (Taiwan) when that is the notification language — the
/// summary model and speech-to-text often write Simplified — and left as it is
/// otherwise (the conversion would rewrite Japanese kanji and Simplified Chinese).
pub fn for_owner(text: &str) -> String {
    for_owner_in(notify_lang(), text)
}

/// [`for_owner`] for a given notification language.
pub fn for_owner_in(lang: Lang, text: &str) -> String {
    if lang == Lang::ZhTw {
        crate::zh_tw::for_reading(text)
    } else {
        text.to_string()
    }
}

/// `wrong=right` lines of the setting `name_corrections` as pairs; lines
/// without both sides are dropped.
pub fn parse_name_fixes(lines: &[String]) -> Vec<(String, String)> {
    lines
        .iter()
        .filter_map(|l| l.split_once('=').or_else(|| l.split_once('→')))
        .map(|(a, b)| (a.trim().to_string(), b.trim().to_string()))
        .filter(|(a, b)| !a.is_empty() && !b.is_empty() && a != b)
        .take(50)
        .collect()
}

/// Apply name corrections to a text. In a language written with spaces a name
/// is replaced as a whole word only ("Jon" → "John" leaves "Jones" alone);
/// Chinese, Japanese and Korean, whose names run into the next word, as before.
pub fn fix_names_with(text: &str, fixes: &[(String, String)]) -> String {
    let mut out = text.to_string();
    for (wrong, right) in fixes {
        if out.contains(wrong.as_str()) {
            out = replace_name(&out, wrong, right);
        }
    }
    out
}

/// Han, kana or Hangul: no word boundary is needed around a name in them.
fn runs_into_words(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF | 0x3040..=0x30FF | 0x3130..=0x318F | 0x31F0..=0x31FF
        | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF)
}

/// A letter or digit of a language written with spaces between words.
fn word_char(c: char) -> bool {
    c.is_alphanumeric() && !runs_into_words(c)
}

/// `text` with `wrong` replaced by `right` where it is a whole word: its first
/// (last) letter, when of a spaced language, may not follow (precede) another one.
fn replace_name(text: &str, wrong: &str, right: &str) -> String {
    let check_start = wrong.chars().next().is_some_and(word_char);
    let check_end = wrong.chars().next_back().is_some_and(word_char);
    if !check_start && !check_end {
        return text.replace(wrong, right);
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (i, _) in text.match_indices(wrong) {
        let end = i + wrong.len();
        let joined_before = check_start && text[..i].chars().next_back().is_some_and(word_char);
        let joined_after = check_end && text[end..].chars().next().is_some_and(word_char);
        if joined_before || joined_after {
            continue;
        }
        out.push_str(&text[last..i]);
        out.push_str(right);
        last = end;
    }
    out.push_str(&text[last..]);
    out
}

/// A transcript or summary with the owner's name corrections applied
/// (speech-to-text mishears names the same way every time).
pub fn fix_names(text: &str) -> String {
    let fixes = active().name_fixes;
    if fixes.is_empty() {
        return text.to_string();
    }
    fix_names_with(text, &fixes)
}

/// Country calling code of the phone's SIM (digits); empty = not set, numbers
/// are compared as they are written.
pub fn country_code() -> String {
    active()
        .country_code
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect()
}

/// Main language of a country, by calling code, for the countries whose
/// language the AI speaks: a call from there opens in it when the phone app
/// does not say which SIM took the call (see `gemini_live::call_lang_plan`).
const CALLING_CODE_LANGS: &[(&str, Lang)] = &[
    // US / Canada, UK, Australia, New Zealand, Ireland
    ("1", Lang::En),
    ("44", Lang::En),
    ("61", Lang::En),
    ("64", Lang::En),
    ("353", Lang::En),
    // Singapore
    ("65", Lang::En),
    ("81", Lang::Ja),
    ("82", Lang::Ko),
    // Spain, Mexico, Argentina, Colombia, Chile, Peru, Venezuela
    ("34", Lang::Es),
    ("52", Lang::Es),
    ("54", Lang::Es),
    ("57", Lang::Es),
    ("56", Lang::Es),
    ("51", Lang::Es),
    ("58", Lang::Es),
    // France, Belgium
    ("33", Lang::Fr),
    ("32", Lang::Fr),
    // Germany, Austria, Switzerland
    ("49", Lang::De),
    ("43", Lang::De),
    ("41", Lang::De),
    // Brazil, Portugal
    ("55", Lang::Pt),
    ("351", Lang::Pt),
    ("86", Lang::ZhCn),
    // Taiwan, Hong Kong, Macau
    ("886", Lang::ZhTw),
    ("852", Lang::ZhTw),
    ("853", Lang::ZhTw),
];

/// The language of the country an international number (`+…`) belongs to,
/// when it is one the AI speaks. Unknown country or no `+`: `None`.
pub fn calling_code_lang(number: &str) -> Option<Lang> {
    let n: String = number
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '+')
        .collect();
    longest_code_match(n.strip_prefix('+')?, CALLING_CODE_LANGS)
}

/// The entry with the longest calling code that starts `digits`: a `+1xxx`
/// entry wins over `+1`, and `+351` / `+353` never fall back to a shorter code.
fn longest_code_match(digits: &str, table: &[(&str, Lang)]) -> Option<Lang> {
    table
        .iter()
        .filter(|&&(code, _)| !code.is_empty() && digits.starts_with(code))
        .max_by_key(|&&(code, _)| code.len())
        .map(|&(_, lang)| lang)
}

/// Main language of a country (ISO 3166 alpha-2), for the countries whose
/// language the AI speaks: what a caller from there most likely speaks (see
/// `gemini_live::call_lang_plan`). Other countries are left out, so their
/// callers are not taken for speakers of a language they may not know.
const COUNTRY_LANGS: &[(&str, Lang)] = &[
    ("us", Lang::En),
    ("ca", Lang::En),
    ("gb", Lang::En),
    ("ie", Lang::En),
    ("au", Lang::En),
    ("nz", Lang::En),
    ("gg", Lang::En),
    ("im", Lang::En),
    ("je", Lang::En),
    ("sg", Lang::En),
    // English-speaking members of the North American plan (+1).
    ("ag", Lang::En),
    ("ai", Lang::En),
    ("as", Lang::En),
    ("bb", Lang::En),
    ("bm", Lang::En),
    ("bs", Lang::En),
    ("dm", Lang::En),
    ("gd", Lang::En),
    ("gu", Lang::En),
    ("jm", Lang::En),
    ("kn", Lang::En),
    ("ky", Lang::En),
    ("lc", Lang::En),
    ("mp", Lang::En),
    ("ms", Lang::En),
    ("sx", Lang::En),
    ("tc", Lang::En),
    ("tt", Lang::En),
    ("vc", Lang::En),
    ("vg", Lang::En),
    ("vi", Lang::En),
    ("jp", Lang::Ja),
    ("kr", Lang::Ko),
    ("es", Lang::Es),
    ("mx", Lang::Es),
    ("ar", Lang::Es),
    ("co", Lang::Es),
    ("cl", Lang::Es),
    ("pe", Lang::Es),
    ("ve", Lang::Es),
    ("ec", Lang::Es),
    ("bo", Lang::Es),
    ("py", Lang::Es),
    ("uy", Lang::Es),
    ("gt", Lang::Es),
    ("hn", Lang::Es),
    ("sv", Lang::Es),
    ("ni", Lang::Es),
    ("cr", Lang::Es),
    ("pa", Lang::Es),
    ("cu", Lang::Es),
    ("do", Lang::Es),
    ("pr", Lang::Es),
    // France with its overseas departments and territories, Belgium, Monaco.
    ("fr", Lang::Fr),
    ("re", Lang::Fr),
    ("yt", Lang::Fr),
    ("gp", Lang::Fr),
    ("bl", Lang::Fr),
    ("mf", Lang::Fr),
    ("mq", Lang::Fr),
    ("gf", Lang::Fr),
    ("pm", Lang::Fr),
    ("nc", Lang::Fr),
    ("pf", Lang::Fr),
    ("wf", Lang::Fr),
    ("be", Lang::Fr),
    ("mc", Lang::Fr),
    ("de", Lang::De),
    ("at", Lang::De),
    ("ch", Lang::De),
    ("li", Lang::De),
    ("br", Lang::Pt),
    ("pt", Lang::Pt),
    ("cn", Lang::ZhCn),
    ("tw", Lang::ZhTw),
    ("hk", Lang::ZhTw),
    ("mo", Lang::ZhTw),
];

/// The main language of a country (ISO 3166 alpha-2, any case) when the AI
/// speaks it ([`COUNTRY_LANGS`]); other countries: `None`.
pub fn country_lang(iso: &str) -> Option<Lang> {
    let iso = iso.trim();
    COUNTRY_LANGS
        .iter()
        .find(|&&(c, _)| c.eq_ignore_ascii_case(iso))
        .map(|&(_, lang)| lang)
}

/// Between a label and its value in the agent's language ("Number: …", "號碼：…").
pub fn kv_sep() -> &'static str {
    &words(current()).sep
}

/// A phrase template with its `{}` filled in.
pub fn fill(template: &str, value: &str) -> String {
    template.replacen("{}", value, 1)
}

/// `text` in the language's quotation marks.
pub fn quoted(lang: Lang, text: &str) -> String {
    let [open, close] = &words(lang).quote;
    format!("{open}{text}{close}")
}

/// A date and time as notifications show it, in the language's own order:
/// "Nov 13, 08:09", "11月13日 08:09", "13.11. 08:09". The time is whatever
/// zone the caller converted it to (the machine's local time, see `TZ`).
pub fn date_time(lang: Lang, t: &chrono::NaiveDateTime) -> String {
    t.format(&words(lang).dt_fmt).to_string()
}

/// A date alone ("Nov 13", "11月13日", "13.11.").
pub fn date_only(lang: Lang, d: &chrono::NaiveDate) -> String {
    d.format(&words(lang).date_fmt).to_string()
}

/// How long something lasted, the way people say it: seconds under a minute
/// and a half, whole minutes from there ("72 秒", "10 min").
pub fn duration(lang: Lang, secs: f64) -> String {
    let p = words(lang);
    let secs = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
    if secs < 90.0 {
        fill(&p.u_sec, &format!("{secs:.0}"))
    } else {
        fill(&p.u_min, &format!("{:.0}", secs / 60.0))
    }
}

/// Everything language-specific that is a short phrase or a word list.
pub struct Phrases {
    /// English name of the language, for the model's instructions.
    pub name_en: &'static str,
    /// Speech-to-text hint (BCP-47).
    pub stt: &'static str,
    /// How a person answers the phone.
    pub hello: &'static str,
    pub hello_again: &'static str,
    /// Answer to a caller who keeps saying hello.
    pub im_here: &'static str,
    pub still_there: &'static str,
    pub bye: &'static str,
    pub go_ahead: &'static str,
    pub no_thanks: &'static str,
    pub think_about_it: &'static str,
    pub wait_one_more: &'static str,
    /// Lines that make the caller wait or say the AI is looking something up,
    /// in this language (the prompt names them as things not to say).
    pub no_wait: &'static str,
    /// Stand-in when no owner name is set.
    pub owner_fallback: &'static str,
    /// Blocked-number announcement.
    pub unavailable: &'static str,
    /// Voice sample on the settings page (AI's three lines).
    pub preview: [&'static str; 3],
    /// A goodbye when it ends what was said.
    pub farewells: &'static [&'static str],
    /// The caller is done ("that's all") when it ends what they said.
    pub wrapups: &'static [&'static str],
}

pub fn phrases(lang: Lang) -> &'static Phrases {
    match lang {
        Lang::ZhTw => &ZH_TW,
        Lang::ZhCn => &ZH_CN,
        Lang::En => &EN,
        // A language with call speech in the translations uses it; any other
        // reads English.
        Lang::Ext(e) => crate::locales_store::current().phrases(e).unwrap_or(&EN),
        Lang::Ja => &JA,
        Lang::Ko => &KO,
        Lang::Es => &ES,
        Lang::Fr => &FR,
        Lang::De => &DE,
        Lang::Pt => &PT,
    }
}

// The Chinese word lists stay in `gemini_live` (they predate this module and
// carry their own tests); the entries here are only the display phrases.
static ZH_TW: Phrases = Phrases {
    name_en: "Traditional Chinese (Taiwan Mandarin)",
    stt: "zh-TW",
    hello: "喂？",
    hello_again: "喂？你好？",
    im_here: "有，我在，聽得到，請說",
    still_there: "喂？還在嗎？",
    bye: "掰掰",
    go_ahead: "可以啊，你說",
    no_thanks: "喔不用了，謝謝",
    think_about_it: "這個我要再想一下",
    wait_one_more: "欸等一下，我還有一個問題",
    no_wait: "「等一下」「我查一下」「我看一下行程」「稍等」",
    owner_fallback: "機主",
    unavailable: "此號碼目前無法接通，請稍後再撥。",
    preview: [
        "喂？你好。",
        "喔，是包裹喔，好啊，那你先幫我放管理室就好，謝謝喔。",
        "好，掰掰。",
    ],
    farewells: &[],
    wrapups: &[],
};

static ZH_CN: Phrases = Phrases {
    name_en: "Simplified Chinese (Mainland Mandarin)",
    stt: "cmn-CN",
    hello: "喂？",
    hello_again: "喂？你好？",
    im_here: "在的，我在，听得到，你说",
    still_there: "喂？还在吗？",
    bye: "拜拜",
    go_ahead: "可以，你说",
    no_thanks: "哦不用了，谢谢",
    think_about_it: "这个我得再想想",
    wait_one_more: "哎等一下，我还有个问题",
    no_wait: "“等一下”“我查一下”“我看一下行程”“稍等”",
    owner_fallback: "机主",
    unavailable: "您拨打的号码暂时无法接通，请稍后再拨。",
    preview: [
        "喂？你好。",
        "哦，是快递啊，行，那你先帮我放门口就好，谢谢啊。",
        "好，拜拜。",
    ],
    farewells: &[],
    wrapups: &[],
};

static EN: Phrases = Phrases {
    name_en: "English",
    stt: "en-US",
    hello: "Hello?",
    hello_again: "Hello? Hi?",
    im_here: "Yes, I'm here, I can hear you. Go ahead.",
    still_there: "Hello? Are you still there?",
    bye: "Bye",
    go_ahead: "Sure, go ahead",
    no_thanks: "Oh, no thanks",
    think_about_it: "I'd have to think about that",
    wait_one_more: "Oh wait, I've got one more question",
    no_wait: "\"just a sec\", \"give me a moment\", \"let me see\"",
    owner_fallback: "the owner",
    unavailable: "The number you have dialed is not available right now. Please try again later.",
    preview: [
        "Hello?",
        "Oh, a package? Sure, just leave it at the front door, thanks a lot.",
        "Okay, bye.",
    ],
    farewells: &[
        "bye", "bye bye", "bye now", "goodbye", "good bye", "see you", "see ya", "see you later",
        "talk to you later", "talk later", "talk soon", "speak soon", "take care",
        "have a good day", "have a nice day", "have a great day", "have a wonderful day",
        "have a lovely day", "have a good one", "have a good evening", "have a great evening",
        "have a good night", "good night", "cheers", "byebye",
    ],
    wrapups: &[
        "that's all", "that is all", "that's it", "that's everything", "nothing else",
        "i'll let you go", "won't keep you", "gotta go", "got to go", "have to go",
        "i'll call back", "call back later", "i'll try again later",
    ],
};

static JA: Phrases = Phrases {
    name_en: "Japanese",
    stt: "ja-JP",
    hello: "もしもし？",
    hello_again: "もしもし？聞こえますか？",
    im_here: "はい、聞こえてますよ。どうぞ。",
    still_there: "もしもし？聞こえてますか？",
    bye: "失礼します",
    go_ahead: "はい、大丈夫ですよ、どうぞ",
    no_thanks: "あ、結構です",
    think_about_it: "それはちょっと考えさせてください",
    wait_one_more: "あ、すみません、もう一つだけ聞いてもいいですか",
    no_wait: "「ちょっと待って」「確認するから待って」「調べてみるね」「少々お待ちください」",
    owner_fallback: "持ち主",
    unavailable: "おかけになった電話は、現在おつなぎできません。しばらくしてからおかけ直しください。",
    preview: [
        "もしもし？",
        "あ、荷物ですか。じゃあ玄関の前に置いといてください、ありがとうございます。",
        "はい、失礼します。",
    ],
    farewells: &[
        "さようなら", "失礼します", "失礼いたします", "失礼致します", "じゃあね", "じゃあまた",
        "ではまた", "またね", "バイバイ", "ばいばい", "切りますね", "お疲れ様でした",
        "お疲れさまでした",
    ],
    wrapups: &[
        "以上です", "それだけです", "また連絡します", "またご連絡します", "またかけます",
        "かけ直します", "掛け直します",
        // Not 「それでは」 alone: it opens what comes next too (「それでは伝言です」).
        // 「それでは失礼します」「それではまた」 end on their own farewells.
        // Alone, 「よろしくお願いします」 also ends a caller's introduction at the
        // start of a business call: only after 「では」 does it close one.
        "ではよろしくお願いいたします", "ではよろしくお願いします", "じゃあよろしくお願いします",
    ],
};

static KO: Phrases = Phrases {
    name_en: "Korean",
    stt: "ko-KR",
    hello: "여보세요?",
    hello_again: "여보세요? 들리세요?",
    im_here: "네, 듣고 있어요. 말씀하세요.",
    still_there: "여보세요? 아직 계세요?",
    bye: "네, 들어가세요",
    go_ahead: "네, 괜찮아요. 말씀하세요",
    no_thanks: "아, 괜찮습니다",
    think_about_it: "그건 좀 생각해 볼게요",
    wait_one_more: "아 그런데요, 하나만 더 물어볼게요",
    no_wait: "\"잠깐만요\", \"확인해 볼게요\", \"잠시만 기다려 주세요\", \"조금만 기다려 주세요\", \"기다려 줄래?\"",
    owner_fallback: "주인",
    unavailable: "지금은 전화를 받을 수 없습니다. 잠시 후 다시 걸어 주시기 바랍니다.",
    preview: [
        "여보세요?",
        "아, 택배요? 네, 그럼 문 앞에 놔 주세요. 감사합니다.",
        "네, 들어가세요.",
    ],
    farewells: &[
        "안녕히 계세요", "안녕히계세요", "안녕히 가세요", "들어가세요", "끊을게요", "끊겠습니다",
        "수고하세요", "좋은 하루 되세요", "좋은 하루 보내세요", "바이바이", "다음에 봬요", "다음에 뵙겠습니다",
    ],
    wrapups: &[
        "이상입니다", "그게 다예요", "다시 전화드릴게요", "다시 전화드리겠습니다",
        "다시 연락드릴게요", "다시 연락드리겠습니다",
    ],
};

static ES: Phrases = Phrases {
    name_en: "Spanish",
    stt: "es-ES",
    hello: "¿Sí? ¿Diga?",
    hello_again: "¿Hola? ¿Me oye?",
    im_here: "Sí, estoy aquí, le oigo bien. Dígame.",
    still_there: "¿Hola? ¿Sigue ahí?",
    bye: "Adiós",
    go_ahead: "Sí, claro, dígame",
    no_thanks: "Ah, no, gracias",
    think_about_it: "Eso tendría que pensarlo",
    wait_one_more: "Ay, una cosa más, tengo otra pregunta",
    no_wait: "\"un momento\", \"espera, que lo miro\", \"déjame ver\"",
    owner_fallback: "el dueño",
    unavailable: "El número al que llama no está disponible en este momento. Por favor, inténtelo más tarde.",
    preview: [
        "¿Sí? ¿Diga?",
        "Ah, ¿un paquete? Vale, déjelo en la puerta, muchas gracias.",
        "Vale, adiós.",
    ],
    farewells: &[
        "adiós", "adios", "hasta luego", "hasta pronto", "hasta mañana", "nos vemos", "chao",
        "chau", "que tenga buen día", "que tenga un buen día", "que tengas buen día",
        "que tenga un excelente día", "que tengas un excelente día", "que tenga excelente día",
        "que tenga un lindo día", "que tengas un lindo día", "que tenga un feliz día",
        "que tengas un feliz día", "que tenga buena tarde", "que tenga una buena tarde",
        "que tenga excelente tarde", "cuídese", "cuídate", "un saludo", "órale gracias", "orale gracias",
    ],
    wrapups: &[
        "eso es todo", "nada más", "eso sería todo", "le dejo", "te dejo", "tengo que irme",
        "me tengo que ir", "le llamo luego", "vuelvo a llamar",
    ],
};

static FR: Phrases = Phrases {
    name_en: "French",
    stt: "fr-FR",
    hello: "Allô ?",
    hello_again: "Allô ? Vous m'entendez ?",
    im_here: "Oui, je suis là, je vous entends bien. Allez-y.",
    still_there: "Allô ? Vous êtes toujours là ?",
    bye: "Au revoir",
    go_ahead: "Oui, bien sûr, allez-y",
    no_thanks: "Ah non, merci",
    think_about_it: "Ça, il faudrait que j'y réfléchisse",
    wait_one_more: "Ah, attendez, j'ai encore une question",
    no_wait: "« un instant », « attendez, je regarde », « laissez-moi vérifier »",
    owner_fallback: "le propriétaire",
    unavailable: "Le numéro que vous avez composé n'est pas disponible pour le moment. Veuillez rappeler ultérieurement.",
    preview: [
        "Allô ?",
        "Ah, un colis ? D'accord, laissez-le devant la porte, merci beaucoup.",
        "D'accord, au revoir.",
    ],
    farewells: &[
        "au revoir", "à bientôt", "a bientot", "à plus", "à plus tard", "à tout à l'heure",
        "à demain", "bonne journée", "bonne journee", "excellente journée", "très bonne journée",
        "bonne soirée", "bonne fin de journée", "ciao",
    ],
    wrapups: &[
        "c'est tout", "rien d'autre", "je vous laisse", "je te laisse", "je vous laisse à vos occupations",
        "je te laisse à tes occupations", "je dois y aller",
        "je rappellerai", "je vous rappelle",
    ],
};

static DE: Phrases = Phrases {
    name_en: "German",
    stt: "de-DE",
    hello: "Hallo?",
    hello_again: "Hallo? Hören Sie mich?",
    im_here: "Ja, ich bin dran, ich höre Sie gut. Bitte.",
    still_there: "Hallo? Sind Sie noch dran?",
    bye: "Tschüss",
    go_ahead: "Ja klar, schießen Sie los",
    no_thanks: "Ach nein, danke",
    think_about_it: "Das müsste ich mir noch überlegen",
    wait_one_more: "Ach, eine Frage hab ich noch",
    no_wait: "„Moment mal“, „warte, ich schau nach“, „lass mich kurz nachsehen“",
    owner_fallback: "der Besitzer",
    unavailable: "Der gewünschte Gesprächspartner ist zurzeit nicht erreichbar. Bitte versuchen Sie es später noch einmal.",
    preview: [
        "Hallo?",
        "Ach, ein Paket? Gut, stellen Sie es einfach vor die Tür, vielen Dank.",
        "Okay, tschüss.",
    ],
    farewells: &[
        "tschüss", "tschüs", "tschuess", "auf wiederhören", "auf wiedersehen", "wiederhören",
        "bis bald", "bis später", "bis dann", "schönen tag noch", "schönen tag", "einen schönen tag",
        "schönen abend noch", "mach's gut", "machs gut", "ciao",
    ],
    wrapups: &[
        "das war's", "das wars", "das wäre alles", "das ist alles", "sonst nichts",
        "ich muss los", "ich muss auflegen", "ich melde mich wieder", "ich rufe später an",
    ],
};

static PT: Phrases = Phrases {
    name_en: "Portuguese",
    stt: "pt-BR",
    hello: "Alô?",
    hello_again: "Alô? Está me ouvindo?",
    im_here: "Sim, estou aqui, estou ouvindo bem. Pode falar.",
    still_there: "Alô? Ainda está aí?",
    bye: "Tchau",
    go_ahead: "Claro, pode falar",
    no_thanks: "Ah, não, obrigado",
    think_about_it: "Isso eu teria que pensar",
    wait_one_more: "Ah, espera, tenho mais uma pergunta",
    no_wait: "\"só um momento\", \"espera que eu vou ver\", \"deixa eu verificar\"",
    owner_fallback: "o dono",
    unavailable: "O número chamado não está disponível no momento. Por favor, tente novamente mais tarde.",
    preview: [
        "Alô?",
        "Ah, uma encomenda? Tá bom, pode deixar na porta, muito obrigado.",
        "Tá bom, tchau.",
    ],
    farewells: &[
        "tchau", "adeus", "até logo", "até mais", "até breve", "até amanhã", "tenha um bom dia",
        "tenha um ótimo dia", "tenha uma boa tarde", "tenha uma ótima tarde",
        "tenha uma boa noite", "um abraço",
    ],
    wrapups: &[
        "é só isso", "só isso", "mais nada", "é tudo", "tenho que ir", "preciso desligar",
        "ligo depois", "volto a ligar",
    ],
};

/// Languages whose word lists are matched as whole words (spaces between words).
const WORD_LANGS: [Lang; 5] = [Lang::En, Lang::Es, Lang::Fr, Lang::De, Lang::Pt];
/// Languages written without spaces: matched as substrings.
const RUN_LANGS: [Lang; 2] = [Lang::Ja, Lang::Ko];

/// Lowercase of the text. Rust lowers a Turkish capital İ to "i" plus a combining
/// dot (U+0307), so "İyi" would never match "iyi": the dot is dropped.
fn lowercase(text: &str) -> String {
    text.to_lowercase().replace('\u{307}', "")
}

/// Lowercase words of `text`, punctuation dropped (apostrophes kept inside words).
fn word_list(text: &str) -> Vec<String> {
    lowercase(text)
        .replace('’', "'")
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .map(|w| w.trim_matches('\'').to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Punctuation that ends a clause: what stands between "No" and "that's all" in
/// "No, that's all", and before a closing "falou" in "Certo, falou".
const CLAUSE_MARKS: &str = ",.;:!?…，。；：！？、";

/// The same words as [`words`], and for each of them whether one of
/// [`CLAUSE_MARKS`] stands between it and the word before.
fn words_after_marks(text: &str) -> (Vec<String>, Vec<bool>) {
    let mut ws = Vec::new();
    let mut after_mark = Vec::new();
    let mut word = String::new();
    let mut mark = false;
    for c in lowercase(text).replace('’', "'").chars().chain(std::iter::once(' ')) {
        if c.is_alphanumeric() || c == '\'' {
            word.push(c);
            continue;
        }
        let w = word.trim_matches('\'');
        if !w.is_empty() {
            ws.push(w.to_string());
            after_mark.push(mark);
            mark = false;
        }
        word.clear();
        mark |= CLAUSE_MARKS.contains(c);
    }
    (ws, after_mark)
}

/// `phrase` (space-separated words) ends `words`, with at most `slack` words after it.
/// `after_mark`: where the punctuation was ([`words_after_marks`]).
fn phrase_near_end(words: &[String], after_mark: &[bool], phrase: &str, slack: usize) -> bool {
    let p = self::word_list(phrase);
    if p.is_empty() || words.len() < p.len() {
        return false;
    }
    let last_start = words.len() - p.len();
    let first_start = last_start.saturating_sub(slack);
    let key = p.join(" ");
    // "Hace tiempo que no nos vemos" (we haven't seen each other in ages) is
    // no goodbye: a negation right before the phrase rules it out. The "no" of
    // "No, that's all" is an answer, not a negation.
    let negated = |i: usize| {
        let direct = i > 0
            && NEGATIONS.contains(&words[i - 1].as_str())
            && !no_is_the_answer(&key, &words[i - 1], after_mark.get(i).copied().unwrap_or(false));
        // "Não é só isso" (that is not all): the negation stands before the "é".
        let over_the_verb = i > 1
            && words[i - 1] == "é"
            && ["não", "nao"].contains(&words[i - 2].as_str())
            && !after_mark.get(i - 1).copied().unwrap_or(false);
        direct || over_the_verb
    };
    // "Say bye to grandma" is said to someone else on the caller's side.
    let to_someone_else = |i: usize| {
        let after = &words[i + p.len()..];
        after.first().is_some_and(|w| w == "to")
            && after.get(1).is_some_and(|w| !["you", "y'all", "all", "everyone", "everybody"].contains(&w.as_str()))
    };
    (first_start..=last_start).any(|i| {
        words[i..i + p.len()] == p[..]
            && !negated(i)
            && !to_someone_else(i)
            && followed_as_a_goodbye(&key, &words[i + p.len()..])
    })
}

/// Asked "anything else?", callers answer "No, that's all" / "No, nada más":
/// the "no" before these is their answer, not a negation.
const NOTHING_MORE: &[&str] = &[
    "that's all", "that is all", "that's it", "that's everything", "nothing else", "nada más",
    "eso es todo", "eso sería todo", "mais nada",
];
/// The same in Portuguese, where these words also follow a real negation
/// ("Isso não é tudo", "Não só isso": there is more to come). The "não" is the
/// answer only with a comma or a full stop after it: "Não, é só isso".
const NOTHING_MORE_AFTER_A_MARK: &[&str] = &["é só isso", "só isso", "é tudo"];

/// The "no" (`word_before`) right before the phrase `key` answers a question
/// rather than negating it. `mark_between`: punctuation stands between the two.
fn no_is_the_answer(key: &str, word_before: &str, mark_between: bool) -> bool {
    ["no", "não", "nao"].contains(&word_before)
        && (NOTHING_MORE.contains(&key) || (mark_between && NOTHING_MORE_AFTER_A_MARK.contains(&key)))
}

/// What is left with someone, or what they are left to do, after "je vous
/// laisse" / "je te laisse": "Je vous laisse mon numéro", "Je vous laisse réfléchir".
const FR_LEFT_WITH: &[&str] = &[
    "mon", "ma", "mes", "un", "une", "le", "la", "les", "du", "des", "ce", "cet", "cette", "ces", "votre",
    "vos", "ton", "ta", "tes", "son", "sa", "ses", "notre", "nos", "leur", "quelques", "réfléchir", "voir",
    "faire", "choisir", "décider", "regarder", "vérifier", "parler", "finir", "continuer", "y", "deux", "trois",
    "quatre", "cinq", "six", "dix", "vingt", "plusieurs", "tout", "toutes",
];
/// The same after "le dejo" / "te dejo": "Te dejo mi número", "Le dejo el paquete".
const ES_LEFT_WITH: &[&str] = &[
    "el", "la", "los", "las", "un", "una", "unos", "unas", "mi", "mis", "tu", "tus", "su", "sus", "este",
    "esta", "esto", "eso", "aquí", "todo", "dicho", "saber", "pensar", "ver", "elegir", "decidir", "dos", "tres",
    "cuatro", "cinco", "diez", "veinte", "varios", "varias", "algunos", "algunas",
];

/// Phrases that are no goodbye when one of these words comes right after them:
/// "I'll take care of it", "That's all right", "C'est tout à fait", "Je vous
/// laisse mon numéro", "Na razie nie wiem" ("I don't know yet"). Any other word
/// after them — a name, a title, a particle, another goodbye — leaves them one:
/// "Je vous laisse, madame", "Das ist alles, Herr Müller", "No to na razie, Ania".
const CANCELLED_BY: &[(&str, &[&str])] = &[
    ("take care", &["of"]),
    ("that's all", &["right", "good", "fine", "true", "wrong"]),
    ("that is all", &["right", "good", "fine", "true", "wrong"]),
    ("c'est tout", &["à", "près", "de", "ce", "le", "la"]),
    ("je vous rappelle", &["que"]),
    (
        "nada más",
        &["quería", "queria", "quiero", "para", "que", "llamaba", "llamo", "llamar", "un", "una", "le", "te"],
    ),
    ("je vous laisse", FR_LEFT_WITH),
    ("je te laisse", FR_LEFT_WITH),
    ("à plus", &["de", "cher", "ou"]),
    // "Ciao, sono Marco" is a greeting: the words that go on after it.
    (
        "ciao",
        &[
            "sono", "mi", "chiamo", "volevo", "vorrei", "ho", "sei", "senti", "scusa", "scusi", "posso", "parlo", "ti", "chi", "come",
            "c'è", "dimmi", "dammi", "ascolta", "dove", "quando", "hai", "puoi", "è",
        ],
    ),
    ("le dejo", ES_LEFT_WITH),
    ("te dejo", ES_LEFT_WITH),
    (
        "das ist alles",
        &[
            "sehr", "zu", "ganz", "ziemlich", "total", "völlig", "nicht", "noch", "nur", "was", "in", "im",
            "kein", "keine", "quatsch", "unsinn", "falsch", "richtig", "teuer", "kompliziert", "schwierig",
            "egal", "bereits", "schon", "gelogen", "bezahlt", "erledigt", "andere", "anders", "gut", "klar",
            "wahr", "neu", "kaputt", "weg", "fertig", "möglich",
        ],
    ),
    (
        "é tudo",
        &[
            "verdade", "mentira", "que", "o", "isso", "muito", "culpa", "igual", "novo", "diferente", "a",
            "um", "uma", "meu", "minha", "bem", "errado", "caro", "barato", "mais", "tão", "tao",
        ],
    ),
    (
        "na razie",
        &[
            "nie", "nic", "to", "jest", "tak", "tylko", "mam", "jestem", "wiem", "mogę", "będę", "bez",
            "wszystko", "w", "na", "z", "po", "się", "jeszcze", "mi", "nam", "brak", "nikogo", "czekam",
            "czekamy", "są", "sa", "był", "byl",
        ],
    ),
];

/// The same for a next word that starts like this: "Je vous laisse l'adresse",
/// "à plus d'un titre".
const CANCELLED_BY_START: &[(&str, &[&str])] = &[
    ("je vous laisse", &["l'", "d'"]),
    ("je te laisse", &["l'", "d'"]),
    ("à plus", &["d'"]),
];

/// The words after a phrase (its words joined by spaces: key) leave it a goodbye.
fn followed_as_a_goodbye(key: &str, after: &[String]) -> bool {
    let Some(next) = after.first() else {
        return true;
    };
    // "Take care of yourself" is the goodbye itself.
    if key == "take care" && next == "of" && after.get(1).is_some_and(|w| w == "yourself" || w == "yourselves") {
        return true;
    }
    // "Je vous laisse 20 euros", "Le dejo 3 paquetes": something is left, in figures.
    if matches!(key, "je vous laisse" | "je te laisse" | "le dejo" | "te dejo") && next.starts_with(|c: char| c.is_ascii_digit()) {
        return false;
    }
    !CANCELLED_BY.iter().any(|(k, ws)| *k == key && ws.contains(&next.as_str()))
        // "Je vous laisse, d'accord ?" asks nothing to be left: still a goodbye.
        && (next == "d'accord"
            || !CANCELLED_BY_START.iter().any(|(k, starts)| *k == key && starts.iter().any(|s| next.starts_with(s))))
}

/// Words that turn a following goodbye phrase into something else.
const NEGATIONS: &[&str] = &[
    "no", "not", "never", "nunca", "pas", "nicht", "nie", "não", "nao",
    // Vietnamese, Indonesian ("đừng tạm biệt vội", "jangan dadah dulu")
    "không", "khong", "đừng", "chưa", "chẳng", "jangan", "tidak", "tak", "belum", "bukan",
];

/// `marker` sits within `slack` characters of the end of a text without spaces.
fn run_near_end(compact: &str, marker: &str, slack: usize) -> bool {
    let m: String = marker.chars().filter(|c| c.is_alphanumeric()).collect();
    compact
        .rfind(&m)
        .is_some_and(|i| compact[i + m.len()..].chars().count() <= slack)
}

fn ends_on(text: &str, pick: fn(&Phrases) -> &'static [&'static str], slack_words: usize, slack_chars: usize) -> bool {
    let (ws, after_mark) = words_after_marks(text);
    if WORD_LANGS
        .iter()
        .any(|l| pick(phrases(*l)).iter().any(|m| phrase_near_end(&ws, &after_mark, m, slack_words)))
    {
        return true;
    }
    let compact: String = text.chars().filter(|c| c.is_alphanumeric()).collect();
    RUN_LANGS
        .iter()
        .any(|l| pick(phrases(*l)).iter().any(|m| run_near_end(&compact, m, slack_chars)))
}

fn is_question(text: &str) -> bool {
    let t = text.trim_end();
    t.ends_with('?') || t.ends_with('？') || t.ends_with("か。") || t.ends_with("까요")
}

/// Thanks that people add after their goodbye or wrap-up (「好的，那就先這樣，謝謝您」,
/// "That's all, thank you so much"). Longer ones first: "muchas gracias" before
/// "gracias", so nothing of them is left behind.
const THANKS: &[&str] = &[
    "thank you so much", "thank you very much", "thanks so much", "thanks a lot", "many thanks",
    "thank you", "thanks", "thx",
    "muchísimas gracias", "muchas gracias", "gracias", "merci beaucoup", "merci bien", "merci",
    "vielen dank", "danke schön", "danke schon", "dankeschön", "danke",
    "muito obrigado", "muito obrigada", "obrigado", "obrigada", "valeu",
    "grazie mille", "molte grazie", "mille grazie", "grazie tante", "grazie infinite", "grazie",
    "どうもありがとうございました", "どうもありがとうございます", "ありがとうございました",
    "ありがとうございます", "ありがとう",
    "감사합니다", "고맙습니다", "감사해요", "고마워요",
    "謝謝您", "謝謝你", "謝謝妳", "謝謝", "谢谢您", "谢谢你", "谢谢", "感謝您", "感謝你", "感謝",
    "感谢您", "感谢你", "感谢", "多謝", "多谢",
    "cảm ơn", "cám ơn", "terima kasih", "makasih",
    "большое спасибо", "спасибо",
    "شكرا جزيلا", "شكراً جزيلاً", "شكرا لك", "شكراً لك", "شكرا", "شكراً",
    "बहुत धन्यवाद", "बहुत शुक्रिया", "धन्यवाद", "शुक्रिया",
    "çok teşekkür ederim", "teşekkür ederim", "teşekkürler", "sağ olun", "sağ ol", "sağolun", "sağol",
];

fn is_latin_letter(c: char) -> bool {
    c.is_ascii_alphabetic() || ('\u{00C0}'..='\u{024F}').contains(&c) || ('\u{1E00}'..='\u{1EFF}').contains(&c)
}

/// `text` without `suffix` (lowercase) at its end, compared without case; a
/// Latin suffix must be a whole word ("thanks", not the end of another word).
fn strip_suffix_nocase<'a>(text: &'a str, suffix: &str) -> Option<&'a str> {
    let n = suffix.chars().count();
    if n == 0 {
        return None;
    }
    let (start, _) = text.char_indices().rev().nth(n - 1)?;
    if lowercase(&text[start..]) != suffix {
        return None;
    }
    let head = &text[..start];
    let latin = suffix.chars().next().is_some_and(is_latin_letter);
    if latin && head.chars().next_back().is_some_and(char::is_alphanumeric) {
        return None;
    }
    Some(head)
}

fn trim_end_marks(t: &str) -> &str {
    t.trim_end_matches(|c: char| !c.is_alphanumeric())
}

/// What was said, without the thanks (and punctuation) at its end: the words
/// before them decide whether the caller is leaving. Thanks alone leave nothing
/// — a bare 「謝謝」 is no goodbye.
pub fn strip_trailing_thanks(text: &str) -> &str {
    let mut t = trim_end_marks(text);
    'again: loop {
        for thanks in THANKS {
            if let Some(head) = strip_suffix_nocase(t, thanks) {
                t = trim_end_marks(head);
                continue 'again;
            }
        }
        return t;
    }
}

/// "Hold on", "one moment", 「等一下，我去拿」: the caller is leaving the phone for a
/// while or putting us through, so a long quiet line is expected. Matched
/// anywhere in what they said: Chinese as requests only ([`zh_asks_to_hold`]),
/// Japanese, Korean and Thai as runs of letters, the others as whole words.
const HOLD_RUNS: &[&str] = &[
    "少々お待ち", "ちょっと待って", "お待ちください",
    "잠시만요", "잠깐만요", "잠시만기다려",
    "รอสักครู่", "รอแป๊บ", "แป๊บนึง",
];
const HOLD_WORDS: &[&str] = &[
    "hold on", "one moment", "just a sec", "just a second", "let me check", "hang on",
    "un momento", "espere", "um momento", "só um minuto", "so um minuto", "un instant",
    "ne quittez pas", "einen moment", "bleiben sie dran",
    "chờ chút", "đợi chút", "chờ một chút", "đợi một chút", "cho chut",
    "tunggu sebentar", "sebentar ya", "mohon tunggu",
];

pub fn asks_to_hold(text: &str) -> bool {
    if zh_asks_to_hold(text) {
        return true;
    }
    let runs = squeeze(text);
    if HOLD_RUNS.iter().any(|m| runs.contains(squeeze(m).as_str())) {
        return true;
    }
    let ws = word_list(text);
    HOLD_WORDS.iter().any(|m| has_phrase(&ws, m))
}

/// Chinese asking us to hold on. 「等一下」 is also "later" (「我等一下要出門」「那我等
/// 一下再打給你」): it counts only as a request — in one of these forms, or on
/// its own (「好，等一下。」).
const ZH_HOLD_REQUESTS: &[&str] = &[
    "等一下喔", "等一下哦", "等一下下", "等我一下", "你等一下", "您等一下", "妳等一下", "請等一下",
    "稍等", "我查一下", "我去拿", "幫你轉", "幫您轉", "轉接", "等等喔", "等等哦",
];

fn zh_asks_to_hold(text: &str) -> bool {
    let t: String = crate::zh_tw::norm(text).chars().filter(|c| !c.is_whitespace()).collect();
    if ZH_HOLD_REQUESTS.iter().any(|m| t.contains(m)) {
        return true;
    }
    ["等一下", "等等"].iter().any(|m| {
        t.match_indices(m).any(|(i, _)| {
            let before = t[..i].chars().next_back();
            let after = t[i + m.len()..].chars().next();
            // Nothing of the sentence around it but a lead-in like 「好，」「那」.
            after.map_or(true, |c| !c.is_alphanumeric())
                && before.map_or(true, |c| !c.is_alphanumeric() || "好嗯欸喂啊那".contains(c))
        })
    })
}

// Goodbyes in languages the AI has no phrase set for but answers a caller in:
// Vietnamese, Thai, Indonesian, Russian, Arabic, Hindi. [`ends_with_farewell`]
// checks them next to the nine languages' own lists — for the caller's words and
// for the AI's own (its goodbye in the caller's language starts the wait to hang up).

/// Vietnamese, Indonesian, Russian, Arabic and Hindi goodbyes, whole words near
/// the end. Vietnamese also without its tone marks: speech-to-text sometimes
/// drops them. Arabic with and without its hamza and tanween, as speech-to-text
/// writes it either way.
const EXTRA_WORD_FAREWELLS: &[&str] = &[
    "tạm biệt", "tam biet", "hẹn gặp lại", "hen gap lai", "chào nhé", "chao nhe",
    // "That's it then": how many Vietnamese calls end.
    "thế nhé", "vậy nhé", "thôi nhé", "thoi nhe", "bye nhé", "bye nhe",
    // "(Have) a good day": "Chúc anh một ngày tốt lành".
    "một ngày tốt lành", "ngày tốt lành",
    // …a good afternoon / morning / evening: "Chúc anh một buổi chiều tốt lành".
    "buổi chiều tốt lành", "buổi sáng tốt lành", "buổi tối tốt lành", "chiều tốt lành", "tối tốt lành",
    "sampai jumpa", "sampai ketemu", "selamat tinggal", "dadah", "daah",
    "sampai nanti", "sudah dulu ya", "udah dulu ya", "segitu dulu ya",
    // Russian: "goodbye", "all the best", "have a good day", "see you", "bye-bye".
    "до свидания", "до свиданья", "всего доброго", "всего хорошего", "хорошего дня",
    "хорошего вечера", "до встречи", "до связи", "пока пока",
    // Arabic: "with safety", "until we meet", "in God's safety", "farewell", "a happy day".
    "مع السلامة", "مع السلامه", "إلى اللقاء", "الى اللقاء", "إلي اللقاء", "في أمان الله",
    "في امان الله", "وداعا", "وداعاً", "يوما سعيدا", "يوماً سعيداً", "باي باي",
    // Hindi: "farewell", "we'll meet again", "talk later", "have a good day", "bye-bye"
    // (also in Latin letters, as speech-to-text writes it at times).
    "अलविदा", "फिर मिलेंगे", "फिर मिलते हैं", "फिर बात करते हैं", "बाद में बात करते हैं",
    "आपका दिन शुभ हो", "बाय बाय", "alvida", "phir milenge", "phir milte hain",
    // Italian / Spanish / French / German / Korean / Cantonese additions
    "arrivederci", "arrivederla", "buona giornata", "buongiornata", "buonanotte", "grazie a lei",
    "stammi bene", "stia bene", "a domani", "ci vediamo",
    "buona serata", "a presto", "ci sentiamo", "ci risentiamo", "a risentirci", "buon proseguimento",
    "buona continuazione", "ciao ciao",
    "que le vaya bien", "que te vaya bien", "que tenga buen día", "que tengas buen día",
    "je raccroche", "bonne continuation", "gute nacht",
    "잘 가", "잘 가요", "끊을게", "끊을게요", "得閒再傾", "得闲再倾", "收線啦", "收线啦",
    // Dutch / Polish / Romanian / Ukrainian / Bengali / Marathi / Tamil / Telugu
    "tot ziens", "tot spreeks", "doei", "fijne dag nog",
    "do widzenia", "na razie", "do usłyszenia", "do uslyszenia", "miłego dnia", "milego dnia",
    "la revedere", "o zi bună", "o zi buna", "toate cele bune", "pe curând", "pe curand",
    "до побачення", "на все добре", "гарного дня", "бувайте", "до зустрічі",
    "বিদায়", "আল্লাহ হাফেজ", "ভালো থাকবেন", "আবার কথা হবে", "শুভ দিন",
    "नमस्कार पुन्हा बोलू", "काळजी घ्या", "शुभ दिवस", "आता ठेवतो", "बाय बाय",
    // Tamil / Telugu "I am coming" (வருகிறேன், వస్తాను) is no goodbye: only "I'll go and come".
    "போய் வருகிறேன்", "போயிட்டு வருகிறேன்", "போயிட்டு வரேன்", "பை பை", "பிறகு பேசலாம்", "நல்ல நாள்",
    "వెళ్ళొస్తాను", "వెళ్లొస్తాను", "సరే ఉంటాను", "ఇక ఉంటాను", "బై బై", "మళ్ళీ మాట్లాడుదాం", "శుభ దినం",
    // Turkish
    "hoşça kal", "hoşça kalın", "hoşçakal", "hoşçakalın", "görüşürüz", "görüşmek üzere", "iyi geceler",
    "kendine iyi bak", "kendinize iyi bakın", "güle güle", "allaha ısmarladık", "bay bay",
];

/// A goodbye that also opens calls: "chào anh", "selamat pagi", "sawasdee ka".
/// It counts only as the very last words, and only right after a closing cue
/// ("Cảm ơn anh, chào anh.", see [`cue_window`]) or with a word after it that only
/// a goodbye has ("chào anh nhé") — so a caller's "Alo, chào anh." at the start of
/// a call is no goodbye.
struct GreetingBye {
    heads: &'static [&'static str],
    /// Words that may follow it (whom it is said to, polite particles), at most
    /// [`GREETING_BYE_TAIL`] of them.
    tail: &'static [&'static str],
    /// Words after it that make it a goodbye by themselves.
    closing_tail: &'static [&'static str],
    /// Words just before it that mark the end of a call.
    cues: &'static [&'static str],
}

const GREETING_BYE_TAIL: usize = 3;
/// A closing cue counts within this many words before the goodbye: the caller's
/// text is the whole window since the AI last spoke, and an "ok" from earlier on
/// belongs to something else.
const GREETING_BYE_CUE_WORDS: usize = 6;
/// How a call is answered: a closing cue before one of these belongs to an
/// earlier talk ("…nhé. Alo, em chào anh ạ." — the phone was handed over).
const HELLOS: &[&str] = &["alo", "a lô", "a lo", "allo", "halo", "hallo", "hello", "helo", "喂"];

const GREETING_BYES: &[GreetingBye] = &[
    // Vietnamese: "Cảm ơn anh, chào anh." "Thôi chào em nhé." "Cảm ơn, chào."
    GreetingBye {
        heads: &["chào", "chao"],
        tail: &[
            "anh", "chị", "chi", "em", "bạn", "ban", "cô", "co", "chú", "chu", "ông", "ong", "bà",
            "ba", "cháu", "chau", "con", "cả", "ca", "nhà", "mọi", "moi", "người", "nguoi", "ạ", "a",
            "nhé", "nhe", "nhá", "nha",
        ],
        closing_tail: &["nhé", "nhe", "nhá", "nha"],
        // Not "dạ" / "vâng": those open a call too ("Alo, dạ chào anh ạ").
        cues: &[
            "cảm ơn", "cám ơn", "cam on", "thôi", "thoi", "hiểu rồi", "được rồi", "ok", "oke", "okay", "thanks",
            "thank you",
        ],
    },
    // Indonesian: "Baik, terima kasih, selamat siang."
    // Speech-to-text set to Chinese writes "terima kasih" as 「謝謝」.
    GreetingBye {
        heads: &["selamat pagi", "selamat siang", "selamat sore", "selamat malam"],
        tail: &["ya", "pak", "bu", "bapak", "ibu", "mas", "mbak", "kak", "semua"],
        closing_tail: &[],
        cues: &[
            "terima kasih", "trima kasih", "makasih", "trims", "itu saja", "sudah dulu", "ya sudah",
            "segitu dulu", "ok", "oke", "okay", "thanks", "thank you", "謝謝", "谢谢",
        ],
    },
    // Thai as speech-to-text sometimes writes it, in Latin letters.
    GreetingBye {
        heads: &["sawasdee", "sawatdee", "sawadee", "sawaddee"],
        tail: &["ka", "kha", "krub", "krab", "khrap", "krap", "kub", "na"],
        closing_tail: &[],
        cues: &[
            "khob khun", "khob kun", "kob kun", "khop khun", "kop khun", "ok", "okay", "thanks",
            "thank you",
        ],
    },
    // Russian "bye" (пока) is also "while / for now" (「Я пока не знаю」): only as
    // the last words, right after a closing cue ("Ладно, спасибо, пока!").
    GreetingBye {
        heads: &["пока"],
        tail: &["всем", "тебе", "вам"],
        closing_tail: &[],
        cues: &[
            "спасибо", "всё", "все", "ладно", "хорошо", "давай", "давайте", "ну", "ок", "окей", "ok",
            "okay", "thanks", "thank you",
        ],
    },
    // Hindi "hello / goodbye" (नमस्ते, नमस्कार) and "bye" (बाय): a goodbye only
    // right after thanks or "okay" ("धन्यवाद, नमस्ते").
    GreetingBye {
        heads: &["नमस्ते", "नमस्कार", "बाय"],
        tail: &["जी"],
        closing_tail: &[],
        cues: &[
            "धन्यवाद", "शुक्रिया", "ठीक है", "अच्छा", "ok", "okay", "thanks", "thank you", "dhanyavaad",
            "shukriya",
        ],
    },
    // Korean casual "안녕" / Spanish "buen día": only counts as farewell after a closing cue.
    GreetingBye {
        heads: &["안녕", "buen día", "buen dia"],
        tail: &[],
        closing_tail: &[],
        cues: &["감사합니다", "고마워요", "알겠습니다", "gracias", "ok", "okay", "thanks", "thank you"],
    },
    // Tamil / Telugu polite greeting-farewells.
    GreetingBye {
        heads: &["வணக்கம்", "నమస్కారం"],
        tail: &[],
        closing_tail: &[],
        cues: &["நன்றி", "సరే", "ధన్యవాదాలు", "ok", "okay", "thanks", "thank you"],
    },
    // Turkish: "Tamam, teşekkürler, iyi günler."
    GreetingBye {
        heads: &["iyi günler", "iyi akşamlar", "kolay gelsin"],
        tail: &["efendim", "hanım", "bey"],
        closing_tail: &[],
        cues: &["teşekkürler", "teşekkür ederim", "sağ olun", "sağ ol", "tamam", "peki", "ok", "okay"],
    },
    // Italian: "Grazie, ciao." "Va bene, grazie, buonasera." These open calls too, so only after a closing cue.
    GreetingBye {
        heads: &["ciao", "buongiorno", "buonasera", "buon pomeriggio"],
        tail: &["a", "tutti", "signora", "signore", "ciao"],
        closing_tail: &[],
        cues: &["grazie", "va bene", "perfetto", "benissimo", "ok", "okay", "thanks", "thank you"],
    },
];

impl GreetingBye {
    fn ends(&self, ws: &[String]) -> bool {
        self.heads.iter().any(|head| {
            let h = word_list(head);
            if h.is_empty() || ws.len() < h.len() {
                return false;
            }
            let last = ws.len() - h.len();
            (last.saturating_sub(GREETING_BYE_TAIL)..=last).any(|i| {
                let after = &ws[i + h.len()..];
                ws[i..i + h.len()] == h[..]
                    && !(i > 0 && NEGATIONS.contains(&ws[i - 1].as_str()))
                    && after.iter().all(|w| self.tail.contains(&w.as_str()))
                    && (after.iter().any(|w| self.closing_tail.contains(&w.as_str()))
                        || self.cues.iter().any(|c| has_cue(cue_window(&ws[..i]), c)))
            })
        })
    }
}

/// Where a closing cue for a goodbye after `before` may be: its last
/// [`GREETING_BYE_CUE_WORDS`] words, and only after the last of [`HELLOS`].
fn cue_window(before: &[String]) -> &[String] {
    let mut start = before.len().saturating_sub(GREETING_BYE_CUE_WORDS);
    for h in HELLOS {
        let p = word_list(h);
        if p.is_empty() || before.len() < p.len() {
            continue;
        }
        if let Some(j) = (0..=before.len() - p.len())
            .rev()
            .find(|&j| before[j..j + p.len()] == p[..])
        {
            start = start.max(j + p.len());
        }
    }
    &before[start..]
}

/// `phrase` (space-separated words) appears anywhere in `ws`.
fn has_phrase(ws: &[String], phrase: &str) -> bool {
    let p = word_list(phrase);
    !p.is_empty() && ws.windows(p.len()).any(|w| w == &p[..])
}

/// A closing cue among `ws`: as whole words, or, for a cue in Chinese
/// characters, inside a word (Chinese is written without spaces: 「那謝謝」).
fn has_cue(ws: &[String], cue: &str) -> bool {
    let han = !cue.is_empty() && cue.chars().all(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c));
    has_phrase(ws, cue) || (han && ws.iter().any(|w| w.contains(cue)))
}

/// Thai goodbyes (Thai is written without spaces: matched in the squeezed text).
const TH_FAREWELLS: &[&str] = &[
    "ลาก่อน", "บ๊ายบาย", "บายบาย", "บ๊ายบ๊าย",
    // "Talk again later", "that's all for now".
    "ไว้คุยกันใหม่", "ไว้ค่อยคุยกัน", "แค่นี้ก่อน", "แค่นี้นะ", "วางสายนะ", "แล้วคุยกันใหม่", "แล้วค่อยคุยกัน",
    "แล้วพบกันใหม่", "พบกันใหม่",
    // "See you (again)".
    "ไว้เจอกัน", "ไว้เจอกันใหม่",
    // "Good luck", "have a good day": how the AI's own Thai calls end.
    "โชคดี", "ขอให้มีวันที่ดี",
];
/// Thai "hello / goodbye" (สวัสดีค่ะ), also as Chinese speech-to-text writes it:
/// a goodbye only right after one of [`TH_CUES`] (see [`thai_cue_window`]).
const TH_GREETING_BYES: &[&str] = &["สวัสดี", "沙瓦迪卡", "薩瓦迪卡", "萨瓦迪卡"];
const TH_CUES: &[&str] = &["ขอบคุณ", "แค่นี้", "ไปก่อน", "วางสาย", "โอเค", "ok", "thank", "謝謝", "谢谢"];
/// A Thai closing cue counts within this many letters before the goodbye (about
/// [`GREETING_BYE_CUE_WORDS`] words).
const TH_CUE_CHARS: usize = 18;
const TH_HELLOS: &[&str] = &["ฮัลโหล", "ฮัลโล", "ฮาโหล", "hello", "halo", "喂"];
/// Polite particles that end a Thai sentence; they do not count against the slack.
const TH_PARTICLES: &[&str] = &["ครับ", "คับ", "ค่ะ", "คะ", "นะ", "จ้า", "จ้ะ", "ฮะ"];
/// What may come right before a Thai goodbye inside one run of letters
/// ("ขอบคุณค่ะลาก่อน"); anything else is part of another word ("ขอเวลาก่อน",
/// "give me a moment first", holds "ลาก่อน").
const TH_LEADS: &[&str] = &["ครับ", "คับ", "ค่ะ", "คะ", "นะ", "จ้า", "ขอ", "ขอตัว", "งั้น", "แล้ว", "โอเค"];
const TH_NEGATIONS: &[&str] = &["อย่า", "อย่าเพิ่ง", "ไม่", "ไม่ต้อง", "ยังไม่"];

/// Lowercase letters and digits only. Thai tone marks drop out, so spellings
/// that differ only in them match.
fn squeeze(text: &str) -> String {
    lowercase(text).chars().filter(|c| c.is_alphanumeric()).collect()
}

/// [`squeeze`]d `text`, and where each of its runs between spaces or
/// punctuation starts in it (byte offsets).
fn squeeze_runs(text: &str) -> (String, Vec<usize>) {
    let mut out = String::new();
    let mut starts = Vec::new();
    let mut new_run = true;
    for c in lowercase(text).chars() {
        if c.is_alphanumeric() {
            if new_run {
                starts.push(out.len());
                new_run = false;
            }
            out.push(c);
        } else if c.is_whitespace() || c.is_ascii_punctuation() || "，。！？、；：…「」『』（）".contains(c) {
            new_run = true;
        }
    }
    (out, starts)
}

fn ends_with_any(text: &str, list: &[&str]) -> bool {
    list.iter().any(|w| {
        let w = squeeze(w);
        !w.is_empty() && text.ends_with(w.as_str())
    })
}

/// [`cue_window`] for squeezed Thai: the last [`TH_CUE_CHARS`] letters of
/// `before`, and only after the last of [`TH_HELLOS`].
fn thai_cue_window(before: &str) -> &str {
    let mut start = 0;
    for h in TH_HELLOS {
        let h = squeeze(h);
        if h.is_empty() {
            continue;
        }
        if let Some(j) = before.rfind(h.as_str()) {
            start = start.max(j + h.len());
        }
    }
    let seg = &before[start..];
    let n = seg.chars().count();
    match seg.char_indices().nth(n.saturating_sub(TH_CUE_CHARS)) {
        Some((k, _)) => &seg[k..],
        None => seg,
    }
}

/// A Thai goodbye within `slack_chars` letters of the end (particles aside).
fn thai_farewell(text: &str, slack_chars: usize) -> bool {
    let (all, starts) = squeeze_runs(text);
    let mut t = all.as_str();
    'trim: loop {
        for p in TH_PARTICLES {
            let p = squeeze(p);
            if p.is_empty() {
                continue;
            }
            if let Some(rest) = t.strip_suffix(p.as_str()) {
                t = rest;
                continue 'trim;
            }
        }
        break;
    }
    let near_end = |marker: &str| -> Option<usize> {
        let m = squeeze(marker);
        if m.is_empty() {
            return None;
        }
        t.rfind(m.as_str())
            .filter(|&i| t[i + m.len()..].chars().count() <= slack_chars)
    };
    TH_FAREWELLS.iter().any(|&m| {
        near_end(m).is_some_and(|i| {
            let before = &t[..i];
            !ends_with_any(before, TH_NEGATIONS)
                && (i == 0 || starts.contains(&i) || ends_with_any(before, TH_LEADS))
        })
    }) || TH_GREETING_BYES.iter().any(|&m| {
        near_end(m).is_some_and(|i| {
            let before = &t[..i];
            !ends_with_any(before, TH_NEGATIONS)
                && TH_CUES
                    .iter()
                    .any(|c| thai_cue_window(before).contains(squeeze(c).as_str()))
        })
    })
}

/// [`ends_with_farewell`] for Vietnamese, Thai and Indonesian.
/// Italian goodbyes said as the very last words only: "La saluto mia madre"
/// passes a greeting on, "Perfetto, la saluto" ends the call.
const IT_LAST_FAREWELLS: &[&str] = &["la saluto", "ti saluto"];

/// Italian goodbyes that are everyday words too ("cerco un buon lavoro", "alla
/// prossima fermata scendo"): a goodbye only as the very last words, with thanks
/// or an "okay" right before them ("Grazie, buon lavoro", "Va bene, alla prossima").
const IT_CUED_FAREWELLS: &[&str] = &["buon lavoro", "alla prossima"];
const IT_FAREWELL_CUES: &[&str] = &["grazie", "va bene", "perfetto", "ok", "okay", "d'accordo"];
/// Words right before a cued goodbye that may hold its cue ("grazie mille, buon lavoro").
const IT_FAREWELL_CUE_WORDS: usize = 2;

fn it_cued_farewell(ws: &[String]) -> bool {
    IT_CUED_FAREWELLS.iter().any(|f| {
        let h = word_list(f);
        if ws.len() <= h.len() || !ws.ends_with(&h) {
            return false;
        }
        let at = ws.len() - h.len();
        let before = &ws[at.saturating_sub(IT_FAREWELL_CUE_WORDS)..at];
        IT_FAREWELL_CUES.iter().any(|c| has_phrase(before, c))
    })
}

fn extra_farewell(text: &str, slack: usize) -> bool {
    let (ws, after_mark) = words_after_marks(text);
    EXTRA_WORD_FAREWELLS.iter().any(|m| phrase_near_end(&ws, &after_mark, m, slack))
        || IT_LAST_FAREWELLS.iter().any(|m| phrase_near_end(&ws, &after_mark, m, 0))
        || it_cued_farewell(&ws)
        || GREETING_BYES.iter().any(|g| g.ends(&ws))
        || falou_closing(&ws, &after_mark)
        || thai_farewell(text, slack * 2)
}

/// Words right before a closing Portuguese "falou" ("Beleza, valeu falou!").
/// "Então" only after another cue: "E então falou" is "and then he said".
const FALOU_CUES: &[&str] = &[
    "valeu", "obrigado", "obrigada", "beleza", "tá bom", "ta bom", "então tá", "combinado", "ok", "okay",
    "tchau", "abraço", "até mais", "certo", "fechou", "fechado", "tá", "ta", "tranquilo",
    "show", "joia", "jóia", "blz", "beleza então", "beleza entao", "combinado então", "combinado entao",
    "certo então", "certo entao", "fechado então", "fechado entao", "ok então", "ok entao", "tá bom então",
    "ta bom entao",
];

/// "Falou" is "bye" in Brazilian speech, and also just "said" ("Foi o que ele
/// falou."). It is a goodbye only as the very last word: alone, set off by
/// punctuation ("Certo, falou") or right after a closing cue.
fn falou_closing(ws: &[String], after_mark: &[bool]) -> bool {
    let Some((last, before)) = ws.split_last() else {
        return false;
    };
    last == "falou"
        && (before.is_empty()
            || after_mark.last().copied().unwrap_or(false)
            || FALOU_CUES.iter().any(|c| before.ends_with(&word_list(c))))
}

/// What was said ends on a goodbye, in any supported language other than
/// Chinese (those rules live in `gemini_live`), or in Vietnamese, Thai or
/// Indonesian. `slack` = words that may follow it ("bye now", "goodbye then").
/// Thanks after it do not count ([`strip_trailing_thanks`]): "Bye, thank you so much".
pub fn ends_with_farewell(text: &str, slack: usize) -> bool {
    let farewell = |t: &str| ends_on(t, |p| p.farewells, slack, slack * 2) || extra_farewell(t, slack);
    farewell(text) || farewell(strip_trailing_thanks(text))
}

/// The caller's words end the call: a goodbye or "that's all" at the end, and
/// not a question. Thanks after it do not count: "Alright, that's all, thank you so much".
pub fn caller_wraps_up(text: &str) -> bool {
    let wrapup = |t: &str| ends_on(t, |p| p.wrapups, 2, 4);
    !is_question(text)
        && (ends_with_farewell(text, 2)
            || wrapup(text)
            || wrapup(strip_trailing_thanks(text))
            || VI_THANKS_CLOSING.ends(&word_list(text))
            || ja_thanks_closing(text))
}

/// Japanese business calls close with thanks in the past tense, as the very
/// last words: 「（ご利用）ありがとうございました」 ends the call the way
/// 「失礼します」 does (a courier's call never hung up after it). The present
/// 「ありがとうございます」 is said mid-call and does not count. The caller's
/// side only, like [`caller_wraps_up`] as a whole.
fn ja_thanks_closing(text: &str) -> bool {
    let t = text.trim_end_matches(|c: char| c.is_whitespace() || "。．.、,!！〜～ー".contains(c));
    ["ありがとうございました", "有難うございました", "ありがとう御座いました"]
        .iter()
        .any(|s| t.ends_with(s))
}

/// Vietnamese callers often close with thanks alone, said politely to the end
/// ("Em cảm ơn anh ạ", "Cảm ơn anh nhé"): a courier's call never hung up after
/// it. Thanks then a particle (ạ / nhé / nha) as the very last words; the
/// caller's side only — the AI's own 「cảm ơn anh ạ」 is no goodbye.
const VI_THANKS_CLOSING: GreetingBye = GreetingBye {
    heads: &["cảm ơn", "cám ơn", "cam on"],
    tail: &[
        "anh", "chị", "chi", "em", "bạn", "ban", "cô", "co", "chú", "chu", "ông", "ong", "bà", "ba", "cháu",
        "chau", "nhiều", "nhieu", "lắm", "ạ", "nhé", "nhe", "nha", "nhá",
    ],
    closing_tail: &["ạ", "nhé", "nhe", "nha", "nhá"],
    cues: &[],
};


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goodbyes_in_thai_russian_arabic_and_hindi_end_the_call() {
        // The AI's own goodbye (no words may follow it) and the caller's.
        for bye in [
            "ขอบคุณมากค่ะ สวัสดีค่ะ",
            "โชคดีนะคะ",
            "แล้วคุยกันใหม่นะคะ",
            "ลาก่อนครับ",
            "Спасибо за звонок, до свидания!",
            "Всего доброго.",
            "Хорошо, пока!",
            "До свидания, спасибо!",
            "Хорошего дня!",
            "شكراً لك، مع السلامة.",
            "إلى اللقاء",
            "مع السلامة، شكرا",
            "धन्यवाद, नमस्ते।",
            "ठीक है, फिर मिलेंगे।",
            "अलविदा",
            "फिर बात करते हैं, धन्यवाद",
        ] {
            assert!(ends_with_farewell(bye, 0), "{bye}");
            assert!(caller_wraps_up(bye), "{bye}");
        }
        // Not goodbyes: "I don't know yet", "while you wait, I'll check", a Hindi
        // and an Arabic hello, Thai "give me a moment first".
        for not_yet in [
            "Я пока не знаю.",
            "Пока вы ждёте, я проверю.",
            "Пока",
            "नमस्ते, मैं राहुल बोल रहा हूँ",
            "नमस्ते",
            "السلام عليكم",
            "ขอเวลาก่อน",
            "ฉันโชคดีมาก",
        ] {
            assert!(!ends_with_farewell(not_yet, 0), "{not_yet}");
        }
        // Vietnamese and Indonesian as before.
        assert!(ends_with_farewell("Cảm ơn anh, chào anh.", 0));
        assert!(ends_with_farewell("Baik, terima kasih, sampai jumpa.", 0));
        // Their thanks alone are no goodbye.
        for thanks in ["Спасибо!", "شكرا", "धन्यवाद"] {
            assert!(!caller_wraps_up(thanks), "{thanks}");
        }
    }

    #[test]
    fn a_negated_goodbye_phrase_does_not_end_the_call() {
        assert!(!caller_wraps_up("¡Hola! ¿Qué tal? Hace tiempo que no nos vemos."));
        assert!(caller_wraps_up("Vale, perfecto. Nos vemos."));
        assert!(caller_wraps_up("Bueno, adiós."));
        assert!(!caller_wraps_up("Ainda não, tchau não"));
    }

    #[test]
    fn a_no_that_answers_anything_else_is_no_negation() {
        for s in [
            "No, that's all, thanks.",
            "No that's everything, thank you so much",
            "No, nothing else.",
            "No, that's it.",
            "No, nada más, gracias.",
            "No, eso es todo.",
            "Não, é só isso, obrigado.",
            "Não, mais nada.",
            "Não, é tudo.",
            "Não, só isso.",
        ] {
            assert!(caller_wraps_up(s), "must wrap up: {s}");
        }
        // A real negation of the same words: there is more to come.
        for s in ["Mas isso não é tudo.", "Não só isso.", "No, that's all right.", "That's not all."] {
            assert!(!caller_wraps_up(s), "must not wrap up: {s}");
        }
    }

    #[test]
    fn words_after_marks_has_the_same_words() {
        for s in [
            "No, that's all — thanks!",
            "Certo, falou",
            "’Bye… l'adresse, d'accord ?",
            "Teşekkürler, İyi günler.",
            "好的，謝謝。Bye-bye",
            "",
            " ' ",
        ] {
            let (ws, after_mark) = words_after_marks(s);
            assert_eq!(ws, word_list(s), "{s}");
            assert_eq!(ws.len(), after_mark.len(), "{s}");
        }
        assert_eq!(words_after_marks("Certo, falou").1, [false, true]);
        assert_eq!(words_after_marks("ele falou").1, [false, false]);
    }

    #[test]
    fn name_corrections_rewrite_what_stt_mishears() {
        let fixes = parse_name_fixes(&[
            "黃先生=王先生".to_string(),
            " Jon Smith → John Smith ".to_string(),
            "no separator".to_string(),
            "same=same".to_string(),
            "=x".to_string(),
        ]);
        assert_eq!(fixes.len(), 2);
        assert_eq!(fix_names_with("請轉告黃先生明天開會", &fixes), "請轉告王先生明天開會");
        assert_eq!(fix_names_with("Is Jon Smith there?", &fixes), "Is John Smith there?");
        assert_eq!(fix_names_with("從黃山寄來的包裹", &fixes), "從黃山寄來的包裹");
        // A name in a spaced language is a whole word; Chinese, Japanese, Korean run on.
        let jon = vec![("Jon".to_string(), "John".to_string())];
        assert_eq!(fix_names_with("Mr. Jones asked for Jon, Jon's brother.", &jon), "Mr. Jones asked for John, John's brother.");
        assert_eq!(fix_names_with("Jon", &jon), "John");
        assert_eq!(fix_names_with("找Jon先生", &jon), "找John先生");
        let kr = vec![("민수".to_string(), "민호".to_string())];
        assert_eq!(fix_names_with("민수씨 계세요?", &kr), "민호씨 계세요?");
        let ja = vec![("斉藤".to_string(), "斎藤".to_string())];
        assert_eq!(fix_names_with("斉藤さんからです", &ja), "斎藤さんからです");
        // Nothing set (the default): text is untouched.
        assert_eq!(fix_names("請轉告黃先生"), "請轉告黃先生");
    }

    #[test]
    fn codes_round_trip() {
        for l in ALL {
            assert_eq!(Lang::parse(l.code()), l);
        }
        assert_eq!(Lang::parse("zh_TW"), Lang::ZhTw);
        assert_eq!(Lang::parse("pt-BR"), Lang::Pt);
        assert_eq!(Lang::parse("xx"), Lang::En);
        assert_eq!(Lang::parse(""), Lang::En);
    }

    #[test]
    fn owner_text_follows_the_notification_language() {
        // Unset (unit tests): the AI and the owner both on zh-TW.
        let a = Active::default();
        assert_eq!((a.lang, a.notify_lang), (Lang::ZhTw, Lang::ZhTw));
        // Traditional Chinese only when the owner reads zh-TW, whatever the AI speaks.
        assert_eq!(for_owner_in(Lang::ZhTw, "听录音"), "聽錄音");
        assert_eq!(for_owner_in(Lang::ZhCn, "听录音"), "听录音");
        assert_eq!(for_owner_in(Lang::Ja, "会議の件"), "会議の件");
        assert_eq!(for_owner_in(Lang::En, "Call back"), "Call back");
        assert_eq!(fill_in(Lang::ZhCn, "{n}（{m}）", &[("n", "號碼"), ("m", "王小明")]), "号码（王小明）");
        assert_eq!(fill_in(Lang::De, "{n} ({m}) {x} {", &[("n", "{m}"), ("m", "b")]), "{m} (b) {x} {");
    }

    #[test]
    fn farewells_only_count_at_the_end() {
        for t in [
            "Okay, thanks a lot. Bye!",
            "alright bye now",
            "Great, talk to you later.",
            "Vale, muchas gracias, hasta luego.",
            "D'accord, merci, au revoir !",
            "Gut, danke, auf Wiederhören.",
            "Tá bom, obrigado, tchau.",
            "はい、ありがとうございました。失礼します。",
            "네 감사합니다. 안녕히 계세요.",
            // Everyday closings in each language.
            "Okay, have a good one.",
            "Take care!",
            "Cheers.",
            "Talk soon.",
            "Okay bye now.",
            "それでは失礼いたします。",
            "수고하세요.",
            "네, 들어가세요.",
            "네 감사합니다 안녕히 계세요",
            "Órale, gracias.",
            "Bueno, hasta luego.",
            "Nos vemos.",
            "Chao.",
            "Valeu, tchau.",
            "Até mais.",
            "Bonne journée !",
            "Merci, à bientôt.",
            "Danke, schönen Tag.",
            "Tschüss.",
            "Auf Wiederhören.",
            // Thanks after the goodbye.
            "Okay, bye, thank you so much!",
            "Adiós, muchas gracias.",
        ] {
            assert!(ends_with_farewell(t, 2), "{t}");
            assert!(caller_wraps_up(t), "{t}");
        }
        for t in [
            "Maybe.",
            "I said goodbye to him yesterday and then he called again about the loan",
            "The bylaws say so",
            "Hello?",
            "Is that all?",
            "もしもし",
            "Buenas noches",
            // Thanks alone, or a goodbye said to someone else in the room.
            "Thank you.",
            "Thanks so much",
            "Muchas gracias",
            // Portuguese "valeu" is thanks.
            "Valeu!",
            "감사합니다",
            "Say bye to grandma",
            "Come on, say bye bye to Auntie",
            // A caller introducing themselves on a business call.
            "株式会社ABCの田中と申します。よろしくお願いいたします。",
        ] {
            assert!(!caller_wraps_up(t), "{t}");
        }
        assert!(caller_wraps_up("Okay, that's all, thank you."));
        assert!(caller_wraps_up("Alright, that's all, thank you so much"));
        assert!(caller_wraps_up("Okay, bye to you"));
        assert!(caller_wraps_up("以上です。"));
        assert!(caller_wraps_up("それでは失礼します。"));
        assert!(caller_wraps_up("では、よろしくお願いいたします。"));
        // The same words turned into a question: not leaving yet.
        assert!(!caller_wraps_up("それではよろしいでしょうか？"));
        assert!(!caller_wraps_up("Okay, that's all for today, right?"));
        assert!(!caller_wraps_up("Bye?"));
    }

    #[test]
    fn words_that_open_a_sentence_do_not_end_the_call() {
        // 「それでは」「그만」「이만」 also start what comes next: up to four letters
        // after them used to count as the end and hung up on the caller.
        for t in ["はい、それでは伝言です。", "それでは質問です", "それでは", "그만 울어요", "이만 원이에요", "그만해요"] {
            assert!(!caller_wraps_up(t), "{t}");
        }
        // Real closings still end it.
        for t in ["それでは失礼します。", "はい、それではまた。", "それでは失礼いたします", "이만 끊을게요", "그럼 이만 들어가세요"] {
            assert!(caller_wraps_up(t), "{t}");
        }
    }

    #[test]
    fn thanks_after_the_last_words_are_set_aside() {
        assert_eq!(strip_trailing_thanks("好的，那就先這樣，謝謝您"), "好的，那就先這樣");
        assert_eq!(strip_trailing_thanks("Alright, that's all, thank you so much!"), "Alright, that's all");
        assert_eq!(strip_trailing_thanks("はい、以上です。ありがとうございました。"), "はい、以上です");
        assert_eq!(strip_trailing_thanks("Merci beaucoup"), "");
        assert_eq!(strip_trailing_thanks("謝謝"), "");
        // Only whole Latin words.
        assert_eq!(strip_trailing_thanks("bethanks"), "bethanks");
        assert_eq!(strip_trailing_thanks("Hello?"), "Hello");
    }

    #[test]
    fn asking_to_hold_in_every_language() {
        for t in [
            "等一下喔，我去拿",
            "好，我幫你轉接",
            "帮你转一下",
            "稍等",
            "Hold on a sec, let me get him",
            "One moment please",
            "少々お待ちください",
            "잠시만요",
            "잠시만 기다려 주세요",
            "Un momento, por favor",
            "Um momento",
            "Ne quittez pas",
            "Einen Moment bitte",
            "Đợi chút nhé",
            "Tunggu sebentar ya",
            "รอสักครู่นะคะ",
            "好，等一下。",
            "你等一下，我叫他來聽",
            "請稍等，我幫您轉接",
        ] {
            assert!(asks_to_hold(t), "{t}");
        }
        for t in [
            "我今天有空",
            "Hello, is this Alex?",
            "That's all, thanks",
            "Momentan nicht",
            "もしもし",
            "",
            // 「等一下」 as "later", 「等等」 as "and so on".
            "我等一下要出門，所以放管理室",
            "那我等一下再打給你",
            "等一下再說",
            "保險、貸款等等都有",
        ] {
            assert!(!asks_to_hold(t), "{t}");
        }
    }

    #[test]
    fn japanese_past_thanks_at_the_end_closes_the_call() {
        // A courier's call that never hung up.
        assert!(caller_wraps_up("かしこまりました。それでは、管理室の方にお預けしておきますね。ご利用ありがとうございました。"));
        assert!(caller_wraps_up("お忙しいところありがとうございました"));
        // Present-tense thanks are said mid-call; thanks followed by more words are not the end.
        assert!(!caller_wraps_up("ありがとうございます。"));
        assert!(!caller_wraps_up("ありがとうございました。それで、明日の件ですが"));
        assert!(!caller_wraps_up("ありがとうございましたか？"));
    }

    #[test]
    fn vietnamese_goodbyes_count_only_at_the_end() {
        for t in [
            // A real call that never hung up.
            "Dạ vậy em để ở quầy lễ tân nhé. Cảm ơn anh, chào anh.",
            "Vâng, tạm biệt anh.",
            "Thôi chào em nhé.",
            "Ok cảm ơn, chào.",
            "Dạ vâng, cảm ơn chị. Em chào chị ạ.",
            "Hẹn gặp lại nhé.",
            "Chào nhé.",
            "Cam on anh, chao anh",
            "Dạ vâng, thế nhé.",
            "Ok vậy nhé.",
        ] {
            assert!(ends_with_farewell(t, 2), "{t}");
            assert!(caller_wraps_up(t), "{t}");
        }
        for t in [
            // Greetings at the start of a call.
            "Alo, chào anh.",
            "Dạ em chào anh ạ.",
            "Chào anh, em gọi từ bên giao hàng về đơn hàng của anh.",
            // Not at the end, negated, a question.
            "Hôm qua em đã nói tạm biệt với anh ấy rồi mà anh ấy vẫn gọi lại hỏi về khoản vay.",
            "Khoan đã, đừng tạm biệt vội.",
            "Cảm ơn anh, chào anh?",
            // An "ok" / "nhé" from earlier in the window, then the phone is handed
            // over and the next person greets.
            "Dạ ok anh, để em đưa máy cho chị em nhé. Alo, em chào anh ạ.",
            "Dạ vâng, em để ở quầy lễ tân nhé. Em chào anh ạ.",
        ] {
            assert!(!caller_wraps_up(t), "{t}");
        }
        // Thanks alone, said to the end, closes a Vietnamese call — the caller's
        // words only (the AI's own thanks are no goodbye).
        for t in ["Dạ, em cảm ơn anh ạ.", "Em cảm ơn anh ạ", "Ok, cảm ơn anh nhé", "Em cảm ơn anh nhiều ạ", "Cảm ơn chị nha"] {
            assert!(caller_wraps_up(t), "{t}");
            assert!(!ends_with_farewell(t, 2), "{t}");
        }
        for t in ["Cảm ơn anh ạ, cho em hỏi thêm một chút về đơn hàng.", "Cảm ơn anh ạ?", "Em cảm ơn anh"] {
            assert!(!caller_wraps_up(t), "{t}");
        }
    }

    #[test]
    fn thai_goodbyes_count_only_at_the_end() {
        for t in [
            // A real call that never hung up.
            "งั้นฝากไว้ที่นิติบุคคลนะคะ ขอบคุณค่ะ สวัสดีค่ะ",
            "โอเคครับ ลาก่อนครับ",
            "ขอบคุณค่ะลาก่อนค่ะ",
            "บ๊ายบาย",
            "บายบายค่ะ",
            "ขอบคุณครับ สวัสดีครับ",
            "Khob khun ka, sawasdee ka.",
            "ขอบคุณค่ะ ไว้คุยกันใหม่นะคะ",
        ] {
            assert!(ends_with_farewell(t, 2), "{t}");
            assert!(caller_wraps_up(t), "{t}");
        }
        for t in [
            // Greetings at the start of a call.
            "ฮัลโหล สวัสดีค่ะ",
            "สวัสดีค่ะ ดิฉันโทรมาจากธนาคารนะคะ",
            "Sawasdee ka.",
            // "Give me a moment first": ลาก่อน inside another word.
            "ขอเวลาก่อนนะคะ",
            // "Don't say goodbye yet."
            "อย่าเพิ่งลาก่อนนะ",
            "อย่าเพิ่ง ลาก่อนนะ",
            // Thanks from earlier, then the phone is handed over and the next person greets.
            "ขอบคุณค่ะ เดี๋ยวส่งให้แม่คุยนะคะ ฮัลโหล สวัสดีค่ะ",
        ] {
            assert!(!caller_wraps_up(t), "{t}");
        }
    }

    #[test]
    fn indonesian_goodbyes_count_only_at_the_end() {
        for t in [
            "Baik, terima kasih, selamat siang.",
            "Makasih ya, selamat malam, Bu.",
            "Oke, sampai jumpa.",
            "Ya sudah, sampai ketemu lagi ya.",
            "Dadah!",
            "Daah.",
            "Ya sudah, sampai nanti ya.",
            "Oke, sudah dulu ya.",
            // "Terima kasih" written in Chinese by speech-to-text, alone or run into
            // other Chinese characters.
            "謝謝，selamat siang.",
            "那謝謝 selamat sore ya",
            "谢谢。Selamat malam, Bu.",
        ] {
            assert!(ends_with_farewell(t, 2), "{t}");
            assert!(caller_wraps_up(t), "{t}");
        }
        for t in [
            // Greetings at the start of a call.
            "Halo, selamat pagi.",
            "Selamat siang, saya dari bank mau menawarkan pinjaman.",
            // Not at the end, negated, a question.
            "Kemarin saya sudah bilang sampai jumpa ke dia tapi dia telepon lagi soal pinjaman.",
            "Jangan dadah dulu.",
            "Terima kasih, selamat sore?",
            // "Ok" from earlier, then the phone is handed over and the next person greets.
            "Oke Pak, sebentar ya, saya kasih ke ibu saya. Halo, selamat siang.",
            "謝謝。Halo, selamat siang.",
            "謝謝，selamat siang?",
        ] {
            assert!(!caller_wraps_up(t), "{t}");
        }
    }

    #[test]
    fn countries_name_the_callers_language() {
        assert_eq!(country_lang("us"), Some(Lang::En));
        assert_eq!(country_lang(" CA "), Some(Lang::En));
        assert_eq!(country_lang("jm"), Some(Lang::En));
        assert_eq!(country_lang("sg"), Some(Lang::En));
        assert_eq!(country_lang("pr"), Some(Lang::Es));
        assert_eq!(country_lang("mx"), Some(Lang::Es));
        assert_eq!(country_lang("jp"), Some(Lang::Ja));
        assert_eq!(country_lang("kr"), Some(Lang::Ko));
        assert_eq!(country_lang("br"), Some(Lang::Pt));
        assert_eq!(country_lang("ch"), Some(Lang::De));
        assert_eq!(country_lang("be"), Some(Lang::Fr));
        assert_eq!(country_lang("cn"), Some(Lang::ZhCn));
        assert_eq!(country_lang("hk"), Some(Lang::ZhTw));
        assert_eq!(country_lang("tw"), Some(Lang::ZhTw));
        // Not one of the languages (Vietnam, Thailand, Indonesia, Luxembourg), or no country.
        for iso in ["vn", "th", "id", "lu", "", "zz"] {
            assert_eq!(country_lang(iso), None, "{iso}");
        }
        // Every country listed is one the number reader knows.
        for &(iso, _) in COUNTRY_LANGS {
            assert!(crate::phone_region::calling_code(iso).is_some(), "{iso}");
        }
        // The calling-code table (phone app that does not report its SIM) agrees
        // for each code's main country.
        for &(code, lang) in CALLING_CODE_LANGS {
            let iso = crate::phone_region::country_of_code(code).expect(code);
            assert_eq!(country_lang(iso), Some(lang), "{code} {iso}");
        }
    }

    #[test]
    fn calling_codes_name_the_callers_language() {
        assert_eq!(calling_code_lang("+1 202-555-0143"), Some(Lang::En));
        assert_eq!(calling_code_lang("+447700900123"), Some(Lang::En));
        assert_eq!(calling_code_lang("+353871234567"), Some(Lang::En));
        assert_eq!(calling_code_lang("+65 9123 4567"), Some(Lang::En));
        assert_eq!(calling_code_lang("+351912345678"), Some(Lang::Pt));
        assert_eq!(calling_code_lang("+5511912345678"), Some(Lang::Pt));
        assert_eq!(calling_code_lang("+5215512345678"), Some(Lang::Es));
        assert_eq!(calling_code_lang("+3225123456"), Some(Lang::Fr));
        assert_eq!(calling_code_lang("+41441234567"), Some(Lang::De));
        assert_eq!(calling_code_lang("+81312345678"), Some(Lang::Ja));
        assert_eq!(calling_code_lang("+821012345678"), Some(Lang::Ko));
        assert_eq!(calling_code_lang("+8613800138000"), Some(Lang::ZhCn));
        assert_eq!(calling_code_lang("+886912345678"), Some(Lang::ZhTw));
        assert_eq!(calling_code_lang("+85291234567"), Some(Lang::ZhTw));
        assert_eq!(calling_code_lang("+85361234567"), Some(Lang::ZhTw));
        // Not one of the languages (Vietnam, Luxembourg), or no country code.
        assert_eq!(calling_code_lang("+84912345678"), None);
        assert_eq!(calling_code_lang("+352621123456"), None);
        assert_eq!(calling_code_lang("0912345678"), None);
        assert_eq!(calling_code_lang(""), None);
        // The longest code that starts the number wins.
        let table = [("1", Lang::En), ("1787", Lang::Es), ("35", Lang::De), ("351", Lang::Pt)];
        assert_eq!(longest_code_match("17875551234", &table), Some(Lang::Es));
        assert_eq!(longest_code_match("12025550143", &table), Some(Lang::En));
        assert_eq!(longest_code_match("351912345678", &table), Some(Lang::Pt));
        assert_eq!(longest_code_match("353871234567", &table), Some(Lang::De));
        for (code, _) in CALLING_CODE_LANGS {
            assert!(!code.is_empty() && code.chars().all(|c| c.is_ascii_digit()), "{code}");
        }
    }

    #[test]
    fn stall_keywords_match_whole_words() {
        // The lists that lived here are in the scam database now, and match as they did.
        let hits = crate::scam_db::keyword_hits("Hi, I'm calling about your car's extended warranty and a low interest loan");
        let kws: Vec<&str> = hits.iter().map(|h| h.text).collect();
        assert!(kws.contains(&"warranty") && kws.contains(&"loan"), "{kws:?}");
        // Only the start of a word counts: "bank" does not hit "embankment".
        assert!(crate::scam_db::keyword_hits("the embankment by the river").is_empty());
        assert!(!crate::scam_db::keyword_hits("保険のご案内です").is_empty());
        assert_eq!(crate::scam_db::keyword("warranty").map(|h| h.weight), Some(2));
        assert_eq!(crate::scam_db::keyword("nonsense"), None);
    }

    #[test]
    fn every_language_has_its_phrases() {
        for l in ALL {
            let (p, w) = (phrases(l), words(l));
            assert!(!p.hello.is_empty() && !p.bye.is_empty() && !p.unavailable.is_empty(), "{l:?}");
            assert!(w.n_blocked_done.contains("{}"), "{l:?}");
            assert!(!w.n_repeat_title.is_empty(), "{l:?}");
            assert!(w.n_repeat_body.contains("{number}") && w.n_repeat_body.contains("{count}"), "{l:?}");
            if !l.is_zh() {
                assert!(!p.farewells.is_empty(), "{l:?}");
            }
        }
    }

    #[test]
    fn dates_and_lengths_follow_the_language() {
        let t = chrono::NaiveDate::from_ymd_opt(2026, 11, 3)
            .unwrap()
            .and_hms_opt(8, 9, 0)
            .unwrap();
        assert_eq!(date_time(Lang::En, &t), "Nov 3, 08:09");
        assert_eq!(date_time(Lang::ZhTw, &t), "11月3日 08:09");
        assert_eq!(date_time(Lang::Ko, &t), "11월 3일 08:09");
        assert_eq!(date_time(Lang::De, &t), "03.11. 08:09");
        assert_eq!(date_time(Lang::Fr, &t), "03/11 08:09");
        assert_eq!(date_only(Lang::Ja, &t.date()), "11月3日");
        assert_eq!(date_only(Lang::En, &t.date()), "Nov 3");
        for l in ALL {
            let s = date_time(l, &t);
            assert!(!s.contains('%') && s.contains("08:09") && s.contains('3'), "{l:?}: {s}");
        }
        assert_eq!(duration(Lang::ZhTw, 71.9), "72 秒");
        assert_eq!(duration(Lang::ZhTw, 600.0), "10 分鐘");
        assert_eq!(duration(Lang::En, 45.0), "45 s");
        assert_eq!(duration(Lang::De, 300.0), "5 Min.");
        assert_eq!(duration(Lang::Ja, 30.0), "30秒");
        assert_eq!(duration(Lang::En, f64::NAN), "0 s");
        assert_eq!(quoted(Lang::ZhTw, "你好"), "「你好」");
    }

    #[test]
    fn every_template_has_exactly_one_slot() {
        for l in ALL {
            let p = words(l);
            for t in [
                &p.u_sec, &p.u_min, &p.s_no_transcript, &p.s_ai_fail_matter, &p.s_stall_topic_kw,
                &p.s_stall_reply, &p.vm_call_back_to, &p.vm_email_to, &p.vm_length, &p.vm_no_transcript,
                &p.f_bad_time, &p.f_openclaw_failed, &p.e_appointment,
            ] {
                assert_eq!(t.matches("{}").count(), 1, "{l:?}: {t}");
            }
            for t in [
                &p.s_no_service, &p.s_see_transcript, &p.s_listen_recording, &p.s_ai_fail_prefix,
                &p.s_stall_topic, &p.vm_call_back_unknown, &p.tc_confirm, &p.tc_ignore, &p.tc_confirm_ack,
                &p.f_callback_waiting, &p.f_gmail_missing, &p.g_short_password,
                &p.e_subject, &p.e_empty,
            ] {
                assert!(!t.contains('{') && !t.trim().is_empty(), "{l:?}: {t}");
            }
            assert!(p.sep.contains(':') || p.sep.contains('：'), "{l:?}");
            assert_eq!(fill(&p.vm_call_back_to, "0912").matches("0912").count(), 1, "{l:?}");
        }
    }

    #[test]
    fn polite_service_farewells_close_without_matching_greetings() {
        for s in [
            "Thanks for your time and have a wonderful day.",
            "Muchas gracias por su tiempo. Y que tenga un excelente día.",
            "Merci beaucoup, excellente journée.",
            "Vielen Dank und einen schönen Tag.",
            "Obrigado, tenha um ótimo dia.",
            "감사합니다, 좋은 하루 되세요.",
        ] {
            assert!(ends_with_farewell(s, 0), "expected farewell: {s}");
        }
        for open in ["Hola, buen día, hablo del banco.", "Olá, bom dia, tudo bem?", "안녕!"] {
            assert!(!ends_with_farewell(open, 2), "greeting must not match farewell: {open}");
        }
        assert!(ends_with_farewell("Grazie mille, buongiornata!", 0));
        assert!(ends_with_farewell("감사합니다, 안녕!", 0));
        assert!(ends_with_farewell("ขอบคุณค่ะ แล้วพบกันใหม่นะคะ", 0));
        assert!(ends_with_farewell("Bedankt, tot ziens!", 0));
        assert!(ends_with_farewell("Dziękuję, do widzenia!", 0));
        assert!(ends_with_farewell("Mulțumesc, la revedere!", 0));
        assert!(ends_with_farewell("Дякую, до побачення!", 0));
        assert!(ends_with_farewell("ধন্যবাদ, বিদায়!", 0));
        assert!(ends_with_farewell("நன்றி, போய் வருகிறேன்!", 0));
        assert!(ends_with_farewell("ధన్యవాదాలు, వెళ్ళొస్తాను!", 0));
    }

    #[test]
    fn pt_valeu_is_thanks_and_falou_closes_only_after_a_cue() {
        assert_eq!(strip_trailing_thanks("Ah, valeu."), "Ah");
        for s in [
            "Foi o que ele falou.",
            "O gerente falou que sim.",
            "Ela já falou com você.",
            "Fica combinado como ele falou.",
            "Ele chegou e falou.",
            "Valeu a pena.",
            "Ah, valeu.",
        ] {
            assert!(!caller_wraps_up(s), "must not wrap up: {s}");
        }
        for s in [
            "Beleza, valeu, falou!",
            "Então tá, falou.",
            "Tchau, falou.",
            "Falou!",
            // Set off by punctuation, or right after a closing cue.
            "Certo, falou",
            "Tá combinado então, falou",
            "Beleza então, falou",
            "Fechou, falou",
            "Obrigado, até mais, falou",
            "Então tá bom, falou valeu",
            "Tá bom, obrigada, falou",
            "Beleza então falou",
        ] {
            assert!(caller_wraps_up(s), "must wrap up: {s}");
        }
    }

    #[test]
    fn a_word_after_a_goodbye_phrase_can_make_it_something_else() {
        for s in [
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
            // What is left with them, or what they are left to do.
            "Je vous laisse l'adresse.",
            "Je vous laisse réfléchir.",
            "Te dejo un mensaje.",
            "Le dejo mis datos.",
            "Das ist alles nicht so einfach.",
            "Das ist alles zu teuer.",
            "Na razie nic nie wiem.",
            "Na razie jestem w pracy.",
            "Isso é tudo que eu sei.",
        ] {
            assert!(!caller_wraps_up(s), "must not wrap up: {s}");
        }
        for s in [
            "Take care.",
            "Okay, take care, bye.",
            "Take care now.",
            "Take care of yourself.",
            "That's all.",
            "That's all, thanks.",
            "That's all for now.",
            "That's all I need.",
            "Je vous laisse.",
            "Bon, je vous laisse, bonne journée.",
            "Bueno, te dejo.",
            "Le dejo, hasta luego.",
            "Nada más, gracias.",
            "Das ist alles.",
            "Das ist alles, danke.",
            "C'est tout.",
            "C'est tout, merci.",
            "É tudo.",
            "É tudo, obrigado.",
            "Na razie!",
            "To na razie, pa.",
            // A name, a title, a particle or another goodbye after it.
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
        ] {
            assert!(caller_wraps_up(s), "must wrap up: {s}");
        }
    }

    #[test]
    fn tamil_and_telugu_coming_is_no_goodbye_but_going_and_coming_is() {
        for s in [
            "பத்து நிமிடத்தில் வருகிறேன்.",
            "நான் நாளைக்கு வருகிறேன்.",
            "నేను రేపు వస్తాను.",
            "పది నిమిషాల్లో వస్తాను.",
        ] {
            assert!(!caller_wraps_up(s), "must not wrap up: {s}");
        }
        for s in ["சரி, போய் வருகிறேன்.", "சரி, போயிட்டு வருகிறேன்.", "సరే, వెళ్ళొస్తాను.", "సరే, ఉంటాను.", "ఇక ఉంటాను."] {
            assert!(caller_wraps_up(s), "must wrap up: {s}");
        }
    }

    #[test]
    fn byebye_written_as_one_word_is_a_goodbye() {
        assert!(ends_with_farewell("Chào em nhé. Byebye.", 1));
        assert!(ends_with_farewell("Okay, byebye.", 1));
    }

    #[test]
    fn words_that_only_look_like_a_cue_or_an_object_are_told_apart() {
        // "And then he said": no goodbye. With a comma it is one.
        assert!(!caller_wraps_up("E então falou."));
        assert!(caller_wraps_up("Beleza então, falou."));
        // "That is not all" is no wrap-up; "No, that is all" is.
        assert!(!caller_wraps_up("Não é só isso."));
        assert!(!caller_wraps_up("Isso não é tudo."));
        assert!(caller_wraps_up("Não, é só isso."));
        // "d'accord" after the goodbye leaves nothing with anyone.
        assert!(caller_wraps_up("Je vous laisse, d'accord."));
        assert!(!caller_wraps_up("Je vous laisse d'autres documents."));
        // What is left, counted; what "all that" is.
        for s in [
            "Je vous laisse deux colis.",
            "Je vous laisse 20 euros.",
            "Le dejo veinte euros.",
            "Le dejo 3 paquetes.",
            "Das ist alles bereits bezahlt.",
            "Das ist alles schon erledigt.",
            "É tudo bem barato.",
            "Isso é tudo errado.",
            "Na razie brak paczki.",
        ] {
            assert!(!caller_wraps_up(s), "must not wrap up: {s}");
        }
    }

    #[test]
    fn thai_see_you_and_vietnamese_good_day_are_goodbyes() {
        assert!(ends_with_farewell("ขอบคุณครับ ไว้เจอกันครับ", 1));
        assert!(ends_with_farewell("โอเคค่ะ ไว้เจอกันใหม่นะคะ", 1));
        assert!(ends_with_farewell("Cảm ơn anh nhiều, chúc anh một ngày tốt lành", 1));
        assert!(ends_with_farewell("Vâng em hiểu rồi, chào anh ạ", 1));
        assert!(ends_with_farewell("Được rồi, chào chị.", 1));
        // "Dạ" opens a call as well: no closing cue.
        assert!(!ends_with_farewell("Alo, dạ chào anh ạ", 1));
        assert!(!caller_wraps_up("Alo, dạ chào anh ạ"));
    }

    #[test]
    fn turkish_goodbyes_thanks_and_the_dotted_capital() {
        assert!(ends_with_farewell("Tamam, görüşürüz.", 1));
        for s in ["Hoşça kalın.", "Görüşürüz.", "İyi geceler.", "Güle güle.", "Kendinize iyi bakın."] {
            assert!(ends_with_farewell(s, 1), "must be a goodbye: {s}");
        }
        // A greeting that also says goodbye needs a closing cue before it.
        assert!(!ends_with_farewell("Merhaba, iyi günler.", 1));
        assert!(ends_with_farewell("Tamam, teşekkürler, iyi günler.", 1));
        // Turkish capital İ lowercases to "i" plus a combining dot: it must still match.
        assert!(ends_with_farewell("Teşekkürler, İyi günler.", 1));
        assert!(ends_with_farewell("İYİ GECELER.", 1));
        assert_eq!(strip_trailing_thanks("Tamam, teşekkürler."), "Tamam");
        assert_eq!(strip_trailing_thanks("Anladım, Çok Teşekkür Ederim!"), "Anladım");
        assert!(caller_wraps_up("Tamam, görüşürüz."));
        assert!(!caller_wraps_up("Yarın evde misiniz?"));
        assert!(!caller_wraps_up("Yarın görüşürüz mü?"));
    }

    #[test]
    fn italian_goodbyes_thanks_and_the_greetings_that_say_goodbye() {
        for s in [
            "Va bene, a presto.",
            "Buona serata.",
            "Ci sentiamo.",
            "Grazie mille, arrivederci.",
            "Arrivederci, grazie mille.",
            "Grazie, buon lavoro.",
            "Va bene, alla prossima!",
            "Grazie mille, alla prossima.",
            "Perfetto, la saluto.",
            "La saluto.",
        ] {
            assert!(ends_with_farewell(s, 1), "must be a goodbye: {s}");
        }
        // Everyday sentences with the same words, with the slack the caller's check allows.
        for s in [
            "Siamo un'azienda che cerca persone per un buon lavoro",
            "Alla prossima fermata scendo",
            "Buongiorno, buon lavoro, sono l'idraulico",
            "La saluto mia madre",
            "Buon lavoro.",
            "Grazie, cerchiamo persone per un buon lavoro",
        ] {
            assert!(!ends_with_farewell(s, 2), "must not be a goodbye: {s}");
            assert!(!caller_wraps_up(s), "must not wrap up: {s}");
        }
        // Greetings that also say goodbye need a closing cue before them.
        assert!(ends_with_farewell("Va bene, grazie, buonasera.", 1));
        assert!(ends_with_farewell("Perfetto, grazie, ciao!", 1));
        assert!(ends_with_farewell("Grazie mille, buongiorno.", 1));
        for s in [
            "Buongiorno.",
            "Pronto, buongiorno.",
            "Ho un pacco per lei, buongiorno",
        ] {
            assert!(!ends_with_farewell(s, 1), "must not be a goodbye: {s}");
        }
        // A greeting with a name after it, with the slack the caller's check allows.
        for s in ["Ciao, sono Marco.", "Ciao, mi chiamo Marco.", "Ciao, ho un pacco per lei.", "Ciao, c'è il signor Rossi?"] {
            assert!(!ends_with_farewell(s, 2), "must not be a goodbye: {s}");
            assert!(!caller_wraps_up(s), "must not wrap up: {s}");
        }
        // A name after a goodbye is still a goodbye.
        assert!(ends_with_farewell("Va bene, ciao Marco.", 2));
        assert!(ends_with_farewell("Grazie, ciao a tutti.", 2));
        assert_eq!(strip_trailing_thanks("Va bene, grazie."), "Va bene");
        assert_eq!(strip_trailing_thanks("Perfetto, Grazie Mille!"), "Perfetto");
        assert_eq!(strip_trailing_thanks("Grazie"), "");
        assert!(caller_wraps_up("Va bene, a presto."));
        assert!(!caller_wraps_up("Ci sentiamo domani?"));
        // Other languages as before.
        assert!(ends_with_farewell("Okay, goodbye.", 1));
        assert!(ends_with_farewell("Vale, hasta luego.", 1));
        assert!(!ends_with_farewell("Gracias", 1));
        assert_eq!(strip_trailing_thanks("Vale, gracias."), "Vale");
    }
}

#[cfg(test)]
mod ext_tests {
    use super::*;
    use bageo_packs::data::{OFFICIAL_LANGS, OFFICIAL_LANG_INFO};

    /// [Lang::parse] as it was before non-core languages existed.
    fn parse_before(s: &str) -> Lang {
        let t = s.trim().to_ascii_lowercase().replace('_', "-");
        match t.as_str() {
            "zh-tw" | "zh-hant" | "zh-hk" | "zh" => Lang::ZhTw,
            "zh-cn" | "zh-hans" | "zh-sg" => Lang::ZhCn,
            _ => match t.split('-').next().unwrap_or("") {
                "ja" => Lang::Ja,
                "ko" => Lang::Ko,
                "es" => Lang::Es,
                "fr" => Lang::Fr,
                "de" => Lang::De,
                "pt" => Lang::Pt,
                _ => Lang::En,
            },
        }
    }

    fn ext(code: &str) -> Lang {
        Lang::Ext(Ext::from_code(code).unwrap_or_else(|| panic!("{code}")))
    }

    #[test]
    fn parse_is_unchanged_for_every_input() {
        let mut inputs: Vec<String> = [
            "zh-TW", "zh_TW", "zh-tw", "ZH-HANT", "zh-Hant", "zh-HK", "zh", " zh ", "zh-CN", "zh_cn", "zh-Hans", "zh-SG",
            "zh-MO", "zh-Hant-TW", "cmn", "cmn-TW", "en", "EN", "en-US", "en-GB", "en_IN", "ja", "ja-JP", "JA_jp", "ko",
            "ko-KR", "es", "es-US", "es-ES", "es-419", "fr", "fr-FR", "fr-CA", "de", "de-DE", "de-CH", "pt", "pt-BR",
            "pt-PT", "it", "it-IT", "th", "th-TH", "ar", "ar-EG", "ru", "ru-RU", "hi", "hi-IN", "fil", "nb", "nn", "no",
            "iw", "in", "tl", "xx", "", "  ", "-", "--", "_", "english", "日本語", "it-", "-it",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        for code in OFFICIAL_LANGS {
            inputs.push(code.to_string());
            inputs.push(code.to_ascii_uppercase());
            inputs.push(code.replace('-', "_"));
        }
        for s in &inputs {
            let l = Lang::parse(s);
            assert_eq!(l, parse_before(s), "{s:?}");
            assert!(l.is_core(), "{s:?}");
        }
    }

    #[test]
    fn core_list() {
        assert_eq!(ALL, CORE);
        assert_eq!(CORE.len(), 9);
        for l in CORE {
            assert!(l.is_core());
            assert!(!l.rtl());
            assert_eq!(l.base(), l);
            assert_eq!(Lang::parse_any(l.code()), Some(l));
            assert_eq!(l.name_en(), phrases(l).name_en);
        }
    }

    #[test]
    fn every_official_code_round_trips() {
        assert_eq!(Ext::all().count(), 70);
        for info in &OFFICIAL_LANG_INFO {
            for input in [info.official.to_string(), info.official.to_ascii_lowercase(), info.official.replace('-', "_")] {
                let l = Lang::parse_any(&input).unwrap_or_else(|| panic!("{input}"));
                assert_eq!(l.is_core(), info.core, "{input}");
                assert_eq!(Lang::parse_any(l.code()), Some(l), "{input}");
                if !info.core {
                    assert_eq!(l.code(), info.code);
                    assert_eq!(l.name_en(), info.name_en);
                    assert_eq!(l.rtl(), info.rtl);
                    assert_eq!(l.base(), Lang::En);
                    let Lang::Ext(e) = l else { unreachable!() };
                    assert_eq!(e.official(), info.official);
                    assert_eq!(format!("{e:?}"), info.code);
                }
            }
        }
    }

    #[test]
    fn parse_any_examples() {
        assert_eq!(Lang::parse_any("it-IT"), Some(ext("it")));
        assert_eq!(Lang::parse_any("it_CH"), Some(ext("it")));
        assert_eq!(Lang::parse_any("th"), Some(ext("th")));
        assert_eq!(Lang::parse_any("ar-EG"), Some(ext("ar")));
        assert_eq!(Lang::parse_any("ar-SA"), Some(ext("ar")));
        assert_eq!(Lang::parse_any(" RU "), Some(ext("ru")));
        assert_eq!(Lang::parse_any("hi_IN"), Some(ext("hi")));
        assert_eq!(Lang::parse_any("fil-PH"), Some(ext("fil")));
        // Legacy and macro codes browsers and Android still send.
        assert_eq!(Lang::parse_any("no"), Some(ext("nb")));
        assert_eq!(Lang::parse_any("nb-NO"), Some(ext("nb")));
        assert_eq!(Lang::parse_any("nn"), Some(ext("nn")));
        assert_ne!(ext("nb"), ext("nn"));
        assert_eq!(Lang::parse_any("iw"), Some(ext("he")));
        assert_eq!(Lang::parse_any("in-ID"), Some(ext("id")));
        assert_eq!(Lang::parse_any("tl"), Some(ext("fil")));
        // Core languages as before, Mandarin's official code read as zh.
        assert_eq!(Lang::parse_any("cmn"), Some(Lang::ZhTw));
        assert_eq!(Lang::parse_any("zh_TW"), Some(Lang::ZhTw));
        assert_eq!(Lang::parse_any("zh-Hans"), Some(Lang::ZhCn));
        assert_eq!(Lang::parse_any("en-IN"), Some(Lang::En));
        assert_eq!(Lang::parse_any("es-US"), Some(Lang::Es));
        assert_eq!(Lang::parse_any("pt-BR"), Some(Lang::Pt));
        // Unknown stays unknown: no fallback to English.
        for s in ["", " ", "xx", "zh-MO", "yue", "nan", "hak", "-", "english"] {
            assert_eq!(Lang::parse_any(s), None, "{s:?}");
        }
        assert!(ext("ar").rtl() && ext("he").rtl() && ext("fa").rtl() && ext("ur").rtl());
        assert!(!ext("it").rtl() && !ext("th").rtl() && !ext("ru").rtl() && !ext("hi").rtl());
        assert_eq!(format!("{:?}", ext("th")), "Ext(th)");
        assert!(Ext::from_code("ar").unwrap() < Ext::from_code("ur").unwrap());
        assert_eq!(Ext::from_code("en"), None);
        assert_eq!(Ext::from_code("cmn"), None);
    }

    /// Until a non-core language has its own phrases and prompts, every entry
    /// point treats it exactly like English and none of them panics.
    #[test]
    fn every_ext_language_works_like_english() {
        // Without translations every other language works like English.
        crate::locales_store::testing::use_empty();
        use crate::stall_script::Family;
        let en = Lang::En;
        let dt = chrono::NaiveDate::from_ymd_opt(2026, 11, 21).unwrap().and_hms_opt(15, 30, 0).unwrap();
        let rt = crate::settings_store::RuntimeSettings::from_base(&crate::config::Settings::from_env());
        let regions = crate::country_profile::all();
        let families = [
            Family::Investment,
            Family::Insurance,
            Family::Credit,
            Family::Impersonation,
            Family::Collection,
            Family::Harassment,
            Family::General,
        ];
        let lines = [
            "Buongiorno, sono il corriere, ha un pacco per lei.",
            "สวัสดีครับ มีพัสดุมาส่งครับ",
            "مرحبا، لدي طرد لك، هل أنت في المنزل؟",
            "Здравствуйте, вам посылка, вы дома?",
            "नमस्ते, आपके लिए एक पार्सल है, क्या आप घर पर हैं?",
            "Hello, I have a delivery for you.",
        ];
        for e in Ext::all() {
            let l = Lang::Ext(e);
            assert!(std::ptr::eq(phrases(l), phrases(en)), "{l:?}");
            assert!(std::ptr::eq(crate::prompts::pack(l), crate::prompts::pack(en)), "{l:?}");
            assert!(std::ptr::eq(crate::control::words::words(l), crate::control::words::words(en)));
            for a in [false, true] {
                assert_eq!(crate::prompts::kick_open(l, a, None), crate::prompts::kick_open(en, a, None));
                assert_eq!(crate::prompts::kick_open_call(l, a, None, true), crate::prompts::kick_open_call(en, a, None, true));
                assert_eq!(crate::prompts::default_prompt(l, a, "Alex"), crate::prompts::default_prompt(en, a, "Alex"));
                assert_eq!(crate::prompts::default_prompt_template(l, a), crate::prompts::default_prompt_template(en, a));
                assert_eq!(crate::prompts::persona_chars(l, a, "Alex", ""), crate::prompts::persona_chars(en, a, "Alex", ""));
                assert_eq!(crate::contacts::caller_block(l, a, "Alex", "Sam", "note"), crate::contacts::caller_block(en, a, "Alex", "Sam", "note"));
                assert_eq!(crate::urgent_call::not_answered_turn(l, a, "Alex"), crate::urgent_call::not_answered_turn(en, a, "Alex"));
                assert_eq!(crate::urgent_call::callback_turn(l, a, "Alex"), crate::urgent_call::callback_turn(en, a, "Alex"));
                for who in [crate::urgent_call::TransferWho::Whitelist, crate::urgent_call::TransferWho::Contact, crate::urgent_call::TransferWho::Stranger] {
                    for will_call in [false, true] {
                        assert_eq!(
                            crate::contacts::urgent_block(l, a, "Alex", who, will_call),
                            crate::contacts::urgent_block(en, a, "Alex", who, will_call)
                        );
                    }
                }
            }
            let _ = crate::prompts::kick_open(Lang::ZhTw, false, Some(l));
            let _ = crate::prompts::call_language_note(l, Lang::Ja);
            let _ = crate::prompts::call_language_note(Lang::ZhTw, l);
            let _ = crate::prompts::language_drift_note(Lang::De, l);
            assert_eq!(crate::prompts::other_call_language_note(l), crate::prompts::other_call_language_note(en));
            assert_eq!(crate::prompts::owner_label(l, "Alex"), crate::prompts::owner_label(en, "Alex"));
            let aliases = vec!["Al".to_string()];
            assert_eq!(crate::prompts::alias_list(l, "Alex", &aliases), crate::prompts::alias_list(en, "Alex", &aliases));
            assert_eq!(crate::prompts::with_caller_language_note("x", l), crate::prompts::with_caller_language_note("x", en));
            assert_eq!(crate::prompts::wait_one_more(l), crate::prompts::wait_one_more(en));
            assert_eq!(rt.gemini_system_prompt_for_call_in(Some(l)), rt.gemini_system_prompt_for_call_in(Some(en)));
            assert_eq!(rt.persona_stall_system_prompt_in(Some(l)), rt.persona_stall_system_prompt_in(Some(en)));
            assert_eq!(rt.persona_user_chars(Some(l)), rt.persona_user_chars(Some(en)));
            // Owner-facing words: notifications, summaries, alerts.
            assert_eq!(crate::summary::summary_system_prompt(l, "Alex", &aliases), crate::summary::summary_system_prompt(en, "Alex", &aliases));
            assert_eq!(crate::summary::sales_labels(l), crate::summary::sales_labels(en));
            assert_eq!(crate::summary::summary_says_sales(l, "Sales call"), crate::summary::summary_says_sales(en, "Sales call"));
            assert_eq!(crate::extract::intent_system_prompt(l), crate::extract::intent_system_prompt(en));
            assert_eq!(crate::urgent_call::alert_title(l), crate::urgent_call::alert_title(en));
            for alert in [crate::urgent_call::Alert::AiEnded, crate::urgent_call::Alert::StillOnHold] {
                assert_eq!(crate::urgent_call::alert_line(alert, l), crate::urgent_call::alert_line(alert, en));
            }
            assert_eq!(crate::urgent_call::with_caller_language("x", Some(l)), crate::urgent_call::with_caller_language("x", Some(en)));
            assert_eq!(crate::urgency::language_name(l), "English");
            assert!(crate::urgency::is_latin_lang(l));
            assert_eq!(crate::direct_transfer_words(l), crate::direct_transfer_words(en));
            assert_eq!(for_owner_in(l, "Ciao"), "Ciao");
            assert_eq!(quoted(l, "ok"), quoted(en, "ok"));
            assert_eq!(date_time(l, &dt), date_time(en, &dt));
            assert_eq!(date_only(l, &dt.date()), date_only(en, &dt.date()));
            assert_eq!(duration(l, 75.0), duration(en, 75.0));
            assert_eq!(crate::zh_tw::to_tw_in(l, "这个"), crate::zh_tw::to_tw_in(en, "这个"));
            assert_eq!(crate::backup::failure_text(l, "io"), crate::backup::failure_text(en, "io"));
            assert_eq!(crate::lan::bad_host_words(l), crate::lan::bad_host_words(en));
            assert_eq!(crate::lan::page_words(l), crate::lan::page_words(en));
            let _ = crate::lan::need_password_page(l);
            assert_eq!(crate::appointments::sms_code_text(l, "busy"), crate::appointments::sms_code_text(en, "busy"));
            assert_eq!(crate::appointments::callback_headline(l, "+15550100"), crate::appointments::callback_headline(en, "+15550100"));
            assert_eq!(crate::appointments::callback_result_line(l, "busy", "+15550100"), crate::appointments::callback_result_line(en, "busy", "+15550100"));
            assert_eq!(crate::appointments::when_text(l, &dt), crate::appointments::when_text(en, &dt));
            assert_eq!(crate::calendar::extraction_prompt(l, "Alex", dt), crate::calendar::extraction_prompt(en, "Alex", dt));
            assert_eq!(crate::preflight::opening(l), crate::preflight::opening(en));
            assert_eq!(crate::preflight::card_text(l, &[]), crate::preflight::card_text(en, &[]));
            // Voice preview.
            assert_eq!(crate::voice_preview::sample_lines(l), crate::voice_preview::sample_lines(en));
            assert_eq!(crate::voice_preview::reject_announce_text_in(l), crate::voice_preview::reject_announce_text_in(en));
            // Regions, scenarios, stalling.
            assert_ne!(crate::country_profile::lang_default_country(l), "us");
            assert_eq!(crate::country_profile::prompt_overhead(l), crate::country_profile::prompt_overhead(en));
            assert_eq!(crate::country_profile::stall_overhead(l), crate::country_profile::stall_overhead(en));
            let _ = crate::country_profile::resolve(None, l);
            for r in regions.iter().copied().take(4) {
                assert_eq!(crate::country_profile::block(r, l, false), crate::country_profile::block(r, en, false));
                if crate::country_profile::has_lang_override(r, l) {
                    let hb = crate::country_profile::habits_block(r, l);
                    let sl = crate::country_profile::style_line(r, l, false, 3, "Alex");
                    assert!(!hb.is_empty() && hb != crate::country_profile::habits_block(r, en), "{r:?} {l:?}");
                    assert!(!sl.is_empty() && sl != crate::country_profile::style_line(r, en, false, 3, "Alex"), "{r:?} {l:?}");
                } else {
                    assert_eq!(crate::country_profile::habits_block(r, l), crate::country_profile::habits_block(r, en));
                    assert_eq!(crate::country_profile::style_line(r, l, false, 3, "Alex"), crate::country_profile::style_line(r, en, false, 3, "Alex"));
                }
                assert_eq!(crate::country_profile::stall_line(r, l), crate::country_profile::stall_line(r, en));
                assert_eq!(crate::country_profile::stall_block(r, l), crate::country_profile::stall_block(r, en));
                assert_eq!(crate::prompts::dialect_note(r, l), crate::prompts::dialect_note(r, en));
            }
            for f in families {
                assert_eq!(crate::stall_script::script(f, l), crate::stall_script::script(f, en));
            }
            for sc in crate::scenario::Scenario::ALL {
                for booking in [false, true] {
                    assert_eq!(crate::scenario::cue(sc, l, None, booking), crate::scenario::cue(sc, en, None, booking));
                }
            }
            assert_eq!(crate::scenario::note_mark(l), crate::scenario::note_mark(en));
            // Language checks on lines in several languages.
            for text in lines {
                assert_eq!(crate::summary::needs_translation(text, l), crate::summary::needs_translation(text, en), "{l:?}: {text}");
                assert_eq!(crate::urgency::latin_margin(text, l, Lang::Es), crate::urgency::latin_margin(text, en, Lang::Es));
                let _ = crate::summary::lines_to_translate(&format!("對方：{text}\n接聽：Ok"), l);
                let mut call = crate::lang_lock::CallLanguage::new(l, l);
                let _ = call.caller_turn(text);
                let _ = call.ai_turn(text, true);
            }
            let _ = crate::gemini_live::callback_lang_plan(l, en);
            let _ = crate::gemini_live::callback_lang_plan(Lang::Ja, l);
        }
    }
}
