// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The command line, for the module's scripts and for people checking a phone (as root,
//! e.g. `adb shell su -c ...`). The reading commands exist only here, not for the app.

use std::fs;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::alsa::AlsaMixer;
use super::params::AppProcessParams;
use super::{server, DISABLE_FILE, STATE_FILE};
use crate::auth::Keys;
use crate::control::{CallState, Dispatcher, MixerBackend, ParamBackend, PcmHost, PcmRequest, Policy};
use crate::json::Json;
use crate::mixer;
use crate::session::{Origin, Session};

const USAGE: &str = "usage: bageo-audio-helper <command>
  status                     is it running, turned off, or holding settings to put back
  restore [--force]          put back the audio settings of a call nobody ended
                             (--force: also values that failed too often)
  disable                    turn it off: stop it, put everything back, never start
  enable                     allow it to start again (when the app asks)
  probe                      print this phone's audio facts as JSON (read-only)
  mixer list <card>          print every control of a sound card with its value (read-only)
  mixer get <card> <control> print one control (read-only)
  mixer try <card> <control> <seconds> <value>...
                             change a control for a few seconds, then put it back
  params get <key>           print an audio parameter (read-only)
  serve                      serve the Bageo app (started by the module)
  version                    print the helper and protocol version
A control is its name, <name>@<index>, or numid=<n>.";

/// The command line opens no streams.
struct NoPcm;
impl PcmHost for NoPcm {
    fn open(&mut self, _req: &PcmRequest) -> Result<Json, String> {
        Err("not available here".into())
    }
    fn flush(&mut self, _id: &str) -> Result<(), String> {
        Err("no such stream".into())
    }
    fn close(&mut self, _id: &str) -> Result<(), String> {
        Err("no such stream".into())
    }
    fn close_all(&mut self) {}
}

struct NoCalls;
impl CallState for NoCalls {
    fn in_call(&mut self) -> bool {
        false
    }
}

fn fail(e: impl std::fmt::Display) -> i32 {
    eprintln!("bageo-audio-helper: {e}");
    1
}

