//! Telegram inbound polling for AI task confirm/ignore buttons.
//!
//! Outbound task cards use [`send_task_card`] (Bot API + inline keyboard; Apprise
//! cannot carry buttons). Inbound clicks: [`TgPoller::poll_once`] long-polls
//! `getUpdates`. Wired from `main.rs` when bot token + chat id are set.
//!
//! **Not** gated by spam-blocklist (deny removed). Outbound cards share the
//! `CALL_NOTIFY` / `call_notify` master switch via [`task_card_ready`].
//! Call/VM plain alerts stay in [`crate::notify`] (Apprise → webhook → Bot API).

use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{debug, info, warn};

/// Telegram inline-keyboard action: user confirmed or ignored an AI task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TgAction {
    pub task_id: String,
    pub confirm: bool,
    pub chat_id: String,
    pub message_id: i64,
}

/// Long-poll state for Telegram `getUpdates`.
pub struct TgPoller {
    /// Next `offset` for getUpdates (last processed `update_id + 1`).
    offset: i64,
    /// When set, callbacks from other chats are dropped (security).
    allowed_chat_id: Option<String>,
}

/// Telegram `callback_data` limit (bytes).
const MAX_CALLBACK_DATA: usize = 64;

/// Long-poll timeout passed to getUpdates (seconds).
const GET_UPDATES_TIMEOUT_SECS: u32 = 10;

impl TgPoller {
    pub fn new() -> Self {
        Self {
            offset: 0,
            allowed_chat_id: None,
        }
    }

    /// Restrict inbound callbacks to this chat (`RuntimeSettings.telegram_chat_id`).
    pub fn set_allowed_chat_id(&mut self, chat_id: &str) {
        let trimmed = chat_id.trim();
        self.allowed_chat_id = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }

    /// One long-poll cycle: fetch updates, answer callbacks, strip keyboards.
    pub async fn poll_once(&mut self, http: &Client, bot_token: &str) -> Result<Vec<TgAction>> {
        let token = bot_token.trim();
        if token.is_empty() {
            return Ok(Vec::new());
        }

        let url = bot_method_url(token, "getUpdates");
        let resp = http
            .post(&url)
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

        let mut actions = Vec::new();

        for update in updates {
            let update_id = update.get("update_id").and_then(Value::as_i64).unwrap_or(0);
            if update_id >= self.offset {
                self.offset = update_id + 1;
            }

            if let Some(action) = self
                .handle_callback_query(http, token, update.get("callback_query"))
                .await?
            {
                actions.push(action);
            }
        }

        Ok(actions)
    }

    async fn handle_callback_query(
        &self,
        http: &Client,
        bot_token: &str,
        query: Option<&Value>,
    ) -> Result<Option<TgAction>> {
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
                answer_callback_query(http, bot_token, callback_id, None).await;
                return Ok(None);
            }
        };

        let (task_id, confirm) = match decode_callback_data(data) {
            Some(parsed) => parsed,
            None => {
                debug!("telegram callback ignored unknown data={data:?}");
                answer_callback_query(http, bot_token, callback_id, None).await;
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
                answer_callback_query(http, bot_token, callback_id, None).await;
                return Ok(None);
            }
        };

        if let Some(expected) = self.allowed_chat_id.as_deref() {
            if !chat_id_matches(expected, chat_id_val.unwrap()) {
                warn!(
                    "telegram callback rejected chat_id={chat_id} expected={expected} task_id={task_id}"
                );
                answer_callback_query(http, bot_token, callback_id, None).await;
                return Ok(None);
            }
        }

        let p = crate::locale::phrases(crate::locale::current());
        let ack = if confirm { p.tc_confirm_ack } else { p.tc_ignore_ack };
        answer_callback_query(http, bot_token, callback_id, Some(ack)).await;

        if let Some(mid) = message_id {
            remove_inline_keyboard(http, bot_token, &chat_id, mid).await;
        }

        info!(
            "telegram task action task_id={task_id} confirm={confirm} chat_id={chat_id} message_id={message_id:?}"
        );

        Ok(Some(TgAction {
            task_id,
            confirm,
            chat_id,
            message_id: message_id.unwrap_or(0),
        }))
    }
}

