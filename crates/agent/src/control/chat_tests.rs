//! The Discord, Slack and Matrix adapters against stand-ins on this machine:
//! what becomes an Inbound, what is ignored, reconnecting, limits, secrets,
//! and no answer at all where commands are off. All ids and tokens are made up.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::WebSocketStream;

use super::link::{self, Account, Retry};
use super::*;
use crate::calls_http::PhoneDevices;
use crate::config::Settings;
use crate::settings_store::SettingsStore;

// ------------------------------------------------------------ rig

struct Rig {
    root: PathBuf,
    state: CallsState,
}

impl Rig {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("bageo-chat-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        let mut base = Settings::from_env();
        base.calls_data_dir = root.to_string_lossy().into_owned();
        base.telegram_bot_token = String::new();
        base.telegram_chat_id = String::new();
        base.gemini_api_key = "".into();
        let state = CallsState {
            settings: base.clone(),
            store: crate::calls::CallStore::new(root.join("calls"), true, false, 30),
            settings_store: SettingsStore::load(base),
            task_store: crate::tasks::TaskStore::new(root.join("tasks")),
            http: reqwest::Client::new(),
            control: crate::callback::ControlRegistry::new(),
            ambience: Arc::new(crate::ambience::AmbienceLibrary::load(&root, 16000)),
            previews: Arc::new(crate::voice_preview::VoicePreviews::new(root.join("voice_preview"), root.join("voices"))),
            phones: PhoneDevices::new(),
            contacts: crate::contacts::ContactStore::open(root.join("contacts.json")),
            errands: crate::errands::ErrandStore::open(root.join("errands.json"), crate::errands::SimBook::in_memory()),
        };
        let rig = Rig { root, state };
        rig.set(json!({"gemini_api_key_clear": true}));
        rig
    }

    fn set(&self, patch: Value) {
        self.state.settings_store.apply_patch(&patch).unwrap();
    }

    /// One channel of each kind, commands on, only the listed sender.
    fn channels(&self, matrix_url: &str, control: bool) {
        self.set(json!({"notify_channels": [
            {"id": "d1", "kind": "discord", "enabled": true, "url": "https://discord.example/api/webhooks/1/x",
             "token": "test-token-discord", "target": "123", "control": control, "control_users": ["111"]},
            {"id": "s1", "kind": "slack", "enabled": true, "url": "https://hooks.slack.example/x",
             "token": "xoxb-test-token", "app_token": "xapp-test-token", "target": "C123", "control": control, "control_users": ["U111"]},
            {"id": "m1", "kind": "matrix", "enabled": true, "url": matrix_url,
             "token": "test-token-matrix", "target": "!room:example.org", "control": control, "control_users": ["@lee:example.org"]},
        ]}));
    }

    fn account(&self, kind: &str) -> Account {
        link::accounts(&self.state.settings_store.snapshot(), kind).remove(0)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

const FAST: Retry = Retry { first: Duration::from_millis(50), max: Duration::from_millis(200) };

fn acct(kind: &str, chat: &str) -> Account {
    Account { kind: kind.into(), url: String::new(), token: "test-token".into(), app_token: String::new(), chats: vec![("c1".into(), chat.into())] }
}

async fn until(limit: Duration, f: impl Fn() -> bool) -> bool {
    let end = tokio::time::Instant::now() + limit;
    while tokio::time::Instant::now() < end {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    f()
}

// ------------------------------------------------------------ stand-ins

#[derive(Debug, Clone)]
struct Req {
    method: String,
    path: String,
    auth: String,
    body: Value,
}

type Reqs = Arc<Mutex<Vec<Req>>>;

fn bare(path: &str) -> &str {
    path.split('?').next().unwrap_or("")
}

/// An HTTP stand-in: answer(request, how many earlier requests had the same
/// path) gives (status, JSON body). Its address and what it was sent.
async fn http_server(answer: impl Fn(&Req, usize) -> (u16, String) + Send + Sync + 'static) -> (String, Reqs) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let reqs: Reqs = Arc::new(Mutex::new(Vec::new()));
    let log = reqs.clone();
    let answer = Arc::new(answer);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let (log, answer) = (log.clone(), answer.clone());
            tokio::spawn(async move {
                let Some(req) = read_request(&mut sock).await else { return };
                let n = log.lock().unwrap().iter().filter(|r| bare(&r.path) == bare(&req.path)).count();
                let (status, body) = answer(&req, n);
                log.lock().unwrap().push(req);
                tokio::time::sleep(Duration::from_millis(30)).await;
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (base, reqs)
}

async fn read_request(sock: &mut TcpStream) -> Option<Req> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = sock.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        let Some(h) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { continue };
        let head = String::from_utf8_lossy(&buf[..h]).to_string();
        let header = |name: &str| {
            head.lines()
                .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string()))
                .unwrap_or_default()
        };
        let len = header("content-length").parse::<usize>().unwrap_or(0);
        if buf.len() < h + 4 + len {
            continue;
        }
        let mut first = head.lines().next().unwrap_or("").split_whitespace();
        let method = first.next().unwrap_or("").to_string();
        let path = first.next().unwrap_or("").to_string();
        let body = serde_json::from_slice(&buf[h + 4..h + 4 + len]).unwrap_or(Value::Null);
        return Some(Req { method, path, auth: header("authorization"), body });
    }
}

fn hits(reqs: &Reqs, f: impl Fn(&Req) -> bool) -> Vec<Req> {
    reqs.lock().unwrap().iter().filter(|r| f(r)).cloned().collect()
}

type Ws = WebSocketStream<TcpStream>;

/// A WebSocket stand-in: every connection is handed to the test.
async fn ws_server() -> (String, mpsc::UnboundedReceiver<Ws>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { return };
            if let Ok(ws) = tokio_tungstenite::accept_async(sock).await {
                if tx.send(ws).is_err() {
                    return;
                }
            }
        }
    });
    (url, rx)
}

async fn next_conn(rx: &mut mpsc::UnboundedReceiver<Ws>) -> Ws {
    tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.expect("no connection").expect("server gone")
}

async fn recv_json(ws: &mut Ws) -> Value {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next()).await.expect("nothing sent").expect("closed").unwrap();
        if let WsMessage::Text(t) = m {
            return serde_json::from_str(t.as_str()).unwrap();
        }
    }
}

async fn send_json(ws: &mut Ws, v: Value) {
    ws.send(WsMessage::Text(v.to_string().into())).await.unwrap();
}

fn buttons(n: usize) -> Vec<Button> {
    let mut b: Vec<Button> = (0..n).map(|i| Button { label: format!("choice {i} {}", "x".repeat(100)), data: format!("d:{i}") }).collect();
    b.push(Button { label: "too long".into(), data: "y".repeat(BUTTON_DATA_MAX + 1) });
    b
}

