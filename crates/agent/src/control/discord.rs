// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The Discord adapter: one Gateway connection per bot (the bot token of the
//! Discord channels set up to take commands), serving the Discord text
//! channels named as those channels' channel ID (Channel::target). Messages
//! there and presses of its buttons go to the [Controller]; answers and pushes
//! go back over the REST API. In a server only what is addressed to the bot
//! is passed on: a command ("/…"), a mention of the bot, a reply to the bot.
//! The bot needs Message Content Intent switched on in the developer portal.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, RequestBuilder};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio::time::{timeout, Instant, MissedTickBehavior};
use tracing::{debug, info, warn};

use crate::settings_store::SettingsStore;

use super::link::{self, text_at, AbortOnDrop, Account, Frame, Retry};
use super::{access, Body, Button, Controller, Inbound, Outbound, Push, BUTTON_DATA_MAX};

pub const API: &str = "https://discord.com/api/v10";
pub const GATEWAY: &str = "wss://gateway.discord.gg/?v=10&encoding=json";
/// Discord's limit for a message's text.
pub const TEXT_MAX: usize = 2000;
const LABEL_MAX: usize = 80;
/// Five rows of five.
const BUTTONS_MAX: usize = 25;
/// GUILD_MESSAGES, DIRECT_MESSAGES and MESSAGE_CONTENT.
const INTENTS: u64 = (1 << 9) | (1 << 12) | (1 << 15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Close codes after which connecting again the same way cannot work: the
/// token is wrong, or an intent is not allowed.
const HOPELESS: &[u16] = &[4004, 4010, 4011, 4012, 4013, 4014];
/// Close codes after which the session cannot be resumed.
const FRESH: &[u16] = &[4007, 4009];
const USER_AGENT: &str = concat!("DiscordBot (bageo, ", env!("CARGO_PKG_VERSION"), ")");

/// Where to connect: Discord, or a stand-in in tests.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub api: String,
    pub gateway: String,
    pub retry: Retry,
}

impl Endpoints {
    pub fn live() -> Self {
        Endpoints { api: API.to_string(), gateway: GATEWAY.to_string(), retry: Retry::LIVE }
    }
}

/// Connect the Discord bots the settings name, start and stop them as the
/// settings change.
pub fn spawn(ctrl: Arc<Controller>, store: Arc<SettingsStore>, http: Client) -> JoinHandle<()> {
    link::supervise("discord", store, move |acct| run(ctrl.clone(), http.clone(), Endpoints::live(), acct))
}

/// What a Gateway session needs to resume.
#[derive(Debug, Default)]
struct Session {
    id: String,
    resume_url: String,
    seq: Option<u64>,
    bot_id: String,
    /// The bot's application: a button press names it.
    app_id: String,
}

/// One bot: connect, answer, send the pushes for its chats, connect again when
/// the line drops. When Discord turns the bot away for good (a wrong token, an
/// intent that is not switched on) it stops connecting, notes why for the
/// notification settings' test ([link::gave_up]) and only keeps sending
/// pushes. Runs until aborted.
pub async fn run(ctrl: Arc<Controller>, http: Client, ep: Endpoints, acct: Account) {
    let _pushes = AbortOnDrop(tokio::spawn(send_pushes(ctrl.subscribe(), http.clone(), ep.api.clone(), acct.clone())));
    link::set_gave_up("discord", &acct.token, None);
    let mut session = Session::default();
    let mut wait = ep.retry.first;
    loop {
        let started = Instant::now();
        let resumed = !session.id.is_empty();
        match connect(&ctrl, &http, &ep, &acct, &mut session).await {
            Ok(None) => {}
            Ok(Some(code)) => {
                warn!(code, "discord refused the bot: check its token and that Message Content Intent is on; not connecting again until the settings change");
                session = Session::default();
                link::set_gave_up("discord", &acct.token, Some(refusal(code)));
                std::future::pending::<()>().await;
            }
            Err(e) => {
                warn!("discord connection dropped: {}", acct.redact(&format!("{e:#}")));
                // A resume that failed at once (a dead resume address, a session
                // Discord forgot) must not be tried again: identify afresh.
                if resumed && started.elapsed() < link::STEADY {
                    session = Session::default();
                }
            }
        }
        let pause = ep.retry.pause(wait, started.elapsed(), false);
        tokio::time::sleep(pause).await;
        wait = ep.retry.after(pause);
    }
}

/// Why Discord turned a bot away, from its close code: 4014 / 4013 an intent
/// that is not allowed (Message Content Intent is off), 4004 a token it does
/// not accept.
fn refusal(code: u16) -> &'static str {
    match code {
        4013 | 4014 => "discord_message_content",
        4004 => "discord_bad_token",
        _ => "discord_refused",
    }
}

