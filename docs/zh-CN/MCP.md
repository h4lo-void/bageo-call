# MCP：让你的 AI 代理使用 Bageo

Bageo 内置 [Model Context Protocol](https://modelcontextprotocol.io)（MCP）服务器。只要是能接远程 MCP 服务器的 AI 代理或聊天 App（Claude、ChatGPT、Grok、Gemini CLI、OpenClaw（龙虾）、Cursor 等），都能连上来。

连上后可以查你的通话记录和待处理事项：“今天谁打来？”“餐厅怎么说？”你允许的话，还能让 Bageo 的 AI 帮你打电话：“打给鸡排店订两份，下午四点半拿”。

## 怎么开

MCP 服务器默认**关闭**，没设 token 之前什么都不响应。

1. 准备一组足够长的随机 token（例如 `openssl rand -hex 32`）。
2. 把 `mcp_enabled` 设成 `true`、`mcp_token` 设成那组 token（环境变量 `MCP_ENABLED=true`、`MCP_TOKEN=…`，或用 `PUT /calls/api/settings`）。
3. 用 `mcp_calls` 决定代理能不能让 AI 打电话：
   - `off`（默认）：只能查。
   - `confirm`：可以提出代打；你会在下指令的聊天室收到一张带 **执行／取消** 按钮的卡片（见[在聊天室操作](CHAT_COMMANDS.md)），你按了执行才会拨。需要先开一个允许下指令的聊天室。
   - `direct`：立刻排进代打。只给你完全信任的代理用。
4. 在代理那边填网址 `http://<Bageo 的地址>:8787/mcp`，加上请求头 `Authorization: Bearer <token>`。

这些设置在控制台“设置 → 高级”的“让其他 AI 工具连进来（MCP）”折叠里也能设，字段是“开启 MCP”“Token”“可以让 AI 代打电话吗”“每小时最多代打几通”“允许的网站”。

大部分代理接受这样的配置：

```json
{
  "mcpServers": {
    "bageo": {
      "type": "http",
      "url": "http://bageo.local:8787/mcp",
      "headers": { "Authorization": "Bearer 你的TOKEN" }
    }
  }
}
```

Claude Code：`claude mcp add --transport http bageo http://bageo.local:8787/mcp --header "Authorization: Bearer 你的TOKEN"`。

代理如果跑在别的网络（例如云端的聊天 App），需要一个能连到 Bageo 的 HTTPS 网址，用你自己的反向代理或隧道。token 请保密：拿到 token 的人就能看你的通话。

## 工具

| 工具 | 做什么 | 什么时候有 |
|---|---|---|
| `get_status` | 版本、每部手机（连接、AI 接听或普通手机、通话中）、进行中的通话 | 一直有 |
| `list_calls` | 最近的通话：`limit`（1–20）、`day`（`today`、`yesterday`、`YYYY-MM-DD`）、`only`（`all`、`missed`、`scam`）、`text`（名字、号码或关键词） | 一直有 |
| `get_call` | 用 `id` 看某一通：摘要、逐字稿开头（最多 600 字）、网页控制台链接 | 一直有 |
| `list_tasks` | 等你处理的待处理事项 | 一直有 |
| `list_errands` | AI 代打的电话和进度 | 一直有 |
| `find_contacts` | 找符合 `query` 的联系人 | 一直有 |
| `request_call` | AI 打给 `target`（号码或联系人名字）去办 `task` | `mcp_calls` 是 `confirm` 或 `direct` |
| `cancel_call` | 用 `id` 停止 AI 代打 | `mcp_calls` 是 `confirm` 或 `direct` |
| `say_to_call` | 通话中交代 AI 一句话 | `mcp_calls` 是 `direct` |

每个结果都同时给 JSON 文本和 `structuredContent`（`kind` 表示是什么：`calls`、`call`、`confirm`、`errand_started`、带 `code` 的 `failed`…）。错误（`isError: true`）的代码跟[网页 API](API.md) 一样，例如 `no_phone`、`not_allowed`、`bad_day`。

`confirm` 模式下提出代打，会返回 `kind: "confirm"` 和你会看到的摘要。之后用 `list_errands` 查：你按了执行，那通就会出现在列表里。确认只等 2 分钟。

交代任务时把时间日期写清楚（“下午四点半”“11 月 15 日星期六”）。通话中，AI 会把每个时间、日期、金额、名字用问句复述一次，等对方说对才算数；没人确认的时间会在结果里标成未确认。

## 安全

- 默认关闭；没设 token 时完全不响应（404）。每个请求都要带 token，用固定时间比较。
- 网页发出的请求（带 `Origin` 请求头）一律拒绝，除非那个来源完全符合 `mcp_allowed_origins` 里的一项。这能挡住其他网页和 DNS rebinding 通过你的浏览器连进来。本机的代理不发送 Origin，不受影响。
- 代打跟网页控制台走一样的检查：紧急、短号、增值付费号码一律不拨，要有那个国家的 SIM 卡，一样排队。每小时有上限（`mcp_errands_per_hour`，默认 3，最多 30，所有代理合计）；每次提出都算，不管你有没有按。`confirm` 模式拨出时也算进那个聊天室自己的每小时上限。
- 结果绝不包含密钥或 token：Bageo 持有的任何密钥都会在发送前被遮掉。
- 摘要和逐字稿是来电者说的话。服务器会告诉代理的模型这些只是数据、不是指令；而且在 `confirm` 模式下，拨电话还是要你按执行。代理如果也会读别人的内容，请用 `confirm`。
- 所有代理合计每分钟最多 60 个请求；每个请求最多 64 KB。

## 设置

| 设置 | 环境变量 | 默认 |
|---|---|---|
| `mcp_enabled` | `MCP_ENABLED` | 关 |
| `mcp_token` | `MCP_TOKEN` | 无（服务器不响应） |
| `mcp_calls` | `MCP_CALLS` | `off`（`off`、`confirm`、`direct`） |
| `mcp_errands_per_hour` | `MCP_ERRANDS_PER_HOUR` | 3（0–30） |
| `mcp_allowed_origins` | `MCP_ALLOWED_ORIGINS` | 无（逗号分隔，例如 `https://app.example`） |

## 协议细节

Streamable HTTP 传输、无状态：每个 `POST /mcp` 一条 JSON-RPC 消息，返回一个 JSON。没有事件流（`GET` 返回 405）、没有 session id、不支持 batch。

方法：`initialize`、`ping`、`tools/list`、`tools/call`；通知返回 `202 Accepted`。支持协议版本 `2025-11-25`、`2025-06-18`、`2025-03-26`；不认识的 `MCP-Protocol-Version` 请求头返回 400。

代码在 `crates/agent/src/mcp.rs`，工具执行的是聊天室指令的同一组动作（`crates/agent/src/control/`）。

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
