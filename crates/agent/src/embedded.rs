// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Embedded standalone runtime support (Android all-in-one mode) and `incoming_sms`
//! control-channel handling.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use axum::Router;
use serde_json::{json, Value};
use tokio::sync::{oneshot, Notify};
use tracing::{error, info, warn};

use crate::calls::CallStore;
use crate::config::{EmbeddedOverrides, Settings};
use crate::settings_store::{self, SettingsStore};
use crate::{bootstrap_runtime, calendar, lan, locale, mdns, notify, recent_notices, sms_store, AppState};

/// Dedicated loopback bridge port used by Standalone Android (`ws://127.0.0.1:8766`)
/// so desktop `adb reverse tcp:8765` never collides if the phone is plugged into a PC.
pub const LOCAL_BRIDGE_PORT: u16 = 8766;

static LAN_NOTIFY_HOOK: std::sync::OnceLock<Arc<Notify>> = std::sync::OnceLock::new();

/// Wake the embedded listener loop immediately when HTTP/settings change `ui_password`,
/// `mdns_name`, or `phone_wifi`, avoiding background idle polling.
pub(crate) fn notify_lan_config_changed() {
    if let Some(n) = LAN_NOTIFY_HOOK.get() {
        n.notify_one();
    }
}

/// Runtime handles captured once the embedded agent has initialized its stores.
#[derive(Clone)]
pub struct EmbeddedHandles {
    pub settings: Settings,
    pub settings_store: Arc<SettingsStore>,
    pub calls: Arc<CallStore>,
}

/// Shared state between the Android JNI bridge and the embedded Tokio server.
pub struct EmbeddedControl {
    pub data_dir: PathBuf,
    pub assets_dir: PathBuf,
    pub port: u16,
    pub lan_enabled: AtomicBool,
    pub ready: AtomicBool,
    pub lan_notify: Arc<Notify>,
    pub handles: Mutex<Option<EmbeddedHandles>>,
}

impl EmbeddedControl {
    pub fn new(data_dir: PathBuf, assets_dir: PathBuf, port: u16, lan_enabled: bool) -> Arc<Self> {
        Arc::new(Self {
            data_dir,
            assets_dir,
            port,
            lan_enabled: AtomicBool::new(lan_enabled),
            ready: AtomicBool::new(false),
            lan_notify: Arc::new(Notify::new()),
            handles: Mutex::new(None),
        })
    }

    pub fn set_lan_enabled(&self, enabled: bool) -> bool {
        self.lan_enabled.store(enabled, Ordering::Release);
        if let Some(h) = self.handles.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let mdns_name = if enabled { "bageo" } else { "off" };
            let _ = h.settings_store.apply_patch(&json!({
                "phone_wifi": enabled,
                "mdns_name": mdns_name,
            }));
        }
        self.lan_notify.notify_one();
        true
    }

    pub fn set_ui_password(&self, password: &str) -> Result<()> {
        let trimmed = password.trim();
        if trimmed.is_empty() || settings_store::ui_password_too_short(trimmed) {
            anyhow::bail!(
                "console password must be at least {} characters",
                settings_store::MIN_UI_PASSWORD_CHARS
            );
        }
        let handles = self
            .handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| anyhow::anyhow!("agent not ready"))?;
        handles
            .settings_store
            .apply_patch(&json!({ "ui_password": trimmed }))?;
        self.lan_notify.notify_one();
        Ok(())
    }

    pub fn apply_settings_patch(&self, patch_json: &str) -> Result<()> {
        let mut patch: Value = serde_json::from_str(patch_json)?;
        let Some(obj) = patch.as_object_mut() else {
            anyhow::bail!("settings patch must be a JSON object");
        };
        obj.insert("gemini_live_enabled".to_string(), Value::Bool(true));
        let handles = self
            .handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| anyhow::anyhow!("agent not ready"))?;
        handles.settings_store.apply_patch(&patch)?;
        if patch.get("ui_password").is_some()
            || patch.get("mdns_name").is_some()
            || patch.get("phone_wifi").is_some()
        {
            self.lan_notify.notify_one();
        }
        Ok(())
    }

    pub fn status_json(&self) -> Value {
        let handles = self.handles.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let ready = self.ready.load(Ordering::Acquire);
        let lan_requested = self.lan_enabled.load(Ordering::Acquire);
        let (password_set, ui_token, active_calls, mdns_name) = match handles {
            Some(h) => {
                let tok = settings_store::active_ui_token(&h.settings.calls_ui_token);
                let rt = h.settings_store.snapshot();
                let active = h.calls.live_calls_count_try();
                (
                    !tok.trim().is_empty(),
                    tok,
                    active,
                    mdns::parse_name_setting(&rt.mdns_name),
                )
            }
            None => (false, String::new(), 0, String::new()),
        };
        let lan_active = ready && lan_requested && password_set;
        let lan_ip = if lan_active {
            mdns::lan_ipv4().map(|ip| ip.to_string()).unwrap_or_default()
        } else {
            String::new()
        };
        let lan_url = if lan_active && !lan_ip.is_empty() {
            format!("http://{}:{}/calls/", lan_ip, self.port)
        } else {
            String::new()
        };
        let mdns_url = if lan_active && !mdns_name.is_empty() {
            format!("http://{}.local:{}/calls/", mdns_name, self.port)
        } else {
            String::new()
        };
        json!({
            "running": true,
            "ready": ready,
            "lan_enabled": lan_active,
            "lan_requested": lan_requested,
            "password_set": password_set,
            "ui_token": ui_token,
            "port": self.port,
            "lan_ip": lan_ip,
            "lan_url": lan_url,
            "mdns_url": mdns_url,
            "active_calls": active_calls,
        })
    }
}

