// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! One call is one session. Before the helper changes a mixer control or an audio
//! parameter for the first time in a session, it saves the old value to a state file
//! (written and flushed to disk before the change). The session's end puts every saved
//! value back, newest first, and deletes the file. That happens when the app ends the
//! session, when the connection to the app breaks, when the helper is told to stop
//! (a signal), and, if none of those ran (the helper was killed, the phone rebooted),
//! the next time the helper starts or the phone boots: a state file that is still
//! there means a session that was never put back.
//!
//! State file (root only, `/data/adb/pixelcallai/audio_session`):
//!
//! ```text
//! bageo-audio-session 1
//! id <session id>
//! mixer <card> <iface> <device> <subdevice> <index> <name, hex> <v1,v2,...>
//! param <key> =<value>
//! ```
//!
//! (Putting values back from a state file, instead of writing fixed values as gsm2sip
//! does in its restore commands, is this helper's own choice.)

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::json::Json;
use crate::mixer::{CtlKey, MAX_CARD};
use crate::params;

pub const STATE_HEADER: &str = "bageo-audio-session 1";
/// Most values one session may save.
pub const MAX_SAVED: usize = 512;
/// A state file larger than this is not read.
pub const MAX_STATE_BYTES: u64 = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Saved {
    Mixer { key: CtlKey, values: Vec<i64> },
    Param { key: String, value: String },
}

/// Writes saved values back to the phone.
pub trait Restorer {
    fn write_ctl(&mut self, key: &CtlKey, values: &[i64]) -> Result<(), String>;
    fn set_param(&mut self, key: &str, value: &str) -> Result<(), String>;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub restored: usize,
    pub failed: Vec<String>,
}

impl Report {
    pub fn json(&self) -> Json {
        Json::obj()
            .with("restored", Json::Num(self.restored as i64))
            .with("failed", Json::Arr(self.failed.iter().map(|f| Json::str(f.clone())).collect()))
    }
}

pub fn valid_session_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

pub struct Session {
    path: PathBuf,
    id: Option<String>,
    saved: Vec<Saved>,
}

impl Session {
    pub fn new(path: impl Into<PathBuf>) -> Session {
        Session { path: path.into(), id: None, saved: Vec::new() }
    }

    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn saved(&self) -> &[Saved] {
        &self.saved
    }

    pub fn begin(&mut self, id: &str) -> Result<(), String> {
        if self.id.is_some() {
            return Err("a session is already open".into());
        }
        if !valid_session_id(id) {
            return Err("bad session id".into());
        }
        write_state(&self.path, id, &[]).map_err(|e| format!("cannot save the session: {e}"))?;
        self.id = Some(id.to_string());
        self.saved.clear();
        Ok(())
    }

    fn already_saved(&self, item: &Saved) -> bool {
        self.saved.iter().any(|s| match (s, item) {
            (Saved::Mixer { key: a, .. }, Saved::Mixer { key: b, .. }) => a == b,
            (Saved::Param { key: a, .. }, Saved::Param { key: b, .. }) => a == b,
            _ => false,
        })
    }

    /// Saves an old value before its first change in this session (a later change of
    /// the same control keeps the first value). Saved on disk before it returns.
    pub fn remember(&mut self, item: Saved) -> Result<(), String> {
        let id = self.id.clone().ok_or("no session: send session.begin first")?;
        if self.already_saved(&item) {
            return Ok(());
        }
        if self.saved.len() >= MAX_SAVED {
            return Err("this session changed too many values".into());
        }
        self.saved.push(item);
        if let Err(e) = write_state(&self.path, &id, &self.saved) {
            self.saved.pop();
            return Err(format!("cannot save the old value: {e}"));
        }
        Ok(())
    }

    /// Ends the session: puts every saved value back and deletes the state file.
    /// `None` when no session was open.
    pub fn end<R: Restorer>(&mut self, r: &mut R) -> Option<Report> {
        self.id.as_ref()?;
        let report = restore_items(&self.saved, r);
        let _ = fs::remove_file(&self.path);
        self.id = None;
        self.saved.clear();
        Some(report)
    }
}

fn restore_items<R: Restorer>(items: &[Saved], r: &mut R) -> Report {
    let mut report = Report::default();
    for item in items.iter().rev() {
        let result = match item {
            Saved::Mixer { key, values } => r.write_ctl(key, values).map_err(|e| format!("{}: {e}", key.name)),
            Saved::Param { key, value } => r.set_param(key, value).map_err(|e| format!("{key}: {e}")),
        };
        match result {
            Ok(()) => report.restored += 1,
            Err(e) => report.failed.push(e),
        }
    }
    report
}

