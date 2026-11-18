// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Who may operate the agent from a chat, and how much.
//!
//! Off unless the owner switches it on for a chat: per notification channel
//! (its "commands" switch and its chat as the channel's target), or for the
//! environment's Telegram bot (setting telegram_control). A list of sender ids
//! narrows a chat down to some of its members. A group (any chat that is not
//! known to be one-to-one with the bot) needs that list: without it nobody
//! there may give commands ([Verdict::NeedsList]). A private chat needs none.
//! Anything else gets no answer at all.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::settings_store::RuntimeSettings;

use super::{Inbound, COMMAND_KINDS, ENV_TELEGRAM};

/// A chat that may give commands: the key its state is kept under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub chat: String,
}

pub fn chat_key(channel_id: &str, chat_id: &str) -> String {
    format!("{}|{}", channel_id.trim(), chat_id.trim())
}

/// What [check] decided for one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow(Grant),
    /// The chat's own, switched on, but a group with no list of who may give
    /// commands: nobody may (the owner is told how to add their id).
    NeedsList,
    Deny,
}

/// A chat known to be one-to-one with the bot: a Telegram private chat (a
/// user's id, positive; groups and channels are negative or @names) or a
/// Slack direct message (D…). Discord and Matrix do not tell it by the chat's
/// id: their messages say it themselves ([Inbound::direct]).
pub fn direct_chat(kind: &str, chat: &str) -> bool {
    let chat = chat.trim();
    match kind {
        "telegram" => chat.parse::<i64>().is_ok_and(|id| id > 0),
        "slack" => chat.starts_with('D'),
        _ => false,
    }
}

/// Whether this sender in this chat may give commands. None: not allowed (the
/// switch is off, another chat, a sender not on the list, an unknown channel).
pub fn authorize(rt: &RuntimeSettings, m: &Inbound) -> Option<Grant> {
    match check(rt, m) {
        Verdict::Allow(g) => Some(g),
        Verdict::NeedsList | Verdict::Deny => None,
    }
}

/// [authorize], telling apart a group that has no list yet.
pub fn check(rt: &RuntimeSettings, m: &Inbound) -> Verdict {
    let (on, kind, chat, token, users) = if m.channel_id == ENV_TELEGRAM {
        (
            rt.telegram_control && !rt.telegram_bot_token.trim().is_empty(),
            "telegram",
            rt.telegram_chat_id.trim(),
            rt.telegram_bot_token.trim(),
            &rt.telegram_control_users,
        )
    } else {
        let Some(c) = rt.notify_channels.iter().find(|c| !c.id.is_empty() && c.id == m.channel_id) else {
            return Verdict::Deny;
        };
        (
            c.enabled && c.control && COMMAND_KINDS.contains(&c.kind.as_str()),
            c.kind.as_str(),
            c.target.trim(),
            c.token.trim(),
            &c.control_users,
        )
    };
    if !on || kind != m.platform || chat.is_empty() || m.chat_id.trim() != chat {
        return Verdict::Deny;
    }
    // Every list on the same bot and chat applies: one entry switched on
    // without a list must not undo another one's list.
    let sender = m.sender_id.trim();
    let mut lists = sender_lists(rt, kind, token, chat);
    lists.push(users);
    if !lists.iter().all(|l| l.is_empty() || (!sender.is_empty() && l.iter().any(|u| u.trim() == sender))) {
        return Verdict::Deny;
    }
    if lists.iter().all(|l| l.is_empty()) && !direct_chat(kind, chat) && !m.direct {
        return Verdict::NeedsList;
    }
    Verdict::Allow(Grant { chat: chat_key(&m.channel_id, chat) })
}

