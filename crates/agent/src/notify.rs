//! Notifications: incoming call, blocked call, voicemail and the post-call summary.
//!
//! Every enabled channel on the settings page gets every message ([`Channel`],
//! [`KINDS`]). Three older routes still work when set through the environment:
//! an Apprise gateway (`NOTIFY_GATEWAY_URL`), a plain JSON webhook
//! (`NOTIFY_WEBHOOK_URL`) and a Telegram bot (`TELEGRAM_BOT_TOKEN` +
//! `TELEGRAM_CHAT_ID`, also used by the task cards in [`crate::tg_inbound`]).
//!
//! Sending never fails a call: errors are logged and the next channel is tried.
//! The wording follows the agent's language ([`crate::locale`]).

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{bail, Result};
use base64::Engine;
use chrono::Local;
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use tracing::{info, warn};

use crate::config::Settings;
use crate::locale;

/// One destination for notifications. What `url` / `token` / `target` mean
/// depends on the kind ([`KINDS`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub kind: String,
    /// The owner's label for it ("Family group").
    #[serde(default)]
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub target: String,
}

fn yes() -> bool {
    true
}

/// A kind of channel and what its three fields hold (empty = not used).
pub struct Kind {
    pub kind: &'static str,
    pub label: &'static str,
    pub url: &'static str,
    pub token: &'static str,
    pub target: &'static str,
    /// Used when `url` is left empty.
    pub url_default: &'static str,
}

pub const KINDS: &[Kind] = &[
    Kind { kind: "telegram", label: "Telegram", url: "", token: "Bot token", target: "Chat ID", url_default: "" },
    Kind { kind: "discord", label: "Discord", url: "Webhook URL", token: "", target: "", url_default: "" },
    Kind { kind: "slack", label: "Slack / Mattermost", url: "Incoming webhook URL", token: "", target: "", url_default: "" },
    Kind { kind: "line", label: "LINE (Messaging API)", url: "", token: "Channel access token", target: "User / group ID", url_default: "" },
    Kind { kind: "ntfy", label: "ntfy", url: "Server", token: "Access token (optional)", target: "Topic", url_default: "https://ntfy.sh" },
    Kind { kind: "gotify", label: "Gotify", url: "Server URL", token: "App token", target: "", url_default: "" },
    Kind { kind: "bark", label: "Bark (iOS)", url: "Server", token: "Device key", target: "", url_default: "https://api.day.app" },
    Kind { kind: "pushover", label: "Pushover", url: "", token: "API token", target: "User key", url_default: "" },
    Kind { kind: "wecom", label: "WeCom group bot (企业微信)", url: "Webhook URL", token: "", target: "", url_default: "" },
    Kind { kind: "dingtalk", label: "DingTalk bot (钉钉)", url: "Webhook URL", token: "Signing secret (optional)", target: "", url_default: "" },
    Kind { kind: "feishu", label: "Feishu / Lark bot (飞书)", url: "Webhook URL", token: "", target: "", url_default: "" },
    Kind { kind: "serverchan", label: "ServerChan (Server酱)", url: "", token: "SendKey", target: "", url_default: "" },
    Kind { kind: "matrix", label: "Matrix", url: "Homeserver URL", token: "Access token", target: "Room ID", url_default: "" },
    Kind { kind: "gmail", label: "Email (Gmail)", url: "Gmail address", token: "App password", target: "Send to (empty = yourself)", url_default: "" },
    Kind { kind: "apprise", label: "Apprise API", url: "Notify URL (…/notify/KEY)", token: "", target: "Tag (optional)", url_default: "" },
    Kind { kind: "webhook", label: "Webhook (JSON)", url: "URL", token: "Bearer token (optional)", target: "", url_default: "" },
];

pub fn kind(name: &str) -> Option<&'static Kind> {
    KINDS.iter().find(|k| k.kind == name)
}

impl Channel {
    fn url_or_default(&self) -> String {
        let u = self.url.trim();
        if u.is_empty() {
            kind(&self.kind).map(|k| k.url_default).unwrap_or("").to_string()
        } else {
            u.trim_end_matches('/').to_string()
        }
    }

    /// What the settings page gets: the address and token only as hints.
    pub fn masked(&self) -> Value {
        json!({
            "id": self.id,
            "kind": self.kind,
            "name": self.name,
            "enabled": self.enabled,
            "url": "",
            "url_hint": mask_url(&self.url),
            "url_set": !self.url.trim().is_empty(),
            "token": "",
            "token_hint": mask_token(&self.token),
            "token_set": !self.token.trim().is_empty(),
            "target": self.target,
        })
    }
}

fn mask_token(s: &str) -> String {
    let chars: Vec<char> = s.trim().chars().collect();
    match chars.len() {
        0 => String::new(),
        1..=9 => "••••".to_string(),
        n => format!(
            "{}…{}",
            chars[..3].iter().collect::<String>(),
            chars[n - 4..].iter().collect::<String>()
        ),
    }
}

/// Scheme and host stay readable; the path (where webhook secrets live) does not.
fn mask_url(s: &str) -> String {
    let t = s.trim();
    if t.is_empty() {
        return String::new();
    }
    match url::Url::parse(t) {
        Ok(u) if u.host_str().is_some() => {
            let rest = &t[t.find(u.host_str().unwrap_or("")).unwrap_or(0)..];
            let has_path = rest.contains('/') && !rest.ends_with('/');
            format!(
                "{}://{}{}",
                u.scheme(),
                u.host_str().unwrap_or(""),
                if has_path { "/…" } else { "" }
            )
        }
        // An e-mail address or something else that is not a URL.
        _ => mask_token(t),
    }
}

