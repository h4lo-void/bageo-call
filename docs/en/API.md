# API

The web console is a single page on top of a small JSON API, and everything
the page does you can do from a script: read calls and transcripts, change
settings, decide appointments, manage contacts, switch phones between AI and
normal mode, and have the AI place calls for you. This page describes every
route of Bageo 1.0.

- **Base address:** where the agent listens, by default
  `http://127.0.0.1:8787`. Every route below starts with `/calls/api/`,
  except `/health` and the phones' WebSocket.
- **Format:** JSON in UTF-8, unless a route says otherwise (WAV, CSV, vCard,
  iCalendar). Send JSON bodies with `Content-Type: application/json`.
- **Times:** call records use RFC 3339 in UTC (`2026-11-14T07:02:11Z`);
  appointments use local time (`TZ`) as `YYYY-MM-DDTHH:MM`; errands use
  local time with its offset (`2026-11-15T19:00:00+08:00`).
- **Stability:** field names come from the Rust structs in
  [`crates/agent/src`](../../crates/agent/src); new fields may be added in later
  versions, so ignore what you do not know.

## Access

Without `CALLS_UI_TOKEN` the API is open to whoever can reach the port (by
default only the computer itself). With it, send the token on every request in
one of these ways:

```text
GET /calls/api/list?token=<token>
Authorization: Bearer <token>
X-Calls-Token: <token>
```

