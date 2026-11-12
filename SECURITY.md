# Security

## Reporting a vulnerability

Please do not open a public issue for security problems. Use GitHub's
**Report a vulnerability** button (Security tab → Advisories) of this
repository to report privately. Include what an attacker can do, how to
reproduce it, and the version.

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

See [Privacy, safety and the law](docs/en/PRIVACY_AND_LEGAL.md).
