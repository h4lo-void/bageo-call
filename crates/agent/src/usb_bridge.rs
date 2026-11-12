//! USB link to the phones: keeps `adb reverse` up so the app's
//! `ws://127.0.0.1:8765` reaches this agent through the USB cable, and starts
//! the app's call service when a phone is plugged in but not connected.
//!
//! Rules (learned the hard way on a phone farm):
//! - never remove a healthy reverse; only add it when `reverse --list` lacks it
//!   (re-adding drops the phone's open sockets);
//! - start the call service only after a reverse was restored, or when fewer
//!   phones are connected than plugged in and no call is running — and not more
//!   often than every [`ARM_BACKOFF`] per phone;
//! - one adb binary: mixing versions makes each one restart the adb server,
//!   which wipes every reverse. Set `ADB_BIN` when several are installed.
//!
//! Off with `USB_BRIDGE=off` (phones on Wi-Fi, or another machine does this).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::process::Command;
use tracing::{info, warn};

use crate::config::{env_bool, env_or, env_parse};

/// The Android app.
pub const APP_PACKAGE: &str = "com.pixelcallai.streamer";
/// Port the app dials on the phone's own loopback.
pub const PHONE_PORT_DEFAULT: u16 = 8765;
const ARM_BACKOFF: Duration = Duration::from_secs(90);
const ADB_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
pub struct Config {
    pub enabled: bool,
    /// `ADB_BIN`; empty = look for it.
    pub adb: String,
    pub phone_port: u16,
    pub agent_port: u16,
    /// `ADB_SERIALS`: only these phones; empty = every phone plugged in.
    pub serials: Vec<String>,
    pub interval: Duration,
}

impl Config {
    pub fn from_env(agent_port: u16) -> Self {
        Self {
            enabled: env_bool("USB_BRIDGE", true),
            adb: env_or("ADB_BIN", ""),
            phone_port: env_parse("USB_PHONE_PORT", PHONE_PORT_DEFAULT),
            agent_port,
            serials: env_or("ADB_SERIALS", "")
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            interval: Duration::from_secs(env_parse("USB_BRIDGE_INTERVAL_SEC", 15u64).clamp(5, 300)),
        }
    }
}

/// What the web console shows about the USB side.
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub enabled: bool,
    pub adb_path: String,
    pub error: String,
    pub phones: Vec<Phone>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Phone {
    pub serial: String,
    /// `device`, `unauthorized`, `offline`, …
    pub state: String,
    pub model: String,
    pub reverse: bool,
    pub app_installed: Option<bool>,
}

static STATUS: Mutex<Option<Status>> = Mutex::new(None);

pub fn status() -> Status {
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

pub fn status_json() -> Value {
    let s = status();
    json!({
        "enabled": s.enabled,
        "adb_found": !s.adb_path.is_empty(),
        "adb_path": s.adb_path,
        "error": s.error,
        "phones": s.phones.iter().map(|p| json!({
            "serial": p.serial,
            "state": p.state,
            "model": p.model,
            "reverse": p.reverse,
            "app_installed": p.app_installed,
        })).collect::<Vec<_>>(),
    })
}

fn set_status(s: Status) {
    *STATUS.lock().unwrap_or_else(|e| e.into_inner()) = Some(s);
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "adb.exe"
    } else {
        "adb"
    }
}

/// Places adb usually lives, most specific first.
fn adb_candidates(configured: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if !configured.trim().is_empty() {
        out.push(PathBuf::from(configured.trim()));
    }
    let exe = exe_name();
    // Next to the program / in the working directory (the start scripts put
    // Google's platform-tools there).
    out.push(PathBuf::from("platform-tools").join(exe));
    if let Ok(me) = std::env::current_exe() {
        let mut dir = me.parent().map(|p| p.to_path_buf());
        for _ in 0..4 {
            let Some(d) = dir else { break };
            out.push(d.join("platform-tools").join(exe));
            dir = d.parent().map(|p| p.to_path_buf());
        }
    }
    for var in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        if let Ok(sdk) = std::env::var(var) {
            out.push(PathBuf::from(sdk).join("platform-tools").join(exe));
        }
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        out.push(PathBuf::from(local).join("Android").join("Sdk").join("platform-tools").join(exe));
    }
    if let Ok(home) = std::env::var("HOME") {
        let home = PathBuf::from(home);
        out.push(home.join("Library/Android/sdk/platform-tools").join(exe));
        out.push(home.join("Android/Sdk/platform-tools").join(exe));
    }
    for p in ["/opt/homebrew/bin/adb", "/usr/local/bin/adb", "/usr/bin/adb"] {
        out.push(PathBuf::from(p));
    }
    out
}

