# Contributing

Thank you for looking into Bageo. The most valuable contributions right
now are **reports**: which phone, which language, what happened.

## Reporting what works and what does not

Open an issue with:

- computer system (Windows / macOS / Linux) and how you installed (release or
  source);
- phone model, Android version, root method (Magisk / KernelSU);
- agent version (bottom of the *System* page) and the language and answer style
  you use;
- what you did, what you expected, what happened;
- the relevant lines of the agent's terminal output (`RUST_LOG=agent=debug` for
  more) and, for phone problems, `adb logcat` filtered as shown in
  `android/README.md`.

Remove phone numbers, names and keys. Never attach `settings.json` or
recordings of other people.

## Building

```text
cargo test -p agent          # the agent's tests
cargo build --release -p agent
```

The phone app: see [`android/README.md`](android/README.md).
The SMS forwarder: `python -m unittest` in `tools/sms_forward`.

Working on the web console without rebuilding: start the agent with
`CALLS_UI_FILE=crates/agent/static/calls.html` and reload the browser.

Run one area's tests by name while you work, then the whole suite before a pull
request:

```text
cargo test -p agent errands        # AI errands: numbers, phone and SIM choice, the API
cargo test -p agent contacts       # contacts: matching, import formats, the API
cargo test -p agent stall_review   # the second look before stalling a sales / scam call
cargo test -p agent appointments   # appointments, texts, AI call-backs
cargo test -p agent settings_store # settings: defaults, migration of older keys
```

## Finding your way in the agent

Everything lives in `crates/agent/src`. The files you are most likely to touch:

| File | What it does |
|---|---|
| `main.rs` | Start-up, the phones' WebSocket (control and media connections), the call flow around a Live session, the post-call work (summary, translations, notifications). |
| `gemini_live.rs` | The Gemini Live session of a call: opening line, turn taking, barge-in, goodbye detection, sales / scam stalling, the carrier's warning on calls from abroad, silence handling, session rotation on long calls. |
| `errands.rs` | AI errands: number and phone / SIM checks (`pick_phone`), the SIMs each phone has (`SimBook`, `phone_sims.json`), the errand's script and the `ask_owner` function, live captions, reading the result, the `/calls/api/errands` routes. |
| `contacts.rs` | Contacts: number matching however it is written, vCard / CSV / text / `.xlsx` / `.docx` import, PDF and pictures through Gemini, vCard export, the `/calls/api/contacts` routes. |
| `stall_review.rs` | A second look before the AI starts keeping a caller busy: a cheap model reads the call so far, together with whether the caller is a contact, within a short timeout; no answer counts as sales / scam. |
| `appointments.rs` | Appointments: the Live tools, *To do* tasks, decisions from the console, the API and Telegram, texts to callers, the AI call-back (`call_lang`), and the outbound queue it shares with errands (one call to a number at a time). |
| `callback.rs` | The phones' control connections as seen by the outbound side: `place_call` and `send_sms` to one phone, and its answers. |
| `deepseek.rs` | The post-call summary and transcript translation over an OpenAI-compatible API (Gemini by default; the name is historical). |
| `notify.rs` | Notification channels and every message (summary, blocked, call waiting, AI call-back, AI errand). |
| `settings_store.rs` | `RuntimeSettings`: every console setting, its default, its environment variable, and the migration of keys from older versions. |
| `calls_http.rs` | The web console's API (calls, settings, tasks, devices, previews) and the `Content-Type` checks. |
| `locale.rs` | The nine languages: phrases, goodbye words, notification labels, sales keywords. |
| `calendar.rs`, `phone_region.rs`, `voice_preview.rs` | Calendar links and `.ics`; phone numbers and countries; the recorded voice previews. |

The web console is the single file `crates/agent/static/calls.html` (all nine
languages inside). The phone side is described in
[`android/ARCHITECTURE.md`](android/ARCHITECTURE.md).

## Changing the call logic

The files `crates/agent/src/gemini_live.rs` and `main.rs` hold behaviour that
was tuned on real phone calls over months; much of it looks odd until you know
the failure it prevents. Please:

- keep changes small and say what real-call behaviour they are for;
- add or adapt a unit test (the pure helpers — goodbye detection, sales
  keywords, prompt building, request shapes, number checks, errand results — are
  all testable without a phone);
- say in the pull request whether you tried it on a real call, on which phone
  and in which language. "Not tried on a call" is a fine and useful answer.

Anything that dials or texts (errands, call-backs, appointment texts) needs
extra care: keep the number checks (`not_allowed` for emergency, short and
premium-rate numbers), one call to a number at a time, and the SIM check before
the AI speaks. A state-changing route must keep requiring
`Content-Type: application/json` (see [SECURITY.md](SECURITY.md)).

## Adding a language

1. `crates/agent/src/locale.rs`: add the variant to `Lang` (and `ALL`,
   `parse`, `code`), then a `Phrases` block: how one answers the phone, how one
   says goodbye, the notification labels, the goodbye and wrap-up phrases the
   hang-up logic should recognise, and sales / scam keywords with weights.
2. Languages written with spaces go into `WORD_LANGS`, others into `RUN_LANGS`.
3. The web console (`crates/agent/static/calls.html`) and the phone app
   (`android/app/src/main/res/values-<lang>/strings.xml`): add the dictionary.
4. `cargo test -p agent` — several tests run over every language.

## Adding a notification service

`crates/agent/src/notify.rs`: one entry in `KINDS`, one arm in `prepare`
(build the HTTP request), and a test of the request shape. Mention in the pull
request whether you tried it against the real service.

## Documentation

User documentation lives in `docs/en`, `docs/zh-TW` and `docs/zh-CN`. A change
that users see goes into all three, with the same facts in each: Traditional
Chinese as written in Taiwan, Simplified Chinese as written in mainland China.
Field names, codes and routes stay in English. Examples use made-up names,
`192.0.2.x` / `.local` addresses and placeholder numbers (`0912345678`,
`+1 202 555 0143`).

## Style

- Rust: `cargo fmt`; no new dependencies without a reason.
- Comments say why, not what; no personal data, real phone numbers, private
  network addresses or deployment details, in code, tests or docs.
- User-facing text goes through the language tables.

By contributing you agree that your contribution is licensed under the MIT
License of this repository.
