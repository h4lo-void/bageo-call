//! Remote control from a chat: away from the web console, the owner looks up
//! calls, has the AI make a call, decides to-dos and answers the AI's
//! questions from the chat the notifications go to.
//!
//! # Layers
//!
//! - [command]: what the owner can ask for ([Action]), from a slash command
//!   ("/calls today") or, for free text, from a small Gemini model ([nlu]).
//!   Both go through the same checks ([command::validate]).
//! - [Controller]: who may ask ([access]: off by default, one switch per chat,
//!   an optional list of senders, rate limits), the confirmation of anything
//!   with side effects ([confirm]), and the actions. These call what the web
//!   console's API calls (errands::create / cancel / say, the phone mode
//!   switch, fulfill and appointments for to-dos, the call and contact
//!   stores), with the same checks; nothing goes through HTTP.
//! - Platform adapters, thin transports: [telegram], [discord] (Gateway),
//!   [slack] (Socket Mode) and [matrix] (/sync); the last three share [link].
//!
//! # Writing an adapter (Discord Gateway, Slack Socket Mode, Matrix sync, ntfy, LINE, MCP)
//!
//! An adapter does three things and nothing else.
//!
//! 1. Receive. Turn each message or button press into an [Inbound]:
//!    channel_id = the notification channel's id on the settings page
//!    (notify::Channel::id; [ENV_TELEGRAM] for the environment's bot),
//!    platform = that channel's kind ("telegram", "discord"…), chat_id = the
//!    conversation, written as the channel's target is, sender_id = the
//!    platform's user id of who wrote, body = [Body::Text] or [Body::Button]
//!    (the data of the pressed button, as [Button::data] set it),
//!    message_id = the platform's id of that message (for a press: the
//!    message carrying the button), reply_to_bot = it answers a message the
//!    bot sent. In a group, pass on only what is addressed to the bot: a
//!    command, a mention, a reply to the bot.
//! 2. Hand it to [Controller::handle]. None means: send nothing. The chat or
//!    sender is not allowed, or is over its limit; answering would tell a
//!    stranger that the bot is there.
//! 3. Send the [Outbound]: text (plain, at most 4000 characters), buttons
//!    (a label, and the data to give back in [Body::Button] when pressed; a
//!    platform without buttons shows only the text, which always says how to
//!    answer by typing: 1 / 2, or a reply), edit (edit that message instead
//!    of sending a new one). Also subscribe ([Controller::subscribe]) and send
//!    every [Push] whose channel_id is one of yours (the AI's questions during
//!    a call).
//!
//! What a chat may do is decided here: an adapter never filters, rewords or
//! adds commands. A new kind needs its name in [COMMAND_KINDS] (the settings
//! page then shows the switch for its channels). An MCP tool interface maps
//! each tool to an [Action] ([crate::mcp]): [Controller::perform] runs the
//! ones without side effects; a call goes through [Controller::request_errand],
//! which waits for the owner's yes in the command chats unless the owner set
//! `mcp_calls` to `direct`.
//!
//! # Safety
//!
//! Off by default; one switch per channel; only that channel's chat, and only
//! the listed senders when there is a list; no answer at all to anything
//! else (logged). Messages per minute and AI calls per hour are limited per
//! chat. Calls go through errands::create (its number checks, SIM rules and
//! queue). Replies never carry a key or token (they are cut out before
//! sending); transcript excerpts are short. Only the owner's own message is
//! read as a command: transcripts and summaries are never given to the model.

pub mod access;
pub mod command;
pub mod confirm;
pub mod discord;
pub mod link;
pub mod matrix;
pub mod nlu;
pub mod slack;
pub mod telegram;
pub mod words;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod sec_tests;
#[cfg(test)]
mod chat_tests;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::calls::{CallListFilter, CallRecord};
use crate::calls_http::CallsState;
use crate::errands::{self, Errand};
use crate::locale;
use crate::settings_store::RuntimeSettings;
use crate::tasks::{TaskRecord, TaskStatus};

use command::{Action, CallQuery, Only, Parsed};
use confirm::{Confirmations, Job};
use words::{words, Words};

/// The environment's Telegram bot (TELEGRAM_BOT_TOKEN + TELEGRAM_CHAT_ID), as
/// the notifications name it.
pub const ENV_TELEGRAM: &str = "env-telegram";
/// Channel kinds that can take commands (an adapter exists for them).
pub const COMMAND_KINDS: &[&str] = &["telegram", "discord", "slack", "matrix"];
/// How long a confirmation waits for the owner.
pub const CONFIRM_TTL: Duration = Duration::from_secs(120);
/// At most this much of a call's transcript goes to a chat.
pub const TRANSCRIPT_MAX_CHARS: usize = 600;
const SUMMARY_MAX_CHARS: usize = 1200;
const OUT_MAX_CHARS: usize = 4000;
const LIST_MAX: usize = 10;
/// Telegram's limit for a button's data; the shortest of the platforms.
pub const BUTTON_DATA_MAX: usize = 64;
/// A question's buttons still answer this long after the AI stopped waiting
/// (the answer then comes back as too late).
const QUESTION_GRACE: Duration = Duration::from_secs(5);

/// What a message carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    Text(String),
    /// The data of a pressed button ([Button::data]).
    Button(String),
}

/// A message or button press from a chat (see the module documentation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inbound {
    pub channel_id: String,
    pub platform: String,
    pub chat_id: String,
    pub sender_id: String,
    pub body: Body,
    pub message_id: String,
    pub reply_to_bot: bool,
    /// Known to be a private chat with the bot (a Discord direct message, a
    /// Matrix room of only the owner and the bot): it needs no list of who may
    /// give commands. Telegram and Slack tell it by the chat's id instead.
    pub direct: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Button {
    pub label: String,
    pub data: String,
}

/// What to show in the chat.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Outbound {
    pub text: String,
    pub buttons: Vec<Button>,
    /// Edit this message (the platform's id) instead of sending a new one.
    pub edit: Option<String>,
}

