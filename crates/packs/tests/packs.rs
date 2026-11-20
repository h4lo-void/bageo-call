// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Each rule has a pack that passes and a pack that is refused. The sample
//! data in tests/fixtures is invented.

use std::path::PathBuf;

use bageo_packs::source::{SrcRegion, SrcText};
use bageo_packs::*;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const BUILT: &str = "2026-01-01T00:00:00Z";
const VERSION: u64 = 2026010101;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn valid() -> Sources {
    load_sources(&fixture("valid")).expect("the valid fixture loads")
}

fn built() -> Bundle {
    build_at(&valid(), VERSION, COMMIT, BUILT).expect("the valid fixture builds")
}

fn issues_of(f: impl FnOnce(&mut Sources)) -> Vec<Issue> {
    let mut s = valid();
    f(&mut s);
    check_sources(&s)
}

fn region<'a>(s: &'a mut Sources, id: &str) -> &'a mut SrcRegion {
    &mut s.regions.iter_mut().find(|r| r.id == id).expect("region in the fixture").data
}

fn text(en: &str, zh: &str) -> SrcText {
    SrcText { en: en.to_string(), zh: zh.to_string() }
}

fn has(issues: &[Issue], rule: &str, field: &str) -> bool {
    issues.iter().any(|i| i.rule == rule && i.field.contains(field))
}

#[track_caller]
fn expect(issues: &[Issue], rule: &str, field: &str) {
    assert!(
        has(issues, rule, field),
        "expected {rule} on {field}, got:\n{}",
        issues.iter().map(|i| i.to_string()).collect::<Vec<_>>().join("\n")
    );
}

#[track_caller]
fn expect_none(issues: &[Issue], rule: &str) {
    assert!(
        !issues.iter().any(|i| i.rule == rule),
        "unexpected {rule}:\n{}",
        issues.iter().map(|i| i.to_string()).collect::<Vec<_>>().join("\n")
    );
}

// ---- the good pack ---------------------------------------------------------------------

#[test]
fn the_valid_fixture_passes_every_check() {
    let issues = check_sources(&valid());
    assert!(issues.is_empty(), "{issues:#?}");
}

#[test]
fn the_valid_fixture_builds_merges_and_reads_back() {
    let b = built();
    assert_eq!(b.regions.keys().map(String::as_str).collect::<Vec<_>>(), ["jp", "th"]);
    assert_eq!(b.countries.get("jp").map(String::as_str), Some("jp"));
    assert_eq!(b.countries.get("th").map(String::as_str), Some("th"));
    assert!(b.languages.contains_key("en"));
    assert_eq!(b.manifest.len(), 3);
    assert!(b.manifest.iter().all(|e| e.status == "draft" && e.sha256.len() == 64));

    let jp = &b.regions["jp"];
    // A level the region does not write comes from the layer; one it writes wins.
    assert!(jp.talk_style.persona[0].en.starts_with("Talk like a close friend"));
    assert!(jp.talk_style.persona[2].en.starts_with("Calm and polite"));
    assert!(jp.talk_style.assistant[0].en.contains("{owner}"));
    // Habits: the layer first, then the region.
    assert_eq!(jp.habits.en, "Keep sentences short and friendly. Polite です・ます speech with strangers; casual with friends.");
    // Vocab: the region's words first, then the layer's.
    assert_eq!(jp.vocab["delivery"], ["宅配便", "置き配", "parcel", "front desk"]);
    // The layer fills what the region leaves out.
    assert_eq!(b.regions["th"].fraud, jp_free_fraud(&b));
    assert!(b.regions["th"].stall_local.is_some());
    assert_eq!(b.regions["th"].hello.as_ref().map(|h| h.hello.as_str()), Some("สวัสดีครับ?"));
    assert_eq!(b.regions["th"].stt.as_deref(), Some("th-TH"));
    // Notes never reach the bundle.
    let json = serde_json::to_string(&b).unwrap();
    assert!(!json.contains("notes") && !json.contains("Invented sample"));

    let back = parse_bundle(&json).expect("the bundle reads back");
    assert_eq!(back, b);
}

