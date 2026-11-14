# API

網頁控制台是一個建在小型 JSON API 上的單一頁面，頁面能做的事，腳本都能做：讀通話和逐字稿、改設定、決定行程預約、管理聯絡人、把手機切成 AI 接聽或一般手機，以及讓 AI 替你打電話。這一頁說明 Bageo 1.0 的每一條路由。

- **基底位址：** 主程式監聽的位置，預設是 `http://127.0.0.1:8787`。下面的路由除了 `/health` 和手機用的 WebSocket，都以 `/calls/api/` 開頭。
- **格式：** UTF-8 的 JSON；路由另有說明的除外（WAV、CSV、vCard、iCalendar）。送 JSON body 時請帶 `Content-Type: application/json`。
- **時間：** 通話紀錄用 UTC 的 RFC 3339（`2026-11-14T07:02:11Z`）；行程預約用當地時間（`TZ`），格式 `YYYY-MM-DDTHH:MM`；AI 代打用帶時差的當地時間（`2026-11-15T19:00:00+08:00`）。
- **相容性：** 欄位名稱來自 [`crates/agent/src`](../../crates/agent/src) 裡的 Rust 結構。之後的版本可能會加新欄位，不認得的欄位請直接忽略。

## 存取

沒有設定 `CALLS_UI_TOKEN` 時，任何連得到這個連接埠的人都能使用 API（預設只有這台電腦自己）。有設定時，每個請求都要用下列其中一種方式帶上權杖：

```text
GET /calls/api/list?token=<token>
Authorization: Bearer <token>
X-Calls-Token: <token>
```

