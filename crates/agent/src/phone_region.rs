//! Which country a caller's number belongs to. The caller ID is read the way
//! the SIM that took the call reads it: `0912 345 678` on a Taiwanese SIM is a
//! Taiwanese number, `(202) 555-0143` on a US SIM an American one, and `+…` or
//! `00…` carries its own country code.
//!
//! Only as much numbering as the language decisions need: each country's
//! calling code, the national trunk prefix, and which country a +1 area code or
//! a +7 number belongs to. Also remembered here: the SIM country each phone
//! reported last.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

/// A caller ID in international form, and the country it belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerNumber {
    /// `+` and the digits (`+886912345678`).
    pub e164: String,
    /// ISO 3166 alpha-2, lowercase (`tw`).
    pub country: &'static str,
}

/// Country (ISO 3166 alpha-2, lowercase) and its calling code. Where countries
/// share a code, the main one comes first: a number of that code belongs to it
/// unless [`NANP_AREAS`] or the +7 rule say otherwise.
const COUNTRIES: &[(&str, &str)] = &[
    // North American Numbering Plan.
    ("us", "1"), ("ca", "1"), ("ag", "1"), ("ai", "1"), ("as", "1"), ("bb", "1"), ("bm", "1"),
    ("bs", "1"), ("dm", "1"), ("do", "1"), ("gd", "1"), ("gu", "1"), ("jm", "1"), ("kn", "1"),
    ("ky", "1"), ("lc", "1"), ("mp", "1"), ("ms", "1"), ("pr", "1"), ("sx", "1"), ("tc", "1"),
    ("tt", "1"), ("vc", "1"), ("vg", "1"), ("vi", "1"),
    ("ru", "7"), ("kz", "7"),
    ("eg", "20"), ("ss", "211"), ("ma", "212"), ("eh", "212"), ("dz", "213"), ("tn", "216"),
    ("ly", "218"), ("gm", "220"), ("sn", "221"), ("mr", "222"), ("ml", "223"), ("gn", "224"),
    ("ci", "225"), ("bf", "226"), ("ne", "227"), ("tg", "228"), ("bj", "229"), ("mu", "230"),
    ("lr", "231"), ("sl", "232"), ("gh", "233"), ("ng", "234"), ("td", "235"), ("cf", "236"),
    ("cm", "237"), ("cv", "238"), ("st", "239"), ("gq", "240"), ("ga", "241"), ("cg", "242"),
    ("cd", "243"), ("ao", "244"), ("gw", "245"), ("io", "246"), ("ac", "247"), ("sc", "248"),
    ("sd", "249"), ("rw", "250"), ("et", "251"), ("so", "252"), ("dj", "253"), ("ke", "254"),
    ("tz", "255"), ("ug", "256"), ("bi", "257"), ("mz", "258"), ("zm", "260"), ("mg", "261"),
    ("re", "262"), ("yt", "262"), ("zw", "263"), ("na", "264"), ("mw", "265"), ("ls", "266"),
    ("bw", "267"), ("sz", "268"), ("km", "269"), ("za", "27"), ("sh", "290"), ("ta", "290"),
    ("er", "291"), ("aw", "297"), ("fo", "298"), ("gl", "299"),
    ("gr", "30"), ("nl", "31"), ("be", "32"), ("fr", "33"), ("es", "34"), ("gi", "350"),
    ("pt", "351"), ("lu", "352"), ("ie", "353"), ("is", "354"), ("al", "355"), ("mt", "356"),
    ("cy", "357"), ("fi", "358"), ("ax", "358"), ("bg", "359"), ("hu", "36"), ("lt", "370"),
    ("lv", "371"), ("ee", "372"), ("md", "373"), ("am", "374"), ("by", "375"), ("ad", "376"),
    ("mc", "377"), ("sm", "378"), ("ua", "380"), ("rs", "381"), ("me", "382"), ("xk", "383"),
    ("hr", "385"), ("si", "386"), ("ba", "387"), ("mk", "389"), ("it", "39"), ("va", "39"),
    ("ro", "40"), ("ch", "41"), ("cz", "420"), ("sk", "421"), ("li", "423"), ("at", "43"),
    ("gb", "44"), ("gg", "44"), ("im", "44"), ("je", "44"), ("dk", "45"), ("se", "46"),
    ("no", "47"), ("sj", "47"), ("pl", "48"), ("de", "49"),
    ("fk", "500"), ("bz", "501"), ("gt", "502"), ("sv", "503"), ("hn", "504"), ("ni", "505"),
    ("cr", "506"), ("pa", "507"), ("pm", "508"), ("ht", "509"), ("pe", "51"), ("mx", "52"),
    ("cu", "53"), ("ar", "54"), ("br", "55"), ("cl", "56"), ("co", "57"), ("ve", "58"),
    ("gp", "590"), ("bl", "590"), ("mf", "590"), ("bo", "591"), ("gy", "592"), ("ec", "593"),
    ("gf", "594"), ("py", "595"), ("mq", "596"), ("sr", "597"), ("uy", "598"), ("cw", "599"),
    ("bq", "599"),
    ("my", "60"), ("au", "61"), ("cx", "61"), ("cc", "61"), ("id", "62"), ("ph", "63"),
    ("nz", "64"), ("sg", "65"), ("th", "66"), ("tl", "670"), ("nf", "672"), ("bn", "673"),
    ("nr", "674"), ("pg", "675"), ("to", "676"), ("sb", "677"), ("vu", "678"), ("fj", "679"),
    ("pw", "680"), ("wf", "681"), ("ck", "682"), ("nu", "683"), ("ws", "685"), ("ki", "686"),
    ("nc", "687"), ("tv", "688"), ("pf", "689"), ("tk", "690"), ("fm", "691"), ("mh", "692"),
    ("jp", "81"), ("kr", "82"), ("vn", "84"), ("kp", "850"), ("hk", "852"), ("mo", "853"),
    ("kh", "855"), ("la", "856"), ("cn", "86"), ("bd", "880"), ("tw", "886"),
    ("tr", "90"), ("in", "91"), ("pk", "92"), ("af", "93"), ("lk", "94"), ("mm", "95"),
    ("mv", "960"), ("lb", "961"), ("jo", "962"), ("sy", "963"), ("iq", "964"), ("kw", "965"),
    ("sa", "966"), ("ye", "967"), ("om", "968"), ("ps", "970"), ("ae", "971"), ("il", "972"),
    ("bh", "973"), ("qa", "974"), ("bt", "975"), ("mn", "976"), ("np", "977"), ("ir", "98"),
    ("tj", "992"), ("tm", "993"), ("az", "994"), ("ge", "995"), ("kg", "996"), ("uz", "998"),
];

