//! The language a call is held in. It starts as the call's opening language;
//! the caller's first clear words set it, and after that it changes only when
//! the caller says two turns in a row clearly in another language
//! ([crate::urgency::read_language]): a stray "OK", a name, a few kana that
//! speech-to-text wrote into Chinese or a noise change nothing. Every mid-call
//! instruction names it ([crate::prompts::call_language_note]), and when the AI
//! drifts into another language on its own it is told, at most
//! [DRIFT_NOTES_MAX] times a call. Memory only; a few string scans per turn.

use crate::locale::Lang;

/// What a caller speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spoken {
    /// One of the languages with a setting.
    Lang(Lang),
    /// A language without a setting (Thai, Russian, Vietnamese…).
    Other,
}

/// Drift reminders per call.
pub const DRIFT_NOTES_MAX: u32 = 2;

/// The AI's turn came out in another language than the call's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Drift {
    pub ai: Lang,
    pub call: Lang,
    /// The AI's turns so far, this one included.
    pub turn: u32,
}

#[derive(Debug, Clone)]
pub struct CallLanguage {
    now: Spoken,
    /// The caller has said something clear in some language.
    settled: bool,
    /// The other language of the caller's last clear turn, waiting for a second one.
    pending: Option<Spoken>,
    /// The language of the caller's last clear turn.
    caller_last: Option<Spoken>,
    /// Chinese is held in this variant (the prompt's when it is Chinese).
    zh: Lang,
    ai_turns: u32,
    drift_notes: u32,
}

impl CallLanguage {
    /// `opening`: the language the call opens in; `prompt`: the language of the
    /// call's prompt (its Chinese variant is kept whatever script the
    /// speech-to-text writes).
    pub fn new(opening: Lang, prompt: Lang) -> Self {
        let zh = if prompt.is_zh() {
            prompt
        } else if opening.is_zh() {
            opening
        } else {
            Lang::ZhTw
        };
        let opening = if opening.is_zh() { zh } else { opening };
        Self {
            now: Spoken::Lang(opening),
            settled: false,
            pending: None,
            caller_last: None,
            zh,
            ai_turns: 0,
            drift_notes: 0,
        }
    }

    /// The call's language; None for a language without a setting.
    pub fn lang(&self) -> Option<Lang> {
        match self.now {
            Spoken::Lang(l) => Some(l),
            Spoken::Other => None,
        }
    }

    pub fn is_other(&self) -> bool {
        self.now == Spoken::Other
    }

    fn same_variant(&self, lang: Lang) -> Lang {
        if lang.is_zh() {
            self.zh
        } else {
            lang
        }
    }

    /// What `said` is clearly in, if anything.
    fn clear(&self, said: &str) -> Option<Spoken> {
        if crate::urgency::unlisted_language(said) {
            return Some(Spoken::Other);
        }
        crate::urgency::read_language(said)
            .filter(|r| r.clear)
            .map(|r| Spoken::Lang(self.same_variant(r.lang)))
    }

    /// A finished turn of the caller's. Returns the call's new language when it
    /// changed.
    pub fn caller_turn(&mut self, said: &str) -> Option<Spoken> {
        let heard = self.clear(said)?;
        self.caller_last = Some(heard);
        if heard == self.now {
            self.settled = true;
            self.pending = None;
            return None;
        }
        if !self.settled || self.pending == Some(heard) {
            self.settled = true;
            self.pending = None;
            self.now = heard;
            return Some(heard);
        }
        self.pending = Some(heard);
        None
    }

