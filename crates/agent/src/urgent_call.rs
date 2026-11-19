// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Whitelist urgent call: with `whitelist_mode` emergency and `whitelist_forward_number`
//! filled, an urgent Whitelist caller is put through to the owner.
//!
//! A Whitelist caller (whitelist_mode emergency) says it is urgent: the AI asks
//! them to hold on, the agent sends the phone add_call on the call's media
//! connection, and the phone dials the owner's number (whitelist_forward_number)
//! from the SIM of this call and merges the two calls. Merged: the AI leaves the
//! call (it stays up and is recorded). Anything else: the phone ends the second
//! call, takes this one off hold and the AI tells the caller the owner did not
//! answer.
//!
//! Agent -> phone: {"type":"add_call","id":…,"number":…,"merge":true,"answer_timeout_ms":25000}
//!                 {"type":"add_call_abort","id":…} (no final result in time)
//! Phone -> agent: {"type":"add_call_result","id":…,"status":"dialing"|"active"|"merged"|"failed","reason":…}

use crate::locale::Lang;
use crate::settings_store::RuntimeSettings;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// How long the phone lets the owner's phone ring.
pub const OWNER_ANSWER_TIMEOUT: Duration = Duration::from_secs(25);
/// How long the agent waits for the phone to settle it (dial, ring, merge)
/// before it treats the attempt as failed.
pub const RESULT_WAIT: Duration = Duration::from_secs(50);
/// Longest wait for the AI's "hold on" line to play before the phone dials.
pub const HOLD_LINE_MAX_WAIT: Duration = Duration::from_secs(10);
/// No `dialing` from the phone this soon after add_call: its app does not know the
/// command (an older version); the AI comes back at once.
pub const DIAL_ACK_WAIT: Duration = Duration::from_secs(5);
/// After add_call_abort: how long a final result may still come.
pub const ABORT_WAIT: Duration = Duration::from_secs(8);
/// After the attempt was given up: a late "merged" still hands the call over.
pub const LATE_MERGE_WAIT: Duration = Duration::from_secs(90);

/// The owner's number to dial: whitelist_mode emergency and whitelist_forward_number
/// filled. The old setting `whitelist_urgent_call` is still read from settings files
/// and backups but no longer changes anything.
pub fn number_to_call(rt: &RuntimeSettings) -> Option<String> {
    let number = rt.whitelist_forward_number.trim();
    (rt.whitelist_mode == "emergency" && !number.is_empty()).then(|| number.to_string())
}

/// Who a call that may be forwarded to the owner comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferWho {
    /// A Whitelist contact (setting `transfer_whitelist`).
    Whitelist,
    /// Any other contact off the Blacklist and the stall list (`transfer_contacts`).
    Contact,
    /// A number not in the contacts, hidden numbers too (`transfer_others`).
    Stranger,
}

/// A call that may be forwarded: who it is from, and the contact's name
/// (empty for a number not in the contacts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    pub who: TransferWho,
    pub name: String,
}

/// Whether a caller may be forwarded (`whitelist_mode` direct or emergency).
/// `kind`: the contact list the number is on (None: not in the contacts);
/// `blocked`: the number is turned away (Blacklist, also by a prefix, or a
/// blocked number of an older version). Blacklist and stall-list contacts never
/// are; a Whitelist contact is even when a Blacklist prefix covers its number.
pub fn transfer_who(rt: &RuntimeSettings, kind: Option<&str>, blocked: bool) -> Option<TransferWho> {
    if !matches!(rt.whitelist_mode.as_str(), "direct" | "emergency") {
        return None;
    }
    match kind {
        Some("whitelist") => rt.transfer_whitelist.then_some(TransferWho::Whitelist),
        Some("" | "contact") => (rt.transfer_contacts && !blocked).then_some(TransferWho::Contact),
        Some(_) => None,
        None => (rt.transfer_others && !blocked).then_some(TransferWho::Stranger),
    }
}

/// [`transfer_who`] for the number `caller` on a call that came in on the SIM
/// of country `sim`. The one place that decides whether a call is forwarded
/// (direct) or gets the urgent part and the call to the owner (emergency). Our
/// own calls (call-backs, errands) are left out by the callers.
pub fn transfer_target(rt: &RuntimeSettings, caller: &str, sim: Option<&str>) -> Option<Transfer> {
    if !matches!(rt.whitelist_mode.as_str(), "direct" | "emergency") {
        return None;
    }
    let contact = crate::contacts::is_whitelist(caller, sim).or_else(|| crate::contacts::lookup(caller, sim));
    let blocked = crate::settings_store::reject_number_hit_on(rt.pending_rejects(), caller, sim);
    let who = transfer_who(rt, contact.as_ref().map(|c| c.kind.as_str()), blocked)?;
    Some(Transfer {
        who,
        name: contact.map(|c| c.name).unwrap_or_default(),
    })
}

/// A call judged sales / scam (stalling started, or a judgement that stands)
/// gets no urgent alert and no call to the owner, whoever the caller is.
pub fn urgent_blocked_by_scam(stall_engaged: bool, judged: Option<&crate::calls::ScamJudgement>) -> bool {
    stall_engaged || judged.is_some_and(crate::calls::ScamJudgement::stands)
}

/// A number not in the contacts gets the owner dialed at most once in this
/// long; hidden numbers share one allowance. Contacts have no such limit.
pub const STRANGER_DIAL_EVERY: Duration = Duration::from_secs(30 * 60);

/// When the owner was last dialed for each number not in the contacts (memory only).
#[derive(Debug, Default)]
pub struct DialLimit(Mutex<HashMap<String, Instant>>);

impl DialLimit {
    /// The allowance of `key` is free at `now`.
    pub fn free(&self, key: &str, now: Instant) -> bool {
        let g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        g.get(key).is_none_or(|t| now.saturating_duration_since(*t) >= STRANGER_DIAL_EVERY)
    }

    /// Uses the allowance of `key`: false when it was used less than
    /// [`STRANGER_DIAL_EVERY`] ago.
    pub fn take(&self, key: &str, now: Instant) -> bool {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if g.get(key).is_some_and(|t| now.saturating_duration_since(*t) < STRANGER_DIAL_EVERY) {
            return false;
        }
        g.retain(|_, t| now.saturating_duration_since(*t) < STRANGER_DIAL_EVERY);
        g.insert(key.to_string(), now);
        true
    }
}

pub fn stranger_dials() -> &'static DialLimit {
    static L: OnceLock<DialLimit> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// The key a number not in the contacts is limited under; hidden numbers share one.
pub fn stranger_key(caller: &str, sim: Option<&str>) -> String {
    crate::scam_db::number_key(caller, sim).unwrap_or_else(|| "hidden".to_string())
}

/// Whether the phone may dial the owner for `t` on this call: the owner's number is set
/// ([`number_to_call`]) and, for a number not in the contacts, its allowance
/// (`key`, [`stranger_key`]) is free. Otherwise the AI says the owner will call
/// back and the owner is only alerted.
pub fn will_dial(rt: &RuntimeSettings, t: &Transfer, key: &str, limits: &DialLimit, now: Instant) -> bool {
    number_to_call(rt).is_some() && (t.who != TransferWho::Stranger || limits.free(key, now))
}

/// The owner's number as the phone may dial it: full `+` numbers are checked
/// against their own country (`phone_region::normalize`), even when the incoming
/// phone carries another country's SIM (`sim`); numbers without `+` use the
/// incoming phone's SIM (`sim`, ISO country) or fall back to the owner's
/// country (`home_code`, `effective_country_code`). Emergency, short,
/// premium-rate, MMI codes, and non-`+` international prefixes are rejected.
pub fn check_owner_number(number: &str, sim: Option<&str>, home_code: &str) -> Result<String, &'static str> {
    if number.contains('*') || number.contains('#') {
        return Err("bad_number");
    }
    let clean = crate::errands::clean_number(number).ok_or("bad_number")?;
    let region = if clean.starts_with('+') {
        crate::phone_region::normalize(&clean, None)
            .ok_or("bad_number")?
            .country
            .to_string()
    } else {
        match sim.map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => s.to_ascii_lowercase(),
            None => crate::phone_region::country_of_code(home_code.trim())
                .ok_or("bad_number")?
                .to_string(),
        }
    };
    crate::errands::check_number(number, &region)
}

/// The phone's add_call command.
pub fn add_call_payload(id: &str, number: &str) -> Value {
    json!({
        "type": "add_call",
        "id": id,
        "number": number,
        "merge": true,
        "answer_timeout_ms": OWNER_ANSWER_TIMEOUT.as_millis() as u64,
    })
}

/// The phone's add_call_abort: end the second call and take this one off hold
/// (merged already: it answers "merged").
pub fn abort_payload(id: &str) -> Value {
    json!({ "type": "add_call_abort", "id": id })
}

/// The AI's hold-on line (urgent_block with the call on) in what it said, in any
/// of the nine languages. Dialing waits for it. Both sides are folded first (no
/// spaces, punctuation or accents, one width, one case, simplified Chinese), so a
/// transcript like "neraccroche pas" still reads as "ne raccroche pas".
pub fn is_hold_line(ai_said: &str) -> bool {
    hold_cues(false, ai_said)
}

/// [is_hold_line] with only the wordings that ask the caller not to hang up or to
/// stay on the line ("hold on", "ちょっと待って" also say "one moment" in passing):
/// what counts before the caller's words named an emergency.
pub fn is_strong_hold_line(ai_said: &str) -> bool {
    hold_cues(true, ai_said)
}

