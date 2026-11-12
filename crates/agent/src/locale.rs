//! Languages: which one the AI speaks, the phrases the call logic listens for,
//! and the words used in notifications.
//!
//! Traditional Chinese (Taiwan) is the language this project grew up in and
//! the only one exercised on real calls. Simplified Chinese reuses its prompts
//! (converted); every other language shares one set of English instructions
//! ([`crate::prompts`]) filled in with that language's phrases from [`phrases`].
//!
//! The settings value lives in a process-wide slot ([`set_current`]) because
//! the Live session's helpers are free functions. Unset (unit tests) = zh-TW.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    ZhTw,
    ZhCn,
    En,
    Ja,
    Ko,
    Es,
    Fr,
    De,
    Pt,
}

pub const ALL: [Lang; 9] = [
    Lang::En,
    Lang::ZhTw,
    Lang::ZhCn,
    Lang::Ja,
    Lang::Ko,
    Lang::Es,
    Lang::Fr,
    Lang::De,
    Lang::Pt,
];

impl Lang {
    /// Settings / UI code (`en`, `zh-TW`, …). Unknown → English.
    pub fn parse(s: &str) -> Lang {
        let t = s.trim().to_ascii_lowercase().replace('_', "-");
        match t.as_str() {
            "zh-tw" | "zh-hant" | "zh-hk" | "zh" => Lang::ZhTw,
            "zh-cn" | "zh-hans" | "zh-sg" => Lang::ZhCn,
            _ => match t.split('-').next().unwrap_or("") {
                "ja" => Lang::Ja,
                "ko" => Lang::Ko,
                "es" => Lang::Es,
                "fr" => Lang::Fr,
                "de" => Lang::De,
                "pt" => Lang::Pt,
                _ => Lang::En,
            },
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Lang::ZhTw => "zh-TW",
            Lang::ZhCn => "zh-CN",
            Lang::En => "en",
            Lang::Ja => "ja",
            Lang::Ko => "ko",
            Lang::Es => "es",
            Lang::Fr => "fr",
            Lang::De => "de",
            Lang::Pt => "pt",
        }
    }

    pub fn is_zh(self) -> bool {
        matches!(self, Lang::ZhTw | Lang::ZhCn)
    }

}

/// What the running agent is set to (settings `language`, `answer_style`,
/// `owner_name`, `country_code`), for the Live session's free functions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Active {
    pub lang: Lang,
    /// Answer as the owner's AI assistant instead of as the owner.
    pub assistant: bool,
    pub owner: String,
    /// Country calling code of the phone's SIM, digits only (`886`, `1`, `81`).
    pub country_code: String,
    /// Names speech-to-text keeps getting wrong → how they should read
    /// (settings `name_corrections`).
    pub name_fixes: Vec<(String, String)>,
}

impl Default for Active {
    /// Unset (unit tests): Taiwan, answering as the owner — what the call
    /// logic was written and tested with.
    fn default() -> Self {
        Self {
            lang: Lang::ZhTw,
            assistant: false,
            owner: String::new(),
            country_code: "886".to_string(),
            name_fixes: Vec::new(),
        }
    }
}

static ACTIVE: std::sync::RwLock<Option<Active>> = std::sync::RwLock::new(None);

/// Set at startup and on every settings save.
pub fn set_active(a: Active) {
    *ACTIVE.write().unwrap_or_else(|e| e.into_inner()) = Some(a);
}

pub fn active() -> Active {
    ACTIVE
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

pub fn current() -> Lang {
    active().lang
}

/// `wrong=right` lines of the setting `name_corrections` as pairs; lines
/// without both sides are dropped.
pub fn parse_name_fixes(lines: &[String]) -> Vec<(String, String)> {
    lines
        .iter()
        .filter_map(|l| l.split_once('=').or_else(|| l.split_once('→')))
        .map(|(a, b)| (a.trim().to_string(), b.trim().to_string()))
        .filter(|(a, b)| !a.is_empty() && !b.is_empty() && a != b)
        .take(50)
        .collect()
}

/// Apply name corrections to a text.
pub fn fix_names_with(text: &str, fixes: &[(String, String)]) -> String {
    let mut out = text.to_string();
    for (wrong, right) in fixes {
        if out.contains(wrong.as_str()) {
            out = out.replace(wrong.as_str(), right);
        }
    }
    out
}

/// A transcript or summary with the owner's name corrections applied
/// (speech-to-text mishears names the same way every time).
pub fn fix_names(text: &str) -> String {
    let fixes = active().name_fixes;
    if fixes.is_empty() {
        return text.to_string();
    }
    fix_names_with(text, &fixes)
}

/// Country calling code of the phone's SIM (digits); empty = not set, numbers
/// are compared as they are written.
pub fn country_code() -> String {
    active()
        .country_code
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect()
}

/// A line that exists in Chinese and English only (fallback summaries, notes
/// for the summary model): Chinese for the two Chinese settings, English for
/// every other language.
pub fn zh_or(zh_tw: &str, en: &str) -> String {
    match current() {
        Lang::ZhTw => zh_tw.to_string(),
        Lang::ZhCn => zhconv::zhconv(zh_tw, zhconv::Variant::ZhCN),
        _ => en.to_string(),
    }
}

/// Everything language-specific that is a short phrase or a word list.
pub struct Phrases {
    /// English name of the language, for the model's instructions.
    pub name_en: &'static str,
    /// Speech-to-text hint (BCP-47).
    pub stt: &'static str,
    /// How a person answers the phone.
    pub hello: &'static str,
    pub hello_again: &'static str,
    /// Answer to a caller who keeps saying hello.
    pub im_here: &'static str,
    pub still_there: &'static str,
    pub bye: &'static str,
    pub go_ahead: &'static str,
    pub no_thanks: &'static str,
    pub think_about_it: &'static str,
    pub wait_one_more: &'static str,
    /// Stand-in when no owner name is set.
    pub owner_fallback: &'static str,
    /// Blocked-number announcement.
    pub unavailable: &'static str,
    /// Voice sample on the settings page (AI's three lines).
    pub preview: [&'static str; 3],
    // Notifications
    pub n_incoming: &'static str,
    pub n_summary: &'static str,
    pub n_blocked: &'static str,
    pub n_voicemail: &'static str,
    pub n_number: &'static str,
    pub n_phone: &'static str,
    pub n_time: &'static str,
    pub n_reason: &'static str,
    pub n_length: &'static str,
    pub n_seconds: &'static str,
    pub n_hidden: &'static str,
    pub n_unknown: &'static str,
    pub n_details: &'static str,
    pub n_recording: &'static str,
    /// `{}` = the announcement text.
    pub n_blocked_done: &'static str,
    /// Label of the calendar suggestion line.
    pub n_calendar: &'static str,
    pub n_test_title: &'static str,
    pub n_test_body: &'static str,
    // Post-call summary lines
    pub s_caller: &'static str,
    pub s_matter: &'static str,
    pub s_reply: &'static str,
    pub s_todo: &'static str,
    pub s_no_summary: &'static str,
    pub s_ai_failed: &'static str,
    /// A goodbye when it ends what was said.
    pub farewells: &'static [&'static str],
    /// The caller is done ("that's all") when it ends what they said.
    pub wrapups: &'static [&'static str],
    /// Sales / scam words: weight 2 = clear on its own, 1 = needs company.
    pub stall: &'static [(&'static str, u32)],
    /// The caller is following up on something the owner raised.
    pub own_matter: &'static [&'static str],
}

