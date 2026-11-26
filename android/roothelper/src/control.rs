// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The commands the app may send, and the loop that serves one connection. Only the
//! commands below exist; there is no way to run anything else.
//!
//! | command | fields after it | reply |
//! |---|---|---|
//! | `ping` | | `pong` |
//! | `version` | | `<protocol> <helper version>` |
//! | `probe` | | JSON, see [`crate::probe`] |
//! | `mixer.list` | card | JSON array of controls with values |
//! | `mixer.get` | card, control | JSON control |
//! | `mixer.set` | card, control, value ... | JSON control after the change |
//! | `params.get` | key | value |
//! | `params.set` | key, value [, value to put back] | `ok` |
//! | `session.begin` | id | `ok` |
//! | `session.end` | | JSON `{"restored":n,"failed":[...]}` |
//! | `pcm.open` | capture or playback, card, device, rate, channels, period frames, periods | JSON stream |
//! | `pcm.close` | stream id | `ok` |
//!
//! A control is `numid=<n>` or its name (`<name>@<index>` for a later one of the same
//! name). `mixer.set` and `params.set` need an open session: the old value is saved
//! before the change (see [`crate::session`]). `params.set` needs the third field only
//! when the phone does not report the key's current value.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use crate::json::Json;
use crate::mixer::{self, CtlInfo, CtlKey, Selector};
use crate::params;
use crate::proto::{self, Kind, ProtoError};
use crate::session::{Report, Restorer, Saved, Session};

pub trait MixerBackend {
    fn list(&mut self, card: u32) -> Result<Vec<CtlInfo>, String>;
    fn find(&mut self, card: u32, sel: &Selector) -> Result<CtlInfo, String>;
    fn find_key(&mut self, key: &CtlKey) -> Result<CtlInfo, String>;
    fn read(&mut self, info: &CtlInfo) -> Result<Vec<i64>, String>;
    fn write(&mut self, info: &CtlInfo, values: &[i64]) -> Result<(), String>;
}

pub trait ParamBackend {
    /// `None`: the phone does not report this key.
    fn get(&mut self, key: &str) -> Result<Option<String>, String>;
    fn set(&mut self, key: &str, value: &str) -> Result<(), String>;
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
        return Err(format!("bad {what}: {s}"));
    }
    s.parse::<u32>().map_err(|_| format!("bad {what}: {s}"))
}

/// `pcm.open` fields: direction, card, device, rate, channels, period frames, periods.
pub fn parse_pcm(f: &[String]) -> Result<PcmRequest, String> {
    if f.len() != 7 {
        return Err("pcm.open needs 7 fields".into());
    }
    let dir = match f[0].as_str() {
        "capture" => Direction::Capture,
        "playback" => Direction::Playback,
        other => return Err(format!("bad direction: {other}")),
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
        return Err(format!("bad device: {}", req.device));
    }
    if !PCM_RATES.contains(&req.rate) {
        return Err(format!("rate {} not supported", req.rate));
    }
    if !(1..=8).contains(&req.channels) {
        return Err(format!("bad channel count: {}", req.channels));
    }
    if !(16..=16384).contains(&req.period_frames) {
        return Err(format!("bad period: {}", req.period_frames));
    }
    if !(2..=32).contains(&req.periods) {
        return Err(format!("bad period count: {}", req.periods));
    }
    Ok(req)
}

/// Opens and closes PCM streams; each stream's data goes over its own connection.
pub trait PcmHost {
    fn open(&mut self, req: &PcmRequest) -> Result<Json, String>;
    fn close(&mut self, id: &str) -> Result<(), String>;
    fn close_all(&mut self);
}

pub trait Prober {
    fn probe(&mut self) -> Json;
}

/// The phone's mixer and audio parameters.
pub struct Hardware<M, P> {
    pub mixer: M,
    pub params: P,
}

impl<M: MixerBackend, P: ParamBackend> Restorer for Hardware<M, P> {
    fn write_ctl(&mut self, key: &CtlKey, values: &[i64]) -> Result<(), String> {
        let info = self.mixer.find_key(key)?;
        if !info.settable() || info.count as usize != values.len() {
            return Err("the control changed since its value was saved".into());
        }
        self.mixer.write(&info, values)
    }

    fn set_param(&mut self, key: &str, value: &str) -> Result<(), String> {
        self.params.set(key, value)
    }
}

pub struct Dispatcher<M, P, H, X> {
    pub hw: Hardware<M, P>,
    pub pcm: H,
    pub prober: X,
    pub session: Session,
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

impl<M: MixerBackend, P: ParamBackend, H: PcmHost, X: Prober> Dispatcher<M, P, H, X> {
    pub fn new(hw: Hardware<M, P>, pcm: H, prober: X, session: Session, guard: Arc<Mutex<()>>) -> Self {
        Dispatcher { hw, pcm, prober, session, guard }
    }

