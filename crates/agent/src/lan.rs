// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The web console on this network when `AGENT_HOST` is not set ("automatic"):
//! it listens on all addresses at `AGENT_PORT` and, when it can, at port 80
//! too, so `http://bageo.local/` opens it on this computer and on the phones
//! and computers of the same Wi-Fi.
//!
//! Other devices get in only once a console password is set ([`guard`]):
//! without one, a browser there gets a short page that says to set it on this
//! computer first, and the API answers 403 `need_password`. Requests from this
//! computer itself (loopback, or this computer's own LAN address) go through as
//! before. A set `AGENT_HOST` keeps the old behavior: none of this applies.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::connect_info::{Connected, MockConnectInfo};
use axum::extract::{ConnectInfo, Request};
use axum::http::{header, Extensions, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::serve::IncomingStream;
use axum::{Json, Router};
use serde_json::json;
use tracing::warn;

use crate::config::Settings;
use crate::locale::Lang;
use crate::settings_store::SettingsStore;

/// Both ends of a connection: who connected (`peer`) and the address of this
/// computer it reached (`local`), so a request this computer makes to its own
/// LAN address counts as its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnAddrs {
    pub peer: SocketAddr,
    pub local: Option<SocketAddr>,
}

impl Connected<IncomingStream<'_, tokio::net::TcpListener>> for ConnAddrs {
    fn connect_info(stream: IncomingStream<'_, tokio::net::TcpListener>) -> Self {
        Self {
            peer: *stream.remote_addr(),
            local: stream.io().local_addr().ok(),
        }
    }
}

/// The console listens on this network because `AGENT_HOST` is not set.
static AUTO: AtomicBool = AtomicBool::new(false);
/// The console also answers at port 80.
static PORT_80: AtomicBool = AtomicBool::new(false);

pub fn set_auto(on: bool) {
    AUTO.store(on, Ordering::Relaxed);
}

/// The console is open to this network on its own (`AGENT_HOST` not set).
pub fn auto() -> bool {
    AUTO.load(Ordering::Relaxed)
}

pub fn set_port_80(on: bool) {
    PORT_80.store(on, Ordering::Relaxed);
}

/// The console answers at port 80 right now.
pub fn port_80() -> bool {
    PORT_80.load(Ordering::Relaxed)
}

/// `CONSOLE_PORT_80`: `off`, `0`, `false` or `no` (any case) keeps port 80
/// closed; anything else, or not set, opens it in the automatic mode.
pub fn port_80_wanted(raw: Option<&str>) -> bool {
    !matches!(
        raw.map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Some("off" | "0" | "false" | "no")
    )
}

/// The peer and this computer's address of a request: from [`ConnAddrs`], or
/// only the peer when the server gave a plain `SocketAddr` (tests, the
/// phones' Wi-Fi entry). `MockConnectInfo` (tests) counts like the real one.
pub fn addrs(ext: &Extensions) -> Option<(SocketAddr, Option<SocketAddr>)> {
    let both = ext
        .get::<ConnectInfo<ConnAddrs>>()
        .map(|c| c.0)
        .or_else(|| ext.get::<MockConnectInfo<ConnAddrs>>().map(|m| m.0));
    if let Some(c) = both {
        return Some((c.peer, c.local));
    }
    ext.get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0)
        .or_else(|| ext.get::<MockConnectInfo<SocketAddr>>().map(|m| m.0))
        .map(|peer| (peer, None))
}

/// `peer` is this computer: a loopback address, or the very address of this
/// computer the connection reached (`http://192.168.1.20/` opened on 192.168.1.20).
pub fn same_computer(peer: IpAddr, local: Option<IpAddr>) -> bool {
    let peer = peer.to_canonical();
    peer.is_loopback()
        || local.is_some_and(|l| {
            let l = l.to_canonical();
            !l.is_unspecified() && l == peer
        })
}

/// The request's peer is this computer ([`same_computer`]); a reverse proxy
/// on it is not checked here.
pub fn peer_is_this_computer(ext: &Extensions) -> bool {
    addrs(ext).is_some_and(|(peer, local)| same_computer(peer.ip(), local.map(|l| l.ip())))
}

/// The request comes from this computer itself, not through a reverse proxy on it.
pub fn from_this_computer(ext: &Extensions, headers: &HeaderMap) -> bool {
    peer_is_this_computer(ext) && !crate::calls_http::came_through_proxy(headers)
}

/// Gives the handlers that read `ConnectInfo<SocketAddr>` the peer of a
/// connection served with [`ConnAddrs`].
pub async fn copy_peer(mut req: Request, next: Next) -> Response {
    if req.extensions().get::<ConnectInfo<SocketAddr>>().is_none() {
        if let Some((peer, _)) = addrs(req.extensions()) {
            req.extensions_mut().insert(ConnectInfo(peer));
        }
    }
    next.run(req).await
}

/// What [`gate`] decides for a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    /// Goes on to its handler (which may check a password or a key itself).
    Pass,
    /// A phone without the pairing key or `DEVICE_TOKEN`: 401.
    Phone,
    /// A browser page: "set a console password first".
    Page,
    /// The API: 403 `need_password`.
    NeedPassword,
}

/// What [`gate`] looks at.
#[derive(Clone, Copy, Debug, Default)]
pub struct Facts<'a> {
    pub path: &'a str,
    /// A WebSocket upgrade (a phone on `/`).
    pub websocket: bool,
    /// From this computer itself ([`from_this_computer`]).
    pub this_computer: bool,
    /// A console password is set.
    pub password_set: bool,
    /// It carries `DEVICE_TOKEN` or, while phone Wi-Fi is on, the pairing key.
    pub phone_key_ok: bool,
    /// An MCP token is set (then the chat command webhook needs a token).
    pub mcp_token_set: bool,
    /// An `.ics` link's key that opens this file.
    pub ics_key_ok: bool,
}

