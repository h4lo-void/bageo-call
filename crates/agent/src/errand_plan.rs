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
    /// of its time). Once a person is on the line, no more tries. Each
    /// errand sets its own ([GrabRules::for_minutes]); this is the default.
    pub window: Duration,
    /// Not a rule, a guard against a loop gone wrong: at most this many dials
    /// in any one minute.
    pub per_minute_cap: usize,
    /// A queue's recording ("please hold", music, "you are number 3"): the AI
    /// stays on the line this long for a person (never past the window).
    pub queue_wait: Duration,
    /// The phone is on another call: looked at again this soon.
    pub phone_retry: Duration,
}

pub const GRAB: GrabRules = GrabRules {
    lead: Duration::from_secs(2),
    gap: Duration::from_secs(3),
    window: Duration::from_secs(30 * 60),
    per_minute_cap: 10,
    queue_wait: Duration::from_secs(30 * 60),
    phone_retry: Duration::from_secs(1),
};

/// How many minutes an errand keeps redialing when the owner sets none.
pub const GRAB_MIN_DEFAULT: u32 = 30;
/// The fewest and most minutes the owner may set.
pub const GRAB_MIN_LOW: u32 = 1;
pub const GRAB_MIN_HIGH: u32 = 120;

/// Minutes the owner may set ([GRAB_MIN_LOW] to [GRAB_MIN_HIGH]).
pub fn grab_min_ok(min: u64) -> bool {
    (GRAB_MIN_LOW as u64..=GRAB_MIN_HIGH as u64).contains(&min)
}

/// A window of `min` minutes.
pub fn grab_window(min: u32) -> Duration {
    Duration::from_secs(min as u64 * 60)
}

impl GrabRules {
    /// These rules with a window of `min` minutes (one errand's own).
    pub fn for_minutes(self, min: u32) -> GrabRules {
        GrabRules { window: grab_window(min), ..self }
    }
}

/// How a try ended, as grab mode tells them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrabCase {
    /// It rang out or was declined after ringing (a ringing call is given up
    /// after about 70 seconds; voicemail usually answers before that).
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

/// The earliest a new dial may go out under the per-minute cap (`dials`: the
/// latest dials to the number); None: the cap leaves room now.
pub fn cap_free_at(rules: &GrabRules, dials: &[DateTime<Utc>], now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if rules.per_minute_cap == 0 || dials.len() < rules.per_minute_cap {
        return None;
    }
    let mut sorted = dials.to_vec();
    sorted.sort();
    let at = sorted[sorted.len() - rules.per_minute_cap] + chrono::Duration::seconds(60);
    (at > now).then_some(at)
}

/// How long the AI may stay in a queue on a try of a run that started at
/// `since`: [GrabRules::queue_wait], but never past the run's window.
pub fn queue_wait_left(rules: &GrabRules, since: DateTime<Utc>, now: DateTime<Utc>) -> Duration {
    let left = (since + span(rules.window) - now).to_std().unwrap_or(Duration::ZERO);
    left.min(rules.queue_wait)
}

// ---------------------------------------------------------------- scheduled errands

/// From when to when (the end not included).
pub type Span = (DateTime<Utc>, DateTime<Utc>);

/// How far ahead an errand may be scheduled.
pub const MAX_DAYS: i64 = 60;
/// At most this many errands not over yet (scheduled ones included).
pub const OPEN_MAX: usize = 50;
/// A scheduled errand without grab mode holds its line this long from its
/// time (a call of up to 10 minutes and its ringing).
pub const PLAIN_HOLD: Duration = Duration::from_secs(12 * 60);
/// A scheduled errand without grab mode that Bageo could not start on time
/// (it was not running) still goes out this long after its time.
pub const LATE_MAX: Duration = Duration::from_secs(5 * 60);

/// The line a scheduled errand holds: from its first dial ([GrabRules::lead]
/// before its time) to the end of its window (`grab`: grab mode for that many
/// minutes) or of one call (None).
pub fn hold(at: DateTime<Utc>, grab: Option<u32>) -> (DateTime<Utc>, DateTime<Utc>) {
    (at - span(GRAB.lead), at + span(grab.map_or(PLAIN_HOLD, grab_window)))
}

/// Until when a scheduled errand may still start once its time has passed
/// (`grab` as for [hold]).
pub fn start_by(at: DateTime<Utc>, grab: Option<u32>) -> DateTime<Utc> {
    at + span(grab.map_or(LATE_MAX, grab_window))
}

/// Bageo's time zone as an IANA name ("Asia/Taipei"): `TZ` when it names
/// one, else the system's; empty when neither can be read.
pub fn zone_name() -> String {
    let from_env = std::env::var("TZ").ok().map(|z| z.trim().trim_start_matches(':').to_string()).filter(|z| z.contains('/'));
    from_env.or_else(|| iana_time_zone::get_timezone().ok()).unwrap_or_default()
}

/// Bageo's offset from UTC now, in minutes (+480 for UTC+8).
pub fn utc_offset_min() -> i32 {
    Local::now().offset().local_minus_utc() / 60
}

/// A local wall-clock time in the owner's zone (the earlier one when the
/// clock goes back; None when it is skipped).
pub fn local_of(n: NaiveDateTime) -> Option<DateTime<Local>> {
    Local.from_local_datetime(&n).earliest()
}

/// A time as the API takes it: RFC 3339, or local `YYYY-MM-DDTHH:MM[:SS]`.
/// Err `bad_time` (unreadable) or `time_skipped` (a local time the clock
/// skips when it goes forward for summer time).
pub fn parse_at(s: &str) -> Result<DateTime<Local>, &'static str> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&Local));
    }
    let n = ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"]
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(s, f).ok())
        .ok_or("bad_time")?;
    local_of(n).ok_or("time_skipped")
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

