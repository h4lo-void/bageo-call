// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The quick look before an AI errand dials: what kind of call it is and which
//! details the owner's words do not give yet (at most three questions), the
//! order in which the owner would give way (a ladder of times), and the short
//! block of all that the call's script carries.
//!
//! The first look is local and takes microseconds. Only a request the local
//! look cannot read (another script, long text full of numbers) asks a small
//! model, within [MODEL_TIMEOUT]; an answer that does not come in time (or an
//! error) lets the call go ahead without questions.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use reqwest::Client;
use serde_json::{json, Value};

use crate::locale::Lang;

pub const GEMINI_BASE: &str = "https://generativelanguage.googleapis.com";
pub const MODEL: &str = "gemini-flash-lite-latest";
/// The most the model may take: the owner never waits longer to dial.
pub const MODEL_TIMEOUT: Duration = Duration::from_millis(900);
/// What the script block may be, in characters.
pub const BLOCK_MAX_CHARS: usize = 300;
/// The most questions asked.
pub const MISSING_MAX: usize = 3;
/// Every known key and the note: the owner may answer all the questions.
const ANSWERS_MAX: usize = KEYS.len() + 1;
const ANSWER_MAX_CHARS: usize = 80;
const NOTE_MAX_CHARS: usize = 120;
const LADDER_MAX: usize = 4;
const TASK_FOR_MODEL_CHARS: usize = 600;

/// The details a question can be about (also the keys of an errand's answers).
pub const KEYS: [&str; 11] = [
    "time", "party", "phone", "name", "item", "contact", "backup", "spec", "pickup", "substitute", "orig_time",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Restaurant,
    Appointment,
    Order,
    Change,
    Contact,
}

const KINDS: [&str; 5] = ["restaurant", "appointment", "order", "change", "contact"];

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Restaurant => KINDS[0],
            Kind::Appointment => KINDS[1],
            Kind::Order => KINDS[2],
            Kind::Change => KINDS[3],
            Kind::Contact => KINDS[4],
        }
    }

    fn parse(s: &str) -> Option<Kind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "restaurant" => Some(Kind::Restaurant),
            "appointment" => Some(Kind::Appointment),
            "order" => Some(Kind::Order),
            "change" => Some(Kind::Change),
            "contact" => Some(Kind::Contact),
            _ => None,
        }
    }

    /// The details a call of this kind may ask about.
    fn keys(self) -> &'static [&'static str] {
        match self {
            Kind::Restaurant => &["time", "party", "phone", "name"],
            Kind::Appointment => &["time", "item", "contact", "backup"],
            Kind::Order => &["spec", "pickup", "substitute"],
            Kind::Change => &["orig_time", "contact"],
            Kind::Contact => &[],
        }
    }
}

// ---------------------------------------------------------------- reading the words

fn digit_of(c: char) -> Option<u32> {
    match c {
        '0'..='9' => Some(c as u32 - '0' as u32),
        '０'..='９' => Some(c as u32 - '０' as u32),
        _ => None,
    }
}

fn cjk_digit(c: char) -> Option<u32> {
    match c {
        '零' | '〇' => Some(0),
        '一' => Some(1),
        '二' | '兩' | '两' => Some(2),
        '三' => Some(3),
        '四' => Some(4),
        '五' => Some(5),
        '六' => Some(6),
        '七' => Some(7),
        '八' => Some(8),
        '九' => Some(9),
        _ => None,
    }
}

/// A number at `i`: digits (also full-width), or Chinese and Japanese numerals up to
/// 99. Its value and length in characters.
fn read_number(c: &[char], i: usize) -> Option<(u32, usize)> {
    let first = *c.get(i)?;
    if let Some(d) = digit_of(first) {
        let (mut v, mut j) = (d, i + 1);
        while let Some(x) = c.get(j).and_then(|ch| digit_of(*ch)) {
            v = v.checked_mul(10)?.checked_add(x)?;
            j += 1;
            if j - i > 6 {
                return None;
            }
        }
        return Some((v, j - i));
    }
    if first == '十' {
        let mut v = 10;
        let mut j = i + 1;
        if let Some(d) = c.get(j).and_then(|ch| cjk_digit(*ch)).filter(|d| *d > 0) {
            v += d;
            j += 1;
        }
        return Some((v, j - i));
    }
    let d = cjk_digit(first)?;
    let mut j = i + 1;
    if c.get(j) == Some(&'十') {
        let mut v = d * 10;
        j += 1;
        if let Some(d2) = c.get(j).and_then(|ch| cjk_digit(*ch)).filter(|d| *d > 0) {
            v += d2;
            j += 1;
        }
        return Some((v, j - i));
    }
    Some((d, 1))
}

fn contains_any(low: &str, words: &[&str]) -> bool {
    words.iter().any(|w| low.contains(w))
}

/// A request lower case for reading: Turkish İ comes out as "i" plus U+0307,
/// and the dot goes ("İptal" reads as "iptal").
fn fold(text: &str) -> String {
    text.to_lowercase().replace('\u{307}', "")
}

#[derive(Clone, Copy, PartialEq)]
enum Marker {
    /// A written "am" (or 凌晨, 午前, 오전): 12 is midnight.
    Am,
    /// A part of the morning named in words (上午, 早上, de la mañana): 12 is noon.
    Morning,
    Pm,
    Noon,
}

fn is_hour_char(c: char) -> bool {
    matches!(c, '點' | '点' | '時' | '时' | '시')
}

fn skip_spaces(c: &[char], mut i: usize) -> usize {
    while c.get(i) == Some(&' ') {
        i += 1;
    }
    i
}

fn marker_before(c: &[char], i: usize) -> Option<Marker> {
    let pre: String = c[i.saturating_sub(4)..i].iter().collect();
    if contains_any(&pre, &["中午", "正午", "점심"]) {
        Some(Marker::Noon)
    } else if contains_any(&pre, &["下午", "晚上", "晚間", "傍晚", "今晚", "明晚", "夜", "午後", "오후", "저녁", "밤"]) {
        Some(Marker::Pm)
    } else if contains_any(&pre, &["凌晨", "午前", "오전"]) {
        Some(Marker::Am)
    } else if contains_any(&pre, &["上午", "早上", "早晨", "清晨", "아침", "朝"]) {
        Some(Marker::Morning)
    } else {
        tr_marker_before(c, i).or_else(|| it_marker_before(c, i))
    }
}

/// Turkish names the part of the day before the time ("akşam saat 8'de",
/// "öğleden sonra saat 3'te"), farther back than the four characters above.
fn tr_marker_before(c: &[char], i: usize) -> Option<Marker> {
    let pre: String = c[i.saturating_sub(15)..i].iter().collect();
    if pre.contains("öğleden sonra") || pre.contains("akşam") {
        Some(Marker::Pm)
    } else if pre.contains("öğlen") || pre.contains("öğle ") {
        Some(Marker::Noon)
    } else if pre.contains("sabah") {
        Some(Marker::Morning)
    } else {
        None
    }
}

/// Italian names the part of the day before the time too ("stasera alle 8",
/// "domani mattina alle 9", "nel pomeriggio alle 3"). Whole Italian words only:
/// the French "sera" ("il sera là à 8h") is no evening.
fn it_marker_before(c: &[char], i: usize) -> Option<Marker> {
    let pre: String = c[i.saturating_sub(20)..i].iter().collect();
    if pre.contains("stasera") || pre.contains("questa sera") || pre.contains("pomeriggio") {
        Some(Marker::Pm)
    } else if pre.contains("mezzogiorno") {
        Some(Marker::Noon)
    } else if pre.contains("mattina") {
        Some(Marker::Morning)
    } else {
        None
    }
}

/// The part of the day written after a time ("20h de la noche", "8 Uhr abends",
/// "7 giờ tối"): the marker, skipping spaces.
fn marker_after(c: &[char], at: usize) -> Option<Marker> {
    const PM: &[&str] = &[
        "de la tarde", "de la noche", "du soir", "de l'après-midi", "de l'apres-midi", "nachmittags", "abends", "da tarde", "da noite", "della sera",
        "di sera", "di pomeriggio", "tối", "chiều", "malam", "sore",
    ];
    const MORNING: &[&str] = &["de la mañana", "du matin", "morgens", "vormittags", "da manhã", "da manha", "di mattina", "sáng", "pagi"];
    let j = skip_spaces(c, at);
    let word: String = c[j.min(c.len())..].iter().take(16).collect();
    if PM.iter().any(|p| word.starts_with(p)) {
        Some(Marker::Pm)
    } else if MORNING.iter().any(|p| word.starts_with(p)) {
        Some(Marker::Morning)
    } else {
        None
    }
}

