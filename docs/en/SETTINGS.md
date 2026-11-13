# Settings reference

There are two kinds of settings:

- **In the web console** (*Settings*): everything about how calls are answered.
  Saved to `data/calls/config/settings.json`, applied to the next call, no
  restart.
- **Start-up options**: where the agent listens, where data lives, the USB
  link. Read once at start from environment variables or a `.env` file next to
  the start script (copy `.env.example`).

## In the web console

### Basics

| Setting | What it does |
|---|---|
| Language | The language the AI speaks with callers: what it says, its first words, the blocked-number announcement and the texts it sends callers. English, 繁體中文 (Taiwan), 简体中文, 日本語, 한국어, Español, Français, Deutsch, Português. |
| Language of notifications and summaries | The language of what you read: notifications, call summaries (also in the call list), appointment cards and their results, messages from the SMS forwarder. Default *Same as the AI's language*. Set it when callers should hear another language than the one you read — say you are abroad and the AI answers in English, while your notifications stay in 繁體中文. The summary is written in this language whatever language the call was in. |
| Your name | How callers refer to you. Used wherever a prompt says `{owner}`. |
| Answer style | **AI assistant** — says it is an AI answering for you and takes a message. **As me** — answers as you. |
| AI answers calls | Master switch. Off: calls are picked up and hung up at once, nothing is said. (To have one phone ring normally so you can answer it yourself, switch it to a [normal phone](PHONE.md#normal-phone-mode) instead.) |
| Gemini API key | Your key from Google AI Studio. Stored on your computer only. |
| Live model | The Gemini Live model. Default `gemini-3.8-live`. Models with "thinking" in the name answer more carefully and a little slower. |
| Voice | One of Gemini's built-in voices; ▶ plays a sample in your language. |

### How it answers

| Setting | What it does |
|---|---|
| Ring before answering | How long the caller hears ringing (default 3 s, up to 8 s). The AI gets ready during this time. |
| Get the AI ready while the phone rings | Connects to Gemini and has the first words ready before pick-up, so the AI speaks at once. Costs one short Gemini session even when the caller hangs up while it rings. |
| Answer hidden numbers | On: callers without caller ID are answered like anyone else. Off: they hear the same short "not available" announcement as blocked numbers (in your language and the *Announcement voice*), and the call ends. The announcement comes recorded with the program, so no Gemini key is needed. |
| Let the caller interrupt | On phones where it is possible (see [Phone setup](PHONE.md#which-phones)), the AI stops talking when the caller cuts in. |
| Let Gemini decide when the caller has finished | On (recommended): Gemini's own voice detection decides when it is the AI's turn. Off: a simpler detector in the agent does. |
| Longest normal call | The AI starts wrapping up about 25 seconds before this (3–30 minutes, default 5). |
| Reply in the caller's language | When a caller clearly speaks another language, the AI switches to it. |
| Status note | One line about right now ("expecting a delivery today"); mentioned only when relevant. |
| Background sound | A quiet room tone under the AI's voice: off, home, office, street, hospital, ambulance passing, typhoon, thunderstorm, construction, metro, night market, firecrackers, car. Three levels. |

### Sales & scam calls

| Setting | What it does |
|---|---|
| Keep them talking | Once the caller's words show a sales pitch or scam, the AI plays an interested but slow customer to keep them busy. Never gives data, never agrees. |
| Longest such call | 3–30 minutes (default 30). |
| Model for these calls | Model to switch to for the stalling part; empty = the same model. |

### Prompt

See [Writing the prompt](PROMPTS.md): your own prompt (empty = built-in), notes
the AI may answer from, the interview prompt.

### Blocked numbers

A list of numbers with an optional reason. Such a caller hears a short "the
number you have dialed is not available" announcement and is hung up on; the AI
and Gemini are not used. The announcement is in the language you chose, in the
AI's voice unless you pick another under *Announcement voice*; every voice comes
recorded in every language, so it costs no Gemini quota. `*` at the end matches a prefix of
at least four digits (`0800*`). Write numbers the way they show up in the call
list; `+…` forms match when *Country calling code* is set.

The phone app cannot refuse a ringing call, so these callers see the call
connect for a few seconds.

### Notifications

See [Notifications](NOTIFICATIONS.md). *Address of this console* is the address
put into notification links (e.g. `http://192.168.1.20:8787`). Left empty, it
is filled in while the console is open to your network (`AGENT_HOST=0.0.0.0`):
`http://<network name>.local:8787/` while the agent answers that name, else
the computer's network address. With the console on this computer only,
notifications carry no links; inside Docker nothing is filled in, set it
yourself. *System → About* shows the address in use.

*Network name* (default `bageo`) is the name phones and computers on your
network can use instead of the IP: `http://bageo.local:8787/`. Letters, digits
and hyphens; empty, `off`, `0`, `none` or `false` turns it off. The agent
answers the name itself (multicast DNS, UDP port 5353) while the console is
open to your network, sharing the port with the system's own responder
(Bonjour on macOS, avahi on Linux, the DNS client of Windows). Some systems
still keep the port to themselves: the agent then logs a line starting with
`mDNS:` and the links use the IP address. It does not start inside a
container. Some older Android phones do not open `.local` names at all; under
a `.local` address *System → About* also shows the IP address, and the QR
code works as well. Before the project was renamed the default was `myna`: a
name saved in the console stays as it is; otherwise the address becomes
`bageo.local`, and to keep `http://myna.local:8787/` set the name to `myna`.

### Summary & calendar

| Setting | What it does |
|---|---|
| Summary service | **Automatic** (default): the service under *Use another service for summaries* when one is set, else DeepSeek when its key is entered, else Gemini with your Gemini key (model `gemini-flash-latest`). **Gemini (cheap)**: always Gemini with your Gemini key, on its lowest-cost model `gemini-flash-lite-latest` (a *Model* typed below is tried first; `gemini-flash-latest` is the fallback). **DeepSeek**: always DeepSeek. A choice without its key works like *Automatic*. Calendar suggestions use the same service. |
| Use another service for summaries | Give the service's OpenAI-compatible **base URL** (e.g. `https://api.openai.com/v1`, `https://api.deepseek.com/v1`, `https://openrouter.ai/api/v1`, or a local Ollama: `http://127.0.0.1:11434/v1`), a **model** name, and the **API key** (not needed for a local server). A DeepSeek key alone (no base URL) uses DeepSeek. The base URL is used when *Summary service* is *Automatic*. |
| Suggest calendar events | When a call fixes a date — a delivery window, an appointment, a time someone will call back — the summary gets an "add to calendar" link (Google Calendar form, plus an `.ics` download in the console). One extra request to the summary model per call. Nothing is added to any calendar by itself. |
| Add to Google Calendar automatically | Inserts the event right away. Needs Google OAuth credentials (client ID, client secret, refresh token with the `calendar.events` scope, calendar ID or `primary`) — for people who already have a Google Cloud project; everyone else is better served by the link. |
| Appointments | Off by default. When a caller wants to set a time with you (a meeting, a visit, a call at a set time), the AI looks at your Google Calendar — only whether you are free, never what is in it — without saying it is checking or asking the caller to wait. Busy: it says you already have something then, and suggests another time only if asked. Free: **Ask me first** — it says it should be fine and that you will confirm; after the call you decide with **✅ Confirm** / **❌ Decline** under *Appointments to confirm* at the top of the web console's Calls page, or in the call's details (✅ puts it in your calendar). With a Telegram bot set up and *Send notifications* on you also get a card you can tap; other notification channels get a reminder line with a link to the call; with no notifications set up it only shows in the console. Only the first decision counts: a second one — even at the same moment, on the card, or through the API — changes nothing, and a card whose appointment was decided elsewhere loses its buttons. **Book it if I'm free** — it agrees and the event goes into your calendar during the call (if that fails, it waits for your decision instead). When the two of you change the time later in the call, that event is moved — there is never a second one; a back-up time the caller only mentions ("if not, the afternoon works too") does not replace the agreed one. Never with sales or scam callers: when a call is judged one — during the call, or afterwards because the summary marks it "Sales call" or "Suspected scam" (a bank's fraud desk or the police calling about fraud does not count; when no summary model answered: because the caller's words add up to one) — or when the AI could not connect and the call was hung up, what it agreed to no longer counts, no card comes, and an event it already put in your calendar is deleted after the call (the summary says so); if that fails, the summary gets a line asking you to delete it yourself. Works with both answer styles (as you, or as your assistant). The appointment replaces the calendar suggestion for that call. |
| Text the caller | Off by default. After you confirm or decline (or the AI books it), the phone that took the call texts the caller a short message in your name, e.g. "Hi, it's Alex. Sat, Nov 14, 3:00 PM works for me. See you then!" Only the time (with its weekday) is in it, never what the appointment is about: those are the caller's own words and could carry a link or a chat ID. Only Taiwan mobile numbers (`09…` / `+8869…`); for landlines, hidden numbers and foreign numbers the card says to call them back yourself. Needs a phone app that sends texts (its control hello says `supports_sms`); the result (in the console, and the Telegram reply) tells you when a text did not go out and why. |

Checking free times and writing to the calendar need the Google Calendar
credentials above. Without them the AI cannot see your calendar: it always
says it will confirm and get back to the caller, and *Book it if I'm free*
works like *Ask me first*. For free/busy checks the refresh token also needs a
read scope: `calendar.freebusy` (or `calendar.readonly`, or the full
`calendar` scope) next to `calendar.events`. A token made with
`calendar.events` alone is refused ("insufficient scope"); create it again
with both scopes. Times are the computer's local time (`TZ` below).

Without any key for the summary, the notification carries the caller's first
words instead, and the transcript is always in the console.

### Region & advanced

| Setting | What it does |
|---|---|
| Country calling code | Your SIM's country calling code without `+` (`886`, `1`, `44`, `81`). Lets blocked numbers written as `+44 20…` match the national form the phone reports. With the language set to 繁體中文 and this left empty, `886` is assumed. |
| Wait for the carrier's warning on international calls | Taiwan's carriers play a recorded warning at the start of every call from abroad. On *auto* the AI waits for it only when a Taiwanese SIM takes a call from abroad; calls on any other SIM (say, a US SIM in a dual-SIM phone) have no warning, so the AI answers at once. Phone app 0.2.5+ reports which SIM took the call; with an older app, *auto* goes by the language (繁體中文 = a Taiwanese SIM). *On* waits on every call from abroad, *off* never. |
| Thinking level | For "thinking" Live models only: low / medium / high. |

## Start-up options

Set as environment variables or in `.env`. All are optional.

### Network and access

| Variable | Default | Meaning |
|---|---|---|
| `AGENT_HOST` | `127.0.0.1` | Address to listen on. `127.0.0.1` = this computer only. `0.0.0.0` = every network interface. |
| `AGENT_PORT` | `8787` | Port of the web console and of the phones' connection. |
| `CALLS_UI_TOKEN` | *(empty)* | Password for the web console and API. Send as `?token=…`, `Authorization: Bearer …` or `X-Calls-Token`. **Set it whenever `AGENT_HOST` is not `127.0.0.1`.** |
| `DEVICE_TOKEN` | *(empty)* | Phones that do not come through USB must connect with `…/ws?token=<this>`. **Set it whenever `AGENT_HOST` is not `127.0.0.1`.** Phones on USB are exempt (the USB tunnel ends on the computer itself). |
| `PUBLIC_URL` | *(empty)* | Same as *Address of this console*; the web console's value wins. Empty = automatic (see [Notifications](#notifications)). |
| `MDNS_NAME` | `bageo` | Same as *Network name*: the console answers at `http://<this>.local:8787/` on your network. Empty, `off`, `0`, `none` or `false` = off; on Windows an empty value counts as not set, so write `MDNS_NAME=off`. The web console's value wins. |

### Using the console from other devices

By default only the computer itself can open the console. To reach it from
your phone or another computer on your home network:

```ini
AGENT_HOST=0.0.0.0
CALLS_UI_TOKEN=choose-a-long-random-string
DEVICE_TOKEN=another-long-random-string
```

Open `http://bageo.local:8787/?token=choose-a-long-random-string` (or the
computer's IP, e.g. `http://192.168.1.20:8787/?token=…`) once; the page
remembers the token. *System → About* shows the address with a QR code to scan
with your phone, and how to add the console to the home screen: iPhone *Share →
Add to Home Screen*, Android Chrome menu ⋮ → *Add to Home screen*, Chrome or
Edge on a computer the install icon in the address bar.

If this computer is on a VPN that sends all traffic through the tunnel, the
agent may take the VPN's address for the name and the links. Set `AGENT_HOST`
to the computer's LAN IP (e.g. `AGENT_HOST=192.168.1.20`); the name and the
links then use that address.

The connection is plain HTTP — fine on a home network
you trust, not for the open internet. To reach it from outside, use a VPN
(WireGuard, Tailscale) or a reverse proxy with HTTPS and its own login; do not
forward the port.

### Data

| Variable | Default | Meaning |
|---|---|---|
| `CALLS_DATA_DIR` | `data/calls` | Call records, recordings, settings. |
| `CALLS_RETENTION_DAYS` | `7` | Calls older than this are deleted, records and audio (1–3650). |
| `ASSETS_DIR` | `assets` next to the program | Folder with the bundled sounds. |

### Time zone

| Variable | Default | Meaning |
|---|---|---|
| `TZ` | *(the computer's own)* | Time zone of the times in notifications (`Nov 13, 08:09`, written the way your language writes dates), of "today" in the call list and of calendar suggestions. A zone name such as `Asia/Taipei`, `Europe/Berlin` or `America/New_York`. Set it only when the computer the agent runs on is not in your zone. The current value is in `/health` as `local_time`. |

In Docker, `docker-compose.yml` passes the host's zone into the container by
mounting `/etc/localtime` (Linux hosts) and leaves `TZ` unset; put
`TZ=Asia/Taipei` in `.env` to use another zone. On Docker Desktop for Windows
or macOS, remove the `/etc/localtime` line from `docker-compose.yml` and set
`TZ` in `.env`; with neither, the container runs on UTC. Leave `TZ` out
rather than empty: an empty `TZ` means UTC.

### Phones on USB

| Variable | Default | Meaning |
|---|---|---|
| `USB_BRIDGE` | `on` | `off`: the agent does not run `adb` at all (Wi-Fi phones, containers). |
| `ADB_BIN` | *(searched)* | Full path of `adb`. Search order otherwise: `platform-tools/` next to the program, `ANDROID_HOME`, the usual SDK folders, then `adb` on the `PATH`. If several versions are installed, pin one here — different versions restart each other's server and drop the phones. |
| `ADB_SERIALS` | *(all)* | Only manage these phones (serials from `adb devices`, comma separated). |
| `USB_PHONE_PORT` | `8765` | The port the app dials on the phone. Change only together with the app's address. |
| `USB_BRIDGE_INTERVAL_SEC` | `15` | How often the USB link is checked. |

### First-run defaults

These only set the starting value; the web console overrides them:
`GEMINI_API_KEY`, `APP_LANGUAGE` (`en`, `zh-TW`, …), `NOTIFY_LANGUAGE` (empty =
the same as `APP_LANGUAGE`), `OWNER_NAME`,
`ANSWER_STYLE` (`assistant` / `persona`), `GEMINI_LIVE_MODEL`,
`GEMINI_VOICE_NAME`, `COUNTRY_CODE`, `SUMMARY_PROVIDER` (`auto` / `gemini` /
`deepseek`), `SUMMARY_API_BASE`, `SUMMARY_MODEL`,
`DEEPSEEK_API_KEY` (the summary API key), `TELEGRAM_BOT_TOKEN`,
`TELEGRAM_CHAT_ID`, `NOTIFY_WEBHOOK_URL`, `APPOINTMENT_MODE` (`off` /
`confirm` / `auto`), `APPOINTMENT_SMS` (`true` / `false`).

### Logging and development

| Variable | Default | Meaning |
|---|---|---|
| `RUST_LOG` | `agent=info` | `agent=debug` for more detail. |
| `CALLS_UI_FILE` | *(embedded page)* | Serve the web console from this HTML file on every request (for working on the page). |

## Older modes

The code still contains the menu-based flows this project started with
(recorded greeting, "press 1", hold music, voicemail, a Tencent TRTC bridge, a
Whisper / Ollama / Piper pipeline, Telegram task cards). They are not shown in
the web console, ship without their recordings, and are not supported; the
environment variables you may find for them in the source (`AGENT_MODE`,
`IVR_*`, `TRTC_*`, `VM_*`, …) are best left alone.
