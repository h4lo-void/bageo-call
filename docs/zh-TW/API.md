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

## 通話

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/list` | 通話紀錄，最新的在前。查詢參數：`limit`、`offset`、`q`（對號碼、手機、事件、逐字稿做文字搜尋）、`caller`、`serial`、`status`、`has_transcript`。回應：`{ok, calls: [...], total, limit, offset}`。 |
| `GET /calls/api/call/{id}` | 單一通話：`{ok, call, notes, calendar_link}`。`notes`（摘要寫好之前是 null）包含 `summary`、`dialogue`、`end_hint`、`source`、`updated_at`，通話中講定了日期時還有 `calendar` = `{title, start, end, all_day}`（當地時間，`YYYY-MM-DDTHH:MM`）。 |
| `GET /calls/api/call/{id}/event.ics` | 行事曆建議，iCalendar 檔案格式。該通話沒有建議時回 `404`。 |
| `GET /calls/api/audio/{id}/listen` | 你當時會聽到的整通電話（對方和 AI 混在一起），WAV，支援 range 請求。 |
| `GET /calls/api/audio/{id}/remote` · `/tts` · `/audio/{id}` | 只有對方 · 只有 AI · 原始的混音錄音。 |
| `GET /calls/api/live` | 進行中的通話和已連線的手機：`{ok, count, calls, control_idle_clients, control_idle_serials}`。 |
| `GET /calls/api/stats` | `{ok, stats: {total, today, voicemails, blocked}}`。 |
| `GET /calls/api/export.csv` | 篩選後的清單，CSV 格式（查詢參數同 `list`，最多 2000 列）。 |

通話紀錄（`call`，以及 `calls` 裡的每一筆）帶有的欄位包括：`id`、`serial`（手機）、`caller_id`、`started_at` / `ended_at`（RFC 3339，UTC）、`duration_ms`、`status`（`ringing`、`active`、`ended`）、`events`（依序發生的事——例如 `active`、`persona_stall_spam`、`reject_list`、`persona_ai_fail_hangup`、`ended`）、`summary`，以及有哪些音訊檔。

在 `notes.dialogue` 裡，每一行是一次發言，前面加上儲存時用的說話者標籤 `對方：`（來電者）或 `接聽：`（AI）。不論語言是什麼，這兩個標籤都是固定的；顯示時請自行翻譯。

## 設定

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/settings` | `{ok, settings, storage_path}`。機密欄位會以遮罩形式回傳（`"abc…wxyz"`），並附一個 `<name>_set` 旗標；通知管道回傳時 `url` / `token` 是空的，另外附上 `url_hint`、`token_hint`、`url_set`、`token_set`。還有幾個唯讀的輔助欄位：`default_prompt`、`voices`、`ambience_kinds`、`languages`、`notify_kinds`、`sample_lines`、`summary_ready`。 |
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
| `POST /calls/api/notify/test` | Body `{"channel": {kind, url, token, target, id?}}`。把測試訊息傳到那一個管道（已儲存的機密會依 `id` 補上）。回 `200 {ok: true}`，或 `502 {ok: false, error}` 並附上該服務的回應。 |
| `POST /calls/api/notify/send` | Body `{"title": "...", "body": "..."}`。把你的訊息傳到每一個啟用的管道。只要有一個管道接受，就回 `{ok: true}`。 |

你自己的 webhook 會收到的 JSON，在[通知](NOTIFICATIONS.md#webhook-json)裡有說明。

## 語音與背景音試聽

`GET /calls/api/preview` 會回傳一個 WAV。查詢參數：`voice`（聲音名稱，或用 `none` 表示只要背景音）、`ambience`（`off` 或某一種背景音）、`level`（1–3）、`regen=true`（重新產生，不用快取的範例）、`clip=reject`（用該聲音念的拒接號碼語音）。某個聲音的第一次請求會開啟一次短暫的 Gemini 連線。

## 手機與健康狀態

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/devices` | `{ok, usb: {enabled, adb_found, adb_path, error, phones: [{serial, state, model, reverse, app_installed}]}, connected: [...], labels: {<phone id>: <model>}, calls_in_progress, phones: [...]}`。最外層的 `phones` 是主程式啟動後在 control 連線上見過的每支手機：`{serial, connected, mode, supports_mode, model, app_version, in_call, last_seen_ms}`。`mode` 是 `ai`（AI 接聽）或 `normal`（一般手機，見[手機設定](PHONE.md#一般手機模式)）；比這個功能舊的 App，`supports_mode` 是 false，永遠是 `ai`。 |
| `POST /calls/api/devices/{serial}/mode` | Body `{"mode": "ai" \| "normal"}`。叫手機切換，最多等 15 秒等它回覆。一律回 `{ok, mode, error_code}`：`mode` 是切換後手機的模式（不知道時為 `null`），成功時 `error_code` 是 `null`，否則是這些代碼之一：`offline`（沒連上）、`unsupported`（App 太舊）、`in_call`、`timeout`、`busy`（上一次切換還在做）、`no_root`、`save_failed`、`dialer_failed`、`screening_failed`、`dnd_failed`、`volume_failed`、`invalid_mode`、`failed`。Body 的 `mode` 不認得時回 400 與 `invalid_mode`。 |
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
| 主程式 → 手機 | `{"type": "agent_info", "console_url": "http://…/calls/"}` | 緊接在 hello 之後，只有知道控制台網址（`public_url`）時才送。App 會因此顯示「打開網頁控制台」按鈕。 |
