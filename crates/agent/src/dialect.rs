// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Taiwanese / Hokkien, Hakka, and passive Cantonese speech-to-text markers.
//! Gemini Live speaks only the languages on its list, and its Chinese is
//! Mandarin (cmn): the AI never switches to these, never opens or answers in
//! them; it understands what it can and answers in Mandarin ([crate::prompts]).
//! This module only reads speech-to-text, which writes much of those dialects
//! as odd Mandarin, so a review of such a call does not rest on words never said
//! ([crate::stall_review]). Pure string checks on finished text, never on the
//! audio path.

/// Words of Hokkien alone (its grammar and core words), as speech-to-text
/// writes them (in Traditional script: see [marker_hits]).
const HOKKIEN: &[&str] = &[
    "佇咧", "啥物", "按怎", "無要緊", "毋是", "毋免", "毋知", "袂", "恁", "阮", "汝", "遐", "攏",
    "閣", "厝", "明仔載", "下晡", "足感謝", "免客氣", "免煩惱", "按呢", "阿捏", "食飽", "好勢", "敢有", "有影",
    "真濟", "今嘛", "這馬", "佮", "今暗", "暗時", "阮兜", "恁兜", "食飽未", "呷飽未", "食飯未", "好料",
    "共你講", "共我講", "共恁講",
    // As speech-to-text writes them in Mandarin letters (「汝吃飽未？」「有空才吼我知」).
    "吃飽未", "吃飯未", "予我知", "互我知", "吼我知",
    // 「知影」「啥米」「無閒」, and 「按呢」「食飯」 as speech-to-text also writes them.
    "知影", "安捏", "啥米", "無閒", "甲飯", "呷飯",
];

const HAKKA: &[&str] = &[
    "屋下", "仰般", "承蒙", "麼个", "𠊎", "佢兜", "毋使", "恁仔細", "食飯吂", "吂", "這兜", "該兜", "樣般", "做麼",
    "恁靚", "毋會", "今晡日",
];

/// Passive Cantonese markers (STT only; never spoken or switched to).
const CANTONESE: &[&str] = &[
    "唔該", "係咪", "點解", "邊度", "邊個", "咁樣", "冇問題", "唔係", "唔知", "收線", "得閒",
];

/// Hokkien words everyone mixes into Mandarin (「歹勢」「阿嬤」 in Taiwan):
/// everyday speech, never a sign of the dialect.
const LOANS: &[&str] = &["歹勢", "阿嬤"];

/// A question ending on 「無」 (「你佇咧無？」「今暗愛來食飯無？」「好無？」): Hokkien
/// (and Hakka) asks so, Mandarin does not (「有無」「毫無」 are left out).
fn bo_question(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    chars.iter().enumerate().any(|(i, &c)| {
        c == '無'
            && i > 0
            && ('\u{4E00}'..='\u{9FFF}').contains(&chars[i - 1])
            && !"有毫虛虚並并絕绝別别從从全空若一".contains(chars[i - 1])
            && chars.get(i + 1).map_or(true, |n| "？?，,。！!～~ 啦喔咧".contains(*n))
    })
}

/// Distinct words of the list in the text, loans left out (a word inside a
/// longer one found already is not counted again), plus a 「無」 question.
/// Speech-to-text writes Chinese in either script (a Singapore caller's
/// 「来阮兜…好无？」): read in Traditional.
fn marker_hits(markers: &[&str], text: &str) -> usize {
    let mut t = crate::zh_tw::norm(text);
    for l in LOANS {
        t = t.replace(&crate::zh_tw::norm(l), "");
    }
    let mut seen: Vec<String> = Vec::new();
    for m in markers {
        let m = crate::zh_tw::norm(m);
        if t.contains(&m) && !seen.iter().any(|s| s.contains(&m) || m.contains(s.as_str())) {
            seen.push(m);
        }
    }
    seen.len() + usize::from(bo_question(&t))
}

/// The caller's words show Taiwanese / Hokkien, Hakka, or Cantonese markers
/// (one own word or a 「無」 question is enough; loans like 「歹勢」 are not).
pub fn hokkien_or_hakka_words(text: &str) -> bool {
    marker_hits(HOKKIEN, text) > 0 || marker_hits(HAKKA, text) > 0 || marker_hits(CANTONESE, text) > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hokkien_words_through_speech_to_text() {
        // A Singapore friend's dinner invite as speech-to-text wrote it.
        assert!(hokkien_or_hakka_words("喂,王先生啊。你加方位。\n今晚得空無。來軟都示範好無。"));
        assert!(hokkien_or_hakka_words("我知影啦"));
        assert!(hokkien_or_hakka_words("Hello, 王先生 ah! 你吃飽未？"));
        // Simplified speech-to-text, 「無」 written 「无」.
        assert!(hokkien_or_hakka_words("喂,王先生啊。你家饭围。今晚来阮都是饭好无?"));
        assert!(!hokkien_or_hakka_words("您好，我們這邊有免費的體驗課程，想邀請您來參加"));
        assert!(!hokkien_or_hakka_words("喂，王先生，今晚来我家吃饭好吗？"));
    }

    #[test]
    fn hakka_words_count_too() {
        assert!(hokkien_or_hakka_words("喂，王先生，我是送貨个，請問你屋下今晡日有人收包裹無？"));
        assert!(hokkien_or_hakka_words("承蒙你"));
    }

    #[test]
    fn loans_and_taiwanese_mandarin_are_no_dialect() {
        for t in [
            "歹勢啦，阿嬤說明天下午有空",
            "不好意思歹勢啦，我阿嬤叫我打來問一下包裹的事",
            "那個 meeting 改到下週二下午兩點，你 OK 嗎？",
            "你吃飽了沒？要不要一起吃飯",
            "有無問題都可以，你check好schedule再跟我講",
            "你到底欲怎樣才好，這遮住視線哪會這樣",
        ] {
            assert!(!hokkien_or_hakka_words(t), "{t}");
        }
        assert!(hokkien_or_hakka_words("唔該，請問你係咪王生？"));
        assert!(hokkien_or_hakka_words("冇問題，得閒再傾，我收線先"));
    }

    #[test]
    fn a_question_ending_on_bo() {
        assert!(bo_question("你佇咧無？") && bo_question("好無") && bo_question("有人無，我等一下再來"));
        for t in ["有無問題都可以", "這樣毫無意義", "無論如何你先來", "這是有無的問題", "一無所有"] {
            assert!(!bo_question(t), "{t}");
        }
    }
}
