// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The Slack adapter: Socket Mode, one connection per app (the bot token and
//! app-level token of the Slack channels set up to take commands), serving
//! the Slack channels named as those channels' channel ID (Channel::target).
//! Messages there and presses of its buttons (Block Kit) go to the
//! [Controller]; answers and pushes go back with chat.postMessage /
//! chat.update. In a channel only what is addressed to the bot is passed on:
//! a command ("/…"), a mention of the bot, a reply in a thread the bot
//! started, a typed 1 or 2 while a confirmation waits in the channel. A
//! direct message (a channel ID starting with D) is always for the bot.
//! Slack's own slash commands (set up in the app: /calls, /status …) come as
//! their own kind of message in Socket Mode and are read the same way.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio::time::{timeout, Instant, MissedTickBehavior};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, info, warn};

use crate::settings_store::SettingsStore;

use super::link::{self, text_at, AbortOnDrop, Account, Frame, Retry};
use super::{access, Body, Button, Controller, Inbound, Outbound, Push, BUTTON_DATA_MAX};

pub const API: &str = "https://slack.com/api";
/// Slack's limit for a section block's text (the message is one).
pub const TEXT_MAX: usize = 3000;
const LABEL_MAX: usize = 75;
const BUTTONS_MAX: usize = 25;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// A ping goes out this often; a line silent for QUIET is taken as dead.
const PING_EVERY: Duration = Duration::from_secs(30);
const QUIET: Duration = Duration::from_secs(120);

/// Where to connect: Slack, or a stand-in in tests.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub api: String,
    pub retry: Retry,
}

impl Endpoints {
    pub fn live() -> Self {
        Endpoints { api: API.to_string(), retry: Retry::LIVE }
    }
}

/// The bot's own ids: its messages are not commands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Me {
    pub user_id: String,
    pub bot_id: String,
}

/// Connect the Slack apps the settings name, start and stop them as the
/// settings change.
pub fn spawn(ctrl: Arc<Controller>, store: Arc<SettingsStore>, http: Client) -> JoinHandle<()> {
    link::supervise("slack", store, move |acct| run(ctrl.clone(), http.clone(), Endpoints::live(), acct))
}

/// One app: connect, answer, send the pushes for its chats, connect again
/// when the line drops (Slack also asks for that now and then). Runs until
/// aborted.
pub async fn run(ctrl: Arc<Controller>, http: Client, ep: Endpoints, acct: Account) {
    let _pushes = AbortOnDrop(tokio::spawn(send_pushes(ctrl.subscribe(), http.clone(), ep.api.clone(), acct.clone())));
    let mut me: Option<Me> = None;
    let mut wait = ep.retry.first;
    loop {
        let started = Instant::now();
        let result = match me.clone() {
            Some(m) => serve(&ctrl, &http, &ep, &acct, &m).await,
            None => match who_am_i(&http, &ep.api, &acct.token).await {
                Ok(m) => {
                    me = Some(m);
                    continue;
                }
                Err(e) => Err(e),
            },
        };
        let hopeless = match result {
            Ok(()) => false,
            Err(e) => {
                let text = acct.redact(&format!("{e:#}"));
                warn!("slack connection dropped: {text}");
                ["invalid_auth", "not_authed", "account_inactive", "token_revoked", "missing_scope"].iter().any(|c| text.contains(c))
            }
        };
        let pause = ep.retry.pause(wait, started.elapsed(), hopeless);
        tokio::time::sleep(pause).await;
        wait = ep.retry.after(pause);
    }
}

async fn who_am_i(http: &Client, api: &str, token: &str) -> Result<Me> {
    let v = api_call(http, api, "auth.test", token, &json!({})).await?;
    let me = Me { user_id: text_at(&v, "/user_id").to_string(), bot_id: text_at(&v, "/bot_id").to_string() };
    if me.user_id.is_empty() {
        bail!("slack auth.test: no user id");
    }
    Ok(me)
}

