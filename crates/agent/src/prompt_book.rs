// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The prompt book: the words the models are told, kept as data instead of
//! in the code that uses them.
//!
//! - The words are in `data/prompts/<language>.json` (one file per language
//!   the instructions are written in), compiled into the program. A text is a
//!   string, or a list of lines for a text of several paragraphs: one
//!   paragraph a line reads and diffs well.
//! - The code holds a [Text] for each one it uses (see [prompt_texts]): a
//!   name, looked up in the book the first time it is used and remembered, so
//!   every use after that costs one pointer read.
//! - Changing the words, or adding a language file, changes no code. A new
//!   text is one entry in the file and one line where its handle is declared.
//! - The checks below keep the two in step: every handle has its text, and
//!   no text is left in a file without a handle.
//!
//! How texts are put together into a call's prompt stays where it was
//! ([crate::prompts]): the book holds the words, not the rules.

use std::collections::HashMap;
use std::fmt;
use std::sync::OnceLock;

use serde_json::Value;

/// The format of the files this program reads.
pub const FORMAT: u64 = 1;

/// The files of the book, by the language their instructions are written in.
const FILES: &[(&str, &str)] =
    &[("zh-TW", include_str!("../data/prompts/zh-TW.json")), ("en", include_str!("../data/prompts/en.json"))];

/// One text of the book, by name (`<language>/<key>`). Reads as a `&str`
/// ([Text::get]) and writes itself in `format!`.
pub struct Text {
    name: &'static str,
    found: OnceLock<&'static str>,
}

impl Text {
    pub const fn new(name: &'static str) -> Self {
        Self { name, found: OnceLock::new() }
    }

    /// The text. Looked up once, then remembered. A name the book does not
    /// have reads as nothing and is logged (the checks keep that from shipping).
    pub fn get(&self) -> &'static str {
        self.found.get_or_init(|| match book().get(self.name) {
            Some(t) => t,
            None => {
                tracing::error!("prompt book: no text named {}", self.name);
                ""
            }
        })
    }

    /// Its name in the book.
    pub fn name(&self) -> &'static str {
        self.name
    }
}

impl fmt::Display for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.get())
    }
}

