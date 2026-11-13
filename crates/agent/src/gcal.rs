//! Google Calendar via the OAuth2 refresh-token flow: insert an event, and ask
//! whether a time is free (`freeBusy`, appointments). The refresh token needs
//! `calendar.events` to insert and `calendar.freebusy` (or `calendar.readonly`
//! / `calendar`) to ask.
//!
//! Settings (see [`crate::settings_store::RuntimeSettings`]):
//! `gcal_client_id`, `gcal_client_secret`, `gcal_refresh_token`, `gcal_calendar_id`.
//!
//! Operator-facing errors are ZH and never echo OAuth secrets or HTTP response bodies.

use anyhow::{bail, Context, Result};
use chrono::{
    DateTime, Datelike, Duration, FixedOffset, NaiveDate, NaiveTime, TimeZone, Utc, Weekday,
};
use reqwest::Client;
use serde_json::{json, Value};

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const CALENDAR_API: &str = "https://www.googleapis.com/calendar/v3/calendars";
const FREEBUSY_URL: &str = "https://www.googleapis.com/calendar/v3/freeBusy";

/// Google Calendar OAuth fields not set, in the agent's language (also what a
/// task follow-up shows). Never include credential values in an error.
pub fn gcal_missing_cred_msg() -> &'static str {
    crate::locale::phrases(crate::locale::current()).f_gcal_missing
}

/// Exchange a long-lived refresh token for a short-lived access token.
async fn refresh_access_token(
    http: &Client,
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<String> {
    let client_id = validate_client_id(client_id)?;
    let client_secret = validate_client_secret(client_secret)?;
    let refresh_token = validate_refresh_token(refresh_token)?;

    let resp = http
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await
        .context("Google 日曆權杖更新連線失敗")?;

    if !resp.status().is_success() {
        // Never dump response body — may echo client_id / token fragments.
        let status = resp.status().as_u16();
        let _ = resp.text().await;
        bail!("{}", token_http_error_zh(status));
    }

    let data: Value = resp.json().await.context("Google 日曆權杖回應格式錯誤")?;
    data.get("access_token")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Google 日曆權杖回應缺少 access_token — 請至「設定」分頁檢查 Refresh Token"
            )
        })
}

