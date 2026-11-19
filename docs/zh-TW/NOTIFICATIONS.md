# 通知：通話摘要傳到 Telegram、LINE、Discord、Email 等 32 種管道

AI 電話助理接完電話，掛斷幾秒後，你的手機就會收到一則摘要：誰打來、要做什麼、AI 怎麼回、你要處理什麼。

Bageo 也會在這些時候通知你：

- 來電的時候。
- 黑名單號碼或通話中插播被擋下的時候。
- AI 回電或 AI 代打結束的時候。

支援 32 種服務，從 Telegram、LINE、Teams、Signal 到企業微信、ntfy、Home Assistant、Email，以及你自己的 Webhook。

**一分鐘設定好：**

1. 打開「設定 → 通知」。
2. 在「通知管道」下選一個服務，按「**新增管道**」。
3. 填好欄位。各服務要填什麼，見[各項服務](#各項服務)。
4. 按「**傳送測試**」，確認有收到。
5. 按「**儲存變更**」。

**先知道這幾件事：**

- 管道想加幾個都可以，**每則訊息都會送到所有啟用的管道**。
- 每個管道可以取「名稱」（例如「家人群組」）。想暫時停用，關掉「啟用」就好，不用刪除。
- 網址和權杖存在你電腦上的 `data/calls/config/settings.json`。網頁控制台之後只顯示提示，不會再完整顯示；編輯時欄位留空，就保留原本的值。
- 最上面的「傳送通知」關掉後，通話相關訊息和 Telegram 預約卡片都會停，管道不會被刪除。
- 腳本透過 API 傳的訊息（見[下方](#傳送你自己的訊息)），例如轉發的簡訊，不受「傳送通知」影響，照樣會送。
- 訊息用「通知和摘要的語言」撰寫（預設「跟 AI 講話的語言相同」），跟通話講什麼語言無關。
- 太長的訊息會依各服務的上限截短（例如 Pushover 1,000 字元，Discord 和企業微信 1,900 字元）。

> **測試程度：** Telegram 用得最多。其他服務依公開文件實作，請求內容有單元測試，但在真實帳號上用得還不多。哪一個失敗了，「傳送測試」會顯示該服務的回應，歡迎回報。

> 1.0 之後新增的服務（Microsoft Teams、Google Chat、Rocket.Chat、Zulip、Webex、Nextcloud Talk、Synology Chat、Signal、Pushbullet、PushPlus、PushDeer、WxPusher、Chanify、SMTP 電子郵件、Home Assistant、IFTTT）都還沒有在真實帳號上試過。

## 訊息長什麼樣子

一則來自聯絡人的通話摘要：

```text
通話摘要 · 小傑
號碼：0912345678
手機：Pixel 8
時間：11月12日 18:02

對方：小傑（朋友）
事情：明天中午要不要一起吃飯？
AI 回覆：說聽起來不錯，沒有敲定時間
要處理：回電給小傑確認
行事曆：和小傑吃午餐 (11/13 12:00)
https://calendar.google.com/calendar/render?action=TEMPLATE&…
逐字稿／錄音：http://192.0.2.10:8787/calls/#call=…
```

- **標題**：打來的人在「聯絡人」裡時，後面會加名字。
- **行事曆那幾行**：通話中講定了日期、而且「建議行事曆活動」開著時才會出現（[設定參考](SETTINGS.md#摘要與行事曆)）。
- **行事曆連結**：打開你第一次按「加入行事曆」時選的那一家（`calendar_app`）。還沒選時不附連結，要打開最後一行的通話頁，按「加入行事曆」選一家。詳見下方[行程預約](#行程預約)。
- **最後一行**：連到這通電話的網頁，Bageo 有其他裝置打得開的網址時才會出現。網址可以是「設定 → 通知」裡的「通知裡的網址」，或控制台對區網開放時自動帶入的網址（Docker 裡不會自動帶入）。見[從其他裝置使用控制台](SETTINGS.md#從其他裝置使用控制台)。

## 你會收到哪些訊息

你會收到這幾種訊息：

| 標題 | 什麼時候 | Webhook `event` |
|---|---|---|
| 來電 | 電話被接起來。 | `incoming` |
| 去電 | Bageo 自己撥出電話：AI 代打、預約回電或待辦回電。代打設成「撥到通」時，只有第一次撥號會通知。 | `outgoing` |
| 黑名單來電 | 黑名單上的號碼打來。訊息會寫是響鈴時直接拒接，還是接起來播語音，以及你當初填的原因。 | `blocked` |
| 通話摘要 | 通話結束幾秒後：誰、什麼事、AI 怎麼回、你要處理什麼，以及連結。 | `summary` |
| 通話中插播未接 | 通話中又有第二通打進來，手機把它拒接了（對方聽到忙線）。知道號碼的話記得回電。 | `call_waiting` |
| 同一號碼連續來電 | 同一個號碼一分鐘內一直打來。你會收到一則說明又打了幾通的訊息，不會每通都通知。 | `repeat_calls` |
| AI 回電：〈號碼〉 | AI 打給來電者告知預約結果。訊息會寫結果（接通並告知、沒人接、撥不出去，或這個號碼不能回電）和那通電話的摘要。 | `ai_callback` |
| AI 代打：〈名字或號碼〉 | 你交辦的 AI 代打結束了。訊息有結果、細節、需要你決定的選項、下一步、談定的時間，以及「AI 代打」頁的連結。 | `ai_errand` |
| 預約待你確認 | 對方跟 AI 約了時間。Telegram 收到有按鈕的卡片，其他管道收到附一鍵「同意／不同意」簽章按鈕或連結的通知（見[行程預約](#行程預約)）。 | `appointment_pending` / `summary` |
| 來電已轉到你的手機 | 「誰打來可以轉」勾選的人打來、且「轉接方式」是「不接，直接轉到你的手機」時，手機不接，讓電信把電話轉給你，並馬上通知你。 | `whitelist_direct_transfer` |
| 來電急事通知 | 「誰打來可以轉」勾選的人打來、AI 先接，對方說有急事時馬上通知你；轉接方式選「AI 先接，有急事才轉給你」並填好你的號碼時，手機也會撥給你。 | `whitelist_urgent_transfer` |
| 測試通知 | 你按了「傳送測試」。 | `test` |
| 聊天指令回覆 | 呼叫 `POST /calls/api/control/inbound` 並帶入 `"reply": true` 時發出的回覆。 | `control_reply` |
| 新簡訊 | 選用的[簡訊轉發工具](../../tools/sms_forward/README.md)（英文）轉來的簡訊。 | `event` |
| 自動備份失敗 | 排程自動備份失敗或寫入發生錯誤。 | `event` |

同一個號碼一直打來，不會洗版：每個號碼一分鐘內，「來電」「黑名單來電」「通話中插播未接」各最多通知兩次，一分鐘後用一則「同一號碼連續來電」告訴你又打了幾通。急事警報、預約、通話摘要、AI 代打與回電、語音留言、去電和簡訊不受影響，一定會通知。

插播未接的訊息：
```text
通話中插播未接：0912345678（Pixel 9）
時間：11月14日 18:02
```

AI 代打的結果，例如 AI 幫你訂好的餐廳：

```text
AI 代打：鼎泰豐
號碼：0223456789
手機：Pixel 9
時間：11月14日 18:05

辦好了：已訂明天 7 點半 4 位，名字王先生
細節：座位保留 15 分鐘。
約定時間：11月15日 19:30 鼎泰豐晚餐
http://bageo.local/calls/#errands
```

AI 在你來不及回答的問題上先答應了的話，結果會是「要你決定」，並多一行請你確認，不行要打回去改。

## 行程預約

開了「行程預約」（[設定參考](SETTINGS.md#摘要與行事曆)）後，對方跟 AI 約好時間的那通電話，摘要下面會多一行（取代行事曆那幾行），每個管道都收得到：

```text
預約待你確認：11/14（週六）15:00–16:00 看房 — 到網頁確認：http://bageo.local/calls/#call=…
```

**在網頁上決定：**

1. 打開「通話」頁，找最上方的「**待處理事項**」，或打開那通電話的詳情。
2. 選怎麼告訴對方：**傳簡訊**、**AI 回電**或**不通知對方**。
3. 按 **同意** 或 **不同意**。

- 通知裡的連結會打開那通電話。控制台沒有其他裝置打得開的網址（「通知裡的網址」）時，這行改寫「到網頁控制台確認」。
- 完全沒設通知管道時不會推播，預約照樣在網頁上等你。
- Bageo 不會自己把任何東西寫進行事曆。

**在 Telegram 上一鍵決定：**用 `TELEGRAM_BOT_TOKEN` 和 `TELEGRAM_CHAT_ID` 指定了 Telegram Bot（見[下方](#改用環境變數設定)）、「傳送通知」也開著時，等你決定的預約另外會有一張卡片。

只在網頁控制台新增的 Telegram 管道，收得到上面那行提醒，但收不到卡片。

```text
預約待確認

號碼：0912345678
時間：11/14（週六）15:00–16:00
事由：看房
稱呼：陳先生
手機：Pixel 8
簡訊：按同意或不同意後會傳簡訊告訴對方
網頁確認：http://bageo.local/calls/#call=…
[同意] [不同意]
```

「簡訊」這一行照這個號碼的「預設回覆方式」寫，也可能是下面幾種：

- 「回覆方式：按同意或不同意後，AI 會打電話告訴對方」（市話，或沒開簡訊）。
- 「不會傳（設定裡「傳簡訊告訴對方」沒開）」。
- 「對方號碼收不到簡訊，請自己回電」。
- 「未顯示號碼，沒辦法通知對方」。

在卡片上按，用的是「預設回覆方式」；想換方式，請到網頁上決定。

按了之後，Bot 會回覆結果：

```text
已同意預約
11/14（週六）15:00–16:00 看房
已傳簡訊給 0912345678
加入行事曆：https://calendar.google.com/calendar/render?action=TEMPLATE&…
```

其他可能的回覆：

- 「已婉拒預約」
- 「AI 正在回電給 0912345678」
- 「簡訊沒傳出：手機沒連線」、「手機 App 太舊，不支援傳簡訊」，或「失敗代碼 no_service」

簡訊沒傳出時，網頁上那筆預約會出現「**重傳簡訊**」，可以用原本那支或另一支在線的手機再傳。

**「加入行事曆」哪一家都能用。**Bot 回覆、通知和網頁裡的連結，會打開你第一次按「加入行事曆」時選的行事曆（設定 `calendar_app`）：

- Google 日曆、Outlook.com、Microsoft 365 或 Yahoo 日曆：已經填好的「新增活動」表單。
- Apple 日曆或 `.ics`：這個活動在「通知裡的網址」底下的 `.ics` 檔。

還沒選時，Bot 回覆附的是這通電話在網頁的頁面（沒有控制台網址就不附）。按「加入行事曆」選一家，之後就記住。

`.ics` 連結帶的是只能開這一個檔案的金鑰（HMAC），不是你的控制台密碼，從聊天室點開也安全。沒填控制台網址就沒有 `.ics` 連結。

Bageo 不連任何行事曆帳號。

**只有第一次決定算數**，不管在哪裡決定（網頁、卡片或 [API](API.md#行程預約與任務)）。再決定一次（就算是同時按）會回「這個預約已經處理過了」。

在網頁或 API 決定後，卡片上的按鈕會拿掉，Bot 也會在聊天裡回同樣的結果。你決定之後才送到的卡片沒有按鈕。Telegram 連不上也不影響決定。

告訴對方結果的 AI 回電，講的是那通電話的語言：等對方接起來出聲，說是誰打來、說一次你的回覆，然後道別。它的通知是「AI 回電：〈號碼〉」；撥不出去也會通知你。

推銷、詐騙電話不會留下預約。銀行防詐專線、警察通知詐騙這類電話不算。

通話後才被判定是這類電話時（摘要標成「推銷電話」或「疑似詐騙」），預約會作廢、不發卡片，摘要會寫：

```text
判定是推銷電話，已取消通話中排入的預約：11/14（週六）15:00–16:00 看房
```

## 各項服務

挑一個你每天都會看的 App：

| 服務 | 需要準備 | 常見用途 |
|---|---|---|
| [Telegram](#telegram) | Bot 權杖、聊天 ID | 通用；預約卡片只有它有 |
| [Discord](#discord) | Webhook 網址，或 Bot 權杖和頻道 ID | Discord 伺服器 |
| [Slack / Mattermost](#slack--mattermost) | Incoming Webhook 網址，或 Bot 權杖和頻道 ID | 工作群組 |
| [Microsoft Teams](#microsoft-teams) | Workflow Webhook 網址 | 用 Microsoft 365 的公司 |
| [Google Chat](#google-chat) | Webhook 網址 | Google Workspace |
| [Rocket.Chat](#rocketchat) | Webhook 網址 | 自架團隊聊天 |
| [Zulip](#zulip) | 伺服器網址、Bot 信箱和 API 金鑰、頻道 | 開源團隊聊天 |
| [Webex](#webex) | Bot 存取權杖、聊天室 ID 或信箱 | Cisco Webex |
| [Nextcloud Talk](#nextcloud-talk) | 伺服器網址、使用者和應用程式密碼、對話代碼 | 自架 Nextcloud |
| [Synology Chat](#synology-chat) | Webhook 網址 | 群暉 NAS 使用者 |
| [LINE](#line) | Channel access token、使用者或群組 ID | 台灣、日本、泰國 |
| [Signal](#signal) | 自己架的 signal-cli-rest-api、寄件號碼、收件人 | Signal 使用者 |
| [ntfy](#ntfy) | 主題 | 自架、Android 推播 |
| [Gotify](#gotify) | 伺服器網址、App 權杖 | 自架 |
| [Bark (iOS)](#bark-ios) | 裝置金鑰 | iPhone 推播 |
| [Pushover](#pushover) | API 權杖、使用者金鑰 | iOS 與 Android 推播 |
| [Pushbullet](#pushbullet) | 存取權杖 | Android 與電腦推播 |
| [企業微信群機器人（WeCom）](#企業微信群機器人wecom) | Webhook 網址 | 企業微信 |
| [釘釘機器人（DingTalk）](#釘釘機器人dingtalk) | Webhook 網址、加簽密鑰 | 釘釘 |
| [飛書／Lark 機器人](#飛書lark-機器人) | Webhook 網址 | 飛書、Lark |
| [Server醬（ServerChan）](#server醬serverchan) | SendKey | 推到微信 |
| [PushPlus](#pushplus) | Token | 推到微信（推送加） |
| [PushDeer](#pushdeer) | PushKey | iOS 與 Android 推播，可自架 |
| [WxPusher](#wxpusher) | SPT，或 appToken 加 UID | 推到微信 |
| [Chanify](#chanify) | Token | iOS 與 Android 推播，可自架 |
| [Matrix](#matrix) | Homeserver 網址、存取權杖、聊天室 ID | Element 等 Matrix App |
| [電子郵件（Gmail）](#電子郵件gmail) | Gmail 地址、應用程式密碼 | Email |
| [電子郵件（SMTP）](#電子郵件smtp) | SMTP 網址、密碼 | 其他任何郵件服務 |
| [Home Assistant](#home-assistant) | 伺服器網址、存取權杖、通知服務 | 智慧家庭、Companion App |
| [IFTTT Webhooks](#ifttt-webhooks) | Webhooks 金鑰、事件名稱 | IFTTT Applet |
| [Apprise API](#apprise-api) | 通知網址 | 再連上百種服務 |
| [Webhook (JSON)](#webhook-json) | 網址 | n8n、Zapier、Make、Home Assistant、你自己的程式 |

### Telegram

1. 在 Telegram 跟 **@BotFather** 對話，傳 `/newbot`，取個名字。
2. 你會拿到一組像 `123456789:AA…` 的 **bot token**，這就是「Bot 權杖」。
3. 打開跟新機器人的聊天視窗，按 **Start**（機器人不能主動先傳訊息給你）。
4. 跟 **@userinfobot** 對話，它會回你的數字 ID，這就是「聊天 ID（Chat ID）」。
5. 要傳到群組，就把機器人加進群組，改用群組 ID（負數，例如 `-1001234567890`）。

| 欄位 | 填什麼 |
|---|---|
| Bot 權杖 | @BotFather 給的 token |
| 聊天 ID（Chat ID） | 你的數字 ID，或群組 ID |

想收到有按鈕的預約卡片，請把同一個 Bot 也用 `TELEGRAM_BOT_TOKEN` 和 `TELEGRAM_CHAT_ID` 指定；同一則訊息不會收到兩次。

想在這個聊天室查通話紀錄、叫 AI 代打或處理「待處理事項」，見[在聊天室操作](CHAT_COMMANDS.md)。要先在這個管道打開「允許在這個聊天室下指令」才會生效。

### Discord

1. 打開頻道設定 →「整合 → Webhook」。
2. 按「新 Webhook」。
3. 複製 Webhook 網址，貼到「Webhook 網址」欄位。

| 欄位 | 填什麼 |
|---|---|
| Webhook 網址 | `https://discord.com/api/webhooks/…` |

也可以不填 Webhook，改填 Bot 權杖和頻道 ID，兩種擇一就能收通知。用機器人還能傳私訊給你：「頻道 ID」填你自己的使用者 ID。要下指令就填 Bot 權杖和頻道 ID。步驟見[在聊天室操作](CHAT_COMMANDS.md#discord)。

| 欄位 | 填什麼 |
|---|---|
| Bot 權杖 | 機器人的權杖 |
| 頻道 ID | 頻道的 ID；要收私訊就填你自己的使用者 ID |

### Slack / Mattermost

**Slack：**

1. 到 <https://api.slack.com/apps> 建立 App。
2. 開啟「Incoming Webhooks」。
3. 按「Add New Webhook to Workspace」。
4. 複製網址。

**Mattermost：**到「Integrations → Incoming Webhooks」。

| 欄位 | 填什麼 |
|---|---|
| Incoming Webhook 網址 | `https://hooks.slack.com/services/…` |

也可以不填 Webhook，改填 Bot 權杖和頻道 ID，兩種擇一就能收通知。要在 Slack 頻道裡下指令（只限 Slack，Mattermost 不行），下面三欄都要填。步驟見[在聊天室操作](CHAT_COMMANDS.md#slack)。

| 欄位 | 填什麼 |
|---|---|
| Bot 權杖 | `xoxb-` 開頭的權杖 |
| App 層級權杖（聊天室操作用） | `xapp-` 開頭的權杖 |
| 頻道 ID | 頻道的 ID |

### Microsoft Teams

舊的 Office 365 連接器（`webhook.office.com` 開頭的網址）已經停用。這裡用的是 **Workflows（工作流程）** 的 Webhook，送出的是 Adaptive Card。

1. 在 Teams 打開頻道的「**…**」選單 →「工作流程」。
2. 選「收到 Webhook 要求時張貼在頻道中」，選好團隊和頻道，做完設定。
3. 複製它顯示的網址（裡面含有簽章，請像密碼一樣保管）。

| 欄位 | 填什麼 |
|---|---|
| Workflow Webhook 網址 | 第 3 步的網址 |

訊息的每一行在卡片上各自成一行，第一行是粗體。

### Google Chat

需要 Google Workspace 帳戶，個人 Google 帳戶不能加 Webhook。

1. 打開聊天室，點聊天室名稱。
2. 選「應用程式與整合 → Webhook → 新增 Webhook」。
3. 複製網址。

| 欄位 | 填什麼 |
|---|---|
| Webhook 網址 | `https://chat.googleapis.com/v1/spaces/…/messages?key=…&token=…` |

### Rocket.Chat

1. 到「管理 → 工作區 → 整合 → 新增 → 傳入（Incoming）」。
2. 啟用它，選頻道和要用哪個使用者發文。
3. 儲存，複製「Webhook URL」。

| 欄位 | 填什麼 |
|---|---|
| Webhook 網址 | `https://chat.example.org/hooks/…/…` |

### Zulip

1. 到「個人設定 → Bot → 新增 Bot」建立一個 Bot（類型選「Incoming webhook」就夠了）。
2. 讓它訂閱要發的頻道。
3. 在 Bot 卡片上找到它的信箱和 API 金鑰。

| 欄位 | 填什麼 |
|---|---|
| 伺服器網址 | 你的組織，例如 `https://example.zulipchat.com` |
| Bot 信箱:API 金鑰 | 兩者用冒號連起來：`calls-bot@example.zulipchat.com:abc123…` |
| 頻道 > 主題，或信箱 | `calls > phone`；只填頻道（`calls`）會發到主題「Bageo」；填一個以上的信箱（用逗號分隔）則傳私訊 |

訊息超過 2,400 字元會截短，才不會超過 Zulip 10,000 位元組的上限。

### Webex

1. 到 <https://developer.webex.com/my-apps> 建立 Bot。
2. 複製它的 **Bot 存取權杖**（只會顯示一次）。
3. 把 Bot 加進聊天室。
4. 登入 developer.webex.com，用 Bot 的權杖執行「List Rooms」，查到聊天室 ID。
5. 要傳給某個人，就改填對方的信箱。

| 欄位 | 填什麼 |
|---|---|
| Bot 存取權杖 | Bot 的 token |
| 聊天室 ID 或信箱 | `Y2lzY29zcGFyazovL3Vz…` 或 `someone@example.com` |

訊息超過 1,800 字元會截短（Webex 最多收 7,439 位元組）。

### Nextcloud Talk

以一個 Nextcloud 使用者的身分發訊息，另開一個 Bageo 專用的使用者最清楚。

1. 在「個人設定 → 安全性 → 裝置與工作階段」替這個使用者建立**應用程式密碼**。
2. 把這個使用者加進對話。
3. 取對話連結的最後一段當對話代碼（`https://cloud.example.org/call/abc123xy` → `abc123xy`）。

| 欄位 | 填什麼 |
|---|---|
| 伺服器網址 | `https://cloud.example.org` |
| 使用者:應用程式密碼 | `bageo:xxxxx-xxxxx-xxxxx-xxxxx-xxxxx` |
| 對話代碼（token） | `abc123xy` |

### Synology Chat

1. 在 Synology Chat 點大頭貼 →「整合 → 傳入的 Webhook → 建立」。
2. 選頻道並儲存。
3. 複製 Webhook 網址。

| 欄位 | 填什麼 |
|---|---|
| Webhook 網址 | `https://nas.example.org:5001/webapi/entry.cgi?api=SYNO.Chat.External&method=incoming&version=2&token=…` |

Bageo 會檢查 HTTPS 憑證。NAS 還在用它自己簽的憑證的話，在自家網路裡改用 5000 埠的 `http://` 網址。

### LINE

LINE Notify 已在 2025 年停止服務；這裡用的是 **Messaging API**。

1. 到 <https://developers.line.biz/console/> 建立 provider 和「Messaging API」channel（會同時建立一個 LINE 官方帳號）。
2. 在該 channel 的「Messaging API」分頁發行 **channel access token（long-lived）**。
3. 用 LINE App 把這個官方帳號加為好友（同一分頁有 QR code）。
4. 你的**使用者 ID**（`U…`，33 個字元）在「Basic settings」分頁的「Your user ID」。
5. 要傳到群組，就把官方帳號邀進群組，改用群組 ID（`C…`）；群組 ID 要從 webhook 事件取得。

| 欄位 | 填什麼 |
|---|---|
| Channel access token | 那組 long-lived token |
| 使用者或群組 ID | `U…` 或 `C…` |

官方帳號的免費方案，每月可發的推播則數有限。

### Signal

Signal 沒有 Bot API，所以要透過你自己架的伺服器。

1. 自己架一個 [signal-cli-rest-api](https://github.com/bbernhard/signal-cli-rest-api)（一個 Docker 容器）。
2. 把它連結成你自己 Signal 帳號的第二台裝置，或用另一個號碼註冊。

| 欄位 | 填什麼 |
|---|---|
| API 伺服器網址 | 容器的位址，例如 `http://signal-api:8080` |
| 寄件號碼 | signal-cli 用的號碼，國際格式（`+15550100`） |
| 收件人 | 號碼（`+15550101`）或群組 ID（`group.…`），用逗號分隔 |

訊息超過 2,000 字元會截短。

### ntfy

1. 安裝 ntfy App。
2. 訂閱一個**不容易被猜到**的主題名稱。

在公用伺服器上，知道名稱的人都讀得到訊息。

| 欄位 | 填什麼 |
|---|---|
| 伺服器 | 留空就是 `https://ntfy.sh`，或填你自己的伺服器 |
| 主題（Topic） | 主題名稱 |
| 存取權杖（選填） | 只有自架伺服器上受保護的主題才需要（`tk_…`） |

### Gotify

1. 在 Gotify 打開「Apps → Create application」。
2. 複製它的 token。

| 欄位 | 填什麼 |
|---|---|
| 伺服器網址 | 你的 Gotify 伺服器，例如 `https://gotify.example.org` |
| App 權杖 | 該應用程式的 token |

### Bark (iOS)

1. 打開 iPhone 上的 Bark App。
2. 從範例網址複製裝置金鑰；範例網址長這樣：`https://api.day.app/<device key>/…`。

| 欄位 | 填什麼 |
|---|---|
| 伺服器 | 留空就是 `https://api.day.app`，或填你自己的 bark-server |
| 裝置金鑰 | App 裡的那組 key |

### Pushover

1. 到 <https://pushover.net/apps/build> 建立一個應用程式。
2. 複製它的 API token。
3. 在 Pushover 首頁複製你的 user key。

| 欄位 | 填什麼 |
|---|---|
| API 權杖 | 該應用程式的 token |
| 使用者金鑰（User key） | Pushover 首頁上你的 user key |

### Pushbullet

1. 到 pushbullet.com →「Settings → Account → Create Access Token」。
2. 複製產生的存取權杖。

免費帳號每月可推播的則數有限。

| 欄位 | 填什麼 |
|---|---|
| 存取權杖 | `o.…` |
| 裝置 ID 或信箱（選填） | 留空＝你所有的裝置；填裝置 ID（`iden`）只推到那台；填信箱則推給另一位 Pushbullet 使用者 |

### 企業微信群機器人（WeCom）

1. 在群組裡「新增群機器人」。
2. 複製 webhook 網址。

| 欄位 | 填什麼 |
|---|---|
| Webhook 網址 | `https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=…` |

### 釘釘機器人（DingTalk）

1. 打開群設定 →「機器人」→「自訂」。
2. 安全設定選「**加簽**」。
3. 把 webhook 和密鑰（`SEC…`）都複製下來。

如果改用關鍵字當安全設定，每則訊息都必須含有其中一個關鍵字；加簽比較簡單。

| 欄位 | 填什麼 |
|---|---|
| Webhook 網址 | `https://oapi.dingtalk.com/robot/send?access_token=…` |
| 加簽密鑰（選填） | `SEC…` 那組值；機器人沒設就留空 |

### 飛書／Lark 機器人

1. 打開群組 →「設定 → 群機器人 → 新增機器人 → 自訂機器人」。
2. 複製 webhook 網址（`open.feishu.cn` 或 `open.larksuite.com`）。

不要開簽章驗證，Bageo 不支援。自訂關鍵字或 IP 白名單都可以。

| 欄位 | 填什麼 |
|---|---|
| Webhook 網址 | `https://open.feishu.cn/open-apis/bot/v2/hook/…` |

### Server醬（ServerChan）

從 Server醬網站複製你的 SendKey。

| 欄位 | 填什麼 |
|---|---|
| SendKey | Turbo 版是 `SCT…`，Server醬³ 是 `sctp…` |

### PushPlus

1. 用微信登入 <https://www.pushplus.plus>。
2. 複製你的 **token**。

訊息會從 PushPlus 的微信公眾號送到。

| 欄位 | 填什麼 |
|---|---|
| Token | 你的 token |
| 群組編碼（選填） | 填群組編碼就發給該群組；留空只發給自己 |

### PushDeer

1. 安裝 PushDeer 並登入。
2. 在「Key」新增一把 **Key**。
3. 複製這把 Key。

| 欄位 | 填什麼 |
|---|---|
| 伺服器 | 留空就是 `https://api2.pushdeer.com`，或填你自己的 PushDeer 伺服器 |
| PushKey | `PDU…` |

### WxPusher

有兩種方式，都在 <https://wxpusher.zjiekou.com>。

**極簡推送：**

1. 用微信掃 QR code 關注。
2. 取得你的 SPT（`SPT_…`）。
3. 收件留空。

**應用程式：**

1. 建立應用程式，取得 **appToken**（`AT_…`）。
2. 用微信掃它的 QR code，每個掃過的人都會有一個使用者 ID（`UID_…`）。
3. 要發到主題的話，用主題的數字 ID。

| 欄位 | 填什麼 |
|---|---|
| appToken（AT_…）或 SPT | `SPT_…` 或 `AT_…` |
| UID 或主題 ID | 只有 appToken 才要填：`UID_…` 和主題數字，用逗號分隔 |

微信上的摘要只顯示標題前 20 個字。

### Chanify

1. 在 Chanify App 裡替一個傳送者建立 **token**。
2. 複製這個 token。

| 欄位 | 填什麼 |
|---|---|
| 伺服器 | 留空就是 `https://api.chanify.net`，或填你自己的 Chanify 節點 |
| Token | App 給的 token |

### Matrix

1. 建立一個機器人帳號。
2. 邀請它進聊天室，並讓它加入。
3. 在「Room settings → Advanced」找到聊天室 ID（`!abc…:example.org`）。填聊天室的別名（`#name:example.org`）也可以。

聊天室必須是未加密的：Bageo 讀不到加密的聊天室，設定裡按測試時，聊天室有加密就會告訴你。請另外建立一個沒有加密的新聊天室。

| 欄位 | 填什麼 |
|---|---|
| Homeserver 網址 | `https://matrix.example.org` |
| 存取權杖 | 機器人帳號的 access token |
| 聊天室 ID | `!…:example.org` 或 `#name:example.org` |

同一個管道也能下指令，見[在聊天室操作](CHAT_COMMANDS.md#matrix)。

### 電子郵件（Gmail）

透過 Gmail 的 SMTP 伺服器寄送，寄件人名稱是「Bageo」。要先準備**應用程式密碼**：

1. 在你的 Google 帳戶開啟兩步驟驗證。
2. 到「安全性 → 兩步驟驗證 → 應用程式密碼」建立一組。

| 欄位 | 填什麼 |
|---|---|
| Gmail 地址 | 寄件的帳號 |
| 應用程式密碼 | 16 個字元的應用程式密碼 |
| 寄到（空白＝寄給自己） | 收件人 |

### 電子郵件（SMTP）

任何支援 TLS 的郵件伺服器都行：iCloud、Yahoo、Fastmail、QQ 郵箱、163、公司的或你自己架的。伺服器和登入帳號寫在同一個欄位：

```text
smtp://you@example.com@smtp.example.com:587     STARTTLS（大多數郵件服務）
smtps://you@example.com@smtp.example.com:465    一連線就走 TLS
```

- 最後一個 `@` 前面是登入帳號，同時也是寄件地址。
- 帳號本身的 `@` 也可以寫成 `%40`。
- 465 埠一律是一連線就走 TLS。
- 沒寫埠就用 587（`smtps://` 則是 465）。
- 寄件人名稱是「Bageo」。

| 欄位 | 填什麼 |
|---|---|
| SMTP 網址（smtp://帳號@主機:埠） | 如上 |
| 密碼 | 帳號密碼，或郵件服務要求的應用程式密碼／授權碼（iCloud、Yahoo、Fastmail、QQ 郵箱、163 都要） |
| 寄到（空白＝寄給登入帳號） | 收件人 |

限制：

- 登入帳號必須是電子郵件地址。
- 伺服器要接受 `AUTH LOGIN`。
- 憑證必須來自公開的憑證機構（自己簽的憑證會被拒絕）。
- 不支援沒有 TLS 的連線。

寄 Gmail 的話，[Gmail](#電子郵件gmail) 管道要填的欄位比較少。

### Home Assistant

透過 Home Assistant 的 **notify** 動作送出，例如送到手機上的 Companion App。

1. 在 Home Assistant：個人資料 →「安全性」→「長期存取權杖」→「建立權杖」。
2. 在「開發者工具 → 動作」找服務：搜尋 `notify.`（Companion App 會新增 `notify.mobile_app_<手機名稱>`）。

| 欄位 | 填什麼 |
|---|---|
| 伺服器網址 | 留空就是 `http://homeassistant.local:8123`，或填你的網址 |
| 長期存取權杖 | 第 1 步的權杖 |
| 通知服務 | `mobile_app_pixel_9`，前面有沒有 `notify.` 都可以 |

訊息超過 1,000 字元會截短，因為手機推播只能帶大約 4 KB。

### IFTTT Webhooks

1. 到 <https://ifttt.com/maker_webhooks> 連結 Webhooks；「Documentation」會顯示你的**金鑰**。
2. 建立 Applet：*If* Webhooks →「Receive a web request」，事件名稱例如 `bageo_call`，*Then* 選你想做的事。

在 Applet 裡，`{{Value1}}` 是標題、`{{Value2}}` 是內容、`{{Value3}}` 是事件（`summary`、`incoming`…）。

| 欄位 | 填什麼 |
|---|---|
| Webhooks 金鑰 | 第 1 步的金鑰 |
| 事件名稱 | `bageo_call` |

使用 Webhooks 的 Applet 可能需要 IFTTT 付費方案。

### Apprise API

如果你已經在跑 [Apprise API](https://github.com/caronc/apprise-api)，它還能連到上百種服務：

| 欄位 | 填什麼 |
|---|---|
| 通知網址（…/notify/KEY） | `http://apprise.local:8000/notify/<your config key>` |
| 標籤（選填） | 要送到的標籤 |

### Webhook (JSON)

給 n8n、Zapier、Make、Home Assistant、Node-RED 或你自己的程式用。Bageo 會用 `POST` 把 JSON 送到你的網址。

有填「Bearer 權杖（選填）」時，會加上 `Authorization: Bearer <token>`。下面是語言設為 English 時的內容：

```json
{
  "event": "summary",
  "title": "Call summary 0912345678",
  "body": "Call summary\nNumber: 0912345678\n…",
  "text": "(same as body)",
  "data": {
    "caller_id": "0912345678",
    "device": "Pixel 8",
    "time": "Nov 12, 18:02",
    "summary": "Caller: Jay (friend)\nAbout: …",
    "contact": ""
  }
}
```

| `event` | 什麼時候 | `data` |
|---|---|---|
| `incoming` | 電話被接起來 | `caller_id`、`device`、`time` |
| `outgoing` | Bageo 自己撥出的電話開始響鈴 | `caller_id`（撥出的號碼）、`device`、`time` |
| `blocked` | 黑名單上的號碼打來 | `caller_id`、`device`、`time`、`reason`、`declined`（`true`：響鈴時拒接；`false`：接起來播語音） |
| `summary` | 通話結束幾秒後 | `caller_id`、`device`、`time`、`summary`、`contact`（聯絡人名字，不是聯絡人就是空字串） |
| `call_waiting` | 通話中第二通打進來被拒接 | `caller_id`、`device`、`time`、`sim_country` |
| `repeat_calls` | 同一個號碼一直打來，多出來的通知被合併 | `caller_id`、`count`、`kind`（`incoming`、`blocked` 或 `call_waiting`） |
| `ai_callback` | 行程預約的 AI 回電結束，或撥不出去 | `caller_id`、`device`、`time`、`summary` |
| `ai_errand` | AI 代打結束，或沒能開始 | `number`、`device`、`time`、`summary` |
| `appointment_pending` | 對方跟 AI 約了時間，等待你決定 | `task_id`、`caller_id`、`device`、`title`、`start_at`、`accept_url`、`decline_url` |
| `whitelist_direct_transfer` | 手機不接、由電信把來電轉到你的手機 | `caller_id`、`device`、`time`、`forward_to`、`contact` |
| `whitelist_urgent_transfer` | 來電者說有急事，已馬上通知你 | `caller_id`、`device`、`time`、`forward_to`、`reason`、`contact` |
| `voicemail` | 存下一則語音留言（舊的選單模式） | `caller_id`、`device`、`time`、`seconds` |
| `event` | 透過 API 傳的訊息（`/calls/api/notify/send`） | `null` |
| `test` | 「傳送測試」 | `{}` |

收到 `2xx` 回應就算送達。通話的其他內容（逐字稿、錄音），請用通話 ID 從 [API](API.md) 取得。

## 傳送你自己的訊息

你網路上的腳本也能用同一組管道，例如車庫門感應器：

```text
POST /calls/api/notify/send        {"title": "Garage", "body": "Door left open"}
```

- 跟其他 API 一樣，要帶控制台密碼（見 [API](API.md)）。
- `body` 必填；`title` 沒填就是「Bageo」。
- 至少一個管道收下時回 `{"ok": true}`。
- 「傳送通知」關著時，這類訊息照樣會送。

選用的[簡訊轉發工具](../../tools/sms_forward/README.md)（英文）就是用這個方式轉發手機收到的簡訊。

## 改用環境變數設定

給容器和腳本部署用：有三條通知路徑可以在啟動時指定，不必進網頁控制台。它們會跟控制台的管道一起使用：

| 環境變數 | 路徑 |
|---|---|
| `TELEGRAM_BOT_TOKEN` + `TELEGRAM_CHAT_ID` | Telegram Bot；預約卡片也是由它送出。控制台裡有同一個 Bot 和聊天時，不會重複送。 |
| `NOTIFY_WEBHOOK_URL` | 上面的 [JSON Webhook](#webhook-json)，不帶權杖 |
| `NOTIFY_GATEWAY_URL` + `NOTIFY_GATEWAY_TAG` | Apprise API 的基底網址；訊息送到 `<URL>/notify/<tag>`，標籤留空時是 `normal` |

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
