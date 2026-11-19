# Bageo for Android: rooted call audio bridge (Magisk / KernelSU module)

This folder holds the phone side of Bageo, which lets an AI voice (Gemini Live)
answer calls to your own phone number. It builds an Android app and a Magisk /
KernelSU module. Together they turn a spare rooted phone, such as a Pixel, into an
audio bridge: the app answers calls on your SIM, streams the caller's voice to the
Bageo agent on your computer and plays the AI's reply back into the call.

| Part | What it does |
|---|---|
| `app/` | The Android app `com.pixelcallai.streamer`. Answers incoming calls silently, streams the caller's audio to the agent over a WebSocket and plays the AI's voice into the call. For the agent it also sends text messages (appointment replies) and places calls (AI call-backs and AI errands), each from the SIM the agent names. |
| `magisk/pixelcallai_uplink/` | The module. It installs the app as a *privileged system app*, because Android only lets privileged apps record call audio and write audio into a call; a normal install cannot do the job. Its root helper also carries out the switch between AI mode and a normal phone. |

The phone does no AI work itself. The package name and module id keep the project's
earlier working name, so phones already running the module update in place.

**Version 1.6.9:** app `versionName` `1.6.9`, `versionCode` 33; module `version`
`v1.6.9`, `versionCode` 25 (`module.prop`); release file
`bageo-phone-module-1.6.9.zip`. A SIM is always picked by its subscription id (`sub_id`).

