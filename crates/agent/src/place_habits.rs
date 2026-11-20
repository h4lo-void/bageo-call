// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! What businesses in a region usually ask on the phone, for AI errands. The
//! pre-dial check ([crate::preflight]) reads which details a kind of call
//! needs there, what it may also ask, what the owner should know before
//! dialing, and how owners write times, names, numbers and the purpose of a
//! call ([lexicon]). The call's script gets a block on what the other side
//! tends to ask and how to handle it ([call_block]), and the result of the
//! call says how such places confirm ([Region::callback_habits]).
//!
//! The data is `data/place_habits.json`, one object per region (a region may
//! cover several countries). The number called decides the region; a number
//! outside every region gets none of it and the general rules apply.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

const DATA: &str = include_str!("../data/place_habits.json");

/// The kinds of business call the data describes.
pub const TYPES: [&str; 17] = [
    "restaurant", "clinic", "beauty", "repair", "takeout", "shop_order", "change", "inquiry", "hotel", "activity", "home_service", "pet",
    "tracking", "customer_service", "transport", "property", "government_school",
];

/// Every detail code the data may name, each with the words the script
/// uses for it.
pub const CODES: [(&str, &str); 32] = [
    ("time", "the day and time"),
    ("party", "how many people"),
    ("phone", "a number to call back"),
    ("name", "the name it goes under"),
    ("item", "what it is for"),
    ("contact", "a name and a number"),
    ("backup", "other times that would work"),
    ("spec", "what and how many"),
    ("pickup", "when to pick it up"),
    ("substitute", "what to take if it is sold out"),
    ("orig_time", "the time of the original booking"),
    ("address", "the address"),
    ("for_whom", "who it is for"),
    ("kids_detail", "children's ages and a high chair"),
    ("visit_type", "a first or a return visit"),
    ("staff_pref", "a preferred staff member"),
    ("seating_pref", "where to sit"),
    ("checkin_out", "the check-in and check-out dates"),
    ("guests", "how many guests"),
    ("room_type", "the kind of room"),
    ("duration", "how long"),
    ("reference_no", "a booking, order or tracking number"),
    ("pet_detail", "the pet's kind, breed and size"),
    ("pickup_place", "where to be picked up"),
    ("dropoff_place", "where to go"),
    ("budget", "the budget"),
    ("question", "the question"),
    ("vehicle_plate", "the licence plate"),
    ("floor_elevator", "the floor and whether there is a lift"),
    ("customization", "sweetness, ice or spice"),
    ("name_kana", "how the name reads in kana"),
    ("allergy", "allergies or foods to avoid"),
];

/// The most characters of the script's block.
pub const CALL_BLOCK_MAX: usize = 1600;
/// The most characters of one thing they ask or one situation in the block.
const ITEM_MAX_CHARS: usize = 80;
/// The most characters of one local habit (names, phone numbers, times) in the block.
const TIP_MAX_CHARS: usize = 140;
/// The most characters of how far English goes there, in the block.
const ENGLISH_MAX_CHARS: usize = 90;
/// The most characters of a kind's note before dialing, for the pre-dial model.
const BEFORE_MAX_CHARS: usize = 200;
/// The most things they usually ask, in the block.
const ASKED_MAX: usize = 5;
/// The most situations of one way of handling them, in the block.
const HANDLED_MAX: usize = 3;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Data {
    regions: Vec<Region>,
}

/// One region's habits, under the data file's own field names.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Region {
    /// Its id (`tw`, `us`, `hk-mo`).
    pub region: String,
    /// The countries it covers (ISO 3166 alpha-2, lowercase).
    pub countries: Vec<String>,
    /// Its name in English, for the script.
    pub region_name: String,
    /// Which language to speak there.
    pub ai_language_tip: String,
    /// How far English goes there (`often; ...`).
    pub english_ok: String,
    pub politeness: String,
    pub name_tip: String,
    pub phone_tip: String,
    pub time_tip: String,
    pub number_tip: String,
    /// The busy hours not to call in.
    pub avoid_calling: String,
    /// The days places there are closed.
    pub closed_days: String,
    pub booking_channels: String,
    /// Which places answer with a voice menu.
    pub voice_menu: String,
    /// How places there confirm a booking (a text, a message, a call the day before).
    pub callback_habits: String,
    /// By kind of call ([TYPES]).
    pub types: BTreeMap<String, Place>,
    /// By language: how owners write the details of a call there.
    pub how_owners_write: BTreeMap<String, Writing>,
}

/// What one kind of call is like in a region.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Place {
    /// How often such places take bookings by phone (`often`, `rarely`, …).
    pub phone_booking: String,
    /// What the owner should know before dialing, in Chinese (the model words it in the owner's language).
    pub before_dialing_zh: String,
    /// Whose matter it is counts (a patient, a guest): the owner's name does not stand for someone else's.
    pub for_whom_matters: bool,
    /// The details this kind of call needs before it dials ([CODES]).
    pub must_have: Vec<String>,
    /// The details they may also ask about ([CODES]): the owner may add them.
    pub nice_to_have: Vec<String>,
    /// What the other side usually asks, in English.
    pub usually_asked: Vec<String>,
    pub may_come_up: Vec<Situation>,
    pub sensitive_asked: Vec<Situation>,
}

