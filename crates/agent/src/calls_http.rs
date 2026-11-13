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
use tracing::info;

use crate::callback::ControlRegistry;
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
}

const UI_HTML: &str = include_str!("../static/calls.html");

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
    Json(json!({
        "ok": true,
        "note": "Default page player is /listen (system audio + caller, ducked). _remote.wav = caller whole call; _tts.wav = system only; _vm.wav = voicemail slice.",
        "sort": "started_at_desc",
        "calls": page.calls,
        "total": page.total,
        "limit": page.limit,
        "offset": page.offset,
        "tasks_by_call": tasks_by_call,
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
    // The filled-in Google Calendar form for the call's calendar suggestion.
    let calendar_link = notes
        .as_ref()
        .and_then(|n| n.calendar.as_ref().map(|ev| ev.google_link(&n.summary)));
    Json(json!({
        "ok": true,
        "call": rec,
        "notes": notes,
        "calendar_link": calendar_link,
    }))
    .into_response()
}

/// The call's calendar suggestion as an `.ics` file (any calendar app opens it).
async fn call_event_ics(
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
    let Some(notes) = state.store.load_notes(&id).await else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "not found");
    };
    let Some(event) = notes.calendar else {
        return api_error(StatusCode::NOT_FOUND, "no_calendar", "this call has no calendar suggestion");
    };
    let ics = event.ics(&id, &notes.summary, chrono::Utc::now().naive_utc());
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/calendar; charset=utf-8")
        .header(header::CONTENT_DISPOSITION, "attachment; filename=\"call-event.ics\"")
        .body(Body::from(ics))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
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
    let mut voice = pick(&q.voice, &rt.gemini_voice_name);
    if voice.is_empty() {
        voice = crate::gemini_live::default_voice_name().to_string();
    }
    let kind = pick(&q.ambience, &rt.ambience_kind);
    let level = q.level.unwrap_or(rt.ambience_level).clamp(1, 3);
    if voice != "none" && !crate::voice_preview::valid_voice(&voice) {
        return api_error(StatusCode::BAD_REQUEST, "unknown_voice", format!("unknown voice {voice}"));
    }
    if q.clip.as_deref() == Some("reject") && voice != "none" {
        // What a blocked caller hears: the line alone, no room under it.
        let sr = state.settings.sample_rate.max(8000);
        if let Err(e) = state
            .previews
            .ensure_reject_clip(&rt.gemini_api_key, &rt.gemini_live_model, &rt.gemini_thinking_level, &voice, sr)
            .await
        {
            tracing::warn!("reject announcement preview failed voice={voice}: {e:#}");
            return api_error(StatusCode::BAD_GATEWAY, "preview_failed", format!("{e:#}"));
        }
        let Some(pcm) = state.previews.cached_reject_clip(&rt.gemini_live_model, &voice) else {
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
    }))
    .into_response()
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
    let title = if body.title.trim().is_empty() { "Myna" } else { body.title.trim() };
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
    Json(json!({
        "ok": true,
        "usb": crate::usb_bridge::status_json(),
        "connected": connected,
        "labels": labels,
        "calls_in_progress": state.store.live_calls().await.len(),
    }))
    .into_response()
}

/// Tasks tab: prefer TaskStore (canonical); snake_case status.
async fn tasks_list(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    let tasks = state.task_store.list(200).await;
    Json(json!({"ok": true, "tasks": tasks})).into_response()
}

async fn tasks_confirm(
    State(state): State<CallsState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    Path(id): Path<String>,
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    if !task_id_safe(&id) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_id", "invalid task id");
    }
    let rt = state.settings_store.snapshot();
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
) -> Response {
    if !token_ok(&state.settings, &headers, &query) {
        return unauthorized();
    }
    if !task_id_safe(&id) {
        return api_error(StatusCode::BAD_REQUEST, "invalid_id", "invalid task id");
    }
    match on_ignore(&state.task_store, &id).await {
        Ok(task) => Json(json!({"ok": true, "task": task})).into_response(),
        Err(e) => fulfill_err_response(e),
    }
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
}

pub fn router(state: CallsState) -> Router {
    Router::new()
        .route("/", get(ui))
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
        .route("/api/tasks", get(tasks_list))
        .route("/api/tasks/{id}/confirm", post(tasks_confirm))
        .route("/api/tasks/{id}/ignore", post(tasks_ignore))
        .with_state(state)
}