/// Insert a calendar event; returns the created Google event id.
///
/// - `calendar_id`: defaults to `"primary"` when empty.
/// - `start_rfc3339`: when empty, uses `now + 24h` (tentative) and appends a note to `description`.
/// - `end_rfc3339_or_plus1h`: when empty, uses start + 1 hour.
pub async fn insert_event(
    http: &Client,
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    calendar_id: &str,
    summary: &str,
    description: &str,
    start_rfc3339: &str,
    end_rfc3339_or_plus1h: &str,
) -> Result<String> {
    let summary = validate_summary(summary)?;
    let access_token = refresh_access_token(http, client_id, client_secret, refresh_token).await?;
    let cal_id = resolve_calendar_id(calendar_id);

    let now = Utc::now();
    let mut desc = description.trim().to_string();
    let (start_dt, end_dt) = resolve_event_window(
        start_rfc3339.trim(),
        end_rfc3339_or_plus1h.trim(),
        now,
        &mut desc,
    )?;

    let tz = taipei_offset();
    let start_local = start_dt.with_timezone(&tz);
    let end_local = end_dt.with_timezone(&tz);

    let payload = json!({
        "summary": summary,
        "description": desc,
        "start": {
            "dateTime": start_local.to_rfc3339(),
            "timeZone": "Asia/Taipei",
        },
        "end": {
            "dateTime": end_local.to_rfc3339(),
            "timeZone": "Asia/Taipei",
        },
    });

    let url = format!("{CALENDAR_API}/{}/events", urlencoding_path(cal_id));
    let resp = http
        .post(&url)
        .bearer_auth(&access_token)
        .json(&payload)
        .send()
        .await
        .context("Google 日曆建立行程連線失敗")?;

    if !resp.status().is_success() {
        // Never dump response body into fulfill_error.
        let status = resp.status().as_u16();
        let _ = resp.text().await;
        bail!("{}", insert_http_error_zh(status));
    }

    let data: Value = resp
        .json()
        .await
        .context("Google 日曆建立行程回應格式錯誤")?;
    data.get("id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("Google 日曆建立行程回應缺少事件 id"))
}

fn resolve_calendar_id(calendar_id: &str) -> &str {
    let t = calendar_id.trim();
    if t.is_empty() {
        "primary"
    } else {
        t
    }
}

/// The `freeBusy` request for one calendar between two RFC 3339 times
/// (local offset, e.g. `2026-11-14T15:00:00+08:00`).
pub fn freebusy_request(calendar_id: &str, time_min: &str, time_max: &str) -> Value {
    json!({
        "timeMin": time_min,
        "timeMax": time_max,
        "items": [{ "id": resolve_calendar_id(calendar_id) }],
    })
}

/// True when the calendar has nothing in the asked window, from a `freeBusy` answer.
pub fn parse_freebusy(body: &Value, calendar_id: &str) -> Result<bool> {
    let calendars = body
        .get("calendars")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("Google 日曆空閒查詢回應格式錯誤"))?;
    let id = resolve_calendar_id(calendar_id);
    // Keyed by the id asked for; one calendar asked → its one entry.
    let cal = calendars
        .get(id)
        .or_else(|| {
            if calendars.len() == 1 {
                calendars.values().next()
            } else {
                None
            }
        })
        .ok_or_else(|| {
            anyhow::anyhow!("Google 日曆空閒查詢沒有這個日曆的結果 — 請至「設定」分頁檢查 Calendar ID")
        })?;
    if let Some(err) = cal
        .get("errors")
        .and_then(Value::as_array)
        .and_then(|e| e.first())
    {
        // A short code such as notFound / internalError, never a secret.
        let reason: String = err
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(40)
            .collect();
        bail!("Google 日曆空閒查詢失敗（{reason}）— 請至「設定」分頁檢查 Calendar ID 與授權範圍");
    }
    Ok(cal
        .get("busy")
        .and_then(Value::as_array)
        .map_or(true, |b| b.is_empty()))
}

/// Whether the calendar is free between `time_min` and `time_max` (RFC 3339 with offset).
pub async fn query_free(
    http: &Client,
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
    calendar_id: &str,
    time_min: &str,
    time_max: &str,
) -> Result<bool> {
    let start = DateTime::parse_from_rfc3339(time_min.trim())
        .map_err(|_| anyhow::anyhow!("無法解析開始時間"))?;
    let end = DateTime::parse_from_rfc3339(time_max.trim())
        .map_err(|_| anyhow::anyhow!("無法解析結束時間"))?;
    if end <= start {
        bail!("行程結束時間必須晚於開始時間");
    }
    let access_token = refresh_access_token(http, client_id, client_secret, refresh_token).await?;
    let resp = http
        .post(FREEBUSY_URL)
        .bearer_auth(&access_token)
        .json(&freebusy_request(calendar_id, time_min.trim(), time_max.trim()))
        .send()
        .await
        .context("Google 日曆空閒查詢連線失敗")?;
    if !resp.status().is_success() {
        // Never dump the response body.
        let status = resp.status().as_u16();
        let _ = resp.text().await;
        bail!("{}", freebusy_http_error_zh(status));
    }
    let body: Value = resp.json().await.context("Google 日曆空閒查詢回應格式錯誤")?;
    parse_freebusy(&body, calendar_id)
}

fn freebusy_http_error_zh(status: u16) -> String {
    match status {
        403 => "Google 日曆空閒查詢被拒絕：授權範圍不足，需含日曆讀取（calendar 或 calendar.freebusy）— 請重新取得 Refresh Token".to_string(),
        401 => "Google 日曆空閒查詢被拒絕 — 請至「設定」分頁檢查權杖".to_string(),
        404 => "Google 日曆找不到指定日曆 — 請至「設定」分頁檢查 Calendar ID".to_string(),
        _ => format!("Google 日曆空閒查詢失敗（HTTP {status}）"),
    }
}

