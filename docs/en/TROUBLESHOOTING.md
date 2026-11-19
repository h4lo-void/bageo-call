# Troubleshooting & FAQ

Your AI phone assistant rang out, hung up in silence, or an appointment text
never left? Find the message or symptom you see in the tables below: each row
says why it happens and how to fix it. The FAQ at the end answers the common
questions about iPhones, root, cost and keeping your real phone number usable.

## Look here first

1. Open the **System** page of the web console. Its checklist names the piece
   that is missing.
2. Watch Bageo's terminal window on the computer: it shows what it is doing on
   every call. (The Windows and macOS installers run Bageo in the background
   with **Bageo Status** and no terminal window; run `start.cmd -Console` on
   Windows or `./start.sh --console` on Linux to see it.) For more detail, set
   `RUST_LOG=agent=debug` in `.env` and restart. The log shows only how many
   characters were said on a call; to see the words, set `LOG_CALL_TEXT=1` too,
   and turn it off when done.
3. On the phone, open the Bageo app. The card at the top says whether it is
   connected and which mode it is in.

## Bageo does not start

| You see | Why | Fix |
|---|---|---|
| `cannot listen on 0.0.0.0:8787` or `127.0.0.1:8787 … is another copy running?` | Another copy of Bageo, or another program, already uses the port. | Close it, or set `AGENT_PORT=8790` in `.env`. |
| `The agent program was not found.` | You cloned the source and Rust is not installed, so there is nothing to run. | Install [Rust](https://rustup.rs) and run the start script again, or install Bageo from the Windows installer or macOS `.dmg` on GitHub Releases. |
| Windows: "running scripts is disabled on this system" | PowerShell blocks `.ps1` files by default. | Click the **Bageo** shortcut, or start `start.cmd` in the install folder instead of `start.ps1`; it launches PowerShell with the right option. |
| macOS: "cannot be opened because the developer cannot be verified" | The app is not notarised by Apple. | In Finder, right-click Bageo → *Open*, or press *Open Anyway* in *System Settings → Privacy & Security*. |
| The console asks for a password | A console password or `CALLS_UI_TOKEN` is set. | Type the password (the browser remembers it), or open `http://…:8787/?token=<your password>`. |

## The phone does not show up

*System → Phones* shows how far the connection got. Phones on Wi-Fi instead
of USB: see [Phone setup](PHONE.md#wi-fi).

| You see | Why | Fix |
|---|---|---|
| adb not found | Android platform-tools are missing. | Click the **Bageo Status** icon and press **Download**, let the start script download them (about 7 MB, from Google), install them yourself, or set `ADB_BIN`. |
| No phone listed | The cable or port only charges, USB debugging is off, or Windows lacks the driver. | Try another cable and port, switch on USB debugging, and on Windows install Google's USB driver if needed. |
| `unauthorized` | The phone does not trust this computer yet. | Unlock the phone and accept "Allow USB debugging?". No prompt? Developer options → *Revoke USB debugging authorisations*, then replug. |
| App not installed | The root module is not installed or not active. | See [Phone setup](PHONE.md). |
| Plugged in and linked, but not connected to Bageo | The app's service is not running, or it cannot reach the Bageo program. | Open the Bageo app, read the card at the top, check that every setup step is OK, and tap **Advanced → Test connection**. Bageo also retries starting the service every 90 seconds. |
| Connects and drops, again and again | Two different `adb` versions on the computer (Android Studio, a phone-mirroring tool, …) keep restarting each other. | Set `ADB_BIN` to one of them and close the other tool. |

## Calls are not answered, or sound wrong

| Symptom | Why | Fix |
|---|---|---|
| The phone rings on and on | The phone is in [normal phone mode](PHONE.md#normal-phone-mode) (the app card says *Normal phone*), the app's service is not running, or the phone has no connection to Bageo. Without one within 12 seconds the app does not answer at all, so the call rings until your carrier counts it as missed or sends it to voicemail. | Check *System → Phones* and the app card; switch the phone back to AI answering. |
| Picked up, then hung up at once, nothing said | The AI could not start: no Gemini key, a wrong key, an unknown model name, no internet, or *AI answers calls* is off. The call's summary says "The AI did not connect and said nothing"; the terminal shows Gemini's error. | Fix the key or model, check the internet connection, switch *AI answers calls* on. |
| The caller hears nothing, yet the transcript has AI lines | The phone cannot play into the call (unsupported phone, or the app is not a privileged app). | See the table in [`android/README.md`](../../android/README.md#troubleshooting). |
| The AI never reacts; the transcript has no caller lines | The phone delivers no call audio to the app. | Also a phone-support question: see [Which phones](PHONE.md#which-phones). |
| The AI answers itself or talks over the caller | The phone's recording contains the AI's own voice, and the per-call echo check did not catch it. | Switch off *Let the caller interrupt*. |
| The AI hangs up before the caller is done | Its prompt lets it say goodbye too early. | See the ending rule in [Writing the prompt](PROMPTS.md#writing-or-editing-it-yourself). |
| The call never ends | The caller stays on the line without a goodbye Bageo recognises. | It ends at *Longest normal call*; shorten that if it happens often. |
| A silent call ends after about half a minute | Intended: a pocket dial or a robot that only listens should not hold the line. | Nothing to fix; the timings are in the table below. |
| On a call from abroad the AI starts late, or talks over a recorded warning | Carriers in Taiwan play a fraud warning on calls from abroad. When a Taiwanese SIM takes the call, the AI waits until the line goes quiet (7.5 seconds at most) and then says its opening line once. | There is no setting for the wait. Keep the phone app [up to date](PHONE.md#updating). |
| A second caller says your line was busy | One call at a time, by design. A call that rings during a call is declined at once. | It is listed in the calls, and you get a "Missed while on another call" notification. |
| The first words come late | The AI only connects after pick-up. | Switch on *Get the AI ready while the phone rings* and keep *Ring before answering* at 3 s or more. |
| It answers in the wrong language | *Language* sets the opening line, and *Main language for each SIM* the opening on another country's SIM. | Check both, and make sure your prompt does not tie the AI to one language (for example "always answer in English"). |

Opened in the right language, then switched? That is intended: when the caller
clearly speaks another language, the AI follows. A short "hello?" alone does
not switch it. This is on by default; turn it off with *Reply in the caller's
language* under *Settings → How it answers*.

When the line goes quiet, Bageo follows fixed timings (no setting):

| Situation | What happens |
|---|---|
| The caller never says a word | The AI wraps up 25 seconds after its first words (about 27 seconds after pick-up) and hangs up. |
| The line goes quiet mid-call | After 15 seconds the AI asks "Are you still there?" (twice at most) and ends the call 25 seconds later. |
| The caller asked the AI to hold on | It asks after 45 seconds and ends after 90. |
| A recorded voice menu answers | The AI does not reply to it; Bageo hangs up after 4 seconds of quiet line. |

## Sales and scam calls

When a caller pitches a loan or claims to be "the bank's risk desk", Bageo can
keep them busy. The AI turns into a slow, curious listener who keeps asking how
it all works (*Keep them talking*).

If that catches the wrong caller, or a blocked number still gets through, see
the table:

| Symptom | Why | Fix |
|---|---|---|
| A prefix such as `080*` blocks nothing | A `*` pattern needs at least three digits before the star, not counting leading zeros (`0800*`), so one short line cannot block a whole region or every caller. | Lengthen the prefix. Write numbers the way the call list shows them; `+…` forms match only when your country code is known. |
| A real call was treated as sales or scam | The check starts from keywords, which honest callers sometimes use too, and a low-cost model takes a second look. The terminal names the words: `stall engaged … hits=[…]`. | On the call's page press *It was not* under *Sales / scam check*: the number is treated like a saved contact from then on. You can also switch off *Keep them talking*. |
| A friend or family member was kept talking | They are not saved as a contact. | Save them under *Contacts*. |

Before it starts keeping a contact talking, Bageo takes a second look: a
contact talking about money, transfers or bills is left alone. Requests like a
verification code or ATM steps still get kept talking (caller ID can be faked).

## Appointments and texts

| You see | Why | Fix |
|---|---|---|
| *To do*: "The text didn't go out: the phone is not connected" (`offline`) | The phone that took the call is not connected now. | Press **Send the text again** once it is, or pick another connected phone there. |
| "the phone app is too old" (`old_app`) | Sending texts needs module 1.0. | Update that phone ([Phone setup](PHONE.md#updating)). |
| "no mobile signal" (`no_service`), "airplane mode" (`radio_off`), "the carrier did not take the text" (`generic_failure`) | The carrier did not accept the text: no signal, airplane mode, SIM balance used up or line suspended, or the carrier refused it. | Fix that on the phone, then **Send the text again**. |
| "the phone did not respond" (`timeout`), "the phone app may not send texts" (`no_permission`), "the phone app ran into an error" (`exception`) | The app on the phone could not send. | Open the Bageo app, check its setup steps, then **Send the text again**. |
| "the number or the text was not accepted" (`bad_request`) | The phone refused the number or the message. | Check the caller's number. |
| No text option for a caller | Landlines, toll-free numbers, short codes, hidden numbers and numbers from abroad take no texts. | *Default reply* then uses an AI call-back, or tells nobody when a call-back is not allowed either ([Settings](SETTINGS.md#summary--calendar)). |
| The AI call-back said nothing | It waits for the callee to pick up and speak. Nobody within 70 seconds counts as `no_answer`. | *To do* shows how far it got. |
| A Telegram card still has buttons after you decided on the web | The buttons are removed a few seconds after the decision. | Nothing to do; pressing an old card only answers "This appointment has already been handled". |

## AI errands

An AI errand is a call the AI places for you from the *Errands* page, for
example "book a table for four on Friday at 7 pm, outdoor if possible". When
one does not go out, or comes back undecided, the page tells you why:

| The console says | Why | Fix |
|---|---|---|
| "This phone hasn't taken a call yet…" (`sim_unknown`) | Current phone apps report active SIMs automatically when connecting, so this happens with older apps or if Android has not reported the SIM yet. That way an errand never dials from another country's SIM. | Update the phone module, or call that phone once from any phone and try again. |
| "No phone has a SIM that can call this country's numbers." (`no_sim_for_country`) | Errands dial only from a SIM of the number's country. A number without `+` counts as one of your country. | Write numbers abroad with `+` and the country code. |
| "The AI can't call this number" (`not_allowed`) | Emergency numbers, short codes (7 digits or fewer) and premium-rate numbers are never dialed. Neither are numbers written with an international prefix (`00…`, `011…`) instead of `+`. | Use the `+` form, or call it yourself. |
| "No AI phone is available right now" (`no_phone`) | No phone is connected, or every phone is in normal phone mode. | Switch one back on *System → Phones*. |
| The errand stays *Queued* | The phone is on a call, or an earlier call to the same number (an errand or an appointment call-back) is still running. | Wait: it dials by itself when free. It gives up after 10 minutes (`phone_busy`), or after 25 minutes behind the same number (`number_busy`). |
| "The phone dialed from another SIM … hung up at once" (`wrong_sim`) | The SIM Bageo knew was removed or swapped. | Call that phone once so its SIMs are learned again, then **Call again**. |
| "A voice menu that needs key presses" (`keypad_menu`) | The AI cannot press keys, and waits up to 90 seconds for a person. | Call such numbers yourself, or find a direct number. |
| "Carrier message: …" (`carrier_message`) | The carrier answered with a recording (number not in service, phone off). | Check the number. |
| The AI's question never showed up | Questions show only while you watch that errand (its live view open within the last 6 seconds), and the AI waits 40 seconds. Otherwise it notes the other side's options and ends politely. | The result is *Your decision needed*, with the options to pick from for a call back. |
| *Your decision* although the AI booked something | The AI settled a question you did not answer. | Check the details; if it does not suit you, **Call again** with what to change. |

## Notifications

| Symptom | Why | Fix |
|---|---|---|
| *Send test* shows an error | The text is the service's own answer, usually a wrong token, chat ID or URL. | Follow the steps for that service in [Notifications](NOTIFICATIONS.md). |
| Test works, but no message after calls | *Send notifications* is off, or the channel is disabled. | Switch both on. |
| No summary, only the caller's first words | There is no summary service. | Set the Gemini key (used by default), or your own service under *Summary & calendar*. The terminal shows the reason if the service refused. |
| Links in notifications do not open | *Address in notifications* (*Settings → Notifications*) is not reachable from your phone, or Bageo listens only on this computer (`AGENT_HOST=127.0.0.1`). | See [Using the console from other devices](SETTINGS.md#using-the-console-from-other-devices). |
| The calendar link opens the wrong calendar | The first press of **Add to calendar** asks which calendar to use and remembers it. | Press **Pick another calendar** next to it. Apple Calendar and `.ics` links need *Address in notifications*. |
| The SMS forwarder stops reading texts after the phone restarted | Android keeps the SMS inbox locked (Direct Boot) until the phone is unlocked once with its PIN. | Unlock it once; the forwarder catches up by itself. |

With the optional SMS hook, new texts are forwarded even before the unlock. See
the [SMS forwarder](../../tools/sms_forward/README.md#troubleshooting).

## FAQ

**`bageo.local` does not open?**
Check, in this order:

- The phone or computer is on the same Wi-Fi as the computer running Bageo (a guest network usually is not).
- From another device, a console password must be set first on the computer running Bageo; until then you only see "Set a password first".
- On Windows, answer the "Allow access" prompt with *Private networks*. If you chose otherwise, open *Windows Security → Firewall & network protection → Allow an app through firewall* and tick *Private* for Bageo's `agent.exe`.
- On a VPN that sends all traffic through the tunnel, turn the VPN off and try again, or see [Using the console from other devices](SETTINGS.md#using-the-console-from-other-devices).
- Opening it by your own domain, a reverse proxy or a VPN name shows "This address can't open the console": enter that address under *Address in notifications*.
- When another program holds port 80, use `http://bageo.local:8787/`; *System → About* shows the address that works.
- Some older Android phones cannot open `.local` addresses: use the IP address or the QR code under *System → About*.

**Does it work on an iPhone, or on Android without root?**
No. Android lets only privileged system apps record a call and play sound into
it, and iOS does not allow it at all.

**Will it work on my Android phone that is not a Pixel?**
Nobody knows until someone tries: it depends on the phone's audio hardware
layer. See [Which phones](PHONE.md#which-phones).

**Can I keep using the phone normally?**
For a while, yes: switch it to [normal phone mode](PHONE.md#normal-phone-mode)
in the app or the web console, and back when you are done.

For every day, use a spare phone, in one of two ways:

- Move the SIM to the spare phone.
- Forward unanswered calls from your main number to the spare phone's number.
  Your main phone rings first and the AI takes what you miss.

Calls you dial yourself on the AI phone are never muted or taken over.

**What does it cost?**
Bageo is free for personal and open-source use under `AGPL-3.0-only`
(closed-source commercial use requires a
[Commercial License](../../COMMERCIAL_LICENSE.md)). What you pay for:

- Gemini Live audio: Google bills it by use, for incoming calls, AI call-backs
  and AI errands alike.
- The summary: a small text request to Gemini's lowest-cost model.
- A phone plan that receives calls (and sends the texts and places the calls
  the AI makes for you), and electricity.

Voice previews and the blocked-number announcement come prerecorded and cost
nothing.

**Can it run without Google, fully offline?**
Not in the supported mode: the natural, low-delay voice is Gemini Live. The
summary can use a local model (Ollama), see
[Settings](SETTINGS.md#summary--calendar). A local voice pipeline is on the
[roadmap](ROADMAP.md).

**Can it place calls or send text messages?**
Yes. **AI errands**: you write on the *Errands* page what to get done, and the
AI calls, handles it, asks you when unsure and reports back. **AI call-backs**
give a caller your answer to an appointment, and so do **texts**, sent from the
SIM the call came in on.

The optional [SMS forwarder](../../tools/sms_forward/README.md) passes texts
the phone receives on to your notifications and to the console's **SMS** page,
where you can also reply directly.

**Can it transfer a live call to my own phone?**
Yes, with *Forward to your phone* under *Settings → How it answers*. Pick
whether to **send calls straight to your phone** (via your carrier's
forward-when-busy) or have the **AI answer and reach you if urgent** (sending
an instant alert, and optionally turning on **When it's urgent, call me and
join us (trial)** so this phone calls you and merges a 3-way call), and tick
**Who can be forwarded** (*Whitelist*, *Other contacts*, or *Numbers not in your
contacts*).

**Several phones, several numbers?**
Yes: plug several phones into the same computer. They share the settings.

**Where are my recordings, and how do I delete them?**
In `data/calls/<date>/`. They are deleted after *Days to keep records*
(*Settings → Advanced*, default 15 days); delete the folder to remove everything at once.

**Is the caller told it is an AI?**
With the *AI assistant* style, in the first sentence. With *As me*, it speaks as
you without saying it is an AI. Whether the second is allowed where you live is
yours to check: see [Privacy, safety and the law](PRIVACY_AND_LEGAL.md).

## Reporting a problem

Open an issue with your system (Windows, macOS or Linux), phone model and
Android version, the Bageo version (bottom of the *System* page), what you did,
what you expected, and the relevant lines of Bageo's terminal output.
Remove phone numbers and keys first, and never post `settings.json`.

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
