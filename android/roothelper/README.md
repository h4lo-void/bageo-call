# bageo-audio-helper

The phone module's root helper for phones whose call audio path the Bageo app cannot
use by itself. It gives the app, and only the app, a short list of audio operations,
and only those that the helper's own built-in list allows for the phone:

- **mixer**: set a listed mixer control to a listed value;
- **params**: set a listed audio HAL parameter (`AudioSystem.setParameters`) to a
  listed value;
- **pcm**: record from or play to a listed sound device (s16le), only while the phone
  is in a call;
- **session**: everything changed during a call is put back when the call's session
  ends.

The built-in list (`src/allow.rs`) is **empty** in this version, so the app cannot
change anything yet; phone families are added by later changes. Reading the mixer,
parameters or the phone's audio facts is only possible from the root command line.

The module ships a debug build of the app today. The helper refuses a debuggable app
(see "Who it serves"), so **with the current module the helper refuses to serve**; it
can only be used once the module ships a release build of the app.

## When it runs

Not at boot and not by itself. The app asks the module's root helper
(`mode_daemon.sh`) by writing `start <id>` and a newline to `files/mode/audio_helper`
in its device-protected folder; that starts it when no phone mode switch waits, at
most once every 10 seconds, and not while it runs or is turned off.

It quits by itself:

- about a minute after the last connection from the app ended (a connection without a
  session ends 5 minutes after it began or after its last session ended, a session
  after 4 hours);
- within about a second of being turned off (`disable`), at once on a stop signal;
- when the app was reinstalled or changed, or after three connections in a row that
  did not present the key.

At boot the module runs `bageo-audio-helper restore` once, but only when a call's
changes were left behind (`/data/adb/pixelcallai/audio_session` exists); it puts them
back and exits.

## Who it serves

All of these must hold, or it refuses and logs one fixed reason:

- `/data/system/packages.list` (a plain file of at most 8 MiB) lists
  `com.pixelcallai.streamer` exactly once, as not debuggable (third field `0`), with a
  user ID of its own in the main user's app range (10000–19999, no other package with
  the same ID), and with the data folder `/data/user/0/com.pixelcallai.streamer`; any
  malformed line is a refusal;
- the owner of `/data/user_de/0/com.pixelcallai.streamer` is that same user ID, and
  its `files/mode` folder carries an app data SELinux label (`app_data_file`,
  `privapp_data_file` ...);
- the process listening on the socket has that user ID (`SO_PEERCRED`; the app's copy
  in a work profile or another user has another ID) and a privileged-app SELinux
  context (`priv_app` or `priv_app_<n>`, `SO_PEERSEC`);
- it presents this run's app key (below) as the first request of every connection.

## Keys

Each start draws two random 256-bit keys and publishes
`files/mode/audio_helper_key` in the app's device-protected folder: the file is made in
root's own `/data/adb/pixelcallai`, given to the app (owner, mode 0600, the folder's
SELinux label), renamed into the app's folder, and checked to be that same file there.
Without a label it can trust, or if the file is not the one it made, it does not serve.

```text
bageo-audio-helper-keys 1
app=<64 hex digits>
helper=<64 hex digits>
```

The file is removed when the helper quits (only that file: if the app put another file
under the name, that one stays). A user-ID check alone would let in any process that
runs as the app's user ID in another SELinux context (`run-as` on a debuggable build, a
package sharing the ID); such a process cannot read the file.

## What the app must do (next change)

1. Listen on the abstract Unix socket `com.pixelcallai.streamer.audio_helper`
   (`LocalServerSocket`), then write `start <id>` and a newline to
   `files/mode/audio_helper`.
2. For every accepted connection, before trusting anything:
   - the peer's user ID (`LocalSocket.getPeerCredentials().getUid()`) must be `0`;
   - read the first frame, a hello (kind 4): `bageo-audio-helper`, protocol `1`, then
     `control` and the helper key, or `pcm`, the helper key, stream id, direction,
     rate, channels, period frames, periods; the helper key must equal `helper=` in the
     key file (compare in constant time). The helper runs in the root manager's SELinux
     domain (`magisk` or `su`); an app cannot read another process's context, so the
     key is what tells the real helper from another process.
   - send the request `auth` with the `app=` key as the first request.
