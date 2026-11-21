// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The lines a voice pack of a region is recorded with: three preview lines
//! (what the AI says in the settings preview, with the caller's gaps between)
//! and the blocked-number announcement, in the way people of that region talk
//! on the phone. A region without lines here has no pack of its own: its language's
//! common lines ([crate::locale::phrases]) are used.
//!
//! The data is `data/voices/lines.json`, next to the voice index: the lines
//! and the recordings change together (a changed line means a new pack), and
//! neither is part of the prompt data pack, whose size is capped for every call.
//! No names, numbers or companies: a line is something anybody says.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::locale::Lang;

const DATA: &str = include_str!("../data/voices/lines.json");
const FORMAT: u64 = 1;

/// One region's lines.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegionLines {
    /// The language of the lines, as [Lang::code] writes it (`zh-TW`, `en`…): the
    /// pack's id and the prompts' language.
    pub lang: String,
    /// The language code Gemini is given for the recording: one of
    /// [bageo_packs::data::OFFICIAL_LANGS] (Chinese is `cmn`, never `cmn-TW`).
    pub gemini_lang: String,
    pub preview: Vec<String>,
    pub reject: String,
}

impl RegionLines {
    pub(crate) fn lang(&self) -> Option<Lang> {
        Lang::parse_any(&self.lang)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    format: u64,
    regions: BTreeMap<String, RegionLines>,
}

fn file() -> &'static File {
    static F: OnceLock<File> = OnceLock::new();
    F.get_or_init(|| {
        let f: File = serde_json::from_str(DATA).expect("voices/lines.json reads");
        assert_eq!(f.format, FORMAT, "voices/lines.json format");
        f
    })
}

/// The lines of the region with this code (`tw`, `us`…, any case).
pub(crate) fn for_region(code: &str) -> Option<&'static RegionLines> {
    file().regions.get(code.trim().to_ascii_lowercase().as_str())
}

/// Every region that has lines, by code.
pub(crate) fn codes() -> Vec<&'static str> {
    file().regions.keys().map(String::as_str).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Seconds a plain reading takes, roughly: 4.5 CJK characters or 14 other letters a second.
    fn speech_secs(line: &str) -> f32 {
        let (wide, narrow) = line
            .chars()
            .filter(|c| c.is_alphanumeric())
            .fold((0u32, 0u32), |(w, n), c| if c >= '\u{2E80}' { (w + 1, n) } else { (w, n + 1) });
        wide as f32 / 4.5 + narrow as f32 / 14.0
    }

    #[test]
    fn every_region_has_three_short_lines_in_a_language_the_agent_speaks() {
        for code in ["tw", "us", "jp", "kr", "cn", "de", "fr", "br", "mx", "es", "gb-ie"] {
            assert!(codes().contains(&code), "{code}");
        }
        for code in codes() {
            let r = for_region(code).unwrap();
            assert!(r.lang().is_some(), "{code}: {}", r.lang);
            assert!(bageo_packs::data::is_official_lang(&r.gemini_lang), "{code}: {} is not a Gemini Live language", r.gemini_lang);
            assert!(crate::country_profile::Region::of_code(code).is_some(), "{code} is not a region of the data pack");
            assert_eq!(r.preview.len(), crate::voice_preview::LINES, "{code}");
            for line in &r.preview {
                let s = speech_secs(line);
                assert!((1.5..=4.5).contains(&s), "{code}: {line:?} takes about {s:.1} s");
            }
            assert!(!r.reject.trim().is_empty(), "{code}");
        }
    }

    #[test]
    fn lines_carry_no_number_address_or_link() {
        for code in codes() {
            let r = for_region(code).unwrap();
            for line in r.preview.iter().chain(std::iter::once(&r.reject)) {
                assert!(!line.chars().any(|c| c.is_numeric() || matches!(c, '@' | '/' | '#' | '{' | '}')), "{code}: {line:?}");
                assert!(!line.to_ascii_lowercase().contains("http"), "{code}: {line:?}");
            }
        }
    }

    #[test]
    fn gemini_is_given_official_codes_only() {
        assert_eq!(for_region("tw").map(|r| r.gemini_lang.as_str()), Some("cmn"));
        assert_eq!(for_region("us").map(|r| r.gemini_lang.as_str()), Some("en-US"));
        // What a query returned or the settings use is not on the list.
        for bad in ["cmn-TW", "zh-TW", "zh-HK", "yue", "nan", "es-ES", "pt-PT", "en-GB", "cmn-CN"] {
            assert!(!bageo_packs::data::is_official_lang(bad), "{bad}");
        }
        for good in ["cmn", "en-US", "es-US", "pt-BR", "ja-JP", "ko-KR", "de-DE", "fr-FR"] {
            assert!(bageo_packs::data::is_official_lang(good), "{good}");
        }
    }

    #[test]
    fn regions_of_one_language_have_packs_of_their_own() {
        use std::collections::HashSet;
        let mut ids = HashSet::new();
        for code in codes() {
            let lang = for_region(code).and_then(|r| r.lang()).unwrap();
            let id = crate::voice_packs::folder_id(lang.code(), Some(code));
            assert!(crate::voice_packs::valid_id(&id), "{id}");
            assert!(ids.insert(id.clone()), "{id} is made twice");
        }
        for id in ["es-mx", "es-es", "en-us", "en-gb-ie", "pt-br", "zh-cn-cn", "zh-tw-tw"] {
            assert!(ids.contains(id), "{id}");
        }
        // Same language and Gemini code, different lines.
        for (a, b) in [("mx", "es"), ("us", "gb-ie")] {
            let (x, y) = (for_region(a).unwrap(), for_region(b).unwrap());
            assert_eq!((x.lang(), &x.gemini_lang), (y.lang(), &y.gemini_lang), "{a} {b}");
            assert_ne!(x.preview, y.preview, "{a} {b}");
            assert_ne!(x.reject, y.reject, "{a} {b}");
        }
    }

    #[test]
    fn region_and_language_are_looked_up_in_any_case() {
        assert_eq!(for_region(" TW ").map(|r| r.lang()), Some(Some(Lang::ZhTw)));
        assert_eq!(for_region("us").and_then(|r| r.lang()), Some(Lang::En));
        assert!(for_region("zz").is_none());
        assert!(for_region("").is_none());
    }
}
