// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! When an AI errand dials, and how it keeps dialing until it gets through
//! ("Redial until answered", grab mode).
//!
//! - [GRAB]: every number of grab mode in one place, and [GRAB_CASES]: what
//!   each way a try can end leads to. [grab_next_at] works out the next try.
//! - Scheduled errands: the time the owner set ([parse_at], [check_at]) and
//!   the line it holds from just before that time ([hold]).
//! - A time read from a chat message ([call_time_in], [leading_when]).
//! - How the owner reads all this, in the nine languages ([when_label], [words]).
//!
//! Times are the owner's local time: Bageo's clock in its `TZ`.

use std::time::Duration;

use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Timelike, Utc};

use crate::locale::Lang;

// ---------------------------------------------------------------- grab mode

/// The numbers of grab mode. Changed here only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrabRules {
    /// A scheduled errand's first try goes out this long before its time (a
    /// mobile call takes about two seconds to set up).
    pub lead: Duration,
    /// The next try, this long after the phone reported the last one over
    /// (time for the phone to free the line).
    pub gap: Duration,
    /// Tries start within this long of the first one (a scheduled errand:
    /// of its time). Once a person is on the line, no more tries.
    pub window: Duration,
    /// Not a rule, a guard against a loop gone wrong: at most this many dials
    /// in any one minute.
    pub per_minute_cap: usize,
    /// A queue's recording ("please hold", music, "you are number 3"): the AI
    /// stays on the line this long for a person.
    pub queue_wait: Duration,
}

pub const GRAB: GrabRules = GrabRules {
    lead: Duration::from_secs(2),
    gap: Duration::from_secs(3),
    window: Duration::from_secs(30 * 60),
    per_minute_cap: 10,
    queue_wait: Duration::from_secs(30 * 60),
};

/// How a try ended, as grab mode tells them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrabCase {
    /// It rang out or was declined after ringing (a ringing call is never hung
    /// up early: it rings until voicemail, a busy line or the other side ends it).
    NoAnswer,
    /// It ended before it connected: a busy tone or a fast busy.
    EndedEarly,
    /// The carrier's "the line is busy" recording.
    BusyMessage,
    /// The carrier's "switched off / out of reach" recording.
    Unreachable,
    /// The carrier's "not in service / no such number".
    NotInService,
    /// Voicemail picked up: hung up at once, no message.
    Voicemail,
    /// The other side said bookings are not open yet; the AI ended politely.
    NotOpenYet,
    /// A queue held the line past [GrabRules::queue_wait].
    QueueTooLong,
    /// A voice menu that needs key presses, and no person came on.
    KeypadMenu,
    /// The phone could not dial, or none was connected.
    PhoneTrouble,
    /// A person picked up and the errand went its way with them.
    PickedUp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrabMove {
    /// Tried again [GrabRules::gap] later, within the window.
    DialAgain,
    /// The errand is over.
    Stop,
}

/// What each way a try ends leads to in grab mode.
pub const GRAB_CASES: [(GrabCase, GrabMove); 11] = [
    (GrabCase::NoAnswer, GrabMove::DialAgain),
    (GrabCase::EndedEarly, GrabMove::DialAgain),
    (GrabCase::BusyMessage, GrabMove::DialAgain),
    (GrabCase::Unreachable, GrabMove::DialAgain),
    (GrabCase::NotInService, GrabMove::Stop),
    (GrabCase::Voicemail, GrabMove::DialAgain),
    (GrabCase::NotOpenYet, GrabMove::DialAgain),
    (GrabCase::QueueTooLong, GrabMove::DialAgain),
    (GrabCase::KeypadMenu, GrabMove::Stop),
    (GrabCase::PhoneTrouble, GrabMove::DialAgain),
    (GrabCase::PickedUp, GrabMove::Stop),
];

/// Whether a try that ended so is dialed again ([GRAB_CASES]).
pub fn dials_again(case: GrabCase) -> bool {
    GRAB_CASES.iter().find(|(c, _)| *c == case).is_some_and(|(_, m)| *m == GrabMove::DialAgain)
}

/// A std duration as chrono's (zero if it does not fit).
pub fn span(d: Duration) -> chrono::Duration {
    chrono::Duration::from_std(d).unwrap_or_else(|_| chrono::Duration::seconds(0))
}

/// When the next try goes out: `gap` after the last one `ended`, and no
/// earlier than a minute after the dial `per_minute_cap` dials back (`dials`:
/// this run's latest dial times). None: that is past the window that started
/// at `since` (the errand gives up).
pub fn grab_next_at(rules: &GrabRules, since: DateTime<Utc>, dials: &[DateTime<Utc>], ended: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let mut at = ended + span(rules.gap);
    if rules.per_minute_cap > 0 && dials.len() >= rules.per_minute_cap {
        let mut sorted = dials.to_vec();
        sorted.sort();
        at = at.max(sorted[sorted.len() - rules.per_minute_cap] + chrono::Duration::seconds(60));
    }
    (at < since + span(rules.window)).then_some(at)
}

// ---------------------------------------------------------------- scheduled errands

/// From when to when (the end not included).
pub type Span = (DateTime<Utc>, DateTime<Utc>);

/// How far ahead an errand may be scheduled.
pub const MAX_DAYS: i64 = 60;
/// A scheduled errand without grab mode holds its line this long from its
/// time (a call of up to 10 minutes and its ringing).
pub const PLAIN_HOLD: Duration = Duration::from_secs(12 * 60);
/// A scheduled errand without grab mode that Bageo could not start on time
/// (it was not running) still goes out this long after its time.
pub const LATE_MAX: Duration = Duration::from_secs(5 * 60);

/// The line a scheduled errand holds: from its first dial ([GrabRules::lead]
/// before its time) to the end of its window (grab) or of one call.
pub fn hold(at: DateTime<Utc>, grab: bool) -> (DateTime<Utc>, DateTime<Utc>) {
    (at - span(GRAB.lead), at + span(if grab { GRAB.window } else { PLAIN_HOLD }))
}

/// Until when a scheduled errand may still start once its time has passed.
pub fn start_by(at: DateTime<Utc>, grab: bool) -> DateTime<Utc> {
    at + span(if grab { GRAB.window } else { LATE_MAX })
}

/// A local wall-clock time in the owner's zone (the earlier one when the
/// clock goes back; None when it is skipped).
pub fn local_of(n: NaiveDateTime) -> Option<DateTime<Local>> {
    Local.from_local_datetime(&n).earliest()
}

/// A time as the API takes it: RFC 3339, or local `YYYY-MM-DDTHH:MM[:SS]`.
pub fn parse_at(s: &str) -> Option<DateTime<Local>> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&Local));
    }
    ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"]
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(s, f).ok())
        .and_then(local_of)
}

/// A time an errand may be scheduled at: in the future (`past_time`), at most
/// [MAX_DAYS] ahead (`too_far`).
pub fn check_at(at: DateTime<Utc>, now: DateTime<Utc>) -> Result<(), &'static str> {
    if at <= now {
        return Err("past_time");
    }
    if at > now + chrono::Duration::days(MAX_DAYS) {
        return Err("too_far");
    }
    Ok(())
}