// ------------------------------------------------------------ settings

#[test]
fn only_channels_switched_on_with_what_they_need_connect_and_a_bot_connects_once() {
    let base = Settings::from_env();
    let mut rt = crate::settings_store::RuntimeSettings::from_base(&base);
    let ch = |id: &str, kind: &str, target: &str, control: bool| crate::notify::Channel {
        id: id.into(), kind: kind.into(), enabled: true, token: "test-token".into(), target: target.into(), control, ..Default::default()
    };
    rt.notify_channels = vec![
        ch("a", "discord", "123", true),
        ch("b", "discord", "456", true),
        ch("c", "discord", "789", false),
        // Slack without the app-level token, Matrix without a homeserver: nothing to connect with.
        ch("d", "slack", "C1", true),
        ch("e", "matrix", "!r:example.org", true),
    ];
    let d = link::accounts(&rt, "discord");
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].chats, vec![("a".to_string(), "123".to_string()), ("b".to_string(), "456".to_string())]);
    assert!(link::accounts(&rt, "slack").is_empty());
    assert!(link::accounts(&rt, "matrix").is_empty());
    rt.notify_channels[3].app_token = "xapp-test".into();
    rt.notify_channels[4].url = "https://matrix.example.org/".into();
    assert_eq!(link::accounts(&rt, "slack")[0].app_token, "xapp-test");
    assert_eq!(link::accounts(&rt, "matrix")[0].url, "https://matrix.example.org");
}

#[test]
fn the_app_level_token_is_a_secret_like_the_others() {
    let c = crate::notify::Channel {
        id: "s1".into(), kind: "slack".into(), token: "xoxb-SECRETBOT123".into(), app_token: "xapp-SECRETAPP123".into(), target: "C1".into(),
        ..Default::default()
    };
    let v = c.masked();
    let text = v.to_string();
    assert!(!text.contains("SECRETAPP123") && !text.contains("SECRETBOT123"), "{text}");
    assert_eq!(v["app_token_set"], true);
    assert_eq!(v["app_token"], "");
    // Coming back empty keeps it; another kind does not carry one.
    let back = crate::notify::Channel { app_token: String::new(), token: String::new(), ..c.clone() };
    let merged = crate::notify::merge_channels(std::slice::from_ref(&c), vec![back]);
    assert_eq!((merged[0].token.as_str(), merged[0].app_token.as_str()), ("xoxb-SECRETBOT123", "xapp-SECRETAPP123"));
    let other = crate::notify::Channel { id: "x".into(), kind: "discord".into(), app_token: "xapp-SECRETAPP123".into(), ..Default::default() };
    assert_eq!(crate::notify::merge_channels(&[], vec![other])[0].app_token, "");
    // Commands are kept for the new kinds.
    let on = crate::notify::Channel { id: "m".into(), kind: "matrix".into(), control: true, ..Default::default() };
    assert!(crate::notify::merge_channels(&[], vec![on])[0].control);
}

// ------------------------------------------------------------ Discord

fn discord_msg(author: &str, content: &str, guild: bool) -> Value {
    let mut d = json!({"id": "500", "type": 0, "channel_id": "123", "author": {"id": author}, "content": content, "mentions": []});
    if guild {
        d["guild_id"] = json!("1");
    }
    d
}

#[test]
fn discord_messages_and_presses_become_inbounds() {
    let a = acct("discord", "123");
    let mut d = discord_msg("111", "<@999> calls today", true);
    d["mentions"] = json!([{"id": "999"}]);
    let m = discord::inbound_message(&a, "999", &d).unwrap();
    assert_eq!(
        m,
        Inbound {
            channel_id: "c1".into(), platform: "discord".into(), chat_id: "123".into(), sender_id: "111".into(),
            body: Body::Text("calls today".into()), message_id: "500".into(), reply_to_bot: false,
        }
    );
    let mut r = discord_msg("111", "yes", true);
    r["type"] = json!(19);
    r["referenced_message"] = json!({"id": "400", "author": {"id": "999", "bot": true}});
    assert!(discord::inbound_message(&a, "999", &r).unwrap().reply_to_bot);
    let press = json!({"type": 3, "id": "i1", "token": "itok", "channel_id": "123", "member": {"user": {"id": "111"}},
        "application_id": "app1", "data": {"custom_id": "ok:1"}, "message": {"id": "400", "author": {"id": "999"}}});
    let (m, (id, token)) = discord::inbound_press(&a, "app1", "999", &press).unwrap();
    assert_eq!((m.body, m.message_id.as_str(), m.sender_id.as_str(), m.reply_to_bot), (Body::Button("ok:1".into()), "400", "111", true));
    assert_eq!((m.platform.as_str(), m.chat_id.as_str()), ("discord", "123"));
    assert_eq!((id.as_str(), token.as_str()), ("i1", "itok"));
}

/// A press that names another application, or sits under a message the bot
/// did not send, is forged or not ours: dropped.
#[test]
fn discord_drops_presses_for_another_application_or_message() {
    let a = acct("discord", "123");
    let press = json!({"type": 3, "id": "i1", "token": "itok", "channel_id": "123", "member": {"user": {"id": "111"}},
        "application_id": "app1", "data": {"custom_id": "k:X:y"}, "message": {"id": "400", "author": {"id": "999"}}});
    assert!(discord::inbound_press(&a, "app1", "999", &press).is_some());
    let mut other_app = press.clone();
    other_app["application_id"] = json!("app2");
    assert!(discord::inbound_press(&a, "app1", "999", &other_app).is_none());
    let mut other_message = press.clone();
    other_message["message"]["author"]["id"] = json!("888");
    assert!(discord::inbound_press(&a, "app1", "999", &other_message).is_none());
    let mut no_app = press.clone();
    no_app.as_object_mut().unwrap().remove("application_id");
    assert!(discord::inbound_press(&a, "app1", "999", &no_app).is_none());
    // Before READY told the ids, nothing is taken.
    assert!(discord::inbound_press(&a, "", "", &press).is_none());
}

