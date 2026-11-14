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
| Language of notifications and summaries | The language of what you read: notifications, call summaries (also in the call list), appointment cards and their results, messages from the SMS forwarder. Default *Same as the AI's language*. Set it when callers should hear another language than the one you read — say you are abroad and the AI answers in English, while your notifications stay in 繁體中文. The summary is written in this language whatever language the call was in. Switching the console's display language (top right) sets this to the same language. |
| Main language for each SIM | For a phone with SIMs from more than one country. Pick a SIM's country and the language calls on it start in — say *United States (US)* → English, while *Language* stays 繁體中文. A caller whose number is from another country than the SIM's still hears that country's language when the AI speaks it (a Japanese number calling the US SIM hears Japanese). SIMs the phones have reported since the agent started are listed already; on *Same as the AI's language* nothing changes. Needs phone app 1.0 or later, which reports the SIM of each call. Saved as `sim_languages`, e.g. `{"us": "en"}`. |
| Your name | How callers refer to you. Used wherever a prompt says `{owner}`. |
| Other names callers use for you | One per line, up to 10: other names callers may use for you, such as an English name, a nickname or how colleagues address you. When the AI hears one, it knows they mean you — in both answer styles, and with your own prompt too. Call summaries still write *Your name*. Saved as `owner_aliases` (a list); first-run default `OWNER_ALIASES`, comma separated. |
| Answer style | **AI assistant** — says it is an AI answering for you and takes a message. **As me** — answers as you. |
| AI calls: call as | Who the AI says it is when it makes a call for you from the **Errands** page ([AI errands](#ai-errands)): **Your assistant** (default) — it says it is calling on your behalf; **You in person** — it speaks as you. Each errand can still pick its own. In both cases it admits to being an AI when sincerely asked. Saved as `errand_identity` (`assistant` / `owner`; anything else reads as `assistant`); first-run default `ERRAND_IDENTITY`. |
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
| Reply in the caller's language | Always on: when a caller clearly speaks another language, or asks whether you speak one, the AI switches to it. The old setting `reply_in_caller_language` is no longer used. |
| Status note | One line about right now ("expecting a delivery today"); mentioned only when relevant. |
| Background sound | A quiet room tone under the AI's voice: off, home, office, street, hospital, ambulance passing, typhoon, thunderstorm, construction, metro, night market, firecrackers, car. Three levels. |

**International calls and the carrier's warning.** Taiwan's carriers play a
recorded fraud warning at the start of every call from abroad. When a
Taiwanese SIM takes a call from another country (a `+…` or `00…` number with
another country code), the AI waits for that warning to end: it speaks once
the line goes quiet, or after 7.5 seconds at most, and says its opening line
only once. Calls on any other SIM (say, a US SIM in a dual-SIM phone) carry no
warning, so the AI answers at once. This is decided automatically; there is
nothing to set. A phone app older than 1.0 (0.2.4 and earlier) does not say which SIM took the
call; a call from abroad is then a `+…` number whose country code is not
yours ([`COUNTRY_CODE`](#country-calling-code)). The old setting
`intl_carrier_prompt` is gone: a value saved by an older version is ignored
and dropped at the next save.

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

A list of numbers with an optional reason. Such a caller never reaches the AI or
Gemini. *How blocked calls are turned away* (`reject_mode`): **Hang up while it
rings** (the default; the call never connects), or **Answer, play "not
available", then hang up**: a short "the number you have dialed is not
available" announcement in the language you chose, in the AI's voice unless you
pick another under *Announcement voice* (every voice comes recorded in every
language, so it costs no Gemini quota); these callers see the call connect for a
few seconds. `*` at the end matches a prefix of at least four digits (`0800*`).
Write numbers the way they show up in the call list; `+…` forms match when
your country calling code is known ([`COUNTRY_CODE`](#country-calling-code)).
A phone app too old to decline picks up and announces either way. Both leave
the call in the call list and send a "⛔" notification.

### AI errands

The **Errands** page has the AI make a call for you: pick a contact or type a
number, write what to get done and what you can accept ("book a table for 4
tomorrow at 7 pm; 7:30 to 8 is fine too"), choose *Call as* and, if you like,
the phone to call from, then **Start the call**. While the call runs the page
shows live captions; you can type an instruction for the AI, answer a question
it asks you, or **Hang up**. Afterwards the errand shows the result (done,
partly done, not done, your decision needed, no answer, voicemail, didn't get
through) with details, the next step, the time agreed, a link to the call
record and **Call again**; when the other side offered choices, you can pick
one and have the AI call back with it. You also get one notification
("📞 AI errand call: <name or number>").

What it needs and how it behaves:

- A connected phone in AI mode with a SIM of the number's country. The agent
  learns each phone's SIMs from the calls the phone reports, so a phone that
  has never taken a call since it was set up cannot be used yet (*This phone
  hasn't taken a call yet*): call it once.
- It never dials an emergency number, a short code (7 digits or fewer) or a
  premium-rate number; toll-free numbers are fine. Write numbers abroad with
  `+`; a number without `+` is one of your country.
- One call to a number at a time (shared with the appointments' AI
  call-backs). A busy phone makes the errand wait up to 10 minutes; a call
  lasts 10 minutes at most (the AI wraps up at 9:30). The AI cannot press keys:
  it waits up to 90 seconds for a person in a voice menu, and hangs up at once
  on a carrier recording ("the number you dialed…").
- When something comes up that your instructions do not settle, the AI tells
  the other side it will check and asks you on the page (up to 40 seconds while
  you are watching). If you are not watching or do not answer, it agrees to
  nothing about it: it notes their options, says you will confirm and call
  back, and ends politely.
- It agrees to no payment, deposit or transfer and gives out no personal
  details beyond the errand. It never redials on its own.

The default for *Call as* is the [Basics](#basics) setting `errand_identity`.
The full rules, every status and error code are in
[API → AI errands](API.md#ai-errands).

### Notifications

See [Notifications](NOTIFICATIONS.md). *Address of this console* is the address
put into notification links (e.g. `http://192.0.2.10:8787`). Left empty, it
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
| Summary service (`summary_provider`) | Two choices. **Gemini** (`gemini`, the default): your Gemini key on Gemini's lowest-cost model, `gemini-flash-lite-latest` (`gemini-flash-latest` is the fallback; a Gemini model name saved in `summary_model` is tried first). Nothing else to fill in, so the API fields stay hidden. **Another service** (`custom`): shows the three fields of the next row, for any OpenAI-compatible service. Without a Gemini key, *Gemini* works like *Another service*. Settings from older versions are converted once, when they load, and saved that way: `deepseek` becomes `custom`; `auto` (or none) becomes `custom` when a base URL or a summary API key is set, else `gemini`. Calendar suggestions use the same service. |
| Another service | Three fields. **API base URL**: the service's OpenAI-compatible address, e.g. DeepSeek `https://api.deepseek.com/v1` (model `deepseek-chat`), OpenAI `https://api.openai.com/v1` (e.g. `gpt-4o-mini`), OpenRouter `https://openrouter.ai/api/v1`, or a local Ollama `http://127.0.0.1:11434/v1`. **Model**: required when a base URL is set. **API key**: stored like the Gemini key, shown only as a hint; not needed for a local server. With no base URL: DeepSeek when an API key is entered, else Gemini. Saved as `summary_api_base`, `summary_model` and `deepseek_api_key` (the name is historical; it holds the key of whatever service you use). |
| Suggest calendar events | When a call fixes a date — a delivery window, an appointment, a time someone will call back — the summary gets an "add to calendar" link for the calendar you chose (see *Add to calendar*). One extra request to the summary model per call. Nothing is added to any calendar by itself. |
| Add to calendar | Not a field of its own: the first time you press **Add to calendar** in the console (on a calendar suggestion or an accepted appointment) it asks which calendar — Google Calendar, Outlook.com, Microsoft 365 (Outlook with a work or school account), Apple Calendar (the `.ics` file: iPhone Safari offers to add it), Yahoo Calendar, or a plain `.ics` download — and remembers it (setting `calendar_app`); later presses go straight there, and **Change** next to the button picks another. Notifications and Telegram replies link the same calendar; for Apple Calendar or `.ics` that link is the event's `.ics` at *Address of this console* (without that address there is no link; with an access token set, the link carries a key for that one file, never the token). No calendar account is connected to the agent. |
| Appointments | Off by default. When a caller wants to set a time with you (a meeting, a visit, a call at a set time), the AI does not agree on the call and never asks the caller to wait: it says it will check and text them back later — "call you back later" when the answer goes by a call-back, only "get back to you" when neither can reach them (*Default reply* below decides which, by the caller's number). After the call the appointment waits under **To do** at the top of the web console's Calls page (and in the call's details): who, what and when, how the caller hears your answer — **Text**, **AI call-back** or **Don't tell them**, preset from *Default reply* (a text only for numbers that take one, a call-back only for numbers a call-back may dial, see the next row) — and **Accept** / **Decline**. After the press it stays there with what happened, in words: "Accepted, and the caller got a text", why a text did not go out, or how far the call-back got; an accepted one gets **Add to calendar**. With a Telegram bot set up and *Send notifications* on you also get a card with **Accept** / **Decline** (it uses *Default reply*; the bot's answer links *Add to calendar*); other notification channels get a reminder line with a link to the call. Only the first decision counts: a second one — even at the same moment, on the card, or through the API — changes nothing, and a card whose appointment was decided elsewhere loses its buttons. A back-up time the caller only mentions ("if not, the afternoon works too") does not replace the agreed one; a second, separate appointment on the same call (another matter, another day) is kept apart, with its own decision. Never with sales or scam callers: when a call is judged one — during the call, or afterwards because the summary marks it "Sales call" or "Suspected scam" (a bank's fraud desk or the police calling about fraud does not count; when no summary model answered: because the caller's words add up to one) — or when the AI could not connect and the call was hung up, what it agreed to no longer counts and nothing waits for you. Works with both answer styles (as you, or as your assistant). The appointment replaces the calendar suggestion for that call. An `auto` saved by an older version (when Google Calendar could be connected) now works as **Ask me first**. |
| Text the caller | Off by default. Lets the phone that took the call text the caller your answer, in your name, after you accept or decline, e.g. "Hi, it's Alex. Sat, Nov 14, 3:00 PM works for me. See you then!" Only the time (with its weekday) is in it, never what the appointment is about: those are the caller's own words and could carry a link or a chat ID. It texts mobile numbers: Taiwan mobiles, every +1 number (the US and Canada, Google Voice too; not toll-free or 900 numbers) and other countries' mobile numbers; a number without a country code is read the way the SIM that took the call reads it. Needs a phone app that sends texts (its control hello says `supports_sms`); when a text did not go out, *To do* and the Telegram reply say why in words, and the appointment has **Send the text again**, from the phone that took the call or another connected one. |
| Default reply | **Automatic** (default): a text when one can reach the caller (a mobile, *Text the caller* on), else a call-back from the AI (a landline or a number that takes no texts — so with *Text the caller* off, mobiles get a call-back too), else nothing: a hidden number, or one a call-back may not dial — only numbers of the answering SIM's own country with a real phone number's length are called back (in Taiwan 8–9 digits after the 0); never toll-free (0800, +1 800…888), premium (0204, 0209, +1 900), short codes (165, 110) or numbers from abroad. **Always a text** / **Always an AI call-back**: only that way, else nothing. It presets the choice under *To do* (a way that cannot reach the number is greyed out), is what a Telegram button press uses, and decides what the AI promises on the call. The AI call-back goes out on the agent's call-back route — the phone that took the call when it is connected, on the SIM the call came in on — and only to that caller's own number; call-backs to one number go one after another. The AI does not talk over the ringing: it waits until the callee has picked up and spoken (nobody for 70 s: no answer), then says who is calling, gives your answer (the time and what it is about, or a polite no), says goodbye once and the call ends. That call is listed as *AI call-back* (no calendar suggestion and no appointment from it; its notification is titled "📞 AI call-back: <number>"). *To do* shows how it went (waiting for an earlier one, calling, answered, nobody answered, could not be placed); one that could not be placed is also sent to your notification channels. |

Times are the computer's local time (`TZ` below).

The AI call-back speaks the language of the original call (the call's
`call_lang`, e.g. `en` for an English-speaking caller on a 繁體中文 setup), says
your answer once the callee has picked up, and never repeats it in a loop.
Texts and call-backs go out from the SIM the call came in on (`sub_id`), so a
dual-SIM phone answers from the right number. There is no test or demo
calendar any more: a `settings.json` from an older version that still holds
test-number keys or Google Calendar credentials (`gcal_*`) keeps working, and
those keys are dropped at the next save.

Without any key for the summary, the notification carries the caller's first
words instead, and the transcript is always in the console.

### Advanced

| Setting | What it does |
|---|---|
| Thinking level | For Live models with "thinking" in their name only: low / medium / high. Low answers fastest. Saved as `gemini_thinking_level`. |

This is the only field left under *Advanced*. *Country calling code* left the
console (see [Country calling code](#country-calling-code)), and the carrier's
warning on international calls is handled automatically (see
[How it answers](#how-it-answers)).

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
computer's IP, e.g. `http://192.0.2.10:8787/?token=…`) once; the page
remembers the token. *System → About* shows the address with a QR code to scan
with your phone, and how to add the console to the home screen: iPhone *Share →
Add to Home Screen*, Android Chrome menu ⋮ → *Add to Home screen*, Chrome or
Edge on a computer the install icon in the address bar.

If this computer is on a VPN that sends all traffic through the tunnel, the
agent may take the VPN's address for the name and the links. Set `AGENT_HOST`
to the computer's LAN IP (e.g. `AGENT_HOST=192.0.2.10`); the name and the
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

### Country calling code

`COUNTRY_CODE`: your own country's calling code, digits only, without `+`
(`1`, `44`, `81`). It is how the agent reads a number written without a
country code: blocked numbers (so `+44 20…` matches the national form the
phone reports), contacts, AI errands, and calls from a phone app older than 1.0 (0.2.4 and earlier), which does not say which SIM took the call. A caller from another
country is transcribed with their country's language first (or, when the AI
does not speak it, with the language detected), so their goodbye is not lost.

Empty = `886` when the language is 繁體中文, else none. Set it only when your
language does not point to a single country (English, Spanish, …). It is not
in the web console; besides the environment variable, `country_code` in
`settings.json` or `PUT /calls/api/settings` sets it. A `country_code` in
`settings.json` (including one saved from the console by an older version,
even an empty one) wins over `COUNTRY_CODE`; delete that line to go back to the
environment variable.

### First-run defaults

These only set the starting value; the web console overrides them:
`GEMINI_API_KEY`, `APP_LANGUAGE` (`en`, `zh-TW`, …), `NOTIFY_LANGUAGE` (empty =
the same as `APP_LANGUAGE`), `OWNER_NAME`, `OWNER_ALIASES` (comma separated),
`ANSWER_STYLE` (`assistant` / `persona`), `GEMINI_LIVE_MODEL`,
`GEMINI_VOICE_NAME`, `ERRAND_IDENTITY` (`assistant` / `owner`),
`SUMMARY_PROVIDER` (`gemini` / `custom`; the older `deepseek` and `auto` as
in *Summary service*), `SUMMARY_API_BASE`, `SUMMARY_MODEL`,
`DEEPSEEK_API_KEY` (the summary API key), `TELEGRAM_BOT_TOKEN`,
`TELEGRAM_CHAT_ID`, `NOTIFY_WEBHOOK_URL`, `APPOINTMENT_MODE` (`off` /
`confirm`), `APPOINTMENT_SMS` (`true` / `false`), `APPOINTMENT_REPLY`
(`auto` / `sms` / `call`).

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
