// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The Matrix adapter: long-polls /sync for each account (the homeserver and
//! access token of the Matrix channels set up to take commands), serving the
//! rooms named as those channels' room ID (Channel::target, also an alias
//! "#room:server"). Messages there go to the [Controller]; answers and
//! pushes are sent to the room. Matrix has no buttons: only the text is
//! sent, and it always says how to answer by typing. In a room with more than
//! two members only what is addressed to the bot is passed on: a command
//! ("/…"), a mention of the bot, a reply to the bot. The bot joins the rooms
//! it is invited to when they are the ones set up. It cannot read an
//! encrypted room (it has no keys): that is said in the log and, in the
//! notification settings' test, to the owner.

use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use reqwest::{Client, RequestBuilder};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{info, warn};

use crate::settings_store::SettingsStore;

use super::link::{self, text_at, AbortOnDrop, Account, Retry};
use super::{Body, Controller, Inbound, Outbound, Push};

/// The longest text sent (the Controller's own limit).
pub const TEXT_MAX: usize = 4000;
/// How many of the bot's own messages are remembered, to know a reply to one.
const SENT_KEEP: usize = 200;
const CALL_TIMEOUT: Duration = Duration::from_secs(20);

/// How to poll: the wait between retries and how long the server may hold a /sync.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub retry: Retry,
    pub poll: Duration,
}

impl Endpoints {
    pub fn live() -> Self {
        Endpoints { retry: Retry::LIVE, poll: Duration::from_secs(30) }
    }
}

/// The bot's account: its user id and display name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Me {
    pub user_id: String,
    pub name: String,
}

/// A room the account serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Room {
    pub channel_id: String,
    /// As the channel's target is written.
    pub target: String,
    /// The room ID ("!…").
    pub id: String,
    /// Two members or fewer: everything said there is for the bot.
    pub direct: bool,
}

/// Event ids of the bot's own recent messages.
pub type Sent = Arc<Mutex<VecDeque<String>>>;

struct State {
    me: Me,
    rooms: Vec<Room>,
    since: String,
    filter: String,
    /// Rooms already reported as encrypted (said once, not at every poll).
    said_encrypted: HashSet<String>,
}

/// Poll the Matrix accounts the settings name, start and stop them as the
/// settings change.
pub fn spawn(ctrl: Arc<Controller>, store: Arc<SettingsStore>, http: Client) -> JoinHandle<()> {
    link::supervise("matrix", store, move |acct| run(ctrl.clone(), http.clone(), Endpoints::live(), acct))
}

/// One account: poll, answer, send the pushes for its rooms, start again
/// after an error. Runs until aborted.
pub async fn run(ctrl: Arc<Controller>, http: Client, ep: Endpoints, acct: Account) {
    let sent: Sent = Arc::new(Mutex::new(VecDeque::new()));
    let _pushes = AbortOnDrop(tokio::spawn(send_pushes(ctrl.subscribe(), http.clone(), acct.clone(), sent.clone())));
    let mut state: Option<State> = None;
    let mut wait = ep.retry.first;
    loop {
        let started = Instant::now();
        let result = match state.take() {
            Some(mut st) => {
                let r = sync_once(&ctrl, &http, &ep, &acct, &mut st, &sent).await;
                state = Some(st);
                match r {
                    Ok(()) => {
                        wait = ep.retry.first;
                        continue;
                    }
                    Err(e) => Err(e),
                }
            }
            None => match start(&http, &acct).await {
                Ok(st) => {
                    info!(rooms = st.rooms.len(), "matrix account: polling");
                    state = Some(st);
                    continue;
                }
                Err(e) => Err(e),
            },
        };
        let (hopeless, forbidden) = match result {
            Ok(()) => (false, false),
            Err(e) => {
                let text = acct.redact(&format!("{e:#}"));
                warn!("matrix poll failed: {text}");
                (text.contains("M_UNKNOWN_TOKEN"), text.contains("M_FORBIDDEN"))
            }
        };
        let mut pause = ep.retry.pause(wait, started.elapsed(), hopeless);
        if forbidden {
            // A refusal to see a room is no reason to wait the longest: the
            // bot may be invited any moment, and joins then. Five times the
            // first wait at most (ten seconds live), not two minutes.
            pause = pause.min(ep.retry.first.saturating_mul(5));
        }
        tokio::time::sleep(pause).await;
        wait = ep.retry.after(pause);
    }
}