/// `[a0, a1)` and `[b0, b1)` share a moment.
pub fn overlaps(a: (DateTime<Utc>, DateTime<Utc>), b: (DateTime<Utc>, DateTime<Utc>)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

// ---------------------------------------------------------------- times in chat messages

fn cn_digit(c: char) -> Option<u32> {
    Some(match c {
        '〇' | '零' => 0,
        '一' => 1,
        '二' | '兩' | '两' => 2,
        '三' => 3,
        '四' => 4,
        '五' => 5,
        '六' => 6,
        '七' => 7,
        '八' => 8,
        '九' => 9,
        '十' => 10,
        _ => return None,
    })
}

/// 八 = 8, 十 = 10, 十二 = 12, 二十 = 20, 二十一 = 21.
fn cn_number(run: &[char]) -> Option<u32> {
    let v: Vec<u32> = run.iter().map(|c| cn_digit(*c)).collect::<Option<_>>()?;
    match v.as_slice() {
        [d] => Some(*d),
        [10, d] if *d < 10 => Some(10 + d),
        [t, 10] if *t < 10 => Some(t * 10),
        [t, 10, d] if *t < 10 && *d < 10 => Some(t * 10 + d),
        _ => None,
    }
}

/// Lower case, any digits as ASCII ones, Chinese numerals as digits.
fn normalize(text: &str) -> Vec<char> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        if cn_digit(chars[i]).is_some() {
            let start = i;
            while i < chars.len() && cn_digit(chars[i]).is_some() {
                i += 1;
            }
            match cn_number(&chars[start..i]) {
                Some(n) => out.extend(n.to_string().chars()),
                None => out.extend_from_slice(&chars[start..i]),
            }
            continue;
        }
        let c = chars[i];
        let c = match c {
            '０'..='９' => char::from_u32(c as u32 - '０' as u32 + '0' as u32).unwrap_or(c),
            '：' => ':',
            '／' => '/',
            '．' => '.',
            '\u{3000}' => ' ',
            _ => c,
        };
        out.extend(c.to_lowercase());
        i += 1;
    }
    out
}

#[derive(Debug, Clone, Copy)]
struct Num {
    val: u32,
    start: usize,
    end: usize,
    digits: usize,
}

fn numbers(t: &[char]) -> Vec<Num> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < t.len() {
        if t[i].is_ascii_digit() {
            let start = i;
            while i < t.len() && t[i].is_ascii_digit() {
                i += 1;
            }
            let digits = i - start;
            if digits <= 4 {
                let val = t[start..i].iter().collect::<String>().parse().unwrap_or(u32::MAX);
                out.push(Num { val, start, end: i, digits });
            } else {
                // A phone number or the like: never a date or a time.
                out.push(Num { val: u32::MAX, start, end: i, digits });
            }
        } else {
            i += 1;
        }
    }
    out
}

fn at_str(t: &[char], i: usize, s: &str) -> bool {
    s.chars().enumerate().all(|(k, c)| t.get(i + k) == Some(&c))
}

fn letter(c: Option<&char>) -> bool {
    c.is_some_and(|c| c.is_alphabetic() && !is_cjk(*c))
}

fn is_cjk(c: char) -> bool {
    matches!(c, '\u{3040}'..='\u{30FF}' | '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' | '\u{AC00}'..='\u{D7AF}' | '\u{1100}'..='\u{11FF}')
}

/// `word` at `i`, not inside a longer word of letters.
fn word_at(t: &[char], i: usize, word: &str) -> bool {
    let n = word.chars().count();
    at_str(t, i, word) && (i == 0 || !letter(t.get(i - 1))) && !letter(t.get(i + n))
}

/// Where `word` stands as a word in `t` (a CJK word: anywhere).
fn find_word(t: &[char], word: &str) -> Option<usize> {
    let cjk = word.chars().any(is_cjk);
    (0..t.len()).find(|&i| if cjk { at_str(t, i, word) } else { word_at(t, i, word) })
}

fn has_word(t: &[char], words: &[&str]) -> bool {
    words.iter().any(|w| find_word(t, w).is_some())
}

fn skip_spaces(t: &[char], mut i: usize) -> usize {
    while t.get(i).is_some_and(|c| c.is_whitespace()) {
        i += 1;
    }
    i
}

const MONTHS: &[(&str, u32)] = &[
    // en
    ("january", 1), ("february", 2), ("march", 3), ("april", 4), ("may", 5), ("june", 6), ("july", 7), ("august", 8),
    ("september", 9), ("october", 10), ("november", 11), ("december", 12), ("jan", 1), ("feb", 2), ("mar", 3), ("apr", 4),
    ("jun", 6), ("jul", 7), ("aug", 8), ("sept", 9), ("sep", 9), ("oct", 10), ("nov", 11), ("dec", 12),
    // es
    ("enero", 1), ("febrero", 2), ("marzo", 3), ("abril", 4), ("mayo", 5), ("junio", 6), ("julio", 7), ("agosto", 8),
    ("septiembre", 9), ("setiembre", 9), ("octubre", 10), ("noviembre", 11), ("diciembre", 12),
    // fr
    ("janvier", 1), ("février", 2), ("fevrier", 2), ("mars", 3), ("avril", 4), ("mai", 5), ("juin", 6), ("juillet", 7),
    ("août", 8), ("aout", 8), ("septembre", 9), ("octobre", 10), ("novembre", 11), ("décembre", 12), ("decembre", 12),
    ("janv", 1), ("févr", 2), ("déc", 12),
    // de
    ("januar", 1), ("jänner", 1), ("februar", 2), ("märz", 3), ("maerz", 3), ("juni", 6), ("juli", 7), ("oktober", 10),
    ("dezember", 12),
    // pt
    ("janeiro", 1), ("fevereiro", 2), ("março", 3), ("marco", 3), ("maio", 5), ("junho", 6), ("julho", 7),
    ("setembro", 9), ("outubro", 10), ("dezembro", 12), ("dez", 12),
];

const IN_TWO_DAYS: &[&str] = &["後天", "后天", "明後日", "あさって", "모레", "pasado mañana", "après-demain", "apres-demain", "übermorgen", "depois de amanhã", "depois de amanha", "day after tomorrow"];
const TOMORROW: &[&str] = &["明天", "明日", "あした", "내일", "tomorrow", "mañana", "demain", "morgen", "amanhã", "amanha"];
const TODAY: &[&str] = &["今天", "今日", "오늘", "today", "hoy", "aujourd'hui", "heute", "hoje"];
/// Words with "mañana" / "Morgen" in them that mean the morning, not tomorrow.
const MORNING_NOT_TOMORROW: &[&str] = &["de la mañana", "por la mañana", "morgens", "am morgen"];

const PM: &[&str] = &[
    "下午", "晚上", "傍晚", "晚間", "晚间", "夜裡", "夜里", "中午", "午後", "夕方", "夜", "오후", "저녁", "밤", "de la tarde", "de la noche",
    "por la tarde", "por la noche", "du soir", "de l'après-midi", "de l'apres-midi", "abends", "nachmittags", "am abend", "am nachmittag",
    "da tarde", "da noite", "à tarde", "à noite", "in the evening", "in the afternoon", "tonight",
];
const AM: &[&str] = &[
    "早上", "上午", "凌晨", "清晨", "早晨", "午前", "朝", "오전", "아침", "새벽", "de la mañana", "por la mañana", "du matin", "morgens",
    "vormittags", "am morgen", "da manhã", "da manha", "in the morning",
];

/// A date found in a text: its year if written, month, day, and the
/// character ranges it took.
type DateHit = (Option<i32>, u32, u32, Vec<(usize, usize)>);