/// Puts back a session nobody ended (a state file still on disk), then deletes the
/// file. `None` when there is no state file.
pub fn restore_leftover<R: Restorer>(path: &Path, r: &mut R) -> Option<Report> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => return Some(Report { restored: 0, failed: vec![format!("state file: {e}")] }),
    };
    let report = if !meta.is_file() || meta.len() > MAX_STATE_BYTES {
        Report { restored: 0, failed: vec!["state file is not a plain file of a sane size".into()] }
    } else {
        match fs::read(path) {
            Ok(bytes) => match decode_state(&String::from_utf8_lossy(&bytes)) {
                Ok((_, items, bad)) => {
                    let mut report = restore_items(&items, r);
                    report.failed.extend((0..bad).map(|_| "unreadable line in the state file".to_string()));
                    report
                }
                Err(e) => Report { restored: 0, failed: vec![e] },
            },
            Err(e) => return Some(Report { restored: 0, failed: vec![format!("state file: {e}")] }),
        }
    };
    let _ = fs::remove_file(path);
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

pub fn encode_state(id: &str, items: &[Saved]) -> String {
    let mut out = format!("{STATE_HEADER}\nid {id}\n");
    for item in items {
        match item {
            Saved::Mixer { key, values } => {
                let vals: Vec<String> = values.iter().map(|v| v.to_string()).collect();
                out.push_str(&format!(
                    "mixer {} {} {} {} {} {} {}\n",
                    key.card,
                    key.iface,
                    key.device,
                    key.subdevice,
                    key.index,
                    hex(&key.name),
                    vals.join(",")
                ));
            }
            Saved::Param { key, value } => out.push_str(&format!("param {key} ={value}\n")),
        }
    }
    out
}

fn decode_line(line: &str) -> Option<Saved> {
    let f: Vec<&str> = line.split(' ').collect();
    match f.first().copied() {
        Some("mixer") if f.len() == 8 => {
            let card = f[1].parse::<u32>().ok().filter(|&c| c <= MAX_CARD)?;
            let values: Option<Vec<i64>> = f[7].split(',').map(|v| v.parse::<i64>().ok()).collect();
            let values = values.filter(|v| !v.is_empty() && v.len() <= 128)?;
            Some(Saved::Mixer {
                key: CtlKey {
                    card,
                    iface: f[2].parse().ok()?,
                    device: f[3].parse().ok()?,
                    subdevice: f[4].parse().ok()?,
                    index: f[5].parse().ok()?,
                    name: unhex(f[6])?,
                },
                values,
            })
        }
        Some("param") if f.len() == 3 => {
            let value = f[2].strip_prefix('=')?;
            if !params::valid_key(f[1]) || !params::valid_value(value) {
                return None;
            }
            Some(Saved::Param { key: f[1].to_string(), value: value.to_string() })
        }
        _ => None,
    }
}

/// The session id, the saved values and the number of lines that could not be read.
pub fn decode_state(text: &str) -> Result<(String, Vec<Saved>, usize), String> {
    let mut lines = text.lines();
    if lines.next() != Some(STATE_HEADER) {
        return Err("not a session state file".into());
    }
    let mut id = String::new();
    let mut items = Vec::new();
    let mut bad = 0;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("id ") {
            id = rest.to_string();
            continue;
        }
        match decode_line(line) {
            Some(item) if items.len() < MAX_SAVED => items.push(item),
            _ => bad += 1,
        }
    }
    Ok((id, items, bad))
}

