//! The console's texts. Each language is a file in static/i18n/ ({"schema":1,
//! "lang","strings"}), compiled in. The page (static/calls.html) carries no
//! translations of its own: when the agent sends it, it puts English and the
//! language in use into it (the i18n-boot block), picked from the address
//! (?lang=), the language cookie and the browser's languages, in that order.
//! Another language is fetched when it is chosen (/calls/i18n/<code>.json).

use serde_json::{json, Map, Value};
use sha2::Digest;
use std::collections::HashMap;
use std::io::Write as _;
use std::sync::{Arc, Mutex, OnceLock};

/// The page, before the languages are put in.
const TEMPLATE: &str = include_str!("../static/calls.html");
/// Where the page takes its languages from.
const MARKER: &str = "__I18N_BOOT__";
/// The cookie the console writes when its language is chosen.
pub const COOKIE: &str = "pca_lang";

/// Every language the console has, English first: code and file.
const FILES: [(&str, &str); 9] = [
    ("en", include_str!("../static/i18n/en.json")),
    ("zh-TW", include_str!("../static/i18n/zh-TW.json")),
    ("zh-CN", include_str!("../static/i18n/zh-CN.json")),
    ("ja", include_str!("../static/i18n/ja.json")),
    ("ko", include_str!("../static/i18n/ko.json")),
    ("es", include_str!("../static/i18n/es.json")),
    ("fr", include_str!("../static/i18n/fr.json")),
    ("de", include_str!("../static/i18n/de.json")),
    ("pt", include_str!("../static/i18n/pt.json")),
];

/// The codes of the languages the console has.
pub fn codes() -> Vec<&'static str> {
    FILES.iter().map(|f| f.0).collect()
}

/// The code as written here, for a code that is exactly one of them.
pub fn exact(code: &str) -> Option<&'static str> {
    FILES.iter().map(|f| f.0).find(|c| *c == code)
}

fn file(code: &str) -> Option<&'static str> {
    FILES.iter().find(|f| f.0 == code).map(|f| f.1)
}

/// A language tag ("pt-BR", "zh_hant", "ja") as one of the console's languages.
/// The same rules as matchLang() in the page.
pub fn match_lang(s: &str) -> Option<&'static str> {
    let low = s.trim().to_lowercase();
    if low.is_empty() {
        return None;
    }
    if let Some(c) = FILES.iter().map(|f| f.0).find(|c| c.to_lowercase() == low) {
        return Some(c);
    }
    if low.starts_with("zh") {
        let traditional = ["hant", "tw", "hk", "mo"].iter().any(|k| low.contains(k));
        return exact(if traditional { "zh-TW" } else { "zh-CN" });
    }
    let base = low.split(['-', '_']).next().unwrap_or("");
    exact(base)
}

/// The first language of an Accept-Language header that the console has
/// (highest q first, the order written for equal q).
fn from_accept_language(header: &str) -> Option<&'static str> {
    let mut tags: Vec<(f32, usize, &str)> = Vec::new();
    for (i, part) in header.split(',').enumerate() {
        let mut it = part.split(';');
        let tag = it.next().unwrap_or("").trim();
        let q = it
            .filter_map(|p| p.trim().strip_prefix("q="))
            .find_map(|v| v.trim().parse::<f32>().ok())
            .unwrap_or(1.0);
        if !tag.is_empty() && q > 0.0 {
            tags.push((q, i, tag));
        }
    }
    tags.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then(a.1.cmp(&b.1)));
    tags.into_iter().find_map(|t| match_lang(t.2))
}

/// The language for a page request: the address (?lang=), else the cookie,
/// else the browser's languages, else English.
pub fn pick(query: Option<&str>, cookie: Option<&str>, accept_language: Option<&str>) -> &'static str {
    let from_query = query.and_then(|q| {
        url::form_urlencoded::parse(q.as_bytes())
            .find(|(k, _)| k == "lang")
            .and_then(|(_, v)| match_lang(&v))
    });
    let from_cookie = cookie.and_then(|h| {
        h.split(';')
            .filter_map(|p| p.trim().split_once('='))
            .find(|(k, _)| *k == COOKIE)
            .and_then(|(_, v)| match_lang(v))
    });
    from_query
        .or(from_cookie)
        .or_else(|| accept_language.and_then(from_accept_language))
        .unwrap_or("en")
}

/// A body ready to send: raw, gzip and a strong ETag, computed once.
pub struct Asset {
    pub body: String,
    pub gzip: Vec<u8>,
    pub etag: String,
}

fn asset(body: String) -> Asset {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    let _ = enc.write_all(body.as_bytes());
    let gzip = enc.finish().unwrap_or_default();
    let digest = sha2::Sha256::digest(body.as_bytes());
    let etag = format!("\"{}\"", digest.iter().take(8).map(|b| format!("{b:02x}")).collect::<String>());
    Asset { body, gzip, etag }
}

