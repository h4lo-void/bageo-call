//! The MCP interface end to end: off and silent by default, the token and
//! the browser origin, the protocol's methods, which tools each mode offers,
//! and that results carry no key or token.

use std::path::PathBuf;
use std::sync::Arc;

use axum::http::{HeaderMap, HeaderValue, StatusCode};
use serde_json::{json, Value};

use super::*;
use crate::calls_http::{CallsState, PhoneDevices};
use crate::config::Settings;
use crate::settings_store::SettingsStore;

const TOKEN: &str = "mcp-test-token-0123456789";

struct Rig {
    root: PathBuf,
    state: CallsState,
}

impl Rig {
    /// MCP on with a token, calls off, no phones.
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("bageo-mcp-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        let mut base = Settings::from_env();
        base.calls_data_dir = root.to_string_lossy().into_owned();
        base.telegram_bot_token = String::new();
        base.telegram_chat_id = String::new();
        base.gemini_api_key = "".into();
        base.calls_ui_token = "console-secret-token".into();
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
        rig.set(json!({"gemini_api_key_clear": true, "mcp_enabled": true, "mcp_token": TOKEN, "mcp_calls": "off"}));
        rig
    }

    fn set(&self, patch: Value) {
        self.state.settings_store.apply_patch(&patch).unwrap();
    }

