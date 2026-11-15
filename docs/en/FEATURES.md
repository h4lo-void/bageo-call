# Bageo features: everything the AI phone assistant does

Every Bageo feature in detail. For the short version, go back to the [README](../../README.md).

## Hear it in action

**A loan sales call.** Answer style *As me*, with *Keep them talking* on (the
default).

> **Caller:** Good afternoon, this is Sunrise Finance. You've been
> pre-approved for a low-interest personal loan.
>
> **AI:** Oh, a loan? How does that work, is the interest by the month or by
> the year?
>
> **Caller:** By the year, only 2.8 percent. I just need your ID number to get
> started.
>
> **AI:** Sorry, 2.8 percent of what, the whole amount? And if I pay it back
> early, do I pay extra for that?

The AI keeps asking, mishears the numbers, wanders off topic and never gives
out a single detail, until the caller gives up or 30 minutes pass. You get the
recording, the transcript and a short summary.

**An AI errand.** On the *Errands* page you write: "Book a table for four
tomorrow at 7 pm. 7:30 is fine too."

> **AI:** Hello, I'm Alex's assistant. I'd like to book a table for four
> tomorrow at seven.
>
> **Restaurant:** Seven is full, I'm afraid. Would half past seven work?
>
> **AI:** Half past seven is fine. The booking is for Alex.

The result reads **Done**: tomorrow 7:30 pm, four people, with a button that
adds it to your calendar. Had the restaurant offered only 8:30, the AI would
have asked you on the console first.

## Answers your calls like a person

- **One ring, then "Hello?".** The phone rings for 3 seconds by default (up to
  8) while the agent gets the opening line ready (*ring pre-warm*).
- **Two answer styles.** *AI assistant* (the default) says it is an AI
  answering for you and takes a message. *As me* answers the way you would; it
  still admits to being an AI when someone sincerely asks.
- **Natural turn-taking.** At the start of every call the agent checks whether
  the phone's capture contains the AI's own voice. Where it does not (the
  Pixel 9 in testing), callers can cut in: about 0.8 seconds of speech stops
  the AI, a quick "mm" or "OK" does not. A silent line gets "are you still
  there?". The AI hangs up when the caller's own last words are a goodbye;
  ordinary calls wrap up politely at *Longest normal call* (5 minutes by
  default).
- **Sounds like a real phone call.** Optional background sound under the AI's
  voice, 12 kinds at three levels: home, office, street, hospital, ambulance
  passing, typhoon, thunderstorm, construction site, metro, night market,
  firecrackers, driving.
- **30 voices in 9 languages, previewed for free.** 1,080 recorded clips ship
  with the program (IMA ADPCM, a quarter of the size of plain WAV), so voice
  previews and the "number not available" announcement use no API quota.
- **Only what you wrote down.** *Notes the AI may answer from* holds facts such
  as where parcels go or your office hours; the *Status note* holds one line
  about right now ("expecting a delivery this afternoon"). Anything not written
  there, the AI must not make up.
- **Blocked and hidden numbers.** Blocked numbers are refused while ringing, or
  hear a short announcement, and never reach the AI. Callers without caller ID
  get the AI like anyone else, or the announcement if you switch *Answer hidden
  numbers* off.

## AI errands: it makes calls for you

On the **Errands** page, or with **Ask AI to call** next to a contact, pick
who to call and write what needs doing: book a table, move an appointment,
find a time with a friend, ask a shop about stock or opening hours. The AI
calls as **Your assistant** (the default) or as **You in person**.

- **Live captions** show both sides while the call runs.
- **It asks before it guesses.** When the other side raises something your
  instructions do not settle, the AI asks them to wait a moment and calls
  `ask_owner`: a question card with its options and a 40-second countdown
  appears on your screen. Tap an option or type an answer. You can also type
  an instruction at any time during the call (`say`).
