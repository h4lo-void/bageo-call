// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The chat commands end to end: who gets an answer, the confirmation, the
//! limits, what the replies say, and the Telegram bot against a stand-in.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::*;
use crate::calls_http::PhoneDevices;
use crate::config::Settings;
use crate::locale::Lang;
use crate::settings_store::SettingsStore;
use crate::tg_inbound::fake_api;

/// An agent's worth of stores for the controller, in a folder of its own: no
/// phones, no Gemini key, no Telegram until a test sets one.
struct Rig {
    root: PathBuf,
    state: CallsState,
}

impl Rig {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("bageo-control-{}", uuid::Uuid::new_v4().simple()));
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
        // The key may come from the environment of the machine running the tests.
        rig.set(json!({"gemini_api_key_clear": true}));
        rig
    }

    /// The environment's bot with commands on in chat 42.
    fn with_env_bot() -> Self {
        let rig = Self::new();
        rig.set(json!({"telegram_bot_token": "123:TESTTOKEN", "telegram_chat_id": "42", "telegram_control": true}));
        rig
    }

    fn set(&self, patch: Value) {
        self.state.settings_store.apply_patch(&patch).unwrap();
    }

    fn controller(&self) -> Arc<Controller> {
        Controller::new(self.state.clone())
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

    async fn task_status(&self, id: &str) -> TaskStatus {
        self.state.task_store.get(id).await.unwrap().status
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn from(channel: &str, chat: &str, sender: &str, body: Body) -> Inbound {
    Inbound {
        channel_id: channel.to_string(),
        platform: "telegram".to_string(),
        chat_id: chat.to_string(),
        sender_id: sender.to_string(),
        body,
        message_id: "77".to_string(),
        reply_to_bot: false,
        direct: false,
    }
}

/// The owner (42) writing in chat 42 of the environment's bot.
fn owner(text: &str) -> Inbound {
    from(ENV_TELEGRAM, "42", "42", Body::Text(text.to_string()))
}

fn press(data: &str) -> Inbound {
    from(ENV_TELEGRAM, "42", "42", Body::Button(data.to_string()))
}

/// The text is the same in some language (other tests may switch the notification language).
fn reads_as(text: &str, pick: impl Fn(&'static Words) -> String) -> bool {
    crate::locale::ALL.iter().any(|l| text == crate::locale::for_owner_in(*l, &pick(words(*l))))
}

#[tokio::test]
async fn nothing_is_answered_until_commands_are_on_for_that_very_chat() {
    let rig = Rig::new();
    rig.set(json!({"telegram_bot_token": "123:TESTTOKEN", "telegram_chat_id": "42"}));
    let c = rig.controller();
    assert_eq!(c.handle(&owner("/help")).await, None, "switched off by default");

    rig.set(json!({"telegram_control": true}));
    let help = c.handle(&owner("/help")).await.unwrap();
    assert!(help.text.contains("/errand") && help.text.contains("/calls"), "{}", help.text);

    // Another chat, another platform, a channel that does not exist: silence.
    assert_eq!(c.handle(&from(ENV_TELEGRAM, "43", "43", Body::Text("/help".into()))).await, None);
    let mut discord = owner("/help");
    discord.platform = "discord".into();
    assert_eq!(c.handle(&discord).await, None);
    assert_eq!(c.handle(&from("no-such-channel", "42", "42", Body::Text("/help".into()))).await, None);

    // A sender list narrows the chat down.
    rig.set(json!({"telegram_control_users": ["42", "x/y", "王小明", "42"]}));
    // Entries that are no id are kept as written (they match nobody), so a
    // list of names alone still narrows the chat; repeats go.
    assert_eq!(rig.state.settings_store.snapshot().telegram_control_users, vec!["42".to_string(), "x/y".to_string(), "王小明".to_string()]);
    assert_eq!(c.handle(&from(ENV_TELEGRAM, "42", "99", Body::Text("/help".into()))).await, None);
    assert!(c.handle(&owner("/status")).await.is_some());
}

#[tokio::test]
async fn a_settings_page_channel_takes_commands_only_from_its_own_chat_and_senders() {
    let rig = Rig::new();
    rig.set(json!({"notify_channels": [
        {"kind": "telegram", "name": "Family", "enabled": true, "token": "456:CHANNELTOKEN", "target": "-1001", "control": true, "control_users": ["7", "8"]},
        {"kind": "ntfy", "enabled": true, "target": "calls", "control": true},
    ]}));
    let rt = rig.state.settings_store.snapshot();
    let tg = rt.notify_channels[0].clone();
    assert!(tg.control);
    assert!(!rt.notify_channels[1].control, "no adapter for that kind yet");
    let c = rig.controller();
    assert!(c.handle(&from(&tg.id, "-1001", "7", Body::Text("/help".into()))).await.is_some());
    assert_eq!(c.handle(&from(&tg.id, "-1001", "9", Body::Text("/help".into()))).await, None);
    assert_eq!(c.handle(&from(&tg.id, "-1002", "7", Body::Text("/help".into()))).await, None);
    // Off for notifications: off for commands too.
    let mut off = rt.notify_channels.iter().map(|ch| serde_json::to_value(ch).unwrap()).collect::<Vec<_>>();
    off[0]["enabled"] = json!(false);
    rig.set(json!({ "notify_channels": off }));
    assert_eq!(c.handle(&from(&tg.id, "-1001", "7", Body::Text("/help".into()))).await, None);

    let bots = access::telegram_bots(&rig.state.settings_store.snapshot());
    assert!(bots.is_empty(), "nothing to poll: {bots:?}");
}

#[test]
fn each_bot_is_polled_once_and_a_chat_with_commands_comes_first() {
    let base = Settings::from_env();
    let mut rt = crate::settings_store::RuntimeSettings::from_base(&base);
    rt.telegram_bot_token = "123:ENV".into();
    rt.telegram_chat_id = "42".into();
    rt.telegram_control = false;
    rt.notify_channels = vec![
        crate::notify::Channel { id: "c1".into(), kind: "telegram".into(), enabled: true, token: "123:ENV".into(), target: "42".into(), control: true, ..Default::default() },
        crate::notify::Channel { id: "c2".into(), kind: "telegram".into(), enabled: true, token: "456:OTHER".into(), target: "-5".into(), control: true, ..Default::default() },
        crate::notify::Channel { id: "c3".into(), kind: "telegram".into(), enabled: true, token: "789:QUIET".into(), target: "-6".into(), control: false, ..Default::default() },
    ];
    let bots = access::telegram_bots(&rt);
    assert_eq!(bots.len(), 2, "{bots:?}");
    let env = bots.iter().find(|b| b.token == "123:ENV").unwrap();
    assert!(env.env && env.commands);
    assert_eq!(env.chats[0], ("c1".to_string(), "42".to_string()));
    assert_eq!(env.chats[1], (ENV_TELEGRAM.to_string(), "42".to_string()));
    // The same chat through the same bot gets a question once.
    rt.telegram_control = true;
    let chats = access::command_chats(&rt);
    assert_eq!(chats, vec![(ENV_TELEGRAM.to_string(), "42".to_string()), ("c2".to_string(), "-5".to_string())]);
}

#[tokio::test]
async fn a_flood_of_messages_goes_unanswered_past_the_limit() {
    let rig = Rig::with_env_bot();
    rig.set(json!({"control_commands_per_minute": 3}));
    let c = rig.controller();
    for _ in 0..3 {
        assert!(c.handle(&owner("/help")).await.is_some());
    }
    assert_eq!(c.handle(&owner("/help")).await, None);
    // Another chat has its own count.
    rig.set(json!({"notify_channels": [{"kind": "telegram", "enabled": true, "token": "456:CHANNELTOKEN", "target": "-1001", "control": true}]}));
    let id = rig.state.settings_store.snapshot().notify_channels[0].id.clone();
    assert!(c.handle(&from(&id, "-1001", "7", Body::Text("/help".into()))).await.is_some());
}

#[test]
fn the_limits_count_per_chat_over_their_window() {
    let l = access::Limits::default();
    assert!(l.errand_room("a", 2));
    l.note_errand("a");
    l.note_errand("a");
    assert!(!l.errand_room("a", 2));
    assert!(l.errand_room("b", 2));
    assert!(!l.errand_room("b", 0), "0 = no calls from chats");
    assert!(l.allow_message("a", 1));
    assert!(!l.allow_message("a", 1));
    assert!(l.allow_message("b", 1));
}

#[tokio::test]
async fn a_call_is_checked_before_it_is_offered_and_counts_against_the_hourly_limit() {
    let rig = Rig::with_env_bot();
    let c = rig.controller();
    // No phone connected: said at once, nothing to confirm.
    let out = c.handle(&owner("/errand 0912345678 訂明天晚上七點兩位")).await.unwrap();
    assert!(out.buttons.is_empty() && reads_as(&out.text, |w| w.e_no_phone.to_string()), "{}", out.text);
    // Emergency and premium-rate numbers never get that far.
    let out = c.handle(&owner("/errand 110 hello")).await.unwrap();
    assert!(reads_as(&out.text, |w| w.e_not_allowed.to_string()), "{}", out.text);
    // A name that is no contact.
    let out = c.handle(&owner("/errand 王小明 約週六吃飯")).await.unwrap();
    assert!(reads_as(&out.text, |w| words::fmt(w.no_contact, &["王小明"])), "{}", out.text);
    // No calls from chats at all.
    rig.set(json!({"control_errands_per_hour": 0}));
    let out = c.handle(&owner("/errand 0912345678 訂位")).await.unwrap();
    assert!(reads_as(&out.text, |w| words::fmt(w.too_many, &["0"])), "{}", out.text);
    assert!(out.buttons.is_empty());
}

#[tokio::test]
async fn a_to_do_is_decided_only_after_one_yes_from_the_one_who_asked() {
    let rig = Rig::with_env_bot();
    let c = rig.controller();
    let id = rig.task("task-chat-1", "王小明想改約時間").await;

    // Asked, then dropped with 2: nothing happens.
    let ask = c.handle(&owner(&format!("/ignore {id}"))).await.unwrap();
    assert_eq!(ask.buttons.len(), 2, "{}", ask.text);
    assert!(ask.text.contains("王小明想改約時間"), "{}", ask.text);
    let dropped = c.handle(&owner("2")).await.unwrap();
    assert!(reads_as(&dropped.text, |w| w.dropped.to_string()), "{}", dropped.text);
    assert!(matches!(rig.task_status(&id).await, TaskStatus::Pending));

    // The list number works too; an old button and another member do nothing.
    let listed = c.handle(&owner("/tasks")).await.unwrap();
    assert!(listed.text.contains("1. "), "{}", listed.text);
    let ask = c.handle(&owner("/ignore 1")).await.unwrap();
    let yes = ask.buttons[0].data.clone();
    assert!(yes.starts_with("k:") && yes.ends_with(":y") && yes.len() <= BUTTON_DATA_MAX, "{yes}");
    let stale = c.handle(&press("k:ZZZZZZ:y")).await.unwrap();
    assert!(reads_as(&stale.text, |w| w.expired.to_string()), "{}", stale.text);
    let other = c.handle(&from(ENV_TELEGRAM, "42", "43", Body::Button(yes.clone()))).await.unwrap();
    assert!(reads_as(&other.text, |w| w.expired.to_string()));
    assert!(matches!(rig.task_status(&id).await, TaskStatus::Pending));

    // The owner's yes: done once, the question message edited.
    let done = c.handle(&press(&yes)).await.unwrap();
    assert_eq!(done.edit.as_deref(), Some("77"));
    assert!(done.buttons.is_empty());
    assert!(matches!(rig.task_status(&id).await, TaskStatus::Ignored));
    let again = c.handle(&press(&yes)).await.unwrap();
    assert!(reads_as(&again.text, |w| w.expired.to_string()), "{}", again.text);
}

#[tokio::test]
async fn a_confirmation_left_too_long_does_nothing() {
    let rig = Rig::with_env_bot();
    let c = Controller::with(rig.state.clone(), Duration::ZERO, nlu::GEMINI_BASE);
    let id = rig.task("task-chat-2", "回電給王小明").await;
    let ask = c.handle(&owner(&format!("/confirm {id}"))).await.unwrap();
    assert_eq!(ask.buttons.len(), 2);
    tokio::time::sleep(Duration::from_millis(5)).await;
    let late = c.handle(&owner("1")).await.unwrap();
    assert!(reads_as(&late.text, |w| w.expired.to_string()), "{}", late.text);
    assert!(matches!(rig.task_status(&id).await, TaskStatus::Pending));
}

#[tokio::test]
async fn free_text_without_a_gemini_key_gets_the_short_hint() {
    let rig = Rig::with_env_bot();
    let c = rig.controller();
    let out = c.handle(&owner("昨天有誰打來")).await.unwrap();
    assert!(reads_as(&out.text, |w| w.hint.to_string()), "{}", out.text);
    let out = c.handle(&owner("/weather")).await.unwrap();
    assert!(reads_as(&out.text, |w| w.hint.to_string()), "{}", out.text);
    let out = c.handle(&owner("/call")).await.unwrap();
    assert!(reads_as(&out.text, |w| words::fmt(w.usage, &[&words::usage_line(w, "call")])), "{}", out.text);
}

#[tokio::test]
async fn free_text_goes_through_the_model_and_still_waits_for_the_owners_yes() {
    let rig = Rig::with_env_bot();
    rig.set(json!({"gemini_api_key": "test-gemini-key"}));
    let id = rig.task("task-chat-3", "王小明想改約時間").await;
    let model = json!({"candidates": [{"content": {"parts": [{"text": json!({"action": "ignore", "ref": id}).to_string()}]}}]});
    let (base, seen) = fake_api::serve(vec![("gemini-flash-lite-latest:generateContent", model.to_string())]).await;
    let c = Controller::with(rig.state.clone(), CONFIRM_TTL, &base);
    let out = c.handle(&owner("把王小明那件待辦忽略掉")).await.unwrap();
    assert_eq!(out.buttons.len(), 2, "{}", out.text);
    assert!(matches!(rig.task_status(&id).await, TaskStatus::Pending));
    let sent = fake_api::sent(&seen, "gemini-flash-lite-latest:generateContent");
    let user = sent[0]["contents"][0]["parts"][0]["text"].as_str().unwrap().to_string();
    assert!(user.ends_with("把王小明那件待辦忽略掉"), "{user}");
}

#[test]
fn replies_never_carry_a_key_or_a_token() {
    let rig = Rig::with_env_bot();
    rig.set(json!({"gemini_api_key": "AIzaTESTKEY123", "notify_channels": [{"kind": "telegram", "enabled": true, "token": "456:CHANNELTOKEN", "target": "-1001"}]}));
    let c = rig.controller();
    let rt = rig.state.settings_store.snapshot();
    let out = c.scrub(
        &rt,
        Outbound {
            text: "123:TESTTOKEN AIzaTESTKEY123 456:CHANNELTOKEN console-secret-token ok".into(),
            buttons: vec![Button { label: "AIzaTESTKEY123".into(), data: "x".into() }],
            edit: None,
        },
    );
    assert_eq!(out.text, "••• ••• ••• ••• ok");
    assert_eq!(out.buttons[0].label, "•••");
}

#[test]
fn every_result_reads_in_traditional_chinese_and_english() {
    let call = CallLine { id: "c1".into(), when: "11/14 20:31".into(), who: "王小明".into(), number: "0912345678".into(), summary: "問明天的訂位".into() };
    let errand = ErrandLine { id: "e1".into(), who: String::new(), number: "0223456789".into(), status: "dialing".into(), state: "撥號中".into() };
    let cases: Vec<(Outcome, &str, &str)> = vec![
        (Outcome::Calls { calls: vec![call.clone()] }, "通話紀錄\n1. 11/14 20:31 · 王小明 (0912345678) · 問明天的訂位", "Calls\n1. 11/14 20:31 · 王小明 (0912345678) · 問明天的訂位"),
        (Outcome::Calls { calls: vec![] }, "沒有符合的通話。", "No calls match."),
        (
            Outcome::Call { call: CallDetail { id: "c1".into(), when: "11/14 20:31".into(), who: String::new(), number: String::new(), summary: "推銷貸款".into(), transcript: "對方：您好".into(), link: "http://bageo.example/calls/#call=c1".into() } },
            "11/14 20:31 · 未顯示號碼\n\n摘要\n推銷貸款\n\n逐字稿節錄\n對方：您好\n\n在控制台打開: http://bageo.example/calls/#call=c1",
            "11/14 20:31 · Hidden number\n\nSummary\n推銷貸款\n\nTranscript (excerpt)\n對方：您好\n\nOpen in the console: http://bageo.example/calls/#call=c1",
        ),
        (
            Outcome::Status { version: "1.0.0".into(), phones: vec![PhoneLine { serial: "p1".into(), label: "Pixel 9".into(), connected: true, mode: "normal".into(), in_call: true }], live_calls: 1 },
            "Bageo 1.0.0\n進行中的通話：1\n\n手機\n1. Pixel 9 — 一般手機 · 通話中",
            "Bageo 1.0.0\nCalls in progress: 1\n\nPhones\n1. Pixel 9 — normal phone · in a call",
        ),
        (Outcome::ErrandStarted { errand: errand.clone() }, "AI 代打 0223456789：撥號中", "AI call to 0223456789: 撥號中"),
        (Outcome::Errands { errands: vec![] }, "還沒有 AI 代打的電話。", "No AI calls yet."),
        (Outcome::ModeFailed { phone: "Pixel 7a".into(), reason: "in_call".into() }, "Pixel 7a 沒有切換：手機通話中", "Pixel 7a was not switched: it is in a call"),
        (Outcome::ModeSet { phone: "Pixel 7a".into(), mode: "ai".into() }, "Pixel 7a：AI 接聽", "Pixel 7a: AI answers"),
        (Outcome::TooMany { limit: 5 }, "這個聊天室一小時內已叫 AI 打了 5 通，到上限了。晚點再試，或到網頁控制台操作。", "This chat has started 5 AI calls in the last hour, the limit. Try later or use the web console."),
        (Outcome::Failed { code: "not_allowed".into() }, "緊急、短碼和加值付費號碼一律不撥。", "Emergency, short and premium-rate numbers are never dialed."),
        (Outcome::Failed { code: "weird".into() }, "沒有成功（weird）。", "That didn't work (weird)."),
        (Outcome::Contacts { contacts: vec![ContactLine { name: "王小明".into(), phones: vec!["0912345678".into()] }] }, "聯絡人\n1. 王小明 — 0912345678", "Contacts\n1. 王小明 — 0912345678"),
        (Outcome::Confirm { code: "AB12CD".into(), summary: "把 Pixel 9 切到「一般手機」？".into() }, "把 Pixel 9 切到「一般手機」？\n\n2 分鐘內回覆 1 執行、2 取消（確認碼 AB12CD）。", "把 Pixel 9 切到「一般手機」？\n\nReply 1 to go ahead or 2 to drop it, within 2 minutes (code AB12CD)."),
    ];
    for (o, zh, en) in cases {
        assert_eq!(words::render(&o, words(Lang::ZhTw)), zh, "{o:?}");
        assert_eq!(words::render(&o, words(Lang::En)), en, "{o:?}");
    }
    assert_eq!(words::ask_errand(words(Lang::ZhTw), "", "0912345678", "訂位"), "讓 AI 打給 0912345678：\n訂位");
    assert_eq!(words::ask_errand(words(Lang::En), "王小明", "0912345678", "book"), "Have the AI call 王小明 (0912345678) to:\n book".replace("\n ", "\n"));
    assert_eq!(words::ask_mode(words(Lang::En), "Pixel 9", "normal"), "Switch Pixel 9 to “normal phone”?");
    let help = words::render(&Outcome::Help, words(Lang::En));
    assert!(help.contains("/errand <number or contact> <what to get done> — Have the AI make a call for you"), "{help}");
}

#[test]
fn every_language_has_a_menu_telegram_takes() {
    for lang in crate::locale::ALL {
        let w = words(lang);
        for (command, what) in words::menu(w) {
            assert!(command.len() <= 32 && command.chars().all(|c| c.is_ascii_lowercase()), "{command}");
            assert!(!what.trim().is_empty() && what.chars().count() <= 256, "{lang:?} {command}");
        }
        for t in [w.usage, w.live, w.started, w.cancelled, w.mode_done, w.mode_failed, w.confirm_how, w.too_many, w.not_found, w.no_contact, w.many_contacts, w.failed, w.question_how] {
            assert!(t.contains("{1}"), "{lang:?}: {t}");
        }
        assert!(w.ask_errand.contains("{2}") && w.question.contains("{2}"), "{lang:?}");
    }
}

#[tokio::test]
async fn the_ais_question_reaches_the_command_chat_and_a_late_press_is_too_late() {
    let rig = Rig::with_env_bot();
    let c = rig.controller();
    let q = crate::errands::OwnerQuestion {
        errand_id: "e0123456789abcdef".into(),
        question_id: "q1".into(),
        who: "王小明".into(),
        text: "對方問七點還是八點？".into(),
        options: vec!["七點".into(), "八點".into()],
        wait: Duration::from_secs(40),
    };
    let hook = c.question_hook();
    assert!(!hook(&q), "nobody listens: the AI does not wait");
    let mut rx = c.subscribe();
    assert!(hook(&q));
    let push = rx.try_recv().unwrap();
    assert_eq!((push.channel_id.as_str(), push.chat_id.as_str()), (ENV_TELEGRAM, "42"));
    assert!(push.out.text.contains("對方問七點還是八點？") && push.out.text.contains("40"), "{}", push.out.text);
    assert_eq!(push.out.buttons[1].data, "q:e0123456789abcdef:q1:1");
    // The errand is gone (or its question closed): the press says so.
    let late = c.handle(&press("q:e0123456789abcdef:q1:1")).await.unwrap();
    assert!(!late.text.is_empty());
    let wrong = c.handle(&press("q:e0123456789abcdef:q9:0")).await.unwrap();
    assert!(reads_as(&wrong.text, |w| w.closed.to_string()), "{}", wrong.text);
    // Commands off: no question goes out.
    rig.set(json!({"telegram_control": false}));
    assert!(!hook(&q));
}

#[test]
fn old_settings_files_load_with_commands_off_and_the_page_never_sees_a_token() {
    let rig = Rig::new();
    let file = rig.root.join("config").join("settings.json");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        r#"{"notify_channels":[{"id":"a1","kind":"telegram","name":"Me","enabled":true,"url":"","token":"456:OLDTOKEN","target":"42"}],"errand_identity":"owner"}"#,
    )
    .unwrap();
    let store = SettingsStore::load(rig.state.settings.clone());
    let rt = store.snapshot();
    assert_eq!(rt.errand_identity, "owner");
    assert!(!rt.notify_channels[0].control && rt.notify_channels[0].control_users.is_empty());
    assert!(!rt.telegram_control);
    assert_eq!((rt.control_errands_per_hour, rt.control_commands_per_minute), (5, 20));
    let page = store.masked_json();
    assert_eq!(page["notify_channels"][0]["control"], false);
    assert!(!page.to_string().contains("456:OLDTOKEN"));
    // A row coming back from the page keeps its token and takes the switch.
    let saved = store
        .apply_patch(&json!({"notify_channels": [{"id": "a1", "kind": "telegram", "enabled": true, "token": "", "target": "42", "control": true, "control_users": ["42"]}], "control_errands_per_hour": 99}))
        .unwrap();
    assert_eq!(saved.notify_channels[0].token, "456:OLDTOKEN");
    assert!(saved.notify_channels[0].control);
    assert_eq!(saved.control_errands_per_hour, 30);
}

#[tokio::test]
async fn the_telegram_bot_answers_its_chat_and_hands_card_presses_on() {
    let rig = Rig::with_env_bot();
    let c = rig.controller();
    let updates = json!({"ok": true, "result": [
        {"update_id": 1, "message": {"message_id": 5, "from": {"id": 42}, "chat": {"id": 42, "type": "private"}, "text": "/help"}},
        {"update_id": 2, "message": {"message_id": 6, "from": {"id": 66}, "chat": {"id": 66, "type": "private"}, "text": "/help"}},
        {"update_id": 3, "callback_query": {"id": "cb", "from": {"id": 42}, "data": "t:task-9:i", "message": {"message_id": 8, "chat": {"id": 42}}}},
    ]});
    let me = json!({"ok": true, "result": {"id": 123, "is_bot": true, "username": "bageo_test_bot"}});
    let (api, seen) = fake_api::serve(vec![("getMe", me.to_string()), ("getUpdates", updates.to_string())]).await;
    let bot = access::telegram_bots(&rig.state.settings_store.snapshot()).remove(0);
    let (cards, mut card_rx) = tokio::sync::mpsc::unbounded_channel();
    let task = tokio::spawn(telegram::run_bot(c, reqwest::Client::new(), api, bot, cards));
    let card = tokio::time::timeout(Duration::from_secs(5), card_rx.recv()).await.unwrap().unwrap();
    assert_eq!((card.task_id.as_str(), card.confirm), ("task-9", false));
    let mut replies = Vec::new();
    for _ in 0..50 {
        replies = fake_api::sent(&seen, "sendMessage");
        if !replies.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    task.abort();
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(replies[0]["chat_id"], "42");
    assert!(replies[0]["text"].as_str().unwrap().contains("/errand"));
    assert_eq!(fake_api::sent(&seen, "setMyCommands")[0]["commands"].as_array().unwrap().len(), command::COMMANDS.len());
}
// ---- Acceptance review (correctness): edge cases of the confirmation and the questions.

#[tokio::test]
async fn review_a_yes_after_the_web_decided_the_to_do_does_not_undo_it() {
    let rig = Rig::with_env_bot();
    let c = rig.controller();
    let id = rig.task("task-review-1", "王小明想改約時間").await;
    let ask = c.handle(&owner(&format!("/ignore {id}"))).await.unwrap();
    assert_eq!(ask.buttons.len(), 2, "{}", ask.text);
    // Meanwhile the owner accepts it on the web console (it now waits for the phone).
    rig.state.task_store.update_status(&id, TaskStatus::Confirmed).await.unwrap();
    let late = c.handle(&owner("1")).await.unwrap();
    assert!(
        matches!(rig.task_status(&id).await, TaskStatus::Confirmed),
        "the chat's yes to a pending to-do overrode the web console's decision: {}",
        late.text
    );
}

#[tokio::test]
async fn review_a_typed_1_from_another_member_leaves_the_owners_confirmation_waiting() {
    let rig = Rig::with_env_bot();
    let c = rig.controller();
    let id = rig.task("task-review-2", "回電給王小明").await;
    let ask = c.handle(&owner(&format!("/ignore {id}"))).await.unwrap();
    assert_eq!(ask.buttons.len(), 2, "{}", ask.text);
    let other = c.handle(&from(ENV_TELEGRAM, "42", "43", Body::Text("1".into()))).await.unwrap();
    assert!(reads_as(&other.text, |w| w.expired.to_string()), "{}", other.text);
    assert!(matches!(rig.task_status(&id).await, TaskStatus::Pending));
    // The owner's typed 1 works like the button (a new message, no edit).
    let done = c.handle(&owner("1")).await.unwrap();
    assert!(done.edit.is_none());
    assert!(matches!(rig.task_status(&id).await, TaskStatus::Ignored), "{}", done.text);
}

#[test]
fn review_a_long_reply_is_cut_to_4000_characters() {
    let rig = Rig::with_env_bot();
    let c = rig.controller();
    let rt = rig.state.settings_store.snapshot();
    let out = c.scrub(&rt, Outbound { text: "字".repeat(5000), buttons: vec![], edit: None });
    assert_eq!(out.text.chars().count(), 4000);
    assert!(out.text.ends_with('…'));
}

#[tokio::test]
async fn review_a_question_is_not_counted_as_sent_to_a_chat_no_bot_can_reach() {
    let rig = Rig::new();
    rig.set(json!({
        "telegram_bot_token": "123:TESTTOKEN",
        "telegram_chat_id": "42",
        "notify_channels": [{"kind": "telegram", "enabled": true, "token": "", "target": "-1001", "control": true}],
    }));
    let rt = rig.state.settings_store.snapshot();
    assert!(rt.notify_channels[0].control, "{:?}", rt.notify_channels);
    let bots = access::telegram_bots(&rt);
    assert!(bots.iter().all(|b| b.chats.iter().all(|(_, chat)| chat != "-1001")), "no bot serves that chat: {bots:?}");
    let c = rig.controller();
    // The environment's bot is polled for its to-do cards, so a receiver exists.
    let _env_bot = c.subscribe();
    let q = crate::errands::OwnerQuestion {
        errand_id: "e0123456789abcdef".into(),
        question_id: "q1".into(),
        who: "王小明".into(),
        text: "七點還是八點？".into(),
        options: vec![],
        wait: Duration::from_secs(40),
    };
    assert!(!(c.question_hook())(&q), "the AI would wait 40 s on the phone for an answer no chat can give");
}

/// The fixed word is Bageo (UI_TEXT_RULES), in every language.
#[test]
fn the_status_texts_say_bageo_and_not_the_agent() {
    for lang in crate::locale::ALL {
        let w = words(lang);
        for t in w.menu.iter().map(|(_, what)| *what).chain([w.no_phones]) {
            let low = t.to_lowercase();
            assert!(!low.contains("agent") && !t.contains("エージェント") && !t.contains("에이전트"), "{lang:?}: {t}");
        }
        assert!(w.no_phones.contains("Bageo"), "{lang:?}: {}", w.no_phones);
        let status = w.menu.iter().map(|(_, what)| *what).find(|what| what.contains("Bageo"));
        assert!(status.is_some(), "{lang:?}: the status line names Bageo");
    }
}

#[test]
fn words_fmt_does_not_reexpand_placeholders_inside_earlier_arguments() {
    assert_eq!(words::fmt("{1}: {2}", &["literal {2}", "real"]), "literal {2}: real");
}