pub fn phrases(lang: Lang) -> &'static Phrases {
    match lang {
        Lang::ZhTw => &ZH_TW,
        Lang::ZhCn => &ZH_CN,
        Lang::En => &EN,
        Lang::Ja => &JA,
        Lang::Ko => &KO,
        Lang::Es => &ES,
        Lang::Fr => &FR,
        Lang::De => &DE,
        Lang::Pt => &PT,
    }
}

// The Chinese word lists stay in `gemini_live` (they predate this module and
// carry their own tests); the entries here are only the display phrases.
static ZH_TW: Phrases = Phrases {
    name_en: "Traditional Chinese (Taiwan Mandarin)",
    stt: "zh-TW",
    hello: "喂？",
    hello_again: "喂？你好？",
    im_here: "有，我在，聽得到，請說",
    still_there: "喂？還在嗎？",
    bye: "掰掰",
    go_ahead: "可以啊，你說",
    no_thanks: "喔不用了，謝謝",
    think_about_it: "這個我要再想一下",
    wait_one_more: "欸等一下，我還有一個問題",
    owner_fallback: "機主",
    unavailable: "此號碼目前無法接通，請稍後再撥。",
    preview: [
        "喂？你好。",
        "喔，是包裹喔，好啊，那你先幫我放管理室就好，謝謝喔。",
        "好，掰掰。",
    ],
    n_incoming: "來電",
    n_summary: "通話摘要",
    n_blocked: "拒接號碼來電",
    n_voicemail: "語音留言",
    n_number: "號碼",
    n_phone: "手機",
    n_time: "時間",
    n_reason: "原因",
    n_length: "長度",
    n_seconds: "秒",
    n_hidden: "未顯示號碼",
    n_unknown: "未知",
    n_details: "逐字稿／錄音",
    n_recording: "聽錄音",
    n_blocked_done: "已播「{}」後掛斷，沒有用 AI",
    n_calendar: "行事曆",
    n_test_title: "測試通知",
    n_test_body: "這是 Myna 的測試通知，看到這則就代表設定成功。",
    s_caller: "對方",
    s_matter: "事情",
    s_reply: "AI 回覆",
    s_todo: "要處理",
    s_no_summary: "（沒有摘要，請看網頁逐字稿）",
    s_ai_failed: "AI 沒接上，沒講話",
    farewells: &[],
    wrapups: &[],
    stall: &[],
    own_matter: &[],
};

static ZH_CN: Phrases = Phrases {
    name_en: "Simplified Chinese (Mainland Mandarin)",
    stt: "cmn-CN",
    hello: "喂？",
    hello_again: "喂？你好？",
    im_here: "在的，我在，听得到，你说",
    still_there: "喂？还在吗？",
    bye: "拜拜",
    go_ahead: "可以，你说",
    no_thanks: "哦不用了，谢谢",
    think_about_it: "这个我得再想想",
    wait_one_more: "哎等一下，我还有个问题",
    owner_fallback: "机主",
    unavailable: "您拨打的号码暂时无法接通，请稍后再拨。",
    preview: [
        "喂？你好。",
        "哦，是快递啊，行，那你先帮我放门口就好，谢谢啊。",
        "好，拜拜。",
    ],
    n_incoming: "来电",
    n_summary: "通话摘要",
    n_blocked: "拒接号码来电",
    n_voicemail: "语音留言",
    n_number: "号码",
    n_phone: "手机",
    n_time: "时间",
    n_reason: "原因",
    n_length: "时长",
    n_seconds: "秒",
    n_hidden: "未显示号码",
    n_unknown: "未知",
    n_details: "通话记录／录音",
    n_recording: "听录音",
    n_blocked_done: "已播放「{}」后挂断，没有使用 AI",
    n_calendar: "日历",
    n_test_title: "测试通知",
    n_test_body: "这是 Myna 的测试通知，看到这条就说明设置成功。",
    s_caller: "对方",
    s_matter: "事情",
    s_reply: "AI 回复",
    s_todo: "要处理",
    s_no_summary: "（没有摘要，请看网页通话记录）",
    s_ai_failed: "AI 没接上，没说话",
    farewells: &[],
    wrapups: &[],
    stall: &[],
    own_matter: &[],
};

