// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! "Check for updates" of the local data pack: the console's button downloads
//! a newer bundle from the public bageo-packs releases on GitHub, checks it and
//! installs it for the next call ([packs_store]). Bageo never checks on its
//! own; nothing is sent but the two downloads.
//!
//! The rules, each with its test below:
//! - Only the release's own files: the index at [RELEASES]/latest/download/
//!   and the bundle it names (a plain file name) in the same release. The
//!   address is fixed in the program; nothing in the settings or environment
//!   changes it.
//! - Redirects are followed by hand, at most [MAX_HOPS]: every address must be
//!   https on the default port, without a user name, on one of [HOSTS]; on
//!   github.com only under this repository's releases ([hop_allowed]).
//! - The index may be [MAX_INDEX_BYTES], the bundle what the index says and at
//!   most [MAX_PACK_BYTES], counted as it comes in; 10 s to connect, 60 s for
//!   the whole check. The bundle's size and SHA-256 must match the index.
//! - A format newer than this Bageo reads, or a newer min_bageo: "update Bageo
//!   first". A version not newer than the one in use: nothing is downloaded.
//! - The bundle is written to the packs folder's temporary file, checked again
//!   in full ([packs_store::install_file]) and renamed into place in one step.
//!   A failure, a cut connection or a stop leaves no file behind; start-up
//!   clears one left by a crash.
//! - One check at a time, in a task of its own; a call already going keeps its
//!   table, the next call uses the new one.
//! - The same check fetches the translations (bageo-locales.json.gz) the
//!   index's locales entry names, by the same rules: same release, same
//!   hosts and redirects, its own size (at most 48 MiB, and unpacked while
//!   counted, [locales_store]), SHA-256, format, version and min_bageo, the
//!   same temporary file, check and rename. A version not newer than the one
//!   in use is not downloaded. Either file failing costs only that file.
//! - "Restore the built-in version" puts both back; it fails when either
//!   installed file cannot be deleted.
//!
//! The voice packs ([crate::voice_packs]) come from the same releases through
//! the same pieces: [Source], [open], [fetch_index], [download_checked] and
//! [Staged].

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bageo_packs::locales::{check_locales_ref, LOCALES_FORMAT};
use bageo_packs::{parse_index, Issue, LocalesRef, FORMAT, MAX_BUNDLE_BYTES};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use url::{Host, Url};

use crate::calls_http::{api_error, token_ok_parts, unauthorized, CallsState, TokenQuery};
use crate::{locales_store, packs_store};

/// The public data repository on github.com.
const REPO_PATH: &str = "/h4lo-void/bageo-packs";
/// Its releases; the index is in the latest one.
const RELEASES: &str = "https://github.com/h4lo-void/bageo-packs/releases";
const INDEX_NAME: &str = "bageo-pack-index.json";
/// The only hosts a download may go through: GitHub and its release downloads.
const HOSTS: [&str; 3] = ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"];
/// Redirects followed for one file.
const MAX_HOPS: usize = 5;
const MAX_INDEX_BYTES: u64 = 16 * 1024;
const MAX_PACK_BYTES: u64 = MAX_BUNDLE_BYTES as u64;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(60);
const BAGEO_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How a check ended; [Outcome::code] is what the console shows (pk_r_<code>).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    UpToDate,
    /// Installed; the next call uses it.
    Updated(u64),
    /// No connection, a time-out, or GitHub did not answer: try again later.
    Offline,
    /// The download was refused (address, size, checksum or content).
    BadPack,
    /// The pack needs a newer Bageo.
    UpdateBageo,
    /// Updates installed in this run reached [packs_store::MAX_INSTALLS].
    Restart,
    /// The pack could not be saved on this computer.
    Io,
    /// Another check or a restore is running.
    Busy,
}

impl Outcome {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Outcome::UpToDate => "up_to_date",
            Outcome::Updated(_) => "updated",
            Outcome::Offline => "offline",
            Outcome::BadPack => "bad_pack",
            Outcome::UpdateBageo => "update_bageo",
            Outcome::Restart => "restart",
            Outcome::Io => "io",
            Outcome::Busy => "busy",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Fail {
    UpToDate,
    Offline(String),
    Bad(String),
    UpdateBageo(String),
    Io(String),
    /// Installs of this file reached their cap in this run.
    Restart,
}

impl Fail {
    fn outcome(&self) -> Outcome {
        match self {
            Fail::UpToDate => Outcome::UpToDate,
            Fail::Offline(_) => Outcome::Offline,
            Fail::Bad(_) => Outcome::BadPack,
            Fail::UpdateBageo(_) => Outcome::UpdateBageo,
            Fail::Io(_) => Outcome::Io,
            Fail::Restart => Outcome::Restart,
        }
    }

