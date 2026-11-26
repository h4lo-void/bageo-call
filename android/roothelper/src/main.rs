// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

#[cfg(any(target_os = "linux", target_os = "android"))]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(bageo_audio_helper::sys::cli::run(args));
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    if let Some(code) = bageo_audio_helper::appdir::run_ipc(&refs) {
        std::process::exit(code);
    }
    eprintln!("bageo-audio-helper runs on Android (and Linux) only");
    std::process::exit(1);
}

#[cfg(not(unix))]
fn main() {
    eprintln!("bageo-audio-helper runs on Android (and Linux) only");
    std::process::exit(1);
}