/// Who the bot is, its rooms, and where to start: what was said before now
/// is not answered.
async fn start(http: &Client, acct: &Account) -> Result<State> {
    let who = call(http.get(endpoint(acct, &["account", "whoami"], &[])?), &acct.token).await?;
    let user_id = text_at(&who, "/user_id").to_string();
    if user_id.is_empty() {
        bail!("matrix whoami: no user id");
    }
    let name = match call(http.get(endpoint(acct, &["profile", &user_id, "displayname"], &[])?), &acct.token).await {
        Ok(v) => text_at(&v, "/displayname").trim().to_string(),
        Err(_) => String::new(),
    };
    let mut rooms = Vec::new();
    for (channel_id, target) in &acct.chats {
        let id = room_id(http, acct, target).await?;
        let direct = match members(http, acct, &id).await {
            Ok(n) => n <= 2,
            Err(e) => {
                // Not in the room (yet): join when invited, else say so.
                if format!("{e:#}").contains("M_FORBIDDEN") {
                    join(http, acct, &id).await;
                }
                false
            }
        };
        if room_encrypted(http, acct, &id).await {
            warn!("matrix room {target}: it is encrypted and the bot cannot read encrypted rooms; create a room without encryption");
        }
        rooms.push(Room { channel_id: channel_id.clone(), target: target.clone(), id, direct });
    }
    let ids: Vec<&str> = rooms.iter().map(|r| r.id.as_str()).collect();
    let none = json!({"not_types": ["*"]});
    let filter = json!({
        "presence": none, "account_data": none,
        "room": {"rooms": ids, "timeline": {"limit": 30}, "ephemeral": none, "account_data": none,
            "state": {"lazy_load_members": true}},
    })
    .to_string();
    let first = call(http.get(endpoint(acct, &["sync"], &[("timeout", "0"), ("filter", &filter)])?), &acct.token).await?;
    let since = text_at(&first, "/next_batch").to_string();
    if since.is_empty() {
        bail!("matrix sync: no next_batch");
    }
    // An invitation that came before the bot started.
    for room in &rooms {
        if first.pointer("/rooms/invite").and_then(|i| i.get(&room.id)).is_some() {
            join(http, acct, &room.id).await;
        }
    }
    Ok(State { me: Me { user_id, name }, rooms, since, filter, said_encrypted: HashSet::new() })
}

async fn sync_once(ctrl: &Arc<Controller>, http: &Client, ep: &Endpoints, acct: &Account, st: &mut State, sent: &Sent) -> Result<()> {
    let hold = ep.poll.as_millis().to_string();
    let url = endpoint(acct, &["sync"], &[("since", &st.since), ("timeout", &hold), ("filter", &st.filter)])?;
    let v = call_for(http.get(url), &acct.token, ep.poll + CALL_TIMEOUT).await?;
    if let Some(next) = v["next_batch"].as_str().filter(|n| !n.is_empty()) {
        st.since = next.to_string();
    }
    for room in st.rooms.iter_mut() {
        if v.pointer("/rooms/invite").and_then(|i| i.get(&room.id)).is_some() {
            join(http, acct, &room.id).await;
        }
        let Some(r) = v.pointer("/rooms/join").and_then(|j| j.get(&room.id)) else {
            continue;
        };
        let events: Vec<&Value> = r.pointer("/timeline/events").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
        let state_events: Vec<&Value> = r.pointer("/state/events").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
        if has_encrypted(events.iter().chain(state_events.iter()).copied()) && st.said_encrypted.insert(room.id.clone()) {
            warn!("matrix room {}: it is encrypted and the bot cannot read encrypted rooms; create a room without encryption", room.target);
        }
        let member_change = events.iter().any(|e| e["type"] == "m.room.member")
            || r.pointer("/state/events").and_then(Value::as_array).is_some_and(|a| a.iter().any(|e| e["type"] == "m.room.member"));
        if member_change {
            if let Ok(n) = members(http, acct, &room.id).await {
                room.direct = n <= 2;
            }
        }
        for ev in events {
            let known = |id: &str| sent.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|s| s == id);
            if let Some(msg) = inbound_event(room, &st.me, known, ev) {
                answer(ctrl, http, acct, msg, sent);
            }
        }
    }
    Ok(())
}

/// The room ID of a target: as written when it is one, else the alias looked up.
async fn room_id(http: &Client, acct: &Account, target: &str) -> Result<String> {
    let t = target.trim();
    if t.starts_with('!') {
        return Ok(t.to_string());
    }
    if !t.starts_with('#') {
        bail!("a Matrix room ID starts with ! (or an alias with #)");
    }
    let v = call(http.get(endpoint(acct, &["directory", "room", t], &[])?), &acct.token).await?;
    let id = text_at(&v, "/room_id");
    if id.is_empty() {
        bail!("matrix: no room for that alias");
    }
    Ok(id.to_string())
}

/// Whether any of the events shows that the room is encrypted: an encrypted
/// message (the bot only sees its envelope) or the room's switch to encryption.
pub fn has_encrypted<'a>(mut events: impl Iterator<Item = &'a Value>) -> bool {
    events.any(|e| e["type"] == "m.room.encrypted" || e["type"] == "m.room.encryption")
}

