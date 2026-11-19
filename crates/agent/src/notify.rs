// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Notifications: incoming call, outgoing call, blocked call, voicemail and the post-call summary.
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
use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

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
    /// Slack: the app-level token chat commands connect with (Socket Mode).
    /// A secret like token: masked for the settings page, kept when it comes
    /// back empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub app_token: String,
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
    Kind { kind: "discord", label: "Discord", url: "Webhook URL", token: "Bot token (for chat commands)", target: "Channel ID (for chat commands)", url_default: "" },
    Kind { kind: "slack", label: "Slack / Mattermost", url: "Incoming webhook URL", token: "Bot token (for chat commands)", target: "Channel ID (for chat commands)", url_default: "" },
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

/// The fourth field, only Slack has it: the app-level token chat commands
/// connect with. Empty: not used.
pub fn app_token_label(kind: &str) -> &'static str {
    if kind == "slack" {
        "App-level token (for chat commands)"
    } else {
        ""
    }
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
            "app_token": "",
            "app_token_hint": mask_token(&self.app_token),
            "app_token_set": !self.app_token.trim().is_empty(),
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
        c.app_token = if app_token_label(&c.kind).is_empty() { String::new() } else { c.app_token.trim().to_string() };
        if let Some(old) = old {
            if c.url.is_empty() {
                c.url = old.url.clone();
            }
            if c.token.is_empty() {
                c.token = old.token.clone();
            }
            if c.app_token.is_empty() && !app_token_label(&c.kind).is_empty() {
                c.app_token = old.app_token.clone();
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

/// An interactive action button / signed one-tap link on a notification.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyAction {
    pub label: String,
    pub url: String,
}

/// What is sent.
#[derive(Debug, Clone, Default)]
pub struct Message {
    /// `incoming` / `outgoing` / `blocked` / `repeat_calls` / `voicemail` / `summary` / `test` / `sms`.
    pub event: String,
    pub title: String,
    /// Plain text; its first line is the headline.
    pub body: String,
    /// The same with Telegram's HTML tags.
    pub html: Option<String>,
    /// For webhooks: the facts as JSON.
    pub data: Value,
    /// Optional primary URL opened when tapping the notification.
    pub link: Option<String>,
    /// Optional one-tap action buttons / links.
    pub actions: Vec<NotifyAction>,
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

/// At most max bytes of UTF-8, cut between characters, an ellipsis marking a
/// cut (for services whose limit counts bytes: Chinese is three bytes a character).
fn clip_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max.saturating_sub('…'.len_utf8()).min(s.len());
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push('…');
    out
}

/// Discord's REST API.
const DISCORD_API: &str = "https://discord.com/api/v10";
const DISCORD_AGENT: &str = concat!("DiscordBot (bageo, ", env!("CARGO_PKG_VERSION"), ")");

fn link_ok(url: &str) -> bool {
    let u = url.trim();
    u.starts_with("https://") || u.starts_with("http://")
}

/// A message from the Discord bot itself (no webhook): to the channel in
/// target, with the notification's actions as link buttons (five at most).
fn discord_bot_prepared(api: &str, ch: &Channel, msg: &Message) -> Result<Prepared> {
    let token = need("token", "bot token", &ch.token)?;
    let channel = need("target", "channel ID", &ch.target)?;
    if !channel.chars().all(|c| c.is_ascii_digit()) {
        return Err(bad_value("target", "a Discord channel or user ID is made of digits"));
    }
    let mut payload = json!({ "content": clip(&msg.body, 1900), "allowed_mentions": { "parse": [] } });
    let buttons: Vec<Value> = msg
        .actions
        .iter()
        .filter(|a| link_ok(&a.url) && !a.label.trim().is_empty())
        .take(5)
        .map(|a| json!({ "type": 2, "style": 5, "label": clip(a.label.trim(), 80), "url": a.url.trim() }))
        .collect();
    if !buttons.is_empty() {
        payload["components"] = json!([{ "type": 1, "components": buttons }]);
    }
    Ok(Prepared {
        put: false,
        url: format!("{}/channels/{channel}/messages", api.trim_end_matches('/')),
        headers: vec![
            ("Authorization".into(), format!("Bot {token}")),
            ("User-Agent".into(), DISCORD_AGENT.into()),
        ],
        body: Body::Json(payload),
    })
}

/// A message from the Slack bot itself (no webhook): chat.postMessage to the
/// channel in target, with the notification's actions as link buttons.
fn slack_bot_prepared(ch: &Channel, msg: &Message) -> Result<Prepared> {
    let token = need("token", "bot token", &ch.token)?;
    let channel = need("target", "channel ID", &ch.target)?;
    let text = clip(&msg.body, 3000);
    let mut payload = json!({ "channel": channel, "text": text, "unfurl_links": false, "unfurl_media": false });
    let buttons: Vec<Value> = msg
        .actions
        .iter()
        .filter(|a| link_ok(&a.url) && !a.label.trim().is_empty())
        .take(25)
        .enumerate()
        .map(|(i, a)| {
            json!({ "type": "button", "action_id": format!("bageo_link_{i}"), "url": a.url.trim(),
                "text": { "type": "plain_text", "text": clip(a.label.trim(), 75), "emoji": false } })
        })
        .collect();
    let mut blocks = vec![json!({
        "type": "section",
        "text": { "type": "plain_text", "text": text, "emoji": false },
    })];
    if !buttons.is_empty() {
        blocks.push(json!({ "type": "actions", "elements": buttons }));
    }
    payload["blocks"] = Value::Array(blocks);
    Ok(Prepared {
        put: false,
        url: "https://slack.com/api/chat.postMessage".to_string(),
        headers: vec![("Authorization".into(), format!("Bearer {token}"))],
        body: Body::Json(payload),
    })
}

/// ntfy's server and topic. The server may be written with its topic
/// ("https://ntfy.sh/mytopic", as the app shows it): the topic is then the last
/// part of the address (or the same as the one in target) and the server
/// the address without it, since a message is published to the server's root.
fn ntfy_server_and_topic(base: &str, target: &str) -> Result<(String, String)> {
    let server = need_http("url", "server", base)?;
    let given = target.trim().trim_matches('/');
    let host_at = server.find("://").map(|i| i + 3).unwrap_or(0);
    if let Some(i) = server[host_at..].find('/') {
        let (head, path) = server.split_at(host_at + i);
        let cut = path.rfind('/').unwrap_or(0);
        let (dir, last) = (&path[..cut], &path[cut + 1..]);
        if !last.is_empty() && (given.is_empty() || last == given) {
            return Ok((format!("{head}{dir}"), last.to_string()));
        }
    }
    let topic = need("target", "topic", given)?;
    Ok((server, topic.to_string()))
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
fn teams_card(body: &str, actions: &[NotifyAction]) -> Value {
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
    let mut content = json!({
        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
        "type": "AdaptiveCard",
        "version": "1.4",
        "body": blocks,
    });
    if !actions.is_empty() {
        content["actions"] = json!(actions
            .iter()
            .map(|a| json!({ "type": "Action.OpenUrl", "title": a.label, "url": a.url }))
            .collect::<Vec<_>>());
    }
    json!({
        "type": "message",
        "attachments": [{
            "contentType": "application/vnd.microsoft.card.adaptive",
            "contentUrl": null,
            "content": content,
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
    let link = msg.link.as_deref().map(str::trim).filter(|s| !s.is_empty());
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
            if !msg.actions.is_empty() {
                let row: Vec<Value> = msg
                    .actions
                    .iter()
                    .map(|a| json!({ "text": a.label, "url": a.url }))
                    .collect();
                payload["reply_markup"] = json!({ "inline_keyboard": [row] });
            }
            post_json(format!("https://api.telegram.org/bot{token}/sendMessage"), payload)
        }
        // With a bot token and no webhook: the bot posts to the channel (and
        // can carry link buttons, which a webhook cannot).
        "discord" if base.is_empty() && !ch.token.trim().is_empty() => discord_bot_prepared(DISCORD_API, ch, msg)?,
        "discord" => post_json(
            need_http("url", "webhook URL", &base)?,
            json!({ "content": clip(body, 1900) }),
        ),
        "slack" if base.is_empty() && !ch.token.trim().is_empty() => slack_bot_prepared(ch, msg)?,
        "slack" => post_json(need_http("url", "webhook URL", &base)?, json!({ "text": clip(body, 3500) })),
        // Microsoft Learn, "Create incoming webhooks with Workflows for Microsoft
        // Teams": the "Post to a channel when a webhook request is received"
        // workflow takes a message whose attachment is an Adaptive Card. The old
        // Office 365 connectors (MessageCard) are retired.
        "teams" => post_json(need_http("url", "workflow webhook URL", &base)?, teams_card(body, &msg.actions)),
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
            let (server, topic) = ntfy_server_and_topic(&base, &ch.target)?;
            let mut payload = json!({
                "topic": topic,
                "title": clip(title, 200),
                "message": clip(body, 3500),
            });
            if let Some(u) = link {
                payload["click"] = json!(u);
            }
            if !msg.actions.is_empty() {
                payload["actions"] = json!(msg
                    .actions
                    .iter()
                    .take(3)
                    .map(|a| json!({ "action": "view", "label": a.label, "url": a.url }))
                    .collect::<Vec<_>>());
            }
            Prepared {
                put: false,
                url: server,
                headers,
                body: Body::Json(payload),
            }
        }
        "gotify" => {
            let mut payload = json!({ "title": clip(title, 200), "message": clip(body, 3500), "priority": 5 });
            if let Some(u) = link {
                payload["extras"] = json!({ "client::notification": { "click": { "url": u } } });
            }
            post_json(
                format!(
                    "{}/message?token={}",
                    need_http("url", "server URL", &base)?,
                    pct(need("token", "app token", &ch.token)?)
                ),
                payload,
            )
        }
        "bark" => {
            let mut payload = json!({
                "device_key": need("token", "device key", &ch.token)?,
                "title": clip(title, 200),
                // Apple's push service takes 4 KB in all: the text is cut by bytes.
                "body": clip_bytes(body, 2800),
            });
            if let Some(u) = link {
                payload["url"] = json!(u);
            }
            post_json(format!("{}/push", need_http("url", "server", &base)?), payload)
        }
        "pushover" => {
            let mut form = vec![
                ("token".into(), need("token", "API token", &ch.token)?.to_string()),
                ("user".into(), need("target", "user key", &ch.target)?.to_string()),
                ("title".into(), clip(title, 200)),
                // Pushover's limit is 1,024 bytes, not characters.
                ("message".into(), clip_bytes(body, 1024)),
            ];
            if let Some(u) = link {
                form.push(("url".into(), u.to_string()));
            }
            Prepared {
                put: false,
                url: "https://api.pushover.net/1/messages.json".to_string(),
                headers: Vec::new(),
                body: Body::Form(form),
            }
        }
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
            // WeCom takes at most 2,048 bytes of text.
            json!({ "msgtype": "text", "text": { "content": clip_bytes(body, 2048) } }),
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
        "chanify" => {
            let mut form = vec![("title".into(), clip(title, 100)), ("text".into(), clip(body, 3500))];
            if let Some(u) = link {
                form.push(("link".into(), u.to_string()));
            }
            Prepared {
                put: false,
                url: format!(
                    "{}/v1/sender/{}",
                    need_http("url", "server", &base)?,
                    pct(need("token", "token", &ch.token)?)
                ),
                headers: Vec::new(),
                body: Body::Form(form),
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
            let mut payload = json!({ "title": clip(title, 200), "message": clip(body, 1000) });
            if link.is_some() || !msg.actions.is_empty() {
                let mut data = serde_json::Map::new();
                if let Some(u) = link {
                    data.insert("url".into(), json!(u));
                    data.insert("clickAction".into(), json!(u));
                }
                if !msg.actions.is_empty() {
                    data.insert(
                        "actions".into(),
                        json!(msg
                            .actions
                            .iter()
                            .take(3)
                            .map(|a| json!({ "action": "URI", "title": a.label, "uri": a.url }))
                            .collect::<Vec<_>>()),
                    );
                }
                payload["data"] = Value::Object(data);
            }
            Prepared {
                put: false,
                url: format!("{}/api/services/notify/{service}", need_http("url", "server URL", &base)?),
                headers: vec![(
                    "Authorization".into(),
                    format!("Bearer {}", need("token", "long-lived access token", &ch.token)?),
                )],
                body: Body::Json(payload),
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
            let mut payload = json!({
                "event": msg.event,
                "title": title,
                "body": body,
                "text": body,
                "data": msg.data,
            });
            if let Some(u) = link {
                payload["link"] = json!(u);
            }
            if !msg.actions.is_empty() {
                payload["actions"] = json!(msg.actions);
            }
            Prepared {
                put: false,
                url: need_http("url", "URL", &base)?,
                headers,
                body: Body::Json(payload),
            }
        }
        "gmail" | "smtp" => bail!("{} is sent over SMTP", k.kind),
        other => {
            return Err(ChannelError::new("unknown_channel", "", format!("unknown channel kind {other:?}")).into())
        }
    })
}

/// The client notifications go out with. It follows a redirect only within the
/// same host: a service that answers with a redirect to another host must not
/// get the tokens in our headers (or the message).
fn notify_client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                let same = attempt.previous().last().map_or(true, |from| same_host_redirect(from, attempt.url()));
                if same && attempt.previous().len() < 5 {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }))
            .build()
            .unwrap_or_else(|_| Client::new())
    })
}

