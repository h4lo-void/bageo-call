// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Keeps what people say on a call out of the log, unless the owner asks for it.
//!
//! By default a log line shows only how many characters were said (`<12 chars>`).
//! With `LOG_CALL_TEXT=1` (also `true`, `yes`, `on`) the words are logged as they
//! are, for troubleshooting and for the simulation tools that read the log.
//! Wrap any call words that go into a log line or an error message in [`Said`]:
//! `{}` shows them as written and `{:?}` shows them quoted, like a plain string.
//!
//! Where a line used to log only a cut-down preview, the wrapper is put on the
//! preview, so with the switch off the count is the preview's, not the whole turn's.

use std::fmt;
use std::sync::OnceLock;

/// Words from a call (the caller's, the AI's, a message, a summary) bound for a log line.
pub struct Said<'a>(pub &'a str);

impl fmt::Display for Said<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_said(f, self.0, enabled(), false)
    }
}

impl fmt::Debug for Said<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write_said(f, self.0, enabled(), true)
    }
}

/// Whether `LOG_CALL_TEXT` asks for call words in the log; read once.
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| flag_on(std::env::var("LOG_CALL_TEXT").ok().as_deref()))
}

/// Logs a reminder at startup when the switch is on.
pub fn announce() {
    if enabled() {
        tracing::warn!("LOG_CALL_TEXT is on: the log now holds what callers and the AI say on calls. Turn it off unless you are troubleshooting.");
    }
}

fn flag_on(value: Option<&str>) -> bool {
    value.is_some_and(|v| {
        let v = v.trim();
        ["1", "true", "yes", "on"].iter().any(|on| v.eq_ignore_ascii_case(on))
    })
}

/// The words as written (or quoted, for `{:?}`) when `show` is set, else their length in characters.
fn write_said(f: &mut fmt::Formatter<'_>, text: &str, show: bool, quoted: bool) -> fmt::Result {
    match (show, quoted) {
        (true, true) => fmt::Debug::fmt(text, f),
        (true, false) => f.write_str(text),
        (false, _) => write!(f, "<{} chars>", text.chars().count()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Probe<'a>(&'a str, bool, bool);
    impl fmt::Display for Probe<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write_said(f, self.0, self.1, self.2)
        }
    }
    fn shown(text: &str, show: bool, quoted: bool) -> String {
        Probe(text, show, quoted).to_string()
    }

    #[test]
    fn off_logs_only_the_character_count_in_every_language() {
        let cases = [
            ("Hello, this is Alex", 19),
            ("喂，我是小明", 6),
            ("こんにちは、アレックスです", 13),
            ("مرحبا، معك أليكس", 16),
            ("Привет, это Алекс", 17),
            ("안녕하세요", 5),
            ("¿Hola? Soy Álex", 15),
            ("สวัสดีครับ", 10),
            ("ok 👍🏽 👨‍👩‍👧", 11),
            ("", 0),
        ];
        for (text, n) in cases {
            for quoted in [false, true] {
                let out = shown(text, false, quoted);
                assert_eq!(out, format!("<{n} chars>"), "{text:?}");
                if !text.is_empty() {
                    assert!(!out.contains(text), "{out}");
                }
            }
        }
    }

    #[test]
    fn on_matches_the_plain_display_and_debug_of_a_string() {
        for text in ["Hello, this is Alex", "喂，我是小明", "こんにちは", "مرحبا", "say \"hi\"\nplease", "👍🏽", ""] {
            assert_eq!(shown(text, true, false), text);
            assert_eq!(shown(text, true, true), format!("{text:?}"));
        }
    }

    #[test]
    fn the_wrapper_follows_the_switch_for_display_and_debug() {
        // The switch is read once from the environment, so check the wrapper against
        // whatever it was started with.
        let text = "你好 Alex";
        let (display, debug) = (Said(text).to_string(), format!("{:?}", Said(text)));
        if enabled() {
            assert_eq!((display, debug), (text.to_string(), format!("{text:?}")));
        } else {
            assert_eq!((display.as_str(), debug.as_str()), ("<7 chars>", "<7 chars>"));
        }
        assert_eq!(format!("{:?}", Some(Said("x"))).contains('x'), enabled());
    }

    #[test]
    fn flag_values() {
        for on in ["1", "true", "TRUE", "Yes", "on", "ON", " 1 ", "oN\n"] {
            assert!(flag_on(Some(on)), "{on:?}");
        }
        for off in ["0", "", " ", "false", "no", "off", "2", "enable", "１", "はい"] {
            assert!(!flag_on(Some(off)), "{off:?}");
        }
        assert!(!flag_on(None));
    }
}