fn jp_free_fraud(b: &Bundle) -> Text {
    // The layer's line, which Thailand did not override.
    b.languages.get("en").map(|_| ()).expect("layer in the bundle");
    Text {
        en: "Anything about unlocking an account or moving money to a safe account is a scam".into(),
        zh: "說要解鎖帳戶或轉到安全帳戶的都是詐騙".into(),
    }
}

// ---- 1 format and unknown fields -----------------------------------------------------------

#[test]
fn an_unknown_field_is_refused_in_a_source_file() {
    let err = load_sources(&fixture("bad_unknown_field")).expect_err("must be refused");
    expect(&err, rule::UNKNOWN_FIELD, "");
    assert!(err[0].detail.contains("extra_field"), "{err:?}");
    assert_eq!(err[0].file, "regions/jp.json");
}

#[test]
fn an_unknown_field_is_tolerated_in_a_bundle() {
    let mut v = serde_json::to_value(built()).unwrap();
    v["from_the_future"] = serde_json::json!({"a": 1});
    v["regions"]["jp"]["new_field"] = serde_json::json!("x");
    v["regions"]["jp"]["name"]["new_field"] = serde_json::json!(2);
    v["manifest"][0]["new_field"] = serde_json::json!(true);
    let b = parse_bundle(&v.to_string()).expect("unknown fields are ignored");
    assert_eq!(b.regions.len(), 2);
}

#[test]
fn a_newer_format_is_refused_with_a_plain_message() {
    let mut v = serde_json::to_value(built()).unwrap();
    v["format"] = serde_json::json!(2);
    v["regions"] = serde_json::json!({"anything": [1, 2, 3]});
    let err = parse_bundle(&v.to_string()).expect_err("newer format");
    expect(&err, rule::FORMAT, "format");
    assert!(err[0].detail.contains("update Bageo"), "{err:?}");
}

#[test]
fn the_wrong_schema_number_is_refused() {
    let i = issues_of(|s| region(s, "jp").schema = 2);
    expect(&i, rule::FORMAT, "schema");
}

#[test]
fn a_talk_style_key_other_than_1_to_5_is_refused_in_a_bundle() {
    let mut v = serde_json::to_value(built()).unwrap();
    v["regions"]["jp"]["talk_style"]["persona"]["6"] = serde_json::json!({"en": "x", "zh": "y"});
    let err = parse_bundle(&v.to_string()).expect_err("extra level");
    expect(&err, rule::FORMAT, "");
    assert!(err[0].detail.contains("\"6\""), "{err:?}");

    let mut v = serde_json::to_value(built()).unwrap();
    v["regions"]["jp"]["talk_style"]["assistant"].as_object_mut().unwrap().remove("4");
    assert!(parse_bundle(&v.to_string()).is_err());
}

// ---- 2 codes ---------------------------------------------------------------------------------

#[test]
fn a_code_must_have_the_right_shape_and_match_the_file_name() {
    let i = issues_of(|s| region(s, "jp").code = "xx".into());
    expect(&i, rule::CODE, "code");
    let i = issues_of(|s| region(s, "jp").code = "Bad_Code".into());
    expect(&i, rule::CODE, "code");
    let i = issues_of(|s| region(s, "th").code = "jp".into());
    expect(&i, rule::CODE, "code");
}

#[test]
fn a_layer_that_does_not_exist_is_refused() {
    let i = issues_of(|s| region(s, "jp").layer = Some("zz".into()));
    expect(&i, rule::CODE, "layer");
}

// ---- 3 countries ------------------------------------------------------------------------------

#[test]
fn a_country_in_two_regions_is_refused() {
    let i = issues_of(|s| region(s, "th").countries = vec!["th".into(), "jp".into()]);
    expect(&i, rule::COUNTRY, "countries");
}

