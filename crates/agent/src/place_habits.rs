// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! What businesses in a region usually ask on the phone, for AI errands. The
//! pre-dial check ([crate::preflight]) reads which details a kind of call
//! needs there, and the call's script gets a short block on what the other
//! side tends to ask and how to handle it ([call_block]).
//!
//! The data is `data/place_habits.json`, one object per region (a region may
//! cover several countries). The number called decides the region; a number
//! outside every region gets none of it and the general rules apply.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

const DATA: &str = include_str!("../data/place_habits.json");

/// The kinds of business call the data describes.
pub const TYPES: [&str; 17] = [
    "restaurant", "clinic", "beauty", "repair", "takeout", "shop_order", "change", "inquiry", "hotel", "activity", "home_service", "pet",
    "tracking", "customer_service", "transport", "property", "government_school",
];

/// Every detail code the data may name.
pub const CODES: [&str; 27] = [
    "time", "party", "phone", "name", "item", "contact", "backup", "spec", "pickup", "substitute", "orig_time", "address", "for_whom",
    "kids_detail", "visit_type", "staff_pref", "seating_pref", "checkin_out", "guests", "room_type", "duration", "reference_no", "pet_detail",
    "pickup_place", "dropoff_place", "budget", "question",
];

/// The most characters of the script's block.
pub const CALL_BLOCK_MAX: usize = 900;
/// The most characters of one thing they ask or one situation in the block.
const ITEM_MAX_CHARS: usize = 90;
/// The most characters of one local habit (names, phone numbers, times) in the block.
const TIP_MAX_CHARS: usize = 140;
/// The most characters of a kind's note before dialing, for the pre-dial model.
const BEFORE_MAX_CHARS: usize = 70;
/// The most things they usually ask, in the block.
const ASKED_MAX: usize = 5;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Data {
    regions: Vec<Region>,
}

/// One region's habits, under the data file's own field names.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Region {
    /// Its id (`tw`, `us`, `hk-mo`).
    pub region: String,
    /// The countries it covers (ISO 3166 alpha-2, lowercase).
    pub countries: Vec<String>,
    /// Its name in English, for the script.
    pub region_name: String,
    pub politeness: String,
    pub name_tip: String,
    pub phone_tip: String,
    pub time_tip: String,
    pub number_tip: String,
    pub booking_channels: String,
    pub callback_habits: String,
    /// By kind of call ([TYPES]).
    pub types: BTreeMap<String, Place>,
}

/// What one kind of call is like in a region.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Place {
    /// How often such places take bookings by phone (`often`, `rarely`, …).
    pub phone_booking: String,
    /// What the owner should know before dialing, in Chinese (the model words it in the owner's language).
    pub before_dialing_zh: String,
    /// Whose matter it is counts (a patient, a guest): the owner's name does not stand for someone else's.
    pub for_whom_matters: bool,
    /// The details this kind of call needs before it dials ([CODES]).
    pub must_have: Vec<String>,
    /// What the other side usually asks, in English.
    pub usually_asked: Vec<String>,
    pub may_come_up: Vec<Situation>,
    pub sensitive_asked: Vec<Situation>,
}

/// Something that may come up on the call and how to handle it.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Situation {
    pub what: String,
    /// `ai_can_answer`, `ask_owner`, `owner_only` or `decline`.
    pub handle: String,
}

/// All regions of the data (the tests make sure it reads).
pub fn regions() -> &'static [Region] {
    static R: OnceLock<Vec<Region>> = OnceLock::new();
    R.get_or_init(|| serde_json::from_str::<Data>(DATA).map(|d| d.regions).unwrap_or_default())
}

/// The region a country (ISO 3166 alpha-2) belongs to.
pub fn for_country(iso: &str) -> Option<&'static Region> {
    let iso = iso.trim();
    if iso.is_empty() {
        return None;
    }
    regions().iter().find(|r| r.countries.iter().any(|c| c.eq_ignore_ascii_case(iso)))
}

/// The region of the number called; `home` (the owner's country) reads a
/// number written without its country code.
pub fn for_number(number: &str, home: &str) -> Option<&'static Region> {
    let home = Some(home.trim()).filter(|h| !h.is_empty());
    for_country(crate::phone_region::normalize(number, home)?.country)
}

/// A kind of call as the data names it, or `contact`; anything else is empty.
pub fn clean_type(s: &str) -> String {
    let t = s.trim().to_ascii_lowercase();
    if t == "contact" || TYPES.contains(&t.as_str()) {
        t
    } else {
        String::new()
    }
}

