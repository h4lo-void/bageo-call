# Phone-side architecture: how the rooted Android app bridges calls to the AI

This page shows how the Bageo app turns a live phone call on a rooted Android
phone into a two-way audio stream for the agent, and why it has to run as a
privileged system app. Build, install, permissions and the full message list
are in [README.md](README.md); the agent side is in
[docs/en/ARCHITECTURE.md](../docs/en/ARCHITECTURE.md). This page describes app
`1.6.9` (versionCode 33) in module `v1.6.9` (versionCode 25).

## Overview

```text
 caller <== phone network ==> modem / audio HAL
                                |          ^
          call audio (downlink) |          | AI audio (uplink)
                                v          |
                      VoiceCallCapture   TelephonyUplinkInjector
                      AudioRecord        AudioTrack -> TYPE_TELEPHONY
                                |          ^
                                v          |
                              BridgeClient (media WebSocket, one per call)
     app -> agent: hello, ringing, call_end, add_call_result
     agent -> app: ready, tts_start/end, dtmf, hangup, add_call, add_call_abort
                                |          ^
             binary PCM frames  |          |  binary PCM frames + JSON
                                v          |
                         agent on the PC (speech AI)

 AgentControlClient (control WebSocket, open while the service runs, ConnectionPlan USB/Wi-Fi)
     app -> agent: hello (with sims), ping, status (with sims), incoming,
                   incoming_cancel, reject_failed, call_waiting_rejected,
                   place_call_ack / _failed, outbound_ready, call_end,
                   send_sms_result, send_sms_delivered, set_mode_result
     agent -> app: answer_plan, place_call, send_sms, set_mode,
                   agent_info (console_url, wifi.urls), blocklist_sync

 PhoneModeManager --request file--> mode_daemon.sh (root, started by the module)
                                        |
                                        v
                                   apply_mode.sh  (roles, Do Not Disturb, volumes)
```

- **Audio format:** raw PCM both ways, signed 16-bit little-endian, mono,
  16 kHz. The capture side sends 60 ms frames.
- **Address:** both WebSockets go to `ws://127.0.0.1:8765` by default, which
  `adb reverse` maps to the agent over USB. Change it on the setup screen
  (`Prefs.agentUrl`) to use Wi-Fi.
