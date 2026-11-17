//! Ordinary calls, by what they are about (scenario plan, module 2).
//!
//! The second look at a call ([`crate::stall_review`]) also names what an
//! ordinary call is about — a courier, the owner's bank checking a charge, an
//! important notice, setting a time, family chat, a wrong number — in the same
//! answer, no extra request. The AI never waits for it: it answers the caller
//! at once, and once the scenario is known one short cue for it (at most
//! [`MAX_CUE_CHARS`] characters) goes in the pause after the AI's turn, once per
//! scenario per call, never while a sales / scam caller is being stalled.
//! Pure in-memory work.

use crate::country_profile::Region;
use crate::locale::Lang;
use crate::stall_review::{Confidence, Verdict};

/// What an ordinary call is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scenario {
    Delivery,
    BankVerify,
    ImportantNotice,
    ScheduleInvite,
    CasualChat,
    WrongNumber,
    #[default]
    None,
}

impl Scenario {
    pub const ALL: [Scenario; 7] = [
        Scenario::Delivery,
        Scenario::BankVerify,
        Scenario::ImportantNotice,
        Scenario::ScheduleInvite,
        Scenario::CasualChat,
        Scenario::WrongNumber,
        Scenario::None,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Scenario::Delivery => "delivery",
            Scenario::BankVerify => "bank_verify",
            Scenario::ImportantNotice => "important_notice",
            Scenario::ScheduleInvite => "schedule_invite",
            Scenario::CasualChat => "casual_chat",
            Scenario::WrongNumber => "wrong_number",
            Scenario::None => "none",
        }
    }

    /// The review's label; unknown labels are none.
    pub fn from_label(label: &str) -> Scenario {
        let l = label.trim().to_ascii_lowercase();
        Scenario::ALL.into_iter().find(|s| s.as_str() == l).unwrap_or(Scenario::None)
    }
}

/// Longest cue (characters, the whole text with its note mark and the local
/// delivery habit).
pub const MAX_CUE_CHARS: usize = 100;