async fn run(adb: &str, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new(adb);
    cmd.args(args).kill_on_drop(true).stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        // No console window flashing up every few seconds.
        cmd.creation_flags(0x0800_0000);
    }
    match tokio::time::timeout(ADB_TIMEOUT, cmd.output()).await {
        Err(_) => Err(format!("adb {} timed out", args.join(" "))),
        Ok(Err(e)) => Err(format!("adb could not start: {e}")),
        Ok(Ok(out)) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            if out.status.success() {
                Ok(text)
            } else {
                Err(text.trim().chars().take(200).collect())
            }
        }
    }
}

/// The first candidate that exists, else plain `adb` when it is on the PATH.
async fn find_adb(configured: &str) -> Option<String> {
    for c in adb_candidates(configured) {
        if c.is_file() {
            return Some(c.to_string_lossy().into_owned());
        }
    }
    run("adb", &["version"]).await.ok().map(|_| "adb".to_string())
}

/// `adb devices` → (serial, state). Emulators and network devices included;
/// `ADB_SERIALS` narrows it down.
pub fn parse_devices(out: &str) -> Vec<(String, String)> {
    out.lines()
        .skip_while(|l| !l.starts_with("List of devices"))
        .skip(1)
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let serial = parts.next()?;
            let state = parts.next()?;
            (!serial.starts_with('*')).then(|| (serial.to_string(), state.to_string()))
        })
        .collect()
}

/// `adb reverse --list` already maps the phone's port to the agent's.
pub fn has_reverse(listed: &str, phone_port: u16, agent_port: u16) -> bool {
    let want = format!("tcp:{phone_port} tcp:{agent_port}");
    listed.lines().any(|l| l.trim().ends_with(&want))
}

/// Whether to start the call service on the plugged-in phones: fewer are
/// connected than plugged in, and nothing is in a call.
pub fn should_arm(connected_phones: usize, usb_phones: usize, active_calls: usize) -> bool {
    usb_phones > 0 && connected_phones < usb_phones && active_calls == 0
}