#[test]
fn a_country_must_be_a_real_lower_case_code() {
    for bad in ["zz", "JP", "jpn", ""] {
        let i = issues_of(|s| region(s, "jp").countries = vec![bad.into()]);
        expect(&i, rule::COUNTRY, "countries");
    }
    let i = issues_of(|s| region(s, "jp").countries = vec!["jp".into(), "jp".into()]);
    expect(&i, rule::COUNTRY, "countries");
    let i = issues_of(|s| region(s, "jp").countries = vec!["xk".into()]);
    expect_none(&i, rule::COUNTRY);
}

// ---- 4 languages --------------------------------------------------------------------------------

#[test]
fn a_language_outside_the_official_list_is_refused() {
    let i = issues_of(|s| region(s, "jp").lang = "xx-YY".into());
    expect(&i, rule::LANG, "lang");
    let i = issues_of(|s| region(s, "jp").stt = Some("tlh".into()));
    expect(&i, rule::LANG, "stt");
    let i = issues_of(|s| region(s, "jp").other_langs = vec!["zh-TW".into()]);
    expect(&i, rule::LANG, "other_langs");
    let i = issues_of(|s| s.layers[0].data.stt = Some("nan-TW".into()));
    expect(&i, rule::LANG, "stt");
    for ok in ["th-TH", "nb", "ar-EG", "hi-IN", "cmn", "es-US", "pt-BR", "uk-UA", "kok"] {
        assert!(data::is_official_lang(ok), "{ok}");
    }
    for no in ["zh-TW", "cmn-TW", "cmn-CN", "es-ES", "pt-PT", "en-GB", "th", "ja", "yue", "nan", "hak", "ar-SA"] {
        assert!(!data::is_official_lang(no), "{no}");
    }
    assert_eq!(data::OFFICIAL_LANGS.len(), 79);
}

#[test]
fn a_bundle_is_checked_against_the_built_in_language_list() {
    let mut b = built();
    b.regions.get_mut("th").unwrap().lang = "xx".into();
    b.languages.get_mut("en").unwrap().stt = Some("xx".into());
    let i = check_bundle(&b);
    expect(&i, rule::LANG, "regions.th.lang");
    expect(&i, rule::LANG, "languages.en.stt");
}

// ---- 5 required fields ---------------------------------------------------------------------------

#[test]
fn a_field_missing_after_the_merge_is_refused() {
    let i = issues_of(|s| region(s, "th").delivery = None);
    expect(&i, rule::REQUIRED, "delivery");
    // The layer supplies habits, so a region without them still passes.
    let i = issues_of(|s| region(s, "th").habits = None);
    assert!(i.is_empty(), "{i:?}");
    let i = issues_of(|s| {
        region(s, "th").habits = None;
        s.layers[0].data.common.habits = None;
    });
    expect(&i, rule::REQUIRED, "habits");
}

#[test]
fn a_region_with_its_own_language_needs_its_own_hello() {
    let i = issues_of(|s| region(s, "th").hello = None);
    expect(&i, rule::REQUIRED, "hello");
    // Japanese is one of the nine, so Japan may leave it out.
    let i = issues_of(|s| region(s, "jp").hello = None);
    assert!(i.is_empty(), "{i:?}");
}

#[test]
fn all_ten_talk_style_levels_are_needed_and_only_those() {
    let i = issues_of(|s| {
        s.layers[0].data.common.talk_style.as_mut().unwrap().persona.remove("2");
    });
    expect(&i, rule::REQUIRED, "talk_style.persona.2");
    let i = issues_of(|s| {
        s.layers[0].data.common.talk_style.as_mut().unwrap().assistant.remove("5");
    });
    expect(&i, rule::REQUIRED, "talk_style.assistant.5");
    let i = issues_of(|s| {
        region(s, "jp").talk_style.as_mut().unwrap().persona.insert("6".into(), text("x", "y"));
    });
    expect(&i, rule::REQUIRED, "talk_style.persona");
    assert!(i.iter().any(|x| x.detail.contains("\"6\"")), "{i:?}");
}

