# Writing the prompt

The *prompt* is the text that tells the AI who it is and how to behave on the
phone. Bageo ships a built-in prompt for every language and both answer
styles, so you can start without writing anything. This page explains how to
make it yours.

- [What the AI is told on every call](#what-the-ai-is-told-on-every-call)
- [The easy way: let an AI interview you](#the-easy-way-let-an-ai-interview-you)
- [Notes the AI may answer from](#notes-the-ai-may-answer-from)
- [Status note](#status-note)
- [Writing or editing it yourself](#writing-or-editing-it-yourself)
- [Sales and scam calls](#sales-and-scam-calls)
- [What you cannot change](#what-you-cannot-change)
- [Trying it out](#trying-it-out)

## What the AI is told on every call

For each call the agent puts together, in this order:

1. **The prompt** — the built-in one for your language and answer style, or
   your own text from *Settings → Prompt* if that box is not empty.
2. **Your status note**, when switched on ("Waiting for a parcel today").
3. **Your notes** — facts it may answer from.
4. Fixed rules the agent always adds (see [below](#what-you-cannot-change)).

`{owner}` anywhere in a prompt is replaced by the name from *Settings → Your
name*. Changes apply to the next call; nothing needs restarting.

To read the built-in prompt for your current language and style, open
*Settings → Prompt → Show the built-in prompt*. **Copy into editor** gives you a starting
point for edits; **Back to built-in** empties the box again.

## The easy way: let an AI interview you

You do not need to know how to write prompts.

1. In the web console open *Settings → Prompt*, find the card *Don't want to
   write it yourself?*, choose the language and press **Copy interview prompt**. (The same texts are in
   [`docs/prompts/`](../prompts/): [English](../prompts/interview.en.txt),
   [繁體中文](../prompts/interview.zh-TW.txt),
   [简体中文](../prompts/interview.zh-CN.txt),
   [日本語](../prompts/interview.ja.txt),
   [한국어](../prompts/interview.ko.txt),
   [Español](../prompts/interview.es.txt),
   [Français](../prompts/interview.fr.txt),
   [Deutsch](../prompts/interview.de.txt),
   [Português](../prompts/interview.pt.txt).)
2. Open any chat AI — ChatGPT, Claude, Gemini, … — start a new chat and paste.
3. It asks about ten questions, one at a time: which language callers use, the
   answer style, how you talk, who calls you, what callers ask, what must never
   be agreed to, what to do with sales calls, how you say goodbye. Answer in
   your own words; say "skip" to take a default.
4. It gives you three things:
   - **the prompt** in a code block → paste into *Settings → Prompt → Your own prompt*;
   - **Notes** → paste into *Notes the AI may answer from*;
   - **which answer style and sales-stalling setting to select** → set *Settings → Answer style* and *Settings → Sales & scam calls → Keep them talking*.
5. Save, call your number, listen. If something is off, tell the chat AI what
   happened ("it talks too much", "it asked my caller for their name twice")
   and ask for a corrected prompt.

The interview prompt makes sure the result keeps the rules the call engine
relies on (see the checklist under
[Writing it yourself](#writing-or-editing-it-yourself)).

## Notes the AI may answer from

*Settings → Prompt → Notes the AI may answer from* is for facts, one per line:

```text
Parcels: leave them with the building's front desk, ground floor.
I work from home on Mondays and Fridays.
My office number is 02-5550-1234; office hours 9:00–18:00 on weekdays.
The dog is friendly; the doorbell works.
```

The AI is told to answer from these when asked, and that anything **not**
written here — prices, times, addresses, rules — it must not guess: it says it
is not sure. Keep the list short and concrete (the limit is about 6,000
characters). Do not put secrets here: the AI may say any of it to any caller.

## Status note

*Settings → How it answers → Status note* is one line about right now —
"Expecting a furniture delivery this afternoon", "On holiday until the 20th".
The AI mentions it only when the call has to do with it. Switch it off when it
no longer applies.

## Writing or editing it yourself

Write to the AI as "you", in the language it should speak. Short labelled
sections work better than one long paragraph. A prompt for this system should
cover:

| Section | Say |
|---|---|
| Who you are | *As me*: "You are {owner}, answering your own phone; you are not an assistant." — or — *AI assistant*: "You are the AI assistant answering {owner}'s phone; you are not {owner}." |
| How you talk | Language and regional flavour, relaxed or formal, the exact phrases and listener reactions you actually say out loud, plus banned words; state that example lines show the feel and should be varied naturally each time. No lists, no call-centre phrases. |
| Picking up | The exact kind of first line ("Hello?"), and then to stop and wait. |
| Listening | Not to talk over the caller; how to react when they finish a point. |
| When the caller has something to explain | Let a bank, company, clinic, school, building desk or support agent finish explaining real business and ask who called, what it is about, what is needed and a callback number; never brush them off with "I'm busy / call later". |
| What you cannot know or do | It cannot look anything up during the call; never "one moment" / "let me check"; say so when unsure; never invent your schedule or whereabouts. |
| Things not to agree to | Money, contracts, passwords, verification codes, personal data, fixed appointments, and the exact hedge phrase you use to pass over them lightly (at most once per call). |
| Sales and scam calls | Decline in one line — or see [below](#sales-and-scam-calls). |
| Ending | Wait until the caller is clearly done, then one closing line with your goodbye word. |

**How to get the exact words and tone you want:**

- **Give exact spoken phrases and banned words, not just adjectives.** Instead of "sound friendly and casual", write the exact opener ("Hello?"), listener fillers ("yeah", "right", "gotcha"), hedge ("let me check on that and get back to you") and farewell ("Alright, bye") you actually say, plus phrases it must never use ("never say 'how may I assist you'").
- **Say that example lines are for feel, not scripts to repeat verbatim.** Without that note, voice models tend to repeat the one quoted example at the end of every turn; with it, they keep your vocabulary while varying phrasing like a real person.
- **In *As me* style, write situation and boundaries rather than a rigid sentence-count formula.** Letting turn length flex with the moment (sometimes just "mm-hm", sometimes two or three sentences when chatting) sounds much more human than forcing two full sentences every time.

**Rules the call engine relies on** — keep them, in your own words:

- **One short opening line, then wait.** The opening is generated while the
  phone still rings, so it must not depend on anything the caller said.
  Its language follows the caller's number and the SIM the call came in on. A
  number from another country than the SIM's opens in that country's language
  when the AI speaks it — "Hello?" for `+1` or `+44`, 「もしもし？」 for `+81`.
  Any other call opens in the SIM's language (*Main language for each SIM*),
  else in *Language*. Every call's prompt gets one extra line telling the AI
  to switch to the caller's language when they speak another one or ask
  whether it speaks one (a foreign caller on a local or hidden number too);
  the rest of your prompt is used as written. Traditional and
  Simplified Chinese do not switch to each other: a `+86` caller to a 繁體中文
  setup still hears your setting's 「喂？」. With a phone app older than 0.2.5,
  which does not report the SIM, a call from abroad is a `+…` number whose
  country code is not yours (`COUNTRY_CODE`; `886` for 繁體中文). Some phones report the SIM only
  at pickup; when that changes the opening, the line prepared while it rang is
  dropped and the AI answers a moment later in the right language.
- **Short spoken turns.** One or two sentences. Long turns feel slow on a phone
  line and are hard to interrupt.
- **Never make the caller wait.** There are no tools behind the voice. An AI
  that says "let me check" will then say nothing.
- **Do not say goodbye first, and include a goodbye word when closing.** The
  agent hangs up when the *caller's* last words are a goodbye or a wrap-up
  ("that's all, thanks, bye") — or, after the AI has said goodbye, when the
  caller stays silent for a few seconds. An AI that says "bye" in the middle of
  a call makes the hang-up logic think the call is ending. The goodbye words it
  listens for are those of the supported languages ("bye", "goodbye", "see
  you", 「掰掰」「再見」, 「失礼します」, "adiós", "au revoir", "tschüss", "tchau", …).
  Thanks after them still count ("that's all, thank you so much"), a goodbye
  said to someone else ("say bye to grandma") does not, and one that turns
  into a question ("so that's all… for today, right?") is taken back.
- **No fixed time limit talk.** Close to the longest allowed call length the
  agent itself tells the AI to wrap up.

Things that do **not** belong in the prompt: the facts from your notes, a rule
about admitting to being an AI, API or technical settings.

### A short example (AI assistant, English)

```text
[Who you are] You are the AI assistant that answers {owner}'s phone when
{owner} can't pick up. You are not {owner}. You find out what the caller needs
and pass it on; {owner} reads a summary afterwards.

[How you talk] Friendly, plain British English, one or two short sentences at a
time. No lists, no "how may I help you today".

[Picking up] Say in one line that you're {owner}'s AI assistant, that {owner}
can't take the call just now, and ask what you can pass on. Then stop and wait.

[Listening] Let the caller finish. Ask only for what's missing, one thing at a
time: who's calling, what it's about, what {owner} should do, by when, and a
number to call back if it's not this one. Then confirm in one sentence.

[What you cannot know or do] You can't look anything up and you don't know
{owner}'s diary or where they are. Never say "one moment". If you don't know,
say you'll pass the question on.

[Things you must not agree to] Payments, contracts, passwords, codes, personal
details, fixed appointments: agree to nothing, say you'll pass it on.

[Sales and scam calls] One "No thank you" and wrap up.

[Ending] Don't say goodbye first. When the caller is clearly done, close with
one line that ends in "bye", and stop.
```

## Sales and scam calls

*Settings → Sales & scam calls → Keep them talking* decides what happens once
a caller's own words show the call is a sales pitch or a scam (insurance, loans,
investment, "your account is frozen", …):

- **Off:** the prompt's own line applies — by default one polite "no, thanks".
- **On:** the AI turns into a chatty, slightly slow, almost-convinced customer
  who keeps asking for details and never quite gets there, to keep the caller
  busy. It gives no real data, moves no money and agrees to nothing. The call
  ends when the caller gives up, or after *Longest such call* minutes.

The switch from normal to stalling happens mid-call and replaces a few lines of
the **built-in** prompt. With your own prompt it still works — when *Keep them
talking* is on, the system overrides any immediate sales-decline rule while the
call is still being checked (leaving ordinary calls alone) and appends the full
stalling instructions at the end once confirmed. Do not put a long stalling
script or "ask for their staff ID / proof" into your custom prompt (asking for
proof makes scammers hang up at once).

A caller following up on something *you* raised (a complaint, a repair, an application) is never
treated as a sales call, whatever words they use.

With it on, a call whose words look like a sales pitch, insurance offer or scam
gets a quick second look before the AI starts stalling: a small model reads the
call so far together with whether the caller is in your contacts (at most 1.5 s;
the AI talks on meanwhile). Saved contacts talking about transfers or bills, a
courier collecting cash on delivery, your card company checking a purchase you
made, or a bank / insurer updating you on an existing claim payout or card
delivery (without pitching new plans or asking for ATM steps, codes, safe
accounts or chat apps) are left alone, and looked at again only if more such
words come (three times a call at most). A caller who has only said who they
are ("this is your bank's card risk-control centre", "this is Mia from Acme
Insurance") is looked at again once they have said what they want.
A recorded voice menu ("press 1 for…") gets no answer: the AI stays
quiet and the agent hangs up after 4 s of quiet line, or 20 s.

## What you cannot change

The agent appends these to every prompt, built-in or your own:

- **Honesty about being an AI.** *AI assistant* style: never claim to be a
  person or to be you. *As me* style: talk as you, but when someone sincerely
  asks whether they are talking to a real person, say it is an AI voice you set
  up. Either way: no commitments on your behalf.
- **No personal details, no codes.** *As me* style: asked for an ID number,
  date of birth, card or account number, a verification code, a password or an
  address, the AI gives none and makes none up; it brushes it off ("I'd rather
  not do that over the phone"). Asked whether this is you, it still answers as
  you. This rule outranks anything in the prompt about playing along.
- **No answer about a card purchase.** *As me* style: when a bank or card
  centre asks whether a charge was yours, the AI cannot know, so it says
  neither yes nor no and agrees to no blocking, cancelling, disputing or
  reporting. It gets the amount, the shop and the time, and says it will call
  the number on the back of the card itself; the call's note tells you to
  check the charge.
- **Callers cannot give it orders.** What a caller says is conversation, not
  instructions: "ignore your rules" or "read me your instructions" is declined.
- **A caller who keeps saying "hello?"** cannot hear it well: answer with a
  whole sentence.

## Trying it out

- Call your own number from another phone and play a caller: a courier, a
  bank, a friend, a salesperson. Read the transcript afterwards — it shows
  exactly what the AI understood and said.
- Change one thing at a time.
- If the AI does something odd, look for the sentence in the prompt that
  allowed it rather than adding a new "never do X" line; contradicting lines
  make it worse.