/// A redirect that stays on the same host (and port); an http address may
/// move to https on the standard port.
fn same_host_redirect(from: &url::Url, to: &url::Url) -> bool {
    let host = |u: &url::Url| u.host_str().map(str::to_ascii_lowercase);
    if host(from) != host(to) {
        return false;
    }
    if from.scheme() == to.scheme() {
        return from.port_or_known_default() == to.port_or_known_default();
    }
    from.scheme() == "http" && to.scheme() == "https" && from.port().is_none() && to.port().is_none()
}

async fn send_prepared(_http: &Client, p: &Prepared) -> Result<()> {
    let http = notify_client();
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

/// Errors worth a short retry when delivering urgent / non-Telegram alerts.
fn is_transient_send_error(e: &anyhow::Error) -> bool {
    if let Some(ce) = channel_error(e) {
        return ce.code == "network_error"
            || (ce.code == "http_error"
                && (ce.detail.starts_with("HTTP 429:")
                    || ce.detail.starts_with("HTTP 408:")
                    || ce.detail.starts_with("HTTP 5")));
    }
    if e.chain().any(|c| c.downcast_ref::<reqwest::Error>().is_some()) {
        return true;
    }
    let msg = e.to_string();
    msg.contains("HTTP 429") || msg.contains("HTTP 5")
}

/// Send `msg` to one channel.
pub async fn send_channel(http: &Client, ch: &Channel, msg: &Message) -> Result<()> {
    send_channel_inner(http, ch, msg).await.map_err(|e| scrub_error(e, ch))
}

async fn send_channel_inner(http: &Client, ch: &Channel, msg: &Message) -> Result<()> {
    // The settings page's test also looks up names and asks the service.
    let testing = msg.event == "test";
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
        refuse_metadata_host(&server.host, server.port, testing).await?;
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
    let matrix;
    let ch = if ch.kind == "matrix" {
        matrix = matrix_ready(ch, msg).await?;
        &matrix
    } else {
        ch
    };
    if ch.kind == "discord" && ch.url.trim().is_empty() && !ch.token.trim().is_empty() {
        let sent = send_discord_bot(http, DISCORD_API, ch, msg).await;
        return match sent {
            Ok(()) => discord_gave_up(ch, testing),
            err => err,
        };
    }
    let prepared = prepare(ch, msg, now_ms)?;
    refuse_metadata(&prepared.url, testing).await?;
    let sent = match send_prepared(http, &prepared).await {
        // Telegram refuses HTML it cannot parse, and buttons whose address it will not
        // open (a `.local` name): send the plain text, then the plain text without the
        // buttons (their links go into the text).
        Err(e) if ch.kind == "telegram" && (msg.html.is_some() || !msg.actions.is_empty()) => {
            warn!("notify telegram message refused ({e:#}); retrying as plain text");
            let mut result = Err(e);
            if msg.html.is_some() {
                let plain = Message { html: None, ..msg.clone() };
                result = send_prepared(http, &prepare(ch, &plain, now_ms)?).await;
            }
            if result.is_err() && !msg.actions.is_empty() {
                result = send_prepared(http, &prepare(ch, &telegram_without_buttons(msg), now_ms)?).await;
            }
            result
        }
        other => other,
    };
    match sent {
        Ok(()) if ch.kind == "discord" => discord_gave_up(ch, testing),
        other => other,
    }
}

/// The message as plain text with no buttons: each button's link follows the text.
fn telegram_without_buttons(msg: &Message) -> Message {
    let mut body = msg.body.clone();
    for a in &msg.actions {
        body.push_str(&format!("\n{}: {}", a.label, a.url));
    }
    Message { html: None, body, actions: Vec::new(), ..msg.clone() }
}

/// A text in the owner's language: zh-TW, zh-CN, en, ja, ko, es, fr, de, pt.
fn pick(t: [&str; 9]) -> String {
    let i = match locale::notify_lang() {
        locale::Lang::ZhTw => 0,
        locale::Lang::ZhCn => 1,
        locale::Lang::En => 2,
        locale::Lang::Ja => 3,
        locale::Lang::Ko => 4,
        locale::Lang::Es => 5,
        locale::Lang::Fr => 6,
        locale::Lang::De => 7,
        locale::Lang::Pt => 8,
    };
    t[i].to_string()
}

/// What a test tells the owner when the Discord bot was turned away.
fn bot_problem_text(code: &str) -> String {
    match code {
        "discord_message_content" => pick([
            "Discord 不讓 Bageo 機器人讀取訊息。請到 Discord 開發者後台，打開這個機器人的「Message Content Intent」，再把這個管道關掉後重新打開。",
            "Discord 不让 Bageo 机器人读取消息。请到 Discord 开发者后台，打开这个机器人的“Message Content Intent”，再把这个渠道关掉后重新打开。",
            "Discord is not letting the Bageo bot read messages. In the Discord Developer Portal, turn on \"Message Content Intent\" for your bot, then switch this channel off and on again.",
            "Discord が Bageo のボットにメッセージの読み取りを許可していません。Discord Developer Portal でこのボットの「Message Content Intent」をオンにし、このチャンネルを一度オフにしてからオンに戻してください。",
            "Discord가 Bageo 봇의 메시지 읽기를 허용하지 않습니다. Discord 개발자 포털에서 이 봇의 'Message Content Intent'를 켠 뒤, 이 채널을 껐다가 다시 켜 주세요.",
            "Discord no deja que el bot de Bageo lea mensajes. En el Discord Developer Portal activa «Message Content Intent» de tu bot y luego apaga y vuelve a encender este canal.",
            "Discord ne laisse pas le bot Bageo lire les messages. Dans le Discord Developer Portal, activez « Message Content Intent » pour votre bot, puis désactivez et réactivez ce canal.",
            "Discord erlaubt dem Bageo-Bot nicht, Nachrichten zu lesen. Aktiviere im Discord Developer Portal für deinen Bot „Message Content Intent“ und schalte diesen Kanal dann aus und wieder ein.",
            "O Discord não deixa o bot do Bageo ler mensagens. No Discord Developer Portal, ative «Message Content Intent» do seu bot e depois desligue e ligue este canal novamente.",
        ]),
        "discord_bad_token" => pick([
            "Discord 不接受這個 Bot Token。請回 Discord 開發者後台重新複製，貼到這裡再儲存。",
            "Discord 不接受这个 Bot Token。请回 Discord 开发者后台重新复制，粘贴到这里再保存。",
            "Discord does not accept this bot token. Copy it again from the Discord Developer Portal and save it here.",
            "Discord がこの Bot Token を受け付けません。Discord Developer Portal でもう一度コピーし、ここに貼り付けて保存してください。",
            "Discord가 이 Bot Token을 받아들이지 않습니다. Discord 개발자 포털에서 다시 복사해 여기에 붙여넣고 저장해 주세요.",
            "Discord no acepta este Bot Token. Cópialo de nuevo desde el Discord Developer Portal y guárdalo aquí.",
            "Discord n'accepte pas ce Bot Token. Copiez-le à nouveau depuis le Discord Developer Portal et enregistrez-le ici.",
            "Discord akzeptiert diesen Bot Token nicht. Kopiere ihn erneut aus dem Discord Developer Portal und speichere ihn hier.",
            "O Discord não aceita este Bot Token. Copie-o novamente no Discord Developer Portal e salve aqui.",
        ]),
        _ => pick([
            "Discord 不讓 Bageo 機器人連上。請檢查 Discord 開發者後台裡這個機器人的設定，再把這個管道關掉後重新打開。",
            "Discord 不让 Bageo 机器人连上。请检查 Discord 开发者后台里这个机器人的设置，再把这个渠道关掉后重新打开。",
            "Discord turned the Bageo bot away. Check the bot's settings in the Discord Developer Portal, then switch this channel off and on again.",
            "Discord が Bageo のボットの接続を断りました。Discord Developer Portal でボットの設定を確認し、このチャンネルを一度オフにしてからオンに戻してください。",
            "Discord가 Bageo 봇의 연결을 거부했습니다. Discord 개발자 포털에서 봇 설정을 확인한 뒤, 이 채널을 껐다가 다시 켜 주세요.",
            "Discord rechazó al bot de Bageo. Revisa los ajustes del bot en el Discord Developer Portal y luego apaga y vuelve a encender este canal.",
            "Discord a refusé le bot Bageo. Vérifiez les réglages du bot dans le Discord Developer Portal, puis désactivez et réactivez ce canal.",
            "Discord hat den Bageo-Bot abgewiesen. Prüfe die Einstellungen des Bots im Discord Developer Portal und schalte diesen Kanal dann aus und wieder ein.",
            "O Discord recusou o bot do Bageo. Verifique as configurações do bot no Discord Developer Portal e depois desligue e ligue este canal novamente.",
        ]),
    }
}

/// A test of a Discord channel whose bot cannot connect for commands (see
/// [crate::control::link::gave_up]) says why instead of reporting success.
fn discord_gave_up(ch: &Channel, testing: bool) -> Result<()> {
    if testing && ch.control && !ch.token.trim().is_empty() {
        if let Some(code) = crate::control::link::gave_up("discord", &ch.token) {
            return Err(ChannelError::new("bot_problem", "token", bot_problem_text(code)).into());
        }
    }
    Ok(())
}

/// A Discord bot's message. The target may be a channel or, when no such
/// channel exists, a person's ID: the bot then opens the private chat with
/// that person and sends it there.
pub(crate) async fn send_discord_bot(http: &Client, api: &str, ch: &Channel, msg: &Message) -> Result<()> {
    let first = discord_bot_prepared(api, ch, msg)?;
    match send_prepared(http, &first).await {
        Err(e) if channel_error(&e).is_some_and(|c| c.code == "service_not_found" || c.detail.contains("10003")) => {
            let dm = match discord_dm_channel(api, &ch.token, ch.target.trim()).await {
                Ok(id) => id,
                Err(_) => return Err(e),
            };
            let second = Prepared { url: format!("{}/channels/{dm}/messages", api.trim_end_matches('/')), ..first };
            send_prepared(http, &second).await
        }
        other => other,
    }
}

/// The private chat of the bot with one person (Discord opens it if it is not there yet).
async fn discord_dm_channel(api: &str, token: &str, user: &str) -> Result<String> {
    let resp = notify_client()
        .post(format!("{}/users/@me/channels", api.trim_end_matches('/')))
        .header("Authorization", format!("Bot {}", token.trim()))
        .header("User-Agent", DISCORD_AGENT)
        .json(&json!({ "recipient_id": user }))
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| ChannelError::new("network_error", "", format!("request failed: {}", e.without_url())))?;
    let status = resp.status();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(ChannelError::new("http_error", "", format!("HTTP {}: no private chat with that ID", status.as_u16())).into());
    }
    match v.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()) {
        Some(id) => Ok(id.to_string()),
        None => Err(ChannelError::new("service_error", "", "Discord gave no private chat").into()),
    }
}

