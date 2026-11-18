//! Background polling of Bageo every 5 seconds.

use std::sync::atomic::Ordering;
use std::time::Duration;

use reqwest::Client;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};

use crate::notify::{CallWatch, Notice, SmsWatch};
use crate::state::{lock, Shared};
use crate::status::{self, DevicesResult, Snapshot};
use crate::{config, toast, tray};

const INTERVAL: Duration = Duration::from_secs(5);
const TIMEOUT: Duration = Duration::from_secs(4);
/// The console address is re-read from /health once per this many rounds.
const UI_URL_EVERY: u64 = 12;
/// Failed rounds before automatic discovery starts over.
const REDISCOVER_AFTER: u32 = 3;
const CANDIDATES: [&str; 3] = ["http://127.0.0.1:8787/", "http://bageo.local/", "http://bageo.local:8787/"];
const TOKEN_HEADER: &str = "x-calls-token";

async fn get(client: &Client, url: &str, token: Option<&str>) -> Option<(u16, Option<Value>)> {
    let mut req = client.get(url);
    if let Some(t) = token {
        req = req.header(TOKEN_HEADER, t);
    }
    let resp = req.send().await.ok()?;
    let code = resp.status().as_u16();
    let body = if code == 200 { resp.json::<Value>().await.ok() } else { None };
    Some((code, body))
}

/// GET {base}health; Some only for HTTP 200 with `"ok": true`.
async fn health(client: &Client, base: &str, token: Option<&str>) -> Option<Value> {
    let (code, body) = get(client, &format!("{base}health"), token).await?;
    if code != 200 {
        return None;
    }
    body.filter(|b| b.get("ok").and_then(Value::as_bool) == Some(true))
}

async fn devices(client: &Client, base: &str, token: Option<&str>) -> DevicesResult {
    match get(client, &format!("{base}calls/api/devices"), token).await {
        Some((200, Some(body))) => DevicesResult::Detail(body),
        Some((401, _)) => DevicesResult::Locked,
        _ => DevicesResult::Unavailable,
    }
}

async fn discover(client: &Client) -> Option<(String, Value)> {
    for base in CANDIDATES {
        if let Some(h) = health(client, base, None).await {
            return Some((base.to_string(), h));
        }
    }
    None
}

struct Poller {
    client: Client,
    system_locale: String,
    /// Address found by discovery. Kept in memory only.
    found: Option<String>,
    failures: u32,
    last_good: Option<Snapshot>,
    round: u64,
    /// Console address Bageo reports about itself (may be empty).
    ui_url: String,
    refresh_ui_url: bool,
    lang: String,
    calls: CallWatch,
    sms: SmsWatch,
    calls_on: bool,
    sms_on: bool,
    /// The Bageo base address the watchers last looked at.
    watched_base: Option<String>,
}

impl Poller {
    fn new(system_locale: String) -> Option<Self> {
        let client = Client::builder().no_proxy().timeout(TIMEOUT).build().ok()?;
        let lang = status::locale_to_lang(&system_locale).to_string();
        Some(Poller {
            client,
            system_locale,
            found: None,
            failures: 0,
            last_good: None,
            round: 0,
            ui_url: String::new(),
            refresh_ui_url: true,
            lang,
            calls: CallWatch::default(),
            sms: SmsWatch::default(),
            calls_on: false,
            sms_on: false,
            watched_base: None,
        })
    }

    fn forget(&mut self) {
        self.found = None;
        self.failures = 0;
        self.last_good = None;
        self.ui_url.clear();
        self.refresh_ui_url = true;
        self.calls.reset();
        self.sms.reset();
        self.watched_base = None;
    }

    fn address(&self, base: Option<&str>) -> String {
        match base {
            None => String::new(),
            Some(_) if status::is_web_url(&self.ui_url) => self.ui_url.clone(),
            Some(b) => b.to_string(),
        }
    }

