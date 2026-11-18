# Getting started: an AI phone assistant on your real phone number

This guide takes you from a fresh download to an AI that answers your phone
calls on your own number, and then to the first call the AI makes for you.
Bageo is self-hosted: a rooted Android phone holds your SIM, one program on
your computer streams each call to Gemini Live, and every recording and
transcript stays in a folder on your disk. Plan for about an hour; most of it
goes into preparing the phone.

1. [What you need](#1-what-you-need)
2. [Install and start Bageo](#2-install-and-start-bageo)
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

## 2. Install and start Bageo

Bageo runs as one program on your computer. It talks to the phone and to
Gemini, keeps the call records and serves the web console. There is no
database and no other service to install.

### From a release (easiest)

1. Download the package for your system from the
   [Releases](https://github.com/h4lo-void/bageo-call/releases) page and unpack
   it anywhere, for example `Documents/bageo-call`:

   | System | Package |
   |---|---|
   | Windows | `bageo-call-<version>-windows-x64.zip` |
   | macOS, Apple Silicon | `bageo-call-<version>-macos-arm64.tar.gz` |
   | macOS, Intel | `bageo-call-<version>-macos-x64.tar.gz` |
   | Linux | `bageo-call-<version>-linux-x64.tar.gz` |

   Each one holds the program, the bundled sounds, the start scripts and the
   documents.
2. Start it:
   - **Windows:** double-click `bageo-status.exe` (or `start.cmd`). If
     SmartScreen warns about an unknown publisher, choose *More info → Run
     anyway*.
   - **macOS:** the first time, double-click `start.command`. It opens Bageo
     Status and shows it where Bageo is; you can close the Terminal window
     afterwards. From then on, open `Bageo Status.app` directly. The program
     is not notarised, so the first time macOS may refuse: right-click the
     file → *Open*. If the icon shows up but Bageo does not start, allow the
     Bageo program under *System Settings → Privacy & Security*, or run
     `xattr -dr com.apple.quarantine .` once in the folder, then start again.
   - **Linux:** run `./start.sh` in a terminal.
3. On Windows and macOS there is no command window. An icon appears in the
   system tray (Windows) or the menu bar (macOS), and Bageo runs in the
   background with it. Bageo stops only when you choose "Quit Bageo" from the
   icon's menu.
4. The first time, your browser opens the web console. It offers to set a
   console password; skip it if you only use the console at home. Later, click
   the icon to open the console, or go to <http://localhost:8787/>.
5. The USB link needs `adb` (Google's Android platform-tools). If it is
   missing, click the icon and press "Download". It goes into a
   `platform-tools` folder next to the program.
6. Bageo writes its log to `bageo.log` in the same folder. To see the output
   in a terminal as before, start with `start.cmd -Console` (Windows) or
   `./start.sh --console` (macOS).

On Linux, and with `-Console` or `--console`, the start script does the same
work in the terminal: it offers the `adb` download, opens the web console and
puts a **Bageo Console** shortcut on the desktop (to skip the shortcut, start
with `-NoShortcut` on Windows or `--no-shortcut`, or set
`BAGEO_NO_SHORTCUT=1` in `.env`). Leave that terminal open: closing it stops
Bageo.

### From source

```text
git clone https://github.com/h4lo-void/bageo-call.git
cd bageo-call
./start.sh            # Windows: .\start.cmd
```

With [Rust](https://rustup.rs) installed, the start script builds the program
the first time (a few minutes) and then starts it. After pulling new code, add
`--build` (PowerShell: `-Build`) to rebuild.

### With Docker

```text
cp .env.example .env        # set CALLS_UI_TOKEN and DEVICE_TOKEN
docker compose up -d --build
```

In a container Bageo is reachable from your network, so two values in `.env`
are required: `CALLS_UI_TOKEN` (the console password) and `DEVICE_TOKEN` (for
the phone's connection).

- The console is at `http://<host>:8787/`; type the `CALLS_UI_TOKEN` value
  when it asks for the password.
- Phones connect over Wi-Fi to `ws://<host>:8787/ws?token=<DEVICE_TOKEN>` (see
  [Phone setup → Wi-Fi](PHONE.md#wi-fi)).
- Set `PUBLIC_URL` in `.env` (for example `http://192.168.1.20:8787`) so the
  links in notifications open. `bageo.local` does not work inside a container.
- The comments in `docker-compose.yml` cover the time zone.

### Where your data lives

Everything stays in the folder you started from (`/data/calls` in Docker):

| Path | Contents |
|---|---|
| `data/calls/` | call records, recordings, transcripts, contacts, to-do items, errands |
| `data/calls/config/settings.json` | everything you set in the web console, **including your API keys** |
| `.env` | optional start-up options (port, passwords); see [Settings reference](SETTINGS.md) |

Back up or delete `data/` as you like. Nothing is stored anywhere else.

## 3. Fill in the basics

Open *Settings* in the web console.

1. **Language**: the language the AI speaks. (The console's own language is
   the switch in the header.)
2. **Your name**: the way callers say it ("Alex", "Ms. Chen", "王先生").
3. **Answer style**
   - *AI assistant*: "Hello, this is Alex's AI assistant, he can't take the
     call right now. What can I pass on?" Clear to the caller, and the safe
     choice legally.
   - *As me*: the AI answers "Hello?" the way you would and talks as you. It
     never promises anything on your behalf and admits to being an AI when
     someone sincerely asks, but callers usually assume it is you. Read
     [Privacy, safety and the law](PRIVACY_AND_LEGAL.md) first.
4. **Gemini API key**: paste it and save. Summaries use the same key by
   default, so there is nothing else to set up.
5. **Voice**: press **Preview** next to the list to hear the voice say a few
   lines in your language. The samples ship with the program, so listening
   costs nothing.

**More options** holds two more you may want: *Other names callers use for
you* (an English name, a nickname; the AI then knows the caller means you) and
*Language of notifications and summaries* (the AI can answer in English while
you read 繁體中文).

Save. The checklist on the *System* page shows anything still missing.

## 4. Prepare the phone

The short version is below; details and rollback are in
[Phone setup](PHONE.md).

1. Copy `bageo-phone-module-<version>.zip` from the release to the phone and
   install it as a module in the Magisk or KernelSU app. Reboot.
2. Open the **Bageo** app and work through **Setup steps** until every step
   is done: permissions, default phone app, call screening, Do Not Disturb
   access, battery exemption. The app itself needs no root access; the module
   does that work.
3. Turn on **USB debugging**: *Settings → About phone*, tap *Build number*
   seven times, then *System → Developer options → USB debugging*.
4. Plug the phone into the computer. On the phone, accept **"Allow USB
   debugging?"** and tick *Always allow from this computer*.

Within about 15 seconds Bageo sets up the USB link and starts the app's call
service by itself. *System → Phones* in the web console lists the phone; once
it shows *Ready for calls*, you are done.

After that you can unplug the cable: on the same network the phone switches to
Wi-Fi by itself (see [Phone setup → Wi-Fi](PHONE.md#wi-fi)). On Windows, allow
Bageo on private networks when the firewall asks.

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

Go to *Settings → Notifications*, pick a service, press **Add channel**, fill
in the fields (each has a hint on where to find it) and press **Send test**.
Add as many as you like; every message goes to all of them. There are 32
services; setup for each one is in [Notifications](NOTIFICATIONS.md).

For the links in notifications to open the call, first open the console to
your network (see
[Using the console from other devices](SETTINGS.md#using-the-console-from-other-devices)).
Links then use `http://bageo.local:8787/` or the computer's network address
by themselves; *System → About* shows which one, with a QR code.

Fill in **Address of this console** under *Settings → Advanced* only when you
reach the computer through a VPN, a reverse proxy or Docker.

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

Each contact goes on one of four lists: ordinary contact, Whitelist (close
family and friends, whose calls can be put through to you), Blacklist (turned
away, no AI) or stall list (kept talking from the first second).

### Appointments

Under *Settings → Summary & calendar*, set *Appointments* to *Ask me first*.
From then on:

1. When a caller wants to fix a time, the AI says it will check and get back
   to them.
2. After the call, the request waits under **To do** at the top of the
   *Calls* page, and in the notification.
3. Pick how the caller hears your answer (**Text**, **AI call-back** or
   **Don't tell them**), then press **Accept** or **Decline**. Texts also need
   *Text the caller* switched on in the settings.
4. On an accepted appointment, press **Add to calendar**. The first time it
   asks which calendar you use (Google, Outlook.com, Microsoft 365, Apple,
   Yahoo or `.ics`); after that it goes straight there.

### Your first AI errand

The phone must have taken at least one call first, so Bageo knows which
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

Calls are answered only while Bageo runs.

- **Windows:** click the Bageo Status icon, open "Settings" and turn on "Start
  when I log in". Turn off sleep while plugged in.
- **macOS:** click the Bageo Status icon, open "Settings" and turn on "Start
  when I log in". Prevent sleep (*Battery / Energy → Prevent automatic
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

### See the status in the system tray or menu bar

On Windows and macOS, **Bageo Status** is the program you start Bageo with. It
sits in the system tray (Windows) or the menu bar (macOS). The frame around
its icon is green when everything works, yellow when something needs a look
(for example the console still lists settings to fill in, a phone is not
connected, or the Android tools are missing) and red when Bageo cannot be
reached or no phone is connected.

- **Windows:** Windows puts new icons behind the `^` next to the clock; drag
  the icon onto the taskbar to keep it in view.
- **macOS:** the icon is on the right side of the menu bar.

Click the icon to see the console address and your phones, and to open the
console. Right-click it and choose "Quit Bageo" to stop Bageo and remove the
icon.

Under "Settings" you can type the address, enter the console password, turn on
"Start when I log in", and turn on small notifications for incoming calls and
text messages. They are off by default; click one to open the console. Text
message notifications need the
[SMS forwarder](../../tools/sms_forward/README.md) from the same version.

Bageo Status can also watch a Bageo that runs somewhere else, such as another
computer or Docker. Copy `bageo-status.exe` or `Bageo Status.app` to the
computer you sit at and start it there. It finds Bageo on your network by
itself and never stops that Bageo.

## Next

- Make it sound like you: [Writing the prompt](PROMPTS.md)
- Everything you can change: [Settings reference](SETTINGS.md)
- How it works inside: [Architecture](ARCHITECTURE.md)


<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