/// Writes the state file: a temporary file next to it, flushed, then renamed over it,
/// then the folder flushed, so a crash leaves the old file or the new one, whole.
fn write_state(path: &Path, id: &str, items: &[Saved]) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let dir = path.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no folder"))?;
    if !dir.is_dir() {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let _ = fs::remove_file(&tmp);
    {
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(encode_state(id, items).as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    fs::File::open(dir)?.sync_all()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A phone: control values by name, parameters by key, and a log of every write.
    #[derive(Default)]
    pub struct FakePhone {
        pub ctls: HashMap<String, Vec<i64>>,
        pub params: HashMap<String, String>,
        pub writes: Vec<String>,
        pub fail_ctl: Option<String>,
    }

    impl Restorer for FakePhone {
        fn write_ctl(&mut self, key: &CtlKey, values: &[i64]) -> Result<(), String> {
            if self.fail_ctl.as_deref() == Some(key.name.as_str()) {
                return Err("card is gone".into());
            }
            self.writes.push(format!("ctl {}={values:?}", key.name));
            self.ctls.insert(key.name.clone(), values.to_vec());
            Ok(())
        }
        fn set_param(&mut self, key: &str, value: &str) -> Result<(), String> {
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
        CtlKey { card: 0, iface: 2, device: 0, subdevice: 0, index: 0, name: name.into() }
    }

    #[test]
    fn state_file_round_trip() {
        let items = vec![
            Saved::Mixer { key: key("Voice Rx Device Mute"), values: vec![0, -1, 20] },
            Saved::Mixer {
                key: CtlKey { card: 1, iface: 3, device: 2, subdevice: 1, index: 4, name: "A,B=C".into() },
                values: vec![7],
            },
            Saved::Param { key: "incall_music_enabled".into(), value: "false".into() },
            Saved::Param { key: "Set_BGS_UL_Mute".into(), value: String::new() },
        ];
        let text = encode_state("call-1", &items);
        let (id, back, bad) = decode_state(&text).unwrap();
        assert_eq!((id.as_str(), bad), ("call-1", 0));
        assert_eq!(back, items);
    }

    #[test]
    fn damaged_state_lines_are_skipped() {
        let text = format!(
            "{STATE_HEADER}\nid x\nmixer 0 2 0 0 0 zz 1\nmixer 99 2 0 0 0 41 1\nparam a=b =1\nparam k =v\nwhatever\n"
        );
        let (_, items, bad) = decode_state(&text).unwrap();
        assert_eq!(items, vec![Saved::Param { key: "k".into(), value: "v".into() }]);
        assert_eq!(bad, 4);
        assert!(decode_state("something else\n").is_err());
    }

    #[test]
    fn normal_end_puts_everything_back_newest_first() {
        let dir = temp_dir("end");
        let path = dir.join("audio_session");
        let mut s = Session::new(&path);
        assert!(s.remember(Saved::Param { key: "k".into(), value: "v".into() }).is_err(), "no session yet");
        s.begin("call-1").unwrap();
        assert!(s.begin("call-2").is_err());
        s.remember(Saved::Mixer { key: key("Incall Mic Mute"), values: vec![0] }).unwrap();
        s.remember(Saved::Param { key: "incall_music_enabled".into(), value: "false".into() }).unwrap();
        // A second change of the same control keeps the first old value.
        s.remember(Saved::Mixer { key: key("Incall Mic Mute"), values: vec![1] }).unwrap();
        let on_disk = fs::read_to_string(&path).unwrap();
        assert_eq!(decode_state(&on_disk).unwrap().1.len(), 2);

        let mut phone = FakePhone::default();
        let report = s.end(&mut phone).unwrap();
        assert_eq!(report, Report { restored: 2, failed: vec![] });
        assert_eq!(phone.writes, vec!["param incall_music_enabled=false", "ctl Incall Mic Mute=[0]"]);
        assert!(!path.exists());
        assert!(s.end(&mut phone).is_none(), "nothing left to end");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn leftover_state_is_put_back_once() {
        let dir = temp_dir("leftover");
        let path = dir.join("audio_session");
        {
            // A helper that was killed mid-call: the session never ended.
            let mut s = Session::new(&path);
            s.begin("call-9").unwrap();
            s.remember(Saved::Mixer { key: key("Incall Sink Mute"), values: vec![0] }).unwrap();
            s.remember(Saved::Mixer { key: key("Broken"), values: vec![3] }).unwrap();
        }
        let mut phone = FakePhone { fail_ctl: Some("Broken".into()), ..Default::default() };
        let report = restore_leftover(&path, &mut phone).unwrap();
        assert_eq!(report.restored, 1);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(phone.ctls.get("Incall Sink Mute"), Some(&vec![0]));
        assert!(!path.exists(), "a failed value does not keep the file around forever");
        assert!(restore_leftover(&path, &mut phone).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn unreadable_leftover_is_removed() {
        let dir = temp_dir("garbage");
        let path = dir.join("audio_session");
        fs::write(&path, b"\xff\xfe garbage").unwrap();
        let mut phone = FakePhone::default();
        let report = restore_leftover(&path, &mut phone).unwrap();
        assert_eq!(report.restored, 0);
        assert!(!path.exists());
        assert!(phone.writes.is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn session_ids() {
        assert!(valid_session_id("call-20261126-1"));
        assert!(!valid_session_id(""));
        assert!(!valid_session_id("a b"));
        assert!(!valid_session_id(&"a".repeat(65)));
        let dir = temp_dir("ids");
        let mut s = Session::new(dir.join("audio_session"));
        assert!(s.begin("../x").is_err());
        let _ = fs::remove_dir_all(dir);
    }
}