fn hold_cues(strong_only: bool, ai_said: &str) -> bool {
    const STRONG: &[&str] = &[
        // Chinese (folded to simplified)
        "不要挂", "别挂", "先别走", "别走开", "不要走开",
        // English
        "don't hang up", "do not hang up", "stay on the line", "stay on the phone", "please hold", "hold the line", "don't go anywhere",
        // Japanese
        "切らないで", "切らずに", "そのままお待ち", "そのまま待って", "そのままで待って", "そのままでいて",
        // Korean
        "끊지 마", "끊지 말", "끊지 않",
        // Spanish
        "no cuelgue", "no cuelgues", "no cortes", "no corte", "quédese en la línea", "quédate en la línea", "siga en la línea", "sigue en la línea",
        "no se vaya", "no te vayas",
        // French
        "ne raccroche", "ne quittez pas", "ne quitte pas", "ne coupez pas", "ne coupe pas", "restez en ligne", "reste en ligne",
        // German
        "nicht auflegen", "leg nicht auf", "legen sie nicht auf", "legt nicht auf", "bleib dran", "bleiben sie dran", "bleiben sie bitte dran",
        "bleib bitte dran", "bleib am apparat", "bleiben sie am apparat",
        // Portuguese
        "não desligue", "não desliga", "não desliguem", "fique na linha", "fica na linha", "aguarde na linha",
        // Italian
        "non riattaccare", "non riattacchi", "non chiudere", "non chiuda", "rimani in linea", "rimanga in linea", "resti in linea", "resta in linea",
        "attenda in linea", "aspetti in linea",
        "non attacchi", "non attaccare", "non metta giù", "non mettere giù", "resti in attesa", "rimanga in attesa", "rimani in attesa",
        "non si allontani", "non andare via",
        // Russian
        "не кладите трубку", "не клади трубку", "не вешайте трубку", "не вешай трубку", "оставайтесь на линии", "оставайся на линии",
        "подождите на линии", "будьте на линии",
        // Hindi
        "फोन मत काटिए", "फ़ोन मत काटिए", "फोन मत काटना", "लाइन पर रहिए", "लाइन पर रहें", "लाइन पर बने रहिए", "कॉल मत काटिए", "हटिएगा नहीं",
        // Arabic
        "لا تقفل الخط", "لا تغلق الخط", "لا تسكر الخط", "ابقي على الخط", "ابق على الخط", "خليك على الخط", "خلك على الخط", "انتظر على الخط",
        // Dutch
        "hang niet op", "niet ophangen", "blijf aan de lijn", "blijft u aan de lijn", "wacht aan de lijn",
        // Polish
        "nie rozlaczaj sie", "nie rozłączaj się", "prosze sie nie rozlaczac", "proszę się nie rozłączać",
        "nie odkladaj sluchawki", "nie odkładaj słuchawki", "zostan na linii", "zostań na linii", "prosze zostac na linii", "proszę zostać na linii",
        // Romanian
        "nu inchide", "nu închide", "nu inchideti", "nu închideți", "ramaneti pe fir", "rămâneți pe fir", "ramai pe fir", "rămâi pe fir",
        "asteptati pe fir", "așteptați pe fir",
        // Ukrainian
        "не кладіть слухавку", "не клади слухавку", "не вибивайте", "залишайтесь на лінії", "залишайтеся на лінії", "будьте на лінії",
        // Bengali
        "ফোন কাটবেন না", "কল কাটবেন না", "লাইনে থাকুন", "লাইন ধরে থাকুন",
        // Marathi
        "फोन ठेवू नका", "कॉल कट करू नका", "लाईनवर राहा", "लाइनवर थांबा",
        // Tamil
        "போனை வைக்காதீர்கள்", "அழைப்பை துண்டிக்காதீர்கள்", "லைனில் இருங்கள்", "லைனில் காத்திருங்கள்",
        // Telugu
        "ఫోన్ కట్ చేయకండి", "కాల్ కట్ చేయవద్దు", "లైన్లో ఉండండి", "లైన్ లోనే ఉండండి",
        // Cantonese (folded to simplified: 唔好掛線 and 唔好挂线 read the same)
        "唔好挂", "唔好收线", "咪挂", "咪收线", "唔好走开", "咪走开", "唔好挂线", "咪挂线",
        // Vietnamese (accents folded)
        "đừng cúp máy", "đừng gác máy", "đừng cắt máy", "đừng ngắt máy", "đừng tắt máy", "đừng cúp điện thoại", "không được cúp máy",
        "vui lòng giữ máy", "xin giữ máy", "giữ máy nhé", "giữ máy giúp", "giữ máy chút", "giữ máy một",
        // Indonesian
        "jangan tutup telepon", "jangan tutup telpon", "jangan ditutup telepon", "jangan ditutup telpon", "jangan menutup telepon",
        "jangan matikan telepon", "jangan matikan telpon", "jangan dimatikan telepon", "jangan putus telepon",
        "jangan tutup dulu", "jangan ditutup dulu", "jangan matikan dulu", "tetap di telepon", "tetap di saluran", "tetap di line", "tetap tersambung",
        // Thai
        "อย่าวางสาย", "อย่าเพิ่งวางสาย", "ห้ามวางสาย", "ถือสายไว้", "ถือสายรอ", "รออยู่ในสาย", "อยู่ในสายก่อน", "อยู่ในสายนะ", "กรุณารอสาย", "โปรดรอสาย",
        // Filipino (Tagalog)
        "huwag ibaba", "huwag mong ibaba", "huwag mo munang ibaba", "huwag munang ibaba", "huwag po munang ibaba", "huwag po ibaba",
        "huwag po ninyong ibaba", "huwag po kayong magbaba", "huwag magbaba", "huwag mong patayin", "wag ibaba", "wag mong ibaba",
        "wag mo munang ibaba", "wag munang ibaba", "wag po munang ibaba", "manatili sa linya", "manatili po sa linya",
        "dito lang po kayo sa linya", "dito ka lang sa linya", "maghintay po sa linya",
        // Turkish
        "kapatmayın", "telefonu kapatmayın", "kapatma", "hatta kalın", "hatta kal", "hatta bekleyin", "hattan ayrılmayın", "ayrılmayın",
    ];
    const WEAK: &[&str] = &[
        "请稍候", "请稍等", "稍等一下", "你等一下", "等我一下", "先等一下", "hold on", "hang on", "少々お待ち", "お待ちください", "ちょっと待って", "待っててね", "待ってて",
        "잠깐만 기다려", "잠시만 기다려", "기다려 주세요", "기다려 줘", "espera un momento", "espere un momento", "patientez",
        "ne bougez pas", "ne bouge pas", "nicht weggehen", "aguarde um pouco", "espera um pouco", "espere um pouco",
        // Turkish
        "bir dakika", "bir saniye", "lütfen bekleyin",
        // Italian
        "aspetti un attimo", "aspetta un attimo", "aspetti un momento", "aspetta un momento", "attenda un attimo", "attenda un momento",
        "solo un attimo", "solo un momento", "un attimo di pazienza", "un secondo",
    ];
    static FOLDED: std::sync::OnceLock<(Vec<String>, Vec<String>)> = std::sync::OnceLock::new();
    let (strong, weak) = FOLDED.get_or_init(|| {
        let fold = |l: &[&str]| l.iter().map(|c| crate::urgency::fold_compact(c)).collect::<Vec<_>>();
        (fold(STRONG), fold(WEAK))
    });
    let said = crate::urgency::fold_compact(ai_said);
    strong.iter().chain(if strong_only { [].iter() } else { weak.iter() }).any(|c| said.contains(c.as_str()))
}

/// The same sentence twice in what the AI said (a line it repeats while it waits:
/// its hold-on line, whatever the wording). Sentences of fewer than six letters
/// ("喂？", "Okay.") do not count.
pub fn repeats_a_line(ai_said: &str) -> bool {
    let mut seen = std::collections::HashSet::new();
    ai_said
        .split(['.', '!', '?', '。', '！', '？', '\n', '…', ';', '；', '，', ','])
        .map(crate::urgency::fold_compact)
        .filter(|s| s.chars().count() >= 6)
        .any(|s| !seen.insert(s))
}

/// A turn for the model with the language it must say it in: the one the caller
/// speaks on this call (`spoken`, from their words), whatever the language of the
/// instruction; unknown: the caller's language all the same.
pub fn with_caller_language(turn: &str, spoken: Option<Lang>) -> String {
    let note = match spoken {
        Some(l) => {
            let name = crate::urgency::language_name(l);
            format!(" [Language: the caller is speaking {name} on this call. Say it in {name}, whatever language this note is in.]")
        }
        None => " [Language: say it in the language the caller is speaking on this call, whatever language this note is in.]".to_string(),
    };
    let Ok(mut v) = serde_json::from_str::<Value>(turn) else {
        return turn.to_string();
    };
    match v.pointer_mut("/clientContent/turns/0/parts/0/text") {
        Some(Value::String(text)) => {
            text.push_str(&note);
            v.to_string()
        }
        _ => turn.to_string(),
    }
}

/// One add_call_result from the phone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    Dialing,
    Active,
    Merged,
    Failed(String),
}

/// A phone's add_call_result: its id and what happened. The reason is kept to a
/// short code (letters, digits, _ and -).
pub fn parse_result(msg: &Value) -> Option<(String, Report)> {
    if msg.get("type")?.as_str()? != "add_call_result" {
        return None;
    }
    let id = msg.get("id")?.as_str()?.trim();
    if id.is_empty() {
        return None;
    }
    let report = match msg.get("status")?.as_str()? {
        "dialing" => Report::Dialing,
        "active" => Report::Active,
        "merged" => Report::Merged,
        "failed" => Report::Failed(reason_code(msg.get("reason").and_then(|v| v.as_str()).unwrap_or(""))),
        _ => return None,
    };
    Some((id.to_string(), report))
}

fn reason_code(s: &str) -> String {
    let code: String = s
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(48)
        .collect();
    if code.is_empty() {
        "unknown".to_string()
    } else {
        code
    }
}

/// What the agent does after a report or the wait running out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Keep waiting.
    Wait,
    /// The owner is on the call: the AI leaves it.
    Merged,
    /// Not merged: the AI comes back and says so.
    Failed(String),
}

/// Where one attempt stands. Settled once (merged or failed); later reports
/// are ignored.
#[derive(Debug, Default)]
pub struct Tracker {
    dialing: bool,
    active: bool,
    settled: bool,
}

