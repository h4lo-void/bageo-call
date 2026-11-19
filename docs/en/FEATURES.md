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
  8) while Bageo gets the opening line ready. Change *Ring before answering* under
  **Settings → How it answers**; *Get the AI ready while the phone rings* sits
  under *More options* there.
- **Two answer styles.** *AI assistant* (the default) says it is an AI
  answering for you and takes a message. *As me* answers the way you would,
  without agreeing to anything for you. Choose it under **Settings → Basics**
  (*Answer style*).
- **Callers can cut in.** At the start of every call Bageo checks whether the
  phone's capture contains the AI's own voice. Where it does not (the Pixel 9 in
  testing), about 0.8 seconds of speech stops the AI; a quick "mm" or "OK" does
  not. The setting is *Let the caller interrupt*, under *More options* in
  **How it answers**.
- **No hanging up on a whim.** A silent line gets "are you still there?". The AI
  hangs up when the caller's own last words are a goodbye; ordinary calls wrap up
  politely at *Longest normal call* (5 minutes by default) in **How it answers**.
- **Ends the way a person would.** Bageo recognizes "I won't keep you" and goodbye words across 25 official Gemini Live languages plus Cantonese (26 word sets in all, including Italian, Turkish, Arabic, Hindi and Russian), and hangs up only once the caller has said goodbye (Cantonese is only listened for; the AI answers in Mandarin). A greeting that also means goodbye (Korean 수고하십니다) never ends a call at the start, and an "I see" in another language (なるほど, Genau, Vale) does not interrupt the AI. Each region has its own wait before hanging up, about 1.2 to 2.5 seconds; a wait you set yourself wins ([Settings](SETTINGS.md)). AI errands end as before.
- **Speaks like the place the SIM is from.** Each call carries only the local habits of the SIM's region: where couriers leave parcels, how times and numbers are repeated, which scams are common. The region only sets the language the AI opens in; a caller in another language Gemini supports is answered in theirs. Its Chinese is always Mandarin: Taiwanese, Hakka, Cantonese and other dialects Gemini does not speak are answered in Mandarin, never put on. See [Prompts](PROMPTS.md).
- **Knows what the call is about.** A courier, a bank checking a charge, an important notice, setting a time, family or friend chat, a wrong number: each gets one short hint in the pause after the AI finishes a sentence, so replies stay as fast. A bank hears the amount, shop and time repeated back, gets neither a yes nor a no and no card block, and the AI ends with calling the official line itself later. A wrong number never hears your name.
- **Sounds like a real phone call.** Optional background sound under the AI's
  voice, 12 kinds at three levels: home, office, street, hospital, ambulance
  passing, typhoon, thunderstorm, construction site, metro, night market,
  firecrackers, driving. Pick *Background sound* and *Background volume* in
  **How it answers**.
- **30 voices in 9 languages, previewed for free.** 1,080 recorded clips ship
  with Bageo (IMA ADPCM, a quarter of the size of plain WAV), so voice
  previews and the "number not available" announcement use no API quota.
