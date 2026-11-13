# Myna: Android app and root module

This folder contains the phone side of Myna:

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
- A PC running the Myna agent, connected by USB (recommended) or on the same
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
5. Open **Myna**. The card at the top says in one sentence what the phone does right
   now. Work through **Setup steps** until every step shows OK:
   - *Install the Myna module*: confirms the module is active (privileged app and
     privileged permissions).
   - *Allow Myna to use the phone and microphone*: tap **Allow**.
   - *Make Myna the default phone app*, *Make Myna the call screening app*: tap
     **Set**.
   - *Let Myna control Do Not Disturb*, *Do not let Android put Myna to sleep*: tap
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
2. Grants the app `RECORD_AUDIO`, `READ_PHONE_STATE`, `ANSWER_PHONE_CALLS` and
   `ACCESS_NOTIFICATION_POLICY`, and allows the `ACCESS_NOTIFICATION_POLICY` app-op.
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
(status and the remote mode switch). It does not repeat the role commands, mute
anything, silence or answer calls.

### Status bar

With the app as the default dialer and Google Dialer disabled, no full-screen call
screen appears. Android may still show its own small "ongoing call" indicator in the
status bar; that belongs to the system and the app does not hide it.

## Normal phone mode

The app's main switch, **AI answers my calls**, moves the phone between two modes;
the web console does the same remotely (`set_mode`, see below). A switch is refused
while a call rings or is active and needs root. It can simply be repeated: every
step runs again and is checked, and the first one that does not take effect is
reported as a code (`dialer_failed`, `screening_failed`, `dnd_failed`,
`volume_failed`, `save_failed`, `no_root`, `in_call`, `busy`).