/// Runs for the life of the agent. `counts` = (phones connected to the agent,
/// calls in progress).
pub fn spawn<F, Fut>(cfg: Config, counts: F)
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = (usize, usize)> + Send,
{
    if !cfg.enabled {
        info!("usb bridge off (USB_BRIDGE=off): phones must reach the agent on their own");
        set_status(Status::default());
        return;
    }
    let counts = Arc::new(counts);
    tokio::spawn(async move {
        let mut adb: Option<String> = None;
        let mut last_arm: HashMap<String, Instant> = HashMap::new();
        let mut models: HashMap<String, String> = HashMap::new();
        let mut installed: HashMap<String, bool> = HashMap::new();
        let mut said_missing = false;
        loop {
            if adb.is_none() {
                adb = find_adb(&cfg.adb).await;
                match &adb {
                    Some(path) => info!("usb bridge: adb={path} phone_port={} agent_port={}", cfg.phone_port, cfg.agent_port),
                    None => {
                        if !said_missing {
                            said_missing = true;
                            warn!("usb bridge: adb not found — install Android platform-tools or set ADB_BIN (looking again every minute)");
                        }
                        set_status(Status {
                            enabled: true,
                            error: "adb not found".to_string(),
                            ..Status::default()
                        });
                        tokio::time::sleep(Duration::from_secs(60)).await;
                        continue;
                    }
                }
            }
            let path = adb.clone().unwrap_or_default();
            let mut st = Status { enabled: true, adb_path: path.clone(), ..Status::default() };
            match run(&path, &["devices"]).await {
                Err(e) => {
                    st.error = e;
                    // The binary may have been moved or replaced: look again next round.
                    adb = None;
                }
                Ok(out) => {
                    let mut restored: Vec<String> = Vec::new();
                    for (serial, state) in parse_devices(&out) {
                        if !cfg.serials.is_empty() && !cfg.serials.contains(&serial) {
                            continue;
                        }
                        let mut phone = Phone { serial: serial.clone(), state: state.clone(), ..Phone::default() };
                        if state == "device" {
                            if !models.contains_key(&serial) {
                                if let Ok(m) = run(&path, &["-s", &serial, "shell", "getprop", "ro.product.model"]).await {
                                    models.insert(serial.clone(), m.trim().to_string());
                                }
                            }
                            if installed.get(&serial) != Some(&true) {
                                let found = run(&path, &["-s", &serial, "shell", "pm", "path", APP_PACKAGE])
                                    .await
                                    .is_ok_and(|o| o.contains("package:"));
                                installed.insert(serial.clone(), found);
                            }
                            phone.app_installed = installed.get(&serial).copied();
                            let listed = run(&path, &["-s", &serial, "reverse", "--list"]).await.unwrap_or_default();
                            if has_reverse(&listed, cfg.phone_port, cfg.agent_port) {
                                phone.reverse = true;
                            } else {
                                let from = format!("tcp:{}", cfg.phone_port);
                                let to = format!("tcp:{}", cfg.agent_port);
                                match run(&path, &["-s", &serial, "reverse", &from, &to]).await {
                                    Ok(_) => {
                                        info!("usb bridge: reverse {from} → {to} set on {serial}");
                                        phone.reverse = true;
                                        restored.push(serial.clone());
                                    }
                                    Err(e) => warn!("usb bridge: reverse failed on {serial}: {e}"),
                                }
                            }
                        }
                        phone.model = models.get(&serial).cloned().unwrap_or_default();
                        st.phones.push(phone);
                    }
                    let ready: Vec<String> = st
                        .phones
                        .iter()
                        .filter(|p| p.state == "device" && p.reverse && p.app_installed == Some(true))
                        .map(|p| p.serial.clone())
                        .collect();
                    let (connected, active) = counts().await;
                    let lagging = should_arm(connected, ready.len(), active);
                    for serial in &ready {
                        let why = if restored.contains(serial) {
                            "reverse restored"
                        } else if lagging && last_arm.get(serial).is_none_or(|t| t.elapsed() >= ARM_BACKOFF) {
                            "not connected"
                        } else {
                            continue;
                        };
                        let service = format!("{APP_PACKAGE}/.CallAudioService");
                        let action = format!("{APP_PACKAGE}.START");
                        let r = run(
                            &path,
                            &["-s", serial, "shell", "am", "start-foreground-service", "-n", &service, "-a", &action],
                        )
                        .await;
                        last_arm.insert(serial.clone(), Instant::now());
                        match r {
                            Ok(_) => info!("usb bridge: started the call service on {serial} ({why})"),
                            Err(e) => warn!("usb bridge: could not start the call service on {serial} ({why}): {e}"),
                        }
                    }
                }
            }
            set_status(st);
            tokio::time::sleep(cfg.interval).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_adb_devices() {
        let out = "* daemon not running; starting now at tcp:5037\n* daemon started successfully\nList of devices attached\n3A151FDJH000XY\tdevice\n4B201JEHN0ABCD\tunauthorized\nemulator-5554\toffline\n\n";
        assert_eq!(
            parse_devices(out),
            vec![
                ("3A151FDJH000XY".to_string(), "device".to_string()),
                ("4B201JEHN0ABCD".to_string(), "unauthorized".to_string()),
                ("emulator-5554".to_string(), "offline".to_string()),
            ]
        );
        assert!(parse_devices("List of devices attached\n\n").is_empty());
        assert!(parse_devices("error: no adb").is_empty());
    }

    #[test]
    fn reverse_is_only_added_when_missing() {
        let listed = "UsbFfs tcp:8765 tcp:8787\nUsbFfs tcp:9000 tcp:9000\n";
        assert!(has_reverse(listed, 8765, 8787));
        assert!(!has_reverse(listed, 8765, 8765));
        assert!(!has_reverse(listed, 876, 8787));
        assert!(!has_reverse("", 8765, 8787));
    }

    #[test]
    fn call_service_is_started_only_when_phones_lag_and_no_call_runs() {
        assert!(should_arm(0, 1, 0));
        assert!(should_arm(1, 3, 0));
        assert!(!should_arm(1, 1, 0));
        assert!(!should_arm(0, 1, 1), "never during a call");
        assert!(!should_arm(0, 0, 0));
    }

    #[test]
    fn configured_adb_is_looked_at_first() {
        let c = adb_candidates("/opt/custom/adb");
        assert_eq!(c[0], PathBuf::from("/opt/custom/adb"));
        assert!(c.iter().any(|p| p.ends_with(PathBuf::from("platform-tools").join(exe_name()))));
        assert!(adb_candidates("")[0].ends_with(PathBuf::from("platform-tools").join(exe_name())));
    }
}
