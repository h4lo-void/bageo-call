# Phone setup: turn a rooted Android phone into an AI call answerer

The phone is Bageo's line to the telephone network. It holds your SIM, picks
up calls without a sound, streams the caller's voice to Bageo on your computer
and plays the AI's voice back into the call. The AI (Gemini Live) runs through
Bageo on the computer; the phone does no AI work itself.

This page covers:

- which phones work
- installing the root module
- connecting by USB or Wi-Fi
- switching between AI answering and a normal phone
- undoing it all

Building the app, its permissions and its protocol are covered in
[`android/README.md`](../../android/README.md).

This page describes **Bageo 2.1**: the root module `v2.1.0` (versionCode 31) with
the app `2.1.0` (versionCode 39) inside, installed from
`bageo-phone-module-2.1.0.zip` (**Bageo Uplink**, for pairing with Bageo on a
computer or NAS) or
`bageo-phone-standalone-2.1.0.zip` (**Bageo AI Call Agent**, the phone-only
edition: runs the full Bageo engine and console inside the phone with no computer
needed). The app shows its version under *Advanced*, and the console under
*System → Phones*.

| Install file | What it is | Module ID |
|---|---|---|
| `bageo-phone-standalone-2.1.0.zip` | **Bageo AI Call Agent**: phone-only, no computer. All of Bageo runs on the phone. | `bageo_ai_call_agent` (shown as **Bageo AI Call Agent**) |
| `bageo-phone-module-2.1.0.zip` | For Bageo on a computer: use it with the Windows, macOS or Linux installer, or Docker. | `pixelcallai_uplink` (shown as **Bageo Uplink (for Bageo on a computer)**) |

Install only one of the two. When you install one, the other is removed automatically at the next reboot.

> **Want phone-only with no computer?** Flash `bageo-phone-standalone-2.1.0.zip`
> instead. With a free Gemini key from Google AI Studio you pay no API fees; in
> our own tests the free quota covered 10 or more calls a day (Google sets the
> free limit, and it can change). The two editions have different module IDs:
> the phone-only edition is `bageo_ai_call_agent` (shown as *Bageo AI Call Agent*
> in the root manager), the computer edition is `pixelcallai_uplink` (shown as
> *Bageo Uplink (for Bageo on a computer)*). When you install one, the other is removed automatically at the
> next reboot; the app and its settings stay. In the phone-only edition the console
> opens only inside the Bageo app on the phone; other phones and computers cannot
> open it.

## What the phone does

| Job | How |
|---|---|
| Answers calls silently | Lets the call ring for as long as Bageo asks (3 s by default), picks up without a sound and carries both voices between the call and Bageo on the computer. Nobody near the phone hears either side. |
| Switches between AI and normal phone | One switch in the app, or one button in the console. The app needs no root permission: the module's root helper changes the system settings. See [Normal phone mode](#normal-phone-mode). |
| Leaves your own calls alone | A call you dial by hand gets no AI, and the microphone and earpiece work normally; afterwards the phone goes silent again. Only incoming calls and calls Bageo asks for (AI call-backs and errands) get the AI. |
| Sends texts for you | When you answer an appointment by text, the phone that took the call sends it from its own number (`send_sms`). |
| Places calls for Bageo | AI call-backs with your answer to an appointment, and AI errands from the console's *Errands* page (`place_call`). It never dials while ringing or on a call: the new call waits (an errand up to 10 minutes). |
| Picks the right SIM | On a dual-SIM phone, texts and outgoing calls use the SIM Bageo names by its Android subscription id (`sub_id`): the SIM the original call came in on, or for an errand a SIM from the number's country. |
| Takes one call at a time | A second call during a call is declined at once (the caller hears busy) and reported to Bageo (`call_waiting_rejected`). It shows in the call list, and you get a "Missed while on another call" notification. |
| Fails safe | Bageo unreachable while a call rings: the phone does not answer, and the call goes to your carrier's missed call or voicemail. Connection to Bageo lost for 3 seconds during a call: it hangs up instead of leaving the caller in silence. |

## Which phones

