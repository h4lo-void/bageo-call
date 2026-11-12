# Getting started

From nothing to the first answered call. Plan for about an hour, most of it the
phone.

1. [What you need](#1-what-you-need)
2. [Install and start the agent](#2-install-and-start-the-agent)
3. [Fill in the basics](#3-fill-in-the-basics)
4. [Prepare the phone](#4-prepare-the-phone)
5. [The first call](#5-the-first-call)
6. [Notifications](#6-notifications)
7. [Keep it running](#7-keep-it-running)

## 1. What you need

- A **spare Android phone** with a SIM that receives calls, **rooted** with
  Magisk or KernelSU. Tested on Pixel 7, 7a, 8 and 9. Rooting is not covered
  here; follow a guide for your exact model and keep in mind that it wipes the
  phone.
- A **computer** (Windows 10/11, macOS, Linux) that stays on while calls should
  be answered, and a USB cable to the phone.
- A **Gemini API key**: sign in at <https://aistudio.google.com/apikey>, create
  a key and copy it. Google bills Live audio per use; set a budget in your
  Google account if you want a ceiling.

## 2. Install and start the agent

The *agent* is the program on your computer that talks to the phone and to
Gemini, and serves the web console.

### The easy way: a release

1. Download the package for your system from the Releases page and unpack it
   anywhere (for example `Documents/myna-call`).
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

### From source

```text
git clone <this repository>
cd myna-call
./start.sh            # Windows: .\start.cmd
```

With [Rust](https://rustup.rs) installed, the start script builds the agent
the first time (a few minutes) and then starts it. `--build` (PowerShell:
`-Build`) rebuilds after you pull new code.

### Where things are kept

Everything lives in the folder you started from:

| | |
|---|---|
| `data/calls/` | call records, recordings, transcripts |
| `data/calls/config/settings.json` | what you set in the web console, **including your API keys** |
| `.env` | optional start-up options (port, tokens) — see [Settings reference](SETTINGS.md) |

Back up or delete `data/` as you see fit. Nothing is stored anywhere else.

## 3. Fill in the basics

Open *Settings* in the web console.

1. **Language** — the language the AI speaks; notifications and summaries use
   it too. (The language of the console itself is the switch in the header.)
2. **Your name** — as callers say it ("Alex", "Ms. Chen", "王先生").
3. **Answer style**
   - *AI assistant*: "Hello, this is Alex's AI assistant, he can't take the
     call right now — what can I pass on?" Clear to the caller, and the safe
     choice legally.
   - *As me*: the AI answers "Hello?" the way you would and talks as you.
     It never claims anything on your behalf and admits to being an AI when
     someone sincerely asks, but callers will usually assume it is you. Read
     [Privacy, safety and the law](PRIVACY_AND_LEGAL.md) first.
4. **Gemini API key** — paste it and save.
5. **Voice** — press ▶ next to the list to hear each voice say a few lines in
   your language (the first play of a voice takes a few seconds; then it is
   cached).

Save. The checklist on the *System* page shows what is still missing.

## 4. Prepare the phone

Details and rollback: [Phone setup](PHONE.md). In short:

1. Copy `pixelcallai_uplink-<version>.zip` (from the release) to the phone and
   install it as a module in the Magisk or KernelSU app. Reboot.
2. Open the **Myna** app and tap through its checklist until every
   line says OK (permissions, default phone app, call screening, Do Not
   Disturb access, battery exemption). Allow root when asked.
3. Turn on **USB debugging** (*Settings → About phone →* tap *Build number*
   seven times, then *System → Developer options → USB debugging*).
4. Plug the phone into the computer. On the phone, accept **"Allow USB
   debugging?"** and tick *Always allow from this computer*.

Within about fifteen seconds the agent sets up the USB link and starts the
app's call service by itself. *System → Phones* in the web console shows the
phone, first as plugged in, then as connected.

> The phone is now a call box: it does not ring out loud, its own phone app is
> hidden, and its microphone and speaker stay muted during calls so nothing in
> the room is heard. That is intended.

## 5. The first call

Call the phone's number from another phone.

- It rings once on the caller's side (about three seconds, adjustable).
- The AI answers. Talk to it; interrupt it; say goodbye.
- A few seconds after the call ends, it appears under *Calls* with the
  recording, the transcript and the summary.

If nothing answers, see [Troubleshooting](TROUBLESHOOTING.md) — the *System*
page usually says which piece is missing.

## 6. Notifications

*Settings → Notifications → Add channel*, pick a service, fill in the fields
(each has a hint how to get them) and press **Send test**. You can add several;
every message goes to all of them. Per-service instructions:
[Notifications](NOTIFICATIONS.md).

For the links in notifications to open the call, set **Address of this
console** to how *you* reach the computer (for example
`http://192.168.1.20:8787`) and open the console to your network — see
[Using it from other devices](SETTINGS.md#using-the-console-from-other-devices).

## 7. Keep it running

The agent has to be running for calls to be answered.

- **Windows:** put a shortcut to `start.cmd` in the Startup folder
  (`Win+R` → `shell:startup`), or create a Task Scheduler task "at log on" that
  runs `start.cmd -NoBrowser`. Disable sleep while plugged in.
- **macOS:** *System Settings → General → Login Items* → add `start.command`;
  prevent sleep (*Battery / Energy → Prevent automatic sleeping*).
- **Linux:** a systemd user unit:

  ```ini
  # ~/.config/systemd/user/myna-call.service
  [Unit]
  Description=Myna agent
  After=network-online.target

  [Service]
  WorkingDirectory=%h/myna-call
  ExecStart=%h/myna-call/start.sh --no-browser --yes
  Restart=on-failure

  [Install]
  WantedBy=default.target
  ```

  `systemctl --user enable --now myna-call` and `loginctl enable-linger $USER`
  so it runs without a login session.

Keep the phone on a charger. If the phone or the computer restarts, both sides
reconnect by themselves.

## Next

- Make it sound like you: [Writing the prompt](PROMPTS.md)
- Everything you can change: [Settings reference](SETTINGS.md)