/// Where the hour's unit ends when a number at `after` is followed by "h", "Uhr"
/// or "giờ" ("19h30", "20 Uhr 30"); not "2 hours".
fn hour_unit_end(c: &[char], after: usize) -> Option<usize> {
    let j = skip_spaces(c, after);
    for unit in ["uhr", "giờ", "h"] {
        let n = unit.chars().count();
        let word: String = c[j.min(c.len())..].iter().take(n).collect();
        if word == unit && !c.get(j + n).is_some_and(|x| x.is_alphabetic()) {
            return Some(j + n);
        }
    }
    None
}

/// A number written after the word for "at" in Italian, Spanish, Portuguese,
/// Indonesian and Turkish ("alle 20", "a las 8", "pukul 19.30", "saat 20.30").
fn after_at_word(c: &[char], i: usize) -> bool {
    let pre: Vec<char> = c[i.saturating_sub(8)..i].iter().copied().collect();
    ["alle ", "ore ", "pukul ", "jam ", "a las ", "a la ", "às ", "saat "].iter().any(|w| {
        let w: Vec<char> = w.chars().collect();
        pre.ends_with(&w) && !(pre.len() > w.len() && pre[pre.len() - w.len() - 1].is_alphabetic())
    })
}

/// The hour on a 24-hour clock. Without am/pm, a dinner is in the evening and
/// early hours of other calls are afternoons.
fn to_24(h: u32, marker: Option<Marker>, kind: Kind) -> u32 {
    if h >= 24 {
        return 0;
    }
    if h >= 13 || h == 0 {
        return h;
    }
    match marker {
        Some(Marker::Am) => {
            if h == 12 {
                0
            } else {
                h
            }
        }
        Some(Marker::Morning) => h,
        Some(Marker::Pm) => {
            if h < 12 {
                h + 12
            } else {
                h
            }
        }
        Some(Marker::Noon) => {
            if h <= 5 {
                h + 12
            } else {
                h
            }
        }
        None => match kind {
            Kind::Restaurant if h <= 10 => h + 12,
            Kind::Restaurant => h,
            _ if h <= 6 => h + 12,
            _ => h,
        },
    }
}

/// The times of day the words name, in order, each once (hour, minute).
fn clocks_of(c: &[char], kind: Kind) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    // The am/pm the owner wrote last: a later time without one is the same part of the day.
    let mut last_marker: Option<Marker> = None;
    let mut i = 0;
    while i < c.len() {
        let Some((h, len)) = read_number(c, i) else {
            i += 1;
            continue;
        };
        let after = i + len;
        let a2 = skip_spaces(c, after);
        let ascii = digit_of(c[i]).is_some();
        let unit_end = if ascii && len <= 2 && h <= 24 { hour_unit_end(c, after) } else { None };
        // (minute, where it ends, am/pm written after it)
        let mut found: Option<(u32, usize, Option<Marker>)> = None;
        if ascii && len <= 2 && h <= 24 && after < c.len() && matches!(c[after], ':' | '：') {
            let d1 = c.get(after + 1).and_then(|x| digit_of(*x));
            let d2 = c.get(after + 2).and_then(|x| digit_of(*x));
            if let (Some(d1), Some(d2)) = (d1, d2) {
                if c.get(after + 3).and_then(|x| digit_of(*x)).is_none() && d1 * 10 + d2 < 60 {
                    let m = d1 * 10 + d2;
                    let (suffix, end) = ampm_after(c, after + 3);
                    found = Some((m, end, suffix));
                }
            }
        } else if a2 < c.len() && is_hour_char(c[a2]) && h <= 24 && c.get(a2 + 1) != Some(&c[a2]) {
            let mut j = a2 + 1;
            let mut m = 0;
            let half = skip_spaces(c, j);
            if matches!(c.get(half), Some('半') | Some('반')) {
                m = 30;
                j = half + 1;
            } else if let Some((mv, l2)) = read_number(c, j) {
                let k = j + l2;
                if mv < 60 && matches!(c.get(k), Some('分') | Some('분')) {
                    m = mv;
                    j = k + 1;
                } else if mv < 60 && l2 == 2 && digit_of(c[j]).is_some() {
                    m = mv;
                    j = k;
                }
            }
            found = Some((m, j, None));
        } else if let Some(unit_end) = unit_end {
            // 19h30, 20h, 19 Uhr 30, 19 giờ 30
            let k = skip_spaces(c, unit_end);
            let d1 = c.get(k).and_then(|x| digit_of(*x));
            let d2 = c.get(k + 1).and_then(|x| digit_of(*x));
            let mut m = 0;
            let mut j = unit_end;
            if let (Some(d1), Some(d2)) = (d1, d2) {
                if c.get(k + 2).and_then(|x| digit_of(*x)).is_none() && d1 * 10 + d2 < 60 {
                    m = d1 * 10 + d2;
                    j = k + 2;
                }
            }
            found = Some((m, j, None));
        } else if ascii && len <= 2 && h <= 24 && after_at_word(c, i) {
            // alle 20, a las 8, pukul 19.30
            let d1 = c.get(after + 1).and_then(|x| digit_of(*x));
            let d2 = c.get(after + 2).and_then(|x| digit_of(*x));
            let mut m = 0;
            let mut j = after;
            if let (true, Some(d1), Some(d2)) = (matches!(c.get(after), Some('.')), d1, d2) {
                if c.get(after + 3).and_then(|x| digit_of(*x)).is_none() && d1 * 10 + d2 < 60 {
                    m = d1 * 10 + d2;
                    j = after + 3;
                }
            }
            let (suffix, end) = ampm_after(c, j);
            found = Some((m, end, suffix));
        } else if ascii && len <= 2 && (1..=12).contains(&h) {
            let (suffix, end) = ampm_after(c, after);
            if end > after {
                found = Some((0, end, suffix));
            }
        }
        match found {
            Some((m, end, suffix)) => {
                let explicit = suffix.or_else(|| marker_after(c, end)).or_else(|| marker_before(c, i));
                // A time after "11 am" with no marker of its own is the same part of the day,
                // but "12:30" after it is the noon hour, not midnight.
                let marker = explicit.or(last_marker.map(|m| if m == Marker::Am { Marker::Morning } else { m }));
                if explicit.is_some() {
                    last_marker = explicit;
                }
                let t = (to_24(h, marker, kind), m);
                if !out.contains(&t) {
                    out.push(t);
                }
                i = end.max(i + 1);
            }
            None => i = after,
        }
    }
    out
}

/// "pm", "a.m.", "o'clock" right after a number (spaces allowed): the marker
/// and where it ends (the same place when there is none).
fn ampm_after(c: &[char], at: usize) -> (Option<Marker>, usize) {
    let mut j = at;
    while c.get(j) == Some(&' ') {
        j += 1;
    }
    let word: String = c[j.min(c.len())..].iter().take_while(|x| x.is_alphabetic() || **x == '.' || **x == '\'').take(8).collect();
    let end = j + word.chars().count();
    match word.trim_end_matches('.') {
        "am" | "a.m" => (Some(Marker::Am), end),
        "pm" | "p.m" => (Some(Marker::Pm), end),
        "o'clock" => (None, end),
        _ => (None, at),
    }
}

/// The longest stretch of the words that reads as a phone number (8 to 15
/// digits, joined by spaces, dashes or brackets; not a written date).
fn has_phone(c: &[char]) -> bool {
    let mut i = 0;
    while i < c.len() {
        if !(digit_of(c[i]).is_some() || c[i] == '+') {
            i += 1;
            continue;
        }
        let mut j = i;
        let mut groups: Vec<usize> = Vec::new();
        let mut cur = 0;
        while j < c.len() && (digit_of(c[j]).is_some() || matches!(c[j], '-' | ' ' | '(' | ')' | '+' | '－')) {
            if digit_of(c[j]).is_some() {
                cur += 1;
            } else if cur > 0 {
                groups.push(cur);
                cur = 0;
            }
            j += 1;
        }
        if cur > 0 {
            groups.push(cur);
        }
        let digits: usize = groups.iter().sum();
        let date = matches!(groups.as_slice(), [4, 2, 2] | [2, 2, 4]);
        if (8..=15).contains(&digits) && !date {
            return true;
        }
        i = j.max(i + 1);
    }
    false
}

