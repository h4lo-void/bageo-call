// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Backups of Bageo's own data: "Back up now", automatic backups (every day,
//! week or month), and every setting in one file for another computer
//! (export / import). Nothing here goes over the network.
//!
//! A backup is a zip file. `bageo-backup.json` at its root ([`Manifest`])
//! lists every other file: under `data/`, with its path inside the data folder
//! (`CALLS_DATA_DIR`), its size and its SHA-256, and `items` names the parts
//! it holds ([`ITEMS`]): settings, keys (credentials, where data goes and who
//! may operate Bageo: [`crate::settings_store::is_key_field`], and the phone
//! pairing key), contacts, the number book, phones (SIMs and
//! names), AI errands, and the call records with their recordings, text
//! messages and to-do items. Settings and keys share `settings.json`: a file
//! without one of them leaves its fields out. Never the backup folder itself,
//! temporary files, debug output or caches: only the files named in
//! [`item_of`] go in.
//!
//! Importing first answers a preview (nothing changed yet) with a token. The
//! token, used once within 15 minutes, restores the parts the owner picks: a
//! `before_import` backup is made first and put back when restoring fails
//! half-way. Restoring `keys` takes that whole group from the file (so putting
//! the `before_import` copy back undoes it); without `keys` the group here
//! stays. How long records are kept never gets shorter. Call records, texts
//! and to-do items are only added; the ones already here stay.
//!
//! With `backup_encrypt` (or a password given to an export) the zip is sealed
//! with a password into a `.bageo` file ([`crate::backup_crypt`]). Such a file
//! is opened (`unlock`) before its preview; a restore from the list tries the
//! password saved here first. Downloads go through one-time tickets that last
//! two minutes, so no password ever sits in a download link.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use axum::body::{Body, Bytes};
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{header, Extensions, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Local, Months, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use crate::calls_http::{api_error, token_ok_parts, unauthorized, CallsState, TokenQuery};
use crate::backup_crypt::{self, CryptErr};
use crate::locale::Lang;
use crate::settings_store::RuntimeSettings;

/// `format` of the manifest.
pub const FORMAT: &str = "bageo-backup";
/// The newest manifest `version` this agent reads (and the one it writes).
pub const VERSION: u64 = 1;
const MANIFEST_NAME: &str = "bageo-backup.json";
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Largest file `POST /api/backup/import` takes.
pub const IMPORT_MAX_BYTES: usize = 200 << 20;
/// What the parts other than the call records may unpack to: each file, all of
/// them, how many. Set by what a file is, never by what the manifest says it holds.
const SMALL_FILE_MAX_BYTES: u64 = 20 << 20;
const SMALL_MAX_BYTES: u64 = 50 << 20;
const SMALL_MAX_FILES: usize = 2_000;
/// What the call records, texts and to-do items may unpack to.
const CALLS_MAX_BYTES: u64 = 8 << 30;
const CALLS_MAX_FILES: usize = 500_000;
/// A file that unpacks to more than this many times its packed size is refused
/// (when it unpacks to more than [`RATIO_FREE_BYTES`]).
const RATIO_MAX: u64 = 200;
const RATIO_FREE_BYTES: u64 = 64 << 10;
/// Previews waiting at once, what their uploads may take together, uploads under way.
const PENDING_MAX: usize = 3;
const STAGING_MAX_BYTES: u64 = 600 << 20;
const UPLOADS_MAX: usize = 2;
/// Room left on the disk after the call records are restored.
const DISK_MARGIN_BYTES: u64 = 64 << 20;
const MANIFEST_MAX_BYTES: u64 = 64 << 20;
/// A JSON file read whole for the preview (the call list, the texts).
const COUNT_FILE_MAX_BYTES: u64 = 256 << 20;
/// An older single settings.json.
const PLAIN_MAX_BYTES: u64 = 16 << 20;
/// The zip's central directory.
const DIRECTORY_MAX_BYTES: u64 = 256 << 20;
const TOKEN_TTL: Duration = Duration::from_secs(15 * 60);
/// A download ticket works once, within two minutes.
const TICKET_TTL: Duration = Duration::from_secs(120);
/// Wrong passwords one preview takes before it goes, and the least time between two tries.
const UNLOCK_TRIES: u32 = 5;
const UNLOCK_GAP: Duration = Duration::from_secs(1);
/// After [`UNLOCK_TRIES`] wrong passwords in a row, on any preview, each try
/// waits twice as long as the one before, up to this.
const UNLOCK_WAIT_MAX: Duration = Duration::from_secs(if cfg!(test) { 4 } else { 60 });
/// The name ending of an encrypted backup (the others end in `.zip`).
const SEALED_EXT: &str = ".bageo";
const BEFORE_IMPORT_KEEP: usize = 5;
const CHECK_EVERY: Duration = Duration::from_secs(10 * 60);
const RETRY_AFTER_HOURS: i64 = 1;
const STAMP: &str = "%Y%m%d-%H%M%S";
const U32_MAX: u64 = 0xFFFF_FFFF;
/// Files this big get zip64 sizes in their local header.
const BIG_FILE: u64 = 0xF000_0000;

/// The parts of a backup, in the order the preview lists them.
pub const ITEMS: [&str; 9] = ["settings", "keys", "contacts", "numbers", "phones", "errands", "calls", "sms", "tasks"];
/// What backups (manual, automatic, before an import) and an export without
/// `items` hold; with `backup_include_calls` backups also hold [`CALL_ITEMS`].
pub const DEFAULT_ITEMS: [&str; 6] = ["settings", "keys", "contacts", "numbers", "phones", "errands"];
/// The parts that are only added to what is here.
const CALL_ITEMS: [&str; 3] = ["calls", "sms", "tasks"];
/// The parts that are read again by their store once they are back in place.
const STORE_ITEMS: [&str; 5] = ["keys", "contacts", "numbers", "phones", "errands"];

/// `items` as the parts they name (in [`ITEMS`] order); None when the list is
/// empty or names something else.
pub fn parse_items<S: AsRef<str>>(items: &[S]) -> Option<Vec<&'static str>> {
    let mut out: Vec<&'static str> = Vec::new();
    for i in items {
        let id = ITEMS.iter().copied().find(|x| *x == i.as_ref().trim())?;
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out.sort_by_key(|id| ITEMS.iter().position(|x| x == id));
    (!out.is_empty()).then_some(out)
}

// ---------------------------------------------------------------- names

/// Why a backup was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Scheduled,
    Manual,
    BeforeImport,
    Export,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Scheduled => "scheduled",
            Kind::Manual => "manual",
            Kind::BeforeImport => "before_import",
            Kind::Export => "export",
        }
    }
}

/// The file name of a backup made at `at` (local time), not encrypted.
pub fn file_name(kind: Kind, at: NaiveDateTime) -> String {
    let stamp = at.format(STAMP);
    match kind {
        Kind::Scheduled => format!("bageo-backup-{stamp}-auto.zip"),
        Kind::Manual => format!("bageo-backup-{stamp}-manual.zip"),
        Kind::BeforeImport => format!("bageo-backup-{stamp}-before-import.zip"),
        Kind::Export => format!("bageo-export-{stamp}.zip"),
    }
}

/// [`file_name`], ending in `.bageo` when the file is encrypted.
pub fn file_name_as(kind: Kind, at: NaiveDateTime, sealed: bool) -> String {
    let name = file_name(kind, at);
    match name.strip_suffix(".zip") {
        Some(stem) if sealed => format!("{stem}{SEALED_EXT}"),
        _ => name,
    }
}

/// The name of an encrypted backup.
pub fn is_sealed_name(name: &str) -> bool {
    name.ends_with(SEALED_EXT)
}

/// What a backup file name says (kind and local time); None for any other
/// name. `.zip` and `.bageo` (encrypted) alike.
pub fn parse_name(name: &str) -> Option<(Kind, NaiveDateTime)> {
    if !name.is_ascii() {
        return None;
    }
    let stem = name.strip_suffix(".zip").or_else(|| name.strip_suffix(SEALED_EXT))?;
    let export = stem.strip_prefix("bageo-export-").or_else(|| stem.strip_prefix("bageo-settings-"));
    let (kind, stamp) = if let Some(rest) = export {
        (Kind::Export, rest)
    } else {
        let rest = stem.strip_prefix("bageo-backup-")?;
        if rest.len() < 15 {
            return None;
        }
        let (stamp, tail) = rest.split_at(15);
        let kind = match tail {
            "-auto" => Kind::Scheduled,
            "-manual" => Kind::Manual,
            "-before-import" => Kind::BeforeImport,
            _ => return None,
        };
        (kind, stamp)
    };
    let shaped = stamp.len() == 15 && stamp.bytes().enumerate().all(|(i, b)| if i == 8 { b == b'-' } else { b.is_ascii_digit() });
    if !shaped {
        return None;
    }
    Some((kind, NaiveDateTime::parse_from_str(stamp, STAMP).ok()?))
}

fn local_rfc3339(at: NaiveDateTime) -> String {
    Local
        .from_local_datetime(&at)
        .earliest()
        .map(|d| d.to_rfc3339_opts(SecondsFormat::Secs, false))
        .unwrap_or_else(|| at.format("%Y-%m-%dT%H:%M:%S").to_string())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------- what goes in

/// The part of a backup the file at `rel` (path inside the data folder, `/`
/// between folders) belongs to; None: never in a backup.
pub fn item_of(rel: &str) -> Option<&'static str> {
    match rel {
        "config/settings.json" => return Some("settings"),
        "config/scam_numbers.json" | "config/scam_feedback.jsonl" | "config/scam_db.local.json" => return Some("numbers"),
        "contacts.json" => return Some("contacts"),
        "phone_sims.json" => return Some("phones"),
        "phone-pairing.key" => return Some("keys"),
        "errands.json" => return Some("errands"),
        "index.json" => return Some("calls"),
        "sms/messages.json" => return Some("sms"),
        _ => {}
    }
    let mut parts = rel.split('/');
    let (Some(first), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    if is_scratch(name) {
        return None;
    }
    match first {
        "config" if name.ends_with(".json") || name.ends_with(".jsonl") => Some("settings"),
        "tasks" if name.ends_with(".json") => Some("tasks"),
        // Recordings and call notes; `.pcm` is a call still being recorded.
        day if is_day(day) && !name.ends_with(".pcm") => Some("calls"),
        _ => None,
    }
}

/// Temporary files and files set aside as unreadable.
fn is_scratch(name: &str) -> bool {
    name.is_empty() || name.starts_with('.') || name.ends_with(".tmp") || name.contains(".unreadable")
}

fn is_day(s: &str) -> bool {
    s.len() == 10 && NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

/// A regular file (a symbolic link is not followed).
fn is_plain_file(p: &Path) -> bool {
    std::fs::symlink_metadata(p).map(|m| m.file_type().is_file()).unwrap_or(false)
}

fn is_plain_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p).map(|m| m.file_type().is_dir()).unwrap_or(false)
}

/// The names in `dir`, sorted (none when it cannot be read).
fn names_in(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<String> = rd.flatten().filter_map(|e| e.file_name().into_string().ok()).collect();
    v.sort();
    v
}

/// Where each kind of file is kept on this computer.
#[derive(Debug, Clone)]
struct Layout {
    data: PathBuf,
    config: PathBuf,
    calls: PathBuf,
    tasks: PathBuf,
    sms: PathBuf,
    contacts: PathBuf,
    errands: PathBuf,
    sims: PathBuf,
}

impl Layout {
    fn of(state: &CallsState) -> Layout {
        let data = PathBuf::from(&state.settings.calls_data_dir);
        Layout {
            config: state.settings_store.storage_path().parent().map(Path::to_path_buf).unwrap_or_else(|| data.join("config")),
            calls: state.store.root().to_path_buf(),
            tasks: state.task_store.root().to_path_buf(),
            sms: state.sms.dir(),
            contacts: state.contacts.path().to_path_buf(),
            errands: state.errands.path().to_path_buf(),
            sims: state.errands.sims.path().map(Path::to_path_buf).unwrap_or_else(|| data.join("phone_sims.json")),
            data,
        }
    }

    /// Where the file a backup keeps at `rel` goes on this computer.
    fn target(&self, rel: &str) -> Option<PathBuf> {
        let item = item_of(rel)?;
        let (first, rest) = rel.split_once('/').unwrap_or((rel, ""));
        Some(match (item, rel) {
            (_, "contacts.json") => self.contacts.clone(),
            (_, "errands.json") => self.errands.clone(),
            (_, "phone_sims.json") => self.sims.clone(),
            (_, "phone-pairing.key") => self.data.join(crate::phone_wifi::KEY_FILE),
            (_, "index.json") => self.calls.join("index.json"),
            (_, "sms/messages.json") => self.sms.join("messages.json"),
            ("settings" | "numbers", _) => self.config.join(rest),
            ("tasks", _) => self.tasks.join(rest),
            ("calls", _) => self.calls.join(first).join(rest),
            _ => return None,
        })
    }
}

enum Source {
    Bytes(Vec<u8>),
    File(PathBuf),
}

/// The files of a backup of the parts `want` (path inside the data folder,
/// where to read it). `settings.json` is `settings_json` ([`settings_for`]).
fn collect(lay: &Layout, settings_json: Option<Vec<u8>>, want: &[&str]) -> Vec<(String, Source)> {
    let mut out = Vec::new();
    if let Some(json) = settings_json {
        out.push(("config/settings.json".to_string(), Source::Bytes(json)));
    }
    let add = |out: &mut Vec<(String, Source)>, rel: String| {
        if !item_of(&rel).is_some_and(|i| want.contains(&i)) {
            return;
        }
        if let Some(path) = lay.target(&rel) {
            if is_plain_file(&path) {
                out.push((rel, Source::File(path)));
            }
        }
    };
    for name in names_in(&lay.config) {
        let rel = format!("config/{name}");
        if rel != "config/settings.json" {
            add(&mut out, rel);
        }
    }
    for rel in ["contacts.json", "phone_sims.json", "phone-pairing.key", "errands.json"] {
        add(&mut out, rel.to_string());
    }
    if want.iter().any(|i| CALL_ITEMS.contains(i)) {
        add(&mut out, "index.json".to_string());
        for day in names_in(&lay.calls) {
            if !is_day(&day) || !is_plain_dir(&lay.calls.join(&day)) {
                continue;
            }
            for name in names_in(&lay.calls.join(&day)) {
                let rel = format!("{day}/{name}");
                if item_of(&rel) == Some("calls") {
                    add(&mut out, rel);
                }
            }
        }
        add(&mut out, "sms/messages.json".to_string());
        for name in names_in(&lay.tasks) {
            let rel = format!("tasks/{name}");
            if item_of(&rel) == Some("tasks") {
                add(&mut out, rel);
            }
        }
    }
    out
}

/// `settings.json` for a backup of the parts `want`: whole, without the group
/// `keys` (settings only), or only it (keys only); and the parts it holds.
/// `keys` counts even when none is set here: restoring it then clears the
/// group, which is what putting back such a copy has to do. None: neither
/// part is wanted.
fn settings_for(full: &[u8], want: &[&str]) -> (Option<Vec<u8>>, Vec<&'static str>) {
    use crate::settings_store::{keys_part, settings_part};
    let v: Value = serde_json::from_slice(full).unwrap_or_else(|_| json!({}));
    let (s, k) = (want.contains(&"settings"), want.contains(&"keys"));
    let part = match (s, k) {
        (true, true) => v,
        (true, false) => settings_part(&v),
        (false, true) => keys_part(&v),
        _ => return (None, Vec::new()),
    };
    let mut items = Vec::new();
    if s {
        items.push("settings");
    }
    if k {
        items.push("keys");
    }
    (serde_json::to_vec_pretty(&part).ok(), items)
}

// ---------------------------------------------------------------- the zip file

/// What the manifest says.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    format: String,
    version: u64,
    #[serde(default)]
    app_version: String,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    include_calls: bool,
    #[serde(default)]
    items: Vec<String>,
    #[serde(default)]
    files: Vec<FileEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileEntry {
    path: String,
    size: u64,
    sha256: String,
}

struct Counting<W> {
    inner: W,
    count: u64,
}

impl<W: Write> Write for Counting<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.count += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

struct CentralEntry {
    name: String,
    method: u16,
    crc: u32,
    packed: u64,
    size: u64,
    offset: u64,
}

/// A zip file written front to back (deflate; zip64 where sizes need it).
struct ZipOut<W: Write + Seek> {
    out: W,
    entries: Vec<CentralEntry>,
    time: u16,
    date: u16,
}

/// `src` copied to `out`, counted into `crc` and `sha`: how many bytes.
fn pump(src: &mut dyn Read, out: &mut dyn Write, crc: &mut flate2::Crc, sha: &mut Sha256) -> io::Result<u64> {
    let mut size = 0u64;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match src.read(&mut buf) {
            Ok(0) => return Ok(size),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        crc.update(&buf[..n]);
        sha.update(&buf[..n]);
        size += n as u64;
        out.write_all(&buf[..n])?;
    }
}

fn dos_time(at: NaiveDateTime) -> (u16, u16) {
    use chrono::{Datelike, Timelike};
    let year = at.year().clamp(1980, 2107) as u16;
    let date = ((year - 1980) << 9) | ((at.month() as u16) << 5) | at.day() as u16;
    let time = ((at.hour() as u16) << 11) | ((at.minute() as u16) << 5) | (at.second() as u16 / 2);
    (time, date)
}

impl<W: Write + Seek> ZipOut<W> {
    fn new(out: W, at: NaiveDateTime) -> Self {
        let (time, date) = dos_time(at);
        Self { out, entries: Vec::new(), time, date }
    }

    /// One file, what `src` reads: its size and SHA-256 (hex).
    fn add(&mut self, name: &str, src: &mut dyn Read, size_hint: u64) -> io::Result<(u64, String)> {
        let offset = self.out.stream_position()?;
        let zip64 = size_hint >= BIG_FILE;
        // Recordings are kept as they are: they hardly pack, and a file of
        // silence would look like a zip bomb.
        let method: u16 = if name.ends_with(".wav") || name.ends_with(".pcm") { 0 } else { 8 };
        let name_b = name.as_bytes();
        let mut h = Vec::with_capacity(50 + name_b.len());
        h.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        h.extend_from_slice(&(if zip64 { 45u16 } else { 20 }).to_le_bytes());
        h.extend_from_slice(&0x0800u16.to_le_bytes());
        h.extend_from_slice(&method.to_le_bytes());
        h.extend_from_slice(&self.time.to_le_bytes());
        h.extend_from_slice(&self.date.to_le_bytes());
        h.extend_from_slice(&0u32.to_le_bytes());
        let size_field = if zip64 { u32::MAX } else { 0 };
        h.extend_from_slice(&size_field.to_le_bytes());
        h.extend_from_slice(&size_field.to_le_bytes());
        h.extend_from_slice(&(name_b.len() as u16).to_le_bytes());
        h.extend_from_slice(&(if zip64 { 20u16 } else { 0 }).to_le_bytes());
        h.extend_from_slice(name_b);
        if zip64 {
            h.extend_from_slice(&1u16.to_le_bytes());
            h.extend_from_slice(&16u16.to_le_bytes());
            h.extend_from_slice(&[0u8; 16]);
        }
        self.out.write_all(&h)?;
        let level = if size_hint > (1 << 20) { 1 } else { 6 };
        let mut crc = flate2::Crc::new();
        let mut sha = Sha256::new();
        let (size, packed) = if method == 0 {
            let mut out = Counting { inner: &mut self.out, count: 0 };
            let size = pump(src, &mut out, &mut crc, &mut sha)?;
            (size, out.count)
        } else {
            let mut enc = flate2::write::DeflateEncoder::new(Counting { inner: &mut self.out, count: 0 }, flate2::Compression::new(level));
            let size = pump(src, &mut enc, &mut crc, &mut sha)?;
            (size, enc.finish()?.count)
        };
        if !zip64 && (size >= U32_MAX || packed >= U32_MAX) {
            return Err(io::Error::other("a file grew past 4 GB while it was saved"));
        }
        let end = self.out.stream_position()?;
        self.out.seek(SeekFrom::Start(offset + 14))?;
        self.out.write_all(&crc.sum().to_le_bytes())?;
        if zip64 {
            self.out.seek(SeekFrom::Start(offset + 30 + name_b.len() as u64 + 4))?;
            self.out.write_all(&size.to_le_bytes())?;
            self.out.write_all(&packed.to_le_bytes())?;
        } else {
            self.out.write_all(&(packed as u32).to_le_bytes())?;
            self.out.write_all(&(size as u32).to_le_bytes())?;
        }
        self.out.seek(SeekFrom::Start(end))?;
        self.entries.push(CentralEntry { name: name.to_string(), method, crc: crc.sum(), packed, size, offset });
        Ok((size, hex(&sha.finalize())))
    }