fn validate_client_id(client_id: &str) -> Result<&str> {
    let client_id = client_id.trim();
    if client_id.is_empty() {
        bail!("{}", gcal_missing_cred_msg());
    }
    Ok(client_id)
}

fn validate_client_secret(client_secret: &str) -> Result<&str> {
    let client_secret = client_secret.trim();
    if client_secret.is_empty() {
        // Never echo the secret.
        bail!("{}", gcal_missing_cred_msg());
    }
    if client_secret.len() < 8 {
        bail!("Google 日曆 Client Secret 過短 — 請至「設定」分頁重新填寫（不會顯示憑證內容）");
    }
    Ok(client_secret)
}

fn validate_refresh_token(refresh_token: &str) -> Result<&str> {
    let refresh_token = refresh_token.trim();
    if refresh_token.is_empty() {
        bail!("{}", gcal_missing_cred_msg());
    }
    if refresh_token.len() < 8 {
        bail!("Google 日曆 Refresh Token 過短 — 請至「設定」分頁重新填寫（不會顯示憑證內容）");
    }
    Ok(refresh_token)
}

fn validate_summary(summary: &str) -> Result<&str> {
    let summary = summary.trim();
    if summary.is_empty() {
        bail!("日曆行程標題為空 — 無法建立行程");
    }
    Ok(summary)
}

fn token_http_error_zh(status: u16) -> String {
    match status {
        400 | 401 => {
            "Google 日曆權杖更新失敗 — 請至「設定」分頁檢查 Client ID / Secret / Refresh Token"
                .to_string()
        }
        403 => {
            "Google 日曆權杖更新被拒絕（權限不足）— 請至「設定」分頁檢查 OAuth 範圍與 Refresh Token"
                .to_string()
        }
        _ => format!("Google 日曆權杖更新失敗（HTTP {status}）— 請至「設定」分頁檢查憑證後再試"),
    }
}

fn insert_http_error_zh(status: u16) -> String {
    match status {
        401 | 403 => "Google 日曆建立行程被拒絕 — 請至「設定」分頁檢查權杖與日曆權限".to_string(),
        404 => "Google 日曆找不到指定日曆 — 請至「設定」分頁檢查 Calendar ID".to_string(),
        _ => format!("Google 日曆建立行程失敗（HTTP {status}）"),
    }
}

fn resolve_event_window(
    start_rfc3339: &str,
    end_rfc3339_or_plus1h: &str,
    now: DateTime<Utc>,
    description: &mut String,
) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let start = if start_rfc3339.is_empty() {
        let tentative = now + Duration::hours(24);
        let note = "[Pixel agent] 未指定開始時間，暫定為 24 小時後；請在 Google Calendar 調整。";
        if description.is_empty() {
            description.push_str(note);
        } else {
            description.push_str("\n\n");
            description.push_str(note);
        }
        tentative
    } else {
        DateTime::parse_from_rfc3339(start_rfc3339)
            .map_err(|_| anyhow::anyhow!("無法解析開始時間"))?
            .with_timezone(&Utc)
    };

    let end = if end_rfc3339_or_plus1h.is_empty() {
        start + Duration::hours(1)
    } else {
        DateTime::parse_from_rfc3339(end_rfc3339_or_plus1h)
            .map_err(|_| anyhow::anyhow!("無法解析結束時間"))?
            .with_timezone(&Utc)
    };

    if end <= start {
        bail!("行程結束時間必須晚於開始時間");
    }

    Ok((start, end))
}

fn urlencoding_path(segment: &str) -> String {
    segment
        .chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u8),
        })
        .collect()
}

fn taipei_offset() -> FixedOffset {
    FixedOffset::east_opt(8 * 3600).expect("UTC+8")
}

