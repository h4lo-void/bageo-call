# Chat commands: run Bageo from Telegram

Away from the web console, you can use the Telegram chat your notifications
go to: look up calls, have the AI call someone, decide to-dos, answer the AI's
questions during its call, and switch a phone between AI answering and normal
phone. Telegram is the first chat app supported; the code is written so other
chat apps and AI agents can be added (see [For developers](#for-developers)).

## Turn it on

Chat commands are **off** until you switch them on for a chat.

1. Set up the Telegram channel first ([Notifications → Telegram](NOTIFICATIONS.md#telegram)).
2. *Settings → Notifications* → the Telegram channel → **Take commands from this chat**.
3. Optional: **Who may give commands**: Telegram user IDs (ask **@userinfobot**
   for yours). Empty means anyone in that chat. In a group, list your own ID.
4. **Save changes**, then send `/help` to the bot.

The bot set by environment variables (`TELEGRAM_BOT_TOKEN` + `TELEGRAM_CHAT_ID`)
has its own switch below the channels, or `TELEGRAM_CONTROL=true`;
`TELEGRAM_CONTROL_USERS=123456789,987654321` limits who may give commands there.

In a group the bot only reacts to commands (`/calls`, or `/calls@your_bot`)
and to replies to its own messages; everything else people say is ignored.

## Commands

Numbers like `1` or `#2` refer to the list the chat was shown last
(`/calls`, `/tasks`, `/errands`, `/status`).

| Command | What it does |
|---|---|
| `/help` | The commands |
| `/status` | Agent version, each phone (AI answers / normal phone / offline / in a call), calls in progress |
| `/calls [n] [today\|yesterday\|YYYY-MM-DD] [missed\|scam] [word or number]` | Recent calls, one line each: time, caller, first line of the summary (5 by default, up to 20) |
| `/call <#>` | One call: summary, the start of the transcript (600 characters at most), link to the web console |
| `/tasks` | To-dos waiting for you (appointments, messages) |
| `/confirm <#>` · `/ignore <#>` | Decide a to-do, as on the web console |
| `/errand <number or contact> <what to get done>` | The AI calls for you, e.g. `/errand 0912345678 book a table for two at 7 tonight`. A name with spaces goes in quotes |
| `/errands` | AI calls and their results |
| `/cancel [#]` | Stop an AI call (no number: the one under way) |
| `/say [#] <text>` | Tell the AI on the call something |
| `/answer <text>` | Answer the question the AI asked you |
| `/mode <phone> ai\|normal` | Switch a phone (its name, or its number in `/status`) |
| `/contacts <name>` | Find a contact |

You can also just write what you want: "call 0912345678 and book a table for
two at 7 tonight", "who called yesterday?". Gemini's lowest-cost model
(the key from *Settings*) turns it into one of the commands above; without a
key the bot answers with a short pointer to `/help`.

## What waits for your OK

Placing a call, switching a phone's mode and accepting or ignoring a to-do
first get a summary with **✅ Go ahead / ✖ Drop** buttons (or reply **1** / **2**).

- It waits **2 minutes**; after that it does nothing.
- Only the person who asked, in the same chat, can confirm it, and only once.
  A newer request replaces an older one.
- A call is checked before you are asked: emergency, short-code and
  premium-rate numbers, a phone with a SIM of the number's country, and so on,
  exactly as on the web console.

## Questions from the AI during its call

When the AI calling for you needs your decision, the question also arrives in
every chat that takes commands, with the AI's options as buttons. Tap one or
reply to the message within 40 seconds; the web console works too, and the
first answer counts. Because you can now answer from the chat, the AI waits up
to those 40 seconds for you even when the web console is closed.

The result of an AI call still arrives as the usual notification.

## Safety

- Off by default, one switch per chat. Messages from any other chat or sender
  get **no answer at all** (they are logged), so nobody can find out the bot is there.
- Limits per chat: **AI calls per hour** (default 5, at most 30, 0 = only from
  the web console) and **messages per minute** (default 20; past it the bot is
  silent until the minute is over). Both are under *Settings → Notifications*.
- Calls go through the same checks, SIM rules and queue as calls started on the
  web console.
- Replies never contain a key or token: any the agent holds is cut out before a
  message is sent. Transcript excerpts are short.
- Only your own message is read as a command. Transcripts, summaries and what
  callers say are never given to the model that reads your messages, so words
  spoken on a call cannot become a command.
- Anyone who can write in a chat with commands on can use them. Keep it a
  private chat with the bot, or list the allowed user IDs.

## Settings

| Setting | Environment | Default |
|---|---|---|
| Channel: *Take commands from this chat* (`control`) | — | off |
| Channel: *Who may give commands* (`control_users`) | — | anyone in the chat |
| `telegram_control` | `TELEGRAM_CONTROL` | off |
| `telegram_control_users` | `TELEGRAM_CONTROL_USERS` | anyone in the chat |
| `control_errands_per_hour` | `CONTROL_ERRANDS_PER_HOUR` | 5 (0–30) |
| `control_commands_per_minute` | `CONTROL_COMMANDS_PER_MINUTE` | 20 (1–120) |

## For developers

The commands, the checks, the confirmations and the limits live in
`crates/agent/src/control/`; a chat app only needs a thin adapter that turns
its messages into `control::Inbound`, hands them to `Controller::handle`
and shows the `Outbound` it gets back. The contract is written at the top of
`control/mod.rs`; `control/telegram.rs` is the first adapter. An MCP tool
interface can map its tools to the same actions.

