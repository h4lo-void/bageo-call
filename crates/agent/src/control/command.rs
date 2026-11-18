// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! What the owner can ask for from a chat ([Action]), read from a slash
//! command or from the free-text model's answer ([from_model]). Both end in
//! [validate]: the model's answer is untrusted input and gets no more room
//! than a typed command.

use chrono::NaiveDate;
use serde_json::Value;

use crate::calls_http::PhoneMode;
use crate::errands::{SAY_MAX_CHARS, TASK_MAX_CHARS};

/// Slash commands, in the order of the help text and the platform menus.
pub const COMMANDS: [&str; 18] = [
    "help", "status", "calls", "call", "tasks", "confirm", "ignore", "errand", "errands", "cancel", "say", "answer", "mode",
    "contacts", "block", "stall", "white", "unblock",
];

/// Calls a list shows when no number is given, and at most.
pub const CALLS_DEFAULT: usize = 5;
pub const CALLS_MAX: usize = 20;
const REF_MAX: usize = 64;
const TARGET_MAX: usize = 80;
const QUERY_MAX: usize = 60;
const PHONE_MAX: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Only {
    #[default]
    All,
    /// Never answered (rang out, declined, hung up while ringing).
    Missed,
    /// Judged sales or scam, or blocked as spam.
    Scam,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Day {
    Today,
    Yesterday,
    On(NaiveDate),
}

impl Day {
    /// The calendar day it names, counted from today.
    pub fn date(self, today: NaiveDate) -> NaiveDate {
        match self {
            Day::Today => today,
            Day::Yesterday => today.pred_opt().unwrap_or(today),
            Day::On(d) => d,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallQuery {
    pub limit: usize,
    pub day: Option<Day>,
    pub only: Only,
    /// A name, number or word to look for (caller, transcript, summary).
    pub text: String,
}

impl Default for CallQuery {
    fn default() -> Self {
        Self { limit: CALLS_DEFAULT, day: None, only: Only::All, text: String::new() }
    }
}

/// One thing the owner asks for. A "ref" names an item: its number in the
/// list the chat was last shown (1, #2), or its id (or the start of it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Help,
    Status,
    Calls(CallQuery),
    Call { r: String },
    Tasks,
    /// Accept (confirm) or ignore a to-do.
    Decide { r: String, confirm: bool },
    /// The AI calls the target (a number or a contact's name) to get the task done.
    Errand { target: String, task: String },
    Errands,
    /// Empty ref: the one AI call under way.
    Cancel { r: String },
    /// Empty ref: the AI call on the line now.
    Say { r: String, text: String },
    Answer { text: String },
    Mode { phone: String, mode: PhoneMode },
    Contacts { query: String },
    /// Add/move a number to Blacklist (`blacklist`), Force-Stall list (`stall`),
    /// Whitelist (`whitelist`), or unblock it (`unblock`).
    SetContactKind { kind: String, number: String, name: String },
}

impl Action {
    /// Dials, switches a phone or decides a to-do: the owner confirms it first.
    pub fn needs_confirmation(&self) -> bool {
        matches!(self, Action::Errand { .. } | Action::Mode { .. } | Action::Decide { .. })
    }
}

/// What a slash command reads as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Action(Action),
    /// A known command written wrong: show how it is used.
    Usage(&'static str),
    /// Not a command of ours.
    Unknown,
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Text from the owner: control characters (other than line breaks) dropped.
fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect::<String>()
        .trim()
        .to_string()
}

fn ref_ok(r: &str) -> bool {
    r.chars().count() <= REF_MAX && r.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '#'))
}

fn mode_word(s: &str) -> Option<PhoneMode> {
    match s.trim().to_lowercase().as_str() {
        // IA: how Spanish, French and Portuguese write AI; KI: German.
        "ai" | "a" | "ia" | "ki" => Some(PhoneMode::Ai),
        "normal" | "n" | "一般" | "普通" | "通常" | "일반" => Some(PhoneMode::Normal),
        _ => None,
    }
}

/// The same checks for a typed command and for the model's answer: lengths,
/// characters, ranges. Err: why (a code for the log).
pub fn validate(a: Action) -> Result<Action, &'static str> {
    Ok(match a {
        Action::Calls(mut q) => {
            q.limit = q.limit.clamp(1, CALLS_MAX);
            q.text = one_line(&clean(&q.text));
            if q.text.chars().count() > QUERY_MAX {
                return Err("query_too_long");
            }
            Action::Calls(q)
        }
        Action::Call { r } | Action::Decide { r, .. } if r.trim().is_empty() => return Err("no_ref"),
        Action::Call { r } => {
            let r = r.trim().to_string();
            if !ref_ok(&r) {
                return Err("bad_ref");
            }
            Action::Call { r }
        }
        Action::Decide { r, confirm } => {
            let r = r.trim().to_string();
            if !ref_ok(&r) {
                return Err("bad_ref");
            }
            Action::Decide { r, confirm }
        }
        Action::Cancel { r } => {
            let r = r.trim().to_string();
            if !ref_ok(&r) {
                return Err("bad_ref");
            }
            Action::Cancel { r }
        }
        Action::Say { r, text } => {
            let r = r.trim().to_string();
            let text = clean(&text);
            if !ref_ok(&r) {
                return Err("bad_ref");
            }
            if text.is_empty() {
                return Err("empty_text");
            }
            if text.chars().count() > SAY_MAX_CHARS {
                return Err("text_too_long");
            }
            Action::Say { r, text }
        }
        Action::Answer { text } => {
            let text = clean(&text);
            if text.is_empty() {
                return Err("empty_text");
            }
            if text.chars().count() > SAY_MAX_CHARS {
                return Err("text_too_long");
            }
            Action::Answer { text }
        }
        Action::Errand { target, task } => {
            let target = one_line(&clean(&target));
            let task = clean(&task);
            if target.is_empty() {
                return Err("no_target");
            }
            if target.chars().count() > TARGET_MAX {
                return Err("target_too_long");
            }
            if task.is_empty() {
                return Err("no_task");
            }
            if task.chars().count() > TASK_MAX_CHARS {
                return Err("task_too_long");
            }
            Action::Errand { target, task }
        }
        Action::Mode { phone, mode } => {
            let phone = one_line(&clean(&phone));
            if phone.is_empty() || phone.chars().count() > PHONE_MAX {
                return Err("bad_phone");
            }
            Action::Mode { phone, mode }
        }
        Action::Contacts { query } => {
            let query = one_line(&clean(&query));
            if query.chars().count() > QUERY_MAX {
                return Err("query_too_long");
            }
            Action::Contacts { query }
        }
        Action::SetContactKind { kind, number, name } => {
            let kind = kind.trim().to_ascii_lowercase();
            if !matches!(kind.as_str(), "blacklist" | "stall" | "whitelist" | "unblock") {
                return Err("bad_kind");
            }
            let number = one_line(&clean(&number));
            if digit_count(&number) < 3 || number.chars().count() > TARGET_MAX || !numberish(&number.replace(' ', "")) {
                return Err("bad_number");
            }
            let name = one_line(&clean(&name));
            if name.chars().count() > TARGET_MAX {
                return Err("name_too_long");
            }
            Action::SetContactKind { kind, number, name }
        }
        other @ (Action::Help | Action::Status | Action::Tasks | Action::Errands) => other,
    })
}