    /// Runs one request (its fields); `Ok`: the reply, `Err`: the error text.
    pub fn handle(&mut self, f: &[String]) -> Result<String, String> {
        let guard = Arc::clone(&self.guard);
        let _held = guard.lock().unwrap_or_else(|e| e.into_inner());
        let cmd = f.first().map(String::as_str).unwrap_or("");
        match cmd {
            "ping" => {
                need(f, 1, cmd)?;
                Ok("pong".into())
            }
            "version" => {
                need(f, 1, cmd)?;
                Ok(format!("{} {}", proto::VERSION, env!("CARGO_PKG_VERSION")))
            }
            "probe" => {
                need(f, 1, cmd)?;
                Ok(self.prober.probe().render())
            }
            "mixer.list" => {
                need(f, 2, cmd)?;
                let card = mixer::parse_card(&f[1])?;
                let list = self.hw.mixer.list(card)?;
                let items = list
                    .iter()
                    .map(|info| {
                        let value = if info.readable() { Some(self.hw.mixer.read(info)) } else { None };
                        mixer::info_json(info, value)
                    })
                    .collect();
                Ok(Json::Arr(items).render())
            }
            "mixer.get" => {
                need(f, 3, cmd)?;
                let card = mixer::parse_card(&f[1])?;
                let info = self.hw.mixer.find(card, &mixer::parse_selector(&f[2])?)?;
                let value = if info.readable() { Some(self.hw.mixer.read(&info)) } else { None };
                Ok(mixer::info_json(&info, value).render())
            }
            "mixer.set" => {
                if f.len() < 4 {
                    return Err("mixer.set needs a card, a control and a value".into());
                }
                if self.session.id().is_none() {
                    return Err("no session: send session.begin first".into());
                }
                let card = mixer::parse_card(&f[1])?;
                let info = self.hw.mixer.find(card, &mixer::parse_selector(&f[2])?)?;
                let values = mixer::parse_set_values(&info, &f[3..])?;
                let old = self.hw.mixer.read(&info)?;
                self.session.remember(Saved::Mixer { key: info.key.clone(), values: old })?;
                self.hw.mixer.write(&info, &values)?;
                let now = self.hw.mixer.read(&info);
                Ok(mixer::info_json(&info, Some(now)).render())
            }
            "params.get" => {
                need(f, 2, cmd)?;
                if !params::valid_key(&f[1]) {
                    return Err("bad parameter name".into());
                }
                self.hw.params.get(&f[1])?.ok_or_else(|| format!("the phone does not report {}", f[1]))
            }
            "params.set" => {
                if f.len() != 3 && f.len() != 4 {
                    return Err("params.set needs a key, a value and maybe a value to put back".into());
                }
                if self.session.id().is_none() {
                    return Err("no session: send session.begin first".into());
                }
                let (key, value) = (&f[1], &f[2]);
                if !params::valid_key(key) || !params::valid_value(value) {
                    return Err("bad parameter".into());
                }
                let fallback = f.get(3);
                if fallback.is_some_and(|v| !params::valid_value(v)) {
                    return Err("bad value to put back".into());
                }
                let old = match self.hw.params.get(key)? {
                    Some(v) if params::valid_value(&v) => v,
                    _ => fallback.cloned().ok_or_else(|| {
                        format!("the phone does not report {key}; send the value to put back as a third field")
                    })?,
                };
                self.session.remember(Saved::Param { key: key.clone(), value: old })?;
                self.hw.params.set(key, value)?;
                Ok("ok".into())
            }
            "session.begin" => {
                need(f, 2, cmd)?;
                self.session.begin(&f[1])?;
                Ok("ok".into())
            }
            "session.end" => {
                need(f, 1, cmd)?;
                let report = self.session.end(&mut self.hw).ok_or("no session")?;
                Ok(report.json().render())
            }
            "pcm.open" => {
                let req = parse_pcm(&f[1..])?;
                Ok(self.pcm.open(&req)?.render())
            }
            "pcm.close" => {
                need(f, 2, cmd)?;
                self.pcm.close(&f[1])?;
                Ok("ok".into())
            }
            _ => Err("unknown command".into()),
        }
    }

    /// The connection is gone: streams closed, the session put back.
    pub fn on_disconnect(&mut self) -> Option<Report> {
        let guard = Arc::clone(&self.guard);
        let _held = guard.lock().unwrap_or_else(|e| e.into_inner());
        self.pcm.close_all();
        self.session.end(&mut self.hw)
    }