/// A message the agent starts: to one chat of one channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Push {
    pub channel_id: String,
    pub chat_id: String,
    pub out: Outbound,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PhoneLine {
    pub serial: String,
    pub label: String,
    pub connected: bool,
    /// "ai" or "normal".
    pub mode: String,
    pub in_call: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CallLine {
    pub id: String,
    /// Local time, "11/14 20:31".
    pub when: String,
    /// The contact's name; empty when the number is not a contact.
    pub who: String,
    /// Empty: hidden number.
    pub number: String,
    /// The first line of the summary.
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CallDetail {
    pub id: String,
    pub when: String,
    pub who: String,
    pub number: String,
    pub summary: String,
    /// The start of the dialogue, at most [TRANSCRIPT_MAX_CHARS].
    pub transcript: String,
    /// The call in the web console; empty when it has no address.
    pub link: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskLine {
    pub id: String,
    pub who: String,
    pub number: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrandLine {
    pub id: String,
    pub who: String,
    pub number: String,
    /// errands::QUEUED, DIALING, ON_CALL, DONE…
    pub status: String,
    /// Where it stands, in the owner's words (errands::owner_status).
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContactLine {
    pub name: String,
    pub phones: Vec<String>,
}

/// What an action came to: the structured result ([words::render] makes the
/// text in the owner's language).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    Help,
    Hint,
    Usage { command: String },
    Status { version: String, phones: Vec<PhoneLine>, live_calls: usize },
    Calls { calls: Vec<CallLine> },
    Call { call: CallDetail },
    Tasks { tasks: Vec<TaskLine> },
    Decided { task: String, text: String },
    ErrandStarted { errand: ErrandLine },
    Errands { errands: Vec<ErrandLine> },
    ErrandStopped { errand: ErrandLine },
    Said,
    Answered,
    ModeSet { phone: String, mode: String },
    ModeFailed { phone: String, reason: String },
    Contacts { contacts: Vec<ContactLine> },
    ContactKindSet { list: String, number: String, name: String },
    Confirm { code: String, summary: String },
    Dropped,
    Expired,
    TooMany { limit: u32 },
    Closed,
    NotFound { what: String },
    NoContact { what: String },
    ManyContacts { names: Vec<String> },
    Failed { code: String },
}

/// An error code of an action (the web API's codes) as an outcome.
pub fn error_outcome(code: &str) -> Outcome {
    match code {
        "question_closed" => Outcome::Closed,
        other => Outcome::Failed { code: other.to_string() },
    }
}

/// A question from the AI on a call, open in a chat.
#[derive(Debug, Clone)]
struct OpenQuestion {
    errand: String,
    id: String,
    options: Vec<String>,
    until: Instant,
}

/// What one chat was shown last (the numbers in its lists), and the AI's
/// question open in it.
#[derive(Default)]
struct Memory {
    calls: Vec<String>,
    tasks: Vec<String>,
    errands: Vec<String>,
    phones: Vec<String>,
    question: Option<OpenQuestion>,
}

/// A secret as it may appear in a reply: as it is, and percent-encoded the
/// way a URL carries it (upper- and lower-case hex).
fn secret_forms(s: &str) -> Vec<String> {
    let mut up = String::new();
    let mut low = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            up.push(b as char);
            low.push(b as char);
        } else {
            up.push_str(&format!("%{b:02X}"));
            low.push_str(&format!("%{b:02x}"));
        }
    }
    let mut out = vec![s.to_string()];
    for f in [up, low] {
        if !out.contains(&f) {
            out.push(f);
        }
    }
    out
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn first_line(s: &str, max: usize) -> String {
    clip(s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or(""), max)
}

fn local_time(stamp: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(stamp.trim())
        .map(|t| t.with_timezone(&chrono::Local).format("%m/%d %H:%M").to_string())
        .unwrap_or_else(|_| stamp.chars().take(16).collect())
}

fn local_day(stamp: &str) -> Option<NaiveDate> {
    chrono::DateTime::parse_from_rfc3339(stamp.trim())
        .ok()
        .map(|t| t.with_timezone(&chrono::Local).date_naive())
}

fn in_progress(c: &CallRecord) -> bool {
    matches!(c.status.as_str(), "ringing" | "active")
}

/// Never picked up: no "active" mark (the AI or the owner never answered).
fn missed(c: &CallRecord) -> bool {
    !in_progress(c) && !c.events.iter().any(|e| e == "active")
}

fn scam(c: &CallRecord) -> bool {
    c.status == "blocked_spam" || c.events.iter().any(|e| e == "persona_stall_spam" || e == "blocked_spam")
}

/// An item named in a chat: its number in the list last shown there ("2",
/// "#2"), its id, or a unique start of its id (at least 4 characters).
fn pick_ref(listed: &[String], known: &[String], r: &str) -> Option<String> {
    let r = r.trim();
    if r.is_empty() {
        return None;
    }
    let n = r.trim_start_matches('#');
    if n.len() <= 3 {
        if let Ok(i) = n.parse::<usize>() {
            return (i >= 1).then(|| listed.get(i - 1).cloned()).flatten();
        }
    }
    if let Some(k) = known.iter().find(|k| k.eq_ignore_ascii_case(r)) {
        return Some(k.clone());
    }
    if r.chars().count() >= 4 {
        let low = r.to_ascii_lowercase();
        let hits: Vec<&String> = known.iter().filter(|k| k.to_ascii_lowercase().starts_with(&low)).collect();
        if hits.len() == 1 {
            return Some(hits[0].clone());
        }
    }
    None
}

/// A typed answer to a confirmation: 1 = go ahead, 2 = drop it.
fn yes_no(text: &str) -> Option<bool> {
    match text.trim() {
        "1" | "１" => Some(true),
        "2" | "２" => Some(false),
        _ => None,
    }
}

fn reply(o: &Outcome, w: &Words) -> Outbound {
    Outbound { text: words::render(o, w), ..Default::default() }
}

/// The kind of an action, for the log (its text stays out of the log).
fn action_kind(a: &Action) -> &'static str {
    match a {
        Action::Help => "help",
        Action::Status => "status",
        Action::Calls(_) => "calls",
        Action::Call { .. } => "call",
        Action::Tasks => "tasks",
        Action::Decide { confirm: true, .. } => "confirm",
        Action::Decide { .. } => "ignore",
        Action::Errand { .. } => "errand",
        Action::Errands => "errands",
        Action::Cancel { .. } => "cancel",
        Action::Say { .. } => "say",
        Action::Answer { .. } => "answer",
        Action::Mode { .. } => "mode",
        Action::Contacts { .. } => "contacts",
        Action::SetContactKind { .. } => "set_contact_kind",
    }
}

/// Operates the agent for the chats that may (see the module documentation).
pub struct Controller {
    state: CallsState,
    confirms: Confirmations,
    limits: access::Limits,
    memory: Mutex<HashMap<String, Memory>>,
    pushes: broadcast::Sender<Push>,
    gemini_base: String,
    /// Groups told how to add a sender list ([access::Verdict::NeedsList]): when.
    hinted: Mutex<HashMap<String, Instant>>,
}

/// Who sent a command through the API (`POST /calls/api/control/inbound`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiCaller {
    /// With the console password (or none is set): the owner.
    Owner,
    /// With the MCP token (MCP switched on): what an MCP client may do
    /// ([crate::mcp]), under its limits.
    Mcp,
}

/// Why an API command got no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiRefused {
    /// The sender is not on the list of who may give commands there.
    NotOnList,
    /// Over the messages-per-minute limit.
    TooMany,
}

/// How often a group without a sender list is told how to add one.
const LIST_HINT_EVERY: Duration = Duration::from_secs(10 * 60);