/// Who gets through in the automatic mode. Everything from this computer.
/// From other devices: phones only with their key; with a password set,
/// everything else (each route checks it as before); without one, only what
/// has its own key or shows nothing private:
///
/// - `/health` (others get the short version), icons and the app manifest;
/// - `/calls/api/quick-action`: the notification buttons, each link signed;
/// - an `.ics` link with the key made for that one file;
/// - `/mcp` (its own token; off until one is set), and the chat command
///   webhook `/calls/api/control/inbound` while an MCP token is set (without
///   one it would run anyone's command as the owner).
pub fn gate(f: &Facts) -> Gate {
    if f.this_computer {
        return Gate::Pass;
    }
    if f.path == "/ws" || (f.path == "/" && f.websocket) {
        return if f.phone_key_ok { Gate::Pass } else { Gate::Phone };
    }
    if f.password_set {
        return Gate::Pass;
    }
    match f.path {
        "/health"
        | "/favicon.ico"
        | "/calls/favicon.ico"
        | "/calls/manifest.webmanifest"
        | "/calls/icon-192.png"
        | "/calls/icon-512.png"
        | "/calls/apple-touch-icon.png"
        | "/calls/api/quick-action"
        | "/mcp"
        // A phone trades a pairing code for its key; the code is the proof.
        | "/api/phone-pairing/claim" => return Gate::Pass,
        "/calls/api/control/inbound" if f.mcp_token_set => return Gate::Pass,
        _ => {}
    }
    if ics_what(f.path).is_some() && f.ics_key_ok {
        return Gate::Pass;
    }
    if f.path.starts_with("/calls/api/") || f.path.starts_with("/v1/") {
        Gate::NeedPassword
    } else {
        Gate::Page
    }
}

/// `call/<id>` or `tasks/<id>` for the path of an `.ics` download (what its
/// link's key was made for, `crate::calendar::key_opens`).
pub fn ics_what(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/calls/api/")?.strip_suffix("/event.ics")?;
    let (kind, id) = rest.split_once('/')?;
    (matches!(kind, "call" | "tasks") && !id.is_empty() && !id.contains('/')).then_some(rest)
}

/// The value of `name` in a query string.
fn query_param(query: Option<&str>, name: &str) -> Option<String> {
    url::form_urlencoded::parse(query?.as_bytes())
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
}

/// [`gate`] in front of `router` (the automatic mode only).
/// First of all, the address a request names must be one of this computer's
/// ([`host_allowed`]): a web page that points its own name at this
/// computer (DNS rebinding) gets nothing, not even from this computer.
pub fn guard(router: Router, settings: Settings, store: Arc<SettingsStore>) -> Router {
    let settings = Arc::new(settings);
    router.layer(axum::middleware::from_fn(move |req: Request, next: Next| {
        let settings = settings.clone();
        let store = store.clone();
        async move {
            let rt = store.snapshot();
            let host = request_host(req.headers());
            let rules = HostRules {
                local: addrs(req.extensions()).and_then(|(_, local)| local).map(|l| l.ip()),
                lan: crate::mdns::lan_ipv4(),
                mdns: crate::mdns::parse_name_setting(&rt.mdns_name),
                public: public_host(if rt.public_url.trim().is_empty() { &settings.public_url } else { &rt.public_url }),
            };
            if !host_allowed(host.as_deref(), &rules) {
                let path = req.uri().path();
                warn!("refused a request for {path:?} naming {host:?}: not an address of this computer (see Address in notifications)");
                if is_api(path) {
                    return api_refusal(StatusCode::FORBIDDEN, "bad_host", "this address does not open the console; use the one under System → About");
                }
                let accept = req.headers().get(header::ACCEPT_LANGUAGE).and_then(|v| v.to_str().ok());
                let lang = page_lang(accept).unwrap_or_else(crate::locale::notify_lang);
                let (title, detail) = bad_host_words(lang);
                return page(lang, title, detail);
            }
            if from_this_computer(req.extensions(), req.headers()) {
                return next.run(req).await;
            }
            let path = req.uri().path().to_string();
            let query = req.uri().query();
            let token = query_param(query, "token");
            let device = settings.device_token.trim();
            let phone_key_ok = (!device.is_empty()
                && token.as_deref().is_some_and(|t| crate::phone_wifi::same_token(t.trim(), device)))
                || (rt.phone_wifi && crate::phone_wifi::pairing().accepts(token.as_deref()));
            let ics_key_ok = match (ics_what(&path), query_param(query, "k")) {
                (Some(what), Some(k)) => crate::calendar::key_opens(what, k.trim()),
                _ => false,
            };
            let facts = Facts {
                path: &path,
                websocket: req
                    .headers()
                    .get(header::UPGRADE)
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| v.eq_ignore_ascii_case("websocket")),
                this_computer: false,
                password_set: !crate::settings_store::active_ui_token(&settings.calls_ui_token).trim().is_empty(),
                phone_key_ok,
                mcp_token_set: !rt.mcp_token.trim().is_empty(),
                ics_key_ok,
            };
            match gate(&facts) {
                Gate::Pass => next.run(req).await,
                Gate::Phone => (StatusCode::UNAUTHORIZED, "pairing key required").into_response(),
                Gate::NeedPassword => need_password_json(),
                Gate::Page => {
                    let accept = req.headers().get(header::ACCEPT_LANGUAGE).and_then(|v| v.to_str().ok());
                    need_password_page(page_lang(accept).unwrap_or_else(crate::locale::notify_lang))
                }
            }
        }
    }))
}