    pub(crate) fn detail(&self) -> &str {
        match self {
            Fail::UpToDate => "up to date",
            Fail::Restart => "restart Bageo first",
            Fail::Offline(s) | Fail::Bad(s) | Fail::UpdateBageo(s) | Fail::Io(s) => s,
        }
    }
}

pub(crate) fn bad(s: impl Into<String>) -> Fail {
    Fail::Bad(s.into())
}

pub(crate) fn offline(e: reqwest::Error) -> Fail {
    Fail::Offline(e.to_string())
}

fn first_issue(issues: &[Issue]) -> String {
    issues.first().map(|i| i.to_string()).unwrap_or_default()
}

/// An address a download may go to: https on the default port, no user name,
/// a host of [HOSTS] exactly as written (the parser has already lower-cased
/// it and turned other scripts into xn--), and on github.com only this
/// repository's releases.
fn hop_allowed(u: &Url) -> bool {
    if u.scheme() != "https" || !u.username().is_empty() || u.password().is_some() || u.port().is_some() {
        return false;
    }
    let Some(Host::Domain(host)) = u.host() else { return false };
    if !HOSTS.contains(&host) {
        return false;
    }
    host != "github.com" || u.path().starts_with(&format!("{REPO_PATH}/releases/"))
}

/// Where the index and the bundle come from.
pub(crate) struct Source {
    releases: String,
    /// Tests: the local server that plays GitHub (plain http on 127.0.0.1).
    #[cfg(test)]
    test_port: Option<u16>,
}

impl Source {
    pub(crate) fn official() -> Self {
        Source {
            releases: RELEASES.to_string(),
            #[cfg(test)]
            test_port: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn local(port: u16) -> Self {
        Source { releases: format!("http://127.0.0.1:{port}{REPO_PATH}/releases"), test_port: Some(port) }
    }

    fn index_url(&self) -> String {
        self.index_url_named(INDEX_NAME)
    }

    /// The index file `name` in the latest release.
    pub(crate) fn index_url_named(&self, name: &str) -> String {
        format!("{}/latest/download/{name}", self.releases)
    }

    fn may_visit(&self, u: &Url) -> bool {
        #[cfg(test)]
        {
            if let Some(port) = self.test_port {
                if u.scheme() == "http" && u.host_str() == Some("127.0.0.1") && u.port() == Some(port) && u.username().is_empty() && u.password().is_none() {
                    return true;
                }
            }
        }
        hop_allowed(u)
    }

    /// The bundle's address: in the release the index came from (the tag the
    /// latest/ link led to), else the latest one.
    fn pack_url(&self, hops: &[Url], file: &str) -> Option<Url> {
        self.release_file_url(hops, INDEX_NAME, file)
    }

    /// The address of `file` in the release the index `index_name` came from
    /// (the tag its latest/ link led to), else in the latest one. None when
    /// `file` is not a plain file name.
    pub(crate) fn release_file_url(&self, hops: &[Url], index_name: &str, file: &str) -> Option<Url> {
        if !plain_file_name(file) {
            return None;
        }
        let prefix = format!("{}/download/", self.releases);
        let suffix = format!("/{index_name}");
        let base = hops
            .iter()
            .rev()
            .find_map(|u| {
                if u.query().is_some() || u.fragment().is_some() {
                    return None;
                }
                let tag = u.as_str().strip_prefix(&prefix)?.strip_suffix(&suffix)?;
                (!tag.is_empty() && !tag.contains('/')).then(|| format!("{prefix}{tag}/"))
            })
            .unwrap_or_else(|| format!("{}/latest/download/", self.releases));
        Url::parse(&format!("{base}{file}")).ok()
    }
}

/// A file name in the release, never a path or an address.
fn plain_file_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && !s.starts_with('.') && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The x.y.z of a version; what follows a '-' or '+' (-beta, -rc.1, +build)
/// is left out.
fn version_parts(s: &str) -> Option<(u32, u32, u32)> {
    let core = s.trim().split(['-', '+']).next().unwrap_or("");
    let mut it = core.split('.').map(|p| p.parse::<u32>().ok());
    let v = (it.next()??, it.next()??, it.next()??);
    it.next().is_none().then_some(v)
}

/// Version have is at least need, by their x.y.z numbers.
fn version_at_least(have: &str, need: &str) -> bool {
    matches!((version_parts(have), version_parts(need)), (Some(have), Some(need)) if have >= need)
}

/// This Bageo is at least min (x.y.z).
pub(crate) fn bageo_at_least(min: &str) -> bool {
    version_at_least(BAGEO_VERSION, min)
}

fn client() -> reqwest::Result<reqwest::Client> {
    client_with(TOTAL_TIMEOUT)
}

/// The HTTP client of a download: redirects by hand, https only (outside
/// tests), `total` for the whole of one request.
pub(crate) fn client_with(total: Duration) -> reqwest::Result<reqwest::Client> {
    let b = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(total)
        .user_agent(concat!("Bageo/", env!("CARGO_PKG_VERSION")))
        .https_only(!cfg!(test));
    #[cfg(test)]
    let b = b.no_proxy();
    b.build()
}

/// Asks for url, following redirects by hand; every address is checked before
/// it is asked. Returns the answer and the addresses asked.
pub(crate) async fn open(client: &reqwest::Client, src: &Source, start: Url) -> Result<(reqwest::Response, Vec<Url>), Fail> {
    let mut url = start;
    let mut hops = Vec::new();
    for followed in 0..=MAX_HOPS {
        if !src.may_visit(&url) {
            return Err(bad(format!("refused the address on {}", url.host_str().unwrap_or("?"))));
        }
        let resp = client.get(url.clone()).send().await.map_err(offline)?;
        hops.push(url.clone());
        let status = resp.status();
        if status.is_redirection() {
            if followed == MAX_HOPS {
                return Err(bad(format!("more than {MAX_HOPS} redirects")));
            }
            let to = resp.headers().get(reqwest::header::LOCATION).and_then(|v| v.to_str().ok()).ok_or_else(|| bad("a redirect without an address"))?;
            url = url.join(to).map_err(|e| bad(format!("a redirect to a bad address: {e}")))?;
            continue;
        }
        if status != reqwest::StatusCode::OK {
            return Err(Fail::Offline(format!("HTTP {status}")));
        }
        return Ok((resp, hops));
    }
    Err(bad(format!("more than {MAX_HOPS} redirects")))
}

/// Reads a body, at most cap bytes (a longer one is cut off and refused).
pub(crate) async fn read_capped(resp: &mut reqwest::Response, cap: u64, mut each: impl FnMut(&[u8]) -> Result<(), Fail>) -> Result<u64, Fail> {
    if resp.content_length().is_some_and(|n| n > cap) {
        return Err(bad(format!("larger than {cap} bytes")));
    }
    let mut n: u64 = 0;
    while let Some(chunk) = resp.chunk().await.map_err(offline)? {
        n += chunk.len() as u64;
        if n > cap {
            return Err(bad(format!("larger than {cap} bytes")));
        }
        each(&chunk)?;
    }
    Ok(n)
}

/// The index file `name` of the latest release, at most `cap` bytes, as text;
/// and the addresses asked on the way (they tell which release it is in).
pub(crate) async fn fetch_index(client: &reqwest::Client, src: &Source, name: &str, cap: u64) -> Result<(String, Vec<Url>), Fail> {
    let url = Url::parse(&src.index_url_named(name)).map_err(|e| bad(e.to_string()))?;
    let (mut resp, hops) = open(client, src, url).await?;
    let mut raw = Vec::new();
    read_capped(&mut resp, cap, |c| {
        raw.extend_from_slice(c);
        Ok(())
    })
    .await?;
    let text = String::from_utf8(raw).map_err(|_| bad("the index is not UTF-8 text"))?;
    Ok((text, hops))
}

/// Downloads `url` into `staged`: it must be exactly `bytes` long with the
/// SHA-256 `sha256` (lowercase hex), counted and hashed as it comes in.
pub(crate) async fn download_checked(client: &reqwest::Client, src: &Source, url: Url, bytes: u64, sha256: &str, staged: &mut Staged) -> Result<(), Fail> {
    let (mut resp, _) = open(client, src, url).await?;
    if resp.content_length().is_some_and(|n| n != bytes) {
        return Err(bad("the size is not the one in the index"));
    }
    let mut hasher = Sha256::new();
    let n = read_capped(&mut resp, bytes, |c| {
        hasher.update(c);
        staged.write(c)
    })
    .await?;
    if n != bytes {
        return Err(bad("the size is not the one in the index"));
    }
    if format!("{:x}", hasher.finalize()) != sha256 {
        return Err(bad("the checksum is not the one in the index"));
    }
    staged.finish()
}

/// The download being written; removed when dropped (a failure, a time-out, a
/// stopped task) unless it was installed (renamed away) first.
pub(crate) struct Staged {
    path: PathBuf,
    file: Option<std::fs::File>,
}

impl Staged {
    pub(crate) fn create(path: PathBuf) -> Result<Self, Fail> {
        // A new file only: never a file or a link that is already there.
        let file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| Fail::Io(format!("cannot write the pack: {e}")))?;
        Ok(Staged { path, file: Some(file) })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), Fail> {
        let file = self.file.as_mut().ok_or_else(|| Fail::Io("the pack file is closed".into()))?;
        file.write_all(bytes).map_err(|e| Fail::Io(format!("cannot write the pack: {e}")))
    }

    pub(crate) fn finish(&mut self) -> Result<(), Fail> {
        if let Some(file) = self.file.take() {
            file.sync_all().map_err(|e| Fail::Io(format!("cannot write the pack: {e}")))?;
        }
        Ok(())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        self.file.take();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// One check: the index, then (when it is newer) the bundle into packs, then
/// apply (checks it in full and installs it; returns its version).
async fn check_with<F>(client: &reqwest::Client, src: &Source, packs: &Path, in_use: u64, apply: F) -> Outcome
where
    F: FnOnce(&Path, u64) -> Result<u64, Vec<Issue>>,
{
    let result = match tokio::time::timeout(TOTAL_TIMEOUT, attempt(client, src, packs, in_use, apply)).await {
        Ok(r) => r,
        Err(_) => Err(Fail::Offline(format!("no answer within {} s", TOTAL_TIMEOUT.as_secs()))),
    };
    report("local data pack", in_use, result)
}

/// Logs how one file's check ended.
fn report(what: &str, in_use: u64, result: Result<u64, Fail>) -> Outcome {
    match result {
        Ok(v) => {
            tracing::info!("{what} updated to {v}; calls from now on use it");
            Outcome::Updated(v)
        }
        Err(f) => {
            let outcome = f.outcome();
            if outcome == Outcome::UpToDate {
                tracing::info!("{what} {in_use} is the latest");
            } else {
                tracing::warn!("{what} update: {} ({})", outcome.code(), f.detail());
            }
            outcome
        }
    }
}

/// Installs a checked download (staged file, the version the index names).
type Apply<'a> = &'a (dyn Fn(&Path, u64) -> Result<u64, Vec<Issue>> + Sync);

/// One check of both files: the index, then the local data pack and the
/// translations, each on its own (None: its installs reached the cap). A
/// failing index fails both; a failing file only itself.
async fn check_both(client: &reqwest::Client, src: &Source, packs: &Path, in_use: u64, apply: Option<Apply<'_>>, loc_in_use: u64, loc_apply: Option<Apply<'_>>) -> (Outcome, Outcome) {
    let work = async {
        let f = match fetch_pack_index(client, src).await {
            Ok(f) => f,
            Err(e) => return (Err(e.clone()), Err(e)),
        };
        let region = match apply {
            Some(a) => region_step(client, src, &f, packs, in_use, |p, v| a(p, v)).await,
            None => Err(Fail::Restart),
        };
        let loc = match loc_apply {
            Some(a) => locales_step(client, src, &f, packs, loc_in_use, |p, v| a(p, v)).await,
            None => Err(Fail::Restart),
        };
        (region, loc)
    };
    let (region, loc) = match tokio::time::timeout(TOTAL_TIMEOUT, work).await {
        Ok(r) => r,
        Err(_) => {
            let f = Fail::Offline(format!("no answer within {} s", TOTAL_TIMEOUT.as_secs()));
            (Err(f.clone()), Err(f))
        }
    };
    (report("local data pack", in_use, region), report("translations", loc_in_use, loc))
}

async fn attempt<F>(client: &reqwest::Client, src: &Source, packs: &Path, in_use: u64, apply: F) -> Result<u64, Fail>
where
    F: FnOnce(&Path, u64) -> Result<u64, Vec<Issue>>,
{
    let f = fetch_pack_index(client, src).await?;
    region_step(client, src, &f, packs, in_use, apply).await
}

/// The release index as it came: its text, its JSON and the addresses asked.
struct Fetched {
    text: String,
    value: Value,
    hops: Vec<Url>,
}

async fn fetch_pack_index(client: &reqwest::Client, src: &Source) -> Result<Fetched, Fail> {
    let (text, hops) = fetch_index(client, src, INDEX_NAME, MAX_INDEX_BYTES).await?;
    let value: Value = serde_json::from_str(&text).map_err(|e| bad(format!("the index: {e}")))?;
    Ok(Fetched { text, value, hops })
}

/// The local data pack the index names, when it is newer than in_use.
async fn region_step<F>(client: &reqwest::Client, src: &Source, f: &Fetched, packs: &Path, in_use: u64, apply: F) -> Result<u64, Fail>
where
    F: FnOnce(&Path, u64) -> Result<u64, Vec<Issue>>,
{
    if let Some(n) = f.value.get("format").and_then(Value::as_u64) {
        if n > u64::from(FORMAT) {
            return Err(Fail::UpdateBageo(format!("format {n} is newer than this Bageo reads ({FORMAT})")));
        }
    }
    let ix = parse_index(&f.text).map_err(|i| bad(first_issue(&i)))?;
    if ix.version <= in_use {
        return Err(Fail::UpToDate);
    }
    if !bageo_at_least(&ix.min_bageo) {
        return Err(Fail::UpdateBageo(format!("needs Bageo {} or newer", ix.min_bageo)));
    }
    if ix.bytes > MAX_PACK_BYTES {
        return Err(bad(format!("{} bytes, the limit is {MAX_PACK_BYTES}", ix.bytes)));
    }
    let url = src.pack_url(&f.hops, &ix.file).ok_or_else(|| bad("the index names a path, not a file"))?;
    let staged = download(client, src, url, ix.bytes, &ix.sha256, packs, packs_store::incoming_path(packs)).await?;
    apply(&staged.path, ix.version).map_err(|i| bad(first_issue(&i)))
}

/// The translations the index's locales entry names, when they are newer
/// than in_use. No entry: nothing to do.
async fn locales_step<F>(client: &reqwest::Client, src: &Source, f: &Fetched, packs: &Path, in_use: u64, apply: F) -> Result<u64, Fail>
where
    F: FnOnce(&Path, u64) -> Result<u64, Vec<Issue>>,
{
    let Some(entry) = f.value.get("locales").filter(|v| !v.is_null()) else { return Err(Fail::UpToDate) };
    if let Some(n) = entry.get("format").and_then(Value::as_u64) {
        if n > u64::from(LOCALES_FORMAT) {
            return Err(Fail::UpdateBageo(format!("translations format {n} is newer than this Bageo reads ({LOCALES_FORMAT})")));
        }
    }
    let r: LocalesRef = serde_json::from_value(entry.clone()).map_err(|e| bad(format!("the translations entry: {e}")))?;
    if r.version <= in_use {
        return Err(Fail::UpToDate);
    }
    if !bageo_at_least(&r.min_bageo) {
        return Err(Fail::UpdateBageo(format!("the translations need Bageo {} or newer", r.min_bageo)));
    }
    let issues = check_locales_ref(&r);
    if !issues.is_empty() {
        return Err(bad(first_issue(&issues)));
    }
    let url = src.pack_url(&f.hops, &r.file).ok_or_else(|| bad("the index names a path, not a file"))?;
    let staged = download(client, src, url, r.bytes, &r.sha256, packs, locales_store::incoming_path(packs)).await?;
    apply(&staged.path, r.version).map_err(|i| bad(first_issue(&i)))
}

/// Downloads url into a new temporary file: exactly bytes long with this
/// SHA-256, counted and hashed as it comes in.
async fn download(client: &reqwest::Client, src: &Source, url: Url, bytes: u64, sha256: &str, packs: &Path, path: PathBuf) -> Result<Staged, Fail> {
    std::fs::create_dir_all(packs).map_err(|e| Fail::Io(format!("cannot make the packs folder: {e}")))?;
    let mut staged = Staged::create(path)?;
    download_checked(client, src, url, bytes, sha256, &mut staged).await?;
    Ok(staged)
}

static RUNNING: AtomicBool = AtomicBool::new(false);
static LAST: Mutex<Option<Value>> = Mutex::new(None);

/// Holds the one place for a check or a restore.
struct Running;

impl Running {
    fn take() -> Option<Self> {
        // Built only when the place was free: dropping one frees it.
        (!RUNNING.swap(true, Ordering::SeqCst)).then(|| Running)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

/// What the console shows for a check of both files: updated when either
/// was; otherwise the local data pack's result, or the translations' when the
/// pack was up to date.
fn combine(region: Outcome, loc: Outcome) -> Outcome {
    match (region, loc) {
        (Outcome::Updated(v), _) => Outcome::Updated(v),
        (_, Outcome::Updated(_)) => Outcome::Updated(packs_store::current().version),
        (Outcome::UpToDate, other) => other,
        (r, _) => r,
    }
}

/// Checks for a newer pack and newer translations now and installs them. Runs
/// in a task of its own, so it finishes even when the console stops waiting.
pub(crate) async fn check_now() -> Outcome {
    let Some(running) = Running::take() else { return Outcome::Busy };
    let task = tokio::spawn(async move {
        let _running = running;
        let (region, loc) = check_official().await;
        let version = match region {
            Outcome::Updated(v) => v,
            _ => packs_store::current().version,
        };
        let outcome = combine(region, loc);
        *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some(json!({
            "at": chrono::Utc::now().to_rfc3339(),
            "result": outcome.code(),
            "version": version,
            "pack_result": region.code(),
            "locales_result": loc.code(),
            "locales_version": locales_store::current().version,
        }));
        outcome
    });
    task.await.unwrap_or(Outcome::Offline)
}

async fn check_official() -> (Outcome, Outcome) {
    let client = match client() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("local data pack update: no HTTP client ({e})");
            return (Outcome::Offline, Outcome::Offline);
        }
    };
    let apply: Apply = &packs_store::install_file;
    let loc_apply: Apply = &locales_store::install_file;
    check_both(
        &client,
        &Source::official(),
        &packs_store::installed_dir(),
        packs_store::current().version,
        (packs_store::installs_left() > 0).then_some(apply),
        locales_store::current().version,
        (locales_store::installs_left() > 0).then_some(loc_apply),
    )
    .await
}

/// Goes back to the built-in pack, translations and voice list: "restored",
/// "busy" while a check runs, or "io" when an installed file cannot be deleted
/// (that one stays in use).
pub(crate) fn restore() -> &'static str {
    let Some(_running) = Running::take() else { return Outcome::Busy.code() };
    restore_all(&crate::voice_packs::voices_root())
}

/// The built-in pack and translations, and the voice list Bageo came with
/// (kept in voices).
pub(crate) fn restore_all(voices: &Path) -> &'static str {
    if let Err(e) = restore_both() {
        tracing::warn!("local data pack or translations: cannot delete the installed file ({e}); still in use");
        return Outcome::Io.code();
    }
    if let Err(e) = crate::voice_packs::restore_builtin_index(voices) {
        tracing::warn!("voice list: cannot delete the downloaded one ({e}); still in use");
        return Outcome::Io.code();
    }
    if let Err(e) = crate::extras::restore_builtin_index(&crate::extras::extras_root()) {
        tracing::warn!("extras list: cannot delete the downloaded one ({e}); still in use");
        return Outcome::Io.code();
    }
    tracing::info!("local data pack, translations, voice list and extras list: back to the built-in ones");
    "restored"
}

/// Restores both files; both are tried, and either failing is a failure.
fn restore_both() -> std::io::Result<()> {
    let pack = packs_store::restore_builtin();
    let loc = locales_store::restore_builtin();
    pack.and(loc)
}

/// YYYYMMDDNN as YYYY-MM-DD ("" for the empty table).
fn version_date(v: u64) -> String {
    let d = v / 100;
    if d < 10_000_000 {
        return String::new();
    }
    format!("{:04}-{:02}-{:02}", d / 10_000, d / 100 % 100, d % 100)
}

fn status_json() -> Value {
    let t = packs_store::current();
    let l = locales_store::current();
    json!({
        "ok": true,
        "version": t.version,
        "date": version_date(t.version),
        "builtin": packs_store::builtin_in_use(),
        "regions": t.regions.len(),
        "locales": {
            "version": l.version,
            "date": version_date(l.version),
            "builtin": locales_store::builtin_in_use(),
            "languages": l.langs().filter(|(_, x)| x.stage >= locales_store::Stage::Call).count(),
        },
        // A downloaded voice list is kept: "Restore" puts the built-in one back too.
        "voice_list_kept": crate::voice_packs::kept_index(&crate::voice_packs::voices_root()),
        "extras_index_kept": crate::extras::kept_index(&crate::extras::extras_root()),
        "running": RUNNING.load(Ordering::SeqCst),
        "last_check": LAST.lock().unwrap_or_else(|e| e.into_inner()).clone(),
    })
}

/// Changes take JSON only (a form or a no-cors fetch from another page cannot send it).
pub(crate) fn json_request(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("application/json"))
}

pub(crate) fn json_required() -> Response {
    api_error(axum::http::StatusCode::BAD_REQUEST, "bad_request", "Content-Type must be application/json")
}

/// GET /api/packs/update: the pack in use and the last check.
async fn status(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    Json(status_json()).into_response()
}

/// POST /api/packs/update: check now, install a newer pack.
async fn update(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    if !json_request(&headers) {
        return json_required();
    }
    crate::extras::clear_index_cooldown();
    let outcome = check_now().await;
    state.settings_store.snapshot().apply_globals();
    let mut body = status_json();
    body["result"] = json!(outcome.code());
    Json(body).into_response()
}

/// POST /api/packs/restore: back to the built-in pack.
async fn restore_route(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    if !json_request(&headers) {
        return json_required();
    }
    let result = restore();
    state.settings_store.snapshot().apply_globals();
    let mut body = status_json();
    body["result"] = json!(result);
    Json(body).into_response()
}

const BAGEO_LATEST_URL: &str = "https://github.com/h4lo-void/bageo-call/releases/latest";
const BAGEO_TAG_PREFIX: &str = "https://github.com/h4lo-void/bageo-call/releases/tag/";

/// Extracts the `x.y.z` version from a GitHub release redirect `Location` URL.
fn parse_bageo_release_location(loc: &str) -> Option<String> {
    let base = Url::parse(BAGEO_LATEST_URL).ok()?;
    let target = base.join(loc).ok()?;
    let tag = target.as_str().strip_prefix(BAGEO_TAG_PREFIX)?;
    let ver = tag.trim_start_matches(['v', 'V']);
    (!ver.contains('/') && !ver.contains('?') && !ver.contains('#') && version_parts(ver).is_some())
        .then(|| ver.to_string())
}

/// POST /api/packs/bageo-check: manual check of the latest Bageo release on GitHub.
/// Never runs automatically; only when the user clicks "Check for updates".
async fn bageo_check(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    if !json_request(&headers) {
        return json_required();
    }
    let Ok(c) = client_with(CONNECT_TIMEOUT) else {
        return Json(json!({ "ok": true, "result": "offline", "current": BAGEO_VERSION, "url": BAGEO_LATEST_URL })).into_response();
    };
    let resp = match c.get(BAGEO_LATEST_URL).send().await {
        Ok(r) => r,
        Err(_) => {
            return Json(json!({ "ok": true, "result": "offline", "current": BAGEO_VERSION, "url": BAGEO_LATEST_URL })).into_response();
        }
    };
    let latest = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_bageo_release_location);
    match latest {
        Some(ver) if !bageo_at_least(&ver) => Json(json!({
            "ok": true,
            "result": "update_available",
            "current": BAGEO_VERSION,
            "latest": ver,
            "url": BAGEO_LATEST_URL,
        }))
        .into_response(),
        Some(ver) => Json(json!({
            "ok": true,
            "result": "up_to_date",
            "current": BAGEO_VERSION,
            "latest": ver,
            "url": BAGEO_LATEST_URL,
        }))
        .into_response(),
        None => Json(json!({ "ok": true, "result": "offline", "current": BAGEO_VERSION, "url": BAGEO_LATEST_URL })).into_response(),
    }
}

