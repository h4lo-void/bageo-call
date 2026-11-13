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
`error` the details (mostly in English).

## Calls

| Method and path | What |
|---|---|
| `GET /calls/api/list` | Call records, newest first. Query: `limit`, `offset`, `q` (free text over number, phone, events, transcript), `caller`, `serial`, `status`, `has_transcript`. Answer: `{ok, calls: [...], total, limit, offset}`. |
| `GET /calls/api/call/{id}` | One call: `{ok, call, notes, calendar_link}`. `notes` (null until the summary is written) has `summary`, `dialogue`, `end_hint`, `source`, `updated_at` and, when the call fixed a date, `calendar` = `{title, start, end, all_day}` (local time, `YYYY-MM-DDTHH:MM`). |
| `GET /calls/api/call/{id}/event.ics` | The calendar suggestion as an iCalendar file. `404` when the call has none. |
| `GET /calls/api/audio/{id}/listen` | The call as you would have heard it (caller and AI mixed), WAV, range requests supported. |
| `GET /calls/api/audio/{id}/remote` · `/tts` · `/audio/{id}` | Caller only · AI only · the raw mixed recording. |
| `GET /calls/api/live` | Calls in progress and the phones connected: `{ok, count, calls, control_idle_clients, control_idle_serials}`. |
| `GET /calls/api/stats` | `{ok, stats: {total, today, voicemails, blocked}}`. |
| `GET /calls/api/export.csv` | The filtered list as CSV (same query as `list`, up to 2000 rows). |

A call record (`call`, and each entry of `calls`) carries among others: `id`,
`serial` (the phone), `caller_id`, `started_at` / `ended_at` (RFC 3339, UTC),
`duration_ms`, `status` (`ringing`, `active`, `ended`), `events` (what happened,
in order — e.g. `active`, `persona_stall_spam`, `reject_list`,
`persona_ai_fail_hangup`, `ended`), `summary`, and which audio files exist.

In `notes.dialogue` every line is one turn, prefixed with the stored speaker
label `對方：` (the caller) or `接聽：` (the AI). The labels are fixed whatever
the language; translate them for display.

## Settings

| Method and path | What |
|---|---|
| `GET /calls/api/settings` | `{ok, settings, storage_path}`. Secrets come back masked (`"abc…wxyz"`) with a `<name>_set` flag; notification channels come back with empty `url` / `token` and `url_hint`, `token_hint`, `url_set`, `token_set`. Also read-only helpers: `default_prompt`, `voices`, `ambience_kinds`, `languages`, `notify_kinds`, `sample_lines`, `summary_ready`. |
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
`clip=reject` (the blocked-number announcement in that voice). The first
request for a voice opens a short Gemini session.

## Phones and health

| Method and path | What |
|---|---|
| `GET /calls/api/devices` | `{ok, usb: {enabled, adb_found, adb_path, error, phones: [{serial, state, model, reverse, app_installed}]}, connected: [...], labels: {<phone id>: <model>}, calls_in_progress}`. |
| `GET /health` | A large flat JSON object for troubleshooting: version, language, what is configured, connected phones, calls in progress; no keys or tokens. When a console token is set and the request does not carry it, only `ok`, `version`, `language`, `active_call_count` and `control_idle_clients` are returned (enough for monitoring). |

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
