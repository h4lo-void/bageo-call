# Roadmap

What Bageo 1.6 already does as an open-source AI phone assistant, what it does
only in part, and what would be worth building next. Nothing here is a promise
or a date; pull requests are welcome.

## How Bageo differs from other AI receptionists

Most open-source AI receptionists need a SIP trunk or a cloud telephony
account. Bageo answers **the SIM you already have**, in a rooted Android phone
on your desk, with a speech-to-speech model (Gemini Live). There is no number
to rent, and quick replies and interruptions come from the model itself
instead of a chain of speech-to-text, LLM and text-to-speech. Since 1.0 the
same phone also makes calls for you, from your own number.

## Where 1.6 stands

**Done** works today. **Partly** works with the limits noted. **Not yet** is
missing, with no date.

### Answering calls

| Wish | Status | Notes |
|---|---|---|
| Answers around the clock, free conversation instead of menus | Done | |
| Low latency | Done | Speech-to-speech; the opening line is prepared while the phone rings (measured median about 0.43 s from pick-up). |
| Real interruption (barge-in) | Done | Detected per call on phones whose call capture does not contain the AI's own voice (for example the Pixel 9); on other phones the AI finishes its sentence first. |
| Silence handling | Done | Asks whether the caller is still there, then ends the call. |
| Voice-activity detection, noise | Done | Gemini's own detection, or a local detector. |
| Natural surroundings | Done | Twelve background sounds at three levels. |
| Spam and robocall handling | Done | Blacklist (with `*` prefixes); three layers (follow-up whitelist, weighted keywords, a semantic second look), then a polite no or Stall Mode. |
| Recognising returning callers | Partly | Contacts give the AI the caller's name and your note. Earlier summaries for the same number are not in the prompt yet. |
| Knowledge-base answers without making things up | Partly | *Notes the AI may answer from*: plain text in the prompt with a "do not guess" rule. No document search. |
| AI disclosure, recording notice | Partly | Disclosure is built into the *AI assistant* style; *As me* speaks as you without disclosing. A recording notice is up to your prompt. |
| Prompt-injection resistance | Partly | A fixed rule in every prompt; no separate filter. |

### Languages

| Wish | Status | Notes |
|---|---|---|
| Many languages, localised prompts | Done | Nine full languages: voice, prompts, console, app and notifications. Traditional Chinese is proven on real calls, the others in simulated calls. |
| Language detection, switching mid-call | Done | The opening follows the caller's number and the SIM; mid-call the AI follows the caller. Simulated with callers speaking 28 languages. |
| Dual SIM with a language per SIM | Done | *Main language for each SIM*. |
| Translated transcripts | Done | Lines in another language keep their wording, with a translation into your notification language. |
| Dialects and accents | Partly | Whatever the model understands; no tuning of our own. |
| Live translation between a caller and a person | Not yet | |

### Doing things for you

| Wish | Status | Notes |
|---|---|---|
| Outbound calls on your behalf | Done | AI errands with live captions, questions to you (`ask_owner`), instructions during the call, a structured result and follow-up calls. Proven in simulation; reports from real phones are welcome. |
| Appointment booking | Done | The AI takes the request without agreeing; you accept or decline under *To do* or on a Telegram card; the caller hears back by text or AI call-back. |
| Calendar | Done | One tap into Google, Outlook.com, Microsoft 365, Apple, Yahoo or `.ics`, with no calendar account connected. |
| SMS replies | Done | Appointment answers go out by text from the SIM that took the call. Incoming texts can be forwarded with the SMS forwarder, and you can read conversations and reply directly on the console's **SMS** page. |
| Contacts | Done | Import vCard, CSV, TSV, text, Excel and Word files, and PDFs or pictures through Gemini; merge duplicates; export vCard. |
| Tools during the call | Partly | `propose_appointment` and `ask_owner`, each under a strict state machine. No open-ended tool use, by design. |
| Post-call summary, intent, to-do | Done | Four lines; appointments become to-do items. No sentiment analysis or analytics dashboard. |

### Running it

| Wish | Status | Notes |
|---|---|---|
| Bring your own keys, no per-minute platform fee | Done | One Gemini key covers everything; summaries can use any OpenAI-compatible service or a local model. |
| Everything stored on your own machine | Done | Except what goes to the models and the notification services; see [Privacy](PRIVACY_AND_LEGAL.md). |
| Notifications, webhooks, n8n / Zapier | Done | 32 channel types, including e-mail and a [JSON webhook](NOTIFICATIONS.md#webhook-json); the REST API for the rest. |
| Web console from any device | Done | `bageo.local`, QR code, add to home screen. |
| One-step start | Done | Windows installer (`setup.exe`) and macOS `.dmg` (both include the **Bageo Status** tray app with desktop call, SMS and summary notifications), Linux start script, and ready-made Docker images (`bageo/bageo-call`, `ghcr.io/h4lo-void/bageo-call`). |
| Chat commands | Done | Control Bageo from Telegram, Discord, Slack, Matrix or any tool via the inbound command route (`POST /calls/api/control/inbound`). |
| Backup & restore | Done | Scheduled and manual backups, selective export/import, and optional `.bageo` password encryption. |
| Use the spare phone as a normal phone again | Done | One switch in the console or the app; the app needs no root. |
| REST API | Done | See the [API reference](API.md). |
| MCP server | Done | See [MCP](MCP.md). |
| Business hours, holidays | Not yet | The status note covers simple cases. |
| Forward calls to your phone | Partly | Choose who can be forwarded (*Whitelist*, *Other contacts*, *Numbers not in your contacts*), with carrier forward-when-busy, instant urgent alerts, and a trial urgent 3-way conference merge (*When it's urgent, call me and join us (trial)*). |
| Transfer to a person with a summary | Not yet | Urgent 3-way conference merge is available as a trial, without a private spoken summary briefing before joining. |
| Emergency keyword handling | Not yet | Errands never dial emergency numbers. |
| Spending caps, daily call limits | Not yet | Only the longest-call settings; set a budget with your API provider. |
| Fully local voice stack (Whisper / Ollama / Piper) | Not yet | Not in the supported mode. An older pipeline exists in the code, unmaintained. The summary can already use a local model. |
| SIP / Asterisk / FreeSWITCH | Not yet | Not the goal, but the [phone protocol](../../android/README.md#websocket-protocol) is small and documented, so a SIP front end could stand in for the phone. |
| Multi-tenant, white-label | Not planned | |
| CRM integrations | Not yet | Use the webhook. |

## Good next steps

Roughly in order of usefulness for the effort:

1. **Reports from real use**: AI errands dialing out on real phones, other
   phone models, other languages, notification channels other than Telegram.
   This matters more than any new feature.
2. **Caller memory**: the last summaries for the same number in the prompt,
   on top of the contact's name and note.
3. **Per-number rules**: whitelisted numbers that always notify loudly, numbers
   that are never stalled, a different prompt for certain contacts.
4. **Business hours**, with a different prompt or an announcement outside them.
5. **Clearer call states from the phone**: report the moment an outgoing call
   is answered, so *To do* and the errand page can tell "answered" from
   "ringing" without listening for a voice.
6. **Maturing call transfer and 3-way conference**, graduating the trial urgent merge and adding warm transfer with a spoken briefing.
7. **Other realtime models** behind the same call logic (OpenAI Realtime, and
   local speech-to-speech models as they mature).
8. **Removing the older menu-based code paths** to make Bageo's code easier to
   read.
9. **Signed, notarised releases** for macOS and Windows.

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