#[test]
fn discord_ignores_the_bot_itself_other_bots_forwards_and_other_channels() {
    let a = acct("discord", "123");
    assert!(discord::inbound_message(&a, "999", &discord_msg("999", "/status", false)).is_none());
    let mut other_bot = discord_msg("222", "/status", false);
    other_bot["author"]["bot"] = json!(true);
    assert!(discord::inbound_message(&a, "999", &other_bot).is_none());
    let mut forward = discord_msg("111", "/status", false);
    forward["message_reference"] = json!({"type": 1, "message_id": "1"});
    assert!(discord::inbound_message(&a, "999", &forward).is_none());
    let mut elsewhere = discord_msg("111", "/status", false);
    elsewhere["channel_id"] = json!("456");
    assert!(discord::inbound_message(&a, "999", &elsewhere).is_none());
    let mut bot_press = json!({"type": 3, "id": "i", "token": "t", "channel_id": "123", "user": {"id": "222", "bot": true}, "data": {"custom_id": "x"},
        "application_id": "app1", "message": {"id": "400", "author": {"id": "999"}}});
    assert!(discord::inbound_press(&a, "app1", "999", &bot_press).is_none());
    bot_press["user"]["bot"] = json!(false);
    assert!(discord::inbound_press(&a, "app1", "999", &bot_press).is_some());
    bot_press["channel_id"] = json!("456");
    assert!(discord::inbound_press(&a, "app1", "999", &bot_press).is_none());
}

#[test]
fn discord_in_a_server_passes_on_only_what_is_said_to_the_bot() {
    let a = acct("discord", "123");
    assert!(discord::inbound_message(&a, "999", &discord_msg("111", "see you at 7", true)).is_none());
    assert!(discord::inbound_message(&a, "999", &discord_msg("111", "<@888> see you", true)).is_none());
    assert!(discord::inbound_message(&a, "999", &discord_msg("111", "/status", true)).is_some());
    assert!(discord::inbound_message(&a, "999", &discord_msg("111", "<@!999> status", true)).is_some());
    // A direct message is for the bot.
    assert!(discord::inbound_message(&a, "999", &discord_msg("111", "see you at 7", false)).is_some());
}

#[tokio::test]
async fn discord_connects_again_and_resumes_after_the_line_drops() {
    let rig = Rig::new();
    let (url, mut conns) = ws_server().await;
    let ep = discord::Endpoints { api: "http://127.0.0.1:9".into(), gateway: url.clone(), retry: FAST };
    let task = tokio::spawn(discord::run(Controller::new(rig.state.clone()), reqwest::Client::new(), ep, acct("discord", "123")));
    let mut ws = next_conn(&mut conns).await;
    send_json(&mut ws, json!({"op": 10, "d": {"heartbeat_interval": 60000}})).await;
    let hi = recv_json(&mut ws).await;
    assert_eq!((hi["op"].as_u64(), hi["d"]["token"].as_str()), (Some(2), Some("test-token")));
    assert_ne!(hi["d"]["intents"].as_u64().unwrap() & (1 << 15), 0, "message content intent");
    send_json(&mut ws, json!({"op": 0, "s": 1, "t": "READY", "d": {"session_id": "s1", "resume_gateway_url": url, "user": {"id": "999"}}})).await;
    ws.close(None).await.unwrap();
    drop(ws);
    let mut again = next_conn(&mut conns).await;
    send_json(&mut again, json!({"op": 10, "d": {"heartbeat_interval": 60000}})).await;
    let resume = recv_json(&mut again).await;
    task.abort();
    assert_eq!(resume["op"], 6, "{resume}");
    assert_eq!((resume["d"]["session_id"].as_str(), resume["d"]["seq"].as_u64()), (Some("s1"), Some(1)));
}

#[test]
fn discord_resumes_only_at_discords_own_address_or_the_configured_one() {
    let gw = discord::GATEWAY;
    assert!(discord::trusted_resume("wss://gateway-us-east1-b.discord.gg", gw));
    assert!(discord::trusted_resume("wss://discord.gg/?v=10", gw));
    assert!(!discord::trusted_resume("ws://gateway.discord.gg", gw), "not encrypted");
    assert!(!discord::trusted_resume("wss://discord.gg.evil.example", gw));
    assert!(!discord::trusted_resume("wss://evil-discord.gg", gw));
    assert!(!discord::trusted_resume("not a url", gw));
    assert!(discord::trusted_resume("ws://127.0.0.1:4000/x", "ws://127.0.0.1:4000"));
    assert!(!discord::trusted_resume("ws://127.0.0.1:4001", "ws://127.0.0.1:4000"));
}

/// READY names a resume address elsewhere: the token is never sent there;
/// the next connection goes to the configured gateway and identifies afresh.
#[tokio::test]
async fn discord_ignores_a_foreign_resume_address_and_identifies_afresh() {
    let rig = Rig::new();
    let (url, mut conns) = ws_server().await;
    let ep = discord::Endpoints { api: "http://127.0.0.1:9".into(), gateway: url.clone(), retry: FAST };
    let task = tokio::spawn(discord::run(Controller::new(rig.state.clone()), reqwest::Client::new(), ep, acct("discord", "123")));
    let mut ws = next_conn(&mut conns).await;
    send_json(&mut ws, json!({"op": 10, "d": {"heartbeat_interval": 60000}})).await;
    let _identify = recv_json(&mut ws).await;
    send_json(&mut ws, json!({"op": 0, "s": 1, "t": "READY", "d": {"session_id": "s1", "resume_gateway_url": "wss://evil.example", "user": {"id": "999"}}})).await;
    ws.close(None).await.unwrap();
    drop(ws);
    let mut again = next_conn(&mut conns).await;
    send_json(&mut again, json!({"op": 10, "d": {"heartbeat_interval": 60000}})).await;
    let first = recv_json(&mut again).await;
    task.abort();
    assert_eq!(first["op"], 2, "{first}");
}

async fn discord_live(rig: &Rig) -> (Reqs, Ws, tokio::task::JoinHandle<()>) {
    let (api, reqs) = http_server(|_, _| (200, "{}".into())).await;
    let (url, mut conns) = ws_server().await;
    let ep = discord::Endpoints { api, gateway: url, retry: FAST };
    let task = tokio::spawn(discord::run(Controller::new(rig.state.clone()), reqwest::Client::new(), ep, rig.account("discord")));
    let mut ws = next_conn(&mut conns).await;
    send_json(&mut ws, json!({"op": 10, "d": {"heartbeat_interval": 60000}})).await;
    let _identify = recv_json(&mut ws).await;
    send_json(&mut ws, json!({"op": 0, "s": 1, "t": "READY", "d": {"session_id": "s1", "resume_gateway_url": "", "user": {"id": "999"}, "application": {"id": "app1"}}})).await;
    (reqs, ws, task)
}

fn press_event(sender: &str, app: &str) -> Value {
    json!({"op": 0, "s": 3, "t": "INTERACTION_CREATE", "d": {"type": 3, "id": "i1", "token": "itok", "channel_id": "123", "application_id": app,
        "member": {"user": {"id": sender}}, "data": {"custom_id": "x:1"}, "message": {"id": "500", "author": {"id": "999"}}}})
}