    /// The central directory; the writer back.
    fn finish(mut self) -> io::Result<W> {
        let start = self.out.stream_position()?;
        for e in &self.entries {
            let mut wide = Vec::new();
            for v in [e.size, e.packed, e.offset] {
                if v >= U32_MAX {
                    wide.extend_from_slice(&v.to_le_bytes());
                }
            }
            let mut extra = Vec::new();
            if !wide.is_empty() {
                extra.extend_from_slice(&1u16.to_le_bytes());
                extra.extend_from_slice(&(wide.len() as u16).to_le_bytes());
                extra.extend_from_slice(&wide);
            }
            let narrow = |v: u64| if v >= U32_MAX { u32::MAX } else { v as u32 };
            let mut h = Vec::with_capacity(46 + e.name.len() + extra.len());
            h.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            h.extend_from_slice(&45u16.to_le_bytes());
            h.extend_from_slice(&(if extra.is_empty() { 20u16 } else { 45 }).to_le_bytes());
            h.extend_from_slice(&0x0800u16.to_le_bytes());
            h.extend_from_slice(&e.method.to_le_bytes());
            h.extend_from_slice(&self.time.to_le_bytes());
            h.extend_from_slice(&self.date.to_le_bytes());
            h.extend_from_slice(&e.crc.to_le_bytes());
            h.extend_from_slice(&narrow(e.packed).to_le_bytes());
            h.extend_from_slice(&narrow(e.size).to_le_bytes());
            h.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            h.extend_from_slice(&(extra.len() as u16).to_le_bytes());
            h.extend_from_slice(&[0u8; 6]);
            h.extend_from_slice(&0u32.to_le_bytes());
            h.extend_from_slice(&narrow(e.offset).to_le_bytes());
            h.extend_from_slice(e.name.as_bytes());
            h.extend_from_slice(&extra);
            self.out.write_all(&h)?;
        }
        let end = self.out.stream_position()?;
        let (count, cd_size) = (self.entries.len() as u64, end - start);
        let mut t = Vec::with_capacity(98);
        if count >= 0xFFFF || cd_size >= U32_MAX || start >= U32_MAX {
            t.extend_from_slice(&0x0606_4b50u32.to_le_bytes());
            t.extend_from_slice(&44u64.to_le_bytes());
            t.extend_from_slice(&45u16.to_le_bytes());
            t.extend_from_slice(&45u16.to_le_bytes());
            t.extend_from_slice(&[0u8; 8]);
            t.extend_from_slice(&count.to_le_bytes());
            t.extend_from_slice(&count.to_le_bytes());
            t.extend_from_slice(&cd_size.to_le_bytes());
            t.extend_from_slice(&start.to_le_bytes());
            t.extend_from_slice(&0x0706_4b50u32.to_le_bytes());
            t.extend_from_slice(&0u32.to_le_bytes());
            t.extend_from_slice(&end.to_le_bytes());
            t.extend_from_slice(&1u32.to_le_bytes());
        }
        let short = count.min(0xFFFF) as u16;
        t.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        t.extend_from_slice(&[0u8; 4]);
        t.extend_from_slice(&short.to_le_bytes());
        t.extend_from_slice(&short.to_le_bytes());
        t.extend_from_slice(&(cd_size.min(U32_MAX) as u32).to_le_bytes());
        t.extend_from_slice(&(start.min(U32_MAX) as u32).to_le_bytes());
        t.extend_from_slice(&0u16.to_le_bytes());
        self.out.write_all(&t)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

/// A backup zip written to `out`: the `sources`, then the manifest. Its
/// `items`: the parts of the files in it, with `settings_items` for `settings.json`.
fn write_zip<W: Write + Seek>(out: W, sources: Vec<(String, Source)>, kind: Kind, settings_items: &[&'static str], at: DateTime<Local>) -> io::Result<(W, Vec<&'static str>)> {
    let mut zip = ZipOut::new(out, at.naive_local());
    let mut files = Vec::with_capacity(sources.len());
    let mut held: Vec<&'static str> = Vec::new();
    for (rel, src) in sources {
        let name = format!("data/{rel}");
        let (size, sha256) = match src {
            Source::Bytes(b) => zip.add(&name, &mut b.as_slice(), b.len() as u64)?,
            Source::File(path) => match File::open(&path) {
                Ok(mut f) => {
                    let hint = f.metadata().map(|m| m.len()).unwrap_or(0);
                    zip.add(&name, &mut f, hint)?
                }
                // Gone since the folder was read (a call record that just expired).
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            },
        };
        if let Some(item) = item_of(&rel).filter(|_| rel != "config/settings.json") {
            if !held.contains(&item) {
                held.push(item);
            }
        }
        files.push(FileEntry { path: name, size, sha256 });
    }
    for item in settings_items {
        if !held.contains(item) {
            held.push(item);
        }
    }
    held.sort_by_key(|id| ITEMS.iter().position(|x| x == id));
    let with_calls = held.iter().any(|i| CALL_ITEMS.contains(i));
    let manifest = Manifest {
        format: FORMAT.to_string(),
        version: VERSION,
        app_version: APP_VERSION.to_string(),
        created_at: at.to_rfc3339_opts(SecondsFormat::Secs, false),
        kind: kind.as_str().to_string(),
        include_calls: with_calls,
        items: held.iter().map(|i| i.to_string()).collect(),
        files,
    };
    let body = serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)?;
    zip.add(MANIFEST_NAME, &mut body.as_slice(), body.len() as u64)?;
    Ok((zip.finish()?, held))
}

/// Why a file is not restored.
#[derive(Debug)]
enum Bad {
    NotBackup,
    Damaged,
    TooNew,
    TooLarge,
    Io(io::Error),
}

impl From<io::Error> for Bad {
    fn from(e: io::Error) -> Self {
        Bad::Io(e)
    }
}

fn le16(b: &[u8], at: usize) -> Result<u64, Bad> {
    let s = b.get(at..at + 2).ok_or(Bad::Damaged)?;
    Ok(u64::from(u16::from_le_bytes([s[0], s[1]])))
}

fn le32(b: &[u8], at: usize) -> Result<u64, Bad> {
    let s = b.get(at..at + 4).ok_or(Bad::Damaged)?;
    Ok(u64::from(u32::from_le_bytes([s[0], s[1], s[2], s[3]])))
}

fn le64(b: &[u8], at: usize) -> Result<u64, Bad> {
    let s = b.get(at..at + 8).ok_or(Bad::Damaged)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Ok(u64::from_le_bytes(a))
}

fn read_at(f: &mut File, at: u64, len: usize) -> Result<Vec<u8>, Bad> {
    f.seek(SeekFrom::Start(at))?;
    let mut v = vec![0u8; len];
    f.read_exact(&mut v).map_err(|e| if e.kind() == io::ErrorKind::UnexpectedEof { Bad::Damaged } else { Bad::Io(e) })?;
    Ok(v)
}

#[derive(Debug, Clone, Copy)]
struct ZipEntry {
    method: u64,
    packed: u64,
    size: u64,
    offset: u64,
}

/// A zip file read from disk, one file at a time (zip64 too).
struct ZipIn {
    file: File,
    len: u64,
    entries: HashMap<String, ZipEntry>,
}

impl ZipIn {
    fn open(mut file: File) -> Result<Self, Bad> {
        let len = file.metadata()?.len();
        if len < 22 {
            return Err(Bad::NotBackup);
        }
        let tail_len = len.min(22 + 65_535);
        let tail = read_at(&mut file, len - tail_len, tail_len as usize)?;
        let pos = (0..=tail.len() - 22).rev().find(|&i| tail[i..i + 4] == [0x50, 0x4b, 0x05, 0x06]).ok_or(Bad::NotBackup)?;
        let end = &tail[pos..];
        let mut count = le16(end, 10)?;
        let mut cd_size = le32(end, 12)?;
        let mut cd_at = le32(end, 16)?;
        if count == 0xFFFF || cd_size == U32_MAX || cd_at == U32_MAX {
            let end_at = len - tail_len + pos as u64;
            if end_at < 20 {
                return Err(Bad::Damaged);
            }
            let locator = read_at(&mut file, end_at - 20, 20)?;
            if le32(&locator, 0)? != 0x0706_4b50 {
                return Err(Bad::Damaged);
            }
            let wide_at = le64(&locator, 8)?;
            if wide_at.checked_add(56).is_none_or(|e| e > len) {
                return Err(Bad::Damaged);
            }
            let wide = read_at(&mut file, wide_at, 56)?;
            if le32(&wide, 0)? != 0x0606_4b50 {
                return Err(Bad::Damaged);
            }
            count = le64(&wide, 32)?;
            cd_size = le64(&wide, 40)?;
            cd_at = le64(&wide, 48)?;
        }
        if count > CALLS_MAX_FILES as u64 + 16 || cd_size > DIRECTORY_MAX_BYTES {
            return Err(Bad::TooLarge);
        }
        if cd_at.checked_add(cd_size).is_none_or(|e| e > len) {
            return Err(Bad::Damaged);
        }
        let cd = read_at(&mut file, cd_at, cd_size as usize)?;
        let mut entries = HashMap::with_capacity(count.min(65_536) as usize);
        let mut p = 0usize;
        for _ in 0..count {
            if le32(&cd, p)? != 0x0201_4b50 {
                return Err(Bad::Damaged);
            }
            let flags = le16(&cd, p + 8)?;
            let method = le16(&cd, p + 10)?;
            let mut packed = le32(&cd, p + 20)?;
            let mut size = le32(&cd, p + 24)?;
            let name_len = le16(&cd, p + 28)? as usize;
            let extra_len = le16(&cd, p + 30)? as usize;
            let comment_len = le16(&cd, p + 32)? as usize;
            let mut offset = le32(&cd, p + 42)?;
            let name = cd.get(p + 46..p + 46 + name_len).ok_or(Bad::Damaged)?;
            let name = std::str::from_utf8(name).map_err(|_| Bad::Damaged)?.to_string();
            let extra = cd.get(p + 46 + name_len..p + 46 + name_len + extra_len).ok_or(Bad::Damaged)?;
            let mut q = 0;
            while q + 4 <= extra.len() {
                let id = le16(extra, q)?;
                let field_len = le16(extra, q + 2)? as usize;
                let body = extra.get(q + 4..q + 4 + field_len).ok_or(Bad::Damaged)?;
                if id == 1 {
                    let mut r = 0;
                    for slot in [&mut size, &mut packed, &mut offset] {
                        if *slot == U32_MAX {
                            *slot = le64(body, r)?;
                            r += 8;
                        }
                    }
                }
                q += 4 + field_len;
            }
            // Encrypted: not one of ours.
            if flags & 1 != 0 {
                return Err(Bad::Damaged);
            }
            if entries.insert(name, ZipEntry { method, packed, size, offset }).is_some() {
                return Err(Bad::Damaged);
            }
            p += 46 + name_len + extra_len + comment_len;
        }
        Ok(Self { file, len, entries })
    }

    fn reader(&mut self, e: ZipEntry) -> Result<Box<dyn Read + '_>, Bad> {
        let head = read_at(&mut self.file, e.offset, 30)?;
        if le32(&head, 0)? != 0x0403_4b50 {
            return Err(Bad::Damaged);
        }
        let start = e.offset + 30 + le16(&head, 26)? + le16(&head, 28)?;
        if start.checked_add(e.packed).is_none_or(|end| end > self.len) {
            return Err(Bad::Damaged);
        }
        self.file.seek(SeekFrom::Start(start))?;
        let raw = (&mut self.file).take(e.packed);
        match e.method {
            0 => Ok(Box::new(raw)),
            8 => Ok(Box::new(flate2::read::DeflateDecoder::new(raw))),
            _ => Err(Bad::Damaged),
        }
    }

    /// The file `name` (at most `max` bytes), or None when the zip has none.
    fn read_entry(&mut self, name: &str, max: u64) -> Result<Option<Vec<u8>>, Bad> {
        let Some(e) = self.entries.get(name).copied() else {
            return Ok(None);
        };
        if e.size > max {
            return Err(Bad::TooLarge);
        }
        let mut v = Vec::new();
        self.reader(e)?.take(max + 1).read_to_end(&mut v).map_err(|_| Bad::Damaged)?;
        if v.len() as u64 > max {
            return Err(Bad::TooLarge);
        }
        Ok(Some(v))
    }

    /// The file `name` copied to `out`; it must be `size` bytes with SHA-256 `sha`.
    fn copy_checked(&mut self, name: &str, size: u64, sha: &str, out: &mut dyn Write) -> Result<(), Bad> {
        let e = *self.entries.get(name).ok_or(Bad::Damaged)?;
        if e.size != size {
            return Err(Bad::Damaged);
        }
        let mut r = self.reader(e)?.take(size + 1);
        let mut h = Sha256::new();
        let mut n = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let k = match r.read(&mut buf) {
                Ok(0) => break,
                Ok(k) => k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(Bad::Damaged),
            };
            n += k as u64;
            if n > size {
                return Err(Bad::Damaged);
            }
            h.update(&buf[..k]);
            out.write_all(&buf[..k])?;
        }
        if n != size || !hex(&h.finalize()).eq_ignore_ascii_case(sha.trim()) {
            return Err(Bad::Damaged);
        }
        Ok(())
    }
}

/// A manifest path (`data/…`) as a path inside the data folder; None when it
/// could point anywhere else (`..`, an absolute path, a backslash, a drive letter).
fn safe_rel(path: &str) -> Option<&str> {
    let rel = path.strip_prefix("data/")?;
    if rel.is_empty() || rel.contains('\\') || rel.contains(':') || rel.starts_with('/') || rel.chars().any(char::is_control) {
        return None;
    }
    if rel.split('/').any(|c| c.is_empty() || c == "." || c == ".." || c.ends_with(' ') || c.ends_with('.') || reserved_name(c)) {
        return None;
    }
    Some(rel)
}

/// A name Windows keeps for a device, with or without an extension (`CON`,
/// `nul.json`, `COM1`, `lpt9.txt`, `COM¹`, `lpt³.wav`): such a file cannot
/// be made there.
fn reserved_name(c: &str) -> bool {
    let stem: Vec<char> = c.split('.').next().unwrap_or("").trim_end().chars().map(|ch| ch.to_ascii_uppercase()).collect();
    match stem.as_slice() {
        ['C', 'O', 'N'] | ['P', 'R', 'N'] | ['A', 'U', 'X'] | ['N', 'U', 'L'] => true,
        ['C', 'O', 'M', d] | ['L', 'P', 'T', d] => matches!(d, '1'..='9' | '\u{b9}' | '\u{b2}' | '\u{b3}'),
        _ => false,
    }
}

// ---------------------------------------------------------------- reading a backup

/// A file of a backup that is restored.
#[derive(Debug, Clone)]
struct Wanted {
    rel: String,
    item: &'static str,
    entry: FileEntry,
}

/// What a backup holds, checked.
#[derive(Debug, Clone)]
struct Inspected {
    /// An older single settings.json, not a zip.
    plain: bool,
    manifest: Option<Manifest>,
    files: Vec<Wanted>,
    /// The parts it has, with how many records where that is known.
    counts: BTreeMap<&'static str, Option<usize>>,
    /// What its settings.json holds, and the file itself.
    settings: Option<crate::settings_store::SettingsPeek>,
    settings_text: Option<String>,
    /// What the files it restores unpack to.
    total_size: u64,
}

/// Read `path` (a backup zip or an older settings.json) and check every file
/// in it: paths, sizes, SHA-256, limits.
fn inspect(path: &Path) -> Result<Inspected, Bad> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let mut head = [0u8; 2];
    let mut got = 0;
    while got < head.len() {
        match f.read(&mut head[got..])? {
            0 => break,
            n => got += n,
        }
    }
    if got == 2 && head == *b"PK" {
        return inspect_zip(f);
    }
    inspect_plain(f, len)
}

fn inspect_plain(mut f: File, len: u64) -> Result<Inspected, Bad> {
    if len == 0 || len > PLAIN_MAX_BYTES {
        return Err(Bad::NotBackup);
    }
    f.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| Bad::NotBackup)?.trim_start_matches('\u{feff}');
    let peek = crate::settings_store::peek_settings_text(text).map_err(|_| Bad::NotBackup)?;
    if !peek.has_settings && !peek.has_keys {
        return Err(Bad::NotBackup);
    }
    let mut counts = BTreeMap::new();
    if peek.has_settings {
        counts.insert("settings", None);
    }
    if peek.has_keys {
        counts.insert("keys", None);
    }
    let entry = FileEntry { path: "data/config/settings.json".to_string(), size: bytes.len() as u64, sha256: hex(&Sha256::digest(&bytes)) };
    Ok(Inspected {
        plain: true,
        manifest: None,
        files: vec![Wanted { rel: "config/settings.json".to_string(), item: "settings", entry }],
        counts,
        settings: Some(peek),
        settings_text: Some(text.to_string()),
        total_size: bytes.len() as u64,
    })
}

/// The files of a backup whose number of records the preview shows.
fn counted(rel: &str) -> bool {
    matches!(
        rel,
        "config/settings.json" | "contacts.json" | "config/scam_numbers.json" | "phone_sims.json" | "errands.json" | "index.json" | "sms/messages.json"
    )
}

fn inspect_zip(f: File) -> Result<Inspected, Bad> {
    let mut zip = ZipIn::open(f)?;
    let raw = zip.read_entry(MANIFEST_NAME, MANIFEST_MAX_BYTES)?.ok_or(Bad::NotBackup)?;
    let v: Value = serde_json::from_slice(&raw).map_err(|_| Bad::NotBackup)?;
    if v.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err(Bad::NotBackup);
    }
    let version = v.get("version").and_then(Value::as_u64).ok_or(Bad::Damaged)?;
    if version > VERSION {
        return Err(Bad::TooNew);
    }
    if version < 1 {
        return Err(Bad::Damaged);
    }
    let manifest: Manifest = serde_json::from_value(v).map_err(|_| Bad::Damaged)?;
    // Every path is checked; only the files of known parts are read, within
    // the limits of what each one is.
    let mut seen = HashSet::new();
    let (mut small, mut small_n, mut big, mut big_n) = (0u64, 0usize, 0u64, 0usize);
    let mut kept: Vec<(String, &'static str, FileEntry)> = Vec::new();
    for f in &manifest.files {
        let rel = safe_rel(&f.path).ok_or(Bad::Damaged)?;
        if !seen.insert(rel) || f.sha256.len() != 64 || !f.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Bad::Damaged);
        }
        let Some(item) = item_of(rel) else {
            continue;
        };
        if CALL_ITEMS.contains(&item) {
            big_n += 1;
            big = big.checked_add(f.size).ok_or(Bad::TooLarge)?;
            if big_n > CALLS_MAX_FILES || big > CALLS_MAX_BYTES {
                return Err(Bad::TooLarge);
            }
        } else {
            small_n += 1;
            small = small.checked_add(f.size).ok_or(Bad::TooLarge)?;
            if f.size > SMALL_FILE_MAX_BYTES || small_n > SMALL_MAX_FILES || small > SMALL_MAX_BYTES {
                return Err(Bad::TooLarge);
            }
        }
        kept.push((rel.to_string(), item, f.clone()));
    }
    for (_, _, f) in &kept {
        let e = zip.entries.get(&f.path).copied().ok_or(Bad::Damaged)?;
        if e.size != f.size {
            return Err(Bad::Damaged);
        }
        // Packed far tighter than any real file: a zip bomb.
        if e.size > RATIO_FREE_BYTES && e.size / e.packed.max(1) > RATIO_MAX {
            return Err(Bad::Damaged);
        }
    }
    let mut files = Vec::new();
    let mut counts: BTreeMap<&'static str, Option<usize>> = BTreeMap::new();
    let (mut settings, mut settings_text) = (None, None);
    let (mut tasks, mut total) = (0usize, 0u64);
    for (rel, item, f) in kept {
        if counted(&rel) {
            let cap = if CALL_ITEMS.contains(&item) { COUNT_FILE_MAX_BYTES } else { SMALL_FILE_MAX_BYTES };
            if f.size > cap {
                return Err(Bad::TooLarge);
            }
            let mut buf = Vec::with_capacity(f.size as usize);
            zip.copy_checked(&f.path, f.size, &f.sha256, &mut buf)?;
            if rel == "config/settings.json" {
                let text = String::from_utf8(buf).map_err(|_| Bad::Damaged)?;
                settings = Some(crate::settings_store::peek_settings_text(&text).map_err(|_| Bad::Damaged)?);
                settings_text = Some(text);
            } else {
                let v: Value = serde_json::from_slice(&buf).map_err(|_| Bad::Damaged)?;
                let n = match rel.as_str() {
                    "contacts.json" => v.get("contacts").and_then(Value::as_array).map(Vec::len),
                    "config/scam_numbers.json" => v.get("numbers").and_then(Value::as_object).map(|m| m.len()),
                    "phone_sims.json" => v.as_object().map(|m| m.len()),
                    "errands.json" => v.get("errands").and_then(Value::as_array).map(Vec::len),
                    "index.json" => v.get("calls").and_then(Value::as_array).map(Vec::len),
                    "sms/messages.json" => v.get("messages").and_then(Value::as_array).map(Vec::len),
                    _ => None,
                };
                counts.insert(item, n);
            }
        } else {
            zip.copy_checked(&f.path, f.size, &f.sha256, &mut io::sink())?;
        }
        if item == "tasks" && rel != "tasks/index.json" {
            tasks += 1;
        }
        // settings.json holds settings, keys or both: told below.
        if rel != "config/settings.json" {
            counts.entry(item).or_insert(None);
        }
        total += f.size;
        files.push(Wanted { rel, item, entry: f });
    }
    if tasks > 0 {
        counts.insert("tasks", Some(tasks));
    }
    if let Some(p) = &settings {
        if p.has_settings || manifest.items.iter().any(|i| i == "settings") {
            counts.entry("settings").or_insert(None);
        }
        // A backup of the keys says so even when none was set: restoring it clears them.
        if p.has_keys || manifest.items.iter().any(|i| i == "keys") {
            counts.entry("keys").or_insert(None);
        }
    }
    Ok(Inspected { plain: false, manifest: Some(manifest), files, counts, settings, settings_text, total_size: total })
}

/// `a` is a later version than `than` (`1.4.10` > `1.4.9`).
fn newer(a: &str, than: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        let core = v.trim().trim_start_matches('v').split(['-', '+']).next().unwrap_or("");
        core.split('.').map(|p| p.parse().unwrap_or(0)).collect()
    }
    if a.trim().is_empty() {
        return false;
    }
    let (mut x, mut y) = (parts(a), parts(than));
    let n = x.len().max(y.len());
    x.resize(n, 0);
    y.resize(n, 0);
    x > y
}

fn preview_warnings(ins: &Inspected) -> Vec<&'static str> {
    let mut w = Vec::new();
    if ins.manifest.as_ref().is_some_and(|m| newer(&m.app_version, APP_VERSION)) {
        w.push("newer_version");
    }
    if ins.plain {
        w.push("plain_settings_file");
    }
    w
}