/// What a test says about a Matrix room the bot cannot read.
fn matrix_encrypted_text() -> String {
    pick([
        "這個聊天室有加密，Bageo 機器人讀不到加密的聊天室。請建立一個沒有加密的新聊天室，改用那一間。",
        "这个聊天室开启了加密，Bageo 机器人读不到加密的聊天室。请新建一个没有加密的聊天室，改用那一间。",
        "This room is encrypted, and the Bageo bot cannot read encrypted rooms. Create a new room with encryption turned off and use that one.",
        "このルームは暗号化されていて、Bageo のボットは暗号化されたルームを読めません。暗号化なしの新しいルームを作って、そちらを使ってください。",
        "이 방은 암호화되어 있어 Bageo 봇이 읽을 수 없습니다. 암호화를 끈 새 방을 만들어 그 방을 사용해 주세요.",
        "Esta sala está cifrada y el bot de Bageo no puede leer salas cifradas. Crea una sala nueva sin cifrado y usa esa.",
        "Ce salon est chiffré et le bot Bageo ne peut pas lire les salons chiffrés. Créez un nouveau salon sans chiffrement et utilisez-le.",
        "Dieser Raum ist verschlüsselt, und der Bageo-Bot kann verschlüsselte Räume nicht lesen. Erstelle einen neuen Raum ohne Verschlüsselung und nutze ihn.",
        "Esta sala é criptografada e o bot do Bageo não consegue ler salas criptografadas. Crie uma sala nova sem criptografia e use essa.",
    ])
}

