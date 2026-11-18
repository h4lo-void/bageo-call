# MCP：讓你的 AI 代理使用 Bageo

Bageo 內建 [Model Context Protocol](https://modelcontextprotocol.io)（MCP）伺服器。只要是能接遠端 MCP 伺服器的 AI 代理或聊天 App（Claude、ChatGPT、Grok、Gemini CLI、OpenClaw（龍蝦）、Cursor 等），都能連上來。

連上後可以查你的通話紀錄和待處理事項：「今天誰打來？」「餐廳怎麼說？」你允許的話，還能叫 Bageo 的 AI 幫你打電話：「打給雞排店訂兩份，下午四點半拿」。

## 怎麼開

MCP 伺服器預設**關閉**，沒設 token 之前什麼都不回應。

1. 準備一組夠長的隨機 token（例如 `openssl rand -hex 32`）。
2. 把 `mcp_enabled` 設成 `true`、`mcp_token` 設成那組 token（環境變數 `MCP_ENABLED=true`、`MCP_TOKEN=…`，或用 `PUT /calls/api/settings`）。
3. 用 `mcp_calls` 決定代理能不能叫 AI 打電話：
   - `off`（預設）：只能查。
   - `confirm`：可以提出代打；你會在下指令的聊天室收到一張有 **執行／取消** 按鈕的卡片（見[在聊天室操作](CHAT_COMMANDS.md)），你按了執行才會撥。需要先開一個允許下指令的聊天室。
   - `direct`：立刻排進代打。只給你完全信任的代理用。
4. 在代理那邊填網址 `http://<Bageo 的位址>:8787/mcp`，加上標頭 `Authorization: Bearer <token>`。

這些設定在控制台「設定 → 進階」的「讓其他 AI 工具連進來（MCP）」折疊裡也能設，欄位是「開啟 MCP」「Token」「可以叫 AI 代打電話嗎」「每小時最多代打幾通」「允許的網站」。

大部分代理接受這樣的設定：

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

代理如果跑在別的網路（例如雲端的聊天 App），需要一個能連到 Bageo 的 HTTPS 網址，用你自己的反向代理或通道。token 請保密：拿到 token 的人就能看你的通話。

## 工具

| 工具 | 做什麼 | 什麼時候有 |
|---|---|---|
| `get_status` | 版本、每支手機（連線、AI 接聽或一般手機、通話中）、進行中的通話 | 一直有 |
| `list_calls` | 最近的通話：`limit`（1–20）、`day`（`today`、`yesterday`、`YYYY-MM-DD`）、`only`（`all`、`missed`、`scam`）、`text`（名字、號碼或關鍵字） | 一直有 |
| `get_call` | 用 `id` 看某一通：摘要、逐字稿開頭（最多 600 字）、網頁控制台連結 | 一直有 |
| `list_tasks` | 等你處理的待處理事項 | 一直有 |
| `list_errands` | AI 代打的電話和進度 | 一直有 |
| `find_contacts` | 找符合 `query` 的聯絡人 | 一直有 |
| `request_call` | AI 打給 `target`（號碼或聯絡人名字）去辦 `task` | `mcp_calls` 是 `confirm` 或 `direct` |
| `cancel_call` | 用 `id` 停止 AI 代打 | `mcp_calls` 是 `confirm` 或 `direct` |
| `say_to_call` | 通話中交代 AI 一句話 | `mcp_calls` 是 `direct` |

每個結果都同時給 JSON 文字和 `structuredContent`（`kind` 表示是什麼：`calls`、`call`、`confirm`、`errand_started`、帶 `code` 的 `failed`…）。錯誤（`isError: true`）的代碼跟[網頁 API](API.md) 一樣，例如 `no_phone`、`not_allowed`、`bad_day`。

`confirm` 模式下提出代打，會回 `kind: "confirm"` 和你會看到的摘要。之後用 `list_errands` 查：你按了執行，那通就會出現在清單裡。確認只等 2 分鐘。

交代任務時把時間日期寫清楚（「下午四點半」「11 月 15 日星期六」）。通話中，AI 會把每個時間、日期、金額、名字用問句複誦一次，等對方說對才算數；沒人確認的時間會在結果裡標成未確認。

## 安全

- 預設關閉；沒設 token 時完全不回應（404）。每個請求都要帶 token，用固定時間比對。
- 網頁發出的請求（帶 `Origin` 標頭）一律拒絕，除非那個來源完全符合 `mcp_allowed_origins` 裡的一筆。這能擋住其他網頁和 DNS rebinding 透過你的瀏覽器連進來。本機的代理不送 Origin，不受影響。
- 代打跟網頁控制台走一樣的檢查：緊急、短碼、加值付費號碼一律不撥，要有那個國家的 SIM 卡，一樣排隊。每小時有上限（`mcp_errands_per_hour`，預設 3，最多 30，所有代理合計）；每次提出都算，不管你有沒有按。`confirm` 模式撥出時也算進那個聊天室自己的每小時上限。
- 結果絕不包含金鑰或 token：Bageo 持有的任何金鑰都會在送出前被遮掉。
- 摘要和逐字稿是來電者說的話。伺服器會告訴代理的模型這些只是資料、不是指令；而且在 `confirm` 模式下，撥電話還是要你按執行。代理如果也會讀別人的內容，請用 `confirm`。
- 所有代理合計每分鐘最多 60 個請求；每個請求最多 64 KB。

## 設定

| 設定 | 環境變數 | 預設 |
|---|---|---|
| `mcp_enabled` | `MCP_ENABLED` | 關 |
| `mcp_token` | `MCP_TOKEN` | 無（伺服器不回應） |
| `mcp_calls` | `MCP_CALLS` | `off`（`off`、`confirm`、`direct`） |
| `mcp_errands_per_hour` | `MCP_ERRANDS_PER_HOUR` | 3（0–30） |
| `mcp_allowed_origins` | `MCP_ALLOWED_ORIGINS` | 無（逗號分隔，例如 `https://app.example`） |

## 協定細節

Streamable HTTP 傳輸、無狀態：每個 `POST /mcp` 一則 JSON-RPC 訊息，回一個 JSON。沒有事件串流（`GET` 回 405）、沒有 session id、不支援 batch。

方法：`initialize`、`ping`、`tools/list`、`tools/call`；通知回 `202 Accepted`。支援協定版本 `2025-11-25`、`2025-06-18`、`2025-03-26`；不認得的 `MCP-Protocol-Version` 標頭回 400。

程式在 `crates/agent/src/mcp.rs`，工具執行的是聊天室指令的同一組動作（`crates/agent/src/control/`）。

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
