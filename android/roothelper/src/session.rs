// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! One call is one session. Before the helper changes a mixer control or an audio
//! parameter for the first time in a session, it saves the old value it read from the
//! phone to a state file (written and flushed before the change). Ending the session
//! puts every saved value back, newest first. That happens when the app ends the
//! session, when the connection breaks or runs out of time, when the helper is told to
//! stop, and otherwise (the helper was killed, the phone restarted) the next time the
//! helper starts or the phone boots.
//!
//! A value that could not be put back stays in the file and is tried again at the next
//! start or boot, up to [`MAX_ATTEMPTS`] times; after that the file stays and nothing
//! retries it until `restore --force`. While the file exists no new session starts.
//! The file is only read when it is a plain file owned by this user (root), not
//! readable or writable by anyone else, and not larger than [`MAX_STATE_BYTES`].
//!
//! State file (`/data/adb/pixelcallai/audio_session`):
//!
//! ```text
//! bageo-audio-session 2
//! origin app|root
//! mixer <tries> <card> <card ID, hex> <iface> <device> <subdevice> <index> <name, hex> <v1,v2,...>
//! param <tries> <key> =<value>
//! ```
//!
//! `origin app`: the app's session; only values of controls and parameters on the
//! helper's built-in list are put back. `origin root`: a session run by root from the
//! command line (`mixer try`).
//!
//! (Putting back values read before the change, instead of writing fixed values as
//! gsm2sip does in its restore commands, is this helper's own choice.)

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::json::Json;
use crate::mixer::{CtlKey, MAX_CARD};
use crate::params;

pub const STATE_HEADER: &str = "bageo-audio-session 2";
pub const MAX_SAVED: usize = 512;
pub const MAX_STATE_BYTES: usize = 128 * 1024;
pub const MAX_ATTEMPTS: u32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    App,
    Root,
}

impl Origin {
    fn name(self) -> &'static str {
        match self {
            Origin::App => "app",
            Origin::Root => "root",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Saved {
    Mixer { key: CtlKey, values: Vec<i64> },
    Param { key: String, value: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub item: Saved,
    pub attempts: u32,
}

/// Why a value was not put back (fixed names, for the log and the reply).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// Not on the built-in list (an app session) — kept, never written.
    NotAllowed,
    /// The control or card is not there.
    Gone,
    /// The control is there but its type, size or range no longer fits the value.
    Changed,
    /// The device or the parameter tool refused or failed.
    Device,
}

impl Failure {
    pub fn name(self) -> &'static str {
        match self {
            Failure::NotAllowed => "not on the list",
            Failure::Gone => "control gone",
            Failure::Changed => "control changed",
            Failure::Device => "device error",
        }
    }
}

/// Writes saved values back to the phone.
pub trait Restorer {
    fn write_ctl(&mut self, key: &CtlKey, values: &[i64], origin: Origin) -> Result<(), Failure>;
    fn set_param(&mut self, key: &str, value: &str, origin: Origin) -> Result<(), Failure>;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub restored: usize,
    /// Failed this time; kept for another try.
    pub failed: usize,
    /// Not on the list; kept, not written.
    pub not_allowed: usize,
    /// Out of tries; kept, not written.
    pub given_up: usize,
    /// The state file could not be trusted or read; kept as it is.
    pub unreadable: bool,
    /// The kinds of failure seen (fixed names).
    pub kinds: Vec<&'static str>,
    /// Values still in the state file afterwards.
    pub left: usize,
}

impl Report {
    pub fn json(&self) -> Json {
        Json::obj()
            .with("restored", Json::Num(self.restored as i64))
            .with("failed", Json::Num(self.failed as i64))
            .with("not_allowed", Json::Num(self.not_allowed as i64))
            .with("given_up", Json::Num(self.given_up as i64))
            .with("unreadable", Json::Bool(self.unreadable))
            .with("left", Json::Num(self.left as i64))
            .with("kinds", Json::Arr(self.kinds.iter().map(|k| Json::str(*k)).collect()))
    }

    pub fn clean(&self) -> bool {
        self.left == 0 && !self.unreadable
    }

    fn kind(&mut self, f: Failure) {
        if !self.kinds.contains(&f.name()) {
            self.kinds.push(f.name());
        }
    }
}

pub fn valid_session_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// The user that must own the state file: the helper's own (root on a phone).
fn me() -> u32 {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() }
}

pub struct Session {
    path: PathBuf,
    origin: Origin,
    open: bool,
    started: Option<Instant>,
    entries: Vec<Entry>,
}

