//! Fulfillment orchestration: user confirm → OpenClaw handoff or built-in Gmail / GCal / callback queue.
//!
//! Web (`calls_http`) and Telegram (`main` poller) both call [`on_confirm`] with shared
//! `http`, `RuntimeSettings`, `TaskStore`, and [`ControlRegistry`].
//!
//! Works without OpenRouter — only Gmail / GCal / OpenClaw / phone-control credentials matter
//! for the chosen reply path.

use anyhow::{Context, Result};
use chrono::Utc;
use reqwest::Client;
use serde_json::to_value;
use tracing::{info, warn};

use crate::callback::{callback_number, callback_whitelist_allows, ControlRegistry};
use crate::gcal::{insert_event, parse_datetime_hint, GCAL_MISSING_CRED_MSG};
use crate::gmail::{send_mail, GMAIL_MISSING_CRED_MSG};
use crate::openclaw::{configured as openclaw_configured, handoff_task};
use crate::settings_store::RuntimeSettings;
use crate::tasks::{ReplyMethod, TaskIntent, TaskPatch, TaskRecord, TaskStatus, TaskStore};

const CALLBACK_WAITING_MSG: &str =
    "回撥已排程，但手機控制通道離線 — 請確保 Pixel 已連上 bridge（CONTROL_IDLE）後再按一次確認";

const CALLBACK_WHITELIST_MSG: &str =
    "回撥號碼不在白名單 — 請至「設定」加入允許回撥的號碼（或前綴*）後再確認";

const GMAIL_MISSING_MSG: &str = GMAIL_MISSING_CRED_MSG;

const GCAL_MISSING_MSG: &str = GCAL_MISSING_CRED_MSG;

const EMAIL_CONTACT_MISSING_MSG: &str =
    "此任務要寄信，但沒有回覆信箱（reply_contact）— 請從通話逐字稿補齊聯絡方式，或改忽略";

/// User confirmed the task (Telegram button or web UI). Runs fulfillment once.
pub async fn on_confirm(
    http: &Client,
    rt: &RuntimeSettings,
    store: &TaskStore,
    task_id: &str,
    control: Option<&ControlRegistry>,
) -> Result<TaskRecord> {
    let task = store
        .get(task_id)
        .await
        .with_context(|| format!("task not found: {task_id}"))?;

    if !is_actionable(task.status) {
        info!(
            task_id,
            status = ?task.status,
            "fulfill on_confirm: skip non-actionable status"
        );
        return Ok(task);
    }

    // Retry / re-confirm: clear stale error so the UI shows the new outcome.
    if task.fulfill_error.is_some() {
        store
            .patch(
                task_id,
                TaskPatch {
                    fulfill_error: Some(None),
                    ..Default::default()
                },
            )
            .await?;
    }
    store.push_event(task_id, "confirmed").await?;

    if openclaw_configured(&rt.openclaw_url) {
        return fulfill_via_openclaw(http, rt, store, task_id, &task).await;
    }

    fulfill_builtin(http, rt, store, task_id, &task, control).await
}

/// User ignored the task — terminal state, no side effects.
pub async fn on_ignore(store: &TaskStore, task_id: &str) -> Result<TaskRecord> {
    let task = store
        .get(task_id)
        .await
        .with_context(|| format!("task not found: {task_id}"))?;

    if task.status == TaskStatus::Ignored {
        return Ok(task);
    }
    if matches!(task.status, TaskStatus::Fulfilled | TaskStatus::Failed) {
        warn!(
            task_id,
            status = ?task.status,
            "fulfill on_ignore: already terminal, skipping"
        );
        return Ok(task);
    }

    store.push_event(task_id, "ignored").await?;
    store.update_status(task_id, TaskStatus::Ignored).await
}

/// Pending, confirmed (e.g. waiting for phone control), or failed (retry after fixing creds).
fn is_actionable(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Pending | TaskStatus::Confirmed | TaskStatus::Failed
    )
}

