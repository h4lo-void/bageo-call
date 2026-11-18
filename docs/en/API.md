# API reference: script your AI phone assistant

Everything the Bageo web console does goes through a small JSON API on your
own computer, so a script can do it too. You can read calls, transcripts and
summaries, change settings, accept the appointments callers made with the AI,
manage contacts, switch a phone between AI answering and a normal phone, and
send the AI to make a phone call for you.

This page covers every route the console and the phones use in Bageo 1.4,
including the WebSocket protocol the rooted Android phone speaks.

| | |
|---|---|
| **Base address** | Where Bageo listens, by default `http://127.0.0.1:8787`. The console itself is `/calls/` (`/` redirects there). |
| **Routes** | API routes start with `/calls/api/`. The exceptions are `/health` and the phones' WebSocket. |
| **AI tools** | `POST /mcp` is a Model Context Protocol server with its own token, off by default: see [MCP](MCP.md). |
| **Format** | UTF-8 JSON unless a route says otherwise (WAV, CSV, vCard, iCalendar). Send JSON bodies with `Content-Type: application/json`. |
| **Times in call records** | RFC 3339 in UTC (`2026-11-14T07:02:11Z`). |
| **Times in appointments** | Local time (`TZ`) as `YYYY-MM-DDTHH:MM`. |
| **Times in errands** | Local time with its offset (`2026-11-15T19:00:00+08:00`). |
| **Stability** | Field names come from the Rust structs in [`crates/agent/src`](../../crates/agent/src). Later versions may add fields, so ignore the ones you do not know. |

## Try it

With Bageo running on this computer, these work as they are:

```bash
curl -s http://127.0.0.1:8787/health
curl -s "http://127.0.0.1:8787/calls/api/list?limit=5"
curl -s http://127.0.0.1:8787/calls/api/stats
```

