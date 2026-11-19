# Security

Bageo answers calls, places calls and sends texts from your real phone number.
Whoever can use its console or API can do the same, so treat access to it like
access to your phone. This page covers how to report a problem, how to run
Bageo safely, and what it already guards against.

## Reporting a vulnerability

Please do not open a public issue for security problems. Use GitHub's
**Report a vulnerability** button (Security tab → Advisories) of this
repository to report privately. Include what an attacker can do, how to
reproduce it, and the version.

Security fixes go into the latest release; update the Bageo program and the
phone module together.

## Running it safely

- With `AGENT_HOST` unset (the default), Bageo listens on your local network
  (`AGENT_PORT` and port 80 for `http://bageo.local/`), but refuses other
  devices until you set a console password (in the console or via
  `CALLS_UI_TOKEN`); phones paired once over USB connect over Wi-Fi on
  `PHONE_WIFI_PORT` (`8788`) using a pairing key.
- Set `AGENT_HOST=127.0.0.1` to listen on this computer only. When you set
  `AGENT_HOST=0.0.0.0`, always set both `CALLS_UI_TOKEN` (web console and API)
  and `DEVICE_TOKEN` (phones over Wi-Fi).
- The web console uses one shared password (`ui_password` set in the browser or
  `CALLS_UI_TOKEN`), no user accounts and no HTTPS. Do not expose it to the
  internet; reach it from outside through a VPN.
- Since 1.0 Bageo can place calls (AI errands, AI call-backs) and send texts
  from your SIM. Treat the console password (`CALLS_UI_TOKEN`) like the PIN of
  your phone.
- `data/calls/config/settings.json` holds your API keys and notification
  tokens in plain text; recordings and transcripts are other people's personal
  data. Automatic and manual backup files (`data/calls/backups/` or your
  chosen folder) also include keys, passwords and connections unless encrypted
  with a backup password (`.bageo`). Protect the `data/` folder and its
  backups accordingly.
- The phone module installs a privileged system app. Install only builds you
  trust.

## Built-in protections

- **Other web pages cannot drive the console.** The console has no login cookie,
  but a browser on your network could be tricked into sending it requests. Every
  route that places a call, sends a text, decides an appointment, switches a phone,
  changes stored contacts, clears records or manages backups requires
  `Content-Type: application/json` (else HTTP 400 `bad_request`): the decision
  routes `/calls/api/tasks/{id}/confirm` and `/ignore`,
  `/calls/api/appointments/{id}/retry-sms`, `/calls/api/calls/clear`,
  `/calls/api/call/{id}/scam-feedback`, the SMS routes (`/calls/api/sms/send`,
  `/calls/api/sms/{id}/retry`, `/calls/api/sms/clear`, `DELETE /calls/api/sms/thread`),
  `/calls/api/devices/{serial}/mode` and `/sim-number`, `/calls/api/phone-pairing/renew`,
  `/calls/api/initial-password`, contact changes (`POST`, `PUT`, `block`, `batch`;
  `import` needs an `X-Filename` header), every `POST` under `/calls/api/errands`,
  and every backup `POST` route (`/calls/api/backup/import` needs an `X-Filename`
  header). A page on another site cannot send that header without a CORS preflight,
  and Bageo never answers preflights.
- **Host header check & password rate limiting.** Under the default setup, Bageo
  refuses requests with unknown `Host` headers to resist DNS rebinding, and
  throttles repeated wrong console or backup password attempts.
- **MCP disabled by default.** The `/mcp` endpoint stays silent (404) until
  both `mcp_enabled` and `mcp_token` are set, checks the bearer token in
  constant time, and rejects browser requests whose `Origin` is not in
  `mcp_allowed_origins`.
- **Calendar links carry no token.** A notification's `event.ics` link opens with
  a key (an HMAC) valid for that one file only.
- **What the AI may dial is limited.** Errands never dial emergency numbers,
  short codes or premium-rate numbers, call each errand once (with Redial until
  answered on, at most 30 times, a minute apart), and hang up before
  the AI speaks when the phone used another SIM than the one asked for.
- **Secrets stay hidden.** The settings API returns keys and notification tokens
  masked. `/health` returns only a short status unless the request carries the
  console password, or, with no password set, comes from this computer itself
  (not through a reverse proxy).

Found a way around any of these? Please report it privately, as above.

See also [Privacy, safety and the law](docs/en/PRIVACY_AND_LEGAL.md), [Legal disclaimer & usage notice](LEGAL.md) and the [v1.0 AI-assisted internal security review note (`gemini-4-carbon`)](AUDIT_REPORT.md).

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