fn preview_json(token: &str, source_name: &str, ins: &Inspected, change: crate::settings_store::KeysChange, warnings: &[&'static str]) -> Value {
    let items: Vec<Value> = ITEMS
        .iter()
        .filter_map(|id| {
            ins.counts.get(id).map(|c| match c {
                Some(n) => json!({"id": id, "count": n}),
                None => json!({"id": id}),
            })
        })
        .collect();
    let m = ins.manifest.as_ref();
    let text = |s: Option<&String>| s.filter(|s| !s.trim().is_empty()).cloned();
    json!({
        "ok": true,
        "token": token,
        "source_name": source_name,
        "created_at": text(m.map(|m| &m.created_at)),
        "app_version": text(m.map(|m| &m.app_version)),
        "kind": text(m.map(|m| &m.kind)),
        "items": items,
        "has_calls": CALL_ITEMS.iter().any(|i| ins.counts.contains_key(i)),
        "calls_count": ins.counts.get("calls").copied().flatten().unwrap_or(0),
        // Only restoring the keys changes the password (or clears it).
        "changes_password": ins.counts.contains_key("keys") && change.password,
        // Where notifications go, the forwarding number or who may give commands.
        "changes_connections": ins.counts.contains_key("keys") && change.connections,
        "total_size": ins.total_size,
        "warnings": warnings,
    })
}

// ---------------------------------------------------------------- folders

/// The default backup folder: `backups` in the data folder.
pub fn default_dir(data: &Path) -> PathBuf {
    data.join("backups")
}

/// Uploads and files being restored wait here (never in a backup).
fn staging_dir(data: &Path) -> PathBuf {
    data.join("backup-staging")
}

/// The backup folder the settings name, else the default one.
fn configured_dir(rt: &RuntimeSettings, data: &Path) -> PathBuf {
    let d = rt.backup_dir.trim();
    if !d.is_empty() && Path::new(d).is_absolute() {
        PathBuf::from(d)
    } else {
        default_dir(data)
    }
}

fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

/// A file can be written in `dir` (which exists).
fn probe_write(dir: &Path) -> bool {
    let probe = dir.join(format!(".bageo-write-test-{}.tmp", uuid::Uuid::new_v4().simple()));
    let ok = create_private(&probe).and_then(|mut f| f.write_all(b"ok")).is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// `dir` is made when missing, and a file can be written there.
pub fn dir_writable(dir: &Path) -> bool {
    std::fs::create_dir_all(dir).is_ok() && probe_write(dir)
}

/// A `backup_dir` setting that works here: empty (the default folder), or an
/// absolute path Bageo can make and write in.
pub fn dir_setting_ok(d: &str) -> bool {
    let d = d.trim();
    d.is_empty() || (Path::new(d).is_absolute() && dir_writable(Path::new(d)))
}

/// `PUT /api/settings` refused (400): a `backup_dir` backups cannot be saved
/// in (`backup_dir_unwritable`; the folder is made when it can be), a backup
/// password under 8 characters (`backup_password_short`), or encryption
/// turned on, or its password removed, with no password left
/// (`backup_password_missing`). `rt`: the settings now.
pub fn check_settings_patch(patch: &Value, rt: &RuntimeSettings) -> Option<Response> {
    let refuse = |code: &'static str| Some(api_error(StatusCode::BAD_REQUEST, code, message(code)));
    if let Some(d) = patch.get("backup_dir").and_then(Value::as_str) {
        if !dir_setting_ok(d.trim()) {
            return refuse("backup_dir_unwritable");
        }
    }
    let typed = patch.get("backup_password").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty());
    if typed.is_some_and(|p| !backup_crypt::password_ok(p)) {
        return refuse("backup_password_short");
    }
    let clearing = patch.get("backup_password_clear").and_then(Value::as_bool) == Some(true);
    let asked = patch.get("backup_encrypt").and_then(Value::as_bool);
    let encrypt = asked.unwrap_or(rt.backup_encrypt);
    let has_password = !clearing && (typed.is_some() || !rt.backup_password.trim().is_empty());
    if (asked.is_some() || clearing) && encrypt && !has_password {
        return refuse("backup_password_missing");
    }
    None
}

/// A new file at `path`, readable by its owner only (Unix). It must not be
/// there yet (`O_EXCL`): every temporary file has a random name of its own.
fn create_private(path: &Path) -> io::Result<File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

/// `from` moved to `to` (copied when they are on different disks).
fn move_into(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    let name = to.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = to.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let copied = (|| -> io::Result<()> {
        let mut out = create_private(&tmp)?;
        io::copy(&mut File::open(from)?, &mut out)?;
        out.sync_all()
    })();
    if let Err(e) = copied.and_then(|_| std::fs::rename(&tmp, to)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let _ = std::fs::remove_file(from);
    Ok(())
}

fn stage_path(stage: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(stage.to_path_buf(), |p, c| p.join(c))
}

/// Delete the oldest backups of `kind` in `dir` past the newest `keep`. Only
/// files named like backups of that kind are touched. How many went.
fn prune(dir: &Path, kind: Kind, keep: usize) -> usize {
    let mut list: Vec<(NaiveDateTime, String)> = names_in(dir)
        .into_iter()
        .filter_map(|n| match parse_name(&n) {
            Some((k, at)) if k == kind && is_plain_file(&dir.join(&n)) => Some((at, n)),
            _ => None,
        })
        .collect();
    list.sort_by(|a, b| b.cmp(a));
    let mut removed = 0;
    for (_, name) in list.into_iter().skip(keep.max(1)) {
        if std::fs::remove_file(dir.join(&name)).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// The newest automatic backup in `dir` (local time of its name).
fn last_auto(dir: &Path) -> Option<NaiveDateTime> {
    names_in(dir)
        .iter()
        .filter_map(|n| parse_name(n).filter(|(k, _)| *k == Kind::Scheduled).map(|(_, at)| at))
        .max()
}

/// One backup in the list.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub name: String,
    pub at: String,
    pub size: u64,
    pub kind: &'static str,
    pub app_version: Option<String>,
    /// Unknown (null) for an encrypted file until it is opened.
    pub include_calls: Option<bool>,
    pub encrypted: bool,
}

fn describe(path: &Path, name: String, kind: Kind, at: NaiveDateTime) -> Entry {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    // An encrypted file says nothing before its password.
    if is_sealed_name(&name) {
        return Entry { at: local_rfc3339(at), app_version: None, include_calls: None, encrypted: true, name, size, kind: kind.as_str() };
    }
    let manifest: Option<Value> = File::open(path)
        .ok()
        .and_then(|f| ZipIn::open(f).ok())
        .and_then(|mut z| z.read_entry(MANIFEST_NAME, MANIFEST_MAX_BYTES).ok().flatten())
        .and_then(|b| serde_json::from_slice(&b).ok());
    let field = |k: &str| manifest.as_ref().and_then(|m| m.get(k)).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    Entry {
        at: field("created_at").unwrap_or_else(|| local_rfc3339(at)),
        app_version: field("app_version"),
        include_calls: Some(manifest.as_ref().and_then(|m| m.get("include_calls")).and_then(Value::as_bool).unwrap_or(false)),
        encrypted: false,
        name,
        size,
        kind: kind.as_str(),
    }
}

/// The backups in `dir`, newest first.
fn list_backups(dir: &Path) -> Vec<Entry> {
    let mut v: Vec<(NaiveDateTime, Entry)> = names_in(dir)
        .into_iter()
        .filter_map(|n| {
            let (kind, at) = parse_name(&n)?;
            let path = dir.join(&n);
            is_plain_file(&path).then(|| (at, describe(&path, n, kind, at)))
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.name.cmp(&a.1.name)));
    v.into_iter().map(|(_, e)| e).collect()
}

// ---------------------------------------------------------------- making backups

/// Why a backup could not be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunErr {
    Busy,
    DirUnwritable,
    DiskFull,
    Io,
}

impl RunErr {
    pub fn code(self) -> &'static str {
        match self {
            RunErr::Busy => "busy",
            RunErr::DirUnwritable => "dir_unwritable",
            RunErr::DiskFull => "disk_full",
            RunErr::Io => "io",
        }
    }
}

fn run_err(e: &io::Error, opening: bool) -> RunErr {
    let full_code = if cfg!(windows) { matches!(e.raw_os_error(), Some(39) | Some(112)) } else { e.raw_os_error() == Some(28) };
    if e.kind() == io::ErrorKind::StorageFull || full_code {
        return RunErr::DiskFull;
    }
    if opening || matches!(e.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem | io::ErrorKind::NotFound) {
        return RunErr::DirUnwritable;
    }
    RunErr::Io
}

/// The backup zip of the parts `want` written into `file` (`settings_json`:
/// the settings file as saved); the parts it holds.
fn write_backup(lay: &Layout, settings_json: &[u8], file: File, kind: Kind, want: &[&'static str], at: DateTime<Local>) -> io::Result<Vec<&'static str>> {
    let (json, settings_items) = settings_for(settings_json, want);
    let sources = collect(lay, json, want);
    let (out, held) = write_zip(BufWriter::new(file), sources, kind, &settings_items, at)?;
    let file = out.into_inner().map_err(|e| e.into_error())?;
    file.sync_all()?;
    Ok(held)
}

fn crypt_io(e: CryptErr) -> io::Error {
    match e {
        CryptErr::Io(e) => e,
        _ => io::Error::other("encrypting failed"),
    }
}

/// [`write_backup`] sealed with `password` into `out`: the zip is written to
/// the staging folder first (it needs to seek), then encrypted a chunk at a
/// time, then deleted.
fn write_sealed(lay: &Layout, settings_json: &[u8], out: File, kind: Kind, want: &[&'static str], at: DateTime<Local>, password: &str) -> io::Result<Vec<&'static str>> {
    let stage = staging_dir(&lay.data);
    std::fs::create_dir_all(&stage)?;
    let plain = stage.join(format!("seal-{}.zip", uuid::Uuid::new_v4().simple()));
    let made = (|| -> io::Result<Vec<&'static str>> {
        let held = write_backup(lay, settings_json, create_private(&plain)?, kind, want, at)?;
        let mut w = BufWriter::new(out);
        backup_crypt::seal(password, &mut BufReader::new(File::open(&plain)?), &mut w).map_err(crypt_io)?;
        w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        Ok(held)
    })();
    let _ = std::fs::remove_file(&plain);
    made
}

/// `tmp` named `to`, unless a file already has that name (false then).
fn publish(tmp: &Path, to: &Path) -> io::Result<bool> {
    match std::fs::hard_link(tmp, to) {
        Ok(()) => {
            let _ = std::fs::remove_file(tmp);
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        // A disk without hard links: the name is looked at, then taken.
        Err(_) if to.exists() => Ok(false),
        Err(_) => std::fs::rename(tmp, to).map(|_| true),
    }
}

/// A backup file of the parts `want` in `dir` (mode 600 on Unix), encrypted
/// with `seal` when there is one: written under a random name, then named for
/// its time (a second later while another file has that name).
fn write_backup_file(
    lay: &Layout,
    settings_json: Vec<u8>,
    dir: &Path,
    kind: Kind,
    want: &[&'static str],
    now: DateTime<Local>,
    seal: Option<&str>,
) -> Result<Entry, RunErr> {
    std::fs::create_dir_all(dir).map_err(|e| run_err(&e, true))?;
    let tmp = dir.join(format!(".bageo-{}.tmp", uuid::Uuid::new_v4().simple()));
    let file = create_private(&tmp).map_err(|e| run_err(&e, true))?;
    let written = match seal {
        Some(pw) => write_sealed(lay, &settings_json, file, kind, want, now, pw),
        None => write_backup(lay, &settings_json, file, kind, want, now),
    };
    let held = match written {
        Ok(held) => held,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(run_err(&e, false));
        }
    };
    let mut at = now;
    let name = loop {
        let name = file_name_as(kind, at.naive_local(), seal.is_some());
        match publish(&tmp, &dir.join(&name)) {
            Ok(true) => break name,
            Ok(false) if at < now + chrono::Duration::seconds(60) => at += chrono::Duration::seconds(1),
            Ok(false) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(RunErr::Io);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(run_err(&e, false));
            }
        }
    };
    let size = std::fs::metadata(dir.join(&name)).map(|m| m.len()).unwrap_or(0);
    Ok(Entry {
        name,
        at: now.to_rfc3339_opts(SecondsFormat::Secs, false),
        size,
        kind: kind.as_str(),
        app_version: Some(APP_VERSION.to_string()),
        include_calls: Some(held.iter().any(|i| CALL_ITEMS.contains(i))),
        encrypted: seal.is_some(),
    })
}

/// What backups hold: [`DEFAULT_ITEMS`], with the call records when asked.
fn backup_items(with_calls: bool) -> Vec<&'static str> {
    let mut v = DEFAULT_ITEMS.to_vec();
    if with_calls {
        v.extend(CALL_ITEMS);
    }
    v
}

/// The password backups are encrypted with now: none unless `backup_encrypt`.
fn backup_seal(rt: &RuntimeSettings) -> Option<String> {
    let pw = rt.backup_password.trim();
    (rt.backup_encrypt && !pw.is_empty()).then(|| pw.to_string())
}

async fn create_in(state: &CallsState, lay: &Layout, dir: &Path, kind: Kind, want: Vec<&'static str>, seal: Option<String>) -> Result<Entry, RunErr> {
    let settings_json = state.settings_store.overrides_json();
    let (lay, dir, now) = (lay.clone(), dir.to_path_buf(), Local::now());
    tokio::task::spawn_blocking(move || write_backup_file(&lay, settings_json, &dir, kind, &want, now, seal.as_deref()))
        .await
        .unwrap_or(Err(RunErr::Io))
}

/// The last backup tried, for the console.
#[derive(Debug, Clone, Serialize)]
struct LastRun {
    at: String,
    ok: bool,
    name: Option<String>,
    size: u64,
    error: Option<&'static str>,
}

#[derive(Default)]
struct Inner {
    last: Option<LastRun>,
    /// When the last automatic backup failed (local time); cleared by one that works.
    failed_at: Option<NaiveDateTime>,
    /// The failure the owner was told about.
    notified: Option<&'static str>,
}

/// One data folder's backups: one at a time, and how the last ones went.
#[derive(Default)]
struct DirState {
    running: tokio::sync::Mutex<()>,
    inner: Mutex<Inner>,
    /// Uploads under way ([`UPLOADS_MAX`]).
    uploads: AtomicUsize,
}

impl DirState {
    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

static DIRS: LazyLock<Mutex<HashMap<PathBuf, Arc<DirState>>>> = LazyLock::new(Default::default);

fn dir_state(data: &Path) -> Arc<DirState> {
    DIRS.lock().unwrap_or_else(|e| e.into_inner()).entry(data.to_path_buf()).or_default().clone()
}

/// A backup now ("Back up now" or the schedule), in the backup folder; with the
/// call records when the settings say so. Automatic ones past `backup_keep` go.
pub(crate) async fn run_backup(state: &CallsState, kind: Kind) -> Result<Entry, RunErr> {
    let lay = Layout::of(state);
    let ds = dir_state(&lay.data);
    let Ok(_running) = ds.running.try_lock() else {
        return Err(RunErr::Busy);
    };
    let rt = state.settings_store.snapshot();
    let dir = configured_dir(&rt, &lay.data);
    let res = create_in(state, &lay, &dir, kind, backup_items(rt.backup_include_calls), backup_seal(&rt)).await;
    let at = Local::now().to_rfc3339_opts(SecondsFormat::Secs, false);
    ds.inner().last = Some(match &res {
        Ok(e) => LastRun { at, ok: true, name: Some(e.name.clone()), size: e.size, error: None },
        Err(e) => LastRun { at, ok: false, name: None, size: 0, error: Some(e.code()) },
    });
    match &res {
        Ok(e) => {
            info!("backup made kind={} size={} with_calls={} encrypted={}", kind.as_str(), e.size, e.include_calls == Some(true), e.encrypted);
            if kind == Kind::Scheduled {
                let gone = prune(&dir, Kind::Scheduled, rt.backup_keep as usize);
                if gone > 0 {
                    info!("old automatic backups removed count={gone}");
                }
            }
        }
        Err(e) => warn!("backup failed kind={} error={}", kind.as_str(), e.code()),
    }
    res
}

/// The file "Export" downloads, of the parts `want` (encrypted with
/// `password` when there is one): written to a temporary file (it can hold
/// every recording); its download name and that file.
pub(crate) async fn export_file(state: &CallsState, want: Vec<&'static str>, password: Option<String>) -> Result<(String, PathBuf), RunErr> {
    let lay = Layout::of(state);
    let settings_json = state.settings_store.overrides_json();
    let now = Local::now();
    tokio::task::spawn_blocking(move || -> Result<(String, PathBuf), RunErr> {
        let stage = staging_dir(&lay.data);
        std::fs::create_dir_all(&stage).map_err(|e| run_err(&e, true))?;
        sweep_exports(&stage);
        // A name of its own: exports at the same moment never meet.
        let ext = if password.is_some() { SEALED_EXT } else { ".zip" };
        let path = stage.join(format!("export-{}{ext}", uuid::Uuid::new_v4().simple()));
        let file = create_private(&path).map_err(|e| run_err(&e, true))?;
        let written = match password.as_deref() {
            Some(pw) => write_sealed(&lay, &settings_json, file, Kind::Export, &want, now, pw),
            None => write_backup(&lay, &settings_json, file, Kind::Export, &want, now),
        };
        if let Err(e) = written {
            let _ = std::fs::remove_file(&path);
            return Err(run_err(&e, false));
        }
        Ok((file_name_as(Kind::Export, now.naive_local(), password.is_some()), path))
    })
    .await
    .unwrap_or(Err(RunErr::Io))
}

/// Exports a download left behind (it was cut off), and copies being sealed
/// or opened when Bageo stopped, go after an hour.
fn sweep_exports(stage: &Path) {
    for name in names_in(stage) {
        let path = stage.join(&name);
        let old = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > Duration::from_secs(3600));
        if ["export-", "seal-"].iter().any(|p| name.starts_with(p)) && old {
            let _ = std::fs::remove_file(&path);
        }
    }
}

// ---------------------------------------------------------------- schedule

/// When an automatic backup made at `last` is due again: a day, a week or a
/// calendar month later (Jan 31 → Feb 28). None: `off`.
pub fn next_due(schedule: &str, last: NaiveDateTime) -> Option<NaiveDateTime> {
    match schedule {
        "daily" => Some(last + chrono::Duration::days(1)),
        "weekly" => Some(last + chrono::Duration::days(7)),
        "monthly" => last.checked_add_months(Months::new(1)),
        _ => None,
    }
}

/// When the next automatic backup runs: when one is due (now when there was
/// none), but not sooner than an hour after one failed.
fn next_run(schedule: &str, last: Option<NaiveDateTime>, failed_at: Option<NaiveDateTime>, now: NaiveDateTime) -> Option<NaiveDateTime> {
    let due = match last {
        Some(l) => next_due(schedule, l)?,
        None => next_due(schedule, now).map(|_| now)?,
    };
    Some(match failed_at {
        Some(f) => due.max(f + chrono::Duration::hours(RETRY_AFTER_HOURS)),
        None => due,
    })
}

/// Look every 10 minutes whether an automatic backup is due (the first look a
/// minute after start, so one missed while the computer was off is made then).
pub fn spawn(state: CallsState) {
    let lay = Layout::of(&state);
    let _ = std::fs::remove_dir_all(staging_dir(&lay.data));
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let mut tick = tokio::time::interval(CHECK_EVERY);
        loop {
            tick.tick().await;
            // Downloads and previews nobody came back for go with their files.
            sweep_tickets();
            sweep_pending();
            scheduled_tick(&state, Local::now()).await;
        }
    });
}

/// One look at the schedule; None when no backup was due (or a call is on: it
/// waits for the next look). A failure is told once until a backup works again.
pub(crate) async fn scheduled_tick(state: &CallsState, now: DateTime<Local>) -> Option<Result<Entry, RunErr>> {
    let rt = state.settings_store.snapshot();
    let lay = Layout::of(state);
    let dir = configured_dir(&rt, &lay.data);
    let ds = dir_state(&lay.data);
    let failed_at = ds.inner().failed_at;
    let last = tokio::task::spawn_blocking(move || last_auto(&dir)).await.ok().flatten();
    let due = next_run(&rt.backup_schedule, last, failed_at, now.naive_local())?;
    if now.naive_local() < due || !state.store.live_calls().await.is_empty() {
        return None;
    }
    let res = run_backup(state, Kind::Scheduled).await;
    match &res {
        Ok(_) => {
            let mut i = ds.inner();
            i.failed_at = None;
            i.notified = None;
        }
        Err(RunErr::Busy) => {}
        Err(e) => {
            let code = e.code();
            let tell = {
                let mut i = ds.inner();
                i.failed_at = Some(now.naive_local());
                let tell = i.notified != Some(code);
                i.notified = Some(code);
                tell
            };
            if tell {
                notify_failure(state, code).await;
            }
        }
    }
    Some(res)
}

/// The notification about an automatic backup that failed (`code`: `dir_unwritable`,
/// `disk_full` or `io`), in the owner's language.
pub fn failure_text(lang: Lang, code: &str) -> (&'static str, String) {
    let (title, folder, full, other, tail) = match lang {
        Lang::ZhTw => (
            "Bageo 自動備份沒有完成",
            "存放備份的資料夾目前無法寫入，可能被移走、改名，或外接硬碟沒有接上。",
            "存放備份的磁碟空間不夠了。",
            "存檔時發生問題。",
            "Bageo 每小時會再試一次。請到控制台的「備份」看看存放位置。",
        ),
        Lang::ZhCn => (
            "Bageo 自动备份没有完成",
            "存放备份的文件夹目前无法写入，可能被移走、改名，或外接硬盘没有接上。",
            "存放备份的磁盘空间不够了。",
            "保存时出了问题。",
            "Bageo 每小时会再试一次。请到控制台的“备份”看看存放位置。",
        ),
        Lang::En => (
            "Bageo couldn't finish the automatic backup",
            "The backup folder can't be written to right now. It may have been moved or renamed, or an external drive isn't connected.",
            "The disk that holds the backups is full.",
            "Something went wrong while saving.",
            "Bageo will try again every hour. Check where backups are saved under Backup in the console.",
        ),
        Lang::Ja => (
            "Bageo の自動バックアップが完了しませんでした",
            "バックアップの保存先フォルダーに書き込めません。移動や名前の変更をしたか、外付けドライブがつながっていない可能性があります。",
            "バックアップを保存するディスクの空きが足りません。",
            "保存中に問題が起きました。",
            "Bageo は 1 時間ごとにやり直します。コンソールの「バックアップ」で保存先を確認してください。",
        ),
        Lang::Ko => (
            "Bageo 자동 백업이 완료되지 않았습니다",
            "백업 폴더에 지금 저장할 수 없습니다. 폴더를 옮기거나 이름을 바꿨거나, 외장 드라이브가 연결되지 않았을 수 있습니다.",
            "백업을 저장하는 디스크 공간이 부족합니다.",
            "저장하는 중에 문제가 생겼습니다.",
            "Bageo가 1시간마다 다시 시도합니다. 콘솔의 '백업'에서 저장 위치를 확인해 주세요.",
        ),
        Lang::Es => (
            "Bageo no pudo terminar la copia de seguridad automática",
            "No se puede guardar en la carpeta de copias ahora mismo. Puede que se haya movido o renombrado, o que un disco externo no esté conectado.",
            "El disco donde se guardan las copias está lleno.",
            "Algo salió mal al guardar.",
            "Bageo lo volverá a intentar cada hora. Revisa dónde se guardan las copias en Copia de seguridad, en la consola.",
        ),
        Lang::Fr => (
            "Bageo n'a pas pu terminer la sauvegarde automatique",
            "Impossible d'écrire dans le dossier des sauvegardes pour l'instant. Il a peut-être été déplacé ou renommé, ou un disque externe n'est pas branché.",
            "Le disque des sauvegardes est plein.",
            "Un problème est survenu pendant l'enregistrement.",
            "Bageo réessaiera toutes les heures. Vérifiez l'emplacement des sauvegardes dans Sauvegarde, sur la console.",
        ),
        Lang::De => (
            "Bageo konnte die automatische Sicherung nicht abschließen",
            "In den Sicherungsordner kann gerade nicht geschrieben werden. Er wurde vielleicht verschoben oder umbenannt, oder ein externes Laufwerk ist nicht angeschlossen.",
            "Die Festplatte für die Sicherungen ist voll.",
            "Beim Speichern ist ein Fehler aufgetreten.",
            "Bageo versucht es jede Stunde erneut. Prüfen Sie den Speicherort unter Sicherung in der Konsole.",
        ),
        Lang::Pt => (
            "O Bageo não conseguiu concluir o backup automático",
            "Não é possível gravar na pasta de backups agora. Ela pode ter sido movida ou renomeada, ou um disco externo não está conectado.",
            "O disco onde ficam os backups está cheio.",
            "Algo deu errado ao salvar.",
            "O Bageo vai tentar de novo a cada hora. Confira onde os backups são salvos em Backup, no console.",
        ),
    };
    let why = match code {
        "dir_unwritable" => folder,
        "disk_full" => full,
        _ => other,
    };
    let sep = if matches!(lang, Lang::ZhTw | Lang::ZhCn | Lang::Ja) { "" } else { " " };
    (title, format!("{why}{sep}{tail}"))
}

async fn notify_failure(state: &CallsState, code: &'static str) {
    let rt = state.settings_store.snapshot();
    let (title, body) = failure_text(rt.notify_lang(), code);
    let eff = rt.effective(&state.settings);
    let sent = crate::notify::notify_event(&state.http, &eff, title, &body).await;
    info!("backup failure notice error={code} sent={sent}");
}

// ---------------------------------------------------------------- restoring

type ApiErr = (StatusCode, &'static str);

fn bad_err(e: Bad) -> ApiErr {
    match e {
        Bad::NotBackup => (StatusCode::BAD_REQUEST, "not_backup"),
        Bad::Damaged => (StatusCode::BAD_REQUEST, "damaged"),
        Bad::TooNew => (StatusCode::BAD_REQUEST, "too_new"),
        Bad::TooLarge => (StatusCode::BAD_REQUEST, "too_large"),
        Bad::Io(e) => {
            warn!("backup: file error kind={:?}", e.kind());
            (StatusCode::INTERNAL_SERVER_ERROR, "io")
        }
    }
}

/// A preview waiting for "Restore".
struct Pending {
    /// The data folder it is for.
    data: PathBuf,
    /// The file as uploaded or picked from the list (encrypted or not).
    file: PathBuf,
    /// An upload (deleted when done); a file of the backup folder stays.
    owned: bool,
    /// What it takes in the staging folder: an upload, or an upload's opened copy.
    bytes: u64,
    made: Instant,
    /// An encrypted file.
    encrypted: bool,
    /// Its opened copy in the staging folder (deleted when done).
    opened: Option<PathBuf>,
    /// What it holds; None while an encrypted file waits for its password.
    ins: Option<Inspected>,
    warnings: Vec<&'static str>,
    source_name: String,
    /// Wrong passwords so far, when the last one was tried, one being tried now.
    fails: u32,
    tried: Option<Instant>,
    trying: bool,
}

impl Pending {
    fn new(data: PathBuf, file: PathBuf, owned: bool, bytes: u64, source_name: &str) -> Pending {
        Pending {
            data,
            file,
            owned,
            bytes,
            made: Instant::now(),
            encrypted: false,
            opened: None,
            ins: None,
            warnings: Vec::new(),
            source_name: source_name.to_string(),
            fails: 0,
            tried: None,
            trying: false,
        }
    }

    fn with(mut self, ins: Inspected) -> Pending {
        self.warnings = preview_warnings(&ins);
        self.ins = Some(ins);
        self
    }

    /// The file a restore reads: the opened copy of an encrypted one.
    fn source(&self) -> &Path {
        self.opened.as_deref().unwrap_or(&self.file)
    }
}

static PENDING: LazyLock<Mutex<HashMap<String, Pending>>> = LazyLock::new(Default::default);

fn pending() -> MutexGuard<'static, HashMap<String, Pending>> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner())
}

fn drop_pending_file(p: &Pending) {
    if p.owned {
        let _ = std::fs::remove_file(&p.file);
    }
    if let Some(o) = &p.opened {
        let _ = std::fs::remove_file(o);
    }
}

/// Forget previews older than 15 minutes (and their uploads).
fn sweep_pending() {
    let gone: Vec<Pending> = {
        let mut map = pending();
        let old: Vec<String> = map.iter().filter(|(_, p)| p.made.elapsed() >= TOKEN_TTL).map(|(k, _)| k.clone()).collect();
        old.into_iter().filter_map(|k| map.remove(&k)).collect()
    };
    for p in &gone {
        drop_pending_file(p);
    }
}

/// The previews waiting for the data folder `data`, and what they take in the staging folder.
fn pending_load(data: &Path) -> (usize, u64) {
    pending().values().filter(|p| p.data == data).fold((0, 0), |(n, b), p| (n + 1, b + p.bytes))
}

/// The oldest previews of the data folder `data` taken out of `map` (but
/// `except`) until at most `keep` wait there and what they take leaves room
/// for `bytes` more in the staging folder. True: that much room is there now.
fn take_oldest(map: &mut HashMap<String, Pending>, data: &Path, keep: usize, bytes: u64, except: Option<&str>) -> (Vec<Pending>, bool) {
    let mut mine: Vec<(Instant, String, u64)> = map.iter().filter(|(_, p)| p.data == data).map(|(k, p)| (p.made, k.clone(), p.bytes)).collect();
    mine.sort();
    let (mut n, mut staged) = (mine.len(), mine.iter().map(|m| m.2).sum::<u64>());
    let room = |n: usize, staged: u64| n <= keep && staged.saturating_add(bytes) <= STAGING_MAX_BYTES;
    let mut gone = Vec::new();
    for (_, k, b) in mine {
        if room(n, staged) {
            break;
        }
        if except == Some(k.as_str()) {
            continue;
        }
        if let Some(p) = map.remove(&k) {
            gone.push(p);
            n -= 1;
            staged -= b;
        }
    }
    (gone, room(n, staged))
}

/// Room for one more preview of `data` (or, with `except`, for that one to
/// grow) taking `bytes` in the staging folder: the oldest ones waiting go (a closed
/// dialog may never have said so). False: there is no such room.
fn make_room(data: &Path, bytes: u64, except: Option<&str>) -> bool {
    let keep = if except.is_some() { PENDING_MAX } else { PENDING_MAX - 1 };
    let (gone, ok) = take_oldest(&mut pending(), data, keep, bytes, except);
    for p in &gone {
        drop_pending_file(p);
    }
    ok
}

/// Forget the preview `token` of the data folder `data` and its upload (the
/// dialog was closed). One that is not there: nothing to do.
pub(crate) fn discard(data: &Path, token: &str) {
    let gone = {
        let mut map = pending();
        match map.get(token) {
            Some(p) if p.data == data => map.remove(token),
            _ => None,
        }
    };
    if let Some(p) = gone {
        drop_pending_file(&p);
    }
}

/// The answer for the preview `token`: what it holds, or that its password is needed.
fn preview_of(state: &CallsState, token: &str, p: &Pending) -> Value {
    let Some(ins) = &p.ins else {
        return json!({"ok": true, "token": token, "source_name": p.source_name, "encrypted": true, "needs_password": true});
    };
    let change = match ins.settings_text.as_deref() {
        Some(t) if ins.counts.contains_key("keys") => state.settings_store.keys_change(t, whole_keys(p, ins)),
        _ => Default::default(),
    };
    let mut v = preview_json(token, &p.source_name, ins, change, &p.warnings);
    v["encrypted"] = json!(p.encrypted);
    v["needs_password"] = json!(false);
    v
}

/// The preview is this computer's own `before_import` copy, picked from the
/// backup folder (an upload never is): its keys come back exactly as they were.
fn whole_keys(p: &Pending, ins: &Inspected) -> bool {
    !p.owned
        && ins.manifest.as_ref().is_some_and(|m| m.kind == "before_import")
        && p.file.file_name().and_then(|n| n.to_str()).and_then(parse_name).is_some_and(|(k, _)| k == Kind::BeforeImport)
}

/// Wrong passwords in a row across every preview, and when the last try was:
/// a new preview does not start the count again.
static GUESSES: std::sync::Mutex<(u32, Option<Instant>)> = std::sync::Mutex::new((0, None));
/// One password try at a time, whichever preview it is for.
static UNLOCKING: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

/// How long the next password try still waits.
fn guess_wait() -> Duration {
    let (fails, last) = *GUESSES.lock().unwrap_or_else(|e| e.into_inner());
    let Some(last) = last else {
        return Duration::ZERO;
    };
    let gap = if fails < UNLOCK_TRIES {
        UNLOCK_GAP
    } else {
        UNLOCK_GAP.saturating_mul(1u32 << (fails - UNLOCK_TRIES + 1).min(6)).min(UNLOCK_WAIT_MAX)
    };
    gap.saturating_sub(last.elapsed())
}

fn guessed(right: bool) {
    let mut g = GUESSES.lock().unwrap_or_else(|e| e.into_inner());
    g.0 = if right { 0 } else { g.0.saturating_add(1) };
    g.1 = Some(Instant::now());
}

fn register(state: &CallsState, p: Pending) -> Value {
    let token = uuid::Uuid::new_v4().simple().to_string();
    let v = preview_of(state, &token, &p);
    let data = p.data.clone();
    // Two at once may both have made room: the oldest still goes.
    let gone = {
        let mut map = pending();
        map.insert(token.clone(), p);
        take_oldest(&mut map, &data, PENDING_MAX, 0, Some(&token)).0
    };
    for p in &gone {
        drop_pending_file(p);
    }
    v
}

/// One upload under way (at most [`UPLOADS_MAX`] for a data folder).
struct UploadSlot(Arc<DirState>);

impl Drop for UploadSlot {
    fn drop(&mut self) {
        self.0.uploads.fetch_sub(1, Ordering::SeqCst);
    }
}

fn upload_slot(ds: Arc<DirState>) -> Option<UploadSlot> {
    if ds.uploads.fetch_add(1, Ordering::SeqCst) >= UPLOADS_MAX {
        ds.uploads.fetch_sub(1, Ordering::SeqCst);
        return None;
    }
    Some(UploadSlot(ds))
}

/// The upload `body` written to the new file `file`: its size. At most
/// [`IMPORT_MAX_BYTES`], and no more than the staging folder may still take.
async fn receive<S, E>(file: &Path, body: &mut S, staged: u64) -> Result<u64, ApiErr>
where
    S: futures_util::Stream<Item = Result<Bytes, E>> + Unpin,
{
    use futures_util::StreamExt as _;
    use tokio::io::AsyncWriteExt as _;
    let io_err = |_: io::Error| (StatusCode::INTERNAL_SERVER_ERROR, "io");
    if let Some(dir) = file.parent() {
        tokio::fs::create_dir_all(dir).await.map_err(io_err)?;
    }
    let path = file.to_path_buf();
    let made = tokio::task::spawn_blocking(move || create_private(&path)).await.map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "io"))?;
    let mut out = tokio::fs::File::from_std(made.map_err(io_err)?);
    let mut n = 0u64;
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|_| (StatusCode::BAD_REQUEST, "bad_request"))?;
        n += chunk.len() as u64;
        if n > IMPORT_MAX_BYTES as u64 {
            return Err((StatusCode::BAD_REQUEST, "too_large"));
        }
        if staged + n > STAGING_MAX_BYTES {
            return Err((StatusCode::CONFLICT, "busy"));
        }
        out.write_all(&chunk).await.map_err(io_err)?;
    }
    out.sync_all().await.map_err(io_err)?;
    Ok(n)
}

