// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The Telegram adapter: long-polls every bot that serves a chat
//! ([access::telegram_bots]), hands messages and button presses to the
//! [Controller] and sends its answers and pushes back. The environment's bot
//! also carries the to-do cards: their buttons go to the card handler in
//! main.rs, as before.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Client;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::settings_store::SettingsStore;
use crate::tg_inbound::{self, TgAction, TgIncoming, TgPoller};

use super::access::{self, TelegramBot};
use super::{Body, Controller, Inbound, Push, ENV_TELEGRAM};

/// How often the bots to poll are read from the settings.
const RECHECK: Duration = Duration::from_secs(10);

/// A task that stops with its owner (a bot's pusher with its poller).
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Poll the bots the settings name, start and stop them as settings change.
/// cards: the to-do card buttons of the environment's bot.
pub fn spawn(ctrl: Arc<Controller>, store: Arc<SettingsStore>, http: Client, cards: mpsc::UnboundedSender<TgAction>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut running: HashMap<String, (TelegramBot, AbortOnDrop)> = HashMap::new();
        loop {
            let bots = access::telegram_bots(&store.snapshot());
            running.retain(|token, (bot, _)| bots.iter().any(|b| b.token == *token && b == bot));
            for bot in bots {
                if running.contains_key(&bot.token) {
                    continue;
                }
                info!(chats = bot.chats.len(), env = bot.env, commands = bot.commands, "telegram bot: polling");
                let task = tokio::spawn(run_bot(ctrl.clone(), http.clone(), tg_inbound::TELEGRAM_API.to_string(), bot.clone(), cards.clone()));
                running.insert(bot.token.clone(), (bot, AbortOnDrop(task)));
            }
            tokio::time::sleep(RECHECK).await;
        }
    })
}

/// The Inbound of a Telegram message or button press in one of the bot's chats.
pub fn inbound(bot: &TelegramBot, inc: &TgIncoming) -> Option<Inbound> {
    let channel_id = bot.chats.iter().find(|(_, chat)| *chat == inc.chat_id).map(|(id, _)| id.clone())?;
    let body = match (&inc.button, &inc.text) {
        (Some(data), _) => Body::Button(data.clone()),
        (None, Some(text)) => Body::Text(text.clone()),
        (None, None) => return None,
    };
    Some(Inbound {
        channel_id,
        platform: "telegram".to_string(),
        chat_id: inc.chat_id.clone(),
        sender_id: inc.sender_id.clone(),
        body,
        message_id: if inc.message_id > 0 { inc.message_id.to_string() } else { String::new() },
        reply_to_bot: inc.reply_to_bot,
        direct: false,
    })
}

/// One bot: poll, answer, send the pushes for its chats. Runs until aborted.
pub async fn run_bot(ctrl: Arc<Controller>, http: Client, api: String, bot: TelegramBot, cards: mpsc::UnboundedSender<TgAction>) {
    let _pushes = AbortOnDrop(tokio::spawn(send_pushes(ctrl.subscribe(), http.clone(), api.clone(), bot.clone())));
    let mut poller = TgPoller::with_api(&api);
    if bot.env {
        if let Some((_, chat)) = bot.chats.iter().find(|(id, _)| id == ENV_TELEGRAM) {
            poller.set_allowed_chat_id(chat);
        }
        poller.set_card_senders(bot.card_lists.clone());
    }
    // The menu goes only to this bot's chats that take commands now.
    let rt = ctrl.settings();
    let takes_commands = |id: &str| {
        (id == ENV_TELEGRAM && rt.telegram_control) || rt.notify_channels.iter().any(|c| c.id == id && c.enabled && c.control)
    };
    let command_chats: Vec<String> = bot.chats.iter().filter(|(id, _)| takes_commands(id)).map(|(_, chat)| chat.clone()).collect();
    let menu = if bot.commands && !command_chats.is_empty() {
        super::words::menu(super::words::words(crate::locale::notify_lang()))
    } else {
        Vec::new()
    };
    let all_chats: Vec<String> = bot.chats.iter().map(|(_, chat)| chat.clone()).collect();
    tg_inbound::set_my_commands(&http, &api, &bot.token, &menu, if menu.is_empty() { &all_chats } else { &command_chats }).await;
    loop {
        let polled = match poller.poll_once(&http, &bot.token).await {
            Ok(p) => p,
            Err(e) => {
                warn!("telegram poll failed: {}", tg_inbound::redact(&format!("{e:#}"), &bot.token));
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        for action in polled.actions {
            if bot.env {
                let _ = cards.send(action);
            }
        }
        for inc in polled.incoming {
            let Some(msg) = inbound(&bot, &inc) else {
                warn!(chat = inc.chat_id.as_str(), sender = inc.sender_id.as_str(), "telegram message from a chat this bot does not serve: no answer");
                continue;
            };
            let (ctrl, http, api, token) = (ctrl.clone(), http.clone(), api.clone(), bot.token.clone());
            // Its own task: a phone mode switch waits for the phone, and the
            // next message must not wait for that.
            tokio::spawn(async move {
                if let Some(out) = ctrl.handle(&msg).await {
                    if let Err(e) = tg_inbound::send_outbound(&http, &api, &token, &msg.chat_id, &out).await {
                        warn!("telegram reply failed: {}", tg_inbound::redact(&format!("{e:#}"), &token));
                    }
                }
            });
        }
    }
}

async fn send_pushes(mut rx: broadcast::Receiver<Push>, http: Client, api: String, bot: TelegramBot) {
    loop {
        match rx.recv().await {
            Ok(p) => {
                if !bot.chats.iter().any(|(id, chat)| *id == p.channel_id && *chat == p.chat_id) {
                    continue;
                }
                if let Err(e) = tg_inbound::send_outbound(&http, &api, &bot.token, &p.chat_id, &p.out).await {
                    warn!("telegram push failed: {}", tg_inbound::redact(&format!("{e:#}"), &bot.token));
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => warn!(skipped = n, "telegram pushes fell behind"),
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