- **Only what you wrote down.** *Notes the AI may answer from* (in **Prompt**)
  holds facts such as where parcels go or your office hours; the *Status note* (in
  **How it answers**) holds one line about right now ("expecting a delivery this
  afternoon"). Anything not written there, the AI must not make up.
- **Blocked and hidden numbers.** Numbers on the **Blacklist** in **Contacts**
  are refused while ringing, or hear a short announcement, and never reach the
  AI. A Blacklist number ending in `*` covers every number that starts that way.
  Callers without caller ID get the AI like anyone else; switch *Answer hidden
  numbers* off (in **How it answers**) and they hear the announcement instead.

## AI errands: it makes calls for you

AI errands make phone calls for you: book a table, move an appointment, find a
time with a friend, ask a shop about stock or opening hours.

On the **Errands** page, or with **Ask AI to call** next to a contact, pick who
to call and write what needs doing. The AI calls as **Your assistant** (the
default) or as **You in person**.

- **Live captions** show both sides while the call runs.
- **It asks what is missing before it dials.** Within a second of starting, Bageo sees what the task lacks (for a table: how many people, which phone number to leave, the booking name) and asks at most 3 questions. Pick **Add and call** or **Call anyway**. It also notes the order you would give way in (if 7:30 is full, 8:30) and opens with the polite phrases of the place you call. A confirmation card in your chat lists the same questions: reply to answer, reply "cancel" to dial nothing.
- **It asks before it guesses.** When the other side raises something your
  instructions do not settle, the AI asks them to wait a moment and calls
  `ask_owner`: a question card with its options and a 40-second countdown
  appears on your screen. Tap an option or type an answer. You can also type
  an instruction at any time during the call (`say`).
- **Nothing agreed behind your back.** If you are not watching or do not
  answer in time, the AI accepts none of its own options. It asks which times
  or choices would work, says you will confirm, and closes politely.
- **"Your decision needed": reply, then call again.** The result reads **Your
  decision needed**. Pick an option or write a reply, and **Reply, and have the
  AI call again** starts a follow-up call that knows the earlier conversation
  (`follow_up_of`).
- **A clear result, one notification.** *Done*, *Partly done*, *Not done*,
  *Your decision needed*, *No answer*, *Voicemail* or *Failed*, with the
  details, the options offered and any agreed time, ready for your calendar.
- **Redial until answered.** For a restaurant or shop whose line is always
  busy, turn this on when you start the errand. If nobody picks up or the line
  is busy, the AI calls again every minute, up to 30 times. It leaves no
  voicemail message, and you get one notification at the end.
- **Safe dialing.** Emergency numbers, short codes, premium-rate numbers and
  numbers written after an international call prefix are refused, so an errand
  never turns into an international call by mistake.
- **The right SIM.** Phone app 1.4 and later reports its active SIMs automatically on connect and when SIMs change (`phone_sims.json`; older apps learn them from incoming calls), and errands dial only on a SIM of the number's own country. If the phone reports a different SIM as the call starts, Bageo hangs up before it connects (`wrong_sim`).
- **Sensible limits.** One call to a number at a time; at most 10 minutes per
  call, with a wrap-up at 9:30; no automatic redials unless you turn on
  **Redial until answered** (otherwise **Call again** is your button); voice
  menus waited out for up to 90 seconds; a carrier's "not in service"
  recording ends the call at once.

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
   seconds).

A caller who has only said who they are is `too_early` and is looked at again
after their next turn.

To run this check on every call, turn on *Check every call for sales and scams*
under **Settings → Sales & scam calls**: new tricks are caught too, for a little
more usage.

Honest calls are left alone: a bank checking a card charge, a clinic, a
courier collecting cash on delivery, family talking about money.

When a bank asks whether a charge was yours, the AI neither confirms nor denies
it, agrees to no blocking or reporting, notes the amount, merchant and time, and
the summary tells you to call the number on the back of your card.

A confirmed sales or scam call meets **Stall Mode**: an interested, slow,
easily distracted customer who asks about every detail and keeps running into
problems, for up to 30 minutes, without giving out a single real detail or
agreeing to pay anything.

Stall Mode follows the kind of call. Investment, insurance, loan, fake official, bank or store, debt collection and harassment calls each have their own script: the AI sounds keen first, then runs into one problem after another, and when the caller wants to hang up it says "wait, one more question". A debt collector never gets an admission of debt. The scam word list also covers Taiwan "cancel the installment" ATM calls, Japan ATM tax refunds, Korean prosecutor "safe account" calls, US IRS and Medicare gift-card calls, and European customs fee links and TAN codes.

Under **Settings → Sales & scam calls**, *Keep them talking* is on by default and
*Longest such call* sets the time limit. Switch it off and the AI declines
politely and ends the call.

## Appointments become a to-do list

When a caller wants to set a time, the AI does not agree on the spot. It says it
will check and text or call back, depending on what can reach that number. This
is the *Appointments* setting, off by default; turn it on under **Settings →
Summary & calendar**.

After the call the request waits under **To do** on the console: who, what and
when, and you press **Accept** or **Decline**. With a bot set through
`TELEGRAM_BOT_TOKEN` it also arrives as a Telegram card with buttons.

You choose how the caller hears your answer:

- **Text**, from the SIM that took the call. It contains the time, never the
  caller's own words.
- **AI call-back.** The AI waits through the ringback until the callee picks up
  and speaks, gives your answer once in the language of the original call,
  says goodbye and hangs up.
- **Don't tell them.**

One tap adds an accepted appointment to **Google Calendar, Outlook.com,
Microsoft 365, Apple Calendar (.ics), Yahoo Calendar or a plain .ics file**.
Bageo never holds a calendar login or token.

## Contacts from almost any file

Contacts let Bageo know who is calling: they show up by name in the call list,
the summary and the notification, the AI reads your note about them, and saved
contacts are protected in the scam review. Import from:

- vCard 2.1 / 3.0 / 4.0, Google and Outlook CSV, TSV and plain text, read on
  your computer with automatic encoding detection (UTF-8, UTF-16, Big5, GBK,
  Shift_JIS, EUC-KR, Windows-1252);
- tables in Excel `.xlsx` and Word `.docx` files, also read locally;
- PDFs, business cards and screenshots of an address book, read by Gemini
  Flash-Lite.

You see every entry before anything is saved; duplicates are merged or
skipped, and the whole list exports as vCard.

### Four contact lists and whitelist call transfer

Every contact can be assigned to one of four lists (from the Contacts page, via one-tap buttons on call summary notifications, or via chat commands `/block`, `/stall`, `/white`, `/unblock`):

- **Contacts**: the AI knows who is calling and reads your note about them.
- **Whitelist (`whitelist`)**: close friends, family and clients, who can be put through to your phone (turn on *Turn on forwarding* in **Settings → How it answers**; the Contacts page can set it too). It is off by default, so the AI answers as usual and tells you afterwards, and a confirmation box comes first when you turn it on. Once it is on, choose *Who can be forwarded* (**Whitelist** by default, plus optional **Other contacts** and **Numbers not in your contacts**, the latter at most one call to you every 30 minutes); Blacklist, stall-list and confirmed sales or scam calls are never forwarded. Two ways:
  1. *AI answers, puts you through if urgent*: the AI answers first. If the caller says it is urgent (in any of the 9 languages), the AI asks them to hold and this phone calls you (background sound pauses while dialing); once you pick up you talk to them directly, and Bageo sends you an urgent alert at once. Without your number you only get the alert. The summary records whether you were put through, could not be reached, or only got an alert.
  2. *Don't answer: send to your phone*: this phone doesn't answer; your carrier forwards the call to your phone, the AI stays silent, and you get a notification at once. First switch the phone to normal phone and dial `**67*your number#` to turn on forward-when-busy, or callers reach voicemail.
- **Blacklist (`blacklist`)**: turned away while ringing, or after a short "not available" announcement, without using the AI; the **Blacklist** tab sets which. A number ending in `*` (`0800*`, at least three digits not counting leading zeros) covers every number that starts that way.
- **Stall list (`stall`)**: known debt collectors, sales callers or harassers. The AI starts wasting their time the moment the call connects, without waiting for keywords.

## Nine languages, dual SIM, translated transcripts

- **Nine languages** for the AI's voice, the built-in prompts, the web
  console, the phone app and the notifications: English, 繁體中文 (Taiwan),
  简体中文, 日本語, 한국어, Español, Français, Deutsch, Português.
- **A main language per SIM.** On a dual-SIM phone each SIM opens calls in its
  own language, and a caller from abroad hears their country's language when
  the AI speaks it. Set it under **Settings → Basics → More options**, *Main
  language for each SIM*.
- **Abroad calls wait for the warning.** On a Taiwanese SIM, the AI waits out the
  carrier's international-call warning before speaking.
- **It follows the caller**, switching mid-call when they speak another
  language or ask whether the AI speaks one. This is on by default; turn it
  off with *Reply in the caller's language* under **Settings → How it answers**.
- **Transcripts keep the original words**: lines said in another language stay
  as they were said, each with a translation into your notification language.
- **Other names callers use for you** (`owner_aliases`), such as an English
  name or a nickname, so the AI knows they mean you. Set them under **Settings →
  Basics → More options**.