/// The API (JSON answers) rather than a page.
fn is_api(path: &str) -> bool {
    path.starts_with("/calls/api/") || path.starts_with("/v1/") || path == "/mcp" || path == "/health"
}

/// An API refusal: `code` for the console, `error` for scripts.
fn api_refusal(status: StatusCode, code: &str, error: &str) -> Response {
    (status, Json(json!({"ok": false, "code": code, "error": error}))).into_response()
}

/// The host a request names: its `Host` header without the port, in lower
/// case, without a trailing dot, an IPv6 address without brackets. None
/// without a `Host` header; empty when the header cannot be read.
pub fn request_host(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::HOST)?;
    Some(raw.to_str().map(host_part).unwrap_or_default())
}

fn host_part(raw: &str) -> String {
    let h = raw.trim().to_ascii_lowercase();
    let name = match h.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or("").to_string(),
        None => match h.rsplit_once(':') {
            Some((name, port)) if !name.contains(':') && port.bytes().all(|b| b.is_ascii_digit()) => name.to_string(),
            _ => h.clone(),
        },
    };
    name.trim_end_matches('.').to_string()
}

/// The host of the typed console address ("Address in notifications"), as
/// [`request_host`] gives it.
fn public_host(public_url: &str) -> Option<String> {
    let url = url::Url::parse(public_url.trim()).ok()?;
    url.host_str().map(host_part).filter(|h| !h.is_empty())
}

/// The names and addresses this computer answers to.
#[derive(Debug, Default)]
pub struct HostRules {
    /// The address of this computer the connection reached.
    pub local: Option<IpAddr>,
    /// This computer's LAN address (`crate::mdns::lan_ipv4`).
    pub lan: Option<Ipv4Addr>,
    /// The network name, without `.local` (empty: off).
    pub mdns: String,
    /// The host of the typed console address.
    pub public: Option<String>,
}

/// A request may name: `localhost` and `*.localhost`, a loopback address,
/// the address of this computer the connection reached or its LAN address,
/// `<network name>.local`, or the host of the typed console address.
/// Anything else is a name someone else controls (DNS rebinding). A request
/// without a `Host` header (HTTP/1.0 tools) passes: a browser always sends
/// one, and only a browser can be led into rebinding.
pub fn host_allowed(host: Option<&str>, r: &HostRules) -> bool {
    let Some(h) = host else {
        return true;
    };
    if h.is_empty() {
        return false;
    }
    if h == "localhost" || h.ends_with(".localhost") {
        return true;
    }
    if let Ok(ip) = h.parse::<IpAddr>() {
        let ip = ip.to_canonical();
        return !ip.is_unspecified()
            && (ip.is_loopback()
                || r.local.is_some_and(|l| l.to_canonical() == ip)
                || r.lan.is_some_and(|l| IpAddr::V4(l) == ip));
    }
    (!r.mdns.is_empty() && h == format!("{}.local", r.mdns)) || r.public.as_deref() == Some(h)
}

/// The page's title and sentence when the address is not this computer's.
pub fn bad_host_words(lang: Lang) -> (&'static str, &'static str) {
    match lang {
        Lang::ZhTw => ("用這個網址打不開控制台", "請改用「系統 → 關於」裡的網址。用自己的網域時，先把它填進「通知裡的網址」。"),
        Lang::ZhCn => ("用这个地址打不开控制台", "请改用“系统 → 关于”里的地址。用自己的域名时，先把它填进“通知里的地址”。"),
        Lang::En => ("This address can't open the console", "Use the address under System → About. For your own domain, first enter it under Address in notifications."),
        Lang::Ja => ("このアドレスではコンソールを開けません", "「システム → 情報」のアドレスを使ってください。独自のドメインを使うときは、先に「通知に使うアドレス」に入力してください。"),
        Lang::Ko => ("이 주소로는 콘솔을 열 수 없습니다", "‘시스템 → 정보’에 나온 주소를 쓰세요. 자기 도메인을 쓸 때는 먼저 ‘알림에 쓰는 주소’에 입력하세요."),
        Lang::Es => ("Con esta dirección no se abre la consola", "Usa la dirección de «Sistema → Acerca de». Para tu propio dominio, escríbelo antes en «Dirección en las notificaciones»."),
        Lang::Fr => ("Cette adresse n'ouvre pas la console", "Utilisez l'adresse indiquée dans « Système → À propos ». Pour votre propre domaine, saisissez-le d'abord dans « Adresse dans les notifications »."),
        Lang::De => ("Mit dieser Adresse öffnet sich die Konsole nicht", "Nutze die Adresse unter „System → Info“. Für deine eigene Domain trag sie zuerst unter „Adresse in Benachrichtigungen“ ein."),
        Lang::Pt => ("Este endereço não abre o console", "Use o endereço em “Sistema → Sobre”. Para o seu próprio domínio, digite-o antes em “Endereço nas notificações”."),
    }
}

/// Wrong passwords from one address: the first [`FREE_TRIES`] answer at
/// once, the next ones a second late, and after [`MAX_TRIES`] within
/// [`TRY_WINDOW`] every request with a password waits out [`TRY_LOCK`]
/// (429 `too_many_tries`). A right password starts over. The same wrong
/// password sent again (the console's parallel requests) counts once.
pub const FREE_TRIES: u32 = 5;
pub const MAX_TRIES: u32 = 10;
pub const TRY_WINDOW: Duration = Duration::from_secs(60);
pub const TRY_LOCK: Duration = Duration::from_secs(60);
const SLOW_ANSWER: Duration = Duration::from_secs(1);
const _: () = assert!(FREE_TRIES < MAX_TRIES);
/// Addresses kept at most (older ones are dropped first when they are done).
const MAX_ADDRESSES: usize = 4096;

