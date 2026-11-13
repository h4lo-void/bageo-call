//! Appointments: while a call is on, the AI can look at the owner's Google
//! Calendar (free or busy, never what is in it) and take the time a caller
//! asks for. After the call the owner confirms it with a Telegram button — or,
//! in `auto` mode, it is already in the calendar — and the caller can get a
//! text message back through the phone that took the call.
//!
//! Off by default (setting `appointment_mode`). How the pieces fit:
//! - [`ApptSession::for_call`] is made per call. The Live session
//!   ([`crate::gemini_live::run_session`]) adds [`tool_declarations`] and
//!   [`ApptSession::prompt_block`] to a normal call's setup (never to the
//!   sales / scam stalling session) and answers each `toolCall` with
//!   [`ApptSession::run_tool`] on a task of its own, so the audio loop never waits.
//! - What the caller agreed to is kept in the call's [`CallAppointments`] and
//!   goes with the call record ([`crate::calls::CallRecord::appointment`]).
//! - After the call [`create_task`] and [`after_summary`] write the task and
//!   send the card, the text message or both; [`decide`] carries out the
//!   owner's ✅ / ❌ (Telegram button or `POST /calls/api/tasks/{id}/…`).
//!
//! Times are the machine's local time (`TZ`), written `YYYY-MM-DDTHH:MM`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{Datelike, Duration as Span, NaiveDateTime};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::callback::{ControlRegistry, SmsOutcome, SMS_RESULT_WAIT};
use crate::calls::{CallRecord, CallStore};
use crate::locale::{self, Lang, Phrases};
use crate::settings_store::RuntimeSettings;
use crate::tasks::{ReplyMethod, TaskIntent, TaskPatch, TaskRecord, TaskStatus, TaskStore};

/// Setting `appointment_mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The AI takes no appointments (default).
    Off,
    /// The AI says it will confirm; the owner decides from a Telegram card.
    Confirm,
    /// A free time is taken on the call and put in Google Calendar.
    Auto,
}

impl Mode {
    /// Anything that is not `confirm` or `auto` is off.
    pub fn parse(s: &str) -> Mode {
        match s.trim().to_ascii_lowercase().as_str() {
            "confirm" => Mode::Confirm,
            "auto" => Mode::Auto,
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
/// Put in the calendar on the call (`auto`).
pub const BOOKED: &str = "booked";
/// The owner said yes.
pub const CONFIRMED: &str = "confirmed";
/// The owner said no.
pub const DECLINED: &str = "declined";

pub const CHECK: &str = "check_availability";
pub const PROPOSE: &str = "propose_appointment";

/// How long a calendar check may take: the caller is waiting on the line.
const CHECK_TIMEOUT: Duration = Duration::from_secs(8);
/// How long writing to the calendar may take. Its own limit (not the check's)
/// so a slow insert is not cut short and taken for a failure; when it is cut
/// anyway, the event is found again by its tag ([`Appointment::tag`]).
const WRITE_TIMEOUT: Duration = Duration::from_secs(15);
/// The longest one function call can run (check, then write), plus a margin.
pub const TOOL_WAIT: Duration = Duration::from_secs(24);
const STAMP: &str = "%Y-%m-%dT%H:%M";
const TITLE_MAX: usize = 80;
const NAME_MAX: usize = 40;
const SMS_TEXT_MAX: usize = 500;

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
    /// `confirm` or `auto`: how the call took it.
    pub mode: String,
    /// [`PENDING`], [`BOOKED`], [`CONFIRMED`] or [`DECLINED`].
    pub status: String,
    /// Google Calendar event id once it is in the calendar.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub event_id: String,
    /// `auto` could not book it (the owner decides instead): why.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auto_error: String,
    /// Times this call had already put in the calendar before the caller changed it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaced: Vec<Earlier>,
    /// The text message to the caller: `sent`, or why not (`offline`,
    /// `old_app`, `timeout`, `busy`, `no_number`, the phone's error code). Empty = none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sms: String,
    /// This call's mark on the events it writes (Google Calendar private
    /// property `myna_call`): finds them again when an answer was lost.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tag: String,
    /// A write to the calendar timed out: it may or may not be there (look it up by `tag`).
    #[serde(skip)]
    pub write_unsure: bool,
    /// The Telegram card sent for it, to take its buttons off when the owner
    /// decides on the web or through the API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tg_card: Option<TgCard>,
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
}

impl Earlier {
    fn of(a: &Appointment) -> Self {
        Self {
            start: a.start.clone(),
            end: a.end.clone(),
            title: a.title.clone(),
            event_id: a.event_id.clone(),
            tag: a.tag.clone(),
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

    /// As a calendar entry for the web console (`.ics`, "added to Google
    /// Calendar" — only once it stands there at this time).
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
            google_event_id: if self.status == BOOKED || self.status == CONFIRMED {
                self.event_id.clone()
            } else {
                String::new()
            },
        }
    }
}

impl Earlier {
    fn when(&self, lang: Lang) -> String {
        format!("{} {}", when_range(lang, &self.start, &self.end), self.title.trim())
            .trim()
            .to_string()
    }
}

fn texts() -> &'static Phrases {
    locale::phrases(locale::current())
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
}

#[derive(Debug, Default)]
struct Book {
    current: Option<Appointment>,
    earlier: Vec<Earlier>,
    /// Judged a sales / scam call: nothing of this call is kept.
    discarded: bool,
    /// Calendar events this call wrote that no longer stand (the call was
    /// judged sales / scam): deleted after the call.
    voided: Vec<Earlier>,
}

/// Counts a running proposal while it lives.
struct InFlight<'a>(&'a std::sync::atomic::AtomicUsize);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl CallAppointments {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn book(&self) -> std::sync::MutexGuard<'_, Book> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The caller agreed to this; a later one replaces it. False: the call was
    /// judged sales / scam, nothing is kept (an event it already wrote goes on
    /// the voided list, to be deleted after the call).
    pub fn record(&self, appt: Appointment) -> bool {
        let mut b = self.book();
        if b.discarded {
            if !appt.event_id.is_empty() || appt.write_unsure {
                b.voided.push(Earlier::of(&appt));
            }
            return false;
        }
        if let Some(prev) = b.current.take() {
            // A moved event keeps its id: it is not an earlier one.
            if !prev.event_id.is_empty() && prev.event_id != appt.event_id {
                b.earlier.push(Earlier::of(&prev));
            }
        }
        b.current = Some(appt);
        true
    }

