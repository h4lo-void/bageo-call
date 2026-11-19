// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Backups of Bageo's own data: "Back up now", automatic backups (every day,
//! week or month), and every setting in one file for another computer
//! (export / import). Nothing here goes over the network.
//!
//! A backup is a zip file. `bageo-backup.json` at its root ([`Manifest`])
//! lists every other file: under `data/`, with its path inside the data folder
//! (`CALLS_DATA_DIR`), its size and its SHA-256. Without the call records it
//! holds the settings and the small files next to them, the contacts, the
//! number book, phone pairing and SIMs, and AI errands; with them also the call
//! records and recordings, text messages and to-do items. Never the backup
//! folder itself, temporary files, debug output or caches: only the files
//! named in [`item_of`] go in.
//!
//! Importing first answers a preview (nothing changed yet) with a token. The
//! token, used once within 15 minutes, restores it: a `before_import` backup
//! of the settings is made first and put back when restoring fails half-way.
//! Call records, texts and to-do items are only added; the ones already here stay.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use axum::body::{Body, Bytes};
use axum::extract::rejection::BytesRejection;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path as UrlPath, Query, State};
use axum::http::{header, Extensions, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Local, Months, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use crate::calls_http::{api_error, came_through_proxy, token_ok_parts, unauthorized, CallsState, TokenQuery};
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
/// What a backup may unpack to: settings only / with the call records.
const SETTINGS_MAX_BYTES: u64 = 50 << 20;
const CALLS_MAX_BYTES: u64 = 8 << 30;
const SETTINGS_MAX_FILES: usize = 2_000;
const CALLS_MAX_FILES: usize = 500_000;
const MANIFEST_MAX_BYTES: u64 = 64 << 20;
/// A JSON file read whole for the preview (the call list, the texts).
const COUNT_FILE_MAX_BYTES: u64 = 256 << 20;
/// An older single settings.json.
const PLAIN_MAX_BYTES: u64 = 16 << 20;
/// The zip's central directory.
const DIRECTORY_MAX_BYTES: u64 = 256 << 20;
const TOKEN_TTL: Duration = Duration::from_secs(15 * 60);
const BEFORE_IMPORT_KEEP: usize = 5;
const CHECK_EVERY: Duration = Duration::from_secs(10 * 60);
const RETRY_AFTER_HOURS: i64 = 1;
const STAMP: &str = "%Y%m%d-%H%M%S";
const U32_MAX: u64 = 0xFFFF_FFFF;
/// Files this big get zip64 sizes in their local header.
const BIG_FILE: u64 = 0xF000_0000;

/// The parts of a backup, as the preview names them.
pub const ITEMS: [&str; 8] = ["settings", "contacts", "numbers", "phones", "errands", "calls", "sms", "tasks"];
/// The parts that are only added, and only when asked (`with_calls`).
const CALL_ITEMS: [&str; 3] = ["calls", "sms", "tasks"];
/// The parts that are read again by their store once they are back in place.
const STORE_ITEMS: [&str; 4] = ["contacts", "numbers", "phones", "errands"];

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

/// The file name of a backup made at `at` (local time).
pub fn file_name(kind: Kind, at: NaiveDateTime) -> String {
    let stamp = at.format(STAMP);
    match kind {
        Kind::Scheduled => format!("bageo-backup-{stamp}-auto.zip"),
        Kind::Manual => format!("bageo-backup-{stamp}-manual.zip"),
        Kind::BeforeImport => format!("bageo-backup-{stamp}-before-import.zip"),
        Kind::Export => format!("bageo-settings-{stamp}.zip"),
    }
}

/// What a backup file name says (kind and local time); None for any other name.
pub fn parse_name(name: &str) -> Option<(Kind, NaiveDateTime)> {
    if !name.is_ascii() {
        return None;
    }
    let (kind, stamp) = if let Some(rest) = name.strip_prefix("bageo-settings-") {
        (Kind::Export, rest.strip_suffix(".zip")?)
    } else {
        let rest = name.strip_prefix("bageo-backup-")?.strip_suffix(".zip")?;
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
        "phone_sims.json" | "phone-pairing.key" => return Some("phones"),
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

/// The files of a backup (path inside the data folder, where to read it).
/// The settings come from the store (`settings_json`), the way they are saved.
fn collect(lay: &Layout, settings_json: Vec<u8>, with_calls: bool) -> Vec<(String, Source)> {
    let mut out = vec![("config/settings.json".to_string(), Source::Bytes(settings_json))];
    let add = |out: &mut Vec<(String, Source)>, rel: String| {
        if let Some(path) = lay.target(&rel) {
            if is_plain_file(&path) {
                out.push((rel, Source::File(path)));
            }
        }
    };
    for name in names_in(&lay.config) {
        let rel = format!("config/{name}");
        if rel != "config/settings.json" && item_of(&rel).is_some() {
            add(&mut out, rel);
        }
    }
    for rel in ["contacts.json", "phone_sims.json", "phone-pairing.key", "errands.json"] {
        add(&mut out, rel.to_string());
    }
    if with_calls {
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
        let name_b = name.as_bytes();
        let mut h = Vec::with_capacity(50 + name_b.len());
        h.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        h.extend_from_slice(&(if zip64 { 45u16 } else { 20 }).to_le_bytes());
        h.extend_from_slice(&0x0800u16.to_le_bytes());
        h.extend_from_slice(&8u16.to_le_bytes());
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
        let mut size = 0u64;
        let mut enc = flate2::write::DeflateEncoder::new(Counting { inner: &mut self.out, count: 0 }, flate2::Compression::new(level));
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            crc.update(&buf[..n]);
            sha.update(&buf[..n]);
            size += n as u64;
            enc.write_all(&buf[..n])?;
        }
        let packed = enc.finish()?.count;
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
        self.entries.push(CentralEntry { name: name.to_string(), crc: crc.sum(), packed, size, offset });
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
            h.extend_from_slice(&8u16.to_le_bytes());
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

/// A backup zip written to `out`: the `sources`, then the manifest.
fn write_zip<W: Write + Seek>(out: W, sources: Vec<(String, Source)>, kind: Kind, with_calls: bool, at: DateTime<Local>) -> io::Result<W> {
    let mut zip = ZipOut::new(out, at.naive_local());
    let mut files = Vec::with_capacity(sources.len());
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
        files.push(FileEntry { path: name, size, sha256 });
    }
    let manifest = Manifest {
        format: FORMAT.to_string(),
        version: VERSION,
        app_version: APP_VERSION.to_string(),
        created_at: at.to_rfc3339_opts(SecondsFormat::Secs, false),
        kind: kind.as_str().to_string(),
        include_calls: with_calls,
        files,
    };
    let body = serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)?;
    zip.add(MANIFEST_NAME, &mut body.as_slice(), body.len() as u64)?;
    zip.finish()
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
    if rel.split('/').any(|c| c.is_empty() || c == "." || c == "..") {
        return None;
    }
    Some(rel)
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
    /// Its console password and backup folder.
    settings: Option<(String, String)>,
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
    let text = std::str::from_utf8(&bytes).map_err(|_| Bad::NotBackup)?;
    let peek = crate::settings_store::peek_settings_text(text.trim_start_matches('\u{feff}'), true).map_err(|_| Bad::NotBackup)?;
    let entry = FileEntry { path: "data/config/settings.json".to_string(), size: bytes.len() as u64, sha256: hex(&Sha256::digest(&bytes)) };
    Ok(Inspected {
        plain: true,
        manifest: None,
        files: vec![Wanted { rel: "config/settings.json".to_string(), item: "settings", entry }],
        counts: BTreeMap::from([("settings", None)]),
        settings: Some(peek),
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
    let (max_files, max_bytes) = if manifest.include_calls { (CALLS_MAX_FILES, CALLS_MAX_BYTES) } else { (SETTINGS_MAX_FILES, SETTINGS_MAX_BYTES) };
    if manifest.files.len() > max_files {
        return Err(Bad::TooLarge);
    }
    let mut total = 0u64;
    let mut seen = HashSet::new();
    for f in &manifest.files {
        let rel = safe_rel(&f.path).ok_or(Bad::Damaged)?;
        if !seen.insert(rel) || f.sha256.len() != 64 || !f.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Bad::Damaged);
        }
        total = total.checked_add(f.size).ok_or(Bad::TooLarge)?;
        if total > max_bytes {
            return Err(Bad::TooLarge);
        }
    }
    let mut files = Vec::new();
    let mut counts: BTreeMap<&'static str, Option<usize>> = BTreeMap::new();
    let mut settings = None;
    let mut tasks = 0usize;
    for f in &manifest.files {
        let rel = safe_rel(&f.path).ok_or(Bad::Damaged)?.to_string();
        let item = item_of(&rel);
        if counted(&rel) {
            if f.size > COUNT_FILE_MAX_BYTES {
                return Err(Bad::TooLarge);
            }
            let mut buf = Vec::with_capacity(f.size as usize);
            zip.copy_checked(&f.path, f.size, &f.sha256, &mut buf)?;
            if rel == "config/settings.json" {
                let text = std::str::from_utf8(&buf).map_err(|_| Bad::Damaged)?;
                settings = Some(crate::settings_store::peek_settings_text(text, false).map_err(|_| Bad::Damaged)?);
            } else if let Some(item) = item {
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
        let Some(item) = item else {
            continue;
        };
        if item == "tasks" && rel != "tasks/index.json" {
            tasks += 1;
        }
        counts.entry(item).or_insert(None);
        files.push(Wanted { rel, item, entry: f.clone() });
    }
    if tasks > 0 {
        counts.insert("tasks", Some(tasks));
    }
    Ok(Inspected { plain: false, manifest: Some(manifest), files, counts, settings })
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

/// The backup folder of a backup's settings can be used here as it is (it is
/// not made for a preview).
fn dir_usable_now(d: &str) -> bool {
    let d = d.trim();
    d.is_empty() || (Path::new(d).is_absolute() && Path::new(d).is_dir() && probe_write(Path::new(d)))
}

fn preview_warnings(ins: &Inspected) -> Vec<&'static str> {
    let mut w = Vec::new();
    if ins.manifest.as_ref().is_some_and(|m| newer(&m.app_version, APP_VERSION)) {
        w.push("newer_version");
    }
    if ins.plain {
        w.push("plain_settings_file");
    }
    if ins.settings.as_ref().is_some_and(|(_, d)| !dir_usable_now(d)) {
        w.push("backup_dir_reset");
    }
    w
}

fn preview_json(token: &str, source_name: &str, ins: &Inspected, current_password: &str, warnings: &[&'static str]) -> Value {
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
        "changes_password": ins.settings.as_ref().is_some_and(|(pw, _)| pw != current_password.trim()),
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
    let ok = std::fs::write(&probe, b"ok").is_ok();
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

/// `PUT /api/settings` with a `backup_dir` backups cannot be saved in: the
/// 400 answer (`backup_dir_unwritable`). The folder is made when it can be.
pub fn check_settings_patch(patch: &Value) -> Option<Response> {
    let d = patch.get("backup_dir")?.as_str()?.trim();
    if dir_setting_ok(d) {
        return None;
    }
    Some(api_error(StatusCode::BAD_REQUEST, "backup_dir_unwritable", message("backup_dir_unwritable")))
}

fn create_private(path: &Path) -> io::Result<File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
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
    std::fs::copy(from, &tmp)?;
    if let Err(e) = std::fs::rename(&tmp, to) {
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
    pub include_calls: bool,
}

fn describe(path: &Path, name: String, kind: Kind, at: NaiveDateTime) -> Entry {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let manifest: Option<Value> = File::open(path)
        .ok()
        .and_then(|f| ZipIn::open(f).ok())
        .and_then(|mut z| z.read_entry(MANIFEST_NAME, MANIFEST_MAX_BYTES).ok().flatten())
        .and_then(|b| serde_json::from_slice(&b).ok());
    let field = |k: &str| manifest.as_ref().and_then(|m| m.get(k)).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    Entry {
        at: field("created_at").unwrap_or_else(|| local_rfc3339(at)),
        app_version: field("app_version"),
        include_calls: manifest.as_ref().and_then(|m| m.get("include_calls")).and_then(Value::as_bool).unwrap_or(false),
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

/// A backup file in `dir`: written next to its name, then renamed (mode 600 on Unix).
fn write_backup_file(lay: &Layout, settings_json: Vec<u8>, dir: &Path, kind: Kind, with_calls: bool, now: DateTime<Local>) -> Result<Entry, RunErr> {
    std::fs::create_dir_all(dir).map_err(|e| run_err(&e, true))?;
    let mut at = now;
    let mut name = file_name(kind, at.naive_local());
    for _ in 0..60 {
        if !dir.join(&name).exists() {
            break;
        }
        at += chrono::Duration::seconds(1);
        name = file_name(kind, at.naive_local());
    }
    if dir.join(&name).exists() {
        return Err(RunErr::Io);
    }
    let tmp = dir.join(format!(".{name}.tmp"));
    let file = create_private(&tmp).map_err(|e| run_err(&e, true))?;
    let written = (move || -> io::Result<()> {
        let sources = collect(lay, settings_json, with_calls);
        let out = write_zip(BufWriter::new(file), sources, kind, with_calls, at)?;
        let file = out.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()
    })();
    let done = written.and_then(|_| std::fs::rename(&tmp, dir.join(&name)));
    if let Err(e) = done {
        let _ = std::fs::remove_file(&tmp);
        return Err(run_err(&e, false));
    }
    let size = std::fs::metadata(dir.join(&name)).map(|m| m.len()).unwrap_or(0);
    Ok(Entry {
        name,
        at: at.to_rfc3339_opts(SecondsFormat::Secs, false),
        size,
        kind: kind.as_str(),
        app_version: Some(APP_VERSION.to_string()),
        include_calls: with_calls,
    })
}

async fn create_in(state: &CallsState, lay: &Layout, dir: &Path, kind: Kind, with_calls: bool) -> Result<Entry, RunErr> {
    let settings_json = state.settings_store.overrides_json();
    let (lay, dir, now) = (lay.clone(), dir.to_path_buf(), Local::now());
    tokio::task::spawn_blocking(move || write_backup_file(&lay, settings_json, &dir, kind, with_calls, now))
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
    let res = create_in(state, &lay, &dir, kind, rt.backup_include_calls).await;
    let at = Local::now().to_rfc3339_opts(SecondsFormat::Secs, false);
    ds.inner().last = Some(match &res {
        Ok(e) => LastRun { at, ok: true, name: Some(e.name.clone()), size: e.size, error: None },
        Err(e) => LastRun { at, ok: false, name: None, size: 0, error: Some(e.code()) },
    });
    match &res {
        Ok(e) => {
            info!("backup made kind={} size={} with_calls={}", kind.as_str(), e.size, e.include_calls);
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

/// The settings-only zip "Export settings" downloads: its name and bytes.
pub(crate) async fn export_bytes(state: &CallsState) -> Result<(String, Vec<u8>), RunErr> {
    let lay = Layout::of(state);
    let settings_json = state.settings_store.overrides_json();
    let now = Local::now();
    tokio::task::spawn_blocking(move || -> Result<(String, Vec<u8>), RunErr> {
        let sources = collect(&lay, settings_json, false);
        let out = write_zip(io::Cursor::new(Vec::new()), sources, Kind::Export, false, now).map_err(|e| run_err(&e, false))?;
        Ok((file_name(Kind::Export, now.naive_local()), out.into_inner()))
    })
    .await
    .unwrap_or(Err(RunErr::Io))
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
            warn!("backup: file error: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "io")
        }
    }
}

/// A preview waiting for "Restore".
struct Pending {
    file: PathBuf,
    /// An upload (deleted when done); a file of the backup folder stays.
    owned: bool,
    made: Instant,
    ins: Inspected,
    warnings: Vec<&'static str>,
}

static PENDING: LazyLock<Mutex<HashMap<String, Pending>>> = LazyLock::new(Default::default);

fn pending() -> MutexGuard<'static, HashMap<String, Pending>> {
    PENDING.lock().unwrap_or_else(|e| e.into_inner())
}

fn drop_pending_file(p: &Pending) {
    if p.owned {
        let _ = std::fs::remove_file(&p.file);
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

fn register(state: &CallsState, file: PathBuf, owned: bool, ins: Inspected, source_name: &str) -> Value {
    let token = uuid::Uuid::new_v4().simple().to_string();
    let warnings = preview_warnings(&ins);
    let password = state.settings_store.snapshot().ui_password.clone();
    let v = preview_json(&token, source_name, &ins, &password, &warnings);
    pending().insert(token, Pending { file, owned, made: Instant::now(), ins, warnings });
    v
}

/// The preview of an uploaded file (kept for 15 minutes). Nothing changes yet.
pub(crate) async fn preview_upload(state: &CallsState, source_name: &str, body: Bytes) -> Result<Value, ApiErr> {
    sweep_pending();
    if body.len() > IMPORT_MAX_BYTES {
        return Err((StatusCode::BAD_REQUEST, "too_large"));
    }
    if body.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "not_backup"));
    }
    let stage = staging_dir(&PathBuf::from(&state.settings.calls_data_dir));
    let file = stage.join(format!("upload-{}.bin", uuid::Uuid::new_v4().simple()));
    let path = file.clone();
    let read = tokio::task::spawn_blocking(move || -> Result<Inspected, Bad> {
        std::fs::create_dir_all(&stage)?;
        let mut f = create_private(&path)?;
        f.write_all(&body)?;
        f.sync_all()?;
        drop(f);
        inspect(&path)
    })
    .await
    .unwrap_or_else(|_| Err(Bad::Io(io::Error::other("preview task"))));
    match read {
        Ok(ins) => Ok(register(state, file, true, ins, source_name)),
        Err(e) => {
            let _ = std::fs::remove_file(&file);
            Err(bad_err(e))
        }
    }
}

/// The preview of a file in the backup folder.
pub(crate) async fn preview_saved(state: &CallsState, name: &str) -> Result<Value, ApiErr> {
    sweep_pending();
    let rt = state.settings_store.snapshot();
    let path = configured_dir(&rt, &PathBuf::from(&state.settings.calls_data_dir)).join(name);
    if parse_name(name).is_none() || !is_plain_file(&path) {
        return Err((StatusCode::NOT_FOUND, "not_found"));
    }
    let p = path.clone();
    let read = tokio::task::spawn_blocking(move || inspect(&p)).await.unwrap_or_else(|_| Err(Bad::Io(io::Error::other("preview task"))));
    match read {
        Ok(ins) => Ok(register(state, path, false, ins, name)),
        Err(e) => Err(bad_err(e)),
    }
}

/// Restore what the preview `token` showed: refused during a call; a
/// `before_import` backup first; the call records, texts and to-do items only
/// with `with_calls`, and only added.
pub(crate) async fn apply(state: &CallsState, token: &str, with_calls: bool) -> Result<Value, ApiErr> {
    sweep_pending();
    let valid = pending().get(token).is_some_and(|p| p.made.elapsed() < TOKEN_TTL);
    if !valid {
        return Err((StatusCode::BAD_REQUEST, "expired"));
    }
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
    let out = apply_pending(state, &p, with_calls).await;
    drop_pending_file(&p);
    out
}

/// A copy of the settings as they are now, in the backup folder (in the
/// default one when that cannot be written): the entry and its folder.
async fn make_before_import(state: &CallsState, lay: &Layout) -> Result<(Entry, PathBuf), RunErr> {
    let dir = configured_dir(&state.settings_store.snapshot(), &lay.data);
    match create_in(state, lay, &dir, Kind::BeforeImport, false).await {
        Ok(e) => Ok((e, dir)),
        Err(first) => {
            let fallback = default_dir(&lay.data);
            if fallback == dir {
                return Err(first);
            }
            create_in(state, lay, &fallback, Kind::BeforeImport, false).await.map(|e| (e, fallback))
        }
    }
}

async fn apply_pending(state: &CallsState, p: &Pending, with_calls: bool) -> Result<Value, ApiErr> {
    let lay = Layout::of(state);
    let (before, before_dir) = make_before_import(state, &lay).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.code()))?;
    let before_path = before_dir.join(&before.name);
    let want: Vec<Wanted> = p.ins.files.iter().filter(|w| with_calls || !CALL_ITEMS.contains(&w.item)).cloned().collect();
    let stage = staging_dir(&lay.data).join(format!("apply-{}", uuid::Uuid::new_v4().simple()));
    let unpacked = {
        let (src, plain, stage, want) = (p.file.clone(), p.ins.plain, stage.clone(), want.clone());
        tokio::task::spawn_blocking(move || extract(&src, plain, &want, &stage))
            .await
            .unwrap_or_else(|_| Err(Bad::Io(io::Error::other("restore task"))))
    };
    if let Err(e) = unpacked {
        let _ = std::fs::remove_dir_all(&stage);
        return Err(bad_err(e));
    }
    let mut created = Vec::new();
    let installed = match install(state, &lay, &stage, &want, &mut created).await {
        Ok(i) => i,
        Err(e) => {
            warn!("backup: restoring failed, the settings from before are put back: {e:#}");
            roll_back(state, &lay, &before_path, &created).await;
            let _ = std::fs::remove_dir_all(&stage);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "io"));
        }
    };
    if with_calls {
        match merge_records(state, &lay, &stage, &want).await {
            Ok((calls, sms, tasks)) => info!("backup records added calls={calls} sms={sms} tasks={tasks}"),
            Err(e) => {
                warn!("backup: adding the call records failed, the settings from before are put back: {e:#}");
                roll_back(state, &lay, &before_path, &created).await;
                let _ = std::fs::remove_dir_all(&stage);
                return Err((StatusCode::INTERNAL_SERVER_ERROR, "io"));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&stage);
    prune(&before_dir, Kind::BeforeImport, BEFORE_IMPORT_KEEP);
    let mut warnings: Vec<&'static str> = p.warnings.iter().copied().filter(|w| *w != "backup_dir_reset").collect();
    if installed.dir_reset {
        warnings.push("backup_dir_reset");
    }
    let items: Vec<&str> = want.iter().map(|w| w.item).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    info!("backup restored items={items:?} with_calls={with_calls} restart_needed={}", installed.restart_needed);
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
        std::fs::copy(src, &to)?;
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
    dir_reset: bool,
    restart_needed: bool,
}

/// The settings-sized parts in place (files that were not here go into
/// `created`), then read again by their stores.
async fn install(state: &CallsState, lay: &Layout, stage: &Path, want: &[Wanted], created: &mut Vec<PathBuf>) -> anyhow::Result<Installed> {
    let mut items: HashSet<&'static str> = HashSet::new();
    for w in want.iter().filter(|w| !CALL_ITEMS.contains(&w.item)) {
        items.insert(w.item);
        if w.rel == "config/settings.json" {
            continue;
        }
        let target = lay.target(&w.rel).context("no place for a file of the backup")?;
        if !target.exists() {
            created.push(target.clone());
        }
        move_into(&stage_path(stage, &w.rel), &target).with_context(|| format!("put back {}", w.rel))?;
    }
    let mut dir_reset = false;
    let staged_settings = stage_path(stage, "config/settings.json");
    if items.contains("settings") && staged_settings.is_file() {
        let text = std::fs::read_to_string(&staged_settings).context("read the settings of the backup")?;
        let days_before = state.settings_store.snapshot().retention_days;
        let (rt, reset) = state.settings_store.replace_from_text(text.trim_start_matches('\u{feff}'), &dir_setting_ok)?;
        dir_reset = reset;
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
    if items.contains("settings") {
        // Blocked numbers kept in an older settings file join the Blacklist.
        crate::contacts::absorb_reject_numbers(&state.settings_store, &state.contacts).await;
    }
    Ok(Installed { dir_reset, restart_needed })
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
        crate::phone_wifi::pairing().reload();
    }
    if items.contains("errands") {
        state.errands.reload();
    }
    Ok(restart)
}

/// Restoring failed half-way: the files it added go, and the `before_import`
/// backup is put back.
async fn roll_back(state: &CallsState, lay: &Layout, before: &Path, created: &[PathBuf]) {
    for p in created {
        let _ = std::fs::remove_file(p);
    }
    let src = before.to_path_buf();
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
            if let Err(e) = install(state, lay, &stage, &ins.files, &mut again).await {
                warn!("backup: the settings from before could not all be put back: {e:#}");
            }
            let all: HashSet<&'static str> = STORE_ITEMS.into_iter().collect();
            if let Err(e) = reload_stores(state, &all).await {
                warn!("backup: reading the restored files again failed: {e:#}");
            }
        }
        Err(e) => warn!("backup: the copy made before restoring cannot be read: {}", bad_err(e).1),
    }
    let _ = std::fs::remove_dir_all(&stage);
}

/// Call records with their recordings, texts and to-do items of the backup
/// that are not here yet are added; how many of each.
async fn merge_records(state: &CallsState, lay: &Layout, stage: &Path, want: &[Wanted]) -> anyhow::Result<(usize, usize, usize)> {
    let (mut calls, mut sms, mut tasks) = (0, 0, 0);
    // Recordings first, so the call list never points at a file that is not there.
    for w in want.iter().filter(|w| w.item == "calls" && w.rel != "index.json") {
        let target = lay.target(&w.rel).context("no place for a recording")?;
        if !target.exists() {
            move_into(&stage_path(stage, &w.rel), &target).with_context(|| format!("add {}", w.rel))?;
        }
    }
    let index = stage_path(stage, "index.json");
    if want.iter().any(|w| w.rel == "index.json") && index.is_file() {
        let records = crate::calls::records_from_index_json(&std::fs::read(&index)?).context("call list of the backup")?;
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
        list.push(serde_json::from_slice::<crate::tasks::TaskRecord>(&bytes).with_context(|| format!("to-do item {}", w.rel))?);
    }
    if !list.is_empty() {
        tasks = state.task_store.merge(list).await?;
    }
    Ok((calls, sms, tasks))
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
        "busy" => "a backup is already running",
        "dir_unwritable" => "backups cannot be saved in the backup folder",
        "backup_dir_unwritable" => "backups cannot be saved in this folder",
        "disk_full" => "the disk is full",
        "not_found" => "no such backup file",
        "not_local" => "only on the computer Bageo runs on",
        "bad_request" => "bad request",
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

/// The request comes from this computer itself (not through a proxy on it).
fn from_this_computer(ext: &Extensions, headers: &HeaderMap) -> bool {
    let Some(ConnectInfo(peer)) = ext.get::<ConnectInfo<std::net::SocketAddr>>() else {
        return false;
    };
    let local = match peer.ip() {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback()),
    };
    local && !came_through_proxy(headers)
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

/// `GET /api/backup/export`: the settings-only zip, as a download.
async fn export(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    match export_bytes(&state).await {
        Ok((name, bytes)) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/zip")
            .header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\""))
            .header(header::CACHE_CONTROL, "no-store")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Err(e) => err_response((StatusCode::INTERNAL_SERVER_ERROR, e.code())),
    }
}

/// `GET /api/backup/file/{name}`: a backup of the backup folder, as a download.
async fn download(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>, UrlPath(name): UrlPath<String>) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    let path = configured_dir(&state.settings_store.snapshot(), &PathBuf::from(&state.settings.calls_data_dir)).join(&name);
    if parse_name(&name).is_none() || !is_plain_file(&path) {
        return err_response((StatusCode::NOT_FOUND, "not_found"));
    }
    let Ok(file) = tokio::fs::File::open(&path).await else {
        return err_response((StatusCode::NOT_FOUND, "not_found"));
    };
    let len = file.metadata().await.map(|m| m.len()).ok();
    let stream = futures_util::stream::unfold(Some(file), |state| async move {
        let mut f = state?;
        let mut buf = vec![0u8; 64 * 1024];
        match tokio::io::AsyncReadExt::read(&mut f, &mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<Bytes, io::Error>(Bytes::from(buf)), Some(f)))
            }
            Err(e) => Some((Err(e), None)),
        }
    });
    let mut res = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/zip")
        .header(header::CONTENT_DISPOSITION, format!("attachment; filename=\"{name}\""))
        .header(header::CACHE_CONTROL, "no-store");
    if let Some(len) = len {
        res = res.header(header::CONTENT_LENGTH, len);
    }
    res.body(Body::from_stream(stream)).unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// `POST /api/backup/import`, the file as the body and its name in