| | AI mode (`ai`, default) | Normal phone (`normal`) |
|---|---|---|
| Default phone app | Myna; Google Dialer disabled | Google Dialer, enabled again (on phones without it, Android's own) |
| Call screening role | Myna (silences the ringer) | removed from Myna |
| Do Not Disturb | on | off |
| Ringer and volumes | silent and zero; earpiece at minimum | ringer normal; ring, notification, earpiece, media, alarm and system volume restored |
| Microphone | muted during calls | left alone |
| Incoming calls | answered by the AI | ring; you answer |
| App service | full call handling | control connection only |

Where it is stored:

- `/data/adb/pixelcallai/mode`: `ai` or `normal`, written by the app through
  `su`. It is outside the module folder, so module updates keep it. `service.sh`
  reads it at every boot; **no file means `ai`**, so a phone that was never switched
  behaves exactly as before.
- `/data/adb/pixelcallai/normal_audio`: the volumes saved when the phone last went
  from normal to AI (`ring=…`, `notification=…`, `voice_call=…`, `music=…`,
  `alarm=…`, `system=…`). Switching to normal restores them; a missing or zero
  value becomes half of that volume's maximum.
- The app keeps a copy of the mode in its device-protected storage
  (`files/phone_mode`). When the service starts and the module's file says
  something else (changed by hand, app data cleared), the app applies the file's
  mode.

The older `com.pixelcallai.streamer.SET_ENABLED` service intent
(`--ez enabled false` / `true`) now performs the same switch.

## Rollback

0. Switch the phone to a [normal phone](#normal-phone-mode) first. That re-enables
   Google Dialer, turns Do Not Disturb off and restores the volumes, so steps 2 and
   3 below are then already done.
1. In the Magisk or KernelSU app, disable or remove the **Myna Uplink**
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

The manifest also sets `usesCleartextTraffic="true"`, because `ws://` to localhost or
to a PC on your own network is not encrypted.

## How a call flows

1. **Idle.** The service keeps a *control* WebSocket open to the agent and sends a
   `ping` every 15 seconds.
2. **Ringing.** The app does not answer yet. It waits up to 12 seconds for the control
   connection, tells the agent about the call (`incoming`), and rings for as long as
   the agent's `answer_plan` says (3 seconds if the agent does not reply within
   500 ms, never more than 8 seconds).
3. **Answer.** The app answers without any sound or screen on the phone.
4. **In call.** It opens a second *media* WebSocket, sends `hello`, streams the
   caller's audio to the agent and plays the audio it receives into the call.
5. **End.** Either side can end the call: the agent sends `hangup`, or the caller
   hangs up and the app sends `call_end`.

A call you dial by hand on the phone is left alone: the AI is not attached.

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
| `hello` | `serial`, `mode` = `"CONTROL_IDLE"`, `play_tts_locally` = `false`, `model`, `app_version`, `phone_mode` (`"ai"` / `"normal"`), `in_call` | Right after connecting. `phone_mode` is not called `mode` because `mode` names the connection. Apps before the mode switch send neither. |
| `ping` | `t` (milliseconds since epoch) | Every 15 s. |
| `status` | `mode` (`"ai"` / `"normal"`), `in_call` | The phone mode or the call state (ringing or active vs idle) changed. |
| `set_mode_result` | `id`, `ok`, `mode`, `error` | Answer to `set_mode`. `mode` is the mode afterwards; `error` is empty on success, otherwise a code from [Normal phone mode](#normal-phone-mode). |
| `incoming` | `serial`, `caller_id`, `sample_rate` | A call is ringing. |
| `incoming_cancel` | `serial` | The call ended before it was answered. |
| `place_call_ack` | `call_id`, `task_id`, `number`, `reason` = `""` | A requested call was dialed. |
| `place_call_failed` | `call_id`, `task_id`, `number`, `reason` | A requested call could not be dialed. |
| `outbound_ready` | `call_id`, `task_id`, `number` | A requested call connected. |
| `call_end` | `call_id`, `task_id`, `serial`, `reason` = `"normal"` | A requested call ended. |

Agent to app:

| `type` | Fields | Effect |
|---|---|---|
| `answer_plan` | `delay_ms`, `prewarm` | How long to let the call ring before answering. `prewarm` is only logged. |
| `place_call` | `number`, `call_id`, `task_id` | Dial `number`. |
| `set_mode` | `mode` (`"ai"` / `"normal"`), `id` | Switch the phone mode; answered with `set_mode_result`. |
| `agent_info` | `console_url` | The web console's address, for the app's **Open the web console** button. Sent after `hello` only when the agent knows it; the app forgets the old one on every new connection. |
| `blocklist_sync` | `numbers` (array) | Stored, but not used: the app never rejects callers. |
| `hello_ack`, `hello_ok`, `ack`, `pong` | | Logged only. |

If the control connection drops, the app reconnects after 1.5 seconds.

### Media connection (one per answered call)

App to agent:

| `type` | Fields | When |
|---|---|---|
| `hello` | `serial`, `caller_id`, `sample_rate` (16000), `mode` = `"SILENT_LOCAL"`, `play_tts_locally` = `false`, `local_menu` (`false` in this build), `model`, `app_version` | Right after connecting. |
| `ringing` | `serial`, `caller_id` | Right after `hello`, and again if the caller's number only becomes known later. |
| `call_end` | `serial`, `caller_id` | The call ended. The app closes the connection shortly afterwards. |
| `local_menu_started`, `local_menu_done`, `local_menu_failed` | | Only in builds with `LOCAL_MENU_ENABLED = true`; never sent by default. |

`caller_id` can be an empty string in `hello` when Android has not reported the number
yet.

Binary frames, app to agent: the caller's voice as raw PCM, signed 16-bit
little-endian, mono, 16 kHz, 60 ms per frame (1920 bytes). They are sent continuously
during the call, including silence.

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
limiter at about -2 dBFS.

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
| The phone does not answer at all | The phone is in normal phone mode (**AI answers my calls** is off), the service is stopped, or the app lacks `ANSWER_PHONE_CALLS`. Without a control connection the app still answers after about 12 seconds. |
| The module made the phone fail to boot | Use the root manager's safe mode, or create `/data/adb/modules/pixelcallai_uplink/disable` from recovery, then reboot. |

## Legal

Recording calls, and having an AI speak to callers without telling them, is regulated
differently from place to place. Find out what applies to you before using this.
You are responsible for how you use it.

`magisk/pixelcallai_uplink/META-INF/com/google/android/update-binary` carries its own
copyright and license header (GPL-3.0-only).
