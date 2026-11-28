// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! On-demand extra language packs (`lang-<code>.json.gz`, carrying
//! notification, console and desktop widget text outside the 9 built-in core
//! languages, plus the mascot drawings of the regions whose main language it
//! is) and the region drawings the console shows.
//!
//! Listed in `bageo-extras-index.json` inside the public `bageo-packs`
//! releases on GitHub, downloaded through [crate::pack_update] with the same
//! fixed host allowlist, redirect checks, SHA-256 verification and size caps:
//! - Index: at most [MAX_INDEX_BYTES] (256 KiB), format <= [EXTRAS_FORMAT],
//!   used only with its signature ([crate::pack_update::fetch_index]).
//! - Language pack: at most [MAX_EXTRA_LANG_BYTES] (600 KiB) both compressed
//!   and unpacked ([crate::locales_store::install_extra_lang]); installs and
//!   deletions share the translations' per-run memory cap
//!   ([crate::locales_store::MAX_NEW_BYTES]).
//! - Region drawings: each at most 64 KiB, 800 SVG nodes, depth 6
//!   ([bageo_packs::extras::check_art]), checked one by one when a pack is
//!   read; a bad one is left out and the language still installs. Deleting a
//!   language takes its drawings with it. Regions whose main language is a
//!   core language have no pack: their drawings are built in
//!   ([bageo_packs::extras::ART_BUNDLE_FILE]). Both are served public at
//!   `/calls/extras/art/` with `X-Content-Type-Options: nosniff`; serving
//!   never triggers a download. Bageo no longer downloads drawings one by one.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bageo_packs::extras::{parse_art_bundle, parse_extras_index, ExtrasIndex, EXTRAS_INDEX_FILE, MAX_INDEX_BYTES};
use bageo_packs::locales::lrule;
use bageo_packs::Issue;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::calls_http::{api_error, asset_response, token_ok_parts, unauthorized, CallsState, TokenQuery};
use crate::console_i18n;
use crate::locale::Ext;
use crate::locales_store::{self, LocaleTable};
use crate::pack_update::{self, Fail, Source, Staged};
use crate::settings_store::RuntimeSettings;

pub(crate) const INDEX_NAME: &str = EXTRAS_INDEX_FILE;
const TOTAL_TIMEOUT: Duration = Duration::from_secs(60);
const INCOMING_PREFIX: &str = ".incoming-";
const GONE_PREFIX: &str = ".gone-";
const ART_FILE_PREFIX: &str = "art-";
const ART_FILE_SUFFIX: &str = ".json";
const BAD_REQUEST: &str = "expected {\"lang\": \"<code>\"}";

fn valid_code(s: &str) -> bool {
    !s.is_empty() && s.len() <= 24 && s.split('-').all(|p| p.len() >= 2 && p.bytes().all(|b| b.is_ascii_lowercase()))
}

/// Regions whose mascot art is drawn into `calls.html` itself.
pub(crate) const PAGE_ART_REGIONS: [&str; 1] = ["tw"];

/// The drawings of the other built-in regions and of the regions whose main
/// language is a core language (no language pack carries them), built by
/// `packs build-art` from bageo-packs.
const BUILTIN_ART: &[u8] = include_bytes!("../data/packs/bageo-art.json.gz");

/// The built-in drawings that passed, as JSON text by region; read once.
fn builtin_art() -> &'static std::collections::BTreeMap<String, String> {
    static ART: OnceLock<std::collections::BTreeMap<String, String>> = OnceLock::new();
    ART.get_or_init(|| match parse_art_bundle(BUILTIN_ART) {
        Ok((bundle, issues)) => {
            if let Some(first) = issues.first() {
                tracing::warn!("built-in region drawings: {} left out (first: {first})", issues.len());
            }
            bundle.art.into_iter().map(|(region, v)| (region, v.to_string())).collect()
        }
        Err(issues) => {
            tracing::warn!("built-in region drawings refused ({})", issues.first().map(|i| i.to_string()).unwrap_or_default());
            std::collections::BTreeMap::new()
        }
    })
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

