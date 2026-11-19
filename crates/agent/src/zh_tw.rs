// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Simplified → Traditional Chinese (Taiwan phrasing).
//!
//! Speech-to-text and summary models often write Simplified for Chinese speech.
//! When the owner reads Traditional Chinese (Taiwan) — the language of what the
//! owner reads, `locale::notify_lang` — what they read (transcripts, summaries,
//! notifications, tasks) is converted ([`to_tw`]); for any other reader it is
//! left as it is: a Simplified Chinese owner keeps Simplified (and their name
//! corrections still match). Japanese and Korean are never converted: their
//! kanji / hanja are not Simplified Chinese.

use crate::locale::Lang;
use crate::zh_convert;

/// A speech-to-text fragment the owner reads: converted to zh-TW when the owner
/// reads zh-TW. Line by line, and only lines that can be told from Japanese: a
/// fragment such as 「失礼」 or 「番号」 (its 「します」 not come yet) reads the
/// same in Japanese and in Simplified Chinese, and is left as it is
/// ([`reading_form`]). Finished text (a summary, a transcript, a whole line) goes
/// through [`to_tw_whole`].
pub fn to_tw(text: &str) -> String {
    to_tw_in(crate::locale::notify_lang(), text)
}

/// [`to_tw`] for an owner who reads `reader`.
pub fn to_tw_in(reader: Lang, text: &str) -> String {
    if reader != Lang::ZhTw {
        return text.to_string();
    }
    text.split_inclusive('\n').map(|line| reading_form(line, true)).collect()
}

/// Finished text the owner reads (summary, transcript, a whole line): converted
/// to zh-TW when the owner reads zh-TW. Unlike [`to_tw`], a line made only of
/// characters Simplified Chinese and Japanese share (「他会来」) is converted:
/// nothing more is coming that could show it is Japanese. Lines with kana or
/// hangul, or with kanji only Japanese writes, are left as they are.
pub fn to_tw_whole(text: &str) -> String {
    to_tw_whole_in(crate::locale::notify_lang(), text)
}

/// [`to_tw_whole`] for an owner who reads `reader`.
pub fn to_tw_whole_in(reader: Lang, text: &str) -> String {
    if reader != Lang::ZhTw {
        return text.to_string();
    }
    for_reading(text)
}

/// Text a person reads (transcripts, summaries, notifications) in zh-TW, whole
/// lines: [`norm`] line by line, leaving Japanese and Korean lines as they are —
/// a Japanese caller's お届け／失礼します came out as お屆け／失禮します in the
/// call record — and kanji only Japanese writes (「斉藤」「駅」) as they are.
pub fn for_reading(text: &str) -> String {
    text.split_inclusive('\n').map(|line| reading_form(line, false)).collect()
}

/// Kanji only Japanese writes this way (neither Simplified nor Traditional
/// Chinese): 「駅」「営業」「斉藤」「沢田」.
const JAPANESE_ONLY: &str = "駅営沢斉広図売読険円気様辺変鉄県関対応発総済桜薬楽転伝帰単権検験経続実価処拡雑剣恵黒渋塩銭戦歳歴継覚観譲豊頼顔駆騒齢霊絵栄蔵縄両亜悪圧囲穏仮帯";

/// Written the same in Simplified Chinese and in Japanese, another way in
/// Traditional Chinese: 「失礼」「番号」「会社」 alone may be either.
///
/// 「机」 (a desk in Japanese, 「機」 in Simplified Chinese) is here too: a live
/// fragment of it waits, and the whole line is converted once it is complete
/// (the call record, summaries and notifications go through [`for_reading`]),
/// so 「手机」 still reads 「手機」 in everything the owner keeps. A fragment with
/// another Simplified-only character in it (「手机没电」) is converted at once.
const SHARED_WITH_JAPANESE: &str = "会号礼体国学声区当写旧万与乱争党画担胆恋双参来麦点断辞献尽属数楼状将条窃誉宝灯炉虫励践浅残径茎痴届内携医寝盗湿机";