/// What a group without a sender list is told (`{id}`: the sender's id).
fn list_hint_text(lang: locale::Lang) -> &'static str {
    use locale::Lang;
    match lang {
        Lang::ZhTw => "要在這裡下指令，請先到 Bageo 控制台的設定頁，在這個聊天室的「可以下指令的人」填上你的 ID。你的 ID：{id}",
        Lang::ZhCn => "要在这里下指令，请先到 Bageo 控制台的设置页，在这个聊天室的“可以下指令的人”填上你的 ID。你的 ID：{id}",
        Lang::Ja => "ここから操作するには、Bageo コンソールの設定ページで、このチャットの「操作できる人」にあなたの ID を入れてください。あなたの ID：{id}",
        Lang::Ko => "여기서 명령하려면 Bageo 콘솔 설정 페이지에서 이 채팅의 '명령할 수 있는 사람'에 내 ID를 넣으세요. 내 ID: {id}",
        Lang::Es => "Para dar comandos aquí, en la configuración de la consola de Bageo añade tu ID en «Quién puede dar comandos» de este chat. Tu ID: {id}",
        Lang::Fr => "Pour donner des commandes ici, ajoutez votre ID dans « Qui peut donner des commandes » de ce chat, dans les réglages de la console Bageo. Votre ID : {id}",
        Lang::De => "Um hier Befehle zu geben, trage in den Einstellungen der Bageo-Konsole deine ID unter „Wer Befehle geben darf“ für diesen Chat ein. Deine ID: {id}",
        Lang::Pt => "Para dar comandos aqui, coloque seu ID em “Quem pode dar comandos” deste chat, nas configurações do console do Bageo. Seu ID: {id}",
        Lang::En => "To give commands here, add your ID under “Who may give commands” for this chat in the Bageo console's settings. Your ID: {id}",
    }
}

impl Controller {
    pub fn new(state: CallsState) -> Arc<Self> {
        Self::with(state, CONFIRM_TTL, nlu::GEMINI_BASE)
    }

    /// [Controller::new] with another confirmation time and Gemini address (tests).
    pub fn with(state: CallsState, confirm_ttl: Duration, gemini_base: &str) -> Arc<Self> {
        let (pushes, _) = broadcast::channel(32);
        Arc::new(Self {
            state,
            confirms: Confirmations::new(confirm_ttl),
            limits: access::Limits::default(),
            memory: Mutex::new(HashMap::new()),
            pushes,
            gemini_base: gemini_base.trim_end_matches('/').to_string(),
            hinted: Mutex::new(HashMap::new()),
        })
    }

    /// The agent's settings now.
    pub fn settings(&self) -> Arc<RuntimeSettings> {
        self.state.settings_store.snapshot()
    }

    /// Messages the agent starts (the AI's questions during a call).
    pub fn subscribe(&self) -> broadcast::Receiver<Push> {
        self.pushes.subscribe()
    }

    fn mem<R>(&self, chat: &str, f: impl FnOnce(&mut Memory) -> R) -> R {
        let mut all = self.memory.lock().unwrap_or_else(|e| e.into_inner());
        f(all.entry(chat.to_string()).or_default())
    }

    /// The answer to one message, or None: send nothing (see the module documentation).
    pub async fn handle(&self, m: &Inbound) -> Option<Outbound> {
        let rt = self.settings();
        let grant = match access::check(&rt, m) {
            access::Verdict::Allow(grant) => grant,
            access::Verdict::NeedsList => {
                warn!(
                    channel = m.channel_id.as_str(),
                    platform = m.platform.as_str(),
                    chat = m.chat_id.as_str(),
                    sender = m.sender_id.as_str(),
                    "chat command in a group without a list of who may give commands: not run"
                );
                return self.list_hint(&rt, m);
            }
            access::Verdict::Deny => {
                warn!(
                    channel = m.channel_id.as_str(),
                    platform = m.platform.as_str(),
                    chat = m.chat_id.as_str(),
                    sender = m.sender_id.as_str(),
                    "chat command from a chat or sender that may not give commands: no answer"
                );
                return None;
            }
        };
        if !self.limits.allow_message(&grant.chat, rt.control_commands_per_minute) {
            warn!(chat = grant.chat.as_str(), "chat over its messages-per-minute limit: no answer");
            return None;
        }
        let w = words(locale::notify_lang());
        let out = match &m.body {
            Body::Button(data) => self.on_button(&rt, &grant.chat, m, data, w).await?,
            Body::Text(text) => self.on_text(&rt, &grant.chat, m, text, w).await?,
        };
        Some(self.scrub(&rt, out))
    }

    /// A group switched on without a list of who may give commands: say how
    /// to add one (with the sender's id), at most once per [LIST_HINT_EVERY]
    /// per chat, only for a typed message.
    fn list_hint(&self, rt: &RuntimeSettings, m: &Inbound) -> Option<Outbound> {
        if !matches!(m.body, Body::Text(_)) || m.sender_id.trim().is_empty() {
            return None;
        }
        let key = access::chat_key(&m.channel_id, &m.chat_id);
        {
            let mut hinted = self.hinted.lock().unwrap_or_else(|e| e.into_inner());
            if hinted.get(&key).is_some_and(|t| t.elapsed() < LIST_HINT_EVERY) {
                return None;
            }
            hinted.insert(key, Instant::now());
        }
        let sender: String = m.sender_id.trim().chars().filter(|c| !c.is_control()).take(80).collect();
        let text = list_hint_text(locale::notify_lang()).replace("{id}", &sender);
        Some(self.scrub(rt, Outbound { text, ..Default::default() }))
    }

