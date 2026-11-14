# Troubleshooting & FAQ

Start with the **System** page of the web console: its checklist names the
piece that is missing. The agent's terminal window shows what it is doing;
`RUST_LOG=agent=debug` in `.env` shows more.

## The agent does not start

| What you see | What to do |
|---|---|
| `cannot listen on 127.0.0.1:8787 … is another copy running?` | Another agent (or another program) uses the port. Close it, or set `AGENT_PORT=8790` in `.env`. |
| "The agent program was not found" | You cloned the source without Rust installed. Install [Rust](https://rustup.rs) and run the start script again, or use a release package. |
| Windows: "running scripts is disabled" | Use `start.cmd` (it starts PowerShell with the right option), not `start.ps1` directly. |
| macOS: "cannot be opened because the developer cannot be verified" | Right-click → *Open*, or allow it in *System Settings → Privacy & Security*, or run `xattr -dr com.apple.quarantine .` in the folder. |
| The page asks for an access token | A `CALLS_UI_TOKEN` is set: type it in (the browser remembers it), or open `http://…:8787/?token=<your token>`. |

## The phone does not show up

*System → Phones* tells you how far it got.

| What it says | What to do |
|---|---|
| adb not found | Let the start script download platform-tools, install them yourself, or set `ADB_BIN`. |
| No phone listed | Cable or port (try another; some cables only charge). USB debugging off. On Windows the phone may need Google's USB driver. |
| `unauthorized` | Unlock the phone and accept "Allow USB debugging?". If the prompt does not appear: Developer options → *Revoke USB debugging authorisations*, replug. |
| App not installed | The module is not installed or not active — see [Phone setup](PHONE.md). |
| Plugged in, link set, but not connected | Open the Bageo app: what does the card at the top say, and is every setup step OK? Tap **Advanced → Test connection**. The agent retries starting the service every 90 seconds. |
| Connects and drops again and again | Two different `adb` versions on the computer (Android Studio, a phone-mirroring tool, …) keep restarting each other. Set `ADB_BIN` to one of them and close the other tool. |

## Calls are not answered, or sound wrong

| Symptom | Likely cause |
|---|---|
| The phone rings on and on | The phone is in [normal phone mode](PHONE.md#normal-phone-mode) (the app's card says *Normal phone*), the app's service is not running, or the phone has no connection to the agent: without one within 12 seconds the app does not answer at all, and the call rings until your carrier takes it as missed or to voicemail. Check *System → Phones* and the app's card. |
| Picked up, then hung up at once, nothing said | The AI could not start: no Gemini key, a wrong key, an unknown model name, no internet, or *AI answers calls* off in the console. The call record's summary says "the AI did not connect"; the terminal shows Gemini's error. |
| The caller hears nothing, the AI seems to answer (transcript has AI lines) | The phone cannot play into the call (unsupported phone, or the app is not a privileged app). See the table in [`android/README.md`](../../android/README.md#troubleshooting). |
| The AI never reacts to the caller (transcript has no caller lines) | The phone delivers no call audio to the app — again a phone-support question. |
| The AI answers itself / talks over the caller | The phone's recording contains the AI's own voice and the echo check did not catch it. Switch off *Let the caller interrupt*. |
| The AI hangs up too early | Its prompt lets it say goodbye before the caller is done. See the ending rule in [Writing the prompt](PROMPTS.md#writing-or-editing-it-yourself). |
| The call never ends | The caller never said a goodbye the agent recognises and stayed on the line; it ends at *Longest normal call*. Shorten that if it happens often. |
| A silent call ends after about half a minute | Intended. When the caller never says a word, the AI wraps up 25 seconds after its first words (about 27 seconds after pick-up) and hangs up, so a pocket dial or a robot that only listens does not hold the line. When the line goes quiet in the middle of a call, the AI asks "still there?" after 15 seconds (twice at most) and ends 25 seconds later; when the caller asked it to hold on, it waits 45 seconds before asking and 90 before ending. A recorded voice menu gets no answer: the agent hangs up after 4 seconds of quiet line. |
| On a call from abroad the AI starts late, or talks over a recorded warning | Taiwan's carriers play a fraud warning on calls from abroad. When a Taiwanese SIM takes the call, the AI waits until the line goes quiet (7.5 seconds at most) and then says its opening line once. There is no setting for it. On a phone app older than 1.0 (0.2.4 and earlier) the agent cannot tell which SIM took the call and goes by the number's country code ([`COUNTRY_CODE`](SETTINGS.md#country-calling-code)). |
| A second caller says your line was busy | Intended: one call at a time. A call that rings during a call is declined at once, listed in the calls and announced with "📵 Missed while on another call". |
| A blocked prefix such as `080*` does not block anything | A `*` pattern needs at least four digits before the star (`0800*`), so one short line cannot block a whole region or every mobile. Write the number the way the call list shows it; `+…` forms match only when your country code is known. |
| First words come late | Switch on *Get the AI ready while the phone rings* and keep *Ring before answering* at 3 s or more. |
| It answers in the wrong language | *Language* sets the opening line (and *SIM languages* the opening on another SIM). After that the AI follows the caller once they clearly speak another language; a short "hello?" alone does not switch it. |
| A real call was treated as sales / scam | Switch off *Keep them talking*, or tell us which words triggered it (the agent's terminal output names them: `stall engaged … hits=[…]`). |
| A friend or family member was treated as sales / scam | Save them as a contact: a contact talking about money, transfers or bills is left alone by the second look the agent takes before stalling. |

## Appointments and texts

| Symptom | What to do |
|---|---|
| *To do* says "Text not sent: the phone is not connected" (`offline`) | The phone that took the call is not connected now. Press **Send the text again** once it is, or pick another connected phone there. |
| "The phone app is too old to send texts" (`old_app`) | Install module 1.0 on that phone ([Phone setup](PHONE.md#updating)). |
| "error code generic_failure" / `no_service` / `radio_off` | The carrier did not take the text: no signal, airplane mode, or a prepaid SIM without credit (a common cause: the SIM's balance ran out). Fix that on the phone, then **Send the text again**. |
| No text option for a caller | Landlines, toll-free, short codes, hidden numbers and numbers from abroad take no texts; *Default reply* then uses an AI call-back, or tells nobody when a call-back is not allowed either (see [Settings](SETTINGS.md#summary--calendar)). |
| The AI call-back said nothing | It waits for the callee to pick up and speak; nobody within 70 seconds counts as `no_answer`. *To do* shows how far it got. |
| A Telegram card still has buttons after you decided on the web | The buttons are taken off a few seconds after the decision; a press on an old card only answers "This appointment has already been handled". |

## AI errands

| What the console says | Why, and what to do |
|---|---|
| *This phone hasn't taken a call yet* (`sim_unknown`) | The agent learns each phone's SIMs — which subscription id belongs to which country — from the calls the phone reports, so that an errand never dials a number of one country from another country's SIM (an expensive international call, or no connection at all). A phone that has not taken a call since it was set up cannot be used yet: call it once from any phone, then try again. |
| *No phone has a SIM that can call this country's numbers* (`no_sim_for_country`) | Errands dial only from a SIM of the number's country. Write the number with `+` and its country code if it is abroad; without `+` it counts as a number of your country. |
| *The AI can't call this number* (`not_allowed`) | Emergency numbers, short codes (7 digits or fewer) and premium-rate numbers are never dialed, nor numbers written with an international prefix (`00…`, `011…`) instead of `+`. |
| *No AI phone is available right now* (`no_phone`) | No phone is connected, or every phone is in normal phone mode. Switch one back on *System → Phones*. |
| The errand stays *Queued* | The phone is on a call, or an earlier call to the same number (an errand or an appointment call-back) is still running. It dials by itself when free; it gives up after 10 minutes (`phone_busy`) or 25 minutes behind the same number (`number_busy`). |
| *The phone dialed from another SIM… hung up at once* (`wrong_sim`) | The SIM the agent knew is gone or was swapped. Call that phone once so its SIMs are learned again, then **Call again**. |
| *A voice menu that needs key presses* (`keypad_menu`) | The AI cannot press keys. It waits up to 90 seconds for a person; call such numbers yourself, or find a direct number. |
| *Carrier message: …* (`carrier_message`) | The carrier answered with a recording (number not in service, phone off). Check the number. |
| The AI's question never showed up on the page | Questions show only while you are watching that errand (its live view open and polled in the last 6 seconds) and wait 40 seconds. Otherwise the AI notes the other side's options and ends politely; the result is *Your decision needed*, with the options to pick from for a call back. |
| The result says *Your decision* although the AI booked something | The AI settled a question you did not answer. Check the details and, if it does not suit you, **Call again** with what to change. |

## Notifications

| Symptom | What to do |
|---|---|
| *Send test* shows an error | The text is the service's own answer — usually a wrong token, chat ID or URL. Per-service steps: [Notifications](NOTIFICATIONS.md). |
| Test works, but no message after calls | *Send notifications* is off, or the channel is disabled. |
| No summary, only the caller's first words | No summary service: set the Gemini key (it is used by default), or your own under *Summary & calendar*. The terminal shows the reason if the service refused. |
| Links in notifications do not open | *Address of this console* must be an address your phone can reach, and the agent must listen on the network — see [Settings reference](SETTINGS.md#using-the-console-from-other-devices). |
| The calendar link opens the wrong calendar | The first press of **Add to calendar** in the console asks which calendar to use; **Change** next to that button picks another. An Apple Calendar / `.ics` link needs *Address of this console*. |
| The SMS forwarder stops reading texts after the phone restarted | Android keeps the SMS inbox locked (Direct Boot) until the phone is unlocked once with its PIN after a restart. Unlock it once; the forwarder catches up by itself. With the optional SMS hook, new texts are forwarded even before the first unlock. See the [SMS forwarder](../../tools/sms_forward/README.md#troubleshooting). |

## FAQ

**Does it work with an iPhone or without root?**
No. Android lets only privileged system apps record a call and play sound into
it, and iOS not at all.

**Will it work on my (non-Pixel) Android phone?**
Unknown until someone tries; it depends on the phone's audio hardware layer.
See [Phone setup](PHONE.md#which-phones).

**Can I keep using the phone normally?**
For a while, yes: switch it to a [normal phone](PHONE.md#normal-phone-mode) in
the app or the web console, and back when you are done. For every day, use a
spare phone, or move the SIM (or set up call forwarding from your main number
to the spare phone's number — then your main phone rings first and unanswered
calls go to the AI). Calls you dial yourself on the call-box phone are never
muted or taken over.

**What does it cost?**
The software is free. Google bills the Gemini Live audio per use (incoming
calls, AI call-backs and AI errands alike); the summary is a small text request
to Gemini's lowest-cost model. The voice previews and the blocked-number
announcement come recorded and cost nothing. A phone plan that receives calls
(and sends the texts and calls the AI makes for you). Electricity.

**Can it run without Google / fully offline?**
Not in the supported mode: the natural, low-delay voice is Gemini Live. The
summary can use a local model (Ollama) — see
[Settings reference](SETTINGS.md#summary--calendar). A local voice pipeline
is on the [roadmap](ROADMAP.md).

**Can it place calls or send text messages?**
Yes. It places calls in two ways: **AI errands**, where you write on the
*Errands* page what to get done and the AI calls, handles it, asks you when
unsure and reports back; and the **AI call-back** that gives a caller your
answer to an appointment. It sends **text messages** to callers with your
answer to an appointment, from the SIM the call came in on. The optional
[SMS forwarder](../../tools/sms_forward/README.md) passes text messages the
phone receives on to your notifications.

**Can it transfer a call in progress to my own phone?**
Not yet. Bridging a live call to a person is not supported; the AI takes a
message or gets the errand done, and you call back.

**Several phones / several numbers?**
Yes: plug several phones into the same computer. They share the settings.

**Where are my recordings? How do I delete them?**
In `data/calls/<date>/`. They are deleted after `CALLS_RETENTION_DAYS` (default
7); delete the folder to remove everything.

**Is the caller told it is an AI?**
With the *AI assistant* style, in the first sentence. With *As me*, only when
they sincerely ask. Whether the second is allowed where you live is for you to
find out — see [Privacy, safety and the law](PRIVACY_AND_LEGAL.md).

## Reporting a problem

Open an issue with: your system (Windows / macOS / Linux), phone model and
Android version, the agent version (bottom of the *System* page), what you did,
what you expected, and the relevant lines of the agent's terminal output.
Remove phone numbers and keys first. Never post `settings.json`.
