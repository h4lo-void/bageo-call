# Notifications

The agent sends a message when a call comes in, when a blocked number called,
when a second call was turned away during a call, when an AI call-back or an
AI errand has ended, and — a few seconds after a call ends — the summary.
Appointments waiting for your decision come along with the summary, and on
Telegram as a card with **Accept** / **Decline** buttons. Set it up under
*Settings → Notifications*: **Add channel**, choose the service, fill in its
fields, **Send test**, **Save**.

- Add as many channels as you like; **every message goes to all enabled ones**.
- Addresses and tokens are stored in `data/calls/config/settings.json` on your
  computer. The web console never shows them again in full, only a hint; leave
  a field empty when editing to keep what is stored.
- *Send notifications* at the top, switched off, silences everything without deleting the
  channels.
- Messages are written in the *Language of notifications and summaries*
  (default: the AI's language), whatever language the call was in.

> **Status:** Telegram has been used daily. The other services are implemented
> from their public documentation and their requests are covered by unit tests,
> but they have not been tried against live accounts yet. If one fails, the
> *Send test* button shows the service's answer — please report it.

## What a message looks like

```text
☎️ Call summary
Number: 0912345678
Phone: Pixel 8
Time: 11/12 18:02

Caller: Jay (friend)
About: lunch tomorrow at noon?
AI said: sounded keen, did not fix the time
To do: call Jay back to confirm
📅 Calendar: Lunch with Jay (11/13 12:00)
https://calendar.google.com/calendar/render?action=TEMPLATE&…
Transcript / recording: http://192.0.2.10:8787/calls/#call=…
```

The last line appears when the agent has a console address: *Address of this console*, or the automatic one when the console is open to your network (not inside Docker). The calendar
lines appear only when the call fixed a date
([Settings reference](SETTINGS.md#summary--calendar)).
The calendar link opens the calendar you chose for **Add to calendar**
(`calendar_app`; Google Calendar until you choose): a filled-in "new event"
form, or for Apple Calendar and `.ics` the event's file at your console
address, opened by a key for that one file. A caller in your contacts appears
as "☎️ Call summary · Jay" in the title.

## Messages you get

Every message goes to all enabled channels, in the *Language of notifications
and summaries*. Each starts with an icon so you can tell them apart at a
glance:

| Icon and title | When | Webhook `event` |
|---|---|---|
| 📞 Incoming call | A call was picked up. | `incoming` |
| ⛔ Blocked number called | A number on your blocked list called; the message says whether it was declined while it rang or answered with the announcement, and your reason for blocking it. | `blocked` |
| ☎️ Call summary | A few seconds after a call ended: who, what about, what the AI said, what you should do, and the links. A caller in your contacts gets their name after the title. | `summary` |
| 📵 Missed while on another call | A second call rang during a call and the phone declined it (the caller heard busy). Call them back if you know the number. | `call_waiting` |
| 📞 AI call-back: <number> | The AI called a caller back with your answer to an appointment: how it went (answered, nobody answered, could not be placed) and the call's summary. | `ai_callback` |
| 📞 AI errand call: <name or number> | An AI errand you started has ended: the result, details, your decision when it needs one, the next step, the time agreed and a link to the *Errands* page. | `ai_errand` |
| 📅 Appointment to confirm | A caller set a time with the AI; on Telegram also as a card with buttons (see [Appointments](#appointments)). | part of `summary` |
| New SMS | A text message the optional [SMS forwarder](../../tools/sms_forward/README.md) passed on. | `event` |

A call-waiting message looks like this:

```text
📵 Missed while on another call: 0912345678 (Pixel 9)
Time: Nov 14, 18:02
```

An errand's result:

```text
📞 AI errand call: Din Tai Fung
Number: 0223456789
Phone: Pixel 9
Time: Nov 14, 18:05

Done: Table for 4 booked tomorrow 7:30 pm under Wang
Details: They hold the table for 15 minutes.
📅 Agreed time: Nov 15, 19:30 Dinner at Din Tai Fung
http://bageo.local:8787/calls/#errands
```

When the AI agreed to something on a question you did not answer, the result
is *Your decision* and a line asks you to check it and call back to change it.

## Appointments

With *Appointments* on ([Settings reference](SETTINGS.md#summary--calendar)),
a call where the caller set a time with the AI gets one line under the
summary instead of the calendar lines, on every notification channel you set
up (Telegram, Apprise, a webhook, …):

```text
📅 Appointment to confirm: Sat, Nov 14, 3:00 PM–4:00 PM House viewing — confirm in the web console: http://bageo.local/calls/#call=…
```

You decide it in the web console: under **To do** at the top of the Calls page,
or in the call's details, pick how the caller hears your answer (**Text**,
**AI call-back** or **Don't tell them**) and press **Accept** or **Decline**.
The link opens the call; when the console has no address other devices can
open (*Address of this console* in Settings), the line ends with "confirm in
the web console" instead. With no notification channel set up nothing is sent,
and the appointment simply waits in the console. Nothing is ever written into
a calendar by itself.

With a Telegram bot set up (bot token and chat ID) and *Send notifications* on,
an appointment waiting for you also comes as a card you can tap:

```text
📅 Appointment to confirm

Number: 0912345678
Time: Sat, Nov 14, 3:00 PM–4:00 PM
What: House viewing
Name: Mr Chen
Phone: Pixel 8
Text to caller: sent when you tap Accept or Decline
Confirm on the web: http://bageo.local/calls/#call=…
[Accept] [Decline]
```

The *Text to caller* line follows *Default reply* for this number. It can also
read "Reply: the AI calls them back once you tap Accept or Decline" (a
landline, or texts turned off), "won't be sent ("Text the caller" is off in
Settings)", "can't text this number, so call them back yourself", or "hidden
number: the caller can't be told". A button press uses *Default reply*; to
choose another way, decide in the web console.

After a press the bot answers with what happened, for example:

```text
✅ Appointment accepted
Sat, Nov 14, 3:00 PM–4:00 PM House viewing
Text sent to 0912345678
📅 Add it to your calendar: https://calendar.google.com/calendar/render?action=TEMPLATE&…
```

or "❌ Appointment declined", "The AI is calling 0912345678 back", "Text not
sent: the phone is not connected" / "the phone app is too old to send texts" /
"error code no_service". When a text did not go out, the appointment in the
console gets **Send the text again**, from the same phone or another connected
one.

**Add to calendar works with any calendar.** The link in the bot's answer, in
notifications and in the console goes to the calendar you picked the first time
you pressed **Add to calendar** (setting `calendar_app`): the filled-in "new
event" form of Google Calendar, Outlook.com, Microsoft 365 or Yahoo Calendar,
or, for Apple Calendar and the plain `.ics` choice, the event's `.ics` file at
*Address of this console*. That `.ics` link carries a key (an HMAC) that opens
only this one file, never your console token, so it is safe to tap from a chat;
without a console address there is no `.ics` link. The agent connects to no
calendar account.

Only the first decision counts, wherever it is made (console, card or
[API](API.md#appointments-and-tasks)); a second one, also at the same moment,
gets "This appointment has already been handled". When you decide in the
console or through the API, the card's buttons are taken off and the bot posts
the same answer in the chat, so the card never offers a decision that was
already made (a card that arrives after you already decided comes without
buttons). If Telegram cannot be reached, the decision still stands.

The AI call-back with your answer speaks the call's language, waits for the
callee to pick up and speak, says who is calling and your answer once, and
says goodbye. Its notification is "📞 AI call-back: <number>"; one that could
not be placed is sent to your channels too.

Sales and scam calls never leave an appointment. When a call turns out to be
one only afterwards — its summary marks it "Sales call" or "Suspected scam" (a
bank's fraud desk or the police calling about fraud does not count) — the
appointment is dropped, no card comes, and the summary says so:

```text
📅 Judged a sales call, so the appointment booked during the call was canceled: Sat, Nov 14, 3:00 PM–4:00 PM House viewing
```


## Services

For each service: what to create, and which field it goes into.

### Telegram

1. In Telegram, talk to **@BotFather**: `/newbot`, pick a name → you get a
   **bot token** like `123456789:AA…`.
2. Open a chat with your new bot and press **Start** (a bot cannot message you
   first).
3. Find your **chat ID**: talk to **@userinfobot**, it replies with your
   numeric ID. For a group, add your bot to the group and use the group's ID
   (a negative number, e.g. `-1001234567890`).

| Field | Value |
|---|---|
| Bot token | the token from @BotFather |
| Chat ID | your numeric ID or the group ID |

### Discord

Channel → *Edit channel → Integrations → Webhooks → New webhook → Copy
webhook URL*.

| Field | Value |
|---|---|
| Webhook URL | `https://discord.com/api/webhooks/…` |

### Slack / Mattermost

Slack: create an app at <https://api.slack.com/apps> → *Incoming Webhooks* →
activate → *Add New Webhook to Workspace* → copy the URL. Mattermost and
Rocket.Chat incoming webhooks accept the same format.

| Field | Value |
|---|---|
| Incoming webhook URL | `https://hooks.slack.com/services/…` |

### LINE

LINE Notify was discontinued in 2025; this uses the **Messaging API**.

1. At <https://developers.line.biz/console/> create a provider and a
   *Messaging API* channel (this creates a LINE Official Account).
2. On the channel's *Messaging API* tab issue a **channel access token
   (long-lived)**.
3. Add the official account as a friend from your LINE app (QR code on the
   same tab).
4. Your **user ID** (`U…`, 33 characters) is on the channel's *Basic settings*
   tab under *Your user ID*. To send to a group instead, invite the account to
   the group and use the group ID (`C…`), which you get from a webhook event.

| Field | Value |
|---|---|
| Channel access token | the long-lived token |
| User / group ID | `U…` or `C…` |

The free plan of an official account includes a limited number of push
messages per month.

### ntfy

Install the ntfy app, subscribe to a topic with a **hard-to-guess name**
(anyone who knows the name can read the messages on the public server).

| Field | Value |
|---|---|
| Server | empty for `https://ntfy.sh`, or your own server's address |
| Topic | the topic name |
| Access token | only for protected topics on your own server (`tk_…`) |

### Gotify

In Gotify: *Apps → Create application* → copy its token.

| Field | Value |
|---|---|
| Server URL | `https://gotify.example.org` |
| App token | the application token |

### Bark (iOS)

Open the Bark app; the URL it shows is `https://api.day.app/<device key>/…`.

| Field | Value |
|---|---|
| Server | empty for `https://api.day.app`, or your own bark-server |
| Device key | the key from the app |

### Pushover

Create an application at <https://pushover.net/apps/build>.

| Field | Value |
|---|---|
| API token | the application's token |
| User key | your user key from the Pushover dashboard |

### WeCom group bot (企业微信群机器人)

Group → *Add group robot* → copy the webhook address.

| Field | Value |
|---|---|
| Webhook URL | `https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=…` |

### DingTalk bot (钉钉群机器人)

Group settings → *Bots* → *Custom* → choose **Sign** as the security setting
and copy both the webhook and the secret (`SEC…`). A bot secured by keywords
instead needs one of its keywords in every message — signing is simpler.

| Field | Value |
|---|---|
| Webhook URL | `https://oapi.dingtalk.com/robot/send?access_token=…` |
| Signing secret | the `SEC…` value (leave empty if the bot has none) |

### Feishu / Lark bot (飞书群机器人)

Group → *Settings → Bots → Add bot → Custom bot* → copy the webhook address
(`open.feishu.cn` or `open.larksuite.com`). Bots with signature verification
enabled are not supported; use a custom keyword or IP allow-list, or none.

| Field | Value |
|---|---|
| Webhook URL | `https://open.feishu.cn/open-apis/bot/v2/hook/…` |

### ServerChan (Server酱)

| Field | Value |
|---|---|
| SendKey | your SendKey (`SCT…` for Turbo, `sctp…` for ServerChan³) |

### Matrix

Create a bot account, invite it to a room and join the room with it. The room
ID is under *Room settings → Advanced* (`!abc…:example.org`). The room must be
unencrypted.

| Field | Value |
|---|---|
| Homeserver URL | `https://matrix.example.org` |
| Access token | the bot account's access token |
| Room ID | `!…:example.org` |

### Email (Gmail)

Sends through Gmail's SMTP server with an **app password** (Google account →
*Security → 2-Step Verification → App passwords*; needs 2-step verification).

| Field | Value |
|---|---|
| Gmail address | the sending account |
| App password | the 16-character app password |
| Send to | recipient; empty = the same address |

### Apprise API

If you already run [Apprise API](https://github.com/caronc/apprise-api), which
reaches a hundred more services:

| Field | Value |
|---|---|
| Notify URL | `http://apprise.local:8000/notify/<your config key>` |
| Tag | optional tag |

### Webhook (JSON)

For n8n, Zapier, Make, Home Assistant, Node-RED or your own code. The agent
`POST`s JSON to your URL; with a token set it adds
`Authorization: Bearer <token>`.

```json
{
  "event": "summary",
  "title": "Call summary 0912345678",
  "body": "☎️ Call summary\nNumber: 0912345678\n…",
  "text": "(same as body)",
  "data": {
    "caller_id": "0912345678",
    "device": "Pixel 8",
    "time": "11/12 18:02",
    "summary": "Caller: Jay (friend)\nAbout: …"
  }
}
```

| `event` | When | `data` |
|---|---|---|
| `incoming` | a call was picked up | `caller_id`, `device`, `time` |
| `blocked` | a blocked number called | `caller_id`, `device`, `time`, `reason`, `declined` (`true`: refused while it rang; `false`: answered with the announcement) |
| `summary` | a few seconds after a call ended | `caller_id`, `device`, `time`, `summary`, `contact` (the contact's name, or empty) |
| `call_waiting` | a second call rang during a call and was declined | `caller_id`, `device`, `time`, `sim_country` |
| `ai_callback` | the AI's call-back for an appointment ended, or could not be placed | `caller_id`, `device`, `time`, `summary` |
| `ai_errand` | an AI errand ended, or never got going | `number`, `device`, `time`, `summary` |
| `voicemail` | a voicemail was saved (older menu mode) | `caller_id`, `device`, `time`, `seconds` |
| `event` | a message sent through the API (`/calls/api/notify/send`) | — |
| `test` | *Send test* | — |

A `2xx` answer counts as delivered. For everything about a call (transcript,
recordings), fetch it from the [API](API.md) with the call's ID.

## Sending your own messages

Scripts on your network can use the same channels:

```text
POST /calls/api/notify/send        {"title": "Garage", "body": "Door left open"}
```

See [API](API.md). The optional [SMS forwarder](../../tools/sms_forward/README.md)
uses this to pass on the phone's text messages.

## Set through the environment instead

For containers and scripted set-ups, three routes can be given at start-up
instead of in the web console: `TELEGRAM_BOT_TOKEN` + `TELEGRAM_CHAT_ID`,
`NOTIFY_WEBHOOK_URL` (the JSON webhook above without a token), and
`NOTIFY_GATEWAY_URL` + `NOTIFY_GATEWAY_TAG` (an Apprise API base address).
They are used in addition to the channels of the web console.
