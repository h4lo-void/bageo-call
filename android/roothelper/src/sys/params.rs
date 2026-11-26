// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Audio HAL parameters through Android's own Java runtime: the helper starts
//! `/system/bin/app_process` with a small class of this project
//! (`android/roothelper/java/.../AudioParams.java`, shipped as
//! `bageo-audio-params.jar`) that calls `AudioSystem.getParameters` or
//! `setParameters`, the same calls an app's `AudioManager.setParameters` ends up in.
//! The interface to the audio server changes between Android versions; the Java
//! method does not, so this works without knowing the binder transaction numbers.
//! A call takes a few hundred milliseconds (the runtime starts each time).

use std::env;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::control::ParamBackend;
use crate::params;

const CLASS: &str = "com.pixelcallai.roothelper.AudioParams";
const TIMEOUT: Duration = Duration::from_secs(8);
const MAX_OUTPUT: u64 = 64 * 1024;

/// The runtime settings `app_process` needs; taken from zygote when this process
/// was started without them.
const RUNTIME_ENV: &[&str] = &[
    "ANDROID_ROOT",
    "ANDROID_DATA",
    "ANDROID_ART_ROOT",
    "ANDROID_I18N_ROOT",
    "ANDROID_TZDATA_ROOT",
    "BOOTCLASSPATH",
    "DEX2OATBOOTCLASSPATH",
    "SYSTEMSERVERCLASSPATH",
];

pub struct AppProcessParams {
    jar: PathBuf,
}

impl AppProcessParams {
    /// The jar next to the running helper.
    pub fn next_to_helper() -> AppProcessParams {
        let dir = env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_default();
        AppProcessParams { jar: dir.join(super::PARAMS_JAR) }
    }

    fn run(&self, args: &[&str]) -> Result<String, String> {
        if !self.jar.is_file() {
            return Err("the audio parameter tool is not installed".into());
        }
        let mut cmd = Command::new("/system/bin/app_process");
        cmd.env("CLASSPATH", &self.jar)
            .arg("/system/bin")
            .arg(CLASS)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if env::var_os("BOOTCLASSPATH").is_none() {
            for (k, v) in zygote_env() {
                cmd.env(k, v);
            }
        }
        let mut child = cmd.spawn().map_err(|e| format!("cannot start app_process: {e}"))?;
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if start.elapsed() < TIMEOUT => thread::sleep(Duration::from_millis(20)),
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("the audio parameter tool did not answer".into());
                }
                Err(e) => return Err(e.to_string()),
            }
        };
        let mut out = String::new();
        if let Some(stdout) = child.stdout.take() {
            let _ = stdout.take(MAX_OUTPUT).read_to_string(&mut out);
        }
        if status.success() {
            Ok(out)
        } else {
            Err(format!("the audio parameter tool failed ({status}): {}", out.trim()))
        }
    }
}

/// The runtime settings from zygote's environment (Android's own Java runtime).
fn zygote_env() -> Vec<(String, String)> {
    let Ok(dir) = fs::read_dir("/proc") else { return Vec::new() };
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().filter(|s| s.bytes().all(|b| b.is_ascii_digit())) else { continue };
        let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) else { continue };
        let argv0 = cmdline.split(|&b| b == 0).next().unwrap_or(&[]);
        if argv0 != b"zygote64" && argv0 != b"zygote" {
            continue;
        }
        let Ok(environ) = fs::read(format!("/proc/{pid}/environ")) else { continue };
        return environ
            .split(|&b| b == 0)
            .filter_map(|kv| std::str::from_utf8(kv).ok()?.split_once('='))
            .filter(|(k, _)| RUNTIME_ENV.contains(k))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
    }
    Vec::new()
}

impl ParamBackend for AppProcessParams {
    fn get(&mut self, key: &str) -> Result<Option<String>, String> {
        if !params::valid_key(key) {
            return Err("bad parameter name".into());
        }
        let reply = self.run(&["get", key])?;
        Ok(params::value_in_reply(&reply, key))
    }

    fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        if !params::valid_key(key) || !params::valid_value(value) {
            return Err("bad parameter".into());
        }
        self.run(&["set", &format!("{key}={value}")]).map(|_| ())
    }
}