#[test]
fn a_fraud_line_has_to_say_scam() {
    let i = issues_of(|s| region(s, "jp").fraud = Some(text("Be careful with that call", "小心這通電話")));
    expect(&i, rule::REQUIRED, "fraud.en");
    expect(&i, rule::REQUIRED, "fraud.zh");
    let i = issues_of(|s| region(s, "jp").fraud = Some(text("Such a call is a SCAM", "這種電話是詐騙")));
    assert!(i.is_empty(), "{i:?}");
}

// ---- 6 lengths ---------------------------------------------------------------------------------------

#[test]
fn text_over_its_limit_is_refused() {
    let i = issues_of(|s| region(s, "jp").delivery = Some(text(&"x".repeat(81), "短")));
    expect(&i, rule::LENGTH, "delivery.en");
    let i = issues_of(|s| region(s, "jp").delivery = Some(text("ok", &"短".repeat(37))));
    expect(&i, rule::LENGTH, "delivery.zh");
    let i = issues_of(|s| region(s, "jp").delivery = Some(text(&"x".repeat(80), &"短".repeat(36))));
    assert!(i.is_empty(), "{i:?}");
    // Characters are counted, not bytes: 36 Chinese characters are 108 bytes.
    let i = issues_of(|s| region(s, "jp").hello.as_mut().unwrap().hello = "あ".repeat(31));
    expect(&i, rule::LENGTH, "hello.hello");
}

#[test]
fn the_four_blocks_together_have_a_limit_too() {
    // Each within its own limit; together 80 + 80 + 110 + 306 = 576 over 560.
    let i = issues_of(|s| {
        let r = region(s, "jp");
        r.delivery = Some(text(&"d".repeat(80), "短"));
        r.readback = Some(text(&"r".repeat(80), "短"));
        r.fraud = Some(text(&format!("scam{}", "f".repeat(106)), "詐騙"));
        r.habits = Some(text(&"h".repeat(271), "短"));
    });
    expect(&i, rule::LENGTH, "total.en");
    assert!(!has(&i, rule::LENGTH, "delivery") && !has(&i, rule::LENGTH, "habits"), "{i:?}");
}

#[test]
fn the_layer_has_tighter_limits() {
    let i = issues_of(|s| s.layers[0].data.common.habits = Some(text(&"h".repeat(201), "短")));
    expect(&i, rule::LENGTH, "common.habits.en");
    let i = issues_of(|s| {
        s.layers[0].data.common.talk_style.as_mut().unwrap().persona.insert("1".into(), text(&"t".repeat(121), "短"));
    });
    expect(&i, rule::LENGTH, "common.talk_style.persona.1.en");
}

#[test]
fn vocab_has_limits_on_situations_words_and_names() {
    let words = |n: usize| (0..n).map(|i| format!("w{}", (b'a' + i as u8) as char)).collect::<Vec<_>>();
    let i = issues_of(|s| {
        region(s, "jp").vocab.insert("delivery".into(), words(5));
    });
    expect(&i, rule::LENGTH, "vocab.delivery");
    let i = issues_of(|s| {
        region(s, "jp").vocab.insert("Bad Name".into(), words(1));
    });
    expect(&i, rule::LENGTH, "vocab.Bad Name");
    let i = issues_of(|s| {
        for n in 0..9 {
            let name = format!("sit_{}", (b'a' + n as u8) as char);
            region(s, "jp").vocab.insert(name, words(1));
        }
    });
    expect(&i, rule::LENGTH, "vocab");
    let i = issues_of(|s| {
        region(s, "jp").vocab.insert("bank".into(), vec!["x".repeat(25)]);
    });
    expect(&i, rule::LENGTH, "vocab.bank[0]");
}

#[test]
fn a_bundle_has_limits_on_its_collections() {
    let mut b = built();
    let template = b.regions["th"].clone();
    for n in 0..401 {
        let mut r = template.clone();
        r.code = format!("zz-{}", "q".repeat(2 + n % 3));
        b.regions.insert(format!("k{n}"), r);
    }
    expect(&check_bundle(&b), rule::LENGTH, "regions");
}

