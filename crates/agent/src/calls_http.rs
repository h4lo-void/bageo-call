//! LAN web console: call history + runtime settings + tasks (`/calls`).

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write as _;
use std::sync::{Arc, OnceLock};
use tokio::sync::oneshot;
use tracing::info;

use crate::callback::{ControlRegistry, WsSink};
use crate::calls::{CallListFilter, CallStore};
use crate::config::Settings;
use crate::fulfill::{on_confirm, on_ignore};
use crate::settings_store::SettingsStore;
use crate::tasks::TaskStore;

#[derive(Clone)]
pub struct CallsState {
    pub settings: Settings,
    pub store: Arc<CallStore>,
    pub settings_store: Arc<SettingsStore>,
    pub task_store: Arc<TaskStore>,
    pub http: reqwest::Client,
    pub control: Arc<ControlRegistry>,
    /// Background sound beds (same ones calls use) for the settings page's preview.
    pub ambience: Arc<crate::ambience::AmbienceLibrary>,
    pub previews: Arc<crate::voice_preview::VoicePreviews>,
    /// Phones on the control connection: mode, app version, `set_mode` round trips.
    pub phones: Arc<PhoneDevices>,
    /// The owner's contacts: names for the call list, and the contacts API.
    pub contacts: Arc<crate::contacts::ContactStore>,
    /// Calls the owner has the AI make ([`crate::errands`]), and their API.
    pub errands: Arc<crate::errands::ErrandStore>,
}

const UI_HTML: &str = include_str!("../static/calls.html");
// "Add to home screen": the app manifest and its icons (the bird of docs/images/bageo.svg).
const MANIFEST: &[u8] = include_bytes!("../static/manifest.webmanifest");
const ICON_192: &[u8] = include_bytes!("../static/icon-192.png");
const ICON_512: &[u8] = include_bytes!("../static/icon-512.png");
const APPLE_TOUCH_ICON: &[u8] = include_bytes!("../static/apple-touch-icon.png");
const FAVICON: &[u8] = include_bytes!("../static/favicon.ico");

/// One embedded file. Public like the page itself: nothing private in them.
fn static_file(bytes: &'static [u8], content_type: &'static str) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "public, max-age=86400")
        .body(Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// `/favicon.ico` (also at the site root, where browsers look for it).
pub async fn favicon() -> Response {
    static_file(FAVICON, "image/x-icon")
}

/// Embedded UI: raw bytes, gzip bytes and a strong ETag, computed once.
struct UiAsset {
    gzip: Vec<u8>,
    etag: String,
}

fn ui_asset() -> &'static UiAsset {
    static ASSET: OnceLock<UiAsset> = OnceLock::new();
    ASSET.get_or_init(|| {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        let _ = enc.write_all(UI_HTML.as_bytes());
        let gzip = enc.finish().unwrap_or_default();
        use sha2::Digest;
        let digest = sha2::Sha256::digest(UI_HTML.as_bytes());
        let etag = format!(
            "\"{}\"",
            digest.iter().take(8).map(|b| format!("{b:02x}")).collect::<String>()
        );
        UiAsset { gzip, etag }
    })
}

fn accepts_gzip(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(',').any(|p| p.trim().starts_with("gzip")))
        .unwrap_or(false)
}

#[derive(Deserialize)]
pub struct TokenQuery {
    pub token: Option<String>,
}

/// An `.ics` download: the console token, or the key a link outside the
/// console carries instead ([`crate::calendar::link_key`]).
#[derive(Deserialize)]
pub struct IcsQuery {
    pub token: Option<String>,
    pub k: Option<String>,
}

/// The console token opens it, or the key made for this one file (`what` =
/// its path under `/calls/api/`).
fn ics_access_ok(settings: &Settings, headers: &HeaderMap, query: &IcsQuery, what: &str) -> bool {
    token_ok_parts(settings, headers, query.token.as_deref())
        || query
            .k
            .as_deref()
            .is_some_and(|k| crate::calendar::key_opens(&settings.calls_ui_token, what, k))
}

/// An event as an `.ics` file download.
fn ics_response(ics: String) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/calendar; charset=utf-8")
        .header(header::CONTENT_DISPOSITION, "attachment; filename=\"call-event.ics\"")
        .body(Body::from(ics))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Extends auth token with optional list filters (all additive / backward compatible).
/// Sorted newest-first (`sort=started_at_desc` in the JSON response).
#[derive(Deserialize)]
pub struct ListQuery {
    pub token: Option<String>,
    /// Exact status (`ended`, `blocked_spam`, `ringing`, ...).
    pub status: Option<String>,
    /// Substring match on device serial.
    pub serial: Option<String>,
    /// Substring match on caller_id.
    pub caller: Option<String>,
    /// Free-text: caller_id, serial, id, events, reply_contact, task transcript/summary.
    pub q: Option<String>,
    /// `true` / `false` - only rows with / without voicemail.
    pub has_voicemail: Option<bool>,
    /// `true` / `false` - only rows with / without a non-empty task transcript.
    pub has_transcript: Option<bool>,
    /// Page size. Omit for full list (legacy clients).
    pub limit: Option<usize>,
    /// Skip N matches (default 0).
    pub offset: Option<usize>,
}

impl ListQuery {
    fn as_token(&self) -> TokenQuery {
        TokenQuery {
            token: self.token.clone(),
        }
    }

    fn to_filter(&self) -> CallListFilter {
        CallListFilter {
            status: self.status.clone(),
            serial: self.serial.clone(),
            caller: self.caller.clone(),
            q: self.q.clone(),
            has_voicemail: self.has_voicemail,
            has_transcript: self.has_transcript,
            limit: self.limit.map(|n| n.min(500)),
            offset: self.offset,
        }
    }

    fn needs_transcript_index(&self) -> bool {
        self.has_transcript.is_some()
            || self
                .q
                .as_deref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
    }
}

/// call_id -> searchable text (transcript + summary; multi-task calls are merged).
async fn transcript_index(task_store: &TaskStore) -> HashMap<String, String> {
    // limit=0 -> all tasks (TaskStore newest-first).
    let tasks = task_store.list(0).await;
    let mut map: HashMap<String, String> = HashMap::new();
    for t in tasks {
        if t.call_id.is_empty() {
            continue;
        }
        let mut parts: Vec<&str> = Vec::new();
        let tx = t.transcript.trim();
        let sum = t.summary.trim();
        if !tx.is_empty() {
            parts.push(tx);
        }
        if !sum.is_empty() && sum != tx {
            parts.push(sum);
        }
        if parts.is_empty() {
            continue;
        }
        let chunk = parts.join("\n");
        map.entry(t.call_id)
            .and_modify(|existing| {
                if !existing.contains(&chunk) {
                    existing.push('\n');
                    existing.push_str(&chunk);
                }
            })
            .or_insert(chunk);
    }
    map
}

fn token_ok(settings: &Settings, headers: &HeaderMap, query: &TokenQuery) -> bool {
    token_ok_parts(settings, headers, query.token.as_deref())
}

/// The request carries the console token (query, bearer or `x-calls-token`),
/// or none is set.
pub fn token_ok_parts(settings: &Settings, headers: &HeaderMap, query_token: Option<&str>) -> bool {
    let expected = settings.calls_ui_token.trim();
    if expected.is_empty() {
        return true;
    }
    if query_token == Some(expected) {
        return true;
    }
    if let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(rest) = auth.strip_prefix("Bearer ") {
            if rest == expected {
                return true;
            }
        }
    }
    if let Some(t) = headers.get("x-calls-token").and_then(|v| v.to_str().ok()) {
        if t == expected {
            return true;
        }
    }
    false
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": "unauthorized", "code": "unauthorized", "hint": "pass ?token= or Authorization: Bearer"})),
    )
        .into_response()
}

/// An error for the web console: `code` is what the page translates
/// (`err_<code>` in its dictionary); `error` keeps the agent's own words for
/// the tooltip and for scripts.
fn api_error(status: StatusCode, code: &str, error: impl Into<String>) -> Response {
    let error: String = error.into();
    (status, Json(json!({"ok": false, "code": code, "error": error}))).into_response()
}

/// A notification channel that failed: the code and the field at fault
/// ([`crate::notify::ChannelError`]), `send_failed` when it is not known.
fn channel_error_body(kind: &str, e: &anyhow::Error) -> Value {
    let (code, field) = crate::notify::channel_error(e)
        .map(|c| (c.code, c.field))
        .unwrap_or(("send_failed", ""));
    json!({"ok": false, "code": code, "field": field, "kind": kind, "error": format!("{e:#}")})
}