pub fn router() -> Router<CallsState> {
    Router::new()
        .route("/api/packs/update", get(status).post(update))
        .route("/api/packs/restore", post(restore_route))
        .route("/api/packs/bageo-check", post(bageo_check))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    const LATEST: &str = "/h4lo-void/bageo-packs/releases/latest/download/bageo-pack-index.json";
    const TAGGED: &str = "/h4lo-void/bageo-packs/releases/download/v9/";
    const PACK: &str = "bageo-pack.json";

    fn url_allowed(raw: &str) -> bool {
        Url::parse(raw).is_ok_and(|u| hop_allowed(&u))
    }

    pub(crate) enum Reply {
        Body(Vec<u8>),
        Redirect(String),
        /// Declares this many bytes, sends the rest, then hangs up.
        Cut(usize, Vec<u8>),
        /// No length; 8 MiB, then hangs up.
        Endless,
        NotFound,
    }

    /// A tiny HTTP server playing GitHub, one connection at a time.
    pub(crate) struct Server {
        pub(crate) port: u16,
        pub(crate) hits: Arc<Mutex<Vec<String>>>,
    }

    impl Server {
        pub(crate) fn start(route: impl Fn(&str) -> Reply + Send + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let hits = Arc::new(Mutex::new(Vec::new()));
            let seen = hits.clone();
            std::thread::spawn(move || {
                for s in listener.incoming() {
                    let Ok(s) = s else { continue };
                    let Some(path) = request_path(&s) else { continue };
                    seen.lock().unwrap().push(path.clone());
                    answer(s, route(&path));
                }
            });
            Server { port, hits }
        }

        pub(crate) fn source(&self) -> Source {
            Source::local(self.port)
        }

        pub(crate) fn asked(&self, end: &str) -> usize {
            self.hits.lock().unwrap().iter().filter(|p| p.ends_with(end)).count()
        }
    }

    fn request_path(s: &TcpStream) -> Option<String> {
        let mut r = BufReader::new(s);
        let mut first = String::new();
        r.read_line(&mut first).ok()?;
        loop {
            let mut line = String::new();
            if r.read_line(&mut line).ok()? == 0 || line == "\r\n" {
                break;
            }
        }
        first.split_whitespace().nth(1).map(str::to_string)
    }

    fn answer(mut s: TcpStream, reply: Reply) {
        let head = |status: &str, extra: String| format!("HTTP/1.1 {status}\r\n{extra}Connection: close\r\n\r\n");
        let _ = match reply {
            Reply::Body(b) => s.write_all(head("200 OK", format!("Content-Length: {}\r\n", b.len())).as_bytes()).and_then(|_| s.write_all(&b)),
            Reply::Redirect(to) => s.write_all(head("302 Found", format!("Location: {to}\r\nContent-Length: 0\r\n")).as_bytes()),
            Reply::Cut(declared, b) => s.write_all(head("200 OK", format!("Content-Length: {declared}\r\n")).as_bytes()).and_then(|_| s.write_all(&b)),
            Reply::Endless => s.write_all(head("200 OK", String::new()).as_bytes()).and_then(|_| {
                let block = vec![b' '; 64 * 1024];
                (0..128).try_for_each(|_| s.write_all(&block))
            }),
            Reply::NotFound => s.write_all(head("404 Not Found", "Content-Length: 0\r\n".into()).as_bytes()),
        };
        let _ = s.flush();
        let _ = s.shutdown(Shutdown::Both);
    }

    pub(crate) struct Dir(pub(crate) PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) fn tmp() -> Dir {
        let d = std::env::temp_dir().join(format!("bageo-pack-update-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    fn v0() -> u64 {
        packs_store::builtin().version
    }

    fn pack(version: u64, edit: impl FnOnce(&mut Value)) -> Vec<u8> {
        let mut v: Value = serde_json::from_str(packs_store::builtin_json()).unwrap();
        v["version"] = version.into();
        edit(&mut v);
        serde_json::to_vec(&v).unwrap()
    }

    fn index_for(body: &[u8], version: u64) -> Value {
        json!({"format": FORMAT, "version": version, "file": PACK, "bytes": body.len(), "sha256": format!("{:x}", Sha256::digest(body)), "min_bageo": "1.0.0"})
    }

    /// A release: the latest index leads to the tagged one, which names the pack.
    fn release(index: Value, pack: Vec<u8>) -> Server {
        let index = serde_json::to_vec(&index).unwrap();
        Server::start(move |path| {
            if path == LATEST {
                Reply::Redirect(format!("{TAGGED}{INDEX_NAME}"))
            } else if path == format!("{TAGGED}{INDEX_NAME}") {
                Reply::Body(index.clone())
            } else if path == format!("{TAGGED}{PACK}") {
                Reply::Body(pack.clone())
            } else {
                Reply::NotFound
            }
        })
    }

    /// One check into a folder of its own, with its own install count.
    async fn run(src: &Source, data: &Path, in_use: u64) -> Outcome {
        let count = AtomicUsize::new(0);
        let dir = data.to_path_buf();
        check_with(&client().unwrap(), src, &data.join("packs"), in_use, |staged, want| {
            packs_store::install_file_counted(&count, &dir, staged, in_use, Some(want)).map(|t| t.version)
        })
        .await
    }

    /// The packs folder holds nothing (no install, no download left).
    fn empty(data: &Path) -> bool {
        std::fs::read_dir(data.join("packs")).map(|d| d.count() == 0).unwrap_or(true)
    }

    /// One check the way the console's button runs it (this test's own data folder).
    async fn run_real(src: &Source) -> Outcome {
        check_with(&client().unwrap(), src, &packs_store::installed_dir(), packs_store::current().version, packs_store::install_file).await
    }

    /// The index straight from latest/, and for the pack whatever `pack` answers.
    fn pack_step(index: Value, pack: impl Fn() -> Reply + Send + 'static) -> Server {
        let index = serde_json::to_vec(&index).unwrap();
        Server::start(move |path| if path == LATEST { Reply::Body(index.clone()) } else if path.ends_with(&format!("/{PACK}")) { pack() } else { Reply::NotFound })
    }

    #[tokio::test]
    async fn an_update_is_installed_and_the_next_call_uses_it() {
        let v = v0() + 1;
        let body = pack(v, |_| {});
        let s = release(index_for(&body, v), body.clone());
        assert!(packs_store::builtin_in_use());
        let out = check_with(&client().unwrap(), &s.source(), &packs_store::installed_dir(), packs_store::current().version, packs_store::install_file).await;
        assert_eq!(out, Outcome::Updated(v));
        assert_eq!(packs_store::current().version, v, "the next call reads the new pack");
        assert!(!packs_store::builtin_in_use());
        let st = status_json();
        assert_eq!((st["version"].as_u64(), st["builtin"].as_bool()), (Some(v), Some(false)));
        assert_eq!(st["regions"].as_u64(), Some(packs_store::builtin().regions.len() as u64));
        assert_eq!(s.asked(&format!("{TAGGED}{PACK}")), 1, "the pack comes from the index's release");
        let packs = packs_store::installed_dir();
        assert_eq!(std::fs::read(packs.join(packs_store::FILE_NAME)).unwrap(), body);
        assert!(!packs_store::has_incoming(&packs));

        // Back to the built-in pack.
        packs_store::restore_builtin().unwrap();
        assert_eq!(packs_store::current().version, v0());
        assert!(packs_store::builtin_in_use());
        assert!(!packs.join(packs_store::FILE_NAME).exists());
        let _ = std::fs::remove_dir_all(packs.parent().unwrap());
    }

    #[tokio::test]
    async fn one_changed_byte_is_refused() {
        let d = tmp();
        let v = v0() + 1;
        let body = pack(v, |_| {});
        let ix = index_for(&body, v);
        let mut changed = body;
        let mid = changed.len() / 2;
        changed[mid] ^= 0x01;
        let s = release(ix, changed);
        assert_eq!(run(&s.source(), &d.0, v0()).await, Outcome::BadPack);
        assert!(empty(&d.0));
    }

    #[tokio::test]
    async fn a_size_unlike_the_index_is_refused() {
        let v = v0() + 1;
        let body = pack(v, |_| {});
        for delta in [-1i64, 1] {
            let d = tmp();
            let mut ix = index_for(&body, v);
            ix["bytes"] = json!(body.len() as i64 + delta);
            let s = release(ix, body.clone());
            assert_eq!(run(&s.source(), &d.0, v0()).await, Outcome::BadPack, "{delta}");
            assert!(empty(&d.0));
        }
    }

    fn redirecting(to: String) -> Server {
        Server::start(move |path| if path == LATEST { Reply::Redirect(to.clone()) } else { Reply::NotFound })
    }

    #[tokio::test]
    async fn a_redirect_to_another_site_is_refused() {
        for to in [
            "https://evil.example/bageo-pack-index.json",
            "https://github.com.attacker.example/h4lo-void/bageo-packs/releases/download/v9/bageo-pack-index.json",
            "https://raw.githubusercontent.com/h4lo-void/bageo-packs/main/bageo-pack-index.json",
            "https://github.com/someone/else/releases/download/v9/bageo-pack-index.json",
        ] {
            let d = tmp();
            let s = redirecting(to.to_string());
            assert_eq!(run(&s.source(), &d.0, v0()).await, Outcome::BadPack, "{to}");
            assert_eq!(s.hits.lock().unwrap().len(), 1, "{to}");
        }
        // Another name for the same test server is another site too.
        let d = tmp();
        let s = Server::start(|path| if path == LATEST { Reply::Redirect("http://localhost:1/x".into()) } else { Reply::NotFound });
        assert_eq!(run(&s.source(), &d.0, v0()).await, Outcome::BadPack);
    }

    #[tokio::test]
    async fn a_redirect_of_the_pack_to_another_site_is_refused() {
        let v = v0() + 1;
        let body = pack(v, |_| {});
        for to in [
            "https://evil.example/bageo-pack.json",
            "https://objects.githubusercontent.com.attacker.example/bageo-pack.json",
            "https://raw.githubusercontent.com/h4lo-void/bageo-packs/main/bageo-pack.json",
        ] {
            let d = tmp();
            let s = pack_step(index_for(&body, v), move || Reply::Redirect(to.to_string()));
            assert_eq!(run(&s.source(), &d.0, v0()).await, Outcome::BadPack, "{to}");
            assert_eq!(s.asked(PACK), 1, "{to}");
            assert!(empty(&d.0), "{to}");
        }
    }

    #[tokio::test]
    async fn a_redirect_of_the_pack_to_plain_http_is_refused() {
        let v = v0() + 1;
        let body = pack(v, |_| {});
        for to in ["http://objects.githubusercontent.com/bageo-pack.json", "http://github.com/h4lo-void/bageo-packs/releases/download/v9/bageo-pack.json"] {
            let d = tmp();
            let s = pack_step(index_for(&body, v), move || Reply::Redirect(to.to_string()));
            assert_eq!(run(&s.source(), &d.0, v0()).await, Outcome::BadPack, "{to}");
            assert_eq!(s.asked(PACK), 1, "{to}");
            assert!(empty(&d.0), "{to}");
        }
    }

    #[tokio::test]
    async fn a_redirect_to_plain_http_is_refused() {
        let d = tmp();
        let s = redirecting("http://github.com/h4lo-void/bageo-packs/releases/download/v9/bageo-pack-index.json".into());
        assert_eq!(run(&s.source(), &d.0, v0()).await, Outcome::BadPack);
        let s = redirecting("http://objects.githubusercontent.com/x".into());
        assert_eq!(run(&s.source(), &d.0, v0()).await, Outcome::BadPack);
        assert!(empty(&d.0));
    }

    /// The index after this many redirects.
    fn hops(redirects: usize, index: Value, body: Vec<u8>) -> Server {
        let index = serde_json::to_vec(&index).unwrap();
        let hop = |i: usize, n: usize| if i < n { format!("/h4lo-void/bageo-packs/releases/hop/{i}") } else { format!("{TAGGED}{INDEX_NAME}") };
        Server::start(move |path| {
            if path == LATEST {
                Reply::Redirect(hop(1, redirects))
            } else if let Some(i) = path.strip_prefix("/h4lo-void/bageo-packs/releases/hop/").and_then(|i| i.parse::<usize>().ok()) {
                Reply::Redirect(hop(i + 1, redirects))
            } else if path == format!("{TAGGED}{INDEX_NAME}") {
                Reply::Body(index.clone())
            } else if path == format!("{TAGGED}{PACK}") {
                Reply::Body(body.clone())
            } else {
                Reply::NotFound
            }
        })
    }

    #[tokio::test]
    async fn more_than_five_redirects_are_refused() {
        let v = v0() + 1;
        let body = pack(v, |_| {});
        let d = tmp();
        let s = hops(5, index_for(&body, v), body.clone());
        assert_eq!(run(&s.source(), &d.0, v0()).await, Outcome::Updated(v), "five are fine");
        let d = tmp();
        let s = hops(6, index_for(&body, v), body.clone());
        assert_eq!(run(&s.source(), &d.0, v0()).await, Outcome::BadPack);
        assert_eq!(s.asked(INDEX_NAME), 1, "only the latest/ link: the index itself was never asked");
        assert!(empty(&d.0));
    }

    #[tokio::test]
    async fn an_index_or_pack_over_its_limit_is_refused() {
        let v = v0() + 1;
        let body = pack(v, |_| {});
        // An index over 16 KiB, with or without a length.
        let mut big = serde_json::to_vec(&index_for(&body, v)).unwrap();
        big.extend(vec![b' '; 17 * 1024]);
        for reply in [0, 1] {
            let d = tmp();
            let big = big.clone();
            let s = Server::start(move |path| match (path == LATEST, reply) {
                (true, 0) => Reply::Body(big.clone()),
                (true, _) => Reply::Endless,
                _ => Reply::NotFound,
            });
            assert_eq!(run(&s.source(), &d.0, v0()).await, Outcome::BadPack);
            assert!(empty(&d.0));
        }
        // An index naming a pack over 4 MiB: not downloaded.
        let d = tmp();
        let mut ix = index_for(&body, v);
        ix["bytes"] = json!(MAX_PACK_BYTES + 1);
        let s = release(ix, body.clone());
        assert_eq!(run(&s.source(), &d.0, v0()).await, Outcome::BadPack);
        assert_eq!(s.asked(PACK), 0);
        // A pack that keeps coming past its size: cut off.
        let d = tmp();
        let index = serde_json::to_vec(&index_for(&body, v)).unwrap();
        let s = Server::start(move |path| if path == LATEST { Reply::Body(index.clone()) } else { Reply::Endless });
        assert_eq!(run(&s.source(), &d.0, v0()).await, Outcome::BadPack);
        assert!(empty(&d.0));
    }

    #[tokio::test]
    async fn an_index_naming_a_path_or_address_is_refused() {
        let v = v0() + 1;
        let body = pack(v, |_| {});
        for file in ["../bageo-pack.json", "sub/bageo-pack.json", "https://evil.example/bageo-pack.json", "..", ".incoming-1.json", "a%2fb.json", "a\\b.json", "//evil.example/x"] {
            let d = tmp();
            let mut ix = index_for(&body, v);
            ix["file"] = json!(file);
            let s = release(ix, body.clone());
            assert_eq!(run(&s.source(), &d.0, v0()).await, Outcome::BadPack, "{file}");
            assert_eq!(s.hits.lock().unwrap().len(), 2, "{file}: only the index");
            assert!(empty(&d.0));
        }
    }

    #[tokio::test]
    async fn the_same_or_an_older_version_is_not_applied() {
        let v = v0() + 1;
        let body = pack(v, |_| {});
        for (ix_version, in_use) in [(v, v), (v0(), v)] {
            let d = tmp();
            let s = release(index_for(&body, ix_version), body.clone());
            assert_eq!(run(&s.source(), &d.0, in_use).await, Outcome::UpToDate);
            assert_eq!(s.asked(PACK), 0);
            assert!(empty(&d.0));
        }
    }

    #[tokio::test]
    async fn a_newer_format_or_bageo_asks_to_update_bageo() {
        let v = v0() + 1;
        let body = pack(v, |_| {});
        for (field, value) in [("format", json!(FORMAT + 1)), ("min_bageo", json!("99.0.0"))] {
            let d = tmp();
            let mut ix = index_for(&body, v);
            ix[field] = value;
            let s = release(ix, body.clone());
            assert_eq!(run(&s.source(), &d.0, v0()).await, Outcome::UpdateBageo, "{field}");
            assert_eq!(s.asked(PACK), 0);
        }
        assert!(bageo_at_least(BAGEO_VERSION) && bageo_at_least("1.0.0") && !bageo_at_least("1.6") && !bageo_at_least("x"));
    }

    #[test]
    fn versions_compare_by_their_numbers() {
        assert!(version_at_least("1.10.0", "1.9.0") && !version_at_least("1.9.0", "1.10.0"));
        assert!(version_at_least("1.7.0-rc.1", "1.6.9") && version_at_least("1.7.0-beta", "1.7.0"));
        assert!(version_at_least("1.6.9+abc", "1.6.9") && !version_at_least("1.6.9+abc", "1.7.0"));
        assert!(!version_at_least("1.6.8-rc.1", "1.6.9"));
        assert_eq!(version_parts("1.7.0-rc.1+build.5"), Some((1, 7, 0)));
        assert!(version_parts("-1.2.3").is_none() && version_parts("1.2").is_none() && version_parts("1.2.3.4").is_none());
    }

    #[tokio::test]
    async fn a_pack_that_fails_the_checks_is_refused_and_nothing_changes() {
        let v = v0() + 1;
        let good = pack(v, |_| {});
        let s = release(index_for(&good, v), good.clone());
        assert_eq!(run_real(&s.source()).await, Outcome::Updated(v));
        let packs = packs_store::installed_dir();
        let installed = packs.join(packs_store::FILE_NAME);
        let in_use = packs_store::current();
        // Right checksum, but text the checks refuse (a phone number and a handle).
        let banned = pack(v + 1, |p| p["regions"]["us"]["habits"]["en"] = "Call me at 0912345678 or @someone".into());
        let s = release(index_for(&banned, v + 1), banned);
        assert_eq!(run_real(&s.source()).await, Outcome::BadPack);
        assert_eq!(s.asked(PACK), 1, "it was downloaded, then refused");
        assert!(std::ptr::eq(packs_store::current(), in_use) && packs_store::current().version == v, "calls keep the pack in use");
        assert_eq!(std::fs::read(&installed).unwrap(), good, "the installed pack stays");
        // A pack whose version is not the index's.
        let other = pack(v + 2, |_| {});
        let s = release(index_for(&other, v + 1), other);
        assert_eq!(run_real(&s.source()).await, Outcome::BadPack);
        assert!(std::ptr::eq(packs_store::current(), in_use), "calls keep the pack in use");
        assert_eq!(std::fs::read(&installed).unwrap(), good, "the installed pack stays");
        assert_eq!(status_json()["version"].as_u64(), Some(v));
        assert!(!packs_store::has_incoming(&packs));
        let _ = std::fs::remove_dir_all(packs.parent().unwrap());
    }

    #[tokio::test]
    async fn a_download_cut_halfway_leaves_nothing() {
        let d = tmp();
        let v = v0() + 1;
        let body = pack(v, |_| {});
        let index = serde_json::to_vec(&index_for(&body, v)).unwrap();
        let half = body[..body.len() / 2].to_vec();
        let len = body.len();
        let s = Server::start(move |path| if path == LATEST { Reply::Body(index.clone()) } else { Reply::Cut(len, half.clone()) });
        let out = run(&s.source(), &d.0, v0()).await;
        assert!(matches!(out, Outcome::Offline | Outcome::BadPack), "{out:?}");
        assert_eq!(s.asked(PACK), 1);
        assert!(empty(&d.0));
    }

    #[tokio::test]
    async fn no_connection_is_a_retry_and_calls_go_on() {
        let d = tmp();
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        assert_eq!(run(&Source::local(port), &d.0, v0()).await, Outcome::Offline);
        assert!(empty(&d.0));
        // Calls still get their regional part.
        assert!(packs_store::current().region("tw").is_some());
        // GitHub answering with an error is a retry too.
        let s = Server::start(|_| Reply::NotFound);
        assert_eq!(run(&s.source(), &d.0, v0()).await, Outcome::Offline);
    }

    #[tokio::test]
    async fn one_check_at_a_time() {
        let held = Running::take().expect("free");
        assert_eq!(check_now().await, Outcome::Busy);
        assert_eq!(restore(), "busy", "no restore during a check");
        drop(held);
        assert!(Running::take().is_some());
    }

    #[test]
    fn only_github_release_addresses_are_allowed() {
        for ok in [
            "https://github.com/h4lo-void/bageo-packs/releases/latest/download/bageo-pack-index.json",
            "https://GitHub.COM/h4lo-void/bageo-packs/releases/download/v1/bageo-pack.json",
            "https://github.com:443/h4lo-void/bageo-packs/releases/download/v1/bageo-pack.json",
            "https://objects.githubusercontent.com/github-production-release-asset-2e65be/1?X-Amz-Signature=x",
            "https://release-assets.githubusercontent.com/github-production-release-asset/1/2?sp=r",
        ] {
            assert!(url_allowed(ok), "{ok}");
        }
        for no in [
            "http://github.com/h4lo-void/bageo-packs/releases/download/v1/x.json",
            "https://github.com./h4lo-void/bageo-packs/releases/download/v1/x.json",
            "https://objects.githubusercontent.com./x",
            "https://user@github.com/h4lo-void/bageo-packs/releases/download/v1/x.json",
            "https://user:pw@objects.githubusercontent.com/x",
            "https://github.com@evil.example/h4lo-void/bageo-packs/releases/download/v1/x.json",
            "https://github.com\\@evil.example/x",
            "https://github.com:8443/h4lo-void/bageo-packs/releases/download/v1/x.json",
            "https://objects.githubusercontent.com:444/x",
            "https://evil.github.com.attacker.example/h4lo-void/bageo-packs/releases/x",
            "https://github.com.attacker.example/h4lo-void/bageo-packs/releases/x",
            "https://evil.github.com/h4lo-void/bageo-packs/releases/x",
            "https://evil.objects.githubusercontent.com/x",
            "https://raw.githubusercontent.com/h4lo-void/bageo-packs/main/x.json",
            "https://gіthub.com/h4lo-void/bageo-packs/releases/x",
            "https://githսb.com/h4lo-void/bageo-packs/releases/x",
            "https://xn--github-8ya.com/h4lo-void/bageo-packs/releases/x",
            "https://140.82.112.3/h4lo-void/bageo-packs/releases/x",
            "https://[::1]/h4lo-void/bageo-packs/releases/x",
            "https://github.com/someone/else/releases/download/v1/x.json",
            "https://github.com/h4lo-void/bageo-packs-evil/releases/download/v1/x.json",
            "https://github.com/h4lo-void/bageo-packs/releases/../../../evil/x.json",
            "https://github.com/h4lo-void/bageo-packs/archive/main.zip",
            "file:///etc/passwd",
            "github.com/h4lo-void/bageo-packs/releases/x",
            "",
        ] {
            assert!(!url_allowed(no), "{no}");
        }
    }

    #[test]
    fn the_pack_comes_from_the_release_of_its_index() {
        let src = Source::official();
        let tagged = Url::parse(&format!("{RELEASES}/download/v2026112101/{INDEX_NAME}")).unwrap();
        let latest = Url::parse(&src.index_url()).unwrap();
        assert_eq!(src.pack_url(&[latest.clone(), tagged], PACK).unwrap().as_str(), format!("{RELEASES}/download/v2026112101/{PACK}"));
        assert_eq!(src.pack_url(&[latest.clone()], PACK).unwrap().as_str(), format!("{RELEASES}/latest/download/{PACK}"));
        assert!(src.pack_url(&[latest], "../x.json").is_none());
        assert_eq!(version_date(2026112001), "2026-11-20");
        assert_eq!(version_date(0), "");
    }

    // ---- the translations, downloaded with the pack ----

    const LOC: &str = "bageo-locales.json.gz";

    fn lv0() -> u64 {
        locales_store::builtin().version
    }

    fn loc_entry(body: &[u8], version: u64) -> Value {
        json!({"file": LOC, "bytes": body.len(), "sha256": format!("{:x}", Sha256::digest(body)), "format": 1, "version": version, "min_bageo": "1.0.0"})
    }

    /// A release with both files: the latest index leads to the tagged one.
    fn release2(index: Value, pack: Vec<u8>, loc: Vec<u8>) -> Server {
        let index = serde_json::to_vec(&index).unwrap();
        Server::start(move |path| {
            if path == LATEST {
                Reply::Redirect(format!("{TAGGED}{INDEX_NAME}"))
            } else if path == format!("{TAGGED}{INDEX_NAME}") {
                Reply::Body(index.clone())
            } else if path == format!("{TAGGED}{PACK}") {
                Reply::Body(pack.clone())
            } else if path == format!("{TAGGED}{LOC}") {
                Reply::Body(loc.clone())
            } else {
                Reply::NotFound
            }
        })
    }

    /// Both files into a folder of their own, each with its own install count.
    async fn run_both(src: &Source, data: &Path, in_use: u64, loc_in_use: u64) -> (Outcome, Outcome) {
        let (c1, c2) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let packs = data.join("packs");
        let dir = data.to_path_buf();
        let apply = |staged: &Path, want: u64| packs_store::install_file_counted(&c1, &dir, staged, in_use, Some(want)).map(|t| t.version);
        let loc_apply = |staged: &Path, want: u64| locales_store::install_file_counted(&c2, &packs, staged, loc_in_use, Some(want)).map(|t| t.version);
        check_both(&client().unwrap(), src, &data.join("packs"), in_use, Some(&apply), loc_in_use, Some(&loc_apply)).await
    }

    fn has_file(data: &Path, name: &str) -> bool {
        data.join("packs").join(name).is_file()
    }

    #[tokio::test]
    async fn translations_come_with_the_pack_and_the_next_call_uses_them() {
        let (v, lv) = (v0() + 1, lv0() + 1);
        let body = pack(v, |_| {});
        // A hello the shipped file does not have, to tell which file a call reads.
        let loc = locales_store::testing::gz(lv, |v| v["langs"]["it"]["speech"]["hello"] = "Pronto, chi parla?".into());
        let mut ix = index_for(&body, v);
        ix["locales"] = loc_entry(&loc, lv);
        let s = release2(ix, body, loc.clone());
        let apply: Apply = &packs_store::install_file;
        let loc_apply: Apply = &locales_store::install_file;
        let (region, l) = check_both(
            &client().unwrap(),
            &s.source(),
            &packs_store::installed_dir(),
            packs_store::current().version,
            Some(apply),
            locales_store::current().version,
            Some(loc_apply),
        )
        .await;
        assert_eq!((region, l), (Outcome::Updated(v), Outcome::Updated(lv)));
        assert_eq!(s.asked(&format!("{TAGGED}{LOC}")), 1, "from the index's release");
        let it = crate::locale::Lang::Ext(crate::locale::Ext::from_code("it").unwrap());
        let it_ext = crate::locale::Ext::from_code("it").unwrap();
        assert_ne!(locales_store::builtin().phrases(it_ext).map(|p| p.hello), Some("Pronto, chi parla?"));
        assert_eq!(locales_store::stage(it), locales_store::Stage::Notify);
        assert!(crate::prompts::pack(it).kick_open_persona.contains("Pronto, chi parla?"), "the next call reads the download");
        assert_eq!(status_json()["locales"]["version"].as_u64(), Some(lv));
        let packs = packs_store::installed_dir();
        assert_eq!(std::fs::read(packs.join(locales_store::FILE_NAME)).unwrap(), loc);
        assert!(!locales_store::has_incoming(&packs) && !packs_store::has_incoming(&packs));
        // Both back to the built-in files.
        restore_both().unwrap();
        assert!(packs_store::builtin_in_use() && locales_store::builtin_in_use());
        assert_eq!(locales_store::stage(it), locales_store::builtin().stage(it_ext));
        assert!(!crate::prompts::pack(it).kick_open_persona.contains("chi parla"), "the next call reads the shipped file again");
        let _ = std::fs::remove_dir_all(packs.parent().unwrap());
    }

    #[tokio::test]
    async fn a_broken_translations_file_costs_only_itself() {
        let (v, lv) = (v0() + 1, lv0() + 1);
        let body = pack(v, |_| {});
        let loc = locales_store::testing::gz(lv, |_| {});
        // One changed byte.
        let d = tmp();
        let mut ix = index_for(&body, v);
        ix["locales"] = loc_entry(&loc, lv);
        let mut changed = loc.clone();
        let mid = changed.len() / 2;
        changed[mid] ^= 0x01;
        let s = release2(ix.clone(), body.clone(), changed);
        assert_eq!(run_both(&s.source(), &d.0, v0(), lv0()).await, (Outcome::Updated(v), Outcome::BadPack));
        assert!(has_file(&d.0, packs_store::FILE_NAME) && !has_file(&d.0, locales_store::FILE_NAME));
        assert!(!locales_store::has_incoming(&d.0.join("packs")));
        // Data after the gzip member, listed with its own size and checksum.
        let d = tmp();
        let mut tail = loc.clone();
        tail.extend_from_slice(b"\0more");
        let mut ix = index_for(&body, v);
        ix["locales"] = loc_entry(&tail, lv);
        let s = release2(ix, body.clone(), tail);
        assert_eq!(run_both(&s.source(), &d.0, v, lv0()).await, (Outcome::UpToDate, Outcome::BadPack));
        assert!(!has_file(&d.0, locales_store::FILE_NAME) && !locales_store::has_incoming(&d.0.join("packs")));
        // A pack that fails leaves the translations alone.
        let d = tmp();
        let mut ix = index_for(&body, v);
        ix["locales"] = loc_entry(&loc, lv);
        let mut bad_pack = body.clone();
        bad_pack[mid.min(body.len() - 1)] ^= 0x01;
        let s = release2(ix, bad_pack, loc.clone());
        assert_eq!(run_both(&s.source(), &d.0, v0(), lv0()).await, (Outcome::BadPack, Outcome::Updated(lv)));
        assert!(has_file(&d.0, locales_store::FILE_NAME) && !has_file(&d.0, packs_store::FILE_NAME));
    }

    #[tokio::test]
    async fn translations_over_their_limit_older_or_newer_in_format_are_not_downloaded() {
        let (v, lv) = (v0() + 1, lv0() + 1);
        let body = pack(v, |_| {});
        let loc = locales_store::testing::gz(lv, |_| {});
        let cases: [(&str, Value, u64, Outcome); 5] = [
            ("bytes", json!(bageo_packs::locales::MAX_LOCALES_BYTES + 1), lv0(), Outcome::BadPack),
            ("version", json!(lv0()), lv0(), Outcome::UpToDate),
            ("version", json!(lv), lv, Outcome::UpToDate),
            ("format", json!(2), lv0(), Outcome::UpdateBageo),
            ("min_bageo", json!("99.0.0"), lv0(), Outcome::UpdateBageo),
        ];
        for (field, value, loc_in_use, want) in cases {
            let d = tmp();
            let mut ix = index_for(&body, v);
            ix["locales"] = loc_entry(&loc, lv);
            ix["locales"][field] = value.clone();
            let s = release2(ix, body.clone(), loc.clone());
            assert_eq!(run_both(&s.source(), &d.0, v0(), loc_in_use).await, (Outcome::Updated(v), want), "{field} {value}");
            assert_eq!(s.asked(LOC), 0, "{field}: not downloaded");
        }
        // A file that keeps coming past its listed size: cut off.
        let d = tmp();
        let mut ix = index_for(&body, v);
        ix["locales"] = loc_entry(&loc, lv);
        let index = serde_json::to_vec(&ix).unwrap();
        let pk = body.clone();
        let s = Server::start(move |path| {
            if path == LATEST {
                Reply::Body(index.clone())
            } else if path.ends_with(&format!("/{PACK}")) {
                Reply::Body(pk.clone())
            } else {
                Reply::Endless
            }
        });
        assert_eq!(run_both(&s.source(), &d.0, v0(), lv0()).await, (Outcome::Updated(v), Outcome::BadPack));
        assert!(!locales_store::has_incoming(&d.0.join("packs")));
        // No entry at all: up to date, as an older release.
        let d = tmp();
        let s = release2(index_for(&body, v), body.clone(), loc);
        assert_eq!(run_both(&s.source(), &d.0, v0(), lv0()).await, (Outcome::Updated(v), Outcome::UpToDate));
        assert_eq!(combine(Outcome::UpToDate, Outcome::Updated(lv)), Outcome::Updated(packs_store::current().version));
        assert_eq!(combine(Outcome::UpToDate, Outcome::BadPack), Outcome::BadPack);
        assert_eq!(combine(Outcome::Offline, Outcome::UpToDate), Outcome::Offline);
    }

    #[tokio::test]
    async fn a_restore_fails_when_either_file_cannot_be_deleted() {
        let (v, lv) = (v0() + 1, lv0() + 1);
        let body = pack(v, |_| {});
        let loc = locales_store::testing::gz(lv, |_| {});
        let mut ix = index_for(&body, v);
        ix["locales"] = loc_entry(&loc, lv);
        let s = release2(ix, body, loc);
        let apply: Apply = &packs_store::install_file;
        let loc_apply: Apply = &locales_store::install_file;
        let packs = packs_store::installed_dir();
        let out = check_both(&client().unwrap(), &s.source(), &packs, packs_store::current().version, Some(apply), locales_store::current().version, Some(loc_apply)).await;
        assert_eq!(out, (Outcome::Updated(v), Outcome::Updated(lv)));
        // The translations file cannot be deleted (a folder is in its place).
        let file = packs.join(locales_store::FILE_NAME);
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert!(restore_both().is_err(), "one file left: the restore failed");
        assert!(packs_store::builtin_in_use(), "the pack that could be deleted is back");
        assert!(!locales_store::builtin_in_use(), "the translations still in use");
        std::fs::remove_dir(&file).unwrap();
        restore_both().unwrap();
        assert!(packs_store::builtin_in_use() && locales_store::builtin_in_use());
        let _ = std::fs::remove_dir_all(packs.parent().unwrap());
    }
}