/// The first word and the rest (leading spaces dropped from both).
fn first_word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    match s.find(char::is_whitespace) {
        Some(i) => (&s[..i], s[i..].trim_start()),
        None => (s, ""),
    }
}

fn numberish(w: &str) -> bool {
    !w.is_empty() && w.chars().all(|c| c.is_ascii_digit() || "+-().".contains(c) || ('０'..='９').contains(&c))
}

fn digit_count(w: &str) -> usize {
    w.chars().filter(|c| c.is_ascii_digit() || ('０'..='９').contains(c)).count()
}

/// "/errand <number or contact> <task>". A number may be written in groups
/// ("02 2345 6789"): groups of three or more digits are joined until it has
/// ten digits, so the "2" of "0912345678 2 people" stays in the task. A name
/// with spaces goes in quotes.
fn parse_errand(args: &str) -> Option<Action> {
    let args = args.trim();
    if let Some(rest) = args.strip_prefix('"').or_else(|| args.strip_prefix('“')) {
        let end = rest.find(['"', '”'])?;
        let task = rest[end..].chars().skip(1).collect::<String>();
        return Some(Action::Errand { target: rest[..end].to_string(), task });
    }
    let (first, mut rest) = first_word(args);
    if first.is_empty() {
        return None;
    }
    let mut target = first.to_string();
    if numberish(first) {
        let mut digits = digit_count(first);
        while digits < 10 {
            let (next, after) = first_word(rest);
            if !(numberish(next) && digit_count(next) >= 3) {
                break;
            }
            target.push_str(next);
            digits += digit_count(next);
            rest = after;
        }
    }
    Some(Action::Errand { target, task: rest.to_string() })
}