#[tokio::test]
async fn discord_answers_the_listed_sender_and_acknowledges_the_press() {
    let rig = Rig::new();
    rig.channels("https://matrix.example.org", true);
    let (reqs, mut ws, task) = discord_live(&rig).await;
    send_json(&mut ws, json!({"op": 0, "s": 2, "t": "MESSAGE_CREATE", "d": discord_msg("111", "/help", true)})).await;
    let posted = until(Duration::from_secs(5), || !hits(&reqs, |r| r.method == "POST" && r.path == "/channels/123/messages").is_empty()).await;
    send_json(&mut ws, press_event("111", "app1")).await;
    let acked = until(Duration::from_secs(5), || !hits(&reqs, |r| r.path == "/interactions/i1/itok/callback").is_empty()).await;
    task.abort();
    assert!(posted, "no answer: {:?}", reqs.lock().unwrap());
    assert!(acked, "press not acknowledged");
    let sent = hits(&reqs, |r| r.path == "/channels/123/messages").remove(0);
    assert_eq!(sent.auth, "Bot test-token-discord");
    let text = sent.body["content"].as_str().unwrap();
    assert!(!text.is_empty() && text.chars().count() <= discord::TEXT_MAX);
    assert_eq!(sent.body["allowed_mentions"]["parse"], json!([]));
}

#[tokio::test]
async fn discord_keeps_to_its_limits() {
    let (api, reqs) = http_server(|_, _| (200, "{}".into())).await;
    let out = Outbound { text: "字".repeat(5000), buttons: buttons(30), edit: None };
    discord::send(&reqwest::Client::new(), &api, "test-token", "123", &out).await.unwrap();
    let body = hits(&reqs, |_| true).remove(0).body;
    assert!(body["content"].as_str().unwrap().chars().count() <= discord::TEXT_MAX);
    let rows = body["components"].as_array().unwrap();
    assert!(rows.len() <= 5 && rows.iter().all(|r| r["components"].as_array().unwrap().len() <= 5));
    let all: Vec<&Value> = rows.iter().flat_map(|r| r["components"].as_array().unwrap()).collect();
    assert_eq!(all.len(), 25);
    assert!(all.iter().all(|b| b["custom_id"].as_str().unwrap().len() <= BUTTON_DATA_MAX && b["label"].as_str().unwrap().chars().count() <= 80));
    // An edit goes to that message.
    let edit = Outbound { text: "done".into(), buttons: Vec::new(), edit: Some("400".into()) };
    discord::send(&reqwest::Client::new(), &api, "test-token", "123", &edit).await.unwrap();
    assert!(!hits(&reqs, |r| r.method == "PATCH" && r.path == "/channels/123/messages/400").is_empty());
}

// ------------------------------------------------------------ Slack

fn slack_me() -> slack::Me {
    slack::Me { user_id: "UBOT".into(), bot_id: "BBOT".into() }
}

fn slack_msg(user: &str, text: &str, channel_type: &str) -> Value {
    json!({"type": "message", "channel": "C123", "user": user, "text": text, "ts": "1.1", "channel_type": channel_type})
}

#[test]
fn slack_messages_and_presses_become_inbounds() {
    let a = acct("slack", "C123");
    let m = slack::inbound_event(&a, &slack_me(), &slack_msg("U111", "<@UBOT> calls &lt;today&gt;", "channel")).unwrap();
    assert_eq!(
        m,
        Inbound {
            channel_id: "c1".into(), platform: "slack".into(), chat_id: "C123".into(), sender_id: "U111".into(),
            body: Body::Text("calls <today>".into()), message_id: "1.1".into(), reply_to_bot: false,
        }
    );
    let mut r = slack_msg("U111", "yes", "channel");
    r["thread_ts"] = json!("0.9");
    r["parent_user_id"] = json!("UBOT");
    assert!(slack::inbound_event(&a, &slack_me(), &r).unwrap().reply_to_bot);
    let press = json!({"type": "block_actions", "user": {"id": "U111"}, "channel": {"id": "C123"},
        "container": {"message_ts": "0.9", "channel_id": "C123"}, "actions": [{"action_id": "bageo_0", "value": "ok:1"}]});
    let m = slack::inbound_action(&a, &slack_me(), &press).unwrap();
    assert_eq!((m.body, m.message_id.as_str(), m.sender_id.as_str(), m.reply_to_bot), (Body::Button("ok:1".into()), "0.9", "U111", true));
}

#[test]
fn slack_ignores_the_bot_itself_other_bots_changes_and_other_channels() {
    let a = acct("slack", "C123");
    assert!(slack::inbound_event(&a, &slack_me(), &slack_msg("UBOT", "/status", "im")).is_none());
    let mut bot = slack_msg("U222", "/status", "im");
    bot["bot_id"] = json!("B222");
    assert!(slack::inbound_event(&a, &slack_me(), &bot).is_none());
    let mut changed = slack_msg("U111", "/status", "im");
    changed["subtype"] = json!("message_changed");
    assert!(slack::inbound_event(&a, &slack_me(), &changed).is_none());
    let mut elsewhere = slack_msg("U111", "/status", "im");
    elsewhere["channel"] = json!("C999");
    assert!(slack::inbound_event(&a, &slack_me(), &elsewhere).is_none());
    let own_press = json!({"type": "block_actions", "user": {"id": "UBOT"}, "channel": {"id": "C123"}, "actions": [{"value": "x"}]});
    assert!(slack::inbound_action(&a, &slack_me(), &own_press).is_none());
}

#[test]
fn slack_in_a_channel_passes_on_only_what_is_said_to_the_bot() {
    let a = acct("slack", "C123");
    assert!(slack::inbound_event(&a, &slack_me(), &slack_msg("U111", "see you at 7", "channel")).is_none());
    assert!(slack::inbound_event(&a, &slack_me(), &slack_msg("U111", "<@U999> see you", "channel")).is_none());
    assert!(slack::inbound_event(&a, &slack_me(), &slack_msg("U111", "/status", "channel")).is_some());
    // The event's own word that it is a direct message is not trusted.
    assert!(slack::inbound_event(&a, &slack_me(), &slack_msg("U111", "see you at 7", "im")).is_none());
    // A direct message channel (D…) is for the bot.
    let dm = acct("slack", "D123");
    let mut m = slack_msg("U111", "see you at 7", "channel");
    m["channel"] = json!("D123");
    assert!(slack::inbound_event(&dm, &slack_me(), &m).is_some());
}

