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
use crate::gcal::{gcal_missing_cred_msg, insert_event, parse_datetime_hint};
use crate::gmail::{gmail_missing_cred_msg, send_mail};
use crate::locale::{self, Lang, Phrases};
use crate::openclaw::{configured as openclaw_configured, handoff_task};
use crate::settings_store::RuntimeSettings;
use crate::tasks::{ReplyMethod, TaskIntent, TaskPatch, TaskRecord, TaskStatus, TaskStore};

/// What the owner reads (task errors, the e-mail, the follow-up message), in
/// the agent's language.
fn texts() -> &'static Phrases {
    locale::phrases(locale::current())
}

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
            let msg = locale::fill(texts().f_openclaw_failed, &format!("{e:#}"));
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
                errors.push(texts().f_callback_no_number.to_string());
                phone_callback_pending = false;
            } else if !callback_whitelist_allows(&number, &rt.callback_whitelist) {
                warn!(
                    task_id,
                    whitelist_len = rt.callback_whitelist.len(),
                    "fulfill: callback number not on whitelist"
                );
                errors.push(texts().f_callback_whitelist.to_string());
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
                        errors.push(format!("{}{}{e:#}", texts().f_callback_send_failed, locale::kv_sep()));
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
                        fulfill_error: Some(Some(texts().f_callback_waiting.to_string())),
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
        let joiner = match locale::current() {
            Lang::ZhTw | Lang::ZhCn | Lang::Ja => "；",
            _ => "; ",
        };
        let msg = errors.join(joiner);
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
        anyhow::bail!("{}", texts().f_email_no_address);
    };

    if !rt.gmail_configured() {
        anyhow::bail!("{}", gmail_missing_cred_msg());
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
        anyhow::bail!("{}", gcal_missing_cred_msg());
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
            .with_context(|| locale::fill(texts().f_bad_time, hint))?;
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
    .with_context(|| texts().f_gcal_failed.to_string())?;

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
        format!("Bageo — {}", texts().e_subject)
    } else {
        format!("Re: {summary}")
    }
}

fn email_body(task: &TaskRecord) -> String {
    let p = texts();
    // "Summary:" heading over its own paragraph ("摘要：", "Résumé :").
    let heading = |label: &str| format!("{label}{}\n", locale::kv_sep().trim_end());
    let mut body = String::new();
    let summary = task.summary.trim();
    if !summary.is_empty() {
        body.push_str(&heading(p.e_summary));
        body.push_str(summary);
        body.push_str("\n\n");
    }
    let transcript = task.transcript.trim();
    if !transcript.is_empty() {
        body.push_str(&heading(p.tc_transcript));
        body.push_str(transcript);
        body.push('\n');
    }
    if body.is_empty() {
        body.push_str(p.e_empty);
        body.push('\n');
    }
    body
}

fn calendar_summary(task: &TaskRecord) -> String {
    let summary = task.summary.trim();
    if !summary.is_empty() {
        summary.to_string()
    } else {
        locale::fill(texts().e_appointment, display_caller(&task.caller_id))
    }
}

fn calendar_description(task: &TaskRecord) -> String {
    let p = texts();
    let sep = locale::kv_sep();
    let mut parts = Vec::new();
    parts.push(format!("{}{sep}{}", p.s_caller, display_caller(&task.caller_id)));
    parts.push(format!("{}{sep}{}", p.n_phone, crate::notify::device_label(&task.serial)));
    if !task.summary.trim().is_empty() {
        parts.push(format!("{}{sep}{}", p.e_summary, task.summary.trim()));
    }
    if !task.transcript.trim().is_empty() {
        parts.push(format!("{}{sep}{}", p.tc_transcript, task.transcript.trim()));
    }
    parts.join("\n")
}

fn display_caller(caller_id: &str) -> &str {
    let t = caller_id.trim();
    if t.is_empty() {
        texts().n_hidden
    } else {
        t
    }
}

/// Short status line for Telegram follow-up after confirm/ignore.
pub fn tg_result_text(task: &TaskRecord, confirmed: bool) -> String {
    let p = texts();
    let sep = locale::kv_sep();
    if !confirmed {
        return format!("{}\n{}", p.tc_ignored_task, task.summary.trim());
    }
    let status = match task.status {
        TaskStatus::Fulfilled => p.tc_done,
        TaskStatus::Confirmed => p.tc_in_progress,
        TaskStatus::Failed => p.tc_failed,
        TaskStatus::Pending => p.tc_pending,
        TaskStatus::Ignored => p.tc_ignore_ack,
    };
    let mut lines = vec![format!("{}{sep}{status}", p.tc_task), task.summary.trim().to_string()];
    if let Some(err) = task.fulfill_error.as_deref().filter(|s| !s.is_empty()) {
        lines.push(format!("{}{sep}{err}", p.n_reason));
    }
    if let Some(r) = task.openclaw_ref.as_deref().filter(|s| !s.is_empty()) {
        lines.push(format!("OpenClaw{sep}{r}"));
    }
    if let Some(c) = task.calendar_event_id.as_deref().filter(|s| !s.is_empty()) {
        lines.push(format!("{}{sep}{c}", p.n_calendar));
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
        assert!(gmail_missing_cred_msg().contains("設定"));
        assert!(gcal_missing_cred_msg().contains("設定"));
        assert!(texts().f_callback_waiting.contains("控制"));
        // Every language says where to fix it.
        for l in locale::ALL {
            let p = locale::phrases(l);
            assert!(p.f_gmail_missing.contains("Gmail") && p.f_gcal_missing.contains("Google"), "{l:?}");
        }
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
            fulfill_error: Some(gmail_missing_cred_msg().to_string()),
            openclaw_ref: None,
            events: vec![],
            appointment: None,
        };
        let text = tg_result_text(&task, true);
        assert!(text.contains("履行失敗"));
        assert!(text.contains("測試摘要"));
        assert!(text.contains("Gmail"));
        assert!(text.contains("原因：Gmail"), "{text}");
        let ignored = tg_result_text(&task, false);
        assert!(ignored.starts_with("已忽略任務"));
    }
}