fn channel_error_response(status: StatusCode, kind: &str, e: &anyhow::Error) -> Response {
    (status, Json(channel_error_body(kind, e))).into_response()
}

fn task_id_safe(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn fulfill_err_response(e: anyhow::Error) -> Response {
    let msg = format!("{e:#}");
    let lower = msg.to_ascii_lowercase();
    if lower.contains("not found") {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "task not found", "code": "task_not_found", "detail": msg})),
        )
            .into_response();
    }
    api_error(StatusCode::INTERNAL_SERVER_ERROR, "fulfill_failed", format!("fulfill failed: {msg}"))
}

async fn ui(headers: HeaderMap) -> Response {
    // The page itself holds nothing private and is served to anyone: it asks
    // for the console token when the API answers 401 (a link from a
    // notification has no token in it).
    // `CALLS_UI_FILE`: serve the page from disk on every request (working on the
    // page without rebuilding, or a page of your own).
    if let Ok(path) = std::env::var("CALLS_UI_FILE") {
        if !path.trim().is_empty() {
            return match tokio::fs::read(path.trim()).await {
                Ok(bytes) => Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
                    .header(header::CACHE_CONTROL, "no-store")
                    .body(Body::from(bytes))
                    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("CALLS_UI_FILE {}: {e}", path.trim()),
                )
                    .into_response(),
            };
        }
    }
    let asset = ui_asset();
    let not_modified = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(',').any(|t| t.trim() == asset.etag))
        .unwrap_or(false);
    let builder = Response::builder()
        .header(header::ETAG, asset.etag.as_str())
        // Revalidate every load (cheap 304) so a rebuilt agent ships the new UI at once.
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::VARY, "Accept-Encoding");
    if not_modified {
        return builder
            .status(StatusCode::NOT_MODIFIED)
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    if accepts_gzip(&headers) && !asset.gzip.is_empty() {
        return builder
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(header::CONTENT_ENCODING, "gzip")
            .body(Body::from(asset.gzip.clone()))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    builder
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(UI_HTML))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn list(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query.as_token()) {
        return unauthorized();
    }
    let filter = query.to_filter();
    // Transcript index only when `q` or `has_transcript` needs it (avoids task IO otherwise).
    let transcript_map = if query.needs_transcript_index() {
        Some(transcript_index(&state.task_store).await)
    } else {
        None
    };
    let page = state
        .store
        .list_filtered(&filter, transcript_map.as_ref())
        .await;
    // Tasks for just this page (newest task per call) so the UI never has to pull
    // the whole task list to badge rows.
    let tasks_by_call = {
        let want: std::collections::HashSet<&str> =
            page.calls.iter().map(|c| c.id.as_str()).collect();
        let mut out = serde_json::Map::new();
        if !want.is_empty() {
            for t in state.task_store.list(0).await {
                if want.contains(t.call_id.as_str()) && !out.contains_key(&t.call_id) {
                    out.insert(
                        t.call_id.clone(),
                        json!({"id": t.id, "status": t.status, "summary": t.summary}),
                    );
                }
            }
        }
        Value::Object(out)
    };
    // Names for the numbers that are contacts (the list shows them).
    let contacts_by_number = crate::contacts::names_for(&state.contacts, &page.calls);
    Json(json!({
        "ok": true,
        "note": "Default page player is /listen (system audio + caller, ducked). _remote.wav = caller whole call; _tts.wav = system only; _vm.wav = voicemail slice.",
        "sort": "started_at_desc",
        "calls": page.calls,
        "total": page.total,
        "limit": page.limit,
        "offset": page.offset,
        "tasks_by_call": tasks_by_call,
        "contacts_by_number": contacts_by_number,
    }))
    .into_response()
}

async fn stats(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    let s = state.store.stats().await;
    Json(json!({
        "ok": true,
        "stats": {
            "total": s.total,
            "today": s.today,
            "voicemails": s.voicemails,
            "blocked": s.blocked,
        }
    }))
    .into_response()
}

/// Live in-progress calls (`ringing` / `active`) for the console indicator.
async fn live(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    let calls = state.store.live_calls().await;
    let control_serials = state.control.connected_serials().await;
    Json(json!({
        "ok": true,
        "count": calls.len(),
        "calls": calls,
        "control_idle_clients": control_serials.len(),
        "control_idle_serials": control_serials,
        // Connected phones switched to "normal phone": the page does not count them as
        // ready for calls.
        "normal_mode_serials": state.phones.normal_mode_serials(),
    }))
    .into_response()
}

/// Full call metadata JSON (export / deep link).
async fn call_get(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    Path(id): Path<String>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    if !task_id_safe(&id) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_id", "invalid call id");
    }
    let Some(rec) = state.store.get(&id).await else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "not found");
    };
    // Post-call summary + full dialogue (null until the summary task wrote it).
    let notes = state.store.load_notes(&id).await;
    // The filled-in Google Calendar form for the call's calendar suggestion
    // (kept for older pages), and every web calendar's form ("Add to calendar").
    let calendar_link = notes
        .as_ref()
        .and_then(|n| n.calendar.as_ref().map(|ev| ev.google_link(&n.summary)));
    // The caller's contact, when the number is one (null otherwise).
    let contact = state
        .contacts
        .lookup(&rec.caller_id, rec.sim_country.as_deref())
        .map(|c| json!({"id": c.id, "name": c.name, "note": c.note}));
    let calendar_links = notes
        .as_ref()
        .and_then(|n| n.calendar.as_ref().map(|ev| ev.links(&n.summary)));
    Json(json!({
        "ok": true,
        "call": rec,
        "notes": notes,
        "calendar_link": calendar_link,
        "contact": contact,
        "calendar_links": calendar_links,
    }))
    .into_response()
}

/// The call's calendar suggestion as an `.ics` file (any calendar app opens it;
/// iPhone Safari offers to add it to Apple Calendar).
async fn call_event_ics(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<IcsQuery>,
    Path(id): Path<String>,
) -> Response {
    if !task_id_safe(&id) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_id", "invalid call id");
    }
    if !ics_access_ok(&state.settings, &headers, &query, &format!("call/{id}")) {
        return unauthorized();
    }
    let Some(notes) = state.store.load_notes(&id).await else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "not found");
    };
    let Some(event) = notes.calendar else {
        return api_error(StatusCode::NOT_FOUND, "no_calendar", "this call has no calendar suggestion");
    };
    ics_response(event.ics(&id, &notes.summary, chrono::Utc::now().naive_utc()))
}

/// An appointment task's event as an `.ics` file: "Add to calendar" for
/// Apple Calendar or another app, also from a notification or a Telegram reply.
async fn task_event_ics(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<IcsQuery>,
    Path(id): Path<String>,
) -> Response {
    if !task_id_safe(&id) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_id", "invalid task id");
    }
    if !ics_access_ok(&state.settings, &headers, &query, &format!("tasks/{id}")) {
        return unauthorized();
    }
    let Some((task, appt)) = state
        .task_store
        .get(&id)
        .await
        .and_then(|t| t.appointment.clone().map(|a| (t, a)))
    else {
        return api_error(StatusCode::NOT_FOUND, "no_calendar", "this task has no appointment");
    };
    let details = crate::appointments::event_description(&task.caller_id, &appt);
    ics_response(appt.calendar_event().ics(&id, &details, chrono::Utc::now().naive_utc()))
}

/// One task for the web console: as stored, and for an appointment the web
/// calendars' forms (`calendar_links`) and how its answer can reach the caller
/// (`reply`: `{sms, call, default}`, [`crate::appointments::default_reply`]).
/// Also the phone and SIM a test call's text goes out from (`sms_route`,
/// [`crate::appointments::task_json`]).
fn task_json(task: &crate::tasks::TaskRecord, rt: &crate::settings_store::RuntimeSettings, connected: &[String]) -> Value {
    let mut v = crate::appointments::task_json(rt, connected, task);
    if let (Some(appt), Some(o)) = (task.appointment.as_ref(), v.as_object_mut()) {
        use crate::appointments as ap;
        let details = ap::event_description(&task.caller_id, appt);
        o.insert("calendar_links".into(), json!(appt.calendar_event().links(&details)));
        let region = appt
            .sim_country
            .clone()
            .or_else(|| crate::phone_region::last_sim(&task.serial));
        let opts = ap::reply_options(rt, &task.caller_id, region.as_deref());
        o.insert(
            "reply".into(),
            json!({ "sms": opts.sms, "call": opts.call, "default": ap::default_reply(&rt.appointment_reply, opts).as_str() }),
        );
        // The caller as the owner saved them (the to-do list shows the name).
        if let Some(c) = crate::contacts::lookup(&task.caller_id, region.as_deref()) {
            o.insert("contact_name".into(), json!(c.name));
        }
    }
    v
}