A wrong or missing token gets HTTP 401. `/health` still answers without the
token, with a short monitoring subset (see [Phones and health](#phones-and-health)).

```bash
curl -s -H "Authorization: Bearer $CALLS_UI_TOKEN" \
  "http://127.0.0.1:8787/calls/api/list?limit=5"
```

## Errors and status codes

Errors answer `{ok: false, code, error}`: `code` is a stable word to act on (the
web console shows it in your language), `error` the details (mostly in
English). Two routes answer in their own shape: `POST /calls/api/devices/{serial}/mode`
always answers `{ok, mode, error_code}`, and the errand routes put the same code
in both `error` and `code`.

| HTTP | When | Typical `code` |
|---|---|---|
| 200 | Done. | — |
| 400 | The request is wrong: not JSON, a missing `Content-Type: application/json`, a bad id or value. | `bad_request`, `invalid_id`, `missing_field`, `invalid_contact`, an errand code |
| 401 | Token missing or wrong. | `unauthorized` |
| 404 | No such call, task, contact or errand. | `not_found`, `task_not_found`, `contact_not_found`, `no_audio` |
| 409 | The change conflicts with what is stored. | `contact_exists`, `sms_not_retryable` |
| 413 / 415 | An import file too large / of an unknown type. | `file_too_large`, `unsupported_file` |
| 502 | An outside service failed (notification channel, Gemini). | `auth_failed`, `network_error`, `ai_failed`, `preview_failed` |

### Protection against other web pages

The console has no login cookie, but a browser on your network could still be
tricked by another web page into sending requests to it. Every route that
places a call, sends a text, decides something or changes stored data
therefore insists on `Content-Type: application/json` (a request without it gets
HTTP 400 `bad_request`). A page on another site cannot send that header without a
CORS preflight, and the agent never answers preflights. This covers:

- `POST /calls/api/tasks/{id}/confirm` and `…/ignore`;
- `POST /calls/api/appointments/{id}/retry-sms`;
- `POST /calls/api/devices/{serial}/mode`;
- `POST /calls/api/contacts`, `PUT /calls/api/contacts/{id}`, `POST /calls/api/contacts/batch`
  (and `POST /calls/api/contacts/import` needs an `X-Filename` header);
- `POST /calls/api/errands`, `…/{id}/cancel`, `…/{id}/retry`, `…/{id}/say`.

An empty body counts as `{}` where a route allows one. This is a second line
of defence; the first is `CALLS_UI_TOKEN` whenever the console is reachable from
your network.

## Calls

| Method and path | What |
|---|---|
| `GET /calls/api/list` | Call records, newest first. Query: `limit`, `offset`, `q` (free text over number, phone, events, transcript), `caller`, `serial`, `status`, `has_transcript`. Answer: `{ok, calls: [...], total, limit, offset, contacts_by_number}`; `contacts_by_number` is `{<caller_id>: {id, name}}` for the callers on the page who are contacts. |
| `GET /calls/api/call/{id}` | One call: `{ok, call, notes, contact, calendar_link, calendar_links}`. `contact` is `{id, name, note}` or `null`. `calendar_links` = `{google, outlook, office365, yahoo}`: each web calendar's "new event" form, filled in with the call's calendar suggestion (`calendar_link` is the Google one, kept for older clients). |
| `GET /calls/api/call/{id}/event.ics` | The calendar suggestion as an iCalendar file. HTTP 404 when the call has none. Opens with the console token, or with `?k=<key>`: an HMAC key that a notification link carries for this one file instead of the token. |
| `GET /calls/api/audio/{id}/listen` | The call as you would have heard it (caller and AI mixed), WAV, range requests supported. |
| `GET /calls/api/audio/{id}/remote` · `/tts` · `/audio/{id}` | Caller only · AI only · the raw mixed recording. HTTP 404 `no_audio` when that file does not exist. |
| `GET /calls/api/live` | Calls in progress and the phones connected: `{ok, count, calls, control_idle_clients, control_idle_serials, normal_mode_serials}`. `normal_mode_serials` are connected phones switched to a normal phone; the page does not count them as ready. |
| `GET /calls/api/stats` | `{ok, stats: {total, today, voicemails, blocked}}`. |
| `GET /calls/api/export.csv` | The filtered list as CSV (same query as `list`, up to 2000 rows). |

### The call record

A call record (`call`, and each entry of `calls`) carries among others:

| Field | Meaning |
|---|---|
| `id` | The record's id, used by every `/call/{id}` and `/audio/{id}` route. |
| `serial` | The phone that took (or placed) the call. |
| `caller_id` | The other side's number as the phone reported it; empty for a hidden number. |
| `started_at` / `ended_at` | RFC 3339, UTC. |
| `duration_ms` | Length of the call. |
| `status` | `ringing`, `active`, `ended`. |
| `events` | What happened, in order — e.g. `active`, `persona_stall_spam` (judged sales / scam), `reject_list` (a blocked number), `call_waiting_rejected` (a second call declined during a call), `ai_callback` (the AI's call-back for an appointment), `ai_errand` (an AI errand), `persona_ai_fail_hangup`, `ended`. |
| `summary` | The summary text, once written. |
| `appointment` | When the caller set a time with the AI: the appointment as the call left it (see [Appointments](#appointments-and-tasks)). `notes.appointment` is kept up to date afterwards (your decision, the text and call-back results). |
| audio flags | Which recordings exist. |

### Notes, transcript and translations

`notes` is `null` until the summary is written, then:

| Field | Meaning |
|---|---|
| `summary` | The summary in the notification language. |
| `dialogue` | The transcript, one turn per line, each prefixed with the speaker label in the agent's language and its separator: `Caller: ` / `AI: ` in English, `對方：` / `接聽：` in Traditional Chinese, `对方：` / `接听：` in Simplified Chinese, `相手：` / `AI：`, `상대: ` / `AI: `, `Llamante: ` / `IA: `, `Appelant : ` / `IA : `, `Anrufer: ` / `KI: `, `Chamador: ` / `IA: `. Records saved by earlier versions use `對方：` / `接聽：` in every language. Each line stays in the language it was spoken in. |
| `dialogue_translations` | Present only when something was translated: an array with one entry per line of `dialogue` (same index). An entry is the translation into the notification language of a line spoken in another language, or `null` for a line already in your language, too short, or not translated. The console shows the translation under the original line. |
| `end_hint`, `source`, `updated_at` | How the call ended, what wrote the notes, when. |
| `calendar` | When the call fixed a date: `{title, start, end, all_day}` (local time, `YYYY-MM-DDTHH:MM`). |
| `appointment` | The appointment as it stands now. |

```json
{
  "dialogue": "Caller: Hola, ¿está Alex?\nAI: Sí, soy yo. ¿Quién habla?",
  "dialogue_translations": ["Hello, is Alex there?", "Yes, speaking. Who's calling?"]
}
```

A caller who is a contact gets their name on the call list
(`contacts_by_number`), on the call (`contact`), after the number in the title of
the summary notification, and in the AI's prompt on ordinary calls.

## Settings

| Method and path | What |
|---|---|
| `GET /calls/api/settings` | `{ok, settings, storage_path, console_url, console_ip_url}`. `console_url` is the console's address in use (the one you typed, or the automatic one; empty when only this computer can open it), `console_ip_url` the same with the LAN IP instead of a `.local` name. Secrets come back masked (`"abc…wxyz"`) with a `<name>_set` flag; notification channels come back with empty `url` / `token` and `url_hint`, `token_hint`, `url_set`, `token_set`. Read-only helpers: `default_prompt`, `voices`, `ambience_kinds`, `languages`, `countries` (ISO codes for *Main language for each SIM*), `seen_sim_countries` (SIM countries the phones reported since the agent started), `notify_kinds`, `sample_lines`, `summary_ready`. |
| `PUT /calls/api/settings` | A JSON object with only the fields to change. Answers as `GET`. Applied to the next call; no restart. |

Fields worth knowing (all of them are documented in `RuntimeSettings` in
[`settings_store.rs`](../../crates/agent/src/settings_store.rs) and on the
[Settings reference](SETTINGS.md)):

| Field | Values |
|---|---|
| `owner_name` | Your name, used for `{owner}`. |
| `owner_aliases` | Array of strings, up to 10: other names callers use for you. Extra entries and blanks are dropped. |
| `answer_style` | `assistant` / `persona` (as you). |
| `errand_identity` | `assistant` (default) / `owner`: who the AI says it is on an AI errand; anything else is stored as `assistant`. |
| `summary_provider` | `gemini` (default) / `custom`. `custom` uses `summary_api_base`, `summary_model` (required with a base URL) and `deepseek_api_key` (the key of that service). An older `auto` / `deepseek` is converted once when the settings load. |
| `calendar_app` | `google`, `outlook`, `office365`, `apple`, `yahoo`, `ics`, or empty (not chosen yet); anything else is stored as empty. |
| `appointment_mode` | `off` / `confirm` (an older `auto` reads as `confirm`). |
| `appointment_sms` | `true` / `false`: text the caller your decision. |
| `appointment_reply` | `auto`, `sms` or `call`. |
| `reject_numbers`, `reject_mode` | A list of `{number, reason}`; `decline` (refuse while it rings) or answer and announce. |
| `country_code` | Digits only (`886`, `1`). Not shown in the console; a value here wins over `COUNTRY_CODE`. |
| `gemini_thinking_level` | `low` / `medium` / `high` (thinking models only). |

Rules for `PUT`:

- A secret field (`gemini_api_key`, `deepseek_api_key`) that is absent or `""`
  keeps its stored value; `"<name>_clear": true` empties it.
- `notify_channels` is replaced as a whole. A row `{id, kind, name, enabled, url, token, target}`
  whose `url` or `token` is empty keeps the stored value of the row with the same
  `id` and `kind`; a row without `id` is new; rows you leave out are deleted.
- Keys an older version stored and this one no longer has are ignored and
  dropped at the next save: the Google Calendar credentials (`gcal_*`),
  `calendar_auto_add`, `intl_carrier_prompt`, and the test-number and test-text settings of the removed demo calendar.

```bash
curl -s -X PUT "http://127.0.0.1:8787/calls/api/settings" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"language": "ja", "owner_aliases": ["Alex", "Al"], "errand_identity": "owner",
       "reject_numbers": [{"number": "0800*", "reason": "telemarketing"}]}'
```

## Notifications

| Method and path | What |
|---|---|
| `POST /calls/api/notify/test` | Body `{"channel": {kind, url, token, target, id?}}`. Sends the test message to that one channel (stored secrets are filled in by `id`). HTTP 200 `{ok: true}` or 502 `{ok: false, code, field, kind, error}`: `code` says what went wrong (`missing_field`, `bad_url`, `bad_value`, `auth_failed`, `service_not_found`, `http_error`, `service_error`, `network_error`, `send_failed`), `field` which of `url` / `token` / `target` (or empty), `error` the service's answer. |
| `POST /calls/api/notify/send` | Body `{"title": "...", "body": "..."}`. Sends your message to every enabled channel. `{ok: true}` when at least one accepted it. The [SMS forwarder](../../tools/sms_forward/README.md) uses this route. |

The JSON your own webhook receives, and every event type, is described in
[Notifications](NOTIFICATIONS.md#webhook-json).

## Voice and sound previews

`GET /calls/api/preview` returns a WAV. Query: `voice` (a voice name, or `none`
for the background alone), `ambience` (`off` or a kind), `level` (1–3),
`regen=true` (generate again instead of the stored sample), `clip=reject` (the
blocked-number announcement in that voice), `lang` (`ja`, `zh-TW`, …; empty = the
saved language). Every one of the 30 built-in voices comes recorded in each of
the 9 languages, both the preview and the announcement — 1,080 clips in
`assets/voices/` — so a preview needs no Gemini session; only `regen=true` or a
voice without a recording opens one. Errors: 502 `preview_failed`.

## Appointments and tasks

With `appointment_mode` `confirm` (see
[Settings](SETTINGS.md#summary--calendar)), a call where the caller set a time
with the AI leaves a task (`intent` `appointment`) and an `appointment` object on
the call record, in its notes and on the task:

```json
{
  "start": "2026-11-14T15:00", "end": "2026-11-14T16:00",
  "title": "House viewing", "caller_name": "Mr Chen",
  "mode": "confirm", "status": "confirmed",
  "reply": "sms", "sms": "sent", "call_lang": "en",
  "tag": "6f1c2a9e-3b7d-4e0a-9c55-2d8f1b7e4a10",
  "tg_card": {"chat_id": "123456789", "message_id": 42}
}
```

| Field | Meaning |
|---|---|
| `start`, `end` | Local time (`TZ`). A back-up time the caller only mentioned does not replace the agreed one; a second, separate appointment on the same call becomes a task of its own. |
| `title`, `caller_name` | What it is about, and how the caller introduced themselves (the caller's own words). |
| `status` | `pending` (waiting for you), `confirmed`, `declined`; `booked` only on records from older versions. |
| `reply` | How the caller was told: `sms`, `call` or `none`. |
| `sms` | `sent`, or why the text did not go: `offline` (the phone is not connected), `old_app` (its app cannot send texts), `timeout`, `busy` (the same text is already being sent), `no_number` (a number that takes no texts), or the phone's code: `no_permission`, `bad_request`, `no_service`, `radio_off`, `generic_failure`, `exception`. The console and the Telegram reply say each in words. |
| `call` | The AI call-back: `queued` (after an earlier call-back to the same number), `dialing`, `connected` (the phone dialed; the AI waits for the callee to speak), `answered` (the callee was told), `no_answer` (nobody spoke within 70 s), `failed`, `done` (ended, outcome unknown), or why it was not placed: `no_phone`, `send_failed`, `no_number`, `not_callable` (abroad, toll-free, premium or a short code). The phone's reports are also in the task's `events` (`callback_place_call_ack`, `callback_place_call_failed`, `callback_outbound_ready`, `callback_done`, `callback_failed`). |
| `call_lang` | The language spoken on the call (`zh-TW`, `en`, …). The AI call-back speaks it, and says your answer once, after the callee has picked up and spoken. Records without it use the `language` setting. |
| `tg_card` | `{chat_id, message_id}` of the Telegram card sent for it; its buttons are taken off when you decide elsewhere. |
| `tag` | A stable id for the appointment. |

Empty fields are left out.

| Method and path | What |
|---|---|
| `GET /calls/api/tasks` | Tasks, newest first (up to 200): `{ok, tasks}`. An appointment task also carries `calendar_links` (`{google, outlook, office365, yahoo}`), `reply` = `{sms, call, default}` (whether a text or a call-back can reach this caller, and the way *Default reply* picks) and `sms_route` = `{serial, label}`: the phone its text goes out from when you pick none (the phone that took the call; `label` its name in `device_labels`, else its id). The console's *To do* is the tasks whose `appointment.status` is `pending`. |
| `GET /calls/api/tasks/{id}/event.ics` | An appointment as an iCalendar file (Apple Calendar and other apps). Opens with the console token or a link's `?k=<HMAC key>`. |
| `POST /calls/api/tasks/{id}/confirm` | **Accept**, as in the web console or on the Telegram card. Body: empty, `{}` or `{"reply": "sms" \| "call" \| "none"}` (none given: *Default reply*; a way that cannot reach this caller tells nobody). Answer `{ok, task, message}`; `message` is what happened, in words, as the Telegram bot replies it, with the *Add to calendar* link for your `calendar_app`. A text may take up to 90 seconds; a call-back is handed to the phone (follow it in `appointment.call`). |
| `POST /calls/api/tasks/{id}/ignore` | **Decline**: the caller is told the same way, with a polite no. Same body, answer and rules. |
| `POST /calls/api/appointments/{id}/retry-sms` | Text the caller of a decided appointment again, when the text did not go out. Body: empty, `{}` or `{"serial": "<phone>"}` (another connected phone; none: the phone that took the call). Answer `{ok, sms, message}`: `ok` when it was sent, `sms` the new code. HTTP 409 `sms_not_retryable` (with the current `sms`) when it is still pending, already sent or never to be sent (`no_number`); 404 `task_not_found`; 400 `invalid_serial` / `bad_request`. |

Rules for decisions:

- **Only the first decision counts.** A second confirm or decline — also one
  sent while the first is still running, or a press on the Telegram card —
  answers "already handled" and changes nothing.
- When a Telegram card went out for the appointment, its buttons are taken off
  and `message` is posted in that chat (best effort; the decision stands either
  way).
- Texts and call-backs go out from the SIM the call came in on (`sub_id`). A
  text says only the time with its weekday, never what the appointment is
  about.
- Sales and scam calls never leave an appointment, and an appointment from a
  call judged one afterwards is dropped.

```bash
curl -s -X POST "http://127.0.0.1:8787/calls/api/tasks/$TASK/confirm" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"reply": "call"}'
```

```json
{"ok": true, "task": {"id": "…", "appointment": {"status": "confirmed", "reply": "call", "call": "dialing"}},
 "message": "✅ Appointment accepted\nSat, Nov 14, 3:00 PM–4:00 PM House viewing\nThe AI is calling 0912345678 back\n📅 Add it to your calendar: https://calendar.google.com/…"}
```

## AI errands

The AI makes a call for you: you give it a number (or a contact) and what to
get done — book a table, fix a time with a friend, ask a shop something — and a
phone in AI mode dials out.

**During the call.** The web console can show live captions. When the AI meets
something your instructions do not settle, it tells the other side it will
check and asks you (the `ask_owner` function): if you are watching the errand
(something read its live view in the last 6 seconds), the question shows there
for up to 40 seconds; otherwise, or when you do not answer in time, the AI asks
which options would work, says you will confirm and call back, and ends the
call politely. The AI waits for the answer and settles nothing about it
meanwhile; if the other side says goodbye first, the question ends there.
Choices inside the room your instructions leave ("7:30 to 8 is fine too") are
accepted without asking. What you type during the call goes to the AI as an
instruction.

**Afterwards.** The summary model reads the call against the errand and you
get one notification (title "📞 AI errand call: <name or number>", in the
notification language; webhook event `ai_errand`). Something the AI agreed to
on a question you did not answer is never `done`: the result is `need_owner`,
telling you to confirm it or call back to change it.

**Who calls.** `errand_identity` (setting; `assistant`, your assistant, by
default, or `owner`, as you), and each errand may pick its own. Either way the
AI says it is an AI when sincerely asked, gives out no personal details beyond
the errand, and agrees to no payment, deposit or transfer.

**Which numbers.** Never an emergency number, a short code (7 digits or fewer)
or a premium-rate number (e.g. Taiwan 020x such as 0204 and 0209, +1 900 / 976,
Japan 0990, UK 09…); toll-free numbers are fine. A number written without a plus is one of
your country (`country_code`); one that starts with an international call
prefix instead (`00`, Taiwan's `01x` and `00x`, North America's `011`, Japan's
`010`) is `not_allowed` (write numbers abroad with `+`), as is a Taiwan number
starting with 1 after the trunk 0.

**Which phone and SIM.** The call goes out from a connected phone in AI mode
with a SIM of the number's country. The agent learns each phone's SIMs
(subscription id and country) from the calls the phone reports (`sim_sub_id`
and `sim_country` in `incoming` and the media `hello`), and from an optional
`sims: [{sub_id, country}]` list in the control `hello` (the 1.0 app does not
send one yet); they are kept in `phone_sims.json`. A phone whose SIMs it has
never heard about takes no errand (`sim_unknown`: call that phone once first).
The phone is sent the number in E.164 form with the SIM to use (`sub_id`);
when the call's own report (its media `hello`) shows another SIM, or none, the
agent hangs up at once, before the AI starts (`failed`, `call_code` `wrong_sim`,
you are told), and forgets that SIM on that phone until a call reports it
again.

**Queues and limits.** A phone on a call: the errand waits (`queued`) and
fails after 10 minutes (`phone_busy`). One call to a number at a time, shared
with the appointments' AI call-backs; an errand waiting behind an earlier call
to its number fails after 25 minutes (`number_busy`). A call lasts at most 10
minutes (the AI wraps up at 9:30). A voice menu: the AI waits up to 90 seconds
for a person (it cannot press keys; `keypad_menu`); a carrier recording ("the
number you dialed is not in service"): it hangs up at once
(`carrier_message`). No retries on its own: **Try again** is yours. Cancelling an
errand that is dialing frees the phone and the number for the next call at
once; should its call still start, it is hung up before the AI speaks.

### The errand object

```json
{
  "id": "e3f1a29c0b7d4e55", "created_at": "2026-11-14T18:02:11+08:00", "updated_at": "2026-11-14T18:05:40+08:00",
  "number": "0223456789", "name": "Din Tai Fung", "contact_id": "",
  "task": "Book a table for 4 tomorrow at 7 pm under Wang. 7:30 to 8 is fine too.",
  "identity": "assistant", "serial": "pixel-12345", "device_label": "Pixel 9",
  "status": "done", "call_code": "answered", "attempts": 1, "record_id": "c8b2…",
  "follow_up_of": "",
  "questions": [{"id": "q1", "text": "7 pm is full; 7:30 or 8:15?", "options": ["7:30", "8:15"],
                 "answer": "7:30", "answered_by": "owner", "asked_at": "2026-11-14T18:03:05+08:00"}],
  "result": {"outcome": "done", "summary": "Table for 4 booked tomorrow 7:30 pm under Wang",
             "details": "They hold the table for 15 minutes.", "follow_up": "", "options": [],
             "event": {"start": "2026-11-15T19:30:00+08:00", "title": "Dinner at Din Tai Fung"}},
  "history": []
}
```

| Field | Meaning |
|---|---|
| `id` | `e` + 16 hex characters. |
| `number` | As you wrote it, without spaces and dashes. |
| `name`, `contact_id` | The name to use (else the contact's name), and the contact it came from. |
| `task` | Your own words (up to 1,000 characters). |
| `identity` | `assistant` or `owner`. |
| `serial`, `device_label` | The phone that dials it, and its name. |
| `status` | `queued`, `dialing` (the line rings, nobody has spoken yet), `on_call` (the other side picked up and spoke), `done`, `no_answer`, `failed`, `cancelled`. The last four are final. |
| `call_code` | Where the dial stands: `dialing`, `queued`, `answered`, `no_answer`, `failed`, `phone_busy`, `number_busy`, `no_phone`, `send_failed`, `keypad_menu`, `carrier_message`, `wrong_sim`, `agent_restarted`, `cancelled`. |
| `attempts` | 1, plus one per **Try again**. |
| `record_id` | The call record of this try (`GET /calls/api/call/{id}`: transcript and recording). |
| `follow_up_of` | An earlier errand this call follows up (its last 20 transcript lines go into the AI's script). |
| `questions` | `[{id, text, options, answer, answered_by, asked_at}]`: what the AI asked you. `answered_by`: `owner` (you answered), `timeout` (you watched but did not answer in time), `absent` (nobody was watching); `null` while it waits. Up to 4 options. |
| `result` | `null` or `{outcome, summary, details, follow_up, options, event}`. `outcome`: `done`, `partial`, `not_done`, `need_owner` (choices in `options`, up to 8), `no_answer`, `voicemail`, `failed`. `event`: `null` or `{start, end, title}` (local time with its offset; `end` left out when not agreed). |
| `history` | Earlier tries, `[{attempt, status, result, record_id, ended_at, questions}]`. |

Errands are kept in `errands.json` in the data folder (the newest 500). One
still under way when the agent restarts becomes `failed` (`call_code`
`agent_restarted`).

### Errand routes

| Method and path | What |
|---|---|
| `GET /calls/api/errands` | `{ok, errands}`, the newest first, at most 200. |
| `POST /calls/api/errands` | Start one. Body `{number, name, contact_id, task, identity, serial, follow_up_of}`: `task` and a number (or a contact, or an earlier errand, whose number is used) are required; `identity` empty = `errand_identity`; `serial` empty = any suitable phone. Answer `{ok, errand}` (status `dialing`, or `queued` when the phone or the number is busy). |
| `POST /calls/api/errands/{id}/cancel` | Waiting: dropped; dialing or on the call: hung up. An errand already over is returned unchanged. `{ok, errand}`. |
| `POST /calls/api/errands/{id}/retry` | The same errand once more (`attempts` + 1, the last try moved to `history`). `{ok, errand}`; under way (or its last call not ended yet): 400 `busy`; the phone codes as for a new errand. |
| `DELETE /calls/api/errands/{id}` | Removes it from the list (its call records stay). `{ok}`; dialing or on the call: 400 `busy`. |
| `GET /calls/api/errands/{id}/live?after=<seq>` | `{status, lines, question, watching}`. `lines`: the captions after `seq`, `[{seq, who, text, at}]` with `who` `ai`, `callee`, `owner` or `system` (the newest 400 are kept; a line is complete after 1.2 s without more words). `question`: `null` or `{id, text, options, asked_at, expires_at}`, the question waiting for you. `watching`: whether you count as watching. Polling this route marks you as watching for 6 seconds, so poll it every few seconds while the page is open. |
| `POST /calls/api/errands/{id}/say` | Body `{text, question_id}` (up to 300 characters): with `question_id`, the answer to that question; without it, the answer to the question waiting, or else an instruction for the AI ("tell them 8 pm works too"). `{ok}`. |

Error codes (HTTP 400 `{ok: false, error, code}`, the same word in both):

| Code | Route | Meaning |
|---|---|---|
| `no_number` | create | No number, and no contact or earlier errand to take one from. |
| `bad_number` | create | Not a phone number (letters, a plus that does not lead, more than 15 digits, too short for its country). |
| `not_allowed` | create | Emergency, short code, premium rate, or an international prefix instead of `+`. |
| `no_task` | create | `task` empty. |
| `task_too_long` | create | `task` over 1,000 characters. |
| `no_phone` | create, retry | No connected phone in AI mode (or the `serial` asked for is not one). |
| `sim_unknown` | create, retry | No phone fits, and at least one has SIMs the agent has not learned yet: call it once. |
| `no_sim_for_country` | create, retry | No phone has a SIM of the number's country. |
| `not_found` | create | `contact_id` or `follow_up_of` does not exist (400 here). |
| `busy` | retry, delete | The errand is still dialing or on the call. |
| `not_on_call` | say | The call is not (or no longer) running. |
| `empty_text` / `text_too_long` | say | Nothing to say / over 300 characters. |
| `question_closed` | say | That question was answered, timed out, or is not the one waiting. |
| `bad_request` | all `POST` | No `Content-Type: application/json`, or the body is not a JSON object. |

On every `…/errands/{id}…` path an unknown (or malformed) errand id is HTTP 404
`not_found`.

```bash
curl -s -X POST "http://127.0.0.1:8787/calls/api/errands" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"number": "02-2345-6789", "name": "Din Tai Fung",
       "task": "Book a table for 4 tomorrow at 7 pm under Wang. 7:30 to 8 is fine too."}'

curl -s "http://127.0.0.1:8787/calls/api/errands/e3f1a29c0b7d4e55/live?after=0" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN"

curl -s -X POST "http://127.0.0.1:8787/calls/api/errands/e3f1a29c0b7d4e55/say" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"text": "7:30 is fine", "question_id": "q1"}'
```

```json
{"status": "on_call",
 "lines": [{"seq": 3, "who": "callee", "text": "We're full at seven. Half past or quarter past eight?", "at": "2026-11-14T18:03:01+08:00"},
           {"seq": 4, "who": "ai", "text": "Let me just check with him.", "at": "2026-11-14T18:03:04+08:00"}],
 "question": {"id": "q1", "text": "7 pm is full; 7:30 or 8:15?", "options": ["7:30", "8:15"],
              "asked_at": "2026-11-14T18:03:05+08:00", "expires_at": "2026-11-14T18:03:45+08:00"},
 "watching": true}
```

## Contacts

Names for the numbers that call, kept in `contacts.json` in the call records'
folder (`CALLS_DATA_DIR`). A number matches however it is written:
`+886912345678`, `0912-345-678` and `886912345678` are one number; a number
without a country code is read in your country (`country_code`) or, for a call,
in the country of the SIM it came in on. Contacts also feed the AI errands
(pick a contact instead of typing a number) and the sales check: a saved contact
talking about money is left alone.

A contact is `{id, name, phones: [...], note, source, updated_at}`; `source` is
`manual`, `vcf`, `csv`, `text` or `ai` (read by Gemini). A number belongs to one
contact only.

| Method and path | What |
|---|---|
| `GET /calls/api/contacts` | Query `q` (name, note, or a number written any way), `limit`, `offset`. `{ok, contacts, total, offset, count}`: the matches by name (`total` of them) and how many contacts there are in all (`count`). |
| `POST /calls/api/contacts` | Body `{name, phones: [...], note}`: `{ok, contact}`. A number that is another contact's: HTTP 409 `contact_exists` with `contact: {id, name}`; no name or no number: 400 `invalid_contact`. |
| `PUT /calls/api/contacts/{id}` | The same body; replaces the name, numbers and note. 404 `contact_not_found`, 409 and 400 as above. |
| `DELETE /calls/api/contacts/{id}` | `{ok}`. |
| `POST /calls/api/contacts/import` | The file itself as the body (up to 10 MB), its name URL-encoded in `X-Filename`. Reads it and **saves nothing** (a preview): `{ok, file, format, ai, entries: [{name, phones, note, source, duplicate}], skipped}`. `format`: `vcf`, `csv`, `text`, `xlsx`, `docx`, `pdf` or `image`; `ai`: Gemini read it; `duplicate`: `null`, or the saved contact the entry repeats, `{id, name, by: "phone" \| "name"}`; `skipped`: entries without a name or a number. Errors: 400 `empty_file`, 413 `file_too_large`, 415 `unsupported_file`, 400 `need_gemini_key`, 502 `ai_failed` (Gemini's answer in `error`). |
| `POST /calls/api/contacts/batch` | Body `{contacts: [{name, phones, note, source}], duplicates: "merge" \| "skip"}`: the entries you keep from an import. One that repeats a saved contact is merged into it (its new numbers and note added) or skipped. `{ok, added, merged, skipped, count}`. |
| `GET /calls/api/contacts/export.vcf` | Every contact as a vCard 3.0 file. |

Read by the agent itself: vCard 2.1, 3.0 and 4.0 (quoted-printable, any
`CHARSET`); CSV and TSV with a header row (Google Contacts, Outlook, or columns
named like *Name*, *Phone*, 姓名, 電話) or without one, in UTF-8, UTF-16 or your
computer's encoding (Big5, GBK, Shift_JIS, EUC-KR, Windows-1252); plain text
with a name and a number on each line; the tables in `.xlsx` and `.docx` files.
PDF files, pictures (a photo of a printed list, a screenshot of a phone book)
and files in which nothing was found go to Gemini's vision model
(`gemini-flash-lite-latest`, with the Gemini API key of the settings).

```bash
curl -s -X POST "http://127.0.0.1:8787/calls/api/contacts/import" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" \
  -H "X-Filename: $(printf %s 'family list.pdf' | jq -sRr @uri)" \
  --data-binary @"family list.pdf"
```

```text
POST /calls/api/contacts
Content-Type: application/json
{"name": "Mei Lin", "phones": ["0912 000 000", "+1 202 555 0143"], "note": "my sister"}

{"ok": true, "contact": {"id": "4f0c9a…", "name": "Mei Lin", "phones": ["0912 000 000", "+1 202 555 0143"],
 "note": "my sister", "source": "manual", "updated_at": "2026-11-14T08:00:00Z"}}
```

## Phones and health

| Method and path | What |
|---|---|
| `GET /calls/api/devices` | `{ok, usb: {enabled, adb_found, adb_path, error, phones: [{serial, state, model, reverse, app_installed}]}, connected: [...], labels: {<phone id>: <model>}, calls_in_progress, phones: [...]}`. Each entry of the top-level `phones` is a phone seen on its control connection since the agent started: `{serial, connected, mode, supports_mode, model, app_version, in_call, last_seen_ms}`. `mode` is `ai` (the AI answers) or `normal` (an ordinary phone, see [Phone setup](PHONE.md#normal-phone-mode)); `supports_mode` is false for apps older than the switch, which always run as `ai`. |
| `POST /calls/api/devices/{serial}/mode` | Body `{"mode": "ai" \| "normal"}`. Asks the phone to switch and waits up to 15 seconds for its answer. Always `{ok, mode, error_code}` (see below). |
| `GET /health` | A large flat JSON object for troubleshooting: version, language, what is configured, connected phones, calls in progress; no keys or tokens. `control_idle_clients` / `control_idle_serials` count every connected phone; `normal_mode_serials` lists those switched to a normal phone; `local_time` is the agent's clock in its `TZ`. Without the console token (when one is set), only `ok`, `version`, `language`, `active_call_count` and `control_idle_clients` are returned — enough for monitoring. |

`POST /calls/api/devices/{serial}/mode` in detail: `mode` is the phone's mode
afterwards (`null` when unknown); `error_code` is `null` on success, otherwise one
of:

| `error_code` | Meaning |
|---|---|
| `offline` | The phone is not connected. |
| `unsupported` | Its app is older than the mode switch. |
| `in_call` | A call rings or is up; try after it. |
| `busy` | A switch is already running. |
| `no_root` | The Bageo module's root helper did not take the request: the module is not running. |
| `save_failed`, `dialer_failed`, `screening_failed`, `dnd_failed`, `volume_failed`, `failed` | The step that did not take effect. Switching again repeats every step. |
| `timeout` | Not confirmed yet — not "failed": the switch can restart the phone app, which reports after it reconnects. Read `GET /calls/api/devices` a little later. |
| `bad_request` / `invalid_mode` | HTTP 400: not JSON, no `Content-Type: application/json`, no string `mode`, a serial that is not letters, digits, `-` and `_`; or an unknown `mode`. |

```bash
curl -s -X POST "http://127.0.0.1:8787/calls/api/devices/pixel-12345/mode" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"mode": "normal"}'
# {"ok": true, "mode": "normal", "error_code": null}
```

## The phones' side (WebSocket)

Phones connect by WebSocket to `/` or `/ws` (same thing): one long-lived
*control* connection per phone and one *media* connection per call, carrying
JSON text frames and raw 16 kHz mono 16-bit little-endian PCM binary frames in
both directions (the phone sends 60 ms frames). A connection from the computer
itself (the USB tunnel) is always accepted; any other must carry
`?token=<DEVICE_TOKEN>` when that is set.

The complete message list, field by field, is in
[`android/README.md`](../../android/README.md#websocket-protocol). Anything that
speaks this protocol can stand in for the phone — which is how a SIP or
softphone front end could be added without touching the call logic. Both sides
ignore what they do not know, so old apps and old agents keep working.

### Control connection

| Direction | Message | Meaning |
|---|---|---|
| phone → agent | `hello` | `serial`, `mode` = `CONTROL_IDLE`, `model`, `app_version`, `phone_mode` (`ai` / `normal`), `in_call`, `supports_sms`, `supports_reject`. No `phone_mode` = an app older than the mode switch (always `ai`). No `supports_sms` = no texts are asked of it. No `supports_reject` = blocked numbers are answered and get the announcement instead. |
| phone → agent | `ping` | Every 15 s. |
| phone → agent | `status` | `{mode, in_call}`: the mode or the call state changed. |
| phone → agent | `incoming`, `incoming_cancel` | A call rings (`caller_id`, empty for a hidden number; `sim_country`; `sim_sub_id` when known) / ended before it was answered. |
| agent → phone | `answer_plan` | `{delay_ms, prewarm, reject}`: how long to ring before answering, or `reject: true` to decline the ringing call (blocked numbers with `reject_mode` `decline`). |
| phone → agent | `call_waiting_rejected` | `{serial, caller_id, sim_country, sim_sub_id}`: a second call rang during a call and the phone declined it at once (AI mode). Becomes a call record and a "📵" notification. |
| agent → phone | `set_mode` / phone → agent `set_mode_result` | `{mode, id}` / `{id, ok, mode, error}`: the remote mode switch; `error` uses the codes above. |
| agent → phone | `agent_info` | `{console_url}`: for the app's **Open the web console** button. Sent after the hello when the agent knows an address: `public_url`, or the LAN IP address when the console is open to your network. |
| agent → phone | `place_call` | `{number, call_id, task_id, sub_id}`: dial out, for an AI call-back or an AI errand, from the SIM with subscription id `sub_id` (the phone's default when that SIM is not found). While a call rings or is up the phone does not dial and answers `place_call_failed` with `reason` `busy`. |
| phone → agent | `place_call_ack`, `place_call_failed`, `outbound_ready`, `call_end` | The outgoing call was dialed / could not be dialed / connected / ended. |
| agent → phone | `send_sms` | `{id, to, text, sub_id}`: text a caller (appointments). `id` 1–64 characters; `to` digits with an optional leading `+`; `text` up to 500 characters; `sub_id` picks the SIM (the `sim_sub_id` the phone reported), else the default SMS SIM. An id already sent is answered again, never texted twice. |
| phone → agent | `send_sms_result` | `{id, ok, error}`: once every part reported back (the phone gives up after 60 s: `timeout`; a later report sends a second result with the real outcome). `error`: `no_permission`, `bad_request`, `no_service`, `radio_off`, `generic_failure`, `timeout`, `exception`. The agent waits up to 90 s. Results for the last 20 ids are sent again after a reconnect; the agent drops repeats by `id`. |
| phone → agent | `send_sms_delivered` | `{id, ok}`: the network's delivery report, when it gives one (the phone waits up to an hour). |

### Media connection

| Direction | Message | Meaning |
|---|---|---|
| phone → agent | `hello` | `serial`, `caller_id`, `sample_rate` (16000), `mode` = `SILENT_LOCAL`, `sim_country`, `sim_sub_id`, `model`, `app_version`. For an outgoing call the agent checks `sim_sub_id` against the SIM it asked for (errands: `wrong_sim`). |
| phone → agent | `ringing`, `call_end` | The caller's number (again, when it becomes known later) / the call ended. |
| agent → phone | `tts_start`, `tts_end`, `hangup` | Audio follows / ended / end the call. |
| both | binary | The caller's voice (phone → agent) and the AI's voice (agent → phone), PCM as above. |

On the agent's side: a text result that comes after the agent stopped waiting
is still recorded, and the phone's later real result under the same `id`
replaces a `timeout`; the same result again, or a different one for a text
already settled, changes nothing. A blocked number is declined only when
`reject_mode` is `decline` and the phone said `supports_reject`; otherwise it is
picked up and gets the announcement, and the notification says which happened.
A call-back and an errand carry `sub_id` so that a dual-SIM phone calls from the
right number.