async fn slack_live(rig: &Rig) -> (Reqs, mpsc::UnboundedReceiver<Ws>, tokio::task::JoinHandle<()>) {
    let (url, conns) = ws_server().await;
    let (api, reqs) = http_server(move |r, _| match r.path.as_str() {
        "/auth.test" => (200, json!({"ok": true, "user_id": "UBOT", "bot_id": "BBOT"}).to_string()),
        "/apps.connections.open" => (200, json!({"ok": true, "url": url}).to_string()),
        _ => (200, json!({"ok": true, "ts": "2.2"}).to_string()),
    })
    .await;
    let ep = slack::Endpoints { api, retry: FAST };
    let task = tokio::spawn(slack::run(Controller::new(rig.state.clone()), reqwest::Client::new(), ep, rig.account("slack")));
    (reqs, conns, task)
}

#[tokio::test]
async fn slack_acknowledges_envelopes_answers_and_connects_again() {
    let rig = Rig::new();
    rig.channels("https://matrix.example.org", true);
    let (reqs, mut conns, task) = slack_live(&rig).await;
    let mut ws = next_conn(&mut conns).await;
    send_json(&mut ws, json!({"type": "hello"})).await;
    send_json(&mut ws, json!({"envelope_id": "e1", "type": "events_api",
        "payload": {"event": slack_msg("U111", "<@UBOT> /help", "channel")}})).await;
    let ack = recv_json(&mut ws).await;
    let posted = until(Duration::from_secs(5), || !hits(&reqs, |r| r.path == "/chat.postMessage").is_empty()).await;
    send_json(&mut ws, json!({"type": "disconnect", "reason": "refresh_requested"})).await;
    let again = tokio::time::timeout(Duration::from_secs(5), conns.recv()).await;
    task.abort();
    assert_eq!(ack, json!({"envelope_id": "e1"}));
    assert!(posted, "no answer: {:?}", reqs.lock().unwrap());
    assert!(matches!(again, Ok(Some(_))), "did not connect again");
    assert_eq!(hits(&reqs, |r| r.path == "/auth.test")[0].auth, "Bearer xoxb-test-token");
    assert_eq!(hits(&reqs, |r| r.path == "/apps.connections.open")[0].auth, "Bearer xapp-test-token");
    let sent = hits(&reqs, |r| r.path == "/chat.postMessage").remove(0);
    assert_eq!(sent.body["channel"], "C123");
    assert!(sent.body["text"].as_str().unwrap().chars().count() <= slack::TEXT_MAX);
}

#[tokio::test]
async fn slack_keeps_to_its_limits() {
    let (api, reqs) = http_server(|_, _| (200, json!({"ok": true}).to_string())).await;
    let out = Outbound { text: "字".repeat(5000), buttons: buttons(30), edit: Some("0.9".into()) };
    slack::send(&reqwest::Client::new(), &api, "xoxb-test", "C123", &out).await.unwrap();
    let sent = hits(&reqs, |r| r.path == "/chat.update").remove(0).body;
    assert_eq!(sent["ts"], "0.9");
    assert!(sent["text"].as_str().unwrap().chars().count() <= slack::TEXT_MAX);
    assert!(sent["blocks"][0]["text"]["text"].as_str().unwrap().chars().count() <= slack::TEXT_MAX);
    let elements = sent["blocks"][1]["elements"].as_array().unwrap();
    assert_eq!(elements.len(), 25);
    assert!(elements.iter().all(|e| e["value"].as_str().unwrap().len() <= BUTTON_DATA_MAX && e["text"]["text"].as_str().unwrap().chars().count() <= 75));
}

// ------------------------------------------------------------ Matrix

fn room(direct: bool) -> matrix::Room {
    matrix::Room { channel_id: "c1".into(), target: "!room:example.org".into(), id: "!room:example.org".into(), direct }
}

fn matrix_me() -> matrix::Me {
    matrix::Me { user_id: "@bot:example.org".into(), name: "Bageo".into() }
}

fn matrix_msg(sender: &str, body: &str) -> Value {
    json!({"type": "m.room.message", "event_id": "$e1", "sender": sender, "content": {"msgtype": "m.text", "body": body}})
}

#[test]
fn matrix_messages_become_inbounds() {
    let mut m = matrix_msg("@lee:example.org", "Bageo: calls today");
    m["content"]["m.mentions"] = json!({"user_ids": ["@bot:example.org"]});
    let got = matrix::inbound_event(&room(false), &matrix_me(), |_| false, &m).unwrap();
    assert_eq!(
        got,
        Inbound {
            channel_id: "c1".into(), platform: "matrix".into(), chat_id: "!room:example.org".into(), sender_id: "@lee:example.org".into(),
            body: Body::Text("calls today".into()), message_id: "$e1".into(), reply_to_bot: false,
        }
    );
    let mut reply = matrix_msg("@lee:example.org", "> <@bot:example.org> Call Lee?\n\n1");
    reply["content"]["m.relates_to"] = json!({"m.in_reply_to": {"event_id": "$bot1"}});
    let got = matrix::inbound_event(&room(false), &matrix_me(), |id| id == "$bot1", &reply).unwrap();
    assert_eq!((got.body, got.reply_to_bot), (Body::Text("1".into()), true));
}

#[test]
fn matrix_ignores_the_bot_itself_notices_and_edits() {
    assert!(matrix::inbound_event(&room(true), &matrix_me(), |_| false, &matrix_msg("@bot:example.org", "/status")).is_none());
    let mut notice = matrix_msg("@other-bot:example.org", "/status");
    notice["content"]["msgtype"] = json!("m.notice");
    assert!(matrix::inbound_event(&room(true), &matrix_me(), |_| false, &notice).is_none());
    let mut edit = matrix_msg("@lee:example.org", "* /status");
    edit["content"]["m.new_content"] = json!({"msgtype": "m.text", "body": "/status"});
    assert!(matrix::inbound_event(&room(true), &matrix_me(), |_| false, &edit).is_none());
}

#[test]
fn matrix_in_a_group_room_passes_on_only_what_is_said_to_the_bot() {
    let plain = matrix_msg("@lee:example.org", "see you at 7");
    assert!(matrix::inbound_event(&room(false), &matrix_me(), |_| false, &plain).is_none());
    assert!(matrix::inbound_event(&room(false), &matrix_me(), |_| false, &matrix_msg("@lee:example.org", "Bageolicious")).is_none());
    assert!(matrix::inbound_event(&room(false), &matrix_me(), |_| false, &matrix_msg("@lee:example.org", "/status")).is_some());
    // A reply to someone else is not to the bot.
    let mut other = matrix_msg("@lee:example.org", "ok");
    other["content"]["m.relates_to"] = json!({"m.in_reply_to": {"event_id": "$someone"}});
    assert!(matrix::inbound_event(&room(false), &matrix_me(), |_| false, &other).is_none());
    // Two members: a direct chat.
    assert!(matrix::inbound_event(&room(true), &matrix_me(), |_| false, &plain).is_some());
}