fn strings_of(code: &str) -> Option<Value> {
    serde_json::from_str::<Value>(file(code)?).ok()?.get("strings").cloned()
}

/// What the page reads at start: the language in use, the list of languages,
/// and the texts of English and of the language in use.
fn boot_json(code: &str) -> String {
    let mut strings = Map::new();
    for c in ["en", code] {
        if let Some(s) = strings_of(c) {
            strings.insert(c.to_string(), s);
        }
    }
    let v = json!({ "lang": code, "langs": codes(), "strings": Value::Object(strings) });
    // Inside a script block: no "</script" or "<!--" can come out of it.
    v.to_string().replace('<', "\\u003c")
}

/// The template with English and the language in use put in.
pub fn render(template: &str, code: &str) -> String {
    let code = exact(code).unwrap_or("en");
    template.replacen(MARKER, &boot_json(code), 1)
}

type AssetCache = OnceLock<Mutex<HashMap<&'static str, Arc<Asset>>>>;

/// A language's keys and texts (for tests of other places).
#[cfg(test)]
pub fn texts(code: &str) -> Vec<(String, String)> {
    match strings_of(code) {
        Some(Value::Object(m)) => m.into_iter().map(|(k, v)| (k, v.as_str().unwrap_or("").to_string())).collect(),
        _ => Vec::new(),
    }
}

