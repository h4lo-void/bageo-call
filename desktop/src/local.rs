// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Running Bageo itself when it sits next to this program: finding it, reading
//! its .env, starting it without a window, keeping it running, and fetching adb.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::poll;
use crate::state::{lock, Shared};
use crate::status::{Level, Reason};

pub const DEFAULT_PORT: u16 = 8787;
/// Places to look for Bageo when no address is set.
const DEFAULT_CANDIDATES: [&str; 3] = ["http://127.0.0.1:8787/", "http://bageo.local/", "http://bageo.local:8787/"];
/// How long a freshly started Bageo may take to answer.
pub const LAUNCH_GRACE: Duration = Duration::from_secs(30);
/// Wait this long before starting Bageo again after it stopped.
const RESTART_DELAY: Duration = Duration::from_secs(5);
/// Stopping this many times within the window means something is wrong: give up.
const CRASH_LIMIT: usize = 4;
const CRASH_WINDOW: Duration = Duration::from_secs(300);
const WATCH_EVERY: Duration = Duration::from_secs(1);
const FIRST_CHECK_TIMEOUT: Duration = Duration::from_millis(1500);
/// How long Bageo gets to stop before it is stopped the hard way.
const STOP_WAIT: Duration = Duration::from_secs(3);
const LOG_NAME: &str = "bageo.log";
const LOG_OLD_NAME: &str = "bageo.log.1";
const LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
/// platform-tools is about 7 MB; anything far larger is not it.
const DOWNLOAD_MAX_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Platform {
    Windows,
    Mac,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Mac
        }
    }

    fn adb_name(self) -> &'static str {
        match self {
            Platform::Windows => "adb.exe",
            Platform::Mac => "adb",
        }
    }

    /// Google's download for the Android platform-tools.
    pub fn adb_url(self) -> &'static str {
        match self {
            Platform::Windows => "https://dl.google.com/android/repository/platform-tools-latest-windows.zip",
            Platform::Mac => "https://dl.google.com/android/repository/platform-tools-latest-darwin.zip",
        }
    }
}

// ---------------------------------------------------------------------------
// Pure decisions (no file system, no processes, no network)
// ---------------------------------------------------------------------------

/// Where the Bageo program may be, most specific first.
/// `exe_dir` is the folder that holds this program's executable.
/// `remembered` is where it was found the last time, tried last.
pub fn agent_candidates(exe_dir: &Path, override_path: Option<&str>, remembered: &str, platform: Platform) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = override_path.map(str::trim).filter(|p| !p.is_empty()) {
        out.push(PathBuf::from(p));
    }
    match platform {
        Platform::Windows => out.push(exe_dir.join("agent.exe")),
        Platform::Mac => {
            // The executable is in "Bageo Status.app/Contents/MacOS": look next to the .app first.
            if let Some(beside_app) = exe_dir.ancestors().nth(3) {
                out.push(beside_app.join("agent"));
            }
            out.push(exe_dir.join("agent"));
        }
    }
    let remembered = remembered.trim();
    if !remembered.is_empty() {
        out.push(PathBuf::from(remembered));
    }
    out
}

pub fn find_agent(candidates: &[PathBuf], exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    candidates.iter().find(|p| exists(p)).cloned()
}

/// Bageo inside a Mac app ("Bageo.app/Contents/MacOS/agent"): the app's
/// Contents folder. Nothing may be written inside an app, so such a Bageo
/// keeps its data elsewhere and reads its sounds from Contents/Resources.
pub fn mac_app_contents(agent: &Path) -> Option<PathBuf> {
    let macos = agent.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    let is_app = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension().is_some_and(|e| e.eq_ignore_ascii_case("app"));
    is_app.then(|| contents.to_path_buf())
}

/// Where Bageo from a Mac app keeps its data, .env and log.
pub fn mac_data_dir(home: &Path) -> PathBuf {
    home.join("Library").join("Application Support").join("Bageo")
}

fn unquote(v: &str) -> &str {
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return &v[1..v.len() - 1];
        }
    }
    v
}

