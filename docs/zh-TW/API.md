# API 參考：用程式控制你的 AI 電話助理

Bageo 網頁控制台做的每件事，都是透過你自己電腦上的一個小型 JSON API 完成的，所以寫個程式也能做到：讀通話紀錄、逐字稿和摘要，改設定，同意或婉拒來電者跟 AI 約的時間，管理聯絡人，把手機在「AI 接聽」和一般手機之間切換，以及叫 AI 替你打電話。這一頁涵蓋 Bageo 1.0 控制台和手機會用到的每一條路由，也包括已 root 的 Android 手機用的 WebSocket 通訊協定。

| | |
|---|---|
| **基底位址** | 主程式監聽的位置，預設 `http://127.0.0.1:8787`。API 路由都以 `/calls/api/` 開頭，例外是 `/health` 和手機用的 WebSocket。控制台本身在 `/calls/`（`/` 會轉址過去）。 |
| **AI 代理** | `POST /mcp` 是 Model Context Protocol 伺服器，有自己的 token，預設關閉：見 [MCP](MCP.md)。 |
| **格式** | UTF-8 的 JSON；路由另有說明的除外（WAV、CSV、vCard、iCalendar）。送 JSON body 時請帶 `Content-Type: application/json`。 |
| **時間** | 通話紀錄：UTC 的 RFC 3339（`2026-11-14T07:02:11Z`）。行程預約：當地時間（`TZ`），格式 `YYYY-MM-DDTHH:MM`。AI 代打：帶時差的當地時間（`2026-11-15T19:00:00+08:00`）。 |
| **相容性** | 欄位名稱來自 [`crates/agent/src`](../../crates/agent/src) 裡的 Rust 結構。之後的版本可能加新欄位，不認得的欄位請直接忽略。 |

## 先試試看

主程式在這台電腦上跑著時，下面幾行可以直接用：

```bash
curl -s http://127.0.0.1:8787/health
curl -s "http://127.0.0.1:8787/calls/api/list?limit=5"
curl -s http://127.0.0.1:8787/calls/api/stats
```

