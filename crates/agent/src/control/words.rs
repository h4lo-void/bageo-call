// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! What the chat replies say, in the owner's notification language (all nine).
//!
//! [render] turns an action's structured result ([super::Outcome]) into the
//! text an adapter shows; the menu doubles as the platform's command list.

use crate::locale::Lang;

use super::command::COMMANDS;
use super::Outcome;

/// The phrases of one language. Templates fill {1}, {2}, {3} in order ([fmt]).
pub struct Words {
    /// One entry per [COMMANDS] entry, in that order: (arguments, what it does).
    pub menu: [(&'static str, &'static str); 18],
    pub help_head: &'static str,
    pub help_free: &'static str,
    pub hint: &'static str,
    pub usage: &'static str,
    pub phones: &'static str,
    pub no_phones: &'static str,
    pub mode_ai: &'static str,
    pub mode_normal: &'static str,
    pub offline: &'static str,
    pub busy_line: &'static str,
    pub live: &'static str,
    pub calls_head: &'static str,
    pub no_calls: &'static str,
    pub hidden: &'static str,
    pub no_summary: &'static str,
    pub summary: &'static str,
    pub transcript: &'static str,
    pub open: &'static str,
    pub tasks_head: &'static str,
    pub no_tasks: &'static str,
    pub tasks_how: &'static str,
    pub errands_head: &'static str,
    pub no_errands: &'static str,
    pub contacts_head: &'static str,
    pub no_contacts: &'static str,
    pub started: &'static str,
    pub cancelled: &'static str,
    pub said: &'static str,
    pub answered: &'static str,
    pub mode_done: &'static str,
    pub mode_failed: &'static str,
    /// offline, unsupported, in_call, timeout.
    pub mode_why: [&'static str; 4],
    pub ask_errand: &'static str,
    pub ask_mode: &'static str,
    pub ask_confirm: &'static str,
    pub ask_ignore: &'static str,
    pub ask_say: &'static str,
    pub ask_answer: &'static str,
    pub confirm_how: &'static str,
    pub yes: &'static str,
    pub no: &'static str,
    pub expired: &'static str,
    pub e_decided: &'static str,
    pub dropped: &'static str,
    pub too_many: &'static str,
    pub question: &'static str,
    pub question_how: &'static str,
    pub closed: &'static str,
    pub not_found: &'static str,
    pub no_contact: &'static str,
    pub many_contacts: &'static str,
    pub failed: &'static str,
    pub e_bad_number: &'static str,
    pub e_not_allowed: &'static str,
    pub e_no_phone: &'static str,
    pub e_no_sim: &'static str,
    pub e_no_task: &'static str,
    pub e_too_long: &'static str,
    pub e_not_on_call: &'static str,
    pub e_busy: &'static str,
    pub list_blocked: &'static str,
    pub list_stall: &'static str,
    pub list_white: &'static str,
    pub list_unblocked: &'static str,
}

/// {1}, {2}, {3} of a template, in order.
pub fn fmt(template: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            if let Some(rel_end) = bytes[i + 1..].iter().position(|&b| b == b'}') {
                let inner = &template[i + 1..i + 1 + rel_end];
                if !inner.is_empty() && inner.bytes().all(|b| b.is_ascii_digit()) {
                    if let Ok(idx) = inner.parse::<usize>() {
                        if let Some(arg) = idx.checked_sub(1).and_then(|k| args.get(k)) {
                            out.push_str(arg);
                            i += rel_end + 2;
                            continue;
                        }
                    }
                }
            }
        }
        let ch = template[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

pub fn words(lang: Lang) -> &'static Words {
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

/// The command list for a platform's menu: (command, what it does).
pub fn menu(w: &Words) -> Vec<(&'static str, &'static str)> {
    COMMANDS.iter().zip(w.menu.iter()).map(|(c, (_, what))| (*c, *what)).collect()
}

/// "/errand <number or contact> <what to get done>".
pub fn usage_line(w: &Words, command: &str) -> String {
    match COMMANDS.iter().position(|c| *c == command) {
        Some(i) => format!("/{command} {}", w.menu[i].0).trim_end().to_string(),
        None => format!("/{command}"),
    }
}

fn help(w: &Words) -> String {
    let mut s = String::from(w.help_head);
    for (i, c) in COMMANDS.iter().enumerate() {
        let (args, what) = w.menu[i];
        let head = format!("/{c} {args}");
        s.push_str(&format!("\n{} — {what}", head.trim_end()));
    }
    s.push_str("\n\n");
    s.push_str(w.help_free);
    s
}

fn mode_label(w: &Words, mode: &str) -> &'static str {
    if mode == "normal" {
        w.mode_normal
    } else {
        w.mode_ai
    }
}

/// "王小明 (0912345678)", the number alone, or "hidden number".
pub fn person(w: &Words, who: &str, number: &str) -> String {
    let who = who.trim();
    let number = number.trim();
    match (who.is_empty(), number.is_empty()) {
        (true, true) => w.hidden.to_string(),
        (true, false) => number.to_string(),
        (false, true) => who.to_string(),
        (false, false) if who == number => who.to_string(),
        (false, false) => format!("{who} ({number})"),
    }
}

/// The text for an error code of an action (the codes of the web API).
pub fn error_text(w: &Words, code: &str) -> String {
    match code {
        "bad_number" | "no_number" => w.e_bad_number.to_string(),
        "not_allowed" => w.e_not_allowed.to_string(),
        "no_phone" => w.e_no_phone.to_string(),
        "no_sim_for_country" | "sim_unknown" => w.e_no_sim.to_string(),
        "no_task" | "empty_text" => w.e_no_task.to_string(),
        "task_too_long" | "text_too_long" => w.e_too_long.to_string(),
        "not_on_call" => w.e_not_on_call.to_string(),
        "busy" => w.e_busy.to_string(),
        "question_closed" => w.closed.to_string(),
        "already_decided" => w.e_decided.to_string(),
        other => fmt(w.failed, &[other]),
    }
}

/// The reply text of an action's result.
pub fn render(o: &Outcome, w: &Words) -> String {
    match o {
        Outcome::Help => help(w),
        Outcome::Hint => w.hint.to_string(),
        Outcome::Usage { command } => fmt(w.usage, &[&usage_line(w, command)]),
        Outcome::Status { version, phones, live_calls } => {
            let mut s = format!("Bageo {version}\n{}\n\n{}", fmt(w.live, &[&live_calls.to_string()]), w.phones);
            if phones.is_empty() {
                s.push('\n');
                s.push_str(w.no_phones);
            }
            for (i, p) in phones.iter().enumerate() {
                let state = if !p.connected {
                    w.offline.to_string()
                } else if p.in_call {
                    format!("{} · {}", mode_label(w, &p.mode), w.busy_line)
                } else {
                    mode_label(w, &p.mode).to_string()
                };
                s.push_str(&format!("\n{}. {} — {state}", i + 1, p.label));
            }
            s
        }
        Outcome::Calls { calls } => {
            if calls.is_empty() {
                return w.no_calls.to_string();
            }
            let mut s = String::from(w.calls_head);
            for (i, c) in calls.iter().enumerate() {
                let summary = if c.summary.is_empty() { w.no_summary } else { c.summary.as_str() };
                s.push_str(&format!("\n{}. {} · {} · {summary}", i + 1, c.when, person(w, &c.who, &c.number)));
            }
            s
        }
        Outcome::Call { call } => {
            let mut s = format!("{} · {}\n\n{}\n", call.when, person(w, &call.who, &call.number), w.summary);
            s.push_str(if call.summary.is_empty() { w.no_summary } else { call.summary.as_str() });
            if !call.transcript.is_empty() {
                s.push_str(&format!("\n\n{}\n{}", w.transcript, call.transcript));
            }
            if !call.link.is_empty() {
                s.push_str(&format!("\n\n{}: {}", w.open, call.link));
            }
            s
        }
        Outcome::Tasks { tasks } => {
            if tasks.is_empty() {
                return w.no_tasks.to_string();
            }
            let mut s = String::from(w.tasks_head);
            for (i, t) in tasks.iter().enumerate() {
                s.push_str(&format!("\n{}. {} · {}", i + 1, person(w, &t.who, &t.number), t.summary));
            }
            s.push_str("\n\n");
            s.push_str(w.tasks_how);
            s
        }
        Outcome::Decided { text, .. } => text.clone(),
        Outcome::ErrandStarted { errand } => fmt(w.started, &[&person(w, &errand.who, &errand.number), &errand.state]),
        Outcome::Errands { errands } => {
            if errands.is_empty() {
                return w.no_errands.to_string();
            }
            let mut s = String::from(w.errands_head);
            for (i, e) in errands.iter().enumerate() {
                s.push_str(&format!("\n{}. {} — {}", i + 1, person(w, &e.who, &e.number), e.state));
            }
            s
        }
        Outcome::ErrandStopped { errand } => fmt(w.cancelled, &[&person(w, &errand.who, &errand.number)]),
        Outcome::Said => w.said.to_string(),
        Outcome::Answered => w.answered.to_string(),
        Outcome::ModeSet { phone, mode } => fmt(w.mode_done, &[phone, mode_label(w, mode)]),
        Outcome::ModeFailed { phone, reason } => {
            let why = match reason.as_str() {
                "offline" => w.mode_why[0].to_string(),
                "unsupported" => w.mode_why[1].to_string(),
                "in_call" => w.mode_why[2].to_string(),
                "timeout" => w.mode_why[3].to_string(),
                other => other.to_string(),
            };
            fmt(w.mode_failed, &[phone, &why])
        }
        Outcome::Contacts { contacts } => {
            if contacts.is_empty() {
                return w.no_contacts.to_string();
            }
            let mut s = String::from(w.contacts_head);
            for (i, c) in contacts.iter().enumerate() {
                s.push_str(&format!("\n{}. {} — {}", i + 1, c.name, c.phones.join(", ")));
            }
            s
        }
        Outcome::Confirm { code, summary } => {
            let shown: String = code.chars().take(super::confirm::CODE_SHOWN).collect();
            format!("{summary}\n\n{}", fmt(w.confirm_how, &[&shown]))
        }
        Outcome::Dropped => w.dropped.to_string(),
        Outcome::Expired => w.expired.to_string(),
        Outcome::TooMany { limit } => fmt(w.too_many, &[&limit.to_string()]),
        Outcome::Closed => w.closed.to_string(),
        Outcome::NotFound { what } => fmt(w.not_found, &[what]),
        Outcome::NoContact { what } => fmt(w.no_contact, &[what]),
        Outcome::ManyContacts { names } => fmt(w.many_contacts, &[&names.join(", ")]),
        Outcome::ContactKindSet { list, number, name } => {
            let who = person(w, name, number);
            match list.as_str() {
                "blacklist" => fmt(w.list_blocked, &[&who]),
                "stall" => fmt(w.list_stall, &[&who]),
                "whitelist" => fmt(w.list_white, &[&who]),
                _ => fmt(w.list_unblocked, &[&who]),
            }
        }
        Outcome::Failed { code } => error_text(w, code),
    }
}

/// The confirmation question for an errand: who (name and number, or the
/// number alone) and the task.
pub fn ask_errand(w: &Words, who: &str, number: &str, task: &str) -> String {
    fmt(w.ask_errand, &[&person(w, who, number), task])
}

pub fn ask_mode(w: &Words, phone: &str, mode: &str) -> String {
    fmt(w.ask_mode, &[phone, mode_label(w, mode)])
}

const EN: Words = Words {
    menu: [
        ("", "List the commands"),
        ("", "Bageo version, phones and calls in progress"),
        ("[n] [today|yesterday|YYYY-MM-DD] [missed|scam] [word or number]", "Recent calls"),
        ("<#>", "One call: summary, transcript excerpt, link"),
        ("", "To-dos waiting for you"),
        ("<#>", "Accept a to-do"),
        ("<#>", "Ignore a to-do"),
        ("<number or contact> <what to get done>", "Have the AI make a call for you"),
        ("", "AI calls and their results"),
        ("[#]", "Stop an AI call"),
        ("[#] <text>", "Tell the AI something during its call"),
        ("<text>", "Answer the question the AI asked you"),
        ("<phone> ai|normal", "Switch a phone between AI answering and normal phone"),
        ("<name>", "Find a contact"),
        ("<number> [name/reason]", "Add a number to the blacklist"),
        ("<number> [name/note]", "Add a number to the stall list"),
        ("<number> [name/note]", "Add a number to the whitelist"),
        ("<number>", "Remove a number from the blacklist / stall list"),
    ],
    help_head: "Bageo from this chat. Commands:",
    help_free: "You can also just write what you want, e.g. “call 0912345678 and book a table for two at 7 tonight” or “who called yesterday?”. Calls, mode switches and to-do decisions wait for your OK.",
    hint: "I didn't get that. Send /help for the commands.",
    usage: "Use: {1}",
    phones: "Phones",
    no_phones: "No phone has connected since Bageo started.",
    mode_ai: "AI answers",
    mode_normal: "normal phone",
    offline: "offline",
    busy_line: "in a call",
    live: "Calls in progress: {1}",
    calls_head: "Calls",
    no_calls: "No calls match.",
    hidden: "Hidden number",
    no_summary: "(no summary yet)",
    summary: "Summary",
    transcript: "Transcript (excerpt)",
    open: "Open in the console",
    tasks_head: "To-dos waiting for you",
    no_tasks: "Nothing is waiting for you.",
    tasks_how: "/confirm # or /ignore # decides one.",
    errands_head: "AI calls",
    no_errands: "No AI calls yet.",
    contacts_head: "Contacts",
    no_contacts: "No contact matches.",
    started: "AI call to {1}: {2}",
    cancelled: "Stopped: {1}",
    said: "Passed to the AI on the call.",
    answered: "Your answer went to the AI.",
    mode_done: "{1}: {2}",
    mode_failed: "{1} was not switched: {2}",
    mode_why: ["the phone is offline", "its app cannot switch", "it is in a call", "the phone did not answer (it may still switch)"],
    ask_errand: "Have the AI call {1} to:\n{2}",
    ask_mode: "Switch {1} to “{2}”?",
    ask_confirm: "Accept this to-do?\n{1}",
    ask_ignore: "Ignore this to-do?\n{1}",
    ask_say: "Tell the AI on the call:\n“{1}”?",
    ask_answer: "Answer the AI's question:\n“{1}”?",
    confirm_how: "Reply 1 to go ahead or 2 to drop it, within 2 minutes (code {1}).",
    yes: "Go ahead",
    no: "Drop",
    expired: "That confirmation has expired or was already used. Send the command again.",
    e_decided: "That to-do was already decided elsewhere; nothing was changed.",
    dropped: "Dropped. Nothing was done.",
    too_many: "This chat has started {1} AI calls in the last hour, the limit. Try later or use the web console.",
    question: "The AI on the call to {1} asks you:\n{2}",
    question_how: "Tap an answer or reply to this message within {1} seconds.",
    closed: "Too late: that question is closed (the AI has gone on without it).",
    not_found: "Nothing matches “{1}”. /calls, /tasks and /errands show the numbers to use.",
    no_contact: "No contact or number matches “{1}”.",
    many_contacts: "Several contacts match: {1}. Write the number or the full name.",
    failed: "That didn't work ({1}).",
    e_bad_number: "That is not a number the AI can dial.",
    e_not_allowed: "Emergency, short and premium-rate numbers are never dialed.",
    e_no_phone: "No phone in AI mode is connected to make this call.",
    e_no_sim: "No connected phone has a SIM of that number's country (or its SIMs aren't known yet).",
    e_no_task: "Say what the AI should get done on the call.",
    e_too_long: "That text is too long.",
    e_not_on_call: "That AI call is not on the line now.",
    e_busy: "That AI call is still under way.",
    list_blocked: "Added to blacklist: {1}",
    list_stall: "Added to the stall list: {1}",
    list_white: "Added to whitelist: {1}",
    list_unblocked: "Restored to normal contacts: {1}",
};

const ZH_TW: Words = Words {
    menu: [
        ("", "列出指令"),
        ("", "Bageo 版本、手機狀態、進行中的通話"),
        ("[筆數] [today|yesterday|YYYY-MM-DD] [missed|scam] [關鍵字或號碼]", "最近的通話"),
        ("<#>", "某一通：摘要、逐字稿節錄、連結"),
        ("", "等你處理的待辦"),
        ("<#>", "同意待辦"),
        ("<#>", "忽略待辦"),
        ("<號碼或聯絡人> <要辦的事>", "叫 AI 幫你打電話"),
        ("", "AI 代打的電話與結果"),
        ("[#]", "停止 AI 代打"),
        ("[#] <內容>", "通話中交代 AI 一句話"),
        ("<內容>", "回答 AI 問你的問題"),
        ("<手機> ai|normal", "把手機切到 AI 接聽或一般手機"),
        ("<名字>", "找聯絡人"),
        ("<號碼> [名稱/原因]", "加入黑名單（來電拒接）"),
        ("<號碼> [名稱/備註]", "加入陪聊名單（一接起就陪聊）"),
        ("<號碼> [名稱/備註]", "加入白名單（重要聯絡人）"),
        ("<號碼>", "解除黑名單／陪聊名單"),
    ],
    help_head: "在聊天室操作 Bageo。指令：",
    help_free: "也可以直接打字，例如「幫我打給 0912345678 訂今晚七點兩位」或「昨天有誰打來」。撥電話、切換手機模式、處理待辦都會先問你確認。",
    hint: "看不懂這句。傳 /help 看指令。",
    usage: "用法：{1}",
    phones: "手機",
    no_phones: "Bageo 啟動後還沒有手機連上。",
    mode_ai: "AI 接聽",
    mode_normal: "一般手機",
    offline: "離線",
    busy_line: "通話中",
    live: "進行中的通話：{1}",
    calls_head: "通話紀錄",
    no_calls: "沒有符合的通話。",
    hidden: "未顯示號碼",
    no_summary: "（還沒有摘要）",
    summary: "摘要",
    transcript: "逐字稿節錄",
    open: "在控制台打開",
    tasks_head: "等你處理的待辦",
    no_tasks: "沒有等你處理的事。",
    tasks_how: "用 /confirm # 或 /ignore # 處理。",
    errands_head: "AI 代打",
    no_errands: "還沒有 AI 代打的電話。",
    contacts_head: "聯絡人",
    no_contacts: "沒有符合的聯絡人。",
    started: "AI 代打 {1}：{2}",
    cancelled: "已停止：{1}",
    said: "已轉告通話中的 AI。",
    answered: "你的回答已交給 AI。",
    mode_done: "{1}：{2}",
    mode_failed: "{1} 沒有切換：{2}",
    mode_why: ["手機離線", "這支手機的 App 不能切換", "手機通話中", "手機沒有回應（可能稍後才切好）"],
    ask_errand: "讓 AI 打給 {1}：\n{2}",
    ask_mode: "把 {1} 切到「{2}」？",
    ask_confirm: "同意這件待辦？\n{1}",
    ask_ignore: "忽略這件待辦？\n{1}",
    ask_say: "要 AI 在通話中說：\n「{1}」？",
    ask_answer: "這樣回答 AI 的問題：\n「{1}」？",
    confirm_how: "2 分鐘內回覆 1 執行、2 取消（確認碼 {1}）。",
    yes: "執行",
    no: "取消",
    expired: "這個確認已過期或已用過，請重新下指令。",
    e_decided: "這件待辦已經在別處處理過了，沒有變動。",
    dropped: "已取消，什麼都沒做。",
    too_many: "這個聊天室一小時內已叫 AI 打了 {1} 通，到上限了。晚點再試，或到網頁控制台操作。",
    question: "正在打給 {1} 的 AI 問你：\n{2}",
    question_how: "{1} 秒內點選答案，或回覆這則訊息。",
    closed: "來不及了：這個問題已結束（AI 已先繼續）。",
    not_found: "找不到「{1}」。/calls、/tasks、/errands 會列出可用的編號。",
    no_contact: "沒有符合「{1}」的聯絡人或號碼。",
    many_contacts: "有好幾位聯絡人符合：{1}。請寫號碼或完整名字。",
    failed: "沒有成功（{1}）。",
    e_bad_number: "這不是 AI 能撥的號碼。",
    e_not_allowed: "緊急、短碼和加值付費號碼一律不撥。",
    e_no_phone: "沒有連線中、在 AI 模式的手機可以撥這通。",
    e_no_sim: "連線中的手機沒有這個號碼國家的 SIM 卡（或還不知道它有哪些卡）。",
    e_no_task: "請說要 AI 在電話裡辦什麼事。",
    e_too_long: "內容太長了。",
    e_not_on_call: "這通 AI 代打現在不在線上。",
    e_busy: "這通 AI 代打還在進行中。",
    list_blocked: "已加入黑名單：{1}",
    list_stall: "已加入陪聊名單：{1}",
    list_white: "已加入白名單：{1}",
    list_unblocked: "已恢復為一般聯絡人：{1}",
};

const ZH_CN: Words = Words {
    menu: [
        ("", "列出指令"),
        ("", "Bageo 版本、手机状态、进行中的通话"),
        ("[条数] [today|yesterday|YYYY-MM-DD] [missed|scam] [关键字或号码]", "最近的通话"),
        ("<#>", "某一通：摘要、逐字稿节选、链接"),
        ("", "等你处理的待办"),
        ("<#>", "同意待办"),
        ("<#>", "忽略待办"),
        ("<号码或联系人> <要办的事>", "让 AI 帮你打电话"),
        ("", "AI 代打的电话和结果"),
        ("[#]", "停止 AI 代打"),
        ("[#] <内容>", "通话中交代 AI 一句话"),
        ("<内容>", "回答 AI 问你的问题"),
        ("<手机> ai|normal", "把手机切到 AI 接听或普通手机"),
        ("<名字>", "找联系人"),
        ("<号码> [名称/原因]", "加入黑名单（来电拒接）"),
        ("<号码> [名称/备注]", "加入陪聊名单（一接起就陪聊）"),
        ("<号码> [名称/备注]", "加入白名单（重要联系人）"),
        ("<号码>", "解除黑名单／陪聊名单"),
    ],
    help_head: "在聊天室操作 Bageo。指令：",
    help_free: "也可以直接打字，例如“帮我打给 0912345678 订今晚七点两位”或“昨天有谁打来”。拨电话、切换手机模式、处理待办都会先问你确认。",
    hint: "看不懂这句。发 /help 看指令。",
    usage: "用法：{1}",
    phones: "手机",
    no_phones: "Bageo 启动后还没有手机连上。",
    mode_ai: "AI 接听",
    mode_normal: "普通手机",
    offline: "离线",
    busy_line: "通话中",
    live: "进行中的通话：{1}",
    calls_head: "通话记录",
    no_calls: "没有符合的通话。",
    hidden: "未显示号码",
    no_summary: "（还没有摘要）",
    summary: "摘要",
    transcript: "逐字稿节选",
    open: "在控制台打开",
    tasks_head: "等你处理的待办",
    no_tasks: "没有等你处理的事。",
    tasks_how: "用 /confirm # 或 /ignore # 处理。",
    errands_head: "AI 代打",
    no_errands: "还没有 AI 代打的电话。",
    contacts_head: "联系人",
    no_contacts: "没有符合的联系人。",
    started: "AI 代打 {1}：{2}",
    cancelled: "已停止：{1}",
    said: "已转告通话中的 AI。",
    answered: "你的回答已交给 AI。",
    mode_done: "{1}：{2}",
    mode_failed: "{1} 没有切换：{2}",
    mode_why: ["手机离线", "这部手机的 App 不能切换", "手机通话中", "手机没有回应（可能稍后才切好）"],
    ask_errand: "让 AI 打给 {1}：\n{2}",
    ask_mode: "把 {1} 切到“{2}”？",
    ask_confirm: "同意这件待办？\n{1}",
    ask_ignore: "忽略这件待办？\n{1}",
    ask_say: "让 AI 在通话中说：\n“{1}”？",
    ask_answer: "这样回答 AI 的问题：\n“{1}”？",
    confirm_how: "2 分钟内回复 1 执行、2 取消（确认码 {1}）。",
    yes: "执行",
    no: "取消",
    expired: "这个确认已过期或已用过，请重新下指令。",
    e_decided: "这件待办已经在别处处理过了，没有变动。",
    dropped: "已取消，什么都没做。",
    too_many: "这个聊天室一小时内已让 AI 打了 {1} 通，到上限了。晚点再试，或到网页控制台操作。",
    question: "正在打给 {1} 的 AI 问你：\n{2}",
    question_how: "{1} 秒内点选答案，或回复这条消息。",
    closed: "来不及了：这个问题已结束（AI 已先继续）。",
    not_found: "找不到“{1}”。/calls、/tasks、/errands 会列出可用的编号。",
    no_contact: "没有符合“{1}”的联系人或号码。",
    many_contacts: "有好几位联系人符合：{1}。请写号码或完整名字。",
    failed: "没有成功（{1}）。",
    e_bad_number: "这不是 AI 能拨的号码。",
    e_not_allowed: "紧急、短码和增值付费号码一律不拨。",
    e_no_phone: "没有在线、处于 AI 模式的手机可以拨这通。",
    e_no_sim: "在线的手机没有这个号码所属国家的 SIM 卡（或还不知道它有哪些卡）。",
    e_no_task: "请说要 AI 在电话里办什么事。",
    e_too_long: "内容太长了。",
    e_not_on_call: "这通 AI 代打现在不在线上。",
    e_busy: "这通 AI 代打还在进行中。",
    list_blocked: "已加入黑名单：{1}",
    list_stall: "已加入陪聊名单：{1}",
    list_white: "已加入白名单：{1}",
    list_unblocked: "已恢复为普通联系人：{1}",
};

const JA: Words = Words {
    menu: [
        ("", "コマンド一覧"),
        ("", "Bageo のバージョン、スマホ、通話中の電話"),
        ("[件数] [today|yesterday|YYYY-MM-DD] [missed|scam] [キーワードか番号]", "最近の通話"),
        ("<#>", "1 件の通話：要約、文字起こしの抜粋、リンク"),
        ("", "判断待ちの用件"),
        ("<#>", "用件を承認"),
        ("<#>", "用件を無視"),
        ("<番号か連絡先> <してほしいこと>", "AI に電話をかけてもらう"),
        ("", "AI 代理電話とその結果"),
        ("[#]", "AI 代理電話を止める"),
        ("[#] <内容>", "通話中の AI に伝える"),
        ("<内容>", "AI からの質問に答える"),
        ("<スマホ> ai|normal", "スマホを AI 応答と通常の電話で切り替え"),
        ("<名前>", "連絡先を探す"),
        ("<番号> [名前/理由]", "着信拒否リストに追加"),
        ("<番号> [名前/メモ]", "引き延ばしリストに追加"),
        ("<番号> [名前/メモ]", "ホワイトリストに追加"),
        ("<番号>", "着信拒否・引き延ばしを解除"),
    ],
    help_head: "このチャットから Bageo を操作できます。コマンド：",
    help_free: "普通に書いても大丈夫です。例：「0912345678 に電話して今夜 7 時に 2 名で予約して」「昨日だれから電話があった？」。発信、モード切り替え、用件の判断はあなたの確認を待ちます。",
    hint: "よくわかりませんでした。/help でコマンドを確認してください。",
    usage: "使い方：{1}",
    phones: "スマホ",
    no_phones: "Bageo の起動後、まだスマホが接続していません。",
    mode_ai: "AI が応答",
    mode_normal: "通常の電話",
    offline: "オフライン",
    busy_line: "通話中",
    live: "通話中の電話：{1}",
    calls_head: "通話履歴",
    no_calls: "該当する通話はありません。",
    hidden: "非通知",
    no_summary: "（要約はまだありません）",
    summary: "要約",
    transcript: "文字起こし（抜粋）",
    open: "コンソールで開く",
    tasks_head: "判断待ちの用件",
    no_tasks: "判断待ちの用件はありません。",
    tasks_how: "/confirm # か /ignore # で決めてください。",
    errands_head: "AI 代理電話",
    no_errands: "AI 代理電話はまだありません。",
    contacts_head: "連絡先",
    no_contacts: "該当する連絡先はありません。",
    started: "{1} への AI 代理電話：{2}",
    cancelled: "止めました：{1}",
    said: "通話中の AI に伝えました。",
    answered: "回答を AI に渡しました。",
    mode_done: "{1}：{2}",
    mode_failed: "{1} は切り替わりませんでした：{2}",
    mode_why: ["スマホがオフライン", "このスマホのアプリは切り替えに対応していません", "通話中", "スマホから応答がありません（あとで切り替わるかもしれません）"],
    ask_errand: "AI が {1} に電話して：\n{2}",
    ask_mode: "{1} を「{2}」に切り替えますか？",
    ask_confirm: "この用件を承認しますか？\n{1}",
    ask_ignore: "この用件を無視しますか？\n{1}",
    ask_say: "通話中の AI にこう伝えますか：\n「{1}」",
    ask_answer: "AI の質問にこう答えますか：\n「{1}」",
    confirm_how: "2 分以内に 1（実行）か 2（取り消し）で返信してください（確認コード {1}）。",
    yes: "実行",
    no: "取り消し",
    expired: "この確認は期限切れか、すでに使われています。もう一度コマンドを送ってください。",
    e_decided: "この用件はすでに別の場所で処理済みです。何も変更していません。",
    dropped: "取り消しました。何もしていません。",
    too_many: "このチャットから 1 時間に {1} 件の AI 代理電話を頼んだので上限です。あとで試すか、Web コンソールを使ってください。",
    question: "{1} に電話中の AI からの質問：\n{2}",
    question_how: "{1} 秒以内に答えを選ぶか、このメッセージに返信してください。",
    closed: "間に合いませんでした：この質問はもう終わっています（AI は先に進みました）。",
    not_found: "「{1}」に当たるものがありません。/calls、/tasks、/errands で番号を確認してください。",
    no_contact: "「{1}」に当たる連絡先や番号はありません。",
    many_contacts: "複数の連絡先が当たります：{1}。番号かフルネームで書いてください。",
    failed: "できませんでした（{1}）。",
    e_bad_number: "AI が発信できる番号ではありません。",
    e_not_allowed: "緊急番号、短縮番号、有料番号には発信しません。",
    e_no_phone: "この電話をかけられる AI モードのスマホが接続していません。",
    e_no_sim: "接続中のスマホに、その番号の国の SIM がありません（または SIM がまだわかりません）。",
    e_no_task: "電話で何をしてほしいか書いてください。",
    e_too_long: "内容が長すぎます。",
    e_not_on_call: "その AI 代理電話はいま通話中ではありません。",
    e_busy: "その AI 代理電話はまだ進行中です。",
    list_blocked: "着信拒否リストに追加しました：{1}",
    list_stall: "引き延ばしリストに追加しました：{1}",
    list_white: "ホワイトリストに追加しました：{1}",
    list_unblocked: "通常の連絡先に戻しました：{1}",
};

const KO: Words = Words {
    menu: [
        ("", "명령어 목록"),
        ("", "Bageo 버전, 휴대폰, 진행 중인 통화"),
        ("[개수] [today|yesterday|YYYY-MM-DD] [missed|scam] [검색어 또는 번호]", "최근 통화"),
        ("<#>", "통화 하나: 요약, 녹취록 일부, 링크"),
        ("", "처리를 기다리는 할 일"),
        ("<#>", "할 일 승인"),
        ("<#>", "할 일 무시"),
        ("<번호 또는 연락처> <할 일>", "AI가 대신 전화하게 하기"),
        ("", "AI 대리 전화와 결과"),
        ("[#]", "AI 대리 전화 중지"),
        ("[#] <내용>", "통화 중인 AI에게 전달"),
        ("<내용>", "AI의 질문에 답하기"),
        ("<휴대폰> ai|normal", "휴대폰을 AI 응답 / 일반 휴대폰으로 전환"),
        ("<이름>", "연락처 찾기"),
        ("<번호> [이름/사유]", "차단 목록에 추가"),
        ("<번호> [이름/메모]", "시간 끌기 목록에 추가"),
        ("<번호> [이름/메모]", "화이트리스트에 추가"),
        ("<번호>", "차단·시간 끌기 해제"),
    ],
    help_head: "이 채팅에서 Bageo를 조작합니다. 명령어:",
    help_free: "그냥 써도 됩니다. 예: “0912345678에 전화해서 오늘 저녁 7시 2명 예약해 줘”, “어제 누가 전화했어?”. 전화 걸기, 모드 전환, 할 일 처리는 확인을 받은 뒤에 합니다.",
    hint: "이해하지 못했어요. /help 로 명령어를 확인하세요.",
    usage: "사용법: {1}",
    phones: "휴대폰",
    no_phones: "Bageo가 시작된 뒤 연결된 휴대폰이 없습니다.",
    mode_ai: "AI 응답",
    mode_normal: "일반 휴대폰",
    offline: "오프라인",
    busy_line: "통화 중",
    live: "진행 중인 통화: {1}",
    calls_head: "통화 기록",
    no_calls: "해당하는 통화가 없습니다.",
    hidden: "발신번호 표시 제한",
    no_summary: "(아직 요약 없음)",
    summary: "요약",
    transcript: "녹취록 일부",
    open: "콘솔에서 열기",
    tasks_head: "처리를 기다리는 할 일",
    no_tasks: "기다리는 할 일이 없습니다.",
    tasks_how: "/confirm # 또는 /ignore # 로 처리하세요.",
    errands_head: "AI 대리 전화",
    no_errands: "아직 AI 대리 전화가 없습니다.",
    contacts_head: "연락처",
    no_contacts: "해당하는 연락처가 없습니다.",
    started: "{1}에게 AI 대리 전화: {2}",
    cancelled: "중지함: {1}",
    said: "통화 중인 AI에게 전달했습니다.",
    answered: "답을 AI에게 전달했습니다.",
    mode_done: "{1}: {2}",
    mode_failed: "{1} 전환 안 됨: {2}",
    mode_why: ["휴대폰이 오프라인", "이 휴대폰 앱은 전환을 지원하지 않음", "통화 중", "휴대폰이 응답하지 않음(나중에 전환될 수 있음)"],
    ask_errand: "AI가 {1}에게 전화해서:\n{2}",
    ask_mode: "{1}을(를) “{2}”(으)로 전환할까요?",
    ask_confirm: "이 할 일을 승인할까요?\n{1}",
    ask_ignore: "이 할 일을 무시할까요?\n{1}",
    ask_say: "통화 중인 AI에게 이렇게 전할까요?\n“{1}”",
    ask_answer: "AI의 질문에 이렇게 답할까요?\n“{1}”",
    confirm_how: "2분 안에 1(실행) 또는 2(취소)로 답하세요(확인 코드 {1}).",
    yes: "실행",
    no: "취소",
    expired: "이 확인은 만료되었거나 이미 사용되었습니다. 명령을 다시 보내세요.",
    e_decided: "이 할 일은 이미 다른 곳에서 처리되었습니다. 아무것도 바꾸지 않았습니다.",
    dropped: "취소했습니다. 아무것도 하지 않았습니다.",
    too_many: "이 채팅에서 1시간에 AI 대리 전화를 {1}통 요청해 한도에 도달했습니다. 나중에 다시 하거나 웹 콘솔을 쓰세요.",
    question: "{1}에게 전화 중인 AI의 질문:\n{2}",
    question_how: "{1}초 안에 답을 고르거나 이 메시지에 답장하세요.",
    closed: "늦었습니다: 이 질문은 끝났습니다(AI가 먼저 진행했습니다).",
    not_found: "“{1}”에 해당하는 것이 없습니다. /calls, /tasks, /errands 에서 번호를 확인하세요.",
    no_contact: "“{1}”에 해당하는 연락처나 번호가 없습니다.",
    many_contacts: "여러 연락처가 해당합니다: {1}. 번호나 전체 이름을 쓰세요.",
    failed: "실패했습니다({1}).",
    e_bad_number: "AI가 걸 수 있는 번호가 아닙니다.",
    e_not_allowed: "긴급 번호, 단축 번호, 유료 번호에는 걸지 않습니다.",
    e_no_phone: "이 전화를 걸 수 있는 AI 모드 휴대폰이 연결되어 있지 않습니다.",
    e_no_sim: "연결된 휴대폰에 그 번호 국가의 SIM이 없습니다(또는 SIM 정보를 아직 모릅니다).",
    e_no_task: "전화로 무엇을 해야 하는지 적어 주세요.",
    e_too_long: "내용이 너무 깁니다.",
    e_not_on_call: "그 AI 대리 전화는 지금 통화 중이 아닙니다.",
    e_busy: "그 AI 대리 전화는 아직 진행 중입니다.",
    list_blocked: "차단 목록에 추가했습니다: {1}",
    list_stall: "시간 끌기 목록에 추가했습니다: {1}",
    list_white: "화이트리스트에 추가했습니다: {1}",
    list_unblocked: "일반 연락처로 복구했습니다: {1}",
};

const ES: Words = Words {
    menu: [
        ("", "Lista de comandos"),
        ("", "Versión de Bageo, teléfonos y llamadas en curso"),
        ("[n] [today|yesterday|AAAA-MM-DD] [missed|scam] [palabra o número]", "Llamadas recientes"),
        ("<#>", "Una llamada: resumen, extracto y enlace"),
        ("", "Pendientes que esperan tu decisión"),
        ("<#>", "Aceptar un pendiente"),
        ("<#>", "Ignorar un pendiente"),
        ("<número o contacto> <qué hay que hacer>", "Que la IA haga una llamada por ti"),
        ("", "Llamadas de la IA y sus resultados"),
        ("[#]", "Detener una llamada de la IA"),
        ("[#] <texto>", "Decirle algo a la IA durante su llamada"),
        ("<texto>", "Responder la pregunta de la IA"),
        ("<teléfono> ai|normal", "Cambiar un teléfono entre la IA y teléfono normal"),
        ("<nombre>", "Buscar un contacto"),
        ("<número> [nombre/motivo]", "Añadir a la lista negra"),
        ("<número> [nombre/nota]", "Añadir a la lista para entretener"),
        ("<número> [nombre/nota]", "Añadir a la lista blanca"),
        ("<número>", "Quitar de la lista negra / para entretener"),
    ],
    help_head: "Bageo desde este chat. Comandos:",
    help_free: "También puedes escribir lo que quieres, p. ej. «llama al 0912345678 y reserva mesa para dos a las 7» o «¿quién llamó ayer?». Las llamadas, los cambios de modo y las decisiones sobre pendientes esperan tu confirmación.",
    hint: "No lo entendí. Envía /help para ver los comandos.",
    usage: "Uso: {1}",
    phones: "Teléfonos",
    no_phones: "Ningún teléfono se ha conectado desde que arrancó Bageo.",
    mode_ai: "contesta la IA",
    mode_normal: "teléfono normal",
    offline: "sin conexión",
    busy_line: "en una llamada",
    live: "Llamadas en curso: {1}",
    calls_head: "Llamadas",
    no_calls: "Ninguna llamada coincide.",
    hidden: "Número oculto",
    no_summary: "(aún sin resumen)",
    summary: "Resumen",
    transcript: "Transcripción (extracto)",
    open: "Abrir en la consola",
    tasks_head: "Pendientes que esperan tu decisión",
    no_tasks: "No hay nada pendiente.",
    tasks_how: "/confirm # o /ignore # para decidir.",
    errands_head: "Llamadas de la IA",
    no_errands: "Todavía no hay llamadas de la IA.",
    contacts_head: "Contactos",
    no_contacts: "Ningún contacto coincide.",
    started: "Llamada de la IA a {1}: {2}",
    cancelled: "Detenida: {1}",
    said: "Se lo pasé a la IA de la llamada.",
    answered: "Tu respuesta llegó a la IA.",
    mode_done: "{1}: {2}",
    mode_failed: "{1} no cambió: {2}",
    mode_why: ["el teléfono está sin conexión", "su app no puede cambiar de modo", "está en una llamada", "el teléfono no respondió (puede que aún cambie)"],
    ask_errand: "La IA llamará a {1} para:\n{2}",
    ask_mode: "¿Cambiar {1} a «{2}»?",
    ask_confirm: "¿Aceptar este pendiente?\n{1}",
    ask_ignore: "¿Ignorar este pendiente?\n{1}",
    ask_say: "¿Decirle a la IA en la llamada:\n“{1}”?",
    ask_answer: "¿Responder a la pregunta de la IA:\n“{1}”?",
    confirm_how: "Responde 1 para seguir o 2 para descartarlo, en 2 minutos (código {1}).",
    yes: "Adelante",
    no: "Descartar",
    expired: "Esa confirmación caducó o ya se usó. Envía el comando otra vez.",
    e_decided: "Ese pendiente ya se decidió en otro lugar; no se cambió nada.",
    dropped: "Descartado. No se hizo nada.",
    too_many: "Este chat ya pidió {1} llamadas de la IA en la última hora, el límite. Inténtalo más tarde o usa la consola web.",
    question: "La IA que llama a {1} te pregunta:\n{2}",
    question_how: "Toca una respuesta o responde a este mensaje en {1} segundos.",
    closed: "Demasiado tarde: esa pregunta ya se cerró (la IA siguió sin ella).",
    not_found: "Nada coincide con «{1}». /calls, /tasks y /errands muestran los números que puedes usar.",
    no_contact: "Ningún contacto ni número coincide con «{1}».",
    many_contacts: "Coinciden varios contactos: {1}. Escribe el número o el nombre completo.",
    failed: "No funcionó ({1}).",
    e_bad_number: "Ese no es un número que la IA pueda marcar.",
    e_not_allowed: "Nunca se marcan números de emergencia, cortos ni de tarificación especial.",
    e_no_phone: "No hay ningún teléfono en modo IA conectado para hacer esta llamada.",
    e_no_sim: "Ningún teléfono conectado tiene una SIM del país de ese número (o aún no se conocen sus SIM).",
    e_no_task: "Di qué debe conseguir la IA en la llamada.",
    e_too_long: "Ese texto es demasiado largo.",
    e_not_on_call: "Esa llamada de la IA no está en línea ahora.",
    e_busy: "Esa llamada de la IA sigue en curso.",
    list_blocked: "Añadido a la lista negra: {1}",
    list_stall: "Añadido a la lista para entretener: {1}",
    list_white: "Añadido a la lista blanca: {1}",
    list_unblocked: "Restaurado como contacto normal: {1}",
};

const FR: Words = Words {
    menu: [
        ("", "Liste des commandes"),
        ("", "Version de Bageo, téléphones et appels en cours"),
        ("[n] [today|yesterday|AAAA-MM-JJ] [missed|scam] [mot ou numéro]", "Appels récents"),
        ("<#>", "Un appel : résumé, extrait, lien"),
        ("", "Tâches qui attendent ta décision"),
        ("<#>", "Accepter une tâche"),
        ("<#>", "Ignorer une tâche"),
        ("<numéro ou contact> <ce qu'il faut obtenir>", "Faire passer un appel par l'IA"),
        ("", "Appels de l'IA et résultats"),
        ("[#]", "Arrêter un appel de l'IA"),
        ("[#] <texte>", "Dire quelque chose à l'IA pendant son appel"),
        ("<texte>", "Répondre à la question de l'IA"),
        ("<téléphone> ai|normal", "Passer un téléphone en réponse IA ou en téléphone normal"),
        ("<nom>", "Chercher un contact"),
        ("<numéro> [nom/motif]", "Ajouter à la liste noire"),
        ("<numéro> [nom/note]", "Ajouter à la liste « Les faire parler »"),
        ("<numéro> [nom/note]", "Ajouter à la liste blanche"),
        ("<numéro>", "Retirer de la liste noire / « Les faire parler »"),
    ],
    help_head: "Bageo depuis ce chat. Commandes :",
    help_free: "Tu peux aussi écrire ce que tu veux, p. ex. « appelle le 0912345678 et réserve une table pour deux à 19 h » ou « qui a appelé hier ? ». Les appels, les changements de mode et les décisions sur les tâches attendent ton accord.",
    hint: "Je n'ai pas compris. Envoie /help pour les commandes.",
    usage: "Utilisation : {1}",
    phones: "Téléphones",
    no_phones: "Aucun téléphone ne s'est connecté depuis le démarrage de Bageo.",
    mode_ai: "l'IA répond",
    mode_normal: "téléphone normal",
    offline: "hors ligne",
    busy_line: "en appel",
    live: "Appels en cours : {1}",
    calls_head: "Appels",
    no_calls: "Aucun appel ne correspond.",
    hidden: "Numéro masqué",
    no_summary: "(pas encore de résumé)",
    summary: "Résumé",
    transcript: "Transcription (extrait)",
    open: "Ouvrir dans la console",
    tasks_head: "Tâches qui attendent ta décision",
    no_tasks: "Rien n'attend ta décision.",
    tasks_how: "/confirm # ou /ignore # pour décider.",
    errands_head: "Appels de l'IA",
    no_errands: "Pas encore d'appel de l'IA.",
    contacts_head: "Contacts",
    no_contacts: "Aucun contact ne correspond.",
    started: "Appel de l'IA à {1} : {2}",
    cancelled: "Arrêté : {1}",
    said: "Transmis à l'IA en ligne.",
    answered: "Ta réponse a été transmise à l'IA.",
    mode_done: "{1} : {2}",
    mode_failed: "{1} n'a pas changé : {2}",
    mode_why: ["le téléphone est hors ligne", "son appli ne sait pas changer de mode", "il est en appel", "le téléphone n'a pas répondu (il peut encore changer)"],
    ask_errand: "L'IA appellera {1} pour :\n{2}",
    ask_mode: "Passer {1} en « {2} » ?",
    ask_confirm: "Accepter cette tâche ?\n{1}",
    ask_ignore: "Ignorer cette tâche ?\n{1}",
    ask_say: "Dire à l'IA pendant l'appel :\n« {1} » ?",
    ask_answer: "Répondre à la question de l'IA :\n« {1} » ?",
    confirm_how: "Réponds 1 pour lancer ou 2 pour abandonner, sous 2 minutes (code {1}).",
    yes: "Lancer",
    no: "Abandonner",
    expired: "Cette confirmation a expiré ou a déjà servi. Renvoie la commande.",
    e_decided: "Cette tâche a déjà été traitée ailleurs ; rien n'a été modifié.",
    dropped: "Abandonné. Rien n'a été fait.",
    too_many: "Ce chat a déjà demandé {1} appels de l'IA dans l'heure, la limite. Réessaie plus tard ou passe par la console web.",
    question: "L'IA qui appelle {1} te demande :\n{2}",
    question_how: "Touche une réponse ou réponds à ce message dans les {1} secondes.",
    closed: "Trop tard : cette question est close (l'IA a continué sans).",
    not_found: "Rien ne correspond à « {1} ». /calls, /tasks et /errands affichent les numéros à utiliser.",
    no_contact: "Aucun contact ni numéro ne correspond à « {1} ».",
    many_contacts: "Plusieurs contacts correspondent : {1}. Écris le numéro ou le nom complet.",
    failed: "Ça n'a pas marché ({1}).",
    e_bad_number: "Ce n'est pas un numéro que l'IA peut composer.",
    e_not_allowed: "Les numéros d'urgence, courts et surtaxés ne sont jamais composés.",
    e_no_phone: "Aucun téléphone en mode IA n'est connecté pour passer cet appel.",
    e_no_sim: "Aucun téléphone connecté n'a de SIM du pays de ce numéro (ou ses SIM ne sont pas encore connues).",
    e_no_task: "Dis ce que l'IA doit obtenir pendant l'appel.",
    e_too_long: "Ce texte est trop long.",
    e_not_on_call: "Cet appel de l'IA n'est pas en ligne.",
    e_busy: "Cet appel de l'IA est encore en cours.",
    list_blocked: "Ajouté à la liste noire : {1}",
    list_stall: "Ajouté à la liste « Les faire parler » : {1}",
    list_white: "Ajouté à la liste blanche : {1}",
    list_unblocked: "Rétabli comme contact normal : {1}",
};

const DE: Words = Words {
    menu: [
        ("", "Befehle anzeigen"),
        ("", "Bageo-Version, Telefone und laufende Anrufe"),
        ("[n] [today|yesterday|JJJJ-MM-TT] [missed|scam] [Wort oder Nummer]", "Letzte Anrufe"),
        ("<#>", "Ein Anruf: Zusammenfassung, Auszug, Link"),
        ("", "Aufgaben, die auf dich warten"),
        ("<#>", "Aufgabe annehmen"),
        ("<#>", "Aufgabe ignorieren"),
        ("<Nummer oder Kontakt> <was erledigt werden soll>", "Die KI für dich anrufen lassen"),
        ("", "KI-Anrufe und Ergebnisse"),
        ("[#]", "KI-Anruf abbrechen"),
        ("[#] <Text>", "Der KI während des Anrufs etwas sagen"),
        ("<Text>", "Die Frage der KI beantworten"),
        ("<Telefon> ai|normal", "Telefon zwischen KI und normalem Telefon umschalten"),
        ("<Name>", "Kontakt suchen"),
        ("<Nummer> [Name/Grund]", "Nummer zur Sperrliste hinzufügen"),
        ("<Nummer> [Name/Notiz]", "Nummer zur Hinhalteliste hinzufügen"),
        ("<Nummer> [Name/Notiz]", "Nummer zur Whitelist hinzufügen"),
        ("<Nummer>", "Von Sperr-/Hinhalteliste entfernen"),
    ],
    help_head: "Bageo aus diesem Chat. Befehle:",
    help_free: "Du kannst auch einfach schreiben, was du willst, z. B. „ruf 0912345678 an und reservier für zwei um 19 Uhr“ oder „wer hat gestern angerufen?“. Anrufe, Moduswechsel und Entscheidungen zu Aufgaben warten auf dein OK.",
    hint: "Das habe ich nicht verstanden. /help zeigt die Befehle.",
    usage: "Verwendung: {1}",
    phones: "Telefone",
    no_phones: "Seit dem Start von Bageo hat sich kein Telefon verbunden.",
    mode_ai: "KI nimmt ab",
    mode_normal: "normales Telefon",
    offline: "offline",
    busy_line: "im Gespräch",
    live: "Laufende Anrufe: {1}",
    calls_head: "Anrufe",
    no_calls: "Keine passenden Anrufe.",
    hidden: "Unterdrückte Nummer",
    no_summary: "(noch keine Zusammenfassung)",
    summary: "Zusammenfassung",
    transcript: "Transkript (Auszug)",
    open: "In der Konsole öffnen",
    tasks_head: "Aufgaben, die auf dich warten",
    no_tasks: "Nichts wartet auf dich.",
    tasks_how: "/confirm # oder /ignore # entscheidet.",
    errands_head: "KI-Anrufe",
    no_errands: "Noch keine KI-Anrufe.",
    contacts_head: "Kontakte",
    no_contacts: "Kein passender Kontakt.",
    started: "KI-Anruf an {1}: {2}",
    cancelled: "Abgebrochen: {1}",
    said: "An die KI im Gespräch weitergegeben.",
    answered: "Deine Antwort ging an die KI.",
    mode_done: "{1}: {2}",
    mode_failed: "{1} wurde nicht umgeschaltet: {2}",
    mode_why: ["das Telefon ist offline", "seine App kann nicht umschalten", "es ist im Gespräch", "das Telefon hat nicht geantwortet (es schaltet vielleicht noch um)"],
    ask_errand: "Die KI ruft {1} an, um:\n{2}",
    ask_mode: "{1} auf „{2}“ umschalten?",
    ask_confirm: "Diese Aufgabe annehmen?\n{1}",
    ask_ignore: "Diese Aufgabe ignorieren?\n{1}",
    ask_say: "Der KI im Gespräch sagen:\n„{1}“?",
    ask_answer: "Die Frage der KI so beantworten:\n„{1}“?",
    confirm_how: "Antworte innerhalb von 2 Minuten mit 1 zum Ausführen oder 2 zum Verwerfen (Code {1}).",
    yes: "Ausführen",
    no: "Verwerfen",
    expired: "Diese Bestätigung ist abgelaufen oder schon benutzt. Schick den Befehl noch einmal.",
    e_decided: "Diese Aufgabe wurde schon anderswo entschieden; nichts wurde geändert.",
    dropped: "Verworfen. Nichts wurde getan.",
    too_many: "Dieser Chat hat in der letzten Stunde schon {1} KI-Anrufe gestartet, das Limit. Versuch es später oder nimm die Web-Konsole.",
    question: "Die KI im Gespräch mit {1} fragt dich:\n{2}",
    question_how: "Tippe innerhalb von {1} Sekunden auf eine Antwort oder antworte auf diese Nachricht.",
    closed: "Zu spät: Die Frage ist geschlossen (die KI hat ohne sie weitergemacht).",
    not_found: "Nichts passt zu „{1}“. /calls, /tasks und /errands zeigen die Nummern dafür.",
    no_contact: "Kein Kontakt und keine Nummer passt zu „{1}“.",
    many_contacts: "Mehrere Kontakte passen: {1}. Schreib die Nummer oder den vollen Namen.",
    failed: "Hat nicht geklappt ({1}).",
    e_bad_number: "Diese Nummer kann die KI nicht wählen.",
    e_not_allowed: "Notruf-, Kurz- und Mehrwertnummern werden nie gewählt.",
    e_no_phone: "Kein Telefon im KI-Modus ist verbunden, um diesen Anruf zu machen.",
    e_no_sim: "Kein verbundenes Telefon hat eine SIM aus dem Land dieser Nummer (oder seine SIMs sind noch unbekannt).",
    e_no_task: "Schreib, was die KI im Anruf erledigen soll.",
    e_too_long: "Der Text ist zu lang.",
    e_not_on_call: "Dieser KI-Anruf ist gerade nicht in der Leitung.",
    e_busy: "Dieser KI-Anruf läuft noch.",
    list_blocked: "Zur Sperrliste hinzugefügt: {1}",
    list_stall: "Zur Hinhalteliste hinzugefügt: {1}",
    list_white: "Zur Whitelist hinzugefügt: {1}",
    list_unblocked: "Als normaler Kontakt wiederhergestellt: {1}",
};

const PT: Words = Words {
    menu: [
        ("", "Lista de comandos"),
        ("", "Versão do Bageo, celulares e ligações em andamento"),
        ("[n] [today|yesterday|AAAA-MM-DD] [missed|scam] [palavra ou número]", "Ligações recentes"),
        ("<#>", "Uma ligação: resumo, trecho e link"),
        ("", "Pendências esperando você"),
        ("<#>", "Aceitar uma pendência"),
        ("<#>", "Ignorar uma pendência"),
        ("<número ou contato> <o que resolver>", "Fazer a IA ligar por você"),
        ("", "Ligações da IA e resultados"),
        ("[#]", "Parar uma ligação da IA"),
        ("[#] <texto>", "Dizer algo à IA durante a ligação"),
        ("<texto>", "Responder à pergunta da IA"),
        ("<celular> ai|normal", "Trocar um celular entre a IA atender e celular normal"),
        ("<nome>", "Procurar um contato"),
        ("<número> [nome/motivo]", "Adicionar à lista de bloqueio"),
        ("<número> [nome/nota]", "Adicionar à lista para manter falando"),
        ("<número> [nome/nota]", "Adicionar à lista branca"),
        ("<número>", "Remover da lista de bloqueio / para manter falando"),
    ],
    help_head: "Bageo por este chat. Comandos:",
    help_free: "Você também pode escrever o que quer, por ex. “ligue para 0912345678 e reserve mesa para dois às 19h” ou “quem ligou ontem?”. Ligações, trocas de modo e decisões sobre pendências esperam sua confirmação.",
    hint: "Não entendi. Envie /help para ver os comandos.",
    usage: "Uso: {1}",
    phones: "Celulares",
    no_phones: "Nenhum celular se conectou desde que o Bageo iniciou.",
    mode_ai: "a IA atende",
    mode_normal: "celular normal",
    offline: "offline",
    busy_line: "em ligação",
    live: "Ligações em andamento: {1}",
    calls_head: "Ligações",
    no_calls: "Nenhuma ligação corresponde.",
    hidden: "Número oculto",
    no_summary: "(ainda sem resumo)",
    summary: "Resumo",
    transcript: "Transcrição (trecho)",
    open: "Abrir no console",
    tasks_head: "Pendências esperando você",
    no_tasks: "Nada esperando você.",
    tasks_how: "/confirm # ou /ignore # para decidir.",
    errands_head: "Ligações da IA",
    no_errands: "Ainda não há ligações da IA.",
    contacts_head: "Contatos",
    no_contacts: "Nenhum contato corresponde.",
    started: "Ligação da IA para {1}: {2}",
    cancelled: "Parada: {1}",
    said: "Passei para a IA na ligação.",
    answered: "Sua resposta foi para a IA.",
    mode_done: "{1}: {2}",
    mode_failed: "{1} não trocou: {2}",
    mode_why: ["o celular está offline", "o app dele não troca de modo", "está em ligação", "o celular não respondeu (pode ainda trocar)"],
    ask_errand: "A IA vai ligar para {1} para:\n{2}",
    ask_mode: "Trocar {1} para “{2}”?",
    ask_confirm: "Aceitar esta pendência?\n{1}",
    ask_ignore: "Ignorar esta pendência?\n{1}",
    ask_say: "Dizer à IA na ligação:\n“{1}”?",
    ask_answer: "Responder à pergunta da IA:\n“{1}”?",
    confirm_how: "Responda 1 para seguir ou 2 para descartar, em até 2 minutos (código {1}).",
    yes: "Seguir",
    no: "Descartar",
    expired: "Essa confirmação expirou ou já foi usada. Envie o comando de novo.",
    e_decided: "Essa pendência já foi decidida em outro lugar; nada foi alterado.",
    dropped: "Descartado. Nada foi feito.",
    too_many: "Este chat já pediu {1} ligações da IA na última hora, o limite. Tente mais tarde ou use o console web.",
    question: "A IA que está ligando para {1} pergunta:\n{2}",
    question_how: "Toque numa resposta ou responda a esta mensagem em até {1} segundos.",
    closed: "Tarde demais: essa pergunta já fechou (a IA seguiu sem ela).",
    not_found: "Nada corresponde a “{1}”. /calls, /tasks e /errands mostram os números para usar.",
    no_contact: "Nenhum contato ou número corresponde a “{1}”.",
    many_contacts: "Vários contatos correspondem: {1}. Escreva o número ou o nome completo.",
    failed: "Não deu certo ({1}).",
    e_bad_number: "Esse não é um número que a IA possa discar.",
    e_not_allowed: "Números de emergência, curtos e de tarifa especial nunca são discados.",
    e_no_phone: "Nenhum celular no modo IA está conectado para fazer esta ligação.",
    e_no_sim: "Nenhum celular conectado tem chip do país desse número (ou os chips ainda não são conhecidos).",
    e_no_task: "Diga o que a IA deve resolver na ligação.",
    e_too_long: "Esse texto é longo demais.",
    e_not_on_call: "Essa ligação da IA não está na linha agora.",
    e_busy: "Essa ligação da IA ainda está em andamento.",
    list_blocked: "Adicionado à lista de bloqueio: {1}",
    list_stall: "Adicionado à lista para manter falando: {1}",
    list_white: "Adicionado à lista branca: {1}",
    list_unblocked: "Restaurado como contato normal: {1}",
};

