# Roadmap

Where the project stands against what people ask of an open-source phone
answering system, and what would be good to build next. Nothing here is a
promise; pull requests are welcome.

## What makes this project different

Most open-source receptionists need a SIP trunk or a cloud telephony account.
This one answers **the SIM you already have**, on a phone on your desk, with a
speech-to-speech model — so there is no number to rent, and latency and
interruption handling come from the model rather than from a chain of
speech-to-text, LLM and text-to-speech.

## Asked for by the community — status

| Wish | Status |
|---|---|
| Answers around the clock, free conversation instead of menus | ✅ |
| Low latency | ✅ Speech-to-speech; the opening line is prepared while the phone rings. |
| Real interruption (barge-in) | ✅ On phones whose call recording is free of the AI's own voice (Pixel 9 in our use); otherwise the AI finishes its sentence first. |
| Silence handling | ✅ Asks whether the caller is still there, then ends the call. |
| Voice-activity detection, noise | ✅ Gemini's, or a local detector. |
| Language detection, switching mid-call | 🟡 The AI follows the caller's language on every call; relies on the model. |
| Many languages, localised prompts | 🟡 Nine languages; only Traditional Chinese is proven on real calls. |
| Dialects and accents | 🟡 Whatever the model understands; no tuning of our own. |
| Spam / robocall handling | ✅ Blocked numbers; sales and scam calls detected by keywords and either declined or stalled. |
| Knowledge-base answers without making things up | 🟡 *Notes the AI may answer from* — plain text in the prompt with a "do not guess" rule. No document search. |
| Caller memory (recognise returning callers) | ❌ |
| Post-call summary, intent, to-do | ✅ Four lines. No sentiment or analytics dashboard. |
| Calendar | 🟡 A suggestion with an "add to calendar" link / `.ics`; optional automatic insert into Google Calendar. The AI does **not** book appointments during the call. |
| Notifications, SMS / e-mail summary | ✅ Sixteen channel types including e-mail. No SMS sending. |
| Webhooks, n8n / Zapier | ✅ JSON webhook per event; REST API for the rest. |
| Describe-your-business self-configuration | 🟡 The interview prompt you run in any chat AI. |
| AI disclosure, recording notice | 🟡 Disclosure built into the *AI assistant* style and enforced on request in *As me*. A recording notice is up to your prompt. |
| Prompt-injection resistance | 🟡 A fixed rule in every prompt; no separate filter. |
| Bring your own keys, no per-minute platform fee | ✅ |
| Everything stored on your own machine | ✅ Except what goes to the model and the notification services. |
| Web console | ✅ |
| One-step start | ✅ Start scripts for Windows / macOS / Linux; Docker image. |
| REST API | ✅ — MCP ❌ |
| Business hours, holidays | ❌ (the status note covers simple cases) |
| VIP numbers straight to a human | ❌ |
| Transfer to a human with a summary | ❌ The phone app cannot bridge calls yet. |
| Live translation between caller and human | ❌ |
| Emergency keyword handling | ❌ |
| Spending caps, daily call limits | ❌ Only the longest-call settings; set a budget with your API provider. |
| Fully local voice stack (Whisper / Ollama / Piper) | ❌ in the supported mode. An older pipeline exists in the code, unmaintained. The summary can already use a local model. |
| SIP / Asterisk / FreeSWITCH | ❌ Not the goal — but the phone protocol is small and documented, so a SIP front end could stand in for the phone. |
| Multi-tenant, white-label | ❌ Not planned. |
| CRM integrations | ❌ Use the webhook. |

## Good next steps

Roughly in order of usefulness for the effort:

1. **Reports from other phones and other languages** — what works, what does
   not. This matters more than any feature.
2. **Per-number rules**: names and notes for known callers ("this is my
   mother"), VIPs that always notify loudly, numbers that are never stalled.
3. **Caller memory**: the last summaries for the same number in the prompt.
4. **Business hours** with a different prompt or an announcement outside them.
5. **Tools during the call** (Gemini function calling): check a calendar, look
   up an order. Needs care — an AI that can act can act wrongly.
6. **Call transfer / conference** from the phone app, so a human can take over.
7. **Other realtime models** behind the same call logic (OpenAI Realtime, local
   speech-to-speech models as they mature).
8. **Removing the older menu-based code paths** to make the agent easier to
   read.
9. **Signed, notarised releases** for macOS and Windows.
