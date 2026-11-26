//! `uplink_health`: after each call the phone app reports how the track that plays the
//! AI's voice into the call behaved (which settings it started and ended with, how much
//! audio the audio server threw away, how often the track was rebuilt). Bageo writes one
//! log line per report and keeps the phone's `audio_fit` for the phone list. Nothing in
//! the report identifies a person: no serial number, IMEI, phone number or call content.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How well a phone carries the AI's voice into calls, as the phone app reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioFit {
    Untested,
    Ok,
    Adjusted,
    Problem,
    Unsupported,
}

impl AudioFit {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Untested => "untested",
            Self::Ok => "ok",
            Self::Adjusted => "adjusted",
            Self::Problem => "problem",
            Self::Unsupported => "unsupported",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "untested" => Some(Self::Untested),
            "ok" => Some(Self::Ok),
            "adjusted" => Some(Self::Adjusted),
            "problem" => Some(Self::Problem),
            "unsupported" => Some(Self::Unsupported),
            _ => None,
        }
    }

    /// `audio_fit` of a phone message; None when missing or unknown (older apps).
    pub fn from_msg(msg: &Value) -> Option<Self> {
        msg.get("audio_fit").and_then(Value::as_str).and_then(Self::parse)
    }
}

/// Profile names the app may send; anything else is logged as `?`.
const PROFILES: &[&str] = &["standard", "primed", "primed-48k", "primed-48k-stereo"];
const SOURCES: &[&str] = &["forced", "owner_report", "remembered", "chip", "caps", "default"];
const REASONS: &[&str] = &[
    "audio_lost",
    "owner_report",
    "no_telephony_device",
    "prefer_rejected",
    "track_rejected",
];

fn known<'a>(msg: &'a Value, key: &str, allowed: &[&str]) -> &'a str {
    match msg.get(key).and_then(Value::as_str) {
        Some(s) if allowed.contains(&s) => s,
        _ => "?",
    }
}

fn num(msg: &Value, key: &str) -> u64 {
    msg.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn flag(msg: &Value, key: &str) -> bool {
    msg.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// Device and chip names: printable ASCII without spaces, at most 32 characters.
fn word(msg: &Value, key: &str) -> String {
    let s: String = msg
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .chars()
        .map(|c| if c == ' ' { '_' } else { c })
        .filter(|c| c.is_ascii_graphic())
        .take(32)
        .collect();
    if s.is_empty() {
        "-".to_string()
    } else {
        s
    }
}

/// Up to 16 numbers of a capability list, as `[a,b]`.
fn list(msg: &Value, key: &str) -> String {
    let items: Vec<String> = msg
        .get("caps")
        .and_then(|c| c.get(key))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_u64).take(16).map(|n| n.to_string()).collect())
        .unwrap_or_default();
    format!("[{}]", items.join(","))
}

/// The log line for one report: starts with `uplink health`, then `key=value` pairs.
pub fn log_line(serial: &str, msg: &Value) -> String {
    let fit = AudioFit::from_msg(msg).map_or("?", AudioFit::as_str);
    format!(
        "uplink health serial={serial} start={} final={} source={} rate={} channels={} buffer_ms={} \
         lost_ms={} loss_events={} rebuilds={} write_errors={} underruns={} switches={} \
         measure_only={} audio_ms={} live={} fit={fit} reason={} maker={} model={} soc={} \
         soc_model={} sdk={} caps_rates={} caps_channels={} caps_masks={} caps_encodings={}",
        known(msg, "start_profile", PROFILES),
        known(msg, "final_profile", PROFILES),
        known(msg, "start_source", SOURCES),
        num(msg, "track_rate"),
        num(msg, "channels"),
        num(msg, "buffer_ms"),
        num(msg, "lost_ms"),
        num(msg, "loss_events"),
        num(msg, "rebuilds"),
        num(msg, "write_errors"),
        num(msg, "underruns"),
        num(msg, "switches"),
        flag(msg, "measure_only"),
        num(msg, "audio_ms"),
        flag(msg, "live"),
        if msg.get("reason").is_some() { known(msg, "reason", REASONS) } else { "-" },
        word(msg, "manufacturer"),
        word(msg, "model"),
        if msg.get("soc_manufacturer").is_some() { word(msg, "soc_manufacturer") } else { word(msg, "hardware") },
        word(msg, "soc_model"),
        num(msg, "sdk"),
        list(msg, "sample_rates"),
        list(msg, "channel_counts"),
        list(msg, "channel_masks"),
        list(msg, "encodings"),
    )
}

