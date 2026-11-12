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
| Plugged in, link set, but not connected | Open the Myna app: is *AI answering* on, is every checklist line OK? Tap **Test connection**. The agent retries starting the service every 90 seconds. |
| Connects and drops again and again | Two different `adb` versions on the computer (Android Studio, a phone-mirroring tool, …) keep restarting each other. Set `ADB_BIN` to one of them and close the other tool. |

## Calls are not answered, or sound wrong

| Symptom | Likely cause |
|---|---|
| The phone rings on and on | The app's service is not running or *AI answering* is off in the app. Without a connection to the agent the app still picks up after about 12 seconds — and the agent then hangs up. |
| Picked up, then hung up at once, nothing said | The AI could not start: no Gemini key, a wrong key, an unknown model name, no internet, or *AI answers calls* off in the console. The call record's summary says "the AI did not connect"; the terminal shows Gemini's error. |
| The caller hears nothing, the AI seems to answer (transcript has AI lines) | The phone cannot play into the call (unsupported phone, or the app is not a privileged app). See the table in [`android/README.md`](../../android/README.md#troubleshooting). |
| The AI never reacts to the caller (transcript has no caller lines) | The phone delivers no call audio to the app — again a phone-support question. |
| The AI answers itself / talks over the caller | The phone's recording contains the AI's own voice and the echo check did not catch it. Switch off *Let the caller interrupt*. |
| The AI hangs up too early | Its prompt lets it say goodbye before the caller is done. See the ending rule in [Writing the prompt](PROMPTS.md#writing-or-editing-it-yourself). |
| The call never ends | The caller never said a goodbye the agent recognises and stayed on the line; it ends at *Longest normal call*. Shorten that if it happens often. |
| First words come late | Switch on *Get the AI ready while the phone rings* and keep *Ring before answering* at 3 s or more. |
| It answers in the wrong language | Set *Language*. Short first words ("hello?") are easy to mis-detect; with *Reply in the caller's language* off the AI stays in your language. |
| A real call was treated as sales / scam | Switch off *Keep them talking*, or tell us which words triggered it (the agent's terminal output names them: `stall engaged … hits=[…]`). |

## Notifications

| Symptom | What to do |
|---|---|
| *Send test* shows an error | The text is the service's own answer — usually a wrong token, chat ID or URL. Per-service steps: [Notifications](NOTIFICATIONS.md). |
| Test works, but no message after calls | *Send notifications* is off, or the channel is disabled. |
| No summary, only the caller's first words | No summary service: set the Gemini key (it is used by default), or your own under *Summary & calendar*. The terminal shows the reason if the service refused. |
| Links in notifications do not open | *Address of this console* must be an address your phone can reach, and the agent must listen on the network — see [Settings reference](SETTINGS.md#using-the-console-from-other-devices). |

## FAQ

**Does it work with an iPhone or without root?**
No. Android lets only privileged system apps record a call and play sound into
it, and iOS not at all.

**Will it work on my (non-Pixel) Android phone?**
Unknown until someone tries; it depends on the phone's audio hardware layer.
See [Phone setup](PHONE.md#which-phones).

**Can I keep using the phone normally?**
Not comfortably. Use a spare phone, or move the SIM (or set up call forwarding
from your main number to the spare phone's number — then your main phone rings
first and unanswered calls go to the AI).

**What does it cost?**
The software is free. Google bills the Gemini Live audio per use; the summary
is a small text request. A phone plan that receives calls. Electricity.

**Can it run without Google / fully offline?**
Not in the supported mode: the natural, low-delay voice is Gemini Live. The
summary can use a local model (Ollama) — see
[Settings reference](SETTINGS.md#summary--calendar). A local voice pipeline
is on the [roadmap](ROADMAP.md).

**Can it place calls, transfer a call to me, or send SMS?**
Not in this version. The optional [SMS forwarder](../../tools/sms_forward/README.md)
passes received text messages on to your notifications.

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