const PARTY_UNITS: [&str; 22] = [
    "位", "人", "個人", "个人", "名", "명", "people", "persons", "person", "guests", "guest", "ppl", "pax", "adults", "adult", "kids", "children",
    "personas", "personnes", "personen", "pessoas", "pers.",
];
const PARTY_PREFIXES: [&str; 7] = ["for ", "party of ", "table for ", "para ", "pour ", "für ", "x"];
const NUMBER_WORDS: [&str; 13] = [
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "dos", "tres", "cuatro",
];

fn has_party(low: &str, c: &[char]) -> bool {
    let mut i = 0;
    while i < c.len() {
        let Some((_, len)) = read_number(c, i) else {
            i += 1;
            continue;
        };
        let after = i + len;
        let clock = c.get(skip_spaces(c, after)).is_some_and(|x| is_hour_char(*x)) || matches!(c.get(after), Some(':') | Some('：'));
        if !clock {
            let rest: String = c[after..].iter().take(10).collect();
            let rest = rest.trim_start();
            if PARTY_UNITS.iter().any(|u| rest.starts_with(u)) {
                return true;
            }
            let before: String = c[i.saturating_sub(12)..i].iter().collect();
            if PARTY_PREFIXES.iter().take(6).any(|p| before.ends_with(p)) {
                return true;
            }
        }
        i = after;
    }
    for w in NUMBER_WORDS {
        for p in ["for ", "party of ", "table for ", "para ", "pour ", "für "] {
            if low.contains(&format!("{p}{w}")) {
                return true;
            }
        }
        if low.contains(&format!("{w} people")) || low.contains(&format!("{w} of us")) || low.contains(&format!("{w} guests")) {
            return true;
        }
    }
    contains_any(low, &["deux personnes", "zwei personen", "duas pessoas", "dois", "für zwei", "pour deux"])
}

fn has_date(low: &str, c: &[char]) -> bool {
    if contains_any(
        low,
        &[
            "明天", "後天", "后天", "今天", "今晚", "明晚", "週", "周", "星期", "禮拜", "礼拜", "tomorrow", "tonight", "today", "monday", "tuesday",
            "wednesday", "thursday", "friday", "saturday", "sunday", "明日", "今日", "今夜", "来週", "来周", "내일", "오늘", "mañana", "hoy",
            "demain", "aujourd", "morgen", "heute", "amanhã", "hoje",
        ],
    ) {
        return true;
    }
    let mut i = 0;
    while i < c.len() {
        match read_number(c, i) {
            Some((_, len)) => {
                if matches!(c.get(i + len), Some('月') | Some('日') | Some('號') | Some('号') | Some('월') | Some('일')) {
                    return true;
                }
                i += len;
            }
            None => i += 1,
        }
    }
    false
}

fn has_qty(low: &str, c: &[char]) -> bool {
    const UNITS: [&str; 27] = [
        "份", "個", "个", "杯", "碗", "盒", "包", "瓶", "隻", "只", "張", "件", "塊", "pcs", "piece", "x", "orders", "order", "portion", "cup",
        "box", "bottle", "serving", "つ", "개", "인분", "잔",
    ];
    let mut i = 0;
    while i < c.len() {
        match read_number(c, i) {
            Some((_, len)) => {
                let after = i + len;
                if !(c.get(skip_spaces(c, after)).is_some_and(|x| is_hour_char(*x)) || matches!(c.get(after), Some(':') | Some('：'))) {
                    let rest: String = c[after..].iter().take(10).collect();
                    let rest = rest.trim_start();
                    if UNITS.iter().any(|u| rest.starts_with(u)) {
                        return true;
                    }
                }
                if i > 0 && matches!(c[i - 1], 'x' | '×') {
                    return true;
                }
                i = after;
            }
            None => i += 1,
        }
    }
    const WORDS: [&str; 18] = [
        "one", "two", "three", "four", "five", "six", "dozen", "uno", "dos", "tres", "un", "une", "deux", "trois", "ein", "eine", "zwei", "drei",
    ];
    low.split(|ch: char| !ch.is_alphabetic()).any(|w| WORDS.contains(&w)) || contains_any(low, &["porciones", "unidades", "pièces", "stück", "porções"])
}

fn foreign_script(low: &str) -> bool {
    low.chars().any(|c| {
        // Cyrillic, Hebrew, Arabic, Devanagari, Thai, and the letters only Vietnamese writes
        // (đ, ơ, ư, and the stacked-accent block).
        matches!(
            c as u32,
            0x0400..=0x04FF | 0x0590..=0x05FF | 0x0600..=0x06FF | 0x0900..=0x097F | 0x0E00..=0x0E7F | 0x0110..=0x0111 | 0x01A0..=0x01A1 | 0x01AF..=0x01B0 | 0x1EA0..=0x1EF9
        )
    }) || contains_any(low, OTHER_LATIN_WORDS)
}

/// Words of booking, eating and ordering in Latin-script languages the word lists
/// above do not read (Italian, Indonesian, Dutch, Polish, Turkish, ...). Matching
/// one only sends the request to the model, which is cheap; missing one asks the
/// owner questions the request already answered.
const OTHER_LATIN_WORDS: &[&str] = &[
    "prenot", "ristorante", "tavolo", "appuntamento", "stasera", "domani", "ordinare", "pesan", "meja", "restoran", "reservasi", "janji temu", "dokter",
    "besok", "reserveer", "tafel", "afspraak", "stolik", "rezerwac", "wizyt", "rezervasyon", "randevu", "akşam", "yarın",
    "ayırt", "kişilik", "sipariş", "bugün", "doktor", "kuaför", "berber", "iptal", "ertele", "öğleden sonra",
    "pranzo", "la cena", "a cena", "parrucchier", "barbiere", "disdire", "annullare", "spostare", "dottore", "oggi", "dopodomani",
    "persone", "asporto",
];