static EN: Phrases = Phrases {
    name_en: "English",
    stt: "en-US",
    hello: "Hello?",
    hello_again: "Hello? Hi?",
    im_here: "Yes, I'm here, I can hear you. Go ahead.",
    still_there: "Hello? Are you still there?",
    bye: "Bye",
    go_ahead: "Sure, go ahead",
    no_thanks: "Oh, no thanks",
    think_about_it: "I'd have to think about that",
    wait_one_more: "Oh wait, hang on, I've got one more question",
    owner_fallback: "the owner",
    unavailable: "The number you have dialed is not available right now. Please try again later.",
    preview: [
        "Hello?",
        "Oh, a package? Sure, just leave it at the front door, thanks a lot.",
        "Okay, bye.",
    ],
    n_incoming: "Incoming call",
    n_summary: "Call summary",
    n_blocked: "Blocked number called",
    n_voicemail: "Voicemail",
    n_number: "Number",
    n_phone: "Phone",
    n_time: "Time",
    n_reason: "Reason",
    n_length: "Length",
    n_seconds: "s",
    n_hidden: "Hidden number",
    n_unknown: "Unknown",
    n_details: "Transcript / recording",
    n_recording: "Recording",
    n_blocked_done: "Played \"{}\" and hung up. The AI was not used.",
    n_calendar: "Calendar",
    n_test_title: "Test notification",
    n_test_body: "This is a test notification from Myna. If you can read it, the channel works.",
    s_caller: "Caller",
    s_matter: "About",
    s_reply: "AI said",
    s_todo: "To do",
    s_no_summary: "(No summary — see the transcript in the web console.)",
    s_ai_failed: "The AI did not connect and said nothing",
    farewells: &[
        "bye", "bye bye", "bye now", "goodbye", "good bye", "see you", "see ya", "see you later",
        "talk to you later", "talk later", "talk soon", "speak soon", "take care",
        "have a good day", "have a nice day", "have a great day", "have a good one",
        "have a good night", "good night", "cheers",
    ],
    wrapups: &[
        "that's all", "that is all", "that's it", "that's everything", "nothing else",
        "i'll let you go", "won't keep you", "gotta go", "got to go", "have to go",
        "i'll call back", "call back later", "i'll try again later",
    ],
    stall: &[
        ("insurance", 2), ("life insurance", 2), ("coverage", 1), ("premium", 1), ("policy", 1),
        ("loan", 2), ("mortgage", 2), ("refinanc", 2), ("credit card", 2), ("interest rate", 2),
        ("debt", 2), ("consolidat", 2), ("past due", 2), ("overdue", 2), ("final notice", 2),
        ("collection", 1), ("investment", 2), ("invest", 1), ("crypto", 2), ("bitcoin", 2),
        ("forex", 2), ("trading", 1), ("guaranteed return", 2), ("warranty", 2),
        ("social security", 2), ("arrest warrant", 2), ("gift card", 2), ("wire transfer", 2),
        ("verification code", 2), ("tech support", 2), ("medicare", 2), ("prize", 2),
        ("lottery", 2), ("sweepstakes", 2), ("timeshare", 2), ("solar", 2), ("special offer", 2),
        ("limited time", 2), ("business listing", 2), ("duct cleaning", 2), ("promotion", 1),
        ("discount", 1), ("free", 1), ("survey", 1), ("upgrade", 1), ("refund", 1),
        ("suspended", 1), ("donation", 1), ("offer", 1), ("account", 1), ("bank", 1),
        ("customer service", 1), ("representative", 1), ("department", 1),
    ],
    own_matter: &[
        "your complaint", "your inquiry", "your enquiry", "your request", "you reported",
        "you contacted us", "following up on your", "regarding your case", "your claim",
        "your ticket", "returning your call",
    ],
};