權杖錯誤或沒帶，會得到 HTTP 401。`/health` 沒帶權杖也會回應，但只給監控用的幾個欄位（見[手機與健康狀態](#手機與健康狀態)）。

```bash
curl -s -H "Authorization: Bearer $CALLS_UI_TOKEN" \
  "http://127.0.0.1:8787/calls/api/list?limit=5"
```

## 錯誤與狀態碼

錯誤的回應是 `{ok: false, code, error}`：`code` 是固定的英文代碼，可以拿來判斷（網頁控制台會用你的語言顯示），`error` 是詳細說明（大多是英文）。有兩組路由用自己的格式：`POST /calls/api/devices/{serial}/mode` 一律回 `{ok, mode, error_code}`；AI 代打的路由在 `error` 和 `code` 放同一個代碼。

| HTTP | 什麼時候 | 常見的 `code` |
|---|---|---|
| 200 | 完成。 | — |
| 400 | 請求有誤：不是 JSON、沒帶 `Content-Type: application/json`、id 或值不對。 | `bad_request`、`invalid_id`、`missing_field`、`invalid_contact`、AI 代打的代碼 |
| 401 | 沒帶權杖或權杖錯誤。 | `unauthorized` |
| 404 | 找不到這通電話、任務、聯絡人或代打。 | `not_found`、`task_not_found`、`contact_not_found`、`no_audio` |
| 409 | 這個變更跟已存的資料衝突。 | `contact_exists`、`sms_not_retryable` |
| 413／415 | 匯入的檔案太大／格式不認得。 | `file_too_large`、`unsupported_file` |
| 502 | 外部服務失敗（通知管道、Gemini）。 | `auth_failed`、`network_error`、`ai_failed`、`preview_failed` |

### 防止其他網頁冒用

控制台沒有登入 cookie，但你區網裡的瀏覽器還是可能被別的網頁騙去送請求給它。所以凡是會撥電話、傳簡訊、做決定或改動已存資料的路由，一律要求 `Content-Type: application/json`（沒帶就回 HTTP 400 `bad_request`）。別的網站的頁面要送這個標頭，必須先經過 CORS 預檢，而主程式從不回應預檢。適用的路由：

- `POST /calls/api/tasks/{id}/confirm` 和 `…/ignore`；
- `POST /calls/api/appointments/{id}/retry-sms`；
- `POST /calls/api/devices/{serial}/mode`；
- `POST /calls/api/contacts`、`PUT /calls/api/contacts/{id}`、`POST /calls/api/contacts/batch`（`POST /calls/api/contacts/import` 則要帶 `X-Filename` 標頭）；
- `POST /calls/api/errands`、`…/{id}/cancel`、`…/{id}/retry`、`…/{id}/say`。

允許空 body 的路由，空 body 當成 `{}`。這是第二道防線；第一道是控制台對區網開放時一定要設的 `CALLS_UI_TOKEN`。

## 通話

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/list` | 通話紀錄，最新的在前。查詢參數：`limit`、`offset`、`q`（對號碼、手機、事件、逐字稿做文字搜尋）、`caller`、`serial`、`status`、`has_transcript`。回應：`{ok, calls: [...], total, limit, offset, contacts_by_number}`；`contacts_by_number` 是 `{<caller_id>: {id, name}}`，列出這一頁裡是聯絡人的來電者。 |
| `GET /calls/api/call/{id}` | 單一通話：`{ok, call, notes, contact, calendar_link, calendar_links}`。`contact` 是 `{id, name, note}` 或 `null`。`calendar_links` = `{google, outlook, office365, yahoo}`：各網頁行事曆填好這通電話行事曆建議的「新增活動」表單（`calendar_link` 是 Google 那個，留給舊版用）。 |
| `GET /calls/api/call/{id}/event.ics` | 行事曆建議，iCalendar 檔。該通話沒有建議時回 HTTP 404。用控制台權杖開，或用 `?k=<金鑰>`：通知連結帶的是只能開這一個檔案的 HMAC 金鑰，不是權杖。 |
| `GET /calls/api/audio/{id}/listen` | 你當時會聽到的整通電話（對方和 AI 混在一起），WAV，支援 range 請求。 |
| `GET /calls/api/audio/{id}/remote` · `/tts` · `/audio/{id}` | 只有對方 · 只有 AI · 原始的混音錄音。檔案不存在時回 HTTP 404 `no_audio`。 |
| `GET /calls/api/live` | 進行中的通話和已連線的手機：`{ok, count, calls, control_idle_clients, control_idle_serials, normal_mode_serials}`。`normal_mode_serials` 是已連線、但切成一般手機的手機；網頁不把它們算進待命手機。 |
| `GET /calls/api/stats` | `{ok, stats: {total, today, voicemails, blocked}}`。 |
| `GET /calls/api/export.csv` | 篩選後的清單，CSV 格式（查詢參數同 `list`，最多 2000 列）。 |

### 通話紀錄

通話紀錄（`call`，以及 `calls` 裡的每一筆）帶有的欄位包括：

| 欄位 | 意思 |
|---|---|
| `id` | 紀錄 id，`/call/{id}` 和 `/audio/{id}` 路由都用它。 |
| `serial` | 接這通（或撥出這通）的手機。 |
| `caller_id` | 手機回報的對方號碼；未顯示號碼時是空字串。 |
| `started_at`／`ended_at` | RFC 3339，UTC。 |
| `duration_ms` | 通話長度。 |
| `status` | `ringing`、`active`、`ended`。 |
| `events` | 依序發生的事——例如 `active`、`persona_stall_spam`（判定推銷／詐騙）、`reject_list`（拒接號碼）、`call_waiting_rejected`（通話中拒接的第二通）、`ai_callback`（行程預約的 AI 回電）、`ai_errand`（AI 代打）、`persona_ai_fail_hangup`、`ended`。 |
| `summary` | 寫好後的摘要文字。 |
| `appointment` | 對方跟 AI 約了時間時：通話結束當下的預約（見[行程預約與任務](#行程預約與任務)）。之後的變化（你的決定、簡訊和回電結果）會更新在 `notes.appointment`。 |
| 音訊旗標 | 有哪些錄音檔。 |

### 摘要、逐字稿與翻譯

摘要寫好之前 `notes` 是 `null`，之後包含：

| 欄位 | 意思 |
|---|---|
| `summary` | 用通知語言寫的摘要。 |
| `dialogue` | 逐字稿，一行一次發言，前面加上主程式語言的說話者標籤和分隔符號：繁體中文是 `對方：`／`接聽：`，簡體中文 `对方：`／`接听：`，英文 `Caller: `／`AI: `，日文 `相手：`／`AI：`，韓文 `상대: `／`AI: `，西班牙文 `Llamante: `／`IA: `，法文 `Appelant : `／`IA : `，德文 `Anrufer: `／`KI: `，葡萄牙文 `Chamador: `／`IA: `。舊版存的紀錄不論語言都是 `對方：`／`接聽：`。每一行保留當時講的語言。 |
| `dialogue_translations` | 有翻譯時才出現：一個陣列，跟 `dialogue` 一行對一筆（同樣的索引）。用別種語言講的那一行，這裡是翻成通知語言的譯文；本來就是你的語言、太短或沒翻的那一行是 `null`。網頁控制台會把譯文顯示在原文下面。 |
| `end_hint`、`source`、`updated_at` | 通話怎麼結束、誰寫的、什麼時候寫的。 |
| `calendar` | 通話中講定了日期時：`{title, start, end, all_day}`（當地時間，`YYYY-MM-DDTHH:MM`）。 |
| `appointment` | 預約目前的狀態。 |

```json
{
  "dialogue": "對方：Hello, is Alex there?\n接聽：Yes, speaking. Who's calling?",
  "dialogue_translations": ["你好，Alex 在嗎？", "我就是，請問哪位？"]
}
```

打來的人在聯絡人裡時：通話列表顯示名字（`contacts_by_number`）、通話詳情也有（`contact`）、通話摘要通知的標題在號碼後面加名字，一般電話的 AI 提示詞也會說是誰打來的。

## 設定

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/settings` | `{ok, settings, storage_path, console_url, console_ip_url}`。`console_url` 是目前用的控制台網址（你填的，或自動帶入的；只有這台電腦能開時是空的），`console_ip_url` 是把 `.local` 名稱換成區網 IP 的同一個網址。機密欄位會以遮罩形式回傳（`"abc…wxyz"`），並附一個 `<name>_set` 旗標；通知管道回傳時 `url`／`token` 是空的，另外附上 `url_hint`、`token_hint`、`url_set`、`token_set`。唯讀的輔助欄位：`default_prompt`、`voices`、`ambience_kinds`、`languages`、`countries`（「各 SIM 卡的主要語言」可選的國家代碼）、`seen_sim_countries`（主程式啟動後手機回報過的 SIM 卡國家）、`notify_kinds`、`sample_lines`、`summary_ready`。 |
| `PUT /calls/api/settings` | 一個 JSON 物件，只放要修改的欄位。回應同 `GET`。下一通電話就生效，不用重新啟動。 |

值得知道的欄位（全部都在 [`settings_store.rs`](../../crates/agent/src/settings_store.rs) 的 `RuntimeSettings` 和[設定參考](SETTINGS.md)有說明）：

| 欄位 | 值 |
|---|---|
| `owner_name` | 你的名字，用在 `{owner}`。 |
| `owner_aliases` | 字串陣列，最多 10 個：其他人怎麼叫你。多的和空白的會被丟掉。 |
| `answer_style` | `assistant`／`persona`（本人）。 |
| `errand_identity` | `assistant`（預設）／`owner`：AI 代打時說自己是誰；其他值存成 `assistant`。 |
| `summary_provider` | `gemini`（預設）／`custom`。`custom` 用 `summary_api_base`、`summary_model`（填了網址就必填）和 `deepseek_api_key`（那家服務的金鑰）。舊的 `auto`／`deepseek` 會在設定載入時轉換一次。 |
| `calendar_app` | `google`、`outlook`、`office365`、`apple`、`yahoo`、`ics` 或空白（還沒選）；其他值存成空白。 |
| `appointment_mode` | `off`／`confirm`（舊的 `auto` 當作 `confirm`）。 |
| `appointment_sms` | `true`／`false`：要不要傳簡訊告訴對方你的決定。 |
| `appointment_reply` | `auto`、`sms` 或 `call`。 |
| `reject_numbers`、`reject_mode` | `{number, reason}` 的清單；`decline`（響鈴時拒接）或接起播語音。 |
| `country_code` | 只有數字（`886`、`1`）。控制台不顯示；這裡有值時優先於 `COUNTRY_CODE`。 |
| `gemini_thinking_level` | `low`／`medium`／`high`（只對 thinking 模型有效）。 |

`PUT` 的規則：

- 機密欄位（`gemini_api_key`、`deepseek_api_key`）沒帶或是 `""` 時，保留原本儲存的值；`"<name>_clear": true` 則會清空它。
- `notify_channels` 是整個取代。一列 `{id, kind, name, enabled, url, token, target}` 的 `url` 或 `token` 是空的時，會保留 `id` 和 `kind` 相同的那一列原本儲存的值；沒有 `id` 的列是新增的；你沒列出來的列會被刪除。
- 舊版存的、這版已經沒有的欄位，讀進來時忽略，下次存檔就不見：Google 日曆憑證（`gcal_*`）、`calendar_auto_add`、`intl_carrier_prompt`，以及已移除的模擬日曆的測試號碼與測試簡訊設定。

```bash
curl -s -X PUT "http://127.0.0.1:8787/calls/api/settings" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"language": "ja", "owner_aliases": ["Alex", "小明"], "errand_identity": "owner",
       "reject_numbers": [{"number": "0800*", "reason": "電話行銷"}]}'
```

## 通知

| 方法與路徑 | 內容 |
|---|---|
| `POST /calls/api/notify/test` | Body `{"channel": {kind, url, token, target, id?}}`。把測試訊息傳到那一個管道（已儲存的機密會依 `id` 補上）。回 HTTP 200 `{ok: true}`，或 502 `{ok: false, code, field, kind, error}`：`code` 是出錯的原因（`missing_field`、`bad_url`、`bad_value`、`auth_failed`、`service_not_found`、`http_error`、`service_error`、`network_error`、`send_failed`），`field` 是 `url`／`token`／`target` 哪一欄（或空白），`error` 是該服務的回應。 |
| `POST /calls/api/notify/send` | Body `{"title": "...", "body": "..."}`。把你的訊息傳到每一個啟用的管道。只要有一個管道接受，就回 `{ok: true}`。[簡訊轉發工具](../../tools/sms_forward/README.md)（英文）就是用這條路由。 |

你自己的 webhook 會收到的 JSON 和每一種事件，在[通知](NOTIFICATIONS.md#webhook-json)裡有說明。

## 語音與背景音試聽

`GET /calls/api/preview` 會回傳一個 WAV。查詢參數：`voice`（聲音名稱，或用 `none` 表示只要背景音）、`ambience`（`off` 或某一種背景音）、`level`（1–3）、`regen=true`（重新產生，不用已存的範例）、`clip=reject`（用該聲音念的拒接號碼語音）、`lang`（`ja`、`zh-TW`…；留空 = 存著的語言）。內建的 30 個聲音在 9 種語言都已預先錄好試聽和拒接語音——`assets/voices/` 裡共 1,080 段——所以試聽不用連 Gemini；只有 `regen=true` 或沒有預錄的聲音才會開一次連線。錯誤：502 `preview_failed`。

## 行程預約與任務

`appointment_mode` 設成 `confirm` 時（見[設定](SETTINGS.md#摘要與行事曆)），對方跟 AI 約好時間的那通電話會留下一筆任務（`intent` 為 `appointment`），通話紀錄、它的 notes 和任務上都有一個 `appointment` 物件：

```json
{
  "start": "2026-11-14T15:00", "end": "2026-11-14T16:00",
  "title": "看房", "caller_name": "陳先生",
  "mode": "confirm", "status": "confirmed",
  "reply": "sms", "sms": "sent", "call_lang": "zh-TW",
  "tag": "6f1c2a9e-3b7d-4e0a-9c55-2d8f1b7e4a10",
  "tg_card": {"chat_id": "123456789", "message_id": 42}
}
```

| 欄位 | 意思 |
|---|---|
| `start`、`end` | 當地時間（`TZ`）。對方只是順口提的備案時間不會蓋掉談好的時間；同一通電話另外約的第二件事會變成另一筆任務。 |
| `title`、`caller_name` | 約的是什麼事、對方怎麼自稱（來電者自己的話）。 |
| `status` | `pending`（等你決定）、`confirmed`、`declined`；`booked` 只會出現在舊版的紀錄。 |
| `reply` | 怎麼告訴對方：`sms`、`call` 或 `none`。 |
| `sms` | `sent`，或簡訊沒傳出的原因：`offline`（手機沒連線）、`old_app`（App 不能傳簡訊）、`timeout`、`busy`（同一則簡訊正在傳）、`no_number`（收不到簡訊的號碼），或手機回的代碼：`no_permission`、`bad_request`、`no_service`、`radio_off`、`generic_failure`、`exception`。網頁和 Telegram 回覆會用白話寫。 |
| `call` | AI 回電：`queued`（等打給同一個號碼的前一通回電結束）、`dialing`、`connected`（手機已撥號，AI 等對方接起來出聲）、`answered`（對方已得知結果）、`no_answer`（70 秒內沒人出聲）、`failed`、`done`（通話結束、結果不明），或沒撥出的原因：`no_phone`、`send_failed`、`no_number`、`not_callable`（國外、免付費、加值或特殊號碼）。手機的回報也記在任務的 `events`（`callback_place_call_ack`、`callback_place_call_failed`、`callback_outbound_ready`、`callback_done`、`callback_failed`）。 |
| `call_lang` | 那通電話講的語言（`zh-TW`、`en`…）。AI 回電就講這個語言，對方接起來出聲後才說你的回覆，只說一次。沒有這個欄位的舊紀錄照設定 `language`。 |
| `tg_card` | 發給這筆預約的 Telegram 卡片 `{chat_id, message_id}`；在別處決定後用來拿掉卡片上的按鈕。 |
| `tag` | 這筆預約固定的 id。 |

空的欄位不會出現。

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/tasks` | 任務，最新的在前（最多 200 筆）：`{ok, tasks}`。預約任務另外帶 `calendar_links`（`{google, outlook, office365, yahoo}`）、`reply` = `{sms, call, default}`（這位來電者能不能收簡訊、能不能回電，以及「預設回覆方式」選的方式），以及 `sms_route` = `{serial, label}`：你沒另選手機時簡訊從哪支手機發（接這通電話的手機；`label` 是它在 `device_labels` 的名稱，沒有就是 id）。網頁的「待處理事項」就是 `appointment.status` 為 `pending` 的任務。 |
| `GET /calls/api/tasks/{id}/event.ics` | 預約的 iCalendar 檔（Apple 日曆和其他 App）。用控制台權杖或連結的 `?k=<HMAC 金鑰>` 開。 |
| `POST /calls/api/tasks/{id}/confirm` | **同意**，跟在網頁或 Telegram 卡片上按一樣。Body：空的、`{}` 或 `{"reply": "sms" \| "call" \| "none"}`（沒帶：照「預設回覆方式」；選了這位來電者用不了的方式就不通知）。回應 `{ok, task, message}`；`message` 是用白話寫的結果，跟 Telegram Bot 回的同一段，附上你選的 `calendar_app` 的「加入行事曆」連結。傳簡訊最多可能等 90 秒；回電交給手機撥出（在 `appointment.call` 看進度）。 |
| `POST /calls/api/tasks/{id}/ignore` | **不同意**：用同樣的方式告訴對方，客氣婉拒。body、回應與規則同上。 |
| `POST /calls/api/appointments/{id}/retry-sms` | 已決定的預約簡訊沒傳出時，再傳一次。Body：空的、`{}` 或 `{"serial": "<手機>"}`（另一支連著的手機；沒帶：接這通電話的手機）。回應 `{ok, sms, message}`：傳出時 `ok` 為 true，`sms` 是新的代碼。還沒決定、已經傳出、或本來就不會傳（`no_number`）時回 HTTP 409 `sms_not_retryable`（附目前的 `sms`）；404 `task_not_found`；400 `invalid_serial`／`bad_request`。 |

決定的規則：

- **只有第一次決定算數。** 之後再同意或不同意——包括第一次還在處理時送來的，或在 Telegram 卡片上按的——都只回「已經處理過了」，不會有任何變化。
- 這筆預約有發過 Telegram 卡片時，卡片上的按鈕會拿掉，並把 `message` 回到那個聊天（盡力而為，Telegram 失敗不影響決定）。
- 簡訊和回電都從那通電話打進來的 SIM 發出（`sub_id`）。簡訊只寫時間（含星期），不寫約的是什麼事。
- 推銷、詐騙電話不會留下預約；通話後才被判定是這類電話的，預約也會作廢。

```bash
curl -s -X POST "http://127.0.0.1:8787/calls/api/tasks/$TASK/confirm" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"reply": "call"}'
```

```json
{"ok": true, "task": {"id": "…", "appointment": {"status": "confirmed", "reply": "call", "call": "dialing"}},
 "message": "✅ 已同意預約\n11/14（週六）15:00–16:00 看房\nAI 正在回電給 0912345678\n📅 加入行事曆：https://calendar.google.com/…"}
```

## AI 代打

讓 AI 替你打電話：給它號碼（或聯絡人）和要辦的事——訂位、跟朋友約時間、問店家事情——一支 AI 模式的手機就會撥出。

**通話中。** 網頁控制台可以看即時字幕。遇到你沒交代清楚的事，AI 會先跟對方說要確認一下，再問你（`ask_owner` 函式）：你正在看這一筆（6 秒內有人讀過它的即時畫面），問題就出現在網頁上，最多等 40 秒；沒在看或逾時，AI 會問對方有哪些可行的選項、說你確認後再回電，然後客氣收尾。AI 問你時會停下來等答案，這段時間對這件事什麼都不答應；對方先道別，這個問題就到此為止。在你交代的範圍內的選擇（「7 點半到 8 點也可以」）AI 會自己答應，不會來問你。通話中你在網頁打的字，會當成指示交給 AI。

**通話後。** 摘要模型依交辦內容整理結果，發一則通知（標題「📞 AI 代打：〈名字或號碼〉」，用通知語言；webhook 事件是 `ai_errand`）。你沒回答的事 AI 若先答應了，結果一律不會是 `done`，而是 `need_owner`：請你確認，不行再打回去改。

**用什麼身分。** 設定 `errand_identity`（`assistant` 助理，預設；`owner` 本人），每筆也能單獨選。不論哪種，被認真問是不是真人時都會承認是 AI，不給交辦以外的個資，不答應付款、訂金、轉帳。

**能撥哪些號碼。** 不撥緊急號碼、7 碼以下的短碼與加值付費號碼（例如台灣 020x 開頭的 0204、0209，+1 900／976，日本 0990，英國 09…）；免付費電話可以撥。沒有 `+` 的號碼一律當成本國號碼（`country_code`）；如果開頭是國際冠碼（`00`、台灣的 `01x`／`00x`、北美 `011`、日本 `010`）就是 `not_allowed`（國外號碼請寫 `+`），台灣號碼去掉開頭的 0 後以 1 開頭也擋。

**用哪支手機、哪張 SIM。** 從一支連著、在 AI 模式、有該國 SIM 的手機撥出。主程式從手機回報的來電（`incoming` 和 media `hello` 裡的 `sim_sub_id`、`sim_country`）記下每支手機有哪些 SIM（subscription id 與國家），控制連線的 `hello` 帶有 `sims: [{sub_id, country}]` 清單時也會採用（1.0 版 App 還不會送）；這些存在 `phone_sims.json`。完全不知道有哪些 SIM 的手機不接代打（`sim_unknown`：先打給它一通）。送給手機的是 E.164 格式的號碼和要用的 SIM（`sub_id`）；這通電話自己的回報（media `hello`）顯示用的是別張 SIM、或沒回報時，主程式會在 AI 開口前立刻掛斷（`failed`、`call_code` `wrong_sim`，會通知你），並先把那張卡從這支手機的紀錄裡拿掉，等之後有來電回報它再加回來。

**排隊與上限。** 手機通話中：這筆排隊（`queued`），超過 10 分鐘算失敗（`phone_busy`）。同一個號碼一次只打一通，跟行程預約的 AI 回電共用排隊；排在同號碼前一通後面的最多等 25 分鐘（`number_busy`）。一通最長 10 分鐘（9 分半開始收尾）。語音選單：AI 最多等 90 秒等真人來接（它不能按鍵；`keypad_menu`）；電信錄音（「您撥的號碼是空號」）：直接掛斷（`carrier_message`）。不會自己重撥：要「再打一次」由你按。取消撥號中的代打，手機和號碼會立刻放出來給下一通；萬一那通電話還是接通了，會在 AI 開口前掛斷。

### 代打物件

```json
{
  "id": "e3f1a29c0b7d4e55", "created_at": "2026-11-14T18:02:11+08:00", "updated_at": "2026-11-14T18:05:40+08:00",
  "number": "0223456789", "name": "鼎泰豐", "contact_id": "",
  "task": "明天晚上 7 點訂 4 位，名字寫王先生。7 點半到 8 點也可以。",
  "identity": "assistant", "serial": "pixel-12345", "device_label": "Pixel 9",
  "status": "done", "call_code": "answered", "attempts": 1, "record_id": "c8b2…",
  "follow_up_of": "",
  "questions": [{"id": "q1", "text": "7 點客滿，7 點半還是 8 點 15 分？", "options": ["7 點半", "8 點 15 分"],
                 "answer": "7 點半", "answered_by": "owner", "asked_at": "2026-11-14T18:03:05+08:00"}],
  "result": {"outcome": "done", "summary": "已訂明天 7 點半 4 位，名字王先生",
             "details": "座位保留 15 分鐘。", "follow_up": "", "options": [],
             "event": {"start": "2026-11-15T19:30:00+08:00", "title": "鼎泰豐晚餐"}},
  "history": []
}
```

| 欄位 | 意思 |
|---|---|
| `id` | `e` 加 16 個十六進位字元。 |
| `number` | 你寫的號碼，去掉空白和連字號。 |
| `name`、`contact_id` | 要用的名字（沒填就用聯絡人的名字），以及來自哪位聯絡人。 |
| `task` | 你自己寫的交辦內容（最多 1,000 字）。 |
| `identity` | `assistant` 或 `owner`。 |
| `serial`、`device_label` | 撥出的手機和它的名稱。 |
| `status` | `queued`、`dialing`（電話在響，還沒有人出聲）、`on_call`（對方接起來出聲了）、`done`、`no_answer`、`failed`、`cancelled`。後四個是最終狀態。 |
| `call_code` | 撥號進行到哪裡：`dialing`、`queued`、`answered`、`no_answer`、`failed`、`phone_busy`、`number_busy`、`no_phone`、`send_failed`、`keypad_menu`、`carrier_message`、`wrong_sim`、`agent_restarted`、`cancelled`。 |
| `attempts` | 從 1 開始，每按一次「再打一次」加 1。 |
| `record_id` | 這一次的通話紀錄（`GET /calls/api/call/{id}`：逐字稿和錄音）。 |
| `follow_up_of` | 這通是接續哪一筆先前的代打（那一筆最後 20 行逐字稿會放進 AI 的腳本）。 |
| `questions` | `[{id, text, options, answer, answered_by, asked_at}]`：AI 問過你的問題。`answered_by`：`owner`（你回答了）、`timeout`（你在看但沒來得及回答）、`absent`（沒人在看）；還在等時是 `null`。選項最多 4 個。 |
| `result` | `null` 或 `{outcome, summary, details, follow_up, options, event}`。`outcome`：`done`、`partial`、`not_done`、`need_owner`（選項在 `options`，最多 8 個）、`no_answer`、`voicemail`、`failed`。`event`：`null` 或 `{start, end, title}`（帶時差的當地時間；沒談定結束時間就沒有 `end`）。 |
| `history` | 先前幾次的嘗試，`[{attempt, status, result, record_id, ended_at, questions}]`。 |

代打存在資料夾裡的 `errands.json`（保留最新 500 筆）。主程式重新啟動時還在進行中的代打，會變成 `failed`（`call_code` `agent_restarted`）。

### 代打路由

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/errands` | `{ok, errands}`，最新的在前，最多 200 筆。 |
| `POST /calls/api/errands` | 開始一筆。Body `{number, name, contact_id, task, identity, serial, follow_up_of}`：`task` 和號碼必填（或給聯絡人、先前的代打，用它們的號碼）；`identity` 留空 = `errand_identity`；`serial` 留空 = 任何合適的手機。回應 `{ok, errand}`（狀態 `dialing`，手機或號碼忙線時是 `queued`）。 |
| `POST /calls/api/errands/{id}/cancel` | 排隊中：取消；撥號中或通話中：掛斷。已經結束的原樣回傳。`{ok, errand}`。 |
| `POST /calls/api/errands/{id}/retry` | 同一筆再打一次（`attempts` 加 1，上一次移到 `history`）。`{ok, errand}`；還在進行中（或上一通還沒結束）：400 `busy`；手機相關的代碼同新增。 |
| `DELETE /calls/api/errands/{id}` | 從清單刪掉（通話紀錄會留著）。`{ok}`；撥號中或通話中：400 `busy`。 |
| `GET /calls/api/errands/{id}/live?after=<seq>` | `{status, lines, question, watching}`。`lines`：`seq` 之後的字幕，`[{seq, who, text, at}]`，`who` 是 `ai`、`callee`、`owner` 或 `system`（保留最新 400 行；1.2 秒沒有新字就算一行結束）。`question`：`null` 或 `{id, text, options, asked_at, expires_at}`，等你回答的問題。`watching`：你是否算「正在看」。讀這條路由會讓你算是正在看 6 秒，所以頁面開著時每隔幾秒讀一次。 |
| `POST /calls/api/errands/{id}/say` | Body `{text, question_id}`（最多 300 字）：帶 `question_id` 就是回答那個問題；沒帶時，有問題在等就當成它的答案，否則當成給 AI 的指示（「跟他說 8 點也可以」）。`{ok}`。 |

錯誤代碼（HTTP 400 `{ok: false, error, code}`，兩者同值）：

| 代碼 | 路由 | 意思 |
|---|---|---|
| `no_number` | 新增 | 沒有號碼，也沒有可以拿號碼的聯絡人或先前代打。 |
| `bad_number` | 新增 | 不是電話號碼（有字母、`+` 不在開頭、超過 15 碼、以那個國家來說太短）。 |
| `not_allowed` | 新增 | 緊急號碼、短碼、加值付費號碼，或用國際冠碼代替 `+`。 |
| `no_task` | 新增 | `task` 是空的。 |
| `task_too_long` | 新增 | `task` 超過 1,000 字。 |
| `no_phone` | 新增、再打 | 沒有連著、在 AI 模式的手機（或指定的 `serial` 不是這樣的手機）。 |
| `sim_unknown` | 新增、再打 | 沒有合適的手機，而且至少有一支的 SIM 主程式還不知道：先打給它一通。 |
| `no_sim_for_country` | 新增、再打 | 沒有手機有這個號碼所屬國家的 SIM。 |
| `not_found` | 新增 | `contact_id` 或 `follow_up_of` 不存在（這裡回 400）。 |
| `busy` | 再打、刪除 | 這筆還在撥號或通話中。 |
| `not_on_call` | say | 電話不在通話中（或已經結束）。 |
| `empty_text`／`text_too_long` | say | 沒有內容／超過 300 字。 |
| `question_closed` | say | 那個問題已經回答、逾時，或不是正在等的那一個。 |
| `bad_request` | 所有 `POST` | 沒帶 `Content-Type: application/json`，或 body 不是 JSON 物件。 |

所有 `…/errands/{id}…` 路徑，代打 id 不存在（或格式不對）時一律回 HTTP 404 `not_found`。

```bash
curl -s -X POST "http://127.0.0.1:8787/calls/api/errands" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"number": "02-2345-6789", "name": "鼎泰豐",
       "task": "明天晚上 7 點訂 4 位，名字寫王先生。7 點半到 8 點也可以。"}'

curl -s "http://127.0.0.1:8787/calls/api/errands/e3f1a29c0b7d4e55/live?after=0" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN"

curl -s -X POST "http://127.0.0.1:8787/calls/api/errands/e3f1a29c0b7d4e55/say" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"text": "7 點半可以", "question_id": "q1"}'
```

```json
{"status": "on_call",
 "lines": [{"seq": 3, "who": "callee", "text": "7 點已經客滿了，7 點半或 8 點 15 分可以嗎？", "at": "2026-11-14T18:03:01+08:00"},
           {"seq": 4, "who": "ai", "text": "我確認一下喔。", "at": "2026-11-14T18:03:04+08:00"}],
 "question": {"id": "q1", "text": "7 點客滿，7 點半還是 8 點 15 分？", "options": ["7 點半", "8 點 15 分"],
              "asked_at": "2026-11-14T18:03:05+08:00", "expires_at": "2026-11-14T18:03:45+08:00"},
 "watching": true}
```

## 聯絡人

替打來的號碼配上名字，存在通話紀錄資料夾（`CALLS_DATA_DIR`）的 `contacts.json`。號碼不管怎麼寫都對得上：`+886912345678`、`0912-345-678`、`886912345678` 是同一支；沒有國碼的號碼照你的國家（`country_code`）讀，通話則照接到這通電話的 SIM 卡所屬國家讀。聯絡人也用在 AI 代打（選聯絡人就不用打號碼）和推銷判斷：已存的聯絡人講到錢，不會被當成推銷或詐騙。

一位聯絡人是 `{id, name, phones: [...], note, source, updated_at}`；`source` 是 `manual`、`vcf`、`csv`、`text` 或 `ai`（Gemini 讀出來的）。一支號碼只屬於一位聯絡人。

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/contacts` | 查詢參數 `q`（名字、備註或號碼，號碼怎麼寫都可以）、`limit`、`offset`。回應 `{ok, contacts, total, offset, count}`：依名字排序的符合結果（共 `total` 筆），以及全部聯絡人數（`count`）。 |
| `POST /calls/api/contacts` | Body `{name, phones: [...], note}`，回應 `{ok, contact}`。號碼已經是別的聯絡人的：HTTP 409 `contact_exists`，附 `contact: {id, name}`；沒有名字或沒有號碼：400 `invalid_contact`。 |
| `PUT /calls/api/contacts/{id}` | 同樣的 body，取代名字、號碼和備註。404 `contact_not_found`、409、400 同上。 |
| `DELETE /calls/api/contacts/{id}` | `{ok}`。 |
| `POST /calls/api/contacts/import` | Body 就是檔案本身（最大 10 MB），檔名 URL 編碼後放在 `X-Filename`。只讀、**不存**（預覽）：`{ok, file, format, ai, entries: [{name, phones, note, source, duplicate}], skipped}`。`format`：`vcf`、`csv`、`text`、`xlsx`、`docx`、`pdf` 或 `image`；`ai`：是 Gemini 讀的；`duplicate`：`null`，或這筆重複的已存聯絡人 `{id, name, by: "phone" \| "name"}`；`skipped`：沒有名字或號碼而略過的筆數。錯誤：400 `empty_file`、413 `file_too_large`、415 `unsupported_file`、400 `need_gemini_key`、502 `ai_failed`（Gemini 的回覆在 `error`）。 |
| `POST /calls/api/contacts/batch` | Body `{contacts: [{name, phones, note, source}], duplicates: "merge" \| "skip"}`：匯入時勾選要留的那些。跟已存聯絡人重複的，合併進去（補上新號碼和備註）或略過。回應 `{ok, added, merged, skipped, count}`。 |
| `GET /calls/api/contacts/export.vcf` | 全部聯絡人匯出成 vCard 3.0 檔。 |

主程式自己讀的格式：vCard 2.1、3.0、4.0（含 quoted-printable、各種 `CHARSET`）；有表頭（Google 通訊錄、Outlook，或欄名像 *姓名*、*電話*、Name、Phone）或沒有表頭的 CSV、TSV，編碼 UTF-8、UTF-16 或電腦的編碼（Big5、GBK、Shift_JIS、EUC-KR、Windows-1252）；一行一個「名字 號碼」的純文字；`.xlsx`、`.docx` 裡的表格。PDF、圖片（印出來的名單照片、通訊錄截圖）和讀不到人的檔案交給 Gemini 的視覺模型（`gemini-flash-lite-latest`，用設定裡的 Gemini 金鑰）。

```bash
curl -s -X POST "http://127.0.0.1:8787/calls/api/contacts/import" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" \
  -H "X-Filename: $(printf %s '家人名單.pdf' | jq -sRr @uri)" \
  --data-binary @"家人名單.pdf"
```

```text
POST /calls/api/contacts
Content-Type: application/json
{"name": "林小美", "phones": ["0912 000 000", "+1 202 555 0143"], "note": "我姊"}

{"ok": true, "contact": {"id": "4f0c9a…", "name": "林小美", "phones": ["0912 000 000", "+1 202 555 0143"],
 "note": "我姊", "source": "manual", "updated_at": "2026-11-14T08:00:00Z"}}
```

## 手機與健康狀態

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/devices` | `{ok, usb: {enabled, adb_found, adb_path, error, phones: [{serial, state, model, reverse, app_installed}]}, connected: [...], labels: {<phone id>: <model>}, calls_in_progress, phones: [...]}`。最外層的 `phones` 是主程式啟動後在 control 連線上見過的每支手機：`{serial, connected, mode, supports_mode, model, app_version, in_call, last_seen_ms}`。`mode` 是 `ai`（AI 接聽）或 `normal`（一般手機，見[手機設定](PHONE.md#一般手機模式)）；比這個功能舊的 App，`supports_mode` 是 false，永遠是 `ai`。 |
| `POST /calls/api/devices/{serial}/mode` | Body `{"mode": "ai" \| "normal"}`。叫手機切換，最多等 15 秒等它回覆。一律回 `{ok, mode, error_code}`（見下方）。 |
| `GET /health` | 一個很大的扁平 JSON 物件，用於排查問題：版本、語言、哪些東西已設定、已連線的手機、進行中的通話；不含金鑰或權杖。`control_idle_clients`／`control_idle_serials` 算的是所有連上的手機；`normal_mode_serials` 列出切成一般手機的那幾支；`local_time` 是主程式依 `TZ` 的時鐘。有設定控制台權杖而請求沒帶時，只回 `ok`、`version`、`language`、`active_call_count` 和 `control_idle_clients`——監控用已經足夠。 |

`POST /calls/api/devices/{serial}/mode` 的細節：`mode` 是切換後手機的模式（不知道時為 `null`）；成功時 `error_code` 是 `null`，否則是下面其中之一：

| `error_code` | 意思 |
|---|---|
| `offline` | 手機沒連上。 |
| `unsupported` | App 比模式切換功能舊。 |
| `in_call` | 有電話在響或通話中；結束後再試。 |
| `busy` | 上一次切換還在做。 |
| `no_root` | Bageo 模組的 root 小幫手沒有接手：模組沒有在執行。 |
| `save_failed`、`dialer_failed`、`screening_failed`、`dnd_failed`、`volume_failed`、`failed` | 沒成功的那一步。再切一次會把每一步重做一遍。 |
| `timeout` | 還沒確認——不是失敗：切換本身可能讓手機 App 重新啟動，它重新連上後才回報。稍後再讀一次 `GET /calls/api/devices`。 |
| `bad_request`／`invalid_mode` | HTTP 400：不是 JSON、沒帶 `Content-Type: application/json`、沒有字串 `mode`、serial 不是英數字、`-`、`_` 組成；或 `mode` 不認得。 |

```bash
curl -s -X POST "http://127.0.0.1:8787/calls/api/devices/pixel-12345/mode" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"mode": "normal"}'
# {"ok": true, "mode": "normal", "error_code": null}
```

## 手機這一端（WebSocket）

手機透過 WebSocket 連到 `/` 或 `/ws`（兩者相同）：每支手機一條長期維持的 *control* 連線，每通電話一條 *media* 連線，雙向傳送 JSON 文字訊框和原始的 16 kHz 單聲道 16 位元小端序 PCM 二進位訊框（手機每 60 ms 送一框）。從這台電腦本身來的連線（USB 通道）一律接受；其他連線在有設定權杖時，必須帶上 `?token=<DEVICE_TOKEN>`。

逐欄位的完整訊息列表在 [`android/README.md`](../../android/README.md#websocket-protocol)（英文）。任何會講這個通訊協定的東西都可以代替手機——所以要加上 SIP 或軟體電話前端，可以完全不動通話邏輯。雙方都會忽略不認得的東西，所以舊 App、舊主程式照樣能用。

### Control 連線

| 方向 | 訊息 | 意思 |
|---|---|---|
| 手機 → 主程式 | `hello` | `serial`、`mode` = `CONTROL_IDLE`、`model`、`app_version`、`phone_mode`（`ai`／`normal`）、`in_call`、`supports_sms`、`supports_reject`。沒有 `phone_mode` = 比模式切換舊的 App（永遠是 `ai`）。沒有 `supports_sms` = 不會要它傳簡訊。沒有 `supports_reject` = 拒接號碼改成接起來播語音。 |
| 手機 → 主程式 | `ping` | 每 15 秒一次。 |
| 手機 → 主程式 | `status` | `{mode, in_call}`：模式或通話狀態變了。 |
| 手機 → 主程式 | `incoming`、`incoming_cancel` | 有電話在響（`caller_id`，未顯示號碼時是空字串；`sim_country`；知道時帶 `sim_sub_id`）／接起來之前就結束了。 |
| 主程式 → 手機 | `answer_plan` | `{delay_ms, prewarm, reject}`：響多久再接，或 `reject: true` 響鈴中直接拒接（`reject_mode` 為 `decline` 的拒接號碼）。 |
| 手機 → 主程式 | `call_waiting_rejected` | `{serial, caller_id, sim_country, sim_sub_id}`：通話中又有第二通打進來，手機立刻拒接（AI 模式）。會留下通話紀錄並送出「📵」通知。 |
| 主程式 → 手機／手機 → 主程式 | `set_mode`／`set_mode_result` | `{mode, id}`／`{id, ok, mode, error}`：遠端切換模式；`error` 用上面的代碼。 |
| 主程式 → 手機 | `agent_info` | `{console_url}`：給 App 的「**打開網頁控制台**」按鈕。主程式知道網址時才在 hello 之後送：`public_url`，或控制台對區網開放時的區網 IP 網址。 |
| 主程式 → 手機 | `place_call` | `{number, call_id, task_id, sub_id}`：撥出電話（AI 回電或 AI 代打），用 subscription id 為 `sub_id` 的 SIM 撥（找不到那張 SIM 就用手機預設的）。手機有電話在響或通話中時不撥，回 `place_call_failed`，`reason` 為 `busy`。 |
| 手機 → 主程式 | `place_call_ack`、`place_call_failed`、`outbound_ready`、`call_end` | 撥出的電話已撥號／撥不出去／接通／結束。 |
| 主程式 → 手機 | `send_sms` | `{id, to, text, sub_id}`：傳簡訊給來電者（行程預約）。`id` 1–64 字元；`to` 只有數字，開頭可以有 `+`；`text` 最多 500 字元；`sub_id` 指定 SIM（手機回報的 `sim_sub_id`），沒有就用預設的簡訊 SIM。已經送出過的 id 再來一次只會再回一次結果，不會重複傳。 |
| 手機 → 主程式 | `send_sms_result` | `{id, ok, error}`：每一段都回報後才回（手機最多等 60 秒，逾時回 `timeout`；之後才收到回報會再送一次真正的結果）。`error`：`no_permission`、`bad_request`、`no_service`、`radio_off`、`generic_failure`、`timeout`、`exception`。主程式最多等 90 秒。重連後手機會把最近 20 個 id 的結果再送一次，主程式用 `id` 去掉重複的。 |
| 手機 → 主程式 | `send_sms_delivered` | `{id, ok}`：電信有給送達回報時才送（手機最多等一小時）。 |

### Media 連線

| 方向 | 訊息 | 意思 |
|---|---|---|
| 手機 → 主程式 | `hello` | `serial`、`caller_id`、`sample_rate`（16000）、`mode` = `SILENT_LOCAL`、`sim_country`、`sim_sub_id`、`model`、`app_version`。撥出的電話，主程式會拿 `sim_sub_id` 比對它指定的 SIM（代打不符就是 `wrong_sim`）。 |
| 手機 → 主程式 | `ringing`、`call_end` | 對方號碼（晚一點才知道時會再送）／通話結束。 |
| 主程式 → 手機 | `tts_start`、`tts_end`、`hangup` | 接下來有聲音／聲音結束／掛斷。 |
| 雙向 | 二進位 | 對方的聲音（手機 → 主程式）和 AI 的聲音（主程式 → 手機），格式同上。 |

主程式這邊：簡訊結果在主程式不再等之後才到，照樣會記下來；手機之後用同一個 `id` 送來的真正結果會取代 `timeout`；同樣的結果再來、或已經有確定結果的簡訊收到不同結果，都不會改。拒接號碼只有在 `reject_mode` 是 `decline`、而且手機說過 `supports_reject` 時才叫手機拒接；否則接起來播語音，通知會寫清楚是哪一種。AI 回電和 AI 代打都帶 `sub_id`，雙卡手機會用對的號碼撥出。

