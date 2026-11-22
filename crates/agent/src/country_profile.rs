// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Where a call is, and the local part of its prompt: where couriers leave
//! parcels, times and numbers said the local way, the local scams, the local
//! ways of talking and the talk-style slider (scenario plan, module 6; plan A2).
//! A region never limits the languages a call may speak: the caller is
//! followed into any language Gemini Live supports ([crate::prompts]).
//!
//! The regions come from the local data pack ([crate::packs_store]); this
//! module only decides which one a call gets and puts its fields into fixed
//! places. The hard lines stay in the prompts, never in the data.
//!
//! Decided from memory before the call is picked up (the ring prewarm has the
//! SIM): a table lookup, no I/O. Instructions are in the prompt's own
//! instruction language: Chinese for the two Chinese settings, English for the
//! others (as every other part of those prompts, [crate::prompts]); local words
//! stay in the local script.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use bageo_packs::{Hello, MultilangEntry, RegionData, Text};

use crate::locale::Lang;
use crate::packs_store;
use crate::prompts::LocalHello;

/// One region of the local data pack. Two regions are the same when their codes are.
#[derive(Clone, Copy)]
pub struct Region(&'static RegionData);

impl PartialEq for Region {
    fn eq(&self, other: &Self) -> bool {
        self.0.code == other.0.code
    }
}

impl Eq for Region {}

impl Hash for Region {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.code.hash(state);
    }
}

impl std::fmt::Debug for Region {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.code)
    }
}

impl Region {
    /// The region's code in the pack (tw, hk-mo, latam…).
    pub fn code(self) -> &'static str {
        &self.0.code
    }

    /// The region with this code in the pack in use.
    pub fn of_code(code: &str) -> Option<Region> {
        packs_store::current().region(code.trim().to_ascii_lowercase().as_str()).map(Region)
    }

    /// The region of a country (ISO 3166 alpha-2, any case); a country the pack
    /// does not list gets its default region. None for something that is not a
    /// country code.
    pub fn of_country(iso: &str) -> Option<Region> {
        let iso = iso.trim().to_ascii_lowercase();
        if !bageo_packs::data::is_country(&iso) {
            return None;
        }
        let t = packs_store::current();
        t.region_of_country(&iso).or_else(|| t.region("default")).map(Region)
    }
}

/// Every region of the pack in use.
pub fn all() -> Vec<Region> {
    packs_store::current().regions.values().map(Region).collect()
}

/// The country a language setting stands for when nothing else says where the call is.
pub fn lang_default_country(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => "tw",
        Lang::ZhCn => "cn",
        Lang::En => "us",
        Lang::Ext(e) => e.country(),
        Lang::Ja => "jp",
        Lang::Ko => "kr",
        Lang::Es => "mx",
        Lang::Fr => "fr",
        Lang::De => "de",
        Lang::Pt => "br",
    }
}

/// A call's region ([resolve_with]) with the owner's home country from the settings.
pub fn resolve(sim: Option<&str>, lang: Lang) -> Option<Region> {
    let home = crate::locale::active().home_country;
    resolve_with(sim, home.as_deref(), lang)
}

/// A call's region: the country of the SIM that took it; when the SIM does not
/// say, the owner's home country; else the language setting's country.
pub fn resolve_with(sim: Option<&str>, home: Option<&str>, lang: Lang) -> Option<Region> {
    let given = |s: Option<&str>| s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    match (given(sim), given(home)) {
        (Some(s), _) | (None, Some(s)) => Region::of_country(&s),
        (None, None) => Region::of_country(lang_default_country(lang)),
    }
}

/// Regions where callers may speak a Chinese dialect Gemini Live does not
/// speak (Taiwanese / Hokkien, Hakka, Cantonese): the AI answers them in
/// Mandarin ([crate::prompts::scope_to_region]).
pub fn chinese_dialects_heard(region: Region) -> bool {
    region.0.chinese_dialects
}

/// The kinds of local rule a region has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Where couriers leave a parcel.
    Delivery,
    /// Times and numbers said the local way, to read back.
    Readback,
    /// Scams of the place, as the institutions they claim to be.
    Fraud,
}