static JA: Phrases = Phrases {
    name_en: "Japanese",
    stt: "ja-JP",
    hello: "もしもし？",
    hello_again: "もしもし？聞こえますか？",
    im_here: "はい、聞こえてますよ。どうぞ。",
    still_there: "もしもし？聞こえてますか？",
    bye: "失礼します",
    go_ahead: "はい、大丈夫ですよ、どうぞ",
    no_thanks: "あ、結構です",
    think_about_it: "それはちょっと考えさせてください",
    wait_one_more: "あ、ちょっと待って、もう一つ聞いてもいいですか",
    owner_fallback: "持ち主",
    unavailable: "おかけになった電話は、現在おつなぎできません。しばらくしてからおかけ直しください。",
    preview: [
        "もしもし？",
        "あ、荷物ですか。じゃあ玄関の前に置いといてください、ありがとうございます。",
        "はい、失礼します。",
    ],
    n_incoming: "着信",
    n_summary: "通話の要約",
    n_blocked: "拒否リストの番号から着信",
    n_voicemail: "留守番電話",
    n_number: "番号",
    n_phone: "端末",
    n_time: "時刻",
    n_reason: "理由",
    n_length: "長さ",
    n_seconds: "秒",
    n_hidden: "非通知",
    n_unknown: "不明",
    n_details: "文字起こし／録音",
    n_recording: "録音",
    n_blocked_done: "「{}」を流して切断しました（AI は使っていません）",
    n_calendar: "カレンダー",
    n_test_title: "テスト通知",
    n_test_body: "Myna からのテスト通知です。これが届いていれば設定は完了です。",
    s_caller: "相手",
    s_matter: "用件",
    s_reply: "AI の返答",
    s_todo: "対応",
    s_no_summary: "（要約なし。Web の文字起こしを確認してください）",
    s_ai_failed: "AI が接続できず、何も話していません",
    farewells: &[
        "さようなら", "失礼します", "失礼いたします", "失礼致します", "じゃあね", "じゃあまた",
        "ではまた", "またね", "バイバイ", "ばいばい", "切りますね", "お疲れ様でした",
        "お疲れさまでした",
    ],
    wrapups: &[
        "以上です", "それだけです", "また連絡します", "またご連絡します", "またかけます",
        "かけ直します", "掛け直します",
    ],
    stall: &[
        ("保険", 2), ("ローン", 2), ("融資", 2), ("借入", 2), ("クレジットカード", 2),
        ("金利", 2), ("返済", 2), ("滞納", 2), ("未納", 2), ("督促", 2), ("投資", 2),
        ("資産運用", 2), ("仮想通貨", 2), ("暗号資産", 2), ("ビットコイン", 2), ("株", 1),
        ("不動産", 2), ("マンション", 1), ("還付金", 2), ("振り込", 2), ("振込", 2),
        ("暗証番号", 2), ("認証コード", 2), ("警察", 1), ("検察", 2), ("逮捕", 2),
        ("当選", 2), ("キャンペーン", 2), ("お得", 1), ("無料", 1), ("割引", 1),
        ("アンケート", 1), ("光回線", 2), ("電気料金", 1), ("プラン", 1), ("ご案内", 1),
        ("口座", 1), ("銀行", 1), ("カスタマー", 1), ("担当", 1),
    ],
    own_matter: &["お問い合わせの件", "お問い合わせいただいた", "ご依頼の件", "ご連絡いただいた件", "折り返し"],
};

static KO: Phrases = Phrases {
    name_en: "Korean",
    stt: "ko-KR",
    hello: "여보세요?",
    hello_again: "여보세요? 들리세요?",
    im_here: "네, 듣고 있어요. 말씀하세요.",
    still_there: "여보세요? 아직 계세요?",
    bye: "네, 들어가세요",
    go_ahead: "네, 괜찮아요. 말씀하세요",
    no_thanks: "아, 괜찮습니다",
    think_about_it: "그건 좀 생각해 볼게요",
    wait_one_more: "아 잠깐만요, 하나만 더 물어볼게요",
    owner_fallback: "주인",
    unavailable: "지금은 전화를 받을 수 없습니다. 잠시 후 다시 걸어 주시기 바랍니다.",
    preview: [
        "여보세요?",
        "아, 택배요? 네, 그럼 문 앞에 놔 주세요. 감사합니다.",
        "네, 들어가세요.",
    ],
    n_incoming: "수신 전화",
    n_summary: "통화 요약",
    n_blocked: "차단 번호에서 전화",
    n_voicemail: "음성 메시지",
    n_number: "번호",
    n_phone: "기기",
    n_time: "시간",
    n_reason: "사유",
    n_length: "길이",
    n_seconds: "초",
    n_hidden: "발신번호 표시 제한",
    n_unknown: "알 수 없음",
    n_details: "대화 기록／녹음",
    n_recording: "녹음",
    n_blocked_done: "「{}」 안내 후 끊었습니다 (AI 사용 안 함)",
    n_calendar: "캘린더",
    n_test_title: "테스트 알림",
    n_test_body: "Myna 테스트 알림입니다. 이 메시지가 보이면 설정이 완료된 것입니다.",
    s_caller: "상대",
    s_matter: "용건",
    s_reply: "AI 답변",
    s_todo: "할 일",
    s_no_summary: "(요약 없음. 웹 콘솔의 대화 기록을 확인하세요)",
    s_ai_failed: "AI가 연결되지 않아 아무 말도 하지 않았습니다",
    farewells: &[
        "안녕히 계세요", "안녕히계세요", "안녕히 가세요", "들어가세요", "끊을게요", "끊겠습니다",
        "수고하세요", "바이바이", "다음에 봬요", "다음에 뵙겠습니다",
    ],
    wrapups: &[
        "이상입니다", "그게 다예요", "다시 전화드릴게요", "다시 전화드리겠습니다",
        "다시 연락드릴게요", "다시 연락드리겠습니다",
    ],
    stall: &[
        ("보험", 2), ("대출", 2), ("금리", 2), ("신용카드", 2), ("카드론", 2), ("연체", 2),
        ("미납", 2), ("채무", 2), ("투자", 2), ("주식", 1), ("코인", 2), ("가상화폐", 2),
        ("비트코인", 2), ("분양", 2), ("부동산", 1), ("검찰", 2), ("수사관", 2), ("금융감독원", 2),
        ("계좌이체", 2), ("송금", 2), ("인증번호", 2), ("비밀번호", 2), ("당첨", 2),
        ("이벤트", 1), ("할인", 1), ("무료", 1), ("혜택", 1), ("요금제", 1), ("설문", 1),
        ("상담", 1), ("고객센터", 1), ("계좌", 1), ("은행", 1), ("안내", 1),
    ],
    own_matter: &["문의하신", "접수하신", "요청하신", "신고하신", "민원"],
};