async fn fulfill_via_openclaw(
    http: &Client,
    rt: &RuntimeSettings,
    store: &TaskStore,
    task_id: &str,
    task: &TaskRecord,
) -> Result<TaskRecord> {
    info!(task_id, "fulfill: openclaw handoff");
    let task_json = to_value(task).context("serialize task for openclaw")?;

    match handoff_task(http, &rt.openclaw_url, &rt.openclaw_token, &task_json).await {
        Ok(handoff) => {
            store
                .patch(
                    task_id,
                    TaskPatch {
                        openclaw_ref: Some(handoff.external_id.clone()),
                        fulfill_error: Some(None),
                        ..Default::default()
                    },
                )
                .await?;
            store.push_event(task_id, "openclaw_handoff").await?;
            store.update_status(task_id, TaskStatus::Fulfilled).await
        }
        Err(e) => {
            let msg = format!("OpenClaw 轉派失敗：{e:#}（請檢查設定中的 OpenClaw URL / Token）");
            warn!(task_id, error = %msg, "fulfill: openclaw handoff failed");
            store
                .patch(
                    task_id,
                    TaskPatch {
                        fulfill_error: Some(Some(msg)),
                        ..Default::default()
                    },
                )
                .await?;
            store.update_status(task_id, TaskStatus::Failed).await
        }
    }
}

async fn fulfill_builtin(
    http: &Client,
    rt: &RuntimeSettings,
    store: &TaskStore,
    task_id: &str,
    task: &TaskRecord,
    control: Option<&ControlRegistry>,
) -> Result<TaskRecord> {
    info!(task_id, intent = ?task.intent, reply = ?task.reply_method, "fulfill: built-in path");

    let mut errors: Vec<String> = Vec::new();
    let mut phone_callback_pending = false;

    // Email reply
    if task.reply_method == ReplyMethod::Email {
        match try_send_email(http, rt, task).await {
            Ok(()) => {
                store.push_event(task_id, "email_sent").await?;
            }
            Err(e) => errors.push(format!("{e:#}")),
        }
    }

    // Calendar event
    if should_schedule_calendar(task) {
        match try_insert_calendar(http, rt, store, task_id, task).await {
            Ok(()) => {}
            Err(e) => errors.push(format!("{e:#}")),
        }
    }

    // Phone callback — dial only after confirm + whitelist + CONTROL_IDLE.
    if task.reply_method == ReplyMethod::Phone {
        store.push_event(task_id, "callback_requested").await?;
        phone_callback_pending = true;

        let mut waiting_for_control = control.is_none();
        if let Some(ctrl) = control {
            let number = callback_number(task.reply_contact.as_deref(), &task.caller_id);
            if number.is_empty() {
                warn!(task_id, "fulfill: callback number missing");
                errors.push("回撥號碼缺失 — 任務沒有 reply_contact 且來電號碼為空".to_string());
                phone_callback_pending = false;
            } else if !callback_whitelist_allows(&number, &rt.callback_whitelist) {
                warn!(
                    task_id,
                    whitelist_len = rt.callback_whitelist.len(),
                    "fulfill: callback number not on whitelist"
                );
                errors.push(CALLBACK_WHITELIST_MSG.to_string());
                phone_callback_pending = false;
            } else {
                match ctrl
                    .try_dispatch_phone_callback(
                        task_id,
                        &number,
                        &task.serial,
                        &rt.callback_whitelist,
                    )
                    .await
                {
                    Ok(true) => {
                        waiting_for_control = false;
                        store.push_event(task_id, "callback_dialing").await?;
                        // Clear prior waiting hint once dial command is on the wire.
                        store
                            .patch(
                                task_id,
                                TaskPatch {
                                    fulfill_error: Some(None),
                                    ..Default::default()
                                },
                            )
                            .await?;
                        info!(task_id, serial = %task.serial, "fulfill: place_call sent");
                    }
                    Ok(false) => {
                        waiting_for_control = true;
                        info!(
                            task_id,
                            serial = %task.serial,
                            "fulfill: no CONTROL_IDLE client connected"
                        );
                    }
                    Err(e) => {
                        warn!(task_id, error = %e, "fulfill: callback dispatch failed");
                        errors.push(format!("回撥發送失敗：{e:#}"));
                        phone_callback_pending = false;
                    }
                }
            }
        }

        if waiting_for_control && phone_callback_pending {
            store
                .push_event(task_id, "callback_waiting_for_phone_control")
                .await?;
            store
                .patch(
                    task_id,
                    TaskPatch {
                        fulfill_error: Some(Some(CALLBACK_WAITING_MSG.to_string())),
                        ..Default::default()
                    },
                )
                .await?;
        }
    }

    if phone_callback_pending && errors.is_empty() {
        info!(task_id, "fulfill: callback requested, leaving confirmed");
        return store.update_status(task_id, TaskStatus::Confirmed).await;
    }

    if errors.is_empty() {
        store
            .patch(
                task_id,
                TaskPatch {
                    fulfill_error: Some(None),
                    ..Default::default()
                },
            )
            .await?;
        store.push_event(task_id, "fulfilled").await?;
        store.update_status(task_id, TaskStatus::Fulfilled).await
    } else {
        let msg = errors.join("；");
        warn!(task_id, error = %msg, "fulfill: built-in failed");
        store
            .patch(
                task_id,
                TaskPatch {
                    fulfill_error: Some(Some(msg)),
                    ..Default::default()
                },
            )
            .await?;
        store.update_status(task_id, TaskStatus::Failed).await
    }
}

