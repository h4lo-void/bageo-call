# API

The web console is a single page that uses a small JSON API. You can use the
same API from scripts. Base address: where the agent listens, by default
`http://127.0.0.1:8787`.

## Access

Without `CALLS_UI_TOKEN` the API is open to whoever can reach the port (by
default only the computer itself). With it, send the token on every request in
one of these ways:

```text
GET /calls/api/list?token=<token>
Authorization: Bearer <token>
X-Calls-Token: <token>
```

A wrong or missing token gets `401`.

Errors answer `{ok: false, code, error}`: `code` is a stable word to act on
(`not_found`, `invalid_id`, `no_audio`, `preview_failed`, `save_failed`,
`missing_field`, `auth_failed`, …; the web console shows it in your language),
`error` the details (mostly in English). One exception: `POST /calls/api/devices/{serial}/mode` always answers `{ok, mode, error_code}` (see below).

## Calls

| Method and path | What |
|---|---|
| `GET /calls/api/list` | Call records, newest first. Query: `limit`, `offset`, `q` (free text over number, phone, events, transcript), `caller`, `serial`, `status`, `has_transcript`. Answer: `{ok, calls: [...], total, limit, offset}`. |
| `GET /calls/api/call/{id}` | One call: `{ok, call, notes, calendar_link}`. `notes` (null until the summary is written) has `summary`, `dialogue`, `end_hint`, `source`, `updated_at` and, when the call fixed a date, `calendar` = `{title, start, end, all_day}` (local time, `YYYY-MM-DDTHH:MM`). |
| `GET /calls/api/call/{id}/event.ics` | The calendar suggestion as an iCalendar file. `404` when the call has none. |
| `GET /calls/api/audio/{id}/listen` | The call as you would have heard it (caller and AI mixed), WAV, range requests supported. |
| `GET /calls/api/audio/{id}/remote` · `/tts` · `/audio/{id}` | Caller only · AI only · the raw mixed recording. |
| `GET /calls/api/live` | Calls in progress and the phones connected: `{ok, count, calls, control_idle_clients, control_idle_serials, normal_mode_serials}`. `normal_mode_serials` are connected phones switched to a normal phone; the page does not count them as ready. |
| `GET /calls/api/stats` | `{ok, stats: {total, today, voicemails, blocked}}`. |
| `GET /calls/api/export.csv` | The filtered list as CSV (same query as `list`, up to 2000 rows). |

