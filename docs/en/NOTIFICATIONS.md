# Notifications

The agent sends a message when a call comes in, when a blocked number called,
and — a few seconds after a call ends — the summary. Set it up under
*Settings → Notifications*: **Add channel**, choose the service, fill in its
fields, **Send test**, **Save**.

- Add as many channels as you like; **every message goes to all enabled ones**.
- Addresses and tokens are stored in `data/calls/config/settings.json` on your
  computer. The web console never shows them again in full, only a hint; leave
  a field empty when editing to keep what is stored.
- *Send notifications* at the top, switched off, silences everything without deleting the
  channels.
- Messages are written in the language chosen under *Settings → Language*.

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
Transcript / recording: http://192.168.1.20:8787/calls/#call=…
```

The last line appears when the agent has a console address: *Address of this console*, or the automatic one when the console is open to your network (not inside Docker). The calendar
lines appear only when the call fixed a date
([Settings reference](SETTINGS.md#summary--calendar)).

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
| `blocked` | a blocked number called | `caller_id`, `device`, `time`, `reason` |
| `summary` | a few seconds after a call ended | `caller_id`, `device`, `time`, `summary` |
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
