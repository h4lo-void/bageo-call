# 通知

有來電時、拒接號碼打來時，主程式會傳一則訊息；通話結束幾秒後，再傳摘要。設定位置在「設定 → 通知」：「**新增管道**」、選擇服務、填好它的欄位、「**傳送測試**」、「**儲存變更**」。

- 管道想加幾個就加幾個；**每則訊息都會傳到所有啟用的管道**。
- 位址和 token 存在你電腦上的 `data/calls/config/settings.json`。網頁控制台之後不會再完整顯示它們，只會給提示；編輯時把欄位留空，就會保留原本儲存的值。
- 最上面的「傳送通知」關掉之後，所有通知都會停止，但管道不會被刪除。
- 訊息會用「設定 → 語言」選的語言撰寫。

> **現況：** Telegram 每天都在使用。其他服務是依照它們的公開文件實作的，請求內容有單元測試涵蓋，但還沒有用真實帳號試過。如果哪一個失敗了，「傳送測試」按鈕會顯示該服務的回應——請回報給我們。

## 訊息長什麼樣子

```text
☎️ Call summary
Number: 0912345678
Phone: Pixel 8
Time: 11/12 18:02

Caller: Jay (friend)
About: lunch tomorrow at noon?
AI said: sounded keen, did not fix the time
To do: call Jay back to confirm
📅 Calendar: Lunch with Jay (11/13 12:00)
https://calendar.google.com/calendar/render?action=TEMPLATE&…
Transcript / recording: http://192.168.1.20:8787/calls/#call=…
```

上面是語言設為 English 的安裝環境送出的內容；實際的用字會跟著控制台裡設定的語言走。