/// Two holds ([hold]) share a moment beyond the [GrabRules::lead] one of
/// them dials early with: a schedule right where another's window ends
/// does not clash with it.
pub fn overlaps(a: Span, b: Span) -> bool {
    let lead = span(GRAB.lead);
    a.0 + lead < b.1 && b.0 + lead < a.1
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

/// Weekdays and "next …": a time named so is the chat model's to read, not ours.
const WEEKDAY_WORDS: &[&str] = &[
    "週一", "週二", "週三", "週四", "週五", "週六", "週日", "週天", "周一", "周二", "周三", "周四", "周五", "周六", "周日", "周天", "星期", "禮拜", "礼拜", "下週",
    "下周", "曜日", "曜", "来週", "요일", "다음 주", "다음주", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday", "mon", "tue",
    "wed", "thu", "fri", "sat", "sun", "next", "lunes", "martes", "miércoles", "miercoles", "jueves", "viernes", "sábado", "sabado", "domingo",
    "próximo", "proximo", "próxima", "proxima", "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche", "prochain", "prochaine",
    "montag", "dienstag", "mittwoch", "donnerstag", "freitag", "samstag", "sonntag", "nächste", "nächsten", "nächstes", "naechste", "segunda",
    "terça", "terca", "quarta", "quinta", "sexta",
];
/// A booking named before the words asking for the call: its time is the booking's.
const BOOKING_WORDS: &[&str] = &["訂", "订", "予約", "예약", "book", "reserv", "réserv", "agend", "marcar", "termin"];

/// When the owner wants the call placed, read from their message only when
/// that is plain: a full date and time ("12/1 8點", "Dec 1 at 8 am", "le
/// 1/12 à 20h") before the words that ask for the call, no weekday or "next",
/// no booking named before them ("book a table at 7 pm tomorrow, call …").
/// Anything less clear is None: the chat model's reading stands.
pub fn call_time_in(text: &str, now: NaiveDateTime, owner: Lang) -> Option<NaiveDateTime> {
    // A time with a zone is the chat model's to read (as `YYYY-MM-DDTHH:MM+08:00`).
    if has_zone(text) {
        return None;
    }
    let t = normalize(text);
    let cut = CALL_WORDS.iter().filter_map(|w| find_word(&t, w)).min()?;
    let head: String = t[..cut].iter().collect();
    if head.trim().is_empty() {
        return None;
    }
    // The weekday and booking words as written (normalize turns 週一 into 週1).
    let raw: Vec<char> = text.to_lowercase().chars().collect();
    let raw_cut = CALL_WORDS.iter().filter_map(|w| find_word(&raw, w)).min().unwrap_or(raw.len());
    let raw_head: String = raw[..raw_cut].iter().collect();
    let raw_head_chars: Vec<char> = raw_head.chars().collect();
    if has_word(&raw_head_chars, WEEKDAY_WORDS) || BOOKING_WORDS.iter().any(|w| raw_head.contains(w)) {
        return None;
    }
    let first = day_first(text, owner);
    let head_chars: Vec<char> = head.chars().collect();
    // A date written out, not "tomorrow".
    find_date(&head_chars, &numbers(&head_chars), first)?;
    parse_when(&head, now, first)
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
        // A zone after the time ("UTC", "+0800", "(UTC)", "Z", "Asia/Taipei") stays with it, so the
        // time is refused, never read as local.
        || zone_token(&fold_wide(tok))
}

/// A word that names a zone on its own, brackets and a trailing comma or
/// full stop aside: a zone name ([zone_word]), `Z`, an IANA name
/// ("Asia/Taipei"), an offset with a colon ("+08:00") or one alone ([offset_token]).
fn zone_token(word: &str) -> bool {
    let bare = word.trim_end_matches([',', '.', ';']).trim_matches(['(', ')', '[', ']']).trim_end_matches([',', '.', ';']);
    !bare.is_empty() && (zone_word(bare) || bare.eq_ignore_ascii_case("z") || iana_zone(bare) || colon_offset(bare) || offset_token(bare))
}

/// A zone named as "Area/City" ("Asia/Taipei", "America/New_York").
fn iana_zone(word: &str) -> bool {
    let parts: Vec<&str> = word.split('/').collect();
    parts.len() >= 2
        && parts.iter().all(|p| {
            p.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+'))
        })
        && parts.iter().any(|p| p.chars().any(|c| c.is_ascii_lowercase()))
}

/// An offset with a colon: `+8:00`, `+08:00`, `-05:30`.
fn colon_offset(word: &str) -> bool {
    let Some(d) = word.strip_prefix(['+', '-', '\u{2212}']) else {
        return false;
    };
    let Some((h, m)) = d.split_once(':') else {
        return false;
    };
    (1..=2).contains(&h.len()) && m.len() == 2 && h.chars().chain(m.chars()).all(|c| c.is_ascii_digit())
}

/// Full-width letters, digits and signs ("８：００", "ＵＴＣ") as their ASCII
/// ones, and the ideographic space as a space.
fn fold_wide(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            '\u{3000}' => ' ',
            _ => c,
        })
        .collect()
}

/// An offset written alone: `+H`, `+HH`, `+HMM` or `+HHMM` (`-` too: "+8",
/// "+08", "+530", "-0500"), hours up to 14, minutes 00, 30 or 45. A country
/// code ("+1", "+7", "+886", "+44", "+353") is none, so a number written after
/// the time stays the number.
fn offset_token(tok: &str) -> bool {
    let (sign, d) = match tok.chars().next() {
        Some(c @ ('+' | '-' | '\u{2212}')) => (c, &tok[c.len_utf8()..]),
        _ => return false,
    };
    if !(1..=4).contains(&d.len()) || !d.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    // "+1" and "+7" start numbers in North America and Russia.
    if d.len() == 1 && sign == '+' && matches!(d, "1" | "7") {
        return false;
    }
    let (h, m) = d.split_at(d.len() - if d.len() <= 2 { 0 } else { 2 });
    let (h, m): (u32, u32) = (h.parse().unwrap_or(99), m.parse().unwrap_or(0));
    h <= 14 && matches!(m, 0 | 30 | 45)
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
/// model writes; with a zone, `Z` or `+08:00`, that moment in the owner's
/// local time), else a date and time as the owner wrote them ([parse_when]).
/// Any other time with a zone ("12/1 08:00Z", "… 08:00+08:00", "… 08:00
/// UTC") is no time at all: the zone is never dropped.
pub fn resolve_when(when: &str, now: NaiveDateTime, owner: Lang) -> Option<NaiveDateTime> {
    let w = when.trim();
    let zoned = w.strip_suffix(['Z', 'z']).map(|s| format!("{s}+00:00")).unwrap_or_else(|| w.to_string());
    if let Some(t) = DateTime::parse_from_rfc3339(&zoned).ok().or_else(|| DateTime::parse_from_str(&zoned, "%Y-%m-%dT%H:%M%:z").ok()) {
        return Some(t.with_timezone(&Local).naive_local());
    }
    if has_zone(w) {
        return None;
    }
    ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"]
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(w, f).ok())
        .or_else(|| parse_when(w, now, day_first(w, owner)))
}

/// Zone names a time may be followed by ("08:00 UTC", "8 pm EST"). UTC and
/// GMT in any case; the others only in capitals (French "est" is a word).
const ZONE_NAMES: &[&str] = &[
    "UTC", "GMT", "UT", "WET", "WEST", "CET", "CEST", "EET", "EEST", "BST", "IST", "MSK", "EST", "EDT", "CST", "CDT", "MST", "MDT", "PST", "PDT",
    "AKST", "AKDT", "HST", "AST", "ADT", "NST", "NDT", "BRT", "ART", "JST", "KST", "HKT", "SGT", "PHT", "WIB", "ICT", "AEST", "AEDT", "ACST",
    "ACDT", "AWST", "NZST", "NZDT",
];

