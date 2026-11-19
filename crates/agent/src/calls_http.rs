// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! LAN web console: call history + runtime settings + tasks (`/calls`).

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, Extensions, HeaderMap, StatusCode};
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
    /// Text messages the SMS forwarder handed over ([`crate::sms_store`]), for the SMS page.
    pub sms: Arc<crate::sms_store::SmsStore>,
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

/// Who may download an `.ics`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IcsAccess {
    Open,
    /// A link's key that does not open this file (another file's, changed, or
    /// an older link's): answered like a file that is not there.
    BadKey,
    /// Neither the console token nor a key.
    NoAccess,
}

/// The console token opens it, or the key made for this one file (`what` =
/// its path under `/calls/api/`, [`crate::calendar::key_opens`]).
fn ics_access(token_ok: bool, k: Option<&str>, what: &str) -> IcsAccess {
    if token_ok {
        return IcsAccess::Open;
    }
    match k.map(str::trim).filter(|k| !k.is_empty()) {
        Some(k) if crate::calendar::key_opens(what, k) => IcsAccess::Open,
        Some(_) => IcsAccess::BadKey,
        None => IcsAccess::NoAccess,
    }
}

/// [`ics_access`] for a request; `None` when it may download the file, else
/// the answer (not found for a wrong key, nothing about the file).
fn ics_access_denied(settings: &Settings, headers: &HeaderMap, query: &IcsQuery, what: &str) -> Option<Response> {
    match ics_access(token_ok_parts(settings, headers, query.token.as_deref()), query.k.as_deref(), what) {
        IcsAccess::Open => None,
        IcsAccess::BadKey => Some(api_error(StatusCode::NOT_FOUND, "not_found", "not found")),
        IcsAccess::NoAccess => Some(unauthorized()),
    }
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
pub(crate) async fn transcript_index(task_store: &TaskStore) -> HashMap<String, String> {
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
    let active = crate::settings_store::active_ui_token(&settings.calls_ui_token);
    let expected = active.trim();
    if expected.is_empty() {
        return true;
    }
    if query_token.is_some_and(|q| crate::mcp::same(q, expected)) {
        return true;
    }
    if let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(rest) = auth.strip_prefix("Bearer ") {
            if crate::mcp::same(rest, expected) {
                return true;
            }
        }
    }
    if let Some(t) = headers.get("x-calls-token").and_then(|v| v.to_str().ok()) {
        if crate::mcp::same(t, expected) {
            return true;
        }
    }
    false
}

pub fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": "unauthorized", "code": "unauthorized", "hint": "pass ?token= or Authorization: Bearer"})),
    )
        .into_response()
}

/// An error for the web console: `code` is what the page translates
/// (`err_<code>` in its dictionary); `error` keeps the agent's own words for
/// the tooltip and for scripts.
pub(crate) fn api_error(status: StatusCode, code: &str, error: impl Into<String>) -> Response {
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
    let contacts_by_number = crate::contacts::names_for(&state.contacts, &calls);
    let control_serials = state.control.connected_serials().await;
    Json(json!({
        "ok": true,
        "count": calls.len(),
        "calls": calls,
        "contacts_by_number": contacts_by_number,
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
    // The caller's contact, when the number is one (else any matching blacklist prefix).
    let contact = state
        .contacts
        .lookup(&rec.caller_id, rec.sim_country.as_deref())
        .or_else(|| state.contacts.blacklisted(&rec.caller_id, rec.sim_country.as_deref()))
        .map(|c| {
            json!({
                "id": c.id,
                "name": c.name,
                "note": c.note,
                "kind": crate::contacts::sanitize_kind(&c.kind)
            })
        });
    let calendar_links = notes
        .as_ref()
        .and_then(|n| n.calendar.as_ref().map(|ev| ev.links(&n.summary)));
    // The judged kind of call in every language the page speaks (null when none was named).
    let scam_names = rec
        .scam
        .as_ref()
        .and_then(|j| crate::scam_db::typology(&j.typology))
        .map(|t| t.names.clone());
    Json(json!({
        "ok": true,
        "call": rec,
        "notes": notes,
        "calendar_link": calendar_link,
        "contact": contact,
        "calendar_links": calendar_links,
        "scam_names": scam_names,
    }))
    .into_response()
}
/// The body of `POST /api/call/{id}/scam-feedback`: `{"verdict": "scam" | "sales" | "safe"}`
/// (`not_scam` / `normal` = safe), optionally with the database's `typology`.
/// Err is the 400 answer's code.
pub fn parse_scam_feedback_body(body: &[u8]) -> Result<(&'static str, String), &'static str> {
    let v: Value = serde_json::from_slice(body).map_err(|_| "bad_request")?;
    let verdict = v
        .get("verdict")
        .and_then(Value::as_str)
        .and_then(crate::calls::parse_scam_verdict)
        .ok_or("invalid_verdict")?;
    let typology = match v.get("typology") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(t)) if t.trim().is_empty() => String::new(),
        Some(Value::String(t)) => crate::scam_db::typology(t.trim()).ok_or("invalid_typology")?.id.clone(),
        Some(_) => return Err("invalid_typology"),
    };
    Ok((verdict, typology))
}

/// The owner's mark for a call: kept on the record, for the caller's number
/// (not a hidden one) in the number book — "safe" protects it like a contact,
/// "scam" / "sales" has its next calls looked at sooner — and in the feedback
/// log. Err: (status, code).
pub async fn apply_scam_feedback(
    store: &CallStore,
    book: &crate::scam_db::NumberBook,
    id: &str,
    verdict: &str,
    typology: &str,
) -> Result<Value, (StatusCode, &'static str)> {
    let Some(before) = store.get(id).await else {
        return Err((StatusCode::NOT_FOUND, "not_found"));
    };
    let typology = if typology.is_empty() && verdict != "safe" {
        before.scam.as_ref().map(|j| j.typology.clone()).unwrap_or_default()
    } else {
        typology.to_string()
    };
    let rec = store.set_scam_feedback(id, verdict).await.ok_or((StatusCode::NOT_FOUND, "not_found"))?;
    // The SIM's country the record kept places a local number; without it the
    // number is remembered by its digits.
    let key = crate::scam_db::number_key(&rec.caller_id, rec.sim_country.as_deref());
    let entry = key.as_deref().map(|k| book.mark_owner(k, verdict, &typology));
    book.log_feedback(&json!({
        "at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "call_id": rec.id,
        "number": key,
        "verdict": verdict,
        "typology": typology,
        "judged": rec.scam,
        "events": rec.events,
    }));
    info!(
        "scam feedback call_id={id} verdict={verdict} typology={typology} number_remembered={} judged={:?}",
        entry.is_some(),
        rec.scam.as_ref().map(|j| (j.kind.as_str(), j.typology.as_str()))
    );
    Ok(json!({"ok": true, "call": rec, "number": entry}))
}

/// `POST /api/call/{id}/scam-feedback`: "this call was a scam / sales call" or "it was not".
async fn call_scam_feedback(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    // JSON only: other web pages cannot mark numbers from a browser on the network.
    if !is_json_request(&headers) {
        return json_required();
    }
    if !task_id_safe(&id) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_id", "invalid call id");
    }
    let (verdict, typology) = match parse_scam_feedback_body(&body) {
        Ok(x) => x,
        Err(code) => {
            return api_error(StatusCode::BAD_REQUEST, code, "the body must be {\"verdict\": \"scam\" | \"sales\" | \"safe\"} with an optional known \"typology\"")
        }
    };
    match apply_scam_feedback(&state.store, crate::scam_db::numbers(), &id, verdict, &typology).await {
        Ok(v) => Json(v).into_response(),
        Err((status, code)) => api_error(status, code, "call not found"),
    }
}

/// `DELETE /api/call/{id}`: remove a single finished call record, its WAV
/// files, `_notes.json`, and any associated task records.
async fn call_delete(
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
    match state.store.delete_call(&id).await {
        Ok(true) => {
            let mut ids = std::collections::HashSet::with_capacity(1);
            ids.insert(id.clone());
            let tasks_deleted = state.task_store.delete_by_call_ids(&ids).await;
            Json(json!({"ok": true, "id": id, "tasks_deleted": tasks_deleted})).into_response()
        }
        Ok(false) => api_error(StatusCode::NOT_FOUND, "not_found", "call not found"),
        Err("call_in_progress") => api_error(
            StatusCode::CONFLICT,
            "call_in_progress",
            "cannot delete a call while it is in progress",
        ),
        Err(code) => api_error(StatusCode::BAD_REQUEST, code, "cannot delete call"),
    }
}

/// `POST /api/calls/clear`: remove all finished call records, their WAV files,
/// `_notes.json`, and associated task records (preserving any live call).
async fn calls_clear(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    if !is_json_request(&headers) {
        return json_required();
    }
    let (deleted, ids) = state.store.delete_all_finished().await;
    let tasks_deleted = state.task_store.delete_by_call_ids(&ids).await;
    Json(json!({"ok": true, "deleted": deleted, "tasks_deleted": tasks_deleted})).into_response()
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
    if let Some(denied) = ics_access_denied(&state.settings, &headers, &query, &format!("call/{id}")) {
        return denied;
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
    if let Some(denied) = ics_access_denied(&state.settings, &headers, &query, &format!("tasks/{id}")) {
        return denied;
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
/// Also the phone its text goes out from (`sms_route`,
/// [`crate::appointments::task_json`]).
fn task_json(task: &crate::tasks::TaskRecord, rt: &crate::settings_store::RuntimeSettings) -> Value {
    let mut v = crate::appointments::task_json(rt, task);
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
    // A cell a spreadsheet would read as a formula (= + - @, or a tab / CR in
    // front) gets a leading apostrophe: it shows as text, never runs.
    let guarded;
    let s = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        guarded = format!("'{s}");
        guarded.as_str()
    } else {
        s
    };
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') || s.contains('\t') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// A request that came through a reverse proxy on this machine (nginx, Caddy,
/// a tunnel): it says so in one of these headers. Such a request is not this
/// machine's own, even when it reaches the agent from 127.0.0.1.
pub fn came_through_proxy(headers: &HeaderMap) -> bool {
    ["x-forwarded-for", "forwarded", "x-real-ip", "cf-connecting-ip", "x-forwarded-host", "true-client-ip"]
        .iter()
        .any(|h| headers.contains_key(*h))
}

/// Redirects an outgoing request may follow: at most 10, and only on the same
/// host and port (http may turn into https, never back). Keys sent in headers
/// (`x-goog-api-key`, `Access-Token`, bot tokens) then never reach another
/// server that a redirect points at.
pub fn same_host_redirects() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 10 {
            return attempt.error("too many redirects");
        }
        let ok = attempt.previous().first().is_some_and(|first| redirect_stays(first, attempt.url()));
        if ok {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

fn redirect_stays(from: &reqwest::Url, to: &reqwest::Url) -> bool {
    let scheme_ok = from.scheme() == to.scheme() || (from.scheme() == "http" && to.scheme() == "https");
    let port = |u: &reqwest::Url| match (u.port(), u.scheme()) {
        (Some(p), _) => Some(p),
        // http → https on the default ports counts as the same place.
        (None, "http") | (None, "https") => None,
        (None, _) => u.port_or_known_default(),
    };
    scheme_ok
        && from.host_str().is_some_and(|h| to.host_str().is_some_and(|t| t.eq_ignore_ascii_case(h)))
        && port(from) == port(to)
}

/// An HTTP client for requests that carry a key: [`same_host_redirects`].
pub fn keyed_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(same_host_redirects())
        .build()
        .unwrap_or_default()
}

#[derive(Deserialize)]
struct AudioQuery {
    pub token: Option<String>,
    pub download: Option<String>,
}

impl AudioQuery {
    fn as_token(&self) -> TokenQuery {
        TokenQuery {
            token: self.token.clone(),
        }
    }

    fn wants_download(&self) -> bool {
        matches!(
            self.download.as_deref().map(str::trim),
            Some("1" | "true" | "yes")
        )
    }
}

fn attach_download(mut resp: Response, wants_download: bool, filename: &str) -> Response {
    if wants_download && (resp.status() == StatusCode::OK || resp.status() == StatusCode::PARTIAL_CONTENT) {
        if let Ok(val) = header::HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
            resp.headers_mut().insert(header::CONTENT_DISPOSITION, val);
        }
    }
    resp
}

async fn audio(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<AudioQuery>,
    Path(id): Path<String>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query.as_token()) {
        return unauthorized();
    }
    if !task_id_safe(&id) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_id", "invalid call id");
    }
    let Some(rec) = state.store.get(&id).await else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "not found");
    };
    let Some(rel) = rec.audio_path.as_deref() else {
        return api_error(StatusCode::NOT_FOUND, "no_audio", "no audio");
    };
    let resp = stream_wav(&state.store, rel, &headers).await;
    attach_download(resp, query.wants_download(), &format!("call-{id}.wav"))
}

/// Page-listen WAV: ducked mix rebuilt from remote+tts (works for old additive files).
async fn audio_listen(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<AudioQuery>,
    Path(id): Path<String>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query.as_token()) {
        return unauthorized();
    }
    if !task_id_safe(&id) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_id", "invalid call id");
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
        Some(bytes) => attach_download(
            ranged_response(&headers, bytes),
            query.wants_download(),
            &format!("call-{id}.wav"),
        ),
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
    Query(query): Query<AudioQuery>,
    Path(id): Path<String>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query.as_token()) {
        return unauthorized();
    }
    if !task_id_safe(&id) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_id", "invalid call id");
    }
    let Some(rec) = state.store.get(&id).await else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "not found");
    };
    let Some(rel) = rec.tts_audio_path.as_deref() else {
        return api_error(StatusCode::NOT_FOUND, "no_audio", "no tts audio");
    };
    let resp = stream_wav(&state.store, rel, &headers).await;
    attach_download(resp, query.wants_download(), &format!("call-{id}-ai.wav"))
}

/// Message-only voicemail WAV (`<id>_vm.wav`), mirrors the full-call handler.
async fn audio_vm(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<AudioQuery>,
    Path(id): Path<String>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query.as_token()) {
        return unauthorized();
    }
    if !task_id_safe(&id) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_id", "invalid call id");
    }
    let Some(rec) = state.store.get(&id).await else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "not found");
    };
    let Some(rel) = rec.voicemail_path.as_deref() else {
        return api_error(StatusCode::NOT_FOUND, "no_audio", "no voicemail");
    };
    let resp = stream_wav(&state.store, rel, &headers).await;
    attach_download(resp, query.wants_download(), &format!("call-{id}-voicemail.wav"))
}

/// Whole-call remote/caller uplink WAV (`<id>_remote.wav`) - greeting/AI/VM/goodbye.
async fn audio_remote(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<AudioQuery>,
    Path(id): Path<String>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query.as_token()) {
        return unauthorized();
    }
    if !task_id_safe(&id) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_id", "invalid call id");
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
        Some(bytes) => attach_download(
            ranged_response(&headers, bytes),
            query.wants_download(),
            &format!("call-{id}-caller.wav"),
        ),
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
        "console_default_url": console_default_url(&state),
        "console_access": console_access(&state),
        "console_password_set": console_password_set(&state),
        "language_chosen": state.settings_store.language_chosen(),
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

/// The address an empty "Address in notifications" stands for.
fn console_default_url(state: &CallsState) -> String {
    crate::settings_store::default_console_url(&state.settings_store.snapshot().effective(&state.settings))
}

/// `local` (this computer only) or `lan` (other devices too).
fn console_access(state: &CallsState) -> &'static str {
    crate::settings_store::console_access(&state.settings)
}

fn console_password_set(state: &CallsState) -> bool {
    !crate::settings_store::active_ui_token(&state.settings.calls_ui_token).trim().is_empty()
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
    // A new console password: at least 8 characters (one already saved stays usable).
    if patch.get("ui_password").and_then(Value::as_str).is_some_and(crate::settings_store::ui_password_too_short) {
        return password_too_short();
    }
    // A backup folder this computer cannot write in, or a backup password that
    // does not work, is not saved.
    if let Some(refused) = crate::backup::check_settings_patch(&patch, &state.settings_store.snapshot()) {
        return refused;
    }
    let days_before = state.settings_store.snapshot().retention_days;
    // Blocked numbers sent the old way go onto the Blacklist (the settings do
    // not keep them); nothing is saved when the contacts cannot be.
    if let Some(v) = patch.get("reject_numbers") {
        let entries: Vec<(String, String)> = crate::settings_store::reject_entries(v).into_iter().map(|e| (e.number, e.reason)).collect();
        if let Err(e) = state.contacts.block_many(&entries).await {
            tracing::warn!("settings: blocked numbers not put on the Blacklist: {e:?}");
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "contacts_save_failed", "the blocked numbers could not be saved");
        }
    }
    match state.settings_store.apply_patch(&patch) {
        Ok(rt) => {
            purge_after_fewer_days(&state, days_before, rt.retention_days);
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
                "console_default_url": console_default_url(&state),
                "console_access": console_access(&state),
                "console_password_set": console_password_set(&state),
                "language_chosen": state.settings_store.language_chosen(),
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
    /// `"sms"` marks a forwarded text message; anything else is a plain message.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub text: String,
    /// Only for `"kind": "sms"`: the number of the SIM that received it,
    /// the phone (serial and name), when the phone received it (Unix
    /// milliseconds) and the sender's own key for the message. A second
    /// request with the same `ref` is not stored again.
    #[serde(default)]
    pub to: String,
    #[serde(default)]
    pub serial: String,
    #[serde(default)]
    pub device: String,
    #[serde(default, deserialize_with = "lenient_u64")]
    pub received_ms: u64,
    #[serde(default, rename = "ref")]
    pub reference: String,
    /// Only for a text message: the SIM (Android subscription id) that received it.
    #[serde(default, deserialize_with = "lenient_i64")]
    pub sub_id: Option<i64>,
}

/// A whole number given as a number or as text; anything else counts as 0
/// (the field is optional, a script's odd value must not fail the request).
fn lenient_u64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Number(n) => n
            .as_u64()
            .or_else(|| n.as_f64().filter(|f| f.is_finite() && *f > 0.0).map(|f| f as u64))
            .unwrap_or(0),
        Value::String(s) => s.trim().parse().unwrap_or(0),
        _ => 0,
    })
}