const CHANGE_WORDS: &[&str] = &[
    "改期", "改時間", "改时间", "改到", "取消", "延期", "改約", "改约", "換時間", "换时间", "reschedule", "cancel", "postpone", "rebook",
    "move my appointment", "move the reservation", "move my reservation", "change my reservation", "change my appointment",
    "change the reservation", "change the appointment", "change the booking", "変更", "キャンセル", "取り消し", "延期", "취소", "변경", "바꿔",
    "cancelar", "cambiar mi", "reprogramar", "annuler", "modifier ma", "reporter", "déplacer", "stornieren", "verschieben", "absagen", "umbuchen",
    "remarcar",
];
const RESTAURANT_WORDS: &[&str] = &[
    "訂位", "订位", "訂桌", "订桌", "訂座", "订座", "book a table", "table for", "reserve a table", "a table at", "reservation", "reservar una mesa",
    "reservar mesa", "réservation", "réserver une table", "reservierung", "einen tisch", "reservar uma mesa", "mesa para",
];
/// A place to eat: a table booking only together with a word of booking ([BOOK_WORDS]).
const EATING_WORDS: &[&str] = &[
    "餐廳", "餐厅", "吃飯", "聚餐", "用餐", "restaurant", "dinner", "lunch", "レストラン", "食事", "ディナー", "ランチ", "식당", "레스토랑", "restaurante",
];
const BOOK_WORDS: &[&str] = &["訂", "订", "預約", "预约", "預定", "book", "reserv", "予約", "예약", "réserver"];
const ORDER_WORDS: &[&str] = &[
    "外帶", "外带", "外賣", "外卖", "訂購", "订购", "點餐", "点餐", "訂餐", "订餐", "購買", "购买", "預購", "预购", "下單", "下单", "訂一份", "點一份",
    "takeout", "take-out", "take away", "takeaway", "for pickup", "to go", "place an order", "order a", "order some", "テイクアウト", "注文", "持ち帰り",
    "出前", "주문", "포장", "배달", "pedido", "para llevar", "recoger", "commande", "à emporter", "bestellen", "bestellung", "zum mitnehmen", "abholen",
    "encomenda",
];
const APPOINTMENT_WORDS: &[&str] = &[
    "預約", "预约", "預定", "約診", "掛號", "挂号", "看診", "看医生", "看醫生", "剪髮", "剪头发", "剪頭髮", "剪发", "燙髮", "染髮", "護髮", "美髮", "理髮",
    "診所", "诊所", "牙醫", "牙医", "洗牙", "維修", "维修", "修理", "保養", "保养", "換機油", "按摩", "美甲", "appointment", "haircut", "hair cut",
    "hairdresser", "barber", "salon", "dentist", "doctor", "clinic", "checkup", "check-up", "massage", "repair", "予約", "美容院", "美容室", "散髪",
    "カット", "診察", "病院", "歯医者", "예약", "미용실", "병원", "진료", "치과", "수리", "cita", "peluquería", "médico", "dentista", "corte de pelo",
    "reparación", "rendez-vous", "coiffeur", "médecin", "dentiste", "réparation", "termin", "friseur", "arzt", "zahnarzt", "reparatur", "consulta",
    "cabeleireiro", "conserto",
];
/// What an appointment is for, named: the item or the symptom.
const SERVICE_WORDS: &[&str] = &[
    "剪", "燙", "烫", "染", "護髮", "洗牙", "洗頭", "洗头", "檢查", "检查", "維修", "维修", "修", "換", "换", "保養", "保养", "按摩", "美甲", "症", "痛", "咳",
    "燒", "感冒", "過敏", "过敏", "haircut", "hair cut", "cut", "color", "colour", "trim", "massage", "checkup", "check-up", "repair", "fix", "clean", "tire",
    "oil", "screen", "pain", "fever", "cough", "ache", "hurt", "symptom", "カット", "カラー", "診察", "歯", "修理", "커트", "염색", "진료", "치과", "수리",
    "corte", "revisión", "reparación", "coupe", "contrôle", "réparation", "haarschnitt", "reparatur", "kontrolle", "reparo", "conserto",
];
const NAME_WORDS: &[&str] = &[
    "姓", "名字", "稱呼", "称呼", "名義", "名义", "先生", "小姐", "太太", "女士", "under ", "name is", "my name", "name:", "under the name", " mr", " mrs",
    " ms ", "名前", "様", "で予約", "이름", "성함", "님", "으로 예약", "a nombre", "nombre", "au nom", "nom de", "sous le nom", "auf den namen", "unter dem namen",
    "name ist", "em nome", "nome",
];
const ASKS_TIME_WORDS: &[&str] = &[
    "幾點", "几点", "什麼時候", "什么时候", "哪天", "哪一天", "when ", "what time", "which day", "いつ", "何時", "몇 시", "언제", "a qué hora", "cuándo",
    "à quelle heure", "quand ", "wann", "um wie viel uhr", "que horas", "quando ",
];
const PICKUP_WORDS: &[&str] = &[
    "asap", "馬上", "马上", "現在", "现在", "盡快", "尽快", "immediately", "right away", "すぐ", "바로", "ahora", "maintenant", "sofort", "agora", "等一下", "待會",
    "待会",
];
const SUBSTITUTE_WORDS: &[&str] = &[
    "缺貨", "缺货", "沒有的話", "沒有就", "没有的话", "没有就", "替代", "換成", "换成", "換別", "或是", "或者", "if not", "if they don't", "if out", "sold out",
    "instead", "otherwise", "or else", "substitute", "なければ", "品切れ", "代わり", "없으면", "품절", "대신", "si no", "sin existencias", "en su lugar",
    "sinon", "à défaut", "ersatz", "stattdessen", "falls nicht", "se não", "em vez",
];
const BACKUP_WORDS: &[&str] = &[
    "不行就", "不行的話", "不行的话", "不然", "沒有的話", "没有的话", "備案", "备案", "或者", "或是", "也可以", "都可以", "也行", "if not", "otherwise", "or else",
    "alternatively", "backup", "if that's full", "if full", "fallback", "だめなら", "だめだったら", "ダメなら", "難しければ", "でも可", "でもいい", "안 되면",
    "안되면", "아니면", "si no", "en caso de", "sinon", "ou bien", "falls nicht", "andernfalls", "sonst", "ou então", "caso contrário", "se não",
];

fn kind_of(low: &str) -> Kind {
    if contains_any(low, CHANGE_WORDS) {
        Kind::Change
    } else if contains_any(low, RESTAURANT_WORDS) || (contains_any(low, EATING_WORDS) && contains_any(low, BOOK_WORDS)) {
        Kind::Restaurant
    } else if contains_any(low, ORDER_WORDS) {
        Kind::Order
    } else if contains_any(low, APPOINTMENT_WORDS) {
        Kind::Appointment
    } else {
        Kind::Contact
    }
}

/// The kind of call: by its words, and a request that names a time and a
/// number of people but no service is a table booking whatever the language.
fn classify(low: &str, c: &[char]) -> Kind {
    let kind = kind_of(low);
    if matches!(kind, Kind::Contact | Kind::Appointment)
        && !contains_any(low, SERVICE_WORDS)
        && has_party(low, c)
        && !clocks_of(c, Kind::Restaurant).is_empty()
    {
        return Kind::Restaurant;
    }
    kind
}

/// What the quick local look found.
#[derive(Debug, Clone, PartialEq)]
pub struct Local {
    pub kind: Kind,
    /// The questions, most important first (at most [MISSING_MAX]).
    pub missing: Vec<&'static str>,
    /// The words are in a form this look cannot read (another language): the model should.
    pub unsure: bool,
}

/// The local look: pure text work, microseconds. A simple call to someone the
/// owner knows, and a request that already says what the call needs, have
/// nothing missing.
pub fn local(task: &str) -> Local {
    let low = fold(task);
    let c: Vec<char> = low.chars().collect();
    let kind = classify(&low, &c);
    let clocks = clocks_of(&c, kind);
    let asks_time = contains_any(&low, ASKS_TIME_WORDS);
    let time_ok = !clocks.is_empty() || asks_time;
    let phone = has_phone(&c);
    let name = contains_any(&low, NAME_WORDS);
    let mut miss: Vec<&'static str> = Vec::new();
    match kind {
        Kind::Restaurant => {
            if !time_ok {
                miss.push("time");
            }
            if !has_party(&low, &c) {
                miss.push("party");
            }
            if !phone && !name {
                miss.push("phone");
                miss.push("name");
            }
        }
        Kind::Appointment => {
            if !time_ok {
                miss.push("time");
            }
            if !contains_any(&low, SERVICE_WORDS) {
                miss.push("item");
            }
            if !phone && !name {
                miss.push("contact");
            }
            if !miss.is_empty() && time_ok && !(clocks.len() >= 2 || contains_any(&low, BACKUP_WORDS)) {
                miss.push("backup");
            }
        }
        Kind::Order => {
            if !has_qty(&low, &c) {
                miss.push("spec");
            }
            if !time_ok && !contains_any(&low, PICKUP_WORDS) {
                miss.push("pickup");
            }
            if !miss.is_empty() && !contains_any(&low, SUBSTITUTE_WORDS) {
                miss.push("substitute");
            }
        }
        Kind::Change => {
            if !time_ok && !has_date(&low, &c) {
                miss.push("orig_time");
            }
            if !phone && !name {
                miss.push("contact");
            }
        }
        Kind::Contact => {}
    }
    miss.truncate(MISSING_MAX);
    let has_digit = c.iter().any(|ch| digit_of(*ch).is_some());
    let unsure = kind == Kind::Contact && (foreign_script(&low) || (has_digit && c.len() >= 24));
    Local { kind, missing: miss, unsure }
}

/// The times the owner would accept, best first ("19:30", "20:30"); empty
/// unless two or more are named.
pub fn ladder_of(task: &str, kind: Kind) -> Vec<String> {
    let c: Vec<char> = fold(task).chars().collect();
    let times = clocks_of(&c, kind);
    if times.len() < 2 {
        return Vec::new();
    }
    times.iter().take(LADDER_MAX).map(|(h, m)| format!("{h:02}:{m:02}")).collect()
}

// ---------------------------------------------------------------- the model

#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub kind: Kind,
    pub missing: Vec<&'static str>,
    pub ladder: Vec<String>,
    /// `local`, `model` or `timeout` (the model did not answer in time: go ahead).
    pub source: &'static str,
    pub ms: f64,
}

const SYSTEM_PROMPT: &str = "A person asks an AI assistant to make a phone call for them; the request may be in any language. Decide what kind of call it is and which details the request does not give yet.\n\
kind: restaurant (book a table), appointment (clinic, hair, repair or another booking), order (takeout or ordering goods), change (reschedule or cancel a booking), contact (talking to someone the person knows, or anything simple).\n\
Details (keys): time (day and time), party (how many people; restaurant), phone (contact phone; restaurant), name (booking name; restaurant), item (service, symptom or model; appointment), contact (name and phone to give; appointment or change), backup (backup time; appointment), spec (what and how many; order), pickup (pickup time; order), substitute (what if out of stock; order), orig_time (original booking time; change).\n\
List a key only if the request really does not say it in any wording or language, and does not ask the other side to tell it. List at most 3, the most important first. For contact list nothing. Answer with JSON only.";

