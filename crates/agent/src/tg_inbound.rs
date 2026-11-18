// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Telegram Bot API: the to-do cards' buttons, and the messages and button
//! presses of the chats that take commands ([crate::control]).
//!
//! Outbound task cards use [send_task_card] (Bot API + inline keyboard; Apprise
//! cannot carry buttons). Inbound: [TgPoller::poll_once] long-polls
//! getUpdates for one bot. A card button comes back as a [TgAction] (handled
//! in main.rs); any other message or button press as a [TgIncoming], for
//! crate::control::telegram, which decides whether it may be answered.
//!
//! **Not** gated by spam-blocklist (deny removed). Outbound cards share the
//! CALL_NOTIFY / call_notify master switch via [task_card_ready].
//! Call/VM plain alerts stay in [crate::notify] (Apprise → webhook → Bot API).

use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::control::{Button, Outbound};

/// The Bot API's address.
pub const TELEGRAM_API: &str = "https://api.telegram.org";

/// Telegram inline-keyboard action: user confirmed or ignored an AI task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TgAction {
    pub task_id: String,
    pub confirm: bool,
    pub chat_id: String,
    pub message_id: i64,
}

/// A message or a (non-card) button press in a chat, for the chat commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TgIncoming {
    pub chat_id: String,
    /// The Telegram user id of who wrote or pressed.
    pub sender_id: String,
    pub text: Option<String>,
    /// The pressed button's callback data.
    pub button: Option<String>,
    /// The message, or the message carrying the pressed button.
    pub message_id: i64,
    /// It answers a message of this bot (a button press always does).
    pub reply_to_bot: bool,
}

/// What one long poll brought.
#[derive(Debug, Default)]
pub struct Polled {
    pub actions: Vec<TgAction>,
    pub incoming: Vec<TgIncoming>,
}

/// The bot itself (getMe).
#[derive(Debug, Clone, PartialEq, Eq)]
struct BotMe {
    id: i64,
    username: String,
}

enum Callback {
    Card(TgAction),
    Other(TgIncoming),
}

/// Long-poll state for Telegram getUpdates.
pub struct TgPoller {
    /// Next offset for getUpdates (last processed update_id + 1).
    offset: i64,
    /// When set, card callbacks from other chats are dropped (security).
    allowed_chat_id: Option<String>,
    /// Card callbacks count only from a sender on each of these lists.
    card_senders: Vec<Vec<String>>,
    api: String,
    me: Option<BotMe>,
}

/// Telegram callback_data limit (bytes).
const MAX_CALLBACK_DATA: usize = 64;

/// Long-poll timeout passed to getUpdates (seconds).
const GET_UPDATES_TIMEOUT_SECS: u32 = 10;

impl TgPoller {
    pub fn new() -> Self {
        Self::with_api(TELEGRAM_API)
    }

    /// A poller of the Bot API at another address (tests).
    pub fn with_api(api: &str) -> Self {
        Self {
            offset: 0,
            allowed_chat_id: None,
            card_senders: Vec::new(),
            api: api.trim_end_matches('/').to_string(),
            me: None,
        }
    }

    /// Card buttons count only from a sender on each of these lists (the
    /// owner narrowed the chat to some of its members); none: anyone in it.
    pub fn set_card_senders(&mut self, lists: Vec<Vec<String>>) {
        self.card_senders = lists.into_iter().filter(|l| !l.is_empty()).collect();
    }

    /// Restrict inbound card callbacks to this chat (RuntimeSettings.telegram_chat_id).
    pub fn set_allowed_chat_id(&mut self, chat_id: &str) {
        let trimmed = chat_id.trim();
        self.allowed_chat_id = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }

    /// Who the bot is (getMe): its id tells a reply to the bot from other
    /// replies, its username a command for it in a group ("/calls@my_bot").
    /// Asked once; without an answer the id is the token's first part and
    /// commands naming a bot are not taken.
    async fn ensure_me(&mut self, http: &Client, token: &str) {
        if self.me.is_some() {
            return;
        }
        let answer: Option<Value> = match http.post(method_url(&self.api, token, "getMe")).json(&json!({})).send().await {
            Ok(resp) => resp.json().await.ok(),
            Err(_) => None,
        };
        let me = answer
            .filter(|v| v.get("ok").and_then(Value::as_bool) == Some(true))
            .and_then(|v| {
                Some(BotMe {
                    id: v.pointer("/result/id")?.as_i64()?,
                    username: v.pointer("/result/username").and_then(Value::as_str).unwrap_or("").to_string(),
                })
            });
        self.me = Some(me.unwrap_or_else(|| BotMe {
            id: token.split(':').next().and_then(|s| s.parse().ok()).unwrap_or(0),
            username: String::new(),
        }));
    }