#[derive(Debug, Clone, Copy)]
struct TryEntry {
    fails: u32,
    since: Instant,
    last_wrong: u64,
    locked_until: Option<Instant>,
}

/// Wrong-password counts per address ([`throttle`]).
#[derive(Debug, Default)]
pub struct Tries {
    map: HashMap<IpAddr, TryEntry>,
}

impl Tries {
    /// How long `ip` must still wait, while it is locked out.
    pub fn locked(&mut self, ip: IpAddr, now: Instant) -> Option<Duration> {
        let until = self.map.get(&ip)?.locked_until?;
        if now < until {
            return Some(until - now);
        }
        self.map.remove(&ip);
        None
    }

    /// A wrong password (`wrong`: a fingerprint of it) from `ip`: the count so far.
    pub fn failed(&mut self, ip: IpAddr, wrong: u64, now: Instant) -> u32 {
        if self.map.len() >= MAX_ADDRESSES && !self.map.contains_key(&ip) {
            self.map.retain(|_, e| e.locked_until.is_some_and(|u| now < u) || now.duration_since(e.since) < TRY_WINDOW);
            if self.map.len() >= MAX_ADDRESSES {
                return 0;
            }
        }
        let e = self.map.entry(ip).or_insert(TryEntry { fails: 0, since: now, last_wrong: 0, locked_until: None });
        if now.duration_since(e.since) >= TRY_WINDOW {
            *e = TryEntry { fails: 0, since: now, last_wrong: 0, locked_until: None };
        }
        if e.fails > 0 && e.last_wrong == wrong {
            return e.fails;
        }
        e.fails += 1;
        e.last_wrong = wrong;
        if e.fails >= MAX_TRIES {
            e.locked_until = Some(now + TRY_LOCK);
        }
        e.fails
    }

    /// A right password from `ip`: its count starts over.
    pub fn succeeded(&mut self, ip: IpAddr) {
        self.map.remove(&ip);
    }
}

/// A fingerprint of the passwords a request carries (`?token=`, a bearer
/// token, `X-Calls-Token`, or the `token` in a chat command's body); None
/// when it carries none.
fn credential(headers: &HeaderMap, query: Option<&str>, body_token: Option<&str>) -> Option<u64> {
    let token = query_param(query, "token").filter(|t| !t.trim().is_empty());
    let auth = headers.get(header::AUTHORIZATION).map(|v| v.as_bytes().to_vec());
    let calls = headers.get("x-calls-token").map(|v| v.as_bytes().to_vec());
    let body = body_token.map(str::trim).filter(|t| !t.is_empty());
    if token.is_none() && auth.is_none() && calls.is_none() && body.is_none() {
        return None;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (token, auth, calls, body).hash(&mut h);
    Some(h.finish())
}

/// The chat command webhook, which also takes its password in the JSON body.
const INBOUND_PATH: &str = "/calls/api/control/inbound";
/// The most of a chat command's body read here (the server's default limit).
const INBOUND_BODY_MAX: usize = 2 << 20;

/// The request again with its body read, and the `token` of that JSON body.
async fn body_token(req: Request) -> Result<(Request, Option<String>), Response> {
    let (parts, body) = req.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, INBOUND_BODY_MAX).await else {
        return Err(api_refusal(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "the request is too large"));
    };
    let token = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|v| v.get("token").and_then(|t| t.as_str()).map(|t| t.trim().to_string()))
        .filter(|t| !t.is_empty());
    Ok((Request::from_parts(parts, axum::body::Body::from(bytes)), token))
}

/// Routes whose `?token=` / bearer token is not the console password (the
/// phones' key, the MCP token): their own 401 says whether it was wrong.
fn own_token_route(path: &str, websocket: bool) -> bool {
    path == "/mcp" || path == "/calls/api/control/inbound" || path == "/ws" || (path == "/" && websocket)
}

/// Whose wrong passwords a request counts as: the address that connected.
/// Only a reverse proxy on this computer itself is believed about the
/// browser behind it (the last address it put in `X-Forwarded-For`, else
/// `X-Real-IP`): anyone who reaches Bageo directly can write those headers,
/// so from any other address they would let each guess look new. A proxy on
/// another machine (a NAS) is counted as itself.
fn try_key(ext: &Extensions, headers: &HeaderMap) -> Option<IpAddr> {
    let (peer, local) = addrs(ext)?;
    let peer = peer.ip().to_canonical();
    if same_computer(peer, local.map(|l| l.ip())) {
        if let Some(client) = forwarded_client(headers) {
            return Some(client);
        }
    }
    Some(peer)
}

/// The client a proxy names: the last `X-Forwarded-For` address (the one the
/// proxy added), else `X-Real-IP`.
fn forwarded_client(headers: &HeaderMap) -> Option<IpAddr> {
    let parse = |s: &str| {
        let s = s.trim().trim_matches('"');
        s.parse::<IpAddr>()
            .ok()
            .or_else(|| s.parse::<SocketAddr>().ok().map(|a| a.ip()))
            .map(|ip| ip.to_canonical())
    };
    headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(parse)
        .next_back()
        .or_else(|| headers.get("x-real-ip").and_then(|v| v.to_str().ok()).and_then(parse))
}