/// +1 area codes outside the United States, by country.
const NANP_AREAS: &[(&str, &[&str])] = &[
    (
        "ca",
        &[
            "204", "226", "236", "249", "250", "257", "263", "289", "306", "343", "354", "365",
            "367", "368", "382", "403", "416", "418", "428", "431", "437", "438", "450", "468",
            "474", "506", "514", "519", "548", "579", "581", "584", "587", "604", "613", "639",
            "647", "672", "683", "705", "709", "742", "753", "778", "780", "782", "807", "819",
            "825", "867", "873", "879", "902", "905", "942",
        ],
    ),
    ("ag", &["268"]),
    ("ai", &["264"]),
    ("as", &["684"]),
    ("bb", &["246"]),
    ("bm", &["441"]),
    ("bs", &["242"]),
    ("dm", &["767"]),
    ("do", &["809", "829", "849"]),
    ("gd", &["473"]),
    ("gu", &["671"]),
    ("jm", &["876", "658"]),
    ("kn", &["869"]),
    ("ky", &["345"]),
    ("lc", &["758"]),
    ("mp", &["670"]),
    ("ms", &["664"]),
    ("pr", &["787", "939"]),
    ("sx", &["721"]),
    ("tc", &["649"]),
    ("tt", &["868"]),
    ("vc", &["784"]),
    ("vg", &["284"]),
    ("vi", &["340"]),
];

