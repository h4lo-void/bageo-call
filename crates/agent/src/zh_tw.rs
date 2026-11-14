//! Simplified → Traditional Chinese (Taiwan phrasing).
//!
//! Speech-to-text and summary models often write Simplified for Chinese speech.
//! With the agent's language set to zh-TW, everything the owner reads
//! (transcripts, summaries, notifications) is converted ([`to_tw`]); in any
//! other language the text is left as it is — the conversion would rewrite
//! Japanese kanji and Simplified Chinese.

use zhconv::{zhconv, Variant};

/// Text the owner reads: converted to zh-TW when that is the agent's language.
pub fn to_tw(text: &str) -> String {
    if crate::locale::current() != crate::locale::Lang::ZhTw {
        return text.to_string();
    }
    for_reading(text)
}

/// Text a person reads (transcripts, summaries, notifications) in zh-TW: [`norm`]
/// line by line, leaving Japanese and Korean lines as they are — a Japanese
/// caller's お届け／失礼します came out as お屆け／失禮します in the call record.
pub fn for_reading(text: &str) -> String {
    text.split_inclusive('\n')
        .map(|line| if is_japanese_or_korean(line) { line.to_string() } else { norm(line) })
        .collect()
}

/// The line has kana or hangul: Japanese or Korean, whose kanji / hanja are not
/// Simplified Chinese.
fn is_japanese_or_korean(line: &str) -> bool {
    line.chars().any(|c| {
        ('\u{3040}'..='\u{30ff}').contains(&c)
            || ('\u{31f0}'..='\u{31ff}').contains(&c)
            || ('\u{ac00}'..='\u{d7af}').contains(&c)
            || ('\u{1100}'..='\u{11ff}').contains(&c)
            || ('\u{3130}'..='\u{318f}').contains(&c)
    })
}

/// Always convert to zh-TW (OpenCC + MediaWiki tables; includes 软件→軟體-style
/// phrases): what the keyword matching reads, whatever the agent's language.
pub fn norm(text: &str) -> String {
    if text.is_empty() || !text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) {
        return text.to_string();
    }
    zhconv(text, Variant::ZhTW)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_common_call_phrases() {
        assert_eq!(norm("要联系"), "要聯繫");
        assert!(norm("对方表示没有事情要转达").contains("對方表示沒有事情要轉達"));
        assert!(norm("我这边是管理室，你有一个包裹").contains("我這邊是管理室"));
        assert_eq!(norm("听录音"), "聽錄音");
        assert_eq!(norm("Caller: 0900"), "Caller: 0900");
        assert_eq!(norm("已是繁體"), "已是繁體");
    }

    #[test]
    fn japanese_and_korean_lines_are_left_alone() {
        let call = "對方：お荷物のお届けですが、今日の午後はご在宅ですか？\n接聽：好，我知道了，你放管理室\n對方：그럼 경비실에 맡겨 둘게요\n對方：我这边是管理室";
        let out = for_reading(call);
        assert!(out.contains("お届け") && !out.contains("お屆け"), "{out}");
        assert!(out.contains("그럼 경비실에"), "{out}");
        assert!(out.contains("我這邊是管理室"), "{out}");
        assert_eq!(for_reading("失礼します"), "失礼します");
    }
}
