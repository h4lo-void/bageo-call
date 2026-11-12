# Phone setup

The phone is the gateway to the telephone network: it holds your SIM, answers
the call without a sound, sends the caller's voice to the agent and plays the
AI's voice into the call. It does no AI work itself.

Everything about building the app, its permissions and its protocol is in
[`android/README.md`](../../android/README.md). This page is the user's view.

## Which phones

- **Tested: Google Pixel 7, 7a, 8 and 9**, rooted with Magisk or KernelSU.
- **Other phones: try at your own risk.** The app needs two things from the
  phone's audio hardware layer that many phones do not offer to apps, even
  privileged ones: recording the other party's voice during a call, and playing
  sound *into* the call. If either is missing, the call is answered but one
  side hears nothing. There is no list of other working models yet — if you try
  one, please report what happened.
- Android 10 or newer.

One behaviour differs between phones, and the agent works it out by itself at
the start of every call. Where the phone's call recording does not contain the
AI's own voice (the Pixel 9 in our use), the caller can cut in while the AI is
talking and the background sound runs through the whole call. Where it does
(Pixel 7 and 7a in our use), the AI's own voice is filtered out of what it
hears, so it finishes its sentence before it listens again, and the background
sound plays only while the AI speaks.

## Use a spare phone

Installing the module turns the phone into a call box:

- the app becomes the default phone app and call-screening app, and Google's
  phone app is disabled (no call screen appears);
- Do Not Disturb is switched on, ringer and notification volume go to zero;
- during calls the microphone is muted, the speaker is off and the earpiece is
  at its minimum — nothing in the room is heard and nothing is played out loud.

You can still place calls from it with another dialer app, but do not expect a
normal handset. All of this can be undone: see [Rollback](#rollback).

## Install

1. **Root** the phone (Magisk or KernelSU). Not covered here.
2. If you ever installed the app as a normal APK, uninstall it. Never have both.
3. Copy `pixelcallai_uplink-<version>.zip` to the phone, open the Magisk or
   KernelSU app → *Modules* → *Install from storage* → pick the zip → reboot.
4. Open **Myna**. Work through the checklist; each line has a button:

   | Line | What to do |
   |---|---|
   | Installed as a privileged system app | Should be OK after the reboot. If not, the module is not active. |
   | Privileged permissions | Granted by the module; nothing to tap. |
   | App permissions | **Grant** (microphone, phone, call log, notifications). |
   | Default phone app / Call screening app | **Set**. |
   | Do Not Disturb access | Open the settings page and allow it. |
   | Battery optimisation | Allow the exemption, so Android does not stop the service. |

   Allow root when the root manager asks.
5. Leave **AI answering** on.

## Connect it to the computer

### USB (recommended)

1. On the phone: *Settings → About phone →* tap *Build number* seven times;
   then *System → Developer options →* **USB debugging** on.
2. Plug it into the computer running the agent.
3. Accept **"Allow USB debugging?"** on the phone (tick *Always allow*).

That is all. The agent finds the phone, maps the app's local address
(`ws://127.0.0.1:8765`) through the cable to itself and starts the app's
service. It repeats the check every fifteen seconds, so unplugging and
replugging, or restarting either side, heals by itself. USB also keeps the
phone charged and works without Wi-Fi.

Several phones can be plugged into one computer; each answers its own number.

### Wi-Fi

Use this when the phone cannot be cabled to the computer.

1. Start the agent so that it listens on the network and requires a token: in
   `.env` set `AGENT_HOST=0.0.0.0`, `DEVICE_TOKEN=<a long random string>` and
   `CALLS_UI_TOKEN=<another one>` (see [Settings reference](SETTINGS.md)).
2. In the app, under *Connection*, enter
   `ws://<computer's address>:8787/ws?token=<DEVICE_TOKEN>` and save.
   **Test connection** tells you whether it works.

The connection is not encrypted (`ws://`): use it on your own network only, or
put a TLS reverse proxy in front and use `wss://`. Calls are more robust on
USB.

## Day to day

- The app shows a quiet permanent notification while its service runs and
  starts by itself after a reboot.
- To pause answering, switch **AI answering** off in the app (calls then ring
  silently and are not picked up), or switch off *AI answers calls* in the web
  console (calls are picked up and hung up).
- A call you dial yourself on the phone is left alone.

## Rollback

1. Disable or remove the **Myna Uplink** module in Magisk / KernelSU
   and reboot. If the phone does not boot far enough, use the root manager's
   safe mode, or with root from recovery:
   `touch /data/adb/modules/pixelcallai_uplink/disable`.
2. Removing the module does **not** bring back Google's phone app or turn off
   Do Not Disturb — Android remembers those. Re-enable the phone app:

   ```text
   adb shell su -c "pm enable com.google.android.dialer"
   adb shell su -c "pm enable com.google.android.dialer/com.android.incallui.InCallServiceImpl"
   adb shell su -c "pm enable com.google.android.dialer/com.android.incallui.InCallActivity"
   adb shell su -c "cmd role add-role-holder android.app.role.DIALER com.google.android.dialer"
   adb shell su -c "telecom set-default-dialer com.google.android.dialer"
   ```

3. Turn off Do Not Disturb and raise the volumes in the system settings.

## When it does not work

See [Troubleshooting](TROUBLESHOOTING.md) and the table at the end of
[`android/README.md`](../../android/README.md#troubleshooting).
