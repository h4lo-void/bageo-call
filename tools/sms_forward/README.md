# SMS forwarder (optional, experimental)

`sms_forward.py` reads the text messages (SMS) that arrive on your Android phones
and sends each one to the notification channels you set up in the Myna web
console (*Settings → Notifications*). It is a separate script: the agent works
without it, and nothing starts it for you.

It runs on the computer the phones are plugged into and uses only the Python
standard library (Python 3.10 or newer; tested with 3.12 on Windows).

**Status:** the logic is covered by offline tests with fake phones. On real phones,
this way of reading SMS has only been exercised on the project's own rooted Pixel
phones. Reading a phone without root (inbox only) is covered by the tests but has
not been tried on a real device. Expect rough edges on other devices and Android
versions.

## What it does

For every phone `adb devices` lists as `device`, it reads two sources:

| Source | How | Needs |
|---|---|---|
| **Inbox** | `adb shell content query --uri content://sms/inbox`, every 60 s, rows newer than a saved position | USB debugging. No app on the phone. |
| **Hook queue** (optional) | one small file per incoming SMS, written on the phone by the Myna app's SMS hook, checked every 4 s | The app, an LSPosed-compatible framework, root. See below. |

- A message seen by both sources is sent once.
- **No backlog.** What is already on a phone when the script first sees it is never
  forwarded: the inbox position starts at the newest message, and hook files already
  waiting are deleted (the messages stay in the phone's inbox).
- A message that could not be delivered stays where it is and is tried again. If the
  agent answers `{"ok": false}` (no channel accepted it), that counts as not delivered.
- While the agent has a call in progress, the inbox is not polled (the hook queue
  still is). If the agent is not running, polling continues and delivery is retried.
- If more than 20 not-yet-forwarded messages show up in an inbox at once, they are
  skipped and one notice is sent instead. This protects you from a flood after a long
  outage or a restored phone.
- Only SMS. MMS and chat (RCS) messages are not in the SMS inbox and are not forwarded.

The forwarded message looks like this; the labels follow `SMS_LANG`, the text of the
SMS is passed on as it is:

```text
New SMS
From: +15555550123
To: +15555550100
Message: Your parcel is on its way.
Device: Pixel 7 (TESTSERIAL0001)
```

`To` is the number or carrier of the receiving SIM and is left out when the phone
does not tell. A `Received` line is added when a message is forwarded more than five
minutes late, in the agent's time zone (the `local_time` of its `/health`). The agent
removes control characters, cuts a notification at 3500 characters and, when its
language is `zh-TW`, converts Simplified Chinese to Traditional.

## Requirements

- The Myna agent, running, with at least one notification channel
  ([Notifications](../../docs/en/NOTIFICATIONS.md)).
- `adb` (Android platform-tools). The agent's start scripts offer to download it.
- Each phone connected by USB with **USB debugging** on and this computer allowed.
- For the inbox: the phone must have been unlocked once since it was switched on.
  Reading SMS with `content query` needs no extra app on most phones, but some
  devices and Android versions refuse it to adb. If the phone is rooted, the script
  then retries through `su`; if not, that phone cannot be read.
- For the hook queue (optional, faster, also works before the first unlock): the
  Myna app on the phone, an LSPosed-compatible framework with the app
  enabled as a module for `com.android.phone`
  ([Optional SMS hook](../../android/README.md#optional-sms-hook)), and root for
  adb's shell (`su`). The phone may ask once whether *Shell* may have root access.
  Without all of this the script falls back to the inbox on its own.

## Run it

```text
cd tools/sms_forward
python sms_forward.py             # runs until you stop it (Ctrl+C)
python sms_forward.py --once      # one cycle, then exit
python sms_forward.py --dry-run   # one cycle; shows what it would do
```

`--dry-run` sends nothing, deletes nothing and saves nothing (it still writes to
the log). The first normal run only records where each phone's inbox stands; send
yourself a new SMS to see one arrive.

Only one copy runs at a time: a second one finds the lock file and exits.

Tests (no phone, no adb, no network): `python -m unittest` in this folder.

## Settings

Environment variables, or `KEY=VALUE` lines in `<SMS_HOME>/sms_forward.env` (copy
[`sms_forward.env.example`](sms_forward.env.example)). The environment wins over the
file. All are optional.

| Variable | Default | Meaning |
|---|---|---|
| `AGENT_URL` | `http://127.0.0.1:8787` | Where the agent listens. |
| `AGENT_TOKEN` | *(none)* | The agent's `CALLS_UI_TOKEN`, sent as `Authorization: Bearer <token>`. |
| `SMS_LANG` | the agent's language | Language of the labels: `en`, `zh-TW`, `zh-CN`, `ja`, `ko`, `es`, `fr`, `de`, `pt`. Without it the agent's language is used and asked again every minute; while the agent has not told it yet, messages wait (at most three minutes, then English). |
| `ADB_BIN` | found automatically | Full path of `adb`. Without it: the adb an agent on this computer reports, then `adb` on the `PATH`, then the Android SDK (`ANDROID_HOME`, `ANDROID_SDK_ROOT`, the usual SDK folders), then a `platform-tools` folder in the repository root or the current folder. |
| `SMS_SERIALS` | every phone | Only these serials (as in `adb devices`), comma separated. |
| `SMS_SIM_NUMBERS` | *(none)* | `ICCID=label` pairs, comma separated, for SIM cards that do not report their own number. The label is shown as `To`. |
| `SMS_USE_SU` | `1` | `0` = never run `su` on a phone: no hook queue, inbox as the shell user only. |
| `SMS_PUSH_SEC` | `4` | Seconds between checks of the hook queue. |
| `SMS_INBOX_SEC` | `60` | Seconds between inbox polls. |
| `SMS_DEDUP_SEC` | `86400` | The same text from the same sender on the same phone is sent once within this time. |
| `SMS_FLOOD_CAP` | `20` | More new inbox messages than this at once are skipped. |
| `SMS_HOME` | `~/.myna-call/sms` | Folder for the state, log, lock and settings file. Environment only. |
| `SMS_ENV_FILE` | `<SMS_HOME>/sms_forward.env` | Another settings file. Environment only. |

Files in `SMS_HOME`: `state.json`, `health.json` (what the last cycle saw; handy for
checking that it is alive), `sms_forward.log` (kept under about 4 MB),
`sms_forward.lock`.

## Start it when you log in

Put your settings in `<SMS_HOME>/sms_forward.env`, so they apply however the script
is started. Replace the paths below with your own. These are minimal examples.

**Windows (Task Scheduler)**, in PowerShell from the repository root. `pythonw.exe`
runs it without a console window:

```powershell
$python = (Get-Command pythonw.exe).Source
$script = (Resolve-Path .\tools\sms_forward\sms_forward.py).Path
Register-ScheduledTask -TaskName "Myna SMS forwarder" `
  -Action (New-ScheduledTaskAction -Execute $python -Argument "`"$script`"") `
  -Trigger (New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME) `
  -Settings (New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) `
    -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries)
```

Remove it with `Unregister-ScheduledTask -TaskName "Myna SMS forwarder"`.

**macOS (launchd)**, as `~/Library/LaunchAgents/com.mynacall.sms-forward.plist`,
then `launchctl load ~/Library/LaunchAgents/com.mynacall.sms-forward.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.mynacall.sms-forward</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/python3</string>
    <string>/Users/you/myna-call/tools/sms_forward/sms_forward.py</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
</dict>
</plist>
```

launchd starts programs with a short `PATH`, so set `ADB_BIN` in the settings file.

**Linux (systemd user unit)**, as `~/.config/systemd/user/sms-forward.service`, then
`systemctl --user daemon-reload` and `systemctl --user enable --now sms-forward`:

```ini
[Unit]
Description=Myna SMS forwarder

[Service]
ExecStart=/usr/bin/python3 %h/myna-call/tools/sms_forward/sms_forward.py
Restart=on-failure
RestartSec=30

[Install]
WantedBy=default.target
```

To keep it running while you are logged out: `loginctl enable-linger $USER`.

## Privacy

- **Where your messages go.** The sender, the text and the receiving number of every
  SMS, including one-time codes, are sent to the agent and from there to every
  enabled notification channel. Those are services run by other parties (Telegram,
  Discord, a webhook, …). Forward only what you are willing to store there.
- **On the way to the agent.** With the default `AGENT_URL` nothing leaves the
  computer before the agent sends it on. If the agent runs on another machine, the
  messages and `AGENT_TOKEN` travel as plain HTTP unless you give an `https://` URL.
- **`state.json`** holds phone serials and model names, each inbox position (row id
  and timestamp), and for each forwarded message a shortened SHA-256 fingerprint of
  phone + sender + text, dropped after `SMS_DEDUP_SEC`. It holds no message text and
  no phone numbers. A fingerprint cannot be read back, but a short, guessable text
  could be confirmed by trying candidates.
- **`health.json`** holds the process id, timestamps, counters, serials and models.
- **`sms_forward.log`** holds serials, models, row ids and the last four digits of
  sender numbers (sender names only as a length). It never holds message texts or
  whole phone numbers.
- **`sms_forward.env`** may hold the agent's token and your SIM numbers. Keep it
  private. When the script creates the folder on macOS or Linux, only you can read
  it; if you create it yourself, set that up yourself (`chmod 700`).
- **On the phone**, a hook file holds the full text until it is delivered, then it
  is deleted.

## Troubleshooting

Look at `sms_forward.log` and `health.json` in `SMS_HOME` first.

| What you see | What to check |
|---|---|
| `adb not found` | Install Android platform-tools, or set `ADB_BIN`. |
| A phone is missing from `health.json`, or its `adb` is `unauthorized` / `offline` | Run `adb devices`. Unlock the phone and accept the *Allow USB debugging* prompt; try another cable or port. With `SMS_SERIALS` set, check the serial. |
| `deliver failed … no answer` | The agent is not running, or `AGENT_URL` is wrong. Messages are kept and sent later. |
| `deliver failed … 401` | The agent has a `CALLS_UI_TOKEN`; set the same value as `AGENT_TOKEN`. |
| `deliver failed … no notification channel accepted it` | No channel is set up, or every channel failed. Use the test button under *Settings → Notifications*. |
| `inbox … unreadable` | Unlock the phone once after a restart. If it stays, this phone does not let adb read SMS; a rooted phone is read through `su` instead. |
| `hook queue … not readable` | Normal on a phone without root or without the hook; the inbox is used. Set `SMS_USE_SU=0` to stop it trying. |
| Old messages did not arrive after the first start | Intended: only messages that arrive after a phone was first seen are forwarded. |
| A repeated, identical message arrived only once | Intended within `SMS_DEDUP_SEC` (24 hours). Lower it if you need every repeat. |
| A notice says messages were skipped | More than `SMS_FLOOD_CAP` new messages appeared at once. They stay in the phone's inbox. |
| The same SMS arrives twice | The phone is listed twice by `adb devices` (USB and wireless debugging). Name one of them in `SMS_SERIALS`. |
| Phones drop their connection to the agent when this script starts | Two different adb versions are installed and each restarts the adb server. Set `ADB_BIN` to the adb the agent uses. The script itself never runs `adb kill-server`. |
| `another sms_forward.py holds the lock` | It is already running, for example from the login task. |
| Labels are in the wrong language | Set `SMS_LANG`. Without it the agent's language is used; messages wait up to three minutes for the agent to answer, then go out in English. |
