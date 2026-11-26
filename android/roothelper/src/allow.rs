// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! What the app may change, per phone family: built into the helper, never sent by the
//! app. Anything not listed is refused, and a value put back after a call must belong
//! to a listed control or parameter too.
//!
//! - a control: its card (by the card's ID string), interface, device, subdevice, name
//!   and index, its type and value count, and the values it may be set to;
//! - a parameter: its key, the values it may be set to, and optionally the value to put
//!   back when the phone does not report the current one;
//! - a PCM stream: card, device, direction, rates and channel counts (always s16le).
//!
//! [`RULES`] is empty: no phone family has been added yet, so `mixer.set`, `params.set`
//! and `pcm.open` refuse everything from the app.

use crate::control::{Direction, PcmRequest};
use crate::mixer::{CtlInfo, CtlType};

pub enum Values {
    /// One of these value lists, whole (one value per channel).
    Exact(&'static [&'static [i64]]),
    /// Every value within this range.
    Range(i64, i64),
    /// Every value one of these item names (enumerated controls).
    Items(&'static [&'static str]),
}

pub struct CtlRule {
    pub card_id: &'static str,
    pub iface: i32,
    pub device: u32,
    pub subdevice: u32,
    pub name: &'static str,
    pub index: u32,
    pub ty: CtlType,
    pub count: u32,
    pub values: Values,
}

pub struct ParamRule {
    pub key: &'static str,
    pub values: &'static [&'static str],
    /// Put back when the phone does not report the key's value; without it such a key
    /// is never changed.
    pub default: Option<&'static str>,
}

pub struct PcmRule {
    pub card_id: &'static str,
    pub device: u32,
    pub dir: Direction,
    pub rates: &'static [u32],
    pub channels: &'static [u32],
    pub max_period_frames: u32,
}

/// The rules of one phone family: phones whose `ro.board.platform` is one of
/// `platforms`, and, when `devices` is not empty, whose `ro.product.device` is one of them.
pub struct PhoneRules {
    pub platforms: &'static [&'static str],
    pub devices: &'static [&'static str],
    pub ctls: &'static [CtlRule],
    pub params: &'static [ParamRule],
    pub pcm: &'static [PcmRule],
}

/// No phone family yet.
pub const RULES: &[PhoneRules] = &[];

/// The rules for this phone.
#[derive(Default)]
pub struct Allowlist {
    ctls: Vec<&'static CtlRule>,
    params: Vec<&'static ParamRule>,
    pcm: Vec<&'static PcmRule>,
}

impl Allowlist {
    pub fn for_phone(rules: &'static [PhoneRules], platform: &str, device: &str) -> Allowlist {
        let mut a = Allowlist::default();
        if platform.is_empty() {
            return a;
        }
        for r in rules {
            if !r.platforms.contains(&platform) || (!r.devices.is_empty() && !r.devices.contains(&device)) {
                continue;
            }
            a.ctls.extend(r.ctls.iter());
            a.params.extend(r.params.iter());
            a.pcm.extend(r.pcm.iter());
        }
        a
    }

    pub fn is_empty(&self) -> bool {
        self.ctls.is_empty() && self.params.is_empty() && self.pcm.is_empty()
    }