/// Bind a reusable TCP listener so fast loopback <-> all-interface swaps never
/// fail with `EADDRINUSE` on lingering client sockets.
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

/// Compute the target bind address for the embedded HTTP console listener.
pub fn target_console_bind(port: u16, lan_requested: bool, password_set: bool) -> SocketAddr {
    if lan_requested && password_set {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port)
    } else {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }
}

struct RunningListener {
    addr: SocketAddr,
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl RunningListener {
    async fn stop(self) {
        let _ = self.stop.send(());
        let _ = tokio::time::timeout(Duration::from_secs(3), self.task).await;
    }
}

async fn spawn_console_listener(addr: SocketAddr, app: Router) -> Result<RunningListener> {
    let listener = bind_tcp_reusable(addr)?;
    let (stop, stopped) = oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let served = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<lan::ConnAddrs>(),
        )
        .with_graceful_shutdown(async {
            let _ = stopped.await;
        })
        .await;
        if let Err(e) = served {
            warn!("embedded console listener on {addr} stopped ({e})");
        }
    });
    Ok(RunningListener { addr, stop, task })
}

/// Run the full Bageo agent inside the Android application process.
pub async fn run_embedded(cfg: Arc<EmbeddedControl>) -> Result<()> {
    crate::config::set_embedded_overrides(EmbeddedOverrides {
        data_dir: cfg.data_dir.clone(),
        assets_dir: cfg.assets_dir.clone(),
        port: cfg.port,
    });
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (state, calls_state, remote) = bootstrap_runtime().await?;
    let settings = state.settings.clone();
    let settings_store = state.settings_store.clone();
    let calls = state.calls.clone();

    // Sync initial LAN / mDNS flags with Android's saved preference, and ensure
    // `gemini_live_enabled` is always true on standalone (the single Android mode
    // switch controls AI answering vs normal phone).
    let initial_lan = cfg.lan_enabled.load(Ordering::Acquire);
    let _ = settings_store.apply_patch(&json!({
        "gemini_live_enabled": true,
        "phone_wifi": initial_lan,
        "mdns_name": if initial_lan { "bageo" } else { "off" },
    }));

    *cfg.handles.lock().unwrap_or_else(|e| e.into_inner()) = Some(EmbeddedHandles {
        settings: settings.clone(),
        settings_store: settings_store.clone(),
        calls: calls.clone(),
    });

    calendar::set_link_secret(&settings_store::active_ui_token(&settings.calls_ui_token));
    let _ = LAN_NOTIFY_HOOK.set(cfg.lan_notify.clone());

    let app = crate::build_app(state.clone(), calls_state, remote, true);

    // Permanent loopback phone-bridge listener (`ws://127.0.0.1:8766`): active
    // phone calls never drop even if the user toggles LAN sharing on port 8787.
    let bridge_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), LOCAL_BRIDGE_PORT);
    match bind_tcp_reusable(bridge_addr) {
        Ok(bridge_listener) => {
            let bridge_app = app.clone();
            tokio::spawn(async move {
                if let Err(e) = axum::serve(
                    bridge_listener,
                    bridge_app.into_make_service_with_connect_info::<lan::ConnAddrs>(),
                )
                .await
                {
                    error!("embedded phone bridge listener stopped: {e}");
                }
            });
            info!("embedded phone bridge listening on ws://{bridge_addr}");
        }
        Err(e) => {
            warn!("embedded phone bridge could not bind {bridge_addr} ({e}); falling back to console port");
        }
    }

    // Dynamic console + optional LAN listener on `cfg.port` (default 8787).
    // mDNS responder is spawned lazily only once LAN sharing + password are active
    // so idle standalone phones with LAN OFF create zero extra sockets/wakeups.
    let mut mdns_spawned = false;
    let mut current: Option<RunningListener> = None;
    let mut retry_delay = Duration::from_millis(200);
    loop {
        let lan_req = cfg.lan_enabled.load(Ordering::Acquire);
        let pw_set = !settings_store::active_ui_token(&settings.calls_ui_token)
            .trim()
            .is_empty();
        if lan_req && pw_set && !mdns_spawned {
            mdns_spawned = true;
            let cfg_mdns = cfg.clone();
            let store_mdns = settings_store.clone();
            let base_tok = settings.calls_ui_token.clone();
            mdns::spawn(
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                cfg.port,
                move || {
                    let on = cfg_mdns.lan_enabled.load(Ordering::Acquire)
                        && !settings_store::active_ui_token(&base_tok).trim().is_empty();
                    if on {
                        let name = mdns::parse_name_setting(&store_mdns.snapshot().mdns_name);
                        if name.is_empty() {
                            "bageo".to_string()
                        } else {
                            name
                        }
                    } else {
                        String::new()
                    }
                },
            );
        }
        let target = target_console_bind(cfg.port, lan_req, pw_set);
        let needs_swap = current.as_ref().is_none_or(|c| c.addr != target || c.task.is_finished());

        if needs_swap {
            if let Some(prev) = current.take() {
                prev.stop().await;
            }
            match spawn_console_listener(target, app.clone()).await {
                Ok(running) => {
                    lan::set_auto(target.ip().is_unspecified());
                    info!("embedded console listening on http://{target}/calls/");
                    current = Some(running);
                    cfg.ready.store(true, Ordering::Release);
                    retry_delay = Duration::from_millis(200);
                }
                Err(e) => {
                    warn!("embedded console bind {target} failed ({e}); retrying");
                    tokio::time::sleep(retry_delay).await;
                    retry_delay = (retry_delay * 2).min(Duration::from_secs(5));
                    continue;
                }
            }
        }

        cfg.lan_notify.notified().await;
    }
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
/// configured notification channels.
pub(crate) async fn handle_incoming_sms(state: &AppState, serial: &str, msg: &Value) -> Value {
    let id = msg
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
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
    fn target_console_bind_requires_both_lan_flag_and_password() {
        assert_eq!(
            target_console_bind(8787, false, false),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8787)
        );
        assert_eq!(
            target_console_bind(8787, true, false),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8787)
        );
        assert_eq!(
            target_console_bind(8787, false, true),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8787)
        );
        assert_eq!(
            target_console_bind(8787, true, true),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8787)
        );
    }

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

    #[test]
    fn embedded_control_status_requires_password_before_reporting_lan_active() {
        let dir = std::env::temp_dir().join(format!("bageo-emb-{}", uuid::Uuid::new_v4().simple()));
        let cfg = EmbeddedControl::new(dir.clone(), dir.join("assets"), 8787, true);
        cfg.ready.store(true, Ordering::Release);
        let j = cfg.status_json();
        assert_eq!(j["ready"], true);
        assert_eq!(j["lan_requested"], true);
        assert_eq!(j["password_set"], false);
        assert_eq!(j["lan_enabled"], false);
        let _ = std::fs::remove_dir_all(&dir);
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
