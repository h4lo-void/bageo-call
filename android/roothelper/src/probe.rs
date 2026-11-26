// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! `probe`: read-only facts about this phone's audio for the app to decide whether and
//! how to use the helper. It reads a fixed list of system properties, the sound cards
//! and their control names, the PCM devices, and the call-related parts of the vendor
//! audio policy and mixer path files. Nothing that identifies the phone or its owner
//! (serial number, IMEI, phone number) is read, and long digit runs are blanked in
//! everything it reports, as a second guard.
//!
//! (Which files and words to look at follows gsm2sip's `tools/check-device.sh`, MIT.)

use crate::json::Json;

/// The only system properties reported.
pub const PROPS: &[&str] = &[
    "ro.board.platform",
    "ro.hardware",
    "ro.hardware.audio.primary",
    "ro.soc.manufacturer",
    "ro.soc.model",
    "ro.product.manufacturer",
    "ro.product.model",
    "ro.product.device",
    "ro.product.board",
    "ro.build.version.release",
    "ro.build.version.sdk",
    "ro.vendor.build.version.sdk",
];

/// Words that mark a call-related port, route or mixer path.
const CALL_WORDS: &[&str] = &["incall", "in_call", "voice", "telephony", "call", "vocrec", "voc_rec"];

/// Most lines reported per file, and most files.
const MAX_MATCHES: usize = 80;
pub const MAX_FILES: usize = 40;
const MAX_LINE: usize = 240;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardFacts {
    pub index: u32,
    pub id: String,
    pub driver: String,
    pub name: String,
    pub mixer_name: String,
    /// Control name, index, type, value count; or why they could not be listed.
    pub controls: Result<Vec<(String, u32, String, u32)>, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PcmDevice {
    pub card: u32,
    pub device: u32,
    pub id: String,
    pub name: String,
    pub playback: bool,
    pub capture: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ProbeInput {
    pub props: Vec<(String, String)>,
    pub cards: Vec<CardFacts>,
    pub proc_pcm: Option<String>,
    /// Policy and mixer path files: path and contents.
    pub files: Vec<(String, String)>,
    pub helper_version: String,
    pub abi: String,
}

/// Blanks every run of 8 or more digits (phone numbers, IMEIs, serials).
pub fn scrub(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut digits = String::new();
    let flush = |digits: &mut String, out: &mut String| {
        if digits.len() >= 8 {
            out.push_str("<number>");
        } else {
            out.push_str(digits);
        }
        digits.clear();
    };
    for c in s.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else {
            flush(&mut digits, &mut out);
            out.push(c);
        }
    }
    flush(&mut digits, &mut out);
    out
}

fn clip(s: &str) -> String {
    let t = s.trim();
    let mut end = t.len().min(MAX_LINE);
    while !t.is_char_boundary(end) {
        end -= 1;
    }
    scrub(&t[..end])
}

/// `/proc/asound/pcm` lines: `00-20: Incall Capture Stream0 (*) :  : capture 1`.
pub fn parse_proc_pcm(text: &str) -> Vec<PcmDevice> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((num, rest)) = line.split_once(':') else { continue };
        let Some((c, d)) = num.trim().split_once('-') else { continue };
        let (Ok(card), Ok(device)) = (c.parse::<u32>(), d.parse::<u32>()) else { continue };
        let parts: Vec<&str> = rest.split(" : ").map(str::trim).collect();
        let id = parts.first().copied().unwrap_or("").trim_end_matches("(*)").trim().to_string();
        let name = parts.get(1).copied().unwrap_or("").to_string();
        out.push(PcmDevice {
            card,
            device,
            id: clip(&id),
            name: clip(&name),
            playback: parts.iter().any(|p| p.starts_with("playback")),
            capture: parts.iter().any(|p| p.starts_with("capture")),
        });
    }
    out
}

fn has_call_word(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    CALL_WORDS.iter().any(|w| l.contains(w))
}

/// `attr="value"` inside one tag.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let pat = format!("{name}=\"");
    let mut from = 0;
    while let Some(i) = tag[from..].find(&pat) {
        let at = from + i;
        let ok = at == 0 || tag.as_bytes()[at - 1].is_ascii_whitespace();
        let start = at + pat.len();
        let end = start + tag[start..].find('"')?;
        if ok {
            return Some(&tag[start..end]);
        }
        from = end;
    }
    None
}

/// Every `<name ...>` tag in an XML text (comments are skipped).
fn tags<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let open = format!("<{name}");
    let mut from = 0;
    while let Some(i) = text[from..].find(&open) {
        let start = from + i;
        let after = start + open.len();
        let Some(end) = text[after..].find('>').map(|e| after + e) else { break };
        let next = text.as_bytes().get(after).copied().unwrap_or(b'>');
        if next.is_ascii_whitespace() || next == b'>' || next == b'/' {
            let before = &text[..start];
            let in_comment = before.rfind("<!--").is_some_and(|c| before[c..].find("-->").is_none());
            if !in_comment {
                out.push(&text[start..=end]);
            }
        }
        from = end;
    }
    out
}