    /// The rule for this control, matched on every part of its identity, type and count.
    pub fn ctl_rule(&self, card_id: &str, info: &CtlInfo) -> Option<&'static CtlRule> {
        self.ctls.iter().copied().find(|r| {
            r.card_id == card_id
                && r.iface == info.key.iface
                && r.device == info.key.device
                && r.subdevice == info.key.subdevice
                && r.name == info.key.name
                && r.index == info.key.index
                && r.ty == info.ty
                && r.count == info.count
        })
    }

    /// Whether the app may set this control to `values`.
    pub fn ctl_values_ok(rule: &CtlRule, info: &CtlInfo, values: &[i64]) -> bool {
        if values.len() != rule.count as usize {
            return false;
        }
        match rule.values {
            Values::Exact(lists) => lists.contains(&values),
            Values::Range(lo, hi) => values.iter().all(|v| (lo..=hi).contains(v)),
            Values::Items(names) => values.iter().all(|&v| {
                usize::try_from(v)
                    .ok()
                    .and_then(|i| info.items.get(i))
                    .is_some_and(|item| names.contains(&item.as_str()))
            }),
        }
    }

    pub fn param_rule(&self, key: &str) -> Option<&'static ParamRule> {
        self.params.iter().copied().find(|r| r.key == key)
    }

    pub fn pcm_ok(&self, card_id: &str, req: &PcmRequest) -> bool {
        self.pcm.iter().any(|r| {
            r.card_id == card_id
                && r.device == req.device
                && r.dir == req.dir
                && r.rates.contains(&req.rate)
                && r.channels.contains(&req.channels)
                && req.period_frames <= r.max_period_frames
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::mixer::test_ctl;

    /// Rules for tests only (the card in `mixer::test_ctl` is "testcard").
    pub static TEST_RULES: &[PhoneRules] = &[PhoneRules {
        platforms: &["testplatform"],
        devices: &[],
        ctls: &[
            CtlRule {
                card_id: "testcard",
                iface: 2,
                device: 0,
                subdevice: 0,
                name: "Incall Mic Mute",
                index: 0,
                ty: CtlType::Boolean,
                count: 1,
                values: Values::Exact(&[&[1]]),
            },
            CtlRule {
                card_id: "testcard",
                iface: 2,
                device: 0,
                subdevice: 0,
                name: "Incall Capture Stream0",
                index: 0,
                ty: CtlType::Enumerated,
                count: 1,
                values: Values::Items(&["DL"]),
            },
            CtlRule {
                card_id: "testcard",
                iface: 2,
                device: 0,
                subdevice: 0,
                name: "Voice Rx Device Mute",
                index: 0,
                ty: CtlType::Integer,
                count: 3,
                values: Values::Range(0, 100),
            },
        ],
        params: &[
            ParamRule { key: "incall_music_enabled", values: &["true", "false"], default: None },
            ParamRule { key: "Set_BGS_UL_Mute", values: &["0"], default: Some("1") },
        ],
        pcm: &[PcmRule {
            card_id: "testcard",
            device: 20,
            dir: Direction::Capture,
            rates: &[48000],
            channels: &[1],
            max_period_frames: 1920,
        }],
    }];

    #[test]
    fn the_built_in_list_is_empty() {
        assert!(RULES.is_empty());
        assert!(Allowlist::for_phone(RULES, "gs201", "panther").is_empty());
        assert!(Allowlist::for_phone(RULES, "", "").is_empty());
    }

    #[test]
    fn rules_apply_to_their_phones_only() {
        assert!(!Allowlist::for_phone(TEST_RULES, "testplatform", "any").is_empty());
        assert!(Allowlist::for_phone(TEST_RULES, "otherplatform", "any").is_empty());
        assert!(Allowlist::for_phone(TEST_RULES, "", "").is_empty());
    }

    #[test]
    fn controls_match_on_their_whole_identity_and_values() {
        let a = Allowlist::for_phone(TEST_RULES, "testplatform", "x");
        let mute = test_ctl(1, "Incall Mic Mute", CtlType::Boolean, 1);
        let rule = a.ctl_rule("testcard", &mute).unwrap();
        assert!(Allowlist::ctl_values_ok(rule, &mute, &[1]));
        assert!(!Allowlist::ctl_values_ok(rule, &mute, &[0]), "only the listed value");
        assert!(a.ctl_rule("othercard", &mute).is_none());
        let mut other_index = mute.clone();
        other_index.key.index = 1;
        assert!(a.ctl_rule("testcard", &other_index).is_none());
        let mut other_count = mute.clone();
        other_count.count = 2;
        assert!(a.ctl_rule("testcard", &other_count).is_none());
        let unlisted = test_ctl(9, "Speaker Volume", CtlType::Integer, 1);
        assert!(a.ctl_rule("testcard", &unlisted).is_none());

        let stream = test_ctl(2, "Incall Capture Stream0", CtlType::Enumerated, 1);
        let rule = a.ctl_rule("testcard", &stream).unwrap();
        assert!(Allowlist::ctl_values_ok(rule, &stream, &[1]), "DL");
        assert!(!Allowlist::ctl_values_ok(rule, &stream, &[2]), "UL is not listed");
        assert!(!Allowlist::ctl_values_ok(rule, &stream, &[7]));

        let rx = test_ctl(3, "Voice Rx Device Mute", CtlType::Integer, 3);
        let rule = a.ctl_rule("testcard", &rx).unwrap();
        assert!(Allowlist::ctl_values_ok(rule, &rx, &[1, 100, 20]));
        assert!(!Allowlist::ctl_values_ok(rule, &rx, &[1, 101, 20]));
        assert!(!Allowlist::ctl_values_ok(rule, &rx, &[1, 100]));
    }

    #[test]
    fn pcm_streams_match_whole() {
        let a = Allowlist::for_phone(TEST_RULES, "testplatform", "x");
        let ok = PcmRequest {
            dir: Direction::Capture,
            card: 0,
            device: 20,
            rate: 48000,
            channels: 1,
            period_frames: 960,
            periods: 4,
        };
        assert!(a.pcm_ok("testcard", &ok));
        assert!(!a.pcm_ok("othercard", &ok));
        assert!(!a.pcm_ok("testcard", &PcmRequest { dir: Direction::Playback, ..ok }));
        assert!(!a.pcm_ok("testcard", &PcmRequest { device: 21, ..ok }));
        assert!(!a.pcm_ok("testcard", &PcmRequest { rate: 16000, ..ok }));
        assert!(!a.pcm_ok("testcard", &PcmRequest { channels: 2, ..ok }));
        assert!(!a.pcm_ok("testcard", &PcmRequest { period_frames: 1921, ..ok }));
    }
}