    /// One long-poll cycle: fetch updates, answer callbacks, strip card keyboards.
    pub async fn poll_once(&mut self, http: &Client, bot_token: &str) -> Result<Polled> {
        let token = bot_token.trim();
        if token.is_empty() {
            return Ok(Polled::default());
        }
        self.ensure_me(http, token).await;

        let resp = http
            .post(method_url(&self.api, token, "getUpdates"))
            .json(&json!({
                "offset": self.offset,
                "timeout": GET_UPDATES_TIMEOUT_SECS,
                "allowed_updates": ["message", "callback_query"],
            }))
            .send()
            .await
            .context("telegram getUpdates request")?;

        let body: Value = resp
            .json()
            .await
            .context("telegram getUpdates response JSON")?;

        if body.get("ok").and_then(Value::as_bool) != Some(true) {
            let desc = body
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            anyhow::bail!("telegram getUpdates not ok: {desc}");
        }

        let updates = body
            .get("result")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut polled = Polled::default();

        for update in updates {
            let update_id = update.get("update_id").and_then(Value::as_i64).unwrap_or(0);
            if update_id >= self.offset {
                self.offset = update_id + 1;
            }

            if let Some(msg) = update.get("message") {
                if let Some(inc) = self.read_message(msg) {
                    polled.incoming.push(inc);
                }
                continue;
            }
            match self
                .handle_callback_query(http, token, update.get("callback_query"))
                .await?
            {
                Some(Callback::Card(action)) => polled.actions.push(action),
                Some(Callback::Other(inc)) => polled.incoming.push(inc),
                None => {}
            }
        }

        Ok(polled)
    }

    /// A text message the bot should see. In a group: a command (for this
    /// bot) or a reply to the bot; everything else there is people talking.
    fn read_message(&self, msg: &Value) -> Option<TgIncoming> {
        if msg.pointer("/from/is_bot").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        // Someone else's words passed on (a forward) are no command of the owner's.
        if ["forward_origin", "forward_date", "forward_from", "forward_from_chat", "forward_sender_name"]
            .iter()
            .any(|k| msg.get(*k).is_some())
        {
            return None;
        }
        let text = msg.get("text")?.as_str()?.trim();
        if text.is_empty() {
            return None;
        }
        let chat = msg.get("chat")?;
        let chat_id = json_chat_id_string(chat.get("id")?);
        let private = chat.get("type").and_then(Value::as_str) == Some("private");
        let me = self.me.as_ref();
        let reply_to_bot = me.is_some_and(|me| {
            me.id != 0 && msg.pointer("/reply_to_message/from/id").and_then(Value::as_i64) == Some(me.id)
        });
        let text = for_this_bot(text, me.map(|m| m.username.as_str()).unwrap_or(""))?;
        if !private && !text.starts_with('/') && !reply_to_bot {
            return None;
        }
        Some(TgIncoming {
            chat_id,
            sender_id: msg.pointer("/from/id").map(json_chat_id_string).unwrap_or_default(),
            text: Some(text),
            button: None,
            message_id: msg.get("message_id").and_then(Value::as_i64).unwrap_or(0),
            reply_to_bot,
        })
    }

    async fn handle_callback_query(
        &self,
        http: &Client,
        bot_token: &str,
        query: Option<&Value>,
    ) -> Result<Option<Callback>> {
        let Some(query) = query else {
            return Ok(None);
        };

        let callback_id = match query.get("id").and_then(Value::as_str) {
            Some(id) => id,
            None => return Ok(None),
        };

        let data = match query.get("data").and_then(Value::as_str) {
            Some(d) => d,
            None => {
                answer_callback_query(http, &self.api, bot_token, callback_id, None).await;
                return Ok(None);
            }
        };

        let message = query.get("message");
        let chat_id_val = message
            .and_then(|m| m.get("chat"))
            .and_then(|c| c.get("id"));
        let message_id = message
            .and_then(|m| m.get("message_id"))
            .and_then(Value::as_i64);

        let chat_id = match chat_id_val {
            Some(id) => json_chat_id_string(id),
            None => {
                answer_callback_query(http, &self.api, bot_token, callback_id, None).await;
                return Ok(None);
            }
        };

        let Some((task_id, confirm)) = decode_callback_data(data) else {
            // Not a to-do card's: a button of the chat commands, which decide
            // whether this chat and sender may press it.
            answer_callback_query(http, &self.api, bot_token, callback_id, None).await;
            return Ok(Some(Callback::Other(TgIncoming {
                chat_id,
                sender_id: query.pointer("/from/id").map(json_chat_id_string).unwrap_or_default(),
                text: None,
                button: Some(data.to_string()),
                message_id: message_id.unwrap_or(0),
                reply_to_bot: true,
            })));
        };

        if let Some(expected) = self.allowed_chat_id.as_deref() {
            if !chat_id_matches(expected, chat_id_val.unwrap_or(&Value::Null)) {
                warn!(
                    "telegram callback rejected chat_id={chat_id} expected={expected} task_id={task_id}"
                );
                answer_callback_query(http, &self.api, bot_token, callback_id, None).await;
                return Ok(None);
            }
        }

        let sender = query.pointer("/from/id").map(json_chat_id_string).unwrap_or_default();
        // A group (negative chat id) with no list of who may press: nobody may.
        // A private chat is the owner's own.
        if self.card_senders.is_empty() && chat_id.trim().parse::<i64>().is_ok_and(|id| id < 0) {
            warn!("telegram callback rejected sender={sender} chat_id={chat_id} task_id={task_id}: a group with no sender list");
            answer_callback_query(http, &self.api, bot_token, callback_id, None).await;
            return Ok(None);
        }
        if !self.card_senders.iter().all(|l| !sender.is_empty() && l.iter().any(|u| u.trim() == sender)) {
            warn!("telegram callback rejected sender={sender} chat_id={chat_id} task_id={task_id}: not on the sender list");
            answer_callback_query(http, &self.api, bot_token, callback_id, None).await;
            return Ok(None);
        }

        let p = crate::locale::notify_phrases();
        let ack = if confirm { p.tc_confirm_ack } else { p.tc_ignore_ack };
        answer_callback_query(http, &self.api, bot_token, callback_id, Some(ack)).await;

        if let Some(mid) = message_id {
            remove_keyboard_at(http, &self.api, bot_token, &chat_id, mid).await;
        }

        info!(
            "telegram task action task_id={task_id} confirm={confirm} chat_id={chat_id} message_id={message_id:?}"
        );

        Ok(Some(Callback::Card(TgAction {
            task_id,
            confirm,
            chat_id,
            message_id: message_id.unwrap_or(0),
        })))
    }
}