/// The body of a decision (`POST /api/tasks/{id}/confirm|ignore`): empty, `{}`
/// or `{"reply": "sms" | "call" | "none"}`, how the caller of an appointment is
/// told (none given: the default). Err: not that shape (`bad_request`).
pub fn parse_decision_body(body: &[u8]) -> Result<Option<crate::appointments::Reply>, &'static str> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let v: Value = serde_json::from_slice(body).map_err(|_| "bad_request")?;
    let obj = v.as_object().ok_or("bad_request")?;
    match obj.get("reply") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => crate::appointments::Reply::parse(s).map(Some).ok_or("bad_request"),
        Some(_) => Err("bad_request"),
    }
}

/// CSV export of the filtered call list (same query params as `/api/list`).
/// Ignores UI pagination: returns up to 2000 matching rows (newest first).
async fn export_csv(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query.as_token()) {
        return unauthorized();
    }
    let mut filter = query.to_filter();
    filter.offset = Some(0);
    filter.limit = Some(query.limit.map(|n| n.min(2000)).unwrap_or(2000));
    let transcript_map = if query.needs_transcript_index() {
        Some(transcript_index(&state.task_store).await)
    } else {
        None
    };
    let page = state
        .store
        .list_filtered(&filter, transcript_map.as_ref())
        .await;
    let mut out = String::with_capacity(256 + page.calls.len() * 160);
    out.push_str(
        "id,started_at,ended_at,serial,caller_id,duration_ms,status,voicemail,vm_duration_ms,reply_method,reply_contact,contact_verified,audio,remote,uplink_bytes\n",
    );
    for c in &page.calls {
        out.push_str(&csv_row(&[
            &c.id,
            &c.started_at,
            c.ended_at.as_deref().unwrap_or(""),
            &c.serial,
            &c.caller_id,
            &c.duration_ms.to_string(),
            &c.status,
            if c.voicemail_path.is_some() { "1" } else { "0" },
            &c.vm_duration_ms.to_string(),
            c.reply_method.as_deref().unwrap_or(""),
            c.reply_contact.as_deref().unwrap_or(""),
            if c.contact_verified { "1" } else { "0" },
            if c.audio_path.is_some() { "1" } else { "0" },
            if c.remote_audio_path.is_some() {
                "1"
            } else {
                "0"
            },
            &c.uplink_bytes.to_string(),
        ]));
    }
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/csv; charset=utf-8")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"calls-export.csv\"",
        )
        .header(header::CACHE_CONTROL, "private, no-store")
        .header("X-Calls-Export-Total", page.total.to_string())
        .header("X-Calls-Export-Rows", page.calls.len().to_string())
        .body(Body::from(out))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn csv_row(fields: &[&str]) -> String {
    let mut line = String::new();
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            line.push(',');
        }
        line.push_str(&csv_escape(f));
    }
    line.push('\n');
    line
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

async fn audio(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    Path(id): Path<String>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    let Some(rec) = state.store.get(&id).await else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "not found");
    };
    let Some(rel) = rec.audio_path.as_deref() else {
        return api_error(StatusCode::NOT_FOUND, "no_audio", "no audio");
    };
    stream_wav(&state.store, rel, &headers).await
}

/// Page-listen WAV: ducked mix rebuilt from remote+tts (works for old additive files).
async fn audio_listen(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    Path(id): Path<String>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    let Some(rec) = state.store.get(&id).await else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "not found");
    };
    let store = state.store.clone();
    let built = tokio::task::spawn_blocking(move || store.listen_wav_cached(&rec))
        .await
        .ok()
        .flatten();
    match built {
        Some(bytes) => ranged_response(&headers, bytes),
        None => api_error(StatusCode::NOT_FOUND, "no_audio", "no listen audio"),
    }
}

/// Parse a single `Range: bytes=a-b` header against `len`. `None` = no/invalid range.
fn parse_range(headers: &HeaderMap, len: u64) -> Option<(u64, u64)> {
    let v = headers.get(header::RANGE)?.to_str().ok()?.trim();
    let spec = v.strip_prefix("bytes=")?;
    if spec.contains(',') || len == 0 {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (start, end) = if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        (len.saturating_sub(n), len - 1)
    } else {
        let s: u64 = a.parse().ok()?;
        let e = if b.is_empty() { len - 1 } else { b.parse::<u64>().ok()?.min(len - 1) };
        (s, e)
    };
    (start <= end && start < len).then_some((start, end))
}

/// Serve in-memory WAV bytes with `Accept-Ranges` / 206 support so browser
/// seeking works (Safari/iOS require it).
fn ranged_response(headers: &HeaderMap, bytes: bytes::Bytes) -> Response {
    let len = bytes.len() as u64;
    let base = Response::builder()
        .header(header::CONTENT_TYPE, "audio/wav")
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "private, max-age=300");
    let resp = match parse_range(headers, len) {
        Some((s, e)) => base
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, format!("bytes {s}-{e}/{len}"))
            .header(header::CONTENT_LENGTH, (e - s + 1).to_string())
            .body(Body::from(bytes.slice(s as usize..=e as usize))),
        None if headers.contains_key(header::RANGE) && len > 0 => base
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::CONTENT_RANGE, format!("bytes */{len}"))
            .body(Body::empty()),
        None => base
            .status(StatusCode::OK)
            .header(header::CONTENT_LENGTH, len.to_string())
            .body(Body::from(bytes)),
    };
    resp.unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// System-only WAV (`<id>_tts.wav`) - greeting / AI / goodbye playout.
async fn audio_tts(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    Path(id): Path<String>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    let Some(rec) = state.store.get(&id).await else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "not found");
    };
    let Some(rel) = rec.tts_audio_path.as_deref() else {
        return api_error(StatusCode::NOT_FOUND, "no_audio", "no tts audio");
    };
    stream_wav(&state.store, rel, &headers).await
}

/// Message-only voicemail WAV (`<id>_vm.wav`), mirrors the full-call handler.
async fn audio_vm(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    Path(id): Path<String>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    let Some(rec) = state.store.get(&id).await else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "not found");
    };
    let Some(rel) = rec.voicemail_path.as_deref() else {
        return api_error(StatusCode::NOT_FOUND, "no_audio", "no voicemail");
    };
    stream_wav(&state.store, rel, &headers).await
}

/// Whole-call remote/caller uplink WAV (`<id>_remote.wav`) - greeting/AI/VM/goodbye.
async fn audio_remote(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    Path(id): Path<String>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    let Some(rec) = state.store.get(&id).await else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "not found");
    };
    if rec.remote_audio_path.is_none() {
        return api_error(StatusCode::NOT_FOUND, "no_audio", "no remote audio");
    }
    // Echo-cleaned (old recordings still hold raw inject echo on disk).
    let store = state.store.clone();
    match tokio::task::spawn_blocking(move || store.remote_clean_cached(&rec))
        .await
        .ok()
        .flatten()
    {
        Some(bytes) => ranged_response(&headers, bytes),
        None => api_error(StatusCode::NOT_FOUND, "open_failed", "open failed"),
    }
}

#[derive(Deserialize)]
pub struct PreviewQuery {
    pub token: Option<String>,
    /// A Gemini voice, or `none` for the room alone. Empty = the saved setting.
    pub voice: Option<String>,
    /// `off` / `office` / `home` / `street`. Empty = the saved setting.
    pub ambience: Option<String>,
    /// 1 low / 2 medium / 3 high. Empty = the saved setting.
    pub level: Option<u32>,
    /// Make the voice's lines again instead of using the cached ones.
    pub regen: Option<bool>,
    /// `reject`: the blocked-number announcement in `voice` (made once, then cached).
    pub clip: Option<String>,
    /// Language of the lines (`ja`, `zh-TW`, …). Empty = the saved setting.
    pub lang: Option<String>,
}

