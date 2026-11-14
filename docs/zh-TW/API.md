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
| `GET /calls/api/call/{id}` | 單一通話：`{ok, call, notes, calendar_link}`。`notes`（摘要寫好之前是 null）包含 `summary`、`dialogue`、`end_hint`、`source`、`updated_at`，通話中講定了日期時還有 `calendar` = `{title, start, end, all_day}`（當地時間，`YYYY-MM-DDTHH:MM`）。 |
| `GET /calls/api/call/{id}/event.ics` | 行事曆建議，iCalendar 檔案格式。該通話沒有建議時回 `404`。 |
| `GET /calls/api/audio/{id}/listen` | 你當時會聽到的整通電話（對方和 AI 混在一起），WAV，支援 range 請求。 |
| `GET /calls/api/audio/{id}/remote` · `/tts` · `/audio/{id}` | 只有對方 · 只有 AI · 原始的混音錄音。 |
| `GET /calls/api/live` | 進行中的通話和已連線的手機：`{ok, count, calls, control_idle_clients, control_idle_serials, normal_mode_serials}`。`normal_mode_serials` 是已連線、但切成一般手機的手機；網頁不把它們算進待命手機。 |
| `GET /calls/api/stats` | `{ok, stats: {total, today, voicemails, blocked}}`。 |
| `GET /calls/api/export.csv` | 篩選後的清單，CSV 格式（查詢參數同 `list`，最多 2000 列）。 |

