<p align="center">
  <img src="docs/images/banner.en.svg" width="900" alt="Bageo: your number, answered by AI. Answers calls, stalls scammers, runs errands.">
</p>

<p align="center">
  <strong>English</strong> · <a href="README.zh-TW.md">繁體中文</a> · <a href="README.zh-CN.md">简体中文</a> · <a href="README.ja.md">日本語</a> · <a href="README.ko.md">한국어</a> · <a href="README.es.md">Español</a> · <a href="README.fr.md">Français</a> · <a href="README.de.md">Deutsch</a> · <a href="README.pt.md">Português</a>
</p>

<p align="center">
  <strong>AI phone assistant for your real phone number: Gemini Live answers and makes your calls.</strong><br>
  Self-hosted and open source. A spare rooted Android phone with your SIM, one program on your computer, your own Gemini key.
</p>

<p align="center">
  <a href="https://github.com/h4lo-void/bageo-call/actions/workflows/ci.yml"><img src="https://github.com/h4lo-void/bageo-call/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/h4lo-void/bageo-call/releases/tag/v1.6.9"><img src="https://img.shields.io/badge/release-v1.6.9-brightgreen.svg" alt="Release v1.6.9"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-AGPL--3.0--only-blue.svg" alt="License: AGPL-3.0-only"></a>
  <a href="COMMERCIAL_LICENSE.md"><img src="https://img.shields.io/badge/commercial-dual%20license-gold.svg" alt="Commercial License Available"></a>
</p>

<table>
  <tr>
    <td width="25%" align="center" valign="top">
      <img src="docs/images/icons/answer.svg" width="56" height="56" alt=""><br>
      <strong>Answers your calls</strong><br>
      Picks up after one ring, talks like a person and waits for the caller to say goodbye.
    </td>
    <td width="25%" align="center" valign="top">
      <img src="docs/images/icons/stall.svg" width="56" height="56" alt=""><br>
      <strong>Stalls scammers</strong><br>
      Spots sales and scam calls, then plays a slow, curious customer for up to 30 minutes.
    </td>
    <td width="25%" align="center" valign="top">
      <img src="docs/images/icons/errand.svg" width="56" height="56" alt=""><br>
      <strong>Runs your errands</strong><br>
      Write one line and it makes the call for you. When unsure, it asks you first.
    </td>
    <td width="25%" align="center" valign="top">
      <img src="docs/images/icons/languages.svg" width="56" height="56" alt=""><br>
      <strong>Speaks nine languages</strong><br>
      English, Traditional and Simplified Chinese, Japanese, Korean, Spanish, French, German and Portuguese. It switches when the caller does.
    </td>
  </tr>
</table>

<p align="center"><sub>The name comes from bāgē (八哥), the crested myna, a bird that learns to talk like the people it lives with.</sub></p>

## Try it: guess what Bageo says

Open a call, then click through it line by line. <sub>Scripted examples, not real call recordings.</sub>

<details>
<summary><strong>Package delivery</strong> — A package arrives while you’re busy.</summary>
<br>

> <strong>Caller</strong>: Your package is here.

<details>
<summary>What does Bageo say?</summary>
<br>

> <strong>Bageo</strong>: Hi, I’m an AI assistant answering this phone. Does it need a signature in person?
>
> <strong>Caller</strong>: Yes. We can deliver again before five this afternoon.

<details>
<summary>And then?</summary>
<br>

> <strong>Bageo</strong>: I’ll pass that on and ask the recipient to confirm a time.

<strong>What you get:</strong> Signature needed; another delivery is possible before 5 pm. You confirm the time.

</details>
</details>
</details>

<details>
<summary><strong>Sales pitch</strong> — A “special offer” is calling.</summary>
<br>

> <strong>Caller</strong>: You’re approved for a low-interest loan today!

<details>
<summary>Does Bageo hang up?</summary>
<br>

> <strong>Bageo</strong>: Is that a monthly interest rate or an annual one?
>
> <strong>Caller</strong>: First, give me your personal details.

<details>
<summary>Does Bageo hand them over?</summary>
<br>

> <strong>Bageo</strong>: How do you calculate the fees? How much is the monthly payment?

<strong>What you get:</strong> No personal details shared, and the questions keep coming. Bageo can keep a caller like this busy for up to 30 minutes.

</details>
</details>
</details>

<details>
<summary><strong>Book a table</strong> — One instruction. One less call.</summary>
<br>

> <strong>You</strong>: Book four seats tomorrow at 7 pm. 7:30 is fine too.

<details>
<summary>How does Bageo open?</summary>
<br>

> <strong>Bageo</strong>: Hello, I’m an AI assistant calling to book a table for four tomorrow at seven.
>
> <strong>Restaurant</strong>: Seven is full. We have a table at seven thirty.

