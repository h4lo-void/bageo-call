// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

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
    // The caller's contact, when the number is one (null otherwise).
    let contact = state
        .contacts
        .lookup(&rec.caller_id, rec.sim_country.as_deref())
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

    /// Whether the phone currently holds an open control connection here.
    pub fn is_connected(&self, serial: &str) -> bool {
        self.phones()
            .get(serial)
            .is_some_and(|p| p.sink.is_some())
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
         a.btn{{display:inline-block;background:#2f5e4e;color:#fff;text-decoration:none;padding:9px 18px;border-radius:8px;font-size:14px;font-weight:600}}</style></head>\
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
            store: crate::calls::CallStore::new(root.join("calls"), true, false, 30),
            settings_store: SettingsStore::load(base),
            task_store: TaskStore::new(root.join("tasks")),
            http: reqwest::Client::new(),
            control: ControlRegistry::new(),
            ambience: Arc::new(crate::ambience::AmbienceLibrary::load(root, 16000)),
            previews: Arc::new(crate::voice_preview::VoicePreviews::new(root.join("voice_preview"), root.join("voices"))),
            phones: PhoneDevices::new(),
            contacts: crate::contacts::ContactStore::open(root.join("contacts.json")),
            errands: crate::errands::ErrandStore::open(root.join("errands.json"), crate::errands::SimBook::in_memory()),
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

    #[test]
    fn interview_prompts_in_html_match_docs_files_word_for_word() {
        let codes = ["en", "zh-TW", "zh-CN", "ja", "ko", "es", "fr", "de", "pt"];
        let start = UI_HTML.find("const INTERVIEW = {").expect("INTERVIEW start");
        let after = &UI_HTML[start + "const INTERVIEW = ".len()..];
        let end = after.find("\n};").or_else(|| after.find("\r\n};")).expect("INTERVIEW end");
        // Turn the JS object literal (trailing comma before closing brace) into JSON.
        let body = after[..end].trim_end();
        let json_str = format!("{}}}", body.strip_suffix(',').unwrap_or(body));
        let map: serde_json::Map<String, Value> =
            serde_json::from_str(&json_str).expect("INTERVIEW is valid JSON once trailing comma is stripped");
        assert_eq!(map.len(), codes.len());
        let prompts_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/prompts");
        for code in codes {
            let got = map
                .get(code)
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("missing {code} in INTERVIEW"));
            assert!(got.contains("{owner}"), "{code} missing {{owner}} placeholder");
            let file = prompts_dir.join(format!("interview.{code}.txt"));
            if file.is_file() {
                let raw = std::fs::read_to_string(&file).expect("read interview file");
                let expected = raw.replace("\r\n", "\n");
                let expected = expected.trim_end_matches('\n');
                assert_eq!(got, expected, "calls.html INTERVIEW[{code}] drifted from {}", file.display());
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
        .route("/api/devices", get(devices))
        .route("/api/devices/{serial}", axum::routing::delete(device_delete))
        .route("/api/devices/{serial}/mode", post(device_mode))
        .route("/api/devices/{serial}/sim-number", axum::routing::put(device_sim_number))
        .route("/api/tasks", get(tasks_list))
        .route("/api/tasks/{id}/confirm", post(tasks_confirm))
        .route("/api/tasks/{id}/ignore", post(tasks_ignore))
        .route("/api/tasks/{id}/event.ics", get(task_event_ics))
        .route("/api/appointments/{id}/retry-sms", post(appointment_retry_sms))
        .merge(crate::contacts::router())
        .merge(crate::errands::router())
        .with_state(state)
}
