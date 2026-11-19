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

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::extract::connect_info::{Connected, MockConnectInfo};
use axum::extract::{ConnectInfo, Request};
use axum::http::{header, Extensions, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::serve::IncomingStream;
use axum::{Json, Router};
use serde_json::json;

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
        | "/mcp" => return Gate::Pass,
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
pub fn guard(router: Router, settings: Settings, store: Arc<SettingsStore>) -> Router {
    let settings = Arc::new(settings);
    router.layer(axum::middleware::from_fn(move |req: Request, next: Next| {
        let settings = settings.clone();
        let store = store.clone();
        async move {
            if from_this_computer(req.extensions(), req.headers()) {
                return next.run(req).await;
            }
            let path = req.uri().path().to_string();
            let query = req.uri().query();
            let token = query_param(query, "token");
            let rt = store.snapshot();
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
    let (html_lang, title, detail) = page_words(lang);
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