## AI phone or normal phone, one switch

From the console's **System** page or in the Bageo app, turn the spare phone
back into an ordinary phone and back again.

In normal mode Google's phone app is the dialer again, Do Not Disturb is off,
the volumes are restored and calls ring for you to answer. The mode survives a
reboot.

The Magisk / KernelSU module's root helper makes the change, so the Bageo app
itself never needs root access. See [Normal phone mode](PHONE.md#normal-phone-mode).

## 32 notification channels, data on your disk

Pick notification channels under **Settings → Notifications**. Use any mix; every
message goes to all of them. Bageo supports these 32:

- **Personal chat apps:** Telegram (with Accept / Decline buttons for appointments), Discord, LINE Messaging API, Signal (through signal-cli-rest-api), Matrix.
- **Team chat tools:** Slack / Mattermost, Microsoft Teams, Google Chat, Rocket.Chat, Zulip, Webex, Nextcloud Talk, Synology Chat.
- **Phone push:** ntfy, Gotify, Bark, Pushover, Pushbullet, Chanify.
- **Business and Chinese-market services:** WeCom, DingTalk (with signing), Feishu / Lark, ServerChan, PushPlus, PushDeer, WxPusher.
- **Email and automation:** Gmail, email through any SMTP server, Home Assistant, IFTTT, Apprise, and a JSON webhook for n8n or Zapier.

The optional [SMS forwarder](../../tools/sms_forward/README.md) sends the phone's
incoming text messages to the same channels.

While the forwarder runs, those texts also show up on the console's **SMS** page, one
conversation per number like your phone's messages app (with the contact's name when
you have one). You can reply right there: by default the reply goes out from the phone
and SIM that received the text, and you can pick another number. You can search, copy
and delete them. Like the other records they are kept for 15 days; change that with
*Days to keep records* in the settings.

Built-in **Backup & restore** lets you schedule automatic backups, run manual
backups and selectively export or import settings, keys, contacts and call
records (with optional `.bageo` password encryption). On Windows and macOS,
the **Bageo Status** tray app runs Bageo in the background and shows desktop
notifications for incoming calls, text messages and ready call summaries.

Only three things leave your computer:

- **Live call audio** goes to Gemini.
- **The transcript** goes to the summary model (Gemini Flash-Lite on the same key
  by default, or any OpenAI-compatible service including a local Ollama).
- **The summary** goes to the notification channels you set up.

Details: [Privacy, safety and the law](PRIVACY_AND_LEGAL.md).

## Make it sound like you

The built-in prompts are a short general baseline that works out of the box. To
tailor the AI to your own situation and the way *you* talk, let another AI
interview you:

1. Open **Settings → Prompt**, find *Don't want to write it yourself?*, choose
   a language and press **Copy interview prompt** (the same texts, in all nine
   languages, are in [`docs/prompts/`](../prompts/)).
2. Paste it into ChatGPT, Claude, Gemini or any chat AI. It asks about eight
   questions, one at a time: your personality and phone manner, everyday
   phrases and ones you never use, who calls you, your boundaries, and how you
   brush off unwanted sales calls.
3. It returns a finished prompt in your own style, ready to paste into *Your
   own prompt*. Save, and the next call uses it.

More, including the techniques that make the biggest difference:
[Writing the prompt](PROMPTS.md).

## Test status

| | |
|---|---|
| **Automated** | More than 1,400 unit and integration tests run in CI on every change. |
| **Simulated calls** | 234 calls against Bageo itself, with Gemini playing callers in 28 languages, plus 15 AI errand scenarios. All pass. |
| **Real phones** | Pixel 7, 7a and 9: answering, interruptions, background sound, sales and scam stalling, blocked numbers, appointment texts, AI call-backs, normal phone mode (including Android 15+), SIM change reporting and urgent 3-way call merge. |
| **Needs real-world reports** | AI errands dialing out, the call-back's ringback filter on a real ringback, notification channels other than Telegram on live accounts, other languages, the *AI assistant* style, Docker, USB on macOS and Linux. |

Reports from other phones and languages help the most: see
[CONTRIBUTING.md](../../CONTRIBUTING.md).

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
