//! Appointments: when a caller wants to set a time, the AI takes it down and
//! tells them it will check and text them back later (only "get back to you"
//! when no text can reach them). No calendar is connected: after the call the
//! owner accepts or declines it under "To do" on the web console (or with a
//! Telegram button), the caller gets a text message back through the phone
//! that took the call, and the owner adds it to their own calendar with one
//! tap ([`crate::calendar`], setting `calendar_app`).
//!
//! Off by default (setting `appointment_mode`: `off` or `confirm`; an `auto`
//! saved by an older agent is `confirm`). How the pieces fit:
//! - [`ApptSession::for_call`] is made per call. The Live session
//!   ([`crate::gemini_live::run_session`]) adds [`tool_declarations`] and
//!   [`ApptSession::prompt_block`] to a normal call's setup (never to the
//!   sales / scam stalling session) and answers each `toolCall` with
//!   [`ApptSession::run_tool`] on a task of its own, so the audio loop never waits.
//! - What the caller agreed to is kept in the call's [`CallAppointments`] and
//!   goes with the call record ([`crate::calls::CallRecord::appointment`]).
//!   A call can agree to more than one appointment (another matter, another
//!   day): the first goes on the record, the others in its [`Appointment::more`].
//! - After the call [`create_tasks`] makes one task per appointment and
//!   [`after_summary`] sends each one's card, text message or both; [`decide`]
//!   carries out the owner's "accept" / "decline" on one of them (Telegram
//!   button or `POST /calls/api/tasks/{id}/…`): the text to the caller, and
//!   for "accept" the link that adds it to the owner's calendar.
//! - A call from one of the test numbers (setting `appointment_test_numbers`)
//!   takes appointments in `auto` mode on the demo calendar ([`DemoCal`],
//!   `demo_calendar.json`), whatever `appointment_mode` says: free times are
//!   checked and booked on the call, so the whole path can be tried without a
//!   real phone call to anyone. Its appointment remembers that
//!   ([`Appointment::calendar`]), so the owner's decision and a voided event go
//!   to the demo calendar too. An "accept" the demo calendar cannot take (the
//!   write failed, or another appointment took the time meanwhile) leaves it
//!   waiting, with nothing texted, to be pressed again.
//!
//! Times are the machine's local time (`TZ`), written `YYYY-MM-DDTHH:MM`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{Datelike, Duration as Span, NaiveDateTime, Timelike};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::callback::{ControlRegistry, SmsOutcome, SmsSim, SMS_RESULT_WAIT};
use crate::calls::{CallRecord, CallStore};
use crate::locale::{self, Lang, Phrases};
use crate::settings_store::RuntimeSettings;
use crate::tasks::{ReplyMethod, TaskIntent, TaskPatch, TaskRecord, TaskStatus, TaskStore};

/// Setting `appointment_mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The AI takes no appointments (default).
    Off,
    /// The AI says it will check and get back to the caller; the owner decides
    /// on the web console or a Telegram card.
    Confirm,
    /// A free time is taken on the call and put in the demo calendar: test
    /// numbers only ([`ApptSession::for_call`]), never a setting.
    Auto,
}

impl Mode {
    /// The setting: `confirm`, or `auto` from an older agent (when Google
    /// Calendar could be connected), takes appointments; anything else is off.
    pub fn parse(s: &str) -> Mode {
        match s.trim().to_ascii_lowercase().as_str() {
            "confirm" | "auto" => Mode::Confirm,
            _ => Mode::Off,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Confirm => "confirm",
            Mode::Auto => "auto",
        }
    }
}

/// Waiting for the owner.
pub const PENDING: &str = "pending";

/// How the caller hears the owner's answer to an appointment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// A text message from the phone that took the call.
    Sms,
    /// The AI calls them back and tells them ([`call_caller`]).
    Call,
    /// Nobody tells them (a hidden number, or the owner's choice).
    None,
}

impl Reply {
    /// `sms` / `call` / `none` (any case); anything else is not a choice.
    pub fn parse(s: &str) -> Option<Reply> {
        match s.trim().to_ascii_lowercase().as_str() {
            "sms" => Some(Reply::Sms),
            "call" => Some(Reply::Call),
            "none" => Some(Reply::None),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Reply::Sms => "sms",
            Reply::Call => "call",
            Reply::None => "none",
        }
    }
}

/// What can reach a caller: a text (texts on, setting `appointment_sms`, and
/// a number that takes one), a call (a number a call-back may dial, [`callable_on`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct ReplyOptions {
    pub sms: bool,
    pub call: bool,
}

impl ReplyOptions {
    pub fn allows(self, way: Reply) -> bool {
        match way {
            Reply::Sms => self.sms,
            Reply::Call => self.call,
            Reply::None => true,
        }
    }
}

/// A number that can be called back: one that is shown (hidden, "anonymous",
/// "private" and the like carry no digits).
pub fn callable(number: &str) -> bool {
    number.chars().filter(char::is_ascii_digit).count() >= 3
}

/// Toll-free, premium and shared-cost numbers (national number prefixes) a
/// call-back never dials, by country.
const NO_CALLBACK_PREFIXES: &[(&str, &[&str])] = &[
    // 0800 / 0809 toll-free; 0203 / 0204 / 0205 / 0209 premium (Taipei
    // numbers never start 02-0).
    ("tw", &["800", "809", "20"]),
    ("jp", &["120", "800", "570", "990"]),
    ("kr", &["80", "60"]),
    ("cn", &["400", "800"]),
    ("hk", &["800", "900"]),
    ("sg", &["800", "1800", "1900"]),
    ("gb", &["800", "808", "84", "87", "9"]),
    ("de", &["800", "137", "138", "180", "900"]),
    ("fr", &["80", "81", "82", "89"]),
    ("au", &["1800", "1300", "190", "13"]),
];

/// A number an AI call-back may dial on a line of country `region` (the SIM
/// that took the call, ISO 3166 alpha-2; None: Taiwan): a number of that same
/// country, as long as a real phone number there (Taiwan: 8 or 9 digits after
/// the trunk 0, landline or mobile; +1: 10 digits), and no toll-free, premium
/// or shared-cost number ([`NO_CALLBACK_PREFIXES`], +1 800…888 and 900).
/// Hidden numbers, short codes (165, 110, 1999…), numbers from abroad and
/// cut-off digits are not.
pub fn callable_on(caller_id: &str, region: Option<&str>) -> bool {
    let t = caller_id.trim();
    if t.is_empty()
        || !t
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | ' ' | '-' | '(' | ')' | '.'))
    {
        return false;
    }
    let region = region
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_SMS_REGION)
        .to_ascii_lowercase();
    let Some(code) = crate::phone_region::calling_code(&region) else {
        return false;
    };
    let digits: String = t.chars().filter(char::is_ascii_digit).collect();
    if !t.starts_with('+') && !digits.starts_with("00") {
        // Dialled at home: with the trunk 0, or the country code without its
        // plus; bare digits are a short code or a cut-off number.
        let with_code = digits.starts_with(code) && digits.len() >= code.len() + 8;
        if !NO_TRUNK_CODES.contains(&code) && !digits.starts_with('0') && !with_code {
            return false;
        }
    }
    let Some(n) = crate::phone_region::normalize(t, Some(&region)) else {
        return false;
    };
    if n.country != region {
        return false;
    }
    let Some(national) = n.e164.strip_prefix('+').and_then(|d| d.strip_prefix(code)) else {
        return false;
    };
    let national = if code == "39" { national } else { national.trim_start_matches('0') };
    let (min, max) = match (region.as_str(), code) {
        ("tw", _) => (8, 9),
        (_, "1") => (10, 10),
        _ => (7, 15 - code.len()),
    };
    if !(min..=max).contains(&national.len()) {
        return false;
    }
    let blocked: &[&str] = if code == "1" {
        NANP_NO_TEXT
    } else {
        NO_CALLBACK_PREFIXES.iter().find(|(c, _)| *c == region).map_or(&[], |(_, p)| p)
    };
    !blocked.iter().any(|p| national.starts_with(p))
}

/// [`ReplyOptions`] for `caller_id`; `region`: the country of the SIM that
/// took the call ([`sms_number_on`]).
pub fn reply_options(rt: &RuntimeSettings, caller_id: &str, region: Option<&str>) -> ReplyOptions {
    ReplyOptions {
        sms: rt.appointment_sms && sms_number_on(caller_id, region).is_some(),
        call: callable_on(caller_id, region),
    }
}

/// The answer's way when the owner does not pick one (setting
/// `appointment_reply`): `auto` = a text when one can reach the caller (a
/// mobile), else a call (a landline, a toll-free or foreign number that takes
/// no texts), else nobody (a hidden number); `sms` / `call` = only that one,
/// else nobody.
pub fn default_reply(setting: &str, opts: ReplyOptions) -> Reply {
    let pick = |way: Reply| if opts.allows(way) { way } else { Reply::None };
    match setting.trim().to_ascii_lowercase().as_str() {
        "sms" => pick(Reply::Sms),
        "call" => pick(Reply::Call),
        _ if opts.sms => Reply::Sms,
        _ => pick(Reply::Call),
    }
}

/// Setting `appointment_reply`: `auto` (default), `sms` or `call`.
pub fn sanitize_reply_setting(s: &str) -> String {
    match s.trim().to_ascii_lowercase().as_str() {
        "sms" => "sms",
        "call" => "call",
        _ => "auto",
    }
    .to_string()
}
/// Put in the demo calendar on the call (`auto`, a test number).
pub const BOOKED: &str = "booked";
/// The owner said yes.
pub const CONFIRMED: &str = "confirmed";
/// The owner said no.
pub const DECLINED: &str = "declined";

pub const CHECK: &str = "check_availability";
pub const PROPOSE: &str = "propose_appointment";
/// [`Appointment::calendar`] of an appointment on the demo calendar ([`DemoCal`]).
pub const DEMO_CALENDAR: &str = "demo";

/// How long `check_availability` may take: the caller is waiting on the line.
/// A slower calendar answers "unknown" and the AI says it will confirm.
const CHECK_TIMEOUT: Duration = Duration::from_secs(3);
/// How long the check before writing may take (an `auto` booking, the owner's
/// ✅). Nobody waits on the line for it, so it gets longer than [`CHECK_TIMEOUT`].
const BOOK_CHECK_TIMEOUT: Duration = Duration::from_secs(5);
/// How long writing to the calendar may take. Its own limit (not the check's)
/// so a slow insert is not cut short and taken for a failure; when it is cut
/// anyway, the event is found again by its tag ([`Appointment::tag`]).
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long an `auto` booking waits for the calendar while another booking or
/// an owner's ✅ is writing (that one holds it at most check + write, 15 s);
/// then the owner decides instead. Waiting, checking and writing together stay
/// within [`TOOL_WAIT`], which is what the end of a call waits for.
const CALENDAR_TURN_WAIT: Duration = Duration::from_secs(8);
/// How long a proposal waits for the one the model sent just before it in the
/// same message ([`ToolCall::after`]).
const AFTER_WAIT: Duration = Duration::from_secs(3);
/// The longest one proposal can run (waiting for the calendar, the check, the
/// write), plus a margin.
pub const TOOL_WAIT: Duration = Duration::from_secs(24);
const STAMP: &str = "%Y-%m-%dT%H:%M";
const TITLE_MAX: usize = 80;
const NAME_MAX: usize = 40;
const SMS_TEXT_MAX: usize = 500;
/// How a caller ID without a country code is read when the SIM that took the
/// call is not known (records and apps from before the SIM was reported).
const DEFAULT_SMS_REGION: &str = "tw";

/// One check-then-write on a calendar at a time, across every call and every
/// decision: two callers asking for the same free hour (or the owner confirming
/// two appointments for it) must not both find it free and both write it.
fn calendar_turn() -> &'static tokio::sync::Mutex<()> {
    static TURN: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    TURN.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Numbers proposals in the order they arrive (process-wide, from 1).
fn next_seq() -> u64 {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

/// A time agreed with a caller.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Appointment {
    /// Local time, `YYYY-MM-DDTHH:MM`.
    pub start: String,
    pub end: String,
    /// What it is about, a few words.
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub caller_name: String,
    /// `confirm` or `auto` (a test number): how the call took it.
    pub mode: String,
    /// [`PENDING`], [`BOOKED`], [`CONFIRMED`] or [`DECLINED`].
    pub status: String,
    /// The event's id once it is in the demo calendar (records from before
    /// Google Calendar was disconnected: its event there).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub event_id: String,
    /// `auto` could not book it (the owner decides instead): why.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auto_error: String,
    /// Times this call had already put in the calendar before the caller changed it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaced: Vec<Earlier>,
    /// The text message to the caller: `sent`, or why not (`offline`,
    /// `old_app`, `timeout`, `busy`, `no_number`, the phone's error code;
    /// [`sms_code_text`] says each in words). Empty = none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sms: String,
    /// How the caller was told the owner's answer: `sms`, `call` or `none`
    /// ([`Reply`]; empty: not decided yet, or decided before there was a choice).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reply: String,
    /// The AI's call-back with the answer: `dialing` (the phone was asked to
    /// call), or why it was not placed (`no_phone`, `failed`, `no_number`).
    /// How the call went follows in the task's `callback_*` events.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub call: String,
    /// This call's mark on the events it writes: finds them again when an
    /// answer was lost.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tag: String,
    /// The calendar its events are in: [`DEMO_CALENDAR`] = the demo calendar of
    /// a test number; empty = none (the owner adds it to their own calendar).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub calendar: String,
    /// A write to the calendar timed out: it may or may not be there (look it up by `tag`).
    #[serde(skip)]
    pub write_unsure: bool,
    /// The Telegram card sent for it, to take its buttons off when the owner
    /// decides on the web or through the API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tg_card: Option<TgCard>,
    /// The owner's "accept" could not put it in the demo calendar (the write
    /// failed, or the time was taken meanwhile): what they were told. It still
    /// waits; cleared once an "accept" goes through.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub write_error: String,
    /// An "accept" was held because the time is not free in the demo calendar.
    /// The owner was told so: the next one books it anyway.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub held_busy: bool,
    /// The country of the SIM that took the call (ISO 3166 alpha-2): how a
    /// caller ID without a country code is read for the text message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sim_country: Option<String>,
    /// The subscription (SIM) that took the call, so the text goes out from
    /// the number the caller rang.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sim_sub_id: Option<i64>,
    /// The other appointments the same call agreed to, each with a task of its
    /// own ([`create_tasks`]). Only on the call's record and notes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub more: Vec<Appointment>,
}

/// Where a Telegram card went: its chat and message.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TgCard {
    pub chat_id: String,
    pub message_id: i64,
}

/// An earlier time of the same call that is in the calendar.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Earlier {
    pub start: String,
    pub end: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub event_id: String,
    /// The call's tag, for an event whose id never came back.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tag: String,
    /// The calendar it is in ([`Appointment::calendar`]).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub calendar: String,
}

impl Earlier {
    fn of(a: &Appointment) -> Self {
        Self {
            start: a.start.clone(),
            end: a.end.clone(),
            title: a.title.clone(),
            event_id: a.event_id.clone(),
            tag: a.tag.clone(),
            calendar: a.calendar.clone(),
        }
    }
}

fn parse_local(s: &str) -> Option<NaiveDateTime> {
    let t = s.trim();
    for fmt in [STAMP, "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(t, fmt) {
            return Some(dt);
        }
    }
    // With a zone (`…+08:00`): the wall-clock part is what was meant.
    chrono::DateTime::parse_from_rfc3339(t).ok().map(|dt| dt.naive_local())
}

fn stamp(t: NaiveDateTime) -> String {
    t.format(STAMP).to_string()
}

/// "11/14（週六）15:00", "Sat, Nov 14, 3:00 PM": a time with its weekday, the way
/// the agent's language writes it.
pub fn when_text(lang: Lang, t: &NaiveDateTime) -> String {
    let p = locale::phrases(lang);
    let wd = p.ap_weekdays[t.weekday().num_days_from_monday() as usize];
    t.format(p.ap_dt_fmt).to_string().replacen("{}", wd, 1)
}

fn when_range(lang: Lang, start: &str, end: &str) -> String {
    let Some(s) = parse_local(start) else {
        return start.to_string();
    };
    let mut out = when_text(lang, &s);
    if let Some(e) = parse_local(end).filter(|e| *e > s) {
        if e.date() == s.date() {
            out.push_str(&format!("–{}", e.format(locale::phrases(lang).ap_end_fmt)));
        } else {
            out.push_str(&format!(" – {}", when_text(lang, &e)));
        }
    }
    out
}

impl Appointment {
    /// "11/14（週六）15:00–16:00".
    pub fn when(&self, lang: Lang) -> String {
        when_range(lang, &self.start, &self.end)
    }

    /// The start alone, for the text message.
    pub fn start_text(&self, lang: Lang) -> String {
        parse_local(&self.start).map_or_else(|| self.start.clone(), |s| when_text(lang, &s))
    }

    /// Its time and what it is ("11/14（週六）15:00–16:00 看房").
    pub fn headline(&self, lang: Lang) -> String {
        format!("{} {}", self.when(lang), self.title.trim()).trim().to_string()
    }

    /// The calendar event's title: what it is, else "Appointment — caller".
    fn calendar_title(&self, caller_id: &str) -> String {
        let t = self.title.trim();
        if t.is_empty() {
            locale::fill(texts().e_appointment, display_caller(caller_id))
        } else {
            t.to_string()
        }
    }

    /// As a calendar entry: "Add to calendar" in the web console and the
    /// owner's reply, the `.ics` file.
    pub fn calendar_event(&self) -> crate::calendar::Event {
        let title = if self.title.trim().is_empty() {
            locale::fill(texts().e_appointment, "")
                .trim_end_matches([' ', '—', '-'])
                .to_string()
        } else {
            self.title.trim().to_string()
        };
        crate::calendar::Event {
            title,
            start: self.start.clone(),
            end: self.end.clone(),
            all_day: false,
        }
    }

    /// This appointment and the others of the same call ([`Appointment::more`]),
    /// each one on its own.
    pub fn each(&self) -> Vec<Appointment> {
        let mut first = self.clone();
        let more = std::mem::take(&mut first.more);
        let mut out = vec![first];
        out.extend(more.into_iter().map(|mut a| {
            a.more.clear();
            a
        }));
        out
    }

    /// Two copies of the same appointment (the call's, its task's): the same
    /// tag — each appointment of a call has its own — or, for copies without
    /// one, the same time and title.
    fn same_as(&self, other: &Appointment) -> bool {
        if !self.tag.is_empty() || !other.tag.is_empty() {
            return self.tag == other.tag;
        }
        self.start == other.start && self.title == other.title
    }
}

impl Earlier {
    fn when(&self, lang: Lang) -> String {
        format!("{} {}", when_range(lang, &self.start, &self.end), self.title.trim())
            .trim()
            .to_string()
    }
}

/// What the owner reads (cards, results, calendar entries): the notification
/// language. Texts to the caller use the AI's ([`sms_text`]).
fn texts() -> &'static Phrases {
    locale::notify_phrases()
}

fn display_caller(caller_id: &str) -> &str {
    let t = caller_id.trim();
    if t.is_empty() {
        texts().n_hidden
    } else {
        t
    }
}

// ---------------------------------------------------------------------------
// One call's appointment
// ---------------------------------------------------------------------------

/// What the caller has agreed to on this call, shared by the Live session (it
/// writes) and the call record (it reads at the end of the call).
#[derive(Debug, Default)]
pub struct CallAppointments {
    inner: std::sync::Mutex<Book>,
    /// Proposals still running (a calendar write may be under way).
    in_flight: std::sync::atomic::AtomicUsize,
    /// One proposal of the call at a time ([`ApptSession::run_tool`] runs each
    /// function call on a task of its own): "eight doesn't work, make it half
    /// past nine" arrives as two calls almost at once, and two `auto` bookings
    /// side by side would both write.
    turns: tokio::sync::Mutex<()>,
}

#[derive(Debug, Default)]
struct Book {
    /// What the caller agreed to, one entry per appointment, in the order the
    /// model first proposed them.
    slots: Vec<Slot>,
    /// Appointments this call has started (each gets a tag of its own).
    made: usize,
    /// Judged a sales / scam call: nothing of this call is kept.
    discarded: bool,
    /// Calendar events this call wrote that no longer stand (the call was
    /// judged sales / scam): deleted after the call.
    voided: Vec<Earlier>,
    /// The line that took the call ([`CallAppointments::set_line`]).
    sim_country: Option<String>,
    sim_sub_id: Option<i64>,
    /// Proposals of this call that are over (kept, refused or superseded), for
    /// the one sent after them ([`ToolCall::after`]).
    finished: std::collections::HashSet<u64>,
}

/// One appointment of the call.
#[derive(Debug, Clone)]
struct Slot {
    appt: Appointment,
    /// Earlier times of it that are in the calendar.
    earlier: Vec<Earlier>,
    /// The proposal ([`ToolCall::seq`]) that started it, and the latest one
    /// that changed it.
    born: u64,
    seq: u64,
    /// The proposal that started it was a change of time
    /// (`replaces_previous`) that found nothing before it to change.
    replacing: bool,
}

/// Where a proposal goes ([`Book::target`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// It changes this appointment (an index into the slots).
    Change(usize),
    /// A new appointment.
    New,
    /// It would change an appointment a later proposal already changed: the
    /// later one stands (it reached the agent first).
    Stale,
}

impl Book {
    /// The appointment proposed last.
    fn latest(&self) -> Option<&Slot> {
        self.slots.iter().max_by_key(|s| s.born)
    }

    /// Where proposal `seq` goes. `replaces` (the model's `replaces_previous`):
    /// it changes the appointment proposed last before it — by arrival, not by
    /// which task ran first; else it is another appointment (one at the very
    /// time of an appointment already agreed is that one again, not a second).
    fn target(&self, seq: u64, replaces: bool, start: &str, end: &str) -> Target {
        let pick = |i: usize| {
            if self.slots[i].seq > seq {
                Target::Stale
            } else {
                Target::Change(i)
            }
        };
        if !replaces {
            return match self.slots.iter().position(|s| s.appt.start == start && s.appt.end == end) {
                Some(i) => pick(i),
                None => Target::New,
            };
        }
        match self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, s)| s.born < seq)
            .max_by_key(|(_, s)| s.born)
        {
            Some((i, _)) => return pick(i),
            None => {}
        }
        // Nothing before it — but a change of time that came after it already
        // ran and found nothing to change: it was meant for this one.
        match self.slots.iter().filter(|s| s.born > seq).min_by_key(|s| s.born) {
            Some(s) if s.replacing => Target::Stale,
            _ => Target::New,
        }
    }

    /// The tag of the events a proposal writes: the appointment's own, or for
    /// a new one the call's tag (the first) or the call's tag and its number.
    fn tag_for(&self, target: Target, call_tag: &str) -> String {
        match target {
            Target::Change(i) if !self.slots[i].appt.tag.is_empty() => self.slots[i].appt.tag.clone(),
            Target::New if self.made > 0 => format!("{call_tag}-{}", self.made + 1),
            _ => call_tag.to_string(),
        }
    }

    /// Keep what proposal `seq` agreed to. False: the call was judged sales /
    /// scam, nothing is kept (an event it already wrote goes on the voided list).
    fn put(&mut self, target: Target, appt: Appointment, seq: u64, replaces: bool) -> bool {
        if self.discarded {
            if !appt.event_id.is_empty() || appt.write_unsure {
                self.voided.push(Earlier::of(&appt));
            }
            return false;
        }
        match target {
            Target::Change(i) if i < self.slots.len() => {
                let slot = &mut self.slots[i];
                let prev = std::mem::replace(&mut slot.appt, appt);
                // A moved event keeps its id: it is not an earlier one.
                if !prev.event_id.is_empty() && prev.event_id != slot.appt.event_id {
                    slot.earlier.push(Earlier::of(&prev));
                }
                slot.seq = seq;
            }
            _ => {
                let at = self.slots.iter().position(|s| s.born > seq).unwrap_or(self.slots.len());
                self.slots.insert(
                    at,
                    Slot {
                        appt,
                        earlier: Vec::new(),
                        born: seq,
                        seq,
                        replacing: replaces,
                    },
                );
                self.made += 1;
            }
        }
        true
    }
}

/// Counts a running proposal while it lives.
struct InFlight<'a>(&'a std::sync::atomic::AtomicUsize);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Marks a proposal over when it returns, however it returns.
struct Finished<'a>(&'a CallAppointments, u64);

impl Drop for Finished<'_> {
    fn drop(&mut self) {
        self.0.book().finished.insert(self.1);
    }
}

impl CallAppointments {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn book(&self) -> std::sync::MutexGuard<'_, Book> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The caller agreed to this: it changes the appointment agreed last (the
    /// first one when there is none). False: the call was judged sales / scam,
    /// nothing is kept (an event it already wrote goes on the voided list, to be
    /// deleted after the call).
    pub fn record(&self, appt: Appointment) -> bool {
        let seq = next_seq();
        let mut b = self.book();
        let target = b.target(seq, true, &appt.start, &appt.end);
        b.put(target, appt, seq, true)
    }