/// What a file is: an encrypted backup (its header checked; the rest waits
/// for the password) or one read and checked whole ([`inspect`]).
enum Look {
    Sealed,
    Open(Box<Inspected>),
}

fn crypt_bad(e: CryptErr) -> Bad {
    match e {
        CryptErr::Io(e) => Bad::Io(e),
        CryptErr::TooNew => Bad::TooNew,
        CryptErr::WrongPassword | CryptErr::Damaged => Bad::Damaged,
    }
}

fn look(path: &Path) -> Result<Look, Bad> {
    let mut f = File::open(path)?;
    let mut head = Vec::with_capacity(backup_crypt::MAGIC.len());
    (&mut f).take(backup_crypt::MAGIC.len() as u64).read_to_end(&mut head)?;
    if backup_crypt::is_sealed(&head) {
        f.seek(SeekFrom::Start(0))?;
        backup_crypt::check_header(&mut f).map_err(crypt_bad)?;
        return Ok(Look::Sealed);
    }
    inspect(path).map(|ins| Look::Open(Box::new(ins)))
}

async fn look_at(path: &Path) -> Result<Look, Bad> {
    let p = path.to_path_buf();
    tokio::task::spawn_blocking(move || look(&p)).await.unwrap_or_else(|_| Err(Bad::Io(io::Error::other("preview task"))))
}

/// Why an encrypted backup was not opened.
enum OpenErr {
    WrongPassword,
    Bad(Bad),
    Api(ApiErr),
}

impl From<CryptErr> for OpenErr {
    fn from(e: CryptErr) -> Self {
        match e {
            CryptErr::WrongPassword => OpenErr::WrongPassword,
            e => OpenErr::Bad(crypt_bad(e)),
        }
    }
}

impl From<io::Error> for OpenErr {
    fn from(e: io::Error) -> Self {
        OpenErr::Bad(Bad::Io(e))
    }
}

/// The encrypted backup `src` opened with `password` into the staging
/// folder of `data` and checked: the opened copy and what it holds. Fixed memory
/// whatever its size; the disk must have room for the copy.
async fn open_sealed(data: &Path, src: &Path, password: &str) -> Result<(PathBuf, Inspected), OpenErr> {
    let stage = staging_dir(data);
    std::fs::create_dir_all(&stage)?;
    let size = std::fs::metadata(src)?.len();
    if free_space(&stage).is_some_and(|free| free < size.saturating_add(DISK_MARGIN_BYTES)) {
        return Err(OpenErr::Api((StatusCode::INTERNAL_SERVER_ERROR, "disk_full")));
    }
    let out = stage.join(format!("open-{}.zip", uuid::Uuid::new_v4().simple()));
    let (src, pw, to) = (src.to_path_buf(), password.to_string(), out.clone());
    let done = tokio::task::spawn_blocking(move || -> Result<Inspected, OpenErr> {
        let mut w = BufWriter::new(create_private(&to)?);
        backup_crypt::open(&pw, &mut BufReader::new(File::open(&src)?), &mut w)?;
        w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        inspect(&to).map_err(OpenErr::Bad)
    })
    .await
    .unwrap_or_else(|_| Err(OpenErr::Bad(Bad::Io(io::Error::other("open task")))));
    match done {
        Ok(ins) => Ok((out, ins)),
        Err(e) => {
            let _ = std::fs::remove_file(&out);
            Err(e)
        }
    }
}

/// The preview of an uploaded file read from `body` (`declared`: its
/// Content-Length), kept for 15 minutes. Nothing changes yet. At most
/// [`PENDING_MAX`] previews wait (the oldest goes for a new one) and
/// [`UPLOADS_MAX`] uploads run at once. An encrypted file waits for its password.
pub(crate) async fn preview_upload_stream<S, E>(state: &CallsState, source_name: &str, declared: Option<u64>, mut body: S) -> Result<Value, ApiErr>
where
    S: futures_util::Stream<Item = Result<Bytes, E>> + Unpin,
{
    sweep_pending();
    if declared.is_some_and(|n| n > IMPORT_MAX_BYTES as u64) {
        return Err((StatusCode::BAD_REQUEST, "too_large"));
    }
    let data = PathBuf::from(&state.settings.calls_data_dir);
    let Some(_slot) = upload_slot(dir_state(&data)) else {
        return Err((StatusCode::CONFLICT, "busy"));
    };
    if !make_room(&data, declared.unwrap_or(0), None) {
        return Err((StatusCode::CONFLICT, "busy"));
    }
    let staged = pending_load(&data).1;
    let file = staging_dir(&data).join(format!("upload-{}.bin", uuid::Uuid::new_v4().simple()));
    let bytes = match receive(&file, &mut body, staged).await {
        Ok(0) => Err((StatusCode::BAD_REQUEST, "not_backup")),
        other => other,
    };
    let bytes = match bytes {
        Ok(n) => n,
        Err(e) => {
            let _ = std::fs::remove_file(&file);
            return Err(e);
        }
    };
    let p = Pending::new(data, file.clone(), true, bytes, source_name);
    match look_at(&file).await {
        Ok(Look::Open(ins)) => Ok(register(state, p.with(*ins))),
        Ok(Look::Sealed) => Ok(register(state, Pending { encrypted: true, ..p })),
        Err(e) => {
            let _ = std::fs::remove_file(&file);
            Err(bad_err(e))
        }
    }
}

/// [`preview_upload_stream`] of a body already read.
pub(crate) async fn preview_upload(state: &CallsState, source_name: &str, body: Bytes) -> Result<Value, ApiErr> {
    let declared = Some(body.len() as u64);
    preview_upload_stream(state, source_name, declared, futures_util::stream::iter([Ok::<Bytes, io::Error>(body)])).await
}

/// The preview of a file in the backup folder. An encrypted one is opened with
/// the backup password saved here when that works; else it waits for one.
pub(crate) async fn preview_saved(state: &CallsState, name: &str) -> Result<Value, ApiErr> {
    sweep_pending();
    let rt = state.settings_store.snapshot();
    let data = PathBuf::from(&state.settings.calls_data_dir);
    let path = configured_dir(&rt, &data).join(name);
    if parse_name(name).is_none() || !is_plain_file(&path) {
        return Err((StatusCode::NOT_FOUND, "not_found"));
    }
    if !make_room(&data, 0, None) {
        return Err((StatusCode::CONFLICT, "busy"));
    }
    let p = Pending::new(data.clone(), path.clone(), false, 0, name);
    match look_at(&path).await.map_err(bad_err)? {
        Look::Open(ins) => Ok(register(state, p.with(*ins))),
        Look::Sealed => {
            let mut p = Pending { encrypted: true, ..p };
            let saved = rt.backup_password.trim();
            if !saved.is_empty() {
                match open_sealed(&data, &path, saved).await {
                    Ok((opened, ins)) => {
                        p.opened = Some(opened);
                        p = p.with(ins);
                    }
                    // Another password: the owner types it.
                    Err(OpenErr::WrongPassword) => {}
                    Err(OpenErr::Bad(e)) => return Err(bad_err(e)),
                    Err(OpenErr::Api(e)) => return Err(e),
                }
            }
            Ok(register(state, p))
        }
    }
}

/// Open the encrypted file of the preview `token` with `password`: its
/// preview. `wrong_password`; after [`UNLOCK_TRIES`] of them the preview goes
/// (`expired`). One try at a time over every preview, at least [`UNLOCK_GAP`]
/// apart, and longer after wrong ones in a row ([`guess_wait`]).
pub(crate) async fn unlock(state: &CallsState, token: &str, password: &str) -> Result<Value, ApiErr> {
    sweep_pending();
    let data = PathBuf::from(&state.settings.calls_data_dir);
    let expired = (StatusCode::BAD_REQUEST, "expired");
    let (src, upload, wait) = {
        let mut map = pending();
        let Some(p) = map.get_mut(token).filter(|p| p.data == data && p.made.elapsed() < TOKEN_TTL) else {
            return Err(expired);
        };
        if p.ins.is_some() {
            return Ok(preview_of(state, token, p));
        }
        if p.trying {
            return Err((StatusCode::CONFLICT, "busy"));
        }
        p.trying = true;
        let wait = p.tried.map(|t| UNLOCK_GAP.saturating_sub(t.elapsed())).unwrap_or_default();
        (p.file.clone(), p.owned.then_some(p.bytes), wait)
    };
    let release = || {
        if let Some(p) = pending().get_mut(token) {
            p.trying = false;
        }
    };
    // One try at a time across every preview, spaced by the wrong ones before.
    let _one = UNLOCKING.get_or_init(|| tokio::sync::Mutex::new(())).lock().await;
    tokio::time::sleep(wait.max(guess_wait())).await;
    // The opened copy of an upload takes about as much as the upload.
    if let Some(size) = upload {
        if !make_room(&data, size, Some(token)) {
            release();
            return Err((StatusCode::CONFLICT, "busy"));
        }
    }
    let res = open_sealed(&data, &src, password).await;
    match &res {
        Ok(_) => guessed(true),
        Err(OpenErr::WrongPassword) => guessed(false),
        Err(_) => {}
    }
    let mut map = pending();
    let Some(p) = map.get_mut(token) else {
        // Gone meanwhile (swept or pushed out).
        if let Ok((opened, _)) = &res {
            let _ = std::fs::remove_file(opened);
        }
        return Err(expired);
    };
    p.trying = false;
    p.tried = Some(Instant::now());
    let gone = match res {
        Ok((opened, ins)) => {
            // An upload: its opened copy takes its place.
            if p.owned {
                let _ = std::fs::remove_file(&p.file);
                p.bytes = std::fs::metadata(&opened).map(|m| m.len()).unwrap_or(0);
            }
            p.opened = Some(opened);
            p.warnings = preview_warnings(&ins);
            p.ins = Some(ins);
            return Ok(preview_of(state, token, p));
        }
        Err(OpenErr::WrongPassword) => {
            p.fails += 1;
            if p.fails < UNLOCK_TRIES {
                warn!("backup: a wrong password for an encrypted backup tries={}", p.fails);
                return Err((StatusCode::BAD_REQUEST, "wrong_password"));
            }
            warn!("backup: too many wrong passwords, the encrypted backup is let go");
            (map.remove(token), expired)
        }
        Err(OpenErr::Bad(e)) => (map.remove(token), bad_err(e)),
        Err(OpenErr::Api(e)) => return Err(e),
    };
    drop(map);
    if let Some(p) = gone.0 {
        drop_pending_file(&p);
    }
    Err(gone.1)
}

/// Restore the parts `items` of what the preview `token` showed (each one the
/// preview listed, else `bad_items`): refused during a call; a `before_import`
/// backup first; call records, texts and to-do items are only added.
pub(crate) async fn apply<S: AsRef<str>>(state: &CallsState, token: &str, items: &[S]) -> Result<Value, ApiErr> {
    sweep_pending();
    let listed: Option<Vec<&'static str>> = pending()
        .get(token)
        .filter(|p| p.made.elapsed() < TOKEN_TTL)
        .map(|p| p.ins.as_ref().map(|i| i.counts.keys().copied().collect()).unwrap_or_default());
    let Some(listed) = listed else {
        return Err((StatusCode::BAD_REQUEST, "expired"));
    };
    let chosen = match parse_items(items) {
        Some(c) if c.iter().all(|i| listed.contains(i)) => c,
        _ => return Err((StatusCode::BAD_REQUEST, "bad_items")),
    };
    if !state.store.live_calls().await.is_empty() {
        return Err((StatusCode::CONFLICT, "call_active"));
    }
    let ds = dir_state(&PathBuf::from(&state.settings.calls_data_dir));
    let Ok(_running) = ds.running.try_lock() else {
        return Err((StatusCode::CONFLICT, "busy"));
    };
    let taken = pending().remove(token);
    let Some(p) = taken else {
        return Err((StatusCode::BAD_REQUEST, "expired"));
    };
    let out = match &p.ins {
        Some(ins) => apply_pending(state, &p, ins, &chosen).await,
        None => Err((StatusCode::BAD_REQUEST, "bad_items")),
    };
    drop_pending_file(&p);
    out
}