// ---- 7 placeholders ----------------------------------------------------------------------------------

#[test]
fn a_placeholder_other_than_owner_is_refused() {
    for bad in ["Say hi to {name}", "Say hi to {OWNER}", "an open { brace", "a stray } brace", "{owner}", "{}", "full ｛name｝"] {
        let i = issues_of(|s| region(s, "jp").habits = Some(text(bad, "短")));
        expect(&i, rule::PLACEHOLDER, "habits.en");
    }
    // {owner} is only meant for the talk-style levels.
    let i = issues_of(|s| {
        region(s, "jp").talk_style.as_mut().unwrap().persona.insert("3".into(), text("Sound like {owner}", "像{owner}"));
    });
    expect_none(&i, rule::PLACEHOLDER);
    let i = issues_of(|s| {
        region(s, "jp").talk_style.as_mut().unwrap().persona.insert("3".into(), text("Sound like {name}", "像{owner}"));
    });
    expect(&i, rule::PLACEHOLDER, "talk_style.persona.3.en");
}

#[test]
fn hello_may_not_hold_the_owner_name() {
    let i = issues_of(|s| region(s, "th").hello.as_mut().unwrap().hello = "สวัสดี {owner}".into());
    expect(&i, rule::PLACEHOLDER, "hello.hello");
    let i = issues_of(|s| region(s, "jp").hello.as_mut().unwrap().hello_again = "{owner}？".into());
    expect(&i, rule::PLACEHOLDER, "hello.hello_again");
}

// ---- 8 web addresses, e-mail, long numbers ------------------------------------------------------------

#[test]
fn web_addresses_and_e_mail_are_refused() {
    for bad in [
        "see https://example.test/x",
        "see HTTP://EXAMPLE.TEST",
        "visit www.example.test",
        "go to example.com today",
        "write to a@b.test",
        "follow @someone",
        "ｈｔｔｐｓ：／／ｅｘａｍｐｌｅ．ｔｅｓｔ",
    ] {
        let i = issues_of(|s| region(s, "jp").habits = Some(text(bad, "短")));
        expect(&i, rule::PATTERN, "habits.en");
    }
    let i = issues_of(|s| region(s, "jp").habits = Some(text("It costs e.g. five yen. Fine.", "短")));
    expect_none(&i, rule::PATTERN);
}

#[test]
fn six_digits_in_a_row_are_refused_even_in_full_width_or_other_scripts() {
    for bad in ["call 123456", "call １２３４５６", "code 1234567890", "number ๑๒๓๔๕๖", "number ٠١٢٣٤٥"] {
        let i = issues_of(|s| region(s, "jp").habits = Some(text(bad, "短")));
        expect(&i, rule::PATTERN, "habits.en");
    }
    for ok in ["a 12 34 56 78 number", "4-3-3 with 12345", "ｘｘ ｘｘ"] {
        let i = issues_of(|s| region(s, "jp").habits = Some(text(ok, "短")));
        expect_none(&i, rule::PATTERN);
    }
    let i = issues_of(|s| region(s, "jp").habits = Some(text("ok", "電話一二三四五六 或 １２３４５６")));
    expect(&i, rule::PATTERN, "habits.zh");
}

// ---- 9 banned phrases ----------------------------------------------------------------------------------

#[test]
fn banned_phrases_are_refused_however_they_are_written() {
    for bad in [
        "Say: hold on",
        "Say: Let me check",
        "say HOLD ON please",
        "say hold   on",
        "ｈｏｌｄ　ｏｎ",
        "Ｌｅｔ ｍｅ ｃｈｅｃｋ",
        "How may I help you",
    ] {
        let i = issues_of(|s| region(s, "jp").habits = Some(text(bad, "短")));
        expect(&i, rule::BANNED, "habits.en");
    }
    for bad in ["先說查一下", "說請稍等", "說 請 稍 等", "说请稍等", "答應轉帳", "給驗證碼"] {
        let i = issues_of(|s| region(s, "jp").habits = Some(text("ok", bad)));
        expect(&i, rule::BANNED, "habits.zh");
    }
}

