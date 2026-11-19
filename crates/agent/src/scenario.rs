// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Ordinary calls, by what they are about (scenario plan, module 2).
//!
//! The second look at a call ([`crate::stall_review`]) also names what an
//! ordinary call is about — a courier, the owner's bank checking a charge, an
//! important notice, setting a time, family chat, a wrong number — in the same
//! answer, no extra request. The AI never waits for it: it answers the caller
//! at once, and once the scenario is known one short cue for it (at most
//! [`MAX_CUE_CHARS`] characters) goes in the pause after the AI's turn, once per
//! scenario per call, never while a sales / scam caller is being stalled.
//! When that look gets no answer in time, the caller's own words still name the
//! scenarios plain words show ([local_guess]: a courier, a card desk checking
//! a purchase, a clinic, school or office with a change, a deadline or a number
//! to call back). Pure in-memory work.

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
/// wrong_number — each a colleague's short reminder of what kind of call it is
/// and where to take it, with the lines not to cross; never a line to say.
/// Simplified Chinese is converted from Traditional.
const CUES: [(Lang, &str, [&str; 7]); 8] = [
    (Lang::ZhTw, "【系統小抄·不是對方說的】", [
        "是快遞，照這裡的習慣讓他把東西放好就好；貨到付款先問金額和寄件人，他道謝收尾就自然說掰掰。",
        "像銀行在核對刷卡：聽清楚金額、商店、時間，不說是不是你刷的、沒印象或要再想想，也不答應停卡或卡控，說你晚點自己打卡片背面的客服確認。",
        "是正事通知，認真聽完，別推說在忙或說要確認一下；缺什麼（單位、期限、回電號碼）一次問一項，重要數字複誦。",
        "有人約時間，開心接住，說那天晚點回他，時間先不說死，也別說要確認或查一下。",
        "有人約時間，開心接住，照預約流程記下時間，複誦一次。",
        "親友來聊天，放鬆跟著聊就好，不用一直問、不用客服腔。",
        "像是打錯了，簡短跟他說就好，別報出你的名字或身分。",
    ]),
    (Lang::En, "[Note] ", [
        "Courier: leave it as usual; COD: amount, sender; they thank you: say bye.",
        "Bank checking a charge: note amount, shop, time; neither yes nor no, no block; you'll call.",
        "Notice: hear it all; ask what's missing (who, deadline, callback no.); repeat key figures.",
        "Scheduling: welcome it; you'll get back to them later; no time fixed yet.",
        "Scheduling: welcome it, book the date and time with the booking steps, repeat them.",
        "Family or friend chat: relax, talk as usual, no list of questions, no service tone.",
        "Wrong number: say so briefly; never give the owner's name or identity.",
    ]),
    (Lang::Ja, "[メモ] ", [
        "宅配：いつもの置き方で。代引きは金額と差出人、お礼で締められたら挨拶を返す。",
        "カード会社の確認らしい：金額・店名・時刻を聞き、本人利用とも違うとも覚えがないとも言わず、停止にも同意せず、後で公式窓口に確認すると伝える。",
        "大事な連絡：最後まで聞き、足りない点（部署・期限・折り返し番号）を一つずつ聞き、数字は復唱。",
        "日程の誘い：明るく受け、後で連絡すると伝え、日時はまだ決めない。",
        "日程の誘い：明るく受け、予約の手順で日時を記録し復唱。",
        "家族・友人との雑談：いつも通り気楽に。質問攻めや事務的な口調はなし。",
        "間違い電話らしい：短く伝え、持ち主の名前や身元は言わない。",
    ]),
    (Lang::Ko, "[메모] ", [
        "택배: 평소처럼 두게 하고, 착불이면 금액·보낸 사람 확인, 감사 인사엔 인사로 답하기.",
        "카드사 확인 같아요: 금액·가맹점·시간을 듣고, 본인 사용 여부도 기억 안 난다고도 말하지 말고 정지 동의 없이 나중에 직접 고객센터에 확인한다고 하기.",
        "중요한 연락: 끝까지 듣고 빠진 것(부서·기한·회신 번호)을 하나씩 묻고 숫자는 복창.",
        "일정 약속: 반갑게 받고 나중에 연락하겠다고 하며 시간은 아직 정하지 않기.",
        "일정 약속: 반갑게 받고 예약 절차대로 날짜와 시간을 기록하고 복창.",
        "가족·친구와 수다: 평소처럼 편하게, 캐묻거나 상담원 말투 없이.",
        "잘못 걸린 전화: 짧게 알려 주고 주인 이름이나 신원은 말하지 않기.",
    ]),
    (Lang::Es, "[Nota] ", [
        "Envío: como se suele; contra reembolso: importe, remitente; si agradece, despídete.",
        "Banco revisando un cargo: anota importe, comercio, hora; ni sí ni no; sin bloqueo; llamas tú.",
        "Aviso importante: escucha todo, pide lo que falte (área, plazo, número) y repite las cifras.",
        "Cita: acéptala con gusto; di que lo confirmas luego; sin fijar hora.",
        "Cita: acéptala con gusto, anota fecha y hora con la reserva y repítelas.",
        "Familia o amigos: charla relajada, como siempre, sin interrogar.",
        "Número equivocado: dilo en breve, sin el nombre ni datos del titular.",
    ]),
    (Lang::Fr, "[Note] ", [
        "Colis : comme d'habitude ; à payer : combien, de qui ; s'il remercie, au revoir.",
        "Banque, un achat : montant, commerce, heure ; ni oui ni non ; aucun blocage ; tu rappelles.",
        "Avis : écoute tout, demande ce qui manque (service, délai, numéro), répète les chiffres.",
        "Rendez-vous : accueille-le, tu confirmeras plus tard ; rien de fixé.",
        "Rendez-vous : accueille-le, note date et heure avec la réservation, répète-les.",
        "Proche ou ami : discute tranquille, comme d'habitude, sans interroger.",
        "Faux numéro : dis-le simplement, sans le nom ni l'identité du titulaire.",
    ]),
    (Lang::De, "[Note] ", [
        "Paket: wie üblich abgeben lassen; Nachnahme: Betrag, Absender; dankt er, verabschiede dich.",
        "Bank prüft Kauf: Betrag, Händler, Zeit; weder ja noch nein; nichts sperren; selbst anrufen.",
        "Mitteilung: zuhören, Fehlendes (Stelle, Frist, Rückrufnummer) erfragen, Zahlen wiederholen.",
        "Termin: gern annehmen, du meldest dich später dazu; noch nichts festlegen.",
        "Termin: gern annehmen, Datum und Uhrzeit per Buchung notieren und wiederholen.",
        "Familie oder Freunde: locker plaudern wie immer, nicht ausfragen.",
        "Falsch verbunden: kurz sagen; keinen Namen, keine Angaben des Inhabers.",
    ]),
    (Lang::Pt, "[Nota] ", [
        "Entrega: como de costume; a pagar: valor, remetente; se ele agradecer, despeça-se.",
        "Banco conferindo compra: valor, loja, hora; nem sim nem não; sem bloqueio; você liga depois.",
        "Aviso importante: ouça tudo, pergunte o que falta (setor, prazo, número), repita os números.",
        "Marcação: aceite com alegria, diga que confirma depois; sem fechar horário.",
        "Marcação: aceite com alegria, registre data e hora pela reserva e repita.",
        "Família ou amigos: conversa leve, como sempre, sem interrogar.",
        "Número errado: diga em breve, sem o nome nem dados do titular.",
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

/// The note mark the cues of `lang` start with (Traditional for both Chinese
/// scripts: the caller converts the whole note).
pub fn note_mark(lang: Lang) -> &'static str {
    let row_lang = if lang == Lang::ZhCn { Lang::ZhTw } else { lang };
    CUES.iter().find(|(l, _, _)| *l == row_lang).map_or("[Note] ", |(_, mark, _)| *mark)
}

/// Words of a courier or a delivery, in the languages of the cues.
const DELIVERY_WORDS: &[&str] = &[
    "快遞", "快递", "宅配", "包裹", "貨運", "货运", "送貨", "送货", "派件", "派送", "收件", "速遞", "速递", "外送", "外賣",
    "外卖", "順豐", "顺丰", "物流", "簽收", "签收", "送件", "お届け", "配達", "荷物", "置き配", "택배", "배송", "배달",
    "courier", "parcel", "package", "delivery", "deliver", "paquete", "envío", "envio", "repartidor", "mensajería",
    "colis", "livraison", "livreur", "paket", "lieferung", "zusteller", "sendung", "encomenda", "entregador", "pacote",
    "corriere", "consegna", "pacco", "курьер", "посылка", "доставка", "giao hàng", "bưu kiện", "người giao hàng",
    "corrier*", "consegn*", "spedizion*", "fattorin*", "pacch*",
    "kurir", "pengiriman", "ส่งของ", "พัสดุ", "ขนส่ง", "डिलीवरी", "पार्सल", "कूरियर", "مندوب التوصيل", "شحنة", "توصيل",
    // tr (a trailing * is a stem: the endings pile up, "kargonuz")
    "kargo*", "kurye*", "teslimat*", "gönderi*", "sipariş*",
];

/// A card or the bank it belongs to.
const CARD_WORDS: &[&str] = &[
    "卡", "銀行", "银行", "カード", "카드", "card", "bank", "tarjeta", "banco", "carte", "banque", "karte", "cartão",
    "cartao", "carta", "banca", "карта", "банк", "thẻ", "ngân hàng", "kartu", "บัตร", "ธนาคาร", "कार्ड", "बैंक",
    "بطاقة", "البنك",
    "kart*", "banka*",
    "carte", "banche", "banc*",
];

/// One purchase or charge on it.
const CHARGE_WORDS: &[&str] = &[
    "刷卡", "消費", "消费", "交易", "簽賬", "签账", "簽帳", "扣款", "一筆", "一笔", "ご利用", "決済", "取引", "결제", "거래",
    "승인", "purchase", "charge", "transaction", "compra", "cargo", "transacción", "transaccion", "movimiento", "achat",
    "paiement", "débit", "kauf", "zahlung", "umsatz", "transaktion", "abbuchung", "transação", "transacao", "cobrança",
    "cobranca", "acquisto", "addebito", "transazione", "покупка", "списание", "транзакция", "giao dịch", "thanh toán",
    "transaksi", "pembayaran", "รูดบัตร", "รายการใช้จ่าย", "काटने", "लेनदेन", "सौदा", "عملية شراء", "خصم", "معاملة",
    "işlem*", "harcama*", "alışveriş*", "ödeme*", "çekim*", "tahsilat*",
    "acquist*", "addebit*", "transazion*", "operazion*", "moviment*", "prelev*", "l'acquist*", "l'addebit*", "un'operazion*", "l'operazion*",
];

/// Asking whether it was the owner's.
const OWNER_CHECK_WORDS: &[&str] = &[
    "本人", "是不是您", "是不是你", "是您", "確認", "确认", "係咪你", "係唔係你", "ご本人", "본인", "확인", "verify",
    "confirm", "did you", "was it you", "yourself", "recognize", "recognise", "authorize", "confirmar", "verificar",
    "usted mismo", "reconoce", "confirmer", "vérifier", "verifier", "vous même", "bestätigen", "bestatigen",
    "sie selbst", "verifizieren", "você mesmo", "voce mesmo", "reconhece", "confermare", "lei stesso", "riconosce",
    "подтвердить", "вы сами", "xác nhận", "chính chủ", "konfirmasi", "anda sendiri", "ยืนยัน", "ด้วยตัวเอง",
    "पुष्टि", "खुद", "क्या आपने", "تأكيد", "بنفسك",
    "doğrula*", "onayla*", "siz mi", "sizin mi", "kendiniz*", "yaptınız mı", "tanıyor musunuz",
    "conferm*", "riconosc*", "lei stessa", "è stato lei", "e stato lei", "ha fatto lei", "ha effettuato lei", "autorizzat*",
];

/// What only a scammer asks a card holder for (or a step a real card desk never
/// takes): any of it and the call is no plain card check. Kept wide: a miss here
/// only leaves the decision to the call loop's rules.
const SCAM_ASK_WORDS: &[&str] = &[
    "密碼", "密码", "驗證碼", "验证码", "認證碼", "认证码", "簡訊碼", "卡號", "卡号", "安全碼", "安全码", "背面三碼", "身分證",
    "身份证", "atm", "提款機", "提款机", "轉帳", "转账", "匯款", "汇款", "分期", "解除", "監管", "监管", "安全帳戶", "安全账户",
    "加line", "加 line", "add line", "add me on line", "line id", "on line", "下載", "下载", "遠端", "远程", "螢幕", "屏幕",
    "禮物卡", "礼品卡", "點數卡", "保密", "警察",
    "檢察", "检察", "法院", "password", "pin", "code", "otp", "card number", "cvv", "social security", "transfer",
    "wire", "gift card", "remote", "screen", "download", "install", "app", "whatsapp", "telegram", "police",
    "暗証番号", "認証", "振込", "振り込", "비밀번호", "인증", "이체", "contraseña", "código", "codigo",
    "transferencia", "mot de passe", "virement", "passwort", "überweisung", "senha", "transferência", "pix",
    "codice", "bonifico", "polizia", "conto sicuro", "пароль", "перевод", "полиция", "безопасный счет", "безопасный счёт",
    "mật khẩu", "chuyển khoản", "công an", "kata sandi", "pinjaman", "polisi", "รหัสผ่าน", "โอนเงิน", "ตำรวจ",
    "पासवर्ड", "ओटीपी", "पुलिस", "ट्रांसफर", "كلمة المرور", "رمز التحقق", "تحويل", "الشرطة",
    "身份證", "身分证", "出生", "生日", "有效期", "帳號", "帐号", "帳戶", "账户", "連結", "链接", "網址", "网址", "點擊",
    "点击", "運費", "运费", "關稅", "关税", "click", "link", "date of birth", "birthday", "expiry", "expiration",
    "account number", "delivery fee", "customs", "生年月日", "口座", "リンク", "생년월일", "계좌", "링크",
    "繳費", "缴费", "付款", "付費", "付费", "繳款", "缴款", "證件", "证件", "送金", "お支払い", "支払って", "송금", "결제",
    "pay a", "pay the", "pay us", "to pay", "fee", "your id", "id card", "photo of your", "pagar", "payer", "zahlen",
    "pagamento",
    "codic*", "bonific*", "polizi*", "carabinier*", "conto sicuro", "conto protetto", "numero della carta", "scadenza", "data di nascita",
    "carta d'identità", "collegamento", "l'app", "un'app", "applicazione", "dogan*", "spese di spedizione", "versament*", "ricaric*",
    "pagare", "paghi",
    // Not "parola": Italian says it for any "word".
    "şifre*", "parolanız*", "parolanizi", "doğrulama kodu*", "güvenlik kodu*", "sms kodu*", "kart numara*", "son kullanma", "doğum tarihi", "tc kimlik*",
    "kimlik numara*", "iban*", "havale*", "eft", "güvenli hesap*", "polis*", "savcı*", "uzaktan", "uzak masaüstü", "bağlantı*", "tıkla*",
    "uygulama indir*", "uygulamayı indir*", "ödeme yapın", "ödeme yapmanız", "gümrük*", "kargo ücreti*", "teslimat ücreti*", "para gönder*",
    "para yatır*", "hesaba para",
];

/// Letters of words in Latin script (the rest are compared as they are).
fn latin_letter(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || c == '\''
        || ('\u{00C0}'..='\u{024F}').contains(&c)
        || ('\u{1E00}'..='\u{1EFF}').contains(&c)
}

/// The text lower case: as it is (for Chinese, Japanese and Korean words) and
/// as its Latin words only, one space apart and around ("online" is not "line",
/// "appliance" not "app").
struct Words {
    lower: String,
    latin: String,
}

impl Words {
    fn of(text: &str) -> Words {
        // Turkish İ lower-cases to "i" plus U+0307; the dot goes, so "İŞLEM" reads as "işlem".
        let lower = text.to_lowercase().replace('\u{307}', "");
        let mut latin = String::from(" ");
        for c in lower.chars() {
            if latin_letter(c) {
                latin.push(c);
            } else if !latin.ends_with(' ') {
                latin.push(' ');
            }
        }
        if !latin.ends_with(' ') {
            latin.push(' ');
        }
        Words { lower, latin }
    }

    /// A Latin word or phrase whole (plural and past endings allowed); others
    /// anywhere. A word ending in * is a stem: any word that starts with it.
    fn has(&self, w: &str) -> bool {
        if let Some(stem) = w.strip_suffix('*') {
            return self.latin.contains(&format!(" {stem}"));
        }
        if !w.is_ascii() || !w.chars().any(|c| c.is_ascii_alphabetic()) {
            return self.lower.contains(w);
        }
        let needle = format!(" {w}");
        self.latin.match_indices(&needle).any(|(i, m)| {
            let tail = self.latin[i + m.len()..].split(' ').next().unwrap_or("");
            matches!(tail, "" | "s" | "es" | "d" | "ed" | "ing" | "y")
        })
    }

    fn any(&self, words: &[&str]) -> bool {
        words.iter().any(|w| self.has(w))
    }
}

/// The caller asks whether one purchase on a card was the owner's, and asks for
/// nothing a scammer asks for: how a real card desk checks a charge
/// (「想跟您確認昨天晚上一筆線上刷卡的消費，是您本人刷的嗎」, "verify a purchase on
/// your card… did you make it yourself?").
pub fn card_check(caller: &str) -> bool {
    let w = Words::of(caller);
    w.any(CARD_WORDS) && w.any(CHARGE_WORDS) && w.any(OWNER_CHECK_WORDS) && !w.any(SCAM_ASK_WORDS)
}

/// Who sends an important notice, or what it is about: a clinic, a hospital, a
/// checkup, a school, an office's front desk.
const NOTICE_FROM_WORDS: &[&str] = &[
    "診所", "诊所", "醫院", "医院", "健檢", "健检", "體檢", "体检", "學校", "学校", "區公所", "区公所", "戶政", "户政",
    "事務所", "事务所", "櫃檯", "柜台", "前臺", "前台", "健康檢查", "健康检查", "クリニック", "病院", "健診", "検診",
    "健康診断", "人間ドック", "区役所", "市役所", "受付", "병원", "의원", "검진", "건강검진", "학교", "주민센터", "clinic",
    "hospital", "checkup", "check up", "dentist", "dental office", "doctor's office",
    "school", "city hall", "clínica", "consultorio", "escuela", "colegio", "clinique", "hôpital", "cabinet médical",
    "école", "mairie", "praxis", "klinik", "krankenhaus", "schule", "rathaus", "escola", "consultório", "prefeitura",
];

/// What it tells: a change, a deadline, results, a number or extension to call back.
const NOTICE_WHAT_WORDS: &[&str] = &[
    "改到", "改期", "改時間", "改时间", "調整", "调整", "變更", "变更", "延期", "期限", "截止", "回電", "回电", "分機",
    "分机", "複診", "复诊", "報告", "报告", "結果", "结果", "通知", "変更", "締切", "折り返し", "内線", "변경", "기한",
    "마감", "회신", "내선", "결과", "reschedule", "moved", "changed", "deadline", "call back", "call us back",
    "extension", "results", "reprogramado", "cambiado", "cambio", "plazo", "fecha límite", "extensión", "resultados",
    "déplacé", "décalé", "modifié", "date limite", "rappeler", "poste", "résultats", "verschoben", "geändert", "frist",
    "zurückrufen", "durchwahl", "ergebnis", "remarcado", "alterado", "prazo", "ramal", "resultado",
];

/// What an ordinary call is about, from the caller's own words alone, for when
/// the second look did not answer in time ([crate::stall_review]): a card desk
/// checking a purchase ([card_check]), a courier, or a clinic, school or office
/// telling of a change, a deadline or a number to call back (the acceptance run's
/// clinic notices went unrouted when every look timed out). None of them when
/// the caller asks for anything a scammer asks for. Anything else is none — the
/// model names the rest. Microseconds; string checks only.
pub fn local_guess(caller: &str) -> Scenario {
    if card_check(caller) {
        return Scenario::BankVerify;
    }
    // A courier who asks for a code, an account, a link or a fee is no plain courier.
    let w = Words::of(caller);
    if w.any(SCAM_ASK_WORDS) {
        return Scenario::None;
    }
    if w.any(DELIVERY_WORDS) {
        return Scenario::Delivery;
    }
    // A bank is no notice desk here (「銀行櫃檯…期限快到了，請回電」): left to the review.
    if w.any(NOTICE_FROM_WORDS) && w.any(NOTICE_WHAT_WORDS) && !w.any(BANK_WORDS) {
        return Scenario::ImportantNotice;
    }
    Scenario::None
}

/// A bank or card company calling.
const BANK_WORDS: &[&str] = &[
    "銀行", "银行", "信用卡", "卡片中心", "カード会社", "은행", "카드사", "bank", "credit card", "banco", "banque", "sparkasse", "banca",
    "carta di credito",
];

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
    /// Japanese call pulled the AI's next line into Chinese. `other`: the caller's
    /// last words are in a language without a setting (Thai, Italian…): the cue
    /// in English, marked to be said in theirs.
    pub fn take_pending(&mut self, stalling: bool, spoken: Option<Lang>, other: bool) -> Option<(Scenario, String)> {
        let s = self.pending.take()?;
        if stalling || self.sent.contains(&s) {
            return None;
        }
        self.sent.push(s);
        if other {
            let en = cue(s, Lang::En, self.region, self.booking)?;
            let text = en.strip_prefix(note_mark(Lang::En)).unwrap_or(&en);
            return Some((s, format!("{OTHER_LANGUAGE_MARK}{text}")));
        }
        let lang = match spoken {
            Some(l) if !(l.is_zh() && self.lang.is_zh()) => l,
            _ => self.lang,
        };
        Some((s, cue(s, lang, self.region, self.booking)?))
    }
}

/// The mark of a cue for a caller in a language without a setting.
const OTHER_LANGUAGE_MARK: &str = "[Note, in caller's language] ";

#[cfg(test)]
mod tests {
    use super::*;

    const REGIONS: [Region; 22] = [
        Region::Tw, Region::HkMo, Region::Cn, Region::MySg, Region::Us, Region::Ca, Region::GbIe,
        Region::AuNz, Region::In, Region::Jp, Region::Kr, Region::EsEu, Region::EsAm, Region::EsRp,
        Region::Fr, Region::Be, Region::De, Region::At, Region::Ch, Region::Br, Region::Pt, Region::Tr,
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

    /// The schedule, notice and bank cues no longer hand the AI a "let me
    /// check" line (「我先確認那天的安排」 came back as 「我確認一下」).
    #[test]
    fn no_cue_puts_a_checking_line_in_the_ais_mouth() {
        let sched = cue(Scenario::ScheduleInvite, Lang::ZhTw, None, false).unwrap();
        assert!(!sched.contains("我先確認") && sched.contains("晚點回他") && sched.contains("別說要確認"), "{sched}");
        let notice = cue(Scenario::ImportantNotice, Lang::ZhTw, None, false).unwrap();
        assert!(notice.contains("別推說在忙") && notice.contains("說要確認一下"), "{notice}");
        let bank = cue(Scenario::BankVerify, Lang::ZhTw, None, false).unwrap();
        assert!(bank.contains("要再想想"), "{bank}");
        let en = cue(Scenario::ScheduleInvite, Lang::En, None, false).unwrap();
        assert!(!en.contains("confirm") && en.contains("get back to them"), "{en}");
    }

    #[test]
    fn a_card_desk_checking_a_purchase_is_seen_without_the_model() {
        for t in [
            "喂您好，這邊是大愛銀行信用卡風險管控中心。我們看到您的卡片昨天晚上11點左右在網路電器商店有一筆3280元的消費。想跟您確認是您本人刷的嗎?",
            "Hello, this is the credit card risk-control center at First National Bank. I'm calling to verify a purchase on your account for about a hundred dollars at an online appliance store.",
            "カード会社です。昨夜のオンラインストアでのご利用について、ご本人様かどうか確認させてください。",
            "Hola, le llamamos del banco por un cargo en su tarjeta de anoche, ¿podría confirmar si fue usted?",
        ] {
            assert!(card_check(t), "{t}");
            assert_eq!(local_guess(t), Scenario::BankVerify, "{t}");
        }
        // What only a scammer asks for: no plain card check, whatever the rest.
        for t in [
            "這邊是銀行信用卡中心，您昨天一筆消費被設成分期，要請您到ATM解除，跟您確認一下。",
            "We need to verify the purchase on your card, please read me the code we just texted you.",
            "銀行通知您卡片有一筆異常消費，請您加LINE跟專員確認。",
            "To confirm the charge on your card, please install our app so we can see your screen.",
        ] {
            assert!(!card_check(t), "{t}");
        }
        // A bank that only says who it is, or sells something, is no card check.
        assert!(!card_check("喂您好，這邊是大愛銀行信用卡風險管控中心。"));
        assert!(!card_check("您好，我們銀行信用卡現在有新的卡友專案，額度可以幫您提高"));
    }

    #[test]
    fn a_courier_is_seen_without_the_model() {
        for t in [
            "喂，你好，請問是不是王先生啊?我是順豐派件的。",
            "喂王生你好呀，我係送速遞嘅，我想確認聽日下晝大約咩時間有人喺屋企收件呀？",
            "G'day, Mr. Wang. It's the courier here. Just ringing to check if someone'll be home tomorrow arvo to sign for a parcel?",
            "ヤマト運輸です。お荷物のお届けについてお電話しました。",
            "Bonjour, c'est le livreur, j'ai un colis pour vous.",
        ] {
            assert_eq!(local_guess(t), Scenario::Delivery, "{t}");
        }
        for t in ["王先生，禮拜六有空嗎？一起吃飯", "I'm calling about the meeting tomorrow at three", "我在線上看到你的貼文"] {
            assert_eq!(local_guess(t), Scenario::None, "{t}");
        }
        // Whole words in Latin script: "online" is no LINE, "appliance" no app.
        assert!(!Words::of("an online appliance store").any(&["card", "line", "app"]));
    }

    #[test]
    fn the_bank_and_wrong_number_cues_hold_the_lines() {
        let bank = cue(Scenario::BankVerify, Lang::ZhTw, None, false).unwrap();
        for must in ["金額、商店、時間", "不說是不是你刷的", "沒印象", "不答應停卡或卡控", "客服確認"] {
            assert!(bank.contains(must), "{must}");
        }
        let en = cue(Scenario::BankVerify, Lang::En, None, false).unwrap();
        assert!(en.contains("neither yes nor no") && en.contains("no block"), "{en}");
        let wrong = cue(Scenario::WrongNumber, Lang::ZhTw, None, false).unwrap();
        assert!(wrong.contains("別報出你的名字"), "{wrong}");
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
        assert!(book.contains("預約") && no_book.contains("不說死"));
        // Each cue names the kind of call first, as a colleague would.
        let delivery = cue(Scenario::Delivery, Lang::ZhTw, None, false).unwrap();
        assert!(delivery.contains("是快遞") && delivery.contains("照這裡的習慣"), "{delivery}");
        assert!(bank.starts_with("【系統小抄·不是對方說的】像銀行在核對刷卡"), "{bank}");
    }

    #[test]
    fn one_cue_per_scenario_and_none_while_stalling() {
        let mut r = Router::new(Lang::ZhTw, Some(Region::Tw), false);
        assert_eq!(r.on_review(Scenario::None, Verdict::Normal, Confidence::High, false), RouteAction::None);
        assert_eq!(r.on_review(Scenario::Delivery, Verdict::ScamOrSales, Confidence::High, false), RouteAction::NotNormal);
        assert_eq!(r.on_review(Scenario::Delivery, Verdict::Normal, Confidence::Low, false), RouteAction::LowConfidence);
        assert!(r.take_pending(false, None, false).is_none());
        assert_eq!(r.on_review(Scenario::Delivery, Verdict::Normal, Confidence::High, false), RouteAction::Queued);
        let (s, text) = r.take_pending(false, None, false).unwrap();
        assert_eq!(s, Scenario::Delivery);
        assert!(text.contains("快遞"));
        assert!(r.take_pending(false, None, false).is_none(), "once");
        assert_eq!(r.on_review(Scenario::Delivery, Verdict::Normal, Confidence::High, false), RouteAction::AlreadySent);
        // A card desk's first words (too early) may already be the bank scenario.
        assert_eq!(r.on_review(Scenario::BankVerify, Verdict::TooEarly, Confidence::Medium, false), RouteAction::Queued);
        // Stalling starts before the pause: the ordinary cue stays out.
        assert!(r.take_pending(true, None, false).is_none());
        assert_eq!(r.on_review(Scenario::BankVerify, Verdict::Normal, Confidence::High, true), RouteAction::Stalling);
        assert!(r.take_pending(false, None, false).is_none());
        assert_eq!(r.on_review(Scenario::CasualChat, Verdict::Normal, Confidence::High, false), RouteAction::Queued);
        r.drop_pending();
        assert!(r.take_pending(false, None, false).is_none());
    }

    #[test]
    fn the_cue_is_in_the_language_the_caller_speaks() {
        // A Chinese-set call where the caller speaks Japanese: the cue is Japanese.
        let mut r = Router::new(Lang::ZhTw, Some(Region::Jp), false);
        r.on_review(Scenario::Delivery, Verdict::Normal, Confidence::High, false);
        let (_, text) = r.take_pending(false, Some(Lang::Ja), false).unwrap();
        assert!(text.starts_with("[メモ]") && text.contains("宅配"), "{text}");
        // Chinese of the other script keeps the call's own.
        let mut r = Router::new(Lang::ZhTw, Some(Region::Tw), false);
        r.on_review(Scenario::WrongNumber, Verdict::Normal, Confidence::High, false);
        let (_, text) = r.take_pending(false, Some(Lang::ZhCn), false).unwrap();
        assert!(text.contains("打錯"), "{text}");
        // An English caller on the same call: English.
        let mut r = Router::new(Lang::ZhTw, Some(Region::Tw), false);
        r.on_review(Scenario::Delivery, Verdict::Normal, Confidence::High, false);
        assert!(r.take_pending(false, Some(Lang::En), false).unwrap().1.starts_with("[Note]"));
        // A Thai caller on a Taiwan SIM's Chinese call (their earlier Chinese still
        // in the transcript): the cue in English, said in Thai.
        let mut r = Router::new(Lang::ZhTw, Some(Region::Tw), false);
        r.on_review(Scenario::Delivery, Verdict::Normal, Confidence::High, false);
        let (_, text) = r.take_pending(false, Some(Lang::ZhTw), true).unwrap();
        assert!(text.starts_with(OTHER_LANGUAGE_MARK) && !text.contains("系統小抄") && !text.contains("[Note] "), "{text}");
        assert!(text.chars().count() <= MAX_CUE_CHARS + OTHER_LANGUAGE_MARK.len(), "{text}");
    }
}


#[cfg(test)]
mod no_answer_fallback_scams {
    use super::{local_guess, Scenario};

    #[test]
    fn a_courier_or_card_desk_asking_for_scam_things_is_not_ordinary() {
        for t in [
            "我是順豐，您有一個包裹，請加LINE提供地址並付運費",
            "我是快遞，請提供您的身份證字號和銀行帳號",
            "快遞包裹有問題，請點擊連結",
            "Courier here, your parcel is held, click the link and pay the delivery fee",
            "銀行信用卡中心，一筆消費確認是不是您本人，請提供您的身份證字號",
            "銀行信用卡中心，一筆消費確認是不是您本人，請提供您的出生年月日和卡片有效期限",
        ] {
            assert_eq!(local_guess(t), Scenario::None, "{t}");
        }
        assert_eq!(local_guess("您好，黑貓宅急便，您的包裹明天下午送到，家裡有人嗎"), Scenario::Delivery);
    }

    /// A clinic, school or office with a change, a deadline or a callback number
    /// is an important notice without the model; the same desk asking for an
    /// account, an ID or a link is not.
    #[test]
    fn a_clinic_or_office_notice_is_known_without_the_model() {
        for t in [
            "王先生您好,我是健檢中心的櫃檯,您原本預約的健檢時間有做調整。幫您改到下週三上午10點。可以嗎?",
            "再請您撥打分機一二回電確認，這裡是仁愛診所",
            "まるまるクリニックの受付です。健康診断の日程が変更になりまして来週水曜日の午前10時となりました。",
            "先日の健康診断の予約なんですが、来週の水曜日の午前10時変更になりました。",
            "Hi, this is the clinic calling. Your appointment has been rescheduled to next Wednesday, please call us back at extension 12.",
            "안녕하세요, 서울병원입니다. 검진 일정이 변경되었습니다.",
            "Bonjour, ici la clinique, votre rendez-vous est déplacé à mercredi.",
        ] {
            assert_eq!(local_guess(t), Scenario::ImportantNotice, "{t}");
        }
        for t in [
            "這裡是醫院，你的資料外洩了，請提供帳號和密碼",
            "Hi, this is the clinic, your results are ready, click the link to pay the fee",
            "我在醫院附近，等一下一起吃飯",
            "I'm calling about the meeting tomorrow at three",
        ] {
            assert_eq!(local_guess(t), Scenario::None, "{t}");
        }
    }
}

#[cfg(test)]
mod notice_fallback_edges {
    use super::{local_guess, Scenario};

    #[test]
    fn a_bank_desk_or_a_payment_ask_is_no_notice() {
        for t in [
            "這裡是銀行櫃檯，您的貸款期限快到了，請回電",
            "銀行の受付です。お手続きの期限が変更になりました",
            "This is the clinic, your results are ready, please pay a 200 dollar fee by today",
            "This is the clinic, please send us a photo of your ID",
            "診所通知您檢查結果出來了，需要先繳費五千元",
            "クリニックの受付です。先にお金を送金してください",
        ] {
            assert_ne!(local_guess(t), Scenario::ImportantNotice, "{t}");
        }
    }

    #[test]
    fn line_wording_and_extended_languages_classify_cleanly() {
        assert_eq!(local_guess("Courier here, delivery on the line waiting downstairs"), Scenario::Delivery);
        assert_eq!(local_guess("Delivery here, please add me on line for the address"), Scenario::None);
        for (sample, want) in [
            ("Corriere espresso, ho una consegna e un pacco per lei", Scenario::Delivery),
            ("Xin chào, tôi là người giao hàng có bưu kiện gửi anh", Scenario::Delivery),
            ("Halo, saya kurir pengiriman paket", Scenario::Delivery),
            ("มีพัสดุมาส่งครับ", Scenario::Delivery),
            ("Здравствуйте, курьер доставки, у меня посылка для вас", Scenario::Delivery),
            ("नमस्ते, मैं डिलीवरी और पार्सल लेकर आया हूँ", Scenario::Delivery),
            ("مرحبا، معك مندوب التوصيل وعندي شحنة لك", Scenario::Delivery),
            ("La sua banca per confermare un acquisto con carta, lo riconosce?", Scenario::BankVerify),
            ("Merhaba, kurye geldi, kargonuz için kapınızdayım", Scenario::Delivery),
            ("Merhaba, bankanızdan arıyorum. Kartınızla dün yapılan alışveriş işlemini doğrulamak istiyorum, siz mi yaptınız?", Scenario::BankVerify),
            ("Merhaba, bankanızdan arıyorum. Kartınızla yapılan işlemi doğrulamak için SMS kodunu söyler misiniz?", Scenario::None),
            ("Merhaba, kargo şirketinden arıyorum, kargo ücretini ödemeniz gerekiyor", Scenario::None),
        ] {
            assert_eq!(local_guess(sample), want, "{sample}");
        }
    }

    #[test]
    fn turkish_stems_take_endings_and_the_capital_dotted_i() {
        // A stem is any word that starts with it; a whole Latin word is not.
        use super::Words;
        assert!(Words::of("Kargonuz geldi").has("kargo*"));
        assert!(!Words::of("Bu bir karga").has("kargo*"));
        // İ lower-cases to i plus a combining dot, which would split the word.
        assert!(Words::of("İŞLEMİ onaylayın").has("işlem*"));
        assert_eq!(local_guess("İYİ GÜNLER, KURYE GELDİ"), Scenario::Delivery);
    }

    #[test]
    fn italian_words_take_their_endings_and_the_card_desk_and_courier_are_told_from_scammers() {
        use super::Words;
        assert!(Words::of("Ho consegnato i pacchi ieri").has("consegn*"));
        assert!(Words::of("Ci sono due pacchi per lei").has("pacch*"));
        assert!(!Words::of("Una scatola grande").has("pacch*"));
        for (sample, want) in [
            ("Buongiorno, sono il corriere, ho una spedizione per lei, la lascio in portineria?", Scenario::Delivery),
            ("Buonasera, sono il fattorino, ho dei pacchi per lei", Scenario::Delivery),
            ("Buongiorno, sono della sua banca, vorrei verificare un addebito di ieri sulla sua carta, è stato lei?", Scenario::BankVerify),
            ("Dalla banca: abbiamo visto un acquisto con la carta, lo riconosce?", Scenario::BankVerify),
            ("Buongiorno, la chiamo dalla banca per un'operazione sulla sua carta, la conferma lei stessa?", Scenario::BankVerify),
            ("Sono il corriere, per consegnare il pacco mi dica il codice di verifica", Scenario::None),
            ("Sono il corriere, deve pagare le spese di spedizione con una ricarica", Scenario::None),
            ("Buongiorno, la chiamo dalla banca, ha fatto lei questo acquisto con la carta? Mi dica la data di nascita", Scenario::None),
            ("Buongiorno, ho un appuntamento dal dentista domani", Scenario::None),
        ] {
            assert_eq!(local_guess(sample), want, "{sample}");
        }
        // Other languages as before.
        for (sample, want) in [
            ("Courier here, delivery on the line waiting downstairs", Scenario::Delivery),
            ("Hola, soy el repartidor, tengo un paquete para usted", Scenario::Delivery),
            ("Bonjour, livreur, j'ai un colis pour vous", Scenario::Delivery),
            ("お荷物のお届けです", Scenario::Delivery),
            ("Hi, this is your bank verifying a purchase on your card, did you make it yourself?", Scenario::BankVerify),
            ("Courier here, please send the gift card code", Scenario::None),
        ] {
            assert_eq!(local_guess(sample), want, "{sample}");
        }
    }
}
