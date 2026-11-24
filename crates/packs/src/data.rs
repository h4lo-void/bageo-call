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

/// Writing system a language is written in on the phone and in the console.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Script {
    Arabic,
    Latin,
    Devanagari,
    Japanese,
    Hangul,
    Cyrillic,
    Thai,
    Bengali,
    Tamil,
    Telugu,
    Ethiopic,
    Armenian,
    Myanmar,
    Han,
    Georgian,
    Greek,
    Gujarati,
    Hebrew,
    Kannada,
    Lao,
    Malayalam,
    Odia,
    Gurmukhi,
    Sinhala,
}

/// One entry of [OFFICIAL_LANGS], with what the agent needs to know about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OfficialLang {
    /// The code exactly as the official page writes it (`it-IT`, `fil`).
    pub official: &'static str,
    /// The primary language subtag of `official` (`it`, `fil`, `nb`). This is
    /// the code settings store for a language outside the nine core ones.
    pub code: &'static str,
    pub name_en: &'static str,
    pub script: Script,
    /// Written right to left.
    pub rtl: bool,
    /// One of the nine settings languages (English, Mandarin, Japanese, Korean,
    /// Spanish, French, German, Portuguese; Mandarin covers both zh-TW and zh-CN).
    pub core: bool,
}

/// [OFFICIAL_LANGS] with details, same snapshot, same order.
///
/// Primary codes are unique among the non-core entries, so a non-core
/// language is identified by its `code` alone. Only core entries share one:
/// en-US and en-IN are both `en` (English is a core language; en-IN is listed
/// on the page as an "en-IN & hi-IN bundle" and adds no language of its own).
/// Norwegian is two separate entries, `nb` (Bokmål) and `nn` (Nynorsk), each
/// with its own code; neither is merged into the other. Codes the page does not
/// list (the macro code `no`, legacy `iw`, `in`, `tl`) are not entries here;
/// callers that accept them map them onto an entry themselves.
pub const OFFICIAL_LANG_INFO: [OfficialLang; 79] = [
    OfficialLang { official: "ar-EG", code: "ar", name_en: "Arabic", script: Script::Arabic, rtl: true, core: false },
    OfficialLang { official: "de-DE", code: "de", name_en: "German", script: Script::Latin, rtl: false, core: true },
    OfficialLang { official: "en-US", code: "en", name_en: "English", script: Script::Latin, rtl: false, core: true },
    OfficialLang { official: "es-US", code: "es", name_en: "Spanish", script: Script::Latin, rtl: false, core: true },
    OfficialLang { official: "fr-FR", code: "fr", name_en: "French", script: Script::Latin, rtl: false, core: true },
    OfficialLang { official: "hi-IN", code: "hi", name_en: "Hindi", script: Script::Devanagari, rtl: false, core: false },
    OfficialLang { official: "id-ID", code: "id", name_en: "Indonesian", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "it-IT", code: "it", name_en: "Italian", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "ja-JP", code: "ja", name_en: "Japanese", script: Script::Japanese, rtl: false, core: true },
    OfficialLang { official: "ko-KR", code: "ko", name_en: "Korean", script: Script::Hangul, rtl: false, core: true },
    OfficialLang { official: "pt-BR", code: "pt", name_en: "Portuguese", script: Script::Latin, rtl: false, core: true },
    OfficialLang { official: "ru-RU", code: "ru", name_en: "Russian", script: Script::Cyrillic, rtl: false, core: false },
    OfficialLang { official: "nl-NL", code: "nl", name_en: "Dutch", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "pl-PL", code: "pl", name_en: "Polish", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "th-TH", code: "th", name_en: "Thai", script: Script::Thai, rtl: false, core: false },
    OfficialLang { official: "tr-TR", code: "tr", name_en: "Turkish", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "vi-VN", code: "vi", name_en: "Vietnamese", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "ro-RO", code: "ro", name_en: "Romanian", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "uk-UA", code: "uk", name_en: "Ukrainian", script: Script::Cyrillic, rtl: false, core: false },
    OfficialLang { official: "bn-BD", code: "bn", name_en: "Bengali", script: Script::Bengali, rtl: false, core: false },
    OfficialLang { official: "en-IN", code: "en", name_en: "English", script: Script::Latin, rtl: false, core: true },
    OfficialLang { official: "mr-IN", code: "mr", name_en: "Marathi", script: Script::Devanagari, rtl: false, core: false },
    OfficialLang { official: "ta-IN", code: "ta", name_en: "Tamil", script: Script::Tamil, rtl: false, core: false },
    OfficialLang { official: "te-IN", code: "te", name_en: "Telugu", script: Script::Telugu, rtl: false, core: false },
    OfficialLang { official: "af", code: "af", name_en: "Afrikaans", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "sq", code: "sq", name_en: "Albanian", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "am", code: "am", name_en: "Amharic", script: Script::Ethiopic, rtl: false, core: false },
    OfficialLang { official: "hy", code: "hy", name_en: "Armenian", script: Script::Armenian, rtl: false, core: false },
    OfficialLang { official: "az", code: "az", name_en: "Azerbaijani", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "eu", code: "eu", name_en: "Basque", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "be", code: "be", name_en: "Belarusian", script: Script::Cyrillic, rtl: false, core: false },
    OfficialLang { official: "bg", code: "bg", name_en: "Bulgarian", script: Script::Cyrillic, rtl: false, core: false },
    OfficialLang { official: "my", code: "my", name_en: "Burmese", script: Script::Myanmar, rtl: false, core: false },
    OfficialLang { official: "ca", code: "ca", name_en: "Catalan", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "ceb", code: "ceb", name_en: "Cebuano", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "cmn", code: "cmn", name_en: "Mandarin Chinese", script: Script::Han, rtl: false, core: true },
    OfficialLang { official: "hr", code: "hr", name_en: "Croatian", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "cs", code: "cs", name_en: "Czech", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "da", code: "da", name_en: "Danish", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "et", code: "et", name_en: "Estonian", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "fil", code: "fil", name_en: "Filipino", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "fi", code: "fi", name_en: "Finnish", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "gl", code: "gl", name_en: "Galician", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "ka", code: "ka", name_en: "Georgian", script: Script::Georgian, rtl: false, core: false },
    OfficialLang { official: "el", code: "el", name_en: "Greek", script: Script::Greek, rtl: false, core: false },
    OfficialLang { official: "gu", code: "gu", name_en: "Gujarati", script: Script::Gujarati, rtl: false, core: false },
    OfficialLang { official: "ht", code: "ht", name_en: "Haitian Creole", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "he", code: "he", name_en: "Hebrew", script: Script::Hebrew, rtl: true, core: false },
    OfficialLang { official: "hu", code: "hu", name_en: "Hungarian", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "is", code: "is", name_en: "Icelandic", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "jv", code: "jv", name_en: "Javanese", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "kn", code: "kn", name_en: "Kannada", script: Script::Kannada, rtl: false, core: false },
    OfficialLang { official: "kok", code: "kok", name_en: "Konkani", script: Script::Devanagari, rtl: false, core: false },
    OfficialLang { official: "lo", code: "lo", name_en: "Lao", script: Script::Lao, rtl: false, core: false },
    OfficialLang { official: "la", code: "la", name_en: "Latin", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "lv", code: "lv", name_en: "Latvian", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "lt", code: "lt", name_en: "Lithuanian", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "lb", code: "lb", name_en: "Luxembourgish", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "mk", code: "mk", name_en: "Macedonian", script: Script::Cyrillic, rtl: false, core: false },
    OfficialLang { official: "mai", code: "mai", name_en: "Maithili", script: Script::Devanagari, rtl: false, core: false },
    OfficialLang { official: "mg", code: "mg", name_en: "Malagasy", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "ms", code: "ms", name_en: "Malay", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "ml", code: "ml", name_en: "Malayalam", script: Script::Malayalam, rtl: false, core: false },
    OfficialLang { official: "mn", code: "mn", name_en: "Mongolian", script: Script::Cyrillic, rtl: false, core: false },
    OfficialLang { official: "ne", code: "ne", name_en: "Nepali", script: Script::Devanagari, rtl: false, core: false },
    OfficialLang { official: "nb", code: "nb", name_en: "Norwegian Bokmål", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "nn", code: "nn", name_en: "Norwegian Nynorsk", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "or", code: "or", name_en: "Odia", script: Script::Odia, rtl: false, core: false },
    OfficialLang { official: "ps", code: "ps", name_en: "Pashto", script: Script::Arabic, rtl: true, core: false },
    OfficialLang { official: "fa", code: "fa", name_en: "Persian", script: Script::Arabic, rtl: true, core: false },
    OfficialLang { official: "pa", code: "pa", name_en: "Punjabi", script: Script::Gurmukhi, rtl: false, core: false },
    OfficialLang { official: "sr", code: "sr", name_en: "Serbian", script: Script::Cyrillic, rtl: false, core: false },
    OfficialLang { official: "sd", code: "sd", name_en: "Sindhi", script: Script::Arabic, rtl: true, core: false },
    OfficialLang { official: "si", code: "si", name_en: "Sinhala", script: Script::Sinhala, rtl: false, core: false },
    OfficialLang { official: "sk", code: "sk", name_en: "Slovak", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "sl", code: "sl", name_en: "Slovenian", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "sw", code: "sw", name_en: "Swahili", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "sv", code: "sv", name_en: "Swedish", script: Script::Latin, rtl: false, core: false },
    OfficialLang { official: "ur", code: "ur", name_en: "Urdu", script: Script::Arabic, rtl: true, core: false },
];

