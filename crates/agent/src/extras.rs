// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! On-demand extra packs: full language packs (`lang-<code>.json.gz`, carrying
//! notification, console and desktop widget text outside the 9 built-in core
//! languages) and region mascot art (`art-<region>.json`, loaded on demand for
//! the 64 non-built-in regions).
//!
//! Listed in `bageo-extras-index.json` inside the public `bageo-packs`
//! releases on GitHub, downloaded through [crate::pack_update] with the same
//! fixed host allowlist, redirect checks, SHA-256 verification and size caps:
//! - Index: at most [MAX_INDEX_BYTES] (256 KiB), format <= [EXTRAS_FORMAT].
//! - Language pack: at most [MAX_EXTRA_LANG_BYTES] (600 KiB) both compressed
//!   and unpacked ([crate::locales_store::install_extra_lang]); installs and
//!   deletions share the translations' per-run memory cap
//!   ([crate::locales_store::MAX_NEW_BYTES]).
//! - Mascot art: at most [MAX_ART_BYTES] (64 KiB), 800 SVG nodes, depth 6
//!   ([bageo_packs::extras::check_art]), served public only from already-verified
//!   files with `X-Content-Type-Options: nosniff` (never triggers a download).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bageo_packs::extras::{
    art_file_name, check_art, parse_extras_index, ExtrasArtRef, ExtrasIndex, EXTRAS_INDEX_FILE, MAX_ART_BYTES,
    MAX_INDEX_BYTES,
};
use bageo_packs::locales::lrule;
use bageo_packs::Issue;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::calls_http::{api_error, asset_response, token_ok_parts, unauthorized, CallsState, TokenQuery};
use crate::console_i18n;
use crate::locale::Ext;
use crate::locales_store;
use crate::pack_update::{self, Fail, Source, Staged};
use crate::settings_store::RuntimeSettings;

pub(crate) const INDEX_NAME: &str = EXTRAS_INDEX_FILE;
const TOTAL_TIMEOUT: Duration = Duration::from_secs(60);
const INCOMING_PREFIX: &str = ".incoming-";
const ART_INCOMING_PREFIX: &str = ".incoming-art-";
const GONE_PREFIX: &str = ".gone-";
const ART_FILE_PREFIX: &str = "art-";
const ART_FILE_SUFFIX: &str = ".json";

fn valid_code(s: &str) -> bool {
    !s.is_empty() && s.len() <= 24 && s.split('-').all(|p| p.len() >= 2 && p.bytes().all(|b| b.is_ascii_lowercase()))
}

/// Regions whose mascot art is shipped built into `calls.html`.
pub(crate) const BUILTIN_ART_REGIONS: [&str; 9] = ["tw", "jp", "kr", "us", "de", "fr", "tr", "br", "gulf"];

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Target {
    Lang(Ext),
    Art(String),
}

impl Target {