    /// The call turned out to be sales / scam: drop what it agreed to. Events
    /// it already put in the calendar move to the voided list.
    pub fn discard(&self) {
        let mut b = self.book();
        b.discarded = true;
        if let Some(cur) = b.current.take() {
            if !cur.event_id.is_empty() || cur.write_unsure {
                b.voided.push(Earlier::of(&cur));
            }
        }
        let earlier = std::mem::take(&mut b.earlier);
        b.voided.extend(earlier);
    }

    pub fn is_discarded(&self) -> bool {
        self.book().discarded
    }

    /// The event this call has in the calendar right now (empty = none).
    pub fn booked_event(&self) -> Option<Appointment> {
        self.book().current.clone().filter(|a| !a.event_id.is_empty())
    }

    /// Calendar events of this call that no longer stand (to delete).
    pub fn voided(&self) -> Vec<Earlier> {
        self.book().voided.clone()
    }

    fn start_proposal(&self) -> InFlight<'_> {
        self.in_flight.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        InFlight(&self.in_flight)
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

    /// The call's appointment (the last one agreed), with the earlier times
    /// that are already in the calendar.
    pub fn outcome(&self) -> Option<Appointment> {
        let b = self.book();
        if b.discarded {
            return None;
        }
        let mut a = b.current.clone()?;
        a.replaced = b.earlier.clone();
        Some(a)
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
                        "end": { "type": "STRING", "description": "Local end time, YYYY-MM-DDTHH:MM. Leave out for one hour." }
                    },
                    "required": ["start"]
                }
            },
            {
                "name": PROPOSE,
                "description": "Record the time agreed with the caller. Answers {\"status\": \"booked\"} (it is in the calendar) or {\"status\": \"pending_owner_confirmation\"} (not settled yet). Call it again only when the two of you change the agreed time; the last call counts. A back-up time the caller mentions is not a change.",
                "parameters": {
                    "type": "OBJECT",
                    "properties": {
                        "start": { "type": "STRING", "description": "Local start time, YYYY-MM-DDTHH:MM." },
                        "weekday": { "type": "STRING", "description": "The weekday of start in English (Monday … Sunday), the day the caller means." },
                        "end": { "type": "STRING", "description": "Local end time, YYYY-MM-DDTHH:MM. Leave out for one hour." },
                        "title": { "type": "STRING", "description": "What it is about, a few words." },
                        "caller_name": { "type": "STRING", "description": "The caller's name, only when they said it. Leave it out otherwise: never a word like \"friend\" or \"caller\"." }
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
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: Value,
}

/// The function calls in a server message (none for every other message).
pub fn parse_tool_calls(msg: &Value) -> Vec<ToolCall> {
    msg.pointer("/toolCall/functionCalls")
        .and_then(Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .filter_map(|c| {
                    Some(ToolCall {
                        id: c.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                        name: c.get("name")?.as_str()?.to_string(),
                        args: c.get("args").cloned().unwrap_or_else(|| json!({})),
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

/// Google Calendar access from the settings (`gcal_*`). Never printed.
#[derive(Clone)]
pub struct CalendarAccess {
    client_id: String,
    client_secret: String,
    refresh_token: String,
    calendar_id: String,
}

impl std::fmt::Debug for CalendarAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CalendarAccess(..)")
    }
}

impl CalendarAccess {
    /// None until the Google Calendar credentials are filled in.
    pub fn from_settings(rt: &RuntimeSettings) -> Option<Self> {
        rt.gcal_configured().then(|| Self {
            client_id: rt.gcal_client_id.clone(),
            client_secret: rt.gcal_client_secret.clone(),
            refresh_token: rt.gcal_refresh_token.clone(),
            calendar_id: rt.gcal_calendar_id.clone(),
        })
    }

    fn window(start: &str, end: &str) -> Result<(String, String)> {
        Ok((
            crate::calendar::local_rfc3339(start).context("bad start time")?,
            crate::calendar::local_rfc3339(end).context("bad end time")?,
        ))
    }

    /// Nothing in the calendar between `start` and `end` (local stamps) apart
    /// from this call's own event `own_id` (empty: none; then freeBusy is asked).
    pub async fn is_free_except(&self, http: &Client, start: &str, end: &str, own_id: &str) -> Result<bool> {
        let (a, b) = Self::window(start, end)?;
        if own_id.is_empty() {
            return crate::gcal::query_free(
                http,
                &self.client_id,
                &self.client_secret,
                &self.refresh_token,
                &self.calendar_id,
                &a,
                &b,
            )
            .await;
        }
        let events = crate::gcal::list_events(
            http,
            &self.client_id,
            &self.client_secret,
            &self.refresh_token,
            &self.calendar_id,
            &a,
            &b,
        )
        .await?;
        Ok(!events.iter().any(|e| e.blocks_time && e.id != own_id))
    }

    /// Put it in the calendar, tagged with the call's tag; the new event's id.
    pub async fn insert(&self, http: &Client, appt: &Appointment, caller_id: &str) -> Result<String> {
        let (a, b) = Self::window(&appt.start, &appt.end)?;
        crate::gcal::insert_event_tagged(
            http,
            &self.client_id,
            &self.client_secret,
            &self.refresh_token,
            &self.calendar_id,
            &appt.calendar_title(caller_id),
            &event_description(caller_id, appt),
            &a,
            &b,
            &appt.tag,
        )
        .await
    }

    /// Move an event to the appointment's time (same event, same id).
    pub async fn move_to(&self, http: &Client, event_id: &str, appt: &Appointment) -> Result<()> {
        let (a, b) = Self::window(&appt.start, &appt.end)?;
        crate::gcal::move_event(
            http,
            &self.client_id,
            &self.client_secret,
            &self.refresh_token,
            &self.calendar_id,
            event_id,
            &a,
            &b,
        )
        .await
    }

    pub async fn delete(&self, http: &Client, event_id: &str) -> Result<()> {
        crate::gcal::delete_event(
            http,
            &self.client_id,
            &self.client_secret,
            &self.refresh_token,
            &self.calendar_id,
            event_id,
        )
        .await
    }

    /// Events carrying a call's tag.
    pub async fn find_tagged(&self, http: &Client, tag: &str) -> Result<Vec<String>> {
        crate::gcal::find_tagged(
            http,
            &self.client_id,
            &self.client_secret,
            &self.refresh_token,
            &self.calendar_id,
            tag,
        )
        .await
    }
}

/// Where appointments are checked and written: Google Calendar, or (tests) a
/// stand-in that counts what was written.
#[derive(Clone, Debug)]
enum Cal {
    Google(CalendarAccess),
    #[cfg(test)]
    Fake(Arc<FakeCal>),
}

impl Cal {
    fn from_settings(rt: &RuntimeSettings) -> Option<Self> {
        CalendarAccess::from_settings(rt).map(Cal::Google)
    }

    async fn is_free_except(&self, http: &Client, start: &str, end: &str, own_id: &str) -> Result<bool> {
        match self {
            Cal::Google(c) => c.is_free_except(http, start, end, own_id).await,
            #[cfg(test)]
            Cal::Fake(f) => f.is_free_except(start, end, own_id),
        }
    }

    async fn insert(&self, http: &Client, appt: &Appointment, caller_id: &str) -> Result<String> {
        match self {
            Cal::Google(c) => c.insert(http, appt, caller_id).await,
            #[cfg(test)]
            Cal::Fake(f) => f.insert(appt).await,
        }
    }

    async fn move_to(&self, http: &Client, event_id: &str, appt: &Appointment) -> Result<()> {
        match self {
            Cal::Google(c) => c.move_to(http, event_id, appt).await,
            #[cfg(test)]
            Cal::Fake(f) => f.move_to(event_id, appt).await,
        }
    }

    async fn delete(&self, http: &Client, event_id: &str) -> Result<()> {
        match self {
            Cal::Google(c) => c.delete(http, event_id).await,
            #[cfg(test)]
            Cal::Fake(f) => f.delete(event_id),
        }
    }

    async fn find_tagged(&self, http: &Client, tag: &str) -> Result<Vec<String>> {
        match self {
            Cal::Google(c) => c.find_tagged(http, tag).await,
            #[cfg(test)]
            Cal::Fake(f) => Ok(f.find_tagged(tag)),
        }
    }

    /// Put the appointment in the calendar at its time: move the event it
    /// already has (or that carries its tag after a lost answer), else insert
    /// one. The event's id.
    async fn write(&self, http: &Client, appt: &Appointment, caller_id: &str) -> Result<String> {
        let mut id = appt.event_id.clone();
        if id.is_empty() && appt.mode == Mode::Auto.as_str() && !appt.tag.is_empty() {
            match self.find_tagged(http, &appt.tag).await {
                Ok(found) => id = found.into_iter().next().unwrap_or_default(),
                Err(e) => warn!("appointment tagged lookup failed: {e:#}"),
            }
        }
        if id.is_empty() {
            self.insert(http, appt, caller_id).await
        } else {
            self.move_to(http, &id, appt).await?;
            Ok(id)
        }
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
            anyhow::bail!("Google 日曆刪除行程失敗（HTTP 500）");
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

fn event_description(caller_id: &str, appt: &Appointment) -> String {
    let p = texts();
    let sep = p.sep;
    let mut parts = vec![format!("{}{sep}{}", p.n_number, display_caller(caller_id))];
    if !appt.caller_name.trim().is_empty() {
        parts.push(format!("{}{sep}{}", p.ap_name, appt.caller_name.trim()));
    }
    if !appt.title.trim().is_empty() {
        parts.push(format!("{}{sep}{}", p.ap_what, appt.title.trim()));
    }
    parts.push("Myna".to_string());
    parts.join("\n")
}

/// What one call's Live session needs for appointments.
#[derive(Clone)]
pub struct ApptSession {
    /// [`Mode::Confirm`] or [`Mode::Auto`] (never off; `auto` without calendar
    /// access is confirm).
    pub mode: Mode,
    calendar: Option<Cal>,
    /// The caller can get a text message back (setting on, a mobile number).
    pub reply_by_text: bool,
    caller_id: String,
    http: Client,
    /// This call's mark on the events it writes ([`Appointment::tag`]).
    tag: String,
    /// What the caller agreed to.
    pub book: Arc<CallAppointments>,
}

impl ApptSession {
    /// None when appointments are off.
    pub fn for_call(rt: &RuntimeSettings, http: &Client, caller_id: &str) -> Option<Self> {
        let mode = Mode::parse(&rt.appointment_mode);
        if mode == Mode::Off {
            return None;
        }
        let calendar = Cal::from_settings(rt);
        // Without calendar access nothing can be checked or booked: the owner decides.
        let mode = if calendar.is_none() { Mode::Confirm } else { mode };
        Some(Self {
            mode,
            calendar,
            reply_by_text: rt.appointment_sms && sms_number(caller_id).is_some(),
            caller_id: caller_id.trim().to_string(),
            http: http.clone(),
            tag: uuid::Uuid::new_v4().to_string(),
            book: CallAppointments::new(),
        })
    }

    pub fn can_check(&self) -> bool {
        self.calendar.is_some()
    }

    /// Added after the call's system instruction (the stored prompt is not touched).
    pub fn prompt_block(&self, now: chrono::DateTime<chrono::Local>) -> String {
        prompt_block(
            self.mode,
            self.can_check(),
            self.reply_by_text,
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
            PROPOSE => self.propose(&call.args, now).await,
            other => {
                warn!("appointment tool: unknown function {other}");
                json!({ "error": "unknown_function" })
            }
        }
    }

    /// This call's own event in the calendar (it does not clash with itself).
    fn own_event_id(&self) -> String {
        self.book.booked_event().map(|a| a.event_id).unwrap_or_default()
    }

    async fn check(&self, args: &Value, now: NaiveDateTime) -> Value {
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
                warn!("appointment check timed out after {}s", CHECK_TIMEOUT.as_secs());
                unavailable()
            }
        }
    }

    async fn propose(&self, args: &Value, now: NaiveDateTime) -> Value {
        let pending = json!({ "status": "pending_owner_confirmation" });
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
        let mut appt = Appointment {
            start: stamp(start),
            end: stamp(end),
            title: clean_text(args.get("title"), TITLE_MAX),
            caller_name: clean_text(args.get("caller_name"), NAME_MAX),
            mode: self.mode.as_str().to_string(),
            status: PENDING.to_string(),
            tag: self.tag.clone(),
            ..Default::default()
        };
        if let (Mode::Auto, Some(cal)) = (self.mode, &self.calendar) {
            self.book_auto(cal, &mut appt).await;
        }
        let booked = appt.status == BOOKED;
        info!(
            "appointment proposed start={} end={} mode={} booked={booked} auto_error={:?}",
            appt.start, appt.end, appt.mode, appt.auto_error
        );
        if !self.book.record(appt) {
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

    /// `auto`: check for a clash (this call's own event does not count), then
    /// move that event to the new time — or insert one when there is none yet.
    /// The same time as the event already booked writes nothing. When it cannot
    /// be booked the call's event stays where it is and the appointment keeps its
    /// id, so the owner's ✅ moves it instead of adding a second one.
    async fn book_auto(&self, cal: &Cal, appt: &mut Appointment) {
        let own = self.book.booked_event();
        if let Some(o) = own.as_ref().filter(|o| o.start == appt.start && o.end == appt.end) {
            appt.event_id = o.event_id.clone();
            appt.status = BOOKED.to_string();
            return;
        }
        let own_id = own.map(|o| o.event_id).unwrap_or_default();
        let fail = |appt: &mut Appointment, why: String| {
            appt.auto_error = why;
            if !own_id.is_empty() {
                appt.event_id = own_id.clone();
            }
        };
        match tokio::time::timeout(
            CHECK_TIMEOUT,
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
                fail(appt, format!("Google Calendar: timeout ({}s)", CHECK_TIMEOUT.as_secs()));
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
                fail(appt, format!("Google Calendar: timeout ({}s)", WRITE_TIMEOUT.as_secs()));
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

/// The instructions added after a call's system prompt (English; the model
/// answers in the call's language). Coming last, they win over a prompt line
/// such as "do not agree to plans for the owner".
pub fn prompt_block(
    mode: Mode,
    can_check: bool,
    reply_by_text: bool,
    active: &locale::Active,
    now: NaiveDateTime,
    utc_offset: &str,
) -> String {
    let owner = crate::prompts::owner_label(active.lang, &active.owner);
    let language = locale::phrases(active.lang).name_en;
    let me = !active.assistant;
    // To the owner (what they will do) and to the caller (what the AI says).
    let back = if reply_by_text {
        "text them to confirm"
    } else {
        "get back to them"
    };
    let back_said = if reply_by_text {
        "text you to confirm"
    } else {
        "get back to you"
    };
    let has_plans = if me {
        "you already have something on then".to_string()
    } else {
        format!("{owner} already has something on then")
    };
    let will_check = if me {
        format!("you'll check your schedule and {back}")
    } else {
        format!("you'll check with {owner} and {back}")
    };
    let tentative = if me {
        format!("\"That should work. Let me double-check and I'll {back_said}.\"")
    } else {
        format!("\"That should work. I'll check with {owner} and {back_said}.\"")
    };
    let mut s = format!(
        "\n\n[Appointments — this part overrides anything above about not agreeing to plans or invitations for {owner}]\n"
    );
    let days: Vec<String> = (1..=14)
        .map(|d| (now.date() + Span::days(d)).format("%a %Y-%m-%d").to_string())
        .collect();
    s.push_str(&format!(
        "It is now {} ({}), local time (UTC{utc_offset}). The next 14 days: {}. Work out \"tomorrow\", \"Wednesday\" or \"the 20th\" from these dates. All times you pass to the functions are local, written YYYY-MM-DDTHH:MM; also pass the weekday you mean, which is checked against the date.\n",
        now.format("%Y-%m-%d %H:%M"),
        now.format("%A"),
        days.join(", "),
    ));
    s.push_str(&format!(
        "When the caller asks whether {owner} is free at some time, or wants to set a time (a meeting, a visit, coming by, a call at a set time):\n"
    ));
    s.push_str("- Never make the caller wait and never say that you are checking, looking something up or seeing your schedule, in any language (nothing like \"one moment\", \"let me check\", \"hold on\", \"ちょっと待って\", \"確認するから待って\", \"等一下\"): call the function silently and carry on with the conversation.\n");
    if can_check {
        s.push_str("- As soon as there is a day and a time, call check_availability (start, weekday, and end if they said how long) before you say anything about whether it works. Only a vague time (\"next week\", \"some afternoon\")? Ask which day and what time first.\n");
        s.push_str(&format!(
            "- {{\"free\": false}}: say {has_plans}, never what it is. Only if they ask, suggest another time (you may check that one too).\n"
        ));
        if mode == Mode::Auto {
            s.push_str("- {\"free\": true}: agree naturally, then call propose_appointment.\n");
        } else {
            s.push_str(&format!(
                "- {{\"free\": true}}: say something like {tentative}, in {language} — not that it is settled — then call propose_appointment.\n"
            ));
        }
        s.push_str(&format!(
            "- {{\"free\": null}} (the calendar could not be read): you do not know whether {owner} is free, so do not say it is free or that it should work; say {will_check}, then call propose_appointment.\n"
        ));
    } else {
        s.push_str(&format!(
            "- The calendar cannot be read on this call, so do not call check_availability, and you do not know whether {owner} is free: do not say it is free or that it should work. Once there is a day and a time, say {will_check}, then call propose_appointment. Only a vague time? Ask which day and what time first.\n"
        ));
    }
    s.push_str(&format!(
        "- propose_appointment: start (and end) as agreed, weekday, title = what it is about in a few words in {language}, caller_name only if they said their name (otherwise leave it out). Call it once for the time you agreed on. Call it again only if the two of you really change the agreed time later in the call (the last call counts). A back-up time the caller offers (\"if not, the afternoon works too\") or another time named in passing is not a change: do not call it for that.\n"
    ));
    s.push_str(&format!(
        "- Its answer \"pending_owner_confirmation\" means it is not settled yet: {will_check}, never say it is booked. If it also has \"not_booked\": true, you had already agreed but it could not go into the calendar: briefly tell the caller you still need to confirm. \"booked\" means it is settled and you may say so.\n"
    ));
    s.push_str("- {\"error\": \"weekday_mismatch\"}: the date you sent is another weekday (\"date_is\" says which). Take the right date from the list above and call again; say nothing about it to the caller.\n");
    s.push_str("- After calling propose_appointment do not repeat what you have just told the caller: if you already said it, simply go on with the conversation.\n");
    s.push_str("- Never make an appointment with a sales, insurance, loan, investment or scam caller.");
    s
}

// ---------------------------------------------------------------------------
// Text messages to the caller
// ---------------------------------------------------------------------------

/// A number that takes text messages: a Taiwan mobile number (`09xxxxxxxx`,
/// `+8869xxxxxxxx`), always as `+8869xxxxxxxx`. Landlines, hidden and
/// foreign numbers: None (the owner calls them back).
pub fn sms_number(caller_id: &str) -> Option<String> {
    let t = caller_id.trim();
    if t.is_empty()
        || !t
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | ' ' | '-' | '(' | ')'))
    {
        return None;
    }
    let d: String = t.chars().filter(|c| c.is_ascii_digit()).collect();
    let national = if t.starts_with('+') {
        d.strip_prefix("886")?.to_string()
    } else if let Some(r) = d.strip_prefix("00886") {
        r.to_string()
    } else if d.len() == 12 && d.starts_with("886") {
        d[3..].to_string()
    } else {
        d.strip_prefix('0')?.to_string()
    };
    let national = national.trim_start_matches('0');
    (national.len() == 9 && national.starts_with('9')).then(|| format!("+886{national}"))
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
        SmsOutcome::Failed(code) => (code.clone(), not_sent(&locale::fill(p.ap_sms_code, code))),
    }
}

/// Send the caller the text for this decision through the phone that took the
/// call. Returns the [`Appointment::sms`] code and the owner's line.
async fn text_caller(
    rt: &RuntimeSettings,
    control: Option<&ControlRegistry>,
    task: &TaskRecord,
    appt: &Appointment,
    confirmed: bool,
) -> (String, String) {
    let p = texts();
    let Some(to) = sms_number(&task.caller_id) else {
        return ("no_number".to_string(), p.ap_sms_cannot.to_string());
    };
    let Some(control) = control else {
        return sms_report(&SmsOutcome::Offline, &to);
    };
    let text = sms_text(locale::current(), &rt.owner_name, appt, confirmed);
    let out = control
        .send_sms(&task.serial, &task.id, &to, &text, SMS_RESULT_WAIT)
        .await;
    info!("appointment sms task_id={} serial={} outcome={out:?}", task.id, task.serial);
    sms_report(&out, task.caller_id.trim())
}

// ---------------------------------------------------------------------------
// After the call
// ---------------------------------------------------------------------------

/// The line under the post-call summary: "📅 已排入行事曆：…", or — waiting for
/// the owner — "📅 預約待你確認：… — 到網頁確認：<link>" (`link` = the call's page
/// in the web console; without one, where to go). Every notification channel
/// gets it, so the owner learns of it whatever they use.
pub fn summary_line(appt: &Appointment, link: &str) -> String {
    let lang = locale::current();
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

/// The task for the appointment a call took (`call` = its record).
pub async fn create_task(store: &TaskStore, call: &CallRecord, appt: &Appointment) -> Option<TaskRecord> {
    let lang = locale::current();
    let booked = appt.status == BOOKED;
    let mut events = vec!["appointment_from_call".to_string()];
    if booked {
        events.push("gcal_event_created".to_string());
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
        reply_contact: sms_number(&call.caller_id),
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
    let lang = locale::current();
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
    let sms = if !rt.appointment_sms {
        p.ap_sms_off
    } else if sms_number(&task.caller_id).is_some() {
        p.ap_sms_will
    } else {
        p.ap_sms_cannot
    };
    lines.push(format!("{}{sep}{sms}", p.ap_sms));
    if !appt.auto_error.is_empty() {
        lines.push(locale::fill(p.ap_auto_failed, &appt.auto_error));
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
            // Kept on the task: a decision on the web takes the buttons off.
            Ok(message_id) if message_id != 0 => {
                appt.tg_card = Some(TgCard { chat_id: chat.to_string(), message_id });
                let _ = store
                    .patch(
                        &task.id,
                        TaskPatch {
                            appointment: Some(appt.clone()),
                            ..Default::default()
                        },
                    )
                    .await;
            }
            Ok(_) => {}
            Err(e) => warn!("appointment card failed task_id={}: {e:#}", task.id),
        }
        return;
    }
    if appt.status != BOOKED || !rt.appointment_sms {
        return;
    }
    let (code, line) = text_caller(rt, Some(control), task, &appt, true).await;
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
        let lang = locale::current();
        let text = format!("📅 {}\n{line}", appt.headline(lang));
        if let Err(e) = crate::tg_inbound::send_plain_message(http, token, chat, &text).await {
            warn!("appointment sms report failed task_id={}: {e:#}", task.id);
        }
    }
}

/// Appointment tasks being decided right now: one decision per task at a time
/// (two Telegram presses in one batch, Telegram and the API together, ✅ then ❌).
static DECIDING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

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
/// "already handled" and changes nothing.
pub async fn decide(
    http: &Client,
    rt: &RuntimeSettings,
    store: &TaskStore,
    calls: &CallStore,
    control: Option<&ControlRegistry>,
    task_id: &str,
    confirm: bool,
) -> Result<(TaskRecord, String)> {
    decide_with(Cal::from_settings(rt), http, rt, store, calls, control, task_id, confirm).await
}

/// [`decide`] for a decision made on the web console or through the API. When
/// it acted and a Telegram card went out for the appointment, the card's
/// buttons are taken off and the result is posted in that chat, as after a
/// press on the card (best effort, in the background: Telegram failing changes
/// nothing about the decision).
pub async fn decide_from_web(
    http: &Client,
    rt: &RuntimeSettings,
    store: &TaskStore,
    calls: &CallStore,
    control: Option<&ControlRegistry>,
    task_id: &str,
    confirm: bool,
) -> Result<(TaskRecord, String)> {
    let (task, text, acted) =
        decide_inner(Cal::from_settings(rt), http, rt, store, calls, control, task_id, confirm).await?;
    if let Some(card) = card_to_clear(&rt.telegram_bot_token, task.appointment.as_ref(), acted) {
        let (http, token, reply, id) = (http.clone(), rt.telegram_bot_token.trim().to_string(), text.clone(), task.id.clone());
        tokio::spawn(async move {
            crate::tg_inbound::remove_inline_keyboard(&http, &token, &card.chat_id, card.message_id).await;
            match crate::tg_inbound::send_plain_message(&http, &token, &card.chat_id, &reply).await {
                Ok(()) => info!("appointment card closed after a web decision task_id={id}"),
                Err(e) => warn!("appointment card follow-up failed task_id={id}: {e:#}"),
            }
        });
    }
    Ok((task, text))
}

/// The Telegram card to close after a decision made elsewhere: only when this
/// decision acted (not "already handled"), a card went out and the bot is
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

/// [`decide_with`], also saying whether this decision acted (false: "already handled").
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
) -> Result<(TaskRecord, String, bool)> {
    let lang = locale::current();
    let p = locale::phrases(lang);
    let already = |task: TaskRecord| {
        let headline = task.appointment.as_ref().map(|a| a.headline(lang)).unwrap_or_default();
        (task, format!("{}\n{headline}", p.ap_already).trim_end().to_string(), false)
    };
    let task = store
        .get(task_id)
        .await
        .with_context(|| format!("task not found: {task_id}"))?;
    task.appointment.as_ref().context("not an appointment task")?;
    let Some(_claim) = Deciding::claim(task_id) else {
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
    let mut error: Option<String> = None;
    if confirm {
        store.push_event(task_id, "appointment_confirmed").await?;
        match &cal {
            None => lines.push(p.ap_cal_none.to_string()),
            Some(cal) => match cal.write(http, &appt, &task.caller_id).await {
                Ok(id) => {
                    appt.replaced.retain(|e| e.event_id != id);
                    appt.event_id = id;
                    lines.push(p.ap_cal_added.to_string());
                    store.push_event(task_id, "gcal_event_created").await?;
                }
                Err(e) => {
                    let msg = locale::fill(p.ap_cal_failed, &format!("{e:#}"));
                    warn!("appointment calendar write failed task_id={task_id}: {e:#}");
                    lines.push(msg.clone());
                    error = Some(msg);
                }
            },
        }
        appt.status = CONFIRMED.to_string();
    } else {
        store.push_event(task_id, "appointment_declined").await?;
        // The call's own event (auto mode, a time it could not move to, or one
        // whose write answer was lost and is found by the call's tag): not kept.
        if let Some(cal) = &cal {
            let mut ids: Vec<String> = Vec::new();
            if !appt.event_id.is_empty() {
                ids.push(appt.event_id.clone());
            } else if appt.mode == Mode::Auto.as_str() && !appt.tag.is_empty() {
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
                        store.push_event(task_id, "gcal_event_deleted").await?;
                    }
                    Err(e) => {
                        warn!("appointment calendar delete failed task_id={task_id}: {e:#}");
                        lines.push(locale::fill(p.ap_void_failed, &appt.headline(lang)));
                    }
                }
            }
        }
        appt.status = DECLINED.to_string();
    }
    if rt.appointment_sms {
        let (code, line) = text_caller(rt, control, &task, &appt, confirm).await;
        appt.sms = code;
        lines.push(line);
        store.push_event(task_id, &format!("sms_{}", appt.sms)).await?;
    }
    store
        .patch(
            task_id,
            TaskPatch {
                appointment: Some(appt.clone()),
                calendar_event_id: (!appt.event_id.is_empty()).then(|| appt.event_id.clone()),
                fulfill_error: Some(error.clone()),
                ..Default::default()
            },
        )
        .await?;
    let status = if !confirm {
        TaskStatus::Ignored
    } else if error.is_some() {
        TaskStatus::Failed
    } else {
        TaskStatus::Fulfilled
    };
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
    Ok((updated, text, true))
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
pub async fn delete_voided(http: &Client, rt: &RuntimeSettings, voided: &[Earlier]) -> Vec<Earlier> {
    delete_voided_with(Cal::from_settings(rt), http, voided).await
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
    let lang = locale::current();
    format!("📅 {}", locale::fill(locale::phrases(lang).ap_void_failed, &e.when(lang)))
}

/// The summary line for an event deleted because the summary judged the call
/// a sales call afterwards.
pub fn sales_void_line(e: &Earlier) -> String {
    let lang = locale::current();
    format!("📅 {}", locale::fill(locale::phrases(lang).ap_sales_voided, &e.when(lang)))
}

/// Everything an appointment put in the calendar (the event and earlier ones),
/// for when the whole appointment no longer stands.
pub fn all_events(appt: &Appointment) -> Vec<Earlier> {
    let mut out: Vec<Earlier> = appt.replaced.clone();
    if !appt.event_id.is_empty() || appt.write_unsure {
        out.push(Earlier::of(appt));
    }
    out
}

/// The web console's call notes follow the appointment's state.
async fn update_call_notes(calls: &CallStore, call_id: &str, appt: &Appointment) {
    let Some(mut notes) = calls.load_notes(call_id).await else {
        return;
    };
    notes.appointment = Some(appt.clone());
    notes.calendar = Some(appt.calendar_event());
    if let Err(e) = calls.save_notes(call_id, &notes).await {
        warn!("appointment notes update failed call_id={call_id}: {e:#}");
    }
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
            reply_by_text: false,
            caller_id: "0912345678".into(),
            http: Client::new(),
            tag: "tag-test".into(),
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
        assert_eq!(Mode::parse("AUTO"), Mode::Auto);
        for s in ["", "off", "yes", "on", "automatic"] {
            assert_eq!(Mode::parse(s), Mode::Off, "{s}");
        }
        assert_eq!(Mode::parse(Mode::Auto.as_str()), Mode::Auto);
    }

    #[test]
    fn appointments_follow_the_setting_and_need_the_calendar_for_auto() {
        let http = Client::new();
        let mut rt = RuntimeSettings::from_base(&crate::config::Settings::from_env());
        rt.gcal_client_id = String::new();
        rt.gcal_client_secret = String::new();
        rt.gcal_refresh_token = String::new();
        assert!(ApptSession::for_call(&rt, &http, "0912345678").is_none());
        rt.appointment_mode = "auto".into();
        rt.appointment_sms = true;
        let s = ApptSession::for_call(&rt, &http, "0912345678").unwrap();
        // No calendar access: the owner decides, nothing is checked.
        assert_eq!((s.mode, s.can_check(), s.reply_by_text), (Mode::Confirm, false, true));
        assert!(!ApptSession::for_call(&rt, &http, "0225550123").unwrap().reply_by_text);
        rt.gcal_client_id = "id.apps.example".into();
        rt.gcal_client_secret = "secret-123456".into();
        rt.gcal_refresh_token = "refresh-123456".into();
        let s = ApptSession::for_call(&rt, &http, "").unwrap();
        assert_eq!((s.mode, s.can_check(), s.reply_by_text), (Mode::Auto, true, false));
        assert!(!format!("{:?}", s.calendar).contains("secret"));
    }

    #[test]
    fn mobile_numbers_take_texts_and_others_do_not() {
        for n in ["0912345678", "0912-345-678", "+886912345678", "+886 912 345 678", "886912345678", "00886912345678", "+8860912345678"] {
            assert_eq!(sms_number(n).as_deref(), Some("+886912345678"), "{n}");
        }
        for n in ["", "  ", "0225550123", "+886225550123", "+12025550143", "anonymous", "private", "091234567", "09123456789", "912345678", "+81 90-1234-5678"] {
            assert_eq!(sms_number(n), None, "{n}");
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
            for s in [p.ap_sms_hi_owner, p.ap_sms_ok, p.ap_sms_no, p.ap_auto_failed, p.ap_replaced, p.ap_cal_failed, p.ap_sms_sent, p.ap_sms_not_sent, p.ap_sms_code, p.ap_void_failed, p.ap_sales_voided] {
                assert_eq!(s.matches("{}").count(), 1, "{l:?}: {s}");
            }
            for s in [p.ap_sms_hi, p.ap_confirm, p.ap_decline, p.ap_card_title, p.ap_what, p.ap_name, p.ap_sms, p.ap_sms_will, p.ap_sms_off, p.ap_sms_cannot, p.ap_pending, p.ap_booked, p.ap_busy, p.ap_confirmed, p.ap_declined, p.ap_cal_added, p.ap_cal_none, p.ap_sms_old_app, p.ap_sms_offline, p.ap_sms_timeout, p.ap_already, p.ap_sms_busy, p.ap_cal_removed] {
                assert!(!s.trim().is_empty() && !s.contains('{'), "{l:?}: {s}");
            }
            assert!(p.ap_confirm.starts_with('✅') && p.ap_decline.starts_with('❌'), "{l:?}");
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
            calls[0],
            ToolCall { id: "fc-1".into(), name: CHECK.into(), args: json!({"start": "2026-11-14T15:00"}) }
        );
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
        };
        let r = s.run_tool(&call).await;
        assert_eq!(r["error"], "weekday_mismatch");
        assert_eq!(r["date_is"], start.format("%A").to_string());
        assert_eq!(s.book.outcome(), None);
        let right = ToolCall {
            args: json!({"start": stamp(start), "weekday": start.format("%A").to_string(), "title": "dinner"}),
            ..call
        };
        assert_eq!(s.run_tool(&right).await, json!({"status": "pending_owner_confirmation"}));
        assert_eq!(s.book.outcome().unwrap().start, stamp(start));
    }

    #[tokio::test]
    async fn without_calendar_check_is_unknown_and_proposals_wait_for_the_owner() {
        let s = session(Mode::Confirm);
        let start = (chrono::Local::now().naive_local() + Span::days(2)).format(STAMP).to_string();
        let check = ToolCall { id: "1".into(), name: CHECK.into(), args: json!({"start": start}) };
        assert_eq!(s.run_tool(&check).await, json!({"free": null, "reason": "calendar_unavailable"}));
        let bad = ToolCall { id: "2".into(), name: CHECK.into(), args: json!({"start": "soon"}) };
        assert_eq!(s.run_tool(&bad).await["error"], "bad_start");
        let propose = ToolCall {
            id: "3".into(),
            name: PROPOSE.into(),
            args: json!({"start": start, "title": " 看房\n", "caller_name": "陳先生"}),
        };
        assert_eq!(s.run_tool(&propose).await, json!({"status": "pending_owner_confirmation"}));
        let got = s.book.outcome().unwrap();
        assert_eq!(
            (got.title.as_str(), got.caller_name.as_str(), got.status.as_str(), got.mode.as_str()),
            ("看房", "陳先生", PENDING, "confirm")
        );
        // The last proposal counts.
        let later = (chrono::Local::now().naive_local() + Span::days(3)).format(STAMP).to_string();
        let again = ToolCall { id: "4".into(), name: PROPOSE.into(), args: json!({"start": later, "title": "看房"}) };
        s.run_tool(&again).await;
        assert_eq!(s.book.outcome().unwrap().start, later);
        let other = ToolCall { id: "5".into(), name: "other".into(), args: json!({}) };
        assert_eq!(s.run_tool(&other).await["error"], "unknown_function");
        // Judged sales / scam: nothing is kept, the model still gets an answer.
        s.book.discard();
        assert_eq!(s.run_tool(&propose).await, json!({"status": "pending_owner_confirmation"}));
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
        assert!(acted);
        assert_eq!(card_to_clear("123:abc", done.appointment.as_ref(), acted), Some(card));
        let (again, text, acted) = decide_inner(Some(Cal::Fake(cal.clone())), &http, &plain_settings(), &store, &calls, None, &t.id, false)
            .await
            .unwrap();
        assert!(!acted && text.starts_with("這個預約已經處理過了"), "{text}");
        assert_eq!(card_to_clear("123:abc", again.appointment.as_ref(), acted), None);
        // The card stays out of JSON when none went out.
        assert!(serde_json::to_value(appt("2026-11-14T15:00", "", "x")).unwrap().get("tg_card").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prompt_tells_the_time_and_follows_mode_and_style() {
        let now = at(2026, 11, 13, 14, 5);
        let me = locale::Active { owner: "王先生".into(), ..Default::default() };
        let p = prompt_block(Mode::Confirm, true, true, &me, now, "+08:00");
        assert!(p.contains("2026-11-13 14:05 (Friday)") && p.contains("UTC+08:00"), "{p}");
        // A dated list to count weekdays on: "Wednesday" said on a Friday is the 18th.
        assert!(p.contains("Sat 2026-11-14, Sun 2026-11-15") && p.contains("Wed 2026-11-18") && p.contains("Fri 2026-11-27"), "{p}");
        assert!(!p.contains("Sat 2026-11-28"), "{p}");
        assert!(p.contains("check_availability") && p.contains("propose_appointment"));
        // What the AI says to the caller is about "you"; what it tells itself, "them".
        assert!(p.contains("\"That should work. Let me double-check and I'll text you to confirm.\", in Traditional"), "{p}");
        assert!(p.contains("check your schedule and text them to confirm"), "{p}");
        assert!(p.contains("overrides anything above") && p.contains("王先生"));
        assert!(p.contains("never say that you are checking") && p.contains("ちょっと待って") && p.contains("never what it is"));
        assert!(p.contains("A back-up time the caller offers") && p.contains("do not call it for that"));
        assert!(p.contains("weekday_mismatch"));
        assert!(p.contains("scam caller"));
        let auto = prompt_block(Mode::Auto, true, false, &me, now, "+08:00");
        assert!(auto.contains("agree naturally") && !auto.contains("text them"));
        let blind = prompt_block(Mode::Confirm, false, false, &me, now, "+08:00");
        assert!(blind.contains("do not call check_availability") && blind.contains("do not say it is free"));
        let assistant = locale::Active { assistant: true, ..me };
        let a = prompt_block(Mode::Confirm, true, false, &assistant, now, "+08:00");
        assert!(a.contains("王先生 already has something on") && a.contains("check with 王先生"));
        assert!(a.contains("\"That should work. I'll check with 王先生 and get back to you.\""), "{a}");
    }

    #[test]
    fn card_lists_time_reason_and_whether_a_text_can_go() {
        let mut rt = RuntimeSettings::from_base(&crate::config::Settings::from_env());
        rt.appointment_sms = true;
        let mut a = appt("2026-11-14T15:00", "2026-11-14T16:00", "看房");
        a.caller_name = "陳先生".into();
        let task = task_for("0912345678", &a);
        let (title, body) = card_text(&rt, &task, &a, "");
        assert_eq!(title, "📅 預約待確認");
        assert!(body.contains("號碼：0912345678") && body.contains("時間：11/14（週六）15:00–16:00"), "{body}");
        assert!(body.contains("事由：看房") && body.contains("陳先生"), "{body}");
        assert!(body.contains("簡訊：按確定或婉拒後會傳簡訊告訴對方"), "{body}");
        let landline = task_for("0225550123", &a);
        assert!(card_text(&rt, &landline, &a, "").1.contains("對方號碼收不到簡訊，請自己回電"));
        let hidden = task_for("", &a);
        assert!(card_text(&rt, &hidden, &a, "").1.contains("號碼：未顯示號碼"));
        rt.appointment_sms = false;
        assert!(card_text(&rt, &task, &a, "").1.contains("簡訊：不會傳"));
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
            ("no_service".into(), "簡訊沒傳出：失敗代碼 no_service".into())
        );
    }

    fn stores() -> (std::path::PathBuf, Arc<TaskStore>, Arc<CallStore>) {
        let root = std::env::temp_dir().join(format!("myna-appt-{}", uuid::Uuid::new_v4()));
        let tasks = TaskStore::new(root.join("tasks"));
        let calls = CallStore::new(root.join("calls"), false, false, 30);
        (root, tasks, calls)
    }

    fn plain_settings() -> RuntimeSettings {
        let mut rt = RuntimeSettings::from_base(&crate::config::Settings::from_env());
        rt.appointment_sms = false;
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
        assert!(first.1.starts_with("✅ 已確定預約"), "{}", first.1);
        assert!(second.1.starts_with("這個預約已經處理過了"), "{}", second.1);
        assert_eq!(count(&cal.inserts), 1);
        let done = store.get(&t.id).await.unwrap();
        assert_eq!(done.status, TaskStatus::Fulfilled);
        assert_eq!(done.appointment.as_ref().map(|a| (a.status.as_str(), a.event_id.as_str())), Some((CONFIRMED, "ev1")));
        assert_eq!(done.events.iter().filter(|e| e.starts_with("appointment_")).count(), 1, "{:?}", done.events);
        // Asked while another decision holds the task: nothing happens.
        let t2 = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &a) }).await.unwrap();
        let held = Deciding::claim(&t2.id).unwrap();
        assert!(Deciding::claim(&t2.id).is_none());
        let (_, text) = decide_with(Some(Cal::Fake(cal.clone())), &http, &rt, &store, &calls, None, &t2.id, true).await.unwrap();
        assert!(text.starts_with("這個預約已經處理過了"), "{text}");
        assert_eq!(store.get(&t2.id).await.unwrap().status, TaskStatus::Pending);
        drop(held);
        let (_, text) = decide_with(Some(Cal::Fake(cal.clone())), &http, &rt, &store, &calls, None, &t2.id, true).await.unwrap();
        assert!(text.starts_with("✅ 已確定預約"), "{text}");
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
        let check = ToolCall { id: "c".into(), name: CHECK.into(), args: json!({"start": a}) };
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
        assert!(out.replaced.is_empty() && out.calendar_event().google_event_id.is_empty());
        // …so the owner's ✅ moves it there instead of adding a second one.
        let (root, store, calls) = stores();
        let t = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &out) }).await.unwrap();
        let (done, _) = decide_with(Some(Cal::Fake(cal.clone())), &Client::new(), &plain_settings(), &store, &calls, None, &t.id, true)
            .await
            .unwrap();
        assert_eq!((count(&cal.inserts), count(&cal.moves)), (1, 2));
        assert_eq!(done.appointment.unwrap().event_id, "ev1");
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
        assert!(text.contains("已從 Google 日曆刪除"), "{text}");
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
        assert!(text.contains("已從 Google 日曆刪除"), "{text}");
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
        assert_eq!(running.await.unwrap(), json!({"status": "pending_owner_confirmation"}));
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
        let root = std::env::temp_dir().join(format!("myna-appt-{}", uuid::Uuid::new_v4()));
        let store = TaskStore::new(root.join("tasks"));
        let calls = CallStore::new(root.join("calls"), false, false, 30);
        let mut rt = RuntimeSettings::from_base(&crate::config::Settings::from_env());
        rt.gcal_client_id = String::new();
        rt.appointment_sms = false;
        let a = appt("2026-11-14T15:00", "2026-11-14T16:00", "看房");
        let t = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &a) }).await.unwrap();
        let (done, text) = decide(&Client::new(), &rt, &store, &calls, None, &t.id, true).await.unwrap();
        assert_eq!(done.status, TaskStatus::Fulfilled);
        assert_eq!(done.appointment.as_ref().unwrap().status, CONFIRMED);
        assert!(text.starts_with("✅ 已確定預約\n11/14（週六）15:00–16:00 看房"), "{text}");
        assert!(text.contains("沒有 Google 日曆授權"), "{text}");
        assert!(done.events.contains(&"appointment_confirmed".to_string()));
        // Pressed again: nothing happens twice.
        let (_, again) = decide(&Client::new(), &rt, &store, &calls, None, &t.id, false).await.unwrap();
        assert!(again.starts_with("這個預約已經處理過了"), "{again}");
        // Declined, texts on, the phone is not connected: said so.
        rt.appointment_sms = true;
        let t2 = store.create(TaskRecord { id: String::new(), ..task_for("0912345678", &a) }).await.unwrap();
        let control = ControlRegistry::default();
        let (done, text) = decide(&Client::new(), &rt, &store, &calls, Some(&control), &t2.id, false).await.unwrap();
        assert_eq!(done.status, TaskStatus::Ignored);
        assert_eq!(done.appointment.as_ref().unwrap().sms, "offline");
        assert!(text.starts_with("❌ 已婉拒預約") && text.contains("簡訊沒傳出：手機沒連線"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