    /// Looks for new calls and text messages. Updates `snap.sms_supported`.
    async fn collect_notices(
        &mut self,
        cfg: &config::Config,
        base: &str,
        token: Option<&str>,
        health_json: &Value,
        snap: &mut Snapshot,
    ) -> Vec<Notice> {
        let mut notices = Vec::new();
        if self.watched_base.as_deref() != Some(base) {
            self.calls.reset();
            self.sms.reset();
            self.watched_base = Some(base.to_string());
        }

        if cfg.notify_calls {
            if !self.calls_on {
                self.calls.reset();
                self.calls_on = true;
            }
            match get(&self.client, &format!("{base}calls/api/live"), token).await {
                Some((200, Some(body))) => notices.extend(self.calls.on_live(&body, &snap.phones, &snap.address)),
                Some((401, _)) => {
                    let active = status::count(health_json.get("active_call_count"));
                    notices.extend(self.calls.on_active_count(active, &snap.address));
                }
                _ => {}
            }
        } else {
            self.calls_on = false;
        }

        if cfg.notify_sms {
            if !self.sms_on {
                self.sms.reset();
                self.sms_on = true;
            }
            if self.sms.due() {
                let url = match self.sms.after() {
                    Some(id) => format!("{base}calls/api/notify/recent?after={id}"),
                    None => format!("{base}calls/api/notify/recent"),
                };
                match get(&self.client, &url, token).await {
                    Some((200, Some(body))) => notices.extend(self.sms.on_response(&body, &snap.address)),
                    Some((404, _)) => self.sms.on_not_found(),
                    _ => self.sms.on_error(),
                }
            }
            snap.sms_supported = self.sms.supported;
        } else {
            self.sms_on = false;
            snap.sms_supported = None;
        }
        notices
    }

    async fn run_round(&mut self, app: &AppHandle) -> Snapshot {
        let shared = app.state::<Shared>();
        if shared.reset.swap(false, Ordering::SeqCst) {
            self.forget();
        }
        let cfg = lock(&shared.config).clone();
        let configured = status::normalize_address(&cfg.address);
        let password = lock(&shared.password).clone();
        let has_password = password.is_some();
        let round = self.round;
        self.round += 1;

        // Step 1: is Bageo there?
        let found: Option<(String, Value)> = match (&configured, &self.found) {
            (Some(base), _) => health(&self.client, base, None).await.map(|h| (base.clone(), h)),
            (None, Some(base)) => health(&self.client, base, None).await.map(|h| (base.clone(), h)),
            (None, None) => discover(&self.client).await,
        };

        let Some((base, health_json)) = found else {
            self.failures += 1;
            if configured.is_none() && self.failures >= REDISCOVER_AFTER {
                self.found = None;
            }
            let base_for_address = configured.clone().or_else(|| self.found.clone());
            let address = self.address(base_for_address.as_deref());
            return status::after_failure(self.last_good.as_ref(), self.failures, &self.lang, has_password, &address);
        };
        self.failures = 0;
        if configured.is_none() {
            self.found = Some(base.clone());
        }

        // Step 3 (before building the snapshot so the address is current).
        if self.refresh_ui_url || round % UI_URL_EVERY == 0 {
            if let Some(h) = health(&self.client, &base, password.as_deref()).await {
                self.ui_url = h.get("calls_ui_url").and_then(Value::as_str).unwrap_or("").trim().to_string();
                self.refresh_ui_url = false;
            }
        }

        // Step 2: phone details.
        let detail = devices(&self.client, &base, password.as_deref()).await;
        let address = self.address(Some(&base));
        let mut snap = status::build_snapshot(&health_json, &detail, has_password, &address, &self.system_locale);
        self.lang = snap.lang.clone();

        // Calls and text messages for desktop notifications.
        let notices = self.collect_notices(&cfg, &base, password.as_deref(), &health_json, &mut snap).await;
        self.last_good = Some(snap.clone());
        for notice in notices {
            toast::show(app, notice, &snap.lang);
        }
        snap
    }
}

/// Stores the snapshot and, when it differs from the current one, tells the window and the tray.
pub fn publish(app: &AppHandle, snap: Snapshot) {
    let shared = app.state::<Shared>();
    let (level_changed, lang_changed) = {
        let mut cur = lock(&shared.snapshot);
        if *cur == snap {
            return;
        }
        let flags = (cur.level != snap.level, cur.lang != snap.lang);
        *cur = snap.clone();
        flags
    };
    let _ = app.emit_to("main", "snapshot", &snap);
    tray::update(app, &snap, level_changed, lang_changed);
}

pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let stored = tauri::async_runtime::spawn_blocking(config::read_password).await.ok().flatten();
        *lock(&app.state::<Shared>().password) = stored;

        let locale = sys_locale::get_locale().unwrap_or_default();
        let Some(mut poller) = Poller::new(locale) else { return };
        loop {
            let snap = poller.run_round(&app).await;
            publish(&app, snap);
            let shared = app.state::<Shared>();
            tokio::select! {
                _ = tokio::time::sleep(INTERVAL) => {}
                _ = shared.notify.notified() => {}
            }
        }
    });
}