    fn controller(&self) -> Arc<Controller> {
        Controller::new(self.state.clone())
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn headers(token: Option<&str>, origin: Option<&str>) -> HeaderMap {
    let mut h = HeaderMap::new();
    if let Some(t) = token {
        h.insert("authorization", HeaderValue::from_str(&format!("Bearer {t}")).unwrap());
    }
    if let Some(o) = origin {
        h.insert("origin", HeaderValue::from_str(o).unwrap());
    }
    h
}

fn rpc(method: &str, params: Value) -> Vec<u8> {
    json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string().into_bytes()
}

async fn ask(c: &Controller, method: &str, params: Value) -> Value {
    let (status, body) = answer(c, &headers(Some(TOKEN), None), &rpc(method, params)).await;
    assert_eq!(status, StatusCode::OK);
    body.unwrap()
}

async fn tool_names(c: &Controller) -> Vec<String> {
    ask(c, "tools/list", json!({})).await["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn it_is_silent_until_enabled_with_a_token_and_needs_that_token() {
    let rig = Rig::new();
    let c = rig.controller();
    let body = rpc("ping", json!({}));
    rig.set(json!({"mcp_enabled": false}));
    assert_eq!(answer(&c, &headers(Some(TOKEN), None), &body).await, (StatusCode::NOT_FOUND, None));
    rig.set(json!({"mcp_enabled": true, "mcp_token_clear": true}));
    assert_eq!(answer(&c, &headers(Some(""), None), &body).await, (StatusCode::NOT_FOUND, None));
    rig.set(json!({"mcp_token": TOKEN}));
    assert_eq!(answer(&c, &headers(None, None), &body).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(answer(&c, &headers(Some("mcp-test-token-012345678"), None), &body).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(answer(&c, &headers(Some(TOKEN), None), &body).await.0, StatusCode::OK);
    // The settings page never sees the token.
    let page = rig.state.settings_store.masked_json();
    assert!(!page.to_string().contains(TOKEN));
    assert_eq!(page["mcp_token_set"], true);
}

#[tokio::test]
async fn a_browser_must_come_from_an_allowed_origin() {
    let rig = Rig::new();
    let c = rig.controller();
    let body = rpc("ping", json!({}));
    assert_eq!(answer(&c, &headers(Some(TOKEN), Some("https://evil.example")), &body).await.0, StatusCode::FORBIDDEN);
    rig.set(json!({"mcp_allowed_origins": ["https://App.Example/", "not a url", "ftp://x"]}));
    assert_eq!(rig.state.settings_store.snapshot().mcp_allowed_origins, vec!["https://app.example".to_string()]);
    assert_eq!(answer(&c, &headers(Some(TOKEN), Some("https://app.example")), &body).await.0, StatusCode::OK);
    assert_eq!(answer(&c, &headers(Some(TOKEN), Some("https://app.example.evil")), &body).await.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn it_speaks_the_protocol() {
    let rig = Rig::new();
    let c = rig.controller();
    let init = ask(&c, "initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}})).await;
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "bageo");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    let newer = ask(&c, "initialize", json!({"protocolVersion": "2099-01-01"})).await;
    assert_eq!(newer["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
    assert_eq!(ask(&c, "ping", json!({})).await["result"], json!({}));
    assert_eq!(ask(&c, "resources/list", json!({})).await["error"]["code"], -32601);
    // A notification: accepted, nothing to answer.
    let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string();
    assert_eq!(answer(&c, &headers(Some(TOKEN), None), note.as_bytes()).await, (StatusCode::ACCEPTED, None));
    // Not JSON, a batch, an unknown protocol version.
    let (status, body) = answer(&c, &headers(Some(TOKEN), None), b"{oops").await;
    assert_eq!((status, body.unwrap()["error"]["code"].clone()), (StatusCode::BAD_REQUEST, json!(-32700)));
    let batch = format!("[{}]", String::from_utf8(rpc("ping", json!({}))).unwrap());
    assert_eq!(answer(&c, &headers(Some(TOKEN), None), batch.as_bytes()).await.0, StatusCode::BAD_REQUEST);
    let mut old = headers(Some(TOKEN), None);
    old.insert("mcp-protocol-version", HeaderValue::from_static("2024-01-01"));
    assert_eq!(answer(&c, &old, &rpc("ping", json!({}))).await.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_call_tools_follow_the_setting() {
    let rig = Rig::new();
    let c = rig.controller();
    let looks = ["get_status", "list_calls", "get_call", "list_tasks", "list_errands", "find_contacts"];
    assert_eq!(tool_names(&c).await, looks);
    // Off: asking for a call anyway is an unknown tool.
    let r = ask(&c, "tools/call", json!({"name": "request_call", "arguments": {"target": "0912345678", "task": "x"}})).await;
    assert_eq!(r["error"]["code"], -32602);
    rig.set(json!({"mcp_calls": "confirm"}));
    let names = tool_names(&c).await;
    assert!(names.contains(&"request_call".to_string()) && names.contains(&"cancel_call".to_string()));
    assert!(!names.contains(&"say_to_call".to_string()));
    rig.set(json!({"mcp_calls": "direct"}));
    assert!(tool_names(&c).await.contains(&"say_to_call".to_string()));
    rig.set(json!({"mcp_calls": "anything else"}));
    assert_eq!(rig.state.settings_store.snapshot().mcp_calls, "off");
}

#[tokio::test]
async fn a_call_request_is_checked_and_counted() {
    let rig = Rig::new();
    let c = rig.controller();
    rig.set(json!({"mcp_calls": "confirm", "mcp_errands_per_hour": 2}));
    let call = |target: &str| json!({"name": "request_call", "arguments": {"target": target, "task": "訂今天下午四點半兩份雞排"}});
    // Emergency numbers never get that far; no phone is said at once.
    let r = ask(&c, "tools/call", call("110")).await;
    assert_eq!((r["result"]["isError"].clone(), r["result"]["structuredContent"]["code"].clone()), (json!(true), json!("not_allowed")));
    let r = ask(&c, "tools/call", call("0912345678")).await;
    assert_eq!(r["result"]["structuredContent"]["code"], "no_phone");
    let r = ask(&c, "tools/call", json!({"name": "request_call", "arguments": {"target": "0912345678", "task": "  "}})).await;
    assert_eq!(r["result"]["structuredContent"]["code"], "no_task");
    rig.set(json!({"mcp_errands_per_hour": 0}));
    let r = ask(&c, "tools/call", call("0912345678")).await;
    assert_eq!(r["result"]["structuredContent"]["kind"], "too_many");
}

#[tokio::test]
async fn the_look_tools_answer_and_check_their_arguments() {
    let rig = Rig::new();
    let c = rig.controller();
    let r = ask(&c, "tools/call", json!({"name": "get_status", "arguments": {}})).await;
    assert_eq!(r["result"]["isError"], false);
    assert_eq!(r["result"]["structuredContent"]["kind"], "status");
    assert_eq!(r["result"]["content"][0]["type"], "text");
    let r = ask(&c, "tools/call", json!({"name": "list_calls", "arguments": {"day": "today", "only": "missed", "limit": 3}})).await;
    assert_eq!(r["result"]["structuredContent"], json!({"kind": "calls", "calls": []}));
    let r = ask(&c, "tools/call", json!({"name": "list_calls", "arguments": {"day": "someday"}})).await;
    assert_eq!((r["result"]["isError"].clone(), r["result"]["structuredContent"]["code"].clone()), (json!(true), json!("bad_day")));
    let r = ask(&c, "tools/call", json!({"name": "get_call", "arguments": {"id": "c-missing"}})).await;
    assert_eq!(r["result"]["structuredContent"]["kind"], "not_found");
    let r = ask(&c, "tools/call", json!({"name": "get_call", "arguments": {"id": "../x"}})).await;
    assert_eq!(r["result"]["structuredContent"]["code"], "bad_ref");
}

#[test]
fn results_carry_no_key_or_token() {
    let rig = Rig::new();
    let c = rig.controller();
    let rt = c.settings();
    let o = Outcome::Failed { code: format!("boom {TOKEN} console-secret-token") };
    let v = c.outcome_json(&rt, &o);
    let s = v.to_string();
    assert!(!s.contains(TOKEN) && !s.contains("console-secret-token"), "{s}");
    assert_eq!(v["kind"], "failed");
}

#[tokio::test]
async fn oversized_body_and_url_encoded_secrets_are_handled() {
    let rig = Rig::new();
    rig.set(json!({"openclaw_token": "sec/ret?tok=123456"}));
    let c = rig.controller();
    let big = vec![b'a'; 64 * 1024 + 1];
    assert_eq!(answer(&c, &headers(Some(TOKEN), None), &big).await, (StatusCode::PAYLOAD_TOO_LARGE, None));
    let mut upper_bearer = HeaderMap::new();
    upper_bearer.insert("authorization", HeaderValue::from_str(&format!("BEARER {TOKEN}")).unwrap());
    assert_eq!(answer(&c, &upper_bearer, &rpc("ping", json!({}))).await.0, StatusCode::OK);
    let rt = c.settings();
    let o = Outcome::Failed {
        code: "raw=sec/ret?tok=123456 up=sec%2Fret%3Ftok%3D123456 low=sec%2fret%3ftok%3d123456".into(),
    };
    let s = c.outcome_json(&rt, &o).to_string();
    assert!(!s.contains("123456"), "{s}");
}

#[tokio::test]
async fn tool_argument_and_cancel_validation_errors_are_reported() {
    let rig = Rig::new();
    rig.set(json!({"mcp_calls": "confirm"}));
    let c = rig.controller();
    let r = ask(&c, "tools/call", json!({"name": "cancel_call", "arguments": {"id": "   "}})).await;
    assert_eq!((r["result"]["isError"].clone(), r["result"]["structuredContent"]["code"].clone()), (json!(true), json!("no_ref")));
    let r = ask(&c, "tools/call", json!({"name": "get_status", "arguments": ["not-an-object"]})).await;
    assert_eq!((r["result"]["isError"].clone(), r["result"]["structuredContent"]["code"].clone()), (json!(true), json!("bad_arguments")));
    let r = ask(&c, "tools/call", json!({"name": "no_such_tool", "arguments": {}})).await;
    assert_eq!(r["error"]["code"], -32602);
}
