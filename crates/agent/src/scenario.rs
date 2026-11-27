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
//! The caller's own words also name the scenarios plain words show
//! ([local_guess]: a courier, a card desk checking a purchase, a clinic, school
//! or office with a change, a deadline or a number to call back) after each of
//! their turns, and when that look gets no answer in time. A caller whose call
//! is never looked at for sales / scam (a saved contact, the owner's own matter)
//! still gets its cue: one look a call only for what it is about.
//!
//! An ordinary call is harder to take for sales / scam ([ordinary]): keywords
//! alone do not back the look's "scam" on a courier, a notice, setting a time
//! or a card desk checking one purchase, as long as the caller asks for nothing
//! a scammer asks for and threatens nothing a scammer threatens. Pure in-memory work.

use crate::country_profile::Region;
use crate::locale::Lang;
use crate::prompt_book::Text;
use crate::stall_review::{Confidence, Verdict};

// The words are in the prompt book (data/prompts/<language>.json); these are their handles.
crate::prompt_book::prompt_texts! {
    ZH_CUE_MARK = "zh-TW/scenario_cue_mark";
    ZH_CUE_DELIVERY = "zh-TW/scenario_cue_delivery";
    ZH_CUE_BANK_VERIFY = "zh-TW/scenario_cue_bank_verify";
    ZH_CUE_IMPORTANT_NOTICE = "zh-TW/scenario_cue_important_notice";
    ZH_CUE_SCHEDULE_INVITE = "zh-TW/scenario_cue_schedule_invite";
    ZH_CUE_SCHEDULE_INVITE_BOOKING = "zh-TW/scenario_cue_schedule_invite_booking";
    ZH_CUE_CASUAL_CHAT = "zh-TW/scenario_cue_casual_chat";
    ZH_CUE_WRONG_NUMBER = "zh-TW/scenario_cue_wrong_number";
    EN_CUE_MARK = "en/scenario_cue_mark";
    EN_CUE_DELIVERY = "en/scenario_cue_delivery";
    EN_CUE_BANK_VERIFY = "en/scenario_cue_bank_verify";
    EN_CUE_IMPORTANT_NOTICE = "en/scenario_cue_important_notice";
    EN_CUE_SCHEDULE_INVITE = "en/scenario_cue_schedule_invite";
    EN_CUE_SCHEDULE_INVITE_BOOKING = "en/scenario_cue_schedule_invite_booking";
    EN_CUE_CASUAL_CHAT = "en/scenario_cue_casual_chat";
    EN_CUE_WRONG_NUMBER = "en/scenario_cue_wrong_number";
    /// The mark of a cue for a caller in a language without a setting.
    EN_OTHER_LANGUAGE_MARK = "en/scenario_other_language_mark";
    JA_CUE_MARK = "ja/scenario_cue_mark";
    JA_CUE_DELIVERY = "ja/scenario_cue_delivery";
    JA_CUE_BANK_VERIFY = "ja/scenario_cue_bank_verify";
    JA_CUE_IMPORTANT_NOTICE = "ja/scenario_cue_important_notice";
    JA_CUE_SCHEDULE_INVITE = "ja/scenario_cue_schedule_invite";
    JA_CUE_SCHEDULE_INVITE_BOOKING = "ja/scenario_cue_schedule_invite_booking";
    JA_CUE_CASUAL_CHAT = "ja/scenario_cue_casual_chat";
    JA_CUE_WRONG_NUMBER = "ja/scenario_cue_wrong_number";
    KO_CUE_MARK = "ko/scenario_cue_mark";
    KO_CUE_DELIVERY = "ko/scenario_cue_delivery";
    KO_CUE_BANK_VERIFY = "ko/scenario_cue_bank_verify";
    KO_CUE_IMPORTANT_NOTICE = "ko/scenario_cue_important_notice";
    KO_CUE_SCHEDULE_INVITE = "ko/scenario_cue_schedule_invite";
    KO_CUE_SCHEDULE_INVITE_BOOKING = "ko/scenario_cue_schedule_invite_booking";
    KO_CUE_CASUAL_CHAT = "ko/scenario_cue_casual_chat";
    KO_CUE_WRONG_NUMBER = "ko/scenario_cue_wrong_number";
    ES_CUE_MARK = "es/scenario_cue_mark";
    ES_CUE_DELIVERY = "es/scenario_cue_delivery";
    ES_CUE_BANK_VERIFY = "es/scenario_cue_bank_verify";
    ES_CUE_IMPORTANT_NOTICE = "es/scenario_cue_important_notice";
    ES_CUE_SCHEDULE_INVITE = "es/scenario_cue_schedule_invite";
    ES_CUE_SCHEDULE_INVITE_BOOKING = "es/scenario_cue_schedule_invite_booking";
    ES_CUE_CASUAL_CHAT = "es/scenario_cue_casual_chat";
    ES_CUE_WRONG_NUMBER = "es/scenario_cue_wrong_number";
    FR_CUE_MARK = "fr/scenario_cue_mark";
    FR_CUE_DELIVERY = "fr/scenario_cue_delivery";
    FR_CUE_BANK_VERIFY = "fr/scenario_cue_bank_verify";
    FR_CUE_IMPORTANT_NOTICE = "fr/scenario_cue_important_notice";
    FR_CUE_SCHEDULE_INVITE = "fr/scenario_cue_schedule_invite";
    FR_CUE_SCHEDULE_INVITE_BOOKING = "fr/scenario_cue_schedule_invite_booking";
    FR_CUE_CASUAL_CHAT = "fr/scenario_cue_casual_chat";
    FR_CUE_WRONG_NUMBER = "fr/scenario_cue_wrong_number";
    DE_CUE_MARK = "de/scenario_cue_mark";
    DE_CUE_DELIVERY = "de/scenario_cue_delivery";
    DE_CUE_BANK_VERIFY = "de/scenario_cue_bank_verify";
    DE_CUE_IMPORTANT_NOTICE = "de/scenario_cue_important_notice";
    DE_CUE_SCHEDULE_INVITE = "de/scenario_cue_schedule_invite";
    DE_CUE_SCHEDULE_INVITE_BOOKING = "de/scenario_cue_schedule_invite_booking";
    DE_CUE_CASUAL_CHAT = "de/scenario_cue_casual_chat";
    DE_CUE_WRONG_NUMBER = "de/scenario_cue_wrong_number";
    PT_CUE_MARK = "pt/scenario_cue_mark";
    PT_CUE_DELIVERY = "pt/scenario_cue_delivery";
    PT_CUE_BANK_VERIFY = "pt/scenario_cue_bank_verify";
    PT_CUE_IMPORTANT_NOTICE = "pt/scenario_cue_important_notice";
    PT_CUE_SCHEDULE_INVITE = "pt/scenario_cue_schedule_invite";
    PT_CUE_SCHEDULE_INVITE_BOOKING = "pt/scenario_cue_schedule_invite_booking";
    PT_CUE_CASUAL_CHAT = "pt/scenario_cue_casual_chat";
    PT_CUE_WRONG_NUMBER = "pt/scenario_cue_wrong_number";
}

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
static CUES: [(Lang, &Text, [&Text; 7]); 8] = [
    (Lang::ZhTw, &ZH_CUE_MARK, [
        &ZH_CUE_DELIVERY,
        &ZH_CUE_BANK_VERIFY,
        &ZH_CUE_IMPORTANT_NOTICE,
        &ZH_CUE_SCHEDULE_INVITE,
        &ZH_CUE_SCHEDULE_INVITE_BOOKING,
        &ZH_CUE_CASUAL_CHAT,
        &ZH_CUE_WRONG_NUMBER,
    ]),
    (Lang::En, &EN_CUE_MARK, [
        &EN_CUE_DELIVERY,
        &EN_CUE_BANK_VERIFY,
        &EN_CUE_IMPORTANT_NOTICE,
        &EN_CUE_SCHEDULE_INVITE,
        &EN_CUE_SCHEDULE_INVITE_BOOKING,
        &EN_CUE_CASUAL_CHAT,
        &EN_CUE_WRONG_NUMBER,
    ]),
    (Lang::Ja, &JA_CUE_MARK, [
        &JA_CUE_DELIVERY,
        &JA_CUE_BANK_VERIFY,
        &JA_CUE_IMPORTANT_NOTICE,
        &JA_CUE_SCHEDULE_INVITE,
        &JA_CUE_SCHEDULE_INVITE_BOOKING,
        &JA_CUE_CASUAL_CHAT,
        &JA_CUE_WRONG_NUMBER,
    ]),
    (Lang::Ko, &KO_CUE_MARK, [
        &KO_CUE_DELIVERY,
        &KO_CUE_BANK_VERIFY,
        &KO_CUE_IMPORTANT_NOTICE,
        &KO_CUE_SCHEDULE_INVITE,
        &KO_CUE_SCHEDULE_INVITE_BOOKING,
        &KO_CUE_CASUAL_CHAT,
        &KO_CUE_WRONG_NUMBER,
    ]),
    (Lang::Es, &ES_CUE_MARK, [
        &ES_CUE_DELIVERY,
        &ES_CUE_BANK_VERIFY,
        &ES_CUE_IMPORTANT_NOTICE,
        &ES_CUE_SCHEDULE_INVITE,
        &ES_CUE_SCHEDULE_INVITE_BOOKING,
        &ES_CUE_CASUAL_CHAT,
        &ES_CUE_WRONG_NUMBER,
    ]),
    (Lang::Fr, &FR_CUE_MARK, [
        &FR_CUE_DELIVERY,
        &FR_CUE_BANK_VERIFY,
        &FR_CUE_IMPORTANT_NOTICE,
        &FR_CUE_SCHEDULE_INVITE,
        &FR_CUE_SCHEDULE_INVITE_BOOKING,
        &FR_CUE_CASUAL_CHAT,
        &FR_CUE_WRONG_NUMBER,
    ]),
    (Lang::De, &DE_CUE_MARK, [
        &DE_CUE_DELIVERY,
        &DE_CUE_BANK_VERIFY,
        &DE_CUE_IMPORTANT_NOTICE,
        &DE_CUE_SCHEDULE_INVITE,
        &DE_CUE_SCHEDULE_INVITE_BOOKING,
        &DE_CUE_CASUAL_CHAT,
        &DE_CUE_WRONG_NUMBER,
    ]),
    (Lang::Pt, &PT_CUE_MARK, [
        &PT_CUE_DELIVERY,
        &PT_CUE_BANK_VERIFY,
        &PT_CUE_IMPORTANT_NOTICE,
        &PT_CUE_SCHEDULE_INVITE,
        &PT_CUE_SCHEDULE_INVITE_BOOKING,
        &PT_CUE_CASUAL_CHAT,
        &PT_CUE_WRONG_NUMBER,
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
    let row_lang = if lang == Lang::ZhCn { Lang::ZhTw } else { lang.base() };
    let (_, mark, texts) = CUES.iter().find(|(l, _, _)| *l == row_lang)?;
    let text = format!("{}{}", mark.get(), texts[i].get());
    Some(if lang == Lang::ZhCn { crate::zh_convert::to_cn(&text) } else { text })
}

/// The note mark the cues of `lang` start with (Traditional for both Chinese
/// scripts: the caller converts the whole note).
pub fn note_mark(lang: Lang) -> &'static str {
    let row_lang = if lang == Lang::ZhCn { Lang::ZhTw } else { lang.base() };
    CUES.iter().find(|(l, _, _)| *l == row_lang).map_or("[Note] ", |(_, mark, _)| mark.get())
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

/// What the scams that pose as an ordinary desk threaten or tell: a line or an
/// account cut off, frozen or seized, an arrest, a stolen identity, money
/// laundering, a debt collector, customs holding a parcel, a stranger asking to
/// borrow money, and the ATM or a link in the words [SCAM_ASK_WORDS] lacks. Any
/// of it and the call is no plain courier, notice or card check.
const THREAT_WORDS: &[&str] = &[
    "cajero automático", "cajero automatico", "caixa eletrônico", "caixa eletronico", "distributeur automatique",
    "distributeur de billets", "geldautomat*", "bancomat", "atm'*",
    "enlace", "lien", "liên kết", "tautan", "ลิงก์",
    "停話", "停话", "停機", "停机", "斷話", "断话", "凍結", "冻结", "查封", "扣押", "逮捕", "拘提", "通緝", "通缉", "盜用",
    "盗用", "盜刷", "盗刷", "冒用", "洗錢", "洗钱", "涉案", "涉嫌", "刑案", "討債", "讨债", "催收", "借錢", "借钱", "借我錢",
    "借我钱", "借點錢", "借点钱", "海關", "海关",
    "差し押さえ", "差押", "不正利用", "マネーロンダリング", "利用停止", "お金を貸して", "税関",
    "동결", "압류", "체포", "구속", "도용", "자금세탁", "사용 정지", "정지 처리", "돈 좀 빌려", "돈 빌려", "세관",
    "frozen account", "account frozen", "account will be frozen", "freeze your account", "suspend your account",
    "account suspended", "account will be suspended", "cut off", "arrest", "arrest warrant", "seize", "lawsuit",
    "identity theft", "stolen identity", "money laundering", "debt collector", "collection agency", "lend me",
    "borrow money",
    "cuenta bloqueada", "cuenta congelada", "bloquear su cuenta", "embargo", "embargar", "orden de arresto",
    "orden de captura", "lavado de dinero", "blanqueo", "suplantación de identidad", "préstame", "agencia de cobro",
    "aduana", "aduanas",
    "compte bloqué", "compte gelé", "bloquer votre compte", "saisie", "mandat d'arrêt", "arrestation", "blanchiment",
    "usurpation d'identité", "prête-moi", "recouvrement", "huissier", "douane",
    "gesperrt", "eingefroren", "pfändung", "haftbefehl", "festnahme", "geldwäsche", "identitätsdiebstahl", "inkasso",
    "leih mir", "abgeschaltet", "zoll",
    "conta bloqueada", "conta congelada", "bloquear sua conta", "penhora", "mandado de prisão", "lavagem de dinheiro",
    "me empresta", "alfândega", "alfandega",
    "conto bloccato", "conto congelato", "bloccare il conto", "pignoramento", "mandato di arresto", "riciclaggio",
    "furto d'identità", "prestami", "recupero crediti",
    // Not "bloke" alone: English says it for any man.
    "bloke ed*", "bloke oldu*", "dondurul*", "haciz*", "tutuklama*", "gözaltı*", "kara para*", "kimlik hırsızlığı", "borç ver*",
    "khóa tài khoản", "phong tỏa", "đóng băng", "bắt giữ", "lệnh bắt", "rửa tiền", "cho mượn tiền", "cắt điện",
    "hải quan",
    "diblokir", "dibekukan", "pemblokiran", "penyitaan", "ditangkap", "pencucian uang", "pinjam uang", "pinjamkan",
    "penagih utang", "bea cukai",
];

/// Paying the courier at the door: what a real courier asks for ("付款" alone
/// is a scammer's ask).
const COD_WORDS: &[&str] = &[
    "貨到付款", "货到付款", "代金引換", "代引き", "착불", "cash on delivery", "contra reembolso", "pago contra entrega",
    "paiement à la livraison", "nachnahme", "pagamento na entrega", "contrassegno", "thanh toán khi nhận hàng",
    "เก็บเงินปลายทาง", "bayar di tempat", "kapıda ödeme",
];

/// The text's words without paying the courier at the door ([COD_WORDS]), for
/// the scammer's asks.
fn ask_words(text: &str) -> Words {
    let mut lower = text.to_lowercase();
    for c in COD_WORDS {
        lower = lower.replace(c, " ");
    }
    Words::of(&lower)
}

/// The caller asks for anything a scammer asks for, or threatens or tells what
/// a scammer posing as an ordinary desk does. Paying a courier at the door is
/// no such ask.
pub fn scam_ask(caller: &str) -> bool {
    let w = ask_words(caller);
    w.any(SCAM_ASK_WORDS) || w.any(THREAT_WORDS)
}

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
    w.any(CARD_WORDS) && w.any(CHARGE_WORDS) && w.any(OWNER_CHECK_WORDS) && !w.any(SCAM_ASK_WORDS) && !w.any(THREAT_WORDS)
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

/// What an ordinary call is about, from the caller's own words alone, after each
/// of their turns and for when the second look did not answer in time
/// ([crate::stall_review]): a card desk checking a purchase ([card_check]), a
/// courier, or a clinic, school or office telling of a change, a deadline or a
/// number to call back (the acceptance run's clinic notices went unrouted when
/// every look timed out). None of them when the caller asks for anything a
/// scammer asks for or threatens what one threatens ([scam_ask]). Anything else
/// is none — the model names the rest. Microseconds; string checks only.
pub fn local_guess(caller: &str) -> Scenario {
    if card_check(caller) {
        return Scenario::BankVerify;
    }
    // A courier who asks for a code, an account, a link or a fee is no plain courier.
    if scam_ask(caller) {
        return Scenario::None;
    }
    let w = Words::of(caller);
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

/// Kinds of ordinary call that make it harder to take for sales / scam. A
/// family chat and a wrong number do not: "guess who this is" and "sorry, wrong
/// number" open scams too.
fn eases_the_check(s: Scenario) -> bool {
    matches!(s, Scenario::Delivery | Scenario::BankVerify | Scenario::ImportantNotice | Scenario::ScheduleInvite)
}

/// What the call is about, when it is a kind that makes it harder to take for
/// sales / scam ([eases_the_check]): the caller's words (`caller`: all of their
/// side so far) as they read now ([local_guess]), else what an earlier look named
/// (`known`). A card desk only while its words are still a plain purchase check
/// ([card_check]): it is the desk scammers pose as most. None once the caller
/// asks for anything a scammer asks for or threatens what one threatens
/// ([scam_ask]), anywhere in the call.
pub fn ordinary(known: Option<Scenario>, caller: &str) -> Option<Scenario> {
    if scam_ask(caller) {
        return None;
    }
    let s = match local_guess(caller) {
        Scenario::None => known?,
        s => s,
    };
    if !eases_the_check(s) || (s == Scenario::BankVerify && !card_check(caller)) {
        return None;
    }
    Some(s)
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
    /// The last scenario a look named for sure enough (see [ordinary]).
    known: Option<Scenario>,
}

impl Router {
    pub fn new(lang: Lang, region: Option<Region>, booking: bool) -> Self {
        Router { lang, region, booking, sent: Vec::new(), pending: None, known: None }
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
        self.known = Some(scenario);
        if self.sent.contains(&scenario) {
            return RouteAction::AlreadySent;
        }
        self.pending = Some(scenario);
        RouteAction::Queued
    }

    /// The caller's turn ended and their words name `scenario` ([local_guess]).
    /// A cue already owed is kept (None: nothing new).
    pub fn on_local(&mut self, scenario: Scenario, stalling: bool) -> RouteAction {
        if scenario == Scenario::None {
            return RouteAction::None;
        }
        if stalling {
            self.pending = None;
            return RouteAction::Stalling;
        }
        if self.sent.contains(&scenario) {
            return RouteAction::AlreadySent;
        }
        if self.pending.is_some() {
            return RouteAction::None;
        }
        self.pending = Some(scenario);
        RouteAction::Queued
    }

    /// The answer to a look only for what the call is about (a caller whose
    /// call is never looked at for sales / scam): its verdict is not used.
    pub fn on_scenario(&mut self, scenario: Scenario, confidence: Confidence, stalling: bool) -> RouteAction {
        self.on_review(scenario, Verdict::Normal, confidence, stalling)
    }

    /// The last scenario a look named for sure enough.
    pub fn known(&self) -> Option<Scenario> {
        self.known
    }

    /// Something named the call's scenario already (a cue owed or sent).
    pub fn named(&self) -> bool {
        self.known.is_some() || self.pending.is_some() || !self.sent.is_empty()
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
            return Some((s, format!("{EN_OTHER_LANGUAGE_MARK}{text}")));
        }
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

    fn r(code: &str) -> Region {
        Region::of_code(code).unwrap()
    }

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
                    for region in crate::country_profile::all().into_iter().map(Some).chain([None]) {
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
            (Lang::ZhTw, r("tw"), "管理室", "掰掰"),
            (Lang::En, r("us"), "porch", "say bye"),
            (Lang::Ja, r("jp"), "置き配", "挨拶を返す"),
            (Lang::De, r("de"), "Packstation", "verabschiede dich"),
        ] {
            let c = cue(Scenario::Delivery, lang, Some(region), false).unwrap();
            assert!(c.contains(bye) && !c.contains(habit), "{c}");
            assert!(block(region, lang, false).contains(habit), "{lang:?}");
            // No "keep it short" any more: it cost the goodbye.
            for brief in ["俐落", "brief", "手短", "kurz", "짧게", "breve", "bref"] {
                assert!(!c.contains(brief), "{c}");
            }
        }
        let cn = cue(Scenario::Delivery, Lang::ZhCn, Some(r("cn")), false).unwrap();
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
        // The courier is not handed a spot or the local habit as advice: the AI passes it on.
        assert!(delivery.contains("是快遞") && delivery.contains("別提議") && delivery.contains("說會轉達"), "{delivery}");
        assert!(bank.starts_with("【系統小抄·不是對方說的】像銀行在核對刷卡"), "{bank}");
    }

    #[test]
    fn one_cue_per_scenario_and_none_while_stalling() {
        let mut r = Router::new(Lang::ZhTw, Some(self::r("tw")), false);
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
        let mut r = Router::new(Lang::ZhTw, Some(self::r("jp")), false);
        r.on_review(Scenario::Delivery, Verdict::Normal, Confidence::High, false);
        let (_, text) = r.take_pending(false, Some(Lang::Ja), false).unwrap();
        assert!(text.starts_with("[メモ]") && text.contains("宅配"), "{text}");
        // Chinese of the other script keeps the call's own.
        let mut r = Router::new(Lang::ZhTw, Some(self::r("tw")), false);
        r.on_review(Scenario::WrongNumber, Verdict::Normal, Confidence::High, false);
        let (_, text) = r.take_pending(false, Some(Lang::ZhCn), false).unwrap();
        assert!(text.contains("打錯"), "{text}");
        // An English caller on the same call: English.
        let mut r = Router::new(Lang::ZhTw, Some(self::r("tw")), false);
        r.on_review(Scenario::Delivery, Verdict::Normal, Confidence::High, false);
        assert!(r.take_pending(false, Some(Lang::En), false).unwrap().1.starts_with("[Note]"));
        // A Thai caller on a Taiwan SIM's Chinese call (their earlier Chinese still
        // in the transcript): the cue in English, said in Thai.
        let mut r = Router::new(Lang::ZhTw, Some(self::r("tw")), false);
        r.on_review(Scenario::Delivery, Verdict::Normal, Confidence::High, false);
        let (_, text) = r.take_pending(false, Some(Lang::ZhTw), true).unwrap();
        assert!(text.starts_with(EN_OTHER_LANGUAGE_MARK.get()) && !text.contains("系統小抄") && !text.contains("[Note] "), "{text}");
        assert!(text.chars().count() <= MAX_CUE_CHARS + EN_OTHER_LANGUAGE_MARK.get().len(), "{text}");
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

#[cfg(test)]
mod everyday_calls {
    use super::*;

    /// Couriers, clinics and card desks the way scammers pose as them: a
    /// threat, a stolen identity, customs, an account frozen. No plain
    /// scenario from the words, and nothing to make the call harder to judge.
    /// Many languages.
    #[test]
    fn a_desk_that_threatens_or_asks_for_scam_things_is_no_everyday_call() {
        for t in [
            "這裡是仁愛診所，通知您健保卡被盜用，涉嫌洗錢，請回電",
            "您好我是快遞，您的包裹被海關扣押了",
            "Hi, this is the clinic, your identity was stolen in a money laundering case, call us back",
            "Courier here, your parcel is with customs, otherwise your account will be frozen",
            "Hello, this is the bank verifying a purchase on your card, did you make it? Otherwise an arrest warrant goes out",
            "クリニックの受付です。保険証が不正利用されています。折り返しお電話ください",
            "택배입니다. 고객님 계좌가 자금세탁에 도용되어 동결됩니다",
            "Hola, soy el repartidor, su paquete está en la aduana y su cuenta bloqueada",
            "Bonjour, ici la clinique, votre compte bloqué suite à une usurpation d'identité, rappelez-nous",
            "Hallo, hier ist die Praxis, Ihr Konto wird gesperrt, rufen Sie zurück",
            "Olá, sou o entregador, sua encomenda está na alfândega",
            "Sono il corriere, il pacco è bloccato, c'è un pignoramento sul conto",
            "Merhaba kurye, kargonuz gümrükte, hesabınız bloke edilecek",
            "Xin chào, tôi là người giao hàng, bưu kiện của anh bị hải quan giữ lại",
            "Halo, saya kurir, paket anda tertahan di bea cukai",
        ] {
            assert!(scam_ask(t), "{t}");
            assert_eq!(local_guess(t), Scenario::None, "{t}");
            for known in [None, Some(Scenario::Delivery), Some(Scenario::ImportantNotice), Some(Scenario::BankVerify)] {
                assert_eq!(ordinary(known, t), None, "{t} {known:?}");
            }
        }
    }

    /// Plain couriers, notices and card checks in many languages are everyday calls.
    #[test]
    fn plain_couriers_notices_and_card_checks_are_everyday_calls() {
        for (t, want) in [
            ("您好，黑貓宅急便，您的包裹明天下午送到，家裡有人嗎", Scenario::Delivery),
            ("Bonjour, c'est le livreur, j'ai un colis pour vous.", Scenario::Delivery),
            ("ヤマト運輸です。お荷物のお届けについてお電話しました。", Scenario::Delivery),
            ("Hola, soy el repartidor, tengo un paquete para usted", Scenario::Delivery),
            ("Merhaba, kurye geldi, kargonuz için kapınızdayım", Scenario::Delivery),
            ("Hi, this is the clinic calling. Your appointment has been rescheduled to next Wednesday, please call us back at extension 12.", Scenario::ImportantNotice),
            ("안녕하세요, 서울병원입니다. 검진 일정이 변경되었습니다.", Scenario::ImportantNotice),
            ("Bonjour, ici la clinique, votre rendez-vous est déplacé à mercredi.", Scenario::ImportantNotice),
            ("Hi, this is your bank verifying a purchase on your card, did you make it yourself?", Scenario::BankVerify),
            ("Buongiorno, sono della sua banca, vorrei verificare un addebito di ieri sulla sua carta, è stato lei?", Scenario::BankVerify),
        ] {
            assert!(!scam_ask(t), "{t}");
            assert_eq!(ordinary(None, t), Some(want), "{t}");
        }
        // Setting a time: only a look names it.
        assert_eq!(ordinary(None, "Are you free on Saturday for coffee?"), None);
        assert_eq!(ordinary(Some(Scenario::ScheduleInvite), "Are you free on Saturday for coffee?"), Some(Scenario::ScheduleInvite));
        assert_eq!(ordinary(Some(Scenario::ScheduleInvite), "¿Te viene bien el sábado para tomar un café?"), Some(Scenario::ScheduleInvite));
    }

    /// A family chat and a wrong number open scams too ("guess who this is",
    /// "sorry, wrong number"), and a bank the look names without the words of a
    /// purchase check is the desk scammers pose as most: none eases the check.
    #[test]
    fn chats_wrong_numbers_and_a_bank_by_name_only_do_not_ease_the_check() {
        for t in [
            "猜猜我是誰？好久不見",
            "Hey, it's me, guess who!",
            "やあ、久しぶり、誰だかわかる？",
            "Oi, adivinha quem é!",
            "Sorry, wrong number, but you sound nice. Where are you from?",
            "Perdón, número equivocado. ¿De dónde eres?",
        ] {
            for known in [Some(Scenario::CasualChat), Some(Scenario::WrongNumber)] {
                assert_eq!(ordinary(known, t), None, "{t} {known:?}");
            }
        }
        for t in [
            "這邊是銀行，想跟您聊一下您的帳務",
            "Hello, this is your bank calling about your account.",
            "Hallo, hier ist Ihre Bank, es geht um Ihr Konto.",
            "Bonjour, c'est votre banque, au sujet de votre compte.",
        ] {
            assert_eq!(ordinary(Some(Scenario::BankVerify), t), None, "{t}");
        }
    }

    /// Paying the courier at the door is what a real courier asks for; a fee by
    /// link or transfer still is not. Many languages.
    #[test]
    fn cash_on_delivery_is_an_everyday_courier() {
        for t in [
            "我是黑貓宅急便，明天下午送包裹，貨到付款五百二十元",
            "ヤマト運輸です。お荷物は代金引換で三千二百円です",
            "택배입니다. 착불이라 오천 원 준비해 주세요",
            "Courier here, your parcel is cash on delivery, 20 dollars",
            "Soy el repartidor, el paquete es contra reembolso",
            "Hallo, der Zusteller hier, das Paket kommt per Nachnahme",
            "Sono il corriere, il pacco è in contrassegno",
            "Em là shipper giao hàng, thanh toán khi nhận hàng 250 nghìn",
        ] {
            assert!(!scam_ask(t), "{t}");
            assert_eq!(local_guess(t), Scenario::Delivery, "{t}");
            assert_eq!(ordinary(None, t), Some(Scenario::Delivery), "{t}");
        }
        for t in [
            "我是黑貓宅急便，貨到付款，請先轉帳到這個帳號",
            "Courier here, cash on delivery, or click the link to pay the fee",
            "Soy el repartidor, contra reembolso o pague por transferencia",
        ] {
            assert!(scam_ask(t), "{t}");
            assert_eq!(ordinary(Some(Scenario::Delivery), t), None, "{t}");
        }
    }

    /// A courier at first, then customs and a fee by link: the everyday call ends there.
    #[test]
    fn a_courier_who_turns_to_a_fee_or_a_threat_is_no_everyday_call_any_more() {
        for (first, then) in [
            ("我是順豐派件的，您有一個包裹。", "包裹卡在海關，要先付關稅，點擊連結繳費。"),
            ("Courier here, I have a parcel for you.", "It's held at customs, click the link and pay the fee."),
            ("Sono il corriere, ho un pacco per lei.", "È fermo in dogana, deve pagare le spese di spedizione."),
            ("Soy el repartidor, tengo un paquete para usted.", "Está en la aduana, tiene que pagar la tarifa con el enlace."),
            ("Bonjour, c'est le livreur, j'ai un colis pour vous.", "Il est bloqué à la douane, cliquez sur le lien."),
            ("ヤマト運輸です。お荷物のお届けです。", "税関で止まっています。リンクからお支払いください。"),
        ] {
            assert_eq!(ordinary(None, first), Some(Scenario::Delivery), "{first}");
            let both = format!("{first}\n{then}");
            assert_eq!(ordinary(Some(Scenario::Delivery), &both), None, "{both}");
        }
    }

    /// The caller's own words name the scenario after their turn: once, never
    /// while stalling, and a cue a look queued first is kept.
    #[test]
    fn the_callers_words_queue_a_cue_once() {
        let mut r = Router::new(Lang::En, None, false);
        assert!(!r.named());
        assert_eq!(r.on_local(Scenario::None, false), RouteAction::None);
        assert_eq!(r.on_local(Scenario::Delivery, true), RouteAction::Stalling);
        assert!(r.take_pending(false, None, false).is_none());
        assert_eq!(r.on_local(Scenario::Delivery, false), RouteAction::Queued);
        assert!(r.named());
        assert_eq!(r.on_local(Scenario::Delivery, false), RouteAction::None, "already owed");
        let (s, text) = r.take_pending(false, Some(Lang::Es), false).unwrap();
        assert_eq!(s, Scenario::Delivery);
        assert!(text.starts_with(note_mark(Lang::Es)), "{text}");
        assert_eq!(r.on_local(Scenario::Delivery, false), RouteAction::AlreadySent);
        // A look's cue owed first stays.
        let mut r = Router::new(Lang::Ja, None, false);
        assert_eq!(r.on_review(Scenario::ImportantNotice, Verdict::Normal, Confidence::High, false), RouteAction::Queued);
        assert_eq!(r.known(), Some(Scenario::ImportantNotice));
        assert_eq!(r.on_local(Scenario::Delivery, false), RouteAction::None);
        assert_eq!(r.take_pending(false, None, false).unwrap().0, Scenario::ImportantNotice);
        // A guess or a scam answer is not remembered.
        let mut r = Router::new(Lang::De, None, false);
        r.on_review(Scenario::Delivery, Verdict::Normal, Confidence::Low, false);
        r.on_review(Scenario::Delivery, Verdict::ScamOrSales, Confidence::High, false);
        assert_eq!(r.known(), None);
    }

    /// The look only for what a trusted call is about: its scenario is used
    /// whatever its verdict, never a guess, and never while stalling.
    #[test]
    fn a_look_only_for_the_scenario_queues_its_cue() {
        let mut r = Router::new(Lang::Ko, None, true);
        assert_eq!(r.on_scenario(Scenario::None, Confidence::High, false), RouteAction::None);
        assert_eq!(r.on_scenario(Scenario::CasualChat, Confidence::Low, false), RouteAction::LowConfidence);
        assert_eq!(r.on_scenario(Scenario::CasualChat, Confidence::Medium, false), RouteAction::Queued);
        assert_eq!(r.known(), Some(Scenario::CasualChat));
        let (_, text) = r.take_pending(false, Some(Lang::Fr), false).unwrap();
        assert!(text.starts_with(note_mark(Lang::Fr)), "{text}");
        assert_eq!(r.on_scenario(Scenario::ScheduleInvite, Confidence::High, true), RouteAction::Stalling);
        assert!(r.take_pending(false, None, false).is_none());
    }
}
