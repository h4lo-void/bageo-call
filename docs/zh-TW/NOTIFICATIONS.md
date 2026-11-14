# 通知

有來電時、拒接號碼打來時、通話中拒接了插播的第二通時、AI 回電或 AI 代打結束時，主程式都會傳一則訊息；通話結束幾秒後，再傳摘要。等你決定的行程預約跟著摘要一起送，Telegram 另外會收到有 **同意**／**不同意** 按鈕的卡片。設定位置在「設定 → 通知」：「**新增管道**」、選擇服務、填好它的欄位、「**傳送測試**」、「**儲存變更**」。

- 管道想加幾個就加幾個；**每則訊息都會傳到所有啟用的管道**。
- 位址和 token 存在你電腦上的 `data/calls/config/settings.json`。網頁控制台之後不會再完整顯示它們，只會給提示；編輯時把欄位留空，就會保留原本儲存的值。
- 最上面的「傳送通知」關掉之後，所有通知都會停止，但管道不會被刪除。
- 訊息用「通知語言」撰寫（預設跟 AI 講話的語言相同），跟通話講的是什麼語言無關。

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
Transcript / recording: http://192.0.2.10:8787/calls/#call=…
```

上面是語言設為 English 的安裝環境送出的內容；實際的用字會跟著控制台裡設定的語言走。

最後一行在主程式有控制台網址時才會出現：設定裡的「這個控制台的網址」，或控制台對區網開放時自動帶入的網址（Docker 裡不會自動帶入）。行事曆那幾行只有在通話中講定了日期時才會出現（[設定參考](SETTINGS.md#摘要與行事曆)）。
行事曆連結會打開你在「加入行事曆」選的那一家（`calendar_app`；還沒選時是 Google 日曆）：已經填好的「新增活動」表單；選 Apple 日曆或 `.ics` 時則是控制台網址底下這個活動的檔案，用只能開這一個檔案的金鑰打開。打來的人在聯絡人裡時，標題會寫成「☎️ 通話摘要 · 小傑」。

## 你會收到哪些訊息

每則訊息都會傳到所有啟用的管道，用「通知語言」撰寫。每一則開頭都有圖示，一眼就分得出來：

| 圖示與標題 | 什麼時候 | Webhook `event` |
|---|---|---|
| 📞 來電 | 電話被接起來。 | `incoming` |
| ⛔ 拒接號碼來電 | 拒接名單上的號碼打來；訊息會寫是響鈴時直接拒接，還是接起來播語音，以及你當初填的原因。 | `blocked` |
| ☎️ 通話摘要 | 通話結束幾秒後：誰、什麼事、AI 怎麼回、你要處理什麼，以及連結。打來的人在聯絡人裡時，標題後面會加名字。 | `summary` |
| 📵 通話中插播未接 | 通話中又有第二通打進來，手機把它拒接了（對方聽到忙線）。知道號碼的話記得回電。 | `call_waiting` |
| 📞 AI 回電：〈號碼〉 | AI 打給來電者告訴他預約結果：結果如何（接通並告知、沒人接、撥不出去）和那通電話的摘要。 | `ai_callback` |
| 📞 AI 代打：〈名字或號碼〉 | 你交辦的 AI 代打結束了：結果、細節、需要你決定的選項、下一步、談定的時間，以及「代打」頁的連結。 | `ai_errand` |
| 📅 預約待你確認 | 對方跟 AI 約了時間；Telegram 另外會收到有按鈕的卡片（見[行程預約](#行程預約)）。 | 包在 `summary` 裡 |
| 新簡訊 | 選用的[簡訊轉發工具](../../tools/sms_forward/README.md)（英文）轉來的簡訊。 | `event` |

插播未接的訊息長這樣：

```text
📵 通話中插播未接：0912345678（Pixel 9）
時間：11月14日 18:02
```

AI 代打的結果：

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

你沒回答的問題 AI 若先答應了，結果會是「要你決定」，並多一行「AI 在你沒回答的問題上先做了決定（…），請確認；不行要打回去改」。

## 行程預約

開了「行程預約」（[設定參考](SETTINGS.md#摘要與行事曆)）後，對方跟 AI 約好時間的那通電話，摘要下面會多一行（取代行事曆那幾行），你設定的每個通知管道（Telegram、Apprise、Webhook…）都收得到：

```text
📅 預約待你確認：11/14（週六）15:00–16:00 看房 — 到網頁確認：http://bageo.local/calls/#call=…
```

在網頁上決定：「通話」頁最上方的「**待處理事項**」，或該通電話的詳情，選怎麼告訴對方（**傳簡訊**、**AI 回電**或**不通知對方**），再按 **同意**／**不同意**。連結會打開那通電話；控制台沒有別的裝置開得了的網址時（設定裡的「這個控制台的網址」），這行改寫「到網頁控制台確認」。完全沒設定通知管道時不會推播，預約照樣在網頁上等你。主程式不會自己把任何東西寫進行事曆。

有設定 Telegram Bot（bot token 和 chat ID）、「傳送通知」也開著時，等你決定的預約另外會有一張可以直接按的卡片：

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

「簡訊」這一行照這個號碼的「預設回覆方式」寫，也可能是「回覆方式：按同意或不同意後，AI 會打電話告訴對方」（市話，或沒開簡訊）、「不會傳（設定裡「傳簡訊告訴對方」沒開）」、「對方號碼收不到簡訊，請自己回電」，或「未顯示號碼，沒辦法通知對方」。在卡片上按用的是「預設回覆方式」；想換別的方式，請到網頁上決定。

按了之後 Bot 會回覆結果，例如：

```text
✅ 已同意預約
11/14（週六）15:00–16:00 看房
已傳簡訊給 0912345678
📅 加入行事曆：https://calendar.google.com/calendar/render?action=TEMPLATE&…
```

或「❌ 已婉拒預約」、「AI 正在回電給 0912345678」、「簡訊沒傳出：手機沒連線」／「手機 App 太舊，不支援傳簡訊」／「失敗代碼 no_service」。簡訊沒傳出時，網頁上那筆預約會出現「**重傳簡訊**」，可以用原本那支或另一支在線的手機再傳。

**「加入行事曆」哪一家都能用。** Bot 回覆、通知和網頁裡的連結，會連到你第一次按「加入行事曆」時選的行事曆（設定 `calendar_app`）：Google 日曆、Outlook.com、Microsoft 365 或 Yahoo 日曆已經填好的「新增活動」表單；選 Apple 日曆或 `.ics` 時，則是這個活動在「這個控制台的網址」底下的 `.ics` 檔。這個 `.ics` 連結帶的是只能開這一個檔案的金鑰（HMAC），不是你的控制台權杖，從聊天室點開也安全；沒填控制台網址就沒有 `.ics` 連結。主程式不連任何行事曆帳號。

不管在哪裡決定（網頁、卡片或 [API](API.md#行程預約與任務)），只有第一次算數；再按一次（就算是同時按）會回「這個預約已經處理過了」。在網頁或 API 決定後，卡片上的按鈕會拿掉，Bot 也會在聊天裡回同樣的結果，卡片不會再讓你決定已經決定過的事（卡片送到之前你就決定了的話，送到的卡片沒有按鈕）。Telegram 連不上也不影響決定的結果。

告訴對方結果的 AI 回電講的是那通電話的語言，等對方接起來出聲，才說是誰打來、說一次你的回覆，然後道別掛斷。它的通知是「📞 AI 回電：〈號碼〉」；撥不出去時也會傳到你的通知管道。

推銷、詐騙電話不會留下預約。通話後才被判定是這類電話時——摘要標成「推銷電話」「疑似詐騙」（銀行防詐騙、警察通知這類電話不算）——預約會作廢、不發卡片，摘要會寫：

```text
📅 判定是推銷電話，已取消通話中排入的預約：11/14（週六）15:00–16:00 看房
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
| `blocked` | 拒接號碼打來 | `caller_id`、`device`、`time`、`reason`、`declined`（`true`：響鈴時拒接；`false`：接起來播語音） |
| `summary` | 通話結束幾秒後 | `caller_id`、`device`、`time`、`summary`、`contact`（聯絡人名字，不是聯絡人就是空字串） |
| `call_waiting` | 通話中第二通打進來被拒接 | `caller_id`、`device`、`time`、`sim_country` |
| `ai_callback` | 行程預約的 AI 回電結束，或撥不出去 | `caller_id`、`device`、`time`、`summary` |
| `ai_errand` | AI 代打結束，或沒能開始 | `number`、`device`、`time`、`summary` |
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
