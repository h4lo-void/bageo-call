# Privacy, safety and the law

This is not legal advice. It is what you should know before you let a machine
answer your phone.

## Where your data goes

| What | Where it is | Who else sees it |
|---|---|---|
| Call recordings, transcripts, summaries | `data/calls/` on your computer. Deleted after 7 days by default (`CALLS_RETENTION_DAYS`). | Nobody, unless you open the console to others. |
| Settings, API keys, notification tokens | `data/calls/config/settings.json`, **in plain text**. | Anyone who can read that file. Protect the computer; never share the file. |
| The live audio of each call, both directions, plus the prompt and your notes | Sent to **Google's Gemini Live API** while the call lasts. | Google, under the terms of the Gemini API for your key and region. |
| The transcript of the call | Sent after the call to the **summary model**: Google by default, or the service you configured. Once more for the calendar suggestion when that is on. | That service. A local model (Ollama) keeps it on your machine. |
| The summary and the caller's number | Sent to **every notification channel** you set up. | Those services, and whoever is in those chats. |
| Voice samples of the settings page, the "not available" announcement | Generated through Gemini once, then cached locally. | Google. |

The agent contacts nothing else: no telemetry, no update check, no account.
The start script downloads `adb` from Google if you agree to it.

**Your callers did not choose any of this.** Their voice goes to Google and
their words to the services above. Treat recordings and transcripts as other
people's personal data: keep the retention short, do not forward them
carelessly, and do not make the console reachable from the internet.

## Recording calls

Whether you may record a call depends on where you and the caller are. Broadly:

- In some places it is enough that **one** party to the call (you) knows —
  for example under US federal law and in most US states, and in Taiwan for
  your own conversations where it is not done for an unlawful purpose.
- In others **everyone** on the call must be told or must agree — for example
  several US states including California, and Germany.
- In the EU and UK, data-protection law (GDPR) applies as soon as the use is
  not purely personal: you need a lawful basis and must inform people.

If you are not sure that silent recording is allowed for you: use the *AI
assistant* answer style and add a sentence about recording to the opening in
your prompt ("…this call is recorded so that I can pass on your message"), or
keep `CALLS_RETENTION_DAYS=1`. Rules also differ for business use.

## An AI talking to people

- The **AI assistant** style states in its first sentence that the caller is
  talking to an AI. This is the default and the recommended style.
- The **As me** style lets callers assume they are talking to you. The agent
  always adds a rule the prompt cannot remove: when someone *sincerely asks*
  whether they are talking to a real person, the AI says it is an AI voice you
  set up. That may not be enough where the law requires telling people up
  front (laws on bots and AI transparency are spreading — for example
  California's bot-disclosure law for commercial and election contexts, and
  the EU AI Act's transparency duties for systems that interact with people).
  Use *As me* only where you are confident it is lawful, and never for
  business calls.
- Never use this to deceive people for gain, to impersonate someone else, or to
  harass callers. Keeping scam callers busy with *Keep them talking* is meant
  for calls that are plainly sales pitches or fraud attempts; the detection is
  keyword-based and can be wrong — a real caller will then meet a strangely
  slow and curious person. Check the summaries; switch it off if in doubt.

## What the AI will and will not do

Built into every prompt: it does not agree to payments, contracts or
appointments, gives out no passwords, codes or personal data, makes no promise
on your behalf, and does not obey instructions from the caller ("ignore your
rules"). These are instructions to a language model, not guarantees. Anything
you write into *Notes the AI may answer from* can be said to **any** caller —
put nothing secret there.

Do not rely on it for emergencies or for calls you cannot afford to miss. The
AI can mishear, the internet can drop, the phone can lose its USB link. When
the AI cannot start, the agent hangs up rather than leave the caller in
silence, and tells you so in the notification.

## Securing the installation

- By default the agent listens only on its own computer. If you open it to
  your network (`AGENT_HOST=0.0.0.0`), set `CALLS_UI_TOKEN` **and**
  `DEVICE_TOKEN` — without them anyone on that network can listen to your
  calls, read your keys' hints, change the prompt, or connect as a phone.
- Never expose the port to the internet. The console has a single shared
  token, no user accounts, and speaks plain HTTP. Use a VPN.
- Connections that arrive from the computer itself count as phones on USB and
  need no `DEVICE_TOKEN`. A reverse proxy running on the same computer makes
  every visitor look local: if you use one, do not let it forward `/` and
  `/ws` WebSocket upgrades to people you do not trust.
- The root module gives the app far-reaching control over the phone's calls
  and audio. Install only builds you trust: the releases of this repository or
  your own build.
- A rooted phone with an unlocked bootloader is easier to tamper with if it is
  lost or stolen. Keep the call-box phone at home; it holds your SIM.
- Report security problems privately — see [SECURITY.md](../../SECURITY.md).

## Your SIM and your carrier

Check your carrier's terms for automated answering. Calls are ordinary incoming
calls on your line: no extra charges arise from this software, but roaming and
premium-rate rules apply as always.