/// One Gateway connection until it ends. Some(code): Discord refused the bot.
async fn connect(ctrl: &Arc<Controller>, http: &Client, ep: &Endpoints, acct: &Account, s: &mut Session) -> Result<Option<u16>> {
    let resume = !s.id.is_empty();
    let url = if resume && !s.resume_url.is_empty() { gateway_url(&s.resume_url) } else { ep.gateway.clone() };
    let (ws, _) = timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(url.as_str()))
        .await
        .map_err(|_| anyhow!("connecting timed out"))??;
    let (mut tx, mut rx) = ws.split();
    let hello = match timeout(CONNECT_TIMEOUT, link::next_frame(&mut rx)).await.map_err(|_| anyhow!("no hello"))?? {
        Frame::Json(v) if v["op"] == 10 => v,
        other => bail!("expected hello, got {other:?}"),
    };
    let every = Duration::from_millis(hello.pointer("/d/heartbeat_interval").and_then(Value::as_u64).unwrap_or(41_250).max(100));
    let first = if resume {
        json!({"op": 6, "d": {"token": acct.token, "session_id": s.id, "seq": s.seq}})
    } else {
        json!({"op": 2, "d": {"token": acct.token, "intents": INTENTS,
            "properties": {"os": std::env::consts::OS, "browser": "bageo", "device": "bageo"}}})
    };
    tx.send(link::ws_json(&first)).await?;
    let mut beat = tokio::time::interval_at(Instant::now() + every, every);
    beat.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut acked = true;
    loop {
        tokio::select! {
            _ = beat.tick() => {
                if !acked {
                    bail!("no heartbeat answer");
                }
                acked = false;
                tx.send(link::ws_json(&json!({"op": 1, "d": s.seq}))).await?;
            }
            frame = link::next_frame(&mut rx) => {
                let v = match frame? {
                    Frame::Json(v) => v,
                    Frame::Other => continue,
                    Frame::Closed(Some(code)) if HOPELESS.contains(&code) => return Ok(Some(code)),
                    Frame::Closed(code) => {
                        if code.is_some_and(|c| FRESH.contains(&c)) {
                            *s = Session::default();
                        }
                        bail!("closed by discord ({code:?})");
                    }
                };
                if let Some(n) = v.get("s").and_then(Value::as_u64) {
                    s.seq = Some(n);
                }
                match v["op"].as_u64() {
                    Some(0) => dispatch(ctrl, http, ep, acct, s, &v),
                    Some(1) => tx.send(link::ws_json(&json!({"op": 1, "d": s.seq}))).await?,
                    Some(7) => return Ok(None),
                    Some(9) => {
                        if v["d"].as_bool() != Some(true) {
                            *s = Session::default();
                        }
                        return Ok(None);
                    }
                    Some(11) => acked = true,
                    _ => {}
                }
            }
        }
    }
}

fn gateway_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.contains('?') {
        base.to_string()
    } else {
        format!("{base}/?v=10&encoding=json")
    }
}

/// Whether a resume address from READY may be used: Discord's own (wss, a
/// discord.gg host), or the one configured (a stand-in in tests). Anything
/// else would send the bot token elsewhere.
pub fn trusted_resume(url: &str, gateway: &str) -> bool {
    let Ok(u) = url::Url::parse(url.trim()) else {
        return false;
    };
    let host = u.host_str().unwrap_or("").to_ascii_lowercase();
    if u.scheme() == "wss" && (host == "discord.gg" || host.ends_with(".discord.gg")) {
        return true;
    }
    match url::Url::parse(gateway.trim()) {
        Ok(g) => g.scheme() == u.scheme() && g.host_str().is_some_and(|h| h.eq_ignore_ascii_case(&host)) && g.port_or_known_default() == u.port_or_known_default(),
        Err(_) => false,
    }
}

fn dispatch(ctrl: &Arc<Controller>, http: &Client, ep: &Endpoints, acct: &Account, s: &mut Session, v: &Value) {
    let d = &v["d"];
    let api = ep.api.as_str();
    match v["t"].as_str().unwrap_or("") {
        "READY" => {
            s.id = text_at(d, "/session_id").to_string();
            s.resume_url = text_at(d, "/resume_gateway_url").to_string();
            s.bot_id = text_at(d, "/user/id").to_string();
            s.app_id = text_at(d, "/application/id").to_string();
            if !s.resume_url.is_empty() && !trusted_resume(&s.resume_url, &ep.gateway) {
                warn!("discord gave a resume address that is not Discord's: the next connection starts afresh");
                s.id.clear();
                s.resume_url.clear();
                s.seq = None;
            }
            info!(chats = acct.chats.len(), "discord bot: connected");
        }
        "MESSAGE_CREATE" => {
            if let Some(msg) = inbound_message(acct, &s.bot_id, d) {
                answer(ctrl, http, api, acct, msg, None);
            }
        }
        "INTERACTION_CREATE" => {
            if let Some((msg, press)) = inbound_press(acct, &s.app_id, &s.bot_id, d) {
                answer(ctrl, http, api, acct, msg, Some(press));
            }
        }
        _ => {}
    }
}