最後一行在主程式有控制台網址時才會出現：設定裡的「這個控制台的網址」，或控制台對區網開放時自動帶入的網址（Docker 裡不會自動帶入）。行事曆那幾行只有在通話中講定了日期時才會出現（[設定參考](SETTINGS.md#摘要與行事曆)）。

## 行程預約

開了「行程預約」（[設定參考](SETTINGS.md#摘要與行事曆)）後，對方跟 AI 約好時間的那通電話，摘要下面會多一行（取代行事曆那幾行），你設定的每個通知管道（Telegram、Apprise、Webhook…）都收得到：

```text
📅 預約待你確認：11/14（週六）15:00–16:00 看房 — 到網頁確認：http://bageo.local/calls/#call=…
📅 已排入行事曆：11/14（週六）15:00–16:00 看房
```

第一種是「先問我」（或「有空直接答應」但日曆寫不進去時）：在網頁上決定——「通話」頁最上方的「待確認的預約」，或該通電話的詳情，按 **✅ 確定**／**❌ 婉拒**。連結會打開那通電話；控制台沒有別的裝置開得了的網址時（設定裡的「這個控制台的網址」），這行改寫「到網頁控制台確認」。第二種是「有空直接答應」。完全沒設定通知管道時不會推播，預約照樣在網頁上等你。

有設定 Telegram Bot（bot token 和 chat ID）、「傳送通知」也開著時，等你決定的預約另外會有一張可以直接按的卡片：

```text
📅 預約待確認

號碼：0912345678
時間：11/14（週六）15:00–16:00
事由：看房
稱呼：陳先生
手機：Pixel 8
簡訊：按確定或婉拒後會傳簡訊告訴對方
網頁確認：http://bageo.local/calls/#call=…
[✅ 確定] [❌ 婉拒]
```

按 **✅ 確定** 會寫進 Google 日曆；按 **❌ 婉拒** 日曆不動（通話中已經排入的那筆會刪掉）。開了「傳簡訊告訴對方」時，接著由接這通電話的手機以你的名義傳簡訊給對方——只寫時間，不寫約的是什麼事。不管在哪裡決定（網頁、卡片或 [API](API.md#行程預約)），只有第一次算數；再按一次（就算是同時按）會回「這個預約已經處理過了」。結果會顯示在網頁上，在卡片上按的由 Bot 回覆：「已加入 Google 日曆」（或為什麼沒有）、「已傳簡訊給 0912345678」，或「簡訊沒傳出：手機沒連線」／「手機 App 太舊，不支援傳簡訊」／「失敗代碼 no_service」。市話、未顯示號碼、國外號碼，卡片會寫「對方號碼收不到簡訊，請自己回電」。「有空直接答應」的簡訊在通話結束後馬上傳，結果一樣由 Bot 回報。

在網頁或 API 決定後，卡片上的按鈕會拿掉，Bot 也會在聊天裡回同樣的結果，卡片不會再讓你決定已經決定過的事。Telegram 連不上也不影響決定的結果。

已經把活動寫進日曆的電話，後來被判定是推銷或詐騙時（通話中判定，或通話後摘要標成「推銷電話」「疑似詐騙」；銀行防詐騙、警察通知這類電話不算），那筆活動會在通話結束後刪掉，也不會發卡片。由摘要判定時，摘要會寫：

```text
📅 判定是推銷電話，已取消通話中排入的預約：11/14（週六）15:00–16:00 看房
```

Google 不讓刪時，改寫：

```text
📅 通話中排入的「11/14（週六）15:00–16:00 看房」已作廢，但沒能從行事曆刪掉，請自己刪
```

## 各項服務

每項服務：要建立什麼，以及填到哪個欄位。

### Telegram

1. 在 Telegram 裡跟 **@BotFather** 對話：`/newbot`，取個名字 → 你會拿到一組 **bot token**，像 `123456789:AA…`。
2. 打開跟新機器人的聊天視窗，按 **Start**（機器人不能主動先傳訊息給你）。
3. 找出你的 **chat ID**：跟 **@userinfobot** 對話，它會回覆你的數字 ID。要傳到群組的話，把你的機器人加進群組，改用群組的 ID（是負數，例如 `-1001234567890`）。

| 欄位 | 填什麼 |
|---|---|
| Bot token | @BotFather 給的 token |
| Chat ID | 你的數字 ID，或群組 ID |

### Discord

頻道 →「編輯頻道 → 整合 → Webhook → 新 Webhook → 複製 Webhook 網址」。

| 欄位 | 填什麼 |
|---|---|
| Webhook URL | `https://discord.com/api/webhooks/…` |

### Slack / Mattermost

Slack：到 <https://api.slack.com/apps> 建立一個 App →「Incoming Webhooks」→ 啟用 →「Add New Webhook to Workspace」→ 複製網址。Mattermost 和 Rocket.Chat 的 incoming webhook 接受同樣的格式。

| 欄位 | 填什麼 |
|---|---|
| Incoming webhook URL | `https://hooks.slack.com/services/…` |

### LINE

LINE Notify 已在 2025 年停止服務；這裡用的是 **Messaging API**。

1. 到 <https://developers.line.biz/console/> 建立一個 provider 和一個「Messaging API」channel（這會建立一個 LINE 官方帳號）。
2. 在該 channel 的「Messaging API」分頁發行 **channel access token（long-lived）**。
3. 用你的 LINE App 把這個官方帳號加為好友（同一個分頁上有 QR code）。
4. 你的**使用者 ID**（`U…`，33 個字元）在該 channel 的「Basic settings」分頁，「Your user ID」底下。如果要改傳到群組，把這個帳號邀進群組，並使用群組 ID（`C…`）——群組 ID 要從 webhook 事件取得。

| 欄位 | 填什麼 |
|---|---|
| Channel access token | 那組 long-lived token |
| User / group ID | `U…` 或 `C…` |

官方帳號的免費方案，每個月能發送的推播訊息則數有限。

### ntfy

安裝 ntfy App，訂閱一個**名稱不容易被猜到**的主題（在公用伺服器上，任何知道名稱的人都能讀到訊息）。

| 欄位 | 填什麼 |
|---|---|
| Server | 留空就是 `https://ntfy.sh`，或填你自己伺服器的位址 |
| Topic | 主題名稱 |
| Access token | 只有自己伺服器上受保護的主題才需要（`tk_…`） |

### Gotify

在 Gotify：「Apps → Create application」→ 複製它的 token。

| 欄位 | 填什麼 |
|---|---|
| Server URL | `https://gotify.example.org` |
| App token | 該應用程式的 token |

### Bark (iOS)

打開 Bark App；它顯示的網址是 `https://api.day.app/<device key>/…`。

| 欄位 | 填什麼 |
|---|---|
| Server | 留空就是 `https://api.day.app`，或填你自己的 bark-server |
| Device key | App 裡的那組 key |

### Pushover

到 <https://pushover.net/apps/build> 建立一個應用程式。

| 欄位 | 填什麼 |
|---|---|
| API token | 該應用程式的 token |
| User key | Pushover 首頁上你的 user key |

### WeCom 群機器人（企業微信群機器人）

群組 →「新增群機器人」→ 複製 webhook 位址。

| 欄位 | 填什麼 |
|---|---|
| Webhook URL | `https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=…` |

### DingTalk 機器人（釘釘群機器人）

群設定 →「機器人」→「自訂」→ 安全設定選「**加簽**」，把 webhook 和密鑰（`SEC…`）都複製下來。如果機器人改用關鍵字當安全設定，每則訊息裡都必須含有其中一個關鍵字——加簽比較簡單。

| 欄位 | 填什麼 |
|---|---|
| Webhook URL | `https://oapi.dingtalk.com/robot/send?access_token=…` |
| Signing secret | `SEC…` 那組值（機器人沒有設就留空） |

### Feishu / Lark 機器人（飛書群機器人）

群組 →「設定 → 群機器人 → 新增機器人 → 自訂機器人」→ 複製 webhook 位址（`open.feishu.cn` 或 `open.larksuite.com`）。不支援開啟了簽章驗證的機器人；請改用自訂關鍵字或 IP 白名單，或都不設。

| 欄位 | 填什麼 |
|---|---|
| Webhook URL | `https://open.feishu.cn/open-apis/bot/v2/hook/…` |

### ServerChan（Server醬）

| 欄位 | 填什麼 |
|---|---|
| SendKey | 你的 SendKey（Turbo 版是 `SCT…`，ServerChan³ 是 `sctp…`） |

### Matrix

建立一個機器人帳號，邀請它進聊天室，並用它加入該聊天室。聊天室 ID 在「Room settings → Advanced」底下（`!abc…:example.org`）。聊天室必須是未加密的。

| 欄位 | 填什麼 |
|---|---|
| Homeserver URL | `https://matrix.example.org` |
| Access token | 機器人帳號的 access token |
| Room ID | `!…:example.org` |

### 電子郵件（Gmail）

透過 Gmail 的 SMTP 伺服器，用**應用程式密碼**寄送（Google 帳戶 →「安全性 → 兩步驟驗證 → 應用程式密碼」；需要先開啟兩步驟驗證）。

| 欄位 | 填什麼 |
|---|---|
| Gmail address | 寄件的帳號 |
| App password | 16 個字元的應用程式密碼 |
| Send to | 收件人；留空 = 同一個信箱 |

### Apprise API

如果你已經在跑 [Apprise API](https://github.com/caronc/apprise-api)（它還能連到上百種其他服務）：

| 欄位 | 填什麼 |
|---|---|
| Notify URL | `http://apprise.local:8000/notify/<your config key>` |
| Tag | 選填的標籤 |

### Webhook (JSON)

給 n8n、Zapier、Make、Home Assistant、Node-RED 或你自己的程式用。主程式會把 JSON 用 `POST` 送到你的網址；有設 token 的話，會加上 `Authorization: Bearer <token>`。

```json
{
  "event": "summary",
  "title": "Call summary 0912345678",
  "body": "☎️ Call summary\nNumber: 0912345678\n…",
  "text": "(same as body)",
  "data": {
    "caller_id": "0912345678",
    "device": "Pixel 8",
    "time": "11/12 18:02",
    "summary": "Caller: Jay (friend)\nAbout: …"
  }
}
```

| `event` | 什麼時候 | `data` |
|---|---|---|
| `incoming` | 電話被接起來 | `caller_id`、`device`、`time` |
| `blocked` | 拒接號碼打來 | `caller_id`、`device`、`time`、`reason` |
| `summary` | 通話結束幾秒後 | `caller_id`、`device`、`time`、`summary` |
| `voicemail` | 存下一則語音留言（舊的選單模式） | `caller_id`、`device`、`time`、`seconds` |
| `event` | 透過 API 傳送的訊息（`/calls/api/notify/send`） | — |
| `test` | 「傳送測試」 | — |

收到 `2xx` 回應就算送達。通話的其他所有內容（逐字稿、錄音），請用通話 ID 從 [API](API.md) 取得。

## 傳送你自己的訊息

你網路上的腳本也可以用同樣這些管道：

```text
POST /calls/api/notify/send        {"title": "Garage", "body": "Door left open"}
```

見 [API](API.md)。選用的[簡訊轉發工具](../../tools/sms_forward/README.md)（英文）就是用這個方式轉發手機收到的簡訊。

## 改用環境變數設定

給容器和用腳本部署的環境：有三條通知路徑可以在啟動時指定，不必到網頁控制台設定：`TELEGRAM_BOT_TOKEN` + `TELEGRAM_CHAT_ID`、`NOTIFY_WEBHOOK_URL`（就是上面的 JSON webhook，不帶 token），以及 `NOTIFY_GATEWAY_URL` + `NOTIFY_GATEWAY_TAG`（Apprise API 的基底位址）。它們會在網頁控制台的管道之外額外使用。
