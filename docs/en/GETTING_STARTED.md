# Getting started: an AI phone assistant on your real phone number

This guide takes you from a fresh download to an AI that answers your phone
calls on your own number, and then to the first call the AI makes for you.
Bageo is self-hosted: a rooted Android phone holds your SIM, one program on
your computer streams each call to Gemini Live, and every recording and
transcript stays in a folder on your disk. Plan for about an hour; most of it
goes into preparing the phone.

1. [What you need](#1-what-you-need)
2. [Install and start the agent](#2-install-and-start-the-agent)
3. [Fill in the basics](#3-fill-in-the-basics)
4. [Prepare the phone](#4-prepare-the-phone)
5. [Make the first test call](#5-make-the-first-test-call)
6. [Get notified](#6-get-notified)
7. [Contacts, appointments and AI errands](#7-contacts-appointments-and-ai-errands)
8. [Keep it running](#8-keep-it-running)

## 1. What you need

| | |
|---|---|
| **A spare Android phone** | Rooted with Magisk or KernelSU, Android 10 or newer, with a SIM that receives calls. Tested on Google Pixel 7, 7a, 8 and 9 (see [which phones work](PHONE.md#which-phones)). Rooting is not covered here: follow a guide for your exact model, and remember that unlocking the bootloader wipes the phone. |
| **A computer** | Windows 10/11, macOS (Apple Silicon or Intel) or Linux, switched on whenever calls should be answered, plus a USB cable to the phone. Docker works too, with the phone on Wi-Fi. |
| **A Gemini API key** | Create one at <https://aistudio.google.com/apikey> and copy it. It is the only key Bageo needs: live calls, summaries, the scam review and contact import all use it. Google bills per use; set a budget in your Google account if you want a ceiling. |

## 2. Install and start the agent

The *agent* is the program on your computer. It talks to the phone and to
Gemini, keeps the call records and serves the web console. It is a single
program: no database, no other services to install.

### From a release (easiest)

1. Download the package for your system from the
   [Releases](https://github.com/h4lo-void/bageo-call/releases) page and unpack
   it anywhere, for example `Documents/bageo-call`:

   | System | Package |
   |---|---|
   | Windows | `bageo-call-1.0.0-windows-x64.zip` |
   | macOS, Apple Silicon | `bageo-call-1.0.0-macos-arm64.tar.gz` |
   | macOS, Intel | `bageo-call-1.0.0-macos-x64.tar.gz` |
   | Linux | `bageo-call-1.0.0-linux-x64.tar.gz` |

   Each one holds the agent, the bundled sounds, the start scripts and the
   documents.
2. Start it:
   - **Windows:** double-click `start.cmd`. If SmartScreen warns about an
     unknown publisher, choose *More info → Run anyway*.
   - **macOS:** double-click `start.command`. The program is not notarised, so
     the first time macOS may refuse: right-click `start.command` → *Open*, and
     if the agent itself is blocked, allow it under *System Settings → Privacy &
     Security*. Or run `xattr -dr com.apple.quarantine .` once in the folder.
   - **Linux:** run `./start.sh` in a terminal.
3. The script looks for `adb` (Google's Android platform-tools, needed for the
   USB link). If it is missing, the script offers to download it from Google
   into a `platform-tools` folder next to the program.
4. Your browser opens the web console at <http://localhost:8787/>. Leave the
   terminal window open: closing it stops the agent.
5. The first time the agent answers, the script puts a **Bageo Console**
   shortcut on the desktop (macOS: a `.webloc` file; Linux: an app menu entry,
   also copied to the desktop folder). It is not created again while it
   exists. To skip it, start with `-NoShortcut` (Windows) or `--no-shortcut`,
   or set `BAGEO_NO_SHORTCUT=1` in `.env`.

### From source

```text
git clone https://github.com/h4lo-void/bageo-call.git
cd bageo-call
./start.sh            # Windows: .\start.cmd
```

With [Rust](https://rustup.rs) installed, the start script builds the agent the
first time (a few minutes) and then starts it. After pulling new code, add
`--build` (PowerShell: `-Build`) to rebuild.

### With Docker

```text
cp .env.example .env        # set CALLS_UI_TOKEN and DEVICE_TOKEN
docker compose up -d --build
```

In a container the agent is reachable from your network, so both tokens are
required. The console is at `http://<host>:8787/?token=<CALLS_UI_TOKEN>`, and
phones connect over Wi-Fi to `ws://<host>:8787/ws?token=<DEVICE_TOKEN>` (see
[Phone setup → Wi-Fi](PHONE.md#wi-fi)). Inside a container the agent does not
answer to `bageo.local` and cannot work out the console address for
notification links, so set `PUBLIC_URL` in `.env` (for example
`http://192.168.1.20:8787`). The comments in `docker-compose.yml` cover the time zone.

### Where your data lives

Everything stays in the folder you started from (`/data/calls` in Docker):

| Path | Contents |
|---|---|
| `data/calls/` | call records, recordings, transcripts, contacts, to-do items, errands |
| `data/calls/config/settings.json` | everything you set in the web console, **including your API keys** |
| `.env` | optional start-up options (port, tokens); see [Settings reference](SETTINGS.md) |

Back up or delete `data/` as you like. Nothing is stored anywhere else.

## 3. Fill in the basics

Open *Settings* in the web console.

1. **Language**: the language the AI speaks. *Language of notifications and
   summaries* can differ: the AI can answer in English while you read
   繁體中文. (The console's own language is the switch in the header.)
2. **Your name**: the way callers say it ("Alex", "Ms. Chen", "王先生").
3. **Other names callers use for you**: one per line, up to 10. An English
   name, a nickname, a short form ("Lex"). When the AI hears one, it knows the
   caller means you.
4. **Answer style**
   - *AI assistant*: "Hello, this is Alex's AI assistant, he can't take the
     call right now. What can I pass on?" Clear to the caller, and the safe
     choice legally.
   - *As me*: the AI answers "Hello?" the way you would and talks as you. It
     never promises anything on your behalf and admits to being an AI when
     someone sincerely asks, but callers usually assume it is you. Read
     [Privacy, safety and the law](PRIVACY_AND_LEGAL.md) first.
5. **Gemini API key**: paste it and save. Summaries use the same key by default
   (Gemini's cheapest model). You need a second key only if you pick *Another
   service* under *Summary service*, such as DeepSeek, OpenRouter or a local
   Ollama.
6. **Voice**: press ▶ next to the list to hear each voice say a few lines in
   your language. The samples ship with the program, so listening costs
   nothing.

Save. The checklist on the *System* page shows anything still missing.

## 4. Prepare the phone

The short version is below; details and rollback are in
[Phone setup](PHONE.md).

1. Copy `pixelcallai_uplink-<version>.zip` from the release to the phone and
   install it as a module in the Magisk or KernelSU app. Reboot.
2. Open the **Bageo** app and work through **Setup steps** until every step
   is done: permissions, default phone app, call screening, Do Not Disturb
   access, battery exemption. The app itself needs no root access; the module
   does that work.
3. Turn on **USB debugging**: *Settings → About phone*, tap *Build number*
   seven times, then *System → Developer options → USB debugging*.
4. Plug the phone into the computer. On the phone, accept **"Allow USB
   debugging?"** and tick *Always allow from this computer*.

Within about 15 seconds the agent sets up the USB link and starts the app's
call service by itself. *System → Phones* in the web console lists the phone,
first as plugged in, then as connected.

> From now on the phone is a call box: it does not ring out loud, its own
> phone app is hidden, and its microphone and speaker stay muted during calls,
> so nothing in the room is heard. That is by design. To use it as an ordinary
> phone for a while, switch it to [normal phone mode](PHONE.md#normal-phone-mode)
> under *System → Phones* or in the app, and switch back whenever you like.

## 5. Make the first test call

Call the phone's number from another phone.

1. It rings once on your side, about three seconds (*Ring before answering*;
   0 picks up at once). During that ring the AI connects and gets its first
   words ready.
2. The AI answers. Talk to it, interrupt it, switch to another language, then
   say goodbye.
3. A few seconds after you hang up, the call appears under *Calls* with the
   recording (mixed, caller only, AI only), the transcript and a four-line
   summary: who called, what about, what the AI said, what you need to do.
   Lines spoken in another language keep their wording, with a translation
   underneath.

**Try it:** call again and pitch an insurance plan. With *Sales & scam calls →
Keep them talking* on (the default), the AI plays along, asks about every
detail and keeps you on the line, without giving out any data or agreeing to
anything.

If nothing answers, see [Troubleshooting](TROUBLESHOOTING.md); the *System*
page usually names the missing piece.

## 6. Get notified

Go to *Settings → Notifications → Add channel*, pick a service, fill in the
fields (each has a hint on where to find it) and press **Send test**. Add as
many as you like; every message goes to all of them. There are 32 services,
from Telegram (with Accept / Decline buttons for appointments) to a custom
webhook. Setup for each one: [Notifications](NOTIFICATIONS.md).

For the links in notifications to open the call, open the console to your
network (see
[Using the console from other devices](SETTINGS.md#using-the-console-from-other-devices)).
Links then point to `http://bageo.local:8787/` when the agent can answer that
name, otherwise to the computer's network address; *System → About* shows
which one, with a QR code. Fill in **Address of this console** only when you
reach the computer some other way (a VPN name, a reverse proxy, Docker).

To get the console on your own phone, scan that QR code, then add the page to
the home screen (iPhone: *Share → Add to Home Screen*; Android: Chrome menu ⋮ →
*Add to Home screen*). Next time, tap the Bageo icon.

## 7. Contacts, appointments and AI errands

None of these is needed for answering calls, and each takes a few minutes.

### Contacts

*Contacts → Import*, then drop in a file:

- a vCard (`.vcf`) exported from your phone, or a Google Contacts or Outlook
  CSV;
- a spreadsheet saved as CSV or TSV, or a table in Excel `.xlsx` or Word
  `.docx`;
- plain text with a name and a number on each line;
- a PDF, a photo of a business card or a screenshot of an address book (read
  by Gemini).

You review every entry before anything is saved and choose whether duplicates
are merged or skipped. A note on a contact ("my sister", "the landlord") goes
into the AI's prompt when that person calls. In a call's details, **Add to
contacts** saves the caller.

### Appointments

Turn them on under *Settings → Summary & calendar → Appointments → Ask me
first*. When a caller wants to fix a time, the AI says it will check and get
back to them; after the call, the request waits under **To do** at the top of
the *Calls* page. Pick how the caller hears your answer (**Text**, **AI
call-back** or **Don't tell them**), then press **Accept** or **Decline**.
Texts also need *Text the caller* switched on. The first time you press **Add
to calendar** on an accepted appointment, pick your calendar (Google,
Outlook.com, Microsoft 365, Apple, Yahoo or `.ics`); after that it goes
straight there.

### Your first AI errand

The phone must have taken at least one call first, so the agent knows which
SIMs it holds. Then open **Errands**:

1. **Who to call**: a contact's name or a number. A number without `+` is
   read as one from your own country; write numbers abroad with `+`. The AI
   only calls from a SIM of the number's country.
2. **What to take care of**: what you want and what you can accept, for
   example "Book a table for 4 tomorrow at 7 pm under Wang, non-smoking;
   7:30 to 8 is fine too."
3. **Call as**: your assistant (the default) or you in person.
4. Press **Start the call** and keep the page open. Live captions show the
   conversation. If the restaurant only offers 8:30, the AI asks you on a card
   with a 40-second countdown; **Tell the AI** sends it an instruction at any
   time.

The result appears under *Your errands* and in a single notification. When it
says *Your decision needed*, pick one of the options or write a reply and let
the AI call again.

## 8. Keep it running

Calls are answered only while the agent runs.

- **Windows:** put a shortcut to `start.cmd` in the Startup folder (`Win+R` →
  `shell:startup`), or add a Task Scheduler task "at log on" that runs
  `start.cmd -NoBrowser`. Turn off sleep while plugged in.
- **macOS:** *System Settings → General → Login Items* → add
  `start.command`, and prevent sleep (*Battery / Energy → Prevent automatic
  sleeping*).
- **Linux:** a systemd user unit:

  ```ini
  # ~/.config/systemd/user/bageo-call.service
  [Unit]
  Description=Bageo agent
  After=network-online.target

  [Service]
  WorkingDirectory=%h/bageo-call
  ExecStart=%h/bageo-call/start.sh --no-browser --yes
  Restart=on-failure

  [Install]
  WantedBy=default.target
  ```

  Then `systemctl --user enable --now bageo-call`, and
  `loginctl enable-linger $USER` so it runs without a login session.
- **Docker:** `restart: unless-stopped` in `docker-compose.yml` already brings
  it back after a reboot.

Keep the phone on a charger. If the phone or the computer restarts, both sides
reconnect by themselves.

## Next

- Make it sound like you: [Writing the prompt](PROMPTS.md)
- Everything you can change: [Settings reference](SETTINGS.md)
- How it works inside: [Architecture](ARCHITECTURE.md)

