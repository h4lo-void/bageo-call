// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Embedded standalone runtime support (Android all-in-one mode) and `incoming_sms`
//! control-channel handling.
//!
//! The standalone edition opens no console port. The app hands each console
//! request straight to the agent's routes ([`EmbeddedControl::http`], called over
//! JNI), so only the Bageo app itself can read calls or change settings. The one
//! socket left is the phone bridge on loopback ([`LOCAL_BRIDGE_PORT`]) that the app's
//! own call service uses; it serves only the phone WebSocket and refuses every
//! request that does not start the link check ([`crate::phone_link`]) with the key
//! made at start. The key lives in memory only, reaches the app over JNI once the
//! bridge listens, and never goes over the socket.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::handler::Handler;
use axum::http::{header, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{any_service, get, MethodRouter};
use axum::Router;
use serde_json::{json, Value};
use tracing::{debug, error, info, warn};

use crate::calls::CallStore;
use crate::config::{EmbeddedOverrides, Settings};
use crate::settings_store::{self, SettingsStore};
use crate::{bootstrap_runtime, calendar, lan, locale, notify, recent_notices, sms_store, AppState};

/// Loopback port of the phone bridge used by Standalone Android (`ws://127.0.0.1:8766`),
/// apart from the desktop `adb reverse tcp:8765` so the two never collide when the
/// phone is plugged into a PC.
pub const LOCAL_BRIDGE_PORT: u16 = 8766;

/// Header older apps sent the bridge key in. No longer taken: the key never
/// goes over the socket ([`crate::phone_link`]).
pub const BRIDGE_KEY_HEADER: &str = "x-bageo-bridge-key";

/// Fills `Settings.port` (links in settings answers). Nothing listens on it.
const NOMINAL_CONSOLE_PORT: u16 = 8787;

/// Largest answer handed back to the app in one piece (backups, recordings).
const MAX_LOCAL_BODY: usize = 512 * 1024 * 1024;

/// Runtime handles captured once the embedded agent has initialized its stores.
#[derive(Clone)]
pub struct EmbeddedHandles {
    pub settings: Settings,
    pub settings_store: Arc<SettingsStore>,
    pub calls: Arc<CallStore>,
    /// The agent's routes (the same ones [`crate::build_app`] gives a desktop agent).
    pub app: Router,
    /// The embedded Tokio runtime, for requests coming in from app threads.
    pub runtime: tokio::runtime::Handle,
}

/// Shared state between the Android JNI bridge and the embedded Tokio server.
pub struct EmbeddedControl {
    pub data_dir: PathBuf,
    pub assets_dir: PathBuf,
    /// Stores are open and the phone bridge listens.
    pub ready: AtomicBool,
    /// The agent's thread is running (false once it stopped; [`start`] runs it again).
    pub alive: AtomicBool,
    bridge_key: String,
    pub handles: Mutex<Option<EmbeddedHandles>>,
}

impl EmbeddedControl {
    pub fn new(data_dir: PathBuf, assets_dir: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            data_dir,
            assets_dir,
            ready: AtomicBool::new(false),
            alive: AtomicBool::new(false),
            bridge_key: new_bridge_key(),
            handles: Mutex::new(None),
        })
    }

    /// The key the app's call service checks [`LOCAL_BRIDGE_PORT`] with. A new one
    /// each start; empty until the bridge listens, so the app never talks to
    /// whatever holds the port before that.
    pub fn bridge_key(&self) -> &str {
        if self.ready.load(Ordering::Acquire) {
            &self.bridge_key
        } else {
            ""
        }
    }

    fn handles(&self) -> Option<EmbeddedHandles> {
        self.handles.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn apply_settings_patch(&self, patch_json: &str) -> Result<()> {
        let mut patch: Value = serde_json::from_str(patch_json)?;
        let Some(obj) = patch.as_object_mut() else {
            anyhow::bail!("settings patch must be a JSON object");
        };
        obj.insert("gemini_live_enabled".to_string(), Value::Bool(true));
        let handles = self.handles().ok_or_else(|| anyhow::anyhow!("agent not ready"))?;
        handles.settings_store.apply_patch(&patch)?;
        Ok(())
    }

    pub fn status_json(&self) -> Value {
        let active_calls = self.handles().map(|h| h.calls.live_calls_count_try()).unwrap_or(0);
        json!({
            "running": self.alive.load(Ordering::Acquire),
            "ready": self.ready.load(Ordering::Acquire),
            "active_calls": active_calls,
        })
    }

    /// Answer one console request from the app (blocks the calling thread,
    /// which must not be a Tokio worker). The console password, when one is
    /// set, goes with it: the app never needs to know it.
    pub fn http(&self, req: LocalRequest) -> LocalResponse {
        let Some(h) = self.handles() else {
            return LocalResponse::not_ready();
        };
        let token = settings_store::active_ui_token(&h.settings.calls_ui_token);
        h.runtime.block_on(dispatch_local(h.app, &token, req))
    }
}