/// Per language: note mark, then delivery, bank_verify, important_notice,
/// schedule_invite (no booking), schedule_invite (booking on), casual_chat,
/// wrong_number. Simplified Chinese is converted from Traditional.
const CUES: [(Lang, &str, [&str; 7]); 8] = [
    (Lang::ZhTw, "【系統小抄·不是對方說的】", [
        "快遞外送：貨到付款先問金額和寄件人，不代墊大筆錢；他說好、道謝收尾時，回一句帶掰掰的道別。",
        "銀行核對交易：複誦金額、商店、時間；不說是你刷的、也不說沒印象；停卡、卡控都不答應，只說「晚點我自己打卡片背面的客服確認」。",
        "正事通知：認真聽完，不推說在忙；一次補問一項缺的（單位、期限、回電號碼），關鍵數字完整複誦。",
        "約時間：開心接住，說「我先確認那天的安排再回你」，不把時間說死。",
        "約時間：開心接住，照預約流程記下日期時間並複誦確認。",
        "親友閒聊：放鬆跟著聊，不要一條一條追問，不要客服腔。",
        "打錯電話：簡短說他打錯了，不要報出你的名字或身分。",
    ]),
    (Lang::En, "[Note] ", [
        "Courier: COD: amount, sender; they wrap up or thank you: say bye.",
        "Bank: repeat amount, shop, time; don't confirm or deny; no block or hold; call them later.",
        "Notice: hear it all; ask what's missing (who, deadline, callback no.); repeat key figures.",
        "Scheduling: welcome it; say you'll confirm the day later; don't fix a time.",
        "Scheduling: welcome it, book the date and time with the booking steps and repeat them.",
        "Family or friend chat: relaxed and natural, no list of questions, no service tone.",
        "Wrong number: say so briefly; never give the owner's name or identity.",
    ]),
    (Lang::Ja, "[メモ] ", [
        "宅配：代引きは金額と差出人を確認。お礼で締められたら挨拶を返す。",
        "カード会社の確認：金額・店名・時刻を復唱。本人利用とも違うとも覚えがないとも言わず、停止や一時停止にも同意せず「後で公式窓口に確認します」で締める。",
        "大事な連絡：最後まで聞き、足りない点（部署・期限・折り返し番号）を一つずつ確認し、数字は復唱。",
        "日程の相談：明るく受け「後で確定して連絡します」と言い、日時は確定しない。",
        "日程の相談：明るく受け、予約の手順で日時を記録して復唱する。",
        "家族・友人との雑談：肩の力を抜いて合わせる。質問攻めや事務的な口調はしない。",
        "間違い電話：間違いだと短く伝え、持ち主の名前や身元は言わない。",
    ]),
    (Lang::Ko, "[메모] ", [
        "택배: 착불이면 금액과 보낸 사람 확인. 감사 인사로 마무리하면 인사로 답하기.",
        "카드사 확인: 금액·가맹점·시간 복창. 본인 사용 여부도, 기억 안 난다고도 말하지 말고 정지·일시정지 동의 없이 '나중에 공식 고객센터에 확인할게요'로 마무리.",
        "중요한 연락: 끝까지 듣고 빠진 것(부서·기한·회신 번호)을 하나씩 묻고 숫자는 복창.",
        "일정 약속: 반갑게 받고 '나중에 확정해서 연락드릴게요'라고 하며 시간은 확정하지 않기.",
        "일정 약속: 반갑게 받고 예약 절차대로 날짜와 시간을 기록하고 복창.",
        "가족·친구와 수다: 편하게 맞장구, 캐묻거나 상담원 말투는 쓰지 않기.",
        "잘못 걸린 전화: 짧게 알려 주고 주인의 이름이나 신원은 말하지 않기.",
    ]),
    (Lang::Es, "[Nota] ", [
        "Envío: cobro: importe, remitente; si cierra o agradece, despídete.",
        "Banco: repite importe, comercio, hora; ni sí ni no; sin bloqueo ni retención; llamarás tú.",
        "Aviso importante: escucha todo; pide lo que falte (área, plazo, número) y repite las cifras.",
        "Cita: acéptala con gusto; di que la confirmas más tarde; no fijes la hora.",
        "Cita: acéptala con gusto, anota fecha y hora con la reserva y repítelas.",
        "Familia o amigos: relajado y natural, sin interrogar ni tono de operador.",
        "Número equivocado: dilo en breve; no digas el nombre ni datos del titular.",
    ]),
    (Lang::Fr, "[Note] ", [
        "Colis: à payer: combien, de qui; s'il conclut, dis au revoir.",
        "Banque : répète montant, commerce, heure ; ni oui ni non ; aucun blocage ; tu rappelleras.",
        "Avis : écoute tout ; demande ce qui manque (service, délai, numéro) ; répète les chiffres.",
        "Rendez-vous : accueille-le, dis que tu confirmeras plus tard ; rien de fixé.",
        "Rendez-vous : accueille-le, note date et heure avec la réservation et répète-les.",
        "Proche ou ami : détendu et naturel, pas d'interrogatoire ni de ton de standard.",
        "Faux numéro : dis-le brièvement, sans le nom ni l'identité du titulaire.",
    ]),
    (Lang::De, "[Note] ", [
        "Paket: Nachnahme: Betrag, Absender; dankt er zum Schluss, verabschiede dich.",
        "Bank: Betrag, Händler, Zeit wiederholen; weder ja noch nein; nichts sperren; selbst anrufen.",
        "Mitteilung: zuhören; Fehlendes (Stelle, Frist, Rückrufnummer) erfragen; Zahlen wiederholen.",
        "Termin: gern annehmen; sagen, du bestätigst ihn später; nichts fest zusagen.",
        "Termin: gern annehmen, Datum und Uhrzeit per Buchung notieren und wiederholen.",
        "Familie oder Freunde: locker mitreden, nicht ausfragen, kein Hotline-Ton.",
        "Falsch verbunden: kurz sagen; keinen Namen und keine Angaben des Inhabers nennen.",
    ]),
    (Lang::Pt, "[Nota] ", [
        "Entrega: pagar: valor, remetente; se ele encerrar, despeça-se.",
        "Banco: repita valor, loja, hora; nem sim nem não; sem bloqueio ou suspensão; ligue depois.",
        "Aviso importante: ouça tudo; pergunte o que falta (setor, prazo, número) e repita os números.",
        "Marcação: aceite com alegria; diga que confirma mais tarde; não feche o horário.",
        "Marcação: aceite com alegria, registre data e hora pela reserva e repita.",
        "Família ou amigos: descontraído e natural, sem interrogar nem tom de atendente.",
        "Número errado: diga em breve; não diga o nome nem dados do titular.",
    ]),
];