/// The date in `t` (year if written, month, day) and the characters it took.
fn find_date(t: &[char], nums: &[Num], day_first: bool) -> Option<DateHit> {
    let ok_md = |m: u32, d: u32| (1..=12).contains(&m) && (1..=31).contains(&d);
    let sep = |i: usize, set: &[char]| t.get(i).is_some_and(|c| set.contains(c));
    for (k, n) in nums.iter().enumerate() {
        // 2026-12-01, 2026/12/1, 2026年12月1日
        if n.digits == 4 && (1900..=2100).contains(&n.val) && sep(n.end, &['-', '/', '.', '年']) {
            if let (Some(m), Some(d)) = (nums.get(k + 1), nums.get(k + 2)) {
                if m.start == n.end + 1 && sep(m.end, &['-', '/', '.', '月']) && d.start == m.end + 1 && ok_md(m.val, d.val) {
                    return Some((Some(n.val as i32), m.val, d.val, vec![(n.start, d.end + 1)]));
                }
            }
        }
    }
    for (k, n) in nums.iter().enumerate() {
        if n.digits > 2 {
            continue;
        }
        // 12月1日, 12月1號
        if sep(n.end, &['月']) {
            if let Some(d) = nums.get(k + 1).filter(|d| skip_spaces(t, n.end + 1) == d.start && ok_md(n.val, d.val)) {
                return Some((None, n.val, d.val, vec![(n.start, d.end + 1)]));
            }
        }
        // 12월 1일
        let after = skip_spaces(t, n.end);
        if sep(after, &['월']) {
            if let Some(d) = nums.get(k + 1).filter(|d| skip_spaces(t, after + 1) == d.start && ok_md(n.val, d.val)) {
                return Some((None, n.val, d.val, vec![(n.start, d.end + 1)]));
            }
        }
        // 12/1, 1/12, 12/1/2026
        if sep(n.end, &['/']) {
            if let Some(b) = nums.get(k + 1).filter(|b| b.start == n.end + 1 && b.digits <= 2) {
                let (m, d) = if day_first { (b.val, n.val) } else { (n.val, b.val) };
                if ok_md(m, d) {
                    let year = nums.get(k + 2).filter(|y| y.start == b.end + 1 && sep(b.end, &['/'])).map(|y| if y.digits == 2 { 2000 + y.val as i32 } else { y.val as i32 });
                    let end = year.map_or(b.end, |_| nums[k + 2].end);
                    return Some((year, m, d, vec![(n.start, end)]));
                }
            }
        }
        // 1.12. or 1.12.2026 (day first)
        if sep(n.end, &['.']) {
            if let Some(b) = nums.get(k + 1).filter(|b| b.start == n.end + 1 && b.digits <= 2 && sep(b.end, &['.'])) {
                if ok_md(b.val, n.val) {
                    let year = nums.get(k + 2).filter(|y| y.start == b.end + 1 && y.digits == 4).map(|y| y.val as i32);
                    let end = year.map_or(b.end + 1, |_| nums[k + 2].end);
                    return Some((year, b.val, n.val, vec![(n.start, end)]));
                }
            }
        }
    }
    // A month's name with its day before ("1 de diciembre", "1er décembre", "1. Dezember") or after ("Dec 1st").
    for i in 0..t.len() {
        let Some(&(name, month)) = MONTHS.iter().filter(|(w, _)| word_at(t, i, w)).max_by_key(|(w, _)| w.len()) else {
            continue;
        };
        let name_end = i + name.chars().count();
        let before = nums.iter().rev().find(|n| n.end <= i && n.digits <= 2).filter(|n| {
            let between: String = t[n.end..i].iter().collect();
            let b = between.trim().trim_end_matches("de").trim().trim_end_matches("of").trim();
            matches!(b, "" | "." | "º" | "°" | "er" | "st" | "nd" | "rd" | "th")
        });
        let after = nums.iter().find(|n| n.start >= name_end && n.digits <= 2).filter(|n| {
            let between: String = t[name_end..n.start].iter().collect();
            matches!(between.trim(), "" | "." | ",")
        });
        if let Some(d) = before.or(after).filter(|d| ok_md(month, d.val)) {
            let last = d.end.max(name_end);
            let year = nums.iter().find(|y| y.digits == 4 && y.start > last && y.start <= last + 5);
            let to = year.map_or(last, |y| y.end);
            return Some((year.map(|y| y.val as i32), month, d.val, vec![(d.start.min(i), to)]));
        }
    }
    None
}

fn relative_day(t: &[char]) -> Option<i64> {
    if has_word(t, IN_TWO_DAYS) {
        return Some(2);
    }
    if has_word(t, MORNING_NOT_TOMORROW) {
        let without: String = {
            let mut s: String = t.iter().collect();
            for w in MORNING_NOT_TOMORROW {
                s = s.replace(w, " ");
            }
            s
        };
        let w: Vec<char> = without.chars().collect();
        if has_word(&w, TOMORROW) {
            return Some(1);
        }
    } else if has_word(t, TOMORROW) {
        return Some(1);
    }
    has_word(t, TODAY).then_some(0)
}

/// The hour and minute in `t`, not counting what the date took.
fn find_time(t: &[char], nums: &[Num], taken: &[(usize, usize)]) -> Option<(u32, u32, Option<bool>)> {
    let free = |n: &Num| !taken.iter().any(|(a, b)| n.start >= *a && n.start < *b);
    let two = |n: Option<&Num>| n.filter(|m| m.digits == 2 && m.val < 60).map(|m| m.val);
    for (k, n) in nums.iter().enumerate() {
        if !free(n) || n.digits > 2 || n.val > 24 {
            continue;
        }
        let next = nums.get(k + 1);
        let mut hm: Option<(u32, u32)> = None;
        let mut meridiem: Option<bool> = None;
        // 8:30
        if t.get(n.end) == Some(&':') && next.is_some_and(|m| m.start == n.end + 1) {
            hm = two(next).map(|m| (n.val, m));
        }
        // 8.30 Uhr / 8.30h
        if hm.is_none() && t.get(n.end) == Some(&'.') && next.is_some_and(|m| m.start == n.end + 1) {
            if let Some(m) = two(next) {
                let after = skip_spaces(t, next.map_or(0, |x| x.end));
                if word_at(t, after, "uhr") || (t.get(after) == Some(&'h') && !letter(t.get(after + 1))) {
                    hm = Some((n.val, m));
                }
            }
        }
        let after = skip_spaces(t, n.end);
        if hm.is_none() {
            // 8點 8点 8時 8시 (8點半, 8點30分, 8시 반)
            if t.get(after).is_some_and(|c| matches!(c, '點' | '点' | '時' | '时' | '시')) {
                let rest = skip_spaces(t, after + 1);
                let min = if matches!(t.get(rest), Some('半') | Some('반')) {
                    30
                } else {
                    next.filter(|m| m.start == rest && m.val < 60 && t.get(skip_spaces(t, m.end)).is_some_and(|c| matches!(c, '分' | '분'))).map_or(0, |m| m.val)
                };
                hm = Some((n.val, min));
            }
        }
        if hm.is_none() && t.get(after) == Some(&'h') && !letter(t.get(after + 1)) {
            // 8h, 8h30, 20 h
            let m = next.filter(|m| m.start == after + 1).and_then(|m| two(Some(m))).unwrap_or(0);
            hm = Some((n.val, m));
        }
        if hm.is_none() && ["uhr", "heures", "heure", "horas", "hora", "o'clock", "oclock"].iter().any(|w| word_at(t, after, w)) {
            hm = Some((n.val, 0));
        }
        if hm.is_none() {
            for (w, pm) in [("a.m.", false), ("p.m.", true), ("am", false), ("pm", true)] {
                if at_str(t, after, w) && !letter(t.get(after + w.chars().count())) {
                    hm = Some((n.val, 0));
                    meridiem = Some(pm);
                    break;
                }
            }
        }
        if hm.is_none() {
            // "a las 8", "à 8", "às 8", "um 8", "at 8", "alle 8"
            let before: String = t[..n.start].iter().collect();
            let before = before.trim_end();
            if ["a las", "a la", " à", " às", " as", " um", " at", " alle"].iter().any(|w| before.ends_with(w.trim_start()) && {
                let rest = &before[..before.len() - w.trim_start().len()];
                rest.is_empty() || rest.ends_with(|c: char| !c.is_alphabetic())
            }) {
                hm = Some((n.val, 0));
            }
        }
        if let Some((h, m)) = hm {
            if meridiem.is_none() {
                let a = skip_spaces(t, next.filter(|x| x.start <= n.end + 1).map_or(n.end, |x| x.end));
                for (w, pm) in [("a.m.", false), ("p.m.", true), ("am", false), ("pm", true)] {
                    if at_str(t, a, w) && !letter(t.get(a + w.chars().count())) {
                        meridiem = Some(pm);
                    }
                }
            }
            if h <= 24 {
                return Some((h, m, meridiem));
            }
        }
    }
    None
}