/// One line as the zh-TW owner reads it. `fragment`: the line may be a piece of
/// one still coming in, so Chinese that reads the same in Japanese waits.
fn reading_form(line: &str, fragment: bool) -> String {
    if has_kana_or_hangul(line) {
        return line.to_string();
    }
    let japanese_only = line.chars().any(|c| JAPANESE_ONLY.contains(c));
    let shared = fragment && line.chars().any(|c| SHARED_WITH_JAPANESE.contains(c));
    if !japanese_only && !shared {
        return norm(line);
    }
    if !has_simplified_only(line) {
        // Nothing in it only Chinese writes: Japanese, or not changed anyway.
        return line.to_string();
    }
    if !japanese_only {
        return norm(line);
    }
    // Chinese with a Japanese name or place in it: converted around those kanji.
    let mut out = String::with_capacity(line.len());
    let mut run = String::new();
    for c in line.chars() {
        if JAPANESE_ONLY.contains(c) {
            out.push_str(&norm(&run));
            run.clear();
            out.push(c);
        } else {
            run.push(c);
        }
    }
    out.push_str(&norm(&run));
    out
}

/// A Chinese character only Simplified Chinese writes this way (「订」「说」「电」).
fn has_simplified_only(line: &str) -> bool {
    line.chars()
        .filter(|c| is_han(*c) && !JAPANESE_ONLY.contains(*c) && !SHARED_WITH_JAPANESE.contains(*c))
        .any(|c| {
            let mut buf = [0u8; 4];
            let s: &str = c.encode_utf8(&mut buf);
            zh_convert::to_tw(s).as_str() != s
        })
}

fn is_han(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c) || ('\u{3400}'..='\u{4dbf}').contains(&c)
}

/// The line has kana or hangul: Japanese or Korean, whose kanji / hanja are not
/// Simplified Chinese.
fn has_kana_or_hangul(line: &str) -> bool {
    line.chars().any(|c| {
        ('\u{3040}'..='\u{30ff}').contains(&c)
            || ('\u{31f0}'..='\u{31ff}').contains(&c)
            || ('\u{ac00}'..='\u{d7af}').contains(&c)
            || ('\u{1100}'..='\u{11ff}').contains(&c)
            || ('\u{3130}'..='\u{318f}').contains(&c)
    })
}