pub fn request_body(task: &str, l: &Local) -> Value {
    let task: String = task.trim().chars().take(TASK_FOR_MODEL_CHARS).collect();
    let user = format!(
        "Request:\n{task}\n\nA fast local check read it as kind={} and missing={}.",
        l.kind.as_str(),
        l.missing.join(",")
    );
    json!({
        "systemInstruction": {"parts": [{"text": SYSTEM_PROMPT}]},
        "contents": [{"role": "user", "parts": [{"text": user}]}],
        "generationConfig": {
            "temperature": 0,
            "maxOutputTokens": 120,
            "responseMimeType": "application/json",
            "thinkingConfig": {"thinkingLevel": "minimal"},
            "responseSchema": {
                "type": "OBJECT",
                "properties": {
                    "kind": {"type": "STRING", "enum": KINDS},
                    "missing": {"type": "ARRAY", "items": {"type": "STRING", "enum": KEYS}}
                },
                "required": ["kind", "missing"]
            }
        }
    })
}

/// The kind and questions in a generateContent response; only keys that fit
/// the kind count.
pub fn parse_answer(body: &str) -> Option<(Kind, Vec<&'static str>)> {
    let v: Value = serde_json::from_str(body.trim()).ok()?;
    let text: String = v
        .pointer("/candidates/0/content/parts")?
        .as_array()?
        .iter()
        .filter(|p| !p.get("thought").and_then(Value::as_bool).unwrap_or(false))
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect();
    let (start, end) = (text.find('{')?, text.rfind('}')?);
    if end <= start {
        return None;
    }
    let answer: Value = serde_json::from_str(&text[start..=end]).ok()?;
    let kind = Kind::parse(answer.get("kind")?.as_str()?)?;
    let mut missing: Vec<&'static str> = Vec::new();
    for k in answer.get("missing").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        if let Some(key) = kind.keys().iter().find(|x| **x == k.trim()) {
            if !missing.contains(key) {
                missing.push(key);
            }
        }
    }
    missing.truncate(MISSING_MAX);
    Some((kind, missing))
}

async fn ask_model(http: &Client, base: &str, key: &str, task: &str, l: &Local) -> Option<(Kind, Vec<&'static str>)> {
    let url = format!("{}/v1beta/models/{MODEL}:generateContent", base.trim_end_matches('/'));
    let resp = http.post(&url).header("x-goog-api-key", key).json(&request_body(task, l)).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    parse_answer(&resp.text().await.ok()?)
}

fn since_ms(t0: Instant) -> f64 {
    (t0.elapsed().as_secs_f64() * 10_000.0).round() / 10.0
}

/// The look at an errand's words before it dials. A request the local look can
/// read is settled locally, with its questions (the model needs over a second,
/// more than the owner may wait). One in words the local look cannot read asks
/// the model, which has `limit` in all: later than that (or any error) the call
/// goes ahead without questions. No key: the local look stands.
pub async fn check(http: &Client, base: &str, key: &str, task: &str, limit: Duration) -> Verdict {
    let t0 = Instant::now();
    let l = local(task);
    let ladder = ladder_of(task, l.kind);
    if !l.unsure || key.trim().is_empty() {
        return Verdict { kind: l.kind, missing: l.missing, ladder, source: "local", ms: since_ms(t0) };
    }
    match tokio::time::timeout(limit, ask_model(http, base, key, task, &l)).await {
        Ok(Some((kind, missing))) => {
            let ladder = if kind == l.kind { ladder } else { ladder_of(task, kind) };
            Verdict { kind, missing, ladder, source: "model", ms: since_ms(t0) }
        }
        _ => Verdict { kind: l.kind, missing: Vec::new(), ladder, source: "timeout", ms: since_ms(t0) },
    }
}

// ---------------------------------------------------------------- the words of questions

fn lang_index(lang: Lang) -> usize {
    match lang {
        Lang::ZhTw => 0,
        Lang::ZhCn => 1,
        Lang::En => 2,
        Lang::Ja => 3,
        Lang::Ko => 4,
        Lang::Es => 5,
        Lang::Fr => 6,
        Lang::De => 7,
        Lang::Pt => 8,
    }
}

/// Per key, the question in zh-TW, zh-CN, en, ja, ko, es, fr, de, pt.
const QUESTIONS: [(&str, [&str; 9]); 11] = [
    ("time", ["哪一天、幾點？", "哪一天、几点？", "What day and time?", "何日の何時ですか？", "며칠 몇 시인가요?", "¿Qué día y a qué hora?", "Quel jour et à quelle heure ?", "Welcher Tag, welche Uhrzeit?", "Que dia e a que horas?"]),
    ("party", ["幾位用餐？（幾大幾小）", "几位用餐？（几大几小）", "How many people (adults and kids)?", "何名様ですか？（大人と子ども）", "몇 명인가요? (어른, 아이)", "¿Cuántas personas (adultos y niños)?", "Combien de personnes (adultes et enfants) ?", "Wie viele Personen (Erwachsene, Kinder)?", "Quantas pessoas (adultos e crianças)?"]),
    ("phone", ["留哪支聯絡電話？", "留哪个联系电话？", "Which phone number should they note?", "連絡先の電話番号は？", "연락처 전화번호는?", "¿Qué teléfono de contacto dejo?", "Quel numéro de téléphone laisser ?", "Welche Telefonnummer soll ich angeben?", "Qual telefone de contato deixo?"]),
    ("name", ["訂位用什麼稱呼？有座位要求嗎？", "订位用什么称呼？有座位要求吗？", "Name for the booking? Any seating limits?", "予約のお名前は？席の希望は？", "예약 이름은? 좌석 요청이 있나요?", "¿A nombre de quién? ¿Alguna preferencia de mesa?", "Nom pour la réservation ? Une préférence de place ?", "Name für die Reservierung? Sitzwünsche?", "Em nome de quem? Alguma preferência de mesa?"]),
    ("item", ["要預約什麼項目、症狀或型號？", "要预约什么项目、症状或型号？", "What is it for (service, symptom or model)?", "何の予約ですか？（内容・症状・型番）", "어떤 내용인가요? (항목, 증상, 모델)", "¿Para qué es (servicio, síntoma o modelo)?", "C'est pour quoi (service, symptôme ou modèle) ?", "Worum geht es (Leistung, Beschwerde oder Modell)?", "É para quê (serviço, sintoma ou modelo)?"]),
    ("contact", ["要報什麼稱呼和電話？", "要报什么称呼和电话？", "Which name and phone number may I give?", "伝えてよいお名前と電話番号は？", "알려도 되는 이름과 전화번호는?", "¿Qué nombre y teléfono puedo dar?", "Quel nom et quel numéro puis-je donner ?", "Welchen Namen und welche Nummer darf ich nennen?", "Qual nome e telefone posso informar?"]),
    ("backup", ["備選時段有哪些？", "备选时段有哪些？", "Any backup times that work?", "第二希望の日時は？", "대체 가능한 시간은?", "¿Algún horario alternativo?", "Un autre créneau possible ?", "Gibt es Ausweichtermine?", "Algum horário alternativo?"]),
    ("spec", ["要訂什麼、幾份？", "要订什么、几份？", "What and how many?", "何をいくつ注文しますか？", "무엇을 몇 개 주문하나요?", "¿Qué y cuántos?", "Quoi et combien ?", "Was und wie viele?", "O quê e quantos?"]),
    ("pickup", ["幾點取貨？", "几点取货？", "What time to pick up?", "受け取りは何時？", "몇 시에 가져가나요?", "¿A qué hora se recoge?", "À quelle heure passer ?", "Um wie viel Uhr abholen?", "A que horas retirar?"]),
    ("substitute", ["缺貨時可以換什麼？", "缺货时可以换什么？", "What if it is out of stock?", "品切れなら代わりは？", "품절이면 대체는?", "¿Y si no hay existencias?", "Et en cas de rupture ?", "Was tun, wenn es ausverkauft ist?", "E se estiver esgotado?"]),
    ("orig_time", ["原本預約是哪天幾點？", "原来预约是哪天几点？", "When was the original booking?", "元の予約は何日の何時？", "원래 예약은 언제인가요?", "¿Cuándo era la reserva original?", "Quand était la réservation initiale ?", "Wann war die ursprüngliche Buchung?", "Quando era a reserva original?"]),
];

/// The question for a key in a language ("" for an unknown key).
pub fn question(lang: Lang, key: &str) -> &'static str {
    QUESTIONS.iter().find(|(k, _)| *k == key).map(|(_, q)| q[lang_index(lang)]).unwrap_or("")
}