/// Preview for the settings page: a few phone lines in `voice` with `ambience` at
/// `level` under them, as a caller hears it (form values, saved or not). A
/// voice's first preview runs a short Gemini Live session; later ones are cached.
async fn preview(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(q): Query<PreviewQuery>,
) -> Response {
    if !token_ok(&state.settings, &headers, &TokenQuery { token: q.token.clone() }) {
        return unauthorized();
    }
    let rt = state.settings_store.snapshot();
    let pick = |v: &Option<String>, saved: &str| {
        let v = v.as_deref().unwrap_or("").trim();
        if v.is_empty() { saved.trim().to_string() } else { v.to_string() }
    };
    let is_reject = q.clip.as_deref() == Some("reject");
    let saved_voice = if is_reject { crate::voice_preview::reject_voice(&rt) } else { rt.gemini_voice_name.as_str() };
    let mut voice = pick(&q.voice, saved_voice);
    if voice.is_empty() {
        voice = crate::gemini_live::default_voice_name().to_string();
    }
    let lang = match q.lang.as_deref().map(str::trim) {
        Some(l) if !l.is_empty() => crate::locale::Lang::parse(l),
        _ => rt.lang(),
    };
    let kind = pick(&q.ambience, &rt.ambience_kind);
    let level = q.level.unwrap_or(rt.ambience_level).clamp(1, 3);
    if voice != "none" && !crate::voice_preview::valid_voice(&voice) {
        return api_error(StatusCode::BAD_REQUEST, "unknown_voice", format!("unknown voice {voice}"));
    }
    if is_reject && voice != "none" {
        // What a blocked caller hears: the line alone, no room under it.
        let sr = state.settings.sample_rate.max(8000);
        if let Err(e) = state
            .previews
            .ensure_reject_clip(&rt.gemini_api_key, &rt.gemini_live_model, &rt.gemini_thinking_level, &voice, lang, sr)
            .await
        {
            tracing::warn!("reject announcement preview failed voice={voice}: {e:#}");
            return api_error(StatusCode::BAD_GATEWAY, "preview_failed", format!("{e:#}"));
        }
        let Some(pcm) = state.previews.cached_reject_clip(&rt.gemini_live_model, &voice, lang, sr) else {
            return api_error(StatusCode::BAD_GATEWAY, "preview_failed", "the announcement could not be made");
        };
        let mut padded = vec![0u8; (sr as usize / 2) * 2];
        padded.extend_from_slice(&pcm);
        padded.resize(padded.len() + (sr as usize / 2) * 2, 0);
        return ranged_response(&headers, bytes::Bytes::from(crate::calls::wav_from_pcm(&padded, sr, 1)));
    }
    if kind != "off" && !crate::ambience::valid_kind(&kind) {
        return api_error(StatusCode::BAD_REQUEST, "unknown_background", format!("unknown background sound {kind}"));
    }
    let sr = state.settings.sample_rate.max(8000);
    let lines = if voice == "none" {
        Vec::new()
    } else {
        match state
            .previews
            .lines(
                &rt.gemini_api_key,
                &rt.gemini_live_model,
                &rt.gemini_thinking_level,
                &voice,
                lang,
                sr,
                q.regen.unwrap_or(false),
            )
            .await
        {
            Ok(lines) => lines,
            Err(e) => {
                tracing::warn!("voice preview failed voice={voice}: {e:#}");
                return api_error(StatusCode::BAD_GATEWAY, "preview_failed", format!("{e:#}"));
            }
        }
    };
    let bed = if kind == "off" {
        None
    } else {
        match state.ambience.get(&kind) {
            Some(b) => Some((b, crate::ambience::level_gain(level), crate::ambience::preview_start_s(&kind))),
            None => {
                return api_error(
                    StatusCode::NOT_FOUND,
                    "background_missing",
                    format!("the sound file for background {kind} is not loaded"),
                )
            }
        }
    };
    let wav = tokio::task::spawn_blocking(move || {
        crate::calls::wav_from_pcm(&crate::voice_preview::assemble(&lines, bed, sr), sr, 1)
    })
    .await
    .unwrap_or_default();
    ranged_response(&headers, bytes::Bytes::from(wav))
}

/// Masked runtime settings (secrets show `*_set` flags, never raw values).
async fn settings_get(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    Json(json!({
        "ok": true,
        "settings": state.settings_store.masked_json(),
        "storage_path": state.settings_store.storage_path().display().to_string(),
        "console_url": console_url(&state),
        "console_ip_url": console_ip_url(&state),
    }))
    .into_response()
}

/// The console's address as other devices open it (empty: this computer only).
fn console_url(state: &CallsState) -> String {
    crate::settings_store::effective_console_url(&state.settings_store.snapshot().effective(&state.settings))
}

/// The same at the LAN IP, for phones that cannot open `.local` names.
fn console_ip_url(state: &CallsState) -> String {
    crate::settings_store::lan_console_url(&state.settings_store.snapshot().effective(&state.settings))
}

/// JSON patch of runtime settings. Secret fields: absent/"" keeps the stored
/// value; `<field>_clear: true` clears it (see settings_store module docs).
async fn settings_put(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    Json(patch): Json<Value>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    if !patch.is_object() {
        return api_error(StatusCode::BAD_REQUEST, "bad_request", "JSON object body required");
    }
    match state.settings_store.apply_patch(&patch) {
        Ok(rt) => {
            // Language, answer style, owner name: what the next call's helpers read.
            rt.apply_globals();
            // Blocked numbers / hidden numbers / voice / model changed: have the "cannot be
            // reached" announcement ready before the next such call (cached: free when it exists).
            if [
                "reject_numbers",
                "answer_unknown_callers",
                "reject_voice_name",
                "gemini_voice_name",
                "gemini_live_model",
                "gemini_api_key",
                "language",
            ]
            .iter()
            .any(|k| patch.get(*k).is_some())
            {
                let previews = state.previews.clone();
                let sr = state.settings.sample_rate;
                tokio::spawn(async move {
                    crate::voice_preview::prepare_reject_clip(&previews, &rt, sr).await;
                });
            }
            // Blacklist deny removed: always push empty so phones clear cached reject lists.
            if patch.get("spam_blocklist").is_some()
                || patch.get("spam_ai_filter_enabled").is_some()
            {
                let sent = state.control.broadcast_blocklist_sync(&[]).await;
                info!(
                    sent,
                    "blocklist_sync cleared after settings save (deny gating removed)"
                );
            }
            Json(json!({
                "ok": true,
                "settings": state.settings_store.masked_json(),
                "storage_path": state.settings_store.storage_path().display().to_string(),
                "console_url": console_url(&state),
                "console_ip_url": console_ip_url(&state),
            }))
            .into_response()
        }
        Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "save_failed", format!("save failed: {e:#}")),
    }
}

#[derive(Deserialize)]
pub struct NotifyTestBody {
    /// The row as it is on the page (saved or not). An empty address / token
    /// means the stored one of the same id.
    pub channel: crate::notify::Channel,
}

/// Send the test message to one channel of the settings page.
async fn notify_test(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    Json(body): Json<NotifyTestBody>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    let rt = state.settings_store.snapshot();
    let Some(channel) = crate::notify::merge_channels(&rt.notify_channels, vec![body.channel])
        .into_iter()
        .next()
    else {
        return api_error(StatusCode::BAD_REQUEST, "unknown_channel", "unknown channel kind");
    };
    match crate::notify::send_channel(&state.http, &channel, &crate::notify::test_message()).await {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => {
            tracing::warn!("notify test failed kind={}: {e:#}", channel.kind);
            channel_error_response(StatusCode::BAD_GATEWAY, &channel.kind, &e)
        }
    }
}

#[derive(Deserialize)]
pub struct NotifySendBody {
    #[serde(default)]
    pub title: String,
    pub body: String,
}

/// Send a message of your own to every notification channel (for scripts
/// around the agent, e.g. forwarding the phone's text messages).
async fn notify_send(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    Json(body): Json<NotifySendBody>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    if body.body.trim().is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "empty_body", "body is empty");
    }
    let eff = state.settings_store.snapshot().effective(&state.settings);
    let title = if body.title.trim().is_empty() { "Bageo" } else { body.title.trim() };
    let ok = crate::notify::notify_event(&state.http, &eff, title, &body.body).await;
    Json(json!({"ok": ok})).into_response()
}

/// Phones: what is plugged in (USB), what is connected to the agent, and the
/// models the connected ones reported.
async fn devices(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    let connected = state.control.connected_serials().await;
    let labels: serde_json::Map<String, Value> = crate::notify::known_devices()
        .into_iter()
        .map(|(id, label)| (id, json!(label)))
        .collect();
    let live = state.store.live_calls().await;
    let agent_calls: Vec<String> = live.iter().map(|c| c.serial.clone()).collect();
    Json(json!({
        "ok": true,
        "usb": crate::usb_bridge::status_json(),
        "connected": connected,
        "labels": labels,
        "calls_in_progress": live.len(),
        // One object per phone: mode, model, app_version, in_call, supports_mode.
        "phones": state.phones.list_json(&agent_calls),
    }))
    .into_response()
}
// ---------------------------------------------------------------- phone mode