/// Reads KEY=VALUE lines the way the start scripts do: blank lines, lines that
/// start with # and lines without = are skipped; one pair of quotes around the
/// value is removed. The first line for a key wins.
pub fn parse_env(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.trim_start_matches('\u{feff}').lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let Some((key, val)) = t.split_once('=') else { continue };
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let val = unquote(val.trim());
        if val.contains('\0') || out.iter().any(|(k, _)| k == key) {
            continue;
        }
        out.push((key.to_string(), val.to_string()));
    }
    out
}

/// The .env values to hand to Bageo: only those the real environment does not set.
pub fn child_env(parsed: &[(String, String)], is_set: impl Fn(&str) -> bool) -> Vec<(String, String)> {
    parsed.iter().filter(|(k, _)| !is_set(k)).cloned().collect()
}

/// One setting: the real environment first, then .env.
pub fn setting(key: &str, real: impl Fn(&str) -> Option<String>, parsed: &[(String, String)]) -> Option<String> {
    real(key).or_else(|| parsed.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()))
}

pub fn port_from(value: Option<&str>) -> u16 {
    value.and_then(|v| v.trim().parse::<u16>().ok()).filter(|p| *p != 0).unwrap_or(DEFAULT_PORT)
}

/// The host to ask on this computer: 127.0.0.1 unless Bageo listens on one named address.
pub fn probe_host(agent_host: Option<&str>) -> String {
    let h = agent_host.map(str::trim).unwrap_or("");
    if h.is_empty() || h == "0.0.0.0" || h == "::" {
        "127.0.0.1".to_string()
    } else if h.contains(':') && !h.starts_with('[') {
        format!("[{h}]")
    } else {
        h.to_string()
    }
}

pub fn local_base(host: &str, port: u16) -> String {
    format!("http://{host}:{port}/")
}

/// Addresses to try when no address is set. With Bageo next to this program its
/// own address replaces the first one; when this program started Bageo, it is the only one.
pub fn discovery_candidates(local_base: Option<&str>, managed: bool) -> Vec<String> {
    let Some(local) = local_base else {
        return DEFAULT_CANDIDATES.iter().map(|s| s.to_string()).collect();
    };
    let mut out = vec![local.to_string()];
    if !managed {
        out.extend(DEFAULT_CANDIDATES[1..].iter().map(|s| s.to_string()));
    }
    out
}

/// Remembers when Bageo stopped, to decide whether to start it again.
#[derive(Default)]
pub struct Restarts {
    exits: VecDeque<Duration>,
}

impl Restarts {
    /// Records a stop at `now` (time since this program started).
    /// True when Bageo should be started again.
    pub fn on_exit(&mut self, now: Duration) -> bool {
        self.exits.push_back(now);
        while self.exits.front().is_some_and(|first| now.saturating_sub(*first) > CRASH_WINDOW) {
            self.exits.pop_front();
        }
        self.exits.len() < CRASH_LIMIT
    }

    pub fn clear(&mut self) {
        self.exits.clear();
    }
}

