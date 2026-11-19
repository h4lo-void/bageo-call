# 在聊天室操作：用 Telegram、Discord、Slack 或 Matrix 遙控 Bageo

人不在控制台前面時，可以在收通知的聊天室裡操作 Bageo：

- 查通話紀錄。
- 叫 AI 代打電話。
- 處理待處理事項。
- 回答 AI 通話中問你的問題。
- 把手機切到 AI 接聽或一般手機。

支援 Telegram、Discord、Slack 和 Matrix。之後可以接其他聊天軟體和 AI 代理（見[給開發者](#給開發者)）。

## 怎麼開

聊天室指令預設**關閉**，要在每個聊天室各自打開。請照你用的聊天軟體往下做：先設好它的通知管道，再打開指令。

### Telegram

1. 先設定好 Telegram 通知管道（[通知 → Telegram](NOTIFICATIONS.md#telegram)）。
2. *設定 → 通知* → 那個 Telegram 管道 → 勾選**允許在這個聊天室下指令**。
3. 在**可以下指令的人**填 Telegram 使用者 ID（問 **@userinfobot** 就知道自己的）。群組一定要填：沒填就沒有人能下指令。和機器人的私訊可以留空。
4. 按**儲存變更**，再傳 `/help` 給機器人。

用環境變數設定的機器人（`TELEGRAM_BOT_TOKEN` + `TELEGRAM_CHAT_ID`）：

- 在*設定 → 通知*的「在聊天室操作」裡有自己的開關，或設 `TELEGRAM_CONTROL=true`。
- 要指定可以下指令的人，設 `TELEGRAM_CONTROL_USERS=123456789,987654321`。群組要設，私訊可以不設。

在群組裡，機器人只理會指令（`/calls` 或 `/calls@你的機器人`）和回覆它自己訊息的話，其他人聊天一律不理。

### Discord

1. 打開 <https://discord.com/developers/applications>，按「New Application」，再打開「Bot」。
2. 在「Bot」頁面按「Reset Token」，複製權杖。
3. 同一頁把 **Message Content Intent** 打開。沒打開的話，機器人讀不到你打的字。
4. 邀請機器人：到「OAuth2 → URL Generator」，勾選「bot」，再勾選「View Channels」「Send Messages」「Read Message History」。打開它產生的網址，選你的伺服器。
5. 如果是私人頻道，也要讓機器人在該頻道有讀寫權限。
6. 設定好 Discord 通知管道（[通知 → Discord](NOTIFICATIONS.md#discord)），填「Bot 權杖」和「頻道 ID」。
7. 找頻道 ID：先在「使用者設定 → 進階」打開開發者模式，再在頻道上按右鍵 →「複製頻道 ID」。
8. 勾選**允許在這個聊天室下指令**。
9. 在**可以下指令的人**填你的 Discord 使用者 ID（開發者模式打開後，在自己的名字上按右鍵 →「複製使用者 ID」）。伺服器裡的頻道一定要填：沒填就沒有人能下指令。和機器人的私訊可以留空。
10. 按**儲存變更**，再在頻道裡傳 `/help`。

### Slack

只有 Slack 可以下指令，Mattermost 不行。

1. 到 <https://api.slack.com/apps> 建立 App（「Create New App → From scratch」）。
2. 打開「Socket Mode」，建立一組 App 層級權杖，權限選 `connections:write`。它以 `xapp-` 開頭，複製下來。
3. 打開「Event Subscriptions」，在「Subscribe to bot events」加入 `message.channels` 和 `message.im`。
4. 打開「Interactivity & Shortcuts」，把「Interactivity」打開，按鈕才按得動。
5. 打開「OAuth & Permissions」，在「Bot Token Scopes」加入 `chat:write`、`channels:history`、`im:history`。
6. 按「Install to Workspace」，複製 Bot 權杖（`xoxb-` 開頭）。
7. 把 App 加進要用的頻道：在頻道裡打開「Integrations → Add apps」。
8. 設定好 Slack 通知管道（[通知 → Slack / Mattermost](NOTIFICATIONS.md#slack--mattermost)），填「Bot 權杖」「App 層級權杖（聊天室操作用）」和「頻道 ID」。
9. 找頻道 ID：打開頻道詳細資料，在最下面，`C` 開頭。
10. 勾選**允許在這個聊天室下指令**。
11. 在**可以下指令的人**填你的 Slack 成員 ID（個人檔案 → ⋮ →「複製成員 ID」）。頻道一定要填：沒填就沒有人能下指令。和機器人的私訊可以留空。
12. 按**儲存變更**，再傳「`@你的機器人 /help`」。Slack 會把 `/` 開頭的訊息當成它自己的指令，所以要先寫機器人的名字。

### Matrix

只能用未加密的聊天室。Element 的私訊預設是加密的，請另外開一個聊天室。

1. 建立一個機器人專用的 Matrix 帳號。
2. 用它登入 Element，複製權杖：「All settings → Help & About → Advanced → Access Token」。把它當密碼保管。
3. 用你自己的帳號建立新聊天室，並關掉「Enable end-to-end encryption」。建立後就不能再關。
4. 邀請機器人帳號進聊天室，再用機器人帳號加入。
5. 在「Room settings → Advanced」找聊天室 ID（`!` 開頭）。填聊天室別名（`#名稱:example.org`）也可以。
6. 設定好 Matrix 通知管道（[通知 → Matrix](NOTIFICATIONS.md#matrix)），填「Homeserver 網址」「存取權杖」和「聊天室 ID」。
7. 勾選**允許在這個聊天室下指令**。
8. 在**可以下指令的人**填你的 Matrix ID，例如 `@lee:example.org`（在 Element 的個人資料看得到）。有其他人的聊天室一定要填：沒填就沒有人能下指令。只有你和機器人的聊天室可以留空。
9. 按**儲存變更**，再傳「`@你的機器人 /help`」。Element 會把 `/` 開頭的訊息當成它自己的指令，所以要先寫機器人的名字。

Matrix 沒有按鈕。要確認時，直接回覆 **1** 或 **2**。

## 指令

在 Discord 或 Slack 的頻道，以及超過兩個人的 Matrix 聊天室裡，機器人只理會 `/` 開頭的指令、提到它的訊息，和回覆它自己訊息的話。Slack 和 Element 請寫成 `@你的機器人 /calls`。

`1`、`#2` 這種編號，指的是這個聊天室最近一次列出的清單（`/calls`、`/tasks`、`/errands`、`/status`）。

| 指令 | 做什麼 |
|---|---|
| `/help` | 列出指令 |
| `/status` | Bageo 版本、每支手機的狀態（AI 接聽／一般手機／離線／通話中）、進行中的通話 |
| `/calls [筆數] [today\|yesterday\|YYYY-MM-DD] [missed\|scam] [關鍵字或號碼]` | 最近的通話，一通一行：時間、對方、摘要第一行。預設 5 筆，最多 20 |
| `/call <#>` | 某一通：摘要、逐字稿開頭（最多 600 字）、控制台連結 |
| `/tasks` | 等你處理的待處理事項（預約、留言） |
| `/confirm <#>`・`/ignore <#>` | 同意／忽略待處理事項，跟控制台上按一樣 |
| `/errand <號碼或聯絡人> <要辦的事>` | 叫 AI 幫你打電話，例如 `/errand 0912345678 訂今晚七點兩位`。名字有空格請加引號 |
| `/errands` | AI 代打的電話與結果 |
| `/cancel [#]` | 停止 AI 代打（不填編號 = 正在進行的那通） |
| `/say [#] <內容>` | 通話中交代 AI 一句話 |
| `/answer <內容>` | 回答 AI 問你的問題 |
| `/mode <手機> ai\|normal` | 切換手機模式（手機名稱，或 `/status` 裡的編號） |
| `/contacts <名字>` | 找聯絡人 |
| `/block <號碼> [名稱／原因]` | 把號碼加入 **黑名單**（來電一律拒接） |
| `/stall <號碼> [名稱／備註]` | 把號碼加入 **陪聊名單**（一接起就陪聊拖時間） |
| `/white <號碼> [名稱／備註]` | 把號碼加入 **白名單**（重要親友，可以直接轉到你的手機，或讓 AI 先接、有急事才轉給你） |
| `/unblock <號碼>` | 從黑名單或陪聊名單移除，恢復正常接聽 |

也可以直接打字，不用記指令，例如：

- 「幫我打給 0912345678 訂今晚七點兩位」
- 「昨天有誰打來」
- 「把 0912345678 加進黑名單」
- 「把 0223456789 加進陪聊名單」
- 「把 0988111222 加進白名單」

這種一般文字會交給 Gemini 最便宜的模型（用*設定*裡的金鑰）轉成上面的指令。沒有金鑰時，機器人只回一句提示，請你用 `/help`。

## 哪些事會先問你

這幾件事會先回一則摘要，附 **執行／取消** 按鈕（也可以回覆 **1**／**2**），你按了才做：

- 撥電話。
- 切換手機模式。
- 同意或忽略待處理事項。
- 模型從你的一般文字讀出來的「要 AI 在通話中說的話」或「回答 AI 問題」。直接打 `/say` 或回覆那則問題，則會立刻送出。

確認的規則：

- 只等 **2 分鐘**，過了就作廢。
- 只有下指令的人、在同一個聊天室才能確認，而且只能用一次。
- 新的請求會取代舊的。已經在控制台或卡片上處理過的待處理事項，不會被改掉。
- 撥電話前就先檢查，跟控制台完全一樣：緊急、短碼、加值付費號碼一律不撥，要有那個號碼國家的 SIM 卡等等。

## AI 通話中問你的問題

AI 代打時拿不準、要你決定，問題會傳到所有允許下指令的聊天室，AI 列的選項會變成按鈕。

- 40 秒內點選按鈕，或回覆那則訊息。
- 控制台照樣能答，先回答的算數。
- 現在可以從聊天室回答，所以就算沒開控制台，AI 也會等你最多 40 秒。

AI 代打的結果照舊用一般通知送出。

## 安全

- 預設關閉，每個聊天室各自一個開關。
- 其他聊天室或其他人傳來的訊息**完全不回覆**（只記在記錄檔），別人無從知道這個機器人存在。
- 每個聊天室有兩個上限，都在*設定 → 通知*的「在聊天室操作」：
  - **每個聊天室每小時最多代打幾通**：預設 5，最多 30，0 = 只能在控制台叫。
  - **每個聊天室每分鐘最多幾則訊息**：預設 20，超過就安靜到這一分鐘結束。
- 代打的號碼檢查、SIM 卡規則、排隊跟控制台建立的完全一樣。
- 回覆絕不包含金鑰或密碼：Bageo 持有的任何金鑰都會在送出前被遮掉。逐字稿只給一小段。
- 只有你自己傳的那句話會被當成指令。通話逐字稿、摘要、對方說的話都不會交給讀你訊息的模型，所以電話裡說的話不會變成指令。
- 在群組、頻道或有其他人的聊天室，只有**可以下指令的人**裡的人能用。沒填名單就沒有人能用，第一則訊息會收到一小段如何加入名單的提示。和機器人的私訊不用填名單：Telegram 私訊、Slack 私訊、Discord 私訊，或只有你和機器人的 Matrix 聊天室。
- **可以下指令的人**填的不是 ID（例如名字）也會保留，只是對不上任何人，名單不會因此變成「所有人」。
- 這份名單也套用在待處理事項卡片的按鈕上；同一個機器人和聊天室開了兩次時，兩邊的名單都要符合。
- 用 MCP 的 token 打指令 API（`POST /calls/api/control/inbound`）時，只能做 `mcp_calls` 允許的事。設成 `off` 時只能查看，不能改任何東西。
- 指令選單只設定在允許下指令的聊天室，陌生人打開機器人看不到。轉傳的訊息不會被當成指令。

## 設定

| 設定 | 環境變數 | 預設 |
|---|---|---|
| 管道：*允許在這個聊天室下指令*（`control`） | — | 關 |
| 管道：*可以下指令的人*（`control_users`） | — | 群組沒填就沒有人；私訊不用填 |
| `telegram_control` | `TELEGRAM_CONTROL` | 關 |
| `telegram_control_users` | `TELEGRAM_CONTROL_USERS` | 群組沒填就沒有人；私訊不用填 |
| `control_errands_per_hour` | `CONTROL_ERRANDS_PER_HOUR` | 5（0–30） |
| `control_commands_per_minute` | `CONTROL_COMMANDS_PER_MINUTE` | 20（1–120） |

## 其他通知管道的操作方式（一鍵按鈕與通用指令 Webhook）

除了 Telegram、Discord、Slack 和 Matrix 雙向聊天外，**所有其他通知管道**也都支援互動操作：

1. **通話摘要與預約通知的一鍵按鈕（免輸入密碼、HMAC 簽章保護）**
   - 在「設定 → 通知」填好「通知裡的網址」（`public_url`）後，每通電話的摘要通知與預約確認通知都會附上單次簽章的動作按鈕或連結：
      - 通話摘要：**加入黑名單**、**設為陪聊**、**加入白名單**
      - 行程預約：**同意**、**不同意**
   - 各管道的呈現方式：
      - **Telegram**：Inline Keyboard 按鈕
      - **ntfy**：原生 Action 按鈕（在手機通知欄直接點按）
      - **Home Assistant**：Companion App 通知 Action 按鈕與點擊跳轉（`clickAction`）
      - **Microsoft Teams**：Adaptive Card `Action.OpenUrl` 按鈕
      - **Bark / Pushover / Gotify / Chanify**：點擊通知直接跳轉連結（`url` / `click` / `link`）
      - **Discord / Slack / LINE / Matrix / 企業微信 / 釘釘 / 飛書 / Email**：訊息內附帶可點擊的簽章連結
      - **Webhook（JSON）**：結構化的 `actions: [{ "label": "...", "url": "..." }]` 陣列
   - 點連結會先打開確認頁，按下頁面上的按鈕才會執行；連結 7 天後失效。背後是 `GET/POST /calls/api/quick-action`。

2. **通用指令端點 `POST /calls/api/control/inbound`（讓任何管道都能下指令）**
   - Home Assistant、n8n、Make、Apple 捷徑（Shortcuts）、Discord／Slack／LINE／企業微信／飛書自架機器人等，都可以直接 POST 文字指令或自然語言到：
      ```bash
      curl -X POST http://bageo.local:8787/calls/api/control/inbound \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer <控制台密碼或 MCP token>" \
        -d '{"text": "/status", "channel_id": "ntfy", "reply": true}'
      ```
   - JSON 欄位：
      - `text`：要下的指令。支援全部 18 個斜線指令（`/status`、`/calls`、`/errand`、`/block`、`/stall`、`/white`、`/unblock` 等）和 9 種語言的自然語言。
      - `channel_id`：要把回覆推到哪個通知管道。
      - `sender_id`：下指令的人。
      - `reply`：`true` 時，Bageo 會把執行結果推到那個通知管道。
      - `token`：密碼放在這裡也可以。
   - `"reply": true` 只會推到「已啟用、而且 id、名稱或服務種類等於 `channel_id`」的那一個通知管道；沒給 `channel_id` 或對不上就不推。
   - 驗證用控制台密碼或 MCP 的 token，放在 `Authorization: Bearer`、`X-Calls-Token` 標頭，或 JSON 的 `token` 欄位。兩個都沒設定時不檢查。
   - 回傳的 JSON 有 `ok`、`text`（回覆內容）、`buttons` 和 `pushed`（有沒有推出去）。

## 測試程度

Discord、Slack、Matrix 只用模擬的伺服器測試過，還沒在真實帳號上試過。

## 給開發者

指令、檢查、確認和上限都在 `crates/agent/src/control/`。要接新的聊天軟體，只要寫一層薄薄的轉接：把訊息轉成 `control::Inbound` 交給 `Controller::handle`，或直接呼叫 `POST /calls/api/control/inbound`。

AI 代理（Claude、ChatGPT、Grok、OpenClaw（龍蝦）…）可以透過內建的 MCP 伺服器使用同一組動作：見 [MCP](MCP.md)。

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