/// The list from the settings page onto the stored one: a row that comes back
/// with an empty address or token keeps what was stored under the same id; rows
/// without an id get one; unknown kinds are dropped.
pub fn merge_channels(stored: &[Channel], incoming: Vec<Channel>) -> Vec<Channel> {
    let mut out: Vec<Channel> = Vec::new();
    for mut c in incoming.into_iter().take(30) {
        c.kind = c.kind.trim().to_ascii_lowercase();
        if kind(&c.kind).is_none() {
            continue;
        }
        c.id = c.id.trim().to_string();
        let old = stored.iter().find(|s| !c.id.is_empty() && s.id == c.id && s.kind == c.kind);
        c.url = c.url.trim().to_string();
        c.token = c.token.trim().to_string();
        if let Some(old) = old {
            if c.url.is_empty() {
                c.url = old.url.clone();
            }
            if c.token.is_empty() {
                c.token = old.token.clone();
            }
        }
        if c.id.is_empty() || out.iter().any(|o| o.id == c.id) {
            c.id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
        }
        c.name = c.name.trim().chars().take(60).collect();
        c.target = c.target.trim().to_string();
        out.push(c);
    }
    out
}

/// What is sent.
#[derive(Debug, Clone, Default)]
pub struct Message {
    /// `incoming` / `blocked` / `voicemail` / `summary` / `test` / `sms`.
    pub event: String,
    pub title: String,
    /// Plain text; its first line is the headline.
    pub body: String,
    /// The same with Telegram's HTML tags.
    pub html: Option<String>,
    /// For webhooks: the facts as JSON.
    pub data: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Json(Value),
    Form(Vec<(String, String)>),
}

/// An HTTP request for one channel (everything except Gmail, which is SMTP).
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub put: bool,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

fn clip(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
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

/// Why a channel could not be used: a stable `code` the settings page
/// translates, the channel field at fault (`url` / `token` / `target`, empty
/// when none) and the details for the log and the page's tooltip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelError {
    pub code: &'static str,
    pub field: &'static str,
    pub detail: String,
}