async fn try_send_email(http: &Client, rt: &RuntimeSettings, task: &TaskRecord) -> Result<()> {
    let to = task
        .reply_contact
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(to) = to else {
        anyhow::bail!("{EMAIL_CONTACT_MISSING_MSG}");
    };

    if !rt.gmail_configured() {
        anyhow::bail!("{GMAIL_MISSING_MSG}");
    }

    let subject = email_subject(task);
    let body = email_body(task);

    send_mail(
        http,
        &rt.gmail_user,
        &rt.gmail_app_password,
        &rt.gmail_from_name,
        to,
        &subject,
        &body,
    )
    .await
}

async fn try_insert_calendar(
    http: &Client,
    rt: &RuntimeSettings,
    store: &TaskStore,
    task_id: &str,
    task: &TaskRecord,
) -> Result<()> {
    if !rt.gcal_configured() {
        anyhow::bail!("{GCAL_MISSING_MSG}");
    }

    let summary = calendar_summary(task);
    let description = calendar_description(task);

    let (start_rfc3339, end_rfc3339) = if let Some(hint) = task
        .datetime_hint
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let now = Utc::now();
        let (start, end) = parse_datetime_hint(hint, now)
            .with_context(|| format!("無法解析時間提示「{hint}」"))?;
        (start.to_rfc3339(), end.to_rfc3339())
    } else {
        (String::new(), String::new())
    };

    let event_id = insert_event(
        http,
        &rt.gcal_client_id,
        &rt.gcal_client_secret,
        &rt.gcal_refresh_token,
        &rt.gcal_calendar_id,
        &summary,
        &description,
        &start_rfc3339,
        &end_rfc3339,
    )
    .await
    .with_context(|| "Google 日曆建立行程失敗".to_string())?;

    store
        .patch(
            task_id,
            TaskPatch {
                calendar_event_id: Some(event_id),
                ..Default::default()
            },
        )
        .await?;
    store.push_event(task_id, "gcal_event_created").await?;
    Ok(())
}

fn should_schedule_calendar(task: &TaskRecord) -> bool {
    matches!(task.intent, TaskIntent::Appointment)
        || task
            .datetime_hint
            .as_ref()
            .is_some_and(|h| !h.trim().is_empty())
}

fn email_subject(task: &TaskRecord) -> String {
    let summary = task.summary.trim();
    if summary.is_empty() {
        format!("Pixel {} — 語音留言回覆", task.serial.trim())
    } else {
        format!("Re: {summary}")
    }
}

