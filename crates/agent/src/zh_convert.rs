// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Pure-Rust Chinese script conversion backed by OpenCC (`ver.1.4.2`, Apache-2.0)
//! dictionaries embedded at compile time.
//!
//! - [`to_tw`]: Simplified → Traditional Chinese (Taiwan standard), OpenCC `s2tw` chain
//!   (`STPhrases` + `STCharacters` → `TWVariantsPhrases` + `TWVariants`).
//! - [`to_cn`]: Traditional (Taiwan) → Simplified Chinese, OpenCC `tw2s` chain
//!   (`TWVariantsRevPhrases` + reversed `TWVariants` → `TSPhrases` + `TSCharacters`)
//!   plus corner-quote normalisation (`「」` → `“”`, `『』` → `‘’`).

use std::collections::HashMap;
use std::sync::OnceLock;

const ST_CHARACTERS: &str = include_str!("../data/opencc/STCharacters.txt");
const ST_PHRASES: &str = include_str!("../data/opencc/STPhrases.txt");
const TW_VARIANTS: &str = include_str!("../data/opencc/TWVariants.txt");
const TW_VARIANTS_PHRASES: &str = include_str!("../data/opencc/TWVariantsPhrases.txt");
const TW_VARIANTS_REV_PHRASES: &str = include_str!("../data/opencc/TWVariantsRevPhrases.txt");
const TS_CHARACTERS: &str = include_str!("../data/opencc/TSCharacters.txt");
const TS_PHRASES: &str = include_str!("../data/opencc/TSPhrases.txt");

struct Stage {
    single: HashMap<char, String>,
    /// Multi-character keys indexed by their first two Unicode scalar values.
    /// Each bucket holds `(remaining_key_bytes, char_len_of_remaining, replacement)`,
    /// sorted by `char_len_of_remaining` descending for greedy longest-prefix match.
    phrases: HashMap<(char, char), Vec<(String, usize, String)>>,
}

impl Stage {
    fn from_pairs(entries: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut single: HashMap<char, String> = HashMap::new();
        let mut seen_phrases: HashMap<String, String> = HashMap::new();
        for (key, val) in entries {
            if key.is_empty() {
                continue;
            }
            let mut chars = key.chars();
            let Some(c0) = chars.next() else { continue };
            if chars.next().is_none() {
                single.entry(c0).or_insert(val);
            } else {
                seen_phrases.entry(key).or_insert(val);
            }
        }

        let mut phrases: HashMap<(char, char), Vec<(String, usize, String)>> = HashMap::new();
        for (key, val) in seen_phrases {
            let mut chars = key.chars();
            let c0 = chars.next().unwrap();
            let c1 = chars.next().unwrap();
            let rest: String = chars.collect();
            let rest_chars = rest.chars().count();
            phrases
                .entry((c0, c1))
                .or_default()
                .push((rest, rest_chars, val));
        }
        for bucket in phrases.values_mut() {
            bucket.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        }
        Self { single, phrases }
    }

    fn apply(&self, text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        let mut out = String::with_capacity(text.len());
        let mut pos = 0;
        while pos < text.len() {
            let tail = &text[pos..];
            let mut chars = tail.chars();
            let c0 = chars.next().unwrap();
            let c0_len = c0.len_utf8();
            if let Some(c1) = chars.next() {
                if let Some(bucket) = self.phrases.get(&(c0, c1)) {
                    let after_pair = &tail[c0_len + c1.len_utf8()..];
                    if let Some((rest, _, val)) = bucket
                        .iter()
                        .find(|(rest, _, _)| after_pair.starts_with(rest.as_str()))
                    {
                        out.push_str(val);
                        pos += c0_len + c1.len_utf8() + rest.len();
                        continue;
                    }
                }
            }
            if let Some(val) = self.single.get(&c0) {
                out.push_str(val);
            } else {
                out.push(c0);
            }
            pos += c0_len;
        }
        out
    }
}

fn parse_dict(raw: &'static str) -> impl Iterator<Item = (String, String)> {
    raw.lines().filter_map(|line| {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let (key, vals) = line.split_once('\t')?;
        let first = vals.split(' ').next()?;
        if key.is_empty() || first.is_empty() {
            return None;
        }
        Some((key.to_string(), first.to_string()))
    })
}