/// `word` names a zone: one of [ZONE_NAMES], or one with an offset after it ("UTC+8").
fn zone_word(word: &str) -> bool {
    let name: String = word.chars().take_while(char::is_ascii_alphabetic).collect();
    let rest = &word[name.len()..];
    if !(rest.is_empty() || rest.starts_with(['+', '-'])) {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    ["utc", "gmt", "mez", "mesz", "pacific", "eastern"].contains(&lower.as_str()) || ZONE_NAMES.contains(&name.as_str())
}

/// Whether any word of `text` names a zone, wherever it stands ("UTC 12/1
/// 08:00", "12/1 08:00 (UTC)", "… Asia/Taipei"): a zone name, `Z`, an IANA
/// name or an offset with a colon; an offset alone only in brackets ("(+0800)").
fn mentions_zone(text: &str) -> bool {
    text.split(char::is_whitespace).any(|raw| {
        let bare = raw.trim_end_matches([',', '.', ';']).trim_matches(['(', ')', '[', ']']).trim_end_matches([',', '.', ';']);
        let bracketed = bare.len() != raw.trim_end_matches([',', '.', ';']).len();
        !bare.is_empty()
            && (zone_word(bare) || bare.eq_ignore_ascii_case("z") || iana_zone(bare) || colon_offset(bare) || (bracketed && offset_token(bare)))
    })
}

/// The words that end a time as [find_time] reads it ("8 pm", "8點半",
/// "8시 30분", "8 o'clock", "20 heures"), longest first.
const TIME_ENDS: &[&str] = &[
    "o'clock", "o\u{2019}clock", "oclock", "heures", "heure", "horas", "hora", "a.m.", "p.m.", "uhr", "am", "pm", "h", "點", "点", "時", "时", "시", "分鐘",
    "分钟", "分", "半", "분", "반",
];
/// Words a bare hour follows ("at 8", "à 20", "a las 8", "às 8", "um 8", "alle 8").
const TIME_BEFORE: &[&str] = &["at", "à", "às", "as", "las", "la", "um", "alle"];

/// The [TIME_ENDS] word at `i` in `t` (lower case), and its length; a Latin
/// one only when no letter follows it.
fn time_end_at(t: &[char], i: usize) -> Option<usize> {
    let ahead: String = t[i.min(t.len())..t.len().min(i + 8)].iter().collect::<String>().to_lowercase();
    TIME_ENDS.iter().find(|w| ahead.starts_with(**w)).map(|w| w.chars().count()).filter(|n| {
        let cjk = t.get(i).is_some_and(|c| !c.is_ascii());
        cjk || !letter(t.get(i + n))
    })
}

/// The word just before `i` in `t`, lower case ("at" in "at 8").
fn word_before(t: &[char], i: usize) -> String {
    let mut b = i;
    while b > 0 && t[b - 1].is_whitespace() {
        b -= 1;
    }
    let mut a = b;
    while a > 0 && t[a - 1].is_alphabetic() && !is_cjk(t[a - 1]) {
        a -= 1;
    }
    t[a..b].iter().collect::<String>().to_lowercase()
}

/// Whether a time in `text` is followed by a zone: `Z`, an offset (`+08:00`,
/// `-5`) or a zone name ([ZONE_NAMES]). A time, as [find_time] reads it:
/// digits after `:` or `h` ("08:00", "20h15"), an hour with the words that end
/// it (am/pm, h, Uhr, o'clock, heures, horas, 點, 點半, 點30分, 시 30분: [TIME_ENDS])
/// or after the words a bare hour follows ("at 8", "a las 8": [TIME_BEFORE]).
pub fn has_zone(text: &str) -> bool {
    let text = fold_wide(text);
    if mentions_zone(&text) {
        return true;
    }
    let t: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < t.len() {
        if !t[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < t.len() && t[i].is_ascii_digit() {
            i += 1;
        }
        let mut is_time = start > 1 && (t[start - 1] == ':' || (t[start - 1].eq_ignore_ascii_case(&'h') && t[start - 2].is_ascii_digit()));
        is_time |= TIME_BEFORE.contains(&word_before(&t, start).as_str());
        // The words ending the time, and the minutes after a CJK hour ("8點30分", "8시 30분").
        let mut end = i;
        loop {
            let s = skip_spaces(&t, end);
            if let Some(n) = time_end_at(&t, s) {
                end = s + n;
                is_time = true;
                continue;
            }
            let after_cjk_unit = end > 0 && is_cjk(t[end - 1]);
            if after_cjk_unit && t.get(s).is_some_and(|c| c.is_ascii_digit()) {
                let mut k = s;
                while k < t.len() && t[k].is_ascii_digit() {
                    k += 1;
                }
                if time_end_at(&t, skip_spaces(&t, k)).is_some() {
                    end = k;
                    continue;
                }
            }
            break;
        }
        if !is_time {
            continue;
        }
        let s = skip_spaces(&t, end);
        let next = t.get(s).copied();
        let glued = s == end;
        if (matches!(next, Some('Z')) || (glued && matches!(next, Some('z')))) && !letter(t.get(s + 1)) {
            return true;
        }
        if matches!(next, Some('+' | '-' | '\u{2212}')) && t.get(s + 1).is_some_and(|c| c.is_ascii_digit()) {
            return true;
        }
        let word: String = t[s..].iter().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | ':')).collect();
        if !word.is_empty() && zone_word(&word) {
            return true;
        }
        i = i.max(end);
    }
    false
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
/// in seconds and the errand's window in minutes; `{who}`: the errand holding
/// a line; `{low}`, `{high}`: the minutes the owner may set.
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
    /// Another errand redials the same number then.
    pub e_number_taken: &'static str,
    /// Too many errands not over yet (`{max}`).
    pub e_too_many_open: &'static str,
    /// A local time the clock skips for summer time.
    pub e_time_skipped: &'static str,
    /// Minutes of redialing out of range or not a whole number (`{low}`, `{high}`).
    pub e_bad_grab_min: &'static str,
    /// Minutes of redialing given for a call now (only a scheduled call from
    /// a chat or MCP redials).
    pub e_grab_needs_time: &'static str,
    /// Why a scheduled errand is cancelled: a backup was restored.
    pub restored: &'static str,
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

/// `template` with `{gap}` from [GRAB] and `{min}`: the errand's minutes.
pub fn fill_grab(template: &str, min: u32) -> String {
    template.replace("{gap}", &GRAB.gap.as_secs().to_string()).replace("{min}", &min.to_string())
}

/// The lines a chat's confirmation card shows for a scheduled call: when
/// (`label`, a [when_label]), from which line (`line`, picked by Bageo), and
/// that it redials for `min` minutes.
pub fn card_lines(lang: Lang, label: &str, line: &str, min: u32) -> String {
    let w = words(lang);
    let sep = crate::locale::words(lang).sep.as_str();
    let line = w.line_auto.replace("{line}", line);
    format!("{}{sep}{label}\n{}{sep}{line}\n{}", w.when, w.line, fill_grab(w.grab, min))
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
    e_line_taken: "這支手機那段時間已經被「{who}」（{when}）排走了，請換一支或換時間。",
    e_line_reserved: "這支手機接下來要給排定的代打用（{who}，{when}），請換一支。",
    e_not_scheduled: "這筆已經開始或結束，不能再改時間。",
    e_number_taken: "這個號碼那段時間已經有「{who}」（{when}）在撥到通，請換個時間。",
    e_too_many_open: "還沒結束的代打已經有 {max} 筆，請先取消或等幾筆結束。",
    e_time_skipped: "這個時間在夏令時間調整中不存在，請換一個時間。",
    e_bad_grab_min: "一直重撥的時間請設 {low} 到 {high} 分鐘的整數。",
    e_grab_needs_time: "只有排定時間的代打會一直重撥，請一起給撥打時間。",
    restored: "還原備份後，這個排程已取消",
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
    e_line_taken: "这部手机那段时间已经被“{who}”（{when}）占用了，请换一部或换时间。",
    e_line_reserved: "这部手机接下来要给定时代打用（{who}，{when}），请换一部。",
    e_not_scheduled: "这条已经开始或结束，不能再改时间。",
    e_number_taken: "这个号码那段时间已经有“{who}”（{when}）在拨到通，请换个时间。",
    e_too_many_open: "还没结束的代打已经有 {max} 条，请先取消或等几条结束。",
    e_time_skipped: "这个时间在夏令时调整中不存在，请换一个时间。",
    e_bad_grab_min: "一直重拨的时间请设 {low} 到 {high} 分钟的整数。",
    e_grab_needs_time: "只有定时代打会一直重拨，请同时给出拨打时间。",
    restored: "恢复备份后，这个安排已取消",
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
    e_line_taken: "That phone is already booked then by “{who}” ({when}). Pick another phone or time.",
    e_line_reserved: "That phone is booked soon for a scheduled call ({who}, {when}). Pick another phone.",
    e_not_scheduled: "This call has already started or ended, so its time can't change.",
    e_number_taken: "“{who}” ({when}) is already redialing that number then. Pick another time.",
    e_too_many_open: "{max} AI calls aren't over yet. Cancel some or wait for them to end.",
    e_time_skipped: "That time doesn't exist: the clock skips it for summer time. Pick another time.",
    e_bad_grab_min: "Set how long to keep redialing as a whole number from {low} to {high} minutes.",
    e_grab_needs_time: "Only a scheduled call keeps redialing. Give the time to call too.",
    restored: "Cancelled when a backup was restored",
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
    e_line_taken: "そのスマホはその時間「{who}」（{when}）で予約済みです。別のスマホか時間を選んでください。",
    e_line_reserved: "そのスマホはこのあと予約発信で使います（{who}、{when}）。別のスマホを選んでください。",
    e_not_scheduled: "この発信はもう始まったか終わったため、時刻を変えられません。",
    e_number_taken: "その番号にはその時間「{who}」（{when}）がかけ直し中です。別の時間を選んでください。",
    e_too_many_open: "終わっていない代理電話が {max} 件あります。いくつか取り消すか、終わるまでお待ちください。",
    e_time_skipped: "その時刻はサマータイムの切り替えで存在しません。別の時刻を選んでください。",
    e_bad_grab_min: "かけ直し続ける時間は {low}〜{high} 分の整数で指定してください。",
    e_grab_needs_time: "かけ直し続けるのは予約発信だけです。発信日時も指定してください。",
    restored: "バックアップを復元したため、この予約は取り消されました",
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
    e_line_taken: "그 휴대폰은 그 시간에 ‘{who}’({when}) 예약이 있어요. 다른 휴대폰이나 시간을 골라 주세요.",
    e_line_reserved: "그 휴대폰은 곧 예약 전화에 쓰여요({who}, {when}). 다른 휴대폰을 골라 주세요.",
    e_not_scheduled: "이미 시작했거나 끝난 전화라 시간을 바꿀 수 없어요.",
    e_number_taken: "그 번호는 그 시간에 ‘{who}’({when})이(가) 다시 걸고 있어요. 다른 시간을 골라 주세요.",
    e_too_many_open: "끝나지 않은 AI 대리 전화가 {max}건이에요. 몇 건을 취소하거나 끝날 때까지 기다려 주세요.",
    e_time_skipped: "그 시간은 서머타임 조정으로 존재하지 않아요. 다른 시간을 골라 주세요.",
    e_bad_grab_min: "다시 거는 시간은 {low}~{high}분 사이의 정수로 정해 주세요.",
    e_grab_needs_time: "예약 전화만 계속 다시 겁니다. 걸 시간도 함께 알려 주세요.",
    restored: "백업을 복원해서 이 예약은 취소됐어요",
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
    e_line_taken: "Ese teléfono ya está reservado a esa hora para «{who}» ({when}). Elige otro teléfono u otra hora.",
    e_line_reserved: "Ese teléfono se usará pronto para una llamada programada ({who}, {when}). Elige otro.",
    e_not_scheduled: "Esta llamada ya empezó o terminó; no se puede cambiar la hora.",
    e_number_taken: "«{who}» ({when}) ya está llamando a ese número a esa hora. Elige otra hora.",
    e_too_many_open: "Hay {max} llamadas de la IA sin terminar. Cancela alguna o espera a que terminen.",
    e_time_skipped: "Esa hora no existe: el reloj se la salta por el horario de verano. Elige otra.",
    e_bad_grab_min: "Indica cuánto tiempo seguir rellamando: un número entero de {low} a {high} minutos.",
    e_grab_needs_time: "Solo una llamada programada sigue rellamando. Indica también la hora.",
    restored: "Cancelada al restaurar una copia de seguridad",
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
    e_line_taken: "Ce téléphone est déjà pris à cette heure par « {who} » ({when}). Choisissez un autre téléphone ou une autre heure.",
    e_line_reserved: "Ce téléphone sert bientôt à un appel programmé ({who}, {when}). Choisissez-en un autre.",
    e_not_scheduled: "Cet appel a déjà commencé ou est fini : son heure ne peut plus changer.",
    e_number_taken: "« {who} » ({when}) rappelle déjà ce numéro à cette heure. Choisissez une autre heure.",
    e_too_many_open: "{max} appels de l'IA ne sont pas terminés. Annulez-en ou attendez qu'ils finissent.",
    e_time_skipped: "Cette heure n'existe pas : l'horloge la saute au passage à l'heure d'été. Choisissez-en une autre.",
    e_bad_grab_min: "Indiquez combien de temps rappeler : un nombre entier de {low} à {high} minutes.",
    e_grab_needs_time: "Seul un appel programmé rappelle en boucle. Indiquez aussi l'heure.",
    restored: "Annulé lors de la restauration d'une sauvegarde",
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
    e_line_taken: "Dieses Telefon ist zu der Zeit schon für „{who}“ ({when}) verplant. Wähl ein anderes Telefon oder eine andere Zeit.",
    e_line_reserved: "Dieses Telefon ist gleich für einen geplanten Anruf verplant ({who}, {when}). Wähl ein anderes.",
    e_not_scheduled: "Dieser Anruf hat schon begonnen oder ist vorbei, die Zeit lässt sich nicht mehr ändern.",
    e_number_taken: "„{who}“ ({when}) ruft diese Nummer zu der Zeit schon wiederholt an. Wähl eine andere Zeit.",
    e_too_many_open: "{max} KI-Anrufe sind noch nicht vorbei. Brich welche ab oder warte, bis sie fertig sind.",
    e_time_skipped: "Diese Uhrzeit gibt es nicht: Die Uhr springt bei der Sommerzeit darüber. Wähl eine andere.",
    e_bad_grab_min: "Gib an, wie lange neu gewählt wird: eine ganze Zahl von {low} bis {high} Minuten.",
    e_grab_needs_time: "Nur ein geplanter Anruf wählt immer wieder neu. Gib auch die Uhrzeit an.",
    restored: "Beim Wiederherstellen eines Backups abgebrochen",
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
    e_line_taken: "Esse celular já está reservado nesse horário para “{who}” ({when}). Escolha outro celular ou horário.",
    e_line_reserved: "Esse celular vai ser usado em breve numa ligação agendada ({who}, {when}). Escolha outro.",
    e_not_scheduled: "Esta ligação já começou ou terminou; não dá para mudar o horário.",
    e_number_taken: "“{who}” ({when}) já está ligando para esse número nesse horário. Escolha outro horário.",
    e_too_many_open: "Há {max} ligações da IA sem terminar. Cancele algumas ou espere terminarem.",
    e_time_skipped: "Esse horário não existe: o relógio pula ele no horário de verão. Escolha outro.",
    e_bad_grab_min: "Diga por quanto tempo continuar ligando: um número inteiro de {low} a {high} minutos.",
    e_grab_needs_time: "Só uma ligação agendada continua ligando. Informe também o horário.",
    restored: "Cancelada ao restaurar um backup",
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
        "number_taken" => w.e_number_taken,
        "too_many_open" => w.e_too_many_open,
        "time_skipped" => w.e_time_skipped,
        "bad_grab_min" => w.e_bad_grab_min,
        "grab_needs_time" => w.e_grab_needs_time,
        _ => return None,
    };
    Some(
        text.replace("{days}", &MAX_DAYS.to_string())
            .replace("{max}", &OPEN_MAX.to_string())
            .replace("{low}", &GRAB_MIN_LOW.to_string())
            .replace("{high}", &GRAB_MIN_HIGH.to_string()),
    )
}