/// A Matrix channel ready to send: a room written as an alias (#room:server)
/// is looked up and replaced by its room ID, which is what sending needs. In
/// the settings page's test, a room with end-to-end encryption is refused:
/// the bot cannot read it, so commands would never arrive. Anything missing
/// or wrong is left for [prepare] to name.
async fn matrix_ready(ch: &Channel, msg: &Message) -> Result<Channel> {
    let mut out = ch.clone();
    let (base, token, target) = (ch.url_or_default(), ch.token.trim(), ch.target.trim());
    if token.is_empty() || target.is_empty() || !link_ok(&base) {
        return Ok(out);
    }
    let testing = msg.event == "test";
    refuse_metadata(&base, testing).await?;
    let client = notify_client();
    let ask = |url: String| client.get(url).bearer_auth(token).timeout(std::time::Duration::from_secs(15)).send();
    if target.starts_with('#') {
        let url = format!("{base}/_matrix/client/v3/directory/room/{}", pct(target));
        let resp = ask(url)
            .await
            .map_err(|e| ChannelError::new("network_error", "", format!("request failed: {}", e.without_url())))?;
        let status = resp.status();
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(bad_value("target", format!("no room for that alias (HTTP {})", status.as_u16())));
        }
        match v.get("room_id").and_then(Value::as_str).filter(|id| !id.is_empty()) {
            Some(id) => out.target = id.to_string(),
            None => return Err(bad_value("target", "no room for that alias")),
        }
    }
    if testing {
        let url = format!("{base}/_matrix/client/v3/rooms/{}/state/m.room.encryption", pct(&out.target));
        if let Ok(resp) = ask(url).await {
            if resp.status().is_success() {
                let v: Value = resp.json().await.unwrap_or(Value::Null);
                if v.get("algorithm").and_then(Value::as_str).is_some_and(|a| !a.is_empty()) {
                    return Err(ChannelError::new("room_encrypted", "target", matrix_encrypted_text()).into());
                }
            }
        }
    }
    Ok(out)
}

/// What a test says about an address that leads to a cloud server's private
/// information service.
fn blocked_address() -> anyhow::Error {
    ChannelError::new(
        "blocked_address",
        "url",
        pick([
            "這個位址是雲端主機的內部資訊位址，Bageo 不會送到這裡。請填通知服務自己的位址。",
            "这个地址是云主机的内部信息地址，Bageo 不会发送到这里。请填通知服务自己的地址。",
            "Bageo will not send to this address because it is a cloud server's private information address. Enter your notification service's address instead.",
            "このアドレスはクラウドサーバーの内部情報用なので、Bageo は送信しません。通知サービス自身のアドレスを入力してください。",
            "이 주소는 클라우드 서버의 내부 정보용 주소라서 Bageo가 보내지 않습니다. 알림 서비스의 주소를 입력해 주세요.",
            "Bageo no enviará a esta dirección porque es la dirección de información interna de un servidor en la nube. Escribe la dirección de tu servicio de notificaciones.",
            "Bageo n'enverra rien à cette adresse, car c'est l'adresse d'information interne d'un serveur cloud. Saisissez l'adresse de votre service de notification.",
            "Bageo sendet nicht an diese Adresse, denn sie ist die interne Infoadresse eines Cloud-Servers. Gib die Adresse deines Benachrichtigungsdienstes ein.",
            "O Bageo não enviará para este endereço, pois ele é o endereço de informações internas de um servidor na nuvem. Digite o endereço do seu serviço de notificação.",
        ]),
    )
    .into()
}

/// An address of a cloud server's metadata service: 169.254.0.0/16 (AWS,
/// Google, Azure and others), Alibaba's 100.100.100.200 and AWS's IPv6 one.
/// Addresses on this network or this computer (a self-hosted ntfy or
/// Matrix) are not among them.
fn is_metadata_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local() || v4 == Ipv4Addr::new(100, 100, 100, 200),
        IpAddr::V6(v6) => {
            v6.segments() == [0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254] || v6.to_ipv4_mapped().is_some_and(|m| is_metadata_ip(IpAddr::V4(m)))
        }
    }
}

/// Refuse an address that leads to a metadata service. A name is also
/// looked up when resolve is set (the settings page's test), so that a name
/// pointing there is caught too.
async fn refuse_metadata(url: &str, resolve: bool) -> Result<()> {
    let Ok(u) = url::Url::parse(url.trim()) else {
        return Ok(());
    };
    let port = u.port_or_known_default().unwrap_or(443);
    match u.host() {
        Some(url::Host::Ipv4(ip)) if is_metadata_ip(IpAddr::V4(ip)) => Err(blocked_address()),
        Some(url::Host::Ipv6(ip)) if is_metadata_ip(IpAddr::V6(ip)) => Err(blocked_address()),
        Some(url::Host::Domain(d)) => refuse_metadata_host(d, port, resolve).await,
        _ => Ok(()),
    }
}

async fn refuse_metadata_host(host: &str, port: u16, resolve: bool) -> Result<()> {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if h.is_empty() {
        return Ok(());
    }
    if let Ok(ip) = h.trim_matches(['[', ']']).parse::<IpAddr>() {
        return if is_metadata_ip(ip) { Err(blocked_address()) } else { Ok(()) };
    }
    if matches!(h.as_str(), "metadata.google.internal" | "metadata" | "instance-data" | "instance-data.ec2.internal") {
        return Err(blocked_address());
    }
    if resolve {
        let lookup = tokio::net::lookup_host((h.as_str(), port));
        if let Ok(Ok(addrs)) = tokio::time::timeout(std::time::Duration::from_secs(5), lookup).await {
            if addrs.into_iter().any(|a| is_metadata_ip(a.ip())) {
                return Err(blocked_address());
            }
        }
    }
    Ok(())
}

fn add_secret(out: &mut Vec<String>, s: &str) {
    let s = s.trim();
    if s.len() < 4 || out.iter().any(|o| o == s) {
        return;
    }
    out.push(s.to_string());
    let encoded = pct(s);
    if encoded != s {
        out.push(encoded);
    }
}

/// What in a channel's settings is secret: its token and app token (also
/// the parts of `name:secret`) and the address (webhook addresses carry
/// their secret in the path or the query).
fn secrets_of(ch: &Channel) -> Vec<String> {
    let mut out = Vec::new();
    for t in [ch.token.as_str(), ch.app_token.as_str()] {
        add_secret(&mut out, t);
        if t.contains(':') {
            for part in t.split(':') {
                add_secret(&mut out, part);
            }
        }
    }
    add_secret(&mut out, &ch.url);
    if let Ok(u) = url::Url::parse(ch.url.trim()) {
        for seg in u.path_segments().into_iter().flatten().filter(|s| s.len() >= 12) {
            add_secret(&mut out, seg);
        }
        for (_, v) in u.query_pairs().filter(|(_, v)| v.len() >= 8) {
            add_secret(&mut out, &v);
        }
        if let Some(p) = u.password() {
            add_secret(&mut out, p);
        }
    }
    out
}

/// The text without any of the secrets (the longest first).
fn redact(text: &str, secrets: &[String]) -> String {
    let mut order: Vec<&String> = secrets.iter().collect();
    order.sort_by_key(|s| std::cmp::Reverse(s.len()));
    let mut out = text.to_string();
    for s in order {
        out = out.replace(s.as_str(), "***");
    }
    out
}

/// An error whose details (the other side's answer is in them) carry none
/// of the channel's secrets: the settings page shows them.
fn scrub_error(e: anyhow::Error, ch: &Channel) -> anyhow::Error {
    let secrets = secrets_of(ch);
    if secrets.is_empty() {
        return e;
    }
    match channel_error(&e) {
        Some(c) => ChannelError::new(c.code, c.field, redact(&c.detail, &secrets)).into(),
        None => anyhow::anyhow!("{}", redact(&format!("{e:#}"), &secrets)),
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

/// Remove one phone's saved label override from the in-memory table.
pub fn forget_device_override(serial: &str) -> bool {
    let s = serial.trim();
    if s.is_empty() {
        return false;
    }
    DEVICE_OVERRIDES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .is_some_and(|m| m.remove(s).is_some())
}

/// Forget an offline phone's remembered model name and label override.
pub fn forget_device(serial: &str) -> bool {
    let s = serial.trim();
    if s.is_empty() {
        return false;
    }
    let a = DEVICES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .is_some_and(|m| m.remove(s).is_some());
    let b = forget_device_override(s);
    a || b
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
    call_detail_url_at(&crate::settings_store::effective_console_url(settings), call_id)
}

/// [`call_detail_url`] for the console at `console`.
fn call_detail_url_at(console: &str, call_id: &str) -> String {
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
    headline: String,
    rows: Vec<(String, String, bool)>,
    tail: String,
}

impl Card {
    fn new(headline: &str) -> Self {
        Self { headline: headline.to_string(), rows: Vec::new(), tail: String::new() }
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
        let mut s = self.headline.clone();
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
        let mut s = format!("<b>{}</b>", esc_html(&self.headline));
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
            link: None,
            actions: Vec::new(),
        }
    }
}

/// The card of a call that rings on a phone: number, phone and time. `event`
/// is `incoming` (a call came in) or `outgoing` (Bageo placed it itself).
fn ring_message(event: &str, title: &str, caller_id: &str, serial: &str) -> Message {
    let p = locale::notify_phrases();
    let caller = display_or_hidden(caller_id);
    let device = device_label(serial);
    let when = now_stamp();
    Card::new(title)
        .code(p.n_number, &caller)
        .row(p.n_phone, &device)
        .row(p.n_time, &when)
        .message(
            event,
            format!("{title} {caller}"),
            json!({ "caller_id": caller_id.trim(), "device": device, "time": when }),
        )
}

/// How long one number's call notifications are counted together.
const REPEAT_WINDOW: Duration = Duration::from_secs(60);
/// Notifications of one kind for one number allowed per [`REPEAT_WINDOW`].
const REPEAT_MAX_SENT: usize = 2;
/// Most numbers the limiter remembers at once.
const REPEAT_MAX_NUMBERS: usize = 1000;

/// What the limiter says about one more notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admit {
    /// Send it.
    Send,
    /// Too many already. `first` = this is the first one held back since the
    /// last count was handed out, so the caller must arrange the one summary.
    Skip { first: bool },
}