/// A copy of the settings as they are now, in the backup folder (in the
/// default one when that cannot be written): the entry, its folder and the
/// password it is encrypted with (none when backups are not encrypted).
async fn make_before_import(state: &CallsState, lay: &Layout) -> Result<(Entry, PathBuf, Option<String>), RunErr> {
    let rt = state.settings_store.snapshot();
    let dir = configured_dir(&rt, &lay.data);
    let seal = backup_seal(&rt);
    match create_in(state, lay, &dir, Kind::BeforeImport, backup_items(false), seal.clone()).await {
        Ok(e) => Ok((e, dir, seal)),
        Err(first) => {
            let fallback = default_dir(&lay.data);
            if fallback == dir {
                return Err(first);
            }
            create_in(state, lay, &fallback, Kind::BeforeImport, backup_items(false), seal.clone()).await.map(|e| (e, fallback, seal))
        }
    }
}

/// `settings.json` is restored for settings or keys; any other file for its part.
fn wanted_file(w: &Wanted, chosen: &[&str]) -> bool {
    if w.rel == "config/settings.json" {
        chosen.contains(&"settings") || chosen.contains(&"keys")
    } else {
        chosen.contains(&w.item)
    }
}

async fn apply_pending(state: &CallsState, p: &Pending, ins: &Inspected, chosen: &[&'static str]) -> Result<Value, ApiErr> {
    let lay = Layout::of(state);
    let want: Vec<Wanted> = ins.files.iter().filter(|w| wanted_file(w, chosen)).cloned().collect();
    // Room for what is unpacked (on the disk of the data folder) first.
    let need: u64 = want.iter().map(|w| w.entry.size).sum();
    if free_space(&lay.data).is_some_and(|free| free < need.saturating_add(DISK_MARGIN_BYTES)) {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, "disk_full"));
    }
    let (before, before_dir, before_seal) = make_before_import(state, &lay).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.code()))?;
    let before_path = before_dir.join(&before.name);
    let undo = |created: Vec<PathBuf>| {
        let (before_path, seal) = (before_path.clone(), before_seal.clone());
        let lay = lay.clone();
        async move { roll_back(state, &lay, &before_path, &created, seal.as_deref()).await }
    };
    let stage = staging_dir(&lay.data).join(format!("apply-{}", uuid::Uuid::new_v4().simple()));
    let unpacked = {
        let (src, plain, stage, want) = (p.source().to_path_buf(), ins.plain, stage.clone(), want.clone());
        tokio::task::spawn_blocking(move || extract(&src, plain, &want, &stage))
            .await
            .unwrap_or_else(|_| Err(Bad::Io(io::Error::other("restore task"))))
    };
    if let Err(e) = unpacked {
        let _ = std::fs::remove_dir_all(&stage);
        return Err(bad_err(e));
    }
    let mut created = Vec::new();
    let way = if whole_keys(p, ins) { SettingsWay::WholeKeys } else { SettingsWay::Mix };
    let installed = match install(state, &lay, &stage, &want, chosen, way, &mut created).await {
        Ok(i) => i,
        Err(e) => {
            warn!("backup: restoring failed ({}), the settings from before are put back", err_kind(&e));
            undo(created).await;
            let _ = std::fs::remove_dir_all(&stage);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "io"));
        }
    };
    if chosen.iter().any(|i| CALL_ITEMS.contains(i)) {
        match merge_records(state, &lay, &stage, &want).await {
            Ok((calls, sms, tasks)) => info!("backup records added calls={calls} sms={sms} tasks={tasks}"),
            Err(e) => {
                warn!("backup: adding the call records failed ({}), the settings from before are put back", err_kind(&e));
                undo(created).await;
                let _ = std::fs::remove_dir_all(&stage);
                return Err((StatusCode::INTERNAL_SERVER_ERROR, "io"));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&stage);
    prune(&before_dir, Kind::BeforeImport, BEFORE_IMPORT_KEEP);
    let warnings = p.warnings.clone();
    info!("backup restored items={chosen:?} restart_needed={} encrypted={}", installed.restart_needed, p.encrypted);
    Ok(json!({"ok": true, "before_import": before.name, "restart_needed": installed.restart_needed, "warnings": warnings}))
}

/// The files to restore, checked again, unpacked into `stage`.
fn extract(src: &Path, plain: bool, want: &[Wanted], stage: &Path) -> Result<(), Bad> {
    std::fs::create_dir_all(stage)?;
    if plain {
        let to = stage_path(stage, "config/settings.json");
        if let Some(dir) = to.parent() {
            std::fs::create_dir_all(dir)?;
        }
        io::copy(&mut File::open(src)?, &mut create_private(&to)?)?;
        return Ok(());
    }
    let mut zip = ZipIn::open(File::open(src)?)?;
    for w in want {
        let to = stage_path(stage, &w.rel);
        if let Some(dir) = to.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut out = BufWriter::new(create_private(&to)?);
        zip.copy_checked(&w.entry.path, w.entry.size, &w.entry.sha256, &mut out)?;
        out.flush()?;
    }
    Ok(())
}

struct Installed {
    restart_needed: bool,
}

/// How a restore takes the settings file of a backup.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsWay {
    /// Mixed into this computer's: the chosen parts, keys only where the backup has them.
    Mix,
    /// This computer's own `before_import` copy: the keys group exactly as it was.
    WholeKeys,
    /// Putting the `before_import` copy back after a restore failed: the file whole.
    Exact,
}

/// The parts `chosen` other than the call records in place (files that were
/// not here go into `created`), then read again by their stores. Settings and
/// keys mix into this computer's ([`crate::settings_store::combine_settings`]);
/// see [`SettingsWay`].
async fn install(
    state: &CallsState,
    lay: &Layout,
    stage: &Path,
    want: &[Wanted],
    chosen: &[&'static str],
    way: SettingsWay,
    created: &mut Vec<PathBuf>,
) -> anyhow::Result<Installed> {
    let items: HashSet<&'static str> = chosen.iter().copied().collect();
    let (s, k) = (items.contains("settings"), items.contains("keys"));
    for w in want.iter().filter(|w| !CALL_ITEMS.contains(&w.item)) {
        if w.rel == "config/settings.json" {
            continue;
        }
        let target = lay.target(&w.rel).context("no place for a file of the backup")?;
        if !target.exists() {
            created.push(target.clone());
        }
        move_into(&stage_path(stage, &w.rel), &target).with_context(|| format!("put back {}", w.rel))?;
    }
    let staged_settings = stage_path(stage, "config/settings.json");
    if (s || k) && staged_settings.is_file() {
        let text = std::fs::read_to_string(&staged_settings).context("read the settings of the backup")?;
        let text = text.trim_start_matches('\u{feff}');
        let days_before = state.settings_store.snapshot().retention_days;
        let rt = match way {
            SettingsWay::Exact => state.settings_store.replace_from_text(text)?,
            w => state.settings_store.replace_from_backup(text, s, k, w == SettingsWay::WholeKeys)?,
        };
        crate::calls_http::purge_after_fewer_days(state, days_before, rt.retention_days);
        // Language, answer style, phone names: process-wide, like a settings
        // save does (left alone in unit tests, where other tests read them).
        if !cfg!(test) {
            rt.apply_globals();
            let (previews, sr) = (state.previews.clone(), state.settings.sample_rate);
            tokio::spawn(async move {
                crate::voice_preview::prepare_reject_clip(&previews, &rt, sr).await;
            });
        }
    }
    let restart_needed = reload_stores(state, &items).await?;
    if s {
        // Blocked numbers kept in an older settings file join the Blacklist.
        crate::contacts::absorb_reject_numbers(&state.settings_store, &state.contacts).await;
    }
    Ok(Installed { restart_needed })
}

/// The stores of `items` read their files again. True when one of them can
/// only take its file at the next start.
async fn reload_stores(state: &CallsState, items: &HashSet<&'static str>) -> anyhow::Result<bool> {
    let mut restart = false;
    if items.contains("contacts") {
        state.contacts.reload().await?;
        restart |= state.contacts.is_blocked();
    }
    if items.contains("numbers") {
        crate::scam_db::numbers().reload();
    }
    if items.contains("phones") {
        restart |= !state.errands.sims.reload();
    }
    if items.contains("keys") {
        crate::phone_wifi::pairing().reload();
    }
    if items.contains("errands") {
        state.errands.reload();
    }
    Ok(restart)
}

/// Restoring failed half-way: the files it added go, and the `before_import`
/// backup is put back (opened with `seal` first when it is encrypted).
async fn roll_back(state: &CallsState, lay: &Layout, before: &Path, created: &[PathBuf], seal: Option<&str>) {
    for p in created {
        let _ = std::fs::remove_file(p);
    }
    let mut src = before.to_path_buf();
    let mut opened = None;
    if before.file_name().and_then(|n| n.to_str()).is_some_and(is_sealed_name) {
        match open_sealed(&lay.data, before, seal.unwrap_or_default()).await {
            Ok((o, _)) => {
                src = o.clone();
                opened = Some(o);
            }
            Err(_) => {
                warn!("backup: the copy made before restoring cannot be opened");
                return;
            }
        }
    }
    let stage = staging_dir(&lay.data).join(format!("undo-{}", uuid::Uuid::new_v4().simple()));
    let read = {
        let stage = stage.clone();
        tokio::task::spawn_blocking(move || -> Result<Inspected, Bad> {
            let ins = inspect(&src)?;
            extract(&src, ins.plain, &ins.files, &stage)?;
            Ok(ins)
        })
        .await
        .unwrap_or_else(|_| Err(Bad::Io(io::Error::other("undo task"))))
    };
    match read {
        Ok(ins) => {
            let mut again = Vec::new();
            let held: Vec<&'static str> = ins.counts.keys().copied().collect();
            if let Err(e) = install(state, lay, &stage, &ins.files, &held, SettingsWay::Exact, &mut again).await {
                warn!("backup: the settings from before could not all be put back ({})", err_kind(&e));
            }
            let all: HashSet<&'static str> = STORE_ITEMS.into_iter().collect();
            if let Err(e) = reload_stores(state, &all).await {
                warn!("backup: reading the restored files again failed ({})", err_kind(&e));
            }
        }
        Err(e) => warn!("backup: the copy made before restoring cannot be read: {}", bad_err(e).1),
    }
    let _ = std::fs::remove_dir_all(&stage);
    if let Some(o) = opened {
        let _ = std::fs::remove_file(o);
    }
}

/// Call records with their recordings, texts and to-do items of the backup
/// that are not here yet are added; how many of each.
async fn merge_records(state: &CallsState, lay: &Layout, stage: &Path, want: &[Wanted]) -> anyhow::Result<(usize, usize, usize)> {
    let (mut calls, mut sms, mut tasks) = (0, 0, 0);
    // The recordings this restore brings back: the only files a restored record may name.
    let restored: HashSet<&str> = want.iter().filter(|w| w.item == "calls" && w.rel != "index.json").map(|w| w.rel.as_str()).collect();
    // Recordings first, so the call list never points at a file that is not there.
    for w in want.iter().filter(|w| w.item == "calls" && w.rel != "index.json") {
        let target = lay.target(&w.rel).context("no place for a recording")?;
        if !target.exists() {
            move_into(&stage_path(stage, &w.rel), &target).with_context(|| format!("add {}", w.rel))?;
        }
    }
    let index = stage_path(stage, "index.json");
    if want.iter().any(|w| w.rel == "index.json") && index.is_file() {
        let mut records = crate::calls::records_from_index_json(&std::fs::read(&index)?).context("call list of the backup")?;
        for r in records.iter_mut() {
            for slot in [&mut r.audio_path, &mut r.tts_audio_path, &mut r.remote_audio_path, &mut r.ai_audio_path, &mut r.voicemail_path] {
                keep_restored(slot, &restored);
            }
        }
        calls = state.store.merge_records(records).await;
    }
    let texts = stage_path(stage, "sms/messages.json");
    if want.iter().any(|w| w.rel == "sms/messages.json") && texts.is_file() {
        let list = crate::sms_store::messages_from_json(&std::fs::read(&texts)?).context("texts of the backup")?;
        sms = state.sms.merge(list);
    }
    let mut list = Vec::new();
    for w in want.iter().filter(|w| w.item == "tasks" && w.rel != "tasks/index.json") {
        let bytes = std::fs::read(stage_path(stage, &w.rel))?;
        let mut task = serde_json::from_slice::<crate::tasks::TaskRecord>(&bytes).with_context(|| format!("to-do item {}", w.rel))?;
        keep_restored(&mut task.voicemail_path, &restored);
        list.push(task);
    }
    if !list.is_empty() {
        tasks = state.task_store.merge(list).await?;
    }
    Ok((calls, sms, tasks))
}

/// A file a restored record names stays only when it is `YYYY-MM-DD/<name>`
/// and this restore brought it back; anything else is cleared.
fn keep_restored(slot: &mut Option<String>, restored: &HashSet<&str>) {
    if slot.as_deref().is_some_and(|p| !(crate::calls::record_file_ok(p) && restored.contains(p))) {
        *slot = None;
    }
}

/// What kind of failure, for the log (never a message that could quote a file).
fn err_kind(e: &anyhow::Error) -> String {
    if let Some(io) = e.downcast_ref::<io::Error>() {
        return format!("io:{:?}", io.kind());
    }
    if e.downcast_ref::<serde_json::Error>().is_some() {
        return "format".to_string();
    }
    "other".to_string()
}

/// Free room on the disk that holds `dir`; None when it cannot be told.
#[cfg(unix)]
// The statvfs fields are u32 on some systems and u64 on others.
#[allow(clippy::useless_conversion)]
fn free_space(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt as _;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: an all-zero statvfs is a valid value to be filled in.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a NUL-terminated path and `st` a writable statvfs.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some(u64::from(st.f_bavail).saturating_mul(u64::from(st.f_frsize)))
}

#[cfg(windows)]
fn free_space(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt as _;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(dir: *const u16, free: *mut u64, total: *mut u64, total_free: *mut u64) -> i32;
    }
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0u64;
    // SAFETY: a NUL-terminated wide path and a writable u64; the other two may be null.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(free)
}

#[cfg(not(any(unix, windows)))]
fn free_space(_dir: &Path) -> Option<u64> {
    None
}

// ---------------------------------------------------------------- web API

fn message(code: &str) -> &'static str {
    match code {
        "not_backup" => "this is not a Bageo backup or settings file",
        "damaged" => "the backup file is damaged",
        "too_new" => "this backup was made by a newer Bageo",
        "too_large" => "the file is too large",
        "expired" => "this preview has expired; choose the file again",
        "call_active" => "a call is in progress; try again when it ends",
        "busy" => "a backup or another restore is under way; try again in a moment",
        "need_password" => "set a console password first, or open the console on the computer Bageo runs on",
        "dir_unwritable" => "backups cannot be saved in the backup folder",
        "backup_dir_unwritable" => "backups cannot be saved in this folder",
        "disk_full" => "the disk is full",
        "not_found" => "no such backup file",
        "not_local" => "only on the computer Bageo runs on",
        "bad_request" => "bad request",
        "bad_items" => "pick one or more of: settings, keys, contacts, numbers, phones, errands, calls, sms, tasks",
        "wrong_password" => "this password does not open the backup",
        "backup_password_missing" => "set a backup password before turning on encryption",
        "backup_password_short" => "the password needs at least 8 characters",
        _ => "something went wrong while saving",
    }
}

fn err_response((status, code): ApiErr) -> Response {
    api_error(status, code, message(code))
}

/// Changes take JSON only (a form or a no-cors fetch from another page cannot send it).
fn json_request(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("application/json"))
        .unwrap_or(false)
}

fn json_required() -> Response {
    api_error(StatusCode::BAD_REQUEST, "bad_request", "Content-Type must be application/json")
}

/// The uploaded file's name (`X-Filename`, URL-encoded).
fn upload_name(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get("x-filename")?.to_str().ok()?;
    let name = url::form_urlencoded::parse(format!("n={raw}").as_bytes()).next().map(|(_, v)| v.into_owned())?;
    let clean: String = name.chars().filter(|c| !c.is_control()).take(200).collect();
    let clean = clean.trim();
    (!clean.is_empty()).then(|| clean.to_string())
}

/// The request comes from this computer itself (not through a proxy on it):
/// loopback, or this computer's own LAN address (`crate::lan`).
fn from_this_computer(ext: &Extensions, headers: &HeaderMap) -> bool {
    crate::lan::from_this_computer(ext, headers)
}

/// The request names this computer by a loopback name (`localhost`, `127.x.x.x`,
/// `[::1]`): a page of another site cannot reach Bageo by pointing its own name
/// at this computer (DNS rebinding).
fn loopback_host(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let host = host.trim().to_ascii_lowercase();
    let name = match host.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or("").to_string(),
        None => match host.rsplit_once(':') {
            Some((h, port)) if port.bytes().all(|b| b.is_ascii_digit()) => h.to_string(),
            _ => host.clone(),
        },
    };
    name == "localhost"
        || name.ends_with(".localhost")
        || name == "::1"
        || name.parse::<std::net::Ipv4Addr>().is_ok_and(|ip| ip.is_loopback())
}

/// A file holding keys may be downloaded: a console password is set, or the
/// request comes from this computer itself, to a loopback name. Otherwise
/// anyone who can reach the console could take them (`need_password`).
fn may_download_keys(state: &CallsState, ext: &Extensions, headers: &HeaderMap) -> bool {
    let password = crate::settings_store::active_ui_token(&state.settings.calls_ui_token);
    !password.trim().is_empty() || (from_this_computer(ext, headers) && loopback_host(headers))
}

fn can_open_folder(ext: &Extensions, headers: &HeaderMap) -> bool {
    cfg!(any(target_os = "windows", target_os = "macos")) && from_this_computer(ext, headers)
}

/// `GET /api/backup`: the folder, the last backup, the next one, the list.
async fn status(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>, ext: Extensions) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    let rt = state.settings_store.snapshot();
    let data = PathBuf::from(&state.settings.calls_data_dir);
    let dir = configured_dir(&rt, &data);
    let ds = dir_state(&data);
    let running = ds.running.try_lock().is_err();
    let (last, failed_at) = {
        let i = ds.inner();
        (i.last.clone(), i.failed_at)
    };
    let d = dir.clone();
    let (dir_ok, backups, last_auto_at) = tokio::task::spawn_blocking(move || (dir_writable(&d), list_backups(&d), last_auto(&d)))
        .await
        .unwrap_or((false, Vec::new(), None));
    let last = match last {
        Some(l) => json!(l),
        None => backups.first().map_or(Value::Null, |e| json!({"at": e.at, "ok": true, "name": e.name, "size": e.size, "error": null})),
    };
    let next_at = next_run(&rt.backup_schedule, last_auto_at, failed_at, Local::now().naive_local()).map(local_rfc3339);
    Json(json!({
        "ok": true,
        "dir": absolute(&dir).display().to_string(),
        "default_dir": absolute(&default_dir(&data)).display().to_string(),
        "dir_ok": dir_ok,
        "can_open_folder": can_open_folder(&ext, &headers),
        // Downloads that hold keys work here (else they answer need_password).
        "can_download": may_download_keys(&state, &ext, &headers),
        "running": running,
        "last": last,
        "next_at": next_at,
        "backups": backups,
    }))
    .into_response()
}

/// `POST /api/backup/run`: a manual backup now.
async fn run(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    if !json_request(&headers) {
        return json_required();
    }
    match run_backup(&state, Kind::Manual).await {
        Ok(entry) => Json(json!({"ok": true, "entry": entry})).into_response(),
        Err(RunErr::Busy) => err_response((StatusCode::CONFLICT, "busy")),
        Err(e) => err_response((StatusCode::INTERNAL_SERVER_ERROR, e.code())),
    }
}

#[derive(Deserialize)]
struct ExportBody {
    /// The parts ([`ITEMS`]); none: [`DEFAULT_ITEMS`].
    #[serde(default)]
    items: Option<Vec<String>>,
    /// Encrypt the file with this password (a `.bageo`).
    #[serde(default)]
    password: Option<String>,
}

/// A download that works once ("Export", "Download").
struct Ticket {
    path: PathBuf,
    name: String,
    /// The file goes once sent (an export).
    remove: bool,
    made: Instant,
}

static TICKETS: LazyLock<Mutex<HashMap<String, Ticket>>> = LazyLock::new(Default::default);

fn tickets() -> MutexGuard<'static, HashMap<String, Ticket>> {
    TICKETS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Tickets older than two minutes go, with the exports they were for.
fn sweep_tickets() {
    let gone: Vec<Ticket> = {
        let mut map = tickets();
        let old: Vec<String> = map.iter().filter(|(_, t)| t.made.elapsed() >= TICKET_TTL).map(|(k, _)| k.clone()).collect();
        old.into_iter().filter_map(|k| map.remove(&k)).collect()
    };
    for t in gone.iter().filter(|t| t.remove) {
        let _ = std::fs::remove_file(&t.path);
    }
}

/// A new ticket (256 random bits) for downloading `path` as `name`.
fn issue_ticket(path: PathBuf, name: String, remove: bool) -> io::Result<String> {
    sweep_tickets();
    let mut b = [0u8; 32];
    backup_crypt::random(&mut b)?;
    let t = hex(&b);
    tickets().insert(t.clone(), Ticket { path, name, remove, made: Instant::now() });
    Ok(t)
}

/// The ticket `t`, used up now; None when it is unknown, used or too old.
fn take_ticket(t: &str) -> Option<Ticket> {
    sweep_tickets();
    tickets().remove(t)
}

/// A file sent as a download; `remove`: deleted once sent (or the download is cut off).
struct Sending {
    file: Option<tokio::fs::File>,
    path: PathBuf,
    remove: bool,
}

impl Drop for Sending {
    fn drop(&mut self) {
        drop(self.file.take());
        if self.remove {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// `path` as a download named `name` (a zip, or an encrypted `.bageo`), read a piece at a time.
async fn send_file(path: PathBuf, name: &str, remove: bool) -> Response {
    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(_) => {
            if remove {
                let _ = std::fs::remove_file(&path);
            }
            return err_response((StatusCode::NOT_FOUND, "not_found"));
        }
    };
    let len = file.metadata().await.map(|m| m.len()).ok();
    let sending = Sending { file: Some(file), path, remove };
    let stream = futures_util::stream::unfold(Some(sending), |state| async move {
        let mut s = state?;
        let mut buf = vec![0u8; 64 * 1024];
        let f = s.file.as_mut()?;
        match tokio::io::AsyncReadExt::read(f, &mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<Bytes, io::Error>(Bytes::from(buf)), Some(s)))
            }
            Err(e) => Some((Err(e), None)),
        }
    });
    let mut res = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, if is_sealed_name(name) { "application/octet-stream" } else { "application/zip" })
        .header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\""))
        .header(header::CACHE_CONTROL, "no-store");
    if let Some(len) = len {
        res = res.header(header::CONTENT_LENGTH, len);
    }
    res.body(Body::from_stream(stream)).unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// `POST /api/backup/export` `{items, password}`: the file of those parts