/// Something that may come up on the call and how to handle it.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Situation {
    pub what: String,
    /// `ai_can_answer`, `ask_owner`, `owner_only` or `decline`.
    pub handle: String,
}

/// How owners write the details of a call in one language: examples, each
/// maybe with a note in brackets after it.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Writing {
    pub party: Vec<String>,
    pub time: Vec<String>,
    pub date: Vec<String>,
    pub name: Vec<String>,
    pub phone: Vec<String>,
    pub booking_words: Vec<String>,
    pub appointment_words: Vec<String>,
    pub order_words: Vec<String>,
    pub change_words: Vec<String>,
    pub inquiry_words: Vec<String>,
    /// Look-alikes: a number that is no party, time or date ("4樓", "for 2 hours").
    pub false_friends: Vec<String>,
}

/// All regions of the data (the tests make sure it reads).
pub fn regions() -> &'static [Region] {
    static R: OnceLock<Vec<Region>> = OnceLock::new();
    R.get_or_init(|| serde_json::from_str::<Data>(DATA).map(|d| d.regions).unwrap_or_default())
}

/// The region a country (ISO 3166 alpha-2) belongs to.
pub fn for_country(iso: &str) -> Option<&'static Region> {
    let iso = iso.trim();
    if iso.is_empty() {
        return None;
    }
    regions().iter().find(|r| r.countries.iter().any(|c| c.eq_ignore_ascii_case(iso)))
}

/// The region of the number called; `home` (the owner's country) reads a
/// number written without its country code.
pub fn for_number(number: &str, home: &str) -> Option<&'static Region> {
    let home = Some(home.trim()).filter(|h| !h.is_empty());
    for_country(crate::phone_region::normalize(number, home)?.country)
}

/// A kind of call as the data names it, or `contact`; anything else is empty.
pub fn clean_type(s: &str) -> String {
    let t = s.trim().to_ascii_lowercase();
    if t == "contact" || TYPES.contains(&t.as_str()) {
        t
    } else {
        String::new()
    }
}

fn code(s: &str) -> Option<&'static str> {
    CODES.iter().map(|(c, _)| *c).find(|c| *c == s.trim())
}

/// The script's words for a detail code ("" for an unknown one).
pub fn code_words(c: &str) -> &'static str {
    CODES.iter().find(|(k, _)| *k == c).map(|(_, w)| *w).unwrap_or("")
}

/// What a kind of call is like there.
pub fn place<'a>(region: Option<&'a Region>, kind: &str) -> Option<&'a Place> {
    region.and_then(|r| r.types.get(kind))
}

/// The details a kind of call needs before it dials: the region's, or with
/// no region the ones most regions name.
pub fn must_have(region: Option<&Region>, kind: &str) -> Vec<&'static str> {
    if let Some(p) = place(region, kind) {
        return p.must_have.iter().filter_map(|c| code(c)).collect();
    }
    let all = regions();
    let mut out: Vec<&'static str> = Vec::new();
    for r in all {
        for c in r.types.get(kind).map(|p| p.must_have.as_slice()).unwrap_or_default() {
            let Some(c) = code(c) else { continue };
            let named = all.iter().filter(|x| x.types.get(kind).is_some_and(|p| p.must_have.iter().any(|m| m == c))).count();
            if named * 2 > all.len() && !out.contains(&c) {
                out.push(c);
            }
        }
    }
    out
}

/// What the other side may also ask about on this kind of call there, beyond
/// what it needs ([must_have]); none without a region.
pub fn nice_to_have(region: Option<&Region>, kind: &str) -> Vec<&'static str> {
    let Some(p) = place(region, kind) else {
        return Vec::new();
    };
    p.nice_to_have.iter().filter_map(|c| code(c)).filter(|c| !p.must_have.iter().any(|m| m == c)).collect()
}

/// Whose matter it is counts for this kind of call there (with no region:
/// in most regions).
pub fn for_whom_matters(region: Option<&Region>, kind: &str) -> bool {
    match region {
        Some(r) => r.types.get(kind).is_some_and(|p| p.for_whom_matters),
        None => {
            let all = regions();
            all.iter().filter(|r| r.types.get(kind).is_some_and(|p| p.for_whom_matters)).count() * 2 > all.len()
        }
    }
}

/// Such places there seldom or never get this done by phone (`rarely`, `no`).
/// No region: never.
pub fn hard_by_phone(region: Option<&Region>, kind: &str) -> bool {
    place(region, kind).is_some_and(|p| matches!(p.phone_booking.trim().to_ascii_lowercase().as_str(), "rarely" | "no"))
}