/// Join a room the bot was invited to. Failing is only logged: without an
/// invitation the room says no (M_FORBIDDEN) and the bot tries again when one comes.
async fn join(http: &Client, acct: &Account, room: &str) -> bool {
    let url = match endpoint(acct, &["join", room], &[]) {
        Ok(u) => u,
        Err(e) => {
            warn!("matrix join failed: {}", acct.redact(&format!("{e:#}")));
            return false;
        }
    };
    match call(http.post(url).json(&json!({})), &acct.token).await {
        Ok(_) => {
            info!("matrix: joined a room the bot was invited to");
            true
        }
        Err(e) => {
            warn!("matrix: the bot is not in a room it should serve and could not join it (invite it first): {}", acct.redact(&format!("{e:#}")));
            false
        }
    }
}

/// Whether the room has end-to-end encryption on (an m.room.encryption state
/// event with an algorithm). An answer that cannot be read counts as not.
pub async fn room_encrypted(http: &Client, acct: &Account, room: &str) -> bool {
    let Ok(url) = endpoint(acct, &["rooms", room, "state", "m.room.encryption"], &[]) else {
        return false;
    };
    match call(http.get(url), &acct.token).await {
        Ok(v) => !text_at(&v, "/algorithm").is_empty(),
        Err(_) => false,
    }
}

async fn members(http: &Client, acct: &Account, room: &str) -> Result<usize> {
    let v = call(http.get(endpoint(acct, &["rooms", room, "joined_members"], &[])?), &acct.token).await?;
    Ok(v["joined"].as_object().map(|o| o.len()).unwrap_or(0))
}

/// A reply quoting the bot, in the older way (the quote in the text): whether
/// it quotes the bot, and the text without the quote.
fn without_quote<'a>(body: &'a str, me: &str) -> (bool, &'a str) {
    if !body.starts_with("> ") {
        return (false, body);
    }
    let quotes_bot = body.starts_with(&format!("> <{me}>"));
    match body.find("\n\n") {
        Some(i) => (quotes_bot, body[i + 2..].trim()),
        None => {
            let mut rest = body;
            while rest.trim_start_matches([' ', '\t']).starts_with('>') {
                match rest.find('\n') {
                    Some(i) => rest = &rest[i + 1..],
                    None => return (quotes_bot, ""),
                }
            }
            (quotes_bot, rest.trim())
        }
    }
}

/// Whether the text starts with the bot's name (user id or display name), and
/// the text without it.
fn without_name<'a>(body: &'a str, me: &Me) -> (bool, &'a str) {
    for name in [me.user_id.trim(), me.name.trim()] {
        if name.is_empty() {
            continue;
        }
        if let Some(rest) = body.strip_prefix(name) {
            if !rest.chars().next().is_some_and(char::is_alphanumeric) {
                return (true, rest.trim_start_matches([':', ',']).trim());
            }
        }
    }
    (!me.user_id.is_empty() && body.contains(me.user_id.as_str()), body)
}

/// The Inbound of a message in a room the account serves. None: not for the
/// bot (its own, a notice, an edit, or in a room with more members neither a
/// command nor a mention nor a reply to the bot). sent_by_bot: whether an
/// event id is one of the bot's own messages.
pub fn inbound_event(room: &Room, me: &Me, sent_by_bot: impl Fn(&str) -> bool, ev: &Value) -> Option<Inbound> {
    if ev["type"] != "m.room.message" {
        return None;
    }
    let sender = ev["sender"].as_str()?;
    if sender.is_empty() || sender == me.user_id {
        return None;
    }
    let c = &ev["content"];
    // A notice is what bots send; an edit is no new message.
    if c["msgtype"] != "m.text" || c.get("m.new_content").is_some() || text_at(c, "/m.relates_to/rel_type") == "m.replace" {
        return None;
    }
    let raw = c["body"].as_str()?.trim();
    let reply_id = text_at(c, "/m.relates_to/m.in_reply_to/event_id");
    let (quotes_bot, body) = without_quote(raw, &me.user_id);
    let reply_to_bot = !reply_id.is_empty() && (sent_by_bot(reply_id) || quotes_bot);
    let tagged = c.pointer("/m.mentions/user_ids").and_then(Value::as_array).is_some_and(|a| a.iter().any(|u| u == me.user_id.as_str()));
    let (named, text) = without_name(body, me);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if !room.direct && !text.starts_with('/') && !tagged && !named && !reply_to_bot {
        return None;
    }
    Some(Inbound {
        channel_id: room.channel_id.clone(),
        platform: "matrix".to_string(),
        chat_id: room.target.clone(),
        sender_id: sender.to_string(),
        body: Body::Text(text.to_string()),
        message_id: text_at(ev, "/event_id").to_string(),
        reply_to_bot,
        direct: room.direct,
    })
}