/// How a phone handles calls: the AI answers them (`ai`), or it rings like any
/// phone and its owner answers (`normal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhoneMode {
    Ai,
    Normal,
}

impl PhoneMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ai => "ai",
            Self::Normal => "normal",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "ai" => Some(Self::Ai),
            "normal" => Some(Self::Normal),
            _ => None,
        }
    }
}

/// How long `POST /api/devices/{serial}/mode` waits for the phone's answer.
const SET_MODE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// What a phone says about itself on its control connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneHello {
    pub mode: PhoneMode,
    /// The app understands `set_mode` (it reported its mode). Older apps do not.
    pub supports_mode: bool,
    pub model: String,
    pub app_version: String,
    pub in_call: bool,
}

/// Read the control `hello`. The phone mode is in `phone_mode` (`mode` already says
/// which connection this is, `CONTROL_IDLE`). An app without it answers every call
/// with the AI and cannot be switched from here.
pub fn parse_phone_hello(hello: &Value) -> PhoneHello {
    let text = |key: &str| -> String {
        hello
            .get(key)
            .and_then(Value::as_str)
            .map(|s| s.trim().chars().filter(|c| !c.is_control()).take(40).collect())
            .unwrap_or_default()
    };
    let mode = hello
        .get("phone_mode")
        .and_then(Value::as_str)
        .and_then(PhoneMode::parse);
    PhoneHello {
        mode: mode.unwrap_or(PhoneMode::Ai),
        supports_mode: mode.is_some(),
        model: text("model"),
        app_version: text("app_version"),
        in_call: hello.get("in_call").and_then(Value::as_bool).unwrap_or(false),
    }
}

/// The outcome of a mode switch: the phone's `set_mode_result`, or why it was not sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeReply {
    pub ok: bool,
    /// The phone's mode afterwards, when known.
    pub mode: Option<PhoneMode>,
    /// Empty when `ok`.
    pub error: String,
}

impl ModeReply {
    fn failed(mode: Option<PhoneMode>, error: &str) -> Self {
        Self {
            ok: false,
            mode,
            error: error.to_string(),
        }
    }
}

/// An error code from the phone, kept short and plain (the page looks it up).
fn error_code(s: &str) -> String {
    s.trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(40)
        .collect()
}

pub fn parse_mode_reply(msg: &Value) -> ModeReply {
    let ok = msg.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let error = msg
        .get("error")
        .and_then(Value::as_str)
        .map(error_code)
        .unwrap_or_default();
    ModeReply {
        ok,
        mode: msg
            .get("mode")
            .and_then(Value::as_str)
            .and_then(PhoneMode::parse),
        error: if ok {
            String::new()
        } else if error.is_empty() {
            "failed".to_string()
        } else {
            error
        },
    }
}

/// Why a phone cannot be switched right now, if it cannot.
fn switch_blocker(connected: bool, supports_mode: bool, in_call: bool) -> Option<&'static str> {
    if !connected {
        Some("offline")
    } else if !supports_mode {
        Some("unsupported")
    } else if in_call {
        Some("in_call")
    } else {
        None
    }
}

/// `{ok, mode, error_code}` for the web console.
pub fn mode_reply_json(r: &ModeReply) -> Value {
    let error_code = if r.ok { Value::Null } else { json!(r.error) };
    json!({
        "ok": r.ok,
        "mode": r.mode.map(PhoneMode::as_str),
        "error_code": error_code,
    })
}

/// The web console's address for the phone app's "Open the web console" button,
/// from the console's address (`settings_store::effective_console_url`). None when
/// that is empty or not a web address.
pub fn phone_console_url(public_url: &str) -> Option<String> {
    let base = public_url.trim().trim_end_matches('/');
    let lower = base.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return None;
    }
    let host = base.split_once("://").map(|(_, rest)| rest).unwrap_or("");
    if host.is_empty() || host.starts_with('/') || host.chars().any(char::is_whitespace) {
        return None;
    }
    let base = base.strip_suffix("/calls").unwrap_or(base);
    Some(format!("{base}/calls/"))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

struct PhoneEntry {
    /// The open control connection; None after it closed.
    sink: Option<WsSink>,
    hello: PhoneHello,
    /// Last message on the control connection (ms since epoch).
    last_seen_ms: u64,
}

fn phone_json(serial: &str, p: &PhoneEntry, agent_call: bool) -> Value {
    let connected = p.sink.is_some();
    json!({
        "serial": serial,
        "connected": connected,
        "mode": p.hello.mode.as_str(),
        "supports_mode": p.hello.supports_mode,
        "model": p.hello.model,
        "app_version": p.hello.app_version,
        "in_call": connected && (p.hello.in_call || agent_call),
        "last_seen_ms": p.last_seen_ms,
    })
}

/// Phones on the idle control connection: what they reported, their socket, and
/// the `set_mode` requests waiting for an answer.
#[derive(Default)]
pub struct PhoneDevices {
    phones: std::sync::Mutex<HashMap<String, PhoneEntry>>,
    /// `set_mode` id → (serial, the waiting request).
    waiting: std::sync::Mutex<HashMap<String, (String, oneshot::Sender<ModeReply>)>>,
}