const CARD_HEAD: [&str; 9] = [
    "撥出前還缺：",
    "拨出前还缺：",
    "Still missing before the call:",
    "発信前に足りない情報：",
    "전화 전에 부족한 정보:",
    "Falta antes de llamar:",
    "Il manque avant l'appel :",
    "Vor dem Anruf fehlt noch:",
    "Falta antes de ligar:",
];
const CARD_TAIL: [&str; 9] = [
    "回覆這則訊息補上答案，就會帶著撥出。",
    "回复这条消息补上答案，就会带着拨出。",
    "Reply to this message with the answers and the AI calls with them.",
    "このメッセージに返信で答えると、その内容で発信します。",
    "이 메시지에 답을 회신하면 그 내용으로 전화합니다.",
    "Responde a este mensaje con los datos y la IA llamará con ellos.",
    "Répondez à ce message avec les infos et l'IA appellera avec elles.",
    "Antworte auf diese Nachricht mit den Angaben, dann ruft die KI damit an.",
    "Responda a esta mensagem com os dados e a IA liga com eles.",
];

/// The questions as lines for a confirmation card in a chat.
pub fn card_text(lang: Lang, keys: &[&str]) -> String {
    let i = lang_index(lang);
    let mut out = String::from(CARD_HEAD[i]);
    for k in keys.iter().take(MISSING_MAX) {
        out.push_str("\n- ");
        out.push_str(question(lang, k));
    }
    out.push('\n');
    out.push_str(CARD_TAIL[i]);
    out
}

// ---------------------------------------------------------------- the script's block

/// An example of the polite opener a business call starts with after the
/// greeting, in the call's language: the AI words its own (no casual phone
/// greeting) and then says why it calls.
pub fn opening(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => "您好，不好意思打擾一下",
        Lang::ZhCn => "您好，打扰一下",
        Lang::En => "Hi, sorry to bother you",
        Lang::Ja => "お忙しいところ恐れ入りますが",
        Lang::Ko => "바쁘신데 죄송합니다만",
        Lang::Es => "Hola, ¿cómo está? Disculpe la molestia",
        Lang::Fr => "Excusez-moi, je ne vous dérange pas ?",
        Lang::De => "Guten Tag, ich habe eine kurze Frage",
        Lang::Pt => "Oi, tudo bem? Desculpe incomodar",
    }
}

/// The answers a request carries, kept: known keys only, one clean line each.
pub fn sanitize_answers(v: Option<&Value>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(map) = v.and_then(Value::as_object) else {
        return out;
    };
    for key in KEYS.iter().copied().chain(["note"]) {
        let raw = match map.get(key) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => continue,
        };
        let max = if key == "note" { NOTE_MAX_CHARS } else { ANSWER_MAX_CHARS };
        let value = crate::errands::prompt_safe(&raw, max);
        if !value.is_empty() && out.len() < ANSWERS_MAX {
            out.insert(key.to_string(), value);
        }
    }
    out
}

/// The answers in the order of their weight, each with the words it goes by.
const DETAIL_LABELS: [(&str, &str); 12] = [
    ("phone", "phone"),
    ("name", "booking name"),
    ("contact", "name and phone"),
    ("party", "people"),
    ("item", "for"),
    ("spec", "order"),
    ("pickup", "pickup"),
    ("substitute", "if out"),
    ("backup", "backup"),
    ("time", "when"),
    ("orig_time", "original time"),
    ("note", "note"),
];