    fn label(&self) -> String {
        match self {
            Target::Lang(e) => format!("lang:{}", e.code()),
            Target::Art(r) => format!("art:{r}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Installed,
    Removed,
    UpToDate,
    InUse,
    NotListed,
    Offline,
    BadPack,
    UpdateBageo,
    Restart,
    Io,
    Busy,
}

impl Outcome {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Outcome::Installed => "installed",
            Outcome::Removed => "removed",
            Outcome::UpToDate => "up_to_date",
            Outcome::InUse => "in_use",
            Outcome::NotListed => "not_listed",
            Outcome::Offline => "offline",
            Outcome::BadPack => "bad_pack",
            Outcome::UpdateBageo => "update_bageo",
            Outcome::Restart => "restart",
            Outcome::Io => "io",
            Outcome::Busy => "busy",
        }
    }
}

enum Stop {
    Fail(Fail),
    NotListed,
}

impl From<Fail> for Stop {
    fn from(f: Fail) -> Self {
        Stop::Fail(f)
    }
}

fn outcome_of(r: &Result<u64, Stop>) -> Outcome {
    match r {
        Ok(_) => Outcome::Installed,
        Err(Stop::NotListed) => Outcome::NotListed,
        Err(Stop::Fail(Fail::UpToDate)) => Outcome::UpToDate,
        Err(Stop::Fail(Fail::Offline(_))) => Outcome::Offline,
        Err(Stop::Fail(Fail::UpdateBageo(_))) => Outcome::UpdateBageo,
        Err(Stop::Fail(Fail::Restart)) => Outcome::Restart,
        Err(Stop::Fail(Fail::Io(_))) => Outcome::Io,
        Err(Stop::Fail(Fail::Bad(_))) => Outcome::BadPack,
    }
}

fn io_fail(msg: &str, e: std::io::Error) -> Fail {
    Fail::Io(format!("{msg}: {e}"))
}

fn temp_name(prefix: &str, suffix: &str) -> String {
    format!("{prefix}{}-{}{suffix}", std::process::id(), uuid::Uuid::new_v4().simple())
}

/// Where downloaded extra language packs, art files and index live (`<data>/packs/extras`).
pub(crate) fn extras_root() -> PathBuf {
    locales_store::extras_root()
}

/// Removes unfinished downloads or pending deletions in `root` left by an earlier crash.
pub(crate) fn clear_unfinished(root: &Path) {
    locales_store::clear_unfinished_extras(root);
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for e in entries.flatten() {
        let name = e.file_name();
        let s = name.to_string_lossy();
        if s.starts_with(INCOMING_PREFIX) {
            let _ = std::fs::remove_file(e.path());
        } else if s.starts_with(GONE_PREFIX) {
            let p = e.path();
            let _ = if p.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
        }
    }
}

pub(crate) fn kept_index(root: &Path) -> bool {
    root.join(INDEX_NAME).is_file()
}

pub(crate) fn restore_builtin_index(root: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(root.join(INDEX_NAME)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

pub(crate) fn known_index(root: &Path) -> Option<ExtrasIndex> {
    let bytes = std::fs::read(root.join(INDEX_NAME)).ok()?;
    if bytes.len() > MAX_INDEX_BYTES {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    parse_extras_index(&text).ok().map(|(ix, _)| ix)
}

fn save_index(root: &Path, text: &str, ix: &ExtrasIndex) {
    if known_index(root).is_some_and(|cur| ix.version < cur.version) {
        return;
    }
    let _ = std::fs::create_dir_all(root);
    let tmp = root.join(temp_name(INCOMING_PREFIX, ".json"));
    let done = std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, root.join(INDEX_NAME)));
    if let Err(e) = done {
        let _ = std::fs::remove_file(&tmp);
        tracing::warn!("extras index not kept: {e}");
    }
}

/// Validates and reads an installed `art-<region>.json` from `root`.
/// Returns `(size_bytes, version, json_text)` when valid.
fn read_installed_art(root: &Path, region: &str) -> Option<(u64, u64, String)> {
    if !valid_code(region) || region == "default" || BUILTIN_ART_REGIONS.contains(&region) {
        return None;
    }
    let file = art_file_name(region);
    let path = root.join(&file);
    let meta = std::fs::symlink_metadata(&path).ok()?;
    if !meta.file_type().is_file() || meta.len() == 0 || meta.len() > MAX_ART_BYTES as u64 {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    if bytes.len() > MAX_ART_BYTES || !check_art(&file, &bytes, region).is_empty() {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    let ver = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get("version").and_then(Value::as_u64))
        .unwrap_or(0);
    Some((meta.len(), ver, text))
}

fn art_version(root: &Path, region: &str) -> u64 {
    read_installed_art(root, region).map_or(0, |(_, ver, _)| ver)
}

/// Installed valid art files in `root`: region -> (bytes, version).
fn installed_art_map(root: &Path) -> BTreeMap<String, (u64, u64)> {
    let mut out = BTreeMap::new();
    let Ok(rd) = std::fs::read_dir(root) else { return out };
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(s) = name.to_str() else { continue };
        let Some(region) = s.strip_prefix("art-").and_then(|rest| rest.strip_suffix(".json")) else { continue };
        if let Some((bytes, ver, _)) = read_installed_art(root, region) {
            out.insert(region.to_string(), (bytes, ver));
        }
    }
    out
}

/// Canonical extra languages referenced anywhere in settings (`language`, `notify_language`, `sim_languages`).
fn in_use_langs(rt: &RuntimeSettings) -> BTreeSet<Ext> {
    let mut set = BTreeSet::new();
    for code in std::iter::once(rt.language.as_str())
        .chain(std::iter::once(rt.notify_language.as_str()))
        .chain(rt.sim_languages.values().map(String::as_str))
    {
        if let Some(ext) = Ext::from_code(&code.trim().to_ascii_lowercase()) {
            set.insert(ext);
        }
    }
    set
}

fn target_from_req(lang: Option<&str>, art: Option<&str>) -> Result<Option<Target>, &'static str> {
    let l = lang.map(str::trim).filter(|s| !s.is_empty());
    let a = art.map(str::trim).filter(|s| !s.is_empty());
    match (l, a) {
        (Some(code), None) => Ok(Ext::from_code(&code.to_ascii_lowercase()).map(Target::Lang)),
        (None, Some(region)) => {
            let r = region.to_ascii_lowercase();
            let valid = r != "default" && !BUILTIN_ART_REGIONS.contains(&r.as_str()) && valid_code(&r);
            Ok(valid.then_some(Target::Art(r)))
        }
        _ => Err("expected {\"lang\": \"<code>\"} or {\"art\": \"<region>\"}"),
    }
}

static RUNNING: AtomicBool = AtomicBool::new(false);

struct Running;

impl Running {
    fn take() -> Option<Self> {
        (!RUNNING.swap(true, Ordering::SeqCst)).then_some(Running)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

fn parse_index_for_download(text: &str) -> Result<ExtrasIndex, Stop> {
    match parse_extras_index(text) {
        Ok((ix, _)) => Ok(ix),
        Err(issues) => {
            if issues.iter().any(|i| i.field == "format" && i.detail.contains("newer")) {
                let msg = issues.first().map(|i| i.detail.clone()).unwrap_or_else(|| "update Bageo first".into());
                Err(Fail::UpdateBageo(msg).into())
            } else {
                let msg = issues.first().map(|i| i.to_string()).unwrap_or_else(|| "bad extras index".into());
                Err(pack_update::bad(msg).into())
            }
        }
    }
}

fn install_art_file(root: &Path, staged: &Path, region: &str, want_version: u64) -> Result<u64, Fail> {
    let file = art_file_name(region);
    let len = std::fs::metadata(staged).map_err(|e| io_fail("cannot read the art file", e))?.len();
    if len == 0 || len > MAX_ART_BYTES as u64 {
        return Err(pack_update::bad(format!("{file}: {len} bytes (limit {MAX_ART_BYTES})")));
    }
    let bytes = std::fs::read(staged).map_err(|e| io_fail("cannot read the art file", e))?;
    let issues = check_art(&file, &bytes, region);
    if !issues.is_empty() {
        if issues.iter().any(|i| i.field == "format" && i.detail.contains("newer")) {
            return Err(Fail::UpdateBageo(issues[0].detail.clone()));
        }
        return Err(pack_update::bad(issues[0].to_string()));
    }
    let val: Value = serde_json::from_slice(&bytes).map_err(|e| pack_update::bad(format!("{file}: {e}")))?;
    if val.get("version").and_then(Value::as_u64) != Some(want_version) {
        return Err(pack_update::bad(format!("{file}: version does not match index ({want_version})")));
    }
    let cur = art_version(root, region);
    if cur > 0 && want_version <= cur {
        return Err(Fail::UpToDate);
    }
    std::fs::create_dir_all(root).map_err(|e| io_fail("cannot create extras folder", e))?;
    std::fs::rename(staged, root.join(&file)).map_err(|e| io_fail("cannot install the art file", e))?;
    Ok(bytes.len() as u64)
}

fn map_lang_issues(issues: &[Issue]) -> Fail {
    if issues.iter().any(|i| i.field == "format" && i.detail.contains("newer")) {
        return Fail::UpdateBageo(issues.first().map(|i| i.detail.clone()).unwrap_or_default());
    }
    if issues.iter().any(|i| i.rule == lrule::FORMAT && i.detail.contains("restart Bageo")) {
        return Fail::Restart;
    }
    Fail::Bad(issues.first().map(|i| i.to_string()).unwrap_or_else(|| "bad language pack".into()))
}

enum StagedTarget {
    Lang { ext: Ext, version: u64, bytes: u64 },
    Art { region: String, version: u64, bytes: u64 },
}

async fn fetch_index_step(client: &reqwest::Client, src: &Source) -> Result<(String, ExtrasIndex, Vec<url::Url>), Stop> {
    let (text, hops) = pack_update::fetch_index(client, src, INDEX_NAME, MAX_INDEX_BYTES as u64).await?;
    let ix = parse_index_for_download(&text)?;
    Ok((text, ix, hops))
}

async fn download_entry(
    client: &reqwest::Client,
    src: &Source,
    hops: &[url::Url],
    ix: &ExtrasIndex,
    root: &Path,
    target: &Target,
) -> Result<(Staged, StagedTarget), Stop> {
    match target {
        Target::Lang(ext) => {
            let entry = ix.langs.iter().find(|r| r.code == ext.code()).ok_or(Stop::NotListed)?;
            if !pack_update::bageo_at_least(&entry.min_bageo) {
                return Err(Fail::UpdateBageo(format!("needs Bageo {} or newer", entry.min_bageo)).into());
            }
            let cur = locales_store::single_extra_version(root, *ext);
            if cur >= entry.version {
                return Err(Fail::UpToDate.into());
            }
            let url = src
                .release_file_url(hops, INDEX_NAME, &entry.file)
                .ok_or_else(|| pack_update::bad("the index names a path, not a file"))?;
            std::fs::create_dir_all(root).map_err(|e| io_fail("cannot make extras folder", e))?;
            let mut staged = Staged::create(locales_store::extra_incoming_path(root))?;
            pack_update::download_checked(client, src, url, entry.bytes, &entry.sha256, &mut staged).await?;
            Ok((staged, StagedTarget::Lang { ext: *ext, version: entry.version, bytes: entry.bytes }))
        }
        Target::Art(region) => {
            let entry: &ExtrasArtRef = ix.art.iter().find(|r| r.region == *region).ok_or(Stop::NotListed)?;
            if !pack_update::bageo_at_least(&entry.min_bageo) {
                return Err(Fail::UpdateBageo(format!("needs Bageo {} or newer", entry.min_bageo)).into());
            }
            let cur = art_version(root, region);
            if cur >= entry.version && cur > 0 {
                return Err(Fail::UpToDate.into());
            }
            let url = src
                .release_file_url(hops, INDEX_NAME, &entry.file)
                .ok_or_else(|| pack_update::bad("the index names a path, not a file"))?;
            std::fs::create_dir_all(root).map_err(|e| io_fail("cannot make extras folder", e))?;
            let mut staged = Staged::create(root.join(temp_name(ART_INCOMING_PREFIX, ".json")))?;
            pack_update::download_checked(client, src, url, entry.bytes, &entry.sha256, &mut staged).await?;
            Ok((
                staged,
                StagedTarget::Art { region: region.clone(), version: entry.version, bytes: entry.bytes },
            ))
        }
    }
}

fn install_staged<F>(dir: &Path, staged: Staged, meta: StagedTarget, apply_lang: F) -> Result<u64, Fail>
where
    F: FnOnce(&Path, Ext, u64) -> Result<(), Vec<Issue>>,
{
    let res = match meta {
        StagedTarget::Lang { ext, version, bytes } => apply_lang(staged.path(), ext, version).map(|_| bytes).map_err(|i| map_lang_issues(&i)),
        StagedTarget::Art { region, version, .. } => install_art_file(dir, staged.path(), &region, version),
    };
    drop(staged);
    res
}

async fn download_with<H, F>(
    client: &reqwest::Client,
    src: &Source,
    root: &Path,
    target: &Target,
    limit: Duration,
    hold: H,
    apply_lang: F,
) -> Outcome
where
    H: Send + 'static,
    F: FnOnce(&Path, Ext, u64) -> Result<(), Vec<Issue>> + Send + 'static,
{
    let res = match tokio::time::timeout(limit, async {
        let (text, ix, hops) = fetch_index_step(client, src).await?;
        let (staged, meta) = download_entry(client, src, &hops, &ix, root, target).await?;
        Ok::<_, Stop>((staged, meta, text, ix))
    })
    .await
    {
        Err(_) => Err(Fail::Offline(format!("no answer within {} ms", limit.as_millis())).into()),
        Ok(Err(stop)) => Err(stop),
        Ok(Ok((staged, meta, text, ix))) => {
            let dir = root.to_path_buf();
            let done = tokio::task::spawn_blocking(move || {
                let _hold = hold;
                let r = install_staged(&dir, staged, meta, apply_lang);
                if r.is_ok() {
                    save_index(&dir, &text, &ix);
                }
                r
            })
            .await;
            match done {
                Ok(Ok(n)) => Ok(n),
                Ok(Err(f)) => Err(f.into()),
                Err(_) => Err(Fail::Io("the install stopped".into()).into()),
            }
        }
    };
    let out = outcome_of(&res);
    let label = target.label();
    match res {
        Ok(n) => tracing::info!("extra pack {label} installed ({n} bytes)"),
        Err(Stop::Fail(Fail::UpToDate)) => tracing::info!("extra pack {label}: already up to date"),
        Err(Stop::Fail(f)) => tracing::warn!("extra pack {label}: {} ({})", out.code(), f.detail()),
        Err(Stop::NotListed) => tracing::warn!("extra pack {label}: not listed in extras index"),
    }
    out
}

pub(crate) fn remove_art(root: &Path, region: &str) -> Outcome {
    if !valid_code(region) || region == "default" || BUILTIN_ART_REGIONS.contains(&region) {
        return Outcome::NotListed;
    }
    let path = root.join(art_file_name(region));
    let Ok(meta) = std::fs::symlink_metadata(&path) else { return Outcome::NotListed };
    if !meta.file_type().is_file() {
        return Outcome::NotListed;
    }
    let gone = root.join(temp_name(GONE_PREFIX, ".json"));
    if let Err(e) = std::fs::rename(&path, &gone) {
        tracing::warn!("extra art {region}: cannot delete ({e})");
        return Outcome::Io;
    }
    let _ = std::fs::remove_file(&gone);
    tracing::info!("extra art {region} deleted");
    Outcome::Removed
}

fn remove_lang_with<F>(remove_fn: F) -> Outcome
where
    F: FnOnce() -> Result<bool, std::io::Error>,
{
    match remove_fn() {
        Ok(true) => Outcome::Removed,
        Ok(false) => Outcome::NotListed,
        Err(e) if e.to_string().contains("restart Bageo") => Outcome::Restart,
        Err(e) => {
            tracing::warn!("extra language pack: cannot delete ({e})");
            Outcome::Io
        }
    }
}

pub(crate) fn list_json_with(root: &Path, rt: &RuntimeSettings, out: Option<Outcome>) -> Value {
    let ix = known_index(root);
    let table = locales_store::current();
    let active = in_use_langs(rt);
    let installed_art = installed_art_map(root);

    let langs: Vec<Value> = table
        .langs()
        .map(|(ext, loc)| {
            let entry = ix.as_ref().and_then(|x| x.langs.iter().find(|r| r.code == ext.code()));
            json!({
                "code": ext.code(),
                "native": loc.names.native,
                "en": loc.names.en,
                "zh": loc.names.zh,
                "dir": if locales_store::writing_rtl(ext.code()) { "rtl" } else { "ltr" },
                "stage": loc.stage.code(),
                "reviewed": loc.reviewed(),
                "downloaded": loc.downloaded,
                "extra_version": loc.extra_version,
                "in_use": active.contains(&ext),
                "bytes": entry.map_or(0, |r| r.bytes),
                "version": entry.map_or(0, |r| r.version),
                "level": entry.map_or_else(|| loc.stage.code().to_string(), |r| r.level.clone()),
                "help": locales_store::help_url(ext.code()),
            })
        })
        .collect();

    let mut seen_art = BTreeSet::new();
    let mut art = Vec::new();
    if let Some(ix) = &ix {
        for r in &ix.art {
            seen_art.insert(r.region.clone());
            let have = installed_art.get(&r.region);
            art.push(json!({
                "region": r.region,
                "bytes": r.bytes,
                "version": r.version,
                "downloaded": have.is_some(),
                "installed_version": have.map_or(0, |(_, v)| *v),
            }));
        }
    }
    for (region, (bytes, ver)) in installed_art {
        if seen_art.contains(&region) {
            continue;
        }
        art.push(json!({
            "region": region,
            "bytes": bytes,
            "version": ver,
            "downloaded": true,
            "installed_version": ver,
        }));
    }

    let mut body = json!({
        "ok": true,
        "version": ix.as_ref().map_or(0, |x| x.version),
        "running": RUNNING.load(Ordering::SeqCst),
        "langs": langs,
        "art": art,
    });
    if let Some(o) = out {
        body["result"] = json!(o.code());
    }
    body
}

fn with_result(state: &CallsState, out: Outcome) -> Response {
    let rt = state.settings_store.snapshot();
    Json(list_json_with(&extras_root(), &rt, Some(out))).into_response()
}

#[derive(Deserialize)]
struct TargetRequest {
    #[serde(default)]
    lang: Option<String>,
    #[serde(default)]
    art: Option<String>,
}

/// `GET /api/extras`: all 70 extra languages + known region art and install status.
async fn list(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    let rt = state.settings_store.snapshot();
    Json(list_json_with(&extras_root(), &rt, None)).into_response()
}

/// `POST /api/extras/download {"lang": "..."}` or `{"art": "..."}`.
async fn download(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>, body: Bytes) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    if !pack_update::json_request(&headers) {
        return pack_update::json_required();
    }
    let Ok(req) = serde_json::from_slice::<TargetRequest>(&body) else {
        return api_error(StatusCode::BAD_REQUEST, "bad_request", "expected {\"lang\": \"<code>\"} or {\"art\": \"<region>\"}");
    };
    let target = match target_from_req(req.lang.as_deref(), req.art.as_deref()) {
        Ok(Some(t)) => t,
        Ok(None) => return with_result(&state, Outcome::NotListed),
        Err(msg) => return api_error(StatusCode::BAD_REQUEST, "bad_request", msg),
    };
    let Some(running) = Running::take() else {
        return with_result(&state, Outcome::Busy);
    };
    let root = extras_root();
    let task = tokio::spawn(async move {
        let client = match pack_update::client_with(TOTAL_TIMEOUT) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("extra pack download: no HTTP client ({e})");
                return Outcome::Offline;
            }
        };
        download_with(&client, &Source::official(), &root, &target, TOTAL_TIMEOUT, running, |staged, ext, ver| {
            locales_store::install_extra_lang(staged, ext, Some(ver)).map(|_| ())
        })
        .await
    });
    let out = task.await.unwrap_or(Outcome::Offline);
    if out == Outcome::Installed {
        state.settings_store.snapshot().apply_globals();
    }
    with_result(&state, out)
}