/// The date and time in a short piece of text ("12/1 8點", "Dec 1 at 8am",
/// "le 1/12 à 20h"), relative to `now`. A date without a year is its next
/// one to come; a time without a date, today or else tomorrow. None without a
/// time. `day_first`: 1/12 is the 1st of December.
pub fn parse_when(text: &str, now: NaiveDateTime, day_first: bool) -> Option<NaiveDateTime> {
    let t = normalize(text);
    let nums = numbers(&t);
    let date = find_date(&t, &nums, day_first);
    let taken = date.as_ref().map(|d| d.3.clone()).unwrap_or_default();
    let (mut h, m, meridiem) = find_time(&t, &nums, &taken)?;
    let pm = meridiem.unwrap_or_else(|| has_word(&t, PM));
    let am = meridiem.map_or_else(|| has_word(&t, AM), |p| !p);
    if pm && h < 12 {
        h += 12;
    } else if am && h == 12 {
        h = 0;
    }
    if h == 24 {
        h = 0;
    }
    let time = NaiveTime::from_hms_opt(h, m, 0)?;
    if let Some((year, month, day, _)) = date {
        if let Some(y) = year {
            return NaiveDate::from_ymd_opt(y, month, day).map(|d| d.and_time(time));
        }
        return (0..=8).find_map(|k| NaiveDate::from_ymd_opt(now.year() + k, month, day).map(|d| d.and_time(time)).filter(|at| *at > now));
    }
    if let Some(days) = relative_day(&t) {
        return Some((now.date() + chrono::Duration::days(days)).and_time(time));
    }
    let today = now.date().and_time(time);
    Some(if today > now { today } else { today + chrono::Duration::days(1) })
}

/// Words that ask for the call: a time before them is when to call.
const CALL_WORDS: &[&str] = &[
    "打給", "打给", "打電話", "打电话", "撥給", "拨给", "撥打", "拨打", "致電", "致电", "打去", "電話して", "電話を", "電話", "전화", "call", "phone", "ring",
    "llama", "llamar", "llámame", "appelle", "appeler", "téléphone", "telephone", "ruf", "rufe", "anrufen", "liga", "ligar", "ligue", "telefona",
];

/// Whether 1/12 means the 1st of December in `text`: not in Chinese,
/// Japanese, Korean or English; yes in Spanish, French, German, Portuguese;
/// else as the owner's language writes dates.
fn day_first(text: &str, owner: Lang) -> bool {
    let t = normalize(text);
    if t.iter().any(|c| is_cjk(*c)) {
        return false;
    }
    if has_word(&t, &["llama", "llamar", "a las", "para", "appelle", "appeler", "pour", "à", "ruf", "rufe", "anrufen", "uhr", "liga", "ligar", "ligue", "às", "dia", "le", "el"]) {
        return true;
    }
    if has_word(&t, &["call", "at", "on", "book", "pm", "am", "tomorrow"]) {
        return false;
    }
    matches!(owner, Lang::Es | Lang::Fr | Lang::De | Lang::Pt)
}

/// When the owner wants the call placed, if their message says it before
/// the words that ask for the call ("12/1 8點幫我打給…", "On Dec 1 at 8 am
/// call…", "12월 1일 오전 8시에 …에 전화해서"). A time after those words is
/// part of the task ("book for 7 pm"), and a message without them has none:
/// None, the call goes now.
pub fn call_time_in(text: &str, now: NaiveDateTime, owner: Lang) -> Option<NaiveDateTime> {
    let t = normalize(text);
    let cut = CALL_WORDS.iter().filter_map(|w| find_word(&t, w)).min()?;
    let head: String = t[..cut].iter().collect();
    if head.trim().is_empty() {
        return None;
    }
    parse_when(&head, now, day_first(text, owner))
}

/// A word of a date or time as a chat command writes it before the number
/// ("/errand 12/1 8:00 0912345678 …").
fn when_token(tok: &str) -> bool {
    let t = normalize(tok);
    let s: String = t.iter().collect();
    let digits = t.iter().any(char::is_ascii_digit);
    if digits && t.iter().filter(|c| c.is_ascii_digit()).count() > 8 {
        return false;
    }
    (digits && t.iter().any(|c| matches!(c, '/' | ':' | '-' | '月' | '日' | '號' | '号' | '點' | '点' | '時' | '时' | '시' | '월' | '일' | '年')))
        || (digits && (s.ends_with("am") || s.ends_with("pm") || s.ends_with('.') || s.contains('h')))
        || ["am", "pm", "a.m.", "p.m.", "uhr", "at", "um", "à", "às", "a", "las", "de", "le", "el", "on"].contains(&s.as_str())
        || TOMORROW.contains(&s.as_str())
        || IN_TWO_DAYS.contains(&s.as_str())
        || TODAY.contains(&s.as_str())
        || PM.contains(&s.as_str())
        || AM.contains(&s.as_str())
        || MONTHS.iter().any(|(m, _)| s.trim_end_matches(['.', ',']) == *m)
        || (digits && s.trim_end_matches([',', '.', 'º', '°']).chars().all(|c| c.is_ascii_digit()) && s.len() <= 2)
}

/// The date and time a chat command starts with ("12/1 08:00 0912345678 …"),
/// as written, and what follows it. Read into a time with [resolve_when].
pub fn leading_when(args: &str) -> Option<(&str, &str)> {
    let mut end = 0;
    let mut rest = args;
    loop {
        let trimmed = rest.trim_start();
        let lead = rest.len() - trimmed.len();
        let tok_len = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
        if tok_len == 0 || !when_token(&trimmed[..tok_len]) {
            break;
        }
        end += lead + tok_len;
        rest = &trimmed[tok_len..];
    }
    if end == 0 {
        return None;
    }
    let head = &args[..end];
    // Only what reads as a time (a lone "2" does not). Any day will do for that check.
    let any_day = NaiveDate::from_ymd_opt(2026, 6, 15)?.and_hms_opt(0, 0, 0)?;
    parse_when(head, any_day, false)?;
    Some((head.trim(), args[end..].trim_start()))
}

/// The time an errand's `when` names: `YYYY-MM-DDTHH:MM` (what the chat
/// model writes), else a date and time as the owner wrote them ([parse_when]).
pub fn resolve_when(when: &str, now: NaiveDateTime, owner: Lang) -> Option<NaiveDateTime> {
    let w = when.trim();
    ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"]
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(w, f).ok())
        .or_else(|| parse_when(w, now, day_first(w, owner)))
}

