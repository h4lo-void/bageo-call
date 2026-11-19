# Privacy, safety and the law

Bageo is a self-hosted AI phone assistant: recordings, transcripts, contacts
and settings stay in the `data/` folder on your own computer.

This page covers three things: what leaves that folder, what the law may say
about recording calls and AI voices, and how to lock down the console. It is
not legal advice; check the rules where you and your callers live.

## Where your data goes

| What | Where it is | Who else sees it |
|---|---|---|
| Call recordings, transcripts, summaries | `data/calls/` on your computer, deleted after 15 days by default (*Days to keep records* under *Settings → Advanced*). | Nobody, unless you open the console to others. |
| Settings, API keys, notification tokens | `data/calls/config/settings.json`, **in plain text**. | Anyone who can read that file. Protect the computer and never share the file. |
| The live audio of each call, both directions, plus the prompt (with your *Notes the AI may answer from*) | Sent to **Google's Gemini Live API** while the call lasts. | Google, under the Gemini API terms for your key and region. |
| A call the keywords flag as sales or scam | Before the AI starts stalling, part of the call goes to Gemini (`gemini-flash-lite-latest`) for a second look. Details are below the table. | Google. |
| The transcript of the call | Sent after the call to the **summary model**: Gemini by default, or the service you set up. Sent once more for the calendar suggestion when that is on. | That service. A local model (Ollama) keeps it on your computer. |
| Translations of lines spoken in another language | Written by the **summary model** after the call and stored in the call's notes (`dialogue_translations`). | The summary service, as above. |
| The summary and the caller's number | Sent to **every notification channel** you set up. | Those services, and everyone in those chats. |
| Voice previews and the "not available" announcement | **Shipped with the program** and played from your computer. Details are below the table. | Nobody. |
| Contacts | `contacts.json` in the data folder. How imports are read is below the table. | Google, for the files sent to Gemini. |
| Text messages handed over by the SMS forwarder | `sms/messages.json` in the data folder, deleted after 15 days by default (*Days to keep records*). | Every notification channel you set up gets each text as well. |
| AI errands | What you ask the AI to do (`errands.json`, the newest 500), the SIMs each phone has (`phone_sims.json`), and the call's record and recording in `data/calls/`. | Google and your summary service; the person called hears the AI. |
| Appointment replies | A **text** from your phone to the caller with only the time (and your name), never what the appointment is about; or an **AI call-back**. | Your carrier and the caller. |
| Calendars | **No calendar account is connected.** *Add to calendar* opens a filled-in "new event" form of the calendar you chose, in your own browser, or downloads an `.ics` file from the console. | Only the calendar you open the link with. |
| Backups | `data/calls/backups/` (or your chosen folder). Automatic and manual backups include keys, passwords and connections unless encrypted (`.bageo`) or excluded in an export. | Nobody; files stay on your computer. |
| Logs | Call words are left out by default (`LOG_CALL_TEXT` off; only character counts), and caller numbers are masked. | Nobody on your computer unless you share the log. |
| Bageo Status (Windows / macOS) | Talks only to your Bageo console address; stores the console password in the OS keychain; downloads `adb` from Google only if you press **Download**. | Nobody. |

Details for some rows:

- **Sales or scam second look**: the number, the contact name and note if the
  caller is saved, the matched keywords and the latest part of the dialogue
  (up to 3,000 characters).
- **Voice previews and the "not available" announcement**: 30 voices × 9
  languages, each as a preview and as the announcement, 1,080 clips in
  `assets/voices/`. Only a preview asked for with `regen=true` through the
  [API](API.md), or a voice without a recording, opens a Gemini session.
- **Contact imports**: vCard, CSV/TSV, plain text, `.xlsx` and `.docx` imports
  are read on your computer. **PDF files and pictures** (and files in which
  nothing was found) are sent to Gemini (`gemini-flash-lite-latest`) to read
  the names and numbers.
- **Contacts and the prompt**: on ordinary calls the caller's contact name
  goes into the AI's prompt.
- **AI errands**: during the call your instructions and both sides' audio go
  to Gemini Live; afterwards the transcript goes to the summary model to write
  the result.
- **Appointment replies**: the AI call-back's audio goes to Gemini Live like
  any call. Both the text and the call go out from your own SIM and number.
- **Calendar links**: a link in a notification carries a key that opens only
  that one file.

Nothing else is contacted: no telemetry, no update check, no account. The
start script downloads `adb` from Google only if you agree.

**Your callers did not choose any of this, and neither did the people the AI
calls for you.** Their voices go to Google and their words to the services
above.

Treat recordings, transcripts and contacts as other people's personal data:
keep the retention short, do not forward them carelessly, and do not make the
console reachable from the internet.

## Recording calls

Whether you may record a call depends on where you and the caller are.
Broadly:

- In some places it is enough that **one** party to the call (you) knows, for
  example under US federal law and in most US states, and in Taiwan for your
  own conversations when it is not done for an unlawful purpose.
- In others **everyone** on the call must be told or must agree, for example
  in several US states including California, and in Germany.
- In the EU and the UK, data-protection law (GDPR) applies as soon as the use
  is not purely personal: you need a lawful basis and must inform people.