#[test]
fn the_fraud_line_is_checked_for_banned_phrases_too() {
    let i = issues_of(|s| region(s, "jp").fraud = Some(text("A scam: hold on a second", "詐騙，請稍等")));
    expect(&i, rule::BANNED, "fraud.en");
    expect(&i, rule::BANNED, "fraud.zh");
}

#[test]
fn banned_phrases_are_refused_in_talk_style_hello_and_vocab() {
    let i = issues_of(|s| {
        region(s, "jp").talk_style.as_mut().unwrap().persona.insert("3".into(), text("Say hold on", "短"));
    });
    expect(&i, rule::BANNED, "talk_style.persona.3.en");
    let i = issues_of(|s| region(s, "th").hello.as_mut().unwrap().hello = "hold on".into());
    expect(&i, rule::BANNED, "hello.hello");
    let i = issues_of(|s| {
        region(s, "jp").vocab.insert("bank".into(), vec!["請稍等".into()]);
    });
    expect(&i, rule::BANNED, "vocab.bank[0]");
}

// ---- 10 languages and accents Gemini does not speak -------------------------------------------------------

#[test]
fn naming_an_unsupported_language_or_accent_is_refused() {
    for bad in [
        "answer in Cantonese",
        "ANSWER IN HOKKIEN",
        "Speak only English",
        "use Egyptian Arabic",
        "a Gulf Arabic accent",
        "Levantine speech",
        "[Local language]",
    ] {
        let i = issues_of(|s| region(s, "jp").habits = Some(text(bad, "短")));
        assert!(has(&i, rule::UNSUPPORTED, "habits.en") || has(&i, rule::CHARSET, "habits.en"), "{bad}: {i:?}");
    }
    for bad in ["用台語回", "只講英文", "埃及腔的阿拉伯語"] {
        let i = issues_of(|s| region(s, "jp").habits = Some(text("ok", bad)));
        expect(&i, rule::UNSUPPORTED, "habits.zh");
    }
    assert_eq!(data::UNSUPPORTED_OR_LIMITING.len(), 21);
}

// ---- 11 Chinese dialects -----------------------------------------------------------------------------------

#[test]
fn chinese_dialects_are_only_for_the_chinese_speaking_countries() {
    let i = issues_of(|s| region(s, "jp").chinese_dialects = true);
    expect(&i, rule::DIALECTS, "chinese_dialects");
    let i = issues_of(|s| {
        let r = region(s, "jp");
        r.chinese_dialects = true;
        r.countries = vec!["tw".into(), "hk".into()];
    });
    expect_none(&i, rule::DIALECTS);
    let i = issues_of(|s| {
        let r = region(s, "jp");
        r.chinese_dialects = true;
        r.countries = vec![];
    });
    expect(&i, rule::DIALECTS, "chinese_dialects");
}

// ---- 12 status and dates ----------------------------------------------------------------------------------------

#[test]
fn status_and_review_date_are_checked() {
    let i = issues_of(|s| region(s, "jp").status = "reviewed".into());
    expect(&i, rule::STATUS, "status");
    let i = issues_of(|s| region(s, "jp").status = "local_reviewed".into());
    expect(&i, rule::STATUS, "reviewed_on");
    let i = issues_of(|s| {
        let r = region(s, "jp");
        r.status = "local_reviewed".into();
        r.reviewed_on = Some("2026-02-30".into());
        s.layers[0].data.status = "local_reviewed".into();
        s.layers[0].data.reviewed_on = Some("2026-01-05".into());
    });
    expect(&i, rule::STATUS, "reviewed_on");
    let i = issues_of(|s| {
        let r = region(s, "jp");
        r.status = "local_reviewed".into();
        r.reviewed_on = Some("2099-01-01".into());
    });
    expect(&i, rule::STATUS, "reviewed_on");
    let i = issues_of(|s| {
        let r = region(s, "jp");
        r.status = "local_reviewed".into();
        r.reviewed_on = Some("2026-01-05".into());
        s.layers[0].data.status = "local_reviewed".into();
        s.layers[0].data.reviewed_on = Some("2026-01-05".into());
    });
    assert!(i.is_empty(), "{i:?}");
}

