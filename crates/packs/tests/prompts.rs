// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The prompt texts: the built-in files, the rules a changed text must pass,
//! the download and build-prompts. Example sentences are invented.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use bageo_packs::prompts::{placeholders, BUILTIN_FILES};
use bageo_packs::*;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde_json::{json, Value};

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const BUILT: &str = "2026-11-22T00:00:00Z";
const VERSION: u64 = 2026112202;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bageo-prompts-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn gz(bytes: &[u8]) -> Vec<u8> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(bytes).unwrap();
    enc.finish().unwrap()
}

fn rule_of(r: Result<(), TextProblem>) -> &'static str {
    r.err().map(|p| p.rule).unwrap_or("ok")
}

// ---- the built-in files ------------------------------------------------------------------------

#[test]
fn the_builtin_files_read_and_their_locked_keys_exist() {
    let b = builtin_book();
    assert!(b.errors.is_empty(), "{:?}", b.errors);
    assert_eq!(b.langs.len(), BUILTIN_FILES.len());
    let hold = ["short_text_hold"];
    for (lang, keys) in [
        (
            "zh-TW",
            &[
                "live_persona_guard",
                "guard_assistant",
                "injection_guard",
                "live_persona_hold_off_default",
                "live_persona_hold_off_line",
                "live_persona_hold_off_stall",
            ][..],
        ),
        ("en", &["guard_persona", "guard_assistant", "injection_guard", "hold_off_line", "hold_off_stall", "short_text_hold", "sys"][..]),
        ("ja", &hold[..]),
        ("ko", &hold[..]),
        ("es", &hold[..]),
        ("fr", &hold[..]),
        ("de", &hold[..]),
        ("pt", &hold[..]),
    ] {
        let locked: Vec<_> = b.langs[lang].locked.iter().map(String::as_str).collect();
        let mut want = keys.to_vec();
        want.sort();
        assert_eq!(locked, want, "{lang}");
        for k in keys {
            assert!(b.text(lang, k).is_some() && b.locked(lang, k), "{lang}/{k}");
        }
    }
    for (lang, f) in &b.langs {
        for (key, text) in &f.texts {
            assert_eq!(check_text(text, text), Ok(()), "{lang}/{key} passes against itself");
        }
    }
}