fn cached(cache: &'static AssetCache, code: &'static str, make: impl Fn(&'static str) -> String) -> Arc<Asset> {
    let mut m = cache.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    m.entry(code).or_insert_with(|| Arc::new(asset(make(code)))).clone()
}

/// The page in one language (English for a code that is not one of them).
/// Each language has its own ETag.
pub fn page(code: &str) -> Arc<Asset> {
    static PAGES: AssetCache = OnceLock::new();
    cached(&PAGES, exact(code).unwrap_or("en"), |c| render(TEMPLATE, c))
}

/// One language's file, for the page to fetch when it is chosen.
pub fn language_file(code: &str) -> Option<Arc<Asset>> {
    static FILES_CACHE: AssetCache = OnceLock::new();
    Some(cached(&FILES_CACHE, exact(code)?, |c| file(c).unwrap_or("{}").to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// The {placeholders} of a text, sorted.
    fn placeholders(s: &str) -> Vec<String> {
        let mut out = Vec::new();
        let b: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < b.len() {
            if b[i] == '{' {
                if let Some(end) = b[i + 1..].iter().position(|c| *c == '}') {
                    let name: String = b[i + 1..i + 1 + end].iter().collect();
                    if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                        out.push(format!("{{{name}}}"));
                    }
                    i += end + 1;
                }
            }
            i += 1;
        }
        out.sort();
        out
    }

    /// The keys the markup asks for: data-i18n="key", data-i18n-ph="key" ...
    fn markup_keys(html: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut rest = html;
        while let Some(at) = rest.find("data-i18n") {
            rest = &rest[at + "data-i18n".len()..];
            let tail = rest.trim_start_matches(|c: char| c == '-' || c.is_ascii_lowercase());
            if let Some(v) = tail.strip_prefix("=\"") {
                if let Some(end) = v.find('"') {
                    out.insert(v[..end].to_string());
                }
            }
        }
        out
    }

    fn parsed(code: &str) -> Map<String, Value> {
        let doc: Value = serde_json::from_str(file(code).unwrap()).unwrap_or_else(|e| panic!("{code}.json: {e}"));
        assert_eq!(doc["schema"], 1, "{code}");
        assert_eq!(doc["lang"], code, "{code}");
        doc["strings"].as_object().unwrap_or_else(|| panic!("{code}: strings")).clone()
    }

    #[test]
    fn console_i18n_keys() {
        let en = parsed("en");
        for code in codes() {
            let s = parsed(code);
            // A key written twice would be read as the last one.
            let written = file(code).unwrap().lines().filter(|l| l.starts_with("    \"")).count();
            assert_eq!(written, s.len(), "{code}: a key is written twice");
            let missing: Vec<_> = en.keys().filter(|k| !s.contains_key(*k)).collect();
            let extra: Vec<_> = s.keys().filter(|k| !en.contains_key(*k)).collect();
            assert!(missing.is_empty() && extra.is_empty(), "{code}: missing {missing:?} extra {extra:?}");
            for (k, v) in &s {
                let v = v.as_str().unwrap_or_else(|| panic!("{code} {k}: not a text"));
                assert!(!v.trim().is_empty(), "{code} {k}: empty");
                assert_eq!(placeholders(v), placeholders(en[k].as_str().unwrap()), "{code} {k}: placeholders differ from English");
            }
        }
        // Every key the markup asks for is in English (the fallback of every language).
        let lost: Vec<_> = markup_keys(TEMPLATE).into_iter().filter(|k| k != "key" && !en.contains_key(k)).collect();
        assert!(lost.is_empty(), "markup keys missing from English: {lost:?}");
    }

    #[test]
    fn the_page_has_no_translations_of_its_own() {
        assert_eq!(TEMPLATE.matches(MARKER).count(), 1);
        assert!(!TEMPLATE.contains("I18N[\""), "a language block is back in calls.html");
    }

    #[test]
    fn every_mascot_region_is_a_pack_region_and_has_all_its_drawings() {
        let start = TEMPLATE.find("const TS_REGIONS = {").expect("TS_REGIONS") + "const TS_REGIONS = {".len();
        let block = &TEMPLATE[start..start + TEMPLATE[start..].find("\n};").expect("end of TS_REGIONS")];
        let regions = &crate::packs_store::builtin().regions;
        let has = |what: String| TEMPLATE.contains(&what);
        let (mut codes, mut scenes) = (Vec::new(), Vec::new());
        let mut prints = Vec::new();
        for line in block.lines() {
            if let Some((key, _)) = line.strip_prefix("  ").filter(|l| !l.starts_with(' ') && !l.starts_with("\"default\"")).and_then(|l| l.split_once(": { bow:")) {
                codes.push(key.trim_matches('"').to_string());
                prints.push(line.split("print: \"").nth(1).and_then(|r| r.split('"').next()).unwrap_or("").to_string());
            } else if let Some(rest) = line.strip_prefix("    scenes: ") {
                let v: Value = serde_json::from_str(rest.trim_end().trim_end_matches(',').trim_end_matches('}').trim_end()).unwrap_or_else(|e| panic!("scenes {rest}: {e}"));
                scenes.push(v);
            }
        }
        assert!(codes.len() >= 73, "only {} regions in TS_REGIONS", codes.len());
        assert_eq!(codes.len(), scenes.len());
        for (i, code) in codes.iter().enumerate() {
            assert!(regions.contains_key(code), "{code}: not a region of the built-in pack");
            assert!(!prints[i].is_empty() && has(format!("class=\"bts-print\" data-p=\"{}\"", prints[i])), "{code}: T-shirt print {:?} is not drawn", prints[i]);
            assert!(has(format!("class=\"bts-item\" data-r=\"{code}\"")), "{code}: no desk item");
            let list = scenes[i].as_array().unwrap();
            assert_eq!(list.len(), 3, "{code}: needs three views");
            for sc in list {
                for p in sc[1].as_array().unwrap() {
                    let p = p.as_str().unwrap();
                    assert!(has(format!("class=\"bts-prop\" data-p=\"{p}\"")), "{code}: view prop {p} is not drawn");
                }
            }
        }
        for code in regions.keys().filter(|c| *c != "default") {
            assert!(codes.contains(code), "pack region {code} has no mascot");
        }
    }

    #[test]
    fn the_language_is_picked_from_address_then_cookie_then_browser() {
        let cookie = Some("a=b; pca_lang=ja; c=d");
        let al = Some("fr-CA,fr;q=0.9,en;q=0.8");
        assert_eq!(pick(Some("x=1&lang=de"), cookie, al), "de");
        assert_eq!(pick(Some("x=1"), cookie, al), "ja");
        assert_eq!(pick(None, None, al), "fr");
        assert_eq!(pick(Some("lang=xx"), None, Some("xx,pt-BR")), "pt");
        assert_eq!(pick(None, Some("pca_lang=xx"), Some("de;q=0.2, ko;q=0.7")), "ko");
        assert_eq!(pick(None, None, Some("ar,it;q=0.5")), "en");
        assert_eq!(pick(None, None, Some("zh-HK")), "zh-TW");
        assert_eq!(pick(None, None, Some("zh-Hans")), "zh-CN");
        assert_eq!(pick(None, None, Some("zh")), "zh-CN");
        assert_eq!(pick(None, None, Some("de;q=0")), "en");
        assert_eq!(pick(None, None, None), "en");
    }

    #[test]
    fn the_page_carries_english_and_the_language_in_use() {
        use std::io::Read;
        for code in codes() {
            let p = page(code);
            let start = p.body.find("<script id=\"i18n-boot\"").unwrap();
            let after = &p.body[start..];
            let json = &after[after.find('>').unwrap() + 1..after.find("</script>").unwrap()];
            let boot: Value = serde_json::from_str(json).unwrap();
            assert_eq!(boot["lang"], code);
            assert_eq!(boot["langs"].as_array().unwrap().len(), FILES.len());
            let have: BTreeSet<_> = boot["strings"].as_object().unwrap().keys().cloned().collect();
            let want: BTreeSet<_> = ["en", code].iter().map(|s| s.to_string()).collect();
            assert_eq!(have, want, "{code}");
            assert_eq!(boot["strings"][code], Value::Object(parsed(code)), "{code}");
            assert!(!p.body.contains(MARKER));
            // Same bytes back from the gzip, smaller than the page, own ETag.
            let mut out = String::new();
            flate2::read::GzDecoder::new(&p.gzip[..]).read_to_string(&mut out).unwrap();
            assert_eq!(out, p.body);
            assert!(p.gzip.len() < p.body.len() / 2);
        }
        assert_ne!(page("ja").etag, page("de").etag);
        assert_eq!(page("ja").etag, page("ja").etag);
        // A text can never end the script block.
        assert!(!boot_json("en").contains("</"));
        assert_eq!(page("xx").etag, page("en").etag);
    }

    #[test]
    fn a_language_file_is_the_same_text_as_compiled_in() {
        for code in codes() {
            let a = language_file(code).unwrap();
            assert_eq!(a.body, file(code).unwrap());
        }
        assert!(language_file("ar").is_none());
        assert!(language_file("../en").is_none());
    }

    #[test]
    fn only_the_languages_it_has_can_be_asked_for() {
        let long = "a".repeat(100_000);
        for bad in [
            "", " ", "..", "../en", "..\\en", "/etc/passwd", "C:\\Windows\\win.ini", "%2e%2e", "%2e%2e%2fen", "en\0", "\0en",
            "EN", "Zh-tw", "en.json", "en/", "./en", "en ", " en", "en\n", "zh_TW", "en%00", "<script>", long.as_str(),
        ] {
            assert!(language_file(bad).is_none(), "{bad:?}");
            assert!(exact(bad).is_none(), "{bad:?}");
            // A page asked for in such a code is the English one.
            assert_eq!(page(bad).etag, page("en").etag, "{bad:?}");
        }
        for code in codes() {
            assert_eq!(exact(code), Some(code));
            assert!(language_file(code).is_some(), "{code}");
        }
    }

    #[test]
    fn what_the_browser_sends_never_leaves_the_languages_it_has() {
        let long = "x,".repeat(60_000);
        let hostile = [
            "", "\0", "<script>alert(1)</script>", "\"><img src=x>", "ja\r\nSet-Cookie: a=b", "../../etc/passwd", "ja%00", "%6ja",
            "ja\u{202e}", "zh-TW\u{0}", long.as_str(), ";;;;", "q=1", "ja;q=NaN", "ja;q=inf", "ja;q=-1", "*", "\u{ff4a}\u{ff41}",
        ];
        let all = codes();
        for v in hostile {
            let q = format!("lang={v}");
            let c = format!("pca_lang={v}");
            for got in [
                pick(Some(&q), None, None),
                pick(None, Some(&c), None),
                pick(None, None, Some(v)),
                pick(Some(&q), Some(&c), Some(v)),
            ] {
                assert!(all.contains(&got), "{v:?} -> {got}");
            }
        }
        // An empty or unknown value in front does not hide a good one behind it.
        assert_eq!(pick(Some("lang="), Some("pca_lang=ko"), Some("de")), "ko");
        assert_eq!(pick(Some("lang=zz"), Some("pca_lang=zz"), Some("de")), "de");
        // Another cookie of the same kind of name is not the language cookie.
        assert_eq!(pick(None, Some("xpca_lang=ja; pca_lang_x=ja"), None), "en");
        // A percent-encoded address value is decoded like the browser wrote it.
        assert_eq!(pick(Some("lang=zh%2DTW"), None, None), "zh-TW");
    }

    #[test]
    fn no_text_can_break_out_of_the_script_block_of_any_page() {
        for code in codes() {
            let p = page(code);
            let start = p.body.find("<script id=\"i18n-boot\"").unwrap();
            let after = &p.body[start..];
            let json = &after[after.find('>').unwrap() + 1..after.find("</script>").unwrap()];
            // Nothing in it can open a tag or a comment, whatever a text holds.
            assert!(!json.contains('<'), "{code}");
            assert!(!json.contains("-->") || !json.contains("<!--"), "{code}");
            let boot: Value = serde_json::from_str(json).unwrap();
            assert_eq!(boot["strings"][code], Value::Object(parsed(code)), "{code}");
        }
        // A text with the dangerous characters comes back whole after the escaping.
        let nasty = "</script><!-- <img onerror=1> --> \u{2028}";
        let json = json!({ "strings": { "en": { "k": nasty } } }).to_string().replace('<', "\\u003c");
        assert!(!json.contains('<'));
        let back: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(back["strings"]["en"]["k"], nasty);
    }
}