- **Messages:** every field is listed under
  [WebSocket protocol](README.md#websocket-protocol).

## Components

| Class | Role |
|---|---|
| `CallAudioService` | Foreground service and state machine. Watches the call state, decides when to answer, owns both WebSocket clients, the capture loop and the injector. |
| `AgentControlClient` | Control WebSocket. Announces the phone, keeps the link alive, reports ringing calls, receives the agent's commands. Reconnects by itself. |
| `BridgeClient` | Media WebSocket for one call. Sends `hello`, then call audio; hands received audio and JSON to the service and reports a dropped link. |
| `VoiceCallCapture` | `AudioRecord` on the far-end call audio. Tries `VOICE_DOWNLINK` first, then `VOICE_CALL`. |
| `TelephonyUplinkInjector` (`UplinkInjector.kt`) | `AudioTrack` whose preferred device is the `TYPE_TELEPHONY` output, fed from a queue by a worker thread. |
| `LocalAudioGuard` | Keeps the phone itself silent: microphone muted, speaker off, earpiece at minimum, ringer and Do Not Disturb. All audio system calls run on one worker thread. |
| `SilentInCallService` | The in-call service Android binds because the app is the default dialer. Shows nothing, never answers by itself, tells `CallAudioService` when a call becomes active or ends. Keeps each call's number and SIM, declines a second call that rings during a call, leaves calls dialed by hand alone, and handles `add_call` / 3-way conference merging for urgent calls. |
| `SilentCallScreeningService` | Call screening role. Allows every call and asks Android to silence the ringtone. Runs in its own process (`:screening`). |
| `TransparentDialerActivity` | The activity a default dialer must have. Invisible; finishes at once. |
| `DialerRoleHelper` | Takes the dialer and call screening roles and disables Google Dialer's call UI, through `su`. |
| `OutboundCallHelper` | Dials a number when the agent sends `place_call`, from the SIM its `sub_id` names. |
| `SimList` | Reads active SIM subscription IDs, countries, slots, carriers and numbers (Android 13+ with `READ_PHONE_NUMBERS`) for `sims` in `hello` and `status`. |
| `SimSubscriptions` | Reads the optional `sub_id` of an agent command and picks that subscription's phone account. A missing or invalid id means the default SIM. SIMs are never matched by carrier name or by their own number. |
| `ConnectionPlan` | Picks USB (`ws://127.0.0.1:8765`) first and paired Wi-Fi URLs (`agent_info.wifi.urls`) next, or only a manually typed address when changed. |
| `SmsSender` | Sends a text from the phone's number when the agent sends `send_sms` (replies to appointment requests), from the SIM its `sub_id` names, else the default SMS SIM. Answers with `send_sms_result` and `send_sms_delivered`, keeps the results of the last 20 ids and resends them after a reconnect. Needs `SEND_SMS`, which the module grants at boot and on every switch to AI mode. |
| `PhoneModeManager` | The AI / normal phone switch: writes the request for the module's root helper (`files/mode/request`), reads its answer (`files/mode/state`), keeps the app's copy of the mode, repeats an unfinished switch at start-up, and saves and restores the normal phone's volumes. See [Normal phone mode](README.md#normal-phone-mode). |
| `RootShell` | Runs the optional `su` commands (roles, mute); the app works without root. |
| `BootReceiver` | Starts the service after boot and after the app is replaced. |
| `MainActivity` | Setup and status screen. Not involved in calls. |
| `Prefs` | Settings in device-protected storage, readable before the first unlock. |
| `LocalMenuPlayer` | Plays audio clips bundled in the APK into the call. Only the "please wait" clip is reachable in the default build (`LOCAL_MENU_ENABLED = false`). |
| `SmsRilHook` | Optional LSPosed hook; inactive without an LSPosed framework. See [Optional SMS hook](README.md#optional-sms-hook). |
| `TtsPlayer` | Deliberately empty. Playing AI audio on the phone's own speaker is not supported. |

## Call sequence

An incoming call:

```text
RINGING
  SilentCallScreeningService: allow the call, silence the ringtone
  SilentInCallService:        mute, do not answer; keep this call's number and SIM
                              (another call already up: decline this one at once)
  CallAudioService:           wait for the control WebSocket (max 12 s; not up: do not
                              answer, the call rings on to missed call / voicemail)
                              wait for this call's number and SIM (max 150 ms)
                              send "incoming", wait for "answer_plan" (max 0.5 s)
                              "reject": decline the call, do not answer
                              keep ringing for delay_ms (default 3 s, max 8 s)
                              answer
OFFHOOK
  CallAudioService:           wait 50 ms
                              in parallel: start TelephonyUplinkInjector
                                           open BridgeClient, send "hello" + "ringing"
                              start VoiceCallCapture loop -> binary frames to the agent
                              agent binary frames -> injector queue -> call uplink
                              media WebSocket lost for 3 s -> end the call
IDLE
  CallAudioService:           send "call_end", close the media WebSocket,
                              stop capture and injector, stay armed for the next call
```

An outgoing call (AI call-backs and AI errands):

```text
place_call {number, call_id, task_id, sub_id}
  CallAudioService:           a call rings or is up -> place_call_failed (reason "busy")
  OutboundCallHelper:         dial from the phone account of sub_id (default if not found)
                              -> place_call_ack; not dialing within 30 s -> forget it
OFFHOOK (the call the agent asked for)
  CallAudioService:           outbound_ready; open BridgeClient, "hello" with sim_sub_id
                              (the agent hangs up an errand that went out on another SIM)
                              audio as for an incoming call
IDLE
  CallAudioService:           "call_end" with call_id and task_id
```

## Details that matter

- **The call is answered late on purpose.** Ringing about once gives the agent
  time to prepare and sounds like a person picking up.
- **The audio track is not created while ringing.** A comment in the source
  records that on Android 16, creating it during the call-mode switch could
  block inside the audio server and leave the call silent, so the injector
  waits for `MODE_IN_CALL`.
- **Audio that arrives before the route is ready is queued, not dropped**, so
  the first words are not lost.
- **The audio track is never left idle.** Once nothing has been queued for
  100 ms, the injector writes 40 ms of silence whenever the track is about to
  run dry. On the Pixel 9 an idle track let the audio HAL sleep, and the next
  write rebuilt the track (about 0.6 s) and cut off a short first word. Queued
  audio always goes first.
- **Only the preferred device is checked, not the routed one.** The source
  notes that on the tested Pixels the routed device often reports the earpiece
  even though the audio does reach the call.
- **No fallback to speaker or earpiece.** If the `TYPE_TELEPHONY` route
  cannot be set, the injector writes nothing and the app posts a notification.
- **One media session per phone call.** Duplicate "call active" signals keep
  the existing session; a different call tears the old one down first. Every
  callback carries a session generation number, so late callbacks from an
  earlier call are ignored.
- **A call dialed by hand is left alone.** Only incoming calls and calls the
  agent asked for with `place_call` get an AI session. For a call dialed by
  hand, the app releases the Telecom mute, the microphone mute and the minimum
  earpiece volume, and mutes again when it ends. If the app did not answer a
  call and Android cannot tell its direction yet, the app asks again for
  200 ms and otherwise leaves the AI off.
- **One call at a time.** A call that rings while another is up is declined at
  once and reported as `call_waiting_rejected`. The call in progress keeps
  its number, SIM, media session and mute state.
- **The mode switch runs outside the app.** Taking the dialer role away from
  an app can make Android stop it, so the module's root helper
  (`mode_daemon.sh` → `apply_mode.sh`) does the switch, finishes even
  when the app is killed, and changes the phone app last. In normal mode the
  service keeps only the control connection (status, the remote switch,
  `send_sms`).
- **No dead air.** If the agent is not reachable while the phone rings, the
  call is not answered. If the media WebSocket drops during a call and is not
  back within 3 seconds, the call is ended.
- **The captured audio is quiet.** The earpiece volume is held at minimum so
  nobody near the phone hears the caller, and on the tested Pixels that volume
  also scales the captured audio. The agent amplifies it.

Something not working? See [Troubleshooting](README.md#troubleshooting).

## Why a privileged app

Android reserves both halves of the call audio path for the system:

- Recording a call needs `CAPTURE_AUDIO_OUTPUT`.
- Playing into a call needs an `AudioTrack` routed to the `TYPE_TELEPHONY`
  device, which the source documents as needing a privileged app with
  `MODIFY_PHONE_STATE`.

Both are `signature|privileged` permissions. The module places the APK under
`/system/priv-app` and adds an allow-list file, which is what lets Android
grant them. That is why the phone has to be rooted (Magisk or KernelSU).

## What is not here

- No speech recognition, language model or speech synthesis. Those run in the
  agent.
- No echo cancellation on the phone.
- No call blocking of its own. `SilentCallScreeningService` allows every
  call, and the list stored from `blocklist_sync` is not consulted. A ringing
  call is declined only when the agent's `answer_plan` says `reject`, or
  when another call is already up.
- No blind call transfer inside the app (direct forwarding uses carrier
  forward-when-busy). Adding and merging a second call into a 3-way conference
  is supported via `add_call` for urgent calls (trial).
- No decisions about outgoing calls: the agent decides which number, which SIM
  and when; the app dials what `place_call` says, or reports `busy`.

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