/// A whole number given as a number or as text; anything else is None.
fn lenient_i64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    })
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
    let rt = state.settings_store.snapshot();
    let eff = rt.effective(&state.settings);
    let title = if body.title.trim().is_empty() { "Bageo" } else { body.title.trim() };
    // A text message is kept for the console's SMS page. The forwarder sends
    // one again until a channel took it: the same ref is stored only once.
    let is_sms = body.kind.trim().eq_ignore_ascii_case("sms");
    let repeated = is_sms
        && (!body.from.trim().is_empty() || !body.text.trim().is_empty())
        && state.sms.push(
            crate::sms_store::now_ms(),
            rt.retention_days,
            crate::sms_store::NewSms {
                received_ms: body.received_ms,
                from: &body.from,
                to: &body.to,
                text: &body.text,
                serial: &body.serial,
                device: &body.device,
                reference: &body.reference,
                sub_id: body.sub_id,
            },
        ) == crate::sms_store::Pushed::Duplicate;
    // Remember it for the desktop status app whether or not a channel works.
    if !repeated {
        crate::recent_notices::record(&body.kind, title, &body.from, &body.text, &body.body);
    }
    let ok = crate::notify::notify_event(&state.http, &eff, title, &body.body).await;
    Json(json!({"ok": ok})).into_response()
}

#[derive(Deserialize)]
struct RecentQuery {
    token: Option<String>,
    after: Option<String>,
}

/// The last messages sent through `notify/send` (memory only, newest 50), so
/// a desktop app can poll. Without a numeric `after` only `last_id` is useful:
/// the client uses it as its starting point.
async fn notify_recent(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<RecentQuery>,
) -> Response {
    if !token_ok_parts(&state.settings, &headers, query.token.as_deref()) {
        return unauthorized();
    }
    let after = query.after.as_deref().and_then(|s| s.trim().parse::<u64>().ok());
    let (last_id, events) = crate::recent_notices::snapshot(after.unwrap_or(u64::MAX));
    Json(json!({"ok": true, "last_id": last_id, "events": events})).into_response()
}

#[derive(Deserialize)]
struct SmsListQuery {
    token: Option<String>,
    q: Option<String>,
    before: Option<String>,
    limit: Option<String>,
}

/// The stored text messages, newest first: `q` searches sender, contact name
/// and text; `before` (an id) gives the next page.
async fn sms_list(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<SmsListQuery>,
) -> Response {
    if !token_ok_parts(&state.settings, &headers, query.token.as_deref()) {
        return unauthorized();
    }
    let keep_days = state.settings_store.snapshot().retention_days;
    let before = query.before.as_deref().and_then(|s| s.trim().parse::<u64>().ok()).filter(|id| *id > 0);
    let limit = query
        .limit
        .as_deref()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(crate::sms_store::PAGE_DEFAULT)
        .clamp(1, crate::sms_store::PAGE_MAX);
    let name_of = |from: &str| -> String {
        if from.trim().is_empty() {
            return String::new();
        }
        state.contacts.lookup(from, None).map(|c| c.name).unwrap_or_default()
    };
    let page = state.sms.list(
        crate::sms_store::now_ms(),
        keep_days,
        query.q.as_deref().unwrap_or(""),
        before,
        limit,
        &name_of,
    );
    let labels: HashMap<String, String> = crate::notify::known_devices().into_iter().collect();
    let messages: Vec<Value> = page
        .messages
        .iter()
        .map(|m| {
            // The name this Bageo knows the phone by, else the model the forwarder sent.
            let device = labels.get(&m.serial).filter(|l| !l.trim().is_empty()).cloned().unwrap_or_else(|| m.device.clone());
            json!({
                "id": m.id,
                "at_ms": m.at_ms,
                "dir": m.dir,
                "from": m.from,
                "from_name": name_of(&m.from),
                "to": m.to,
                "text": m.text,
                "serial": m.serial,
                "device": device,
                "status": m.status,
            })
        })
        .collect();
    Json(json!({
        "ok": true,
        "total": page.total,
        "retention_days": keep_days,
        "has_more": page.has_more,
        "messages": messages,
    }))
    .into_response()
}

async fn sms_delete(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<TokenQuery>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    match id.trim().parse::<u64>() {
        Ok(id) if state.sms.delete(id) => Json(json!({"ok": true})).into_response(),
        _ => api_error(StatusCode::NOT_FOUND, "not_found", "text message not found"),
    }
}

async fn sms_clear(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    // Like clearing the calls: a JSON request cannot come from another site's form.
    if !is_json_request(&headers) {
        return json_required();
    }
    Json(json!({"ok": true, "removed": state.sms.clear()})).into_response()
}

/// The owner now keeps fewer days: what is too old goes right away (in the
/// background; the hourly cleanup would only get to it later).
pub(crate) fn purge_after_fewer_days(state: &CallsState, days_before: u32, days_now: u32) {
    if days_now >= days_before {
        return;
    }
    let (calls, tasks, errands, sms) = (state.store.clone(), state.task_store.clone(), state.errands.clone(), state.sms.clone());
    tokio::spawn(async move { purge_expired_records(&calls, &tasks, &errands, &sms, days_now).await });
}

/// Delete what is older than retention_days (the setting as it is now):
/// calls and their recordings, to-do items, AI errands that are over, text
/// messages. Run every hour and right after the owner keeps fewer days.
pub async fn purge_expired_records(
    calls: &CallStore,
    tasks: &TaskStore,
    errands: &crate::errands::ErrandStore,
    sms: &crate::sms_store::SmsStore,
    retention_days: u32,
) {
    calls.purge_expired(retention_days).await;
    tasks.purge_expired(retention_days).await;
    errands.purge_expired(retention_days).await;
    sms.purge(crate::sms_store::now_ms(), retention_days);
}

/// How long sending a text from the console waits for the phone's answer.
#[cfg(not(test))]
const SMS_SEND_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(test)]
const SMS_SEND_WAIT: std::time::Duration = std::time::Duration::from_millis(800);
/// Longest text the console sends, in characters (what the phone app takes).
const SMS_SEND_MAX_CHARS: usize = 500;

/// A SIM a text can go out from: every SIM the phones reported, and a phone
/// that is connected without a SIM list (its default SIM, sub_id None).
#[derive(Clone, Debug, Default, PartialEq)]
struct SmsLine {
    serial: String,
    sub_id: Option<i64>,
    device: String,
    number: String,
    carrier: String,
    slot: Option<i64>,
    country: String,
    can_send: bool,
}

impl SmsLine {
    fn json(&self) -> Value {
        json!({
            "serial": self.serial,
            "sub_id": self.sub_id,
            "device": self.device,
            "number": self.number,
            "carrier": self.carrier,
            "slot": self.slot,
            "can_send": self.can_send,
        })
    }
}

/// The name of a phone: the owner's (or its app's) label, else the model it
/// reported, else its serial.
fn sms_device_name(state: &CallsState, labels: &HashMap<String, String>, serial: &str) -> String {
    labels
        .get(serial)
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .or_else(|| state.phones.model_of(serial))
        .unwrap_or_else(|| serial.to_string())
}

async fn sms_lines(state: &CallsState) -> Vec<SmsLine> {
    let labels: HashMap<String, String> = crate::notify::known_devices().into_iter().collect();
    let mut lines: Vec<SmsLine> = Vec::new();
    for (serial, list) in state.errands.sims.all_json() {
        for v in list.as_array().into_iter().flatten() {
            let sub_id = v.get("sub_id").and_then(Value::as_i64).filter(|s| *s >= 0);
            // A number saved before the phone listed its SIMs stands for its default SIM.
            if sub_id.is_none() && lines.iter().any(|l| l.serial == serial && l.sub_id.is_none()) {
                continue;
            }
            let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
            lines.push(SmsLine {
                serial: serial.clone(),
                sub_id,
                number: text("effective_number"),
                carrier: text("carrier"),
                country: text("country"),
                slot: v.get("slot").and_then(Value::as_i64),
                ..Default::default()
            });
        }
    }
    for serial in state.control.connected_serials().await {
        if !lines.iter().any(|l| l.serial == serial) {
            lines.push(SmsLine { serial, ..Default::default() });
        }
    }
    for line in &mut lines {
        line.can_send = state.control.can_send_sms(&line.serial).await;
        line.device = sms_device_name(state, &labels, &line.serial);
    }
    lines
}

/// The owner's country, for numbers without a known SIM.
fn sms_home_region() -> Option<String> {
    crate::phone_region::country_of_code(&crate::locale::country_code()).map(str::to_string)
}

/// The country a message's numbers are read in: its SIM's, else the one its
/// phone's SIMs share, else the owner's.
fn sms_region(lines: &[SmsLine], m: &crate::sms_store::SmsMessage, home: Option<&str>) -> Option<String> {
    let phone: Vec<&SmsLine> = lines.iter().filter(|l| l.serial == m.serial && !l.country.is_empty()).collect();
    if let Some(l) = m.sub_id.and_then(|s| phone.iter().find(|l| l.sub_id == Some(s))) {
        return Some(l.country.clone());
    }
    match phone.first() {
        Some(first) if phone.iter().all(|l| l.country == first.country) => Some(first.country.clone()),
        _ => home.map(str::to_string),
    }
}

fn sms_key_of(lines: &[SmsLine], m: &crate::sms_store::SmsMessage, home: Option<&str>) -> String {
    crate::sms_store::thread_key(m.peer(), sms_region(lines, m, home).as_deref())
}

/// The contact name of a message's other party, or "".
fn sms_name_of(state: &CallsState, lines: &[SmsLine], m: &crate::sms_store::SmsMessage, home: Option<&str>) -> String {
    let peer = m.peer().trim();
    if peer.is_empty() {
        return String::new();
    }
    state.contacts.lookup(peer, sms_region(lines, m, home).as_deref()).map(|c| c.name).unwrap_or_default()
}

/// The line a reply goes out on by default: the phone and SIM of the newest
/// message received in the conversation (its SIM found by the number it was
/// sent to when the forwarder did not say, else the phone's first SIM); with
/// none received, the line of the newest message sent. When that line cannot
/// send now, or the conversation is new: the first line that can.
fn sms_reply_line<'a>(
    msgs: &[crate::sms_store::SmsMessage],
    lines: &'a [SmsLine],
    region_of: &dyn Fn(&crate::sms_store::SmsMessage) -> Option<String>,
) -> Option<&'a SmsLine> {
    let first_can = || lines.iter().find(|l| l.can_send);
    let basis = msgs.iter().rev().find(|m| !m.is_out()).or_else(|| msgs.iter().rev().find(|m| m.is_out()));
    let Some(m) = basis else { return first_can() };
    let phone: Vec<&SmsLine> = lines.iter().filter(|l| l.serial == m.serial).collect();
    let picked = match m.sub_id {
        Some(sub) => phone.iter().find(|l| l.sub_id == Some(sub)).copied(),
        None => {
            let region = region_of(m);
            let by_number = (!m.is_out() && !m.to.trim().is_empty())
                .then(|| {
                    phone.iter().find(|l| {
                        !l.number.is_empty() && crate::contacts::same_number(&m.to, &l.number, region.as_deref())
                    })
                })
                .flatten()
                .copied();
            by_number.or_else(|| phone.iter().find(|l| l.sub_id.is_none()).copied())
        }
    }
    .or_else(|| phone.first().copied());
    match picked {
        Some(l) if l.can_send => Some(l),
        _ => first_can(),
    }
}

/// A message for the SMS page.
fn sms_message_json(state: &CallsState, labels: &HashMap<String, String>, m: &crate::sms_store::SmsMessage) -> Value {
    let device = labels
        .get(&m.serial)
        .filter(|l| !l.trim().is_empty())
        .cloned()
        .or_else(|| (!m.device.is_empty()).then(|| m.device.clone()))
        .or_else(|| state.phones.model_of(&m.serial))
        .unwrap_or_default();
    json!({
        "id": m.id,
        "at_ms": m.at_ms,
        "dir": m.dir,
        "from": m.from,
        "to": m.to,
        "text": m.text,
        "serial": m.serial,
        "sub_id": m.sub_id,
        "device": device,
        "status": m.status,
        "error": m.error,
    })
}

#[derive(Deserialize)]
struct SmsThreadsQuery {
    token: Option<String>,
    q: Option<String>,
}

/// The conversations (one per number), newest first, and the lines a text can go out on.
async fn sms_threads(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<SmsThreadsQuery>,
) -> Response {
    if !token_ok_parts(&state.settings, &headers, query.token.as_deref()) {
        return unauthorized();
    }
    let keep_days = state.settings_store.snapshot().retention_days;
    let lines = sms_lines(&state).await;
    let home = sms_home_region();
    let key_of = |m: &crate::sms_store::SmsMessage| sms_key_of(&lines, m, home.as_deref());
    let name_of = |m: &crate::sms_store::SmsMessage| sms_name_of(&state, &lines, m, home.as_deref());
    let (total, threads) = state.sms.threads(
        crate::sms_store::now_ms(),
        keep_days,
        query.q.as_deref().unwrap_or(""),
        &key_of,
        &name_of,
    );
    let threads: Vec<Value> = threads
        .iter()
        .map(|t| {
            let region = sms_region(&lines, &t.last, home.as_deref());
            json!({
                "key": t.key,
                "number": t.number,
                "name": name_of(&t.last),
                "last_at": t.last.at_ms,
                "last_text": t.last.text,
                "last_dir": t.last.dir,
                "last_status": t.last.status,
                "count": t.count,
                "last_in_at": t.last_in_at,
                "can_reply": crate::sms_store::check_send_number(&t.number, region.as_deref()).is_ok(),
            })
        })
        .collect();
    let lines: Vec<Value> = lines.iter().map(SmsLine::json).collect();
    Json(json!({
        "ok": true,
        "total": total,
        "retention_days": keep_days,
        "threads": threads,
        "lines": lines,
    }))
    .into_response()
}

/// The conversation a number (as written, or a conversation key) names: the
/// one with its key, else with that exact key, else the only one whose number
/// ends with the same (at least eight) digits. None: no conversation yet.
fn sms_find_thread(keys: &[String], raw: &str, home: Option<&str>) -> Option<String> {
    let key = crate::sms_store::thread_key(raw, home);
    if keys.contains(&key) {
        return Some(key);
    }
    let lower = raw.trim().to_lowercase();
    if keys.contains(&lower) {
        return Some(lower);
    }
    let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
    let digits = digits.trim_start_matches('0');
    if digits.len() < 8 || raw.chars().any(char::is_alphabetic) {
        return None;
    }
    let mut hits = keys.iter().filter(|k| {
        let kd: String = k.chars().filter(char::is_ascii_digit).collect();
        !k.chars().any(char::is_alphabetic) && kd.ends_with(digits)
    });
    match (hits.next(), hits.next()) {
        (Some(k), None) => Some(k.clone()),
        _ => None,
    }
}

#[derive(Deserialize)]
struct SmsThreadQuery {
    token: Option<String>,
    number: Option<String>,
    before: Option<String>,
    limit: Option<String>,
}

/// One conversation, oldest first, a page at a time (before: an id), and the
/// line a reply goes out on by default. A number with no conversation yet
/// gives an empty one (a new message).
async fn sms_thread(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<SmsThreadQuery>,
) -> Response {
    if !token_ok_parts(&state.settings, &headers, query.token.as_deref()) {
        return unauthorized();
    }
    let keep_days = state.settings_store.snapshot().retention_days;
    let raw = query.number.as_deref().unwrap_or("").trim().to_string();
    let before = query.before.as_deref().and_then(|s| s.trim().parse::<u64>().ok()).filter(|id| *id > 0);
    let limit = query
        .limit
        .as_deref()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(crate::sms_store::PAGE_DEFAULT)
        .clamp(1, crate::sms_store::PAGE_MAX);
    let lines = sms_lines(&state).await;
    let home = sms_home_region();
    let key_of = |m: &crate::sms_store::SmsMessage| sms_key_of(&lines, m, home.as_deref());
    let now = crate::sms_store::now_ms();
    let (_, threads) = state.sms.threads(now, keep_days, "", &key_of, &|_: &crate::sms_store::SmsMessage| String::new());
    let keys: Vec<String> = threads.iter().map(|t| t.key.clone()).collect();
    let key = sms_find_thread(&keys, &raw, home.as_deref()).unwrap_or_else(|| crate::sms_store::thread_key(&raw, home.as_deref()));
    let mut all = state.sms.thread(now, keep_days, &key, &key_of);
    // A text the wait gave up on may have been reported since.
    for m in all.iter_mut().filter(|m| {
        m.is_out() && (m.status == crate::sms_store::STATUS_TIMEOUT || m.status == crate::sms_store::STATUS_SENDING)
    }) {
        if let Some((true, _)) = state.control.sms_outcome(&format!("sms-{}", m.id)).await {
            if let Some(now_sent) = state.sms.set_status(m.id, crate::sms_store::STATUS_SENT, "") {
                *m = now_sent;
            }
        }
    }
    let (number, name) = match all.last() {
        Some(m) => (m.peer().to_string(), sms_name_of(&state, &lines, m, home.as_deref())),
        None => {
            let number = crate::sms_store::sendable_number(&raw).unwrap_or_else(|| raw.clone());
            let name = if number.is_empty() {
                String::new()
            } else {
                state.contacts.lookup(&number, home.as_deref()).map(|c| c.name).unwrap_or_default()
            };
            (number, name)
        }
    };
    let region_of = |m: &crate::sms_store::SmsMessage| sms_region(&lines, m, home.as_deref());
    let reply_region = all.last().map(region_of).unwrap_or_else(|| home.clone());
    let reply = sms_reply_line(&all, &lines, &region_of).map(|l| json!({"serial": l.serial, "sub_id": l.sub_id}));
    let older: Vec<&crate::sms_store::SmsMessage> = all.iter().filter(|m| before.map_or(true, |b| m.id < b)).collect();
    let start = older.len().saturating_sub(limit);
    let labels: HashMap<String, String> = crate::notify::known_devices().into_iter().collect();
    let messages: Vec<Value> = older[start..].iter().map(|m| sms_message_json(&state, &labels, m)).collect();
    Json(json!({
        "ok": true,
        "key": key,
        "number": number,
        "name": name,
        "can_reply": crate::sms_store::check_send_number(&number, reply_region.as_deref()).is_ok(),
        "has_more": start > 0,
        "reply": reply,
        "messages": messages,
    }))
    .into_response()
}