/// Minutes of redialing an owner's chat message asks for ("打 10 分鐘", "10
/// 分鐘內一直打", "keep calling for 45 minutes", "10分間かけ続けて", "10분
/// 동안 계속 걸어", "durante 20 minutos", "pendant 20 minutes", "20 Minuten
/// lang", "por 20 minutos"), as written: the caller checks the range. None:
/// it names none (a time to call is no length of redialing).
pub fn grab_minutes_in(text: &str) -> Option<u32> {
    let t = normalize(text);
    let dial = |c: Option<&char>| c.is_some_and(|c| matches!(c, '打' | '撥' | '拨'));
    for n in numbers(&t) {
        let after = skip_spaces(&t, n.end);
        let mut before = n.start;
        while before > 0 && t[before - 1].is_whitespace() {
            before -= 1;
        }
        // 打 10 分鐘 / 重撥10分钟 / 10 分鐘內一直打
        if let Some(unit) = ["分鐘", "分钟"].iter().find(|u| at_str(&t, after, u)) {
            if before > 0 && dial(t.get(before - 1)) {
                return Some(n.val);
            }
            let rest = after + unit.chars().count();
            if matches!(t.get(rest), Some('內' | '内')) && (rest + 1..(rest + 6).min(t.len())).any(|i| dial(t.get(i))) {
                return Some(n.val);
            }
        }
        let near = |from: usize, words: &[&str]| (from..(from + 10).min(t.len())).any(|i| words.iter().any(|w| at_str(&t, i, w)));
        // 10分間かけ続けて
        if at_str(&t, after, "分間") && near(after + 2, &["かけ", "電話", "発信"]) {
            return Some(n.val);
        }
        // 10분 동안 계속 걸어
        if at_str(&t, after, "분") {
            let d = skip_spaces(&t, after + 1);
            if at_str(&t, d, "동안") && near(d + 2, &["걸", "전화"]) {
                return Some(n.val);
            }
        }
    }
    // The Latin-script languages: "for 45 minutes" with a word asking for the call shortly before.
    let s: String = t.iter().collect();
    let words: Vec<&str> = s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    const UNITS: &[&str] = &["minute", "minutes", "min", "mins", "minuto", "minutos", "minuten"];
    const BEFORE: &[&str] = &["for", "durante", "pendant", "por", "für"];
    const CALLING: &[&str] = &[
        "call", "calling", "redial", "redialing", "redialling", "dial", "dialing", "dialling", "ring", "ringing", "try", "trying", "llama", "llamando",
        "llamar", "intenta", "intentando", "insiste", "insistiendo", "marca", "marcando", "appelle", "appeler", "rappelle", "rappeler", "essaie",
        "essayer", "insistez", "ruf", "rufe", "anrufen", "wähl", "wähle", "wählen", "versuch", "versuche", "probier", "liga", "ligando",
        "ligar", "tenta", "tentando", "disca", "discando",
    ];
    for (i, w) in words.iter().enumerate() {
        if !w.chars().all(|c| c.is_ascii_digit()) || !words.get(i + 1).is_some_and(|u| UNITS.contains(u)) {
            continue;
        }
        let prep = i > 0 && BEFORE.contains(&words[i - 1]);
        let lang = words.get(i + 2).is_some_and(|l| *l == "lang");
        if !(prep || lang) {
            continue;
        }
        let from = i.saturating_sub(if prep { 6 } else { 5 });
        if words[from..i].iter().any(|x| CALLING.contains(x)) {
            return Some(w.parse().unwrap_or(u32::MAX));
        }
    }
    None
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
        let (from, to) = hold(t, Some(30));
        assert_eq!((from, to), (t - chrono::Duration::seconds(2), t + chrono::Duration::minutes(30)));
        assert_eq!(hold(t, None).1, t + chrono::Duration::minutes(12));
        assert!(overlaps(hold(t, Some(30)), hold(t + chrono::Duration::minutes(29), Some(30))));
        assert!(!overlaps(hold(t, Some(30)), hold(t + chrono::Duration::minutes(31), Some(30))));
        // Exactly 30 minutes apart: the second's early 2 seconds are no clash, either way round.
        assert!(!overlaps(hold(t, Some(30)), hold(t + chrono::Duration::minutes(30), Some(30))));
        assert!(!overlaps(hold(t + chrono::Duration::minutes(30), Some(30)), hold(t, Some(30))));
        assert!(overlaps(hold(t, Some(30)), hold(t + chrono::Duration::minutes(30) - chrono::Duration::seconds(1), Some(30))));
        // A queue may be waited out for 30 minutes, never past the run's window.
        assert_eq!(queue_wait_left(&GRAB, t, t), Duration::from_secs(30 * 60));
        assert_eq!(queue_wait_left(&GRAB, t, t + chrono::Duration::minutes(25)), Duration::from_secs(5 * 60));
        assert_eq!(queue_wait_left(&GRAB, t, t + chrono::Duration::minutes(31)), Duration::ZERO);
        let now = utc("2026-11-28T00:00:00Z");
        assert_eq!(check_at(now, now), Err("past_time"));
        assert_eq!(check_at(now + chrono::Duration::days(61), now), Err("too_far"));
        assert_eq!(check_at(now + chrono::Duration::days(3), now), Ok(()));
        assert!(parse_at("2026-12-01T08:00").is_ok());
        assert!(parse_at("2026-12-01T08:00:00+08:00").is_ok());
        assert_eq!(parse_at("tomorrow").map(|_| ()), Err("bad_time"));
        // A local time the clock skips for summer time has its own words, in the nine languages.
        for lang in [Lang::ZhTw, Lang::ZhCn, Lang::En, Lang::Ja, Lang::Ko, Lang::Es, Lang::Fr, Lang::De, Lang::Pt] {
            for code in ["time_skipped", "number_taken", "too_many_open"] {
                let s = error_text(lang, code).unwrap();
                assert!(!s.contains("{max}"), "{lang:?} {code}");
            }
            assert!(error_text(lang, "too_many_open").unwrap().contains("50"), "{lang:?}");
        }
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
            (Lang::Ja, "1月2日9時に市役所に電話して"),
            (Lang::Fr, "Le 2/1 à 9h appelle la mairie"),
            (Lang::De, "Am 2.1. um 9 Uhr ruf die Praxis an"),
        ] {
            assert_eq!(call_time_in(text, late, lang), Some(at(2027, 1, 2, 9, 0)), "{lang:?} {text}");
        }
        // Not plain enough: a weekday or "next", a day said in words, a time
        // alone, a booking named before the call, a time after the words
        // asking for the call. None: the chat model's reading stands.
        for (lang, text) in [
            (Lang::En, "Next Saturday at 8am call Joe's Diner to book 4"),
            (Lang::ZhTw, "下週六晚上8點打給鼎泰豐訂4位"),
            (Lang::Ja, "来週の土曜の8時に鈴木食堂に電話して"),
            (Lang::Es, "El sábado a las 8 llama al restaurante"),
            (Lang::De, "Am Montag um 9 Uhr ruf die Praxis an"),
            (Lang::En, "Book a table for 4 at 7pm tomorrow, call 0223456789"),
            (Lang::ZhTw, "幫我訂明天晚上七點兩位，打給鼎泰豐"),
            (Lang::Ko, "다음 주 토요일 오전 8시에 한강식당에 전화해 줘"),
            (Lang::Fr, "Samedi prochain à 8h appelle le bistrot"),
            (Lang::Pt, "Sábado às 8h liga para a pizzaria"),
            (Lang::ZhTw, "明天 8:00 打給 0223456789 訂位"),
            (Lang::Es, "Mañana a las 9 llama a la clínica"),
            (Lang::En, "Tomorrow 9am call the clinic"),
            (Lang::ZhTw, "8點幫我打給 0223456789"),
            (Lang::ZhCn, "帮我预订12月1日晚上7点4位，打给 0223456789"),
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
    fn a_time_with_its_zone_is_that_moment_in_local_time() {
        let now = at(2026, 11, 28, 10, 0);
        let local = |h: u32, offset_h: i32| {
            chrono::FixedOffset::east_opt(offset_h * 3600).unwrap().with_ymd_and_hms(2026, 12, 1, h, 0, 0).unwrap().with_timezone(&Local).naive_local()
        };
        for (when, want) in [
            ("2026-12-01T08:00:00Z", local(8, 0)),
            ("2026-12-01T08:00Z", local(8, 0)),
            ("2026-12-01T08:00:00+08:00", local(8, 8)),
            ("2026-12-01T08:00+08:00", local(8, 8)),
            ("2026-12-01T08:00:00-05:00", local(8, -5)),
            ("2026-12-01T08:00-05:00", local(8, -5)),
        ] {
            assert_eq!(resolve_when(when, now, Lang::En), Some(want), "{when}");
        }
        // No zone: the owner's local time as written.
        assert_eq!(resolve_when("2026-12-01T08:00", now, Lang::En), Some(at(2026, 12, 1, 8, 0)));
        assert_eq!(resolve_when("2026-12-01T08:00:00", now, Lang::En), Some(at(2026, 12, 1, 8, 0)));
        // A zone that cannot be read: refused, not dropped.
        for when in ["2026-12-01T08:00+8", "2026-12-01T08:00:00+08", "2026-12-01T08:00 -5"] {
            assert_eq!(resolve_when(when, now, Lang::En), None, "{when}");
        }
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
            let g = fill_grab(words(lang).grab, 30);
            assert!(g.contains('3') && g.contains("30") && !g.contains('{'), "{lang:?} {g}");
            assert!(error_text(lang, "line_taken").is_some_and(|s| s.contains("{who}") && s.contains("{when}")), "{lang:?}");
        }
        assert_eq!(error_text(Lang::En, "busy"), None);
    }

    /// Each errand redials for its own minutes (1 to 120, 30 unless set):
    /// its window, the line it holds, how late it may still start, and how
    /// long a queue may keep it all follow them; the gap, the head start and
    /// the per-minute cap do not change.
    #[test]
    fn each_errand_redials_for_its_own_minutes_and_holds_its_line_that_long() {
        assert!(!grab_min_ok(0) && grab_min_ok(1) && grab_min_ok(120) && !grab_min_ok(121));
        assert_eq!(GRAB_MIN_DEFAULT, 30);
        let t = utc("2026-12-01T00:00:00Z");
        let ten = GRAB.for_minutes(10);
        assert_eq!((ten.window, ten.gap, ten.lead, ten.per_minute_cap), (Duration::from_secs(600), GRAB.gap, GRAB.lead, GRAB.per_minute_cap));
        // Its window: no try starts once its minutes are over.
        assert!(grab_next_at(&ten, t, &[], t + chrono::Duration::seconds(10 * 60 - 4)).is_some());
        assert_eq!(grab_next_at(&ten, t, &[], t + chrono::Duration::seconds(10 * 60 - 2)), None);
        let long = GRAB.for_minutes(120);
        assert!(grab_next_at(&long, t, &[], t + chrono::Duration::minutes(90)).is_some());
        // The line it holds and how late it may still start.
        assert_eq!(hold(t, Some(10)), (t - chrono::Duration::seconds(2), t + chrono::Duration::minutes(10)));
        assert_eq!(hold(t, Some(120)).1, t + chrono::Duration::minutes(120));
        assert_eq!(start_by(t, Some(10)), t + chrono::Duration::minutes(10));
        assert_eq!(start_by(t, Some(90)), t + chrono::Duration::minutes(90));
        assert_eq!(start_by(t, None), t + chrono::Duration::minutes(5));
        // Two schedules with different minutes: a short one ending before the next is free; a long one reaching into it clashes.
        assert!(!overlaps(hold(t, Some(10)), hold(t + chrono::Duration::minutes(10), Some(30))));
        assert!(overlaps(hold(t, Some(45)), hold(t + chrono::Duration::minutes(30), Some(10))));
        assert!(!overlaps(hold(t, Some(30)), hold(t + chrono::Duration::minutes(31), Some(120))));
        // A queue: never past its own window.
        assert_eq!(queue_wait_left(&ten, t, t), Duration::from_secs(600));
        assert_eq!(queue_wait_left(&long, t, t), GRAB.queue_wait);
        // What the owner reads says its minutes.
        for lang in [Lang::ZhTw, Lang::ZhCn, Lang::En, Lang::Ja, Lang::Ko, Lang::Es, Lang::Fr, Lang::De, Lang::Pt] {
            let g = fill_grab(words(lang).grab, 45);
            assert!(g.contains("45") && !g.contains("30") && !g.contains('{'), "{lang:?} {g}");
            for code in ["bad_grab_min", "grab_needs_time"] {
                let s = error_text(lang, code).unwrap();
                assert!(!s.contains('{'), "{lang:?} {code} {s}");
            }
            assert!(error_text(lang, "bad_grab_min").unwrap().contains("120"), "{lang:?}");
        }
    }

    /// The chat card of a scheduled call says its minutes, in languages other than Chinese too.
    #[test]
    fn a_scheduled_calls_card_says_how_many_minutes_it_redials() {
        for (lang, want) in [
            (Lang::En, "for up to 45 minutes"),
            (Lang::Ja, "最長 45 分"),
            (Lang::Fr, "pendant 45 minutes au plus"),
            (Lang::De, "höchstens 45 Minuten"),
            (Lang::Es, "durante 45 minutos"),
            (Lang::ZhTw, "最多 45 分鐘"),
        ] {
            let card = card_lines(lang, "Tue, Dec 1, 08:00", "Pixel 9", 45);
            assert!(card.contains(want) && card.contains("Pixel 9") && card.contains("Dec 1"), "{lang:?}: {card}");
            assert_eq!(card.lines().count(), 3, "{lang:?}");
        }
    }

    /// Minutes of redialing read from an owner's message, in the nine
    /// languages; a time, a party size or minutes not about redialing are none.
    #[test]
    fn minutes_of_redialing_are_read_from_a_chat_message() {
        for (text, want) in [
            ("12/1 8點幫我打給 0223456789 訂位，打 10 分鐘", 10),
            ("12/1 8點打給鼎泰豐，一直打十五分鐘", 15),
            ("12月1日8点打给 0223456789，重拨20分钟", 20),
            ("12/1 8點打給 0223456789，45 分鐘內一直撥", 45),
            ("On 12/1 at 8am call Joe's Diner, keep calling for 45 minutes", 45),
            ("Dec 1 8am redial 0912345678 for 90 min", 90),
            ("12月1日8時に鈴木食堂に電話して、10分間かけ続けて", 10),
            ("12월 1일 오전 8시에 한강식당에 전화해 줘, 20분 동안 계속 걸어 줘", 20),
            ("El 1/12 a las 8 llama al restaurante durante 25 minutos", 25),
            ("Le 1/12 à 8h appelle le bistrot pendant 40 minutes", 40),
            ("Am 1.12. um 8 Uhr ruf das Gasthaus 15 Minuten lang an", 15),
            ("Dia 1/12 às 8h liga para o restaurante por 30 minutos", 30),
            ("12/1 8點打給 0223456789，打 200 分鐘", 200),
        ] {
            assert_eq!(grab_minutes_in(text), Some(want), "{text}");
        }
        for text in [
            "12/1 8點幫我打給鼎泰豐訂 4 位",
            "12/1 8點打給阿姨說我會晚到 10 分鐘",
            "On 12/1 at 8am call Joe's Diner and book a table for 4",
            "Call the clinic and say I'll be 10 minutes late",
            "12月1日8時に電話して、10分遅れると伝えて",
            "Le 1/12 à 8h appelle le bistrot, on arrive dans 20 minutes",
            "Am 1.12. um 8 Uhr ruf an und sag, dass wir 15 Minuten später kommen",
        ] {
            assert_eq!(grab_minutes_in(text), None, "{text}");
        }
    }

    /// A time written another way with a zone after it (Z, an offset, a
    /// zone's name) is refused, never read as local time; RFC 3339 still reads.
    #[test]
    fn a_time_with_a_zone_not_in_rfc_3339_is_refused() {
        let now = at(2026, 11, 28, 10, 0);
        for (when, lang) in [
            ("12/1 08:00Z", Lang::ZhTw),
            ("12/1 08:00+08:00", Lang::ZhTw),
            ("12/1 08:00 +0800", Lang::En),
            ("2026-12-01T08:00 UTC", Lang::En),
            ("2026-12-01 08:00 GMT", Lang::En),
            ("12/1 08:00 GMT+8", Lang::En),
            ("12/1 08:00 utc", Lang::En),
            ("Dec 1 8am EST", Lang::En),
            ("12/1 8 pm PST", Lang::En),
            ("1/12 20h CET", Lang::Fr),
            ("1.12. 8 Uhr CET", Lang::De),
        ] {
            assert_eq!(resolve_when(when, now, lang), None, "{when}");
        }
        assert!(resolve_when("2026-12-01T08:00Z", now, Lang::En).is_some());
        assert_eq!(resolve_when("12/1 08:00", now, Lang::ZhTw), Some(at(2026, 12, 1, 8, 0)));
        assert_eq!(resolve_when("1/12 20h15", now, Lang::Fr), Some(at(2026, 12, 1, 20, 15)));
        // In a chat message: none read (the chat model reads it with its zone).
        assert_eq!(call_time_in("12/1 08:00 UTC call 0912345678", now, Lang::En), None);
        assert_eq!(call_time_in("12/1 8:00Z 打給 0223456789", now, Lang::ZhTw), None);
        // A chat command keeps the zone name with its time, so it is refused.
        let (when, rest) = leading_when("12/1 08:00 UTC 0912345678 訂4位").unwrap();
        assert_eq!((when, rest), ("12/1 08:00 UTC", "0912345678 訂4位"));
        assert_eq!(resolve_when(when, now, Lang::ZhTw), None);
        // A French "est" after a time is a word, not a zone.
        assert!(!has_zone("Le 1/12 à 20h est parfait, appelle le bistrot"));
        assert!(!has_zone("2026-12-01 08:00"));
    }

    /// Every way of writing the hour that a time is read in (minutes in
    /// Chinese and Korean, am/pm after a space, o'clock, heures, horas, a word
    /// before a bare hour) is refused with a zone after it; written without
    /// the zone it reads as before. A chat command keeps an offset written
    /// alone with its time, never a country code.
    #[test]
    fn every_way_of_writing_the_hour_is_refused_with_a_zone_after_it() {
        let now = at(2026, 11, 28, 10, 0);
        for (time, zone, lang, want) in [
            ("12/1 8點30分", " UTC", Lang::ZhTw, at(2026, 12, 1, 8, 30)),
            ("12/1 8點半", " Z", Lang::ZhTw, at(2026, 12, 1, 8, 30)),
            ("12월 1일 8시 30분", " GMT", Lang::Ko, at(2026, 12, 1, 8, 30)),
            ("12/1 08:00 am", " UTC", Lang::En, at(2026, 12, 1, 8, 0)),
            ("12/1 08:00 PM", " +08:00", Lang::En, at(2026, 12, 1, 20, 0)),
            ("12/1 8 o'clock", " UTC", Lang::En, at(2026, 12, 1, 8, 0)),
            ("1/12 20 heures", " UTC", Lang::Fr, at(2026, 12, 1, 20, 0)),
            ("1/12 8 horas", " UTC", Lang::Es, at(2026, 12, 1, 8, 0)),
            ("12/1 at 8", " UTC", Lang::En, at(2026, 12, 1, 8, 0)),
            ("1/12 à 20", " CET", Lang::Fr, at(2026, 12, 1, 20, 0)),
            ("1/12 a las 8", " +01:00", Lang::Es, at(2026, 12, 1, 8, 0)),
        ] {
            let zoned = format!("{time}{zone}");
            assert!(has_zone(&zoned), "{zoned}");
            assert_eq!(resolve_when(&zoned, now, lang), None, "{zoned}");
            assert!(!has_zone(time), "{time}");
            assert_eq!(resolve_when(time, now, lang), Some(want), "{time}");
        }
        // A chat command: "+0800" stays with the time and is refused; "+886" is the number's.
        let (when, rest) = leading_when("12/1 08:00 +0800 0912345678 訂位").unwrap();
        assert_eq!((when, rest), ("12/1 08:00 +0800", "0912345678 訂位"));
        assert_eq!(resolve_when(when, now, Lang::ZhTw), None);
        assert_eq!(leading_when("12/1 08:00 +886 2 2345 6789 訂位"), Some(("12/1 08:00", "+886 2 2345 6789 訂位")));
        assert_eq!(leading_when("12/1 08:00 +44 20 7946 0000 book"), Some(("12/1 08:00", "+44 20 7946 0000 book")));
        for t in ["+08", "-05", "+0800", "-0530", "+530", "+14"] {
            assert!(offset_token(t), "{t}");
        }
        for t in ["+886", "+44", "+1", "+81", "+08000", "0800", "+1599"] {
            assert!(!offset_token(t), "{t}");
        }
    }

    #[test]
    fn a_zone_written_wide_before_the_time_in_brackets_or_by_its_area_is_refused() {
        let now = at(2026, 11, 28, 10, 0);
        for (zoned, lang) in [
            ("12/1 ８：００ UTC", Lang::ZhTw),
            ("12/1 20:00 ＵＴＣ", Lang::ZhTw),
            ("UTC 12/1 08:00", Lang::En),
            ("UTC+8 12/1 08:00", Lang::En),
            ("12/1 UTC 08:00", Lang::En),
            ("12/1 UTC 8點", Lang::ZhTw),
            ("12/1 08:00 (UTC)", Lang::En),
            ("12/1 08:00 (+08:00)", Lang::En),
            ("12/1 08:00 （+0800）", Lang::ZhTw),
            ("12/1 08:00 z", Lang::En),
            ("12/1 08:00 Asia/Taipei", Lang::En),
            ("12/1 08:00 America/New_York", Lang::En),
            ("1/12 8 Uhr MEZ", Lang::De),
            ("12/1 8 PM Pacific", Lang::En),
            ("12/1 8:00 +8", Lang::En),
        ] {
            assert!(has_zone(zoned), "{zoned}");
            assert_eq!(resolve_when(zoned, now, lang), None, "{zoned}");
        }
        // The same without a zone still reads; full-width digits too.
        assert_eq!(resolve_when("12/1 ８：００", now, Lang::ZhTw), Some(at(2026, 12, 1, 8, 0)));
        assert_eq!(resolve_when("12/1 08:00", now, Lang::En), Some(at(2026, 12, 1, 8, 0)));
        assert!(!has_zone("Le 1/12 à 20h est parfait"));
        // A chat command keeps a lone Z, a bracketed offset and a one-digit
        // offset with the time, so the time is refused.
        for (cmd, when) in [
            ("12/1 08:00 Z 0912345678 訂位", "12/1 08:00 Z"),
            ("12/1 08:00 (+0800) 0912345678 訂位", "12/1 08:00 (+0800)"),
            ("12/1 08:00 +8 0912345678 訂位", "12/1 08:00 +8"),
            ("12/1 ８：００ UTC 0912345678 訂位", "12/1 ８：００ UTC"),
        ] {
            let (w, rest) = leading_when(cmd).unwrap();
            assert_eq!((w, rest), (when, "0912345678 訂位"), "{cmd}");
            assert_eq!(resolve_when(w, now, Lang::ZhTw), None, "{cmd}");
        }
        // A country code after the time stays the number's: +1, +7 and three digits.
        for cmd in ["12/1 08:00 +1 650 555 0164 book", "12/1 08:00 +7 495 123 4567 book", "12/1 08:00 +353 1 234 5678 book"] {
            let (w, _) = leading_when(cmd).unwrap();
            assert_eq!(w, "12/1 08:00", "{cmd}");
        }
        for t in ["+8", "-5", "+0545", "+1245"] {
            assert!(offset_token(t), "{t}");
        }
        for t in ["+1", "+7", "+353", "+420", "+358", "+1242"] {
            assert!(!offset_token(t), "{t}");
        }
    }
}
