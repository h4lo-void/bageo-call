// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Assistant task store — voicemail → extract → fulfill pipeline.
//!
//! Persists under `{CALLS_DATA_DIR}/tasks/` as `{id}.json` plus optional
//! `index.json` (atomic `.tmp` + rename, same pattern as `calls.rs`).
//!
//! Created once in `main.rs` and shared with the web console (`calls_http`).

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tokio::sync::Mutex;
use tracing::{info, warn};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Pending,
    Confirmed,
    Ignored,
    Fulfilled,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReplyMethod {
    #[default]
    None,
    Phone,
    Email,
    Other,
}

/// Known intents serialize as snake_case strings; any other string round-trips as `Other`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskIntent {
    Message,
    Appointment,
    Callback,
    EmailReply,
    Other(String),
}

impl TaskIntent {
    fn as_str(&self) -> &str {
        match self {
            TaskIntent::Message => "message",
            TaskIntent::Appointment => "appointment",
            TaskIntent::Callback => "callback",
            TaskIntent::EmailReply => "email_reply",
            TaskIntent::Other(s) => s.as_str(),
        }
    }
}

impl Serialize for TaskIntent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for TaskIntent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "message" => TaskIntent::Message,
            "appointment" => TaskIntent::Appointment,
            "callback" => TaskIntent::Callback,
            "email_reply" => TaskIntent::EmailReply,
            other => TaskIntent::Other(other.to_string()),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskRecord {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub call_id: String,
    pub serial: String,
    pub caller_id: String,
    pub status: TaskStatus,
    pub intent: TaskIntent,
    pub summary: String,
    pub transcript: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub datetime_hint: Option<String>,
    pub reply_method: ReplyMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_contact: Option<String>,
    #[serde(default)]
    pub contact_verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voicemail_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fulfill_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openclaw_ref: Option<String>,
    #[serde(default)]
    pub events: Vec<String>,
    /// An appointment the AI took on a call (intent `appointment`): its time,
    /// what it is and how far it got. Decided by [`crate::appointments::decide`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appointment: Option<crate::appointments::Appointment>,
}