impl Tracker {
    pub fn on_report(&mut self, report: Report) -> Step {
        if self.settled {
            return Step::Wait;
        }
        match report {
            Report::Dialing => {
                self.dialing = true;
                Step::Wait
            }
            Report::Active => {
                self.active = true;
                Step::Wait
            }
            Report::Merged => {
                self.settled = true;
                Step::Merged
            }
            Report::Failed(reason) => {
                self.settled = true;
                Step::Failed(reason)
            }
        }
    }

    /// No final result in RESULT_WAIT, or the phone could not be reached.
    pub fn on_timeout(&mut self) -> Step {
        if self.settled {
            return Step::Wait;
        }
        self.settled = true;
        let reason = if self.active {
            "merge_timeout"
        } else if self.dialing {
            "no_result"
        } else {
            "phone_no_reply"
        };
        Step::Failed(reason.to_string())
    }

    pub fn settled(&self) -> bool {
        self.settled
    }
}

/// Shared by the agent's watcher and the Live session of one call.
#[derive(Debug, Default)]
pub struct CallCtl {
    /// The phone is dialing the owner: no AI audio goes out, the caller's audio
    /// does not go to the model, and the session neither nudges nor hangs up.
    paused: AtomicBool,
    /// Merged: the session ends without a hangup.
    handed: AtomicBool,
    /// A turn for the model (the "did not answer" line), sent once.
    say: Mutex<Option<String>>,
    /// When the AI's output first held the hold-on line and `delivered_bytes` at
    /// the start of that turn (so only bytes from that turn onward count).
    hold_said_at: Mutex<Option<(Instant, u64)>>,
    /// No phone call on this call any more (fallback taken, or tried once).
    dial_blocked: AtomicBool,
    /// The Live session ended while the AI was paused: nobody can talk to the caller.
    session_gone: AtomicBool,
    /// A number not in the contacts: the key its dial allowance is under
    /// ([`stranger_key`]), used when the phone dials ([`DialLimit::take`]).
    limit_key: Option<String>,
}

impl CallCtl {
    /// For a caller whose call to the owner counts against [`STRANGER_DIAL_EVERY`].
    pub fn limited(key: String) -> Self {
        Self {
            limit_key: Some(key),
            ..Self::default()
        }
    }

    pub fn limit_key(&self) -> Option<&str> {
        self.limit_key.as_deref()
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::Relaxed);
    }

    /// Back to the caller with one turn for the model.
    pub fn resume_with(&self, turn: String) {
        if let Ok(mut g) = self.say.lock() {
            *g = Some(turn);
        }
        self.paused.store(false, Ordering::Relaxed);
    }

    pub fn hand_over(&self) {
        self.handed.store(true, Ordering::Relaxed);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn is_handed(&self) -> bool {
        self.handed.load(Ordering::Relaxed)
    }

    /// AI audio and the caller's audio to the model are held back.
    pub fn ai_quiet(&self) -> bool {
        self.is_paused() || self.is_handed()
    }

    /// The agent must not hang up now (dialing the owner, or the owner is on).
    pub fn blocks_hangup(&self) -> bool {
        self.ai_quiet()
    }

    pub fn take_say(&self) -> Option<String> {
        self.say.lock().ok().and_then(|mut g| g.take())
    }

    /// The AI's output held the hold-on line (first time counts).
    pub fn note_hold_line(&self) {
        self.note_hold_line_from(0);
    }

    /// Record when the hold-on line was spoken along with `base_bytes` of
    /// `delivered_bytes` at the start of the turn (first time counts).
    pub fn note_hold_line_from(&self, base_bytes: u64) {
        if let Ok(mut g) = self.hold_said_at.lock() {
            g.get_or_insert_with(|| (Instant::now(), base_bytes));
        }
    }

    pub fn hold_said(&self) -> Option<(Instant, u64)> {
        self.hold_said_at.lock().ok().and_then(|g| *g)
    }

    pub fn hold_said_at(&self) -> Option<Instant> {
        self.hold_said().map(|(t, _)| t)
    }

    /// No (further) phone call on this call. True the first time.
    pub fn block_dial(&self) -> bool {
        !self.dial_blocked.swap(true, Ordering::Relaxed)
    }

    pub fn dial_blocked(&self) -> bool {
        self.dial_blocked.load(Ordering::Relaxed)
    }

    pub fn mark_session_gone(&self) {
        self.session_gone.store(true, Ordering::Relaxed);
    }

    pub fn session_gone(&self) -> bool {
        self.session_gone.load(Ordering::Relaxed)
    }
}

/// What the agent does when the Live session ends (any reason, any error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEnd {
    /// No urgent call in progress: the usual end (goodbye, retry, voicemail, hangup).
    Usual,
    /// The owner is merged in: leave the call up, nothing else.
    LeaveUp,
    /// The phone is dialing the owner (or the caller is stuck on hold): leave the
    /// call up and alert the owner — nobody is left to talk to the caller.
    LeaveUpAndAlert,
}

pub fn on_session_end(ctl: Option<&CallCtl>) -> SessionEnd {
    match ctl {
        Some(c) if c.is_handed() => SessionEnd::LeaveUp,
        Some(c) if c.is_paused() => SessionEnd::LeaveUpAndAlert,
        _ => SessionEnd::Usual,
    }
}

type Waiting = Mutex<HashMap<String, mpsc::UnboundedSender<Report>>>;

fn waiting() -> &'static Waiting {
    static W: OnceLock<Waiting> = OnceLock::new();
    W.get_or_init(Default::default)
}

/// Results for attempt id arrive on the returned receiver until unregister.
pub fn register(id: &str) -> mpsc::UnboundedReceiver<Report> {
    let (tx, rx) = mpsc::unbounded_channel();
    if let Ok(mut g) = waiting().lock() {
        g.insert(id.to_string(), tx);
    }
    rx
}

pub fn unregister(id: &str) {
    if let Ok(mut g) = waiting().lock() {
        g.remove(id);
    }
}

/// A phone's add_call_result (media connection): true when an attempt waits for it.
pub fn deliver(msg: &Value) -> bool {
    let Some((id, report)) = parse_result(msg) else {
        return false;
    };
    let tx = waiting().lock().ok().and_then(|g| g.get(&id).cloned());
    match tx {
        Some(tx) => tx.send(report).is_ok(),
        None => false,
    }
}

/// The turn that tells the model the owner did not come on, and the direction for how to respond naturally.
pub fn not_answered_turn(lang: Lang, assistant: bool, owner: &str) -> String {
    let owner = crate::prompts::owner_label(lang, owner);
    let text = match (lang, assistant) {
        (Lang::ZhTw, false) => "（系統：剛才沒有接通。不要再叫對方等或不要掛，自然地向對方致歉、說明剛才沒接通並表示會馬上回電打給你，請用對方這通電話講的語言自然說話，不要預設中文。不要說已經接通，也不要說要轉給誰。）".to_string(),
        (Lang::ZhTw, true) => format!("（系統：{owner}沒有接電話。不要再叫對方等或不要掛，自然地告訴對方{owner}現在沒接，你已經通知他請他馬上打給你，請用對方這通電話講的語言自然說話，不要預設中文。不要說已經接通。）"),
        (Lang::ZhCn, false) => "（系统：刚才没有接通。不要再叫对方等或不要挂，自然地向对方致歉、说明刚才没接通并表示会马上回电打给你，请用对方这通电话讲的语言自然说话，不要默认中文。不要说已经接通，也不要说要转给谁。）".to_string(),
        (Lang::ZhCn, true) => format!("（系统：{owner}没有接电话。不要再叫对方等或不要挂，自然地告诉对方{owner}现在没接，你已经通知他请他马上打给你，请用对方这通电话讲的语言自然说话，不要默认中文。不要说已经接通。）"),
        (_, false) => "(System: the call did not go through. Do not ask them to hold on or stay on the line any more. Briefly explain in your own words that it didn't connect and let them know you'll call them back right away, speaking in the language the caller is speaking on this call, not necessarily English. Do not say anyone is on the line.)".to_string(),
        (_, true) => format!("(System: {owner} did not answer. Do not ask them to hold on or stay on the line any more. Briefly tell them in your own words that {owner} isn't picking up and you've asked them to call them back straight away, speaking in the language the caller is speaking on this call, not necessarily English. Do not say anyone is on the line.)"),
    };
    crate::prompts::client_turn(&text)
}

/// The turn for when no call to the owner happens after the hold-on line (the
/// phone cannot dial): the AI stops asking the caller to wait and says in its
/// own words that the owner will call back soon.
pub fn callback_turn(lang: Lang, assistant: bool, owner: &str) -> String {
    let owner = crate::prompts::owner_label(lang, owner);
    let text = match (lang, assistant) {
        (Lang::ZhTw, false) => "（系統：現在不轉接。不要再叫對方等或不要掛，自然地表示已經瞭解情況，等一下會盡快回電打給你，請用對方這通電話講的語言自然說話，不要預設中文。不要說已經接通，也不要說要轉給誰。）".to_string(),
        (Lang::ZhTw, true) => format!("（系統：現在不轉接。不要再叫對方等或不要掛，自然地告訴對方你會請{owner}盡快回電打給你，請用對方這通電話講的語言自然說話，不要預設中文。不要說已經接通。）"),
        (Lang::ZhCn, false) => "（系统：现在不转接。不要再叫对方等或不要挂，自然地表示已经了解情况，等一下会尽快回电打给你，请用对方这通电话讲的语言自然说话，不要默认中文。不要说已经接通，也不要说要转给谁。）".to_string(),
        (Lang::ZhCn, true) => format!("（系统：现在不转接。不要再叫对方等或不要挂，自然地告诉对方你会请{owner}尽快回电打给你，请用对方这通电话讲的语言自然说话，不要默认中文。不要说已经接通。）"),
        (_, false) => "(System: no call is put through now. Do not ask them to hold on or stay on the line any more. Briefly confirm you've got it and let them know in your own words that you'll call them back as soon as you can, speaking in the language the caller is speaking on this call, not necessarily English. Do not say anyone is on the line.)".to_string(),
        (_, true) => format!("(System: no call is put through now. Do not ask them to hold on or stay on the line any more. Briefly let them know in your own words that you'll ask {owner} to call them back as soon as possible, speaking in the language the caller is speaking on this call, not necessarily English. Do not say anyone is on the line.)"),
    };
    crate::prompts::client_turn(&text)
}