3. Then send requests on the control connection; on a PCM connection, raw s16le PCM
   follows (read it for capture, write it for playback). Close the listening socket
   when the helper is no longer wanted.
4. Playback: send audio at the device's pace (one period per period's time), or keep
   the socket's send buffer small. The helper takes at most 8 periods ahead and leaves
   the rest in the connection, so the app's writes block and slow down by themselves;
   audio sent ahead is played late, never dropped. Only `pcm.flush` drops audio: send
   it after you stopped sending the old audio (for example when the other side talks
   over the AI); it answers `ok` once the stream dropped what waited, what the
   connection held at that moment, and what the device held.
5. When the call ends the helper closes the streams and the control connection, and
   puts the session back; connect again for the next call.

Frames, commands, time limits: see the top of [`src/proto.rs`](src/proto.rs) and
[`src/control.rs`](src/control.rs).

## Putting things back

Before a change the old value read from the phone is saved to
`/data/adb/pixelcallai/audio_session` (root only). The session's end puts it back:
when the app ends it, the connection breaks or runs out of time, the call ends, or the
helper stops. Stopping puts back only while nothing else is being changed; if a change
does not finish within 3 seconds, the saved values stay in the file and a restore that
waits for the stopped helper to be gone (`restore --wait`, started by the helper) puts
them back. Failing all of those, the next start or boot puts them back.

A value that could not be put back stays in the file and is tried again at the next
start or boot (a running helper tries again at most every 30 seconds), up to 5 times in
all; then only `restore --force` tries it. While the file exists no new session
starts. Values of controls or parameters no longer on the list are kept in the file,
not written.

What is not promised: a value the phone refuses to take back (the control is gone,
the driver fails) cannot be put back by any of these; it stays in the file until it
can be, or until root gives it up (below). Restarting the phone resets most mixer
controls by itself, but not every phone resets every one.

## When something seems wrong

As root (`adb shell su -c '...'`), with `H=/data/adb/modules/pixelcallai_uplink/bin/bageo-audio-helper`
(`bageo_ai_call_agent` for the phone-only edition):

1. Is it running? `$H status` (`pidof bageo-audio-helper` prints nothing when not).
2. Put everything back: `$H restore`. `--force` also retries values that failed 5
   times, and drops (and logs) values whose control is gone or no longer on the list
   and lines it cannot read.
3. Give up what a call left, when it cannot be put back: `$H restore --discard` removes
   the state file whatever it holds, puts nothing back, and logs how many values were
   given up. New sessions can start again.
4. Turn it off: `$H disable` stops it (it also stops by itself within a second once
   the disable file is there), puts back what it can, and keeps it from starting
   (`$H enable` allows it again). If it says the helper did not stop, restart the
   phone.
5. Last resort: restart the phone; the module puts back what was left at boot.

## Command line (as root)

```text
bageo-audio-helper status | restore [--force|--discard] | disable | enable
bageo-audio-helper probe
bageo-audio-helper mixer list <card>
bageo-audio-helper mixer get <card> <control>
bageo-audio-helper mixer try <card> <control> <seconds> <value>...
bageo-audio-helper params get <key>
bageo-audio-helper version
```

`mixer try` is for checking a phone by hand: it may set any control root can put back
(not limited by the list), in a session of its own, and puts it back after the wait. A
control is its name, `<name>@<index>`, `numid=<n>`, or its whole identity
`id:<iface>:<device>:<subdevice>:<index>:<name>` when several controls share a name.

## Build

`android/scripts/build-roothelper.sh` builds it for arm64-v8a and armeabi-v7a with the
Android NDK and builds `bageo-audio-params.jar` (javac and d8), and puts both where the
module packaging picks them up. Tests: `cargo test` and `cargo clippy --all-targets --
-D warnings` here; they run on any computer (the phone parts' own tests on Linux), and
the phone parts are type-checked with `--target aarch64-linux-android`.

## Licenses

This crate is Bageo's (`AGPL-3.0-only OR LicenseRef-Bageo-Commercial`). Its one
dependency, `libc`, is `MIT OR Apache-2.0`. Some approaches follow gsm2sip
(<https://github.com/pulpoff/gsm2sip>, MIT); no gsm2sip code is included.

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