impl Session {
    pub fn new(path: impl Into<PathBuf>, origin: Origin) -> Session {
        Session { path: path.into(), origin, open: false, started: None, entries: Vec::new() }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn age(&self) -> Option<Duration> {
        self.started.map(|s| s.elapsed())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn begin(&mut self, id: &str) -> Result<(), String> {
        if self.open {
            return Err("a session is already open".into());
        }
        if !valid_session_id(id) {
            return Err("bad session id".into());
        }
        if fs::symlink_metadata(&self.path).is_ok() {
            return Err("audio settings of an earlier call are not put back yet".into());
        }
        write_state(&self.path, self.origin, &[]).map_err(|e| format!("cannot save the session: {}", e.kind()))?;
        self.open = true;
        self.started = Some(Instant::now());
        self.entries.clear();
        Ok(())
    }

    fn already_saved(&self, item: &Saved) -> bool {
        self.entries.iter().any(|e| match (&e.item, item) {
            (Saved::Mixer { key: a, .. }, Saved::Mixer { key: b, .. }) => a == b,
            (Saved::Param { key: a, .. }, Saved::Param { key: b, .. }) => a == b,
            _ => false,
        })
    }

    /// Saves an old value before its first change in this session (a later change of
    /// the same control keeps the first value), on disk before it returns.
    pub fn remember(&mut self, item: Saved) -> Result<(), String> {
        if !self.open {
            return Err("no session: send session.begin first".into());
        }
        if self.already_saved(&item) {
            return Ok(());
        }
        if self.entries.len() >= MAX_SAVED {
            return Err("this session changed too many values".into());
        }
        self.entries.push(Entry { item, attempts: 0 });
        if let Err(e) = write_state(&self.path, self.origin, &self.entries) {
            self.entries.pop();
            return Err(format!("cannot save the old value: {}", e.kind()));
        }
        Ok(())
    }

    /// Ends the session: puts every saved value back; what failed stays in the state
    /// file for the next start. `None` when no session was open.
    pub fn end<R: Restorer>(&mut self, r: &mut R) -> Option<Report> {
        if !self.open {
            return None;
        }
        let entries = std::mem::take(&mut self.entries);
        let (mut report, kept) = restore_entries(entries, self.origin, r, false);
        finish(&self.path, self.origin, &kept, &mut report);
        self.open = false;
        self.started = None;
        Some(report)
    }
}

fn restore_entries<R: Restorer>(entries: Vec<Entry>, origin: Origin, r: &mut R, force: bool) -> (Report, Vec<Entry>) {
    let mut report = Report::default();
    let mut kept = Vec::new();
    for mut e in entries.into_iter().rev() {
        if e.attempts >= MAX_ATTEMPTS && !force {
            report.given_up += 1;
            kept.push(e);
            continue;
        }
        let result = match &e.item {
            Saved::Mixer { key, values } => r.write_ctl(key, values, origin),
            Saved::Param { key, value } => r.set_param(key, value, origin),
        };
        match result {
            Ok(()) => report.restored += 1,
            Err(Failure::NotAllowed) => {
                report.not_allowed += 1;
                report.kind(Failure::NotAllowed);
                kept.push(e);
            }
            Err(f) => {
                report.failed += 1;
                report.kind(f);
                e.attempts = e.attempts.saturating_add(1);
                kept.push(e);
            }
        }
    }
    kept.reverse();
    (report, kept)
}

/// Deletes the state file when nothing is left, else rewrites it with what is left.
fn finish(path: &Path, origin: Origin, kept: &[Entry], report: &mut Report) {
    report.left = kept.len();
    if kept.is_empty() {
        let _ = fs::remove_file(path);
    } else if write_state(path, origin, kept).is_err() {
        // The old file (a superset of what is left) stays.
        report.kind(Failure::Device);
    }
}

/// Reads the state file if it can be trusted.
fn read_state(path: &Path) -> Result<Option<String>, ()> {
    use std::os::unix::fs::MetadataExt;
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    if !meta.file_type().is_file() || meta.uid() != me() || meta.mode() & 0o077 != 0 || meta.len() > MAX_STATE_BYTES as u64
    {
        return Err(());
    }
    let bytes = fs::read(path).map_err(|_| ())?;
    if bytes.len() > MAX_STATE_BYTES {
        return Err(());
    }
    String::from_utf8(bytes).map(Some).map_err(|_| ())
}

/// Puts back a session nobody ended (a state file still on disk). `None`: no state
/// file. `force`: also try values that ran out of tries.
pub fn restore_leftover<R: Restorer>(path: &Path, r: &mut R, force: bool) -> Option<Report> {
    let text = match read_state(path) {
        Ok(None) => return None,
        Ok(Some(t)) => t,
        Err(()) => return Some(Report { unreadable: true, left: 1, ..Report::default() }),
    };
    let Some((origin, entries, bad)) = decode_state(&text) else {
        return Some(Report { unreadable: true, left: 1, ..Report::default() });
    };
    let (mut report, kept) = restore_entries(entries, origin, r, force);
    if bad > 0 {
        // Lines it cannot read cannot be put back or dropped: the file stays as it is.
        report.unreadable = true;
        report.left = kept.len() + bad;
    } else {
        finish(path, origin, &kept, &mut report);
    }
    Some(report)
}

fn hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<String> {
    if s.is_empty() || !s.len().is_multiple_of(2) || s.len() > 2 * 64 {
        return None;
    }
    let bytes: Option<Vec<u8>> =
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect();
    String::from_utf8(bytes?).ok()
}

pub fn encode_state(origin: Origin, entries: &[Entry]) -> String {
    let mut out = format!("{STATE_HEADER}\norigin {}\n", origin.name());
    for e in entries {
        match &e.item {
            Saved::Mixer { key, values } => {
                let vals: Vec<String> = values.iter().map(|v| v.to_string()).collect();
                out.push_str(&format!(
                    "mixer {} {} {} {} {} {} {} {} {}\n",
                    e.attempts,
                    key.card,
                    hex(&key.card_id),
                    key.iface,
                    key.device,
                    key.subdevice,
                    key.index,
                    hex(&key.name),
                    vals.join(",")
                ));
            }
            Saved::Param { key, value } => out.push_str(&format!("param {} {key} ={value}\n", e.attempts)),
        }
    }
    out
}

fn decode_line(line: &str) -> Option<Entry> {
    let f: Vec<&str> = line.split(' ').collect();
    let attempts = f.get(1)?.parse::<u32>().ok()?;
    let item = match f[0] {
        "mixer" if f.len() == 10 => {
            let card = f[2].parse::<u32>().ok().filter(|&c| c <= MAX_CARD)?;
            let values: Option<Vec<i64>> = f[9].split(',').map(|v| v.parse::<i64>().ok()).collect();
            let values = values.filter(|v| !v.is_empty() && v.len() <= 128)?;
            Saved::Mixer {
                key: CtlKey {
                    card,
                    card_id: unhex(f[3])?,
                    iface: f[4].parse().ok()?,
                    device: f[5].parse().ok()?,
                    subdevice: f[6].parse().ok()?,
                    index: f[7].parse().ok()?,
                    name: unhex(f[8])?,
                },
                values,
            }
        }
        "param" if f.len() == 4 => {
            let value = f[3].strip_prefix('=')?;
            if !params::valid_key(f[2]) || !params::valid_value(value) {
                return None;
            }
            Saved::Param { key: f[2].to_string(), value: value.to_string() }
        }
        _ => return None,
    };
    Some(Entry { item, attempts })
}

/// The origin, the saved values and the number of lines that could not be read.
pub fn decode_state(text: &str) -> Option<(Origin, Vec<Entry>, usize)> {
    let mut lines = text.lines();
    if lines.next() != Some(STATE_HEADER) {
        return None;
    }
    let origin = match lines.next() {
        Some("origin app") => Origin::App,
        Some("origin root") => Origin::Root,
        _ => return None,
    };
    let mut entries = Vec::new();
    let mut bad = 0;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        match decode_line(line) {
            Some(e) if entries.len() < MAX_SAVED => entries.push(e),
            _ => bad += 1,
        }
    }
    Some((origin, entries, bad))
}

/// Writes the state file: a temporary file next to it, flushed, then renamed over it,
/// then the folder flushed, so a crash leaves the old file or the new one, whole.
fn write_state(path: &Path, origin: Origin, entries: &[Entry]) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let text = encode_state(origin, entries);
    if text.len() > MAX_STATE_BYTES {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let dir = path.parent().ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    if !dir.is_dir() {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    let tmp = path.with_extension("tmp");
    match fs::symlink_metadata(&tmp) {
        Ok(m) if m.file_type().is_file() || m.file_type().is_symlink() => fs::remove_file(&tmp)?,
        Ok(_) => return Err(io::Error::from(io::ErrorKind::AlreadyExists)),
        Err(_) => {}
    }
    {
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    fs::File::open(dir)?.sync_all()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;

    /// A phone: control values by name, parameters by key, and a log of every write.
    #[derive(Default)]
    pub struct FakePhone {
        pub ctls: HashMap<String, Vec<i64>>,
        pub params: HashMap<String, String>,
        pub writes: Vec<String>,
        pub fail_ctl: Option<String>,
        pub not_allowed: Option<String>,
    }

    impl Restorer for FakePhone {
        fn write_ctl(&mut self, key: &CtlKey, values: &[i64], _origin: Origin) -> Result<(), Failure> {
            if self.fail_ctl.as_deref() == Some(key.name.as_str()) {
                return Err(Failure::Gone);
            }
            if self.not_allowed.as_deref() == Some(key.name.as_str()) {
                return Err(Failure::NotAllowed);
            }
            self.writes.push(format!("ctl {}={values:?}", key.name));
            self.ctls.insert(key.name.clone(), values.to_vec());
            Ok(())
        }
        fn set_param(&mut self, key: &str, value: &str, _origin: Origin) -> Result<(), Failure> {
            self.writes.push(format!("param {key}={value}"));
            self.params.insert(key.to_string(), value.to_string());
            Ok(())
        }
    }

    pub fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bageo-helper-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    pub fn key(name: &str) -> CtlKey {
        CtlKey { card: 0, card_id: "testcard".into(), iface: 2, device: 0, subdevice: 0, index: 0, name: name.into() }
    }

    fn e(item: Saved) -> Entry {
        Entry { item, attempts: 0 }
    }

    #[test]
    fn state_file_round_trip() {
        let entries = vec![
            e(Saved::Mixer { key: key("Voice Rx Device Mute"), values: vec![0, -1, 20] }),
            Entry {
                item: Saved::Mixer {
                    key: CtlKey {
                        card: 1,
                        card_id: "aoc".into(),
                        iface: 3,
                        device: 2,
                        subdevice: 1,
                        index: 4,
                        name: "A,B=C".into(),
                    },
                    values: vec![7],
                },
                attempts: 3,
            },
            e(Saved::Param { key: "incall_music_enabled".into(), value: "false".into() }),
            e(Saved::Param { key: "Set_BGS_UL_Mute".into(), value: String::new() }),
        ];
        let text = encode_state(Origin::App, &entries);
        let (origin, back, bad) = decode_state(&text).unwrap();
        assert_eq!((origin, bad), (Origin::App, 0));
        assert_eq!(back, entries);
        assert!(decode_state("something else\n").is_none());
        assert!(decode_state(&format!("{STATE_HEADER}\norigin nobody\n")).is_none());
    }

    #[test]
    fn normal_end_puts_everything_back_newest_first() {
        let dir = temp_dir("end");
        let path = dir.join("audio_session");
        let mut s = Session::new(&path, Origin::App);
        assert!(s.remember(Saved::Param { key: "k".into(), value: "v".into() }).is_err(), "no session yet");
        s.begin("call-1").unwrap();
        assert!(s.begin("call-2").is_err());
        s.remember(Saved::Mixer { key: key("Incall Mic Mute"), values: vec![0] }).unwrap();
        s.remember(Saved::Param { key: "incall_music_enabled".into(), value: "false".into() }).unwrap();
        s.remember(Saved::Mixer { key: key("Incall Mic Mute"), values: vec![1] }).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(decode_state(&fs::read_to_string(&path).unwrap()).unwrap().1.len(), 2);

        let mut phone = FakePhone::default();
        let report = s.end(&mut phone).unwrap();
        assert_eq!((report.restored, report.left), (2, 0));
        assert_eq!(phone.writes, vec!["param incall_music_enabled=false", "ctl Incall Mic Mute=[0]"]);
        assert!(!path.exists());
        assert!(s.end(&mut phone).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn failed_values_stay_and_block_new_sessions() {
        let dir = temp_dir("failed");
        let path = dir.join("audio_session");
        let mut s = Session::new(&path, Origin::App);
        s.begin("call-1").unwrap();
        s.remember(Saved::Mixer { key: key("Incall Sink Mute"), values: vec![0] }).unwrap();
        s.remember(Saved::Mixer { key: key("Broken"), values: vec![3] }).unwrap();
        let mut phone = FakePhone { fail_ctl: Some("Broken".into()), ..Default::default() };
        let report = s.end(&mut phone).unwrap();
        assert_eq!((report.restored, report.failed, report.left), (1, 1, 1));
        assert_eq!(report.kinds, vec!["control gone"]);
        assert!(path.exists(), "the failed value is kept");
        assert!(s.begin("call-2").is_err(), "no new session while something is not put back");

        // Every later start tries again, up to MAX_ATTEMPTS times; then it gives up but keeps the file.
        for _ in 1..MAX_ATTEMPTS {
            let r = restore_leftover(&path, &mut phone, false).unwrap();
            assert_eq!((r.failed, r.left), (1, 1));
        }
        let r = restore_leftover(&path, &mut phone, false).unwrap();
        assert_eq!((r.given_up, r.failed, r.left), (1, 0, 1));
        assert!(path.exists());
        phone.fail_ctl = None;
        let r = restore_leftover(&path, &mut phone, true).unwrap();
        assert_eq!((r.restored, r.left), (1, 0), "forced");
        assert!(!path.exists());
        s.begin("call-3").unwrap();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn leftover_state_is_put_back_once() {
        let dir = temp_dir("leftover");
        let path = dir.join("audio_session");
        {
            // A helper that was killed mid-call: the session never ended.
            let mut s = Session::new(&path, Origin::App);
            s.begin("call-9").unwrap();
            s.remember(Saved::Mixer { key: key("Incall Sink Mute"), values: vec![0] }).unwrap();
        }
        let mut phone = FakePhone::default();
        let report = restore_leftover(&path, &mut phone, false).unwrap();
        assert!(report.clean());
        assert_eq!(phone.ctls.get("Incall Sink Mute"), Some(&vec![0]));
        assert!(!path.exists());
        assert!(restore_leftover(&path, &mut phone, false).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn values_off_the_list_are_kept_not_written() {
        let dir = temp_dir("offlist");
        let path = dir.join("audio_session");
        let mut s = Session::new(&path, Origin::App);
        s.begin("call-1").unwrap();
        s.remember(Saved::Mixer { key: key("Speaker Volume"), values: vec![80] }).unwrap();
        drop(s);
        let mut phone = FakePhone { not_allowed: Some("Speaker Volume".into()), ..Default::default() };
        let r = restore_leftover(&path, &mut phone, false).unwrap();
        assert_eq!((r.not_allowed, r.left, r.restored), (1, 1, 0));
        assert!(phone.writes.is_empty());
        assert!(path.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn untrusted_or_unreadable_state_is_kept_and_not_used() {
        let dir = temp_dir("untrusted");
        let path = dir.join("audio_session");
        fs::write(&path, encode_state(Origin::App, &[e(Saved::Mixer { key: key("A"), values: vec![1] })])).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let mut phone = FakePhone::default();
        let r = restore_leftover(&path, &mut phone, false).unwrap();
        assert!(r.unreadable && phone.writes.is_empty() && path.exists(), "readable by others: not trusted");

        fs::write(&path, b"\xff\xfe garbage").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let r = restore_leftover(&path, &mut phone, false).unwrap();
        assert!(r.unreadable && path.exists());

        let text = format!("{}mixer 0 99 61 2 0 0 0 41 1\n", encode_state(Origin::App, &[e(Saved::Mixer { key: key("A"), values: vec![1] })]));
        fs::write(&path, text).unwrap();
        let r = restore_leftover(&path, &mut phone, false).unwrap();
        assert_eq!(r.restored, 1, "the readable line is put back");
        assert!(r.unreadable && path.exists(), "the unreadable line keeps the file");

        fs::write(&path, "x".repeat(MAX_STATE_BYTES + 1)).unwrap();
        let r = restore_leftover(&path, &mut phone, false).unwrap();
        assert!(r.unreadable);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn the_state_file_has_a_size_limit() {
        let dir = temp_dir("limit");
        let path = dir.join("audio_session");
        let mut s = Session::new(&path, Origin::Root);
        s.begin("big").unwrap();
        let long = "n".repeat(40);
        let mut failed = false;
        for i in 0..MAX_SAVED {
            let mut k = key(&long);
            k.index = i as u32;
            if s.remember(Saved::Mixer { key: k, values: vec![i64::MIN; 128] }).is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed, "a session cannot write more than the limit");
        assert!(fs::metadata(&path).unwrap().len() as usize <= MAX_STATE_BYTES);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn session_ids() {
        assert!(valid_session_id("call-20261126-1"));
        assert!(!valid_session_id(""));
        assert!(!valid_session_id("a b"));
        assert!(!valid_session_id(&"a".repeat(65)));
        let dir = temp_dir("ids");
        let mut s = Session::new(dir.join("audio_session"), Origin::App);
        assert!(s.begin("../x").is_err());
        let _ = fs::remove_dir_all(dir);
    }
}
