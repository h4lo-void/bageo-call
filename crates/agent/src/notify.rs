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
    /// The owner may operate the agent from this chat ([crate::control]);
    /// kinds in control::COMMAND_KINDS only. Off by default.
    #[serde(default)]
    pub control: bool,
    /// Who may give commands there (platform user ids); empty = anyone in the chat.
    #[serde(default)]
    pub control_users: Vec<String>,
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
    Kind { kind: "teams", label: "Microsoft Teams (Workflows)", url: "Workflow webhook URL", token: "", target: "", url_default: "" },
    Kind { kind: "googlechat", label: "Google Chat", url: "Webhook URL", token: "", target: "", url_default: "" },
    Kind { kind: "rocketchat", label: "Rocket.Chat", url: "Webhook URL", token: "", target: "", url_default: "" },
    Kind { kind: "zulip", label: "Zulip", url: "Server URL", token: "Bot email:API key", target: "Channel > topic, or email", url_default: "" },
    Kind { kind: "webex", label: "Webex", url: "", token: "Bot access token", target: "Room ID or email", url_default: "" },
    Kind { kind: "nextcloud", label: "Nextcloud Talk", url: "Server URL", token: "User:app password", target: "Conversation token", url_default: "" },
    Kind { kind: "synology", label: "Synology Chat", url: "Webhook URL", token: "", target: "", url_default: "" },
    Kind { kind: "line", label: "LINE (Messaging API)", url: "", token: "Channel access token", target: "User / group ID", url_default: "" },
    Kind { kind: "signal", label: "Signal (signal-cli-rest-api)", url: "API URL", token: "Sender number", target: "Recipients", url_default: "" },
    Kind { kind: "ntfy", label: "ntfy", url: "Server", token: "Access token (optional)", target: "Topic", url_default: "https://ntfy.sh" },
    Kind { kind: "gotify", label: "Gotify", url: "Server URL", token: "App token", target: "", url_default: "" },
    Kind { kind: "bark", label: "Bark (iOS)", url: "Server", token: "Device key", target: "", url_default: "https://api.day.app" },
    Kind { kind: "pushover", label: "Pushover", url: "", token: "API token", target: "User key", url_default: "" },
    Kind { kind: "pushbullet", label: "Pushbullet", url: "", token: "Access token", target: "Device ID or email (optional)", url_default: "" },
    Kind { kind: "wecom", label: "WeCom group bot (企业微信)", url: "Webhook URL", token: "", target: "", url_default: "" },
    Kind { kind: "dingtalk", label: "DingTalk bot (钉钉)", url: "Webhook URL", token: "Signing secret (optional)", target: "", url_default: "" },
    Kind { kind: "feishu", label: "Feishu / Lark bot (飞书)", url: "Webhook URL", token: "", target: "", url_default: "" },
    Kind { kind: "serverchan", label: "ServerChan (Server酱)", url: "", token: "SendKey", target: "", url_default: "" },
    Kind { kind: "pushplus", label: "PushPlus (推送加)", url: "", token: "Token", target: "Group code (optional)", url_default: "" },
    Kind { kind: "pushdeer", label: "PushDeer", url: "Server", token: "PushKey", target: "", url_default: "https://api2.pushdeer.com" },
    Kind { kind: "wxpusher", label: "WxPusher", url: "", token: "appToken (AT_…) or SPT", target: "UIDs or topic IDs", url_default: "" },
    Kind { kind: "chanify", label: "Chanify", url: "Server", token: "Token", target: "", url_default: "https://api.chanify.net" },
    Kind { kind: "matrix", label: "Matrix", url: "Homeserver URL", token: "Access token", target: "Room ID", url_default: "" },
    Kind { kind: "gmail", label: "Email (Gmail)", url: "Gmail address", token: "App password", target: "Send to (empty = yourself)", url_default: "" },
    Kind { kind: "smtp", label: "Email (SMTP)", url: "SMTP URL (smtp://login@host:port)", token: "Password", target: "Send to (empty = the login)", url_default: "" },
    Kind { kind: "homeassistant", label: "Home Assistant", url: "Server URL", token: "Long-lived access token", target: "Notify service", url_default: "http://homeassistant.local:8123" },
    Kind { kind: "ifttt", label: "IFTTT Webhooks", url: "", token: "Webhooks key", target: "Event name", url_default: "" },
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
            "control": self.control,
            "control_users": self.control_users,
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
        // Commands only where an adapter exists; who may give them, as ids.
        c.control = c.control && crate::control::COMMAND_KINDS.contains(&c.kind.as_str());
        c.control_users = crate::settings_store::sanitize_sender_ids(&c.control_users);
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

fn bad_value(field: &'static str, detail: impl Into<String>) -> anyhow::Error {
    ChannelError::new("bad_value", field, detail).into()
}

/// HTTP Basic credentials typed into one field as `name:secret`, the way the
/// services' own curl examples write them (`-u name:secret`).
fn basic_auth(field: &'static str, what: &str, value: &str) -> Result<String> {
    let v = need(field, what, value)?;
    match v.split_once(':') {
        Some((name, secret)) if !name.trim().is_empty() && !secret.trim().is_empty() => Ok(format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", name.trim(), secret.trim()))
        )),
        _ => Err(bad_value(field, format!("{what} must be written as name:secret"))),
    }
}

/// Several recipients in one field, separated by commas, semicolons or lines.
fn list(s: &str) -> Vec<String> {
    s.split([',', ';', '\n'])
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_string)
        .collect()
}

/// "+1 555-0100" as signal-cli wants it ("+15550100"); group IDs and other
/// values are left alone.
fn phone_number(s: &str) -> String {
    let t = s.trim();
    if t.starts_with('+') {
        t.chars().filter(|c| !matches!(c, ' ' | '-' | '(' | ')' | '.')).collect()
    } else {
        t.to_string()
    }
}