impl Default for TgPoller {
    fn default() -> Self {
        Self::new()
    }
}

/// A command naming a bot ("/calls@some_bot") is this bot's only when it names
/// it (the name is dropped then); other text is as it is.
fn for_this_bot(text: &str, username: &str) -> Option<String> {
    if !text.starts_with('/') {
        return Some(text.to_string());
    }
    let (head, rest) = match text.find(char::is_whitespace) {
        Some(i) => (&text[..i], &text[i..]),
        None => (text, ""),
    };
    match head.split_once('@') {
        None => Some(text.to_string()),
        Some((command, name)) if !username.is_empty() && name.eq_ignore_ascii_case(username) => Some(format!("{command}{rest}")),
        Some(_) => None,
    }
}

/// Whether to send a new task card: call_notify on + bot token + chat id.
/// Independent of spam-blocklist (always allowed from that gate).
pub fn task_card_ready(call_notify: bool, bot_token: &str, chat_id: &str) -> bool {
    call_notify && !bot_token.trim().is_empty() && !chat_id.trim().is_empty()
}

/// Send an AI task card with inline confirm / ignore buttons.
pub async fn send_task_card(
    http: &Client,
    bot_token: &str,
    chat_id: &str,
    task_summary_title: &str,
    task_id: &str,
    body_text: &str,
) -> Result<()> {
    let p = crate::locale::notify_phrases();
    send_task_card_labeled(
        http,
        bot_token,
        chat_id,
        task_summary_title,
        task_id,
        body_text,
        p.tc_confirm,
        p.tc_ignore,
    )
    .await
    .map(|_| ())
}

/// [send_task_card] with its own button texts (appointments: "確定" / "婉拒").
/// The buttons carry the same confirm / ignore data. Returns the card's
/// message_id (0 when Telegram did not say), so its buttons can be taken off
/// when the owner decides somewhere else.
#[allow(clippy::too_many_arguments)]
pub async fn send_task_card_labeled(
    http: &Client,
    bot_token: &str,
    chat_id: &str,
    task_summary_title: &str,
    task_id: &str,
    body_text: &str,
    confirm_label: &str,
    ignore_label: &str,
) -> Result<i64> {
    send_task_card_labeled_at(
        http,
        TELEGRAM_API,
        bot_token,
        chat_id,
        task_summary_title,
        task_id,
        body_text,
        confirm_label,
        ignore_label,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn send_task_card_labeled_at(
    http: &Client,
    api: &str,
    bot_token: &str,
    chat_id: &str,
    task_summary_title: &str,
    task_id: &str,
    body_text: &str,
    confirm_label: &str,
    ignore_label: &str,
) -> Result<i64> {
    let token = bot_token.trim();
    let chat = chat_id.trim();
    if token.is_empty() || chat.is_empty() {
        anyhow::bail!("telegram bot token and chat id are required");
    }

    let title = task_summary_title.trim();
    let body = body_text.trim();
    let text = if title.is_empty() {
        body.to_string()
    } else if body.is_empty() {
        title.to_string()
    } else {
        format!("{title}\n\n{body}")
    };
    let text = clip_message(&crate::locale::for_owner(&text));

    let url = method_url(api, token, "sendMessage");
    let payload = task_card_payload(chat, &text, task_id, confirm_label, ignore_label);
    let delays = [std::time::Duration::from_millis(1500), std::time::Duration::from_millis(3000)];
    let mut attempt = 0usize;
    let body_json: Value = loop {
        let send_res = http.post(&url).json(&payload).send().await;
        let retry_delay = match send_res {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    break resp
                        .json()
                        .await
                        .context("telegram sendMessage response JSON")?;
                }
                let err_body = resp.text().await.unwrap_or_default();
                let transient = status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
                if !transient || attempt >= delays.len() {
                    anyhow::bail!("telegram sendMessage HTTP {status}: {err_body}");
                }
                let retry_after = (status == reqwest::StatusCode::TOO_MANY_REQUESTS)
                    .then(|| {
                        serde_json::from_str::<Value>(&err_body)
                            .ok()
                            .and_then(|v| v.pointer("/parameters/retry_after").and_then(Value::as_u64))
                    })
                    .flatten()
                    .map(|s| std::time::Duration::from_secs(s.min(5)));
                retry_after.unwrap_or(delays[attempt])
            }
            Err(e) => {
                if attempt >= delays.len() {
                    return Err(e).context("telegram sendMessage task card");
                }
                delays[attempt]
            }
        };
        warn!("telegram task card send transient error (attempt {}), retrying in {}ms", attempt + 1, retry_delay.as_millis());
        tokio::time::sleep(retry_delay).await;
        attempt += 1;
    };

    if body_json.get("ok").and_then(Value::as_bool) != Some(true) {
        let desc = body_json
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        anyhow::bail!("telegram sendMessage not ok: {desc}");
    }

    let message_id = sent_message_id(&body_json);
    info!("telegram task card sent task_id={task_id} chat_id={chat} message_id={message_id}");
    Ok(message_id)
}

/// The message_id of a sendMessage answer ({"ok": true, "result": {"message_id": …}}); 0 when missing.
fn sent_message_id(answer: &Value) -> i64 {
    answer
        .pointer("/result/message_id")
        .and_then(Value::as_i64)
        .unwrap_or(0)
}