/// `X-Filename`: the preview, nothing restored yet.
async fn import(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>, body: Result<Bytes, BytesRejection>) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    let Some(name) = upload_name(&headers) else {
        return api_error(StatusCode::BAD_REQUEST, "bad_request", "send the file name (URL-encoded) in X-Filename");
    };
    let body = match body {
        Ok(b) => b,
        Err(rej) if rej.status() == StatusCode::PAYLOAD_TOO_LARGE => return err_response((StatusCode::BAD_REQUEST, "too_large")),
        Err(_) => return err_response((StatusCode::BAD_REQUEST, "bad_request")),
    };
    match preview_upload(&state, &name, body).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err_response(e),
    }
}

#[derive(Deserialize)]
struct RestoreBody {
    name: String,
}

/// `POST /api/backup/restore` `{name}`: the preview of a file in the backup folder.
async fn restore(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>, body: Bytes) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    if !json_request(&headers) {
        return json_required();
    }
    let Ok(b) = serde_json::from_slice::<RestoreBody>(&body) else {
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
    with_calls: bool,
}

/// `POST /api/backup/apply` `{token, with_calls}`: restore what the preview showed.
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
    match apply(&state, b.token.trim(), b.with_calls).await {
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
            warn!("backup: the folder could not be opened: {e}");
            err_response((StatusCode::INTERNAL_SERVER_ERROR, "io"))
        }
    }
}

