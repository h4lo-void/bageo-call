# API

網頁控制台是單一頁面，背後使用一組小型的 JSON API。你也可以從腳本使用同一組 API。基底位址：主程式監聽的位置，預設是 `http://127.0.0.1:8787`。

## 存取

沒有設定 `CALLS_UI_TOKEN` 時，任何連得到這個連接埠的人都能使用 API（預設只有這台電腦自己）。有設定時，每個請求都要用下列其中一種方式帶上權杖：

```text
GET /calls/api/list?token=<token>
Authorization: Bearer <token>
X-Calls-Token: <token>
```

權杖錯誤或沒帶，會得到 `401`。

錯誤的回應是 `{ok: false, code, error}`：`code` 是固定的英文代碼，可以拿來判斷（`not_found`、`invalid_id`、`no_audio`、`preview_failed`、`save_failed`、`missing_field`、`auth_failed`…；網頁控制台會用你的語言顯示），`error` 是詳細說明（大多是英文）。例外：`POST /calls/api/devices/{serial}/mode` 一律回 `{ok, mode, error_code}`（見下方）。

## 通話

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/list` | 通話紀錄，最新的在前。查詢參數：`limit`、`offset`、`q`（對號碼、手機、事件、逐字稿做文字搜尋）、`caller`、`serial`、`status`、`has_transcript`。回應：`{ok, calls: [...], total, limit, offset}`。 |
| `GET /calls/api/call/{id}` | 單一通話：`{ok, call, notes, calendar_link, calendar_links}`。`notes`（摘要寫好之前是 null）包含 `summary`、`dialogue`、`end_hint`、`source`、`updated_at`，通話中講定了日期時還有 `calendar` = `{title, start, end, all_day}`（當地時間，`YYYY-MM-DDTHH:MM`）。`calendar_links` = `{google, outlook, office365, yahoo}`：各網頁行事曆填好這個活動的「新增活動」表單（`calendar_link` 是 Google 那個，留給舊版用）。 |
| `GET /calls/api/call/{id}/event.ics` | 行事曆建議，iCalendar 檔案格式。該通話沒有建議時回 `404`。用控制台 token 開，或用 `?k=<金鑰>`：通知裡的連結帶的是只能開這個檔案的金鑰，不是 token。 |
| `GET /calls/api/audio/{id}/listen` | 你當時會聽到的整通電話（對方和 AI 混在一起），WAV，支援 range 請求。 |
| `GET /calls/api/audio/{id}/remote` · `/tts` · `/audio/{id}` | 只有對方 · 只有 AI · 原始的混音錄音。 |
| `GET /calls/api/live` | 進行中的通話和已連線的手機：`{ok, count, calls, control_idle_clients, control_idle_serials, normal_mode_serials}`。`normal_mode_serials` 是已連線、但切成一般手機的手機；網頁不把它們算進待命手機。 |
| `GET /calls/api/stats` | `{ok, stats: {total, today, voicemails, blocked}}`。 |
| `GET /calls/api/export.csv` | 篩選後的清單，CSV 格式（查詢參數同 `list`，最多 2000 列）。 |

通話紀錄（`call`，以及 `calls` 裡的每一筆）帶有的欄位包括：`id`、`serial`（手機）、`caller_id`、`started_at` / `ended_at`（RFC 3339，UTC）、`duration_ms`、`status`（`ringing`、`active`、`ended`）、`events`（依序發生的事——例如 `active`、`persona_stall_spam`、`reject_list`、`persona_ai_fail_hangup`、`ended`）、`summary`、有哪些音訊檔，對方跟 AI 約了時間時還有 `appointment`（通話結束當下的樣子，見[行程預約](#行程預約)）；通話後的變化（你的決定、簡訊結果）會更新在 `notes.appointment`。

在 `notes.dialogue` 裡，每一行是一次發言，前面加上主程式語言的說話者標籤和分隔符號：繁體中文是 `對方：`／`接聽：`，簡體中文 `对方：`／`接听：`，英文 `Caller: `／`AI: `，日文 `相手：`／`AI：`，韓文 `상대: `／`AI: `，西班牙文 `Llamante: `／`IA: `，法文 `Appelant : `／`IA : `，德文 `Anrufer: `／`KI: `，葡萄牙文 `Chamador: `／`IA: `。舊版存的紀錄不論語言都是 `對方：`／`接聽：`。

## 設定

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/settings` | `{ok, settings, storage_path, console_url, console_ip_url}`：`console_url` 是目前用的控制台網址（你填的，或自動帶入的；只有這台電腦能開時是空的），`console_ip_url` 是把 `.local` 名稱換成區網 IP 的同一個網址。機密欄位會以遮罩形式回傳（`"abc…wxyz"`），並附一個 `<name>_set` 旗標；通知管道回傳時 `url` / `token` 是空的，另外附上 `url_hint`、`token_hint`、`url_set`、`token_set`。還有幾個唯讀的輔助欄位：`default_prompt`、`voices`、`ambience_kinds`、`languages`、`countries`（「各 SIM 卡的主要語言」可選的國家代碼）、`seen_sim_countries`（主程式啟動後手機回報過的 SIM 卡國家）、`notify_kinds`、`sample_lines`、`summary_ready`。 |
| `PUT /calls/api/settings` | 一個 JSON 物件，只放要修改的欄位。回應同 `GET`。 |