    /// The line that took the call: the SIM's country (ISO 3166 alpha-2) and
    /// its subscription id, for the text message. Every appointment of the call
    /// carries them ([`Self::outcomes`]), also one agreed before they were known.
    pub fn set_line(&self, sim_country: Option<String>, sim_sub_id: Option<i64>) {
        let country = sim_country
            .map(|c| c.trim().to_ascii_lowercase())
            .filter(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_lowercase()));
        let mut b = self.book();
        b.sim_country = country;
        b.sim_sub_id = sim_sub_id;
    }

    /// The SIM country set by [`Self::set_line`].
    pub fn sim_country(&self) -> Option<String> {
        self.book().sim_country.clone()
    }

    /// The call turned out to be sales / scam: drop what it agreed to. Events
    /// it already put in the calendar move to the voided list.
    pub fn discard(&self) {
        let mut b = self.book();
        b.discarded = true;
        for slot in std::mem::take(&mut b.slots) {
            if !slot.appt.event_id.is_empty() || slot.appt.write_unsure {
                b.voided.push(Earlier::of(&slot.appt));
            }
            b.voided.extend(slot.earlier);
        }
    }

    pub fn is_discarded(&self) -> bool {
        self.book().discarded
    }

    /// The event of the appointment agreed last, when it is in the calendar.
    pub fn booked_event(&self) -> Option<Appointment> {
        self.book()
            .latest()
            .map(|s| s.appt.clone())
            .filter(|a| !a.event_id.is_empty())
    }

    /// Calendar events of this call that no longer stand (to delete).
    pub fn voided(&self) -> Vec<Earlier> {
        self.book().voided.clone()
    }

    fn start_proposal(&self) -> InFlight<'_> {
        self.in_flight.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        InFlight(&self.in_flight)
    }

    fn is_finished(&self, seq: u64) -> bool {
        self.book().finished.contains(&seq)
    }

    /// Wait (up to `max`) until proposal `seq` of this call is over. False: it
    /// was not (it never came, or is still writing).
    async fn wait_finished(&self, seq: u64, max: Duration) -> bool {
        let until = tokio::time::Instant::now() + max;
        loop {
            if self.is_finished(seq) {
                return true;
            }
            if tokio::time::Instant::now() >= until {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Wait (up to `max`) until no proposal is running, so the end of a call
    /// does not miss an event that is being written.
    pub async fn settled(&self, max: Duration) {
        let until = tokio::time::Instant::now() + max;
        while self.in_flight.load(std::sync::atomic::Ordering::SeqCst) > 0
            && tokio::time::Instant::now() < until
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Every appointment the call agreed to, in the order they were first
    /// proposed, each with its earlier times that are already in the calendar
    /// and the line that took the call.
    pub fn outcomes(&self) -> Vec<Appointment> {
        let b = self.book();
        if b.discarded {
            return Vec::new();
        }
        b.slots
            .iter()
            .map(|s| {
                let mut a = s.appt.clone();
                a.replaced = s.earlier.clone();
                if a.sim_country.is_none() {
                    a.sim_country = b.sim_country.clone();
                }
                if a.sim_sub_id.is_none() {
                    a.sim_sub_id = b.sim_sub_id;
                }
                a
            })
            .collect()
    }

    /// What the call agreed to, as the call record keeps it: the first
    /// appointment, the others in its [`Appointment::more`].
    pub fn outcome(&self) -> Option<Appointment> {
        let mut all = self.outcomes().into_iter();
        let mut first = all.next()?;
        first.more = all.collect();
        Some(first)
    }

    /// Keep what a proposal agreed to ([`Book::put`]).
    fn put(&self, target: Target, appt: Appointment, seq: u64, replaces: bool) -> bool {
        self.book().put(target, appt, seq, replaces)
    }
}

// ---------------------------------------------------------------------------
// Gemini Live functions
// ---------------------------------------------------------------------------

/// The setup's `tools` value: the two functions the model may call.
///
/// `non_blocking`: [`PROPOSE`] is declared `NON_BLOCKING`. The model has already
/// told the caller what happens next when it records the time, so it goes on
/// talking and the answer comes back silent ([`tool_response`]); blocking, it
/// said the same sentence a second time after the answer (seen on a real call).
/// [`CHECK`] always blocks: the reply depends on its answer.
pub fn tool_declarations(non_blocking: bool) -> Value {
    let mut decl = json!([{
        "functionDeclarations": [
            {
                "name": CHECK,
                "description": "Whether the person you speak for is free at a local date and time. Answers {\"free\": true}, {\"free\": false} (busy; what it is stays private) or {\"free\": null} (the calendar could not be read).",
                "parameters": {
                    "type": "OBJECT",
                    "properties": {
                        "start": { "type": "STRING", "description": "Local start time, YYYY-MM-DDTHH:MM." },
                        "weekday": { "type": "STRING", "description": "The weekday of start in English (Monday … Sunday), the day the caller means." },
                        "end": { "type": "STRING", "description": "Local end time, YYYY-MM-DDTHH:MM. Leave out for one hour." },
                        "period": { "type": "STRING", "description": PERIOD_HELP }
                    },
                    "required": ["start"]
                }
            },
            {
                "name": PROPOSE,
                "description": "Record an appointment agreed with the caller. Answers {\"status\": \"booked\"} (it is in the calendar), {\"status\": \"pending_owner_confirmation\"} (not settled yet; tell_caller says what the caller is told) or {\"status\": \"superseded\"} (a later call of yours already changed it; say nothing). Call it again with replaces_previous true only when the two of you change the time agreed last; with replaces_previous false for another, separate appointment (both stand). A back-up time the caller mentions is not a change.",
                "parameters": {
                    "type": "OBJECT",
                    "properties": {
                        "start": { "type": "STRING", "description": "Local start time, YYYY-MM-DDTHH:MM." },
                        "weekday": { "type": "STRING", "description": "The weekday of start in English (Monday … Sunday), the day the caller means." },
                        "end": { "type": "STRING", "description": "Local end time, YYYY-MM-DDTHH:MM. Leave out for one hour." },
                        "period": { "type": "STRING", "description": PERIOD_HELP },
                        "title": { "type": "STRING", "description": "What it is about, a few words." },
                        "caller_name": { "type": "STRING", "description": "The caller's name, only when they said it. Leave it out otherwise: never a word like \"friend\" or \"caller\"." },
                        "replaces_previous": { "type": "BOOLEAN", "description": "true: this changes the time of the appointment agreed last in this call (it replaces it). false: another, separate appointment (another matter or day; both stand). Left out: true." }
                    },
                    "required": ["start", "title"]
                }
            }
        ]
    }]);
    if non_blocking {
        decl[0]["functionDeclarations"][1]["behavior"] = json!("NON_BLOCKING");
    }
    decl
}

/// The model waits for this function's answer ([`tool_declarations`]).
pub fn is_blocking(name: &str, non_blocking: bool) -> bool {
    !(non_blocking && name == PROPOSE)
}

/// One function call from the model (`toolCall.functionCalls[]`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: Value,
    /// Its place in the order calls arrived (process-wide, from 1), given when
    /// the server message is read: the calls then run on tasks of their own,
    /// which may start in any order. 0 = not numbered (taken when it runs).
    pub seq: u64,
    /// The [`PROPOSE`] call sent just before it in the same message (its
    /// `seq`; 0 = none). It runs first: "another appointment" and "change its
    /// time" can come in one message, and the change belongs to the new one.
    pub after: u64,
}

/// The function calls in a server message (none for every other message),
/// numbered in the order they came ([`ToolCall::seq`]), each proposal tied to
/// the one before it ([`ToolCall::after`]).
pub fn parse_tool_calls(msg: &Value) -> Vec<ToolCall> {
    let mut last_proposal = 0;
    msg.pointer("/toolCall/functionCalls")
        .and_then(Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .filter_map(|c| {
                    let name = c.get("name")?.as_str()?.to_string();
                    let seq = next_seq();
                    let after = if name == PROPOSE {
                        std::mem::replace(&mut last_proposal, seq)
                    } else {
                        0
                    };
                    Some(ToolCall {
                        id: c.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                        name,
                        args: c.get("args").cloned().unwrap_or_else(|| json!({})),
                        seq,
                        after,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Ids of calls the server no longer wants answered (`toolCallCancellation`).
pub fn parse_tool_cancellation(msg: &Value) -> Vec<String> {
    msg.pointer("/toolCallCancellation/ids")
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(|i| i.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// The client message answering one function call. [`PROPOSE`]'s answer says
/// `"scheduling": "SILENT"`: the model already said what it had to (blocking
/// or not, it is not to speak about it again) — except when `auto` could not
/// book the time it already agreed to (`"not_booked": true`): then
/// `"WHEN_IDLE"`, so it tells the caller it has to confirm after all — or
/// when the call was refused (`"error"`, e.g. a wrong weekday), so it calls again.
pub fn tool_response(id: &str, name: &str, response: Value) -> Value {
    let mut response = response;
    if name == PROPOSE {
        let silent = answer_is_silent(name, &response);
        if let Some(o) = response.as_object_mut() {
            o.insert("scheduling".into(), json!(if silent { "SILENT" } else { "WHEN_IDLE" }));
        }
    }
    json!({
        "toolResponse": {
            "functionResponses": [{ "id": id, "name": name, "response": response }]
        }
    })
}

/// A [`PROPOSE`] answer the model is not to speak about ([`tool_response`]).
pub fn answer_is_silent(name: &str, response: &Value) -> bool {
    name == PROPOSE
        && response.get("not_booked").and_then(Value::as_bool) != Some(true)
        && response.get("error").is_none()
}

/// Where appointments are checked and written: the demo calendar of a test
/// number, or (tests) a stand-in that counts what was written. Other calls
/// have none: no calendar account is connected.
#[derive(Clone, Debug)]
enum Cal {
    Demo(DemoCal),
    #[cfg(test)]
    Fake(Arc<FakeCal>),
}

impl Cal {
    /// The calendar an appointment's events are in ([`Appointment::calendar`]):
    /// the demo calendar, else none.
    fn for_calendar(rt: &RuntimeSettings, calendar: &str) -> Option<Self> {
        (calendar == DEMO_CALENDAR).then(|| Cal::Demo(DemoCal::from_settings(rt)))
    }

    /// What [`Appointment::calendar`] says for this calendar.
    fn name(&self) -> &'static str {
        match self {
            Cal::Demo(_) => DEMO_CALENDAR,
            _ => "",
        }
    }

    async fn is_free_except(&self, http: &Client, start: &str, end: &str, own_id: &str) -> Result<bool> {
        let _ = http;
        match self {
            Cal::Demo(d) => d.is_free_except(start, end, own_id),
            #[cfg(test)]
            Cal::Fake(f) => f.is_free_except(start, end, own_id),
        }
    }

    async fn insert(&self, http: &Client, appt: &Appointment, caller_id: &str) -> Result<String> {
        let _ = http;
        match self {
            Cal::Demo(d) => d.insert(appt, caller_id),
            #[cfg(test)]
            Cal::Fake(f) => f.insert(appt).await,
        }
    }

    async fn move_to(&self, http: &Client, event_id: &str, appt: &Appointment) -> Result<()> {
        let _ = http;
        match self {
            Cal::Demo(d) => d.move_to(event_id, appt),
            #[cfg(test)]
            Cal::Fake(f) => f.move_to(event_id, appt).await,
        }
    }

    async fn delete(&self, http: &Client, event_id: &str) -> Result<()> {
        let _ = http;
        match self {
            Cal::Demo(d) => d.delete(event_id),
            #[cfg(test)]
            Cal::Fake(f) => f.delete(event_id),
        }
    }

    async fn find_tagged(&self, http: &Client, tag: &str) -> Result<Vec<String>> {
        let _ = http;
        match self {
            Cal::Demo(d) => d.find_tagged(tag),
            #[cfg(test)]
            Cal::Fake(f) => Ok(f.find_tagged(tag)),
        }
    }

    /// Put the appointment in the calendar at its time: move its own event
    /// `own_id` ([`own_event`]), else insert one. The event's id.
    async fn write_at(&self, http: &Client, appt: &Appointment, own_id: &str, caller_id: &str) -> Result<String> {
        if own_id.is_empty() {
            self.insert(http, appt, caller_id).await
        } else {
            self.move_to(http, own_id, appt).await?;
            Ok(own_id.to_string())
        }
    }
}

/// The demo calendar test numbers book on (setting `appointment_test_numbers`):
/// `demo_calendar.json` in the data folder, `{"busy": [[start, end], …],
/// "events": [[id, start, end, tag, title], …]}` with local `YYYY-MM-DDTHH:MM`
/// times. "busy" are other plans; "events" are what calls put in. Without the
/// file, lunch (12:00–13:00) and the evening (19:00–21:00) of the 14 days from
/// today are busy. Read and written whole on every use, one use at a time
/// ([`DEMO_LOCK`], never held across an await).
#[derive(Clone, Debug)]
struct DemoCal {
    path: PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct DemoBook {
    /// Other plans: (start, end).
    #[serde(default)]
    busy: Vec<(String, String)>,
    /// (id, start, end, tag, title)
    #[serde(default)]
    events: Vec<(String, String, String, String, String)>,
}

/// One use of the demo calendar file at a time.
static DEMO_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

const DEMO_FILE: &str = "demo_calendar.json";
/// Days with the default busy times (today first).
const DEMO_DAYS: i64 = 14;

impl DemoCal {
    /// `demo_calendar.json` in the data folder (`/data/calls` when unknown).
    fn from_settings(rt: &RuntimeSettings) -> Self {
        let dir = rt.calls_data_dir.trim();
        let dir = if dir.is_empty() { "/data/calls" } else { dir };
        Self {
            path: PathBuf::from(dir).join(DEMO_FILE),
        }
    }

    fn hold() -> std::sync::MutexGuard<'static, ()> {
        DEMO_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Lunch and the evening busy on the 14 days from `today`.
    fn default_book(today: chrono::NaiveDate) -> DemoBook {
        let at = |day: chrono::NaiveDate, h: u32| day.and_hms_opt(h, 0, 0).map(stamp).unwrap_or_default();
        let mut busy = Vec::new();
        for d in 0..DEMO_DAYS {
            let day = today + Span::days(d);
            busy.push((at(day, 12), at(day, 13)));
            busy.push((at(day, 19), at(day, 21)));
        }
        DemoBook {
            busy,
            events: Vec::new(),
        }
    }

    /// The file as it is (the default when there is none). Under [`DEMO_LOCK`].
    fn load(&self) -> Result<DemoBook> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("demo calendar {} unreadable", self.path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(Self::default_book(chrono::Local::now().naive_local().date()))
            }
            Err(e) => Err(e).with_context(|| format!("demo calendar {}", self.path.display())),
        }
    }

    /// Write the whole file: a temporary file first, then renamed over it. Under [`DEMO_LOCK`].
    fn save(&self, book: &DemoBook) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("demo calendar folder {}", dir.display()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(book)?)
            .with_context(|| format!("demo calendar {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path).with_context(|| format!("demo calendar {}", self.path.display()))
    }

    /// Read, change and write the file back.
    fn change<T>(&self, f: impl FnOnce(&mut DemoBook) -> Result<T>) -> Result<T> {
        let _held = Self::hold();
        let mut book = self.load()?;
        let out = f(&mut book)?;
        self.save(&book)?;
        Ok(out)
    }

    fn read(&self) -> Result<DemoBook> {
        let _held = Self::hold();
        self.load()
    }

    /// Nothing between `start` and `end` apart from the event `own_id`.
    fn is_free_except(&self, start: &str, end: &str, own_id: &str) -> Result<bool> {
        let book = self.read()?;
        let clash = |s: &str, e: &str| s < end && start < e;
        let others = book.busy.iter().any(|(s, e)| clash(s, e));
        let written = book
            .events
            .iter()
            .any(|(id, s, e, _, _)| id != own_id && clash(s, e));
        Ok(!others && !written)
    }

    fn insert(&self, appt: &Appointment, caller_id: &str) -> Result<String> {
        let id = format!("demo-{}", uuid::Uuid::new_v4());
        let event = (
            id.clone(),
            appt.start.clone(),
            appt.end.clone(),
            appt.tag.clone(),
            appt.calendar_title(caller_id),
        );
        self.change(|b| {
            b.events.push(event);
            Ok(())
        })?;
        Ok(id)
    }

    fn move_to(&self, event_id: &str, appt: &Appointment) -> Result<()> {
        self.change(|b| {
            let e = b
                .events
                .iter_mut()
                .find(|e| e.0 == event_id)
                .context("no such event in the demo calendar")?;
            e.1 = appt.start.clone();
            e.2 = appt.end.clone();
            Ok(())
        })
    }

    fn delete(&self, event_id: &str) -> Result<()> {
        self.change(|b| {
            b.events.retain(|e| e.0 != event_id);
            Ok(())
        })
    }

    fn find_tagged(&self, tag: &str) -> Result<Vec<String>> {
        Ok(self
            .read()?
            .events
            .into_iter()
            .filter(|e| !tag.is_empty() && e.3 == tag)
            .map(|e| e.0)
            .collect())
    }
}

/// A calendar for the tests: busy times of others, the events written, and
/// how many inserts / moves / deletes happened.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct FakeCal {
    /// (id, start, end, tag)
    events: std::sync::Mutex<Vec<(String, String, String, String)>>,
    /// Other people's appointments: (start, end).
    busy: std::sync::Mutex<Vec<(String, String)>>,
    inserts: std::sync::atomic::AtomicUsize,
    moves: std::sync::atomic::AtomicUsize,
    deletes: std::sync::atomic::AtomicUsize,
    /// Writes take this long (to overlap two decisions).
    write_delay_ms: std::sync::atomic::AtomicU64,
    fail_delete: std::sync::atomic::AtomicBool,
    fail_insert: std::sync::atomic::AtomicBool,
}

#[cfg(test)]
impl FakeCal {
    fn count(n: &std::sync::atomic::AtomicUsize) -> usize {
        n.load(std::sync::atomic::Ordering::SeqCst)
    }

    async fn delay(&self) {
        let ms = self.write_delay_ms.load(std::sync::atomic::Ordering::SeqCst);
        if ms > 0 {
            tokio::time::sleep(Duration::from_millis(ms)).await;
        }
    }

    fn is_free_except(&self, start: &str, end: &str, own_id: &str) -> Result<bool> {
        let clash = |s: &str, e: &str| s < end && start < e;
        let others = self.busy.lock().unwrap().iter().any(|(s, e)| clash(s, e));
        let written = self
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|(id, s, e, _)| id != own_id && clash(s, e));
        Ok(!others && !written)
    }

    async fn insert(&self, appt: &Appointment) -> Result<String> {
        self.delay().await;
        if self.fail_insert.load(std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("日曆建立行程失敗（HTTP 503）");
        }
        let n = self.inserts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let id = format!("ev{n}");
        self.events
            .lock()
            .unwrap()
            .push((id.clone(), appt.start.clone(), appt.end.clone(), appt.tag.clone()));
        Ok(id)
    }

    async fn move_to(&self, event_id: &str, appt: &Appointment) -> Result<()> {
        self.delay().await;
        self.moves.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut ev = self.events.lock().unwrap();
        let e = ev
            .iter_mut()
            .find(|e| e.0 == event_id)
            .context("no such event")?;
        e.1 = appt.start.clone();
        e.2 = appt.end.clone();
        Ok(())
    }

    fn delete(&self, event_id: &str) -> Result<()> {
        if self.fail_delete.load(std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("日曆刪除行程失敗（HTTP 500）");
        }
        self.deletes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.events.lock().unwrap().retain(|e| e.0 != event_id);
        Ok(())
    }

    fn find_tagged(&self, tag: &str) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| !tag.is_empty() && e.3 == tag)
            .map(|e| e.0.clone())
            .collect()
    }
}

/// What an appointment's calendar entry says under its title: the caller's
/// number and name, what it is about (the `.ics` and "Add to calendar" too).
pub fn event_description(caller_id: &str, appt: &Appointment) -> String {
    let p = texts();
    let sep = p.sep;
    let mut parts = vec![format!("{}{sep}{}", p.n_number, display_caller(caller_id))];
    if !appt.caller_name.trim().is_empty() {
        parts.push(format!("{}{sep}{}", p.ap_name, appt.caller_name.trim()));
    }
    if !appt.title.trim().is_empty() {
        parts.push(format!("{}{sep}{}", p.ap_what, appt.title.trim()));
    }
    parts.push("Bageo".to_string());
    parts.join("\n")
}

/// What one call's Live session needs for appointments.
#[derive(Clone)]
pub struct ApptSession {
    /// [`Mode::Confirm`], or [`Mode::Auto`] for a test number (never off).
    pub mode: Mode,
    calendar: Option<Cal>,
    /// How the owner's answer will reach the caller unless the owner picks
    /// another way ([`default_reply`]): what the AI promises on the call.
    pub reply: Reply,
    /// [`Self::reply`] is a text message.
    pub reply_by_text: bool,
    /// Setting `appointment_sms`.
    sms_on: bool,
    /// Setting `appointment_reply`.
    reply_setting: String,
    /// The owner's country ([`home_region`]): how the caller ID reads until the
    /// SIM that took the call is known.
    home: Option<String>,
    caller_id: String,
    http: Client,
    /// This call's mark on the events it writes ([`Appointment::tag`]).
    tag: String,
    /// How long an `auto` booking waits for the calendar ([`CALENDAR_TURN_WAIT`]).
    turn_wait: Duration,
    /// What the caller agreed to.
    pub book: Arc<CallAppointments>,
}

impl ApptSession {
    /// None when appointments are off. A test number (setting
    /// `appointment_test_numbers`) always gets `auto` on the demo calendar;
    /// every other call `confirm`, with no calendar to look at.
    pub fn for_call(rt: &RuntimeSettings, http: &Client, caller_id: &str) -> Option<Self> {
        if rt.is_appointment_test_number(caller_id) {
            let demo = DemoCal::from_settings(rt);
            info!(
                "appointment test number: auto mode on the demo calendar path={}",
                demo.path.display()
            );
            return Some(Self::new(rt, http, caller_id, Mode::Auto, Some(Cal::Demo(demo))));
        }
        if Mode::parse(&rt.appointment_mode) == Mode::Off {
            return None;
        }
        Some(Self::new(rt, http, caller_id, Mode::Confirm, None))
    }

    fn new(rt: &RuntimeSettings, http: &Client, caller_id: &str, mode: Mode, calendar: Option<Cal>) -> Self {
        let home = home_region(rt);
        let reply = default_reply(&rt.appointment_reply, reply_options(rt, caller_id, home.as_deref()));
        Self {
            mode,
            calendar,
            reply,
            reply_by_text: reply == Reply::Sms,
            sms_on: rt.appointment_sms,
            reply_setting: rt.appointment_reply.clone(),
            home,
            caller_id: caller_id.trim().to_string(),
            http: http.clone(),
            tag: uuid::Uuid::new_v4().to_string(),
            turn_wait: CALENDAR_TURN_WAIT,
            book: CallAppointments::new(),
        }
    }

    /// The line that took the call ([`CallAppointments::set_line`]): the SIM's
    /// country decides how a caller ID without a country code is read, so
    /// whether the caller can be texted back. Kept on the call's book, which
    /// every copy of the session shares (the SIM may only be known at pickup).
    pub fn set_line(&mut self, sim_country: Option<String>, sim_sub_id: Option<i64>) {
        self.book.set_line(sim_country, sim_sub_id);
        let region = self.book.sim_country().or_else(|| self.home.clone());
        let opts = ReplyOptions {
            sms: self.sms_on && sms_number_on(&self.caller_id, region.as_deref()).is_some(),
            call: callable_on(&self.caller_id, region.as_deref()),
        };
        self.reply = default_reply(&self.reply_setting, opts);
        self.reply_by_text = self.reply == Reply::Sms;
    }

    pub fn can_check(&self) -> bool {
        self.calendar.is_some()
    }

    /// Added after the call's system instruction (the stored prompt is not touched).
    pub fn prompt_block(&self, now: chrono::DateTime<chrono::Local>) -> String {
        prompt_block(
            self.mode,
            self.can_check(),
            self.reply,
            &locale::active(),
            now.naive_local(),
            &now.format("%:z").to_string(),
        )
    }

    /// Answer one function call (runs on its own task; at most [`TOOL_WAIT`]).
    pub async fn run_tool(&self, call: &ToolCall) -> Value {
        let now = chrono::Local::now().naive_local();
        match call.name.as_str() {
            CHECK => self.check(&call.args, now).await,
            PROPOSE => {
                let seq = if call.seq == 0 { next_seq() } else { call.seq };
                self.propose(&call.args, now, seq, call.after).await
            }
            other => {
                warn!("appointment tool: unknown function {other}");
                json!({ "error": "unknown_function" })
            }
        }
    }

    /// The event of the appointment agreed last (it does not clash with itself).
    fn own_event_id(&self) -> String {
        self.book.booked_event().map(|a| a.event_id).unwrap_or_default()
    }

    /// The answer to a proposal that waits for the owner: not settled, and what
    /// the caller is told — checked and texted or called back later, or only
    /// "get back to you" when neither can reach them ([`check_back_line`]).
    fn pending_answer(&self) -> Value {
        json!({
            "status": "pending_owner_confirmation",
            "tell_caller": format!(
                "Not settled yet. If you have not said it already, say {}, in the language of the call; never say it is booked.",
                check_back_said(&locale::active(), self.reply)
            ),
        })
    }

    async fn check(&self, args: &Value, now: NaiveDateTime) -> Value {
        if let Some(e) = period_mismatch(args) {
            return e;
        }
        let (start, end) = match window(args, now) {
            Ok(w) => w,
            Err(why) => return bad_time(why),
        };
        if let Some(e) = weekday_mismatch(args, start) {
            return e;
        }
        let Some(cal) = &self.calendar else {
            return unavailable();
        };
        let own = self.own_event_id();
        match tokio::time::timeout(
            CHECK_TIMEOUT,
            cal.is_free_except(&self.http, &stamp(start), &stamp(end), &own),
        )
        .await
        {
            Ok(Ok(free)) => {
                info!("appointment check start={} end={} free={free}", stamp(start), stamp(end));
                json!({ "free": free })
            }
            Ok(Err(e)) => {
                warn!("appointment check failed: {e:#}");
                unavailable()
            }
            Err(_) => {
                // The caller is waiting: "I'll confirm" beats a long silence.
                warn!("appointment check timed out after {}s: answered unknown", CHECK_TIMEOUT.as_secs());
                json!({ "free": null, "reason": "calendar_slow" })
            }
        }
    }

    /// `seq`: the call's place in the order calls arrived ([`ToolCall::seq`]);
    /// `after`: the proposal sent just before it in the same message, which
    /// goes first ([`ToolCall::after`]).
    async fn propose(&self, args: &Value, now: NaiveDateTime, seq: u64, after: u64) -> Value {
        let _over = Finished(&self.book, seq);
        let pending = self.pending_answer();
        if let Some(e) = period_mismatch(args) {
            return e;
        }
        let (start, end) = match window(args, now) {
            Ok(w) => w,
            Err(why) => return bad_time(why),
        };
        if let Some(e) = weekday_mismatch(args, start) {
            return e;
        }
        if self.book.is_discarded() {
            return pending;
        }
        let _running = self.book.start_proposal();
        // Tasks start in any order: one sent after another in the same message
        // lets that one go first (it cannot see it otherwise — "change its
        // time" would change the wrong appointment). Not for ever: it may never come.
        if after != 0 && !self.book.wait_finished(after, AFTER_WAIT).await {
            warn!("appointment proposal seq={seq} went on without seq={after} (not over after {}s)", AFTER_WAIT.as_secs());
        }
        // The call's proposals one after the other; which one a proposal
        // changes is decided by arrival (`seq`), not by which got here first.
        let _turn = self.book.turns.lock().await;
        let replaces = args
            .get("replaces_previous")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let (start, end) = (stamp(start), stamp(end));
        let (target, own, tag) = {
            let b = self.book.book();
            let target = b.target(seq, replaces, &start, &end);
            let own = match target {
                Target::Change(i) => Some(b.slots[i].appt.clone()),
                _ => None,
            };
            (target, own, b.tag_for(target, &self.tag))
        };
        if target == Target::Stale {
            info!("appointment proposal seq={seq} start={start} superseded by a later one");
            return json!({ "status": "superseded" });
        }
        let mut appt = Appointment {
            start,
            end,
            title: clean_text(args.get("title"), TITLE_MAX),
            caller_name: clean_text(args.get("caller_name"), NAME_MAX),
            mode: self.mode.as_str().to_string(),
            status: PENDING.to_string(),
            tag,
            calendar: self.calendar.as_ref().map_or("", Cal::name).to_string(),
            ..Default::default()
        };
        if let (Mode::Auto, Some(cal)) = (self.mode, &self.calendar) {
            self.book_auto(cal, &mut appt, own.as_ref()).await;
        }
        let booked = appt.status == BOOKED;
        info!(
            "appointment proposed seq={seq} replaces={replaces} new={} start={} end={} mode={} booked={booked} auto_error={:?}",
            target == Target::New,
            appt.start,
            appt.end,
            appt.mode,
            appt.auto_error
        );
        if !self.book.put(target, appt, seq, replaces) {
            return pending;
        }
        if booked {
            json!({ "status": "booked" })
        } else if self.mode == Mode::Auto && self.calendar.is_some() {
            // It agreed already (auto) but the time could not be booked.
            json!({ "status": "pending_owner_confirmation", "not_booked": true })
        } else {
            pending
        }
    }

    /// `auto`: check for a clash (the event of the appointment it changes,
    /// `own`, does not count; this call's other appointments do), then move that
    /// event to the new time — or insert one when there is none yet. The same
    /// time as the event already booked writes nothing. When it cannot be booked
    /// the event stays where it is and the appointment keeps its id, so the
    /// owner's ✅ moves it instead of adding a second one.
    async fn book_auto(&self, cal: &Cal, appt: &mut Appointment, own: Option<&Appointment>) {
        let own = own.filter(|o| !o.event_id.is_empty());
        if let Some(o) = own.filter(|o| o.start == appt.start && o.end == appt.end) {
            appt.event_id = o.event_id.clone();
            appt.status = BOOKED.to_string();
            return;
        }
        let own_id = own.map(|o| o.event_id.clone()).unwrap_or_default();
        let fail = |appt: &mut Appointment, why: String| {
            appt.auto_error = why;
            if !own_id.is_empty() {
                appt.event_id = own_id.clone();
            }
        };
        // Another call may be booking the same hour: its check and write first
        // — but only so long; the end of the call does not wait for ever.
        let Ok(_turn) = tokio::time::timeout(self.turn_wait, calendar_turn().lock()).await else {
            warn!("appointment auto booking: the calendar was busy for {:?}, left to the owner", self.turn_wait);
            fail(appt, calendar_busy().to_string());
            return;
        };
        match tokio::time::timeout(
            BOOK_CHECK_TIMEOUT,
            cal.is_free_except(&self.http, &appt.start, &appt.end, &own_id),
        )
        .await
        {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => {
                fail(appt, texts().ap_busy.to_string());
                return;
            }
            Ok(Err(e)) => {
                fail(appt, format!("{e:#}"));
                return;
            }
            Err(_) => {
                fail(appt, calendar_slow(BOOK_CHECK_TIMEOUT.as_secs()));
                return;
            }
        }
        let draft = appt.clone();
        let write = async {
            if own_id.is_empty() {
                cal.insert(&self.http, &draft, &self.caller_id).await
            } else {
                cal.move_to(&self.http, &own_id, &draft).await.map(|()| own_id.clone())
            }
        };
        match tokio::time::timeout(WRITE_TIMEOUT, write).await {
            Ok(Ok(id)) => {
                appt.event_id = id;
                appt.status = BOOKED.to_string();
            }
            Ok(Err(e)) => fail(appt, format!("{e:#}")),
            Err(_) => {
                fail(appt, calendar_slow(WRITE_TIMEOUT.as_secs()));
                // Maybe written after all: a moved event keeps its id; a new one
                // is found by its tag when the owner confirms or it is voided.
                appt.write_unsure = own_id.is_empty();
            }
        }
    }
}

fn unavailable() -> Value {
    json!({ "free": null, "reason": "calendar_unavailable" })
}

/// The demo calendar did not answer within `secs`: why an `auto` booking or an
/// "accept" did not go through, in the owner's language.
fn calendar_slow(secs: u64) -> String {
    let s = match locale::notify_lang() {
        Lang::ZhTw => "日曆沒有及時回應（{} 秒）",
        Lang::ZhCn => "日历没有及时响应（{} 秒）",
        Lang::En => "the calendar did not answer in time ({} s)",
        Lang::Ja => "カレンダーが時間内に応答しませんでした（{} 秒）",
        Lang::Ko => "캘린더가 제때 응답하지 않았습니다({}초)",
        Lang::Es => "el calendario no respondió a tiempo ({} s)",
        Lang::Fr => "l'agenda n'a pas répondu à temps ({} s)",
        Lang::De => "der Kalender hat nicht rechtzeitig geantwortet ({} s)",
        Lang::Pt => "a agenda não respondeu a tempo ({} s)",
    };
    locale::fill(s, &secs.to_string())
}

/// The demo calendar was writing another booking the whole time an `auto`
/// booking could wait for it, in the owner's language.
fn calendar_busy() -> &'static str {
    match locale::notify_lang() {
        Lang::ZhTw => "日曆正在寫入另一筆預約",
        Lang::ZhCn => "日历正在写入另一条预约",
        Lang::En => "the calendar was busy with another booking",
        Lang::Ja => "カレンダーが別の予約を書き込み中でした",
        Lang::Ko => "캘린더가 다른 예약을 기록하는 중이었습니다",
        Lang::Es => "el calendario estaba guardando otra cita",
        Lang::Fr => "l'agenda enregistrait un autre rendez-vous",
        Lang::De => "der Kalender trug gerade einen anderen Termin ein",
        Lang::Pt => "a agenda estava gravando outro compromisso",
    }
}

fn bad_time(why: &str) -> Value {
    json!({ "error": why, "expected": "start as a future local time, YYYY-MM-DDTHH:MM" })
}

/// The weekday the model gave (`weekday`, optional) is not the start date's:
/// it worked the date out wrong ("Wednesday" as the 19th, a Thursday). The
/// answer names the date's real weekday so it corrects the date and calls
/// again. A missing or unknown weekday is not checked.
fn weekday_mismatch(args: &Value, start: NaiveDateTime) -> Option<Value> {
    let said = args
        .get("weekday")
        .and_then(Value::as_str)?
        .trim()
        .parse::<chrono::Weekday>()
        .ok()?;
    (said != start.weekday()).then(|| {
        json!({
            "error": "weekday_mismatch",
            "date_is": start.format("%A").to_string(),
            "expected": "the date of the weekday the caller means (see the list of dates in your instructions); call again with it",
        })
    })
}

fn clean_text(v: Option<&Value>, max: usize) -> String {
    v.and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

/// The `period` parameter of both functions.
const PERIOD_HELP: &str = "The part of the day the caller named, when they named one: morning, afternoon, evening or night. Checked against the hour of start.";

/// A part of the day the caller named ("tonight", 早上, 午後…), sent as
/// `period` so a time on the wrong side of the clock is caught: "eight
/// tonight" sent as 08:00, "three this afternoon" as 03:00.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Period {
    Morning,
    Afternoon,
    Evening,
    Night,
}

impl Period {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "morning" => Some(Self::Morning),
            "afternoon" | "noon" => Some(Self::Afternoon),
            "evening" | "tonight" => Some(Self::Evening),
            "night" => Some(Self::Night),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Morning => "morning",
            Self::Afternoon => "afternoon",
            Self::Evening => "evening",
            Self::Night => "night",
        }
    }

    /// The start hours that fit. Generous and overlapping: languages and people
    /// draw the lines differently, and only a twelve-hour slip is to be caught.
    fn hours(self) -> (u32, u32) {
        match self {
            Self::Morning => (4, 12),
            Self::Afternoon => (11, 18),
            Self::Evening => (16, 23),
            Self::Night => (18, 4),
        }
    }

    fn fits(self, hour: u32) -> bool {
        let (from, to) = self.hours();
        if from <= to {
            (from..=to).contains(&hour)
        } else {
            hour >= from || hour <= to
        }
    }
}

/// The start hour does not fit the part of the day the model said the caller
/// named (`period`): it is sent back, with the same time on the other half of
/// the clock when that fits, so the model corrects it without a word to the
/// caller. A missing or unknown period, or a start that does not parse (the
/// window check answers that), is not checked.
fn period_mismatch(args: &Value) -> Option<Value> {
    let period = Period::parse(args.get("period")?.as_str()?)?;
    let start = args.get("start").and_then(Value::as_str).and_then(parse_local)?;
    if period.fits(start.hour()) {
        return None;
    }
    let (from, to) = period.hours();
    let other = (start.hour() + 12) % 24;
    let fix = if period.fits(other) {
        format!("Did you mean {other:02}:{:02}? ", start.minute())
    } else {
        String::new()
    };
    Some(json!({
        "error": "period_mismatch",
        "hint": format!(
            "start is {}, which is not in the {} ({from:02}:00–{to:02}:59). {fix}Correct start and call again; say nothing about it to the caller.",
            start.format("%H:%M"),
            period.name(),
        ),
    }))
}

/// The time a function call names: a start that is neither in the past nor more
/// than 400 days ahead; a missing or odd end is one hour after the start.
fn window(
    args: &Value,
    now: NaiveDateTime,
) -> std::result::Result<(NaiveDateTime, NaiveDateTime), &'static str> {
    let start = args
        .get("start")
        .and_then(Value::as_str)
        .and_then(parse_local)
        .ok_or("bad_start")?;
    if start < now - Span::minutes(30) {
        return Err("start_in_past");
    }
    if start > now + Span::days(400) {
        return Err("start_too_far");
    }
    let end = args
        .get("end")
        .and_then(Value::as_str)
        .and_then(parse_local)
        .filter(|e| *e > start && *e <= start + Span::hours(24))
        .unwrap_or(start + Span::hours(1));
    Ok((start, end))
}

/// The line the AI says once a caller names a time that is not settled on the
/// call (the owner's wording; English, said in the call's language), by how
/// the answer will reach them ([`default_reply`]): checked and texted back
/// later, checked and called back later, or — neither can reach this caller —
/// only checked and got back to, with nothing promised. `me`: answering as the
/// owner (else as their assistant, checking with `owner`).
pub fn check_back_line(me: bool, owner: &str, reply: Reply) -> String {
    match (me, reply) {
        (true, Reply::Sms) => "Let me check and I'll text you back later.".to_string(),
        (true, Reply::Call) => "Let me check and I'll call you back later.".to_string(),
        (true, Reply::None) => "Let me check and get back to you.".to_string(),
        (false, Reply::Sms) => format!("I'll check with {owner} and text you back later."),
        (false, Reply::Call) => format!("I'll check with {owner} and call you back later."),
        (false, Reply::None) => format!("I'll check with {owner} and get back to you."),
    }
}

/// [`check_back_line`] in quotes, with the owner's own Chinese wording when
/// the call is in Chinese ("我確認一下，晚點再用簡訊回你" / "我確認一下，晚點再打給你"
/// / "我確認一下再回你").
fn check_back_said(active: &locale::Active, reply: Reply) -> String {
    let owner = crate::prompts::owner_label(active.lang, &active.owner);
    let me = !active.assistant;
    let en = check_back_line(me, &owner, reply);
    let (who, tail) = match (active.lang, reply) {
        (Lang::ZhTw, Reply::Sms) => ("確認一下", "，晚點再用簡訊回你"),
        (Lang::ZhTw, Reply::Call) => ("確認一下", "，晚點再打給你"),
        (Lang::ZhTw, Reply::None) => ("確認一下", "再回你"),
        (Lang::ZhCn, Reply::Sms) => ("确认一下", "，晚点再用短信回你"),
        (Lang::ZhCn, Reply::Call) => ("确认一下", "，晚点再打给你"),
        (Lang::ZhCn, Reply::None) => ("确认一下", "再回你"),
        _ => return format!("\"{en}\""),
    };
    let zh = if me { format!("我{who}{tail}") } else { format!("我跟{owner}{who}{tail}") };
    format!("\"{en}\" (in Chinese: 「{zh}」)")
}

/// The instructions added after a call's system prompt (English; the model
/// answers in the call's language). Coming last, they win over a prompt line
/// such as "do not agree to plans for the owner".
pub fn prompt_block(
    mode: Mode,
    can_check: bool,
    reply: Reply,
    active: &locale::Active,
    now: NaiveDateTime,
    utc_offset: &str,
) -> String {
    let owner = crate::prompts::owner_label(active.lang, &active.owner);
    let language = locale::phrases(active.lang).name_en;
    let me = !active.assistant;
    // What the AI says for a time that is not settled on the call (the owner's
    // wording), by how the answer will reach the caller: a text, a call-back,
    // or — neither can — only "get back to you".
    let said = check_back_said(active, reply);
    let back = match reply {
        Reply::Sms => "text them back later",
        Reply::Call => "call them back later",
        Reply::None => "get back to them",
    };
    let has_plans = if me {
        "you already have something on then".to_string()
    } else {
        format!("{owner} already has something on then")
    };
    let mut s = format!(
        "\n\n[Appointments — this part overrides anything above about not agreeing to plans or invitations for {owner}]\n"
    );
    // Today, tomorrow and the day after are named: the model got "tomorrow" a day
    // early when it had to count from "now" itself (real calls at 07:00).
    let days: Vec<String> = (0..=14)
        .map(|d| {
            let day = (now.date() + Span::days(d)).format("%a %Y-%m-%d").to_string();
            match d {
                0 => format!("today = {day}"),
                1 => format!("tomorrow = {day}"),
                2 => format!("the day after tomorrow = {day}"),
                _ => day,
            }
        })
        .collect();
    // This week and next week, Monday first: "Wednesday next week" (下禮拜三)
    // is looked up, not counted.
    let monday = now.date() - Span::days(i64::from(now.weekday().num_days_from_monday()));
    let week = |from: i64| -> String {
        (from..from + 7)
            .map(|d| (monday + Span::days(d)).format("%a %Y-%m-%d").to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    s.push_str(&format!(
        "It is now {} ({}), local time (UTC{utc_offset}). Dates: {}. This week's Mon..Sun: {}. Next week's Mon..Sun: {}. Take \"today\", \"tomorrow\", \"the day after tomorrow\" (in any language: 今天／明天／後天, 明日／明後日, 내일／모레, mañana / pasado mañana…), \"Wednesday\" or \"the 20th\" from these lists; \"Wednesday next week\" (下禮拜三／下週三／来週の水曜日／다음 주 수요일) is next week's Wed and \"this Wednesday\" (這禮拜三／今週の水曜日) this week's; never count days yourself. All times you pass to the functions are local, written YYYY-MM-DDTHH:MM; also pass the weekday you mean, which is checked against the date, and the part of the day as period when the caller named one (早上／上午 = morning, 下午 = afternoon, 傍晚／晚上／今晚 = evening, 半夜／凌晨 = night; 朝／午後／夜, 아침／오후／저녁／밤 alike), which is checked against the hour.\n",
        now.format("%Y-%m-%d %H:%M"),
        now.format("%A"),
        days.join(", "),
        week(0),
        week(7),
    ));
    s.push_str(&format!(
        "When the caller asks whether {owner} is free at some time, or wants to set a time (a meeting, a visit, coming by, a call at a set time):\n"
    ));
    s.push_str("- Never make the caller wait on the line and never say you are looking something up right now, in any language (nothing like \"one moment\", \"hold on\", \"let me look\", \"ちょっと待って\", \"確認するから待って\", \"等一下\"): call the function silently and carry on with the conversation. Saying that you will check it after the call and get back to them is something else: that is exactly what you say below.\n");
    if can_check {
        s.push_str("- As soon as there is a day and a time, call check_availability (start, weekday, and end if they said how long) before you say anything about whether it works. Only a vague time (\"next week\", \"some afternoon\")? Ask which day and what time first.\n");
        s.push_str(&format!(
            "- {{\"free\": false}}: say {has_plans}, never what it is. Only if they ask, suggest another time (you may check that one too).\n"
        ));
        if mode == Mode::Auto {
            s.push_str("- {\"free\": true}: agree naturally, then call propose_appointment.\n");
        } else {
            s.push_str(&format!(
                "- {{\"free\": true}}: do not say it is settled; say {said}, in {language}, then call propose_appointment.\n"
            ));
        }
        s.push_str(&format!(
            "- {{\"free\": null}} (the calendar could not be read): you do not know whether {owner} is free, so do not say it is free or that it should work; say {said}, in {language}, then call propose_appointment.\n"
        ));
    } else {
        s.push_str(&format!(
            "- You cannot see {owner}'s schedule on this call, so do not call check_availability, and you do not know whether {owner} is free: do not say it is free, that it should work or that it is settled. Once there is a day and a time, say {said} — in {language}, with the same meaning — then call propose_appointment. Only a vague time? Ask which day and what time first.\n"
        ));
    }
    match reply {
        Reply::Sms => {}
        Reply::Call => s.push_str("- The answer reaches this caller by a call-back: never promise a text message.\n"),
        Reply::None => s.push_str("- Neither a text message nor a call-back can reach this caller: never promise to text or call them; only say that you will get back to them.\n"),
    }
    s.push_str(&format!(
        "- propose_appointment: start (and end) as agreed, weekday, period, title = what it is about in a few words in {language}, caller_name only if they said their name (otherwise leave it out). Call it once for each appointment you agree on. If the two of you really change the time of the appointment agreed last, call it again with replaces_previous = true (the new time replaces it). If the caller sets up another, separate appointment (another matter or another day, and both stand), call it with replaces_previous = false. A back-up time the caller offers (\"if not, the afternoon works too\") or another time named in passing is not a change: do not call it for that.\n"
    ));
    s.push_str(&format!(
        "- Its answer \"pending_owner_confirmation\" means it is not settled yet: if you have not said it already, say {said}; never say it is booked. If it also has \"not_booked\": true, you had already agreed but it could not go into the calendar: briefly tell the caller you still need to check and will {back}. \"booked\" means it is settled and you may say so.\n"
    ));
    s.push_str("- {\"error\": \"weekday_mismatch\"}: the date you sent is another weekday (\"date_is\" says which). Take the right date from the list above and call again; say nothing about it to the caller.\n");
    s.push_str("- {\"error\": \"period_mismatch\"}: the hour you sent is not in the part of the day the caller named (\"hint\" says what to fix, e.g. 20:00 for eight in the evening). Correct start and call again; say nothing about it to the caller.\n");
    s.push_str("- After calling propose_appointment do not repeat what you have just told the caller: if you already said it, simply go on with the conversation.\n");
    s.push_str("- Never make an appointment with a sales, insurance, loan, investment or scam caller.");
    s
}

// ---------------------------------------------------------------------------
// Text messages to the caller
// ---------------------------------------------------------------------------

/// [`sms_number_on`] for a call whose SIM is not known: a number without a
/// country code is read as Taiwanese (`0912 345 678` → `+886912345678`).
pub fn sms_number(caller_id: &str) -> Option<String> {
    sms_number_on(caller_id, None)
}

/// The owner's country (ISO 3166 alpha-2) from the country calling code setting
/// (`country_code`; Taiwan's for zh-TW): how a caller ID without a country code
/// reads when no SIM country is known, as everywhere else in the agent.
fn home_region(rt: &RuntimeSettings) -> Option<String> {
    crate::phone_region::country_of_code(&rt.effective_country_code()).map(str::to_string)
}

/// How a caller ID without a country code reads for this appointment: the SIM
/// that took the call, else the SIM that phone (`serial`) reported last, else
/// the owner's country.
fn caller_region(rt: &RuntimeSettings, appt: &Appointment, serial: &str) -> Option<String> {
    appt.sim_country
        .clone()
        .or_else(|| crate::phone_region::last_sim(serial))
        .or_else(|| home_region(rt))
}

/// Countries whose mobile numbers can be told from their landlines: national
/// prefixes and the national number's length. A number of any other country is
/// taken when its length is plausible; +1 numbers (the United States, Canada…)
/// always are — mobile and landline look alike there, and Google Voice numbers
/// take texts.
const MOBILE_NUMBERS: &[(&str, &[&str], usize, usize)] = &[
    ("tw", &["9"], 9, 9),
    ("jp", &["70", "80", "90"], 10, 10),
    ("kr", &["10"], 10, 10),
    ("cn", &["13", "14", "15", "16", "17", "18", "19"], 11, 11),
    ("hk", &["4", "5", "6", "7", "9"], 8, 8),
    ("mo", &["6"], 8, 8),
    ("sg", &["8", "9"], 8, 8),
    ("my", &["1"], 9, 10),
    ("th", &["6", "8", "9"], 9, 9),
    ("vn", &["3", "5", "7", "8", "9"], 9, 9),
    ("ph", &["9"], 10, 10),
    ("id", &["8"], 9, 12),
    ("in", &["6", "7", "8", "9"], 10, 10),
    ("au", &["4"], 9, 9),
    ("nz", &["2"], 8, 10),
    ("gb", &["7"], 10, 10),
    ("de", &["15", "16", "17"], 10, 11),
    ("fr", &["6", "7"], 9, 9),
    ("es", &["6", "7"], 9, 9),
    ("it", &["3"], 9, 10),
    ("nl", &["6"], 9, 9),
];

/// Calling codes whose numbers are dialled at home without a trunk `0`.
const NO_TRUNK_CODES: &[&str] = &["1", "39"];

/// +1 toll-free (800, 833…888) and premium (900) codes: business lines, not
/// phones a text reaches.
const NANP_NO_TEXT: &[&str] = &["800", "833", "844", "855", "866", "877", "888", "900"];

fn takes_texts(country: &str, code: &str, national: &str) -> bool {
    if code == "1" {
        return national.len() == 10 && !NANP_NO_TEXT.contains(&&national[..3]);
    }
    match MOBILE_NUMBERS.iter().find(|(c, ..)| *c == country) {
        Some((_, prefixes, min, max)) => {
            (*min..=*max).contains(&national.len()) && prefixes.iter().any(|p| national.starts_with(p))
        }
        None => national.len() >= 7 && code.len() + national.len() <= 15,
    }
}

/// The number to text a caller back on, in E.164 (`+886912345678`), or None:
/// hidden numbers and landlines (the owner calls them back). `sim_country`:
/// the SIM that took the call (ISO 3166 alpha-2), which reads a caller ID
/// without a country code (`2025550143` on a US SIM is `+12025550143`); None
/// reads it as Taiwanese. Taken: Taiwan mobiles, every +1 number (Google Voice
/// too), and the mobile numbers of other countries ([`MOBILE_NUMBERS`]; where
/// mobiles cannot be told apart, any number of a plausible length).
pub fn sms_number_on(caller_id: &str, sim_country: Option<&str>) -> Option<String> {
    let t = caller_id.trim();
    if t.is_empty()
        || !t
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | ' ' | '-' | '(' | ')' | '.'))
    {
        return None;
    }
    let digits: String = t.chars().filter(char::is_ascii_digit).collect();
    let region = sim_country
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_SMS_REGION)
        .to_ascii_lowercase();
    if !t.starts_with('+') && !digits.starts_with("00") {
        // Dialled at home: caller IDs carry the trunk 0 (`0912…`), or the
        // country code without its plus (`886912…`). Bare national digits are
        // a cut-off number, not one to text.
        let code = crate::phone_region::calling_code(&region)?;
        let with_code = digits.starts_with(code) && digits.len() >= code.len() + 8;
        if !NO_TRUNK_CODES.contains(&code) && !digits.starts_with('0') && !with_code {
            return None;
        }
    }
    let n = crate::phone_region::normalize(t, Some(&region))?;
    let code = crate::phone_region::calling_code(n.country)?;
    let national = n.e164.strip_prefix('+')?.strip_prefix(code)?;
    // `+886 0912…`: the trunk 0 written after the country code (Italy keeps its 0).
    let national = if code == "39" { national } else { national.trim_start_matches('0') };
    takes_texts(n.country, code, national).then(|| format!("+{code}{national}"))
}