fn email_body(task: &TaskRecord) -> String {
    let mut body = String::new();
    let summary = task.summary.trim();
    if !summary.is_empty() {
        body.push_str("摘要：\n");
        body.push_str(summary);
        body.push_str("\n\n");
    }
    let transcript = task.transcript.trim();
    if !transcript.is_empty() {
        body.push_str("逐字稿：\n");
        body.push_str(transcript);
        body.push('\n');
    }
    if body.is_empty() {
        body.push_str("(無摘要或逐字稿)\n");
    }
    body
}

fn calendar_summary(task: &TaskRecord) -> String {
    let summary = task.summary.trim();
    if !summary.is_empty() {
        summary.to_string()
    } else {
        format!("預約 — {}", display_caller(&task.caller_id))
    }
}

fn calendar_description(task: &TaskRecord) -> String {
    let mut parts = Vec::new();
    parts.push(format!("來電：{}", display_caller(&task.caller_id)));
    parts.push(format!("Pixel：{}", task.serial.trim()));
    if !task.summary.trim().is_empty() {
        parts.push(format!("摘要：{}", task.summary.trim()));
    }
    if !task.transcript.trim().is_empty() {
        parts.push(format!("逐字稿：{}", task.transcript.trim()));
    }
    parts.join("\n")
}

fn display_caller(caller_id: &str) -> &str {
    let t = caller_id.trim();
    if t.is_empty() {
        "unknown"
    } else {
        t
    }
}

/// Short status line for Telegram follow-up after confirm/ignore.
pub fn tg_result_text(task: &TaskRecord, confirmed: bool) -> String {
    if !confirmed {
        return format!("已忽略任務\n{}", task.summary.trim());
    }
    let status = match task.status {
        TaskStatus::Fulfilled => "已完成",
        TaskStatus::Confirmed => "已確認（進行中）",
        TaskStatus::Failed => "履行失敗",
        TaskStatus::Pending => "待確認",
        TaskStatus::Ignored => "已忽略",
    };
    let mut lines = vec![format!("任務：{status}"), task.summary.trim().to_string()];
    if let Some(err) = task.fulfill_error.as_deref().filter(|s| !s.is_empty()) {
        lines.push(format!("原因：{err}"));
    }
    if let Some(r) = task.openclaw_ref.as_deref().filter(|s| !s.is_empty()) {
        lines.push(format!("OpenClaw：{r}"));
    }
    if let Some(c) = task.calendar_event_id.as_deref().filter(|s| !s.is_empty()) {
        lines.push(format!("日曆事件：{c}"));
    }
    lines.retain(|l| !l.is_empty());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actionable_includes_failed_for_retry() {
        assert!(is_actionable(TaskStatus::Pending));
        assert!(is_actionable(TaskStatus::Confirmed));
        assert!(is_actionable(TaskStatus::Failed));
        assert!(!is_actionable(TaskStatus::Ignored));
        assert!(!is_actionable(TaskStatus::Fulfilled));
    }

    #[test]
    fn missing_cred_messages_are_clear() {
        assert!(GMAIL_MISSING_MSG.contains("設定"));
        assert!(GCAL_MISSING_MSG.contains("設定"));
        assert!(
            CALLBACK_WAITING_MSG.contains("CONTROL_IDLE") || CALLBACK_WAITING_MSG.contains("控制")
        );
    }

    #[test]
    fn tg_result_text_includes_error_reason() {
        let task = TaskRecord {
            id: "t1".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            call_id: "c1".into(),
            serial: "pixel".into(),
            caller_id: "+886".into(),
            status: TaskStatus::Failed,
            intent: TaskIntent::EmailReply,
            summary: "測試摘要".into(),
            transcript: String::new(),
            datetime_hint: None,
            reply_method: ReplyMethod::Email,
            reply_contact: None,
            contact_verified: false,
            voicemail_path: None,
            calendar_event_id: None,
            fulfill_error: Some(GMAIL_MISSING_MSG.to_string()),
            openclaw_ref: None,
            events: vec![],
        };
        let text = tg_result_text(&task, true);
        assert!(text.contains("履行失敗"));
        assert!(text.contains("測試摘要"));
        assert!(text.contains("Gmail"));
        let ignored = tg_result_text(&task, false);
        assert!(ignored.starts_with("已忽略任務"));
    }
}