- **Tested:** Google Pixel (7, 7a, 8 and 9) and the MediaTek phone Redmi K60 Ultra, rooted with Magisk or KernelSU.
- **Android 10 or newer.**
- **Other rooted Android phones are supported too, but we have not tested
  them, so test yours.** After installing, call the phone from another phone
  and talk to the AI for about 20 seconds. Bageo adjusts by itself and tells
  you the result in the console and the app (see
  [First use on a new phone](#first-use-on-a-new-phone-test-the-call-audio)).
  The app needs two things that many phones keep from apps, even privileged
  ones: recording the other party during a call, and playing sound *into* the
  call. If either is missing, the call is answered but one side hears nothing,
  and the test call shows it. If you try another model, please report what
  happened.
- **Samsung phones with Exynos chips mostly do not work.** They have no system
  path that carries sound into a call. You can still install one: the first
  test call shows the result.

Phones also differ in one detail, which Bageo measures by itself at the start
of every call: whether the phone's call recording picks up the AI's own voice.

| Call recording | What changes | Tested phones |
|---|---|---|
| Without the AI's voice | The caller can cut in while the AI is talking, and the background sound plays through the whole call. | Pixel 9 |
| With the AI's voice | The AI's voice is filtered out of what it hears, so it finishes its sentence before it listens again, and the background sound plays only while the AI speaks. | Pixel 7, 7a |

## Use a spare phone

Installing the module turns the phone into a dedicated call box:

- Bageo becomes the default phone app and the call screening app, and Google's
  phone app is disabled, so no call screen appears.
- Do Not Disturb goes on; ringer and notification volume go to zero.
- During calls the microphone is muted, the speaker is off and the earpiece is
  at its minimum: nothing in the room is picked up and nothing plays out loud.

You can still dial out with another dialer app, but it is no longer an everyday
handset. All of this can be switched off from the app or the console (see
[Normal phone mode](#normal-phone-mode)) and removed for good (see
[Rollback](#rollback)).

## Install

1. **Root** the phone with Magisk or KernelSU (not covered here).
2. If you ever installed the app as a plain APK, uninstall it. Never have both.
3. Copy `bageo-phone-module-<version>.zip` to the phone (for this release
   `bageo-phone-module-2.1.0.zip`). In the Magisk or KernelSU app: *Modules →
   Install from storage* → pick the zip → reboot.
4. Open **Bageo**. The card at the top says in one sentence what the phone is
   doing right now. Below it, **Setup steps** lists what is still missing,
   each with a button:

   | Step | What to do |
   |---|---|
   | Install the Bageo module | Done after the reboot. If not, the module is not active. |
   | Allow Bageo to use the phone and microphone | **Allow** (microphone, phone, call log, notifications). |
   | Make Bageo the default phone app | **Set**. |
   | Make Bageo the call screening app | **Set**. |
   | Let Bageo control Do Not Disturb | **Open settings** and allow it. |
   | Do not let Android put Bageo to sleep | **Allow**, so Android does not stop the service. |
   | Plug into the computer by USB to pair | Turn on USB debugging and plug the phone into the computer running Bageo once; after that it also connects over Wi-Fi. Cannot plug it in (Bageo in Docker)? Tap **Connect by IP address** ([Wi-Fi](#wi-fi)). |

   When every step is done the list folds away, and once the phone is
   connected to the computer the card says *The AI is answering your calls*.
   The connection address, the connection test and the detailed checks are
   under **Advanced**.

   The app itself needs no root access (the module does the root work), so if
   the root manager asks, either answer is fine.

   You are not asked for the permission to send texts (`SEND_SMS`): the module
   grants it at boot and whenever the phone switches to AI mode. A dual-SIM
   phone routes each SIM by its subscription id, while `READ_PHONE_NUMBERS` is
   only used to read the SIM numbers reported to Bageo (if a carrier hides the
   number, you can fill it in on the console's **Phones** tab).
5. Leave **AI answers my calls** on.

### Updating

Install the new zip over the old module the same way (*Install from storage*)
and reboot. Your mode, the connection address and the app's permissions are
kept. Releases from this repository are signed with the project's own key, so
one installs over the other. If you switch between a release and a build of
your own, remove the module and reboot first (see
[`android/README.md`](../../android/README.md#build)).

## Connect it to the computer

### USB (recommended)

1. On the phone: *Settings → About phone*, tap *Build number* seven times,
   then *System → Developer options →* **USB debugging** on.
2. Plug it into the computer running Bageo.
3. Accept **"Allow USB debugging?"** on the phone (tick *Always allow*).

That is all. Bageo finds the phone, maps the app's local address
(`ws://127.0.0.1:8765`) through the cable to the computer and starts the
app's service. It checks again every 15 seconds, so unplugging and replugging,
or restarting either side, heals by itself.

USB also keeps the phone charged and works without Wi-Fi. One computer can
take several phones; each answers its own number.

### Wi-Fi

Plug the phone in by USB once (above) and it is paired: Bageo gives it its
Wi-Fi address and a pairing key. Unplug it afterwards and the phone connects
over Wi-Fi by itself; plug it back in and it uses USB again. The *System* page
shows for each phone whether it is connected by USB or over Wi-Fi.

- The phone and the computer must be on the same network.
- On Windows, the first time a phone is paired the firewall asks whether
  Bageo may use the network: allow it on **private networks**.
- Phones connect on port 8788 (`PHONE_WIFI_PORT`; 8787 when Bageo is open
  to the network with `AGENT_HOST=0.0.0.0`). The phone tries the computer's
  IP address first, then `bageo.local` while the network name is on.
- **Phones can also connect over Wi-Fi** (*Settings → Advanced*) turns this
  off; phones on Wi-Fi are disconnected at once. **Pair phones again** gives a
  new pairing key, for a lost phone: every phone on Wi-Fi must then be plugged
  in by USB once more.
- The pairing key goes only to a phone that is already connected. It is not
  shown in the console, notifications or logs.
- An older app ignores the pairing and keeps using USB only.
- With Docker, or when the phone cannot be plugged in at all, pair it by IP
  address (below).

**Pairing by IP address.** Use this when Bageo runs in Docker or on another
computer and the phone cannot be plugged in.

1. In the web console open *System* and, under **Pair a phone by IP address**,
   click **Get a pairing code**. The code works once, for 10 minutes.
2. In the Bageo app tap **Connect by IP address** (on the status card, in the
   setup steps, or under *Advanced → Connection*) and enter Bageo's IP address
   with its port, for example `192.168.1.20:8787`, and the code.

The phone keeps the address and connects to it again after a restart. It uses
the same port as the console (8787 unless you changed it); with Docker the
host port can be another number: type the one you published. The phone and
Bageo must be on the same network, and **Phones can also connect over Wi-Fi**
must be on. A new code replaces the old one, and five wrong codes void it.
**Pair phones again** also disconnects phones paired this way; pair them again
with a new code.

**Entering the address by hand.** Use this when the phone cannot be cabled to
the computer at all.

1. Make Bageo reachable over the network and ask for a password: in `.env`
   set `AGENT_HOST=0.0.0.0`, `DEVICE_TOKEN=<a long random string>` (for the
   phone's connection) and `CALLS_UI_TOKEN=<another one>` (the console
   password). See [Settings reference](SETTINGS.md).
2. In the app, under *Advanced → Connection*, enter
   `ws://<computer's address>:8787/ws?token=<DEVICE_TOKEN>` and save.
   **Test connection** tells you whether it works and, if not, why in plain
   words (the raw error is under *Details*).

The connection is not encrypted (`ws://`): use it only on your own network, or
put a TLS reverse proxy in front and use `wss://`. Calls are more robust over
USB.

## First use on a new phone: test the call audio

Phones differ in how they let an app play sound into a call. Bageo finds the setting that works on each phone by itself, from one test call: call the new phone from another phone and talk to the AI for about 20 seconds. Bageo sends a notification when a new phone first connects, and another with the result.

| The console and the app show | What it means | What to do |
|---|---|---|
| *Call audio not tested yet* | No call long enough has been made yet. | Make the test call. |
| Nothing | The first settings worked for a whole call. | Nothing. |
| *Call audio was adjusted automatically* | The sound dropped, so Bageo switched settings. The new ones work and are remembered. | Nothing. |
| *Other call audio settings are ready* | You said you heard nothing, so Bageo switched the phone to other settings. This stays until a call has settled on settings. | Call the phone again to test them. |
| *Noted: this phone's call audio has a problem* | You said you heard nothing on a Google Pixel. It has no other settings to try, so Bageo only keeps your report, until a system update. | Report the phone (see below). |
| *Call audio has a problem and all settings were tried* | The sound still dropped with every setting. | Report the phone (see below). |
| *This phone cannot send the AI's voice into calls* | The phone does not allow it. See the three reasons below. | Report the phone (see below). |

After a test call that went well, the phone's card on the console's *System* page asks *Did you hear the AI's voice on the last call?* Answer *Yes* and it does not ask again. Answer *No* and Bageo switches the phone to other settings (a Google Pixel has none, so there it only notes your answer): call it again to test them, and it asks again after that call. A phone that becomes untested again (after a system update) starts a new round, with a new notification.

You get a notification when a new phone connects untested, one with the result of its test, and after that only if the audio turns bad (a problem, or not supported). A phone that is known to work (Google Pixel) gets none, and neither does a change between *works* and *adjusted*. The audio turning bad is told once per round. With no notification channel set, nothing is sent; the first message comes after you set one, if the phone is still untested.

**Why a phone is not supported.** Bageo's log has an `uplink health` line after every call; its `reason=` says which of these it is:

- `no_telephony_device`: the phone lists no sound output meant for calls, so there is nowhere to play the AI's voice.
- `prefer_rejected`: the phone has that output but refuses to send the app's sound to it. Check that the module is installed and the phone was restarted afterwards.
- `track_rejected`: the output is there, but the phone could not open a sound track on it with any of the settings.

**Which phones.** Tested: Google Pixel and the MediaTek phone Redmi K60 Ultra. Other rooted Android phones are supported but untested: make the test call and Bageo shows the result. Samsung phones with Exynos chips mostly do not work, because they have no system path that carries sound into a call; you can still install on one, and the test call shows the result.

**Report a phone.** If the console says *has a problem* or *cannot send*, or you keep answering *No*, open an issue with the phone's model and its `uplink health` line (it holds no phone number or call content). See [Reporting a problem](TROUBLESHOOTING.md#reporting-a-problem).

## Day to day

- While its service runs, the app shows a quiet permanent notification. It
  starts by itself after a reboot.
- To use the phone yourself for a while, switch it to a
  [normal phone](#normal-phone-mode).
- To stop the AI on every phone at once, switch off *AI answers calls* under
  *Basics* in the console (calls are then picked up and hung up).
- With two SIMs, Bageo knows which SIM each call came in on. It picks the
  opening language from *Main language for each SIM* (under *Basics → More
  options*) and sends texts and call-backs from the same SIM.
- AI errands need to know the phone's SIMs. The 1.4+ phone app reports installed
  SIMs automatically when it connects (calling the phone once is only needed for
  older app versions if the console says *This phone hasn't taken a call yet*).
- The **Open the web console** button in the app appears once Bageo has a
  console address: *Address in notifications* under *Notifications* or, when
  that is empty and the console is open to your network, the computer's LAN
  address.

## Normal phone mode

The phone has two modes, and you can switch at any time except during a call:

| Mode | What happens |
|---|---|
| **AI answers my calls** (default) | The takeover described in [Use a spare phone](#use-a-spare-phone). A phone that was never switched stays in this mode after updates. |
| **Normal phone** | Calls ring and you answer them yourself. The AI does not pick up, nothing is muted, and Google's phone app is back. |

**On the phone:** open Bageo and turn the big switch at the top, **AI answers
my calls**, off or on. Confirm; after a few seconds the card says *Normal
phone: calls ring as usual*, or that the AI is answering again.

**In the console:** *System → Phones*. Each connected phone shows its
name, app version, mode and a button: **Switch to normal phone** (asks for
confirmation first) or **Let the AI answer again**. A phone whose app predates
this feature shows *Update the Bageo app on this phone to switch it from here*
instead.

### What the switch changes

Switching to a **normal phone** changes these system settings, in this order:

1. Bageo stops being the call screening app, so calls are no longer silenced.
2. Do Not Disturb goes off.
3. The ringer goes back to normal, and the ring, notification, earpiece,
   media, alarm and system volumes return to what they were before the phone
   last became an AI phone. Where nothing was saved, or the saved value was
   zero, half of the maximum is used.
4. The microphone is no longer muted, and the earpiece is no longer held at its
   lowest level.
5. Last, Google Phone (`com.google.android.dialer`) is enabled again and
   becomes the default phone app. On a phone without it, Bageo gives the role
   up and Android uses its own phone app. The switch only counts as done when
   Google Phone really is the default phone app and is not disabled.

The module makes these changes as root: a small helper it starts at every boot
carries out the switch when the app asks, so the app needs no root permission.
Taking the phone-app role away from an app can make Android stop it; the
helper keeps going regardless, which is why the phone app changes last.

In normal mode Bageo's service keeps running and stays connected, so you can
switch back from the console, but it does not answer, silence or mute any
call. The console shows the phone as a normal phone, and `/health` lists it in
`normal_mode_serials`.

Switching back to **AI answers my calls** first saves the current volumes, then
repeats the module's takeover: Bageo becomes the phone app and the call
screener, Google's phone app is disabled, Do Not Disturb goes on and the
volumes go to zero.

### If a switch does not finish

- The app and the console name the step that failed (for example, *the
  phone could not change its default phone app*). Switching again repeats every
  step, and so does the next start of the app (after a reboot, say) while a
  switch is unfinished.
- If the app says the Bageo module is not running, check in Magisk or KernelSU
  that **Bageo Uplink** (computer edition) or **Bageo AI Call Agent** (phone-only
  edition) is enabled, then reboot the phone.
- The app shows the mode the phone is really in, not the one you asked for: it changes only after the switch has finished. A switch to AI answering that fails puts the phone back as a normal phone, and at the next boot the module repairs a normal phone that was left half switched (Google Phone turned off, or Bageo still the phone app).
- If the app says *Do Not Disturb is still on* when you switch to a normal phone,
  open **Settings → Modes** on the phone, turn off the mode the app names (for
  example *Do Not Disturb (Bageo)* or *Bedtime*), then switch again. The app does
  not retry this one by itself. On Android 15 and later, app 1.4.0 and earlier
  turned on a mode of its own, named after Bageo, that the module cannot turn
  off; newer versions no longer do, and turn it off before switching to a normal
  phone.
- When the console gets no answer within 15 seconds, it says *Result not
  confirmed yet*: refresh a little later to see the mode the phone reports.

### Where the mode is stored

The mode lives in `/data/adb/pixelcallai/mode` (`ai` or `normal`), outside
the module's folder, so it survives reboots and module updates. The module
reads it at every boot; no file means AI mode. The result of the last switch is
in `last_apply` in the same folder, and `pending` exists while a switch is
unfinished. The volumes saved for normal mode are kept in the app's own
storage.

### Switching with adb

To switch without touching the phone's screen, from a computer with `adb`:

- Right away, without a reboot (the app does the whole switch):

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

  Back to AI mode: write `ai` instead (`echo ai > …`), or remove the file with
  `adb shell "su -c 'rm -f /data/adb/pixelcallai/mode'"`, then reboot.

## The audio helper

Some phones do not let the app put its voice into a call by itself. For those, the
module carries a small root program, `bageo-audio-helper`, that the app can ask to
adjust the phone's audio during a call. Phones that do not need it never run it.
This version does not change anything yet: no phone has been added to its list. It
only serves the release build of the app that the module installs.

- It does not run in the background. It starts only when the app asks for it, and
  quits by itself about a minute after the app stops using it.
- At boot it runs briefly only if a call left changed audio settings behind, to put
  them back.
- Settings it changed during a call are put back when the call ends. If that fails,
  they are kept and tried again at the next start or boot, up to 5 times.

If something seems wrong, as root (`adb shell su -c '…'`), with the helper at
`/data/adb/modules/pixelcallai_uplink/bin/bageo-audio-helper`
(`bageo_ai_call_agent` for the phone-only edition):

1. Check whether it runs: `bageo-audio-helper status`.
2. Put the settings back: `bageo-audio-helper restore`. After 5 failed tries, use
   `restore --force`.
3. If they can never be put back (for example the setting no longer exists), give
   them up: `bageo-audio-helper restore --discard`. Nothing is put back, and the
   helper can be used again.
4. Turn it off: `bageo-audio-helper disable` (`enable` turns it back on). It stops
   within about a second.
5. If that does not help, restart the phone. Most audio settings go back to normal
   after a restart, but not on every phone.

## Rollback

To remove Bageo:

1. Switch the phone to a [normal phone](#normal-phone-mode) first. That brings
   back Google's phone app, the ringer and the volumes and turns off Do Not
   Disturb, so steps 3 and 4 are already done.
2. Disable or remove the Bageo module in Magisk or KernelSU (**Bageo Uplink** for
   the computer edition, **Bageo AI Call Agent** for the phone-only edition) and
   reboot. If the phone does not boot far enough, use the root manager's safe
   mode, or run one of these with root from recovery:
   `touch /data/adb/modules/pixelcallai_uplink/disable` (computer edition) or
   `touch /data/adb/modules/bageo_ai_call_agent/disable` (phone-only edition).
   The folder
   `/data/adb/pixelcallai/` with the mode file stays; delete it too if you do
   not plan to install Bageo again.
3. If you removed the module without switching first: removing it does **not**
   bring back Google's phone app or turn off Do Not Disturb, because Android
   remembers both. Re-enable the phone app:

   ```text
   adb shell su -c "pm enable com.google.android.dialer"
   adb shell su -c "pm enable com.google.android.dialer/com.android.incallui.InCallServiceImpl"
   adb shell su -c "pm enable com.google.android.dialer/com.android.incallui.InCallActivity"
   adb shell su -c "pm enable com.google.android.dialer/com.android.incallui.LegacyInCallActivity"
   adb shell su -c "cmd role add-role-holder android.app.role.DIALER com.google.android.dialer"
   adb shell su -c "telecom set-default-dialer com.google.android.dialer"
   ```

4. Turn off Do Not Disturb and raise the volumes in the system settings.

## When it does not work

See [Troubleshooting](TROUBLESHOOTING.md) and the table at the end of
[`android/README.md`](../../android/README.md#troubleshooting).


<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
