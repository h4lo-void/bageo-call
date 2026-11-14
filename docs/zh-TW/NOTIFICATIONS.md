# 通知：通話摘要傳到 Telegram、LINE、Discord、Email 等 16 種管道

AI 電話助理幫你接完電話，掛斷幾秒後，手機就收到一段摘要：誰打來、要做什麼、AI 怎麼回、你要處理什麼。Bageo 也能在來電時、拒接號碼或插播被擋下時、AI 回電或 AI 代打結束時通知你。支援 16 種服務，從 Telegram、LINE 到企業微信、ntfy、Email，以及你自己的 Webhook。

**一分鐘設定好：**「設定 → 通知」→「**新增管道**」→ 選服務 → 填欄位 →「**傳送測試**」→「**儲存變更**」。

- 管道想加幾個都可以，**每則訊息都會送到所有啟用的管道**。每個管道可以取「名稱」（例如「家人群組」），用「啟用」暫時關掉而不必刪除。
- 網址和權杖存在你電腦上的 `data/calls/config/settings.json`。網頁控制台之後不會再完整顯示，只給提示；編輯時欄位留空，就保留原本的值。
- 最上面的「傳送通知」關掉後，通話相關訊息和 Telegram 預約卡片都會停，但管道不會被刪除。腳本透過 API 傳的訊息（見[下方](#傳送你自己的訊息)），例如轉發的簡訊，仍會照送。
- 訊息用「通知和摘要的語言」撰寫（預設「跟 AI 講話的語言相同」），跟通話講什麼語言無關。
- 太長的訊息會依各服務的上限截短（例如 Pushover 1,000 字元，Discord 和企業微信 1,900 字元）。

> **測試程度：** Telegram 是用得最多的管道。其他服務依照各自的公開文件實作，請求內容有單元測試涵蓋，但在真實帳號上用得還不多。哪一個失敗了，「傳送測試」會顯示該服務的回應，歡迎回報。

## 訊息長什麼樣子

一則來自聯絡人的通話摘要：

```text
☎️ 通話摘要 · 小傑
號碼：0912345678
手機：Pixel 8
時間：11月12日 18:02

對方：小傑（朋友）
事情：明天中午要不要一起吃飯？
AI 回覆：說聽起來不錯，沒有敲定時間
要處理：回電給小傑確認
📅 行事曆：和小傑吃午餐 (11/13 12:00)
https://calendar.google.com/calendar/render?action=TEMPLATE&…
逐字稿／錄音：http://192.0.2.10:8787/calls/#call=…
```

- **標題**：打來的人在「聯絡人」裡時，後面會加名字。
- **行事曆那幾行**：通話中講定了日期、而且「建議行事曆活動」開著時才會出現（[設定參考](SETTINGS.md#摘要與行事曆)）。連結會打開你第一次按「加入行事曆」時選的那一家（`calendar_app`；還沒選時是 Google 日曆），詳見下方[行程預約](#行程預約)。
- **最後一行**：主程式有其他裝置開得了的網址時才會出現：設定裡的「這個控制台的網址」，或控制台對區網開放時自動帶入的網址（Docker 裡不會自動帶入）。見[從其他裝置使用控制台](SETTINGS.md#從其他裝置使用控制台)。

## 你會收到哪些訊息

每則訊息開頭都有圖示，一眼就分得出來：

| 圖示與標題 | 什麼時候 | Webhook `event` |
|---|---|---|
| 📞 來電 | 電話被接起來。 | `incoming` |
| ⛔ 拒接號碼來電 | 拒接名單上的號碼打來。會寫是響鈴時直接拒接，還是接起來播語音，以及你當初填的原因。 | `blocked` |
| ☎️ 通話摘要 | 通話結束幾秒後：誰、什麼事、AI 怎麼回、你要處理什麼，以及連結。 | `summary` |
| 📵 通話中插播未接 | 通話中又有第二通打進來，手機把它拒接了（對方聽到忙線）。知道號碼的話記得回電。 | `call_waiting` |
| 📞 AI 回電：〈號碼〉 | AI 打給來電者告知預約結果：結果如何（接通並告知、沒人接、撥不出去，或這個號碼不能回電）和那通電話的摘要。 | `ai_callback` |
| 📞 AI 代打：〈名字或號碼〉 | 你交辦的 AI 代打結束了：結果、細節、需要你決定的選項、下一步、談定的時間，以及「代打」頁的連結。 | `ai_errand` |
| 📅 預約待你確認 | 對方跟 AI 約了時間；用 `TELEGRAM_BOT_TOKEN` 指定 Telegram Bot 時，另外會收到有按鈕的卡片（見[行程預約](#行程預約)）。 | 包在 `summary` 裡 |
| ✅ 測試通知 | 你按了「傳送測試」。 | `test` |
| 新簡訊 | 選用的[簡訊轉發工具](../../tools/sms_forward/README.md)（英文）轉來的簡訊。 | `event` |

插播未接的訊息：

```text
📵 通話中插播未接：0912345678（Pixel 9）
時間：11月14日 18:02
```

AI 代打的結果，例如 AI 幫你訂好的餐廳：

```text
📞 AI 代打：鼎泰豐
號碼：0223456789
手機：Pixel 9
時間：11月14日 18:05

辦好了：已訂明天 7 點半 4 位，名字王先生
細節：座位保留 15 分鐘。
📅 約定時間：11月15日 19:30 鼎泰豐晚餐
http://bageo.local:8787/calls/#errands
```

AI 在你來不及回答的問題上先答應了的話，結果會是「要你決定」，並多一行請你確認，不行要打回去改。

## 行程預約

開了「行程預約」（[設定參考](SETTINGS.md#摘要與行事曆)）後，對方跟 AI 約好時間的那通電話，摘要下面會多一行（取代行事曆那幾行），每個管道都收得到：

```text
📅 預約待你確認：11/14（週六）15:00–16:00 看房 — 到網頁確認：http://bageo.local/calls/#call=…
```

**在網頁上決定：**「通話」頁最上方的「**待處理事項**」，或那通電話的詳情，選怎麼告訴對方（**傳簡訊**、**AI 回電**或**不通知對方**），再按 **同意** 或 **不同意**。連結會打開那通電話；控制台沒有其他裝置開得了的網址（「這個控制台的網址」）時，這行改寫「到網頁控制台確認」。完全沒設通知管道時不會推播，預約照樣在網頁上等你。主程式不會自己把任何東西寫進行事曆。

**在 Telegram 上一鍵決定。** 用 `TELEGRAM_BOT_TOKEN` 和 `TELEGRAM_CHAT_ID` 指定了 Telegram Bot（見[下方](#改用環境變數設定)）、「傳送通知」也開著時，等你決定的預約另外會有一張卡片。只在網頁控制台新增的 Telegram 管道，收得到上面那行提醒，但收不到卡片。

```text
📅 預約待確認

號碼：0912345678
時間：11/14（週六）15:00–16:00
事由：看房
稱呼：陳先生
手機：Pixel 8
簡訊：按同意或不同意後會傳簡訊告訴對方
網頁確認：http://bageo.local/calls/#call=…
[同意] [不同意]
```

「簡訊」這一行照這個號碼的「預設回覆方式」寫，也可能是「回覆方式：按同意或不同意後，AI 會打電話告訴對方」（市話，或沒開簡訊）、「不會傳（設定裡「傳簡訊告訴對方」沒開）」、「對方號碼收不到簡訊，請自己回電」，或「未顯示號碼，沒辦法通知對方」。在卡片上按，用的是「預設回覆方式」；想換方式，請到網頁上決定。

按了之後，Bot 會回覆結果：

```text
✅ 已同意預約
11/14（週六）15:00–16:00 看房
已傳簡訊給 0912345678
📅 加入行事曆：https://calendar.google.com/calendar/render?action=TEMPLATE&…
```

其他可能的回覆：「❌ 已婉拒預約」、「AI 正在回電給 0912345678」、「簡訊沒傳出：手機沒連線」／「手機 App 太舊，不支援傳簡訊」／「失敗代碼 no_service」。簡訊沒傳出時，網頁上那筆預約會出現「**重傳簡訊**」，可以用原本那支或另一支在線的手機再傳。

**「加入行事曆」哪一家都能用。** Bot 回覆、通知和網頁裡的連結，會打開你第一次按「加入行事曆」時選的行事曆（設定 `calendar_app`）：Google 日曆、Outlook.com、Microsoft 365 或 Yahoo 日曆已經填好的「新增活動」表單；選 Apple 日曆或 `.ics` 時，則是這個活動在「這個控制台的網址」底下的 `.ics` 檔。這個 `.ics` 連結帶的是只能開這一個檔案的金鑰（HMAC），不是你的控制台權杖，從聊天室點開也安全；沒填控制台網址就沒有 `.ics` 連結。主程式不連任何行事曆帳號。

**只有第一次決定算數**，不管在哪裡決定（網頁、卡片或 [API](API.md#行程預約與任務)）；再決定一次（就算是同時按）會回「這個預約已經處理過了」。在網頁或 API 決定後，卡片上的按鈕會拿掉，Bot 也會在聊天裡回同樣的結果；你決定之後才送到的卡片沒有按鈕。Telegram 連不上也不影響決定。

告訴對方結果的 AI 回電，講的是那通電話的語言：等對方接起來出聲，說是誰打來、說一次你的回覆，然後道別。它的通知是「📞 AI 回電：〈號碼〉」；撥不出去也會通知你。

推銷、詐騙電話不會留下預約。通話後才被判定是這類電話（摘要標成「推銷電話」「疑似詐騙」；銀行防詐專線、警察通知詐騙這類電話不算）時，預約會作廢、不發卡片，摘要會寫：

```text
📅 判定是推銷電話，已取消通話中排入的預約：11/14（週六）15:00–16:00 看房
```

## 各項服務

挑一個你每天都會看的 App：

| 服務 | 需要準備 | 常見用途 |
|---|---|---|
| [Telegram](#telegram) | Bot 權杖、聊天 ID | 通用；預約卡片只有它有 |
| [Discord](#discord) | Webhook 網址 | Discord 伺服器 |
| [Slack / Mattermost](#slack--mattermost) | Incoming Webhook 網址 | 工作群組、Rocket.Chat |
| [LINE](#line) | Channel access token、使用者或群組 ID | 台灣、日本、泰國 |
| [ntfy](#ntfy) | 主題 | 自架、Android 推播 |
| [Gotify](#gotify) | 伺服器網址、App 權杖 | 自架 |
| [Bark (iOS)](#bark-ios) | 裝置金鑰 | iPhone 推播 |
| [Pushover](#pushover) | API 權杖、使用者金鑰 | iOS 與 Android 推播 |
| [企業微信群機器人（WeCom）](#企業微信群機器人wecom) | Webhook 網址 | 企業微信 |
| [釘釘機器人（DingTalk）](#釘釘機器人dingtalk) | Webhook 網址、加簽密鑰 | 釘釘 |
| [飛書／Lark 機器人](#飛書lark-機器人) | Webhook 網址 | 飛書、Lark |
| [Server醬（ServerChan）](#server醬serverchan) | SendKey | 推到微信 |
| [Matrix](#matrix) | Homeserver 網址、存取權杖、聊天室 ID | Element 等 Matrix App |
| [電子郵件（Gmail）](#電子郵件gmail) | Gmail 地址、應用程式密碼 | Email |
| [Apprise API](#apprise-api) | 通知網址 | 再連上百種服務 |
| [Webhook (JSON)](#webhook-json) | 網址 | n8n、Zapier、Make、Home Assistant、你自己的程式 |

### Telegram

1. 在 Telegram 跟 **@BotFather** 對話：`/newbot`，取個名字 → 拿到一組像 `123456789:AA…` 的 **bot token**。
2. 打開跟新機器人的聊天視窗，按 **Start**（機器人不能主動先傳訊息給你）。
3. 找出你的 **chat ID**：跟 **@userinfobot** 對話，它會回你的數字 ID。要傳到群組，就把機器人加進群組，改用群組 ID（負數，例如 `-1001234567890`）。

| 欄位 | 填什麼 |
|---|---|
| Bot 權杖 | @BotFather 給的 token |
| 聊天 ID（Chat ID） | 你的數字 ID，或群組 ID |

想收到有按鈕的預約卡片，請把同一個 Bot 也用 `TELEGRAM_BOT_TOKEN` 和 `TELEGRAM_CHAT_ID` 指定；同一則訊息不會收到兩次。

### Discord

頻道設定 →「整合 → Webhook → 新 Webhook → 複製 Webhook 網址」。

| 欄位 | 填什麼 |
|---|---|
| Webhook 網址 | `https://discord.com/api/webhooks/…` |

### Slack / Mattermost

Slack：到 <https://api.slack.com/apps> 建立 App →「Incoming Webhooks」→ 啟用 →「Add New Webhook to Workspace」→ 複製網址。Mattermost：「Integrations → Incoming Webhooks」。Rocket.Chat 的 incoming webhook 也接受同樣格式。

| 欄位 | 填什麼 |
|---|---|
| Incoming Webhook 網址 | `https://hooks.slack.com/services/…` |

### LINE

LINE Notify 已在 2025 年停止服務；這裡用的是 **Messaging API**。

1. 到 <https://developers.line.biz/console/> 建立 provider 和「Messaging API」channel（會同時建立一個 LINE 官方帳號）。
2. 在該 channel 的「Messaging API」分頁發行 **channel access token（long-lived）**。
3. 用 LINE App 把這個官方帳號加為好友（同一分頁有 QR code）。
4. 你的**使用者 ID**（`U…`，33 個字元）在「Basic settings」分頁的「Your user ID」。要傳到群組，就把官方帳號邀進群組，改用群組 ID（`C…`），群組 ID 要從 webhook 事件取得。

| 欄位 | 填什麼 |
|---|---|
| Channel access token | 那組 long-lived token |
| 使用者或群組 ID | `U…` 或 `C…` |

官方帳號的免費方案，每月可發的推播則數有限。

### ntfy

安裝 ntfy App，訂閱一個**不容易被猜到**的主題名稱：在公用伺服器上，知道名稱的人都讀得到訊息。

| 欄位 | 填什麼 |
|---|---|
| 伺服器 | 留空就是 `https://ntfy.sh`，或填你自己的伺服器 |
| 主題（Topic） | 主題名稱 |
| 存取權杖（選填） | 只有自架伺服器上受保護的主題才需要（`tk_…`） |

### Gotify

在 Gotify：「Apps → Create application」→ 複製它的 token。

| 欄位 | 填什麼 |
|---|---|
| 伺服器網址 | 你的 Gotify 伺服器，例如 `https://gotify.example.org` |
| App 權杖 | 該應用程式的 token |

### Bark (iOS)

打開 iPhone 上的 Bark App，範例網址長這樣：`https://api.day.app/<device key>/…`。

| 欄位 | 填什麼 |
|---|---|
| 伺服器 | 留空就是 `https://api.day.app`，或填你自己的 bark-server |
| 裝置金鑰 | App 裡的那組 key |

### Pushover

到 <https://pushover.net/apps/build> 建立一個應用程式。

| 欄位 | 填什麼 |
|---|---|
| API 權杖 | 該應用程式的 token |
| 使用者金鑰（User key） | Pushover 首頁上你的 user key |

### 企業微信群機器人（WeCom）

在群組裡「新增群機器人」→ 複製 webhook 網址。

| 欄位 | 填什麼 |
|---|---|
| Webhook 網址 | `https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=…` |

### 釘釘機器人（DingTalk）

群設定 →「機器人」→「自訂」→ 安全設定選「**加簽**」，把 webhook 和密鑰（`SEC…`）都複製下來。如果改用關鍵字當安全設定，每則訊息都必須含有其中一個關鍵字；加簽比較簡單。

| 欄位 | 填什麼 |
|---|---|
| Webhook 網址 | `https://oapi.dingtalk.com/robot/send?access_token=…` |
| 加簽密鑰（選填） | `SEC…` 那組值；機器人沒設就留空 |

### 飛書／Lark 機器人

群組 →「設定 → 群機器人 → 新增機器人 → 自訂機器人」→ 複製 webhook 網址（`open.feishu.cn` 或 `open.larksuite.com`）。不要開簽章驗證，不支援；自訂關鍵字或 IP 白名單都可以。

| 欄位 | 填什麼 |
|---|---|
| Webhook 網址 | `https://open.feishu.cn/open-apis/bot/v2/hook/…` |

### Server醬（ServerChan）

從 Server醬網站複製你的 SendKey。

| 欄位 | 填什麼 |
|---|---|
| SendKey | Turbo 版是 `SCT…`，Server醬³ 是 `sctp…` |

### Matrix

建立一個機器人帳號，邀請它進聊天室並讓它加入。聊天室 ID 在「Room settings → Advanced」（`!abc…:example.org`）。聊天室必須是未加密的。

| 欄位 | 填什麼 |
|---|---|
| Homeserver 網址 | `https://matrix.example.org` |
| 存取權杖 | 機器人帳號的 access token |
| 聊天室 ID | `!…:example.org` |

### 電子郵件（Gmail）

透過 Gmail 的 SMTP 伺服器，用**應用程式密碼**寄送（Google 帳戶 →「安全性 → 兩步驟驗證 → 應用程式密碼」；要先開兩步驟驗證）。寄件人名稱是「Bageo」。

| 欄位 | 填什麼 |
|---|---|
| Gmail 地址 | 寄件的帳號 |
| 應用程式密碼 | 16 個字元的應用程式密碼 |
| 寄到（空白＝寄給自己） | 收件人 |

### Apprise API

如果你已經在跑 [Apprise API](https://github.com/caronc/apprise-api)，它還能連到上百種服務：

| 欄位 | 填什麼 |
|---|---|
| 通知網址（…/notify/KEY） | `http://apprise.local:8000/notify/<your config key>` |
| 標籤（選填） | 要送到的標籤 |

### Webhook (JSON)

給 n8n、Zapier、Make、Home Assistant、Node-RED 或你自己的程式用。主程式會用 `POST` 把 JSON 送到你的網址；有填「Bearer 權杖（選填）」時，會加上 `Authorization: Bearer <token>`。下面是語言設為 English 時的內容：

```json
{
  "event": "summary",
  "title": "Call summary 0912345678",
  "body": "☎️ Call summary\nNumber: 0912345678\n…",
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
| `blocked` | 拒接號碼打來 | `caller_id`、`device`、`time`、`reason`、`declined`（`true`：響鈴時拒接；`false`：接起來播語音） |
| `summary` | 通話結束幾秒後 | `caller_id`、`device`、`time`、`summary`、`contact`（聯絡人名字，不是聯絡人就是空字串） |
| `call_waiting` | 通話中第二通打進來被拒接 | `caller_id`、`device`、`time`、`sim_country` |
| `ai_callback` | 行程預約的 AI 回電結束，或撥不出去 | `caller_id`、`device`、`time`、`summary` |
| `ai_errand` | AI 代打結束，或沒能開始 | `number`、`device`、`time`、`summary` |
| `voicemail` | 存下一則語音留言（舊的選單模式） | `caller_id`、`device`、`time`、`seconds` |
| `event` | 透過 API 傳的訊息（`/calls/api/notify/send`） | `null` |
| `test` | 「傳送測試」 | `{}` |

收到 `2xx` 回應就算送達。通話的其他內容（逐字稿、錄音），請用通話 ID 從 [API](API.md) 取得。

## 傳送你自己的訊息

你網路上的腳本也能用同一組管道，例如車庫門感應器：

```text
POST /calls/api/notify/send        {"title": "Garage", "body": "Door left open"}
```

跟其他 API 一樣要帶控制台權杖（見 [API](API.md)）。`body` 必填；`title` 沒填就是「Bageo」。至少一個管道收下時回 `{"ok": true}`。「傳送通知」關著時，這類訊息照樣會送。選用的[簡訊轉發工具](../../tools/sms_forward/README.md)（英文）就是用這個方式轉發手機收到的簡訊。

## 改用環境變數設定

給容器和腳本部署用：有三條通知路徑可以在啟動時指定，不必進網頁控制台。它們會跟控制台的管道一起使用：

| 環境變數 | 路徑 |
|---|---|
| `TELEGRAM_BOT_TOKEN` + `TELEGRAM_CHAT_ID` | Telegram Bot；預約卡片也是由它送出。控制台裡有同一個 Bot 和聊天時，不會重複送。 |
| `NOTIFY_WEBHOOK_URL` | 上面的 [JSON Webhook](#webhook-json)，不帶權杖 |
| `NOTIFY_GATEWAY_URL` + `NOTIFY_GATEWAY_TAG` | Apprise API 的基底網址；訊息送到 `<URL>/notify/<tag>`，標籤留空時是 `normal` |