static ES: Phrases = Phrases {
    name_en: "Spanish",
    stt: "es-ES",
    hello: "¿Sí? ¿Diga?",
    hello_again: "¿Hola? ¿Me oye?",
    im_here: "Sí, estoy aquí, le oigo bien. Dígame.",
    still_there: "¿Hola? ¿Sigue ahí?",
    bye: "Adiós",
    go_ahead: "Sí, claro, dígame",
    no_thanks: "Ah, no, gracias",
    think_about_it: "Eso tendría que pensarlo",
    wait_one_more: "Ay, espere un momento, tengo otra pregunta",
    owner_fallback: "el dueño",
    unavailable: "El número al que llama no está disponible en este momento. Por favor, inténtelo más tarde.",
    preview: [
        "¿Sí? ¿Diga?",
        "Ah, ¿un paquete? Vale, déjelo en la puerta, muchas gracias.",
        "Vale, adiós.",
    ],
    n_incoming: "Llamada entrante",
    n_summary: "Resumen de la llamada",
    n_blocked: "Llamada de un número bloqueado",
    n_voicemail: "Mensaje de voz",
    n_number: "Número",
    n_phone: "Teléfono",
    n_time: "Hora",
    n_reason: "Motivo",
    n_length: "Duración",
    n_seconds: "s",
    n_hidden: "Número oculto",
    n_unknown: "Desconocido",
    n_details: "Transcripción / grabación",
    n_recording: "Grabación",
    n_blocked_done: "Se reprodujo «{}» y se colgó. No se usó la IA.",
    n_calendar: "Calendario",
    n_test_title: "Notificación de prueba",
    n_test_body: "Esta es una notificación de prueba de Myna. Si la ves, el canal funciona.",
    s_caller: "Quién",
    s_matter: "Asunto",
    s_reply: "La IA dijo",
    s_todo: "Pendiente",
    s_no_summary: "(Sin resumen: consulta la transcripción en la consola web.)",
    s_ai_failed: "La IA no se conectó y no dijo nada",
    farewells: &[
        "adiós", "adios", "hasta luego", "hasta pronto", "hasta mañana", "nos vemos", "chao",
        "chau", "que tenga buen día", "que tenga un buen día", "que tengas buen día", "cuídese",
        "cuídate", "un saludo",
    ],
    wrapups: &[
        "eso es todo", "nada más", "eso sería todo", "le dejo", "te dejo", "tengo que irme",
        "me tengo que ir", "le llamo luego", "vuelvo a llamar",
    ],
    stall: &[
        ("seguro", 2), ("póliza", 2), ("préstamo", 2), ("prestamo", 2), ("hipoteca", 2),
        ("crédito", 2), ("credito", 2), ("tarjeta", 1), ("interés", 1), ("deuda", 2),
        ("impago", 2), ("morosidad", 2), ("inversión", 2), ("inversion", 2), ("invertir", 2),
        ("criptomoneda", 2), ("bitcoin", 2), ("bolsa", 1), ("rentabilidad", 2), ("premio", 2),
        ("sorteo", 2), ("lotería", 2), ("transferencia", 2), ("código de verificación", 2),
        ("contraseña", 2), ("oferta", 1), ("promoción", 1), ("promocion", 1), ("descuento", 1),
        ("gratis", 1), ("tarifa", 1), ("encuesta", 1), ("compañía", 1), ("luz", 1), ("fibra", 1),
        ("cuenta", 1), ("banco", 1), ("atención al cliente", 1), ("asesor", 1),
    ],
    own_matter: &["su reclamación", "su consulta", "su solicitud", "su incidencia", "su queja", "devolver su llamada"],
};

static FR: Phrases = Phrases {
    name_en: "French",
    stt: "fr-FR",
    hello: "Allô ?",
    hello_again: "Allô ? Vous m'entendez ?",
    im_here: "Oui, je suis là, je vous entends bien. Allez-y.",
    still_there: "Allô ? Vous êtes toujours là ?",
    bye: "Au revoir",
    go_ahead: "Oui, bien sûr, allez-y",
    no_thanks: "Ah non, merci",
    think_about_it: "Ça, il faudrait que j'y réfléchisse",
    wait_one_more: "Ah, attendez, j'ai encore une question",
    owner_fallback: "le propriétaire",
    unavailable: "Le numéro que vous avez composé n'est pas disponible pour le moment. Veuillez rappeler ultérieurement.",
    preview: [
        "Allô ?",
        "Ah, un colis ? D'accord, laissez-le devant la porte, merci beaucoup.",
        "D'accord, au revoir.",
    ],
    n_incoming: "Appel entrant",
    n_summary: "Résumé de l'appel",
    n_blocked: "Appel d'un numéro bloqué",
    n_voicemail: "Message vocal",
    n_number: "Numéro",
    n_phone: "Téléphone",
    n_time: "Heure",
    n_reason: "Motif",
    n_length: "Durée",
    n_seconds: "s",
    n_hidden: "Numéro masqué",
    n_unknown: "Inconnu",
    n_details: "Transcription / enregistrement",
    n_recording: "Enregistrement",
    n_blocked_done: "Annonce « {} » diffusée, puis raccroché. L'IA n'a pas été utilisée.",
    n_calendar: "Agenda",
    n_test_title: "Notification de test",
    n_test_body: "Ceci est une notification de test de Myna. Si vous la voyez, le canal fonctionne.",
    s_caller: "Qui",
    s_matter: "Objet",
    s_reply: "Réponse de l'IA",
    s_todo: "À faire",
    s_no_summary: "(Pas de résumé : voir la transcription dans la console web.)",
    s_ai_failed: "L'IA ne s'est pas connectée et n'a rien dit",
    farewells: &[
        "au revoir", "à bientôt", "a bientot", "à plus", "à plus tard", "à tout à l'heure",
        "à demain", "bonne journée", "bonne soirée", "bonne fin de journée", "ciao",
    ],
    wrapups: &[
        "c'est tout", "rien d'autre", "je vous laisse", "je te laisse", "je dois y aller",
        "je rappellerai", "je vous rappelle",
    ],
    stall: &[
        ("assurance", 2), ("mutuelle", 2), ("prêt", 2), ("crédit", 2), ("credit", 2),
        ("rachat de crédit", 2), ("hypothèque", 2), ("taux", 1), ("dette", 2), ("impayé", 2),
        ("recouvrement", 2), ("investissement", 2), ("investir", 2), ("placement", 2),
        ("crypto", 2), ("bitcoin", 2), ("bourse", 1), ("rendement", 2), ("gagné", 2),
        ("tirage", 2), ("loterie", 2), ("virement", 2), ("code de vérification", 2),
        ("mot de passe", 2), ("compte formation", 2), ("cpf", 2), ("panneaux solaires", 2),
        ("pompe à chaleur", 2), ("isolation", 2), ("offre", 1), ("promotion", 1),
        ("réduction", 1), ("gratuit", 1), ("forfait", 1), ("sondage", 1), ("enquête", 1),
        ("compte", 1), ("banque", 1), ("service client", 1), ("conseiller", 1),
    ],
    own_matter: &["votre réclamation", "votre demande", "votre dossier", "votre signalement", "suite à votre appel"],
};