impl ChannelError {
    pub fn new(code: &'static str, field: &'static str, detail: impl Into<String>) -> Self {
        Self { code, field, detail: detail.into() }
    }
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for ChannelError {}

/// The [`ChannelError`] behind an error, if there is one.
pub fn channel_error(e: &anyhow::Error) -> Option<&ChannelError> {
    e.chain().find_map(|c| c.downcast_ref::<ChannelError>())
}

fn need<'a>(field: &'static str, what: &str, value: &'a str) -> Result<&'a str> {
    let v = value.trim();
    if v.is_empty() {
        return Err(ChannelError::new("missing_field", field, format!("{what} is empty")).into());
    }
    Ok(v)
}

fn need_http(field: &'static str, what: &str, value: &str) -> Result<String> {
    let v = need(field, what, value)?;
    if !(v.starts_with("https://") || v.starts_with("http://")) {
        return Err(ChannelError::new(
            "bad_url",
            field,
            format!("{what} must start with http:// or https://"),
        )
        .into());
    }
    Ok(v.to_string())
}

fn post_json(url: String, body: Value) -> Prepared {
    Prepared { put: false, url, headers: Vec::new(), body: Body::Json(body) }
}

/// The request a channel needs for `msg`. `now_ms` only matters to DingTalk's
/// signature and Matrix's transaction id.
pub fn prepare(ch: &Channel, msg: &Message, now_ms: u64) -> Result<Prepared> {
    let k = kind(&ch.kind).ok_or_else(|| {
        ChannelError::new("unknown_channel", "", format!("unknown channel kind {:?}", ch.kind))
    })?;
    let base = ch.url_or_default();
    let title = msg.title.as_str();
    let body = msg.body.as_str();
    Ok(match k.kind {
        "telegram" => {
            let token = need("token", "bot token", &ch.token)?;
            let chat = need("target", "chat ID", &ch.target)?;
            let mut payload = json!({
                "chat_id": chat,
                "text": clip(msg.html.as_deref().unwrap_or(body), 4000),
                "disable_web_page_preview": true,
            });
            if msg.html.is_some() {
                payload["parse_mode"] = json!("HTML");
            }
            post_json(format!("https://api.telegram.org/bot{token}/sendMessage"), payload)
        }
        "discord" => post_json(
            need_http("url", "webhook URL", &base)?,
            json!({ "content": clip(body, 1900) }),
        ),
        "slack" => post_json(need_http("url", "webhook URL", &base)?, json!({ "text": clip(body, 3500) })),
        "line" => Prepared {
            put: false,
            url: "https://api.line.me/v2/bot/message/push".to_string(),
            headers: vec![(
                "Authorization".into(),
                format!("Bearer {}", need("token", "channel access token", &ch.token)?),
            )],
            body: Body::Json(json!({
                "to": need("target", "user / group ID", &ch.target)?,
                "messages": [{ "type": "text", "text": clip(body, 4500) }],
            })),
        },
        "ntfy" => {
            let mut headers = Vec::new();
            if !ch.token.trim().is_empty() {
                headers.push(("Authorization".to_string(), format!("Bearer {}", ch.token.trim())));
            }
            Prepared {
                put: false,
                url: need_http("url", "server", &base)?,
                headers,
                body: Body::Json(json!({
                    "topic": need("target", "topic", &ch.target)?,
                    "title": clip(title, 200),
                    "message": clip(body, 3500),
                })),
            }
        }
        "gotify" => post_json(
            format!(
                "{}/message?token={}",
                need_http("url", "server URL", &base)?,
                pct(need("token", "app token", &ch.token)?)
            ),
            json!({ "title": clip(title, 200), "message": clip(body, 3500), "priority": 5 }),
        ),
        "bark" => post_json(
            format!("{}/push", need_http("url", "server", &base)?),
            json!({
                "device_key": need("token", "device key", &ch.token)?,
                "title": clip(title, 200),
                "body": clip(body, 3500),
            }),
        ),
        "pushover" => Prepared {
            put: false,
            url: "https://api.pushover.net/1/messages.json".to_string(),
            headers: Vec::new(),
            body: Body::Form(vec![
                ("token".into(), need("token", "API token", &ch.token)?.to_string()),
                ("user".into(), need("target", "user key", &ch.target)?.to_string()),
                ("title".into(), clip(title, 200)),
                ("message".into(), clip(body, 1000)),
            ]),
        },
        "wecom" => post_json(
            need_http("url", "webhook URL", &base)?,
            json!({ "msgtype": "text", "text": { "content": clip(body, 1900) } }),
        ),
        "dingtalk" => {
            let mut url = need_http("url", "webhook URL", &base)?;
            let secret = ch.token.trim();
            if !secret.is_empty() {
                let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
                    .map_err(|e| ChannelError::new("bad_value", "token", format!("signing secret: {e}")))?;
                mac.update(format!("{now_ms}\n{secret}").as_bytes());
                let sign = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
                let sep = if url.contains('?') { '&' } else { '?' };
                url = format!("{url}{sep}timestamp={now_ms}&sign={}", pct(&sign));
            }
            post_json(url, json!({ "msgtype": "text", "text": { "content": clip(body, 3500) } }))
        }
        "feishu" => post_json(
            need_http("url", "webhook URL", &base)?,
            json!({ "msg_type": "text", "content": { "text": clip(body, 3500) } }),
        ),
        "serverchan" => {
            let key = need("token", "SendKey", &ch.token)?;
            // SC3 keys (`sctp<uid>t…`) have their own host; Turbo keys the shared one.
            let uid: String = key
                .strip_prefix("sctp")
                .map(|rest| rest.chars().take_while(|c| c.is_ascii_digit()).collect())
                .unwrap_or_default();
            let url = if uid.is_empty() {
                format!("https://sctapi.ftqq.com/{}.send", pct(key))
            } else {
                format!("https://{uid}.push.ft07.com/send/{}.send", pct(key))
            };
            Prepared {
                put: false,
                url,
                headers: Vec::new(),
                body: Body::Form(vec![
                    ("title".into(), clip(title, 30)),
                    ("desp".into(), clip(body, 3500)),
                ]),
            }
        }
        "matrix" => Prepared {
            put: true,
            url: format!(
                "{}/_matrix/client/v3/rooms/{}/send/m.room.message/pca{now_ms}",
                need_http("url", "homeserver URL", &base)?,
                pct(need("target", "room ID", &ch.target)?)
            ),
            headers: vec![(
                "Authorization".into(),
                format!("Bearer {}", need("token", "access token", &ch.token)?),
            )],
            body: Body::Json(json!({ "msgtype": "m.text", "body": clip(body, 3500) })),
        },
        "apprise" => {
            let mut form = vec![
                ("title".to_string(), clip(title, 200)),
                ("body".to_string(), clip(body, 3500)),
            ];
            if !ch.target.trim().is_empty() {
                form.push(("tag".to_string(), ch.target.trim().to_string()));
            }
            Prepared {
                put: false,
                url: need_http("url", "notify URL", &base)?,
                headers: Vec::new(),
                body: Body::Form(form),
            }
        }
        "webhook" => {
            let mut headers = Vec::new();
            if !ch.token.trim().is_empty() {
                headers.push(("Authorization".to_string(), format!("Bearer {}", ch.token.trim())));
            }
            Prepared {
                put: false,
                url: need_http("url", "URL", &base)?,
                headers,
                body: Body::Json(json!({
                    "event": msg.event,
                    "title": title,
                    "body": body,
                    "text": body,
                    "data": msg.data,
                })),
            }
        }
        "gmail" => bail!("gmail is sent over SMTP"),
        other => {
            return Err(ChannelError::new("unknown_channel", "", format!("unknown channel kind {other:?}")).into())
        }
    })
}

async fn send_prepared(http: &Client, p: &Prepared) -> Result<()> {
    let mut req = if p.put { http.put(&p.url) } else { http.post(&p.url) };
    for (k, v) in &p.headers {
        req = req.header(k, v);
    }
    req = match &p.body {
        Body::Json(v) => req.json(v),
        Body::Form(f) => req.form(f),
    };
    let resp = req
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| ChannelError::new("network_error", "", format!("request failed: {}", e.without_url())))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let code = match status.as_u16() {
            401 | 403 => "auth_failed",
            404 => "service_not_found",
            _ => "http_error",
        };
        let detail = format!("HTTP {}: {}", status.as_u16(), clip(text.trim(), 200));
        return Err(ChannelError::new(code, "", detail).into());
    }
    // Several services answer 200 with an error inside the body.
    if let Ok(v) = serde_json::from_str::<Value>(&text) {
        let code = v
            .get("errcode")
            .or_else(|| v.get("code"))
            .and_then(|c| c.as_i64());
        if code.is_some_and(|c| c != 0 && c != 200) || v.get("ok").and_then(|o| o.as_bool()) == Some(false) {
            let detail = format!("service answered: {}", clip(text.trim(), 200));
            return Err(ChannelError::new("service_error", "", detail).into());
        }
    }
    Ok(())
}