fn mixer_list(card: &str) -> i32 {
    let card = match mixer::parse_card(card) {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    let mut m = AlsaMixer::new();
    match m.list(card) {
        Ok(list) => {
            let items = list
                .iter()
                .map(|info| {
                    let value = if info.readable() { Some(m.read(info)) } else { None };
                    mixer::info_json(info, value)
                })
                .collect();
            println!("{}", Json::Arr(items).render());
            0
        }
        Err(e) => fail(e),
    }
}

fn mixer_get(card: &str, control: &str) -> i32 {
    let (card, sel) = match (mixer::parse_card(card), mixer::parse_selector(control)) {
        (Ok(c), Ok(s)) => (c, s),
        (Err(e), _) | (_, Err(e)) => return fail(e),
    };
    let mut m = AlsaMixer::new();
    match m.find(card, &sel) {
        Ok(info) => {
            let value = if info.readable() { Some(m.read(&info)) } else { None };
            println!("{}", mixer::info_json(&info, value).render());
            0
        }
        Err(e) => fail(e),
    }
}

fn params_get(key: &str) -> i32 {
    match AppProcessParams::next_to_helper().get(key) {
        Ok(Some(v)) => {
            println!("{v}");
            0
        }
        Ok(None) => fail("the phone does not report this key"),
        Err(e) => fail(e),
    }
}

fn status() -> i32 {
    let running = server::running_pid();
    let app = match server::app_uid() {
        Ok(_) => "accepted".to_string(),
        Err(r) => format!("refused: {}", r.name()),
    };
    let j = Json::obj()
        .with("running", Json::Bool(running.is_some()))
        .with("disabled", Json::Bool(server::disabled()))
        .with("settings_to_put_back", Json::Bool(fs::symlink_metadata(STATE_FILE).is_ok()))
        .with("app", Json::Str(app))
        .with("allowlist_empty", Json::Bool(server::allowlist().is_empty()));
    println!("{}", j.render());
    0
}

fn restore(force: bool) -> i32 {
    match server::take_lock() {
        Ok(Some(_lock)) => {
            match server::restore_state_file(force) {
                Some(r) => {
                    server::log_report("restore", &r);
                    println!("{}", r.json().render());
                    if r.clean() {
                        0
                    } else {
                        1
                    }
                }
                None => {
                    println!("nothing to put back");
                    0
                }
            }
        }
        Ok(None) => {
            println!("the helper is running; stop it first (bageo-audio-helper disable)");
            1
        }
        Err(e) => fail(e.kind()),
    }
}

fn disable() -> i32 {
    if let Err(e) =
        fs::OpenOptions::new().write(true).create(true).truncate(false).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(DISABLE_FILE)
    {
        return fail(e.kind());
    }
    if let Some(pid) = server::running_pid() {
        // SAFETY: kill on the pid the helper wrote, checked to be the helper.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    let start = Instant::now();
    loop {
        match server::take_lock() {
            Ok(Some(_lock)) => {
                let r = server::restore_state_file(true);
                match &r {
                    Some(r) => println!("turned off; {}", r.json().render()),
                    None => println!("turned off; nothing to put back"),
                }
                return if r.is_none_or(|r| r.clean()) { 0 } else { 1 };
            }
            Ok(None) if start.elapsed() < Duration::from_secs(8) => thread::sleep(Duration::from_millis(100)),
            Ok(None) => {
                println!("turned off, but the running helper did not stop; restart the phone");
                return 1;
            }
            Err(e) => return fail(e.kind()),
        }
    }
}

fn enable() -> i32 {
    match fs::remove_file(DISABLE_FILE) {
        Ok(()) => {
            println!("allowed to start again");
            0
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("was not turned off");
            0
        }
        Err(e) => fail(e.kind()),
    }
}

/// `mixer try`: a session of its own, run by root, held for a few seconds. A stop
/// signal or `kill -9` during the wait is how the clean-up paths are checked.
fn mixer_try(card: &str, control: &str, seconds: &str, values: &[&str]) -> i32 {
    let secs = match seconds.parse::<u64>() {
        Ok(s) if s <= 600 => s,
        _ => return fail("seconds must be 0 to 600"),
    };
    let set = server::block_stop_signals();
    let guard = Arc::new(Mutex::new(()));
    let _lock = match server::take_lock() {
        Ok(Some(l)) => l,
        Ok(None) => return fail("the helper is running; try again when it has quit"),
        Err(e) => return fail(e.kind()),
    };
    server::spawn_signal_thread(set, Arc::clone(&guard), None);
    if let Some(r) = server::restore_state_file(false) {
        server::log_report("left over from the last run", &r);
        if !r.clean() {
            return fail("earlier settings are not put back yet; run restore");
        }
    }
    let unused_keys = Keys::from_random([0u8; 64]);
    let mut d = Dispatcher::new(
        server::hardware(),
        NoPcm,
        NoCalls,
        Session::new(STATE_FILE, Origin::Root),
        unused_keys,
        Policy::Root,
        guard,
    )
    .authenticated();
    if mixer_get(card, control) != 0 {
        return 1;
    }
    let pid = std::process::id();
    if let Err(e) = d.handle(&[String::from("session.begin"), format!("cli-{pid}")]) {
        return fail(e);
    }
    let mut set_cmd = vec!["mixer.set".to_string(), card.to_string(), control.to_string()];
    set_cmd.extend(values.iter().map(|s| s.to_string()));
    let changed = match d.handle(&set_cmd) {
        Ok(out) => {
            println!("changed: {out}");
            println!("holding for {secs} s (pid {pid})");
            thread::sleep(Duration::from_secs(secs));
            true
        }
        Err(e) => {
            eprintln!("bageo-audio-helper: {e}");
            false
        }
    };
    match d.handle(&["session.end".to_string()]) {
        Ok(out) => println!("put back: {out}"),
        Err(e) => eprintln!("bageo-audio-helper: {e}"),
    }
    mixer_get(card, control);
    if changed {
        0
    } else {
        1
    }
}

pub fn run(args: Vec<String>) -> i32 {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        &["serve"] => server::serve(),
        &["status"] => status(),
        &["restore"] => restore(false),
        &["restore", "--force"] => restore(true),
        &["disable"] => disable(),
        &["enable"] => enable(),
        &["probe"] => {
            println!("{}", super::probe::probe_json().render());
            0
        }
        &["mixer", "list", card] => mixer_list(card),
        &["mixer", "get", card, control] => mixer_get(card, control),
        ["mixer", "try", card, control, seconds, values @ ..] if !values.is_empty() => {
            mixer_try(card, control, seconds, values)
        }
        &["params", "get", key] => params_get(key),
        &["version"] | &["--version"] => {
            println!("{} {}", crate::proto::VERSION, env!("CARGO_PKG_VERSION"));
            0
        }
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}