/// A command sent through the API (`/calls/api/control/inbound`) naming a
/// chat (`channel_id`: the channel's id, name or kind, or the environment's
/// Telegram bot): every sender list of what it names applies to its sender.
pub fn api_sender_ok(rt: &RuntimeSettings, channel: &str, sender: &str) -> bool {
    let channel = channel.trim();
    let sender = sender.trim();
    let mut lists: Vec<&Vec<String>> = Vec::new();
    if channel == ENV_TELEGRAM {
        lists.push(&rt.telegram_control_users);
    }
    if !channel.is_empty() {
        let enabled: Vec<_> = rt.notify_channels.iter().filter(|c| c.enabled).collect();
        let picked: Vec<_> = {
            let by_id: Vec<_> = enabled.iter().copied().filter(|c| c.id == channel).collect();
            if !by_id.is_empty() {
                by_id
            } else {
                let by_name: Vec<_> = enabled.iter().copied().filter(|c| c.name.eq_ignore_ascii_case(channel)).collect();
                if !by_name.is_empty() {
                    by_name
                } else {
                    enabled.iter().copied().filter(|c| c.kind.eq_ignore_ascii_case(channel)).collect()
                }
            }
        };
        for c in picked {
            lists.push(&c.control_users);
        }
    }
    lists.iter().all(|l| l.is_empty() || (!sender.is_empty() && l.iter().any(|u| u.trim() == sender)))
}

/// The sender lists of every entry switched on for one bot and chat: the
/// environment's Telegram bot and each channel with the same kind, token and target.
pub fn sender_lists<'a>(rt: &'a RuntimeSettings, kind: &str, token: &str, chat: &str) -> Vec<&'a Vec<String>> {
    let mut out = Vec::new();
    if token.is_empty() || chat.is_empty() {
        return out;
    }
    if kind == "telegram" && rt.telegram_control && rt.telegram_bot_token.trim() == token && rt.telegram_chat_id.trim() == chat {
        out.push(&rt.telegram_control_users);
    }
    for c in &rt.notify_channels {
        if c.enabled && c.control && c.kind == kind && c.token.trim() == token && c.target.trim() == chat {
            out.push(&c.control_users);
        }
    }
    out
}

/// The chats that take commands now: (channel id, chat id). Questions from the
/// AI during a call go to these. A chat reached twice through one bot (set
/// up as a channel and in the environment) is listed once.
pub fn command_chats(rt: &RuntimeSettings) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();
    if rt.telegram_control && !rt.telegram_bot_token.trim().is_empty() && !rt.telegram_chat_id.trim().is_empty() {
        out.push((ENV_TELEGRAM.to_string(), rt.telegram_chat_id.trim().to_string()));
        seen.push((rt.telegram_bot_token.trim().to_string(), rt.telegram_chat_id.trim().to_string()));
    }
    for c in &rt.notify_channels {
        if c.enabled && c.control && COMMAND_KINDS.contains(&c.kind.as_str()) && !c.target.trim().is_empty() && !c.id.is_empty() {
            let reach = (c.token.trim().to_string(), c.target.trim().to_string());
            if seen.contains(&reach) {
                continue;
            }
            seen.push(reach);
            out.push((c.id.clone(), c.target.trim().to_string()));
        }
    }
    out
}

/// One Telegram bot to long-poll and the chats it serves: (channel id, chat id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramBot {
    pub token: String,
    /// The environment's bot: it also carries the to-do cards' buttons.
    pub env: bool,
    /// At least one of its chats takes commands (its command menu is set).
    pub commands: bool,
    pub chats: Vec<(String, String)>,
    /// The environment's bot: to-do card buttons count only from a sender on
    /// each of these lists (empty: anyone in its chat).
    pub card_lists: Vec<Vec<String>>,
}

