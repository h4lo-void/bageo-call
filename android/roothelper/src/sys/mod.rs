// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Everything that talks to the phone itself (Linux and Android only): ALSA devices,
//! the connection to the app, audio parameters, system properties, signals.

pub mod alsa;
pub mod cli;
pub mod log;
pub mod net;
pub mod params;
pub mod probe;
pub mod props;
pub mod server;

/// Root's folder for the phone module's state (shared with `mode_daemon.sh`).
pub const DATA_DIR: &str = "/data/adb/pixelcallai";
/// The session state file: present while a session has unsaved-back changes.
pub const STATE_FILE: &str = "/data/adb/pixelcallai/audio_session";
/// Held (flock) by the one running helper.
pub const LOCK_FILE: &str = "/data/adb/pixelcallai/audio_helper.lock";
/// The app listens on this abstract socket name while it wants the helper.
pub const SOCKET_NAME: &str = "com.pixelcallai.streamer.audio_helper";
pub const PACKAGES_LIST: &str = "/data/system/packages.list";
pub const APP_DATA_DIR: &str = "/data/user_de/0/com.pixelcallai.streamer";
/// The audio parameter tool, next to the helper binary.
pub const PARAMS_JAR: &str = "bageo-audio-params.jar";

pub fn abi_name() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64-v8a"
    } else if cfg!(target_arch = "arm") {
        "armeabi-v7a"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        "other"
    }
}
