# API 參考：用程式控制你的 AI 電話助理

Bageo 網頁控制台做的每件事，都是透過你自己電腦上的一個小型 JSON API 完成的，所以寫個程式也能做到：

- 讀通話紀錄、逐字稿和摘要，改設定。
- 同意或婉拒來電者跟 AI 約的時間，管理聯絡人。
- 把手機在「AI 接聽」和一般手機之間切換。
- 叫 AI 替你打電話。

這一頁涵蓋 Bageo 1.6 控制台和手機會用到的每一條路由，也包括已 root 的 Android 手機用的 WebSocket 通訊協定。

| | |
|---|---|
| **基底位址** | Bageo 監聽的位置：預設在區網監聽 `8787` 連接埠（可用時也會監聽 `80` 連接埠供 `http://bageo.local/` 使用），設為 `AGENT_HOST=127.0.0.1` 時則只在 `http://127.0.0.1:8787`。API 路由都以 `/calls/api/` 開頭，例外是 `/health` 和手機用的 WebSocket。控制台本身在 `/calls/`（`/` 會轉址過去）。 |
| **AI 代理** | `POST /mcp` 是 Model Context Protocol 伺服器，有自己的 Token，預設關閉：見 [MCP](MCP.md)。 |
| **格式** | UTF-8 的 JSON；路由另有說明的除外（WAV、CSV、vCard、iCalendar）。送 JSON body 時請帶 `Content-Type: application/json`。 |
| **時間** | 通話紀錄：UTC 的 RFC 3339（`2026-11-14T07:02:11Z`）。行程預約：當地時間（`TZ`），格式 `YYYY-MM-DDTHH:MM`。AI 代打：帶時差的當地時間（`2026-11-15T19:00:00+08:00`）。 |
| **相容性** | 欄位名稱來自 [`crates/agent/src`](../../crates/agent/src) 裡的 Rust 結構。之後的版本可能加新欄位，不認得的欄位請直接忽略。 |

## 先試試看

Bageo 在這台電腦上跑著時，下面幾行可以直接用：

```bash
curl -s http://127.0.0.1:8787/health
curl -s "http://127.0.0.1:8787/calls/api/list?limit=5"
curl -s http://127.0.0.1:8787/calls/api/stats
```