/// The text message to the caller, in the owner's own words: the time is fine
/// (`confirmed`) or not. Only the time (with its weekday): the title came from
/// the caller's own words and is never sent back from the owner's number (it
/// could carry a link or a chat ID). No owner name set: the greeting goes without it.
pub fn sms_text(lang: Lang, owner: &str, appt: &Appointment, confirmed: bool) -> String {
    let p = locale::phrases(lang);
    let owner: String = owner
        .chars()
        .filter(|c| !c.is_control() && *c != '{' && *c != '}')
        .take(NAME_MAX)
        .collect();
    let owner = owner.trim();
    let hi = if owner.is_empty() {
        p.ap_sms_hi.to_string()
    } else {
        locale::fill(p.ap_sms_hi_owner, owner)
    };
    let when = appt.start_text(lang);
    let body = if confirmed {
        locale::fill(p.ap_sms_ok, &when)
    } else {
        locale::fill(p.ap_sms_no, &when)
    };
    format!("{hi}{body}")
        .trim()
        .chars()
        .take(SMS_TEXT_MAX)
        .collect()
}

/// [`Appointment::sms`] code and the line the owner reads.
fn sms_report(out: &SmsOutcome, shown_number: &str) -> (String, String) {
    let p = texts();
    let not_sent = |why: &str| locale::fill(p.ap_sms_not_sent, why);
    match out {
        SmsOutcome::Sent => ("sent".to_string(), locale::fill(p.ap_sms_sent, shown_number)),
        SmsOutcome::Offline => ("offline".to_string(), not_sent(p.ap_sms_offline)),
        SmsOutcome::Unsupported => ("old_app".to_string(), not_sent(p.ap_sms_old_app)),
        SmsOutcome::Timeout => ("timeout".to_string(), not_sent(p.ap_sms_timeout)),
        SmsOutcome::Busy => ("busy".to_string(), not_sent(p.ap_sms_busy)),
        // The phone's code in words; only one this agent does not know as it came.
        SmsOutcome::Failed(code) => (
            code.clone(),
            not_sent(&sms_code_text(locale::notify_lang(), code).map_or_else(|| locale::fill(p.ap_sms_code, code), str::to_string)),
        ),
    }
}

/// A test number's appointment (setting `appointment_test_numbers`): on the
/// demo calendar, or from a number on the list.
fn is_test_call(rt: &RuntimeSettings, task: &TaskRecord, appt: &Appointment) -> bool {
    appt.calendar == DEMO_CALENDAR || rt.is_appointment_test_number(&task.caller_id)
}

/// The phone the setting `appointment_test_sms_phone` names: a connected
/// phone's id (any case), else the phone whose `device_labels` name it is (any
/// case; a connected one first), else the setting as written (a phone id that
/// is not connected now).
fn named_phone(name: &str, labels: &std::collections::BTreeMap<String, String>, connected: &[String]) -> String {
    let name = name.trim();
    if let Some(s) = connected.iter().find(|s| s.eq_ignore_ascii_case(name)) {
        return s.clone();
    }
    let wanted = name.to_lowercase();
    let labelled: Vec<&String> = labels
        .iter()
        .filter(|(_, label)| label.trim().to_lowercase() == wanted)
        .map(|(id, _)| id)
        .collect();
    labelled
        .iter()
        .find(|id| connected.iter().any(|s| s.eq_ignore_ascii_case(id)))
        .or_else(|| labelled.first())
        .map(|id| id.to_string())
        .unwrap_or_else(|| name.to_string())
}

/// Where a test call's text goes out (settings `appointment_test_sms_phone` and
/// `appointment_test_sms_sim`): that phone, or the one that took the call; the
/// SIM by name, else that phone's default SIM, else the SIM that took the call.
/// `connected`: the phones connected now ([`ControlRegistry::connected_serials`]).
fn test_text_route(rt: &RuntimeSettings, connected: &[String], task: &TaskRecord, appt: &Appointment) -> (String, SmsSim) {
    let phone_set = rt.appointment_test_sms_phone.trim();
    let sim_set = rt.appointment_test_sms_sim.trim();
    let phone = if phone_set.is_empty() {
        task.serial.clone()
    } else {
        named_phone(phone_set, &rt.device_labels, connected)
    };
    let sim = if !sim_set.is_empty() {
        SmsSim::Named(sim_set.to_string())
    } else if !phone_set.is_empty() {
        SmsSim::Default
    } else {
        SmsSim::from_sub_id(appt.sim_sub_id)
    };
    (phone, sim)
}

/// The phone and SIM a text goes out from when the owner picks no phone: for a
/// test call ([`is_test_call`]) those its settings name ([`test_text_route`]),
/// else the phone and SIM that took the call.
fn default_text_route(rt: &RuntimeSettings, connected: &[String], task: &TaskRecord, appt: &Appointment) -> (String, SmsSim) {
    if is_test_call(rt, task, appt) {
        test_text_route(rt, connected, task, appt)
    } else {
        (task.serial.clone(), SmsSim::from_sub_id(appt.sim_sub_id))
    }
}

/// Where an appointment's text goes out when the owner picks no phone, as the
/// web console shows it (`sms_route` on `GET /calls/api/tasks`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SmsRoute {
    /// The phone's id.
    pub serial: String,
    /// The phone's name in `device_labels`, else its id.
    pub label: String,
    /// The SIM the phone looks up by name (`appointment_test_sms_sim`); left
    /// out when the phone uses the SIM that took the call or its default SIM.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub sim: String,
    /// A test call whose phone or SIM the test text settings name.
    pub test: bool,
}

/// The route of [`default_text_route`] for a task's appointment; None for a
/// task without an appointment or a phone. The phone need not be connected.
pub fn sms_route(rt: &RuntimeSettings, connected: &[String], task: &TaskRecord) -> Option<SmsRoute> {
    let appt = task.appointment.as_ref()?;
    let (serial, sim) = default_text_route(rt, connected, task, appt);
    let serial = serial.trim().to_string();
    if serial.is_empty() {
        return None;
    }
    let label = rt
        .device_labels
        .iter()
        .find(|(id, _)| id.eq_ignore_ascii_case(&serial))
        .map(|(_, label)| label.trim())
        .filter(|label| !label.is_empty())
        .unwrap_or(&serial)
        .to_string();
    let sim = match sim {
        SmsSim::Named(name) => name.trim().to_string(),
        SmsSim::Default | SmsSim::SubId(_) => String::new(),
    };
    let set = !rt.appointment_test_sms_phone.trim().is_empty() || !rt.appointment_test_sms_sim.trim().is_empty();
    let test = set && is_test_call(rt, task, appt);
    Some(SmsRoute { serial, label, sim, test })
}

/// A task as `GET /calls/api/tasks` lists it: an appointment's task with its
/// [`SmsRoute`] as `sms_route`.
pub fn task_json(rt: &RuntimeSettings, connected: &[String], task: &TaskRecord) -> Value {
    let mut v = serde_json::to_value(task).unwrap_or(Value::Null);
    if let (Some(route), Some(obj)) = (sms_route(rt, connected, task), v.as_object_mut()) {
        obj.insert("sms_route".into(), json!(route));
    }
    v
}

/// Why a text to the caller did not go out ([`Appointment::sms`]: the agent's
/// own codes and the phone's) in the owner's words, in `lang`. None for a code
/// this agent does not know, which is then shown as it came.
pub fn sms_code_text(lang: Lang, code: &str) -> Option<&'static str> {
    let p = locale::phrases(lang);
    let code = code.trim();
    let own = match code {
        "offline" => Some(p.ap_sms_offline),
        "old_app" => Some(p.ap_sms_old_app),
        "timeout" => Some(p.ap_sms_timeout),
        "busy" => Some(p.ap_sms_busy),
        "no_number" => Some(p.ap_sms_cannot),
        "not_test_number" => Some(p.ap_sms_not_test),
        _ => None,
    };
    own.or_else(|| phone_sms_code(lang, code))
}

/// The codes a phone answers a text with ([`crate::callback::SMS_ERROR_CODES`];
/// `no_sim`: no SIM on the phone, or more than one, matched the one asked for).
fn phone_sms_code(lang: Lang, code: &str) -> Option<&'static str> {
    const CODES: [&str; 7] = ["no_permission", "bad_request", "no_service", "radio_off", "generic_failure", "exception", "no_sim"];
    let words: [&'static str; 7] = match lang {
        Lang::ZhTw => ["手機 App 沒有傳簡訊的權限", "號碼或內容不被接受", "手機沒有訊號", "手機的行動網路關閉（飛航模式）", "電信端沒有收下這則簡訊", "手機 App 發生錯誤", "手機上找不到設定裡指定的那張 SIM（或符合的不只一張）"],
        Lang::ZhCn => ["手机 App 没有发短信的权限", "号码或内容不被接受", "手机没有信号", "手机的移动网络关闭（飞行模式）", "运营商没有接收这条短信", "手机 App 出错了", "手机上找不到设置里指定的那张 SIM（或符合的不止一张）"],
        Lang::En => ["the phone app may not send texts", "the number or the text was not accepted", "no mobile signal", "the phone's radio is off (airplane mode)", "the carrier did not take the text", "the phone app ran into an error", "the phone has no SIM matching the one in settings (or more than one)"],
        Lang::Ja => ["アプリに SMS 送信の権限がありません", "番号か本文が受け付けられませんでした", "圏外です", "機内モードなどで電波がオフです", "通信事業者が SMS を受け付けませんでした", "スマホのアプリでエラーが起きました", "設定で指定した SIM がスマホに見つかりません（または複数あります）"],
        Lang::Ko => ["앱에 문자 전송 권한이 없음", "번호나 내용이 받아들여지지 않음", "휴대폰 신호 없음", "비행기 모드 등으로 전파가 꺼짐", "통신사가 문자를 받지 않음", "휴대폰 앱에서 오류 발생", "설정에서 지정한 SIM을 휴대폰에서 찾을 수 없음(또는 여러 개)"],
        Lang::Es => ["la app no tiene permiso para enviar SMS", "no se aceptó el número o el texto", "sin cobertura", "la radio del teléfono está apagada (modo avión)", "el operador no aceptó el SMS", "la app del teléfono tuvo un error", "el teléfono no tiene la SIM indicada en los ajustes (o tiene más de una)"],
        Lang::Fr => ["l'application n'a pas le droit d'envoyer des SMS", "le numéro ou le texte n'a pas été accepté", "pas de réseau mobile", "la radio du téléphone est coupée (mode avion)", "l'opérateur n'a pas accepté le SMS", "l'application du téléphone a rencontré une erreur", "le téléphone n'a pas la SIM indiquée dans les réglages (ou en a plusieurs)"],
        Lang::De => ["die App darf keine SMS senden", "Nummer oder Text wurde nicht angenommen", "kein Mobilfunknetz", "Funk am Telefon aus (Flugmodus)", "der Netzbetreiber hat die SMS nicht angenommen", "in der App auf dem Telefon ist ein Fehler aufgetreten", "das Telefon hat keine SIM, die zur Einstellung passt (oder mehrere)"],
        Lang::Pt => ["o app não tem permissão para enviar SMS", "o número ou o texto não foi aceito", "sem sinal de celular", "o rádio do telefone está desligado (modo avião)", "a operadora não aceitou o SMS", "o app do telefone teve um erro", "o telefone não tem o SIM indicado nas configurações (ou tem mais de um)"],
    };
    CODES.iter().position(|c| *c == code).map(|i| words[i])
}

/// The link that adds an appointment to the owner's calendar (setting
/// `calendar_app`): that calendar's "new event" form filled in, or the task's
/// `.ics` at the console's address (empty without `public_url`).
pub fn calendar_link(rt: &RuntimeSettings, task: &TaskRecord, appt: &Appointment) -> String {
    let ics = crate::calendar::ics_url(&rt.public_url, &format!("tasks/{}", task.id));
    appt.calendar_event()
        .link_for(&rt.calendar_app, &event_description(&task.caller_id, appt), &ics)
}

/// Send the caller the text for this decision through the phone that took the
/// call (or `serial`, another phone the owner picked). A test call
/// ([`is_test_call`]) texts only test numbers, from the phone and SIM its
/// settings name ([`default_text_route`]) unless the owner picked another phone.
/// Returns the [`Appointment::sms`] code and the owner's line.
async fn text_caller(
    rt: &RuntimeSettings,
    control: Option<&ControlRegistry>,
    task: &TaskRecord,
    appt: &Appointment,
    confirmed: bool,
    serial: Option<&str>,
) -> (String, String) {
    let p = texts();
    let region = caller_region(rt, appt, &task.serial);
    let Some(to) = sms_number_on(&task.caller_id, region.as_deref()) else {
        return ("no_number".to_string(), p.ap_sms_cannot.to_string());
    };
    let test_call = is_test_call(rt, task, appt);
    // `to` is the caller's number with its country code added (`sms_number_on`);
    // the list may have it either way.
    let listed = rt.is_appointment_test_number(&to) || rt.is_appointment_test_number(&task.caller_id);
    if test_call && !listed {
        info!("appointment sms task_id={} not sent: a test call to a number off the test list", task.id);
        return (
            "not_test_number".to_string(),
            locale::fill(p.ap_sms_not_sent, p.ap_sms_not_test),
        );
    }
    let Some(control) = control else {
        return sms_report(&SmsOutcome::Offline, &to);
    };
    let (phone, sim) = match serial.map(str::trim).filter(|s| !s.is_empty()) {
        Some(picked) => {
            let route = if test_call {
                Some(test_text_route(rt, &control.connected_serials().await, task, appt))
            } else {
                None
            };
            match route {
                // The owner picked the test phone itself: the SIM the settings name.
                Some((test_phone, sim)) if test_phone.eq_ignore_ascii_case(picked) => (picked.to_string(), sim),
                // The SIM that took the call; another phone sends from its own default.
                _ => {
                    let sim = if picked == task.serial { SmsSim::from_sub_id(appt.sim_sub_id) } else { SmsSim::Default };
                    (picked.to_string(), sim)
                }
            }
        }
        None => default_text_route(rt, &control.connected_serials().await, task, appt),
    };
    // To the caller: in the language the AI spoke with them.
    let text = sms_text(locale::current(), &rt.owner_name, appt, confirmed);
    let out = control.send_sms_from(&phone, &sim, &task.id, &to, &text, SMS_RESULT_WAIT).await;
    info!(
        "appointment sms task_id={} serial={phone} sim={} test_call={test_call} outcome={out:?}",
        task.id,
        sim.describe()
    );
    sms_report(&out, task.caller_id.trim())
}

/// What the owner reads about the way an answer reaches the caller, in each
/// language ([`Reply`]).
struct ReplyWords {
    /// The label on the Telegram card.
    label: &'static str,
    /// The AI will call them back once it is decided.
    call_will: &'static str,
    /// A hidden number: nobody can be told.
    hidden: &'static str,
    /// Nobody was told (the owner's choice).
    none: &'static str,
    /// The call-back is being placed; `{}` = the number.
    dialing: &'static str,
    /// It could not be placed; `{}` = why.
    not_placed: &'static str,
    /// Why: no phone connected that can call.
    no_phone: &'static str,
    /// Why: the phone could not be asked to call.
    send_failed: &'static str,
}

fn reply_words(lang: Lang) -> ReplyWords {
    let w = |label, call_will, hidden, none, dialing, not_placed, no_phone, send_failed| ReplyWords {
        label,
        call_will,
        hidden,
        none,
        dialing,
        not_placed,
        no_phone,
        send_failed,
    };
    match lang {
        Lang::ZhTw => w("回覆方式", "按同意或不同意後，AI 會打電話告訴對方", "未顯示號碼，沒辦法通知對方", "沒有通知對方", "AI 正在回電給 {}", "AI 沒有回電：{}", "沒有可以撥號的手機連線", "手機沒收到撥號指令"),
        Lang::ZhCn => w("回复方式", "按同意或不同意后，AI 会打电话告诉对方", "未显示号码，没办法通知对方", "没有通知对方", "AI 正在回拨给 {}", "AI 没有回拨：{}", "没有可以拨号的手机连接", "手机没收到拨号指令"),
        Lang::En => w("Reply", "the AI calls them back once you tap Accept or Decline", "hidden number: the caller can't be told", "the caller was not told", "The AI is calling {} back", "No call-back: {}", "no phone is connected to place the call", "the phone did not get the request to call"),
        Lang::Ja => w("返事の方法", "承諾またはお断りの後、AI が電話で伝えます", "非通知のため相手に伝えられません", "相手には伝えていません", "AI が {} に折り返し電話しています", "折り返し電話をかけていません：{}", "電話をかけられるスマホが接続されていません", "スマホに発信の指示が届きませんでした"),
        Lang::Ko => w("회신 방법", "수락하거나 거절하면 AI가 전화로 알려 줍니다", "발신번호 표시 제한이라 알릴 수 없습니다", "상대에게 알리지 않았습니다", "AI가 {}에게 다시 전화하는 중입니다", "다시 전화하지 않았습니다: {}", "전화를 걸 수 있는 휴대폰이 연결되어 있지 않음", "휴대폰이 발신 요청을 받지 못함"),
        Lang::Es => w("Respuesta", "la IA le llama para decírselo cuando aceptes o rechaces", "número oculto: no se le puede avisar", "no se avisó a quien llamó", "La IA está llamando a {}", "No se devolvió la llamada: {}", "no hay ningún teléfono conectado para llamar", "el teléfono no recibió la orden de llamar"),
        Lang::Fr => w("Réponse", "l'IA rappelle cette personne quand vous acceptez ou refusez", "numéro masqué : impossible de prévenir l'appelant", "l'appelant n'a pas été prévenu", "L'IA rappelle le {}", "Pas de rappel : {}", "aucun téléphone connecté pour appeler", "le téléphone n'a pas reçu la demande d'appel"),
        Lang::De => w("Antwort", "die KI ruft zurück, sobald du zusagst oder absagst", "unterdrückte Nummer: der Anrufer kann nicht benachrichtigt werden", "der Anrufer wurde nicht benachrichtigt", "Die KI ruft {} zurück", "Kein Rückruf: {}", "kein Telefon verbunden, das anrufen kann", "das Telefon hat den Anrufauftrag nicht bekommen"),
        Lang::Pt => w("Resposta", "a IA liga de volta quando você aceitar ou recusar", "número oculto: não dá para avisar", "quem ligou não foi avisado", "A IA está ligando para {}", "Sem retorno de ligação: {}", "nenhum telefone conectado para ligar", "o telefone não recebeu o pedido de ligação"),
    }
}

/// Call the caller back with the owner's answer: the AI tells them, says
/// goodbye and the call ends. It goes out on the existing call-back route
/// ([`ControlRegistry::try_dispatch_phone_callback`]: the phone that took the
/// call when it is connected, from the SIM that call came in on — the route's
/// own choice), to the caller's own number only, and only to a number a
/// call-back may dial ([`callable_on`]): the owner picked this way for this
/// caller, so the number needs no place on `callback_whitelist`. A call-back to
/// a number another call-back is still calling waits for that one to end
/// ([`finish_callback`]). The AI's script waits for the phone to report this
/// very call ([`callback_for_call`]). Returns the [`Appointment::call`] code
/// and the owner's line.
async fn call_caller(
    rt: &RuntimeSettings,
    control: Option<&ControlRegistry>,
    task: &TaskRecord,
    appt: &Appointment,
    confirmed: bool,
) -> (String, String) {
    let lang = locale::notify_lang();
    let w = reply_words(lang);
    let number = task.caller_id.trim().to_string();
    if !callable(&number) {
        return ("no_number".to_string(), w.hidden.to_string());
    }
    let region = caller_region(rt, appt, &task.serial);
    if !callable_on(&number, region.as_deref()) {
        return ("not_callable".to_string(), callback_words(lang).not_callable.to_string());
    }
    let Some(control) = control else {
        return ("no_phone".to_string(), locale::fill(w.not_placed, w.no_phone));
    };
    let script = callback_prompt(&locale::active(), locale::current(), appt, confirmed);
    let cb = PendingCallback::new(&task.id, &number, &task.serial, script);
    if !callbacks_add(cb.clone()) {
        info!("appointment call-back task_id={} waits for an earlier call-back to the same number", task.id);
        return ("queued".to_string(), callback_words(lang).queued.to_string());
    }
    let (code, line) = dispatch_callback(control, &cb).await;
    if code != "dialing" {
        callbacks_remove(&task.id);
    }
    (code, line)
}

/// Ask a phone to place a call-back: `dialing`, or why it did not go out
/// (`no_phone`, `send_failed`), and the owner's line.
async fn dispatch_callback(control: &ControlRegistry, cb: &PendingCallback) -> (String, String) {
    let w = reply_words(locale::notify_lang());
    let not_placed = |code: &str, why: &str| (code.to_string(), locale::fill(w.not_placed, why));
    let only_this_number = [cb.number.clone()];
    // An errand goes out on the phone and SIM it was given, never another.
    let placed = match cb.kind {
        CallbackKind::Appointment => {
            control
                .try_dispatch_phone_callback(&cb.task_id, &cb.number, &cb.serial, &only_this_number)
                .await
        }
        CallbackKind::Errand => control.try_dispatch_on(&cb.task_id, &cb.number, &cb.serial, cb.sub_id).await,
    };
    match placed {
        Ok(true) => {
            // The phone the route picked (not always the one that took the call).
            if let Some(s) = control.callback_session_for(&cb.task_id, "").await {
                callbacks_set_serial(&cb.task_id, &s.serial);
            }
            info!("appointment call-back placed task_id={}", cb.task_id);
            ("dialing".to_string(), locale::fill(w.dialing, &cb.number))
        }
        Ok(false) => not_placed("no_phone", w.no_phone),
        Err(e) => {
            warn!("appointment call-back not placed task_id={}: {e:#}", cb.task_id);
            not_placed("send_failed", w.send_failed)
        }
    }
}

/// Where an appointment's call-back stands ([`PendingCallback`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallbackStage {
    /// Waits for an earlier call-back to the same number to end.
    Queued,
    /// `place_call` sent; the phone has not reported the call yet.
    Dialing,
    /// The phone that dials it reported `outbound_ready` for it (with its task,
    /// call and number): that phone's next media session with this number is
    /// this call ([`callback_for_call`]).
    Ready,
    /// Its Live session has the script.
    OnCall,
    /// The phone reported its end; whether the callee spoke decides the result
    /// ([`callback_call_over`]).
    Ended,
}

/// What an outbound call is for: an appointment's answer (its task), or an
/// errand the owner gave the AI ([`crate::errands`]; its id in `task_id`). Both
/// share one queue per number and the phone's reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallbackKind {
    Appointment,
    Errand,
}

/// A call-back with an appointment's answer, from the decision until it ended.
#[derive(Debug, Clone)]
struct PendingCallback {
    task_id: String,
    /// The number dialed.
    number: String,
    /// [`number_key`] of `number`: call-backs to one number go one at a time.
    key: String,
    /// The phone asked to dial it (until then, the one that took the call).
    serial: String,
    /// What the AI follows on it ([`callback_prompt`]).
    script: String,
    stage: CallbackStage,
    /// When it got to `stage`.
    since: std::time::Instant,
    kind: CallbackKind,
    /// The SIM to dial from (an errand's; None: the route's own choice).
    sub_id: Option<i64>,
    /// Cancelled after `place_call` went out (an errand): it no longer holds the
    /// phone or the number, and is kept only so that its call, should the phone
    /// still place it, is known and hung up ([`cancel_errand_call`]).
    dropped: bool,
}

impl PendingCallback {
    fn new(task_id: &str, number: &str, serial: &str, script: String) -> Self {
        Self {
            task_id: task_id.to_string(),
            number: number.trim().to_string(),
            key: number_key(number),
            serial: serial.trim().to_string(),
            script,
            stage: CallbackStage::Queued,
            since: std::time::Instant::now(),
            kind: CallbackKind::Appointment,
            sub_id: None,
            dropped: false,
        }
    }

    /// An errand's call: on that phone (and SIM) only.
    fn errand(id: &str, number: &str, serial: &str, sub_id: Option<i64>, script: String) -> Self {
        Self { kind: CallbackKind::Errand, sub_id, ..Self::new(id, number, serial, script) }
    }
}

/// The call-backs under way, by task.
static CALLBACKS: std::sync::Mutex<Vec<PendingCallback>> = std::sync::Mutex::new(Vec::new());
/// Reported ready this long ago and no media session took it: not this call.
const CALLBACK_READY_WINDOW: Duration = Duration::from_secs(60);
/// A media session that starts a moment before the phone's `outbound_ready`
/// is handled waits this long for it (only with a call-back dialing on that
/// phone to that number).
const CALLBACK_READY_WAIT: Duration = Duration::from_secs(3);
/// `place_call` sent and no word from the phone: the app drops a dial that
/// does not start within 30 s without saying so.
const CALLBACK_DIAL_LIMIT: Duration = Duration::from_secs(75);
/// Reported ready, but no media session ever took its script.
const CALLBACK_READY_LIMIT: Duration = Duration::from_secs(180);
/// Ended, but the call's summary never said whether the callee spoke.
const CALLBACK_ENDED_LIMIT: Duration = Duration::from_secs(300);
/// On the call this long: its end was lost.
const CALLBACK_CALL_LIMIT: Duration = Duration::from_secs(45 * 60);

/// The digits that tell numbers apart however they are written (`+886 912…`,
/// `0912…`): the last nine.
fn number_key(number: &str) -> String {
    let digits: Vec<char> = number.chars().filter(char::is_ascii_digit).collect();
    digits[digits.len().saturating_sub(9)..].iter().collect()
}

fn callbacks() -> std::sync::MutexGuard<'static, Vec<PendingCallback>> {
    CALLBACKS.lock().unwrap_or_else(|e| e.into_inner())
}

/// What [`callbacks_try_add`] did with a call-back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Added {
    /// Dial it now (stage `Dialing`).
    Dial,
    /// It waits for an earlier call to the same number (stage `Queued`).
    Wait,
    /// Its task's call already went to a phone (`Dialing`, `Ready`, `OnCall`):
    /// nothing added, never a second `place_call` for it.
    AlreadyOut,
}

fn callbacks_try_add(mut cb: PendingCallback) -> Added {
    let mut list = callbacks();
    let out = |c: &PendingCallback| matches!(c.stage, CallbackStage::Dialing | CallbackStage::Ready | CallbackStage::OnCall);
    if list.iter().any(|c| c.task_id == cb.task_id && out(c)) {
        return Added::AlreadyOut;
    }
    list.retain(|c| c.task_id != cb.task_id);
    let busy = list.iter().any(|c| c.key == cb.key && !c.dropped);
    cb.stage = if busy { CallbackStage::Queued } else { CallbackStage::Dialing };
    cb.since = std::time::Instant::now();
    list.push(cb);
    if busy {
        Added::Wait
    } else {
        Added::Dial
    }
}

/// Take on a call-back: true = dial it now (stage `Dialing`), false = it waits
/// for an earlier one to the same number (stage `Queued`), or its task's call
/// is already out (left as it is).
fn callbacks_add(cb: PendingCallback) -> bool {
    callbacks_try_add(cb) == Added::Dial
}

fn callbacks_stage(task_id: &str) -> Option<CallbackStage> {
    callbacks().iter().find(|c| c.task_id == task_id).map(|c| c.stage)
}

fn callbacks_kind(task_id: &str) -> Option<CallbackKind> {
    callbacks().iter().find(|c| c.task_id == task_id).map(|c| c.kind)
}

fn callbacks_set_stage(task_id: &str, stage: CallbackStage) {
    if let Some(c) = callbacks().iter_mut().find(|c| c.task_id == task_id) {
        c.stage = stage;
        c.since = std::time::Instant::now();
    }
}

fn callbacks_set_serial(task_id: &str, serial: &str) {
    if let Some(c) = callbacks().iter_mut().find(|c| c.task_id == task_id) {
        c.serial = serial.trim().to_string();
    }
}

fn callbacks_remove(task_id: &str) -> Option<PendingCallback> {
    let mut list = callbacks();
    let i = list.iter().position(|c| c.task_id == task_id)?;
    Some(list.remove(i))
}

/// The next call-back waiting for the number `key` (now `Dialing`); none while
/// another call-back to it is still under way.
fn callbacks_next(key: &str) -> Option<PendingCallback> {
    let mut list = callbacks();
    if list.iter().any(|c| c.key == key && c.stage != CallbackStage::Queued && !c.dropped) {
        return None;
    }
    let c = list.iter_mut().find(|c| c.key == key && c.stage == CallbackStage::Queued)?;
    c.stage = CallbackStage::Dialing;
    c.since = std::time::Instant::now();
    Some(c.clone())
}

/// The phone `serial` reported `outbound_ready` for `task_id` (`number`: the
/// number it dialed; empty from an app that does not say). True when that is
/// the call-back dialing on that phone to that number, which is now `Ready`.
fn callback_ready(task_id: &str, serial: &str, number: &str) -> bool {
    let key = number_key(number);
    let mut list = callbacks();
    let Some(c) = list.iter_mut().find(|c| c.task_id == task_id) else {
        return false;
    };
    if c.stage != CallbackStage::Dialing || !c.serial.eq_ignore_ascii_case(serial.trim()) || (!key.is_empty() && key != c.key) {
        return false;
    }
    c.stage = CallbackStage::Ready;
    c.since = std::time::Instant::now();
    true
}

/// A call's Live session that is an appointment's call-back ([`callback_for_call`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackCall {
    pub task_id: String,
    /// The system prompt instead of the answering one ([`callback_prompt`]).
    pub script: String,
    /// An errand's call ([`crate::errands`]): `task_id` is the errand's id.
    pub errand: bool,
}

/// The call-back the phone `serial` reported ready whose number is this
/// media session's `caller_id` (a hello without one: the only one ready on that
/// phone). It is `OnCall` from now on, so its script goes to one session only.
fn take_ready_callback(serial: &str, caller_id: &str) -> Option<CallbackCall> {
    let key = number_key(caller_id);
    let mut list = callbacks();
    let ready = |c: &PendingCallback| {
        c.stage == CallbackStage::Ready && c.serial.eq_ignore_ascii_case(serial.trim()) && c.since.elapsed() < CALLBACK_READY_WINDOW
    };
    let i = if key.is_empty() {
        let mut on_phone = list.iter().enumerate().filter(|(_, c)| ready(c));
        let (i, _) = on_phone.next()?;
        if on_phone.next().is_some() {
            return None;
        }
        i
    } else {
        list.iter().position(|c| ready(c) && c.key == key)?
    };
    let c = &mut list[i];
    c.stage = CallbackStage::OnCall;
    c.since = std::time::Instant::now();
    Some(CallbackCall { task_id: c.task_id.clone(), script: c.script.clone(), errand: c.kind == CallbackKind::Errand })
}