/// The Inbound of a message in one of the account's channels. None: not for
/// the bot (its own, another bot's, a forward, another channel, or in a server
/// neither a command nor a mention of the bot nor a reply to it).
pub fn inbound_message(acct: &Account, bot_id: &str, d: &Value) -> Option<Inbound> {
    let author = &d["author"];
    let sender = author["id"].as_str()?;
    // A webhook's message is no person's, whatever its payload says.
    let webhook = d.get("webhook_id").is_some_and(|w| !w.is_null());
    if webhook || author["bot"].as_bool() == Some(true) || sender.is_empty() || sender == bot_id {
        return None;
    }
    // Message types: 0 a message, 19 a reply.
    if !matches!(d["type"].as_u64(), Some(0) | Some(19)) {
        return None;
    }
    // Someone else's words passed on (a forward) are no command of the owner's.
    if d.pointer("/message_reference/type").and_then(Value::as_u64) == Some(1) {
        return None;
    }
    let (channel_id, chat) = acct.channel_of(d["channel_id"].as_str()?)?.clone();
    let content = d["content"].as_str().unwrap_or("");
    let tags = if bot_id.is_empty() { Vec::new() } else { vec![format!("<@{bot_id}>"), format!("<@!{bot_id}>")] };
    let mentioned = !bot_id.is_empty()
        && (d["mentions"].as_array().is_some_and(|m| m.iter().any(|u| u["id"] == bot_id)) || tags.iter().any(|t| content.contains(t.as_str())));
    let mut text = content.to_string();
    for t in &tags {
        text = text.replace(t.as_str(), " ");
    }
    let text = text.trim().to_string();
    if text.is_empty() {
        return None;
    }
    let reply_to_bot = !bot_id.is_empty() && d.pointer("/referenced_message/author/id").and_then(Value::as_str) == Some(bot_id);
    let in_server = d.get("guild_id").is_some_and(|g| !g.is_null());
    if in_server && !text.starts_with('/') && !mentioned && !reply_to_bot {
        return None;
    }
    Some(Inbound {
        channel_id,
        platform: "discord".to_string(),
        chat_id: chat,
        sender_id: sender.to_string(),
        body: Body::Text(text),
        message_id: text_at(d, "/id").to_string(),
        reply_to_bot,
        direct: !in_server,
    })
}

/// A pressed button to acknowledge: (interaction id, interaction token).
pub type Press = (String, String);

/// The Inbound of a button pressed under a message in one of the account's
/// channels, and the press to acknowledge. Only a press for this bot's
/// application, on a message this bot sent: anything else is dropped
/// unanswered.
pub fn inbound_press(acct: &Account, app_id: &str, bot_id: &str, d: &Value) -> Option<(Inbound, Press)> {
    // 3: a message component (a button).
    if d["type"].as_u64() != Some(3) {
        return None;
    }
    if app_id.is_empty() || bot_id.is_empty() || text_at(d, "/application_id") != app_id || text_at(d, "/message/author/id") != bot_id {
        return None;
    }
    let data = d.pointer("/data/custom_id")?.as_str()?;
    let chat = d["channel_id"].as_str().or_else(|| d.pointer("/channel/id").and_then(Value::as_str))?;
    let (channel_id, chat) = acct.channel_of(chat)?.clone();
    let user = d.pointer("/member/user").or_else(|| d.get("user"))?;
    let sender = user["id"].as_str()?;
    if user["bot"].as_bool() == Some(true) || sender.is_empty() || data.is_empty() {
        return None;
    }
    let (id, token) = (text_at(d, "/id"), text_at(d, "/token"));
    if id.is_empty() || token.is_empty() {
        return None;
    }
    let msg = Inbound {
        channel_id,
        platform: "discord".to_string(),
        chat_id: chat,
        sender_id: sender.to_string(),
        body: Body::Button(data.to_string()),
        message_id: text_at(d, "/message/id").to_string(),
        reply_to_bot: true,
        // A press in a server carries the pressing member; in a direct message only the user.
        direct: d.get("guild_id").map_or(true, Value::is_null) && d.get("member").map_or(true, Value::is_null),
    };
    Some((msg, (id.to_string(), token.to_string())))
}