fn code(s: &str) -> Option<&'static str> {
    CODES.iter().copied().find(|c| *c == s.trim())
}

/// The details a kind of call needs before it dials: the region's, or with
/// no region the ones most regions name.
pub fn must_have(region: Option<&Region>, kind: &str) -> Vec<&'static str> {
    if let Some(p) = region.and_then(|r| r.types.get(kind)) {
        return p.must_have.iter().filter_map(|c| code(c)).collect();
    }
    let all = regions();
    let mut out: Vec<&'static str> = Vec::new();
    for r in all {
        for c in r.types.get(kind).map(|p| p.must_have.as_slice()).unwrap_or_default() {
            let Some(c) = code(c) else { continue };
            let named = all.iter().filter(|x| x.types.get(kind).is_some_and(|p| p.must_have.iter().any(|m| m == c))).count();
            if named * 2 > all.len() && !out.contains(&c) {
                out.push(c);
            }
        }
    }
    out
}

/// Whose matter it is counts for this kind of call there (with no region:
/// in most regions).
pub fn for_whom_matters(region: Option<&Region>, kind: &str) -> bool {
    match region {
        Some(r) => r.types.get(kind).is_some_and(|p| p.for_whom_matters),
        None => {
            let all = regions();
            all.iter().filter(|r| r.types.get(kind).is_some_and(|p| p.for_whom_matters)).count() * 2 > all.len()
        }
    }
}

/// Such places there seldom or never get this done by phone (`rarely`, `no`):
/// only then is a note worth showing the owner before dialing. No region: never.
pub fn hard_by_phone(region: Option<&Region>, kind: &str) -> bool {
    region
        .and_then(|r| r.types.get(kind))
        .is_some_and(|p| matches!(p.phone_booking.trim().to_ascii_lowercase().as_str(), "rarely" | "no"))
}

/// For the pre-dial model: per kind of call, what it needs before dialing
/// and, with a region, how often it books by phone and its local note.
pub fn model_brief(region: Option<&Region>) -> String {
    let mut lines: Vec<String> = Vec::new();
    for kind in TYPES {
        let mut line = format!("{kind}: needs {}", must_have(region, kind).join(","));
        if for_whom_matters(region, kind) {
            line.push_str("; who it is for matters");
        }
        if let Some(p) = region.and_then(|r| r.types.get(kind)) {
            if !p.phone_booking.trim().is_empty() {
                line.push_str(&format!("; phone bookings: {}", one_line(&p.phone_booking)));
            }
            let before = clip(&one_line(&p.before_dialing_zh), BEFORE_MAX_CHARS);
            if !before.is_empty() {
                line.push_str(&format!("; local note: {before}"));
            }
        }
        lines.push(line);
    }
    lines.join("\n")
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// At most `max` characters, cut at a space when there is one late enough.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    match cut.rfind(' ') {
        Some(i) if i > cut.len() / 2 => cut[..i].trim_end_matches([',', ';', ' ']).to_string(),
        _ => cut,
    }
}

/// Words for the script: no bracketed asides (they hold local spellings), no
/// markers, one line, at most `max` characters, no closing stop.
fn plain(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut depth = 0u32;
    for ch in s.chars() {
        match ch {
            '(' | '（' => depth += 1,
            ')' | '）' => depth = depth.saturating_sub(1),
            '[' | ']' | '{' | '}' => {}
            c if depth == 0 => out.push(if c.is_control() { ' ' } else { c }),
            _ => {}
        }
    }
    let line = one_line(&out).replace(" ,", ",").replace(" ;", ";").replace(" .", ".");
    clip(line.trim_end_matches(['.', ';', ',', ' ']), max)
}

/// `lead` and as many of `items` as fit in the block, as one more line.
fn push_list(out: &mut String, lead: &str, items: &[String]) {
    for n in (1..=items.len()).rev() {
        let line = format!("- {lead}: {}.", items[..n].join("; "));
        if out.chars().count() + 1 + line.chars().count() <= CALL_BLOCK_MAX {
            out.push('\n');
            out.push_str(&line);
            return;
        }
    }
}

