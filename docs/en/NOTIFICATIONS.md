# Notifications: call summaries on Telegram, LINE, Discord, email and 28 more

Your AI phone assistant answers the call. A few seconds after it hangs up, you
get a short summary on your phone: who called, what they wanted, what the AI
said and what you should do.

Bageo also tells you:

- when a call comes in.
- when a blocked number or a second caller was turned away.
- when an AI call-back or AI errand has finished.

It sends to 32 services, from Telegram, LINE, Teams and Signal to WeCom, ntfy,
Home Assistant, email and your own webhook.

**Set it up in one minute:**

1. Open *Settings → Notifications*.
2. Under *Channels*, choose a service and press **Add channel**.
3. Fill in its fields. What each one needs is under [Services](#services).
4. Press **Send test** and check that it arrives.
5. Press **Save changes**.

**Good to know:**

- Add as many channels as you like. **Every message goes to every enabled
  channel.**
- Give each channel a *Name* (for example "Family group"). To pause one, switch
  off *Enabled* instead of deleting it.
- Addresses and tokens are stored in `data/calls/config/settings.json` on your
  computer. The web console only shows a hint after that; leave a field empty
  when editing to keep what is stored.
- With *Send notifications* at the top switched off, call messages and Telegram
  appointment cards stop. Your channels stay.
- Messages that scripts send through the API (see
  [below](#sending-your-own-messages)), such as forwarded texts, still go out.
- Messages are written in the *Language of notifications and summaries*
  (default: *Same as the AI's language*), whatever language the call was in.
- Long messages are cut to fit each service (for example 1,000 characters on
  Pushover, 1,900 on Discord and WeCom).

> **How well tested:** Telegram is the most used route. The other services are
> built from their public documentation and covered by unit tests, but have
> seen little use with live accounts. If one fails, **Send test** shows the
> service's answer; please report it.

> The services added after 1.0 (Microsoft Teams, Google Chat, Rocket.Chat,
> Zulip, Webex, Nextcloud Talk, Synology Chat, Signal, Pushbullet, PushPlus,
> PushDeer, WxPusher, Chanify, SMTP email, Home Assistant and IFTTT) have not
> been tried with live accounts at all yet.

## What a message looks like

A call summary from a caller saved in your contacts:

```text
Call summary · Jay
Number: 0912345678
Phone: Pixel 8
Time: Nov 12, 18:02

Caller: Jay (friend)
About: lunch tomorrow at noon?
AI said: sounded keen, did not fix the time
To do: call Jay back to confirm
Calendar: Lunch with Jay (11/13 12:00)
https://calendar.google.com/calendar/render?action=TEMPLATE&…
Transcript / recording: http://192.0.2.10:8787/calls/#call=…
```

- **The title** gets the contact's name when the caller is in *Contacts*.
- **The calendar lines** appear only when the call fixed a date and *Suggest
  calendar events* is on ([Settings reference](SETTINGS.md#summary--calendar)).
- **The calendar link** opens the calendar you chose the first time you pressed
  **Add to calendar** (`calendar_app`). Until you choose, there is no link:
  open the call's page (the last line) and press **Add to calendar** to pick
  one. See [Add to calendar](#appointments) below.
- **The last line** links to the call's page. It appears only when Bageo has an
  address other devices can open: *Address in notifications* (under *Settings →
  Notifications*), or the automatic one when the console is open to your network
  (not inside Docker). See [Using the console from other
  devices](SETTINGS.md#using-the-console-from-other-devices).

## Messages you get

You can receive these messages:

| Title | When | Webhook `event` |
|---|---|---|
| Incoming call | A call was picked up. | `incoming` |
| Outgoing call | Bageo placed a call itself: an AI errand, an appointment call-back or a to-do call-back. An errand set to redial until answered tells you on its first try only. | `outgoing` |
| Blocked number called | A blocked number called. The message says whether it was declined while it rang or answered with the announcement, and your reason for blocking it. | `blocked` |
| Call summary | A few seconds after a call ended: who, what about, what the AI said, what you should do, and the links. | `summary` |
| Missed while on another call | A second call rang during a call and the phone declined it (the caller heard busy). Call back if you know the number. | `call_waiting` |
| Repeated calls from one number | One number called again and again within a minute. You get one message saying how many more calls came, instead of one each. | `repeat_calls` |
| AI call-back: <number> | The AI called a caller back with your answer to an appointment. The message gives how it went (answered, nobody answered, could not be placed, or the number cannot be called back) and the call's summary. | `ai_callback` |
| AI errand call: <name or number> | An AI errand you started has ended. The message has the result, details, your decision when one is needed, the next step, the time agreed and a link to the *Errands* page. | `ai_errand` |
| Appointment to confirm | A caller set a time with the AI. Telegram receives a card with buttons; every other channel receives signed one-tap **Accept / Decline** buttons or links (see [Appointments](#appointments)). | `appointment_pending` / `summary` |
| Call forwarded to your phone | Someone ticked under *Who can be forwarded* called while *Transfer* is *Don't answer: send to your phone*. The phone declined the call so your carrier forwards it to you, and you get an instant alert. | `whitelist_direct_transfer` |
| Urgent call alert | Someone ticked under *Who can be forwarded* called, the AI answered and the caller said it was urgent; you are alerted at once. With *AI answers, puts you through if urgent* and your number filled in, the phone also calls you. | `whitelist_urgent_transfer` |
| Test notification | You pressed **Send test**. | `test` |
| Chat command reply | Reply to a command sent to `POST /calls/api/control/inbound` with `"reply": true`. | `control_reply` |
| New SMS | A text message passed on by the optional [SMS forwarder](../../tools/sms_forward/README.md). | `event` |
| Automatic backup failed | A scheduled backup failed to write or check. | `event` |

A number that keeps calling does not flood you. For each number, Bageo sends at most two *Incoming call*, *Blocked number called* or *Missed while on another call* messages a minute, and a minute later one *Repeated calls from one number* message tells you how many more came. Urgent call alerts, appointments, call summaries, AI errands and call-backs, voicemail, outgoing calls and SMS are never held back.

A call-waiting message:
```text
Missed while on another call: 0912345678 (Pixel 9)
Time: Nov 14, 18:02
```

An errand's result, for example a restaurant booking the AI made for you:

```text
AI errand call: Din Tai Fung
Number: 0223456789
Phone: Pixel 9
Time: Nov 14, 18:05

Done: Table for 4 booked tomorrow 7:30 pm under Wang
Details: They hold the table for 15 minutes.
Agreed time: Nov 15, 19:30 Dinner at Din Tai Fung
http://bageo.local/calls/#errands
```

If the AI agreed to something on a question you did not answer in time, the
result reads *Your decision*, with a line asking you to check it and call back
to change it.

## Appointments

With *Appointments* on ([Settings reference](SETTINGS.md#summary--calendar)),
a call where the caller set a time with the AI gets one line under the summary,
in place of the calendar lines, on every channel:

```text
Appointment to confirm: Sat, Nov 14, 3:00 PM–4:00 PM House viewing — confirm in the web console: http://bageo.local/calls/#call=…
```

**Decide in the web console:**

1. Open the *Calls* page and find *To do* at the top, or open the call's
   details.
2. Pick how the caller hears your answer: **Text**, **AI call-back** or **Don't
   tell them**.
3. Press **Accept** or **Decline**.

- The link in a notification opens the call. Without an address other devices
  can open (*Address in notifications*), the line ends with "confirm in the web
  console" instead.
- With no channel set up nothing is sent, and the appointment simply waits in
  the console.
- Bageo never writes anything into a calendar by itself.

**Decide in Telegram with one tap:** when a Telegram bot is given through
`TELEGRAM_BOT_TOKEN` and `TELEGRAM_CHAT_ID`
([below](#set-through-the-environment-instead)) and *Send notifications* is on,
a waiting appointment also comes as a card.

A Telegram channel added in the web console gets the reminder line above, but
not the card.

```text
Appointment to confirm

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
read:

- "Reply: the AI calls them back once you tap Accept or Decline" (a landline,
  or texts turned off).
- "won't be sent ("Text the caller" is off in Settings)".
- "can't text this number, so call them back yourself".
- "hidden number: the caller can't be told".

A tap uses *Default reply*; to choose another way, decide in the web console.

After a tap the bot answers with what happened:

```text
Appointment accepted
Sat, Nov 14, 3:00 PM–4:00 PM House viewing
Text sent to 0912345678
Add it to your calendar: https://calendar.google.com/calendar/render?action=TEMPLATE&…
```

Other answers:

- "Appointment declined"
- "The AI is calling 0912345678 back"
- "Text not sent: the phone is not connected", "the phone app is too old to
  send texts" or "error code no_service"

When a text did not go out, the appointment in the console gets **Send the text
again**, from the same phone or another connected one.

**Add to calendar works with any calendar.** The link in the bot's answer, in
notifications and in the console opens the calendar you picked the first time
you pressed **Add to calendar** (setting `calendar_app`):

- Google Calendar, Outlook.com, Microsoft 365 or Yahoo Calendar: the filled-in
  "new event" form.
- Apple Calendar or plain `.ics`: the event's `.ics` file at *Address in
  notifications*.

Until you pick one, the bot's answer links the call's page in the console
instead (no console address, no link). There, **Add to calendar** asks which
calendar and remembers it.

The `.ics` link carries a key (an HMAC) that opens only this one file, never
your console password, so it is safe to tap from a chat. Without a console
address there is no `.ics` link.

Bageo connects to no calendar account.

**Only the first decision counts**, wherever it is made (console, card or
[API](API.md#appointments-and-tasks)). A second one, even at the same moment,
gets "This appointment has already been handled".

When you decide in the console or through the API, the card's buttons are
removed and the bot posts the same answer in the chat. A card that arrives
after you decided comes without buttons. If Telegram cannot be reached, the
decision still stands.

The AI call-back speaks the call's language. It waits for the callee to pick up
and speak, says who is calling and your answer once, and says goodbye.

Its notification is "AI call-back: <number>". A call-back that could not be
placed is reported too.

Sales and scam calls never leave an appointment. A bank's fraud desk or the
police calling about fraud does not count as one.

When a call turns out to be a sales or scam call only afterwards (its summary
marks it "Sales call" or "Suspected scam"), the appointment is dropped, no card
is sent, and the summary says so:

```text
Judged a sales call, so the appointment booked during the call was canceled: Sat, Nov 14, 3:00 PM–4:00 PM House viewing
```

## Services

Pick the app you already check every day:

| Service | You need | Popular with |
|---|---|---|
| [Telegram](#telegram) | bot token, chat ID | anyone; best for appointments |
| [Discord](#discord) | webhook URL, or bot token and channel ID | Discord servers |
| [Slack / Mattermost](#slack--mattermost) | incoming webhook URL, or bot token and channel ID | work chats |
| [Microsoft Teams](#microsoft-teams) | workflow webhook URL | Microsoft 365 workplaces |
| [Google Chat](#google-chat) | webhook URL | Google Workspace |
| [Rocket.Chat](#rocketchat) | webhook URL | self-hosted team chat |
| [Zulip](#zulip) | server URL, bot email and API key, channel | open-source team chat |
| [Webex](#webex) | bot access token, room ID or email | Cisco Webex |
| [Nextcloud Talk](#nextcloud-talk) | server URL, user and app password, conversation token | Nextcloud self-hosters |
| [Synology Chat](#synology-chat) | webhook URL | Synology NAS owners |
| [LINE](#line) | channel access token, user or group ID | Taiwan, Japan, Thailand |
| [Signal](#signal) | your own signal-cli-rest-api, sender number, recipients | Signal users |
| [ntfy](#ntfy) | topic | self-hosters, Android push |
| [Gotify](#gotify) | server URL, app token | self-hosters |
| [Bark (iOS)](#bark-ios) | device key | iPhone push |
| [Pushover](#pushover) | API token, user key | iOS and Android push |
| [Pushbullet](#pushbullet) | access token | Android and desktop push |
| [WeCom group bot](#wecom-group-bot) | webhook URL | 企业微信 |
| [DingTalk bot](#dingtalk-bot) | webhook URL, signing secret | 钉钉 |
| [Feishu / Lark bot](#feishu--lark-bot) | webhook URL | 飞书 / Lark |
| [ServerChan](#serverchan) | SendKey | WeChat push (Server酱) |
| [PushPlus](#pushplus) | token | WeChat push (推送加) |
| [PushDeer](#pushdeer) | PushKey | iOS and Android push, self-hostable |
| [WxPusher](#wxpusher) | SPT, or appToken and UIDs | WeChat push |
| [Chanify](#chanify) | token | iOS and Android push, self-hostable |
| [Matrix](#matrix) | homeserver URL, access token, room ID | Element and other Matrix apps |
| [Email (Gmail)](#email-gmail) | Gmail address, app password | email |
| [Email (SMTP)](#email-smtp) | SMTP URL, password | any other mail provider |
| [Home Assistant](#home-assistant) | server URL, access token, notify service | smart homes, the Companion app |
| [IFTTT Webhooks](#ifttt-webhooks) | Webhooks key, event name | IFTTT applets |
| [Apprise API](#apprise-api) | notify URL | 100+ more services |
| [Webhook (JSON)](#webhook-json) | URL | n8n, Zapier, Make, Home Assistant, your code |

### Telegram

1. In Telegram, talk to **@BotFather** and send `/newbot`, then pick a name.
2. You get a **bot token** like `123456789:AA…`. This is the *Bot token*.
3. Open a chat with your new bot and press **Start** (a bot cannot message you
   first).
4. Talk to **@userinfobot**. It replies with your numeric ID, which is the
   *Chat ID*.
5. To send to a group, add your bot to the group and use the group's ID (a
   negative number, e.g. `-1001234567890`).

| Field | Value |
|---|---|
| Bot token | the token from @BotFather |
| Chat ID | your numeric ID or the group ID |

For the appointment card with buttons, give the same bot through
`TELEGRAM_BOT_TOKEN` and `TELEGRAM_CHAT_ID` as well; you will not get each
message twice.

To check calls, have the AI call someone or work through *To do* from this
chat, see [Chat commands](CHAT_COMMANDS.md). Turn on *Take commands from this
chat* for the channel first.

### Discord

1. Open the channel's settings → *Integrations → Webhooks*.
2. Press *New Webhook*.
3. Copy the webhook URL into *Webhook URL*.

| Field | Value |
|---|---|
| Webhook URL | `https://discord.com/api/webhooks/…` |

Instead of a webhook you can fill in a bot token and a channel ID: one of the two is enough for notifications. With the bot, Bageo can also write to you in a private message: put your own user ID in *Channel ID*. To take commands, fill in the bot token and the channel ID. The steps are in [Chat commands](CHAT_COMMANDS.md#discord).

| Field | Value |
|---|---|
| Bot token | the bot's token |
| Channel ID | the ID of the channel, or your own user ID for private messages |

### Slack / Mattermost

**Slack:**

1. Create an app at <https://api.slack.com/apps>.
2. Turn on *Incoming Webhooks*.
3. Press *Add New Webhook to Workspace*.
4. Copy the URL.

**Mattermost:** go to *Integrations → Incoming Webhooks*.

| Field | Value |
|---|---|
| Incoming webhook URL | `https://hooks.slack.com/services/…` |

Instead of a webhook you can fill in a bot token and a channel ID: one of the two is enough for notifications. To take commands in a Slack channel (Slack only, not Mattermost), fill in all three fields below. The steps are in [Chat commands](CHAT_COMMANDS.md#slack).

| Field | Value |
|---|---|
| Bot token | the token that starts with `xoxb-` |
| App-level token (for chat commands) | the token that starts with `xapp-` |
| Channel ID | the ID of the channel |

### Microsoft Teams

The old Office 365 connectors (the `webhook.office.com` addresses) are retired.
This uses a **Workflows** webhook, which posts an Adaptive Card.

1. In Teams, open the channel's **…** menu → *Workflows*.
2. Choose *Post to a channel when a webhook request is received*, pick the
   team and channel, and finish.
3. Copy the URL it shows (it carries a signature, so keep it as private as a
   password).

| Field | Value |
|---|---|
| Workflow webhook URL | the URL from step 3 |

Each line of the message becomes its own line on the card; the first one is
bold.

### Google Chat

Needs a Google Workspace account; personal Google accounts cannot add webhooks.

1. Open the space and click its name.
2. Choose *Apps & integrations → Webhooks → Add webhook*.
3. Copy the URL.

| Field | Value |
|---|---|
| Webhook URL | `https://chat.googleapis.com/v1/spaces/…/messages?key=…&token=…` |

### Rocket.Chat

1. Go to *Administration → Workspace → Integrations → New → Incoming*.
2. Turn it on, then choose the channel and the user it posts as.
3. Save, and copy the *Webhook URL*.

| Field | Value |
|---|---|
| Webhook URL | `https://chat.example.org/hooks/…/…` |

### Zulip

1. Under *Personal settings → Bots*, choose *Add a new bot* (the *Incoming
   webhook* type is enough).
2. Subscribe the bot to the channel.
3. Find its email and API key on the bot's card.

| Field | Value |
|---|---|
| Server URL | your organization, e.g. `https://example.zulipchat.com` |
| Bot email:API key | the two joined by a colon: `calls-bot@example.zulipchat.com:abc123…` |
| Channel > topic, or email | `calls > phone`; a channel alone (`calls`) posts to the topic "Bageo"; one or more emails, separated by commas, send a direct message |

Messages are cut at 2,400 characters to stay inside Zulip's 10,000-byte limit.

### Webex

1. Create a bot at <https://developer.webex.com/my-apps>.
2. Copy its **bot access token** (it is shown once).
3. Add the bot to a space.
4. Sign in on developer.webex.com and run *List Rooms* with the bot's token to
   find the room ID.
5. To message one person, enter their email instead.

| Field | Value |
|---|---|
| Bot access token | the bot's token |
| Room ID or email | `Y2lzY29zcGFyazovL3Vz…` or `someone@example.com` |

Messages are cut at 1,800 characters (Webex takes at most 7,439 bytes).

### Nextcloud Talk

Posts as a Nextcloud user; a separate user just for Bageo keeps things tidy.

1. Create an **app password** for that user under *Personal settings → Security
   → Devices & sessions*.
2. Add the user to the conversation.
3. Use the last part of the conversation's link as the conversation token
   (`https://cloud.example.org/call/abc123xy` → `abc123xy`).

| Field | Value |
|---|---|
| Server URL | `https://cloud.example.org` |
| User:app password | `bageo:xxxxx-xxxxx-xxxxx-xxxxx-xxxxx` |
| Conversation token | `abc123xy` |

### Synology Chat

1. In Synology Chat, click your profile picture → *Integration → Incoming
   Webhooks → Create*.
2. Pick the channel and save.
3. Copy the webhook URL.

| Field | Value |
|---|---|
| Webhook URL | `https://nas.example.org:5001/webapi/entry.cgi?api=SYNO.Chat.External&method=incoming&version=2&token=…` |

Bageo checks HTTPS certificates. If your NAS still has its self-signed
certificate, use the `http://` address on port 5000 inside your own network.

### LINE

LINE Notify was shut down in 2025; this uses the **Messaging API**.

1. At <https://developers.line.biz/console/> create a provider and a
   *Messaging API* channel (this creates a LINE Official Account).
2. On the channel's *Messaging API* tab issue a **channel access token
   (long-lived)**.
3. Add the official account as a friend in your LINE app (QR code on the same
   tab).
4. Your **user ID** (`U…`, 33 characters) is on the channel's *Basic settings*
   tab under *Your user ID*.
5. To send to a group, invite the account to the group and use the group ID
   (`C…`), which you get from a webhook event.

| Field | Value |
|---|---|
| Channel access token | the long-lived token |
| User or group ID | `U…` or `C…` |

The free plan of an official account includes a limited number of push
messages per month.

### Signal

Signal has no bot API, so this goes through a server you run yourself.

1. Run a
   [signal-cli-rest-api](https://github.com/bbernhard/signal-cli-rest-api)
   server (a Docker container).
2. Link it as a second device of your own Signal account, or register it with a
   separate number.

| Field | Value |
|---|---|
| API server URL | where the container listens, e.g. `http://signal-api:8080` |
| Sender number | the number signal-cli uses, in international format (`+15550100`) |
| Recipients | numbers (`+15550101`) or group IDs (`group.…`), separated by commas |

Messages are cut at 2,000 characters.

### ntfy

1. Install the ntfy app.
2. Subscribe to a topic with a **hard-to-guess name**.

On the public server, anyone who knows the name can read the messages.

| Field | Value |
|---|---|
| Server | empty for `https://ntfy.sh`, or your own server's address |
| Topic | the topic name |
| Access token (optional) | only for protected topics on your own server (`tk_…`) |

### Gotify

1. In Gotify, go to *Apps → Create application*.
2. Copy its token.

| Field | Value |
|---|---|
| Server URL | your Gotify server, e.g. `https://gotify.example.org` |
| App token | the application token |

### Bark (iOS)

1. Open the Bark app on your iPhone.
2. Copy the device key from the sample URL, which looks like
   `https://api.day.app/<device key>/…`.

| Field | Value |
|---|---|
| Server | empty for `https://api.day.app`, or your own bark-server |
| Device key | the key from the app |

### Pushover

1. Create an application at <https://pushover.net/apps/build>.
2. Copy its API token.
3. Copy your user key from the Pushover dashboard.

| Field | Value |
|---|---|
| API token | the application's token |
| User key | your user key from the Pushover dashboard |

### Pushbullet

1. Go to pushbullet.com → *Settings → Account → Create Access Token*.
2. Copy the access token.

Free accounts have a monthly push limit.

| Field | Value |
|---|---|
| Access token | `o.…` |
| Device ID or email (optional) | empty = all your devices; a device ID (`iden`) for one device; an email for another Pushbullet user |

### WeCom group bot

1. In the 企业微信 group, choose *Add group robot*.
2. Copy the webhook address.

| Field | Value |
|---|---|
| Webhook URL | `https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=…` |

### DingTalk bot

1. Open the 钉钉 group settings → *Bots* → *Custom*.
2. Choose **Sign** as the security setting.
3. Copy both the webhook and the secret (`SEC…`).

A bot secured by keywords instead needs one of its keywords in every message;
signing is simpler.

| Field | Value |
|---|---|
| Webhook URL | `https://oapi.dingtalk.com/robot/send?access_token=…` |
| Signing secret (optional) | the `SEC…` value; empty if the bot has none |

### Feishu / Lark bot

1. Open the 飞书 group → *Settings → Bots → Add bot → Custom bot*.
2. Copy the webhook address (`open.feishu.cn` or `open.larksuite.com`).

Do not turn on signature verification; Bageo does not support it. A custom
keyword or an IP allow-list is fine.

| Field | Value |
|---|---|
| Webhook URL | `https://open.feishu.cn/open-apis/bot/v2/hook/…` |

### ServerChan

Copy your SendKey from the ServerChan (Server酱) website.

| Field | Value |
|---|---|
| SendKey | `SCT…` for Turbo, `sctp…` for ServerChan³ |

### PushPlus

1. Log in at <https://www.pushplus.plus> with WeChat.
2. Copy your **token**.

Messages arrive through the PushPlus WeChat official account.

| Field | Value |
|---|---|
| Token | your token |
| Group code (optional) | a group's code to send to that group; empty = only you |

### PushDeer

1. Install PushDeer and sign in.
2. Add a **key** under *Keys*.
3. Copy it.

| Field | Value |
|---|---|
| Server | empty for `https://api2.pushdeer.com`, or your own PushDeer server |
| PushKey | `PDU…` |

### WxPusher

Two ways, both on <https://wxpusher.zjiekou.com>.

**Simple push:**

1. Follow the QR code in WeChat.
2. Get your simple push token (`SPT_…`).
3. Leave *UIDs or topic IDs* empty.

**Application:**

1. Create an application to get its **appToken** (`AT_…`).
2. Scan its QR code in WeChat; everyone who scans it gets a user ID (`UID_…`).
3. For topics, use their numeric IDs.

| Field | Value |
|---|---|
| appToken (AT_…) or SPT | `SPT_…` or `AT_…` |
| UIDs or topic IDs | only with an appToken: `UID_…` values and topic numbers, separated by commas |

WeChat shows the first 20 characters of the title as the summary.

### Chanify

1. In the Chanify app, create a **token** for a sender.
2. Copy it.

| Field | Value |
|---|---|
| Server | empty for `https://api.chanify.net`, or your own Chanify node |
| Token | the token from the app |

### Matrix

1. Create a bot account.
2. Invite it to a room and join the room with it.
3. Find the room ID under *Room settings → Advanced* (`!abc…:example.org`). The room's address (`#name:example.org`) works too.

The room must be unencrypted: Bageo cannot read encrypted rooms, and the test in the settings tells you when a room is encrypted. Create a new room with encryption turned off.

| Field | Value |
|---|---|
| Homeserver URL | `https://matrix.example.org` |
| Access token | the bot account's access token |
| Room ID | `!…:example.org` or `#name:example.org` |

The same channel can also take commands. See [Chat commands](CHAT_COMMANDS.md#matrix).

### Email (Gmail)

Sends through Gmail's SMTP server. The sender name is "Bageo". It needs an
**app password**:

1. Turn on 2-step verification in your Google account.
2. Go to *Security → 2-Step Verification → App passwords* and create one.

| Field | Value |
|---|---|
| Gmail address | the sending account |
| App password | the 16-character app password |
| Send to (empty = yourself) | the recipient |

### Email (SMTP)

Any mail server that offers TLS: iCloud, Yahoo, Fastmail, QQ Mail, 163, your
company's or your own. The server and login go into one field:

```text
smtp://you@example.com@smtp.example.com:587     STARTTLS (most providers)
smtps://you@example.com@smtp.example.com:465    TLS from the start
```

- The part before the last `@` is the login, which is also the sender address.
- `%40` works for the login's own `@`.
- Port 465 always means TLS from the start.
- Without a port, 587 is used (465 with `smtps://`).
- The sender name is "Bageo".

| Field | Value |
|---|---|
| SMTP URL (smtp://login@host:port) | as above |
| Password | the account's password, or the app password / authorization code your provider asks for (iCloud, Yahoo, Fastmail, QQ Mail and 163 do) |
| Send to (empty = the login) | the recipient |

Limits:

- The login must be an email address.
- The server must accept `AUTH LOGIN`.
- Its certificate must come from a public certificate authority (self-signed
  certificates are refused).
- Connections without TLS are not supported.

For Gmail, the [Gmail](#email-gmail) channel needs fewer fields.

### Home Assistant

Sends through a Home Assistant **notify** action, for example to the Companion
app on your phone.

1. In Home Assistant: your profile → *Security* → *Long-lived access tokens* →
   *Create token*.
2. Find the service under *Developer tools → Actions*: search for `notify.`
   (the Companion app adds `notify.mobile_app_<phone name>`).

| Field | Value |
|---|---|
| Server URL | empty for `http://homeassistant.local:8123`, or your address |
| Long-lived access token | the token from step 1 |
| Notify service | `mobile_app_pixel_9`, with or without `notify.` |

Messages are cut at 1,000 characters because phone notifications carry only
about 4 KB.

### IFTTT Webhooks

1. At <https://ifttt.com/maker_webhooks> connect Webhooks; *Documentation*
   shows your **key**.
2. Create an applet: *If* Webhooks → *Receive a web request* with an event name
   such as `bageo_call`, *Then* whatever you like.

In the applet, `{{Value1}}` is the title, `{{Value2}}` the message and
`{{Value3}}` the event (`summary`, `incoming`, …).

| Field | Value |
|---|---|
| Webhooks key | the key from step 1 |
| Event name | `bageo_call` |

IFTTT may ask for a paid plan for applets that use Webhooks.

### Apprise API

If you already run [Apprise API](https://github.com/caronc/apprise-api), it
reaches a hundred more services:

| Field | Value |
|---|---|
| Notify URL (…/notify/KEY) | `http://apprise.local:8000/notify/<your config key>` |
| Tag (optional) | a tag to send to |

### Webhook (JSON)

For n8n, Zapier, Make, Home Assistant, Node-RED or your own code. Bageo `POST`s
JSON to your URL.

With a *Bearer token (optional)* set, it adds `Authorization: Bearer <token>`.

```json
{
  "event": "summary",
  "title": "Call summary 0912345678",
  "body": "Call summary\nNumber: 0912345678\n…",
  "text": "(same as body)",
  "data": {
    "caller_id": "0912345678",
    "device": "Pixel 8",
    "time": "Nov 12, 18:02",
    "summary": "Caller: Jay (friend)\nAbout: …",
    "contact": ""
  }
}
```

| `event` | When | `data` |
|---|---|---|
| `incoming` | a call was picked up | `caller_id`, `device`, `time` |
| `outgoing` | Bageo's own call started ringing | `caller_id` (the number called), `device`, `time` |
| `blocked` | a blocked number called | `caller_id`, `device`, `time`, `reason`, `declined` (`true`: refused while it rang; `false`: answered with the announcement) |
| `summary` | a few seconds after a call ended | `caller_id`, `device`, `time`, `summary`, `contact` (the contact's name, or empty) |
| `call_waiting` | a second call rang during a call and was declined | `caller_id`, `device`, `time`, `sim_country` |
| `repeat_calls` | one number kept calling and the extra notifications were held back | `caller_id`, `count`, `kind` (`incoming`, `blocked` or `call_waiting`) |
| `ai_callback` | the AI's call-back for an appointment ended, or could not be placed | `caller_id`, `device`, `time`, `summary` |
| `ai_errand` | an AI errand ended, or never got going | `number`, `device`, `time`, `summary` |
| `appointment_pending` | a caller booked an appointment waiting for your decision | `task_id`, `caller_id`, `device`, `title`, `start_at`, `accept_url`, `decline_url` |
| `whitelist_direct_transfer` | a call was declined so your carrier forwards it to your phone | `caller_id`, `device`, `time`, `forward_to`, `contact` |
| `whitelist_urgent_transfer` | the caller said it was urgent and you were alerted at once | `caller_id`, `device`, `time`, `forward_to`, `reason`, `contact` |
| `voicemail` | a voicemail was saved (older menu mode) | `caller_id`, `device`, `time`, `seconds` |
| `event` | a message sent through the API (`/calls/api/notify/send`) | `null` |
| `test` | **Send test** | `{}` |

A `2xx` answer counts as delivered. For everything else about a call
(transcript, recordings), fetch it from the [API](API.md) with the call's ID.

## Sending your own messages

Scripts on your network can use the same channels, for example a garage door
sensor:

```text
POST /calls/api/notify/send        {"title": "Garage", "body": "Door left open"}
```

- It needs your console password like every API call (see [API](API.md)).
- `body` is required; `title` defaults to "Bageo".
- The answer is `{"ok": true}` when at least one channel took the message.
- These messages go out even with *Send notifications* off.

The optional [SMS forwarder](../../tools/sms_forward/README.md) uses this route
to pass on the phone's text messages.

## Set through the environment instead

For containers and scripted set-ups, three routes can be given at start-up
instead of in the web console. They are used in addition to the console's
channels:

| Variables | Route |
|---|---|
| `TELEGRAM_BOT_TOKEN` + `TELEGRAM_CHAT_ID` | a Telegram bot; also the one that sends appointment cards. Not sent twice when the same bot and chat are also a console channel. |
| `NOTIFY_WEBHOOK_URL` | the [JSON webhook](#webhook-json) above, without a token |
| `NOTIFY_GATEWAY_URL` + `NOTIFY_GATEWAY_TAG` | an Apprise API base address; messages go to `<URL>/notify/<tag>`, tag `normal` when empty |

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