/// Words of the owner's own identity papers in the data (its English and the
/// local words in brackets): an ID, passport or residence number, a date of
/// birth, a health insurance card, a social security number. Not a card
/// number, a deposit, a password, a door or verification code, a driving
/// licence (car hire) or a tax number (CPF, NIF): the AI handles those on the call.
const OWNER_ID_WORDS: &[&str] = &[
    "national id", "id number", "id card", "citizen id", "resident id", "resident registration", "registration number", "national insurance",
    "passport", "passaport", "residence card", "residence permit", "arc", "birth date", "date of birth", "dob", "birth certificate", "ssn",
    "social security", "social insurance", "health card", "health insurance", "insurance card", "insurance member", "medical insurance", "medicare",
    "nhi", "nhs", "pps", "carte vitale", "sécurité sociale", "sv-nummer", "e-card", "versichertennummer", "krankenkassenkarte", "veka", "ahv",
    "ausweis", "ausweisnummer", "ausländerausweis", "geburtsdatum", "matricule", "cns", "rijksregisternummer", "registre national", "numéro national", "nric", "hkid",
    "bir number", "home return permit", "dni", "nie", "cédula", "rut", "curp", "cartão de cidadão", "utente", "niss", "segurança social",
    "carteirinha", "obra social", "prepaga", "my number", "身分證", "身份證", "身份证", "身分证", "居留證", "居留证", "護照", "护照", "通行证", "回鄉證",
    "生日", "出生", "健保卡", "醫療卡", "医保卡", "社保", "マイナンバー", "在留カード", "保険証番号", "生年月日", "パスポート", "身分証", "주민등록",
    "외국인등록", "여권", "생년월일",
];

/// A thing asked that is part of the owner's identity papers ([OWNER_ID_WORDS]).
pub fn names_owner_id(what: &str) -> bool {
    let low = what.to_lowercase();
    OWNER_ID_WORDS.iter().any(|w| crate::preflight::has_word(&low, w))
}

/// Such places there usually ask for the owner's identity papers (an ID
/// number, a passport, a date of birth, a health insurance card), which the
/// AI will not give. No region: never.
pub fn asks_owner_id(region: Option<&Region>, kind: &str) -> bool {
    place(region, kind).is_some_and(|p| p.sensitive_asked.iter().any(|s| s.handle == "owner_only" && names_owner_id(&s.what)))
}

/// Worth a word to the owner before dialing even when nothing is missing:
/// such places seldom do it by phone, or ask for the owner's identity papers.
pub fn worth_a_reminder(region: Option<&Region>, kind: &str) -> bool {
    hard_by_phone(region, kind) || asks_owner_id(region, kind)
}

/// What the owner should know before dialing this kind of call there, in
/// Chinese, on one line ("" when the data has none).
pub fn before_dialing(region: Option<&Region>, kind: &str) -> String {
    place(region, kind).map(|p| one_line(&p.before_dialing_zh)).unwrap_or_default()
}