If you set a console password (`CALLS_UI_TOKEN`), add `-H "Authorization: Bearer $CALLS_UI_TOKEN"` (see
[Access](#access)).

A few things people build on top:

| You want to | Use |
|---|---|
| Pull new transcripts into your notes or CRM | `GET /calls/api/list?has_transcript=true`, then `GET /calls/api/call/{id}` for each |
| Book a table by phone from a script | `POST /calls/api/errands`, poll `…/errands/{id}/live`, answer the AI with `…/say` |
| Accept an appointment from your own button | `POST /calls/api/tasks/{id}/confirm` |
| Let a phone ring normally while you are home | `POST /calls/api/devices/{serial}/mode` with `{"mode": "normal"}` |
| Forward any message to your channels | `POST /calls/api/notify/send` |
| Monitor Bageo | `GET /health` (works without the password) |

## Access

If no password is set (`CALLS_UI_TOKEN`), the API is open to whoever can reach
the port. By default that is only this computer.

With a password set, every request must carry it in one of these ways:

```text
GET /calls/api/list?token=<token>
Authorization: Bearer <token>
X-Calls-Token: <token>
```

A wrong or missing password gets HTTP 401 with
`{"error": "unauthorized", "code": "unauthorized", "hint": …}`. `/health` still
answers without it, with a short subset for monitoring (see
[Phones and health](#phones-and-health)).

```bash
curl -s -H "Authorization: Bearer $CALLS_UI_TOKEN" \
  "http://127.0.0.1:8787/calls/api/list?limit=5"
```

## Errors and status codes

Errors answer `{ok: false, code, error}`. Act on `code`: it is a stable word, and
the console shows it in your language. `error` holds the details, mostly in
English.

Two route groups use their own shape: the phone mode switch answers
`{ok, mode, error_code}`, and the errand routes put the same code in both
`error` and `code`.

| HTTP | When | Typical `code` |
|---|---|---|
| 200 | Done. | — |
| 400 | The request is wrong: not JSON, no `Content-Type: application/json`, a bad id or value. | `bad_request`, `invalid_id`, `missing_field`, `invalid_contact`, an errand code |
| 401 | Password missing or wrong. | `unauthorized` |
| 404 | No such call, task, contact or errand, or no such file. | `not_found`, `task_not_found`, `contact_not_found`, `no_audio`, `no_calendar` |
| 409 | The change conflicts with what is stored. | `contact_exists`, `sms_not_retryable`, `call_in_progress`, `device_online`, `device_in_call` |
| 413 / 415 | An import file too large / of an unknown type. | `file_too_large`, `too_many_rows`, `unsupported_file` |
| 500 | Bageo could not save or read its own files. | `save_failed`, `contacts_save_failed`, `import_failed` |
| 502 | An outside service failed (a notification channel, Gemini). | `auth_failed`, `network_error`, `ai_failed`, `preview_failed` |

### Protection against other web pages

Another web page open in a browser on your network could try to send requests
to the console, which has no login cookie. To stop that, every route that
places a call, sends a text, decides something or changes stored data requires
`Content-Type: application/json`. Without it the route answers HTTP 400
`bad_request`.

A page on another site cannot send that header without a CORS preflight, and
Bageo never answers preflights. This covers:

- `POST /calls/api/tasks/{id}/confirm` and `…/ignore`;
- `POST /calls/api/appointments/{id}/retry-sms`;
- `POST /calls/api/calls/clear`;
- `POST /calls/api/devices/{serial}/mode`;
- `POST /calls/api/contacts`, `PUT /calls/api/contacts/{id}`, `POST /calls/api/contacts/batch`
  (`POST /calls/api/contacts/import` needs an `X-Filename` header instead);
- `POST /calls/api/errands`, `…/{id}/cancel`, `…/{id}/retry`, `…/{id}/say`.

Where a route allows an empty body, it counts as `{}`. This is the second line
of defence. The first is setting a password (`CALLS_UI_TOKEN`) whenever your
network can reach the console.

## Calls

| Method and path | What |
|---|---|
| `GET /calls/api/list` | Call records, newest first. Filter with the query parameters listed below the table. Answer: `{ok, calls, total, limit, offset, tasks_by_call, contacts_by_number}`. |
| `GET /calls/api/call/{id}` | One call: `{ok, call, notes, contact, calendar_link, calendar_links, scam_names}`. The fields are explained below the table. |
| `DELETE /calls/api/call/{id}` | Deletes a finished call record, its WAV files, `_notes.json` and associated tasks: `{ok, id, tasks_deleted}`. HTTP 409 `call_in_progress` if the call is still ringing or active. |
| `POST /calls/api/calls/clear` | Deletes all finished call records, their WAV files, `_notes.json` and associated tasks while keeping any call in progress: `{ok, deleted, tasks_deleted}`. |
| `POST /calls/api/call/{id}/scam-feedback` | Your mark for a call: JSON `{"verdict": "scam" \| "sales" \| "safe"}`, optionally with a built-in `typology`. Answer `{ok, call, number}`; HTTP 400 `invalid_verdict` / `invalid_typology`. |
| `GET /calls/api/call/{id}/event.ics` | The calendar suggestion as an iCalendar file; HTTP 404 `no_calendar` when the call has none. Opens with the console password or with `?k=<key>`. |
| `GET /calls/api/audio/{id}/listen` | The call as you would have heard it (caller and AI mixed), WAV, with range requests for seeking (`?download=1` adds `Content-Disposition: attachment`). |
| `GET /calls/api/audio/{id}/remote` · `/tts` · `/vm` · `/audio/{id}` | Caller only · AI only · the voicemail of the older menu mode · the raw mixed recording. HTTP 404 `no_audio` when that file does not exist. |
| `GET /calls/api/live` | Calls in progress and the phones connected: `{ok, count, calls, control_idle_clients, control_idle_serials, normal_mode_serials}`. `normal_mode_serials` are connected phones switched to a normal phone; the page does not count them as ready. |
| `GET /calls/api/stats` | `{ok, stats: {total, today, voicemails, blocked}}`. |
| `GET /calls/api/export.csv` | The filtered list as CSV: same query as `list`, up to 2000 rows (`limit` can lower that). The headers `X-Calls-Export-Total` and `X-Calls-Export-Rows` say how many matched and how many were written. |

Details for these routes:

- `GET /calls/api/list` query: `limit` (up to 500; leave it out for the whole list), `offset`, `q` (free text over number, phone, events, transcript and summary), `caller`, `serial`, `status`, `has_transcript`, `has_voicemail`.
- `tasks_by_call` is `{<call id>: {id, status, summary}}`, the newest task of each call on the page.
- `contacts_by_number` is `{<caller_id>: {id, name}}` for the callers on the page who are contacts.
- `contact` (in `call`) is `{id, name, note}` or `null`.
- `calendar_links` = `{google, outlook, office365, yahoo}`: each web calendar's "new event" form, filled in with the call's calendar suggestion. `calendar_link` is the Google one, kept for older clients.
- `scam_names`: the judged kind of call's name in each console language, or `null`.
- A scam-feedback mark is kept on the record (`scam_feedback`) and, unless the number is hidden, for the number: its next calls are judged with it.
- `?k=<key>` on an `event.ics` link is an HMAC key that a notification link carries for this one file, in place of the password.

### The call record

Each call record (`call`, and every entry of `calls`) carries, among others:

| Field | Meaning |
|---|---|
| `id` | The record's id, used by every `/call/{id}` and `/audio/{id}` route. |
| `serial` | The phone that took (or placed) the call. |
| `caller_id` | The other side's number as the phone reported it; empty for a hidden number. |
| `started_at` / `ended_at` | RFC 3339, UTC. |
| `duration_ms` | Length of the call. |
| `status` | `ringing`, `active`, `ended`. Records from older versions may also show `blocked_spam`. |
| `events` | What happened, in order. The values are listed below the table. |
| `scam` | Set when the call was judged sales or scam: `{kind, typology, confidence, source, released}`. The values are explained below the table. |
| `scam_feedback` | Your mark from the call's page: `scam`, `sales` or `safe`. |
| `summary` | The summary text, once written. |
| `appointment` | When the caller set a time with the AI: the appointment as the call left it (see [Appointments and tasks](#appointments-and-tasks)). Later changes (your decision, the text and call-back results) go to `notes.appointment`. |
| `sim_country`, `sim_sub_id` | The SIM the call came in on, when the phone said. |
| `audio_path`, `remote_audio_path`, `tts_audio_path`, `voicemail_path` | Which recordings exist (paths inside the data folder). |

`events` values include:

- `active`
- `persona_stall_spam`: judged a sales or scam call
- `persona_scam_detected`: judged so with *Keep them talking* off
- `persona_stall_released`: a later look let the caller go
- `reject_list`: a number on the Blacklist
- `call_waiting_rejected`: a second call declined during a call
- `ai_callback`: the AI calling back about an appointment
- `ai_errand`: an AI errand
- `persona_ai_fail_hangup`
- `ended`

`scam` values:

- `kind` is `scam` or `sales`; `typology` is the built-in kind of call.
- `source` is `review` (the second look), `hard_words` (it did not answer and scam-only words were said) or `keywords` (it kept failing).
- `released`: a later look let the caller go.

### Notes, transcript and translations

`notes` is `null` until the summary is written, then:

| Field | Meaning |
|---|---|
| `summary` | The summary in the notification language. |
| `dialogue` | The transcript, one turn per line. Each line starts with a speaker label in Bageo's language, and stays in the language it was spoken in. |
| `dialogue_translations` | Present only when something was translated: one entry per line of `dialogue` (same index). The entry is the translation into the notification language, or `null` for a line already in your language, too short, or not translated. The console shows it under the original line. |
| `recording_text` | A speech-to-text pass over the caller's track, kept as a backup (empty for sales and scam calls the AI stalled). |
| `end_hint`, `source`, `updated_at` | How the call ended, what wrote the notes, when. |
| `calendar` | When the call fixed a date: `{title, start, end, all_day}` (local time, `YYYY-MM-DDTHH:MM`). |
| `appointment` | The appointment as it stands now. |

Speaker labels in `dialogue` (caller / AI):

- English: `Caller: ` / `AI: `
- Traditional Chinese: `對方：` / `接聽：`
- Simplified Chinese: `对方：` / `接听：`
- Japanese: `相手：` / `AI：`
- Korean: `상대: ` / `AI: `
- Spanish: `Llamante: ` / `IA: `
- French: `Appelant : ` / `IA : `
- German: `Anrufer: ` / `KI: `
- Portuguese: `Chamador: ` / `IA: `

Records saved by earlier versions use `對方：` / `接聽：` in every language.

```json
{
  "dialogue": "Caller: Hola, ¿está Alex?\nAI: Sí, soy yo. ¿Quién habla?",
  "dialogue_translations": ["Hello, is Alex there?", "Yes, speaking. Who's calling?"]
}
```

When the caller is one of your contacts, their name shows on the call list
(`contacts_by_number`), on the call (`contact`), after the number in the title
of the summary notification, and in the AI's prompt on ordinary calls.

## Settings

| Method and path | What |
|---|---|
| `GET /calls/api/settings` | `{ok, settings, storage_path, console_url, console_ip_url}`, plus read-only helpers. Details are below the table. |
| `PUT /calls/api/settings` | A JSON object with only the fields to change. Answers like `GET`. Takes effect on the next call, without a restart. |

Details for `GET /calls/api/settings`:

- `console_url` is the console's address in use (the one you typed, or the automatic one; empty when only this computer can open it). `console_ip_url` is the same with the LAN IP instead of a `.local` name.
- Secrets come back masked (`"abc…wxyz"`) with a `<name>_set` flag.
- Notification channels come back with empty `url` / `token` plus `url_hint`, `token_hint`, `url_set`, `token_set`.
- Read-only helpers: `default_prompt`, `voices`, `ambience_kinds`, `languages`, `countries` (ISO codes for *Main language for each SIM*), `seen_sim_countries` (SIM countries the phones reported since Bageo started), `notify_kinds`, `sample_lines`, `summary_ready`.

Fields worth knowing (all of them are in `RuntimeSettings` in
[`settings_store.rs`](../../crates/agent/src/settings_store.rs) and on the
[Settings reference](SETTINGS.md)):

| Field | Values |
|---|---|
| `owner_name` | Your name, used for `{owner}`. |
| `owner_aliases` | Up to 10 other names callers use for you, each up to 40 characters. Blanks, repeats and your own `owner_name` are dropped. |
| `answer_style` | `assistant` / `persona` (the AI answers as you). |
| `errand_identity` | `assistant` (default) / `owner`: who the AI says it is on an AI errand. Anything else is stored as `assistant`. |
| `summary_provider` | `gemini` (default) / `custom`. `custom` uses `summary_api_base`, `summary_model` (required with a base URL) and `summary_api_key` (the key of that service). |
| `calendar_app` | `google`, `outlook`, `office365`, `apple`, `yahoo`, `ics`, or empty (not chosen yet). Anything else is stored as empty. |
| `appointment_mode` | `off` (default) / `confirm`. An older `auto` reads as `confirm`. |
| `appointment_sms` | `true` / `false`: text the caller your decision. |
| `appointment_reply` | `auto` (default), `sms` or `call`. |
| `reject_numbers` | Older form of the Blacklist, still taken by `PUT`: a list of `{number, reason}`. The numbers join the Blacklist (an ordinary contact moves there; one on the Whitelist or Stall list stays) and are not stored in the settings. Reads back what an older version saved, which counts no more once `reject_numbers_moved` is `true`. |
| `reject_mode` | How Blacklist calls are turned away: `decline` (default: refuse while it rings) or `announce` (answer and play the announcement). |
| `whitelist_mode` | `off` (default: AI answers normally), `direct` (decline to trigger carrier call forwarding and alert), or `emergency` (AI answers and gives out `whitelist_forward_number` if the caller has an urgent matter). |
| `whitelist_forward_number` | Your real forwarding phone number used by Whitelist direct/emergency transfer. |
| `ui_password` | Password for external console/API access (masked on read; `"ui_password_clear": true` clears it). |
| `country_code` | Digits only (`886`, `1`). Not shown in the console; a value here wins over `COUNTRY_CODE`. |
| `gemini_thinking_level` | `low` (default) / `medium` / `high`, for thinking models only. |

Rules for `PUT`:

- A secret field (`gemini_api_key`, `summary_api_key`) that is absent or `""`
  keeps its stored value; `"<name>_clear": true` empties it.
- `notify_channels` is replaced as a whole. In a row
  `{id, kind, name, enabled, url, token, target}`, an empty `url` or `token` keeps
  the stored value of the row with the same `id` and `kind`. A row without `id`
  is new; rows you leave out are deleted.
- Keys an older version stored and this one no longer has are ignored and
  dropped at the next save: the Google Calendar credentials (`gcal_*`),
  `calendar_auto_add`, `intl_carrier_prompt`, and other settings removed in
  1.0.
- A save that fails on disk answers HTTP 500 `save_failed`.
- Values and setting names of older versions (an older `summary_provider`, the summary key's former name) are converted once when the settings load.

```bash
curl -s -X PUT "http://127.0.0.1:8787/calls/api/settings" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"language": "ja", "owner_aliases": ["Alex", "Al"], "errand_identity": "owner",
       "reject_numbers": [{"number": "0800*", "reason": "telemarketing"}]}'
```

## Notifications

| Method and path | What |
|---|---|
| `POST /calls/api/notify/test` | Body `{"channel": {kind, url, token, target, id?}}`. Sends the test message to that one channel (stored secrets are filled in by `id`). |
| `POST /calls/api/notify/send` | Body `{"title": "...", "body": "..."}`. Sends your message to every enabled channel (an empty `title` becomes "Bageo"; an empty `body` is HTTP 400 `empty_body`). `{ok: true}` when at least one channel accepted it. For a text message, add `"kind": "sms"` with `"from"` (sender), `"text"`, `"to"` (the number that received it), `"serial"` and `"device"` (the phone), `"received_ms"` (when it arrived, Unix milliseconds) and `"ref"` (your own key for the message), `"sub_id"` (the SIM that received it): Bageo keeps it for the console's **SMS** page, stores the same `ref` only once, and the desktop status app shows a notification. The [SMS forwarder](../../tools/sms_forward/README.md) uses this route. |
| `GET /calls/api/notify/recent?after=<id>` | The messages sent through `notify/send`, oldest first: `{ok, last_id, events: [{id, at_ms, kind, title, from, text}]}`. `events` holds only those with an `id` above `after`; without a number for `after` it is empty and `last_id` tells the client where to start. Only the latest 50 are kept, in memory only, so a restart empties the list. |
| `GET /calls/api/sms?q=&before=<id>&limit=<n>` | The stored text messages, newest first: `{ok, total, retention_days, has_more, messages: [{id, at_ms, dir, from, from_name, to, text, serial, device, status}]}`. `q` searches sender, contact name and text; `before` gives the messages older than that `id`; `limit` is 1 to 200 (default 50). `total` counts every stored message, whatever the search. Messages older than the *Days to keep records* setting are deleted, and at most 5000 are kept. |
| `DELETE /calls/api/sms/{id}` | Deletes one text message: `{ok: true}`, or HTTP 404 `not_found`. |
| `POST /calls/api/sms/clear` | Deletes every stored text message: `{ok: true, removed: N}`. |
| `GET /calls/api/sms/threads?q=` | The conversations, one per number, newest first: `{ok, total, retention_days, threads: [{key, number, name, last_at, last_text, last_dir, last_status, count, last_in_at, can_reply}], lines: [{serial, sub_id, device, number, carrier, slot, can_send}]}`. `0912…` and `+886912…` are one conversation (numbers are read in the country of the SIM that got them). `q` searches names, numbers and every message. `can_reply` is false for a sender name such as "BankInfo". `lines` are the SIMs texts can go out from; `can_send` means the phone is connected and its app sends texts. |
| `GET /calls/api/sms/thread?number=<number or key>&before=<id>&limit=<n>` | One conversation, oldest first: `{ok, key, number, name, can_reply, has_more, reply, messages: [{id, at_ms, dir, from, to, text, serial, sub_id, device, status, error}]}`. `dir` is `in` or `out`; a sent message has `status` `sending`, `sent`, `failed` (with `error`, the phone's code) or `timeout`. `reply` (`{serial, sub_id}` or `null`) is the line a reply uses by default: the phone and SIM that received the newest message, else the first line that can send. A number without a conversation gives an empty one. |
| `POST /calls/api/sms/send` | Body `{"to", "text", "serial", "sub_id"}` (one of `lines`; `text` 1 to 500 characters). Has that phone send the text and waits up to 30 seconds for its answer: `{ok, message}`, `ok` when it was sent. HTTP 400 `bad_number`, `bad_line` or `bad_text`. The text is kept in the conversation and is not sent to notification channels. |
| `POST /calls/api/sms/{id}/retry` | Sends a text that `failed` or `timeout` again, on the same line: answer as for `send`. HTTP 409 `not_retryable` for any other message. |
| `DELETE /calls/api/sms/thread?number=<number or key>` | Deletes one conversation: `{ok: true, removed: N}`. |
| `GET /calls/api/quick-action` · `POST /calls/api/quick-action` | Signed one-tap action links included in notifications across all 32 channels (`?act=confirm\|ignore\|block\|stall\|white&target=<id_or_number>&sig=<hmac>`). Opening a link (`GET`) only shows a confirmation page; the action runs when you press the button on that page (`POST`). A link stops working after 7 days. Verified with HMAC-SHA256 so no console login is needed; localized in all 9 languages. |
| `POST /calls/api/control/inbound` | Universal chat command webhook, for example `{"text": "calls 5", "user": "owner", "channel": "line"}`. Runs a chat command and returns `{ok, text}` in your notification language. |
| `GET /calls/api/auth-status` · `POST /calls/api/initial-password` | Public status check (`{ok, password_set, authenticated, prompt_initial, language}`) and first-time password setup (`{"password": "..."}` or `{"skip": true}`, allowed before any password/token is set). |

Details for `POST /calls/api/notify/test`:

- Answers HTTP 200 `{ok: true}`, 400 `unknown_channel`, or 502 `{ok: false, code, field, kind, error}`.
- `code` says what went wrong: `missing_field`, `bad_url`, `bad_value`, `auth_failed`, `service_not_found`, `http_error`, `service_error`, `network_error`, `send_failed`, `bot_problem` (Discord turned the bot away), `room_encrypted` (a Matrix room with encryption), `blocked_address` (an address Bageo does not send to).
- `field` is `url`, `token` or `target` (or empty); `error` is the service's answer.

`POST /calls/api/control/inbound` works with Slack, Discord, LINE, WeCom, DingTalk, Feishu/Lark, Mattermost, Rocket.Chat, Zulip, Matrix, Nextcloud Talk, Synology Chat, Home Assistant, n8n, or curl.

Chat commands accepted by `POST /calls/api/control/inbound`: `help`, `status`, `calls`, `call`, `tasks`, `confirm`, `ignore`, `errand`, `errands`, `cancel`, `say`, `answer`, `mode`, `contacts`, `block`, `stall`, `white`, `unblock` (see [Chat commands](CHAT_COMMANDS.md#commands)).

With the MCP token ([MCP](MCP.md)), this API does only what `mcp_calls` allows. While it is `off` (the default), the token can look things up but cannot change anything through a command.

The JSON your own webhook receives, and every event type, is described in
[Notifications](NOTIFICATIONS.md#webhook-json).

## Voice and sound previews

`GET /calls/api/preview` returns a WAV of a few phone lines in a voice, with a
background sound under them. Every query parameter falls back to the saved
setting:

| Query | Meaning |
|---|---|
| `voice` | A voice name, or `none` for the background alone. |
| `ambience` | `off` or a background kind. |
| `level` | 1–3. |
| `regen=true` | Generate again instead of using the stored sample. |
| `clip=reject` | The blocked-number announcement in that voice. |
| `lang` | `ja`, `zh-TW`, … |

All 30 built-in voices come recorded in all 9 languages, both the preview and
the announcement (1,080 clips in `assets/voices/`), so a preview needs no
Gemini session. Only `regen=true` or a voice without a recording opens one.

Errors: 400 `unknown_voice` / `unknown_background`, 404 `background_missing`
(the sound file is not loaded), 502 `preview_failed`.

## Appointments and tasks

A caller wants to come by Saturday at 3. With `appointment_mode` set to
`confirm` (see [Settings](SETTINGS.md#summary--calendar)), the AI takes down the
time without promising anything. The call then leaves a task (`intent`
`appointment`) for you to accept or decline.

The same `appointment` object sits on the call record, in its notes and on the task:

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
| `title`, `caller_name` | What it is about, and how the caller introduced themselves (in their own words). |
| `status` | `pending` (waiting for you), `confirmed`, `declined`; `booked` only on records from older versions. |
| `reply` | How the caller was told: `sms`, `call` or `none`. |
| `sms` | `sent`, or why the text did not go. The values are listed below the table. |
| `call` | The AI call-back. The values are listed below the table. |
| `call_lang` | The language spoken on the call (`zh-TW`, `en`, …). The AI call-back speaks it and gives your answer once, after the callee has picked up and spoken. |
| `tg_card` | `{chat_id, message_id}` of the Telegram card sent for it; its buttons are taken off when you decide elsewhere. |
| `tag` | A stable id for the appointment. |

Empty fields are left out.

`sms` values when the text did not go:

- `offline`: the phone is not connected.
- `old_app`: its app cannot send texts.
- `timeout`.
- `busy`: the same text is already being sent.
- `no_number`: a number that takes no texts.
- The phone's own code: `no_permission`, `bad_request`, `no_service`, `radio_off`, `generic_failure`, `exception`.

The console and the Telegram reply put each in words.

`call` values (the AI call-back):

- `queued` (behind an earlier call-back to the same number), `dialing`, `connected` (the phone dialed; the AI waits for the callee to speak), `answered` (the callee was told), `no_answer` (nobody spoke within 70 s), `failed`, `done` (ended, outcome unknown).
- Not placed: `no_phone`, `send_failed`, `no_number`, `not_callable` (abroad, toll-free, premium-rate or a short code).
- The phone's reports also go into the task's `events`: `callback_place_call_ack`, `callback_place_call_failed`, `callback_outbound_ready`, `callback_done`, `callback_failed`.

Records without `call_lang` use the `language` setting.

| Method and path | What |
|---|---|
| `GET /calls/api/tasks` | Tasks, newest first (up to 200): `{ok, tasks}`. An appointment task also carries extra fields, listed below the table. |
| `GET /calls/api/tasks/{id}/event.ics` | An appointment as an iCalendar file (Apple Calendar and other apps). Opens with the console password or a link's `?k=<HMAC key>`; HTTP 404 `no_calendar` for a task without an appointment. |
| `POST /calls/api/tasks/{id}/confirm` | **Accept**, the same as the button in the console or on the Telegram card. Body: empty, `{}` or `{"reply": "sms" \| "call" \| "none"}`. Answer `{ok, task, message}`. Details are below the table. |
| `POST /calls/api/tasks/{id}/ignore` | **Decline**: the caller is told the same way, with a polite no. Same body, answer and rules. |
| `POST /calls/api/appointments/{id}/retry-sms` | Text the caller of a decided appointment again when the text did not go out. Body: empty, `{}` or `{"serial": "<phone>"}` (another connected phone; none means the phone that took the call). Answer `{ok, sms, message}`. Details are below the table. |

Details for these routes:

- `GET /calls/api/tasks`: an appointment task also carries `calendar_links` (`{google, outlook, office365, yahoo}`) and `contact_name` when the caller is a contact.
- `reply` = `{sms, call, default}`: whether a text or a call-back can reach this caller, and the way *Default reply* picks.
- `sms_route` = `{serial, label}`: the phone the text goes out from when you pick none (the phone that took the call). `label` is its name in `device_labels`, else its id.
- The console's *To do* list is the tasks whose `appointment.status` is `pending`.
- `confirm`: without `reply`, *Default reply* decides; a way that cannot reach this caller tells nobody.
- `confirm` answers `message`: what happened, in words, as the Telegram bot replies it, with the *Add to calendar* link for your `calendar_app`. If none is chosen yet, the link opens the call's page in the console, where *Add to calendar* asks which calendar. With no console address, there is no link.
- `confirm`: a text may take up to 90 seconds; a call-back is handed to the phone (follow it in `appointment.call`).
- `retry-sms`: `ok` when the text was sent, and `sms` is the new code.
- `retry-sms` errors: HTTP 409 `sms_not_retryable` (with the current `sms`) when it is still pending, already sent or never to be sent (`no_number`); 404 `task_not_found`; 400 `invalid_serial` / `bad_request`.

Rules for decisions:

- **Only the first decision counts.** A second confirm or decline, including one
  sent while the first is still running or a press on the Telegram card,
  answers "already handled" and changes nothing.
- When a Telegram card went out for the appointment, its buttons are taken off
  and `message` is posted in that chat (best effort; the decision stands
  either way).
- Texts and call-backs go out from the SIM the call came in on (`sub_id`). A
  text gives only the time and its weekday, never what the appointment is
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
 "message": "Appointment accepted\nSat, Nov 14, 3:00 PM–4:00 PM House viewing\nThe AI is calling 0912345678 back\nAdd it to your calendar: https://calendar.google.com/…"}
```

## AI errands

An AI errand is an outgoing call the AI makes for you. You give it a number
(or a contact) and what to get done: book a table, fix a time with a friend,
ask a shop something. A phone in AI mode dials, the AI talks, and you get the
result. Here is how a restaurant booking plays out through the API:

1. `POST /calls/api/errands` with the number and "Book a table for 4 tomorrow
   at 7 pm under Wang. 7:30 to 8 is fine too." The errand is `dialing`.
2. Poll `GET …/errands/{id}/live` every few seconds for captions. The
   restaurant says 7 is full and offers 7:30 or 8:15.
3. Choices clearly inside your instructions the AI accepts on its own. When
   it is not sure, it tells them it will check, and a `question` with the
   options appears in the live view.
4. Answer with `POST …/errands/{id}/say`, or type an instruction ("tell them
   8 pm works too").
5. After the call the errand is `done` with a `result`, and you get one
   notification.

**During the call.** When the AI meets something your instructions do not
settle, it tells the other side it will check and asks you (the `ask_owner`
function).

- If you are watching the errand (something read its live view in the last 6 seconds), the question shows there for up to 40 seconds.
- If nobody is watching, or you do not answer in time, the AI asks which options would work, says you will confirm and call back, and ends the call politely.
- While it waits it settles nothing about the question; if the other side says goodbye first, the question ends there.
- What you type during the call reaches the AI as an instruction.

**Afterwards.** The summary model reads the call against the errand, and you
get one notification (title "AI errand call: <name or number>", in the
notification language; webhook event `ai_errand`). If the AI agreed to
something on a question you did not answer, the result is never `done`: it is
`need_owner`, telling you to confirm or call back to change it.

**Who calls.** `errand_identity` (setting: `assistant`, your assistant, by
default, or `owner`, as you); each errand may pick its own. Either way the AI
admits it is an AI when sincerely asked, gives out no personal details beyond
the errand, and agrees to no payment, deposit or transfer.

**Which numbers.**

- Never an emergency number, a short code (7 digits or fewer) or a premium-rate number (for example Taiwan 020x such as 0204 and 0209, +1 900 / 976, Japan 0990, UK 09…). Toll-free numbers are fine.
- A number without a plus is read as one of your country (`country_code`).
- A number that starts with an international call prefix (`00`, Taiwan's `01x` and `00x`, North America's `011`, Japan's `010`) is `not_allowed`: write numbers abroad with `+`.
- A Taiwan number starting with 1 after the trunk 0 is also `not_allowed`.

**Which phone and SIM.**

- The call goes out from a connected phone in AI mode that has a SIM of the number's country.
- Bageo learns each phone's SIMs (subscription id and country) from the calls it reports (`sim_sub_id` and `sim_country` in `incoming` and the media `hello`), and from an optional `sims: [{sub_id, country}]` list in the control `hello` (the 1.0 app does not send one yet). They are kept in `phone_sims.json`.
- A phone whose SIMs Bageo has never heard about takes no errand (`sim_unknown`): call that phone once first.
- The phone gets the number in E.164 form with the SIM to use (`sub_id`).
- If the call's own media `hello` shows another SIM, or none, Bageo hangs up before the AI speaks (`failed`, `call_code` `wrong_sim`, you are told) and forgets that SIM on that phone until a call reports it again.

**Queues and limits.**

| Situation | What happens |
|---|---|
| The phone is on a call | The errand waits (`queued`) and fails after 10 minutes (`phone_busy`). |
| Another call to the same number | One call per number at a time, shared with appointment call-backs. Waiting behind it fails after 25 minutes (`number_busy`). |
| Length | At most 10 minutes; the AI wraps up at 9:30. |
| A voice menu | The AI waits up to 90 seconds for a person; it cannot press keys (`keypad_menu`). |
| A carrier recording ("the number you dialed is not in service") | It hangs up at once (`carrier_message`). |
| No answer or failure | No automatic retries: **Try again** is yours. With `redial` on, see the next row. |
| **Redial until answered** (`redial`) | No answer, a busy line, the phone off or voicemail (no message left): the errand waits (`queued`, `call_code` `redial_wait`, the time in `redial_at`) and is dialed again a minute later, up to 30 tries in all. It stops once someone picks up, after 30 tries (`no_answer`), or at a number that does not work, a voice menu or a phone problem. One notification, at the end. |
| You cancel while it dials | The phone and the number are free for the next call at once; if the call still connects, it is hung up before the AI speaks. |

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
| `call_code` | Where the dial stands: `dialing`, `queued`, `answered`, `no_answer`, `failed`, `phone_busy`, `number_busy`, `no_phone`, `send_failed`, `keypad_menu`, `carrier_message`, `wrong_sim`, `agent_restarted`, `cancelled`, `redial_wait`, `voicemail`. |
| `attempts` | 1, plus one per **Try again** and per redial. |
| `redial` | **Redial until answered** is on. Left out when off. |
| `redial_at` | When the next try is dialed (while `call_code` is `redial_wait`). Left out otherwise. |
| `redial_start` | The try the current run of redials started at (**Try again** starts a new run of 30). Left out when 0. |
| `record_id` | The call record of this try (`GET /calls/api/call/{id}`: transcript and recording). |
| `follow_up_of` | An earlier errand this call follows up; its last 20 transcript lines go into the AI's script. |
| `questions` | `[{id, text, options, answer, answered_by, asked_at}]`: what the AI asked you, with up to 4 options. |
| `result` | `null` or `{outcome, summary, details, follow_up, options, event, unconfirmed}`. Details are below the table. |
| `history` | Earlier tries, `[{attempt, status, result, record_id, ended_at, questions}]`. |

- `answered_by` (in `questions`): `owner` (you answered), `timeout` (you watched but did not answer in time), `absent` (nobody was watching); `null` while it waits.
- `outcome` (in `result`): `done`, `partial`, `not_done`, `need_owner` (choices in `options`, up to 8), `no_answer`, `voicemail`, `failed`.
- `event`: `null` or `{start, end, title}` (local time with its offset; `end` left out when not agreed).
- `unconfirmed`: the times, dates, numbers or names in `details` or `event` the other side never clearly confirmed when the AI read them back (up to 8); left out when there are none.

Errands are kept in `errands.json` in the data folder (the newest 500). An
errand still under way when Bageo restarts becomes `failed` (`call_code`
`agent_restarted`). A redialing errand goes on instead, unless the other side
had already picked up.

### Errand routes

| Method and path | What |
|---|---|
| `GET /calls/api/errands` | `{ok, errands}`, newest first, at most 200. |
| `POST /calls/api/errands` | Start one. Body `{number, name, contact_id, task, identity, serial, follow_up_of, answers, redial}` (`redial`: `true` for **Redial until answered**, default `false`). Answer `{ok, errand}` with status `dialing`, or `queued` when the phone or the number is busy. |
| `POST /calls/api/errands/preflight` | What the errand still lacks before it dials. Body `{task, lang}`; answer `{ok, kind, missing, ladder, source, ms}`. Nothing is dialed. Details are below the table. |
| `POST /calls/api/errands/{id}/cancel` | Waiting: dropped. Dialing or on the call: hung up. An errand already over comes back unchanged. `{ok, errand}`. |
| `POST /calls/api/errands/{id}/retry` | The same errand once more (`attempts` + 1, the last try moved to `history`). `{ok, errand}`. Still under way (or its last call not ended yet): 400 `busy`; the phone codes as for a new errand. |
| `DELETE /calls/api/errands/{id}` | Removes it from the list (its call records stay). `{ok}`; dialing or on the call: 400 `busy`. |
| `GET /calls/api/errands/{id}/live?after=<seq>` | `{status, lines, question, watching}`: the live captions and the question waiting for you. Details are below the table. |
| `POST /calls/api/errands/{id}/say` | Body `{text, question_id}` (up to 300 characters). With `question_id`: the answer to that question. Without it: the answer to the question waiting, or else an instruction for the AI ("tell them 8 pm works too"). `{ok}`. |

Details for these routes:

- Creating an errand requires `task`, and a number (or a contact or an earlier errand whose number is used). Empty `identity` means `errand_identity`; empty `serial` means any suitable phone.
- `answers` is optional: what you added after the questions, as `{key: text}`. The keys are the ones `preflight` returns, plus `note` for free text; up to 80 characters each (`note`: 120). Unknown keys are dropped. They reach the AI as a short block in the call's script.
- `lines`: the captions after `seq`, `[{seq, who, text, at}]`, with `who` `ai`, `callee`, `owner` or `system`. The newest 400 are kept; a line is complete after 1.2 s without more words.
- `question`: `null` or `{id, text, options, asked_at, expires_at}`, the question waiting for you.
- `watching`: whether you count as watching. Each read marks you as watching for 6 seconds, so poll every few seconds while you follow the call.
- `preflight` takes `task` (required, up to 1,000 characters) and `lang` (optional, such as `en` or `zh-TW`: the language of the questions; empty means the notification language). It answers:
  - `kind`: `restaurant`, `appointment`, `order`, `change` or `contact`.
  - `missing`: at most 3 `{key, question}`, the most important first; empty when nothing is missing. The keys by kind: `restaurant` `time`, `party`, `phone`, `name`; `appointment` `time`, `item`, `contact`, `backup`; `order` `spec`, `pickup`, `substitute`; `change` `orig_time`, `contact`; `contact` none.
  - `ladder`: the times you would accept, best first (`["19:30", "20:30"]`), at most 4. Empty unless the task names two or more times.
  - `source` and `ms`: `local`, `model` or `timeout`, and how long it took in milliseconds.
- A request Bageo can read itself is answered locally in a few milliseconds. One in words it cannot read (another script, long text full of numbers) asks a small model (`gemini-flash-lite-latest`) through the Gemini key, for at most 0.9 seconds. When no answer comes in time (`source` `timeout`) or there is no key, the call goes ahead without questions.
- In a chat, the confirmation card lists the same questions. Replying to the card with the answers saves your reply as `answers.note` and the AI calls with it; replying "cancel" dials nothing.

Error codes (HTTP 400 `{ok: false, error, code}`, the same word in both):

| Code | Route | Meaning |
|---|---|---|
| `no_number` | create | No number, and no contact or earlier errand to take one from. |
| `bad_number` | create | Not a phone number (letters, a plus that does not lead, more than 15 digits, too short for its country). |
| `not_allowed` | create | Emergency, short code, premium rate, or an international prefix instead of `+`. |
| `no_task` | create, preflight | `task` empty. |
| `task_too_long` | create, preflight | `task` over 1,000 characters. |
| `no_phone` | create, retry | No connected phone in AI mode (or the `serial` asked for is not one). |
| `sim_unknown` | create, retry | No phone fits, and at least one has SIMs Bageo has not learned yet: call it once. |
| `no_sim_for_country` | create, retry | No phone has a SIM of the number's country. |
| `not_found` | create | `contact_id` or `follow_up_of` does not exist (400 here). |
| `busy` | retry, delete | The errand is still dialing or on the call. |
| `not_on_call` | say | The call is not (or no longer) running. |
| `empty_text` / `text_too_long` | say | Nothing to say / over 300 characters. |
| `question_closed` | say | That question was answered, timed out, or is not the one waiting. |
| `bad_request` | all `POST` | No `Content-Type: application/json`, or the body is not a JSON object. |

On every `…/errands/{id}…` path, an unknown or malformed errand id is HTTP 404
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

Contacts put names on the numbers that call. They are kept in `contacts.json`
in the call records' folder (`CALLS_DATA_DIR`).

A number matches however it is written: `+886912345678`, `0912-345-678` and
`886912345678` are one number. A number without a country code is read in your
country (`country_code`) or, for a call, in the country of the SIM it came in on.

Contacts also feed AI errands (pick a contact instead of typing a number) and
the sales check: a saved contact talking about money is not treated as a sales
or scam call.

A contact is `{id, name, phones: [...], note, kind, source, updated_at}`, with up to 10
numbers, a name up to 80 characters and a note up to 500. A number belongs to one
contact only, and the book holds up to 10,000 contacts (`too_many_contacts`).

`kind` is one of:

- `contact` (default).
- `whitelist`: family and other people you trust, used with `whitelist_mode`.
- `blacklist`: turned away at once, without the AI (`reject_mode`). Only a Blacklist number may end with `*` (`0800*`, at least three digits not counting leading zeros, so not `080*` or `0000*`); it covers every number that starts that way. A `*` anywhere else: 400 `invalid_contact` with `field` `prefix`.
- `stall`: the AI starts stalling at once (anti-scam and anti-harassment).

`source` is `manual`, `vcf`, `csv`, `text` or `ai` (read by Gemini).

| Method and path | What |
|---|---|
| `GET /calls/api/contacts` | Query `q` (name, note, or a number written any way), `kind` (`contact` \| `whitelist` \| `blacklist` \| `stall`), `limit`, `offset`. `{ok, contacts, total, offset, count}`: the matches (`total` of them) and how many contacts there are in all (`count`). |
| `POST /calls/api/contacts` | Body `{name, phones: [...], note, kind?}`: `{ok, contact}`. A number that is another contact's: HTTP 409 `contact_exists` with `contact: {id, name}`. No name or no number: 400 `invalid_contact` with the `field` at fault. |
| `POST /calls/api/contacts/block` | Body `{number, reason?}`: puts the number (or a `*` prefix) on the Blacklist. The contact that has it moves there (its note gets `reason` when it has none); else a new contact named after `reason`, or the number. `{ok, contact, already}`; `already`: it was on the Blacklist before. |
| `PUT /calls/api/contacts/{id}` | The same body; replaces the name, numbers, note and kind. 404 `contact_not_found`, 409 and 400 as above. |
| `DELETE /calls/api/contacts/{id}` | `{ok}`. |
| `POST /calls/api/contacts/import` | The file itself as the body (up to 10 MB), its name URL-encoded in `X-Filename` (missing: 400 `bad_request`). Reads it and **saves nothing** (a preview): `{ok, file, format, ai, entries: [{name, phones, note, source, duplicate}], skipped}`. Details are below the table. |
| `POST /calls/api/contacts/batch` | Body `{contacts: [{name, phones, note, source}], duplicates: "merge" \| "skip"}`: the entries you keep from an import. One that repeats a saved contact is merged into it (its new numbers and note added; the default) or skipped. `{ok, added, merged, skipped, count}`. |
| `GET /calls/api/contacts/export.vcf` | Every contact as a vCard 3.0 file. |

Details for `POST /calls/api/contacts/import`:

- `format`: `vcf`, `csv`, `text`, `xlsx`, `docx`, `pdf` or `image`.
- `ai`: Gemini read it.
- `duplicate`: `null`, or the saved contact the entry repeats, `{id, name, by: "phone" \| "name"}`.
- `skipped`: entries without a name or a number.
- Errors: 400 `empty_file`, 413 `file_too_large` / `too_many_rows`, 415 `unsupported_file`, 400 `need_gemini_key`, 502 `ai_failed` (Gemini's answer in `error`).

Bageo reads these itself:

- vCard 2.1, 3.0 and 4.0 (quoted-printable, any `CHARSET`).
- CSV and TSV with a header row (Google Contacts, Outlook, or columns named like *Name*, *Phone*, 姓名, 電話) or without one, in UTF-8, UTF-16 or your computer's encoding (Big5, GBK, Shift_JIS, EUC-KR, Windows-1252).
- Plain text with a name and a number on each line.
- The tables in `.xlsx` and `.docx` files.

PDF files, pictures (a photo of a printed list, a screenshot of a phone book) and files in which nothing was found go to Gemini's vision model (`gemini-flash-lite-latest`, with the Gemini API key from the settings).

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
| `GET /calls/api/devices` | `{ok, usb: {enabled, adb_found, adb_path, error, phones: [{serial, state, model, reverse, app_installed}]}, connected: [...], labels: {<phone id>: <name>}, calls_in_progress, phones: [...]}`. Details are below the table. |
| `POST /calls/api/devices/{serial}/mode` | Body `{"mode": "ai" \| "normal"}`. Asks the phone to switch and waits up to 15 seconds for its answer. Answers `{ok, mode, error_code}` (a 400 adds `error`, a description). |
| `DELETE /calls/api/devices/{serial}` | Removes an offline phone from the phone list, saved device labels and recorded SIMs while keeping past call records: `{ok, serial}`. HTTP 409 `device_online` / `device_in_call` if the phone is connected or in a call. |
| `PUT /calls/api/devices/{serial}/sim-number` | Body `{"sub_id": <int>, "number": "..."}`. Saves (or clears when empty) the phone number for a SIM on that phone so calls and AI errands show and pick by number: `{ok, serial, sims}`. |
| `GET /health` | A large flat JSON object for troubleshooting: version, language, what is configured, connected phones, calls in progress; no keys or tokens. Details are below the table. |

Details for these routes:

- Each entry of the top-level `phones` is a phone seen on its control connection since Bageo started: `{serial, connected, mode, supports_mode, model, app_version, in_call, last_seen_ms}`.
- `mode` is `ai` (the AI answers) or `normal` (an ordinary phone, see [Phone setup](PHONE.md#normal-phone-mode)).
- `supports_mode` is false for apps older than the switch, which always run as `ai`.
- `GET /health`: `control_idle_clients` / `control_idle_serials` count every connected phone; `normal_mode_serials` lists those switched to a normal phone; `local_time` is Bageo's clock in its `TZ`; `summary_ready` (and `summary_api_key_set`, the same value) says whether a summary service is set.
- `GET /health`: `setup_missing` lists what the console's "Ready?" list still needs: `ai_off`, `gemini_key`, `owner_name`, `notify`, `summary` (names only, never values). It is empty when everything is set.
- Without the console password (when one is set), `GET /health` returns only `ok`, `version`, `language`, `notify_language`, `active_call_count`, `control_idle_clients` and `setup_missing`, enough for monitoring.

After a mode switch, `mode` is the phone's mode (`null` when unknown) and
`error_code` is `null` on success, otherwise one of:

| `error_code` | Meaning |
|---|---|
| `offline` | The phone is not connected. |
| `unsupported` | Its app is older than the mode switch. |
| `in_call` | A call rings or is up; try after it. |
| `busy` | A switch is already running. |
| `no_root` | The Bageo module's root helper did not take the request: the module is not running. |
| `save_failed`, `dialer_failed`, `screening_failed`, `dnd_failed`, `volume_failed`, `failed` | The step that did not take effect. Switching again repeats every step. |
| `timeout` | Not confirmed yet, which is not the same as failed: the switch can restart the phone app, and it reports after it reconnects. Read `GET /calls/api/devices` a little later. |
| `bad_request` / `invalid_mode` | HTTP 400: not JSON, no `Content-Type: application/json`, no string `mode`, a serial that is not letters, digits, `-` and `_`; or an unknown `mode`. |

```bash
curl -s -X POST "http://127.0.0.1:8787/calls/api/devices/pixel-12345/mode" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"mode": "normal"}'
# {"ok": true, "mode": "normal", "error_code": null}
```

## The phones' side (WebSocket)

Phones connect by WebSocket to `/` or `/ws` (the same thing). Each phone keeps
one long-lived *control* connection and opens one *media* connection per
call.

Both carry JSON text frames and raw 16 kHz mono 16-bit little-endian PCM binary
frames in both directions (the phone sends 60 ms frames).

A connection from the computer itself (the USB tunnel) is always accepted; any
other must carry `?token=<DEVICE_TOKEN>` when that is set.

The complete message list, field by field, is in
[`android/README.md`](../../android/README.md#websocket-protocol).

Anything that speaks this protocol can stand in for the phone, so a SIP or
softphone front end could be added without touching the call logic. Both sides
ignore what they do not know, so old apps and older Bageo versions keep working.

### Control connection

| Direction | Message | Meaning |
|---|---|---|
| phone → Bageo | `hello` | `serial`, `mode` = `CONTROL_IDLE`, `model`, `app_version`, `phone_mode` (`ai` / `normal`), `in_call`, `supports_sms`, `supports_reject`. No `phone_mode`: an app older than the mode switch (always `ai`). No `supports_sms`: no texts are asked of it. No `supports_reject`: Blacklist numbers are answered and get the announcement instead. |
| phone → Bageo | `ping` | Every 15 s. |
| phone → Bageo | `status` | `{mode, in_call}`: the mode or the call state changed. |
| phone → Bageo | `incoming`, `incoming_cancel` | A call rings (`caller_id`, empty for a hidden number; `sim_country`; `sim_sub_id` when known) / it ended before it was answered. |
| Bageo → phone | `answer_plan` | `{delay_ms, prewarm, reject}`: how long to ring before answering, or `reject: true` to decline the ringing call (Blacklist numbers with `reject_mode` `decline`). |
| phone → Bageo | `call_waiting_rejected` | `{serial, caller_id, sim_country, sim_sub_id}`: a second call rang during a call and the phone declined it at once (AI mode). Becomes a call record and a "Missed while on a call" notification. |
| Bageo → phone / phone → Bageo | `set_mode` / `set_mode_result` | `{mode, id}` / `{id, ok, mode, error}`: the remote mode switch; `error` uses the codes above. |
| Bageo → phone | `agent_info` | `{console_url}` for the app's **Open the web console** button. Sent after the hello when Bageo knows an address: `public_url`, or the LAN IP address when the console is open to your network. |
| Bageo → phone | `place_call` | `{number, call_id, task_id, sub_id}`: dial out for an AI call-back or an AI errand, from the SIM with subscription id `sub_id` (the phone's default when that SIM is not found). While a call rings or is up, the phone does not dial and answers `place_call_failed` with `reason` `busy`. |
| phone → Bageo | `place_call_ack`, `place_call_failed`, `outbound_ready`, `call_end` | The outgoing call was dialed / could not be dialed / connected / ended. |
| Bageo → phone | `send_sms` | `{id, to, text, sub_id}`: text a caller (appointments). An id already sent is answered again and never texted twice. |
| phone → Bageo | `send_sms_result` | `{id, ok, error}`, once every part reported back. The phone gives up after 60 s (`timeout`); a later report sends a second result with the real outcome. |
| phone → Bageo | `send_sms_delivered` | `{id, ok}`: the network's delivery report, when it gives one (the phone waits up to an hour). |

Details for `send_sms` and `send_sms_result`:

- `send_sms`: `id` is 1–64 characters; `to` is digits with an optional leading `+`; `text` is up to 500 characters.
- `sub_id` picks the SIM (the `sim_sub_id` the phone reported), else the default SMS SIM.
- `send_sms_result` `error` values: `no_permission`, `bad_request`, `no_service`, `radio_off`, `generic_failure`, `timeout`, `exception`.
- Bageo waits up to 90 s for the result.
- Results for the last 20 ids are sent again after a reconnect; Bageo drops repeats by `id`.

### Media connection

| Direction | Message | Meaning |
|---|---|---|
| phone → Bageo | `hello` | `serial`, `caller_id`, `sample_rate` (16000), `mode` = `SILENT_LOCAL`, `sim_country`, `sim_sub_id`, `model`, `app_version`. For an outgoing call Bageo checks `sim_sub_id` against the SIM it asked for (errands: `wrong_sim`). |
| phone → Bageo | `ringing`, `call_end` | The caller's number (sent again when it becomes known later) / the call ended. |
| Bageo → phone | `tts_start`, `tts_end`, `hangup` | Audio follows / audio ended / end the call. |
| both | binary | The caller's voice (phone → Bageo) and the AI's voice (Bageo → phone), PCM as above. |

How Bageo handles the edge cases:

- A text result that arrives after Bageo stopped waiting is still
  recorded, and the phone's later real result under the same `id` replaces a
  `timeout`. The same result again, or a different one for a text already
  settled, changes nothing.
- A Blacklist number is declined only when `reject_mode` is `decline` and the
  phone said `supports_reject`. Otherwise it is picked up and gets the
  announcement, and the notification says which happened.
- Call-backs and errands carry `sub_id`, so a dual-SIM phone calls from the
  right number.


<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