// ---------------------------------------------------------------- the owner's words

/// "12/1（週一）08:00": a scheduled time as the owner reads it, the year
/// only when it is not this one.
pub fn when_label(lang: Lang, at: NaiveDateTime, now: NaiveDateTime) -> String {
    let wd = at.weekday().num_days_from_monday() as usize;
    let other_year = at.year() != now.year();
    let (y, mo, d, hm) = (at.year(), at.month(), at.day(), format!("{:02}:{:02}", at.hour(), at.minute()));
    const EN_M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    match lang {
        Lang::ZhTw => {
            let w = ["週一", "週二", "週三", "週四", "週五", "週六", "週日"][wd];
            if other_year { format!("{y}/{mo}/{d}（{w}）{hm}") } else { format!("{mo}/{d}（{w}）{hm}") }
        }
        Lang::ZhCn => {
            let w = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"][wd];
            if other_year { format!("{y}年{mo}月{d}日（{w}）{hm}") } else { format!("{mo}月{d}日（{w}）{hm}") }
        }
        Lang::Ja => {
            let w = ["月", "火", "水", "木", "金", "土", "日"][wd];
            if other_year { format!("{y}年{mo}月{d}日（{w}）{hm}") } else { format!("{mo}月{d}日（{w}）{hm}") }
        }
        Lang::Ko => {
            let w = ["월", "화", "수", "목", "금", "토", "일"][wd];
            if other_year { format!("{y}년 {mo}월 {d}일 ({w}) {hm}") } else { format!("{mo}월 {d}일 ({w}) {hm}") }
        }
        Lang::Es => {
            let w = ["lun", "mar", "mié", "jue", "vie", "sáb", "dom"][wd];
            let m = ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sept", "oct", "nov", "dic"][mo as usize - 1];
            if other_year { format!("{w} {d} {m} {y}, {hm}") } else { format!("{w} {d} {m}, {hm}") }
        }
        Lang::Fr => {
            let w = ["lun.", "mar.", "mer.", "jeu.", "ven.", "sam.", "dim."][wd];
            let m = ["janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.", "déc."][mo as usize - 1];
            if other_year { format!("{w} {d} {m} {y}, {hm}") } else { format!("{w} {d} {m}, {hm}") }
        }
        Lang::De => {
            let w = ["Mo.", "Di.", "Mi.", "Do.", "Fr.", "Sa.", "So."][wd];
            if other_year { format!("{w}, {d}.{mo}.{y}, {hm}") } else { format!("{w}, {d}.{mo}., {hm}") }
        }
        Lang::Pt => {
            let w = ["seg", "ter", "qua", "qui", "sex", "sáb", "dom"][wd];
            let m = ["jan", "fev", "mar", "abr", "mai", "jun", "jul", "ago", "set", "out", "nov", "dez"][mo as usize - 1];
            if other_year { format!("{w}, {d} {m} {y}, {hm}") } else { format!("{w}, {d} {m}, {hm}") }
        }
        Lang::En | Lang::Ext(_) => {
            let w = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"][wd];
            let m = EN_M[mo as usize - 1];
            if other_year { format!("{w}, {m} {d}, {y}, {hm}") } else { format!("{w}, {m} {d}, {hm}") }
        }
    }
}

/// [when_label] of an RFC 3339 stamp, in the local zone; empty when unreadable.
pub fn stamp_label(lang: Lang, stamp: &str) -> String {
    DateTime::parse_from_rfc3339(stamp.trim())
        .map(|t| when_label(lang, t.with_timezone(&Local).naive_local(), Local::now().naive_local()))
        .unwrap_or_default()
}

/// What the owner reads about scheduled errands and grab mode. `{when}`: a
/// [when_label]; `{line}`: the phone and SIM; `{gap}`, `{min}`: [GRAB]'s gap
/// in seconds and window in minutes; `{who}`: the errand holding a line.
pub struct Words {
    /// Notification: an errand was scheduled.
    pub scheduled: &'static str,
    /// Notification: its time came, the first try goes out.
    pub started: &'static str,
    /// Label: when it dials.
    pub when: &'static str,
    /// Label: the phone and SIM it dials from.
    pub line: &'static str,
    /// After the line: picked by Bageo (the owner named none).
    pub line_auto: &'static str,
    /// What grab mode does.
    pub grab: &'static str,
    /// Grab mode off: one call.
    pub once: &'static str,
    /// Where a scheduled errand stands (list, chat).
    pub status: &'static str,
    /// Why it never went out: Bageo was not running at its time.
    pub missed: &'static str,
    /// A try: the other side said bookings are not open yet.
    pub not_open: &'static str,
    /// A try: a queue held the line too long.
    pub queue_long: &'static str,
    pub e_bad_time: &'static str,
    pub e_past_time: &'static str,
    pub e_too_far: &'static str,
    pub e_need_line: &'static str,
    pub e_line_taken: &'static str,
    pub e_line_reserved: &'static str,
    pub e_not_scheduled: &'static str,
}

pub fn words(lang: Lang) -> &'static Words {
    match lang {
        Lang::ZhTw => &ZH_TW,
        Lang::ZhCn => &ZH_CN,
        Lang::Ja => &JA,
        Lang::Ko => &KO,
        Lang::Es => &ES,
        Lang::Fr => &FR,
        Lang::De => &DE,
        Lang::Pt => &PT,
        Lang::En | Lang::Ext(_) => &EN,
    }
}

/// `template` with `{gap}` and `{min}` from [GRAB].
pub fn fill_grab(template: &str) -> String {
    template.replace("{gap}", &GRAB.gap.as_secs().to_string()).replace("{min}", &(GRAB.window.as_secs() / 60).to_string())
}

const ZH_TW: Words = Words {
    scheduled: "已排定，時間到會自動撥出",
    started: "時間到了，開始撥打",
    when: "撥打時間",
    line: "用哪支門號",
    line_auto: "{line}（自動挑那個時間空著的）",
    grab: "沒打進去就 {gap} 秒後重撥，接通就停，最多 {min} 分鐘",
    once: "只打一次",
    status: "排定 {when}",
    missed: "排定的時間 Bageo 沒在執行，這通沒有撥出",
    not_open: "對方說還沒開放",
    queue_long: "排隊等太久，沒等到人接",
    e_bad_time: "看不懂這個時間，請寫日期和幾點，例如 12/1 08:00。",
    e_past_time: "這個時間已經過了，請選之後的時間。",
    e_too_far: "最多只能排 {days} 天內的時間。",
    e_need_line: "排定時間的代打要選用哪支手機的哪張卡。",
    e_line_taken: "這支門號那段時間已經被「{who}」（{when}）排走了，請換一支或換時間。",
    e_line_reserved: "這支門號接下來被排定的代打佔用（{who}，{when}），請換一支。",
    e_not_scheduled: "這筆已經開始或結束，不能再改時間。",
};