/// Declares the handles of a module's texts, and (for the checks) the list of
/// them all:
///
/// ```ignore
/// prompt_texts! {
///     /// What it is for.
///     ZH_KICK_COLD = "zh-TW/kick_cold";
/// }
/// ```
macro_rules! prompt_texts {
    ($( $(#[$m:meta])* $vis:vis $name:ident = $key:literal; )*) => {
        $( $(#[$m])* $vis static $name: $crate::prompt_book::Text = $crate::prompt_book::Text::new($key); )*
        /// Every handle declared above.
        #[cfg(test)]
        pub(crate) static ALL_TEXTS: &[&$crate::prompt_book::Text] = &[$(&$name),*];
    };
}
pub(crate) use prompt_texts;

/// The texts of every file, by name.
struct Book {
    texts: HashMap<&'static str, &'static str>,
}

impl Book {
    fn get(&self, name: &str) -> Option<&'static str> {
        self.texts.get(name).copied()
    }
}

fn book() -> &'static Book {
    static BOOK: OnceLock<Book> = OnceLock::new();
    BOOK.get_or_init(|| {
        let mut texts = HashMap::new();
        for (lang, json) in FILES {
            match parse_file(lang, json) {
                Ok(list) => {
                    for (key, text) in list {
                        // Kept for the life of the program: the files are part of it.
                        let name: &'static str = Box::leak(format!("{lang}/{key}").into_boxed_str());
                        texts.insert(name, &*Box::leak(text.into_boxed_str()));
                    }
                }
                Err(e) => tracing::error!("prompt book: {lang}.json: {e}"),
            }
        }
        Book { texts }
    })
}

/// The texts of one file: `{"format": 1, "lang": "<language>", "texts": {"<key>": <text>}}`,
/// a text being a string or a list of lines (joined with a line break).
fn parse_file(lang: &str, json: &str) -> Result<Vec<(String, String)>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("not JSON: {e}"))?;
    match v.get("format").and_then(Value::as_u64) {
        Some(FORMAT) => {}
        other => return Err(format!("format {other:?}, this program reads {FORMAT}")),
    }
    if v.get("lang").and_then(Value::as_str) != Some(lang) {
        return Err(format!("\"lang\" is not {lang}"));
    }
    let texts = v.get("texts").and_then(Value::as_object).ok_or("no \"texts\"")?;
    let mut out = Vec::with_capacity(texts.len());
    for (key, t) in texts {
        let ok = !key.is_empty() && key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !ok {
            return Err(format!("key {key:?}: lower-case letters, digits and _ only"));
        }
        let text = match t {
            Value::String(s) => s.clone(),
            Value::Array(lines) => {
                let mut parts = Vec::with_capacity(lines.len());
                for l in lines {
                    parts.push(l.as_str().ok_or_else(|| format!("{key}: a line that is not a string"))?);
                }
                parts.join("\n")
            }
            _ => return Err(format!("{key}: neither a string nor a list of lines")),
        };
        out.push((key.clone(), text));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_file_of_the_book_reads() {
        for (lang, json) in FILES {
            let list = parse_file(lang, json).unwrap_or_else(|e| panic!("{lang}.json: {e}"));
            assert!(!list.is_empty(), "{lang}");
            for (key, text) in &list {
                assert!(!text.is_empty(), "{lang}/{key} is empty");
                assert_eq!(book().get(&format!("{lang}/{key}")), Some(text.as_str()), "{lang}/{key}");
            }
        }
    }

    /// Every handle the code declares has its text, under one name each, and
    /// no text sits in a file without a handle: a slip in a key shows here,
    /// not on a call.
    #[test]
    fn handles_and_texts_match_one_to_one() {
        let handles = crate::prompts::ALL_TEXTS;
        let mut seen = std::collections::BTreeSet::new();
        for t in handles {
            assert!(book().get(t.name()).is_some(), "no text named {}", t.name());
            assert!(!t.get().is_empty(), "{}", t.name());
            assert!(seen.insert(t.name()), "two handles for {}", t.name());
        }
        for (lang, json) in FILES {
            for (key, _) in parse_file(lang, json).unwrap() {
                let name = format!("{lang}/{key}");
                assert!(seen.contains(name.as_str()), "{name} is in the book and no handle reads it");
            }
        }
    }

    #[test]
    fn a_text_is_looked_up_once_and_reads_the_same_ever_after() {
        let t = crate::prompts::ALL_TEXTS[0];
        let first = t.get();
        assert!(std::ptr::eq(first, t.get()));
        assert_eq!(t.to_string(), first);
        assert_eq!(format!("{t}"), first);
    }

    #[test]
    fn a_text_of_several_lines_is_its_lines_joined() {
        let file = r#"{"format": 1, "lang": "xx", "texts": {"one": "a line", "many": ["", "[Head] first", "second", ""]}}"#;
        let list = parse_file("xx", file).unwrap();
        let get = |k: &str| list.iter().find(|(key, _)| key == k).map(|(_, t)| t.as_str());
        assert_eq!(get("one"), Some("a line"));
        assert_eq!(get("many"), Some("\n[Head] first\nsecond\n"));
    }

    #[test]
    fn a_file_that_is_not_the_book_is_refused() {
        for (file, why) in [
            (r#"{"format": 2, "lang": "xx", "texts": {"a": "b"}}"#, "format"),
            (r#"{"format": 1, "lang": "yy", "texts": {"a": "b"}}"#, "lang"),
            (r#"{"format": 1, "lang": "xx"}"#, "texts"),
            (r#"{"format": 1, "lang": "xx", "texts": {"Bad-Key": "b"}}"#, "key"),
            (r#"{"format": 1, "lang": "xx", "texts": {"a": 3}}"#, "neither"),
            (r#"{"format": 1, "lang": "xx", "texts": {"a": ["b", 3]}}"#, "line"),
            ("not json", "JSON"),
        ] {
            let err = parse_file("xx", file).unwrap_err();
            assert!(err.contains(why), "{file}: {err}");
        }
    }
}

