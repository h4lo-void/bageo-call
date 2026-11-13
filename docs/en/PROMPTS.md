# Writing the prompt

The *prompt* is the text that tells the AI who it is and how to behave on the
phone. Myna ships a built-in prompt for every language and both answer
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
   - **which answer style to select** → set *Settings → Answer style*.
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
| How you talk | Language and regional flavour, relaxed or formal, short sentences, your typical words. No lists, no call-centre phrases. |
| Picking up | The exact kind of first line ("Hello?"), and then to stop and wait. |
| Listening | Not to talk over the caller; how to react when they finish a point. |
| What you cannot know or do | It cannot look anything up during the call; never "one moment" / "let me check"; say so when unsure; never invent your schedule or whereabouts. |
| Things not to agree to | Money, contracts, passwords, verification codes, personal data, fixed appointments. |
| Sales and scam calls | Decline in one line — or see [below](#sales-and-scam-calls). |
| Ending | Wait until the caller is clearly done, then one closing line with your goodbye word. |

**Rules the call engine relies on** — keep them, in your own words:

- **One short opening line, then wait.** The opening is generated while the
  phone still rings, so it must not depend on anything the caller said.
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
the **built-in** prompt. With your own prompt it still works — the stalling
instructions are added at the end and take priority.

A caller following up on something *you* raised (a complaint, a claim) is never
treated as a sales call, whatever words they use.

## What you cannot change

The agent appends these to every prompt, built-in or your own:

- **Honesty about being an AI.** *AI assistant* style: never claim to be a
  person or to be you. *As me* style: talk as you, but when someone sincerely
  asks whether they are talking to a real person, say it is an AI voice you set
  up. Either way: no commitments on your behalf.
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