/// Send `msg` to one channel.
pub async fn send_channel(http: &Client, ch: &Channel, msg: &Message) -> Result<()> {
    if ch.kind == "gmail" {
        let user = need("url", "Gmail address", &ch.url)?;
        let to = if ch.target.trim().is_empty() { user } else { ch.target.trim() };
        return crate::gmail::send_mail(
            http,
            user,
            need("token", "app password", &ch.token)?,
            "Bageo",
            to,
            &clip(&msg.title, 150),
            &msg.body,
        )
        .await;
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let prepared = prepare(ch, msg, now_ms)?;
    match send_prepared(http, &prepared).await {
        // Telegram refuses HTML it cannot parse: send the plain text instead.
        Err(e) if ch.kind == "telegram" && msg.html.is_some() => {
            warn!("notify telegram HTML refused ({e:#}); retrying as plain text");
            let plain = Message { html: None, ..msg.clone() };
            send_prepared(http, &prepare(ch, &plain, now_ms)?).await
        }
        other => other,
    }
}

/// Phones the agent has seen: logical id (`pixel-<uid>`) → model (`Pixel 8`).
static DEVICES: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

/// The owner's own names for phones (settings `device_labels`).
static DEVICE_OVERRIDES: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

pub fn set_device_labels(labels: &std::collections::BTreeMap<String, String>) {
    *DEVICE_OVERRIDES.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(labels.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
}

fn device_override(serial: &str) -> Option<String> {
    DEVICE_OVERRIDES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(serial).cloned())
}

/// Called when a phone says hello with its model.
pub fn remember_device(serial: &str, model: &str) {
    let (serial, model) = (serial.trim(), model.trim());
    if serial.is_empty() || model.is_empty() {
        return;
    }
    let label: String = model.chars().filter(|c| !c.is_control()).take(40).collect();
    DEVICES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(serial.to_string(), label);
}

pub fn known_devices() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = DEVICES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    // The owner's names win, and cover phones that have not said hello yet.
    if let Some(over) = DEVICE_OVERRIDES.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        for (k, label) in over {
            v.retain(|(id, _)| id != k);
            v.push((k.clone(), label.clone()));
        }
    }
    v.sort();
    v
}

/// The phone as the owner knows it: its model when it told us, else its id.
pub fn device_label(serial: &str) -> String {
    let s = serial.trim();
    if let Some(label) = device_override(s) {
        return label;
    }
    if let Some(label) = DEVICES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(s).cloned())
    {
        return label;
    }
    if s.is_empty() {
        locale::phrases(locale::current()).n_unknown.to_string()
    } else {
        s.to_string()
    }
}

/// Link that opens one call in the web console; empty when the console has no
/// address other devices can open (see `settings_store::effective_console_url`).
pub fn call_detail_url(settings: &Settings, call_id: &str) -> String {
    let console = crate::settings_store::effective_console_url(settings);
    let base = console.trim().trim_end_matches('/');
    if base.is_empty() {
        return String::new();
    }
    let base = base.strip_suffix("/calls").unwrap_or(base);
    format!("{base}/calls/#call={call_id}")
}

/// "Recording: <link> (note)" for voicemail messages; empty without a link.
pub fn listen_recording_line(settings: &Settings, call_id: &str, note: &str) -> String {
    let url = call_detail_url(settings, call_id);
    if url.is_empty() {
        return String::new();
    }
    let label = locale::phrases(locale::current()).n_recording;
    let sep = kv_sep();
    match note.trim() {
        "" => format!("{label}{sep}{url}"),
        note => format!("{label}{sep}{url} ({note})"),
    }
}

/// "Transcript / recording: <link>" under a call summary; empty without a link.
pub fn call_detail_line(settings: &Settings, call_id: &str) -> String {
    let url = call_detail_url(settings, call_id);
    if url.is_empty() {
        return String::new();
    }
    format!("{}{}{url}", locale::phrases(locale::current()).n_details, kv_sep())
}

/// Between a label and its value, as the agent's language writes it.
fn kv_sep() -> &'static str {
    locale::kv_sep()
}

fn display_or_hidden(s: &str) -> String {
    let t = s.trim();
    if t.is_empty() {
        locale::phrases(locale::current()).n_hidden.to_string()
    } else {
        t.to_string()
    }
}

/// The machine's local time as the agent's language writes it, e.g.
/// "Nov 13, 08:09" or "11月13日 08:09" ([`locale::date_time`]).
fn now_stamp() -> String {
    locale::date_time(locale::current(), &Local::now().naive_local())
}

fn esc_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Headline + "label: value" lines, as plain text and as Telegram HTML.
/// A `code` row shows monospaced in Telegram (tap to copy a number).
struct Card {
    icon: &'static str,
    headline: String,
    rows: Vec<(String, String, bool)>,
    tail: String,
}

impl Card {
    fn new(icon: &'static str, headline: &str) -> Self {
        Self { icon, headline: headline.to_string(), rows: Vec::new(), tail: String::new() }
    }

    fn row(mut self, label: &str, value: &str) -> Self {
        self.rows.push((label.to_string(), value.to_string(), false));
        self
    }

    fn code(mut self, label: &str, value: &str) -> Self {
        self.rows.push((label.to_string(), value.to_string(), true));
        self
    }

    fn tail(mut self, text: &str) -> Self {
        self.tail = text.trim().to_string();
        self
    }

    fn plain(&self) -> String {
        let sep = kv_sep();
        let mut s = format!("{} {}", self.icon, self.headline);
        for (l, v, _) in &self.rows {
            s.push_str(&format!("\n{l}{sep}{v}"));
        }
        if !self.tail.is_empty() {
            s.push_str(&format!("\n\n{}", self.tail));
        }
        s
    }