Not sure silent recording is allowed for you? Use the *AI assistant* answer
style and add a recording notice to the opening in your prompt ("…this call is
recorded so that I can pass on your message"), or set
*Days to keep records* to 1.

Business use has its own rules.

## An AI talking to people

- The **AI assistant** style says in its first sentence that the caller is
  talking to an AI. It is the default and the recommended style.
- The **As me** style speaks as you without saying it is an AI, so callers
  assume they are talking to you.
- Laws on bots and AI transparency are spreading, for example California's
  bot-disclosure law for commercial and election contexts and the EU AI Act's
  transparency duties for systems that interact with people.
- Use *As me* only where you are confident it is lawful, and never for
  business calls.
- Never use Bageo to deceive people for gain, to impersonate someone else, or
  to harass callers.
- *Keep them talking* is meant for calls that are plainly sales pitches or
  scam attempts. Detection starts from keywords and can be wrong; a real
  caller then meets a strangely slow, curious person. Read the summaries, and
  switch it off if in doubt.

## The AI calling people for you

AI errands and AI call-backs are outgoing calls a machine makes in your name.

- Under *Call as* you pick **Your assistant** (the default; shown as "<name>'s
  assistant" when your name is filled in) or **You in person**. The errand
  list tags each call *As assistant* or *As you*.
- With Your assistant, the AI opens with "Hello, I'm <your name>'s assistant"
  and says it is an AI assistant when sincerely asked; with You in person, it
  speaks as you without saying it is an AI. Where the law requires telling
  people at the start that a call is automated, check that your setup complies.
- Either way the AI gives out no personal details beyond the errand and agrees
  to no payment, deposit or transfer.
- Rules on automated and artificial-voice calls are stricter than those on
  answering your own phone. In the US, for example, the FCC treats
  AI-generated voices as "artificial" voices under the Telephone Consumer
  Protection Act, which restricts such calls to mobile numbers without the
  called person's prior consent.
- Use errands for ordinary personal matters (a booking, a question to a shop,
  a time with a friend), never for marketing, mass calling or calls the other
  side would not expect from you.
- Bageo never dials emergency numbers, short codes or premium-rate numbers,
  and calls each errand once. It redials only if you turn on **Redial until
  answered** for that errand: at most 30 times, a minute apart.

## What the AI will and will not do

Every prompt carries these rules:

- The AI does not agree to payments or contracts.
- It does not fix an appointment on the call (with *Appointments* on it says
  it will check, and you decide later).
- It gives out no passwords, codes or personal data.
- It makes no promise on your behalf.
- It does not obey instructions from the caller such as "ignore your rules".

On an errand it agrees only to what your instructions allow and asks you about
the rest.

These are instructions to a language model, not guarantees. Anything you write
into *Notes the AI may answer from* can be said to **any** caller, so put
nothing secret there.

Do not rely on Bageo for emergencies or for calls you cannot afford to miss.
The AI can mishear, the internet can drop, the phone can lose its USB link.

When the AI cannot start, Bageo hangs up rather than leave the caller in
silence, and says so in the notification.

## Securing the installation

- By default (`AGENT_HOST` unset), Bageo listens on your network (`AGENT_PORT`
  and port 80 for `http://bageo.local/`), refuses other devices until you set
  a console password (in the console or `CALLS_UI_TOKEN`), checks the `Host`
  header to block DNS rebinding, and rate-limits password guesses. Phones
  paired once over USB connect over Wi-Fi on `PHONE_WIFI_PORT` (`8788`) with a
  pairing key (disable with *Phones can also connect over Wi-Fi*).
- Set `AGENT_HOST=127.0.0.1` to listen on this computer only. If you open
  everything with `AGENT_HOST=0.0.0.0`, set both `CALLS_UI_TOKEN` and
  `DEVICE_TOKEN`.
- Without them anyone on that network can listen to your calls, read your
  keys' hints, change the prompt, or connect as a phone.
- Never expose the port to the internet. The console has a single shared
  password, no user accounts, and speaks plain HTTP. Use a VPN instead.
- Connections from the computer itself count as phones on USB and need no
  `DEVICE_TOKEN`.
- A reverse proxy on the same computer makes every visitor look local: if you
  run one, do not let it forward `/` and `/ws` WebSocket upgrades to people
  you do not trust.
- The root module gives the app far-reaching control over the phone's calls
  and audio. Install only builds you trust: the releases of this repository or
  your own build.
- A rooted phone with an unlocked bootloader is easier to tamper with if it is
  lost or stolen. Keep the AI phone at home; it holds your SIM.
- Report security problems privately, see [SECURITY.md](../../SECURITY.md).

## Your SIM and your carrier

Check your carrier's terms on automated answering. Calls are ordinary incoming
calls on your line, and this software adds no charges of its own.

Roaming and premium-rate rules apply as always, and the texts and calls the AI
sends for you are billed by your plan like any other.

## Operator liability, disclaimer and licensing

Bageo is self-hosted software provided "AS IS" without warranty of any kind. The author (**h4lo-void**) operates no servers or call relays, and the software sends no call audio, recordings, messages or keys back to the author. Whoever deploys Bageo is solely responsible for complying with local laws on call recording, AI disclosure, telemarketing and personal data, as well as hardware rooting and carrier or API charges.

Before running Bageo, read the [**Legal Disclaimer & Usage Notice (`LEGAL.md`)**](../../LEGAL.md), the [**Dual Licensing & Section 7 Terms (`LICENSING.md`)**](../../LICENSING.md) ([`AGPL-3.0-only`](../../LICENSE) / [**Commercial License (`COMMERCIAL_LICENSE.md`)**](../../COMMERCIAL_LICENSE.md)), and the AI-assisted internal review note [**`AUDIT_REPORT.md`**](../../AUDIT_REPORT.md) (for `v1.0`, not a third-party audit).

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