static DE: Phrases = Phrases {
    name_en: "German",
    stt: "de-DE",
    hello: "Hallo?",
    hello_again: "Hallo? Hören Sie mich?",
    im_here: "Ja, ich bin dran, ich höre Sie gut. Bitte.",
    still_there: "Hallo? Sind Sie noch dran?",
    bye: "Tschüss",
    go_ahead: "Ja klar, schießen Sie los",
    no_thanks: "Ach nein, danke",
    think_about_it: "Das müsste ich mir noch überlegen",
    wait_one_more: "Ach, warten Sie kurz, ich hab noch eine Frage",
    owner_fallback: "der Besitzer",
    unavailable: "Der gewünschte Gesprächspartner ist zurzeit nicht erreichbar. Bitte versuchen Sie es später noch einmal.",
    preview: [
        "Hallo?",
        "Ach, ein Paket? Gut, stellen Sie es einfach vor die Tür, vielen Dank.",
        "Okay, tschüss.",
    ],
    n_incoming: "Eingehender Anruf",
    n_summary: "Anrufzusammenfassung",
    n_blocked: "Anruf von gesperrter Nummer",
    n_voicemail: "Sprachnachricht",
    n_number: "Nummer",
    n_phone: "Telefon",
    n_time: "Zeit",
    n_reason: "Grund",
    n_length: "Dauer",
    n_seconds: "s",
    n_hidden: "Unterdrückte Nummer",
    n_unknown: "Unbekannt",
    n_details: "Transkript / Aufnahme",
    n_recording: "Aufnahme",
    n_blocked_done: "Ansage „{}“ abgespielt und aufgelegt. Die KI wurde nicht verwendet.",
    n_calendar: "Kalender",
    n_test_title: "Testbenachrichtigung",
    n_test_body: "Dies ist eine Testbenachrichtigung von Myna. Wenn du sie siehst, funktioniert der Kanal.",
    s_caller: "Anrufer",
    s_matter: "Anliegen",
    s_reply: "KI-Antwort",
    s_todo: "Zu tun",
    s_no_summary: "(Keine Zusammenfassung – siehe Transkript in der Web-Konsole.)",
    s_ai_failed: "Die KI konnte sich nicht verbinden und hat nichts gesagt",
    farewells: &[
        "tschüss", "tschüs", "tschuess", "auf wiederhören", "auf wiedersehen", "wiederhören",
        "bis bald", "bis später", "bis dann", "schönen tag noch", "schönen abend noch",
        "mach's gut", "machs gut", "ciao",
    ],
    wrapups: &[
        "das war's", "das wars", "das wäre alles", "das ist alles", "sonst nichts",
        "ich muss los", "ich muss auflegen", "ich melde mich wieder", "ich rufe später an",
    ],
    stall: &[
        ("versicherung", 2), ("kredit", 2), ("darlehen", 2), ("finanzierung", 2), ("zins", 1),
        ("schulden", 2), ("mahnung", 2), ("inkasso", 2), ("geldanlage", 2), ("investment", 2),
        ("investier", 2), ("krypto", 2), ("bitcoin", 2), ("aktien", 1), ("rendite", 2),
        ("gewinnspiel", 2), ("gewonnen", 2), ("lotterie", 2), ("überweisung", 2),
        ("bestätigungscode", 2), ("passwort", 2), ("stromanbieter", 2), ("stromtarif", 2),
        ("photovoltaik", 2), ("solaranlage", 2), ("angebot", 1), ("rabatt", 1), ("gratis", 1),
        ("kostenlos", 1), ("tarif", 1), ("umfrage", 1), ("vertrag", 1), ("konto", 1),
        ("bank", 1), ("kundenservice", 1), ("berater", 1),
    ],
    own_matter: &["ihre beschwerde", "ihre anfrage", "ihre reklamation", "ihr anliegen", "ihren rückruf"],
};