const ZH_CN: Words = Words {
    scheduled: "已安排，时间到会自动拨出",
    started: "时间到了，开始拨打",
    when: "拨打时间",
    line: "用哪个号码",
    line_auto: "{line}（自动挑那个时间空着的）",
    grab: "没打进去就 {gap} 秒后重拨，接通就停，最多 {min} 分钟",
    once: "只打一次",
    status: "已安排 {when}",
    missed: "安排的时间 Bageo 没在运行，这通没有拨出",
    not_open: "对方说还没开放",
    queue_long: "排队等太久，没等到人接",
    e_bad_time: "看不懂这个时间，请写日期和几点，例如 12/1 08:00。",
    e_past_time: "这个时间已经过了，请选之后的时间。",
    e_too_far: "最多只能安排 {days} 天内的时间。",
    e_need_line: "定时代打要选用哪部手机的哪张卡。",
    e_line_taken: "这个号码那段时间已经被“{who}”（{when}）占用了，请换一个或换时间。",
    e_line_reserved: "这个号码接下来被定时代打占用（{who}，{when}），请换一个。",
    e_not_scheduled: "这条已经开始或结束，不能再改时间。",
};

const EN: Words = Words {
    scheduled: "Scheduled: it dials by itself at that time",
    started: "Time's up: dialing now",
    when: "When",
    line: "Calls from",
    line_auto: "{line} (picked: free at that time)",
    grab: "If it doesn't get through, it redials {gap} s later until someone answers, for up to {min} minutes",
    once: "One call only",
    status: "Scheduled for {when}",
    missed: "Bageo wasn't running at the scheduled time, so this call was not made",
    not_open: "They said bookings aren't open yet",
    queue_long: "Waited in their queue too long; nobody came on",
    e_bad_time: "That time isn't clear. Write the date and time, e.g. 12/1 08:00.",
    e_past_time: "That time has passed. Pick a later one.",
    e_too_far: "You can schedule up to {days} days ahead.",
    e_need_line: "A scheduled call needs a phone and SIM to call from.",
    e_line_taken: "That line is already booked then by “{who}” ({when}). Pick another line or time.",
    e_line_reserved: "That line is booked soon for a scheduled call ({who}, {when}). Pick another line.",
    e_not_scheduled: "This call has already started or ended, so its time can't change.",
};

const JA: Words = Words {
    scheduled: "予約しました。時間になると自動で発信します",
    started: "時間になりました。発信します",
    when: "発信日時",
    line: "発信する番号",
    line_auto: "{line}（その時間に空いている番号を自動で選択）",
    grab: "つながらなければ {gap} 秒後にかけ直し、相手が出たら止めます（最長 {min} 分）",
    once: "1 回だけ",
    status: "{when} に予約済み",
    missed: "予約時刻に Bageo が動いていなかったため、発信していません",
    not_open: "まだ受付開始前だと言われました",
    queue_long: "順番待ちが長すぎて、担当者につながりませんでした",
    e_bad_time: "日時がわかりません。12/1 08:00 のように日付と時刻を書いてください。",
    e_past_time: "その時刻は過ぎています。これから先の時刻を選んでください。",
    e_too_far: "予約できるのは {days} 日先までです。",
    e_need_line: "予約発信には、発信するスマホと SIM を選んでください。",
    e_line_taken: "その番号はその時間「{who}」（{when}）で予約済みです。別の番号か時間を選んでください。",
    e_line_reserved: "その番号はこのあと予約発信で使います（{who}、{when}）。別の番号を選んでください。",
    e_not_scheduled: "この発信はもう始まったか終わったため、時刻を変えられません。",
};

const KO: Words = Words {
    scheduled: "예약했어요. 그 시간에 자동으로 겁니다",
    started: "시간이 됐어요. 지금 겁니다",
    when: "걸 시간",
    line: "걸 번호",
    line_auto: "{line} (그 시간에 비어 있는 번호로 자동 선택)",
    grab: "연결이 안 되면 {gap}초 뒤 다시 걸고, 받으면 멈춥니다 (최대 {min}분)",
    once: "한 번만",
    status: "{when} 예약됨",
    missed: "예약 시간에 Bageo가 실행 중이 아니어서 걸지 못했습니다",
    not_open: "아직 예약 접수 전이라고 했습니다",
    queue_long: "대기 순서가 너무 길어 상담원과 연결되지 않았습니다",
    e_bad_time: "시간을 알 수 없어요. 12/1 08:00처럼 날짜와 시간을 써 주세요.",
    e_past_time: "이미 지난 시간이에요. 이후 시간을 골라 주세요.",
    e_too_far: "{days}일 이내로만 예약할 수 있어요.",
    e_need_line: "예약 전화는 걸 휴대폰과 SIM을 골라야 해요.",
    e_line_taken: "그 번호는 그 시간에 ‘{who}’({when}) 예약이 있어요. 다른 번호나 시간을 골라 주세요.",
    e_line_reserved: "그 번호는 곧 예약 전화에 쓰여요({who}, {when}). 다른 번호를 골라 주세요.",
    e_not_scheduled: "이미 시작했거나 끝난 전화라 시간을 바꿀 수 없어요.",
};

const ES: Words = Words {
    scheduled: "Programada: llamará sola a esa hora",
    started: "Llegó la hora: llamando ahora",
    when: "Cuándo",
    line: "Llama desde",
    line_auto: "{line} (elegida: libre a esa hora)",
    grab: "Si no entra, vuelve a llamar a los {gap} s hasta que contesten, durante {min} minutos como máximo",
    once: "Una sola llamada",
    status: "Programada para el {when}",
    missed: "Bageo no estaba en marcha a la hora programada y no se hizo la llamada",
    not_open: "Dijeron que aún no abren las reservas",
    queue_long: "La espera en cola fue demasiado larga y nadie atendió",
    e_bad_time: "No se entiende la hora. Escribe la fecha y la hora, p. ej. 1/12 08:00.",
    e_past_time: "Esa hora ya pasó. Elige una posterior.",
    e_too_far: "Puedes programar hasta {days} días antes.",
    e_need_line: "Una llamada programada necesita el teléfono y la SIM desde los que llamar.",
    e_line_taken: "Esa línea ya está reservada a esa hora para «{who}» ({when}). Elige otra línea u otra hora.",
    e_line_reserved: "Esa línea se usará pronto para una llamada programada ({who}, {when}). Elige otra.",
    e_not_scheduled: "Esta llamada ya empezó o terminó; no se puede cambiar la hora.",
};

const FR: Words = Words {
    scheduled: "Programmé : l'appel partira tout seul à cette heure",
    started: "C'est l'heure : appel en cours",
    when: "Quand",
    line: "Appelle depuis",
    line_auto: "{line} (choisie : libre à cette heure)",
    grab: "Si ça ne passe pas, rappel {gap} s plus tard jusqu'à ce qu'on décroche, pendant {min} minutes au plus",
    once: "Un seul appel",
    status: "Programmé le {when}",
    missed: "Bageo ne tournait pas à l'heure prévue : cet appel n'a pas été passé",
    not_open: "Ils ont dit que les réservations ne sont pas encore ouvertes",
    queue_long: "Attente trop longue dans la file, personne n'a répondu",
    e_bad_time: "L'heure n'est pas claire. Écrivez la date et l'heure, par ex. 1/12 08:00.",
    e_past_time: "Cette heure est passée. Choisissez-en une plus tard.",
    e_too_far: "Vous pouvez programmer jusqu'à {days} jours à l'avance.",
    e_need_line: "Un appel programmé a besoin du téléphone et de la SIM d'où appeler.",
    e_line_taken: "Cette ligne est déjà prise à cette heure par « {who} » ({when}). Choisissez une autre ligne ou heure.",
    e_line_reserved: "Cette ligne sert bientôt à un appel programmé ({who}, {when}). Choisissez-en une autre.",
    e_not_scheduled: "Cet appel a déjà commencé ou est fini : son heure ne peut plus changer.",
};