/// 32 random bytes as hex.
fn new_bridge_key() -> String {
    let mut bytes = [0u8; 32];
    if getrandom::getrandom(&mut bytes).is_err() {
        bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The running agent, if any.
static RUNNING: Mutex<Option<Arc<EmbeddedControl>>> = Mutex::new(None);

/// The agent while its thread runs.
pub fn current() -> Option<Arc<EmbeddedControl>> {
    RUNNING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .filter(|c| c.alive.load(Ordering::Acquire))
}

/// The status the app shows, also before the agent ever started.
pub fn status_json() -> Value {
    match current() {
        Some(c) => c.status_json(),
        None => json!({ "running": false, "ready": false, "active_calls": 0 }),
    }
}

/// Start the agent on its own thread, or hand back the one already running.
/// An agent that stopped (start failed, the bridge closed) is started anew,
/// with a new bridge key. None when no thread could be made.
pub fn start(data_dir: PathBuf, assets_dir: PathBuf) -> Option<Arc<EmbeddedControl>> {
    let mut slot = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(running) = slot.as_ref().filter(|c| c.alive.load(Ordering::Acquire)) {
        return Some(running.clone());
    }
    let ctrl = EmbeddedControl::new(data_dir, assets_dir);
    ctrl.alive.store(true, Ordering::Release);
    *slot = Some(ctrl.clone());
    drop(slot);

    let run = ctrl.clone();
    let spawned = std::thread::Builder::new()
        .name("bageo-embedded".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("bageo-tokio")
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    error!("failed to build tokio runtime: {e}");
                    stopped(&run);
                    return;
                }
            };
            info!("starting embedded Bageo agent");
            if let Err(e) = rt.block_on(run_embedded(run.clone())) {
                error!("embedded Bageo agent stopped: {e:#}; it starts again when the app asks");
            }
            stopped(&run);
            rt.shutdown_timeout(Duration::from_secs(2));
        });
    if spawned.is_err() {
        stopped(&ctrl);
        return None;
    }
    Some(ctrl)
}

/// The agent's thread ended: requests get "not ready" and the next [`start`] runs it again.
fn stopped(ctrl: &Arc<EmbeddedControl>) {
    ctrl.ready.store(false, Ordering::Release);
    *ctrl.handles.lock().unwrap_or_else(|e| e.into_inner()) = None;
    ctrl.alive.store(false, Ordering::Release);
    let mut slot = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    if slot.as_ref().is_some_and(|c| Arc::ptr_eq(c, ctrl)) {
        *slot = None;
    }
}

/// One console request from the app.
#[derive(Debug, Clone, Default)]
pub struct LocalRequest {
    pub method: String,
    /// Path and query, e.g. `/calls/api/list?limit=20`.
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// The agent's answer to a [`LocalRequest`].
#[derive(Debug, Clone)]
pub struct LocalResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl LocalResponse {
    fn error(status: StatusCode, code: &str) -> Self {
        Self {
            status: status.as_u16(),
            headers: vec![("content-type".into(), "application/json".into())],
            body: json!({ "ok": false, "code": code, "error": code }).to_string().into_bytes(),
        }
    }

    /// The agent is starting or stopped.
    pub fn not_ready() -> Self {
        Self::error(StatusCode::SERVICE_UNAVAILABLE, "agent_not_ready")
    }

    /// The request could not be answered (a panic while handling it).
    pub fn failed() -> Self {
        Self::error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
    }