    async fn on_text(&self, rt: &RuntimeSettings, chat: &str, m: &Inbound, text: &str, w: &'static Words) -> Option<Outbound> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        if let Some(yes) = yes_no(text) {
            if self.confirms.waiting(chat) {
                return Some(reply(&self.settle(rt, chat, &m.sender_id, None, yes).await, w));
            }
        }
        if !text.starts_with('/') && m.reply_to_bot {
            if let Some(q) = self.open_question(chat) {
                return Some(reply(&self.answer_with(chat, Some(q), text), w));
            }
        }
        let action = if text.starts_with('/') {
            match command::parse_slash(text) {
                Parsed::Action(a) => a,
                Parsed::Usage(c) => return Some(reply(&Outcome::Usage { command: c.to_string() }, w)),
                Parsed::Unknown => return Some(reply(&Outcome::Hint, w)),
            }
        } else {
            match self.understand(rt, text).await {
                Some(a) => a,
                None => return Some(reply(&Outcome::Hint, w)),
            }
        };
        info!(chat, action = action_kind(&action), "chat command");
        Some(self.run(rt, chat, &m.sender_id, action, !text.starts_with('/'), w).await)
    }

    async fn on_button(&self, rt: &RuntimeSettings, chat: &str, m: &Inbound, data: &str, w: &'static Words) -> Option<Outbound> {
        if let Some(rest) = data.strip_prefix("k:") {
            let (code, yes) = rest.rsplit_once(':')?;
            let yes = match yes {
                "y" => true,
                "n" => false,
                _ => return None,
            };
            let o = self.settle(rt, chat, &m.sender_id, Some(code), yes).await;
            let edit = (!m.message_id.is_empty()).then(|| m.message_id.clone());
            return Some(Outbound { edit, ..reply(&o, w) });
        }
        if let Some(rest) = data.strip_prefix("q:") {
            let mut parts = rest.splitn(3, ':');
            let (errand, qid) = (parts.next()?, parts.next()?);
            let i: usize = parts.next()?.parse().ok()?;
            let open = self.open_question(chat).filter(|q| q.errand == errand && q.id == qid);
            let o = match open.as_ref().and_then(|q| q.options.get(i).cloned()) {
                Some(text) => self.answer_with(chat, open, &text),
                None => Outcome::Closed,
            };
            return Some(reply(&o, w));
        }
        None
    }

    async fn understand(&self, rt: &RuntimeSettings, text: &str) -> Option<Action> {
        let key = rt.gemini_api_key.trim();
        if key.is_empty() {
            return None;
        }
        nlu::read(&self.state.http, &self.gemini_base, key, text, chrono::Local::now().date_naive()).await
    }

    /// from_model: the action is the model's reading of free text. What it
    /// would say for the owner on a live call (a line for the AI, an answer to
    /// its question) then waits for the owner's yes too: the reading may be wrong.
    async fn run(&self, rt: &RuntimeSettings, chat: &str, sender: &str, action: Action, from_model: bool, w: &'static Words) -> Outbound {
        let speaks = matches!(action, Action::Say { .. } | Action::Answer { .. });
        if !action.needs_confirmation() && !(from_model && speaks) {
            return reply(&self.perform(rt, chat, action).await, w);
        }
        match self.prepare(rt, chat, action, w).await {
            Ok((job, summary)) => {
                let code = self.confirms.ask(chat, sender, job);
                Outbound {
                    text: words::render(&Outcome::Confirm { code: code.clone(), summary }, w),
                    buttons: vec![
                        Button { label: w.yes.to_string(), data: format!("k:{code}:y") },
                        Button { label: w.no.to_string(), data: format!("k:{code}:n") },
                    ],
                    edit: None,
                }
            }
            Err(o) => reply(&o, w),
        }
    }

    /// The owner's yes or no to the confirmation waiting in the chat.
    async fn settle(&self, rt: &RuntimeSettings, chat: &str, sender: &str, code: Option<&str>, yes: bool) -> Outcome {
        match self.confirms.take(chat, sender, code) {
            None => Outcome::Expired,
            Some(_) if !yes => Outcome::Dropped,
            Some(job) => self.execute(rt, chat, job).await,
        }
    }

    /// An action without side effects (anything else needs a confirmation).
    /// chat: whose list numbers it reads and keeps.
    pub async fn perform(&self, rt: &RuntimeSettings, chat: &str, action: Action) -> Outcome {
        match action {
            Action::Help => Outcome::Help,
            Action::Status => self.status(chat).await,
            Action::Calls(q) => self.calls(chat, &q).await,
            Action::Call { r } => self.call(rt, chat, &r).await,
            Action::Tasks => self.tasks(chat).await,
            Action::Errands => self.errands(chat),
            Action::Cancel { r } => self.cancel(chat, &r).await,
            Action::Say { r, text } => match self.errand_ref(chat, &r, |e| e.status == errands::ON_CALL) {
                Ok(id) => match errands::say(&self.state.errands, &id, &text, None) {
                    Ok(()) => Outcome::Said,
                    Err(code) => error_outcome(code),
                },
                Err(o) => o,
            },
            Action::Answer { text } => self.answer_with(chat, None, &text),
            Action::Contacts { query } => self.contacts(&query),
            Action::SetContactKind { kind, number, name } => self.set_contact_kind(&kind, &number, &name).await,
            Action::Errand { .. } | Action::Mode { .. } | Action::Decide { .. } => {
                Outcome::Failed { code: "needs_confirmation".to_string() }
            }
        }
    }

    /// Execute a command or natural-language message from an authenticated API /
    /// webhook client (`POST /calls/api/control/inbound`). The sender lists of
    /// the chat it names apply, and the messages-per-minute limit: the owner's
    /// under that chat's own key, an MCP client's under the MCP clients' one.
    /// An MCP client does what MCP lets it ([Self::mcp_text]).
    pub async fn handle_external(&self, chat: &str, sender: &str, text: &str, caller: ApiCaller) -> Result<Outbound, ApiRefused> {
        let rt = self.settings();
        if !access::api_sender_ok(&rt, chat, sender) {
            warn!(chat, sender, "API command from a sender not on the chat's list: refused");
            return Err(ApiRefused::NotOnList);
        }
        let (key, per_minute) = match caller {
            ApiCaller::Owner => (format!("api|{}", chat.trim()), rt.control_commands_per_minute),
            ApiCaller::Mcp => (crate::mcp::MCP_CHAT.to_string(), crate::mcp::PER_MINUTE),
        };
        if !self.limits.allow_message(&key, per_minute) {
            return Err(ApiRefused::TooMany);
        }
        let w = words(locale::notify_lang());
        if caller == ApiCaller::Mcp {
            let out = self.mcp_text(&rt, text, w).await;
            return Ok(self.scrub(&rt, out));
        }
        let dummy = Inbound {
            channel_id: chat.to_string(),
            platform: "api".to_string(),
            chat_id: chat.to_string(),
            sender_id: sender.to_string(),
            body: Body::Text(text.to_string()),
            message_id: String::new(),
            reply_to_bot: true,
            direct: false,
        };
        let out = self
            .on_text(&rt, &key, &dummy, text, w)
            .await
            .unwrap_or_else(|| reply(&Outcome::Hint, w));
        Ok(self.scrub(&rt, out))
    }

    /// A command from an MCP client through the API: what MCP's tools allow and
    /// nothing more. Reading always; a call or stopping one only when MCP may
    /// make calls (`mcp_calls` confirm / direct, a call then waiting for the
    /// owner's yes unless direct, within `mcp_errands_per_hour`); a line for the
    /// AI on a call only with direct, typed as a command. Never the phone's
    /// mode, a to-do's decision, the contact lists, or a yes to a confirmation.
    async fn mcp_text(&self, rt: &RuntimeSettings, text: &str, w: &'static Words) -> Outbound {
        let text = text.trim();
        if text.is_empty() {
            return reply(&Outcome::Hint, w);
        }
        let typed = text.starts_with('/');
        let action = if typed {
            match command::parse_slash(text) {
                Parsed::Action(a) => a,
                Parsed::Usage(c) => return reply(&Outcome::Usage { command: c.to_string() }, w),
                Parsed::Unknown => return reply(&Outcome::Hint, w),
            }
        } else {
            match self.understand(rt, text).await {
                Some(a) => a,
                None => return reply(&Outcome::Hint, w),
            }
        };
        let action = match command::validate(action) {
            Ok(a) => a,
            Err(code) => return reply(&Outcome::Failed { code: code.to_string() }, w),
        };
        let mode = rt.mcp_calls.as_str();
        let calls = mode == "confirm" || mode == "direct";
        info!(action = action_kind(&action), mcp_calls = mode, "API command with the MCP token");
        let o = match action {
            a @ (Action::Help
            | Action::Status
            | Action::Calls(_)
            | Action::Call { .. }
            | Action::Tasks
            | Action::Errands
            | Action::Contacts { .. }) => self.perform(rt, crate::mcp::MCP_CHAT, a).await,
            Action::Errand { target, task } if calls => {
                self.request_errand(rt, crate::mcp::MCP_CHAT, &target, &task, mode == "direct", rt.mcp_errands_per_hour).await
            }
            a @ Action::Cancel { .. } if calls => self.perform(rt, crate::mcp::MCP_CHAT, a).await,
            a @ Action::Say { .. } if mode == "direct" && typed => self.perform(rt, crate::mcp::MCP_CHAT, a).await,
            _ => Outcome::Failed { code: "not_allowed".to_string() },
        };
        reply(&o, w)
    }

    async fn set_contact_kind(&self, kind: &str, number: &str, name: &str) -> Outcome {
        if kind == "unblock" {
            // Only the contacts: a Blacklist prefix with these digits goes too
            // (`/unblock 0800` lifts `0800*`).
            let found_name = self.state.contacts.lookup(number, None).map(|c| c.name).unwrap_or_default();
            let changed = self.state.contacts.unblock(number, &["blacklist", "stall"]).await.ok().flatten();
            return Outcome::ContactKindSet {
                list: "unblock".to_string(),
                number: number.to_string(),
                name: if found_name.is_empty() { changed.unwrap_or_default() } else { found_name },
            };
        }
        match self.state.contacts.upsert_by_number(number, name, "", kind).await {
            Ok(c) => Outcome::ContactKindSet {
                list: kind.to_string(),
                number: c.phones.first().cloned().unwrap_or_else(|| number.to_string()),
                name: c.name,
            },
            Err(_) => error_outcome("bad_number"),
        }
    }

    fn name_of(&self, number: &str, region: Option<&str>) -> String {
        if number.trim().is_empty() {
            return String::new();
        }
        self.state.contacts.lookup(number, region).map(|c| c.name).unwrap_or_default()
    }

    fn call_line(&self, c: &CallRecord) -> CallLine {
        CallLine {
            id: c.id.clone(),
            when: local_time(&c.started_at),
            who: self.name_of(&c.caller_id, c.sim_country.as_deref()),
            number: c.caller_id.trim().to_string(),
            summary: first_line(c.summary.as_deref().unwrap_or(""), 80),
        }
    }

    fn task_line(&self, t: &TaskRecord) -> TaskLine {
        TaskLine {
            id: t.id.clone(),
            who: self.name_of(&t.caller_id, None),
            number: t.caller_id.trim().to_string(),
            summary: first_line(&t.summary, 100),
        }
    }

    async fn status(&self, chat: &str) -> Outcome {
        let live = self.state.store.live_calls().await;
        let agent_calls: Vec<String> = live.iter().map(|c| c.serial.clone()).collect();
        let phones: Vec<PhoneLine> = self
            .state
            .phones
            .list_json(&agent_calls)
            .iter()
            .map(|p| {
                let serial = p["serial"].as_str().unwrap_or("").to_string();
                PhoneLine {
                    label: crate::notify::device_label(&serial),
                    connected: p["connected"].as_bool().unwrap_or(false),
                    mode: p["mode"].as_str().unwrap_or("ai").to_string(),
                    in_call: p["in_call"].as_bool().unwrap_or(false),
                    serial,
                }
            })
            .collect();
        self.mem(chat, |m| m.phones = phones.iter().map(|p| p.serial.clone()).collect());
        Outcome::Status { version: env!("CARGO_PKG_VERSION").to_string(), phones, live_calls: live.len() }
    }

    async fn calls(&self, chat: &str, q: &CallQuery) -> Outcome {
        let text = q.text.trim().to_string();
        let index = if text.is_empty() {
            None
        } else {
            Some(crate::calls_http::transcript_index(&self.state.task_store).await)
        };
        let filter = CallListFilter { q: (!text.is_empty()).then(|| text.clone()), ..Default::default() };
        let page = self.state.store.list_filtered(&filter, index.as_ref()).await;
        let day = q.day.map(|d| d.date(chrono::Local::now().date_naive()));
        let calls: Vec<CallLine> = page
            .calls
            .iter()
            .filter(|c| day.map_or(true, |d| local_day(&c.started_at) == Some(d)))
            .filter(|c| match q.only {
                Only::All => true,
                Only::Missed => missed(c),
                Only::Scam => scam(c),
            })
            .take(q.limit)
            .map(|c| self.call_line(c))
            .collect();
        self.mem(chat, |m| m.calls = calls.iter().map(|c| c.id.clone()).collect());
        Outcome::Calls { calls }
    }

    async fn call(&self, rt: &RuntimeSettings, chat: &str, r: &str) -> Outcome {
        let listed = self.mem(chat, |m| m.calls.clone());
        let known: Vec<String> = self.state.store.list().await.into_iter().map(|c| c.id).collect();
        let not_found = || Outcome::NotFound { what: r.to_string() };
        let Some(id) = pick_ref(&listed, &known, r) else {
            return not_found();
        };
        let Some(rec) = self.state.store.get(&id).await else {
            return not_found();
        };
        let notes = self.state.store.load_notes(&id).await;
        let summary = notes
            .as_ref()
            .map(|n| n.summary.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| rec.summary.clone())
            .unwrap_or_default();
        let transcript = notes.as_ref().map(|n| clip(n.dialogue.trim(), TRANSCRIPT_MAX_CHARS)).unwrap_or_default();
        let link = crate::notify::call_detail_url(&rt.effective(&self.state.settings), &id);
        let line = self.call_line(&rec);
        Outcome::Call {
            call: CallDetail {
                id,
                when: line.when,
                who: line.who,
                number: line.number,
                summary: clip(summary.trim(), SUMMARY_MAX_CHARS),
                transcript,
                link,
            },
        }
    }

    async fn pending_tasks(&self) -> Vec<TaskRecord> {
        self.state
            .task_store
            .list(200)
            .await
            .into_iter()
            .filter(|t| matches!(t.status, TaskStatus::Pending))
            .collect()
    }

    async fn tasks(&self, chat: &str) -> Outcome {
        let tasks: Vec<TaskLine> = self.pending_tasks().await.iter().take(LIST_MAX).map(|t| self.task_line(t)).collect();
        self.mem(chat, |m| m.tasks = tasks.iter().map(|t| t.id.clone()).collect());
        Outcome::Tasks { tasks }
    }

    fn errand_line(e: &Errand) -> ErrandLine {
        ErrandLine {
            id: e.id.clone(),
            who: e.name.trim().to_string(),
            number: e.number.trim().to_string(),
            status: e.status.clone(),
            state: errands::owner_status(e),
        }
    }

    fn errands(&self, chat: &str) -> Outcome {
        let errands: Vec<ErrandLine> = self.state.errands.list().iter().take(LIST_MAX).map(Self::errand_line).collect();
        self.mem(chat, |m| m.errands = errands.iter().map(|e| e.id.clone()).collect());
        Outcome::Errands { errands }
    }

    /// The errand a ref names; an empty ref: the only one matching when.
    fn errand_ref(&self, chat: &str, r: &str, when: impl Fn(&Errand) -> bool) -> Result<String, Outcome> {
        let all = self.state.errands.list();
        if r.trim().is_empty() {
            let hits: Vec<&Errand> = all.iter().filter(|e| when(e)).collect();
            return match hits.as_slice() {
                [one] => Ok(one.id.clone()),
                _ => Err(Outcome::Failed { code: "not_on_call".to_string() }),
            };
        }
        let listed = self.mem(chat, |m| m.errands.clone());
        let known: Vec<String> = all.into_iter().map(|e| e.id).collect();
        pick_ref(&listed, &known, r).ok_or_else(|| Outcome::NotFound { what: r.to_string() })
    }

    async fn cancel(&self, chat: &str, r: &str) -> Outcome {
        let id = match self.errand_ref(chat, r, |e| !e.is_final()) {
            Ok(id) => id,
            Err(o) => return o,
        };
        let now = errands::now_of(&self.state).await;
        match errands::cancel(&errands::env_of(&self.state, &now), &id).await {
            Ok(e) => {
                info!(errand = id.as_str(), "errand cancelled from a chat");
                Outcome::ErrandStopped { errand: Self::errand_line(&e) }
            }
            Err(code) => error_outcome(code),
        }
    }

    fn open_question(&self, chat: &str) -> Option<OpenQuestion> {
        self.mem(chat, |m| {
            if m.question.as_ref().is_some_and(|q| q.until <= Instant::now()) {
                m.question = None;
            }
            m.question.clone()
        })
    }

    /// The owner's answer to the AI's question (q: the one open in the chat),
    /// or to the AI on the line when none is open: the same as answering on
    /// the web console (errands::say); whichever comes first counts.
    fn answer_with(&self, chat: &str, q: Option<OpenQuestion>, text: &str) -> Outcome {
        let (errand, qid) = match q.or_else(|| self.open_question(chat)) {
            Some(q) => (q.errand, Some(q.id)),
            None => match self.errand_ref(chat, "", |e| e.status == errands::ON_CALL) {
                Ok(id) => (id, None),
                Err(o) => return o,
            },
        };
        match errands::say(&self.state.errands, &errand, text, qid.as_deref()) {
            Ok(()) => {
                if qid.is_some() {
                    self.mem(chat, |m| m.question = None);
                }
                info!(errand = errand.as_str(), "errand answered from a chat");
                Outcome::Answered
            }
            Err(code) => {
                if code == "question_closed" {
                    self.mem(chat, |m| m.question = None);
                }
                error_outcome(code)
            }
        }
    }

    fn contacts(&self, query: &str) -> Outcome {
        let contacts = self
            .state
            .contacts
            .list(query)
            .into_iter()
            .take(LIST_MAX)
            .map(|c| ContactLine { name: c.name, phones: c.phones })
            .collect();
        Outcome::Contacts { contacts }
    }

    /// The number or contact an errand is for: (errands::create body, who, number).
    fn errand_target(&self, target: &str) -> Result<(Value, String, String), Outcome> {
        if let Some(n) = errands::clean_number(target) {
            let who = crate::contacts::lookup(&n, None).map(|c| c.name).unwrap_or_default();
            return Ok((json!({ "number": n }), who, n));
        }
        let found = self.state.contacts.list(target);
        let exact: Vec<_> = found.iter().filter(|c| c.name.trim().eq_ignore_ascii_case(target.trim())).collect();
        let pick = match (exact.as_slice(), found.as_slice()) {
            ([one], _) => Some((*one).clone()),
            (_, [one]) => Some(one.clone()),
            _ => None,
        };
        match pick {
            Some(c) => {
                let number = c.phones.first().cloned().unwrap_or_default();
                if number.trim().is_empty() {
                    return Err(error_outcome("no_number"));
                }
                Ok((json!({ "contact_id": c.id }), c.name, number))
            }
            None if found.is_empty() => Err(Outcome::NoContact { what: target.to_string() }),
            None => Err(Outcome::ManyContacts { names: found.iter().take(5).map(|c| c.name.clone()).collect() }),
        }
    }

    /// The phone a ref names: its number in /status, its serial, or its name.
    fn phone_ref(&self, chat: &str, r: &str) -> Result<String, Outcome> {
        let serials: Vec<String> = self
            .state
            .phones
            .list_json(&[])
            .iter()
            .filter_map(|p| p["serial"].as_str().map(str::to_string))
            .collect();
        let listed = self.mem(chat, |m| m.phones.clone());
        if let Some(s) = pick_ref(&listed, &serials, r) {
            return Ok(s);
        }
        let low = r.trim().to_lowercase();
        let named = |exact: bool| -> Vec<String> {
            serials
                .iter()
                .filter(|s| {
                    let label = crate::notify::device_label(s).to_lowercase();
                    if exact { label == low } else { label.contains(&low) }
                })
                .cloned()
                .collect()
        };
        for exact in [true, false] {
            if let [one] = named(exact).as_slice() {
                return Ok(one.clone());
            }
        }
        Err(Outcome::NotFound { what: r.to_string() })
    }

    /// Resolve an action that needs a confirmation into its job and the
    /// summary the owner confirms; Err: why not (said at once).
    async fn prepare(&self, rt: &RuntimeSettings, chat: &str, action: Action, w: &Words) -> Result<(Job, String), Outcome> {
        match action {
            Action::Errand { target, task } => {
                let cap = rt.control_errands_per_hour.min(access::ERRANDS_PER_HOUR_MAX);
                if !self.limits.errand_room(chat, cap) {
                    return Err(Outcome::TooMany { limit: cap });
                }
                let (mut body, who, number) = self.errand_target(&target)?;
                // The same check errands::create makes, now: a number it will
                // refuse is said before the owner confirms it.
                let now = errands::now_of(&self.state).await;
                errands::pick_phone(&now.phones, &number, "", &errands::home_region(&now.rt)).map_err(error_outcome)?;
                body["task"] = json!(task);
                Ok((Job::Errand { body }, words::ask_errand(w, &who, &number, &task)))
            }
            Action::Mode { phone, mode } => {
                let serial = self.phone_ref(chat, &phone)?;
                let label = crate::notify::device_label(&serial);
                Ok((Job::Mode { serial, mode }, words::ask_mode(w, &label, mode.as_str())))
            }
            Action::Decide { r, confirm } => {
                let pending = self.pending_tasks().await;
                let known: Vec<String> = pending.iter().map(|t| t.id.clone()).collect();
                let listed = self.mem(chat, |m| m.tasks.clone());
                let task = pick_ref(&listed, &known, &r)
                    .and_then(|id| pending.iter().find(|t| t.id == id))
                    .ok_or_else(|| Outcome::NotFound { what: r.clone() })?;
                let line = self.task_line(task);
                let what = format!("{} · {}", words::person(w, &line.who, &line.number), line.summary);
                let ask = if confirm { w.ask_confirm } else { w.ask_ignore };
                Ok((Job::Decide { task_id: line.id, confirm }, words::fmt(ask, &[&what])))
            }
            Action::Say { r, text } => {
                let summary = words::fmt(w.ask_say, &[&text]);
                Ok((Job::Act { action: Action::Say { r, text } }, summary))
            }
            Action::Answer { text } => {
                let summary = words::fmt(w.ask_answer, &[&text]);
                Ok((Job::Act { action: Action::Answer { text } }, summary))
            }
            _ => Err(Outcome::Failed { code: "not_confirmable".to_string() }),
        }
    }

    /// Run a confirmed job.
    async fn execute(&self, rt: &RuntimeSettings, chat: &str, job: Job) -> Outcome {
        let st = &self.state;
        match job {
            Job::Act { action } => self.perform(rt, chat, action).await,
            Job::Errand { body } => {
                let cap = rt.control_errands_per_hour.min(access::ERRANDS_PER_HOUR_MAX);
                if !self.limits.errand_room(chat, cap) {
                    return Outcome::TooMany { limit: cap };
                }
                let now = errands::now_of(st).await;
                match errands::create(&errands::env_of(st, &now), &body).await {
                    Ok(e) => {
                        self.limits.note_errand(chat);
                        self.mem(chat, |m| m.errands = vec![e.id.clone()]);
                        info!(errand = e.id.as_str(), chat, "errand created from a chat");
                        Outcome::ErrandStarted { errand: Self::errand_line(&e) }
                    }
                    Err(code) => error_outcome(code),
                }
            }
            Job::Mode { serial, mode } => {
                let agent_call = st.store.live_calls().await.iter().any(|c| c.serial == serial);
                let r = st.phones.set_mode(&serial, mode, agent_call).await;
                info!(serial = serial.as_str(), target = mode.as_str(), ok = r.ok, error = r.error.as_str(), "phone mode switch from a chat");
                let phone = crate::notify::device_label(&serial);
                if r.ok {
                    Outcome::ModeSet { phone, mode: r.mode.unwrap_or(mode).as_str().to_string() }
                } else {
                    Outcome::ModeFailed { phone, reason: r.error }
                }
            }
            Job::Decide { task_id, confirm } => {
                // The web console (or a card) may have decided it while the owner
                // was asked here: the first decision stands.
                match st.task_store.get(&task_id).await {
                    Some(t) if matches!(t.status, crate::tasks::TaskStatus::Pending) => {}
                    Some(_) => return Outcome::Failed { code: "already_decided".to_string() },
                    None => return Outcome::NotFound { what: task_id },
                }
                let appointment = st.task_store.get(&task_id).await.is_some_and(|t| t.appointment.is_some());
                let done = if appointment {
                    // The console address the web API passes (the calendar link of a
                    // confirmed appointment opens the call's page there).
                    let console = crate::settings_store::effective_console_url(&rt.effective(&st.settings));
                    crate::appointments::decide_from_web(
                        &st.http,
                        rt,
                        &console,
                        &st.task_store,
                        &st.store,
                        Some(st.control.as_ref()),
                        &task_id,
                        confirm,
                        None,
                    )
                    .await
                    .map(|(_, text)| text)
                } else if confirm {
                    crate::fulfill::on_confirm(&st.http, rt, &st.task_store, &task_id, Some(st.control.as_ref()))
                        .await
                        .map(|t| crate::fulfill::tg_result_text(&t, true))
                } else {
                    crate::fulfill::on_ignore(&st.task_store, &task_id)
                        .await
                        .map(|t| crate::fulfill::tg_result_text(&t, false))
                };
                match done {
                    Ok(text) => {
                        info!(task = task_id.as_str(), confirm, "to-do decided from a chat");
                        Outcome::Decided { task: task_id, text }
                    }
                    Err(e) => {
                        warn!(task = task_id.as_str(), "to-do decision from a chat failed: {e:#}");
                        Outcome::Failed { code: "fulfill_failed".to_string() }
                    }
                }
            }
        }
    }

    /// Every key and token the agent holds (8 characters or more).
    fn secrets(&self, rt: &RuntimeSettings) -> Vec<String> {
        let mut secrets: Vec<String> = vec![
            rt.telegram_bot_token.clone(),
            rt.gemini_api_key.clone(),
            rt.openrouter_api_key.clone(),
            rt.deepseek_api_key.clone(),
            rt.gmail_app_password.clone(),
            rt.openclaw_token.clone(),
            rt.mcp_token.clone(),
            self.state.settings.calls_ui_token.clone(),
            self.state.settings.device_token.clone(),
        ];
        for c in &rt.notify_channels {
            secrets.push(c.token.clone());
            secrets.push(c.url.clone());
            secrets.push(c.app_token.clone());
        }
        secrets.into_iter().map(|s| s.trim().to_string()).filter(|s| s.chars().count() >= 8).collect()
    }

    /// Cut every key and token the agent holds out of a reply.
    fn scrub(&self, rt: &RuntimeSettings, mut out: Outbound) -> Outbound {
        let mut text = locale::for_owner(&out.text);
        for s in self.secrets(rt) {
            // Also as written in a URL (a request error names its address).
            for form in secret_forms(&s) {
                text = text.replace(&form, "•••");
                for b in out.buttons.iter_mut() {
                    b.label = b.label.replace(&form, "•••");
                }
            }
        }
        out.text = clip(&text, OUT_MAX_CHARS);
        out
    }

    /// An outcome as JSON for an MCP client ([crate::mcp]), every key and
    /// token cut out of it as from a chat reply.
    pub fn outcome_json(&self, rt: &RuntimeSettings, o: &Outcome) -> Value {
        let mut raw = serde_json::to_string(o).unwrap_or_else(|_| "{}".to_string());
        for s in self.secrets(rt) {
            for form in secret_forms(&s) {
                // As JSON writes it (a quote or backslash in a secret is escaped).
                let written = serde_json::to_string(&form).unwrap_or_default();
                let inner = written.trim_matches('"');
                if !inner.is_empty() {
                    raw = raw.replace(inner, "•••");
                }
            }
        }
        serde_json::from_str(&raw).unwrap_or_else(|_| json!({ "kind": "failed", "code": "scrubbed" }))
    }

    /// Chats that take commands and that a running adapter reaches: a
    /// Telegram channel without a bot token gets nothing, and no one may be
    /// left waiting for an answer from there.
    fn reached_chats(&self, rt: &RuntimeSettings) -> Vec<(String, String)> {
        let reached: Vec<(String, String)> = access::telegram_bots(rt).into_iter().flat_map(|b| b.chats).collect();
        let is_telegram = |id: &str| id == ENV_TELEGRAM || rt.notify_channels.iter().any(|c| c.id == id && c.kind == "telegram");
        access::command_chats(rt)
            .into_iter()
            .filter(|c| !is_telegram(&c.0) || reached.contains(c))
            .collect()
    }

    /// Room for one more message from this caller this minute (an MCP
    /// client counts as one chat, [crate::mcp::MCP_CHAT]).
    pub fn allow(&self, chat: &str, per_minute: u32) -> bool {
        self.limits.allow_message(chat, per_minute)
    }

    /// An AI call an MCP client asks for ([crate::mcp]). origin: the chat key
    /// its list numbers and limits are kept under; cap: requests per hour.
    /// direct: queued at once through errands::create. Otherwise the owner
    /// gets the summary with yes / no in every command chat an adapter
    /// reaches; the first answer counts, and the call is made only on a yes
    /// (then within that chat's own hourly limit too).
    pub async fn request_errand(&self, rt: &RuntimeSettings, origin: &str, target: &str, task: &str, direct: bool, cap: u32) -> Outcome {
        let (target, task) = match command::validate(Action::Errand { target: target.to_string(), task: task.to_string() }) {
            Ok(Action::Errand { target, task }) => (target, task),
            Ok(_) => return Outcome::Failed { code: "bad_request".to_string() },
            Err(code) => return Outcome::Failed { code: code.to_string() },
        };
        let cap = cap.min(access::ERRANDS_PER_HOUR_MAX);
        if !self.limits.errand_room(origin, cap) {
            return Outcome::TooMany { limit: cap };
        }
        let (mut body, who, number) = match self.errand_target(&target) {
            Ok(t) => t,
            Err(o) => return o,
        };
        let now = errands::now_of(&self.state).await;
        if let Err(code) = errands::pick_phone(&now.phones, &number, "", &errands::home_region(&now.rt)) {
            return error_outcome(code);
        }
        body["task"] = json!(task);
        if direct {
            return match errands::create(&errands::env_of(&self.state, &now), &body).await {
                Ok(e) => {
                    self.limits.note_errand(origin);
                    self.mem(origin, |m| m.errands = vec![e.id.clone()]);
                    info!(errand = e.id.as_str(), "errand created from MCP");
                    Outcome::ErrandStarted { errand: Self::errand_line(&e) }
                }
                Err(code) => error_outcome(code),
            };
        }
        let chats = self.reached_chats(rt);
        if chats.is_empty() || self.pushes.receiver_count() == 0 {
            return Outcome::Failed { code: "no_owner_chat".to_string() };
        }
        // Each request counts, answered or not: a client cannot flood the
        // owner's chats with cards.
        self.limits.note_errand(origin);
        let w = words(locale::notify_lang());
        let summary = words::ask_errand(w, &who, &number, &task);
        let keys: Vec<String> = chats.iter().map(|(c, i)| access::chat_key(c, i)).collect();
        let code = self.confirms.ask_many(&keys, confirm::ANY_SENDER, Job::Errand { body });
        let confirm = Outcome::Confirm { code: code.clone(), summary };
        let card = Outbound {
            text: format!("MCP · {}", words::render(&confirm, w)),
            buttons: vec![
                Button { label: w.yes.to_string(), data: format!("k:{code}:y") },
                Button { label: w.no.to_string(), data: format!("k:{code}:n") },
            ],
            edit: None,
        };
        for (channel_id, chat_id) in chats {
            let out = self.scrub(rt, card.clone());
            let _ = self.pushes.send(Push { channel_id, chat_id, out });
        }
        info!(code = code.as_str(), "errand from MCP waits for the owner's yes");
        confirm
    }

    /// What errands::ErrandStore calls when the AI on a call asks the owner
    /// something: the question goes to every chat that takes commands, and
    /// the AI waits for an answer from there as it does for the web console.
    pub fn question_hook(self: &Arc<Self>) -> errands::QuestionHook {
        let me = Arc::downgrade(self);
        Arc::new(move |q: &errands::OwnerQuestion| me.upgrade().is_some_and(|c| c.push_question(q)))
    }

    /// True when the question went out to at least one chat.
    fn push_question(&self, q: &errands::OwnerQuestion) -> bool {
        let rt = self.settings();
        let chats = self.reached_chats(&rt);
        if chats.is_empty() || self.pushes.receiver_count() == 0 {
            return false;
        }
        let w = words(locale::notify_lang());
        let text = format!(
            "{}\n\n{}",
            words::fmt(w.question, &[&q.who, &q.text]),
            words::fmt(w.question_how, &[&q.wait.as_secs().to_string()])
        );
        let buttons: Vec<Button> = q
            .options
            .iter()
            .enumerate()
            .map(|(i, o)| Button { label: o.clone(), data: format!("q:{}:{}:{i}", q.errand_id, q.question_id) })
            .filter(|b| b.data.len() <= BUTTON_DATA_MAX)
            .collect();
        let until = Instant::now() + q.wait + QUESTION_GRACE;
        let mut sent = false;
        for (channel_id, chat_id) in chats {
            let key = access::chat_key(&channel_id, &chat_id);
            self.mem(&key, |m| {
                m.question = Some(OpenQuestion {
                    errand: q.errand_id.clone(),
                    id: q.question_id.clone(),
                    options: q.options.clone(),
                    until,
                })
            });
            let out = self.scrub(&rt, Outbound { text: text.clone(), buttons: buttons.clone(), edit: None });
            sent |= self.pushes.send(Push { channel_id, chat_id, out }).is_ok();
        }
        info!(errand = q.errand_id.as_str(), question = q.question_id.as_str(), sent, "errand question sent to the command chats");
        sent
    }
}