/// A text of the pack in the instruction language of lang's prompts.
fn pick(t: &Text, lang: Lang) -> String {
    match lang {
        Lang::ZhTw => t.zh.clone(),
        Lang::ZhCn => crate::zh_convert::to_cn(&t.zh),
        _ => t.en.clone(),
    }
}

/// The headings of a region's parts in one instruction language, and how its
/// rules are joined. Kept short: every character is in every call's prompt
/// ([bageo_packs::check::PROMPT_SUM_MAX] is worked out with them).
struct Heads {
    local: &'static str,
    ways: &'static str,
    style: &'static str,
    stall: &'static str,
    sep: &'static str,
    end: &'static str,
}

const HEADS_EN: Heads =
    Heads { local: "[Local] ", ways: "[Local ways] ", style: "[Talk style] ", stall: "[Local snags] ", sep: "; ", end: "." };
const HEADS_TW: Heads = Heads { local: "【當地】", ways: "【當地習慣】", style: "【講話風格】", stall: "【當地狀況】", sep: "；", end: "。" };
const HEADS_CN: Heads = Heads { local: "【当地】", ways: "【当地习惯】", style: "【讲话风格】", stall: "【当地状况】", sep: "；", end: "。" };

fn heads(lang: Lang) -> &'static Heads {
    match lang {
        Lang::ZhTw => &HEADS_TW,
        Lang::ZhCn => &HEADS_CN,
        _ => &HEADS_EN,
    }
}

/// Most characters a normal call's region part adds besides the pack's texts:
/// three line breaks, the three headings, two joins and the full stop.
#[cfg(test)]
pub fn prompt_overhead(lang: Lang) -> usize {
    let h = heads(lang);
    let n = |s: &str| s.chars().count();
    3 + n(h.local) + n(h.ways) + n(h.style) + 2 * n(h.sep) + n(h.end)
}

/// The same for the stalling prompt's region part ([stall_block]: the scams and the snags).
#[cfg(test)]
pub fn stall_overhead(lang: Lang) -> usize {
    let h = heads(lang);
    let n = |s: &str| s.chars().count();
    2 + n(h.local) + n(h.stall) + n(h.end)
}

/// One local fact of a region (how things are done there, said as background,
/// never as an order or a line) in the instruction language of lang's prompts,
/// without the paragraph's heading — e.g. the delivery line a scenario hint uses.
pub fn line(region: Region, kind: Kind, lang: Lang) -> Option<String> {
    let d = region.0;
    let t = match kind {
        Kind::Delivery => Some(&d.delivery),
        Kind::Readback => d.readback.as_ref(),
        Kind::Fraud => Some(&d.fraud),
    }?;
    Some(pick(t, lang)).filter(|s| !s.trim().is_empty())
}

/// The region's paragraph for a prompt in lang ("【當地】…" / "[Local] …"), led
/// by a line break; empty when the region has no rule. skip_delivery: the
/// prompt says where parcels go already.
pub fn block(region: Region, lang: Lang, skip_delivery: bool) -> String {
    let kinds = [Kind::Delivery, Kind::Readback, Kind::Fraud];
    let lines: Vec<String> = kinds
        .iter()
        .filter(|k| !(skip_delivery && **k == Kind::Delivery))
        .filter_map(|k| line(region, *k, lang))
        .map(|l| l.trim_end_matches(['。', '.', ';', '；']).to_string())
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    let h = heads(lang);
    format!("\n{}{}{}", h.local, lines.join(h.sep), h.end)
}

/// A word of the region's delivery rule, in the instruction language of lang:
/// a prompt that has it says where parcels go already (the Taiwan prompt's
/// 「管理室」).
pub fn delivery_key(region: Region, lang: Lang) -> Option<&'static str> {
    let k = region.0.delivery_key.as_ref()?;
    if lang.is_zh() { k.zh.as_deref() } else { k.en.as_deref() }.filter(|s| !s.is_empty())
}

