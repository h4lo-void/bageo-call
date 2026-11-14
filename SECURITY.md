# Security

## Reporting a vulnerability

Please do not open a public issue for security problems. Use GitHub's
**Report a vulnerability** button (Security tab → Advisories) of this
repository to report privately. Include what an attacker can do, how to
reproduce it, and the version.

Security fixes go into the latest release (1.0.x). Older 0.x versions get no
fixes; update the agent and the phone module together.

## What to keep in mind when running it

- The agent listens on `127.0.0.1` by default. When you set
  `AGENT_HOST=0.0.0.0`, always set `CALLS_UI_TOKEN` (web console and API) and
  `DEVICE_TOKEN` (phones over Wi-Fi).
- The web console has one shared token, no accounts and no HTTPS. Do not expose
  it to the internet; use a VPN.
- `data/calls/config/settings.json` holds your API keys and notification
  tokens in plain text; recordings and transcripts are other people's personal
  data.
- The phone module installs a privileged system app. Install only builds you
  trust.
- Since 1.0 the agent can place calls (AI errands, AI call-backs) and send texts
  from your SIM. Anyone who can use the console or the API can make it do so:
  treat `CALLS_UI_TOKEN` like the PIN of your phone.

## Built-in protections

- **Other web pages cannot drive the console.** The console has no login cookie,
  but a browser on your network could be tricked into sending it requests. Every
  route that places a call, sends a text, decides an appointment, switches a phone
  or changes stored contacts requires `Content-Type: application/json` (else
  HTTP 400 `bad_request`): the decision routes `/calls/api/tasks/{id}/confirm`
  and `/ignore`, `/calls/api/appointments/{id}/retry-sms`,
  `/calls/api/devices/{serial}/mode`, the contact changes (`POST`, `PUT`,
  `batch`; `import` needs an `X-Filename` header) and every `POST` under
  `/calls/api/errands`. A page on another site cannot send that header without a
  CORS preflight, and the agent never answers preflights.
- **Calendar links carry no token.** A notification's `event.ics` link opens with
  a key (an HMAC) valid for that one file only.
- **What the AI may dial is limited.** Errands never dial emergency numbers,
  short codes or premium-rate numbers, call each errand once, and hang up before
  the AI speaks when the phone used another SIM than the one asked for.
- **Secrets stay hidden.** The settings API returns keys and notification tokens
  masked; `/health` without the token returns only a short status.

Please report a way around any of these privately, as above.

See [Privacy, safety and the law](docs/en/PRIVACY_AND_LEGAL.md).