/// The non-core entry whose primary code is `code` (exact, lower case).
pub fn official_ext_index(code: &str) -> Option<usize> {
    OFFICIAL_LANG_INFO.iter().position(|l| !l.core && l.code == code)
}

/// ISO 3166-1 alpha-2 codes, lower case. "xk" (Kosovo) is allowed on top.
const ISO_COUNTRIES: &str = "ad ae af ag ai al am ao aq ar as at au aw ax az ba bb bd be bf bg bh bi bj bl bm bn bo bq br \
bs bt bv bw by bz ca cc cd cf cg ch ci ck cl cm cn co cr cu cv cw cx cy cz de dj dk dm do dz ec ee eg eh er es et fi fj fk \
fm fo fr ga gb gd ge gf gg gh gi gl gm gn gp gq gr gs gt gu gw gy hk hm hn hr ht hu id ie il im in io iq ir is it je jm jo \
jp ke kg kh ki km kn kp kr kw ky kz la lb lc li lk lr ls lt lu lv ly ma mc md me mf mg mh mk ml mm mn mo mp mq mr ms mt mu \
mv mw mx my mz na nc ne nf ng ni nl no np nr nu nz om pa pe pf pg ph pk pl pm pn pr ps pt pw py qa re ro rs ru rw sa sb sc \
sd se sg sh si sj sk sl sm sn so sr ss st sv sx sy sz tc td tf tg th tj tk tl tm tn to tr tt tv tw tz ua ug um us uy uz va vc \
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

