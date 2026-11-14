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
| Translations of transcript lines spoken in another language | Made by the **summary model** after the call, stored in the call's notes (`dialogue_translations`). | The summary service, as above. |
| Voice previews and the "not available" announcement | **Shipped with the program**: 30 voices × 9 languages, each recorded as a preview and as the announcement — 1,080 clips in `assets/voices/`, played straight from your computer. Only *regenerate* or a voice without a recording opens a Gemini session. | Nobody. |
| Contacts | `contacts.json` in the data folder. vCard, CSV/TSV, plain text, `.xlsx` and `.docx` files you import are read on your computer. **PDF files and pictures** (and files in which nothing was found) are sent to Gemini (`gemini-flash-lite-latest`) to read the names and numbers. A caller's contact name goes into the AI's prompt on ordinary calls. | Google, for the files sent to Gemini. |
| AI errands | What you ask the AI to get done (`errands.json`, the newest 500), the SIMs each phone has (`phone_sims.json`), the call's record and recording in `data/calls/`. During the call your instructions and both sides' audio go to Gemini Live; afterwards the transcript goes to the summary model to write the result. | Google (and your summary service); the person called hears the AI. |
| Appointment replies | A **text** from your phone to the caller with only the time (and your name), never what the appointment is about; an **AI call-back** whose audio goes to Gemini Live like any call. Both go out from your own SIM and number. | Your carrier and the caller. |
| Calendars | **No calendar account is connected.** *Add to calendar* opens a filled-in "new event" form of the calendar you chose in your own browser, or downloads an `.ics` file from the console; a link in a notification carries a key that opens only that one file. | Only the calendar you open the link with. |

The agent contacts nothing else: no telemetry, no update check, no account.
The start script downloads `adb` from Google if you agree to it.

**Your callers did not choose any of this, and neither did the people the AI
calls for you.** Their voice goes to Google and their words to the services
above. Treat recordings, transcripts and contacts as other people's personal
data: keep the retention short, do not forward them carelessly, and do not make
the console reachable from the internet.

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

## The AI calling people for you

AI errands and AI call-backs are outgoing calls made by a machine in your name.

- With *Call as* **Your assistant** (the default) the AI says it is calling on
  your behalf. With **You in person** it speaks as you. In both cases it admits
  to being an AI when sincerely asked, gives out no personal details beyond the
  errand, and agrees to no payment, deposit or transfer.
- Rules on automated and artificial-voice calls are stricter than those on
  answering your own phone. In the US, for example, the FCC treats AI-generated
  voices as "artificial" voices under the Telephone Consumer Protection Act,
  which restricts such calls to mobile numbers without the called person's
  prior consent. Use errands for ordinary personal matters — a booking, a
  question to a shop, a time with a friend — never for marketing, mass calling
  or calls the other side would not expect from you.
- The agent will not dial emergency numbers, short codes or premium-rate
  numbers, and calls each errand once; it never redials on its own.

## What the AI will and will not do

Built into every prompt: it does not agree to payments or contracts, does not
fix an appointment on the call (with *Appointments* on it says it will check and
you decide later), gives out no passwords, codes or personal data, makes no
promise on your behalf, and does not obey instructions from the caller ("ignore
your rules"). On an errand it agrees only to what your instructions allow and
asks you about the rest. These are instructions to a language model, not
guarantees. Anything
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
