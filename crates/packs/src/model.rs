// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The bundle (bageo-pack.json) and the release index. Reading is tolerant:
//! fields this version does not know are ignored, so a newer pack can add
//! fields without breaking an older Bageo.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A line the model reads: English, and traditional Chinese (simplified is
/// converted by Bageo when it is shown).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Text {
    pub en: String,
    pub zh: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Name {
    pub en: String,
    pub native: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zh: Option<String>,
}

/// The opening word, in the local script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub hello: String,
    pub hello_again: String,
}

/// The short word a delivery spot is recognised by, per language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryKey {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub en: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zh: Option<String>,
}

const KEYS: [&str; 5] = ["1", "2", "3", "4", "5"];

/// The talk-style slider: five levels for the owner's own voice and five for
/// the assistant. In JSON each is an object with exactly the keys "1".."5".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TalkStyle {
    pub persona: [Text; 5],
    pub assistant: [Text; 5],
}

impl Serialize for TalkStyle {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Raw<'a> {
            persona: BTreeMap<&'static str, &'a Text>,
            assistant: BTreeMap<&'static str, &'a Text>,
        }
        Raw {
            persona: KEYS.iter().copied().zip(self.persona.iter()).collect(),
            assistant: KEYS.iter().copied().zip(self.assistant.iter()).collect(),
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for TalkStyle {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            persona: BTreeMap<String, Text>,
            assistant: BTreeMap<String, Text>,
        }
        let raw = Raw::deserialize(d)?;
        let persona = five(raw.persona, "persona").map_err(serde::de::Error::custom)?;
        let assistant = five(raw.assistant, "assistant").map_err(serde::de::Error::custom)?;
        Ok(TalkStyle { persona, assistant })
    }
}

fn five(map: BTreeMap<String, Text>, who: &str) -> Result<[Text; 5], String> {
    if let Some(k) = map.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!("talk_style.{who}: unexpected key \"{k}\" (only \"1\" to \"5\")"));
    }
    let mut out = Vec::with_capacity(5);
    for k in KEYS {
        out.push(map.get(k).cloned().ok_or_else(|| format!("talk_style.{who}: key \"{k}\" is missing"))?);
    }
    <[Text; 5]>::try_from(out).map_err(|_| format!("talk_style.{who}: needs five levels"))
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One region as the agent reads it: already merged with its language layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionData {
    pub code: String,
    pub countries: Vec<String>,
    pub name: Name,
    pub lang: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stt: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub other_langs: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub chinese_dialects: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hello: Option<Hello>,
    pub delivery: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_key: Option<DeliveryKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readback: Option<Text>,
    pub fraud: Text,
    pub habits: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall_local: Option<Text>,
    pub talk_style: TalkStyle,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vocab: BTreeMap<String, Vec<String>>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_on: Option<String>,
}

/// A language layer's entry in the bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LangInfo {
    pub name: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stt: Option<String>,
}

/// One source file the bundle was built from. kind is "region" or "layer".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub kind: String,
    pub id: String,
    pub status: String,
    pub sha256: String,
}

/// Secondary `(region, lang)` speech/style override from `regions_multilang.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultilangEntry {
    pub region: String,
    pub lang: String,
    pub hello: Hello,
    pub habits: Text,
    pub talk_style: TalkStyle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bundle {
    pub format: u32,
    /// YYYYMMDDNN.
    pub version: u64,
    pub built_at: String,
    pub source_commit: String,
    pub checker: String,
    pub manifest: Vec<Entry>,
    /// Country (ISO, lower case) to region code.
    pub countries: BTreeMap<String, String>,
    pub languages: BTreeMap<String, LangInfo>,
    pub regions: BTreeMap<String, RegionData>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub multilang: Vec<MultilangEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bubbles: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub place_habits: Vec<serde_json::Value>,
}

/// bageo-pack-index.json, published next to the bundle: what to download and
/// how to verify it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    pub format: u32,
    pub version: u64,
    /// A plain file name, never a path.
    pub file: String,
    pub bytes: u64,
    pub sha256: String,
    /// The oldest Bageo that can use this pack (x.y.z).
    pub min_bageo: String,
    /// The translations download (bageo-locales.json.gz), when the release has
    /// one. A malformed entry reads as absent, so it can never cost the
    /// region pack; older Bageo ignores the field altogether.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "tolerant_locales")]
    pub locales: Option<LocalesRef>,
    /// The scam database download (bageo-scams.json.gz), when the release has
    /// one. Tolerant just like `locales`; older Bageo ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "tolerant_locales")]
    pub scams: Option<LocalesRef>,
}

/// The translations file listed in the release index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalesRef {
    /// A plain file name, never a path.
    pub file: String,
    pub bytes: u64,
    pub sha256: String,
    pub format: u32,
    /// YYYYMMDDNN.
    pub version: u64,
    /// The oldest Bageo that can use these translations (x.y.z).
    pub min_bageo: String,
}

fn tolerant_locales<'de, D: Deserializer<'de>>(d: D) -> Result<Option<LocalesRef>, D::Error> {
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(v.and_then(|v| serde_json::from_value(v).ok()))
}