/// Remove one conversation.
async fn sms_thread_delete(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<SmsThreadQuery>,
) -> Response {
    if !token_ok_parts(&state.settings, &headers, query.token.as_deref()) {
        return unauthorized();
    }
    if !is_json_request(&headers) {
        return json_required();
    }
    let keep_days = state.settings_store.snapshot().retention_days;
    let raw = query.number.as_deref().unwrap_or("").trim().to_string();
    let lines = sms_lines(&state).await;
    let home = sms_home_region();
    let key_of = |m: &crate::sms_store::SmsMessage| sms_key_of(&lines, m, home.as_deref());
    let (_, threads) = state.sms.threads(
        crate::sms_store::now_ms(),
        keep_days,
        "",
        &key_of,
        &|_: &crate::sms_store::SmsMessage| String::new(),
    );
    let keys: Vec<String> = threads.iter().map(|t| t.key.clone()).collect();
    // Deleting takes the conversation's own key (or the number it stands for)
    // only: the "same last digits" guess of opening a conversation is left out.
    let exact = crate::sms_store::thread_key(&raw, home.as_deref());
    let lower = raw.to_lowercase();
    let removed = [exact, lower]
        .into_iter()
        .find(|k| keys.contains(k))
        .map(|key| state.sms.delete_thread(&key, &key_of))
        .unwrap_or(0);
    Json(json!({"ok": true, "removed": removed})).into_response()
}

#[derive(Deserialize)]
struct SmsSendBody {
    #[serde(default)]
    to: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    serial: String,
    #[serde(default)]
    sub_id: Option<i64>,
}

/// Have the phone send a stored "sending" message and record what came of it,
/// in a task of its own: a browser that goes away while the phone works on it
/// does not leave the message "sending".
async fn sms_deliver_detached(state: &CallsState, msg: crate::sms_store::SmsMessage) -> crate::sms_store::SmsMessage {
    let before = msg.clone();
    let task_state = state.clone();
    match tokio::spawn(async move { sms_deliver(&task_state, msg).await }).await {
        Ok(done) => done,
        Err(_) => state.sms.get(before.id).unwrap_or(before),
    }
}

/// Have the phone send a stored "sending" message and record what came of it.
async fn sms_deliver(state: &CallsState, msg: crate::sms_store::SmsMessage) -> crate::sms_store::SmsMessage {
    use crate::callback::SmsOutcome;
    use crate::sms_store::{STATUS_FAILED, STATUS_SENT, STATUS_TIMEOUT};
    let outcome = state
        .control
        .send_sms_on(&msg.serial, msg.sub_id, &format!("sms-{}", msg.id), &msg.to, &msg.text, SMS_SEND_WAIT)
        .await;
    let (status, error) = match &outcome {
        SmsOutcome::Sent => (STATUS_SENT, ""),
        SmsOutcome::Timeout => (STATUS_TIMEOUT, ""),
        SmsOutcome::Offline => (STATUS_FAILED, "offline"),
        SmsOutcome::Unsupported => (STATUS_FAILED, "unsupported"),
        // The same message is already on its way: it stays "sending".
        SmsOutcome::Busy => {
            info!(serial = msg.serial.as_str(), "console text message already being sent");
            return state.sms.get(msg.id).unwrap_or(msg);
        }
        SmsOutcome::Failed(code) => (STATUS_FAILED, code.as_str()),
    };
    info!(
        serial = msg.serial.as_str(),
        to = crate::urgent_call::masked(&msg.to).as_str(),
        status,
        error,
        "console text message"
    );
    state.sms.set_status(msg.id, status, error).unwrap_or(msg)
}

/// Send a text from a phone's SIM and wait for the phone's answer (up to 30 s).
async fn sms_send(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    body: axum::body::Bytes,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    if !is_json_request(&headers) {
        return json_required();
    }
    let req = serde_json::from_slice::<Value>(&body)
        .ok()
        .filter(Value::is_object)
        .and_then(|v| serde_json::from_value::<SmsSendBody>(v).ok());
    let Some(req) = req else {
        return api_error(StatusCode::BAD_REQUEST, "bad_request", "the body must be {to, text, serial, sub_id}");
    };
    let text = req.text.trim();
    if text.is_empty() || text.chars().count() > SMS_SEND_MAX_CHARS {
        return api_error(StatusCode::BAD_REQUEST, "bad_text", "the text must have 1 to 500 characters");
    }
    let Some(to) = crate::sms_store::sendable_number(&req.to) else {
        return api_error(StatusCode::BAD_REQUEST, "bad_number", "not a number a text can go to");
    };
    if to.trim_start_matches('+').len() <= 7 {
        return api_error(StatusCode::BAD_REQUEST, "not_allowed", "cannot text emergency, short or premium-rate numbers");
    }
    let lines = sms_lines(&state).await;
    let serial = req.serial.trim();
    let Some(line) = lines.iter().find(|l| l.serial == serial && l.sub_id == req.sub_id) else {
        return api_error(StatusCode::BAD_REQUEST, "bad_line", "no such phone and SIM");
    };
    if !line.can_send {
        return api_error(StatusCode::CONFLICT, "line_unavailable", "that phone cannot send texts right now");
    }
    let home = sms_home_region();
    let region = (!line.country.trim().is_empty()).then_some(line.country.as_str()).or(home.as_deref());
    if let Err(code) = crate::sms_store::check_send_number(&to, region) {
        let msg = if code == "not_allowed" {
            "cannot text emergency, short or premium-rate numbers"
        } else {
            "not a number a text can go to"
        };
        return api_error(StatusCode::BAD_REQUEST, code, msg);
    }
    let keep_days = state.settings_store.snapshot().retention_days;
    let msg = state.sms.push_out(
        crate::sms_store::now_ms(),
        keep_days,
        crate::sms_store::NewOut {
            to: &to,
            text,
            from: &line.number,
            serial: &line.serial,
            device: &line.device,
            sub_id: line.sub_id,
        },
    );
    let msg = sms_deliver_detached(&state, msg).await;
    let labels: HashMap<String, String> = crate::notify::known_devices().into_iter().collect();
    Json(json!({"ok": msg.status == crate::sms_store::STATUS_SENT, "message": sms_message_json(&state, &labels, &msg)}))
        .into_response()
}

/// Send a text that failed (or whose result never came) again, on the same line.
async fn sms_retry(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<TokenQuery>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    if !is_json_request(&headers) {
        return json_required();
    }
    let Some(msg) = id.trim().parse::<u64>().ok().and_then(|id| state.sms.get(id)) else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "text message not found");
    };
    let retryable = msg.is_out()
        && (msg.status == crate::sms_store::STATUS_FAILED || msg.status == crate::sms_store::STATUS_TIMEOUT);
    if !retryable {
        return api_error(StatusCode::CONFLICT, "not_retryable", "only a text that was not sent can be sent again");
    }
    let Some(msg) = state.sms.set_status(msg.id, crate::sms_store::STATUS_SENDING, "") else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "text message not found");
    };
    let msg = sms_deliver_detached(&state, msg).await;
    let labels: HashMap<String, String> = crate::notify::known_devices().into_iter().collect();
    Json(json!({"ok": msg.status == crate::sms_store::STATUS_SENT, "message": sms_message_json(&state, &labels, &msg)}))
        .into_response()
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
        // Known SIMs per phone: sub_id, country, slot, carrier, auto_number, number, effective_number.
        "sims": state.errands.sims.all_json(),
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
    /// Connected over the network (Wi-Fi) rather than the USB tunnel.
    wifi: bool,
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
        // How it is connected now: "usb" or "wifi".
        "link": if p.wifi { "wifi" } else { "usb" },
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
                wifi: false,
            },
        );
    }

    /// Whether the phone's control connection came over the network (Wi-Fi).
    pub fn set_wifi(&self, serial: &str, wifi: bool) {
        if let Some(p) = self.phones().get_mut(serial) {
            p.wifi = wifi;
        }
    }

    /// Phones connected over Wi-Fi now (for `/health`).
    pub fn wifi_count(&self) -> usize {
        self.phones()
            .iter()
            .filter(|(serial, p)| p.sink.is_some() && p.wifi && !crate::calls::is_twin_serial(serial))
            .count()
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

    /// Whether the phone currently holds an open control connection here.
    pub fn is_connected(&self, serial: &str) -> bool {
        self.phones()
            .get(serial)
            .is_some_and(|p| p.sink.is_some())
    }

    /// The model the phone reported in its last hello, when it did.
    pub fn model_of(&self, serial: &str) -> Option<String> {
        self.phones()
            .get(serial)
            .map(|p| p.hello.model.trim().to_string())
            .filter(|m| !m.is_empty())
    }

    /// Forget an offline phone from `PhoneDevices`. Returns `Err("online")` if
    /// its control connection is still open, `Ok(true)` if removed, `Ok(false)`
    /// if not present.
    pub fn forget(&self, serial: &str) -> Result<bool, &'static str> {
        let mut phones = self.phones();
        if phones.get(serial).is_some_and(|p| p.sink.is_some()) {
            return Err("online");
        }
        Ok(phones.remove(serial).is_some())
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

/// `POST /api/phone-pairing/renew`, "Pair phones again": a new pairing key
/// replaces the old one and the phones on Wi-Fi are dropped; each one is
/// plugged in by USB once more to get the new key. The key is never returned.
async fn phone_pairing_renew(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
) -> Response {
    phone_pairing_renew_with(&state.settings, &headers, &query, crate::phone_wifi::pairing())
}

fn phone_pairing_renew_with(
    settings: &Settings,
    headers: &HeaderMap,
    query: &TokenQuery,
    pairing: &crate::phone_wifi::Pairing,
) -> Response {
    if !token_ok(settings, headers, query) {
        return unauthorized();
    }
    if !is_json_request(headers) {
        return json_required();
    }
    match pairing.renew() {
        Ok(()) => {
            info!("phones paired again: the phones on Wi-Fi must be plugged in by USB once more");
            Json(json!({"ok": true})).into_response()
        }
        Err(e) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "save_failed",
            format!("the new pairing could not be saved: {e}"),
        ),
    }
}

/// `POST /api/phone-pairing/code`, "Get a pairing code": a 6-digit code for
/// pairing a phone by IP address ([`crate::phone_wifi::PairCodes`]); a new one
/// replaces the one before. With it, the agent's LAN address for the console to
/// offer when it was opened as `localhost` or `bageo.local` (none in a container).
async fn phone_pairing_code(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    ext: Extensions,
) -> Response {
    let on = state.settings_store.snapshot().phone_wifi;
    let here = crate::lan::from_this_computer(&ext, &headers);
    phone_pairing_code_with(&state.settings, &headers, &query, on, here, crate::phone_wifi::codes())
}

/// `from_here`: the request comes from the computer Bageo runs on. Without a
/// console password only that computer may get a code (a code is a way to the
/// pairing key, so the open console of a container must not hand it to anyone
/// on the network).
fn phone_pairing_code_with(
    settings: &Settings,
    headers: &HeaderMap,
    query: &TokenQuery,
    phone_wifi_on: bool,
    from_here: bool,
    codes: &crate::phone_wifi::PairCodes,
) -> Response {
    if !token_ok(settings, headers, query) {
        return unauthorized();
    }
    if !is_json_request(headers) {
        return json_required();
    }
    let has_password = !crate::settings_store::active_ui_token(&settings.calls_ui_token).trim().is_empty();
    if !has_password && !from_here {
        return api_error(
            StatusCode::FORBIDDEN,
            "need_password",
            "set a console password first to get a pairing code from another device",
        );
    }
    if !phone_wifi_on {
        return api_error(StatusCode::CONFLICT, "phone_wifi_off", "phones over Wi-Fi are turned off");
    }
    let code = codes.issue(std::time::Instant::now());
    let in_container = crate::settings_store::in_container();
    let lan_ips: Vec<String> = if in_container {
        Vec::new()
    } else {
        crate::mdns::lan_ipv4().map(|ip| ip.to_string()).into_iter().collect()
    };
    info!("phone pairing: a pairing code was made (good for 10 minutes)");
    Json(json!({
        "ok": true,
        "code": code,
        "expires_in": crate::phone_wifi::CODE_TTL.as_secs(),
        "port": settings.port,
        "lan_ips": lan_ips,
        "in_container": in_container,
    }))
    .into_response()
}

/// `DELETE /api/devices/{serial}`: forget an offline phone from `PhoneDevices`,
/// `notify` labels, `phone_sims.json`, and `settings.json` (`device_labels`),
/// keeping past call records intact. Connected or busy phones are refused.
async fn device_delete(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    Path(serial): Path<String>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    if !task_id_safe(&serial) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_serial", "invalid phone serial");
    }
    let connected = state.control.connected_serials().await;
    if connected.iter().any(|s| s == &serial) || state.phones.is_connected(&serial) {
        return api_error(
            StatusCode::CONFLICT,
            "device_online",
            "cannot remove a connected phone",
        );
    }
    let in_call = state
        .store
        .live_calls()
        .await
        .iter()
        .any(|c| c.serial == serial)
        || state.phones.in_call(&serial)
        || crate::appointments::outbound_busy_on(&serial);
    if in_call {
        return api_error(
            StatusCode::CONFLICT,
            "device_in_call",
            "cannot remove a phone while a call is in progress",
        );
    }
    let from_phones = match state.phones.forget(&serial) {
        Ok(v) => v,
        Err(_) => {
            return api_error(
                StatusCode::CONFLICT,
                "device_online",
                "cannot remove a connected phone",
            )
        }
    };
    let from_label = match state.settings_store.remove_device_label(&serial) {
        Ok(v) => v,
        Err(e) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "save_failed",
                format!("{e:#}"),
            )
        }
    };
    let from_notify = crate::notify::forget_device(&serial);
    let from_sims =
        state.errands.sims.forget_phone(&serial) | crate::errands::forget_phone_sims(&serial);
    if !(from_phones || from_label || from_notify || from_sims) {
        return api_error(StatusCode::NOT_FOUND, "not_found", "phone not found");
    }
    info!(serial = serial.as_str(), "offline phone forgotten");
    Json(json!({"ok": true, "serial": serial})).into_response()
}

/// `PUT /api/devices/{serial}/sim-number`: save (or clear when `number` is empty)
/// the owner-entered phone number for a SIM on `serial` (`sub_id >= 0`, or `-1`
/// when the phone has not reported a SIM list yet).
async fn device_sim_number(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    Path(serial): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    if !is_json_request(&headers) {
        return json_required();
    }
    if !task_id_safe(&serial) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_serial", "invalid phone serial");
    }
    let Ok(v) = serde_json::from_slice::<Value>(&body) else {
        return api_error(StatusCode::BAD_REQUEST, "bad_request", "invalid JSON body");
    };
    let sub_id = v.get("sub_id").and_then(Value::as_i64).unwrap_or(-1);
    let number = match v.get("number") {
        None | Some(Value::Null) => "",
        Some(Value::String(s)) => s.as_str(),
        Some(_) => return api_error(StatusCode::BAD_REQUEST, "bad_number", "number must be a string"),
    };
    let country_hint = v.get("country").and_then(Value::as_str);
    let default_country = state.settings_store.snapshot().default_region().to_string();
    let hint = country_hint.unwrap_or(&default_country);
    match state.errands.sims.set_number(&serial, sub_id, Some(hint), number) {
        Ok(sims) => {
            info!(serial = serial.as_str(), sub_id, cleared = number.trim().is_empty(), "phone SIM number updated");
            Json(json!({"ok": true, "serial": serial, "sims": sims})).into_response()
        }
        Err("sim_not_found") => api_error(StatusCode::NOT_FOUND, "sim_not_found", "SIM not found on this phone"),
        Err(code) => api_error(StatusCode::BAD_REQUEST, code, "invalid phone number"),
    }
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
    let tasks: Vec<Value> = state
        .task_store
        .list(200)
        .await
        .iter()
        .map(|t| task_json(t, &rt))
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
    let console = crate::settings_store::effective_console_url(&rt.effective(&state.settings));
    Some(
        match crate::appointments::decide_from_web(
            &state.http,
            rt,
            &console,
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
                Json(json!({"ok": true, "task": task_json(&task, rt), "message": message})).into_response()
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

/// Public auth status endpoint so the web console knows whether a password is
/// required, whether the current token is valid, or whether to show the
/// first-time external access password setup dialog.
async fn auth_status(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
) -> Response {
    let rt = state.settings_store.snapshot();
    let active_tok = crate::settings_store::active_ui_token(&state.settings.calls_ui_token);
    let password_set = !active_tok.trim().is_empty();
    let authenticated = token_ok(&state.settings, &headers, &query);
    let prompt_initial = !password_set && !rt.ui_password_prompted;
    Json(json!({
        "ok": true,
        "password_set": password_set,
        "authenticated": authenticated,
        "prompt_initial": prompt_initial,
        "language": rt.language,
    }))
    .into_response()
}

/// First-time password setup (`POST /calls/api/initial-password`): allowed when
/// no password is set yet, or when already authenticated.
async fn initial_password(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    body: axum::body::Bytes,
) -> Response {
    if !is_json_request(&headers) {
        return json_required();
    }
    let active_tok = crate::settings_store::active_ui_token(&state.settings.calls_ui_token);
    if !active_tok.trim().is_empty() && !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    let Ok(v) = serde_json::from_slice::<Value>(&body) else {
        return api_error(StatusCode::BAD_REQUEST, "bad_request", "invalid JSON");
    };
    let skip = v.get("skip").and_then(Value::as_bool).unwrap_or(false);
    let pw = v
        .get("password")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    let patch = if skip {
        json!({ "ui_password_prompted": true })
    } else if pw.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "bad_request", "password cannot be empty");
    } else if crate::settings_store::ui_password_too_short(pw) {
        return password_too_short();
    } else {
        json!({
            "ui_password": pw,
            "ui_password_prompted": true
        })
    };
    match state.settings_store.apply_patch(&patch) {
        Ok(_) => Json(json!({
            "ok": true,
            "password_set": !skip && !pw.is_empty(),
        }))
        .into_response(),
        Err(e) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "save_failed",
            format!("{e:#}"),
        ),
    }
}

