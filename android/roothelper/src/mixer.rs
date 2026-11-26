// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Mixer controls as the helper sees them, apart from the kernel calls: how a control
//! is named in a request, how new values are checked, and how controls are shown.

use crate::abi;
use crate::json::Json;

/// Highest sound card number accepted (`/dev/snd/controlC0` to `controlC31`).
pub const MAX_CARD: u32 = 31;

/// Names one control across reboots: the sound card by its ID string (its number can
/// change) and the kernel's own lookup key, minus `numid`, which can change when the
/// sound driver loads again. `card` is the card's number when the key was made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CtlKey {
    pub card: u32,
    pub card_id: String,
    pub iface: i32,
    pub device: u32,
    pub subdevice: u32,
    pub index: u32,
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CtlType {
    Boolean,
    Integer,
    Enumerated,
    Bytes,
    Iec958,
    Integer64,
    Other(i32),
}

impl CtlType {
    pub fn from_raw(t: i32) -> CtlType {
        match t {
            abi::CTL_TYPE_BOOLEAN => CtlType::Boolean,
            abi::CTL_TYPE_INTEGER => CtlType::Integer,
            abi::CTL_TYPE_ENUMERATED => CtlType::Enumerated,
            abi::CTL_TYPE_BYTES => CtlType::Bytes,
            abi::CTL_TYPE_IEC958 => CtlType::Iec958,
            abi::CTL_TYPE_INTEGER64 => CtlType::Integer64,
            other => CtlType::Other(other),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            CtlType::Boolean => "bool",
            CtlType::Integer => "int",
            CtlType::Enumerated => "enum",
            CtlType::Bytes => "bytes",
            CtlType::Iec958 => "iec958",
            CtlType::Integer64 => "int64",
            CtlType::Other(_) => "other",
        }
    }