    /// A finished turn of the AI's (its speech-to-text): the drift to remind it
    /// of, when its turn is clearly in another language than the call's that the
    /// caller did not just speak, and reminders are left. `can_tell`: a reminder
    /// can go now; otherwise nothing is counted.
    pub fn ai_turn(&mut self, said: &str, can_tell: bool) -> Option<Drift> {
        self.ai_turns += 1;
        let call = self.lang()?;
        let ai = match self.clear(said)? {
            Spoken::Lang(l) => l,
            Spoken::Other => return None,
        };
        let followed = |s: Option<Spoken>| s == Some(Spoken::Lang(ai));
        if ai == call || followed(self.caller_last) || followed(self.pending) {
            return None;
        }
        if !can_tell || self.drift_notes >= DRIFT_NOTES_MAX {
            return None;
        }
        self.drift_notes += 1;
        Some(Drift { ai, call, turn: self.ai_turns })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::urgency::{caller_language, read_language};

    fn zh_call() -> CallLanguage {
        CallLanguage::new(Lang::ZhTw, Lang::ZhTw)
    }

    #[test]
    fn stray_foreign_letters_and_words_leave_chinese() {
        // Two kana that speech-to-text wrote into Chinese.
        assert_eq!(caller_language("我是你樓下的鄰居の，那個包裹は放在管理室了"), Some(Lang::ZhTw));
        // Taiwan Mandarin with English words in it.
        assert_eq!(caller_language("好啦OK，那我等一下用LINE傳給你"), Some(Lang::ZhTw));
        assert_eq!(caller_language("這個案子你先hold住，OK嗎"), Some(Lang::ZhTw));
        // A foreign name written with 「・」.
        assert_eq!(caller_language("我是幫約翰・史密斯送貨的"), Some(Lang::ZhTw));
        let mut c = zh_call();
        assert_eq!(c.caller_turn("好啦OK，那我等一下用LINE傳給你"), None);
        assert_eq!(c.caller_turn("我是你樓下的鄰居の，那個包裹は放在管理室了"), None);
        assert_eq!(c.lang(), Some(Lang::ZhTw));
    }

    #[test]
    fn whole_sentences_tell_their_language() {
        let r = read_language("すみません、宅配便です。今ご自宅にいらっしゃいますか？").unwrap();
        assert_eq!(r, crate::urgency::Reading { lang: Lang::Ja, clear: true });
        let r = read_language("여보세요, 택배 왔는데 집에 계세요?").unwrap();
        assert_eq!(r, crate::urgency::Reading { lang: Lang::Ko, clear: true });
        // An English caller with a Chinese name in it.
        let r = read_language("Hi, this is 王小明 calling about the package for your office").unwrap();
        assert_eq!(r, crate::urgency::Reading { lang: Lang::En, clear: true });
        assert_eq!(caller_language("Hi, this is 王小明 calling about the package"), Some(Lang::En));
        // Too little to tell.
        assert_eq!(read_language("OK"), None);
        assert!(!read_language("喂？").is_some_and(|r| r.clear));
        assert!(!read_language("Hello?").is_some_and(|r| r.clear));
    }

    #[test]
    fn the_first_clear_words_set_the_call_then_two_turns_change_it() {
        // A US SIM opens in English; the caller speaks Chinese from the start.
        let mut c = CallLanguage::new(Lang::En, Lang::En);
        assert_eq!(c.caller_turn("喂？"), None);
        assert_eq!(c.caller_turn("你好，我是快遞，你的包裹要放哪裡？"), Some(Spoken::Lang(Lang::ZhTw)));
        // One English sentence: not yet.
        let mut c = zh_call();
        assert_eq!(c.caller_turn("你好，我是快遞，你的包裹要放哪裡？"), None);
        assert_eq!(c.caller_turn("Sorry, can you speak English? I don't understand."), None);
        assert_eq!(c.lang(), Some(Lang::ZhTw));
        // Back to Chinese: the English one is forgotten.
        assert_eq!(c.caller_turn("好，那我放在管理室喔"), None);
        assert_eq!(c.caller_turn("Can you hear me? Is this the right number?"), None);
        assert_eq!(c.lang(), Some(Lang::ZhTw));
        // Two in a row: the call is in English now.
        assert_eq!(c.caller_turn("I need to know if you are at home right now."), Some(Spoken::Lang(Lang::En)));
        assert_eq!(c.lang(), Some(Lang::En));
        // Simplified script on a Taiwan call is the same Chinese.
        let mut c = zh_call();
        assert_eq!(c.caller_turn("你好，我是快递，你的包裹放哪里？"), None);
        assert_eq!(c.lang(), Some(Lang::ZhTw));
        // A language without a setting.
        let mut c = zh_call();
        assert_eq!(c.caller_turn("สวัสดีครับ ผมเป็นคนส่งของ"), Some(Spoken::Other));
        assert!(c.is_other() && c.lang().is_none());
    }

    #[test]
    fn the_ai_drifting_alone_is_told_twice_at_most() {
        let mut c = zh_call();
        c.caller_turn("你好，我是快遞，你的包裹要放哪裡？");
        assert_eq!(c.ai_turn("好，那你放管理室就好，謝謝。", true), None);
        // Japanese out of nowhere.
        let d = c.ai_turn("はい、分かりました。管理室に置いてください。", true).unwrap();
        assert_eq!((d.ai, d.call, d.turn), (Lang::Ja, Lang::ZhTw, 2));
        // English: the second one.
        let d = c.ai_turn("Okay, just leave it at the front desk, thank you so much.", true).unwrap();
        assert_eq!((d.ai, d.call, d.turn), (Lang::En, Lang::ZhTw, 3));
        // No more after two.
        assert_eq!(c.ai_turn("Okay, just leave it at the front desk, thank you so much.", true), None);
        // A few words in another language are no drift.
        let mut c = zh_call();
        c.caller_turn("你好，我是快遞，你的包裹要放哪裡？");
        assert_eq!(c.ai_turn("好啊OK，我等一下LINE你。", true), None);
        // Held (the caller is talking): not counted.
        assert_eq!(c.ai_turn("はい、分かりました。管理室に置いてください。", false), None);
        assert!(c.ai_turn("はい、分かりました。管理室に置いてください。", true).is_some());
    }

    #[test]
    fn the_ai_following_a_caller_who_switched_is_no_drift() {
        let mut c = zh_call();
        c.caller_turn("你好，我是快遞，你的包裹要放哪裡？");
        // The caller says one sentence in English; the AI answers in English.
        assert_eq!(c.caller_turn("Sorry, can you speak English? I don't understand."), None);
        assert_eq!(c.ai_turn("Sure, no problem. Please leave it at the front desk.", true), None);
        // A second English turn: the call is in English, still no reminder.
        assert_eq!(c.caller_turn("Okay, I will leave it at the front desk then."), Some(Spoken::Lang(Lang::En)));
        assert_eq!(c.ai_turn("Great, thank you so much for your help.", true), None);
        // Now Chinese from the AI is the drift.
        let d = c.ai_turn("好，謝謝你，那就麻煩你放管理室了。", true).unwrap();
        assert_eq!((d.ai, d.call), (Lang::ZhTw, Lang::En));
    }
}