impl PhoneDevices {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn phones(&self) -> std::sync::MutexGuard<'_, HashMap<String, PhoneEntry>> {
        self.phones.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn waiting(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<String, (String, oneshot::Sender<ModeReply>)>> {
        self.waiting.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A phone said hello on its control connection.
    pub fn connected(&self, serial: &str, sink: WsSink, hello: &Value) {
        let info = parse_phone_hello(hello);
        info!(
            serial,
            mode = info.mode.as_str(),
            supports_mode = info.supports_mode,
            app_version = info.app_version.as_str(),
            "phone control hello"
        );
        self.phones().insert(
            serial.to_string(),
            PhoneEntry {
                sink: Some(sink),
                hello: info,
                last_seen_ms: now_ms(),
            },
        );
    }

    /// The control connection closed. Ignored when the phone already reconnected.
    pub fn disconnected(&self, serial: &str, sink: &WsSink) {
        {
            let mut phones = self.phones();
            let Some(p) = phones.get_mut(serial) else {
                return;
            };
            if !p.sink.as_ref().is_some_and(|s| Arc::ptr_eq(s, sink)) {
                return;
            }
            p.sink = None;
            p.hello.in_call = false;
            p.last_seen_ms = now_ms();
        }
        // A request still waiting for this phone keeps waiting: the phone app may be
        // stopped by the switch itself and send the result after it reconnects.
    }

    /// Any message on the control connection.
    pub fn touch(&self, serial: &str) {
        if let Some(p) = self.phones().get_mut(serial) {
            p.last_seen_ms = now_ms();
        }
    }

    /// `status`: the phone's mode or call state changed.
    pub fn on_status(&self, serial: &str, msg: &Value) {
        let mut phones = self.phones();
        let Some(p) = phones.get_mut(serial) else {
            return;
        };
        if let Some(mode) = msg
            .get("mode")
            .and_then(Value::as_str)
            .and_then(PhoneMode::parse)
        {
            p.hello.mode = mode;
            p.hello.supports_mode = true;
        }
        if let Some(in_call) = msg.get("in_call").and_then(Value::as_bool) {
            p.hello.in_call = in_call;
        }
        p.last_seen_ms = now_ms();
    }

    /// `set_mode_result`: hand it to the request waiting for it.
    pub fn on_mode_reply(&self, serial: &str, msg: &Value) {
        let reply = parse_mode_reply(msg);
        if let Some(mode) = reply.mode {
            if let Some(p) = self.phones().get_mut(serial) {
                p.hello.mode = mode;
                p.hello.supports_mode = true;
            }
        }
        let id = msg.get("id").and_then(Value::as_str).unwrap_or("");
        let waiter = {
            let mut waiting = self.waiting();
            let ours = waiting.get(id).is_some_and(|entry| entry.0 == serial);
            if ours {
                waiting.remove(id)
            } else {
                None
            }
        };
        match waiter {
            Some((_, tx)) => {
                let _ = tx.send(reply);
            }
            None => info!(serial, id, "set_mode_result without a waiting request"),
        }
    }

    /// The phone's mode while it is connected.
    pub fn mode_of(&self, serial: &str) -> Option<PhoneMode> {
        self.phones()
            .get(serial)
            .filter(|p| p.sink.is_some())
            .map(|p| p.hello.mode)
    }

    /// The connected phone said a call rings or is up on it.
    pub fn in_call(&self, serial: &str) -> bool {
        self.phones()
            .get(serial)
            .is_some_and(|p| p.sink.is_some() && p.hello.in_call)
    }

    /// Connected phones in normal-phone mode: the AI does not answer their calls.
    pub fn normal_mode_serials(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .phones()
            .iter()
            .filter(|(serial, p)| {
                p.sink.is_some()
                    && p.hello.mode == PhoneMode::Normal
                    && !crate::calls::is_twin_serial(serial)
            })
            .map(|(serial, _)| serial.clone())
            .collect();
        v.sort();
        v
    }

    /// Every phone seen since the agent started (System view), sorted by serial.
    /// `agent_calls`: serials with a call the agent is handling now.
    pub fn list_json(&self, agent_calls: &[String]) -> Vec<Value> {
        let phones = self.phones();
        let mut serials: Vec<&String> = phones
            .keys()
            .filter(|serial| !crate::calls::is_twin_serial(serial))
            .collect();
        serials.sort();
        serials
            .into_iter()
            .filter_map(|serial| {
                phones.get(serial).map(|p| {
                    phone_json(serial, p, agent_calls.iter().any(|s| s == serial))
                })
            })
            .collect()
    }

    /// Ask the phone to switch and wait for its answer (at most [`SET_MODE_TIMEOUT`]).
    /// `agent_call`: the agent is handling a call on this phone right now.
    pub async fn set_mode(&self, serial: &str, mode: PhoneMode, agent_call: bool) -> ModeReply {
        let (sink, current) = {
            let phones = self.phones();
            let Some(p) = phones.get(serial) else {
                return ModeReply::failed(None, "offline");
            };
            let current = Some(p.hello.mode);
            if let Some(code) = switch_blocker(
                p.sink.is_some(),
                p.hello.supports_mode,
                p.hello.in_call || agent_call,
            ) {
                return ModeReply::failed(current, code);
            }
            match p.sink.clone() {
                Some(sink) => (sink, current),
                None => return ModeReply::failed(current, "offline"),
            }
        };
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.waiting().insert(id.clone(), (serial.to_string(), tx));
        let payload = json!({"type": "set_mode", "mode": mode.as_str(), "id": id});
        if let Err(e) = crate::callback::send_ws_json(&sink, payload).await {
            self.waiting().remove(&id);
            tracing::warn!("set_mode send failed serial={serial}: {e:#}");
            return ModeReply::failed(current, "offline");
        }
        info!(serial, mode = mode.as_str(), id = id.as_str(), "set_mode sent");
        match tokio::time::timeout(SET_MODE_TIMEOUT, rx).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => ModeReply::failed(current, "offline"),
            Err(_) => {
                self.waiting().remove(&id);
                // No answer yet: the switch may still finish (the page says the result is
                // not confirmed). Report the mode the phone last told us.
                let last = self.phones().get(serial).map(|p| p.hello.mode).or(current);
                ModeReply::failed(last, "timeout")
            }
        }
    }
}

/// The body of `POST /api/devices/{serial}/mode`, `{"mode": "ai" | "normal"}`.
/// Err is the error code of the 400 answer: `bad_request` (not JSON, no `mode`) or
/// `invalid_mode`. Parsed here so every error is the same JSON shape.
pub fn parse_mode_body(body: &[u8]) -> Result<PhoneMode, &'static str> {
    let v: Value = serde_json::from_slice(body).map_err(|_| "bad_request")?;
    let mode = v.get("mode").and_then(Value::as_str).ok_or("bad_request")?;
    PhoneMode::parse(mode).ok_or("invalid_mode")
}

fn mode_bad_request(code: &str, message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"ok": false, "mode": null, "error_code": code, "error": message})),
    )
        .into_response()
}

fn is_json_request(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("application/json"))
        .unwrap_or(false)
}

/// Switch one phone between "the AI answers" and "normal phone".
async fn device_mode(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    Path(serial): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    if !task_id_safe(&serial) {
        return mode_bad_request("bad_request", "invalid phone serial");
    }
    // Only JSON: a cross-site form or no-cors fetch cannot send that without a CORS
    // preflight, which this server never answers, so other web pages cannot switch
    // phones from a browser on the same network.
    if !is_json_request(&headers) {
        return mode_bad_request("bad_request", "Content-Type must be application/json");
    }
    let mode = match parse_mode_body(&body) {
        Ok(mode) => mode,
        Err(code) => return mode_bad_request(code, "the body must be {\"mode\": \"ai\"} or {\"mode\": \"normal\"}"),
    };
    let agent_call = state
        .store
        .live_calls()
        .await
        .iter()
        .any(|c| c.serial == serial);
    let reply = state.phones.set_mode(&serial, mode, agent_call).await;
    info!(
        serial = serial.as_str(),
        target = mode.as_str(),
        ok = reply.ok,
        error = reply.error.as_str(),
        "phone mode switch"
    );
    Json(mode_reply_json(&reply)).into_response()
}

/// Tasks tab: prefer TaskStore (canonical); snake_case status. An appointment's
/// task also says where its text goes out by default (`sms_route`).
async fn tasks_list(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    let rt = state.settings_store.snapshot();
    let connected = state.control.connected_serials().await;
    let tasks: Vec<Value> = state
        .task_store
        .list(200)
        .await
        .iter()
        .map(|t| task_json(t, &rt, &connected))
        .collect();
    Json(json!({"ok": true, "tasks": tasks})).into_response()
}

async fn tasks_confirm(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    // JSON only, like the phone mode switch: other web pages cannot confirm a
    // task (write the calendar, text a caller) from a browser on the network.
    if !is_json_request(&headers) {
        return json_required();
    }
    if !task_id_safe(&id) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_id", "invalid task id");
    }
    let reply = match parse_decision_body(&body) {
        Ok(r) => r,
        Err(code) => return api_error(StatusCode::BAD_REQUEST, code, "the body must be {} or {\"reply\": \"sms\" | \"call\" | \"none\"}"),
    };
    let rt = state.settings_store.snapshot();
    if let Some(r) = appointment_decision(&state, &rt, &id, true, reply).await {
        return r;
    }
    match on_confirm(
        &state.http,
        &rt,
        &state.task_store,
        &id,
        Some(state.control.as_ref()),
    )
    .await
    {
        Ok(task) => Json(json!({"ok": true, "task": task})).into_response(),
        Err(e) => fulfill_err_response(e),
    }
}

async fn tasks_ignore(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    if !is_json_request(&headers) {
        return json_required();
    }
    if !task_id_safe(&id) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_id", "invalid task id");
    }
    let reply = match parse_decision_body(&body) {
        Ok(r) => r,
        Err(code) => return api_error(StatusCode::BAD_REQUEST, code, "the body must be {} or {\"reply\": \"sms\" | \"call\" | \"none\"}"),
    };
    let rt = state.settings_store.snapshot();
    if let Some(r) = appointment_decision(&state, &rt, &id, false, reply).await {
        return r;
    }
    match on_ignore(&state.task_store, &id).await {
        Ok(task) => Json(json!({"ok": true, "task": task})).into_response(),
        Err(e) => fulfill_err_response(e),
    }
}

/// HTTP 400 for a task decision without `Content-Type: application/json`.
fn json_required() -> Response {
    api_error(StatusCode::BAD_REQUEST, "bad_request", "Content-Type must be application/json")
}

/// The body of `POST /api/appointments/{id}/retry-sms`: empty, `{}` or
/// `{"serial": "<phone>"}`, the phone to send from (empty or none: the one that
/// took the call). Err is the 400 answer's code: `bad_request` (not a JSON
/// object, `serial` not a string) or `invalid_serial`.
pub fn parse_retry_body(body: &[u8]) -> Result<Option<String>, &'static str> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let v: Value = serde_json::from_slice(body).map_err(|_| "bad_request")?;
    let obj = v.as_object().ok_or("bad_request")?;
    match obj.get("serial") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) if task_id_safe(s.trim()) => Ok(Some(s.trim().to_string())),
        Some(Value::String(_)) => Err("invalid_serial"),
        Some(_) => Err("bad_request"),
    }
}