/// A homeserver whose second /sync fails; the third brings sender's "/help".
async fn matrix_server(sender: &'static str) -> (String, Reqs) {
    http_server(move |r, n| {
        let p = bare(&r.path);
        if p.ends_with("/account/whoami") {
            (200, json!({"user_id": "@bot:example.org"}).to_string())
        } else if p.ends_with("/displayname") {
            (200, json!({"displayname": "Bageo"}).to_string())
        } else if p.ends_with("/joined_members") {
            (200, json!({"joined": {"@bot:example.org": {}, "@lee:example.org": {}, "@wang:example.org": {}}}).to_string())
        } else if p.ends_with("/sync") {
            match n {
                0 => (200, json!({"next_batch": "b1"}).to_string()),
                1 => (502, "{}".to_string()),
                2 => (200, json!({"next_batch": "b2", "rooms": {"join": {"!room:example.org": {"timeline": {"events": [
                    {"type": "m.room.message", "event_id": "$e1", "sender": sender, "content": {"msgtype": "m.text", "body": "/help"}}
                ]}}}}}).to_string()),
                _ => (200, json!({"next_batch": "b3"}).to_string()),
            }
        } else {
            (200, json!({"event_id": "$bot1"}).to_string())
        }
    })
    .await
}

#[tokio::test]
async fn matrix_polls_again_after_an_error_and_answers_in_the_room() {
    let rig = Rig::new();
    let (base, reqs) = matrix_server("@lee:example.org").await;
    rig.channels(&base, true);
    let ep = matrix::Endpoints { retry: FAST, poll: Duration::from_millis(100) };
    let task = tokio::spawn(matrix::run(Controller::new(rig.state.clone()), reqwest::Client::new(), ep, rig.account("matrix")));
    let sent = until(Duration::from_secs(5), || !hits(&reqs, |r| r.method == "PUT").is_empty()).await;
    task.abort();
    assert!(sent, "no answer: {:?}", reqs.lock().unwrap());
    let syncs = hits(&reqs, |r| bare(&r.path).ends_with("/sync"));
    assert!(syncs.len() >= 3, "did not poll again after the error");
    assert!(syncs[2].path.contains("since=b1"), "{}", syncs[2].path);
    let put = hits(&reqs, |r| r.method == "PUT").remove(0);
    assert!(put.path.contains("/rooms/") && put.path.contains("/send/m.room.message/"), "{}", put.path);
    assert_eq!(put.auth, "Bearer test-token-matrix");
    let text = put.body["body"].as_str().unwrap();
    assert!(!text.is_empty() && text.chars().count() <= matrix::TEXT_MAX);
}

#[tokio::test]
async fn matrix_keeps_to_its_limit() {
    let (base, reqs) = http_server(|_, _| (200, json!({"event_id": "$bot9"}).to_string())).await;
    let a = Account { url: base, ..acct("matrix", "!room:example.org") };
    let sent: matrix::Sent = Default::default();
    let out = Outbound { text: "字".repeat(5000), buttons: buttons(3), edit: None };
    matrix::send(&reqwest::Client::new(), &a, "!room:example.org", &out, &sent).await.unwrap();
    let body = hits(&reqs, |_| true).remove(0).body;
    assert!(body["body"].as_str().unwrap().chars().count() <= matrix::TEXT_MAX);
    assert_eq!(body.as_object().unwrap().len(), 2, "only the text: {body}");
    assert_eq!(sent.lock().unwrap().back().map(String::as_str), Some("$bot9"));
}

// ------------------------------------------------------------ secrets

#[tokio::test]
async fn errors_and_log_lines_carry_no_token() {
    // A service that echoes the token back in its error.
    let (api, _) = http_server(|r, _| (401, json!({"message": format!("bad {}", r.auth), "errcode": r.auth, "error": r.auth}).to_string())).await;
    let http = reqwest::Client::new();
    let out = Outbound { text: "hi".into(), ..Default::default() };
    let d = Account { token: "test-token-SECRET1".into(), ..acct("discord", "123") };
    let s = Account { token: "xoxb-SECRET2".into(), app_token: "xapp-SECRET3".into(), ..acct("slack", "C123") };
    let m = Account { url: api.clone(), token: "test-token-SECRET4".into(), ..acct("matrix", "!room:example.org") };
    let errors = vec![
        d.redact(&format!("{:#}", discord::send(&http, &api, &d.token, "123", &out).await.unwrap_err())),
        s.redact(&format!("{:#}", slack::send(&http, &api, &s.token, "C123", &out).await.unwrap_err())),
        m.redact(&format!("{:#}", matrix::send(&http, &m, "!room:example.org", &out, &Default::default()).await.unwrap_err())),
        // Nobody listening: the request error names no address with a token in it.
        format!("{:#}", discord::send(&http, "http://127.0.0.1:9", &d.token, "123", &out).await.unwrap_err()),
        format!("{:#}", slack::send(&http, "http://127.0.0.1:9", &s.token, "C123", &out).await.unwrap_err()),
    ];
    for e in &errors {
        assert!(!e.contains("SECRET"), "{e}");
    }
    let url_form = s.redact("wss://example.org/?t=xapp-SECRET3&u=xoxb-SECRET2");
    assert!(!url_form.contains("SECRET"), "{url_form}");
}

// ------------------------------------------------------------ no answer

/// Commands off for a channel, or a sender not on its list: no connection,
/// and no answer of any kind (no message, no acknowledged press).
#[tokio::test]
async fn sec_no_answer_at_all_when_commands_are_off_or_the_sender_is_not_listed() {
    let rig = Rig::new();
    rig.channels("https://matrix.example.org", false);
    let rt = rig.state.settings_store.snapshot();
    for kind in ["discord", "slack", "matrix"] {
        assert!(link::accounts(&rt, kind).is_empty(), "{kind} connects with commands off");
    }
    rig.channels("https://matrix.example.org", true);

    // Discord: a stranger's command and press.
    let (reqs, mut ws, task) = discord_live(&rig).await;
    send_json(&mut ws, json!({"op": 0, "s": 2, "t": "MESSAGE_CREATE", "d": discord_msg("666", "/help", true)})).await;
    send_json(&mut ws, press_event("666", "app1")).await;
    // The listed sender, but the press names another application: forged.
    send_json(&mut ws, press_event("111", "app-forged")).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    task.abort();
    assert!(hits(&reqs, |_| true).is_empty(), "discord answered a stranger or a forged press: {:?}", reqs.lock().unwrap());

    // Slack.
    let (reqs, mut conns, task) = slack_live(&rig).await;
    let mut ws = next_conn(&mut conns).await;
    send_json(&mut ws, json!({"envelope_id": "e1", "type": "events_api", "payload": {"event": slack_msg("U666", "/help", "im")}})).await;
    let _ack = recv_json(&mut ws).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    task.abort();
    assert!(hits(&reqs, |r| r.path.starts_with("/chat.")).is_empty(), "slack answered a stranger");

    // Matrix.
    let (base, reqs) = matrix_server("@stranger:example.org").await;
    rig.channels(&base, true);
    let ep = matrix::Endpoints { retry: FAST, poll: Duration::from_millis(100) };
    let task = tokio::spawn(matrix::run(Controller::new(rig.state.clone()), reqwest::Client::new(), ep, rig.account("matrix")));
    let read = until(Duration::from_secs(5), || hits(&reqs, |r| bare(&r.path).ends_with("/sync")).len() >= 4).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    task.abort();
    assert!(read, "the message was never read");
    assert!(hits(&reqs, |r| r.method == "PUT").is_empty(), "matrix answered a stranger");
}