/// A list number ("1", "#2") or an errand id.
fn looks_like_ref(w: &str) -> bool {
    let n = w.trim_start_matches('#');
    (!n.is_empty() && n.len() <= 2 && n.chars().all(|c| c.is_ascii_digit()))
        || (w.len() == 17 && w.starts_with('e') && w.chars().all(|c| c.is_ascii_alphanumeric()))
}

fn parse_say(args: &str) -> Option<Action> {
    let (first, rest) = first_word(args);
    if looks_like_ref(first) && !rest.is_empty() {
        return Some(Action::Say { r: first.to_string(), text: rest.to_string() });
    }
    Some(Action::Say { r: String::new(), text: args.to_string() })
}

/// "/mode <phone> ai|normal" (the phone's name may have spaces).
fn parse_mode(args: &str) -> Option<Action> {
    let args = args.trim();
    let (phone, last) = args.rsplit_once(char::is_whitespace)?;
    let mode = mode_word(last)?;
    Some(Action::Mode { phone: phone.trim().to_string(), mode })
}

pub fn day_word(w: &str) -> Option<Day> {
    match w.trim().to_lowercase().as_str() {
        "today" | "今天" | "今日" | "오늘" | "hoy" | "aujourd'hui" | "heute" | "hoje" => Some(Day::Today),
        "yesterday" | "昨天" | "昨日" | "어제" | "ayer" | "hier" | "gestern" | "ontem" => Some(Day::Yesterday),
        other => NaiveDate::parse_from_str(other, "%Y-%m-%d").ok().map(Day::On),
    }
}

pub fn only_word(w: &str) -> Option<Only> {
    match w.trim().to_lowercase().as_str() {
        "missed" | "未接" | "未接來電" | "未接来电" | "不在" | "不在着信" | "未応答" | "부재중" | "미응답" | "perdidas" | "perdida"
        | "perdidos" | "manqués" | "manques" | "manquées" | "manque" | "verpasst" | "verpasste" => Some(Only::Missed),
        "scam" | "spam" | "詐騙" | "诈骗" | "推銷" | "推销" | "詐欺" | "迷惑" | "勧誘" | "営業" | "사기" | "스팸" | "보이스피싱" | "영업"
        | "estafa" | "estafas" | "fraude" | "arnaque" | "arnaques" | "démarchage" | "demarchage" | "betrug" | "werbung" | "golpe"
        | "golpes" => Some(Only::Scam),
        _ => None,
    }
}

fn parse_calls(args: &str) -> CallQuery {
    let mut q = CallQuery::default();
    let mut words: Vec<&str> = Vec::new();
    for w in args.split_whitespace() {
        if let Some(d) = day_word(w) {
            q.day = Some(d);
        } else if let Some(o) = only_word(w) {
            q.only = o;
        } else if w.len() <= 2 && w.chars().all(|c| c.is_ascii_digit()) {
            q.limit = w.parse().unwrap_or(CALLS_DEFAULT);
        } else {
            words.push(w);
        }
    }
    q.text = words.join(" ");
    q
}

