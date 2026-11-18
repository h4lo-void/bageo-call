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
            opening: Spoken::Lang(opening),
            now_clear_turns: 0,
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
        let Some(heard) = self.clear(said) else {
            if let Some(pending) = self.pending {
                if let Some(r) = crate::urgency::read_language(said) {
                    if Spoken::Lang(self.same_variant(r.lang)) != pending {
                        self.pending = None;
                    }
                }
            }
            return None;
        };
        self.caller_last = Some(heard);
        if heard == self.now {
            self.settled = true;
            self.pending = None;
            self.now_clear_turns += 1;
            return None;
        }
        if !self.settled || self.pending == Some(heard) {
            // The turn that waited for a second one counts too.
            self.now_clear_turns = if self.pending == Some(heard) { 2 } else { 1 };
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
        // Close Latin languages share short words (a Spanish line can score a
        // point of Portuguese): a drift between them needs a clear margin.
        if crate::urgency::is_latin_lang(ai)
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
    fn multi_turn_unlisted_latin_locks_as_other_without_mislocking() {
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
}