/// The call-related mix ports, device ports and routes of an audio policy file.
pub fn scan_policy(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for tag in tags(text, "mixPort") {
        let name = attr(tag, "name").unwrap_or("");
        let flags = attr(tag, "flags").unwrap_or("");
        if has_call_word(name) || flags.contains("INCALL") || flags.contains("VOIP") {
            let role = attr(tag, "role").unwrap_or("");
            out.push(clip(&format!("mixPort name=\"{name}\" role=\"{role}\" flags=\"{flags}\"")));
        }
    }
    for tag in tags(text, "devicePort") {
        let name = attr(tag, "tagName").unwrap_or("");
        let ty = attr(tag, "type").unwrap_or("");
        if has_call_word(name) || ty.contains("TELEPHONY") {
            out.push(clip(&format!("devicePort tagName=\"{name}\" type=\"{ty}\"")));
        }
    }
    for tag in tags(text, "route") {
        let sink = attr(tag, "sink").unwrap_or("");
        let sources = attr(tag, "sources").unwrap_or("");
        if has_call_word(sink) || has_call_word(sources) {
            out.push(clip(&format!("route sink=\"{sink}\" sources=\"{sources}\"")));
        }
    }
    out.truncate(MAX_MATCHES);
    out
}

/// The call-related path names of a mixer paths file.
pub fn scan_mixer_paths(text: &str) -> Vec<String> {
    let mut out: Vec<String> = tags(text, "path")
        .into_iter()
        .filter_map(|t| attr(t, "name"))
        .filter(|n| has_call_word(n))
        .map(|n| clip(&format!("path name=\"{n}\"")))
        .collect();
    out.dedup();
    out.truncate(MAX_MATCHES);
    out
}

/// Which scanner a file needs, from its name; `None`: not one the probe reads.
pub fn file_kind(file_name: &str) -> Option<&'static str> {
    let n = file_name.to_ascii_lowercase();
    if !n.ends_with(".xml") {
        return None;
    }
    if n.contains("audio_policy_configuration") {
        Some("policy")
    } else if n.starts_with("mixer_paths") {
        Some("mixer_paths")
    } else {
        None
    }
}