/// For the pre-dial model: per kind of call, what it needs before dialing
/// and, with a region, how often it books by phone and its local note.
pub fn model_brief(region: Option<&Region>) -> String {
    let mut lines: Vec<String> = Vec::new();
    for kind in TYPES {
        let mut line = format!("{kind}: needs {}", must_have(region, kind).join(","));
        if for_whom_matters(region, kind) {
            line.push_str("; who it is for matters");
        }
        if let Some(p) = place(region, kind) {
            if !p.phone_booking.trim().is_empty() {
                line.push_str(&format!("; phone bookings: {}", one_line(&p.phone_booking)));
            }
            let before = clip(&one_line(&p.before_dialing_zh), BEFORE_MAX_CHARS);
            if !before.is_empty() {
                line.push_str(&format!("; local note: {before}"));
            }
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// The words owners use for the purpose of a call, in every region and
/// language of the data, lower case, one phrase each.
#[derive(Debug, Default)]
pub struct Lexicon {
    pub booking: Vec<String>,
    pub appointment: Vec<String>,
    pub order: Vec<String>,
    pub change: Vec<String>,
    pub inquiry: Vec<String>,
}

/// [Lexicon], read once.
pub fn lexicon() -> &'static Lexicon {
    static L: OnceLock<Lexicon> = OnceLock::new();
    L.get_or_init(|| {
        let mut l = Lexicon::default();
        for w in regions().iter().flat_map(|r| r.how_owners_write.values()) {
            for (to, from) in [
                (&mut l.booking, &w.booking_words),
                (&mut l.appointment, &w.appointment_words),
                (&mut l.order, &w.order_words),
                (&mut l.change, &w.change_words),
                (&mut l.inquiry, &w.inquiry_words),
            ] {
                for p in from.iter().flat_map(|s| phrases(s)) {
                    if !to.contains(&p) {
                        to.push(p);
                    }
                }
            }
        }
        l
    })
}

/// A written example without the note in brackets after it ("兩個小時（時長非人數）",
/// "4/F (floor)"; an area code in brackets stays: "(11) 91234-5678").
pub fn without_note(s: &str) -> &str {
    let cut = s
        .find('（')
        .into_iter()
        .chain(s.match_indices(" (").map(|(i, _)| i).filter(|i| !s[i + 2..].starts_with(|c: char| c.is_ascii_digit())))
        .min()
        .unwrap_or(s.len());
    s[..cut].trim()
}

/// A written example without its note ([without_note]), its alternatives
/// apart ("para levar / para fora"), lower case, with no question mark around it.
pub fn phrases(s: &str) -> Vec<String> {
    without_note(s)
        .split(" / ")
        .map(|p| p.to_lowercase().replace('\u{307}', "").trim_matches(|c: char| c.is_whitespace() || matches!(c, '?' | '¿' | '？')).to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// At most `max` characters, cut at a space when there is one late enough.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    match cut.rfind(' ') {
        Some(i) if i > cut.len() / 2 => cut[..i].trim_end_matches([',', ';', ' ']).to_string(),
        _ => cut,
    }
}

/// Words for the script: no bracketed asides (they hold local spellings), no
/// markers, one line, at most `max` characters, no closing stop.
fn plain(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut depth = 0u32;
    for ch in s.chars() {
        match ch {
            '(' | '（' => depth += 1,
            ')' | '）' => depth = depth.saturating_sub(1),
            '[' | ']' | '{' | '}' => {}
            c if depth == 0 => out.push(if c.is_control() { ' ' } else { c }),
            _ => {}
        }
    }
    let line = one_line(&out).replace(" ,", ",").replace(" ;", ";").replace(" .", ".");
    clip(line.trim_end_matches(['.', ';', ',', ' ']), max)
}

/// `lead` and as many of `items` as fit in the block, as one more line.
fn push_list(out: &mut String, lead: &str, items: &[String]) {
    push_within(out, lead, items, CALL_BLOCK_MAX);
}

/// [push_list] within `max` characters of the block.
fn push_within(out: &mut String, lead: &str, items: &[String], max: usize) {
    for n in (1..=items.len()).rev() {
        let line = format!("- {lead}: {}.", items[..n].join("; "));
        if out.chars().count() + 1 + line.chars().count() <= max {
            out.push('\n');
            out.push_str(&line);
            return;
        }
    }
}

/// The script's block for a call to region `r` of kind `kind` ([TYPES]; another
/// kind gets only the region's habits, `contact` none). In this order, as far
/// as it fits: what the other side usually asks; what not to give, what to
/// decline, what to check with the owner and what the AI may settle; which
/// language to speak; voice menus; what else they may ask about; how names,
/// numbers and times go there; manners; how bookings and confirmations go.
/// The language, the voice menus and the first things they may also ask
/// always fit: what they ask and how to handle it leave room for them. A
/// direction in English, at most [CALL_BLOCK_MAX] characters.
pub fn call_block(r: &Region, kind: &str) -> String {
    if kind == "contact" || r.region_name.trim().is_empty() {
        return String::new();
    }
    let place = r.types.get(kind);
    let mut out = match place {
        Some(_) => format!(
            "[LOCAL WAYS] The other side is in {}; this is a {} call. How such calls usually go there, as a direction for your own words (the fixed rules below still come first):",
            r.region_name,
            kind.replace('_', " ")
        ),
        None => format!(
            "[LOCAL WAYS] The other side is in {}. How calls to businesses usually go there, as a direction for your own words (the fixed rules below still come first):",
            r.region_name
        ),
    };
    let language = plain(&r.ai_language_tip, TIP_MAX_CHARS);
    let english = plain(&r.english_ok, ENGLISH_MAX_CHARS);
    let menu = plain(&r.voice_menu, TIP_MAX_CHARS);
    const LANGUAGE: &str = "Language there (the fixed language rule below still decides)";
    const MENU: &str = "Voice menus there; you cannot press keys, so say nothing to a menu and wait for a person";
    const EXTRA: &str = "They may also ask about these; if the matter says nothing on one, say there is no special request, or use ask_owner if it matters";
    let extra: Vec<String> = place
        .map(|p| {
            p.nice_to_have
                .iter()
                .filter(|c| c.as_str() != "question" && !p.must_have.contains(c))
                .map(|c| code_words(c).to_string())
                .filter(|w| !w.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let line = |lead: &str, tip: &str| if tip.is_empty() { 0 } else { lead.chars().count() + tip.chars().count() + 6 };
    let room = CALL_BLOCK_MAX - line(LANGUAGE, &language) - line(MENU, &menu) - line(EXTRA, &extra.iter().take(2).cloned().collect::<Vec<_>>().join("; "));
    if let Some(p) = place {
        let asked: Vec<String> = p.usually_asked.iter().map(|a| plain(a, ITEM_MAX_CHARS)).filter(|a| !a.is_empty()).take(ASKED_MAX).collect();
        push_within(&mut out, "They usually ask about", &asked, room);
        let handled = |h: &str| -> Vec<String> {
            p.sensitive_asked
                .iter()
                .chain(&p.may_come_up)
                .filter(|s| s.handle == h)
                .map(|s| plain(&s.what, ITEM_MAX_CHARS))
                .filter(|s| !s.is_empty())
                .take(HANDLED_MAX)
                .collect()
        };
        push_within(&mut out, "Do not give these; say the person you call for will provide them", &handled("owner_only"), room);
        push_within(&mut out, "Politely decline", &handled("decline"), room);
        push_within(&mut out, "Check through ask_owner before you agree to", &handled("ask_owner"), room);
        push_within(&mut out, "You may settle these yourself within the matter", &handled("ai_can_answer"), room);
    }
    if !language.is_empty() {
        push_list(&mut out, LANGUAGE, &[language]);
    }
    if !menu.is_empty() {
        push_list(&mut out, MENU, &[menu]);
    }
    push_list(&mut out, EXTRA, &extra);
    if !english.is_empty() {
        push_list(&mut out, "English there", &[english]);
    }
    for (lead, tip) in [
        ("Names there", &r.name_tip),
        ("Phone numbers", &r.phone_tip),
        ("Times", &r.time_tip),
        ("Numbers", &r.number_tip),
        ("Manner", &r.politeness),
        ("Bookings", &r.booking_channels),
        ("Confirmations", &r.callback_habits),
    ] {
        let tip = plain(tip, TIP_MAX_CHARS);
        if !tip.is_empty() {
            push_list(&mut out, lead, &[tip]);
        }
    }
    out
}

/// For the result of an errand to region `r`: how places there usually confirm
/// a booking, one line ("" when the data has none).
pub fn confirm_habits(r: &Region) -> String {
    plain(&r.callback_habits, TIP_MAX_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn data() -> Value {
        serde_json::from_str(DATA).expect("place_habits.json reads")
    }

    /// An example without the note in brackets after it ("兩個小時（時長非人數）").
    fn example(s: &str) -> String {
        without_note(s).to_string()
    }

    /// Every example of one field of how_owners_write: (region, language, example).
    fn written(field: &str) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for r in regions() {
            for (lang, w) in &r.how_owners_write {
                let list = match field {
                    "time" => &w.time,
                    "date" => &w.date,
                    "name" => &w.name,
                    "phone" => &w.phone,
                    "false_friends" => &w.false_friends,
                    _ => unreachable!("{field}"),
                };
                for s in list {
                    out.push((r.region.clone(), lang.clone(), example(s)));
                }
            }
        }
        out
    }

    /// The examples a rule does not read, leaving out the exceptions (they
    /// must still be examples, and still not be read).
    fn misread(field: &str, reads: impl Fn(&str) -> bool, exceptions: &[&str]) -> (Vec<String>, usize) {
        let all = written(field);
        let wrong: Vec<String> = all.iter().filter(|(_, _, s)| !reads(s) && !exceptions.contains(&s.as_str())).map(|(r, l, s)| format!("{r} {l}: {s}")).collect();
        for e in exceptions {
            assert!(all.iter().any(|(_, _, s)| s == e), "{field}: exception {e} is no example any more");
            assert!(!reads(e), "{field}: exception {e} is read now; take it off the list");
        }
        (wrong, all.len())
    }

    /// Words that hand the AI a fixed line to recite (as in stall_script).
    const SCRIPTED: [&str; 19] = [
        "必須說", "必须说", "一定要說", "一定要说", "照這句", "照这句", "照著說", "照着说", "照念", "逐字", "一字不改",
        "台詞", "台词", "下一句：", "must say", "say exactly", "word for word", "verbatim", "use this line",
    ];

    #[test]
    fn every_region_reads_with_all_kinds_and_known_codes() {
        let regs = regions();
        assert_eq!(
            regs.iter().map(|r| r.region.as_str()).collect::<Vec<_>>(),
            [
                "tw", "us", "hk-mo", "jp", "cn", "sg", "kr", "gb-ie", "ca", "au-nz", "es", "mx", "latam", "ar-uy", "fr", "be", "lu", "de", "at", "ch", "br",
                "pt"
            ]
        );
        let mut countries: Vec<&str> = Vec::new();
        for r in regs {
            assert!(!r.region_name.is_empty(), "{}", r.region);
            for field in [&r.ai_language_tip, &r.english_ok, &r.voice_menu, &r.avoid_calling, &r.closed_days, &r.callback_habits] {
                assert!(!field.trim().is_empty(), "{}", r.region);
            }
            assert!(!r.how_owners_write.is_empty(), "{}", r.region);
            for (lang, w) in &r.how_owners_write {
                // (Languages only read, like written Cantonese, may lack names and numbers.)
                for (field, list) in [("party", &w.party), ("time", &w.time), ("false_friends", &w.false_friends)] {
                    assert!(!list.is_empty(), "{} {lang} {field}", r.region);
                }
            }
            for c in &r.countries {
                assert!(c.len() == 2 && c.chars().all(|x| x.is_ascii_lowercase()), "{c}");
                assert!(!countries.contains(&c.as_str()), "{c} in two regions");
                countries.push(c);
            }
            for kind in TYPES {
                let p = r.types.get(kind).unwrap_or_else(|| panic!("{} {kind}", r.region));
                assert!(!p.must_have.is_empty(), "{} {kind}", r.region);
                // Every detail a kind needs is one the owner can be asked about.
                for c in &p.must_have {
                    assert!(crate::preflight::KEYS.contains(&c.as_str()), "{} {kind} {c}", r.region);
                }
                // And every one it may also ask about has a question in every language.
                for c in &p.nice_to_have {
                    for lang in crate::locale::ALL {
                        assert!(!crate::preflight::question_for(lang, c, kind).is_empty(), "{} {kind} {c} {lang:?}", r.region);
                    }
                }
                for s in p.may_come_up.iter().chain(&p.sensitive_asked) {
                    assert!(["ai_can_answer", "ask_owner", "owner_only", "decline"].contains(&s.handle.as_str()), "{}", s.handle);
                }
            }
            assert_eq!(r.types.len(), TYPES.len(), "{}", r.region);
        }
        for r in data()["regions"].as_array().unwrap() {
            for (kind, p) in r["types"].as_object().unwrap() {
                for field in ["must_have", "nice_to_have"] {
                    for c in p[field].as_array().unwrap() {
                        assert!(code(c.as_str().unwrap()).is_some(), "{kind} {field} {c}");
                    }
                }
                // Owners call local places from local numbers: no advice to dial from another country's.
                let before = p["before_dialing_zh"].as_str().unwrap();
                assert!(!before.contains("門號撥打") && !before.contains("門號聯絡"), "{before}");
            }
            // What the research had and the code does not use stays out.
            for field in ["caller_id_trust", "ai_call_rules", "notes_zh", "uncertain", "sources"] {
                assert!(r.get(field).is_none(), "{} {field}", r["region"]);
            }
        }
        // The new details sit where they are asked.
        assert!(nice_to_have(for_country("us"), "repair").contains(&"vehicle_plate"));
        assert!(nice_to_have(for_country("de"), "home_service").contains(&"floor_elevator"));
        assert!(nice_to_have(for_country("tw"), "takeout").contains(&"customization"));
        assert!(nice_to_have(for_country("jp"), "restaurant").contains(&"name_kana"));
        assert!(!nice_to_have(for_country("us"), "restaurant").contains(&"name_kana"));
        assert!(nice_to_have(None, "restaurant").is_empty());
    }

    #[test]
    fn every_way_owners_write_a_party_there_reads_as_a_party_and_look_alikes_do_not() {
        let mut seen = 0;
        for r in data()["regions"].as_array().unwrap() {
            for (lang, w) in r["how_owners_write"].as_object().unwrap() {
                for p in w["party"].as_array().unwrap() {
                    let p = example(p.as_str().unwrap());
                    assert!(crate::preflight::names_party(&p), "{} {lang}: {p}", r["region"]);
                    seen += 1;
                }
                for f in w["false_friends"].as_array().unwrap() {
                    let f = example(f.as_str().unwrap());
                    assert!(!crate::preflight::names_party(&f), "{} {lang}: {f}", r["region"]);
                }
            }
        }
        assert!(seen >= 50, "{seen}");
    }

    /// Times and dates the look cannot read, and why.
    const WHEN_EXCEPTIONS: [&str; 1] = [
        // Basque number words ("half past nine at night"): Basque is no language of the console.
        "gaueko bederatziak eta erdietan",
    ];
    /// Look-alikes that do read as a time, and why.
    const LOOK_ALIKE_TIMES: [&str; 1] = [
        // It is a time (12:15), listed for how to read its minutes, not as a non-time.
        "12點3",
    ];

    #[test]
    fn every_way_owners_write_a_time_or_a_date_there_is_read_and_look_alikes_are_not() {
        // A time example names a time of day; a date example names a day.
        let (wrong, n) = misread("time", crate::preflight::names_clock, &WHEN_EXCEPTIONS);
        let (wrong_dates, m) = misread("date", crate::preflight::names_date, &[]);
        assert!(wrong.is_empty() && wrong_dates.is_empty(), "times {wrong:#?}\ndates {wrong_dates:#?}");
        assert!(n + m >= 300, "{n} {m}");
        let reads = |f: &str| crate::preflight::names_clock(f) || crate::preflight::names_date(f);
        let read_wrongly: Vec<String> = written("false_friends")
            .into_iter()
            .filter(|(_, _, f)| reads(f) && !LOOK_ALIKE_TIMES.contains(&f.as_str()))
            .map(|(r, l, f)| format!("{r} {l}: {f}"))
            .collect();
        assert!(read_wrongly.is_empty(), "{read_wrongly:#?}");
        for f in LOOK_ALIKE_TIMES {
            assert!(written("false_friends").iter().any(|(_, _, x)| x == f), "{f}");
        }
    }

    /// Phone numbers the look cannot read, and why.
    const PHONE_EXCEPTIONS: [&str; 3] = [
        // "My Taiwan mobile": no number written; the AI cannot know it.
        "留我台灣那支手機",
        "留我台灣彼支手機仔",
        // Six digits: part of a number, too short to tell from a price or a code.
        "312 345",
    ];

    #[test]
    fn every_phone_number_owners_write_there_is_read() {
        let (wrong, n) = misread("phone", crate::preflight::names_phone, &PHONE_EXCEPTIONS);
        assert!(wrong.is_empty(), "{wrong:#?}");
        assert!(n >= 90, "{n}");
    }

    /// Names the look cannot read, and why.
    const NAME_EXCEPTIONS: [&str; 1] = [
        // One given name alone: any capitalized word could be one.
        "Ahmad",
    ];

    #[test]
    fn every_name_owners_write_there_is_read_as_the_name_to_book_under() {
        let (wrong, n) = misread("name", crate::preflight::names_name, &NAME_EXCEPTIONS);
        assert!(wrong.is_empty(), "{wrong:#?}");
        assert!(n >= 100, "{n}");
    }

    #[test]
    fn every_word_owners_use_for_a_business_call_is_read_as_one_in_every_language() {
        let mut wrong: Vec<String> = Vec::new();
        let mut n = 0;
        for r in regions() {
            for (lang, w) in &r.how_owners_write {
                for (what, list) in [
                    ("booking", &w.booking_words),
                    ("appointment", &w.appointment_words),
                    ("order", &w.order_words),
                    ("change", &w.change_words),
                    ("inquiry", &w.inquiry_words),
                ] {
                    for word in list.iter().flat_map(|s| phrases(s)) {
                        n += 1;
                        // A business call, or one the model is asked about: never a call to a friend that skips the check.
                        let l = crate::preflight::local(&word);
                        if l.place == "contact" && !l.unsure {
                            wrong.push(format!("{} {lang} {what}: {word}", r.region));
                        }
                    }
                }
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
        assert!(n >= 500, "{n}");
        // A room is a hotel's and a court an activity's, not a table.
        for (word, place) in [
            ("訂房", "hotel"),
            ("book a room", "hotel"),
            ("reservar habitación", "hotel"),
            ("réserver une chambre", "hotel"),
            ("Zimmer buchen", "hotel"),
            ("reservar quarto", "hotel"),
            ("宿泊予約", "hotel"),
            ("숙소 예약", "hotel"),
            ("tee time", "activity"),
            ("reservar cancha", "activity"),
            ("Court buchen", "activity"),
            ("訂位", "restaurant"),
            ("訂包廂", "restaurant"),
            ("réserver une table", "restaurant"),
        ] {
            assert_eq!(crate::preflight::place_type_of(word), place, "{word}");
        }
    }

    #[test]
    fn the_number_called_picks_the_region() {
        let id = |n: &str| for_number(n, "tw").map(|r| r.region.as_str());
        assert_eq!(id("+12125550123"), Some("us"));
        assert_eq!(id("0223456789"), Some("tw"));
        assert_eq!(id("+886912345678"), Some("tw"));
        assert_eq!(id("+85221234567"), Some("hk-mo"));
        assert_eq!(id("+85328123456"), Some("hk-mo"));
        assert_eq!(id("+81312345678"), Some("jp"));
        assert_eq!(for_number("09012345678", "jp").map(|r| r.region.as_str()), Some("jp"));
        // A number without its country code is the owner's country's.
        assert_eq!(for_number("2125550123", "us").map(|r| r.region.as_str()), Some("us"));
        assert_eq!(id("+4930123456"), Some("de"));
        assert_eq!(id("+33123456789"), Some("fr"));
        assert_eq!(id("+8613800138000"), Some("cn"));
        assert_eq!(id("+821012345678"), Some("kr"));
        assert_eq!(id("+5511912345678"), Some("br"));
        // Places with no data, and no number at all.
        assert_eq!(id("+390612345678"), None);
        assert_eq!(id(""), None);
    }

    #[test]
    fn a_kind_without_a_region_needs_what_most_regions_say() {
        assert_eq!(must_have(None, "restaurant"), ["time", "party", "name", "phone"]);
        assert_eq!(must_have(for_country("us"), "restaurant"), ["time", "party", "name", "phone"]);
        assert!(must_have(None, "nothing").is_empty());
        assert!(for_whom_matters(None, "clinic"));
        assert!(!for_whom_matters(None, "restaurant"));
        assert_eq!(clean_type(" Hotel "), "hotel");
        assert_eq!(clean_type("contact"), "contact");
        assert_eq!(clean_type("cooking"), "");
    }

    #[test]
    fn what_is_worth_a_word_before_dialing() {
        let tw = for_country("tw");
        // A clinic there asks for an ID number the AI will not give.
        assert!(asks_owner_id(tw, "clinic") && worth_a_reminder(tw, "clinic"));
        assert!(before_dialing(tw, "clinic").contains("身分證"));
        // Customer service seldom works by phone anywhere.
        assert!(hard_by_phone(for_country("de"), "customer_service"));
        // A hotel there seldom books by phone.
        assert!(hard_by_phone(for_country("us"), "hotel") && worth_a_reminder(for_country("us"), "hotel"));
        // A card for a deposit, a pet's insurance policy, a driving licence or a tax
        // number is no identity paper: no reminder on its own.
        for (r, kind) in [("fr", "restaurant"), ("es", "restaurant"), ("br", "restaurant"), ("jp", "pet"), ("kr", "takeout"), ("de", "transport")] {
            assert!(!worth_a_reminder(for_country(r), kind), "{r} {kind}");
        }
        for what in [
            "National ID number, ARC/passport number, or birth date (身分證字號、居留證號、生日)",
            "NHI IC card number (健保卡號)",
            "Asks Resident Registration Number (주민등록번호) or full birth date (생년월일)",
            "Asks DNI/NIE number over phone before viewing",
            "Patient date of birth, insurance member/group ID, or SSN",
            "Arztpraxis asks Geburtsdatum (birth date) to verify identity",
            "Health insurance card number or My Number over phone (保険証番号・マイナンバー)",
        ] {
            assert!(names_owner_id(what), "{what}");
        }
        for what in [
            "Requests credit card digits over phone",
            "Credit card number, expiration date, CVV, and billing zip code",
            "Requests card details over phone for deposit",
            "Apartment auto-lock PIN code for delivery entry (オートロック暗証番号)",
            "One-time SMS or email verification code (2FA passcode)",
            "Phone screen unlock passcode or Apple/Google password (螢幕解鎖密碼、帳號密碼)",
            "Asks CPF number for reservation list",
            "Pet insurance policy number over phone (ペット保険証券番号)",
            "Car rental asks Führerschein or credit card number over phone",
            "Asks NIF for fatura over phone",
        ] {
            assert!(!names_owner_id(what), "{what}");
        }
        // No region, no reminder.
        assert!(!worth_a_reminder(None, "clinic") && before_dialing(None, "clinic").is_empty());
        assert!(!confirm_habits(for_country("us").unwrap()).is_empty());
    }

    #[test]
    fn the_model_brief_names_every_kind_and_the_local_notes() {
        let tw = model_brief(for_country("tw"));
        assert_eq!(tw.lines().count(), TYPES.len());
        assert!(tw.contains("restaurant: needs time,party,name,phone; phone bookings: often; local note: "), "{tw}");
        let none = model_brief(None);
        assert!(none.contains("restaurant: needs time,party,name,phone\n") && !none.contains("local note"), "{none}");
    }

    #[test]
    fn the_script_block_fits_gives_directions_and_skips_personal_calls() {
        for r in regions() {
            for kind in TYPES.iter().copied().chain(["", "unknown"]) {
                let b = call_block(r, kind);
                assert!(b.starts_with("[LOCAL WAYS] The other side is in "), "{b}");
                assert!(b.chars().count() <= CALL_BLOCK_MAX, "{} {kind}: {}", r.region, b.chars().count());
                let low = b.to_lowercase();
                for bad in SCRIPTED {
                    assert!(!low.contains(bad), "{bad}: {b}");
                }
                assert!(!b.contains('（') && !b[1..].contains('['), "{b}");
                // The most important parts come first and always fit.
                if TYPES.contains(&kind) {
                assert!(b.contains("- They usually ask about: "), "{} {kind}: {b}", r.region);
                    assert!(b.contains("- Language there"), "{} {kind}: {b}", r.region);
                    assert!(b.contains("you cannot press keys"), "{} {kind}: {b}", r.region);
                }
            }
            assert_eq!(call_block(r, "contact"), "");
        }
        let us = call_block(for_country("us").unwrap(), "restaurant");
        assert!(us.contains("the United States; this is a restaurant call"), "{us}");
        assert!(us.contains("- Do not give these"), "{us}");
        assert!(us.contains("- They may also ask about these") && us.contains("allergies"), "{us}");
        assert!(us.find("- Language there").unwrap() < us.find("- They may also ask").unwrap(), "{us}");
        assert!(us.len() > 900, "{us}");
        let jp = call_block(for_country("jp").unwrap(), "clinic");
        assert!(jp.contains("how the name reads in kana"), "{jp}");
    }
}