/// What to show while Bageo cannot be reached and this program is the one running it.
/// None: use the normal "cannot reach Bageo" handling.
pub fn launch_reason(managed: bool, gave_up: bool, connected: bool, since_launch: Duration) -> Option<(Level, Reason)> {
    if !managed {
        None
    } else if gave_up {
        Some((Level::Bad, Reason::LaunchFailed))
    } else if connected {
        None
    } else if since_launch < LAUNCH_GRACE {
        Some((Level::Unknown, Reason::Launching))
    } else {
        Some((Level::Bad, Reason::LaunchFailed))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum AdbJob {
    #[default]
    Idle,
    Downloading,
    Failed,
}

/// The `adb` field of the Snapshot: "", "missing", "downloading" or "failed".
pub fn adb_state(local_mode: bool, usb_enabled: bool, adb_found: bool, job: AdbJob) -> &'static str {
    if !local_mode {
        ""
    } else if job == AdbJob::Downloading {
        "downloading"
    } else if !usb_enabled || adb_found {
        ""
    } else if job == AdbJob::Failed {
        "failed"
    } else {
        "missing"
    }
}

/// Unpacks a zip into `dest`. Every entry must be inside the `platform-tools`
/// folder of the archive: nothing is written when one would land outside `dest`
/// or next to that folder (where the Bageo program and its settings are).
pub fn extract_zip<R: Read + Seek>(reader: R, dest: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(reader).map_err(|e| e.to_string())?;
    let mut targets: Vec<PathBuf> = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| e.to_string())?;
        match entry.enclosed_name() {
            Some(rel) if rel.starts_with("platform-tools") => targets.push(rel),
            _ => return Err(format!("unexpected path in archive: {}", entry.name())),
        }
    }
    for (i, rel) in targets.iter().enumerate() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let out = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        let mode = entry.unix_mode();
        // Links could point anywhere: leave them out.
        if mode.is_some_and(|m| m & 0o170000 == 0o120000) {
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut file = File::create(&out).map_err(|e| e.to_string())?;
        io::copy(&mut entry, &mut file).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        if let Some(m) = mode {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&out, fs::Permissions::from_mode(m & 0o777)).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The Bageo program on this computer
// ---------------------------------------------------------------------------

/// Bageo found next to this program.
pub struct Local {
    agent: PathBuf,
    /// Bageo's working directory, .env, log and platform-tools: the program
    /// folder, or ~/Library/Application Support/Bageo for the Mac app.
    dir: PathBuf,
    /// Where Bageo answers on this computer, with a trailing slash.
    pub base: String,
    /// .env values to give to Bageo.
    env: Vec<(String, String)>,
}

impl Local {
    /// Where the Bageo program is, to remember it for the next start.
    pub fn agent_path(&self) -> String {
        self.agent.to_string_lossy().into_owned()
    }
}

/// Looks for the Bageo program and reads its .env. None: this program only watches.
pub fn detect(remembered: &str) -> Option<Local> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;
    let override_path = std::env::var("BAGEO_AGENT").ok();
    let candidates = agent_candidates(exe_dir, override_path.as_deref(), remembered, Platform::current());
    let agent = find_agent(&candidates, |p| p.is_file())?;
    // Bageo from a Mac app works in ~/Library/Application Support/Bageo and
    // reads its sounds from the app; anywhere else it works in its own folder.
    let (dir, assets) = match mac_app_contents(&agent) {
        Some(contents) => {
            let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)?;
            let dir = mac_data_dir(&home);
            if fs::create_dir_all(&dir).is_err() {
                return None;
            }
            (dir, Some(contents.join("Resources").join("assets")))
        }
        None => (agent.parent()?.to_path_buf(), None),
    };

    let text = fs::read(dir.join(".env")).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let parsed = parse_env(&text);
    let mut env = child_env(&parsed, |k| std::env::var_os(k).is_some());
    let get = |key: &str| setting(key, |k| std::env::var(k).ok(), &parsed);
    if let Some(assets) = assets.filter(|a| a.is_dir()) {
        if get("ASSETS_DIR").is_none() {
            env.push(("ASSETS_DIR".to_string(), assets.to_string_lossy().into_owned()));
        }
    }
    let port = port_from(get("AGENT_PORT").as_deref());
    let host = probe_host(get("AGENT_HOST").as_deref());
    Some(Local { agent, dir, base: local_base(&host, port), env })
}

/// The Bageo process this program started, and how that is going.
pub struct Supervisor {
    /// True once this program has started Bageo itself.
    pub managed: bool,
    /// Bageo has answered since it was last started.
    pub connected: bool,
    /// Bageo kept stopping: it is not started again.
    pub gave_up: bool,
    child: Option<Child>,
    launched_at: Instant,
    restart_at: Option<Instant>,
    restarts: Restarts,
    epoch: Instant,
    quitting: bool,
}

impl Supervisor {
    pub fn new() -> Self {
        let now = Instant::now();
        Supervisor {
            managed: false,
            connected: false,
            gave_up: false,
            child: None,
            launched_at: now,
            restart_at: None,
            restarts: Restarts::default(),
            epoch: now,
            quitting: false,
        }
    }

    pub fn since_launch(&self) -> Duration {
        self.launched_at.elapsed()
    }

