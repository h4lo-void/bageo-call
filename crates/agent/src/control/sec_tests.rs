// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Security acceptance tests for the chat commands (attacker's view). A
//! failing test here shows a weakness; a passing one shows a defence holds.
//! All numbers, ids and names are made up.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::*;
use crate::calls_http::PhoneDevices;
use crate::config::Settings;
use crate::locale::Lang;
use crate::settings_store::SettingsStore;
use crate::tg_inbound::{fake_api, TgIncoming, TgPoller};

struct Rig {
    root: PathBuf,
    state: CallsState,
}

impl Rig {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("bageo-sec-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        let mut base = Settings::from_env();
        base.calls_data_dir = root.to_string_lossy().into_owned();
        base.telegram_bot_token = String::new();
        base.telegram_chat_id = String::new();
        base.gemini_api_key = "".into();
        base.calls_ui_token = "console-secret-token".into();
        let state = CallsState {
            settings: base.clone(),
            store: crate::calls::CallStore::new(root.join("calls"), true, false),
            settings_store: SettingsStore::load(base),
            task_store: crate::tasks::TaskStore::new(root.join("tasks")),
            http: reqwest::Client::new(),
            control: crate::callback::ControlRegistry::new(),
            ambience: Arc::new(crate::ambience::AmbienceLibrary::load(&root, 16000)),
            previews: Arc::new(crate::voice_preview::VoicePreviews::new(root.join("voice_preview"), root.join("voices"))),
            phones: PhoneDevices::new(),
            contacts: crate::contacts::ContactStore::open(root.join("contacts.json")),
            errands: crate::errands::ErrandStore::open(root.join("errands.json"), crate::errands::SimBook::in_memory()),
            sms: crate::sms_store::SmsStore::open(root.join("sms"), crate::sms_store::now_ms(), 15),
        };
        let rig = Rig { root, state };
        rig.set(json!({"gemini_api_key_clear": true}));
        rig
    }

    fn set(&self, patch: Value) {
        self.state.settings_store.apply_patch(&patch).unwrap();
    }