/// Fewer digits after the country code: a service or garbled number.
const MIN_NATIONAL_DIGITS: usize = 4;
/// E.164 numbers have at most 15 digits.
const MAX_DIGITS: usize = 15;

/// Every country this module knows, ISO 3166 alpha-2, lowercase.
pub fn countries() -> impl Iterator<Item = &'static str> {
    COUNTRIES.iter().map(|&(iso, _)| iso)
}

/// The calling code of a country (ISO 3166 alpha-2, any case): `tw` → `886`.
pub fn calling_code(iso: &str) -> Option<&'static str> {
    let iso = iso.trim();
    COUNTRIES
        .iter()
        .find(|&&(c, _)| c.eq_ignore_ascii_case(iso))
        .map(|&(_, code)| code)
}

/// The main country of a calling code (digits): `886` → `tw`, `1` → `us`.
pub fn country_of_code(code: &str) -> Option<&'static str> {
    let code = code.trim();
    COUNTRIES
        .iter()
        .find(|&&(_, c)| !code.is_empty() && c == code)
        .map(|&(iso, _)| iso)
}

/// The calling code that starts `digits` (calling codes are prefix-free, so
/// there is at most one) and the digits after it.
fn split_code(digits: &str) -> Option<(&'static str, &str)> {
    (1..=3).filter(|&n| digits.len() > n).find_map(|n| {
        let head = &digits[..n];
        COUNTRIES
            .iter()
            .find(|&&(_, code)| code == head)
            .map(|&(_, code)| (code, &digits[n..]))
    })
}

/// The country of a number with calling code `code` and national digits
/// `national`: the code's main country, except for +1 area codes of other
/// countries and Kazakhstan's +7 6… / +7 7… numbers.
fn country_in_code(code: &str, national: &str) -> Option<&'static str> {
    match code {
        "1" => {
            let area = national.get(..3).unwrap_or("");
            Some(
                NANP_AREAS
                    .iter()
                    .find(|(_, areas)| areas.contains(&area))
                    .map_or("us", |&(iso, _)| iso),
            )
        }
        "7" if national.starts_with('6') || national.starts_with('7') => Some("kz"),
        _ => country_of_code(code),
    }
}

/// The national significant number of digits dialled inside country `iso`
/// (calling code `code`): without the trunk prefix (`0` in most countries, `8`
/// in Russia, Kazakhstan and Belarus, `06` in Hungary, kept in Italy), and for
/// +1 without the leading `1` of eleven digits. Digits that already start with
/// the country code and are long enough for it are read as international.
fn national_number<'a>(code: &str, iso: &str, digits: &'a str) -> &'a str {
    if code == "1" {
        return match digits.strip_prefix('1') {
            Some(rest) if digits.len() == 11 => rest,
            _ => digits,
        };
    }
    if !digits.starts_with('0') && digits.len() >= code.len() + 8 {
        if let Some(rest) = digits.strip_prefix(code) {
            return rest;
        }
    }
    let trunk = match iso {
        "it" | "sm" | "va" => "",
        "ru" | "kz" | "by" => "8",
        "hu" => "06",
        _ => "0",
    };
    if trunk.is_empty() {
        return digits;
    }
    digits.strip_prefix(trunk).unwrap_or(digits)
}

/// A caller ID in international form and its country. `+…` and `00…` carry
/// their own country code; any other number is read as dialled inside `region`
/// (the country of the SIM that took the call, ISO 3166 alpha-2), so without a
/// region it has none. Hidden numbers (`unknown`, `private`, empty), service
/// numbers too short to place and unknown country codes: `None`.
pub fn normalize(caller_id: &str, region: Option<&str>) -> Option<CallerNumber> {
    let t = caller_id.trim();
    if t.is_empty() || t.chars().any(char::is_alphabetic) {
        return None;
    }
    let digits: String = t.chars().filter(char::is_ascii_digit).collect();
    let plus = t.chars().find(|c| c.is_ascii_digit() || *c == '+') == Some('+');
    let international = if plus {
        Some(digits.as_str())
    } else {
        digits.strip_prefix("00")
    };
    let (code, national) = match international {
        Some(rest) => split_code(rest)?,
        None => {
            let iso = region?.trim().to_ascii_lowercase();
            let code = calling_code(&iso)?;
            (code, national_number(code, &iso, &digits))
        }
    };
    if national.len() < MIN_NATIONAL_DIGITS || code.len() + national.len() > MAX_DIGITS {
        return None;
    }
    Some(CallerNumber {
        e164: format!("+{code}{national}"),
        country: country_in_code(code, national)?,
    })
}