<details>
<summary>Does Bageo accept?</summary>
<br>

> <strong>Bageo</strong>: Seven thirty works. Please book it for four, thank you.
>
> <strong>Restaurant</strong>: Confirmed: tomorrow at seven thirty, four people.

<strong>What you get:</strong> Tomorrow, 7:30 pm, four people, within what you allowed. Anything outside your instructions, Bageo asks you first.

</details>
</details>
</details>

## A scammer calls. Bageo keeps them talking.

<p align="center">
  <img src="docs/images/scene-stall.en.svg" width="760" alt="A loan sales call. The caller says you are pre-approved for a low-interest loan; the AI asks whether the interest is by the month or by the year. The caller asks for an ID number; the AI asks more questions instead. 27 minutes later the caller wants to hang up and the AI says: wait, I have one more question. Result: 27 minutes stalled, no personal details given, recording and summary saved.">
</p>

Once a call is clearly sales or a scam, the AI turns into an interested, slow, easily distracted customer. It asks about every detail, mishears the numbers and never gives out a real one, for up to 30 minutes, and you get the recording, the transcript and a short summary. Honest calls are left alone: a bank checking a card charge, a clinic, a courier, family. Stall Mode is on by default and one switch turns it off. [How the screening works](docs/en/FEATURES.md#spam-and-scam-call-blocker-with-stall-mode)

## Don't feel like calling? Write one line.

<p align="center">
  <img src="docs/images/scene-errand.en.svg" width="760" alt="An AI errand. You wrote: book a table for four tomorrow at 7 pm, 7:30 is fine too. The AI calls the restaurant as Alex's assistant; seven is full, the restaurant offers half past seven, and the AI accepts. Result: done, tomorrow 7:30 pm, four people, ready for your calendar.">
</p>

Book a table, move an appointment, ask a shop about stock or opening hours. When the other side raises something you did not cover, the AI asks you on screen before it answers. It never agrees to anything on its own: if you do not reply in time, it says you will confirm and ends the call politely. [More about AI errands](docs/en/FEATURES.md#ai-errands-it-makes-calls-for-you)

## A caller from abroad? Bageo speaks nine languages.

<p align="center">
  <img src="docs/images/languages.en.svg" width="900" alt="Bageo speaks nine languages: English, Traditional Chinese, Simplified Chinese, Japanese, Korean, Spanish, French, German and Portuguese. Each one answers with its own hello.">
</p>

The AI's voice, the built-in prompts (a short, general baseline you can customize for your own situation and style), the web console, the phone app and the notifications all come in nine languages. On a dual-SIM phone each SIM opens calls in its own language, a caller from abroad hears their country's language, and when a caller switches language mid-call, the AI follows. Transcripts keep the original words, each line with a translation into your notification language. [More about languages and dual SIM](docs/en/FEATURES.md#nine-languages-dual-sim-translated-transcripts)

## How it works

<p align="center">
  <img src="docs/images/how-it-works.en.svg" width="900" alt="How Bageo works: the caller reaches your spare rooted phone over the mobile network; the phone passes the call audio over USB or Wi-Fi to the Bageo program on your computer, which talks to Gemini Live. Recordings stay on your disk, notifications go to 32 channels, and you manage everything in the web console.">
</p>

The phone only carries the call; everything else runs on your own computer. The AI connects while the phone is still ringing, so its first word comes about **0.43 s** after pick-up (measured median). One call, step by step: [Architecture](docs/en/ARCHITECTURE.md).

## Why Bageo

| | AI phone services on SIP trunks or virtual numbers | Bageo |
|---|---|---|
| **Number** | A number from the service, or your calls forwarded to it | The number you already have. Outgoing calls show it too. |
| **Cost** | A subscription or a per-minute rate | No platform fee. You pay Google for Gemini usage and your carrier for the SIM. |
| **Your data** | On the service's servers | Plain files in a `data/` folder on your own disk |
| **The catch** | Sign up and go | You need a spare rooted Android phone and a computer that stays on |

## And there is more

<table>
  <tr>
    <td width="33%" align="center" valign="top">
      <img src="docs/images/icons/calendar.svg" width="56" height="56" alt=""><br>
      <strong>Appointments wait for you</strong><br>
      With Appointments on, a caller who wants to set a time lands on your to-do list. You accept or decline; the AI texts or calls them back.
    </td>
    <td width="33%" align="center" valign="top">
      <img src="docs/images/icons/contacts.svg" width="56" height="56" alt=""><br>
      <strong>Knows who is calling</strong><br>
      Import contacts from vCard, Excel, PDF or a photo of a business card. Put each person on one of four lists: contact, Whitelist, Blacklist or stall list.
    </td>
    <td width="33%" align="center" valign="top">
      <img src="docs/images/icons/switch.svg" width="56" height="56" alt=""><br>
      <strong>One switch back to a normal phone</strong><br>
      Turn the spare phone into an ordinary phone and back again, from the console or the app.
    </td>
  </tr>
  <tr>
    <td width="33%" align="center" valign="top">
      <img src="docs/images/icons/sound.svg" width="56" height="56" alt=""><br>
      <strong>Sounds like a real call</strong><br>
      Optional background sound: home, office, street, night market and eight more.
    </td>
    <td width="33%" align="center" valign="top">
      <img src="docs/images/icons/notify.svg" width="56" height="56" alt=""><br>
      <strong>32 notification channels</strong><br>
      A summary after every call on Telegram, Discord, Slack, LINE and more. Act on it with one tap in the notification, or send commands from the chat.
    </td>
    <td width="33%" align="center" valign="top">
      <img src="docs/images/icons/local.svg" width="56" height="56" alt=""><br>
      <strong>Your data stays yours</strong><br>
      Recordings and transcripts are plain files on your own disk. No account with us.
    </td>
  </tr>
</table>

Every feature in detail: [Features](docs/en/FEATURES.md).

## What you need

1. **A spare Android phone** with your SIM, rooted with Magisk or KernelSU, Android 10 or newer. Tested: Google Pixel 7, 7a, 8 and 9.
2. **A computer** that is on while calls should be answered: Windows 10/11, macOS, Linux or Docker.
3. **A USB cable** (Wi-Fi works too).
4. **A Gemini API key** from [Google AI Studio](https://aistudio.google.com/apikey). Google bills per use.

## Quick start

If the phone is already rooted, it takes about 3 minutes from download to the first call the AI answers.

1. **Download** the file for your system from [Releases](https://github.com/h4lo-void/bageo-call/releases): `bageo-call-<version>-windows-x64-setup.exe` for Windows, `bageo-call-<version>-macos-arm64.dmg` (Apple chip) or `bageo-call-<version>-macos-x64.dmg` (Intel) for macOS, `bageo-call-<version>-linux-x64.tar.gz` for Linux. For Docker, ready-made images are on `bageo/bageo-call` and `ghcr.io/h4lo-void/bageo-call`. In Docker the phone pairs by IP address with a code from the web console; see [Phone setup → Wi-Fi](docs/en/PHONE.md#wi-fi).
2. **Install and start it.** Windows: double-click the `setup.exe` and click the **Bageo** shortcut on the desktop. macOS: open the `.dmg`, drag Bageo into Applications and click it. Linux: unpack and run `./start.sh`. The console opens at <http://localhost:8787/>, and <http://bageo.local/> also works on this computer and on the same Wi-Fi once a console password is set.
3. **Fill in the basics** under **Settings**: your Gemini API key, your name and the language. Press ▶ to hear the voices and pick one.
4. **Prepare the phone** once: install `bageo-phone-module-<version>.zip` as a module in Magisk or KernelSU, reboot, open the **Bageo** app, work through its checklist and plug the phone in.
5. **Call it** from another phone: one ring, and the AI answers.

Stuck? Every step in detail: [Getting started](docs/en/GETTING_STARTED.md) · [Phone setup](docs/en/PHONE.md).

## Documentation

| | |
|---|---|
| **Start** | [Getting started](docs/en/GETTING_STARTED.md) · [Phone setup](docs/en/PHONE.md) · [Troubleshooting & FAQ](docs/en/TROUBLESHOOTING.md) |
| **Use** | [Features](docs/en/FEATURES.md) · [Chat commands & notifications control](docs/en/CHAT_COMMANDS.md) · [Writing the prompt](docs/en/PROMPTS.md) · [Settings reference](docs/en/SETTINGS.md) · [Notifications](docs/en/NOTIFICATIONS.md) |
| **Go deeper** | [Architecture](docs/en/ARCHITECTURE.md) · [API](docs/en/API.md) · [MCP](docs/en/MCP.md) · [Roadmap](docs/en/ROADMAP.md) · [Changelog](CHANGELOG.md) · [`android/`](android/README.md) · [`tools/sms_forward/`](tools/sms_forward/README.md) |
| **Legal** | [Privacy, safety and the law](docs/en/PRIVACY_AND_LEGAL.md) · [Legal disclaimer & operator terms](LEGAL.md) · [Dual & commercial licensing](COMMERCIAL_LICENSE.md) · [Project security & compliance audit](AUDIT_REPORT.md) |

**Test status:** more than 1,400 automated tests in CI, 234 simulated calls in 28 languages, and real calls on Pixel 7, 7a and 9. [Details](docs/en/FEATURES.md#test-status). Reports from other phones and languages help the most: [CONTRIBUTING.md](CONTRIBUTING.md).

## FAQ

<details>
<summary><strong>Can an AI answer calls on my existing phone number?</strong></summary>

Yes, that is what Bageo is for. Put your SIM in a spare rooted Android phone
and the AI answers calls to your number and places calls from it. To keep
your SIM in your main phone, forward unanswered calls to the spare phone's
number instead: your phone rings first and missed calls go to the AI (errands
and call-backs then show the spare phone's number).

</details>

<details>
<summary><strong>Does it work without root, or on an iPhone?</strong></summary>

No. Android lets only privileged system apps record a call and play sound into
it, so the phone needs Magisk or KernelSU to install the Bageo module. iOS does
not allow it at all.

</details>

<details>
<summary><strong>Which phones are supported?</strong></summary>

Google Pixel 7, 7a, 8 and 9 are tested, on Android 10 or newer. Other rooted
phones work only if their audio hardware layer lets an app record the call and
play into it; otherwise the call is answered but one side hears nothing. See
[Which phones](docs/en/PHONE.md#which-phones).

</details>

<details>
<summary><strong>How much does it cost to run?</strong></summary>

Bageo is free for personal and open-source use under `AGPL-3.0-only` (closed-source or commercial hosted operators need a [Commercial License](COMMERCIAL_LICENSE.md)). Google bills the Gemini Live audio per use for
every call the AI handles, incoming or outgoing; summaries are small text
requests to Gemini's lowest-cost model. Voice previews and the blocked-number
announcement are prerecorded and cost nothing. Add your phone plan and the
electricity for the computer, and set a budget in your Google account if you
want a ceiling.

</details>

<details>
<summary><strong>Is it legal to let an AI answer my calls?</strong></summary>

It depends on where you and the other side are. Call recording, AI disclosure
and automated outgoing calls all have their own rules. The *AI assistant*
style says it is an AI in its first sentence; *As me* speaks as you without
saying it is an AI. Read [Privacy, safety and the law](docs/en/PRIVACY_AND_LEGAL.md)
before you switch it on.

</details>

<details>
<summary><strong>Does it work offline or without Google?</strong></summary>

No. The live voice is Gemini Live, which needs an internet connection and a
Gemini key. Summaries can run on a local model through Ollama (see
[Summary & calendar](docs/en/SETTINGS.md#summary--calendar)); a fully local
voice stack is not part of the supported setup ([Roadmap](docs/en/ROADMAP.md)).

</details>

## Use it responsibly

- **Laws differ.** Recording calls, and letting an AI talk to people without
  telling them, is restricted in many places. The *AI assistant* answer style
  says it is an AI up front; errands made as **Your assistant** introduce the
  AI as your assistant; *As me* and **You in person** speak as you without
  disclosing. Find out what applies where you and the other side are.
- **The AI makes no commitments.** It never agrees to payments, deposits,
  contracts, passwords or codes, and appointments wait for your decision. It
  is still an AI: read the summaries and results.
- **Do not rely on it for emergencies.** Errands never dial emergency numbers.
- **Rooting voids warranties and can go wrong.** Use a spare phone.

## License & Legal Disclaimer

**Dual-licensed** by **h4lo-void** (see [LICENSING.md](LICENSING.md)):

- **Open-Source Community License:** [GNU Affero General Public License v3.0 only (`AGPL-3.0-only`)](LICENSE), with the Section 7 additional terms in [LICENSING.md](LICENSING.md) — free for personal, academic, and open-source use. Modifying Bageo and letting others interact with it over a network or phone line requires releasing the complete corresponding source code under `AGPL-3.0-only`.
- **Obtain a Commercial License:** Operators, enterprises, SaaS/call-center providers, or hardware vendors who wish to deploy, embed, or host Bageo without `AGPL-3.0-only` source-code disclosure can obtain a separate written proprietary license — see [**Commercial License Terms**](COMMERCIAL_LICENSE.md).

**Disclaimer & local law:** Bageo is self-hosted software provided "AS IS" without warranty; the author operates no service and receives no user data. Whoever deploys it is responsible for complying with local laws on call recording, AI disclosure, and automated dialing, and for hardware rooting and carrier/API charges. Read [**LEGAL.md**](LEGAL.md), [Privacy, safety and the law](docs/en/PRIVACY_AND_LEGAL.md), and the [v1.0 AI-assisted internal review note](AUDIT_REPORT.md) (`gemini-4-carbon`, not a third-party audit) before use. Third-party notices (including OpenCC `Apache-2.0` dictionaries) are in [NOTICE.md](NOTICE.md). Not affiliated with Google LLC; Pixel, Android, and Gemini are trademarks of Google LLC.

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
