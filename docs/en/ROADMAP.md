# Roadmap

Where Bageo 1.0 stands against what people ask of an open-source phone
assistant, and what would be good to build next. Nothing here is a promise;
pull requests are welcome.

## What makes this project different

Most open-source receptionists need a SIP trunk or a cloud telephony account.
Bageo answers **the SIM you already have**, on a phone on your desk, with a
speech-to-speech model, so there is no number to rent, and latency and
interruption handling come from the model rather than from a chain of
speech-to-text, LLM and text-to-speech. Since 1.0 the same phone also makes
calls for you, from your own number.

## Status against what the community asks for

✅ there · 🟡 partly · ❌ not yet

### Answering calls

| Wish | Status |
|---|---|
| Answers around the clock, free conversation instead of menus | ✅ |
| Low latency | ✅ Speech-to-speech; the opening line is prepared while the phone rings (measured median about 0.43 s from pick-up). |
| Real interruption (barge-in) | ✅ On phones whose call capture is free of the AI's own voice (Pixel 9 in our use), detected per call; elsewhere the AI finishes its sentence first. |
| Silence handling | ✅ Asks whether the caller is still there, then ends the call. |
| Voice-activity detection, noise | ✅ Gemini's, or a local detector. |
| Natural surroundings | ✅ Twelve background sounds at three levels. |
| Spam and robocall handling | ✅ Blocked numbers; three layers (follow-up whitelist, weighted keywords, a semantic second look) and either a polite no or Stall Mode. |
| Recognise returning callers | 🟡 Contacts give the AI the caller's name and your note. Earlier summaries of the same number are not in the prompt yet. |
| Knowledge-base answers without making things up | 🟡 *Notes the AI may answer from*: plain text in the prompt with a "do not guess" rule. No document search. |
| AI disclosure, recording notice | 🟡 Disclosure built into the *AI assistant* style and enforced on request in *As me*. A recording notice is up to your prompt. |
| Prompt-injection resistance | 🟡 A fixed rule in every prompt; no separate filter. |

### Languages

| Wish | Status |
|---|---|
| Many languages, localised prompts | ✅ Nine full languages (voice, prompts, console, app, notifications). Traditional Chinese is proven on real calls; the others in simulated calls. |
| Language detection, switching mid-call | ✅ The opening follows the caller's number and the SIM; the AI follows the caller mid-call. Simulated with callers in 28 languages. |
| Dual SIM with a language per SIM | ✅ *Main language for each SIM*. |
| Translated transcripts | ✅ Lines in another language keep their wording, with a translation into your notification language. |
| Dialects and accents | 🟡 Whatever the model understands; no tuning of our own. |
| Live translation between a caller and a human | ❌ |

### Doing things for you

| Wish | Status |
|---|---|
| Outbound calls on your behalf | ✅ AI errands with live captions, questions to you (`ask_owner`), instructions during the call, a structured result and follow-up calls. Proven in simulation; waiting for real-phone reports. |
| Appointment booking | ✅ The AI takes the request without agreeing; you accept or decline under *To do* or on a Telegram card; the caller hears back by text or AI call-back. |
| Calendar | ✅ One tap into Google, Outlook.com, Microsoft 365, Apple, Yahoo or `.ics`, without connecting any calendar account. |
| SMS replies | ✅ Appointment answers by text from the SIM that took the call. Incoming texts can be forwarded with the SMS forwarder. |
| Contacts | ✅ Import from vCard, CSV, TSV, text, Excel, Word, and PDFs or pictures through Gemini; merge duplicates; export vCard. |
| Tools during the call | 🟡 `propose_appointment` and `ask_owner`, each under a strict state machine. No open-ended tool use, by design. |
| Post-call summary, intent, to-do | ✅ Four lines, appointments as to-do items. No sentiment or analytics dashboard. |

### Running it

| Wish | Status |
|---|---|
| Bring your own keys, no per-minute platform fee | ✅ One Gemini key covers everything; summaries can use any OpenAI-compatible service or a local model. |
| Everything stored on your own machine | ✅ Except what goes to the models and the notification services. |
| Notifications, webhooks, n8n / Zapier | ✅ Sixteen channel types, including e-mail and a JSON webhook; REST API for the rest. |
| Web console, from any device | ✅ `bageo.local`, QR code, add to home screen. |
| One-step start | ✅ Start scripts for Windows, macOS and Linux; Docker image. |
| Use the spare phone as a normal phone again | ✅ One switch in the console or the app; the app needs no root. |
| REST API | ✅ MCP ❌ |
| Business hours, holidays | ❌ (the status note covers simple cases) |
| VIP numbers straight to a human | ❌ |
| Transfer to a human with a summary | ❌ The phone app cannot bridge calls yet. |
| Emergency keyword handling | ❌ (errands never dial emergency numbers) |
| Spending caps, daily call limits | ❌ Only the longest-call settings; set a budget with your API provider. |
| Fully local voice stack (Whisper / Ollama / Piper) | ❌ in the supported mode. An older pipeline exists in the code, unmaintained. The summary can already use a local model. |
| SIP / Asterisk / FreeSWITCH | ❌ Not the goal, but the phone protocol is small and documented, so a SIP front end could stand in for the phone. |
| Multi-tenant, white-label | ❌ Not planned. |
| CRM integrations | ❌ Use the webhook. |

## Good next steps

Roughly in order of usefulness for the effort:

1. **Reports from real use**: AI errands dialing out on real phones, other
   phone models, other languages, the notification channels other than
   Telegram. This matters more than any feature.
2. **Caller memory**: the last summaries for the same number in the prompt,
   on top of the contact's name and note.
3. **Per-number rules**: VIPs that always notify loudly, numbers that are never
   stalled, a different prompt for certain contacts.
4. **Business hours** with a different prompt or an announcement outside them.
5. **Clearer call states from the phone**: report the moment an outgoing call
   is answered, so *To do* and the errand page can tell "answered" from
   "ringing" without listening for a voice.
6. **Call transfer / conference** from the phone app, so a human can take over.
7. **Other realtime models** behind the same call logic (OpenAI Realtime, local
   speech-to-speech models as they mature).
8. **Removing the older menu-based code paths** to make the agent easier to
   read.
9. **Signed, notarised releases** for macOS and Windows.