/// A slash command ("/calls today", "/errand 0912345678 book a table"). A bot
/// name after the command ("/calls@my_bot") is the adapter's to check; it is
/// dropped here.
pub fn parse_slash(text: &str) -> Parsed {
    let Some(rest) = text.trim().strip_prefix('/') else {
        return Parsed::Unknown;
    };
    let (head, args) = first_word(rest);
    let name = head.split('@').next().unwrap_or("").to_ascii_lowercase();
    let need = |a: &str| (!a.trim().is_empty()).then(|| a.trim().to_string());
    let action = match name.as_str() {
        "help" | "start" => Some(Action::Help),
        "status" => Some(Action::Status),
        "calls" => Some(Action::Calls(parse_calls(args))),
        "call" => need(args).map(|r| Action::Call { r }),
        "tasks" => Some(Action::Tasks),
        "confirm" => need(args).map(|r| Action::Decide { r, confirm: true }),
        "ignore" => need(args).map(|r| Action::Decide { r, confirm: false }),
        "errand" => parse_errand(args),
        "errands" => Some(Action::Errands),
        "cancel" => Some(Action::Cancel { r: args.trim().to_string() }),
        "say" => need(args).and_then(|a| parse_say(&a)),
        "answer" => need(args).map(|text| Action::Answer { text }),
        "mode" => parse_mode(args),
        "contacts" => Some(Action::Contacts { query: args.trim().to_string() }),
        "block" | "blacklist" => parse_contact_kind("blacklist", args),
        "stall" | "stalls" => parse_contact_kind("stall", args),
        "white" | "whitelist" => parse_contact_kind("whitelist", args),
        "unblock" => parse_contact_kind("unblock", args),
        _ => return Parsed::Unknown,
    };
    let command = match name.as_str() {
        "block" | "blacklist" => "block",
        "stall" | "stalls" => "stall",
        "white" | "whitelist" => "white",
        "unblock" => "unblock",
        _ => COMMANDS.iter().copied().find(|c| *c == name).unwrap_or("help"),
    };
    match action.map(validate) {
        Some(Ok(a)) => Parsed::Action(a),
        _ => Parsed::Usage(command),
    }
}

fn parse_contact_kind(kind: &str, args: &str) -> Option<Action> {
    let (first, mut rest) = first_word(args.trim());
    if first.is_empty() || !numberish(first) {
        return None;
    }
    let mut number = first.to_string();
    let mut digits = digit_count(first);
    while digits < 10 {
        let (next, after) = first_word(rest);
        if !(numberish(next) && digit_count(next) >= 3) {
            break;
        }
        number.push_str(next);
        digits += digit_count(next);
        rest = after;
    }
    Some(Action::SetContactKind {
        kind: kind.to_string(),
        number,
        name: rest.trim().to_string(),
    })
}

