// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The commands the app may send, and the loop that serves one connection. Only these
//! exist; reading the mixer, parameters or the phone's audio facts is not among them
//! (that is the root command line's, for people checking a phone).
//!
//! | command | fields after it | reply |
//! |---|---|---|
//! | `auth` | the app key (hex) | `ok`; must be the first request of every connection |
//! | `ping` | | `pong` |
//! | `version` | | `<protocol> <helper version>` |
//! | `session.begin` | id | `ok` |
//! | `session.end` | | JSON report (counts only) |
//! | `mixer.set` | card, control, value ... | JSON control after the change |
//! | `params.set` | key, value | `ok` |
//! | `pcm.open` | capture or playback, card, device, rate, channels, period frames, periods | JSON stream |
//! | `pcm.flush` | stream id | `ok`: queued audio dropped, stream started again |
//! | `pcm.close` | stream id | `ok` |
//!
//! `mixer.set`, `params.set` and `pcm.open` need an open session and accept only what
//! the helper's built-in list ([`crate::allow`]) names for this phone; that list is empty
//! for now. `pcm.open` also needs the phone to be in a call, which the helper checks
//! for itself. A control is `numid=<n>` or its name (`<name>@<index>` for a later one
//! of the same name; a name several controls share must be given as `numid=`).
//!
//! Time limits: a connection must authenticate within 5 seconds; without a session it
//! may stay quiet 5 minutes; a session lasts at most 4 hours; every frame, once begun,
//! must arrive whole within 5 seconds. Then the connection ends and is cleaned up.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::allow::Allowlist;
use crate::auth::Keys;
use crate::json::Json;
use crate::mixer::{self, CtlInfo, CtlKey, CtlType, Selector};
use crate::params;
use crate::proto::{self, Conn, Kind, ProtoError};
use crate::session::{Failure, Origin, Report, Restorer, Saved, Session};

pub const AUTH_TIME: Duration = Duration::from_secs(5);
pub const QUIET_TIME: Duration = Duration::from_secs(5 * 60);
pub const SESSION_TIME: Duration = Duration::from_secs(4 * 60 * 60);

pub trait MixerBackend {
    fn find(&mut self, card: u32, sel: &Selector) -> Result<CtlInfo, String>;
    /// The control a saved key names, found by the card's ID string.
    fn find_key(&mut self, key: &CtlKey) -> Result<CtlInfo, Failure>;
    fn card_id(&mut self, card: u32) -> Result<String, String>;
    fn read(&mut self, info: &CtlInfo) -> Result<Vec<i64>, String>;
    fn write(&mut self, info: &CtlInfo, values: &[i64]) -> Result<(), String>;
}

pub trait ParamBackend {
    /// `None`: the phone does not report this key.
    fn get(&mut self, key: &str) -> Result<Option<String>, String>;
    fn set(&mut self, key: &str, value: &str) -> Result<(), String>;
}

/// Whether the phone is in a call (the helper's own check).
pub trait CallState {
    fn in_call(&mut self) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Capture,
    Playback,
}

impl Direction {
    pub fn name(self) -> &'static str {
        match self {
            Direction::Capture => "capture",
            Direction::Playback => "playback",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PcmRequest {
    pub dir: Direction,
    pub card: u32,
    pub device: u32,
    pub rate: u32,
    pub channels: u32,
    pub period_frames: u32,
    pub periods: u32,
}

pub const PCM_RATES: &[u32] = &[8000, 16000, 32000, 44100, 48000];
pub const MAX_PCM_DEVICE: u32 = 255;

fn num(s: &str, what: &str) -> Result<u32, String> {
    if s.is_empty() || s.len() > 6 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("bad {what}"));
    }
    s.parse::<u32>().map_err(|_| format!("bad {what}"))
}

