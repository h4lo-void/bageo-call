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

    /// Remove task metadata older than the call retention window.
    pub async fn purge_expired(&self, retention_days: u32) {
        let cutoff = Utc::now() - Duration::days(i64::from(retention_days.clamp(1, 3650)));
        let mut expired = Vec::new();
        {
            let mut idx = self.index.lock().await;
            idx.tasks.retain(|task| {
                let is_expired = DateTime::parse_from_rfc3339(&task.created_at)
                    .map(|dt| dt.with_timezone(&Utc) < cutoff)
                    .unwrap_or(false);
                if is_expired {
                    expired.push(task.id.clone());
                    false
                } else {
                    true
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

    /// Persist a new task (assigns `id` and timestamps when empty).
    pub async fn create(&self, mut task: TaskRecord) -> Result<TaskRecord> {
        let now = now_rfc3339();
        if task.id.is_empty() {
            task.id = Uuid::new_v4().to_string();
        }
        if task.created_at.is_empty() {
            task.created_at = now.clone();
        }
        if task.updated_at.is_empty() {
            task.updated_at = now;
        }
        task.summary = crate::zh_tw::to_tw(&task.summary);
        task.transcript = crate::zh_tw::to_tw(&task.transcript);
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

    pub async fn update_status(&self, id: &str, status: TaskStatus) -> Result<TaskRecord> {
        let mut task = self
            .get(id)
            .await
            .with_context(|| format!("task not found: {id}"))?;
        task.status = status;
        task.updated_at = now_rfc3339();
        self.persist(&task).await?;
        Ok(task)
    }

    /// Merge fulfillment / extract fields; bumps `updated_at`.
    pub async fn patch(&self, id: &str, patch: TaskPatch) -> Result<TaskRecord> {
        let mut task = self
            .get(id)
            .await
            .with_context(|| format!("task not found: {id}"))?;
        if let Some(v) = patch.summary {
            task.summary = crate::zh_tw::to_tw(&v);
        }
        if let Some(v) = patch.transcript {
            task.transcript = crate::zh_tw::to_tw(&v);
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
        task.updated_at = now_rfc3339();
        self.persist(&task).await?;
        Ok(task)
    }

    pub async fn push_event(&self, id: &str, event: &str) -> Result<TaskRecord> {
        let mut task = self
            .get(id)
            .await
            .with_context(|| format!("task not found: {id}"))?;
        task.events.push(event.to_string());
        task.updated_at = now_rfc3339();
        self.persist(&task).await?;
        Ok(task)
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
}