/// A Teams message holding one Adaptive Card. Adaptive Cards ignore single line
/// breaks inside a TextBlock, so every line gets its own block; the first line
/// is the headline and a blank line becomes extra space above the next block.
fn teams_card(body: &str) -> Value {
    let mut blocks: Vec<Value> = Vec::new();
    let mut gap = false;
    for line in clip(body, 3500).lines() {
        if line.trim().is_empty() {
            gap = true;
            continue;
        }
        let mut block = json!({ "type": "TextBlock", "text": line, "wrap": true });
        if blocks.is_empty() {
            block["weight"] = json!("Bolder");
            block["size"] = json!("Medium");
        } else {
            block["spacing"] = json!(if gap { "Medium" } else { "None" });
        }
        gap = false;
        blocks.push(block);
    }
    json!({
        "type": "message",
        "attachments": [{
            "contentType": "application/vnd.microsoft.card.adaptive",
            "contentUrl": null,
            "content": {
                "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                "type": "AdaptiveCard",
                "version": "1.4",
                "body": blocks,
            },
        }],
    })
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
        // Microsoft Learn, "Create incoming webhooks with Workflows for Microsoft
        // Teams": the "Post to a channel when a webhook request is received"
        // workflow takes a message whose attachment is an Adaptive Card. The old
        // Office 365 connectors (MessageCard) are retired.
        "teams" => post_json(need_http("url", "workflow webhook URL", &base)?, teams_card(body)),
        // https://developers.google.com/workspace/chat/quickstart/webhooks — the
        // space's webhook URL carries its key and token; text is capped at 4,096
        // characters.
        "googlechat" => post_json(need_http("url", "webhook URL", &base)?, json!({ "text": clip(body, 4000) })),
        // https://docs.rocket.chat (Integrations → Incoming webhooks): POST
        // {"text": …} to …/hooks/<id>/<token>.
        "rocketchat" => post_json(need_http("url", "webhook URL", &base)?, json!({ "text": clip(body, 3500) })),
        // https://zulip.com/api/send-message — HTTP Basic with the bot's email
        // and API key, form-encoded. Messages are capped at 10,000 bytes, hence
        // 2,400 characters (four bytes each at worst).
        "zulip" => {
            let auth = basic_auth("token", "bot email:API key", &ch.token)?;
            let url = format!("{}/api/v1/messages", need_http("url", "server URL", &base)?);
            let target = need("target", "channel or email", &ch.target)?;
            let mut form = Vec::new();
            if let Some((channel, topic)) = target.split_once('>') {
                let channel = channel.trim().trim_start_matches('#').trim();
                if channel.is_empty() {
                    return Err(bad_value("target", "channel name is empty"));
                }
                let topic = if topic.trim().is_empty() { "Bageo" } else { topic.trim() };
                form.push(("type".to_string(), "stream".to_string()));
                form.push(("to".to_string(), channel.to_string()));
                form.push(("topic".to_string(), clip(topic, 60)));
            } else if target.contains('@') {
                // "private" is the name every server version takes ("direct" only
                // since Zulip 7.0); `to` is a JSON list of emails.
                form.push(("type".to_string(), "private".to_string()));
                form.push(("to".to_string(), json!(list(target)).to_string()));
            } else {
                // Every channel message needs a topic; a fixed one keeps all
                // calls in one thread instead of one topic per caller.
                form.push(("type".to_string(), "stream".to_string()));
                form.push(("to".to_string(), target.trim_start_matches('#').to_string()));
                form.push(("topic".to_string(), "Bageo".to_string()));
            }
            form.push(("content".to_string(), clip(body, 2400)));
            Prepared { put: false, url, headers: vec![("Authorization".into(), auth)], body: Body::Form(form) }
        }
        // https://developer.webex.com/docs/api/v1/messages/create-a-message —
        // text is capped at 7,439 bytes, hence 1,800 characters.
        "webex" => {
            let token = need("token", "bot access token", &ch.token)?;
            let to = need("target", "room ID or email", &ch.target)?;
            let mut payload = json!({ "text": clip(body, 1800) });
            if to.contains('@') {
                payload["toPersonEmail"] = json!(to);
            } else {
                payload["roomId"] = json!(to);
            }
            Prepared {
                put: false,
                url: "https://webexapis.com/v1/messages".to_string(),
                headers: vec![("Authorization".into(), format!("Bearer {token}"))],
                body: Body::Json(payload),
            }
        }
        // https://nextcloud-talk.readthedocs.io/en/latest/chat/ ("Sending a new
        // chat message"): OCS API, HTTP Basic with a user and an app password.
        "nextcloud" => {
            let auth = basic_auth("token", "user:app password", &ch.token)?;
            let url = format!(
                "{}/ocs/v2.php/apps/spreed/api/v1/chat/{}",
                need_http("url", "server URL", &base)?,
                pct(need("target", "conversation token", &ch.target)?)
            );
            Prepared {
                put: false,
                url,
                headers: vec![
                    ("Authorization".into(), auth),
                    ("OCS-APIRequest".into(), "true".into()),
                    ("Accept".into(), "application/json".into()),
                ],
                body: Body::Json(json!({ "message": clip(body, 3500) })),
            }
        }
        // Synology Chat help, "Integration → Incoming webhooks": a form field
        // `payload` holding {"text": …}.
        "synology" => Prepared {
            put: false,
            url: need_http("url", "webhook URL", &base)?,
            headers: Vec::new(),
            body: Body::Form(vec![("payload".into(), json!({ "text": clip(body, 2000) }).to_string())]),
        },
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
        // https://github.com/bbernhard/signal-cli-rest-api (Swagger: POST
        // /v2/send). Signal turns longer texts into attachments, so 2,000 chars.
        "signal" => {
            let number = phone_number(need("token", "sender number", &ch.token)?);
            let recipients: Vec<String> =
                list(need("target", "recipients", &ch.target)?).iter().map(|r| phone_number(r)).collect();
            post_json(
                format!("{}/v2/send", need_http("url", "API URL", &base)?),
                json!({ "message": clip(body, 2000), "number": number, "recipients": recipients }),
            )
        }
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
        // https://docs.pushbullet.com/#create-push — a "note" push to all your
        // devices, or to one device (`device_iden`) or one person (`email`).
        "pushbullet" => {
            let mut payload = json!({ "type": "note", "title": clip(title, 200), "body": clip(body, 3500) });
            let to = ch.target.trim();
            if !to.is_empty() {
                payload[if to.contains('@') { "email" } else { "device_iden" }] = json!(to);
            }
            Prepared {
                put: false,
                url: "https://api.pushbullet.com/v2/pushes".to_string(),
                headers: vec![("Access-Token".into(), need("token", "access token", &ch.token)?.to_string())],
                body: Body::Json(payload),
            }
        }
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
        // https://www.pushplus.plus/doc/guide/api.html — plain text template;
        // `topic` (a group code) sends to a group instead of only yourself.
        "pushplus" => {
            let mut payload = json!({
                "token": need("token", "token", &ch.token)?,
                "title": clip(title, 100),
                "content": clip(body, 3500),
                "template": "txt",
            });
            if !ch.target.trim().is_empty() {
                payload["topic"] = json!(ch.target.trim());
            }
            post_json("https://www.pushplus.plus/send".to_string(), payload)
        }
        // https://github.com/easychen/pushdeer (README, "推送消息"): form
        // fields pushkey, text and type on <server>/message/push.
        "pushdeer" => Prepared {
            put: false,
            url: format!("{}/message/push", need_http("url", "server", &base)?),
            headers: Vec::new(),
            body: Body::Form(vec![
                ("pushkey".into(), need("token", "PushKey", &ch.token)?.to_string()),
                ("text".into(), clip(body, 3500)),
                ("type".into(), "text".into()),
            ]),
        },
        // https://wxpusher.zjiekou.com/docs/ — an appToken sends to user IDs
        // (UID_…) and topic IDs (numbers); a simple push token (SPT_…) sends to
        // its own subscriber. The summary shows only 20 characters in WeChat.
        "wxpusher" => {
            let token = need("token", "appToken or SPT", &ch.token)?;
            if token.starts_with("SPT_") {
                post_json(
                    "https://wxpusher.zjiekou.com/api/send/message/simple-push".to_string(),
                    json!({ "content": clip(body, 3500), "summary": clip(title, 20), "contentType": 1, "spt": token }),
                )
            } else {
                let ids = list(need("target", "UIDs or topic IDs", &ch.target)?);
                let topics: Vec<u64> = ids.iter().filter_map(|i| i.parse().ok()).collect();
                let uids: Vec<&String> = ids.iter().filter(|i| i.parse::<u64>().is_err()).collect();
                post_json(
                    "https://wxpusher.zjiekou.com/api/send/message".to_string(),
                    json!({
                        "appToken": token,
                        "content": clip(body, 3500),
                        "summary": clip(title, 20),
                        "contentType": 1,
                        "uids": uids,
                        "topicIds": topics,
                    }),
                )
            }
        }
        // https://github.com/chanify/chanify (README, "Send message"): form
        // fields title and text on <server>/v1/sender/<token>.
        "chanify" => Prepared {
            put: false,
            url: format!(
                "{}/v1/sender/{}",
                need_http("url", "server", &base)?,
                pct(need("token", "token", &ch.token)?)
            ),
            headers: Vec::new(),
            body: Body::Form(vec![("title".into(), clip(title, 100)), ("text".into(), clip(body, 3500))]),
        },
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
        // https://developers.home-assistant.io/docs/api/rest/ (POST
        // /api/services/<domain>/<service>) with a long-lived access token.
        // Phone notifications travel through Firebase / APNs, which cap the
        // payload at about 4 KB, hence 1,000 characters.
        "homeassistant" => {
            let service = need("target", "notify service", &ch.target)?;
            let service = service.strip_prefix("notify.").unwrap_or(service);
            if service.is_empty() || !service.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
                return Err(bad_value("target", "a notify service looks like mobile_app_pixel_9"));
            }
            Prepared {
                put: false,
                url: format!("{}/api/services/notify/{service}", need_http("url", "server URL", &base)?),
                headers: vec![(
                    "Authorization".into(),
                    format!("Bearer {}", need("token", "long-lived access token", &ch.token)?),
                )],
                body: Body::Json(json!({ "title": clip(title, 200), "message": clip(body, 1000) })),
            }
        }
        // https://ifttt.com/maker_webhooks (Documentation): value1–value3 are
        // the ingredients an applet can use.
        "ifttt" => {
            let key = need("token", "Webhooks key", &ch.token)?;
            let event = need("target", "event name", &ch.target)?;
            post_json(
                format!("https://maker.ifttt.com/trigger/{}/with/key/{}", pct(event), pct(key)),
                json!({ "value1": clip(title, 200), "value2": clip(body, 3500), "value3": msg.event }),
            )
        }
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
        "gmail" | "smtp" => bail!("{} is sent over SMTP", k.kind),
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
    match answer_error(&text) {
        Some(e) => Err(e.into()),
        None => Ok(()),
    }
}