/// `uplink_fit`: ask a phone to mark its fit as problem (the owner says the caller did
/// not hear the AI); the next call starts with the next settings. Older apps ignore it.
pub fn mark_problem_command(id: &str) -> Value {
    serde_json::json!({"type": "uplink_fit", "audio_fit": "problem", "id": id})
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report() -> Value {
        json!({
            "type": "uplink_health",
            "serial": "pixel-1",
            "start_profile": "standard",
            "final_profile": "primed",
            "start_source": "forced",
            "track_rate": 48000,
            "channels": 1,
            "buffer_ms": 160,
            "lost_ms": 2400,
            "loss_events": 21,
            "rebuilds": 0,
            "write_errors": 0,
            "underruns": 3,
            "switches": 1,
            "measure_only": false,
            "audio_ms": 31000,
            "live": true,
            "audio_fit": "adjusted",
            "manufacturer": "Xiaomi",
            "model": "Phone Model X",
            "android": "14",
            "sdk": 34,
            "soc_manufacturer": "Mediatek",
            "soc_model": "MT6985",
            "caps": {"sample_rates": [44100, 48000], "channel_counts": [], "channel_masks": [12], "encodings": [2]}
        })
    }

    #[test]
    fn log_line_has_every_field() {
        let line = log_line("pixel-1", &report());
        assert_eq!(
            line,
            "uplink health serial=pixel-1 start=standard final=primed source=forced rate=48000 \
             channels=1 buffer_ms=160 lost_ms=2400 loss_events=21 rebuilds=0 write_errors=0 \
             underruns=3 switches=1 measure_only=false audio_ms=31000 live=true fit=adjusted \
             reason=- maker=Xiaomi model=Phone_Model_X soc=Mediatek soc_model=MT6985 sdk=34 \
             caps_rates=[44100,48000] caps_channels=[] caps_masks=[12] caps_encodings=[2]"
        );
    }

    #[test]
    fn unknown_or_hostile_values_are_not_logged() {
        let msg = json!({
            "start_profile": "loud\nfake line",
            "start_source": 5,
            "reason": "rm -rf",
            "model": "a\u{1b}[31mred\nnext line that is very long and keeps going on",
            "track_rate": -1,
            "audio_fit": "great",
            "hardware": "mt6893",
            "caps": {"sample_rates": ["x", 8000]}
        });
        let line = log_line("pixel-2", &msg);
        assert!(line.contains(" start=? "), "{line}");
        assert!(line.contains(" source=? "), "{line}");
        assert!(line.contains(" reason=? "), "{line}");
        assert!(line.contains(" fit=? "), "{line}");
        assert!(line.contains(" rate=0 "), "{line}");
        assert!(line.contains(" soc=mt6893 "), "{line}");
        assert!(line.contains(" caps_rates=[8000] "), "{line}");
        assert!(!line.contains('\n') && !line.contains('\u{1b}'), "{line}");
        let model = line.split(" model=").nth(1).unwrap().split(' ').next().unwrap();
        assert!(model.len() <= 32, "{model}");
    }

    #[test]
    fn empty_report_still_logs() {
        let line = log_line("pixel-3", &json!({"type": "uplink_health"}));
        assert!(line.starts_with("uplink health serial=pixel-3 start=? final=? "), "{line}");
        assert!(line.contains(" maker=- "), "{line}");
    }

    #[test]
    fn audio_fit_values() {
        for f in [AudioFit::Untested, AudioFit::Ok, AudioFit::Adjusted, AudioFit::Problem, AudioFit::Unsupported] {
            assert_eq!(AudioFit::parse(f.as_str()), Some(f));
        }
        assert_eq!(AudioFit::from_msg(&json!({"audio_fit": "ok"})), Some(AudioFit::Ok));
        assert_eq!(AudioFit::from_msg(&json!({"audio_fit": "fine"})), None);
        assert_eq!(AudioFit::from_msg(&json!({})), None);
    }

    #[test]
    fn mark_problem_command_shape() {
        assert_eq!(
            mark_problem_command("abc"),
            json!({"type": "uplink_fit", "audio_fit": "problem", "id": "abc"})
        );
    }
}