通話紀錄（`call`，以及 `calls` 裡的每一筆）帶有的欄位包括：`id`、`serial`（手機）、`caller_id`、`started_at` / `ended_at`（RFC 3339，UTC）、`duration_ms`、`status`（`ringing`、`active`、`ended`）、`events`（依序發生的事——例如 `active`、`persona_stall_spam`、`reject_list`、`reject_list_declined`、`call_waiting_rejected`、`persona_ai_fail_hangup`、`ended`）、`summary`、有哪些音訊檔、`sim_country` / `sim_sub_id`（這通電話打進來的 SIM，手機 App 有說才有）、`reply_method` / `reply_contact`（也包括對方在電話裡說要回電的號碼），對方跟 AI 約了時間時還有 `appointment`（通話結束當下的樣子，見[行程預約](#行程預約)）；通話後的變化（你的決定、簡訊結果）會更新在 `notes.appointment`。

在 `notes.dialogue` 裡，每一行是一次發言，前面加上主程式語言的說話者標籤和分隔符號：繁體中文是 `對方：`／`接聽：`，簡體中文 `对方：`／`接听：`，英文 `Caller: `／`AI: `，日文 `相手：`／`AI：`，韓文 `상대: `／`AI: `，西班牙文 `Llamante: `／`IA: `，法文 `Appelant : `／`IA : `，德文 `Anrufer: `／`KI: `，葡萄牙文 `Chamador: `／`IA: `。舊版存的紀錄不論語言都是 `對方：`／`接聽：`。

## 設定

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/settings` | `{ok, settings, storage_path, console_url, console_ip_url}`：`console_url` 是目前用的控制台網址（你填的，或自動帶入的；只有這台電腦能開時是空的），`console_ip_url` 是把 `.local` 名稱換成區網 IP 的同一個網址。機密欄位會以遮罩形式回傳（`"abc…wxyz"`），並附一個 `<name>_set` 旗標；通知管道回傳時 `url` / `token` 是空的，另外附上 `url_hint`、`token_hint`、`url_set`、`token_set`。還有幾個唯讀的輔助欄位：`default_prompt`、`voices`、`ambience_kinds`、`languages`、`countries`（「各 SIM 卡的主要語言」可選的國家代碼）、`seen_sim_countries`（主程式啟動後手機回報過的 SIM 卡國家）、`notify_kinds`、`sample_lines`、`summary_ready`。 |
| `PUT /calls/api/settings` | 一個 JSON 物件，只放要修改的欄位。回應同 `GET`。 |

`PUT` 的規則：

- 機密欄位（`gemini_api_key`、`deepseek_api_key`、`gcal_client_secret`、`gcal_refresh_token`）沒帶或是 `""` 時，保留原本儲存的值；`"<name>_clear": true` 則會清空它。
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

`appointment_mode` 設成 `confirm` 或 `auto` 時（見[設定](SETTINGS.md#摘要與行事曆)），對方跟 AI 約好時間的那通電話會留下一筆任務（`intent` 為 `appointment`），通話紀錄、它的 notes 和任務上都有一個 `appointment` 物件：

```json
{
  "start": "2026-11-14T15:00", "end": "2026-11-14T16:00",
  "title": "看房", "caller_name": "陳先生",
  "mode": "confirm", "status": "pending",
  "tag": "6f1c2a9e-3b7d-4e0a-9c55-2d8f1b7e4a10"
}
```

時間是當地時間（`TZ`）。`status`：`pending`（等你決定）、`booked`（通話中已寫進日曆）、`confirmed`、`declined`。`event_id`：寫進 Google 日曆後的活動 ID。`auto_error`：「有空直接答應」沒能排入的原因。`replaced`：同一通電話裡先前已寫進日曆、後來改掉的時間。`sms`：`sent`，或簡訊沒傳出的原因（`offline`、`old_app`、`timeout`、`busy`、`no_number`，或手機回的錯誤代碼）。`tag`：這通電話在它寫進日曆的活動上留的記號（見下句）。`tg_card`：發給這筆預約的 Telegram 卡片 `{chat_id, message_id}`，在別處決定後用來拿掉卡片上的按鈕。`write_error`：上次按確定沒能寫進日曆的原因（預約還在等）；`held_busy`：被擋是因為時段有行程，下一次確定照樣寫入。`sim_country`／`sim_sub_id`：接這通電話的 SIM 卡；沒有國碼的來電號碼照它的國家來讀，簡訊也從它傳。一通電話可以約不只一件事：每件各有自己的任務和 `tag`（這通電話的 tag，接著是 `<tag>-2`…），通話紀錄和筆記放第一件，其他的在 `more`。空的欄位不會出現。主程式寫進日曆的活動都帶私有擴充屬性 `bageo_call=<appointment.tag>`，寫入時沒收到回應的那筆之後找得回來，不會寫兩次。

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/tasks` | 任務，最新的在前（最多 200 筆）：`{ok, tasks}`。網頁的「待確認的預約」就是 `appointment.status` 為 `pending` 的任務。 |
| `POST /calls/api/tasks/{id}/confirm` | 對預約來說等於在網頁或 Telegram 卡片上按 **✅ 確定**——寫進 Google 日曆，接著（開了 `appointment_sms` 時）傳簡訊給對方，可能要等最多 90 秒。回應 `{ok, task, message}`；`message` 是結果，網頁顯示的和 Telegram Bot 回的是同一段。這筆預約有發過 Telegram 卡片時，卡片上的按鈕會拿掉，並把 `message` 回到那個聊天（盡力而為，Telegram 失敗不影響決定）。只有第一次決定算數：之後再確定或婉拒——包括第一次還在處理時送來的，或在 Telegram 卡片上按的——都只回「已經處理過了」，不會有任何變化。寫進日曆前會再查一次那個時段：日曆上已經有行程或寫入失敗時，預約維持 `pending` 並帶 `write_error`，不傳簡訊、Telegram 卡片的按鈕留著，`message` 寫原因，之後可以再確定；因時段有行程被擋（`held_busy`）的，下一次確定就照樣寫入。請求要帶 `Content-Type: application/json`（不用 body），否則回 HTTP 400 `bad_request`，這樣別的網頁沒辦法透過你區網裡的瀏覽器替你決定。 |
| `POST /calls/api/tasks/{id}/ignore` | 對預約來說等於 **❌ 婉拒**（通話中已經寫進日曆的那筆會刪掉；開了 `appointment_sms` 時傳簡訊）。回應與規則同上。 |
| `POST /calls/api/appointments/{id}/retry-sms` | 已確定或已婉拒、但簡訊沒傳出（`sms` 是失敗代碼）的預約，再傳一次簡訊給對方，內容照當初的決定。body 可省略，帶 `{"serial": "<手機>"}` 就改用另一支在線的手機傳；沒帶就用接這通電話的那支。回應 `{ok, sms, message}`：`ok` 表示傳出去了，`sms` 是新的狀態代碼，`message` 是結果。還在等決定、已經傳過、沒傳過簡訊，或號碼收不到簡訊（`no_number`）的：HTTP 409 `sms_not_retryable`，附目前的 `sms`。請求要帶 `Content-Type: application/json`；手機不是英數字、`-`、`_` 組成的，回 400 `invalid_serial`。 |

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
| 主程式 → 手機 | `{"type": "send_sms", "id": "<最多 64 字元>", "to": "+886912345678", "text": "<UTF-8，最多 500 字元>"}` | 只送給接那通電話的手機。`to` 只有數字，開頭可以有 `+`。 |
| 手機 → 主程式 | `{"type": "send_sms_result", "id": "<同一個>", "ok": true}` 或 `{"type": "send_sms_result", "id": "<同一個>", "ok": false, "error": "<代碼>"}` | 簡訊每一段都回報送出後才回（手機最多等 60 秒，逾時回 `timeout`）。`error` 是 `no_permission`, `bad_request`, `no_service`, `radio_off`, `generic_failure`, `timeout`, `exception` 其中之一。主程式最多等 90 秒，沒回應就當作失敗。 |
| 主程式 → 手機 | `send_sms` 加上 `"sub_id": <整數>` | 選填：用哪張 SIM（Android subscription id）傳——就是那通電話打進來的 SIM。沒有就用手機的預設 SIM。 |
| 手機 → 主程式 | `{"type": "send_sms_delivered", "id": "<同一個>", "ok": bool}` | 電信的送達回報（長簡訊每一段都可能回報；只要有一個 `false` 就算沒送達）。 |

主程式不再等之後才到的結果照樣會記下來。手機回的 `timeout` 不是最後結果：之後收到電信回應時，手機會用同一個 `id` 再送真正的結果，取代那個 timeout（還在等的傳送會繼續等它）。手機重新連上後會補送最近的結果：同樣的結果再來不會改變任何東西，已經有確定結果的簡訊收到不同的結果也不會改。

通話用到這些選填欄位（不認得的 App 或主程式照舊運作）：

| 方向 | 訊息 | 意思 |
|---|---|---|
| 手機 → 主程式 | `incoming` 和 media 的 `hello` 加上 `"sim_sub_id": <整數>` | 接這通電話的 SIM（Android subscription id）；這通電話的簡訊和回撥都用它。 |
| 手機 → 主程式 | control 的 `hello` 加上 `"supports_reject": true` | App 能在響鈴時拒接。沒有這個欄位 = 舊 App：不管 `reject_mode` 怎麼設，拒接號碼都會接起來播語音。 |
| 主程式 → 手機 | `answer_plan` 加上 `"reject": true` | 拒接號碼、`reject_mode` 是 `decline`，而且 App 說過 `supports_reject`：在響鈴時拒接。同時帶 `"delay_ms": 0`；App 還是接起來的話，由主程式播語音。 |
| 手機 → 主程式 | `{"type": "call_waiting_rejected", "serial": "…", "caller_id": "…", "sim_country": "us", "sim_sub_id": 20}` | 通話中有第二通來電，手機忙線拒接。主程式會留下通話紀錄並送出通知。 |
| 主程式 → 手機 | `place_call` 加上 `"sub_id": <整數>` | 回撥從同一支手機打出去時，用對方當初打進來的那張 SIM。 |
