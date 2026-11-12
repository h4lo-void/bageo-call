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
    norm(text)
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
}