    /// Serves one connection until the app closes it or sends something that breaks
    /// the framing; then closes the streams and puts the session back.
    pub fn serve<S: Read + Write>(&mut self, stream: &mut S) -> Option<Report> {
        loop {
            match proto::read_frame(stream, proto::MAX_REQUEST) {
                Ok(None) => break,
                Ok(Some((Kind::Request, payload))) => {
                    let reply = match proto::parse_request(&payload) {
                        Ok(fields) => self.handle(&fields),
                        Err(e) => Err(e.message()),
                    };
                    let sent = match reply {
                        Ok(text) if text.len() <= proto::MAX_REPLY => {
                            proto::write_frame(stream, Kind::Ok, text.as_bytes())
                        }
                        Ok(_) => proto::write_frame(stream, Kind::Error, b"reply too long"),
                        Err(e) => proto::write_frame(stream, Kind::Error, e.as_bytes()),
                    };
                    if sent.is_err() {
                        break;
                    }
                }
                Ok(Some(_)) => {
                    if proto::write_frame(stream, Kind::Error, b"only requests are accepted").is_err() {
                        break;
                    }
                }
                Err(e) => {
                    if !matches!(e, ProtoError::Io(_)) {
                        let _ = proto::write_frame(stream, Kind::Error, e.message().as_bytes());
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
    use crate::mixer::{test_ctl, CtlType};
    use crate::session::tests::temp_dir;
    use std::collections::HashMap;
    use std::io::{self, Cursor};

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
        fn list(&mut self, _card: u32) -> Result<Vec<CtlInfo>, String> {
            Ok(self.ctls.clone())
        }
        fn find(&mut self, _card: u32, sel: &Selector) -> Result<CtlInfo, String> {
            self.ctls
                .iter()
                .find(|c| sel.matches(c.numid, &c.key.name, c.key.index))
                .cloned()
                .ok_or_else(|| "no such control".into())
        }
        fn find_key(&mut self, key: &CtlKey) -> Result<CtlInfo, String> {
            self.ctls.iter().find(|c| &c.key == key).cloned().ok_or_else(|| "no such control".into())
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
    }

    impl PcmHost for FakePcm {
        fn open(&mut self, req: &PcmRequest) -> Result<Json, String> {
            let id = format!("s{}", self.open.len() + 1);
            self.open.push(id.clone());
            Ok(Json::obj().with("id", Json::Str(id)).with("rate", Json::Num(req.rate as i64)))
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

    struct NoProbe;
    impl Prober for NoProbe {
        fn probe(&mut self) -> Json {
            Json::obj().with("ok", Json::Bool(true))
        }
    }

    type D = Dispatcher<FakeMixer, FakeParams, FakePcm, NoProbe>;

    fn dispatcher(tag: &str) -> (D, std::path::PathBuf) {
        let dir = temp_dir(tag);
        let path = dir.join("audio_session");
        let mut params = FakeParams::default();
        params.values.insert("incall_music_enabled".into(), "false".into());
        let d = Dispatcher::new(
            Hardware { mixer: FakeMixer::phone(), params },
            FakePcm::default(),
            NoProbe,
            Session::new(&path),
            Arc::new(Mutex::new(())),
        );
        (d, path)
    }

    fn req(f: &[&str]) -> Vec<String> {
        f.iter().map(|s| s.to_string()).collect()
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

    #[test]
    fn changes_need_a_session_and_are_put_back_at_the_end() {
        let (mut d, path) = dispatcher("dispatch-end");
        assert!(d.handle(&req(&["mixer.set", "0", "Incall Mic Mute", "1"])).is_err());
        d.handle(&req(&["session.begin", "call-1"])).unwrap();
        let r = d.handle(&req(&["mixer.set", "0", "Incall Mic Mute", "1"])).unwrap();
        assert!(r.contains(r#""value":[true]"#), "{r}");
        d.handle(&req(&["mixer.set", "0", "numid=2", "UL"])).unwrap();
        d.handle(&req(&["mixer.set", "0", "Voice Rx Device Mute", "1", "100", "20"])).unwrap();
        d.handle(&req(&["params.set", "incall_music_enabled", "true"])).unwrap();
        assert_eq!(d.hw.mixer.value("Voice Rx Device Mute"), vec![1, 100, 20]);
        assert!(path.exists());

        let r = d.handle(&req(&["session.end"])).unwrap();
        assert_eq!(r, r#"{"restored":4,"failed":[]}"#);
        assert_eq!(d.hw.mixer.value("Incall Mic Mute"), vec![0]);
        assert_eq!(d.hw.mixer.value("Incall Capture Stream0"), vec![0]);
        assert_eq!(d.hw.mixer.value("Voice Rx Device Mute"), vec![0, 0, 0]);
        assert_eq!(d.hw.params.values["incall_music_enabled"], "false");
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_broken_connection_puts_everything_back() {
        let (mut d, path) = dispatcher("dispatch-drop");
        let mut data = frames(&[
            &["session.begin", "call-2"],
            &["mixer.set", "0", "Incall Mic Mute", "on"],
            &["pcm.open", "capture", "0", "20", "48000", "1", "960", "4"],
        ]);
        // The app dies in the middle of the next frame.
        data.extend_from_slice(&[b'B', b'A', proto::VERSION, 1, 50, 0, 0, 0, b'm', b'i']);
        let mut conn = Duplex { input: Cursor::new(data), output: Vec::new() };
        let report = d.serve(&mut conn).unwrap();
        assert_eq!(report.restored, 1);
        assert_eq!(d.hw.mixer.value("Incall Mic Mute"), vec![0]);
        assert_eq!(d.pcm.closed_all, 1);
        assert!(d.pcm.open.is_empty());
        assert!(!path.exists());
        let got = replies(&conn.output);
        assert_eq!(got.len(), 3);
        assert!(got.iter().all(|(k, _)| *k == Kind::Ok), "{got:?}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_closed_connection_puts_everything_back() {
        let (mut d, path) = dispatcher("dispatch-close");
        let data = frames(&[&["session.begin", "call-3"], &["params.set", "incall_music_enabled", "true"]]);
        let mut conn = Duplex { input: Cursor::new(data), output: Vec::new() };
        d.serve(&mut conn);
        assert_eq!(d.hw.params.values["incall_music_enabled"], "false");
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn bad_requests_get_errors_and_the_connection_goes_on() {
        let (mut d, path) = dispatcher("dispatch-bad");
        let mut data = frames(&[&["rm", "-rf", "/"], &["mixer.get", "99", "x"], &["ping"]]);
        proto::write_frame(&mut data, Kind::Request, b"ping\x00").unwrap();
        proto::write_frame(&mut data, Kind::Ok, b"not a request").unwrap();
        data.extend(frames(&[&["ping"]]));
        // Then a frame that claims to be 1 GiB: the connection ends without reading it.
        data.extend_from_slice(&[b'B', b'A', proto::VERSION, 1, 0, 0, 0, 0x40]);
        data.extend(frames(&[&["ping"]]));
        let mut conn = Duplex { input: Cursor::new(data), output: Vec::new() };
        d.serve(&mut conn);
        let got = replies(&conn.output);
        let kinds: Vec<Kind> = got.iter().map(|(k, _)| *k).collect();
        assert_eq!(kinds, vec![Kind::Error, Kind::Error, Kind::Ok, Kind::Error, Kind::Error, Kind::Ok, Kind::Error]);
        assert_eq!(got[0].1, "unknown command");
        assert!(got[6].1.contains("too long"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn params_without_a_known_old_value_need_one_to_put_back() {
        let (mut d, path) = dispatcher("dispatch-params");
        d.handle(&req(&["session.begin", "call-4"])).unwrap();
        assert!(d.handle(&req(&["params.set", "Set_BGS_UL_Mute", "0"])).is_err());
        assert!(d.handle(&req(&["params.set", "Set_BGS_UL_Mute", "0", "1;x=2"])).is_err());
        d.handle(&req(&["params.set", "Set_BGS_UL_Mute", "0", "1"])).unwrap();
        assert!(d.handle(&req(&["params.set", "a=b", "0", "1"])).is_err());
        assert_eq!(d.handle(&req(&["params.get", "Set_BGS_UL_Mute"])), Ok("0".into()));
        d.handle(&req(&["session.end"])).unwrap();
        assert_eq!(d.hw.params.values["Set_BGS_UL_Mute"], "1");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn reads_and_lists() {
        let (mut d, path) = dispatcher("dispatch-read");
        assert_eq!(d.handle(&req(&["ping"])), Ok("pong".into()));
        assert!(d.handle(&req(&["ping", "x"])).is_err());
        let list = d.handle(&req(&["mixer.list", "0"])).unwrap();
        assert!(list.starts_with('[') && list.contains("Incall Capture Stream0"));
        let one = d.handle(&req(&["mixer.get", "0", "numid=3"])).unwrap();
        assert!(one.contains(r#""value":[0,0,0]"#));
        assert!(d.handle(&req(&["mixer.get", "0", "Nope"])).is_err());
        assert!(d.handle(&req(&["session.end"])).is_err());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
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
