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

/// Between a label and its value in the agent's language ("Number: …", "號碼：…").
pub fn kv_sep() -> &'static str {
    phrases(current()).sep
}

/// A phrase template with its `{}` filled in.
pub fn fill(template: &str, value: &str) -> String {
    template.replacen("{}", value, 1)
}

/// `text` in the language's quotation marks.
pub fn quoted(lang: Lang, text: &str) -> String {
    let (open, close) = phrases(lang).quote;
    format!("{open}{text}{close}")
}

/// A date and time as notifications show it, in the language's own order:
/// "Nov 13, 08:09", "11月13日 08:09", "13.11. 08:09". The time is whatever
/// zone the caller converted it to (the machine's local time, see `TZ`).
pub fn date_time(lang: Lang, t: &chrono::NaiveDateTime) -> String {
    t.format(phrases(lang).dt_fmt).to_string()
}

/// A date alone ("Nov 13", "11月13日", "13.11.").
pub fn date_only(lang: Lang, d: &chrono::NaiveDate) -> String {
    d.format(phrases(lang).date_fmt).to_string()
}

/// How long something lasted, the way people say it: seconds under a minute
/// and a half, whole minutes from there ("72 秒", "10 min").
pub fn duration(lang: Lang, secs: f64) -> String {
    let p = phrases(lang);
    let secs = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
    if secs < 90.0 {
        fill(p.u_sec, &format!("{secs:.0}"))
    } else {
        fill(p.u_min, &format!("{:.0}", secs / 60.0))
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
    // Formats
    /// Between a label and its value ("Number: …", "號碼：…").
    pub sep: &'static str,
    /// Between listed words.
    pub list_sep: &'static str,
    /// Quotation marks around a quoted phrase.
    pub quote: (&'static str, &'static str),
    /// chrono format of a date and time in notifications ([`date_time`]).
    pub dt_fmt: &'static str,
    /// chrono format of a date alone ([`date_only`]).
    pub date_fmt: &'static str,
    /// `{}` seconds / `{}` minutes ([`duration`]).
    pub u_sec: &'static str,
    pub u_min: &'static str,
    // Summaries written without the summary model. `{}` = a length
    // ([`duration`]) or the words that were named.
    pub s_no_service: &'static str,
    pub s_see_transcript: &'static str,
    pub s_no_transcript: &'static str,
    pub s_listen_recording: &'static str,
    pub s_unclear: &'static str,
    pub s_ai_fail_matter: &'static str,
    pub s_call_back_if_known: &'static str,
    /// First line put above a summary when the AI never connected.
    pub s_ai_fail_prefix: &'static str,
    pub s_stall_topic: &'static str,
    pub s_stall_topic_kw: &'static str,
    pub s_stall_reply: &'static str,
    /// Nothing to do.
    pub s_nothing: &'static str,
    // Voicemail (older menu flow): how the caller wants an answer. `{}` = the
    // number or address, or a length.
    pub vm_call_back_to: &'static str,
    pub vm_call_back_unknown: &'static str,
    pub vm_email_to: &'static str,
    pub vm_email_unknown: &'static str,
    pub vm_call_back_unchecked: &'static str,
    pub vm_no_reply: &'static str,
    pub vm_no_choice: &'static str,
    pub vm_decide_callback: &'static str,
    pub vm_length: &'static str,
    pub vm_no_transcript: &'static str,
    pub vm_task_failed: &'static str,
    // Telegram task cards, their buttons and the follow-up message
    pub tc_status: &'static str,
    pub tc_pending: &'static str,
    pub tc_transcript: &'static str,
    pub tc_confirm: &'static str,
    pub tc_ignore: &'static str,
    pub tc_confirm_ack: &'static str,
    pub tc_ignore_ack: &'static str,
    pub tc_task: &'static str,
    pub tc_ignored_task: &'static str,
    pub tc_done: &'static str,
    pub tc_in_progress: &'static str,
    pub tc_failed: &'static str,
    pub tc_confirm_failed: &'static str,
    pub tc_ignore_failed: &'static str,
    // Why a task could not be carried out (`fulfill`, Gmail, Google Calendar).
    // `{}` = the time that was not understood / the error.
    pub f_callback_waiting: &'static str,
    pub f_callback_whitelist: &'static str,
    pub f_callback_no_number: &'static str,
    pub f_callback_send_failed: &'static str,
    pub f_email_no_address: &'static str,
    pub f_gmail_missing: &'static str,
    pub f_gcal_missing: &'static str,
    pub f_gcal_failed: &'static str,
    pub f_bad_time: &'static str,
    pub f_openclaw_failed: &'static str,
    pub g_bad_address: &'static str,
    pub g_no_recipient: &'static str,
    pub g_bad_recipient: &'static str,
    pub g_short_password: &'static str,
    // The e-mail and the calendar event a task sends. `{}` = the caller.
    pub e_subject: &'static str,
    pub e_summary: &'static str,
    pub e_empty: &'static str,
    pub e_appointment: &'static str,
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
    sep: "：",
    list_sep: "、",
    quote: ("「", "」"),
    dt_fmt: "%-m月%-d日 %H:%M",
    date_fmt: "%-m月%-d日",
    u_sec: "{} 秒",
    u_min: "{} 分鐘",
    s_no_service: "（沒有設定摘要服務，沒有摘要）",
    s_see_transcript: "請看網頁逐字稿",
    s_no_transcript: "通話約 {}，沒有可用的逐字稿",
    s_listen_recording: "請到網頁聽錄音",
    s_unclear: "聽不清楚",
    s_ai_fail_matter: "AI 沒接上，約 {}後系統掛斷，對方沒聽到回應",
    s_call_back_if_known: "認得這號碼的話回電",
    s_ai_fail_prefix: "【AI 沒接上，系統直接掛斷】",
    s_stall_topic: "推銷／詐騙電話",
    s_stall_topic_kw: "推銷／詐騙電話（提到：{}）",
    s_stall_reply: "故意陪他聊了約 {}，沒給資料也沒答應",
    s_nothing: "不用",
    vm_call_back_to: "要求回電：{}（已按1確認）",
    vm_call_back_unknown: "要求回電此來電號碼（已按1確認，但號碼未知）",
    vm_email_to: "要求回信：{}（已按1確認）",
    vm_email_unknown: "要求回信（已按1確認，但地址未知）",
    vm_call_back_unchecked: "要求回電（未核對號碼）",
    vm_no_reply: "不需回覆（純轉達）",
    vm_no_choice: "未選擇回覆方式（通話提前結束）",
    vm_decide_callback: "要不要回電請自己確認",
    vm_length: "留言約 {}",
    vm_no_transcript: "留言約 {}，逐字稿失敗，請聽錄音",
    vm_task_failed: "語音留言（轉錄失敗，待人工處理）",
    tc_status: "狀態",
    tc_pending: "待確認",
    tc_transcript: "逐字稿",
    tc_confirm: "確認",
    tc_ignore: "忽略",
    tc_confirm_ack: "已確認，執行中…",
    tc_ignore_ack: "已忽略",
    tc_task: "任務",
    tc_ignored_task: "已忽略任務",
    tc_done: "已完成",
    tc_in_progress: "已確認（進行中）",
    tc_failed: "履行失敗",
    tc_confirm_failed: "確認失敗",
    tc_ignore_failed: "忽略失敗",
    f_callback_waiting: "回撥已排程，但手機的控制連線離線 — 請確認手機已連上 agent 後再按一次確認",
    f_callback_whitelist: "回撥號碼不在白名單 — 請至「設定」加入允許回撥的號碼（或前綴*）後再確認",
    f_callback_no_number: "回撥號碼缺失 — 任務沒有聯絡方式，來電號碼也是空的",
    f_callback_send_failed: "回撥發送失敗",
    f_email_no_address: "此任務要寄信，但沒有回覆信箱 — 請從通話逐字稿補齊聯絡方式，或改忽略",
    f_gmail_missing: "Gmail 憑證未設定 — 請至「設定」分頁填寫 Gmail 帳號與應用程式密碼後再確認",
    f_gcal_missing: "Google 日曆憑證未設定 — 請至「設定」分頁填寫 Client ID / Secret / Refresh Token 後再確認",
    f_gcal_failed: "Google 日曆建立行程失敗",
    f_bad_time: "無法解析時間提示「{}」",
    f_openclaw_failed: "OpenClaw 轉派失敗：{}（請檢查設定中的 OpenClaw URL / Token）",
    g_bad_address: "Gmail 帳號格式不正確 — 請至「設定」分頁檢查 Gmail 帳號",
    g_no_recipient: "寄件失敗：收件信箱是空的",
    g_bad_recipient: "寄件失敗：收件信箱格式不正確",
    g_short_password: "Gmail 應用程式密碼太短 — 請至「設定」分頁重新填寫（不會顯示密碼內容）",
    e_subject: "語音留言回覆",
    e_summary: "摘要",
    e_empty: "（沒有摘要或逐字稿）",
    e_appointment: "預約 — {}",
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
    sep: "：",
    list_sep: "、",
    quote: ("“", "”"),
    dt_fmt: "%-m月%-d日 %H:%M",
    date_fmt: "%-m月%-d日",
    u_sec: "{} 秒",
    u_min: "{} 分钟",
    s_no_service: "（没有设置摘要服务，没有摘要）",
    s_see_transcript: "请看网页通话记录",
    s_no_transcript: "通话约 {}，没有可用的通话记录",
    s_listen_recording: "请到网页听录音",
    s_unclear: "听不清楚",
    s_ai_fail_matter: "AI 没接上，约 {}后系统挂断，对方没听到回应",
    s_call_back_if_known: "认得这个号码的话回电",
    s_ai_fail_prefix: "【AI 没接上，系统直接挂断】",
    s_stall_topic: "推销／诈骗电话",
    s_stall_topic_kw: "推销／诈骗电话（提到：{}）",
    s_stall_reply: "故意陪他聊了约 {}，没给信息也没答应",
    s_nothing: "不用",
    vm_call_back_to: "要求回电：{}（已按1确认）",
    vm_call_back_unknown: "要求回电到这个来电号码（已按1确认，但号码未知）",
    vm_email_to: "要求回邮件：{}（已按1确认）",
    vm_email_unknown: "要求回邮件（已按1确认，但地址未知）",
    vm_call_back_unchecked: "要求回电（未核对号码）",
    vm_no_reply: "不需要回复（只是转达）",
    vm_no_choice: "没有选择回复方式（通话提前结束）",
    vm_decide_callback: "要不要回电请自己确认",
    vm_length: "留言约 {}",
    vm_no_transcript: "留言约 {}，转写失败，请听录音",
    vm_task_failed: "语音留言（转写失败，待人工处理）",
    tc_status: "状态",
    tc_pending: "待确认",
    tc_transcript: "文字记录",
    tc_confirm: "确认",
    tc_ignore: "忽略",
    tc_confirm_ack: "已确认，正在处理…",
    tc_ignore_ack: "已忽略",
    tc_task: "任务",
    tc_ignored_task: "已忽略任务",
    tc_done: "已完成",
    tc_in_progress: "已确认（处理中）",
    tc_failed: "执行失败",
    tc_confirm_failed: "确认失败",
    tc_ignore_failed: "忽略失败",
    f_callback_waiting: "回拨已排好，但手机的控制连接已断开 — 请确认手机已连上 agent 后再点一次确认",
    f_callback_whitelist: "回拨号码不在白名单里 — 请到“设置”添加允许回拨的号码（或前缀*）后再确认",
    f_callback_no_number: "缺少回拨号码 — 任务没有联系方式，来电号码也是空的",
    f_callback_send_failed: "回拨发送失败",
    f_email_no_address: "这个任务要发邮件，但没有回复邮箱 — 请从文字记录补上联系方式，或者忽略",
    f_gmail_missing: "Gmail 凭据未设置 — 请到“设置”填写 Gmail 账号和应用专用密码后再确认",
    f_gcal_missing: "Google 日历凭据未设置 — 请到“设置”填写 Client ID / Secret / Refresh Token 后再确认",
    f_gcal_failed: "Google 日历创建日程失败",
    f_bad_time: "无法解析时间提示“{}”",
    f_openclaw_failed: "OpenClaw 转交失败：{}（请检查设置里的 OpenClaw URL / Token）",
    g_bad_address: "Gmail 账号格式不对 — 请到“设置”检查 Gmail 账号",
    g_no_recipient: "发送失败：收件邮箱是空的",
    g_bad_recipient: "发送失败：收件邮箱格式不对",
    g_short_password: "Gmail 应用专用密码太短 — 请到“设置”重新填写（不会显示密码内容）",
    e_subject: "语音留言回复",
    e_summary: "摘要",
    e_empty: "（没有摘要或文字记录）",
    e_appointment: "预约 — {}",
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
    sep: ": ",
    list_sep: ", ",
    quote: ("\"", "\""),
    dt_fmt: "%b %-d, %H:%M",
    date_fmt: "%b %-d",
    u_sec: "{} s",
    u_min: "{} min",
    s_no_service: "No summary service is set up",
    s_see_transcript: "See the transcript in the web console",
    s_no_transcript: "The call lasted about {}; there is no usable transcript",
    s_listen_recording: "Listen to the recording in the web console",
    s_unclear: "Unclear",
    s_ai_fail_matter: "The AI did not connect; the system hung up after about {} and the caller heard nothing",
    s_call_back_if_known: "Call back if you know the number",
    s_ai_fail_prefix: "[The AI did not connect; the system hung up]",
    s_stall_topic: "Sales / scam call",
    s_stall_topic_kw: "Sales / scam call (mentioned: {})",
    s_stall_reply: "Kept them talking for about {}; gave no data and agreed to nothing",
    s_nothing: "Nothing",
    vm_call_back_to: "Wants a call back: {} (confirmed with 1)",
    vm_call_back_unknown: "Wants a call back on this number (confirmed with 1, but the number is unknown)",
    vm_email_to: "Wants a reply by email: {} (confirmed with 1)",
    vm_email_unknown: "Wants a reply by email (confirmed with 1, but the address is unknown)",
    vm_call_back_unchecked: "Wants a call back (number not checked)",
    vm_no_reply: "No reply needed (message only)",
    vm_no_choice: "No reply method chosen (the call ended early)",
    vm_decide_callback: "Decide whether to call back",
    vm_length: "about {}",
    vm_no_transcript: "A message of about {}; it could not be transcribed. Listen to the recording",
    vm_task_failed: "Voicemail (could not be transcribed; needs a look)",
    tc_status: "Status",
    tc_pending: "Waiting for you",
    tc_transcript: "Transcript",
    tc_confirm: "Confirm",
    tc_ignore: "Ignore",
    tc_confirm_ack: "Confirmed, working on it…",
    tc_ignore_ack: "Ignored",
    tc_task: "Task",
    tc_ignored_task: "Task ignored",
    tc_done: "Done",
    tc_in_progress: "Confirmed (in progress)",
    tc_failed: "Failed",
    tc_confirm_failed: "Could not confirm",
    tc_ignore_failed: "Could not ignore",
    f_callback_waiting: "The call back is scheduled, but the phone's control link is offline. Make sure the phone is connected to the agent, then confirm again.",
    f_callback_whitelist: "The call-back number is not on the allowed list. Add it (or a prefix ending in *) in Settings, then confirm again.",
    f_callback_no_number: "No number to call back: the task has no contact and the caller's number is empty.",
    f_callback_send_failed: "Could not send the call back",
    f_email_no_address: "This task sends an email but has no reply address. Add the contact from the transcript, or ignore the task.",
    f_gmail_missing: "Gmail is not set up. Enter the Gmail address and app password in Settings, then confirm again.",
    f_gcal_missing: "Google Calendar is not set up. Enter the client ID, secret and refresh token in Settings, then confirm again.",
    f_gcal_failed: "Could not create the Google Calendar event",
    f_bad_time: "Could not read the time \"{}\"",
    f_openclaw_failed: "Could not hand the task to OpenClaw: {} (check the OpenClaw URL and token in Settings)",
    g_bad_address: "The Gmail address does not look right. Check it in Settings.",
    g_no_recipient: "Not sent: no recipient address",
    g_bad_recipient: "Not sent: the recipient address does not look right",
    g_short_password: "The Gmail app password is too short. Enter it again in Settings (it is never shown).",
    e_subject: "Reply to your voicemail",
    e_summary: "Summary",
    e_empty: "(No summary or transcript)",
    e_appointment: "Appointment — {}",
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
    sep: "：",
    list_sep: "、",
    quote: ("「", "」"),
    dt_fmt: "%-m月%-d日 %H:%M",
    date_fmt: "%-m月%-d日",
    u_sec: "{}秒",
    u_min: "{}分",
    s_no_service: "要約サービスが設定されていないため、要約はありません",
    s_see_transcript: "Web の文字起こしを確認してください",
    s_no_transcript: "約{}の通話で、使える文字起こしがありません",
    s_listen_recording: "Web で録音を聞いてください",
    s_unclear: "聞き取れず",
    s_ai_fail_matter: "AI が接続できず、約{}後にシステムが切断しました。相手には何も聞こえていません",
    s_call_back_if_known: "心当たりのある番号なら折り返してください",
    s_ai_fail_prefix: "【AI が接続できず、システムが切断】",
    s_stall_topic: "営業／詐欺の電話",
    s_stall_topic_kw: "営業／詐欺の電話（話に出たもの：{}）",
    s_stall_reply: "わざと約{}話を引き延ばしました。情報は渡さず、何も約束していません",
    s_nothing: "なし",
    vm_call_back_to: "折り返し希望：{}（1 で確認済み）",
    vm_call_back_unknown: "この番号への折り返し希望（1 で確認済み、ただし番号不明）",
    vm_email_to: "メールでの返信希望：{}（1 で確認済み）",
    vm_email_unknown: "メールでの返信希望（1 で確認済み、ただしアドレス不明）",
    vm_call_back_unchecked: "折り返し希望（番号は未確認）",
    vm_no_reply: "返信不要（伝言のみ）",
    vm_no_choice: "返信方法の選択なし（通話が途中で終了）",
    vm_decide_callback: "折り返すかどうかはご自身で判断してください",
    vm_length: "約{}",
    vm_no_transcript: "約{}の伝言。文字起こしに失敗したので録音を聞いてください",
    vm_task_failed: "留守番電話（文字起こし失敗、要確認）",
    tc_status: "状態",
    tc_pending: "確認待ち",
    tc_transcript: "文字起こし",
    tc_confirm: "確認",
    tc_ignore: "無視",
    tc_confirm_ack: "確認しました。処理中…",
    tc_ignore_ack: "無視しました",
    tc_task: "タスク",
    tc_ignored_task: "タスクを無視しました",
    tc_done: "完了",
    tc_in_progress: "確認済み（処理中）",
    tc_failed: "失敗",
    tc_confirm_failed: "確認できませんでした",
    tc_ignore_failed: "無視できませんでした",
    f_callback_waiting: "折り返しは予約しましたが、スマホの制御接続がオフラインです。スマホが agent に接続されていることを確かめて、もう一度確認を押してください",
    f_callback_whitelist: "折り返し先の番号が許可リストにありません。「設定」で番号（または * で終わる先頭部分）を追加してから確認してください",
    f_callback_no_number: "折り返し先の番号がありません。タスクに連絡先がなく、着信番号も空です",
    f_callback_send_failed: "折り返しを送れませんでした",
    f_email_no_address: "このタスクはメールを送りますが、返信先アドレスがありません。文字起こしから連絡先を補うか、無視してください",
    f_gmail_missing: "Gmail が設定されていません。「設定」で Gmail アドレスとアプリ パスワードを入力してから確認してください",
    f_gcal_missing: "Google カレンダーが設定されていません。「設定」でクライアント ID・シークレット・リフレッシュトークンを入力してから確認してください",
    f_gcal_failed: "Google カレンダーに予定を作成できませんでした",
    f_bad_time: "時刻「{}」を読み取れませんでした",
    f_openclaw_failed: "OpenClaw に引き渡せませんでした：{}（「設定」の OpenClaw URL とトークンを確認してください）",
    g_bad_address: "Gmail アドレスの形式が正しくありません。「設定」で確認してください",
    g_no_recipient: "送信できません：宛先アドレスが空です",
    g_bad_recipient: "送信できません：宛先アドレスの形式が正しくありません",
    g_short_password: "Gmail のアプリ パスワードが短すぎます。「設定」で入力し直してください（内容は表示されません）",
    e_subject: "留守番電話へのお返事",
    e_summary: "要約",
    e_empty: "（要約も文字起こしもありません）",
    e_appointment: "予約 — {}",
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
    sep: ": ",
    list_sep: ", ",
    quote: ("\"", "\""),
    dt_fmt: "%-m월 %-d일 %H:%M",
    date_fmt: "%-m월 %-d일",
    u_sec: "{}초",
    u_min: "{}분",
    s_no_service: "요약 서비스가 설정되지 않아 요약이 없습니다",
    s_see_transcript: "웹 콘솔에서 대화 기록을 확인하세요",
    s_no_transcript: "약 {} 통화했지만 쓸 수 있는 대화 기록이 없습니다",
    s_listen_recording: "웹 콘솔에서 녹음을 들어 보세요",
    s_unclear: "알아듣기 어려움",
    s_ai_fail_matter: "AI가 연결되지 않아 약 {} 뒤 시스템이 끊었습니다. 상대는 아무 말도 듣지 못했습니다",
    s_call_back_if_known: "아는 번호라면 다시 전화하세요",
    s_ai_fail_prefix: "[AI 연결 실패, 시스템이 끊음]",
    s_stall_topic: "영업/사기 전화",
    s_stall_topic_kw: "영업/사기 전화 (언급: {})",
    s_stall_reply: "일부러 약 {} 동안 붙잡아 두었습니다. 정보는 주지 않았고 아무것도 동의하지 않았습니다",
    s_nothing: "없음",
    vm_call_back_to: "회신 전화 요청: {} (1번으로 확인)",
    vm_call_back_unknown: "이 번호로 회신 전화 요청 (1번으로 확인, 번호는 알 수 없음)",
    vm_email_to: "이메일 회신 요청: {} (1번으로 확인)",
    vm_email_unknown: "이메일 회신 요청 (1번으로 확인, 주소는 알 수 없음)",
    vm_call_back_unchecked: "회신 전화 요청 (번호 확인 안 됨)",
    vm_no_reply: "회신 필요 없음 (전달만)",
    vm_no_choice: "회신 방법 선택 안 함 (통화가 일찍 끝남)",
    vm_decide_callback: "다시 전화할지 직접 판단하세요",
    vm_length: "약 {}",
    vm_no_transcript: "약 {} 길이의 메시지입니다. 받아쓰기에 실패했으니 녹음을 들어 보세요",
    vm_task_failed: "음성 메시지 (받아쓰기 실패, 확인 필요)",
    tc_status: "상태",
    tc_pending: "확인 대기",
    tc_transcript: "대화 기록",
    tc_confirm: "확인",
    tc_ignore: "무시",
    tc_confirm_ack: "확인했습니다. 처리 중…",
    tc_ignore_ack: "무시했습니다",
    tc_task: "작업",
    tc_ignored_task: "작업을 무시했습니다",
    tc_done: "완료",
    tc_in_progress: "확인됨 (진행 중)",
    tc_failed: "실패",
    tc_confirm_failed: "확인하지 못했습니다",
    tc_ignore_failed: "무시하지 못했습니다",
    f_callback_waiting: "회신 전화를 예약했지만 휴대폰 제어 연결이 끊겨 있습니다. 휴대폰이 agent에 연결됐는지 확인한 뒤 다시 확인을 누르세요",
    f_callback_whitelist: "회신할 번호가 허용 목록에 없습니다. 설정에서 번호(또는 *로 끝나는 앞자리)를 추가한 뒤 다시 확인하세요",
    f_callback_no_number: "회신할 번호가 없습니다. 작업에 연락처가 없고 발신 번호도 비어 있습니다",
    f_callback_send_failed: "회신 전화를 보내지 못했습니다",
    f_email_no_address: "이메일을 보내는 작업인데 회신 주소가 없습니다. 대화 기록에서 연락처를 채우거나 무시하세요",
    f_gmail_missing: "Gmail이 설정되지 않았습니다. 설정에서 Gmail 주소와 앱 비밀번호를 입력한 뒤 다시 확인하세요",
    f_gcal_missing: "Google 캘린더가 설정되지 않았습니다. 설정에서 클라이언트 ID, 시크릿, 갱신 토큰을 입력한 뒤 다시 확인하세요",
    f_gcal_failed: "Google 캘린더 일정을 만들지 못했습니다",
    f_bad_time: "시간 \"{}\"을(를) 읽지 못했습니다",
    f_openclaw_failed: "OpenClaw에 넘기지 못했습니다: {} (설정의 OpenClaw URL과 토큰을 확인하세요)",
    g_bad_address: "Gmail 주소 형식이 올바르지 않습니다. 설정에서 확인하세요",
    g_no_recipient: "보내지 못했습니다: 받는 주소가 비어 있습니다",
    g_bad_recipient: "보내지 못했습니다: 받는 주소 형식이 올바르지 않습니다",
    g_short_password: "Gmail 앱 비밀번호가 너무 짧습니다. 설정에서 다시 입력하세요 (내용은 표시되지 않습니다)",
    e_subject: "음성 메시지에 대한 회신",
    e_summary: "요약",
    e_empty: "(요약이나 대화 기록이 없습니다)",
    e_appointment: "예약 — {}",
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
    sep: ": ",
    list_sep: ", ",
    quote: ("«", "»"),
    dt_fmt: "%d/%m %H:%M",
    date_fmt: "%d/%m",
    u_sec: "{} s",
    u_min: "{} min",
    s_no_service: "No hay ningún servicio de resúmenes configurado",
    s_see_transcript: "Consulta la transcripción en la consola web",
    s_no_transcript: "La llamada duró unos {} y no hay transcripción útil",
    s_listen_recording: "Escucha la grabación en la consola web",
    s_unclear: "No se entiende",
    s_ai_fail_matter: "La IA no se conectó; el sistema colgó tras unos {} y la persona no oyó nada",
    s_call_back_if_known: "Devuelve la llamada si conoces el número",
    s_ai_fail_prefix: "[La IA no se conectó; el sistema colgó]",
    s_stall_topic: "Llamada de ventas / estafa",
    s_stall_topic_kw: "Llamada de ventas / estafa (mencionó: {})",
    s_stall_reply: "Lo entretuvo unos {}; no dio datos ni aceptó nada",
    s_nothing: "Nada",
    vm_call_back_to: "Pide que le devuelvas la llamada: {} (confirmado con 1)",
    vm_call_back_unknown: "Pide que le devuelvas la llamada a este número (confirmado con 1, pero el número es desconocido)",
    vm_email_to: "Pide respuesta por correo: {} (confirmado con 1)",
    vm_email_unknown: "Pide respuesta por correo (confirmado con 1, pero la dirección es desconocida)",
    vm_call_back_unchecked: "Pide que le devuelvas la llamada (número sin comprobar)",
    vm_no_reply: "No hace falta responder (solo un recado)",
    vm_no_choice: "No eligió cómo responder (la llamada terminó antes)",
    vm_decide_callback: "Decide si devuelves la llamada",
    vm_length: "unos {}",
    vm_no_transcript: "Un mensaje de unos {}; no se pudo transcribir. Escucha la grabación",
    vm_task_failed: "Mensaje de voz (no se pudo transcribir; hay que revisarlo)",
    tc_status: "Estado",
    tc_pending: "Pendiente de confirmar",
    tc_transcript: "Transcripción",
    tc_confirm: "Confirmar",
    tc_ignore: "Ignorar",
    tc_confirm_ack: "Confirmado, en marcha…",
    tc_ignore_ack: "Ignorado",
    tc_task: "Tarea",
    tc_ignored_task: "Tarea ignorada",
    tc_done: "Hecha",
    tc_in_progress: "Confirmada (en curso)",
    tc_failed: "Falló",
    tc_confirm_failed: "No se pudo confirmar",
    tc_ignore_failed: "No se pudo ignorar",
    f_callback_waiting: "La devolución de llamada está programada, pero la conexión de control del teléfono está desconectada. Comprueba que el teléfono esté conectado al agente y vuelve a confirmar.",
    f_callback_whitelist: "El número para devolver la llamada no está en la lista permitida. Añádelo (o un prefijo terminado en *) en Ajustes y vuelve a confirmar.",
    f_callback_no_number: "No hay número para devolver la llamada: la tarea no tiene contacto y el número de quien llamó está vacío.",
    f_callback_send_failed: "No se pudo enviar la devolución de llamada",
    f_email_no_address: "Esta tarea envía un correo, pero no tiene dirección de respuesta. Añade el contacto a partir de la transcripción o ignora la tarea.",
    f_gmail_missing: "Gmail no está configurado. Escribe la dirección de Gmail y la contraseña de aplicación en Ajustes y vuelve a confirmar.",
    f_gcal_missing: "Google Calendar no está configurado. Escribe el ID de cliente, el secreto y el token de actualización en Ajustes y vuelve a confirmar.",
    f_gcal_failed: "No se pudo crear el evento en Google Calendar",
    f_bad_time: "No se pudo entender la hora «{}»",
    f_openclaw_failed: "No se pudo pasar la tarea a OpenClaw: {} (revisa la URL y el token de OpenClaw en Ajustes)",
    g_bad_address: "La dirección de Gmail no parece correcta. Revísala en Ajustes.",
    g_no_recipient: "No se envió: falta la dirección del destinatario",
    g_bad_recipient: "No se envió: la dirección del destinatario no parece correcta",
    g_short_password: "La contraseña de aplicación de Gmail es demasiado corta. Vuelve a escribirla en Ajustes (nunca se muestra).",
    e_subject: "Respuesta a su mensaje de voz",
    e_summary: "Resumen",
    e_empty: "(Sin resumen ni transcripción)",
    e_appointment: "Cita — {}",
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
    sep: " : ",
    list_sep: ", ",
    quote: ("« ", " »"),
    dt_fmt: "%d/%m %H:%M",
    date_fmt: "%d/%m",
    u_sec: "{} s",
    u_min: "{} min",
    s_no_service: "Aucun service de résumé n'est configuré",
    s_see_transcript: "Voir la transcription dans la console web",
    s_no_transcript: "L'appel a duré environ {} et aucune transcription n'est exploitable",
    s_listen_recording: "Écouter l'enregistrement dans la console web",
    s_unclear: "Pas clair",
    s_ai_fail_matter: "L'IA ne s'est pas connectée ; le système a raccroché après environ {} et l'appelant n'a rien entendu",
    s_call_back_if_known: "Rappeler si vous connaissez le numéro",
    s_ai_fail_prefix: "[L'IA ne s'est pas connectée ; le système a raccroché]",
    s_stall_topic: "Démarchage / arnaque",
    s_stall_topic_kw: "Démarchage / arnaque (a parlé de : {})",
    s_stall_reply: "L'a fait parler environ {} ; aucune donnée transmise, rien accepté",
    s_nothing: "Rien",
    vm_call_back_to: "Souhaite être rappelé : {} (confirmé avec 1)",
    vm_call_back_unknown: "Souhaite être rappelé à ce numéro (confirmé avec 1, mais le numéro est inconnu)",
    vm_email_to: "Souhaite une réponse par e-mail : {} (confirmé avec 1)",
    vm_email_unknown: "Souhaite une réponse par e-mail (confirmé avec 1, mais l'adresse est inconnue)",
    vm_call_back_unchecked: "Souhaite être rappelé (numéro non vérifié)",
    vm_no_reply: "Pas de réponse nécessaire (simple message)",
    vm_no_choice: "Aucun moyen de réponse choisi (l'appel s'est terminé avant)",
    vm_decide_callback: "À vous de voir s'il faut rappeler",
    vm_length: "environ {}",
    vm_no_transcript: "Un message d'environ {} ; la transcription a échoué. Écoutez l'enregistrement",
    vm_task_failed: "Message vocal (transcription impossible, à vérifier)",
    tc_status: "État",
    tc_pending: "En attente de confirmation",
    tc_transcript: "Transcription",
    tc_confirm: "Confirmer",
    tc_ignore: "Ignorer",
    tc_confirm_ack: "Confirmé, en cours…",
    tc_ignore_ack: "Ignoré",
    tc_task: "Tâche",
    tc_ignored_task: "Tâche ignorée",
    tc_done: "Terminée",
    tc_in_progress: "Confirmée (en cours)",
    tc_failed: "Échec",
    tc_confirm_failed: "Confirmation impossible",
    tc_ignore_failed: "Impossible d'ignorer",
    f_callback_waiting: "Le rappel est programmé, mais la liaison de contrôle du téléphone est hors ligne. Vérifiez que le téléphone est connecté à l'agent, puis confirmez à nouveau.",
    f_callback_whitelist: "Le numéro à rappeler n'est pas dans la liste autorisée. Ajoutez-le (ou un préfixe terminé par *) dans les réglages, puis confirmez à nouveau.",
    f_callback_no_number: "Aucun numéro à rappeler : la tâche n'a pas de contact et le numéro de l'appelant est vide.",
    f_callback_send_failed: "Impossible d'envoyer le rappel",
    f_email_no_address: "Cette tâche envoie un e-mail mais n'a pas d'adresse de réponse. Ajoutez le contact à partir de la transcription, ou ignorez la tâche.",
    f_gmail_missing: "Gmail n'est pas configuré. Saisissez l'adresse Gmail et le mot de passe d'application dans les réglages, puis confirmez à nouveau.",
    f_gcal_missing: "Google Agenda n'est pas configuré. Saisissez l'ID client, le secret et le jeton d'actualisation dans les réglages, puis confirmez à nouveau.",
    f_gcal_failed: "Impossible de créer l'événement dans Google Agenda",
    f_bad_time: "Impossible de comprendre l'heure « {} »",
    f_openclaw_failed: "Impossible de transmettre la tâche à OpenClaw : {} (vérifiez l'URL et le jeton OpenClaw dans les réglages)",
    g_bad_address: "L'adresse Gmail ne semble pas correcte. Vérifiez-la dans les réglages.",
    g_no_recipient: "Non envoyé : aucune adresse de destinataire",
    g_bad_recipient: "Non envoyé : l'adresse du destinataire ne semble pas correcte",
    g_short_password: "Le mot de passe d'application Gmail est trop court. Saisissez-le à nouveau dans les réglages (il n'est jamais affiché).",
    e_subject: "Réponse à votre message vocal",
    e_summary: "Résumé",
    e_empty: "(Ni résumé ni transcription)",
    e_appointment: "Rendez-vous — {}",
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
    sep: ": ",
    list_sep: ", ",
    quote: ("„", "“"),
    dt_fmt: "%d.%m. %H:%M",
    date_fmt: "%d.%m.",
    u_sec: "{} s",
    u_min: "{} Min.",
    s_no_service: "Es ist kein Zusammenfassungsdienst eingerichtet",
    s_see_transcript: "Transkript in der Web-Konsole ansehen",
    s_no_transcript: "Der Anruf dauerte etwa {}; es gibt kein brauchbares Transkript",
    s_listen_recording: "Aufnahme in der Web-Konsole anhören",
    s_unclear: "Unklar",
    s_ai_fail_matter: "Die KI konnte sich nicht verbinden; das System hat nach etwa {} aufgelegt, der Anrufer hat nichts gehört",
    s_call_back_if_known: "Zurückrufen, wenn du die Nummer kennst",
    s_ai_fail_prefix: "[KI nicht verbunden; das System hat aufgelegt]",
    s_stall_topic: "Werbe- / Betrugsanruf",
    s_stall_topic_kw: "Werbe- / Betrugsanruf (erwähnt: {})",
    s_stall_reply: "Etwa {} hingehalten; keine Daten gegeben, nichts zugesagt",
    s_nothing: "Nichts",
    vm_call_back_to: "Möchte zurückgerufen werden: {} (mit 1 bestätigt)",
    vm_call_back_unknown: "Möchte unter dieser Nummer zurückgerufen werden (mit 1 bestätigt, Nummer aber unbekannt)",
    vm_email_to: "Möchte eine Antwort per E-Mail: {} (mit 1 bestätigt)",
    vm_email_unknown: "Möchte eine Antwort per E-Mail (mit 1 bestätigt, Adresse aber unbekannt)",
    vm_call_back_unchecked: "Möchte zurückgerufen werden (Nummer nicht geprüft)",
    vm_no_reply: "Keine Antwort nötig (nur zur Info)",
    vm_no_choice: "Keine Antwortart gewählt (der Anruf endete vorher)",
    vm_decide_callback: "Selbst entscheiden, ob du zurückrufst",
    vm_length: "etwa {}",
    vm_no_transcript: "Eine Nachricht von etwa {}; sie konnte nicht transkribiert werden. Bitte die Aufnahme anhören",
    vm_task_failed: "Sprachnachricht (Transkription fehlgeschlagen, bitte ansehen)",
    tc_status: "Status",
    tc_pending: "Wartet auf Bestätigung",
    tc_transcript: "Transkript",
    tc_confirm: "Bestätigen",
    tc_ignore: "Ignorieren",
    tc_confirm_ack: "Bestätigt, wird erledigt…",
    tc_ignore_ack: "Ignoriert",
    tc_task: "Aufgabe",
    tc_ignored_task: "Aufgabe ignoriert",
    tc_done: "Erledigt",
    tc_in_progress: "Bestätigt (läuft)",
    tc_failed: "Fehlgeschlagen",
    tc_confirm_failed: "Bestätigen fehlgeschlagen",
    tc_ignore_failed: "Ignorieren fehlgeschlagen",
    f_callback_waiting: "Der Rückruf ist geplant, aber die Steuerverbindung des Telefons ist offline. Prüfe, ob das Telefon mit dem Agenten verbunden ist, und bestätige dann erneut.",
    f_callback_whitelist: "Die Rückrufnummer steht nicht auf der erlaubten Liste. Füge sie (oder einen Anfang mit * am Ende) in den Einstellungen hinzu und bestätige dann erneut.",
    f_callback_no_number: "Keine Rückrufnummer: Die Aufgabe hat keinen Kontakt und die Nummer des Anrufers ist leer.",
    f_callback_send_failed: "Rückruf konnte nicht gesendet werden",
    f_email_no_address: "Diese Aufgabe sendet eine E-Mail, hat aber keine Antwortadresse. Ergänze den Kontakt aus dem Transkript oder ignoriere die Aufgabe.",
    f_gmail_missing: "Gmail ist nicht eingerichtet. Gib in den Einstellungen die Gmail-Adresse und das App-Passwort ein und bestätige dann erneut.",
    f_gcal_missing: "Google Kalender ist nicht eingerichtet. Gib in den Einstellungen Client-ID, Secret und Refresh-Token ein und bestätige dann erneut.",
    f_gcal_failed: "Der Termin in Google Kalender konnte nicht angelegt werden",
    f_bad_time: "Die Zeitangabe „{}“ war nicht lesbar",
    f_openclaw_failed: "Übergabe an OpenClaw fehlgeschlagen: {} (OpenClaw-URL und -Token in den Einstellungen prüfen)",
    g_bad_address: "Die Gmail-Adresse sieht nicht richtig aus. Bitte in den Einstellungen prüfen.",
    g_no_recipient: "Nicht gesendet: keine Empfängeradresse",
    g_bad_recipient: "Nicht gesendet: Die Empfängeradresse sieht nicht richtig aus",
    g_short_password: "Das Gmail-App-Passwort ist zu kurz. Bitte in den Einstellungen neu eingeben (es wird nie angezeigt).",
    e_subject: "Antwort auf Ihre Sprachnachricht",
    e_summary: "Zusammenfassung",
    e_empty: "(Keine Zusammenfassung und kein Transkript)",
    e_appointment: "Termin — {}",
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
    sep: ": ",
    list_sep: ", ",
    quote: ("\"", "\""),
    dt_fmt: "%d/%m %H:%M",
    date_fmt: "%d/%m",
    u_sec: "{} s",
    u_min: "{} min",
    s_no_service: "Nenhum serviço de resumo configurado",
    s_see_transcript: "Veja a transcrição no console web",
    s_no_transcript: "A chamada durou cerca de {} e não há transcrição utilizável",
    s_listen_recording: "Ouça a gravação no console web",
    s_unclear: "Não deu para entender",
    s_ai_fail_matter: "A IA não conectou; o sistema desligou depois de cerca de {} e a pessoa não ouviu nada",
    s_call_back_if_known: "Retorne a ligação se conhecer o número",
    s_ai_fail_prefix: "[A IA não conectou; o sistema desligou]",
    s_stall_topic: "Ligação de vendas / golpe",
    s_stall_topic_kw: "Ligação de vendas / golpe (mencionou: {})",
    s_stall_reply: "Prendeu a pessoa na linha por cerca de {}; não deu dados nem concordou com nada",
    s_nothing: "Nada",
    vm_call_back_to: "Pede retorno da ligação: {} (confirmado com 1)",
    vm_call_back_unknown: "Pede retorno para este número (confirmado com 1, mas o número é desconhecido)",
    vm_email_to: "Pede resposta por e-mail: {} (confirmado com 1)",
    vm_email_unknown: "Pede resposta por e-mail (confirmado com 1, mas o endereço é desconhecido)",
    vm_call_back_unchecked: "Pede retorno da ligação (número não conferido)",
    vm_no_reply: "Não precisa responder (só um recado)",
    vm_no_choice: "Não escolheu como receber a resposta (a chamada terminou antes)",
    vm_decide_callback: "Decida se vai retornar a ligação",
    vm_length: "cerca de {}",
    vm_no_transcript: "Uma mensagem de cerca de {}; não foi possível transcrever. Ouça a gravação",
    vm_task_failed: "Mensagem de voz (não foi possível transcrever; precisa de atenção)",
    tc_status: "Status",
    tc_pending: "Aguardando confirmação",
    tc_transcript: "Transcrição",
    tc_confirm: "Confirmar",
    tc_ignore: "Ignorar",
    tc_confirm_ack: "Confirmado, em andamento…",
    tc_ignore_ack: "Ignorado",
    tc_task: "Tarefa",
    tc_ignored_task: "Tarefa ignorada",
    tc_done: "Concluída",
    tc_in_progress: "Confirmada (em andamento)",
    tc_failed: "Falhou",
    tc_confirm_failed: "Não foi possível confirmar",
    tc_ignore_failed: "Não foi possível ignorar",
    f_callback_waiting: "O retorno está agendado, mas a conexão de controle do telefone está offline. Verifique se o telefone está conectado ao agente e confirme de novo.",
    f_callback_whitelist: "O número de retorno não está na lista permitida. Adicione-o (ou um prefixo terminado em *) nas Configurações e confirme de novo.",
    f_callback_no_number: "Não há número para retornar: a tarefa não tem contato e o número de quem ligou está vazio.",
    f_callback_send_failed: "Não foi possível enviar o retorno",
    f_email_no_address: "Esta tarefa envia um e-mail, mas não tem endereço de resposta. Complete o contato a partir da transcrição ou ignore a tarefa.",
    f_gmail_missing: "O Gmail não está configurado. Informe o endereço do Gmail e a senha de app nas Configurações e confirme de novo.",
    f_gcal_missing: "O Google Agenda não está configurado. Informe o ID do cliente, o segredo e o token de atualização nas Configurações e confirme de novo.",
    f_gcal_failed: "Não foi possível criar o evento no Google Agenda",
    f_bad_time: "Não foi possível entender o horário \"{}\"",
    f_openclaw_failed: "Não foi possível passar a tarefa para o OpenClaw: {} (confira a URL e o token do OpenClaw nas Configurações)",
    g_bad_address: "O endereço do Gmail não parece certo. Confira nas Configurações.",
    g_no_recipient: "Não enviado: falta o endereço do destinatário",
    g_bad_recipient: "Não enviado: o endereço do destinatário não parece certo",
    g_short_password: "A senha de app do Gmail é curta demais. Digite de novo nas Configurações (ela nunca é exibida).",
    e_subject: "Resposta à sua mensagem de voz",
    e_summary: "Resumo",
    e_empty: "(Sem resumo nem transcrição)",
    e_appointment: "Compromisso — {}",
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

    #[test]
    fn dates_and_lengths_follow_the_language() {
        let t = chrono::NaiveDate::from_ymd_opt(2026, 11, 3)
            .unwrap()
            .and_hms_opt(8, 9, 0)
            .unwrap();
        assert_eq!(date_time(Lang::En, &t), "Nov 3, 08:09");
        assert_eq!(date_time(Lang::ZhTw, &t), "11月3日 08:09");
        assert_eq!(date_time(Lang::Ko, &t), "11월 3일 08:09");
        assert_eq!(date_time(Lang::De, &t), "03.11. 08:09");
        assert_eq!(date_time(Lang::Fr, &t), "03/11 08:09");
        assert_eq!(date_only(Lang::Ja, &t.date()), "11月3日");
        assert_eq!(date_only(Lang::En, &t.date()), "Nov 3");
        for l in ALL {
            let s = date_time(l, &t);
            assert!(!s.contains('%') && s.contains("08:09") && s.contains('3'), "{l:?}: {s}");
        }
        assert_eq!(duration(Lang::ZhTw, 71.9), "72 秒");
        assert_eq!(duration(Lang::ZhTw, 600.0), "10 分鐘");
        assert_eq!(duration(Lang::En, 45.0), "45 s");
        assert_eq!(duration(Lang::De, 300.0), "5 Min.");
        assert_eq!(duration(Lang::Ja, 30.0), "30秒");
        assert_eq!(duration(Lang::En, f64::NAN), "0 s");
        assert_eq!(quoted(Lang::ZhTw, "你好"), "「你好」");
    }

    #[test]
    fn every_template_has_exactly_one_slot() {
        for l in ALL {
            let p = phrases(l);
            for t in [
                p.u_sec, p.u_min, p.s_no_transcript, p.s_ai_fail_matter, p.s_stall_topic_kw,
                p.s_stall_reply, p.vm_call_back_to, p.vm_email_to, p.vm_length, p.vm_no_transcript,
                p.f_bad_time, p.f_openclaw_failed, p.e_appointment,
            ] {
                assert_eq!(t.matches("{}").count(), 1, "{l:?}: {t}");
            }
            for t in [
                p.s_no_service, p.s_see_transcript, p.s_listen_recording, p.s_ai_fail_prefix,
                p.s_stall_topic, p.vm_call_back_unknown, p.tc_confirm, p.tc_ignore, p.tc_confirm_ack,
                p.f_callback_waiting, p.f_gmail_missing, p.f_gcal_missing, p.g_short_password,
                p.e_subject, p.e_empty,
            ] {
                assert!(!t.contains('{') && !t.trim().is_empty(), "{l:?}: {t}");
            }
            assert!(p.sep.contains(':') || p.sep.contains('：'), "{l:?}");
            assert_eq!(fill(p.vm_call_back_to, "0912").matches("0912").count(), 1, "{l:?}");
        }
    }
}