- **Nothing agreed behind your back.** If you are not watching or do not
  answer in time, the AI accepts none of its own options. It asks which times
  or choices would work, says you will confirm, and closes politely. The result
  reads **Your decision needed**: pick an option or write a reply, and
  **Reply, and have the AI call again** starts a follow-up call that knows the
  earlier conversation (`follow_up_of`).
- **A clear result, one notification.** *Done*, *Partly done*, *Not done*,
  *Your decision needed*, *No answer*, *Voicemail* or *Failed*, with the
  details, the options offered and any agreed time, ready for your calendar.
- **Safe dialing.** Emergency numbers, short codes, premium-rate numbers and
  numbers written after an international call prefix are refused. The agent
  learns from incoming calls which SIM country each phone holds
  (`phone_sims.json`) and dials only on a SIM of the number's own country. If
  the phone reports a different SIM as the call starts, the agent hangs up
  before it connects (`wrong_sim`), so an errand never turns into an
  international call by mistake.
- **Sensible limits.** One call to a number at a time; at most 10 minutes per
  call, with a wrap-up at 9:30; no automatic redials (**Call again** is your
  button); voice menus waited out for up to 90 seconds; a carrier's "not in
  service" recording ends the call at once.

## Spam and scam call blocker, with Stall Mode

Three layers of call screening decide whether a caller is selling or
scamming:

1. **Follow-ups pass.** A caller following up on something *you* started (a
   complaint, a repair, an application, a claim) is let through, whatever
   words they use.
2. **Weighted keywords in every language.** Insurance, loans and credit
   increases, soft sales pitches, and newer scam patterns (ATM steps, payment
   "errors", gift cards, "add me on LINE").
3. **A second look at the meaning.** When the keywords cross the line,
   `gemini-flash-lite` reads the conversation so far, knowing whether the
   caller is in your contacts, while the AI keeps talking (at most 1.5
   seconds). A caller who has only said who they are is `too_early` and is
   looked at again after their next turn.

Honest calls are left alone: a bank checking a card charge, a clinic, a
courier collecting cash on delivery, family talking about money. When a bank
asks whether a charge was yours, the AI neither confirms nor denies it, agrees
to no blocking or reporting, notes the amount, merchant and time, and the
summary tells you to call the number on the back of your card.

A confirmed sales or scam call meets **Stall Mode** (*Keep them talking*, on by
default): an interested, slow, easily distracted customer who asks about every
detail and keeps running into problems, for up to 30 minutes (*Longest such
call*), without giving out a single real detail or agreeing to pay anything.
Switch it off and the AI declines politely and ends the call.

## Appointments become a to-do list

When a caller wants to set a time (*Appointments*, off by default), the AI does
not agree on the spot. It says it will check and text or call back, depending
on what can reach that number. After the call the request waits under **To
do** on the console (and, with a bot set through `TELEGRAM_BOT_TOKEN`, as a
Telegram card with buttons): who, what and when,
**Accept** or **Decline**, and how the caller hears your answer:

- **Text**, from the SIM that took the call. It contains the time, never the
  caller's own words.
- **AI call-back.** The AI waits through the ringback until the callee picks up
  and speaks, gives your answer once in the language of the original call,
  says goodbye and hangs up.
- **Don't tell them.**

One tap adds an accepted appointment to **Google Calendar, Outlook.com,
Microsoft 365, Apple Calendar (.ics), Yahoo Calendar or a plain .ics file**.
The agent never holds a calendar login or token.

## Contacts from almost any file

Callers in your contacts show up by name in the call list, the summary and the
notification; the AI knows who is calling and reads your note about them, and
saved contacts are protected in the scam review. Import from:

- vCard 2.1 / 3.0 / 4.0, Google and Outlook CSV, TSV and plain text, read on
  your computer with automatic encoding detection (UTF-8, UTF-16, Big5, GBK,
  Shift_JIS, EUC-KR, Windows-1252);
