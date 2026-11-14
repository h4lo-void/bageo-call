# Bageo: Android app and root module

This folder contains the phone side of Bageo:

- **`app/`**: the Android app `com.pixelcallai.streamer`. It answers incoming calls
  silently, streams the caller's voice to the agent program on your PC over a
  WebSocket, and plays the AI's voice back into the call so the caller hears it.
- **`magisk/pixelcallai_uplink/`**: a Magisk / KernelSU module that installs the app
  as a *privileged system app*. Android only lets privileged apps capture call audio
  and write audio into the call, so the app cannot do its job as a normal install.

The phone does no AI work itself. It is an audio bridge between the phone network and
the agent.

The package name (`com.pixelcallai.streamer`) and the module id (`pixelcallai_uplink`)
keep the project's earlier working name, so phones that already run the module can be
updated in place.

> **Use a dedicated phone.** The module takes over the phone app, disables Google
> Dialer, turns on Do Not Disturb and sets the ringer volume to zero. The phone stops
> being usable as a normal handset until you switch it to a
> [normal phone](#normal-phone-mode) or roll back (see [Rollback](#rollback)).

## Requirements

- A phone with an **unlocked bootloader and root** through Magisk or KernelSU.
- A SIM that can receive calls.
- Android 10 or newer (`minSdk 29`).
- **Tested phones: Pixel 7, 7a, 8 and 9.** Comments in the source refer to Android 16
  on those phones.
- **Other phones are untested.** Whether they work depends on the device's telephony
  audio HAL: it must allow a privileged app to record the call
  (`VOICE_DOWNLINK` / `VOICE_CALL` audio source) and to play into the call through an
  output device of type `TYPE_TELEPHONY`. A phone that lacks either one will not
  work. Try at your own risk.
- A PC running the Bageo agent, connected by USB (recommended) or on the same
  network.

## Build

You need **JDK 17** and the **Android SDK** (platform `android-35`; the Android Gradle
Plugin downloads matching build tools if they are missing and the SDK licenses are
accepted). Tell Gradle where the SDK is with the `ANDROID_HOME` environment variable
or a `local.properties` file in this folder:

```properties
sdk.dir=/path/to/android-sdk
```

Build the APK and package the module zip in one step:

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
magisk/dist/pixelcallai_uplink-<versionName>.zip
```

To build only the APK: `.\gradlew.bat :app:assembleDebug` (Windows) or
`sh ./gradlew :app:assembleDebug` (macOS / Linux). The APK is written to
`app/build/outputs/apk/debug/app-debug.apk`.

Notes:

- The debug build is minified on purpose so the app stays in a single dex file; as a
  privileged app on Android 16 a second dex file caused `ClassNotFoundException` at
  start-up.
- The APK is signed with the debug key of the machine that builds it, unless
  `PCA_KEYSTORE_FILE`, `PCA_KEYSTORE_PASSWORD`, `PCA_KEY_ALIAS` and `PCA_KEY_PASSWORD`
  point to a key of your own. Android ties an installed app to its signing key: when
  you move between builds signed by different keys (another machine, a release vs. your
  own build), remove the module and reboot before installing the other one — not doing
  so has not been tested.
- Releases on GitHub are signed with the project's own key, kept as repository secrets
  (`ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS`,
  `ANDROID_KEY_PASSWORD`), so one release installs over the previous one. Moving the
  phones in daily use from a debug-key build to the release key by installing the new
  module over the old one worked: Android logged *signature changed; retaining data*
  and kept the app's data and permissions.

## Install

1. If you ever installed the APK as a normal app (`adb install`), uninstall it first.
   Never have both a normal install and the module.
2. Copy `pixelcallai_uplink-<version>.zip` to the phone.
3. In the Magisk or KernelSU app, install the zip as a module.
4. Reboot.
5. Open **Bageo**. The card at the top says in one sentence what the phone does right
   now. Work through **Setup steps** until every step shows OK:
   - *Install the Bageo module*: confirms the module is active (privileged app and
     privileged permissions).
   - *Allow Bageo to use the phone and microphone*: tap **Allow**.
   - *Make Bageo the default phone app*, *Make Bageo the call screening app*: tap
     **Set**.
   - *Let Bageo control Do Not Disturb*, *Do not let Android put Bageo to sleep*: tap
     the button and allow it.

   The individual permissions and the call-audio check (*AI voice injection into
   the call*) are under **Advanced → Detailed checks**.
6. In **Advanced → Connection**, keep the USB default or enter the Wi-Fi address
   (see below), then tap **Test connection** while the agent is running on the PC.
   A failure is shown as a plain reason (refused, timed out, address not found…);
   the raw error is under **Details**.
7. Leave **AI answers my calls** on. The app shows a permanent low-priority
   notification while the service runs, and starts again by itself after a reboot.
   Once the agent has sent the console's address, **Open the web console** opens it.

When the root manager asks whether the app may use root, allow it. The app uses `su`
for the role and mute commands listed below.

### Connection: USB or Wi-Fi

- **USB (default, recommended).** The app connects to `ws://127.0.0.1:8765`. On the
  PC, `adb reverse tcp:8765 tcp:<agent port>` maps that port to the agent through the
  USB cable. The agent on the PC sets this up by itself and re-checks it every few
  seconds. USB keeps working when Wi-Fi is off.
- **Wi-Fi.** Enter the address the PC program shows, for example
  `ws://192.0.2.10:8787/ws?token=...`. The address must start with `ws://` or `wss://`.
  The phone and the PC must be on the same network. Anything after `?` is left out of
  the app's log output.

The address is stored on the phone (device-protected storage, so it is available
before the first unlock after a reboot). **Reset to USB default** removes the stored
value. After you save a new address the app restarts its service so the new address is
used; if a call is in progress it asks you to restart the service afterwards.

## What changes on the phone

### The module, at every boot (`service.sh`)

The script waits for the system to finish booting (up to about 3 minutes), then
10 more seconds, then until the package manager knows the app (it gives up after
about a minute and does nothing). All commands are best effort and none of them
stop the boot.

It reads the phone mode from `/data/adb/pixelcallai/mode` first (see
[Normal phone mode](#normal-phone-mode)). When the file says `normal` it only does
steps 1, 2, 6 and 7. With no file, or anything else in it, it does all the steps
below, exactly as before the mode switch existed.

1. Creates `/data/local/tmp/pixelcallai_uplink.pcm` (mode 666). This is a debugging
   aid; the normal call path does not use it.
2. Grants the app `RECORD_AUDIO`, `READ_PHONE_STATE`, `ANSWER_PHONE_CALLS`, `SEND_SMS`,
   `READ_PHONE_NUMBERS` and `ACCESS_NOTIFICATION_POLICY`, and allows the
   `ACCESS_NOTIFICATION_POLICY` app-op.
3. Gives the app the **Dialer** and **Call screening** roles and makes it the default
   dialer (`telecom set-default-dialer`).
4. Only if the app really is the default dialer: disables Google Dialer's in-call
   components (`InCallServiceImpl`, `InCallActivity`) and then the whole
   `com.google.android.dialer` package for user 0 (`pm disable-user`).
5. Turns on Do Not Disturb in its strictest mode (`zen_mode 2`) and sets the ring and
   notification volume settings to 0.
6. Starts the app's foreground service.
7. Only if the LSPosed-compatible module `zygisk_vector` is installed: enables this
   app as an LSPosed module scoped to `com.android.phone` (see
   [Optional SMS hook](#optional-sms-hook)).

Before that, `post-fs-data.sh` sets file modes and SELinux labels on the APK and the
permission file, and overlays them onto `/system/priv-app` and
`/system/etc/permissions` (work directory `/dev/pca_ov`).

The module also ships
`system/etc/permissions/privapp-permissions-com.pixelcallai.streamer.xml`, the
allow-list that lets Android grant the privileged permissions.

### The app, while its service runs

In AI mode:

- Repeats the role commands from step 3 and the Google Dialer disabling from step 4
  through `su` each time the service starts. Unlike `service.sh`, the app does this
  without first checking that it is the default dialer.
- Mutes the microphone, turns the speakerphone off, and holds the in-call earpiece
  volume at its minimum. During a call it repeats the microphone mute every 500 ms.
- The first time it arms: sets media, ring, notification, system, alarm and DTMF
  volume to 0, switches the ringer to silent and turns on Do Not Disturb.
- With root, also runs `cmd media.audio_flinger mute mic 1`, the volume settings above
  and several `tinymix` microphone mute controls. Controls that do not exist on a
  device are ignored.
- **Stop service** restores the microphone, speakerphone, audio mode, ringer mode,
  Do Not Disturb filter and volumes it saved when it armed, except the earpiece
  volume, which stays at minimum.

In normal phone mode the service only keeps the control connection to the agent
(status, the remote mode switch, and `send_sms`, which still sends in this mode).
It does not repeat the role commands, mute anything, silence or answer calls.

### Status bar

With the app as the default dialer and Google Dialer disabled, no full-screen call
screen appears. Android may still show its own small "ongoing call" indicator in the
status bar; that belongs to the system and the app does not hide it.

## Normal phone mode

The app's main switch, **AI answers my calls**, moves the phone between two modes;
the web console does the same remotely (`set_mode`, see below). A switch is refused
while a call rings or is active. The module carries it out as root, so the app needs
no root permission for it. It can simply be repeated: every step runs again and is
checked, and the first one that does not take effect is reported as a code
(`dialer_failed`, `screening_failed`, `dnd_failed`, `volume_failed`, `save_failed`,
`no_root`, `in_call`, `busy`). `no_root` means the module's helper did not take the
request: the module is not running (enable **Bageo Uplink**, shown as **Myna Uplink** when installed before the
rename, in Magisk or KernelSU and
reboot).

| | AI mode (`ai`, default) | Normal phone (`normal`) |
|---|---|---|
| Default phone app | Bageo; Google Dialer disabled | Google Dialer, enabled again (on phones without it, Android's own) |
| Call screening role | Bageo (silences the ringer) | removed from Bageo |
| Do Not Disturb | on | off |
| Ringer and volumes | silent and zero; earpiece at minimum | ringer normal; ring, notification, earpiece, media, alarm and system volume restored |
| Microphone | muted during calls | left alone |
| Incoming calls | answered by the AI | ring; you answer |
| App service | full call handling | control connection only |

The system settings are changed by the module's root script,
`magisk/pixelcallai_uplink/apply_mode.sh`. The app cannot count on `su` (KernelSU
only offers it to apps on its allow list), so at every boot `service.sh` starts
`mode_daemon.sh`, a small root helper in the same folder, in its own session. The
app writes a request to `files/mode/request` in its device-protected storage
(`/data/user_de/0/com.pixelcallai.streamer/`), one line:
`<ai|normal> <id> [six volumes]`. The helper looks every 2 seconds, takes it, checks
it (a malformed request is not run and is reported as `error=invalid_request`), runs
`sh apply_mode.sh` with it and writes its answer to `files/mode/state`. A request
nobody takes within 10 seconds is withdrawn and reported as `no_root`. The script
runs outside the app, so it runs to the end even when Android stops the app: removing
an app from the phone-app role can do that. For the same reason the normal-phone
order is call screening, Do Not Disturb, ringer and volumes, microphone, and **the
phone app last**. That last step
only counts as done when `telecom get-default-dialer` reports Google Dialer, Google
Dialer is not in `pm list packages -d` and both in-call components were enabled
(or, without Google Dialer, when the default phone app is no longer Bageo). If
`pm list packages` itself fails, the step fails (`dialer_failed`); it never
assumes there is no Google Dialer. Every root command runs under `toybox timeout`.

Where it is stored:

- `/data/adb/pixelcallai/mode`: `ai` or `normal`, written by the script when the
  app switches. It is outside the module folder, so module updates keep it.
  `service.sh` reads it at every boot; **no file means `ai`**, so a phone that was
  never switched behaves exactly as before. Before comparing, `service.sh`, the
  script and the app all drop blanks (space, tab, CR, LF, VT, FF) and a UTF-8 BOM.
- `/data/adb/pixelcallai/pending`: exists from the start of a switch until every
  step worked.
- `/data/adb/pixelcallai/last_apply`: the last run, `key=value` lines (`id`,
  `mode`, `step_<name>=ok|fail`, then `error=<code>` or `result=ok`, and `done=1`).
- `/data/adb/pixelcallai/apply.lock`: held while a run is going (its pid inside).
- `/data/adb/pixelcallai/daemon.pid`: the helper's pid; `service.sh` removes it at
  boot.
- `files/mode/state` in the app's device-protected storage: the helper's answer,
  owned by the app and readable only by it. `key=value` lines: `daemon=1`, `mode`,
  `pending`, `running`, `updated` and `uptime` (the app ignores a state written
  before the last reboot), `request` and `exit` (the last request and the script's
  exit code once it ended), `error=invalid_request`, and every `last_apply` line as
  `log.<key>`. Written when the helper starts, before and after every run, and
  whenever the file is missing.
- `files/mode/normal_audio` in the same folder: the volumes saved when the phone
  last went from normal to AI (`ring=…`, `notification=…`, `voice_call=…`,
  `music=…`, `alarm=…`, `system=…`). Switching to normal restores them; a missing
  or zero value becomes half of that volume's maximum.
- The app keeps a copy of the mode in its device-protected storage
  (`files/phone_mode`), cached in memory per process; the `:screening` process
  reads it again for every incoming call.

When the service starts it waits for a state the helper wrote since this boot (at
boot the helper starts after the app), then asks for the switch again if `pending`
exists, if the mode is `normal` but Bageo is still the default phone app (an
interrupted switch, or an older version that took the phone over), or if its own
copy differs from the file. If the
agent asked for the switch and the app was stopped before it could answer, it sends
that `set_mode_result` after its next `hello`; the agent keeps waiting through the
reconnect (up to 15 seconds), and the web console calls a timeout "not confirmed yet".

In normal mode the service runs as a foreground service of type `phoneCall` only
(not `microphone`); if Android refuses the foreground start, the app logs it and
still opens the control connection.

Upgrading from a version before 0.2.2 whose **AI answering** switch was off: that old
setting (`AI_CALL_ENABLED`) no longer counts. The phone answers with the AI (no mode
file means `ai`), the app logs it and its status card shows a hint until the new
switch is used once.

The older `com.pixelcallai.streamer.SET_ENABLED` service intent performs the same
switch, without a reboot:

```text
adb shell am start-foreground-service -n com.pixelcallai.streamer/.CallAudioService -a com.pixelcallai.streamer.SET_ENABLED --ez enabled false
```

(`--ez enabled true` = AI mode.) Or write the file and reboot:
`adb shell "su -c 'mkdir -p /data/adb/pixelcallai && echo normal > /data/adb/pixelcallai/mode'"`
(`echo ai`, or `rm -f` the file, for AI mode).

## Rollback

**Before installing an older version** (anything before 0.2.2), switch back to AI
mode, or run `adb shell "su -c 'rm -rf /data/adb/pixelcallai'"`. Older versions do
not read the mode file: their `service.sh` takes the phone over at every boot even
when the file says `normal`. When this version is installed again later, it finds
`normal` and turns the phone into a normal phone by itself, so the AI stops
answering without anyone asking for it.

0. Switch the phone to a [normal phone](#normal-phone-mode) first. That re-enables
   Google Dialer, turns Do Not Disturb off and restores the volumes, so steps 2 and
   3 below are then already done.
1. In the Magisk or KernelSU app, disable or remove the **Bageo Uplink**
   module, then reboot. If the phone does not boot far enough to do that, use the root
   manager's safe mode, or from `adb shell` / recovery with root:

   ```text
   touch /data/adb/modules/pixelcallai_uplink/disable
   reboot
   ```

   `/data/adb/pixelcallai/` (the mode file) stays; delete it too for a clean
   removal.

2. Google Dialer and Do Not Disturb are **not** restored by removing the module,
   because those settings are stored by Android. Re-enable the dialer:

   ```text
   adb shell su -c "pm enable com.google.android.dialer"
   adb shell su -c "pm enable com.google.android.dialer/com.android.incallui.InCallServiceImpl"
   adb shell su -c "pm enable com.google.android.dialer/com.android.incallui.InCallActivity"
   adb shell su -c "pm enable com.google.android.dialer/com.android.incallui.LegacyInCallActivity"
   adb shell su -c "cmd role add-role-holder android.app.role.DIALER com.google.android.dialer"
   adb shell su -c "telecom set-default-dialer com.google.android.dialer"
   ```

3. Turn Do Not Disturb off and raise the ring, notification and call volume in the
   system settings.

Do not disable `com.android.phone`; that is the telephony stack itself.

## Permissions

Privileged permissions (only granted because the module installs the app under
`/system/priv-app` and ships the allow-list file):

| Permission | Why |
|---|---|
| `CAPTURE_AUDIO_OUTPUT` | Record the call audio (`VOICE_DOWNLINK` / `VOICE_CALL` sources). |
| `MODIFY_PHONE_STATE` | Play the AI's audio into the call through the `TYPE_TELEPHONY` output. |
| `CONTROL_INCALL_EXPERIENCE` | Control the in-call experience (the app's invisible in-call service). |
| `WRITE_SECURE_SETTINGS` | Declared and allow-listed; the current code changes settings through `su` instead. |
| `INTERACT_ACROSS_USERS`, `MANAGE_USERS` | Present in the allow-list file only; the app does not declare them. |

Permissions you grant on the setup screen:

| Permission | Why |
|---|---|
| `RECORD_AUDIO` | Required by Android for any audio recording, including call audio. |
| `READ_PHONE_STATE` | Know when a call rings, connects and ends. |
| `READ_CALL_LOG` | Read the caller's number when it did not arrive any other way. |
| `ANSWER_PHONE_CALLS` | Answer and hang up automatically. |
| `CALL_PHONE` | Place a call when the agent asks for a call-back. |
| `POST_NOTIFICATIONS` (Android 13+) | Show the service notification. |

Other declared permissions:

| Permission | Why |
|---|---|
| `INTERNET` | WebSocket connection to the agent. |
| `FOREGROUND_SERVICE`, `FOREGROUND_SERVICE_MICROPHONE`, `FOREGROUND_SERVICE_PHONE_CALL` | Keep the call service alive. |
| `MODIFY_AUDIO_SETTINGS` | Mute the microphone, set volumes. |
| `ACCESS_NOTIFICATION_POLICY` | Turn on Do Not Disturb. |
| `RECEIVE_BOOT_COMPLETED` | Start again after a reboot. |
| `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` | Ask for the battery optimization exemption. |
| `MANAGE_OWN_CALLS` | Declared; not used by the current code. |
| `SEND_SMS` | Send a text message when the agent asks (`send_sms`, for example to reply to an appointment request). The module grants it at boot and when the phone switches to AI mode; the setup screen does not ask for it. |
| `READ_PHONE_NUMBERS` | Read each SIM's own number when the agent's `send_sms` names its SIM by number (`sim`). Granted by the module like `SEND_SMS`; without it only carrier and display names are compared. |

The manifest also sets `usesCleartextTraffic="true"`, because `ws://` to localhost or
to a PC on your own network is not encrypted.

## How a call flows

1. **Idle.** The service keeps a *control* WebSocket open to the agent and sends a
   `ping` every 15 seconds.
2. **Ringing.** The app does not answer yet. It waits up to 12 seconds for the control
   connection, tells the agent about the call (`incoming`), and rings for as long as
   the agent's `answer_plan` says (3 seconds if the agent does not reply within
   500 ms, never more than 8 seconds). If the control connection is not up within
   12 seconds, the app does not answer: the call keeps ringing until the carrier
   ends it (missed call or voicemail). An `answer_plan` with `reject` declines the
   call without answering it.
3. **Answer.** The app answers without any sound or screen on the phone.
4. **In call.** It opens a second *media* WebSocket, sends `hello`, streams the
   caller's audio to the agent and plays the audio it receives into the call.
   If the media connection drops and is not open again within 3 seconds, the app
   ends the call, so the caller is not left in silence.
5. **End.** Either side can end the call: the agent sends `hangup`, or the caller
   hangs up and the app sends `call_end`.

A call you dial by hand on the phone is left alone: the AI is not attached, and the
microphone and earpiece work normally for that call. A second call that rings during
a call is declined at once (`call_waiting_rejected` tells the agent); the call in
progress is not touched.

More detail is in [ARCHITECTURE.md](ARCHITECTURE.md).

## WebSocket protocol

Derived from the app source (`AgentControlClient.kt`, `BridgeClient.kt`,
`CallAudioService.kt`). The app is the WebSocket client for both connections, and both
use the same address. Text frames are JSON objects with a `type` field. Fields the app
does not know are ignored.

With the default address `ws://127.0.0.1:8765` the handshake requests path `/`; the
app requests exactly the path and query you configure.

`serial` is `Build.getSerial()`; if Android refuses it, the app sends
`pixel-<app uid>` instead.

### Control connection (open while the service runs)

App to agent:

| `type` | Fields | When |
|---|---|---|
| `hello` | `serial`, `mode` = `"CONTROL_IDLE"`, `play_tts_locally` = `false`, `model`, `app_version`, `phone_mode` (`"ai"` / `"normal"`), `in_call`, `supports_sms` = `true`, `supports_reject` = `true`, `supports_sim_select` = `true` | Right after connecting. `phone_mode` is not called `mode` because `mode` names the connection. Apps before the mode switch send neither. Apps before 0.2.4 do not send `supports_sms` and cannot send text messages. `supports_reject`: the app declines a ringing call when `answer_plan` says `reject`; apps without it would answer the call instead. `supports_sim_select`: `send_sms` may name its SIM with `sim`; apps without it are not sent such texts. |
| `ping` | `t` (milliseconds since epoch) | Every 15 s. |
| `status` | `mode` (`"ai"` / `"normal"`), `in_call` | The phone mode or the call state (ringing or active vs idle) changed. |
| `set_mode_result` | `id`, `ok`, `mode`, `error` | Answer to `set_mode`. `mode` is the mode afterwards; `error` is empty on success, otherwise a code from [Normal phone mode](#normal-phone-mode). |
| `incoming` | `serial`, `caller_id`, `sim_country`, `sample_rate`, `sim_sub_id` (only when known) | A call is ringing. `caller_id` is empty for a hidden number. `sim_country` (ISO code, lowercase, empty when unknown) and `sim_sub_id` (Android subscription id) name the SIM the call came in on. |
| `incoming_cancel` | `serial` | The call ended before it was answered. |
| `call_waiting_rejected` | `serial`, `caller_id`, `sim_country`, `sim_sub_id` (only when known) | A second call rang during a call and was declined (AI mode). The fields describe the declined call. |
| `place_call_ack` | `call_id`, `task_id`, `number`, `reason` = `""` | A requested call was dialed. |
| `place_call_failed` | `call_id`, `task_id`, `number`, `reason` | A requested call could not be dialed. `reason` `busy`: a call was ringing or up on the phone. |
| `outbound_ready` | `call_id`, `task_id`, `number` | A requested call connected. |
| `call_end` | `call_id`, `task_id`, `serial`, `reason` = `"normal"` | A requested call ended. |
| `send_sms_result` | `id`, `ok`, `error` (only when `ok` is `false`) | Answer to `send_sms`, sent once every part of the message was reported sent, one part failed, or 60 seconds passed. `error` is one of `no_permission`, `bad_request`, `no_service`, `radio_off`, `generic_failure`, `timeout`, `exception`, `no_sim` (no SIM, or more than one, matched `sim`; nothing was sent). If the parts are reported after a `timeout` answer, a second answer with the real result follows. The answers for the last 20 ids are sent again after the control connection reconnects; the agent drops repeats by `id`. |
| `send_sms_delivered` | `id`, `ok` | The network's delivery report: `true` once every part reached the recipient, `false` when a part failed for good. Not sent when the network gives no report (the app stops waiting after an hour). Kept and sent again like `send_sms_result`. |

Agent to app:

| `type` | Fields | Effect |
|---|---|---|
| `answer_plan` | `delay_ms`, `prewarm`, `reject` (optional) | How long to let the call ring before answering. `prewarm` is only logged. `reject` = `true`: decline the ringing call without answering it (`incoming_cancel` follows when it has ended); also honored when it arrives after the 500 ms wait, as long as the call still rings. |
| `place_call` | `number`, `call_id`, `task_id`, `sub_id` (optional) | Dial `number`, from the SIM with subscription id `sub_id` when given (Android's default when that SIM is not found). While a call rings or is up, the app does not dial and answers `place_call_failed` with `reason` `busy`. A call that has not started dialing 30 seconds later is forgotten, so a later call is not taken for it. |
| `set_mode` | `mode` (`"ai"` / `"normal"`), `id` | Switch the phone mode; answered with `set_mode_result`. |
| `send_sms` | `id` (1–64 characters), `to` (3–20 digits, optional leading `+`), `text` (1–500 characters), `sub_id` (optional), `sim` (optional) | Send `text` as a text message from the phone's own number: the SIM with subscription id `sub_id`, else the default SMS SIM. For example to reply to an appointment request. Answered with `send_sms_result` and, when the network reports it, `send_sms_delivered`. A `send_sms` with the id of a message still being sent is ignored; one with the id of a message already sent is answered again without sending it twice. `sim` (a carrier name such as `Tello`, or the SIM's own number; never sent together with `sub_id`) picks the active SIM whose carrier or display name equals or contains it, any case, or, when it has at least 7 digits, whose own number ends in the same digits. Exactly one SIM must match; otherwise nothing is sent and the answer is `no_sim`. |
| `agent_info` | `console_url` | The web console's address, for the app's **Open the web console** button. Sent after `hello` only when the agent knows it; the app forgets the old one on every new connection. |
| `blocklist_sync` | `numbers` (array) | Stored, but not used: the app does not reject callers by this list (the agent decides, with `answer_plan` `reject`). |
| `hello_ack`, `hello_ok`, `ack`, `pong` | | Logged only. |

If the control connection drops, the app reconnects after 1.5 seconds.

### Media connection (one per answered call)

App to agent:

| `type` | Fields | When |
|---|---|---|
| `hello` | `serial`, `caller_id`, `sample_rate` (16000), `mode` = `"SILENT_LOCAL"`, `sim_country`, `sim_sub_id` (only when known), `play_tts_locally` = `false`, `local_menu` (`false` in this build), `model`, `app_version` | Right after connecting. `sim_country` and `sim_sub_id` are the same as in `incoming`. |
| `ringing` | `serial`, `caller_id` | Right after `hello`, and again if the caller's number only becomes known later. |
| `call_end` | `serial`, `caller_id` | The call ended. The app closes the connection shortly afterwards. |
| `local_menu_started`, `local_menu_done`, `local_menu_failed` | | Only in builds with `LOCAL_MENU_ENABLED = true`; never sent by default. |

`caller_id` can be an empty string in `hello` when Android has not reported the number
yet.

Binary frames, app to agent: the caller's voice as raw PCM, signed 16-bit
little-endian, mono, 16 kHz, 60 ms per frame (1920 bytes). They are sent continuously
during the call, including silence.

The app pings the media connection and treats it as lost after 10 seconds without an
answer. When it closes or fails during a call and is not open again 3 seconds later,
the app ends the call (there is no reconnect within a call).

Agent to app:

| `type` | Fields | Effect |
|---|---|---|
| `ready`, `hello_ack`, `hello_ok` | | Marks the agent as ready. No other effect in the current code. |
| `tts_start` | `source` | Audio is about to follow. For most sources the app first drops audio it has queued but not yet played. Sources starting with `gemini_live` keep the queue. `gemini_wait` belongs to an older flow in which the phone played a bundled "please wait" clip; no clip is shipped, so it has no effect. |
| `tts_end` | `source` | End of that audio. |
| `dtmf` | `digit`, `confidence` | Drops queued audio. |
| `stop_local_menu` | `reason` | Only relevant when `LOCAL_MENU_ENABLED = true`. |
| `hangup` | `reason` | End the call. |
| `place_call` | as above | Same as on the control connection. |
| `reply_text`, `tts_text` | `text` | Logged only (the length, not the text). |

Binary frames, agent to app: raw PCM in the same format (signed 16-bit little-endian,
mono, 16 kHz), played into the call in the order received. The app applies a soft
limiter at about -2 dBFS. Between the agent's audio the app writes silence into the
call (40 ms at a time once nothing arrived for 100 ms) so the phone's audio path stays
awake and the first word after a pause is not cut.

Not verified from this folder alone: how the agent reacts to each message. That is
defined by the agent's source.

## Optional SMS hook

`SmsRilHook.kt` is an Xposed / LSPosed hook that is **inactive unless an LSPosed-style
framework is installed** and this app is enabled as a module for `com.android.phone`.
When active, it writes each incoming SMS (sender, text, timestamp) as a JSON file into
the `pixel-sms-push` folder of the phone process's device-protected cache directory,
where a separate tool can pick it up: the optional
[SMS forwarder](../tools/sms_forward/README.md) on the PC. Nothing on the phone forwards
those files anywhere. `service.sh` enables the hook automatically when the `zygisk_vector` module
is present (step 7 above). If you do not want it, do not install an LSPosed framework,
or disable this app in the framework's module list.

`xposed-stub/` contains empty compile-time stand-ins for the Xposed API so the hook
compiles without bundling the real library.

## Troubleshooting

Useful log command:

```text
adb logcat -s CallAudioService BridgeClient AgentControlClient TelephonyUplink VoiceCallCapture LocalAudioGuard SilentInCall SilentCallScreen DialerRoleHelper
```

| Symptom | What to check |
|---|---|
| Checklist says *Installed as a normal app* | The module is not active, or a normal install is shadowing it. Check that the module is enabled, uninstall any normal install, reboot. `adb shell pm path com.pixelcallai.streamer` should print a path under `/system/priv-app/`. |
| *Privileged permissions* show Missing | Same cause as above. These permissions are only granted to an app under `/system/priv-app` at boot. |
| **Test connection** fails with "connection refused" (USB) | The agent is not running, or `adb reverse` is not set up. Run `adb reverse --list` on the PC; it should list `tcp:8765`. Unplugging the cable removes the mapping. |
| **Test connection** fails on Wi-Fi | Wrong address, the PC firewall blocks the port, or the phone and PC are on different networks. |
| **Test connection** reports an invalid status code | Something answered, but not as a WebSocket at that path. Check the path and token in the address. |
| The phone still rings out loud | Do Not Disturb access is missing, or the app is not the default phone app and call screening app. |
| A call screen from Google Dialer appears | The phone is in normal phone mode, or the app is not the default phone app, so Google Dialer was not disabled. Switch **AI answers my calls** on, or fix the role in **Setup steps**, and reboot. |
| The call is answered but the caller hears nothing | Look for `TelephonyUplink` in the log. `TYPE_TELEPHONY unavailable` or `setPreferredDevice(TYPE_TELEPHONY) failed` means the phone does not offer the telephony output to the app (unsupported device, or the privileged permission is missing). The app also posts a notification in this case. |
| The agent receives only silence | Look for `VoiceCallCapture` in the log. The `maxRms` value stays near zero if the device does not deliver call audio to the `VOICE_DOWNLINK` / `VOICE_CALL` sources. On Pixels the level is low by design (the earpiece volume is at minimum) and the agent is expected to amplify it. |
| The phone does not answer at all | The phone is in normal phone mode (**AI answers my calls** is off), the service is stopped, the app lacks `ANSWER_PHONE_CALLS`, or the agent was not reachable: without a control connection within 12 seconds the app does not answer (log: `agent not ready`) and the carrier takes the call as missed or to voicemail. |
| A call ends by itself after a few seconds of silence | The media connection to the agent dropped (log: `media connection lost`). The app ends the call when it is not back within 3 seconds. |
| The module made the phone fail to boot | Use the root manager's safe mode, or create `/data/adb/modules/pixelcallai_uplink/disable` from recovery, then reboot. |

## Legal

Recording calls, and having an AI speak to callers without telling them, is regulated
differently from place to place. Find out what applies to you before using this.
You are responsible for how you use it.

`magisk/pixelcallai_uplink/META-INF/com/google/android/update-binary` carries its own
copyright and license header (GPL-3.0-only).