/// Several services answer 200 with an error inside the body.
fn answer_error(text: &str) -> Option<ChannelError> {
    let v = serde_json::from_str::<Value>(text).ok()?;
    let failed = match v.get("success").and_then(|s| s.as_bool()) {
        // WxPusher's success is code 1000 with "success": true, so the flag
        // wins over the code; Synology Chat and Rocket.Chat fail with false.
        Some(ok) => !ok,
        None => {
            let code = v.get("errcode").or_else(|| v.get("code")).and_then(|c| c.as_i64());
            code.is_some_and(|c| c != 0 && c != 200) || v.get("ok").and_then(|o| o.as_bool()) == Some(false)
        }
    };
    failed.then(|| ChannelError::new("service_error", "", format!("service answered: {}", clip(text.trim(), 200))))
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
    if ch.kind == "smtp" {
        let (server, login) = crate::gmail::parse_smtp_url(&ch.url)?;
        let to = if ch.target.trim().is_empty() { login.as_str() } else { ch.target.trim() };
        return crate::gmail::send_mail_via(
            &server,
            &login,
            need("token", "password", &ch.token)?,
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
        locale::notify_phrases().n_unknown.to_string()
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
    let label = locale::notify_phrases().n_recording;
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
    format!("{}{}{url}", locale::notify_phrases().n_details, kv_sep())
}

/// Between a label and its value, as the owner's language writes it.
fn kv_sep() -> &'static str {
    locale::notify_kv_sep()
}

fn display_or_hidden(s: &str) -> String {
    let t = s.trim();
    if t.is_empty() {
        locale::notify_phrases().n_hidden.to_string()
    } else {
        t.to_string()
    }
}

/// The machine's local time as the owner's language writes it, e.g.
/// "Nov 13, 08:09" or "11月13日 08:09" ([`locale::date_time`]).
fn now_stamp() -> String {
    locale::date_time(locale::notify_lang(), &Local::now().naive_local())
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
    let p = locale::notify_phrases();
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

/// A number on the blocked list called: it got the announcement, no AI — or,
/// `declined` (setting `reject_mode` `decline`), the phone refused it while it rang.
/// `reason` = why the owner listed the number.
pub async fn notify_rejected_call(
    http: &Client,
    settings: &Settings,
    caller_id: &str,
    serial: &str,
    reason: &str,
    declined: bool,
) -> bool {
    if !settings.call_notify {
        return false;
    }
    let p = locale::notify_phrases();
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
        .tail(&blocked_outcome(declined))
        .message(
            "blocked",
            format!("{} {caller}", p.n_blocked),
            json!({
                "caller_id": caller_id.trim(),
                "device": device,
                "time": when,
                "reason": reason,
                "declined": declined,
            }),
        );
    deliver(http, settings, &msg).await
}

/// What became of a blocked number's call: refused while it rang, or picked up
/// for the "cannot be reached" announcement.
fn blocked_outcome(declined: bool) -> String {
    if declined {
        locale::notify_zh_or(
            "響鈴時直接拒接，沒有接起來，沒有用 AI",
            "Declined while it rang: not picked up, no AI",
        )
    } else {
        let p = locale::notify_phrases();
        p.n_blocked_done.replace("{}", p.unavailable)
    }
}

/// Headline of a call the phone refused because it was already on a call.
fn call_waiting_headline(caller: &str, device: &str) -> String {
    locale::notify_zh_or(
        &format!("通話中插播未接：{caller}（{device}）"),
        &format!("Missed while on another call: {caller} ({device})"),
    )
}

/// A second call came in during a call and the phone turned it away (busy).
/// `sim_country`: the SIM it came in on, when the phone said.
pub async fn notify_call_waiting_rejected(
    http: &Client,
    settings: &Settings,
    caller_id: &str,
    serial: &str,
    sim_country: Option<&str>,
) -> bool {
    if !settings.call_notify {
        return false;
    }
    let p = locale::notify_phrases();
    let caller = display_or_hidden(caller_id);
    let device = device_label(serial);
    let when = now_stamp();
    let headline = call_waiting_headline(&caller, &device);
    let msg = Card::new("📵", &headline).row(p.n_time, &when).message(
        "call_waiting",
        headline.clone(),
        json!({
            "caller_id": caller_id.trim(),
            "device": device,
            "time": when,
            "sim_country": sim_country.unwrap_or(""),
        }),
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
    let p = locale::notify_phrases();
    let caller = display_or_hidden(caller_id);
    let device = device_label(serial);
    let when = now_stamp();
    // A caller in the contacts: the name after the number in the title.
    let contact = crate::contacts::lookup(caller_id, None).map(|c| c.name);
    let headline = match contact.as_deref() {
        Some(name) => format!("{} · {name}", p.n_summary),
        None => p.n_summary.to_string(),
    };
    let msg = Card::new("☎️", &headline)
        .code(p.n_number, &caller)
        .row(p.n_phone, &device)
        .row(p.n_time, &when)
        .tail(summary_body)
        .message(
            "summary",
            format!("{} {}", p.n_summary, crate::contacts::named_number(&caller, contact.as_deref())),
            json!({
                "caller_id": caller_id.trim(),
                "device": device,
                "time": when,
                "summary": summary_body.trim(),
                "contact": contact.as_deref().unwrap_or(""),
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
    let p = locale::notify_phrases();
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

/// About the AI's call-back with an appointment's answer: the summary of its
/// call, or why it never got going. `headline`: "AI 回電：<number>" in the
/// owner's language (`crate::appointments::callback_headline`).
pub async fn notify_ai_callback(
    http: &Client,
    settings: &Settings,
    caller_id: &str,
    serial: &str,
    headline: &str,
    body: &str,
) -> bool {
    if !settings.call_notify {
        return false;
    }
    deliver(http, settings, &ai_callback_message(caller_id, &device_label(serial), &now_stamp(), headline, body)).await
}

fn ai_callback_message(caller_id: &str, device: &str, when: &str, headline: &str, body: &str) -> Message {
    let p = locale::notify_phrases();
    let caller = display_or_hidden(caller_id);
    Card::new("📞", headline)
        .code(p.n_number, &caller)
        .row(p.n_phone, device)
        .row(p.n_time, when)
        .tail(body)
        .message(
            "ai_callback",
            headline.to_string(),
            json!({
                "caller_id": caller_id.trim(),
                "device": device,
                "time": when,
                "summary": body.trim(),
            }),
        )
}

/// About an errand the AI called for (`crate::errands`): what came of it, or
/// why the call never got going. `headline`: "AI 代打：<name or number>" in
/// the owner's language.
pub async fn notify_ai_errand(
    http: &Client,
    settings: &Settings,
    number: &str,
    serial: &str,
    headline: &str,
    body: &str,
) -> bool {
    if !settings.call_notify {
        return false;
    }
    deliver(http, settings, &ai_errand_message(number, &device_label(serial), &now_stamp(), headline, body)).await
}

fn ai_errand_message(number: &str, device: &str, when: &str, headline: &str, body: &str) -> Message {
    let p = locale::notify_phrases();
    Card::new("📞", headline)
        .code(p.n_number, &display_or_hidden(number))
        .row(p.n_phone, device)
        .row(p.n_time, when)
        .tail(body)
        .message(
            "ai_errand",
            headline.to_string(),
            json!({
                "number": number.trim(),
                "device": device,
                "time": when,
                "summary": body.trim(),
            }),
        )
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
    let p = locale::notify_phrases();
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
        ..Channel::default()
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
    // Drop control characters; Chinese goes out in the owner's script
    // (Traditional when the owner reads zh-TW, see `locale::for_owner`).
    let msg = Message {
        title: locale::for_owner(&sanitize_notify_text(&msg.title, 200)),
        body: locale::for_owner(&sanitize_notify_text(&msg.body, 3500)),
        html: msg
            .html
            .as_deref()
            .map(|h| locale::for_owner(&sanitize_notify_text(h, 3500))),
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
            ..Channel::default()
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

    #[test]
    fn blocked_card_says_announced_unless_declined() {
        // An app that cannot decline picked up: the card names the announcement.
        let unavailable = locale::notify_phrases().unavailable;
        assert!(blocked_outcome(false).contains(unavailable));
        assert!(!blocked_outcome(true).contains(unavailable));
    }

    #[test]
    fn an_ai_call_back_has_its_own_title() {
        let m = ai_callback_message("0912000000", "Pixel 9", "11/14 15:00", "AI 回電：0912000000", "AI 回電了，對方沒接");
        assert!(m.body.starts_with("📞 AI 回電：0912000000\n"), "{}", m.body);
        assert!(m.body.ends_with("AI 回電了，對方沒接") && m.html.unwrap().starts_with("<b>📞 AI 回電：0912000000</b>"));
        assert_eq!((m.event.as_str(), m.title.as_str()), ("ai_callback", "AI 回電：0912000000"));
    }

    fn form_body(p: &Prepared) -> HashMap<String, String> {
        match &p.body {
            Body::Form(f) => f.iter().cloned().collect(),
            Body::Json(_) => panic!("json body"),
        }
    }

    /// A JSON pointer into a JSON body, or a field name of a form body.
    fn field(p: &Prepared, key: &str) -> String {
        match &p.body {
            Body::Json(v) => v.pointer(key).and_then(|x| x.as_str()).unwrap_or_else(|| panic!("{key} in {v}")).to_string(),
            Body::Form(f) => f.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()).unwrap_or_else(|| panic!("{key} in {f:?}")),
        }
    }

    fn basic(credentials: &str) -> String {
        format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(credentials))
    }

    fn code_and_field(e: &anyhow::Error) -> Option<(&'static str, &'static str)> {
        channel_error(e).map(|c| (c.code, c.field))
    }

    #[test]
    fn every_kind_prepares_a_request_or_says_what_is_missing() {
        for k in KINDS {
            if k.kind == "gmail" || k.kind == "smtp" {
                continue;
            }
            let empty = prepare(&ch(k.kind, "", "", ""), &msg(), 1);
            let err = empty.err().unwrap_or_else(|| panic!("{} accepted an empty channel", k.kind));
            // The settings page names the empty field.
            let ce = channel_error(&err).unwrap_or_else(|| panic!("{}: {err:#}", k.kind));
            assert_eq!(ce.code, "missing_field", "{}", k.kind);
            assert!(["url", "token", "target"].contains(&ce.field), "{}: {}", k.kind, ce.field);
            // Services logging in with HTTP Basic take "name:secret".
            let token = if matches!(k.kind, "zulip" | "nextcloud") { "bot@example.org:tok_1234567890" } else { "tok_1234567890" };
            let full = prepare(&ch(k.kind, "https://example.org/hook/abc", token, "target1"), &msg(), 1)
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
    fn teams_gets_an_adaptive_card_with_one_block_per_line() {
        let m = Message { body: "☎️ Call summary\nNumber: 0912345678\n\nCaller: 王小明".into(), ..msg() };
        let url = "https://example.logic.azure.com:443/workflows/abc/triggers/manual/paths/invoke?sig=s1";
        let p = prepare(&ch("teams", url, "", ""), &m, 0).unwrap();
        assert_eq!(p.url, url);
        assert!(p.headers.is_empty());
        let b = json_body(&p);
        assert_eq!(b["type"], "message");
        // The retired Office 365 connector format is not used.
        assert!(b.get("@type").is_none() && b.get("themeColor").is_none());
        let att = &b["attachments"][0];
        assert_eq!(att["contentType"], "application/vnd.microsoft.card.adaptive");
        assert_eq!(att["content"]["type"], "AdaptiveCard");
        let blocks = att["content"]["body"].as_array().unwrap();
        let texts: Vec<&str> = blocks.iter().map(|b| b["text"].as_str().unwrap()).collect();
        assert_eq!(texts, ["☎️ Call summary", "Number: 0912345678", "Caller: 王小明"]);
        assert_eq!(blocks[0]["weight"], "Bolder");
        assert_eq!(blocks[1]["spacing"], "None");
        assert_eq!(blocks[2]["spacing"], "Medium");
        assert!(blocks.iter().all(|b| b["type"] == "TextBlock" && b["wrap"] == true));
    }

    #[test]
    fn google_chat_rocket_chat_and_synology_take_their_webhook_shapes() {
        let m = msg();
        let url = "https://chat.googleapis.com/v1/spaces/AAAA/messages?key=k1&token=t1";
        let p = prepare(&ch("googlechat", url, "", ""), &m, 0).unwrap();
        assert_eq!(p.url, url);
        assert_eq!(json_body(&p), &json!({ "text": m.body }));
        let p = prepare(&ch("rocketchat", "https://chat.example.org/hooks/abc/def", "", ""), &m, 0).unwrap();
        assert_eq!(p.url, "https://chat.example.org/hooks/abc/def");
        assert_eq!(json_body(&p), &json!({ "text": m.body }));
        let url = "https://nas.example.org:5001/webapi/entry.cgi?api=SYNO.Chat.External&method=incoming&version=2&token=%22abc%22";
        let p = prepare(&ch("synology", url, "", ""), &m, 0).unwrap();
        assert_eq!(p.url, url);
        let f = form_body(&p);
        assert_eq!(f.len(), 1);
        let payload: Value = serde_json::from_str(&f["payload"]).unwrap();
        assert_eq!(payload, json!({ "text": m.body }));
        for kind in ["teams", "googlechat", "rocketchat", "synology"] {
            let e = prepare(&ch(kind, "", "", ""), &m, 0).unwrap_err();
            assert_eq!(code_and_field(&e), Some(("missing_field", "url")), "{kind}");
            let e = prepare(&ch(kind, "chat.example.org/hook", "", ""), &m, 0).unwrap_err();
            assert_eq!(code_and_field(&e), Some(("bad_url", "url")), "{kind}");
        }
    }

    #[test]
    fn zulip_posts_to_a_channel_topic_or_as_a_direct_message() {
        let m = msg();
        let c = ch("zulip", "https://zulip.example.org/", "bot@example.org:apikey123", "calls > phone");
        let p = prepare(&c, &m, 0).unwrap();
        assert_eq!(p.url, "https://zulip.example.org/api/v1/messages");
        assert_eq!(p.headers, vec![("Authorization".to_string(), basic("bot@example.org:apikey123"))]);
        let f = form_body(&p);
        assert_eq!((f["type"].as_str(), f["to"].as_str(), f["topic"].as_str()), ("stream", "calls", "phone"));
        assert_eq!(f["content"], m.body);
        // A channel alone gets one fixed topic, so every call lands in one thread.
        let f = form_body(&prepare(&ch("zulip", "https://zulip.example.org", "bot@example.org:k", "#calls"), &m, 0).unwrap());
        assert_eq!((f["to"].as_str(), f["topic"].as_str()), ("calls", "Bageo"));
        let direct = ch("zulip", "https://zulip.example.org", "bot@example.org:k", "wang@example.org, lee@example.org");
        let f = form_body(&prepare(&direct, &m, 0).unwrap());
        assert_eq!(f["type"], "private");
        assert_eq!(f["to"], r#"["wang@example.org","lee@example.org"]"#);
        assert!(!f.contains_key("topic"));
        // The API key alone cannot log in; the error never repeats it.
        let e = prepare(&ch("zulip", "https://zulip.example.org", "apikey123", "calls"), &m, 0).unwrap_err();
        assert_eq!(code_and_field(&e), Some(("bad_value", "token")));
        assert!(!format!("{e:#}").contains("apikey123"));
        let e = prepare(&ch("zulip", "https://zulip.example.org", "bot@example.org:k", " > phone"), &m, 0).unwrap_err();
        assert_eq!(code_and_field(&e), Some(("bad_value", "target")));
    }

    #[test]
    fn webex_and_nextcloud_talk_name_the_room_they_post_to() {
        let m = msg();
        let p = prepare(&ch("webex", "", "bot-token", "Y2lzY29zcGFyazovL3Vz"), &m, 0).unwrap();
        assert_eq!(p.url, "https://webexapis.com/v1/messages");
        assert_eq!(p.headers[0], ("Authorization".to_string(), "Bearer bot-token".to_string()));
        assert_eq!(json_body(&p), &json!({ "text": m.body, "roomId": "Y2lzY29zcGFyazovL3Vz" }));
        let p = prepare(&ch("webex", "", "bot-token", "wang@example.org"), &m, 0).unwrap();
        assert_eq!(json_body(&p), &json!({ "text": m.body, "toPersonEmail": "wang@example.org" }));
        let e = prepare(&ch("webex", "", "bot-token", ""), &m, 0).unwrap_err();
        assert_eq!(code_and_field(&e), Some(("missing_field", "target")));

        let p = prepare(&ch("nextcloud", "https://cloud.example.org/", "wang:app-pass-123", "abc123xy"), &m, 0).unwrap();
        assert_eq!(p.url, "https://cloud.example.org/ocs/v2.php/apps/spreed/api/v1/chat/abc123xy");
        let h: HashMap<String, String> = p.headers.iter().cloned().collect();
        assert_eq!(h["Authorization"], basic("wang:app-pass-123"));
        assert_eq!(h["OCS-APIRequest"], "true");
        assert_eq!(h["Accept"], "application/json");
        assert_eq!(json_body(&p), &json!({ "message": m.body }));
        let e = prepare(&ch("nextcloud", "https://cloud.example.org", "wang", "abc123xy"), &m, 0).unwrap_err();
        assert_eq!(code_and_field(&e), Some(("bad_value", "token")));
    }

    #[test]
    fn signal_sends_from_the_registered_number_to_every_recipient() {
        let p = prepare(
            &ch("signal", "http://signal-api:8080/", "+1 555 0100", "+1 555-0101, group.abc123==\n+15550102"),
            &msg(),
            0,
        )
        .unwrap();
        assert_eq!(p.url, "http://signal-api:8080/v2/send");
        assert_eq!(
            json_body(&p),
            &json!({
                "message": msg().body,
                "number": "+15550100",
                "recipients": ["+15550101", "group.abc123==", "+15550102"],
            })
        );
        let e = prepare(&ch("signal", "http://signal-api:8080", "+15550100", ""), &msg(), 0).unwrap_err();
        assert_eq!(code_and_field(&e), Some(("missing_field", "target")));
    }

    #[test]
    fn home_assistant_calls_a_notify_service() {
        let m = msg();
        let p = prepare(&ch("homeassistant", "", "llat-token", "notify.mobile_app_pixel_9"), &m, 0).unwrap();
        assert_eq!(p.url, "http://homeassistant.local:8123/api/services/notify/mobile_app_pixel_9");
        assert_eq!(p.headers[0], ("Authorization".to_string(), "Bearer llat-token".to_string()));
        assert_eq!(json_body(&p), &json!({ "title": m.title, "message": m.body }));
        let p = prepare(&ch("homeassistant", "https://ha.example.org", "llat-token", "family"), &m, 0).unwrap();
        assert_eq!(p.url, "https://ha.example.org/api/services/notify/family");
        // Anything else would change the path the token is sent to.
        let e = prepare(&ch("homeassistant", "", "llat-token", "../states/x"), &m, 0).unwrap_err();
        assert_eq!(code_and_field(&e), Some(("bad_value", "target")));
        // "notify." alone names no service: the request would go to the notify domain itself.
        let e = prepare(&ch("homeassistant", "", "llat-token", "notify."), &m, 0).unwrap_err();
        assert_eq!(code_and_field(&e), Some(("bad_value", "target")));
    }

    #[test]
    fn more_push_services_get_their_own_fields() {
        let m = msg();
        let p = prepare(&ch("pushbullet", "", "o.abc123", ""), &m, 0).unwrap();
        assert_eq!(p.url, "https://api.pushbullet.com/v2/pushes");
        assert_eq!(p.headers[0], ("Access-Token".to_string(), "o.abc123".to_string()));
        assert_eq!(json_body(&p), &json!({ "type": "note", "title": m.title, "body": m.body }));
        let p = prepare(&ch("pushbullet", "", "o.abc123", "ujpah72o0"), &m, 0).unwrap();
        assert_eq!(json_body(&p)["device_iden"], "ujpah72o0");
        let p = prepare(&ch("pushbullet", "", "o.abc123", "wang@example.org"), &m, 0).unwrap();
        assert_eq!(json_body(&p)["email"], "wang@example.org");

        let p = prepare(&ch("pushplus", "", "pp-token", ""), &m, 0).unwrap();
        assert_eq!(p.url, "https://www.pushplus.plus/send");
        assert_eq!(
            json_body(&p),
            &json!({ "token": "pp-token", "title": m.title, "content": m.body, "template": "txt" })
        );
        let p = prepare(&ch("pushplus", "", "pp-token", "family"), &m, 0).unwrap();
        assert_eq!(json_body(&p)["topic"], "family");

        let p = prepare(&ch("pushdeer", "", "PDU1Tabc", ""), &m, 0).unwrap();
        assert_eq!(p.url, "https://api2.pushdeer.com/message/push");
        let f = form_body(&p);
        assert_eq!((f["pushkey"].as_str(), f["text"].as_str(), f["type"].as_str()), ("PDU1Tabc", m.body.as_str(), "text"));
        let p = prepare(&ch("pushdeer", "https://pushdeer.example.org/", "PDU1Tabc", ""), &m, 0).unwrap();
        assert_eq!(p.url, "https://pushdeer.example.org/message/push");

        let p = prepare(&ch("wxpusher", "", "AT_app", "UID_wang, 12345"), &m, 0).unwrap();
        assert_eq!(p.url, "https://wxpusher.zjiekou.com/api/send/message");
        let b = json_body(&p);
        assert_eq!((b["appToken"].as_str(), b["contentType"].as_i64()), (Some("AT_app"), Some(1)));
        assert_eq!((&b["uids"], &b["topicIds"]), (&json!(["UID_wang"]), &json!([12345])));
        assert_eq!(b["content"], m.body);
        assert_eq!(b["summary"], clip(&m.title, 20));
        // A simple push token needs no recipients.
        let p = prepare(&ch("wxpusher", "", "SPT_abc", ""), &m, 0).unwrap();
        assert_eq!(p.url, "https://wxpusher.zjiekou.com/api/send/message/simple-push");
        assert_eq!(json_body(&p)["spt"], "SPT_abc");
        let e = prepare(&ch("wxpusher", "", "AT_app", ""), &m, 0).unwrap_err();
        assert_eq!(code_and_field(&e), Some(("missing_field", "target")));

        let p = prepare(&ch("chanify", "", "CH/tok", ""), &m, 0).unwrap();
        assert_eq!(p.url, "https://api.chanify.net/v1/sender/CH%2Ftok");
        let f = form_body(&p);
        assert_eq!((f["title"].as_str(), f["text"].as_str()), (m.title.as_str(), m.body.as_str()));

        let p = prepare(&ch("ifttt", "", "key 1", "bageo_call"), &m, 0).unwrap();
        assert_eq!(p.url, "https://maker.ifttt.com/trigger/bageo_call/with/key/key%201");
        assert_eq!(json_body(&p), &json!({ "value1": m.title, "value2": m.body, "value3": "summary" }));
    }

    #[test]
    fn new_services_cut_long_messages_to_their_limits() {
        let long = Message { title: "標".repeat(500), body: "字".repeat(9000), html: None, ..msg() };
        let cases = [
            (ch("teams", "https://example.org/workflows/a", "", ""), "/attachments/0/content/body/0/text", 3500),
            (ch("googlechat", "https://chat.googleapis.com/v1/spaces/A/messages", "", ""), "/text", 4000),
            (ch("rocketchat", "https://chat.example.org/hooks/a/b", "", ""), "/text", 3500),
            (ch("zulip", "https://zulip.example.org", "bot@example.org:k", "calls"), "content", 2400),
            (ch("webex", "", "bot-token", "room1"), "/text", 1800),
            (ch("nextcloud", "https://cloud.example.org", "wang:pass", "room1"), "/message", 3500),
            (ch("signal", "http://signal-api:8080", "+15550100", "+15550101"), "/message", 2000),
            (ch("pushbullet", "", "o.abc", ""), "/body", 3500),
            (ch("pushbullet", "", "o.abc", ""), "/title", 200),
            (ch("pushplus", "", "pp", ""), "/content", 3500),
            (ch("pushplus", "", "pp", ""), "/title", 100),
            (ch("pushdeer", "", "PDU1", ""), "text", 3500),
            (ch("wxpusher", "", "AT_app", "UID_wang"), "/content", 3500),
            (ch("wxpusher", "", "AT_app", "UID_wang"), "/summary", 20),
            (ch("chanify", "", "CH", ""), "text", 3500),
            (ch("chanify", "", "CH", ""), "title", 100),
            (ch("homeassistant", "", "llat", "mobile_app_x"), "/message", 1000),
            (ch("homeassistant", "", "llat", "mobile_app_x"), "/title", 200),
            (ch("ifttt", "", "key1", "bageo_call"), "/value1", 200),
            (ch("ifttt", "", "key1", "bageo_call"), "/value2", 3500),
        ];
        for (c, key, limit) in cases {
            let p = prepare(&c, &long, 0).unwrap_or_else(|e| panic!("{}: {e:#}", c.kind));
            let got = field(&p, key);
            assert_eq!(got.chars().count(), limit, "{} {key}", c.kind);
            assert!(got.ends_with('…'), "{} {key}", c.kind);
        }
        // Zulip and Webex count bytes: even four-byte characters stay inside.
        let emoji = Message { body: "😀".repeat(9000), ..long.clone() };
        let p = prepare(&ch("zulip", "https://zulip.example.org", "bot@example.org:k", "calls"), &emoji, 0).unwrap();
        assert!(field(&p, "content").len() <= 10_000);
        let p = prepare(&ch("webex", "", "bot-token", "room1"), &emoji, 0).unwrap();
        assert!(field(&p, "/text").len() <= 7439);
        let p = prepare(&ch("synology", "https://nas.example.org/webapi/entry.cgi", "", ""), &long, 0).unwrap();
        let payload: Value = serde_json::from_str(&form_body(&p)["payload"]).unwrap();
        assert_eq!(payload["text"].as_str().unwrap().chars().count(), 2000);
    }

    #[test]
    fn errors_inside_a_200_answer_are_caught_but_wxpusher_success_is_not() {
        assert!(answer_error(r#"{"code":1000,"msg":"处理成功","data":[],"success":true}"#).is_none());
        assert!(answer_error(r#"{"code":1001,"msg":"appToken不正确","success":false}"#).is_some());
        assert!(answer_error(r#"{"success":false,"error":{"code":120,"errors":{}}}"#).is_some());
        assert!(answer_error(r#"{"success":true}"#).is_none());
        assert!(answer_error(r#"{"code":200,"msg":"请求成功","data":"x"}"#).is_none());
        assert!(answer_error(r#"{"code":0,"content":{"result":[]}}"#).is_none());
        assert_eq!(answer_error(r#"{"errcode":93000,"errmsg":"invalid"}"#).map(|e| e.code), Some("service_error"));
        assert!(answer_error(r#"{"ok":false}"#).is_some());
        assert!(answer_error("Congratulations! You've fired the bageo_call event").is_none());
        assert!(answer_error("[]").is_none());
    }

    #[tokio::test]
    async fn an_smtp_channel_names_its_empty_fields_before_connecting() {
        let http = Client::new();
        let e = send_channel(&http, &ch("smtp", "", "secret-pw", ""), &msg()).await.unwrap_err();
        assert_eq!(code_and_field(&e), Some(("missing_field", "url")));
        let url = "smtp://me@example.org@smtp.example.org:587";
        let e = send_channel(&http, &ch("smtp", url, "", ""), &msg()).await.unwrap_err();
        assert_eq!(code_and_field(&e), Some(("missing_field", "token")));
        let e = send_channel(&http, &ch("smtp", "https://smtp.example.org", "secret-pw", ""), &msg()).await.unwrap_err();
        assert_eq!(code_and_field(&e), Some(("bad_value", "url")));
        let e = send_channel(&http, &ch("smtp", url, "secret-pw", "not-an-address"), &msg()).await.unwrap_err();
        assert_eq!(code_and_field(&e), Some(("bad_value", "target")));
        assert!(!format!("{e:#}").contains("secret-pw"));
        // It is SMTP, never an HTTP request.
        assert!(prepare(&ch("smtp", url, "secret-pw", ""), &msg(), 0).is_err());
    }

    #[test]
    fn new_channels_show_only_hints_of_their_secrets() {
        let cases = [
            ("teams", "https://prod-01.westus.logic.azure.com:443/workflows/SECRETFLOW/triggers/manual/paths/invoke?sig=SECRETSIG", "", "https://prod-01.westus.logic.azure.com/…"),
            ("googlechat", "https://chat.googleapis.com/v1/spaces/SECRETSPACE/messages?key=SECRETKEY&token=SECRETTOK", "", "https://chat.googleapis.com/…"),
            ("rocketchat", "https://chat.example.org/hooks/SECRETID/SECRETTOK", "", "https://chat.example.org/…"),
            ("synology", "https://nas.example.org:5001/webapi/entry.cgi?api=SYNO.Chat.External&token=%22SECRETTOK%22", "", "https://nas.example.org/…"),
            ("zulip", "https://zulip.example.org", "bot@example.org:SECRETAPIKEY123", "https://zulip.example.org"),
            ("nextcloud", "https://cloud.example.org", "wang:SECRETAPPPASS", "https://cloud.example.org"),
            ("smtp", "smtp://me@example.org@smtp.example.org:587", "SECRETPASSWORD", "smtp://smtp.example.org"),
            ("ifttt", "", "SECRETWEBHOOKKEY", ""),
        ];
        let secrets = [
            "SECRETFLOW", "SECRETSIG", "SECRETSPACE", "SECRETKEY", "SECRETTOK", "SECRETID",
            "SECRETAPIKEY123", "SECRETAPPPASS", "SECRETPASSWORD", "SECRETWEBHOOKKEY",
        ];
        for (kind, url, token, hint) in cases {
            let v = ch(kind, url, token, "").masked();
            let text = v.to_string();
            assert!(!secrets.iter().any(|s| text.contains(s)), "{kind}: {text}");
            assert_eq!(v["url_hint"], hint, "{kind}");
            assert_eq!((v["url"].as_str(), v["token"].as_str()), (Some(""), Some("")), "{kind}");
        }
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