/// Best-effort parse of Chinese datetime hints (Asia/Taipei local semantics).
///
/// Returns `(start, end)` as UTC; end is start + 1 hour when no duration is given.
/// Returns `None` when the hint is empty or too ambiguous.
pub fn parse_datetime_hint(
    hint: &str,
    now: DateTime<Utc>,
) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let raw = hint.trim();
    if raw.is_empty() {
        return None;
    }

    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        let start = dt.with_timezone(&Utc);
        return Some((start, start + Duration::hours(1)));
    }

    let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    let local_now = now.with_timezone(&taipei_offset());
    let today = local_now.date_naive();

    if let Some((weekday, rest)) = parse_next_week_hint(&compact) {
        let date = weekday_in_next_week(today, weekday);
        let (hour, minute) = parse_time_of_day(rest).unwrap_or((9, 0));
        let start = taipei_local_to_utc(date, NaiveTime::from_hms_opt(hour, minute, 0)?)?;
        return Some((start, start + Duration::hours(1)));
    }

    let (day_offset, rest) = parse_day_offset(&compact).unwrap_or((0, compact));
    let target_date = today + Duration::days(day_offset);

    if let Some(weekday) = parse_weekday_only(&rest) {
        let date = next_weekday_on_or_after(target_date, weekday);
        let (hour, minute) = parse_time_of_day("").unwrap_or((9, 0));
        let start = taipei_local_to_utc(date, NaiveTime::from_hms_opt(hour, minute, 0)?)?;
        return Some((start, start + Duration::hours(1)));
    }

    let (hour, minute) = parse_time_of_day(&rest)?;
    let start = taipei_local_to_utc(target_date, NaiveTime::from_hms_opt(hour, minute, 0)?)?;
    Some((start, start + Duration::hours(1)))
}

fn parse_next_week_hint(s: &str) -> Option<(Weekday, &str)> {
    const PATTERNS: &[(&str, Weekday)] = &[
        ("下週一", Weekday::Mon),
        ("下週二", Weekday::Tue),
        ("下週三", Weekday::Wed),
        ("下週四", Weekday::Thu),
        ("下週五", Weekday::Fri),
        ("下週六", Weekday::Sat),
        ("下週日", Weekday::Sun),
        ("下周一", Weekday::Mon),
        ("下周二", Weekday::Tue),
        ("下周三", Weekday::Wed),
        ("下周四", Weekday::Thu),
        ("下周五", Weekday::Fri),
        ("下周六", Weekday::Sat),
        ("下周日", Weekday::Sun),
        ("下星期一", Weekday::Mon),
        ("下星期二", Weekday::Tue),
        ("下星期三", Weekday::Wed),
        ("下星期四", Weekday::Thu),
        ("下星期五", Weekday::Fri),
        ("下星期六", Weekday::Sat),
        ("下星期日", Weekday::Sun),
        ("下星期天", Weekday::Sun),
    ];
    for (pat, wd) in PATTERNS {
        if let Some(rest) = s.strip_prefix(pat) {
            return Some((*wd, rest));
        }
    }
    None
}

fn weekday_in_next_week(today: NaiveDate, target: Weekday) -> NaiveDate {
    let days_since_mon = today.weekday().num_days_from_monday();
    let this_monday = today - Duration::days(days_since_mon as i64);
    let next_monday = this_monday + Duration::days(7);
    next_monday + Duration::days(target.num_days_from_monday() as i64)
}

fn parse_day_offset(s: &str) -> Option<(i64, String)> {
    let pairs: &[(&str, i64)] = &[
        ("大後天", 3),
        ("大后天", 3),
        ("後天", 2),
        ("后天", 2),
        ("明天", 1),
        ("明日", 1),
        ("今天", 0),
        ("今日", 0),
    ];
    for (word, off) in pairs {
        if let Some(rest) = s.strip_prefix(word) {
            return Some((*off, rest.to_string()));
        }
    }
    None
}

fn parse_weekday_only(s: &str) -> Option<Weekday> {
    let t = s.trim_end_matches('。').trim_end_matches('.');
    if t.contains('點') || t.contains('点') || t.contains('時') || t.contains('时') {
        return None;
    }
    parse_weekday_token(t)
}