/// Text the caller of a decided appointment again, when the text did not go
/// out (or it is not known whether it did): `{ok, sms, message}` — `ok` when it
/// was sent, `sms` the new code ([`crate::appointments::Appointment::sms`]),
/// `message` what happened in the owner's words. Waiting, already sent or
/// never to be sent: HTTP 409 `sms_not_retryable` with the code it has.
async fn appointment_retry_sms(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    // JSON only, like a decision: other web pages cannot text a caller from a
    // browser on the network.
    if !is_json_request(&headers) {
        return json_required();
    }
    if !task_id_safe(&id) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_id", "invalid task id");
    }
    let serial = match parse_retry_body(&body) {
        Ok(serial) => serial,
        Err(code) => return api_error(StatusCode::BAD_REQUEST, code, "the body must be {} or {\"serial\": \"<phone>\"}"),
    };
    let is_appointment = state
        .task_store
        .get(&id)
        .await
        .is_some_and(|t| t.appointment.is_some());
    if !is_appointment {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "appointment not found", "code": "task_not_found"})),
        )
            .into_response();
    }
    let rt = state.settings_store.snapshot();
    match crate::appointments::retry_sms(
        &rt,
        &state.task_store,
        &state.store,
        Some(state.control.as_ref()),
        &id,
        serial.as_deref(),
    )
    .await
    {
        Ok(crate::appointments::RetrySms::Tried { sms, line, .. }) => {
            info!(task_id = id.as_str(), sms = sms.as_str(), "appointment sms retried");
            Json(json!({"ok": sms == "sent", "sms": sms, "message": line})).into_response()
        }
        Ok(crate::appointments::RetrySms::NotAllowed { sms }) => (
            StatusCode::CONFLICT,
            Json(json!({
                "ok": false,
                "sms": sms,
                "code": "sms_not_retryable",
                "error": "only a confirmed or declined appointment whose text did not go out can be texted again",
            })),
        )
            .into_response(),
        Err(e) => fulfill_err_response(e),
    }
}

/// An appointment task (the AI took it on a call) is confirmed or declined by
/// [`crate::appointments::decide_from_web`] (the caller told the way `reply`
/// says, else the default; the Telegram card's buttons taken off); None for
/// any other task.
async fn appointment_decision(
    state: &CallsState,
    rt: &crate::settings_store::RuntimeSettings,
    id: &str,
    confirm: bool,
    reply: Option<crate::appointments::Reply>,
) -> Option<Response> {
    let task = state.task_store.get(id).await?;
    task.appointment.as_ref()?;
    Some(
        match crate::appointments::decide_from_web(
            &state.http,
            rt,
            &state.task_store,
            &state.store,
            Some(state.control.as_ref()),
            id,
            confirm,
            reply,
        )
        .await
        {
            Ok((task, message)) => {
                let connected = state.control.connected_serials().await;
                Json(json!({"ok": true, "task": task_json(&task, rt, &connected), "message": message})).into_response()
            }
            Err(e) => fulfill_err_response(e),
        },
    )
}