/// Phone (canonical serial) → the SIM country it reported last.
fn last_sims() -> &'static Mutex<BTreeMap<String, String>> {
    static M: OnceLock<Mutex<BTreeMap<String, String>>> = OnceLock::new();
    M.get_or_init(Default::default)
}

/// Remember the SIM country (`sim_country` of `incoming` / `hello`) a phone
/// reported. Anything but two letters is ignored.
pub fn note_sim(phone: &str, iso: &str) {
    let iso = iso.trim().to_ascii_lowercase();
    if iso.len() != 2 || !iso.bytes().all(|b| b.is_ascii_lowercase()) {
        return;
    }
    last_sims()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(phone.to_string(), iso);
}

/// The SIM country a phone reported on its last call (since the agent started).
/// While a call rings the phone may not have said yet; this stands in.
pub fn last_sim(phone: &str) -> Option<String> {
    last_sims()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(phone)
        .cloned()
}

/// Every SIM country the phones have reported since the agent started, sorted,
/// each once (the settings page lists them).
pub fn seen_sims() -> Vec<String> {
    let mut v: Vec<String> = last_sims()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .cloned()
        .collect();
    v.sort();
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn country(caller: &str, region: Option<&str>) -> Option<&'static str> {
        normalize(caller, region).map(|n| n.country)
    }

    fn e164(caller: &str, region: Option<&str>) -> Option<String> {
        normalize(caller, region).map(|n| n.e164)
    }

    #[test]
    fn numbers_are_read_the_way_the_sim_reads_them() {
        // Taiwan: national forms, +886, 00886, and the digits written without +.
        assert_eq!(
            normalize("0912-345-678", Some("tw")),
            Some(CallerNumber { e164: "+886912345678".to_string(), country: "tw" })
        );
        assert_eq!(e164("(02) 2345-6789", Some("tw")).as_deref(), Some("+886223456789"));
        assert_eq!(e164("+886 912 345 678", None).as_deref(), Some("+886912345678"));
        assert_eq!(e164("00886912345678", Some("us")).as_deref(), Some("+886912345678"));
        assert_eq!(e164("886912345678", Some("tw")).as_deref(), Some("+886912345678"));
        // The same national digits on another SIM belong to that SIM's country.
        assert_eq!(country("0912345678", Some("TW")), Some("tw"));
        assert_eq!(country("0312345678", Some("jp")), Some("jp"));
        // +1: ten digits, or eleven with the leading 1.
        assert_eq!(e164("(202) 555-0143", Some("us")).as_deref(), Some("+12025550143"));
        assert_eq!(e164("1-202-555-0143", Some("us")).as_deref(), Some("+12025550143"));
        assert_eq!(e164("0012025550143", Some("tw")).as_deref(), Some("+12025550143"));
        // Trunk prefixes: 0 almost everywhere, kept in Italy, 8 in Russia.
        assert_eq!(e164("03-1234-5678", Some("jp")).as_deref(), Some("+81312345678"));
        assert_eq!(e164("020 7946 0018", Some("gb")).as_deref(), Some("+442079460018"));
        assert_eq!(e164("06 1234 5678", Some("it")).as_deref(), Some("+390612345678"));
        assert_eq!(e164("8 912 345-67-89", Some("ru")).as_deref(), Some("+79123456789"));
    }

    #[test]
    fn shared_calling_codes_go_to_the_right_country() {
        assert_eq!(country("+12025550143", None), Some("us"));
        assert_eq!(country("+14165550123", None), Some("ca"));
        assert_eq!(country("5145550123", Some("us")), Some("ca"));
        assert_eq!(country("+18765551234", None), Some("jm"));
        assert_eq!(country("+17875551234", None), Some("pr"));
        assert_eq!(country("+18095551234", None), Some("do"));
        assert_eq!(country("+79123456789", None), Some("ru"));
        assert_eq!(country("+77012345678", None), Some("kz"));
        assert_eq!(country("+447700900123", None), Some("gb"));
        assert_eq!(country("+390612345678", None), Some("it"));
    }

    #[test]
    fn other_countries_by_their_code() {
        assert_eq!(country("+8613800138000", Some("tw")), Some("cn"));
        assert_eq!(country("+85291234567", Some("tw")), Some("hk"));
        assert_eq!(country("+85361234567", Some("tw")), Some("mo"));
        assert_eq!(country("+84912345678", Some("tw")), Some("vn"));
        assert_eq!(country("+66812345678", Some("tw")), Some("th"));
        assert_eq!(country("+6281234567890", Some("tw")), Some("id"));
        assert_eq!(country("+81312345678", Some("us")), Some("jp"));
        assert_eq!(country("+821012345678", None), Some("kr"));
        assert_eq!(country("+5215512345678", None), Some("mx"));
        assert_eq!(country("+351912345678", None), Some("pt"));
        assert_eq!(country("+353871234567", None), Some("ie"));
    }

    #[test]
    fn hidden_short_and_unplaceable_numbers_have_no_country() {
        for caller in [
            "", "  ", "unknown", "Private", "anonymous", "-2", "+12", "+1202", "165", "+999123456",
            "+80012345678", "+1 202 555 0143 0000 0000",
        ] {
            assert_eq!(normalize(caller, Some("tw")), None, "{caller}");
        }
        // A national number without a region, or a region nobody knows.
        assert_eq!(normalize("0912345678", None), None);
        assert_eq!(normalize("0912345678", Some("zz")), None);
    }

    #[test]
    fn calling_code_table_is_consistent() {
        assert_eq!(calling_code("tw"), Some("886"));
        assert_eq!(calling_code(" US "), Some("1"));
        assert_eq!(calling_code("zz"), None);
        assert_eq!(country_of_code("1"), Some("us"));
        assert_eq!(country_of_code("44"), Some("gb"));
        assert_eq!(country_of_code("886"), Some("tw"));
        assert_eq!(country_of_code(""), None);
        assert_eq!(country_of_code("999"), None);
        let mut seen = std::collections::HashSet::new();
        for &(iso, code) in COUNTRIES {
            assert!(iso.len() == 2 && iso.bytes().all(|b| b.is_ascii_lowercase()), "{iso}");
            assert!(seen.insert(iso), "{iso} twice");
            assert!(
                (1..=3).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_digit()),
                "{code}"
            );
            // Prefix-free: a number has one way to be read.
            for &(_, other) in COUNTRIES {
                assert!(other == code || !other.starts_with(code), "{code} starts {other}");
            }
        }
        assert_eq!(countries().count(), COUNTRIES.len());
        for &(iso, areas) in NANP_AREAS {
            assert_eq!(calling_code(iso), Some("1"), "{iso}");
            for area in areas {
                assert!(area.len() == 3 && area.bytes().all(|b| b.is_ascii_digit()), "{area}");
            }
        }
    }

    #[test]
    fn a_phone_keeps_its_last_sim() {
        note_sim("phone-region-test-a", "us");
        note_sim("phone-region-test-a", " TW ");
        assert_eq!(last_sim("phone-region-test-a").as_deref(), Some("tw"));
        assert_eq!(last_sim("phone-region-test-none"), None);
        note_sim("phone-region-test-b", "usa");
        note_sim("phone-region-test-b", "");
        assert_eq!(last_sim("phone-region-test-b"), None);
        let seen = seen_sims();
        assert!(seen.contains(&"tw".to_string()));
        assert!(seen.windows(2).all(|w| w[0] < w[1]));
    }
}