/// (`bad_items` for an empty list or an unknown part), encrypted when a password
/// comes (at least 8 characters, else `backup_password_short`): its name and a
/// ticket to download it.
async fn export(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>, ext: Extensions, body: Bytes) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    if !json_request(&headers) {
        return json_required();
    }
    let Ok(b) = serde_json::from_slice::<ExportBody>(&body) else {
        return err_response((StatusCode::BAD_REQUEST, "bad_request"));
    };
    let want = match &b.items {
        None => Some(DEFAULT_ITEMS.to_vec()),
        Some(list) => parse_items(list),
    };
    let Some(want) = want else {
        return err_response((StatusCode::BAD_REQUEST, "bad_items"));
    };
    let password = match b.password {
        None => None,
        Some(p) if backup_crypt::password_ok(&p) => Some(p.trim().to_string()),
        Some(_) => return err_response((StatusCode::BAD_REQUEST, "backup_password_short")),
    };
    if want.contains(&"keys") && !may_download_keys(&state, &ext, &headers) {
        return err_response((StatusCode::FORBIDDEN, "need_password"));
    }
    match export_file(&state, want, password).await {
        Ok((name, path)) => match issue_ticket(path.clone(), name.clone(), true) {
            Ok(ticket) => Json(json!({"ok": true, "ticket": ticket, "name": name})).into_response(),
            Err(_) => {
                let _ = std::fs::remove_file(&path);
                err_response((StatusCode::INTERNAL_SERVER_ERROR, "io"))
            }
        },
        Err(e) => err_response((StatusCode::INTERNAL_SERVER_ERROR, e.code())),
    }
}

#[derive(Deserialize)]
struct NameBody {
    name: String,
}

/// `POST /api/backup/file-ticket` `{name}`: a ticket to download that file of the
/// backup folder. Backups hold the keys: without a console password, only on
/// this computer (`need_password`).
async fn file_ticket(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>, ext: Extensions, body: Bytes) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    if !json_request(&headers) {
        return json_required();
    }
    let Ok(b) = serde_json::from_slice::<NameBody>(&body) else {
        return err_response((StatusCode::BAD_REQUEST, "bad_request"));
    };
    if !may_download_keys(&state, &ext, &headers) {
        return err_response((StatusCode::FORBIDDEN, "need_password"));
    }
    let name = b.name.trim().to_string();
    let path = configured_dir(&state.settings_store.snapshot(), &PathBuf::from(&state.settings.calls_data_dir)).join(&name);
    if parse_name(&name).is_none() || !is_plain_file(&path) {
        return err_response((StatusCode::NOT_FOUND, "not_found"));
    }
    match issue_ticket(path, name.clone(), false) {
        Ok(ticket) => Json(json!({"ok": true, "ticket": ticket, "name": name})).into_response(),
        Err(_) => err_response((StatusCode::INTERNAL_SERVER_ERROR, "io")),
    }
}

/// `GET /api/backup/download/{ticket}`: the file of a ticket, once, within two
/// minutes; `not_found` after. The ticket is all it needs.
async fn download(UrlPath(ticket): UrlPath<String>) -> Response {
    match take_ticket(ticket.trim()) {
        Some(t) => send_file(t.path, &t.name, t.remove).await,
        None => err_response((StatusCode::NOT_FOUND, "not_found")),
    }
}

/// `POST /api/backup/import`, the file as the body and its name in
/// `X-Filename`: the preview, nothing restored yet.
async fn import(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>, body: Body) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    let Some(name) = upload_name(&headers) else {
        return api_error(StatusCode::BAD_REQUEST, "bad_request", "send the file name (URL-encoded) in X-Filename");
    };
    let declared = headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
    match preview_upload_stream(&state, &name, declared, body.into_data_stream()).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_response(e),
    }
}

/// `POST /api/backup/restore` `{name}`: the preview of a file in the backup folder.
async fn restore(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>, body: Bytes) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    if !json_request(&headers) {
        return json_required();
    }
    let Ok(b) = serde_json::from_slice::<NameBody>(&body) else {
        return err_response((StatusCode::BAD_REQUEST, "bad_request"));
    };
    match preview_saved(&state, b.name.trim()).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_response(e),
    }
}

#[derive(Deserialize)]
struct ApplyBody {
    token: String,
    #[serde(default)]
    items: Vec<String>,
}

/// `POST /api/backup/delete` `{name}`: deletes one file from the backup
/// folder. It cannot be undone: without a console password, only from this
/// computer (as downloading one, `need_password`).
async fn delete_route(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>, ext: Extensions, body: Bytes) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    if !json_request(&headers) {
        return json_required();
    }
    let Ok(b) = serde_json::from_slice::<NameBody>(&body) else {
        return err_response((StatusCode::BAD_REQUEST, "bad_request"));
    };
    if !may_download_keys(&state, &ext, &headers) {
        return err_response((StatusCode::FORBIDDEN, "need_password"));
    }
    match delete_saved(&state, b.name.trim()) {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => err_response(e),
    }
}

/// Deletes the backup file `name` from the backup folder: only a name of a
/// backup (`parse_name`, `.zip` or `.bageo`), with no path in it, that is a
/// regular file there (a symbolic link is not followed). Not while a backup or
/// a restore is under way (`busy`).
pub(crate) fn delete_saved(state: &CallsState, name: &str) -> Result<(), ApiErr> {
    if name.is_empty() || name.contains(['/', '\\', ':', '\0']) || name.starts_with('.') {
        return Err((StatusCode::BAD_REQUEST, "bad_request"));
    }
    if parse_name(name).is_none() {
        return Err((StatusCode::NOT_FOUND, "not_found"));
    }
    let data = PathBuf::from(&state.settings.calls_data_dir);
    let ds = dir_state(&data);
    let Ok(_running) = ds.running.try_lock() else {
        return Err((StatusCode::CONFLICT, "busy"));
    };
    let path = configured_dir(&state.settings_store.snapshot(), &data).join(name);
    if !is_plain_file(&path) {
        return Err((StatusCode::NOT_FOUND, "not_found"));
    }
    // A restore preview of this file could not be applied once it is gone: it goes too.
    let previews: Vec<Pending> = {
        let mut map = pending();
        let of_it: Vec<String> = map.iter().filter(|(_, p)| !p.owned && p.file == path).map(|(k, _)| k.clone()).collect();
        of_it.into_iter().filter_map(|k| map.remove(&k)).collect()
    };
    for p in &previews {
        drop_pending_file(p);
    }
    match std::fs::remove_file(&path) {
        Ok(()) => {
            info!("backup deleted name={name}");
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err((StatusCode::NOT_FOUND, "not_found")),
        Err(e) => {
            warn!("backup: could not delete name={name} kind={:?}", e.kind());
            Err((StatusCode::INTERNAL_SERVER_ERROR, "io"))
        }
    }
}

/// `POST /api/backup/apply` `{token, items}`: restore those parts of what the preview showed.
async fn apply_route(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>, body: Bytes) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    if !json_request(&headers) {
        return json_required();
    }
    let Ok(b) = serde_json::from_slice::<ApplyBody>(&body) else {
        return err_response((StatusCode::BAD_REQUEST, "bad_request"));
    };
    match apply(&state, b.token.trim(), &b.items).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_response(e),
    }
}

#[derive(Deserialize)]
struct DiscardBody {
    token: String,
}

/// `POST /api/backup/discard` `{token}`: the preview was closed without
/// restoring; it and its upload go now (else after 15 minutes).
async fn discard_route(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>, body: Bytes) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    if !json_request(&headers) {
        return json_required();
    }
    let Ok(b) = serde_json::from_slice::<DiscardBody>(&body) else {
        return err_response((StatusCode::BAD_REQUEST, "bad_request"));
    };
    discard(&PathBuf::from(&state.settings.calls_data_dir), b.token.trim());
    Json(json!({"ok": true})).into_response()
}

#[derive(Deserialize)]
struct UnlockBody {
    token: String,
    #[serde(default)]
    password: String,
}

/// `POST /api/backup/unlock` `{token, password}`: open the encrypted file of a
/// preview; its preview (`wrong_password`, `expired` after 5 wrong ones).
async fn unlock_route(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>, ext: Extensions, body: Bytes) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    if !json_request(&headers) {
        return json_required();
    }
    let Ok(b) = serde_json::from_slice::<UnlockBody>(&body) else {
        return err_response((StatusCode::BAD_REQUEST, "bad_request"));
    };
    // Without a console password, guessing a backup password is for this computer only.
    if !may_download_keys(&state, &ext, &headers) {
        return err_response((StatusCode::FORBIDDEN, "need_password"));
    }
    match unlock(&state, b.token.trim(), &b.password).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_response(e),
    }
}

/// Open `dir` in Explorer / Finder.
fn open_in_file_manager(dir: &Path) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    let child = std::process::Command::new("explorer").arg(dir).spawn();
    #[cfg(target_os = "macos")]
    let child = std::process::Command::new("open").arg(dir).spawn();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let child: io::Result<std::process::Child> = {
        let _ = dir;
        Err(io::Error::other("no file manager here"))
    };
    let mut child = child?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// `POST /api/backup/open-folder`: only from this computer, on Windows or macOS.
async fn open_folder(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>, ext: Extensions) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    if !json_request(&headers) {
        return json_required();
    }
    if !can_open_folder(&ext, &headers) {
        return err_response((StatusCode::FORBIDDEN, "not_local"));
    }
    let dir = configured_dir(&state.settings_store.snapshot(), &PathBuf::from(&state.settings.calls_data_dir));
    if std::fs::create_dir_all(&dir).is_err() {
        return err_response((StatusCode::INTERNAL_SERVER_ERROR, "dir_unwritable"));
    }
    match open_in_file_manager(&dir) {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => {
            warn!("backup: the folder could not be opened kind={:?}", e.kind());
            err_response((StatusCode::INTERNAL_SERVER_ERROR, "io"))
        }
    }
}