    fn html(&self) -> String {
        let sep = kv_sep();
        let mut s = format!("<b>{} {}</b>", self.icon, esc_html(&self.headline));
        for (l, v, code) in &self.rows {
            if *code {
                s.push_str(&format!("\n{}{sep}<code>{}</code>", esc_html(l), esc_html(v)));
            } else {
                s.push_str(&format!("\n{}{sep}{}", esc_html(l), esc_html(v)));
            }
        }
        if !self.tail.is_empty() {
            s.push_str(&format!("\n\n{}", esc_html(&self.tail)));
        }
        s
    }

    fn message(&self, event: &str, title: String, data: Value) -> Message {
        Message {
            event: event.to_string(),
            title,
            body: self.plain(),
            html: Some(self.html()),
            data,
        }
    }
}

/// A call came in. Returns true when at least one channel took the message.
pub async fn notify_incoming_call(
    http: &Client,
    settings: &Settings,
    caller_id: &str,
    serial: &str,
) -> bool {
    if !settings.call_notify {
        return false;
    }
    let p = locale::phrases(locale::current());
    let caller = display_or_hidden(caller_id);
    let device = device_label(serial);
    let when = now_stamp();
    let msg = Card::new("📞", p.n_incoming)
        .code(p.n_number, &caller)
        .row(p.n_phone, &device)
        .row(p.n_time, &when)
        .message(
            "incoming",
            format!("{} {caller}", p.n_incoming),
            json!({ "caller_id": caller_id.trim(), "device": device, "time": when }),
        );
    deliver(http, settings, &msg).await
}

/// A number on the blocked list called: it got the announcement, no AI.
/// `reason` = why the owner listed the number.
pub async fn notify_rejected_call(
    http: &Client,
    settings: &Settings,
    caller_id: &str,
    serial: &str,
    reason: &str,
) -> bool {
    if !settings.call_notify {
        return false;
    }
    let p = locale::phrases(locale::current());
    let caller = display_or_hidden(caller_id);
    let device = device_label(serial);
    let when = now_stamp();
    let reason = reason.trim();
    let mut card = Card::new("⛔", p.n_blocked).code(p.n_number, &caller);
    if !reason.is_empty() {
        card = card.row(p.n_reason, reason);
    }
    let msg = card
        .row(p.n_phone, &device)
        .row(p.n_time, &when)
        .tail(&p.n_blocked_done.replace("{}", p.unavailable))
        .message(
            "blocked",
            format!("{} {caller}", p.n_blocked),
            json!({ "caller_id": caller_id.trim(), "device": device, "time": when, "reason": reason }),
        );
    deliver(http, settings, &msg).await
}

/// The post-call summary (`summary_body` = the summary lines and the link).
pub async fn notify_ai_request(
    http: &Client,
    settings: &Settings,
    caller_id: &str,
    serial: &str,
    summary_body: &str,
) -> bool {
    if !settings.call_notify {
        return false;
    }
    let p = locale::phrases(locale::current());
    let caller = display_or_hidden(caller_id);
    let device = device_label(serial);
    let when = now_stamp();
    let msg = Card::new("☎️", p.n_summary)
        .code(p.n_number, &caller)
        .row(p.n_phone, &device)
        .row(p.n_time, &when)
        .tail(summary_body)
        .message(
            "summary",
            format!("{} {caller}", p.n_summary),
            json!({
                "caller_id": caller_id.trim(),
                "device": device,
                "time": when,
                "summary": summary_body.trim(),
            }),
        );
    deliver(http, settings, &msg).await
}

/// A voicemail was saved (older menu flow): caller, length, how to reply.
pub async fn notify_voicemail(
    http: &Client,
    settings: &Settings,
    caller_id: &str,
    serial: &str,
    secs: f64,
    reply_line: &str,
) -> bool {
    if !settings.call_notify {
        return false;
    }
    let p = locale::phrases(locale::current());
    let caller = display_or_hidden(caller_id);
    let device = device_label(serial);
    let when = now_stamp();
    let msg = Card::new("🎙️", p.n_voicemail)
        .code(p.n_number, &caller)
        .row(p.n_phone, &device)
        .row(p.n_length, &format!("{secs:.0} {}", p.n_seconds))
        .row(p.n_time, &when)
        .tail(reply_line)
        .message(
            "voicemail",
            format!("{} {caller}", p.n_voicemail),
            json!({ "caller_id": caller_id.trim(), "device": device, "time": when, "seconds": secs }),
        );
    deliver(http, settings, &msg).await
}

/// A plain title + body message to every channel.
pub async fn notify_event(http: &Client, settings: &Settings, title: &str, body: &str) -> bool {
    let msg = Message {
        event: "event".to_string(),
        title: title.to_string(),
        body: body.to_string(),
        html: None,
        data: Value::Null,
    };
    deliver(http, settings, &msg).await
}

/// The test message of the settings page.
pub fn test_message() -> Message {
    let p = locale::phrases(locale::current());
    Card::new("✅", p.n_test_title)
        .row(p.n_time, &now_stamp())
        .tail(p.n_test_body)
        .message("test", p.n_test_title.to_string(), json!({}))
}

/// The environment's Telegram bot as a channel.
fn env_telegram(settings: &Settings) -> Option<Channel> {
    let token = settings.telegram_bot_token.trim();
    let chat = settings.telegram_chat_id.trim();
    if token.is_empty() || chat.is_empty() {
        return None;
    }
    Some(Channel {
        id: "env-telegram".to_string(),
        kind: "telegram".to_string(),
        name: "Telegram (env)".to_string(),
        enabled: true,
        url: String::new(),
        token: token.to_string(),
        target: chat.to_string(),
    })
}

