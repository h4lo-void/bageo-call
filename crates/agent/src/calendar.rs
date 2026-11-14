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

/// The model's answer as an event, or `None`: no event, not JSON, no title, a
/// date in the past or too far ahead.
pub fn parse_event(reply: &str, now: NaiveDateTime) -> Option<Event> {
    let (a, b) = (reply.find('{')?, reply.rfind('}')?);
    let v: Value = serde_json::from_str(reply.get(a..=b)?).ok()?;
    let ev = v.get("event").filter(|e| e.is_object())?;
    let title: String = ev
        .get("title")?
        .as_str()?
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect();
    let title = title.trim().to_string();
    if title.is_empty() {
        return None;
    }
    let all_day = ev.get("all_day").and_then(|x| x.as_bool()).unwrap_or(false);
    let mut start = parse_stamp(ev.get("start")?.as_str()?)?;
    if all_day {
        start = start.date().and_hms_opt(0, 0, 0)?;
    }
    let earliest = if all_day {
        now.date().and_hms_opt(0, 0, 0)?
    } else {
        now - Duration::hours(1)
    };
    if start < earliest || start > now + Duration::days(MAX_DAYS_AHEAD) {
        return None;
    }
    let default_len = if all_day { Duration::days(1) } else { Duration::hours(1) };
    let mut end = ev
        .get("end")
        .and_then(|x| x.as_str())
        .and_then(parse_stamp)
        .filter(|e| *e > start && *e <= start + Duration::days(14))
        .unwrap_or(start + default_len);
    if all_day {
        // Midnight after the last day; a same-day `end` means one day.
        end = end.date().and_hms_opt(0, 0, 0)?;
        if end <= start {
            end = start + Duration::days(1);
        }
    }
    Some(Event {
        title,
        start: start.format(STAMP).to_string(),
        end: end.format(STAMP).to_string(),
        all_day,
    })
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
        let lang = crate::locale::current();
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
    /// devices can open). Not chosen yet: Google Calendar's form, the one
    /// link notifications carried before there was a choice.
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

/// The console token (`CALLS_UI_TOKEN`), set once at start: links to an
/// `.ics` that leave the console (notifications, Telegram) carry a key made
/// from it ([`link_key`]), never the token itself.
static LINK_SECRET: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub fn set_link_secret(token: &str) {
    let _ = LINK_SECRET.set(token.trim().to_string());
}

/// The key that opens one `.ics` without the console token (`what` =
/// `call/<id>` or `tasks/<id>`, its path under `/calls/api/`). Empty when no
/// token is set: then the file opens without one.
pub fn link_key(token: &str, what: &str) -> String {
    use hmac::{Hmac, Mac};
    let token = token.trim();
    if token.is_empty() {
        return String::new();
    }
    let Ok(mut mac) = Hmac::<sha2::Sha256>::new_from_slice(token.as_bytes()) else {
        return String::new();
    };
    mac.update(b"bageo-ics:");
    mac.update(what.as_bytes());
    mac.finalize().into_bytes().iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// `key` is the one [`link_key`] gives for `what` (compared in full).
pub fn key_opens(token: &str, what: &str, key: &str) -> bool {
    let want = link_key(token, what);
    !want.is_empty()
        && want.len() == key.len()
        && want.bytes().zip(key.bytes()).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0
}

/// Address of an `.ics` for a link outside the console: `<public_url>/calls/api/
/// <what>/event.ics`, with its key when the console has a token. Empty without
/// `public_url`: no address other devices are known to open.
pub fn ics_url(public_url: &str, what: &str) -> String {
    ics_url_with(public_url, what, LINK_SECRET.get().map(String::as_str).unwrap_or(""))
}

fn ics_url_with(public_url: &str, what: &str, token: &str) -> String {
    let base = public_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return String::new();
    }
    let base = base.strip_suffix("/calls").unwrap_or(base);
    let mut url = format!("{base}/calls/api/{what}/event.ics");
    let key = link_key(token, what);
    if !key.is_empty() {
        url.push_str(&format!("?k={key}"));
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
/// summary was written from ([`crate::deepseek::build_summary_input`]).
pub async fn suggest(
    http: &reqwest::Client,
    settings: &crate::config::Settings,
    call: &str,
) -> Option<Event> {
    let endpoint = crate::deepseek::summary_endpoint(settings)?;
    let active = locale::active();
    let now = chrono::Local::now().naive_local();
    let system = extraction_prompt(active.lang, &active.owner, now);
    for model in &endpoint.models {
        match crate::deepseek::chat_at(http, &endpoint, model, &system, call, 200).await {
            Ok(reply) if !reply.trim().is_empty() => return parse_event(&reply, now),
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
        assert_eq!(ics_url_with("", "call/c1", "secret"), "");
        assert_eq!(ics_url_with("http://bageo.local/", "call/c1", ""), "http://bageo.local/calls/api/call/c1/event.ics");
        assert_eq!(
            ics_url_with("https://phone.example.com/calls/", "tasks/t1", ""),
            "https://phone.example.com/calls/api/tasks/t1/event.ics"
        );
        let url = ics_url_with("http://bageo.local", "call/c1", "secret-token");
        let key = link_key("secret-token", "call/c1");
        assert_eq!(key.len(), 32);
        assert_eq!(url, format!("http://bageo.local/calls/api/call/c1/event.ics?k={key}"));
        assert!(!url.contains("secret"));
        assert!(key_opens("secret-token", "call/c1", &key));
        // Another file, another token, a cut key, no token: refused.
        assert!(!key_opens("secret-token", "call/c2", &key));
        assert!(!key_opens("other-token", "call/c1", &key));
        assert!(!key_opens("secret-token", "call/c1", &key[..31]));
        assert!(!key_opens("", "call/c1", ""));
        assert_eq!(link_key(" ", "call/c1"), "");
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
}
