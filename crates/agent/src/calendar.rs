// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Calendar suggestion: when a call fixes a date (a delivery window, an
//! appointment, a time someone will call back), the summary model is asked for
//! it as a small JSON object. The owner adds it to their own calendar with one
//! tap; no calendar account is connected to the agent. Which calendar is the
//! owner's choice (setting `calendar_app`, [`APPS`]): "Add to calendar" in the
//! notification and the web console opens Google Calendar's, Outlook.com's,
//! Microsoft 365's or Yahoo's "new event" form filled in, or the event's
//! `.ics` file, which Apple Calendar and every other calendar app open
//! ([`Event::link_for`]).
//!
//! Times are the machine's local time, written without a zone, so calendar apps
//! read them in the owner's own time zone.

use chrono::{Duration, NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::locale::{self, Lang};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub title: String,
    /// Local time, `YYYY-MM-DDTHH:MM` (all-day: midnight of the day).
    pub start: String,
    /// Local time; all-day: midnight after the last day.
    pub end: String,
    #[serde(default)]
    pub all_day: bool,
}

const STAMP: &str = "%Y-%m-%dT%H:%M";
/// A suggestion further out than this is more likely a misread date.
const MAX_DAYS_AHEAD: i64 = 400;

/// Setting `calendar_app`: where "Add to calendar" puts an event — Google
/// Calendar, Outlook.com, Microsoft 365 (Outlook with a work or school
/// account), Apple Calendar (the `.ics` file: iPhone Safari offers to add it),
/// Yahoo Calendar, or the `.ics` file for any other app. Empty = not chosen
/// yet: the web console asks on the first press.
pub const APPS: [&str; 6] = ["google", "outlook", "office365", "apple", "yahoo", "ics"];

/// One of [`APPS`] (any case); anything else is empty (not chosen).
pub fn sanitize_app(s: &str) -> String {
    let t = s.trim().to_ascii_lowercase();
    APPS.iter().find(|a| **a == t).map(|a| a.to_string()).unwrap_or_default()
}

/// The web calendars' "new event" forms for one event, filled in (Apple
/// Calendar and other apps take the `.ics` file instead).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Links {
    pub google: String,
    pub outlook: String,
    pub office365: String,
    pub yahoo: String,
}

/// What the summary model is asked. `now` = the machine's local time.
pub fn extraction_prompt(lang: Lang, owner: &str, now: NaiveDateTime) -> String {
    let owner = crate::prompts::owner_label(lang, owner);
    let language = locale::phrases(lang).name_en;
    format!(
        "You read a phone call that {owner}'s AI phone answered and decide whether {owner} should put something in the calendar.\n\
Only when the call fixes a concrete date: an appointment, a visit, a delivery window, a deadline, a time someone will call back or come by.\n\
The current local date and time is {} ({}). Resolve relative dates (\"tomorrow afternoon\", \"next Wednesday\") against it. \
A part of the day without a clock time: morning = 09:00, noon = 12:00, afternoon = 14:00, evening = 19:00.\n\
Answer with JSON only, no other text.\n\
- Nothing to schedule, no concrete date, or a sales / scam call: {{\"event\": null}}\n\
- Otherwise: {{\"event\": {{\"title\": \"<a few words, in {language}>\", \"start\": \"YYYY-MM-DDTHH:MM\", \"end\": \"YYYY-MM-DDTHH:MM, or empty when unknown\", \"all_day\": false}}}}\n\
For a whole day without a time set \"all_day\": true and give the date at 00:00.\n\
Do not invent a date the call does not give.",
        now.format("%Y-%m-%d %H:%M"),
        now.format("%A"),
    )
}