impl Default for TgPoller {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether to send a new task card: `call_notify` on + bot token + chat id.
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
    let p = crate::locale::phrases(crate::locale::current());
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
}

/// [`send_task_card`] with its own button texts (appointments: "✅ 確定" / "❌ 婉拒").
/// The buttons carry the same confirm / ignore data.
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
) -> Result<()> {
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
    let text = crate::zh_tw::to_tw(&text);

    let url = bot_method_url(token, "sendMessage");
    let resp = http
        .post(&url)
        .json(&task_card_payload(chat, &text, task_id, confirm_label, ignore_label))
        .send()
        .await
        .context("telegram sendMessage task card")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err_body = resp.text().await.unwrap_or_default();
        anyhow::bail!("telegram sendMessage HTTP {status}: {err_body}");
    }

    let body_json: Value = resp
        .json()
        .await
        .context("telegram sendMessage response JSON")?;

    if body_json.get("ok").and_then(Value::as_bool) != Some(true) {
        let desc = body_json
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        anyhow::bail!("telegram sendMessage not ok: {desc}");
    }

    info!("telegram task card sent task_id={task_id} chat_id={chat}");
    Ok(())
}

/// The `sendMessage` body of a task card: the text and the two buttons.
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

/// Telegram Bot API hard limit for `sendMessage.text`.
const MAX_MESSAGE_TEXT: usize = 4096;

/// Best-effort plain follow-up after confirm/ignore (fulfill outcome).
pub async fn send_plain_message(
    http: &Client,
    bot_token: &str,
    chat_id: &str,
    text: &str,
) -> Result<()> {
    let token = bot_token.trim();
    let chat = chat_id.trim();
    let mut body = crate::zh_tw::to_tw(text.trim());
    if token.is_empty() || chat.is_empty() {
        anyhow::bail!("telegram bot token and chat id are required");
    }
    if body.is_empty() {
        return Ok(());
    }
    if body.len() > MAX_MESSAGE_TEXT {
        let keep = MAX_MESSAGE_TEXT.saturating_sub(1);
        body = body.chars().take(keep).collect();
        body.push('…');
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

fn bot_method_url(bot_token: &str, method: &str) -> String {
    format!("https://api.telegram.org/bot{bot_token}/{method}")
}

/// Encode callback payload. Prefers `t:{uuid}:c|i` (fits full UUID under 64 bytes).
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
    bot_token: &str,
    callback_query_id: &str,
    text: Option<&str>,
) {
    let mut payload = json!({ "callback_query_id": callback_query_id });
    if let Some(t) = text {
        payload["text"] = json!(t);
    }

    let url = bot_method_url(bot_token, "answerCallbackQuery");
    match http.post(&url).json(&payload).send().await {
        Ok(resp) if resp.status().is_success() => {}
        Ok(resp) => warn!(
            "telegram answerCallbackQuery HTTP {}",
            resp.status().as_u16()
        ),
        Err(e) => warn!("telegram answerCallbackQuery failed: {e}"),
    }
}

/// Best-effort: remove inline keyboard after the user clicks a button.
async fn remove_inline_keyboard(http: &Client, bot_token: &str, chat_id: &str, message_id: i64) {
    let url = bot_method_url(bot_token, "editMessageReplyMarkup");
    let payload = json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "reply_markup": { "inline_keyboard": [] },
    });

    match http.post(&url).json(&payload).send().await {
        Ok(resp) if resp.status().is_success() => {}
        Ok(resp) => debug!(
            "telegram editMessageReplyMarkup HTTP {} chat_id={chat_id} message_id={message_id}",
            resp.status().as_u16()
        ),
        Err(e) => debug!(
            "telegram editMessageReplyMarkup failed chat_id={chat_id} message_id={message_id}: {e}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let v = task_card_payload("42", "📅 預約待確認", id, "✅ 確定", "❌ 婉拒");
        let row = &v["reply_markup"]["inline_keyboard"][0];
        assert_eq!(row[0]["text"], "✅ 確定");
        assert_eq!(row[1]["text"], "❌ 婉拒");
        assert_eq!(decode_callback_data(row[0]["callback_data"].as_str().unwrap()), Some((id.to_string(), true)));
        assert_eq!(decode_callback_data(row[1]["callback_data"].as_str().unwrap()), Some((id.to_string(), false)));
    }
}