/// Names of public services and devices that are spelled like a web
/// address and that region text has to mention by name (the Greek
/// government login, a Dutch bank's card reader). Matched whole, in any
/// case, and only without a path after them.
pub const ADDRESS_LIKE_NAMES: &[&str] = &[
    "gov.gr",
    "e.dentifier",
    // Everyday abbreviations written without spaces: "for example" in Danish and
    // Norwegian (f.eks.), Swedish (t.ex., f.ex.), Portuguese (p.ex.), Spanish (p.ej.),
    // German (z.Zt., u.dgl.) and "and more" / "or the like" (m.fl., o.lign.).
    "f.eks",
    "t.ex",
    "f.ex",
    "p.ex",
    "p.ej",
    "m.fl",
    "o.lign",
    "z.zt",
    "u.dgl",
];

/// Instructions that would change the AI's rules or make it someone else,
/// as whole phrases (compared like [BANNED], see [crate::guard]). Region
/// text, layer text, call speech and names may not carry them; a
/// translation may only when its English original does.
pub const RULE_CHANGE_PHRASES: &[&str] = &[
    // English
    "new instructions",
    "system:",
    "system prompt",
    "developer mode",
    "jailbreak",
    // Spanish
    "nuevas instrucciones",
    // French
    "nouvelles instructions",
    // German
    "neue anweisungen",
    // Portuguese
    "novas instruções",
    // Italian
    "nuove istruzioni",
    // Russian
    "новые инструкции",
    // Arabic
    "تعليمات جديدة",
    // Chinese
    "忽略以上",
    "忽略上面",
    "忽略之前",
    "忽略前面",
    "忽略規則",
    "忽略规则",
    "無視規則",
    "无视规则",
    "無視以上",
    "无视以上",
    "忘記規則",
    "忘记规则",
    "從現在起你是",
    "从现在起你是",
    "從現在開始你是",
    "从现在开始你是",
    "新的指令",
    "系統提示詞",
    "系统提示词",
    // Japanese
    "指示を無視",
    "ルールを無視",
    "上記を無視",
    "以前の指示",
    "今からあなたは",
    "あなたは今から",
    "新しい指示",
    // Korean
    "지시를 무시",
    "규칙을 무시",
    "위의 내용을 무시",
    "이전 지시",
    "지금부터 너는",
    "이제부터 너는",
    "지금부터 당신은",
    "이제부터 당신은",
    "새로운 지시",
];