static PT: Phrases = Phrases {
    name_en: "Portuguese",
    stt: "pt-BR",
    hello: "Alô?",
    hello_again: "Alô? Está me ouvindo?",
    im_here: "Sim, estou aqui, estou ouvindo bem. Pode falar.",
    still_there: "Alô? Ainda está aí?",
    bye: "Tchau",
    go_ahead: "Claro, pode falar",
    no_thanks: "Ah, não, obrigado",
    think_about_it: "Isso eu teria que pensar",
    wait_one_more: "Ah, espera um pouquinho, tenho mais uma pergunta",
    owner_fallback: "o dono",
    unavailable: "O número chamado não está disponível no momento. Por favor, tente novamente mais tarde.",
    preview: [
        "Alô?",
        "Ah, uma encomenda? Tá bom, pode deixar na porta, muito obrigado.",
        "Tá bom, tchau.",
    ],
    n_incoming: "Chamada recebida",
    n_summary: "Resumo da chamada",
    n_blocked: "Chamada de número bloqueado",
    n_voicemail: "Mensagem de voz",
    n_number: "Número",
    n_phone: "Telefone",
    n_time: "Hora",
    n_reason: "Motivo",
    n_length: "Duração",
    n_seconds: "s",
    n_hidden: "Número oculto",
    n_unknown: "Desconhecido",
    n_details: "Transcrição / gravação",
    n_recording: "Gravação",
    n_blocked_done: "Foi reproduzido \"{}\" e a chamada foi encerrada. A IA não foi usada.",
    n_calendar: "Calendário",
    n_test_title: "Notificação de teste",
    n_test_body: "Esta é uma notificação de teste do Myna. Se você está vendo isto, o canal funciona.",
    s_caller: "Quem",
    s_matter: "Assunto",
    s_reply: "A IA disse",
    s_todo: "A fazer",
    s_no_summary: "(Sem resumo — veja a transcrição no console web.)",
    s_ai_failed: "A IA não conectou e não disse nada",
    farewells: &[
        "tchau", "adeus", "até logo", "até mais", "até breve", "até amanhã", "tenha um bom dia",
        "tenha uma boa tarde", "um abraço", "falou",
    ],
    wrapups: &[
        "é só isso", "só isso", "mais nada", "é tudo", "tenho que ir", "preciso desligar",
        "ligo depois", "volto a ligar",
    ],
    stall: &[
        ("seguro", 2), ("apólice", 2), ("empréstimo", 2), ("emprestimo", 2), ("consignado", 2),
        ("financiamento", 2), ("crédito", 2), ("credito", 2), ("cartão", 1), ("juros", 1),
        ("dívida", 2), ("divida", 2), ("cobrança", 2), ("atraso", 1), ("investimento", 2),
        ("investir", 2), ("criptomoeda", 2), ("bitcoin", 2), ("rendimento", 2), ("prêmio", 2),
        ("sorteio", 2), ("loteria", 2), ("transferência", 2), ("pix", 2),
        ("código de verificação", 2), ("senha", 2), ("oferta", 1), ("promoção", 1),
        ("desconto", 1), ("grátis", 1), ("plano", 1), ("pesquisa", 1), ("operadora", 1),
        ("conta", 1), ("banco", 1), ("atendimento", 1), ("consultor", 1),
    ],
    own_matter: &["sua reclamação", "sua solicitação", "seu pedido", "seu chamado", "retornando sua ligação"],
};

/// Languages whose word lists are matched as whole words (spaces between words).
const WORD_LANGS: [Lang; 5] = [Lang::En, Lang::Es, Lang::Fr, Lang::De, Lang::Pt];
/// Languages written without spaces: matched as substrings.
const RUN_LANGS: [Lang; 2] = [Lang::Ja, Lang::Ko];

/// Lowercase words of `text`, punctuation dropped (apostrophes kept inside words).
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .replace('’', "'")
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .map(|w| w.trim_matches('\'').to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// `phrase` (space-separated words) ends `words`, with at most `slack` words after it.
fn phrase_near_end(words: &[String], phrase: &str, slack: usize) -> bool {
    let p = self::words(phrase);
    if p.is_empty() || words.len() < p.len() {
        return false;
    }
    let last_start = words.len() - p.len();
    let first_start = last_start.saturating_sub(slack);
    (first_start..=last_start).any(|i| words[i..i + p.len()] == p[..])
}

/// `marker` sits within `slack` characters of the end of a text without spaces.
fn run_near_end(compact: &str, marker: &str, slack: usize) -> bool {
    let m: String = marker.chars().filter(|c| c.is_alphanumeric()).collect();
    compact
        .rfind(&m)
        .is_some_and(|i| compact[i + m.len()..].chars().count() <= slack)
}

fn ends_on(text: &str, pick: fn(&Phrases) -> &'static [&'static str], slack_words: usize, slack_chars: usize) -> bool {
    let ws = words(text);
    if WORD_LANGS
        .iter()
        .any(|l| pick(phrases(*l)).iter().any(|m| phrase_near_end(&ws, m, slack_words)))
    {
        return true;
    }
    let compact: String = text.chars().filter(|c| c.is_alphanumeric()).collect();
    RUN_LANGS
        .iter()
        .any(|l| pick(phrases(*l)).iter().any(|m| run_near_end(&compact, m, slack_chars)))
}

fn is_question(text: &str) -> bool {
    let t = text.trim_end();
    t.ends_with('?') || t.ends_with('？') || t.ends_with("か。") || t.ends_with("까요")
}

