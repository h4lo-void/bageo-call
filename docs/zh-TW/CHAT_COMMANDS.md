# 在聊天室操作：用 Telegram 遙控 Bageo

人不在網頁控制台前面時，可以直接在收通知的 Telegram 聊天室裡操作：查通話紀錄、叫 AI 代打電話、處理待辦、回答 AI 通話中問你的問題、把手機切到 AI 接聽或一般手機。目前支援 Telegram；程式已經把介面定好，之後可以接其他聊天軟體和 AI 代理（見[給開發者](#給開發者)）。

## 怎麼開

聊天室指令預設**關閉**，要在每個聊天室各自打開。

1. 先設定好 Telegram 通知管道（[通知 → Telegram](NOTIFICATIONS.md#telegram)）。
2. *設定 → 通知* → 那個 Telegram 管道 → 勾選**允許在這個聊天室下指令**。
3. 可選：**可以下指令的人**：填 Telegram 使用者 ID（問 **@userinfobot** 就知道自己的）。留空 = 這個聊天室裡的人都可以；群組請填你自己的 ID。
4. **儲存變更**，再傳 `/help` 給機器人。

用環境變數設定的機器人（`TELEGRAM_BOT_TOKEN` + `TELEGRAM_CHAT_ID`）在通知管道下方有自己的開關，或設 `TELEGRAM_CONTROL=true`；要限定可以下指令的人，設 `TELEGRAM_CONTROL_USERS=123456789,987654321`。

在群組裡，機器人只理會指令（`/calls` 或 `/calls@你的機器人`）和回覆它自己訊息的話，其他人聊天一律不理。

## 指令

`1`、`#2` 這種編號，指的是這個聊天室最近一次列出的清單（`/calls`、`/tasks`、`/errands`、`/status`）。

| 指令 | 做什麼 |
|---|---|
| `/help` | 列出指令 |
| `/status` | agent 版本、每支手機的狀態（AI 接聽／一般手機／離線／通話中）、進行中的通話 |
| `/calls [筆數] [today\|yesterday\|YYYY-MM-DD] [missed\|scam] [關鍵字或號碼]` | 最近的通話，一通一行：時間、對方、摘要第一行（預設 5 筆，最多 20） |
| `/call <#>` | 某一通：摘要、逐字稿開頭（最多 600 字）、網頁控制台連結 |
| `/tasks` | 等你處理的待辦（預約、留言） |
| `/confirm <#>`・`/ignore <#>` | 同意／忽略待辦，跟網頁上按一樣 |
| `/errand <號碼或聯絡人> <要辦的事>` | 叫 AI 幫你打電話，例如 `/errand 0912345678 訂今晚七點兩位`。名字有空格請加引號 |
| `/errands` | AI 代打的電話與結果 |
| `/cancel [#]` | 停止 AI 代打（不填編號 = 正在進行的那通） |
| `/say [#] <內容>` | 通話中交代 AI 一句話 |
| `/answer <內容>` | 回答 AI 問你的問題 |
| `/mode <手機> ai\|normal` | 切換手機模式（手機名稱，或 `/status` 裡的編號） |
| `/contacts <名字>` | 找聯絡人 |
| `/block <號碼> [名稱／原因]` | 把號碼加入 ⛔ **黑名單**（拒接名單） |
| `/stall <號碼> [名稱／備註]` | 把號碼加入 🎭 **啟動 Stalls 名單**（接起第 0 秒直接強制進入陪聊拖時間） |
| `/white <號碼> [名稱／備註]` | 把號碼加入 ⭐ **白名單**（重要親友／支援直接轉接或緊急事件轉接） |
| `/unblock <號碼>` | 從黑名單或啟動 Stalls 名單移除，恢復正常接聽 |

也可以直接打字：「幫我打給 0912345678 訂今晚七點兩位」「昨天有誰打來」「把 0912345678 加進黑名單」「把 0223456789 設成啟動 Stalls 陪聊」「把 0988111222 加進白名單」。這種自由文字會交給 Gemini 最便宜的模型（用*設定*裡的金鑰）轉成上面的指令；沒有金鑰時，機器人只回一句提示請你用 `/help`。

## 哪些事會先問你

撥電話、切換手機模式、同意或忽略待辦，以及模型從你的一般文字讀出來「要 AI 在通話中說的話」或「回答 AI 問題」（直接打 `/say` 或回覆那則問題則會立刻送出），都會先回一則摘要和 **✅ 執行／✖ 取消** 按鈕（也可以回覆 **1**／**2**）。

- 只等 **2 分鐘**，過了就作廢。
- 只有下指令的人、在同一個聊天室才能確認，而且只能用一次；新的請求會取代舊的。已經在網頁控制台或卡片上處理過的待辦，不會被改掉。
- 撥電話前就先檢查：緊急、短碼、加值付費號碼一律不撥，要有那個號碼國家的 SIM 卡等等，跟網頁控制台完全一樣。

## AI 通話中問你的問題

AI 代打時拿不準要你決定，問題也會傳到所有允許下指令的聊天室，AI 列的選項會變成按鈕。40 秒內點選或回覆那則訊息就好；網頁控制台照樣能答，先回答的算數。因為現在可以從聊天室回答，就算沒開網頁控制台，AI 也會等你最多 40 秒。

AI 代打的結果照舊用一般通知送出。

## 安全

- 預設關閉，每個聊天室各自一個開關。其他聊天室或其他人傳來的訊息**完全不回覆**（只記在 log），別人無從知道這個機器人存在。
- 每個聊天室有上限：**每小時代打幾通**（預設 5，最多 30，0 = 只能在網頁控制台叫）、**每分鐘幾則訊息**（預設 20，超過就安靜到這一分鐘結束）。兩個都在*設定 → 通知*。
- 代打的號碼檢查、SIM 卡規則、排隊跟網頁控制台建立的完全一樣。
- 回覆絕不包含金鑰或 token：agent 持有的任何金鑰都會在送出前被遮掉。逐字稿只給一小段。
- 只有你自己傳的那句話會被當成指令。通話逐字稿、摘要、對方說的話都不會交給讀你訊息的模型，所以電話裡說的話不會變成指令。
- 任何能在已開啟指令的聊天室裡發言的人都能用。請用你和機器人的私人聊天室，或填上允許的使用者 ID。填的不是 ID（例如名字）也會保留、只是對不上任何人，名單不會因此變成「所有人」。名單也套用在待辦卡片的按鈕上；同一個機器人和聊天室開了兩次時，兩邊的名單都要符合。
- 指令選單只設定在允許下指令的聊天室，陌生人打開機器人看不到。轉傳的訊息不會被當成指令。

## 設定

| 設定 | 環境變數 | 預設 |
|---|---|---|
| 管道：*允許在這個聊天室下指令*（`control`） | — | 關 |
| 管道：*可以下指令的人*（`control_users`） | — | 聊天室裡的人都可以 |
| `telegram_control` | `TELEGRAM_CONTROL` | 關 |
| `telegram_control_users` | `TELEGRAM_CONTROL_USERS` | 聊天室裡的人都可以 |
| `control_errands_per_hour` | `CONTROL_ERRANDS_PER_HOUR` | 5（0–30） |
| `control_commands_per_minute` | `CONTROL_COMMANDS_PER_MINUTE` | 20（1–120） |

## 其他通知管道的操作方式（一鍵按鈕與通用指令 Webhook）

除了 Telegram 雙向聊天外，**所有其他通知管道**也都支援互動操作：

1. **通話摘要與預約通知的一鍵按鈕（免輸入密碼、HMAC 簽章保護）**
   - 當設定好「這個控制台的網址」（`public_url`）時，每通電話的摘要通知與預約確認通知都會自動附上單次簽章動作按鈕／連結：
     - 通話摘要：**⛔ 加入黑名單**、**🎭 設為陪聊（Stalls）**、**⭐ 加入白名單**
     - 行程預約：**同意**、**不同意**
   - 原生按鈕支援：
     - **Telegram**：Inline Keyboard 按鈕
     - **ntfy**：原生 Action 按鈕（在手機通知欄直接點按）
     - **Home Assistant**：Companion App 通知 Action 按鈕與點擊跳轉（`clickAction`）
     - **Microsoft Teams**：Adaptive Card `Action.OpenUrl` 按鈕
     - **Bark / Pushover / Gotify / Chanify**：點擊通知直接跳轉連結（`url` / `click` / `link`）
     - **Discord / Slack / LINE / Matrix / 企業微信 / 釘釘 / 飛書 / Email**：訊息內附帶可點擊的簽章連結

2. **通用指令端點 `POST /calls/api/control/inbound`（讓任何管道都能下指令）**
   - Home Assistant、n8n、Apple 捷徑（Shortcuts）、Discord/Slack/LINE 自架機器人等，都可以直接 POST 文字指令或自然語言到：
     ```bash
     curl -X POST http://bageo.local:8787/calls/api/control/inbound \
       -H "Content-Type: application/json" \
       -H "Authorization: Bearer <外部存取密碼或MCP_TOKEN>" \
       -d '{"text": "/status", "channel_id": "ntfy", "reply": true}'
     ```
   - `text` 支援所有 `/status`、`/calls`、`/errand`、`/block`、`/stall`、`/white` 等斜線指令與自然語言；若 `"reply": true`，主程式還會把執行結果推播回指定的通知管道。

## 給開發者

指令、檢查、確認和上限都在 `crates/agent/src/control/`；要接新的聊天軟體，只要寫一層薄薄的轉接：把訊息轉成 `control::Inbound` 交給 `Controller::handle`，或直接呼叫 `POST /calls/api/control/inbound`。

AI 代理（Claude、ChatGPT、Grok、OpenClaw（龍蝦）…）可以透過內建的 MCP 伺服器使用同一組動作：見 [MCP](MCP.md)。

