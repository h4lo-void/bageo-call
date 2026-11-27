// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Local data packs for Bageo: how a region's local habits are written down,
//! the rules every pack must pass, and the single bundle file
//! (bageo-pack.json) the agent reads.
//!
//! The same checks run in CI on the source files and inside Bageo on every
//! bundle it loads, so one rule set decides what may reach a prompt.
//!
//! Source files: one JSON per region (regions/<code>.json) and one shared
//! layer per language (languages/<layer>/call.json). They are strict: an
//! unknown field is an error. The bundle is tolerant: a newer pack may carry
//! fields this version does not know, and they are ignored.
//!
//! Translations for languages outside the nine core ones live next to the
//! layers (languages/<code>/names.json, notify.json, ...) and ship as their
//! own download, bageo-locales.json.gz ([locales]); [export] turns the app
//! and desktop parts into the apps' resource files and [sync] copies the
//! English originals out of bageo-call. The [extras] module holds the
//! on-demand downloads: one file per extra language, carrying the drawings
//! of its regions, and the drawings built into Bageo.
//! The [prompts] module holds the words Bageo tells the models and the
//! download of changed ones, bageo-prompts.json.gz.

pub mod check;
pub mod data;
pub mod export;
pub mod extras;
pub mod guard;
pub mod locales;
pub mod model;
pub mod prompts;
pub mod scams;
pub mod source;
pub mod sync;
pub mod util;

use std::fmt;

pub use check::{
    check_bundle, check_bundle_at, check_index, pack_bundle, parse_bundle, parse_bundle_bytes, parse_index, parse_index_noting,
    sound_locales,
};
pub use extras::{
    art_home, art_homes, build_art_bundle, build_extras, check_art, check_art_sources, load_art, load_region_langs, parse_art_bundle,
    parse_extras_index, parse_lang_file, read_art_source, ArtBundle, ArtHome, ExtraLang, ExtrasArtRef, ExtrasBuild, ExtrasIndex,
    ExtrasLangRef, ART_BUNDLE_FILE, BUILTIN_ART_REGIONS, EXTRAS_FORMAT, EXTRAS_INDEX_FILE,
};
pub use locales::{
    build_locales_at, build_locales_call_at, check_locales, check_locales_ref, load_locales, lrule, pack_locales, parse_locales,
    parse_locales_limited, sanitize_locales, unpack_locales, ClosingJudge, English, Kind, LocaleSources, Locales, LOCALES_FORMAT,
    MAX_LANG_BYTES, MAX_LOCALES_BYTES,
};
pub use model::{Bundle, DeliveryKey, Entry, Hello, Index, LangInfo, LocalesRef, MultilangEntry, Name, RegionData, TalkStyle, Text};
pub use prompts::{
    build_prompts_at, builtin_book, check_prompts_ref, check_text, load_prompt_changes, pack_prompts, parse_book_file, parse_prompts,
    sound_prompts, BookFile, BuiltinBook, PromptText, PromptsPack, TextProblem, MAX_PROMPTS_BYTES, MAX_PROMPTS_PACKED, PROMPTS_FILE,
    PROMPTS_FORMAT,
};
pub use scams::{
    build_scams_at, check_scams_ref, pack_scams, parse_scams, scam_counts, sound_scams, unpack_scams_bytes, ScamBundle, MAX_SCAMS_BYTES,
    SCAMS_FILE, SCAMS_FORMAT,
};
pub use source::{build, build_at, check_sources, load_sources, Sources};
pub use util::sha256_hex;

/// The bundle format this build reads and writes.
pub const FORMAT: u32 = 1;

/// A bundle (and a source file, per file) larger than this is refused.
pub const MAX_BUNDLE_BYTES: usize = 4 * 1024 * 1024;

/// The checker's name and version as written into a bundle.
pub fn checker_id() -> String {
    format!("bageo-packs {}", env!("CARGO_PKG_VERSION"))
}

/// Rule codes carried by [Issue::rule].
pub mod rule {
    /// Unreadable file, wrong schema or format number.
    pub const FORMAT: &str = "R01-format";
    /// A source file has a field this format does not define.
    pub const UNKNOWN_FIELD: &str = "R01-unknown-field";
    /// Code shape, file-name match, duplicates, layer reference.
    pub const CODE: &str = "R02-code";
    /// Country code invalid, repeated, or owned by two regions.
    pub const COUNTRY: &str = "R03-country";
    /// Language or speech-to-text code not in the official snapshot.
    pub const LANG: &str = "R04-lang";
    /// A required field (after merging with the layer) is missing or empty.
    pub const REQUIRED: &str = "R05-required";
    /// Character, item or total limits.
    pub const LENGTH: &str = "R06-length";
    /// A placeholder other than {owner}, or {owner} where it has no meaning.
    pub const PLACEHOLDER: &str = "R07-placeholder";
    /// URL, e-mail, @handle, or six or more digits in a row.
    pub const PATTERN: &str = "R08-pattern";
    /// A phrase the AI must never be taught.
    pub const BANNED: &str = "R09-banned";
    /// Names a language or accent Gemini Live does not speak, or pins the call to one language.
    pub const UNSUPPORTED: &str = "R10-unsupported";
    /// chinese_dialects on a region outside the Chinese-speaking countries.
    pub const DIALECTS: &str = "R11-dialects";
    /// Status, review date, version, build time.
    pub const STATUS: &str = "R12-status";
    /// The countries index or the manifest disagrees with the regions.
    pub const INDEX: &str = "R13-index";
    /// Line breaks, control, zero-width or bidirectional characters, brackets.
    pub const CHARSET: &str = "R14-charset";
    /// The release index file (bageo-pack-index.json).
    pub const RELEASE_INDEX: &str = "R15-release-index";
}

/// One thing a check did not accept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// The file it was found in (for a bundle: bageo-pack.json).
    pub file: String,
    /// The field, as a dotted path; empty when the whole file is meant.
    pub field: String,
    /// One of the codes in [rule].
    pub rule: &'static str,
    pub detail: String,
}

impl Issue {
    pub fn new(file: &str, field: &str, rule: &'static str, detail: impl Into<String>) -> Self {
        Issue { file: file.to_string(), field: field.to_string(), rule, detail: detail.into() }
    }
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.field.is_empty() {
            write!(f, "{}: [{}] {}", self.file, self.rule, self.detail)
        } else {
            write!(f, "{}: {}: [{}] {}", self.file, self.field, self.rule, self.detail)
        }
    }
}
