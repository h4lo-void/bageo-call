// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The fixed tables the checks use. They are compiled in: a pack never brings
//! its own list of languages, countries or banned phrases.

/// Where the language list comes from.
pub const OFFICIAL_LANGS_SOURCE: &str = "https://firebase.google.com/docs/ai-logic/live-api/limits-and-specs";

/// When the list below was put together.
pub const OFFICIAL_LANGS_SNAPSHOT_DATE: &str = "2026-11-20";

/// Language and speech-to-text codes a pack may use: the Gemini Live languages
/// (snapshot of the official page, last updated 2026-11-18 UTC; see
/// [OFFICIAL_LANGS_SOURCE]), written exactly as the page writes them: 79 codes.
/// Chinese is Mandarin only (cmn), Spanish is es-US, Portuguese is pt-BR.
/// Dialects (Cantonese, Hokkien, ...) and codes such as cmn-TW, es-ES, pt-PT or
/// en-GB are not on the list and cannot be used.
pub const OFFICIAL_LANGS: &[&str] = &[
    // Every model that speaks (24).
    "ar-EG", "de-DE", "en-US", "es-US", "fr-FR", "hi-IN", "id-ID", "it-IT", "ja-JP", "ko-KR", "pt-BR", "ru-RU",
    "nl-NL", "pl-PL", "th-TH", "tr-TR", "vi-VN", "ro-RO", "uk-UA", "bn-BD", "en-IN", "mr-IN", "ta-IN", "te-IN",
    // Only the 3.x models (55).
    "af", "sq", "am", "hy", "az", "eu", "be", "bg", "my", "ca", "ceb", "cmn", "hr", "cs", "da", "et", "fil", "fi", "gl",
    "ka", "el", "gu", "ht", "he", "hu", "is", "jv", "kn", "kok", "lo", "la", "lv", "lt", "lb", "mk", "mai", "mg", "ms",
    "ml", "mn", "ne", "nb", "nn", "or", "ps", "fa", "pa", "sr", "sd", "si", "sk", "sl", "sw", "sv", "ur",
];

pub fn is_official_lang(code: &str) -> bool {
    OFFICIAL_LANGS.contains(&code)
}

/// ISO 3166-1 alpha-2 codes, lower case. "xk" (Kosovo) is allowed on top.
const ISO_COUNTRIES: &str = "ad ae af ag ai al am ao aq ar as at au aw ax az ba bb bd be bf bg bh bi bj bl bm bn bo bq br \
bs bt bv bw by bz ca cc cd cf cg ch ci ck cl cm cn co cr cu cv cw cx cy cz de dj dk dm do dz ec ee eg eh er es et fi fj fk \
fm fo fr ga gb gd ge gf gg gh gi gl gm gn gp gq gr gs gt gu gw gy hk hm hn hr ht hu id ie il im in io iq ir is it je jm jo \
jp ke kg kh ki km kn kp kr kw ky kz la lb lc li lk lr ls lt lu lv ly ma mc md me mf mg mh mk ml mm mn mo mp mq mr ms mt mu \
mv mw mx my mz na nc ne nf ng ni nl no np nr nu nz om pa pe pf pg ph pk pl pm pn pr ps pt pw py qa re ro rs ru rw sa sb sc \
sd se sg sh si sj sk sl sm sn so ss st sv sx sy sz tc td tf tg th tj tk tl tm tn to tr tt tv tw tz ua ug um us uy uz va vc \
ve vg vi vn vu wf ws ye yt za zm zw";

pub fn is_country(code: &str) -> bool {
    code == "xk" || ISO_COUNTRIES.split_whitespace().any(|c| c == code)
}

/// Countries whose main language is one of the nine settings languages (the
/// agent's country-to-language table). A region outside it must bring its own hello.
pub const NINE_LANG_COUNTRIES: &[&str] = &[
    "us", "ca", "gb", "ie", "au", "nz", "gg", "im", "je", "sg", "ag", "ai", "as", "bb", "bm", "bs", "dm", "gd", "gu", "jm",
    "kn", "ky", "lc", "mp", "ms", "sx", "tc", "tt", "vc", "vg", "vi", "jp", "kr", "es", "mx", "ar", "co", "cl", "pe", "ve",
    "ec", "bo", "py", "uy", "gt", "hn", "sv", "ni", "cr", "pa", "cu", "do", "pr", "fr", "re", "yt", "gp", "bl", "mf", "mq",
    "gf", "pm", "nc", "pf", "wf", "be", "mc", "de", "at", "ch", "li", "br", "pt", "cn", "tw", "hk", "mo",
];

/// Primary language codes of the nine settings languages.
pub const NINE_LANG_PRIMARY: &[&str] = &["en", "cmn", "zh", "ja", "ko", "es", "fr", "de", "pt"];

/// The only countries where a region may set chinese_dialects.
pub const DIALECT_COUNTRIES: &[&str] = &["tw", "hk", "mo", "cn", "sg", "my"];

/// Phrases a pack must never teach the AI (compared in lower case after folding).
pub const BANNED: &[&str] = &[
    "查一下",
    "查一查",
    "查詢一下",
    "查询一下",
    "查詢中",
    "查询中",
    "請稍等",
    "请稍等",
    "稍等一下",
    "稍等片刻",
    "答應轉帳",
    "答应转账",
    "給驗證碼",
    "给验证码",
    "hold on",
    "hang on",
    "let me check",
    "let me look",
    "one moment",
    "please wait",
    "how may i help",
    "how can i help you",
    "agree to transfer",
    "give you the code",
    "read out the code",
];

/// Words that would put on a language Gemini Live does not speak, or hold a
/// call to one language. (Moved from the agent's prompt tests.)
pub const UNSUPPORTED_OR_LIMITING: [&str; 21] = [
    "跟著整句用那種話回",
    "用台語回",
    "用客語回",
    "用粵語回",
    "用粤语回",
    "繼續用台語",
    "【當地話】",
    "【當地講法】",
    "[Local language]",
    "[Local speech]",
    "answer in it from then on",
    "answer in Cantonese",
    "answer in Hokkien",
    "keep answering in",
    "Kansai",
    "Swiss German",
    "European Portuguese",
    "只講英文",
    "只講當地",
    "only in English",
    "Speak only",
];

/// Names of Arabic accents. Gemini Live speaks one Arabic; a pack may not
/// tell the AI to put on a regional one.
pub const ARABIC_ACCENTS: &[&str] = &[
    "egyptian arabic",
    "egyptian dialect",
    "gulf arabic",
    "gulf dialect",
    "khaleeji",
    "levantine",
    "maghrebi",
    "moroccan arabic",
    "darija",
    "iraqi arabic",
    "saudi arabic",
    "hejazi",
    "najdi",
    "syrian arabic",
    "lebanese arabic",
    "algerian arabic",
    "tunisian arabic",
    "libyan arabic",
    "sudanese arabic",
    "jordanian arabic",
    "palestinian arabic",
    "yemeni arabic",
    "emirati arabic",
    "埃及腔",
    "海灣腔",
    "海湾腔",
    "黎凡特",
    "馬格里布",
    "马格里布",
    "摩洛哥腔",
    "沙烏地腔",
    "沙特腔",
    "伊拉克腔",
    "敘利亞腔",
    "叙利亚腔",
    "黎巴嫩腔",
];

/// Top-level domains the URL check recognises when there is no scheme.
pub const URL_TLDS: &[&str] = &[
    "com", "net", "org", "edu", "gov", "io", "app", "dev", "xyz", "top", "info", "biz", "tw", "cn", "jp", "kr", "hk", "uk",
];
