// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Gathers what [`crate::probe`] reports. Read-only: opens the control devices to
//! list names, reads `/proc/asound/pcm` and the vendor's audio XML files.

use std::fs;
use std::io::Read;
use std::path::Path;

use super::alsa::Control;
use super::props;
use crate::abi;
use crate::json::Json;
use crate::mixer::MAX_CARD;
use crate::probe::{self, CardFacts, ProbeInput, MAX_FILES, PROPS};

const XML_DIRS: &[&str] = &["/vendor/etc", "/odm/etc"];
const MAX_DEPTH: usize = 3;
const MAX_FILE_BYTES: u64 = 4 << 20;

fn read_limited(path: &Path, max: u64) -> Option<String> {
    let f = fs::File::open(path).ok()?;
    let mut s = String::new();
    let mut bytes = Vec::new();
    f.take(max).read_to_end(&mut bytes).ok()?;
    s.push_str(&String::from_utf8_lossy(&bytes));
    Some(s)
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<(String, String)>) {
    if out.len() >= MAX_FILES {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if out.len() >= MAX_FILES {
            return;
        }
        let path = e.path();
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        if meta.is_dir() {
            if depth < MAX_DEPTH {
                walk(&path, depth + 1, out);
            }
            continue;
        }
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if probe::file_kind(name).is_none() {
            continue;
        }
        if let Some(text) = read_limited(&path, MAX_FILE_BYTES) {
            out.push((path.to_string_lossy().into_owned(), text));
        }
    }
}

fn card(index: u32) -> Option<CardFacts> {
    if !Path::new(&format!("/dev/snd/controlC{index}")).exists() {
        return None;
    }
    let c = match Control::open(index) {
        Ok(c) => c,
        Err(e) => {
            return Some(CardFacts {
                index,
                id: String::new(),
                driver: String::new(),
                name: String::new(),
                mixer_name: String::new(),
                controls: Err(e.to_string()),
            })
        }
    };
    let ci = c.card_info().ok();
    let controls = c
        .ids()
        .map(|ids| {
            ids.iter()
                .filter_map(|id| c.info(id, false).ok())
                .map(|i| (i.key.name, i.key.index, i.ty.name().to_string(), i.count))
                .collect()
        })
        .map_err(|e| e.to_string());
    Some(CardFacts {
        index,
        id: ci.map(|c| abi::cstr(&c.id)).unwrap_or_default(),
        driver: ci.map(|c| abi::cstr(&c.driver)).unwrap_or_default(),
        name: ci.map(|c| abi::cstr(&c.name)).unwrap_or_default(),
        mixer_name: ci.map(|c| abi::cstr(&c.mixername)).unwrap_or_default(),
        controls,
    })
}

pub fn gather() -> ProbeInput {
    let mut files = Vec::new();
    for d in XML_DIRS {
        walk(Path::new(d), 1, &mut files);
    }
    ProbeInput {
        props: PROPS.iter().filter_map(|k| props::get(k).map(|v| (k.to_string(), v))).collect(),
        cards: (0..=MAX_CARD).filter_map(card).collect(),
        proc_pcm: read_limited(Path::new("/proc/asound/pcm"), 256 * 1024),
        files,
        helper_version: env!("CARGO_PKG_VERSION").to_string(),
        abi: super::abi_name().to_string(),
    }
}

pub fn probe_json() -> Json {
    probe::build(&gather())
}