/// The regional identity and speaking-style sentence placed right at the front
/// of a call's prompt (`【你是誰】` / `[Who you are] `): tells the model where
/// it is from and to use that place's speaking style and accent. Empty for the
/// generic fallback region (`default`).
pub fn identity_line(region: Region, lang: Lang) -> String {
    if region.0.code == "default" {
        return String::new();
    }
    if lang.is_zh() {
        let place = region
            .0
            .name
            .zh
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&region.0.name.en);
        let multi = place.contains('、')
            || place.contains('與')
            || place.ends_with('區')
            || place.ends_with('家');
        let tw = if multi {
            format!("你是{place}當地人，請多用當地人說話方式跟腔調。")
        } else {
            format!("你是{place}人，請多用{place}人說話方式跟腔調。")
        };
        if lang == Lang::ZhCn {
            crate::zh_convert::to_cn(&tw)
        } else {
            tw
        }
    } else {
        let place = region.0.name.en.trim();
        if place.is_empty() {
            String::new()
        } else {
            format!("You are from {place}; use the local speaking style and accent. ")
        }
    }
}

fn multilang_styles() -> &'static BTreeMap<(String, String), MultilangEntry> {
    let cur = packs_store::current();
    if !cur.multilang.is_empty() {
        &cur.multilang
    } else {
        &packs_store::builtin().multilang
    }
}

fn lang_override(region: Region, lang: Lang) -> Option<&'static MultilangEntry> {
    multilang_styles().get(&(region.code().to_string(), lang.code().to_string()))
}

/// Whether `bageo-pack` defines a language-specific speech/style
/// override for `(region, lang)`.
pub fn has_lang_override(region: Region, lang: Lang) -> bool {
    lang_override(region, lang).is_some()
}

/// Region+language local phone greeting override from `bageo-pack`,
/// when `(region, lang)` has one.
pub fn lang_hello(region: Region, lang: Lang) -> Option<&'static Hello> {
    lang_override(region, lang).map(|m| &m.hello)
}

/// The region's local ways of talking (words to use and avoid, date order…),
/// led by a line break; empty when it has none. Only with the built-in prompt.
pub fn habits_block(region: Region, lang: Lang) -> String {
    let text = lang_override(region, lang).map(|m| &m.habits).unwrap_or(&region.0.habits);
    let t = pick(text, lang);
    if t.trim().is_empty() {
        return String::new();
    }
    format!("\n{}{t}", heads(lang).ways)
}

/// The talk-style slider's level (1 casual to 5 businesslike, clamped) for the
/// owner's own voice or the assistant, as the region says it; owner (already
/// cleaned, [crate::prompts::owner_label]) fills {owner}. Led by a line break.
/// It goes after the local rules and ways and before the owner's notes and the
/// hard lines ([crate::prompts::scope_to_region]), which come last and win.
pub fn style_line(region: Region, lang: Lang, assistant: bool, level: u8, owner: &str) -> String {
    let ts = lang_override(region, lang).map(|m| &m.talk_style).unwrap_or(&region.0.talk_style);
    let cells = if assistant { &ts.assistant } else { &ts.persona };
    let i = usize::from(level.clamp(1, 5)) - 1;
    let text: String = pick(&cells[i], lang).replace("{owner}", owner).chars().filter(|c| *c != '{' && *c != '}').collect();
    if text.trim().is_empty() {
        return String::new();
    }
    format!("\n{}{text}", heads(lang).style)
}

/// The region's own snags for keeping a sales / scam caller talking (the local
/// bank app, the local payment tool…), led by a line break; empty when it has none.
pub fn stall_line(region: Region, lang: Lang) -> String {
    let Some(t) = region.0.stall_local.as_ref() else { return String::new() };
    let t = pick(t, lang);
    if t.trim().is_empty() {
        return String::new();
    }
    format!("\n{}{t}", heads(lang).stall)
}

/// The stalling prompt's region part: the local scams and the region's
/// stalling snags ([stall_line]); parcels and read-back are no matter there.
pub fn stall_block(region: Region, lang: Lang) -> String {
    let fraud = line(region, Kind::Fraud, lang)
        .map(|l| format!("\n{}{}{}", heads(lang).local, l.trim_end_matches(['。', '.', ';', '；']), heads(lang).end))
        .unwrap_or_default();
    format!("{fraud}{}", stall_line(region, lang))
}