/// `pcm.open` fields: direction, card, device, rate, channels, period frames, periods.
pub fn parse_pcm(f: &[String]) -> Result<PcmRequest, String> {
    if f.len() != 7 {
        return Err("pcm.open needs 7 fields".into());
    }
    let dir = match f[0].as_str() {
        "capture" => Direction::Capture,
        "playback" => Direction::Playback,
        _ => return Err("bad direction".into()),
    };
    let req = PcmRequest {
        dir,
        card: mixer::parse_card(&f[1])?,
        device: num(&f[2], "device")?,
        rate: num(&f[3], "rate")?,
        channels: num(&f[4], "channel count")?,
        period_frames: num(&f[5], "period")?,
        periods: num(&f[6], "period count")?,
    };
    if req.device > MAX_PCM_DEVICE {
        return Err("bad device".into());
    }
    if !PCM_RATES.contains(&req.rate) {
        return Err("rate not supported".into());
    }
    if !(1..=8).contains(&req.channels) {
        return Err("bad channel count".into());
    }
    if !(16..=16384).contains(&req.period_frames) {
        return Err("bad period".into());
    }
    if !(2..=32).contains(&req.periods) {
        return Err("bad period count".into());
    }
    Ok(req)
}

/// Opens and closes PCM streams; each stream's data goes over its own connection.
pub trait PcmHost {
    fn open(&mut self, req: &PcmRequest) -> Result<Json, String>;
    fn flush(&mut self, id: &str) -> Result<(), String>;
    fn close(&mut self, id: &str) -> Result<(), String>;
    fn close_all(&mut self);
}

/// Who may change what: the app only what the built-in list names; root on the
/// command line any control it can put back (no parameters' defaults, no PCM).
pub enum Policy {
    App,
    Root,
}

/// The phone's mixer and audio parameters, and the built-in list that also decides
/// what may be put back.
pub struct Hardware<M, P> {
    pub mixer: M,
    pub params: P,
    pub allow: Allowlist,
}

impl<M: MixerBackend, P: ParamBackend> Restorer for Hardware<M, P> {
    fn write_ctl(&mut self, key: &CtlKey, values: &[i64], origin: Origin) -> Result<(), Failure> {
        let info = self.mixer.find_key(key)?;
        if origin == Origin::App && self.allow.ctl_rule(&key.card_id, &info).is_none() {
            return Err(Failure::NotAllowed);
        }
        if !info.settable() || info.count as usize != values.len() {
            return Err(Failure::Changed);
        }
        let fits = match info.ty {
            CtlType::Boolean => values.iter().all(|v| (0..=1).contains(v)),
            CtlType::Integer | CtlType::Integer64 => {
                info.min > info.max || values.iter().all(|v| (info.min..=info.max).contains(v))
            }
            CtlType::Enumerated => values.iter().all(|&v| v >= 0 && (v as usize) < info.items.len()),
            _ => false,
        };
        if !fits {
            return Err(Failure::Changed);
        }
        self.mixer.write(&info, values).map_err(|_| Failure::Device)
    }

    fn set_param(&mut self, key: &str, value: &str, origin: Origin) -> Result<(), Failure> {
        if origin == Origin::App && self.allow.param_rule(key).is_none() {
            return Err(Failure::NotAllowed);
        }
        if !params::valid_key(key) || !params::valid_value(value) {
            return Err(Failure::Changed);
        }
        self.params.set(key, value).map_err(|_| Failure::Device)
    }
}

pub struct Dispatcher<M, P, H, C> {
    pub hw: Hardware<M, P>,
    pub pcm: H,
    pub calls: C,
    pub session: Session,
    keys: Keys,
    policy: Policy,
    authed: bool,
    /// Held while anything is changed or put back; the signal handler takes it too.
    guard: Arc<Mutex<()>>,
}

fn need(f: &[String], n: usize, cmd: &str) -> Result<(), String> {
    if f.len() == n {
        Ok(())
    } else {
        Err(format!("{cmd} needs {} field(s)", n.saturating_sub(1)))
    }
}

const NOT_ALLOWED: &str = "not allowed on this phone";

impl<M: MixerBackend, P: ParamBackend, H: PcmHost, C: CallState> Dispatcher<M, P, H, C> {
    pub fn new(
        hw: Hardware<M, P>,
        pcm: H,
        calls: C,
        session: Session,
        keys: Keys,
        policy: Policy,
        guard: Arc<Mutex<()>>,
    ) -> Self {
        Dispatcher { hw, pcm, calls, session, keys, policy, authed: false, guard }
    }

    /// For the root command line, which needs no key.
    pub fn authenticated(mut self) -> Self {
        self.authed = true;
        self
    }