/// What was said ends on a goodbye, in any supported language other than
/// Chinese (those rules live in `gemini_live`). `slack` = words that may follow
/// it ("bye now", "goodbye then").
pub fn ends_with_farewell(text: &str, slack: usize) -> bool {
    ends_on(text, |p| p.farewells, slack, slack * 2)
}

/// The caller's words end the call: a goodbye or "that's all" at the end, and
/// not a question.
pub fn caller_wraps_up(text: &str) -> bool {
    !is_question(text) && (ends_with_farewell(text, 2) || ends_on(text, |p| p.wrapups, 2, 4))
}

/// Sales / scam keywords of the non-Chinese lists found in `text`.
/// Word languages match at the start of a word ("loan" hits "loans").
pub fn stall_hits(text: &str) -> Vec<(&'static str, u32)> {
    let lower = text.to_lowercase();
    let padded = format!(" {} ", words(text).join(" "));
    let mut out: Vec<(&'static str, u32)> = Vec::new();
    for l in WORD_LANGS {
        for &(kw, w) in phrases(l).stall {
            if !out.iter().any(|(k, _)| *k == kw) && padded.contains(&format!(" {kw}")) {
                out.push((kw, w));
            }
        }
    }
    for l in RUN_LANGS {
        for &(kw, w) in phrases(l).stall {
            if !out.iter().any(|(k, _)| *k == kw) && lower.contains(kw) {
                out.push((kw, w));
            }
        }
    }
    out
}

/// Weight of a sales / scam keyword from the non-Chinese lists.
pub fn stall_weight(kw: &str) -> Option<u32> {
    WORD_LANGS
        .iter()
        .chain(RUN_LANGS.iter())
        .flat_map(|l| phrases(*l).stall.iter())
        .find(|(k, _)| *k == kw)
        .map(|(_, w)| *w)
}

/// The caller follows up on something the owner raised (non-Chinese lists).
pub fn own_matter(text: &str) -> bool {
    let lower = text.to_lowercase();
    WORD_LANGS
        .iter()
        .chain(RUN_LANGS.iter())
        .any(|l| phrases(*l).own_matter.iter().any(|m| lower.contains(m)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_corrections_rewrite_what_stt_mishears() {
        let fixes = parse_name_fixes(&[
            "黃先生=王先生".to_string(),
            " Jon Smith → John Smith ".to_string(),
            "no separator".to_string(),
            "same=same".to_string(),
            "=x".to_string(),
        ]);
        assert_eq!(fixes.len(), 2);
        assert_eq!(fix_names_with("請轉告黃先生明天開會", &fixes), "請轉告王先生明天開會");
        assert_eq!(fix_names_with("Is Jon Smith there?", &fixes), "Is John Smith there?");
        assert_eq!(fix_names_with("從黃山寄來的包裹", &fixes), "從黃山寄來的包裹");
        // Nothing set (the default): text is untouched.
        assert_eq!(fix_names("請轉告黃先生"), "請轉告黃先生");
    }

    #[test]
    fn codes_round_trip() {
        for l in ALL {
            assert_eq!(Lang::parse(l.code()), l);
        }
        assert_eq!(Lang::parse("zh_TW"), Lang::ZhTw);
        assert_eq!(Lang::parse("pt-BR"), Lang::Pt);
        assert_eq!(Lang::parse("xx"), Lang::En);
        assert_eq!(Lang::parse(""), Lang::En);
    }

    #[test]
    fn farewells_only_count_at_the_end() {
        for t in [
            "Okay, thanks a lot. Bye!",
            "alright bye now",
            "Great, talk to you later.",
            "Vale, muchas gracias, hasta luego.",
            "D'accord, merci, au revoir !",
            "Gut, danke, auf Wiederhören.",
            "Tá bom, obrigado, tchau.",
            "はい、ありがとうございました。失礼します。",
            "네 감사합니다. 안녕히 계세요.",
        ] {
            assert!(ends_with_farewell(t, 2), "{t}");
            assert!(caller_wraps_up(t), "{t}");
        }
        for t in [
            "Maybe.",
            "I said goodbye to him yesterday and then he called again about the loan",
            "The bylaws say so",
            "Hello?",
            "Is that all?",
            "もしもし",
            "Buenas noches",
        ] {
            assert!(!caller_wraps_up(t), "{t}");
        }
        assert!(caller_wraps_up("Okay, that's all, thank you."));
        assert!(caller_wraps_up("以上です。"));
        assert!(!caller_wraps_up("Bye?"));
    }

    #[test]
    fn stall_keywords_match_whole_words() {
        let hits = stall_hits("Hi, I'm calling about your car's extended warranty and a low interest loan");
        let kws: Vec<&str> = hits.iter().map(|(k, _)| *k).collect();
        assert!(kws.contains(&"warranty") && kws.contains(&"loan"), "{kws:?}");
        // Only the start of a word counts: "bank" does not hit "embankment".
        assert!(stall_hits("the embankment by the river").is_empty());
        assert!(!stall_hits("保険のご案内です").is_empty());
        assert_eq!(stall_weight("warranty"), Some(2));
        assert_eq!(stall_weight("nonsense"), None);
    }

    #[test]
    fn every_language_has_its_phrases() {
        for l in ALL {
            let p = phrases(l);
            assert!(!p.hello.is_empty() && !p.bye.is_empty() && !p.unavailable.is_empty(), "{l:?}");
            assert!(p.n_blocked_done.contains("{}"), "{l:?}");
            if !l.is_zh() {
                assert!(!p.farewells.is_empty() && !p.stall.is_empty(), "{l:?}");
            }
        }
    }
}