/// Hand a message to the controller on its own task and send its answer.
fn answer(ctrl: &Arc<Controller>, http: &Client, acct: &Account, msg: Inbound, sent: &Sent) {
    let (ctrl, http, acct, sent) = (ctrl.clone(), http.clone(), acct.clone(), sent.clone());
    tokio::spawn(async move {
        if let Some(out) = ctrl.handle(&msg).await {
            if let Err(e) = send(&http, &acct, &msg.chat_id, &out, &sent).await {
                warn!("matrix reply failed: {}", acct.redact(&format!("{e:#}")));
            }
        }
    });
}

async fn send_pushes(mut rx: broadcast::Receiver<Push>, http: Client, acct: Account, sent: Sent) {
    loop {
        match rx.recv().await {
            Ok(p) => {
                if !acct.serves(&p) {
                    continue;
                }
                if let Err(e) = send(&http, &acct, &p.chat_id, &p.out, &sent).await {
                    warn!("matrix push failed: {}", acct.redact(&format!("{e:#}")));
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => warn!(skipped = n, "matrix pushes fell behind"),
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

fn next_txn() -> String {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    format!("bageo{now}.{}", COUNT.fetch_add(1, Ordering::Relaxed))
}

/// A command's answer or a push, as a new message (buttons are not shown;
/// the text says how to answer by typing).
pub async fn send(http: &Client, acct: &Account, target: &str, out: &Outbound, sent: &Sent) -> Result<()> {
    let text = link::fit(out.text.trim(), TEXT_MAX);
    if text.is_empty() {
        return Ok(());
    }
    let room = room_id(http, acct, target).await?;
    let txn = next_txn();
    let url = endpoint(acct, &["rooms", &room, "send", "m.room.message", &txn], &[])?;
    let body = json!({"msgtype": "m.text", "body": text});
    let v = match call(http.put(url.clone()).json(&body), &acct.token).await {
        // Not in the room: join if invited, then say it once more.
        Err(e) if format!("{e:#}").contains("M_FORBIDDEN") && join(http, acct, &room).await => {
            call(http.put(url).json(&body), &acct.token).await?
        }
        other => other?,
    };
    if let Some(id) = v["event_id"].as_str() {
        let mut s = sent.lock().unwrap_or_else(|e| e.into_inner());
        s.push_back(id.to_string());
        while s.len() > SENT_KEEP {
            s.pop_front();
        }
    }
    Ok(())
}

/// A client-server API address on the account's homeserver.
fn endpoint(acct: &Account, path: &[&str], query: &[(&str, &str)]) -> Result<url::Url> {
    let mut u = url::Url::parse(acct.url.trim()).context("the homeserver URL")?;
    if !matches!(u.scheme(), "http" | "https") {
        bail!("the homeserver URL must start with http:// or https://");
    }
    u.path_segments_mut()
        .map_err(|_| anyhow!("the homeserver URL"))?
        .pop_if_empty()
        .extend(["_matrix", "client", "v3"])
        .extend(path);
    if !query.is_empty() {
        u.query_pairs_mut().extend_pairs(query);
    }
    Ok(u)
}

async fn call(req: RequestBuilder, token: &str) -> Result<Value> {
    call_for(req, token, CALL_TIMEOUT).await
}

async fn call_for(req: RequestBuilder, token: &str, wait: Duration) -> Result<Value> {
    let resp = req
        .bearer_auth(token.trim())
        .timeout(wait)
        .send()
        .await
        .map_err(|e| anyhow!("{}", e.without_url()))?;
    let status = resp.status();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        bail!("HTTP {} {}", status.as_u16(), link::fit(text_at(&v, "/errcode"), 60));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_quote_strips_single_newline_matrix_fallback_quote() {
        let (quotes_bot, text) = without_quote("> <@bot:example.org> Confirm call?\ncancel", "@bot:example.org");
        assert!(quotes_bot);
        assert_eq!(text, "cancel");

        let (quotes_bot2, text2) = without_quote("> line one\n> line two\nyes", "@bot:example.org");
        assert!(!quotes_bot2);
        assert_eq!(text2, "yes");
    }

    #[test]
    fn endpoint_rejects_non_http_with_accurate_scheme_message() {
        let acct = Account {
            kind: "matrix".into(),
            url: "ftp://matrix.example.org".into(),
            token: "tok".into(),
            app_token: String::new(),
            chats: Vec::new(),
        };
        let err = endpoint(&acct, &["sync"], &[]).unwrap_err().to_string();
        assert!(err.contains("http:// or https://"), "{err}");
    }
}