/// Slows down and then stops password guessing from other devices
/// ([`Tries`]); this computer is never held back. A request counts only
/// when it carries a password: wrong when it is not the console password
/// (where one is set), or, on routes with their own token, when the answer is 401.
/// Only a right console password starts the count over. Counted per [`try_key`].
pub fn throttle(router: Router, settings: Settings) -> Router {
    let settings = Arc::new(settings);
    let tries = Arc::new(Mutex::new(Tries::default()));
    router.layer(axum::middleware::from_fn(move |req: Request, next: Next| {
        let settings = settings.clone();
        let tries = tries.clone();
        async move {
            if from_this_computer(req.extensions(), req.headers()) {
                return next.run(req).await;
            }
            let (req, body_tok) = if req.uri().path() == INBOUND_PATH {
                match body_token(req).await {
                    Ok(read) => read,
                    Err(res) => return res,
                }
            } else {
                (req, None)
            };
            let Some(wrong) = credential(req.headers(), req.uri().query(), body_tok.as_deref()) else {
                return next.run(req).await;
            };
            let Some(ip) = try_key(req.extensions(), req.headers()) else {
                return next.run(req).await;
            };
            let wait = tries.lock().unwrap_or_else(|e| e.into_inner()).locked(ip, Instant::now());
            if let Some(wait) = wait {
                let mut res = api_refusal(StatusCode::TOO_MANY_REQUESTS, "too_many_tries", "too many wrong passwords; try again in a minute");
                if let Ok(v) = HeaderValue::from_str(&wait.as_secs().max(1).to_string()) {
                    res.headers_mut().insert(header::RETRY_AFTER, v);
                }
                return res;
            }
            let websocket = req
                .headers()
                .get(header::UPGRADE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
            let password_set = !crate::settings_store::active_ui_token(&settings.calls_ui_token).trim().is_empty();
            let right = (password_set && !own_token_route(req.uri().path(), websocket)).then(|| {
                crate::calls_http::token_ok_parts(&settings, req.headers(), query_param(req.uri().query(), "token").as_deref())
            });
            let res = next.run(req).await;
            match right {
                Some(true) => {
                    tries.lock().unwrap_or_else(|e| e.into_inner()).succeeded(ip);
                    return res;
                }
                Some(false) => {}
                // A route with its own token (or no console password set): only its 401
                // counts as a wrong password, and nothing it answers starts the count over,
                // else any other answer (a 404, a 415) would clear it between guesses.
                None if res.status() != StatusCode::UNAUTHORIZED => return res,
                None => {}
            }
            let fails = tries.lock().unwrap_or_else(|e| e.into_inner()).failed(ip, wrong, Instant::now());
            if fails >= MAX_TRIES {
                warn!("{ip}: {fails} wrong passwords within a minute; it waits a minute now");
            }
            if fails > FREE_TRIES {
                tokio::time::sleep(SLOW_ANSWER).await;
            }
            res
        }
    }))
}

/// 403 `need_password` for the API.
pub fn need_password_json() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "ok": false,
            "code": "need_password",
            "error": "set a console password on the computer running Bageo first",
        })),
    )
        .into_response()
}

/// The first of the console's nine languages in an `Accept-Language` header.
pub fn page_lang(accept: Option<&str>) -> Option<Lang> {
    for part in accept?.split(',') {
        let tag = part.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        let primary = tag.split(['-', '_']).next().unwrap_or("");
        let lang = match primary {
            "zh" if ["hant", "tw", "hk", "mo"].iter().any(|t| tag.contains(t)) => Lang::ZhTw,
            "zh" => Lang::ZhCn,
            "en" => Lang::En,
            "ja" => Lang::Ja,
            "ko" => Lang::Ko,
            "es" => Lang::Es,
            "fr" => Lang::Fr,
            "de" => Lang::De,
            "pt" => Lang::Pt,
            _ => continue,
        };
        return Some(lang);
    }
    None
}

/// The page's `lang` attribute, title and sentence in `lang`.
pub fn page_words(lang: Lang) -> (&'static str, &'static str, &'static str) {
    match lang {
        Lang::ZhTw => ("zh-Hant", "請先設密碼", "要從其他裝置打開控制台，請先在 Bageo 所在的電腦打開控制台、設好密碼。"),
        Lang::ZhCn => ("zh-Hans", "请先设置密码", "要从其他设备打开控制台，请先在 Bageo 所在的电脑上打开控制台，设好密码。"),
        Lang::En => ("en", "Set a password first", "To open the console from another device, first open the console on the computer running Bageo and set a password."),
        Lang::Ja => ("ja", "先にパスワードを設定してください", "ほかの端末からコンソールを開くには、まず Bageo が動いているパソコンでコンソールを開き、パスワードを設定してください。"),
        Lang::Ko => ("ko", "먼저 비밀번호를 정하세요", "다른 기기에서 콘솔을 열려면 먼저 Bageo가 실행 중인 컴퓨터에서 콘솔을 열고 비밀번호를 정하세요."),
        Lang::Es => ("es", "Primero pon una contraseña", "Para abrir la consola desde otro dispositivo, primero ábrela en el equipo donde está Bageo y pon una contraseña."),
        Lang::Fr => ("fr", "Définissez d'abord un mot de passe", "Pour ouvrir la console depuis un autre appareil, ouvrez-la d'abord sur l'ordinateur où tourne Bageo et définissez un mot de passe."),
        Lang::De => ("de", "Zuerst ein Passwort festlegen", "Um die Konsole von einem anderen Gerät zu öffnen, öffne sie zuerst auf dem Computer, auf dem Bageo läuft, und lege ein Passwort fest."),
        Lang::Pt => ("pt-BR", "Defina uma senha primeiro", "Para abrir o console em outro aparelho, abra primeiro o console no computador onde o Bageo está e defina uma senha."),
    }
}