    async fn task(&self, id: &str, summary: &str) -> String {
        let task: crate::tasks::TaskRecord = serde_json::from_value(json!({
            "id": id,
            "created_at": "2026-11-15T10:00:00+08:00",
            "updated_at": "2026-11-15T10:00:00+08:00",
            "call_id": "",
            "serial": "pixel-test",
            "caller_id": "0912345678",
            "status": "pending",
            "intent": "message",
            "summary": summary,
            "transcript": "",
            "reply_method": "other"
        }))
        .unwrap();
        self.state.task_store.create(task).await.unwrap().id
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn msg(channel: &str, chat: &str, sender: &str, body: Body, reply_to_bot: bool) -> Inbound {
    Inbound {
        channel_id: channel.to_string(),
        platform: "telegram".to_string(),
        chat_id: chat.to_string(),
        sender_id: sender.to_string(),
        body,
        message_id: "77".to_string(),
        reply_to_bot,
        direct: false,
    }
}

fn text(s: &str) -> Body {
    Body::Text(s.to_string())
}

const ME: &str = r#"{"ok": true, "result": {"id": 777, "is_bot": true, "username": "bageo_test_bot"}}"#;

// ------------------------------------------------------------ weaknesses

/// P1: the owner restricts a group, but types entries the sanitizer drops
/// (a name instead of a numeric id). The list ends up empty, and empty means
/// "anyone in the chat": the restriction silently becomes no restriction.
#[tokio::test]
async fn sec_a_sender_list_of_only_unusable_entries_must_not_open_the_chat_to_everyone() {
    let rig = Rig::new();
    rig.set(json!({"telegram_bot_token": "123:TESTTOKEN", "telegram_chat_id": "-1001", "telegram_control": true, "telegram_control_users": ["王小明"]}));
    let c = Controller::new(rig.state.clone());
    let saved = rig.state.settings_store.snapshot().telegram_control_users.clone();
    let stranger = msg(ENV_TELEGRAM, "-1001", "666", text("/contacts"), false);
    assert_eq!(c.handle(&stranger).await, None, "owner asked to restrict the group, list saved as {saved:?}, member 666 got an answer");
}

/// P1: with a sender list on the environment's bot, a member of the group who
/// is not on the list presses the to-do card's confirm button: it is handed
/// to the card handler (fulfill::on_confirm) with no sender check and no
/// confirmation step.
#[tokio::test]
async fn sec_a_to_do_card_button_from_a_member_off_the_sender_list_must_not_be_handed_on() {
    let rig = Rig::new();
    rig.set(json!({"telegram_bot_token": "123:TESTTOKEN", "telegram_chat_id": "-1001", "telegram_control": true, "telegram_control_users": ["42"]}));
    let c = Controller::new(rig.state.clone());
    let updates = json!({"ok": true, "result": [
        {"update_id": 1, "callback_query": {"id": "cb", "from": {"id": 666}, "data": "t:task-9:c",
            "message": {"message_id": 8, "chat": {"id": -1001, "type": "supergroup"}}}},
    ]});
    let (api, _seen) = fake_api::serve(vec![("getMe", ME.to_string()), ("getUpdates", updates.to_string())]).await;
    let bot = access::telegram_bots(&rig.state.settings_store.snapshot()).remove(0);
    let (cards, mut card_rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(telegram::run_bot(c, reqwest::Client::new(), api, bot, cards));
    let got = tokio::time::timeout(Duration::from_secs(3), card_rx.recv()).await;
    task.abort();
    assert!(!matches!(got, Ok(Some(_))), "member 666 (not in telegram_control_users) confirmed a to-do: {got:?}");
}

/// P2: the same bot and chat switched on twice (environment and a settings
/// page channel). The channel's empty list wins over the environment's list.
#[test]
fn sec_a_second_switch_for_the_same_bot_and_chat_must_not_drop_the_sender_list() {
    let base = Settings::from_env();
    let mut rt = crate::settings_store::RuntimeSettings::from_base(&base);
    rt.telegram_bot_token = "123:ENV".into();
    rt.telegram_chat_id = "-1001".into();
    rt.telegram_control = true;
    rt.telegram_control_users = vec!["42".into()];
    rt.notify_channels = vec![crate::notify::Channel {
        id: "c1".into(),
        kind: "telegram".into(),
        enabled: true,
        token: "123:ENV".into(),
        target: "-1001".into(),
        control: true,
        ..Default::default()
    }];
    let bot = access::telegram_bots(&rt).remove(0);
    let inc = TgIncoming {
        chat_id: "-1001".into(),
        sender_id: "666".into(),
        text: Some("/contacts".into()),
        button: None,
        message_id: 5,
        reply_to_bot: false,
    };
    let m = telegram::inbound(&bot, &inc).unwrap();
    assert_eq!(access::authorize(&rt, &m), None, "member 666 allowed through channel {}", m.channel_id);
}

/// P2: setMyCommands without a scope shows the full command menu ("have the
/// AI make a call for you"…) to anyone who opens the bot, against "nobody
/// can find out the bot is there".
#[tokio::test]
async fn sec_the_command_menu_must_not_be_shown_to_strangers() {
    let (api, seen) = fake_api::serve(vec![]).await;
    crate::tg_inbound::set_my_commands(&reqwest::Client::new(), &api, "123:TEST", &words::menu(words(Lang::En)), &["42".to_string()]).await;
    let sent = fake_api::sent(&seen, "setMyCommands");
    assert!(sent[0].get("scope").is_some(), "menu set for every chat and user: {}", sent[0]);
    assert_eq!(sent[0]["scope"]["type"], "chat", "{}", sent[0]);
}

/// P2: a token written another way (URL-encoded, as in a request URL) is not
/// cut out of a reply.
#[test]
fn sec_a_url_encoded_token_must_still_be_cut_out() {
    let rig = Rig::new();
    rig.set(json!({"telegram_bot_token": "123456:TESTTOKENabc", "telegram_chat_id": "42", "telegram_control": true}));
    let c = Controller::new(rig.state.clone());
    let rt = rig.state.settings_store.snapshot();
    let out = c.scrub(&rt, Outbound { text: "x https://api.example.org/bot123456%3ATESTTOKENabc/getMe y".into(), ..Default::default() });
    assert!(!out.text.contains("TESTTOKENabc"), "{}", out.text);
}

/// The key phones use over Wi-Fi never shows in a chat reply.
#[test]
fn sec_the_phone_pairing_key_is_cut_out_of_replies() {
    let rig = Rig::new();
    rig.set(json!({"telegram_bot_token": "123456:TESTTOKENabc", "telegram_chat_id": "42", "telegram_control": true}));
    let c = Controller::new(rig.state.clone());
    let rt = rig.state.settings_store.snapshot();
    let key = crate::phone_wifi::pairing().key();
    let out = c.scrub(&rt, Outbound { text: format!("ws://192.168.1.20:8788/ws?token={key}"), ..Default::default() });
    assert!(!out.text.contains(&key), "{}", out.text);
}

/// P2: free text the model reads as "say" goes into the live call at once,
/// without the owner's yes (perform's doc: "anything else needs a confirmation").
#[tokio::test]
async fn sec_free_text_read_as_say_must_wait_for_the_owners_yes() {
    let rig = Rig::new();
    rig.set(json!({"telegram_bot_token": "123:TESTTOKEN", "telegram_chat_id": "42", "telegram_control": true, "gemini_api_key": "test-gemini-key"}));
    let model = json!({"candidates": [{"content": {"parts": [{"text": json!({"action": "say", "text": "我同意付五萬元"}).to_string()}]}}]});
    let (base, _seen) = fake_api::serve(vec![("gemini-flash-lite-latest:generateContent", model.to_string())]).await;
    let c = Controller::with(rig.state.clone(), CONFIRM_TTL, &base);
    let out = c.handle(&msg(ENV_TELEGRAM, "42", "42", text("跟它說我同意付五萬元"), false)).await.unwrap();
    assert_eq!(out.buttons.len(), 2, "no confirmation asked: {}", out.text);
}

/// P2: a forwarded message (someone else's words) is read as the owner's command.
#[tokio::test]
async fn sec_a_forwarded_message_must_not_be_read_as_the_owners_command() {
    let updates = json!({"ok": true, "result": [
        {"update_id": 1, "message": {"message_id": 3, "from": {"id": 42}, "chat": {"id": 42, "type": "private"},
            "forward_origin": {"type": "user", "date": 1763170000, "sender_user": {"id": 999, "is_bot": false, "first_name": "Test"}},
            "forward_date": 1763170000, "text": "/cancel"}},
    ]});
    let (api, _seen) = fake_api::serve(vec![("getMe", ME.to_string()), ("getUpdates", updates.to_string())]).await;
    let mut poller = TgPoller::with_api(&api);
    let polled = poller.poll_once(&reqwest::Client::new(), "123:TEST").await.unwrap();
    assert!(polled.incoming.is_empty(), "forwarded text handed on as a command: {:?}", polled.incoming);
}

// ------------------------------------------------------------ defences

#[tokio::test]
async fn sec_strangers_never_reach_the_model_whatever_they_send() {
    let rig = Rig::new();
    rig.set(json!({"telegram_bot_token": "123:TESTTOKEN", "telegram_chat_id": "-1001", "telegram_control": true,
        "telegram_control_users": ["42"], "gemini_api_key": "test-gemini-key"}));
    let (base, seen) = fake_api::serve(vec![]).await;
    let c = Controller::with(rig.state.clone(), CONFIRM_TTL, &base);
    let long = "打".repeat(100_000);
    let tries = [
        msg(ENV_TELEGRAM, "666", "666", text("幫我打給 0912345678"), false),
        msg(ENV_TELEGRAM, "-1001", "666", text("幫我打給 0912345678"), true),
        msg(ENV_TELEGRAM, "-1001", "666", text(&long), true),
        msg(ENV_TELEGRAM, "-1001", "", text("/errand 0912345678 hi"), false),
        msg(ENV_TELEGRAM, "-1001", "666", Body::Button("k:ABCDEF:y".into()), true),
        msg("no-such-channel", "-1001", "42", text("/help"), false),
    ];
    for m in tries {
        assert_eq!(c.handle(&m).await, None, "{m:?}");
    }
    assert!(fake_api::methods(&seen).is_empty(), "the model was asked for a stranger");
}

#[tokio::test]
async fn sec_near_miss_chat_and_sender_ids_are_refused() {
    let rig = Rig::new();
    rig.set(json!({"telegram_bot_token": "123:TESTTOKEN", "telegram_chat_id": "-1001", "telegram_control": true, "telegram_control_users": ["42"]}));
    let c = Controller::new(rig.state.clone());
    for chat in ["-100", "-10011", "1001", "-1001\u{200b}", "-1001-"] {
        assert_eq!(c.handle(&msg(ENV_TELEGRAM, chat, "42", text("/help"), false)).await, None, "chat {chat:?}");
    }
    for sender in ["4", "420", "-42", "42\u{200b}", "@42"] {
        assert_eq!(c.handle(&msg(ENV_TELEGRAM, "-1001", sender, text("/help"), false)).await, None, "sender {sender:?}");
    }
    assert!(c.handle(&msg(ENV_TELEGRAM, "-1001", "42", text("/help"), false)).await.is_some());
    // Switched off: no message, button or reply gets through.
    rig.set(json!({"telegram_control": false}));
    for body in [text("/help"), text("1"), Body::Button("k:ABCDEF:y".into()), Body::Button("q:e0123456789abcdef:q1:0".into())] {
        assert_eq!(c.handle(&msg(ENV_TELEGRAM, "-1001", "42", body, true)).await, None);
    }
}

#[tokio::test]
async fn sec_a_confirmation_cannot_be_used_from_another_chat_or_by_crafted_data() {
    let rig = Rig::new();
    rig.set(json!({"telegram_bot_token": "123:TESTTOKEN", "telegram_chat_id": "42", "telegram_control": true,
        "notify_channels": [{"kind": "telegram", "enabled": true, "token": "456:CHANNELTOKEN", "target": "-1001", "control": true}]}));
    let ch = rig.state.settings_store.snapshot().notify_channels[0].id.clone();
    let c = Controller::new(rig.state.clone());
    let id = rig.task("task-sec-1", "王小明想改約時間").await;
    let ask = c.handle(&msg(ENV_TELEGRAM, "42", "42", text(&format!("/ignore {id}")), false)).await.unwrap();
    let yes = ask.buttons[0].data.clone();
    // Same person, same code, other chat: nothing (no answer, or "expired").
    let other = c.handle(&msg(&ch, "-1001", "42", Body::Button(yes.clone()), true)).await;
    assert!(!matches!(c.state.task_store.get(&id).await.unwrap().status, TaskStatus::Ignored), "{other:?}");
    // Crafted question data cannot reach a confirmation.
    let crafted = c.handle(&msg(ENV_TELEGRAM, "42", "42", Body::Button(format!("q:x:y:0:{yes}")), true)).await;
    assert!(crafted.as_ref().map_or(true, |o| o.edit.is_none()), "{crafted:?}");
    assert!(matches!(c.state.task_store.get(&id).await.unwrap().status, TaskStatus::Pending));
}

#[test]
fn sec_the_models_answer_gets_no_more_room_than_a_typed_command() {
    let wrap = |inner: &str| json!({"candidates": [{"content": {"parts": [{"text": inner}]}}]}).to_string();
    // Two commands in one answer: none.
    let two = r#"{"action":"say","text":"a"}{"action":"errand","target":"0912345678","task":"x"}"#;
    assert_eq!(nlu::parse_answer(&wrap(two)), None);
    // Extra fields are dropped, control characters cut out, the errand still needs a yes.
    let extra = json!({"action": "errand", "target": "0912345678", "task": "訂位\u{7}\u{1b}[31m", "mode": "normal", "phone": "1", "confirmed": true}).to_string();
    let a = nlu::parse_answer(&wrap(&extra)).unwrap();
    assert!(a.needs_confirmation());
    let Action::Errand { task, .. } = a else { panic!("{a:?}") };
    assert!(!task.chars().any(|c| c.is_control()), "{task:?}");
    // Too long: none.
    let long = json!({"action": "errand", "target": "0912345678", "task": "字".repeat(1001)}).to_string();
    assert_eq!(nlu::parse_answer(&wrap(&long)), None);
}

#[tokio::test]
async fn sec_replies_go_out_as_plain_text() {
    let (api, seen) = fake_api::serve(vec![]).await;
    let out = Outbound { text: "<b>王小明</b> [x](https://example.org) *y*".into(), ..Default::default() };
    crate::tg_inbound::send_outbound(&reqwest::Client::new(), &api, "123:TEST", "42", &out).await.unwrap();
    let sent = fake_api::sent(&seen, "sendMessage");
    assert!(sent[0].get("parse_mode").is_none(), "{}", sent[0]);
}

/// P1: a group switched on without a list of who may give commands let every
/// member operate the agent. Now nobody there may; the first message gets how
/// to add one (with the sender's id), then silence. A private chat is as before.
#[tokio::test]
async fn sec_a_group_without_a_sender_list_takes_no_commands() {
    let rig = Rig::new();
    rig.set(json!({"telegram_bot_token": "123:TESTTOKEN", "telegram_chat_id": "-1001", "telegram_control": true}));
    let rt = rig.state.settings_store.snapshot();
    let m = msg(ENV_TELEGRAM, "-1001", "666", text("/status"), false);
    assert_eq!(access::authorize(&rt, &m), None, "a member of an unlisted group was let in");
    assert_eq!(access::check(&rt, &m), access::Verdict::NeedsList);
    let c = Controller::new(rig.state.clone());
    let hint = c.handle(&m).await.expect("the group is told how to add its list");
    assert!(hint.text.contains("666") && !hint.text.contains("Bageo 0.") && hint.buttons.is_empty(), "{}", hint.text);
    assert_eq!(c.handle(&m).await, None, "told once, then silence");
    // A button press in that group does nothing either.
    assert_eq!(c.handle(&msg(ENV_TELEGRAM, "-1001", "666", Body::Button("k:x:y".into()), true)).await, None);
    // Listed: the listed member only.
    rig.set(json!({"telegram_control_users": ["42"]}));
    assert!(c.handle(&msg(ENV_TELEGRAM, "-1001", "42", text("/status"), false)).await.is_some());
    assert_eq!(c.handle(&msg(ENV_TELEGRAM, "-1001", "666", text("/status"), false)).await, None);
    // A private chat with the bot needs no list.
    rig.set(json!({"telegram_chat_id": "42", "telegram_control_users": []}));
    let rt = rig.state.settings_store.snapshot();
    assert!(access::authorize(&rt, &msg(ENV_TELEGRAM, "42", "42", text("/status"), false)).is_some());
}

#[test]
fn sec_only_known_one_to_one_chats_count_as_private() {
    assert!(access::direct_chat("telegram", "42"));
    assert!(!access::direct_chat("telegram", "-1001"));
    assert!(!access::direct_chat("telegram", "@somechannel"));
    assert!(access::direct_chat("slack", "D0123ABCD"));
    assert!(!access::direct_chat("slack", "C0123ABCD"));
    assert!(!access::direct_chat("discord", "123456789012345678"));
    assert!(!access::direct_chat("matrix", "!room:example.org"));
}

/// A Discord direct message or a Matrix room of only the owner and the bot is
/// a private chat: no sender list needed. A server channel or a bigger room
/// still needs one, and a list that is there still applies.
#[test]
fn sec_a_discord_or_matrix_private_chat_needs_no_sender_list() {
    let base = Settings::from_env();
    let mut rt = crate::settings_store::RuntimeSettings::from_base(&base);
    rt.notify_channels = vec![
        crate::notify::Channel { id: "d1".into(), kind: "discord".into(), enabled: true, token: "test-token-d".into(), target: "123".into(), control: true, ..Default::default() },
        crate::notify::Channel { id: "m1".into(), kind: "matrix".into(), enabled: true, token: "test-token-m".into(), target: "!room:example.org".into(), control: true, ..Default::default() },
    ];
    let ask = |channel: &str, platform: &str, chat: &str, sender: &str, direct: bool| Inbound {
        channel_id: channel.into(),
        platform: platform.into(),
        chat_id: chat.into(),
        sender_id: sender.into(),
        body: text("/status"),
        message_id: "1".into(),
        reply_to_bot: false,
        direct,
    };
    for (channel, platform, chat, sender) in [("d1", "discord", "123", "111"), ("m1", "matrix", "!room:example.org", "@lee:example.org")] {
        assert!(access::authorize(&rt, &ask(channel, platform, chat, sender, true)).is_some(), "{platform}: a private chat was refused");
        assert_eq!(access::check(&rt, &ask(channel, platform, chat, sender, false)), access::Verdict::NeedsList, "{platform}: a group was let in");
    }
    // With a list, the private chat still honours it.
    rt.notify_channels[0].control_users = vec!["111".into()];
    assert!(access::authorize(&rt, &ask("d1", "discord", "123", "111", true)).is_some());
    assert_eq!(access::authorize(&rt, &ask("d1", "discord", "123", "222", true)), None);
}

#[test]
fn sec_the_list_hint_is_written_in_every_language() {
    for l in crate::locale::ALL {
        let t = list_hint_text(l);
        assert!(t.contains("{id}") && t.contains("Bageo"), "{l:?}");
    }
}

#[test]
fn sec_api_sender_ok_filters_disabled_and_resolves_hierarchically() {
    let base = Settings::from_env();
    let mut rt = crate::settings_store::RuntimeSettings::from_base(&base);
    rt.notify_channels = vec![
        crate::notify::Channel {
            id: "off1".into(),
            kind: "line".into(),
            name: "Disabled".into(),
            enabled: false,
            control: false,
            control_users: vec!["only-disabled".into()],
            ..Default::default()
        },
        crate::notify::Channel {
            id: "id-a".into(),
            kind: "line".into(),
            name: "Work".into(),
            enabled: true,
            control: false,
            control_users: vec!["u1".into()],
            ..Default::default()
        },
        crate::notify::Channel {
            id: "id-b".into(),
            kind: "line".into(),
            name: "Home".into(),
            enabled: true,
            control: false,
            control_users: vec!["u1".into(), "u2".into()],
            ..Default::default()
        },
    ];
    // Enabled non-control notification channels retain their control_users.
    assert!(access::api_sender_ok(&rt, "Work", "u1"));
    assert!(!access::api_sender_ok(&rt, "Work", "u2"));
    assert!(access::api_sender_ok(&rt, "id-b", "u2"));
    // Disabled channel's list does not leak into kind lookup.
    assert!(!access::api_sender_ok(&rt, "line", "only-disabled"));
    assert!(access::api_sender_ok(&rt, "line", "u1"));
    assert!(!access::api_sender_ok(&rt, "line", "u2"));
}

#[test]
fn sec_scrub_redacts_trtc_webhook_and_short_alphanumeric_passwords() {
    let rig = Rig::new();
    let mut base = rig.state.settings.clone();
    base.notify_webhook_url = "https://hooks.example.org/secret-hook".into();
    base.calls_ui_token = String::new();
    let mut st = rig.state.clone();
    st.settings = base.clone();
    st.settings_store = SettingsStore::load(base);
    let c = Controller::new(st);
    let mut rt = (*c.settings()).clone();
    rt.trtc_secret_key = "trtc-key-xyz".into();
    rt.trtc_secret_id = "trtc-id-123".into();
    rt.trtc_cloud_secret_key = "trtc-cloud-456".into();
    rt.ui_password = "ab12".into();
    let scrubbed = c.scrub(
        &rt,
        Outbound {
            text: "2026 trtc=trtc-key-xyz id=trtc-id-123 cloud=trtc-cloud-456 hook=https://hooks.example.org/secret-hook pw=ab12".into(),
            buttons: vec![],
            edit: None,
        },
    );
    assert!(scrubbed.text.contains("2026"));
    assert!(!scrubbed.text.contains("trtc-key-xyz"), "{}", scrubbed.text);
    assert!(!scrubbed.text.contains("trtc-id-123"), "{}", scrubbed.text);
    assert!(!scrubbed.text.contains("trtc-cloud-456"), "{}", scrubbed.text);
    assert!(!scrubbed.text.contains("hooks.example.org"), "{}", scrubbed.text);
    assert!(!scrubbed.text.contains("ab12"), "{}", scrubbed.text);

    // Numeric 4-digit year password must not scrub dates like "2026".
    rt.ui_password = "2026".into();
    let keep_year = c.scrub(&rt, Outbound { text: "date 2026-11-18".into(), buttons: vec![], edit: None });
    assert!(keep_year.text.contains("2026-11-18"), "{}", keep_year.text);
}
