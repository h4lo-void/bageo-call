# Notices

Bageo is released under the MIT License (see `LICENSE`), except for the
parts listed here.

## Files under other terms

- `android/magisk/pixelcallai_uplink/META-INF/com/google/android/update-binary`
  — module installer script, GPL-3.0-only (see the header of the file). It is a
  separate program run by the root manager when the module is installed; it is
  not linked into the app or the agent.
- `assets/ambience_*.wav` — built from CC0 (public-domain) recordings from
  Freesound plus sounds synthesised for this project. Sources are listed in
  `assets/README.md`.

## Third-party software

The agent is built from Rust crates listed in `Cargo.lock` (each under its own
license, mostly MIT / Apache-2.0); the Android app uses AndroidX Core,
Java-WebSocket (MIT) and kotlinx.coroutines (Apache-2.0). The start scripts can
download Android SDK Platform-Tools (adb) from Google, which is distributed
under Google's own terms and is not part of this repository.

## Services

The agent talks to services you choose and configure with your own keys
(Google Gemini for the voice, optionally another model for summaries, and the
notification services you add). Their terms and prices are between you and
them. This project is not affiliated with Google or any of those services;
"Pixel" is a trademark of Google LLC and is used here only to say which phones
the project was tested on.