/// Every press from someone allowed counts against the chat's limit, a
/// wrong code too: past it nothing is answered, the right code neither.
/// A stranger's presses are dropped before they count.
#[tokio::test]
async fn sec_wrong_confirmation_codes_count_against_the_limit_and_strangers_use_none_of_it() {
    let rig = Rig::new();
    rig.channels("https://matrix.example.org", true);
    rig.set(json!({"control_commands_per_minute": 3}));
    let c = Controller::new(rig.state.clone());
    let press = |sender: &str, data: String| Inbound {
        channel_id: "d1".into(), platform: "discord".into(), chat_id: "123".into(), sender_id: sender.into(),
        body: Body::Button(data), message_id: "500".into(), reply_to_bot: true,
    };
    for _ in 0..10 {
        assert_eq!(c.handle(&press("666", format!("k:{}:y", confirm::new_code()))).await, None);
    }
    for i in 0..3 {
        assert!(c.handle(&press("111", format!("k:{}:y", confirm::new_code()))).await.is_some(), "press {i} within the limit");
    }
    assert_eq!(c.handle(&press("111", format!("k:{}:y", confirm::new_code()))).await, None, "a guess past the limit was answered");
}
// ------------------------------------------------------------ review additions

/// Counts what the supervisor starts: how many tasks it started, how many are
/// alive now, and the token each was started with.
struct Lines {
    started: Arc<std::sync::atomic::AtomicUsize>,
    alive: Arc<std::sync::atomic::AtomicUsize>,
    tokens: Arc<Mutex<Vec<String>>>,
}

struct Alive(Arc<std::sync::atomic::AtomicUsize>);

