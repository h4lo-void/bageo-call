// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! What the Discord, Slack and Matrix adapters share: the accounts to connect
//! (one connection per bot, serving every chat set up with it), starting and
//! stopping them as the settings change, the wait between retries, reading
//! WebSocket frames, and keeping tokens out of the log.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::{Stream, StreamExt};
use serde_json::Value;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::{self, Message as WsMessage};
use tracing::info;

use crate::notify::Channel;
use crate::settings_store::{RuntimeSettings, SettingsStore};

use super::{Push, COMMAND_KINDS};

/// How often the accounts to connect are read from the settings.
const RECHECK: Duration = Duration::from_secs(10);
/// A connection that stayed up this long was fine: the next wait starts over.
pub const STEADY: Duration = Duration::from_secs(60);

/// One bot to connect and the chats it serves: (channel id, chat id as the
/// channel's target).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub kind: String,
    /// Matrix: the homeserver. Empty for the others.
    pub url: String,
    pub token: String,
    /// Slack: the app-level token Socket Mode connects with. Empty for the others.
    pub app_token: String,
    pub chats: Vec<(String, String)>,
}

impl Account {
    /// (channel id, chat id) of a chat this account serves.
    pub fn channel_of(&self, chat: &str) -> Option<&(String, String)> {
        let chat = chat.trim();
        if chat.is_empty() {
            return None;
        }
        self.chats.iter().find(|(_, c)| c == chat)
    }

    /// A push for one of this account's chats.
    pub fn serves(&self, p: &Push) -> bool {
        self.chats.iter().any(|(id, c)| *id == p.channel_id && c == p.chat_id.trim())
    }

    /// Text without this account's tokens (also as written in a URL).
    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_string();
        for s in [self.token.trim(), self.app_token.trim()] {
            if s.is_empty() {
                continue;
            }
            out = out.replace(s, "<token>");
            let encoded: String = url::form_urlencoded::byte_serialize(s.as_bytes()).collect();
            if encoded != s {
                out = out.replace(&encoded, "<token>");
            }
        }
        out
    }
}

/// What a channel of this kind needs before it can connect: Discord a bot
/// token, Slack a bot token and an app-level token, Matrix a homeserver and
/// an access token; all a chat (the target).
fn ready(c: &Channel) -> bool {
    if c.token.trim().is_empty() || c.target.trim().is_empty() || c.id.is_empty() {
        return false;
    }
    match c.kind.as_str() {
        "slack" => !c.app_token.trim().is_empty(),
        "matrix" => !c.url.trim().is_empty(),
        _ => true,
    }
}

/// The accounts of one kind to connect now: channels switched on that take
/// commands. Channels sharing a bot share one connection; a chat set up twice
/// on one bot is the first channel's.
pub fn accounts(rt: &RuntimeSettings, kind: &str) -> Vec<Account> {
    let mut out: Vec<Account> = Vec::new();
    if !COMMAND_KINDS.contains(&kind) {
        return out;
    }
    for c in rt.notify_channels.iter().filter(|c| c.kind == kind && c.enabled && c.control && ready(c)) {
        let url = if kind == "matrix" { c.url.trim().trim_end_matches('/') } else { "" };
        let app = if kind == "slack" { c.app_token.trim() } else { "" };
        let token = c.token.trim();
        let chat = (c.id.clone(), c.target.trim().to_string());
        match out.iter_mut().find(|a| a.url == url && a.token == token && a.app_token == app) {
            Some(a) => {
                if !a.chats.iter().any(|(_, t)| *t == chat.1) {
                    a.chats.push(chat);
                }
            }
            None => out.push(Account {
                kind: kind.to_string(),
                url: url.to_string(),
                token: token.to_string(),
                app_token: app.to_string(),
                chats: vec![chat],
            }),
        }
    }
    out
}

/// The wait between connection attempts: doubling from first up to max.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retry {
    pub first: Duration,
    pub max: Duration,
}