/// Partial update for extract / fulfill stages.
#[derive(Debug, Clone, Default)]
pub struct TaskPatch {
    pub summary: Option<String>,
    pub transcript: Option<String>,
    pub intent: Option<TaskIntent>,
    pub datetime_hint: Option<Option<String>>,
    pub calendar_event_id: Option<String>,
    pub openclaw_ref: Option<String>,
    /// `Some(None)` clears a previous error; `Some(Some(msg))` sets it.
    pub fulfill_error: Option<Option<String>>,
    pub reply_method: Option<ReplyMethod>,
    pub reply_contact: Option<Option<String>>,
    pub contact_verified: Option<bool>,
    pub appointment: Option<crate::appointments::Appointment>,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TaskIndex {
    tasks: Vec<TaskRecord>,
}

pub struct TaskStore {
    root: PathBuf,
    index: Mutex<TaskIndex>,
}

impl TaskStore {
    pub fn new(root: PathBuf) -> Arc<Self> {
        if let Err(e) = fs::create_dir_all(&root) {
            warn!("tasks dir create {}: {e}", root.display());
        }
        let index = load_or_rebuild_index(&root).unwrap_or_default();
        Arc::new(Self {
            root,
            index: Mutex::new(index),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Add to-do items of a backup this store does not have (by id); the
    /// ones it has stay. How many were added.
    pub async fn merge(&self, tasks: Vec<TaskRecord>) -> Result<usize> {
        let mut idx = self.index.lock().await;
        let mut have: std::collections::HashSet<String> = idx.tasks.iter().map(|t| t.id.clone()).collect();
        let mut added = 0;
        for t in tasks {
            let safe_id = !t.id.is_empty() && t.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            if !safe_id || !have.insert(t.id.clone()) {
                continue;
            }
            save_task_file(&self.root, &t)?;
            idx.tasks.push(t);
            added += 1;
        }
        if added > 0 {
            idx.tasks.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            save_index(&self.root, &idx)?;
        }
        Ok(added)
    }

    /// Remove task metadata older than the call retention window, keeping any
    /// unfinished tasks or appointments whose scheduled date has not yet aged out.
    pub async fn purge_expired(&self, retention_days: u32) {
        let days = i64::from(retention_days.clamp(1, 3650));
        let cutoff = Utc::now() - Duration::days(days);
        let local_now = chrono::Local::now().naive_local();
        let mut expired = Vec::new();
        {
            let mut idx = self.index.lock().await;
            idx.tasks.retain(|task| {
                if Self::should_retain_on_purge(task, cutoff, local_now, days) {
                    true
                } else {
                    expired.push(task.id.clone());
                    false
                }
            });
            if !expired.is_empty() {
                if let Err(e) = save_index(&self.root, &idx) {
                    warn!("expired task index save failed: {e:#}");
                }
            }
        }
        for id in &expired {
            let path = self.root.join(format!("{id}.json"));
            if let Err(e) = fs::remove_file(&path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    warn!("expired task remove failed path={}: {e}", path.display());
                }
            }
        }
        if !expired.is_empty() {
            info!(
                "expired task data purged records={} retention_days={}",
                expired.len(),
                retention_days
            );
        }
    }

    fn should_retain_on_purge(
        task: &TaskRecord,
        cutoff: DateTime<Utc>,
        local_now: chrono::NaiveDateTime,
        retention_days: i64,
    ) -> bool {
        if let Some(appt) = task.appointment.as_ref() {
            if appt.status == crate::appointments::PENDING {
                return true;
            }
            let start = chrono::NaiveDateTime::parse_from_str(appt.start.trim(), "%Y-%m-%dT%H:%M")
                .or_else(|_| chrono::NaiveDateTime::parse_from_str(appt.start.trim(), "%Y-%m-%dT%H:%M:%S"));
            if let Ok(dt) = start {
                if dt + Duration::days(retention_days) > local_now {
                    return true;
                }
            }
        }
        let stamp = if task.updated_at.trim().is_empty() {
            &task.created_at
        } else {
            &task.updated_at
        };
        !DateTime::parse_from_rfc3339(stamp)
            .map(|dt| dt.with_timezone(&Utc) < cutoff)
            .unwrap_or(false)
    }

    /// Delete every task belonging to any of `call_ids` (used when call history
    /// is deleted from the console). Returns the number of removed tasks.
    pub async fn delete_by_call_ids(
        &self,
        call_ids: &std::collections::HashSet<String>,
    ) -> usize {
        if call_ids.is_empty() {
            return 0;
        }
        let mut removed = Vec::new();
        {
            let mut idx = self.index.lock().await;
            idx.tasks.retain(|task| {
                if call_ids.contains(&task.call_id) {
                    removed.push(task.id.clone());
                    false
                } else {
                    true
                }
            });
            if !removed.is_empty() {
                if let Err(e) = save_index(&self.root, &idx) {
                    warn!("task index save after call delete failed: {e:#}");
                }
            }
        }
        for id in &removed {
            let path = self.root.join(format!("{id}.json"));
            if let Err(e) = fs::remove_file(&path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    warn!("task file remove failed path={}: {e}", path.display());
                }
            }
        }
        removed.len()
    }

    /// Persist a new task (assigns `id` and timestamps when empty).
    pub async fn create(&self, mut task: TaskRecord) -> Result<TaskRecord> {
        let now = now_rfc3339();
        if task.id.is_empty() {
            task.id = Uuid::new_v4().to_string();
        }
        if task.created_at.is_empty() {
            task.created_at = now;
        }
        if task.updated_at.is_empty() {
            task.updated_at = task.created_at.clone();
        }
        task.summary = normalize_task_text(&task.summary);
        task.transcript = normalize_task_text(&task.transcript);
        self.persist(&task).await?;
        info!(
            "task create id={} call_id={} status={:?}",
            task.id, task.call_id, task.status
        );
        Ok(task)
    }

    pub async fn get(&self, id: &str) -> Option<TaskRecord> {
        {
            let idx = self.index.lock().await;
            if let Some(t) = idx.tasks.iter().find(|t| t.id == id) {
                return Some(t.clone());
            }
        }
        load_task_file(&self.root, id).ok()
    }

    /// Newest-first, capped at `limit` (0 = return all).
    pub async fn list(&self, limit: usize) -> Vec<TaskRecord> {
        let idx = self.index.lock().await;
        if limit == 0 {
            idx.tasks.clone()
        } else {
            idx.tasks.iter().take(limit).cloned().collect()
        }
    }

    async fn mutate<F>(&self, id: &str, f: F) -> Result<TaskRecord>
    where
        F: FnOnce(&mut TaskRecord),
    {
        let mut idx = self.index.lock().await;
        let pos = if let Some(pos) = idx.tasks.iter().position(|t| t.id == id) {
            pos
        } else {
            let loaded = load_task_file(&self.root, id)
                .with_context(|| format!("task not found: {id}"))?;
            idx.tasks.insert(0, loaded);
            0
        };
        f(&mut idx.tasks[pos]);
        idx.tasks[pos].updated_at = now_rfc3339();
        let updated = idx.tasks[pos].clone();
        save_task_file(&self.root, &updated)?;
        idx.tasks.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if let Err(e) = save_index(&self.root, &idx) {
            warn!("tasks index save: {e:#}");
        }
        Ok(updated)
    }

    pub async fn update_status(&self, id: &str, status: TaskStatus) -> Result<TaskRecord> {
        self.mutate(id, |task| {
            task.status = status;
        })
        .await
    }

    /// Merge fulfillment / extract fields; bumps `updated_at`.
    pub async fn patch(&self, id: &str, patch: TaskPatch) -> Result<TaskRecord> {
        self.mutate(id, |task| {
            if let Some(v) = patch.summary {
                task.summary = normalize_task_text(&v);
            }
            if let Some(v) = patch.transcript {
                task.transcript = normalize_task_text(&v);
            }
            if let Some(v) = patch.intent {
                task.intent = v;
            }
            if let Some(v) = patch.datetime_hint {
                task.datetime_hint = v;
            }
            if let Some(v) = patch.calendar_event_id {
                task.calendar_event_id = Some(v);
            }
            if let Some(v) = patch.openclaw_ref {
                task.openclaw_ref = Some(v);
            }
            if let Some(v) = patch.fulfill_error {
                task.fulfill_error = v;
            }
            if let Some(v) = patch.reply_method {
                task.reply_method = v;
            }
            if let Some(v) = patch.reply_contact {
                task.reply_contact = v;
            }
            if let Some(v) = patch.contact_verified {
                task.contact_verified = v;
            }
            if let Some(v) = patch.appointment {
                task.appointment = Some(v);
            }
        })
        .await
    }

    pub async fn push_event(&self, id: &str, event: &str) -> Result<TaskRecord> {
        let evt = event.to_string();
        self.mutate(id, |task| {
            task.events.push(evt);
        })
        .await
    }

    async fn persist(&self, task: &TaskRecord) -> Result<()> {
        save_task_file(&self.root, task)?;
        let mut idx = self.index.lock().await;
        if let Some(slot) = idx.tasks.iter_mut().find(|t| t.id == task.id) {
            *slot = task.clone();
        } else {
            idx.tasks.insert(0, task.clone());
        }
        idx.tasks.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if let Err(e) = save_index(&self.root, &idx) {
            warn!("tasks index save: {e:#}");
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a pending task from a voicemail capture; summary/transcript filled later by extract.
pub fn draft_from_voicemail(
    call_id: &str,
    serial: &str,
    caller_id: &str,
    voicemail_path: &str,
    reply_method: ReplyMethod,
    reply_contact: Option<String>,
    contact_verified: bool,
) -> TaskRecord {
    let now = now_rfc3339();
    TaskRecord {
        id: Uuid::new_v4().to_string(),
        created_at: now.clone(),
        updated_at: now,
        call_id: call_id.to_string(),
        serial: serial.to_string(),
        caller_id: caller_id.to_string(),
        status: TaskStatus::Pending,
        intent: TaskIntent::Message,
        summary: String::new(),
        transcript: String::new(),
        datetime_hint: None,
        reply_method,
        reply_contact,
        contact_verified,
        voicemail_path: Some(voicemail_path.to_string()),
        calendar_event_id: None,
        fulfill_error: None,
        openclaw_ref: None,
        events: vec!["draft_from_voicemail".to_string()],
        appointment: None,
    }
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn task_file_path(root: &Path, id: &str) -> PathBuf {
    root.join(format!("{id}.json"))
}

fn index_path(root: &Path) -> PathBuf {
    root.join("index.json")
}

fn save_task_file(root: &Path, task: &TaskRecord) -> Result<()> {
    fs::create_dir_all(root)?;
    let path = task_file_path(root, &task.id);
    let tmp = root.join(format!("{}.json.tmp", task.id));
    let data = serde_json::to_vec_pretty(task)?;
    {
        let mut f = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(&data)?;
        f.flush()?;
    }
    fs::rename(&tmp, &path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

fn load_task_file(root: &Path, id: &str) -> Result<TaskRecord> {
    let path = task_file_path(root, id);
    let mut f = File::open(&path).with_context(|| format!("open {}", path.display()))?;
    let mut buf = String::new();
    f.read_to_string(&mut buf)?;
    Ok(serde_json::from_str(&buf)?)
}

fn save_index(root: &Path, index: &TaskIndex) -> Result<()> {
    fs::create_dir_all(root)?;
    let path = index_path(root);
    let tmp = root.join("index.json.tmp");
    let data = serde_json::to_vec_pretty(index)?;
    {
        let mut f = File::create(&tmp)?;
        f.write_all(&data)?;
        f.flush()?;
    }
    fs::rename(&tmp, &path)?;
    Ok(())
}

fn load_index(root: &Path) -> Result<TaskIndex> {
    let path = index_path(root);
    if !path.exists() {
        return Ok(TaskIndex::default());
    }
    let mut f = File::open(&path).with_context(|| format!("open {}", path.display()))?;
    let mut buf = String::new();
    f.read_to_string(&mut buf)?;
    Ok(serde_json::from_str(&buf).unwrap_or_default())
}

fn load_or_rebuild_index(root: &Path) -> Result<TaskIndex> {
    let index = load_index(root)?;
    if !index.tasks.is_empty() {
        return Ok(index);
    }
    let mut tasks = Vec::new();
    if root.is_dir() {
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "index.json" || name.ends_with(".tmp") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            if let Ok(task) = serde_json::from_str::<TaskRecord>(&text) {
                tasks.push(task);
            }
        }
    }
    tasks.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(TaskIndex { tasks })
}

fn has_japanese_kana(text: &str) -> bool {
    text.chars().any(|c| ('\u{3040}'..='\u{30FF}').contains(&c))
}

fn normalize_task_text_in(lang: crate::locale::Lang, text: &str) -> String {
    if lang == crate::locale::Lang::ZhTw && !has_japanese_kana(text) {
        crate::zh_tw::to_tw_whole_in(lang, text)
    } else {
        text.to_string()
    }
}

fn normalize_task_text(text: &str) -> String {
    normalize_task_text_in(crate::locale::notify_lang(), text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_tasks_root() -> PathBuf {
        std::env::temp_dir().join(format!("pixel-call-tasks-test-{}", Uuid::new_v4()))
    }

    #[tokio::test]
    async fn create_list_roundtrip() {
        let root = temp_tasks_root();
        let store = TaskStore::new(root.clone());

        let draft = draft_from_voicemail(
            "call-1",
            "serial-a",
            "+886912345678",
            "2026-08-01/call-1_vm.wav",
            ReplyMethod::Phone,
            Some("+886912345678".into()),
            true,
        );
        let id = draft.id.clone();
        let created = store.create(draft).await.expect("create");
        assert_eq!(created.id, id);
        assert_eq!(created.status, TaskStatus::Pending);

        let listed = store.list(10).await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
        assert_eq!(listed[0].call_id, "call-1");

        let fetched = store.get(&id).await.expect("get");
        assert_eq!(
            fetched.voicemail_path.as_deref(),
            Some("2026-08-01/call-1_vm.wav")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn summary_and_transcript_are_read_in_traditional_chinese_whole() {
        let root = temp_tasks_root();
        let store = TaskStore::new(root.clone());
        let mut draft = draft_from_voicemail("call-tw", "serial-a", "0900000000", "", ReplyMethod::Phone, None, false);
        draft.summary = "他会来".into();
        draft.transcript = "對方：会来".into();
        let created = store.create(draft).await.expect("create");
        assert_eq!(created.summary, "他會來");
        assert_eq!(created.transcript, "對方：會來");
        let patched = store
            .patch(
                &created.id,
                TaskPatch { summary: Some("他会来".into()), transcript: Some("對方：失礼します".into()), ..Default::default() },
            )
            .await
            .expect("patch");
        assert_eq!(patched.summary, "他會來");
        // Japanese stays as it is.
        assert_eq!(patched.transcript, "對方：失礼します");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn preserves_multiline_japanese_kanji_and_simplified_chinese_by_notify_lang() {
        // Multi-line Japanese with a pure-kanji line ("会社説明会") + kana line must NOT convert "会" -> "會".
        let ja_multiline = "会社説明会\nお電話ください";
        assert_eq!(normalize_task_text_in(crate::locale::Lang::ZhTw, ja_multiline), ja_multiline);

        let root = temp_tasks_root();
        let store = TaskStore::new(root.clone());
        let mut draft = draft_from_voicemail("call-ja", "serial-b", "0900000001", "", ReplyMethod::Phone, None, false);
        draft.summary = ja_multiline.into();
        draft.transcript = "電話番号\nお電話ください".into();
        let created = store.create(draft).await.expect("create");
        assert_eq!(created.summary, ja_multiline);
        assert_eq!(created.transcript, "電話番号\nお電話ください");

        // Simplified Chinese converts ONLY for Lang::ZhTw; preserved for ZhCn and all non-ZhTw UI langs.
        for lang in crate::locale::ALL {
            let input = "明天开会请回电";
            let out = normalize_task_text_in(lang, input);
            if lang == crate::locale::Lang::ZhTw {
                assert_eq!(out, "明天開會請回電");
            } else {
                assert_eq!(out, input, "{lang:?} must preserve Simplified Chinese");
            }
        }
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn purge_expired_keeps_pending_and_future_appointments() {
        let root = temp_tasks_root();
        let store = TaskStore::new(root.clone());
        let old_ts = (Utc::now() - Duration::days(20)).to_rfc3339_opts(SecondsFormat::Secs, true);
        let future_start = (chrono::Local::now().naive_local() + Duration::days(10))
            .format("%Y-%m-%dT%H:%M")
            .to_string();
        let past_start = (chrono::Local::now().naive_local() - Duration::days(20))
            .format("%Y-%m-%dT%H:%M")
            .to_string();

        let mut pending_appt = draft_from_voicemail("c1", "s1", "0900000001", "", ReplyMethod::Phone, None, false);
        pending_appt.created_at = old_ts.clone();
        pending_appt.updated_at = old_ts.clone();
        pending_appt.status = TaskStatus::Pending;
        pending_appt.intent = TaskIntent::Appointment;
        pending_appt.appointment = Some(crate::appointments::Appointment {
            start: past_start.clone(),
            status: crate::appointments::PENDING.to_string(),
            ..Default::default()
        });
        let pending_id = pending_appt.id.clone();
        store.create(pending_appt).await.unwrap();

        let mut future_confirmed = draft_from_voicemail("c2", "s1", "0900000002", "", ReplyMethod::Phone, None, false);
        future_confirmed.created_at = old_ts.clone();
        future_confirmed.updated_at = old_ts.clone();
        future_confirmed.status = TaskStatus::Fulfilled;
        future_confirmed.intent = TaskIntent::Appointment;
        future_confirmed.appointment = Some(crate::appointments::Appointment {
            start: future_start,
            status: crate::appointments::CONFIRMED.to_string(),
            ..Default::default()
        });
        let future_id = future_confirmed.id.clone();
        store.create(future_confirmed).await.unwrap();

        let mut old_ignored = draft_from_voicemail("c3", "s1", "0900000003", "", ReplyMethod::None, None, false);
        old_ignored.created_at = old_ts.clone();
        old_ignored.updated_at = old_ts;
        old_ignored.status = TaskStatus::Ignored;
        old_ignored.intent = TaskIntent::Appointment;
        old_ignored.appointment = Some(crate::appointments::Appointment {
            start: past_start,
            status: crate::appointments::DECLINED.to_string(),
            ..Default::default()
        });
        let ignored_id = old_ignored.id.clone();
        store.create(old_ignored).await.unwrap();

        store.purge_expired(15).await;

        assert!(store.get(&pending_id).await.is_some(), "pending appointment must not be purged");
        assert!(store.get(&future_id).await.is_some(), "future confirmed appointment must not be purged");
        assert!(store.get(&ignored_id).await.is_none(), "old declined appointment should be purged");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn concurrent_patch_and_status_updates_do_not_clobber_fields() {
        let root = temp_tasks_root();
        let store = TaskStore::new(root.clone());
        let mut draft = draft_from_voicemail("c-race", "s1", "0900000009", "", ReplyMethod::Phone, None, false);
        draft.intent = TaskIntent::Appointment;
        draft.appointment = Some(crate::appointments::Appointment {
            start: "2026-12-01T10:00".to_string(),
            status: crate::appointments::PENDING.to_string(),
            ..Default::default()
        });
        let id = draft.id.clone();
        store.create(draft).await.unwrap();

        let confirmed = crate::appointments::Appointment {
            start: "2026-12-01T10:00".to_string(),
            status: crate::appointments::CONFIRMED.to_string(),
            ..Default::default()
        };
        let (r1, r2, r3) = tokio::join!(
            store.patch(&id, TaskPatch { appointment: Some(confirmed), ..Default::default() }),
            store.patch(&id, TaskPatch { fulfill_error: Some(None), ..Default::default() }),
            store.push_event(&id, "callback_place_call_ack"),
        );
        r1.unwrap();
        r2.unwrap();
        r3.unwrap();

        let latest = store.get(&id).await.unwrap();
        assert_eq!(latest.appointment.as_ref().map(|a| a.status.as_str()), Some(crate::appointments::CONFIRMED));
        assert!(latest.events.iter().any(|e| e == "callback_place_call_ack"));
        let _ = fs::remove_dir_all(root);
    }
}