/// Hand a message to the controller on its own task (a phone mode switch waits
/// for the phone) and send its answer. A press is acknowledged only when its
/// sender may give commands there: anyone else sees the press fail, as with
/// no bot there.
fn answer(ctrl: &Arc<Controller>, http: &Client, api: &str, acct: &Account, msg: Inbound, press: Option<Press>) {
    let (ctrl, http, api, acct) = (ctrl.clone(), http.clone(), api.to_string(), acct.clone());
    tokio::spawn(async move {
        if let Some((id, token)) = press {
            if access::authorize(&ctrl.settings(), &msg).is_some() {
                if let Err(e) = acknowledge(&http, &api, &id, &token).await {
                    warn!("discord press not acknowledged: {}", acct.redact(&format!("{e:#}")).replace(&token, "<press>"));
                }
            }
        }
        if let Some(out) = ctrl.handle(&msg).await {
            if let Err(e) = send(&http, &api, &acct.token, &msg.chat_id, &out).await {
                warn!("discord reply failed: {}", acct.redact(&format!("{e:#}")));
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
                    warn!("discord push failed: {}", acct.redact(&format!("{e:#}")));
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => warn!(skipped = n, "discord pushes fell behind"),
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// "Got it, nothing to show yet": the answer comes as an edit of the message.
async fn acknowledge(http: &Client, api: &str, id: &str, token: &str) -> Result<()> {
    let url = format!("{}/interactions/{id}/{token}/callback", api.trim_end_matches('/'));
    let resp = http.post(url).header("User-Agent", USER_AGENT).json(&json!({"type": 6})).send().await.map_err(|e| anyhow!("{}", e.without_url()))?;
    if !resp.status().is_success() {
        bail!("HTTP {}", resp.status().as_u16());
    }
    Ok(())
}

fn is_snowflake(s: &str) -> bool {
    !s.is_empty() && s.len() <= 25 && s.chars().all(|c| c.is_ascii_digit())
}

/// A command's answer or a push: a new message, or (edit) the message whose
/// button was pressed rewritten, its buttons replaced. An edit Discord refuses
/// is sent as a new message.
pub async fn send(http: &Client, api: &str, token: &str, chat: &str, out: &Outbound) -> Result<()> {
    let chat = chat.trim();
    if token.trim().is_empty() || !is_snowflake(chat) {
        bail!("discord needs a bot token and a channel ID (digits)");
    }
    let text = link::fit(out.text.trim(), TEXT_MAX);
    if text.is_empty() {
        return Ok(());
    }
    let body = json!({"content": text, "components": components(&out.buttons), "allowed_mentions": {"parse": []}});
    let base = format!("{}/channels/{chat}/messages", api.trim_end_matches('/'));
    if let Some(mid) = out.edit.as_deref().map(str::trim).filter(|m| is_snowflake(m)) {
        match call(http.patch(format!("{base}/{mid}")), token, &body).await {
            Ok(()) => return Ok(()),
            Err(e) => debug!("discord edit refused, sending anew: {e:#}"),
        }
    }
    call(http.post(base), token, &body).await
}

async fn call(req: RequestBuilder, token: &str, body: &Value) -> Result<()> {
    let resp = req
        .header("Authorization", format!("Bot {}", token.trim()))
        .header("User-Agent", USER_AGENT)
        .json(body)
        .send()
        .await
        .map_err(|e| anyhow!("{}", e.without_url()))?;
    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        bail!("HTTP {}: {}", status.as_u16(), link::fit(&detail, 200));
    }
    Ok(())
}

/// The buttons as rows of five (at most 25); one whose data is longer than
/// [BUTTON_DATA_MAX] or repeats another's is left out.
pub fn components(buttons: &[Button]) -> Value {
    let mut usable: Vec<&Button> = Vec::new();
    for b in buttons {
        let ok = !b.data.is_empty() && b.data.len() <= BUTTON_DATA_MAX && !b.label.trim().is_empty();
        if ok && !usable.iter().any(|u| u.data == b.data) && usable.len() < BUTTONS_MAX {
            usable.push(b);
        }
    }
    Value::Array(
        usable
            .chunks(5)
            .map(|row| {
                json!({"type": 1, "components": row.iter().map(|b| json!({
                    "type": 2, "style": 2, "label": link::fit(b.label.trim(), LABEL_MAX), "custom_id": b.data,
                })).collect::<Vec<_>>()})
            })
            .collect(),
    )
}