/// What went wrong in a way the AI cannot fix: the owner is told at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alert {
    /// The Live session ended while the phone dialed the owner.
    AiEnded,
    /// The phone could not take the caller's call off hold.
    StillOnHold,
}

/// One line for the owner's urgent alert.
pub fn alert_line(alert: Alert, lang: Lang) -> &'static str {
    match (alert, lang) {
        (Alert::AiEnded, Lang::ZhTw) => "AI 中途斷線，對方可能還在線上，請立刻回電。",
        (Alert::AiEnded, Lang::ZhCn) => "AI 中途断线，对方可能还在线上，请立刻回电。",
        (Alert::AiEnded, Lang::Ja) => "AI の通話が途中で切れました。相手はまだ待っているかもしれません。すぐに折り返してください。",
        (Alert::AiEnded, Lang::Ko) => "AI 연결이 중간에 끊겼습니다. 상대가 아직 기다릴 수 있으니 바로 전화해 주세요.",
        (Alert::AiEnded, Lang::Es) => "La IA se desconectó. Quien llama puede seguir en la línea: devuelve la llamada ya.",
        (Alert::AiEnded, Lang::Fr) => "L'IA s'est coupée. L'appelant est peut-être encore en ligne : rappelez tout de suite.",
        (Alert::AiEnded, Lang::De) => "Die KI ist ausgefallen. Der Anrufer ist vielleicht noch dran: Ruf sofort zurück.",
        (Alert::AiEnded, Lang::Pt) => "A IA caiu. Quem ligou pode ainda estar na linha: retorne a ligação já.",
        (Alert::AiEnded, Lang::En) => "The AI dropped off. The caller may still be on the line: call back now.",
        (Alert::StillOnHold, Lang::ZhTw) => "對方的電話沒能從保留恢復，他可能還在等，請立刻回電。",
        (Alert::StillOnHold, Lang::ZhCn) => "对方的电话没能从保留恢复，他可能还在等，请立刻回电。",
        (Alert::StillOnHold, Lang::Ja) => "相手の通話を保留から戻せませんでした。まだ待っているかもしれません。すぐに折り返してください。",
        (Alert::StillOnHold, Lang::Ko) => "상대 통화를 보류에서 되돌리지 못했습니다. 아직 기다릴 수 있으니 바로 전화해 주세요.",
        (Alert::StillOnHold, Lang::Es) => "No se pudo quitar la espera de la llamada. Puede que siga esperando: devuelve la llamada ya.",
        (Alert::StillOnHold, Lang::Fr) => "L'appel n'a pas pu être repris. L'appelant attend peut-être encore : rappelez tout de suite.",
        (Alert::StillOnHold, Lang::De) => "Der Anruf konnte nicht aus dem Halten geholt werden. Ruf sofort zurück.",
        (Alert::StillOnHold, Lang::Pt) => "Não deu para tirar a ligação da espera. A pessoa pode ainda estar esperando: retorne já.",
        (Alert::StillOnHold, Lang::En) => "The call could not be taken off hold. The caller may still be waiting: call back now.",
    }
}

/// Title of the owner's urgent alert (same as the urgent-matter alert).
pub fn alert_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => "來電急事通知",
        Lang::ZhCn => "来电急事通知",
        Lang::Ja => "急ぎの着信のお知らせ",
        Lang::Ko => "급한 전화 알림",
        Lang::Es => "Aviso: llamada urgente",
        Lang::Fr => "Alerte : appel urgent",
        Lang::De => "Dringender Anruf",
        Lang::Pt => "Aviso: ligação urgente",
        Lang::En => "Urgent Call Alert",
    }
}

/// Last four digits only, for logs.
pub fn masked(number: &str) -> String {
    let digits: Vec<char> = number.chars().filter(|c| c.is_ascii_digit()).collect();
    let tail: String = digits[digits.len().saturating_sub(4)..].iter().collect();
    format!("***{tail}")
}

/// Longest a phone stays marked "owner on the call" without its idle status.
pub const OWNER_ON_CALL_MAX: Duration = Duration::from_secs(3 * 60 * 60);

fn owner_calls() -> &'static Mutex<HashMap<String, Instant>> {
    static M: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    M.get_or_init(Default::default)
}

/// This phone (canonical serial) is dialing the owner or has the owner merged in: any
/// new media hello or ring on it belongs to that call (the merge adds calls on the
/// phone) and gets no AI, no speech and no hangup until the phone is idle again.
pub fn mark_owner_on_call(phone: &str) {
    if let Ok(mut g) = owner_calls().lock() {
        g.insert(phone.to_string(), Instant::now());
    }
}

/// The phone reported no call (idle), or the AI came back to the caller.
pub fn clear_owner_on_call(phone: &str) -> bool {
    owner_calls().lock().ok().is_some_and(|mut g| g.remove(phone).is_some())
}

pub fn owner_on_call(phone: &str) -> bool {
    owner_on_call_at(phone, Instant::now())
}

fn owner_on_call_at(phone: &str, now: Instant) -> bool {
    owner_calls()
        .lock()
        .ok()
        .and_then(|g| g.get(phone).copied())
        .is_some_and(|t| now.duration_since(t) < OWNER_ON_CALL_MAX)
}