fn parse_weekday_token(s: &str) -> Option<Weekday> {
    let table: &[(&str, Weekday)] = &[
        ("星期一", Weekday::Mon),
        ("周一", Weekday::Mon),
        ("週一", Weekday::Mon),
        ("星期二", Weekday::Tue),
        ("周二", Weekday::Tue),
        ("週二", Weekday::Tue),
        ("星期三", Weekday::Wed),
        ("周三", Weekday::Wed),
        ("週三", Weekday::Wed),
        ("星期四", Weekday::Thu),
        ("周四", Weekday::Thu),
        ("週四", Weekday::Thu),
        ("星期五", Weekday::Fri),
        ("周五", Weekday::Fri),
        ("週五", Weekday::Fri),
        ("星期六", Weekday::Sat),
        ("周六", Weekday::Sat),
        ("週六", Weekday::Sat),
        ("星期日", Weekday::Sun),
        ("星期天", Weekday::Sun),
        ("周日", Weekday::Sun),
        ("週日", Weekday::Sun),
    ];
    for (token, wd) in table {
        if s == *token {
            return Some(*wd);
        }
    }
    None
}

fn next_weekday_on_or_after(base: NaiveDate, target: Weekday) -> NaiveDate {
    let mut date = base;
    for _ in 0..14 {
        if date.weekday() == target {
            return date;
        }
        date += Duration::days(1);
    }
    date
}

fn parse_time_of_day(s: &str) -> Option<(u32, u32)> {
    let s = s.trim_end_matches('。').trim_end_matches('.');
    if s.is_empty() {
        return None;
    }

    if parse_weekday_token(s).is_some() {
        return None;
    }

    let (period, num_part) =
        if let Some(rest) = s.strip_prefix("下午").or_else(|| s.strip_prefix("晚上")) {
            (Period::Pm, rest)
        } else if let Some(rest) = s
            .strip_prefix("上午")
            .or_else(|| s.strip_prefix("早上"))
            .or_else(|| s.strip_prefix("中午"))
        {
            (Period::Am, rest)
        } else {
            (Period::Neutral, s)
        };

    let (hour_s, minute) = split_hour_minute(num_part)?;
    let hour = parse_hour_number(hour_s, period)?;
    Some((hour, minute))
}

#[derive(Clone, Copy)]
enum Period {
    Am,
    Pm,
    Neutral,
}

fn split_hour_minute(s: &str) -> Option<(&str, u32)> {
    let s = s
        .trim_end_matches('點')
        .trim_end_matches('点')
        .trim_end_matches('時')
        .trim_end_matches('时');
    if let Some((h, m)) = s
        .split_once('點')
        .or_else(|| s.split_once('点'))
        .or_else(|| s.split_once(':'))
    {
        let minute = m
            .trim()
            .trim_end_matches('分')
            .trim_end_matches("半")
            .parse()
            .ok()
            .or(if m.contains('半') { Some(30) } else { None })?;
        return Some((h.trim(), minute));
    }
    if s.ends_with('半') {
        let h = s.trim_end_matches('半');
        return Some((h, 30));
    }
    Some((s, 0))
}

fn parse_hour_number(raw: &str, period: Period) -> Option<u32> {
    let hour = if let Ok(n) = raw.parse::<u32>() {
        n
    } else {
        zh_digit(raw)?
    };

    match period {
        Period::Am => {
            if hour == 12 {
                Some(0)
            } else {
                Some(hour.min(11))
            }
        }
        Period::Pm => {
            if hour == 12 {
                Some(12)
            } else if (1..=11).contains(&hour) {
                Some(hour + 12)
            } else {
                None
            }
        }
        Period::Neutral => {
            if (0..=23).contains(&hour) {
                Some(hour)
            } else {
                None
            }
        }
    }
}

fn zh_digit(s: &str) -> Option<u32> {
    match s {
        "零" | "〇" => Some(0),
        "一" => Some(1),
        "二" | "兩" | "两" => Some(2),
        "三" => Some(3),
        "四" => Some(4),
        "五" => Some(5),
        "六" => Some(6),
        "七" => Some(7),
        "八" => Some(8),
        "九" => Some(9),
        "十" => Some(10),
        "十一" => Some(11),
        "十二" => Some(12),
        _ => s.parse().ok(),
    }
}

