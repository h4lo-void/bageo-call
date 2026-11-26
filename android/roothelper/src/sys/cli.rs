// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The command line, for the module's scripts and for checking a phone by hand (as
//! root, e.g. `adb shell su -c ...`).

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::server;
use crate::control::Prober;

const USAGE: &str = "usage: bageo-audio-helper <command>
  serve                      connect to the Bageo app and serve it (started by the module)
  restore                    put back the audio settings of a session nobody ended
  probe                      print this phone's audio facts as JSON (read-only)
  mixer list <card>          print every control of a sound card with its value (read-only)
  mixer get <card> <control> print one control (read-only)
  mixer try <card> <control> <seconds> <value>...
                             change a control for a few seconds, then put it back
  params get <key>           print an audio parameter (read-only)
  version                    print the helper and protocol version
A control is its name, <name>@<index>, or numid=<n>.";

fn run_one(fields: &[&str]) -> i32 {
    let fields: Vec<String> = fields.iter().map(|s| s.to_string()).collect();
    let mut d = server::dispatcher(Arc::new(Mutex::new(())));
    match d.handle(&fields) {
        Ok(out) => {
            println!("{out}");
            0
        }
        Err(e) => {
            eprintln!("bageo-audio-helper: {e}");
            1
        }
    }
}

fn restore() -> i32 {
    match server::take_lock() {
        Ok(Some(_lock)) => {
            match server::restore_state_file() {
                Some(r) => {
                    server::log_report("restore", &r);
                    println!("{}", r.json().render());
                }
                None => println!("nothing to put back"),
            }
            0
        }
        Ok(None) => {
            println!("the helper is running; it puts its own changes back");
            0
        }
        Err(e) => {
            eprintln!("bageo-audio-helper: {e}");
            1
        }
    }
}

/// `mixer try`: a session of its own, as the app would run one, held for a few seconds.
/// A stop signal or `kill -9` during the wait is how the clean-up paths are checked.
fn mixer_try(card: &str, control: &str, seconds: &str, values: &[&str]) -> i32 {
    let secs = match seconds.parse::<u64>() {
        Ok(s) if s <= 600 => s,
        _ => {
            eprintln!("bageo-audio-helper: seconds must be 0 to 600");
            return 2;
        }
    };
    let set = server::block_stop_signals();
    let guard = Arc::new(Mutex::new(()));
    let _lock = match server::take_lock() {
        Ok(Some(l)) => l,
        Ok(None) => {
            eprintln!("bageo-audio-helper: the helper is running; try again when it has quit");
            return 1;
        }
        Err(e) => {
            eprintln!("bageo-audio-helper: {e}");
            return 1;
        }
    };
    server::spawn_signal_thread(set, Arc::clone(&guard));
    if let Some(r) = server::restore_state_file() {
        server::log_report("left over from the last run", &r);
    }
    let mut d = server::dispatcher(guard);
    let f = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<String>>();
    let step = |d: &mut server::SysDispatcher, label: &str, v: Vec<String>| match d.handle(&v) {
        Ok(out) => {
            println!("{label}: {out}");
            true
        }
        Err(e) => {
            eprintln!("bageo-audio-helper: {label}: {e}");
            false
        }
    };
    if !step(&mut d, "before", f(&["mixer.get", card, control])) {
        return 1;
    }
    let pid = std::process::id().to_string();
    if !step(&mut d, "session", f(&["session.begin", &format!("cli-{pid}")])) {
        return 1;
    }
    let mut set_cmd = f(&["mixer.set", card, control]);
    set_cmd.extend(values.iter().map(|s| s.to_string()));
    let changed = step(&mut d, "changed", set_cmd);
    if changed {
        println!("holding for {secs} s (pid {pid})");
        thread::sleep(Duration::from_secs(secs));
    }
    step(&mut d, "put back", f(&["session.end"]));
    step(&mut d, "after", f(&["mixer.get", card, control]));
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
        &["restore"] => restore(),
        &["probe"] => {
            println!("{}", super::probe::SysProber.probe().render());
            0
        }
        &["mixer", "list", card] => run_one(&["mixer.list", card]),
        &["mixer", "get", card, control] => run_one(&["mixer.get", card, control]),
        ["mixer", "try", card, control, seconds, values @ ..] if !values.is_empty() => {
            mixer_try(card, control, seconds, values)
        }
        &["params", "get", key] => run_one(&["params.get", key]),
        &["version"] | &["--version"] => run_one(&["version"]),
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}