`PUT` 的規則：

- 機密欄位（`gemini_api_key`、`deepseek_api_key`）沒帶或是 `""` 時，保留原本儲存的值；`"<name>_clear": true` 則會清空它。
- `calendar_app` 是 `google`、`outlook`、`office365`、`apple`、`yahoo`、`ics` 或空白（還沒選）；其他值存成空白。`appointment_reply` 是 `auto`、`sms` 或 `call`。舊版存的、這版已經沒有的欄位（Google 日曆憑證、`calendar_auto_add`）讀進來時忽略，下次存檔就不見。
- `notify_channels` 是整個取代。一列 `{id, kind, name, enabled, url, token, target}` 的 `url` 或 `token` 是空的時，會保留 `id` 和 `kind` 相同的那一列原本儲存的值；沒有 `id` 的列是新增的；你沒列出來的列會被刪除。
- `reject_numbers` 是一個 `{number, reason}` 的清單。

範例——切換語言並新增一個拒接號碼：

```text
PUT /calls/api/settings
{"language": "ja", "reject_numbers": [{"number": "0800*", "reason": "telemarketing"}]}
```

欄位名稱就是 [`crates/agent/src/settings_store.rs`](../../crates/agent/src/settings_store.rs) 裡 `RuntimeSettings` 的欄位名稱，每一個在那裡都有說明。

## 通知

| 方法與路徑 | 內容 |
|---|---|
| `POST /calls/api/notify/test` | Body `{"channel": {kind, url, token, target, id?}}`。把測試訊息傳到那一個管道（已儲存的機密會依 `id` 補上）。回 `200 {ok: true}`，或 `502 {ok: false, code, field, kind, error}`：`code` 是出錯的原因（`missing_field`、`bad_url`、`bad_value`、`auth_failed`、`service_not_found`、`http_error`、`service_error`、`network_error`、`send_failed`），`field` 是 `url` / `token` / `target` 哪一欄（或空白），`error` 是該服務的回應。 |
| `POST /calls/api/notify/send` | Body `{"title": "...", "body": "..."}`。把你的訊息傳到每一個啟用的管道。只要有一個管道接受，就回 `{ok: true}`。 |