/// `POST /api/extras/delete {"lang": "..."}` or `{"art": "..."}`.
async fn delete(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>, body: Bytes) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    if !pack_update::json_request(&headers) {
        return pack_update::json_required();
    }
    let Ok(req) = serde_json::from_slice::<TargetRequest>(&body) else {
        return api_error(StatusCode::BAD_REQUEST, "bad_request", "expected {\"lang\": \"<code>\"} or {\"art\": \"<region>\"}");
    };
    let target = match target_from_req(req.lang.as_deref(), req.art.as_deref()) {
        Ok(Some(t)) => t,
        Ok(None) => return with_result(&state, Outcome::NotListed),
        Err(msg) => return api_error(StatusCode::BAD_REQUEST, "bad_request", msg),
    };
    let rt = state.settings_store.snapshot();
    if let Target::Lang(ext) = &target {
        if in_use_langs(&rt).contains(ext) {
            return with_result(&state, Outcome::InUse);
        }
    }
    let Some(_running) = Running::take() else {
        return with_result(&state, Outcome::Busy);
    };
    let root = extras_root();
    let out = match target {
        Target::Lang(ext) => remove_lang_with(|| locales_store::remove_extra_lang(ext)),
        Target::Art(region) => remove_art(&root, &region),
    };
    if out == Outcome::Removed {
        state.settings_store.snapshot().apply_globals();
    }
    with_result(&state, out)
}