/// A phone's control `status`: `in_call` false clears the mark.
pub fn status_ends_owner_call(status: &Value) -> bool {
    status.get("in_call").and_then(Value::as_bool) == Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(mode: &str, number: &str) -> RuntimeSettings {
        let mut rt = RuntimeSettings::from_base(&crate::config::Settings::from_env());
        rt.whitelist_mode = mode.to_string();
        rt.whitelist_forward_number = number.to_string();
        rt
    }

    #[test]
    fn only_emergency_mode_with_a_number_dials() {
        assert_eq!(number_to_call(&rt("emergency", " +15550001111 ")).as_deref(), Some("+15550001111"));
        assert_eq!(number_to_call(&rt("emergency", "   ")), None);
        assert_eq!(number_to_call(&rt("emergency", "")), None);
        assert_eq!(number_to_call(&rt("direct", "+15550001111")), None);
        assert_eq!(number_to_call(&rt("off", "+15550001111")), None);
    }

    #[test]
    fn the_old_urgent_call_setting_changes_nothing() {
        for old in [false, true] {
            let mut with_number = rt("emergency", "+15550001111");
            with_number.whitelist_urgent_call = old;
            assert_eq!(number_to_call(&with_number).as_deref(), Some("+15550001111"), "old setting {old}");
            let mut no_number = rt("emergency", "");
            no_number.whitelist_urgent_call = old;
            assert_eq!(number_to_call(&no_number), None, "old setting {old}");
        }
    }

    #[test]
    fn the_command_carries_number_id_and_merge() {
        let v = add_call_payload("u-1", "+15550001111");
        assert_eq!(v["type"], "add_call");
        assert_eq!(v["id"], "u-1");
        assert_eq!(v["number"], "+15550001111");
        assert_eq!(v["merge"], true);
        assert_eq!(v["answer_timeout_ms"], 25_000);
    }

    #[test]
    fn results_parse_and_reasons_stay_short_codes() {
        let r = |s: Value| parse_result(&s);
        assert_eq!(r(json!({"type":"add_call_result","id":"u-1","status":"dialing"})), Some(("u-1".into(), Report::Dialing)));
        assert_eq!(r(json!({"type":"add_call_result","id":"u-1","status":"merged"})), Some(("u-1".into(), Report::Merged)));
        assert_eq!(
            r(json!({"type":"add_call_result","id":"u-1","status":"failed","reason":"no answer; +1555"})),
            Some(("u-1".into(), Report::Failed("noanswer1555".into())))
        );
        assert_eq!(
            r(json!({"type":"add_call_result","id":"u-1","status":"failed"})),
            Some(("u-1".into(), Report::Failed("unknown".into())))
        );
        assert_eq!(r(json!({"type":"add_call_result","id":"","status":"merged"})), None);
        assert_eq!(r(json!({"type":"add_call_result","id":"u-1","status":"maybe"})), None);
        assert_eq!(r(json!({"type":"send_sms_result","id":"u-1","status":"merged"})), None);
    }

    #[test]
    fn merged_settles_once() {
        let mut t = Tracker::default();
        assert_eq!(t.on_report(Report::Dialing), Step::Wait);
        assert_eq!(t.on_report(Report::Active), Step::Wait);
        assert_eq!(t.on_report(Report::Merged), Step::Merged);
        assert!(t.settled());
        assert_eq!(t.on_report(Report::Failed("leg_ended".into())), Step::Wait);
        assert_eq!(t.on_timeout(), Step::Wait);
    }

    #[test]
    fn each_failure_comes_back_with_its_reason() {
        for reason in ["no_answer", "dial_failed", "merge_failed", "leg_ended", "cannot_hold"] {
            let mut t = Tracker::default();
            t.on_report(Report::Dialing);
            assert_eq!(t.on_report(Report::Failed(reason.into())), Step::Failed(reason.into()));
            assert_eq!(t.on_report(Report::Merged), Step::Wait, "a late merge after a failure is ignored");
        }
    }

    #[test]
    fn no_result_in_time_fails_by_how_far_it_got() {
        assert_eq!(Tracker::default().on_timeout(), Step::Failed("phone_no_reply".into()));
        let mut t = Tracker::default();
        t.on_report(Report::Dialing);
        assert_eq!(t.on_timeout(), Step::Failed("no_result".into()));
        let mut t = Tracker::default();
        t.on_report(Report::Active);
        assert_eq!(t.on_timeout(), Step::Failed("merge_timeout".into()));
    }

    #[test]
    fn results_reach_only_a_waiting_attempt() {
        let mut rx = register("u-test-1");
        assert!(deliver(&json!({"type":"add_call_result","id":"u-test-1","status":"active"})));
        assert_eq!(rx.try_recv().ok(), Some(Report::Active));
        assert!(!deliver(&json!({"type":"add_call_result","id":"u-other","status":"active"})));
        unregister("u-test-1");
        assert!(!deliver(&json!({"type":"add_call_result","id":"u-test-1","status":"merged"})));
    }

    #[test]
    fn the_ctl_pauses_hands_over_and_says_one_line() {
        let c = CallCtl::default();
        assert!(!c.blocks_hangup());
        c.pause();
        assert!(c.ai_quiet() && c.blocks_hangup());
        c.resume_with("x".into());
        assert!(!c.ai_quiet());
        assert_eq!(c.take_say().as_deref(), Some("x"));
        assert_eq!(c.take_say(), None);
        c.hand_over();
        assert!(c.is_handed() && c.blocks_hangup());
    }

    #[test]
    fn the_not_answered_line_never_says_the_call_is_through() {
        for lang in [Lang::ZhTw, Lang::ZhCn, Lang::En, Lang::Ja, Lang::Es] {
            for assistant in [false, true] {
                let t = not_answered_turn(lang, assistant, "Sam");
                let v: Value = serde_json::from_str(&t).unwrap();
                let text = v.pointer("/clientContent/turns/0/parts/0/text").and_then(|x| x.as_str()).unwrap();
                assert!(text.contains("打給你") || text.contains("打给你") || text.contains("call them back"), "{text}");
            }
        }
    }

    #[test]
    fn logs_show_only_the_last_four_digits() {
        assert_eq!(masked("+1 (555) 000-1111"), "***1111");
        assert_eq!(masked("12"), "***12");
    }

    // ---- reviewer additions ----

    #[test]
    fn reviewer_default_settings_dial_once_a_number_is_set() {
        let mut rt = RuntimeSettings::from_base(&crate::config::Settings::from_env());
        rt.whitelist_mode = "emergency".to_string();
        assert_eq!(number_to_call(&rt), None);
        rt.whitelist_forward_number = "+15550001111".to_string();
        assert_eq!(number_to_call(&rt).as_deref(), Some("+15550001111"));
    }

    #[test]
    fn reviewer_only_one_settlement_whatever_the_order() {
        let mut t = Tracker::default();
        let steps: Vec<Step> = [Report::Merged, Report::Dialing, Report::Active, Report::Merged, Report::Failed("x".into())]
            .into_iter()
            .map(|r| t.on_report(r))
            .collect();
        assert_eq!(steps, vec![Step::Merged, Step::Wait, Step::Wait, Step::Wait, Step::Wait]);
        assert_eq!(t.on_timeout(), Step::Wait);

        let mut t = Tracker::default();
        let steps: Vec<Step> = [
            Report::Failed("no_answer".into()),
            Report::Failed("no_answer".into()),
            Report::Merged,
            Report::Active,
        ]
        .into_iter()
        .map(|r| t.on_report(r))
        .collect();
        assert_eq!(steps, vec![Step::Failed("no_answer".into()), Step::Wait, Step::Wait, Step::Wait]);
        assert_eq!(t.on_timeout(), Step::Wait);

        // Repeated progress reports never settle; the timeout names how far it got.
        let mut t = Tracker::default();
        for _ in 0..3 {
            assert_eq!(t.on_report(Report::Active), Step::Wait);
            assert_eq!(t.on_report(Report::Dialing), Step::Wait);
        }
        assert!(!t.settled());
        assert_eq!(t.on_timeout(), Step::Failed("merge_timeout".into()));
        assert_eq!(t.on_timeout(), Step::Wait);
    }

    #[test]
    fn reviewer_repeated_and_late_results_over_the_channel_settle_once() {
        let id = "u-rev-chan-1";
        let mut rx = register(id);
        for status in ["dialing", "active", "merged", "merged", "failed", "active"] {
            // The phone pads the id with spaces: still this attempt.
            assert!(deliver(&json!({"type":"add_call_result","id":format!(" {id} "),"status":status,"reason":"merge_failed"})));
        }
        let mut t = Tracker::default();
        let mut settled = Vec::new();
        while let Ok(report) = rx.try_recv() {
            let step = t.on_report(report);
            if step != Step::Wait {
                settled.push(step);
            }
        }
        assert_eq!(settled, vec![Step::Merged], "exactly one settlement, the first final result");
        drop(rx);
        assert!(!deliver(&json!({"type":"add_call_result","id":id,"status":"merged"})), "a dropped waiter takes nothing");
        unregister(id);
    }

    #[test]
    fn reviewer_a_late_merge_after_the_ai_came_back_stays_quiet_for_the_ai() {
        // Handed after a resume still keeps the AI out and the hangup blocked.
        let c = CallCtl::default();
        c.pause();
        c.hand_over();
        c.resume_with("late".into());
        assert!(c.ai_quiet() && c.blocks_hangup());
        // A second resume overwrites, and the line is taken once.
        let c = CallCtl::default();
        c.pause();
        c.resume_with("one".into());
        c.resume_with("two".into());
        assert_eq!(c.take_say().as_deref(), Some("two"));
        assert_eq!(c.take_say(), None);
        assert!(!c.blocks_hangup());
    }

    #[test]
    fn reviewer_handing_over_never_ends_in_a_goodbye_hangup() {
        use crate::gemini_live::SessionEndReason as R;
        assert!(!R::Handed.should_play_goodbye());
        assert_eq!(R::Handed.as_str(), "handed_to_owner");
        // The reasons that do hang up are the ones the session-end code must hold back
        // while CallCtl::blocks_hangup is true.
        for r in [R::Goodbye, R::MaxDuration, R::CantCommunicate, R::WsClosed, R::IvrHangup] {
            assert!(r.should_play_goodbye(), "{}", r.as_str());
        }
    }

    #[test]
    fn reviewer_the_agent_waits_longer_than_the_phone_needs() {
        // Phone: owner answer timeout, 0.6 s before the merge, up to 10 s to merge, then
        // up to about 5 s to take the original call off hold before it reports failed.
        assert!(RESULT_WAIT >= OWNER_ANSWER_TIMEOUT + Duration::from_secs(20));
        // The phone coerces answer_timeout_ms into 5 s..60 s.
        assert!(OWNER_ANSWER_TIMEOUT >= Duration::from_secs(5) && OWNER_ANSWER_TIMEOUT <= Duration::from_secs(60));
        assert!(HOLD_LINE_MAX_WAIT < RESULT_WAIT);
    }

    #[test]
    fn reviewer_logs_never_carry_more_than_four_digits() {
        for n in ["+1 (555) 000-1111", "0912-345-678", "+886 912 345 678", "tel:+15550001111;ext=22", "", "abc"] {
            let m = masked(n);
            assert!(m.starts_with("***"), "{m}");
            assert!(m.chars().filter(|c| c.is_ascii_digit()).count() <= 4, "{m}");
        }
        assert_eq!(masked(""), "***");
    }

    #[test]
    fn reviewer_the_not_answered_line_works_for_every_language_and_a_blank_owner() {
        for lang in crate::locale::ALL {
            for assistant in [false, true] {
                for owner in ["Sam", "", "  ", "{x}\nSam"] {
                    let t = not_answered_turn(lang, assistant, owner);
                    let v: Value = serde_json::from_str(&t).unwrap();
                    let text = v.pointer("/clientContent/turns/0/parts/0/text").and_then(|x| x.as_str()).unwrap();
                    assert!(!text.is_empty());
                    assert!(!text.contains("{x}") && !text.contains('{') && !text.contains('}'), "{lang:?} {assistant} {owner:?}: {text}");
                    assert!(!text.contains('「') && !text.contains('」') && !text.contains('"'), "{lang:?} {assistant} must not contain quoted scripts: {text}");
                    assert!(text.contains("打給你") || text.contains("打给你") || text.contains("call them back"), "{text}");
                }
            }
        }
    }

    #[test]
    fn emergency_short_premium_and_mmi_numbers_are_never_dialed() {
        for n in ["110", "911", "112", "119", "*67#", "**21*+15550001111#", "#31#+15550001111", "123", "+19005550123", "011886912345678", "abc"] {
            assert!(check_owner_number(n, Some("us"), "").is_err(), "{n}");
            assert!(check_owner_number(n, None, "").is_err(), "{n}");
            assert!(check_owner_number(n, None, "1").is_err(), "{n}");
        }
        assert_eq!(check_owner_number("+1 555 000 1111", Some("us"), "").as_deref(), Ok("+15550001111"));
        assert_eq!(check_owner_number("+15550001111", None, "").as_deref(), Ok("+15550001111"));
        // Unknown SIM and no country of the owner's: a number without its country
        // code is not guessed at.
        assert!(check_owner_number("5550001111", None, "").is_err());
    }

    #[test]
    fn sim_unknown_a_national_number_reads_as_the_owners_country() {
        // Taiwan (the owner's country, effective_country_code 886).
        assert_eq!(check_owner_number("0912-345-678", None, "886").as_deref(), Ok("+886912345678"));
        // The US: ten digits.
        assert_eq!(check_owner_number("(555) 000-1111", None, "1").as_deref(), Ok("+15550001111"));
        // A full +number still reads as its own country's.
        assert_eq!(check_owner_number("+886912345678", None, "1").as_deref(), Ok("+886912345678"));
        // Full +international owner numbers validate against their own country
        // even when the incoming line carries a SIM from another country.
        assert_eq!(check_owner_number("+1 555 000 1111", Some("tw"), "886").as_deref(), Ok("+15550001111"));
        assert_eq!(check_owner_number("+81 90 1234 5678", Some("tw"), "886").as_deref(), Ok("+819012345678"));
        assert_eq!(check_owner_number("+44 7700 900123", Some("tw"), "886").as_deref(), Ok("+447700900123"));
        assert_eq!(check_owner_number("+886 912 345 678", Some("us"), "1").as_deref(), Ok("+886912345678"));
        assert!(check_owner_number("+19005550123", Some("tw"), "886").is_err());
        assert!(check_owner_number("+81110", Some("tw"), "886").is_err());
        // Emergency and short numbers stay refused.
        assert!(check_owner_number("119", None, "886").is_err());
        assert!(check_owner_number("0912345678", None, "abc").is_err());
    }

    #[test]
    fn the_hold_on_line_is_found_in_the_ai_output() {
        for said in [
            "好，你等一下，先不要掛。",
            "好，你等一下，先不要卦。",
            "好，我现在帮你打给他，请先不要挂。",
            "Okay, hold on a moment, don't hang up.",
            "Please stay on the line.",
            "No cuelgues, por favor.",
            "え、本当? 大変だね。わかった。ちょっとそのまま待ってて。",
        ] {
            assert!(is_hold_line(said) && is_strong_hold_line(said), "{said}");
        }
        for said in ["好，我知道了，我馬上處理，等一下打給你。", "I'll call you right back.", "掛號了嗎？"] {
            assert!(!is_hold_line(said), "{said}");
        }
    }

    #[test]
    fn the_hold_on_line_is_noted_once_and_dialing_is_blocked_once() {
        let c = CallCtl::default();
        assert!(c.hold_said_at().is_none());
        c.note_hold_line_from(1280);
        let (said, base) = c.hold_said().unwrap();
        assert_eq!(base, 1280);
        c.note_hold_line_from(4096);
        assert_eq!(c.hold_said(), Some((said, 1280)), "the first time counts");
        assert_eq!(c.hold_said_at(), Some(said));
        assert!(c.block_dial());
        assert!(!c.block_dial(), "blocked once");
        assert!(c.dial_blocked());
    }

    #[test]
    fn the_callback_line_promises_a_call_back_and_never_a_connection() {
        for lang in crate::locale::ALL {
            for assistant in [false, true] {
                for t in [callback_turn(lang, assistant, "Sam"), not_answered_turn(lang, assistant, "Sam")] {
                    let v: Value = serde_json::from_str(&t).unwrap();
                    let text = v.pointer("/clientContent/turns/0/parts/0/text").and_then(|x| x.as_str()).unwrap();
                    assert!(!text.contains('「') && !text.contains('」') && !text.contains('"'), "{lang:?} {assistant} must not contain quoted scripts: {text}");
                    assert!(text.contains("打給你") || text.contains("打给你") || text.contains("call them back"), "{text}");
                    assert!(
                        text.contains("不要再叫對方等")
                            || text.contains("不要再叫对方等")
                            || text.contains("Do not ask them to hold on or stay on the line any more"),
                        "{text}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_abort_names_the_attempt_and_alerts_exist_in_every_language() {
        let v = abort_payload("u-9");
        assert_eq!(v["type"], "add_call_abort");
        assert_eq!(v["id"], "u-9");
        for lang in crate::locale::ALL {
            assert!(!alert_line(Alert::AiEnded, lang).is_empty());
            assert!(!alert_line(Alert::StillOnHold, lang).is_empty());
        }
    }

    #[test]
    fn the_phone_gets_a_dialing_ack_window_shorter_than_the_hold_line_wait() {
        assert!(DIAL_ACK_WAIT < OWNER_ANSWER_TIMEOUT);
        assert!(ABORT_WAIT < RESULT_WAIT);
    }

    #[test]
    fn a_session_that_ends_while_dialing_or_merged_never_reaches_the_hangup_path() {
        assert_eq!(on_session_end(None), SessionEnd::Usual);
        let c = CallCtl::default();
        assert_eq!(on_session_end(Some(&c)), SessionEnd::Usual);
        c.pause();
        assert_eq!(on_session_end(Some(&c)), SessionEnd::LeaveUpAndAlert, "dialing: no voicemail, retry or hangup");
        c.resume_with("x".into());
        assert_eq!(on_session_end(Some(&c)), SessionEnd::Usual, "the AI is back: the usual end");
        c.pause();
        c.hand_over();
        assert_eq!(on_session_end(Some(&c)), SessionEnd::LeaveUp, "merged: leave it up");
    }

    #[test]
    fn a_phone_with_the_owner_on_the_call_takes_no_new_ai_session_until_idle() {
        let phone = "test-phone-owner-on";
        assert!(!owner_on_call(phone));
        mark_owner_on_call(phone);
        assert!(owner_on_call(phone), "new hellos / rings on this phone are ignored");
        assert!(!owner_on_call("test-phone-other"), "other phones are not affected");
        assert!(!owner_on_call_at(phone, Instant::now() + OWNER_ON_CALL_MAX), "a lost idle status does not block forever");
        assert!(!status_ends_owner_call(&json!({"type":"status","in_call":true})));
        assert!(!status_ends_owner_call(&json!({"type":"status"})));
        assert!(status_ends_owner_call(&json!({"type":"status","in_call":false})));
        assert!(clear_owner_on_call(phone));
        assert!(!owner_on_call(phone));
        assert!(!clear_owner_on_call(phone));
    }

    // ---- Nine-language table: urgent turns and hold-on lines ----

    /// (language, three real emergencies, three mentions that are not urgent)
    const URGENT_TABLE: &[(&str, [&str; 3], [&str; 3])] = &[
        ("zh-TW", ["我是他姐姐，我爸爸剛被送急診，請馬上找他。", "有急事，麻煩你叫他現在馬上接電話。", "他出車禍了，很急，我要馬上找本人。"],
                  ["明天想約你吃飯，有空再回我。", "最近天氣變涼，記得多穿點。", "我只是打來問候一下，不用特地找他。"]),
        ("zh-CN", ["我是他表哥，我妈妈刚被送去急诊，请马上找他。", "有紧急的事，请让他马上接电话。", "他出车祸了，很急，我要马上找本人。"],
                  ["明天想约你吃饭，有空再回我。", "最近天气变凉，记得多穿点。", "我只是打来问候一下，不用特地找他。"]),
        ("en", ["This is his sister, our dad was just taken to the hospital, I need to reach him right now.", "It's an emergency, please get him on the phone.", "There's been an accident and I need him immediately."],
               ["Want to grab lunch tomorrow?", "Just wanted to say hi, no need to find him.", "Tell him the book he lent me was great."]),
        ("ja", ["兄の友人です。母が事故に遭いました、至急本人につないでください。", "急ぎの用事なんです、すぐ繋いでください。", "父が病院に運ばれました、本人と話したいです。"],
               ["明日ご飯でもどうですか。", "ご無沙汰しています、挨拶だけです。", "お借りした本、とても面白かったと伝えてください。"]),
        ("ko", ["그의 누나인데요, 아버지가 응급실에 실려 갔어요. 지금 바로 연결해 주세요.", "긴급한 일이에요, 본인과 통화해야 해요.", "급한 일이 생겼어요, 빨리 바꿔 주세요."],
               ["내일 밥 한번 먹어요.", "그냥 인사하려고 전화했어요.", "빌려준 책 재미있었다고 전해 주세요."]),
        ("es", ["Soy su hermana, mi padre acaba de ingresar al hospital, necesito hablar con él.", "Es una emergencia, por favor comuníquelo ahora mismo.", "Hubo un accidente y es urgente que hable con él."],
               ["¿Quieres comer mañana?", "Solo llamo para saludar.", "Dile que el libro que me prestó me encantó."]),
        ("fr", ["C'est sa sœur, notre père vient d'être emmené à l'hôpital, je dois lui parler.", "C'est urgent, passez-le-moi tout de suite.", "Il y a eu une urgence, j'ai besoin de lui immédiatement."],
               ["On déjeune demain ?", "J'appelais juste pour dire bonjour.", "Dis-lui que son livre m'a beaucoup plu."]),
        ("de", ["Ich bin seine Schwester, unser Vater ist im Krankenhaus, ich muss ihn sofort sprechen.", "Es ist ein Notfall, bitte verbinden Sie mich.", "Es gab einen Unfall, es ist dringend."],
               ["Hast du morgen Zeit zum Mittagessen?", "Ich rufe nur an, um Hallo zu sagen.", "Sag ihm, das Buch war richtig gut."]),
        ("pt", ["Sou irmã dele, meu pai foi levado para o hospital, preciso falar com ele agora mesmo.", "É uma emergência, por favor chame-o.", "Houve um acidente, é urgente."],
               ["Quer almoçar amanhã?", "Só liguei para dizer oi.", "Diga a ele que o livro emprestado era ótimo."]),
    ];

    /// Mentions of hospital / accident / "right now" with nothing urgent behind them.
    /// `is_urgent_whitelist_turn` matches the bare word, so these come out true.
    const MENTION_ONLY: &[(&str, &str)] = &[
        ("zh-TW", "我上週去醫院拿藥，沒事了。"),
        ("zh-TW", "好意外你還記得我，哈哈。"),
        ("zh-CN", "我上周去医院拿药，没事了。"),
        ("zh-CN", "好意外你还记得我，哈哈。"),
        ("en", "I went to the hospital last week to pick up my meds, nothing serious."),
        ("en", "Right now I'm just calling to say hi."),
        ("ja", "先週、病院で薬をもらいましたが、大丈夫です。"),
        ("ja", "最近は仕事が大変ですね。"),
        ("ko", "지난주에 병원에서 약을 받았는데 괜찮아요."),
        ("ko", "시간이 빨리 가네요, 벌써 가을이에요."),
        ("es", "La semana pasada fui al hospital por mis medicinas, nada grave."),
        ("fr", "La semaine dernière je suis allé à l'hôpital chercher mes médicaments, rien de grave."),
        ("de", "Letzte Woche war ich im Krankenhaus wegen meiner Tabletten, nichts Schlimmes."),
        ("pt", "Semana passada fui ao hospital buscar remédio, nada grave."),
    ];

    #[test]
    fn urgent_turns_are_found_in_every_language() {
        for (lang, urgent, _) in URGENT_TABLE {
            for s in urgent {
                assert!(crate::gemini_live::is_urgent_whitelist_turn(s), "{lang}: {s}");
            }
        }
    }

    #[test]
    fn friendly_turns_without_an_emergency_are_not_urgent_in_every_language() {
        for (lang, _, calm) in URGENT_TABLE {
            for s in calm {
                assert!(!crate::gemini_live::is_urgent_whitelist_turn(s), "{lang}: {s}");
            }
        }
    }

    #[test]
    fn known_gap_mentioning_a_hospital_without_urgency_is_not_urgent() {
        for (lang, s) in MENTION_ONLY {
            assert!(!crate::gemini_live::is_urgent_whitelist_turn(s), "{lang}: {s}");
        }
    }

    /// (language, the line of the owner's own voice, the line of the AI assistant)
    const HOLD_TABLE: &[(&str, &str, &str)] = &[
        ("zh-TW", "好，你等一下，先不要掛。", "我現在幫你打給他，請先不要掛。"),
        ("zh-CN", "好，你等一下，先不要挂。", "我现在帮你打给他，请先不要挂。"),
        ("en", "Okay, hold on a sec, don't hang up.", "I'm calling him for you now, please stay on the line."),
        ("ja", "わかった、ちょっと待って、切らないでね。", "今おつなぎします、電話を切らないでください。"),
        ("ko", "알겠어, 잠깐만, 끊지 말고 기다려.", "지금 연결해 드릴게요, 끊지 마세요."),
        ("es", "Vale, espera un momento, no cuelgues.", "Ahora mismo le llamo, por favor no cuelgue."),
        ("fr", "D'accord, une seconde, ne raccroche pas.", "Je l'appelle tout de suite, ne raccrochez pas, s'il vous plaît."),
        ("de", "Okay, einen Moment, leg nicht auf.", "Ich rufe ihn jetzt an, bitte legen Sie nicht auf."),
        ("pt", "Tá bom, espera um pouco, não desligue.", "Vou ligar para ele agora, por favor não desligue."),
    ];

    /// Ordinary lines of the AI that carry no hold-on request.
    const NOT_HOLD: &[(&str, &str)] = &[
        ("zh-TW", "好的，我知道了，請問是什麼事？"),
        ("zh-CN", "好的，我知道了，请问是什么事？"),
        ("en", "Sure, what is it about?"),
        ("ja", "はい、どうなさいましたか。"),
        ("ko", "네, 무슨 일이세요?"),
        ("es", "Claro, ¿de qué se trata?"),
        ("fr", "Bien sûr, de quoi s'agit-il ?"),
        ("de", "Natürlich, worum geht es?"),
        ("pt", "Claro, do que se trata?"),
    ];

    /// Other natural ways to ask the caller to stay on the line.
    const HOLD_VARIANTS: &[(&str, &str)] = &[
        ("en", "Please hold, I'm connecting you."),
        ("ja", "このまま切らずにお待ちください。"),
        ("fr", "Ne quittez pas, je vous le passe."),
        ("de", "Bleiben Sie bitte dran, ich verbinde."),
        ("pt", "Não desliga, já falo com ele."),
        ("es", "Quédese en la línea, por favor."),
        ("zh-TW", "請你先別走，我馬上處理。"),
    ];

    #[test]
    fn the_hold_on_line_is_found_in_every_language_for_both_voices() {
        for (lang, own, assistant) in HOLD_TABLE {
            assert!(is_hold_line(own), "{lang} own voice: {own}");
            assert!(is_hold_line(assistant), "{lang} assistant: {assistant}");
        }
    }

    #[test]
    fn ordinary_lines_are_not_a_hold_on_line_in_every_language() {
        for (lang, s) in NOT_HOLD {
            assert!(!is_hold_line(s), "{lang}: {s}");
        }
    }

    #[test]
    fn known_gap_other_hold_on_wordings_are_found() {
        for (lang, s) in HOLD_VARIANTS {
            assert!(is_hold_line(s), "{lang}: {s}");
        }
    }

    #[test]
    fn a_hold_on_line_reads_the_same_without_spaces_or_accents() {
        for said in ["Attends un instant. Neraccroche pas.", "NE RACCROCHEZ PAS", "Nao desligue", "quedese en la linea", "bleibensiedran", "先 不 要 掛"] {
            assert!(is_hold_line(said), "{said}");
        }
        for said in ["Hold on, what do you mean?", "ちょっと待って、何の話？"] {
            assert!(is_hold_line(said) && !is_strong_hold_line(said), "weak wording, counted only after urgent words: {said}");
        }
        for said in ["Sure, what is it about?", "我知道了，等一下打給你。", "Bien sûr, de quoi s'agit-il ?", "掛號了嗎？"] {
            assert!(!is_hold_line(said), "{said}");
        }
    }

    #[test]
    fn the_hold_on_line_is_found_in_vietnamese_indonesian_thai_filipino_and_cantonese() {
        for said in [
            "Anh đừng cúp máy nhé, tôi liên lạc với ông ấy ngay.",
            "Anh \u{111}u\u{31b}\u{300}ng cu\u{301}p ma\u{301}y nhe\u{301}, toi lien lac ngay.",
            "dung cup may nhe, toi lien lac ngay",
            "Tolong jangan tutup telepon, saya segera hubungi beliau.",
            "Jangan matikan telepon ya, sebentar saya hubungi.",
            "อย่าเพิ่งวางสายนะคะ เดี๋ยวติดต่อเจ้าของเบอร์ให้ทันที",
            "Huwag po ninyong ibaba ang telepono, kokontakin ko na po siya.",
            "Wag mo munang ibaba, tatawagan ko na siya.",
            "唔好收線呀，我即刻聯絡佢。",
            "唔好掛電話，我而家搵佢。",
        ] {
            assert!(is_hold_line(said) && is_strong_hold_line(said), "{said}");
        }
        for said in [
            "Xin chào, tôi có thể giúp gì cho anh?",
            "Halo, ada yang bisa saya bantu?",
            "สวัสดีค่ะ มีอะไรให้ช่วยไหมคะ",
            "Magandang araw po, ano po ang maitutulong ko?",
            "你好，請問搵邊位呀？",
        ] {
            assert!(!is_hold_line(said), "{said}");
        }
    }

    #[test]
    fn hold_on_lines_cover_extended_official_languages_without_overmatching() {
        for said in [
            "Ne coupez pas, je vous le passe tout de suite.",
            "そのままでいてください、今つなぎます。",
            "Non riattaccare, lo chiamo subito.",
            "Resti in linea per favore, glielo passo.",
            "Не кладите трубку, сейчас соединю.",
            "Оставайтесь на линии, пожалуйста.",
            "फोन मत काटिए, मैं अभी बात कराता हूँ।",
            "لا تقفل الخط، سأتصل به الآن.",
            "Blijf aan de lijn, ik verbind u door.",
            "Proszę się nie rozłączać, już łączę.",
            "Nu închideți, rămâneți pe fir vă rog.",
            "Не кладіть слухавку, залишайтесь на лінії.",
            "ফোন কাটবেন না, লাইনে থাকুন।",
            "फोन ठेवू नका, लाइनवर थांबा.",
            "போனை வைக்காதீர்கள், லைனில் இருங்கள்.",
            "ఫోన్ కట్ చేయకండి, లైన్లో ఉండండి.",
        ] {
            assert!(is_hold_line(said) && is_strong_hold_line(said), "{said}");
        }
        assert!(!is_hold_line("そのままで大丈夫です。"), "bare そのままで must not count");
    }

    #[test]
    fn the_hold_on_line_is_found_in_turkish() {
        for said in [
            "Lütfen telefonu kapatmayın, hemen onu arıyorum.",
            "Kapatmayın, sizi hemen bağlıyorum.",
            "Hatta kalın lütfen, bir saniye.",
            "Hattan ayrılmayın, birazdan döneceğim.",
            "Ayrılmayın, hemen kontrol ediyorum.",
        ] {
            assert!(is_hold_line(said) && is_strong_hold_line(said), "strong Turkish wording: {said}");
        }
        for said in ["Bir dakika, hemen bakıyorum.", "Bir saniye lütfen.", "Lütfen bekleyin."] {
            assert!(is_hold_line(said) && !is_strong_hold_line(said), "weak Turkish wording: {said}");
        }
        for said in ["Tabii, ne hakkında konuşuyoruz?", "Merhaba, size nasıl yardımcı olabilirim?"] {
            assert!(!is_hold_line(said), "{said}");
        }
    }

    #[test]
    fn the_hold_on_line_is_found_in_italian() {
        for said in [
            "Non riattacchi, la passo subito.",
            "Non attacchi, lo chiamo adesso.",
            "Resti in attesa, per favore.",
            "Non metta giù, glielo passo.",
            "Non si allontani, torno subito.",
        ] {
            assert!(is_hold_line(said) && is_strong_hold_line(said), "strong Italian wording: {said}");
        }
        for said in ["Aspetti un attimo, controllo subito.", "Un secondo, guardo qui.", "Solo un momento, la prego."] {
            assert!(is_hold_line(said) && !is_strong_hold_line(said), "weak Italian wording: {said}");
        }
        for said in ["Certo, di cosa stiamo parlando?", "Buongiorno, come posso aiutarla?"] {
            assert!(!is_hold_line(said), "{said}");
        }
        // Other languages as before.
        assert!(is_hold_line("Please hold, I'll put you through."));
        assert!(is_hold_line("No cuelgue, ya le paso."));
        assert!(!is_hold_line("Sure, what is this about?"));
    }

    #[test]
    fn a_line_said_twice_is_found() {
        assert!(repeats_a_line("Attends un instant. Ne raccroche pas. Attends un instant. Ne raccroche pas."));
        assert!(repeats_a_line("Attends un instant. Neraccroche pas. Attends un instant, ne raccroche pas."));
        assert!(!repeats_a_line("Okay. Okay. 喂？喂？"), "short lines do not count");
        assert!(!repeats_a_line("Je comprends. Je préviens ton frère tout de suite."));
    }

    #[test]
    fn every_line_for_the_ai_asks_for_the_callers_language() {
        for lang in crate::locale::ALL {
            for assistant in [false, true] {
                for t in [not_answered_turn(lang, assistant, "Sam"), callback_turn(lang, assistant, "Sam")] {
                    let v: Value = serde_json::from_str(&t).unwrap();
                    let text = v.pointer("/clientContent/turns/0/parts/0/text").and_then(|x| x.as_str()).unwrap();
                    assert!(
                        text.contains("對方這通電話講的語言") || text.contains("对方这通电话讲的语言") || text.contains("language the caller is speaking"),
                        "{lang:?} {assistant}: {text}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_callers_spoken_language_goes_into_the_line_whatever_the_setting() {
        // Owner's setting is Chinese, the caller speaks German: the AI is told German.
        let said = "Es gab einen Unfall, es ist dringend.";
        let spoken = crate::urgency::caller_language(said);
        assert_eq!(spoken, Some(Lang::De));
        let t = with_caller_language(&callback_turn(Lang::ZhTw, false, "Sam"), spoken);
        let v: Value = serde_json::from_str(&t).unwrap();
        let text = v.pointer("/clientContent/turns/0/parts/0/text").and_then(|x| x.as_str()).unwrap();
        assert!(text.contains("Say it in German"), "{text}");
        assert!(text.contains("打給你"), "the instruction is kept: {text}");
        for (said, name) in [("This is his sister, it's an emergency.", "English"), ("C'est urgent, passez-le-moi.", "French"), ("Es una emergencia.", "Spanish")] {
            let t = with_caller_language(&not_answered_turn(Lang::ZhCn, true, "Sam"), crate::urgency::caller_language(said));
            assert!(t.contains(&format!("Say it in {name}")), "{said}: {t}");
        }
        let t = with_caller_language(&callback_turn(Lang::En, true, "Sam"), None);
        assert!(t.contains("language the caller is speaking on this call"), "{t}");
        assert_eq!(with_caller_language("not json", Some(Lang::En)), "not json");
    }

    fn who_rt(mode: &str, wl: bool, contacts: bool, others: bool) -> RuntimeSettings {
        let mut cfg = RuntimeSettings::from_base(&crate::config::Settings::from_env());
        cfg.whitelist_mode = mode.to_string();
        cfg.transfer_whitelist = wl;
        cfg.transfer_contacts = contacts;
        cfg.transfer_others = others;
        cfg
    }

    #[test]
    fn who_may_be_forwarded_follows_the_three_switches() {
        for mode in ["direct", "emergency"] {
            for bits in 0..8u8 {
                let (wl, ct, ot) = (bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
                let cfg = who_rt(mode, wl, ct, ot);
                let case = format!("{mode} wl={wl} contacts={ct} others={ot}");
                assert_eq!(transfer_who(&cfg, Some("whitelist"), false), wl.then_some(TransferWho::Whitelist), "{case}");
                assert_eq!(transfer_who(&cfg, Some(""), false), ct.then_some(TransferWho::Contact), "{case}");
                assert_eq!(transfer_who(&cfg, Some("blacklist"), false), None, "{case}");
                assert_eq!(transfer_who(&cfg, Some("stall"), false), None, "{case}");
                assert_eq!(transfer_who(&cfg, None, false), ot.then_some(TransferWho::Stranger), "{case}");
                // Hidden numbers are never in the contacts: the third switch.
                for hidden in ["", "unknown", "private", "  "] {
                    let t = transfer_target(&cfg, hidden, None);
                    assert_eq!(t.as_ref().map(|t| t.who), ot.then_some(TransferWho::Stranger), "{case} {hidden:?}");
                    assert!(t.is_none_or(|t| t.name.is_empty()));
                }
                assert_eq!(
                    transfer_target(&cfg, "+15555550123", None).map(|t| t.who),
                    ot.then_some(TransferWho::Stranger),
                    "{case}"
                );
                // A blocked number is never forwarded, unless it is on the Whitelist.
                assert_eq!(transfer_who(&cfg, None, true), None, "{case}");
                assert_eq!(transfer_who(&cfg, Some(""), true), None, "{case}");
                assert_eq!(transfer_who(&cfg, Some("whitelist"), true), wl.then_some(TransferWho::Whitelist), "{case}");
            }
        }
        // Off: nobody, whatever the switches say.
        let cfg = who_rt("off", true, true, true);
        for kind in [Some("whitelist"), Some(""), None] {
            assert_eq!(transfer_who(&cfg, kind, false), None);
        }
        assert_eq!(transfer_target(&cfg, "", None), None);
        // A number on an older version's blocked list is turned away, not forwarded.
        let mut cfg = who_rt("emergency", true, true, true);
        cfg.reject_numbers_moved = false;
        cfg.reject_numbers = vec![crate::settings_store::RejectEntry { number: "+15555550177".into(), reason: String::new() }];
        assert_eq!(transfer_target(&cfg, "+15555550177", None), None);
        assert_eq!(transfer_target(&cfg, "+15555550178", None).map(|t| t.who), Some(TransferWho::Stranger));
    }

    #[test]
    fn a_call_judged_sales_or_scam_never_gets_the_owner_dialed() {
        let judged = |released| crate::calls::ScamJudgement {
            kind: "scam".into(),
            typology: "impersonation".into(),
            confidence: "high".into(),
            source: "test".into(),
            released,
        };
        assert!(!urgent_blocked_by_scam(false, None));
        assert!(urgent_blocked_by_scam(true, None), "stalling started");
        assert!(urgent_blocked_by_scam(false, Some(&judged(false))), "judged, stalling off");
        assert!(urgent_blocked_by_scam(true, Some(&judged(false))));
        assert!(!urgent_blocked_by_scam(false, Some(&judged(true))), "let go again: an ordinary call");
    }

    #[test]
    fn a_number_not_in_the_contacts_gets_the_owner_dialed_once_in_30_minutes() {
        let limits = DialLimit::default();
        let t0 = Instant::now();
        let mut cfg = rt("emergency", "+15550001111");
        cfg.transfer_contacts = true;
        cfg.transfer_others = true;
        let stranger = Transfer { who: TransferWho::Stranger, name: String::new() };
        let family = Transfer { who: TransferWho::Whitelist, name: "Amy".into() };
        let friend = Transfer { who: TransferWho::Contact, name: "Bo".into() };
        let key = stranger_key("+15555550123", None);
        assert!(will_dial(&cfg, &stranger, &key, &limits, t0));
        assert!(limits.take(&key, t0));
        let later = t0 + Duration::from_secs(29 * 60);
        assert!(!will_dial(&cfg, &stranger, &key, &limits, later), "used 29 minutes ago");
        assert!(!limits.take(&key, later));
        // Another number has its own allowance; hidden numbers share one.
        let other = stranger_key("+15555550124", None);
        assert_ne!(key, other);
        assert!(limits.take(&other, later));
        assert_eq!(stranger_key("", None), stranger_key("unknown", Some("us")));
        assert_eq!(stranger_key("private", None), "hidden");
        assert!(limits.take("hidden", t0));
        assert!(!will_dial(&cfg, &stranger, &stranger_key("", None), &limits, later));
        // Whitelist and other contacts have no limit.
        for _ in 0..3 {
            assert!(will_dial(&cfg, &family, &key, &limits, later));
            assert!(will_dial(&cfg, &friend, &key, &limits, later));
        }
        // 30 minutes on, the number may get the owner dialed again.
        assert!(will_dial(&cfg, &stranger, &key, &limits, t0 + STRANGER_DIAL_EVERY));
        assert!(limits.take(&key, t0 + STRANGER_DIAL_EVERY));
        // No number: nobody is dialed, the owner is only alerted.
        let off = rt("emergency", "");
        assert!(!will_dial(&off, &family, "", &limits, t0));
        // The call's control carries the key the dial uses.
        assert_eq!(CallCtl::limited(key.clone()).limit_key(), Some(key.as_str()));
        assert_eq!(CallCtl::default().limit_key(), None);
    }

    #[test]
    fn the_owner_sees_a_neutral_name_and_urgent_in_english() {
        let langs = [Lang::ZhTw, Lang::ZhCn, Lang::En, Lang::Ja, Lang::Ko, Lang::Es, Lang::Fr, Lang::De, Lang::Pt];
        let list_names = ["白名單", "白名单", "ホワイトリスト", "화이트리스트", "whitelist", "lista blanca", "liste blanche", "lista branca"];
        for lang in langs {
            let mut words = vec![alert_title(lang)];
            words.extend(crate::direct_transfer_words(lang));
            for w in words {
                for name in list_names {
                    assert!(!w.to_lowercase().contains(name), "{lang:?}: {w}");
                }
            }
        }
        for w in [alert_title(Lang::En)].into_iter().chain(crate::direct_transfer_words(Lang::En)) {
            assert!(!w.to_lowercase().contains("emergency"), "{w}");
        }
        // The console: this section and the call record's labels, in every language.
        let html = include_str!("../static/calls.html");
        let block = |code: &str| {
            let start = html.find(&format!("I18N[\"{code}\"] = {{")).expect("language block");
            let rest = &html[start..];
            &rest[..rest.find("\n};").expect("end of block")]
        };
        for code in ["en", "zh-TW", "zh-CN", "ja", "ko", "es", "fr", "de", "pt"] {
            for line in block(code).lines().map(str::trim) {
                let Some((key, value)) = line.split_once(": ") else { continue };
                if key.starts_with("ev_whitelist") || key == "wl_title" || key == "wl_desc" || key.starts_with("f_wl_mode_h_") {
                    for name in list_names {
                        assert!(!value.to_lowercase().contains(name), "{code} {line}");
                    }
                }
                let ours = key.starts_with("ev_whitelist") || key.starts_with("wl_") || key.starts_with("f_wl_");
                if code == "en" && ours {
                    // "emergency services" / "emergency number" are about calling for help.
                    let plain = value.replace("emergency services", "").replace("emergency number", "");
                    assert!(!plain.to_lowercase().contains("emergency"), "{line}");
                }
            }
        }
    }
}