async fn stream_wav(store: &CallStore, rel: &str, headers: &HeaderMap) -> Response {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let Some(path) = store.audio_abs(rel) else {
        return api_error(StatusCode::NOT_FOUND, "no_audio", "file missing");
    };
    let open_failed = || api_error(StatusCode::NOT_FOUND, "open_failed", "open failed");
    let Ok(meta) = tokio::fs::metadata(&path).await else {
        return open_failed();
    };
    let len = meta.len();
    // Range request: read only the requested slice (seeking a 50 MB call no
    // longer downloads the whole file again).
    if let Some((s, e)) = parse_range(headers, len) {
        let Ok(mut f) = tokio::fs::File::open(&path).await else {
            return open_failed();
        };
        let mut buf = vec![0u8; (e - s + 1) as usize];
        if f.seek(std::io::SeekFrom::Start(s)).await.is_err() || f.read_exact(&mut buf).await.is_err()
        {
            return open_failed();
        }
        return Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_TYPE, "audio/wav")
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CACHE_CONTROL, "private, max-age=300")
            .header(header::CONTENT_RANGE, format!("bytes {s}-{e}/{len}"))
            .header(header::CONTENT_LENGTH, buf.len().to_string())
            .body(Body::from(buf))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    match tokio::fs::read(&path).await {
        Ok(bytes) => ranged_response(headers, bytes::Bytes::from(bytes)),
        Err(_) => open_failed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(range: &str) -> HeaderMap {
        let mut m = HeaderMap::new();
        m.insert(header::RANGE, range.parse().unwrap());
        m
    }

    #[test]
    fn mode_switch_needs_a_json_content_type() {
        let mut m = HeaderMap::new();
        assert!(!is_json_request(&m));
        m.insert(header::CONTENT_TYPE, "text/plain;charset=UTF-8".parse().unwrap());
        assert!(!is_json_request(&m));
        m.insert(header::CONTENT_TYPE, "application/json; charset=utf-8".parse().unwrap());
        assert!(is_json_request(&m));
        m.insert(header::CONTENT_TYPE, "Application/JSON".parse().unwrap());
        assert!(is_json_request(&m));
    }

    #[test]
    fn a_text_is_sent_again_from_the_phone_asked_for() {
        // (the decision's body is read the same careful way)
        use crate::appointments::Reply;
        for body in [&b""[..], b" ", b"{}", br#"{"reply": null}"#] {
            assert_eq!(parse_decision_body(body), Ok(None), "{:?}", String::from_utf8_lossy(body));
        }
        assert_eq!(parse_decision_body(br#"{"reply": "call"}"#), Ok(Some(Reply::Call)));
        assert_eq!(parse_decision_body(br#"{"reply": " SMS "}"#), Ok(Some(Reply::Sms)));
        assert_eq!(parse_decision_body(br#"{"reply": "none"}"#), Ok(Some(Reply::None)));
        for bad in [&br#"{"reply": "fax"}"#[..], br#"{"reply": 1}"#, b"[]", b"reply=call"] {
            assert_eq!(parse_decision_body(bad), Err("bad_request"), "{:?}", String::from_utf8_lossy(bad));
        }
        for body in [&b""[..], b"  ", b"{}", br#"{"serial": null}"#, br#"{"serial": " "}"#] {
            assert_eq!(parse_retry_body(body), Ok(None), "{:?}", String::from_utf8_lossy(body));
        }
        assert_eq!(parse_retry_body(br#"{"serial": " pixel-10341 "}"#), Ok(Some("pixel-10341".to_string())));
        assert_eq!(parse_retry_body(br#"{"serial": "../etc"}"#), Err("invalid_serial"));
        for bad in [&br#"{"serial": 7}"#[..], b"[]", b"serial=pixel-1", br#""pixel-1""#] {
            assert_eq!(parse_retry_body(bad), Err("bad_request"), "{:?}", String::from_utf8_lossy(bad));
        }
    }

    /// Review t5 (E): ids and phones the retry-sms endpoint must refuse.
    #[test]
    fn review_retry_ids_and_serials_that_must_be_refused() {
        for id in ["", "..", "../etc/passwd", "..%2Fetc", "a/b", "a\\b", "x y", "<script>"] {
            assert!(!task_id_safe(id), "{id}");
        }
        assert!(!task_id_safe(&"a".repeat(129)));
        assert!(task_id_safe("20261114-abc_DEF-1"));
        let long = format!(r#"{{"serial": "{}"}}"#, "p".repeat(129));
        assert_eq!(parse_retry_body(long.as_bytes()), Err("invalid_serial"));
        for bad in [r#"{"serial": "../x"}"#, r#"{"serial": "pixel 1"}"#, r#"{"serial": "a/b"}"#] {
            assert_eq!(parse_retry_body(bad.as_bytes()), Err("invalid_serial"), "{bad}");
        }
        assert_eq!(parse_retry_body(br#"{"serial": true}"#), Err("bad_request"));
    }

    #[test]
    fn range_parsing() {
        assert_eq!(parse_range(&HeaderMap::new(), 100), None);
        assert_eq!(parse_range(&h("bytes=0-"), 100), Some((0, 99)));
        assert_eq!(parse_range(&h("bytes=10-19"), 100), Some((10, 19)));
        assert_eq!(parse_range(&h("bytes=90-500"), 100), Some((90, 99)));
        assert_eq!(parse_range(&h("bytes=-10"), 100), Some((90, 99)));
        assert_eq!(parse_range(&h("bytes=100-"), 100), None);
        assert_eq!(parse_range(&h("bytes=5-1"), 100), None);
        assert_eq!(parse_range(&h("bytes=0-1,5-6"), 100), None);
        assert_eq!(parse_range(&h("items=0-1"), 100), None);
    }

    #[test]
    fn ranged_response_statuses() {
        let body = bytes::Bytes::from(vec![7u8; 100]);
        assert_eq!(ranged_response(&HeaderMap::new(), body.clone()).status(), StatusCode::OK);
        let r = ranged_response(&h("bytes=10-19"), body.clone());
        assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(r.headers()[header::CONTENT_RANGE], "bytes 10-19/100");
        assert_eq!(r.headers()[header::CONTENT_LENGTH], "10");
        assert_eq!(
            ranged_response(&h("bytes=200-"), body).status(),
            StatusCode::RANGE_NOT_SATISFIABLE
        );
    }

    #[test]
    fn ui_asset_gzip_roundtrip() {
        use std::io::Read;
        let a = ui_asset();
        assert!(a.etag.starts_with('"') && a.etag.len() > 4);
        let mut out = String::new();
        flate2::read::GzDecoder::new(&a.gzip[..])
            .read_to_string(&mut out)
            .unwrap();
        assert_eq!(out, UI_HTML);
        assert!(a.gzip.len() < UI_HTML.len() / 2);
    }

    #[test]
    fn channel_errors_carry_a_code_and_the_field() {
        let e: anyhow::Error = crate::notify::ChannelError::new("missing_field", "token", "bot token is empty").into();
        let body = channel_error_body("telegram", &e);
        assert_eq!((body["code"].as_str(), body["field"].as_str()), (Some("missing_field"), Some("token")));
        assert_eq!(body["kind"], "telegram");
        assert_eq!(body["error"], "bot token is empty");
        // Behind a context it is still found.
        let wrapped = e.context("sending the test message");
        assert_eq!(channel_error_body("telegram", &wrapped)["code"], "missing_field");
        // Errors without a code fall back to a generic one.
        let plain = anyhow::anyhow!("boom");
        assert_eq!(channel_error_body("ntfy", &plain)["code"], "send_failed");
    }

    #[test]
    fn phone_hello_old_and_new_apps() {
        // An app from before the mode switch: AI mode, cannot be switched from the page.
        let old = parse_phone_hello(&json!({
            "type": "hello", "serial": "pixel-10341", "mode": "CONTROL_IDLE",
            "model": "Pixel 7", "app_version": "0.2.0"
        }));
        assert_eq!(old.mode, PhoneMode::Ai);
        assert!(!old.supports_mode);
        assert_eq!((old.model.as_str(), old.app_version.as_str()), ("Pixel 7", "0.2.0"));
        let new = parse_phone_hello(&json!({
            "type": "hello", "mode": "CONTROL_IDLE", "phone_mode": "normal", "in_call": true
        }));
        assert_eq!(new.mode, PhoneMode::Normal);
        assert!(new.supports_mode && new.in_call);
        // The connection kind is never taken for the phone mode.
        let odd = parse_phone_hello(&json!({"phone_mode": "CONTROL_IDLE"}));
        assert_eq!((odd.mode, odd.supports_mode), (PhoneMode::Ai, false));
    }

    #[test]
    fn mode_reply_parsing() {
        let ok = parse_mode_reply(&json!({"type": "set_mode_result", "id": "x", "ok": true, "mode": "normal", "error": ""}));
        assert_eq!(ok, ModeReply { ok: true, mode: Some(PhoneMode::Normal), error: String::new() });
        let fail = parse_mode_reply(&json!({"ok": false, "mode": "ai", "error": "dialer_failed"}));
        assert_eq!((fail.mode, fail.error.as_str()), (Some(PhoneMode::Ai), "dialer_failed"));
        // Whatever the phone sends ends up as a plain code.
        assert_eq!(parse_mode_reply(&json!({"ok": false, "error": "<b>x</b> y"})).error, "bxby");
        assert_eq!(parse_mode_reply(&json!({})).error, "failed");
    }

    #[test]
    fn mode_reply_json_shape() {
        let ok = ModeReply { ok: true, mode: Some(PhoneMode::Ai), error: String::new() };
        assert_eq!(mode_reply_json(&ok), json!({"ok": true, "mode": "ai", "error_code": null}));
        assert_eq!(
            mode_reply_json(&ModeReply::failed(None, "timeout")),
            json!({"ok": false, "mode": null, "error_code": "timeout"})
        );
    }

    #[test]
    fn switch_blockers() {
        assert_eq!(switch_blocker(false, true, false), Some("offline"));
        assert_eq!(switch_blocker(true, false, false), Some("unsupported"));
        assert_eq!(switch_blocker(true, true, true), Some("in_call"));
        assert_eq!(switch_blocker(true, true, false), None);
    }

    #[test]
    fn mode_reply_goes_only_to_its_phone() {
        let devices = PhoneDevices::default();
        let (tx, mut rx) = oneshot::channel();
        devices.waiting().insert("id1".into(), ("pixel-1".into(), tx));
        devices.on_mode_reply("pixel-2", &json!({"id": "id1", "ok": true, "mode": "normal"}));
        assert!(rx.try_recv().is_err());
        devices.on_mode_reply("pixel-1", &json!({"id": "id1", "ok": true, "mode": "normal"}));
        assert_eq!(rx.try_recv().unwrap().mode, Some(PhoneMode::Normal));
    }

    #[test]
    fn disconnected_phone_is_never_in_call() {
        let hello = parse_phone_hello(&json!({"phone_mode": "normal", "in_call": true}));
        let p = PhoneEntry { sink: None, hello, last_seen_ms: 5 };
        let v = phone_json("pixel-1", &p, true);
        assert_eq!((v["connected"].clone(), v["in_call"].clone()), (json!(false), json!(false)));
        assert_eq!(v["mode"], json!("normal"));
    }

    #[test]
    fn console_url_from_public_url() {
        assert_eq!(phone_console_url(""), None);
        assert_eq!(phone_console_url("192.168.1.20:8787"), None);
        assert_eq!(phone_console_url("ftp://192.168.1.20"), None);
        assert_eq!(phone_console_url("http://"), None);
        assert_eq!(
            phone_console_url("http://192.168.1.20:8787").as_deref(),
            Some("http://192.168.1.20:8787/calls/")
        );
        assert_eq!(
            phone_console_url(" http://192.168.1.20:8787/calls/ ").as_deref(),
            Some("http://192.168.1.20:8787/calls/")
        );
        assert_eq!(phone_console_url("https://bageo.example/").as_deref(), Some("https://bageo.example/calls/"));
    }

    #[test]
    fn mode_body_parsing() {
        assert_eq!(parse_mode_body(br#"{"mode":"normal"}"#), Ok(PhoneMode::Normal));
        assert_eq!(parse_mode_body(br#"{"mode":" ai "}"#), Ok(PhoneMode::Ai));
        assert_eq!(parse_mode_body(br#"{"mode":"off"}"#), Err("invalid_mode"));
        assert_eq!(parse_mode_body(br#"{"mode":1}"#), Err("bad_request"));
        assert_eq!(parse_mode_body(br#"{}"#), Err("bad_request"));
        assert_eq!(parse_mode_body(b"mode=normal"), Err("bad_request"));
        assert_eq!(parse_mode_body(b""), Err("bad_request"));
    }
}

pub fn router(state: CallsState) -> Router {
    Router::new()
        .route("/", get(ui))
        .route("/manifest.webmanifest", get(|| async { static_file(MANIFEST, "application/manifest+json") }))
        .route("/icon-192.png", get(|| async { static_file(ICON_192, "image/png") }))
        .route("/icon-512.png", get(|| async { static_file(ICON_512, "image/png") }))
        .route("/apple-touch-icon.png", get(|| async { static_file(APPLE_TOUCH_ICON, "image/png") }))
        .route("/favicon.ico", get(favicon))
        .route("/api/list", get(list))
        .route("/api/stats", get(stats))
        .route("/api/live", get(live))
        .route("/api/call/{id}", get(call_get))
        .route("/api/call/{id}/event.ics", get(call_event_ics))
        .route("/api/export.csv", get(export_csv))
        .route("/api/audio/{id}", get(audio))
        .route("/api/audio/{id}/listen", get(audio_listen))
        .route("/api/audio/{id}/tts", get(audio_tts))
        .route("/api/audio/{id}/vm", get(audio_vm))
        .route("/api/audio/{id}/remote", get(audio_remote))
        .route("/api/settings", get(settings_get).put(settings_put))
        .route("/api/preview", get(preview))
        .route("/api/notify/test", post(notify_test))
        .route("/api/notify/send", post(notify_send))
        .route("/api/devices", get(devices))
        .route("/api/devices/{serial}/mode", post(device_mode))
        .route("/api/tasks", get(tasks_list))
        .route("/api/tasks/{id}/confirm", post(tasks_confirm))
        .route("/api/tasks/{id}/ignore", post(tasks_ignore))
        .route("/api/tasks/{id}/event.ics", get(task_event_ics))
        .route("/api/appointments/{id}/retry-sms", post(appointment_retry_sms))
        .merge(crate::contacts::router())
        .merge(crate::errands::router())
        .with_state(state)
}