#[test]
fn the_merged_status_is_the_lower_of_region_and_layer() {
    let mut s = valid();
    region(&mut s, "jp").status = "local_reviewed".into();
    region(&mut s, "jp").reviewed_on = Some("2026-01-05".into());
    let b = build_at(&s, VERSION, COMMIT, BUILT).unwrap();
    // The layer is still a draft.
    assert_eq!(b.regions["jp"].status, "draft");
    assert_eq!(b.manifest.iter().find(|e| e.id == "jp").unwrap().status, "draft");
}

#[test]
fn a_version_after_tomorrow_or_not_a_date_is_refused() {
    let s = valid();
    let err = build_at(&s, 2099010101, COMMIT, BUILT).expect_err("far future");
    expect(&err, rule::STATUS, "version");
    for bad in [123, 2026133301, 2026023001, 20260101, 20260101011] {
        let err = build_at(&s, bad, COMMIT, BUILT).expect_err("bad version");
        expect(&err, rule::STATUS, "version");
    }
    // Today's first build is fine, and tomorrow's too.
    let today = util::today_version();
    assert!(build_at(&s, today, COMMIT, BUILT).is_ok());
    let tomorrow = util::civil_from_days(util::now_days() + 1);
    let v = tomorrow.0 as u64 * 1_000_000 + tomorrow.1 as u64 * 10_000 + tomorrow.2 as u64 * 100 + 1;
    assert!(build_at(&s, v, COMMIT, BUILT).is_ok());
    let err = build_at(&s, 2026010101, "not a commit!", BUILT).expect_err("bad commit");
    expect(&err, rule::FORMAT, "source_commit");
    let err = build_at(&s, 2026010101, COMMIT, "yesterday").expect_err("bad time");
    expect(&err, rule::STATUS, "built_at");
}

// ---- 13 countries index and manifest -----------------------------------------------------------------------------------

#[test]
fn the_countries_index_has_to_match_the_regions() {
    let mut b = built();
    b.countries.insert("jp".into(), "th".into());
    expect(&check_bundle(&b), rule::INDEX, "countries.jp");

    let mut b = built();
    b.countries.remove("th");
    expect(&check_bundle(&b), rule::INDEX, "countries.th");

    let mut b = built();
    b.countries.insert("kr".into(), "jp".into());
    expect(&check_bundle(&b), rule::INDEX, "countries.kr");

    let mut b = built();
    b.countries.insert("fr".into(), "nowhere".into());
    expect(&check_bundle(&b), rule::INDEX, "countries.fr");

    let mut b = built();
    b.regions.get_mut("th").unwrap().countries.push("jp".into());
    expect(&check_bundle(&b), rule::COUNTRY, "countries.jp");
}

#[test]
fn the_manifest_has_to_match_the_regions() {
    let mut b = built();
    b.manifest.retain(|e| e.id != "th");
    expect(&check_bundle(&b), rule::INDEX, "manifest");

    let mut b = built();
    b.manifest.push(Entry { kind: "region".into(), id: "kr".into(), status: "draft".into(), sha256: "0".repeat(64) });
    expect(&check_bundle(&b), rule::INDEX, "manifest");

    let mut b = built();
    b.manifest.iter_mut().find(|e| e.id == "jp").unwrap().status = "local_reviewed".into();
    expect(&check_bundle(&b), rule::INDEX, "manifest");

    let mut b = built();
    b.manifest[0].sha256 = "XYZ".into();
    expect(&check_bundle(&b), rule::INDEX, "manifest[0]");

    let mut b = built();
    let dup = b.manifest[0].clone();
    b.manifest.push(dup);
    expect(&check_bundle(&b), rule::INDEX, "manifest[3]");

    let mut b = built();
    b.manifest.retain(|e| e.kind != "layer");
    expect(&check_bundle(&b), rule::INDEX, "manifest");
}