/// Where downloaded extra language packs and the index live (`<data>/packs/extras`).
pub(crate) fn extras_root() -> PathBuf {
    locales_store::extras_root()
}

/// Removes unfinished downloads or pending deletions in `root` left by an
/// earlier crash, and the `art-<region>.json` files earlier versions
/// downloaded one by one (drawings now come with the language packs).
pub(crate) fn clear_unfinished(root: &Path) {
    locales_store::clear_unfinished_extras(root);
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for e in entries.flatten() {
        let name = e.file_name();
        let s = name.to_string_lossy();
        if s.starts_with(INCOMING_PREFIX) || (s.starts_with(ART_FILE_PREFIX) && s.ends_with(ART_FILE_SUFFIX)) {
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

/// Canonical extra languages referenced anywhere in settings (`language`, `notify_language`, `sim_languages`).
fn in_use_langs(rt: &RuntimeSettings) -> BTreeSet<Ext> {
    let mut set = BTreeSet::new();
    for code in std::iter::once(rt.language.as_str())
        .chain(std::iter::once(rt.notify_language.as_str()))
        .chain(rt.sim_languages.values().flat_map(crate::settings_store::SimLang::codes))
    {
        if let Some(ext) = Ext::from_code(&code.trim().to_ascii_lowercase()) {
            set.insert(ext);
        }
    }
    set
}

/// The extra language a request names; None for a core or unknown code.
fn lang_from_req(lang: Option<&str>) -> Result<Option<Ext>, &'static str> {
    match lang.map(str::trim).filter(|s| !s.is_empty()) {
        Some(code) => Ok(Ext::from_code(&code.to_ascii_lowercase())),
        None => Err(BAD_REQUEST),
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

fn map_lang_issues(issues: &[Issue]) -> Fail {
    if issues.iter().any(|i| i.field == "format" && i.detail.contains("newer")) {
        return Fail::UpdateBageo(issues.first().map(|i| i.detail.clone()).unwrap_or_default());
    }
    if issues.iter().any(|i| i.rule == lrule::FORMAT && i.detail.contains("restart Bageo")) {
        return Fail::Restart;
    }
    Fail::Bad(issues.first().map(|i| i.to_string()).unwrap_or_else(|| "bad language pack".into()))
}

async fn fetch_index_step(client: &reqwest::Client, src: &Source) -> Result<(String, ExtrasIndex, Vec<url::Url>), Stop> {
    let (text, hops) = pack_update::fetch_index(client, src, INDEX_NAME, MAX_INDEX_BYTES as u64).await?;
    let ix = parse_index_for_download(&text)?;
    Ok((text, ix, hops))
}

/// Downloads one language pack the index lists into a staged file: (file, version, bytes).
async fn download_entry(
    client: &reqwest::Client,
    src: &Source,
    hops: &[url::Url],
    ix: &ExtrasIndex,
    root: &Path,
    ext: Ext,
) -> Result<(Staged, u64, u64), Stop> {
    let entry = ix.langs.iter().find(|r| r.code == ext.code()).ok_or(Stop::NotListed)?;
    if !pack_update::bageo_at_least(&entry.min_bageo) {
        return Err(Fail::UpdateBageo(format!("needs Bageo {} or newer", entry.min_bageo)).into());
    }
    let cur = locales_store::single_extra_version(root, ext);
    if cur >= entry.version {
        return Err(Fail::UpToDate.into());
    }
    let url = src
        .release_file_url(hops, INDEX_NAME, &entry.file)
        .ok_or_else(|| pack_update::bad("the index names a path, not a file"))?;
    std::fs::create_dir_all(root).map_err(|e| io_fail("cannot make extras folder", e))?;
    let mut staged = Staged::create(locales_store::extra_incoming_path(root))?;
    pack_update::download_checked(client, src, url, entry.bytes, &entry.sha256, &mut staged).await?;
    Ok((staged, entry.version, entry.bytes))
}

async fn download_with<H, F>(
    client: &reqwest::Client,
    src: &Source,
    root: &Path,
    ext: Ext,
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
        let (staged, version, bytes) = download_entry(client, src, &hops, &ix, root, ext).await?;
        Ok::<_, Stop>((staged, version, bytes, text, ix))
    })
    .await
    {
        Err(_) => Err(Fail::Offline(format!("no answer within {} ms", limit.as_millis())).into()),
        Ok(Err(stop)) => Err(stop),
        Ok(Ok((staged, version, bytes, text, ix))) => {
            let dir = root.to_path_buf();
            let done = tokio::task::spawn_blocking(move || {
                let _hold = hold;
                let r = apply_lang(staged.path(), ext, version).map(|_| bytes).map_err(|i| map_lang_issues(&i));
                drop(staged);
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
    let label = ext.code();
    match res {
        Ok(n) => tracing::info!("extra pack lang:{label} installed ({n} bytes)"),
        Err(Stop::Fail(Fail::UpToDate)) => tracing::info!("extra pack lang:{label}: already up to date"),
        Err(Stop::Fail(f)) => tracing::warn!("extra pack lang:{label}: {} ({})", out.code(), f.detail()),
        Err(Stop::NotListed) => tracing::warn!("extra pack lang:{label}: not listed in extras index"),
    }
    out
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

    let mut body = json!({
        "ok": true,
        "version": ix.as_ref().map_or(0, |x| x.version),
        "running": RUNNING.load(Ordering::SeqCst),
        "langs": langs,
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
struct LangRequest {
    #[serde(default)]
    lang: Option<String>,
}

/// `GET /api/extras`: all 70 extra languages and their install status.
async fn list(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    let rt = state.settings_store.snapshot();
    Json(list_json_with(&extras_root(), &rt, None)).into_response()
}

/// `POST /api/extras/download {"lang": "..."}`: the language pack, with the drawings of its regions.
async fn download(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>, body: Bytes) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    if !pack_update::json_request(&headers) {
        return pack_update::json_required();
    }
    let Ok(req) = serde_json::from_slice::<LangRequest>(&body) else {
        return api_error(StatusCode::BAD_REQUEST, "bad_request", BAD_REQUEST);
    };
    let ext = match lang_from_req(req.lang.as_deref()) {
        Ok(Some(e)) => e,
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
        download_with(&client, &Source::official(), &root, ext, TOTAL_TIMEOUT, running, |staged, ext, ver| {
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

/// `POST /api/extras/delete {"lang": "..."}`: the language pack and the drawings it brought.
async fn delete(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>, body: Bytes) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    if !pack_update::json_request(&headers) {
        return pack_update::json_required();
    }
    let Ok(req) = serde_json::from_slice::<LangRequest>(&body) else {
        return api_error(StatusCode::BAD_REQUEST, "bad_request", BAD_REQUEST);
    };
    let ext = match lang_from_req(req.lang.as_deref()) {
        Ok(Some(e)) => e,
        Ok(None) => return with_result(&state, Outcome::NotListed),
        Err(msg) => return api_error(StatusCode::BAD_REQUEST, "bad_request", msg),
    };
    let rt = state.settings_store.snapshot();
    if in_use_langs(&rt).contains(&ext) {
        return with_result(&state, Outcome::InUse);
    }
    let Some(_running) = Running::take() else {
        return with_result(&state, Outcome::Busy);
    };
    let out = remove_lang_with(|| locales_store::remove_extra_lang(ext));
    if out == Outcome::Removed {
        state.settings_store.snapshot().apply_globals();
    }
    with_result(&state, out)
}

// ---- automatic background selection on console login ------------------------------------------

#[derive(Default)]
struct AutoCooldowns {
    last_index_fetch: Option<Instant>,
    items: HashMap<Ext, Instant>,
}

static COOLDOWNS: LazyLock<Mutex<AutoCooldowns>> = LazyLock::new(|| Mutex::new(AutoCooldowns::default()));

/// Clears the 10-minute index fetch cooldown when the user explicitly clicks "Check for updates".
pub(crate) fn clear_index_cooldown() {
    COOLDOWNS.lock().unwrap_or_else(|e| e.into_inner()).last_index_fetch = None;
}

// ---- public read-only static endpoints --------------------------------------------------------

/// A region's drawing: built in, else from a downloaded language pack of
/// table. The region drawn into the page itself and `default` have none here.
fn art_text_in(table: &LocaleTable, region: &str) -> Option<String> {
    if !valid_code(region) || region == "default" || PAGE_ART_REGIONS.contains(&region) {
        return None;
    }
    builtin_art().get(region).cloned().or_else(|| locales_store::region_art_in(table, region))
}

fn serve_art_in(table: &LocaleTable, file: &str, headers: &HeaderMap) -> Response {
    let Some(region) = file.strip_prefix(ART_FILE_PREFIX).and_then(|s| s.strip_suffix(ART_FILE_SUFFIX)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(text) = art_text_in(table, region) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let asset = console_i18n::asset_of(text);
    asset_response(headers, &asset, "application/json; charset=utf-8", "Accept-Encoding")
}

/// `GET /calls/extras/art/<art-<region>.json>`: a built-in drawing or one a
/// downloaded language pack brought (public like `/calls/i18n/`, never
/// triggers a download).
async fn serve_art(UrlPath(file): UrlPath<String>, headers: HeaderMap) -> Response {
    serve_art_in(locales_store::current(), &file, &headers)
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
    use bageo_packs::extras::{check_art, lang_file_name, BUILTIN_ART_REGIONS};
    use crate::pack_update::tests::{test_key, tmp, Reply, Server, TestKey};
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use sha2::{Digest, Sha256};
    use std::io::Write as _;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    const LATEST: &str = "/h4lo-void/bageo-packs/releases/latest/download/bageo-extras-index.json";
    const TAGGED: &str = "/h4lo-void/bageo-packs/releases/download/v9/";

    /// The regions whose main language is a core language (bageo-packs regions/*.json).
    const CORE_LANG_REGIONS: [&str; 33] = [
        "ao-mz-gw-cv-st", "ar-uy", "at", "au-nz", "be", "bt", "ca", "cd-cg-cm-ga-cf-td-bi", "ch", "cn", "es", "gb-ie", "gq", "hk-mo", "in",
        "ke-tz-et-mg-ht", "kh", "latam", "lu", "mt", "mu-sc-km", "mv", "mx", "ng-gh-sl-lr-gm", "pg-fj-sb-vu", "ph", "pt", "sg",
        "sn-ci-bf-ml-ne-tg-bj-gn", "tl", "ws-to-ki-fm-mh-pw-nr-tv", "za", "zm-zw-mw",
    ];

    fn sha256(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn gz(val: &Value) -> Vec<u8> {
        let mut enc = GzEncoder::new(Vec::new(), Compression::best());
        enc.write_all(&serde_json::to_vec(val).unwrap()).unwrap();
        enc.finish().unwrap()
    }

    fn sample_lang(code: &str, version: u64, with_console: bool, with_desktop: bool) -> Value {
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
        root
    }

    fn sample_lang_gz(code: &str, version: u64, with_console: bool, with_desktop: bool) -> Vec<u8> {
        gz(&sample_lang(code, version, with_console, with_desktop))
    }

    fn sample_art(region: &str, version: u64) -> Value {
        json!({
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
        })
    }

    fn index_for(version: u64, langs: &[(&str, u64, &[u8], &str)]) -> String {
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
        json!({
            "format": bageo_packs::extras::EXTRAS_FORMAT,
            "version": version,
            "langs": lang_entries,
            "art": [],
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

    type Slot = Arc<Mutex<Option<&'static LocaleTable>>>;

    /// Downloads and installs one language the way the route does, keeping the table it built.
    async fn get_lang(srv: &Server, packs_dir: &Path, count: &Arc<AtomicUsize>, code: &str) -> (Outcome, Option<&'static LocaleTable>) {
        let root = locales_store::extras_dir(packs_dir);
        let slot: Slot = Arc::new(Mutex::new(None));
        let (c, pd, s) = (count.clone(), packs_dir.to_path_buf(), slot.clone());
        let client = pack_update::client_with(TOTAL_TIMEOUT).unwrap();
        let out = download_with(&client, &srv.source(), &root, Ext::from_code(code).unwrap(), TOTAL_TIMEOUT, (), move |staged, ext, ver| {
            let t = locales_store::install_extra_lang_counted(&c, &pd, staged, ext, Some(ver))?;
            *s.lock().unwrap() = Some(t);
            Ok(())
        })
        .await;
        let t = *slot.lock().unwrap();
        (out, t)
    }

    #[tokio::test]
    async fn a_language_brings_its_drawings_and_takes_them_when_deleted() {
        let d = tmp();
        let count = Arc::new(AtomicUsize::new(0));
        let packs_dir = d.0.join("packs");
        let root = locales_store::extras_dir(&packs_dir);
        let mut bad = sample_art("mm", 2026112101);
        bad["draw"]["print"] = json!([["script", {}]]);
        let mut th = sample_lang("th", 2026112101, true, true);
        th["art"] = json!({ "th": sample_art("th", 2026112101), "la": sample_art("la", 2026112101), "mm": bad });
        let th_gz = gz(&th);
        let srv = release(index_for(2026112101, &[("th", 2026112101, &th_gz, "console")]), vec![(lang_file_name("th"), th_gz.clone())]);

        let (out, table) = get_lang(&srv, &packs_dir, &count, "th").await;
        assert_eq!(out, Outcome::Installed);
        let table = table.unwrap();
        let thai = Ext::from_code("th").unwrap();
        assert_eq!(table.stage(thai), locales_store::Stage::Console, "the language installs even with a broken drawing");
        assert_eq!(locales_store::single_extra_version(&root, thai), 2026112101);
        assert!(kept_index(&root));

        // Its drawings are served; the broken one was left out alone.
        let resp = serve_art_in(table, "art-th.json", &HeaderMap::new());
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["x-content-type-options"], "nosniff");
        assert_eq!(resp.headers()[axum::http::header::CACHE_CONTROL], "no-cache");
        let etag = resp.headers()[axum::http::header::ETAG].clone();
        let mut inm = HeaderMap::new();
        inm.insert(axum::http::header::IF_NONE_MATCH, etag);
        assert_eq!(serve_art_in(table, "art-th.json", &inm).status(), StatusCode::NOT_MODIFIED);
        assert_eq!(serve_art_in(table, "art-la.json", &HeaderMap::new()).status(), StatusCode::OK);
        assert_eq!(serve_art_in(table, "art-mm.json", &HeaderMap::new()).status(), StatusCode::NOT_FOUND);
        let body: Value = serde_json::from_str(&art_text_in(table, "la").unwrap()).unwrap();
        assert_eq!(body["region"], "la");
        // Regions drawn into the page, default and odd names are never served here.
        for file in ["art-tw.json", "art-default.json", "art-TH.json", "th.json", "art-../x.json"] {
            assert_eq!(serve_art_in(table, file, &HeaderMap::new()).status(), StatusCode::NOT_FOUND, "{file}");
        }

        // Same version again -> UpToDate.
        let (out2, _) = get_lang(&srv, &packs_dir, &count, "th").await;
        assert_eq!(out2, Outcome::UpToDate);

        // Restore clears the kept index.
        restore_builtin_index(&root).unwrap();
        assert!(!kept_index(&root));

        // Deleting the language deletes its drawings; the built-in ones stay.
        let (after, removed) = locales_store::remove_extra_lang_counted(&count, &packs_dir, thai).unwrap();
        assert!(removed);
        assert_eq!(serve_art_in(after, "art-th.json", &HeaderMap::new()).status(), StatusCode::NOT_FOUND);
        assert_eq!(serve_art_in(after, "art-la.json", &HeaderMap::new()).status(), StatusCode::NOT_FOUND);
        assert_eq!(serve_art_in(after, "art-mx.json", &HeaderMap::new()).status(), StatusCode::OK);
        assert!(!root.join(lang_file_name("th")).exists());
    }

    #[tokio::test]
    async fn a_pack_without_drawings_still_installs() {
        let d = tmp();
        let count = Arc::new(AtomicUsize::new(0));
        let packs_dir = d.0.join("packs");
        let th_gz = sample_lang_gz("th", 2026112101, true, true);
        assert!(sample_lang("th", 2026112101, true, true).get("art").is_none());
        let srv = release(index_for(2026112101, &[("th", 2026112101, &th_gz, "console")]), vec![(lang_file_name("th"), th_gz)]);
        let (out, table) = get_lang(&srv, &packs_dir, &count, "th").await;
        assert_eq!(out, Outcome::Installed);
        let table = table.unwrap();
        assert_eq!(table.stage(Ext::from_code("th").unwrap()), locales_store::Stage::Console);
        assert_eq!(serve_art_in(table, "art-th.json", &HeaderMap::new()).status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn builtin_drawings_cover_the_built_in_and_core_language_regions() {
        let mut want: Vec<&str> = CORE_LANG_REGIONS.to_vec();
        want.extend(BUILTIN_ART_REGIONS.iter().filter(|r| !PAGE_ART_REGIONS.contains(r)));
        want.sort();
        let have: Vec<&str> = builtin_art().keys().map(String::as_str).collect();
        assert_eq!(have, want);
        let table = locales_store::builtin();
        for region in want {
            let resp = serve_art_in(table, &format!("art-{region}.json"), &HeaderMap::new());
            assert_eq!(resp.status(), StatusCode::OK, "{region}");
            let text = art_text_in(table, region).unwrap();
            let v: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(v["region"], region);
            assert!(check_art(&format!("art-{region}.json"), text.as_bytes(), region).is_empty(), "{region}");
        }
        for region in PAGE_ART_REGIONS {
            assert!(!builtin_art().contains_key(region), "{region} is drawn into the page");
            assert!(art_text_in(table, region).is_none(), "{region}");
        }
        // A region of an extra language comes only with its language pack.
        assert!(art_text_in(table, "pl").is_none());
    }

    #[test]
    fn start_up_clears_drawings_downloaded_one_by_one() {
        let d = tmp();
        let root = d.0.join("extras");
        std::fs::create_dir_all(&root).unwrap();
        for name in ["art-vn.json", ".incoming-art-1-x.json", ".incoming-extra-lang-1-x.json.gz", "lang-th.json.gz", INDEX_NAME] {
            std::fs::write(root.join(name), b"x").unwrap();
        }
        clear_unfinished(&root);
        let mut left: Vec<String> = std::fs::read_dir(&root).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, [INDEX_NAME, "lang-th.json.gz"]);
    }

    #[tokio::test]
    async fn security_checks_refuse_tampered_oversized_and_bad_format_extras() {
        let d = tmp();
        let packs_dir = d.0.join("packs");
        let root = locales_store::extras_dir(&packs_dir);
        let count = Arc::new(AtomicUsize::new(0));
        let th_gz = sample_lang_gz("th", 2026112101, false, false);

        // SHA mismatch -> BadPack, nothing left behind.
        let mut tampered = th_gz.clone();
        tampered[10] ^= 0x55;
        let srv = release(index_for(2026112101, &[("th", 2026112101, &th_gz, "notify")]), vec![(lang_file_name("th"), tampered)]);
        let (out, _) = get_lang(&srv, &packs_dir, &count, "th").await;
        assert_eq!(out, Outcome::BadPack);
        assert!(!kept_index(&root), "index must not be saved when install fails");
        assert_eq!(locales_store::single_extra_version(&root, Ext::from_code("th").unwrap()), 0);

        // Format newer -> UpdateBageo.
        let newer_ix = json!({ "format": 99, "version": 2026112101u64, "langs": [], "art": [] }).to_string();
        let srv2 = release(newer_ix, Vec::new());
        let (out2, _) = get_lang(&srv2, &packs_dir, &count, "th").await;
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
        let srv3 = release(index_for(2026112102, &[("th", 2026112102, &bomb_gz, "notify")]), vec![(lang_file_name("th"), bomb_gz)]);
        let (out3, _) = get_lang(&srv3, &packs_dir, &count, "th").await;
        assert_eq!(out3, Outcome::BadPack);

        // Redirect to an unapproved host -> BadPack.
        let evil_ix = index_for(2026112101, &[("th", 2026112101, &th_gz, "notify")]);
        let srv5 = Server::start(move |path| {
            if path == LATEST {
                Reply::Body(evil_ix.clone().into_bytes())
            } else if path.ends_with(&lang_file_name("th")) {
                Reply::Redirect("https://evil.example.com/lang-th.json.gz".into())
            } else {
                Reply::NotFound
            }
        });
        let (out5, _) = get_lang(&srv5, &packs_dir, &count, "th").await;
        assert_eq!(out5, Outcome::BadPack);
    }

    #[tokio::test]
    async fn an_extras_index_without_its_own_good_signature_is_refused() {
        let th_gz = sample_lang_gz("th", 2026112101, true, true);
        let index = index_for(2026112101, &[("th", 2026112101, &th_gz, "console")]).into_bytes();
        let sig_path = format!("{TAGGED}{INDEX_NAME}.sig");
        let cases: [(&str, Option<Vec<u8>>); 5] = [
            ("no signature", None),
            ("too large", Some(vec![b'0'; 4096])),
            ("another key", Some(TestKey::fresh().sign(INDEX_NAME, &index))),
            ("the voice index's", Some(test_key().sign(crate::voice_packs::INDEX_NAME, &index))),
            ("one byte changed", Some(test_key().sign(INDEX_NAME, &index[..index.len() - 1]))),
        ];
        for (what, sig) in cases {
            let d = tmp();
            let packs_dir = d.0.join("packs");
            let root = locales_store::extras_dir(&packs_dir);
            let count = Arc::new(AtomicUsize::new(0));
            let (ix, gz, sp) = (index.clone(), th_gz.clone(), sig_path.clone());
            let srv = Server::start_raw(move |path| {
                if path == LATEST {
                    Reply::Redirect(format!("{TAGGED}{INDEX_NAME}"))
                } else if path == format!("{TAGGED}{INDEX_NAME}") {
                    Reply::Body(ix.clone())
                } else if path == sp {
                    sig.clone().map_or(Reply::NotFound, Reply::Body)
                } else if path == format!("{TAGGED}{}", lang_file_name("th")) {
                    Reply::Body(gz.clone())
                } else {
                    Reply::NotFound
                }
            });
            let (out, table) = get_lang(&srv, &packs_dir, &count, "th").await;
            assert_eq!(out, Outcome::BadPack, "{what}");
            assert!(table.is_none(), "{what}");
            assert_eq!(srv.asked(&lang_file_name("th")), 0, "{what}: the pack was never asked");
            assert!(!kept_index(&root), "{what}");
            assert_eq!(locales_store::single_extra_version(&root, Ext::from_code("th").unwrap()), 0, "{what}");
            assert!(std::fs::read_dir(&root).map(|d| d.count() == 0).unwrap_or(true), "{what}: nothing left behind");
        }
        // Signed right, the same release installs.
        let d = tmp();
        let srv = release(String::from_utf8(index).unwrap(), vec![(lang_file_name("th"), th_gz)]);
        let (out, _) = get_lang(&srv, &d.0.join("packs"), &Arc::new(AtomicUsize::new(0)), "th").await;
        assert_eq!(out, Outcome::Installed);
        assert_eq!(srv.asked(&sig_path), 1);
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
        assert!(body.get("art").is_none());

        // Core language or bad code returns not_listed.
        let r2 = download(
            State(state.clone()),
            h.clone(),
            Query(TokenQuery { token: Some("extras-secret".into()) }),
            Bytes::from_static(br#"{"lang":"en"}"#),
        )
        .await;
        assert_eq!(r2.status(), StatusCode::OK);
        let body2: Value = serde_json::from_str(&crate::calls_http::tests::body_text(r2).await).unwrap();
        assert_eq!(body2["result"], "not_listed");

        // Drawings are no longer downloaded or deleted one by one.
        for route in ["download", "delete"] {
            let body = Bytes::from_static(br#"{"art":"vn"}"#);
            let q = Query(TokenQuery { token: Some("extras-secret".into()) });
            let r = if route == "download" { download(State(state.clone()), h.clone(), q, body).await } else { delete(State(state.clone()), h.clone(), q, body).await };
            assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{route}");
        }
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
