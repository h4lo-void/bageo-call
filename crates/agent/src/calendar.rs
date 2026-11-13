//! Calendar suggestion: when a call fixes a date (a delivery window, an
//! appointment, a time someone will call back), the summary model is asked for
//! it as a small JSON object. The owner then gets
//!
//! - an "add to Google Calendar" link in the notification and the web console
//!   (a plain link: no Google account is connected to the agent),
//! - an `.ics` file from the web console for every other calendar app,
//! - and, only when the Google Calendar credentials are filled in and
//!   `calendar_auto_add` is on, the event inserted right away.
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
    /// Google Calendar event id when it was inserted automatically.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub google_event_id: String,
}

const STAMP: &str = "%Y-%m-%dT%H:%M";
/// A suggestion further out than this is more likely a misread date.
const MAX_DAYS_AHEAD: i64 = 400;

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
        google_event_id: String::new(),
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
        let details: String = details.trim().chars().take(600).collect();
        if !details.is_empty() {
            url.push_str(&format!("&details={}", pct(&details)));
        }
        url
    }

    /// An iCalendar file with this one event (floating local time).
    pub fn ics(&self, uid: &str, details: &str, stamp_utc: NaiveDateTime) -> String {
        let Some((s, e)) = self.compact_range() else {
            return String::new();
        };
        let value = if self.all_day { ";VALUE=DATE" } else { "" };
        let mut out = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//myna-call//EN\r\nBEGIN:VEVENT\r\n");
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

/// Local wall-clock time as RFC 3339 with the machine's offset.
pub fn local_rfc3339(stamp: &str) -> Option<String> {
    use chrono::TimeZone;
    let naive = parse_stamp(stamp)?;
    chrono::Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|dt| dt.to_rfc3339())
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
            google_event_id: String::new(),
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