有設定控制台密碼（`CALLS_UI_TOKEN`）的話，加上 `-H "Authorization: Bearer $CALLS_UI_TOKEN"`（見[存取](#存取)）。大家常拿它來做這些事：

| 你想要 | 用這個 |
|---|---|
| 把新的逐字稿匯進筆記或客戶管理系統 | `GET /calls/api/list?has_transcript=true`，再對每一筆 `GET /calls/api/call/{id}` |
| 用程式打電話訂位 | `POST /calls/api/errands`，輪詢 `…/errands/{id}/live`，用 `…/say` 回答 AI |
| 用你自己的按鈕同意預約 | `POST /calls/api/tasks/{id}/confirm` |
| 在家時讓手機照常響、自己接 | `POST /calls/api/devices/{serial}/mode`，body `{"mode": "normal"}` |
| 把任何訊息轉到你的通知管道 | `POST /calls/api/notify/send` |
| 監控 Bageo | `GET /health`（短版狀態不用密碼也能讀） |

## 存取

預設（未設定 `AGENT_HOST`）會對區網監聽：本機在還沒設定密碼前就能呼叫，其他裝置連進來時則必須帶控制台密碼（`CALLS_UI_TOKEN` 或設定中儲存的密碼），並且會檢查 `Host` 標頭與限制密碼錯誤次數。有設定密碼時，每個請求都要用下列其中一種方式帶上密碼：

```text
GET /calls/api/list?token=<token>
Authorization: Bearer <token>
X-Calls-Token: <token>
```

密碼錯誤或沒帶，會得到 HTTP 401，內容是 `{"error": "unauthorized", "code": "unauthorized", "hint": …}`。`/health` 沒帶密碼也會回應，但只給監控用的幾個欄位（見[手機與健康狀態](#手機與健康狀態)）。完整的 `/health` 資料也要帶密碼；控制台用 `GET /calls/api/health` 讀。

```bash
curl -s -H "Authorization: Bearer $CALLS_UI_TOKEN" \
  "http://127.0.0.1:8787/calls/api/list?limit=5"
```

## 錯誤與狀態碼

錯誤的回應是 `{ok: false, code, error}`。程式請看 `code`：它是固定的英文代碼，網頁控制台會用你的語言顯示；`error` 是詳細說明，大多是英文。有兩組路由用自己的格式：手機模式切換回 `{ok, mode, error_code}`；AI 代打的路由在 `error` 和 `code` 放同一個代碼。

| HTTP | 什麼時候 | 常見的 `code` |
|---|---|---|
| 200 | 完成。 | — |
| 400 | 請求有誤：不是 JSON、沒帶 `Content-Type: application/json`、id 或值不對。 | `bad_request`、`invalid_id`、`missing_field`、`invalid_contact`、AI 代打的代碼 |
| 401 | 沒帶密碼或密碼錯誤。 | `unauthorized` |
| 404 | 找不到這通電話、待處理事項、聯絡人、代打，或找不到檔案。 | `not_found`、`task_not_found`、`contact_not_found`、`no_audio`、`no_calendar` |
| 409 | 這個變更跟已存的資料衝突。 | `contact_exists`、`sms_not_retryable`、`call_in_progress`、`device_online`、`device_in_call` |
| 413／415 | 匯入的檔案太大／格式不認得。 | `file_too_large`、`too_many_rows`、`unsupported_file` |
| 500 | Bageo 存不了或讀不了自己的檔案。 | `save_failed`、`contacts_save_failed`、`import_failed` |
| 502 | 外部服務失敗（通知管道、Gemini）。 | `auth_failed`、`network_error`、`ai_failed`、`preview_failed` |

### 防止其他網頁冒用

控制台沒有登入 cookie，但你區網裡某台電腦的瀏覽器開著別的網頁時，那個網頁還是可能偷偷送請求過來。

所以凡是會撥電話、傳簡訊、做決定或改動已存資料的路由，一律要求 `Content-Type: application/json`，沒帶就回 HTTP 400 `bad_request`。別的網站要送這個標頭必須先經過 CORS 預檢，而 Bageo 從不回應預檢。適用的路由：

- `POST /calls/api/initial-password` 和 `POST /calls/api/phone-pairing/renew`；
- `POST /calls/api/call/{id}/scam-feedback`；
- `POST /calls/api/tasks/{id}/confirm` 和 `…/ignore`；
- `POST /calls/api/appointments/{id}/retry-sms`；
- `POST /calls/api/calls/clear`；
- `POST /calls/api/sms/send`、`…/retry`、`…/clear` 和 `DELETE /calls/api/sms/thread`；
- `POST /calls/api/devices/{serial}/mode` 和 `PUT /calls/api/devices/{serial}/sim-number`；
- `POST /calls/api/contacts`、`PUT /calls/api/contacts/{id}`、`POST /calls/api/contacts/batch`、`POST /calls/api/contacts/block`（`POST /calls/api/contacts/import` 則改成要帶 `X-Filename` 標頭）；
- `POST /calls/api/errands`、`…/{id}/cancel`、`…/{id}/retry`、`…/{id}/say`；
- `POST /calls/api/backup/run`、`…/export`、`…/file-ticket`、`…/restore`、`…/delete`、`…/unlock`、`…/apply`、`…/discard`、`…/open-folder`（`POST /calls/api/backup/import` 則改成要帶 `X-Filename` 標頭）。

允許空 body 的路由，空 body 當成 `{}`。這是第二道防線；第一道是控制台對區網開放時一定要設密碼（`CALLS_UI_TOKEN`）。

## 通話

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/list` | 通話紀錄，最新的在前。查詢參數和回應格式見下方。 |
| `GET /calls/api/call/{id}` | 單一通話：`{ok, call, notes, contact, calendar_link, calendar_links, scam_names}`。欄位說明見下方。 |
| `DELETE /calls/api/call/{id}` | 刪除一筆已結束的通話紀錄、錄音 WAV、`_notes.json` 和相關待處理事項：`{ok, id, tasks_deleted}`。還在響鈴或通話中回 HTTP 409 `call_in_progress`。 |
| `POST /calls/api/calls/clear` | 刪除所有已結束的通話紀錄、錄音 WAV、`_notes.json` 和相關待處理事項（進行中的通話會保留）：`{ok, deleted, tasks_deleted}`。 |
| `POST /calls/api/call/{id}/scam-feedback` | 你對這通的標記：JSON `{"verdict": "scam" \| "sales" \| "safe"}`，可另帶內建的 `typology`。記在紀錄上（`scam_feedback`），號碼沒隱藏的話也記在這個號碼上，它之後打來會參考。回 `{ok, call, number}`；HTTP 400 `invalid_verdict`／`invalid_typology`。 |
| `GET /calls/api/call/{id}/event.ics` | 行事曆建議的 iCalendar 檔；這通電話沒有建議時回 HTTP 404 `no_calendar`。用控制台密碼開，或用 `?k=<金鑰>`：通知連結裡帶的是只能開這一個檔案的 HMAC 金鑰。 |
| `GET /calls/api/audio/{id}/listen` | 你當時會聽到的整通電話（對方和 AI 混在一起），WAV，支援 range 請求，可以拖動播放位置（加 `?download=1` 會附上 `Content-Disposition: attachment`）。 |
| `GET /calls/api/audio/{id}/remote` · `/tts` · `/vm` · `/audio/{id}` | 只有對方 · 只有 AI · 舊版選單模式的語音留言 · 原始的混音錄音。檔案不存在時回 HTTP 404 `no_audio`。 |
| `GET /calls/api/live` | 進行中的通話和已連線的手機：`{ok, count, calls, control_idle_clients, control_idle_serials, normal_mode_serials}`。 |
| `GET /calls/api/stats` | `{ok, stats: {total, today, voicemails, blocked}}`。 |
| `GET /calls/api/export.csv` | 篩選後的清單，CSV 格式：查詢參數同 `list`，最多 2000 列（`limit` 可以再調低）。標頭 `X-Calls-Export-Total` 和 `X-Calls-Export-Rows` 分別是符合幾筆、寫出幾筆。 |

`GET /calls/api/list` 的內容：

- 查詢參數：`limit`（最多 500；不帶就回全部）、`offset`、`q`（對號碼、手機、事件、逐字稿和摘要做文字搜尋）、`caller`、`serial`、`status`、`has_transcript`、`has_voicemail`。
- 回應：`{ok, calls, total, limit, offset, tasks_by_call, contacts_by_number}`。
- `tasks_by_call` 是 `{<通話 id>: {id, status, summary}}`：這一頁每通電話最新的一筆待處理事項。
- `contacts_by_number` 是 `{<caller_id>: {id, name}}`：這一頁裡是聯絡人的來電者。

`GET /calls/api/call/{id}` 的內容：

- `contact` 是 `{id, name, note}` 或 `null`。
- `calendar_links` = `{google, outlook, office365, yahoo}`：各網頁行事曆已填好這通電話行事曆建議的「新增活動」表單。
- `calendar_link` 是 Google 那個，留給舊版用。
- `scam_names`：判斷出的話術類型在控制台各語言的名稱，沒有就是 `null`。

`GET /calls/api/live`：`normal_mode_serials` 是已連線、但切成一般手機的手機；網頁不把它們算進待命手機。

### 通話紀錄

每筆通話紀錄（`call`，以及 `calls` 裡的每一筆）帶有的欄位包括：

| 欄位 | 意思 |
|---|---|
| `id` | 紀錄 id，`/call/{id}` 和 `/audio/{id}` 路由都用它。 |
| `serial` | 接這通（或撥出這通）的手機。 |
| `caller_id` | 手機回報的對方號碼；未顯示號碼時是空字串。 |
| `started_at`／`ended_at` | RFC 3339，UTC。 |
| `duration_ms` | 通話長度。 |
| `status` | `ringing`、`active`、`ended`。舊版的紀錄可能還有 `blocked_spam`。 |
| `events` | 依序發生的事，例如 `active`、`ended`。事件名稱和意思見下方。 |
| `scam` | 判定是推銷或詐騙時才有：`{kind, typology, confidence, source, released}`。各欄位見下方。 |
| `scam_feedback` | 你在通話頁的標記：`scam`、`sales` 或 `safe`。 |
| `summary` | 寫好後的摘要文字。 |
| `appointment` | 對方跟 AI 約了時間時：通話結束當下的預約（見[行程預約與待處理事項](#行程預約與任務)）。之後的變化（你的決定、簡訊和回電結果）更新在 `notes.appointment`。 |
| `sim_country`、`sim_sub_id` | 這通電話是哪張 SIM 卡接的（手機有回報時）。 |
| `audio_path`、`remote_audio_path`、`tts_audio_path`、`voicemail_path` | 有哪些錄音檔（資料夾裡的路徑）。 |

`events` 的事件名稱：

- `persona_stall_spam`：判定是推銷或詐騙。
- `persona_scam_detected`：陪聊關閉時判定是推銷或詐騙。
- `persona_stall_released`：之後確認是正常電話、改回正常應對。
- `reject_list`／`reject_list_declined`：黑名單上的號碼（播語音／直接拒接）。
- `call_waiting_rejected`：通話中拒接的第二通。
- `ivr_hangup`：自動語音選單／機器人來電提早掛斷。
- `whitelist_urgent_transfer`：白名單急事來電轉接給你。
- `ai_callback`：行程預約的 AI 回電。
- `ai_errand`：AI 代打。
- 其他事件：`active`、`persona_ai_fail_hangup`、`ended`。

`scam` 的欄位：

- `kind`：`scam` 或 `sales`。
- `typology`：內建的話術類型。
- `source`：`review`（複審判的）、`hard_words`（複審沒回答，但對方講了只有詐騙才會講的話）或 `keywords`（複審一直失敗，只靠關鍵字）。
- `released`：之後再看一次，放回正常應對。

### 摘要、逐字稿與翻譯

摘要寫好之前 `notes` 是 `null`，之後包含：

| 欄位 | 意思 |
|---|---|
| `summary` | 用通知語言寫的摘要。 |
| `dialogue` | 逐字稿，一行一次發言，開頭是說話者標籤。標籤用 Bageo 設定的語言，對照見下方；每一行保留當時講的語言。 |
| `dialogue_translations` | 有翻譯時才出現：跟 `dialogue` 一行對一筆（同樣的索引）。別種語言講的那一行，這裡是翻成通知語言的譯文；本來就是你的語言、太短或沒翻的是 `null`。網頁控制台把譯文顯示在原文下面。 |
| `recording_text` | 對方聲音另外做的語音轉文字，當備份（AI 陪聊拖時間的推銷、詐騙電話是空的）。 |
| `end_hint`、`source`、`updated_at` | 通話怎麼結束、誰寫的、什麼時候寫的。 |
| `calendar` | 通話中講定了日期時：`{title, start, end, all_day}`（當地時間，`YYYY-MM-DDTHH:MM`）。 |
| `appointment` | 預約目前的狀態。 |

`dialogue` 的說話者標籤（依 Bageo 的語言）：

- 繁體中文：`對方：`／`接聽：`
- 簡體中文：`对方：`／`接听：`
- 英文：`Caller: `／`AI: `
- 日文：`相手：`／`AI：`
- 韓文：`상대: `／`AI: `
- 西班牙文：`Llamante: `／`IA: `
- 法文：`Appelant : `／`IA : `
- 德文：`Anrufer: `／`KI: `
- 葡萄牙文：`Chamador: `／`IA: `

舊版存的紀錄不論語言都是 `對方：`／`接聽：`。

```json
{
  "dialogue": "對方：Hello, is Alex there?\n接聽：Yes, speaking. Who's calling?",
  "dialogue_translations": ["你好，Alex 在嗎？", "我就是，請問哪位？"]
}
```

打來的人在你的聯絡人裡時，通話列表會顯示名字（`contacts_by_number`），通話詳情也有（`contact`），摘要通知的標題會在號碼後面加上名字，一般電話的 AI 提示詞也會知道是誰打來的。

## 設定

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/settings` | `{ok, settings, storage_path, console_url, console_ip_url}`。欄位說明見下方。 |
| `PUT /calls/api/settings` | 一個 JSON 物件，只放要改的欄位。回應同 `GET`。下一通電話就生效，不用重新啟動。 |

`GET /calls/api/settings` 回應裡的欄位：

- `console_url`：目前用的控制台網址（你填的，或自動帶入的）；只有這台電腦能開時是空的。
- `console_ip_url`：把 `.local` 名稱換成區網 IP 的同一個網址。
- 機密欄位以遮罩回傳（`"abc…wxyz"`），附一個 `<name>_set` 旗標。
- 通知管道回傳時 `url`／`token` 是空的，另外附 `url_hint`、`token_hint`、`url_set`、`token_set`。
- 唯讀的輔助欄位：`default_prompt`、`voices`、`ambience_kinds`、`languages`、`countries`（「各 SIM 卡的主要語言」可選的國家代碼）、`seen_sim_countries`（Bageo 啟動後手機回報過的 SIM 卡國家）、`notify_kinds`、`sample_lines`、`summary_ready`。

值得知道的欄位（全部都在 [`settings_store.rs`](../../crates/agent/src/settings_store.rs) 的 `RuntimeSettings` 和[設定參考](SETTINGS.md)裡）：

| 欄位 | 值 |
|---|---|
| `owner_name` | 你的名字，用在 `{owner}`。 |
| `owner_aliases` | 別人怎麼叫你，最多 10 個，每個最多 40 字。空白的、重複的和跟 `owner_name` 相同的會被拿掉。 |
| `answer_style` | `assistant`／`persona`（AI 以你本人身分接）。 |
| `gemini_assistant_prompt`／`gemini_persona_prompt` | `assistant`／`persona` 各自的自訂提示詞（留空 = 內建）。 |
| `errand_identity` | `assistant`（預設）／`owner`：AI 代打時說自己是誰。其他值存成 `assistant`。 |
| `summary_provider` | `gemini`（預設）／`custom`。`custom` 用 `summary_api_base`、`summary_model`（填了網址就必填）和 `summary_api_key`（那家服務的金鑰）。舊版的值和舊的設定名稱會在設定載入時轉換一次。 |
| `calendar_app` | `google`、`outlook`、`office365`、`apple`、`yahoo`、`ics` 或空白（還沒選）。其他值存成空白。 |
| `appointment_mode` | `off`（預設）／`confirm`。舊的 `auto` 當作 `confirm`。 |
| `appointment_sms` | `true`／`false`：要不要傳簡訊告訴對方你的決定。 |
| `appointment_reply` | `auto`（預設）、`sms` 或 `call`。 |
| `reject_numbers` | 舊版的黑名單寫法，`PUT` 仍然收：`{number, reason}` 的清單。號碼會加進黑名單（一般聯絡人改成黑名單；白名單、陪聊名單上的維持原樣），不存進設定。讀回來的是舊版存的清單，`reject_numbers_moved` 為 `true` 後就不再生效。 |
| `reject_mode` | 黑名單來電怎麼拒接：`decline`（預設：響鈴時直接拒接）或 `announce`（接起來播語音）。 |
| `whitelist_mode` | `off`（預設：照常由 AI 接聽）、`direct`（直接掛斷觸發電信忙線轉接並推播通知）或 `emergency`（AI 接聽並於對方有急事時告知 `whitelist_forward_number` 並發送通知）。只對 `transfer_whitelist`、`transfer_contacts`、`transfer_others` 允許的來電生效。 |
| `whitelist_forward_number` | 直接轉接或有急事轉接時使用的真實電話號碼。 |
| `transfer_whitelist`、`transfer_contacts`、`transfer_others` | 哪些人可以轉：白名單（預設 `true`）、其他聯絡人（預設 `false`）、不在聯絡人裡的號碼，包含未顯示號碼（預設 `false`）。黑名單和陪聊名單永遠不轉。不在聯絡人裡的號碼 30 分鐘內最多撥給主人一次；判定是推銷或詐騙的來電不發急事通知、不撥號。 |
| `ui_password` | 外部存取網頁控制台與 API 的密碼（讀取時遮罩；`"ui_password_clear": true` 可清空）。 |
| `country_code` | 只有數字（`886`、`1`）。控制台不顯示；這裡有值時優先於 `COUNTRY_CODE`。 |
| `gemini_thinking_level` | `low`（預設）／`medium`／`high`，只對 thinking 模型有效。 |

`PUT` 的規則：

- 機密欄位（`gemini_api_key`、`summary_api_key`）沒帶或是 `""` 時，保留原本存的值；`"<name>_clear": true` 會清空它。
- `notify_channels` 是整個取代。一列 `{id, kind, name, enabled, url, token, target}` 的 `url` 或 `token` 是空的時，保留 `id` 和 `kind` 相同那一列原本存的值；沒有 `id` 的列是新增的；沒列出來的列會被刪除。
- 舊版存的、這版已經沒有的欄位，讀進來時忽略，下次存檔就不見：Google 日曆憑證（`gcal_*`）、`calendar_auto_add`、`intl_carrier_prompt`，以及其他在 1.0 移除的設定。
- 寫入磁碟失敗時回 HTTP 500 `save_failed`。

```bash
curl -s -X PUT "http://127.0.0.1:8787/calls/api/settings" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"language": "ja", "owner_aliases": ["Alex", "小明"], "errand_identity": "owner",
       "reject_numbers": [{"number": "0800*", "reason": "電話行銷"}]}'
```

## 通知

| 方法與路徑 | 內容 |
|---|---|
| `POST /calls/api/notify/test` | 把測試訊息傳到那一個管道（已存的機密依 `id` 補上）。Body `{"channel": {kind, url, token, target, id?}}`。回應見下方。 |
| `POST /calls/api/notify/send` | Body `{"title": "...", "body": "..."}`。把你的訊息傳到每一個啟用的管道。可另外帶 `"kind": "sms"` 與簡訊的欄位：Bageo 會把簡訊存進控制台的「簡訊」頁，桌面狀態小程式也用它顯示簡訊通知。規則見下方。 |
| `GET /calls/api/notify/recent?after=<id>` | 由舊到新列出傳過 `notify/send` 的訊息：`{ok, last_id, events: [{id, at_ms, kind, title, from, text}]}`。`events` 只放 `id` 大於 `after` 的；沒帶 `after` 或不是數字時 `events` 是空的，`last_id` 讓用戶端知道從哪裡開始。只留最近 50 筆，只放在記憶體，重新啟動就清空。 |
| `GET /calls/api/sms?q=&before=<id>&limit=<n>` | 由新到舊列出存下來的簡訊：`{ok, total, retention_days, has_more, messages: [{id, at_ms, dir, from, from_name, to, text, serial, device, status}]}`。`q` 搜尋寄件人、聯絡人名稱與內容；`before` 只列比這個 `id` 舊的；`limit` 是 1 到 200（預設 50）。`total` 是全部簡訊的數目，不受搜尋影響。超過「紀錄保留幾天」的簡訊會刪掉，最多留 5000 則。 |
| `DELETE /calls/api/sms/{id}` | 刪除一則簡訊：`{ok: true}`，找不到回 HTTP 404 `not_found`。 |
| `POST /calls/api/sms/clear` | 刪除全部簡訊：`{ok: true, removed: N}`。 |
| `GET /calls/api/sms/threads?q=` | 由新到舊列出對話，一個號碼一則：`{ok, total, retention_days, threads: [{key, number, name, last_at, last_text, last_dir, last_status, count, last_in_at, can_reply}], lines: [{serial, sub_id, device, number, carrier, slot, can_send}]}`。`0912…` 與 `+886912…` 算同一則對話（號碼照收到它的那張 SIM 卡的國家判讀）。`q` 搜尋名稱、號碼與每一則內容。寄件人是「BankInfo」這種名稱時 `can_reply` 是 false。`lines` 是可以用來傳簡訊的 SIM 卡；`can_send` 表示那支手機連著、App 也能傳簡訊。 |
| `GET /calls/api/sms/thread?number=<號碼或 key>&before=<id>&limit=<n>` | 由舊到新列出一則對話：`{ok, key, number, name, can_reply, has_more, reply, messages: [{id, at_ms, dir, from, to, text, serial, sub_id, device, status, error}]}`。`dir` 是 `in` 或 `out`；傳出的簡訊 `status` 是 `sending`、`sent`、`failed`（`error` 是手機回的代碼）或 `timeout`。`reply`（`{serial, sub_id}` 或 `null`）是回覆預設用的門號：收到最近一則簡訊的那支手機與 SIM 卡，不能用時改用第一個能傳的。還沒有對話的號碼回空的對話。 |
| `POST /calls/api/sms/send` | Body `{"to", "text", "serial", "sub_id"}`（`lines` 其中一條；`text` 1 到 500 個字）。叫那支手機傳出，最多等 30 秒它的回覆：`{ok, message}`，送出才是 `ok`。HTTP 400 `bad_number`、`bad_line` 或 `bad_text`。這則簡訊會留在對話裡，不會送到通知管道。 |
| `POST /calls/api/sms/{id}/retry` | 用同一個門號重送 `failed` 或 `timeout` 的簡訊，回應同 `send`。其他簡訊回 HTTP 409 `not_retryable`。 |
| `DELETE /calls/api/sms/thread?number=<號碼或 key>` | 刪除一則對話：`{ok: true, removed: N}`。 |
| `GET /calls/api/quick-action` · `POST /calls/api/quick-action` | 32 種通知管道內附的單次簽章互動按鈕路由（`?act=confirm\|ignore\|block\|stall\|white&target=<id或號碼>&sig=<hmac>`）。打開連結（`GET`）只會先顯示確認頁，按下頁面上的按鈕（`POST`）才會執行；連結 7 天後失效。透過 HMAC-SHA256 驗證，不用登入控制台，支援全部 9 種語言。 |
| `POST /calls/api/control/inbound` | 跨平台通用指令 Webhook，用聊天指令控制 Bageo。Body 例如 `{"text": "清單 5", "channel_id": "line", "sender_id": "owner", "reply": false, "token": "..."}`（也相容 `user`／`channel`）。 |
| `GET /calls/api/auth-status` · `POST /calls/api/initial-password` | 查詢是否已設密碼與是否顯示首次設定密碼視窗（`{ok, password_set, authenticated, prompt_initial, language}`），以及首次建立或略過密碼（`{"password": "..."}` 或 `{"skip": true}`，僅在尚未設定任何密碼／Token 時可用）。 |

`POST /calls/api/notify/test` 的回應：

- 200 `{ok: true}`：測試訊息已送出。
- 400 `unknown_channel`。
- 502 `{ok: false, code, field, kind, error}`：`code` 是出錯原因，`field` 是 `url`／`token`／`target` 哪一欄（或空白），`error` 是該服務的回應。
- `code` 的值：`missing_field`、`bad_url`、`bad_value`、`auth_failed`、`service_not_found`、`http_error`、`service_error`、`network_error`、`send_failed`、`bot_problem`（Discord 不讓機器人連上）、`room_encrypted`（Matrix 聊天室有加密）、`blocked_address`（Bageo 不會送的位址）。

`POST /calls/api/notify/send` 的規則：

- `title` 空白就用「Bageo」；`body` 空白回 HTTP 400 `empty_body`。
- 只要有一個管道接受就回 `{ok: true}`。
- 簡訊可另外帶 `"kind": "sms"`、`"from"`（寄件人）、`"text"`（簡訊內容）、`"to"`（收件門號）、`"serial"` 與 `"device"`（手機）、`"received_ms"`（收到的時間，Unix 毫秒）、`"ref"`（你自己給這則簡訊的識別碼）、`"sub_id"`（收到它的 SIM 卡）。Bageo 會把它存進控制台的「簡訊」頁，同一個 `ref` 只存一次；桌面狀態小程式用它顯示簡訊通知。舊的請求照常運作。
- [簡訊轉發工具](../../tools/sms_forward/README.md)（英文）就是用這條路由。

`POST /calls/api/control/inbound` 的規則：

- 支援 Slack、Discord、LINE、企業微信、釘釘、飛書、Mattermost、Rocket.Chat、Zulip、Matrix、Nextcloud Talk、Synology Chat、Home Assistant、n8n 或 curl。
- 可執行全部 18 個聊天指令（包含 `block`、`stall`、`white`、`unblock`），並以通知語言回傳 `{ok, text, buttons, pushed}`。
- 用 MCP 的 token 打時，只能做 `mcp_calls` 允許的事（見 [MCP](MCP.md)）。設成 `off`（預設）時，只能查看，不能用指令改任何東西。

你自己的 webhook 會收到的 JSON 和每一種事件，在[通知](NOTIFICATIONS.md#webhook-json)裡有說明。

## 語音與背景音試聽

`GET /calls/api/preview` 回傳一段 WAV：用某個聲音講幾句電話用語，背後墊著背景音。每個查詢參數沒帶時都用已存的設定：

| 參數 | 意思 |
|---|---|
| `voice` | 聲音名稱，或 `none` 表示只要背景音。 |
| `ambience` | `off` 或某一種背景音。 |
| `level` | 1–3。 |
| `regen=true` | 重新產生，不用已存的範例。 |
| `clip=reject` | 用這個聲音念的拒接語音。 |
| `lang` | `ja`、`zh-TW`… |

內建的 30 個聲音在 9 種語言都已預先錄好試聽和拒接語音（`assets/voices/` 裡共 1,080 段），所以試聽不用連 Gemini。只有 `regen=true` 或沒有預錄的聲音才會開一次連線。

錯誤：400 `unknown_voice`／`unknown_background`、404 `background_missing`（背景音檔沒載入）、502 `preview_failed`。

## 行程預約與任務

有人打來說想約星期六下午三點來看房。`appointment_mode` 設成 `confirm` 時（見[設定](SETTINGS.md#摘要與行事曆)），AI 會記下時間但不先答應。

通話結束後留下一筆待處理事項（API 裡叫 task，`intent` 為 `appointment`），等你同意或不同意。同一個 `appointment` 物件會出現在通話紀錄、它的 notes 和待處理事項上：

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
| `start`、`end` | 當地時間（`TZ`）。對方只是順口提的備案時間不會蓋掉談好的時間；同一通電話另外約的第二件事會變成另一筆待處理事項。 |
| `title`、`caller_name` | 約的是什麼事、對方怎麼自稱（對方自己的說法）。 |
| `status` | `pending`（等你決定）、`confirmed`、`declined`；`booked` 只會出現在舊版的紀錄。 |
| `reply` | 怎麼告訴對方：`sms`、`call` 或 `none`。 |
| `sms` | `sent`，或簡訊沒傳出的原因。代碼見下方。 |
| `call` | AI 回電的進度，或沒撥出的原因。代碼見下方。 |
| `call_lang` | 那通電話講的語言（`zh-TW`、`en`…）。AI 回電就講這個語言，等對方接起來出聲後才說你的回覆，只說一次。沒有這個欄位的舊紀錄照設定 `language`。 |
| `tg_card` | 發給這筆預約的 Telegram 卡片 `{chat_id, message_id}`；你在別處決定後，用來拿掉卡片上的按鈕。 |
| `tag` | 這筆預約固定的 id。 |

`sms` 的值：

- `sent`：已傳出。
- 沒傳出的原因：`offline`（手機沒連線）、`old_app`（App 不能傳簡訊）、`timeout`、`busy`（同一則簡訊正在傳）、`no_number`（收不到簡訊的號碼）。
- 手機回的代碼：`no_permission`、`bad_request`、`no_service`、`radio_off`、`generic_failure`、`exception`。

網頁和 Telegram 回覆會把這些寫成白話。

`call` 的值：

- `queued`（排在打給同一個號碼的前一通回電後面）、`dialing`、`connected`（手機已撥號，AI 等對方接起來出聲）。
- `answered`（對方已得知結果）、`no_answer`（70 秒內沒人出聲）、`failed`、`done`（通話結束、結果不明）。
- 沒撥出的原因：`no_phone`、`send_failed`、`no_number`、`not_callable`（國外、免付費、加值付費或特殊短碼）。
- 手機的回報也記在待處理事項的 `events`（`callback_place_call_ack`、`callback_place_call_failed`、`callback_outbound_ready`、`callback_done`、`callback_failed`）。

空的欄位不會出現。

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/tasks` | 待處理事項，最新的在前（最多 200 筆）：`{ok, tasks}`。內容見下方。 |
| `GET /calls/api/tasks/{id}/event.ics` | 預約的 iCalendar 檔（Apple 日曆和其他 App）。用控制台密碼或連結裡的 `?k=<HMAC 金鑰>` 開；這筆待處理事項沒有預約時回 HTTP 404 `no_calendar`。 |
| `POST /calls/api/tasks/{id}/confirm` | **同意**，跟在網頁或 Telegram 卡片上按一樣。Body：空的、`{}` 或 `{"reply": "sms" \| "call" \| "none"}`。回應 `{ok, task, message}`。 |
| `POST /calls/api/tasks/{id}/ignore` | **不同意**：用同樣的方式告訴對方，客氣婉拒。body、回應和規則同 `…/confirm`。 |
| `POST /calls/api/appointments/{id}/retry-sms` | 已決定的預約簡訊沒傳出時再傳一次。Body：空的、`{}` 或 `{"serial": "<手機>"}`。回應 `{ok, sms, message}`。 |

`GET /calls/api/tasks` 的內容：

- 預約的待處理事項另外帶 `calendar_links`（`{google, outlook, office365, yahoo}`）。
- `reply` = `{sms, call, default}`：這位來電者能不能收簡訊、能不能回電，以及「預設回覆方式」會選哪個。
- `sms_route` = `{serial, label}`：你沒另選手機時簡訊從哪支發（接這通電話的手機）；`label` 是它在 `device_labels` 的名稱，沒有就是 id。
- 來電者是聯絡人時還有 `contact_name`。
- 網頁上的「待處理事項」列的就是 `appointment.status` 為 `pending` 的項目。

`POST /calls/api/tasks/{id}/confirm` 的內容：

- 沒帶 `reply` 時照「預設回覆方式」；選了這位來電者用不了的方式就不通知。
- `message` 是白話寫的結果，跟 Telegram Bot 回的同一段，附上你選的 `calendar_app` 的「加入行事曆」連結。
- 還沒選 `calendar_app` 時，附的是這通電話在網頁的頁面，在那裡按「加入行事曆」選一家；沒有控制台網址就不附。
- 傳簡訊最多可能等 90 秒；回電交給手機撥出（在 `appointment.call` 看進度）。

`POST /calls/api/appointments/{id}/retry-sms` 的內容：

- `serial` 可以換另一支已連線的手機；沒帶就是接這通電話的那支。
- 傳出去了 `ok` 才是 true，`sms` 是新的代碼。
- 還在等、已經傳出或本來就不能傳（`no_number`）時，回 HTTP 409 `sms_not_retryable`（附目前的 `sms`）。
- 其他錯誤：404 `task_not_found`；400 `invalid_serial`／`bad_request`。

決定的規則：

- **只有第一次決定算數。** 第二次同意或不同意，包括第一次還在處理時送來的、或在 Telegram 卡片上按的，都會回「已經處理過了」，什麼都不改。
- 這筆預約有發過 Telegram 卡片時，卡片上的按鈕會被拿掉，並把 `message` 貼到那個聊天裡（盡力而為；不論成功與否，決定都有效）。
- 簡訊和回電都從接到那通電話的 SIM 卡（`sub_id`）發出。簡訊只寫時間和星期幾，不寫約的是什麼事。
- 推銷和詐騙電話不會留下預約；事後才判定是這類電話的，預約會被拿掉。

```bash
curl -s -X POST "http://127.0.0.1:8787/calls/api/tasks/$TASK/confirm" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"reply": "call"}'
```

```json
{"ok": true, "task": {"id": "…", "appointment": {"status": "confirmed", "reply": "call", "call": "dialing"}},
 "message": "已同意預約\n11/14（週六）15:00–16:00 看房\nAI 正在回電給 0912345678\n加入行事曆：https://calendar.google.com/…"}
```

## AI 代打

AI 代打就是 AI 替你打出去的電話。給它一個號碼（或一位聯絡人）和要辦的事：訂位、跟朋友約時間、問店家一件事。一支 AI 模式的手機撥出去，AI 負責講，你拿結果。用 API 訂一家餐廳的流程大概是這樣：

1. `POST /calls/api/errands`，帶上號碼和「明天晚上 7 點訂 4 位，名字寫王先生。7 點半到 8 點也可以。」代打狀態變成 `dialing`。
2. 每隔幾秒讀一次 `GET …/errands/{id}/live` 看即時字幕。餐廳說 7 點客滿，問 7 點半還是 8 點 15 分。
3. 明顯在你交代範圍內的選擇，AI 會自己答應；不確定時它會跟對方說要確認一下，即時畫面上出現一個帶選項的 `question`。
4. 用 `POST …/errands/{id}/say` 回答，或打一句指示（「跟他說 8 點也可以」）。
5. 通話結束後代打變成 `done` 並附上 `result`，你會收到一則通知。

**通話中。**

- AI 遇到你的交代沒講清楚的事，會跟對方說要確認一下，然後問你（`ask_owner` 函式）。
- 你正在看這筆代打時（6 秒內有人讀過它的即時畫面），問題會顯示在那裡，最多等 40 秒。
- 沒人在看、或你沒來得及回答，AI 會問清楚哪些選項可以，說你確認後再回電，然後客氣地結束通話。
- 等答案的時候，AI 不會先替這個問題做決定；對方先說再見的話，問題就到此為止。
- 你在通話中打的字會當成指示交給 AI。

**通話後。** 摘要模型會對照交辦內容讀這通電話，你會收到一則通知（標題「AI 代打電話：<名字或號碼>」，用通知語言；webhook 事件 `ai_errand`）。AI 在你沒回答的問題上自己答應的事，結果一定不是 `done`，而是 `need_owner`，提醒你確認，不合適就回電改。

**用誰的身分打。** 設定 `errand_identity`（預設 `assistant`，你的助理；或 `owner`，你本人），每筆代打也可以各自指定。選 `assistant` 時，被認真問到會表明是 AI 助理；不論哪一種，都不透露交辦內容以外的個人資料，也不答應任何付款、訂金或轉帳。

**能打哪些號碼。**

- 不打緊急號碼、短碼（7 碼以下）或加值付費號碼（例如台灣 020x 像 0204、0209，+1 900／976，日本 0990，英國 09…）；免付費號碼可以打。
- 沒有加號的號碼當成你所在國家的號碼（`country_code`）。
- 用國際冠碼開頭的號碼（`00`、台灣的 `01x` 和 `00x`、北美的 `011`、日本的 `010`）一律 `not_allowed`：國外號碼請用 `+` 寫。
- 台灣號碼長途冠碼 0 後面接 1 的也是 `not_allowed`。

**用哪支手機、哪張卡。**

- 電話從一支已連線、在 AI 模式、而且有那個號碼所屬國家 SIM 卡的手機撥出。
- Bageo 從手機控制連線 `hello`／`status` 上報的 `sims` 清單學會每支手機有哪些 SIM（subscription id、國家、電信商與門號；舊版 App 則來自 `incoming` 和 media `hello` 裡的 `sim_sub_id`、`sim_country`）。
- 這些記在 `phone_sims.json`。
- Bageo 從沒聽過 SIM 的手機不接代打（`sim_unknown`）：重新連線那支手機（舊版 App 則先打一通電話給它）。
- 手機收到的是 E.164 格式的號碼和要用的 SIM（`sub_id`）。
- 那通電話自己的 media `hello` 顯示用的是別張 SIM、或沒回報時，Bageo 會在 AI 開口前掛斷（`failed`、`call_code` `wrong_sim`，會通知你），並把那張卡從這支手機的紀錄裡拿掉，等之後有來電回報它再加回來。

**排隊與上限。**

| 情況 | 會怎樣 |
|---|---|
| 手機正在通話 | 代打排隊（`queued`），超過 10 分鐘算失敗（`phone_busy`）。 |
| 同一個號碼還有另一通 | 一個號碼一次只打一通，跟行程預約的 AI 回電共用排隊。排在後面超過 25 分鐘算失敗（`number_busy`）。 |
| 通話長度 | 最長 10 分鐘；9 分半開始收尾。 |
| 語音選單 | AI 最多等 90 秒等真人來接；它不能按鍵（`keypad_menu`）。 |
| 電信錄音（「您撥的號碼是空號」） | 直接掛斷（`carrier_message`）。 |
| 沒人接或失敗 | 不會自己重撥：要不要「再打一次」由你決定。打開 `redial` 見下一列。 |
| 「撥到通」（`redial`） | 沒人接、忙線、對方關機或進語音信箱（不留言）：代打排隊（`queued`，`call_code` 是 `redial_wait`，時間在 `redial_at`），一分鐘後再撥，總共最多 30 次。有人接起來、打滿 30 次（`no_answer`）、號碼不通、語音選單或手機出問題就停。只在最後通知一次。 |
| 撥號中你按了取消 | 手機和號碼立刻放出來給下一通；萬一電話還是接通了，會在 AI 開口前掛斷。 |

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
| `call_code` | 撥號進行到哪裡。值見下方。 |
| `attempts` | 從 1 開始，每按一次「再打一次」或每重撥一次加 1。 |
| `redial` | 打開了「撥到通」。沒打開就不出現。 |
| `redial_at` | 下一次什麼時候撥（`call_code` 是 `redial_wait` 時）。其他時候不出現。 |
| `redial_start` | 這一輪重撥從第幾次開始（按「再打一次」會重新算 30 次）。是 0 就不出現。 |
| `record_id` | 這一次的通話紀錄（`GET /calls/api/call/{id}`：逐字稿和錄音）。 |
| `follow_up_of` | 這通是接續哪一筆先前的代打；那一筆最後 20 行逐字稿會放進 AI 的腳本。 |
| `questions` | `[{id, text, options, answer, answered_by, asked_at}]`：AI 問過你的問題，選項最多 4 個。 |
| `result` | `null` 或 `{outcome, summary, details, follow_up, options, event, unconfirmed}`。欄位見下方。 |
| `history` | 先前幾次的嘗試，`[{attempt, status, result, record_id, ended_at, questions}]`。 |

`call_code` 的值：`dialing`、`queued`、`answered`、`no_answer`、`failed`、`phone_busy`、`number_busy`、`no_phone`、`send_failed`、`keypad_menu`、`carrier_message`、`wrong_sim`、`agent_restarted`、`cancelled`、`redial_wait`、`voicemail`。

`questions` 裡的 `answered_by`：`owner`（你回答了）、`timeout`（你在看但沒來得及回答）、`absent`（沒人在看）；還在等時是 `null`。

`result` 的欄位：

- `outcome`：`done`、`partial`、`not_done`、`need_owner`（選項在 `options`，最多 8 個）、`no_answer`、`voicemail`、`failed`。
- `event`：`null` 或 `{start, end, title}`（帶時差的當地時間；沒談定結束時間就沒有 `end`）。
- `unconfirmed`：`details` 或 `event` 裡 AI 複誦時對方沒有明確確認的時間、日期、號碼或名字（最多 8 項）；沒有就不出現。

代打存在資料夾裡的 `errands.json`（保留最新 500 筆）。Bageo 重新啟動時還在進行中的代打會變成 `failed`（`call_code` `agent_restarted`）；打開「撥到通」的會接著撥，除非對方已經接起來過。

### 代打路由

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/errands` | `{ok, errands}`，最新的在前，最多 200 筆。 |
| `POST /calls/api/errands` | 開始一筆。Body `{number, name, contact_id, task, identity, serial, follow_up_of, answers, redial}`（`redial` 是 `true` 就打開「撥到通」，預設 `false`），回應 `{ok, errand}`。 |
| `POST /calls/api/errands/preflight` | 撥出前看這筆代打還缺什麼。Body `{task, lang}`，回應 `{ok, kind, missing, ladder, source, ms}`。不會撥號。內容見下方。 |
| `POST /calls/api/errands/{id}/cancel` | 排隊中：取消。撥號中或通話中：掛斷。已經結束的原樣回傳。`{ok, errand}`。 |
| `POST /calls/api/errands/{id}/retry` | 同一筆再打一次（`attempts` 加 1，上一次移到 `history`）。`{ok, errand}`。還在進行中（或上一通還沒結束）：400 `busy`；手機相關的代碼同新增。 |
| `DELETE /calls/api/errands/{id}` | 從清單刪掉（通話紀錄會留著）。`{ok}`；撥號中或通話中：400 `busy`。 |
| `GET /calls/api/errands/{id}/live?after=<seq>` | 即時字幕和等你回答的問題：`{status, lines, question, watching}`。內容見下方。 |
| `POST /calls/api/errands/{id}/say` | Body `{text, question_id}`（最多 300 字）。帶 `question_id`：回答那個問題。沒帶：有問題在等就當成它的答案，否則當成給 AI 的指示（「跟他說 8 點也可以」）。`{ok}`。 |

`POST /calls/api/errands` 的內容：

- 必填：`task`，以及號碼（或給聯絡人、先前的代打，用它們的號碼）。
- `identity` 留空就照 `errand_identity`；`serial` 留空就用任何合適的手機。
- 回應的狀態是 `dialing`，手機或號碼忙線時是 `queued`。
- `answers` 可以不帶：你回答補問後的內容，格式 `{key: 文字}`。key 就是 `preflight` 回傳的那些，另有 `note` 放自由文字；每項最多 80 字（`note` 120 字），不認得的 key 會被丟掉。它們會以一小段文字放進這通電話的腳本交給 AI。

`POST /calls/api/errands/preflight` 的內容：

- 送出 `task`（必填，最多 1,000 字）和 `lang`（可以不帶，例如 `en`、`zh-TW`：補問用的語言；留空就用通知語言）。
- `kind`：`restaurant`、`appointment`、`order`、`change` 或 `contact`。
- `missing`：最多 3 筆 `{key, question}`，最重要的在前；什麼都不缺就是空的。各類型的 key：`restaurant` `time`、`party`、`phone`、`name`；`appointment` `time`、`item`、`contact`、`backup`；`order` `spec`、`pickup`、`substitute`；`change` `orig_time`、`contact`；`contact` 沒有。
- `ladder`：你願意接受的時間，最好的在前（`["19:30", "20:30"]`），最多 4 個；交辦裡沒有寫到兩個以上的時間就是空的。
- `source` 和 `ms`：`local`、`model` 或 `timeout`，以及花了幾毫秒。
- Bageo 自己讀得懂的交辦，幾毫秒內就在本機回答。讀不懂的（別種文字、夾很多數字的長文）會用 Gemini 金鑰問一個小模型（`gemini-flash-lite-latest`），最多等 0.9 秒；時間內沒答（`source` 為 `timeout`）或沒有金鑰，就不補問、照常撥出。
- 在聊天室，確認卡會列出同樣的問題。直接回覆那張卡就是補答，你的回覆會存成 `answers.note`，AI 帶著它撥出；回「取消」就不撥。

`GET /calls/api/errands/{id}/live?after=<seq>` 的內容：

- `lines`：`seq` 之後的字幕，`[{seq, who, text, at}]`；`who` 是 `ai`、`callee`、`owner` 或 `system`（保留最新 400 行；1.2 秒沒有新字就算一行結束）。
- `question`：`null` 或 `{id, text, options, asked_at, expires_at}`，等你回答的問題。
- `watching`：你是否算正在看。每讀一次就算你正在看 6 秒，所以跟著通話時每隔幾秒讀一次。

錯誤代碼（HTTP 400 `{ok: false, error, code}`，兩者同值）：

| 代碼 | 路由 | 意思 |
|---|---|---|
| `no_number` | 新增 | 沒有號碼，也沒有可以拿號碼的聯絡人或先前代打。 |
| `bad_number` | 新增 | 不是電話號碼（有字母、`+` 不在開頭、超過 15 碼、以那個國家來說太短）。 |
| `not_allowed` | 新增 | 緊急號碼、短碼、加值付費號碼，或用國際冠碼代替 `+`。 |
| `no_task` | 新增、補問 | `task` 是空的。 |
| `task_too_long` | 新增、補問 | `task` 超過 1,000 字。 |
| `no_phone` | 新增、再打 | 沒有已連線、在 AI 模式的手機（或指定的 `serial` 不是這樣的手機）。 |
| `sim_unknown` | 新增、再打 | 沒有合適的手機，而且至少有一支的 SIM Bageo 還不知道：先打給它一通。 |
| `no_sim_for_country` | 新增、再打 | 沒有手機有這個號碼所屬國家的 SIM。 |
| `not_found` | 新增 | `contact_id` 或 `follow_up_of` 不存在（這裡回 400）。 |
| `busy` | 再打、刪除 | 這筆還在撥號或通話中。 |
| `not_on_call` | say | 電話不在通話中（或已經結束）。 |
| `empty_text`／`text_too_long` | say | 沒有內容／超過 300 字。 |
| `question_closed` | say | 那個問題已經回答、逾時，或不是正在等的那一個。 |
| `bad_request` | 所有 `POST` | 沒帶 `Content-Type: application/json`，或 body 不是 JSON 物件。 |

所有 `…/errands/{id}…` 路徑，代打 id 不存在或格式不對時一律回 HTTP 404 `not_found`。

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

聯絡人替打來的號碼配上名字，存在通話紀錄資料夾（`CALLS_DATA_DIR`）的 `contacts.json`。

號碼不管怎麼寫都對得上：`+886912345678`、`0912-345-678`、`886912345678` 是同一支。沒有國碼的號碼照你的國家（`country_code`）解讀；來電則照接到這通電話的 SIM 卡所屬國家。

聯絡人也用在 AI 代打（選聯絡人就不用打號碼）和推銷判斷：已存的聯絡人講到錢，不會被當成推銷或詐騙。

一位聯絡人是 `{id, name, phones: [...], note, kind, source, updated_at}`。

- 最多 10 個號碼，名字最多 80 字、備註最多 500 字。
- `kind`：`contact`（預設，一般聯絡人）、`whitelist`（白名單，家人或重要的人，搭配 `whitelist_mode` 使用）、`blacklist`（黑名單，來電直接拒接）或 `stall`（陪聊名單，來電直接進入 AI 陪聊反制模式）。
- `source`：`manual`、`vcf`、`csv`、`text` 或 `ai`（Gemini 讀出來的）。
- 只有黑名單的號碼可以在結尾加 `*`（`0800*`；開頭的 0 不算，至少三碼，所以 `080*`、`0000*` 不行），擋下同一個開頭的所有號碼；其他名單用 `*` 會回 400 `invalid_contact`，`field` 是 `prefix`。
- 一個號碼只屬於一位聯絡人；通訊錄最多 10,000 位（`too_many_contacts`）。

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/contacts` | 查詢參數 `q`（名字、備註或號碼，號碼怎麼寫都行）、`kind`（`contact` \| `whitelist` \| `blacklist` \| `stall`）、`limit`、`offset`。回應 `{ok, contacts, total, offset, count}`：符合的結果（共 `total` 筆），以及全部聯絡人數（`count`）。 |
| `POST /calls/api/contacts` | Body `{name, phones: [...], note, kind?}`，回應 `{ok, contact}`。錯誤見下方。 |
| `POST /calls/api/contacts/block` | Body `{number, reason?}`：把號碼（或結尾 `*` 的開頭）加進黑名單。已經是某位聯絡人的就把他改成黑名單（沒有備註才補上 `reason`），否則新增一位，名字用 `reason`，沒有就用號碼。回應 `{ok, contact, already}`；`already` 表示本來就在黑名單。 |
| `PUT /calls/api/contacts/{id}` | 同樣的 body，取代名字、號碼、備註和名單分類 `kind`。404 `contact_not_found`，409、400 同上。 |
| `DELETE /calls/api/contacts/{id}` | `{ok}`。 |
| `POST /calls/api/contacts/import` | Body 就是檔案本身（最大 10 MB），檔名 URL 編碼後放在 `X-Filename`。只讀、**不存**（預覽）。內容見下方。 |
| `POST /calls/api/contacts/batch` | Body `{contacts: [{name, phones, note, source}], duplicates: "merge" \| "skip"}`：匯入時你要留的那些。跟已存聯絡人重複的，合併進去（補上新號碼和備註；預設）或略過。回應 `{ok, added, merged, skipped, count}`。 |
| `GET /calls/api/contacts/export.vcf` | 全部聯絡人匯出成 vCard 3.0 檔。 |

`POST /calls/api/contacts` 的錯誤：

- 號碼已經是別的聯絡人的：HTTP 409 `contact_exists`，附 `contact: {id, name}`。
- 沒有名字或沒有號碼：400 `invalid_contact`，附出問題的 `field`。

`POST /calls/api/contacts/import` 的內容：

- 沒帶 `X-Filename` 回 400 `bad_request`。
- 預覽回應：`{ok, file, format, ai, entries: [{name, phones, note, source, duplicate}], skipped}`。
- `format`：`vcf`、`csv`、`text`、`xlsx`、`docx`、`pdf` 或 `image`。`ai`：是 Gemini 讀的。
- `duplicate`：`null`，或這筆重複的已存聯絡人 `{id, name, by: "phone" | "name"}`。
- `skipped`：沒有名字或號碼而略過的筆數。
- 錯誤：400 `empty_file`、413 `file_too_large`／`too_many_rows`、415 `unsupported_file`、400 `need_gemini_key`、502 `ai_failed`（Gemini 的回覆在 `error`）。

Bageo 自己讀得懂的格式：

- vCard 2.1、3.0、4.0（含 quoted-printable、各種 `CHARSET`）。
- 有表頭（Google 通訊錄、Outlook，或欄名像 *姓名*、*電話*、Name、Phone）或沒有表頭的 CSV、TSV，編碼 UTF-8、UTF-16 或電腦的編碼（Big5、GBK、Shift_JIS、EUC-KR、Windows-1252）。
- 一行一個名字加號碼的純文字。
- `.xlsx`、`.docx` 裡的表格。

PDF、圖片（印出來的名單照片、通訊錄截圖）和讀不出人的檔案，交給 Gemini 的視覺模型（`gemini-flash-lite-latest`，用設定裡的 Gemini 金鑰）。

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

## 備份與還原

十二個路由用來備份和還原 Bageo 自己的資料，對應控制台設定裡的「備份與還原」（見[設定參考](SETTINGS.md#備份與還原)）。任何資料都不會送到你的電腦以外。它們照控制台的密碼規則，每個 `POST` 也要照上面的 `Content-Type: application/json` 規則（`import` 跟 `contacts/import` 一樣，body 是檔案本身，另外帶 `X-Filename` 標頭）。`GET …/download/{ticket}` 只要下載碼，下載網址裡不會出現任何密碼。

四個設定用 `PUT /calls/api/settings` 存：`backup_schedule`（`off`、`daily`、預設的 `weekly`，或 `monthly`）、`backup_dir`（空白：資料夾底下的 `backups`；否則要填絕對路徑，Bageo 會建好資料夾並試寫一個檔，寫不進去就回 HTTP 400 `backup_dir_unwritable`）、`backup_keep`（1 到 100，預設 10，只算自動備份）、`backup_include_calls`（預設 `false`；`true` 時自動和手動備份也會放進通話紀錄、錄音、簡訊和待處理事項）。另外兩個是加密：`backup_encrypt`（預設 `false`；`true` 時手動、自動和 `before_import` 備份都加密）、`backup_password`（至少 8 個字元，否則 HTTP 400 `backup_password_short`；不會回傳，只回 `backup_password_set`；`backup_password_clear: true` 會刪掉）。沒有密碼卻開 `backup_encrypt`，或加密開著時刪掉密碼，回 HTTP 400 `backup_password_missing`。密碼跟其他設定一起存在這台電腦，自動備份才用得到；加密保護的是放到別處的那份，例如外接硬碟、雲端或網路資料夾。它屬於 `keys`。

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/backup` | 目前狀態：`{dir, default_dir, dir_ok, can_open_folder, can_download, running, last, next_at, backups}`。 |
| `POST /calls/api/backup/run` | Body `{}`。馬上做一份手動備份：`{entry}`，是 `backups` 裡的一筆。已經有備份在跑：HTTP 409 `busy`。失敗：HTTP 500 `dir_unwritable`、`disk_full` 或 `io`。 |
| `POST /calls/api/backup/export` | Body `{items, password}`。做一個檔案，內容是 `items` 指定的項目：`settings`、`keys`、`contacts`、`numbers`、`phones`、`errands`、`calls`、`sms`、`tasks`。沒帶 `items` 就是 `settings`、`keys`、`contacts`、`numbers`、`phones`、`errands`。有不認識的 id 或清單是空的：HTTP 400 `bad_items`。有 `password`（至少 8 個字元，否則 HTTP 400 `backup_password_short`）就加密成 `.bageo`，沒有就是 zip。回 `{ticket, name}`，再用 `GET …/download/{ticket}` 下載。含 `calls` 時檔案可能很大。含 `keys` 又沒設控制台密碼時，只有在 Bageo 所在的這台電腦用 `localhost` 打開才行，否則 HTTP 403 `need_password`。 |
| `POST /calls/api/backup/file-ticket` | Body `{name}`。拿備份資料夾裡某個檔案的下載碼：`{ticket, name}`。`name` 必須是資料夾裡真的有的備份檔名（否則 HTTP 404 `not_found`）。備份檔裡有金鑰：沒設控制台密碼時，規則跟含 `keys` 的匯出一樣（HTTP 403 `need_password`）。 |
| `GET /calls/api/backup/download/{ticket}` | 下載某個下載碼的檔案。下載碼是 256 位元的隨機數，2 分鐘內有效、只能用一次，之後回 HTTP 404 `not_found`。匯出的檔案送完或下載碼過期就刪掉。 |
| `POST /calls/api/backup/import` | Body 是檔案本身（最大 200 MB），檔名 URL 編碼後放 `X-Filename`。可以是備份 zip、加密的備份（`.bageo`），或舊版單獨的 `settings.json`。**不會還原任何東西**：檔案暫存 15 分鐘，回傳預覽。加密的檔案只回 `{token, source_name, encrypted: true, needs_password: true}`，沒有 `items`，要先用 `unlock` 解開。最多 3 份預覽等著：再來一份就丟掉最舊的（它的 token 之後回 `expired`）。同時最多 2 個上傳在跑，再多回 HTTP 409 `busy`。 |
| `POST /calls/api/backup/restore` | Body `{name}`。對備份資料夾裡的檔案做同樣的預覽（沒有 200 MB 的上傳上限）。加密的檔案會先用這台存的備份密碼試，打得開就直接回預覽，打不開就跟匯入一樣回 `needs_password`。 |
| `POST /calls/api/backup/delete` | Body `{name}`。刪掉備份資料夾裡這個備份檔，回 `{ok}`，刪了不能復原。`name` 只能是備份檔名（`.zip` 或 `.bageo`），帶路徑回 HTTP 400 `bad_request`；不是資料夾裡的一般檔案（不存在、符號連結）回 HTTP 404 `not_found`；正在備份或還原時回 HTTP 409 `busy`。沒設控制台密碼時，規則跟 `file-ticket` 一樣（HTTP 403 `need_password`）。 |
| `POST /calls/api/backup/unlock` | Body `{token, password}`。用密碼解開預覽裡的加密檔，回完整的預覽。密碼不對：HTTP 400 `wrong_password`。一次只試一個，每次至少隔一秒；不管是哪份預覽，連續錯 5 次以後每次等待加倍，最長 60 秒。同一份預覽錯 5 次就丟掉（HTTP 400 `expired`）。沒設控制台密碼時只能在執行 Bageo 的這台電腦上解（HTTP 403 `need_password`）。 |
| `POST /calls/api/backup/apply` | Body `{token, items}`。`items` 必須是預覽 `items` 的子集合、不能是空的，否則 HTTP 400 `bad_items`。只還原這些項目。`{ok, before_import, restart_needed, warnings}`。 |
| `POST /calls/api/backup/discard` | Body `{token}`。丟掉不還原的預覽和它的上傳檔：`{ok}`（token 不存在或已經沒了也一樣）。控制台關掉還原視窗時會呼叫。 |
| `POST /calls/api/backup/open-folder` | Body `{}`。用檔案總管或 Finder 打開備份資料夾。只有從 Bageo 所在的這台電腦連線、而且是 Windows 或 macOS 才行，否則 HTTP 403 `not_local`。 |

細節：

- `GET /calls/api/backup`：`dir` 是正在用的資料夾，`default_dir` 是預設資料夾，`dir_ok` 是寫不寫得進去，`can_open_folder` 是 `open-folder` 能不能用，`can_download` 是在這裡能不能下載含金鑰的檔案，`running` 是不是正在備份，`next_at` 是下一次自動備份的時間（RFC 3339，或 `null`）。`last` 是最近一次自動或手動備份的 `{at, ok, name, size, error}`，沒有就是 `null`。`backups` 是資料夾裡的檔案，新到舊，每筆 `{name, at, size, kind, app_version, include_calls, encrypted}`；`kind` 是 `scheduled`、`manual`、`before_import` 或 `export`。加密的檔案沒有密碼什麼都讀不到，所以 `app_version` 和 `include_calls` 是 `null`。
- 備份檔是 zip。根目錄的 `bageo-backup.json` 放 `{format: "bageo-backup", version: 1, app_version, created_at, kind, include_calls, items, files: [{path, size, sha256}]}`，`items` 是檔案裡真的有的項目 id。其他檔案都在 `data/` 底下，路徑是資料夾裡的相對路徑（`data/config/settings.json`）。不放的有：`backups` 資料夾本身、暫存檔、除錯檔、重新啟動標記這類一次性的檔案，以及快取。自動、手動和 `before_import` 備份一律含 `settings`、`keys`、`contacts`、`numbers`、`phones`、`errands`，`backup_include_calls` 為 `true` 時再加 `calls`、`sms`、`tasks`。匯出只放勾選的項目：選了 `settings` 沒選 `keys`，`settings.json` 不含整組 `keys`；選了 `keys` 沒選 `settings`，只放這一組和手機配對金鑰（`phone-pairing.key`）。`keys` 是金鑰、密碼和連線：Gemini 和其他服務的金鑰、通知和聊天機器人的權杖、控制台密碼、MCP token，以及所有決定資料送去哪裡、誰可以遙控 Bageo 的設定（整個通知管道，含送去的對象和誰可以下指令；Telegram 聊天和遙控；通知閘道；寄信帳號；摘要服務；TRTC 橋接和端點；轉接號碼和急事要不要直接轉過去；Bageo 可以回撥的號碼；MCP 存取），再加上手機配對金鑰。完整清單是 `crates/agent/src/settings_store.rs` 的 `CREDENTIAL_FIELDS` 和 `CONNECTION_FIELDS`。
- 檔名用本機時間：`bageo-backup-YYYYMMDD-HHMMSS-auto.zip`、`…-manual.zip`、`…-before-import.zip`，匯出的是 `bageo-export-YYYYMMDD-HHMMSS.zip`（`kind` 是 `export`，跟其他檔案一起列出，Bageo 不會自動刪）。加密的檔案結尾是 `.bageo`，不是 `.zip`。
- 加密的檔案（`.bageo`）就是同一個 zip 加密後的樣子，有密碼就能用一般工具自己解開：
  - 檔頭 62 位元組（整數都是 little-endian）：`BAGEOENC`（8 位元組）；格式版本 `1`（1）；金鑰推導 `1` = Argon2id、版本 0x13（1）；記憶體 KiB（4，65536 = 64 MiB）；次數（4，3）；並行數（4，1）；salt（16，隨機）；加密法 `1` = XChaCha20-Poly1305 的 STREAM 做法（1）；每塊大小（4，1048576）；nonce 前綴（19，隨機）。
  - 金鑰：用 Argon2id 從密碼（UTF-8，去掉前後空白）和 salt 算出 32 位元組。
  - 接著是 zip 一塊一塊加密：每塊是每塊大小那麼多位元組（最後一塊比較短，也可能是空的），用 XChaCha20-Poly1305 加密，後面接 16 位元組的 tag。每塊的 24 位元組 nonce 是前綴、這塊從 0 起算的編號（4 位元組，big-endian）、再一個位元組：最後一塊是 1，其他是 0。整個檔頭是每一塊的 associated data。
  - 要求超過 256 MiB 記憶體、超過 4 次或超過 4 個並行數的檔案，Bageo 直接拒絕；任何一塊被改、被截斷或少了，都回 `damaged`（第一塊的話是 `wrong_password`）。
- 預覽是 `{token, source_name, created_at, app_version, kind, items, has_calls, calls_count, changes_password, changes_connections, total_size, warnings, encrypted, needs_password}`。`items` 的每筆是 `{id}` 或 `{id, count}`，列出檔案裡真的有的項目：`settings`、`keys`、`contacts`、`numbers`、`phones`、`errands`、`calls`、`sms`、`tasks`。舊版單獨的設定檔有金鑰或密碼欄位時，也會列出 `keys`。只有還原 `keys` 會換掉控制台密碼（換成別的或變成沒有）時，`changes_password` 才是 `true`；會改變通知送去哪裡、轉接號碼或誰可以遙控時，`changes_connections` 才是 `true`。兩個都是拿這台現在的跟還原後的結果比（算法跟 `apply` 一樣）。`total_size` 是可以還原的檔案解開後的大小（位元組）。`warnings` 可能有 `newer_version`、`plain_settings_file`（舊版單獨的設定檔：只還原設定，有金鑰的話一起還原）。預覽不會去碰檔案裡寫的備份資料夾。
- `apply` 只還原勾選的 `items`。選 `settings` 沒選 `keys`：換掉其他設定，這台電腦整組 `keys` 都保留。有選 `keys`：檔案裡有填值的欄位換掉這台的，檔案裡沒有或空的欄位保留這台的。只有從備份資料夾還原這台自己的 `before_import` 備份時，整組完全照檔案（沒有或空的欄位一律清空或回到預設），所以能把匯入帶進來的金鑰和連線全部拿掉；上傳的檔案一律不走這條。選 `keys` 沒選 `settings`：只換這一組，檔案裡有手機配對金鑰才換。檔案裡沒有 `keys`，或沒選 `keys`，這台的整組都不會變。`retention_days` 和 `backup_keep` 只會維持或變長：檔案的值比較大才採用，檔案沒寫就保留這台的。`backup_dir` 和 `public_url` 一律維持這台電腦的設定。還原的通話紀錄和待處理事項，只保留這次真的還原了的錄音路徑（`YYYY-MM-DD/<name>`），其他路徑一律清空。解開之前 Bageo 會先確認硬碟空間夠（不夠回 HTTP 500 `disk_full`）。`calls`、`sms`、`tasks` 是加進現有的，不會刪掉任何東西。動手之前 Bageo 會先把現在的設定做成一份 `before_import` 備份（選的資料夾寫不進去就放預設資料夾），檔名放在 `before_import`。`restart_needed` 表示有些變更要重新開啟 Bageo 才會全部生效。
- 預覽路由的錯誤（HTTP 400）：`not_backup`、`damaged`、`too_new`（來自比較新的 Bageo）、`too_large`。上限看檔案實際是什麼，不看 manifest 怎麼說：`calls`、`sms`、`tasks` 以外的項目每個檔案解開最多 20 MB、合計 50 MB，通話紀錄合計最多 8 GB；解開後比壓縮時大 200 倍以上的檔案算 `damaged`。已經有 2 個上傳在跑：HTTP 409 `busy`。`unlock` 密碼不對回 HTTP 400 `wrong_password`，錯 5 次回 `expired`。`apply` 在通話中回 HTTP 409 `call_active`；`items` 是空的或有預覽沒列的 id，回 HTTP 400 `bad_items`；token 超過 15 分鐘或已經用過，回 HTTP 400 `expired`。

## 手機與健康狀態

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/devices` | 手機清單和連線狀態。回應格式見下方。 |
| `POST /calls/api/devices/{serial}/mode` | Body `{"mode": "ai" \| "normal"}`。叫手機切換，最多等 15 秒等它回覆。回 `{ok, mode, error_code}`（400 時另附說明 `error`）。 |
| `DELETE /calls/api/devices/{serial}` | 移除一支沒連線的舊手機（同時清除手機清單、已存名稱與 SIM 紀錄，保留過往通話紀錄）：`{ok, serial}`。手機還在線或通話中回 HTTP 409 `device_online`／`device_in_call`。 |
| `PUT /calls/api/devices/{serial}/sim-number` | Body `{"sub_id": <整數>, "number": "..."}`。設定（空白代表清除）這支手機某張 SIM 的門號，讓來電與 AI 代打直接顯示並依門號挑選：`{ok, serial, sims}`。 |
| `POST /calls/api/phone-pairing/renew` | 更換 Wi-Fi 配對金鑰（`phone-pairing.key`）並中斷透過 Wi-Fi 連線的手機，每支手機重新插上 USB 一次即可取得新金鑰：`{ok: true}`（失敗回傳 HTTP 500 `save_failed`）。金鑰本身不會回傳。 |
| `GET /health` | 排查問題用的大型 JSON 物件：版本、語言、哪些東西已設定、已連線的手機、進行中的通話；不含金鑰或密碼。要帶密碼才拿得到（見下方說明最後一點）。 |
| `GET /calls/api/health` | 控制台用，內容和 `/health` 的完整版相同。跟其他 `/calls/api/` 路由一樣：有設密碼而沒帶回 HTTP 401，沒設密碼就開放。 |

`GET /calls/api/devices` 的內容：

- 回應：`{ok, usb: {enabled, adb_found, adb_path, error, phones: [{serial, state, model, reverse, app_installed}]}, connected: [...], labels: {<phone id>: <名稱>}, calls_in_progress, phones: [...]}`。
- 最外層的 `phones` 是 Bageo 啟動後在 control 連線上見過的每支手機：`{serial, connected, mode, supports_mode, model, app_version, in_call, last_seen_ms}`。
- `mode` 是 `ai`（AI 接聽）或 `normal`（一般手機，見[手機設定](PHONE.md#一般手機模式)）。
- 比這個功能舊的 App，`supports_mode` 是 false，永遠是 `ai`。

`GET /health` 的內容：

- `control_idle_clients`／`control_idle_serials` 算的是所有連上的手機；`normal_mode_serials` 列出切成一般手機的那幾支。
- `local_time` 是 Bageo 依 `TZ` 的時鐘。
- `summary_ready`（`summary_api_key_set` 是同一個值）表示有沒有設定摘要服務。
- `setup_missing` 列出控制台「都準備好了嗎？」還沒完成的項目：`ai_off`、`gemini_key`、`owner_name`、`notify`、`summary`（只有名稱，沒有設定值）；全部設好是空陣列。
- `GET /health` 只有帶了控制台密碼的請求才拿到完整內容。沒設密碼時，只有這台電腦自己送的請求拿得到（經過反向代理的不算；用 Docker 預設網路時，從主機送的請求也算別台電腦）。其他來源只回 `ok`、`version`、`local_time`、`language`、`notify_language`、`active_call_count`、`control_idle_clients` 和 `setup_missing`，監控用已經足夠。

切換模式後，`mode` 是手機現在的模式（不知道時為 `null`）；成功時 `error_code` 是 `null`，否則是下面其中之一：

| `error_code` | 意思 |
|---|---|
| `offline` | 手機沒連上。 |
| `unsupported` | App 比模式切換功能舊。 |
| `in_call` | 有電話在響或通話中；結束後再試。 |
| `busy` | 上一次切換還在做。 |
| `no_root` | Bageo 模組的 root 小幫手沒有接手：模組沒有在執行。 |
| `save_failed`、`dialer_failed`、`screening_failed`、`dnd_failed`、`volume_failed`、`failed` | 沒成功的那一步。再切一次會把每一步重做一遍。 |
| `timeout` | 還沒確認，不等於失敗：切換本身可能讓手機 App 重新啟動，它重新連上後才回報。稍後再讀一次 `GET /calls/api/devices`。 |
| `bad_request`／`invalid_mode` | HTTP 400：不是 JSON、沒帶 `Content-Type: application/json`、沒有字串 `mode`、serial 不是英數字、`-`、`_` 組成；或 `mode` 不認得。 |

```bash
curl -s -X POST "http://127.0.0.1:8787/calls/api/devices/pixel-12345/mode" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"mode": "normal"}'
# {"ok": true, "mode": "normal", "error_code": null}
```

## 手機這一端（WebSocket）

手機透過 WebSocket 連到 `/` 或 `/ws`（兩者相同）。

- 每支手機維持一條長期的 *control* 連線，每通電話另開一條 *media* 連線。
- 兩種連線都雙向傳送 JSON 文字訊框和原始的 16 kHz 單聲道 16 位元小端序 PCM 二進位訊框（手機每 60 ms 送一框）。
- 從這台電腦本身來的連線（USB 通道）不需 Token 一律接受；Wi-Fi 連線可連 `PHONE_WIFI_PORT`（預設 `8788`，僅提供 `/ws`）並帶上 USB 配對時發放的 `phone-pairing.key` 或 `DEVICE_TOKEN`，或連到主連接埠並在有設定 `DEVICE_TOKEN` 時帶上 `?token=<DEVICE_TOKEN>`。

逐欄位的完整訊息列表在 [`android/README.md`](../../android/README.md#websocket-protocol)（英文）。任何會講這個通訊協定的程式都能代替手機，所以要加上 SIP 或軟體電話前端，完全不用動通話邏輯。雙方都會忽略不認得的東西，所以舊 App、舊版 Bageo照樣能用。

### Control 連線

| 方向 | 訊息 | 意思 |
|---|---|---|
| 手機 → Bageo | `hello` | `serial`、`mode` = `CONTROL_IDLE`、`model`、`app_version`、`phone_mode`（`ai`／`normal`）、`in_call`、`supports_sms`、`supports_reject`、`sims`（`[{sub_id, country, slot?, carrier?, number?}]`，1.4 以上 App 會自動上報）。缺少欄位的意思見下方。 |
| 手機 → Bageo | `ping` | 每 15 秒一次。 |
| 手機 → Bageo | `status` | `{mode, in_call}`（SIM 變動時另含 `sims`）：模式、通話狀態或 SIM 清單變了。 |
| 手機 → Bageo | `incoming`、`incoming_cancel` | 有電話在響（`caller_id`，未顯示號碼時是空字串；`sim_country`；知道時帶 `sim_sub_id`）／接起來之前就結束了。 |
| 手機 → Bageo | `reject_failed` | 收到 `reject: true` 的 `answer_plan` 但未能拒接該通來電。 |
| Bageo → 手機 | `answer_plan` | `{delay_ms, prewarm, reject}`：響多久再接，或 `reject: true` 響鈴中直接拒接（`reject_mode` 為 `decline` 的黑名單號碼）。 |
| 手機 → Bageo | `call_waiting_rejected` | `{serial, caller_id, sim_country, sim_sub_id}`：通話中又有第二通打進來，手機立刻拒接（AI 模式）。會留下通話紀錄並送出「通話中插播未接」通知。 |
| Bageo → 手機／手機 → Bageo | `set_mode`／`set_mode_result` | `{mode, id}`／`{id, ok, mode, error}`：遠端切換模式；`error` 用上面的代碼。 |
| Bageo → 手機 | `agent_info` | `{console_url}` 給 App 的「**打開網頁控制台**」按鈕用（`public_url` 或區網網址）；透過受信任的 USB 通道時另附 `wifi: {urls: [...]}` 提供 Wi-Fi 配對網址（`ws://<區網-IP>:<PHONE_WIFI_PORT>/ws?token=<key>`，開啟區網名稱時另附 `ws://<名稱>.local:<PHONE_WIFI_PORT>/ws?token=<key>`）。 |
| Bageo → 手機 | `place_call` | `{number, call_id, task_id, sub_id}`：撥出電話（AI 回電或 AI 代打），用 subscription id 為 `sub_id` 的 SIM 撥（找不到就用手機預設的）。忙線時見下方。 |
| 手機 → Bageo | `place_call_ack`、`place_call_failed`、`outbound_ready`、`call_end` | 撥出的電話已撥號／撥不出去／接通／結束。 |
| Bageo → 手機 | `send_sms` | `{id, to, text, sub_id}`：傳簡訊給來電者（行程預約）。欄位限制見下方。 |
| 手機 → Bageo | `send_sms_result` | `{id, ok, error}`，每一段都回報後才送。逾時和重送的規則見下方。 |
| 手機 → Bageo | `send_sms_delivered` | `{id, ok}`：電信有給送達回報時才送（手機最多等一小時）。 |

`hello` 缺少欄位時：

- 沒有 `phone_mode`：比模式切換舊的 App（永遠是 `ai`）。
- 沒有 `supports_sms`：不會要它傳簡訊。
- 沒有 `supports_reject`：黑名單號碼改成接起來播語音。

`place_call`：手機有電話在響或通話中時不撥，回 `place_call_failed`，`reason` 為 `busy`。

`send_sms` 的欄位：

- `id`：1–64 字元。
- `to`：只有數字，開頭可以有 `+`。
- `text`：最多 500 字元。
- `sub_id`：指定 SIM（手機回報的 `sim_sub_id`），沒有就用預設的簡訊 SIM。
- 已經送過的 id 再來一次，只會再回一次結果，不會重複傳。

`send_sms_result` 的規則：

- 手機最多等 60 秒（逾時回 `timeout`）；之後才收到回報，會再送一次真正的結果。
- `error`：`no_permission`、`bad_request`、`no_service`、`radio_off`、`generic_failure`、`timeout`、`exception`。
- Bageo 最多等 90 秒。
- 重新連線後手機會把最近 20 個 id 的結果再送一次，Bageo 用 `id` 去掉重複的。

### Media 連線

| 方向 | 訊息 | 意思 |
|---|---|---|
| 手機 → Bageo | `hello` | `serial`、`caller_id`、`sample_rate`（16000）、`mode` = `SILENT_LOCAL`、`sim_country`、`sim_sub_id`、`model`、`app_version`。撥出電話的 SIM 比對見下方。 |
| 手機 → Bageo | `ringing`、`call_end` | 對方號碼（晚一點才知道時會再送）／通話結束。 |
| Bageo → 手機 | `tts_start`、`tts_end`、`hangup` | 接下來有聲音／聲音結束／掛斷。 |
| Bageo → 手機 | `add_call`、`add_call_abort` | `{id, number, merge, answer_timeout_ms}`／`{id}`：白名單急事轉接時請手機撥給你並合併成三方通話（或取消並把來電者切回通話）。 |
| 手機 → Bageo | `add_call_result` | `{id, status, reason?}`（`status` 為 `dialing`／`active`／`merged`／`failed`）：三方撥號與合併的進度或結果。 |
| 雙向 | 二進位 | 對方的聲音（手機 → Bageo）和 AI 的聲音（Bageo → 手機），格式同上。 |

Bageo 怎麼處理邊界情況：

- 撥出的電話：Bageo 會拿 media `hello` 的 `sim_sub_id` 比對它指定的 SIM（代打不符就是 `wrong_sim`）。
- 簡訊結果在 Bageo 不再等之後才到，照樣會記下來；手機之後用同一個 `id` 送來的真正結果會取代 `timeout`。同樣的結果再來、或已有確定結果的簡訊收到不同結果，都不會改。
- 黑名單號碼只有在 `reject_mode` 是 `decline`、而且手機說過 `supports_reject` 時才叫手機拒接；否則接起來播語音，通知會寫清楚是哪一種。
- AI 回電和 AI 代打都帶 `sub_id`，雙卡手機會用對的號碼撥出。

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
