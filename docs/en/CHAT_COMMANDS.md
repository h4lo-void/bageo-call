# Chat commands: run Bageo from Telegram, Discord, Slack or Matrix

Away from the console, you can run Bageo from the chat your notifications go to:

- Look up calls.
- Have the AI call someone.
- Decide pending items.
- Answer the AI's questions during its call.
- Switch a phone between AI answering and normal phone.

Telegram, Discord, Slack and Matrix are supported. Other chat apps and AI agents
can be added later (see [For developers](#for-developers)).

## Turn it on

Chat commands are **off** until you switch them on for a chat. Follow the steps
for your chat app: set up its notification channel, then turn on commands.

### Telegram

1. Set up the Telegram channel first ([Notifications → Telegram](NOTIFICATIONS.md#telegram)).
2. *Settings → Notifications* → the Telegram channel → **Take commands from this chat**.
3. In **Who may give commands**, enter Telegram user IDs (ask
   **@userinfobot** for yours). In a group this is required: with the list
   empty, nobody there can give commands. In a private chat with the bot you
   can leave it empty.
4. **Save changes**, then send `/help` to the bot.

For the bot set by environment variables (`TELEGRAM_BOT_TOKEN` + `TELEGRAM_CHAT_ID`):

- It has its own switch under *Settings → Notifications* → "Commands from chat", or set `TELEGRAM_CONTROL=true`.
- To say who may give commands, set `TELEGRAM_CONTROL_USERS=123456789,987654321`. A group needs it; a private chat does not.

In a group the bot only reacts to commands (`/calls`, or `/calls@your_bot`)
and to replies to its own messages. Everything else people say is ignored.

### Discord

1. Open <https://discord.com/developers/applications>, press *New Application*, then open *Bot*.
2. Under *Bot*, press *Reset Token* and copy the token.
3. On the same page, turn on **Message Content Intent**. Without it the bot cannot read what you type.
4. Invite the bot: *OAuth2 → URL Generator*, tick *bot*, then tick *View Channels*, *Send Messages* and *Read Message History*. Open the URL it makes and pick your server.
5. For a private channel, also give the bot permission to read and write in that channel.
6. Set up the Discord channel ([Notifications → Discord](NOTIFICATIONS.md#discord)). Fill in *Bot token* and *Channel ID*.
7. To get the channel ID: turn on *User Settings → Advanced → Developer Mode*, right-click the channel → *Copy Channel ID*.
8. Turn on **Take commands from this chat**.
9. In **Who may give commands**, enter your Discord user ID (Developer Mode on, right-click your name → *Copy User ID*). For a channel in a server this is required: with the list empty, nobody there can give commands. In a private message with the bot you can leave it empty.
10. **Save changes**, then send `/help` in the channel.

### Slack

Commands work with Slack only, not Mattermost.

1. Create an app at <https://api.slack.com/apps> (*Create New App → From scratch*).
2. Turn on *Socket Mode* and create an app-level token with the `connections:write` scope. It starts with `xapp-`; copy it.
3. Open *Event Subscriptions*, turn it on, and under *Subscribe to bot events* add `message.channels` and `message.im`.
4. Open *Interactivity & Shortcuts* and turn on *Interactivity*, so the buttons work.
5. Open *OAuth & Permissions* and under *Bot Token Scopes* add `chat:write`, `channels:history` and `im:history`.
6. Press *Install to Workspace* and copy the bot token (it starts with `xoxb-`).
7. Add the app to the channel: in the channel, *Integrations → Add apps*.
8. Set up the Slack channel ([Notifications → Slack / Mattermost](NOTIFICATIONS.md#slack--mattermost)). Fill in *Bot token*, *App-level token (for chat commands)* and *Channel ID*.
9. To get the channel ID: open the channel's details; it is at the bottom and starts with `C`.
10. Turn on **Take commands from this chat**.
11. In **Who may give commands**, enter your Slack member ID (open your profile → ⋮ → *Copy member ID*). For a channel this is required: with the list empty, nobody there can give commands. In a direct message with the bot you can leave it empty.
12. **Save changes**, then send `@your bot /help`. Slack takes a message that starts with `/` for its own commands, so start with the bot's name.

### Matrix

Only unencrypted rooms work. Element makes direct messages encrypted by default, so create a separate room instead.

1. Create a Matrix account just for the bot.
2. Log in to Element with it and copy the token: *All settings → Help & About → Advanced → Access Token*. Treat it like a password.
3. With your own account, create a new room and turn off *Enable end-to-end encryption*. It cannot be turned off later.
4. Invite the bot account to the room, then join with it.
5. Find the room ID in *Room settings → Advanced* (it starts with `!`). The room's address (`#name:example.org`) works too.
6. Set up the Matrix channel ([Notifications → Matrix](NOTIFICATIONS.md#matrix)). Fill in *Homeserver URL*, *Access token* and *Room ID*.
7. Turn on **Take commands from this chat**.
8. In **Who may give commands**, enter your Matrix ID, for example `@lee:example.org` (shown in your Element profile). For a room with other people this is required: with the list empty, nobody there can give commands. A room with only you and the bot can stay empty.
9. **Save changes**, then send `@your bot /help`. Element takes a message that starts with `/` for its own commands, so start with the bot's name.

Matrix has no buttons. To confirm something, reply **1** or **2**.

## Commands

In a Discord or Slack channel, and in a Matrix room with more than two people, the bot only reacts to commands that start with `/`, to messages that mention it, and to replies to its own messages. In Slack and Element, write `@your bot /calls`.

Numbers like `1` or `#2` refer to the list the chat was shown last
(`/calls`, `/tasks`, `/errands`, `/status`).

| Command | What it does |
|---|---|
| `/help` | The commands |
| `/status` | Bageo version, each phone (AI answers / normal phone / offline / in a call), calls in progress |
| `/calls [n] [today\|yesterday\|YYYY-MM-DD] [missed\|scam] [word or number]` | Recent calls, one line each: time, caller, first line of the summary. 5 by default, up to 20 |
| `/call <#>` | One call: summary, the start of the transcript (600 characters at most), link to the console |
| `/tasks` | Pending items waiting for you (appointments, messages) |
| `/confirm <#>` · `/ignore <#>` | Decide a pending item, as on the console |
| `/errand <number or contact> <what to get done>` | The AI calls for you, e.g. `/errand 0912345678 book a table for two at 7 tonight`. A name with spaces goes in quotes |
| `/errands` | AI calls and their results |
| `/cancel [#]` | Stop an AI call (no number: the one under way) |
| `/say [#] <text>` | Tell the AI on the call something |
| `/answer <text>` | Answer the question the AI asked you |
| `/mode <phone> ai\|normal` | Switch a phone (its name, or its number in `/status`) |
| `/contacts <name>` | Find a contact |
| `/block <number> [name/reason]` | Add a number to the **Blacklist** (its calls are always rejected) |
| `/stall <number> [name/note]` | Add a number to the **Stall list** (the AI keeps them talking from the first second) |
| `/white <number> [name/note]` | Add a number to the **Whitelist** (family and friends; calls can go straight to your phone, or the AI answers and puts you through only if urgent) |
| `/unblock <number>` | Remove a number from the Blacklist or Stall list; the AI answers it normally again |

You can also just write what you want, in any language, for example:

- "call 0912345678 and book a table for two at 7 tonight"
- "who called yesterday?"
- "block 0912345678 loan spam"
- "put 0223456789 on the stall list"
- "add 0988111222 to whitelist Mom"

Gemini's lowest-cost model (the key from *Settings*) turns your message into
one of the commands above. Without a key, the bot answers with a short pointer
to `/help`.

## What waits for your OK

These get a summary first, with **Go ahead / Drop** buttons (or reply **1** / **2**), and happen only after you answer:

- Placing a call.
- Switching a phone's mode.
- Accepting or ignoring a pending item.
- A line for the AI, or an answer to its question, that the model read from your free text. A typed `/say` and replies to its question go at once.

How the OK works:

- It waits **2 minutes**; after that it does nothing.
- Only the person who asked, in the same chat, can confirm it, and only once.
- A newer request replaces an older one. A pending item already decided on the console or a card is left as it is.
- A call is checked before you are asked, exactly as on the console: emergency, short-code and premium-rate numbers are never called, there must be a phone with a SIM of the number's country, and so on.

## Questions from the AI during its call

When the AI calling for you needs your decision, the question also arrives in
every chat that takes commands, with the AI's options as buttons.

- Tap a button or reply to the message within 40 seconds.
- The console works too, and the first answer counts.
- Because you can now answer from the chat, the AI waits up to those 40 seconds for you even when the console is closed.

The result of an AI call still arrives as the usual notification.

## Safety

- Off by default, one switch per chat.
- Messages from any other chat or sender get **no answer at all** (they are only written to the log), so nobody can find out the bot is there.
- Two limits per chat, both under *Settings → Notifications* → "Commands from chat":
  - **AI calls per chat per hour**: default 5, at most 30, 0 = only from the console.
  - **Messages per chat per minute**: default 20. Past it, the bot is silent until the minute is over.
- Calls go through the same checks, SIM rules and queue as calls started on the console.
- Replies never contain a key or password: any key Bageo holds is cut out before a message is sent. Transcript excerpts are short.
- Only your own message is read as a command. Transcripts, summaries and what callers say are never given to the model that reads your messages, so words spoken on a call cannot become a command.
- In a group, a channel or a room with other people, only the people in **Who may give commands** can use them. With the list empty, nobody can, and the first message there gets a short hint on how to add one. A private chat with the bot needs no list: a Telegram private chat, a Slack direct message, a Discord private message, or a Matrix room with only you and the bot.
- Entries in **Who may give commands** that are no ID (a name) are kept and match nobody, so the list never turns into "everyone".
- The list also applies to the buttons on pending-item cards. When the same bot and chat are switched on twice, both lists apply.
- The command API (`POST /calls/api/control/inbound`) with the MCP token does only what `mcp_calls` allows. While it is `off`, it can look things up but changes nothing.
- The command menu is set only for the chats that take commands; strangers who open the bot see none. Forwarded messages are never read as commands.

## Settings

| Setting | Environment | Default |
|---|---|---|
| Channel: *Take commands from this chat* (`control`) | — | off |
| Channel: *Who may give commands* (`control_users`) | — | nobody in a group; a private chat needs no list |
| `telegram_control` | `TELEGRAM_CONTROL` | off |
| `telegram_control_users` | `TELEGRAM_CONTROL_USERS` | nobody in a group; a private chat needs no list |
| `control_errands_per_hour` | `CONTROL_ERRANDS_PER_HOUR` | 5 (0–30) |
| `control_commands_per_minute` | `CONTROL_COMMANDS_PER_MINUTE` | 20 (1–120) |

## Interactive buttons & commands across all 32 notification channels

Beyond Telegram, Discord, Slack and Matrix two-way chat, **all 32 notification channels** support interactive operation:

1. **One-tap signed action buttons on call summaries & appointments (HMAC-SHA256, no console password needed)**
   - When *Address in notifications* (`public_url`, under *Settings → Notifications*) is filled in, every call summary and pending appointment notification includes one-tap signed actions:
      - Call summaries: **Block**, **Stall**, **Whitelist**
      - Pending appointments: **Accept**, **Decline**
   - How each channel shows them:
      - **Telegram**: Inline Keyboard buttons
      - **ntfy**: Native action buttons (opening the confirmation page) directly in the phone notification shade
      - **Home Assistant**: Companion App `data.actions` notification buttons & `clickAction`
      - **Microsoft Teams**: Adaptive Card `Action.OpenUrl` buttons
      - **Bark / Pushover / Gotify / Chanify**: Native tap-to-open URLs (`url` / `click` / `link`)
      - **Discord / Slack / LINE / Matrix / WeCom / DingTalk / Feishu / Email**: Clickable signed action links appended to the message
      - **Webhook (JSON)**: Structured `actions: [{ "label": "...", "url": "..." }]` array
   - A link first opens a confirmation page, and the action runs only when you press the button on it. A link stops working after 7 days. Behind them is `GET/POST /calls/api/quick-action`.

2. **Universal inbound command endpoint (`POST /calls/api/control/inbound`)**
   - Connect Home Assistant, n8n, Make, Apple Shortcuts, or custom Discord / Slack / LINE / WeCom / Feishu bots by POSTing any slash command or natural language message:
      ```bash
      curl -X POST http://bageo.local:8787/calls/api/control/inbound \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer <console password or MCP token>" \
        -d '{"text": "/status", "channel_id": "ntfy", "reply": true}'
      ```
   - JSON fields:
      - `text`: The command. Accepts all 18 slash commands (`/status`, `/calls`, `/errand`, `/block`, `/stall`, `/white`, `/unblock`, etc.) and natural language in any of the 9 supported languages.
      - `channel_id`: The notification channel to send the reply to.
      - `sender_id`: Who is giving the command.
      - `reply`: With `true`, Bageo sends the reply to that notification channel.
      - `token`: The password can go here too.
   - `"reply": true` goes only to the one notification channel that is enabled and whose id, name or service type equals `channel_id`. With no `channel_id`, or one that matches nothing, nothing is sent.
   - Authenticate with the console password or the MCP token, in the `Authorization: Bearer` or `X-Calls-Token` header, or in the JSON `token` field. If neither is set, nothing is checked.
   - The JSON that comes back has `ok`, `text` (the reply), `buttons`, and `pushed` (whether it was sent out).

## How well tested

Discord, Slack and Matrix have only been tested with simulated servers. They have not been tried on real accounts yet.

## For developers

The commands, the checks, the confirmations and the limits live in
`crates/agent/src/control/`. A chat app only needs a thin adapter that turns
its messages into `control::Inbound`, hands them to `Controller::handle`
and shows the `Outbound` it gets back — or simply calls `POST /calls/api/control/inbound`.

An AI agent (Claude, ChatGPT, Grok, OpenClaw…) can use the same actions
through the built-in MCP server: see [MCP](MCP.md).

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