// ---- line breaks, control characters, brackets ------------------------------------------------------------------------------

#[test]
fn line_breaks_control_and_zero_width_characters_are_refused() {
    for bad in [
        "a\nb",
        "a\r\nb",
        "a\tb",
        "a\u{0007}b",
        "a\u{200B}b",
        "a\u{200D}b",
        "a\u{FEFF}b",
        "a\u{2060}b",
        "a\u{202E}b",
        "a\u{2067}b",
        "a\u{2028}b",
        "a\u{E0041}b",
    ] {
        let i = issues_of(|s| region(s, "jp").habits = Some(text(bad, "短")));
        expect(&i, rule::CHARSET, "habits.en");
        let i = issues_of(|s| region(s, "jp").habits = Some(text("ok", bad)));
        expect(&i, rule::CHARSET, "habits.zh");
    }
}

#[test]
fn brackets_and_markup_are_refused_but_quotes_are_not() {
    for bad in ["a [x]", "a ] b", "【x】", "<b>", "x > y", "fish & chips", "［x］", "＜b＞", "＆"] {
        let i = issues_of(|s| region(s, "jp").habits = Some(text(bad, "短")));
        expect(&i, rule::CHARSET, "habits.en");
    }
    let i = issues_of(|s| region(s, "jp").habits = Some(text("say \"hi\" and 'bye', “ok”, 「はい」", "「好」")));
    expect_none(&i, rule::CHARSET);
}

// ---- the release index -----------------------------------------------------------------------------------------------------------

fn good_index() -> Index {
    Index { format: FORMAT, version: VERSION, file: "bageo-pack.json".into(), bytes: 1000, sha256: sha256_hex(b"x"), min_bageo: "1.6.9".into() }
}

#[test]
fn a_good_index_passes_and_unknown_fields_are_ignored() {
    assert!(check_index(&good_index()).is_empty());
    let mut v = serde_json::to_value(good_index()).unwrap();
    v["later"] = serde_json::json!(1);
    assert!(parse_index(&v.to_string()).is_ok());
}

#[test]
fn the_index_file_must_be_a_plain_file_name() {
    for bad in ["../x.json", "sub/x.json", "sub\\x.json", "C:\\x.json", "/etc/passwd", ".hidden.json", "", "a b.json", "x.json?y=1", "https://example.test/x.json"] {
        let mut ix = good_index();
        ix.file = bad.into();
        expect(&check_index(&ix), rule::RELEASE_INDEX, "file");
    }
}

#[test]
fn the_index_checks_hash_size_version_and_minimum() {
    let mut ix = good_index();
    ix.sha256 = "ABC".into();
    expect(&check_index(&ix), rule::RELEASE_INDEX, "sha256");
    let mut ix = good_index();
    ix.bytes = MAX_BUNDLE_BYTES as u64 + 1;
    expect(&check_index(&ix), rule::LENGTH, "bytes");
    let mut ix = good_index();
    ix.version = 2099010101;
    expect(&check_index(&ix), rule::STATUS, "version");
    let mut ix = good_index();
    ix.min_bageo = "1.6".into();
    expect(&check_index(&ix), rule::RELEASE_INDEX, "min_bageo");
    let mut ix = good_index();
    ix.format = 2;
    expect(&check_index(&ix), rule::FORMAT, "format");
}

// ---- the tables --------------------------------------------------------------------------------------------------------------------

#[test]
fn the_country_table_knows_the_real_codes() {
    for ok in ["tw", "jp", "us", "ye", "ly", "sd", "xk", "re", "yt"] {
        assert!(data::is_country(ok), "{ok}");
    }
    for no in ["uk", "zz", "TW", "t", "twn", ""] {
        assert!(!data::is_country(no), "{no}");
    }
}