/// The sendMessage body of a task card: the text and the two buttons.
fn task_card_payload(chat: &str, text: &str, task_id: &str, confirm_label: &str, ignore_label: &str) -> Value {
    json!({
        "chat_id": chat,
        "text": text,
        "disable_web_page_preview": true,
        "reply_markup": {
            "inline_keyboard": [[
                { "text": confirm_label, "callback_data": encode_callback_data(task_id, true) },
                { "text": ignore_label, "callback_data": encode_callback_data(task_id, false) },
            ]],
        },
    })
}

/// Telegram Bot API hard limit for sendMessage.text.
const MAX_MESSAGE_TEXT: usize = 4096;

fn clip_message(text: &str) -> String {
    let mut utf16 = 0usize;
    let mut exceeded = false;
    for c in text.chars() {
        utf16 += c.len_utf16();
        if utf16 > MAX_MESSAGE_TEXT {
            exceeded = true;
            break;
        }
    }
    if !exceeded {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    let limit = MAX_MESSAGE_TEXT - 1;
    for c in text.chars() {
        let w = c.len_utf16();
        if used + w > limit {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// The text of a plain follow-up: in the owner's script, within Telegram's
/// limit. The limit counts characters (not bytes: Chinese is three bytes
/// each), so only a text with more than that many characters is cut.
fn plain_body(text: &str) -> String {
    clip_message(&crate::locale::for_owner(text.trim()))
}

/// Best-effort plain follow-up after confirm/ignore (fulfill outcome).
pub async fn send_plain_message(
    http: &Client,
    bot_token: &str,
    chat_id: &str,
    text: &str,
) -> Result<()> {
    let token = bot_token.trim();
    let chat = chat_id.trim();
    let body = plain_body(text);
    if token.is_empty() || chat.is_empty() {
        anyhow::bail!("telegram bot token and chat id are required");
    }
    if body.is_empty() {
        return Ok(());
    }

    let url = bot_method_url(token, "sendMessage");
    let resp = http
        .post(&url)
        .json(&json!({
            "chat_id": chat,
            "text": body,
            "disable_web_page_preview": true,
        }))
        .send()
        .await
        .context("telegram sendMessage plain")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err_body = resp.text().await.unwrap_or_default();
        anyhow::bail!("telegram sendMessage HTTP {status}: {err_body}");
    }
    Ok(())
}

/// The inline keyboard for a reply's buttons: one row when they are few and
/// short (yes / no), else one per row (an answer may be a whole phrase).
fn keyboard(buttons: &[Button]) -> Option<Value> {
    if buttons.is_empty() {
        return None;
    }
    let cell = |b: &Button| json!({"text": b.label, "callback_data": b.data});
    let short = buttons.len() <= 3 && buttons.iter().map(|b| b.label.chars().count()).sum::<usize>() <= 30;
    let rows: Vec<Value> = if short {
        vec![Value::Array(buttons.iter().map(cell).collect())]
    } else {
        buttons.iter().map(|b| json!([cell(b)])).collect()
    };
    Some(json!({ "inline_keyboard": rows }))
}

/// POST one Bot API method; Err unless Telegram says ok.
async fn post_ok(http: &Client, url: &str, payload: &Value) -> Result<()> {
    let resp = http.post(url).json(payload).send().await.context("telegram request")?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() || body.get("ok").and_then(Value::as_bool) != Some(true) {
        let desc = body.get("description").and_then(Value::as_str).unwrap_or("unknown error");
        bail!("telegram HTTP {}: {desc}", status.as_u16());
    }
    Ok(())
}

/// A chat command's answer: a new message, or (edit) the message whose button
/// was pressed rewritten, its buttons replaced. An edit Telegram refuses (the
/// message is gone or too old) is sent as a new message.
pub async fn send_outbound(http: &Client, api: &str, bot_token: &str, chat_id: &str, out: &Outbound) -> Result<()> {
    let token = bot_token.trim();
    let chat = chat_id.trim();
    if token.is_empty() || chat.is_empty() {
        bail!("telegram bot token and chat id are required");
    }
    let text = clip_message(out.text.trim());
    if text.is_empty() {
        return Ok(());
    }
    let markup = keyboard(&out.buttons);
    if let Some(mid) = out.edit.as_deref().and_then(|m| m.trim().parse::<i64>().ok()) {
        let payload = json!({
            "chat_id": chat,
            "message_id": mid,
            "text": text,
            "disable_web_page_preview": true,
            "reply_markup": markup.clone().unwrap_or_else(|| json!({ "inline_keyboard": [] })),
        });
        match post_ok(http, &method_url(api, token, "editMessageText"), &payload).await {
            Ok(()) => return Ok(()),
            Err(e) => debug!("telegram edit refused, sending anew: {}", redact(&format!("{e:#}"), token)),
        }
    }
    let mut payload = json!({ "chat_id": chat, "text": text, "disable_web_page_preview": true });
    if let Some(m) = markup {
        payload["reply_markup"] = m;
    }
    post_ok(http, &method_url(api, token, "sendMessage"), &payload).await
}

/// The bot's command menu, set only for the chats that take commands (scope
/// "chat"): strangers who open the bot see none. The menu for every chat is
/// removed first (older versions set it); an empty list removes the menu of
/// the chats given too.
pub async fn set_my_commands(http: &Client, api: &str, bot_token: &str, commands: &[(&str, &str)], chats: &[String]) {
    let token = bot_token.trim();
    if token.is_empty() {
        return;
    }
    let scope = |chat: &str| match chat.trim().parse::<i64>() {
        Ok(id) => json!({"type": "chat", "chat_id": id}),
        Err(_) => json!({"type": "chat", "chat_id": chat.trim()}),
    };
    let mut calls: Vec<(&str, Value)> = vec![("deleteMyCommands", json!({}))];
    if commands.is_empty() {
        for chat in chats.iter().filter(|c| !c.trim().is_empty()) {
            calls.push(("deleteMyCommands", json!({ "scope": scope(chat) })));
        }
    } else {
        let list: Vec<Value> = commands
            .iter()
            .map(|(c, d)| json!({"command": c, "description": d.chars().take(256).collect::<String>()}))
            .collect();
        for chat in chats.iter().filter(|c| !c.trim().is_empty()) {
            calls.push(("setMyCommands", json!({ "commands": list, "scope": scope(chat) })));
        }
    }
    for (method, payload) in calls {
        if let Err(e) = post_ok(http, &method_url(api, token, method), &payload).await {
            debug!("telegram {method}: {}", redact(&format!("{e:#}"), token));
        }
    }
}

/// Text without the bot token: a request error names its URL, and the token is in it.
pub fn redact(text: &str, bot_token: &str) -> String {
    let t = bot_token.trim();
    if t.is_empty() {
        text.to_string()
    } else {
        text.replace(t, "<token>")
    }
}

fn method_url(api: &str, bot_token: &str, method: &str) -> String {
    format!("{}/bot{bot_token}/{method}", api.trim_end_matches('/'))
}

fn bot_method_url(bot_token: &str, method: &str) -> String {
    method_url(TELEGRAM_API, bot_token, method)
}

/// Encode callback payload. Prefers t:{uuid}:c|i (fits full UUID under 64 bytes).
fn encode_callback_data(task_id: &str, confirm: bool) -> String {
    let action = if confirm { "c" } else { "i" };
    let short = format!("t:{task_id}:{action}");
    if short.len() <= MAX_CALLBACK_DATA {
        return short;
    }

    // Long-form fallback before truncation.
    let long_action = if confirm { "confirm" } else { "ignore" };
    let long = format!("task:{task_id}:{long_action}");
    if long.len() <= MAX_CALLBACK_DATA {
        return long;
    }

    // Truncate task id from the end; fulfill side must map short ids if used.
    let prefix = "t:";
    let suffix = format!(":{action}");
    let budget = MAX_CALLBACK_DATA.saturating_sub(prefix.len() + suffix.len());
    let truncated: String = task_id.chars().take(budget).collect();
    format!("t:{truncated}:{action}")
}

/// Decode callback payload from short or long format.
fn decode_callback_data(data: &str) -> Option<(String, bool)> {
    if let Some(rest) = data.strip_prefix("t:") {
        let (id, action) = rest.rsplit_once(':')?;
        let confirm = match action {
            "c" => true,
            "i" => false,
            _ => return None,
        };
        if id.is_empty() {
            return None;
        }
        return Some((id.to_string(), confirm));
    }

    if let Some(rest) = data.strip_prefix("task:") {
        let (id, action) = rest.rsplit_once(':')?;
        let confirm = match action {
            "confirm" => true,
            "ignore" => false,
            _ => return None,
        };
        if id.is_empty() {
            return None;
        }
        return Some((id.to_string(), confirm));
    }

    None
}

fn json_chat_id_string(id: &Value) -> String {
    match id {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.trim().to_string(),
        _ => String::new(),
    }
}

fn chat_id_matches(expected: &str, actual: &Value) -> bool {
    json_chat_id_string(actual) == expected.trim()
}

async fn answer_callback_query(
    http: &Client,
    api: &str,
    bot_token: &str,
    callback_query_id: &str,
    text: Option<&str>,
) {
    let mut payload = json!({ "callback_query_id": callback_query_id });
    if let Some(t) = text {
        payload["text"] = json!(t);
    }

    match http.post(method_url(api, bot_token, "answerCallbackQuery")).json(&payload).send().await {
        Ok(resp) if resp.status().is_success() => {}
        Ok(resp) => warn!(
            "telegram answerCallbackQuery HTTP {}",
            resp.status().as_u16()
        ),
        Err(e) => warn!("telegram answerCallbackQuery failed: {}", redact(&e.to_string(), bot_token)),
    }
}

/// Best-effort: remove a card's inline keyboard (after a button press, or when
/// the owner decided on the web).
pub async fn remove_inline_keyboard(http: &Client, bot_token: &str, chat_id: &str, message_id: i64) {
    remove_keyboard_at(http, TELEGRAM_API, bot_token, chat_id, message_id).await
}

async fn remove_keyboard_at(http: &Client, api: &str, bot_token: &str, chat_id: &str, message_id: i64) {
    let payload = json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "reply_markup": { "inline_keyboard": [] },
    });

    match http.post(method_url(api, bot_token, "editMessageReplyMarkup")).json(&payload).send().await {
        Ok(resp) if resp.status().is_success() => {}
        Ok(resp) => debug!(
            "telegram editMessageReplyMarkup HTTP {} chat_id={chat_id} message_id={message_id}",
            resp.status().as_u16()
        ),
        Err(e) => debug!(
            "telegram editMessageReplyMarkup failed chat_id={chat_id} message_id={message_id}: {}",
            redact(&e.to_string(), bot_token)
        ),
    }
}

/// A Bot API stand-in on this machine, for tests here and in crate::control.
#[cfg(test)]
pub(crate) mod fake_api {
    use std::sync::{Arc, Mutex};

    use serde_json::Value;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// What it was sent: (method, JSON body).
    pub type Seen = Arc<Mutex<Vec<(String, Value)>>>;

    /// Answers each method with its next canned body, in order (a method
    /// without one left: {"ok": true, "result": true}, which getUpdates reads
    /// as no updates). Its address and what it was sent.
    pub async fn serve(answers: Vec<(&'static str, String)>) -> (String, Seen) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            let mut answers = answers;
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let (path, body) = read_request(&mut sock).await;
                let method = path.rsplit('/').next().unwrap_or("").to_string();
                let reply = match answers.iter().position(|(m, _)| m.eq_ignore_ascii_case(&method)) {
                    Some(i) => answers.remove(i).1,
                    None => r#"{"ok":true,"result":true}"#.to_string(),
                };
                log.lock().unwrap().push((method, serde_json::from_str(&body).unwrap_or(Value::Null)));
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (base, seen)
    }

    /// The methods it was asked, in order (as written in the path, lowercase).
    pub fn methods(seen: &Seen) -> Vec<String> {
        seen.lock().unwrap().iter().map(|(m, _)| m.clone()).collect()
    }

    /// The bodies sent to one method.
    pub fn sent(seen: &Seen, method: &str) -> Vec<Value> {
        seen.lock().unwrap().iter().filter(|(m, _)| m.eq_ignore_ascii_case(method)).map(|(_, b)| b.clone()).collect()
    }

    async fn read_request(sock: &mut tokio::net::TcpStream) -> (String, String) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = sock.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(h) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..h]).to_string();
                let len = head
                    .lines()
                    .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().to_string()))
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(0);
                if buf.len() >= h + 4 + len {
                    let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                    let body = String::from_utf8_lossy(&buf[h + 4..h + 4 + len]).into_owned();
                    return (path, body);
                }
            }
        }
        (String::new(), String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sent_card_reports_its_message_id() {
        assert_eq!(sent_message_id(&json!({"ok": true, "result": {"message_id": 4711, "chat": {"id": 1}}})), 4711);
        assert_eq!(sent_message_id(&json!({"ok": true})), 0);
    }

    #[test]
    fn encode_full_uuid_fits_short_form() {
        let uuid = "550e8400-e29b-41d4-a716-446655440000";
        let confirm = encode_callback_data(uuid, true);
        let ignore = encode_callback_data(uuid, false);
        assert!(confirm.len() <= MAX_CALLBACK_DATA);
        assert!(ignore.len() <= MAX_CALLBACK_DATA);
        assert_eq!(confirm, format!("t:{uuid}:c"));
        assert_eq!(ignore, format!("t:{uuid}:i"));
    }

    #[test]
    fn roundtrip_short_and_long_formats() {
        let id = "abc-123";
        assert_eq!(
            decode_callback_data(&encode_callback_data(id, true)),
            Some((id.to_string(), true))
        );
        assert_eq!(
            decode_callback_data(&encode_callback_data(id, false)),
            Some((id.to_string(), false))
        );
        assert_eq!(
            decode_callback_data("task:abc-123:confirm"),
            Some(("abc-123".to_string(), true))
        );
        assert_eq!(
            decode_callback_data("task:abc-123:ignore"),
            Some(("abc-123".to_string(), false))
        );
    }

    #[test]
    fn truncates_overlong_task_id() {
        let long_id = "x".repeat(80);
        let encoded = encode_callback_data(&long_id, true);
        assert!(encoded.len() <= MAX_CALLBACK_DATA);
        assert!(encoded.starts_with("t:"));
        assert!(encoded.ends_with(":c"));
    }

    #[test]
    fn chat_id_matches_number_and_string() {
        assert!(chat_id_matches("12345", &json!(12345)));
        assert!(chat_id_matches("-100123", &json!("-100123")));
        assert!(!chat_id_matches("12345", &json!(99999)));
    }

    #[test]
    fn plain_message_cap_constant() {
        assert_eq!(MAX_MESSAGE_TEXT, 4096);
    }

    #[test]
    fn a_long_chinese_follow_up_is_cut_by_characters_not_bytes() {
        // 3000 characters are 9000 bytes: well within Telegram's limit, so no ellipsis.
        let text = "字".repeat(3000);
        let body = plain_body(&text);
        assert_eq!(body.chars().count(), 3000);
        assert!(!body.ends_with('…'));
        // Over the limit: cut to it, an ellipsis marking the cut.
        let body = plain_body(&"字".repeat(5000));
        assert_eq!(body.chars().count(), MAX_MESSAGE_TEXT);
        assert!(body.ends_with('…'));
    }

    #[test]
    fn task_card_ready_respects_call_notify() {
        assert!(task_card_ready(true, "tok", "1"));
        assert!(!task_card_ready(false, "tok", "1"));
        assert!(!task_card_ready(true, "", "1"));
        assert!(!task_card_ready(true, "tok", ""));
        assert!(!task_card_ready(true, "  ", "  "));
    }

    #[test]
    fn appointment_card_buttons_carry_confirm_and_ignore() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let v = task_card_payload("42", "預約待確認", id, "確定", "婉拒");
        let row = &v["reply_markup"]["inline_keyboard"][0];
        assert_eq!(row[0]["text"], "確定");
        assert_eq!(row[1]["text"], "婉拒");
        assert_eq!(decode_callback_data(row[0]["callback_data"].as_str().unwrap()), Some((id.to_string(), true)));
        assert_eq!(decode_callback_data(row[1]["callback_data"].as_str().unwrap()), Some((id.to_string(), false)));
    }

    #[test]
    fn a_command_naming_another_bot_is_not_this_bots() {
        assert_eq!(for_this_bot("/calls@Bageo_Test_Bot today", "bageo_test_bot"), Some("/calls today".to_string()));
        assert_eq!(for_this_bot("/calls@other_bot today", "bageo_test_bot"), None);
        assert_eq!(for_this_bot("/calls@bageo_test_bot", ""), None);
        assert_eq!(for_this_bot("/calls today", ""), Some("/calls today".to_string()));
        assert_eq!(for_this_bot("hello @bageo_test_bot", "bageo_test_bot"), Some("hello @bageo_test_bot".to_string()));
    }

    #[test]
    fn errors_never_show_the_bot_token() {
        let e = "error sending request for url (https://api.telegram.org/bot123:SECRETpart/getUpdates)";
        assert_eq!(redact(e, "123:SECRETpart"), "error sending request for url (https://api.telegram.org/bot<token>/getUpdates)");
    }

    fn update(id: i64, body: Value) -> Value {
        let mut u = body;
        u["update_id"] = json!(id);
        u
    }

    #[tokio::test]
    async fn one_poll_sorts_card_buttons_from_messages_and_keeps_groups_quiet() {
        let updates = json!({"ok": true, "result": [
            // Private chat: every message.
            update(10, json!({"message": {"message_id": 1, "from": {"id": 42, "is_bot": false}, "chat": {"id": 42, "type": "private"}, "text": "/calls today"}})),
            update(11, json!({"message": {"message_id": 2, "from": {"id": 42}, "chat": {"id": 42, "type": "private"}, "text": "昨天有誰打來"}})),
            // A to-do card's button: as before.
            update(12, json!({"callback_query": {"id": "cb1", "from": {"id": 42}, "data": "t:abc-123:c", "message": {"message_id": 9, "chat": {"id": 42}}}})),
            // A chat command's button.
            update(13, json!({"callback_query": {"id": "cb2", "from": {"id": 42}, "data": "k:ABCDEF:y", "message": {"message_id": 10, "chat": {"id": 42}}}})),
            // Group: people talking, a command for another bot, another bot.
            update(14, json!({"message": {"message_id": 3, "from": {"id": 43}, "chat": {"id": -100, "type": "supergroup"}, "text": "see you at 7"}})),
            update(15, json!({"message": {"message_id": 4, "from": {"id": 43}, "chat": {"id": -100, "type": "supergroup"}, "text": "/status@other_bot"}})),
            update(16, json!({"message": {"message_id": 5, "from": {"id": 99, "is_bot": true}, "chat": {"id": -100, "type": "supergroup"}, "text": "/status"}})),
            // Group: a command for this bot, a reply to this bot.
            update(17, json!({"message": {"message_id": 6, "from": {"id": 43}, "chat": {"id": -100, "type": "supergroup"}, "text": "/status@bageo_test_bot"}})),
            update(18, json!({"message": {"message_id": 7, "from": {"id": 43}, "chat": {"id": -100, "type": "supergroup"}, "text": "可以", "reply_to_message": {"message_id": 6, "from": {"id": 777, "is_bot": true}}}})),
        ]});
        let me = json!({"ok": true, "result": {"id": 777, "is_bot": true, "username": "bageo_test_bot"}});
        let (api, seen) = fake_api::serve(vec![("getMe", me.to_string()), ("getUpdates", updates.to_string())]).await;
        let mut poller = TgPoller::with_api(&api);
        poller.set_allowed_chat_id("42");
        let polled = poller.poll_once(&Client::new(), "123:TEST").await.unwrap();

        assert_eq!(polled.actions, vec![TgAction { task_id: "abc-123".into(), confirm: true, chat_id: "42".into(), message_id: 9 }]);
        let got: Vec<(String, Option<String>, Option<String>, bool)> = polled
            .incoming
            .iter()
            .map(|i| (i.chat_id.clone(), i.text.clone(), i.button.clone(), i.reply_to_bot))
            .collect();
        assert_eq!(
            got,
            vec![
                ("42".into(), Some("/calls today".into()), None, false),
                ("42".into(), Some("昨天有誰打來".into()), None, false),
                ("42".into(), None, Some("k:ABCDEF:y".into()), true),
                ("-100".into(), Some("/status".into()), None, false),
                ("-100".into(), Some("可以".into()), None, true),
            ]
        );
        assert_eq!(polled.incoming[3].sender_id, "43");
        // Both buttons were acknowledged; only the card lost its keyboard.
        let m = fake_api::methods(&seen);
        assert_eq!(m.iter().filter(|x| x.eq_ignore_ascii_case("answerCallbackQuery")).count(), 2);
        assert_eq!(fake_api::sent(&seen, "editMessageReplyMarkup").len(), 1);
        assert_eq!(fake_api::sent(&seen, "getUpdates")[0]["allowed_updates"], json!(["message", "callback_query"]));

        // The next poll starts after the last update; who the bot is was asked once.
        let _ = poller.poll_once(&Client::new(), "123:TEST").await.unwrap();
        assert_eq!(fake_api::sent(&seen, "getUpdates")[1]["offset"], 19);
        assert_eq!(fake_api::sent(&seen, "getMe").len(), 1);
    }

    /// A to-do card's button in a group works only for someone on the sender
    /// list; a group with no list refuses everyone. A private chat needs none.
    #[tokio::test]
    async fn a_card_button_in_a_group_needs_a_sender_list() {
        let press = |from: i64, chat: i64| {
            json!({"ok": true, "result": [update(30, json!({"callback_query": {"id": "cb", "from": {"id": from}, "data": "t:abc-123:c", "message": {"message_id": 9, "chat": {"id": chat}}}}))]})
        };
        let me = json!({"ok": true, "result": {"id": 777, "is_bot": true, "username": "bageo_test_bot"}});
        let run = |chat: i64, from: i64, list: Vec<String>| {
            let (updates, me) = (press(from, chat), me.clone());
            async move {
                let (api, seen) = fake_api::serve(vec![("getMe", me.to_string()), ("getUpdates", updates.to_string())]).await;
                let mut poller = TgPoller::with_api(&api);
                poller.set_allowed_chat_id(&chat.to_string());
                poller.set_card_senders(vec![list]);
                let polled = poller.poll_once(&Client::new(), "123:TEST").await.unwrap();
                (polled.actions.len(), fake_api::sent(&seen, "editMessageReplyMarkup").len())
            }
        };
        // A group, no list: refused, the card keeps its buttons.
        assert_eq!(run(-100, 43, vec![]).await, (0, 0));
        // A group, the sender is on the list: done.
        assert_eq!(run(-100, 43, vec!["43".into()]).await, (1, 1));
        // A group, another sender: refused.
        assert_eq!(run(-100, 44, vec!["43".into()]).await, (0, 0));
        // A private chat, no list: as before.
        assert_eq!(run(42, 42, vec![]).await, (1, 1));
    }

    #[tokio::test]
    async fn an_answer_goes_out_with_its_buttons_and_a_pressed_one_is_edited() {
        let (api, seen) = fake_api::serve(vec![]).await;
        let http = Client::new();
        let out = Outbound {
            text: "讓 AI 打給 0912345678".into(),
            buttons: vec![
                Button { label: "執行".into(), data: "k:ABCDEF:y".into() },
                Button { label: "取消".into(), data: "k:ABCDEF:n".into() },
            ],
            edit: None,
        };
        send_outbound(&http, &api, "123:TEST", "42", &out).await.unwrap();
        let sent = fake_api::sent(&seen, "sendMessage");
        assert_eq!(sent[0]["chat_id"], "42");
        assert_eq!(sent[0]["reply_markup"]["inline_keyboard"][0][1]["callback_data"], "k:ABCDEF:n");

        let done = Outbound { text: "已取消".into(), buttons: vec![], edit: Some("10".into()) };
        send_outbound(&http, &api, "123:TEST", "42", &done).await.unwrap();
        let edits = fake_api::sent(&seen, "editMessageText");
        assert_eq!((edits[0]["message_id"].clone(), edits[0]["reply_markup"].clone()), (json!(10), json!({"inline_keyboard": []})));
        assert_eq!(fake_api::sent(&seen, "sendMessage").len(), 1);

        set_my_commands(&http, &api, "123:TEST", &[("calls", "Recent calls")], &["42".to_string()]).await;
        set_my_commands(&http, &api, "123:TEST", &[], &["42".to_string()]).await;
        assert_eq!(fake_api::sent(&seen, "setMyCommands")[0]["commands"][0]["command"], "calls");
        // Only for the chat that takes commands, never for every chat.
        assert_eq!(fake_api::sent(&seen, "setMyCommands")[0]["scope"], json!({"type": "chat", "chat_id": 42}));
        assert!(fake_api::methods(&seen).last().is_some_and(|m| m.eq_ignore_ascii_case("deleteMyCommands")));
    }

    #[test]
    fn clip_message_counts_utf16_code_units() {
        let exact = "😀".repeat(2048); // 4096 UTF-16 code units
        assert_eq!(clip_message(&exact), exact);

        let over = "😀".repeat(2049); // 4098 UTF-16 code units
        let clipped = clip_message(&over);
        let units: usize = clipped.chars().map(char::len_utf16).sum();
        assert!(units <= MAX_MESSAGE_TEXT, "utf16={units}");
        assert!(clipped.ends_with('…'));
    }

    #[tokio::test]
    async fn send_task_card_retries_transient_500_and_clips_long_body() {
        use axum::{
            extract::State, http::StatusCode, response::IntoResponse, routing::post, Json, Router,
        };
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        async fn send_message(
            State(hits): State<Arc<AtomicUsize>>,
            Json(body): Json<serde_json::Value>,
        ) -> axum::response::Response {
            assert!(body["text"].as_str().is_some_and(|s| {
                !s.is_empty() && s.chars().map(char::len_utf16).sum::<usize>() <= MAX_MESSAGE_TEXT
            }));
            let n = hits.fetch_add(1, Ordering::Relaxed);
            if n == 0 {
                (StatusCode::INTERNAL_SERVER_ERROR, "busy").into_response()
            } else {
                Json(serde_json::json!({ "ok": true, "result": { "message_id": 99 } }))
                    .into_response()
            }
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/bot123:TEST/sendMessage", post(send_message))
            .with_state(hits.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mid = send_task_card_labeled_at(
            &Client::new(),
            &format!("http://{addr}"),
            "123:TEST",
            "42",
            "Title",
            "task-1",
            &"😀".repeat(2050),
            "Confirm",
            "Ignore",
        )
        .await
        .unwrap();
        assert_eq!(mid, 99);
        assert_eq!(hits.load(Ordering::Relaxed), 2);
        server.abort();
    }
}