    /// Started and still inside the time it may take to answer.
    pub fn launching(&self) -> bool {
        self.managed && !self.connected && !self.gave_up && self.since_launch() < LAUNCH_GRACE
    }
}

/// Opens the log for appending. A log over 5 MB is first moved to bageo.log.1.
fn open_log(dir: &Path) -> io::Result<File> {
    let path = dir.join(LOG_NAME);
    if fs::metadata(&path).map(|m| m.len() > LOG_MAX_BYTES).unwrap_or(false) {
        let old = dir.join(LOG_OLD_NAME);
        let _ = fs::remove_file(&old);
        let _ = fs::rename(&path, &old);
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    // Bageo's output names callers: on shared computers only the owner reads it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn spawn_child(local: &Local) -> io::Result<Child> {
    let mut cmd = Command::new(&local.agent);
    cmd.current_dir(&local.dir).envs(local.env.iter().map(|(k, v)| (k, v))).stdin(Stdio::null());
    // Bageo finds platform-tools in its working directory by itself, so ADB_BIN is not set here.
    match open_log(&local.dir).and_then(|out| Ok((out.try_clone()?, out))) {
        Ok((out, err)) => {
            cmd.stdout(out).stderr(err);
        }
        Err(_) => {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: Bageo is a console program; no window may show up.
        cmd.creation_flags(0x0800_0000);
    }
    cmd.spawn()
}

fn on_stopped(sup: &mut Supervisor) {
    sup.child = None;
    sup.connected = false;
    if sup.restarts.on_exit(sup.epoch.elapsed()) {
        // The time it may take to answer counts from now.
        sup.launched_at = Instant::now();
        sup.restart_at = Some(Instant::now() + RESTART_DELAY);
    } else {
        sup.gave_up = true;
        sup.restart_at = None;
    }
}

fn launch(sup: &mut Supervisor, local: &Local) {
    sup.restart_at = None;
    sup.connected = false;
    sup.launched_at = Instant::now();
    match spawn_child(local) {
        Ok(child) => sup.child = Some(child),
        Err(_) => on_stopped(sup),
    }
}

/// Asks the process to stop, waits a little, then stops it the hard way.
fn terminate(child: &mut Child) {
    #[cfg(unix)]
    {
        let _ = Command::new("/bin/kill")
            .arg("-TERM")
            .arg(child.id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    let deadline = Instant::now() + STOP_WAIT;
    while Instant::now() < deadline {
        if !matches!(child.try_wait(), Ok(None)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

async fn already_running(base: &str) -> bool {
    let Ok(client) = reqwest::Client::builder().no_proxy().timeout(FIRST_CHECK_TIMEOUT).build() else {
        return false;
    };
    let Ok(resp) = client.get(format!("{base}health")).send().await else {
        return false;
    };
    if resp.status().as_u16() != 200 {
        return false;
    }
    resp.json::<Value>().await.ok().and_then(|b| b.get("ok").and_then(Value::as_bool)) == Some(true)
}

/// One look at the process. False when watching should end.
fn tick(app: &AppHandle) -> bool {
    let shared = app.state::<Shared>();
    let Some(local) = shared.local.as_ref() else { return false };
    let mut changed = false;
    {
        let mut sup = lock(&shared.sup);
        if sup.quitting {
            return false;
        }
        let stopped = match sup.child.as_mut() {
            Some(child) => !matches!(child.try_wait(), Ok(None)),
            None => false,
        };
        if stopped {
            on_stopped(&mut sup);
            changed = true;
        }
        let due = sup.restart_at.is_some_and(|t| Instant::now() >= t);
        if sup.child.is_none() && !sup.gave_up && due {
            launch(&mut sup, local);
            changed = true;
        }
    }
    if changed {
        shared.notify.notify_one();
    }
    true
}

/// When Bageo sits next to this program and is not running yet: starts it and keeps it running.
pub async fn start(app: &AppHandle) {
    let shared = app.state::<Shared>();
    let Some(local) = shared.local.as_ref() else { return };
    if already_running(&local.base).await {
        return;
    }
    {
        let mut sup = lock(&shared.sup);
        sup.managed = true;
        launch(&mut sup, local);
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(WATCH_EVERY).await;
            if !tick(&app) {
                break;
            }
        }
    });
}

/// Stops Bageo and starts it again (after adb was installed). May block for a few seconds.
fn restart(app: &AppHandle) {
    let shared = app.state::<Shared>();
    let Some(local) = shared.local.as_ref() else { return };
    {
        let mut sup = lock(&shared.sup);
        if !sup.managed || sup.quitting {
            return;
        }
        if let Some(mut child) = sup.child.take() {
            terminate(&mut child);
        }
        sup.gave_up = false;
        sup.restarts.clear();
        launch(&mut sup, local);
    }
    shared.notify.notify_one();
}

/// Stops the Bageo this program started, waiting up to 3 seconds. Safe to call twice.
pub fn stop(app: &AppHandle) {
    let Some(shared) = app.try_state::<Shared>() else { return };
    let mut sup = lock(&shared.sup);
    sup.quitting = true;
    if let Some(mut child) = sup.child.take() {
        terminate(&mut child);
    }
}

async fn download_adb(dir: PathBuf) -> Result<(), String> {
    let platform = Platform::current();
    // Google may hand the download to another of its hosts: follow, but never to plain http.
    let https_only = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.url().scheme() != "https" || attempt.previous().len() > 5 {
            attempt.stop()
        } else {
            attempt.follow()
        }
    });
    let client = reqwest::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .redirect(https_only)
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(platform.adb_url()).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("download answered {}", resp.status().as_u16()));
    }
    if resp.url().scheme() != "https" {
        return Err("the download left https".to_string());
    }
    if resp.content_length().is_some_and(|n| n > DOWNLOAD_MAX_BYTES) {
        return Err("the download is larger than expected".to_string());
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    if bytes.len() as u64 > DOWNLOAD_MAX_BYTES {
        return Err("the download is larger than expected".to_string());
    }
    let tmp = std::env::temp_dir().join(format!("bageo-platform-tools-{}.zip", std::process::id()));
    tauri::async_runtime::spawn_blocking(move || {
        let unpack = || -> Result<(), String> {
            fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
            let file = File::open(&tmp).map_err(|e| e.to_string())?;
            extract_zip(file, &dir)?;
            if dir.join("platform-tools").join(platform.adb_name()).is_file() {
                Ok(())
            } else {
                Err("the download did not contain adb".to_string())
            }
        };
        let result = unpack();
        let _ = fs::remove_file(&tmp);
        result
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Downloads Google's platform-tools into the program folder. One download at a time.
pub async fn install_adb(app: AppHandle) {
    let shared = app.state::<Shared>();
    let Some(local) = shared.local.as_ref() else { return };
    {
        let mut job = lock(&shared.adb_job);
        if *job == AdbJob::Downloading {
            return;
        }
        *job = AdbJob::Downloading;
    }
    poll::patch(&app, |s| s.adb = "downloading".to_string());

    let ok = download_adb(local.dir.clone()).await.is_ok();
    let managed = lock(&shared.sup).managed;
    if ok && managed {
        let handle = app.clone();
        let _ = tauri::async_runtime::spawn_blocking(move || restart(&handle)).await;
    }
    *lock(&shared.adb_job) = if ok { AdbJob::Idle } else { AdbJob::Failed };
    poll::patch(&app, |s| s.adb = if ok { String::new() } else { "failed".to_string() });
    shared.notify.notify_one();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn env_file_is_read_like_the_start_scripts() {
        let text = "\u{feff}# settings\n\nAGENT_PORT=9000\n  GEMINI_API_KEY = \"abc def\"  \nNAME='single'\nno equals sign\n#OFF=1\nEMPTY=\nURL=http://192.168.1.20:8787/?a=b\nAGENT_PORT=1\nbad key=1\n=novalue\nQ=\"it's\"\r\n";
        assert_eq!(
            parse_env(text),
            pairs(&[
                ("AGENT_PORT", "9000"),
                ("GEMINI_API_KEY", "abc def"),
                ("NAME", "single"),
                ("EMPTY", ""),
                ("URL", "http://192.168.1.20:8787/?a=b"),
                ("Q", "it's"),
            ])
        );
    }

    #[test]
    fn only_one_pair_of_quotes_is_removed() {
        assert_eq!(parse_env("A=\"\"x\"\"\nB=\"\nC='x\""), pairs(&[("A", "\"x\""), ("B", "\""), ("C", "'x\"")]));
    }

    #[test]
    fn real_environment_wins_over_env_file() {
        let parsed = pairs(&[("AGENT_PORT", "9000"), ("USB_BRIDGE", "off")]);
        let env = child_env(&parsed, |k| k == "AGENT_PORT");
        assert_eq!(env, pairs(&[("USB_BRIDGE", "off")]));

        let real = |k: &str| (k == "AGENT_PORT").then(|| "7000".to_string());
        assert_eq!(setting("AGENT_PORT", real, &parsed).as_deref(), Some("7000"));
        assert_eq!(setting("USB_BRIDGE", real, &parsed).as_deref(), Some("off"));
        assert_eq!(setting("AGENT_HOST", real, &parsed), None);
    }

    #[test]
    fn windows_layout_looks_next_to_the_executable() {
        let dir = Path::new("bageo-call");
        let c = agent_candidates(dir, None, "", Platform::Windows);
        assert_eq!(c, vec![dir.join("agent.exe")]);
        assert_eq!(find_agent(&c, |p| p == dir.join("agent.exe")), Some(dir.join("agent.exe")));
        assert_eq!(find_agent(&c, |_| false), None);
    }

    #[test]
    fn mac_layout_looks_next_to_the_app_then_inside_it() {
        let exe_dir = Path::new("/path/to/bageo-call/Bageo Status.app/Contents/MacOS");
        let c = agent_candidates(exe_dir, None, "", Platform::Mac);
        assert_eq!(c, vec![PathBuf::from("/path/to/bageo-call/agent"), exe_dir.join("agent")]);
        // Next to the .app wins when both exist.
        assert_eq!(find_agent(&c, |_| true), Some(PathBuf::from("/path/to/bageo-call/agent")));
        let inside = exe_dir.join("agent");
        assert_eq!(find_agent(&c, |p| p == inside), Some(inside.clone()));
        assert_eq!(find_agent(&c, |_| false), None);
        // A folder too shallow to be inside an .app only has the last candidate.
        assert_eq!(agent_candidates(Path::new("bin"), None, "", Platform::Mac), vec![Path::new("bin").join("agent")]);
    }

    #[test]
    fn bageo_inside_a_mac_app_is_told_apart() {
        let agent = Path::new("/Applications/Bageo.app/Contents/MacOS/agent");
        assert_eq!(mac_app_contents(agent), Some(PathBuf::from("/Applications/Bageo.app/Contents")));
        // Next to an app, in a plain folder or too shallow: not inside an app.
        assert_eq!(mac_app_contents(Path::new("/path/to/bageo-call/agent")), None);
        assert_eq!(mac_app_contents(Path::new("/path/MacOS/agent")), None);
        assert_eq!(mac_app_contents(Path::new("/path/Folder/Contents/MacOS/agent")), None);
        assert_eq!(mac_app_contents(Path::new("agent")), None);
        assert_eq!(
            mac_data_dir(Path::new("/Users/someone")),
            PathBuf::from("/Users/someone/Library/Application Support/Bageo")
        );
    }

    #[test]
    fn override_path_comes_first_and_falls_through_when_missing() {
        let dir = Path::new("bageo-call");
        let c = agent_candidates(dir, Some(" /path/to/agent "), "", Platform::Windows);
        assert_eq!(c, vec![PathBuf::from("/path/to/agent"), dir.join("agent.exe")]);
        assert_eq!(find_agent(&c, |_| true), Some(PathBuf::from("/path/to/agent")));
        assert_eq!(find_agent(&c, |p| p != Path::new("/path/to/agent")), Some(dir.join("agent.exe")));
        assert_eq!(agent_candidates(dir, Some("  "), "", Platform::Windows).len(), 1);
        // The place it was found last time is tried after the usual ones.
        let c = agent_candidates(dir, None, " /path/to/old/agent ", Platform::Windows);
        assert_eq!(c.len(), 2);
        assert_eq!(c[1], PathBuf::from("/path/to/old/agent"));
        assert_eq!(find_agent(&c, |p| p == Path::new("/path/to/old/agent")), Some(PathBuf::from("/path/to/old/agent")));
    }

    #[test]
    fn port_and_first_candidate() {
        assert_eq!(port_from(None), 8787);
        assert_eq!(port_from(Some("")), 8787);
        assert_eq!(port_from(Some(" 9000 ")), 9000);
        assert_eq!(port_from(Some("abc")), 8787);
        assert_eq!(port_from(Some("0")), 8787);
        assert_eq!(port_from(Some("70000")), 8787);

        assert_eq!(probe_host(None), "127.0.0.1");
        assert_eq!(probe_host(Some("0.0.0.0")), "127.0.0.1");
        assert_eq!(probe_host(Some("::")), "127.0.0.1");
        assert_eq!(probe_host(Some("192.168.1.20")), "192.168.1.20");
        assert_eq!(probe_host(Some("fe80::1")), "[fe80::1]");
        assert_eq!(local_base(&probe_host(None), port_from(Some("9000"))), "http://127.0.0.1:9000/");

        let plain = discovery_candidates(None, false);
        assert_eq!(plain, vec!["http://127.0.0.1:8787/", "http://bageo.local/", "http://bageo.local:8787/"]);
        let local = discovery_candidates(Some("http://127.0.0.1:9000/"), false);
        assert_eq!(local, vec!["http://127.0.0.1:9000/", "http://bageo.local/", "http://bageo.local:8787/"]);
        // While this program runs Bageo, another Bageo on the network must not stand in for it.
        assert_eq!(discovery_candidates(Some("http://127.0.0.1:9000/"), true), vec!["http://127.0.0.1:9000/"]);
    }

    #[test]
    fn fourth_stop_within_five_minutes_ends_restarting() {
        let s = Duration::from_secs;
        let mut r = Restarts::default();
        assert!(r.on_exit(s(0)));
        assert!(r.on_exit(s(60)));
        assert!(r.on_exit(s(120)));
        assert!(!r.on_exit(s(180)));

        // Spread out, the same number of stops keeps restarting.
        let mut r = Restarts::default();
        assert!(r.on_exit(s(0)));
        assert!(r.on_exit(s(100)));
        assert!(r.on_exit(s(200)));
        assert!(r.on_exit(s(301)));
        assert!(!r.on_exit(s(302)));

        r.clear();
        assert!(r.on_exit(s(303)));
    }

    #[test]
    fn launching_then_launch_failed() {
        let s = Duration::from_secs;
        // Not started by this program: the normal handling applies.
        assert_eq!(launch_reason(false, false, false, s(1)), None);
        assert_eq!(launch_reason(false, true, false, s(100)), None);
        // Started, no answer yet.
        assert_eq!(launch_reason(true, false, false, s(0)), Some((Level::Unknown, Reason::Launching)));
        assert_eq!(launch_reason(true, false, false, s(29)), Some((Level::Unknown, Reason::Launching)));
        assert_eq!(launch_reason(true, false, false, s(30)), Some((Level::Bad, Reason::LaunchFailed)));
        // It answered before: a missed answer is handled the normal way.
        assert_eq!(launch_reason(true, false, true, s(500)), None);
        // Giving up wins over everything else.
        assert_eq!(launch_reason(true, true, false, s(1)), Some((Level::Bad, Reason::LaunchFailed)));
        assert_eq!(launch_reason(true, true, true, s(1)), Some((Level::Bad, Reason::LaunchFailed)));
    }

    #[test]
    fn adb_field_values() {
        assert_eq!(adb_state(true, true, false, AdbJob::Idle), "missing");
        assert_eq!(adb_state(true, true, false, AdbJob::Failed), "failed");
        assert_eq!(adb_state(true, true, false, AdbJob::Downloading), "downloading");
        assert_eq!(adb_state(true, true, true, AdbJob::Downloading), "downloading");
        assert_eq!(adb_state(true, true, true, AdbJob::Idle), "");
        assert_eq!(adb_state(true, true, true, AdbJob::Failed), "");
        // USB link turned off: adb is not needed.
        assert_eq!(adb_state(true, false, false, AdbJob::Idle), "");
        // Only watching another Bageo: never.
        for job in [AdbJob::Idle, AdbJob::Downloading, AdbJob::Failed] {
            assert_eq!(adb_state(false, true, false, job), "");
        }
    }

    #[test]
    fn download_addresses_per_system() {
        assert!(Platform::Windows.adb_url().ends_with("platform-tools-latest-windows.zip"));
        assert!(Platform::Mac.adb_url().ends_with("platform-tools-latest-darwin.zip"));
        assert_eq!(Platform::Windows.adb_name(), "adb.exe");
        assert_eq!(Platform::Mac.adb_name(), "adb");
    }

    fn zip_with(entries: &[(&str, &[u8], u32)]) -> Cursor<Vec<u8>> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data, mode) in entries {
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(*mode);
            w.start_file(*name, opts).unwrap();
            w.write_all(data).unwrap();
        }
        let mut cursor = w.finish().unwrap();
        cursor.set_position(0);
        cursor
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bageo-status-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn zip_is_unpacked_into_the_folder() {
        let root = scratch("unpack");
        let dest = root.join("dest");
        fs::create_dir_all(&dest).unwrap();
        let zip = zip_with(&[("platform-tools/adb", b"#!/bin/sh\n", 0o755), ("platform-tools/lib/notes.txt", b"hello", 0o644)]);
        extract_zip(zip, &dest).unwrap();
        assert_eq!(fs::read(dest.join("platform-tools/adb")).unwrap(), b"#!/bin/sh\n");
        assert_eq!(fs::read(dest.join("platform-tools/lib/notes.txt")).unwrap(), b"hello");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(dest.join("platform-tools/adb")).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111);
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn zip_entry_that_leaves_the_folder_is_refused_and_nothing_is_written() {
        let root = scratch("escape");
        let dest = root.join("dest");
        fs::create_dir_all(&dest).unwrap();
        let zip = zip_with(&[("platform-tools/adb", b"ok", 0o755), ("../outside.txt", b"no", 0o644)]);
        assert!(extract_zip(zip, &dest).is_err());
        assert!(!root.join("outside.txt").exists());
        assert!(!dest.join("platform-tools").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn zip_entry_next_to_platform_tools_is_refused_and_nothing_is_written() {
        let root = scratch("beside");
        let dest = root.join("dest");
        fs::create_dir_all(&dest).unwrap();
        for name in ["agent.exe", ".env", "platform-tools-old/adb"] {
            let zip = zip_with(&[("platform-tools/adb", b"ok", 0o755), (name, b"no", 0o644)]);
            assert!(extract_zip(zip, &dest).is_err(), "{name}");
            assert!(!dest.join(name).exists(), "{name}");
            assert!(!dest.join("platform-tools").exists(), "{name}");
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn broken_zip_is_an_error() {
        let root = scratch("broken");
        assert!(extract_zip(Cursor::new(b"not a zip".to_vec()), &root).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn large_log_is_moved_aside_before_appending() {
        let root = scratch("log");
        fs::write(root.join(LOG_NAME), b"small").unwrap();
        drop(open_log(&root).unwrap());
        assert!(!root.join(LOG_OLD_NAME).exists());

        fs::write(root.join(LOG_OLD_NAME), b"older").unwrap();
        let big = File::create(root.join(LOG_NAME)).unwrap();
        big.set_len(LOG_MAX_BYTES + 1).unwrap();
        drop(big);
        let mut log = open_log(&root).unwrap();
        log.write_all(b"new").unwrap();
        drop(log);
        assert_eq!(fs::metadata(root.join(LOG_OLD_NAME)).unwrap().len(), LOG_MAX_BYTES + 1);
        assert_eq!(fs::read(root.join(LOG_NAME)).unwrap(), b"new");
        let _ = fs::remove_dir_all(&root);
    }
}