const DE: Words = Words {
    scheduled: "Geplant: Der Anruf startet zu dieser Zeit von selbst",
    started: "Es ist so weit: Anruf läuft",
    when: "Wann",
    line: "Ruft an von",
    line_auto: "{line} (gewählt: zu der Zeit frei)",
    grab: "Kommt der Anruf nicht durch, wählt es {gap} s später neu, bis jemand abnimmt, höchstens {min} Minuten",
    once: "Nur ein Anruf",
    status: "Geplant für {when}",
    missed: "Bageo lief zur geplanten Zeit nicht, der Anruf ging nicht raus",
    not_open: "Sie sagten, Reservierungen sind noch nicht offen",
    queue_long: "Zu lange in der Warteschleife, niemand kam ran",
    e_bad_time: "Die Zeit ist unklar. Schreib Datum und Uhrzeit, z. B. 1.12. 08:00.",
    e_past_time: "Diese Zeit ist vorbei. Wähl eine spätere.",
    e_too_far: "Du kannst bis zu {days} Tage im Voraus planen.",
    e_need_line: "Ein geplanter Anruf braucht Telefon und SIM, von denen er anruft.",
    e_line_taken: "Diese Leitung ist zu der Zeit schon für „{who}“ ({when}) verplant. Wähl eine andere Leitung oder Zeit.",
    e_line_reserved: "Diese Leitung ist gleich für einen geplanten Anruf verplant ({who}, {when}). Wähl eine andere.",
    e_not_scheduled: "Dieser Anruf hat schon begonnen oder ist vorbei, die Zeit lässt sich nicht mehr ändern.",
};

const PT: Words = Words {
    scheduled: "Agendada: a ligação sai sozinha nesse horário",
    started: "Chegou a hora: ligando agora",
    when: "Quando",
    line: "Liga do",
    line_auto: "{line} (escolhido: livre nesse horário)",
    grab: "Se não completar, liga de novo {gap} s depois até atenderem, por até {min} minutos",
    once: "Só uma ligação",
    status: "Agendada para {when}",
    missed: "O Bageo não estava rodando no horário marcado, então a ligação não foi feita",
    not_open: "Disseram que as reservas ainda não abriram",
    queue_long: "A espera na fila foi longa demais e ninguém atendeu",
    e_bad_time: "Não deu para entender o horário. Escreva a data e a hora, ex.: 1/12 08:00.",
    e_past_time: "Esse horário já passou. Escolha um mais tarde.",
    e_too_far: "Dá para agendar até {days} dias antes.",
    e_need_line: "Uma ligação agendada precisa do celular e do chip de onde ligar.",
    e_line_taken: "Essa linha já está reservada nesse horário para “{who}” ({when}). Escolha outra linha ou horário.",
    e_line_reserved: "Essa linha vai ser usada em breve numa ligação agendada ({who}, {when}). Escolha outra.",
    e_not_scheduled: "Esta ligação já começou ou terminou; não dá para mudar o horário.",
};