/// Everything a message goes to: the settings page's enabled channels, then the
/// environment's gateway / webhook / Telegram bot.
fn destinations(settings: &Settings) -> Vec<Channel> {
    let mut out: Vec<Channel> = settings
        .notify_channels
        .iter()
        .filter(|c| c.enabled)
        .cloned()
        .collect();
    let gateway = settings.notify_gateway_url.trim();
    if !gateway.is_empty() {
        let tag = match settings.notify_gateway_tag.trim().trim_matches('/') {
            "" => "normal",
            t => t,
        };
        out.push(Channel {
            id: "env-gateway".to_string(),
            kind: "apprise".to_string(),
            name: "Apprise gateway (env)".to_string(),
            enabled: true,
            url: format!("{}/notify/{tag}", gateway.trim_end_matches('/')),
            ..Channel::default()
        });
    }
    let webhook = settings.notify_webhook_url.trim();
    if !webhook.is_empty() {
        out.push(Channel {
            id: "env-webhook".to_string(),
            kind: "webhook".to_string(),
            name: "Webhook (env)".to_string(),
            enabled: true,
            url: webhook.to_string(),
            ..Channel::default()
        });
    }
    if let Some(tg) = env_telegram(settings) {
        let dup = out
            .iter()
            .any(|c| c.kind == "telegram" && c.token.trim() == tg.token && c.target.trim() == tg.target);
        if !dup {
            out.push(tg);
        }
    }
    out
}

/// Send to every destination at once; true when at least one took it.
async fn deliver(http: &Client, settings: &Settings, msg: &Message) -> bool {
    // Drop control characters; Chinese goes out in the owner's script.
    let msg = Message {
        title: crate::zh_tw::to_tw(&sanitize_notify_text(&msg.title, 200)),
        body: crate::zh_tw::to_tw(&sanitize_notify_text(&msg.body, 3500)),
        html: msg
            .html
            .as_deref()
            .map(|h| crate::zh_tw::to_tw(&sanitize_notify_text(h, 3500))),
        ..msg.clone()
    };
    let channels = destinations(settings);
    if channels.is_empty() {
        return false;
    }
    let sends = channels.iter().map(|ch| {
        let msg = &msg;
        async move { (ch, send_channel(http, ch, msg).await) }
    });
    let mut any = false;
    for (ch, result) in futures_util::future::join_all(sends).await {
        match result {
            Ok(()) => {
                any = true;
                info!("notify ok kind={} name={:?} event={}", ch.kind, ch.name, msg.event);
            }
            Err(e) => warn!(
                "notify failed kind={} name={:?} event={}: {e:#}",
                ch.kind, ch.name, msg.event
            ),
        }
    }
    any
}

/// Drop NULs / C0 controls (keep `\n` `\r` `\t`); truncate to `max_chars`.
fn sanitize_notify_text(s: &str, max_chars: usize) -> String {
    let cleaned: String = s
        .chars()
        .filter(|c| {
            let u = *c as u32;
            *c == '\n' || *c == '\r' || *c == '\t' || u >= 0x20
        })
        .collect();
    clip(&cleaned, max_chars)
}

pub fn notify_configured(settings: &Settings) -> bool {
    settings.call_notify && !destinations(settings).is_empty()
}