/// One Socket Mode connection until it ends.
async fn serve(ctrl: &Arc<Controller>, http: &Client, ep: &Endpoints, acct: &Account, me: &Me) -> Result<()> {
    let v = api_call(http, &ep.api, "apps.connections.open", &acct.app_token, &json!({})).await?;
    let url = v["url"].as_str().filter(|u| !u.is_empty()).ok_or_else(|| anyhow!("slack gave no socket address"))?;
    let (ws, _) = timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(url)).await.map_err(|_| anyhow!("connecting timed out"))??;
    let (mut tx, mut rx) = ws.split();
    let mut ping = tokio::time::interval_at(Instant::now() + PING_EVERY, PING_EVERY);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut heard = Instant::now();
    loop {
        tokio::select! {
            _ = ping.tick() => {
                if heard.elapsed() >= QUIET {
                    bail!("slack went quiet");
                }
                tx.send(WsMessage::Ping(Vec::new().into())).await?;
            }
            frame = link::next_frame(&mut rx) => {
                heard = Instant::now();
                let v = match frame? {
                    Frame::Json(v) => v,
                    Frame::Other => continue,
                    Frame::Closed(_) => return Ok(()),
                };
                // Every envelope is acknowledged at once, or Slack sends it again.
                if let Some(id) = v["envelope_id"].as_str() {
                    tx.send(link::ws_json(&json!({"envelope_id": id}))).await?;
                }
                match v["type"].as_str().unwrap_or("") {
                    "hello" => info!(chats = acct.chats.len(), "slack app: connected"),
                    "disconnect" => return Ok(()),
                    "events_api" => {
                        let waiting = |chat: &str| ctrl.confirms.waiting(chat);
                        if let Some(msg) = inbound_event_with(acct, me, &v["payload"]["event"], waiting) {
                            answer(ctrl, http, &ep.api, acct, msg);
                        }
                    }
                    "slash_commands" => {
                        if let Some(msg) = inbound_slash(acct, &v["payload"]) {
                            answer(ctrl, http, &ep.api, acct, msg);
                        }
                    }
                    "interactive" => {
                        if let Some(msg) = inbound_action(acct, me, &v["payload"]) {
                            answer(ctrl, http, &ep.api, acct, msg);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Slack writes &, < and > as entities.
fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// The Inbound of a message in one of the account's channels. None: not for
/// the bot (its own, another bot's, an edit or other change, another channel,
/// or in a channel neither a command nor a mention nor a reply in its thread).
/// A typed 1 or 2 in a channel is not taken (see [inbound_event_with]).
pub fn inbound_event(acct: &Account, me: &Me, ev: &Value) -> Option<Inbound> {
    inbound_event_with(acct, me, ev, |_| false)
}

/// The same, where a bare 1 or 2 typed in a channel's main view also counts
/// while waiting(chat key) says a confirmation is open there (it answers the
/// bot's question, which Slack does not show as a thread reply).
pub fn inbound_event_with(acct: &Account, me: &Me, ev: &Value, waiting: impl Fn(&str) -> bool) -> Option<Inbound> {
    if ev["type"] != "message" || ev.get("subtype").is_some_and(|s| !s.is_null()) {
        return None;
    }
    let sender = ev["user"].as_str()?;
    if sender.is_empty() || sender == me.user_id || ev.get("bot_id").is_some_and(|b| !b.is_null()) {
        return None;
    }
    let (channel_id, chat) = acct.channel_of(ev["channel"].as_str()?)?.clone();
    let raw = ev["text"].as_str().unwrap_or("");
    let tag = format!("<@{}>", me.user_id);
    let mentioned = !me.user_id.is_empty() && raw.contains(&tag);
    let text = unescape(raw.replace(&tag, " ").trim());
    let text = text.trim().to_string();
    if text.is_empty() {
        return None;
    }
    let ts = text_at(ev, "/ts");
    let thread = text_at(ev, "/thread_ts");
    let reply_to_bot = !me.user_id.is_empty() && !thread.is_empty() && thread != ts && text_at(ev, "/parent_user_id") == me.user_id;
    // Slack's direct message channels start with D; what the event itself
    // says about the channel is not taken on trust.
    let direct = chat.starts_with('D');
    let answers_open_question = super::yes_no(&text).is_some() && waiting(&access::chat_key(&channel_id, &chat));
    if !direct && !text.starts_with('/') && !mentioned && !reply_to_bot && !answers_open_question {
        return None;
    }
    Some(Inbound {
        channel_id,
        platform: "slack".to_string(),
        chat_id: chat,
        sender_id: sender.to_string(),
        body: Body::Text(text),
        message_id: ts.to_string(),
        reply_to_bot,
        direct,
    })
}

/// The Inbound of a button pressed under a message in one of the account's channels.
pub fn inbound_action(acct: &Account, me: &Me, p: &Value) -> Option<Inbound> {
    if p["type"] != "block_actions" {
        return None;
    }
    let sender = p.pointer("/user/id")?.as_str()?;
    if sender.is_empty() || sender == me.user_id {
        return None;
    }
    let chat = p.pointer("/channel/id").or_else(|| p.pointer("/container/channel_id")).and_then(Value::as_str)?;
    let (channel_id, chat) = acct.channel_of(chat)?.clone();
    let data = p.pointer("/actions/0/value")?.as_str()?;
    if data.is_empty() {
        return None;
    }
    let message_id = p.pointer("/container/message_ts").or_else(|| p.pointer("/message/ts")).and_then(Value::as_str).unwrap_or("");
    Some(Inbound {
        channel_id,
        platform: "slack".to_string(),
        chat_id: chat,
        sender_id: sender.to_string(),
        body: Body::Button(data.to_string()),
        message_id: message_id.to_string(),
        reply_to_bot: true,
        direct: false,
    })
}

/// The Inbound of a Slack slash command ("/calls today") typed in one of the
/// account's channels. Slack takes every message that starts with a slash for
/// a slash command of its own, so this is how a command reaches the bot there.
pub fn inbound_slash(acct: &Account, p: &Value) -> Option<Inbound> {
    let command = text_at(p, "/command").trim();
    let sender = text_at(p, "/user_id").trim();
    if !command.starts_with('/') || command.len() < 2 || sender.is_empty() {
        return None;
    }
    let (channel_id, chat) = acct.channel_of(text_at(p, "/channel_id"))?.clone();
    let args = text_at(p, "/text").trim();
    let text = if args.is_empty() { command.to_string() } else { format!("{command} {args}") };
    Some(Inbound {
        channel_id,
        platform: "slack".to_string(),
        chat_id: chat,
        sender_id: sender.to_string(),
        body: Body::Text(text),
        message_id: String::new(),
        reply_to_bot: false,
        direct: false,
    })
}

/// Hand a message to the controller on its own task and send its answer.
fn answer(ctrl: &Arc<Controller>, http: &Client, api: &str, acct: &Account, msg: Inbound) {
    let (ctrl, http, api, acct) = (ctrl.clone(), http.clone(), api.to_string(), acct.clone());
    tokio::spawn(async move {
        if let Some(out) = ctrl.handle(&msg).await {
            if let Err(e) = send(&http, &api, &acct.token, &msg.chat_id, &out).await {
                warn!("slack reply failed: {}", acct.redact(&format!("{e:#}")));
            }
        }
    });
}

async fn send_pushes(mut rx: broadcast::Receiver<Push>, http: Client, api: String, acct: Account) {
    loop {
        match rx.recv().await {
            Ok(p) => {
                if !acct.serves(&p) {
                    continue;
                }
                if let Err(e) = send(&http, &api, &acct.token, &p.chat_id, &p.out).await {
                    warn!("slack push failed: {}", acct.redact(&format!("{e:#}")));
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => warn!(skipped = n, "slack pushes fell behind"),
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

async fn api_call(http: &Client, api: &str, method: &str, token: &str, body: &Value) -> Result<Value> {
    let url = format!("{}/{method}", api.trim_end_matches('/'));
    let resp = http
        .post(url)
        .bearer_auth(token.trim())
        .json(body)
        .send()
        .await
        .map_err(|e| anyhow!("slack {method}: {}", e.without_url()))?;
    let status = resp.status();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        bail!("slack {method}: HTTP {}", status.as_u16());
    }
    if v["ok"].as_bool() != Some(true) {
        bail!("slack {method}: {}", link::fit(v["error"].as_str().unwrap_or("refused"), 100));
    }
    Ok(v)
}

/// A command's answer or a push: a new message, or (edit) the message whose
/// button was pressed rewritten, its buttons replaced. An edit Slack refuses
/// is sent as a new message.
pub async fn send(http: &Client, api: &str, token: &str, chat: &str, out: &Outbound) -> Result<()> {
    let chat = chat.trim();
    if token.trim().is_empty() || chat.is_empty() {
        bail!("slack needs a bot token and a channel ID");
    }
    let text = link::fit(out.text.trim(), TEXT_MAX);
    if text.is_empty() {
        return Ok(());
    }
    let mut blocks = vec![json!({"type": "section", "text": {"type": "plain_text", "text": text, "emoji": false}})];
    if let Some(a) = actions(&out.buttons) {
        blocks.push(a);
    }
    let mut body = json!({"channel": chat, "text": text, "blocks": blocks, "unfurl_links": false, "unfurl_media": false});
    if let Some(ts) = out.edit.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        body["ts"] = json!(ts);
        match api_call(http, api, "chat.update", token, &body).await {
            Ok(_) => return Ok(()),
            Err(e) => debug!("slack edit refused, sending anew: {e:#}"),
        }
        if let Some(o) = body.as_object_mut() {
            o.remove("ts");
        }
    }
    api_call(http, api, "chat.postMessage", token, &body).await.map(|_| ())
}

/// The buttons as one actions block (at most 25); one whose data is longer
/// than [BUTTON_DATA_MAX] is left out.
pub fn actions(buttons: &[Button]) -> Option<Value> {
    let elements: Vec<Value> = buttons
        .iter()
        .filter(|b| !b.data.is_empty() && b.data.len() <= BUTTON_DATA_MAX && !b.label.trim().is_empty())
        .take(BUTTONS_MAX)
        .enumerate()
        .map(|(i, b)| {
            json!({"type": "button", "action_id": format!("bageo_{i}"), "value": b.data,
                "text": {"type": "plain_text", "text": link::fit(b.label.trim(), LABEL_MAX), "emoji": false}})
        })
        .collect();
    if elements.is_empty() {
        None
    } else {
        Some(json!({"type": "actions", "elements": elements}))
    }
}