impl Drop for Alive {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

fn supervise_counting(rig: &Rig, kind: &'static str) -> (Lines, tokio::task::JoinHandle<()>) {
    let lines = Lines { started: Default::default(), alive: Default::default(), tokens: Default::default() };
    let (s0, a0, t0) = (lines.started.clone(), lines.alive.clone(), lines.tokens.clone());
    let handle = link::supervise(kind, rig.state.settings_store.clone(), move |acct: Account| {
        let (s, a, t) = (s0.clone(), a0.clone(), t0.clone());
        async move {
            s.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            t.lock().unwrap().push(acct.token.clone());
            a.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _alive = Alive(a);
            std::future::pending::<()>().await;
        }
    });
    (lines, handle)
}

fn count(n: &Arc<std::sync::atomic::AtomicUsize>) -> usize {
    n.load(std::sync::atomic::Ordering::SeqCst)
}

fn discord_channel(token: &str, control: bool, users: Value) -> Value {
    json!({"notify_channels": [
        {"id": "d1", "kind": "discord", "enabled": true, "url": "https://discord.example/api/webhooks/1/x",
         "token": token, "target": "123", "control": control, "control_users": users},
    ]})
}

/// A changed bot token replaces the connection: the old one is stopped (its
/// task dropped) and exactly one runs, so no message is read twice.
#[tokio::test]
async fn review_a_changed_token_replaces_the_connection_and_never_leaves_two() {
    let rig = Rig::new();
    rig.set(discord_channel("test-token-one", true, json!(["111"])));
    let (lines, handle) = supervise_counting(&rig, "discord");
    assert!(until(Duration::from_secs(5), || count(&lines.alive) == 1).await, "no connection started");
    rig.set(discord_channel("test-token-two", true, json!(["111"])));
    let swapped = until(Duration::from_secs(15), || count(&lines.started) == 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (started, alive, tokens) = (count(&lines.started), count(&lines.alive), lines.tokens.lock().unwrap().clone());
    handle.abort();
    assert!(swapped, "the new token was never connected: {tokens:?}");
    assert_eq!((started, alive), (2, 1), "old connection still alive: {tokens:?}");
    assert_eq!(tokens, vec!["test-token-one".to_string(), "test-token-two".to_string()]);
}

/// Changing only who may give commands keeps the connection (no second one);
/// switching commands off stops it.
#[tokio::test]
async fn review_the_connection_stays_for_a_sender_list_change_and_stops_when_commands_go_off() {
    let rig = Rig::new();
    rig.set(discord_channel("test-token-one", true, json!(["111"])));
    let (lines, handle) = supervise_counting(&rig, "discord");
    assert!(until(Duration::from_secs(5), || count(&lines.alive) == 1).await, "no connection started");
    rig.set(discord_channel("test-token-one", true, json!(["111", "222"])));
    tokio::time::sleep(Duration::from_secs(11)).await;
    assert_eq!((count(&lines.started), count(&lines.alive)), (1, 1), "a sender list change reconnected");
    rig.set(discord_channel("test-token-one", false, json!(["111", "222"])));
    let stopped = until(Duration::from_secs(15), || count(&lines.alive) == 0).await;
    let started = count(&lines.started);
    handle.abort();
    assert!(stopped, "the connection kept running with commands off");
    assert_eq!(started, 1);
}

/// A webhook's message is no command, whether or not the payload says "bot".
#[test]
fn review_discord_ignores_webhook_messages() {
    let a = acct("discord", "123");
    let mut hook = discord_msg("333", "/status", false);
    hook["webhook_id"] = json!("777");
    assert!(discord::inbound_message(&a, "999", &hook).is_none(), "a webhook message was taken as a command");
    let mut hook_in_server = discord_msg("333", "<@999> /status", true);
    hook_in_server["webhook_id"] = json!("777");
    hook_in_server["mentions"] = json!([{"id": "999"}]);
    assert!(discord::inbound_message(&a, "999", &hook_in_server).is_none());
}

#[test]
fn review_the_wait_doubles_to_a_ceiling_and_starts_over_after_a_steady_line() {
    let r = Retry::LIVE;
    let mut wait = r.first;
    let mut seen = Vec::new();
    for _ in 0..8 {
        let pause = r.pause(wait, Duration::from_secs(1), false);
        seen.push(pause.as_secs());
        wait = r.after(pause);
    }
    assert_eq!(seen, vec![2, 4, 8, 16, 32, 64, 120, 120]);
    assert_eq!(r.pause(Duration::from_secs(64), link::STEADY, false), r.first);
    assert_eq!(r.pause(r.first, Duration::from_secs(1), true), r.max);
}

/// Answer every heartbeat: the line stays. Leave one unanswered: the bot
/// connects again and resumes with the last sequence number it saw.
#[tokio::test]
async fn review_discord_heartbeats_carry_the_sequence_and_a_silent_server_means_reconnect() {
    let rig = Rig::new();
    let (url, mut conns) = ws_server().await;
    let ep = discord::Endpoints { api: "http://127.0.0.1:9".into(), gateway: url.clone(), retry: FAST };
    let task = tokio::spawn(discord::run(Controller::new(rig.state.clone()), reqwest::Client::new(), ep, acct("discord", "123")));
    let mut ws = next_conn(&mut conns).await;
    send_json(&mut ws, json!({"op": 10, "d": {"heartbeat_interval": 100}})).await;
    let _identify = recv_json(&mut ws).await;
    send_json(&mut ws, json!({"op": 0, "s": 1, "t": "READY", "d": {"session_id": "s1", "resume_gateway_url": url, "user": {"id": "999"}}})).await;
    send_json(&mut ws, json!({"op": 0, "s": 5, "t": "PRESENCE_UPDATE", "d": {}})).await;
    let beat = recv_json(&mut ws).await;
    assert_eq!((beat["op"].as_u64(), beat["d"].as_u64()), (Some(1), Some(5)), "{beat}");
    // No answer to that heartbeat: the next tick gives the line up.
    let mut again = next_conn(&mut conns).await;
    send_json(&mut again, json!({"op": 10, "d": {"heartbeat_interval": 100}})).await;
    let resume = recv_json(&mut again).await;
    assert_eq!((resume["op"].as_u64(), resume["d"]["seq"].as_u64(), resume["d"]["session_id"].as_str()), (Some(6), Some(5), Some("s1")), "{resume}");
    // Answered heartbeats keep this line.
    for _ in 0..4 {
        let beat = recv_json(&mut again).await;
        assert_eq!(beat["op"], 1, "{beat}");
        send_json(&mut again, json!({"op": 11})).await;
    }
    let third = tokio::time::timeout(Duration::from_millis(300), conns.recv()).await;
    task.abort();
    assert!(third.is_err(), "reconnected although every heartbeat was answered");
}

/// Invalid session: not resumable -> identify afresh; resumable -> resume.
#[tokio::test]
async fn review_discord_invalid_session_identifies_afresh_unless_it_can_be_resumed() {
    let rig = Rig::new();
    let (url, mut conns) = ws_server().await;
    let ep = discord::Endpoints { api: "http://127.0.0.1:9".into(), gateway: url.clone(), retry: FAST };
    let task = tokio::spawn(discord::run(Controller::new(rig.state.clone()), reqwest::Client::new(), ep, acct("discord", "123")));
    let mut ws = next_conn(&mut conns).await;
    send_json(&mut ws, json!({"op": 10, "d": {"heartbeat_interval": 60000}})).await;
    let _identify = recv_json(&mut ws).await;
    send_json(&mut ws, json!({"op": 0, "s": 1, "t": "READY", "d": {"session_id": "s1", "resume_gateway_url": url, "user": {"id": "999"}}})).await;
    send_json(&mut ws, json!({"op": 9, "d": false})).await;
    let mut second = next_conn(&mut conns).await;
    send_json(&mut second, json!({"op": 10, "d": {"heartbeat_interval": 60000}})).await;
    let first_try = recv_json(&mut second).await;
    send_json(&mut second, json!({"op": 0, "s": 1, "t": "READY", "d": {"session_id": "s2", "resume_gateway_url": url, "user": {"id": "999"}}})).await;
    send_json(&mut second, json!({"op": 9, "d": true})).await;
    let mut third = next_conn(&mut conns).await;
    send_json(&mut third, json!({"op": 10, "d": {"heartbeat_interval": 60000}})).await;
    let second_try = recv_json(&mut third).await;
    task.abort();
    assert_eq!(first_try["op"], 2, "not resumable, but it tried to resume: {first_try}");
    assert_eq!((second_try["op"].as_u64(), second_try["d"]["session_id"].as_str()), (Some(6), Some("s2")), "{second_try}");
}

/// When the member count cannot be read the room counts as a group: only
/// what is addressed to the bot is answered.
#[tokio::test]
async fn review_matrix_treats_a_room_as_a_group_when_the_member_count_is_unknown() {
    let rig = Rig::new();
    let (base, reqs) = http_server(|r, n| {
        let p = bare(&r.path);
        if p.ends_with("/account/whoami") {
            (200, json!({"user_id": "@bot:example.org"}).to_string())
        } else if p.ends_with("/displayname") {
            (200, json!({"displayname": "Bageo"}).to_string())
        } else if p.ends_with("/joined_members") {
            (500, "{}".to_string())
        } else if p.ends_with("/sync") {
            if n == 0 {
                (200, json!({"next_batch": "b1"}).to_string())
            } else if n == 1 {
                let ev = |id: &str, body: &str| json!({"type": "m.room.message", "event_id": id, "sender": "@lee:example.org",
                    "content": {"msgtype": "m.text", "body": body}});
                (200, json!({"next_batch": "b2", "rooms": {"join": {"!room:example.org": {"timeline": {"events": [
                    ev("$e1", "see you at 7"), ev("$e2", "/help")
                ]}}}}}).to_string())
            } else {
                (200, json!({"next_batch": "b3"}).to_string())
            }
        } else {
            (200, json!({"event_id": "$bot1"}).to_string())
        }
    })
    .await;
    rig.channels(&base, true);
    let ep = matrix::Endpoints { retry: FAST, poll: Duration::from_millis(100) };
    let task = tokio::spawn(matrix::run(Controller::new(rig.state.clone()), reqwest::Client::new(), ep, rig.account("matrix")));
    let answered = until(Duration::from_secs(5), || !hits(&reqs, |r| r.method == "PUT").is_empty()).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    task.abort();
    assert!(answered, "no answer: {:?}", reqs.lock().unwrap());
    assert_eq!(hits(&reqs, |r| r.method == "PUT").len(), 1, "the plain chat line was answered too");
}