/// The free-text model's answer ({"action": …, fields}) as an action, through
/// the same checks as a typed command. None: no action, or not a valid one.
pub fn from_model(v: &Value) -> Option<Action> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    let action = match s("action").as_str() {
        "help" => Action::Help,
        "status" => Action::Status,
        "calls" => Action::Calls(CallQuery {
            limit: v.get("limit").and_then(Value::as_u64).map_or(CALLS_DEFAULT, |n| n.min(CALLS_MAX as u64) as usize),
            day: day_word(&s("day")),
            only: only_word(&s("only")).unwrap_or_default(),
            text: s("query"),
        }),
        "call" => Action::Call { r: s("ref") },
        "tasks" => Action::Tasks,
        "confirm" => Action::Decide { r: s("ref"), confirm: true },
        "ignore" => Action::Decide { r: s("ref"), confirm: false },
        "errand" => Action::Errand { target: s("target"), task: s("task") },
        "errands" => Action::Errands,
        "cancel" => Action::Cancel { r: s("ref") },
        "say" => Action::Say { r: s("ref"), text: s("text") },
        "answer" => Action::Answer { text: s("text") },
        "mode" => Action::Mode { phone: s("phone"), mode: mode_word(&s("mode"))? },
        "contacts" => Action::Contacts { query: s("query") },
        "block" => Action::SetContactKind { kind: "blacklist".into(), number: s("target"), name: s("text") },
        "stall" => Action::SetContactKind { kind: "stall".into(), number: s("target"), name: s("text") },
        "white" => Action::SetContactKind { kind: "whitelist".into(), number: s("target"), name: s("text") },
        "unblock" => Action::SetContactKind { kind: "unblock".into(), number: s("target"), name: String::new() },
        _ => return None,
    };
    validate(action).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn action(text: &str) -> Action {
        match parse_slash(text) {
            Parsed::Action(a) => a,
            other => panic!("{text}: {other:?}"),
        }
    }

    #[test]
    fn mode_takes_ia_and_ki_for_the_ai() {
        for w in ["ai", "IA", "ia", "KI", "ki"] {
            assert_eq!(action(&format!("/mode Pixel 9 {w}")), Action::Mode { phone: "Pixel 9".into(), mode: PhoneMode::Ai }, "{w}");
        }
        assert_eq!(action("/mode Pixel normal"), Action::Mode { phone: "Pixel".into(), mode: PhoneMode::Normal });
        // What the free-text model writes goes through the same check.
        for w in ["ia", "ki"] {
            let v = json!({"action": "mode", "phone": "pixel", "mode": w});
            assert_eq!(from_model(&v), Some(Action::Mode { phone: "pixel".into(), mode: PhoneMode::Ai }), "{w}");
        }
    }

    #[test]
    fn calls_filters_read_missed_and_scam_in_every_language() {
        for w in ["missed", "未接", "不在", "不在着信", "未応答", "부재중", "perdidas", "manqués", "verpasst"] {
            assert_eq!(only_word(w), Some(Only::Missed), "{w}");
        }
        for w in ["scam", "詐騙", "詐欺", "迷惑", "勧誘", "사기", "스팸", "estafa", "arnaque", "betrug", "golpe", "fraude"] {
            assert_eq!(only_word(w), Some(Only::Scam), "{w}");
        }
        assert_eq!(only_word("hello"), None);
        // Typed in a command it is a filter, not a word to look for in the calls.
        for (text, only) in [("/calls perdidas", Only::Missed), ("/calls 不在 today", Only::Missed), ("/calls arnaque", Only::Scam)] {
            match action(text) {
                Action::Calls(q) => assert_eq!((q.only, q.text.as_str()), (only, ""), "{text}"),
                other => panic!("{text}: {other:?}"),
            }
        }
    }

    #[test]
    fn slash_commands_read_into_actions() {
        assert_eq!(action("/help"), Action::Help);
        assert_eq!(action("/start"), Action::Help);
        assert_eq!(action("/status@bageo_bot"), Action::Status);
        assert_eq!(action("/call 2"), Action::Call { r: "2".into() });
        assert_eq!(action("/confirm #1"), Action::Decide { r: "#1".into(), confirm: true });
        assert_eq!(action("/ignore 3"), Action::Decide { r: "3".into(), confirm: false });
        assert_eq!(action("/cancel"), Action::Cancel { r: String::new() });
        assert_eq!(action("/say 2 請說九點到"), Action::Say { r: "2".into(), text: "請說九點到".into() });
        assert_eq!(action("/say 九點到"), Action::Say { r: String::new(), text: "九點到".into() });
        assert_eq!(action("/answer 可以"), Action::Answer { text: "可以".into() });
        assert_eq!(action("/mode Pixel 7a normal"), Action::Mode { phone: "Pixel 7a".into(), mode: PhoneMode::Normal });
        assert_eq!(action("/mode 1 ai"), Action::Mode { phone: "1".into(), mode: PhoneMode::Ai });
        assert_eq!(action("/contacts 王小明"), Action::Contacts { query: "王小明".into() });
    }

    #[test]
    fn call_lists_take_a_count_a_day_a_kind_and_words_in_any_order() {
        let Action::Calls(q) = action("/calls scam 10 yesterday 銀行") else { panic!() };
        assert_eq!((q.limit, q.day, q.only, q.text.as_str()), (10, Some(Day::Yesterday), Only::Scam, "銀行"));
        let Action::Calls(q) = action("/calls 2026-11-14 missed") else { panic!() };
        assert_eq!(q.day, Some(Day::On(NaiveDate::from_ymd_opt(2026, 11, 14).unwrap())));
        assert_eq!(q.only, Only::Missed);
        assert_eq!(q.limit, CALLS_DEFAULT);
        let Action::Calls(q) = action("/calls 99 0912345678") else { panic!() };
        assert_eq!((q.limit, q.text.as_str()), (CALLS_MAX, "0912345678"));
        assert_eq!(Day::Yesterday.date(NaiveDate::from_ymd_opt(2026, 3, 1).unwrap()), NaiveDate::from_ymd_opt(2026, 2, 28).unwrap());
    }

    #[test]
    fn an_errand_number_written_in_groups_stays_one_number_and_the_task_keeps_its_digits() {
        assert_eq!(
            action("/errand 0912345678 2 people at 7"),
            Action::Errand { target: "0912345678".into(), task: "2 people at 7".into() }
        );
        assert_eq!(
            action("/errand 02 2345 6789 訂明天晚上七點兩位"),
            Action::Errand { target: "0223456789".into(), task: "訂明天晚上七點兩位".into() }
        );
        assert_eq!(
            action("/errand +886 912 345 678 問營業時間"),
            Action::Errand { target: "+886912345678".into(), task: "問營業時間".into() }
        );
        assert_eq!(action("/errand 王小明 約週六吃飯"), Action::Errand { target: "王小明".into(), task: "約週六吃飯".into() });
        assert_eq!(
            action("/errand \"Wang Xiao Ming\" lunch on Saturday"),
            Action::Errand { target: "Wang Xiao Ming".into(), task: "lunch on Saturday".into() }
        );
    }

    #[test]
    fn a_command_written_wrong_gets_its_usage_and_others_are_not_ours() {
        assert_eq!(parse_slash("/errand 0912345678"), Parsed::Usage("errand"));
        assert_eq!(parse_slash("/call"), Parsed::Usage("call"));
        assert_eq!(parse_slash("/mode pixel sideways"), Parsed::Usage("mode"));
        assert_eq!(parse_slash("/call ../../etc/passwd"), Parsed::Usage("call"));
        assert_eq!(parse_slash(&format!("/say {}", "長".repeat(SAY_MAX_CHARS + 1))), Parsed::Usage("say"));
        assert_eq!(parse_slash("/weather"), Parsed::Unknown);
        assert_eq!(parse_slash("hello"), Parsed::Unknown);
    }

    #[test]
    fn the_models_answer_passes_the_same_checks_as_a_typed_command() {
        let errand = json!({"action": "errand", "target": "0912345678", "task": "訂明天晚上七點兩位"});
        assert_eq!(from_model(&errand), Some(Action::Errand { target: "0912345678".into(), task: "訂明天晚上七點兩位".into() }));
        let calls = json!({"action": "calls", "day": "yesterday", "limit": 500, "only": "nonsense"});
        assert_eq!(from_model(&calls), Some(Action::Calls(CallQuery { limit: CALLS_MAX, day: Some(Day::Yesterday), ..Default::default() })));
        // Unknown actions, missing fields, out-of-range values: nothing.
        assert_eq!(from_model(&json!({"action": "delete_everything"})), None);
        assert_eq!(from_model(&json!({"action": "none"})), None);
        assert_eq!(from_model(&json!({"action": "errand", "target": "0912345678"})), None);
        assert_eq!(from_model(&json!({"action": "call", "ref": "1; rm -rf /"})), None);
        assert_eq!(from_model(&json!({"action": "mode", "phone": "pixel", "mode": "root"})), None);
        assert_eq!(from_model(&json!({"action": "errand", "target": "x".repeat(200), "task": "a"})), None);
        assert_eq!(from_model(&json!("errand")), None);
    }
}