/// "You are now …" in several languages. Ordinary text says it too ("Vous
/// êtes maintenant connecté", "Du bist jetzt verbunden"), so it only counts
/// when one of [RULE_CHANGE_IDENTITY_NEXT] follows: someone the AI is told
/// to become ("You are now DAN", "ab jetzt bist du ein Assistent").
pub const RULE_CHANGE_IDENTITY_PHRASES: &[&str] = &[
    "you are now",
    "from now on you are",
    "ahora eres",
    "a partir de ahora eres",
    "tu es maintenant",
    "vous êtes maintenant",
    "désormais tu es",
    "désormais vous êtes",
    "du bist jetzt",
    "ab jetzt bist du",
    "agora você é",
    "a partir de agora você é",
    "da ora in poi sei",
    "d'ora in poi sei",
    "с этого момента ты",
    "من الآن أنت",
];

/// Whole words that turn [RULE_CHANGE_IDENTITY_PHRASES] into an instruction:
/// articles, possessives and the roles a prompt injection hands out.
/// Prepositions are left out ("vous êtes maintenant en ligne" is fine).
pub const RULE_CHANGE_IDENTITY_NEXT: &[&str] = &[
    "a", "an", "the", "my", "your", "dan", "ai", "assistant", "bot", "chatgpt", "gemini", "developer", "admin", "un", "una", "uno",
    "el", "la", "le", "l", "il", "lo", "mi", "tu", "mon", "ma", "mio", "mia", "ein", "eine", "der", "die", "das", "mein", "meine",
    "dein", "um", "uma", "o", "meu", "minha", "ki", "ia", "asistente", "assistente", "assistent", "assistante", "мой", "моя",
    "ассистент", "ассистентом", "бот", "ботом", "ии", "مساعد", "مساعدي", "بوت", "نظام",
];

/// Word starts of verbs that only ever mean "drop it" ("ignore", "игнорируй",
/// "تجاهل"); with a rule word or a word for what came before in the next
/// four words they make an instruction ("Ignore all previous rules").
pub const RULE_CHANGE_STRONG_VERBS: &[&str] = &["ignor", "disregard", "overrid", "bypass", "игнор", "تجاهل"];

/// Word starts of "forget" verbs. Everyday text forgets things all the time
/// ("Don't forget the previous message", "N'oubliez pas de régler"), so
/// these count only with a rule word after them and not when negated.
pub const RULE_CHANGE_WEAK_VERBS: &[&str] = &["forget", "olvid", "oubli", "vergiss", "vergess", "esquec", "dimentic", "забуд", "забыть"];

/// Whole words for rules and instructions (compared without accents;
/// "regelmäßig", "правильно" and "régler" are other words).
pub const RULE_NOUNS: &[&str] = &[
    "rule", "rules", "instruction", "instructions", "prompt", "prompts", "guideline", "guidelines", "regla", "reglas", "instrucción",
    "instrucciones", "règle", "règles", "consigne", "consignes", "regel", "regeln", "anweisung", "anweisungen", "regra", "regras",
    "instrução", "instruções", "regola", "regole", "istruzione", "istruzioni", "правило", "правила", "правил", "инструкция",
    "инструкции", "инструкций", "инструкцию", "указания", "قاعدة", "قواعد", "تعليمات",
];

/// Word starts for "what was said before", counted only after a strong verb
/// ("Ignore everything above", "تجاهل السابق").
pub const RULE_POSITION_WORDS: &[&str] = &[
    "above", "previous", "earlier", "anterior", "precedent", "obig", "vorherig", "выше", "предыдущ", "سابق",
];