- tables in Excel `.xlsx` and Word `.docx` files, also read locally;
- PDFs, business cards and screenshots of an address book, read by Gemini
  Flash-Lite.

You see every entry before anything is saved; duplicates are merged or
skipped, and the whole list exports as vCard.

## Nine languages, dual SIM, translated transcripts

- **Nine languages** for the AI's voice, the built-in prompts, the web
  console, the phone app and the notifications: English, 繁體中文 (Taiwan),
  简体中文, 日本語, 한국어, Español, Français, Deutsch, Português.
- **A main language per SIM.** On a dual-SIM phone each SIM opens calls in its
  own language, and a caller from abroad hears their country's language when
  the AI speaks it. On a Taiwanese SIM, the AI waits out the carrier's
  international-call warning before speaking.
- **It follows the caller**, switching mid-call when they speak another
  language or ask whether the AI speaks one.
- **Transcripts keep the original words**: lines said in another language stay
  as they were said, each with a translation into your notification language.
- **Other names callers use for you** (`owner_aliases`), such as an English
  name or a nickname, so the AI knows they mean you.

## AI phone or normal phone, one switch

From the console's **System** page or in the Bageo app, turn the spare phone
back into an ordinary phone and back again. In normal mode Google's phone app
is the dialer again, Do Not Disturb is off, the volumes are restored and calls
ring for you to answer; the mode survives a reboot. The Magisk / KernelSU
module's root helper makes the change, so the Bageo app itself never needs root
access. See [Normal phone mode](PHONE.md#normal-phone-mode).

## 16 notification channels, data on your disk

Telegram (with Accept / Decline buttons for appointments), Discord, Slack /
Mattermost, LINE Messaging API, ntfy, Gotify, Bark, Pushover, WeCom, DingTalk
(with signing), Feishu / Lark, ServerChan, Matrix, Gmail, Apprise, and a JSON
webhook for n8n, Zapier or Home Assistant. Use any mix; every message goes to
all of them. The optional [SMS forwarder](../../tools/sms_forward/README.md) sends
the phone's incoming text messages to the same channels.

What leaves your computer: the live call audio goes to Gemini, the transcript
to the summary model (Gemini Flash-Lite on the same key by default, or any
OpenAI-compatible service including a local Ollama), and the summary to the
channels you set up. Details: [Privacy, safety and the law](PRIVACY_AND_LEGAL.md).

## Make it sound like you

The built-in prompts work out of the box. To make the AI talk the way *you* do,
let another AI interview you:

1. Open **Settings → Prompt**, find *Don't want to write it yourself?*, choose
   a language and press **Copy interview prompt** (the same texts, in all nine
   languages, are in [`docs/prompts/`](../prompts/)).
2. Paste it into ChatGPT, Claude, Gemini or any chat AI. It asks about ten
   questions, one at a time: how you talk, the words you actually say and the
   ones you never would, who calls you, what must never be agreed to, how to
   treat sales calls.
3. It returns a prompt in your own phrases, your notes, and which answer style
   and sales setting to pick. Paste, save, and the next call uses it.

More, including the techniques that make the biggest difference:
[Writing the prompt](PROMPTS.md).

## Test status

| | |
|---|---|
| **Automated** | More than 520 unit and integration tests run in CI on every change. |
| **Simulated calls** | 234 calls against the real agent, with Gemini playing callers in 28 languages, plus 15 AI errand scenarios. All pass. |
| **Real phones (1.0)** | Pixel 7, 7a and 9: answering, interruptions, background sound, sales and scam stalling, blocked numbers, appointment texts, AI call-backs, normal phone mode. |
| **Needs real-world reports** | AI errands dialing out, the call-back's ringback filter on a real ringback, notification channels other than Telegram on live accounts, other languages, the *AI assistant* style, Docker, USB on macOS and Linux. |

Reports from other phones and languages help the most: see
[CONTRIBUTING.md](../../CONTRIBUTING.md).
