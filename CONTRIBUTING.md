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

## Changing the call logic

The files `crates/agent/src/gemini_live.rs` and `main.rs` hold behaviour that
was tuned on real phone calls over months; much of it looks odd until you know
the failure it prevents. Please:

- keep changes small and say what real-call behaviour they are for;
- add or adapt a unit test (the pure helpers — goodbye detection, sales
  keywords, prompt building, request shapes — are all testable without a phone);
- say in the pull request whether you tried it on a real call, on which phone
  and in which language. "Not tried on a call" is a fine and useful answer.

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

## Style

- Rust: `cargo fmt`; no new dependencies without a reason.
- Comments say why, not what; no personal data or deployment details.
- User-facing text goes through the language tables.

By contributing you agree that your contribution is licensed under the MIT
License of this repository.
