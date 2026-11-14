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
normal handset. All of this can be switched off from the app or the web
console (see [Normal phone mode](#normal-phone-mode)) and undone for good (see
[Rollback](#rollback)).

## Install

1. **Root** the phone (Magisk or KernelSU). Not covered here.
2. If you ever installed the app as a normal APK, uninstall it. Never have both.
3. Copy `pixelcallai_uplink-<version>.zip` to the phone, open the Magisk or
   KernelSU app → *Modules* → *Install from storage* → pick the zip → reboot.
4. Open **Bageo**. The card at the top says in one sentence what the phone does
   right now. Below it, **Setup steps** lists what is still missing, each step
   with a button:

   | Step | What to do |
   |---|---|
   | Install the Bageo module | Done after the reboot. If not, the module is not active. |
   | Allow Bageo to use the phone and microphone | **Allow** (microphone, phone, call log, notifications). |
   | Make Bageo the default phone app / the call screening app | **Set**. |
   | Let Bageo control Do Not Disturb | **Open settings** and allow it. |
   | Do not let Android put Bageo to sleep | **Allow**, so Android does not stop the service. |

   The Bageo app itself does not need root access (the module does the root
   work), so if the root manager asks, either answer is fine. When every step is done the list
   folds away, and once the phone is connected to the computer the card says
   *The AI is answering your calls*. The connection address, the connection
   test and the detailed checks are under **Advanced**.
5. Leave **AI answers my calls** on.

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
2. In the app, under *Advanced → Connection*, enter
   `ws://<computer's address>:8787/ws?token=<DEVICE_TOKEN>` and save.
   **Test connection** tells you whether it works, and if not, why in plain
   words (the raw error is under *Details*).

The connection is not encrypted (`ws://`): use it on your own network only, or
put a TLS reverse proxy in front and use `wss://`. Calls are more robust on
USB.

## Day to day

- The app shows a quiet permanent notification while its service runs and
  starts by itself after a reboot.
- To use the phone yourself for a while, switch it to a
  [normal phone](#normal-phone-mode). To stop the AI on every phone at once,
  switch off *AI answers calls* in the web console (calls are then picked up
  and hung up).
- A call you dial yourself on the phone is left alone: no AI, and the microphone
  and earpiece work normally for it.
- One call at a time: a second call that rings during a call is declined at once,
  and the agent is told about it.
- If the computer cannot be reached when a call rings, the phone does not answer;
  the call goes to your carrier's missed call or voicemail. If the connection to
  the computer drops during a call for more than 3 seconds, the phone hangs up
  instead of leaving the caller in silence.
- With two SIMs, the agent knows which SIM each call came in on.
- The **Open the web console** button in the app appears once the agent has a
  console address: *Address of this console* in the settings, or, when that is
  empty and the console is open to your network, the computer's LAN address.

## Normal phone mode

The phone has two modes. You can switch at any time except during a call:

- **AI answers my calls** (the default): the takeover described in
  [Use a spare phone](#use-a-spare-phone). A phone that was never switched
  stays in this mode after updates.
- **Normal phone**: calls ring and you answer them yourself. The AI does not
  pick up, nothing is muted, and Google's phone app is back.

**On the phone:** open Bageo and turn the big switch at the top,
**AI answers my calls**, off or on. Confirm, and after a few seconds the card
says *Normal phone: calls ring as usual* (or that the AI answers again).

**From the web console:** *System → Phones*. Each connected phone shows its
name, app version, mode and a button, **Switch to normal phone** or
**Let the AI answer again**. Switching to a normal phone asks for confirmation
first. A phone whose app is older than this feature shows *Update the Bageo
app…* instead of the button.

Switching to a **normal phone** changes these system settings, in this order:

1. Bageo is no longer the call-screening app, so calls are not silenced.
2. Do Not Disturb is turned off.
3. The ringer is set to normal, and the ring, notification, earpiece, media,
   alarm and system volumes go back to what they were before the phone last
   became an AI phone. Where nothing was saved, or the saved value was zero,
   half of the maximum is used.
4. The microphone is no longer muted and the earpiece is no longer held at its
   lowest level.
5. Last, Google Phone (`com.google.android.dialer`) is enabled again and
   becomes the default phone app. On a phone without it, Bageo gives the role
   up and Android uses its own phone app. The switch only counts as done when
   Google Phone really is the default phone app and is not disabled.

The Bageo module makes these changes as root: a small helper it starts at
every boot carries out the switch when the app asks, so the app needs no
root permission for it. The changes go on even when Android stops the Bageo
app on the way (taking the phone-app role away from an app can do that),
which is why the phone app changes last.

Bageo's service keeps running and stays connected to the agent, so you can
switch back from the web console, but it does not answer, silence or mute any
call. The web console shows the phone as a normal phone, and `/health` lists
it in `normal_mode_serials`.

Switching back to **AI answers my calls** first saves the current volumes,
then repeats the module's takeover: Bageo becomes the phone app and the call
screener, the Google Phone app is disabled, Do Not Disturb goes on and
the volumes go to zero.

If a switch stops halfway, the app and the web console say which step did not
work (for example, the default phone app could not be changed). Switching
again repeats every step, and so does the next start of the app (after a
reboot, for example) while a switch is unfinished. If the app says the Bageo
module is not running, check in Magisk or KernelSU that **Bageo Uplink** is
enabled (a module installed before the rename shows as **Myna Uplink**), then
reboot the phone. When the web console gets no answer within
15 seconds it says the result is not confirmed yet: refresh a little later
to see the mode the phone reports.

If you used an app version before 0.2.2 and its **AI answering** switch was
off, that setting no longer counts: after the update the phone answers with
the AI, and the card at the top says so. Turn **AI answers my calls** off if
the phone should stay a normal phone.

The mode is stored in `/data/adb/pixelcallai/mode` (`ai` or `normal`),
outside the module's folder, so it survives reboots and module updates. The
module reads it at every boot; no file means AI mode. The result of the last
switch is in `last_apply` in the same folder, and `pending` exists while a
switch is unfinished. The volumes saved for the normal phone are kept in the
app's own storage.

To switch without the app's screen, from a computer with `adb`:

- Without a reboot (the app does the whole switch at once):

  ```text
  adb shell am start-foreground-service -n com.pixelcallai.streamer/.CallAudioService -a com.pixelcallai.streamer.SET_ENABLED --ez enabled false
  ```

  `--ez enabled true` switches back to **AI answers my calls**.
- By writing the file and rebooting (the app applies the rest when its service
  starts):

  ```text
  adb shell "su -c 'mkdir -p /data/adb/pixelcallai && echo normal > /data/adb/pixelcallai/mode'"
  adb reboot
  ```

  Back to AI mode: write `ai` instead (`echo ai > …`), or remove the file
  with `adb shell "su -c 'rm -f /data/adb/pixelcallai/mode'"`, then reboot.

## Rollback

**Before installing an older Bageo version** (any version before 0.2.2), switch
the phone back to **AI answers my calls**, or delete the state folder:
`adb shell "su -c 'rm -rf /data/adb/pixelcallai'"`. Older versions do not
know the mode file: their module takes the phone over again at every boot,
even when the file says `normal`. When you later install this version
again, it finds `normal` in the file and turns the phone into a normal phone
by itself, so the AI stops answering without anyone asking for it.

1. Switch the phone to a [normal phone](#normal-phone-mode) first. That
   brings back Google's phone app, the ringer and the volumes, and turns off
   Do Not Disturb, so steps 3 and 4 are then already done.
2. Disable or remove the **Bageo Uplink** module in Magisk / KernelSU
   and reboot. If the phone does not boot far enough, use the root manager's
   safe mode, or with root from recovery:
   `touch /data/adb/modules/pixelcallai_uplink/disable`.
   The mode file `/data/adb/pixelcallai/` stays; delete that folder too if
   you do not plan to install Bageo again.
3. If you removed the module without switching first: removing it does
   **not** bring back Google's phone app or turn off Do Not Disturb — Android
   remembers those. Re-enable the phone app:

   ```text
   adb shell su -c "pm enable com.google.android.dialer"
   adb shell su -c "pm enable com.google.android.dialer/com.android.incallui.InCallServiceImpl"
   adb shell su -c "pm enable com.google.android.dialer/com.android.incallui.InCallActivity"
   adb shell su -c "cmd role add-role-holder android.app.role.DIALER com.google.android.dialer"
   adb shell su -c "telecom set-default-dialer com.google.android.dialer"
   ```

4. Turn off Do Not Disturb and raise the volumes in the system settings.

## When it does not work

See [Troubleshooting](TROUBLESHOOTING.md) and the table at the end of
[`android/README.md`](../../android/README.md#troubleshooting).