/// The owner's words for an error code of a scheduled errand (`{who}` and
/// `{when}` left for the caller); None for other codes.
pub fn error_text(lang: Lang, code: &str) -> Option<String> {
    let w = words(lang);
    let text = match code {
        "bad_time" => w.e_bad_time,
        "past_time" => w.e_past_time,
        "too_far" => w.e_too_far,
        "need_line" => w.e_need_line,
        "line_taken" => w.e_line_taken,
        "line_reserved" => w.e_line_reserved,
        "not_scheduled" => w.e_not_scheduled,
        _ => return None,
    };
    Some(text.replace("{days}", &MAX_DAYS.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d).unwrap().and_hms_opt(h, mi, 0).unwrap()
    }

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn grab_redials_after_the_gap_within_the_window_and_under_the_safety_cap() {
        let since = utc("2026-12-01T08:00:00Z");
        // The gap after the last try's end.
        let ended = utc("2026-12-01T08:00:20Z");
        assert_eq!(grab_next_at(&GRAB, since, &[since], ended), Some(ended + chrono::Duration::seconds(3)));
        assert_eq!(GRAB.gap, Duration::from_secs(3));
        assert_eq!(GRAB.lead, Duration::from_secs(2));
        assert_eq!(GRAB.window, Duration::from_secs(30 * 60));
        assert_eq!(GRAB.per_minute_cap, 10);
        // Ten dials within the last minute: the next waits for the minute to pass.
        let dials: Vec<DateTime<Utc>> = (0..10).map(|k| since + chrono::Duration::seconds(k * 4)).collect();
        let ended = since + chrono::Duration::seconds(40);
        assert_eq!(grab_next_at(&GRAB, since, &dials, ended), Some(since + chrono::Duration::seconds(60)));
        // Nine: not limited.
        assert_eq!(grab_next_at(&GRAB, since, &dials[..9], ended), Some(ended + chrono::Duration::seconds(3)));
        // Past the window: no more tries.
        let late = since + chrono::Duration::seconds(30 * 60 - 2);
        assert_eq!(grab_next_at(&GRAB, since, &[], late), None);
        assert!(grab_next_at(&GRAB, since, &[], since + chrono::Duration::seconds(30 * 60 - 4)).is_some());
        // Other numbers work the same (the rules are what the tuning changes).
        let slow = GrabRules { gap: Duration::from_secs(15), per_minute_cap: 3, window: Duration::from_secs(20 * 60), ..GRAB };
        let dials: Vec<DateTime<Utc>> = (0..3).map(|k| since + chrono::Duration::seconds(k * 10)).collect();
        assert_eq!(grab_next_at(&slow, since, &dials, since + chrono::Duration::seconds(25)), Some(since + chrono::Duration::seconds(60)));
    }

    #[test]
    fn what_each_way_a_try_ends_leads_to() {
        for c in [GrabCase::NoAnswer, GrabCase::EndedEarly, GrabCase::BusyMessage, GrabCase::Unreachable, GrabCase::Voicemail, GrabCase::NotOpenYet, GrabCase::QueueTooLong, GrabCase::PhoneTrouble] {
            assert!(dials_again(c), "{c:?}");
        }
        for c in [GrabCase::NotInService, GrabCase::KeypadMenu, GrabCase::PickedUp] {
            assert!(!dials_again(c), "{c:?}");
        }
    }

    #[test]
    fn a_schedule_holds_its_line_from_just_before_its_time_to_the_end_of_its_window() {
        let t = utc("2026-12-01T00:00:00Z");
        let (from, to) = hold(t, true);
        assert_eq!((from, to), (t - chrono::Duration::seconds(2), t + chrono::Duration::minutes(30)));
        assert_eq!(hold(t, false).1, t + chrono::Duration::minutes(12));
        assert!(overlaps(hold(t, true), hold(t + chrono::Duration::minutes(29), true)));
        assert!(!overlaps(hold(t, true), hold(t + chrono::Duration::minutes(31), true)));
        let now = utc("2026-11-28T00:00:00Z");
        assert_eq!(check_at(now, now), Err("past_time"));
        assert_eq!(check_at(now + chrono::Duration::days(61), now), Err("too_far"));
        assert_eq!(check_at(now + chrono::Duration::days(3), now), Ok(()));
        assert!(parse_at("2026-12-01T08:00").is_some());
        assert!(parse_at("2026-12-01T08:00:00+08:00").is_some());
        assert!(parse_at("tomorrow").is_none());
    }

    /// The chat's time for the call, in the nine languages (most not Chinese),
    /// with the year left out (the next one to come, across New Year too).
    #[test]
    fn a_call_time_is_read_from_chat_messages_in_the_nine_languages() {
        let now = at(2026, 11, 28, 10, 0);
        let dec1_8 = at(2026, 12, 1, 8, 0);
        for (lang, text, want) in [
            (Lang::ZhTw, "12/1 8點幫我打給鼎泰豐訂 4 位", dec1_8),
            (Lang::ZhTw, "12月1號早上八點打給 0223456789 訂位四位", dec1_8),
            (Lang::ZhCn, "12月1日晚上8点帮我打给 0223456789 订4位", at(2026, 12, 1, 20, 0)),
            (Lang::En, "On 12/1 at 8am call Joe's Diner and book a table for 4", dec1_8),
            (Lang::En, "Dec 1 8:00 pm call 0912345678 to book 4", at(2026, 12, 1, 20, 0)),
            (Lang::Ja, "12月1日の8時に鈴木食堂に電話して4名で予約して", dec1_8),
            (Lang::Ko, "12월 1일 오전 8시에 한강식당에 전화해서 4명 예약해 줘", dec1_8),
            (Lang::Ko, "12월 1일 오후 8시 반에 한강식당에 전화해 줘", at(2026, 12, 1, 20, 30)),
            (Lang::Es, "El 1/12 a las 8 llama al restaurante y reserva para 4", dec1_8),
            (Lang::Es, "1 de diciembre a las 20:30 llama a Casa Lucio", at(2026, 12, 1, 20, 30)),
            (Lang::Fr, "Le 1/12 à 8h appelle le bistrot pour réserver 4 couverts", dec1_8),
            (Lang::Fr, "1er décembre à 20h15, appelle Chez Paul", at(2026, 12, 1, 20, 15)),
            (Lang::De, "Am 1.12. um 8 Uhr ruf das Gasthaus an und reservier für 4", dec1_8),
            (Lang::De, "1. Dezember 20:00 Uhr ruf Müller an", at(2026, 12, 1, 20, 0)),
            (Lang::Pt, "Dia 1/12 às 8h liga para o restaurante e reserva para 4", dec1_8),
            (Lang::Pt, "1 de dezembro às 20h liga para a pizzaria", at(2026, 12, 1, 20, 0)),
        ] {
            assert_eq!(call_time_in(text, now, lang), Some(want), "{lang:?} {text}");
        }
        // Across New Year: the next 2 January is in 2027.
        let late = at(2026, 12, 28, 10, 0);
        for (lang, text) in [
            (Lang::ZhTw, "1/2 9點幫我打給 0223456789 訂位"),
            (Lang::En, "Jan 2 at 9am call 0912345678"),
            (Lang::Ja, "1月2日9時に予約センターに電話して"),
            (Lang::Fr, "Le 2/1 à 9h appelle la mairie"),
            (Lang::De, "Am 2.1. um 9 Uhr ruf die Praxis an"),
        ] {
            assert_eq!(call_time_in(text, late, lang), Some(at(2027, 1, 2, 9, 0)), "{lang:?} {text}");
        }
        // Relative days, and a time alone (today if still ahead, else tomorrow).
        assert_eq!(call_time_in("明天 8:00 打給 0223456789 訂位", now, Lang::ZhTw), Some(at(2026, 11, 29, 8, 0)));
        assert_eq!(call_time_in("Mañana a las 9 llama a la clínica", now, Lang::Es), Some(at(2026, 11, 29, 9, 0)));
        assert_eq!(call_time_in("Morgen um 9 Uhr ruf die Praxis an", now, Lang::De), Some(at(2026, 11, 29, 9, 0)));
        assert_eq!(call_time_in("Tomorrow 9am call the clinic", now, Lang::En), Some(at(2026, 11, 29, 9, 0)));
        assert_eq!(call_time_in("8點幫我打給 0223456789", now, Lang::ZhTw), Some(at(2026, 11, 29, 8, 0)));
        assert_eq!(call_time_in("晚上8點幫我打給 0223456789", now, Lang::ZhTw), Some(at(2026, 11, 28, 20, 0)));
        // A time after the words that ask for the call is the task's: call now.
        for (lang, text) in [
            (Lang::ZhTw, "幫我打給鼎泰豐訂明天晚上七點四位"),
            (Lang::En, "Call 0912345678 and book a table for 4 at 7 pm on Dec 1"),
            (Lang::Es, "Llama al restaurante y reserva el 1/12 a las 21"),
            (Lang::Ja, "鈴木食堂に予約の電話"),
        ] {
            assert_eq!(call_time_in(text, now, lang), None, "{lang:?} {text}");
        }
    }

    #[test]
    fn a_chat_command_can_start_with_the_date_and_time() {
        let now = at(2026, 11, 28, 10, 0);
        assert_eq!(leading_when("12/1 08:00 0912345678 訂4位"), Some(("12/1 08:00", "0912345678 訂4位")));
        assert_eq!(leading_when("1/12 20h Chez Paul réserver"), Some(("1/12 20h", "Chez Paul réserver")));
        assert_eq!(leading_when("2026-12-01 8:30 王小明 約吃飯"), Some(("2026-12-01 8:30", "王小明 約吃飯")));
        assert_eq!(leading_when("0912345678 訂明晚 7 點"), None);
        assert_eq!(leading_when("王小明 約週六"), None);
        assert_eq!(leading_when("2 people at 7"), None);
        assert_eq!(resolve_when("12/1 08:00", now, Lang::ZhTw), Some(at(2026, 12, 1, 8, 0)));
        assert_eq!(resolve_when("1/12 20h", now, Lang::Fr), Some(at(2026, 12, 1, 20, 0)));
        assert_eq!(resolve_when("2027-01-02T09:00", now, Lang::De), Some(at(2027, 1, 2, 9, 0)));
        assert_eq!(resolve_when("whenever", now, Lang::En), None);
    }

    #[test]
    fn scheduled_times_read_with_their_weekday_in_each_language() {
        let now = at(2026, 11, 28, 10, 0);
        let t = at(2026, 12, 1, 8, 0);
        assert_eq!(when_label(Lang::ZhTw, t, now), "12/1（週二）08:00");
        assert_eq!(when_label(Lang::ZhCn, t, now), "12月1日（周二）08:00");
        assert_eq!(when_label(Lang::En, t, now), "Tue, Dec 1, 08:00");
        assert_eq!(when_label(Lang::Ja, t, now), "12月1日（火）08:00");
        assert_eq!(when_label(Lang::Ko, t, now), "12월 1일 (화) 08:00");
        assert_eq!(when_label(Lang::Es, t, now), "mar 1 dic, 08:00");
        assert_eq!(when_label(Lang::Fr, t, now), "mar. 1 déc., 08:00");
        assert_eq!(when_label(Lang::De, t, now), "Di., 1.12., 08:00");
        assert_eq!(when_label(Lang::Pt, t, now), "ter, 1 dez, 08:00");
        assert_eq!(when_label(Lang::ZhTw, at(2027, 1, 2, 9, 0), now), "2027/1/2（週六）09:00");
        for lang in [Lang::ZhTw, Lang::ZhCn, Lang::En, Lang::Ja, Lang::Ko, Lang::Es, Lang::Fr, Lang::De, Lang::Pt] {
            let g = fill_grab(words(lang).grab);
            assert!(g.contains('3') && g.contains("30") && !g.contains('{'), "{lang:?} {g}");
            assert!(error_text(lang, "line_taken").is_some_and(|s| s.contains("{who}") && s.contains("{when}")), "{lang:?}");
        }
        assert_eq!(error_text(Lang::En, "busy"), None);
    }
}