// ---- automatic background selection on console login ------------------------------------------

#[derive(Default)]
struct AutoCooldowns {
    last_index_fetch: Option<Instant>,
    items: HashMap<Target, Instant>,
}

static COOLDOWNS: LazyLock<Mutex<AutoCooldowns>> = LazyLock::new(|| Mutex::new(AutoCooldowns::default()));

/// Clears the 10-minute index fetch cooldown when the user explicitly clicks "Check for updates".
pub(crate) fn clear_index_cooldown() {
    COOLDOWNS.lock().unwrap_or_else(|e| e.into_inner()).last_index_fetch = None;
}

// ---- public read-only static endpoints --------------------------------------------------------

fn serve_art_from(root: &Path, file: &str, headers: &HeaderMap) -> Response {
    let Some(region) = file.strip_prefix(ART_FILE_PREFIX).and_then(|s| s.strip_suffix(ART_FILE_SUFFIX)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some((_, _, text)) = read_installed_art(root, region) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let asset = console_i18n::asset_of(text);
    asset_response(headers, &asset, "application/json; charset=utf-8", "Accept-Encoding")
}

/// `GET /calls/extras/art/<art-<region>.json>`: serves an already-downloaded and
/// verified mascot art file (public like `/calls/i18n/`, never triggers download).
async fn serve_art(UrlPath(file): UrlPath<String>, headers: HeaderMap) -> Response {
    serve_art_from(&extras_root(), &file, &headers)
}

/// `GET /calls/i18n/desktop/<code>.json`: one extra language's desktop strings
/// (from its installed `lang-<code>.json.gz`), falling back per key to English.
async fn i18n_desktop_file(UrlPath(file): UrlPath<String>, headers: HeaderMap) -> Response {
    let Some(code) = file.strip_suffix(".json") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(body) = locales_store::desktop_file(code) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let asset = console_i18n::asset_of(body);
    asset_response(&headers, &asset, "application/json; charset=utf-8", "Accept-Encoding")
}

pub fn router() -> Router<CallsState> {
    Router::new()
        .route("/api/extras", get(list))
        .route("/api/extras/download", post(download))
        .route("/api/extras/delete", post(delete))
        .route("/extras/art/{file}", get(serve_art))
        .route("/i18n/desktop/{file}", get(i18n_desktop_file))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bageo_packs::extras::lang_file_name;
    use crate::pack_update::tests::{tmp, Reply, Server};
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use sha2::{Digest, Sha256};
    use std::io::Write as _;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    const LATEST: &str = "/h4lo-void/bageo-packs/releases/latest/download/bageo-extras-index.json";
    const TAGGED: &str = "/h4lo-void/bageo-packs/releases/download/v9/";

    fn sha256(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn gz(val: &Value) -> Vec<u8> {
        let mut enc = GzEncoder::new(Vec::new(), Compression::best());
        enc.write_all(&serde_json::to_vec(val).unwrap()).unwrap();
        enc.finish().unwrap()
    }

    fn sample_lang_gz(code: &str, version: u64, with_console: bool, with_desktop: bool) -> Vec<u8> {
        let info = Ext::from_code(code).unwrap();
        let (native, en_name, zh_name, sample) = match code {
            "th" => ("ไทย", "Thai", "泰文", "ข้อความ"),
            "it" => ("Italiano", "Italian", "義大利文", "Prova"),
            _ => ("Tiếng Việt", "Vietnamese", "越南文", "Thử nghiệm"),
        };
        let mut root = json!({
            "format": 1,
            "code": code,
            "version": version,
            "built_at": "2026-11-21T08:00:00Z",
            "source_commit": "abc123",
            "names": {
                "code": code,
                "official": info.official(),
                "native": native,
                "en": en_name,
                "zh": zh_name,
            },
            "status": if with_console { "local_reviewed" } else { "ai_checked" },
            "notify": crate::locales_store::testing::notify(sample, &[]),
        });
        if with_console {
            root["console"] = crate::locales_store::testing::console(sample);
        }
        if with_desktop {
            let en: Value = serde_json::from_str(include_str!("../../../desktop/ui/i18n.json")).unwrap();
            let mut strings = serde_json::Map::new();
            for (k, v) in en["en"].as_object().unwrap() {
                strings.insert(k.clone(), json!(format!("{sample} {}", v.as_str().unwrap())));
            }
            root["desktop"] = json!({ "strings": strings });
        }
        gz(&root)
    }

    fn sample_art_json(region: &str, version: u64) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "format": 1,
            "region": region,
            "version": version,
            "look": {
                "bow": false,
                "print": "lotus",
                "pal": {
                    "o1": "#2a5d4a", "o1d": "#1f4738", "o2": "#d4af37",
                    "o3": "#e8c456", "o3l": "#f5d97e", "o4": "#c43e3e",
                    "o5": "#f0ece1", "o5l": "#fffdf7", "tie": "#a32929"
                },
                "scenes": [["day", ["hat"]]]
            },
            "draw": {
                "print": [["circle", { "cx": "50", "cy": "50", "r": "10", "fill": "#ffffff" }]],
                "item": [["rect", { "x": "10", "y": "10", "width": "12", "height": "8", "fill": "var(--bts-o2)" }]],
                "props": {
                    "hat": [["path", { "d": "M 10 20 L 30 20 L 20 8 Z", "fill": "#d4af37" }]]
                },
                "acc": [{ "l": [1, 2], "nodes": [["path", { "d": "M 0 0 L 5 5", "stroke": "#2a5d4a" }]] }]
            }
        }))
        .unwrap()
    }

    fn index_for(version: u64, langs: &[(&str, u64, &[u8], &str)], art: &[(&str, u64, &[u8])]) -> String {
        let lang_entries: Vec<Value> = langs
            .iter()
            .map(|(code, ver, bytes, level)| {
                json!({
                    "code": code,
                    "file": lang_file_name(code),
                    "bytes": bytes.len(),
                    "sha256": sha256(bytes),
                    "version": ver,
                    "min_bageo": "1.0.0",
                    "status": "ai_checked",
                    "level": level,
                })
            })
            .collect();
        let art_entries: Vec<Value> = art
            .iter()
            .map(|(region, ver, bytes)| {
                json!({
                    "region": region,
                    "file": art_file_name(region),
                    "bytes": bytes.len(),
                    "sha256": sha256(bytes),
                    "version": ver,
                    "min_bageo": "1.0.0",
                })
            })
            .collect();
        json!({
            "format": bageo_packs::extras::EXTRAS_FORMAT,
            "version": version,
            "langs": lang_entries,
            "art": art_entries,
        })
        .to_string()
    }

    fn release(index: String, files: Vec<(String, Vec<u8>)>) -> Server {
        Server::start(move |path| {
            if path == LATEST {
                Reply::Redirect(format!("{TAGGED}{INDEX_NAME}"))
            } else if path == format!("{TAGGED}{INDEX_NAME}") {
                Reply::Body(index.clone().into_bytes())
            } else if let Some((_, b)) = files.iter().find(|(f, _)| path == format!("{TAGGED}{f}")) {
                Reply::Body(b.clone())
            } else {
                Reply::NotFound
            }
        })
    }

    #[tokio::test]
    async fn lang_and_art_download_install_serve_and_remove_cleanly() {
        let d = tmp();
        let count = Arc::new(AtomicUsize::new(0));
        let packs_dir = d.0.join("packs");
        let root = locales_store::extras_dir(&packs_dir);
        let th_gz = sample_lang_gz("th", 2026112101, true, true);
        let vn_art = sample_art_json("vn", 2026112101);
        let ix = index_for(
            2026112101,
            &[("th", 2026112101, &th_gz, "console")],
            &[("vn", 2026112101, &vn_art)],
        );
        let srv = release(
            ix,
            vec![
                (lang_file_name("th"), th_gz.clone()),
                (art_file_name("vn"), vn_art.clone()),
            ],
        );

        let c = count.clone();
        let pd = packs_dir.clone();
        let client = pack_update::client_with(TOTAL_TIMEOUT).unwrap();
        let out = download_with(&client, &srv.source(), &root, &Target::Lang(Ext::from_code("th").unwrap()), TOTAL_TIMEOUT, (), move |staged, ext, ver| {
            locales_store::install_extra_lang_counted(&c, &pd, staged, ext, Some(ver)).map(|_| ())
        })
        .await;
        assert_eq!(out, Outcome::Installed);
        assert_eq!(locales_store::single_extra_version(&root, Ext::from_code("th").unwrap()), 2026112101);
        assert!(kept_index(&root));

        // Same version again -> UpToDate, index not refetched excessively.
        let c = count.clone();
        let pd = packs_dir.clone();
        let out2 = download_with(&client, &srv.source(), &root, &Target::Lang(Ext::from_code("th").unwrap()), TOTAL_TIMEOUT, (), move |staged, ext, ver| {
            locales_store::install_extra_lang_counted(&c, &pd, staged, ext, Some(ver)).map(|_| ())
        })
        .await;
        assert_eq!(out2, Outcome::UpToDate);

        // Download & serve art-vn.json.
        let out_art = download_with(&client, &srv.source(), &root, &Target::Art("vn".into()), TOTAL_TIMEOUT, (), |_, _, _| Ok(())).await;
        assert_eq!(out_art, Outcome::Installed);
        let resp = serve_art_from(&root, "art-vn.json", &HeaderMap::new());
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["x-content-type-options"], "nosniff");
        assert_eq!(resp.headers()[axum::http::header::CACHE_CONTROL], "no-cache");
        let etag = resp.headers()[axum::http::header::ETAG].clone();
        let mut inm = HeaderMap::new();
        inm.insert(axum::http::header::IF_NONE_MATCH, etag);
        assert_eq!(serve_art_from(&root, "art-vn.json", &inm).status(), StatusCode::NOT_MODIFIED);

        // Built-in or missing art is never served.
        assert_eq!(serve_art_from(&root, "art-tw.json", &HeaderMap::new()).status(), StatusCode::NOT_FOUND);
        assert_eq!(serve_art_from(&root, "art-th.json", &HeaderMap::new()).status(), StatusCode::NOT_FOUND);

        // Restore clears the kept index; remove_art & remove_lang clean up files.
        restore_builtin_index(&root).unwrap();
        assert!(!kept_index(&root));
        assert_eq!(remove_art(&root, "vn"), Outcome::Removed);
        assert_eq!(remove_art(&root, "vn"), Outcome::NotListed);
        assert_eq!(serve_art_from(&root, "art-vn.json", &HeaderMap::new()).status(), StatusCode::NOT_FOUND);
        // Removing the only pack goes back to the built-in table: nothing new kept.
        let before = count.load(Ordering::SeqCst);
        let removed = remove_lang_with(|| {
            locales_store::remove_extra_lang_counted(&count, &packs_dir, Ext::from_code("th").unwrap()).map(|(_, r)| r)
        });
        assert_eq!(removed, Outcome::Removed);
        assert_eq!(count.load(Ordering::SeqCst), before);
    }

    #[tokio::test]
    async fn security_checks_refuse_tampered_oversized_and_bad_format_extras() {
        let d = tmp();
        let packs_dir = d.0.join("packs");
        let root = locales_store::extras_dir(&packs_dir);
        let client = pack_update::client_with(TOTAL_TIMEOUT).unwrap();
        let th_gz = sample_lang_gz("th", 2026112101, false, false);

        // SHA mismatch -> BadPack, nothing left behind.
        let mut tampered = th_gz.clone();
        tampered[10] ^= 0x55;
        let ix = index_for(2026112101, &[("th", 2026112101, &th_gz, "notify")], &[]);
        let srv = release(ix, vec![(lang_file_name("th"), tampered)]);
        let c = Arc::new(AtomicUsize::new(0));
        let pd = packs_dir.clone();
        let out = download_with(&client, &srv.source(), &root, &Target::Lang(Ext::from_code("th").unwrap()), TOTAL_TIMEOUT, (), move |staged, ext, ver| {
            locales_store::install_extra_lang_counted(&c, &pd, staged, ext, Some(ver)).map(|_| ())
        })
        .await;
        assert_eq!(out, Outcome::BadPack);
        assert!(!kept_index(&root), "index must not be saved when install fails");
        assert_eq!(locales_store::single_extra_version(&root, Ext::from_code("th").unwrap()), 0);

        // Format newer -> UpdateBageo.
        let newer_ix = json!({ "format": 99, "version": 2026112101u64, "langs": [], "art": [] }).to_string();
        let srv2 = release(newer_ix, Vec::new());
        let c2 = Arc::new(AtomicUsize::new(0));
        let pd2 = packs_dir.clone();
        let out2 = download_with(&client, &srv2.source(), &root, &Target::Lang(Ext::from_code("th").unwrap()), TOTAL_TIMEOUT, (), move |staged, ext, ver| {
            locales_store::install_extra_lang_counted(&c2, &pd2, staged, ext, Some(ver)).map(|_| ())
        })
        .await;
        assert_eq!(out2, Outcome::UpdateBageo);

        // Uncompressed > 600 KiB zip bomb -> BadPack.
        let big_str = "ก".repeat(650_000);
        let bomb_gz = gz(&json!({
            "format": 1,
            "code": "th",
            "version": 2026112102u64,
            "built_at": "2026-11-21T08:00:00Z",
            "source_commit": "abc",
            "names": { "native": "ไทย", "en": "Thai", "zh": "泰文" },
            "status": "ai_checked",
            "notify": { "strings": { "n_test_title": big_str }, "lists": {}, "formats": {} },
        }));
        let bomb_ix = index_for(2026112102, &[("th", 2026112102, &bomb_gz, "notify")], &[]);
        let srv3 = release(bomb_ix, vec![(lang_file_name("th"), bomb_gz)]);
        let c3 = Arc::new(AtomicUsize::new(0));
        let pd3 = packs_dir.clone();
        let out3 = download_with(&client, &srv3.source(), &root, &Target::Lang(Ext::from_code("th").unwrap()), TOTAL_TIMEOUT, (), move |staged, ext, ver| {
            locales_store::install_extra_lang_counted(&c3, &pd3, staged, ext, Some(ver)).map(|_| ())
        })
        .await;
        assert_eq!(out3, Outcome::BadPack);

        // Script injection inside art SVG -> rejected by check_art and never served.
        let bad_art = serde_json::to_vec(&json!({
            "format": 1,
            "region": "vn",
            "version": 2026112101u64,
            "look": { "bow": false, "print": "lotus", "pal": { "o1": "#2a5d4a" }, "scenes": [["day", []]] },
            "draw": { "print": [["script", {}, []]] }
        }))
        .unwrap();
        let bad_ix = index_for(2026112101, &[], &[("vn", 2026112101, &bad_art)]);
        let srv4 = release(bad_ix, vec![(art_file_name("vn"), bad_art)]);
        let out4 = download_with(&client, &srv4.source(), &root, &Target::Art("vn".into()), TOTAL_TIMEOUT, (), |_, _, _| Ok(())).await;
        assert_eq!(out4, Outcome::BadPack);

        // Redirect to an unapproved host -> BadPack.
        let evil_ix = index_for(2026112101, &[("th", 2026112101, &th_gz, "notify")], &[]);
        let srv5 = Server::start(move |path| {
            if path == LATEST {
                Reply::Body(evil_ix.clone().into_bytes())
            } else if path.ends_with(&lang_file_name("th")) {
                Reply::Redirect("https://evil.example.com/lang-th.json.gz".into())
            } else {
                Reply::NotFound
            }
        });
        let c5 = Arc::new(AtomicUsize::new(0));
        let pd5 = packs_dir.clone();
        let out5 = download_with(&client, &srv5.source(), &root, &Target::Lang(Ext::from_code("th").unwrap()), TOTAL_TIMEOUT, (), move |staged, ext, ver| {
            locales_store::install_extra_lang_counted(&c5, &pd5, staged, ext, Some(ver)).map(|_| ())
        })
        .await;
        assert_eq!(out5, Outcome::BadPack);
    }

    #[test]
    fn stage_progresses_from_call_to_console_on_extra_install_and_back_on_remove() {
        let d = tmp();
        let packs = d.0.join("packs");
        let th = Ext::from_code("th").unwrap();
        let count = AtomicUsize::new(0);
        assert_eq!(locales_store::builtin().stage(th), locales_store::Stage::Call);

        let staged = packs.join(".tmp-th.json.gz");
        std::fs::create_dir_all(&packs).unwrap();
        std::fs::write(&staged, sample_lang_gz("th", 2026112101, true, true)).unwrap();
        let tbl = locales_store::install_extra_lang_counted(&count, &packs, &staged, th, Some(2026112101)).unwrap();
        assert_eq!(tbl.stage(th), locales_store::Stage::Console);
        assert!(tbl.get(th).unwrap().downloaded);

        let (tbl2, removed) = locales_store::remove_extra_lang_counted(&count, &packs, th).unwrap();
        assert!(removed);
        assert_eq!(tbl2.stage(th), locales_store::Stage::Call);
    }

    #[tokio::test]
    async fn routes_require_auth_json_and_refuse_deleting_active_language() {
        let d = tmp();
        let mut state = crate::calls_http::tests::quick_state(&d.0);
        state.settings.calls_ui_token = "extras-secret".into();
        let _ = state.settings_store.apply_patch(&json!({ "language": "th" })).unwrap();

        // 401 without token.
        let r = list(State(state.clone()), HeaderMap::new(), Query(TokenQuery { token: None })).await;
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

        // 400 without application/json Content-Type.
        let r = delete(
            State(state.clone()),
            HeaderMap::new(),
            Query(TokenQuery { token: Some("extras-secret".into()) }),
            Bytes::from_static(br#"{"lang":"th"}"#),
        )
        .await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);

        // Deleting an active language is refused with result: "in_use".
        let mut h = HeaderMap::new();
        h.insert(axum::http::header::CONTENT_TYPE, "application/json".parse().unwrap());
        let r = delete(
            State(state.clone()),
            h.clone(),
            Query(TokenQuery { token: Some("extras-secret".into()) }),
            Bytes::from_static(br#"{"lang":"th"}"#),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        let body: Value = serde_json::from_str(&crate::calls_http::tests::body_text(r).await).unwrap();
        assert_eq!(body["result"], "in_use");

        // Core language or bad code returns not_listed.
        let r2 = download(
            State(state.clone()),
            h,
            Query(TokenQuery { token: Some("extras-secret".into()) }),
            Bytes::from_static(br#"{"lang":"en"}"#),
        )
        .await;
        assert_eq!(r2.status(), StatusCode::OK);
        let body2: Value = serde_json::from_str(&crate::calls_http::tests::body_text(r2).await).unwrap();
        assert_eq!(body2["result"], "not_listed");
    }

    #[test]
    fn install_cap_returns_restart_on_extra_lang() {
        let count = AtomicUsize::new(locales_store::MAX_NEW_BYTES);
        let res = locales_store::install_extra_lang_counted(&count, Path::new("/nonexistent"), Path::new("/nonexistent"), Ext::from_code("th").unwrap(), Some(1));
        let Err(issues) = res else { panic!("expected install cap error"); };
        assert_eq!(map_lang_issues(&issues).outcome_for_test(), Outcome::Restart);
    }

    impl Fail {
        fn outcome_for_test(&self) -> Outcome {
            match self {
                Fail::Restart => Outcome::Restart,
                Fail::UpdateBageo(_) => Outcome::UpdateBageo,
                Fail::UpToDate => Outcome::UpToDate,
                Fail::Offline(_) => Outcome::Offline,
                Fail::Io(_) => Outcome::Io,
                Fail::Bad(_) => Outcome::BadPack,
            }
        }
    }
}