/// The script's block for a call to region `r` of kind `kind` ([TYPES]; another
/// kind gets only the region's habits, `contact` none): what the other side usually
/// asks, how to handle what may come up, and how names, numbers and times go
/// there. A direction in English, at most [CALL_BLOCK_MAX] characters; the
/// most important parts first.
pub fn call_block(r: &Region, kind: &str) -> String {
    if kind == "contact" || r.region_name.trim().is_empty() {
        return String::new();
    }
    let place = r.types.get(kind);
    let mut out = match place {
        Some(_) => format!(
            "[LOCAL WAYS] The other side is in {}; this is a {} call. How such calls usually go there, as a direction for your own words (the fixed rules below still come first):",
            r.region_name,
            kind.replace('_', " ")
        ),
        None => format!(
            "[LOCAL WAYS] The other side is in {}. How calls to businesses usually go there, as a direction for your own words (the fixed rules below still come first):",
            r.region_name
        ),
    };
    if let Some(p) = place {
        let asked: Vec<String> = p.usually_asked.iter().map(|a| plain(a, ITEM_MAX_CHARS)).filter(|a| !a.is_empty()).take(ASKED_MAX).collect();
        push_list(&mut out, "They usually ask about", &asked);
        let handled = |h: &str| -> Vec<String> {
            p.sensitive_asked
                .iter()
                .chain(&p.may_come_up)
                .filter(|s| s.handle == h)
                .map(|s| plain(&s.what, ITEM_MAX_CHARS))
                .filter(|s| !s.is_empty())
                .collect()
        };
        push_list(&mut out, "If they ask for these, do not give them; say the person you call for will provide them", &handled("owner_only"));
        push_list(&mut out, "Politely decline", &handled("decline"));
        push_list(&mut out, "Check through ask_owner before you agree to", &handled("ask_owner"));
        push_list(&mut out, "You may settle these yourself within the matter", &handled("ai_can_answer"));
    }
    for (lead, tip) in [
        ("Names there", &r.name_tip),
        ("Phone numbers", &r.phone_tip),
        ("Times", &r.time_tip),
        ("Numbers", &r.number_tip),
        ("Manner", &r.politeness),
        ("Bookings", &r.booking_channels),
        ("Confirmations", &r.callback_habits),
    ] {
        let tip = plain(tip, TIP_MAX_CHARS);
        if !tip.is_empty() {
            push_list(&mut out, lead, &[tip]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn data() -> Value {
        serde_json::from_str(DATA).expect("place_habits.json reads")
    }

    /// An example without the note in brackets after it ("兩個小時（時長非人數）").
    fn example(s: &str) -> String {
        let cut = s.find('（').or_else(|| s.find(" (")).unwrap_or(s.len());
        s[..cut].trim().to_string()
    }

    /// Words that hand the AI a fixed line to recite (as in stall_script).
    const SCRIPTED: [&str; 19] = [
        "必須說", "必须说", "一定要說", "一定要说", "照這句", "照这句", "照著說", "照着说", "照念", "逐字", "一字不改",
        "台詞", "台词", "下一句：", "must say", "say exactly", "word for word", "verbatim", "use this line",
    ];

    #[test]
    fn every_region_reads_with_all_kinds_and_known_codes() {
        let regs = regions();
        assert_eq!(
            regs.iter().map(|r| r.region.as_str()).collect::<Vec<_>>(),
            [
                "tw", "us", "hk-mo", "jp", "cn", "sg", "kr", "gb-ie", "ca", "au-nz", "es", "mx", "latam", "ar-uy", "fr", "be", "lu", "de", "at", "ch", "br",
                "pt"
            ]
        );
        let mut countries: Vec<&str> = Vec::new();
        for r in regs {
            assert!(!r.region_name.is_empty(), "{}", r.region);
            for c in &r.countries {
                assert!(c.len() == 2 && c.chars().all(|x| x.is_ascii_lowercase()), "{c}");
                assert!(!countries.contains(&c.as_str()), "{c} in two regions");
                countries.push(c);
            }
            for kind in TYPES {
                let p = r.types.get(kind).unwrap_or_else(|| panic!("{} {kind}", r.region));
                assert!(!p.must_have.is_empty(), "{} {kind}", r.region);
                // Every detail a kind needs is one the owner can be asked about.
                for c in &p.must_have {
                    assert!(crate::preflight::KEYS.contains(&c.as_str()), "{} {kind} {c}", r.region);
                }
                for s in p.may_come_up.iter().chain(&p.sensitive_asked) {
                    assert!(["ai_can_answer", "ask_owner", "owner_only", "decline"].contains(&s.handle.as_str()), "{}", s.handle);
                }
            }
            assert_eq!(r.types.len(), TYPES.len(), "{}", r.region);
        }
        for r in data()["regions"].as_array().unwrap() {
            for (kind, p) in r["types"].as_object().unwrap() {
                for field in ["must_have", "nice_to_have"] {
                    for c in p[field].as_array().unwrap() {
                        assert!(CODES.contains(&c.as_str().unwrap()), "{kind} {field} {c}");
                    }
                }
                // Owners call local places from local numbers: no advice to dial from another country's.
                let before = p["before_dialing_zh"].as_str().unwrap();
                assert!(!before.contains("門號撥打") && !before.contains("門號聯絡"), "{before}");
            }
        }
    }

    #[test]
    fn every_way_owners_write_a_party_there_reads_as_a_party_and_look_alikes_do_not() {
        let mut seen = 0;
        for r in data()["regions"].as_array().unwrap() {
            for (lang, w) in r["how_owners_write"].as_object().unwrap() {
                for p in w["party"].as_array().unwrap() {
                    let p = example(p.as_str().unwrap());
                    assert!(crate::preflight::names_party(&p), "{} {lang}: {p}", r["region"]);
                    seen += 1;
                }
                for f in w["false_friends"].as_array().unwrap() {
                    let f = example(f.as_str().unwrap());
                    assert!(!crate::preflight::names_party(&f), "{} {lang}: {f}", r["region"]);
                }
            }
        }
        assert!(seen >= 50, "{seen}");
    }

    #[test]
    fn the_number_called_picks_the_region() {
        let id = |n: &str| for_number(n, "tw").map(|r| r.region.as_str());
        assert_eq!(id("+12125550123"), Some("us"));
        assert_eq!(id("0223456789"), Some("tw"));
        assert_eq!(id("+886912345678"), Some("tw"));
        assert_eq!(id("+85221234567"), Some("hk-mo"));
        assert_eq!(id("+85328123456"), Some("hk-mo"));
        assert_eq!(id("+81312345678"), Some("jp"));
        assert_eq!(for_number("09012345678", "jp").map(|r| r.region.as_str()), Some("jp"));
        // A number without its country code is the owner's country's.
        assert_eq!(for_number("2125550123", "us").map(|r| r.region.as_str()), Some("us"));
        assert_eq!(id("+4930123456"), Some("de"));
        assert_eq!(id("+33123456789"), Some("fr"));
        assert_eq!(id("+8613800138000"), Some("cn"));
        assert_eq!(id("+821012345678"), Some("kr"));
        assert_eq!(id("+5511912345678"), Some("br"));
        // Places with no data, and no number at all.
        assert_eq!(id("+390612345678"), None);
        assert_eq!(id(""), None);
    }

    #[test]
    fn a_kind_without_a_region_needs_what_most_regions_say() {
        assert_eq!(must_have(None, "restaurant"), ["time", "party", "name", "phone"]);
        assert_eq!(must_have(for_country("us"), "restaurant"), ["time", "party", "name", "phone"]);
        assert!(must_have(None, "nothing").is_empty());
        assert!(for_whom_matters(None, "clinic"));
        assert!(!for_whom_matters(None, "restaurant"));
        assert_eq!(clean_type(" Hotel "), "hotel");
        assert_eq!(clean_type("contact"), "contact");
        assert_eq!(clean_type("cooking"), "");
    }

    #[test]
    fn the_model_brief_names_every_kind_and_the_local_notes() {
        let tw = model_brief(for_country("tw"));
        assert_eq!(tw.lines().count(), TYPES.len());
        assert!(tw.contains("restaurant: needs time,party,name,phone; phone bookings: often; local note: "), "{tw}");
        let none = model_brief(None);
        assert!(none.contains("restaurant: needs time,party,name,phone\n") && !none.contains("local note"), "{none}");
    }

    #[test]
    fn the_script_block_fits_gives_directions_and_skips_personal_calls() {
        for r in regions() {
            for kind in TYPES.iter().copied().chain(["", "unknown"]) {
                let b = call_block(r, kind);
                assert!(b.starts_with("[LOCAL WAYS] The other side is in "), "{b}");
                assert!(b.chars().count() <= CALL_BLOCK_MAX, "{} {kind}: {}", r.region, b.chars().count());
                let low = b.to_lowercase();
                for bad in SCRIPTED {
                    assert!(!low.contains(bad), "{bad}: {b}");
                }
                assert!(!b.contains('（') && !b[1..].contains('['), "{b}");
            }
            assert_eq!(call_block(r, "contact"), "");
        }
        let us = call_block(for_country("us").unwrap(), "restaurant");
        assert!(us.contains("the United States; this is a restaurant call"), "{us}");
        assert!(us.contains("- They usually ask about: "), "{us}");
        assert!(us.contains("do not give them"), "{us}");
        assert!(us.len() > 400, "{us}");
    }
}

