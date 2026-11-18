# MCP: let your AI agent use Bageo

Bageo has a built-in [Model Context Protocol](https://modelcontextprotocol.io) server. Any AI agent or chat app that can use a remote MCP server (Claude, ChatGPT, Grok, Gemini CLI, OpenClaw, Cursor and others) can then look at your call log and to-dos and, if you allow it, have Bageo's AI make a call for you: "who called today?", "what did the restaurant say?", "call the chicken shop and order two portions for 4:30 pm".

## Turn it on

The MCP server is **off** by default and answers nothing until it has a token.

1. Pick a long random token (for example `openssl rand -hex 32`).
2. Set `mcp_enabled` to `true` and `mcp_token` to that token (environment: `MCP_ENABLED=true`, `MCP_TOKEN=…`, or `PUT /calls/api/settings`).
3. Choose what clients may do about calls with `mcp_calls`:
   - `off` (default): they can only look.
   - `confirm`: they may ask for a call; you get a card with **yes / no** in your command chat (see [Chat commands](CHAT_COMMANDS.md)) and the call is made only after your yes. This needs a chat with commands switched on.
   - `direct`: the call is queued at once. Use only with an agent you trust completely.
4. Point your client at `http://<Bageo address>:8787/mcp` with the header `Authorization: Bearer <token>`.

You can also set all of this in the console under *Settings → Advanced*, in the *Let other AI tools connect (MCP)* fold: *Turn on MCP*, *Token*, *May it ask the AI to place calls?*, *Most AI calls per hour* and *Allowed websites*.

Most clients take a JSON entry like this:

```json
{
  "mcpServers": {
    "bageo": {
      "type": "http",
      "url": "http://bageo.local:8787/mcp",
      "headers": { "Authorization": "Bearer YOUR_TOKEN" }
    }
  }
}
```

With Claude Code: `claude mcp add --transport http bageo http://bageo.local:8787/mcp --header "Authorization: Bearer YOUR_TOKEN"`.

A client running on another network (a cloud chat app) needs an HTTPS address that reaches Bageo, from your own reverse proxy or tunnel. Keep the token secret: whoever has it can read your calls.

## Tools

| Tool | What it does | Offered when |
|---|---|---|
| `get_status` | Version, phones (connected, AI or normal mode, in a call), live calls | always |
| `list_calls` | Recent calls: `limit` (1–20), `day` (`today`, `yesterday`, `YYYY-MM-DD`), `only` (`all`, `missed`, `scam`), `text` (name, number or word) | always |
| `get_call` | One call by `id`: summary, the start of the transcript (600 characters at most), its page in the web console | always |
| `list_tasks` | To-dos waiting for your decision | always |
| `list_errands` | The AI's calls for you and where each stands | always |
| `find_contacts` | Contacts matching `query` | always |
| `request_call` | The AI calls `target` (a number or a contact's name) to do `task` | `mcp_calls` = `confirm` or `direct` |
| `cancel_call` | Stops an AI call by its `id` | `mcp_calls` = `confirm` or `direct` |
| `say_to_call` | A line for the AI on a call that is on the line now | `mcp_calls` = `direct` |

Every result comes as JSON text and as `structuredContent` (`kind` says what it is: `calls`, `call`, `confirm`, `errand_started`, `failed` with a `code`…). Errors (`isError: true`) use the same codes as the [web API](API.md#errors-and-status-codes), for example `no_phone`, `not_allowed`, `bad_day`.

A call request in `confirm` mode returns `kind: "confirm"` with the summary you will see. Check `list_errands` afterwards: the call shows up there once you say yes. The confirmation lapses after 2 minutes.

Say times and dates exactly in the task ("4:30 pm", "Saturday 15 November"). On the call, the AI reads every time, date, amount and name back as a question and waits for the other side's yes; a time nobody confirmed comes back in the result as unconfirmed.

## Safety

- Off by default; silent (404) without a token. Every request needs the token, compared in constant time.
- A request from a web page (it has an `Origin` header) is refused unless that origin is listed in `mcp_allowed_origins` exactly. This stops other web pages and DNS rebinding from reaching the server through your browser. Local clients send no Origin and are not affected.
- Calls go through the same checks as the web console: never an emergency, short or premium-rate number, a SIM for the number's country, the same queue. Requests are limited per hour (`mcp_errands_per_hour`, default 3, at most 30, all clients together); each request counts, answered or not. In `confirm` mode the call also counts against the chat's own hourly limit.
- Results never contain a key or token: everything Bageo holds is cut out before sending.
- Summaries and transcripts are what callers said. The server tells the client model to treat them as data and never as instructions, and with `confirm` a call still needs your yes. Prefer `confirm` for any agent that also reads other people's content.
- At most 60 requests per minute from all clients together; a request body may be 64 KB.

## Settings

| Setting | Environment | Default |
|---|---|---|
| `mcp_enabled` | `MCP_ENABLED` | off |
| `mcp_token` | `MCP_TOKEN` | none (server silent) |
| `mcp_calls` | `MCP_CALLS` | `off` (`off`, `confirm`, `direct`) |
| `mcp_errands_per_hour` | `MCP_ERRANDS_PER_HOUR` | 3 (0–30) |
| `mcp_allowed_origins` | `MCP_ALLOWED_ORIGINS` | none (comma separated, `https://app.example`) |

## Protocol details

Streamable HTTP transport, stateless: one JSON-RPC message per `POST /mcp`, answered with one JSON body. No event stream (`GET` returns 405), no session id, no batches. Methods: `initialize`, `ping`, `tools/list`, `tools/call`; notifications get `202 Accepted`. Protocol versions `2025-11-25`, `2025-06-18` and `2025-03-26`; an unknown `MCP-Protocol-Version` header gets 400. The code is in `crates/agent/src/mcp.rs`; the tools run the chat commands' actions (`crates/agent/src/control/`).

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