#[test]
fn a_file_with_a_locked_key_it_does_not_have_or_an_unknown_field_is_refused() {
    for (file, why) in [
        (r#"{"format": 1, "lang": "xx", "locked": ["b"], "texts": {"a": "x"}}"#, "locked"),
        (r#"{"format": 1, "lang": "xx", "locked": "a", "texts": {"a": "x"}}"#, "locked"),
        (r#"{"format": 1, "lang": "xx", "locked": [1], "texts": {"a": "x"}}"#, "locked"),
        (r#"{"format": 1, "lang": "xx", "note": "x", "texts": {"a": "x"}}"#, "note"),
    ] {
        let err = parse_book_file("xx", file).unwrap_err();
        assert!(err.contains(why), "{file}: {err}");
    }
    let ok = parse_book_file("xx", r#"{"format": 1, "lang": "xx", "locked": ["a"], "texts": {"a": "x", "b": ["y", "z"]}}"#).unwrap();
    assert!(ok.locked.contains("a") && ok.texts["b"] == "y\nz");
}

// ---- one changed text --------------------------------------------------------------------------

#[test]
fn placeholders_must_stay_the_same() {
    assert_eq!(placeholders("{owner} 先生，{time}（{owner}）{ not } {x-y} {}").into_iter().collect::<Vec<_>>(), ["owner", "time"]);
    for (old, good, lost, extra) in [
        ("照念這一句：「{line}」", "請照著念：「{line}」", "請照著念這句。", "請照著念：「{line}」{owner}"),
        ("Say exactly this line: \"{line}\"", "Read this line word for word: \"{line}\"", "Read the line.", "Read \"{line}\" to {name}"),
        ("この一文をそのまま言って：「{line}」", "「{line}」とそのまま言って。", "そのまま言って。", "「{line}」と{who}に言って。"),
        ("이 문장을 그대로 말하세요: \"{line}\"", "\"{line}\" 그대로 읽어 주세요.", "그대로 읽어 주세요.", "\"{line}\" {line2}"),
        ("Di exactamente esta frase: «{line}»", "Repite tal cual: «{line}»", "Repite la frase.", "«{line}» y {otra}"),
        ("Dis exactement cette phrase : « {line} »", "Répète mot pour mot : « {line} »", "Répète la phrase.", "« {line} » {autre}"),
        ("Sag genau diesen Satz: „{line}“", "Wiederhole wörtlich: „{line}“", "Wiederhole den Satz.", "„{line}“ und {mehr}"),
        ("Diga exatamente esta frase: \"{line}\"", "Repita palavra por palavra: \"{line}\"", "Repita a frase.", "\"{line}\" {outra}"),
        ("พูดประโยคนี้ตามนี้: \"{line}\"", "อ่านตามนี้ทุกคำ: \"{line}\"", "อ่านตามนี้", "\"{line}\" {other}"),
        ("قل هذه الجملة كما هي: «{line}»", "كرر حرفيًا: «{line}»", "كرر الجملة.", "«{line}» {owner}"),
        ("Nói đúng câu này: \"{line}\"", "Đọc nguyên văn: \"{line}\"", "Đọc câu đó.", "\"{line}\" cho {owner}"),
    ] {
        assert_eq!(rule_of(check_text(old, good)), "ok", "{good}");
        assert_eq!(rule_of(check_text(old, lost)), rule::PLACEHOLDER, "{lost}");
        assert_eq!(rule_of(check_text(old, extra)), rule::PLACEHOLDER, "{extra}");
    }
}

#[test]
fn spaces_and_line_breaks_at_both_ends_must_stay_the_same() {
    for (old, good, bad) in [
        ("\n[通話] 簡短地回答。\n", "\n[通話] 回答要短。\n", "[通話] 回答要短。\n"),
        ("[System instruction] ", "[System note] ", "[System note]"),
        ("、海外の番号から", "、海外からの電話", " 、海外からの電話"),
        ("\n\n[주의] 짧게 답하세요.", "\n\n[주의] 짧게 말하세요.", "\n[주의] 짧게 말하세요."),
        (", desde un número del extranjero", ", desde el extranjero", ", desde el extranjero "),
        ("Réponds brièvement.\n", "Réponds en peu de mots.\n", "Réponds en peu de mots."),
        (" Antworte kurz.", " Antworte knapp.", "Antworte knapp."),
        ("Responda curto.\n\n", "Responda em poucas palavras.\n\n", "Responda em poucas palavras.\n"),
        ("ตอบสั้น ๆ\n", "ตอบให้สั้น\n", "ตอบให้สั้น"),
        ("\nأجب باختصار.", "\nأجب بإيجاز.", "أجب بإيجاز."),
        ("Trả lời ngắn gọn. ", "Trả lời thật ngắn. ", "Trả lời thật ngắn."),
    ] {
        assert_eq!(rule_of(check_text(old, good)), "ok", "{good:?}");
        assert_eq!(rule_of(check_text(old, bad)), rule::FORMAT, "{bad:?}");
    }
}

#[test]
fn a_text_may_grow_to_twice_its_size_or_400_characters_more() {
    let short = "Kurz antworten."; // 15 characters: up to 415
    assert_eq!(rule_of(check_text(short, &"ä".repeat(415))), "ok");
    assert_eq!(rule_of(check_text(short, &"ä".repeat(416))), rule::LENGTH);
    let long = "長".repeat(500); // up to 1000
    assert_eq!(rule_of(check_text(&long, &"說".repeat(1000))), "ok");
    assert_eq!(rule_of(check_text(&long, &"說".repeat(1001))), rule::LENGTH);
    assert_eq!(rule_of(check_text("짧게.", &"말".repeat(403))), "ok");
    assert_eq!(rule_of(check_text("짧게.", &("말".repeat(403) + "다"))), rule::LENGTH);
    for empty in ["", "   ", "\n"] {
        assert_eq!(rule_of(check_text("Répondez.", empty)), rule::REQUIRED, "{empty:?}");
    }
}

#[test]
fn control_replacement_and_bidirectional_characters_are_refused() {
    assert_eq!(rule_of(check_text("Habla claro.", "Habla claro\ny despacio.")), "ok");
    for bad in [
        "Habla\tclaro.",
        "Fale\rdevagar.",
        "話し\u{0}方",
        "말\u{7f}하기",
        "Sprich \u{FFFD} klar.",
        "تحدث \u{202E}ببطء.",
        "Nói \u{2067}chậm\u{2069} thôi.",
        "พูด\u{202A}ช้า ๆ",
        "說\u{2066}慢一點",
    ] {
        assert_eq!(rule_of(check_text("Habla claro y despacio, por favor.", bad)), rule::CHARSET, "{bad:?}");
    }
}

/// Zero-width marks, the soft hyphen, line separators and tag characters are
/// not seen on a page but a model reads them: refused, unless the built-in
/// text has the same one (a script that joins letters with it).
#[test]
fn characters_nobody_sees_are_refused_unless_the_built_in_text_has_them() {
    for (builtin, bad) in [
        ("先聽對方說完。", "先聽對方\u{200B}說完。"),
        ("Listen first.", "Listen\u{200D} first."),
        ("最後まで聞きます。", "最後まで\u{FEFF}聞きます。"),
        ("끝까지 듣습니다.", "끝까지\u{2060} 듣습니다."),
        ("Escucha primero.", "Escucha\u{00AD} primero."),
        ("Écoutez d'abord.", "Écoutez\u{2028}d'abord."),
        ("Erst zuhören.", "Erst\u{200E} zuhören."),
        ("Ouça primeiro.", "Ouça\u{E0041} primeiro."),
        ("ฟังให้จบก่อน", "ฟัง\u{200C}ให้จบก่อน"),
        ("استمع أولاً.", "استمع\u{200F} أولاً."),
        ("Hãy nghe trước.", "Hãy\u{2029}nghe trước."),
    ] {
        assert_eq!(rule_of(check_text(builtin, bad)), rule::CHARSET, "{bad:?}");
    }
    // Persian joins letters with U+200C: a built-in text that has it may keep it.
    assert_eq!(rule_of(check_text("می\u{200C}شنوم.", "خوب می\u{200C}شنوم.")), "ok");
    assert_eq!(rule_of(check_text("می\u{200C}شنوم.", "می\u{200B}شنوم.")), rule::CHARSET);
    // The narrow no-break space of French punctuation is ordinary text.
    assert_eq!(rule_of(check_text("Qui est-ce ?", "Qui est à l'appareil\u{202F}?")), "ok");
}

/// The hard lines (what is never agreed to, who the AI says it is, whose words
/// count as instructions) only change with a new Bageo, in every language.
#[test]
fn the_hard_lines_of_every_language_are_locked() {
    let mut seen = 0;
    for (lang, file) in &builtin_book().langs {
        for (key, _) in &file.texts {
            if key.contains("guard") || key.contains("hold_off") || key == "short_text_hold" || key == "sys" {
                seen += 1;
                assert!(file.locked.contains(key), "{lang}/{key} is a hard line and must be locked");
            }
        }
    }
    assert_eq!(seen, 19);
}

// ---- the download ------------------------------------------------------------------------------

fn sample() -> PromptsPack {
    let mut langs = std::collections::BTreeMap::new();
    langs.insert("ja".to_string(), [("short_text_language".to_string(), PromptText::One("言葉はふだんの日本語で。".into()))].into());
    langs.insert("pt".to_string(), [("short_text_language".to_string(), PromptText::Lines(vec!["Fale simples,".into(), "como ao telefone.".into()]))].into());
    PromptsPack { format: PROMPTS_FORMAT, version: VERSION, built_at: BUILT.into(), source_commit: COMMIT.into(), langs }
}

fn pack_value(v: &Value) -> Vec<u8> {
    gz(&serde_json::to_vec(v).unwrap())
}

#[test]
fn a_download_reads_back_and_its_limits_and_fields_are_kept() {
    let p = sample();
    let back = parse_prompts(&pack_prompts(&p).unwrap()).unwrap();
    assert_eq!(back, p);
    assert_eq!(back.langs["pt"]["short_text_language"].joined(), "Fale simples,\ncomo ao telefone.");
    assert_eq!(back.count(), 2);

    let good = serde_json::to_value(&p).unwrap();
    let field_of = |v: Value| parse_prompts(&pack_value(&v)).unwrap_err()[0].clone();
    let mut v = good.clone();
    v["extra"] = json!(1);
    assert_eq!(field_of(v).rule, rule::FORMAT);
    for bad in [2026133101u64, 2026022901, 123, 99_999_999_999] {
        let mut v = good.clone();
        v["version"] = json!(bad);
        assert_eq!(field_of(v).field, "version", "{bad}");
    }
    let mut v = good.clone();
    v["format"] = json!(2);
    assert!(field_of(v).detail.contains("update Bageo"));
    let mut v = good.clone();
    v["built_at"] = json!("yesterday");
    assert_eq!(field_of(v).field, "built_at");
    let mut v = good.clone();
    v["langs"]["ja"]["short_text_language"] = json!(3);
    assert_eq!(field_of(v).rule, rule::FORMAT);

    let packed = vec![0u8; MAX_PROMPTS_PACKED + 1];
    assert_eq!(parse_prompts(&packed).unwrap_err()[0].rule, rule::LENGTH);
    // Small packed, too large unpacked: stopped while unpacking.
    let bomb = gz(&vec![b' '; MAX_PROMPTS_BYTES + 1]);
    assert!(bomb.len() < MAX_PROMPTS_PACKED);
    let err = parse_prompts(&bomb).unwrap_err();
    assert!(err[0].rule == rule::LENGTH && err[0].detail.contains("stopped"), "{err:?}");
    let mut tail = pack_prompts(&p).unwrap();
    tail.extend_from_slice(b"\0more");
    assert_eq!(parse_prompts(&tail).unwrap_err()[0].rule, rule::FORMAT);
}

#[test]
fn the_index_entry_is_checked_and_a_broken_one_reads_as_absent() {
    let hex = sha256_hex(b"x");
    let r = LocalesRef { file: PROMPTS_FILE.into(), bytes: 900, sha256: hex.clone(), format: 1, version: VERSION, min_bageo: "1.6.9".into() };
    assert!(check_prompts_ref(&r).is_empty());
    let mut big = r.clone();
    big.bytes = MAX_PROMPTS_PACKED as u64 + 1;
    assert!(check_prompts_ref(&big).iter().any(|i| i.field == "prompts.bytes"));
    let mut path = r.clone();
    path.file = "../bageo-prompts.json.gz".into();
    assert!(check_prompts_ref(&path).iter().any(|i| i.field == "prompts.file"));

    let base = json!({"format": 1, "version": VERSION, "file": "bageo-pack.json", "bytes": 10, "sha256": hex, "min_bageo": "1.6.9"});
    let mut v = base.clone();
    v["prompts"] = serde_json::to_value(&r).unwrap();
    assert_eq!(parse_index(&v.to_string()).unwrap().prompts, Some(r.clone()));
    v["prompts"]["sha256"] = json!("ABC");
    let (ix, note) = parse_index_noting(&v.to_string()).unwrap();
    assert!(ix.prompts.is_none() && note.is_some_and(|n| n.field == "prompts"));
    v["prompts"] = json!("not an entry");
    assert!(parse_index(&v.to_string()).unwrap().prompts.is_none());
    assert!(parse_index(&base.to_string()).unwrap().prompts.is_none());
}

// ---- build-prompts -----------------------------------------------------------------------------

fn builtin(lang: &str, key: &str) -> String {
    builtin_book().text(lang, key).unwrap_or_else(|| panic!("{lang}/{key}")).to_string()
}

fn write_lang(dir: &Path, lang: &str, texts: Value) {
    let p = dir.join("prompts");
    fs::create_dir_all(&p).unwrap();
    fs::write(p.join(format!("{lang}.json")), json!({"format": 1, "lang": lang, "texts": texts}).to_string()).unwrap();
}

const INDEX: &str = r#"{"format":1,"version":2026112201,"file":"bageo-pack.json","bytes":10,"sha256":"0000000000000000000000000000000000000000000000000000000000000000","min_bageo":"1.6.9","later":1}"#;

fn build(dir: &Path) -> (bool, PathBuf, PathBuf) {
    let (out, index) = (dir.join(PROMPTS_FILE), dir.join("bageo-pack-index.json"));
    if !index.exists() {
        fs::write(&index, INDEX).unwrap();
    }
    let ok = Command::new(env!("CARGO_BIN_EXE_packs"))
        .arg("build-prompts")
        .arg(dir)
        .args(["--version", &VERSION.to_string(), "--commit", COMMIT, "--built-at", BUILT, "--out"])
        .arg(&out)
        .arg("--index")
        .arg(&index)
        .output()
        .unwrap()
        .status
        .success();
    (ok, out, index)
}

#[test]
fn build_prompts_carries_only_the_changed_texts_and_keeps_the_index() {
    let d = tmp("build");
    let ja_new = "言葉はふだんの話し言葉の日本語で。";
    let fr_new = "Parle un français de tous les jours, celui du téléphone.";
    write_lang(&d, "ja", json!({"short_text_language": ja_new, "short_text_sales": builtin("ja", "short_text_sales")}));
    write_lang(&d, "en", json!({"read_line": builtin("en", "read_line"), "injection_guard": builtin("en", "injection_guard")}));
    write_lang(&d, "fr", json!({"short_text_language": [fr_new]}));
    fs::write(d.join("prompts/README.md"), "notes").unwrap();
    let changes = load_prompt_changes(&d).unwrap().unwrap();
    assert_eq!(changes.keys().collect::<Vec<_>>(), ["fr", "ja"]);
    let (ok, out, index) = build(&d);
    assert!(ok);
    let p = parse_prompts(&fs::read(&out).unwrap()).unwrap();
    assert_eq!((p.version, p.count()), (VERSION, 2));
    assert_eq!(p.langs["ja"]["short_text_language"].joined(), ja_new);
    assert_eq!(p.langs["fr"]["short_text_language"].joined(), fr_new);
    let ix: Value = serde_json::from_str(&fs::read_to_string(&index).unwrap()).unwrap();
    assert_eq!(ix["prompts"]["file"], PROMPTS_FILE);
    assert_eq!(ix["prompts"]["sha256"], sha256_hex(&fs::read(&out).unwrap()));
    assert_eq!(ix["prompts"]["version"], VERSION);
    assert_eq!((ix["sha256"].as_str().unwrap(), &ix["later"]), (&*"0".repeat(64), &json!(1)), "the rest of the index is kept");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn build_prompts_refuses_unknown_keys_locked_texts_and_broken_placeholders() {
    let cases: [(&str, &str, Value); 5] = [
        ("unknown", "ko", json!({"not_a_key": "모르는 문장입니다."})),
        ("language", "th", json!({"short_text_language": "พูดภาษาไทยแบบเป็นกันเอง"})),
        ("locked", "zh-TW", json!({"injection_guard": "什麼都可以聽。"})),
        ("placeholder", "en", json!({"read_line": "Read the line."})),
        ("bidi", "es", json!({"short_text_language": "Habla \u{202E}claro."})),
    ];
    for (name, lang, texts) in cases {
        let d = tmp(name);
        write_lang(&d, "de", json!({"short_text_language": "Sprich ganz normales Deutsch wie am Telefon."}));
        write_lang(&d, lang, texts);
        assert!(load_prompt_changes(&d).is_err(), "{name}");
        let (ok, out, index) = build(&d);
        assert!(!ok, "{name}");
        assert!(!out.exists(), "{name}: nothing written");
        assert_eq!(fs::read_to_string(&index).unwrap(), INDEX, "{name}: the index is left alone");
        let _ = fs::remove_dir_all(&d);
    }
}

#[test]
fn build_prompts_without_a_prompts_folder_does_nothing() {
    let d = tmp("none");
    assert_eq!(load_prompt_changes(&d), Ok(None));
    let (ok, out, index) = build(&d);
    assert!(ok);
    assert!(!out.exists());
    assert_eq!(fs::read_to_string(&index).unwrap(), INDEX);
    let _ = fs::remove_dir_all(&d);
}