/// The bots to poll: the environment's whenever it is set (its to-do cards
/// have buttons), and the token of every Telegram channel that takes commands.
/// A token used twice is polled once (two pollers on one bot fight over its
/// updates). Chats that take commands come first, so a chat set up both ways
/// is the channel's when only the channel has commands on.
pub fn telegram_bots(rt: &RuntimeSettings) -> Vec<TelegramBot> {
    let mut bots: Vec<TelegramBot> = Vec::new();
    let mut add = |token: &str, env: bool, chat: (String, String), commands: bool| {
        let token = token.trim();
        if token.is_empty() || chat.1.is_empty() {
            return;
        }
        match bots.iter_mut().find(|b| b.token == token) {
            Some(b) => {
                b.env |= env;
                b.commands |= commands;
                if !b.chats.contains(&chat) {
                    if commands {
                        b.chats.insert(0, chat);
                    } else {
                        b.chats.push(chat);
                    }
                }
            }
            None => bots.push(TelegramBot { token: token.to_string(), env, commands, chats: vec![chat], card_lists: Vec::new() }),
        }
    };
    for c in &rt.notify_channels {
        if c.kind == "telegram" && c.enabled && c.control && !c.id.is_empty() {
            add(&c.token, false, (c.id.clone(), c.target.trim().to_string()), true);
        }
    }
    add(
        &rt.telegram_bot_token,
        true,
        (ENV_TELEGRAM.to_string(), rt.telegram_chat_id.trim().to_string()),
        rt.telegram_control,
    );
    // The card buttons of the environment's bot follow the sender lists of its
    // chat (its own, also when commands are off, and the same chat's channels).
    let env_chat = rt.telegram_chat_id.trim();
    if let Some(b) = bots.iter_mut().find(|b| b.env) {
        let mut lists: Vec<Vec<String>> = Vec::new();
        if !rt.telegram_control_users.is_empty() {
            lists.push(rt.telegram_control_users.clone());
        }
        for l in sender_lists(rt, "telegram", &b.token, env_chat) {
            if !l.is_empty() && !lists.contains(l) {
                lists.push(l.clone());
            }
        }
        b.card_lists = lists;
    }
    bots
}

/// Upper bounds of the two limits (settings above them are read as these).
pub const ERRANDS_PER_HOUR_MAX: u32 = 30;
pub const MESSAGES_PER_MINUTE_MAX: u32 = 120;

#[derive(Default)]
struct Window {
    messages: VecDeque<Instant>,
    errands: VecDeque<Instant>,
}

fn prune(q: &mut VecDeque<Instant>, span: Duration) {
    while q.front().is_some_and(|t| t.elapsed() >= span) {
        q.pop_front();
    }
}

/// Per-chat counts over a sliding window: messages per minute (past the
/// limit the chat gets no answers until it slows down) and AI calls per hour.
#[derive(Default)]
pub struct Limits {
    chats: Mutex<HashMap<String, Window>>,
}

impl Limits {
    fn with<R>(&self, chat: &str, f: impl FnOnce(&mut Window) -> R) -> R {
        let mut all = self.chats.lock().unwrap_or_else(|e| e.into_inner());
        f(all.entry(chat.to_string()).or_default())
    }

    /// Counts one message; false when the chat is over its per-minute limit.
    pub fn allow_message(&self, chat: &str, per_minute: u32) -> bool {
        let cap = per_minute.clamp(1, MESSAGES_PER_MINUTE_MAX) as usize;
        self.with(chat, |w| {
            prune(&mut w.messages, Duration::from_secs(60));
            if w.messages.len() >= cap {
                return false;
            }
            w.messages.push_back(Instant::now());
            true
        })
    }

    /// Room for one more AI call from this chat this hour.
    pub fn errand_room(&self, chat: &str, per_hour: u32) -> bool {
        let cap = per_hour.min(ERRANDS_PER_HOUR_MAX) as usize;
        self.with(chat, |w| {
            prune(&mut w.errands, Duration::from_secs(3600));
            w.errands.len() < cap
        })
    }

    /// An AI call from this chat went out.
    pub fn note_errand(&self, chat: &str) {
        self.with(chat, |w| w.errands.push_back(Instant::now()));
    }
}

