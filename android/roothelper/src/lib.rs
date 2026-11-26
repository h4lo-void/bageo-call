// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! bageo-audio-helper: the Bageo phone module's root helper for phones whose call audio
//! path the app cannot use on its own. It gives the app, and only the app, a short list
//! of audio operations: read and set mixer controls, read and set audio HAL parameters,
//! and stream call PCM from and to a sound device. Every change is undone when the
//! call's session ends, however it ends. See `README.md` next to `Cargo.toml`.
//!
//! Every module but `sys` is plain logic and is tested on any computer; `sys` is the part
//! that touches the phone (its own tests run on Linux).

pub mod abi;
pub mod allow;
pub mod auth;
pub mod calls;
pub mod control;
pub mod feeder;
pub mod json;
pub mod life;
pub mod mixer;
pub mod params;
pub mod peer;
pub mod probe;
pub mod proto;
pub mod session;
pub mod stream;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub mod sys;
