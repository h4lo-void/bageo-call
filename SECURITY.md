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
  `PHONE_WIFI_PORT` (`8788`) using `wss://` with SHA-256 leaf-certificate
  pinning and a pairing key (`PHONE_WIFI_TLS_ONLY=true` disables the legacy
  `ws://` fallback), and IP pairing trades the 6-digit code over `https://`
  using a channel-bound HMAC.
- Set `AGENT_HOST=127.0.0.1` to listen on this computer only. When you set
  `AGENT_HOST=0.0.0.0`, always set both `CALLS_UI_TOKEN` (web console and API)
  and `DEVICE_TOKEN` (phones over Wi-Fi).
- The web console uses one shared password (`ui_password` set in the browser or
  `CALLS_UI_TOKEN`) and no user accounts. Do not expose it directly to the
  internet; reach it from outside through a VPN.
- Since 1.0 Bageo can place calls (AI errands, AI call-backs) and send texts
  from your SIM. Treat the console password (`CALLS_UI_TOKEN`) like the PIN of
  your phone.
- `data/calls/config/settings.json` seals saved API keys, passwords and
  notification tokens at rest with `XChaCha20-Poly1305` (`enc:v1:…`) using a
  local key file (`data/calls/config/.settings.key`, mode `0600` on Unix) or
  `BAGEO_MASTER_KEY` from the environment. Recordings and transcripts are other
  people's personal data, and unencrypted backup files (`data/calls/backups/` or
  your chosen folder) include keys unless encrypted with a backup password
  (`.bageo`). Protect the `data/` folder and its backups accordingly.
- The phone module installs a privileged system app. Install only builds you
  trust.
- The phone module also carries a root program, `bageo-audio-helper`, for phones
  whose call audio the app cannot reach by itself. It does not stay in the
  background: it starts when the app asks and quits about a minute after the app
  stops using it; at boot it runs briefly only to put back settings a call left
  changed. `bageo-audio-helper disable` (as root) turns it off; see
  [Phone setup](docs/en/PHONE.md#the-audio-helper).

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
  `/calls/api/devices/{serial}/mode`, `/audio-problem` and `/sim-number`, `/calls/api/phone-pairing/renew`,
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
- **The phone's root audio helper serves only the Bageo app, and only for listed
  changes.** `bageo-audio-helper` serves the app only when the system lists it once,
  as a non-debuggable app with a user ID of its own in the main user, and only over a
  connection whose listener has that user ID and a privileged-app SELinux context
  and presents a one-time key that only the app can read; the helper proves itself
  to the app with a second key. Requests are versioned and size- and time-limited.
  The app may only set mixer controls, audio parameters and sound devices that a
  list built into the helper names for that phone (empty for now), open sound
  devices only while the phone is in a call, and read nothing. Old values are saved
  to a root-only file first and put back when the call ends; values that cannot be
  put back are kept and retried at the next start or boot, and no new call session
  starts until they are. The module's root scripts handle the app's requests without
  following links the app could plant. The module adds no SELinux rule.
- **Secrets stay hidden.** The settings API returns keys and notification tokens
  masked. `/health` returns only a short status unless the request carries the
  console password, or, with no password set, comes from this computer itself
  (not through a reverse proxy).

Found a way around any of these? Please report it privately, as above.

See also [Privacy, safety and the law](docs/en/PRIVACY_AND_LEGAL.md), [Legal disclaimer & usage notice](LEGAL.md) and the [v1.0 AI-assisted internal security review note (`gemini-4-carbon`)](AUDIT_REPORT.md).

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
