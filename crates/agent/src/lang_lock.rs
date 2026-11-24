// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The language a call is held in. It starts as the call's opening language;
//! the caller's first clear words set it, and after that it changes only when
//! the caller says two turns in a row clearly in another language
//! ([crate::urgency::read_language]): a stray "OK", a name, a few kana that
//! speech-to-text wrote into Chinese or a noise change nothing. Every mid-call
//! instruction names it ([crate::prompts::call_language_note]), and when the AI
//! drifts into another language on its own it is told, at most
//! [DRIFT_NOTES_MAX] times a call, and, when the call is no longer in its
//! opening language, only once the caller has clearly spoken the call's language
//! in [DRIFT_CALLER_TURNS] turns. Memory only; a few string scans per turn.
//! Until the caller has said anything clear, the opening language is only the
//! AI's guess (from the SIM or the number): a caller turn of a few words that
//! reads as another language, even not clearly, moves the call to it for now,
//! and the caller's latest words are never called a drift when the AI answers
//! in their language.
//! The caller's speech-to-text often comes in after the AI has begun its reply,
//! so a turn can hold the end of the last one: a turn in two scripts
//! (「もしもし、大和運輸です。」 then a Spanish misreading) is read part by part
//! ([script_parts]), and one turn never counts twice toward a switch.

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

/// Clear caller turns in the call's language before the AI is told it drifted
/// away from it, when that language is not the one the call opened in: one
/// misread turn must not turn the AI back.
pub const DRIFT_CALLER_TURNS: u32 = 2;

/// Words (Latin script) or letters (Chinese, Japanese, Korean) a caller turn
/// needs before a reading that is not clear moves a call still on its opening
/// guess: a "Hello?" or 「喂？」 does not.
const TENTATIVE_MIN_WORDS: usize = 3;

/// Letters (Chinese, Japanese, Korean, other scripts) or Latin words a part of
/// a turn needs to be read on its own ([script_parts]).
const PART_MIN: usize = 4;