    /// Most values a control of this type can hold.
    pub fn max_count(self) -> u32 {
        match self {
            CtlType::Boolean | CtlType::Integer => abi::MAX_VALUES_LONG,
            CtlType::Enumerated => abi::MAX_VALUES_ENUM,
            CtlType::Integer64 => abi::MAX_VALUES_INTEGER64,
            CtlType::Bytes => abi::MAX_VALUES_BYTES,
            CtlType::Iec958 | CtlType::Other(_) => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CtlInfo {
    pub numid: u32,
    pub key: CtlKey,
    pub ty: CtlType,
    pub access: u32,
    pub count: u32,
    pub min: i64,
    pub max: i64,
    pub step: i64,
    /// Item names of an enumerated control.
    pub items: Vec<String>,
}

impl CtlInfo {
    pub fn readable(&self) -> bool {
        self.access & abi::CTL_ACCESS_READ != 0 && self.ty.max_count() > 0 && self.count <= self.ty.max_count()
    }

    /// Whether `mixer.set` may change it: writable, a number, switch or list type, and
    /// readable too (its value must be saved first so it can be put back).
    pub fn settable(&self) -> bool {
        self.access & abi::CTL_ACCESS_WRITE != 0
            && self.readable()
            && matches!(self.ty, CtlType::Boolean | CtlType::Integer | CtlType::Enumerated | CtlType::Integer64)
            && self.count > 0
    }
}

/// How a request names a control:
///
/// - `numid=<n>`;
/// - its name, with an optional `@<index>` for the second, third ... control of the same
///   name (refused when several controls still match);
/// - its whole identity, `id:<iface>:<device>:<subdevice>:<index>:<name>` (the
///   interface as a number or as `card`, `mixer`, `pcm` ...), for controls that share a
///   name and an index but not a device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selector {
    Numid(u32),
    Name { name: String, index: u32 },
    Full { iface: i32, device: u32, subdevice: u32, index: u32, name: String },
}

/// What the kernel calls one control, for matching it against a [`Selector`].
pub struct CtlId<'a> {
    pub numid: u32,
    pub iface: i32,
    pub device: u32,
    pub subdevice: u32,
    pub index: u32,
    pub name: &'a str,
}

impl Selector {
    pub fn matches(&self, id: &CtlId) -> bool {
        match self {
            Selector::Numid(n) => *n == id.numid,
            Selector::Name { name, index } => name == id.name && *index == id.index,
            Selector::Full { iface, device, subdevice, index, name } => {
                *iface == id.iface
                    && *device == id.device
                    && *subdevice == id.subdevice
                    && *index == id.index
                    && name == id.name
            }
        }
    }
}

fn small_num(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 9 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn parse_iface(s: &str) -> Option<i32> {
    if let Some(n) = small_num(s) {
        return i32::try_from(n).ok().filter(|&n| n <= 6);
    }
    (0..=6).find(|&i| iface_name(i) == s)
}

pub fn parse_selector(s: &str) -> Result<Selector, String> {
    if let Some(n) = s.strip_prefix("numid=") {
        return match n.parse::<u32>() {
            Ok(v) if v > 0 && !n.starts_with('+') => Ok(Selector::Numid(v)),
            _ => Err(format!("bad control number: {s}")),
        };
    }
    if let Some(rest) = s.strip_prefix("id:") {
        let bad = || format!("bad control identity: {s}");
        let mut f = rest.splitn(5, ':');
        let iface = f.next().and_then(parse_iface).ok_or_else(bad)?;
        let device = f.next().and_then(small_num).ok_or_else(bad)?;
        let subdevice = f.next().and_then(small_num).ok_or_else(bad)?;
        let index = f.next().and_then(small_num).ok_or_else(bad)?;
        let name = f.next().ok_or_else(bad)?;
        if name.is_empty() || name.len() >= abi::CTL_NAME_LEN {
            return Err(bad());
        }
        return Ok(Selector::Full { iface, device, subdevice, index, name: name.to_string() });
    }
    let (name, index) = match s.rsplit_once('@') {
        Some((n, i)) if !i.is_empty() && i.len() <= 5 && i.bytes().all(|b| b.is_ascii_digit()) => {
            (n, i.parse::<u32>().map_err(|_| format!("bad control index: {s}"))?)
        }
        _ => (s, 0),
    };
    if name.is_empty() || name.len() >= abi::CTL_NAME_LEN {
        return Err(format!("bad control name: {s}"));
    }
    Ok(Selector::Name { name: name.to_string(), index })
}

pub fn parse_card(s: &str) -> Result<u32, String> {
    match s.parse::<u32>() {
        Ok(c) if c <= MAX_CARD && !s.starts_with('+') => Ok(c),
        _ => Err(format!("bad card number: {s}")),
    }
}

fn parse_one(info: &CtlInfo, s: &str) -> Result<i64, String> {
    match info.ty {
        CtlType::Boolean => match s {
            "1" | "on" | "true" => Ok(1),
            "0" | "off" | "false" => Ok(0),
            _ => Err(format!("{}: not on or off: {s}", info.key.name)),
        },
        CtlType::Integer | CtlType::Integer64 => {
            let v = s.parse::<i64>().map_err(|_| format!("{}: not a number: {s}", info.key.name))?;
            if info.min <= info.max && (v < info.min || v > info.max) {
                return Err(format!("{}: {v} is outside {}..{}", info.key.name, info.min, info.max));
            }
            if info.ty == CtlType::Integer && abi::WORD == 4 && i32::try_from(v).is_err() {
                return Err(format!("{}: {v} does not fit", info.key.name));
            }
            Ok(v)
        }
        CtlType::Enumerated => {
            if let Some(i) = info.items.iter().position(|item| item == s) {
                return Ok(i as i64);
            }
            match s.parse::<usize>() {
                Ok(i) if i < info.items.len() => Ok(i as i64),
                _ => Err(format!("{}: no item {s}", info.key.name)),
            }
        }
        _ => Err(format!("{}: this kind of control cannot be set", info.key.name)),
    }
}

/// The values to write for `args`: one value for every channel of the control, or a
/// single value used for all of them.
pub fn parse_set_values(info: &CtlInfo, args: &[String]) -> Result<Vec<i64>, String> {
    if !info.settable() {
        return Err(format!("{}: this control cannot be set", info.key.name));
    }
    let count = info.count as usize;
    if args.len() == 1 {
        let v = parse_one(info, &args[0])?;
        return Ok(vec![v; count]);
    }
    if args.len() != count {
        return Err(format!("{}: give 1 or {count} values, not {}", info.key.name, args.len()));
    }
    args.iter().map(|a| parse_one(info, a)).collect()
}

pub fn iface_name(iface: i32) -> &'static str {
    match iface {
        0 => "card",
        1 => "hwdep",
        2 => "mixer",
        3 => "pcm",
        4 => "rawmidi",
        5 => "timer",
        6 => "sequencer",
        _ => "other",
    }
}

fn access_text(access: u32) -> String {
    let mut s = String::new();
    s.push(if access & abi::CTL_ACCESS_READ != 0 { 'r' } else { '-' });
    s.push(if access & abi::CTL_ACCESS_WRITE != 0 { 'w' } else { '-' });
    if access & abi::CTL_ACCESS_VOLATILE != 0 {
        s.push('v');
    }
    if access & abi::CTL_ACCESS_INACTIVE != 0 {
        s.push('i');
    }
    s
}

/// A control's current values, by type.
pub fn values_json(info: &CtlInfo, values: &[i64]) -> Json {
    match info.ty {
        CtlType::Boolean => Json::Arr(values.iter().map(|&v| Json::Bool(v != 0)).collect()),
        CtlType::Enumerated => Json::Arr(
            values
                .iter()
                .map(|&v| match usize::try_from(v).ok().and_then(|i| info.items.get(i)) {
                    Some(name) => Json::str(name.clone()),
                    None => Json::Num(v),
                })
                .collect(),
        ),
        CtlType::Bytes => {
            let hex: String = values.iter().map(|&b| format!("{:02x}", b as u8)).collect();
            Json::Str(hex)
        }
        _ => Json::Arr(values.iter().map(|&v| Json::Num(v)).collect()),
    }
}

/// One control for `mixer.list` and `mixer.get`. `value`: `None` leaves it out.
pub fn info_json(info: &CtlInfo, value: Option<Result<Vec<i64>, String>>) -> Json {
    let mut j = Json::obj()
        .with("numid", Json::Num(info.numid as i64))
        .with("name", Json::str(info.key.name.clone()))
        .with("index", Json::Num(info.key.index as i64))
        .with("iface", Json::str(iface_name(info.key.iface)))
        .with("type", Json::str(info.ty.name()))
        .with("access", Json::Str(access_text(info.access)))
        .with("count", Json::Num(info.count as i64));
    if info.key.device != 0 || info.key.subdevice != 0 {
        j = j
            .with("device", Json::Num(info.key.device as i64))
            .with("subdevice", Json::Num(info.key.subdevice as i64));
    }
    match info.ty {
        CtlType::Integer | CtlType::Integer64 => {
            j = j
                .with("min", Json::Num(info.min))
                .with("max", Json::Num(info.max))
                .with("step", Json::Num(info.step));
        }
        CtlType::Enumerated => {
            j = j.with("items", Json::Arr(info.items.iter().map(|s| Json::str(s.clone())).collect()));
        }
        _ => {}
    }
    match value {
        Some(Ok(v)) => j.with("value", values_json(info, &v)),
        Some(Err(e)) => j.with("value", Json::Null).with("error", Json::Str(e)),
        None => j,
    }
}

#[cfg(test)]
pub(crate) fn test_ctl(numid: u32, name: &str, ty: CtlType, count: u32) -> CtlInfo {
    CtlInfo {
        numid,
        key: CtlKey { card: 0, card_id: "testcard".into(), iface: 2, device: 0, subdevice: 0, index: 0, name: name.to_string() },
        ty,
        access: abi::CTL_ACCESS_READ | abi::CTL_ACCESS_WRITE,
        count,
        min: 0,
        max: 100,
        step: 1,
        items: if ty == CtlType::Enumerated { vec!["Off".into(), "DL".into(), "UL".into()] } else { Vec::new() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn selectors() {
        assert_eq!(parse_selector("numid=12"), Ok(Selector::Numid(12)));
        assert!(parse_selector("numid=0").is_err());
        assert!(parse_selector("numid=+3").is_err());
        assert!(parse_selector("numid=x").is_err());
        assert_eq!(
            parse_selector("Incall Mic Mute"),
            Ok(Selector::Name { name: "Incall Mic Mute".into(), index: 0 })
        );
        assert_eq!(parse_selector("Capture Volume@1"), Ok(Selector::Name { name: "Capture Volume".into(), index: 1 }));
        assert_eq!(parse_selector("a@b"), Ok(Selector::Name { name: "a@b".into(), index: 0 }));
        assert!(parse_selector("").is_err());
        assert!(parse_selector("@1").is_err());
        assert!(parse_selector(&"n".repeat(44)).is_err());
        let id = |numid, device, index, name| CtlId { numid, iface: 2, device, subdevice: 0, index, name };
        assert!(Selector::Numid(5).matches(&id(5, 0, 3, "x")));
        assert!(!Selector::Name { name: "x".into(), index: 1 }.matches(&id(5, 0, 0, "x")));
    }

    #[test]
    fn whole_identities_tell_apart_controls_that_share_a_name() {
        let full = parse_selector("id:mixer:3:0:1:Voice Rx: Gain").unwrap();
        assert_eq!(
            full,
            Selector::Full { iface: 2, device: 3, subdevice: 0, index: 1, name: "Voice Rx: Gain".into() }
        );
        assert_eq!(parse_selector("id:2:3:0:1:Voice Rx: Gain"), Ok(full.clone()));
        let on = |device, subdevice| CtlId { numid: 9, iface: 2, device, subdevice, index: 1, name: "Voice Rx: Gain" };
        assert!(full.matches(&on(3, 0)));
        assert!(!full.matches(&on(4, 0)), "another device");
        assert!(!full.matches(&on(3, 1)), "another subdevice");
        let by_name = parse_selector("Voice Rx: Gain@1").unwrap();
        assert!(by_name.matches(&on(3, 0)) && by_name.matches(&on(4, 0)), "a name alone matches both");
        for bad in ["id:", "id:mixer:3:0:1:", "id:7:0:0:0:x", "id:mixr:0:0:0:x", "id:2:-1:0:0:x", "id:2:0:0:x", "id:2:0:0:0"] {
            assert!(parse_selector(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn cards() {
        assert_eq!(parse_card("0"), Ok(0));
        assert_eq!(parse_card("31"), Ok(31));
        assert!(parse_card("32").is_err());
        assert!(parse_card("-1").is_err());
        assert!(parse_card("+1").is_err());
        assert!(parse_card("").is_err());
    }

    #[test]
    fn set_values_by_type() {
        let b = test_ctl(1, "Incall Mic Mute", CtlType::Boolean, 1);
        assert_eq!(parse_set_values(&b, &args(&["on"])), Ok(vec![1]));
        assert_eq!(parse_set_values(&b, &args(&["0"])), Ok(vec![0]));
        assert!(parse_set_values(&b, &args(&["2"])).is_err());

        let i = test_ctl(2, "Voice Rx Device Mute", CtlType::Integer, 3);
        assert_eq!(parse_set_values(&i, &args(&["1", "100", "20"])), Ok(vec![1, 100, 20]));
        assert_eq!(parse_set_values(&i, &args(&["5"])), Ok(vec![5, 5, 5]));
        assert!(parse_set_values(&i, &args(&["1", "2"])).is_err(), "wrong count");
        assert!(parse_set_values(&i, &args(&["101"])).is_err(), "above max");
        assert!(parse_set_values(&i, &args(&["-1"])).is_err(), "below min");
        assert!(parse_set_values(&i, &args(&["1.5"])).is_err());

        let e = test_ctl(3, "Incall Capture Stream0", CtlType::Enumerated, 1);
        assert_eq!(parse_set_values(&e, &args(&["DL"])), Ok(vec![1]));
        assert_eq!(parse_set_values(&e, &args(&["2"])), Ok(vec![2]));
        assert!(parse_set_values(&e, &args(&["3"])).is_err());
        assert!(parse_set_values(&e, &args(&["dl"])).is_err());
    }

    #[test]
    fn refuses_controls_it_cannot_put_back() {
        let mut bytes = test_ctl(4, "Some Bytes", CtlType::Bytes, 16);
        assert!(parse_set_values(&bytes, &args(&["0"])).is_err());
        bytes.ty = CtlType::Integer;
        bytes.access = abi::CTL_ACCESS_WRITE;
        assert!(parse_set_values(&bytes, &args(&["0"])).is_err(), "write-only: no value to put back");
        bytes.access = abi::CTL_ACCESS_READ;
        assert!(parse_set_values(&bytes, &args(&["0"])).is_err(), "read-only");
    }

    #[test]
    fn shows_controls() {
        let e = test_ctl(3, "Incall Capture Stream0", CtlType::Enumerated, 1);
        let j = info_json(&e, Some(Ok(vec![1]))).render();
        assert_eq!(
            j,
            r#"{"numid":3,"name":"Incall Capture Stream0","index":0,"iface":"mixer","type":"enum","access":"rw","count":1,"items":["Off","DL","UL"],"value":["DL"]}"#
        );
        let b = test_ctl(1, "Incall Mic Mute", CtlType::Boolean, 2);
        assert!(info_json(&b, Some(Ok(vec![1, 0]))).render().ends_with(r#""value":[true,false]}"#));
        let i = test_ctl(2, "ADC1 Volume", CtlType::Integer, 1);
        assert!(info_json(&i, Some(Err("busy".into()))).render().ends_with(r#""value":null,"error":"busy"}"#));
        let mut bytes = test_ctl(4, "B", CtlType::Bytes, 2);
        bytes.access = abi::CTL_ACCESS_READ;
        assert!(info_json(&bytes, Some(Ok(vec![0, 255]))).render().ends_with(r#""value":"00ff"}"#));
    }
}