fn taipei_local_to_utc(date: NaiveDate, time: NaiveTime) -> Option<DateTime<Utc>> {
    let tz = taipei_offset();
    let naive = date.and_time(time);
    tz.from_local_datetime(&naive)
        .single()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    #[test]
    fn parse_tomorrow_afternoon() {
        let now = utc(2026, 8, 3, 4, 0); // Mon 2026-08-03 12:00 Taipei
        let (start, end) = parse_datetime_hint("明天下午2點", now).unwrap();
        assert_eq!(start, utc(2026, 8, 4, 6, 0)); // 14:00 Taipei
        assert_eq!(end - start, Duration::hours(1));
    }

    #[test]
    fn parse_next_monday() {
        let now = utc(2026, 8, 3, 4, 0); // Mon
        let (start, _) = parse_datetime_hint("下週一", now).unwrap();
        assert_eq!(start, utc(2026, 8, 10, 1, 0)); // next Mon 09:00 Taipei
    }

    #[test]
    fn validation_rejects_empty_client_id() {
        let err = validate_client_id("  ").unwrap_err();
        assert!(err.to_string().contains("憑證未設定"));
    }

    #[test]
    fn validation_rejects_short_secret() {
        let err = validate_client_secret("short").unwrap_err();
        assert!(err.to_string().contains("過短"));
        assert!(!err.to_string().contains("short"));
    }

    #[test]
    fn resolve_calendar_id_defaults_primary() {
        assert_eq!(resolve_calendar_id(""), "primary");
        assert_eq!(resolve_calendar_id("  "), "primary");
        assert_eq!(
            resolve_calendar_id("work@group.calendar.google.com"),
            "work@group.calendar.google.com"
        );
    }

    #[test]
    fn end_must_be_after_start_zh() {
        let now = utc(2026, 8, 3, 4, 0);
        let mut desc = String::new();
        let err = resolve_event_window(
            "2026-08-04T02:00:00Z",
            "2026-08-04T01:00:00Z",
            now,
            &mut desc,
        )
        .unwrap_err();
        assert!(err.to_string().contains("結束時間必須晚於"));
    }

    #[test]
    fn freebusy_request_and_answers() {
        let req = freebusy_request("", "2026-11-14T15:00:00+08:00", "2026-11-14T16:00:00+08:00");
        assert_eq!(
            req,
            json!({"timeMin": "2026-11-14T15:00:00+08:00", "timeMax": "2026-11-14T16:00:00+08:00", "items": [{"id": "primary"}]})
        );
        assert_eq!(freebusy_request("work@group.calendar.google.com", "a", "b")["items"][0]["id"], "work@group.calendar.google.com");
        let free = json!({"kind": "calendar#freeBusy", "calendars": {"primary": {"busy": []}}});
        assert!(parse_freebusy(&free, "").unwrap());
        let busy = json!({"calendars": {"primary": {"busy": [{"start": "2026-11-14T07:00:00Z", "end": "2026-11-14T08:00:00Z"}]}}});
        assert!(!parse_freebusy(&busy, " ").unwrap());
        // The answer may key the calendar by its address instead of "primary".
        let keyed = json!({"calendars": {"me@example.com": {"busy": []}}});
        assert!(parse_freebusy(&keyed, "").unwrap());
        let missing = json!({"calendars": {"notFound@x": {"errors": [{"domain": "global", "reason": "notFound"}]}}});
        let err = parse_freebusy(&missing, "notFound@x").unwrap_err().to_string();
        assert!(err.contains("notFound") && err.contains("Calendar ID"), "{err}");
        assert!(parse_freebusy(&json!({"error": {"code": 400}}), "").is_err());
        let scope = freebusy_http_error_zh(403);
        assert!(scope.contains("授權範圍不足") && scope.contains("calendar.freebusy"), "{scope}");
        assert!(freebusy_http_error_zh(500).contains("HTTP 500"));
    }
}