你自己的 webhook 會收到的 JSON，在[通知](NOTIFICATIONS.md#webhook-json)裡有說明。

## 語音與背景音試聽

`GET /calls/api/preview` 會回傳一個 WAV。查詢參數：`voice`（聲音名稱，或用 `none` 表示只要背景音）、`ambience`（`off` 或某一種背景音）、`level`（1–3）、`regen=true`（重新產生，不用快取的範例）、`clip=reject`（用該聲音念的拒接號碼語音）、`lang`（`ja`、`zh-TW`…；留空 = 存著的語言）。內建的每個聲音在每種語言都已預先錄好（`assets/voices/`），試聽不用連 Gemini；只有 `regen=true` 或沒有預錄的聲音才會開一次短暫的連線。

## 行程預約

`appointment_mode` 設成 `confirm` 時（見[設定](SETTINGS.md#摘要與行事曆)；舊版存的 `auto` 當作 `confirm`），對方跟 AI 約好時間的那通電話會留下一筆任務（`intent` 為 `appointment`），通話紀錄、它的 notes 和任務上都有一個 `appointment` 物件：

```json
{
  "start": "2026-11-14T15:00", "end": "2026-11-14T16:00",
  "title": "看房", "caller_name": "陳先生",
  "mode": "confirm", "status": "pending",
  "tag": "6f1c2a9e-3b7d-4e0a-9c55-2d8f1b7e4a10"
}
```

時間是當地時間（`TZ`）。`status`：`pending`（等你決定）、`booked`（測試號碼的通話在模擬日曆上排入）、`confirmed`、`declined`。`reply`：怎麼告訴對方——`sms`、`call` 或 `none`。`sms`：`sent`，或簡訊沒傳出的原因（`offline`、`old_app`、`timeout`、`busy`、`no_number`、`not_test_number`（測試來電的簡訊只傳給測試號碼），或手機回的代碼：`no_permission`、`bad_request`、`no_service`、`radio_off`、`generic_failure`、`exception`、`no_sim`；網頁和 Telegram 回覆會用白話寫）。`call`：AI 回電——`queued`（等打給同一個號碼的前一通回電結束）、`dialing`（已請手機撥出）、`connected`（手機已撥號，AI 等對方接起來出聲）、`answered`（接通並已告知）、`no_answer`（沒人出聲）、`failed`（手機撥不出去或沒回報）、`done`（通話結束、結果不明），或沒撥出的原因（`no_phone`、`send_failed`、`no_number`、`not_callable`：這個號碼不能回電）；手機的回報也記在任務的 `events`（`callback_place_call_ack`、`callback_place_call_failed`、`callback_outbound_ready`、`callback_done`、`callback_failed`）。`calendar`：測試號碼的預約是 `demo`，它的 `event_id`、`replaced`（同一通電話先前的時間）、`auto_error`、`tag` 都是模擬日曆上的。`tg_card`：發給這筆預約的 Telegram 卡片 `{chat_id, message_id}`，在別處決定後用來拿掉卡片上的按鈕。空的欄位不會出現。

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/tasks` | 任務，最新的在前（最多 200 筆）：`{ok, tasks}`。預約任務另外有 `calendar_links`（`{google, outlook, office365, yahoo}`，同通話）和 `reply` = `{sms, call, default}`：這位來電者能不能收簡訊、能不能回電，以及「預設回覆方式」選的方式。網頁的「待處理事項」就是 `appointment.status` 為 `pending` 的任務。有預約的任務另帶 `sms_route`：沒另選手機時簡訊從哪裡發（網頁「重傳簡訊」選單的預設選項），跟主程式實際發送用同一套規則：`{serial, label, sim, test}`——`serial` 手機 ID，`label` 它在 `device_labels` 的名稱（沒有就是 ID），`sim` 依名稱挑的 SIM（`appointment_test_sms_sim`；用接這通的 SIM 或手機預設 SIM 時不出現），`test` 為 true 表示這是測試來電、手機或 SIM 由 `appointment_test_sms_phone`／`appointment_test_sms_sim` 指定。手機不在線也照樣列出，發送時才會回 `offline`。 |
| `GET /calls/api/tasks/{id}/event.ics` | 預約的 iCalendar 檔（Apple 日曆和其他 App）。跟通話的一樣，用控制台 token 或連結的 `?k=<金鑰>` 開。 |
| `POST /calls/api/tasks/{id}/confirm` | 對預約來說等於在網頁或 Telegram 卡片上按 **同意**。Body：空的、`{}` 或 `{"reply": "sms" \| "call" \| "none"}`，怎麼告訴對方（沒帶：照「預設回覆方式」；選了這位來電者用不了的方式就不通知）。傳簡訊可能要等最多 90 秒；回電交給手機撥出（`appointment.call`）。回應 `{ok, task, message}`；`message` 是用白話寫的結果，Telegram Bot 回的是同一段（附加入你的行事曆的連結）。這筆預約有發過 Telegram 卡片時，卡片上的按鈕會拿掉，並把 `message` 回到那個聊天（盡力而為，Telegram 失敗不影響決定）。只有第一次決定算數：之後再同意或不同意——包括第一次還在處理時送來的，或在 Telegram 卡片上按的——都只回「已經處理過了」，不會有任何變化。請求要帶 `Content-Type: application/json`，否則回 HTTP 400 `bad_request`，這樣別的網頁沒辦法透過你區網裡的瀏覽器替你決定；body 不是上面的格式也回 HTTP 400 `bad_request`。 |
| `POST /calls/api/tasks/{id}/ignore` | 對預約來說等於 **不同意**（用同樣的方式告訴對方，客氣婉拒）。body、回應與規則同上。 |

## 聯絡人

替打來的號碼配上名字，存在通話紀錄資料夾（`CALLS_DATA_DIR`）的 `contacts.json`。號碼不管怎麼寫都對得上：`+886912345678`、`0912-345-678`、`886912345678` 是同一支；沒有國碼的號碼照你的國家（`country_code`）讀，通話則照接到這通電話的 SIM 卡所屬國家讀。打來的人在聯絡人裡時：通話列表顯示名字（`GET /calls/api/list` 的 `contacts_by_number`：`{<caller_id>: {id, name}}`）、通話詳情也有（`GET /calls/api/call/{id}` 的 `contact`：`{id, name, note}` 或 `null`）、通話摘要通知的標題在號碼後面加名字，一般電話的 AI 提示詞也會說是誰打來的（判定推銷／詐騙、AI 陪聊的電話不加）。

一位聯絡人是 `{id, name, phones: [...], note, source, updated_at}`；`source` 是 `manual`、`vcf`、`csv`、`text` 或 `ai`（Gemini 讀出來的）。一支號碼只屬於一位聯絡人。

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/contacts` | 查詢參數 `q`（名字、備註或號碼，號碼怎麼寫都可以）、`limit`、`offset`。回應 `{ok, contacts, total, offset, count}`：依名字排序的符合結果（共 `total` 筆），以及全部聯絡人數（`count`）。 |
| `POST /calls/api/contacts` | Body `{name, phones: [...], note}`，回應 `{ok, contact}`。號碼已經是別的聯絡人的：HTTP 409 `contact_exists`，附 `contact: {id, name}`；沒有名字或沒有號碼：400 `invalid_contact`。 |
| `PUT /calls/api/contacts/{id}` | 同樣的 body，取代名字、號碼和備註。404 `contact_not_found`、409、400 同上。 |
| `DELETE /calls/api/contacts/{id}` | `{ok}`。 |
| `POST /calls/api/contacts/import` | Body 就是檔案本身（最大 10 MB），檔名 URL 編碼後放在 `X-Filename`。只讀、不存：`{ok, file, format, ai, entries: [{name, phones, note, source, duplicate}], skipped}`。`format`：`vcf`、`csv`、`text`、`xlsx`、`docx`、`pdf` 或 `image`；`ai`：是 Gemini 讀的；`duplicate`：`null`，或這筆重複的已存聯絡人 `{id, name, by: "phone" \| "name"}`；`skipped`：沒有名字或號碼而略過的筆數。錯誤：400 `empty_file`、413 `file_too_large`、415 `unsupported_file`、400 `need_gemini_key`、502 `ai_failed`（Gemini 的回覆在 `error`）。 |
| `POST /calls/api/contacts/batch` | Body `{contacts: [{name, phones, note, source}], duplicates: "merge" \| "skip"}`：匯入時勾選要留的那些。跟已存聯絡人重複的，合併進去（補上新號碼和備註）或略過。回應 `{ok, added, merged, skipped, count}`。 |
| `GET /calls/api/contacts/export.vcf` | 全部聯絡人匯出成 vCard 3.0 檔。 |

主程式自己讀的格式：vCard 2.1、3.0、4.0（含 quoted-printable、各種 `CHARSET`）；有表頭（Google 通訊錄、Outlook，或欄名像 *姓名*、*電話*、Name、Phone）或沒有表頭的 CSV、TSV，編碼 UTF-8、UTF-16 或電腦的編碼（Big5、GBK、Shift_JIS、EUC-KR、Windows-1252）；一行一個「名字 號碼」的純文字；`.xlsx`、`.docx` 裡的表格。PDF、圖片和讀不到人的檔案交給 Gemini（`gemini-flash-lite-latest`，用設定裡的 Gemini 金鑰）。跟其他會改資料的 API 一樣，`POST`、`PUT`、`batch` 要帶 `Content-Type: application/json`，`import` 要帶 `X-Filename`，這樣別的網頁沒辦法透過你區網裡的瀏覽器送這些請求。

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
| `POST /calls/api/devices/{serial}/mode` | Body `{"mode": "ai" \| "normal"}`。叫手機切換，最多等 15 秒等它回覆。一律回 `{ok, mode, error_code}`：`mode` 是切換後手機的模式（不知道時為 `null`），成功時 `error_code` 是 `null`，否則是這些代碼之一：`offline`（沒連上）、`unsupported`（App 太舊）、`in_call`、`busy`（上一次切換還在做）、`no_root`（手機上的 Bageo 模組沒有在執行）、`save_failed`、`dialer_failed`、`screening_failed`、`dnd_failed`、`volume_failed`、`failed`，或 `timeout`。`timeout` 的意思是「還沒確認」，不是失敗：切換本身可能讓手機 App 重新啟動，它重新連上後才回報結果，所以稍後再讀一次模式（`GET /calls/api/devices`）。Body 由主程式自己解析：不是 JSON、沒有字串 `mode`、或 serial 不是英數字、`-`、`_` 組成時，回 HTTP 400、同樣格式的 JSON，`error_code` 是 `bad_request`；`mode` 不認得時回 400 與 `invalid_mode`。請求一定要帶 `Content-Type: application/json`（沒有也回 400 `bad_request`），這樣別的網頁沒辦法透過你區網裡的瀏覽器偷偷切換手機。 |
| `GET /health` | 一個很大的扁平 JSON 物件，用於排查問題：版本、語言、哪些東西已設定、已連線的手機、進行中的通話；不含金鑰或 token。`control_idle_clients`／`control_idle_serials` 算的是所有連上的手機；`normal_mode_serials` 列出切成一般手機、AI 不接它們電話的那幾支。有設定控制台權杖而請求沒帶時，只會回傳 `ok`、`version`、`language`、`active_call_count` 和 `control_idle_clients`（監控用已經足夠）。 |

## 手機這一端

手機透過 WebSocket 連到 `/` 或 `/ws`（兩者相同）：每支手機一條長期維持的「control」連線，每通接聽的電話一條「media」連線，雙向傳送 JSON 文字訊框和原始的 16 kHz 單聲道 16 位元 PCM 二進位訊框。從這台電腦本身來的連線（USB 通道）一律接受；其他連線在有設定該權杖時，必須帶上 `?token=<DEVICE_TOKEN>`。

訊息列表在 [`android/README.md`](../../android/README.md#websocket-protocol)（英文）。任何會講這個通訊協定的東西都可以代替手機——所以要加上 SIP 或軟體電話前端，可以完全不動通話邏輯。

手機模式用到 control 連線上的這些訊息（雙方都會忽略不認得的東西，所以舊 App、舊主程式照樣能用）：

| 方向 | 訊息 | 意思 |
|---|---|---|
| 手機 → 主程式 | `hello` 加上 `phone_mode`（`ai`／`normal`）和 `in_call` | 連上時手機的模式。`mode` 仍然是 `CONTROL_IDLE`（連線種類）。沒有 `phone_mode` = 舊 App：`ai`，不能從這裡切換。 |
| 手機 → 主程式 | `{"type": "status", "mode": "ai" \| "normal", "in_call": bool}` | 模式或通話狀態變了。 |
| 主程式 → 手機 | `{"type": "set_mode", "mode": "ai" \| "normal", "id": "<uuid>"}` | 切換（由上面的路由送出）。 |
| 手機 → 主程式 | `{"type": "set_mode_result", "id": "<同一個>", "ok": bool, "mode": "...", "error": "<代碼>"}` | 結果；成功時 `error` 是空字串，代碼同上。 |
| 主程式 → 手機 | `{"type": "agent_info", "console_url": "http://…/calls/"}` | 緊接在 hello 之後，主程式有控制台網址時才送：你填的網址（`public_url`）；沒填而控制台對區網開放時，用區網 IP 的網址（有些手機打不開 `.local` 名稱）。App 會因此顯示「打開網頁控制台」按鈕。 |

傳給來電者的簡訊（行程預約）也走同一條連線：

| 方向 | 訊息 | 意思 |
|---|---|---|
| 手機 → 主程式 | `hello` 加上 `"supports_sms": true` | App 能替主程式傳簡訊。沒有這個欄位 = 舊 App：不會要它傳簡訊。 |
| 手機 → 主程式 | `hello` 加上 `"supports_sim_select": true` | App 認得 `send_sms` 的 `sim`。沒有這個欄位 = 舊 App：要從指定 SIM 傳的簡訊不會交給它。 |
| 主程式 → 手機 | `{"type": "send_sms", "id": "<最多 64 字元>", "to": "+886912345678", "text": "<UTF-8，最多 500 字元>"}`，可再加 `"sub_id": <整數>` 或 `"sim": "<電信名稱或號碼>"` | 送給接那通電話的手機；測試來電則送給設定指定的手機。`to` 只有數字，開頭可以有 `+`。`sub_id` 指定用哪張 SIM 傳（就是手機回報的 `sim_sub_id`）；沒有就用手機預設的簡訊 SIM。`sim`（設定 `appointment_test_sms_sim`，不會跟 `sub_id` 一起出現）找電信名稱或顯示名稱等於或包含它（不分大小寫）的 SIM，它有 7 碼以上數字時也比對 SIM 自己號碼的末幾碼；剛好一張符合才傳，否則不傳、回 `no_sim`。已經送出過的 id 再來一次，手機只會再回一次結果，不會重複傳。 |
| 手機 → 主程式 | `{"type": "send_sms_result", "id": "<同一個>", "ok": true}` 或 `{"type": "send_sms_result", "id": "<同一個>", "ok": false, "error": "<代碼>"}` | 簡訊每一段都回報送出後才回（手機最多等 60 秒，逾時回 `timeout`；之後才收到回報，會再送一次真正的結果）。`error` 是 `no_permission`, `bad_request`, `no_service`, `radio_off`, `generic_failure`, `timeout`, `exception`, `no_sim`（沒有或不只一張 SIM 符合 `sim`）其中之一。主程式最多等 90 秒，沒回應就當作失敗。control 連線重連後，手機會把最近 20 個 id 的結果再送一次，主程式用 `id` 去掉重複的。 |
| 手機 → 主程式 | `{"type": "send_sms_delivered", "id": "<同一個>", "ok": true \| false}` | 電信回報的送達結果：每一段都送到對方是 `true`，有一段確定送不到是 `false`。電信有回報才會送（手機最多等一小時）；重連後也會跟結果一起再送一次。 |

通話也用同一條連線，多了這些可省略的欄位和訊息（舊 App、舊主程式不送也不認得，照樣能用）：

| 方向 | 訊息 | 意思 |
|---|---|---|
| 手機 → 主程式 | `hello` 加上 `"supports_reject": true` | App 認得 `answer_plan` 的 `reject`，會在響鈴時拒接。沒有這個欄位 = 舊 App，叫它拒接它還是會接起來，所以主程式改成接起來播語音。 |
| 手機 → 主程式 | `incoming` 和 media 連線的 `hello` 加上 `"sim_sub_id": <整數>` | 這通電話打進來的那張 SIM 在 Android 的訂閱 id，跟 `sim_country` 一起送。不知道就不送。隱藏號碼時 `caller_id` 是空字串（不會是上一通的號碼）。 |
| 主程式 → 手機 | `answer_plan` 加上 `"reject": true` | 響鈴中直接拒接，不接起來。電話結束後照樣送 `incoming_cancel`。晚到也有效，只要電話還在響。 |
| 手機 → 主程式 | `{"type": "call_waiting_rejected", "serial": "...", "caller_id": "...", "sim_country": "tw", "sim_sub_id": <整數>}` | 通話中又有第二通打進來，手機立刻拒接（AI 模式）。欄位是被拒那一通的資料；不知道 `sim_sub_id` 就不送。 |
| 主程式 → 手機 | `place_call` 加上 `"sub_id": <整數>` | 用那張 SIM 撥出。找不到那張 SIM 就用手機預設的。手機有電話在響或通話中時不撥，回 `place_call_failed`，`"reason": "busy"`。 |

主程式這邊：主程式不再等之後才到的結果照樣會記下來；手機之後用同一個 `id` 送來的真正結果會取代 `timeout`（還在等的傳送會繼續等它），同樣的結果再來、或已經有確定結果的簡訊收到不同結果，都不會改。拒接號碼只有在 `reject_mode` 是 `decline`、而且手機說過 `supports_reject` 時才叫手機拒接；否則接起來播語音，通知會寫清楚是哪一種。收到 `call_waiting_rejected` 會留下通話紀錄並送出通知。回撥從接原本那通電話的手機打出去時才帶 `sub_id`。