> **Use a dedicated phone.** The module takes over the phone app, disables Google
> Dialer, turns on Do Not Disturb and sets the ringer volume to zero. The phone is not
> a usable handset until you switch it to a [normal phone](#normal-phone-mode) or
> [roll back](#rollback).

| I want to… | Go to |
|---|---|
| Check whether my phone can work | [Requirements](#requirements) |
| Build the APK and the module zip | [Build](#build) |
| Put it on the phone and connect it | [Install](#install), [Connect over USB or Wi-Fi](#connect-over-usb-or-wi-fi) |
| Know exactly what the module changes | [What changes on the phone](#what-changes-on-the-phone) |
| Use the phone as a normal phone again | [Normal phone mode](#normal-phone-mode) |
| Remove Bageo | [Rollback](#rollback) |
| See which permissions it uses and why | [Permissions](#permissions) |
| Write or debug an agent | [How a call flows](#how-a-call-flows), [WebSocket protocol](#websocket-protocol) |
| Forward incoming SMS (for example to Telegram) | [Optional SMS hook](#optional-sms-hook) |
| Fix a problem | [Troubleshooting](#troubleshooting) |

## Requirements

| You need | Details |
|---|---|
| Root | An unlocked bootloader and Magisk or KernelSU. |
| SIM | One that can receive calls. |
| Android | 10 or newer (`minSdk 29`). |
| Phone | **Tested: Pixel 7, 7a, 8 and 9.** Comments in the source refer to Android 16 on those phones. Other phones are untested. |
| Computer | Running the Bageo agent, connected by USB (recommended) or on the same network. |

On other phones, success depends on the device's telephony audio HAL. It must let a
privileged app record the call (`VOICE_DOWNLINK` / `VOICE_CALL` audio source) and
play into the call through an output device of type `TYPE_TELEPHONY`. A phone that
lacks either will not work. Try at your own risk.

## Build

You need **JDK 17** and the **Android SDK** (platform `android-35`; the Android
Gradle Plugin downloads matching build tools if they are missing and the SDK licenses
are accepted). Point Gradle at the SDK with the `ANDROID_HOME` environment variable
or a `local.properties` file in this folder:

```properties
sdk.dir=/path/to/android-sdk
```

Build the APK and package the module zip in one step, from the `android` folder:

```powershell
# Windows (PowerShell), from the android folder
.\scripts\package-uplink-module.ps1
```

```bash
# macOS / Linux, from the android folder
./scripts/package-uplink-module.sh
```

Both scripts run `:app:assembleDebug`, copy the APK into the module as
`system/priv-app/com.pixelcallai.streamer/base.apk`, and write

```text
magisk/dist/bageo-phone-module-<versionName>.zip
```

To build only the APK, run `.\gradlew.bat :app:assembleDebug` (Windows) or
`sh ./gradlew :app:assembleDebug` (macOS / Linux). It is written to
`app/build/outputs/apk/debug/app-debug.apk`.

The debug build is minified on purpose so the app stays in a single dex file: as a
privileged app on Android 16, a second dex file caused `ClassNotFoundException` at
start-up.

### Signing keys

- **Your own build** is signed with the debug key of the machine that builds it,
  unless `PCA_KEYSTORE_FILE`, `PCA_KEYSTORE_PASSWORD`, `PCA_KEY_ALIAS` and
  `PCA_KEY_PASSWORD` point to a key of your own.
- **GitHub releases** are signed with the project's key, kept as repository secrets
  (`ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS`,
  `ANDROID_KEY_PASSWORD`), so one release installs over the previous one.
- **Changing keys.** Android ties an installed app to its signing key. A release-key
  module installed over a debug-key build kept the app's data and permissions (Android
  logs *signature changed; retaining data*). Other key changes are untested: remove the
  module and reboot before installing the other build.

## Install

1. If you ever installed the APK as a normal app (`adb install`), uninstall it.
   Never have a normal install and the module at the same time.
2. Copy `bageo-phone-module-<version>.zip` to the phone.
3. In the Magisk or KernelSU app, install the zip as a module, then reboot.
4. Open **Bageo**. The card at the top says in one sentence what the phone does right
   now. Work through **Setup steps** until every step shows OK:

   | Step | Action |
   |---|---|
   | *Install the Bageo module* | Confirms the module is active (privileged app and privileged permissions). |
   | *Allow Bageo to use the phone and microphone* | Tap **Allow**. |
   | *Make Bageo the default phone app*, *Make Bageo the call screening app* | Tap **Set**. |
   | *Let Bageo control Do Not Disturb*, *Do not let Android put Bageo to sleep* | Tap the button and allow it. |
   | *Plug into the computer by USB to pair* | Turn on USB debugging and plug into the computer running Bageo once so it can send its Wi-Fi addresses and pairing key. |

   The individual permissions and the call-audio check (*AI voice injection into the
   call*) are under **Advanced → Detailed checks**.
5. In **Advanced → Connection**, keep the USB default or enter the Wi-Fi address (see
   below), then tap **Test connection** while the agent is running. A failure is shown
   as a plain reason (refused, timed out, address not found…); the raw error is under
   **Details**.
6. Leave **AI answers my calls** on. The app shows a permanent low-priority
   notification while its service runs and starts again by itself after a reboot.
   Once the agent has sent the console's address, **Open the web console** opens it.

The app needs no root for anything essential: the module grants the permissions,
takes the phone over at boot and runs the mode switch. If the root manager asks, allowing
root only lets the app repeat the role and mute commands
([below](#while-the-apps-service-runs)) each time its service starts, which helps after
another app took the dialer role. KernelSU only offers `su` to apps on its allow list.

### Updating

Install the new module zip over the old one and reboot. The mode file, the connection
address and the app's permissions are kept (mind [Signing keys](#signing-keys)).

### Connect over USB or Wi-Fi

| | USB (default, recommended) | Wi-Fi |
|---|---|---|
| Address on the phone | `ws://127.0.0.1:8765` | Automatic after plugging in once over USB (`agent_info.wifi.urls`, port `8788` with a pairing key); or enter `ws://<host>:8787/ws?token=<DEVICE_TOKEN>` by hand (required for Docker or when the phone cannot be plugged in). Must start with `ws://` or `wss://`. |
| How it reaches the agent | `adb reverse tcp:8765 tcp:<agent port>` maps the port through the cable. Bageo sets this up by itself and re-checks it every few seconds. | Phone and PC must be on the same network. When paired, `ConnectionPlan` tries the LAN IP first and `bageo.local` next, and switches back to USB when plugged in. |
| Good to know | Keeps working with Wi-Fi off. | Anything after `?` is left out of the app's log output. **Forget Wi-Fi connection** clears paired addresses until plugged in again. |

The address and paired Wi-Fi URLs are stored on the phone in device-protected storage, so they are available before the first unlock after a reboot. **Reset to USB default** removes a typed address. After you save a new address the app restarts its service; if a call is in progress it asks you to restart the service afterwards.

## What changes on the phone

### At every boot (`service.sh`)

The script waits for the system to finish booting (up to about 3 minutes), then 10 more
seconds, then until the package manager knows the app (it gives up after about a
minute and does nothing). Every command is best effort; none of them can stop the boot.

It starts the module's root helper, `mode_daemon.sh`, and reads the phone mode from
`/data/adb/pixelcallai/mode` (see [Normal phone mode](#normal-phone-mode)). If the
file says `normal`, only steps 1, 2, 6 and 7 run. With no file, or anything else in
it, all steps run:

1. Creates `/data/local/tmp/pixelcallai_uplink.pcm` (mode 666), a debugging aid the
   normal call path does not use.
2. Grants the app `RECORD_AUDIO`, `READ_PHONE_STATE`, `ANSWER_PHONE_CALLS`,
   `SEND_SMS` and `ACCESS_NOTIFICATION_POLICY`, and allows the
   `ACCESS_NOTIFICATION_POLICY` app-op.
3. Gives the app the **Dialer** and **Call screening** roles and makes it the default
   dialer (`telecom set-default-dialer`).
4. Only if the app really is the default dialer: disables Google Dialer's in-call
   components (`InCallServiceImpl`, `InCallActivity`), then the whole
   `com.google.android.dialer` package for user 0 (`pm disable-user`).
5. Turns on Do Not Disturb in its strictest mode (`zen_mode 2` and
   `cmd notification set_dnd on`; Android puts a bare `zen_mode` write back) and sets
   the ring and notification volume settings to 0.
6. Starts the app's foreground service.
7. Only if the LSPosed-compatible module `zygisk_vector` is installed: enables this
   app as an LSPosed module scoped to `com.android.phone` (see
   [Optional SMS hook](#optional-sms-hook)).

Earlier in boot, `post-fs-data.sh` sets file modes and SELinux labels on the APK and
the permission file and overlays them onto `/system/priv-app` and
`/system/etc/permissions` (work directory `/dev/pca_ov`). The module ships
`system/etc/permissions/privapp-permissions-com.pixelcallai.streamer.xml`, the
allow-list that lets Android grant the privileged permissions.

### While the app's service runs

In AI mode the service:

- Repeats the role commands of step 3 and the Google Dialer disabling of step 4 through
  `su` each time it starts. Unlike `service.sh`, it does not first check that it is
  the default dialer.
- Mutes the microphone, turns the speakerphone off and holds the in-call earpiece
  volume at its minimum. During a call it repeats the microphone mute every 500 ms.
- The first time it arms: sets media, ring, notification, system, alarm and DTMF volume
  to 0 and switches the ringer to silent. On Android 14 and below it also turns on
  Do Not Disturb; on Android 15 and later it skips `setInterruptionFilter` (because
  Android 15 creates an app-owned Zen mode the root module cannot disable) and leaves
  Do Not Disturb to the root module (`service.sh` / `apply_mode.sh`), while turning off
  any legacy app-owned mode before switching back to a normal phone.
- With root, also runs `cmd media.audio_flinger mute mic 1`, the volume settings
  above and several `tinymix` microphone mute controls (controls a device lacks are
  ignored).
- On **Stop service**, restores the microphone, speakerphone, audio mode, ringer mode,
  Do Not Disturb filter and volumes it saved when it armed. The earpiece volume stays
  at minimum.

In normal phone mode the service only keeps the control connection to the agent
(status, the remote mode switch, and `send_sms`, which still works). It does not
repeat the role commands, mute anything, or silence or answer calls.

With the app as default dialer and Google Dialer disabled, no full-screen call screen
appears. Android may still show its own small "ongoing call" indicator in the status
bar; that belongs to the system and the app does not hide it.

## Normal phone mode

The app's main switch, **AI answers my calls**, moves the phone between two modes. The
web console does the same remotely (`set_mode`, see the
[protocol](#websocket-protocol)). No reboot is needed, and the app needs no root
permission: the module's helper runs the switch as root.

| | AI mode (`ai`, default) | Normal phone (`normal`) |
|---|---|---|
| Default phone app | Bageo; Google Dialer disabled | Google Dialer, enabled again (on phones without it, Android's own) |
| Call screening role | Bageo (silences the ringer) | Removed from Bageo |
| Do Not Disturb | On | Off |
| Ringer and volumes | Silent and zero; earpiece at minimum | Ringer normal; ring, notification, earpiece, media, alarm and system volume restored |
| Microphone | Muted during calls | Left alone |
| Incoming calls | Answered by the AI | Ring; you answer |
| App service | Full call handling | Control connection only |

A switch is refused while a call rings or is active. It can safely be repeated: every
step runs again and is checked, and the first one that does not take effect is
reported as a code:

| Code | Meaning |
|---|---|
| `in_call` | A call rings or is active. |
| `busy` | Another switch is still running. |
| `invalid_mode` | The request named neither `ai` nor `normal`. |
| `no_root` | The module's helper did not take the request: the module is not running. Enable **Bageo Uplink** in Magisk or KernelSU and reboot. |
| `dialer_failed`, `screening_failed`, `dnd_failed`, `volume_failed`, `save_failed` | That step did not take effect. With `dnd_failed` on the way to a normal phone, the app names the mode that kept Do Not Disturb on (`dnd_mode` below) and does not retry by itself. |
| `failed` | Any other failure, for example the request could not be written or the helper gave no usable answer. |

### How the switch runs

The system settings are changed by the module's root script,
`magisk/pixelcallai_uplink/apply_mode.sh`. The app cannot count on `su`, so at
every boot `service.sh` starts `mode_daemon.sh`, a small root helper in the same
folder, in its own session.

1. The app writes a one-line request, `<ai|normal> <id> [six volumes]`, to
   `files/mode/request` in its device-protected storage
   (`/data/user_de/0/com.pixelcallai.streamer/`).
2. The helper looks every 2 seconds, takes the request and checks it. A malformed
   request is not run and is reported as `error=invalid_request`. A request nobody
   takes within 10 seconds is withdrawn and reported as `no_root`.
3. The helper runs `sh apply_mode.sh` and writes its answer to `files/mode/state`.

The script runs outside the app, so it finishes even if Android stops the app, which
removing an app from the phone-app role can do. For the same reason the order towards
a normal phone is call screening, Do Not Disturb, ringer and volumes, microphone, and
**the phone app last**. That last step only counts as done when
`telecom get-default-dialer` reports Google Dialer, Google Dialer is not in
`pm list packages -d` and both in-call components were enabled (or, without Google
Dialer, when the default phone app is no longer Bageo). If `pm list packages` itself
fails, the step fails (`dialer_failed`); it never assumes Google Dialer is absent.
Every root command runs under `toybox timeout`.

### Where the mode is stored

| Path | Contents |
|---|---|
| `/data/adb/pixelcallai/mode` | `ai` or `normal`, written by the script when the app switches. Outside the module folder, so module updates keep it. `service.sh` reads it at every boot; **no file means `ai`**, so a phone that was never switched behaves as before. `service.sh`, the script and the app all drop blanks (space, tab, CR, LF, VT, FF) and a UTF-8 BOM before comparing. |
| `/data/adb/pixelcallai/pending` | Exists from the start of a switch until every step worked. |
| `/data/adb/pixelcallai/last_apply` | The last run as `key=value` lines: `id`, `mode`, `step_<name>=ok\|fail`, then `error=<code>` or `result=ok`, and `done=1`. When Do Not Disturb will not go off, also `dnd_blocker=app\|mode\|manual\|unknown` (Bageo's own mode, another mode, the phone's own Do Not Disturb, not found) and `dnd_mode=<name>` (the mode's name in Settings > Modes, Android 15+). |
| `/data/adb/pixelcallai/apply.lock` | Held while a run is going (its pid inside). |
| `/data/adb/pixelcallai/daemon.pid` | The helper's pid; `service.sh` removes it at boot. |
| `files/mode/state` (app's device-protected storage) | The helper's answer, owned by the app and readable only by it. `key=value` lines: `daemon=1`, `mode`, `pending`, `running`, `updated` and `uptime` (the app ignores a state written before the last reboot), `request` and `exit` (the last request and the script's exit code once it ended), `error=invalid_request`, and every `last_apply` line as `log.<key>`. Written when the helper starts, before and after every run, and whenever the file is missing. |
| `files/mode/normal_audio` (same folder) | The volumes saved when the phone last went from normal to AI (`ring=…`, `notification=…`, `voice_call=…`, `music=…`, `alarm=…`, `system=…`). Switching to normal restores them; a missing or zero value becomes half of that volume's maximum. |
| `files/phone_mode` (same storage) | The app's own copy of the mode, cached in memory per process; the `:screening` process reads it again for every incoming call. |

### After a restart or an interrupted switch

When the service starts, it waits for a state the helper wrote since this boot (at
boot the helper starts after the app). It then asks for the switch again if
`pending` exists, if the mode is `normal` but Bageo is still the default phone
app (an interrupted switch, or an older version that took the phone over), or if its
own copy differs from the file. If the agent asked for the switch and the app was
stopped before it could answer, it sends that `set_mode_result` after its next
`hello`; the agent keeps waiting through the reconnect (up to 15 seconds), and the
web console calls a timeout "not confirmed yet".

In normal mode the service runs as a foreground service of type `phoneCall` only
(not `microphone`); if Android refuses the foreground start, the app logs it and
still opens the control connection.


### Switching from adb

The older `com.pixelcallai.streamer.SET_ENABLED` service intent performs the same
switch without a reboot (`--ez enabled true` = AI mode):

```text
adb shell am start-foreground-service -n com.pixelcallai.streamer/.CallAudioService -a com.pixelcallai.streamer.SET_ENABLED --ez enabled false
```

Or write the mode file and reboot (`echo ai`, or `rm -f` the file, for AI mode):

```text
adb shell "su -c 'mkdir -p /data/adb/pixelcallai && echo normal > /data/adb/pixelcallai/mode'"
```

## Rollback

To remove Bageo:

1. Switch the phone to a [normal phone](#normal-phone-mode). That re-enables Google
   Dialer, turns Do Not Disturb off and restores the volumes, so steps 3 and 4 below
   are then already done.
2. In the Magisk or KernelSU app, disable or remove the **Bageo Uplink** module and
   reboot. If the phone does not boot far enough for that, use the root manager's
   safe mode, or from `adb shell` / recovery with root:

   ```text
   touch /data/adb/modules/pixelcallai_uplink/disable
   reboot
   ```

   `/data/adb/pixelcallai/` (the mode file) stays; delete it too for a clean removal.
3. Removing the module does **not** restore Google Dialer or Do Not Disturb, because
   Android stores those settings. Re-enable the dialer:

   ```text
   adb shell su -c "pm enable com.google.android.dialer"
   adb shell su -c "pm enable com.google.android.dialer/com.android.incallui.InCallServiceImpl"
   adb shell su -c "pm enable com.google.android.dialer/com.android.incallui.InCallActivity"
   adb shell su -c "pm enable com.google.android.dialer/com.android.incallui.LegacyInCallActivity"
   adb shell su -c "cmd role add-role-holder android.app.role.DIALER com.google.android.dialer"
   adb shell su -c "telecom set-default-dialer com.google.android.dialer"
   ```

4. Turn Do Not Disturb off and raise the ring, notification and call volume in the
   system settings.

Do not disable `com.android.phone`: that is the telephony stack itself.

## Permissions

**Privileged permissions.** Granted only because the module installs the app under
`/system/priv-app` and ships the allow-list file.

| Permission | Why |
|---|---|
| `CAPTURE_AUDIO_OUTPUT` | Record the call audio (`VOICE_DOWNLINK` / `VOICE_CALL` sources). |
| `MODIFY_PHONE_STATE` | Play the AI's audio into the call through the `TYPE_TELEPHONY` output. |
| `CONTROL_INCALL_EXPERIENCE` | Run the app's invisible in-call service. |
| `WRITE_SECURE_SETTINGS` | Declared and allow-listed; the current code changes settings through `su` instead. |
| `INTERACT_ACROSS_USERS`, `MANAGE_USERS` | In the allow-list file only; the app does not declare them. |

**Granted on the setup screen.**

| Permission | Why |
|---|---|
| `RECORD_AUDIO` | Required by Android for any audio recording, including call audio. |
| `READ_PHONE_STATE` | Know when a call rings, connects and ends, and read active SIM subscriptions (`sub_id`, country, slot, carrier). |
| `READ_PHONE_NUMBERS` | Read each SIM's own phone number on Android 13+ when the card exposes one, so the console can display it. |
| `READ_CALL_LOG` | Read the caller's number when it did not arrive any other way. |
| `ANSWER_PHONE_CALLS` | Answer and hang up automatically. |
| `CALL_PHONE` | Place a call when the agent asks for a call-back or errand. |
| `POST_NOTIFICATIONS` (Android 13+) | Show the service notification. |

**Other declared permissions.**

| Permission | Why |
|---|---|
| `INTERNET` | WebSocket connection to the agent. |
| `FOREGROUND_SERVICE`, `FOREGROUND_SERVICE_MICROPHONE`, `FOREGROUND_SERVICE_PHONE_CALL` | Keep the call service alive. |
| `MODIFY_AUDIO_SETTINGS` | Mute the microphone, set volumes. |
| `ACCESS_NOTIFICATION_POLICY` | Turn on Do Not Disturb. |
| `RECEIVE_BOOT_COMPLETED` | Start again after a reboot. |
| `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` | Ask for the battery optimization exemption. |
| `MANAGE_OWN_CALLS` | Declared; not used by the current code. |
| `SEND_SMS` | Send a text when the agent asks (`send_sms`, for example to reply to an appointment request). The module grants it at boot and when the phone switches to AI mode; the setup screen does not ask for it. |

SIMs are still matched strictly by Android's subscription id (`sub_id`, covered by
`READ_PHONE_STATE`), never by their phone numbers; `READ_PHONE_NUMBERS` is only used
to show each SIM's number in the console when the card exposes one. The manifest
sets `usesCleartextTraffic="true"`, because `ws://` to localhost or to a PC on
your own network is not encrypted.

## How a call flows

**Incoming call**

1. **Idle.** The service keeps a *control* WebSocket open to the agent and sends a
   `ping` every 15 seconds.
2. **Ringing.** The app waits up to 12 seconds for the control connection, tells the
   agent about the call (`incoming`) and lets it ring for as long as the agent's
   `answer_plan` says (3 seconds if the agent does not reply within 500 ms, never
   more than 8 seconds). Without a control connection within 12 seconds, the app does
   not answer and the call rings until the carrier ends it (missed call or voicemail).
   An `answer_plan` with `reject` declines the call without answering it.
3. **Answer.** The app answers with no sound or screen on the phone.
4. **In call.** It opens a second *media* WebSocket, sends `hello`, streams the
   caller's audio to the agent and plays the audio it receives into the call. If the
   media connection drops and is not back within 3 seconds, the app ends the call so
   the caller is not left in silence.
5. **End.** The agent sends `hangup`, or the caller hangs up and the app sends
   `call_end`.

A call you dial by hand gets no AI, and its microphone and earpiece work normally. A second call that rings during a call is declined at once
(`call_waiting_rejected` tells the agent); the call in progress is not touched.

**Outgoing call for the agent** (call-back or errand)

1. The agent sends `place_call` with the number (E.164 for errands), a `call_id`,
   a `task_id` and usually a `sub_id`.
2. The app dials from the SIM with that subscription id (Android's default when that
   SIM is not found) and answers `place_call_ack`, or answers `place_call_failed`
   without dialing ([reasons](#control-connection-open-while-the-service-runs); on
   `busy` the agent queues the call).
3. When the call connects, the app sends `outbound_ready`, opens the media WebSocket
   and sends `hello` with `sim_sub_id`. The agent checks that the call went out on
   the SIM it asked for; an errand on another SIM is hung up at once, before the AI
   speaks.
4. Audio flows as for an incoming call. When the call ends the app sends `call_end`
   with the same `call_id` and `task_id`. A requested call that has not started
   dialing 30 seconds later is forgotten, so a later call is not mistaken for it.

Only incoming calls and calls the agent asked for get an AI session. More detail is in
[ARCHITECTURE.md](ARCHITECTURE.md).

## WebSocket protocol

Derived from the app source (`AgentControlClient.kt`, `BridgeClient.kt`,
`CallAudioService.kt`, `SmsSender.kt`). The app is the WebSocket client for both
connections, and both use the same address. Text frames are JSON objects with a
`type` field; fields the app does not know are ignored. With the default address
`ws://127.0.0.1:8765` the handshake requests path `/`; otherwise the app requests
exactly the path and query you configure.

`serial` is `Build.getSerial()`; if Android refuses it, the app sends
`pixel-<app uid>` instead.

### Control connection (open while the service runs)

App to agent:

| `type` | Fields | When |
|---|---|---|
| `hello` | `serial`, `mode` = `"CONTROL_IDLE"`, `play_tts_locally` = `false`, `model`, `app_version`, `phone_mode` (`"ai"` / `"normal"`), `in_call`, `supports_sms` = `true`, `supports_reject` = `true`, `sims` (optional array) | Right after connecting. `phone_mode` is not called `mode` because `mode` names the connection. Without `supports_reject` an app would answer a call the agent wants rejected. `SimList.kt` attaches `sims` (`[{"sub_id", "country", "slot"?, "carrier"?, "number"?}]`) whenever active SIMs can be read. |
| `ping` | `t` (milliseconds since epoch) | Every 15 s. |
| `status` | `mode` (`"ai"` / `"normal"`), `in_call`, `sims` (when changed or ready) | The phone mode, call state (ringing or active vs idle), or active SIM list changed. |
| `set_mode_result` | `id`, `ok`, `mode`, `error` | Answer to `set_mode`. `mode` is the mode afterwards; `error` is empty on success, otherwise a code from [Normal phone mode](#normal-phone-mode). |
| `incoming` | `serial`, `caller_id`, `sim_country`, `sample_rate`, `sim_sub_id` (only when known) | A call is ringing. `caller_id` is empty for a hidden number. `sim_country` (ISO code, lowercase, empty when unknown) and `sim_sub_id` (Android subscription id) name the SIM the call came in on. |
| `incoming_cancel` | `serial` | The call ended before it was answered. |
| `reject_failed` | `serial` | Declining a ringing call failed (the app retries once after a short delay). |
| `call_waiting_rejected` | `serial`, `caller_id`, `sim_country`, `sim_sub_id` (only when known) | A second call rang during a call and was declined (AI mode). The fields describe the declined call. |
| `place_call_ack` | `call_id`, `task_id`, `number`, `reason` = `""` | A requested call was dialed. |
| `place_call_failed` | `call_id`, `task_id`, `number`, `reason` | A requested call was not dialed. `reason`: `busy` (a call was ringing or up), `ai_call_disabled` (normal phone mode), `missing number or call_id`, or `dial_helper_failed` (Android refused to place it). |
| `outbound_ready` | `call_id`, `task_id`, `number` | A requested call connected. |
| `call_end` | `call_id`, `task_id`, `serial`, `reason` = `"normal"` | A requested call ended. |
| `send_sms_result` | `id`, `ok`, `error` (only when `ok` is `false`) | Answer to `send_sms`, sent once every part was reported sent, one part failed, or 60 seconds passed. `error`: `no_permission`, `bad_request`, `no_service`, `radio_off`, `generic_failure`, `timeout` or `exception`. If the parts are reported after a `timeout` answer, a second answer with the real result follows. The answers for the last 20 ids are sent again after the control connection reconnects; the agent drops repeats by `id`. |
| `send_sms_delivered` | `id`, `ok` | The network's delivery report: `true` once every part reached the recipient, `false` when a part failed for good. Not sent when the network gives no report (the app stops waiting after an hour). Kept and re-sent like `send_sms_result`. |

Agent to app:

| `type` | Fields | Effect |
|---|---|---|
| `answer_plan` | `delay_ms`, `prewarm`, `reject` (optional) | How long to let the call ring before answering. `prewarm` is only logged. `reject` = `true`: decline the ringing call without answering (`incoming_cancel` follows once it has ended); also honored after the 500 ms wait, as long as the call still rings. |
| `place_call` | `number`, `call_id`, `task_id`, `sub_id` (optional) | Dial `number` from the SIM with subscription id `sub_id` when given (Android's default when that SIM is not found). Answered with `place_call_ack` or `place_call_failed`. |
| `set_mode` | `mode` (`"ai"` / `"normal"`), `id` | Switch the phone mode; answered with `set_mode_result`. |
| `send_sms` | `id` (1–64 characters), `to` (3–20 digits, optional leading `+`), `text` (1–500 characters), `sub_id` (optional) | Send `text` from the phone's own number: the SIM with subscription id `sub_id`, else the default SMS SIM. Answered with `send_sms_result` and, when the network reports it, `send_sms_delivered`. A repeat of an id still being sent is ignored; a repeat of an id already sent is answered again without sending twice. |
| `agent_info` | `console_url`, `wifi` (optional: `{"urls": [...]}`) | The web console's address for **Open the web console**, plus the paired Wi-Fi WebSocket addresses (`urls`). Sent after `hello`; omitting `wifi` keeps stored URLs, while an empty `urls` list clears them. |
| `blocklist_sync` | `numbers` (array) | Stored but unused: the agent decides rejections with `answer_plan` `reject`. |
| `hello_ack`, `hello_ok`, `ack`, `pong` | | Logged only. |

If the control connection drops, the app reconnects after 1.5 seconds.

### Media connection (one per answered call)

App to agent:

| `type` | Fields | When |
|---|---|---|
| `hello` | `serial`, `caller_id`, `sample_rate` (16000), `mode` = `"SILENT_LOCAL"`, `sim_country`, `sim_sub_id` (only when known), `play_tts_locally` = `false`, `local_menu` (`false` in this build), `model`, `app_version` | Right after connecting. `sim_country` and `sim_sub_id` match `incoming`. `caller_id` can be empty when Android has not reported the number yet. |
| `ringing` | `serial`, `caller_id` | Right after `hello`, and again if the caller's number becomes known later. |
| `call_end` | `serial`, `caller_id` | The call ended. The app closes the connection shortly afterwards. |
| `add_call_result` | `id`, `status`, `reason` | Progress of `add_call`: `status` is `dialing`, `active`, `merged`, or `failed` (`reason`: `bad_number`, `no_incall_service`, `busy`, `no_active_call`, `cannot_add_call`, `cannot_hold`, `no_account`, `no_telecom`, `ai_call_disabled`, `missing_id_or_number`). |
| `local_menu_started`, `local_menu_done`, `local_menu_failed` | | Only in builds with `LOCAL_MENU_ENABLED = true`; never sent by default. |

Agent to app:

| `type` | Fields | Effect |
|---|---|---|
| `ready`, `hello_ack`, `hello_ok` | | Marks the agent as ready; no other effect in the current code. |
| `tts_start` | `source` | Audio is about to follow. For most sources the app first drops audio it has queued but not yet played; sources starting with `gemini_live` keep the queue. `gemini_wait` belongs to an older flow in which the phone played a bundled "please wait" clip; no clip is shipped, so it has no effect. |
| `tts_end` | `source` | End of that audio. |
| `dtmf` | `digit`, `confidence` | Drops queued audio. |
| `stop_local_menu` | `reason` | Only relevant when `LOCAL_MENU_ENABLED = true`. |
| `hangup` | `reason` | End the call. |
| `place_call` | as on the control connection | As on the control connection. |
| `add_call` | `id`, `number`, `merge` (optional, default `true`), `answer_timeout_ms` (optional, 5000–60000, default 25000) | Dials a second call (e.g. the owner for an urgent call) and merges it into a 3-way conference once answered. |
| `add_call_abort` | `id` | Cancels an in-progress `add_call`. |
| `reply_text`, `tts_text` | `text` | Logged only (the length, not the text). |

**Audio frames.** Both directions use raw PCM, signed 16-bit little-endian, mono,
16 kHz, in binary frames.

- App to agent: the caller's voice, 60 ms per frame (1920 bytes), sent continuously
  during the call, including silence.
- Agent to app: played into the call in the order received, through a soft limiter at
  about -2 dBFS. Between the agent's audio the app writes silence into the call (40 ms
  at a time once nothing arrived for 100 ms), so the phone's audio path stays awake and
  the first word after a pause is not cut.

The app pings the media connection and treats it as lost after 10 seconds without an
answer. There is no reconnect within a call: if the connection closes or fails and is
not open again 3 seconds later, the app ends the call.

## Optional SMS hook

`SmsRilHook.kt` is an Xposed / LSPosed hook that stays **inactive unless an
LSPosed-style framework is installed** and this app is enabled as a module for
`com.android.phone`. When active, it writes each incoming SMS (sender, text,
timestamp) as a JSON file into the `pixel-sms-push` folder of the phone process's
device-protected cache directory. Nothing on the phone forwards those files; the
optional [SMS forwarder](../tools/sms_forward/README.md) on the PC picks them up and
sends them to your notification channels, such as Telegram.

`service.sh` enables the hook automatically when the `zygisk_vector` module is
present ([step 7](#at-every-boot-servicesh)). If you do not want it, do not install an
LSPosed framework, or disable this app in the framework's module list.

`xposed-stub/` contains empty compile-time stand-ins for the Xposed API, so the hook
compiles without bundling the real library.

## Troubleshooting

Start with the log:

```text
adb logcat -s CallAudioService BridgeClient AgentControlClient TelephonyUplink VoiceCallCapture LocalAudioGuard SilentInCall SilentCallScreen DialerRoleHelper
```

| Symptom | What to check |
|---|---|
| Checklist says *Installed as a normal app*, or *Privileged permissions* show Missing | The module is not active, or a normal install shadows it; privileged permissions are only granted at boot to an app under `/system/priv-app`. Enable the module, uninstall any normal install, reboot. `adb shell pm path com.pixelcallai.streamer` should print a path under `/system/priv-app/`. |
| **Test connection**: "connection refused" (USB) | The agent is not running, or `adb reverse` is not set up. `adb reverse --list` on the PC should list `tcp:8765`. Unplugging the cable removes the mapping. |
| **Test connection** fails on Wi-Fi | Wrong address, the PC firewall blocks the port, or phone and PC are on different networks. |
| **Test connection** reports an invalid status code | Something answered, but not as a WebSocket at that path. Check the path and token in the address. |
| The phone still rings out loud | Do Not Disturb access is missing, or the app is not the default phone app and call screening app. |
| Google Dialer's call screen appears | The phone is in normal phone mode, or the app is not the default phone app, so Google Dialer was not disabled. Turn **AI answers my calls** on, or fix the role in **Setup steps**, and reboot. |
| Call is answered but the caller hears nothing | Look for `TelephonyUplink`. `TYPE_TELEPHONY unavailable` or `setPreferredDevice(TYPE_TELEPHONY) failed` means the phone does not offer the telephony output to the app (unsupported device, or the privileged permission is missing). The app also posts a notification. |
| The agent receives only silence | Look for `VoiceCallCapture`. `maxRms` stays near zero if the device does not deliver call audio to the `VOICE_DOWNLINK` / `VOICE_CALL` sources. On Pixels the level is low by design (earpiece at minimum) and the agent amplifies it. |
| The phone does not answer at all | Normal phone mode (**AI answers my calls** off), the service is stopped, `ANSWER_PHONE_CALLS` is missing, or the agent was unreachable: without a control connection within 12 seconds the app does not answer (log: `agent not ready`) and the carrier treats the call as missed or sends it to voicemail. |
| A call ends by itself after a few seconds of silence | The media connection dropped (log: `media connection lost`) and was not back within 3 seconds. |
| A text from the agent is not sent | Look for `SmsSender`. `no_permission`: the module did not grant `SEND_SMS` (reboot, or switch to AI mode once). `no_service` / `radio_off` / `generic_failure`: no signal, airplane mode, or the carrier refused it (for example, the SIM has no balance or the line is suspended). |
| The agent's call-back or errand never dials | Look for `OutboundCallHelper` and the `place_call_failed` reason: `busy` (the phone was ringing or on a call; the agent retries later), `ai_call_disabled` (normal phone mode), `dial_helper_failed` (Android refused the call). |
| An errand call is hung up the moment it connects | It went out on another SIM than the agent asked for (`wrong_sim`), usually after a SIM swap. Call the phone once so the agent learns its SIMs again. |
| The module made the phone fail to boot | Use the root manager's safe mode, or create `/data/adb/modules/pixelcallai_uplink/disable` from recovery, then reboot. |

## Legal

Recording calls, and letting an AI speak to callers without telling them, is regulated
differently from place to place. Find out what applies to you before using this; you
are responsible for how you use it.

`magisk/pixelcallai_uplink/META-INF/com/google/android/update-binary` carries its own
copyright and license header (GPL-3.0-only).

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
