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
| Language | The language the AI speaks and the language of notifications and summaries. English, 繁體中文 (Taiwan), 简体中文, 日本語, 한국어, Español, Français, Deutsch, Português. |
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
| Answer hidden numbers | On: callers without caller ID are answered like anyone else. Off: they are hung up on. |
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
number you have dialed is not available" announcement in the AI's voice and is
hung up on; the AI and Gemini are not used. `*` at the end matches a prefix of
at least four digits (`0800*`). Write numbers the way they show up in the call
list; `+…` forms match when *Country calling code* is set.

The phone app cannot refuse a ringing call, so these callers see the call
connect for a few seconds.

### Notifications

See [Notifications](NOTIFICATIONS.md). *Address of this console* is the address
put into notification links (e.g. `http://192.168.1.20:8787`); leave it empty
for no links.

### Summary & calendar

| Setting | What it does |
|---|---|
| Summary service | By default the summary is written with your Gemini key (model `gemini-flash-latest`). To use another service give its OpenAI-compatible **base URL** (e.g. `https://api.openai.com/v1`, `https://api.deepseek.com/v1`, `https://openrouter.ai/api/v1`, or a local Ollama: `http://127.0.0.1:11434/v1`), a **model** name, and the **API key** (not needed for a local server). A DeepSeek key alone (no base URL) uses DeepSeek. |
| Suggest calendar events | When a call fixes a date — a delivery window, an appointment, a time someone will call back — the summary gets an "add to calendar" link (Google Calendar form, plus an `.ics` download in the console). One extra request to the summary model per call. Nothing is added to any calendar by itself. |
| Add to Google Calendar automatically | Inserts the event right away. Needs Google OAuth credentials (client ID, client secret, refresh token with the `calendar.events` scope, calendar ID or `primary`) — for people who already have a Google Cloud project; everyone else is better served by the link. |

Without any key for the summary, the notification carries the caller's first
words instead, and the transcript is always in the console.

### Region & advanced

| Setting | What it does |
|---|---|
| Country calling code | Your SIM's country calling code without `+` (`886`, `1`, `44`, `81`). Lets blocked numbers written as `+44 20…` match the national form the phone reports. With the language set to 繁體中文 and this left empty, `886` is assumed. |
| Wait for the carrier's warning on international calls | Taiwan's carriers play a recorded warning at the start of every call from abroad. *Auto* (on for 繁體中文) makes the AI wait for it instead of answering it. Elsewhere leave it on *auto* or *off*. |
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
| `PUBLIC_URL` | *(empty)* | Same as *Address of this console*; the web console's value wins. |

### Using the console from other devices

By default only the computer itself can open the console. To reach it from
your phone or another computer on your home network:

```ini
AGENT_HOST=0.0.0.0
CALLS_UI_TOKEN=choose-a-long-random-string
DEVICE_TOKEN=another-long-random-string
PUBLIC_URL=http://192.168.1.20:8787
```

Open `http://192.168.1.20:8787/?token=choose-a-long-random-string` once; the
page remembers the token. The connection is plain HTTP — fine on a home network
you trust, not for the open internet. To reach it from outside, use a VPN
(WireGuard, Tailscale) or a reverse proxy with HTTPS and its own login; do not
forward the port.

### Data

| Variable | Default | Meaning |
|---|---|---|
| `CALLS_DATA_DIR` | `data/calls` | Call records, recordings, settings. |
| `CALLS_RETENTION_DAYS` | `7` | Calls older than this are deleted, records and audio (1–3650). |
| `ASSETS_DIR` | `assets` next to the program | Folder with the bundled sounds. |

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
`GEMINI_API_KEY`, `APP_LANGUAGE` (`en`, `zh-TW`, …), `OWNER_NAME`,
`ANSWER_STYLE` (`assistant` / `persona`), `GEMINI_LIVE_MODEL`,
`GEMINI_VOICE_NAME`, `COUNTRY_CODE`, `SUMMARY_API_BASE`, `SUMMARY_MODEL`,
`DEEPSEEK_API_KEY` (the summary API key), `TELEGRAM_BOT_TOKEN`,
`TELEGRAM_CHAT_ID`, `NOTIFY_WEBHOOK_URL`.

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