/// The backup API, merged into the console's router (`/calls`).
pub fn router() -> Router<CallsState> {
    Router::new()
        .route("/api/backup", get(status))
        .route("/api/backup/run", post(run))
        .route("/api/backup/export", get(export))
        .route("/api/backup/file/{name}", get(download))
        .route("/api/backup/import", post(import).layer(DefaultBodyLimit::max(IMPORT_MAX_BYTES + (1 << 20))))
        .route("/api/backup/restore", post(restore))
        .route("/api/backup/apply", post(apply_route))
        .route("/api/backup/open-folder", post(open_folder))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calls_http::PhoneDevices;

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
               "kind": "manual", "include_calls": false, "files": files})
    }

    fn call_json(id: &str, day: &str) -> Value {
        json!({"id": id, "started_at": format!("{day}T03:04:05Z"), "ended_at": format!("{day}T03:05:05Z"), "serial": "pixel-test-1",
               "caller_id": "+15555550100", "duration_ms": 60000, "status": "ended", "audio_path": format!("{day}/{id}.wav"),
               "tts_audio_path": null, "uplink_bytes": 0, "tts_bytes": 0, "sample_rate": 16000, "events": []})
    }

    fn code(r: Result<Value, ApiErr>) -> &'static str {
        r.expect_err("refused").1
    }

    #[tokio::test]
    async fn export_then_import_brings_back_settings_and_contacts() {
        let (ra, rb) = (temp_root("a"), temp_root("b"));
        let a = state_at(&ra);
        a.settings_store.apply_patch(&json!({"owner_name": "Sam Example", "backup_keep": 7, "public_url": "http://198.51.100.20:8787"})).unwrap();
        let draft = crate::contacts::Draft { name: "Alex Example".into(), phones: vec!["+1 555 555 0123".into()], ..Default::default() };
        a.contacts.add(&draft).await.unwrap();
        let (name, bytes) = export_bytes(&a).await.unwrap();
        assert_eq!(parse_name(&name).map(|(k, _)| k), Some(Kind::Export));

        let b = state_at(&rb);
        let preview = preview_upload(&b, &name, Bytes::from(bytes)).await.unwrap();
        assert_eq!(preview["kind"], "export");
        assert_eq!(preview["has_calls"], false);
        assert_eq!(preview["changes_password"], false);
        let items = preview["items"].as_array().unwrap();
        assert!(items.contains(&json!({"id": "settings"})));
        assert!(items.contains(&json!({"id": "contacts", "count": 1})));
        assert!(preview["warnings"].as_array().unwrap().is_empty());

        let token = preview["token"].as_str().unwrap().to_string();
        let done = apply(&b, &token, false).await.unwrap();
        assert_eq!(done["ok"], true);
        let rt = b.settings_store.snapshot();
        assert_eq!((rt.owner_name.as_str(), rt.backup_keep), ("Sam Example", 7));
        assert_eq!(rt.public_url, "", "the address of this computer stays");
        let contacts = b.contacts.list("");
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].name, "Alex Example");
        // Saved: a restart reads the same.
        let saved = std::fs::read_to_string(rb.join("config").join("settings.json")).unwrap();
        assert!(saved.contains("Sam Example"));

        // A before_import copy came first, of the settings before.
        let before = done["before_import"].as_str().unwrap();
        assert_eq!(parse_name(before).map(|(k, _)| k), Some(Kind::BeforeImport));
        let kept = inspect(&default_dir(&rb).join(before)).unwrap();
        assert!(!kept.manifest.unwrap().include_calls);
        // The token works once.
        assert_eq!(code(apply(&b, &token, false).await), "expired");
        let _ = std::fs::remove_dir_all(&ra);
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
        let done = apply(&b, preview["token"].as_str().unwrap(), false).await.unwrap();
        assert_eq!(done["warnings"], json!(["plain_settings_file"]));
        assert_eq!(b.settings_store.snapshot().owner_name, "Plain Example");

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

        for path in ["data/../escape.json", "/etc/passwd", "data//etc/passwd", "data/C:/x.json", "data\\config\\settings.json", "data/config/../../x.json", "config/settings.json"] {
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
        m["files"][0]["size"] = json!(SETTINGS_MAX_BYTES + 1);
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
        for bad in ["bageo-backup-20261119-080509-auto.zip/../x", "../bageo-settings-20261119-080509.zip", "bageo-backup-20261119-080509.zip", "bageo-settings-2026111-0805099.zip"] {
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
        assert_eq!(apply(&b, &token, false).await.expect_err("refused"), (StatusCode::CONFLICT, "call_active"));
        assert!(pending().contains_key(&token), "the preview can be used after the call");
        assert_ne!(b.settings_store.snapshot().owner_name, "Call Example");
        assert!(names_in(&default_dir(&rb)).is_empty(), "no before_import copy either");
        let _ = std::fs::remove_dir_all(&rb);
    }

    #[tokio::test]
    async fn call_records_are_added_and_none_here_go() {
        let (ra, rb) = (temp_root("ca"), temp_root("cb"));
        std::fs::write(ra.join("index.json"), json!({"calls": [call_json("call-a", "2026-02-03")]}).to_string()).unwrap();
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
        a.settings_store.apply_patch(&json!({"backup_include_calls": true})).unwrap();

        let entry = run_backup(&a, Kind::Manual).await.unwrap();
        assert!(entry.include_calls);
        let path = default_dir(&ra).join(&entry.name);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let held = inspect(&path).unwrap();
        assert!(held.files.iter().all(|w| !w.rel.ends_with(".pcm")), "a call still being recorded stays out");
        assert!(held.files.iter().all(|w| !w.rel.starts_with("backups/")));

        let b = state_at(&rb);
        let preview = preview_upload(&b, &entry.name, Bytes::from(std::fs::read(&path).unwrap())).await.unwrap();
        assert_eq!((preview["has_calls"].as_bool(), preview["calls_count"].as_u64()), (Some(true), Some(1)));
        let done = apply(&b, preview["token"].as_str().unwrap(), true).await.unwrap();
        assert_eq!(done["ok"], true);
        let ids: Vec<String> = b.store.list().await.into_iter().map(|c| c.id).collect();
        assert!(ids.contains(&"call-a".to_string()) && ids.contains(&"call-b".to_string()), "{ids:?}");
        assert_eq!(std::fs::read(rb.join("2026-02-03").join("call-a.wav")).unwrap(), b"RIFF test recording");
        assert!(b.sms.get(text_id).is_some());
        assert!(b.task_store.get(&task.id).await.is_some());
        let _ = std::fs::remove_dir_all(&ra);
        let _ = std::fs::remove_dir_all(&rb);
    }

    #[tokio::test]
    async fn a_backup_folder_that_cannot_be_written_is_refused() {
        let root = temp_root("dir");
        let blocker = root.join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        for bad in [blocker.join("backups").to_string_lossy().into_owned(), "relative/folder".to_string()] {
            let r = check_settings_patch(&json!({"backup_dir": bad})).expect("refused");
            assert_eq!(r.status(), StatusCode::BAD_REQUEST);
            let body = axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap();
            let v: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["code"], "backup_dir_unwritable");
        }
        let fine = root.join("fine");
        assert!(check_settings_patch(&json!({"backup_dir": fine.to_string_lossy()})).is_none());
        assert!(fine.is_dir(), "made when saved");
        assert!(check_settings_patch(&json!({"backup_dir": ""})).is_none());
        assert!(check_settings_patch(&json!({"owner_name": "x"})).is_none());
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
}