/// A call-back is dialing on `serial` to this number (its `outbound_ready`
/// may still be on its way).
fn callback_dialing_on(serial: &str, caller_id: &str) -> bool {
    let key = number_key(caller_id);
    callbacks().iter().any(|c| {
        c.stage == CallbackStage::Dialing && c.serial.eq_ignore_ascii_case(serial.trim()) && (key.is_empty() || c.key == key)
    })
}

/// Whether the Live session starting for a call on `serial` with `caller_id`
/// is an appointment's call-back, and its script. Only the call the phone
/// reported dialing for it (`outbound_ready` with its task, call and number,
/// [`on_callback_event`]): never an incoming call from that number, a ring
/// prewarm or a session of another phone. Waits a moment only while a
/// call-back to this number is dialing on this phone.
pub async fn callback_for_call(serial: &str, caller_id: &str) -> Option<CallbackCall> {
    let until = tokio::time::Instant::now() + CALLBACK_READY_WAIT;
    loop {
        if let Some(c) = take_ready_callback(serial, caller_id) {
            return Some(c);
        }
        if !callback_dialing_on(serial, caller_id) || tokio::time::Instant::now() >= until {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The call-back whose call the phone `serial` is on, or has just ended (for
/// the call's summary).
pub fn callback_task_on(serial: &str) -> Option<String> {
    callbacks()
        .iter()
        .find(|c| matches!(c.stage, CallbackStage::OnCall | CallbackStage::Ended) && c.serial.eq_ignore_ascii_case(serial.trim()))
        .map(|c| c.task_id.clone())
}

/// Errands: have `serial` dial `number` (from the SIM `sub_id`, None: its
/// default) for errand `id`, its AI following `script` on that call only.
/// `dialing`, `queued` (an earlier call to the number is under way: its end
/// dials this one), `no_phone` or `send_failed`. An errand whose call is
/// already out is `dialing` without a second `place_call`.
pub(crate) async fn place_errand_call(control: &ControlRegistry, id: &str, number: &str, serial: &str, sub_id: Option<i64>, script: String) -> String {
    let cb = PendingCallback::errand(id, number, serial, sub_id, script);
    match callbacks_try_add(cb.clone()) {
        Added::AlreadyOut => {
            warn!("errand id={id}: its call is already out — not dialed again");
            return "dialing".to_string();
        }
        Added::Wait => {
            info!("errand id={id} waits for an earlier call to the same number");
            return "queued".to_string();
        }
        Added::Dial => {}
    }
    let (code, _) = dispatch_callback(control, &cb).await;
    if code != "dialing" {
        callbacks_remove(id);
    }
    code
}

/// Errands: forget errand `id`'s outbound call while it still waits its turn
/// (cancelled, deleted or given up). False when none waits: a call already
/// handed to the phone stays until the phone reports its end.
pub(crate) fn drop_errand_call(id: &str) -> bool {
    let mut list = callbacks();
    let Some(i) = list.iter().position(|c| c.task_id == id && c.stage == CallbackStage::Queued) else {
        return false;
    };
    list.remove(i);
    true
}

/// Errands: errand `id` has an outbound call here (waiting its turn or under way).
pub(crate) fn errand_call_known(id: &str) -> bool {
    callbacks().iter().any(|c| c.task_id == id)
}

/// Errands: the owner cancelled errand `id`. Waiting its turn: forgotten. Sent
/// to the phone but not on the line yet (`Dialing`, `Ready`): it no longer
/// holds the phone nor the number, and the next call waiting for that number
/// goes out now; it is kept only so that, should the phone still place it, its
/// media session is known as this errand's and hung up at once
/// ([`crate::errands::ErrandStore::call_gate`]). On the line: its session hangs
/// up (the errand's cancel flag).
pub(crate) async fn cancel_errand_call(ctx: &CallbackCtx<'_>, id: &str) {
    let key = {
        let mut list = callbacks();
        let Some(i) = list.iter().position(|c| c.task_id == id) else {
            return;
        };
        match list[i].stage {
            CallbackStage::Queued => {
                list.remove(i);
                return;
            }
            CallbackStage::Dialing | CallbackStage::Ready if !list[i].dropped => {
                list[i].dropped = true;
                list[i].key.clone()
            }
            _ => return,
        }
    };
    info!("errand id={id} cancelled while it dialed: its phone and number are free for the next call");
    advance_queue(ctx, &key).await;
}

/// An outbound call (call-back or errand) is dialing on `serial` or on the
/// line there.
pub(crate) fn outbound_busy_on(serial: &str) -> bool {
    callbacks().iter().any(|c| {
        !c.dropped
            && matches!(c.stage, CallbackStage::Dialing | CallbackStage::Ready | CallbackStage::OnCall)
            && c.serial.eq_ignore_ascii_case(serial.trim())
    })
}

/// The call record's event for a call that was the AI's call-back.
pub const AI_CALLBACK_EVENT: &str = "ai_callback";

/// A call record that was the AI's call-back: no calendar suggestion, no
/// appointments, its own summary title ([`callback_headline`]).
pub fn is_ai_callback(events: &[String]) -> bool {
    events.iter().any(|e| e == AI_CALLBACK_EVENT)
}

/// The callee said something on the call (the Live dialogue has a line of
/// theirs): they answered.
pub fn callee_spoke(dialogue: &str) -> bool {
    dialogue
        .lines()
        .filter_map(|l| l.trim().strip_prefix("對方："))
        .any(|l| !l.trim().is_empty())
}

/// What call-back bookkeeping outside a decision works with.
pub struct CallbackCtx<'a> {
    pub http: &'a Client,
    /// The effective settings (notification channels).
    pub settings: &'a crate::config::Settings,
    pub tasks: &'a TaskStore,
    pub control: &'a ControlRegistry,
    /// The errands, whose calls go out on the same route.
    pub errands: Option<&'a crate::errands::ErrandStore>,
}

/// A phone's report on a call (`outbound_ready`, `place_call_failed`,
/// `call_end` on its control socket, from `serial`) that is about an
/// appointment's call-back: `outbound_ready` of the call this phone was asked
/// to place, to that number, readies its script; a failed dial, or an end
/// before its call started, ends it as `failed`; the end of its call waits for
/// the call's summary ([`callback_call_over`]). Reports on anything else are
/// left alone.
pub async fn on_callback_event(ctx: &CallbackCtx<'_>, serial: &str, msg: &Value) {
    let kind = msg.get("type").and_then(Value::as_str).unwrap_or("");
    let call_id = msg.get("call_id").and_then(Value::as_str).unwrap_or("").trim();
    let task_id = msg.get("task_id").and_then(Value::as_str).unwrap_or("").trim();
    let Some(session) = ctx.control.callback_session_for(task_id, call_id).await else {
        return;
    };
    let task_id = session.task_id.clone();
    let Some(stage) = callbacks_stage(&task_id) else {
        return;
    };
    let errand = callbacks_kind(&task_id) == Some(CallbackKind::Errand);
    if !call_id.is_empty() && session.call_id != call_id {
        info!("appointment call-back task_id={task_id}: {kind} of another call ignored");
        return;
    }
    match kind {
        "outbound_ready" => {
            callbacks_set_serial(&task_id, &session.serial);
            let number = msg.get("number").and_then(Value::as_str).unwrap_or("");
            if callback_ready(&task_id, serial, number) {
                info!("appointment call-back task_id={task_id} serial={serial}: dialing, the AI waits for the callee");
                if !errand {
                    set_call_code(ctx.tasks, &task_id, "connected").await;
                }
            } else {
                warn!("appointment call-back task_id={task_id}: outbound_ready from serial={serial} does not match the call asked for — not used");
            }
        }
        // The phone was on a call: an errand waits for it again.
        "place_call_failed" if errand && msg.get("reason").and_then(Value::as_str) == Some("busy") => {
            finish_callback(ctx, &task_id, "phone_busy").await
        }
        "place_call_failed" => finish_callback(ctx, &task_id, "failed").await,
        "call_end" => match stage {
            CallbackStage::OnCall => callbacks_set_stage(&task_id, CallbackStage::Ended),
            CallbackStage::Ended | CallbackStage::Queued => {}
            CallbackStage::Dialing | CallbackStage::Ready => finish_callback(ctx, &task_id, "failed").await,
        },
        _ => {}
    }
}

/// The call-back's call is over and its summary is written: `answered` when
/// the callee said something on it ([`callee_spoke`]), else `no_answer`.
pub async fn callback_call_over(ctx: &CallbackCtx<'_>, task_id: &str, answered: bool) {
    finish_callback(ctx, task_id, if answered { "answered" } else { "no_answer" }).await;
}

/// Call-backs the phone never reported on in time (see the limits above).
pub async fn sweep_callbacks(ctx: &CallbackCtx<'_>) {
    for (task_id, code) in callbacks_overdue() {
        warn!("appointment call-back task_id={task_id}: no report in time — {code}");
        finish_callback(ctx, &task_id, code).await;
    }
}

fn callbacks_overdue() -> Vec<(String, &'static str)> {
    callbacks()
        .iter()
        .filter_map(|c| {
            let age = c.since.elapsed();
            let code = match c.stage {
                CallbackStage::Dialing if age >= CALLBACK_DIAL_LIMIT => "failed",
                CallbackStage::Ready if age >= CALLBACK_READY_LIMIT => "failed",
                CallbackStage::Ended if age >= CALLBACK_ENDED_LIMIT => "done",
                CallbackStage::OnCall if age >= CALLBACK_CALL_LIMIT => "done",
                _ => return None,
            };
            Some((c.task_id.clone(), code))
        })
        .collect()
}

/// A call-back is over (`code`, [`Appointment::call`]): kept on its task,
/// told on the notification channels when its call never happened (its
/// summary tells the rest), and the next call-back to that number dialed.
async fn finish_callback(ctx: &CallbackCtx<'_>, task_id: &str, code: &str) {
    let Some(done) = callbacks_remove(task_id) else {
        return;
    };
    info!("outbound call over task_id={task_id} kind={:?} result={code}", done.kind);
    match done.kind {
        CallbackKind::Appointment => {
            set_call_code(ctx.tasks, task_id, code).await;
            if callback_needs_notice(code) {
                notify_callback(ctx, task_id, code).await;
            }
        }
        CallbackKind::Errand => {
            if let Some(store) = ctx.errands {
                crate::errands::outbound_over(ctx, store, task_id, code).await;
            }
        }
    }
    advance_queue(ctx, &done.key).await;
}

/// The call-backs waiting for the number `key`, dialed in turn until one goes
/// out (none while another call to it is under way).
async fn advance_queue(ctx: &CallbackCtx<'_>, key: &str) {
    while let Some(next) = callbacks_next(key) {
        // An errand cancelled or deleted while it waited: never dialed.
        if next.kind == CallbackKind::Errand && ctx.errands.is_some_and(|s| !s.call_allowed(&next.task_id)) {
            info!("errand id={} no longer wanted: its turn skipped", next.task_id);
            callbacks_remove(&next.task_id);
            continue;
        }
        let (c, _) = dispatch_callback(ctx.control, &next).await;
        if next.kind == CallbackKind::Appointment {
            set_call_code(ctx.tasks, &next.task_id, &c).await;
        }
        if c == "dialing" {
            if next.kind == CallbackKind::Errand {
                if let Some(store) = ctx.errands {
                    crate::errands::outbound_dialing(store, &next.task_id).await;
                }
            }
            break;
        }
        callbacks_remove(&next.task_id);
        match next.kind {
            CallbackKind::Appointment => notify_callback(ctx, &next.task_id, &c).await,
            CallbackKind::Errand => {
                if let Some(store) = ctx.errands {
                    crate::errands::outbound_over(ctx, store, &next.task_id, &c).await;
                }
            }
        }
    }
}

/// Results the call's summary does not tell: the call never got going.
fn callback_needs_notice(code: &str) -> bool {
    matches!(code, "failed" | "no_phone" | "send_failed")
}

/// Keep a call-back's progress on its task (`appointment.call`). A decision
/// still writing the task finishes first (it writes `dialing` or `queued`).
async fn set_call_code(tasks: &TaskStore, task_id: &str, code: &str) {
    let _claim = Deciding::claim_within(task_id, Duration::from_secs(15)).await;
    let Some(task) = tasks.get(task_id).await else {
        return;
    };
    let Some(mut appt) = task.appointment.clone() else {
        return;
    };
    if appt.call == code {
        return;
    }
    appt.call = code.to_string();
    let patch = TaskPatch { appointment: Some(appt), ..Default::default() };
    if let Err(e) = tasks.patch(task_id, patch).await {
        warn!("appointment call-back result not kept task_id={task_id}: {e:#}");
        return;
    }
    let _ = tasks.push_event(task_id, &format!("callback_result_{code}")).await;
}

/// "📞 AI 回電：<number>" with what became of the call-back and the appointment.
async fn notify_callback(ctx: &CallbackCtx<'_>, task_id: &str, code: &str) {
    let Some(task) = ctx.tasks.get(task_id).await else {
        return;
    };
    let Some(appt) = task.appointment.as_ref() else {
        return;
    };
    let lang = locale::notify_lang();
    let body = format!("{}\n{}", callback_result_line(lang, code, &task.caller_id), appt.headline(lang));
    let ok = crate::notify::notify_ai_callback(
        ctx.http,
        ctx.settings,
        &task.caller_id,
        &task.serial,
        &callback_headline(lang, &task.caller_id),
        &body,
    )
    .await;
    info!("appointment call-back notice task_id={task_id} result={code} sent={ok}");
}

/// The title of everything about a call-back: "AI 回電：<number>".
pub fn callback_headline(lang: Lang, number: &str) -> String {
    format!("{}{}{}", callback_words(lang).title, locale::phrases(lang).sep, number.trim())
}

/// A call-back's result ([`Appointment::call`]) in the owner's words.
pub fn callback_result_line(lang: Lang, code: &str, number: &str) -> String {
    let w = reply_words(lang);
    let c = callback_words(lang);
    match code {
        "answered" => c.answered.to_string(),
        "no_answer" => c.no_answer.to_string(),
        "failed" => c.failed.to_string(),
        "done" => c.done.to_string(),
        "queued" => c.queued.to_string(),
        "not_callable" => c.not_callable.to_string(),
        "dialing" | "connected" => locale::fill(w.dialing, number.trim()),
        "no_phone" => locale::fill(w.not_placed, w.no_phone),
        "send_failed" => locale::fill(w.not_placed, w.send_failed),
        "no_number" => w.hidden.to_string(),
        other => other.to_string(),
    }
}

/// The owner's words about the AI's call-backs ([`callback_result_line`]).
struct CallbackWords {
    /// "AI 回電" (the notification's title).
    title: &'static str,
    /// It waits for an earlier call-back to the same number.
    queued: &'static str,
    /// A number a call-back may not dial ([`callable_on`]).
    not_callable: &'static str,
    answered: &'static str,
    no_answer: &'static str,
    failed: &'static str,
    /// It ended; whether anyone answered is not known.
    done: &'static str,
}

fn callback_words(lang: Lang) -> CallbackWords {
    let w = |title, queued, not_callable, answered, no_answer, failed, done| CallbackWords {
        title,
        queued,
        not_callable,
        answered,
        no_answer,
        failed,
        done,
    };
    match lang {
        Lang::ZhTw => w("AI 回電", "等打給同一個號碼的前一通回電結束，AI 再撥", "AI 沒有回電：這個號碼不能回電（國外、免付費、加值或特殊號碼）", "AI 回電接通，已告訴對方結果", "AI 回電了，對方沒接", "AI 回電撥不出去", "AI 回電的通話已結束"),
        Lang::ZhCn => w("AI 回拨", "等拨给同一个号码的前一通回拨结束，AI 再拨", "AI 没有回拨：这个号码不能回拨（境外、免费、增值或特殊号码）", "AI 回拨接通，已告诉对方结果", "AI 回拨了，对方没接", "AI 回拨拨不出去", "AI 回拨的通话已结束"),
        Lang::En => w("AI call-back", "The AI calls once its earlier call-back to the same number has ended", "No call-back: this number can't be called back (abroad, toll-free, premium or a short code)", "The AI's call-back got through and the caller was told", "The AI called back; nobody answered", "The AI's call-back couldn't be placed", "The AI's call-back has ended"),
        Lang::Ja => w("AI の折り返し電話", "同じ番号への前の折り返し電話が終わってから AI が電話します", "折り返し電話なし：この番号には折り返せません（海外・フリーダイヤル・有料・短縮番号）", "AI の折り返し電話がつながり、結果を伝えました", "AI が電話しましたが、相手は出ませんでした", "AI の折り返し電話をかけられませんでした", "AI の折り返し電話は終わりました"),
        Lang::Ko => w("AI 회신 전화", "같은 번호로 건 앞의 회신 전화가 끝나면 AI가 전화합니다", "회신 전화 없음: 이 번호로는 다시 걸 수 없습니다(해외, 수신자 부담, 유료, 특수 번호)", "AI의 회신 전화가 연결되어 결과를 알렸습니다", "AI가 전화했지만 받지 않았습니다", "AI가 전화를 걸지 못했습니다", "AI의 회신 전화가 끝났습니다"),
        Lang::Es => w("Llamada de la IA", "La IA llamará cuando termine su llamada anterior al mismo número", "Sin llamada: a este número no se puede llamar (extranjero, gratuito, de tarificación especial o corto)", "La llamada de la IA conectó y se le dio la respuesta", "La IA llamó y nadie contestó", "La IA no pudo hacer la llamada", "La llamada de la IA terminó"),
        Lang::Fr => w("Rappel de l'IA", "L'IA appellera une fois son rappel précédent au même numéro terminé", "Pas de rappel : ce numéro ne peut pas être rappelé (étranger, gratuit, surtaxé ou court)", "Le rappel de l'IA a abouti et l'appelant a eu la réponse", "L'IA a rappelé, personne n'a répondu", "L'IA n'a pas pu passer l'appel", "Le rappel de l'IA est terminé"),
        Lang::De => w("KI-Rückruf", "Die KI ruft an, sobald ihr voriger Rückruf an dieselbe Nummer vorbei ist", "Kein Rückruf: diese Nummer kann nicht zurückgerufen werden (Ausland, kostenlos, kostenpflichtig oder Kurzwahl)", "Der Rückruf der KI kam durch, der Anrufer weiß Bescheid", "Die KI hat angerufen, niemand ging ran", "Die KI konnte nicht anrufen", "Der Rückruf der KI ist beendet"),
        Lang::Pt => w("Retorno da IA", "A IA liga quando terminar a ligação anterior dela para o mesmo número", "Sem retorno: não dá para ligar para esse número (exterior, gratuito, tarifado ou curto)", "A ligação da IA foi atendida e a pessoa recebeu a resposta", "A IA ligou e ninguém atendeu", "A IA não conseguiu fazer a ligação", "A ligação da IA terminou"),
    }
}

/// What the AI follows on its call-back with the owner's answer (English; it
/// speaks `lang`, the language it used with the caller, or theirs): who is
/// calling, the answer — the time and what it is about, or a polite no — then
/// one goodbye, after which the call ends.
pub fn callback_prompt(active: &locale::Active, lang: Lang, appt: &Appointment, confirmed: bool) -> String {
    let owner = crate::prompts::owner_label(lang, &active.owner);
    let language = locale::phrases(lang).name_en;
    let when = appt.start_text(lang);
    let about = match appt.title.trim() {
        "" => String::new(),
        t => format!(" ({t})"),
    };
    let who = if active.assistant {
        format!("{owner}'s assistant")
    } else {
        format!("{owner} — you speak as {owner} yourself")
    };
    let answer = if confirmed {
        format!("{owner} agrees: {when}{about} works. Confirm the time and what it is about, and say you look forward to it.")
    } else {
        format!("{owner} cannot make it at {when}{about} after all. Apologize politely and say they are welcome to call again to find another time.")
    };
    format!(
        "You are making a short phone call for {owner}. Earlier this person called and asked to meet at {when}{about}; {owner} has decided, and you are calling them back with the answer.\n\
Who is calling: {who}.\n\
The answer: {answer}\n\
How the call goes:\n\
- The call starts while it still rings: say nothing until they have picked up and said something (a ringing tone, music or silence is not them).\n\
- Then greet them in one short sentence, say who is calling and that you are calling back about the appointment they asked for.\n\
- Then give the answer in one or two short sentences, in {language} (if they answer in another language, use theirs).\n\
- If they ask, say the time again. Agree to nothing new and do not move the time; if they want another time, say {owner} will get back to them about it. Give out no personal details, codes or other information.\n\
- Then say goodbye once and stop talking: the call ends after your goodbye.\n\
- If voicemail or an answering machine picks up, leave the answer in two sentences and say goodbye.\n\
If they seriously ask whether they are talking to a real person, say that you are an AI assistant."
    )
}

// ---------------------------------------------------------------------------
// After the call
// ---------------------------------------------------------------------------

/// The line under the post-call summary: "📅 已排入行事曆：…", or — waiting for
/// the owner — "📅 預約待你確認：… — 到網頁確認：<link>" (`link` = the call's page
/// in the web console; without one, where to go). Every notification channel
/// gets it, so the owner learns of it whatever they use. One line for each
/// appointment of the call ([`Appointment::more`]).
pub fn summary_line(appt: &Appointment, link: &str) -> String {
    appt.each()
        .iter()
        .map(|a| summary_line_one(a, link))
        .collect::<Vec<_>>()
        .join("\n")
}

fn summary_line_one(appt: &Appointment, link: &str) -> String {
    let lang = locale::notify_lang();
    let p = locale::phrases(lang);
    let label = if appt.status == BOOKED { p.ap_booked } else { p.ap_pending };
    let mut s = format!("📅 {label}{}{}", p.sep, appt.headline(lang));
    if appt.status == PENDING {
        let link = link.trim();
        let web = if link.is_empty() {
            p.ap_web_console.to_string()
        } else {
            locale::fill(p.ap_web_confirm, link)
        };
        s.push_str(" — ");
        s.push_str(&web);
    }
    for e in &appt.replaced {
        s.push('\n');
        s.push_str(&locale::fill(p.ap_replaced, &e.when(lang)));
    }
    s
}

/// The tasks for what a call agreed to (`call` = its record): one for `appt`
/// and one for each of the others in its [`Appointment::more`], each decided on
/// its own. Each gets [`after_summary`].
pub async fn create_tasks(store: &TaskStore, call: &CallRecord, appt: &Appointment) -> Vec<TaskRecord> {
    let mut out = Vec::new();
    for a in appt.each() {
        if let Some(t) = create_one(store, call, &a).await {
            out.push(t);
        }
    }
    out
}

/// [`create_tasks`], answering with the first task only (every appointment of
/// the call still gets its task).
pub async fn create_task(store: &TaskStore, call: &CallRecord, appt: &Appointment) -> Option<TaskRecord> {
    create_tasks(store, call, appt).await.into_iter().next()
}

async fn create_one(store: &TaskStore, call: &CallRecord, appt: &Appointment) -> Option<TaskRecord> {
    let lang = locale::notify_lang();
    let booked = appt.status == BOOKED;
    let mut events = vec!["appointment_from_call".to_string()];
    if booked {
        events.push("calendar_event_created".to_string());
    }
    let task = TaskRecord {
        id: String::new(),
        created_at: String::new(),
        updated_at: String::new(),
        call_id: call.id.clone(),
        serial: call.serial.clone(),
        caller_id: call.caller_id.clone(),
        status: if booked { TaskStatus::Fulfilled } else { TaskStatus::Pending },
        intent: TaskIntent::Appointment,
        summary: appt.headline(lang),
        transcript: String::new(),
        datetime_hint: Some(appt.start.clone()),
        reply_method: ReplyMethod::None,
        reply_contact: sms_number_on(
            &call.caller_id,
            appt.sim_country
                .clone()
                .or_else(|| crate::phone_region::last_sim(&call.serial))
                .as_deref(),
        ),
        contact_verified: false,
        voicemail_path: None,
        calendar_event_id: (!appt.event_id.is_empty()).then(|| appt.event_id.clone()),
        fulfill_error: (!appt.auto_error.is_empty()).then(|| appt.auto_error.clone()),
        openclaw_ref: None,
        events,
        appointment: Some(appt.clone()),
    };
    match store.create(task).await {
        Ok(t) => {
            info!(
                "appointment task id={} call_id={} status={} mode={}",
                t.id, t.call_id, appt.status, appt.mode
            );
            Some(t)
        }
        Err(e) => {
            warn!("appointment task create failed call_id={}: {e:#}", call.id);
            None
        }
    }
}

/// The Telegram card of an appointment waiting for the owner: (title, body).
/// `link`: the call's page in the web console (a line of its own when known).
pub fn card_text(rt: &RuntimeSettings, task: &TaskRecord, appt: &Appointment, link: &str) -> (String, String) {
    let lang = locale::notify_lang();
    let p = locale::phrases(lang);
    let sep = p.sep;
    let mut lines = vec![
        format!("{}{sep}{}", p.n_number, display_caller(&task.caller_id)),
        format!("{}{sep}{}", p.n_time, appt.when(lang)),
    ];
    if !appt.title.trim().is_empty() {
        lines.push(format!("{}{sep}{}", p.ap_what, appt.title.trim()));
    }
    if !appt.caller_name.trim().is_empty() {
        lines.push(format!("{}{sep}{}", p.ap_name, appt.caller_name.trim()));
    }
    lines.push(format!("{}{sep}{}", p.n_phone, crate::notify::device_label(&task.serial)));
    // How the answer reaches the caller when it is decided on the card (the
    // default for this number; the web console can pick another way).
    let opts = reply_options(rt, &task.caller_id, caller_region(rt, appt, &task.serial).as_deref());
    let w = reply_words(lang);
    lines.push(match default_reply(&rt.appointment_reply, opts) {
        Reply::Sms => format!("{}{sep}{}", p.ap_sms, p.ap_sms_will),
        Reply::Call => format!("{}{sep}{}", w.label, w.call_will),
        Reply::None if !opts.call => format!("{}{sep}{}", w.label, w.hidden),
        Reply::None if !rt.appointment_sms => format!("{}{sep}{}", p.ap_sms, p.ap_sms_off),
        Reply::None => format!("{}{sep}{}", p.ap_sms, p.ap_sms_cannot),
    });
    if !appt.auto_error.is_empty() {
        lines.push(locale::fill(p.ap_auto_failed, &appt.auto_error));
    }
    if !appt.write_error.is_empty() {
        lines.push(appt.write_error.clone());
    }
    for e in &appt.replaced {
        lines.push(locale::fill(p.ap_replaced, &e.when(lang)));
    }
    if !link.trim().is_empty() {
        lines.push(format!("{}{sep}{}", p.ap_web_label, link.trim()));
    }
    (p.ap_card_title.to_string(), lines.join("\n"))
}

/// After the summary went out: the card for an appointment that waits for
/// the owner (Telegram set up and `call_notify` on; the web console shows it
/// either way), or — booked on the call — the text message to the caller.
/// `link`: the call's page in the web console.
pub async fn after_summary(
    http: &Client,
    rt: &RuntimeSettings,
    store: &TaskStore,
    calls: &CallStore,
    control: &ControlRegistry,
    task: &TaskRecord,
    link: &str,
) {
    let Some(mut appt) = task.appointment.clone() else {
        return;
    };
    let token = rt.telegram_bot_token.trim();
    let chat = rt.telegram_chat_id.trim();
    let tg = crate::tg_inbound::task_card_ready(rt.call_notify, token, chat);
    if appt.status == PENDING {
        if !tg {
            info!("appointment card not sent (Telegram not set up; decided on the web) task_id={}", task.id);
            return;
        }
        let p = texts();
        let (title, body) = card_text(rt, task, &appt, link);
        match crate::tg_inbound::send_task_card_labeled(
            http, token, chat, &title, &task.id, &body, p.ap_confirm, p.ap_decline,
        )
        .await
        {
            Ok(message_id) if message_id != 0 => {
                let card = TgCard { chat_id: chat.to_string(), message_id };
                if keep_card(store, &task.id, &card, CARD_SETTLE_WAIT).await == CardFate::Close {
                    // Decided on the web while the card was on its way: no buttons
                    // for a decision already made.
                    crate::tg_inbound::remove_inline_keyboard(http, token, &card.chat_id, card.message_id).await;
                    let text = format!("{}\n{}", p.ap_already, appt.headline(locale::notify_lang()));
                    if let Err(e) = crate::tg_inbound::send_plain_message(http, token, &card.chat_id, &text).await {
                        warn!("appointment card close note failed task_id={}: {e:#}", task.id);
                    }
                    info!("appointment card closed at once (decided while it was sent) task_id={}", task.id);
                }
            }
            Ok(_) => {}
            Err(e) => warn!("appointment card failed task_id={}: {e:#}", task.id),
        }
        return;
    }
    if appt.status != BOOKED || !rt.appointment_sms {
        return;
    }
    let (code, line) = text_caller(rt, Some(control), task, &appt, true, None).await;
    appt.sms = code;
    let _ = store.push_event(&task.id, &format!("sms_{}", appt.sms)).await;
    let _ = store
        .patch(
            &task.id,
            TaskPatch {
                appointment: Some(appt.clone()),
                ..Default::default()
            },
        )
        .await;
    update_call_notes(calls, &task.call_id, &appt).await;
    if tg {
        let lang = locale::notify_lang();
        let text = format!("📅 {}\n{line}", appt.headline(lang));
        if let Err(e) = crate::tg_inbound::send_plain_message(http, token, chat, &text).await {
            warn!("appointment sms report failed task_id={}: {e:#}", task.id);
        }
    }
}

/// Appointment tasks being decided right now: one decision per task at a time
/// (two Telegram presses in one batch, Telegram and the API together, ✅ then ❌).
static DECIDING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// How long a card just sent waits for a decision under way (calendar, then a
/// text message that may wait 90 s for the phone) before it is settled.
const CARD_SETTLE_WAIT: Duration = Duration::from_secs(150);

/// What becomes of a Telegram card that was just sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CardFate {
    /// Still waiting for the owner: the card is kept on the task (a decision on
    /// the web takes its buttons off).
    Keep,
    /// Decided meanwhile (on the web, through the API) or not to be found: its
    /// buttons come off now.
    Close,
}

/// The card's message is known: put it on the task — but only on the task as it
/// stands now, under the decision lock, and only while it still waits. The task
/// is on the web console from the moment it is made, so the owner may have
/// decided while the card was on its way; writing back the copy taken before
/// would undo that decision (pending again, the event forgotten, the buttons
/// live for a second decision). A decision under way is waited for (up to
/// `wait`), so its outcome is what is read.
async fn keep_card(store: &TaskStore, task_id: &str, card: &TgCard, wait: Duration) -> CardFate {
    let Some(_claim) = Deciding::claim_within(task_id, wait).await else {
        warn!("appointment card: a decision still holds task_id={task_id}; card closed");
        return CardFate::Close;
    };
    let Some(mut appt) = store.get(task_id).await.and_then(|t| t.appointment) else {
        return CardFate::Close;
    };
    if appt.status != PENDING {
        return CardFate::Close;
    }
    appt.tg_card = Some(card.clone());
    match store
        .patch(
            task_id,
            TaskPatch {
                appointment: Some(appt),
                ..Default::default()
            },
        )
        .await
    {
        Ok(_) => CardFate::Keep,
        Err(e) => {
            // Not on the task: nothing could take its buttons off later.
            warn!("appointment card not kept task_id={task_id}: {e:#}");
            CardFate::Close
        }
    }
}

/// Holds a task while it is being decided; let go when dropped.
struct Deciding(String);

impl Deciding {
    /// None: another decision on this task is under way.
    fn claim(task_id: &str) -> Option<Self> {
        let mut busy = DECIDING.lock().unwrap_or_else(|e| e.into_inner());
        if busy.iter().any(|t| t == task_id) {
            return None;
        }
        busy.push(task_id.to_string());
        Some(Self(task_id.to_string()))
    }

    /// [`Self::claim`], waiting up to `wait` for a decision under way to finish.
    async fn claim_within(task_id: &str, wait: Duration) -> Option<Self> {
        let until = tokio::time::Instant::now() + wait;
        loop {
            if let Some(c) = Self::claim(task_id) {
                return Some(c);
            }
            if tokio::time::Instant::now() >= until {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

impl Drop for Deciding {
    fn drop(&mut self) {
        DECIDING
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|t| t != &self.0);
    }
}

/// The owner's ✅ (`confirm`) or ❌ on an appointment: calendar, text
/// message, task and call notes. Returns the task and the reply for Telegram.
/// Only the first decision acts; one made while it runs, or after, is answered
/// "already handled" and changes nothing — except a ✅ the calendar could not
/// take ([`Held`]): the appointment still waits, and as the press took the
/// Telegram card's buttons off, a new card goes out to press again.
pub async fn decide(
    http: &Client,
    rt: &RuntimeSettings,
    store: &TaskStore,
    calls: &CallStore,
    control: Option<&ControlRegistry>,
    task_id: &str,
    confirm: bool,
) -> Result<(TaskRecord, String)> {
    let cal = cal_for_task(rt, store, task_id).await;
    let (task, text, done) = decide_inner(cal, http, rt, store, calls, control, task_id, confirm).await?;
    if done == Decision::Held {
        resend_card(http, rt, store, &task).await;
    }
    Ok((task, text))
}

/// A new Telegram card for an appointment that still waits after a ✅ on its
/// card (the press took the old card's buttons off).
async fn resend_card(http: &Client, rt: &RuntimeSettings, store: &TaskStore, task: &TaskRecord) {
    let token = rt.telegram_bot_token.trim();
    let chat = rt.telegram_chat_id.trim();
    let Some(appt) = task.appointment.as_ref() else {
        return;
    };
    if !crate::tg_inbound::task_card_ready(true, token, chat) {
        return;
    }
    let p = texts();
    let (title, body) = card_text(rt, task, appt, "");
    match crate::tg_inbound::send_task_card_labeled(
        http, token, chat, &title, &task.id, &body, p.ap_confirm, p.ap_decline,
    )
    .await
    {
        Ok(message_id) if message_id != 0 => {
            let card = TgCard { chat_id: chat.to_string(), message_id };
            if keep_card(store, &task.id, &card, CARD_SETTLE_WAIT).await == CardFate::Close {
                crate::tg_inbound::remove_inline_keyboard(http, token, &card.chat_id, card.message_id).await;
            }
        }
        Ok(_) => {}
        Err(e) => warn!("appointment card (again) failed task_id={}: {e:#}", task.id),
    }
}

/// The calendar a task's appointment is in ([`Appointment::calendar`]): a test
/// number's appointment stays on the demo calendar.
async fn cal_for_task(rt: &RuntimeSettings, store: &TaskStore, task_id: &str) -> Option<Cal> {
    let calendar = store
        .get(task_id)
        .await
        .and_then(|t| t.appointment)
        .map(|a| a.calendar)
        .unwrap_or_default();
    Cal::for_calendar(rt, &calendar)
}

/// [`decide`] for a decision made on the web console or through the API. When
/// it acted and a Telegram card went out for the appointment, the card's
/// buttons are taken off and the result is posted in that chat, as after a
/// press on the card (best effort, in the background: Telegram failing changes
/// nothing about the decision). A ✅ the calendar could not take leaves the
/// buttons on (it can be pressed again there); only the result is posted.
/// `reply`: how the caller is told, as the owner picked it (None: the default).
#[allow(clippy::too_many_arguments)]
pub async fn decide_from_web(
    http: &Client,
    rt: &RuntimeSettings,
    store: &TaskStore,
    calls: &CallStore,
    control: Option<&ControlRegistry>,
    task_id: &str,
    confirm: bool,
    reply: Option<Reply>,
) -> Result<(TaskRecord, String)> {
    let cal = cal_for_task(rt, store, task_id).await;
    let (task, text, done) = decide_inner_with(cal, http, rt, store, calls, control, task_id, confirm, reply).await?;
    if let Some(card) = card_to_clear(&rt.telegram_bot_token, task.appointment.as_ref(), done != Decision::Already) {
        let close = done == Decision::Settled;
        let (http, token, reply, id) = (http.clone(), rt.telegram_bot_token.trim().to_string(), text.clone(), task.id.clone());
        tokio::spawn(async move {
            if close {
                crate::tg_inbound::remove_inline_keyboard(&http, &token, &card.chat_id, card.message_id).await;
            }
            match crate::tg_inbound::send_plain_message(&http, &token, &card.chat_id, &reply).await {
                Ok(()) => info!("appointment card followed a web decision task_id={id} closed={close}"),
                Err(e) => warn!("appointment card follow-up failed task_id={id}: {e:#}"),
            }
        });
    }
    Ok((task, text))
}

/// The Telegram card to follow up after a decision made elsewhere: only when
/// this decision acted (not "already handled"), a card went out and the bot is
/// still set up.
pub fn card_to_clear(bot_token: &str, appt: Option<&Appointment>, acted: bool) -> Option<TgCard> {
    if !acted || bot_token.trim().is_empty() {
        return None;
    }
    appt?.tg_card.clone().filter(|c| !c.chat_id.trim().is_empty() && c.message_id != 0)
}

#[allow(clippy::too_many_arguments)]
async fn decide_with(
    cal: Option<Cal>,
    http: &Client,
    rt: &RuntimeSettings,
    store: &TaskStore,
    calls: &CallStore,
    control: Option<&ControlRegistry>,
    task_id: &str,
    confirm: bool,
) -> Result<(TaskRecord, String)> {
    decide_inner(cal, http, rt, store, calls, control, task_id, confirm)
        .await
        .map(|(task, text, _)| (task, text))
}

/// What an owner's decision did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    /// Confirmed or declined: calendar, text, task and notes follow it.
    Settled,
    /// ✅, but the calendar could not take it ([`Held`]): it still waits,
    /// nothing was texted, and it can be decided again.
    Held,
    /// Decided before, or a decision is under way: nothing changed.
    Already,
}

/// Why a ✅ left the appointment waiting.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Held {
    /// Something else is in the calendar at that time now (another caller's
    /// appointment booked meanwhile, or the owner's own entry for it). The next
    /// ✅ books it anyway ([`Appointment::held_busy`]).
    Taken,
    /// The calendar did not take the write: why.
    Failed(String),
}

impl Held {
    /// What the owner is told (the reply, the card, the web console).
    fn line(&self, lang: Lang) -> String {
        let (failed, taken) = held_phrases(lang);
        match self {
            Held::Taken => taken.to_string(),
            Held::Failed(why) => locale::fill(failed, why),
        }
    }

    /// The task's event.
    fn event(&self) -> &'static str {
        match self {
            Held::Taken => "appointment_time_taken",
            Held::Failed(_) => "appointment_write_failed",
        }
    }
}

/// What the owner reads when an "accept" on the demo calendar is held, in each
/// language: the write failed (`{}` = why), the time is not free (and another
/// "accept" books it anyway).
fn held_phrases(lang: Lang) -> (&'static str, &'static str) {
    match lang {
        Lang::ZhTw => (
            "⚠️ 寫入日曆失敗，還沒傳簡訊，預約仍待確認，可再按一次同意：{}",
            "⚠️ 這個時段日曆上已經有行程：還沒寫入日曆、也沒傳簡訊，預約仍待確認。再按一次同意就照樣排入",
        ),
        Lang::ZhCn => (
            "⚠️ 写入日历失败，还没发短信，预约仍待确认，可再按一次同意：{}",
            "⚠️ 这个时段日历上已经有日程：还没写入日历、也没发短信，预约仍待确认。再按一次同意就照样排入",
        ),
        Lang::En => (
            "⚠️ Couldn't write it to the calendar, so no text went out; it still waits, press Accept again: {}",
            "⚠️ There is already something in your calendar at that time: nothing written, no text sent, it still waits. Press Accept again to book it anyway",
        ),
        Lang::Ja => (
            "⚠️ カレンダーへの書き込みに失敗したため SMS は送っていません。予約は確認待ちのままで、もう一度「承諾」を押せます：{}",
            "⚠️ この時間はカレンダーにすでに予定があります。書き込みも SMS の送信もしておらず、予約は確認待ちのままです。もう一度「承諾」を押すとそのまま登録します",
        ),
        Lang::Ko => (
            "⚠️ 캘린더에 기록하지 못해 문자는 보내지 않았습니다. 예약은 확인 대기 중이며 수락을 다시 누를 수 있습니다: {}",
            "⚠️ 그 시간에는 캘린더에 이미 일정이 있습니다. 기록하지 않았고 문자도 보내지 않았으며 예약은 확인 대기 중입니다. 수락을 한 번 더 누르면 그대로 등록합니다",
        ),
        Lang::Es => (
            "⚠️ No se pudo guardar en el calendario y no se envió el SMS; la cita sigue pendiente, puedes pulsar Aceptar otra vez: {}",
            "⚠️ A esa hora ya hay algo en tu calendario: no se guardó nada ni se envió SMS; la cita sigue pendiente. Pulsa Aceptar otra vez para agendarla igualmente",
        ),
        Lang::Fr => (
            "⚠️ Échec de l'écriture dans l'agenda, aucun SMS envoyé ; le rendez-vous reste en attente, vous pouvez appuyer à nouveau sur Accepter : {}",
            "⚠️ Il y a déjà quelque chose dans votre agenda à cette heure : rien n'a été écrit ni envoyé par SMS ; le rendez-vous reste en attente. Appuyez à nouveau sur Accepter pour l'ajouter quand même",
        ),
        Lang::De => (
            "⚠️ Eintragen in den Kalender fehlgeschlagen, keine SMS gesendet; der Termin wartet weiter, du kannst noch einmal auf Zusagen drücken: {}",
            "⚠️ Zu dieser Zeit steht schon etwas in deinem Kalender: nichts eingetragen, keine SMS gesendet; der Termin wartet weiter. Drück noch einmal auf Zusagen, um ihn trotzdem einzutragen",
        ),
        Lang::Pt => (
            "⚠️ Não foi possível gravar na agenda e nenhum SMS foi enviado; o compromisso continua pendente, pode tocar em Aceitar de novo: {}",
            "⚠️ Já há algo na sua agenda nesse horário: nada foi gravado nem enviado por SMS; o compromisso continua pendente. Toque em Aceitar de novo para marcar assim mesmo",
        ),
    }
}

/// An earlier write of this appointment may be in the calendar without its id
/// (`auto`, or a ✅ whose write timed out): looked up by its tag.
fn may_have_written(appt: &Appointment) -> bool {
    !appt.tag.is_empty() && (appt.mode == Mode::Auto.as_str() || !appt.write_error.is_empty())
}

/// The appointment's own event in the calendar: its id, or one an earlier write
/// left (found by its tag). Empty: none.
async fn own_event(cal: &Cal, http: &Client, appt: &Appointment) -> String {
    if !appt.event_id.is_empty() || !may_have_written(appt) {
        return appt.event_id.clone();
    }
    match tokio::time::timeout(BOOK_CHECK_TIMEOUT, cal.find_tagged(http, &appt.tag)).await {
        Ok(Ok(found)) => found.into_iter().next().unwrap_or_default(),
        Ok(Err(e)) => {
            warn!("appointment tagged lookup failed: {e:#}");
            String::new()
        }
        Err(_) => {
            warn!("appointment tagged lookup timed out");
            String::new()
        }
    }
}

/// ✅: put the appointment in the calendar at its time — while that time is
/// still free apart from its own event: another call's appointment may have
/// taken it since. Checked and written as one step for every call at once
/// ([`calendar_turn`], held at most check + write, 15 s; its own event is
/// looked up before). A check that cannot answer (no read access, slow) does
/// not stop the write, nor does a busy time the owner was already told about
/// ([`Appointment::held_busy`]). The event's id.
async fn confirm_in_calendar(
    cal: &Cal,
    http: &Client,
    appt: &Appointment,
    caller_id: &str,
) -> std::result::Result<String, Held> {
    let own = own_event(cal, http, appt).await;
    let _turn = calendar_turn().lock().await;
    if appt.held_busy {
        info!("appointment confirmed again after a busy time: written anyway");
    } else {
        match tokio::time::timeout(BOOK_CHECK_TIMEOUT, cal.is_free_except(http, &appt.start, &appt.end, &own)).await {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => return Err(Held::Taken),
            Ok(Err(e)) => warn!("appointment re-check before writing failed (written anyway): {e:#}"),
            Err(_) => warn!("appointment re-check before writing timed out (written anyway)"),
        }
    }
    match tokio::time::timeout(WRITE_TIMEOUT, cal.write_at(http, appt, &own, caller_id)).await {
        Ok(Ok(id)) => Ok(id),
        Ok(Err(e)) => Err(Held::Failed(format!("{e:#}"))),
        Err(_) => Err(Held::Failed(calendar_slow(WRITE_TIMEOUT.as_secs()))),
    }
}

/// [`decide_with`], also saying what this decision did (the caller is told
/// the default way).
#[allow(clippy::too_many_arguments)]
async fn decide_inner(
    cal: Option<Cal>,
    http: &Client,
    rt: &RuntimeSettings,
    store: &TaskStore,
    calls: &CallStore,
    control: Option<&ControlRegistry>,
    task_id: &str,
    confirm: bool,
) -> Result<(TaskRecord, String, Decision)> {
    decide_inner_with(cal, http, rt, store, calls, control, task_id, confirm, None).await
}

/// [`decide_inner`] with the way the caller is told: `reply` as the owner
/// picked it (one that cannot reach this caller tells nobody), None: the
/// default for the number ([`default_reply`]).
#[allow(clippy::too_many_arguments)]
async fn decide_inner_with(
    cal: Option<Cal>,
    http: &Client,
    rt: &RuntimeSettings,
    store: &TaskStore,
    calls: &CallStore,
    control: Option<&ControlRegistry>,
    task_id: &str,
    confirm: bool,
    reply: Option<Reply>,
) -> Result<(TaskRecord, String, Decision)> {
    let lang = locale::notify_lang();
    let p = locale::phrases(lang);
    let already = |task: TaskRecord| {
        let headline = task.appointment.as_ref().map(|a| a.headline(lang)).unwrap_or_default();
        (task, format!("{}\n{headline}", p.ap_already).trim_end().to_string(), Decision::Already)
    };
    let task = store
        .get(task_id)
        .await
        .with_context(|| format!("task not found: {task_id}"))?;
    task.appointment.as_ref().context("not an appointment task")?;
    // The card bookkeeping holds the claim for a few milliseconds: wait that out
    // instead of calling a decision nobody made "already handled".
    let Some(_claim) = Deciding::claim_within(task_id, Duration::from_secs(2)).await else {
        info!("appointment decision ignored task_id={task_id} confirm={confirm}: another one is under way");
        return Ok(already(task));
    };
    // Read again under the claim: an earlier decision may have just finished.
    let task = store
        .get(task_id)
        .await
        .with_context(|| format!("task not found: {task_id}"))?;
    let mut appt = task
        .appointment
        .clone()
        .context("not an appointment task")?;
    if appt.status != PENDING {
        return Ok(already(task));
    }
    let headline = appt.headline(lang);
    let mut lines: Vec<String> = Vec::new();
    if confirm {
        // Only a test number's appointment is in a calendar here (the demo
        // one); everyone else's goes in the owner's own with the link below.
        if let Some(cal) = &cal {
            match confirm_in_calendar(cal, http, &appt, &task.caller_id).await {
                Ok(id) => {
                    appt.replaced.retain(|e| e.event_id != id);
                    appt.event_id = id;
                    lines.push(p.ap_booked.to_string());
                    store.push_event(task_id, "calendar_event_created").await?;
                }
                Err(held) => {
                    // Not settled: no text to the caller, and it can be decided again.
                    let line = held.line(lang);
                    warn!("appointment confirm held task_id={task_id}: {held:?}");
                    appt.write_error = line.clone();
                    // Told the time is not free: the next ✅ books it anyway.
                    appt.held_busy = held == Held::Taken;
                    store.push_event(task_id, held.event()).await?;
                    let updated = store
                        .patch(
                            task_id,
                            TaskPatch {
                                appointment: Some(appt.clone()),
                                fulfill_error: Some(Some(line.clone())),
                                ..Default::default()
                            },
                        )
                        .await?;
                    update_call_notes(calls, &task.call_id, &appt).await;
                    return Ok((updated, format!("{line}\n{headline}"), Decision::Held));
                }
            }
        }
        store.push_event(task_id, "appointment_confirmed").await?;
        appt.write_error.clear();
        appt.held_busy = false;
        appt.status = CONFIRMED.to_string();
    } else {
        store.push_event(task_id, "appointment_declined").await?;
        // Its own event (auto mode, a time it could not move to, or one whose
        // write answer was lost and is found by its tag): not kept.
        if let Some(cal) = &cal {
            let mut ids: Vec<String> = Vec::new();
            if !appt.event_id.is_empty() {
                ids.push(appt.event_id.clone());
            } else if may_have_written(&appt) {
                match cal.find_tagged(http, &appt.tag).await {
                    Ok(found) => ids = found,
                    Err(e) => warn!("appointment tagged lookup failed task_id={task_id}: {e:#}"),
                }
            }
            for id in ids {
                match cal.delete(http, &id).await {
                    Ok(()) => {
                        lines.push(p.ap_cal_removed.to_string());
                        appt.event_id.clear();
                        store.push_event(task_id, "calendar_event_deleted").await?;
                    }
                    Err(e) => {
                        warn!("appointment calendar delete failed task_id={task_id}: {e:#}");
                        lines.push(locale::fill(p.ap_void_failed, &appt.headline(lang)));
                    }
                }
            }
        }
        appt.write_error.clear();
        appt.status = DECLINED.to_string();
    }
    // The answer reaches the caller the way the owner picked, else the
    // default for this number (setting `appointment_reply`).
    let region = caller_region(rt, &appt, &task.serial);
    let opts = reply_options(rt, &task.caller_id, region.as_deref());
    let way = match reply {
        Some(w) if opts.allows(w) => w,
        Some(w) => {
            warn!("appointment reply {} cannot reach this caller task_id={task_id}: nobody told", w.as_str());
            Reply::None
        }
        None => default_reply(&rt.appointment_reply, opts),
    };
    appt.reply = way.as_str().to_string();
    match way {
        Reply::Sms => {
            let (code, line) = text_caller(rt, control, &task, &appt, confirm, None).await;
            appt.sms = code;
            lines.push(line);
            store.push_event(task_id, &format!("sms_{}", appt.sms)).await?;
        }
        Reply::Call => {
            let (code, line) = call_caller(rt, control, &task, &appt, confirm).await;
            appt.call = code;
            lines.push(line);
            store.push_event(task_id, &format!("callback_{}", appt.call)).await?;
        }
        Reply::None => {
            let w = reply_words(lang);
            lines.push(if opts.call { w.none } else { w.hidden }.to_string());
        }
    }
    if confirm {
        // One tap puts it in the owner's own calendar (the one they chose).
        let link = calendar_link(rt, &task, &appt);
        if !link.is_empty() {
            lines.push(format!("📅 {}", locale::fill(p.ap_cal_link, &link)));
        }
    }
    store
        .patch(
            task_id,
            TaskPatch {
                appointment: Some(appt.clone()),
                calendar_event_id: (!appt.event_id.is_empty()).then(|| appt.event_id.clone()),
                fulfill_error: Some(None),
                ..Default::default()
            },
        )
        .await?;
    let status = if confirm { TaskStatus::Fulfilled } else { TaskStatus::Ignored };
    let updated = store.update_status(task_id, status).await?;
    update_call_notes(calls, &task.call_id, &appt).await;
    info!(
        "appointment decided task_id={task_id} confirm={confirm} event={} sms={:?}",
        !appt.event_id.is_empty(),
        appt.sms
    );
    let head = if confirm { p.ap_confirmed } else { p.ap_declined };
    let mut text = format!("{head}\n{headline}");
    for l in lines {
        text.push('\n');
        text.push_str(&l);
    }
    Ok((updated, text, Decision::Settled))
}

/// What [`retry_sms`] did.
#[derive(Debug)]
pub enum RetrySms {
    /// Sent again: the new [`Appointment::sms`] code (`sent`, or why not), the
    /// owner's line and the task as it stands now.
    Tried { sms: String, line: String, task: TaskRecord },
    /// Not to be sent again (not decided yet, or its text already went or was
    /// never to go, or a decision holds the task): the code it has.
    NotAllowed { sms: String },
}

/// A decided appointment whose text did not go out, or may not have (its
/// [`Appointment::sms`] is a failure code): it may be sent again — not to a
/// number that takes no texts (`no_number`), which would only fail again.
pub fn sms_retryable(appt: &Appointment) -> bool {
    matches!(appt.status.as_str(), BOOKED | CONFIRMED | DECLINED)
        && !matches!(appt.sms.as_str(), "" | "sent" | "no_number")
}

/// Text the caller of a decided appointment again (`POST
/// /calls/api/appointments/{id}/retry-sms`), saying what the decision said:
/// the time is fine, or it is not. `serial`: another phone to send it from
/// (None: the one that took the call). Not while a decision holds the task.
pub async fn retry_sms(
    rt: &RuntimeSettings,
    store: &TaskStore,
    calls: &CallStore,
    control: Option<&ControlRegistry>,
    task_id: &str,
    serial: Option<&str>,
) -> Result<RetrySms> {
    let Some(_claim) = Deciding::claim_within(task_id, Duration::from_secs(2)).await else {
        let sms = store
            .get(task_id)
            .await
            .and_then(|t| t.appointment)
            .map(|a| a.sms)
            .unwrap_or_default();
        return Ok(RetrySms::NotAllowed { sms });
    };
    let task = store
        .get(task_id)
        .await
        .with_context(|| format!("task not found: {task_id}"))?;
    let mut appt = task.appointment.clone().context("not an appointment task")?;
    if !sms_retryable(&appt) {
        return Ok(RetrySms::NotAllowed { sms: appt.sms });
    }
    let confirmed = appt.status != DECLINED;
    let (code, line) = text_caller(rt, control, &task, &appt, confirmed, serial).await;
    appt.sms = code.clone();
    store.push_event(task_id, &format!("sms_{code}")).await?;
    let task = store
        .patch(
            task_id,
            TaskPatch {
                appointment: Some(appt.clone()),
                ..Default::default()
            },
        )
        .await?;
    update_call_notes(calls, &task.call_id, &appt).await;
    info!("appointment sms sent again task_id={task_id} serial={serial:?} sms={code}");
    Ok(RetrySms::Tried { sms: code, line, task })
}

/// The events a voided appointment left in the calendar, also one whose id was
/// lost (found by the call's tag).
async fn voided_ids(cal: &Cal, http: &Client, e: &Earlier) -> Result<Vec<String>> {
    if !e.event_id.is_empty() {
        return Ok(vec![e.event_id.clone()]);
    }
    if e.tag.is_empty() {
        return Ok(Vec::new());
    }
    cal.find_tagged(http, &e.tag).await
}

/// After the call: delete the events of appointments that no longer stand (a
/// sales / scam call, or the AI never really answered). Returns those that
/// could not be deleted, for a line in the summary ([`void_failed_line`]).
/// Each is deleted from the calendar it is in ([`Earlier::calendar`]).
pub async fn delete_voided(http: &Client, rt: &RuntimeSettings, voided: &[Earlier]) -> Vec<Earlier> {
    let (demo, other): (Vec<Earlier>, Vec<Earlier>) =
        voided.iter().cloned().partition(|e| e.calendar == DEMO_CALENDAR);
    // Events an older agent wrote to Google Calendar: nothing here can reach
    // them now, so the summary asks the owner to delete them.
    let mut failed = delete_voided_with(None, http, &other).await;
    if !demo.is_empty() {
        failed.extend(delete_voided_with(Cal::for_calendar(rt, DEMO_CALENDAR), http, &demo).await);
    }
    failed
}

async fn delete_voided_with(cal: Option<Cal>, http: &Client, voided: &[Earlier]) -> Vec<Earlier> {
    if voided.is_empty() {
        return Vec::new();
    }
    let Some(cal) = cal else {
        // Nothing can be deleted without access (nothing was written without it either).
        return voided.iter().filter(|e| !e.event_id.is_empty()).cloned().collect();
    };
    let mut failed = Vec::new();
    for e in voided {
        let ids = match voided_ids(&cal, http, e).await {
            Ok(ids) => ids,
            Err(err) => {
                warn!("voided appointment lookup failed: {err:#}");
                failed.push(e.clone());
                continue;
            }
        };
        for id in ids {
            match cal.delete(http, &id).await {
                Ok(()) => info!("voided appointment deleted from the calendar start={}", e.start),
                Err(err) => {
                    warn!("voided appointment delete failed start={}: {err:#}", e.start);
                    failed.push(Earlier { event_id: id, ..e.clone() });
                }
            }
        }
    }
    failed
}

/// The summary's line for an event that is void but still in the calendar.
pub fn void_failed_line(e: &Earlier) -> String {
    let lang = locale::notify_lang();
    format!("📅 {}", locale::fill(locale::phrases(lang).ap_void_failed, &e.when(lang)))
}

/// The summary line for an event deleted because the summary judged the call
/// a sales call afterwards.
pub fn sales_void_line(e: &Earlier) -> String {
    let lang = locale::notify_lang();
    format!("📅 {}", locale::fill(locale::phrases(lang).ap_sales_voided, &e.when(lang)))
}

/// Everything an appointment put in the calendar (the event and earlier ones,
/// also those of the call's other appointments in [`Appointment::more`]), for
/// when the whole call's appointments no longer stand.
pub fn all_events(appt: &Appointment) -> Vec<Earlier> {
    let mut out: Vec<Earlier> = Vec::new();
    for a in appt.each() {
        out.extend(a.replaced.iter().cloned());
        if !a.event_id.is_empty() || a.write_unsure {
            out.push(Earlier::of(&a));
        }
    }
    out
}

/// The web console's call notes follow the appointment's state (one of the
/// call's appointments; the others stay as they are).
async fn update_call_notes(calls: &CallStore, call_id: &str, appt: &Appointment) {
    let Some(mut notes) = calls.load_notes(call_id).await else {
        return;
    };
    let merged = merged_into(notes.appointment.take(), appt);
    notes.calendar = Some(merged.calendar_event());
    notes.appointment = Some(merged);
    if let Err(e) = calls.save_notes(call_id, &notes).await {
        warn!("appointment notes update failed call_id={call_id}: {e:#}");
    }
}

/// The call's record of what it agreed to (`call`: its first appointment, the
/// others in [`Appointment::more`]) with one of them brought up to date.
fn merged_into(call: Option<Appointment>, appt: &Appointment) -> Appointment {
    let mut one = appt.clone();
    one.more.clear();
    let Some(mut first) = call else {
        return one;
    };
    if first.same_as(&one) || first.more.is_empty() {
        one.more = std::mem::take(&mut first.more);
        return one;
    }
    match first.more.iter_mut().find(|m| m.same_as(&one)) {
        Some(m) => *m = one,
        None => first.more.push(one),
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, 0).unwrap()
    }

    fn appt(start: &str, end: &str, title: &str) -> Appointment {
        Appointment {
            start: start.into(),
            end: end.into(),
            title: title.into(),
            mode: "confirm".into(),
            status: PENDING.into(),
            ..Default::default()
        }
    }

    fn session(mode: Mode) -> ApptSession {
        ApptSession {
            mode,
            calendar: None,
            reply: Reply::None,
            reply_by_text: false,
            sms_on: false,
            reply_setting: "sms".into(),
            home: None,
            caller_id: "0912345678".into(),
            http: Client::new(),
            tag: "tag-test".into(),
            turn_wait: CALENDAR_TURN_WAIT,
            book: CallAppointments::new(),
        }
    }

    /// An `auto` session on a stand-in calendar.
    fn auto_session(cal: &Arc<FakeCal>) -> ApptSession {
        ApptSession {
            calendar: Some(Cal::Fake(cal.clone())),
            ..session(Mode::Auto)
        }
    }

    /// A local time this many days and hours from now, `YYYY-MM-DDTHH:00`.
    fn later(days: i64, hour: u32) -> String {
        let d = chrono::Local::now().naive_local().date() + Span::days(days);
        d.and_hms_opt(hour, 0, 0).unwrap().format(STAMP).to_string()
    }

    fn propose_call(start: &str, end: &str) -> ToolCall {
        ToolCall {
            id: "p".into(),
            name: PROPOSE.into(),
            args: json!({"start": start, "end": end, "title": "看房"}),
            seq: 0,
            after: 0,
        }
    }

    fn count(n: &std::sync::atomic::AtomicUsize) -> usize {
        FakeCal::count(n)
    }

    fn task_for(caller_id: &str, a: &Appointment) -> TaskRecord {
        TaskRecord {
            id: "t".into(),
            created_at: String::new(),
            updated_at: String::new(),
            call_id: "c".into(),
            serial: "pixel-1".into(),
            caller_id: caller_id.into(),
            status: TaskStatus::Pending,
            intent: TaskIntent::Appointment,
            summary: String::new(),
            transcript: String::new(),
            datetime_hint: None,
            reply_method: ReplyMethod::None,
            reply_contact: None,
            contact_verified: false,
            voicemail_path: None,
            calendar_event_id: None,
            fulfill_error: None,
            openclaw_ref: None,
            events: vec![],
            appointment: Some(a.clone()),
        }
    }

    #[test]
    fn mode_parses_and_unknown_is_off() {
        assert_eq!(Mode::parse(" Confirm "), Mode::Confirm);
        // "auto" (booking on Google Calendar) is gone: an old setting asks the owner.
        assert_eq!(Mode::parse("AUTO"), Mode::Confirm);
        assert_eq!(Mode::parse(Mode::Auto.as_str()), Mode::Confirm);
        for s in ["", "off", "yes", "on", "automatic"] {
            assert_eq!(Mode::parse(s), Mode::Off, "{s}");
        }
    }

    #[test]
    fn appointments_follow_the_setting_and_no_calendar_is_checked() {
        let http = Client::new();
        let mut rt = RuntimeSettings::from_base(&crate::config::Settings::from_env());
        rt.appointment_test_numbers.clear();
        rt.appointment_mode = "off".into();
        assert!(ApptSession::for_call(&rt, &http, "0912345678").is_none());
        // "confirm", and an "auto" saved by an older agent: the owner decides,
        // nothing is checked; a text goes back only to numbers that take one.
        for mode in ["confirm", "auto"] {
            rt.appointment_mode = mode.into();
            rt.appointment_sms = true;
            let s = ApptSession::for_call(&rt, &http, "0912345678").unwrap();
            assert_eq!((s.mode, s.can_check(), s.reply_by_text), (Mode::Confirm, false, true), "{mode}");
            assert!(s.calendar.is_none());
            assert!(!ApptSession::for_call(&rt, &http, "0225550123").unwrap().reply_by_text);
            assert!(!ApptSession::for_call(&rt, &http, "").unwrap().reply_by_text);
            rt.appointment_sms = false;
            assert!(!ApptSession::for_call(&rt, &http, "0912345678").unwrap().reply_by_text);
        }
    }

    /// Settings with a fresh data folder (the demo calendar's home), no texts.
    fn demo_settings() -> (PathBuf, RuntimeSettings) {
        let root = std::env::temp_dir().join(format!("bageo-demo-cal-{}", uuid::Uuid::new_v4()));
        let mut rt = RuntimeSettings::from_base(&crate::config::Settings::from_env());
        rt.calls_data_dir = root.to_string_lossy().to_string();
        rt.appointment_sms = false;
        rt.appointment_reply = "sms".into();
        (root, rt)
    }

    #[test]
    fn test_numbers_book_on_the_demo_calendar_whatever_the_mode() {
        let http = Client::new();
        let (root, mut rt) = demo_settings();
        rt.appointment_mode = "off".into();
        rt.appointment_test_numbers = vec!["+1 650 555 0100".into()];
        // On the list: auto on the demo calendar, though appointments are off.
        for caller in ["+16505550100", "16505550100", "+1 (650) 555-0100"] {
            let s = ApptSession::for_call(&rt, &http, caller).unwrap();
            assert_eq!((s.mode, s.can_check()), (Mode::Auto, true), "{caller}");
            assert!(matches!(s.calendar, Some(Cal::Demo(_))), "{caller}");
        }
        // Not on the list: as before.
        for caller in ["0912345678", "+16505550101", ""] {
            assert!(ApptSession::for_call(&rt, &http, caller).is_none(), "{caller}");
        }
        // "confirm": the test number still books on the demo calendar; everyone
        // else is asked about, with no calendar.
        rt.appointment_mode = "confirm".into();
        let s = ApptSession::for_call(&rt, &http, "+16505550100").unwrap();
        assert_eq!((s.mode, s.can_check()), (Mode::Auto, true));
        assert!(matches!(s.calendar, Some(Cal::Demo(_))));
        let other = ApptSession::for_call(&rt, &http, "0912345678").unwrap();
        assert_eq!((other.mode, other.can_check()), (Mode::Confirm, false));
        // Nothing was written by making the sessions.
        assert!(!root.exists());
    }

    #[test]
    fn demo_calendar_has_lunch_and_evenings_busy_and_keeps_what_is_written() {
        let (root, rt) = demo_settings();
        let cal = DemoCal::from_settings(&rt);
        assert_eq!(cal.path, root.join("demo_calendar.json"));
        // No file yet: lunch and the evening are busy, the rest is free.
        assert!(!cal.is_free_except(&later(1, 12), &later(1, 13), "").unwrap());
        assert!(!cal.is_free_except(&later(1, 11), &later(1, 13), "").unwrap());
        assert!(!cal.is_free_except(&later(3, 19), &later(3, 20), "").unwrap());
        assert!(cal.is_free_except(&later(1, 10), &later(1, 11), "").unwrap());
        assert!(cal.is_free_except(&later(1, 13), &later(1, 14), "").unwrap());
        assert!(cal.is_free_except(&later(1, 21), &later(1, 22), "").unwrap());
        // Reading writes nothing.
        assert!(!root.exists());
        let mut a = appt(&later(1, 10), &later(1, 11), "看房");
        a.tag = "tag-demo".into();
        let id = cal.insert(&a, "+16505550100").unwrap();
        assert!(!cal.is_free_except(&later(1, 10), &later(1, 11), "").unwrap());
        // Its own event does not clash with itself.
        assert!(cal.is_free_except(&later(1, 10), &later(1, 11), &id).unwrap());
        assert_eq!(cal.find_tagged("tag-demo").unwrap(), vec![id.clone()]);
        assert!(cal.find_tagged("").unwrap().is_empty());
        // Saved in the documented shape; another handle on the folder sees it.
        let file: Value = serde_json::from_str(&std::fs::read_to_string(&cal.path).unwrap()).unwrap();
        assert_eq!(file["busy"].as_array().unwrap().len(), 28);
        assert_eq!(file["busy"][0], json!([later(0, 12), later(0, 13)]));
        assert_eq!(file["events"], json!([[id.as_str(), later(1, 10), later(1, 11), "tag-demo", "看房"]]));
        assert!(!cal.path.with_extension("json.tmp").exists());
        let again = DemoCal::from_settings(&rt);
        assert!(!again.is_free_except(&later(1, 10), &later(1, 11), "").unwrap());
        // Moved, then deleted.
        let b = appt(&later(1, 15), &later(1, 16), "看房");
        again.move_to(&id, &b).unwrap();
        assert!(again.is_free_except(&later(1, 10), &later(1, 11), "").unwrap());
        assert!(!again.is_free_except(&later(1, 15), &later(1, 16), "").unwrap());
        assert!(again.move_to("no-such-event", &b).is_err());
        again.delete(&id).unwrap();
        assert!(again.is_free_except(&later(1, 15), &later(1, 16), "").unwrap());
        assert!(!again.is_free_except(&later(1, 12), &later(1, 13), "").unwrap());
        // A file that is not the demo calendar is not overwritten.
        std::fs::write(&cal.path, "not json").unwrap();
        assert!(cal.is_free_except(&later(1, 10), &later(1, 11), "").is_err());
        assert!(cal.insert(&a, "").is_err());
        assert_eq!(std::fs::read_to_string(&cal.path).unwrap(), "not json");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn demo_appointments_are_decided_and_voided_on_the_demo_calendar() {
        let http = Client::new();
        let (root, mut rt) = demo_settings();
        rt.appointment_mode = "off".into();
        rt.appointment_test_numbers = vec!["+1 650 555 0100".into()];
        let s = ApptSession::for_call(&rt, &http, "+16505550100").unwrap();
        let check = ToolCall {
            id: "c".into(),
            name: CHECK.into(),
            args: json!({ "start": later(2, 12) }),
            seq: 0,
            after: 0,
        };
        assert_eq!(s.run_tool(&check).await, json!({ "free": false }));
        assert_eq!(s.run_tool(&propose_call(&later(2, 10), &later(2, 11))).await, json!({ "status": "booked" }));
        let booked = s.book.outcome().unwrap();
        assert_eq!((booked.status.as_str(), booked.calendar.as_str()), (BOOKED, DEMO_CALENDAR));
        assert!(booked.event_id.starts_with("demo-"), "{}", booked.event_id);
        let demo = DemoCal::from_settings(&rt);
        assert!(!demo.is_free_except(&later(2, 10), &later(2, 11), "").unwrap());
        // Voided after the call: deleted from the demo calendar.
        let voided = all_events(&booked);
        assert_eq!(voided[0].calendar, DEMO_CALENDAR);
        assert!(delete_voided(&http, &rt, &voided).await.is_empty());
        assert!(demo.is_free_except(&later(2, 10), &later(2, 11), "").unwrap());
        // The owner's ✅ on a demo appointment writes to the demo calendar.
        let tasks = TaskStore::new(root.join("tasks"));
        let calls = CallStore::new(root.join("calls"), false, false, 30);
        let mut a = appt(&later(3, 10), &later(3, 11), "看房");
        a.calendar = DEMO_CALENDAR.into();
        let t = tasks
            .create(TaskRecord { id: String::new(), ..task_for("+16505550100", &a) })
            .await
            .unwrap();
        let (done, _) = decide(&http, &rt, &tasks, &calls, None, &t.id, true).await.unwrap();
        assert_eq!(done.status, TaskStatus::Fulfilled);
        let event_id = done.appointment.as_ref().unwrap().event_id.clone();
        assert!(event_id.starts_with("demo-"), "{event_id}");
        assert!(!demo.is_free_except(&later(3, 10), &later(3, 11), "").unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn mobile_numbers_take_texts_and_others_do_not() {
        for n in ["0912345678", "0912-345-678", "+886912345678", "+886 912 345 678", "886912345678", "00886912345678", "+8860912345678"] {
            assert_eq!(sms_number(n).as_deref(), Some("+886912345678"), "{n}");
        }
        for n in ["", "  ", "0225550123", "+886225550123", "anonymous", "private", "091234567", "09123456789", "912345678", "+81 3-1234-5678"] {
            assert_eq!(sms_number(n), None, "{n}");
        }
    }

    #[test]
    fn callers_abroad_and_on_other_sims_are_texted_back() {
        // A US number (Google Voice too): written in full, or as dialled on a US SIM.
        assert_eq!(sms_number_on("+12025550143", None).as_deref(), Some("+12025550143"));
        assert_eq!(sms_number_on("+1 (202) 555-0143", Some("tw")).as_deref(), Some("+12025550143"));
        assert_eq!(sms_number_on("2025550143", Some("us")).as_deref(), Some("+12025550143"));
        assert_eq!(sms_number_on("(202) 555-0143", Some("US")).as_deref(), Some("+12025550143"));
        assert_eq!(sms_number_on("12025550143", Some("us")).as_deref(), Some("+12025550143"));
        // A Taiwanese mobile on a Taiwanese SIM; a landline is called back.
        assert_eq!(sms_number_on("0912345678", Some("tw")).as_deref(), Some("+886912345678"));
        assert_eq!(sms_number_on("0225550123", Some("tw")), None);
        assert_eq!(sms_number_on("02-2555-0123", None), None);
        // A Japanese mobile, in full or on a Japanese SIM; a Tokyo landline is not.
        assert_eq!(sms_number_on("+81 90-1234-5678", None).as_deref(), Some("+819012345678"));
        assert_eq!(sms_number_on("090-1234-5678", Some("jp")).as_deref(), Some("+819012345678"));
        assert_eq!(sms_number_on("+81312345678", None), None);
        // Other countries' mobiles (France); where mobiles cannot be told from
        // landlines (Switzerland), any number of a plausible length.
        assert_eq!(sms_number_on("+33 6 12 34 56 78", None).as_deref(), Some("+33612345678"));
        assert_eq!(sms_number_on("+33 1 23 45 67 89", None), None);
        assert_eq!(sms_number_on("+41 79 123 45 67", None).as_deref(), Some("+41791234567"));
        assert_eq!(sms_number_on("+41 123", None), None);
        // Hidden, cut off or read the wrong way: nothing.
        for (n, sim) in [("", Some("us")), ("private", None), ("Unknown", Some("tw")), ("5550143", Some("us")), ("0912345678", Some("us"))] {
            assert_eq!(sms_number_on(n, sim), None, "{n} {sim:?}");
        }
    }

    #[test]
    fn sms_text_in_every_language_with_and_without_owner() {
        let a = appt("2026-11-14T15:00", "2026-11-14T16:00", "看房");
        let ok = sms_text(Lang::ZhTw, "王小明", &a, true);
        assert_eq!(ok, "您好，我是王小明。約在11/14（週六）15:00沒問題，到時見。");
        let no = sms_text(Lang::ZhTw, "", &a, false);
        assert_eq!(no, "您好。不好意思，11/14（週六）15:00我沒辦法，我們再約別的時間好嗎？");
        // What the caller said the appointment is about never goes out from the
        // owner's number (a link or a chat ID could be slipped into it).
        let bait = appt("2026-11-14T15:00", "2026-11-14T16:00", "加 LINE: scam01 https://example.com/x");
        for l in locale::ALL {
            let with = sms_text(l, "Alex", &a, true);
            let without = sms_text(l, " ", &a, false);
            let hour = if l == Lang::En { "3:00 PM" } else if l == Lang::Fr { "15h00" } else { "15:00" };
            assert!(with.contains("Alex") && with.contains(hour) && !with.contains("看房"), "{l:?}: {with}");
            assert!(!without.contains('{') && !without.contains("Alex") && without.contains(hour), "{l:?}: {without}");
            assert!(with.chars().count() <= SMS_TEXT_MAX && !with.starts_with(' ') && !with.ends_with(' '));
            assert_ne!(sms_text(l, "", &a, true), without, "{l:?}");
            for confirmed in [true, false] {
                let t = sms_text(l, "Alex", &bait, confirmed);
                assert!(!t.contains("LINE") && !t.contains("http") && !t.contains("scam01"), "{l:?}: {t}");
                assert!(t.contains(locale::phrases(l).ap_weekdays[5]), "{l:?}: {t}");
            }
        }
        assert_eq!(
            sms_text(Lang::En, "Sam", &a, true),
            "Hi, it's Sam. Sat, Nov 14, 3:00 PM works for me. See you then!"
        );
        // A message starts on a capital, not on the date ("sáb 14/11: …").
        assert_eq!(sms_text(Lang::Es, "", &a, true), "Hola. El sáb 14/11, 15:00 me va bien. ¡Nos vemos!");
        assert_eq!(sms_text(Lang::Fr, "", &a, true), "Bonjour. Le sam. 14/11 à 15h00, c'est bon pour moi. À bientôt !");
        assert_eq!(sms_text(Lang::Pt, "", &a, true), "Olá. Combinado: sáb, 14/11 15:00. Até lá!");
    }

    #[test]
    fn appointment_phrases_exist_in_every_language() {
        let t = at(2026, 11, 16, 9, 5); // a Monday
        for l in locale::ALL {
            let p = locale::phrases(l);
            for s in [p.ap_sms_hi_owner, p.ap_sms_ok, p.ap_sms_no, p.ap_auto_failed, p.ap_replaced, p.ap_cal_link, p.ap_sms_sent, p.ap_sms_not_sent, p.ap_sms_code, p.ap_void_failed, p.ap_sales_voided] {
                assert_eq!(s.matches("{}").count(), 1, "{l:?}: {s}");
            }
            for s in [p.ap_sms_hi, p.ap_confirm, p.ap_decline, p.ap_card_title, p.ap_what, p.ap_name, p.ap_sms, p.ap_sms_will, p.ap_sms_off, p.ap_sms_cannot, p.ap_pending, p.ap_booked, p.ap_busy, p.ap_confirmed, p.ap_declined, p.ap_sms_old_app, p.ap_sms_offline, p.ap_sms_timeout, p.ap_already, p.ap_sms_busy, p.ap_cal_removed] {
                assert!(!s.trim().is_empty() && !s.contains('{'), "{l:?}: {s}");
            }
            // The buttons are words ("同意" / "不同意"), no ✅ / ❌; nothing says Google.
            assert!(!p.ap_confirm.contains('✅') && !p.ap_decline.contains('❌') && p.ap_confirm != p.ap_decline, "{l:?}");
            for s in [p.ap_cal_removed, p.ap_cal_link, p.ap_booked, p.ap_sms_will] {
                assert!(!s.contains("Google"), "{l:?}: {s}");
            }
            assert_eq!(p.ap_dt_fmt.matches("{}").count(), 1, "{l:?}");
            assert!(!p.ap_end_fmt.contains('{'), "{l:?}");
            let w = when_text(l, &t);
            let hour = if l == Lang::En { "9:05 AM" } else if l == Lang::Fr { "09h05" } else { "09:05" };
            assert!(w.contains(hour) && w.contains(p.ap_weekdays[0]) && !w.contains('%'), "{l:?}: {w}");
        }
        assert_eq!(when_text(Lang::ZhTw, &t), "11/16（週一）09:05");
        assert_eq!(when_text(Lang::En, &t), "Mon, Nov 16, 9:05 AM");
        assert_eq!(when_text(Lang::ZhCn, &t), "11月16日（周一）09:05");
        assert_eq!(when_text(Lang::De, &t), "Mo., 16.11., 09:05");
        let a = appt("2026-11-14T15:00", "2026-11-14T16:30", "看房");
        assert_eq!(a.when(Lang::ZhTw), "11/14（週六）15:00–16:30");
        assert_eq!(a.when(Lang::En), "Sat, Nov 14, 3:00 PM–4:30 PM");
        assert_eq!(a.when(Lang::Fr), "sam. 14/11 à 15h00–16h30");
        assert_eq!(a.headline(Lang::ZhTw), "11/14（週六）15:00–16:30 看房");
    }

    #[test]
    fn tool_calls_parse_and_answers_are_shaped_for_live() {
        let msg = json!({"toolCall": {"functionCalls": [
            {"id": "fc-1", "name": "check_availability", "args": {"start": "2026-11-14T15:00"}},
            {"id": "fc-2", "name": "propose_appointment"},
            {"id": "fc-3"}
        ]}});
        let calls = parse_tool_calls(&msg);
        assert_eq!(calls.len(), 2);
        assert_eq!(
            (calls[0].id.as_str(), calls[0].name.as_str(), &calls[0].args),
            ("fc-1", CHECK, &json!({"start": "2026-11-14T15:00"}))
        );
        // Numbered in the order they came, before any of them runs.
        assert!(calls[0].seq > 0 && calls[1].seq > calls[0].seq, "{calls:?}");
        assert_eq!(calls[1].args, json!({}));
        assert!(parse_tool_calls(&json!({"serverContent": {"turnComplete": true}})).is_empty());
        assert_eq!(
            parse_tool_cancellation(&json!({"toolCallCancellation": {"ids": ["fc-1", "fc-2"]}})),
            vec!["fc-1", "fc-2"]
        );
        assert!(parse_tool_cancellation(&msg).is_empty());
        let r = tool_response("fc-1", CHECK, json!({"free": true}));
        assert_eq!(
            r,
            json!({"toolResponse": {"functionResponses": [{"id": "fc-1", "name": "check_availability", "response": {"free": true}}]}})
        );
        // propose_appointment's answer is silent (the model already said it).
        let silent = tool_response("fc-2", PROPOSE, json!({"status": "pending_owner_confirmation"}));
        assert_eq!(
            silent,
            json!({"toolResponse": {"functionResponses": [{"id": "fc-2", "name": "propose_appointment",
                "response": {"status": "pending_owner_confirmation", "scheduling": "SILENT"}}]}})
        );
        assert!(r["toolResponse"]["functionResponses"][0]["response"].get("scheduling").is_none());
        // auto could not book what it agreed to: the model gets to say so.
        let speak = tool_response("fc-3", PROPOSE, json!({"status": "pending_owner_confirmation", "not_booked": true}));
        assert_eq!(speak["toolResponse"]["functionResponses"][0]["response"]["scheduling"], "WHEN_IDLE");
        // NON_BLOCKING only on propose_appointment, and only when asked for.
        let nb = tool_declarations(true);
        assert_eq!(nb[0]["functionDeclarations"][1]["behavior"], "NON_BLOCKING");
        assert!(nb[0]["functionDeclarations"][0].get("behavior").is_none());
        assert!(!is_blocking(PROPOSE, true) && is_blocking(PROPOSE, false) && is_blocking(CHECK, true));
        let decl = tool_declarations(false);
        assert!(!decl.to_string().contains("behavior"));
        let names: Vec<&str> = decl[0]["functionDeclarations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec![CHECK, PROPOSE]);
        assert_eq!(decl[0]["functionDeclarations"][1]["parameters"]["required"], json!(["start", "title"]));
    }

    #[test]
    fn function_times_are_checked() {
        let now = at(2026, 11, 13, 14, 0);
        let (s, e) = window(&json!({"start": "2026-11-14T15:00"}), now).unwrap();
        assert_eq!((stamp(s), stamp(e)), ("2026-11-14T15:00".to_string(), "2026-11-14T16:00".to_string()));
        let (_, e) = window(&json!({"start": "2026-11-14 15:00", "end": "2026-11-14T15:30"}), now).unwrap();
        assert_eq!(stamp(e), "2026-11-14T15:30");
        // An end before the start or days later is one hour instead.
        let (_, e) = window(&json!({"start": "2026-11-14T15:00", "end": "2026-11-14T14:00"}), now).unwrap();
        assert_eq!(stamp(e), "2026-11-14T16:00");
        assert_eq!(window(&json!({"start": "2026-11-12T15:00"}), now), Err("start_in_past"));
        assert_eq!(window(&json!({"start": "2028-11-12T15:00"}), now), Err("start_too_far"));
        assert_eq!(window(&json!({"start": "tomorrow 3pm"}), now), Err("bad_start"));
        assert_eq!(window(&json!({}), now), Err("bad_start"));
    }

    #[test]
    fn a_wrong_weekday_is_sent_back_with_the_real_one() {
        let start = at(2026, 11, 19, 19, 0); // a Thursday
        let e = weekday_mismatch(&json!({"weekday": "Wednesday"}), start).unwrap();
        assert_eq!(e["error"], "weekday_mismatch");
        assert_eq!(e["date_is"], "Thursday");
        for ok in [json!({"weekday": "Thursday"}), json!({"weekday": "thu"}), json!({}), json!({"weekday": "週四"})] {
            assert_eq!(weekday_mismatch(&ok, start), None, "{ok}");
        }
        // The refusal makes the model speak (call again), never silently.
        let r = tool_response("p", PROPOSE, e);
        assert_eq!(r["toolResponse"]["functionResponses"][0]["response"]["scheduling"], "WHEN_IDLE");
        let decl = tool_declarations(true);
        for f in decl[0]["functionDeclarations"].as_array().unwrap() {
            assert!(f["parameters"]["properties"].get("weekday").is_some(), "{f}");
        }
    }

    #[tokio::test]
    async fn a_proposal_on_the_wrong_weekday_is_not_recorded() {
        let s = session(Mode::Confirm);
        let d = chrono::Local::now().naive_local().date() + Span::days(3);
        let start = d.and_hms_opt(19, 0, 0).unwrap();
        let wrong = start.weekday().succ().to_string();
        let call = ToolCall {
            id: "w".into(),
            name: PROPOSE.into(),
            args: json!({"start": stamp(start), "weekday": wrong, "title": "dinner"}),
            seq: 0,
            after: 0,
        };
        let r = s.run_tool(&call).await;
        assert_eq!(r["error"], "weekday_mismatch");
        assert_eq!(r["date_is"], start.format("%A").to_string());
        assert_eq!(s.book.outcome(), None);
        let right = ToolCall {
            args: json!({"start": stamp(start), "weekday": start.format("%A").to_string(), "title": "dinner"}),
            ..call
        };
        assert_eq!(s.run_tool(&right).await["status"], "pending_owner_confirmation");
        assert_eq!(s.book.outcome().unwrap().start, stamp(start));
    }

    #[tokio::test]
    async fn without_calendar_check_is_unknown_and_proposals_wait_for_the_owner() {
        let s = session(Mode::Confirm);
        let start = (chrono::Local::now().naive_local() + Span::days(2)).format(STAMP).to_string();
        let check = ToolCall { id: "1".into(), name: CHECK.into(), args: json!({"start": start}), seq: 0, after: 0 };
        assert_eq!(s.run_tool(&check).await, json!({"free": null, "reason": "calendar_unavailable"}));
        let bad = ToolCall { id: "2".into(), name: CHECK.into(), args: json!({"start": "soon"}), seq: 0, after: 0 };
        assert_eq!(s.run_tool(&bad).await["error"], "bad_start");
        let propose = ToolCall {
            id: "3".into(),
            name: PROPOSE.into(),
            args: json!({"start": start, "title": " 看房\n", "caller_name": "陳先生"}),
            seq: 0,
            after: 0,
        };
        let answer = s.run_tool(&propose).await;
        assert_eq!(answer["status"], "pending_owner_confirmation");
        // What the caller hears: checked later, no text promised (texts off).
        let tell = answer["tell_caller"].as_str().unwrap();
        assert!(tell.contains("Let me check and get back to you.") && tell.contains("我確認一下再回你") && !tell.contains("text you"), "{tell}");
        let got = s.book.outcome().unwrap();
        assert_eq!(
            (got.title.as_str(), got.caller_name.as_str(), got.status.as_str(), got.mode.as_str()),
            ("看房", "陳先生", PENDING, "confirm")
        );
        // The last proposal counts.
        let later = (chrono::Local::now().naive_local() + Span::days(3)).format(STAMP).to_string();
        let again = ToolCall { id: "4".into(), name: PROPOSE.into(), args: json!({"start": later, "title": "看房"}), seq: 0, after: 0 };
        s.run_tool(&again).await;
        assert_eq!(s.book.outcome().unwrap().start, later);
        let other = ToolCall { id: "5".into(), name: "other".into(), args: json!({}), seq: 0, after: 0 };
        assert_eq!(s.run_tool(&other).await["error"], "unknown_function");
        // Judged sales / scam: nothing is kept, the model still gets an answer.
        s.book.discard();
        assert_eq!(s.run_tool(&propose).await["status"], "pending_owner_confirmation");
        assert_eq!(s.book.outcome(), None);
    }

    #[test]
    fn replaced_booked_times_are_listed() {
        // The summary judged it a sales call after the call: the deleted event is named.
        let gone = Earlier::of(&appt("2026-11-14T15:00", "2026-11-14T16:00", "看房"));
        assert_eq!(sales_void_line(&gone), "📅 判定是推銷電話，已取消通話中排入的預約：11/14（週六）15:00–16:00 看房");
        let book = CallAppointments::new();
        let mut first = appt("2026-11-14T15:00", "2026-11-14T16:00", "看房");
        first.event_id = "ev1".into();
        first.status = BOOKED.into();
        assert!(book.record(first));
        assert!(book.record(appt("2026-11-15T10:00", "2026-11-15T11:00", "看房")));
        let out = book.outcome().unwrap();
        assert_eq!(out.start, "2026-11-15T10:00");
        assert_eq!(out.replaced.len(), 1);
        assert_eq!(out.replaced[0].event_id, "ev1");
        let line = summary_line(&out, "");
        assert!(line.starts_with("📅 預約待你確認：11/15（週日）10:00–11:00 看房"), "{line}");
        assert!(line.contains("11/14（週六）15:00–16:00 看房"), "{line}");
        let mut booked = out.clone();
        booked.status = BOOKED.into();
        booked.replaced.clear();
        assert_eq!(summary_line(&booked, "http://bageo.local/calls/#call=c1"), "📅 已排入行事曆：11/15（週日）10:00–11:00 看房");
    }

    #[test]
    fn the_pending_line_sends_the_owner_to_the_web_console() {
        let a = appt("2026-11-14T15:00", "2026-11-14T16:00", "看房");
        assert_eq!(
            summary_line(&a, "http://bageo.local/calls/#call=c1"),
            "📅 預約待你確認：11/14（週六）15:00–16:00 看房 — 到網頁確認：http://bageo.local/calls/#call=c1"
        );
        // No address other devices can open: where to go instead.
        assert_eq!(summary_line(&a, " "), "📅 預約待你確認：11/14（週六）15:00–16:00 看房 — 到網頁控制台確認");
        for l in locale::ALL {
            let p = locale::phrases(l);
            assert_eq!(p.ap_web_confirm.matches("{}").count(), 1, "{l:?}");
            assert!(!p.ap_web_console.trim().is_empty() && !p.ap_web_console.contains('{'), "{l:?}");
            assert!(!p.ap_web_label.trim().is_empty() && !p.ap_web_label.contains('{'), "{l:?}");
            assert!(locale::fill(p.ap_web_confirm, "http://x/").ends_with("http://x/"), "{l:?}");
        }
    }

    #[tokio::test]
    async fn cards_go_out_only_with_telegram_and_carry_the_web_link() {
        let mut rt = plain_settings();
        rt.telegram_bot_token = String::new();
        rt.telegram_chat_id = String::new();
        assert!(!crate::tg_inbound::task_card_ready(rt.call_notify, &rt.telegram_bot_token, &rt.telegram_chat_id));
        assert!(!crate::tg_inbound::task_card_ready(false, "123:abc", "42"));
        assert!(crate::tg_inbound::task_card_ready(true, "123:abc", "42"));
        // Without Telegram nothing is sent: the task waits on the web, no card kept.
        let (root, store, calls) = stores();
        let a = appt(&later(2, 15), &later(2, 16), "看房");
        let t = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &a) }).await.unwrap();
        after_summary(&Client::new(), &rt, &store, &calls, &ControlRegistry::default(), &t, "http://x/calls/#call=c").await;
        let kept = store.get(&t.id).await.unwrap();
        assert_eq!(kept.status, TaskStatus::Pending);
        assert_eq!(kept.appointment.and_then(|a| a.tg_card), None);
        // The card names the call's page.
        let (_, body) = card_text(&rt, &t, &a, "http://x/calls/#call=c");
        assert!(body.ends_with("網頁確認：http://x/calls/#call=c"), "{body}");
        assert!(!card_text(&rt, &t, &a, "").1.contains("網頁確認"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_decision_on_the_web_closes_the_telegram_card() {
        let card = TgCard { chat_id: "42".into(), message_id: 7 };
        let mut a = appt("2026-11-14T15:00", "2026-11-14T16:00", "看房");
        a.tg_card = Some(card.clone());
        assert_eq!(card_to_clear("123:abc", Some(&a), true), Some(card.clone()));
        // "Already handled", no bot any more, no card sent, not an appointment: nothing to do.
        assert_eq!(card_to_clear("123:abc", Some(&a), false), None);
        assert_eq!(card_to_clear(" ", Some(&a), true), None);
        assert_eq!(card_to_clear("123:abc", Some(&appt("2026-11-14T15:00", "", "看房")), true), None);
        assert_eq!(card_to_clear("123:abc", None, true), None);
        // The card is kept through the decision; only the first one acts.
        let (root, store, calls) = stores();
        let cal = Arc::new(FakeCal::default());
        let mut b = appt(&later(2, 15), &later(2, 16), "看房");
        b.tg_card = Some(card.clone());
        let t = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &b) }).await.unwrap();
        let http = Client::new();
        let (done, _, acted) = decide_inner(Some(Cal::Fake(cal.clone())), &http, &plain_settings(), &store, &calls, None, &t.id, true)
            .await
            .unwrap();
        assert_eq!(acted, Decision::Settled);
        assert_eq!(card_to_clear("123:abc", done.appointment.as_ref(), acted != Decision::Already), Some(card));
        let (again, text, acted) = decide_inner(Some(Cal::Fake(cal.clone())), &http, &plain_settings(), &store, &calls, None, &t.id, false)
            .await
            .unwrap();
        assert!(acted == Decision::Already && text.starts_with("這個預約已經處理過了"), "{text}");
        assert_eq!(card_to_clear("123:abc", again.appointment.as_ref(), acted != Decision::Already), None);
        // The card stays out of JSON when none went out.
        assert!(serde_json::to_value(appt("2026-11-14T15:00", "", "x")).unwrap().get("tg_card").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_card_that_lands_after_a_web_decision_never_undoes_it() {
        let (root, store, calls) = stores();
        let cal = Arc::new(FakeCal::default());
        let http = Client::new();
        let card = TgCard { chat_id: "42".into(), message_id: 7 };
        // Decided on the web while the card was on its way…
        let a = appt(&later(2, 15), &later(2, 16), "看房");
        let t = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &a) }).await.unwrap();
        let (_, _, acted1) = decide_inner(Some(Cal::Fake(cal.clone())), &http, &plain_settings(), &store, &calls, None, &t.id, true)
            .await
            .unwrap();
        assert_eq!(acted1, Decision::Settled);
        // …then its message id comes back: the decision stands, the buttons come off.
        assert_eq!(keep_card(&store, &t.id, &card, Duration::from_secs(1)).await, CardFate::Close);
        let after = store.get(&t.id).await.unwrap().appointment.unwrap();
        assert_eq!((after.status.as_str(), after.event_id.as_str(), after.tg_card.as_ref()), (CONFIRMED, "ev1", None));
        // A press on the card (or the web again) does nothing: one calendar write.
        let (_, text, acted2) = decide_inner(Some(Cal::Fake(cal.clone())), &http, &plain_settings(), &store, &calls, None, &t.id, true)
            .await
            .unwrap();
        assert!(acted2 == Decision::Already && text.starts_with("這個預約已經處理過了"), "{text}");
        assert_eq!(count(&cal.inserts), 1);

        // Still waiting: the card goes onto the task as it stands now.
        let t2 = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &a) }).await.unwrap();
        assert_eq!(keep_card(&store, &t2.id, &card, Duration::from_secs(1)).await, CardFate::Keep);
        let kept = store.get(&t2.id).await.unwrap().appointment.unwrap();
        assert_eq!((kept.status.as_str(), kept.tg_card), (PENDING, Some(card.clone())));

        // A decision under way: waited for, then its outcome is what counts.
        let t3 = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &a) }).await.unwrap();
        let held = Deciding::claim(&t3.id).unwrap();
        let waiting = {
            let (store, id, card) = (store.clone(), t3.id.clone(), card.clone());
            tokio::spawn(async move { keep_card(&store, &id, &card, Duration::from_secs(5)).await })
        };
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(!waiting.is_finished(), "the card waits for the decision");
        let mut decided = store.get(&t3.id).await.unwrap().appointment.unwrap();
        decided.status = DECLINED.into();
        store.patch(&t3.id, TaskPatch { appointment: Some(decided), ..Default::default() }).await.unwrap();
        drop(held);
        assert_eq!(waiting.await.unwrap(), CardFate::Close);
        assert_eq!(store.get(&t3.id).await.unwrap().appointment.unwrap().status, DECLINED);
        // One that never lets go: the card is closed, nothing written.
        let t4 = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &a) }).await.unwrap();
        let _stuck = Deciding::claim(&t4.id).unwrap();
        assert_eq!(keep_card(&store, &t4.id, &card, Duration::from_millis(200)).await, CardFate::Close);
        assert_eq!(store.get(&t4.id).await.unwrap().appointment.unwrap().tg_card, None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prompt_tells_the_time_and_follows_mode_and_style() {
        let now = at(2026, 11, 13, 14, 5);
        let me = locale::Active { owner: "王先生".into(), ..Default::default() };
        let p = prompt_block(Mode::Confirm, true, Reply::Sms, &me, now, "+08:00");
        assert!(p.contains("2026-11-13 14:05 (Friday)") && p.contains("UTC+08:00"), "{p}");
        // A dated list to count weekdays on: "Wednesday" said on a Friday is the 18th.
        // Today, tomorrow and the day after are named (the model counted a day short).
        assert!(p.contains("today = Fri 2026-11-13, tomorrow = Sat 2026-11-14, the day after tomorrow = Sun 2026-11-15"), "{p}");
        assert!(p.contains("Wed 2026-11-18") && p.contains("Fri 2026-11-27") && p.contains("never count days yourself"), "{p}");
        assert!(!p.contains("Sat 2026-11-28"), "{p}");
        // This week and next week, Monday first: "next week's Wednesday" is looked up.
        assert!(p.contains("This week's Mon..Sun: Mon 2026-11-09, Tue 2026-11-10, Wed 2026-11-11, Thu 2026-11-12, Fri 2026-11-13, Sat 2026-11-14, Sun 2026-11-15."), "{p}");
        assert!(p.contains("Next week's Mon..Sun: Mon 2026-11-16, Tue 2026-11-17, Wed 2026-11-18, Thu 2026-11-19, Fri 2026-11-20, Sat 2026-11-21, Sun 2026-11-22."), "{p}");
        assert!(p.contains("下禮拜三") && p.contains("is next week's Wed"), "{p}");
        // On a Sunday this week still starts on the Monday before.
        let sunday = prompt_block(Mode::Confirm, true, Reply::Sms, &me, at(2026, 11, 15, 9, 0), "+08:00");
        assert!(sunday.contains("This week's Mon..Sun: Mon 2026-11-09,") && sunday.contains("Next week's Mon..Sun: Mon 2026-11-16,"), "{sunday}");
        // Another appointment is not a change of time; the part of the day is checked.
        assert!(p.contains("replaces_previous = true") && p.contains("replaces_previous = false"), "{p}");
        assert!(p.contains("period_mismatch") && p.contains("晚上") && p.contains("evening"), "{p}");
        assert!(p.contains("check_availability") && p.contains("propose_appointment"));
        // What the AI says to the caller is about "you", in the owner's words.
        assert!(p.contains("say \"Let me check and I'll text you back later.\" (in Chinese: 「我確認一下，晚點再用簡訊回你」), in Traditional"), "{p}");
        assert!(p.contains("overrides anything above") && p.contains("王先生"));
        // No waiting on the line; saying it will be checked after the call is that line.
        assert!(p.contains("never say you are looking something up right now") && p.contains("ちょっと待って") && p.contains("never what it is"), "{p}");
        assert!(!p.contains("\"let me check\""), "{p}");
        assert!(p.contains("A back-up time the caller offers") && p.contains("do not call it for that"));
        assert!(p.contains("weekday_mismatch"));
        assert!(p.contains("scam caller"));
        let auto = prompt_block(Mode::Auto, true, Reply::None, &me, now, "+08:00");
        assert!(auto.contains("agree naturally") && !auto.contains("text them"));
        let blind = prompt_block(Mode::Confirm, false, Reply::None, &me, now, "+08:00");
        assert!(blind.contains("do not call check_availability") && blind.contains("do not say it is free"), "{blind}");
        let assistant = locale::Active { assistant: true, ..me };
        let a = prompt_block(Mode::Confirm, true, Reply::None, &assistant, now, "+08:00");
        assert!(a.contains("王先生 already has something on") && a.contains("check with 王先生"));
        assert!(a.contains("\"I'll check with 王先生 and get back to you.\" (in Chinese: 「我跟王先生確認一下再回你」)"), "{a}");
    }

    /// The owner's wording, by how the answer will reach the caller: a text, a
    /// call-back, or — neither can — only "I'll get back to you", nothing promised.
    #[test]
    fn the_ai_promises_a_text_a_call_back_or_nothing() {
        let now = at(2026, 11, 13, 14, 5);
        let me = locale::Active::default();
        let text = prompt_block(Mode::Confirm, false, Reply::Sms, &me, now, "+08:00");
        assert!(text.contains("\"Let me check and I'll text you back later.\" (in Chinese: 「我確認一下，晚點再用簡訊回你」)"), "{text}");
        assert!(!text.contains("never promise a text") && !text.contains("never promise to text"), "{text}");
        let call = prompt_block(Mode::Confirm, false, Reply::Call, &me, now, "+08:00");
        assert!(call.contains("\"Let me check and I'll call you back later.\" (in Chinese: 「我確認一下，晚點再打給你」)"), "{call}");
        assert!(call.contains("never promise a text message") && !call.contains("text you back"), "{call}");
        let none = prompt_block(Mode::Confirm, false, Reply::None, &me, now, "+08:00");
        assert!(none.contains("\"Let me check and get back to you.\" (in Chinese: 「我確認一下再回你」)"), "{none}");
        assert!(none.contains("never promise to text or call them") && !none.contains("text you back") && !none.contains("call you back"), "{none}");
        // Every prompt says it before the proposal and never calls it settled.
        for p in [&text, &call, &none] {
            assert!(p.contains("then call propose_appointment") && p.contains("never say it is booked"), "{p}");
        }
        // Simplified Chinese, English and the assistant style.
        let cn = locale::Active { lang: Lang::ZhCn, ..Default::default() };
        assert!(prompt_block(Mode::Confirm, false, Reply::Sms, &cn, now, "+08:00").contains("「我确认一下，晚点再用短信回你」"));
        assert!(prompt_block(Mode::Confirm, false, Reply::Call, &cn, now, "+08:00").contains("「我确认一下，晚点再打给你」"));
        let en = locale::Active { lang: Lang::En, ..Default::default() };
        let p = prompt_block(Mode::Confirm, false, Reply::Sms, &en, now, "+08:00");
        assert!(p.contains("say \"Let me check and I'll text you back later.\" — in English") && !p.contains("in Chinese"), "{p}");
        assert_eq!(check_back_line(false, "Sam", Reply::Call), "I'll check with Sam and call you back later.");
        // The proposal's answer to the model carries the same line.
        let mut s = session(Mode::Confirm);
        s.reply = Reply::Call;
        assert!(s.pending_answer()["tell_caller"].as_str().unwrap().contains("I'll call you back later."));
    }

    #[test]
    fn the_default_reply_follows_the_number_and_the_setting() {
        let mobile = ReplyOptions { sms: true, call: true };
        let landline = ReplyOptions { sms: false, call: true };
        let hidden = ReplyOptions { sms: false, call: false };
        // Automatic: a text to a mobile, a call to a landline, nothing to a hidden number.
        for auto in ["auto", "", "whatever"] {
            assert_eq!(default_reply(auto, mobile), Reply::Sms);
            assert_eq!(default_reply(auto, landline), Reply::Call);
            assert_eq!(default_reply(auto, hidden), Reply::None);
        }
        // Always a text / always a call: only that, else nobody.
        assert_eq!(default_reply("sms", mobile), Reply::Sms);
        assert_eq!(default_reply("sms", landline), Reply::None);
        assert_eq!(default_reply(" CALL ", mobile), Reply::Call);
        assert_eq!(default_reply("call", landline), Reply::Call);
        assert_eq!(default_reply("call", hidden), Reply::None);
        assert_eq!(default_reply("sms", hidden), Reply::None);
        // What a number allows: texts need the setting and a number that takes
        // them; a call needs a number that is shown.
        let mut rt = plain_settings();
        rt.appointment_sms = true;
        assert_eq!(reply_options(&rt, "0912345678", Some("tw")), mobile);
        assert_eq!(reply_options(&rt, "02-2555-0123", Some("tw")), landline);
        assert_eq!(reply_options(&rt, "0800-000-123", Some("tw")), hidden);
        for h in ["", "anonymous", "private", "Unknown"] {
            assert_eq!(reply_options(&rt, h, Some("tw")), hidden, "{h}");
        }
        rt.appointment_sms = false;
        assert_eq!(reply_options(&rt, "0912345678", Some("tw")), landline);
        // A call's session promises what its default does.
        rt.appointment_sms = true;
        rt.appointment_reply = "auto".into();
        rt.appointment_mode = "confirm".into();
        rt.appointment_test_numbers = Vec::new();
        let http = Client::new();
        let reply_of = |n: &str| ApptSession::for_call(&rt, &http, n).unwrap().reply;
        assert_eq!((reply_of("0912345678"), reply_of("0225550123"), reply_of("")), (Reply::Sms, Reply::Call, Reply::None));
        for w in [Reply::Sms, Reply::Call, Reply::None] {
            assert_eq!(Reply::parse(&w.as_str().to_uppercase()), Some(w));
        }
        assert_eq!(Reply::parse("text"), None);
        assert_eq!((sanitize_reply_setting(" Call "), sanitize_reply_setting("x")), ("call".to_string(), "auto".to_string()));
    }

    #[tokio::test]
    async fn the_owner_picks_how_the_caller_is_told() {
        let (root, store, calls) = stores();
        let mut rt = plain_settings();
        rt.appointment_sms = true;
        rt.appointment_reply = "auto".into();
        let a = appt(&later(2, 15), &later(2, 16), "看房");
        let http = Client::new();
        let control = ControlRegistry::default();
        // A landline by default gets a call-back; no phone connected to place it: said so.
        let t = store.create(TaskRecord { id: String::new(), ..task_for("0225550123", &a) }).await.unwrap();
        let (task, text, _) = decide_inner_with(None, &http, &rt, &store, &calls, Some(&control), &t.id, true, None).await.unwrap();
        let got = task.appointment.unwrap();
        assert_eq!((got.reply.as_str(), got.call.as_str(), got.sms.as_str()), ("call", "no_phone", ""));
        assert!(text.contains("AI 沒有回電：沒有可以撥號的手機連線"), "{text}");
        assert!(task.events.contains(&"callback_no_phone".to_string()), "{:?}", task.events);
        // Nothing left waiting for a call that never went out.
        assert!(callbacks_stage(&t.id).is_none());
        assert!(take_ready_callback("", "0225550123").is_none());
        // A mobile, the owner picks "nobody": nothing is sent.
        let t = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &a) }).await.unwrap();
        let (task, text, _) = decide_inner_with(None, &http, &rt, &store, &calls, Some(&control), &t.id, false, Some(Reply::None)).await.unwrap();
        let got = task.appointment.unwrap();
        assert_eq!((got.reply.as_str(), got.sms.as_str(), got.call.as_str()), ("none", "", ""));
        assert!(text.contains("沒有通知對方"), "{text}");
        // A text asked for a landline cannot go: nobody is told.
        let t = store.create(TaskRecord { id: String::new(), ..task_for("0225550123", &a) }).await.unwrap();
        let (task, _, _) = decide_inner_with(None, &http, &rt, &store, &calls, Some(&control), &t.id, true, Some(Reply::Sms)).await.unwrap();
        assert_eq!(task.appointment.unwrap().reply, "none");
        // A hidden number: never a text or a call.
        let t = store.create(TaskRecord { id: String::new(), ..task_for("", &a) }).await.unwrap();
        let (task, text, _) = decide_inner_with(None, &http, &rt, &store, &calls, Some(&control), &t.id, true, Some(Reply::Call)).await.unwrap();
        assert_eq!(task.appointment.unwrap().reply, "none");
        assert!(text.contains("未顯示號碼，沒辦法通知對方"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_call_back_script_says_the_answer_and_waits_for_its_call() {
        let a = appt("2026-11-14T15:00", "2026-11-14T16:00", "看房");
        let me = locale::Active { owner: "王先生".into(), ..Default::default() };
        let yes = callback_prompt(&me, Lang::ZhTw, &a, true);
        assert!(yes.contains("11/14（週六）15:00 (看房) works") && yes.contains("Confirm the time"), "{yes}");
        assert!(yes.contains("in Traditional Chinese") && yes.contains("you speak as 王先生 yourself"), "{yes}");
        assert!(yes.contains("say goodbye once and stop talking") && yes.contains("AI assistant"), "{yes}");
        let no = callback_prompt(&locale::Active { assistant: true, ..me }, Lang::En, &a, false);
        assert!(no.contains("cannot make it at Sat, Nov 14, 3:00 PM (看房) after all") && no.contains("Apologize"), "{no}");
        assert!(no.contains("王先生's assistant") && no.contains("in English"), "{no}");
        // The AI waits for the callee: the ringing is no one to talk to.
        assert!(yes.contains("say nothing until they have picked up and said something"), "{yes}");
    }

    #[test]
    fn a_call_back_script_goes_only_to_the_call_its_phone_reported() {
        let cb = PendingCallback::new("t-cb-a1", "0912000101", "phone-a1", "SCRIPT-A1".into());
        assert!(callbacks_add(cb));
        // Dialing, nothing reported yet: no session of that phone gets it — an
        // incoming call from that number, a ring prewarm (no report ever comes).
        assert!(take_ready_callback("phone-a1", "0912000101").is_none());
        // outbound_ready from another phone, or for another number: not this call.
        assert!(!callback_ready("t-cb-a1", "phone-b1", "0912000101"));
        assert!(!callback_ready("t-cb-a1", "phone-a1", "0912000199"));
        assert!(!callback_ready("t-cb-other", "phone-a1", "0912000101"));
        assert!(callback_ready("t-cb-a1", "phone-a1", "+886912000101"));
        // Ready: only that phone's session with that number (written any way).
        assert!(take_ready_callback("phone-b1", "0912000101").is_none());
        assert!(take_ready_callback("phone-a1", "0922000101").is_none());
        let got = take_ready_callback("phone-a1", "+886 912 000 101").unwrap();
        assert_eq!((got.task_id.as_str(), got.script.as_str()), ("t-cb-a1", "SCRIPT-A1"));
        // Used once: a later call from that number is an ordinary call.
        assert!(take_ready_callback("phone-a1", "0912000101").is_none());
        assert_eq!(callbacks_stage("t-cb-a1"), Some(CallbackStage::OnCall));
        // Its summary finds it by the phone; a ready report comes only once.
        assert_eq!(callback_task_on("phone-a1").as_deref(), Some("t-cb-a1"));
        assert!(!callback_ready("t-cb-a1", "phone-a1", "0912000101"));
        callbacks_remove("t-cb-a1");
        assert_eq!(callback_task_on("phone-a1"), None);
        // A hello without a number: the one call-back ready on that phone.
        assert!(callbacks_add(PendingCallback::new("t-cb-a2", "0912000102", "phone-a2", "S2".into())));
        assert!(callback_ready("t-cb-a2", "phone-a2", ""));
        assert_eq!(take_ready_callback("phone-a2", "").map(|c| c.task_id).as_deref(), Some("t-cb-a2"));
        callbacks_remove("t-cb-a2");
    }

    #[test]
    fn call_backs_to_one_number_go_one_after_another() {
        assert!(callbacks_add(PendingCallback::new("t-cb-q1", "0912000201", "phone-q", "A".into())));
        // The same number, written another way: it waits.
        assert!(!callbacks_add(PendingCallback::new("t-cb-q2", "+886912000201", "phone-q", "B".into())));
        // Another number goes right away.
        assert!(callbacks_add(PendingCallback::new("t-cb-q3", "0912000202", "phone-q", "C".into())));
        assert_eq!(callbacks_stage("t-cb-q2"), Some(CallbackStage::Queued));
        // Waiting, it cannot be taken for a call.
        assert!(!callback_ready("t-cb-q2", "phone-q", "0912000201"));
        let key = number_key("0912000201");
        assert!(callbacks_next(&key).is_none(), "the first one is still under way");
        callbacks_remove("t-cb-q1");
        assert_eq!(callbacks_next(&key).map(|c| c.task_id).as_deref(), Some("t-cb-q2"));
        assert_eq!(callbacks_stage("t-cb-q2"), Some(CallbackStage::Dialing));
        assert!(callbacks_next(&key).is_none());
        callbacks_remove("t-cb-q2");
        callbacks_remove("t-cb-q3");
    }

    #[test]
    fn an_errand_and_a_call_back_to_one_number_take_turns() {
        assert!(callbacks_add(PendingCallback::new("t-cb-e1", "0912000401", "phone-e", "APPT".into())));
        assert!(!callbacks_add(PendingCallback::errand("e-cb-e2", "+886912000401", "phone-e", Some(2), "ERRAND".into())));
        assert!(errand_call_known("e-cb-e2") && !errand_call_known("e-cb-nope"));
        let key = number_key("0912000401");
        assert!(callbacks_next(&key).is_none(), "the call-back is still under way");
        // The appointment's call-back ends: the errand's turn, on its phone and SIM.
        callbacks_remove("t-cb-e1");
        let next = callbacks_next(&key).unwrap();
        assert_eq!((next.task_id.as_str(), next.kind, next.sub_id), ("e-cb-e2", CallbackKind::Errand, Some(2)));
        // An incoming call from that number before the phone reports the dial: not it.
        assert!(take_ready_callback("phone-e", "0912000401").is_none());
        assert!(callback_ready("e-cb-e2", "phone-e", "0912000401"));
        let call = take_ready_callback("phone-e", "0912000401").unwrap();
        assert_eq!((call.task_id.as_str(), call.script.as_str(), call.errand), ("e-cb-e2", "ERRAND", true));
        assert!(outbound_busy_on("phone-e") && !outbound_busy_on("phone-other"));
        // Its call is out: never added (dialed) twice, nor dropped as if it waited.
        assert!(!callbacks_add(PendingCallback::errand("e-cb-e2", "+886912000401", "phone-e", Some(2), "AGAIN".into())));
        assert_eq!(callbacks_stage("e-cb-e2"), Some(CallbackStage::OnCall));
        assert!(!drop_errand_call("e-cb-e2"));
        callbacks_remove("e-cb-e2");
        assert!(!outbound_busy_on("phone-e") && !errand_call_known("e-cb-e2"));
        // An appointment's call-back is still no errand.
        assert!(callbacks_add(PendingCallback::new("t-cb-e3", "0912000402", "phone-e", "APPT".into())));
        assert!(callback_ready("t-cb-e3", "phone-e", "0912000402"));
        assert!(!take_ready_callback("phone-e", "0912000402").unwrap().errand);
        callbacks_remove("t-cb-e3");
    }

    #[test]
    fn a_call_back_the_phone_never_reports_on_ends_as_failed() {
        assert!(callbacks_add(PendingCallback::new("t-cb-o1", "0912000301", "phone-o", "A".into())));
        assert!(!callbacks_overdue().iter().any(|(id, _)| id == "t-cb-o1"));
        let back = |by: Duration| std::time::Instant::now().checked_sub(by).unwrap();
        callbacks().iter_mut().find(|c| c.task_id == "t-cb-o1").unwrap().since = back(CALLBACK_DIAL_LIMIT);
        assert!(callbacks_overdue().contains(&("t-cb-o1".to_string(), "failed")));
        // Ended without its summary: over, result unknown.
        callbacks_set_stage("t-cb-o1", CallbackStage::Ended);
        callbacks().iter_mut().find(|c| c.task_id == "t-cb-o1").unwrap().since = back(CALLBACK_ENDED_LIMIT);
        assert!(callbacks_overdue().contains(&("t-cb-o1".to_string(), "done")));
        callbacks_remove("t-cb-o1");
        // What the owner reads, and which results get their own notice.
        assert_eq!(callback_result_line(Lang::ZhTw, "no_answer", "x"), "AI 回電了，對方沒接");
        assert_eq!(callback_result_line(Lang::ZhTw, "failed", "x"), "AI 回電撥不出去");
        assert_eq!(callback_result_line(Lang::ZhTw, "answered", "x"), "AI 回電接通，已告訴對方結果");
        assert_eq!(callback_headline(Lang::ZhTw, " 0912000000 "), "AI 回電：0912000000");
        assert!(callback_needs_notice("failed") && !callback_needs_notice("no_answer") && !callback_needs_notice("answered"));
        for l in locale::ALL {
            for code in ["answered", "no_answer", "failed", "done", "queued", "not_callable", "no_phone", "send_failed"] {
                let s = callback_result_line(l, code, "x");
                assert!(!s.is_empty() && !s.contains('_') && !s.contains('{'), "{l:?} {code}: {s}");
            }
        }
    }

    #[test]
    fn a_call_back_record_is_marked_and_tells_whether_the_callee_spoke() {
        assert!(is_ai_callback(&["gemini_unmute_after_greet".into(), AI_CALLBACK_EVENT.into()]));
        assert!(!is_ai_callback(&["gemini_unmute_after_greet".into()]));
        assert!(callee_spoke("AI：\n對方：喂？\nAI：您好"));
        assert!(!callee_spoke("AI：您好，我是王先生\n對方： "));
        assert!(!callee_spoke(""));
    }

    #[test]
    fn only_a_real_number_of_the_line_s_own_country_is_called_back() {
        for n in ["0912345678", "+886912345678", "886912345678", "02-2555-0123", "037-123456", "089-123456"] {
            assert!(callable_on(n, Some("tw")), "{n}");
        }
        // Toll-free, premium, short codes, cut-off digits, hidden, abroad.
        for n in ["0800-000-123", "0809-000-123", "0204-123456", "0209-123456", "165", "110", "1999", "113", "2555-0123", "", "anonymous", "+12025550143", "+8613800138000", "00852-2123-4567"] {
            assert!(!callable_on(n, Some("tw")), "{n}");
        }
        // A US line: US numbers, not toll-free or premium ones.
        assert!(callable_on("+1 330-555-0100", Some("us")));
        assert!(callable_on("3305550100", Some("us")));
        for n in ["+18005550100", "+18885550100", "+19005550100", "+886912345678", "911"] {
            assert!(!callable_on(n, Some("us")), "{n}");
        }
        // What the owner may pick, and so what the AI promises on the call.
        let mut rt = plain_settings();
        rt.appointment_sms = true;
        let none = ReplyOptions { sms: false, call: false };
        assert_eq!(reply_options(&rt, "0800-000-123", Some("tw")), none);
        assert_eq!(reply_options(&rt, "165", Some("tw")), none);
        assert!(!reply_options(&rt, "+12025550143", Some("tw")).call);
        assert_eq!(default_reply("call", reply_options(&rt, "0204-123456", Some("tw"))), Reply::None);
        assert_eq!(default_reply("auto", reply_options(&rt, "0800-000-123", Some("tw"))), Reply::None);
        rt.appointment_mode = "confirm".into();
        rt.appointment_test_numbers = Vec::new();
        rt.appointment_reply = "auto".into();
        let s = ApptSession::for_call(&rt, &Client::new(), "0800000123").unwrap();
        assert_eq!(s.reply, Reply::None);
    }

    #[test]
    fn card_lists_time_reason_and_whether_a_text_can_go() {
        let mut rt = RuntimeSettings::from_base(&crate::config::Settings::from_env());
        rt.appointment_sms = true;
        rt.appointment_reply = "auto".into();
        let mut a = appt("2026-11-14T15:00", "2026-11-14T16:00", "看房");
        a.caller_name = "陳先生".into();
        let task = task_for("0912345678", &a);
        let (title, body) = card_text(&rt, &task, &a, "");
        assert_eq!(title, "📅 預約待確認");
        assert!(body.contains("號碼：0912345678") && body.contains("時間：11/14（週六）15:00–16:00"), "{body}");
        assert!(body.contains("事由：看房") && body.contains("陳先生"), "{body}");
        assert!(body.contains("簡訊：按同意或不同意後會傳簡訊告訴對方"), "{body}");
        // A landline: the AI calls back (always texts: it cannot, so call yourself).
        let landline = task_for("0225550123", &a);
        assert!(card_text(&rt, &landline, &a, "").1.contains("回覆方式：按同意或不同意後，AI 會打電話告訴對方"));
        rt.appointment_reply = "sms".into();
        assert!(card_text(&rt, &landline, &a, "").1.contains("對方號碼收不到簡訊，請自己回電"));
        let hidden = task_for("", &a);
        let body = card_text(&rt, &hidden, &a, "").1;
        assert!(body.contains("號碼：未顯示號碼") && body.contains("回覆方式：未顯示號碼，沒辦法通知對方"), "{body}");
        rt.appointment_sms = false;
        assert!(card_text(&rt, &task, &a, "").1.contains("簡訊：不會傳"));
        rt.appointment_reply = "call".into();
        assert!(card_text(&rt, &task, &a, "").1.contains("AI 會打電話告訴對方"));
    }

    #[test]
    fn sms_reports_say_what_happened() {
        assert_eq!(sms_report(&SmsOutcome::Sent, "0912345678"), ("sent".into(), "已傳簡訊給 0912345678".into()));
        assert_eq!(sms_report(&SmsOutcome::Unsupported, "x").1, "簡訊沒傳出：手機 App 太舊，不支援傳簡訊");
        assert_eq!(sms_report(&SmsOutcome::Offline, "x"), ("offline".into(), "簡訊沒傳出：手機沒連線".into()));
        assert_eq!(sms_report(&SmsOutcome::Timeout, "x").0, "timeout");
        assert_eq!(sms_report(&SmsOutcome::Busy, "x"), ("busy".into(), "簡訊沒傳出：同一則簡訊正在傳送中".into()));
        assert_eq!(
            sms_report(&SmsOutcome::Failed("no_service".into()), "x"),
            ("no_service".into(), "簡訊沒傳出：手機沒有訊號".into())
        );
        // Only a code this agent does not know shows as it came.
        assert_eq!(
            sms_report(&SmsOutcome::Failed("weird_code".into()), "x"),
            ("weird_code".into(), "簡訊沒傳出：失敗代碼 weird_code".into())
        );
    }

    #[test]
    fn text_result_codes_read_as_words_in_every_language() {
        let codes = [
            "offline", "old_app", "timeout", "busy", "no_number", "no_permission", "bad_request", "no_service",
            "radio_off", "generic_failure", "exception", "no_sim", "not_test_number",
        ];
        for l in locale::ALL {
            for code in codes {
                let w = sms_code_text(l, code).unwrap_or_else(|| panic!("{l:?} {code}"));
                assert!(!w.trim().is_empty() && !w.contains('{') && !w.contains('_') && !w.contains(code), "{l:?} {code}: {w}");
            }
            assert_eq!(sms_code_text(l, "weird_code"), None);
            assert_eq!(sms_code_text(l, "sent"), None);
        }
        assert_eq!(sms_code_text(Lang::ZhTw, " generic_failure "), Some("電信端沒有收下這則簡訊"));
        assert_eq!(sms_code_text(Lang::ZhTw, "not_test_number"), Some("這是測試來電，對方號碼不在測試號碼名單上"));
        assert_eq!(
            sms_code_text(Lang::En, "no_sim"),
            Some("the phone has no SIM matching the one in settings (or more than one)")
        );
        for code in crate::callback::SMS_ERROR_CODES {
            assert!(sms_code_text(Lang::En, code).is_some(), "{code}");
        }
    }

    fn stores() -> (std::path::PathBuf, Arc<TaskStore>, Arc<CallStore>) {
        let root = std::env::temp_dir().join(format!("bageo-appt-{}", uuid::Uuid::new_v4()));
        let tasks = TaskStore::new(root.join("tasks"));
        let calls = CallStore::new(root.join("calls"), false, false, 30);
        (root, tasks, calls)
    }

    fn plain_settings() -> RuntimeSettings {
        let mut rt = RuntimeSettings::from_base(&crate::config::Settings::from_env());
        rt.appointment_sms = false;
        rt.appointment_reply = "sms".into();
        rt
    }

    #[tokio::test]
    async fn two_decisions_at_once_act_only_once() {
        let (root, store, calls) = stores();
        let rt = plain_settings();
        let http = Client::new();
        let cal = Arc::new(FakeCal::default());
        cal.write_delay_ms.store(150, std::sync::atomic::Ordering::SeqCst);
        let a = appt(&later(2, 15), &later(2, 16), "看房");
        let t = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &a) }).await.unwrap();
        // ✅ and ❌ pressed together (or Telegram and the API): the first one acts.
        let (first, second) = tokio::join!(
            decide_with(Some(Cal::Fake(cal.clone())), &http, &rt, &store, &calls, None, &t.id, true),
            decide_with(Some(Cal::Fake(cal.clone())), &http, &rt, &store, &calls, None, &t.id, false),
        );
        let (first, second) = (first.unwrap(), second.unwrap());
        assert!(first.1.starts_with("✅ 已同意預約"), "{}", first.1);
        assert!(second.1.starts_with("這個預約已經處理過了"), "{}", second.1);
        assert_eq!(count(&cal.inserts), 1);
        let done = store.get(&t.id).await.unwrap();
        assert_eq!(done.status, TaskStatus::Fulfilled);
        assert_eq!(done.appointment.as_ref().map(|a| (a.status.as_str(), a.event_id.as_str())), Some((CONFIRMED, "ev1")));
        assert_eq!(done.events.iter().filter(|e| e.starts_with("appointment_")).count(), 1, "{:?}", done.events);
        // Asked while another decision holds the task: nothing happens. (Another
        // hour: the first one is taken now.)
        let a2 = appt(&later(3, 15), &later(3, 16), "看房");
        let t2 = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &a2) }).await.unwrap();
        let held = Deciding::claim(&t2.id).unwrap();
        assert!(Deciding::claim(&t2.id).is_none());
        let (_, text) = decide_with(Some(Cal::Fake(cal.clone())), &http, &rt, &store, &calls, None, &t2.id, true).await.unwrap();
        assert!(text.starts_with("這個預約已經處理過了"), "{text}");
        assert_eq!(store.get(&t2.id).await.unwrap().status, TaskStatus::Pending);
        drop(held);
        let (_, text) = decide_with(Some(Cal::Fake(cal.clone())), &http, &rt, &store, &calls, None, &t2.id, true).await.unwrap();
        assert!(text.starts_with("✅ 已同意預約"), "{text}");
        assert_eq!(count(&cal.inserts), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn auto_moves_its_own_event_instead_of_booking_twice() {
        let cal = Arc::new(FakeCal::default());
        let s = auto_session(&cal);
        let booked = json!({"status": "booked"});
        let (a, b) = (later(2, 15), later(2, 16));
        assert_eq!(s.run_tool(&propose_call(&a, &b)).await, booked);
        assert_eq!(count(&cal.inserts), 1);
        // Its own event does not make that time look busy.
        let check = ToolCall { id: "c".into(), name: CHECK.into(), args: json!({"start": a}), seq: 0, after: 0 };
        assert_eq!(s.run_tool(&check).await, json!({"free": true}));
        // Longer now (overlaps only its own event): moved, not added.
        assert_eq!(s.run_tool(&propose_call(&a, &later(2, 17))).await, booked);
        assert_eq!((count(&cal.inserts), count(&cal.moves)), (1, 1));
        let out = s.book.outcome().unwrap();
        assert_eq!((out.event_id.as_str(), out.end.as_str()), ("ev1", later(2, 17).as_str()));
        assert!(out.replaced.is_empty());
        // The same time again: booked, nothing written.
        assert_eq!(s.run_tool(&propose_call(&a, &later(2, 17))).await, booked);
        assert_eq!((count(&cal.inserts), count(&cal.moves)), (1, 1));
        // Someone else has that time: not booked; the call's event keeps its id…
        cal.busy.lock().unwrap().push((later(3, 9), later(3, 12)));
        assert_eq!(
            s.run_tool(&propose_call(&later(3, 10), &later(3, 11))).await,
            json!({"status": "pending_owner_confirmation", "not_booked": true})
        );
        let out = s.book.outcome().unwrap();
        assert_eq!((out.status.as_str(), out.event_id.as_str()), (PENDING, "ev1"));
        assert_eq!(out.auto_error, texts().ap_busy);
        assert!(out.replaced.is_empty());
        // …so the owner's ✅ moves it there instead of adding a second one — once
        // the other plans are gone: while they stand, ✅ is held (nothing written).
        let (root, store, calls) = stores();
        let t = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &out) }).await.unwrap();
        let (held, text) = decide_with(Some(Cal::Fake(cal.clone())), &Client::new(), &plain_settings(), &store, &calls, None, &t.id, true)
            .await
            .unwrap();
        assert!(text.starts_with("⚠️ 這個時段日曆上已經有行程"), "{text}");
        assert_eq!(held.appointment.as_ref().map(|a| a.status.as_str()), Some(PENDING));
        assert_eq!((count(&cal.inserts), count(&cal.moves)), (1, 1));
        cal.busy.lock().unwrap().clear();
        let (done, _) = decide_with(Some(Cal::Fake(cal.clone())), &Client::new(), &plain_settings(), &store, &calls, None, &t.id, true)
            .await
            .unwrap();
        assert_eq!((count(&cal.inserts), count(&cal.moves)), (1, 2));
        let done = done.appointment.unwrap();
        assert_eq!((done.event_id.as_str(), done.status.as_str()), ("ev1", CONFIRMED));
        assert!(done.write_error.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn lost_answers_and_declines_do_not_leave_extra_events() {
        let (root, store, calls) = stores();
        let http = Client::new();
        let cal = Arc::new(FakeCal::default());
        // An insert whose answer was lost (timeout) is found by the call's tag.
        cal.events.lock().unwrap().push(("ev7".into(), later(2, 9), later(2, 10), "tag-test".into()));
        let mut a = appt(&later(2, 9), &later(2, 10), "看房");
        a.mode = "auto".into();
        a.tag = "tag-test".into();
        let t = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &a) }).await.unwrap();
        let (done, _) = decide_with(Some(Cal::Fake(cal.clone())), &http, &plain_settings(), &store, &calls, None, &t.id, true)
            .await
            .unwrap();
        assert_eq!((count(&cal.inserts), count(&cal.moves)), (0, 1));
        assert_eq!(done.appointment.unwrap().event_id, "ev7");
        // Declining an appointment whose event is in the calendar takes it out.
        let mut b = a.clone();
        b.event_id = "ev7".into();
        let t = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &b) }).await.unwrap();
        let (done, text) = decide_with(Some(Cal::Fake(cal.clone())), &http, &plain_settings(), &store, &calls, None, &t.id, false)
            .await
            .unwrap();
        assert!(text.contains("已從行事曆刪除"), "{text}");
        assert_eq!(count(&cal.deletes), 1);
        assert!(done.appointment.unwrap().event_id.is_empty());
        // Declining one whose write answer was lost (no id kept): found by its tag
        // and taken out as well.
        cal.events.lock().unwrap().push(("ev8".into(), later(3, 9), later(3, 10), "tag-lost".into()));
        let mut c = appt(&later(3, 9), &later(3, 10), "看房");
        c.mode = "auto".into();
        c.tag = "tag-lost".into();
        let t = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &c) }).await.unwrap();
        let (_, text) = decide_with(Some(Cal::Fake(cal.clone())), &http, &plain_settings(), &store, &calls, None, &t.id, false)
            .await
            .unwrap();
        assert!(text.contains("已從行事曆刪除"), "{text}");
        assert_eq!(count(&cal.deletes), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_scam_verdict_voids_what_the_call_booked_and_it_is_deleted() {
        let http = Client::new();
        let cal = Arc::new(FakeCal::default());
        let s = auto_session(&cal);
        s.run_tool(&propose_call(&later(2, 15), &later(2, 16))).await;
        s.book.discard();
        assert_eq!(s.book.outcome(), None);
        let voided = s.book.voided();
        assert_eq!(voided.iter().map(|e| e.event_id.as_str()).collect::<Vec<_>>(), vec!["ev1"]);
        assert!(delete_voided_with(Some(Cal::Fake(cal.clone())), &http, &voided).await.is_empty());
        assert_eq!(count(&cal.deletes), 1);
        // The verdict lands while the event is being written: it is voided as well,
        // and the model is only told it waits.
        let cal2 = Arc::new(FakeCal::default());
        cal2.write_delay_ms.store(200, std::sync::atomic::Ordering::SeqCst);
        let s2 = auto_session(&cal2);
        let runner = s2.clone();
        let call = propose_call(&later(4, 9), &later(4, 10));
        let running = tokio::spawn(async move { runner.run_tool(&call).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        s2.book.discard();
        assert_eq!(running.await.unwrap()["status"], "pending_owner_confirmation");
        let voided = s2.book.voided();
        assert_eq!(voided.iter().map(|e| e.event_id.as_str()).collect::<Vec<_>>(), vec!["ev1"]);
        s2.book.settled(Duration::from_millis(10)).await;
        // Could not be deleted: a line for the summary says so.
        cal2.fail_delete.store(true, std::sync::atomic::Ordering::SeqCst);
        let failed = delete_voided_with(Some(Cal::Fake(cal2.clone())), &http, &voided).await;
        assert_eq!(failed.len(), 1);
        let line = void_failed_line(&failed[0]);
        assert!(line.starts_with("📅 通話中排入的「") && line.contains("請自己刪"), "{line}");
        // An event whose id was never answered is found by the call's tag.
        cal2.fail_delete.store(false, std::sync::atomic::Ordering::SeqCst);
        let lost = Earlier { start: later(4, 9), tag: "tag-test".into(), ..Default::default() };
        assert!(delete_voided_with(Some(Cal::Fake(cal2.clone())), &http, &[lost]).await.is_empty());
        assert_eq!(count(&cal2.deletes), 1);
        // A booked appointment of a call that turned out bad: everything it wrote.
        let mut booked = appt(&later(2, 15), &later(2, 16), "看房");
        booked.event_id = "ev9".into();
        booked.replaced = vec![Earlier { event_id: "ev8".into(), ..Default::default() }];
        let ids: Vec<String> = all_events(&booked).into_iter().map(|e| e.event_id).collect();
        assert_eq!(ids, vec!["ev8", "ev9"]);
        assert!(all_events(&appt(&later(2, 15), &later(2, 16), "看房")).is_empty());
    }

    #[tokio::test]
    async fn owner_decision_without_calendar_or_texts_updates_the_task() {
        let root = std::env::temp_dir().join(format!("bageo-appt-{}", uuid::Uuid::new_v4()));
        let store = TaskStore::new(root.join("tasks"));
        let calls = CallStore::new(root.join("calls"), false, false, 30);
        let mut rt = RuntimeSettings::from_base(&crate::config::Settings::from_env());
        rt.appointment_sms = false;
        rt.calendar_app = String::new();
        rt.appointment_reply = "sms".into();
        rt.public_url = "http://bageo.local/".into();
        let a = appt("2026-11-14T15:00", "2026-11-14T16:00", "看房");
        let t = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &a) }).await.unwrap();
        let (done, text) = decide(&Client::new(), &rt, &store, &calls, None, &t.id, true).await.unwrap();
        assert_eq!(done.status, TaskStatus::Fulfilled);
        assert_eq!(done.appointment.as_ref().unwrap().status, CONFIRMED);
        assert!(text.starts_with("✅ 已同意預約\n11/14（週六）15:00–16:00 看房"), "{text}");
        // No calendar of its own: one tap adds it to the owner's (Google
        // Calendar's form until another calendar is chosen), with the caller in it.
        assert!(
            text.contains("\n📅 加入行事曆：https://calendar.google.com/calendar/render?action=TEMPLATE&text=%E7%9C%8B%E6%88%BF&dates=20261114T150000/20261114T160000&details="),
            "{text}"
        );
        // The caller's number goes in the entry (`號碼：0912345678`).
        assert!(text.contains("%E8%99%9F%E7%A2%BC%EF%BC%9A0912345678"), "{text}");
        assert!(done.events.contains(&"appointment_confirmed".to_string()));
        // Pressed again: nothing happens twice.
        let (_, again) = decide(&Client::new(), &rt, &store, &calls, None, &t.id, false).await.unwrap();
        assert!(again.starts_with("這個預約已經處理過了"), "{again}");
        // Apple Calendar chosen: the task's .ics at the console's address.
        rt.calendar_app = "apple".into();
        let t3 = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &a) }).await.unwrap();
        let (_, text) = decide(&Client::new(), &rt, &store, &calls, None, &t3.id, true).await.unwrap();
        assert!(text.ends_with(&format!("📅 加入行事曆：http://bageo.local/calls/api/tasks/{}/event.ics", t3.id)), "{text}");
        // …and without an address other devices can open: no link at all.
        rt.public_url = String::new();
        let t4 = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &a) }).await.unwrap();
        let (_, text) = decide(&Client::new(), &rt, &store, &calls, None, &t4.id, true).await.unwrap();
        assert!(!text.contains("加入行事曆"), "{text}");
        // Declined, texts on, the phone is not connected: said so.
        rt.appointment_sms = true;
        let t2 = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &a) }).await.unwrap();
        let control = ControlRegistry::default();
        let (done, text) = decide(&Client::new(), &rt, &store, &calls, Some(&control), &t2.id, false).await.unwrap();
        assert_eq!(done.status, TaskStatus::Ignored);
        assert_eq!(done.appointment.as_ref().unwrap().sms, "offline");
        assert!(text.starts_with("❌ 已婉拒預約") && text.contains("簡訊沒傳出：手機沒連線"), "{text}");
        // Declined: nothing to add to a calendar.
        assert!(!text.contains("加入行事曆"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    fn propose_args(id: &str, args: Value, seq: u64) -> ToolCall {
        ToolCall { id: id.into(), name: PROPOSE.into(), args, seq, after: 0 }
    }

    const SEQ: std::sync::atomic::Ordering = std::sync::atomic::Ordering::SeqCst;

    #[tokio::test]
    async fn a_failed_calendar_write_keeps_it_waiting_and_texts_nobody() {
        let (root, store, calls) = stores();
        let http = Client::new();
        let cal = Arc::new(FakeCal::default());
        cal.fail_insert.store(true, SEQ);
        let mut rt = plain_settings();
        rt.appointment_sms = true;
        let control = ControlRegistry::default();
        let mut a = appt(&later(2, 15), &later(2, 16), "看房");
        a.sim_country = Some("tw".into());
        let t = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &a) }).await.unwrap();
        let (task, text, done) = decide_inner(Some(Cal::Fake(cal.clone())), &http, &rt, &store, &calls, Some(&control), &t.id, true)
            .await
            .unwrap();
        assert_eq!(done, Decision::Held);
        assert!(text.starts_with("⚠️ 寫入日曆失敗") && text.contains("可再按一次") && text.contains("HTTP 503"), "{text}");
        let held = task.appointment.clone().unwrap();
        // Still waiting, nothing texted (no "sms_…" either), the reason kept.
        assert_eq!((held.status.as_str(), held.sms.as_str()), (PENDING, ""));
        assert!(held.write_error.contains("HTTP 503"), "{}", held.write_error);
        assert_eq!(task.status, TaskStatus::Pending);
        assert!(task.fulfill_error.as_deref().is_some_and(|e| e.contains("HTTP 503")));
        assert!(task.events.contains(&"appointment_write_failed".to_string()), "{:?}", task.events);
        assert!(!task.events.iter().any(|e| e == "appointment_confirmed" || e.starts_with("sms_")), "{:?}", task.events);
        // The card (sent again after a press) says so.
        let (_, body) = card_text(&rt, &task, &held, "");
        assert!(body.contains("寫入日曆失敗"), "{body}");
        // Pressed again once the calendar answers: confirmed, and only now the text.
        cal.fail_insert.store(false, SEQ);
        let (task, text, done) = decide_inner(Some(Cal::Fake(cal.clone())), &http, &rt, &store, &calls, Some(&control), &t.id, true)
            .await
            .unwrap();
        assert_eq!(done, Decision::Settled);
        assert!(text.starts_with("✅ 已同意預約"), "{text}");
        let ok = task.appointment.clone().unwrap();
        assert_eq!((ok.status.as_str(), ok.event_id.as_str(), ok.sms.as_str()), (CONFIRMED, "ev1", "offline"));
        assert!(ok.write_error.is_empty() && task.fulfill_error.is_none());
        assert_eq!(task.status, TaskStatus::Fulfilled);
        // Decided now: another press changes nothing.
        let (_, _, done) = decide_inner(Some(Cal::Fake(cal.clone())), &http, &rt, &store, &calls, Some(&control), &t.id, true)
            .await
            .unwrap();
        assert_eq!(done, Decision::Already);
        assert_eq!(count(&cal.inserts), 1);
        // Held phrases exist in every language.
        for l in locale::ALL {
            let (failed, taken) = held_phrases(l);
            assert_eq!(failed.matches("{}").count(), 1, "{l:?}");
            assert!(!taken.contains('{') && taken.starts_with("⚠️"), "{l:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_quick_change_of_time_books_once_and_the_later_call_wins() {
        let booked = json!({"status": "booked"});
        let eight = later(2, 8);
        let half_nine = later(2, 9).replace(":00", ":30");
        // "Eight doesn't work, make it half past nine": two calls almost at once;
        // the first is still writing (slow calendar) when the second runs.
        let cal = Arc::new(FakeCal::default());
        cal.write_delay_ms.store(150, SEQ);
        let s = auto_session(&cal);
        let first = propose_args("a", json!({"start": eight, "title": "看房"}), next_seq());
        let second = propose_args("b", json!({"start": half_nine, "title": "看房"}), next_seq());
        let (r1, r2) = tokio::join!(s.run_tool(&first), async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            s.run_tool(&second).await
        });
        assert_eq!((r1, r2), (booked.clone(), booked.clone()));
        // One event, moved to the later time — not two.
        assert_eq!((count(&cal.inserts), count(&cal.moves)), (1, 1));
        let events = cal.events.lock().unwrap().clone();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].1, half_nine);
        let all = s.book.outcomes();
        assert_eq!(all.len(), 1);
        assert_eq!((all[0].start.as_str(), all[0].event_id.as_str()), (half_nine.as_str(), "ev1"));

        // The later call's task ran first: the earlier one finds it already
        // changed and leaves it alone.
        let cal2 = Arc::new(FakeCal::default());
        cal2.write_delay_ms.store(50, SEQ);
        let s2 = auto_session(&cal2);
        let (older, newer) = (next_seq(), next_seq());
        assert_eq!(s2.run_tool(&propose_args("b", json!({"start": half_nine, "title": "看房"}), newer)).await, booked);
        assert_eq!(
            s2.run_tool(&propose_args("a", json!({"start": eight, "title": "看房"}), older)).await,
            json!({"status": "superseded"})
        );
        assert_eq!((count(&cal2.inserts), count(&cal2.moves)), (1, 0));
        assert_eq!(s2.book.outcome().unwrap().start, half_nine);
        assert!(answer_is_silent(PROPOSE, &json!({"status": "superseded"})));
    }

    #[tokio::test]
    async fn two_callers_at_once_cannot_both_take_the_same_hour() {
        let cal = Arc::new(FakeCal::default());
        cal.write_delay_ms.store(150, SEQ);
        let a = ApptSession { tag: "tag-a".into(), ..auto_session(&cal) };
        let b = ApptSession { tag: "tag-b".into(), ..auto_session(&cal) };
        let (from, to) = (later(3, 10), later(3, 11));
        let (ca, cb) = (propose_call(&from, &to), propose_call(&from, &to));
        let (ra, rb) = tokio::join!(a.run_tool(&ca), b.run_tool(&cb));
        let booked = json!({"status": "booked"});
        let waits = json!({"status": "pending_owner_confirmation", "not_booked": true});
        assert!((ra == booked && rb == waits) || (ra == waits && rb == booked), "{ra} {rb}");
        assert_eq!(count(&cal.inserts), 1);
        let loser = if ra == booked { b.book.outcome().unwrap() } else { a.book.outcome().unwrap() };
        assert_eq!((loser.status.as_str(), loser.auto_error.as_str()), (PENDING, texts().ap_busy));

        // The owner confirms the one that waits: the hour is taken, so it stays
        // waiting and nobody is texted "fine".
        let (root, store, calls) = stores();
        let http = Client::new();
        let mut rt = plain_settings();
        rt.appointment_sms = true;
        let control = ControlRegistry::default();
        let mut l = loser.clone();
        l.sim_country = Some("tw".into());
        let t = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &l) }).await.unwrap();
        let (task, text, done) = decide_inner(Some(Cal::Fake(cal.clone())), &http, &rt, &store, &calls, Some(&control), &t.id, true)
            .await
            .unwrap();
        assert_eq!(done, Decision::Held);
        assert!(text.contains("已經有行程") && text.contains("再按一次同意就照樣排入"), "{text}");
        let held = task.appointment.unwrap();
        assert_eq!((held.status.as_str(), held.sms.as_str()), (PENDING, ""));
        assert!(task.events.contains(&"appointment_time_taken".to_string()));
        assert_eq!(count(&cal.inserts), 1);

        // Two appointments for one free hour confirmed at the same moment: one
        // is written, the other is held.
        let (x, y) = (appt(&later(4, 10), &later(4, 11), "看房"), appt(&later(4, 10), &later(4, 11), "修水管"));
        let tx = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &x) }).await.unwrap();
        let ty = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &y) }).await.unwrap();
        let rt = plain_settings();
        let (dx, dy) = tokio::join!(
            decide_inner(Some(Cal::Fake(cal.clone())), &http, &rt, &store, &calls, None, &tx.id, true),
            decide_inner(Some(Cal::Fake(cal.clone())), &http, &rt, &store, &calls, None, &ty.id, true),
        );
        let mut outcomes = vec![dx.unwrap().2, dy.unwrap().2];
        outcomes.sort_by_key(|d| *d == Decision::Held);
        assert_eq!(outcomes, vec![Decision::Settled, Decision::Held]);
        assert_eq!(count(&cal.inserts), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_second_appointment_on_the_same_call_is_kept_apart() {
        let cal = Arc::new(FakeCal::default());
        let s = auto_session(&cal);
        let booked = json!({"status": "booked"});
        // A viewing first, then a separate call on another day: two events.
        assert_eq!(s.run_tool(&propose_call(&later(2, 10), &later(2, 11))).await, booked);
        let other = |start: String, replaces: Option<bool>| {
            let mut args = json!({"start": start, "title": "電話會議"});
            if let Some(r) = replaces {
                args["replaces_previous"] = json!(r);
            }
            propose_args("o", args, 0)
        };
        assert_eq!(s.run_tool(&other(later(4, 15), Some(false))).await, booked);
        assert_eq!((count(&cal.inserts), count(&cal.moves)), (2, 0));
        // A change of time (replaces_previous left out = true) moves the one agreed last.
        assert_eq!(s.run_tool(&other(later(4, 16), None)).await, booked);
        assert_eq!((count(&cal.inserts), count(&cal.moves)), (2, 1));
        // Proposed again as "another" at the very same time: the same one, nothing written.
        assert_eq!(s.run_tool(&other(later(4, 16), Some(false))).await, booked);
        assert_eq!((count(&cal.inserts), count(&cal.moves)), (2, 1));
        let all = s.book.outcomes();
        assert_eq!(all.len(), 2);
        assert_eq!((all[0].start.as_str(), all[0].event_id.as_str()), (later(2, 10).as_str(), "ev1"));
        assert_eq!((all[1].start.as_str(), all[1].event_id.as_str()), (later(4, 16).as_str(), "ev2"));
        assert_eq!((all[0].tag.as_str(), all[1].tag.as_str()), ("tag-test", "tag-test-2"));
        // The record keeps the first, the other in `more`; the summary names both;
        // voiding the call takes out every event.
        let rec = s.book.outcome().unwrap();
        assert_eq!(rec.more.len(), 1);
        assert_eq!(summary_line(&rec, "").lines().count(), 2);
        let ids: Vec<String> = all_events(&rec).into_iter().map(|e| e.event_id).collect();
        assert_eq!(ids, vec!["ev1", "ev2"]);
        s.book.discard();
        let mut voided: Vec<String> = s.book.voided().into_iter().map(|e| e.event_id).collect();
        voided.sort();
        assert_eq!(voided, vec!["ev1", "ev2"]);
    }

    #[tokio::test]
    async fn each_appointment_of_a_call_has_its_own_task_and_decision() {
        let s = session(Mode::Confirm);
        let pending = json!("pending_owner_confirmation");
        assert_eq!(s.run_tool(&propose_call(&later(2, 10), &later(2, 11))).await["status"], pending);
        let second = propose_args("o", json!({"start": later(5, 14), "title": "修水管", "replaces_previous": false}), 0);
        assert_eq!(s.run_tool(&second).await["status"], pending);
        // The call's record and notes carry both.
        let root = std::env::temp_dir().join(format!("bageo-appt-{}", uuid::Uuid::new_v4()));
        let tasks = TaskStore::new(root.join("tasks"));
        let calls = CallStore::new(root.join("calls"), true, false, 7);
        let mut call = calls.start("pixel-1", "0912345678", 16000, "active").await.unwrap();
        call.set_appointments(s.book.clone());
        let rec = calls.finalize(call, "ended").await;
        let agreed = rec.appointment.clone().unwrap();
        assert_eq!((agreed.title.as_str(), agreed.more.len()), ("看房", 1));
        let notes = crate::calls::CallNotes { appointment: Some(agreed.clone()), ..Default::default() };
        calls.save_notes(&rec.id, &notes).await.unwrap();
        // One task each.
        let made = create_tasks(&tasks, &rec, &agreed).await;
        assert_eq!(made.len(), 2);
        let titles: Vec<String> = made.iter().map(|t| t.appointment.as_ref().unwrap().title.clone()).collect();
        assert_eq!(titles, vec!["看房", "修水管"]);
        assert!(made.iter().all(|t| t.appointment.as_ref().unwrap().more.is_empty()));
        // The second is confirmed on its own; the first still waits, also in the call's notes.
        let cal = Arc::new(FakeCal::default());
        let (_, text) = decide_with(Some(Cal::Fake(cal.clone())), &Client::new(), &plain_settings(), &tasks, &calls, None, &made[1].id, true)
            .await
            .unwrap();
        assert!(text.starts_with("✅ 已同意預約"), "{text}");
        assert_eq!(tasks.get(&made[0].id).await.unwrap().appointment.unwrap().status, PENDING);
        let n = calls.load_notes(&rec.id).await.unwrap().appointment.unwrap();
        assert_eq!((n.status.as_str(), n.more.len(), n.more[0].status.as_str()), (PENDING, 1, CONFIRMED));
        assert_eq!(n.more[0].event_id, "ev1");
        // Notes from before a call could take two: replaced as a whole.
        let mut only = made[0].appointment.clone().unwrap();
        only.status = DECLINED.into();
        assert_eq!(merged_into(Some(appt(&later(2, 10), &later(2, 11), "看房")), &only).status, DECLINED);
        assert_eq!(merged_into(None, &only), only);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_time_on_the_wrong_side_of_the_clock_is_sent_back() {
        let e = period_mismatch(&json!({"start": "2026-11-20T08:00", "period": "evening"})).unwrap();
        assert_eq!(e["error"], "period_mismatch");
        let hint = e["hint"].as_str().unwrap();
        assert!(hint.contains("20:00") && hint.contains("evening") && hint.contains("say nothing"), "{hint}");
        let hint_of = |v: Value| period_mismatch(&v).unwrap()["hint"].as_str().unwrap().to_string();
        assert!(hint_of(json!({"start": "2026-11-20T03:00", "period": "afternoon"})).contains("15:00"));
        assert!(hint_of(json!({"start": "2026-11-20T20:30", "period": "Morning"})).contains("08:30"));
        assert!(hint_of(json!({"start": "2026-11-20T09:00", "period": "night"})).contains("21:00"));
        for ok in [
            json!({"start": "2026-11-20T20:00", "period": "evening"}),
            json!({"start": "2026-11-20T23:30", "period": "night"}),
            json!({"start": "2026-11-20T01:00", "period": "night"}),
            json!({"start": "2026-11-20T09:00", "period": "morning"}),
            json!({"start": "2026-11-20T12:00", "period": "afternoon"}),
            json!({"start": "2026-11-20T18:00", "period": "afternoon"}),
            json!({"start": "2026-11-20T08:00"}),
            json!({"start": "2026-11-20T08:00", "period": "sometime"}),
            json!({"start": "soon", "period": "evening"}),
        ] {
            assert_eq!(period_mismatch(&ok), None, "{ok}");
        }
        // Both functions send it back before anything is checked or written,
        // and the model is made to speak (call again), never silently.
        let cal = Arc::new(FakeCal::default());
        let s = auto_session(&cal);
        let check = ToolCall { id: "c".into(), name: CHECK.into(), args: json!({"start": later(2, 8), "period": "evening"}), seq: 0, after: 0 };
        assert_eq!(s.run_tool(&check).await["error"], "period_mismatch");
        let r = s.run_tool(&propose_args("p", json!({"start": later(2, 8), "period": "evening", "title": "晚餐"}), 0)).await;
        assert_eq!(r["error"], "period_mismatch");
        assert_eq!(tool_response("p", PROPOSE, r)["toolResponse"]["functionResponses"][0]["response"]["scheduling"], "WHEN_IDLE");
        assert_eq!((count(&cal.inserts), s.book.outcome()), (0, None));
        let ok = s.run_tool(&propose_args("p", json!({"start": later(2, 20), "period": "evening", "title": "晚餐"}), 0)).await;
        assert_eq!(ok, json!({"status": "booked"}));
        // Offered on both functions; "replaces_previous" on propose_appointment.
        let decl = tool_declarations(true);
        for f in decl[0]["functionDeclarations"].as_array().unwrap() {
            assert!(f["parameters"]["properties"].get("period").is_some(), "{f}");
        }
        assert_eq!(decl[0]["functionDeclarations"][1]["parameters"]["properties"]["replaces_previous"]["type"], "BOOLEAN");
    }

    #[test]
    fn the_line_that_took_the_call_goes_with_its_appointments() {
        let http = Client::new();
        let mut rt = plain_settings();
        rt.appointment_sms = true;
        rt.appointment_mode = "confirm".into();
        rt.appointment_test_numbers = Vec::new();
        let mut s = ApptSession::for_call(&rt, &http, "2025550143").unwrap();
        // Read as Taiwanese until the SIM is known: no text back.
        assert!(!s.reply_by_text);
        s.set_line(Some(" US ".into()), Some(2));
        assert!(s.reply_by_text);
        // Every copy shares it, also for what was agreed before.
        let copy = s.clone();
        assert!(copy.book.record(appt("2026-11-14T15:00", "2026-11-14T16:00", "看房")));
        let a = s.book.outcome().unwrap();
        assert_eq!((a.sim_country.as_deref(), a.sim_sub_id), (Some("us"), Some(2)));
        // Kept on the record and the task in JSON; left out when unknown.
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!((v["sim_country"].clone(), v["sim_sub_id"].clone()), (json!("us"), json!(2)));
        let plain = serde_json::to_value(appt("2026-11-14T15:00", "", "x")).unwrap();
        assert!(plain.get("sim_country").is_none() && plain.get("sim_sub_id").is_none() && plain.get("more").is_none());
        // Not a country code: not kept.
        s.set_line(Some("usa".into()), None);
        assert_eq!(s.book.sim_country(), None);
        assert!(!s.reply_by_text);
    }

    #[tokio::test]
    async fn a_text_that_did_not_go_out_can_be_sent_again() {
        let (root, store, calls) = stores();
        let rt = plain_settings();
        let control = ControlRegistry::default();
        let mut a = appt(&later(2, 15), &later(2, 16), "看房");
        a.sim_country = Some("tw".into());
        // Still waiting, or the text went: nothing to send again.
        let waiting = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &a) }).await.unwrap();
        let r = retry_sms(&rt, &store, &calls, Some(&control), &waiting.id, None).await.unwrap();
        assert!(matches!(&r, RetrySms::NotAllowed { sms } if sms.is_empty()), "{r:?}");
        a.status = CONFIRMED.into();
        a.sms = "sent".into();
        let sent = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &a) }).await.unwrap();
        let r = retry_sms(&rt, &store, &calls, Some(&control), &sent.id, None).await.unwrap();
        assert!(matches!(&r, RetrySms::NotAllowed { sms } if sms == "sent"), "{r:?}");
        // Confirmed but the phone was not connected: tried again, here from another phone.
        a.sms = "offline".into();
        let failed = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &a) }).await.unwrap();
        match retry_sms(&rt, &store, &calls, Some(&control), &failed.id, Some("pixel-2")).await.unwrap() {
            RetrySms::Tried { sms, line, task } => {
                assert_eq!(sms, "offline");
                assert!(line.contains("手機沒連線"), "{line}");
                assert_eq!(task.appointment.unwrap().sms, "offline");
                assert!(task.events.contains(&"sms_offline".to_string()), "{:?}", task.events);
            }
            other => panic!("{other:?}"),
        }
        // Declined, and the number takes no texts: said so.
        a.status = DECLINED.into();
        a.sms = "timeout".into();
        let landline = store.create(TaskRecord { id: String::new(), ..task_for("0225550123", &a) }).await.unwrap();
        match retry_sms(&rt, &store, &calls, Some(&control), &landline.id, None).await.unwrap() {
            RetrySms::Tried { sms, .. } => assert_eq!(sms, "no_number"),
            other => panic!("{other:?}"),
        }
        let mut b = a.clone();
        for (status, sms, ok) in [(BOOKED, "busy", true), (PENDING, "offline", false), (DECLINED, "", false), (CONFIRMED, "no_service", true), (CONFIRMED, "no_number", false)] {
            b.status = status.into();
            b.sms = sms.into();
            assert_eq!(sms_retryable(&b), ok, "{status} {sms}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Run `send` while `phone` answers its `send_sms` with "sent": the code
    /// and the message the phone got.
    async fn text_and_answer<F: std::future::Future<Output = (String, String)>>(
        control: &ControlRegistry,
        phone: &mut crate::callback::test_socket::PhoneEnd,
        send: F,
    ) -> (String, Value) {
        let (r, msg) = tokio::join!(send, async {
            let m = crate::callback::test_socket::next_json(phone, std::time::Duration::from_secs(5))
                .await
                .expect("send_sms");
            control
                .handle_sms_result(&json!({"type": "send_sms_result", "id": m["id"].clone(), "ok": true}))
                .await;
            m
        });
        (r.0, msg)
    }

    #[test]
    fn the_test_text_phone_is_found_by_id_or_name() {
        let labels: std::collections::BTreeMap<String, String> = [
            ("pixel-12345".to_string(), "Pixel 9".to_string()),
            ("pixel-23456".to_string(), "Test phone".to_string()),
            ("pixel-34567".to_string(), "test PHONE".to_string()),
        ]
        .into_iter()
        .collect();
        let connected = vec!["pixel-12345".to_string(), "pixel-34567".to_string()];
        assert_eq!(named_phone(" PIXEL-12345 ", &labels, &connected), "pixel-12345");
        assert_eq!(named_phone("pixel 9", &labels, &connected), "pixel-12345");
        // Two phones with that name: the connected one.
        assert_eq!(named_phone("Test Phone", &labels, &connected), "pixel-34567");
        assert_eq!(named_phone("Test Phone", &labels, &[]), "pixel-23456");
        // Not connected and no such name: the id as written.
        assert_eq!(named_phone("pixel-99999", &labels, &connected), "pixel-99999");
    }

    #[tokio::test]
    async fn test_calls_text_from_the_phone_and_sim_the_settings_name() {
        use crate::callback::test_socket::{next_json, phone_socket};
        let quiet = std::time::Duration::from_millis(150);
        let control = ControlRegistry::default();
        // pixel-1 takes the calls; pixel-12345 ("Pixel 9") is the test phone.
        let (took_sink, mut took) = phone_socket().await;
        let (test_sink, mut tester) = phone_socket().await;
        control.register_client("pixel-1".into(), took_sink).await;
        control.register_client("pixel-12345".into(), test_sink).await;
        for s in ["pixel-1", "pixel-12345"] {
            control.set_supports_sms(s, true).await;
        }
        control.set_supports_sim_select("pixel-12345", true).await;
        let mut rt = plain_settings();
        rt.appointment_test_numbers = vec!["16505550123".into()];
        rt.appointment_test_sms_phone = "pixel 9".into();
        rt.appointment_test_sms_sim = "Tello".into();
        rt.device_labels.insert("pixel-12345".into(), "Pixel 9".into());
        let mut a = appt(&later(2, 15), &later(2, 16), "看房");
        a.status = BOOKED.into();
        a.calendar = DEMO_CALENDAR.into();
        a.sim_country = Some("us".into());
        a.sim_sub_id = Some(2);
        let task = |id: &str, caller: &str, a: &Appointment| TaskRecord { id: id.into(), ..task_for(caller, a) };

        // A test call: the phone named by its label, the SIM by name, no sub_id.
        let t1 = task("t1", "+1 650 555 0123", &a);
        let (code, msg) = text_and_answer(&control, &mut tester, text_caller(&rt, Some(&control), &t1, &a, true, None)).await;
        assert_eq!(code, "sent");
        assert_eq!((msg["to"].clone(), msg["sim"].clone(), msg.get("sub_id")), (json!("+16505550123"), json!("Tello"), None));
        assert_eq!(next_json(&mut took, quiet).await, None);

        // Any other call: the phone and SIM that took it, settings or not.
        let mut plain = a.clone();
        plain.calendar = String::new();
        let t2 = task("t2", "+16505550142", &plain);
        let (code, msg) = text_and_answer(&control, &mut took, text_caller(&rt, Some(&control), &t2, &plain, true, None)).await;
        assert_eq!(code, "sent");
        assert_eq!((msg["to"].clone(), msg["sub_id"].clone(), msg.get("sim")), (json!("+16505550142"), json!(2), None));
        assert_eq!(next_json(&mut tester, quiet).await, None);

        // A phone the owner picks wins, from its own SIM choice.
        let t3 = task("t3", "+16505550123", &a);
        let (code, msg) = text_and_answer(&control, &mut took, text_caller(&rt, Some(&control), &t3, &a, true, Some("pixel-1"))).await;
        assert_eq!(code, "sent");
        assert_eq!((msg["sub_id"].clone(), msg.get("sim")), (json!(2), None));

        // A test call to a number off the list (taken off since): nothing is sent.
        let t4 = task("t4", "+16505550142", &a);
        let (code, line) = text_caller(&rt, Some(&control), &t4, &a, true, None).await;
        assert_eq!(code, "not_test_number");
        assert_eq!(line, "簡訊沒傳出：這是測試來電，對方號碼不在測試號碼名單上");
        assert_eq!(next_json(&mut tester, quiet).await, None);
        assert_eq!(next_json(&mut took, quiet).await, None);

        // No SIM set: that phone's default SIM.
        let mut default_sim = rt.clone();
        default_sim.appointment_test_sms_sim = String::new();
        let t5 = task("t5", "+16505550123", &a);
        let (code, msg) = text_and_answer(&control, &mut tester, text_caller(&default_sim, Some(&control), &t5, &a, true, None)).await;
        assert_eq!(code, "sent");
        assert!(msg.get("sim").is_none() && msg.get("sub_id").is_none(), "{msg}");

        // An app that cannot pick a SIM by name: not sent (never from its default SIM).
        control.set_supports_sim_select("pixel-12345", false).await;
        let t6 = task("t6", "+16505550123", &a);
        let (code, line) = text_caller(&rt, Some(&control), &t6, &a, true, None).await;
        assert_eq!((code.as_str(), line.as_str()), ("old_app", "簡訊沒傳出：手機 App 太舊，不支援傳簡訊"));
        assert_eq!(next_json(&mut tester, quiet).await, None);
        assert_eq!(next_json(&mut took, quiet).await, None);

        // The test phone is not connected: offline.
        let mut away = rt.clone();
        away.appointment_test_sms_phone = "pixel-99999".into();
        let t7 = task("t7", "+16505550123", &a);
        assert_eq!(text_caller(&away, Some(&control), &t7, &a, true, None).await.0, "offline");
        assert_eq!(next_json(&mut took, quiet).await, None);

        // The owner picks the test phone itself: the SIM by name, as without a pick.
        control.set_supports_sim_select("pixel-12345", true).await;
        let t8 = task("t8", "+16505550123", &a);
        let (code, msg) = text_and_answer(&control, &mut tester, text_caller(&rt, Some(&control), &t8, &a, true, Some("pixel-12345"))).await;
        assert_eq!(code, "sent");
        assert_eq!((msg["sim"].clone(), msg.get("sub_id")), (json!("Tello"), None));

        // The list and the phone write the number without its country code: the
        // text (to +1…) still goes to that listed number.
        let mut national = rt.clone();
        national.appointment_test_numbers = vec!["6505550123".into()];
        let t9 = task("t9", "6505550123", &a);
        let (code, msg) = text_and_answer(&control, &mut tester, text_caller(&national, Some(&control), &t9, &a, true, None)).await;
        assert_eq!(code, "sent");
        assert_eq!(msg["to"], json!("+16505550123"));
        assert_eq!(next_json(&mut took, quiet).await, None);
    }

    #[tokio::test]
    async fn the_shown_text_route_is_the_one_the_text_takes() {
        use crate::callback::test_socket::{next_json, phone_socket};
        let quiet = std::time::Duration::from_millis(150);
        let control = ControlRegistry::default();
        // pixel-1 ("Pixel 7") takes the calls; pixel-12345 ("Pixel 9") is the test phone.
        let (took_sink, mut took) = phone_socket().await;
        let (test_sink, mut tester) = phone_socket().await;
        control.register_client("pixel-1".into(), took_sink).await;
        control.register_client("pixel-12345".into(), test_sink).await;
        for s in ["pixel-1", "pixel-12345"] {
            control.set_supports_sms(s, true).await;
            control.set_supports_sim_select(s, true).await;
        }
        let connected = control.connected_serials().await;
        let mut rt = plain_settings();
        rt.appointment_test_numbers = vec!["16505550123".into()];
        rt.appointment_test_sms_phone = "pixel 9".into();
        rt.appointment_test_sms_sim = "Tello".into();
        rt.device_labels.insert("pixel-1".into(), "Pixel 7".into());
        rt.device_labels.insert("pixel-12345".into(), "Pixel 9".into());
        rt.device_labels.insert("pixel-23456".into(), "Pixel 8".into());
        let mut a = appt(&later(2, 15), &later(2, 16), "看房");
        a.status = BOOKED.into();
        a.calendar = DEMO_CALENDAR.into();
        a.sim_sub_id = Some(2);
        let mut plain = a.clone();
        plain.calendar = String::new();
        let task = |id: &str, caller: &str, a: &Appointment| TaskRecord { id: id.into(), ..task_for(caller, a) };
        let route = |serial: &str, label: &str, sim: &str, test: bool| SmsRoute {
            serial: serial.into(),
            label: label.into(),
            sim: sim.into(),
            test,
        };

        // Any other call: the phone that took it, from the SIM that took it.
        let t1 = task("r1", "+16505550142", &plain);
        assert_eq!(sms_route(&rt, &connected, &t1), Some(route("pixel-1", "Pixel 7", "", false)));
        assert_eq!(
            task_json(&rt, &connected, &t1)["sms_route"],
            json!({"serial": "pixel-1", "label": "Pixel 7", "test": false})
        );
        let (code, msg) = text_and_answer(&control, &mut took, text_caller(&rt, Some(&control), &t1, &plain, true, None)).await;
        assert_eq!((code.as_str(), msg["sub_id"].clone(), msg.get("sim")), ("sent", json!(2), None));
        assert_eq!(next_json(&mut tester, quiet).await, None);

        // A test call with the test phone and SIM set: that phone (by its name) and that SIM.
        let t2 = task("r2", "+16505550123", &a);
        assert_eq!(sms_route(&rt, &connected, &t2), Some(route("pixel-12345", "Pixel 9", "Tello", true)));
        assert_eq!(
            task_json(&rt, &connected, &t2)["sms_route"],
            json!({"serial": "pixel-12345", "label": "Pixel 9", "sim": "Tello", "test": true})
        );
        let (code, msg) = text_and_answer(&control, &mut tester, text_caller(&rt, Some(&control), &t2, &a, true, None)).await;
        assert_eq!((code.as_str(), msg["sim"].clone(), msg.get("sub_id")), ("sent", json!("Tello"), None));
        assert_eq!(next_json(&mut took, quiet).await, None);

        // A test call with neither set: as any other call.
        let mut unset = rt.clone();
        unset.appointment_test_sms_phone = String::new();
        unset.appointment_test_sms_sim = String::new();
        let t3 = task("r3", "+16505550123", &a);
        assert_eq!(sms_route(&unset, &connected, &t3), Some(route("pixel-1", "Pixel 7", "", false)));
        let (code, msg) = text_and_answer(&control, &mut took, text_caller(&unset, Some(&control), &t3, &a, true, None)).await;
        assert_eq!((code.as_str(), msg["sub_id"].clone(), msg.get("sim")), ("sent", json!(2), None));
        assert_eq!(next_json(&mut tester, quiet).await, None);

        // Only the SIM set: the phone that took the call, the SIM by name.
        let mut sim_only = rt.clone();
        sim_only.appointment_test_sms_phone = String::new();
        let t4 = task("r4", "+16505550123", &a);
        assert_eq!(sms_route(&sim_only, &connected, &t4), Some(route("pixel-1", "Pixel 7", "Tello", true)));
        let (code, msg) = text_and_answer(&control, &mut took, text_caller(&sim_only, Some(&control), &t4, &a, true, None)).await;
        assert_eq!((code.as_str(), msg["sim"].clone(), msg.get("sub_id")), ("sent", json!("Tello"), None));
        assert_eq!(next_json(&mut tester, quiet).await, None);

        // The test phone is not connected: still shown; the text answers offline.
        let mut away = rt.clone();
        away.appointment_test_sms_phone = "pixel 8".into();
        let t5 = task("r5", "+16505550123", &a);
        assert_eq!(sms_route(&away, &connected, &t5), Some(route("pixel-23456", "Pixel 8", "Tello", true)));
        assert_eq!(text_caller(&away, Some(&control), &t5, &a, true, None).await.0, "offline");
        assert_eq!(next_json(&mut took, quiet).await, None);
        assert_eq!(next_json(&mut tester, quiet).await, None);

        // A task without an appointment has no route.
        let mut other = task("r6", "+16505550142", &plain);
        other.appointment = None;
        assert_eq!(sms_route(&rt, &connected, &other), None);
        assert!(task_json(&rt, &connected, &other).get("sms_route").is_none());
    }

    // ---- Review t5 (acceptance): checks added by the reviewer ----

    /// A: "another appointment" (replaces_previous false) and right after it
    /// "change that one" (true) arrive together in one message, each on a task
    /// of its own as the Live session runs them; the later call's task runs
    /// first. The change belongs to the new appointment; the first one stays.
    #[tokio::test]
    async fn review_a_change_that_runs_before_the_appointment_it_changes() {
        let s = session(Mode::Confirm);
        s.run_tool(&propose_args("1", json!({"start": later(2, 10), "title": "看房"}), next_seq())).await;
        let msg = json!({"toolCall": {"functionCalls": [
            {"id": "2", "name": PROPOSE, "args": {"start": later(4, 15), "title": "電話會議", "replaces_previous": false}},
            {"id": "3", "name": PROPOSE, "args": {"start": later(4, 16), "title": "電話會議", "replaces_previous": true}},
        ]}});
        let calls = parse_tool_calls(&msg);
        let (another, change) = (calls[0].clone(), calls[1].clone());
        assert_eq!((another.after, change.after), (0, another.seq));
        let runner = s.clone();
        let changing = tokio::spawn(async move { runner.run_tool(&change).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!changing.is_finished(), "the change waits for the appointment it changes");
        s.run_tool(&another).await;
        changing.await.unwrap();
        let starts: Vec<String> = s.book.outcomes().into_iter().map(|a| a.start).collect();
        assert_eq!(starts, vec![later(2, 10), later(4, 16)], "the first appointment must stay; the new one moves");
    }

    /// C: check_availability is answered while another call holds the
    /// calendar lock for a booking (it never takes that lock).
    #[tokio::test]
    async fn review_a_check_is_not_held_up_by_another_calls_booking() {
        let cal = Arc::new(FakeCal::default());
        let s = auto_session(&cal);
        let check = ToolCall { id: "c".into(), name: CHECK.into(), args: json!({"start": later(6, 10)}), seq: 0, after: 0 };
        let held = calendar_turn().lock().await;
        let answer = tokio::time::timeout(Duration::from_millis(500), s.run_tool(&check)).await;
        drop(held);
        assert_eq!(answer.expect("check waited for the calendar lock"), json!({"free": true}));
    }

    /// B: ✅ on an `auto` appointment whose write answer was lost finds its own
    /// event by its tag: not "taken", moved instead of written twice.
    #[tokio::test]
    async fn review_confirm_is_not_blocked_by_its_own_auto_event() {
        let (root, store, calls) = stores();
        let cal = Arc::new(FakeCal::default());
        let (from, to) = (later(7, 10), later(7, 11));
        cal.events.lock().unwrap().push(("ev9".into(), from.clone(), to.clone(), "tag-own".into()));
        let mut a = appt(&from, &to, "看房");
        a.mode = "auto".into();
        a.tag = "tag-own".into();
        a.auto_error = "日曆沒有及時回應（15 秒）".into();
        let t = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &a) }).await.unwrap();
        let (task, text, done) = decide_inner(Some(Cal::Fake(cal.clone())), &Client::new(), &plain_settings(), &store, &calls, None, &t.id, true)
            .await
            .unwrap();
        assert_eq!(done, Decision::Settled, "{text}");
        assert_eq!(task.appointment.unwrap().event_id, "ev9");
        assert_eq!((count(&cal.inserts), count(&cal.moves)), (0, 1));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// B: the owner put the same thing in the calendar himself, then pressed ✅:
    /// held once, told the time is not free and that another ✅ books it anyway;
    /// the second ✅ books it (and only then texts).
    #[tokio::test]
    async fn review_the_owners_own_entry_for_the_same_time_is_confirmed_on_the_second_press() {
        let (root, store, calls) = stores();
        let cal = Arc::new(FakeCal::default());
        let (from, to) = (later(8, 15), later(8, 16));
        cal.busy.lock().unwrap().push((from.clone(), to.clone()));
        let t = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &appt(&from, &to, "看房")) }).await.unwrap();
        let (task, text, done) = decide_inner(Some(Cal::Fake(cal.clone())), &Client::new(), &plain_settings(), &store, &calls, None, &t.id, true)
            .await
            .unwrap();
        assert_eq!(done, Decision::Held);
        assert!(text.contains("這個時段日曆上已經有行程") && text.contains("再按一次同意就照樣排入"), "{text}");
        assert!(!text.contains("其他預約"), "{text}");
        let held = task.appointment.unwrap();
        assert_eq!((held.status.as_str(), held.held_busy), (PENDING, true));
        // The card and the console show the same words.
        assert!(held.write_error.contains("再按一次同意就照樣排入"), "{}", held.write_error);
        assert_eq!(count(&cal.inserts), 0);
        let (task, text, done) = decide_inner(Some(Cal::Fake(cal.clone())), &Client::new(), &plain_settings(), &store, &calls, None, &t.id, true)
            .await
            .unwrap();
        assert_eq!(done, Decision::Settled, "{text}");
        assert!(text.starts_with("✅ 已同意預約"), "{text}");
        let done = task.appointment.unwrap();
        assert_eq!((done.status.as_str(), done.event_id.as_str(), done.held_busy), (CONFIRMED, "ev1", false));
        assert!(done.write_error.is_empty());
        assert_eq!(count(&cal.inserts), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// D: the numbers named in the review ticket.
    #[test]
    fn review_sms_numbers_from_the_ticket() {
        assert_eq!(sms_number_on("+12025550143", None).as_deref(), Some("+12025550143"));
        assert_eq!(sms_number_on("+12025550143", Some("tw")).as_deref(), Some("+12025550143"));
        assert_eq!(sms_number_on("2025550143", Some("us")).as_deref(), Some("+12025550143"));
        assert_eq!(sms_number_on("0912345678", Some("tw")).as_deref(), Some("+886912345678"));
        assert_eq!(sms_number_on("+819012345678", None).as_deref(), Some("+819012345678"));
        assert_eq!(sms_number_on("02-2345-6789", Some("tw")), None);
        for sim in [None, Some("tw"), Some("us")] {
            assert_eq!(sms_number_on("", sim), None, "{sim:?}");
            assert_eq!(sms_number_on("unknown", sim), None, "{sim:?}");
        }
        // The old entry point reads a number without a country code as Taiwanese.
        assert_eq!(sms_number("0912345678").as_deref(), Some("+886912345678"));
        assert_eq!(sms_number("2025550143"), None);
        // A US toll-free number is not texted (see the test below).
        assert_eq!(sms_number_on("+18005551234", None), None);
    }

    /// D: North American toll-free numbers (800/888/877/866/855/844/833) are
    /// business lines that do not take ordinary texts.
    #[test]
    fn review_toll_free_numbers_are_not_texted() {
        for n in ["+18005551234", "+1 888 555 1234", "+18775551234", "+18665551234", "+18555551234", "+18445551234", "+18335551234"] {
            assert_eq!(sms_number_on(n, None), None, "{n}");
        }
        // Premium numbers neither; an ordinary number that only contains 800 still is.
        assert_eq!(sms_number_on("+19005551234", None), None);
        assert_eq!(sms_number_on("8005551234", Some("us")), None);
        assert_eq!(sms_number_on("+12128005555", None).as_deref(), Some("+12128005555"));
    }

    /// E: retry on an id that does not exist is an error (the endpoint answers 404 first).
    #[tokio::test]
    async fn review_retry_on_a_missing_task_is_refused() {
        let (root, store, calls) = stores();
        let r = retry_sms(&plain_settings(), &store, &calls, None, "no-such-task", None).await;
        assert!(r.is_err(), "{r:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// E: a number that cannot take texts ("no_number") gets a retry that can only fail again.
    #[test]
    fn review_no_number_is_not_offered_again() {
        let mut a = appt(&later(2, 15), &later(2, 16), "看房");
        a.status = CONFIRMED.into();
        a.sms = "no_number".into();
        assert!(!sms_retryable(&a));
    }

    /// The end of a call waits [`TOOL_WAIT`] for proposals: an `auto` booking
    /// that cannot get the calendar (an owner's ✅ is writing) soon leaves it
    /// to the owner instead of writing after the call was read.
    #[tokio::test]
    async fn an_auto_booking_waits_for_the_calendar_only_so_long() {
        assert!(CALENDAR_TURN_WAIT + BOOK_CHECK_TIMEOUT + WRITE_TIMEOUT < TOOL_WAIT);
        assert!(BOOK_CHECK_TIMEOUT + WRITE_TIMEOUT <= Duration::from_secs(15));
        let cal = Arc::new(FakeCal::default());
        let s = ApptSession { turn_wait: Duration::from_millis(100), ..auto_session(&cal) };
        let held = calendar_turn().lock().await;
        let started = std::time::Instant::now();
        let r = s.run_tool(&propose_call(&later(9, 10), &later(9, 11))).await;
        drop(held);
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        assert_eq!(r, json!({"status": "pending_owner_confirmation", "not_booked": true}));
        assert_eq!(count(&cal.inserts), 0);
        let out = s.book.outcome().unwrap();
        assert_eq!(out.status, PENDING);
        assert_eq!(out.auto_error, "日曆正在寫入另一筆預約");
        // Once the calendar is free again a booking goes in as before.
        let s2 = auto_session(&cal);
        assert_eq!(s2.run_tool(&propose_call(&later(9, 10), &later(9, 11))).await, json!({"status": "booked"}));
    }
}