A call record (`call`, and each entry of `calls`) carries among others: `id`,
`serial` (the phone), `caller_id`, `started_at` / `ended_at` (RFC 3339, UTC),
`duration_ms`, `status` (`ringing`, `active`, `ended`), `events` (what happened,
in order — e.g. `active`, `persona_stall_spam`, `reject_list`,
`reject_list_declined`, `call_waiting_rejected`, `persona_ai_fail_hangup`,
`ended`), `summary`, which audio files exist, `sim_country` / `sim_sub_id` (the
SIM the call came in on, when the phone app says), `reply_method` /
`reply_contact` (also a number the caller said to call back on) and,
when the caller set a time with the AI, `appointment` as the call left it
(see [Appointments](#appointments)); `notes.appointment` is kept up to date
after the call (your decision, the text result).

In `notes.dialogue` every line is one turn, prefixed with the speaker label in
the agent's language and its separator: `Caller: ` / `AI: ` in English,
`對方：` / `接聽：` in Traditional Chinese, `对方：` / `接听：` in Simplified
Chinese, `相手：` / `AI：`, `상대: ` / `AI: `, `Llamante: ` / `IA: `,
`Appelant : ` / `IA : `, `Anrufer: ` / `KI: `, `Chamador: ` / `IA: `.
Records saved by earlier versions use `對方：` / `接聽：` in every language.

## Settings

| Method and path | What |
|---|---|
| `GET /calls/api/settings` | `{ok, settings, storage_path, console_url, console_ip_url}`: `console_url` is the console's address in use (the one you typed, or the automatic one; empty when only this computer can open it), `console_ip_url` the same with the LAN IP instead of a `.local` name. Secrets come back masked (`"abc…wxyz"`) with a `<name>_set` flag; notification channels come back with empty `url` / `token` and `url_hint`, `token_hint`, `url_set`, `token_set`. Also read-only helpers: `default_prompt`, `voices`, `ambience_kinds`, `languages`, `countries` (ISO codes for *Main language for each SIM*), `seen_sim_countries` (SIM countries the phones reported since the agent started), `notify_kinds`, `sample_lines`, `summary_ready`. |
| `PUT /calls/api/settings` | A JSON object with only the fields to change. Answer as for `GET`. |

Rules for `PUT`:

- A secret field (`gemini_api_key`, `deepseek_api_key`, `gcal_client_secret`,
  `gcal_refresh_token`) that is absent or `""` keeps its stored value;
  `"<name>_clear": true` empties it.
- `notify_channels` is replaced as a whole. A row `{id, kind, name, enabled,
  url, token, target}` whose `url` or `token` is empty keeps the stored value
  of the row with the same `id` and `kind`; a row without `id` is new; rows you
  leave out are deleted.
- `reject_numbers` is a list of `{number, reason}`.

Example — switch the language and add a blocked number:

```text
PUT /calls/api/settings
{"language": "ja", "reject_numbers": [{"number": "0800*", "reason": "telemarketing"}]}
```

The field names are those of `RuntimeSettings` in
[`crates/agent/src/settings_store.rs`](../../crates/agent/src/settings_store.rs),
where each is documented.

## Notifications

| Method and path | What |
|---|---|
| `POST /calls/api/notify/test` | Body `{"channel": {kind, url, token, target, id?}}`. Sends the test message to that one channel (stored secrets are filled in by `id`). `200 {ok: true}` or `502 {ok: false, code, field, kind, error}`: `code` says what went wrong (`missing_field`, `bad_url`, `bad_value`, `auth_failed`, `service_not_found`, `http_error`, `service_error`, `network_error`, `send_failed`), `field` which of `url` / `token` / `target` (or empty), `error` the service's answer. |
| `POST /calls/api/notify/send` | Body `{"title": "...", "body": "..."}`. Sends your message to every enabled channel. `{ok: true}` when at least one accepted it. |

The JSON your own webhook receives is described in
[Notifications](NOTIFICATIONS.md#webhook-json).

## Voice and sound previews

`GET /calls/api/preview` returns a WAV. Query: `voice` (a voice name, or `none`
for the background alone), `ambience` (`off` or a kind), `level` (1–3),
`regen=true` (generate again instead of the cached sample),
`clip=reject` (the blocked-number announcement in that voice),
`lang` (`ja`, `zh-TW`, …; empty = the saved language). Every built-in
voice comes recorded in every language (`assets/voices/`), so a preview needs no
Gemini session; only `regen=true` or a voice without a recording opens one.

## Appointments

With `appointment_mode` `confirm` or `auto` (see
[Settings](SETTINGS.md#summary--calendar)), a call where the caller set a time
with the AI leaves a task (`intent` `appointment`) and an `appointment` object
on the call record, in its notes and on the task:

```json
{
  "start": "2026-11-14T15:00", "end": "2026-11-14T16:00",
  "title": "House viewing", "caller_name": "Mr Chen",
  "mode": "confirm", "status": "pending",
  "tag": "6f1c2a9e-3b7d-4e0a-9c55-2d8f1b7e4a10"
}
```

Times are local (`TZ`). `status`: `pending` (waiting for you), `booked` (put
in the calendar on the call), `confirmed`, `declined`. `event_id`: the Google
Calendar event once it is there. `auto_error`: why *Book it if I'm free* could
not book it. `replaced`: earlier times of the same call that were already put
in the calendar. `sms`: `sent`, or why the text did not go (`offline`,
`old_app`, `timeout`, `busy`, `no_number`, or the phone's error code). `tag`:
the call's own mark on the events it writes (below). `tg_card`: `{chat_id,
message_id}` of the Telegram card sent for it, whose buttons are taken off when
you decide elsewhere. Empty fields are left out.
`write_error`: why the last ✅ could not put it in the calendar (it still
waits). `held_busy`: that time was not free, so the next ✅ books it anyway.
`sim_country` / `sim_sub_id`: the SIM that took the call; its country
reads a caller ID without a country code for the text, which goes out from it.
A call can agree to more than one appointment: each gets a task and a `tag` of
its own (the call's, then `<tag>-2`, …), and the call record and notes keep the
first with the others in `more`.
Events the agent writes carry the private extended property
`bageo_call=<appointment.tag>`, so an event whose insert answer was lost is found
again instead of written twice.

| Method and path | What |
|---|---|
| `GET /calls/api/tasks` | Tasks, newest first (up to 200): `{ok, tasks}`. The console's *Appointments to confirm* list is the tasks whose `appointment.status` is `pending`. |
| `POST /calls/api/tasks/{id}/confirm` | For an appointment: the same as **✅ Confirm** in the web console or on its Telegram card — Google Calendar, then (with `appointment_sms`) the text to the caller, which may take up to 90 seconds. `{ok, task, message}`; `message` is what happened, as the console shows it and the Telegram bot replies it. When a Telegram card went out for the appointment, its buttons are taken off and `message` is posted in that chat (best effort; the decision stands either way). Only the first decision counts: a second confirm or decline — also one sent while the first is still running, or a press on the Telegram card — answers "already handled" and changes nothing. The time is checked again just before it is written: when something is in the calendar then by now, or the write fails, the appointment stays `pending` with `write_error`, nothing is texted, the Telegram card keeps its buttons, `message` says why, and it can be confirmed again; after a busy time (`held_busy`) the next confirm writes it anyway. Needs `Content-Type: application/json` (no body), otherwise HTTP 400 `bad_request`, so other web pages cannot decide from a browser on your network. |
| `POST /calls/api/tasks/{id}/ignore` | For an appointment: **❌ Decline** (an event the call already put in the calendar is removed; the text when `appointment_sms` is on). Same answer and rules. |
| `POST /calls/api/appointments/{id}/retry-sms` | Texts the caller of a confirmed or declined appointment again when its text did not go out (`sms` is a failure code), saying what the decision said. Optional body `{"serial": "<phone>"}` sends it from another connected phone; without it, from the phone that took the call. `{ok, sms, message}`: `ok` when it was sent, `sms` the new code, `message` what happened. Still waiting, already sent, never texted, or a number that takes no texts (`no_number`): HTTP 409 `sms_not_retryable` with the current `sms`. Needs `Content-Type: application/json`; a phone that is not letters, digits, `-` and `_` gets 400 `invalid_serial`. |

## Phones and health

| Method and path | What |
|---|---|
| `GET /calls/api/devices` | `{ok, usb: {enabled, adb_found, adb_path, error, phones: [{serial, state, model, reverse, app_installed}]}, connected: [...], labels: {<phone id>: <model>}, calls_in_progress, phones: [...]}`. Each entry of the top-level `phones` is a phone seen on its control connection since the agent started: `{serial, connected, mode, supports_mode, model, app_version, in_call, last_seen_ms}`. `mode` is `ai` (the AI answers) or `normal` (an ordinary phone, see [Phone setup](PHONE.md#normal-phone-mode)); `supports_mode` is false for apps older than the switch, which always run as `ai`. |
| `POST /calls/api/devices/{serial}/mode` | Body `{"mode": "ai" \| "normal"}`. Asks the phone to switch and waits up to 15 seconds for its answer. Always `{ok, mode, error_code}`: `mode` is the phone's mode afterwards (`null` when unknown), `error_code` is `null` on success, otherwise one of `offline` (not connected), `unsupported` (app too old), `in_call`, `busy` (a switch is already running), `no_root` (the Bageo module is not running on the phone), `save_failed`, `dialer_failed`, `screening_failed`, `dnd_failed`, `volume_failed`, `failed`, or `timeout`. `timeout` means "not confirmed yet", not "failed": the phone app can be restarted by the switch itself and reports the result after it reconnects, so read the mode again a little later (`GET /calls/api/devices`). The body is parsed by the agent: anything that is not JSON, has no string `mode`, or a serial that is not letters, digits, `-` and `_` gets HTTP 400 with the same JSON and `error_code` `bad_request`; an unknown `mode` gets 400 and `invalid_mode`. The request must have `Content-Type: application/json` (also 400 `bad_request` otherwise), so that other web pages cannot switch a phone from a browser on your network. |
| `GET /health` | A large flat JSON object for troubleshooting: version, language, what is configured, connected phones, calls in progress; no keys or tokens. `control_idle_clients` / `control_idle_serials` count every connected phone; `normal_mode_serials` lists those switched to a normal phone, whose calls the AI does not answer. When a console token is set and the request does not carry it, only `ok`, `version`, `language`, `active_call_count` and `control_idle_clients` are returned (enough for monitoring). |

## The phones' side

Phones connect by WebSocket to `/` or `/ws` (same thing): one long-lived
*control* connection per phone and one *media* connection per answered call,
carrying JSON text frames and raw 16 kHz mono 16-bit PCM binary frames in both
directions. A connection from the computer itself (the USB tunnel) is always
accepted; any other must carry `?token=<DEVICE_TOKEN>` when that is set.

The messages are listed in
[`android/README.md`](../../android/README.md#websocket-protocol). Anything
that speaks this protocol can stand in for the phone — which is how a SIP or
softphone front end could be added without touching the call logic.

For the phone mode, the control connection carries these messages (both sides
ignore what they do not know, so old apps and old agents keep working):

| Direction | Message | Meaning |
|---|---|---|
| phone → agent | `hello` with `phone_mode` (`ai` / `normal`) and `in_call` | The phone's mode when it connects. `mode` stays `CONTROL_IDLE` (the connection kind). No `phone_mode` = an older app: `ai`, cannot be switched. |
| phone → agent | `{"type": "status", "mode": "ai" \| "normal", "in_call": bool}` | The mode or the call state changed. |
| agent → phone | `{"type": "set_mode", "mode": "ai" \| "normal", "id": "<uuid>"}` | Switch (sent by the route above). |
| phone → agent | `{"type": "set_mode_result", "id": "<same>", "ok": bool, "mode": "...", "error": "<code>"}` | Result; `error` is empty on success and uses the codes above. |
| agent → phone | `{"type": "agent_info", "console_url": "http://…/calls/"}` | Right after the hello, when the agent has a console address: the one you typed (`public_url`) or, when that is empty and the console is open to your network, the LAN IP address (some phones cannot open `.local` names). The app shows an "Open the web console" button for it. |

Text messages to callers (appointments) use the same connection:

| Direction | Message | Meaning |
|---|---|---|
| phone → agent | `hello` with `"supports_sms": true` | The app can send texts for the agent. No field = an older app: no texts are asked of it. |
| agent → phone | `{"type": "send_sms", "id": "<≤ 64 characters>", "to": "+886912345678", "text": "<UTF-8, ≤ 500 characters>"}` | Sent only to the phone that took the call. `to` is digits with an optional leading `+`. |
| phone → agent | `{"type": "send_sms_result", "id": "<same>", "ok": true}` or `{"type": "send_sms_result", "id": "<same>", "ok": false, "error": "<code>"}` | Sent once every part of the message has reported back (the phone gives up after 60 seconds: `timeout`). `error` is one of `no_permission`, `bad_request`, `no_service`, `radio_off`, `generic_failure`, `timeout`, `exception`. The agent waits up to 90 seconds; no answer counts as failed. |
| agent → phone | `send_sms` with `"sub_id": <int>` | Optional: the SIM (Android subscription id) to send from — the one the call came in on. Without it, the phone's default SIM. |
| phone → agent | `{"type": "send_sms_delivered", "id": "<same>", "ok": bool}` | The carrier's delivery report (each part of a long text may report; one `false` means not delivered). |

A result that comes after the agent stopped waiting is still recorded. The
phone's `timeout` is not final: when the radio answers later, the phone sends the
real result under the same `id` and it replaces the timeout (a send still waiting
keeps waiting for it). After reconnecting the phone resends its recent results:
the same result again changes nothing, nor does a different one for a text
already settled.

Calls use these optional fields (an app or agent that does not know them works
as before):

| Direction | Message | Meaning |
|---|---|---|
| phone → agent | `incoming` and the media `hello` with `"sim_sub_id": <int>` | The SIM (Android subscription id) that took the call; texts and call-backs for the call use it. |
| phone → agent | control `hello` with `"supports_reject": true` | The app can refuse a ringing call. No field = an older app: blocked numbers are picked up and get the announcement, whatever `reject_mode` says. |
| agent → phone | `answer_plan` with `"reject": true` | A blocked number with `reject_mode` `decline`, to an app that said `supports_reject`: refuse the ringing call. Sent with `"delay_ms": 0`; if the app picks up after all, the agent plays the announcement. |
| phone → agent | `{"type": "call_waiting_rejected", "serial": "…", "caller_id": "…", "sim_country": "us", "sim_sub_id": 20}` | A second call came in during a call and the phone turned it away (busy). The agent records it and sends a notification. |
| agent → phone | `place_call` with `"sub_id": <int>` | A call-back dials out on the SIM the caller called, when it goes out on that phone. |
