# Notices

Copyright (C) 2026 **h4lo-void** (<https://github.com/h4lo-void>).

Bageo is dual-licensed under the **GNU Affero General Public License v3.0 only**
(`AGPL-3.0-only`, verbatim text in [`LICENSE`](LICENSE)) together with the
Section 7 additional terms in [`LICENSING.md`](LICENSING.md), or a separate
written **Commercial License** from **h4lo-void** (see
[`COMMERCIAL_LICENSE.md`](COMMERCIAL_LICENSE.md)). General disclaimers and usage
notices are in [`LEGAL.md`](LEGAL.md).

The parts listed below come under other terms or from other projects.

## Files under other terms

- `crates/agent/data/opencc/*.txt` (`STCharacters.txt`, `STPhrases.txt`,
  `TSCharacters.txt`, `TSPhrases.txt`, `TWVariants.txt`,
  `TWVariantsPhrases.txt`, `TWVariantsRevPhrases.txt`) — unmodified Chinese
  conversion dictionary files from **OpenCC (Open Chinese Convert)** version
  `ver.1.4.2` (<https://github.com/BYVoid/OpenCC/tree/ver.1.4.2>), Copyright
  2010–2026 Carbo Kuo (`BYVoid`) and OpenCC contributors, licensed under the
  **Apache License, Version 2.0** (full text in
  [`LICENSES/Apache-2.0.txt`](LICENSES/Apache-2.0.txt)).
- `android/gradlew`, `android/gradlew.bat`, and
  `android/gradle/wrapper/gradle-wrapper.jar` — **Gradle Wrapper**, Copyright
  Gradle Inc., licensed under the **Apache License, Version 2.0** (full text in
  [`LICENSES/Apache-2.0.txt`](LICENSES/Apache-2.0.txt)).
- `android/magisk/pixelcallai_uplink/META-INF/com/google/android/update-binary`
  — module installer script, `GPL-3.0-only` (Copyright 2022–2024 Andrew
  Gunnerson; see the header of the file). It is a standalone installer script
  run by the root manager when the module zip is flashed; it is not linked into
  the Android app or the Bageo program.
- `assets/ambience_*.wav` — built from CC0 (public-domain) recordings from
  Freesound plus sounds synthesised for this project. Sources are listed in
  [`assets/README.md`](assets/README.md).
- `assets/voices/*/*.wav` — 1,080 prerecorded voice preview and
  "cannot be reached" announcement clips generated for this project with
  `agent make-voice-samples` (see [`assets/README.md`](assets/README.md)).

## Third-party software

The Bageo program is built from Rust crates listed in `Cargo.lock`, and the
**Bageo Status** desktop tray app (`desktop/`) is built with Tauri 2 and Rust
crates listed in `desktop/Cargo.lock`, each distributed under its own
open-source license. The Android app uses AndroidX Core (`Apache-2.0`),
Java-WebSocket (`MIT`), kotlinx.coroutines (`Apache-2.0`) and compile-only
Xposed API stubs (`Apache-2.0`). Bageo Status and the start scripts can download
Android SDK Platform-Tools (`adb`) from Google on request; `adb` is distributed
under Google's own terms and is not part of this repository.

## Trademarks & third-party services

The names **Bageo**, **八哥** and **bageo-call**, together with the project's
visual marks, are reserved identifiers of this project; the open-source license
grants no trademark rights (AGPL-3.0 Section 7(e); see
[`LICENSING.md`](LICENSING.md)).

Bageo talks only to services you choose and configure with your own keys:
Google Gemini for the live voice, optionally another model for summaries, and
the notification services you add. Their terms and prices are strictly between
you and them. This project is not affiliated with or endorsed by Google LLC or
any of those services. "Pixel", "Android" and "Gemini" are trademarks of
Google LLC and are used here solely to identify compatible hardware and APIs.

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