/// Always convert to zh-TW (OpenCC s2tw tables): what the keyword matching
/// reads, whatever the agent's language.
pub fn norm(text: &str) -> String {
    if text.is_empty() || !text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) {
        return text.to_string();
    }
    zh_convert::to_tw(text)
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

    #[test]
    fn japanese_kanji_are_not_rewritten() {
        // Unset settings (tests): the owner reads Traditional Chinese.
        for frag in ["失礼", "番号", "営業", "駅", "斉藤", "沢田", "電話番号", "会社"] {
            assert_eq!(to_tw(frag), frag, "{frag}");
        }
        // A Japanese name in a Chinese summary keeps its kanji; the rest is converted.
        assert_eq!(for_reading("对方是斉藤，说明天再来"), "對方是斉藤，說明天再來");
        assert_eq!(for_reading("對方：沢田です"), "對方：沢田です");
        // Simplified Chinese is still converted, fragments too.
        assert_eq!(to_tw("订位"), "訂位");
        assert_eq!(to_tw("我这边是管理室\n对"), "我這邊是管理室\n對");
        assert_eq!(to_tw("会议改到下午三点"), "會議改到下午三點");
    }

    #[test]
    fn only_an_owner_who_reads_traditional_chinese_gets_it() {
        assert_eq!(to_tw_in(Lang::ZhCn, "软件下载好了，王先生"), "软件下载好了，王先生");
        assert_eq!(to_tw_in(Lang::Ja, "失礼"), "失礼");
        assert_eq!(to_tw_in(Lang::En, "订位"), "订位");
        assert_eq!(to_tw_in(Lang::ZhTw, "软件下载好了"), norm("软件下载好了"));
    }

    #[test]
    fn finished_text_is_converted_even_when_japanese_shares_its_characters() {
        // A whole line has nothing more coming: 「会」「来」 alone are Chinese here.
        assert_eq!(to_tw_whole_in(Lang::ZhTw, "他会来"), "他會來");
        assert_eq!(to_tw_whole_in(Lang::ZhTw, "對方：会来"), "對方：會來");
        assert_eq!(to_tw_whole_in(Lang::ZhTw, "对方：会来\n接聽：好"), "對方：會來\n接聽：好");
        // The fragment mode of a live stream still waits.
        assert_eq!(to_tw_in(Lang::ZhTw, "他会来"), "他会来");
        // Japanese stays: kana, or kanji only Japanese writes.
        assert_eq!(to_tw_whole_in(Lang::ZhTw, "失礼します"), "失礼します");
        assert_eq!(to_tw_whole_in(Lang::ZhTw, "對方：駅に来ます"), "對方：駅に来ます");
        assert_eq!(to_tw_whole_in(Lang::ZhTw, "对方是斉藤，说他会来"), "對方是斉藤，說他會來");
        // A Simplified Chinese (or any other) reader gets the text as it is.
        assert_eq!(to_tw_whole_in(Lang::ZhCn, "他会来"), "他会来");
        assert_eq!(to_tw_whole_in(Lang::Ja, "他会来"), "他会来");
        assert_eq!(to_tw_whole_in(Lang::En, "他会来"), "他会来");
    }

    #[test]
    fn newly_protected_japanese_kanji_and_band_stay_intact() {
        assert_eq!(for_reading("ご案内"), "ご案内");
        assert_eq!(for_reading("携帯"), "携帯");
        assert_eq!(to_tw_whole_in(Lang::ZhTw, "携帯でお願いします"), "携帯でお願いします");
        assert_eq!(to_tw_in(Lang::ZhTw, "案内"), "案内");
        assert_eq!(to_tw_whole_in(Lang::ZhTw, "手机"), "手機");
        assert_eq!(to_tw_whole_in(Lang::ZhTw, "帯"), "帯");
    }

    #[test]
    fn the_japanese_desk_waits_as_a_fragment_and_the_simplified_phone_is_converted_whole() {
        // The call record, summaries and notifications read the whole line
        // ([`for_reading`], [`to_tw_whole_in`]): errands close a caption line through
        // `to_tw_whole`, the dialogue merge through `locale::for_owner`.
        // A Japanese fragment keeps its 机, with or without more kanji or kana around it.
        assert_eq!(to_tw_in(Lang::ZhTw, "机"), "机");
        assert_eq!(to_tw_in(Lang::ZhTw, "机の上に置いてください"), "机の上に置いてください");
        assert_eq!(to_tw_in(Lang::ZhTw, "机の上"), "机の上");
        assert_eq!(for_reading("机の上に置いてください"), "机の上に置いてください");
        assert_eq!(to_tw_whole_in(Lang::ZhTw, "對方：机の上に置いてください"), "對方：机の上に置いてください");
        // Simplified Chinese with 机 is converted whole, and as a fragment once
        // another character only Simplified Chinese writes is in it.
        assert_eq!(to_tw_whole_in(Lang::ZhTw, "手机"), "手機");
        assert_eq!(to_tw_whole_in(Lang::ZhTw, "我的手机号码"), "我的手機號碼");
        assert_eq!(for_reading("我的手机没电了"), "我的手機沒電了");
        assert_eq!(to_tw_in(Lang::ZhTw, "我的手机没电了"), "我的手機沒電了");
        assert_eq!(for_reading("對方：这个手机很好用"), "對方：這個手機很好用");
        // Other readers are left as they are.
        assert_eq!(to_tw_whole_in(Lang::En, "手机"), "手机");
        assert_eq!(to_tw_whole_in(Lang::ZhCn, "手机"), "手机");
    }
}