fn parse_stamp(s: &str) -> Option<NaiveDateTime> {
    let t = s.trim();
    for fmt in [STAMP, "%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M", "%Y-%m-%d %H:%M:%S"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(t, fmt) {
            return Some(dt);
        }
    }
    // With a zone (`…+08:00`, `…Z`): the wall-clock part is what was meant.
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(t) {
        return Some(dt.naive_local());
    }
    NaiveDate::parse_from_str(t, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
}

enum ParseOutcome {
    Event(Event),
    NoEvent,
    Malformed,
}

fn parse_outcome(reply: &str, now: NaiveDateTime) -> ParseOutcome {
    let (Some(a), Some(b)) = (reply.find('{'), reply.rfind('}')) else {
        return ParseOutcome::Malformed;
    };
    let Some(slice) = reply.get(a..=b) else {
        return ParseOutcome::Malformed;
    };
    let Ok(v) = serde_json::from_str::<Value>(slice) else {
        return ParseOutcome::Malformed;
    };
    let Some(event_val) = v.get("event") else {
        return ParseOutcome::Malformed;
    };
    if event_val.is_null() {
        return ParseOutcome::NoEvent;
    }
    let Some(ev) = event_val.as_object() else {
        return ParseOutcome::Malformed;
    };
    let Some(title_raw) = ev.get("title").and_then(Value::as_str) else {
        return ParseOutcome::NoEvent;
    };
    let title: String = title_raw.chars().filter(|c| !c.is_control()).take(120).collect();
    let title = title.trim().to_string();
    if title.is_empty() {
        return ParseOutcome::NoEvent;
    }
    let all_day = ev.get("all_day").and_then(Value::as_bool).unwrap_or(false);
    let Some(start_str) = ev.get("start").and_then(Value::as_str) else {
        return ParseOutcome::NoEvent;
    };
    let Some(mut start) = parse_stamp(start_str) else {
        return ParseOutcome::NoEvent;
    };
    if all_day {
        let Some(d) = start.date().and_hms_opt(0, 0, 0) else {
            return ParseOutcome::NoEvent;
        };
        start = d;
    }
    let earliest = if all_day {
        let Some(d) = now.date().and_hms_opt(0, 0, 0) else {
            return ParseOutcome::NoEvent;
        };
        d
    } else {
        now - Duration::hours(1)
    };
    if start < earliest || start > now + Duration::days(MAX_DAYS_AHEAD) {
        return ParseOutcome::NoEvent;
    }
    let default_len = if all_day { Duration::days(1) } else { Duration::hours(1) };
    let mut end = ev
        .get("end")
        .and_then(|x| x.as_str())
        .and_then(parse_stamp)
        .filter(|e| *e > start && *e <= start + Duration::days(14))
        .unwrap_or(start + default_len);
    if all_day {
        let Some(d) = end.date().and_hms_opt(0, 0, 0) else {
            return ParseOutcome::NoEvent;
        };
        end = d;
        if end <= start {
            end = start + Duration::days(1);
        }
    }
    ParseOutcome::Event(Event {
        title,
        start: start.format(STAMP).to_string(),
        end: end.format(STAMP).to_string(),
        all_day,
    })
}

/// The model's answer as an event, or `None`: no event, not JSON, no title, a
/// date in the past or too far ahead.
pub fn parse_event(reply: &str, now: NaiveDateTime) -> Option<Event> {
    match parse_outcome(reply, now) {
        ParseOutcome::Event(ev) => Some(ev),
        ParseOutcome::NoEvent | ParseOutcome::Malformed => None,
    }
}

impl Event {
    fn times(&self) -> Option<(NaiveDateTime, NaiveDateTime)> {
        Some((parse_stamp(&self.start)?, parse_stamp(&self.end)?))
    }

    /// `20261114T143000/20261114T153000`, or `20261114/20261115` for whole days.
    fn compact_range(&self) -> Option<(String, String)> {
        let (s, e) = self.times()?;
        let fmt = if self.all_day { "%Y%m%d" } else { "%Y%m%dT%H%M%S" };
        Some((s.format(fmt).to_string(), e.format(fmt).to_string()))
    }

    /// When it is, for the notification line, as the agent's language writes it:
    /// "Nov 14, 14:30" / "11月14日 14:30" (the date alone for whole days).
    pub fn when(&self) -> String {
        self.when_in(crate::locale::notify_lang())
    }

    /// [`Event::when`] in `lang`: the owner reads it, so callers pass the
    /// notification language (setting `notify_language`).
    pub fn when_in(&self, lang: Lang) -> String {
        match self.times() {
            Some((s, _)) if self.all_day => crate::locale::date_only(lang, &s.date()),
            Some((s, _)) => crate::locale::date_time(lang, &s),
            None => self.start.clone(),
        }
    }

    /// Opens Google Calendar's "new event" form filled in. No account needed
    /// on our side; the times have no zone, so they land in the owner's own.
    pub fn google_link(&self, details: &str) -> String {
        let Some((s, e)) = self.compact_range() else {
            return String::new();
        };
        let mut url = format!(
            "https://calendar.google.com/calendar/render?action=TEMPLATE&text={}&dates={s}/{e}",
            pct(&self.title)
        );
        let details = details_text(details);
        if !details.is_empty() {
            url.push_str(&format!("&details={}", pct(&details)));
        }
        url
    }

    /// Outlook.com (a personal Microsoft account): its "new event" form filled in.
    pub fn outlook_link(&self, details: &str) -> String {
        self.microsoft_link("https://outlook.live.com", details)
    }

    /// Microsoft 365 (Outlook with a work or school account).
    pub fn office365_link(&self, details: &str) -> String {
        self.microsoft_link("https://outlook.office.com", details)
    }

    /// Outlook on the web's compose deeplink. `startdt` / `enddt` without a
    /// zone are read in the signed-in user's own; a whole-day event gives
    /// dates, the end being the day after the last, with `allday=true`.
    fn microsoft_link(&self, host: &str, details: &str) -> String {
        let Some((s, e)) = self.times() else {
            return String::new();
        };
        let fmt = if self.all_day { "%Y-%m-%d" } else { "%Y-%m-%dT%H:%M:%S" };
        let mut url = format!(
            "{host}/calendar/0/deeplink/compose?path=/calendar/action/compose&rru=addevent&subject={}&startdt={}&enddt={}",
            pct(&self.title),
            s.format(fmt),
            e.format(fmt)
        );
        if self.all_day {
            url.push_str("&allday=true");
        }
        let details = details_text(details);
        if !details.is_empty() {
            url.push_str(&format!("&body={}", pct(&details)));
        }
        url
    }

    /// Yahoo Calendar's "new event" form (`v=60`). Yahoo has no time zone
    /// parameter: `st` / `et` (`YYYYMMDDTHHMMSS`) are wall-clock times. One
    /// whole day is its date with `dur=allday` (Yahoo ignores `dur` once `et`
    /// is there); several whole days give `et`, the day after the last.
    pub fn yahoo_link(&self, details: &str) -> String {
        let (Some((s, e)), Some((a, b))) = (self.compact_range(), self.times()) else {
            return String::new();
        };
        let mut url = format!("https://calendar.yahoo.com/?v=60&title={}&st={s}", pct(&self.title));
        if self.all_day && b - a <= Duration::days(1) {
            url.push_str("&dur=allday");
        } else {
            url.push_str(&format!("&et={e}"));
        }
        let details = details_text(details);
        if !details.is_empty() {
            url.push_str(&format!("&desc={}", pct(&details)));
        }
        url
    }

    /// All four web calendars' forms.
    pub fn links(&self, details: &str) -> Links {
        Links {
            google: self.google_link(details),
            outlook: self.outlook_link(details),
            office365: self.office365_link(details),
            yahoo: self.yahoo_link(details),
        }
    }

    /// What "Add to calendar" opens for the calendar the owner chose (setting
    /// `calendar_app`): that web calendar's form, or — Apple Calendar, `.ics` —
    /// `ics_url`, the event's file (empty when there is no address other
    /// devices can open). Not chosen yet: Google Calendar's form — callers
    /// that link for the owner check the choice first and send them to the web
    /// console to make it instead.
    pub fn link_for(&self, app: &str, details: &str, ics_url: &str) -> String {
        match sanitize_app(app).as_str() {
            "outlook" => self.outlook_link(details),
            "office365" => self.office365_link(details),
            "yahoo" => self.yahoo_link(details),
            "apple" | "ics" => ics_url.trim().to_string(),
            _ => self.google_link(details),
        }
    }

    /// An iCalendar file with this one event (floating local time).
    pub fn ics(&self, uid: &str, details: &str, stamp_utc: NaiveDateTime) -> String {
        let Some((s, e)) = self.compact_range() else {
            return String::new();
        };
        let value = if self.all_day { ";VALUE=DATE" } else { "" };
        let mut out = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//bageo-call//EN\r\nBEGIN:VEVENT\r\n");
        // The UID domain stays the one from before the rename to Bageo, so an
        // event downloaded again updates the one already imported.
        out.push_str(&format!("UID:{}@myna-call\r\n", ics_text(uid)));
        out.push_str(&format!("DTSTAMP:{}Z\r\n", stamp_utc.format("%Y%m%dT%H%M%S")));
        out.push_str(&format!("DTSTART{value}:{s}\r\n"));
        out.push_str(&format!("DTEND{value}:{e}\r\n"));
        out.push_str(&fold(&format!("SUMMARY:{}", ics_text(&self.title))));
        if !details.trim().is_empty() {
            out.push_str(&fold(&format!("DESCRIPTION:{}", ics_text(details.trim()))));
        }
        out.push_str("END:VEVENT\r\nEND:VCALENDAR\r\n");
        out
    }
}

fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// What goes into a form's description: trimmed, at most 600 characters.
fn details_text(details: &str) -> String {
    details.trim().chars().take(600).collect()
}

/// The console token (`CALLS_UI_TOKEN` or `ui_password`), updated when settings
/// change. Only whether one is set matters here: links to an `.ics` that leave
/// the console (notifications, Telegram) then carry a key ([`link_key`]).
/// Every link is signed with the random key in [`QUICK_LINK_KEY_FILE`]
/// ([`quick_action_key`]), never with the token itself.
static LINK_SECRET: std::sync::RwLock<String> = std::sync::RwLock::new(String::new());

pub fn set_link_secret(token: &str) {
    *LINK_SECRET.write().unwrap_or_else(|e| e.into_inner()) = token.trim().to_string();
}

pub fn link_secret() -> String {
    LINK_SECRET.read().unwrap_or_else(|e| e.into_inner()).clone()
}

fn ics_mac(key: &[u8], what: &str) -> Option<hmac::Hmac<sha2::Sha256>> {
    use hmac::{Hmac, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(key).ok()?;
    mac.update(b"bageo-ics2");
    mac.update(&(what.len() as u64).to_le_bytes());
    mac.update(what.as_bytes());
    Some(mac)
}

/// The key that opens one `.ics` without the console token (`what` =
/// `call/<id>` or `tasks/<id>`, its path under `/calls/api/`): an HMAC with
/// `key`, all 32 bytes in hex.
fn link_key_with(key: &[u8], what: &str) -> String {
    use hmac::Mac;
    ics_mac(key, what).map_or_else(String::new, |m| to_hex(&m.finalize().into_bytes()))
}

/// [`link_key_with`] the link key ([`quick_action_key`]).
pub fn link_key(what: &str) -> String {
    link_key_with(quick_action_key(), what)
}

fn key_opens_with(key: &[u8], what: &str, k: &str) -> bool {
    // Keys from before (16 bytes, made with the console password) or of any
    // other length never open a file.
    let Some(k) = hex_bytes(k).filter(|k| k.len() == QUICK_LINK_KEY_LEN) else {
        return false;
    };
    // verify_slice compares in constant time.
    ics_mac(key, what).is_some_and(|m| hmac::Mac::verify_slice(m, &k).is_ok())
}

/// `k` is the one [`link_key`] gives for `what`.
pub fn key_opens(what: &str, k: &str) -> bool {
    key_opens_with(quick_action_key(), what, k)
}

/// Address of an `.ics` for a link outside the console: `<public_url>/calls/api/
/// <what>/event.ics`, with its key when the console has a token, or when other
/// devices need one without a password set (`AGENT_HOST` not set, `crate::lan`).
/// Empty without `public_url`: no address other devices are known to open.
pub fn ics_url(public_url: &str, what: &str) -> String {
    let key = (!link_secret().trim().is_empty() || crate::lan::auto()).then(quick_action_key);
    ics_url_with(public_url, what, key)
}

/// How long a one-tap notification link (`/calls/api/quick-action`) works.
pub const QUICK_ACTION_TTL_SECS: i64 = 7 * 24 * 60 * 60;

/// File in the data directory (`CALLS_DATA_DIR`, next to `settings.json`)
/// holding the key one-tap links are signed with (64 hex digits, mode 600).
const QUICK_LINK_KEY_FILE: &str = "quick-link.key";

/// Bytes in a one-tap link's key and in its signature.
const QUICK_LINK_KEY_LEN: usize = 32;

/// Bytes in the signature of links from before [`QUICK_LINK_KEY_FILE`]
/// (signed with the console password itself): never acted on.
const OLD_QUICK_SIG_LEN: usize = 16;

/// 32 random bytes (from the system's random source, through uuid v4).
fn fresh_link_key() -> [u8; QUICK_LINK_KEY_LEN] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for _ in 0..4 {
        h.update(uuid::Uuid::new_v4().as_bytes());
    }
    let mut out = [0u8; QUICK_LINK_KEY_LEN];
    out.copy_from_slice(&h.finalize());
    out
}

fn hex_bytes(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 || !s.is_ascii() {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

/// The key kept in `path`; made (and written there, mode 600) when the file
/// is missing or does not hold one. An error when it can be neither read nor
/// written.
fn load_or_create_link_key(path: &std::path::Path) -> std::io::Result<[u8; QUICK_LINK_KEY_LEN]> {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            if let Some(k) = hex_bytes(&s).and_then(|b| <[u8; QUICK_LINK_KEY_LEN]>::try_from(b).ok()) {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
                }
                return Ok(k);
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let key = fresh_link_key();
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("key.tmp");
    {
        use std::io::Write as _;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(to_hex(&key).as_bytes())?;
        f.sync_all()?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(key)
}

/// The key every one-tap link is signed with: random, kept in
/// [`QUICK_LINK_KEY_FILE`] so links still work after a restart. Never the
/// console password: whoever sees a link cannot test password guesses
/// against its signature. When the file can be neither read nor written, a
/// key for this run only (links then stop working at a restart; their page
/// says to open the console).
fn quick_action_key() -> &'static [u8] {
    static KEY: std::sync::OnceLock<[u8; QUICK_LINK_KEY_LEN]> = std::sync::OnceLock::new();
    KEY.get_or_init(|| {
        if cfg!(test) {
            return fresh_link_key();
        }
        let default_dir = if std::path::Path::new("/data/calls").is_dir() { "/data/calls" } else { "data/calls" };
        let path = std::path::PathBuf::from(crate::config::env_or("CALLS_DATA_DIR", default_dir)).join(QUICK_LINK_KEY_FILE);
        load_or_create_link_key(&path).unwrap_or_else(|e| {
            tracing::warn!("one-tap link key {}: {e}; using a key for this run only", path.display());
            fresh_link_key()
        })
    })
}

fn quick_action_mac(key: &[u8], op: &str, target: &str, name: &str, exp: i64) -> Option<hmac::Hmac<sha2::Sha256>> {
    use hmac::{Hmac, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(key).ok()?;
    mac.update(b"bageo-qa3");
    for part in [op.trim(), target.trim(), name.trim(), &exp.to_string()] {
        mac.update(&(part.len() as u64).to_le_bytes());
        mac.update(part.as_bytes());
    }
    Some(mac)
}

/// HMAC over every part of a one-tap link: the action (`confirm_task`,
/// `ignore_task`, `block`, `stall`, `white`, `unblock`), its target, the
/// name it writes and when it stops working (unix seconds). Each part goes in
/// with its length, so no two links share a signature. All 32 bytes, in hex.
fn quick_action_sig_with(key: &[u8], op: &str, target: &str, name: &str, exp: i64) -> String {
    use hmac::Mac;
    quick_action_mac(key, op, target, name, exp).map_or_else(String::new, |m| to_hex(&m.finalize().into_bytes()))
}

/// What a one-tap link's signature and time say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickLink {
    /// Signed by this agent and still within its time.
    Valid,
    /// Past its time, or an older link without one: nothing is done, the page
    /// says to open the console.
    Expired,
    /// Not signed by this agent (or changed since).
    Invalid,
}

fn check_quick_action_with(key: &[u8], op: &str, target: &str, name: &str, exp: &str, sig: &str, now: i64) -> QuickLink {
    let exp = exp.trim();
    // Links from before links had a time (and a name in their signature):
    // never acted on.
    if exp.is_empty() {
        return QuickLink::Expired;
    }
    let Ok(exp) = exp.parse::<i64>() else {
        return QuickLink::Invalid;
    };
    let Some(sig) = hex_bytes(sig) else {
        return QuickLink::Invalid;
    };
    // Links signed before the key file (with the console password): never
    // acted on, whoever made them.
    if sig.len() == OLD_QUICK_SIG_LEN {
        return QuickLink::Expired;
    }
    // verify_slice compares in constant time.
    let same = sig.len() == QUICK_LINK_KEY_LEN
        && quick_action_mac(key, op, target, name, exp).is_some_and(|m| hmac::Mac::verify_slice(m, &sig).is_ok());
    if !same {
        QuickLink::Invalid
    } else if now > exp {
        QuickLink::Expired
    } else {
        QuickLink::Valid
    }
}

/// Check a one-tap link's parts as they came in the address (`exp`: empty for
/// an older link).
pub fn check_quick_action(op: &str, target: &str, name: &str, exp: &str, sig: &str) -> QuickLink {
    check_quick_action_with(quick_action_key(), op, target, name, exp, sig, chrono::Utc::now().timestamp())
}

fn quick_action_url_with(key: &[u8], public_url: &str, op: &str, target: &str, name: &str, now: i64) -> String {
    let base = public_url.trim().trim_end_matches('/');
    if base.is_empty() || op.trim().is_empty() || target.trim().is_empty() {
        return String::new();
    }
    let base = base.strip_suffix("/calls").unwrap_or(base);
    let exp = now + QUICK_ACTION_TTL_SECS;
    let sig = quick_action_sig_with(key, op, target, name, exp);
    let mut u = format!(
        "{base}/calls/api/quick-action?op={}&target={}",
        pct(op.trim()),
        pct(target.trim()),
    );
    if !name.trim().is_empty() {
        u.push_str(&format!("&name={}", pct(name.trim())));
    }
    u.push_str(&format!("&exp={exp}&sig={sig}"));
    u
}

/// Signed one-tap action URL for notification buttons/links outside
/// Telegram. Opening it shows a page asking to confirm; only that page's
/// button does the action, within [`QUICK_ACTION_TTL_SECS`].
pub fn quick_action_url(public_url: &str, op: &str, target: &str, name: &str) -> String {
    quick_action_url_with(quick_action_key(), public_url, op, target, name, chrono::Utc::now().timestamp())
}

/// `key`: the link key, when the console has a token.
fn ics_url_with(public_url: &str, what: &str, key: Option<&[u8]>) -> String {
    let base = public_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return String::new();
    }
    let base = base.strip_suffix("/calls").unwrap_or(base);
    let mut url = format!("{base}/calls/api/{what}/event.ics");
    if let Some(key) = key {
        let k = link_key_with(key, what);
        if !k.is_empty() {
            url.push_str(&format!("?k={k}"));
        }
    }
    url
}

/// iCalendar TEXT escaping.
fn ics_text(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace("\r\n", "\\n")
        .replace('\n', "\\n")
        .replace('\r', "")
}

/// Content lines are at most 75 octets; continuation lines start with a space.
fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut len = 0usize;
    for c in line.chars() {
        let n = c.len_utf8();
        if len + n > 74 {
            out.push_str("\r\n ");
            len = 1;
        }
        out.push(c);
        len += n;
    }
    out.push_str("\r\n");
    out
}

/// Ask the summary model whether the call fixed a date. `call` = what the
/// summary was written from ([`crate::summary::build_summary_input`]).
pub async fn suggest(
    http: &reqwest::Client,
    settings: &crate::config::Settings,
    call: &str,
) -> Option<Event> {
    let endpoint = crate::summary::summary_endpoint(settings)?;
    suggest_at(http, &endpoint, call).await
}

async fn suggest_at(
    http: &reqwest::Client,
    endpoint: &crate::summary::Endpoint,
    call: &str,
) -> Option<Event> {
    let active = locale::active();
    let now = chrono::Local::now().naive_local();
    let system = extraction_prompt(active.lang, &active.owner, now);
    let max_tokens = if endpoint.url == crate::summary::GEMINI_OPENAI_URL { 600 } else { 200 };
    for model in &endpoint.models {
        match crate::summary::chat_at(http, endpoint, model, &system, call, max_tokens).await {
            Ok(reply) if !reply.trim().is_empty() => match parse_outcome(&reply, now) {
                ParseOutcome::Event(ev) => return Some(ev),
                ParseOutcome::NoEvent => return None,
                ParseOutcome::Malformed => tracing::warn!("calendar suggestion malformed model={model}"),
            },
            Ok(_) => tracing::warn!("calendar suggestion empty model={model}"),
            Err(e) => tracing::warn!("calendar suggestion failed model={model}: {e:#}"),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 12)
            .unwrap()
            .and_hms_opt(15, 0, 0)
            .unwrap()
    }

    #[test]
    fn reads_an_event_with_or_without_an_end() {
        let ev = parse_event(
            r#"Sure: {"event": {"title": "Parcel delivery", "start": "2026-11-13T14:00", "end": "", "all_day": false}}"#,
            now(),
        )
        .unwrap();
        assert_eq!(ev.title, "Parcel delivery");
        assert_eq!(ev.start, "2026-11-13T14:00");
        assert_eq!(ev.end, "2026-11-13T15:00");
        let ev = parse_event(
            r#"{"event":{"title":"牙醫回診","start":"2026-11-20T10:30:00+08:00","end":"2026-11-20T11:15"}}"#,
            now(),
        )
        .unwrap();
        assert_eq!((ev.start.as_str(), ev.end.as_str()), ("2026-11-20T10:30", "2026-11-20T11:15"));
        assert_eq!(ev.when(), "11月20日 10:30");
    }

    #[test]
    fn whole_days_end_the_next_midnight() {
        let ev = parse_event(
            r#"{"event":{"title":"Tax deadline","start":"2026-11-30","end":"","all_day":true}}"#,
            now(),
        )
        .unwrap();
        assert!(ev.all_day);
        assert_eq!((ev.start.as_str(), ev.end.as_str()), ("2026-11-30T00:00", "2026-12-01T00:00"));
        assert_eq!(ev.when(), "11月30日");
        // Today as a whole day is still fine at 15:00.
        assert!(parse_event(r#"{"event":{"title":"x","start":"2026-11-12","all_day":true}}"#, now()).is_some());
    }

    #[test]
    fn refuses_what_is_not_a_usable_event() {
        for reply in [
            r#"{"event": null}"#,
            "no event here",
            r#"{"event":{"title":"","start":"2026-11-13T14:00"}}"#,
            r#"{"event":{"title":"Old","start":"2026-11-10T14:00"}}"#,
            r#"{"event":{"title":"Far","start":"2029-01-01T10:00"}}"#,
            r#"{"event":{"title":"No time","start":"sometime next week"}}"#,
            r#"{"event":{"title":"x"}}"#,
        ] {
            assert_eq!(parse_event(reply, now()), None, "{reply}");
        }
        // An end before the start is replaced, not trusted.
        let ev = parse_event(
            r#"{"event":{"title":"Call back","start":"2026-11-13T09:00","end":"2026-11-13T08:00"}}"#,
            now(),
        )
        .unwrap();
        assert_eq!(ev.end, "2026-11-13T10:00");
    }

    #[test]
    fn google_link_and_ics_carry_floating_local_time() {
        let ev = Event {
            title: "牙醫 & 洗牙".into(),
            start: "2026-11-20T10:30".into(),
            end: "2026-11-20T11:15".into(),
            all_day: false,
        };
        let link = ev.google_link("From a call, 0912…");
        assert!(link.starts_with("https://calendar.google.com/calendar/render?action=TEMPLATE&text=%E7%89%99%E9%86%AB%20%26%20"), "{link}");
        assert!(link.contains("&dates=20261120T103000/20261120T111500"), "{link}");
        assert!(link.contains("&details=From%20a%20call%2C%200912"), "{link}");
        let stamp = NaiveDate::from_ymd_opt(2026, 11, 12).unwrap().and_hms_opt(7, 0, 0).unwrap();
        let ics = ev.ics("call-1", "Line one\nLine; two", stamp);
        assert!(ics.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"));
        assert!(ics.contains("UID:call-1@myna-call\r\n"));
        assert!(ics.contains("DTSTAMP:20261112T070000Z\r\n"));
        assert!(ics.contains("DTSTART:20261120T103000\r\nDTEND:20261120T111500\r\n"));
        assert!(ics.contains("SUMMARY:牙醫 & 洗牙\r\n"));
        assert!(ics.contains("DESCRIPTION:Line one\\nLine\\; two\r\n"));
        assert!(ics.ends_with("END:VEVENT\r\nEND:VCALENDAR\r\n"));
        let day = Event { all_day: true, start: "2026-11-30T00:00".into(), end: "2026-12-01T00:00".into(), ..ev };
        assert!(day.google_link("").ends_with("&dates=20261130/20261201"));
        assert!(day.ics("d", "", stamp).contains("DTSTART;VALUE=DATE:20261130\r\nDTEND;VALUE=DATE:20261201\r\n"));
    }

    #[test]
    fn outlook_office365_and_yahoo_links_carry_local_time_and_whole_days() {
        let ev = Event {
            title: "看房 & 簽約".into(),
            start: "2026-11-20T10:30".into(),
            end: "2026-11-20T11:15".into(),
            all_day: false,
        };
        // Outlook.com and Microsoft 365: the same form on two hosts, times
        // without a zone (the signed-in user's own).
        let tail = "/calendar/0/deeplink/compose?path=/calendar/action/compose&rru=addevent\
                    &subject=%E7%9C%8B%E6%88%BF%20%26%20%E7%B0%BD%E7%B4%84\
                    &startdt=2026-11-20T10:30:00&enddt=2026-11-20T11:15:00&body=From%20a%20call";
        assert_eq!(ev.outlook_link("From a call"), format!("https://outlook.live.com{tail}"));
        assert_eq!(ev.office365_link(" From a call\n"), format!("https://outlook.office.com{tail}"));
        assert!(!ev.outlook_link("").contains("&body=") && !ev.outlook_link("").contains("allday"));
        // Yahoo: wall-clock st / et, no zone.
        assert_eq!(
            ev.yahoo_link("Line one\nLine two"),
            "https://calendar.yahoo.com/?v=60&title=%E7%9C%8B%E6%88%BF%20%26%20%E7%B0%BD%E7%B4%84\
             &st=20261120T103000&et=20261120T111500&desc=Line%20one%0ALine%20two"
        );
        // One whole day: dates with the day after as the end (Outlook), the
        // date with dur=allday and no et (Yahoo), the dates range (Google).
        let day = Event { all_day: true, start: "2026-11-30T00:00".into(), end: "2026-12-01T00:00".into(), ..ev.clone() };
        let ms = day.outlook_link("");
        assert!(ms.ends_with("&startdt=2026-11-30&enddt=2026-12-01&allday=true"), "{ms}");
        assert!(day.office365_link("").starts_with("https://outlook.office.com/") && day.office365_link("").contains("&allday=true"));
        assert!(day.yahoo_link("").ends_with("&st=20261130&dur=allday"), "{}", day.yahoo_link(""));
        assert!(day.google_link("").ends_with("&dates=20261130/20261201"));
        // Several whole days: Yahoo takes et, the day after the last.
        let days = Event { end: "2026-12-03T00:00".into(), ..day.clone() };
        assert!(days.yahoo_link("").ends_with("&st=20261130&et=20261203"), "{}", days.yahoo_link(""));
        assert!(days.outlook_link("").contains("&startdt=2026-11-30&enddt=2026-12-03&allday=true"));
        // An unreadable time gives no link at all.
        let bad = Event { start: "soon".into(), ..ev.clone() };
        let none = Links::default();
        assert_eq!(bad.links("x"), none);
        let all = ev.links("x");
        assert!(all.google.starts_with("https://calendar.google.com/") && all.outlook.starts_with("https://outlook.live.com/"));
        assert!(all.office365.starts_with("https://outlook.office.com/") && all.yahoo.starts_with("https://calendar.yahoo.com/?v=60&"));
    }

    #[test]
    fn the_chosen_calendar_decides_the_link() {
        let ev = Event {
            title: "Dentist".into(),
            start: "2026-11-20T10:30".into(),
            end: "2026-11-20T11:15".into(),
            all_day: false,
        };
        let ics = "http://bageo.local/calls/api/call/c1/event.ics";
        assert_eq!(ev.link_for("google", "", ics), ev.google_link(""));
        assert_eq!(ev.link_for(" Outlook ", "", ics), ev.outlook_link(""));
        assert_eq!(ev.link_for("office365", "", ics), ev.office365_link(""));
        assert_eq!(ev.link_for("yahoo", "", ics), ev.yahoo_link(""));
        assert_eq!(ev.link_for("apple", "", ics), ics);
        assert_eq!(ev.link_for("ics", "", ics), ics);
        // Apple / .ics without an address: no link rather than another calendar's.
        assert_eq!(ev.link_for("apple", "", " "), "");
        // Not chosen yet (or a value from elsewhere): Google Calendar, as before.
        assert_eq!(ev.link_for("", "", ics), ev.google_link(""));
        assert_eq!(ev.link_for("gcal", "", ics), ev.google_link(""));
        for app in APPS {
            assert_eq!(sanitize_app(&app.to_uppercase()), app);
        }
        for other in ["", "outlook.com", "google calendar", "auto"] {
            assert_eq!(sanitize_app(other), "", "{other}");
        }
    }

    #[test]
    fn ics_links_outside_the_console_carry_a_key_not_the_token() {
        let key = [9u8; 32];
        assert_eq!(ics_url_with("", "call/c1", Some(&key)), "");
        assert_eq!(ics_url_with("http://bageo.local/", "call/c1", None), "http://bageo.local/calls/api/call/c1/event.ics");
        assert_eq!(
            ics_url_with("https://phone.example.com/calls/", "tasks/t1", None),
            "https://phone.example.com/calls/api/tasks/t1/event.ics"
        );
        let url = ics_url_with("http://bageo.local", "call/c1", Some(&key));
        let k = link_key_with(&key, "call/c1");
        assert_eq!(k.len(), 64);
        assert_eq!(url, format!("http://bageo.local/calls/api/call/c1/event.ics?k={k}"));
        assert!(key_opens_with(&key, "call/c1", &k));
        // Another file, another key, a cut or changed key, no key: refused.
        assert!(!key_opens_with(&key, "call/c2", &k));
        assert!(!key_opens_with(&[8u8; 32], "call/c1", &k));
        assert!(!key_opens_with(&key, "call/c1", &k[..62]));
        let mut changed = k.clone();
        changed.replace_range(..1, if k.starts_with('0') { "1" } else { "0" });
        assert!(!key_opens_with(&key, "call/c1", &changed));
        assert!(!key_opens_with(&key, "call/c1", ""));
        assert!(!key_opens_with(&key, "call/c1", "not-hex"));
        // The run's own key opens what it signs.
        assert!(key_opens("tasks/t1", &link_key("tasks/t1")));
    }

    #[test]
    fn old_ics_keys_made_with_the_password_never_open_a_file() {
        // The format before: 16 bytes of an HMAC keyed with the console password.
        let old = {
            use hmac::{Hmac, Mac};
            let mut mac = Hmac::<sha2::Sha256>::new_from_slice(b"pw-test").unwrap();
            mac.update(b"bageo-ics:");
            mac.update(b"call/c1");
            to_hex(&mac.finalize().into_bytes()[..16])
        };
        assert_eq!(old.len(), 32);
        assert!(!key_opens_with(b"pw-test", "call/c1", &old));
        assert!(!key_opens("call/c1", &old));
        // A link's key is not made from the password.
        assert_ne!(&link_key_with(b"pw-test", "call/c1")[..32], old.as_str());
    }

    #[test]
    fn long_ics_lines_are_folded_on_character_boundaries() {
        let folded = fold(&format!("SUMMARY:{}", "預約".repeat(40)));
        for line in folded.trim_end().split("\r\n") {
            assert!(line.len() <= 75, "{} octets", line.len());
        }
        assert!(folded.split("\r\n").skip(1).filter(|l| !l.is_empty()).all(|l| l.starts_with(' ')));
        assert_eq!(folded.replace("\r\n ", "").trim_end(), format!("SUMMARY:{}", "預約".repeat(40)));
    }

    #[test]
    fn the_model_is_told_today_and_the_language() {
        let p = extraction_prompt(Lang::Ja, "田中", now());
        assert!(p.contains("2026-11-12 15:00 (Thursday)") && p.contains("in Japanese") && p.contains("田中"));
        assert!(p.contains(r#"{"event": null}"#));
    }

    /// The parts of a one-tap link as its page receives them (decoded).
    fn parts(url: &str) -> std::collections::HashMap<String, String> {
        let q = url.split_once('?').unwrap().1;
        url::form_urlencoded::parse(q.as_bytes()).into_owned().collect()
    }

    #[test]
    fn quick_links_are_never_signed_with_a_public_key_or_the_password() {
        let key = quick_action_key();
        assert_eq!(key.len(), 32);
        assert_ne!(key, b"bageo-local-quick-action-key".as_slice());
        assert_eq!(key, quick_action_key(), "one key for the whole run");
        // A link made with someone else's key does not pass here.
        let now = 1_800_000_000;
        let forged = quick_action_url_with(b"bageo-local-quick-action-key", "http://bageo.local", "white", "0912000000", "", now);
        let f = parts(&forged);
        assert_eq!(
            check_quick_action_with(key, "white", "0912000000", "", &f["exp"], &f["sig"], now),
            QuickLink::Invalid
        );
        let pw = quick_action_url_with(b"pw-test", "http://bageo.local", "white", "0912000000", "", now);
        let p = parts(&pw);
        assert_eq!(check_quick_action_with(key, "white", "0912000000", "", &p["exp"], &p["sig"], now), QuickLink::Invalid);
    }

    #[test]
    fn quick_link_signatures_are_full_length_and_old_password_links_only_expire() {
        let key = [7u8; 32];
        let now = 1_800_000_000;
        let u = quick_action_url_with(&key, "http://bageo.local", "block", "0912000003", "", now);
        let p = parts(&u);
        assert_eq!(p["sig"].len(), 64, "{u}");
        assert_eq!(check_quick_action_with(&key, "block", "0912000003", "", &p["exp"], &p["sig"], now), QuickLink::Valid);
        // Cut short, or one digit changed: refused.
        assert_eq!(check_quick_action_with(&key, "block", "0912000003", "", &p["exp"], &p["sig"][..48], now), QuickLink::Invalid);
        let mut flipped = p["sig"].clone();
        let last = if flipped.ends_with('0') { "1" } else { "0" };
        flipped.replace_range(63.., last);
        assert_eq!(check_quick_action_with(&key, "block", "0912000003", "", &p["exp"], &flipped, now), QuickLink::Invalid);
        // A link from before (16-byte signature made with the console password):
        // the expired page, never the action, even when the signature was right then.
        let old = {
            use hmac::{Hmac, Mac};
            let mut mac = Hmac::<sha2::Sha256>::new_from_slice(b"pw-test").unwrap();
            mac.update(b"bageo-qa2");
            let exp = (now + 60).to_string();
            for part in ["block", "0912000003", "", exp.as_str()] {
                mac.update(&(part.len() as u64).to_le_bytes());
                mac.update(part.as_bytes());
            }
            to_hex(&mac.finalize().into_bytes()[..16])
        };
        assert_eq!(check_quick_action_with(b"pw-test", "block", "0912000003", "", &(now + 60).to_string(), &old, now), QuickLink::Expired);
        assert_eq!(check_quick_action_with(&key, "block", "0912000003", "", &(now + 60).to_string(), &old, now), QuickLink::Expired);
        assert_eq!(check_quick_action_with(&key, "block", "0912000003", "", &p["exp"], "not-hex", now), QuickLink::Invalid);
    }

    #[test]
    fn the_quick_link_key_is_kept_in_the_data_directory() {
        let dir = std::env::temp_dir().join(format!("bageo-qlk-{}", uuid::Uuid::new_v4()));
        let path = dir.join(QUICK_LINK_KEY_FILE);
        let first = load_or_create_link_key(&path).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert_eq!(saved, to_hex(&first));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // After a restart: the same key, so links sent before still work.
        assert_eq!(load_or_create_link_key(&path).unwrap(), first);
        // A file that does not hold a key is replaced with a new one.
        std::fs::write(&path, "pw-test").unwrap();
        let next = load_or_create_link_key(&path).unwrap();
        assert_ne!(next, first);
        assert_eq!(load_or_create_link_key(&path).unwrap(), next);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn quick_links_sign_the_name_and_their_time_and_expire() {
        let key = b"test-link-key".to_vec();
        let now = 1_800_000_000;
        let u = quick_action_url_with(&key, "http://bageo.local/calls/", "block", "0912000001", "測試 公司", now);
        assert!(u.starts_with("http://bageo.local/calls/api/quick-action?op=block&target=0912000001&name="), "{u}");
        let p = parts(&u);
        let check = |name: &str, target: &str, exp: &str, at: i64| check_quick_action_with(&key, "block", target, name, exp, &p["sig"], at);
        assert_eq!(check(&p["name"], "0912000001", &p["exp"], now), QuickLink::Valid);
        // Another name, number or time with the same signature: refused.
        assert_eq!(check("別的名字", "0912000001", &p["exp"], now), QuickLink::Invalid);
        assert_eq!(check(&p["name"], "0912000002", &p["exp"], now), QuickLink::Invalid);
        let later = (now + QUICK_ACTION_TTL_SECS * 10).to_string();
        assert_eq!(check(&p["name"], "0912000001", &later, now), QuickLink::Invalid);
        // Past its time: expired; an older link without a time: expired too.
        assert_eq!(check(&p["name"], "0912000001", &p["exp"], now + QUICK_ACTION_TTL_SECS + 1), QuickLink::Expired);
        assert_eq!(check(&p["name"], "0912000001", "", now), QuickLink::Expired);
        assert_eq!(check(&p["name"], "0912000001", "soon", now), QuickLink::Invalid);
    }

    #[test]
    fn the_event_time_is_written_in_the_language_asked_for() {
        let ev = Event { title: "x".into(), start: "2026-11-20T10:30".into(), end: "2026-11-20T11:30".into(), all_day: false };
        assert_eq!(ev.when_in(Lang::ZhTw), "11月20日 10:30");
        assert_ne!(ev.when_in(Lang::En), ev.when_in(Lang::ZhTw));
    }

    #[tokio::test]
    async fn suggest_stops_on_null_event_and_retries_fallback_on_truncated_json() {
        use axum::{extract::State, routing::post, Json, Router};
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        let calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/chat/completions",
                post(|State(n): State<Arc<AtomicUsize>>, Json(body): Json<Value>| async move {
                    n.fetch_add(1, Ordering::SeqCst);
                    let model = body["model"].as_str().unwrap_or("");
                    let content = if model == "first-null" {
                        r#"{"event": null}"#.to_string()
                    } else if model == "first-truncated" {
                        r#"{"event": {"title": "Cut"#.to_string()
                    } else {
                        let tomorrow = (chrono::Local::now().date_naive() + chrono::Duration::days(1)).format("%Y-%m-%d");
                        format!(r#"{{"event": {{"title": "Dentist", "start": "{tomorrow}T10:00", "end": "", "all_day": false}}}}"#)
                    };
                    Json(serde_json::json!({ "choices": [{ "message": { "content": content } }] }))
                }),
            )
            .with_state(calls.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let http = reqwest::Client::new();
        let url = format!("http://{addr}/chat/completions");

        // Valid {"event": null} returns None after 1 call (no fallback).
        let ep_null = crate::summary::Endpoint {
            url: url.clone(),
            key: String::new(),
            models: vec!["first-null".into(), "fallback-ok".into()],
        };
        assert_eq!(suggest_at(&http, &ep_null, "call").await, None);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Truncated/malformed JSON from first model retries fallback model.
        calls.store(0, Ordering::SeqCst);
        let ep_bad = crate::summary::Endpoint {
            url,
            key: String::new(),
            models: vec!["first-truncated".into(), "fallback-ok".into()],
        };
        let ev = suggest_at(&http, &ep_bad, "call").await.unwrap();
        assert_eq!(ev.title, "Dentist");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        server.abort();
    }
}
