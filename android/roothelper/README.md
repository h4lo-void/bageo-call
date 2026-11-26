# bageo-audio-helper

The phone module's root helper for phones whose call audio path the Bageo app cannot
use by itself. It gives the app, and only the app, a short list of audio operations:

- **mixer**: list the controls of a sound card, read one, set one (switch, number,
  list item, several values);
- **params**: read and set audio HAL parameters (`AudioSystem.getParameters` /
  `setParameters`);
- **pcm**: record from or play to a sound device, s16le, with the rate, channels and
  period the app asks for;
- **session**: every change made in a call is undone when the call's session ends:
  when the app ends it, when the connection breaks, when the helper is stopped, or,
  if the helper was killed or the phone rebooted, the next time the helper starts or
  the phone boots;
- **probe**: read-only facts about the phone's audio (JSON) for the app to decide
  whether and how to use the helper.

It only provides these operations. Which control gets which value on which phone is
the app's decision. Phones that do not need it never run it.

## When it runs

Never at boot and never by itself. The app starts it through the module's root helper
(`mode_daemon.sh`) by writing `start <id>` to `files/mode/audio_helper` in its
device-protected folder. The helper quits after a minute without the app. At boot the
module runs `bageo-audio-helper restore` only when a call's changes were left behind
(`/data/adb/pixelcallai/audio_session` exists).

To see whether it runs: `adb shell su -c 'pidof bageo-audio-helper'` prints nothing
when it does not. Its log lines are tagged `bageo_audio_helper` in logcat.

## How the app talks to it

The app listens on the abstract Unix socket `com.pixelcallai.streamer.audio_helper`;
the helper connects to it (every 200 ms while it runs) and serves each connection the
app accepts. Before it sends or reads anything it checks the listener's user ID
(`SO_PEERCRED`) against the app's (from `/data/system/packages.list` and the owner of
the app's folder; both must agree). The app should check that the connecting side is
root (user ID 0) in the same way.

Because the helper (in the root manager's SELinux domain) connects to the app and not
the other way round, the module adds no SELinux rule.

Frames, commands and replies: see the top of [`src/proto.rs`](src/proto.rs) and
[`src/control.rs`](src/control.rs). The first frame of every connection is a hello
from the helper: `bageo-audio-helper`, protocol version, then `control`, or for a PCM
stream `pcm`, stream id, direction, rate, channels, period frames and periods; raw PCM
follows a stream's hello. The app ends a stream with `pcm.close` or by closing its
connection. The app closes its listening socket when it no longer wants the helper.

## Command line (as root)

```text
bageo-audio-helper probe
bageo-audio-helper mixer list <card>
bageo-audio-helper mixer get <card> <control>
bageo-audio-helper mixer try <card> <control> <seconds> <value>...
bageo-audio-helper params get <key>
bageo-audio-helper restore
bageo-audio-helper version
```

## Build

`android/scripts/build-roothelper.sh` builds it for arm64-v8a and armeabi-v7a with the
Android NDK and builds `bageo-audio-params.jar` (javac and d8), and puts both where the
module packaging picks them up. Tests: `cargo test` and `cargo clippy --all-targets --
-D warnings` here; they run on any computer, the phone parts are type-checked with
`--target aarch64-linux-android`.

## Licenses

This crate is Bageo's (`AGPL-3.0-only OR LicenseRef-Bageo-Commercial`). Its one
dependency, `libc`, is `MIT OR Apache-2.0`. Some approaches follow gsm2sip
(<https://github.com/pulpoff/gsm2sip>, MIT); no gsm2sip code is included.

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