/// Words right before a weak verb that negate it ("don't", "no", "n'",
/// "не", "لا"); "don't" reads as "don" and "t".
pub const RULE_NEGATIONS_BEFORE: &[&str] = &["not", "never", "no", "ne", "n", "non", "nao", "nie", "не", "لا"];

/// Words in the two after a weak verb that negate it ("Vergessen Sie
/// nicht", "N'oubliez pas").
pub const RULE_NEGATIONS_AFTER: &[&str] = &["nicht", "pas", "not"];

#[cfg(test)]
mod lang_info_tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn info_matches_the_code_list() {
        assert_eq!(OFFICIAL_LANG_INFO.len(), OFFICIAL_LANGS.len());
        for (info, code) in OFFICIAL_LANG_INFO.iter().zip(OFFICIAL_LANGS) {
            assert_eq!(info.official, *code);
            assert!(is_official_lang(info.official));
        }
    }

    #[test]
    fn code_is_the_primary_subtag() {
        for l in &OFFICIAL_LANG_INFO {
            assert_eq!(l.code, l.official.split('-').next().unwrap(), "{}", l.official);
            assert_eq!(l.code, l.code.to_ascii_lowercase());
            assert!(!l.name_en.is_empty());
        }
    }

    #[test]
    fn non_core_codes_are_unique() {
        let mut seen = HashSet::new();
        for l in OFFICIAL_LANG_INFO.iter().filter(|l| !l.core) {
            assert!(seen.insert(l.code), "duplicate primary code {}", l.code);
        }
        assert_eq!(seen.len(), 70);
        // No non-core code collides with a core one.
        for l in OFFICIAL_LANG_INFO.iter().filter(|l| l.core) {
            assert!(!seen.contains(l.code), "{}", l.code);
        }
    }

    #[test]
    fn shared_and_split_codes() {
        let en: Vec<_> = OFFICIAL_LANG_INFO.iter().filter(|l| l.code == "en").map(|l| l.official).collect();
        assert_eq!(en, ["en-US", "en-IN"]);
        assert!(OFFICIAL_LANG_INFO.iter().filter(|l| l.code == "en").all(|l| l.core));
        let nb = official_ext_index("nb").unwrap();
        let nn = official_ext_index("nn").unwrap();
        assert_ne!(nb, nn);
        assert_eq!(OFFICIAL_LANG_INFO[nb].name_en, "Norwegian Bokmål");
        assert_eq!(OFFICIAL_LANG_INFO[nn].name_en, "Norwegian Nynorsk");
        assert_eq!(official_ext_index("no"), None);
        assert_eq!(official_ext_index("en"), None);
    }

    #[test]
    fn core_set_is_the_nine_languages() {
        let core: HashSet<_> = OFFICIAL_LANG_INFO.iter().filter(|l| l.core).map(|l| l.code).collect();
        let want: HashSet<_> = ["en", "cmn", "ja", "ko", "es", "fr", "de", "pt"].into_iter().collect();
        assert_eq!(core, want);
        for c in &core {
            assert!(NINE_LANG_PRIMARY.contains(c));
        }
    }

    #[test]
    fn right_to_left_languages() {
        let rtl: HashSet<_> = OFFICIAL_LANG_INFO.iter().filter(|l| l.rtl).map(|l| l.code).collect();
        let want: HashSet<_> = ["ar", "he", "fa", "ps", "sd", "ur"].into_iter().collect();
        assert_eq!(rtl, want);
        for l in &OFFICIAL_LANG_INFO {
            if l.rtl {
                assert!(matches!(l.script, Script::Arabic | Script::Hebrew), "{}", l.code);
            }
        }
        let it = &OFFICIAL_LANG_INFO[official_ext_index("it").unwrap()];
        assert_eq!((it.official, it.script, it.rtl), ("it-IT", Script::Latin, false));
        let th = &OFFICIAL_LANG_INFO[official_ext_index("th").unwrap()];
        assert_eq!((th.official, th.script), ("th-TH", Script::Thai));
        let hi = &OFFICIAL_LANG_INFO[official_ext_index("hi").unwrap()];
        assert_eq!(hi.script, Script::Devanagari);
        let ru = &OFFICIAL_LANG_INFO[official_ext_index("ru").unwrap()];
        assert_eq!(ru.script, Script::Cyrillic);
    }
}