pub fn build(input: &ProbeInput) -> Json {
    let props = Json::Obj(
        input
            .props
            .iter()
            .filter(|(k, _)| PROPS.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), Json::Str(clip(v))))
            .collect(),
    );
    let cards = Json::Arr(
        input
            .cards
            .iter()
            .map(|c| {
                let base = Json::obj()
                    .with("index", Json::Num(c.index as i64))
                    .with("id", Json::Str(clip(&c.id)))
                    .with("driver", Json::Str(clip(&c.driver)))
                    .with("name", Json::Str(clip(&c.name)))
                    .with("mixer", Json::Str(clip(&c.mixer_name)));
                match &c.controls {
                    Ok(list) => base.with(
                        "controls",
                        Json::Arr(
                            list.iter()
                                .map(|(name, index, ty, count)| {
                                    Json::obj()
                                        .with("name", Json::Str(clip(name)))
                                        .with("index", Json::Num(*index as i64))
                                        .with("type", Json::str(ty.clone()))
                                        .with("count", Json::Num(*count as i64))
                                })
                                .collect(),
                        ),
                    ),
                    Err(e) => base.with("controls_error", Json::Str(clip(e))),
                }
            })
            .collect(),
    );
    let pcm = Json::Arr(
        parse_proc_pcm(input.proc_pcm.as_deref().unwrap_or(""))
            .into_iter()
            .map(|p| {
                Json::obj()
                    .with("card", Json::Num(p.card as i64))
                    .with("device", Json::Num(p.device as i64))
                    .with("id", Json::Str(p.id))
                    .with("name", Json::Str(p.name))
                    .with("playback", Json::Bool(p.playback))
                    .with("capture", Json::Bool(p.capture))
            })
            .collect(),
    );
    let files = Json::Arr(
        input
            .files
            .iter()
            .take(MAX_FILES)
            .filter_map(|(path, text)| {
                let file_name = path.rsplit('/').next().unwrap_or(path);
                let matches = match file_kind(file_name)? {
                    "policy" => scan_policy(text),
                    _ => scan_mixer_paths(text),
                };
                Some(
                    Json::obj()
                        .with("path", Json::Str(clip(path)))
                        .with("matches", Json::Arr(matches.into_iter().map(Json::Str).collect())),
                )
            })
            .collect(),
    );
    Json::obj()
        .with(
            "helper",
            Json::obj()
                .with("version", Json::str(input.helper_version.clone()))
                .with("protocol", Json::Num(crate::proto::VERSION as i64))
                .with("abi", Json::str(input.abi.clone())),
        )
        .with("props", props)
        .with("cards", cards)
        .with("pcm", pcm)
        .with("files", files)
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLICY: &str = r#"<audioPolicyConfiguration>
      <!-- <mixPort name="incall old" role="source"/> -->
      <mixPort name="primary output" role="source" flags="AUDIO_OUTPUT_FLAG_PRIMARY">
      <mixPort name="incall playback" role="source" flags="AUDIO_OUTPUT_FLAG_INCALL_MUSIC">
      <mixPort name="incall capture" role="sink">
      <devicePort tagName="Telephony Tx" type="AUDIO_DEVICE_OUT_TELEPHONY_TX" role="sink">
      <route type="mix" sink="Telephony Tx" sources="incall playback,voip_rx"/>
      <route type="mix" sink="Speaker" sources="primary output"/>
    </audioPolicyConfiguration>"#;

    #[test]
    fn finds_call_ports_and_routes() {
        let m = scan_policy(POLICY);
        assert_eq!(
            m,
            vec![
                r#"mixPort name="incall playback" role="source" flags="AUDIO_OUTPUT_FLAG_INCALL_MUSIC""#,
                r#"mixPort name="incall capture" role="sink" flags="""#,
                r#"devicePort tagName="Telephony Tx" type="AUDIO_DEVICE_OUT_TELEPHONY_TX""#,
                r#"route sink="Telephony Tx" sources="incall playback,voip_rx""#,
            ]
        );
    }

    #[test]
    fn finds_call_mixer_paths() {
        let x = r#"<mixer><path name="speaker"/><path name="incall-music-uplink"><ctl name="x" value="1"/></path>
            <path name="voice-call"/><path name="voice-call"/></mixer>"#;
        assert_eq!(scan_mixer_paths(x), vec![r#"path name="incall-music-uplink""#, r#"path name="voice-call""#]);
    }

    #[test]
    fn parses_proc_pcm() {
        let t = "00-00: MultiMedia1 (*) :  : playback 1 : capture 1\n00-20: Incall Capture Stream0 (*) :  : capture 1\nbad line\n";
        let p = parse_proc_pcm(t);
        assert_eq!(p.len(), 2);
        assert_eq!(p[1], PcmDevice { card: 0, device: 20, id: "Incall Capture Stream0".into(), name: String::new(), playback: false, capture: true });
        assert!(p[0].playback && p[0].capture);
    }

    #[test]
    fn file_kinds() {
        assert_eq!(file_kind("audio_policy_configuration.xml"), Some("policy"));
        assert_eq!(file_kind("audio_policy_configuration_a2dp_offload_disabled.xml"), Some("policy"));
        assert_eq!(file_kind("mixer_paths_kalama_qrd.xml"), Some("mixer_paths"));
        assert_eq!(file_kind("audio_effects.xml"), None);
        assert_eq!(file_kind("mixer_paths.txt"), None);
    }

    #[test]
    fn reports_no_identifiers() {
        let input = ProbeInput {
            props: vec![
                ("ro.board.platform".into(), "gs201".into()),
                ("ro.serialno".into(), "SERIAL0XYZ123".into()),
                ("persist.radio.imei".into(), "356938035643809".into()),
                ("gsm.sim.operator.numeric".into(), "46692".into()),
                ("ro.product.model".into(), "Phone 15550100123".into()),
            ],
            cards: vec![CardFacts {
                index: 0,
                id: "aoc".into(),
                driver: "aoc".into(),
                name: "aoc-snd-card 0912345678".into(),
                mixer_name: String::new(),
                controls: Ok(vec![("Incall Mic Mute".into(), 0, "bool".into(), 1)]),
            }],
            proc_pcm: Some("00-20: Incall 13800138000 :  : capture 1\n".into()),
            files: vec![(
                "/vendor/etc/audio_policy_configuration.xml".into(),
                r#"<mixPort name="voice call 15550100123" role="source">"#.into(),
            )],
            helper_version: "0.1.0".into(),
            abi: "arm64-v8a".into(),
        };
        let out = build(&input).render();
        for secret in ["SERIAL0XYZ123", "356938035643809", "46692", "15550100123", "0912345678", "13800138000", "ro.serialno", "imei"] {
            assert!(!out.contains(secret), "probe output leaks {secret}: {out}");
        }
        assert!(out.contains(r#""ro.board.platform":"gs201""#));
        assert!(out.contains("Incall Mic Mute"));
        assert!(out.contains("<number>"));
    }

    #[test]
    fn scrub_keeps_short_numbers() {
        assert_eq!(scrub("MultiMedia1 48000 a0912345678b"), "MultiMedia1 48000 a<number>b");
        assert_eq!(scrub("1234567"), "1234567");
        assert_eq!(scrub("12345678"), "<number>");
    }
}