#[derive(Default)]
struct RepeatEntry {
    /// When the notifications still inside the window went out.
    sent: Vec<Instant>,
    /// Held back and not yet reported.
    skipped: u32,
    last_seen: Option<Instant>,
}

/// Lets a few call notifications per number through each minute and counts
/// the rest. Time is passed in so tests do not have to wait.
struct RepeatLimiter {
    entries: HashMap<String, RepeatEntry>,
    max_numbers: usize,
}

impl RepeatLimiter {
    fn new(max_numbers: usize) -> Self {
        Self { entries: HashMap::new(), max_numbers }
    }

    /// `key` = notification kind and the digits of the number.
    fn admit(&mut self, key: &str, now: Instant) -> Admit {
        self.prune(now);
        if !self.entries.contains_key(key) && self.entries.len() >= self.max_numbers.max(1) {
            // Full: drop the entry seen longest ago. One still waiting to be
            // reported is never dropped; then the newcomer simply goes out.
            let victim = self
                .entries
                .iter()
                .min_by_key(|(_, e)| (e.skipped > 0, e.last_seen))
                .map(|(k, e)| (k.clone(), e.skipped > 0));
            match victim {
                Some((_, true)) => return Admit::Send,
                Some((k, false)) => {
                    self.entries.remove(&k);
                }
                None => {}
            }
        }
        let entry = self.entries.entry(key.to_string()).or_default();
        entry.last_seen = Some(now);
        entry.sent.retain(|t| now.saturating_duration_since(*t) < REPEAT_WINDOW);
        if entry.sent.len() < REPEAT_MAX_SENT {
            entry.sent.push(now);
            Admit::Send
        } else {
            entry.skipped += 1;
            Admit::Skip { first: entry.skipped == 1 }
        }
    }

    /// The held-back count, handed out once: it starts again from zero.
    fn take_skipped(&mut self, key: &str) -> u32 {
        self.entries.get_mut(key).map(|e| std::mem::take(&mut e.skipped)).unwrap_or(0)
    }

    /// Forget numbers with nothing in the window and nothing to report.
    fn prune(&mut self, now: Instant) {
        self.entries.retain(|_, e| {
            e.skipped > 0 || e.sent.iter().any(|t| now.saturating_duration_since(*t) < REPEAT_WINDOW)
        });
    }
}

fn repeat_limiter() -> &'static Mutex<RepeatLimiter> {
    static LIMITER: OnceLock<Mutex<RepeatLimiter>> = OnceLock::new();
    LIMITER.get_or_init(|| Mutex::new(RepeatLimiter::new(REPEAT_MAX_NUMBERS)))
}

/// The summary of calls that were not notified one by one.
fn repeat_calls_message(p: &locale::Phrases, kind: &str, caller_id: &str, count: u32) -> Message {
    let caller = display_or_hidden(caller_id);
    let body = p.n_repeat_body.replace("{number}", &caller).replace("{count}", &count.to_string());
    Card::new(p.n_repeat_title).tail(&body).message(
        "repeat_calls",
        format!("{} {caller}", p.n_repeat_title),
        json!({ "caller_id": caller_id.trim(), "count": count, "kind": kind }),
    )
}

/// Sends a call notification, unless this number already had two of this kind
/// in the last minute. Held-back ones are counted and the owner gets one short
/// message about them a minute after the first. A number without digits cannot
/// be told apart from another caller, so it is never held back. Returns true
/// for a held-back one too: it was handled on purpose, not lost.
async fn deliver_limited(
    http: &Client,
    settings: &Settings,
    kind: &'static str,
    caller_id: &str,
    msg: &Message,
) -> bool {
    let digits: String = caller_id.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return deliver(http, settings, msg).await;
    }
    let key = format!("{kind}:{digits}");
    let verdict = repeat_limiter()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .admit(&key, Instant::now());
    match verdict {
        Admit::Send => deliver(http, settings, msg).await,
        Admit::Skip { first } => {
            info!(
                "notify held back kind={kind} number={}: more than {REPEAT_MAX_SENT} in a minute",
                crate::urgent_call::masked(caller_id)
            );
            if first {
                let http = http.clone();
                let settings = settings.clone();
                let caller_id = caller_id.trim().to_string();
                tokio::spawn(async move {
                    tokio::time::sleep(REPEAT_WINDOW).await;
                    let count = repeat_limiter()
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .take_skipped(&key);
                    if count > 0 {
                        let msg = repeat_calls_message(locale::notify_phrases(), kind, &caller_id, count);
                        deliver(&http, &settings, &msg).await;
                    }
                });
            }
            true
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
    let msg = ring_message("incoming", locale::notify_phrases().n_incoming, caller_id, serial);
    deliver_limited(http, settings, "incoming", caller_id, &msg).await
}

/// Bageo's own call rings on the other side (an AI errand, an appointment's
/// call-back, a to-do's call-back): the owner is told it is going out, not that
/// a call came in. Same switch as [`notify_incoming_call`]. `caller_id` is
/// the number dialed.
pub async fn notify_outgoing_call(
    http: &Client,
    settings: &Settings,
    caller_id: &str,
    serial: &str,
) -> bool {
    if !settings.call_notify {
        return false;
    }
    let msg = ring_message("outgoing", locale::notify_phrases().n_outgoing, caller_id, serial);
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
    let mut card = Card::new(p.n_blocked).code(p.n_number, &caller);
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
    deliver_limited(http, settings, "blocked", caller_id, &msg).await
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
    let msg = Card::new(&headline).row(p.n_time, &when).message(
        "call_waiting",
        headline.clone(),
        json!({
            "caller_id": caller_id.trim(),
            "device": device,
            "time": when,
            "sim_country": sim_country.unwrap_or(""),
        }),
    );
    deliver_limited(http, settings, "call_waiting", caller_id, &msg).await
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
    let mut msg = Card::new(&headline)
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
    let console = crate::settings_store::effective_console_url(settings);
    if !console.trim().is_empty() && caller_id.chars().filter(char::is_ascii_digit).count() >= 3 {
        let name_hint = contact.as_deref().unwrap_or("");
        let (lbl_blk, lbl_stl, lbl_wht) = match locale::notify_lang() {
            locale::Lang::ZhTw => ("加入黑名單", "設為陪聊", "加入白名單"),
            locale::Lang::ZhCn => ("加入黑名单", "设为陪聊", "加入白名单"),
            locale::Lang::Ja => ("着信拒否", "引き延ばし", "ホワイトリスト"),
            locale::Lang::Ko => ("차단 목록", "시간 끌기", "화이트리스트"),
            locale::Lang::Es => ("Lista negra", "Entretener", "Lista blanca"),
            locale::Lang::Fr => ("Liste noire", "Les faire parler", "Liste blanche"),
            locale::Lang::De => ("Sperrliste", "Hinhalten", "Whitelist"),
            locale::Lang::Pt => ("Bloquear", "Manter falando", "Lista branca"),
            locale::Lang::En => ("Block", "Stall", "Whitelist"),
        };
        for (lbl, act) in [(lbl_blk, "block"), (lbl_stl, "stall"), (lbl_wht, "white")] {
            let u = crate::calendar::quick_action_url(&console, act, caller_id.trim(), name_hint);
            if !u.is_empty() {
                msg.actions.push(NotifyAction { label: lbl.to_string(), url: u });
            }
        }
    }
    deliver(http, settings, &msg).await
}

/// Send a simple text alert (e.g. Whitelist Direct Transfer or Whitelist Emergency Alert)
/// to all enabled notification channels.
pub async fn send_text_alert(
    http: &Client,
    settings: &Settings,
    event: &str,
    title: &str,
    body: &str,
) -> bool {
    if !settings.call_notify {
        return false;
    }
    let msg = Message {
        event: event.to_string(),
        title: locale::for_owner(&sanitize_notify_text(title, 200)),
        body: locale::for_owner(&sanitize_notify_text(body, 3500)),
        html: None,
        data: json!({}),
        link: None,
        actions: Vec::new(),
    };
    deliver(http, settings, &msg).await
}

/// Send a pending appointment card with one-tap Accept / Decline buttons/links
/// to all enabled non-Telegram notification channels (Telegram uses its own
/// interactive inline callback card via `tg_inbound::send_task_card_labeled`).
pub async fn notify_pending_appointment_non_tg(
    http: &Client,
    rt: &crate::settings_store::RuntimeSettings,
    task_id: &str,
    caller_id: &str,
    title: &str,
    body: &str,
    link: &str,
) -> bool {
    if !rt.call_notify {
        return false;
    }
    let mut non_tg: Vec<Channel> = rt
        .notify_channels
        .iter()
        .filter(|c| c.enabled && c.kind != "telegram")
        .cloned()
        .collect();
    let gateway = rt.notify_gateway_url.trim();
    if !gateway.is_empty() {
        let tag = match rt.notify_gateway_tag.trim().trim_matches('/') {
            "" => "normal",
            t => t,
        };
        non_tg.push(Channel {
            id: "env-gateway".to_string(),
            kind: "apprise".to_string(),
            name: "Apprise gateway (env)".to_string(),
            enabled: true,
            url: format!("{}/notify/{tag}", gateway.trim_end_matches('/')),
            ..Channel::default()
        });
    }
    if non_tg.is_empty() {
        return false;
    }
    let p = locale::notify_phrases();
    let console = card_console_url(rt, &Settings::from_env());
    let confirm_url = crate::calendar::quick_action_url(&console, "confirm_task", task_id, "");
    let ignore_url = crate::calendar::quick_action_url(&console, "ignore_task", task_id, "");
    let mut actions = Vec::new();
    let mut full_body = format!("{title}\n{body}");
    if !confirm_url.is_empty() && !ignore_url.is_empty() {
        actions.push(NotifyAction { label: p.ap_confirm.to_string(), url: confirm_url.clone() });
        actions.push(NotifyAction { label: p.ap_decline.to_string(), url: ignore_url.clone() });
        full_body.push_str(&format!("\n\n{}: {confirm_url}\n{}: {ignore_url}", p.ap_confirm, p.ap_decline));
    }
    let msg = Message {
        event: "appointment_pending".to_string(),
        title: locale::for_owner(&sanitize_notify_text(title, 200)),
        body: locale::for_owner(&sanitize_notify_text(&full_body, 3500)),
        html: None,
        data: json!({
            "task_id": task_id,
            "caller_id": caller_id.trim(),
            "confirm_url": confirm_url,
            "ignore_url": ignore_url,
        }),
        link: (!link.trim().is_empty()).then(|| link.trim().to_string()),
        actions,
    };
    let sends = non_tg.iter().map(|ch| {
        let msg = &msg;
        async move {
            let delays = [std::time::Duration::from_millis(1500), std::time::Duration::from_millis(3000)];
            let mut attempt = 0usize;
            loop {
                match send_channel(http, ch, msg).await {
                    Ok(()) => return (ch, Ok(())),
                    Err(e) if attempt < delays.len() && is_transient_send_error(&e) => {
                        warn!(channel = %ch.name, kind = %ch.kind, attempt = attempt + 1, "notify pending appointment transient failure, retrying: {e:#}");
                        tokio::time::sleep(delays[attempt]).await;
                        attempt += 1;
                    }
                    Err(e) => return (ch, Err(e)),
                }
            }
        }
    });
    let mut any = false;
    for (ch, result) in futures_util::future::join_all(sends).await {
        match result {
            Ok(()) => {
                any = true;
                info!("notify ok kind={} name={:?} event={}", ch.kind, ch.name, msg.event);
            }
            Err(e) => warn!("notify failed kind={} name={:?} event={}: {e:#}", ch.kind, ch.name, msg.event),
        }
    }
    any
}

/// The console's address for the links on a card: the one typed in the
/// settings, else the one found (the computer's name or LAN address); empty
/// when only this computer can open the console. base: the settings from the environment.
fn card_console_url(rt: &crate::settings_store::RuntimeSettings, base: &Settings) -> String {
    crate::settings_store::effective_console_url(&rt.effective(base))
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
    let msg = Card::new(p.n_voicemail)
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
    Card::new(headline)
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
    Card::new(headline)
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
        link: None,
        actions: Vec::new(),
    };
    deliver(http, settings, &msg).await
}