/// The speech-to-text code a country's region names (ISO 3166 alpha-2, any
/// case); None where the region gives none or the pack does not list the country.
pub fn stt_of_country(iso: &str) -> Option<&'static str> {
    let iso = iso.trim().to_ascii_lowercase();
    packs_store::current().region_of_country(&iso)?.stt.as_deref()
}

/// How a country whose language the AI has no setting for answers the phone
/// (ISO 3166 alpha-2, any case): its region's opening. None for a country of
/// one of the nine settings languages ([crate::locale::country_lang]): those
/// keep the setting's opening; and where Chinese callers are expected
/// (chinese_dialects: Malaysia), which keeps the setting's opening as before.
pub fn hello_of_country(iso: &str) -> Option<&'static LocalHello> {
    let iso = iso.trim().to_ascii_lowercase();
    if crate::locale::country_lang(&iso).is_some() {
        return None;
    }
    let t = packs_store::current();
    let code = t.countries.get(&iso)?;
    if t.region(code).is_some_and(|r| r.chinese_dialects) {
        return None;
    }
    t.hello(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(code: &str) -> Region {
        Region::of_code(code).unwrap_or_else(|| panic!("no region {code}"))
    }

    /// The 25 regions the code had before the pack (old name, its countries,
    /// the regions they belong to now): every country still has a region.
    const LEGACY: [(&str, &[&str], &[&str]); 25] = [
        ("Tw", &["tw"], &["tw"]),
        ("HkMo", &["hk", "mo"], &["hk-mo"]),
        ("Cn", &["cn"], &["cn"]),
        ("MySg", &["my", "sg"], &["my", "sg"]),
        (
            "Us",
            &["us", "ag", "ai", "as", "bb", "bm", "bs", "dm", "gd", "gu", "jm", "kn", "ky", "lc", "mp", "ms", "sx", "tc", "tt", "vc", "vg", "vi"],
            &["us"],
        ),
        ("Ca", &["ca"], &["ca"]),
        ("GbIe", &["gb", "ie", "im", "je", "gg"], &["gb-ie"]),
        ("AuNz", &["au", "nz"], &["au-nz"]),
        ("In", &["in"], &["in"]),
        ("It", &["it", "sm", "va"], &["it"]),
        ("Vn", &["vn"], &["vn"]),
        ("Th", &["th"], &["th"]),
        ("Jp", &["jp"], &["jp"]),
        ("Kr", &["kr"], &["kr"]),
        ("EsEu", &["es"], &["es"]),
        (
            "EsAm",
            &["mx", "co", "cl", "pe", "ve", "ec", "bo", "py", "gt", "hn", "sv", "ni", "cr", "pa", "cu", "do", "pr"],
            &["mx", "latam"],
        ),
        ("EsRp", &["ar", "uy"], &["ar-uy"]),
        ("Fr", &["fr", "re", "yt", "gp", "bl", "mf", "mq", "gf", "pm", "nc", "pf", "wf", "mc", "lu"], &["fr", "lu"]),
        ("Be", &["be"], &["be"]),
        ("De", &["de"], &["de"]),
        ("At", &["at"], &["at"]),
        ("Ch", &["ch", "li"], &["ch"]),
        ("Br", &["br"], &["br"]),
        ("Pt", &["pt"], &["pt"]),
        ("Tr", &["tr"], &["tr"]),
    ];

    #[test]
    fn every_country_of_the_old_regions_still_has_one() {
        for (old, countries, now) in LEGACY {
            for c in countries {
                let got = packs_store::current().countries.get(*c).map(String::as_str);
                assert!(got.is_some_and(|g| now.contains(&g)), "{old}: {c} -> {got:?}, want one of {now:?}");
                assert!(Region::of_country(c).is_some_and(|r| now.contains(&r.code())), "{old}: {c}");
            }
        }
        assert!(all().len() >= 74, "{}", all().len());
    }

    /// Longest region paragraph (delivery, read-back, scams): Chinese characters,
    /// and English (about the same number of tokens).
    const BLOCK_MAX_CHARS_ZH: usize = 140;
    const BLOCK_MAX_CHARS_EN: usize = 320;

    #[test]
    fn every_regions_paragraph_is_short() {
        for r in all() {
            for lang in crate::locale::ALL {
                let b = block(r, lang, false);
                let n = b.trim().chars().count();
                let max = if lang.is_zh() { BLOCK_MAX_CHARS_ZH } else { BLOCK_MAX_CHARS_EN };
                assert!(n > 0 && n <= max, "{r:?} {lang:?}: {n} {b}");
                assert!(!b.contains('{') && !habits_block(r, lang).contains('{'), "{r:?} {lang:?}");
                assert!(!stall_line(r, lang).contains('{'), "{r:?} {lang:?}");
            }
        }
    }

    #[test]
    fn sim_first_then_home_then_language() {
        assert_eq!(resolve_with(Some("TW"), None, Lang::En), Some(r("tw")));
        assert_eq!(resolve_with(Some("mo"), None, Lang::ZhTw), Some(r("hk-mo")));
        assert_eq!(resolve_with(Some("sg"), None, Lang::ZhCn), Some(r("sg")));
        assert_eq!(resolve_with(Some("uy"), None, Lang::Es), Some(r("ar-uy")));
        assert_eq!(resolve_with(Some("it"), None, Lang::ZhTw), Some(r("it")));
        assert_eq!(resolve_with(Some("TR"), None, Lang::En), Some(r("tr")));
        // The SIM beats the home country; the home country beats the language.
        assert_eq!(resolve_with(Some("jp"), Some("de"), Lang::En), Some(r("jp")));
        assert_eq!(resolve_with(None, Some("de"), Lang::En), Some(r("de")));
        assert_eq!(resolve_with(Some(" "), Some(""), Lang::Ja), Some(r("jp")));
        assert_eq!(resolve_with(None, None, Lang::ZhTw), Some(r("tw")));
        assert_eq!(resolve_with(None, None, Lang::Es), Some(r("mx")));
        assert_eq!(resolve_with(None, None, Lang::Pt), Some(r("br")));
        // A country the pack does not list: its default region; not a country: none.
        assert_eq!(resolve_with(Some("aq"), None, Lang::ZhTw), Some(r("default")));
        assert_eq!(resolve_with(Some("zz"), None, Lang::ZhTw), None);
        // Settings unset (unit tests): no home country.
        assert_eq!(resolve(None, Lang::De), Some(r("de")));
    }

    /// A language outside the nine core ones stands for its own country
    /// (Italian for Italy, Thai for Thailand), not for the United States.
    #[test]
    fn an_extra_language_stands_for_its_own_country() {
        for e in crate::locale::Ext::all() {
            let c = lang_default_country(Lang::Ext(e));
            assert!(bageo_packs::data::is_country(c), "{e:?}: {c}");
            assert_ne!(c, "us", "{e:?}");
            // Where the official code names a country (it-IT, th-TH), it is that one.
            if let Some((_, cc)) = e.official().split_once('-') {
                if cc.len() == 2 {
                    assert_eq!(c, cc.to_ascii_lowercase(), "{e:?}");
                }
            }
            assert!(resolve_with(None, None, Lang::Ext(e)).is_some(), "{e:?}");
        }
        for (code, country) in [
            ("it", "it"),
            ("th", "th"),
            ("vi", "vn"),
            ("id", "id"),
            ("hi", "in"),
            ("ar", "eg"),
            ("ru", "ru"),
            ("he", "il"),
            ("fa", "ir"),
            ("sv", "se"),
        ] {
            let lang = Lang::Ext(crate::locale::Ext::from_code(code).expect(code));
            assert_eq!(lang_default_country(lang), country, "{code}");
            assert_eq!(resolve_with(None, None, lang), Region::of_country(country), "{code}");
            // The SIM and the home country still come first.
            assert_eq!(resolve_with(Some("jp"), None, lang), Some(r("jp")), "{code}");
            assert_eq!(resolve_with(None, Some("de"), lang), Some(r("de")), "{code}");
        }
    }

    #[test]
    fn a_line_is_in_the_prompts_instruction_language() {
        let tw = line(r("tw"), Kind::Delivery, Lang::ZhTw).unwrap();
        assert!(tw.contains("管理室"), "{tw}");
        let cn = line(r("cn"), Kind::Fraud, Lang::ZhCn).unwrap();
        assert!(cn.contains("诈骗"), "{cn}");
        assert!(line(r("de"), Kind::Delivery, Lang::De).unwrap().contains("Packstation"));
        for (code, lang) in [("us", Lang::En), ("jp", Lang::Ja), ("kr", Lang::Ko), ("es", Lang::Es), ("fr", Lang::Fr), ("br", Lang::Pt), ("gulf", Lang::En)] {
            assert!(line(r(code), Kind::Fraud, lang).unwrap().contains("scam"), "{code}");
        }
        assert!(line(r("th"), Kind::Fraud, Lang::ZhTw).unwrap().contains("詐騙"));
        // The prompt already says where parcels go: only the rest.
        let b = block(r("tw"), Lang::ZhTw, true);
        assert!(!b.contains("管理室") && b.contains("詐騙"), "{b}");
        assert_eq!(delivery_key(r("tw"), Lang::ZhTw), Some("管理室"));
        assert_eq!(delivery_key(r("us"), Lang::En), None);
    }

    #[test]
    fn the_five_levels_differ_and_fill_in_the_owner() {
        for code in ["tw", "us", "jp", "de", "kr", "es", "fr", "br", "th", "eg"] {
            for lang in [Lang::ZhTw, Lang::ZhCn, Lang::En, Lang::Ja, Lang::De] {
                for assistant in [false, true] {
                    let lines: Vec<String> = (1..=5).map(|l| style_line(r(code), lang, assistant, l, "Alex")).collect();
                    for (i, a) in lines.iter().enumerate() {
                        assert!(!a.is_empty() && !a.contains('{') && !a.contains('}'), "{code} {lang:?} {a}");
                        for b in &lines[i + 1..] {
                            assert_ne!(a, b, "{code} {lang:?} assistant={assistant}");
                        }
                    }
                    // Out of range is held to 1..5.
                    assert_eq!(style_line(r(code), lang, assistant, 0, "Alex"), lines[0]);
                    assert_eq!(style_line(r(code), lang, assistant, 9, "Alex"), lines[4]);
                }
            }
        }
        let tw = style_line(r("tw"), Lang::ZhTw, false, 1, "王先生");
        assert!(tw.starts_with("\n【講話風格") && tw.contains("王先生"), "{tw}");
        let cn = style_line(r("tw"), Lang::ZhCn, true, 1, "王先生");
        assert!(cn.starts_with("\n【讲话风格"), "{cn}");
        let us = style_line(r("us"), Lang::En, false, 1, "Alex");
        assert!(us.starts_with("\n[Talk style] ") && us.contains("Alex"), "{us}");
    }

    #[test]
    fn opening_and_speech_codes_come_from_the_pack() {
        // Countries of the nine settings languages keep the setting's opening.
        for c in ["es", "fr", "de", "us", "tw", "jp", "kr", "br", "mx", "be"] {
            assert!(hello_of_country(c).is_none(), "{c}");
        }
        assert!(hello_of_country("ad").is_some(), "Andorra");
        // Kept as before: English in India and the Philippines, Hindi heard in India.
        for c in ["in", "ph"] {
            let h = hello_of_country(c).unwrap();
            assert_eq!((h.code, h.name_en), ("en", "English"), "{c}");
        }
        assert_eq!(stt_of_country("IN"), Some("hi-IN"));
        assert_eq!(stt_of_country("us"), None);
        assert_eq!(stt_of_country("th"), Some("th-TH"));
        assert_eq!(stt_of_country("ua"), Some("uk-UA"));
        assert_eq!(stt_of_country("zz"), None);
        let it = hello_of_country("IT").unwrap();
        assert_eq!(it.code, "it");
        assert!(!it.hello.is_empty() && !it.hello_again.is_empty());
        let th = hello_of_country("th").unwrap();
        assert!(th.hello.chars().any(|c| ('\u{0E00}'..='\u{0E7F}').contains(&c)), "{}", th.hello);
        assert!(hello_of_country("sa").unwrap().hello.chars().any(|c| ('\u{0600}'..='\u{06FF}').contains(&c)));
    }

    #[test]
    fn the_dialect_regions_are_the_chinese_ones() {
        for c in ["tw", "hk-mo", "cn", "sg", "my"] {
            assert!(chinese_dialects_heard(r(c)), "{c}");
        }
        for c in ["us", "jp", "de", "ch", "pt", "ca", "th"] {
            assert!(!chinese_dialects_heard(r(c)), "{c}");
        }
    }

    #[test]
    fn multilang_overrides_cover_all_51_pairs_and_fit_budgets() {
        let map = multilang_styles();
        assert_eq!(map.len(), 51, "expected 51 region/lang overrides");
        for ((code, lang_code), style) in map {
            let reg = r(code);
            let lang = Lang::parse_any(lang_code).unwrap_or_else(|| panic!("invalid lang {lang_code}"));
            assert!(has_lang_override(reg, lang), "{code}/{lang_code}");
            assert!(lang_hello(reg, lang).is_some(), "{code}/{lang_code}");
            assert!(!style.hello.hello.trim().is_empty() && !style.hello.hello_again.trim().is_empty(), "{code}/{lang_code}");

            let h = habits_block(reg, lang);
            assert!(!h.trim().is_empty() && !h.contains('{') && !h.contains('}'), "{code}/{lang_code}: {h}");

            for assistant in [false, true] {
                let lines: Vec<String> = (1..=5).map(|l| style_line(reg, lang, assistant, l, "Alex")).collect();
                for (i, a) in lines.iter().enumerate() {
                    assert!(!a.is_empty() && !a.contains('{') && !a.contains('}'), "{code}/{lang_code} {a}");
                    for b in &lines[i + 1..] {
                        assert_ne!(a, b, "{code}/{lang_code} assistant={assistant}");
                    }
                }
            }

            let base_en = reg.0.delivery.en.chars().count()
                + reg.0.readback.as_ref().map_or(0, |t| t.en.chars().count())
                + reg.0.fraud.en.chars().count();
            let base_zh = reg.0.delivery.zh.chars().count()
                + reg.0.readback.as_ref().map_or(0, |t| t.zh.chars().count())
                + reg.0.fraud.zh.chars().count();
            let cells = style.talk_style.persona.iter().chain(style.talk_style.assistant.iter());
            let max_cell_en = cells.clone().map(|t| t.en.chars().count()).max().unwrap_or(0);
            let max_cell_zh = cells.map(|t| t.zh.chars().count()).max().unwrap_or(0);
            let sum_en = base_en + style.habits.en.chars().count() + max_cell_en;
            let sum_zh = base_zh + style.habits.zh.chars().count() + max_cell_zh;
            assert!(
                sum_en <= bageo_packs::check::PROMPT_SUM_MAX.0 && sum_zh <= bageo_packs::check::PROMPT_SUM_MAX.1,
                "{code}/{lang_code}: prompt sum ({sum_en}, {sum_zh}) exceeded {:?}",
                bageo_packs::check::PROMPT_SUM_MAX
            );
        }
        assert_ne!(habits_block(r("ca"), Lang::Fr), habits_block(r("ca"), Lang::En));
        assert_ne!(style_line(r("ca"), Lang::Fr, false, 2, "Alex"), style_line(r("ca"), Lang::En, false, 2, "Alex"));
        assert_ne!(habits_block(r("sg"), Lang::ZhTw), habits_block(r("sg"), Lang::En));
        assert!(habits_block(r("sg"), Lang::ZhCn).contains("组屋"));
        assert!(habits_block(r("ch"), Lang::Fr).contains("natel"));
        assert!(habits_block(r("us"), Lang::Es).contains("Zelle"));
    }
}