/// 400 `password_too_short`: a new console password needs
/// [`crate::settings_store::MIN_UI_PASSWORD_CHARS`] characters.
fn password_too_short() -> Response {
    api_error(
        StatusCode::BAD_REQUEST,
        "password_too_short",
        format!("the console password needs at least {} characters", crate::settings_store::MIN_UI_PASSWORD_CHARS),
    )
}

#[derive(Deserialize)]
struct QuickActionQuery {
    #[serde(default)]
    op: String,
    #[serde(default)]
    target: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    sig: String,
    #[serde(default)]
    exp: String,
    #[serde(default)]
    token: Option<String>,
}

/// The words of the one-tap link pages, in the owner's language.
struct QuickWords {
    ask_title: &'static str,
    ask_button: &'static str,
    expired_title: &'static str,
    expired_detail: &'static str,
    invalid_title: &'static str,
    invalid_detail: &'static str,
    block: &'static str,
    stall: &'static str,
    white: &'static str,
    unblock: &'static str,
}

fn quick_words(lang: crate::locale::Lang) -> QuickWords {
    use crate::locale::Lang;
    let w = |ask_title, ask_button, expired_title, expired_detail, invalid_title, invalid_detail, block, stall, white, unblock| QuickWords {
        ask_title,
        ask_button,
        expired_title,
        expired_detail,
        invalid_title,
        invalid_detail,
        block,
        stall,
        white,
        unblock,
    };
    match lang {
        Lang::ZhTw => w("確定要這樣做嗎？", "確定", "連結已過期", "請打開控制台處理。", "連結無效", "這個連結不能用，請打開控制台處理。", "加入黑名單", "設為陪聊", "加入白名單", "移出名單"),
        Lang::ZhCn => w("确定要这样做吗？", "确定", "链接已过期", "请打开控制台处理。", "链接无效", "这个链接不能用，请打开控制台处理。", "加入黑名单", "设为陪聊", "加入白名单", "移出名单"),
        Lang::Ja => w("この操作を実行しますか？", "実行する", "リンクの有効期限が切れています", "コンソールを開いて操作してください。", "このリンクは使えません", "コンソールを開いて操作してください。", "着信拒否", "引き延ばし", "ホワイトリスト", "リストから外す"),
        Lang::Ko => w("이 작업을 실행할까요?", "실행", "링크가 만료되었습니다", "콘솔을 열어 처리하세요.", "사용할 수 없는 링크입니다", "콘솔을 열어 처리하세요.", "차단 목록", "시간 끌기", "화이트리스트", "목록에서 제외"),
        Lang::Es => w("¿Hacer esto?", "Confirmar", "El enlace caducó", "Abre la consola para hacerlo allí.", "Este enlace no funciona", "Abre la consola para hacerlo allí.", "Lista negra", "Entretener", "Lista blanca", "Quitar de la lista"),
        Lang::Fr => w("Faire ceci ?", "Confirmer", "Ce lien a expiré", "Ouvrez la console pour le faire.", "Ce lien ne fonctionne pas", "Ouvrez la console pour le faire.", "Liste noire", "Les faire parler", "Liste blanche", "Retirer de la liste"),
        Lang::De => w("Wirklich ausführen?", "Bestätigen", "Dieser Link ist abgelaufen", "Öffne die Konsole und erledige es dort.", "Dieser Link funktioniert nicht", "Öffne die Konsole und erledige es dort.", "Sperrliste", "Hinhalten", "Whitelist", "Von der Liste nehmen"),
        Lang::Pt => w("Fazer isto?", "Confirmar", "Este link expirou", "Abra o console para fazer por lá.", "Este link não funciona", "Abra o console para fazer por lá.", "Bloquear", "Manter falando", "Lista branca", "Tirar da lista"),
        Lang::En => w("Do this?", "Confirm", "This link has expired", "Open the console to do it there.", "This link doesn't work", "Open the console to do it there.", "Block", "Stall", "Whitelist", "Remove from list"),
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn quick_action_html(title: &str, detail: &str, ok: bool) -> Response {
    quick_action_page(
        title,
        detail,
        if ok { StatusCode::OK } else { StatusCode::BAD_REQUEST },
        "",
    )
}

/// One small page for a one-tap link. `form`: HTML put above the console
/// button (the confirm form), already escaped.
fn quick_action_page(title: &str, detail: &str, status: StatusCode, form: &str) -> Response {
    use crate::locale::Lang;
    let lang = crate::locale::notify_lang();
    let (html_lang, open_lbl) = match lang {
        Lang::ZhTw => ("zh-Hant", "開啟 Bageo 控制台"),
        Lang::ZhCn => ("zh-Hans", "打开 Bageo 控制台"),
        Lang::Ja => ("ja", "Bageo コンソールを開く"),
        Lang::Ko => ("ko", "Bageo 콘솔 열기"),
        Lang::Es => ("es", "Abrir la consola de Bageo"),
        Lang::Fr => ("fr", "Ouvrir la console Bageo"),
        Lang::De => ("de", "Bageo-Konsole öffnen"),
        Lang::Pt => ("pt-BR", "Abrir o console do Bageo"),
        Lang::En => ("en", "Open Bageo Console"),
    };
    let tone = if status.is_success() { "" } else { " warn" };
    let esc = html_escape;
    let html = format!(
        "<!doctype html><html lang=\"{html_lang}\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{} — Bageo</title>\
         <style>body{{font-family:system-ui,-apple-system,sans-serif;background:#faf8f5;color:#1f1e1b;display:flex;align-items:center;justify-content:center;min-height:90vh;margin:0;padding:16px}}\
         .card{{background:#fff;border:1px solid #e6e1d6;border-radius:14px;max-width:420px;width:100%;padding:24px;box-shadow:0 4px 16px rgba(0,0,0,.05);text-align:center}}\
         h1{{font-size:18px;margin:0 0 8px}}.warn h1{{color:#b3261e}}p{{font-size:14px;color:#5c584f;margin:0 0 18px;word-break:break-word}}\
         a.btn{{display:inline-block;background:#2f5e4e;color:#fff;text-decoration:none;padding:9px 18px;border-radius:8px;font-size:14px;font-weight:600}}\
         form{{margin:0 0 14px}}button{{background:#2f5e4e;color:#fff;border:0;padding:11px 26px;border-radius:8px;font-size:15px;font-weight:600;cursor:pointer}}\
         form+a.btn{{background:#fff;color:#2f5e4e;border:1px solid #2f5e4e}}</style></head>\
         <body><div class=\"card{tone}\"><h1>{}</h1><p>{}</p>{form}<a class=\"btn\" href=\"/calls/\">{open_lbl}</a></div></body></html>",
        esc(title),
        esc(title),
        esc(detail)
    );
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-frame-options", "DENY")
        .header("content-security-policy", "frame-ancestors 'none'")
        .header("referrer-policy", "no-referrer")
        .body(Body::from(html))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Who may use a one-tap link: the console password (only when one is set),
/// else the link's own signature and time ([`crate::calendar::check_quick_action`]).
fn quick_action_access(settings: &Settings, headers: &HeaderMap, q: &QuickActionQuery) -> crate::calendar::QuickLink {
    let has_password = !crate::settings_store::active_ui_token(&settings.calls_ui_token).trim().is_empty();
    if has_password && token_ok_parts(settings, headers, q.token.as_deref()) {
        return crate::calendar::QuickLink::Valid;
    }
    crate::calendar::check_quick_action(
        &q.op,
        &q.target,
        q.name.as_deref().unwrap_or(""),
        &q.exp,
        &q.sig,
    )
}

/// Signed one-tap action endpoint (`/calls/api/quick-action`) used by the
/// buttons and links of every notification channel. Opening it (GET: a tap,
/// a chat app's link preview, an image on another site) only shows what it
/// would do and a button; that button (POST) does it.
async fn quick_action(
    State(state): State<CallsState>,
    method: axum::http::Method,
    headers: HeaderMap,
    axum::extract::RawQuery(raw_query): axum::extract::RawQuery,
    Query(q): Query<QuickActionQuery>,
) -> Response {
    let access = quick_action_access(&state.settings, &headers, &q);
    quick_action_run(state, method, headers, raw_query, q, access).await
}

/// [`quick_action`] once its access is known.
async fn quick_action_run(
    state: CallsState,
    method: axum::http::Method,
    headers: HeaderMap,
    raw_query: Option<String>,
    q: QuickActionQuery,
    access: crate::calendar::QuickLink,
) -> Response {
    use crate::locale::Lang;
    let op = q.op.trim();
    let target = q.target.trim();
    let wants_json = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| s.contains("application/json"));
    let lang = crate::locale::notify_lang();
    let qw = quick_words(lang);
    match access {
        crate::calendar::QuickLink::Valid => {}
        crate::calendar::QuickLink::Expired => {
            if wants_json {
                return (
                    StatusCode::GONE,
                    Json(json!({"ok": false, "code": "link_expired", "title": qw.expired_title, "message": qw.expired_detail})),
                )
                    .into_response();
            }
            return quick_action_page(qw.expired_title, qw.expired_detail, StatusCode::GONE, "");
        }
        crate::calendar::QuickLink::Invalid => {
            if wants_json {
                return unauthorized();
            }
            return quick_action_page(qw.invalid_title, qw.invalid_detail, StatusCode::UNAUTHORIZED, "");
        }
    }
    if method != axum::http::Method::POST {
        let w = crate::control::words::words(lang);
        let p = crate::locale::phrases(lang);
        let name = q.name.as_deref().unwrap_or("").trim();
        let detail = match op {
            "confirm_task" | "ignore_task" => {
                let label = if op == "confirm_task" { p.ap_confirm } else { p.ap_decline };
                let about = match state.task_store.get(target).await {
                    Some(t) => t.summary.lines().next().unwrap_or("").chars().take(200).collect::<String>(),
                    None => String::new(),
                };
                if about.trim().is_empty() {
                    label.to_string()
                } else {
                    format!("{label}{}{}", crate::locale::kv_sep(), about.trim())
                }
            }
            "block" | "stall" | "white" | "unblock" => {
                let label = match op {
                    "block" => qw.block,
                    "stall" => qw.stall,
                    "white" => qw.white,
                    _ => qw.unblock,
                };
                format!("{label}{}{}", crate::locale::kv_sep(), crate::control::words::person(w, name, target))
            }
            _ => op.to_string(),
        };
        if wants_json {
            return (
                StatusCode::OK,
                Json(json!({"ok": true, "needs_confirmation": true, "title": qw.ask_title, "message": detail})),
            )
                .into_response();
        }
        let action = format!("?{}", raw_query.as_deref().unwrap_or(""));
        let form = format!(
            "<form method=\"post\" action=\"{}\"><button type=\"submit\">{}</button></form>",
            html_escape(&action),
            html_escape(qw.ask_button)
        );
        return quick_action_page(qw.ask_title, &detail, StatusCode::OK, &form);
    }
    let reply_out = |ok: bool, title: &str, detail: &str| -> Response {
        if wants_json {
            (
                if ok { StatusCode::OK } else { StatusCode::BAD_REQUEST },
                Json(json!({"ok": ok, "title": title, "message": detail})),
            )
                .into_response()
        } else {
            quick_action_html(title, detail, ok)
        }
    };
    let w = crate::control::words::words(lang);
    let rt = state.settings_store.snapshot();
    match op {
        "confirm_task" | "ignore_task" => {
            if !task_id_safe(target) {
                return reply_out(false, &crate::control::words::fmt(w.not_found, &[target]), target);
            }
            let confirm = op == "confirm_task";
            let p = crate::locale::phrases(lang);
            if let Some(task) = state.task_store.get(target).await {
                if task.appointment.is_some() {
                    let console =
                        crate::settings_store::effective_console_url(&rt.effective(&state.settings));
                    return match crate::appointments::decide_from_web(
                        &state.http,
                        &rt,
                        &console,
                        &state.task_store,
                        &state.store,
                        Some(state.control.as_ref()),
                        target,
                        confirm,
                        None,
                    )
                    .await
                    {
                        Ok((_, msg)) => reply_out(
                            true,
                            if confirm { p.ap_confirm } else { p.ap_decline },
                            &msg,
                        ),
                        Err(e) => reply_out(false, &crate::control::words::fmt(w.failed, &[""]), &format!("{e:#}")),
                    };
                }
            }
            let res = if confirm {
                on_confirm(
                    &state.http,
                    &rt,
                    &state.task_store,
                    target,
                    Some(state.control.as_ref()),
                )
                .await
            } else {
                on_ignore(&state.task_store, target).await
            };
            match res {
                Ok(t) => reply_out(
                    true,
                    if confirm { p.ap_confirm } else { p.ap_decline },
                    &t.summary,
                ),
                Err(e) => reply_out(false, &crate::control::words::fmt(w.failed, &[""]), &format!("{e:#}")),
            }
        }
        "block" | "stall" | "white" => {
            let (kind, tmpl) = match op {
                "block" => ("blacklist", w.list_blocked),
                "stall" => ("stall", w.list_stall),
                _ => ("whitelist", w.list_white),
            };
            let name = q.name.as_deref().unwrap_or("");
            match state.contacts.upsert_by_number(target, name, "", kind).await {
                Ok(c) => {
                    let num = c.phones.first().map(String::as_str).unwrap_or(target);
                    let who = crate::control::words::person(w, &c.name, num);
                    let title = crate::control::words::fmt(tmpl, &[&who]);
                    reply_out(true, &title, &who)
                }
                Err(_) => reply_out(false, w.e_bad_number, "bad_number"),
            }
        }
        "unblock" => {
            let _ = state.contacts.unblock(target, &["blacklist", "stall", "whitelist"]).await;
            let title = crate::control::words::fmt(w.list_unblocked, &[target]);
            reply_out(true, &title, target)
        }
        _ => {
            let unknown = match lang {
                Lang::ZhTw => "未知的操作",
                Lang::ZhCn => "未知的操作",
                Lang::Ja => "不明な操作",
                Lang::Ko => "알 수 없는 작업",
                Lang::Es => "Operación desconocida",
                Lang::Fr => "Opération inconnue",
                Lang::De => "Unbekannte Aktion",
                Lang::Pt => "Operação desconhecida",
                Lang::En => "Unknown operation",
            };
            reply_out(false, unknown, op)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quick_state(root: &std::path::Path) -> CallsState {
        let mut base = Settings::from_env();
        base.calls_data_dir = root.to_string_lossy().into_owned();
        base.telegram_bot_token = String::new();
        base.telegram_chat_id = String::new();
        CallsState {
            settings: base.clone(),
            store: crate::calls::CallStore::new(root.join("calls"), true, false),
            settings_store: SettingsStore::load(base),
            task_store: TaskStore::new(root.join("tasks")),
            http: reqwest::Client::new(),
            control: ControlRegistry::new(),
            ambience: Arc::new(crate::ambience::AmbienceLibrary::load(root, 16000)),
            previews: Arc::new(crate::voice_preview::VoicePreviews::new(root.join("voice_preview"), root.join("voices"))),
            phones: PhoneDevices::new(),
            contacts: crate::contacts::ContactStore::open(root.join("contacts.json")),
            errands: crate::errands::ErrandStore::open(root.join("errands.json"), crate::errands::SimBook::in_memory()),
            sms: crate::sms_store::SmsStore::open(root.join("sms"), crate::sms_store::now_ms(), 15),
        }
    }

    fn quick_query(op: &str, target: &str) -> QuickActionQuery {
        QuickActionQuery {
            op: op.into(),
            target: target.into(),
            name: Some("測試店家".into()),
            sig: "x".into(),
            exp: "1".into(),
            token: None,
        }
    }

    async fn body_text(r: Response) -> String {
        let b = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
        String::from_utf8_lossy(&b).into_owned()
    }

    /// The page's style sheet is one block closed once: no CSS text shows on the page, and the
    /// button's rules are inside it.
    #[tokio::test]
    async fn the_one_tap_page_closes_its_style_block_once_after_all_the_rules() {
        let html = body_text(quick_action_page("Done", "ok", StatusCode::OK, "<form></form>")).await;
        assert_eq!(html.matches("</style>").count(), 1, "{html}");
        assert_eq!(html.matches("</head>").count(), 1, "{html}");
        let style_end = html.find("</style>").unwrap();
        assert!(html.find("form+a.btn{").unwrap() < style_end, "{html}");
        assert!(html.find("button{background").unwrap() < style_end, "{html}");
        assert!(html[style_end..].starts_with("</style></head><body>"), "{html}");
    }

    /// Opening a one-tap link (a chat app's link preview, an image on another
    /// site) only shows the question; the page's button (POST) does it.
    #[tokio::test]
    async fn quick_action_get_only_asks_and_post_does_it() {
        let root = std::env::temp_dir().join(format!("bageo-qa-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        let state = quick_state(&root);
        let number = "0912000777";
        let raw = Some("op=white&target=0912000777&exp=1&sig=x".to_string());
        let r = quick_action_run(
            state.clone(),
            axum::http::Method::GET,
            HeaderMap::new(),
            raw.clone(),
            quick_query("white", number),
            crate::calendar::QuickLink::Valid,
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        let page = body_text(r).await;
        assert!(page.contains("<form method=\"post\" action=\"?op=white&amp;target=0912000777&amp;exp=1&amp;sig=x\">"), "{page}");
        assert!(state.contacts.lookup(number, None).is_none(), "a GET changed the list");
        // Anything else than a valid link: nothing done, a clear page.
        for access in [crate::calendar::QuickLink::Expired, crate::calendar::QuickLink::Invalid] {
            let r = quick_action_run(
                state.clone(),
                axum::http::Method::POST,
                HeaderMap::new(),
                raw.clone(),
                quick_query("white", number),
                access,
            )
            .await;
            assert!(r.status() == StatusCode::GONE || r.status() == StatusCode::UNAUTHORIZED, "{:?}", r.status());
            assert!(state.contacts.lookup(number, None).is_none());
        }
        let r = quick_action_run(
            state.clone(),
            axum::http::Method::POST,
            HeaderMap::new(),
            raw,
            quick_query("white", number),
            crate::calendar::QuickLink::Valid,
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(state.contacts.lookup(number, None).map(|c| c.kind), Some("whitelist".to_string()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_ics_link_with_a_wrong_or_old_key_is_answered_as_not_found() {
        let what = "call/c1";
        let good = crate::calendar::link_key(what);
        assert_eq!(ics_access(false, Some(&good), what), IcsAccess::Open);
        assert_eq!(ics_access(true, None, what), IcsAccess::Open);
        // Another file's key, an older 16-byte key, a changed key: not found.
        assert_eq!(ics_access(false, Some(&good), "call/c2"), IcsAccess::BadKey);
        assert_eq!(ics_access(false, Some("0123456789abcdef0123456789abcdef"), what), IcsAccess::BadKey);
        assert_eq!(ics_access(false, Some(&good[..63]), what), IcsAccess::BadKey);
        // No key and no token: the console's own answer.
        assert_eq!(ics_access(false, None, what), IcsAccess::NoAccess);
        assert_eq!(ics_access(false, Some(" "), what), IcsAccess::NoAccess);
    }

    #[test]
    fn quick_link_pages_have_words_in_every_language() {
        for l in crate::locale::ALL {
            let w = quick_words(l);
            for s in [w.ask_title, w.ask_button, w.expired_title, w.expired_detail, w.invalid_title, w.invalid_detail, w.block, w.stall, w.white, w.unblock] {
                assert!(!s.trim().is_empty(), "{l:?}");
            }
        }
    }

    #[test]
    fn csv_cells_that_look_like_formulas_stay_text() {
        assert_eq!(csv_escape("=HYPERLINK(\"http://x\")"), "\"'=HYPERLINK(\"\"http://x\"\")\"");
        assert_eq!(csv_escape("+886912000000"), "'+886912000000");
        assert_eq!(csv_escape("-1+2"), "'-1+2");
        assert_eq!(csv_escape("@SUM(A1)"), "'@SUM(A1)");
        assert_eq!(csv_escape("\t=1"), "\"'\t=1\"");
        assert_eq!(csv_escape("\r=1"), "\"'\r=1\"");
        assert_eq!(csv_escape("0912000000"), "0912000000");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
    }

    #[test]
    fn a_proxied_request_is_recognized() {
        assert!(!came_through_proxy(&HeaderMap::new()));
        for h in ["x-forwarded-for", "forwarded", "x-real-ip", "cf-connecting-ip"] {
            let mut m = HeaderMap::new();
            m.insert(h, "203.0.113.5".parse().unwrap());
            assert!(came_through_proxy(&m), "{h}");
        }
    }

    #[test]
    fn redirects_are_followed_only_on_the_same_host() {
        let u = |s: &str| reqwest::Url::parse(s).unwrap();
        assert!(redirect_stays(&u("https://api.example.org/v1/a"), &u("https://api.example.org/v1/b")));
        assert!(redirect_stays(&u("http://api.example.org/a"), &u("https://API.example.org/a")));
        assert!(!redirect_stays(&u("https://api.example.org/a"), &u("https://evil.example.net/a")));
        assert!(!redirect_stays(&u("https://api.example.org/a"), &u("http://api.example.org/a")));
        assert!(!redirect_stays(&u("https://api.example.org/a"), &u("https://api.example.org:8443/a")));
    }

    /// A key sent in a header never reaches the host a redirect points at.
    #[tokio::test]
    async fn a_redirect_to_another_host_does_not_get_the_key() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let other = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let other_port = other.local_addr().unwrap().port();
        let hit = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hit2 = hit.clone();
        tokio::spawn(async move {
            if let Ok((mut s, _)) = other.accept().await {
                hit2.store(true, std::sync::atomic::Ordering::SeqCst);
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf).await;
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n").await;
            }
        });
        let first = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let first_port = first.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut s, _)) = first.accept().await {
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf).await;
                // "localhost" is another host name than 127.0.0.1.
                let reply = format!("HTTP/1.1 307 Temporary Redirect\r\nlocation: http://localhost:{other_port}/steal\r\ncontent-length: 0\r\n\r\n");
                let _ = s.write_all(reply.as_bytes()).await;
            }
        });
        let r = keyed_http_client()
            .get(format!("http://127.0.0.1:{first_port}/v1"))
            .header("x-goog-api-key", "test-key-not-real")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), 307, "the redirect was followed");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!hit.load(std::sync::atomic::Ordering::SeqCst));
    }

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

    async fn recent_send(state: &CallsState, token: Option<&str>, body: Value) -> Response {
        let mut headers = HeaderMap::new();
        if let Some(t) = token {
            headers.insert("x-calls-token", t.parse().unwrap());
        }
        notify_send(
            State(state.clone()),
            headers,
            Query(TokenQuery { token: None }),
            Json(serde_json::from_value(body).unwrap()),
        )
        .await
    }

    async fn recent_get(state: &CallsState, token: Option<&str>, after: Option<&str>) -> Response {
        let mut headers = HeaderMap::new();
        if let Some(t) = token {
            headers.insert("x-calls-token", t.parse().unwrap());
        }
        notify_recent(
            State(state.clone()),
            headers,
            Query(RecentQuery { token: None, after: after.map(str::to_string) }),
        )
        .await
    }

    async fn recent_json(r: Response) -> Value {
        assert_eq!(r.status(), StatusCode::OK);
        serde_json::from_str(&body_text(r).await).unwrap()
    }

    fn recent_state(tag: &str) -> CallsState {
        let root = std::env::temp_dir().join(format!("bageo-{tag}-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        quick_state(&root)
    }

    fn recent_marker(tag: &str) -> String {
        format!("{tag}-{}", uuid::Uuid::new_v4().simple())
    }

    #[tokio::test]
    async fn recent_route_needs_the_password() {
        let mut state = recent_state("recent-auth");
        state.settings.calls_ui_token = "recent-test-pw".into();
        let marker = recent_marker("auth");
        let r = recent_get(&state, None, None).await;
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        // Sending without the password is refused and leaves nothing behind.
        let r = recent_send(&state, None, json!({"body": marker, "kind": "sms"})).await;
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        let ok = recent_json(recent_get(&state, Some("recent-test-pw"), Some("0")).await).await;
        assert_eq!(ok["ok"], true);
        assert!(ok["events"].as_array().unwrap().iter().all(|e| e["text"] != marker.as_str()));
    }

    #[tokio::test]
    async fn recent_route_without_after_only_gives_last_id() {
        let state = recent_state("recent-noafter");
        let _ = recent_send(&state, None, json!({"body": recent_marker("noafter")})).await;
        for after in [None, Some(""), Some("abc"), Some("-1"), Some("1.5")] {
            let v = recent_json(recent_get(&state, None, after).await).await;
            assert_eq!(v["ok"], true, "after {after:?}");
            assert!(v["last_id"].as_u64().unwrap() > 0, "after {after:?}");
            assert_eq!(v["events"], json!([]), "after {after:?}");
        }
    }

    #[tokio::test]
    async fn recent_route_shows_what_notify_send_got() {
        let state = recent_state("recent-send");
        let start = recent_json(recent_get(&state, None, None).await).await["last_id"].as_u64().unwrap();
        let sms = recent_marker("sms");
        let plain = recent_marker("plain");
        let r = recent_send(
            &state,
            None,
            json!({"title": "Texts", "body": format!("Texts\n{sms}"), "kind": "SMS", "from": "+15555550123", "text": format!("  {sms}\n 簡訊內容 ")}),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        assert!(recent_json(r).await.get("ok").is_some());
        // An old-style request (no kind, from, text) still works.
        let r = recent_send(&state, None, json!({"body": format!("{plain}\nsecond line")})).await;
        assert_eq!(r.status(), StatusCode::OK);
        // An empty body is refused and not recorded.
        let r = recent_send(&state, None, json!({"body": "  ", "kind": "sms", "text": "x"})).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);

        let v = recent_json(recent_get(&state, None, Some(&start.to_string())).await).await;
        let events = v["events"].as_array().unwrap();
        let first = events.iter().find(|e| e["text"].as_str().is_some_and(|t| t.contains(&sms))).expect("sms event");
        assert_eq!(first["kind"], "sms");
        assert_eq!(first["title"], "Texts");
        assert_eq!(first["from"], "+15555550123");
        assert_eq!(first["text"], format!("{sms} 簡訊內容"));
        let second = events.iter().find(|e| e["text"] == plain.as_str()).expect("plain event");
        assert_eq!(second["kind"], "message");
        assert_eq!(second["title"], "Bageo");
        assert_eq!(second["from"], "");
        let (a, b) = (first["id"].as_u64().unwrap(), second["id"].as_u64().unwrap());
        assert!(a < b && b <= v["last_id"].as_u64().unwrap());
        assert!(events.windows(2).all(|w| w[0]["id"].as_u64() < w[1]["id"].as_u64()));
        assert!(events.iter().all(|e| e["at_ms"].as_u64().unwrap() > 0));
        // Asking from the first event's id leaves only the later ones.
        let v = recent_json(recent_get(&state, None, Some(&a.to_string())).await).await;
        let events = v["events"].as_array().unwrap();
        assert!(events.iter().all(|e| e["id"].as_u64().unwrap() > a));
        assert!(events.iter().any(|e| e["id"].as_u64() == Some(b)));
    }

    fn sms_headers(token: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(t) = token {
            headers.insert("x-calls-token", t.parse().unwrap());
        }
        headers
    }

    async fn sms_get(state: &CallsState, token: Option<&str>, q: &str, before: Option<u64>, limit: Option<&str>) -> Response {
        let query = SmsListQuery {
            token: None,
            q: Some(q.to_string()),
            before: before.map(|b| b.to_string()),
            limit: limit.map(str::to_string),
        };
        sms_list(State(state.clone()), sms_headers(token), Query(query)).await
    }

    async fn sms_del(state: &CallsState, token: Option<&str>, id: &str) -> Response {
        sms_delete(State(state.clone()), sms_headers(token), Path(id.to_string()), Query(TokenQuery { token: None })).await
    }

    async fn sms_wipe(state: &CallsState, token: Option<&str>) -> Response {
        let mut headers = sms_headers(token);
        headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        sms_clear(State(state.clone()), headers, Query(TokenQuery { token: None })).await
    }

    fn sms_texts(v: &Value) -> Vec<String> {
        v["messages"].as_array().unwrap().iter().map(|m| m["text"].as_str().unwrap().to_string()).collect()
    }

    #[tokio::test]
    async fn sms_routes_need_the_password() {
        let mut state = recent_state("sms-auth");
        state.settings.calls_ui_token = "sms-test-pw".into();
        let pw = Some("sms-test-pw");
        let sms = json!({"body": "New SMS", "kind": "sms", "from": "+15555550123", "text": "kept"});
        assert_eq!(recent_send(&state, pw, sms.clone()).await.status(), StatusCode::OK);
        // Without the password nothing is stored, shown or removed.
        assert_eq!(recent_send(&state, None, sms).await.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(sms_get(&state, None, "", None, None).await.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(sms_get(&state, Some("wrong-pw"), "", None, None).await.status(), StatusCode::UNAUTHORIZED);
        let id = recent_json(sms_get(&state, pw, "", None, None).await).await["messages"][0]["id"].as_u64().unwrap();
        assert_eq!(sms_del(&state, None, &id.to_string()).await.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(sms_wipe(&state, None).await.status(), StatusCode::UNAUTHORIZED);
        let v = recent_json(sms_get(&state, pw, "", None, None).await).await;
        assert_eq!((v["ok"].as_bool(), v["total"].as_u64()), (Some(true), Some(1)));
        assert_eq!(sms_texts(&v), vec!["kept"]);
    }

    #[tokio::test]
    async fn sms_from_notify_send_is_listed_and_one_ref_is_one_message() {
        let state = recent_state("sms-send");
        let start = recent_json(recent_get(&state, None, None).await).await["last_id"].as_u64().unwrap();
        let marker = recent_marker("sms-once");
        let received = crate::sms_store::now_ms() - 4_000;
        let sms = json!({
            "title": "New SMS",
            "body": format!("From: +15555550123\nMessage: {marker}"),
            "kind": "SMS",
            "from": " +15555550123 ",
            "to": "+15555550100",
            "text": format!("{marker}\n第二行 ✓"),
            "serial": "TESTSERIAL01",
            "device": "Pixel Test",
            "received_ms": received,
            "ref": format!("ref-{marker}"),
        });
        // The forwarder sends a message again until a channel took it.
        for _ in 0..3 {
            assert_eq!(recent_send(&state, None, sms.clone()).await.status(), StatusCode::OK);
        }
        let v = recent_json(sms_get(&state, None, "", None, None).await).await;
        assert_eq!((v["total"].as_u64(), v["has_more"].as_bool()), (Some(1), Some(false)));
        assert_eq!(v["retention_days"], 15);
        let m = &v["messages"][0];
        assert_eq!(m["from"], "+15555550123");
        assert_eq!(m["from_name"], "");
        assert_eq!(m["to"], "+15555550100");
        assert_eq!(m["text"], format!("{marker}\n第二行 ✓"));
        assert_eq!((m["serial"].as_str(), m["device"].as_str()), (Some("TESTSERIAL01"), Some("Pixel Test")));
        assert_eq!(m["at_ms"].as_u64(), Some(received));
        assert!(m["id"].as_u64().unwrap() > 0);
        assert!(m.get("ref").is_none(), "the forwarder's key stays inside");
        // The desktop program hears of it once as well.
        let recent = recent_json(recent_get(&state, None, Some(&start.to_string())).await).await;
        let heard = recent["events"].as_array().unwrap().iter().filter(|e| e["text"].as_str().is_some_and(|t| t.contains(&marker))).count();
        assert_eq!(heard, 1);

        // Not a text message: not stored. Without a ref: every request is one.
        assert_eq!(recent_send(&state, None, json!({"body": "plain notice", "from": "x", "text": "y"})).await.status(), StatusCode::OK);
        assert_eq!(recent_send(&state, None, json!({"body": "b", "kind": "sms"})).await.status(), StatusCode::OK);
        for _ in 0..2 {
            let old_style = json!({"body": "b", "kind": "sms", "from": "+15555550124", "text": "no ref", "received_ms": "soon"});
            assert_eq!(recent_send(&state, None, old_style).await.status(), StatusCode::OK);
        }
        let v = recent_json(sms_get(&state, None, "", None, None).await).await;
        assert_eq!(v["total"], 3);
        assert_eq!(sms_texts(&v), vec!["no ref".to_string(), "no ref".to_string(), format!("{marker}\n第二行 ✓")]);
        // The settings decide how long they are kept.
        state.settings_store.apply_patch(&json!({"retention_days": 40})).unwrap();
        assert_eq!(recent_json(sms_get(&state, None, "", None, None).await).await["retention_days"], 40);
    }

    #[tokio::test]
    async fn sms_route_searches_pages_and_names_senders() {
        let state = recent_state("sms-list");
        let serial = format!("SMSTEST{}", uuid::Uuid::new_v4().simple());
        crate::notify::remember_device(&serial, "Pixel Label");
        state.contacts.upsert_by_number("+15555550123", "王小明", "", "").await.unwrap();
        let rows = [
            ("+15555550123", "Your code is 123456"),
            ("+15555550124", "您的驗證碼是 654321"),
            ("+15555550125", "認証コードは 111222 です"),
            ("+15555550126", "인증번호는 333444 입니다"),
            ("+15555550127", "رمز التحقق هو 555666"),
            ("+15555550128", "รหัสยืนยันของคุณคือ 777888"),
            ("+15555550129", "Votre code est 999000 😀"),
        ];
        for (from, text) in rows {
            let sms = json!({"body": "New SMS", "kind": "sms", "from": from, "text": text, "serial": serial});
            assert_eq!(recent_send(&state, None, sms).await.status(), StatusCode::OK);
        }
        // Newest first, a page at a time.
        let first = recent_json(sms_get(&state, None, "", None, Some("3")).await).await;
        assert_eq!((first["total"].as_u64(), first["has_more"].as_bool()), (Some(7), Some(true)));
        assert_eq!(sms_texts(&first), vec![rows[6].1, rows[5].1, rows[4].1]);
        let last_id = first["messages"][2]["id"].as_u64().unwrap();
        let second = recent_json(sms_get(&state, None, "", Some(last_id), Some("10")).await).await;
        assert_eq!(sms_texts(&second), vec![rows[3].1, rows[2].1, rows[1].1, rows[0].1]);
        assert_eq!(second["has_more"], false);
        // An odd limit falls back; it never goes past the range.
        assert_eq!(sms_texts(&recent_json(sms_get(&state, None, "", None, Some("abc")).await).await).len(), 7);
        assert_eq!(sms_texts(&recent_json(sms_get(&state, None, "", None, Some("0")).await).await).len(), 1);
        assert_eq!(sms_texts(&recent_json(sms_get(&state, None, "", None, Some("9999")).await).await).len(), 7);
        // The contact's name and the phone's name are filled in.
        let named = &second["messages"][3];
        assert_eq!((named["from"].as_str(), named["from_name"].as_str()), (Some("+15555550123"), Some("王小明")));
        assert_eq!(named["device"], "Pixel Label");
        assert_eq!(second["messages"][2]["from_name"], "");
        // Search: text in several languages, the sender, the contact's name.
        for (q, want) in [
            ("YOUR CODE", rows[0].1),
            ("驗證碼", rows[1].1),
            ("認証コード", rows[2].1),
            ("인증번호", rows[3].1),
            ("التحقق", rows[4].1),
            ("รหัสยืนยัน", rows[5].1),
            ("😀", rows[6].1),
            ("5550127", rows[4].1),
            ("王小明", rows[0].1),
        ] {
            let v = recent_json(sms_get(&state, None, q, None, None).await).await;
            assert_eq!(sms_texts(&v), vec![want], "q {q:?}");
            assert_eq!(v["total"], 7, "q {q:?}");
        }
        let none = recent_json(sms_get(&state, None, "no such text", None, None).await).await;
        assert_eq!((sms_texts(&none).len(), none["total"].as_u64(), none["has_more"].as_bool()), (0, Some(7), Some(false)));
        crate::notify::forget_device(&serial);
    }

    #[tokio::test]
    async fn sms_route_deletes_one_and_clears_all() {
        let state = recent_state("sms-del");
        for text in ["one", "two", "three"] {
            let sms = json!({"body": "New SMS", "kind": "sms", "from": "+15555550123", "text": text});
            assert_eq!(recent_send(&state, None, sms).await.status(), StatusCode::OK);
        }
        let v = recent_json(sms_get(&state, None, "", None, None).await).await;
        let two = v["messages"][1]["id"].as_u64().unwrap().to_string();
        assert_eq!(recent_json(sms_del(&state, None, &two).await).await, json!({"ok": true}));
        for missing in [two.as_str(), "0", "abc", "-1"] {
            let r = sms_del(&state, None, missing).await;
            assert_eq!(r.status(), StatusCode::NOT_FOUND, "{missing}");
            assert_eq!(serde_json::from_str::<Value>(&body_text(r).await).unwrap()["code"], "not_found");
        }
        assert_eq!(sms_texts(&recent_json(sms_get(&state, None, "", None, None).await).await), vec!["three", "one"]);
        // Without a JSON content type (a form on another site) nothing is removed.
        let r = sms_clear(State(state.clone()), sms_headers(None), Query(TokenQuery { token: None })).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        assert_eq!(recent_json(sms_get(&state, None, "", None, None).await).await["total"], 2);
        assert_eq!(recent_json(sms_wipe(&state, None).await).await, json!({"ok": true, "removed": 2}));
        assert_eq!(recent_json(sms_wipe(&state, None).await).await, json!({"ok": true, "removed": 0}));
        assert_eq!(recent_json(sms_get(&state, None, "", None, None).await).await["total"], 0);
    }

    #[tokio::test]
    async fn retention_change_in_settings_cleans_everything_at_once() {
        let root = std::env::temp_dir().join(format!("bageo-retention-all-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        let ago = |days: i64| (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();
        // Kept before this start: a call 10 days old and one 1 day old, AI errands that are over.
        let (old_call, new_call) = {
            let calls = crate::calls::CallStore::new(root.join("calls"), true, false);
            let a = calls.start("pixel-1", "+15555550123", 16000, "active").await.unwrap();
            let b = calls.start("pixel-1", "+15555550124", 16000, "active").await.unwrap();
            let ids = (a.id.clone(), b.id.clone());
            calls.finalize(a, "ended").await;
            calls.finalize(b, "ended").await;
            ids
        };
        let index = root.join("calls").join("index.json");
        let mut v: Value = serde_json::from_slice(&std::fs::read(&index).unwrap()).unwrap();
        for row in v["calls"].as_array_mut().unwrap() {
            let days = if row["id"] == old_call.as_str() { 10 } else { 1 };
            row["started_at"] = json!(ago(days));
        }
        std::fs::write(&index, serde_json::to_vec(&v).unwrap()).unwrap();
        let errands = json!({"errands": [
            {"id": "e-old", "status": "done", "created_at": ago(11), "updated_at": ago(10)},
            {"id": "e-new", "status": "done", "created_at": ago(2), "updated_at": ago(1)}
        ]});
        std::fs::write(root.join("errands.json"), serde_json::to_vec(&errands).unwrap()).unwrap();
        let state = quick_state(&root);
        state.settings_store.apply_patch(&json!({"retention_days": 15})).unwrap();
        // To-do items and text messages, 10 days old and recent.
        let task = |created: String| crate::tasks::TaskRecord {
            id: String::new(),
            created_at: created,
            updated_at: String::new(),
            call_id: "c".into(),
            serial: "pixel-1".into(),
            caller_id: "+15555550123".into(),
            status: crate::tasks::TaskStatus::Pending,
            intent: crate::tasks::TaskIntent::Appointment,
            summary: String::new(),
            transcript: String::new(),
            datetime_hint: None,
            reply_method: crate::tasks::ReplyMethod::None,
            reply_contact: None,
            contact_verified: false,
            voicemail_path: None,
            calendar_event_id: None,
            fulfill_error: None,
            openclaw_ref: None,
            events: vec![],
            appointment: None,
        };
        let old_task = state.task_store.create(task(ago(10))).await.unwrap().id;
        let new_task = state.task_store.create(task(ago(1))).await.unwrap().id;
        let now = crate::sms_store::now_ms();
        for (age_ms, text) in [(10 * 86_400_000u64, "十日前のメッセージ"), (43_200_000, "Mensaje de hoy")] {
            state.sms.push(now, 15, crate::sms_store::NewSms { received_ms: now - age_ms, from: "+15555550123", text, ..Default::default() });
        }
        let texts = || state.sms.list(now, 365, "", None, 50, &|_: &str| String::new()).messages.into_iter().map(|m| m.text).collect::<Vec<_>>();
        // What saving the settings does with the days (the handler itself also
        // sets the language for the whole program, which other tests read).
        let put = |patch: Value| {
            let before = state.settings_store.snapshot().retention_days;
            let rt = state.settings_store.apply_patch(&patch).unwrap();
            purge_after_fewer_days(&state, before, rt.retention_days);
        };
        // More days: nothing goes.
        put(json!({"retention_days": 30}));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(state.store.get(&old_call).await.is_some());
        assert_eq!(texts().len(), 2);
        // Fewer days: everything older goes right away.
        put(json!({"retention_days": 5}));
        assert_eq!(state.settings_store.snapshot().retention_days, 5);
        let mut gone = false;
        for _ in 0..60 {
            if state.store.get(&old_call).await.is_none()
                && state.task_store.get(&old_task).await.is_none()
                && state.errands.get("e-old").is_none()
                && texts().len() == 1
            {
                gone = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(gone, "the calls, to-do items, errands and texts older than 5 days went");
        assert!(state.store.get(&new_call).await.is_some());
        assert!(state.task_store.get(&new_task).await.is_some());
        assert!(state.errands.get("e-new").is_some());
        assert_eq!(texts(), vec!["Mensaje de hoy".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    // ---- SMS conversations: a simulated phone on the control connection, never a real one.

    fn sms_serial(tag: &str) -> String {
        format!("SMSCHAT{tag}{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
    }

    /// A phone connected over a test socket (its end is returned), with these SIMs.
    async fn sms_phone(state: &CallsState, serial: &str, sims: Value, texts: bool) -> crate::callback::test_socket::PhoneEnd {
        let (sink, phone) = crate::callback::test_socket::phone_socket().await;
        state.control.register_client(serial.to_string(), sink).await;
        state.control.set_supports_sms(serial, texts).await;
        state.errands.sims.note_list(serial, &sims);
        phone
    }

    async fn sms_receive(state: &CallsState, from: &str, text: &str, serial: &str, sub_id: Option<i64>, to: &str) {
        let body = json!({"body": "New SMS", "kind": "sms", "from": from, "text": text, "serial": serial, "sub_id": sub_id, "to": to});
        assert_eq!(recent_send(state, None, body).await.status(), StatusCode::OK);
    }

    fn sms_json_headers(token: Option<&str>) -> HeaderMap {
        let mut h = sms_headers(token);
        h.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
        h
    }

    async fn sms_post_send(state: CallsState, headers: HeaderMap, body: Value) -> Response {
        sms_send(State(state), headers, Query(TokenQuery { token: None }), axum::body::Bytes::from(body.to_string())).await
    }

    async fn sms_post_retry(state: CallsState, headers: HeaderMap, id: u64) -> Response {
        sms_retry(State(state), headers, Path(id.to_string()), Query(TokenQuery { token: None })).await
    }

    async fn sms_threads_get(state: &CallsState, token: Option<&str>, q: &str) -> Response {
        sms_threads(State(state.clone()), sms_headers(token), Query(SmsThreadsQuery { token: None, q: Some(q.to_string()) })).await
    }

    fn sms_thread_query(number: &str, before: Option<u64>, limit: Option<&str>) -> SmsThreadQuery {
        SmsThreadQuery {
            token: None,
            number: Some(number.to_string()),
            before: before.map(|b| b.to_string()),
            limit: limit.map(str::to_string),
        }
    }

    async fn sms_thread_get(state: &CallsState, number: &str, before: Option<u64>, limit: Option<&str>) -> Value {
        recent_json(sms_thread(State(state.clone()), sms_headers(None), Query(sms_thread_query(number, before, limit))).await).await
    }

    async fn sms_phone_gets(phone: &mut crate::callback::test_socket::PhoneEnd) -> Value {
        crate::callback::test_socket::next_json(phone, std::time::Duration::from_secs(3)).await.expect("the phone gets send_sms")
    }

    fn sms_thread_keys(v: &Value) -> Vec<String> {
        v["threads"].as_array().unwrap().iter().map(|t| t["key"].as_str().unwrap().to_string()).collect()
    }

    #[tokio::test]
    async fn sms_threads_route_groups_numbers_and_lists_lines() {
        let state = recent_state("sms-threads");
        let a = sms_serial("A");
        let b = sms_serial("B");
        let _phone = sms_phone(
            &state,
            &a,
            json!([{"sub_id": 1, "country": "tw", "slot": 0, "number": "0912000111"}, {"sub_id": 2, "country": "us", "slot": 1, "number": "+16505550100"}]),
            true,
        )
        .await;
        state.errands.sims.note_list(&b, &json!([{"sub_id": 5, "country": "jp", "slot": 0}]));
        let rows: [(&str, &str, &str, i64); 10] = [
            ("0912345678", "您的包裹已送達", a.as_str(), 1),
            ("(650) 555-0142", "See you at three", a.as_str(), 2),
            ("BankInfo", "Your statement is ready", a.as_str(), 1),
            ("090-1234-5678", "明日の予定を確認してください", b.as_str(), 5),
            ("+821012345678", "내일 회의는 오후 3시입니다", a.as_str(), 1),
            ("+966500000000", "موعدك غدا الساعة العاشرة", a.as_str(), 1),
            ("+66812345678", "พรุ่งนี้เจอกันที่ร้านกาแฟ", a.as_str(), 1),
            ("+34600000000", "¿Nos vemos mañana en la estación?", a.as_str(), 1),
            ("", "no sender at all", a.as_str(), 1),
            ("+886912345678", "第二則：明天見", a.as_str(), 1),
        ];
        for (from, text, serial, sub) in rows {
            sms_receive(&state, from, text, serial, Some(sub), "").await;
        }
        let v = recent_json(sms_threads_get(&state, None, "").await).await;
        assert_eq!((v["ok"].as_bool(), v["total"].as_u64(), v["retention_days"].as_u64()), (Some(true), Some(10), Some(15)));
        let keys = sms_thread_keys(&v);
        assert_eq!(keys.len(), 9, "0912… and +886912… are one conversation: {keys:?}");
        assert_eq!(keys[0], "+886912345678", "newest first");
        let t = &v["threads"][0];
        assert_eq!((t["count"].as_u64(), t["number"].as_str(), t["last_text"].as_str()), (Some(2), Some("+886912345678"), Some("第二則：明天見")));
        assert_eq!((t["last_dir"].as_str(), t["last_status"].as_str(), t["can_reply"].as_bool()), (Some("in"), Some(""), Some(true)));
        assert_eq!(t["last_in_at"], t["last_at"]);
        // Numbers are read in the country of the SIM that got them.
        assert!(keys.contains(&"+16505550142".to_string()), "{keys:?}");
        assert!(keys.contains(&"+819012345678".to_string()), "{keys:?}");
        let bank = v["threads"].as_array().unwrap().iter().find(|t| t["key"] == "bankinfo").unwrap();
        assert_eq!((bank["number"].as_str(), bank["can_reply"].as_bool()), (Some("BankInfo"), Some(false)));
        let nobody = v["threads"].as_array().unwrap().iter().find(|t| t["key"] == "~").unwrap();
        assert_eq!((nobody["number"].as_str(), nobody["can_reply"].as_bool()), (Some(""), Some(false)));
        // Every SIM is a line; only the connected phone that texts can send.
        let lines = v["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 3, "{lines:?}");
        let line = |serial: &str, sub: i64| lines.iter().find(|l| l["serial"] == serial && l["sub_id"] == sub).unwrap().clone();
        let a1 = line(&a, 1);
        assert_eq!((a1["number"].as_str(), a1["slot"].as_i64(), a1["can_send"].as_bool()), (Some("0912000111"), Some(0), Some(true)));
        assert_eq!(a1["device"], a.as_str(), "no name or model known: the serial");
        assert_eq!(line(&a, 2)["number"], "+16505550100");
        assert_eq!(line(&b, 5)["can_send"], false);
        // Search: contact name, number, text in several languages.
        state.contacts.upsert_by_number("+886912345678", "王小明", "", "").await.unwrap();
        for (q, key) in [
            ("包裹", "+886912345678"),
            ("王小明", "+886912345678"),
            ("0912345678", "+886912345678"),
            ("予定", "+819012345678"),
            ("회의는", "+821012345678"),
            ("الساعة", "+966500000000"),
            ("ร้านกาแฟ", "+66812345678"),
            ("MAÑANA", "+34600000000"),
            ("statement", "bankinfo"),
        ] {
            let v = recent_json(sms_threads_get(&state, None, q).await).await;
            assert_eq!(sms_thread_keys(&v), vec![key.to_string()], "q {q:?}");
            assert_eq!(v["total"], 10, "q {q:?}");
        }
        let named = recent_json(sms_threads_get(&state, None, "").await).await;
        assert_eq!(named["threads"][0]["name"], "王小明");
        assert!(sms_thread_keys(&recent_json(sms_threads_get(&state, None, "no such text").await).await).is_empty());
    }

    #[tokio::test]
    async fn sms_thread_route_finds_the_conversation_and_pages() {
        let state = recent_state("sms-thread");
        let a = sms_serial("A");
        let _phone = sms_phone(
            &state,
            &a,
            json!([{"sub_id": 1, "country": "tw", "number": "0912000111"}, {"sub_id": 2, "country": "tw", "number": "0912000222"}]),
            true,
        )
        .await;
        // The forwarder did not say which SIM: the number it was sent to tells.
        sms_receive(&state, "0912345678", "第一則", &a, None, "+886912000222").await;
        for i in 2..=6 {
            sms_receive(&state, "+886912345678", &format!("第{i}則"), &a, Some(1), "").await;
        }
        let texts = |v: &Value| v["messages"].as_array().unwrap().iter().map(|m| m["text"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        for number in ["0912345678", "+886912345678", "0912-345-678", "+886 912 345 678", "912345678"] {
            let v = sms_thread_get(&state, number, None, None).await;
            assert_eq!((v["key"].as_str(), v["messages"].as_array().unwrap().len()), (Some("+886912345678"), 6), "{number}");
        }
        let v = sms_thread_get(&state, "0912345678", None, Some("2")).await;
        assert_eq!((texts(&v), v["has_more"].as_bool()), (vec!["第5則".to_string(), "第6則".to_string()], Some(true)), "oldest first");
        assert_eq!((v["number"].as_str(), v["can_reply"].as_bool()), (Some("+886912345678"), Some(true)));
        let m = &v["messages"][1];
        assert_eq!((m["dir"].as_str(), m["status"].as_str(), m["error"].as_str(), m["sub_id"].as_i64()), (Some("in"), Some(""), Some(""), Some(1)));
        assert_eq!(v["reply"], json!({"serial": a, "sub_id": 1}), "the newest received one came on SIM 1");
        let first_id = v["messages"][0]["id"].as_u64().unwrap();
        let older = sms_thread_get(&state, "0912345678", Some(first_id), Some("10")).await;
        assert_eq!((texts(&older).len(), older["has_more"].as_bool()), (4, Some(false)));
        assert_eq!(texts(&older)[0], "第一則");
        // A number with no conversation yet: empty, and a line to start one.
        let new = sms_thread_get(&state, "0987-654-321", None, None).await;
        assert_eq!((new["messages"].as_array().unwrap().len(), new["has_more"].as_bool()), (0, Some(false)));
        assert_eq!((new["number"].as_str(), new["can_reply"].as_bool()), (Some("0987654321"), Some(true)));
        assert_eq!(new["reply"], json!({"serial": a, "sub_id": 1}));
        // A sender name cannot be answered.
        sms_receive(&state, "BankInfo", "Your code is 123456", &a, Some(1), "").await;
        let bank = sms_thread_get(&state, "bankinfo", None, None).await;
        assert_eq!((bank["number"].as_str(), bank["can_reply"].as_bool(), texts(&bank).len()), (Some("BankInfo"), Some(false), 1));
        // Without the password: nothing.
        let mut locked = state.clone();
        locked.settings.calls_ui_token = "sms-thread-pw".into();
        let r = sms_thread(State(locked), sms_headers(None), Query(sms_thread_query("0912345678", None, None))).await;
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn sms_reply_line_follows_the_received_message() {
        use crate::sms_store::SmsMessage;
        let line = |serial: &str, sub: Option<i64>, number: &str, can: bool| SmsLine {
            serial: serial.into(),
            sub_id: sub,
            number: number.into(),
            country: "tw".into(),
            can_send: can,
            ..Default::default()
        };
        let lines = vec![
            line("A", Some(1), "0912000111", true),
            line("A", Some(2), "0912000222", true),
            line("B", Some(1), "", false),
            line("C", None, "", true),
        ];
        let msg = |dir: &str, serial: &str, sub: Option<i64>, to: &str| SmsMessage {
            id: 1,
            at_ms: 1,
            from: "0912345678".into(),
            to: to.into(),
            text: "x".into(),
            serial: serial.into(),
            device: String::new(),
            reference: String::new(),
            dir: dir.into(),
            sub_id: sub,
            status: String::new(),
            error: String::new(),
        };
        let region = |_: &SmsMessage| Some("tw".to_string());
        let pick = |msgs: &[SmsMessage], lines: &[SmsLine]| sms_reply_line(msgs, lines, &region).map(|l| (l.serial.clone(), l.sub_id));
        let a = |sub: i64| Some(("A".to_string(), Some(sub)));
        // The SIM the newest received message came on.
        assert_eq!(pick(&[msg("in", "A", Some(2), "")], &lines), a(2));
        // No SIM given: the one whose number it was sent to.
        assert_eq!(pick(&[msg("in", "A", None, "+886912000222")], &lines), a(2));
        // No SIM and no number that matches: the phone's first SIM.
        assert_eq!(pick(&[msg("in", "A", None, "")], &lines), a(1));
        assert_eq!(pick(&[msg("in", "A", None, "0933000999")], &lines), a(1));
        assert_eq!(pick(&[msg("in", "A", Some(9), "")], &lines), a(1), "a SIM the phone no longer lists");
        // That phone cannot send now: the first line that can.
        assert_eq!(pick(&[msg("in", "B", Some(1), "")], &lines), a(1));
        assert_eq!(pick(&[msg("in", "Z", Some(1), "")], &lines), a(1), "an unknown phone");
        // A phone without a SIM list sends from its default SIM.
        assert_eq!(pick(&[msg("in", "C", None, "")], &lines), Some(("C".to_string(), None)));
        // Received ones win over sent ones; with none received, the newest sent one.
        let both = [msg("in", "A", Some(2), ""), msg("out", "C", None, "0912345678")];
        assert_eq!(pick(&both, &lines), a(2));
        assert_eq!(pick(&[msg("out", "C", None, "0912345678")], &lines), Some(("C".to_string(), None)));
        assert_eq!(pick(&[msg("out", "A", Some(2), "0912345678")], &lines), a(2));
        // A new conversation: the first line that can.
        assert_eq!(pick(&[], &lines), a(1));
        // No line can send: none.
        let offline: Vec<SmsLine> = lines.iter().map(|l| SmsLine { can_send: false, ..l.clone() }).collect();
        assert_eq!(pick(&[msg("in", "A", Some(2), "")], &offline), None);
        assert_eq!(pick(&[], &[]), None);
    }

    #[tokio::test]
    async fn sms_send_route_reports_what_the_phone_did() {
        let state = recent_state("sms-send-route");
        let a = sms_serial("A");
        let b = sms_serial("B");
        let c = sms_serial("C");
        let mut phone = sms_phone(&state, &a, json!([{"sub_id": 1, "country": "tw", "number": "0912000111"}]), true).await;
        let _old_app = sms_phone(&state, &b, json!([{"sub_id": 3, "country": "jp"}]), false).await;
        state.errands.sims.note_list(&c, &json!([{"sub_id": 4, "country": "tw"}]));
        let send = |to: &str, text: &str, serial: &str, sub: i64| {
            tokio::spawn(sms_post_send(state.clone(), sms_json_headers(None), json!({"to": to, "text": text, "serial": serial, "sub_id": sub})))
        };

        // Sent: the phone gets the number, the text and the SIM.
        let task = send("0912-345-678", "  你好，明天見  ", &a, 1);
        let got = sms_phone_gets(&mut phone).await;
        assert_eq!((got["type"].as_str(), got["to"].as_str(), got["text"].as_str(), got["sub_id"].as_i64()), (Some("send_sms"), Some("0912345678"), Some("你好，明天見"), Some(1)));
        let id = got["id"].as_str().unwrap().to_string();
        assert!(id.starts_with("sms-"));
        assert!(state.control.handle_sms_result(&json!({"type": "send_sms_result", "id": id, "ok": true})).await);
        let v = recent_json(task.await.unwrap()).await;
        let m = &v["message"];
        assert_eq!((v["ok"].as_bool(), m["status"].as_str(), m["error"].as_str(), m["dir"].as_str()), (Some(true), Some("sent"), Some(""), Some("out")));
        assert_eq!((m["from"].as_str(), m["to"].as_str(), m["text"].as_str(), m["sub_id"].as_i64()), (Some("0912000111"), Some("0912345678"), Some("你好，明天見"), Some(1)));
        assert_eq!(format!("sms-{}", m["id"]), id);

        // The phone answers with an error code.
        let task = send("+886912345678", "Hola, ¿qué tal?", &a, 1);
        let got = sms_phone_gets(&mut phone).await;
        state.control.handle_sms_result(&json!({"type": "send_sms_result", "id": got["id"], "ok": false, "error": "no_service"})).await;
        let v = recent_json(task.await.unwrap()).await;
        assert_eq!((v["ok"].as_bool(), v["message"]["status"].as_str(), v["message"]["error"].as_str()), (Some(false), Some("failed"), Some("no_service")));

        // No answer in time: not known yet. A late answer shows up in the conversation.
        let task = send("0912345678", "また明日", &a, 1);
        let late = sms_phone_gets(&mut phone).await;
        let v = recent_json(task.await.unwrap()).await;
        assert_eq!((v["ok"].as_bool(), v["message"]["status"].as_str()), (Some(false), Some("timeout")));
        let timed_out = v["message"]["id"].as_u64().unwrap();
        state.control.handle_sms_result(&json!({"type": "send_sms_result", "id": late["id"], "ok": true})).await;
        let thread = sms_thread_get(&state, "0912345678", None, None).await;
        let statuses: Vec<(u64, String)> = thread["messages"].as_array().unwrap().iter().map(|m| (m["id"].as_u64().unwrap(), m["status"].as_str().unwrap().to_string())).collect();
        assert_eq!(statuses.len(), 3, "all three went to the same person");
        assert_eq!(statuses.iter().find(|(i, _)| *i == timed_out).unwrap().1, "sent");
        assert_eq!(thread["reply"], json!({"serial": a, "sub_id": 1}), "only sent ones: the line they went on");

        // Refused before anything is stored: lines that cannot send right now (409).
        let total = || async { recent_json(sms_threads_get(&state, None, "").await).await["total"].as_u64().unwrap() };
        let before = total().await;
        for (serial, sub, text) in [(&b, 3, "안녕하세요"), (&c, 4, "مرحبا")] {
            let r = send("0912345678", text, serial, sub).await.unwrap();
            assert_eq!(r.status(), StatusCode::CONFLICT, "{serial}");
            assert_eq!(serde_json::from_str::<Value>(&body_text(r).await).unwrap()["code"], "line_unavailable");
        }
        assert_eq!(total().await, before, "unavailable lines must not save outgoing records");

        // Refused before anything is stored: emergency, short, premium and invalid targets.
        for (body, code) in [
            (json!({"to": "110", "text": "hi", "serial": a, "sub_id": 1}), "not_allowed"),
            (json!({"to": "119", "text": "hi", "serial": a, "sub_id": 1}), "not_allowed"),
            (json!({"to": "911", "text": "hi", "serial": a, "sub_id": 1}), "not_allowed"),
            (json!({"to": "112", "text": "hi", "serial": a, "sub_id": 1}), "not_allowed"),
            (json!({"to": "0204-123-456", "text": "hi", "serial": a, "sub_id": 1}), "not_allowed"),
            (json!({"to": "+1 900-555-0100", "text": "hi", "serial": a, "sub_id": 1}), "not_allowed"),
            (json!({"to": "BankInfo", "text": "hi", "serial": a, "sub_id": 1}), "bad_number"),
            (json!({"to": "12", "text": "hi", "serial": a, "sub_id": 1}), "bad_number"),
            (json!({"to": "0912345678", "text": "hi", "serial": a, "sub_id": 9}), "bad_line"),
            (json!({"to": "0912345678", "text": "hi", "serial": "pixel-none", "sub_id": 1}), "bad_line"),
            (json!({"to": "0912345678", "text": "hi", "serial": a}), "bad_line"),
            (json!({"to": "0912345678", "text": "   ", "serial": a, "sub_id": 1}), "bad_text"),
            (json!({"to": "0912345678", "text": "字".repeat(501), "serial": a, "sub_id": 1}), "bad_text"),
            (json!(["not", "an", "object"]), "bad_request"),
        ] {
            let r = sms_post_send(state.clone(), sms_json_headers(None), body.clone()).await;
            assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(serde_json::from_str::<Value>(&body_text(r).await).unwrap()["code"], code, "{body}");
        }
        assert_eq!(total().await, before, "rejected numbers must not save outgoing records");
        // International valid number from a TW SIM is allowed.
        let task = send("+1 (650) 555-0142", "See you soon", &a, 1);
        let got = sms_phone_gets(&mut phone).await;
        assert_eq!(got["to"], "+16505550142");
        state.control.handle_sms_result(&json!({"type": "send_sms_result", "id": got["id"], "ok": true})).await;
        assert_eq!(task.await.unwrap().status(), StatusCode::OK);
        let before = before + 1;
        // 500 characters is fine (and is sent).
        let task = send("0912345678", &"字".repeat(500), &a, 1);
        let got = sms_phone_gets(&mut phone).await;
        assert_eq!(got["text"].as_str().unwrap().chars().count(), 500);
        state.control.handle_sms_result(&json!({"type": "send_sms_result", "id": got["id"], "ok": true})).await;
        assert_eq!(task.await.unwrap().status(), StatusCode::OK);
        let before = before + 1;
        // Not JSON (another site's form) or no password: refused, nothing stored or sent.
        let r = sms_post_send(state.clone(), sms_headers(None), json!({"to": "0912345678", "text": "hi", "serial": a, "sub_id": 1})).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let mut locked = state.clone();
        locked.settings.calls_ui_token = "sms-send-pw".into();
        let r = sms_post_send(locked.clone(), sms_json_headers(None), json!({"to": "0912345678", "text": "hi", "serial": a, "sub_id": 1})).await;
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(sms_threads_get(&locked, None, "").await.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(total().await, before);
        assert!(crate::callback::test_socket::next_json(&mut phone, std::time::Duration::from_millis(200)).await.is_none());
    }

    #[tokio::test]
    async fn sms_retry_route_only_sends_failed_ones_again() {
        let state = recent_state("sms-retry");
        let a = sms_serial("A");
        let mut phone = sms_phone(&state, &a, json!([{"sub_id": 1, "country": "tw", "number": "0912000111"}]), true).await;
        let task = tokio::spawn(sms_post_send(state.clone(), sms_json_headers(None), json!({"to": "0912345678", "text": "Ciao!", "serial": a, "sub_id": 1})));
        let got = sms_phone_gets(&mut phone).await;
        state.control.handle_sms_result(&json!({"type": "send_sms_result", "id": got["id"], "ok": false, "error": "radio_off"})).await;
        let failed = recent_json(task.await.unwrap()).await["message"]["id"].as_u64().unwrap();

        // Without JSON: refused, still failed.
        let r = sms_post_retry(state.clone(), sms_headers(None), failed).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        assert_eq!(state.sms.get(failed).unwrap().status, "failed");
        // Sent again on the same line, as the same message.
        let task = tokio::spawn(sms_post_retry(state.clone(), sms_json_headers(None), failed));
        let again = sms_phone_gets(&mut phone).await;
        assert_eq!((again["id"].clone(), again["text"].as_str(), again["sub_id"].as_i64()), (got["id"].clone(), Some("Ciao!"), Some(1)));
        state.control.handle_sms_result(&json!({"type": "send_sms_result", "id": again["id"], "ok": true})).await;
        let v = recent_json(task.await.unwrap()).await;
        assert_eq!((v["ok"].as_bool(), v["message"]["id"].as_u64(), v["message"]["status"].as_str()), (Some(true), Some(failed), Some("sent")));
        // A sent one, a received one, an unknown one.
        let r = sms_post_retry(state.clone(), sms_json_headers(None), failed).await;
        assert_eq!(r.status(), StatusCode::CONFLICT);
        assert_eq!(serde_json::from_str::<Value>(&body_text(r).await).unwrap()["code"], "not_retryable");
        sms_receive(&state, "0912345678", "收到了", &a, Some(1), "").await;
        let received = state.sms.list(crate::sms_store::now_ms(), 15, "收到了", None, 1, &|_: &str| String::new()).messages[0].id;
        assert_eq!(sms_post_retry(state.clone(), sms_json_headers(None), received).await.status(), StatusCode::CONFLICT);
        assert_eq!(sms_post_retry(state.clone(), sms_json_headers(None), 42).await.status(), StatusCode::NOT_FOUND);
        assert!(crate::callback::test_socket::next_json(&mut phone, std::time::Duration::from_millis(200)).await.is_none());
    }

    #[tokio::test]
    async fn sms_thread_delete_route_removes_one_conversation() {
        let state = recent_state("sms-del-thread");
        let a = sms_serial("A");
        state.errands.sims.note_list(&a, &json!([{"sub_id": 1, "country": "tw"}]));
        sms_receive(&state, "0912345678", "第一則", &a, Some(1), "").await;
        sms_receive(&state, "+886912345678", "第二則", &a, Some(1), "").await;
        sms_receive(&state, "+16505550142", "other person", &a, Some(1), "").await;
        let del = |headers: HeaderMap, number: &str| {
            sms_thread_delete(State(state.clone()), headers, Query(sms_thread_query(number, None, None)))
        };
        assert_eq!(del(sms_headers(None), "0912345678").await.status(), StatusCode::BAD_REQUEST);
        let mut locked = state.clone();
        locked.settings.calls_ui_token = "sms-del-pw".into();
        let r = sms_thread_delete(State(locked), sms_json_headers(None), Query(sms_thread_query("0912345678", None, None))).await;
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(recent_json(sms_threads_get(&state, None, "").await).await["total"], 3);
        assert_eq!(recent_json(del(sms_json_headers(None), "0912345678").await).await, json!({"ok": true, "removed": 2}));
        assert_eq!(recent_json(del(sms_json_headers(None), "0912345678").await).await, json!({"ok": true, "removed": 0}));
        let v = recent_json(sms_threads_get(&state, None, "").await).await;
        assert_eq!((sms_thread_keys(&v), v["total"].as_u64()), (vec!["+16505550142".to_string()], Some(1)));
    }

    #[tokio::test]
    async fn call_get_includes_blacklist_prefix_matches_and_prefers_exact_contacts() {
        let state = recent_state("call-get-bl");
        state
            .contacts
            .upsert_by_number("0800*", "Toll-free spam prefix", "sales prefix", "blacklist")
            .await
            .unwrap();
        state
            .contacts
            .upsert_by_number("0800111222", "Friendly Airline", "vip support", "whitelist")
            .await
            .unwrap();

        let mut c1 = state.store.start("pixel-test-1", "0800999888", 16000, "active").await.unwrap();
        c1.set_sim(Some("tw".into()), None);
        let r1 = state.store.finalize(c1, "ended").await;

        let mut c2 = state.store.start("pixel-test-1", "0800111222", 16000, "active").await.unwrap();
        c2.set_sim(Some("tw".into()), None);
        let r2 = state.store.finalize(c2, "ended").await;

        let v1 = recent_json(
            call_get(State(state.clone()), HeaderMap::new(), Query(TokenQuery { token: None }), Path(r1.id)).await,
        )
        .await;
        assert_eq!(v1["contact"]["name"], "Toll-free spam prefix");
        assert_eq!(v1["contact"]["note"], "sales prefix");
        assert_eq!(v1["contact"]["kind"], "blacklist");

        let v2 = recent_json(
            call_get(State(state), HeaderMap::new(), Query(TokenQuery { token: None }), Path(r2.id)).await,
        )
        .await;
        assert_eq!(v2["contact"]["name"], "Friendly Airline");
        assert_eq!(v2["contact"]["kind"], "whitelist");
    }

    #[tokio::test]
    async fn initial_password_enforces_active_token_and_validates_payload() {
        let mut locked = recent_state("init-pw-locked");
        locked.settings.calls_ui_token = "env-secret".into();
        let st_resp = recent_json(
            auth_status(State(locked.clone()), HeaderMap::new(), Query(TokenQuery { token: None })).await,
        )
        .await;
        assert_eq!((st_resp["password_set"].clone(), st_resp["prompt_initial"].clone()), (json!(true), json!(false)));

        let r = initial_password(
            State(locked.clone()),
            sms_json_headers(None),
            Query(TokenQuery { token: None }),
            axum::body::Bytes::from_static(br#"{"password":"new-pw"}"#),
        )
        .await;
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

        let open = recent_state("init-pw-open");
        let empty = initial_password(
            State(open.clone()),
            sms_json_headers(None),
            Query(TokenQuery { token: None }),
            axum::body::Bytes::from_static(br#"{"password":"   "}"#),
        )
        .await;
        assert_eq!(empty.status(), StatusCode::BAD_REQUEST);

        // A new password needs 8 characters (counted as characters, not bytes).
        for (body, short) in [(r#"{"password":"seven77"}"#, true), (r#"{"password":"密碼密碼密碼密"}"#, true)] {
            let r = initial_password(
                State(open.clone()),
                sms_json_headers(None),
                Query(TokenQuery { token: None }),
                axum::body::Bytes::from(body.to_string()),
            )
            .await;
            assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{body}");
            let got: Value = serde_json::from_str(&body_text(r).await).unwrap();
            assert_eq!(got["code"], json!("password_too_short"), "{body} short={short}");
        }
        let r = settings_put(State(open.clone()), HeaderMap::new(), Query(TokenQuery { token: None }), Json(json!({"ui_password": " seven77 "}))).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let got: Value = serde_json::from_str(&body_text(r).await).unwrap();
        assert_eq!(got["code"], json!("password_too_short"));
        assert!(open.settings_store.snapshot().ui_password.is_empty(), "nothing saved");
        assert!(!crate::settings_store::ui_password_too_short("eight888"));
        assert!(!crate::settings_store::ui_password_too_short(""), "empty keeps the saved one");

        let skipped = initial_password(
            State(open.clone()),
            sms_json_headers(None),
            Query(TokenQuery { token: None }),
            axum::body::Bytes::from_static(br#"{"skip":true}"#),
        )
        .await;
        assert_eq!(skipped.status(), StatusCode::OK);
        let skipped_json = recent_json(skipped).await;
        assert_eq!((skipped_json["ok"].clone(), skipped_json["password_set"].clone()), (json!(true), json!(false)));
        assert!(open.settings_store.snapshot().ui_password_prompted);
    }

    #[tokio::test]
    async fn sms_threads_mark_short_and_premium_numbers_unreplyable() {
        let state = recent_state("sms-unreplyable");
        let a = sms_serial("A");
        let _phone = sms_phone(&state, &a, json!([{"sub_id": 1, "country": "tw"}]), true).await;
        sms_receive(&state, "110", "short notice", &a, Some(1), "").await;
        sms_receive(&state, "0204123456", "premium notice", &a, Some(1), "").await;
        let list = recent_json(sms_threads_get(&state, None, "").await).await;
        for row in list["threads"].as_array().unwrap() {
            assert_eq!(row["can_reply"].as_bool(), Some(false), "{row}");
        }
        assert_eq!(sms_thread_get(&state, "110", None, None).await["can_reply"].as_bool(), Some(false));
        assert_eq!(sms_thread_get(&state, "0204123456", None, None).await["can_reply"].as_bool(), Some(false));
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
        let p = PhoneEntry { sink: None, hello, last_seen_ms: 5, wifi: false };
        let v = phone_json("pixel-1", &p, true);
        assert_eq!((v["connected"].clone(), v["in_call"].clone()), (json!(false), json!(false)));
        assert_eq!(v["mode"], json!("normal"));
    }

    #[test]
    fn the_phone_list_says_usb_or_wifi() {
        let devices = PhoneDevices::new();
        let hello = |model: &str| parse_phone_hello(&json!({"model": model, "phone_mode": "ai"}));
        devices.phones().insert("pixel-usb".into(), PhoneEntry { sink: None, hello: hello("Pixel A"), last_seen_ms: 1, wifi: false });
        devices.phones().insert("pixel-air".into(), PhoneEntry { sink: None, hello: hello("Pixel B"), last_seen_ms: 1, wifi: false });
        devices.set_wifi("pixel-air", true);
        devices.set_wifi("pixel-none", true);
        let list = devices.list_json(&[]);
        let link = |serial: &str| list.iter().find(|p| p["serial"] == serial).map(|p| p["link"].clone());
        assert_eq!(link("pixel-usb"), Some(json!("usb")));
        assert_eq!(link("pixel-air"), Some(json!("wifi")));
        assert_eq!(link("pixel-none"), None, "set_wifi does not add a phone");
        assert_eq!(devices.wifi_count(), 0, "only connected phones count");
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

    #[test]
    fn interview_prompts_in_html_match_docs_files_word_for_word() {
        let codes = ["en", "zh-TW", "zh-CN", "ja", "ko", "es", "fr", "de", "pt"];
        let prompts_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/prompts");
        for (const_name, prefix) in [
            ("const INTERVIEW = {", "interview"),
            ("const INTERVIEW_ASSISTANT = {", "interview_assistant"),
        ] {
            let start = UI_HTML.find(const_name).unwrap_or_else(|| panic!("{const_name} start"));
            let after = &UI_HTML[start + const_name.len() - 1..];
            let end = after.find("\n};").or_else(|| after.find("\r\n};")).expect("INTERVIEW end");
            // Turn the JS object literal (trailing comma before closing brace) into JSON.
            let body = after[..end].trim_end();
            let json_str = format!("{}}}", body.strip_suffix(',').unwrap_or(body));
            let map: serde_json::Map<String, Value> =
                serde_json::from_str(&json_str).expect("INTERVIEW is valid JSON once trailing comma is stripped");
            assert_eq!(map.len(), codes.len());
            for code in codes {
                let got = map
                    .get(code)
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("missing {code} in {const_name}"));
                assert!(got.contains("{owner}"), "{prefix}.{code} missing {{owner}} placeholder");
                let file = prompts_dir.join(format!("{prefix}.{code}.txt"));
                assert!(file.is_file(), "missing {}", file.display());
                let raw = std::fs::read_to_string(&file).expect("read interview file");
                let expected = raw.replace("\r\n", "\n");
                let expected = expected.trim_end_matches('\n');
                assert_eq!(got, expected, "calls.html {prefix}[{code}] drifted from {}", file.display());
            }
        }
    }

    async fn spawn_router(state: CallsState) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = router(state);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://127.0.0.1:{port}")
    }

    #[tokio::test]
    async fn delete_single_call_and_clear_finished_calls_preserve_live_and_clean_files() {
        let root = std::env::temp_dir().join(format!("bageo-cdel-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        let state = quick_state(&root);
        let base = spawn_router(state.clone()).await;
        let client = reqwest::Client::new();

        // Finished call #1 with WAV, notes, and a task.
        let mut c1 = state.store.start("pixel-test-1", "0900000111", 16000, "active").await.unwrap();
        c1.write_uplink(&vec![42u8; 3200]);
        let r1 = state.store.finalize(c1, "ended").await;
        state
            .store
            .save_notes(
                &r1.id,
                &crate::calls::CallNotes {
                    summary: "對方：測試".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let t1 = state
            .task_store
            .create(crate::tasks::TaskRecord {
                id: String::new(),
                created_at: String::new(),
                updated_at: String::new(),
                call_id: r1.id.clone(),
                serial: "pixel-test-1".into(),
                caller_id: "0900000111".into(),
                status: crate::tasks::TaskStatus::Pending,
                intent: crate::tasks::TaskIntent::Message,
                summary: "測試待辦".into(),
                transcript: String::new(),
                datetime_hint: None,
                reply_method: crate::tasks::ReplyMethod::None,
                reply_contact: None,
                contact_verified: false,
                voicemail_path: None,
                calendar_event_id: None,
                fulfill_error: None,
                openclaw_ref: None,
                events: Vec::new(),
                appointment: None,
            })
            .await
            .unwrap();

        // Download header check on /api/audio/{id}/listen?download=1.
        let dl_resp = client
            .get(format!("{base}/api/audio/{}/listen?download=1", r1.id))
            .send()
            .await
            .unwrap();
        assert_eq!(dl_resp.status().as_u16(), 200);
        assert_eq!(
            dl_resp.headers().get(header::CONTENT_DISPOSITION).and_then(|v| v.to_str().ok()),
            Some(format!("attachment; filename=\"call-{}.wav\"", r1.id).as_str())
        );

        let wav_abs = state.store.audio_abs(r1.audio_path.as_deref().unwrap()).unwrap();
        assert!(wav_abs.is_file());
        assert!(state.store.load_notes(&r1.id).await.is_some());

        // Delete finished call #1 via DELETE /api/call/{id}.
        let del_resp = client
            .delete(format!("{base}/api/call/{}", r1.id))
            .send()
            .await
            .unwrap();
        assert_eq!(del_resp.status().as_u16(), 200);
        assert!(state.store.get(&r1.id).await.is_none());
        assert!(!wav_abs.exists(), "WAV must be removed from disk");
        assert!(state.store.load_notes(&r1.id).await.is_none(), "notes must be removed");
        assert!(state.task_store.get(&t1.id).await.is_none(), "associated task must be removed");

        // Finished call #2 + live call #3: live call cannot be deleted individually,
        // and POST /api/calls/clear deletes #2 while keeping #3.
        let mut c2 = state.store.start("pixel-test-1", "0900000222", 16000, "active").await.unwrap();
        c2.write_uplink(&vec![11u8; 3200]);
        let r2 = state.store.finalize(c2, "ended").await;
        let live_call = state.store.start("pixel-test-1", "0900000333", 16000, "active").await.unwrap();

        let live_del = client
            .delete(format!("{base}/api/call/{}", live_call.id))
            .send()
            .await
            .unwrap();
        assert_eq!(live_del.status().as_u16(), 409);

        // Missing Content-Type: application/json on POST /api/calls/clear is rejected.
        let no_json = client
            .post(format!("{base}/api/calls/clear"))
            .send()
            .await
            .unwrap();
        assert_eq!(no_json.status().as_u16(), 400);

        let clear_resp = client
            .post(format!("{base}/api/calls/clear"))
            .header(header::CONTENT_TYPE, "application/json")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(clear_resp.status().as_u16(), 200);
        let clear_json: Value = clear_resp.json().await.unwrap();
        assert_eq!(clear_json["deleted"], json!(1));
        assert!(state.store.get(&r2.id).await.is_none());
        assert!(state.store.get(&live_call.id).await.is_some(), "live call must remain");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn delete_offline_device_cleans_all_stores_and_keeps_call_history() {
        let root = std::env::temp_dir().join(format!("bageo-ddel-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        let mut state = quick_state(&root);
        let sims_path = root.join("phone_sims.json");
        let sims = crate::errands::SimBook::open(sims_path.clone());
        state.errands = crate::errands::ErrandStore::open(root.join("errands.json"), sims.clone());

        let serial = "pixel-old-999";
        // Seed all 4 places + a finished call record on that device.
        state.phones.phones().insert(
            serial.to_string(),
            PhoneEntry {
                sink: None,
                hello: parse_phone_hello(&json!({"model": "Pixel Old", "phone_mode": "ai"})),
                last_seen_ms: 12345,
                wifi: false,
            },
        );
        crate::notify::remember_device(serial, "Pixel Old");
        state
            .settings_store
            .apply_patch(&json!({"device_labels": {serial: "Old Desk Phone"}}))
            .unwrap();
        sims.note(serial, 1, "tw");
        assert_eq!(sims.of(serial), vec![(1, "tw".to_string())]);

        let c = state.store.start(serial, "0900000444", 16000, "active").await.unwrap();
        let finished = state.store.finalize(c, "ended").await;

        let base = spawn_router(state.clone()).await;
        let client = reqwest::Client::new();

        // While a live call is open on another serial, deleting THAT busy serial is 409.
        let busy_serial = "pixel-busy-888";
        let _live = state.store.start(busy_serial, "0900000555", 16000, "ringing").await.unwrap();
        let busy_resp = client
            .delete(format!("{base}/api/devices/{busy_serial}"))
            .send()
            .await
            .unwrap();
        assert_eq!(busy_resp.status().as_u16(), 409);

        // Deleting the offline device succeeds and cleans all 4 stores while keeping call history.
        let ok_resp = client
            .delete(format!("{base}/api/devices/{serial}"))
            .send()
            .await
            .unwrap();
        assert_eq!(ok_resp.status().as_u16(), 200);
        assert!(state.phones.list_json(&[]).iter().all(|p| p["serial"] != serial));
        assert!(!state.settings_store.snapshot().device_labels.contains_key(serial));
        assert!(crate::notify::known_devices().iter().all(|(id, _)| id != serial));
        assert!(sims.of(serial).is_empty());
        assert!(crate::errands::SimBook::open(sims_path).of(serial).is_empty());
        assert!(state.store.get(&finished.id).await.is_some(), "call history must stay intact");

        // Deleting again returns 404.
        let again = client
            .delete(format!("{base}/api/devices/{serial}"))
            .send()
            .await
            .unwrap();
        assert_eq!(again.status().as_u16(), 404);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn sim_number_entry_survives_hello_and_errand_picks_chosen_sim() {
        let root = std::env::temp_dir().join(format!("bageo-simnum-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        let mut state = quick_state(&root);
        let sims_path = root.join("phone_sims.json");
        let sims = crate::errands::SimBook::open(sims_path.clone());
        state.errands = crate::errands::ErrandStore::open(root.join("errands.json"), sims.clone());

        let serial = "pixel-dual-777";
        sims.note_list(
            serial,
            &json!([
                {"sub_id": 20, "country": "us", "slot": 1, "carrier": "Example US", "number": "+1 202-555-0101"},
                {"sub_id": 21, "country": "tw", "slot": 0, "carrier": "Example TW"}
            ]),
        );

        let base = spawn_router(state.clone()).await;
        let client = reqwest::Client::new();

        // Owner sets the missing TW SIM number via PUT /api/devices/{serial}/sim-number.
        let put_resp = client
            .put(format!("{base}/api/devices/{serial}/sim-number"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(r#"{"sub_id": 21, "number": "0900-123-456"}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(put_resp.status().as_u16(), 200);

        // Phone reconnects with a bare hello (no number on SIM 21): owner's saved number stays!
        sims.note_list(
            serial,
            &json!([
                {"sub_id": 20, "country": "us"},
                {"sub_id": 21, "country": "tw"}
            ]),
        );

        let dev_resp: Value = client
            .get(format!("{base}/api/devices"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let list = dev_resp["sims"][serial].as_array().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0]["effective_number"], "+12025550101");
        assert_eq!(list[1]["effective_number"], "0900123456");

        // `pick_phone_sim` honors a specific SIM sub_id (both when two SIMs share a country
        // and when dialing a US local number on the US SIM while home is "tw").
        let phones = vec![crate::errands::PhoneView {
            serial: serial.into(),
            normal: false,
            busy: false,
            sims: vec![(11, "tw".into()), (20, "us".into()), (21, "tw".into())],
        }];
        let pick_tw2 = crate::errands::pick_phone_sim(&phones, "0900888999", serial, Some(21), "tw").unwrap();
        assert_eq!(pick_tw2.sub_id, Some(21));
        let pick_us = crate::errands::pick_phone_sim(&phones, "202-555-0199", serial, Some(20), "tw").unwrap();
        assert_eq!(pick_us.sub_id, Some(20));
        assert_eq!(pick_us.e164, "+12025550199");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn phone_pairing_renew_requires_json_and_rotates_key() {
        let root = std::env::temp_dir().join(format!("bageo-prenew-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        let state = quick_state(&root);
        let base = spawn_router(state.clone()).await;
        let client = reqwest::Client::new();

        // Missing Content-Type: application/json on POST /api/phone-pairing/renew is rejected (CSRF).
        let no_json = client
            .post(format!("{base}/api/phone-pairing/renew"))
            .send()
            .await
            .unwrap();
        assert_eq!(no_json.status().as_u16(), 400);

        let pairing = crate::phone_wifi::Pairing::open(&root);
        let before = pairing.key();
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, header::HeaderValue::from_static("application/json"));
        let res = phone_pairing_renew_with(&state.settings, &headers, &TokenQuery { token: None }, &pairing);
        assert_eq!(res.status().as_u16(), 200);
        let after = pairing.key();
        assert_ne!(after, before);
        assert!(!pairing.accepts(Some(&before)));
        assert!(pairing.accepts(Some(&after)));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_pairing_code_needs_the_password_and_json() {
        let root = std::env::temp_dir().join(format!("bageo-pcode-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        let state = quick_state(&root);
        let base = spawn_router(state.clone()).await;
        let client = reqwest::Client::new();
        // Through the route: no Content-Type: application/json is refused (CSRF).
        let no_json = client.post(format!("{base}/api/phone-pairing/code")).send().await.unwrap();
        assert_eq!(no_json.status().as_u16(), 400);

        let codes = crate::phone_wifi::PairCodes::new();
        let mut json_headers = HeaderMap::new();
        json_headers.insert(header::CONTENT_TYPE, header::HeaderValue::from_static("application/json"));
        let mut locked = state.settings.clone();
        locked.calls_ui_token = "pair-code-pw".into();
        let none = TokenQuery { token: None };
        let res = phone_pairing_code_with(&locked, &json_headers, &none, true, false, &codes);
        assert_eq!(res.status().as_u16(), 401, "not signed in");
        let wrong = TokenQuery { token: Some("nope".into()) };
        assert_eq!(phone_pairing_code_with(&locked, &json_headers, &wrong, true, false, &codes).status().as_u16(), 401);
        let right = TokenQuery { token: Some("pair-code-pw".into()) };
        assert_eq!(phone_pairing_code_with(&locked, &HeaderMap::new(), &right, true, false, &codes).status().as_u16(), 400, "no JSON header");
        let off = phone_pairing_code_with(&locked, &json_headers, &right, false, false, &codes);
        assert_eq!(off.status().as_u16(), 409, "phones over Wi-Fi turned off");

        // No console password: only the computer Bageo runs on gets a code.
        let mut open = state.settings.clone();
        open.calls_ui_token = String::new();
        let other = phone_pairing_code_with(&open, &json_headers, &none, true, false, &codes);
        assert_eq!(other.status().as_u16(), 403, "another device, no password");
        let body: Value = serde_json::from_slice(&axum::body::to_bytes(other.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["code"], "need_password");
        assert_eq!(phone_pairing_code_with(&open, &json_headers, &none, true, true, &codes).status().as_u16(), 200, "this computer, no password");

        // With a password, another device that signed in gets one.
        let res = phone_pairing_code_with(&locked, &json_headers, &right, true, false, &codes);
        assert_eq!(res.status().as_u16(), 200);
        let body: Value = serde_json::from_slice(&axum::body::to_bytes(res.into_body(), 4096).await.unwrap()).unwrap();
        let code = body["code"].as_str().unwrap().to_string();
        assert_eq!(code.len(), 6);
        assert_eq!(body["expires_in"], 600);
        assert!(body["lan_ips"].is_array());
        assert_eq!(codes.claim(&code, std::time::Instant::now()), Ok(()), "the code it showed works");

        let _ = std::fs::remove_dir_all(&root);
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
        .route("/api/auth-status", get(auth_status))
        .route("/api/initial-password", post(initial_password))
        .route("/api/quick-action", get(quick_action).post(quick_action))
        .route("/api/stats", get(stats))
        .route("/api/live", get(live))
        .route("/api/call/{id}", get(call_get).delete(call_delete))
        .route("/api/calls/clear", post(calls_clear))
        .route("/api/call/{id}/event.ics", get(call_event_ics))
        .route("/api/call/{id}/scam-feedback", post(call_scam_feedback))
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
        .route("/api/notify/recent", get(notify_recent))
        .route("/api/sms", get(sms_list))
        .route("/api/sms/clear", post(sms_clear))
        .route("/api/sms/threads", get(sms_threads))
        .route("/api/sms/thread", get(sms_thread).delete(sms_thread_delete))
        .route("/api/sms/send", post(sms_send))
        .route("/api/sms/{id}/retry", post(sms_retry))
        .route("/api/sms/{id}", axum::routing::delete(sms_delete))
        .route("/api/devices", get(devices))
        .route("/api/devices/{serial}", axum::routing::delete(device_delete))
        .route("/api/devices/{serial}/mode", post(device_mode))
        .route("/api/devices/{serial}/sim-number", axum::routing::put(device_sim_number))
        .route("/api/phone-pairing/renew", post(phone_pairing_renew))
        .route("/api/phone-pairing/code", post(phone_pairing_code))
        .route("/api/tasks", get(tasks_list))
        .route("/api/tasks/{id}/confirm", post(tasks_confirm))
        .route("/api/tasks/{id}/ignore", post(tasks_ignore))
        .route("/api/tasks/{id}/event.ics", get(task_event_ics))
        .route("/api/appointments/{id}/retry-sms", post(appointment_retry_sms))
        .merge(crate::contacts::router())
        .merge(crate::errands::router())
        .merge(crate::backup::router())
        .with_state(state)
}