/// The cue of `scenario` in `lang` (None for none); `booking`: appointments are on
/// (the AI books the time). Where parcels go in the call's region is in its prompt
/// already ([`crate::country_profile::block`]): the cue does not say it again. The
/// delivery cue asks for the AI's goodbye once the caller wraps up with thanks:
/// a cue saying "brief" left the AI answering 「好，謝謝」 with no 掰掰, and the
/// call did not end.
pub fn cue(scenario: Scenario, lang: Lang, _region: Option<Region>, booking: bool) -> Option<String> {
    let i = match scenario {
        Scenario::Delivery => 0,
        Scenario::BankVerify => 1,
        Scenario::ImportantNotice => 2,
        Scenario::ScheduleInvite if booking => 4,
        Scenario::ScheduleInvite => 3,
        Scenario::CasualChat => 5,
        Scenario::WrongNumber => 6,
        Scenario::None => return None,
    };
    let row_lang = if lang == Lang::ZhCn { Lang::ZhTw } else { lang };
    let (_, mark, texts) = CUES.iter().find(|(l, _, _)| *l == row_lang)?;
    let text = format!("{mark}{}", texts[i]);
    Some(if lang == Lang::ZhCn { crate::zh_convert::to_cn(&text) } else { text })
}

/// What a review did to the call's routing (for the log).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteAction {
    /// No scenario named.
    None,
    /// The cue goes in the next pause after the AI's turn.
    Queued,
    /// A sales / scam caller is being stalled: ordinary cues stay out.
    Stalling,
    /// Not an ordinary call (scam, recording, no answer).
    NotNormal,
    /// A guess: nothing changes.
    LowConfidence,
    /// This scenario's cue went already.
    AlreadySent,
}

impl RouteAction {
    pub fn as_str(self) -> &'static str {
        match self {
            RouteAction::None => "none",
            RouteAction::Queued => "queued",
            RouteAction::Stalling => "stalling",
            RouteAction::NotNormal => "not_normal",
            RouteAction::LowConfidence => "low_confidence",
            RouteAction::AlreadySent => "already_sent",
        }
    }
}

/// One per call: which scenario cue is owed and which went.
#[derive(Debug, Clone)]
pub struct Router {
    lang: Lang,
    region: Option<Region>,
    booking: bool,
    sent: Vec<Scenario>,
    pending: Option<Scenario>,
}

impl Router {
    pub fn new(lang: Lang, region: Option<Region>, booking: bool) -> Self {
        Router { lang, region, booking, sent: Vec::new(), pending: None }
    }

    /// A review came back. `stalling`: the call is being stalled (or is about to be).
    pub fn on_review(&mut self, scenario: Scenario, verdict: Verdict, confidence: Confidence, stalling: bool) -> RouteAction {
        if scenario == Scenario::None {
            return RouteAction::None;
        }
        if stalling {
            self.pending = None;
            return RouteAction::Stalling;
        }
        if !matches!(verdict, Verdict::Normal | Verdict::TooEarly) {
            return RouteAction::NotNormal;
        }
        if confidence == Confidence::Low {
            return RouteAction::LowConfidence;
        }
        if self.sent.contains(&scenario) {
            return RouteAction::AlreadySent;
        }
        self.pending = Some(scenario);
        RouteAction::Queued
    }