/// Kind of the first destination (logs / health only; never secrets).
pub fn notify_primary_backend(settings: &Settings) -> Option<&'static str> {
    if !settings.call_notify {
        return None;
    }
    destinations(settings)
        .first()
        .and_then(|c| kind(&c.kind))
        .map(|k| k.kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(kind: &str, url: &str, token: &str, target: &str) -> Channel {
        Channel {
            id: "t".into(),
            kind: kind.into(),
            name: String::new(),
            enabled: true,
            url: url.into(),
            token: token.into(),
            target: target.into(),
        }
    }

    fn msg() -> Message {
        Message {
            event: "summary".into(),
            title: "Call summary 0912345678".into(),
            body: "☎️ Call summary\nNumber: 0912345678".into(),
            html: Some("<b>☎️ Call summary</b>\nNumber: <code>0912345678</code>".into()),
            data: json!({ "caller_id": "0912345678" }),
        }
    }

    fn json_body(p: &Prepared) -> &Value {
        match &p.body {
            Body::Json(v) => v,
            Body::Form(_) => panic!("form body"),
        }
    }

    fn form_body(p: &Prepared) -> HashMap<String, String> {
        match &p.body {
            Body::Form(f) => f.iter().cloned().collect(),
            Body::Json(_) => panic!("json body"),
        }
    }

    #[test]
    fn every_kind_prepares_a_request_or_says_what_is_missing() {
        for k in KINDS {
            if k.kind == "gmail" {
                continue;
            }
            let empty = prepare(&ch(k.kind, "", "", ""), &msg(), 1);
            let err = empty.err().unwrap_or_else(|| panic!("{} accepted an empty channel", k.kind));
            // The settings page names the empty field.
            let ce = channel_error(&err).unwrap_or_else(|| panic!("{}: {err:#}", k.kind));
            assert_eq!(ce.code, "missing_field", "{}", k.kind);
            assert!(["url", "token", "target"].contains(&ce.field), "{}: {}", k.kind, ce.field);
            let full = prepare(
                &ch(k.kind, "https://example.org/hook/abc", "tok_1234567890", "target1"),
                &msg(),
                1,
            )
            .unwrap_or_else(|e| panic!("{}: {e:#}", k.kind));
            assert!(full.url.starts_with("https://"), "{}: {}", k.kind, full.url);
        }
        let bad = prepare(&ch("discord", "discord.com/hook", "", ""), &msg(), 0).unwrap_err();
        assert_eq!(channel_error(&bad).map(|c| (c.code, c.field)), Some(("bad_url", "url")));
        let unknown = prepare(&ch("carrier-pigeon", "", "", ""), &msg(), 0).unwrap_err();
        assert_eq!(channel_error(&unknown).map(|c| c.code), Some("unknown_channel"));
    }

    #[test]
    fn telegram_sends_html_and_plain() {
        let c = ch("telegram", "", "123:ABC", "-100200");
        let p = prepare(&c, &msg(), 0).unwrap();
        assert_eq!(p.url, "https://api.telegram.org/bot123:ABC/sendMessage");
        assert_eq!(json_body(&p)["chat_id"], "-100200");
        assert_eq!(json_body(&p)["parse_mode"], "HTML");
        assert!(json_body(&p)["text"].as_str().unwrap().contains("<code>"));
        let plain = Message { html: None, ..msg() };
        let p = prepare(&c, &plain, 0).unwrap();
        assert!(json_body(&p).get("parse_mode").is_none());
        assert!(json_body(&p)["text"].as_str().unwrap().starts_with("☎️ Call summary"));
    }

    #[test]
    fn chat_webhooks_use_each_service_shape() {
        let m = msg();
        let p = prepare(&ch("discord", "https://discord.com/api/webhooks/1/x", "", ""), &m, 0).unwrap();
        assert_eq!(json_body(&p)["content"], m.body);
        let p = prepare(&ch("slack", "https://hooks.slack.com/services/T/B/x", "", ""), &m, 0).unwrap();
        assert_eq!(json_body(&p)["text"], m.body);
        let p = prepare(&ch("wecom", "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=k", "", ""), &m, 0).unwrap();
        assert_eq!(json_body(&p)["msgtype"], "text");
        assert_eq!(json_body(&p)["text"]["content"], m.body);
        let p = prepare(&ch("feishu", "https://open.feishu.cn/open-apis/bot/v2/hook/x", "", ""), &m, 0).unwrap();
        assert_eq!(json_body(&p)["msg_type"], "text");
        assert_eq!(json_body(&p)["content"]["text"], m.body);
        let p = prepare(&ch("line", "", "linetoken", "U123"), &m, 0).unwrap();
        assert_eq!(p.url, "https://api.line.me/v2/bot/message/push");
        assert_eq!(p.headers[0], ("Authorization".to_string(), "Bearer linetoken".to_string()));
        assert_eq!(json_body(&p)["to"], "U123");
        assert_eq!(json_body(&p)["messages"][0]["text"], m.body);
        assert!(prepare(&ch("discord", "discord.com/hook", "", ""), &m, 0).is_err());
    }

    #[test]
    fn push_services_get_title_and_body() {
        let m = msg();
        let p = prepare(&ch("ntfy", "", "", "my-calls"), &m, 0).unwrap();
        assert_eq!(p.url, "https://ntfy.sh");
        assert!(p.headers.is_empty());
        assert_eq!(json_body(&p)["topic"], "my-calls");
        assert_eq!(json_body(&p)["title"], m.title);
        let p = prepare(&ch("ntfy", "https://ntfy.example.org/", "tk_abc", "t"), &m, 0).unwrap();
        assert_eq!(p.url, "https://ntfy.example.org");
        assert_eq!(p.headers[0].1, "Bearer tk_abc");
        let p = prepare(&ch("gotify", "https://push.example.org", "A b/c", ""), &m, 0).unwrap();
        assert_eq!(p.url, "https://push.example.org/message?token=A%20b%2Fc");
        let p = prepare(&ch("bark", "", "devkey", ""), &m, 0).unwrap();
        assert_eq!(p.url, "https://api.day.app/push");
        assert_eq!(json_body(&p)["device_key"], "devkey");
        let p = prepare(&ch("pushover", "", "apptoken", "userkey"), &m, 0).unwrap();
        let f = form_body(&p);
        assert_eq!(f["token"], "apptoken");
        assert_eq!(f["user"], "userkey");
        assert_eq!(f["message"], m.body);
        let p = prepare(&ch("serverchan", "", "SCT123abc", ""), &m, 0).unwrap();
        assert_eq!(p.url, "https://sctapi.ftqq.com/SCT123abc.send");
        let p = prepare(&ch("serverchan", "", "sctp4567tABCDEF", ""), &m, 0).unwrap();
        assert_eq!(p.url, "https://4567.push.ft07.com/send/sctp4567tABCDEF.send");
    }

    #[test]
    fn dingtalk_signs_when_a_secret_is_set() {
        let m = msg();
        let url = "https://oapi.dingtalk.com/robot/send?access_token=abc";
        let p = prepare(&ch("dingtalk", url, "", ""), &m, 1_700_000_000_000).unwrap();
        assert_eq!(p.url, url);
        let p = prepare(&ch("dingtalk", url, "SECabc", ""), &m, 1_700_000_000_000).unwrap();
        assert!(p.url.starts_with(&format!("{url}&timestamp=1700000000000&sign=")), "{}", p.url);
        // HMAC-SHA256("1700000000000\nSECabc", key "SECabc"), base64, URL-encoded.
        let mut mac = Hmac::<Sha256>::new_from_slice(b"SECabc").unwrap();
        mac.update(b"1700000000000\nSECabc");
        let sign = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
        assert!(p.url.ends_with(&pct(&sign)));
        assert!(!p.url.contains('+') && !p.url.ends_with('='));
    }

    #[test]
    fn matrix_puts_with_an_encoded_room() {
        let p = prepare(
            &ch("matrix", "https://matrix.example.org", "syt_tok", "!room:example.org"),
            &msg(),
            42,
        )
        .unwrap();
        assert!(p.put);
        assert_eq!(
            p.url,
            "https://matrix.example.org/_matrix/client/v3/rooms/%21room%3Aexample.org/send/m.room.message/pca42"
        );
        assert_eq!(json_body(&p)["msgtype"], "m.text");
    }

    #[test]
    fn webhook_carries_the_event_and_its_data() {
        let p = prepare(&ch("webhook", "http://n8n.local:5678/webhook/calls", "s3cret", ""), &msg(), 0).unwrap();
        assert_eq!(p.headers[0].1, "Bearer s3cret");
        let b = json_body(&p);
        assert_eq!(b["event"], "summary");
        assert_eq!(b["data"]["caller_id"], "0912345678");
        assert_eq!(b["body"], b["text"]);
        let p = prepare(&ch("apprise", "http://apprise:8000/notify/calls", "", "phone"), &msg(), 0).unwrap();
        let f = form_body(&p);
        assert_eq!(f["tag"], "phone");
        assert_eq!(f["title"], msg().title);
    }

    #[test]
    fn long_messages_are_cut_to_each_service_limit() {
        let long = Message { body: "字".repeat(9000), html: None, ..msg() };
        let p = prepare(&ch("discord", "https://discord.com/api/webhooks/1/x", "", ""), &long, 0).unwrap();
        assert_eq!(json_body(&p)["content"].as_str().unwrap().chars().count(), 1900);
        let p = prepare(&ch("telegram", "", "1:A", "2"), &long, 0).unwrap();
        assert_eq!(json_body(&p)["text"].as_str().unwrap().chars().count(), 4000);
    }

    #[test]
    fn settings_page_never_gets_the_secrets_back() {
        let c = Channel {
            name: "Family".into(),
            ..ch("discord", "https://discord.com/api/webhooks/123456/SECRETSECRET", "tok_1234567890", "")
        };
        let v = c.masked();
        let text = v.to_string();
        assert!(!text.contains("SECRETSECRET") && !text.contains("tok_1234567890"), "{text}");
        assert_eq!(v["url_hint"], "https://discord.com/…");
        assert_eq!(v["token_hint"], "tok…7890");
        assert_eq!(v["url_set"], true);
        assert_eq!(v["name"], "Family");
        assert_eq!(mask_url("https://ntfy.sh"), "https://ntfy.sh");
        assert_eq!(mask_url("someone@gmail.com"), "som….com");
        assert_eq!(mask_url(""), "");
    }

    #[test]
    fn saving_keeps_stored_secrets_and_drops_unknown_kinds() {
        let stored = vec![
            Channel { id: "a1".into(), ..ch("discord", "https://discord.com/api/webhooks/1/x", "", "") },
            Channel { id: "b2".into(), ..ch("telegram", "", "123:ABC", "9") },
        ];
        let incoming = vec![
            // Came back masked: empty address, renamed and switched off.
            Channel { id: "a1".into(), name: "Home".into(), enabled: false, ..ch("discord", "", "", "") },
            // Token replaced.
            Channel { id: "b2".into(), ..ch("telegram", "", "456:DEF", "9") },
            // New row, no id yet.
            Channel { id: String::new(), ..ch("ntfy", "", "", "calls") },
            Channel { id: "zz".into(), ..ch("carrier-pigeon", "", "", "") },
        ];
        let out = merge_channels(&stored, incoming);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].url, "https://discord.com/api/webhooks/1/x");
        assert_eq!(out[0].name, "Home");
        assert!(!out[0].enabled);
        assert_eq!(out[1].token, "456:DEF");
        assert_eq!(out[2].id.len(), 12);
        // A row removed on the page is gone.
        assert!(merge_channels(&stored, Vec::new()).is_empty());
        // The same id on another kind does not inherit a secret.
        let moved = merge_channels(&stored, vec![Channel { id: "b2".into(), ..ch("pushover", "", "", "u") }]);
        assert_eq!(moved[0].token, "");
    }

    #[test]
    fn cards_read_the_same_in_text_and_html() {
        let card = Card::new("📞", "Incoming call")
            .code("Number", "<0912>")
            .row("Phone", "Pixel 8 & co")
            .tail("done");
        assert_eq!(card.plain(), "📞 Incoming call\nNumber：<0912>\nPhone：Pixel 8 & co\n\ndone");
        assert_eq!(
            card.html(),
            "<b>📞 Incoming call</b>\nNumber：<code>&lt;0912&gt;</code>\nPhone：Pixel 8 &amp; co\n\ndone"
        );
    }

    #[test]
    fn devices_are_known_by_their_model() {
        remember_device("pixel-test-77001", " Pixel 8 ");
        assert_eq!(device_label("pixel-test-77001"), "Pixel 8");
        assert_eq!(device_label("pixel-test-77002"), "pixel-test-77002");
        remember_device("pixel-test-77003", "");
        assert_eq!(device_label("pixel-test-77003"), "pixel-test-77003");
        // The owner's own name wins over what the phone reports.
        let mut names = std::collections::BTreeMap::new();
        names.insert("pixel-test-77001".to_string(), "Kitchen phone".to_string());
        names.insert("pixel-test-77009".to_string(), "Spare".to_string());
        set_device_labels(&names);
        assert_eq!(device_label("pixel-test-77001"), "Kitchen phone");
        assert_eq!(device_label("pixel-test-77009"), "Spare");
        let known = known_devices();
        assert!(known.contains(&("pixel-test-77001".to_string(), "Kitchen phone".to_string())));
        assert!(known.contains(&("pixel-test-77009".to_string(), "Spare".to_string())));
        set_device_labels(&std::collections::BTreeMap::new());
        assert_eq!(device_label("pixel-test-77001"), "Pixel 8");
    }
}