    pub fn is_authed(&self) -> bool {
        self.authed
    }

    /// A new connection must present the key again.
    pub fn new_connection(&mut self) {
        self.authed = false;
    }

    /// Runs one request (its fields); `Ok`: the reply, `Err`: the error text.
    pub fn handle(&mut self, f: &[String]) -> Result<String, String> {
        let guard = Arc::clone(&self.guard);
        let _held = guard.lock().unwrap_or_else(|e| e.into_inner());
        let cmd = f.first().map(String::as_str).unwrap_or("");
        if !self.authed {
            if cmd == "auth" && f.len() == 2 && self.keys.app_key_ok(&f[1]) {
                self.authed = true;
                return Ok("ok".into());
            }
            return Err("not authenticated".into());
        }
        match cmd {
            "ping" => {
                need(f, 1, cmd)?;
                Ok("pong".into())
            }
            "version" => {
                need(f, 1, cmd)?;
                Ok(format!("{} {}", proto::VERSION, env!("CARGO_PKG_VERSION")))
            }
            "session.begin" => {
                need(f, 2, cmd)?;
                self.session.begin(&f[1])?;
                Ok("ok".into())
            }
            "session.end" => {
                need(f, 1, cmd)?;
                self.pcm.close_all();
                let report = self.session.end(&mut self.hw).ok_or("no session")?;
                Ok(report.json().render())
            }
            "mixer.set" => self.mixer_set(f),
            "params.set" => self.params_set(f),
            "pcm.open" => {
                if !self.session.is_open() {
                    return Err("no session: send session.begin first".into());
                }
                let req = parse_pcm(&f[1..])?;
                if !matches!(self.policy, Policy::App) {
                    return Err(NOT_ALLOWED.into());
                }
                let card_id = self.hw.mixer.card_id(req.card)?;
                if !self.hw.allow.pcm_ok(&card_id, &req) {
                    return Err(NOT_ALLOWED.into());
                }
                if !self.calls.in_call() {
                    return Err("the phone is not in a call".into());
                }
                Ok(self.pcm.open(&req)?.render())
            }
            "pcm.flush" => {
                need(f, 2, cmd)?;
                self.pcm.flush(&f[1])?;
                Ok("ok".into())
            }
            "pcm.close" => {
                need(f, 2, cmd)?;
                self.pcm.close(&f[1])?;
                Ok("ok".into())
            }
            _ => Err("unknown command".into()),
        }
    }

    fn mixer_set(&mut self, f: &[String]) -> Result<String, String> {
        if f.len() < 4 {
            return Err("mixer.set needs a card, a control and a value".into());
        }
        if !self.session.is_open() {
            return Err("no session: send session.begin first".into());
        }
        let card = mixer::parse_card(&f[1])?;
        let info = self.hw.mixer.find(card, &mixer::parse_selector(&f[2])?)?;
        let values = mixer::parse_set_values(&info, &f[3..])?;
        let card_id = self.hw.mixer.card_id(card)?;
        if matches!(self.policy, Policy::App) {
            let rule = self.hw.allow.ctl_rule(&card_id, &info).ok_or(NOT_ALLOWED)?;
            if !Allowlist::ctl_values_ok(rule, &info, &values) {
                return Err(NOT_ALLOWED.into());
            }
        }
        let old = self.hw.mixer.read(&info)?;
        let mut key = info.key.clone();
        key.card_id = card_id;
        self.session.remember(Saved::Mixer { key, values: old })?;
        self.hw.mixer.write(&info, &values)?;
        let now = self.hw.mixer.read(&info);
        Ok(mixer::info_json(&info, Some(now)).render())
    }

    fn params_set(&mut self, f: &[String]) -> Result<String, String> {
        if f.len() != 3 {
            return Err("params.set needs a key and a value".into());
        }
        if !self.session.is_open() {
            return Err("no session: send session.begin first".into());
        }
        let (key, value) = (&f[1], &f[2]);
        if !params::valid_key(key) || !params::valid_value(value) {
            return Err("bad parameter".into());
        }
        let default = match self.policy {
            Policy::App => {
                let rule = self.hw.allow.param_rule(key).ok_or(NOT_ALLOWED)?;
                if !rule.values.contains(&value.as_str()) {
                    return Err(NOT_ALLOWED.into());
                }
                rule.default
            }
            Policy::Root => None,
        };
        let old = match self.hw.params.get(key)? {
            Some(v) if params::valid_value(&v) => v,
            _ => default.map(str::to_string).ok_or("the phone does not report this value, so it cannot be put back")?,
        };
        self.session.remember(Saved::Param { key: key.clone(), value: old })?;
        self.hw.params.set(key, value)?;
        Ok("ok".into())
    }