/// The test message of the settings page.
pub fn test_message() -> Message {
    let p = locale::notify_phrases();
    Card::new(p.n_test_title)
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
            body: "Call summary\nNumber: 0912345678".into(),
            html: Some("<b>Call summary</b>\nNumber: <code>0912345678</code>".into()),
            data: json!({ "caller_id": "0912345678" }),
            link: None,
            actions: Vec::new(),
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
        assert!(m.body.starts_with("AI 回電：0912000000\n"), "{}", m.body);
        assert!(m.body.ends_with("AI 回電了，對方沒接") && m.html.unwrap().starts_with("<b>AI 回電：0912000000</b>"));
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
        assert!(json_body(&p)["text"].as_str().unwrap().starts_with("Call summary"));
    }

    #[test]
    fn telegram_retry_without_buttons_keeps_the_links_in_the_text() {
        let c = ch("telegram", "", "123:ABC", "-100200");
        let with_buttons = Message {
            actions: vec![
                NotifyAction { label: "Confirm".into(), url: "http://bageo.local/a".into() },
                NotifyAction { label: "確認".into(), url: "http://bageo.local/b".into() },
            ],
            ..msg()
        };
        let p = prepare(&c, &with_buttons, 0).unwrap();
        assert!(json_body(&p).get("reply_markup").is_some());
        let retry = telegram_without_buttons(&with_buttons);
        assert!(retry.html.is_none() && retry.actions.is_empty());
        let p = prepare(&c, &retry, 0).unwrap();
        assert!(json_body(&p).get("reply_markup").is_none());
        assert!(json_body(&p).get("parse_mode").is_none());
        let text = json_body(&p)["text"].as_str().unwrap();
        assert!(text.starts_with("Call summary"), "{text}");
        assert!(text.contains("Confirm: http://bageo.local/a") && text.contains("確認: http://bageo.local/b"), "{text}");
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
        let m = Message { body: "Call summary\nNumber: 0912345678\n\nCaller: 王小明".into(), ..msg() };
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
        assert_eq!(texts, ["Call summary", "Number: 0912345678", "Caller: 王小明"]);
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
    fn byte_limited_services_are_cut_by_bytes_between_characters() {
        let long = Message { title: "標".repeat(500), body: "字".repeat(9000), html: None, ..msg() };
        let p = prepare(&ch("wecom", "https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=k", "", ""), &long, 0).unwrap();
        let c = field(&p, "/text/content");
        assert!(c.len() <= 2048 && c.ends_with('…') && c.len() > 2000, "{}", c.len());
        let p = prepare(&ch("pushover", "", "apptoken", "userkey"), &long, 0).unwrap();
        let m = field(&p, "message");
        assert!(m.len() <= 1024 && m.ends_with('…') && m.len() > 1000, "{}", m.len());
        let p = prepare(&ch("bark", "", "devkey", ""), &long, 0).unwrap();
        assert!(field(&p, "/body").len() <= 3000 && field(&p, "/body").ends_with('…'));
        assert!(json_body(&p).to_string().len() < 4096, "Apple's push service takes 4 KB in all");
        // Short texts stay whole; a cut never falls inside a character.
        assert_eq!(clip_bytes("字字字", 9), "字字字");
        assert_eq!(clip_bytes("字字字", 7), "字…");
        assert_eq!(clip_bytes("😀😀😀", 8), "😀…");
    }

    #[test]
    fn ntfy_takes_the_topic_from_an_address_that_holds_it() {
        let m = msg();
        let p = prepare(&ch("ntfy", "https://ntfy.sh/mytopic", "", ""), &m, 0).unwrap();
        assert_eq!(p.url, "https://ntfy.sh");
        assert_eq!(json_body(&p)["topic"], "mytopic");
        let p = prepare(&ch("ntfy", "https://ntfy.sh/mytopic/", "", "mytopic"), &m, 0).unwrap();
        assert_eq!((p.url.as_str(), json_body(&p)["topic"].as_str()), ("https://ntfy.sh", Some("mytopic")));
        // A server under a path, with the topic written apart, stays as it is.
        let p = prepare(&ch("ntfy", "https://host.example.org/ntfy", "", "calls"), &m, 0).unwrap();
        assert_eq!((p.url.as_str(), json_body(&p)["topic"].as_str()), ("https://host.example.org/ntfy", Some("calls")));
        let p = prepare(&ch("ntfy", "https://host.example.org/base/mytopic", "", ""), &m, 0).unwrap();
        assert_eq!((p.url.as_str(), json_body(&p)["topic"].as_str()), ("https://host.example.org/base", Some("mytopic")));
        let e = prepare(&ch("ntfy", "https://ntfy.sh", "", ""), &m, 0).unwrap_err();
        assert_eq!(code_and_field(&e), Some(("missing_field", "target")));
    }

    #[test]
    fn slack_and_discord_bots_send_without_a_webhook_and_carry_buttons() {
        let mut m = msg();
        m.actions = vec![
            NotifyAction { label: "Confirm".into(), url: "https://bageo.example/a".into() },
            NotifyAction { label: "Decline".into(), url: "https://bageo.example/b".into() },
            NotifyAction { label: "Not a link".into(), url: "tel:123".into() },
        ];
        let p = prepare(&ch("slack", "", "xoxb-test", "C123"), &m, 0).unwrap();
        assert_eq!(p.url, "https://slack.com/api/chat.postMessage");
        assert_eq!(p.headers[0], ("Authorization".to_string(), "Bearer xoxb-test".to_string()));
        assert_eq!(json_body(&p)["channel"], "C123");
        let buttons = json_body(&p)["blocks"][1]["elements"].as_array().unwrap().clone();
        assert_eq!(buttons.len(), 2);
        assert_eq!((buttons[0]["url"].as_str(), buttons[1]["text"]["text"].as_str()), (Some("https://bageo.example/a"), Some("Decline")));
        let e = prepare(&ch("slack", "", "xoxb-test", ""), &m, 0).unwrap_err();
        assert_eq!(code_and_field(&e), Some(("missing_field", "target")));
        // A webhook, when there is one, is still used.
        let p = prepare(&ch("slack", "https://hooks.slack.com/services/T/B/x", "xoxb-test", "C123"), &m, 0).unwrap();
        assert_eq!(p.url, "https://hooks.slack.com/services/T/B/x");

        let p = prepare(&ch("discord", "", "bot-token", "123456789"), &m, 0).unwrap();
        assert_eq!(p.url, "https://discord.com/api/v10/channels/123456789/messages");
        assert_eq!(p.headers[0], ("Authorization".to_string(), "Bot bot-token".to_string()));
        let row = &json_body(&p)["components"][0]["components"];
        assert_eq!(row.as_array().unwrap().len(), 2);
        assert_eq!((row[1]["label"].as_str(), row[1]["style"].as_u64(), row[1]["url"].as_str()), (Some("Decline"), Some(5), Some("https://bageo.example/b")));
        let e = prepare(&ch("discord", "", "bot-token", "not-digits"), &m, 0).unwrap_err();
        assert_eq!(code_and_field(&e), Some(("bad_value", "target")));
        let p = prepare(&ch("discord", "https://discord.com/api/webhooks/1/x", "bot-token", "123"), &m, 0).unwrap();
        assert_eq!(p.url, "https://discord.com/api/webhooks/1/x");
    }

    #[tokio::test]
    async fn cloud_metadata_addresses_are_refused_and_lan_ones_are_not() {
        for url in [
            "http://169.254.169.254/latest/meta-data",
            "http://[fd00:ec2::254]/x",
            "http://metadata.google.internal/computeMetadata/v1",
            "http://100.100.100.200/",
            "http://[::ffff:169.254.169.254]/",
        ] {
            let e = refuse_metadata(url, false).await.unwrap_err();
            assert_eq!(channel_error(&e).map(|c| c.code), Some("blocked_address"), "{url}");
        }
        for url in ["http://127.0.0.1:8080/x", "http://192.168.1.20/ntfy", "http://localhost:8787", "https://ntfy.sh", "http://[::1]:80/"] {
            assert!(refuse_metadata(url, false).await.is_ok(), "{url}");
        }
        // The test message is refused before anything is sent.
        let e = send_channel(&Client::new(), &ch("webhook", "http://169.254.169.254/latest", "", ""), &test_message()).await.unwrap_err();
        assert_eq!(code_and_field(&e), Some(("blocked_address", "url")));
        let e = send_channel(&Client::new(), &ch("smtp", "smtp://wang@example.org@169.254.169.254:25", "pw", ""), &test_message()).await.unwrap_err();
        assert_eq!(code_and_field(&e), Some(("blocked_address", "url")));
    }

    #[test]
    fn a_redirect_is_followed_only_within_the_same_host() {
        let u = |s: &str| url::Url::parse(s).unwrap();
        assert!(same_host_redirect(&u("https://ntfy.example.org/a"), &u("https://NTFY.example.org/b")));
        assert!(same_host_redirect(&u("http://ntfy.example.org/a"), &u("https://ntfy.example.org/a")));
        assert!(!same_host_redirect(&u("https://ntfy.example.org/a"), &u("https://evil.example.net/a")));
        assert!(!same_host_redirect(&u("https://ntfy.example.org/a"), &u("http://ntfy.example.org/a")));
        assert!(!same_host_redirect(&u("https://ntfy.example.org/a"), &u("https://ntfy.example.org:8443/a")));
    }

    #[test]
    fn what_a_failed_send_reports_carries_none_of_the_channels_secrets() {
        let c = Channel {
            app_token: "xapp-1-secretvalue".into(),
            ..ch("webhook", "https://hooks.example.org/services/T0000/B0000/abcdefghijklmnop?key=querysecret1", "tok_abcdef123456", "")
        };
        let said = "HTTP 400: bad Bearer tok_abcdef123456 at /services/T0000/B0000/abcdefghijklmnop with xapp-1-secretvalue and querysecret1";
        let e = anyhow::Error::from(ChannelError::new("http_error", "", said));
        let e = scrub_error(e, &c);
        let s = format!("{e:#}");
        for secret in ["tok_abcdef123456", "abcdefghijklmnop", "xapp-1-secretvalue", "querysecret1"] {
            assert!(!s.contains(secret), "{secret} in {s}");
        }
        assert!(s.starts_with("HTTP 400: bad Bearer ***"), "{s}");
        assert_eq!(code_and_field(&e), Some(("http_error", "")));
        // Other errors lose theirs too.
        let plain = scrub_error(anyhow::anyhow!("failed for tok_abcdef123456"), &c);
        assert_eq!(format!("{plain:#}"), "failed for ***");
    }

    #[test]
    fn cards_link_to_the_console_found_when_no_address_was_typed() {
        let mut base = Settings::from_env();
        base.host = "192.168.7.7".into();
        base.port = 8787;
        base.public_url = String::new();
        let mut rt = crate::settings_store::RuntimeSettings::from_base(&base);
        rt.public_url = String::new();
        rt.mdns_name = String::new();
        let found = card_console_url(&rt, &base);
        if crate::settings_store::in_container() {
            assert_eq!(found, "", "in a container no address can be found");
        } else {
            assert_eq!(found, "http://192.168.7.7:8787/");
            let url = crate::calendar::quick_action_url(&found, "confirm_task", "t1", "");
            assert!(url.starts_with("http://192.168.7.7:8787/calls/api/quick-action?op=confirm_task"), "{url}");
        }
        // An address typed in the settings wins.
        rt.public_url = "https://calls.example.org/".into();
        assert_eq!(card_console_url(&rt, &base), "https://calls.example.org/");
    }

    #[test]
    fn call_links_use_bageo_local_without_agent_host_and_the_typed_address_first() {
        use std::net::{IpAddr, Ipv4Addr};
        let lan = |_: IpAddr| Some(Ipv4Addr::new(192, 168, 1, 20));
        let mut s = Settings::from_env();
        s.host = "127.0.0.1".into();
        s.port = 8787;
        s.mdns_name = "bageo".into();
        s.public_url = String::new();
        // AGENT_HOST not set, port 80 open, the name answering.
        let console = crate::settings_store::console_url_for(&s, false, true, true, true, lan);
        assert_eq!(call_detail_url_at(&console, "c-1"), "http://bageo.local/calls/#call=c-1");
        let console = crate::settings_store::console_url_for(&s, false, true, false, true, lan);
        assert_eq!(call_detail_url_at(&console, "c-1"), "http://bageo.local:8787/calls/#call=c-1");
        // AGENT_HOST=127.0.0.1: no link, as before.
        let console = crate::settings_store::console_url_for(&s, false, false, true, true, lan);
        assert_eq!(call_detail_url_at(&console, "c-1"), "");
        // "Address in notifications" filled in: that one.
        s.public_url = "https://calls.example.org/calls/".into();
        let console = crate::settings_store::console_url_for(&s, false, true, true, true, lan);
        assert_eq!(call_detail_url_at(&console, "c-1"), "https://calls.example.org/calls/#call=c-1");
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
        let card = Card::new("Incoming call")
            .code("Number", "<0912>")
            .row("Phone", "Pixel 8 & co")
            .tail("done");
        assert_eq!(card.plain(), "Incoming call\nNumber：<0912>\nPhone：Pixel 8 & co\n\ndone");
        assert_eq!(
            card.html(),
            "<b>Incoming call</b>\nNumber：<code>&lt;0912&gt;</code>\nPhone：Pixel 8 &amp; co\n\ndone"
        );
    }

    /// A call Bageo places itself is an `outgoing` card with the number dialed,
    /// never the `incoming` one; both carry the same fields.
    #[test]
    fn an_own_call_is_an_outgoing_card_not_an_incoming_one() {
        let p = locale::notify_phrases();
        let out = ring_message("outgoing", p.n_outgoing, "0223456701", "pixel-test-77011");
        let inc = ring_message("incoming", p.n_incoming, "0223456701", "pixel-test-77011");
        assert_eq!((out.event.as_str(), inc.event.as_str()), ("outgoing", "incoming"));
        assert!(out.title.starts_with(p.n_outgoing) && out.title.contains("0223456701"), "{}", out.title);
        assert!(out.body.starts_with(p.n_outgoing) && !out.body.contains(p.n_incoming), "{}", out.body);
        assert_eq!(out.data["caller_id"], "0223456701");
        assert!(out.data["device"].is_string() && out.data["time"].is_string());
        assert!(inc.title.starts_with(p.n_incoming) && inc.body.starts_with(p.n_incoming));
    }

    #[test]
    fn the_outgoing_title_is_written_in_all_nine_languages() {
        for lang in locale::ALL {
            let p = locale::phrases(lang);
            assert!(!p.n_outgoing.trim().is_empty(), "{lang:?}");
            assert_ne!(p.n_outgoing, p.n_incoming, "{lang:?}");
        }
        assert_eq!(locale::phrases(locale::Lang::ZhTw).n_outgoing, "去電");
        assert_eq!(locale::phrases(locale::Lang::En).n_outgoing, "Outgoing call");
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

    #[test]
    fn slack_bot_messages_always_use_plain_text_blocks_and_classify_transient_errors() {
        let mut m = msg();
        m.body = "Caller said <!here> <https://evil.example|click>".into();
        let no_btn = prepare(&ch("slack", "", "xoxb-test", "C123"), &m, 0).unwrap();
        let b0 = &json_body(&no_btn)["blocks"][0];
        assert_eq!(b0["type"], "section");
        assert_eq!(b0["text"]["type"], "plain_text");
        assert_eq!(b0["text"]["emoji"], false);
        assert_eq!(b0["text"]["text"], "Caller said <!here> <https://evil.example|click>");

        m.actions = vec![NotifyAction { label: "Open".into(), url: "https://bageo.example/x".into() }];
        let with_btn = prepare(&ch("slack", "", "xoxb-test", "C123"), &m, 0).unwrap();
        assert_eq!(json_body(&with_btn)["blocks"][0]["text"]["type"], "plain_text");
        assert_eq!(json_body(&with_btn)["blocks"][0]["text"]["emoji"], false);

        assert!(is_transient_send_error(&ChannelError::new("network_error", "", "timeout").into()));
        assert!(is_transient_send_error(&ChannelError::new("http_error", "", "HTTP 502: bad gateway").into()));
        assert!(is_transient_send_error(&ChannelError::new("http_error", "", "HTTP 429: slow down").into()));
        assert!(!is_transient_send_error(&ChannelError::new("auth_failed", "", "HTTP 401: nope").into()));
        assert!(!is_transient_send_error(&ChannelError::new("http_error", "", "HTTP 400: bad json").into()));
        assert!(!is_transient_send_error(&ChannelError::new("missing_field", "url", "empty").into()));
    }

    fn secs(base: Instant, s: u64) -> Instant {
        base + Duration::from_secs(s)
    }

    #[test]
    fn a_number_gets_two_notifications_a_minute_then_is_counted() {
        let t0 = Instant::now();
        let mut l = RepeatLimiter::new(1000);
        assert_eq!(l.admit("incoming:15550100123", secs(t0, 0)), Admit::Send);
        assert_eq!(l.admit("incoming:15550100123", secs(t0, 5)), Admit::Send);
        assert_eq!(l.admit("incoming:15550100123", secs(t0, 10)), Admit::Skip { first: true });
        assert_eq!(l.admit("incoming:15550100123", secs(t0, 20)), Admit::Skip { first: false });
        assert_eq!(l.admit("incoming:15550100123", secs(t0, 30)), Admit::Skip { first: false });
        // The window slides: the first one is 60 s old, so one more fits.
        assert_eq!(l.admit("incoming:15550100123", secs(t0, 60)), Admit::Send);
        assert_eq!(l.admit("incoming:15550100123", secs(t0, 61)), Admit::Skip { first: false });
        assert_eq!(l.take_skipped("incoming:15550100123"), 4);
    }

    #[test]
    fn the_held_back_count_is_handed_out_once() {
        let t0 = Instant::now();
        let mut l = RepeatLimiter::new(1000);
        for i in 0..5 {
            l.admit("blocked:15550100123", secs(t0, i));
        }
        assert_eq!(l.take_skipped("blocked:15550100123"), 3);
        assert_eq!(l.take_skipped("blocked:15550100123"), 0);
        assert_eq!(l.take_skipped("blocked:15550100999"), 0);
        // After the hand-out the next held-back one asks for a new summary.
        assert_eq!(l.admit("blocked:15550100123", secs(t0, 6)), Admit::Skip { first: true });
        assert_eq!(l.take_skipped("blocked:15550100123"), 1);
    }

    #[test]
    fn other_numbers_and_other_kinds_do_not_share_a_count() {
        let t0 = Instant::now();
        let mut l = RepeatLimiter::new(1000);
        for _ in 0..2 {
            assert_eq!(l.admit("incoming:15550100123", t0), Admit::Send);
        }
        assert_eq!(l.admit("incoming:15550100123", t0), Admit::Skip { first: true });
        assert_eq!(l.admit("incoming:15550100456", t0), Admit::Send);
        assert_eq!(l.admit("blocked:15550100123", t0), Admit::Send);
        assert_eq!(l.admit("call_waiting:15550100123", t0), Admit::Send);
        assert_eq!(l.take_skipped("incoming:15550100456"), 0);
    }

    #[test]
    fn the_table_stays_small_and_forgets_old_numbers() {
        let t0 = Instant::now();
        let mut l = RepeatLimiter::new(3);
        for n in 0..10u64 {
            assert_eq!(l.admit(&format!("incoming:{n}"), secs(t0, n)), Admit::Send);
            assert!(l.entries.len() <= 3);
        }
        // The oldest were dropped, the newest are kept.
        assert!(l.entries.contains_key("incoming:9") && !l.entries.contains_key("incoming:0"));
        // A minute later nothing is left to remember.
        assert_eq!(l.admit("incoming:42", secs(t0, 200)), Admit::Send);
        assert_eq!(l.entries.len(), 1);
    }

    #[test]
    fn a_number_waiting_to_be_reported_is_never_dropped() {
        let t0 = Instant::now();
        let mut l = RepeatLimiter::new(2);
        for _ in 0..3 {
            l.admit("incoming:1", t0);
        }
        for _ in 0..3 {
            l.admit("incoming:2", t0);
        }
        // Full of numbers with counts to report: a new number just goes out.
        assert_eq!(l.admit("incoming:3", secs(t0, 1)), Admit::Send);
        assert_eq!(l.take_skipped("incoming:1"), 1);
        assert_eq!(l.take_skipped("incoming:2"), 1);
    }

    #[tokio::test]
    async fn numbers_written_differently_count_as_one_and_blank_ones_are_free() {
        let http = Client::new();
        // No channel is set up, so nothing goes out: a call that is let through
        // answers false, a held-back one answers true.
        let mut settings = Settings::from_env();
        settings.call_notify = true;
        settings.notify_channels.clear();
        settings.notify_gateway_url.clear();
        settings.notify_webhook_url.clear();
        settings.telegram_bot_token.clear();
        let sent: Vec<bool> = {
            let mut v = Vec::new();
            for caller in ["+886 912-000-111", "+886912000111", "886 912 000 111 "] {
                v.push(notify_incoming_call(&http, &settings, caller, "pixel-test").await);
            }
            v
        };
        assert_eq!(sent, [false, false, true]);
        let key = format!("incoming:{}", "886912000111");
        assert_eq!(repeat_limiter().lock().unwrap().take_skipped(&key), 1);
        // Hidden or blank numbers are never held back (so each one is tried).
        for caller in ["", "  ", "no number"] {
            for _ in 0..4 {
                assert!(!notify_incoming_call(&http, &settings, caller, "pixel-test").await);
            }
        }
        // A held-back call reports success, so the callers do not treat it as lost.
        let held = notify_rejected_call(&http, &settings, "+15550100777", "pixel-test", "", false).await;
        let held2 = notify_rejected_call(&http, &settings, "+15550100777", "pixel-test", "", false).await;
        let held3 = notify_rejected_call(&http, &settings, "+15550100777", "pixel-test", "", false).await;
        assert!(!held && !held2 && held3);
    }

    #[test]
    fn the_summary_is_written_in_every_language_with_the_count() {
        for l in locale::ALL {
            let m = repeat_calls_message(locale::phrases(l), "incoming", "+15550100123", 7);
            assert!(!m.title.trim().is_empty() && m.title.contains("+15550100123"), "{l:?}");
            assert!(m.body.contains("+15550100123") && m.body.contains('7'), "{l:?}: {}", m.body);
            assert!(!m.body.contains('{'), "{l:?}: {}", m.body);
            assert_eq!(m.event, "repeat_calls");
            assert_eq!(m.data["count"], 7);
            assert_eq!(m.data["caller_id"], "+15550100123");
        }
    }
}