/// Chinese characters a line without kana needs to count as Chinese on a
/// call held in Japanese: Japanese names and nouns are written in them alone
/// (「大和運輸の田中」 has a single kana).
const JA_CALL_HAN_MIN: usize = 8;

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
    /// The language the call opened in.
    opening: Spoken,
    /// The caller's clear turns in `now`.
    now_clear_turns: u32,
    /// The caller has said something clear in some language.
    settled: bool,
    /// The other language of the caller's last clear turn, waiting for a second one.
    pending: Option<Spoken>,
    /// The caller turn the pending language was heard in.
    pending_turn: u32,
    /// Caller turns so far.
    turns: u32,
    /// The language of the caller's last clear turn.
    caller_last: Option<Spoken>,
    /// What the caller's latest turn reads as, clear or not (None: nothing to read).
    caller_read: Option<Lang>,
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
            opening: Spoken::Lang(opening),
            now_clear_turns: 0,
            settled: false,
            pending: None,
            pending_turn: 0,
            turns: 0,
            caller_last: None,
            caller_read: None,
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

    pub fn spoken(&self) -> Spoken {
        self.now
    }

    fn same_variant(&self, lang: Lang) -> Lang {
        if lang.is_zh() {
            self.zh
        } else {
            lang
        }
    }

    /// What `said` is clearly in, if anything. Words in a language without a
    /// setting read as that language when the translations carry it to the
    /// call stage ([crate::urgency::read_other]); in a call held in such a
    /// language, words in the same script count as it (Arabic letters do not
    /// tell Arabic, Persian and Urdu apart) unless their own words tell another
    /// language ([crate::urgency::other_told_apart]: Italian to Indonesian,
    /// Ukrainian to Russian).
    fn clear(&self, said: &str) -> Option<Spoken> {
        if crate::urgency::unlisted_language(said) {
            if let Some(r) = crate::urgency::read_other(said) {
                if let Spoken::Lang(Lang::Ext(now)) = self.now {
                    if now.script() == r.script {
                        return Some(crate::urgency::other_told_apart(said, now).map_or(self.now, Spoken::Lang));
                    }
                }
                if let Some(l) = r.lang {
                    return Some(Spoken::Lang(l));
                }
            }
            return Some(Spoken::Other);
        }
        let r = crate::urgency::read_language(said).filter(|r| r.clear)?;
        if r.lang.is_zh() && self.now == Spoken::Lang(Lang::Ja) && han_letters(said) < JA_CALL_HAN_MIN {
            return None;
        }
        Some(Spoken::Lang(self.same_variant(r.lang)))
    }

    /// A finished turn of the caller's. Returns the call's new language when it
    /// changed. A turn in two scripts is read part by part, in order.
    pub fn caller_turn(&mut self, said: &str) -> Option<Spoken> {
        self.turns += 1;
        let before = self.now;
        for part in script_parts(said) {
            self.caller_part(part);
        }
        (self.now != before).then_some(self.now)
    }

    /// One part of a caller turn. Once the caller has spoken the call's language
    /// clearly, another one takes over only on a second clear turn in it (a
    /// later turn: two parts of one turn are one piece of evidence).
    fn caller_part(&mut self, said: &str) {
        self.caller_read = self.read(said);
        let Some(heard) = self.clear(said) else {
            if let Some(pending) = self.pending {
                if let Some(r) = crate::urgency::read_language(said) {
                    if Spoken::Lang(self.same_variant(r.lang)) != pending {
                        self.pending = None;
                    }
                }
            }
            self.tentative(said);
            return;
        };
        self.caller_last = Some(heard);
        if heard == self.now {
            self.settled = true;
            self.pending = None;
            self.now_clear_turns += 1;
            return;
        }
        let confirmed = self.pending == Some(heard) && self.pending_turn < self.turns;
        if !self.settled || confirmed {
            // The turn that waited for a second one counts too.
            self.now_clear_turns = if confirmed { 2 } else { 1 };
            self.settled = true;
            self.pending = None;
            self.now = heard;
            return;
        }
        self.pending = Some(heard);
        self.pending_turn = self.turns;
    }

    /// What `said` reads as among the languages with a setting, clear or not.
    fn read(&self, said: &str) -> Option<Lang> {
        if crate::urgency::unlisted_language(said) {
            return None;
        }
        crate::urgency::read_language(said).map(|r| self.same_variant(r.lang))
    }

    /// A caller turn that is not clear, on a call still on its opening guess
    /// that the caller never spoke: a few words that read as another language
    /// move the call there. Nothing is settled, so the caller's next clear turn
    /// decides at once.
    fn tentative(&mut self, said: &str) {
        if self.settled || self.now != self.opening || words_or_letters(said) < TENTATIVE_MIN_WORDS {
            return;
        }
        let Some(read) = self.caller_read.map(Spoken::Lang) else { return };
        if read == self.now {
            return;
        }
        self.now = read;
        self.now_clear_turns = 0;
        self.pending = None;
    }

    /// A finished turn of the AI's (its speech-to-text): the drift to remind it
    /// of, when its turn is clearly in another language than the call's that the
    /// caller did not just speak, and reminders are left. `can_tell`: a reminder
    /// can go now; otherwise nothing is counted.
    pub fn ai_turn(&mut self, said: &str, can_tell: bool) -> Option<Drift> {
        self.ai_turn_after(said, "", can_tell)
    }

    /// [Self::ai_turn] with `late`: the caller's words that came in after the
    /// AI began this turn (speech-to-text of the caller often lags behind the
    /// reply). The AI answering in their language is no drift either.
    pub fn ai_turn_after(&mut self, said: &str, late: &str, can_tell: bool) -> Option<Drift> {
        self.ai_turns += 1;
        let call = self.lang()?;
        let ai = match self.clear(said)? {
            Spoken::Lang(l) => l,
            Spoken::Other => return None,
        };
        // Only a turn in a core language is a drift: another language read from
        // its letters may be the call's own told apart wrong.
        if !ai.is_core() {
            return None;
        }
        let followed = |s: Option<Spoken>| s == Some(Spoken::Lang(ai));
        if ai == call
            || followed(self.caller_last)
            || followed(self.pending)
            || self.caller_read == Some(ai)
            || self.read(late) == Some(ai)
        {
            return None;
        }
        // Close Latin languages share short words (a Spanish line can score a
        // point of Portuguese): a drift between them needs a clear margin.
        if call.is_core()
            && crate::urgency::is_latin_lang(ai)
            && crate::urgency::is_latin_lang(call)
            && crate::urgency::latin_margin(said, ai, call) < 2
        {
            return None;
        }
        if !can_tell || self.drift_notes >= DRIFT_NOTES_MAX {
            return None;
        }
        if self.now != self.opening && self.now_clear_turns < DRIFT_CALLER_TURNS {
            return None;
        }
        self.drift_notes += 1;
        Some(Drift { ai, call, turn: self.ai_turns })
    }
}

/// Latin words plus Chinese, Japanese and Korean letters in `said`.
fn words_or_letters(said: &str) -> usize {
    let cjk = said
        .chars()
        .filter(|c| matches!(*c as u32, 0x3040..=0x30FF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF))
        .count();
    let words = said
        .split(|c: char| !c.is_alphabetic() && c != '\'')
        .filter(|w| !w.is_empty() && w.chars().all(|c| c == '\'' || (c.is_alphabetic() && (c as u32) < 0x2000)))
        .count();
    cjk + words
}