fn assemble(ladder: &[String], details: &[String], lang: Lang) -> String {
    let mut lines: Vec<String> = Vec::new();
    if ladder.len() >= 2 {
        lines.push(format!("[NEGOTIATION LADDER] {} (best first; only a time past the last needs ask_owner; none free: end politely)", ladder.join(" > ")));
    }
    if !details.is_empty() {
        lines.push(format!("[AUTHORIZED DETAILS] you may say: {}", details.join("; ")));
    }
    let local = if lang == Lang::Ja { " (no もしもし)" } else { "" };
    lines.push(format!("[BUSINESS OPENING] greet, {} or similar, then say for whom and why you call{local}", opening(lang)));
    lines.join("
")
}

/// The block the script carries (at most [BLOCK_MAX_CHARS] characters): the
/// order the owner gives way in, what the AI may say about the owner, and the
/// opening. answers: already clean ([sanitize_answers]).
pub fn prompt_block(task: &str, answers: &[(String, String)], lang: Lang) -> String {
    let own = |key: &str| answers.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    let source = match own("backup") {
        Some(b) => format!("{task} {b}"),
        None => task.to_string(),
    };
    let low = source.to_lowercase();
    let kind = classify(&low, &low.chars().collect::<Vec<char>>());
    let ladder = ladder_of(&source, kind);
    let details_with = |limit: usize| -> Vec<String> {
        DETAIL_LABELS
            .iter()
            .filter_map(|(key, label)| own(key).map(|v| format!("{label} {}", v.chars().take(limit).collect::<String>())))
            .collect()
    };
    for limit in [ANSWER_MAX_CHARS, 40, 24, 12] {
        let b = assemble(&ladder, &details_with(limit), lang);
        if b.chars().count() <= BLOCK_MAX_CHARS {
            return b;
        }
    }
    let mut details = details_with(12);
    while !details.is_empty() {
        details.pop();
        let b = assemble(&ladder, &details, lang);
        if b.chars().count() <= BLOCK_MAX_CHARS {
            return b;
        }
    }
    assemble(&ladder, &[], lang).chars().take(BLOCK_MAX_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const ALL: [Lang; 9] = [Lang::ZhTw, Lang::ZhCn, Lang::En, Lang::Ja, Lang::Ko, Lang::Es, Lang::Fr, Lang::De, Lang::Pt];
    /// A request in a script the local look cannot read, with two times.
    const RU: &str = "Забронируйте столик на вечер, в 19:30 или в 20:30";

    fn gemini_json(text: &str) -> String {
        json!({"candidates": [{"content": {"parts": [{"text": text}]}}]}).to_string()
    }

    /// A stand-in for the model's address: waits, then answers with status and body.
    async fn stand_in(status: u16, delay_ms: u64, body: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        let n = sock.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&buf[..p]).to_lowercase();
                            let want = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
                            if buf.len() >= p + 4 + want {
                                break;
                            }
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    let resp = format!("HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        base
    }

    #[test]
    fn a_request_that_says_what_the_call_needs_has_nothing_missing() {
        let clear = [
            ("訂今晚 7 點 4 位，姓王", Kind::Restaurant),
            ("Book a table for 4 tomorrow at 7 pm under Alex", Kind::Restaurant),
            ("Please book a table for two at 7:30 pm under Alex", Kind::Restaurant),
            ("明日の夜7時に4名、王で予約", Kind::Restaurant),
            ("내일 저녁 7시 4명 김 이름으로 예약", Kind::Restaurant),
            ("訂位 19:30 4人 電話 0912345678", Kind::Restaurant),
            ("預約剪頭髮 明天下午 3 點，王小姐 0912345678", Kind::Appointment),
            ("點一份雞排加辣，問幾點可以拿", Kind::Order),
            ("取消明天晚上 7 點的訂位，姓王", Kind::Change),
            ("幫我打給媽媽說晚點回家", Kind::Contact),
            ("問他這週六或週日哪天有空一起吃飯", Kind::Contact),
            ("Ask Sam if he is coming to dinner", Kind::Contact),
        ];
        for (task, kind) in clear {
            let l = local(task);
            assert_eq!((l.kind, l.missing.clone(), l.unsure), (kind, vec![], false), "{task}");
        }
    }

    #[test]
    fn a_request_with_gaps_gets_at_most_the_three_key_questions() {
        let cases: [(&str, Kind, Vec<&str>); 6] = [
            ("訂餐廳 7 點半，不行就 8 點半", Kind::Restaurant, vec!["party", "phone", "name"]),
            ("預約剪頭髮", Kind::Appointment, vec!["time", "contact"]),
            ("預約剪頭髮 明天 3 點", Kind::Appointment, vec!["contact", "backup"]),
            ("幫我外帶訂購雞排", Kind::Order, vec!["spec", "pickup", "substitute"]),
            ("取消我的訂位", Kind::Change, vec!["orig_time", "contact"]),
            ("レストランを予約したい", Kind::Restaurant, vec!["time", "party", "phone"]),
        ];
        for (task, kind, missing) in cases {
            let l = local(task);
            assert_eq!((l.kind, l.missing), (kind, missing), "{task}");
        }
        assert!(local("Reserve a table").missing.len() <= MISSING_MAX);
    }

    #[test]
    fn other_scripts_and_unreadable_requests_are_left_to_the_model() {
        assert!(local("Забронируйте столик на 4 человека").unsure);
        assert!(local("Prenota un tavolo alle 20 per due persone, grazie").unsure);
        assert!(!local("幫我打給媽媽說晚點回家").unsure);
    }

    #[test]
    fn the_ladder_is_the_times_the_owner_would_give_way_to_in_order() {
        let ladder = |t: &str| ladder_of(t, classify(&t.to_lowercase(), &t.to_lowercase().chars().collect::<Vec<_>>()));
        assert_eq!(ladder("訂餐廳 7 點半，不行就 8 點半"), ["19:30", "20:30"]);
        assert_eq!(ladder("訂今晚 7:30 的位子，不行就 8:30"), ["19:30", "20:30"]);
        assert_eq!(ladder("預約剪頭髮 3 點，不行 4 點"), ["15:00", "16:00"]);
        assert_eq!(ladder("Book at 7 pm, otherwise 8:30 pm"), ["19:00", "20:30"]);
        assert_eq!(ladder("晚上 7 點，或者 9 點，最晚十點"), ["19:00", "21:00", "22:00"]);
        assert_eq!(ladder("明日の夜7時、だめなら8時半"), ["19:00", "20:30"]);
        assert_eq!(ladder("내일 저녁 7시, 안 되면 8시 반"), ["19:00", "20:30"]);
        assert!(ladder("訂今晚 7 點 4 位").is_empty());
        assert!(ladder("2026-11-17 0912345678").is_empty());
    }

    #[test]
    fn the_local_look_takes_microseconds_not_milliseconds() {
        let task = "訂餐廳 7 點半，不行就 8 點半，".repeat(60);
        let t0 = Instant::now();
        for _ in 0..20 {
            let _ = local(&task);
        }
        assert!(t0.elapsed() < Duration::from_millis(400), "{:?}", t0.elapsed());
    }

    #[tokio::test]
    async fn a_clear_request_never_asks_the_model() {
        let v = check(&Client::new(), "http://127.0.0.1:1", "key", "訂今晚 7 點 4 位，姓王", MODEL_TIMEOUT).await;
        assert_eq!((v.source, v.missing.is_empty()), ("local", true));
        assert!(v.ms < 50.0, "{}", v.ms);
    }

    #[tokio::test]
    async fn a_request_the_local_look_cannot_read_asks_the_model_for_what_is_missing() {
        let base = stand_in(200, 20, gemini_json(r#"{"kind":"restaurant","missing":["party","backup","party","phone"]}"#)).await;
        let v = check(&Client::new(), &base, "key", RU, MODEL_TIMEOUT).await;
        assert_eq!(v.source, "model");
        assert_eq!(v.kind, Kind::Restaurant);
        // "backup" is no question of a table booking; a key is asked once.
        assert_eq!(v.missing, ["party", "phone"]);
        assert_eq!(v.ladder, ["19:30", "20:30"]);
    }

    #[tokio::test]
    async fn the_model_in_another_language_can_say_nothing_is_missing() {
        let base = stand_in(200, 10, gemini_json(r#"{"kind":"restaurant","missing":[]}"#)).await;
        let v = check(&Client::new(), &base, "key", "Prenota un tavolo alle 20 per due persone, grazie", MODEL_TIMEOUT).await;
        assert_eq!((v.source, v.kind, v.missing.is_empty()), ("model", Kind::Restaurant, true));
    }

    #[tokio::test]
    async fn a_slow_model_lets_the_call_go_ahead_within_the_limit() {
        let base = stand_in(200, 700, gemini_json(r#"{"kind":"restaurant","missing":["party"]}"#)).await;
        let t0 = Instant::now();
        let v = check(&Client::new(), &base, "key", RU, Duration::from_millis(120)).await;
        assert_eq!((v.source, v.missing.is_empty()), ("timeout", true));
        assert!(t0.elapsed() < Duration::from_millis(500), "{:?}", t0.elapsed());
        assert!(v.ms >= 100.0 && v.ms < 500.0, "{}", v.ms);
    }

    #[tokio::test]
    async fn a_model_error_or_nonsense_also_lets_the_call_go_ahead() {
        for (status, body) in [(500, "{}".to_string()), (200, "not json".to_string()), (200, gemini_json("no object here"))] {
            let base = stand_in(status, 5, body).await;
            let v = check(&Client::new(), &base, "key", RU, MODEL_TIMEOUT).await;
            assert_eq!((v.source, v.missing.is_empty()), ("timeout", true), "{status}");
        }
        let gone = check(&Client::new(), "http://127.0.0.1:1", "key", RU, MODEL_TIMEOUT).await;
        assert_eq!((gone.source, gone.missing.is_empty()), ("timeout", true));
    }

    #[tokio::test]
    async fn a_request_the_local_look_reads_keeps_its_questions_and_never_waits_for_the_model() {
        for key in ["key", " "] {
            let v = check(&Client::new(), "http://127.0.0.1:1", key, "訂餐廳 7 點半，不行就 8 點半", MODEL_TIMEOUT).await;
            assert_eq!((v.source, v.kind, v.missing, v.ladder), ("local", Kind::Restaurant, vec!["party", "phone", "name"], vec!["19:30".to_string(), "20:30".to_string()]));
            assert!(v.ms < 50.0, "{}", v.ms);
        }
        // Unreadable words and no key: the local look stands.
        let v = check(&Client::new(), "http://127.0.0.1:1", " ", RU, MODEL_TIMEOUT).await;
        assert_eq!(v.source, "local");
    }

    #[test]
    fn the_model_request_is_small_and_answers_are_checked_against_the_kind() {
        let body = request_body("訂餐廳", &local("訂餐廳"));
        assert_eq!(body["generationConfig"]["thinkingConfig"]["thinkingLevel"], "minimal");
        assert_eq!(body["generationConfig"]["responseMimeType"], "application/json");
        assert_eq!(parse_answer(&gemini_json("{\"kind\":\"order\",\"missing\":[\"spec\",\"time\",\"pickup\"]}")), Some((Kind::Order, vec!["spec", "pickup"])));
        assert_eq!(parse_answer(&gemini_json("{\"kind\":\"contact\",\"missing\":[\"time\"]}")), Some((Kind::Contact, vec![])));
        assert_eq!(parse_answer(&gemini_json("{\"kind\":\"cooking\",\"missing\":[]}")), None);
    }

    #[test]
    fn every_question_and_card_line_exists_in_all_nine_languages() {
        for lang in ALL {
            for key in KEYS {
                assert!(!question(lang, key).is_empty(), "{lang:?} {key}");
            }
            let card = card_text(lang, &["party", "phone", "name", "time"]);
            assert_eq!(card.lines().count(), 5, "{lang:?}: head, three questions, tail");
        }
        assert_eq!(question(Lang::En, "nothing"), "");
    }

    #[test]
    fn a_business_call_opens_politely_in_the_language_of_the_call() {
        for lang in ALL {
            assert!(!opening(lang).is_empty());
        }
        assert!(!opening(Lang::Ja).contains("もしもし"));
        assert!(opening(Lang::Ja).contains("お忙しいところ恐れ入りますが"));
        assert!(opening(Lang::Fr).contains("Excusez-moi, je ne vous dérange pas ?"));
        let ja = prompt_block("予約したい", &[], Lang::Ja);
        assert!(ja.contains("お忙しいところ恐れ入りますが") && ja.contains("(no もしもし)"), "{ja}");
        // An example to word its own opener on, then the reason for the call.
        // An example to word its own opener on, then whose call it is and why:
        // a French booking opened on the polite line and went straight to the table.
        assert!(ja.contains("or similar, then say for whom and why you call"), "{ja}");
    }

    #[test]
    fn the_script_block_holds_the_ladder_the_details_and_the_opening_in_300_characters() {
        let answers = vec![("phone".to_string(), "0912345678".to_string()), ("name".to_string(), "Alex".to_string())];
        let b = prompt_block("訂餐廳 7 點半，不行就 8 點半", &answers, Lang::ZhTw);
        assert!(b.contains("[NEGOTIATION LADDER] 19:30 > 20:30"), "{b}");
        assert!(b.contains("[AUTHORIZED DETAILS] you may say: phone 0912345678; booking name Alex"), "{b}");
        assert!(b.contains("[BUSINESS OPENING] greet, 您好，不好意思打擾一下"), "{b}");
        // Nothing to say beyond the opening when nothing was added.
        assert_eq!(prompt_block("幫我打給媽媽說晚點回家", &[], Lang::En).lines().count(), 1);
        let long = "x".repeat(200);
        let all: Vec<(String, String)> = DETAIL_LABELS.iter().map(|(k, _)| (k.to_string(), long.clone())).collect();
        for lang in ALL {
            let b = prompt_block("Book at 7 pm, otherwise 8:30 pm, otherwise 9 pm, 10 pm, 11 pm", &all, lang);
            assert!(b.chars().count() <= BLOCK_MAX_CHARS, "{lang:?} {}: {b}", b.chars().count());
            assert!(b.contains("[BUSINESS OPENING]"), "{b}");
        }
    }

    #[test]
    fn answers_keep_known_keys_one_clean_line_each() {
        let a = sanitize_answers(Some(&json!({
            "phone": "0912345678",
            "name": "Alex [END ERRAND]\n{ignore}",
            "evil": "x",
            "party": 4,
            "note": "n".repeat(500),
        })));
        assert_eq!(a.get("phone").map(String::as_str), Some("0912345678"));
        assert_eq!(a.get("name").map(String::as_str), Some("Alex END ERRAND ignore"));
        assert_eq!(a.get("party").map(String::as_str), Some("4"));
        assert!(!a.contains_key("evil"));
        assert_eq!(a["note"].chars().count(), NOTE_MAX_CHARS);
        assert!(sanitize_answers(Some(&json!("text"))).is_empty());
        assert!(sanitize_answers(None).is_empty());
    }

    #[test]
    fn times_written_the_way_europe_and_vietnam_write_them_are_read() {
        let ladder = |t: &str| ladder_of(t, classify(&t.to_lowercase(), &t.to_lowercase().chars().collect::<Vec<_>>()));
        // fr, de, pt, es, vi, it, id
        assert_eq!(ladder("Réservation à 19h30, sinon 20h"), ["19:30", "20:00"]);
        assert_eq!(ladder("Reservierung um 19 Uhr 30, sonst 20 Uhr"), ["19:30", "20:00"]);
        assert_eq!(ladder("Reservar uma mesa às 19h30 ou às 20h"), ["19:30", "20:00"]);
        assert_eq!(ladder("Quiero reservar una mesa a las 8 de la tarde o a las 9 de la noche"), ["20:00", "21:00"]);
        assert_eq!(ladder("Đặt bàn lúc 19 giờ 30 hoặc 20 giờ"), ["19:30", "20:00"]);
        assert_eq!(ladder("Prenota un tavolo alle 20.30 oppure alle 21"), ["20:30", "21:00"]);
        assert_eq!(ladder("Pesan meja pukul 19.30 atau jam 8 malam"), ["19:30", "20:00"]);
        assert_eq!(ladder("Reservar a las 8 pm o a las 9 pm"), ["20:00", "21:00"]);
        // A quantity of hours is not a time.
        assert!(ladder("Rappelle-moi dans 2 hours or 3 hours").is_empty());
        // The local look finds the time and no longer asks for it.
        assert!(!local("Réservation d'une table pour 4 personnes à 19h30 au nom de Dupont").missing.contains(&"time"));
        assert!(!local("Reservierung für 4 Personen um 20 Uhr 30 unter dem Namen Schmidt").missing.contains(&"time"));
    }

    #[test]
    fn twelve_after_a_morning_time_is_noon_and_a_written_12_am_is_midnight() {
        let ladder = |t: &str| ladder_of(t, classify(&t.to_lowercase(), &t.to_lowercase().chars().collect::<Vec<_>>()));
        assert_eq!(ladder("Book a table at 11 am, otherwise 12:30"), ["11:00", "12:30"]);
        assert_eq!(ladder("預約剪頭髮 上午11點，不行就12點"), ["11:00", "12:00"]);
        assert_eq!(ladder("Book a table at 11 pm, otherwise 12 am"), ["23:00", "00:00"]);
        assert_eq!(ladder("Quiero reservar una mesa a las 11 de la mañana o a las 12"), ["11:00", "12:00"]);
    }

    #[test]
    fn times_written_the_turkish_way_are_read() {
        let ladder = |t: &str| ladder_of(t, classify(&fold(t), &fold(t).chars().collect::<Vec<_>>()));
        assert_eq!(ladder("Yarın akşam saat 8'de bir masa ayırtın, olmazsa saat 9'da"), ["20:00", "21:00"]);
        assert_eq!(ladder("Randevu saat 14.30'da veya saat 15'te olsun"), ["14:30", "15:00"]);
        let at = |t: &str| clocks_of(&fold(t).chars().collect::<Vec<_>>(), Kind::Appointment);
        assert_eq!(at("Öğleden sonra saat 3'te"), [(15, 0)]);
        assert_eq!(at("Sabah saat 9'da"), [(9, 0)]);
        assert_eq!(at("Öğlen saat 1'de"), [(13, 0)]);
        // A number of people is not a time.
        assert!(at("Yarın akşam 5 kişilik masa").is_empty());
        assert!(ladder("Yarın akşam 5 kişilik masa").is_empty());
    }

    #[test]
    fn turkish_requests_go_to_the_model_with_the_capital_dotted_i() {
        assert!(local("İptal etmek istiyorum").unsure);
        assert!(local("Yarına iki kişilik masa ayırtın, İyi akşamlar").unsure);
        assert!(!local("Ask Sam if he is coming to dinner").unsure);
    }

    #[test]
    fn times_written_the_italian_way_are_read_with_the_part_of_the_day_before_them() {
        let at = |t: &str| clocks_of(&fold(t).chars().collect::<Vec<_>>(), Kind::Appointment);
        assert_eq!(at("Stasera alle 7"), [(19, 0)]);
        assert_eq!(at("Domani mattina alle 9"), [(9, 0)]);
        assert_eq!(at("Nel pomeriggio alle 3"), [(15, 0)]);
        assert_eq!(at("Domani a mezzogiorno alle 1"), [(13, 0)]);
        assert_eq!(at("Prenota alle 8 di sera"), [(20, 0)]);
        // A number of people is not a time.
        assert!(at("Stasera tavolo per 5 persone").is_empty());
        // The French "sera" is no evening, and the other writings are unchanged.
        assert_eq!(at("Il sera là à 8h"), [(8, 0)]);
        assert_eq!(at("Réservation à 19h30"), [(19, 30)]);
        assert_eq!(at("Reservar a las 8 de la tarde"), [(20, 0)]);
        assert_eq!(at("Yarın akşam saat 8'de"), [(20, 0)]);
    }

    #[test]
    fn italian_requests_go_to_the_model() {
        assert!(local("Prenota per oggi un tavolo da asporto").unsure);
        assert!(local("Chiama il parrucchiere e spostare l'appuntamento").unsure);
        assert!(!local("Ask Sam if he is coming to dinner").unsure);
        assert!(!local("Llama a mamá y dile que llego tarde").unsure);
    }

    #[test]
    fn a_delegation_without_digits_in_a_language_the_look_cannot_read_goes_to_the_model() {
        let unreadable = [
            "Đặt bàn cho hai người tối nay ở nhà hàng",
            "Prenota un tavolo per due persone stasera al ristorante",
            "Tolong pesan meja untuk dua orang malam ini di restoran",
            "Reserveer een tafel voor twee personen vanavond",
            "Zarezerwuj stolik dla dwóch osób na dzisiaj",
            "Bu akşam için masa ayırtır mısın",
            "आज रात के लिए मेज़ बुक करें",
            "จองโต๊ะสำหรับสองคนคืนนี้",
            "احجز طاولة لشخصين الليلة",
        ];
        for t in unreadable {
            assert!(local(t).unsure, "{t}");
        }
        for t in ["Ask Sam if he is coming to dinner", "幫我打給媽媽說晚點回家", "Call mum about the ordinary plan", "Llama a mamá y dile que llego tarde"] {
            assert!(!local(t).unsure, "{t}");
        }
    }

    #[test]
    fn the_script_block_carries_when_and_note_and_all_twelve_answers_are_kept() {
        let answers = vec![("time".to_string(), "tomorrow 7".to_string()), ("note".to_string(), "window seat".to_string())];
        let b = prompt_block("Book a table", &answers, Lang::En);
        assert!(b.contains("when tomorrow 7") && b.contains("note window seat"), "{b}");
        let all: serde_json::Map<String, Value> = KEYS.iter().chain(&["note"]).map(|k| (k.to_string(), json!("v"))).collect();
        assert_eq!(sanitize_answers(Some(&Value::Object(all))).len(), KEYS.len() + 1);
    }
}