有設定 `CALLS_UI_TOKEN` 的話，加上 `-H "Authorization: Bearer $CALLS_UI_TOKEN"`（見[存取](#存取)）。大家常拿它來做這些事：

| 你想要 | 用這個 |
|---|---|
| 把新的逐字稿匯進筆記或客戶管理系統 | `GET /calls/api/list?has_transcript=true`，再對每一筆 `GET /calls/api/call/{id}` |
| 用程式打電話訂位 | `POST /calls/api/errands`，輪詢 `…/errands/{id}/live`，用 `…/say` 回答 AI |
| 用你自己的按鈕同意預約 | `POST /calls/api/tasks/{id}/confirm` |
| 在家時讓手機照常響、自己接 | `POST /calls/api/devices/{serial}/mode`，body `{"mode": "normal"}` |
| 把任何訊息轉到你的通知管道 | `POST /calls/api/notify/send` |
| 監控主程式 | `GET /health`（不用權杖也能讀） |

## 存取

沒有設定 `CALLS_UI_TOKEN` 時，任何連得到這個連接埠的人都能用 API（預設只有這台電腦自己）。有設定時，每個請求都要用下列其中一種方式帶上權杖：

```text
GET /calls/api/list?token=<token>
Authorization: Bearer <token>
X-Calls-Token: <token>
```

權杖錯誤或沒帶，會得到 HTTP 401，內容是 `{"error": "unauthorized", "code": "unauthorized", "hint": …}`。`/health` 沒帶權杖也會回應，但只給監控用的幾個欄位（見[手機與健康狀態](#手機與健康狀態)）。

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
| 401 | 沒帶權杖或權杖錯誤。 | `unauthorized` |
| 404 | 找不到這通電話、任務、聯絡人、代打，或找不到檔案。 | `not_found`、`task_not_found`、`contact_not_found`、`no_audio`、`no_calendar` |
| 409 | 這個變更跟已存的資料衝突。 | `contact_exists`、`sms_not_retryable` |
| 413／415 | 匯入的檔案太大／格式不認得。 | `file_too_large`、`too_many_rows`、`unsupported_file` |
| 500 | 主程式存不了或讀不了自己的檔案。 | `save_failed`、`contacts_save_failed`、`import_failed` |
| 502 | 外部服務失敗（通知管道、Gemini）。 | `auth_failed`、`network_error`、`ai_failed`、`preview_failed` |

### 防止其他網頁冒用

控制台沒有登入 cookie，但你區網裡某台電腦的瀏覽器開著別的網頁時，那個網頁還是可能偷偷送請求過來。所以凡是會撥電話、傳簡訊、做決定或改動已存資料的路由，一律要求 `Content-Type: application/json`，沒帶就回 HTTP 400 `bad_request`。別的網站要送這個標頭必須先經過 CORS 預檢，而主程式從不回應預檢。適用的路由：

- `POST /calls/api/tasks/{id}/confirm` 和 `…/ignore`；
- `POST /calls/api/appointments/{id}/retry-sms`；
- `POST /calls/api/devices/{serial}/mode`；
- `POST /calls/api/contacts`、`PUT /calls/api/contacts/{id}`、`POST /calls/api/contacts/batch`（`POST /calls/api/contacts/import` 則改成要帶 `X-Filename` 標頭）；
- `POST /calls/api/errands`、`…/{id}/cancel`、`…/{id}/retry`、`…/{id}/say`。

允許空 body 的路由，空 body 當成 `{}`。這是第二道防線；第一道是控制台對區網開放時一定要設 `CALLS_UI_TOKEN`。

## 通話

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/list` | 通話紀錄，最新的在前。查詢參數：`limit`（最多 500；不帶就回全部）、`offset`、`q`（對號碼、手機、事件、逐字稿和摘要做文字搜尋）、`caller`、`serial`、`status`、`has_transcript`、`has_voicemail`。回應：`{ok, calls, total, limit, offset, tasks_by_call, contacts_by_number}`。`tasks_by_call` 是 `{<通話 id>: {id, status, summary}}`，這一頁每通電話最新的一筆任務；`contacts_by_number` 是 `{<caller_id>: {id, name}}`，這一頁裡是聯絡人的來電者。 |
| `GET /calls/api/call/{id}` | 單一通話：`{ok, call, notes, contact, calendar_link, calendar_links, scam_names}`。`contact` 是 `{id, name, note}` 或 `null`。`calendar_links` = `{google, outlook, office365, yahoo}`：各網頁行事曆已填好這通電話行事曆建議的「新增活動」表單（`calendar_link` 是 Google 那個，留給舊版用）。`scam_names`：判斷出的話術類型在主控台各語言的名稱，沒有就是 `null`。 |
| `POST /calls/api/call/{id}/scam-feedback` | 你對這通的標記：JSON `{"verdict": "scam" \| "sales" \| "safe"}`，可另帶內建的 `typology`。記在紀錄上（`scam_feedback`），號碼沒隱藏的話也記在這個號碼上，它之後打來會參考。回 `{ok, call, number}`；HTTP 400 `invalid_verdict`／`invalid_typology`。 |
| `GET /calls/api/call/{id}/event.ics` | 行事曆建議的 iCalendar 檔；這通電話沒有建議時回 HTTP 404 `no_calendar`。用控制台權杖開，或用 `?k=<金鑰>`：通知連結裡帶的是只能開這一個檔案的 HMAC 金鑰，不是權杖。 |
| `GET /calls/api/audio/{id}/listen` | 你當時會聽到的整通電話（對方和 AI 混在一起），WAV，支援 range 請求，可以拖動播放位置。 |
| `GET /calls/api/audio/{id}/remote` · `/tts` · `/vm` · `/audio/{id}` | 只有對方 · 只有 AI · 舊版選單模式的語音留言 · 原始的混音錄音。檔案不存在時回 HTTP 404 `no_audio`。 |
| `GET /calls/api/live` | 進行中的通話和已連線的手機：`{ok, count, calls, control_idle_clients, control_idle_serials, normal_mode_serials}`。`normal_mode_serials` 是已連線、但切成一般手機的手機；網頁不把它們算進待命手機。 |
| `GET /calls/api/stats` | `{ok, stats: {total, today, voicemails, blocked}}`。 |
| `GET /calls/api/export.csv` | 篩選後的清單，CSV 格式：查詢參數同 `list`，最多 2000 列（`limit` 可以再調低）。標頭 `X-Calls-Export-Total` 和 `X-Calls-Export-Rows` 分別是符合幾筆、寫出幾筆。 |

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
| `events` | 依序發生的事，例如 `active`、`persona_stall_spam`（判定是推銷或詐騙）、`persona_scam_detected`（陪聊關閉時判定是推銷或詐騙）、`persona_stall_released`（之後確認是正常電話、改回正常應對）、`reject_list`（拒接號碼）、`call_waiting_rejected`（通話中拒接的第二通）、`ai_callback`（行程預約的 AI 回電）、`ai_errand`（AI 代打）、`persona_ai_fail_hangup`、`ended`。 |
| `scam` | 判定是推銷或詐騙時才有：`{kind, typology, confidence, source, released}`。`kind` 是 `scam` 或 `sales`，`typology` 是內建的話術類型；`source` 是 `review`（複審判的）、`hard_words`（複審沒回答，但對方講了只有詐騙才會講的話）或 `keywords`（複審一直失敗，只靠關鍵字）；`released` = 之後再看一次，放回正常應對。 |
| `scam_feedback` | 你在通話頁的標記：`scam`、`sales` 或 `safe`。 |
| `summary` | 寫好後的摘要文字。 |
| `appointment` | 對方跟 AI 約了時間時：通話結束當下的預約（見[行程預約與任務](#行程預約與任務)）。之後的變化（你的決定、簡訊和回電結果）更新在 `notes.appointment`。 |
| `sim_country`、`sim_sub_id` | 這通電話是哪張 SIM 卡接的（手機有回報時）。 |
| `audio_path`、`remote_audio_path`、`tts_audio_path`、`voicemail_path` | 有哪些錄音檔（資料夾裡的路徑）。 |

### 摘要、逐字稿與翻譯

摘要寫好之前 `notes` 是 `null`，之後包含：

| 欄位 | 意思 |
|---|---|
| `summary` | 用通知語言寫的摘要。 |
| `dialogue` | 逐字稿，一行一次發言，開頭是主程式語言的說話者標籤：繁體中文 `對方：`／`接聽：`，簡體中文 `对方：`／`接听：`，英文 `Caller: `／`AI: `，日文 `相手：`／`AI：`，韓文 `상대: `／`AI: `，西班牙文 `Llamante: `／`IA: `，法文 `Appelant : `／`IA : `，德文 `Anrufer: `／`KI: `，葡萄牙文 `Chamador: `／`IA: `。舊版存的紀錄不論語言都是 `對方：`／`接聽：`。每一行保留當時講的語言。 |
| `dialogue_translations` | 有翻譯時才出現：跟 `dialogue` 一行對一筆（同樣的索引）。別種語言講的那一行，這裡是翻成通知語言的譯文；本來就是你的語言、太短或沒翻的是 `null`。網頁控制台把譯文顯示在原文下面。 |
| `recording_text` | 對方聲音另外做的語音轉文字，當備份（AI 陪聊拖時間的推銷、詐騙電話是空的）。 |
| `end_hint`、`source`、`updated_at` | 通話怎麼結束、誰寫的、什麼時候寫的。 |
| `calendar` | 通話中講定了日期時：`{title, start, end, all_day}`（當地時間，`YYYY-MM-DDTHH:MM`）。 |
| `appointment` | 預約目前的狀態。 |

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
| `GET /calls/api/settings` | `{ok, settings, storage_path, console_url, console_ip_url}`。`console_url` 是目前用的控制台網址（你填的，或自動帶入的；只有這台電腦能開時是空的），`console_ip_url` 是把 `.local` 名稱換成區網 IP 的同一個網址。機密欄位以遮罩回傳（`"abc…wxyz"`），附一個 `<name>_set` 旗標；通知管道回傳時 `url`／`token` 是空的，另外附 `url_hint`、`token_hint`、`url_set`、`token_set`。唯讀的輔助欄位：`default_prompt`、`voices`、`ambience_kinds`、`languages`、`countries`（「各 SIM 卡的主要語言」可選的國家代碼）、`seen_sim_countries`（主程式啟動後手機回報過的 SIM 卡國家）、`notify_kinds`、`sample_lines`、`summary_ready`。 |
| `PUT /calls/api/settings` | 一個 JSON 物件，只放要改的欄位。回應同 `GET`。下一通電話就生效，不用重新啟動。 |

值得知道的欄位（全部都在 [`settings_store.rs`](../../crates/agent/src/settings_store.rs) 的 `RuntimeSettings` 和[設定參考](SETTINGS.md)裡）：

| 欄位 | 值 |
|---|---|
| `owner_name` | 你的名字，用在 `{owner}`。 |
| `owner_aliases` | 別人怎麼叫你，最多 10 個，每個最多 40 字。空白的、重複的和跟 `owner_name` 相同的會被拿掉。 |
| `answer_style` | `assistant`／`persona`（AI 以你本人身分接）。 |
| `errand_identity` | `assistant`（預設）／`owner`：AI 代打時說自己是誰。其他值存成 `assistant`。 |
| `summary_provider` | `gemini`（預設）／`custom`。`custom` 用 `summary_api_base`、`summary_model`（填了網址就必填）和 `deepseek_api_key`（那家服務的金鑰）。舊的 `auto`／`deepseek` 會在設定載入時轉換一次。 |
| `calendar_app` | `google`、`outlook`、`office365`、`apple`、`yahoo`、`ics` 或空白（還沒選）。其他值存成空白。 |
| `appointment_mode` | `off`（預設）／`confirm`。舊的 `auto` 當作 `confirm`。 |
| `appointment_sms` | `true`／`false`：要不要傳簡訊告訴對方你的決定。 |
| `appointment_reply` | `auto`（預設）、`sms` 或 `call`。 |
| `reject_numbers` | `{number, reason}` 的清單。 |
| `reject_mode` | `decline`（預設：響鈴時直接拒接）或 `announce`（接起來播語音）。 |
| `whitelist_mode` | `off`（預設：照常由 AI 接聽）、`direct`（直接掛斷觸發電信忙線轉接並推播通知）或 `emergency`（AI 接聽並於偵測到緊急事件時告知 `whitelist_forward_number` 並發送緊急警報）。 |
| `whitelist_forward_number` | 白名單直接轉接或緊急事件轉接時使用的真實電話號碼。 |
| `ui_password` | 外部存取網頁控制台與 API 的密碼（讀取時遮罩；`"ui_password_clear": true` 可清空）。 |
| `country_code` | 只有數字（`886`、`1`）。控制台不顯示；這裡有值時優先於 `COUNTRY_CODE`。 |
| `gemini_thinking_level` | `low`（預設）／`medium`／`high`，只對 thinking 模型有效。 |

`PUT` 的規則：

- 機密欄位（`gemini_api_key`、`deepseek_api_key`）沒帶或是 `""` 時，保留原本存的值；`"<name>_clear": true` 會清空它。
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
| `POST /calls/api/notify/test` | Body `{"channel": {kind, url, token, target, id?}}`。把測試訊息傳到那一個管道（已存的機密依 `id` 補上）。回 HTTP 200 `{ok: true}`、400 `unknown_channel`，或 502 `{ok: false, code, field, kind, error}`：`code` 是出錯原因（`missing_field`、`bad_url`、`bad_value`、`auth_failed`、`service_not_found`、`http_error`、`service_error`、`network_error`、`send_failed`），`field` 是 `url`／`token`／`target` 哪一欄（或空白），`error` 是該服務的回應。 |
| `POST /calls/api/notify/send` | Body `{"title": "...", "body": "..."}`。把你的訊息傳到每一個啟用的管道（`title` 空白就用「Bageo」；`body` 空白回 HTTP 400 `empty_body`）。只要有一個管道接受就回 `{ok: true}`。[簡訊轉發工具](../../tools/sms_forward/README.md)（英文）就是用這條路由。 |
| `GET /calls/api/quick-action` · `POST /calls/api/quick-action` | 32 種通知管道內附的單次簽章互動按鈕路由（`?act=confirm\|ignore\|block\|stall\|white&target=<id或號碼>&sig=<hmac>`）。透過 HMAC-SHA256 驗證，在手機通知中點開即生效，支援全部 9 種語言。 |
| `POST /calls/api/control/inbound` | 跨平台通用指令 Webhook（支援 Slack、Discord、LINE、企業微信、釘釘、飛書、Mattermost、Rocket.Chat、Zulip、Matrix、Nextcloud Talk、Synology Chat、Home Assistant、n8n 或 curl：`{"text": "清單 5", "user": "owner", "channel": "line"}`）。可執行全部 18 個聊天指令（包含 `block`、`stall`、`white`、`unblock`）並以通知語言回傳 `{ok, text}`。 |
| `GET /calls/api/auth-status` · `POST /calls/api/initial-password` | 查詢是否需要密碼與是否首次設定密碼（`{ok, password_required, needs_initial_password_setup}`），以及首次建立存取密碼（`{"password": "..."}`，僅在尚未設定任何密碼／Token 時可用）。 |

你自己的 webhook 會收到的 JSON 和每一種事件，在[通知](NOTIFICATIONS.md#webhook-json)裡有說明。

## 語音與背景音試聽

`GET /calls/api/preview` 回傳一段 WAV：用某個聲音講幾句電話用語，背後墊著背景音。每個查詢參數沒帶時都用已存的設定：

| 參數 | 意思 |
|---|---|
| `voice` | 聲音名稱，或 `none` 表示只要背景音。 |
| `ambience` | `off` 或某一種背景音。 |
| `level` | 1–3。 |
| `regen=true` | 重新產生，不用已存的範例。 |
| `clip=reject` | 用這個聲音念的拒接號碼語音。 |
| `lang` | `ja`、`zh-TW`… |

內建的 30 個聲音在 9 種語言都已預先錄好試聽和拒接語音（`assets/voices/` 裡共 1,080 段），所以試聽不用連 Gemini；只有 `regen=true` 或沒有預錄的聲音才會開一次連線。錯誤：400 `unknown_voice`／`unknown_background`、404 `background_missing`（背景音檔沒載入）、502 `preview_failed`。

## 行程預約與任務

有人打來說想約星期六下午三點來看房。`appointment_mode` 設成 `confirm` 時（見[設定](SETTINGS.md#摘要與行事曆)），AI 會記下時間但不先答應，通話結束後留下一筆任務（`intent` 為 `appointment`）等你同意或不同意。同一個 `appointment` 物件會出現在通話紀錄、它的 notes 和任務上：

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
| `title`、`caller_name` | 約的是什麼事、對方怎麼自稱（對方自己的說法）。 |
| `status` | `pending`（等你決定）、`confirmed`、`declined`；`booked` 只會出現在舊版的紀錄。 |
| `reply` | 怎麼告訴對方：`sms`、`call` 或 `none`。 |
| `sms` | `sent`，或簡訊沒傳出的原因：`offline`（手機沒連線）、`old_app`（App 不能傳簡訊）、`timeout`、`busy`（同一則簡訊正在傳）、`no_number`（收不到簡訊的號碼），或手機回的代碼：`no_permission`、`bad_request`、`no_service`、`radio_off`、`generic_failure`、`exception`。網頁和 Telegram 回覆會寫成白話。 |
| `call` | AI 回電：`queued`（排在打給同一個號碼的前一通回電後面）、`dialing`、`connected`（手機已撥號，AI 等對方接起來出聲）、`answered`（對方已得知結果）、`no_answer`（70 秒內沒人出聲）、`failed`、`done`（通話結束、結果不明），或沒撥出的原因：`no_phone`、`send_failed`、`no_number`、`not_callable`（國外、免付費、加值付費或特殊短碼）。手機的回報也記在任務的 `events`（`callback_place_call_ack`、`callback_place_call_failed`、`callback_outbound_ready`、`callback_done`、`callback_failed`）。 |
| `call_lang` | 那通電話講的語言（`zh-TW`、`en`…）。AI 回電就講這個語言，等對方接起來出聲後才說你的回覆，只說一次。沒有這個欄位的舊紀錄照設定 `language`。 |
| `tg_card` | 發給這筆預約的 Telegram 卡片 `{chat_id, message_id}`；你在別處決定後，用來拿掉卡片上的按鈕。 |
| `tag` | 這筆預約固定的 id。 |

空的欄位不會出現。

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/tasks` | 任務，最新的在前（最多 200 筆）：`{ok, tasks}`。預約任務另外帶 `calendar_links`（`{google, outlook, office365, yahoo}`）；`reply` = `{sms, call, default}`（這位來電者能不能收簡訊、能不能回電，以及「預設回覆方式」會選哪個）；`sms_route` = `{serial, label}`，你沒另選手機時簡訊從哪支發（接這通電話的手機；`label` 是它在 `device_labels` 的名稱，沒有就是 id）；來電者是聯絡人時還有 `contact_name`。網頁上的「待處理事項」就是 `appointment.status` 為 `pending` 的任務。 |
| `GET /calls/api/tasks/{id}/event.ics` | 預約的 iCalendar 檔（Apple 日曆和其他 App）。用控制台權杖或連結裡的 `?k=<HMAC 金鑰>` 開；任務沒有預約時回 HTTP 404 `no_calendar`。 |
| `POST /calls/api/tasks/{id}/confirm` | **同意**，跟在網頁或 Telegram 卡片上按一樣。Body：空的、`{}` 或 `{"reply": "sms" \| "call" \| "none"}`。沒帶 `reply` 時照「預設回覆方式」；選了這位來電者用不了的方式就不通知。回應 `{ok, task, message}`：`message` 是白話寫的結果，跟 Telegram Bot 回的同一段，附上你選的 `calendar_app` 的「加入行事曆」連結（還沒選時是這通電話在網頁的頁面，在那裡按「加入行事曆」選一家；沒有控制台網址就不附）。傳簡訊最多可能等 90 秒；回電交給手機撥出（在 `appointment.call` 看進度）。 |
| `POST /calls/api/tasks/{id}/ignore` | **不同意**：用同樣的方式告訴對方，客氣婉拒。body、回應和規則同上。 |
| `POST /calls/api/appointments/{id}/retry-sms` | 已決定的預約簡訊沒傳出時再傳一次。Body：空的、`{}` 或 `{"serial": "<手機>"}`（換另一支已連線的手機；沒帶就是接這通電話的那支）。回應 `{ok, sms, message}`：傳出去了 `ok` 才是 true，`sms` 是新的代碼。還在等、已經傳出或本來就不能傳（`no_number`）時回 HTTP 409 `sms_not_retryable`（附目前的 `sms`）；404 `task_not_found`；400 `invalid_serial`／`bad_request`。 |

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

**通話中。** AI 遇到你的交代沒講清楚的事，會跟對方說要確認一下，然後問你（`ask_owner` 函式）。你正在看這筆代打時（6 秒內有人讀過它的即時畫面），問題會顯示在那裡，最多等 40 秒。沒人在看、或你沒來得及回答，AI 會問清楚哪些選項可以，說你確認後再回電，然後客氣地結束通話。等答案的時候，AI 不會先替這個問題做決定；對方先說再見的話，問題就到此為止。你在通話中打的字會當成指示交給 AI。

**通話後。** 摘要模型會對照交辦內容讀這通電話，你會收到一則通知（標題「AI 代打電話：<名字或號碼>」，用通知語言；webhook 事件 `ai_errand`）。AI 在你沒回答的問題上自己答應的事，結果一定不是 `done`，而是 `need_owner`，提醒你確認，不合適就回電改。

**用誰的身分打。** 設定 `errand_identity`（預設 `assistant`，你的助理；或 `owner`，你本人），每筆代打也可以各自指定。不論哪一種，被認真問到時 AI 都會承認自己是 AI，不透露交辦內容以外的個人資料，也不答應任何付款、訂金或轉帳。

**能打哪些號碼。** 不打緊急號碼、短碼（7 碼以下）或加值付費號碼（例如台灣 020x 像 0204、0209，+1 900／976，日本 0990，英國 09…）；免付費號碼可以打。沒有加號的號碼當成你所在國家的號碼（`country_code`）。用國際冠碼開頭的號碼（`00`、台灣的 `01x` 和 `00x`、北美的 `011`、日本的 `010`）一律 `not_allowed`：國外號碼請用 `+` 寫。台灣號碼長途冠碼 0 後面接 1 的也是 `not_allowed`。

**用哪支手機、哪張卡。** 電話從一支已連線、在 AI 模式、而且有那個號碼所屬國家 SIM 卡的手機撥出。主程式從手機回報的來電學會每支手機有哪些 SIM（subscription id 和國家；來自 `incoming` 和 media `hello` 裡的 `sim_sub_id`、`sim_country`），也可以從 control `hello` 裡選填的 `sims: [{sub_id, country}]` 清單得知（1.0 版 App 還不會送）。這些記在 `phone_sims.json`。主程式從沒聽過 SIM 的手機不接代打（`sim_unknown`）：先打一通電話給那支手機。手機收到的是 E.164 格式的號碼和要用的 SIM（`sub_id`）。那通電話自己的 media `hello` 顯示用的是別張 SIM、或沒回報時，主程式會在 AI 開口前掛斷（`failed`、`call_code` `wrong_sim`，會通知你），並把那張卡從這支手機的紀錄裡拿掉，等之後有來電回報它再加回來。

**排隊與上限。**

| 情況 | 會怎樣 |
|---|---|
| 手機正在通話 | 代打排隊（`queued`），超過 10 分鐘算失敗（`phone_busy`）。 |
| 同一個號碼還有另一通 | 一個號碼一次只打一通，跟行程預約的 AI 回電共用排隊。排在後面超過 25 分鐘算失敗（`number_busy`）。 |
| 通話長度 | 最長 10 分鐘；9 分半開始收尾。 |
| 語音選單 | AI 最多等 90 秒等真人來接；它不能按鍵（`keypad_menu`）。 |
| 電信錄音（「您撥的號碼是空號」） | 直接掛斷（`carrier_message`）。 |
| 沒人接或失敗 | 不會自己重撥：要不要「再打一次」由你決定。 |
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
| `call_code` | 撥號進行到哪裡：`dialing`、`queued`、`answered`、`no_answer`、`failed`、`phone_busy`、`number_busy`、`no_phone`、`send_failed`、`keypad_menu`、`carrier_message`、`wrong_sim`、`agent_restarted`、`cancelled`。 |
| `attempts` | 從 1 開始，每按一次「再打一次」加 1。 |
| `record_id` | 這一次的通話紀錄（`GET /calls/api/call/{id}`：逐字稿和錄音）。 |
| `follow_up_of` | 這通是接續哪一筆先前的代打；那一筆最後 20 行逐字稿會放進 AI 的腳本。 |
| `questions` | `[{id, text, options, answer, answered_by, asked_at}]`：AI 問過你的問題，選項最多 4 個。`answered_by`：`owner`（你回答了）、`timeout`（你在看但沒來得及回答）、`absent`（沒人在看）；還在等時是 `null`。 |
| `result` | `null` 或 `{outcome, summary, details, follow_up, options, event, unconfirmed}`。`outcome`：`done`、`partial`、`not_done`、`need_owner`（選項在 `options`，最多 8 個）、`no_answer`、`voicemail`、`failed`。`event`：`null` 或 `{start, end, title}`（帶時差的當地時間；沒談定結束時間就沒有 `end`）。`unconfirmed`：`details` 或 `event` 裡 AI 複誦時對方沒有明確確認的時間、日期、號碼或名字（最多 8 項）；沒有就不出現。 |
| `history` | 先前幾次的嘗試，`[{attempt, status, result, record_id, ended_at, questions}]`。 |

代打存在資料夾裡的 `errands.json`（保留最新 500 筆）。主程式重新啟動時還在進行中的代打會變成 `failed`（`call_code` `agent_restarted`）。

### 代打路由

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/errands` | `{ok, errands}`，最新的在前，最多 200 筆。 |
| `POST /calls/api/errands` | 開始一筆。Body `{number, name, contact_id, task, identity, serial, follow_up_of}`。必填：`task`，以及號碼（或給聯絡人、先前的代打，用它們的號碼）。`identity` 留空就照 `errand_identity`；`serial` 留空就用任何合適的手機。回應 `{ok, errand}`，狀態是 `dialing`，手機或號碼忙線時是 `queued`。 |
| `POST /calls/api/errands/{id}/cancel` | 排隊中：取消。撥號中或通話中：掛斷。已經結束的原樣回傳。`{ok, errand}`。 |
| `POST /calls/api/errands/{id}/retry` | 同一筆再打一次（`attempts` 加 1，上一次移到 `history`）。`{ok, errand}`。還在進行中（或上一通還沒結束）：400 `busy`；手機相關的代碼同新增。 |
| `DELETE /calls/api/errands/{id}` | 從清單刪掉（通話紀錄會留著）。`{ok}`；撥號中或通話中：400 `busy`。 |
| `GET /calls/api/errands/{id}/live?after=<seq>` | `{status, lines, question, watching}`。`lines`：`seq` 之後的字幕，`[{seq, who, text, at}]`，`who` 是 `ai`、`callee`、`owner` 或 `system`（保留最新 400 行；1.2 秒沒有新字就算一行結束）。`question`：`null` 或 `{id, text, options, asked_at, expires_at}`，等你回答的問題。`watching`：你是否算正在看。每讀一次就算你正在看 6 秒，所以跟著通話時每隔幾秒讀一次。 |
| `POST /calls/api/errands/{id}/say` | Body `{text, question_id}`（最多 300 字）。帶 `question_id`：回答那個問題。沒帶：有問題在等就當成它的答案，否則當成給 AI 的指示（「跟他說 8 點也可以」）。`{ok}`。 |

錯誤代碼（HTTP 400 `{ok: false, error, code}`，兩者同值）：

| 代碼 | 路由 | 意思 |
|---|---|---|
| `no_number` | 新增 | 沒有號碼，也沒有可以拿號碼的聯絡人或先前代打。 |
| `bad_number` | 新增 | 不是電話號碼（有字母、`+` 不在開頭、超過 15 碼、以那個國家來說太短）。 |
| `not_allowed` | 新增 | 緊急號碼、短碼、加值付費號碼，或用國際冠碼代替 `+`。 |
| `no_task` | 新增 | `task` 是空的。 |
| `task_too_long` | 新增 | `task` 超過 1,000 字。 |
| `no_phone` | 新增、再打 | 沒有已連線、在 AI 模式的手機（或指定的 `serial` 不是這樣的手機）。 |
| `sim_unknown` | 新增、再打 | 沒有合適的手機，而且至少有一支的 SIM 主程式還不知道：先打給它一通。 |
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

聯絡人替打來的號碼配上名字，存在通話紀錄資料夾（`CALLS_DATA_DIR`）的 `contacts.json`。號碼不管怎麼寫都對得上：`+886912345678`、`0912-345-678`、`886912345678` 是同一支。沒有國碼的號碼照你的國家（`country_code`）解讀；來電則照接到這通電話的 SIM 卡所屬國家。聯絡人也用在 AI 代打（選聯絡人就不用打號碼）和推銷判斷：已存的聯絡人講到錢，不會被當成推銷或詐騙。

一位聯絡人是 `{id, name, phones: [...], note, kind, source, updated_at}`，最多 10 個號碼，名字最多 80 字、備註最多 500 字。`kind` 為 `contact`（預設一般聯絡人）、`whitelist`（白名單 VIP／家人，搭配 `whitelist_mode` 使用）、`blacklist`（黑名單，來電直接拒接）或 `stall`（陪聊名單，來電直接進入 AI 陪聊反制模式）。`source` 是 `manual`、`vcf`、`csv`、`text` 或 `ai`（Gemini 讀出來的）。一個號碼只屬於一位聯絡人；通訊錄最多 10,000 位（`too_many_contacts`）。

| 方法與路徑 | 內容 |
|---|---|
| `GET /calls/api/contacts` | 查詢參數 `q`（名字、備註或號碼，號碼怎麼寫都行）、`kind`（`contact` \| `whitelist` \| `blacklist` \| `stall`）、`limit`、`offset`。回應 `{ok, contacts, total, offset, count}`：符合的結果（共 `total` 筆），以及全部聯絡人數（`count`）。 |
| `POST /calls/api/contacts` | Body `{name, phones: [...], note, kind?}`，回應 `{ok, contact}`。號碼已經是別的聯絡人的：HTTP 409 `contact_exists`，附 `contact: {id, name}`。沒有名字或沒有號碼：400 `invalid_contact`，附出問題的 `field`。 |
| `PUT /calls/api/contacts/{id}` | 同樣的 body，取代名字、號碼、備註和名單分類 `kind`。404 `contact_not_found`，409、400 同上。 |
| `DELETE /calls/api/contacts/{id}` | `{ok}`。 |
| `POST /calls/api/contacts/import` | Body 就是檔案本身（最大 10 MB），檔名 URL 編碼後放在 `X-Filename`（沒帶回 400 `bad_request`）。只讀、**不存**（預覽）：`{ok, file, format, ai, entries: [{name, phones, note, source, duplicate}], skipped}`。`format`：`vcf`、`csv`、`text`、`xlsx`、`docx`、`pdf` 或 `image`。`ai`：是 Gemini 讀的。`duplicate`：`null`，或這筆重複的已存聯絡人 `{id, name, by: "phone" \| "name"}`。`skipped`：沒有名字或號碼而略過的筆數。錯誤：400 `empty_file`、413 `file_too_large`／`too_many_rows`、415 `unsupported_file`、400 `need_gemini_key`、502 `ai_failed`（Gemini 的回覆在 `error`）。 |
| `POST /calls/api/contacts/batch` | Body `{contacts: [{name, phones, note, source}], duplicates: "merge" \| "skip"}`：匯入時你要留的那些。跟已存聯絡人重複的，合併進去（補上新號碼和備註；預設）或略過。回應 `{ok, added, merged, skipped, count}`。 |
| `GET /calls/api/contacts/export.vcf` | 全部聯絡人匯出成 vCard 3.0 檔。 |

主程式自己讀得懂的格式：vCard 2.1、3.0、4.0（含 quoted-printable、各種 `CHARSET`）；有表頭（Google 通訊錄、Outlook，或欄名像 *姓名*、*電話*、Name、Phone）或沒有表頭的 CSV、TSV，編碼 UTF-8、UTF-16 或電腦的編碼（Big5、GBK、Shift_JIS、EUC-KR、Windows-1252）；一行一個名字加號碼的純文字；`.xlsx`、`.docx` 裡的表格。PDF、圖片（印出來的名單照片、通訊錄截圖）和讀不出人的檔案交給 Gemini 的視覺模型（`gemini-flash-lite-latest`，用設定裡的 Gemini 金鑰）。

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
| `GET /calls/api/devices` | `{ok, usb: {enabled, adb_found, adb_path, error, phones: [{serial, state, model, reverse, app_installed}]}, connected: [...], labels: {<phone id>: <名稱>}, calls_in_progress, phones: [...]}`。最外層的 `phones` 是主程式啟動後在 control 連線上見過的每支手機：`{serial, connected, mode, supports_mode, model, app_version, in_call, last_seen_ms}`。`mode` 是 `ai`（AI 接聽）或 `normal`（一般手機，見[手機設定](PHONE.md#一般手機模式)）；比這個功能舊的 App，`supports_mode` 是 false，永遠是 `ai`。 |
| `POST /calls/api/devices/{serial}/mode` | Body `{"mode": "ai" \| "normal"}`。叫手機切換，最多等 15 秒等它回覆。回 `{ok, mode, error_code}`（400 時另附說明 `error`）。 |
| `GET /health` | 一個很大的扁平 JSON 物件，排查問題用：版本、語言、哪些東西已設定、已連線的手機、進行中的通話；不含金鑰或權杖。`control_idle_clients`／`control_idle_serials` 算的是所有連上的手機；`normal_mode_serials` 列出切成一般手機的那幾支；`local_time` 是主程式依 `TZ` 的時鐘。有設定控制台權杖、請求卻沒帶時，只回 `ok`、`version`、`language`、`notify_language`、`active_call_count` 和 `control_idle_clients`，監控用已經足夠。 |

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

手機透過 WebSocket 連到 `/` 或 `/ws`（兩者相同）。每支手機維持一條長期的 *control* 連線，每通電話另開一條 *media* 連線。兩種連線都雙向傳送 JSON 文字訊框和原始的 16 kHz 單聲道 16 位元小端序 PCM 二進位訊框（手機每 60 ms 送一框）。從這台電腦本身來的連線（USB 通道）一律接受；其他連線在有設定 `DEVICE_TOKEN` 時必須帶上 `?token=<DEVICE_TOKEN>`。

逐欄位的完整訊息列表在 [`android/README.md`](../../android/README.md#websocket-protocol)（英文）。任何會講這個通訊協定的程式都能代替手機，所以要加上 SIP 或軟體電話前端，完全不用動通話邏輯。雙方都會忽略不認得的東西，所以舊 App、舊主程式照樣能用。

### Control 連線

| 方向 | 訊息 | 意思 |
|---|---|---|
| 手機 → 主程式 | `hello` | `serial`、`mode` = `CONTROL_IDLE`、`model`、`app_version`、`phone_mode`（`ai`／`normal`）、`in_call`、`supports_sms`、`supports_reject`。沒有 `phone_mode`：比模式切換舊的 App（永遠是 `ai`）。沒有 `supports_sms`：不會要它傳簡訊。沒有 `supports_reject`：拒接號碼改成接起來播語音。 |
| 手機 → 主程式 | `ping` | 每 15 秒一次。 |
| 手機 → 主程式 | `status` | `{mode, in_call}`：模式或通話狀態變了。 |
| 手機 → 主程式 | `incoming`、`incoming_cancel` | 有電話在響（`caller_id`，未顯示號碼時是空字串；`sim_country`；知道時帶 `sim_sub_id`）／接起來之前就結束了。 |
| 主程式 → 手機 | `answer_plan` | `{delay_ms, prewarm, reject}`：響多久再接，或 `reject: true` 響鈴中直接拒接（`reject_mode` 為 `decline` 的拒接號碼）。 |
| 手機 → 主程式 | `call_waiting_rejected` | `{serial, caller_id, sim_country, sim_sub_id}`：通話中又有第二通打進來，手機立刻拒接（AI 模式）。會留下通話紀錄並送出「通話中插播未接」通知。 |
| 主程式 → 手機／手機 → 主程式 | `set_mode`／`set_mode_result` | `{mode, id}`／`{id, ok, mode, error}`：遠端切換模式；`error` 用上面的代碼。 |
| 主程式 → 手機 | `agent_info` | `{console_url}`：給 App 的「**打開網頁控制台**」按鈕。主程式知道網址時才在 hello 之後送：`public_url`，或控制台對區網開放時的區網 IP 網址。 |
| 主程式 → 手機 | `place_call` | `{number, call_id, task_id, sub_id}`：撥出電話（AI 回電或 AI 代打），用 subscription id 為 `sub_id` 的 SIM 撥（找不到那張 SIM 就用手機預設的）。手機有電話在響或通話中時不撥，回 `place_call_failed`，`reason` 為 `busy`。 |
| 手機 → 主程式 | `place_call_ack`、`place_call_failed`、`outbound_ready`、`call_end` | 撥出的電話已撥號／撥不出去／接通／結束。 |
| 主程式 → 手機 | `send_sms` | `{id, to, text, sub_id}`：傳簡訊給來電者（行程預約）。`id` 1–64 字元；`to` 只有數字，開頭可以有 `+`；`text` 最多 500 字元；`sub_id` 指定 SIM（手機回報的 `sim_sub_id`），沒有就用預設的簡訊 SIM。已經送過的 id 再來一次只會再回一次結果，不會重複傳。 |
| 手機 → 主程式 | `send_sms_result` | `{id, ok, error}`，每一段都回報後才送。手機最多等 60 秒（逾時回 `timeout`）；之後才收到回報，會再送一次真正的結果。`error`：`no_permission`、`bad_request`、`no_service`、`radio_off`、`generic_failure`、`timeout`、`exception`。主程式最多等 90 秒。重新連線後手機會把最近 20 個 id 的結果再送一次，主程式用 `id` 去掉重複的。 |
| 手機 → 主程式 | `send_sms_delivered` | `{id, ok}`：電信有給送達回報時才送（手機最多等一小時）。 |

### Media 連線

| 方向 | 訊息 | 意思 |
|---|---|---|
| 手機 → 主程式 | `hello` | `serial`、`caller_id`、`sample_rate`（16000）、`mode` = `SILENT_LOCAL`、`sim_country`、`sim_sub_id`、`model`、`app_version`。撥出的電話，主程式會拿 `sim_sub_id` 比對它指定的 SIM（代打不符就是 `wrong_sim`）。 |
| 手機 → 主程式 | `ringing`、`call_end` | 對方號碼（晚一點才知道時會再送）／通話結束。 |
| 主程式 → 手機 | `tts_start`、`tts_end`、`hangup` | 接下來有聲音／聲音結束／掛斷。 |
| 雙向 | 二進位 | 對方的聲音（手機 → 主程式）和 AI 的聲音（主程式 → 手機），格式同上。 |

主程式怎麼處理邊界情況：

- 簡訊結果在主程式不再等之後才到，照樣會記下來；手機之後用同一個 `id` 送來的真正結果會取代 `timeout`。同樣的結果再來、或已有確定結果的簡訊收到不同結果，都不會改。
- 拒接號碼只有在 `reject_mode` 是 `decline`、而且手機說過 `supports_reject` 時才叫手機拒接；否則接起來播語音，通知會寫清楚是哪一種。
- AI 回電和 AI 代打都帶 `sub_id`，雙卡手機會用對的號碼撥出。