/// The page for a browser on another device while no password is set.
pub fn need_password_page(lang: Lang) -> Response {
    let (_, title, detail) = page_words(lang);
    page(lang, title, detail)
}

/// A short page in `lang` (403).
fn page(lang: Lang, title: &str, detail: &str) -> Response {
    let (html_lang, _, _) = page_words(lang);
    let html = format!(
        "<!doctype html><html lang=\"{html_lang}\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{title} — Bageo</title>\
         <style>body{{font-family:system-ui,-apple-system,sans-serif;background:#faf8f5;color:#1f1e1b;display:flex;align-items:center;justify-content:center;min-height:90vh;margin:0;padding:16px}}\
         .card{{background:#fff;border:1px solid #e6e1d6;border-radius:14px;max-width:420px;width:100%;padding:24px;box-shadow:0 4px 16px rgba(0,0,0,.05);text-align:center}}\
         h1{{font-size:18px;margin:0 0 8px}}p{{font-size:14px;color:#5c584f;margin:0;word-break:break-word}}</style></head>\
         <body><div class=\"card\"><h1>{title}</h1><p>{detail}</p></div></body></html>"
    );
    let mut res = (StatusCode::FORBIDDEN, html).into_response();
    res.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn this_computer_is_loopback_or_its_own_address() {
        assert!(same_computer(ip("127.0.0.1"), None));
        assert!(same_computer(ip("::1"), None));
        assert!(same_computer(ip("::ffff:127.0.0.1"), None));
        assert!(same_computer(ip("192.168.1.20"), Some(ip("192.168.1.20"))));
        assert!(same_computer(ip("::ffff:192.168.1.20"), Some(ip("192.168.1.20"))));
        assert!(!same_computer(ip("192.168.1.21"), Some(ip("192.168.1.20"))));
        assert!(!same_computer(ip("192.168.1.21"), None));
        assert!(!same_computer(ip("0.0.0.0"), Some(ip("0.0.0.0"))));
    }

    #[test]
    fn from_this_computer_reads_both_ends_and_refuses_proxies() {
        let conn = |peer: &str, local: &str| {
            let mut e = Extensions::new();
            e.insert(ConnectInfo(ConnAddrs { peer: peer.parse().unwrap(), local: Some(local.parse().unwrap()) }));
            e
        };
        let plain = HeaderMap::new();
        let mut proxied = HeaderMap::new();
        proxied.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.7"));
        assert!(from_this_computer(&conn("127.0.0.1:5000", "127.0.0.1:8787"), &plain));
        assert!(from_this_computer(&conn("192.168.1.20:5000", "192.168.1.20:80"), &plain));
        assert!(!from_this_computer(&conn("192.168.1.30:5000", "192.168.1.20:80"), &plain));
        assert!(!from_this_computer(&conn("192.168.1.20:5000", "192.168.1.20:80"), &proxied));
        assert!(!from_this_computer(&conn("127.0.0.1:5000", "127.0.0.1:8787"), &proxied));
        // A plain SocketAddr (no local address): loopback only.
        let mut e = Extensions::new();
        e.insert(ConnectInfo::<SocketAddr>("127.0.0.1:5000".parse().unwrap()));
        assert!(from_this_computer(&e, &plain));
        let mut e = Extensions::new();
        e.insert(ConnectInfo::<SocketAddr>("192.168.1.20:5000".parse().unwrap()));
        assert!(!from_this_computer(&e, &plain));
        assert!(!from_this_computer(&Extensions::new(), &plain));
    }

    #[test]
    fn port_80_is_on_unless_turned_off() {
        assert!(port_80_wanted(None));
        assert!(port_80_wanted(Some("")));
        assert!(port_80_wanted(Some("on")));
        for off in ["off", "OFF", " 0 ", "false", "No"] {
            assert!(!port_80_wanted(Some(off)), "{off}");
        }
    }

    fn outside(path: &'static str) -> Facts<'static> {
        Facts { path, ..Default::default() }
    }

    #[test]
    fn without_a_password_other_devices_get_only_what_has_its_own_key() {
        for path in ["/", "/calls/", "/calls", "/index", "/whatever"] {
            assert_eq!(gate(&outside(path)), Gate::Page, "{path}");
        }
        for path in [
            "/calls/api/list",
            "/calls/api/settings",
            "/calls/api/initial-password",
            "/calls/api/health",
            "/calls/api/auth-status",
            "/calls/api/backup/export",
            "/calls/api/control/inbound",
            "/calls/api/call/abc/event.ics",
            "/v1/turn",
        ] {
            assert_eq!(gate(&outside(path)), Gate::NeedPassword, "{path}");
        }
        for path in [
            "/health",
            "/favicon.ico",
            "/calls/manifest.webmanifest",
            "/calls/icon-192.png",
            "/calls/api/quick-action",
            "/mcp",
            "/api/phone-pairing/claim",
        ] {
            assert_eq!(gate(&outside(path)), Gate::Pass, "{path}");
        }
        let inbound = Facts { mcp_token_set: true, ..outside("/calls/api/control/inbound") };
        assert_eq!(gate(&inbound), Gate::Pass);
        let ics = Facts { ics_key_ok: true, ..outside("/calls/api/tasks/t1/event.ics") };
        assert_eq!(gate(&ics), Gate::Pass);
        // A key that opens a file never opens another route.
        let not_ics = Facts { ics_key_ok: true, ..outside("/calls/api/list") };
        assert_eq!(gate(&not_ics), Gate::NeedPassword);
    }

    #[test]
    fn phones_need_their_key_and_this_computer_always_passes() {
        assert_eq!(gate(&outside("/ws")), Gate::Phone);
        assert_eq!(gate(&Facts { websocket: true, ..outside("/") }), Gate::Phone);
        assert_eq!(gate(&Facts { password_set: true, ..outside("/ws") }), Gate::Phone);
        assert_eq!(gate(&Facts { phone_key_ok: true, ..outside("/ws") }), Gate::Pass);
        assert_eq!(gate(&Facts { phone_key_ok: true, websocket: true, ..outside("/") }), Gate::Pass);
        for path in ["/ws", "/calls/", "/calls/api/list", "/calls/api/initial-password", "/v1/turn"] {
            assert_eq!(gate(&Facts { this_computer: true, ..outside(path) }), Gate::Pass, "{path}");
        }
    }

    #[test]
    fn with_a_password_everything_goes_to_its_own_check() {
        for path in ["/", "/calls/", "/calls/api/list", "/calls/api/settings", "/v1/turn", "/calls/api/control/inbound"] {
            assert_eq!(gate(&Facts { password_set: true, ..outside(path) }), Gate::Pass, "{path}");
        }
    }

    #[test]
    fn ics_paths() {
        assert_eq!(ics_what("/calls/api/call/abc/event.ics"), Some("call/abc"));
    }

    #[test]
    fn host_header_is_read_the_way_browsers_send_it() {
        let read = |v: &[u8]| {
            let mut h = HeaderMap::new();
            h.insert(header::HOST, HeaderValue::from_bytes(v).unwrap());
            request_host(&h)
        };
        assert_eq!(read(b"Evil.Example:8787").as_deref(), Some("evil.example"));
        assert_eq!(read(b"[::1]:8787").as_deref(), Some("::1"));
        assert_eq!(read(b"localhost.").as_deref(), Some("localhost"));
        assert_eq!(read(b"bageo.local.:80").as_deref(), Some("bageo.local"));
        assert_eq!(read(b"192.168.1.20").as_deref(), Some("192.168.1.20"));
        assert_eq!(read(b"\xffbad").as_deref(), Some(""), "unreadable counts as no name of ours");
        assert_eq!(request_host(&HeaderMap::new()), None);
        assert_eq!(public_host("https://Calls.Example.org/calls/").as_deref(), Some("calls.example.org"));
        assert_eq!(public_host("http://[fe80::1]:8787/").as_deref(), Some("fe80::1"));
        assert_eq!(public_host(""), None);
    }

    #[test]
    fn rebinding_names_are_refused_and_this_computers_pass() {
        let rules = HostRules {
            local: Some(ip("192.168.1.20")),
            lan: Some(Ipv4Addr::new(192, 168, 1, 21)),
            mdns: "bageo".into(),
            public: public_host("https://calls.example.org/"),
        };
        for ok in [
            None,
            Some("localhost"),
            Some("app.localhost"),
            Some("127.0.0.1"),
            Some("127.8.9.10"),
            Some("::1"),
            Some("::ffff:127.0.0.1"),
            Some("192.168.1.20"),
            Some("192.168.1.21"),
            Some("bageo.local"),
            Some("calls.example.org"),
        ] {
            assert!(host_allowed(ok, &rules), "{ok:?}");
        }
        for bad in [
            "evil.example",
            "bageo.local.evil.example",
            "localhost.evil.example",
            "evil-localhost",
            "",
            "0.0.0.0",
            "192.168.1.22",
            "other.local",
            "127.1",
            "2130706433",
        ] {
            assert!(!host_allowed(Some(bad), &rules), "{bad:?}");
        }
        // The network name turned off: only addresses and localhost.
        let off = HostRules { mdns: String::new(), ..HostRules::default() };
        assert!(!host_allowed(Some("bageo.local"), &off));
        assert!(!host_allowed(Some(".local"), &off));
        assert!(host_allowed(Some("localhost"), &off));
        for l in crate::locale::ALL {
            let (title, detail) = bad_host_words(l);
            assert!(!title.is_empty() && detail.len() > title.len(), "{l:?}");
        }
    }

    #[test]
    fn wrong_passwords_slow_down_then_wait_and_a_right_one_starts_over() {
        let a = ip("198.51.100.7");
        let b = ip("198.51.100.8");
        let t0 = Instant::now();
        let mut t = Tries::default();
        for n in 1..MAX_TRIES {
            assert_eq!(t.failed(a, u64::from(n), t0), n);
            assert_eq!(t.locked(a, t0), None);
        }
        // The same wrong password again (parallel requests) counts once.
        assert_eq!(t.failed(a, u64::from(MAX_TRIES - 1), t0), MAX_TRIES - 1);
        assert_eq!(t.failed(a, 1000, t0), MAX_TRIES);
        assert_eq!(t.locked(a, t0 + Duration::from_secs(10)), Some(TRY_LOCK - Duration::from_secs(10)));
        // Another address is not held back.
        assert_eq!(t.locked(b, t0), None);
        assert_eq!(t.failed(b, 1, t0), 1);
        // After the wait: a fresh start.
        assert_eq!(t.locked(a, t0 + TRY_LOCK), None);
        assert_eq!(t.failed(a, 1, t0 + TRY_LOCK), 1);
        // Tries older than the window are forgotten.
        let t1 = t0 + TRY_LOCK;
        for n in 2..=4 {
            t.failed(a, n, t1);
        }
        assert_eq!(t.failed(a, 99, t1 + TRY_WINDOW), 1);
        // A right password starts over.
        t.failed(a, 100, t1 + TRY_WINDOW);
        t.succeeded(a);
        assert_eq!(t.failed(a, 101, t1 + TRY_WINDOW), 1);
    }

    #[test]
    fn only_requests_carrying_a_password_are_counted() {
        assert!(own_token_route("/ws", false));
    }

    #[test]
    fn forwarded_addresses_count_only_through_a_proxy_on_this_computer() {
        let conn = |peer: &str, local: &str| {
            let mut e = Extensions::new();
            e.insert(ConnectInfo(ConnAddrs { peer: peer.parse().unwrap(), local: Some(local.parse().unwrap()) }));
            e
        };
        let mut h = HeaderMap::new();
        h.append("x-forwarded-for", HeaderValue::from_static("10.9.9.9, 203.0.113.9"));
        // A proxy on this computer: the address it added last (the first one anyone could write).
        assert_eq!(try_key(&conn("127.0.0.1:5000", "127.0.0.1:8787"), &h), Some(ip("203.0.113.9")));
        assert_eq!(try_key(&conn("192.168.1.20:5000", "192.168.1.20:80"), &h), Some(ip("203.0.113.9")));
        // Anyone else (a NAS proxy, or a forged header): the address that connected.
        assert_eq!(try_key(&conn("192.168.1.30:5000", "192.168.1.20:80"), &h), Some(ip("192.168.1.30")));
        let mut real = HeaderMap::new();
        real.insert("x-real-ip", HeaderValue::from_static("198.51.100.4"));
        assert_eq!(try_key(&conn("127.0.0.1:5000", "127.0.0.1:8787"), &real), Some(ip("198.51.100.4")));
        let mut junk = HeaderMap::new();
        junk.insert("x-forwarded-for", HeaderValue::from_static("unknown"));
        assert_eq!(try_key(&conn("127.0.0.1:5000", "127.0.0.1:8787"), &junk), Some(ip("127.0.0.1")));
        assert_eq!(try_key(&Extensions::new(), &h), None);
    }

    #[test]
    fn only_requests_carrying_a_password_are_counted_too() {
        let mut h = HeaderMap::new();
        assert_eq!(credential(&h, None, None), None);
        assert_eq!(credential(&h, Some("q=1&token="), None), None, "an empty token is no password");
        assert_eq!(credential(&h, None, Some("  ")), None, "nor an empty body token");
        assert!(credential(&h, None, Some("abc")).is_some(), "a chat command's body token counts");
        assert_ne!(credential(&h, None, Some("abc")), credential(&h, None, Some("abd")));
        let q = credential(&h, Some("token=abc"), None);
        assert!(q.is_some());
        assert_eq!(q, credential(&h, Some("x=1&token=abc"), None));
        assert_ne!(q, credential(&h, Some("token=abd"), None));
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer abc"));
        assert!(credential(&h, None, None).is_some());
        let mut c = HeaderMap::new();
        c.insert("x-calls-token", HeaderValue::from_static("abc"));
        assert!(credential(&c, None, None).is_some());
        assert!(own_token_route("/ws", false) && own_token_route("/", true) && own_token_route("/mcp", false));
        assert!(!own_token_route("/", false) && !own_token_route("/calls/api/list", false));
    }

    #[tokio::test]
    async fn a_chat_command_body_token_is_read_and_the_body_kept() {
        let body = r#"{"token":" abc ","text":"/calls"}"#;
        let req = axum::http::Request::builder().uri(INBOUND_PATH).body(axum::body::Body::from(body)).unwrap();
        let (req, tok) = body_token(req).await.unwrap();
        assert_eq!(tok.as_deref(), Some("abc"));
        let kept = axum::body::to_bytes(req.into_body(), 1024).await.unwrap();
        assert_eq!(&kept[..], body.as_bytes(), "the handler still reads the whole body");
        let req = axum::http::Request::builder().uri(INBOUND_PATH).body(axum::body::Body::from("not json")).unwrap();
        assert_eq!(body_token(req).await.unwrap().1, None);
    }

    #[test]
    fn ics_paths_more() {
        assert_eq!(ics_what("/calls/api/call/abc/event.ics"), Some("call/abc"));
        assert_eq!(ics_what("/calls/api/tasks/t-1/event.ics"), Some("tasks/t-1"));
        assert_eq!(ics_what("/calls/api/other/abc/event.ics"), None);
        assert_eq!(ics_what("/calls/api/call//event.ics"), None);
        assert_eq!(ics_what("/calls/api/call/a/b/event.ics"), None);
        assert_eq!(ics_what("/calls/api/call/abc"), None);
    }

    #[test]
    fn page_follows_the_browser_in_nine_languages() {
        assert_eq!(page_lang(Some("zh-TW,zh;q=0.9,en;q=0.8")), Some(Lang::ZhTw));
        assert_eq!(page_lang(Some("zh-Hant-HK")), Some(Lang::ZhTw));
        assert_eq!(page_lang(Some("zh-CN,zh;q=0.9")), Some(Lang::ZhCn));
        assert_eq!(page_lang(Some("it-IT,it;q=0.9,de;q=0.5")), Some(Lang::De));
        assert_eq!(page_lang(Some("pt-BR")), Some(Lang::Pt));
        assert_eq!(page_lang(Some("ja")), Some(Lang::Ja));
        assert_eq!(page_lang(Some("ko-KR")), Some(Lang::Ko));
        assert_eq!(page_lang(Some("es-419")), Some(Lang::Es));
        assert_eq!(page_lang(Some("fr-CA")), Some(Lang::Fr));
        assert_eq!(page_lang(Some("en-GB")), Some(Lang::En));
        assert_eq!(page_lang(Some("it, ru")), None);
        assert_eq!(page_lang(None), None);
        for l in crate::locale::ALL {
            let (code, title, detail) = page_words(l);
            assert!(!code.is_empty() && !title.is_empty() && detail.contains("Bageo"), "{l:?}");
        }
    }
}