/// The backup API, merged into the console's router (`/calls`).
pub fn router() -> Router<CallsState> {
    Router::new()
        .route("/api/backup", get(status))
        .route("/api/backup/run", post(run))
        .route("/api/backup/export", post(export))
        .route("/api/backup/file-ticket", post(file_ticket))
        .route("/api/backup/download/{ticket}", get(download))
        .route("/api/backup/import", post(import))
        .route("/api/backup/restore", post(restore))
        .route("/api/backup/delete", post(delete_route))
        .route("/api/backup/apply", post(apply_route))
        .route("/api/backup/discard", post(discard_route))
        .route("/api/backup/unlock", post(unlock_route))
        .route("/api/backup/open-folder", post(open_folder))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calls_http::PhoneDevices;
    use axum::extract::ConnectInfo;

    const FAKE_GEMINI: &str = "test-fake-gemini-key-0001";
    const FAKE_SUMMARY: &str = "test-fake-summary-key-0002";
    const FAKE_CHANNEL: &str = "test-fake-channel-token-0003";
    const FAKE_PAIRING: &str = "test_fake_pairing_key_0004_abcdefghijklmnopqrstuvwxyz";
    const FAKE_BACKUP_PW: &str = "test-fake-backup-pass-0005";

    fn temp_root(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bageo-backup-{tag}-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A console state whose files all live in `root`, the way the agent lays them out.
    fn state_at(root: &Path) -> CallsState {
        let mut base = crate::config::Settings::from_env();
        base.calls_data_dir = root.to_string_lossy().into_owned();
        base.telegram_bot_token = String::new();
        base.telegram_chat_id = String::new();
        CallsState {
            settings: base.clone(),
            store: crate::calls::CallStore::new(root.to_path_buf(), true, false),
            settings_store: crate::settings_store::SettingsStore::load(base),
            task_store: crate::tasks::TaskStore::new(root.join("tasks")),
            http: reqwest::Client::new(),
            control: crate::callback::ControlRegistry::new(),
            ambience: Arc::new(crate::ambience::AmbienceLibrary::load(root, 16000)),
            previews: Arc::new(crate::voice_preview::VoicePreviews::new(root.join("voice_preview"), root.join("voices"))),
            phones: PhoneDevices::new(),
            contacts: crate::contacts::ContactStore::open(root.join("contacts.json")),
            errands: crate::errands::ErrandStore::open(root.join("errands.json"), crate::errands::SimBook::open(root.join("phone_sims.json"))),
            sms: crate::sms_store::SmsStore::open(root.join("sms"), crate::sms_store::now_ms(), 15),
        }
    }

    /// A state with settings, two fake keys, a channel with a token, a
    /// contact and a pairing key file.
    async fn keyed_state(root: &Path, owner: &str) -> CallsState {
        let st = state_at(root);
        st.settings_store
            .apply_patch(&json!({
                "owner_name": owner,
                "backup_keep": 7,
                "gemini_api_key": FAKE_GEMINI,
                "summary_api_key": FAKE_SUMMARY,
                "notify_channels": [{"kind": "ntfy", "name": "Test channel", "url": "https://ntfy.example/fake-topic", "token": FAKE_CHANNEL, "target": "fake-topic"}],
                "telegram_chat_id": "fake-chat-a",
                "whitelist_forward_number": "+15555550188",
                "summary_api_base": "https://summary-a.example/v1",
                "mcp_enabled": true,
            }))
            .unwrap();
        std::fs::write(root.join(crate::phone_wifi::KEY_FILE), FAKE_PAIRING).unwrap();
        let draft = crate::contacts::Draft { name: "Alex Example".into(), phones: vec!["+1 555 555 0123".into()], ..Default::default() };
        st.contacts.add(&draft).await.unwrap();
        st
    }

    async fn export_bytes(state: &CallsState, items: &[&'static str]) -> (String, Vec<u8>) {
        let (name, path) = export_file(state, items.to_vec(), None).await.unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        (name, bytes)
    }

    /// Every file of a zip, unpacked, one after the other.
    fn unpacked(bytes: &[u8]) -> Vec<u8> {
        let path = std::env::temp_dir().join(format!("bageo-unpack-{}.zip", uuid::Uuid::new_v4().simple()));
        std::fs::write(&path, bytes).unwrap();
        let mut z = ZipIn::open(File::open(&path).unwrap()).unwrap();
        let names: Vec<String> = z.entries.keys().cloned().collect();
        let mut all = Vec::new();
        for n in names {
            all.extend(z.read_entry(&n, 1 << 30).unwrap().unwrap());
        }
        let _ = std::fs::remove_file(&path);
        all
    }

    fn has(hay: &[u8], needle: &str) -> bool {
        hay.windows(needle.len()).any(|w| w == needle.as_bytes())
    }

    fn manifest_items(bytes: &[u8]) -> Vec<String> {
        let path = std::env::temp_dir().join(format!("bageo-manifest-{}.zip", uuid::Uuid::new_v4().simple()));
        std::fs::write(&path, bytes).unwrap();
        let items = inspect(&path).unwrap().manifest.unwrap().items;
        let _ = std::fs::remove_file(&path);
        items
    }

    fn preview_items(preview: &Value) -> Vec<String> {
        preview["items"].as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap().to_string()).collect()
    }

    fn sha(b: &[u8]) -> String {
        hex(&Sha256::digest(b))
    }

    /// A zip holding `files` (zip names) and the manifest `manifest`.
    fn zip_with(manifest: &Value, files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut z = ZipOut::new(io::Cursor::new(Vec::new()), Local::now().naive_local());
        for (name, body) in files {
            z.add(name, &mut &body[..], body.len() as u64).unwrap();
        }
        let m = serde_json::to_vec(manifest).unwrap();
        z.add(MANIFEST_NAME, &mut m.as_slice(), m.len() as u64).unwrap();
        z.finish().unwrap().into_inner()
    }

    fn manifest(version: u64, files: Vec<Value>) -> Value {
        json!({"format": FORMAT, "version": version, "app_version": "1.0.0", "created_at": "2026-01-01T00:00:00+00:00",
               "kind": "manual", "include_calls": false, "items": ["settings"], "files": files})
    }

    fn call_json(id: &str, day: &str) -> Value {
        json!({"id": id, "started_at": format!("{day}T03:04:05Z"), "ended_at": format!("{day}T03:05:05Z"), "serial": "pixel-test-1",
               "caller_id": "+15555550100", "duration_ms": 60000, "status": "ended", "audio_path": format!("{day}/{id}.wav"),
               "tts_audio_path": null, "uplink_bytes": 0, "tts_bytes": 0, "sample_rate": 16000, "events": []})
    }

    fn code(r: Result<Value, ApiErr>) -> &'static str {
        r.expect_err("refused").1
    }

    #[test]
    fn item_lists_are_checked() {
        assert_eq!(parse_items(&["contacts", "settings", "contacts"]), Some(vec!["settings", "contacts"]));
        assert_eq!(parse_items(&[" keys "]), Some(vec!["keys"]));
        assert_eq!(parse_items::<&str>(&[]), None);
        assert_eq!(parse_items(&[""]), None);
        assert_eq!(parse_items(&["settings", "passwords"]), None);
    }

    #[tokio::test]
    async fn export_then_import_brings_back_settings_keys_and_contacts() {
        let (ra, rb) = (temp_root("a"), temp_root("b"));
        let a = keyed_state(&ra, "Sam Example").await;
        a.settings_store.apply_patch(&json!({"public_url": "http://198.51.100.20:8787"})).unwrap();
        let (name, bytes) = export_bytes(&a, &DEFAULT_ITEMS).await;
        assert_eq!(parse_name(&name).map(|(k, _)| k), Some(Kind::Export));
        assert!(name.starts_with("bageo-export-"), "{name}");
        assert_eq!(manifest_items(&bytes), ["settings", "keys", "contacts"]);

        let b = state_at(&rb);
        let preview = preview_upload(&b, &name, Bytes::from(bytes)).await.unwrap();
        assert_eq!(preview["kind"], "export");
        assert_eq!(preview["has_calls"], false);
        assert_eq!(preview_items(&preview), ["settings", "keys", "contacts"]);
        assert!(preview["items"].as_array().unwrap().contains(&json!({"id": "contacts", "count": 1})));
        assert!(preview["warnings"].as_array().unwrap().is_empty());
        assert_eq!(preview["changes_connections"], true, "the channel of the backup is new here");
        assert!(preview["total_size"].as_u64().unwrap() > 0);

        let token = preview["token"].as_str().unwrap().to_string();
        let done = apply(&b, &token, &["settings", "keys", "contacts"]).await.unwrap();
        assert_eq!(done["ok"], true);
        let rt = b.settings_store.snapshot();
        // Fewer automatic backups kept (7 there, 10 here) only from the console.
        assert_eq!((rt.owner_name.as_str(), rt.backup_keep, rt.gemini_api_key.as_str()), ("Sam Example", 10, FAKE_GEMINI));
        assert_eq!(rt.public_url, "", "the address of this computer stays");
        assert_eq!(rt.notify_channels.len(), 1);
        assert_eq!(rt.notify_channels[0].token, FAKE_CHANNEL);
        let contacts = b.contacts.list("");
        assert_eq!((contacts.len(), contacts[0].name.as_str()), (1, "Alex Example"));
        assert_eq!(std::fs::read_to_string(rb.join(crate::phone_wifi::KEY_FILE)).unwrap(), FAKE_PAIRING);
        let saved = std::fs::read_to_string(rb.join("config").join("settings.json")).unwrap();
        assert!(saved.contains("Sam Example"));

        // A before_import copy came first: every part but the call records.
        let before = done["before_import"].as_str().unwrap();
        assert_eq!(parse_name(before).map(|(k, _)| k), Some(Kind::BeforeImport));
        let kept = inspect(&default_dir(&rb).join(before)).unwrap().manifest.unwrap();
        assert!(!kept.include_calls);
        assert_eq!(kept.items, ["settings", "keys"], "a fresh install: its settings and the summary service it uses");
        // The token works once.
        assert_eq!(code(apply(&b, &token, &["settings"]).await), "expired");
        // Its own keys and connections, back again: nothing would change.
        let (name, bytes) = export_bytes(&b, &["keys"]).await;
        let again = preview_upload(&b, &name, Bytes::from(bytes)).await.unwrap();
        assert_eq!(again["changes_connections"], false);
        let _ = std::fs::remove_dir_all(&ra);
        let _ = std::fs::remove_dir_all(&rb);
    }

    #[tokio::test]
    async fn a_settings_only_export_holds_no_key() {
        let ra = temp_root("nokey");
        let a = keyed_state(&ra, "Sam Example").await;
        let (_, bytes) = export_bytes(&a, &["settings"]).await;
        let plain = unpacked(&bytes);
        assert!(has(&plain, "Sam Example"), "the settings are there");
        for secret in [FAKE_GEMINI, FAKE_SUMMARY, FAKE_CHANNEL, FAKE_PAIRING, "ntfy.example", "Test channel", "fake-topic", "fake-chat-a", "+15555550188", "summary-a.example"] {
            assert!(!has(&plain, secret), "{secret} leaked");
        }
        assert_eq!(manifest_items(&bytes), ["settings"]);

        // Keys only: the credentials and the pairing key, nothing else.
        let (_, bytes) = export_bytes(&a, &["keys"]).await;
        let plain = unpacked(&bytes);
        assert!(has(&plain, FAKE_GEMINI) && has(&plain, FAKE_CHANNEL) && has(&plain, FAKE_PAIRING));
        assert!(has(&plain, "Test channel") && has(&plain, "fake-chat-a") && has(&plain, "+15555550188"), "connections go with the keys");
        assert!(!has(&plain, "Sam Example") && !has(&plain, "Alex Example"));
        assert_eq!(manifest_items(&bytes), ["keys"]);

        let (_, bytes) = export_bytes(&a, &["contacts"]).await;
        assert_eq!(manifest_items(&bytes), ["contacts"]);
        assert!(!has(&unpacked(&bytes), FAKE_GEMINI));
        let _ = std::fs::remove_dir_all(&ra);
    }

    #[tokio::test]
    async fn importing_a_file_without_keys_keeps_the_keys_here() {
        let (ra, rb) = (temp_root("ka"), temp_root("kb"));
        let a = keyed_state(&ra, "Sam Example").await;
        let (name, bytes) = export_bytes(&a, &["settings"]).await;
        let rb_owner = "Bob Example";
        let b = keyed_state(&rb, rb_owner).await;
        b.settings_store
            .apply_patch(&json!({
                "gemini_api_key": "test-fake-b-key-0005",
                "telegram_chat_id": "fake-chat-b",
                "whitelist_forward_number": "+15555550199",
                "summary_api_base": "https://summary-b.example/v1",
                "mcp_enabled": false,
                "backup_keep": 3,
            }))
            .unwrap();
        let preview = preview_upload(&b, &name, Bytes::from(bytes)).await.unwrap();
        assert_eq!(preview_items(&preview), ["settings"]);
        assert_eq!((preview["changes_password"].as_bool(), preview["changes_connections"].as_bool()), (Some(false), Some(false)));
        assert_eq!(code(apply(&b, preview["token"].as_str().unwrap(), &["keys"]).await), "bad_items");
        apply(&b, preview["token"].as_str().unwrap(), &["settings"]).await.unwrap();
        let rt = b.settings_store.snapshot();
        assert_eq!(rt.owner_name, "Sam Example");
        assert_eq!((rt.gemini_api_key.as_str(), rt.summary_api_key.as_str()), ("test-fake-b-key-0005", FAKE_SUMMARY));
        assert_eq!(rt.backup_keep, 7, "the other settings come from the file");
        // Its own connections stay whole.
        assert_eq!(rt.notify_channels.len(), 1);
        assert_eq!((rt.notify_channels[0].token.as_str(), rt.notify_channels[0].url.as_str()), (FAKE_CHANNEL, "https://ntfy.example/fake-topic"));
        assert_eq!((rt.telegram_chat_id.as_str(), rt.whitelist_forward_number.as_str()), ("fake-chat-b", "+15555550199"));
        assert_eq!((rt.summary_api_base.as_str(), rt.mcp_enabled), ("https://summary-b.example/v1", false));
        assert_eq!(std::fs::read_to_string(rb.join(crate::phone_wifi::KEY_FILE)).unwrap(), FAKE_PAIRING);
        let _ = std::fs::remove_dir_all(&ra);
        let _ = std::fs::remove_dir_all(&rb);
    }

    #[tokio::test]
    async fn restoring_only_the_keys_leaves_the_other_settings() {
        let (ra, rb) = (temp_root("oa"), temp_root("ob"));
        let a = keyed_state(&ra, "Sam Example").await;
        let (name, bytes) = export_bytes(&a, &DEFAULT_ITEMS).await;
        let b = state_at(&rb);
        b.settings_store.apply_patch(&json!({"owner_name": "Bob Example", "retention_days": 30})).unwrap();
        let preview = preview_upload(&b, &name, Bytes::from(bytes)).await.unwrap();
        assert_eq!(preview["changes_connections"], true);
        apply(&b, preview["token"].as_str().unwrap(), &["keys"]).await.unwrap();
        let rt = b.settings_store.snapshot();
        assert_eq!((rt.owner_name.as_str(), rt.retention_days, rt.backup_keep), ("Bob Example", 30, 10));
        assert_eq!((rt.gemini_api_key.as_str(), rt.summary_api_key.as_str()), (FAKE_GEMINI, FAKE_SUMMARY));
        assert_eq!((rt.notify_channels.len(), rt.telegram_chat_id.as_str(), rt.mcp_enabled), (1, "fake-chat-a", true));
        assert!(b.contacts.list("").is_empty(), "contacts were not picked");
        assert_eq!(std::fs::read_to_string(rb.join(crate::phone_wifi::KEY_FILE)).unwrap(), FAKE_PAIRING);
        let _ = std::fs::remove_dir_all(&ra);
        let _ = std::fs::remove_dir_all(&rb);
    }

    #[tokio::test]
    async fn apply_takes_only_parts_the_preview_listed() {
        let rb = temp_root("items");
        let b = state_at(&rb);
        let preview = preview_upload(&b, "settings.json", Bytes::from_static(br#"{"owner_name": "Items Example"}"#)).await.unwrap();
        let token = preview["token"].as_str().unwrap();
        let lists: [&[&str]; 5] = [&["keys"], &["calls"], &[], &["bogus"], &["settings", "contacts"]];
        for bad in lists {
            assert_eq!(code(apply(&b, token, bad).await), "bad_items", "{bad:?}");
        }
        assert!(names_in(&default_dir(&rb)).is_empty(), "nothing done for a bad list");
        apply(&b, token, &["settings"]).await.unwrap();
        assert_eq!(b.settings_store.snapshot().owner_name, "Items Example");
        let _ = std::fs::remove_dir_all(&rb);
    }

    #[tokio::test]
    async fn an_older_single_settings_file_is_imported() {
        let rb = temp_root("plain");
        let b = state_at(&rb);
        let body = Bytes::from_static("\u{feff}{\"owner_name\": \"Plain Example\", \"country_code\": \"1\"}".as_bytes());
        let preview = preview_upload(&b, "settings.json", body).await.unwrap();
        assert_eq!(preview["items"], json!([{"id": "settings"}]));
        assert_eq!(preview["warnings"], json!(["plain_settings_file"]));
        assert_eq!(preview["kind"], Value::Null);
        let done = apply(&b, preview["token"].as_str().unwrap(), &["settings"]).await.unwrap();
        assert_eq!(done["warnings"], json!(["plain_settings_file"]));
        assert_eq!(b.settings_store.snapshot().owner_name, "Plain Example");

        // With credentials it lists keys too; the password counts only there.
        let body = Bytes::from_static(br#"{"owner_name": "Plain Example", "gemini_api_key": "test-fake-plain-0006", "ui_password": "test-fake-pw"}"#);
        let preview = preview_upload(&b, "settings.json", body).await.unwrap();
        assert_eq!(preview_items(&preview), ["settings", "keys"]);
        assert_eq!(preview["changes_password"], true);
        let preview = preview_upload(&b, "keys.json", Bytes::from_static(br#"{"deepseek_api_key": "test-fake-old-0007"}"#)).await.unwrap();
        assert_eq!(preview_items(&preview), ["keys"]);
        assert_eq!(preview["changes_password"], false);

        let cases: [&[u8]; 4] = [b"[1, 2]", b"{\"contacts\": []}", b"hello", b"PK\x03\x04 not really a zip"];
        for not_settings in cases {
            let r = preview_upload(&b, "x.json", Bytes::copy_from_slice(not_settings)).await;
            assert_eq!(code(r), "not_backup", "{:?}", String::from_utf8_lossy(not_settings));
        }
        let _ = std::fs::remove_dir_all(&rb);
    }

    #[tokio::test]
    async fn newer_formats_unsafe_paths_and_wrong_checksums_are_refused() {
        let rb = temp_root("bad");
        let b = state_at(&rb);
        let settings = br#"{"owner_name": "Zip Example"}"#;
        let entry = |path: &str, body: &[u8], digest: &str| json!({"path": path, "size": body.len(), "sha256": digest});
        let good = entry("data/config/settings.json", settings, &sha(settings));

        let zip = zip_with(&manifest(2, vec![good.clone()]), &[("data/config/settings.json", settings)]);
        assert_eq!(code(preview_upload(&b, "new.zip", Bytes::from(zip)).await), "too_new");

        let zip = zip_with(&manifest(1, vec![good.clone()]), &[("data/config/settings.json", settings)]);
        let ok = preview_upload(&b, "ok.zip", Bytes::from(zip)).await.unwrap();
        assert_eq!(ok["items"], json!([{"id": "settings"}]));

        let evil = [
            "data/../escape.json",
            "/etc/passwd",
            "data//etc/passwd",
            "data/C:/x.json",
            "data\\config\\settings.json",
            "data/config/../../x.json",
            "config/settings.json",
            "data/config/CON.json",
            "data/nul",
            "data/2026-01-01/lpt1.wav",
            "data/config/Aux.txt",
            "data/2026-01-01/com9",
            "data/config/x.json ",
            "data/config/x.json.",
            "data/config/COM\u{b9}.json",
            "data/lpt\u{b3}",
            "data/2026-01-01/Com\u{b2}.wav",
        ];
        for path in evil {
            let zip = zip_with(&manifest(1, vec![entry(path, settings, &sha(settings))]), &[(path, settings)]);
            assert_eq!(code(preview_upload(&b, "evil.zip", Bytes::from(zip)).await), "damaged", "{path}");
        }

        let wrong = entry("data/config/settings.json", settings, &sha(b"something else"));
        let zip = zip_with(&manifest(1, vec![wrong]), &[("data/config/settings.json", settings)]);
        assert_eq!(code(preview_upload(&b, "sum.zip", Bytes::from(zip)).await), "damaged");

        let missing = zip_with(&manifest(1, vec![good]), &[]);
        assert_eq!(code(preview_upload(&b, "missing.zip", Bytes::from(missing)).await), "damaged");

        let big = entry("data/contacts.json", settings, &sha(settings));
        let mut m = manifest(1, vec![big]);
        m["files"][0]["size"] = json!(SMALL_FILE_MAX_BYTES + 1);
        let zip = zip_with(&m, &[("data/contacts.json", settings)]);
        assert_eq!(code(preview_upload(&b, "big.zip", Bytes::from(zip)).await), "too_large");

        let not_ours = zip_with(&json!({"format": "something-else", "version": 1}), &[]);
        assert_eq!(code(preview_upload(&b, "other.zip", Bytes::from(not_ours)).await), "not_backup");
        let _ = std::fs::remove_dir_all(&rb);
    }

    #[test]
    fn only_the_oldest_automatic_backups_go() {
        let dir = temp_root("keep");
        let at = |d: u32| NaiveDate::from_ymd_opt(2026, 1, d).unwrap().and_hms_opt(3, 0, 0).unwrap();
        let autos: Vec<String> = (1..=4).map(|d| file_name(Kind::Scheduled, at(d))).collect();
        let others = [
            file_name(Kind::Manual, at(1)),
            file_name(Kind::BeforeImport, at(1)),
            file_name(Kind::Export, at(1)),
            "notes.txt".to_string(),
            format!("{}.part", autos[0]),
            "bageo-backup-2026010x-030000-auto.zip".to_string(),
        ];
        for n in autos.iter().chain(others.iter()) {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        assert_eq!(prune(&dir, Kind::Scheduled, 2), 2);
        assert!(!dir.join(&autos[0]).exists() && !dir.join(&autos[1]).exists());
        assert!(dir.join(&autos[2]).exists() && dir.join(&autos[3]).exists());
        for n in &others {
            assert!(dir.join(n).exists(), "{n} stays");
        }
        assert_eq!(last_auto(&dir), Some(at(4)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_are_read_back() {
        let at = NaiveDate::from_ymd_opt(2026, 11, 19).unwrap().and_hms_opt(8, 5, 9).unwrap();
        for kind in [Kind::Scheduled, Kind::Manual, Kind::BeforeImport, Kind::Export] {
            let name = file_name(kind, at);
            assert_eq!(parse_name(&name), Some((kind, at)), "{name}");
        }
        assert_eq!(file_name(Kind::Scheduled, at), "bageo-backup-20261119-080509-auto.zip");
        assert_eq!(file_name(Kind::Export, at), "bageo-export-20261119-080509.zip");
        for bad in ["bageo-backup-20261119-080509-auto.zip/../x", "../bageo-export-20261119-080509.zip", "bageo-backup-20261119-080509.zip", "bageo-export-2026111-0805099.zip"] {
            assert_eq!(parse_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn automatic_backups_fall_due_daily_weekly_and_monthly() {
        let t = |y: i32, m: u32, d: u32, h: u32| NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, 0, 0).unwrap();
        assert_eq!(next_due("daily", t(2026, 3, 1, 9)), Some(t(2026, 3, 2, 9)));
        assert_eq!(next_due("weekly", t(2026, 3, 1, 9)), Some(t(2026, 3, 8, 9)));
        assert_eq!(next_due("monthly", t(2026, 3, 15, 9)), Some(t(2026, 4, 15, 9)));
        assert_eq!(next_due("monthly", t(2026, 1, 31, 9)), Some(t(2026, 2, 28, 9)));
        assert_eq!(next_due("monthly", t(2028, 1, 31, 9)), Some(t(2028, 2, 29, 9)));
        assert_eq!(next_due("monthly", t(2026, 12, 31, 9)), Some(t(2027, 1, 31, 9)));
        assert_eq!(next_due("off", t(2026, 3, 1, 9)), None);

        let now = t(2026, 5, 10, 12);
        assert_eq!(next_run("weekly", None, None, now), Some(now), "never made: now (also after the computer was off)");
        assert_eq!(next_run("weekly", Some(t(2026, 5, 1, 12)), None, now), Some(t(2026, 5, 8, 12)));
        let failed = now - chrono::Duration::minutes(10);
        assert_eq!(next_run("weekly", Some(t(2026, 5, 1, 12)), Some(failed), now), Some(failed + chrono::Duration::hours(1)));
        assert_eq!(next_run("off", None, None, now), None);
    }

    #[tokio::test]
    async fn nothing_is_restored_during_a_call() {
        let rb = temp_root("call");
        let b = state_at(&rb);
        let preview = preview_upload(&b, "settings.json", Bytes::from_static(br#"{"owner_name": "Call Example"}"#)).await.unwrap();
        let token = preview["token"].as_str().unwrap().to_string();
        let _call = b.store.start("pixel-test-1", "+15555550188", 16000, "ringing").await.expect("call started");
        assert_eq!(apply(&b, &token, &["settings"]).await.expect_err("refused"), (StatusCode::CONFLICT, "call_active"));
        assert!(pending().contains_key(&token), "the preview can be used after the call");
        assert_ne!(b.settings_store.snapshot().owner_name, "Call Example");
        assert!(names_in(&default_dir(&rb)).is_empty(), "no before_import copy either");
        let _ = std::fs::remove_dir_all(&rb);
    }

    #[tokio::test]
    async fn call_records_are_added_and_none_here_go() {
        let (ra, rb) = (temp_root("ca"), temp_root("cb"));
        // A record naming files the restore does not bring back, or outside the calls.
        let mut evil = call_json("call-evil", "2026-02-03");
        evil["audio_path"] = json!("../../../outside.txt");
        evil["tts_audio_path"] = json!("config/settings.json");
        evil["remote_audio_path"] = json!("/etc/hosts");
        evil["voicemail_path"] = json!("2026-02-03/missing.wav");
        evil["ai_audio_path"] = json!("2026-02-03/call-a.wav");
        let calls = json!({"calls": [call_json("call-a", "2026-02-03"), evil, call_json("../escape", "2026-02-03")]});
        std::fs::write(ra.join("index.json"), calls.to_string()).unwrap();
        std::fs::create_dir_all(ra.join("2026-02-03")).unwrap();
        std::fs::write(ra.join("2026-02-03").join("call-a.wav"), b"RIFF test recording").unwrap();
        std::fs::write(ra.join("2026-02-03").join("call-x.pcm"), b"still recording").unwrap();
        std::fs::write(rb.join("index.json"), json!({"calls": [call_json("call-b", "2026-02-04")]}).to_string()).unwrap();
        let a = state_at(&ra);
        let now = crate::sms_store::now_ms();
        let text = a.sms.push(now, 15, crate::sms_store::NewSms {
            received_ms: now,
            from: "+15555550111",
            to: "+15555550122",
            text: "Test message",
            serial: "pixel-test-1",
            device: "Test phone",
            reference: "ref-test-1",
            sub_id: None,
        });
        let crate::sms_store::Pushed::Stored(text_id) = text else { panic!("stored") };
        let task = crate::tasks::draft_from_voicemail("call-a", "pixel-test-1", "+15555550100", "2026-02-03/call-a.wav", crate::tasks::ReplyMethod::None, None, false);
        let task = a.task_store.create(task).await.unwrap();
        a.settings_store.apply_patch(&json!({"backup_include_calls": true, "owner_name": "Calls Example"})).unwrap();

        let entry = run_backup(&a, Kind::Manual).await.unwrap();
        assert_eq!((entry.include_calls, entry.encrypted), (Some(true), false));
        let path = default_dir(&ra).join(&entry.name);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let held = inspect(&path).unwrap();
        assert_eq!(held.manifest.as_ref().unwrap().items, ["settings", "keys", "calls", "sms", "tasks"]);
        assert!(held.files.iter().all(|w| !w.rel.ends_with(".pcm")), "a call still being recorded stays out");
        assert!(held.files.iter().all(|w| !w.rel.starts_with("backups/")));

        let b = state_at(&rb);
        let preview = preview_upload(&b, &entry.name, Bytes::from(std::fs::read(&path).unwrap())).await.unwrap();
        assert_eq!((preview["has_calls"].as_bool(), preview["calls_count"].as_u64()), (Some(true), Some(3)));
        // The call records alone: the settings here stay.
        let done = apply(&b, preview["token"].as_str().unwrap(), &["calls", "sms", "tasks"]).await.unwrap();
        assert_eq!(done["ok"], true);
        assert_ne!(b.settings_store.snapshot().owner_name, "Calls Example");
        let ids: Vec<String> = b.store.list().await.into_iter().map(|c| c.id).collect();
        assert!(ids.contains(&"call-a".to_string()) && ids.contains(&"call-b".to_string()), "{ids:?}");
        assert!(!ids.iter().any(|i| i.contains("escape")), "a record id that is no file name stays out");
        let evil = b.store.get("call-evil").await.expect("restored");
        assert_eq!(
            (evil.audio_path, evil.tts_audio_path, evil.remote_audio_path, evil.voicemail_path, evil.ai_audio_path.as_deref()),
            (None, None, None, None, Some("2026-02-03/call-a.wav"))
        );
        assert_eq!(std::fs::read(rb.join("2026-02-03").join("call-a.wav")).unwrap(), b"RIFF test recording");
        assert!(b.sms.get(text_id).is_some());
        assert!(b.task_store.get(&task.id).await.is_some());
        let _ = std::fs::remove_dir_all(&ra);
        let _ = std::fs::remove_dir_all(&rb);
    }

    #[tokio::test]
    async fn a_backup_folder_that_cannot_be_written_is_refused() {
        let root = temp_root("dir");
        let rt = state_at(&root).settings_store.snapshot();
        let blocker = root.join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        for bad in [blocker.join("backups").to_string_lossy().into_owned(), "relative/folder".to_string()] {
            let r = check_settings_patch(&json!({"backup_dir": bad}), &rt).expect("refused");
            assert_eq!(r.status(), StatusCode::BAD_REQUEST);
            let body = axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap();
            let v: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["code"], "backup_dir_unwritable");
        }
        let fine = root.join("fine");
        assert!(check_settings_patch(&json!({"backup_dir": fine.to_string_lossy()}), &rt).is_none());
        assert!(fine.is_dir(), "made when saved");
        assert!(check_settings_patch(&json!({"backup_dir": ""}), &rt).is_none());
        assert!(check_settings_patch(&json!({"owner_name": "x"}), &rt).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn big_zips_are_written_and_read_with_zip64_directories() {
        let mut z = ZipOut::new(io::Cursor::new(Vec::new()), Local::now().naive_local());
        for i in 0..66_000u32 {
            z.add(&format!("data/2026-01-01/f{i}.wav"), &mut &b"a"[..], 1).unwrap();
        }
        let bytes = z.finish().unwrap().into_inner();
        let path = std::env::temp_dir().join(format!("bageo-zip64-{}.zip", uuid::Uuid::new_v4().simple()));
        std::fs::write(&path, &bytes).unwrap();
        let mut zin = ZipIn::open(File::open(&path).unwrap()).unwrap();
        assert_eq!(zin.entries.len(), 66_000);
        let mut out = Vec::new();
        zin.copy_checked("data/2026-01-01/f65999.wav", 1, &sha(b"a"), &mut out).unwrap();
        assert_eq!(out, b"a");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_failure_notice_is_written_in_every_language() {
        for lang in crate::locale::ALL {
            for c in ["dir_unwritable", "disk_full", "io"] {
                let (title, body) = failure_text(lang, c);
                assert!(!title.is_empty() && body.len() > 20, "{lang:?} {c}");
                assert!(!body.contains("dir_unwritable") && !body.contains("disk_full"), "{lang:?}");
            }
        }
    }

    #[tokio::test]
    async fn twelve_exports_at_once_all_work() {
        let ra = temp_root("many");
        let a = keyed_state(&ra, "Sam Example").await;
        let runs = (0..12).map(|n| export_file(&a, DEFAULT_ITEMS.to_vec(), (n % 3 == 0).then(|| FAKE_BACKUP_PW.to_string())));
        let done = futures_util::future::join_all(runs).await;
        let paths: HashSet<PathBuf> = done.into_iter().map(|r| r.expect("export").1).collect();
        assert_eq!(paths.len(), 12);
        for p in &paths {
            let sealed = p.extension().is_some_and(|e| e == "bageo");
            assert!(if sealed { look(p).is_ok() } else { inspect(p).is_ok() });
            let _ = std::fs::remove_file(p);
        }
        let _ = std::fs::remove_dir_all(&ra);
    }

    #[tokio::test]
    async fn backups_of_the_same_second_get_names_of_their_own() {
        let root = temp_root("same");
        let st = state_at(&root);
        let lay = Layout::of(&st);
        let dir = default_dir(&root);
        let now = Local::now();
        let first = write_backup_file(&lay, b"{}".to_vec(), &dir, Kind::Manual, &DEFAULT_ITEMS, now, None).unwrap();
        let second = write_backup_file(&lay, b"{}".to_vec(), &dir, Kind::Manual, &DEFAULT_ITEMS, now, None).unwrap();
        assert_ne!(first.name, second.name);
        assert!(dir.join(&first.name).is_file() && dir.join(&second.name).is_file());
        assert!(names_in(&dir).iter().all(|n| !n.ends_with(".tmp")), "no temporary file left");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_backup_file_can_be_deleted_and_nothing_else() {
        let root = temp_root("delete");
        let st = state_at(&root);
        let lay = Layout::of(&st);
        let dir = default_dir(&root);
        let made = write_backup_file(&lay, b"{}".to_vec(), &dir, Kind::Manual, &DEFAULT_ITEMS, Local::now(), None).unwrap();
        // Other files in the folder, and a backup-looking file outside it.
        std::fs::write(dir.join("notes.txt"), b"keep").unwrap();
        let outside = root.join("bageo-backup-20260101-000000-manual.zip");
        std::fs::write(&outside, b"outside").unwrap();
        for (name, code) in [
            ("../bageo-backup-20260101-000000-manual.zip", "bad_request"),
            ("..\\bageo-backup-20260101-000000-manual.zip", "bad_request"),
            ("sub/bageo-backup-20260101-000000-manual.zip", "bad_request"),
            ("C:bageo-backup-20260101-000000-manual.zip", "bad_request"),
            ("..", "bad_request"),
            ("", "bad_request"),
            ("notes.txt", "not_found"),
            ("bageo-backup-20990101-000000-manual.zip", "not_found"),
        ] {
            assert_eq!(delete_saved(&st, name).unwrap_err().1, code, "{name}");
        }
        assert!(outside.is_file() && dir.join("notes.txt").is_file());
        // A symbolic link with a backup's name is not followed.
        #[cfg(unix)]
        {
            let link = "bageo-backup-20260102-000000-auto.zip";
            std::os::unix::fs::symlink(&outside, dir.join(link)).unwrap();
            assert_eq!(delete_saved(&st, link).unwrap_err().1, "not_found");
            assert!(outside.is_file());
        }
        // Not while a backup or a restore is under way.
        {
            let ds = dir_state(&lay.data);
            let _running = ds.running.try_lock().unwrap();
            assert_eq!(delete_saved(&st, &made.name).unwrap_err(), (StatusCode::CONFLICT, "busy"));
        }
        assert!(dir.join(&made.name).is_file());
        delete_saved(&st, &made.name).unwrap();
        assert!(!dir.join(&made.name).exists());
        assert!(list_backups(&dir).iter().all(|e| e.name != made.name));
        assert_eq!(delete_saved(&st, &made.name).unwrap_err().1, "not_found");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn a_new_preview_pushes_out_the_oldest_and_closed_ones_go() {
        let rb = temp_root("three");
        let b = state_at(&rb);
        let mut tokens = Vec::new();
        for n in 0..4 {
            let v = preview_upload(&b, "settings.json", Bytes::from(format!("{{\"owner_name\": \"Wait {n}\"}}"))).await.unwrap();
            tokens.push(v["token"].as_str().unwrap().to_string());
        }
        let uploads = || names_in(&staging_dir(&rb)).into_iter().filter(|n| n.starts_with("upload-")).count();
        assert_eq!((pending_load(&rb).0, uploads()), (PENDING_MAX, PENDING_MAX));
        assert_eq!(code(apply(&b, &tokens[0], &["settings"]).await), "expired", "the oldest went, with its upload");
        // A closed dialog: gone at once. Another folder's or an unknown token: nothing.
        discard(&rb, &tokens[3]);
        discard(&rb, "no-such-preview");
        discard(&std::env::temp_dir(), &tokens[2]);
        assert_eq!(code(apply(&b, &tokens[3], &["settings"]).await), "expired");
        assert_eq!((pending_load(&rb).0, uploads()), (2, 2));
        apply(&b, &tokens[2], &["settings"]).await.unwrap();
        assert_eq!(b.settings_store.snapshot().owner_name, "Wait 2");
        // Restoring from the list makes room the same way.
        let entry = run_backup(&b, Kind::Manual).await.unwrap();
        for _ in 0..4 {
            preview_saved(&b, &entry.name).await.unwrap();
        }
        assert_eq!(pending_load(&rb).0, PENDING_MAX);
        assert_eq!(code(apply(&b, &tokens[1], &["settings"]).await), "expired");
        assert_eq!(uploads(), 0, "every upload went with its preview");
        let ds = dir_state(&rb);
        let one = upload_slot(ds.clone());
        let two = upload_slot(ds.clone());
        assert!(one.is_some() && two.is_some() && upload_slot(ds.clone()).is_none());
        drop(one);
        assert!(upload_slot(ds).is_some());
        let _ = std::fs::remove_dir_all(&rb);
    }

    #[tokio::test]
    async fn limits_go_by_what_the_files_are() {
        let rb = temp_root("limits");
        let b = state_at(&rb);
        // Saying it holds call records does not lift the limit of the other files.
        let body = br#"{"owner_name": "Limit Example"}"#;
        let files: Vec<Value> = (0..3).map(|n| json!({"path": format!("data/config/extra{n}.json"), "size": SMALL_FILE_MAX_BYTES, "sha256": sha(body)})).collect();
        let mut m = manifest(1, files);
        m["include_calls"] = json!(true);
        m["items"] = json!(["settings", "calls"]);
        assert_eq!(code(preview_upload(&b, "lie.zip", Bytes::from(zip_with(&m, &[]))).await), "too_large");
        // Packed far tighter than a real file.
        let blank = vec![b' '; 1 << 20];
        let m = manifest(1, vec![json!({"path": "data/contacts.json", "size": blank.len(), "sha256": sha(&blank)})]);
        let zip = zip_with(&m, &[("data/contacts.json", &blank)]);
        assert_eq!(code(preview_upload(&b, "bomb.zip", Bytes::from(zip)).await), "damaged");
        // A recording is kept as it is: silence is no bomb.
        let mut z = ZipOut::new(io::Cursor::new(Vec::new()), Local::now().naive_local());
        z.add("data/2026-01-01/quiet.wav", &mut &blank[..], blank.len() as u64).unwrap();
        let path = rb.join("quiet.zip");
        std::fs::write(&path, z.finish().unwrap().into_inner()).unwrap();
        let zin = ZipIn::open(File::open(&path).unwrap()).unwrap();
        let e = zin.entries["data/2026-01-01/quiet.wav"];
        assert_eq!((e.method, e.packed), (0, e.size));
        let _ = std::fs::remove_dir_all(&rb);
    }

    #[test]
    fn keys_leave_without_a_password_only_for_this_computer() {
        let host = |h: &str| {
            let mut m = HeaderMap::new();
            m.insert(header::HOST, h.parse().unwrap());
            m
        };
        for ok in ["localhost:8787", "127.0.0.1:8787", "[::1]:8787", "LOCALHOST", "app.localhost:8787", "127.0.0.1"] {
            assert!(loopback_host(&host(ok)), "{ok}");
        }
        for bad in ["bageo.local:8787", "192.0.2.10:8787", "evil.example", "127.0.0.1.evil.example", "[2001:db8::1]:8787", ""] {
            assert!(!loopback_host(&host(bad)), "{bad}");
        }
        let peer = |ip: &str| {
            let mut e = Extensions::new();
            e.insert(ConnectInfo(std::net::SocketAddr::new(ip.parse().unwrap(), 50000)));
            e
        };
        let plain = host("localhost:8787");
        assert!(from_this_computer(&peer("127.0.0.1"), &plain));
        assert!(from_this_computer(&peer("::1"), &plain));
        assert!(!from_this_computer(&peer("192.0.2.10"), &plain));
        let mut proxied = plain.clone();
        proxied.insert("x-forwarded-for", "198.51.100.7".parse().unwrap());
        assert!(!from_this_computer(&peer("127.0.0.1"), &proxied));
        assert!(!from_this_computer(&Extensions::new(), &plain));
    }

    #[cfg(unix)]
    #[test]
    fn free_room_on_the_disk_is_known() {
        assert!(free_space(&std::env::temp_dir()).is_some_and(|n| n > 0));
    }

    #[test]
    fn windows_device_names_are_refused() {
        for bad in ["CON", "con.json", "Nul .txt", "COM1", "lpt9.wav", "COM\u{b9}", "com\u{b2}.json", "LPT\u{b3}.txt", "Lpt\u{b9}"] {
            assert!(reserved_name(bad), "{bad}");
        }
        for fine in ["CONTACTS.json", "COM0", "COM10", "LPT\u{2074}", "COM", "console.json", "com1x.json", "aux_notes.json", "2026-01-01"] {
            assert!(!reserved_name(fine), "{fine}");
        }
        for bad in ["data/config/COM\u{b9}.json", "data/lpt\u{b3}", "data/2026-01-01/Com\u{b2}.wav"] {
            assert_eq!(safe_rel(bad), None, "{bad}");
        }
        assert_eq!(safe_rel("data/config/settings.json"), Some("config/settings.json"));
    }

    #[tokio::test]
    async fn a_restore_never_shortens_how_long_records_are_kept() {
        let rb = temp_root("keepdays");
        let old_day = (Local::now() - chrono::Duration::days(60)).format("%Y-%m-%d").to_string();
        std::fs::write(rb.join("index.json"), json!({"calls": [call_json("call-old", &old_day)]}).to_string()).unwrap();
        std::fs::create_dir_all(rb.join(&old_day)).unwrap();
        std::fs::write(rb.join(&old_day).join("call-old.wav"), b"RIFF old recording").unwrap();
        let b = state_at(&rb);
        b.settings_store.apply_patch(&json!({"retention_days": 90, "backup_keep": 20})).unwrap();
        let purge = |days: u32| crate::calls_http::purge_expired_records(&b.store, &b.task_store, &b.errands, &b.sms, days);
        // The file says nothing, or a shorter time.
        let files: [&[u8]; 2] = [br#"{"owner_name": "Days Example"}"#, br#"{"owner_name": "Days Example", "retention_days": 7, "backup_keep": 2}"#];
        for body in files {
            let preview = preview_upload(&b, "settings.json", Bytes::copy_from_slice(body)).await.unwrap();
            apply(&b, preview["token"].as_str().unwrap(), &["settings"]).await.unwrap();
            let rt = b.settings_store.snapshot();
            assert_eq!((rt.owner_name.as_str(), rt.retention_days, rt.backup_keep), ("Days Example", 90, 20));
            assert!(std::fs::read_to_string(rb.join("config").join("settings.json")).unwrap().contains("90"));
            // The hourly clean-up after it.
            purge(rt.retention_days).await;
            assert!(b.store.get("call-old").await.is_some(), "the 60-day-old call stays");
            assert!(rb.join(&old_day).join("call-old.wav").is_file(), "and its recording");
        }
        // A longer one is taken; the console alone shortens it.
        let preview = preview_upload(&b, "settings.json", Bytes::from_static(br#"{"retention_days": 120, "backup_keep": 30}"#)).await.unwrap();
        apply(&b, preview["token"].as_str().unwrap(), &["settings"]).await.unwrap();
        let rt = b.settings_store.snapshot();
        assert_eq!((rt.retention_days, rt.backup_keep), (120, 30));
        assert_eq!(b.settings_store.apply_patch(&json!({"retention_days": 15})).unwrap().retention_days, 15);
        purge(15).await;
        assert!(b.store.get("call-old").await.is_none(), "the call was one a clean-up does take");
        let _ = std::fs::remove_dir_all(&rb);
    }

    /// The settings of the item `keys` as the agent uses them now, field by field.
    fn key_group(st: &CallsState) -> Vec<(&'static str, Value)> {
        use crate::settings_store::{CONNECTION_FIELDS, CREDENTIAL_FIELDS};
        let v = serde_json::to_value(&*st.settings_store.snapshot()).unwrap();
        CREDENTIAL_FIELDS.into_iter().chain(CONNECTION_FIELDS).map(|f| (f, v.get(f).cloned().unwrap_or(Value::Null))).collect()
    }

    #[tokio::test]
    async fn putting_back_the_copy_from_before_undoes_every_key_and_connection() {
        const OWN_MCP: &str = "test-fake-mcp-own-0010";
        const THEIR_MCP: &str = "test-fake-mcp-attacker-0011";
        let rb = temp_root("undo");
        let b = state_at(&rb);
        b.settings_store
            .apply_patch(&json!({
                "owner_name": "Own Example",
                "gemini_api_key": FAKE_GEMINI,
                "mcp_enabled": true,
                "mcp_token": OWN_MCP,
                "telegram_chat_id": "fake-chat-own",
                "callback_whitelist": ["+15555550100"],
            }))
            .unwrap();
        let before = key_group(&b);
        let attacker = json!({
            "owner_name": "Attacker Example",
            "mcp_enabled": true,
            "mcp_token": THEIR_MCP,
            "mcp_calls": "direct",
            "mcp_errands_per_hour": 30,
            "mcp_allowed_origins": ["https://attacker.example"],
            "telegram_control": true,
            "telegram_control_users": ["1000001"],
            "notify_gateway_url": "https://gateway.attacker.example/notify",
            "trtc_bridge_url": "wss://bridge.attacker.example/ws",
            "gmail_user": "attacker@example.com",
            "callback_whitelist": ["*"],
            "whitelist_forward_number": "+15555550666",
            "whitelist_urgent_call": true,
        });
        let preview = preview_upload(&b, "settings.json", Bytes::from(attacker.to_string())).await.unwrap();
        assert_eq!(preview_items(&preview), ["settings", "keys"]);
        assert_eq!(preview["changes_connections"], true);
        let done = apply(&b, preview["token"].as_str().unwrap(), &["settings", "keys"]).await.unwrap();
        let rt = b.settings_store.snapshot();
        assert_eq!(
            (rt.mcp_token.as_str(), rt.telegram_chat_id.as_str(), rt.callback_whitelist.clone()),
            (THEIR_MCP, "fake-chat-own", vec!["*".to_string()]),
            "the file's keys where it has them, this computer's where it has none"
        );

        // Everything the copy from before holds, put back.
        let name = done["before_import"].as_str().unwrap();
        let again = preview_saved(&b, name).await.unwrap();
        assert_eq!(preview_items(&again), ["settings", "keys"]);
        assert_eq!(again["changes_connections"], true);
        apply(&b, again["token"].as_str().unwrap(), &preview_items(&again)).await.unwrap();
        let after = key_group(&b);
        for ((f, was), (_, now)) in before.iter().zip(&after) {
            assert_eq!(now, was, "{f}");
        }
        assert_eq!(b.settings_store.snapshot().owner_name, "Own Example");
        let ctrl = crate::control::Controller::new(b.clone());
        let bearer = |t: &str| {
            let mut h = HeaderMap::new();
            h.insert(header::AUTHORIZATION, format!("Bearer {t}").parse().unwrap());
            h
        };
        let ping = br#"{"jsonrpc": "2.0", "id": 1, "method": "ping"}"#;
        assert_eq!(crate::mcp::answer(&ctrl, &bearer(THEIR_MCP), ping).await.0, StatusCode::UNAUTHORIZED);
        assert_ne!(crate::mcp::answer(&ctrl, &bearer(OWN_MCP), ping).await.0, StatusCode::UNAUTHORIZED);

        // A file with only one key leaves the rest of the group as it is here: nothing else changes.
        let one = preview_upload(&b, "keys.json", Bytes::from(json!({"gemini_api_key": FAKE_GEMINI}).to_string())).await.unwrap();
        assert_eq!((one["changes_password"].as_bool(), one["changes_connections"].as_bool()), (Some(false), Some(false)));
        discard(&rb, one["token"].as_str().unwrap());
        let _ = std::fs::remove_dir_all(&rb);
    }
    /// The code of a refused settings save.
    async fn refused_code(r: Option<Response>) -> String {
        let r = r.expect("refused");
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap();
        serde_json::from_slice::<Value>(&body).unwrap()["code"].as_str().unwrap_or_default().to_string()
    }

    #[tokio::test]
    async fn the_backup_password_is_checked_when_saved() {
        let root = temp_root("bkpw");
        let st = state_at(&root);
        let rt = st.settings_store.snapshot();
        assert_eq!(refused_code(check_settings_patch(&json!({"backup_password": "short"}), &rt)).await, "backup_password_short");
        assert_eq!(refused_code(check_settings_patch(&json!({"backup_encrypt": true}), &rt)).await, "backup_password_missing");
        assert!(check_settings_patch(&json!({"backup_encrypt": true, "backup_password": FAKE_BACKUP_PW}), &rt).is_none());
        assert!(check_settings_patch(&json!({"backup_encrypt": false}), &rt).is_none());
        st.settings_store.apply_patch(&json!({"backup_encrypt": true, "backup_password": FAKE_BACKUP_PW})).unwrap();
        let rt = st.settings_store.snapshot();
        assert!(rt.backup_encrypt);
        // Removing the password while encryption is on: refused; with it off: fine.
        assert_eq!(refused_code(check_settings_patch(&json!({"backup_password_clear": true}), &rt)).await, "backup_password_missing");
        assert!(check_settings_patch(&json!({"backup_password_clear": true, "backup_encrypt": false}), &rt).is_none());
        assert!(check_settings_patch(&json!({"owner_name": "x"}), &rt).is_none());
        // Never shown: only whether one is set.
        let shown = st.settings_store.masked_json();
        assert_eq!(shown["backup_password_set"], true);
        assert_ne!(shown["backup_password"], FAKE_BACKUP_PW);
        // A settings file asking for encryption without a password backs up as before.
        let rt = st.settings_store.apply_patch(&json!({"backup_password_clear": true})).unwrap();
        assert!(!rt.backup_encrypt);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn encrypted_backups_open_with_the_saved_password_or_a_typed_one() {
        let ra = temp_root("seal");
        let a = keyed_state(&ra, "Sam Example").await;
        a.settings_store.apply_patch(&json!({"backup_encrypt": true, "backup_password": FAKE_BACKUP_PW})).unwrap();
        let entry = run_backup(&a, Kind::Manual).await.unwrap();
        assert!(entry.name.ends_with("-manual.bageo") && entry.encrypted, "{}", entry.name);
        assert_eq!(entry.include_calls, Some(false));
        let path = default_dir(&ra).join(&entry.name);
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(backup_crypt::MAGIC));
        for plain in ["PK\x03\x04", MANIFEST_NAME, "Sam Example", "Alex Example", FAKE_GEMINI, FAKE_BACKUP_PW] {
            assert!(!has(&bytes, plain), "{plain} shows");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let listed = list_backups(&default_dir(&ra));
        assert_eq!((listed[0].name.as_str(), listed[0].encrypted, listed[0].include_calls), (entry.name.as_str(), true, None));
        assert_eq!(parse_name(&entry.name).map(|(k, _)| k), Some(Kind::Manual));

        // From the list: the password saved here opens it.
        let p = preview_saved(&a, &entry.name).await.unwrap();
        assert_eq!((p["encrypted"].as_bool(), p["needs_password"].as_bool()), (Some(true), Some(false)));
        assert_eq!(preview_items(&p), ["settings", "keys", "contacts"]);
        discard(&ra, p["token"].as_str().unwrap());
        assert!(names_in(&staging_dir(&ra)).iter().all(|n| !n.starts_with("open-")), "the opened copy went with it");

        // Another password saved here: the owner types the one of the file.
        a.settings_store.apply_patch(&json!({"backup_password": "test-fake-backup-pass-0006"})).unwrap();
        let p = preview_saved(&a, &entry.name).await.unwrap();
        assert_eq!((p["needs_password"].as_bool(), p.get("items")), (Some(true), None));
        let token = p["token"].as_str().unwrap();
        assert_eq!(code(apply(&a, token, &["settings"]).await), "bad_items", "nothing to restore before it is open");
        assert_eq!(code(unlock(&a, token, "test-fake-backup-pass-0006").await), "wrong_password");
        let p = unlock(&a, token, FAKE_BACKUP_PW).await.unwrap();
        assert_eq!((p["encrypted"].as_bool(), p["needs_password"].as_bool()), (Some(true), Some(false)));
        let done = apply(&a, token, &["settings", "contacts"]).await.unwrap();
        assert_eq!(done["ok"], true);
        // The copy made before the restore is encrypted too, with the password of now.
        let before = done["before_import"].as_str().unwrap();
        assert!(before.ends_with("-before-import.bageo"), "{before}");
        assert!(std::fs::read(default_dir(&ra).join(before)).unwrap().starts_with(backup_crypt::MAGIC));
        let _ = std::fs::remove_dir_all(&ra);
    }

    #[tokio::test]
    async fn an_encrypted_export_needs_its_password_and_five_wrong_ones_end_it() {
        let (ra, rb) = (temp_root("sexp"), temp_root("simp"));
        let a = keyed_state(&ra, "Sam Example").await;
        let (name, path) = export_file(&a, DEFAULT_ITEMS.to_vec(), Some(FAKE_BACKUP_PW.to_string())).await.unwrap();
        assert!(name.starts_with("bageo-export-") && name.ends_with(".bageo"), "{name}");
        assert_eq!(parse_name(&name).map(|(k, _)| k), Some(Kind::Export));
        let sealed = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(!has(&sealed, FAKE_GEMINI) && !has(&sealed, "Sam Example"));

        let b = state_at(&rb);
        let p = preview_upload(&b, &name, Bytes::from(sealed.clone())).await.unwrap();
        assert_eq!((p["encrypted"].as_bool(), p["needs_password"].as_bool(), p.get("items")), (Some(true), Some(true), None));
        let token = p["token"].as_str().unwrap().to_string();
        let started = Instant::now();
        for n in 1..UNLOCK_TRIES {
            assert_eq!(code(unlock(&b, &token, &format!("test-fake-wrong-{n:04}")).await), "wrong_password");
        }
        assert!(started.elapsed() >= UNLOCK_GAP * (UNLOCK_TRIES - 2), "tries wait for each other");
        assert_eq!(code(unlock(&b, &token, "test-fake-wrong-0005").await), "expired");
        assert_eq!(code(unlock(&b, &token, FAKE_BACKUP_PW).await), "expired", "even the right one, once it went");
        assert!(names_in(&staging_dir(&rb)).is_empty(), "the upload went too");

        let p = preview_upload(&b, &name, Bytes::from(sealed)).await.unwrap();
        let token = p["token"].as_str().unwrap();
        let p = unlock(&b, token, &format!("  {FAKE_BACKUP_PW} ")).await.unwrap();
        assert_eq!(preview_items(&p), ["settings", "keys", "contacts"]);
        assert_eq!(p["encrypted"], true);
        apply(&b, token, &["settings", "keys", "contacts"]).await.unwrap();
        let rt = b.settings_store.snapshot();
        assert_eq!((rt.owner_name.as_str(), rt.gemini_api_key.as_str()), ("Sam Example", FAKE_GEMINI));
        assert_eq!(b.contacts.list("").len(), 1);
        // A broken file says so before any password.
        let mut cut = backup_crypt::MAGIC.to_vec();
        cut.extend([1u8; 20]);
        assert_eq!(code(preview_upload(&b, "cut.bageo", Bytes::from(cut)).await), "damaged");
        let _ = std::fs::remove_dir_all(&ra);
        let _ = std::fs::remove_dir_all(&rb);
    }

    #[tokio::test]
    async fn the_backup_password_never_leaves_with_the_settings_alone() {
        let ra = temp_root("pwset");
        let a = keyed_state(&ra, "Sam Example").await;
        a.settings_store.apply_patch(&json!({"backup_encrypt": true, "backup_password": FAKE_BACKUP_PW})).unwrap();
        let (_, bytes) = export_bytes(&a, &["settings"]).await;
        let plain = unpacked(&bytes);
        assert!(has(&plain, "backup_encrypt") && has(&plain, "Sam Example"));
        assert!(!has(&plain, FAKE_BACKUP_PW) && !has(&plain, "backup_password"));
        let (_, bytes) = export_bytes(&a, &["keys"]).await;
        assert!(has(&unpacked(&bytes), FAKE_BACKUP_PW), "it is one of the keys");
        let _ = std::fs::remove_dir_all(&ra);
    }

    #[tokio::test]
    async fn download_tickets_work_once_and_only_for_two_minutes() {
        let root = temp_root("ticket");
        let st = state_at(&root);
        let entry = run_backup(&st, Kind::Manual).await.unwrap();
        let file = default_dir(&root).join(&entry.name);
        let t = issue_ticket(file.clone(), entry.name.clone(), false).unwrap();
        assert_eq!(t.len(), 64, "256 random bits");
        let first = download(UrlPath(t.clone())).await;
        assert_eq!(first.status(), StatusCode::OK);
        assert!(first.headers()[header::CONTENT_DISPOSITION].to_str().unwrap().contains(&entry.name));
        assert_eq!(download(UrlPath(t)).await.status(), StatusCode::NOT_FOUND, "used up");
        assert!(file.is_file(), "a file of the backup folder stays");

        // An export ticket two minutes old: gone, and so is its file.
        let (name, export) = export_file(&st, vec!["settings"], None).await.unwrap();
        let t = issue_ticket(export.clone(), name, true).unwrap();
        tickets().get_mut(&t).unwrap().made = Instant::now().checked_sub(TICKET_TTL).expect("clock");
        assert_eq!(download(UrlPath(t)).await.status(), StatusCode::NOT_FOUND);
        assert!(!export.exists(), "the export went with its ticket");
        assert_eq!(download(UrlPath("0".repeat(64))).await.status(), StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(&root);
    }
}