    /// Stalling started: an owed ordinary cue is dropped.
    pub fn drop_pending(&mut self) {
        self.pending = None;
    }

    /// The pause after the AI's turn: the owed cue, once (none while stalling), in
    /// the language the caller speaks (`spoken`, when told) — a Chinese cue on a
    /// Japanese call pulled the AI's next line into Chinese.
    pub fn take_pending(&mut self, stalling: bool, spoken: Option<Lang>) -> Option<(Scenario, String)> {
        let s = self.pending.take()?;
        if stalling || self.sent.contains(&s) {
            return None;
        }
        self.sent.push(s);
        let lang = match spoken {
            Some(l) if !(l.is_zh() && self.lang.is_zh()) => l,
            _ => self.lang,
        };
        Some((s, cue(s, lang, self.region, self.booking)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGIONS: [Region; 21] = [
        Region::Tw, Region::HkMo, Region::Cn, Region::MySg, Region::Us, Region::Ca, Region::GbIe,
        Region::AuNz, Region::In, Region::Jp, Region::Kr, Region::EsEu, Region::EsAm, Region::EsRp,
        Region::Fr, Region::Be, Region::De, Region::At, Region::Ch, Region::Br, Region::Pt,
    ];

    #[test]
    fn labels_round_trip() {
        for s in Scenario::ALL {
            assert_eq!(Scenario::from_label(s.as_str()), s);
        }
        assert_eq!(Scenario::from_label(" Delivery "), Scenario::Delivery);
        assert_eq!(Scenario::from_label("made_up"), Scenario::None);
    }

    #[test]
    fn every_cue_is_short_in_every_language_and_region() {
        for lang in crate::locale::ALL {
            for s in Scenario::ALL.into_iter().filter(|s| *s != Scenario::None) {
                for booking in [false, true] {
                    for region in REGIONS.iter().copied().map(Some).chain([None]) {
                        let c = cue(s, lang, region, booking).unwrap();
                        assert!(c.chars().count() <= MAX_CUE_CHARS, "{lang:?} {s:?} {region:?} {} {c}", c.chars().count());
                    }
                }
            }
            assert!(cue(Scenario::None, lang, None, false).is_none());
        }
    }

    #[test]
    fn delivery_asks_for_the_goodbye_and_leaves_the_local_habit_to_the_prompt() {
        use crate::country_profile::block;
        for (lang, region, habit, bye) in [
            (Lang::ZhTw, Region::Tw, "管理室", "掰掰"),
            (Lang::En, Region::Us, "front porch", "say bye"),
            (Lang::Ja, Region::Jp, "置き配", "挨拶を返す"),
            (Lang::De, Region::De, "Packstation", "verabschiede dich"),
        ] {
            let c = cue(Scenario::Delivery, lang, Some(region), false).unwrap();
            assert!(c.contains(bye) && !c.contains(habit), "{c}");
            assert!(block(region, lang, false).contains(habit), "{lang:?}");
            // No "keep it short" any more: it cost the goodbye.
            for brief in ["俐落", "brief", "手短", "kurz", "짧게", "breve", "bref"] {
                assert!(!c.contains(brief), "{c}");
            }
        }
        let cn = cue(Scenario::Delivery, Lang::ZhCn, Some(Region::Cn), false).unwrap();
        assert!(cn.contains("快递"), "{cn}");
    }

    #[test]
    fn the_bank_and_wrong_number_cues_hold_the_lines() {
        let bank = cue(Scenario::BankVerify, Lang::ZhTw, None, false).unwrap();
        for must in ["複誦金額、商店、時間", "不說是你刷的", "停卡、卡控都不答應", "客服確認"] {
            assert!(bank.contains(must), "{must}");
        }
        let en = cue(Scenario::BankVerify, Lang::En, None, false).unwrap();
        assert!(en.contains("don't confirm or deny") && en.contains("no block or hold"), "{en}");
        let wrong = cue(Scenario::WrongNumber, Lang::ZhTw, None, false).unwrap();
        assert!(wrong.contains("不要報出你的名字"), "{wrong}");
        assert!(cue(Scenario::WrongNumber, Lang::En, None, false).unwrap().contains("never give the owner's name"));
        // No cue tells the AI to make the caller wait.
        for lang in crate::locale::ALL {
            for s in Scenario::ALL.into_iter().filter(|s| *s != Scenario::None) {
                let c = cue(s, lang, None, false).unwrap();
                for bad in ["我查一下", "請稍等", "查詢中", "hold on", "let me check"] {
                    assert!(!c.contains(bad), "{lang:?} {s:?}");
                }
            }
        }
        let book = cue(Scenario::ScheduleInvite, Lang::ZhTw, None, true).unwrap();
        let no_book = cue(Scenario::ScheduleInvite, Lang::ZhTw, None, false).unwrap();
        assert!(book.contains("預約") && no_book.contains("不把時間說死"));
    }

    #[test]
    fn one_cue_per_scenario_and_none_while_stalling() {
        let mut r = Router::new(Lang::ZhTw, Some(Region::Tw), false);
        assert_eq!(r.on_review(Scenario::None, Verdict::Normal, Confidence::High, false), RouteAction::None);
        assert_eq!(r.on_review(Scenario::Delivery, Verdict::ScamOrSales, Confidence::High, false), RouteAction::NotNormal);
        assert_eq!(r.on_review(Scenario::Delivery, Verdict::Normal, Confidence::Low, false), RouteAction::LowConfidence);
        assert!(r.take_pending(false, None).is_none());
        assert_eq!(r.on_review(Scenario::Delivery, Verdict::Normal, Confidence::High, false), RouteAction::Queued);
        let (s, text) = r.take_pending(false, None).unwrap();
        assert_eq!(s, Scenario::Delivery);
        assert!(text.contains("快遞"));
        assert!(r.take_pending(false, None).is_none(), "once");
        assert_eq!(r.on_review(Scenario::Delivery, Verdict::Normal, Confidence::High, false), RouteAction::AlreadySent);
        // A card desk's first words (too early) may already be the bank scenario.
        assert_eq!(r.on_review(Scenario::BankVerify, Verdict::TooEarly, Confidence::Medium, false), RouteAction::Queued);
        // Stalling starts before the pause: the ordinary cue stays out.
        assert!(r.take_pending(true, None).is_none());
        assert_eq!(r.on_review(Scenario::BankVerify, Verdict::Normal, Confidence::High, true), RouteAction::Stalling);
        assert!(r.take_pending(false, None).is_none());
        assert_eq!(r.on_review(Scenario::CasualChat, Verdict::Normal, Confidence::High, false), RouteAction::Queued);
        r.drop_pending();
        assert!(r.take_pending(false, None).is_none());
    }

    #[test]
    fn the_cue_is_in_the_language_the_caller_speaks() {
        // A Chinese-set call where the caller speaks Japanese: the cue is Japanese.
        let mut r = Router::new(Lang::ZhTw, Some(Region::Jp), false);
        r.on_review(Scenario::Delivery, Verdict::Normal, Confidence::High, false);
        let (_, text) = r.take_pending(false, Some(Lang::Ja)).unwrap();
        assert!(text.starts_with("[メモ]") && text.contains("宅配"), "{text}");
        // Chinese of the other script keeps the call's own.
        let mut r = Router::new(Lang::ZhTw, Some(Region::Tw), false);
        r.on_review(Scenario::WrongNumber, Verdict::Normal, Confidence::High, false);
        let (_, text) = r.take_pending(false, Some(Lang::ZhCn)).unwrap();
        assert!(text.contains("打錯電話"), "{text}");
        // An English caller on the same call: English.
        let mut r = Router::new(Lang::ZhTw, Some(Region::Tw), false);
        r.on_review(Scenario::Delivery, Verdict::Normal, Confidence::High, false);
        assert!(r.take_pending(false, Some(Lang::En)).unwrap().1.starts_with("[Note]"));
    }
}