    /// First value of the header `name` (any case).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// For the app: a 4-byte big-endian length of the head, the head in UTF-8
    /// (the status, then one `name: value` per line), then the body as is.
    pub fn encode(&self) -> Vec<u8> {
        let mut head = format!("{}\n", self.status);
        for (name, value) in &self.headers {
            let value: String = value.chars().filter(|c| *c != '\r' && *c != '\n').collect();
            head.push_str(name);
            head.push_str(": ");
            head.push_str(&value);
            head.push('\n');
        }
        let head = head.into_bytes();
        let mut out = Vec::with_capacity(4 + head.len() + self.body.len());
        out.extend_from_slice(&(head.len() as u32).to_be_bytes());
        out.extend_from_slice(&head);
        out.extend_from_slice(&self.body);
        out
    }
}

/// The app's request headers, one `name: value` per line in UTF-8.
pub fn parse_header_lines(raw: &[u8]) -> Vec<(String, String)> {
    String::from_utf8_lossy(raw)
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            let name = name.trim();
            (!name.is_empty()).then(|| (name.to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect()
}

/// Headers an app request never passes on: the host and the console password
/// are set here, a page's origin or a proxy header would make the agent treat
/// the request as coming from elsewhere, and the app's web view can take
/// neither compressed answers nor a 304.
fn dropped_header(name: &str) -> bool {
    matches!(
        name,
        "host"
            | "origin"
            | "referer"
            | "cookie"
            | "authorization"
            | "x-calls-token"
            | "connection"
            | "keep-alive"
            | "upgrade"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "content-length"
            | "accept-encoding"
            | "if-none-match"
            | "if-modified-since"
            | "if-match"
            | "if-unmodified-since"
            | "if-range"
            | "forwarded"
            | "via"
            | "x-real-ip"
            | "cf-connecting-ip"
            | "true-client-ip"
    ) || name.starts_with("x-forwarded-")
        || name.starts_with("proxy-")
        || name.starts_with("sec-")
}

/// Run one app request through the agent's routes as a request from this
/// device itself, with `ui_token` (the console password, or empty) attached.
pub async fn dispatch_local(app: Router, ui_token: &str, req: LocalRequest) -> LocalResponse {
    let bad = || LocalResponse::error(StatusCode::BAD_REQUEST, "bad_request");
    let Ok(method) = Method::from_bytes(req.method.trim().as_bytes()) else {
        return bad();
    };
    let target = req.target.trim();
    if !target.starts_with('/') || target.starts_with("//") {
        return bad();
    }
    let Ok(uri) = target.parse::<Uri>() else {
        return bad();
    };
    let mut builder = axum::http::Request::builder().method(method).uri(uri);
    for (name, value) in &req.headers {
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty() || dropped_header(&name) {
            continue;
        }
        if let (Ok(n), Ok(v)) = (HeaderName::from_bytes(name.as_bytes()), HeaderValue::from_bytes(value.trim().as_bytes())) {
            builder = builder.header(n, v);
        }
    }
    builder = builder.header(header::HOST, "localhost");
    if let Ok(v) = HeaderValue::from_str(ui_token.trim()) {
        if !v.is_empty() {
            builder = builder.header("x-calls-token", v);
        }
    }
    if !req.body.is_empty() {
        builder = builder.header(header::CONTENT_LENGTH, req.body.len());
    }
    let Ok(mut request) = builder.body(Body::from(req.body)) else {
        return bad();
    };
    let this_device = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    request.extensions_mut().insert(ConnectInfo(lan::ConnAddrs {
        peer: this_device,
        local: Some(this_device),
    }));
    let routes: MethodRouter<()> = any_service(app);
    let response = Handler::call(routes, request, ()).await;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(n, v)| (n.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
        .collect();
    match axum::body::to_bytes(response.into_body(), MAX_LOCAL_BODY).await {
        Ok(body) => LocalResponse { status, headers, body: body.to_vec() },
        Err(e) => {
            warn!("in-app console answer could not be read ({e})");
            LocalResponse::failed()
        }
    }
}

/// The routes on [`LOCAL_BRIDGE_PORT`]: only the phone WebSocket (`/` and `/ws`),
/// and nothing at all without the link check with `key` ([`crate::phone_link`]).
pub(crate) fn phone_bridge_app(state: AppState, key: &str) -> Router {
    let key: Arc<str> = Arc::from(key.trim());
    Router::new()
        .route("/", get(crate::root))
        .route("/ws", get(crate::ws_upgrade))
        .with_state(state)
        .layer(axum::middleware::from_fn_with_state(key, require_link_check))
}

/// Every request must start the link check; the WebSocket handler then proves
/// this agent with `key` and the phone's hello must prove the app.
async fn require_link_check(State(key): State<Arc<str>>, mut req: Request, next: Next) -> Response {
    if key.is_empty() || crate::phone_link::requested_cnonce(req.headers()).is_none() {
        debug!("phone bridge refused a request without the link check");
        return StatusCode::UNAUTHORIZED.into_response();
    }
    req.extensions_mut().insert(crate::phone_link::KeyOverride(key.clone()));
    next.run(req).await
}

/// Serve the phone bridge until it fails.
pub(crate) async fn serve_phone_bridge(listener: tokio::net::TcpListener, app: Router) -> Result<()> {
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

/// Bind a reusable TCP listener so a restarted agent can take the port again
/// at once, while the old sockets linger.
pub fn bind_tcp_reusable(addr: SocketAddr) -> Result<tokio::net::TcpListener> {
    let domain = if addr.is_ipv4() {
        socket2::Domain::IPV4
    } else {
        socket2::Domain::IPV6
    };
    let sock = socket2::Socket::new(domain, socket2::Type::STREAM, Some(socket2::Protocol::TCP))?;
    sock.set_reuse_address(true)?;
    sock.set_nonblocking(true)?;
    sock.bind(&addr.into())?;
    sock.listen(128)?;
    let std_listener: std::net::TcpListener = sock.into();
    Ok(tokio::net::TcpListener::from_std(std_listener)?)
}

/// [`bind_tcp_reusable`], trying again for a few seconds (an agent that just stopped
/// may still hold the port).
async fn bind_with_retries(addr: SocketAddr) -> Result<tokio::net::TcpListener> {
    let mut delay = Duration::from_millis(250);
    let mut tries = 0;
    loop {
        match bind_tcp_reusable(addr) {
            Ok(listener) => return Ok(listener),
            Err(e) if tries < 8 => {
                warn!("embedded phone bridge cannot listen on {addr} yet ({e}); trying again");
                tries += 1;
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(2));
            }
            Err(e) => return Err(e.context(format!("phone bridge cannot listen on {addr}"))),
        }
    }
}

/// Run the full Bageo agent inside the Android application process. Returns
/// only when it cannot go on (start failed, the phone bridge stopped).
pub async fn run_embedded(cfg: Arc<EmbeddedControl>) -> Result<()> {
    crate::config::set_embedded_overrides(EmbeddedOverrides {
        data_dir: cfg.data_dir.clone(),
        assets_dir: cfg.assets_dir.clone(),
        port: NOMINAL_CONSOLE_PORT,
    });
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (state, calls_state, remote) = bootstrap_runtime().await?;
    let settings = state.settings.clone();
    let settings_store = state.settings_store.clone();
    let calls = state.calls.clone();

    // The console opens only inside the app: no Wi-Fi entry for phones and no
    // network name (also turning off what an older version may have saved).
    // `gemini_live_enabled` stays on: the app's mode switch chooses AI answering
    // or a normal phone.
    let _ = settings_store.apply_patch(&json!({
        "gemini_live_enabled": true,
        "phone_wifi": false,
        "mdns_name": "off",
    }));

    calendar::set_link_secret(&settings_store::active_ui_token(&settings.calls_ui_token));

    let app = crate::build_app(state.clone(), calls_state, remote, true);
    let bridge_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), LOCAL_BRIDGE_PORT);
    let listener = bind_with_retries(bridge_addr).await?;
    let bridge = phone_bridge_app(state.clone(), &cfg.bridge_key);

    *cfg.handles.lock().unwrap_or_else(|e| e.into_inner()) = Some(EmbeddedHandles {
        settings,
        settings_store,
        calls,
        app,
        runtime: tokio::runtime::Handle::current(),
    });
    cfg.ready.store(true, Ordering::Release);
    info!("embedded Bageo ready: console inside the app only, phone bridge on ws://{bridge_addr} (link check required)");

    serve_phone_bridge(listener, bridge).await?;
    anyhow::bail!("the phone bridge stopped")
}

/// Core-language labels for incoming SMS notifications (matching `sms_forward.py`).
pub fn sms_notify_labels(lang: locale::Lang) -> (&'static str, &'static str, &'static str) {
    // (title, to_label, text_label)
    match lang {
        locale::Lang::ZhTw => ("新簡訊", "收件號碼", "內容"),
        locale::Lang::ZhCn => ("新短信", "接收号码", "内容"),
        locale::Lang::Ja => ("新着SMS", "宛先", "本文"),
        locale::Lang::Ko => ("새 문자", "받은 번호", "내용"),
        locale::Lang::Es => ("Nuevo SMS", "Para", "Mensaje"),
        locale::Lang::Fr => ("Nouveau SMS", "À", "Message"),
        locale::Lang::De => ("Neue SMS", "An", "Nachricht"),
        locale::Lang::Pt => ("Novo SMS", "Para", "Mensagem"),
        locale::Lang::En | locale::Lang::Ext(_) => ("New SMS", "To", "Message"),
    }
}

/// Format an incoming SMS notification in `locale::notify_lang()` without emojis.
pub fn format_incoming_sms_notification(
    from: &str,
    to: &str,
    device: &str,
    text: &str,
) -> (String, String) {
    let lang = locale::notify_lang();
    let w = locale::notify_words();
    let sep = &w.sep;
    let (title, to_lbl, text_lbl) = sms_notify_labels(lang);

    let sender = if from.trim().is_empty() {
        w.n_unknown.as_str()
    } else {
        from.trim()
    };
    let mut lines = vec![format!("{}{sep}{sender}", w.n_number)];
    if !to.trim().is_empty() {
        lines.push(format!("{to_lbl}{sep}{}", to.trim()));
    }
    if !device.trim().is_empty() {
        lines.push(format!("{}{sep}{}", w.n_phone, device.trim()));
    }
    lines.push(format!("{text_lbl}{sep}{}", text.trim()));
    (title.to_string(), lines.join("\n"))
}

/// Handle `{"type":"incoming_sms", ...}` sent by the phone over `CONTROL_IDLE`.
/// Stores the message into `SmsStore`, records a recent notice, and pushes it to
/// configured notification channels. The phone app keeps no texts
/// ([`sms_store::SmsStore::off`]): it is taken (so nothing sends it again) and dropped.
pub(crate) async fn handle_incoming_sms(state: &AppState, serial: &str, msg: &Value) -> Value {
    let id = msg
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if state.sms.is_off() {
        return json!({
            "type": "incoming_sms_ack",
            "id": id,
            "ok": true,
            "stored": false,
            "notified": false,
        });
    }
    if id.is_empty() {
        return json!({
            "type": "incoming_sms_ack",
            "id": "",
            "ok": false,
            "stored": false,
            "notified": false,
            "error": "missing_id",
        });
    }

    let from = msg.get("from").and_then(Value::as_str).unwrap_or("").trim();
    let to = msg.get("to").and_then(Value::as_str).unwrap_or("").trim();
    let text = msg.get("text").and_then(Value::as_str).unwrap_or("");
    let received_ms = msg.get("received_ms").and_then(Value::as_u64).unwrap_or(0);
    let sub_id = msg.get("sub_id").and_then(Value::as_i64).filter(|s| *s >= 0);

    let canon_serial = crate::canonical_device_serial(serial);
    let known = notify::known_devices();
    let device = known
        .iter()
        .find(|(s, _)| s == &canon_serial || s == serial)
        .map(|(_, label)| label.trim().to_string())
        .filter(|l| !l.is_empty())
        .or_else(|| state.phones.model_of(serial))
        .or_else(|| {
            msg.get("device")
                .and_then(Value::as_str)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| canon_serial.clone());

    let reference = format!("phone:{canon_serial}:{id}");
    let rt = state.settings_store.snapshot();
    let pushed = state.sms.push(
        sms_store::now_ms(),
        rt.retention_days,
        sms_store::NewSms {
            received_ms,
            from,
            to,
            text,
            serial: &canon_serial,
            device: &device,
            reference: &reference,
            sub_id,
        },
    );

    let eff = rt.effective(&state.settings);
    let (title, body) = format_incoming_sms_notification(from, to, &device, text);
    if pushed != sms_store::Pushed::Duplicate {
        recent_notices::record("sms", &title, from, text, &body);
    }

    let channels_on = notify::notify_configured(&eff);
    let notified = if channels_on {
        notify::notify_event(&state.http, &eff, &title, &body).await
    } else {
        false
    };
    let ok = !channels_on || notified;

    info!(
        "incoming_sms serial={} id={} stored={} duplicate={} channels_on={} notified={}",
        canon_serial,
        id,
        pushed != sms_store::Pushed::Duplicate,
        pushed == sms_store::Pushed::Duplicate,
        channels_on,
        notified
    );

    json!({
        "type": "incoming_sms_ack",
        "id": id,
        "ok": ok,
        "stored": true,
        "notified": notified,
    })
}

/// Rolling file writer for Android embedded logs (`<data_dir>/bageo_agent.log`, max 2 MB).
pub struct RollingLogWriter {
    path: PathBuf,
    max_bytes: u64,
    lock: Mutex<()>,
}

impl RollingLogWriter {
    pub fn new(path: PathBuf, max_bytes: u64) -> Self {
        Self {
            path,
            max_bytes,
            lock: Mutex::new(()),
        }
    }
}

impl std::io::Write for RollingLogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(meta) = std::fs::metadata(&self.path) {
            if meta.len() >= self.max_bytes {
                let old = self.path.with_extension("log.1");
                let _ = std::fs::rename(&self.path, old);
            }
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        f.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for RollingLogWriter {
    type Writer = RollingLogWriterHandle<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        RollingLogWriterHandle { inner: self }
    }
}

pub struct RollingLogWriterHandle<'a> {
    inner: &'a RollingLogWriter,
}

impl<'a> std::io::Write for RollingLogWriterHandle<'a> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _guard = self.inner.lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(parent) = self.inner.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(meta) = std::fs::metadata(&self.inner.path) {
            if meta.len() >= self.inner.max_bytes {
                let old = self.inner.path.with_extension("log.1");
                let _ = std::fs::rename(&self.inner.path, old);
            }
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.inner.path)?;
        f.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Initialize tracing into `<data_dir>/bageo_agent.log` once for Android embedded builds.
pub fn init_embedded_logging(data_dir: &Path) {
    static LOG_ONCE: std::sync::Once = std::sync::Once::new();
    LOG_ONCE.call_once(|| {
        let writer = RollingLogWriter::new(data_dir.join("bageo_agent.log"), 2 * 1024 * 1024);
        let _ = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(writer)
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "agent=info,bageo_agent=info".into()),
            )
            .try_init();
        crate::log_text::announce();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health_access_tests::make_test_app_state;
    use serde_json::json;

    #[test]
    fn format_incoming_sms_notification_across_languages() {
        for lang in [
            locale::Lang::ZhTw,
            locale::Lang::ZhCn,
            locale::Lang::En,
            locale::Lang::Ja,
            locale::Lang::Ko,
            locale::Lang::Es,
            locale::Lang::Fr,
            locale::Lang::De,
            locale::Lang::Pt,
        ] {
            let (title, _, _) = sms_notify_labels(lang);
            assert!(!title.is_empty(), "lang={}", lang.code());
        }
        let (title, body) = format_incoming_sms_notification("0912345678", "", "Phone A", "Code 123456");
        assert!(!title.is_empty());
        assert!(body.contains("0912345678"));
        assert!(body.contains("Phone A"));
        assert!(body.contains("Code 123456"));
    }

    #[tokio::test]
    async fn incoming_sms_stores_once_and_deduplicates_retries() {
        let (state, root) = make_test_app_state("incoming-sms");
        let msg = json!({
            "type": "incoming_sms",
            "id": "inbox:42",
            "from": "0912345678",
            "to": "SIM 1",
            "text": "Hello test SMS",
            "received_ms": sms_store::now_ms(),
            "sub_id": 2,
            "device": "Pixel Test"
        });
        let ack1 = handle_incoming_sms(&state, "pixel-123", &msg).await;
        assert_eq!(ack1["type"], "incoming_sms_ack");
        assert_eq!(ack1["id"], "inbox:42");
        assert_eq!(ack1["ok"], true);
        assert_eq!(ack1["stored"], true);
        assert_eq!(ack1["notified"], false);

        // Retry with the same id must deduplicate in SmsStore and still ack ok.
        let ack2 = handle_incoming_sms(&state, "pixel-123", &msg).await;
        assert_eq!(ack2["ok"], true);
        assert_eq!(ack2["stored"], true);

        let page = state.sms.list(sms_store::now_ms(), 15, "", None, 20, &|_| String::new());
        assert_eq!(page.total, 1);
        assert_eq!(page.messages[0].text, "Hello test SMS");
        assert_eq!(page.messages[0].sub_id, Some(2));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The agent's real routes over a temp data folder, with `password` as the console password.
    fn full_app(tag: &str, password: &str) -> (Router, AppState, PathBuf) {
        let (mut state, root) = make_test_app_state(tag);
        state.settings.calls_ui_token = password.to_string();
        let calls_state = crate::calls_http::CallsState {
            settings: state.settings.clone(),
            store: state.calls.clone(),
            settings_store: state.settings_store.clone(),
            task_store: state.task_store.clone(),
            http: state.http.clone(),
            control: state.control.clone(),
            ambience: state.ambience.clone(),
            previews: state.previews.clone(),
            phones: state.phones.clone(),
            contacts: crate::contacts::ContactStore::open(root.join("contacts.json")),
            errands: state.errands.clone(),
            sms: state.sms.clone(),
        };
        let remote = crate::control::Controller::new(calls_state.clone());
        let app = crate::build_app(state.clone(), calls_state, remote, true);
        (app, state, root)
    }

    /// What [`EmbeddedControl::http`] attaches: the console password in effect.
    fn token(state: &AppState) -> String {
        settings_store::active_ui_token(&state.settings.calls_ui_token)
    }

    fn req(method: &str, target: &str, headers: &[(&str, &str)], body: &[u8]) -> LocalRequest {
        LocalRequest {
            method: method.into(),
            target: target.into(),
            headers: headers.iter().map(|(n, v)| (n.to_string(), v.to_string())).collect(),
            body: body.to_vec(),
        }
    }

    fn body_json(r: &LocalResponse) -> Value {
        serde_json::from_slice(&r.body).unwrap_or(Value::Null)
    }

    #[tokio::test]
    async fn the_app_reads_and_changes_data_without_any_port() {
        let (app, state, root) = full_app("emb-http", "");
        let tok = token(&state);
        let json_ct = ("content-type", "application/json");
        // The page's origin and a foreign host name are not passed on: the request is the app's own.
        let created = dispatch_local(
            app.clone(),
            &tok,
            req(
                "POST",
                "/calls/api/contacts",
                &[json_ct, ("origin", "https://appassets.androidplatform.net"), ("host", "evil.example")],
                json!({"name": "王小明", "phones": ["+1 555 0100"], "note": "José"}).to_string().as_bytes(),
            ),
        )
        .await;
        assert_eq!(created.status, 200, "{}", String::from_utf8_lossy(&created.body));
        let id = body_json(&created)["contact"]["id"].as_str().expect("contact id").to_string();

        // Text in any language comes back byte for byte.
        for note in ["Müller よろしく 😀", "안녕하세요 señora", "Привет 你好", "مرحبا"] {
            let updated = dispatch_local(
                app.clone(),
                &tok,
                req(
                    "PUT",
                    &format!("/calls/api/contacts/{id}"),
                    &[json_ct],
                    json!({"name": "王小明", "phones": ["+1 555 0100"], "note": note}).to_string().as_bytes(),
                ),
            )
            .await;
            assert_eq!(updated.status, 200, "{}", String::from_utf8_lossy(&updated.body));
            assert_eq!(body_json(&updated)["contact"]["note"], note);
        }

        let listed = dispatch_local(app.clone(), &tok, req("GET", "/calls/api/contacts?q=%E7%8E%8B", &[], b"")).await;
        assert_eq!(listed.status, 200);
        assert!(listed.header("Content-Type").is_some_and(|v| v.starts_with("application/json")));
        assert!(String::from_utf8_lossy(&listed.body).contains("王小明"));

        let missing = dispatch_local(app.clone(), &tok, req("GET", "/calls/api/no-such-page", &[], b"")).await;
        assert_eq!(missing.status, 404);
        // Only a path on this agent, never an address somewhere else.
        for target in ["https://example.com/calls/", "//example.com/calls/", "calls/api/contacts"] {
            assert_eq!(dispatch_local(app.clone(), &tok, req("GET", target, &[], b"")).await.status, 400, "{target}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_recording_plays_from_any_point_and_pages_are_never_a_cached_copy() {
        let (app, state, root) = full_app("emb-range", "");
        let tok = token(&state);
        let mut call = state.calls.start("pixel-test-1", "+1 555 0101", 16000, "active").await.unwrap();
        call.write_uplink(&vec![42u8; 3200]);
        let rec = state.calls.finalize(call, "ended").await;
        let path = format!("/calls/api/audio/{}/listen", rec.id);

        let whole = dispatch_local(app.clone(), &tok, req("GET", &path, &[], b"")).await;
        assert_eq!(whole.status, 200);
        assert_eq!(whole.header("accept-ranges"), Some("bytes"));
        let len = whole.body.len();
        assert!(len > 20);
        let part = dispatch_local(app.clone(), &tok, req("GET", &path, &[("Range", "bytes=10-19")], b"")).await;
        assert_eq!(part.status, 206);
        assert_eq!(part.header("content-range"), Some(format!("bytes 10-19/{len}").as_str()));
        assert_eq!(&part.body[..], &whole.body[10..20]);

        // The web view can take neither a 304 nor a compressed page.
        let page = dispatch_local(app.clone(), &tok, req("GET", "/calls/mobile", &[], b"")).await;
        assert_eq!(page.status, 200);
        let etag = page.header("etag").expect("the page has an ETag").to_string();
        let again = dispatch_local(
            app,
            &tok,
            req("GET", "/calls/mobile", &[("If-None-Match", &etag), ("Accept-Encoding", "gzip")], b""),
        )
        .await;
        assert_eq!(again.status, 200);
        assert_eq!(again.header("content-encoding"), None);
        assert_eq!(again.body, page.body);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn the_console_password_goes_along_only_from_the_app_side() {
        let pw = "emb-test-pass-1234";
        let (app, state, root) = full_app("emb-pw", pw);
        let ok = dispatch_local(app.clone(), &token(&state), req("GET", "/calls/api/contacts", &[], b"")).await;
        assert_eq!(ok.status, 200);
        // A password written by the page is not passed on.
        let bearer = format!("Bearer {pw}");
        let page_only = dispatch_local(
            app,
            "",
            req("GET", "/calls/api/contacts", &[("x-calls-token", pw), ("authorization", &bearer)], b""),
        )
        .await;
        assert_eq!(page_only.status, 401);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn the_phone_bridge_needs_the_link_check_and_serves_only_calls() {
        use crate::phone_link::{HEADER_CNONCE, HEADER_LINK, HEADER_PROOF, HEADER_SNONCE};
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::Error as WsError;
        use tokio_tungstenite::tungstenite::Message as WsMessage;

        let (state, root) = make_test_app_state("emb-bridge");
        let key = new_bridge_key();
        assert_eq!(key.len(), 64);
        assert_ne!(key, new_bridge_key());
        let listener = bind_tcp_reusable(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(serve_phone_bridge(listener, phone_bridge_app(state, &key)));

        // No link check: refused, also with the old header carrying the right key.
        for path in ["/", "/ws"] {
            for old in [None, Some(key.clone())] {
                let mut rq = format!("ws://127.0.0.1:{port}{path}").into_client_request().unwrap();
                if let Some(k) = old {
                    rq.headers_mut().insert(BRIDGE_KEY_HEADER, k.parse().unwrap());
                }
                match tokio_tungstenite::connect_async(rq).await {
                    Err(WsError::Http(resp)) => assert_eq!(resp.status().as_u16(), 401),
                    other => panic!("{path} opened without the link check: {:?}", other.map(|_| ())),
                }
            }
        }
        let cnonce = "c1".repeat(32);
        let mut rq = format!("ws://127.0.0.1:{port}/ws").into_client_request().unwrap();
        rq.headers_mut().insert(HEADER_LINK, "1".parse().unwrap());
        rq.headers_mut().insert(HEADER_CNONCE, cnonce.parse().unwrap());
        let (mut ws, resp) = tokio_tungstenite::connect_async(rq).await.expect("the link check opens the bridge");
        assert_eq!(resp.status().as_u16(), 101);
        let snonce = resp.headers().get(HEADER_SNONCE).unwrap().to_str().unwrap().to_string();
        let proof = resp.headers().get(HEADER_PROOF).unwrap().to_str().unwrap().to_string();
        let (host, phone) = crate::phone_link::proofs(&key, &cnonce, &snonce).unwrap();
        assert_eq!(proof, host, "the agent proves it holds this start's key");
        assert!(!resp.headers().contains_key(BRIDGE_KEY_HEADER));
        // The app's proof in its hello: the agent answers it.
        let hello = json!({"type": "hello", "serial": "emb-test", "mode": "CONTROL_IDLE", "link_proof": phone});
        ws.send(WsMessage::Text(hello.to_string().into())).await.unwrap();
        let mut acked = false;
        for _ in 0..6 {
            match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
                Ok(Some(Ok(WsMessage::Text(t)))) if t.as_str().contains("hello_ack") => {
                    acked = true;
                    break;
                }
                Ok(Some(Ok(_))) => continue,
                _ => break,
            }
        }
        assert!(acked, "a right link_proof is taken");
        let _ = ws.close(None).await;

        // The console is not on the bridge, even with the link check.
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{port}/calls/api/settings");
        let checked = client.get(&url).header(HEADER_LINK, "1").header(HEADER_CNONCE, &cnonce).send().await.unwrap();
        assert_eq!(checked.status().as_u16(), 404);
        let without = client.get(&url).send().await.unwrap();
        assert_eq!(without.status().as_u16(), 401);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_bridge_key_is_given_out_only_once_the_bridge_listens() {
        let ctrl = EmbeddedControl::new(PathBuf::from("/path/to/data"), PathBuf::from("/path/to/assets"));
        ctrl.alive.store(true, Ordering::Release);
        assert_eq!(ctrl.bridge_key(), "", "starting: whatever holds the port gets nothing");
        ctrl.ready.store(true, Ordering::Release);
        assert_eq!(ctrl.bridge_key().len(), 64);
        ctrl.ready.store(false, Ordering::Release);
        assert_eq!(ctrl.bridge_key(), "");
    }

    #[test]
    fn standalone_whitelist_action_and_takeover_flags() {
        assert_eq!(settings_store::sanitize_standalone_whitelist_action("ring_phone"), "ring_phone");
        assert_eq!(settings_store::sanitize_standalone_whitelist_action("forward_number"), "forward_number");
        assert_eq!(settings_store::sanitize_standalone_whitelist_action("ai_only"), "ai_only");
        assert_eq!(settings_store::sanitize_standalone_whitelist_action("bogus"), "urgent_prompt");

        let serial = "test-standalone-takeover";
        crate::urgent_call::clear_takeover(serial);
        assert!(!crate::urgent_call::is_takeover_active(serial));
        crate::urgent_call::mark_takeover(serial);
        assert!(crate::urgent_call::is_takeover_active(serial));
        assert!(crate::urgent_call::clear_takeover(serial));
        assert!(!crate::urgent_call::is_takeover_active(serial));

        let payload = crate::urgent_call::local_urgent_prompt_payload("u1", "0912345678", "Mom", "Urgent");
        assert_eq!(payload["type"], "owner_urgent_prompt");
        assert_eq!(payload["id"], "u1");
        assert_eq!(payload["caller_name"], "Mom");
    }
}