    /// The connection is gone: streams closed, the session put back.
    pub fn on_disconnect(&mut self) -> Option<Report> {
        let guard = Arc::clone(&self.guard);
        let _held = guard.lock().unwrap_or_else(|e| e.into_inner());
        self.pcm.close_all();
        self.session.end(&mut self.hw)
    }

    /// How long the next request may take to begin; `None`: the session is out of time.
    fn wait_limit(&self) -> Option<Duration> {
        if !self.authed {
            return Some(AUTH_TIME);
        }
        match self.session.age() {
            Some(age) => SESSION_TIME.checked_sub(age).filter(|d| !d.is_zero()),
            None => Some(QUIET_TIME),
        }
    }

    /// Serves one connection until the app closes it, it runs out of time, it fails to
    /// authenticate, or it sends something that breaks the framing; then closes the
    /// streams and puts the session back.
    pub fn serve<S: Conn>(&mut self, conn: &mut S) -> Option<Report> {
        while let Some(limit) = self.wait_limit() {
            match proto::read_frame_timed(conn, proto::MAX_REQUEST, Some(limit)) {
                Ok(None) => break,
                Ok(Some((Kind::Request, payload))) => {
                    let reply = match proto::parse_request(&payload) {
                        Ok(fields) => self.handle(&fields),
                        Err(e) => Err(e.message()),
                    };
                    let sent = match reply {
                        Ok(text) if text.len() <= proto::MAX_REPLY => {
                            proto::write_frame(conn, Kind::Ok, text.as_bytes())
                        }
                        Ok(_) => proto::write_frame(conn, Kind::Error, b"reply too long"),
                        Err(e) => proto::write_frame(conn, Kind::Error, e.as_bytes()),
                    };
                    if sent.is_err() || !self.authed {
                        break;
                    }
                }
                Ok(Some(_)) => {
                    if proto::write_frame(conn, Kind::Error, b"only requests are accepted").is_err() || !self.authed {
                        break;
                    }
                }
                Err(e) => {
                    if !matches!(e, ProtoError::Io(_)) {
                        let _ = proto::write_frame(conn, Kind::Error, e.message().as_bytes());
                    }
                    break;
                }
            }
        }
        self.on_disconnect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allow::tests::TEST_RULES;
    use crate::allow::RULES;
    use crate::auth::{hex, test_keys};
    use crate::mixer::test_ctl;
    use crate::session::tests::temp_dir;
    use std::collections::HashMap;
    use std::io::{self, Cursor, Read, Write};

    #[derive(Default)]
    struct FakeMixer {
        ctls: Vec<CtlInfo>,
        values: HashMap<u32, Vec<i64>>,
    }

    impl FakeMixer {
        fn phone() -> FakeMixer {
            let mut m = FakeMixer::default();
            for (i, (name, ty, count, v)) in [
                ("Incall Mic Mute", CtlType::Boolean, 1, vec![0]),
                ("Incall Capture Stream0", CtlType::Enumerated, 1, vec![0]),
                ("Voice Rx Device Mute", CtlType::Integer, 3, vec![0, 0, 0]),
                ("Speaker Volume", CtlType::Integer, 1, vec![50]),
            ]
            .into_iter()
            .enumerate()
            {
                let numid = i as u32 + 1;
                m.ctls.push(test_ctl(numid, name, ty, count));
                m.values.insert(numid, v);
            }
            m
        }
        fn value(&self, name: &str) -> Vec<i64> {
            let c = self.ctls.iter().find(|c| c.key.name == name).unwrap();
            self.values[&c.numid].clone()
        }
    }

    impl MixerBackend for FakeMixer {
        fn find(&mut self, _card: u32, sel: &Selector) -> Result<CtlInfo, String> {
            self.ctls
                .iter()
                .find(|c| sel.matches(c.numid, &c.key.name, c.key.index))
                .cloned()
                .ok_or_else(|| "no such control".into())
        }
        fn find_key(&mut self, key: &CtlKey) -> Result<CtlInfo, Failure> {
            self.ctls
                .iter()
                .find(|c| c.key.name == key.name && c.key.card_id == key.card_id)
                .cloned()
                .ok_or(Failure::Gone)
        }
        fn card_id(&mut self, _card: u32) -> Result<String, String> {
            Ok("testcard".into())
        }
        fn read(&mut self, info: &CtlInfo) -> Result<Vec<i64>, String> {
            Ok(self.values[&info.numid].clone())
        }
        fn write(&mut self, info: &CtlInfo, values: &[i64]) -> Result<(), String> {
            self.values.insert(info.numid, values.to_vec());
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeParams {
        values: HashMap<String, String>,
    }

    impl ParamBackend for FakeParams {
        fn get(&mut self, key: &str) -> Result<Option<String>, String> {
            Ok(self.values.get(key).cloned())
        }
        fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
            self.values.insert(key.into(), value.into());
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakePcm {
        open: Vec<String>,
        closed_all: usize,
        flushed: Vec<String>,
    }

    impl PcmHost for FakePcm {
        fn open(&mut self, req: &PcmRequest) -> Result<Json, String> {
            let id = format!("s{}", self.open.len() + 1);
            self.open.push(id.clone());
            Ok(Json::obj().with("id", Json::Str(id)).with("rate", Json::Num(req.rate as i64)))
        }
        fn flush(&mut self, id: &str) -> Result<(), String> {
            self.flushed.push(id.into());
            Ok(())
        }
        fn close(&mut self, id: &str) -> Result<(), String> {
            let before = self.open.len();
            self.open.retain(|s| s != id);
            if self.open.len() == before {
                Err("no such stream".into())
            } else {
                Ok(())
            }
        }
        fn close_all(&mut self) {
            self.open.clear();
            self.closed_all += 1;
        }
    }

    struct Calls(bool);
    impl CallState for Calls {
        fn in_call(&mut self) -> bool {
            self.0
        }
    }

    type D = Dispatcher<FakeMixer, FakeParams, FakePcm, Calls>;

    fn dispatcher(tag: &str, rules: &'static [crate::allow::PhoneRules], in_call: bool) -> (D, std::path::PathBuf) {
        let dir = temp_dir(tag);
        let path = dir.join("audio_session");
        let mut params = FakeParams::default();
        params.values.insert("incall_music_enabled".into(), "false".into());
        let d = Dispatcher::new(
            Hardware {
                mixer: FakeMixer::phone(),
                params,
                allow: Allowlist::for_phone(rules, "testplatform", "x"),
            },
            FakePcm::default(),
            Calls(in_call),
            Session::new(&path, Origin::App),
            test_keys(),
            Policy::App,
            Arc::new(Mutex::new(())),
        );
        (d, path)
    }

    fn req(f: &[&str]) -> Vec<String> {
        f.iter().map(|s| s.to_string()).collect()
    }

    fn auth() -> Vec<String> {
        req(&["auth", &hex(&test_keys().app)])
    }

    /// The app's end of a connection: what it sends, then what it got back.
    struct Duplex {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }
    impl Read for Duplex {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.input.read(buf)
        }
    }
    impl Write for Duplex {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl Conn for Duplex {
        fn set_read_limit(&mut self, _limit: Option<Duration>) -> io::Result<()> {
            Ok(())
        }
    }

    fn frames(requests: &[&[&str]]) -> Vec<u8> {
        let mut out = Vec::new();
        for r in requests {
            proto::write_frame(&mut out, Kind::Request, &proto::encode_request(r)).unwrap();
        }
        out
    }

    fn replies(out: &[u8]) -> Vec<(Kind, String)> {
        let mut r = Cursor::new(out);
        let mut v = Vec::new();
        while let Some((k, p)) = proto::read_frame(&mut r, proto::MAX_REPLY).unwrap() {
            v.push((k, String::from_utf8(p).unwrap()));
        }
        v
    }

    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn nothing_before_the_key() {
        let (mut d, path) = dispatcher("dispatch-auth", TEST_RULES, true);
        assert_eq!(d.handle(&req(&["ping"])), Err("not authenticated".into()));
        assert!(d.handle(&req(&["auth", &hex(&test_keys().helper)])).is_err(), "the helper key is not the app key");
        assert!(d.handle(&req(&["auth", "00"])).is_err());
        assert_eq!(d.handle(&auth()), Ok("ok".into()));
        assert_eq!(d.handle(&req(&["ping"])), Ok("pong".into()));
        cleanup(&path);
    }

    #[test]
    fn a_wrong_key_ends_the_connection() {
        let (mut d, path) = dispatcher("dispatch-badkey", TEST_RULES, true);
        let mut conn = Duplex { input: Cursor::new(frames(&[&["ping"], &["ping"]])), output: Vec::new() };
        d.serve(&mut conn);
        let got = replies(&conn.output);
        assert_eq!(got.len(), 1, "one refusal, then the connection ends");
        assert_eq!(got[0].0, Kind::Error);
        cleanup(&path);
    }

    #[test]
    fn with_the_built_in_empty_list_every_change_is_refused() {
        let (mut d, path) = dispatcher("dispatch-empty", RULES, true);
        d.handle(&auth()).unwrap();
        d.handle(&req(&["session.begin", "call-1"])).unwrap();
        for r in [
            req(&["mixer.set", "0", "Incall Mic Mute", "1"]),
            req(&["mixer.set", "0", "numid=2", "DL"]),
            req(&["mixer.set", "0", "Voice Rx Device Mute", "1", "100", "20"]),
            req(&["params.set", "incall_music_enabled", "true"]),
            req(&["params.set", "Set_BGS_UL_Mute", "0"]),
            req(&["pcm.open", "capture", "0", "20", "48000", "1", "960", "4"]),
            req(&["pcm.open", "playback", "0", "8", "16000", "2", "320", "4"]),
        ] {
            assert_eq!(d.handle(&r), Err(NOT_ALLOWED.into()), "{r:?}");
        }
        assert_eq!(d.hw.mixer.value("Incall Mic Mute"), vec![0]);
        assert_eq!(d.hw.params.values["incall_music_enabled"], "false");
        assert!(d.pcm.open.is_empty());
        cleanup(&path);
    }

    #[test]
    fn the_list_lets_through_what_it_names_and_nothing_else() {
        let (mut d, path) = dispatcher("dispatch-list", TEST_RULES, true);
        d.handle(&auth()).unwrap();
        d.handle(&req(&["session.begin", "call-1"])).unwrap();
        assert!(d.handle(&req(&["mixer.set", "0", "Incall Mic Mute", "1"])).is_ok());
        assert_eq!(d.handle(&req(&["mixer.set", "0", "Incall Mic Mute", "0"])), Err(NOT_ALLOWED.into()));
        assert!(d.handle(&req(&["mixer.set", "0", "numid=2", "DL"])).is_ok());
        assert_eq!(d.handle(&req(&["mixer.set", "0", "numid=2", "UL"])), Err(NOT_ALLOWED.into()));
        assert!(d.handle(&req(&["mixer.set", "0", "Voice Rx Device Mute", "1", "100", "20"])).is_ok());
        assert_eq!(d.handle(&req(&["mixer.set", "0", "Speaker Volume", "50"])), Err(NOT_ALLOWED.into()));
        assert!(d.handle(&req(&["params.set", "incall_music_enabled", "true"])).is_ok());
        assert_eq!(d.handle(&req(&["params.set", "incall_music_enabled", "maybe"])), Err(NOT_ALLOWED.into()));
        assert_eq!(d.handle(&req(&["params.set", "routing", "2"])), Err(NOT_ALLOWED.into()));
        assert!(d.handle(&req(&["pcm.open", "capture", "0", "20", "48000", "1", "960", "4"])).is_ok());
        assert_eq!(
            d.handle(&req(&["pcm.open", "capture", "0", "21", "48000", "1", "960", "4"])),
            Err(NOT_ALLOWED.into())
        );
        assert_eq!(d.hw.mixer.value("Voice Rx Device Mute"), vec![1, 100, 20]);

        let r = d.handle(&req(&["session.end"])).unwrap();
        assert!(r.starts_with(r#"{"restored":4,"failed":0"#), "{r}");
        assert_eq!(d.pcm.closed_all, 1, "the session's end closes the streams");
        assert_eq!(d.hw.mixer.value("Incall Mic Mute"), vec![0]);
        assert_eq!(d.hw.mixer.value("Incall Capture Stream0"), vec![0]);
        assert_eq!(d.hw.mixer.value("Voice Rx Device Mute"), vec![0, 0, 0]);
        assert_eq!(d.hw.params.values["incall_music_enabled"], "false");
        assert!(!path.exists());
        cleanup(&path);
    }

    #[test]
    fn pcm_needs_a_session_and_a_call() {
        let (mut d, path) = dispatcher("dispatch-call", TEST_RULES, false);
        d.handle(&auth()).unwrap();
        let open = req(&["pcm.open", "capture", "0", "20", "48000", "1", "960", "4"]);
        assert!(d.handle(&open).unwrap_err().contains("session"));
        d.handle(&req(&["session.begin", "call-1"])).unwrap();
        assert_eq!(d.handle(&open), Err("the phone is not in a call".into()));
        d.calls.0 = true;
        assert!(d.handle(&open).is_ok());
        assert_eq!(d.handle(&req(&["pcm.flush", "s1"])), Ok("ok".into()));
        assert_eq!(d.pcm.flushed, vec!["s1"]);
        cleanup(&path);
    }

    #[test]
    fn params_without_a_known_old_value_need_a_listed_default() {
        let (mut d, path) = dispatcher("dispatch-params", TEST_RULES, true);
        d.handle(&auth()).unwrap();
        d.handle(&req(&["session.begin", "call-4"])).unwrap();
        // Not reported by the phone and no default on the list: refused.
        d.hw.params.values.remove("incall_music_enabled");
        assert!(d.handle(&req(&["params.set", "incall_music_enabled", "true"])).is_err());
        // A third field (a caller's "old value") is not accepted.
        assert!(d.handle(&req(&["params.set", "Set_BGS_UL_Mute", "0", "1"])).is_err());
        // The list's default is what is put back.
        d.handle(&req(&["params.set", "Set_BGS_UL_Mute", "0"])).unwrap();
        d.handle(&req(&["session.end"])).unwrap();
        assert_eq!(d.hw.params.values["Set_BGS_UL_Mute"], "1");
        cleanup(&path);
    }

    #[test]
    fn a_broken_connection_puts_everything_back() {
        let (mut d, path) = dispatcher("dispatch-drop", TEST_RULES, true);
        let key = hex(&test_keys().app);
        let mut data = frames(&[
            &["auth", &key],
            &["session.begin", "call-2"],
            &["mixer.set", "0", "Incall Mic Mute", "on"],
            &["pcm.open", "capture", "0", "20", "48000", "1", "960", "4"],
        ]);
        data.extend_from_slice(&[b'B', b'A', proto::VERSION, 1, 50, 0, 0, 0, b'm', b'i']);
        let mut conn = Duplex { input: Cursor::new(data), output: Vec::new() };
        let report = d.serve(&mut conn).unwrap();
        assert_eq!(report.restored, 1);
        assert_eq!(d.hw.mixer.value("Incall Mic Mute"), vec![0]);
        assert_eq!(d.pcm.closed_all, 1);
        assert!(d.pcm.open.is_empty());
        assert!(!path.exists());
        let got = replies(&conn.output);
        assert_eq!(got.len(), 4);
        assert!(got.iter().all(|(k, _)| *k == Kind::Ok), "{got:?}");
        cleanup(&path);
    }

    #[test]
    fn a_closed_connection_puts_everything_back() {
        let (mut d, path) = dispatcher("dispatch-close", TEST_RULES, true);
        let key = hex(&test_keys().app);
        let data = frames(&[&["auth", &key], &["session.begin", "call-3"], &["params.set", "incall_music_enabled", "true"]]);
        let mut conn = Duplex { input: Cursor::new(data), output: Vec::new() };
        d.serve(&mut conn);
        assert_eq!(d.hw.params.values["incall_music_enabled"], "false");
        assert!(!path.exists());
        cleanup(&path);
    }

    #[test]
    fn bad_requests_get_errors_and_the_connection_goes_on() {
        let (mut d, path) = dispatcher("dispatch-bad", TEST_RULES, true);
        let key = hex(&test_keys().app);
        let mut data = frames(&[&["auth", &key], &["rm", "-rf", "/"], &["probe"], &["mixer.get", "0", "x"], &["ping"]]);
        proto::write_frame(&mut data, Kind::Request, b"ping\x00").unwrap();
        proto::write_frame(&mut data, Kind::Ok, b"not a request").unwrap();
        data.extend(frames(&[&["ping"]]));
        data.extend_from_slice(&[b'B', b'A', proto::VERSION, 1, 0, 0, 0, 0x40]);
        data.extend(frames(&[&["ping"]]));
        let mut conn = Duplex { input: Cursor::new(data), output: Vec::new() };
        d.serve(&mut conn);
        let got = replies(&conn.output);
        let kinds: Vec<Kind> = got.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            kinds,
            vec![Kind::Ok, Kind::Error, Kind::Error, Kind::Error, Kind::Ok, Kind::Error, Kind::Error, Kind::Ok, Kind::Error]
        );
        assert_eq!(got[1].1, "unknown command");
        assert_eq!(got[2].1, "unknown command", "reading commands are not for the app");
        assert!(got[8].1.contains("too long"));
        cleanup(&path);
    }

    #[test]
    fn a_new_session_waits_until_the_last_one_is_put_back() {
        let (mut d, path) = dispatcher("dispatch-block", TEST_RULES, true);
        d.handle(&auth()).unwrap();
        d.handle(&req(&["session.begin", "call-1"])).unwrap();
        d.handle(&req(&["mixer.set", "0", "Incall Mic Mute", "1"])).unwrap();
        // The control vanishes before the end: its value cannot be put back.
        d.hw.mixer.ctls.retain(|c| c.key.name != "Incall Mic Mute");
        let r = d.handle(&req(&["session.end"])).unwrap();
        assert!(r.contains(r#""failed":1"#), "{r}");
        assert!(path.exists());
        assert!(d.handle(&req(&["session.begin", "call-2"])).is_err());
        cleanup(&path);
    }

    #[test]
    fn root_on_the_command_line_may_set_any_control_but_not_open_pcm() {
        let dir = temp_dir("dispatch-root");
        let path = dir.join("audio_session");
        let mut d = Dispatcher::new(
            Hardware { mixer: FakeMixer::phone(), params: FakeParams::default(), allow: Allowlist::default() },
            FakePcm::default(),
            Calls(true),
            Session::new(&path, Origin::Root),
            test_keys(),
            Policy::Root,
            Arc::new(Mutex::new(())),
        )
        .authenticated();
        d.handle(&req(&["session.begin", "cli-1"])).unwrap();
        d.handle(&req(&["mixer.set", "0", "Speaker Volume", "60"])).unwrap();
        assert!(d.handle(&req(&["pcm.open", "capture", "0", "20", "48000", "1", "960", "4"])).is_err());
        d.handle(&req(&["session.end"])).unwrap();
        assert_eq!(d.hw.mixer.value("Speaker Volume"), vec![50]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pcm_requests() {
        let ok = parse_pcm(&req(&["playback", "0", "8", "16000", "2", "320", "4"])).unwrap();
        assert_eq!(ok.dir, Direction::Playback);
        assert_eq!((ok.rate, ok.channels, ok.period_frames, ok.periods), (16000, 2, 320, 4));
        for bad in [
            &["record", "0", "8", "16000", "2", "320", "4"][..],
            &["capture", "0", "256", "16000", "1", "320", "4"],
            &["capture", "0", "8", "22050", "1", "320", "4"],
            &["capture", "0", "8", "16000", "0", "320", "4"],
            &["capture", "0", "8", "16000", "1", "8", "4"],
            &["capture", "0", "8", "16000", "1", "320", "1"],
            &["capture", "0", "8", "16000", "1", "-320", "4"],
            &["capture", "0", "8", "16000", "1", "320"],
        ] {
            assert!(parse_pcm(&req(bad)).is_err(), "{bad:?}");
        }
    }
}