/// Chinese characters (Japanese kanji too) in said.
fn han_letters(said: &str) -> usize {
    said.chars().filter(|c| matches!(*c as u32, 0x4E00..=0x9FFF)).count()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Script {
    /// Chinese, Japanese and Korean letters.
    Cjk,
    Latin,
    /// Any other letters (Thai, Cyrillic, Arabic…).
    Other,
}

fn script_of(c: char) -> Option<Script> {
    match c as u32 {
        0x3040..=0x30FA | 0x30FC..=0x30FF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0x1100..=0x11FF | 0x3130..=0x318F => {
            Some(Script::Cjk)
        }
        _ if c.is_ascii_alphabetic() => Some(Script::Latin),
        0xC0..=0x24F | 0x1E00..=0x1EFF if c != '\u{D7}' && c != '\u{F7}' => Some(Script::Latin),
        _ if c.is_alphabetic() => Some(Script::Other),
        _ => None,
    }
}

/// A caller turn cut where it goes over from one script to another: each cut
/// starts a run of [PART_MIN] letters (Latin: words) of a script other than
/// the part before it. Stray words of another script ("OK", 「LINE」, a name)
/// stay in the part around them, and a turn in one script is one part.
fn script_parts(said: &str) -> Vec<&str> {
    // Runs of one script: (script, where it starts, its letters or words).
    let mut runs: Vec<(Script, usize, usize)> = Vec::new();
    let mut in_word = false;
    for (i, c) in said.char_indices() {
        let Some(s) = script_of(c) else {
            in_word = false;
            continue;
        };
        let counts = s != Script::Latin || !in_word;
        in_word = s == Script::Latin;
        match runs.last_mut() {
            Some(r) if r.0 == s => r.2 += usize::from(counts),
            _ => runs.push((s, i, 1)),
        }
    }
    let mut cuts = vec![0];
    let mut last: Option<Script> = None;
    for &(s, start, _) in runs.iter().filter(|r| r.2 >= PART_MIN) {
        if last.is_some_and(|l| l != s) {
            cuts.push(start);
        }
        last = Some(s);
    }
    cuts.push(said.len());
    cuts.windows(2).map(|w| &said[w[0]..w[1]]).collect()
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
        // Without translations: the other languages read as languages without a setting.
        crate::locales_store::testing::use_empty();
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

    #[test]
    fn a_call_that_left_its_opening_language_needs_two_clear_caller_turns_before_the_ai_is_told() {
        // A Chinese call whose caller's first turn reads as clear Portuguese.
        let mut c = zh_call();
        assert_eq!(
            c.caller_turn("Olá, tenho uma encomenda para você, está em casa?"),
            Some(Spoken::Lang(Lang::Pt))
        );
        // One turn is not enough to send the AI back to Portuguese.
        assert_eq!(c.ai_turn("Okay, just leave it at the front desk, thank you so much.", true), None);
        // The caller says Portuguese again: now the English turn is a drift.
        assert_eq!(c.caller_turn("Sim, tudo bem, obrigado, pode deixar com o porteiro."), None);
        let d = c.ai_turn("Okay, just leave it at the front desk, thank you so much.", true).unwrap();
        assert_eq!((d.ai, d.call), (Lang::En, Lang::Pt));
        // A call still in its opening language is told at once, as before.
        let mut c = zh_call();
        c.caller_turn("你好，我是快遞，你的包裹要放哪裡？");
        assert!(c.ai_turn("Okay, just leave it at the front desk, thank you so much.", true).is_some());
    }

    #[test]
    fn latin_words_of_a_language_without_a_word_list_lock_the_call_as_other() {
        for s in [
            "Hän on töissä ja minä olen kotona, se on ihan hyvä.",
            "Jag är i stan och kommer hem i kväll, det är inte så långt.",
            "Dobrý den, mám pro vás balík, jste doma?",
        ] {
            let mut c = CallLanguage::new(Lang::En, Lang::En);
            assert_eq!(c.caller_turn(s), Some(Spoken::Other), "{s}");
            assert_eq!(c.ai_turn("Sure, just leave it with the front desk, thank you.", true), None, "{s}");
        }
    }
}


#[cfg(test)]
mod close_latin_languages {
    use super::*;

    #[test]
    fn a_short_line_of_the_call_language_is_no_drift_but_a_clear_other_one_is() {
        let mut es = CallLanguage::new(Lang::Es, Lang::Es);
        assert_eq!(es.ai_turn("Sí, soy yo. Puedes dejarlo en la portería.", true), None);
        assert!(es.ai_turn("Olá, não estou em casa hoje, você pode deixar com o porteiro, tá bom?", true).is_some());
        let mut en = CallLanguage::new(Lang::En, Lang::En);
        assert_eq!(en.ai_turn("Sure, just leave it with the front desk, merci.", true), None);
        let mut de = CallLanguage::new(Lang::De, Lang::De);
        assert_eq!(de.ai_turn("Ja, das ist mein Paket, bitte beim Nachbarn abgeben.", true), None);
        let mut fr = CallLanguage::new(Lang::Fr, Lang::Fr);
        assert_eq!(fr.ai_turn("Oui, c'est moi, vous pouvez le laisser chez le gardien.", true), None);
    }

    #[test]
    fn a_caller_in_another_language_moves_a_call_still_on_its_opening_guess() {
        // A US SIM opens in English; the caller's first words are in another
        // language, short or not quite clear.
        for (said, lang, ai_said) in [
            ("Hola. Le habla Carlos de DHL.", Lang::Es, "Hola Carlos, ¿en qué puedo ayudarle?"),
            ("Bonjour, ici la Poste, j'ai un colis pour monsieur.", Lang::Fr, "Bonjour, oui, je vous écoute, c'est pour quel colis ?"),
            ("Guten Tag, hier ist der Paketdienst.", Lang::De, "Guten Tag, worum geht es denn bei dem Paket?"),
            ("もしもし、宅配便です。", Lang::Ja, "はい、お荷物の件ですね、どうぞ。"),
            ("여보세요, 택배입니다.", Lang::Ko, "네, 택배 관련해서 말씀해 주세요."),
            ("你好，我這邊是宅配。", Lang::ZhTw, "你好，請問是什麼包裹呢？"),
        ] {
            let mut c = CallLanguage::new(Lang::En, Lang::En);
            assert_eq!(c.caller_turn(said), Some(Spoken::Lang(lang)), "{said}");
            // The AI answering in the caller's language is no drift…
            assert_eq!(c.ai_turn(ai_said, true), None, "{said}");
            // …and its English is not pulled back to: the call is in theirs now.
            assert_eq!(c.ai_turn("I understand you have a package for the owner, how can I help?", true), None, "{said}");
            assert_eq!(c.lang(), Some(lang), "{said}");
        }
    }

    #[test]
    fn a_few_stray_words_leave_the_opening_and_a_settled_call_alone() {
        // Too short to move anything.
        for said in ["Hello?", "Hello hello", "喂？", "Allô ?"] {
            let mut c = CallLanguage::new(Lang::ZhTw, Lang::ZhTw);
            assert_eq!(c.caller_turn(said), None, "{said}");
            assert_eq!(c.lang(), Some(Lang::ZhTw), "{said}");
        }
        // A call the caller already spoke clearly: one unclear turn changes nothing.
        let mut c = CallLanguage::new(Lang::En, Lang::En);
        assert_eq!(c.caller_turn("Hi, I'm calling about the package for your office today."), None);
        assert_eq!(c.caller_turn("Hola. Le habla Carlos."), None);
        assert_eq!(c.lang(), Some(Lang::En));
        // A wrong guess moved by an unclear turn: the caller's next clear turn decides at once.
        let mut c = CallLanguage::new(Lang::En, Lang::En);
        assert!(c.caller_turn("Hola. Le habla Carlos de DHL.").is_some());
        assert_eq!(
            c.caller_turn("Sorry, I actually speak English, I have a package for your office today."),
            Some(Spoken::Lang(Lang::En))
        );
    }

    #[test]
    fn the_ai_answering_the_callers_latest_words_is_no_drift() {
        // Settled in English; the caller says a few unclear words in Spanish and
        // the AI answers in Spanish.
        let mut c = CallLanguage::new(Lang::En, Lang::En);
        c.caller_turn("Hi, I'm calling about the package for your office today.");
        c.caller_turn("Sí, sí, gracias.");
        assert_eq!(c.ai_turn("Muy bien, gracias a usted, ¿algo más?", true), None);
        // The caller's words that came in late (after the AI began) count too.
        let mut c = CallLanguage::new(Lang::En, Lang::En);
        c.caller_turn("Hi, I'm calling about the package for your office today.");
        assert_eq!(
            c.ai_turn_after("D'accord, je note, je lui transmets votre message.", "Bonjour, je préfère parler français, s'il vous plaît.", true),
            None
        );
        // Without them, the same French turn is a drift from English.
        assert!(c.ai_turn("D'accord, je note, je lui transmets votre message, merci beaucoup.", true).is_some());
    }

    #[test]
    fn multi_turn_unlisted_latin_locks_as_other_without_mislocking() {
        // Without translations: the other languages read as languages without a setting.
        crate::locales_store::testing::use_empty();
        let cases = [
            (
                "it",
                "Ciao, sono Marco, ho un pacco per lei da consegnare oggi.",
                "Grazie mille, però posso lasciarlo in portineria adesso?",
                "Prego, lasci pure il pacco però grazie mille per la consegna.",
            ),
            (
                "vi",
                "Xin chào, tôi là nhân viên giao hàng, anh có ở nhà không ạ?",
                "Cảm ơn anh nhiều nhé, tôi gửi bưu kiện ở quầy lễ tân ạ.",
                "Vâng cảm ơn anh, cứ để bưu kiện ở đây giúp tôi nhé.",
            ),
            (
                "id",
                "Halo, saya kurir paket, bapak atau ibu ada di rumah sekarang?",
                "Terima kasih, paketnya sudah saya titipkan kepada satpam ya.",
                "Baik terima kasih banyak, silakan titip paketnya di depan.",
            ),
            (
                "tr",
                "Merhaba efendim, kargonuz kapıda bekliyor, evde misiniz?",
                "Teşekkürler efendim, paketinizi kapıya bırakabilir miyim şimdi?",
                "Tamam teşekkür ederim, kapıya bırakabilirsiniz efendim.",
            ),
            (
                "pl",
                "Dzień dobry, dzwonię z firmy kurierskiej, mam paczkę dla pani.",
                "Dziękuję bardzo, czy można zostawić paczkę u sąsiada?",
                "Tak, dziękuję bardzo, proszę zostawić paczkę u ochrony.",
            ),
            (
                "nl",
                "Goedemorgen, ik bel voor een bezorging en heb een pakketje bij me.",
                "Dank u wel, zal ik het pakket bij de buren neerzetten?",
                "Ja bedankt, zet het pakketje maar bij de buren neer.",
            ),
        ];
        for (code, turn1, turn2, ai_reply) in cases {
            for default in [Lang::En, Lang::Es, Lang::Fr, Lang::Pt, Lang::ZhTw] {
                let mut call = CallLanguage::new(default, default);
                assert_eq!(call.caller_turn(turn1), Some(Spoken::Other), "{code} from {default:?} turn1");
                assert!(call.is_other(), "{code} from {default:?} should lock Other");
                assert_eq!(call.lang(), None, "{code} must not lock as setting lang");
                assert_eq!(call.caller_turn(turn2), None, "{code} from {default:?} turn2 should stay Other");
                assert!(call.is_other(), "{code} must remain Other after turn2");
                assert_eq!(call.ai_turn(ai_reply, true), None, "{code} AI reply must not trigger drift");
            }
        }
    }

    #[test]
    fn multi_turn_setting_latin_languages_lock_and_switch_after_two_turns() {
        let scripts = [
            (
                Lang::Es,
                "Hola, tengo un paquete para usted, ¿lo dejo en la portería?",
                "Sí, muchas gracias, ya se lo dejo con el portero ahora.",
                "Perfecto, muchas gracias, puede dejarlo con el portero.",
            ),
            (
                Lang::Fr,
                "Bonjour, je suis le livreur, vous êtes chez vous pour le colis ?",
                "Très bien, je vais laisser le colis chez votre gardien.",
                "D'accord, merci beaucoup, laissez le colis chez le gardien.",
            ),
            (
                Lang::De,
                "Guten Tag, ich habe ein Paket für Sie, sind Sie gerade zu Hause?",
                "Kein Problem, dann gebe ich das Paket beim Nachbarn ab.",
                "Vielen Dank, geben Sie das Paket bitte beim Nachbarn ab.",
            ),
            (
                Lang::Pt,
                "Olá, sou o entregador, você está em casa para receber o pacote?",
                "Tudo bem, vou deixar o pacote com o porteiro do prédio.",
                "Obrigado, pode deixar o pacote com o porteiro agora.",
            ),
            (
                Lang::En,
                "Hello, I have a delivery for you, are you at home right now?",
                "No worries, I will leave the package with your front desk.",
                "Thanks, please leave the package with the front desk.",
            ),
        ];
        for (lang, t1, t2, ai) in scripts {
            let start = if lang == Lang::En { Lang::Es } else { Lang::En };
            let mut call = CallLanguage::new(start, start);
            assert_eq!(call.caller_turn(t1), Some(Spoken::Lang(lang)), "{lang:?} turn1");
            assert_eq!(call.lang(), Some(lang));
            assert_eq!(call.caller_turn(t2), None, "{lang:?} turn2 steady");
            assert_eq!(call.ai_turn(ai, true), None, "{lang:?} matching AI reply should not drift");
            // Two turns in another setting Latin language switch cleanly.
            let next = if lang == Lang::Es { Lang::Pt } else { Lang::Es };
            let (_, nt1, nt2, _) = scripts.iter().find(|(l, _, _, _)| *l == next).unwrap();
            assert_eq!(call.caller_turn(nt1), None, "{lang:?} -> {next:?} needs 2nd turn confirmation");
            assert_eq!(call.lang(), Some(lang));
            assert_eq!(call.caller_turn(nt2), Some(Spoken::Lang(next)), "{lang:?} -> {next:?} switches on turn2");
            assert_eq!(call.lang(), Some(next));
        }
    }

    #[test]
    fn spoken_reports_variant_and_pending_survives_neutral_turn_only() {
        // Without translations: the other languages read as languages without a setting.
        crate::locales_store::testing::use_empty();
        let mut call = CallLanguage::new(Lang::ZhTw, Lang::ZhTw);
        assert_eq!(call.spoken(), Spoken::Lang(Lang::ZhTw));

        let mut other = CallLanguage::new(Lang::ZhTw, Lang::ZhTw);
        other.caller_turn("สวัสดีครับ ผมเป็นคนส่งของครับ");
        assert!(other.is_other());
        assert_eq!(other.spoken(), Spoken::Other);

        // Settle call on the initial language before switching.
        assert_eq!(call.caller_turn("喂你好，請問包裹放哪裡？"), None);
        // First English turn sets pending.
        assert_eq!(call.caller_turn("Hello, I am calling about your parcel delivery"), None);
        // Short neutral acknowledgement does not wipe pending.
        assert_eq!(call.caller_turn("Okay"), None);
        // Second English turn still promotes the pending language switch.
        assert_eq!(call.caller_turn("Where should I leave the package?"), Some(Spoken::Lang(Lang::En)));

        // Different-language short turn between two Spanish turns clears pending.
        let mut call2 = CallLanguage::new(Lang::En, Lang::En);
        assert_eq!(call2.caller_turn("Hello, how are you doing today?"), None);
        assert_eq!(call2.caller_turn("Buenos días, llamo por su cita de mañana"), None);
        assert_eq!(call2.caller_turn("こんにちは"), None);
        assert_eq!(call2.caller_turn("Confirmamos el horario de mañana"), None);
        assert_eq!(call2.lang(), Some(Lang::En));
    }

    fn ext(code: &str) -> Lang {
        Lang::Ext(crate::locale::Ext::from_code(code).unwrap())
    }

    const IT_SAID: &str = "Buongiorno, sono il corriere, ho un pacco per lei, posso lasciarlo in portineria?";
    const TH_SAID: &str = "สวัสดีค่ะ มีพัสดุมาส่งค่ะ อยู่บ้านไหมคะ";
    const AR_SAID: &str = "مرحبا، أنا من شركة التوصيل، عندي طرد لك اليوم";
    const FA_SAID: &str = "سلام، از شرکت پست تماس می‌گیرم، یک بسته برای شما دارم";

    #[test]
    fn translated_languages_are_told_by_their_words() {
        crate::locales_store::testing::use_table();
        for (said, code) in [(IT_SAID, "it"), (TH_SAID, "th"), (AR_SAID, "ar")] {
            assert_eq!(crate::urgency::read_other(said).and_then(|r| r.lang), Some(ext(code)), "{said}");
            // An English call: the caller's first clear words set it.
            let mut c = CallLanguage::new(Lang::En, Lang::En);
            assert_eq!(c.caller_turn(said), Some(Spoken::Lang(ext(code))), "{said}");
            assert_eq!(c.lang(), Some(ext(code)));
        }
        // Without the translations they are languages without a setting.
        crate::locales_store::testing::use_empty();
        for said in [IT_SAID, TH_SAID, AR_SAID] {
            assert_eq!(crate::urgency::read_other(said).and_then(|r| r.lang), None, "{said}");
            let mut c = CallLanguage::new(Lang::En, Lang::En);
            assert_eq!(c.caller_turn(said), Some(Spoken::Other), "{said}");
        }
    }

    #[test]
    fn a_call_in_a_translated_language_holds_and_changes_only_on_two_clear_turns() {
        crate::locales_store::testing::use_table();
        // An Italian call: Italian keeps it; an "OK" changes nothing; English
        // twice changes it.
        let mut c = CallLanguage::new(ext("it"), ext("it"));
        assert_eq!(c.caller_turn(IT_SAID), None);
        assert_eq!(c.caller_turn("OK"), None);
        assert_eq!(c.caller_turn("Sorry, I do not speak Italian, can we talk in English please?"), None);
        assert_eq!(c.caller_turn("Yes, I would like to talk in English if that is fine with you"), Some(Spoken::Lang(Lang::En)));
        // An Arabic call: Persian words share its letters and keep it.
        let mut c = CallLanguage::new(ext("ar"), ext("ar"));
        assert_eq!(c.caller_turn(AR_SAID), None);
        assert_eq!(c.caller_turn(FA_SAID), None);
        assert_eq!(c.lang(), Some(ext("ar")));
        // A Thai call: Thai holds; Japanese once does not change it.
        let mut c = CallLanguage::new(ext("th"), ext("th"));
        assert_eq!(c.caller_turn(TH_SAID), None);
        assert_eq!(c.caller_turn("すみません、宅配便です。今ご自宅にいらっしゃいますか？"), None);
        assert_eq!(c.caller_turn(TH_SAID), None);
        assert_eq!(c.lang(), Some(ext("th")));
    }

    /// A call in `from` that hears `turns` after its opening line `first`;
    /// returns what each turn changed and where the call ends up.
    fn run(from: &str, first: &str, turns: &[&str]) -> (Vec<Option<Spoken>>, Option<Lang>) {
        let mut c = CallLanguage::new(ext(from), ext(from));
        assert_eq!(c.caller_turn(first), None, "{first}");
        let changes = turns.iter().map(|t| c.caller_turn(t)).collect();
        (changes, c.lang())
    }

    #[test]
    fn a_translated_call_follows_another_language_of_its_script_told_by_its_words() {
        crate::locales_store::testing::use_builtin();
        let to = |code: &str| Some(Spoken::Lang(ext(code)));
        // Italian to Indonesian and to Vietnamese, Polish to Turkish (Latin
        // letters), Ukrainian to Russian and back (Cyrillic): two clear turns.
        for (from, first, turns, code) in [
            (
                "it",
                IT_SAID,
                [
                    "Maaf, saya tidak bisa datang hari ini, nanti saya telepon lagi ya.",
                    "Iya, paketnya titip di rumah tetangga saja, terima kasih.",
                ],
                "id",
            ),
            (
                "it",
                IT_SAID,
                ["Dạ, tôi không có ở nhà, anh gọi lại sau nhé.", "Vâng, anh cứ để hàng ở cửa giúp tôi, cảm ơn anh."],
                "vi",
            ),
            (
                "pl",
                "Dzień dobry, jestem kurierem, czy pani jest teraz w domu?",
                ["Merhaba, kargo için arıyorum, şimdi evde misiniz?", "Evet, tamam, teşekkür ederim, yarın tekrar ararım."],
                "tr",
            ),
            (
                "uk",
                "Добрий день, це кур'єр, ви сьогодні вдома?",
                ["Здравствуйте, это курьер, вы сейчас дома?", "Хорошо, тогда я оставлю посылку у соседей, если можно."],
                "ru",
            ),
            (
                "ru",
                "Здравствуйте, это курьер, вы сейчас дома?",
                ["Вибачте, я не розумію, можна українською, будь ласка?", "Добре, залиште посилку біля дверей, я її потім заберу."],
                "uk",
            ),
        ] {
            let (changes, lang) = run(from, first, &turns);
            assert_eq!(changes, vec![None, to(code)], "{from} -> {code}");
            assert_eq!(lang, Some(ext(code)), "{from} -> {code}");
        }
        // One such turn between turns of the call's language changes nothing.
        let (_, lang) = run("it", IT_SAID, &["Maaf, saya tidak bisa datang hari ini, nanti saya telepon lagi ya.", IT_SAID]);
        assert_eq!(lang, Some(ext("it")));
    }

    #[test]
    fn a_translated_call_holds_on_words_that_do_not_tell_another_language() {
        crate::locales_store::testing::use_builtin();
        for (from, first, turns) in [
            // Arabic letters: Persian and Urdu keep an Arabic call.
            ("ar", AR_SAID, [FA_SAID, "السلام علیکم، میں کورئیر کمپنی سے بات کر رہا ہوں، آپ گھر پر ہیں؟"]),
            ("ar", AR_SAID, [FA_SAID, FA_SAID]),
            // An English word, a name, a short reply in an Italian call.
            ("it", IT_SAID, ["Sì, perfetto, mi mandi tutto per email, grazie, okay?", "Pronto, sono Budi Santoso, il corriere, ho un pacco per lei."]),
            ("it", IT_SAID, ["Iya, terima kasih.", "Iya, terima kasih."]),
            ("it", IT_SAID, ["Sì, grazie.", "Va bene, a domani."]),
            // Belarusian and Bulgarian are not Russian in a Ukrainian call.
            ("uk", "Добрий день, це кур'єр, ви сьогодні вдома?", ["Добры дзень, я кур'ер, вы сёння дома?", "Здравейте, аз съм куриерът, вкъщи ли сте днес?"]),
            ("uk", "Добрий день, це кур'єр, ви сьогодні вдома?", ["Так, дякую.", "Добре, дякую, до побачення."]),
        ] {
            let (changes, lang) = run(from, first, &turns);
            assert_eq!(changes, vec![None, None], "{from}: {turns:?}");
            assert_eq!(lang, Some(ext(from)), "{from}: {turns:?}");
        }
    }

    #[test]
    fn the_ai_drifting_from_a_translated_language_into_a_core_one_is_told() {
        crate::locales_store::testing::use_table();
        let mut c = CallLanguage::new(ext("it"), ext("it"));
        c.caller_turn(IT_SAID);
        // Its own language, or another read from the letters: no drift.
        assert_eq!(c.ai_turn("Sì, va bene, lo lasci pure in portineria, grazie mille.", true), None);
        assert_eq!(c.ai_turn(TH_SAID, true), None);
        // Clearly English: a drift, named for the call's language.
        let d = c.ai_turn("Okay, you can leave the package at the front desk, thank you so much.", true).expect("drift");
        assert_eq!((d.ai, d.call), (Lang::En, ext("it")));
        let note = crate::prompts::language_drift_note(ext("it"), ext("it"));
        assert!(note.contains("Italian"), "{note}");
        let mut c = CallLanguage::new(ext("ar"), ext("ar"));
        c.caller_turn(AR_SAID);
        assert_eq!(c.ai_turn("حسنًا، اترك الطرد عند الباب، شكرًا لك.", true), None);
        assert!(c.ai_turn("Hola, sí, puede dejar el paquete en la puerta, muchas gracias por todo.", true).is_some());
    }
}

/// A call the caller has spoken clearly is not carried off by one misheard line.
#[cfg(test)]
mod one_misheard_line {
    use super::*;
    use crate::urgency::read_language;

    fn clear_in(said: &str, lang: Lang) {
        let r = read_language(said).unwrap_or_else(|| panic!("nothing read: {said}"));
        assert!(r.clear && (r.lang == lang || (r.lang.is_zh() && lang.is_zh())), "{said}: {r:?}");
    }

    /// Two clear lines in each language.
    const LINES: &[(Lang, &str, &str)] = &[
        (Lang::ZhTw, "你好，我是快遞，你的包裹要放哪裡？", "那我就幫你放在一樓管理室，等一下記得去拿喔。"),
        (
            Lang::Ja,
            "もしもし、大和運輸の田中です。お荷物のお届けに参りました。",
            "では、管理人室に預けておきますので、後で受け取ってください。",
        ),
        (Lang::Ko, "여보세요, 택배 왔는데 집에 계세요?", "네, 그럼 경비실에 맡겨 둘게요. 나중에 찾아가세요."),
        (
            Lang::Es,
            "Hola, buenas tardes, le llamo de la empresa de mensajería por un paquete para el señor.",
            "Si no está en casa, lo dejo en la portería y él lo recoge después.",
        ),
        (
            Lang::Fr,
            "Bonjour, je vous appelle pour le colis de monsieur, il est chez le gardien.",
            "Si vous n'êtes pas là, je le laisse chez le voisin et vous le récupérez ce soir.",
        ),
        (
            Lang::De,
            "Guten Tag, ich habe ein Paket für Sie, sind Sie heute zu Hause?",
            "Wenn Sie nicht da sind, gebe ich es beim Nachbarn ab und Sie holen es heute Abend.",
        ),
    ];

    /// Real call: a US SIM opened in English, the caller spoke Japanese, and
    /// their second Japanese line was written down as Spanish.
    #[test]
    fn a_spanish_misreading_in_a_japanese_call_does_not_move_it() {
        let mut c = CallLanguage::new(Lang::En, Lang::ZhTw);
        assert_eq!(c.caller_turn("もしもし、大和運輸の田中です。"), Some(Spoken::Lang(Lang::Ja)));
        let misread = "¿Cómo es lo de ustedes? ¿Qué es lo que va a hacer?";
        assert_eq!(c.caller_turn(misread), None);
        assert_eq!(c.lang(), Some(Lang::Ja));
        // The AI answering that line in Spanish is told nothing about Spanish,
        // and every note still names Japanese.
        assert_eq!(c.ai_turn("Le dejaré el mensaje al señor, él le llamará más tarde.", true), None);
        assert_eq!(c.lang(), Some(Lang::Ja));
        // Back in Japanese: the Spanish line is forgotten, one more changes nothing.
        assert_eq!(c.caller_turn("では、管理人室に預けておきますね。失礼します。"), None);
        assert_eq!(c.caller_turn(misread), None);
        assert_eq!(c.lang(), Some(Lang::Ja));
    }

    /// Their words came in only after the AI began to answer, so they are read
    /// with the next turn: the Japanese part settles the call first.
    #[test]
    fn the_late_end_of_the_last_turn_is_read_apart_from_this_one() {
        let mut c = CallLanguage::new(Lang::En, Lang::ZhTw);
        assert_eq!(c.caller_turn(""), None);
        assert_eq!(
            c.caller_turn("もしもし、大和運輸の田中です。¿Cómo es lo de ustedes? ¿Qué es lo que va a hacer?"),
            Some(Spoken::Lang(Lang::Ja))
        );
        assert_eq!(c.caller_turn("お荷物のお届けですが、今日の午後はご在宅ですか？"), None);
        assert_eq!(c.lang(), Some(Lang::Ja));
        // One script, stray words of another in it: one part.
        assert_eq!(script_parts("好啦OK，那我等一下用LINE傳給你").len(), 1);
        assert_eq!(script_parts("Hi, this is 王小明 calling about the package for your office").len(), 1);
        assert_eq!(script_parts("").len(), 1);
    }

    #[test]
    fn the_first_clear_line_still_replaces_the_opening_guess_at_once() {
        for &(lang, said, _) in LINES {
            let opening = if lang == Lang::En { Lang::Es } else { Lang::En };
            let mut c = CallLanguage::new(opening, opening);
            assert_eq!(c.caller_turn(said), Some(Spoken::Lang(lang)), "{said}");
        }
    }

    #[test]
    fn a_settled_call_moves_only_on_two_clear_lines_in_a_row() {
        for (i, &(from, first, _)) in LINES.iter().enumerate() {
            let (to, one, two) = LINES[(i + 1) % LINES.len()];
            clear_in(one, to);
            clear_in(two, to);
            let mut c = CallLanguage::new(from, from);
            assert_eq!(c.caller_turn(first), None, "{first}");
            assert_eq!(c.caller_turn(one), None, "{from:?}: {one}");
            assert_eq!(c.lang(), Some(from), "{one}");
            assert_eq!(c.caller_turn(two), Some(Spoken::Lang(to)), "{from:?}: {two}");
            // The other way round as well.
            let (_, back, _) = LINES[i];
            assert_eq!(c.caller_turn(back), None, "{to:?}: {back}");
            assert_eq!(c.lang(), Some(to), "{back}");
        }
    }

    #[test]
    fn kanji_alone_on_a_japanese_call_is_no_chinese() {
        let mut c = CallLanguage::new(Lang::Ja, Lang::Ja);
        c.caller_turn("もしもし、宅配便です。");
        for said in ["大和運輸田中", "管理人室", "大和運輸の田中"] {
            assert_eq!(c.caller_turn(said), None, "{said}");
        }
        assert_eq!(c.lang(), Some(Lang::Ja));
        // Whole Chinese sentences still count: two move the call.
        assert_eq!(c.caller_turn("你好，我是快遞，你的包裹要放哪裡？"), None);
        assert_eq!(c.caller_turn("那我就幫你放在一樓管理室，等一下記得去拿喔。"), Some(Spoken::Lang(Lang::ZhTw)));
    }
}
