# Getting started

From nothing to the first answered call, and then the first call the AI makes
for you. Plan for about an hour; most of it is preparing the phone.

1. [What you need](#1-what-you-need)
2. [Install and start the agent](#2-install-and-start-the-agent)
3. [Fill in the basics](#3-fill-in-the-basics)
4. [Prepare the phone](#4-prepare-the-phone)
5. [The first call](#5-the-first-call)
6. [Notifications](#6-notifications)
7. [Contacts, appointments and AI errands](#7-contacts-appointments-and-ai-errands)
8. [Keep it running](#8-keep-it-running)

## 1. What you need

- A **spare Android phone** with a SIM that receives calls, **rooted** with
  Magisk or KernelSU, Android 10 or newer. Tested on Google Pixel 7, 7a, 8 and
  9. Rooting is not covered here; follow a guide for your exact model and keep
  in mind that unlocking the bootloader wipes the phone.
- A **computer** (Windows 10/11, macOS on Apple Silicon or Intel, Linux) that
  stays on while calls should be answered, and a USB cable to the phone. Docker
  works too, with the phone on Wi-Fi.
- A **Gemini API key**: sign in at <https://aistudio.google.com/apikey>, create
  a key and copy it. This one key is all Bageo needs: live calls, summaries,
  the scam review and contact import all use it. Google bills per use; set a
  budget in your Google account if you want a ceiling.

## 2. Install and start the agent

The *agent* is the program on your computer. It talks to the phone and to
Gemini, keeps the call records and serves the web console. It is a single
program with no database and no other services to install.

### The easy way: a release

1. Download the package for your system from the
   [Releases](https://github.com/h4lo-void/bageo-call/releases) page and
   unpack it anywhere (for example `Documents/bageo-call`):
   `bageo-call-1.0.0-windows-x64.zip`, `-macos-arm64.tar.gz` (Apple
   Silicon), `-macos-x64.tar.gz` (Intel) or `-linux-x64.tar.gz`. Each holds
   the agent, the bundled sounds, the start scripts and the documents.
2. Start it:
   - **Windows:** double-click `start.cmd`. If Windows SmartScreen warns about
     an unknown publisher, choose *More info → Run anyway*.
   - **macOS:** double-click `start.command`. The first time, macOS may refuse
     because the program is not notarised: right-click `start.command` → *Open*,
     and if the agent itself is blocked, allow it under *System Settings →
     Privacy & Security*. From a terminal you can also run
     `xattr -dr com.apple.quarantine .` in the folder once.
   - **Linux:** `./start.sh` in a terminal.
3. The script looks for `adb` (Google's Android platform-tools, needed for the
   USB link). If it is not installed it offers to download it from Google into
   a `platform-tools` folder next to the program.
4. Your browser opens the web console at <http://localhost:8787/>. Leave the
   terminal window open: closing it stops the agent.
5. The first time the agent answers, the script puts a **Bageo Console**
   shortcut on the desktop (macOS: a `.webloc` file; Linux: an app menu
   entry, also copied to the desktop folder). Use it to open the console next
   time. It is not made again while it exists; to skip it, start with
   `-NoShortcut` (Windows) / `--no-shortcut`, or set `BAGEO_NO_SHORTCUT=1`
   in `.env`. A *Myna Console* shortcut from before the project was renamed
   counts as well, and `MYNA_NO_SHORTCUT=1` still works.

### From source

```text
git clone https://github.com/h4lo-void/bageo-call.git
cd bageo-call
./start.sh            # Windows: .\start.cmd
```

With [Rust](https://rustup.rs) installed, the start script builds the agent
the first time (a few minutes) and then starts it. `--build` (PowerShell:
`-Build`) rebuilds after you pull new code.

### With Docker

```text
cp .env.example .env        # set CALLS_UI_TOKEN and DEVICE_TOKEN
docker compose up -d --build
```

Inside a container the agent is reachable from your network, so both tokens are
required. The console is then at `http://<host>:8787/?token=<CALLS_UI_TOKEN>`,
and phones connect over Wi-Fi to `ws://<host>:8787/ws?token=<DEVICE_TOKEN>`
(see [Phone setup → Wi-Fi](PHONE.md#wi-fi)). The agent does not answer
`bageo.local` from inside a container, and it cannot fill in the console
address for notification links by itself: set `PUBLIC_URL` in `.env`
(for example `http://192.168.1.20:8787`). The comments in
`docker-compose.yml` cover the time zone and upgrading from the Myna name.

### Where things are kept

Everything lives in the folder you started from (`/data/calls` in Docker):

| | |
|---|---|
| `data/calls/` | call records, recordings, transcripts, contacts, to-do items, errands |
| `data/calls/config/settings.json` | what you set in the web console, **including your API keys** |
| `.env` | optional start-up options (port, tokens); see [Settings reference](SETTINGS.md) |

Back up or delete `data/` as you see fit. Nothing is stored anywhere else.

## 3. Fill in the basics

Open *Settings* in the web console.

1. **Language**: the language the AI speaks. *Language of notifications and
   summaries* can differ, for example the AI answers in English while you read
   繁體中文. (The language of the console itself is the switch in the header.)
2. **Your name**: as callers say it ("Alex", "Ms. Chen", "王先生").
3. **Other names callers use for you**: one per line, up to ten. An English
   name, a nickname, a short form ("Lex"). When the AI hears one, it knows the
   caller means you.
4. **Answer style**
   - *AI assistant*: "Hello, this is Alex's AI assistant, he can't take the
     call right now. What can I pass on?" Clear to the caller, and the safe
     choice legally.
   - *As me*: the AI answers "Hello?" the way you would and talks as you.
     It never claims anything on your behalf and admits to being an AI when
     someone sincerely asks, but callers will usually assume it is you. Read
     [Privacy, safety and the law](PRIVACY_AND_LEGAL.md) first.
5. **Gemini API key**: paste it and save. Summaries use the same key by
   default (Gemini's lowest-cost model); you only need another key if you pick
   *Another service* under *Summary service*, such as DeepSeek, OpenRouter or a
   local Ollama.
6. **Voice**: press ▶ next to the list to hear each voice say a few lines in
   your language. The samples ship with the program, so this costs nothing.

Save. The checklist on the *System* page shows what is still missing.

## 4. Prepare the phone

Details and rollback: [Phone setup](PHONE.md). In short:

1. Copy `pixelcallai_uplink-<version>.zip` (from the release) to the phone and
   install it as a module in the Magisk or KernelSU app. Reboot.
2. Open the **Bageo** app and tap through its **Setup steps** until every
   step says OK (permissions, default phone app, call screening, Do Not
   Disturb access, battery exemption). The app itself does not need root
   access: the module does that work.
3. Turn on **USB debugging** (*Settings → About phone →* tap *Build number*
   seven times, then *System → Developer options → USB debugging*).
4. Plug the phone into the computer. On the phone, accept **"Allow USB
   debugging?"** and tick *Always allow from this computer*.

Within about fifteen seconds the agent sets up the USB link and starts the
app's call service by itself. *System → Phones* in the web console shows the
phone, first as plugged in, then as connected.

> The phone is now a call box: it does not ring out loud, its own phone app is
> hidden, and its microphone and speaker stay muted during calls so nothing in
> the room is heard. That is intended. To use it as an ordinary phone for a
> while, switch it to [normal phone mode](PHONE.md#normal-phone-mode) from
> *System → Phones* or in the app; switch back whenever you like.

## 5. The first call

Call the phone's number from another phone.

- It rings once on the caller's side (about three seconds, adjustable). The AI
  is connected and has its first words ready during that ring.
- The AI answers. Talk to it, interrupt it, ask it something in another
  language, say goodbye.
- A few seconds after the call ends, it appears under *Calls* with the
  recording (mixed, caller only, AI only), the transcript and the four-line
  summary: who called, what about, what the AI said, what you need to do.
  Lines said in another language keep their wording, with a translation
  underneath.

If nothing answers, see [Troubleshooting](TROUBLESHOOTING.md); the *System*
page usually says which piece is missing.

## 6. Notifications

*Settings → Notifications → Add channel*, pick a service, fill in the fields
(each has a hint how to get them) and press **Send test**. You can add several;
every message goes to all of them. Sixteen services are supported, from
Telegram (with Accept / Decline buttons for appointments) to a custom webhook.
Per-service instructions: [Notifications](NOTIFICATIONS.md).

For the links in notifications to open the call, open the console to your
network; see
[Using it from other devices](SETTINGS.md#using-the-console-from-other-devices).
The links then point to `http://bageo.local:8787/` when the agent can answer
that name, otherwise to the computer's network address; *System → About*
shows which one, with a QR code. Set **Address of this console** only when you
reach the computer some other way (a VPN name, a reverse proxy, Docker).

On your phone, open *System* in the console on the computer: *About* shows the
console's address with a QR code. Scan it, then add the page to the home
screen (iPhone: *Share → Add to Home Screen*; Android: Chrome menu ⋮ → *Add to
Home screen*). Next time, tap the Bageo icon.

## 7. Contacts, appointments and AI errands

None of these is needed for answering calls. Each takes a few minutes.

### Contacts

*Contacts → Import*, then drop in a file: a vCard (`.vcf`) exported
from your phone, Google Contacts or Outlook CSV, a spreadsheet saved as CSV or
TSV, an Excel `.xlsx` or Word `.docx` table, plain text with a name and a
number on each line, or a PDF, a photo of a business card or a screenshot of an
address book (read by Gemini). You see every entry before anything is saved,
and choose whether duplicates are merged or skipped. A note on a contact
("my sister", "the landlord") goes into the AI's prompt when that person calls.
You can also add a caller from a call's details with **Add to contacts**.

### Appointments

*Settings → Summary & calendar → Appointments → Ask me first* turns them on.
When a caller wants to fix a time, the AI says it will check and get back to
them; after the call the request waits under **To do** at the top of the
*Calls* page. Pick how the caller hears your answer (**Text**, **AI
call-back** or **Don't tell them**), then **Accept** or **Decline**. *Text the
caller* must be on for texts. The first time you press **Add to calendar** on
an accepted appointment, choose your calendar (Google, Outlook.com, Microsoft
365, Apple, Yahoo or `.ics`); later presses go straight there.

### Your first AI errand

The phone must have taken at least one call first, so the agent knows which
SIM it holds. Then open **Errands**:

1. Under *Who to call*, type a contact's name or a number. A number without a
   `+` is read as your own country's; write numbers abroad with `+`. The AI
   only calls on a SIM of the number's country.
2. Under *What to take care of*, say what you want and what you can accept,
   for example: "Book a table for 4 tomorrow at 7 pm under Wang, non-smoking;
   7:30 to 8 is fine too."
3. Choose *Call as*: your assistant (the default) or you in person.
4. **Start the call** and keep the page open. You see live captions; when the
   AI is unsure it asks you on a card with a 40-second countdown, and **Tell
   the AI** sends it an instruction at any time.

The result appears under *Your errands* and in one notification. When it says
*Your decision needed*, pick one of the options or write a reply and let the AI
call again.

## 8. Keep it running

The agent has to be running for calls to be answered.

- **Windows:** put a shortcut to `start.cmd` in the Startup folder
  (`Win+R` → `shell:startup`), or create a Task Scheduler task "at log on" that
  runs `start.cmd -NoBrowser`. Disable sleep while plugged in.
- **macOS:** *System Settings → General → Login Items* → add `start.command`;
  prevent sleep (*Battery / Energy → Prevent automatic sleeping*).
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

  `systemctl --user enable --now bageo-call` and `loginctl enable-linger $USER`
  so it runs without a login session.
- **Docker:** `restart: unless-stopped` in `docker-compose.yml` already
  brings it back after a reboot.

Keep the phone on a charger. If the phone or the computer restarts, both sides
reconnect by themselves.

## Next

- Make it sound like you: [Writing the prompt](PROMPTS.md)
- Everything you can change: [Settings reference](SETTINGS.md)
- How it works inside: [Architecture](ARCHITECTURE.md)