impl Retry {
    pub const LIVE: Retry = Retry { first: Duration::from_secs(2), max: Duration::from_secs(120) };

    pub fn after(&self, last: Duration) -> Duration {
        last.saturating_mul(2).clamp(self.first, self.max)
    }

    /// The wait after a connection ended: from the start again when it had
    /// stayed up, the longest when connecting again the same way cannot work.
    pub fn pause(&self, current: Duration, lasted: Duration, hopeless: bool) -> Duration {
        if hopeless {
            self.max
        } else if lasted >= STEADY {
            self.first
        } else {
            current.clamp(self.first, self.max)
        }
    }
}

/// A task that stops with its owner.
pub struct AbortOnDrop(pub JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Connect the accounts of one kind the settings name; start and stop them
/// as the settings change (a changed token, chat or switch restarts it).
pub fn supervise<F, Fut>(kind: &'static str, store: Arc<SettingsStore>, start: F) -> JoinHandle<()>
where
    F: Fn(Account) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        let mut running: Vec<(Account, AbortOnDrop)> = Vec::new();
        loop {
            let want = accounts(&store.snapshot(), kind);
            running.retain(|(a, _)| want.contains(a));
            for a in want {
                if running.iter().any(|(r, _)| *r == a) {
                    continue;
                }
                info!(kind, chats = a.chats.len(), "chat commands: connecting");
                let task = tokio::spawn(start(a.clone()));
                running.push((a, AbortOnDrop(task)));
            }
            tokio::time::sleep(RECHECK).await;
        }
    })
}

/// At most max characters, an ellipsis marking a cut.
pub fn fit(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// What came over a WebSocket.
#[derive(Debug)]
pub enum Frame {
    Json(Value),
    /// Closed, with the close code when one was given.
    Closed(Option<u16>),
    /// A ping, pong or binary frame: the line is alive.
    Other,
}

pub async fn next_frame<S>(rx: &mut S) -> Result<Frame>
where
    S: Stream<Item = Result<WsMessage, tungstenite::Error>> + Unpin,
{
    match rx.next().await {
        None => Ok(Frame::Closed(None)),
        Some(Err(e)) => Err(e.into()),
        Some(Ok(WsMessage::Text(t))) => Ok(Frame::Json(serde_json::from_str(t.as_str()).context("a frame that is not JSON")?)),
        Some(Ok(WsMessage::Close(f))) => Ok(Frame::Closed(f.map(|f| u16::from(f.code)))),
        Some(Ok(_)) => Ok(Frame::Other),
    }
}

pub fn ws_json(v: &Value) -> WsMessage {
    WsMessage::Text(v.to_string().into())
}

pub fn text_at<'a>(v: &'a Value, pointer: &str) -> &'a str {
    v.pointer(pointer).and_then(Value::as_str).unwrap_or("")
}

/// Bots whose connection ended for good (the service turned the bot away,
/// and connecting again the same way cannot work), by kind and token: why, as
/// a short code. The notification settings' test reads it to tell the owner.
static GAVE_UP: Mutex<Option<HashMap<u64, &'static str>>> = Mutex::new(None);

fn bot_key(kind: &str, token: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (kind, token.trim()).hash(&mut h);
    h.finish()
}

/// Note why a bot's connection ended for good (None: it is fine again).
pub fn set_gave_up(kind: &str, token: &str, why: Option<&'static str>) {
    let mut all = GAVE_UP.lock().unwrap_or_else(|e| e.into_inner());
    let map = all.get_or_insert_with(HashMap::new);
    match why {
        Some(w) => {
            map.insert(bot_key(kind, token), w);
        }
        None => {
            map.remove(&bot_key(kind, token));
        }
    }
}

/// Why this bot's connection ended for good, if it did.
pub fn gave_up(kind: &str, token: &str) -> Option<&'static str> {
    let all = GAVE_UP.lock().unwrap_or_else(|e| e.into_inner());
    all.as_ref()?.get(&bot_key(kind, token)).copied()
}