/// Reverses an OpenCC variant dictionary the same way OpenCC's `data/scripts/reverse.py`
/// does, respecting `# @reverse-prefer: <key> [preferred_value]` directives.
fn reverse_dict(raw: &'static str) -> Vec<(String, String)> {
    let mut prefer: HashMap<String, String> = HashMap::new();
    let mut rev: HashMap<String, Vec<String>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("# @reverse-prefer:") {
            let fields: Vec<&str> = rest.split_whitespace().collect();
            if let (Some(&k), Some(&v)) = (fields.first(), fields.last()) {
                prefer.insert(k.to_string(), v.to_string());
            }
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, vals)) = line.split_once('\t') else {
            continue;
        };
        for v in vals.split(' ').filter(|s| !s.is_empty()) {
            let entry = rev.entry(v.to_string()).or_insert_with(|| {
                order.push(v.to_string());
                Vec::new()
            });
            if !entry.iter().any(|x| x == key) {
                entry.push(key.to_string());
            }
        }
    }
    order
        .into_iter()
        .filter_map(|k| {
            let vals = rev.get(&k)?;
            let chosen = prefer
                .get(&k)
                .and_then(|p| vals.iter().find(|&x| x == p))
                .or_else(|| vals.first())?;
            Some((k, chosen.clone()))
        })
        .collect()
}

struct Tables {
    s2t_stage: Stage,
    tw_stage: Stage,
    tw_rev_stage: Stage,
    t2s_stage: Stage,
}

fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| {
        let s2t_stage = Stage::from_pairs(parse_dict(ST_PHRASES).chain(parse_dict(ST_CHARACTERS)));
        let tw_stage =
            Stage::from_pairs(parse_dict(TW_VARIANTS_PHRASES).chain(parse_dict(TW_VARIANTS)));
        let tw_rev_stage =
            Stage::from_pairs(parse_dict(TW_VARIANTS_REV_PHRASES).chain(reverse_dict(TW_VARIANTS)));
        let t2s_stage = Stage::from_pairs(parse_dict(TS_PHRASES).chain(parse_dict(TS_CHARACTERS)));

        Tables {
            s2t_stage,
            tw_stage,
            tw_rev_stage,
            t2s_stage,
        }
    })
}

/// Simplified → Traditional Chinese (Taiwan standard character forms).
pub fn to_tw(text: &str) -> String {
    if text.is_empty() || !has_cjk(text) {
        return text.to_string();
    }
    let t = tables();
    t.tw_stage.apply(&t.s2t_stage.apply(text))
}

/// Traditional Chinese (Taiwan) → Simplified Chinese.
pub fn to_cn(text: &str) -> String {
    if text.is_empty() || (!has_cjk(text) && !has_corner_quotes(text)) {
        return text.to_string();
    }
    let t = tables();
    t.t2s_stage
        .apply(&t.tw_rev_stage.apply(text))
        .chars()
        .map(|c| match c {
            '「' => '“',
            '」' => '”',
            '『' => '‘',
            '』' => '’',
            other => other,
        })
        .collect()
}

fn has_corner_quotes(text: &str) -> bool {
    text.chars().any(|c| matches!(c, '「' | '」' | '『' | '』'))
}

fn has_cjk(text: &str) -> bool {
    text.chars().any(|c| {
        ('\u{3400}'..='\u{9fff}').contains(&c)
            || ('\u{f900}'..='\u{faff}').contains(&c)
            || ('\u{20000}'..='\u{2ebef}').contains(&c)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_simplified_to_taiwan_traditional() {
        assert_eq!(to_tw("要联系"), "要聯繫");
        assert_eq!(to_tw("对方表示没有事情要转达"), "對方表示沒有事情要轉達");
        assert_eq!(
            to_tw("我这边是管理室，你有一个包裹"),
            "我這邊是管理室，你有一個包裹"
        );
        assert_eq!(to_tw("听录音"), "聽錄音");
        assert_eq!(to_tw("里面着重说明为什么"), "裡面著重說明為什麼");
        assert_eq!(to_tw("喂，你好，回复消息"), "喂，你好，回覆消息");
        assert_eq!(to_tw("Caller: 0900"), "Caller: 0900");
        assert_eq!(to_tw("已是繁體"), "已是繁體");
    }

    #[test]
    fn converts_taiwan_traditional_to_simplified() {
        assert_eq!(to_cn("對方：沒說是誰"), "对方：没说是谁");
        assert_eq!(to_cn("一句話簡體中文摘要"), "一句话简体中文摘要");
        assert_eq!(
            to_cn("這裡的例句只是抓語氣和感覺"),
            "这里的例句只是抓语气和感觉"
        );
        assert_eq!(to_cn("裡面著重說明為什麼"), "里面着重说明为什么");
        assert_eq!(to_cn("也不要說「沒印象」"), "也不要说“没印象”");
    }

    #[test]
    fn respects_taiwan_variant_phrase_exceptions() {
        // TWVariantsPhrases keeps 張棟樑 intact instead of mapping 樑 → 梁.
        assert_eq!(to_tw("张栋梁"), "張棟樑");
        // Reverse preference keeps 么 → 幺 for single character while phrases map 什麼 → 什么.
        assert_eq!(to_cn("什麼"), "什么");
    }
}
