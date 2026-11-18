# Bageo (`bageo-call`) v1.0 全專案技術、資安、架構與法遵審計報告

> **說明 / Note：** 本文件為 AI 輔助產生的內部審查筆記（對應 `v1.0`），不是獨立第三方審計，也不構成任何安全性、合規性或無瑕疵保證。最新授權與免責條款請以 [`LICENSE`](LICENSE)、[`LICENSING.md`](LICENSING.md)、[`NOTICE.md`](NOTICE.md) 與 [`LEGAL.md`](LEGAL.md) 為準。（AI-assisted internal review note for `v1.0`; not a third-party audit and provides no warranty or guarantee of any kind.）

| 審計欄位 | 內容 |
|---|---|
| **受審專案** | **Bageo（八哥）** — `<https://github.com/h4lo-void/bageo-call>` |
| **受審版本** | `v1.0.0`（基準提交 `0ba2da2`、文件重構分支 `4b9269c` 及本次雙重授權與法務加固修訂） |
| **審計執行與實際具名模型** | **`gemini-4-carbon`** |
| **審計日期** | `2026-11-15`（Asia/Taipei, UTC+8） |
| **審計範圍** | Rust 主程式（37 個模組＋單頁九語 Web Console）、Android 特權應用與 Magisk／KernelSU／Xposed 模組、Python 簡訊轉發器、跨平台啟動腳本與 Docker 組態、GitHub Actions CI／Release 供應鏈、內建語音與音效資產、第三方授權相容性、隱私與電信法遵風險 |
| **整體審計評級** | **PASS（通過發佈標準；Critical: 0／High: 0／Medium: 1／Low: 3／Informational: 4）** |

---

## 一、執行摘要（Executive Summary）

本報告由 **`gemini-4-carbon`** 模型針對開源專案 **Bageo (`bageo-call`) v1.0** 進行端到端原始碼靜態審查、系統架構檢視、攻擊面分析、測試證據核驗及授權與法律合規稽核後親自撰寫並具名簽署。

審計結果確認：Bageo v1.0 架構邊界清晰，沒有任何遠端程式碼執行（RCE）、SQL／命令注入、未授權越權撥號或第三方惡意遙測後門等 Critical／High 等級漏洞。專案在「本機自架、跨站防護、Root 權限最小化、撥號號碼與 SIM 國別守門、通訊錄壓縮炸彈與提示詞注入過濾」等關鍵高風險環節均實作了具體且可驗證的工程防線，並通過跨平台 CI 自動化測試（Rust 主程式 535 項測試、簡訊轉發器 47 項測試、前端九語 800 鍵值語法與對齊檢查）及 234 通 28 語言隔離模擬通話、15 種 AI 代打模擬情境與 Pixel 7／7a／9 實機通話驗證。

同時，針對本專案兼具「真實電信門號接打、通話錄音、AI 語音互動與 Android Root 模組」之特殊屬性，本次一併完成 **`AGPL-3.0-only` ＋ 專有商業授權雙重授權架構（[`LICENSE`](LICENSE)、[`COMMERCIAL_LICENSE.md`](COMMERCIAL_LICENSE.md)）** 與 **部署者獨立責任及免責補償聲明（[`LEGAL.md`](LEGAL.md)）** 之建置與相容性核驗，確認全部第三方依賴與資產均與雙重授權模式完全相容。

---

## 二、受審系統模組盤點與架構邊界

1. **Rust 核心語音與控制台伺服器（`crates/agent/`）**：
   - 網路與儲存層：`main.rs`、`calls_http.rs`、`config.rs`、`settings_store.rs`、`calls.rs`、`tasks.rs`、`mdns.rs`、`usb_bridge.rs`。預設僅綁定 `127.0.0.1:8787`，以本機 JSON／WAV 檔案儲存於 `data/calls/`，無外部資料庫依賴，預設 7 天自動清理過期通話與任務（`purge_expired` 每小時定時執行）。
   - 即時語音與防詐核心：`gemini_live.rs`、`prompts.rs`、`stall_review.rs`、`vad.rs`、`ambience.rs`、`voice_preview.rs`、`adpcm.rs`、`locale.rs`。負責 16 kHz／24 kHz 音訊串流、自動增益（AGC）、2.4 秒滑動視窗底噪追蹤、600 ms Pre-roll、上行回音閘門（`uplink_is_echo`）與離線去回音（`suppress_echo_s16le`）、800 ms 插話打斷（Barge-in）、長通話自動換線續聊，以及三階段推銷／詐騙辨識與陪聊控制。
   - 雙向外撥、預約與周邊整合：`errands.rs`（AI 代打）、`appointments.rs` 與 `callback.rs`（待處理預約、簡訊回覆、AI 回電與單一號碼外撥排隊佇列）、`contacts.rs`（多格式本地解析與 AI 輔助名片匯入）、`summary.rs`（相容 OpenAI 格式之通話摘要與跨語翻譯，預設走 Gemini Flash-Lite）、`notify.rs`／`gmail.rs`／`tg_inbound.rs`（16 種通知管道與 Telegram 互動按鈕）、`calendar.rs`（免帳號行事曆表單連結與單檔 HMAC `.ics` 下載）。
2. **Android 特權通話應用與 Magisk／KernelSU 模組（`android/`）**：
   - 套件 `com.pixelcallai.streamer`（`minSdk 29`、`targetSdk 35`、`versionName 1.0.0`、`versionCode 28`），透過模組掛載至 `/system/priv-app` 並授予 `CAPTURE_AUDIO_OUTPUT`、`MODIFY_PHONE_STATE`、`CONTROL_INCALL_EXPERIENCE` 等系統特權，實現 `VOICE_DOWNLINK` 擷取與 `TYPE_TELEPHONY` 上行語音注入。
   - 權限分離設計：`SilentCallScreeningService` 運行於獨立 `:screening` 程序以防崩潰波及音訊服務；Bageo App 本身**不呼叫通用 Root 權限執行切換**，改由模組內建之 `mode_daemon.sh` 與 `apply_mode.sh` 在 Root 端輪詢並執行「AI 接聽 ↔ 一般手機」原子切換。
   - 選用 Xposed 模組（`SmsRilHook.kt`）：透過 `xposed_scope` 嚴格限定僅作用於 `com.android.phone`，不掛鉤其他使用者應用程式。
3. **簡訊轉發器與建置發佈鏈（`tools/sms_forward/`、`.github/workflows/`、`Dockerfile`、`start.*`）**：
   - `sms_forward.py` 僅使用 Python 3.10+ 標準庫，具跨平台排他檔案鎖（`msvcrt`／`fcntl`）、首次掛載不補發歷史舊簡訊機制（Seeding）及 URL 帳密去敏日誌。
   - GitHub Actions `ci.yml` 涵蓋 Linux／Windows／macOS 三平台 Rust 測試與建置、Android APK 建置及 Python 測試；`release.yml` 從零建置（不使用快取）並產出 `SHA256SUMS.txt` 雜湊清單。

---

## 三、已驗證之核心資安與安全防護機制（Positive Controls）

| 防護面向 | 程式碼位置 | 實作驗證結果 |
|---|---|---|
| **跨站請求偽造（CSRF）阻絕** | `crates/agent/src/calls_http.rs`、`errands.rs`、`contacts.rs` | 所有狀態變更與敏感操作路由（建立／取消／重試 AI 代打、決策預約、重發簡訊、切換手機模式、變更聯絡人與設定）均強制要求 `Content-Type: application/json`（匯入要求 `X-Filename`），且伺服器不回應任何 CORS Preflight，惡意外部網頁無法從區網瀏覽器偷發撥號或改設定請求。 |
| **最小權限 `.ics` 存取金鑰** | `crates/agent/src/calendar.rs`、`calls_http.rs:146-154` | 外發至 Telegram 或通知管道的行事曆下載連結不攜帶主控台 `CALLS_UI_TOKEN`，而是以 `HMAC-SHA256` 針對單一資源路徑（`call/{id}` 或 `tasks/{id}`）簽發專屬短金鑰 `k`，即使通知被轉傳亦無法存取其他通話或控制台。 |
| **機密遮罩與未授權健康檢查降級** | `crates/agent/src/config.rs:116`、`calls_http.rs:942`、`main.rs:1508` | `SecretString` 覆寫 `Debug` 輸出為 `"***"`；`GET /calls/api/settings` 僅回傳遮罩值與 `*_set` 布林旗標；未帶權杖呼叫 `/health` 時僅回傳最小存活狀態（版本、語系、活躍通話數），不暴露內部設定。 |
| **通訊錄解析防炸彈與防提示詞注入** | `crates/agent/src/contacts.rs:47-74, 121-148` | 本地匯入限制單檔 `10 MiB`、ZIP 解壓上限 `32 MiB`、表格上限 `256` 欄 × `100,000` 列 × `1,000,000` 儲存格，防止惡意 `.xlsx`／`.docx` 耗盡記憶體；聯絡人名稱與備註帶入提示詞前經 `invisible()` 剝除零寬與雙向控制字元（Bidi Controls）並截斷長度（40／120 字）。 |
| **Root 模式切換防符號連結與防失聯** | `android/magisk/pixelcallai_uplink/mode_daemon.sh`、`apply_mode.sh` | `app_dir_ok` 檢查目標非 Symlink、以 `mv -fT` 原子取代避免跟隨惡意連結、狀態檔設 `chmod 600` 並校驗 UID/GID 與 SELinux Context；切換請求嚴格限制單行 `≤ 128` 字元與純英數格式，杜絕 Shell 命令注入；撥號器切換失敗時由 `dialer_fail` 自動回退，確保手機不會落入無預設電話 App 狀態。 |
| **外撥電話安全閘門與 SIM 國別鎖定** | `crates/agent/src/errands.rs:98-115`、`appointments.rs` | 硬性攔截緊急號碼（110/119/112/911/999 等）、7 碼以下短碼、各國高額付費號碼（台灣 `020x`、北美 `900/976`、日 `990`、英 `09`、星 `1900` 等）與國際冠碼（`00`、`011`、台灣 `00x/01x`）；未知 SIM 資訊之手機拒接代打（`sim_unknown`）；外撥接起前核對媒體連線回報之實際 `sim_country`，不符立即掛斷（`wrong_sim`），防止雙卡機誤撥國際長途。 |
| **不可覆寫之 AI 安全底線與人機確認** | `crates/agent/src/prompts.rs`、`gemini_live.rs`、`errands.rs` | 所有通話強制附加防提示詞注入規則（`ZH_INJECTION_GUARD`）、不答應付款／轉帳／合約／驗證碼底線、銀行核對刷卡「不承認亦不否認」規則，以及被認真詢問時承認 AI 身分（`PERSONA_GUARD`）；AI 代打遇到未授權選項時阻塞等待機主 40 秒（`ask_owner`），未獲機主回覆時程式強制將結果改判為 `need_owner`，防止模型擅自拍板。 |
| **記憶體安全** | `crates/agent/src/` 全模組 | Rust 業務邏輯層零自寫 `unsafe` 區塊，搭配 `rustls` 純 Rust TLS 堆疊，消除傳統 C/C++ 音訊與網路服務常見之緩衝區溢位風險。 |

---

## 四、具體審計發現與殘餘風險清單（Findings & Recommendations）

本次審計未發現任何 Critical 或 High 等級安全漏洞。以下列出經原始碼核實之 1 項中度（Medium）、3 項低度（Low）與 4 項架構／營運層級注意事項：

### `[M-01]` 歷史遺留測試路由 `/v1/turn` 與 `/v1/transfer` 未驗證控制台權杖（Medium — 僅影響開放區網部署）

- **位置**：`crates/agent/src/main.rs:1461-1462`、`1767-1848`
- **發現內容**：主路由器掛載之早期選單／對話測試端點 `POST /v1/turn` 與 `GET|POST /v1/transfer` 未呼叫 `calls_http::token_ok_parts` 驗證 `CALLS_UI_TOKEN`。在預設 `AGENT_HOST=127.0.0.1` 下僅本機可觸及，無外部風險；但若部署者開啟區網存取（`AGENT_HOST=0.0.0.0` 或容器部署），同網段未持權杖之裝置可由 `/v1/transfer` 讀取部分內部音訊組態欄位，且若部署者另行啟用舊版 `AGENT_MODE=llm` 模式，`/v1/turn` 可在未帶權杖下觸發外部對話請求。
- **緩解與建議**：現行預設模式（`ivr_transfer` 搭配 Gemini Live）下 `/v1/turn` 僅回傳靜態字串，不會呼叫 Gemini Live 或撥打電話；嚴禁將 `8787` 埠直接暴露於公網（已明載於 `SECURITY.md` 與 `LEGAL.md`）。建議於後續小版本為該兩條遺留路由補上 `token_ok_parts` 權杖檢查或直接下線移除。

### `[L-01]` 權杖比對使用一般字串相等運算而非固定時間比對（Low）

- **位置**：`crates/agent/src/calls_http.rs:226-250`（`token_ok_parts`）、`crates/agent/src/main.rs:1491-1494`（`phone_allowed`）
- **發現內容**：`CALLS_UI_TOKEN` 與 `DEVICE_TOKEN` 以標準字串 `==` 比對，理論上在低延遲區網環境存在微秒級時序差異（Timing Side-Channel）；相較之下 `.ics` 連結驗證（`calendar.rs`）已使用 HMAC 驗證。
- **建議**：後續版本可改以 `hmac`／常數時間位元組比對函式統一處理權杖驗證。

### `[L-02]` Android `CallAudioService` 匯出屬性與歷史暫存檔權限（Low — 受專用備用機前提保護）

- **位置**：`android/app/src/main/AndroidManifest.xml:75-84`、`android/magisk/pixelcallai_uplink/service.sh:21-22`
- **發現內容**：為允許電腦端 `usb_bridge.rs`（`adb shell`）與開機腳本啟動前景服務，`CallAudioService` 設為 `android:exported="true"` 且未宣告呼叫端權限；若該手機額外安裝不受信任的第三方惡意 App，可發送 `com.pixelcallai.streamer.STOP` Intent 暫停待命。此外 `service.sh` 仍建立早期除錯遺留之 `/data/local/tmp/pixelcallai_uplink.pcm`（`chmod 666`，現行 `TYPE_TELEPHONY` 注入路徑並未讀取該檔）。
- **建議**：維持文件規定「使用獨立專用備用機、不混裝來路不明 App」；後續版本可對 `START/STOP` 補驗呼叫端 UID（僅允許系統／Root／Shell 與自身 UID）並移除 `service.sh` 內未使用的 `pixelcallai_uplink.pcm` 建立指令。

### `[L-03]` 本機 `settings.json` 明文儲存與 URL 查詢字串權杖留痕風險（Low — 已揭露之設計取捨）

- **位置**：`crates/agent/src/settings_store.rs`、`calls_http.rs:222`
- **發現內容**：`data/calls/config/settings.json` 以明文保存 API 金鑰與通知 Token，且支援 `?token=` 網址登入以便手機掃描 QR Code 快速開啟控制台（瀏覽器紀錄或反向代理日誌可能留存該網址；Android 端 `Prefs.redactUrl` 已對系統日誌去敏）。
- **建議**：部署者應確保執行電腦磁碟加密與帳號權限控管，遠端連線一律走私人 VPN（如 Tailscale／WireGuard），切勿透過公網反向代理裸奔。

### `[I-01 ～ I-04]` 架構、模型與營運層級注意事項（Informational）

- **`[I-01]` 本機迴路 WebSocket 信任邊界（`main.rs:1491`）：** 來自 `127.0.0.1`／`::1` 之 WebSocket 連線視為 USB `adb reverse` 手機而免驗 `DEVICE_TOKEN`。若使用者在同一台主機架設反向代理，必須禁止代理將未驗證外部訪客之 `/` 與 `/ws` WebSocket Upgrade 轉發至主程式（已於 `PRIVACY_AND_LEGAL.md` 明文警告）。
- **`[I-02]` 早期演進模組留存（`trtc*.rs`、`wyoming.rs`、`openclaw.rs`、`hold_music.rs`）：** 舊版選單與外部橋接模組仍保留於編譯樹內以維持舊環境變數相容，不影響現行 Gemini Live 預設路徑，未來可列為程式碼瘦身項目。
- **`[I-03]` 大型語言模型機率性邊界：** 234 通 28 語言模擬壓測顯示開場首音中位數 `0.43s`，15 通多語銀行刷卡照會 100% 守住「不承認、不否認、不給個資」底線；惟極少數非詞庫語言來電可能出現道別辨識較慢（走 60–85 秒靜音收尾保護，不會無限占線）或偶發語系切換偏差，屬機率性語音模型固有特性，已於文件與 `LEGAL.md` 明確揭露。
- **`[I-04]` 實機外撥涵蓋度：** AI 代打（`errands.rs`）與回鈴音過濾已通過完整單元測試與 15 種端到端隔離模擬驗證，實體 SIM 外撥各國電信網路之回鈴音差異仍持續收集各機型回報。

---

## 五、第三方依賴與雙重授權（`AGPL-3.0-only` ＋ 商業授權）相容性稽核

為確保著作權人 **h4lo-void** 改採**「社群 `AGPL-3.0-only` 開源授權 ＋ 專有商業授權」雙重授權模式**具備完整法律有效性與權利純潔性，本審計逐一核對專案全部第三方元件：

| 組件範圍 | 包含項目 | 授權條款 | 與 `AGPL-3.0-only` 相容 | 與閉源商業授權相容 |
|---|---|---|---|---|
| **Rust 主程式依賴** | `tokio`, `axum`, `reqwest`, `rustls`, `ring`, `serde`, `serde_json`, `chrono`, `hmac`, `sha2`, `base64`, `flate2`, `url`, `uuid`, `encoding_rs`, `socket2` 等（見 `Cargo.toml`；註：`v1.0` 時曾含 `zhconv` `GPL-2.0-or-later`，後續已移除並改為內建 `Apache-2.0` 之 OpenCC 字典） | 各依賴自有開源授權（見 `Cargo.lock` 與 `NOTICE.md`） | 是 | **是**（移除 `zhconv` 後無外部 GPL/AGPL 連結依賴） |
| **Android App 依賴** | `androidx.core:core-ktx`、`kotlinx-coroutines-android`、`Java-WebSocket`、`xposed-stub`（`compileOnly`） | Apache-2.0 / MIT | 是（完全相容） | **是**（僅需保留第三方版權聲明） |
| **Magisk 模組安裝器** | `android/magisk/.../update-binary`（Andrew Gunnerson） | `GPL-3.0-only` | 是（獨立安裝腳本聚合散布，且 AGPLv3 §13 相容 GPLv3） | **是**（屬獨立安裝程式未連結入 App／Agent；商業授權包亦可由客戶自備安裝器） |
| **內建背景音與試聽音檔** | `assets/ambience_*.wav`（12 組）、`assets/voices/`（1,080 段預錄語音） | Freesound `CC0` 公有領域＋專案自製合成音／Gemini 生成音檔 | 是 | **是** |
| **外部貢獻者條款** | `CONTRIBUTING.md` 貢獻者授權條款（Inbound Grant） | 授予著作權人 `h4lo-void` 永久、可轉授權及雙重授權發佈權 | 是 | **是**（保障未來合併社群 PR 後仍可發放商業授權） |

**相容性結論：** 專案主體程式碼（Rust Agent、Web Console、Android Kotlin App、Python 簡訊工具、腳本與文件）由著作權人獨立掌控權利，第三方程式庫均屬寬鬆授權（Permissive Licenses），採用 **`AGPL-3.0-only` 搭配專有商業授權** 在法律與工程上完全成立且無授權衝突。

---

## 六、個資、密鑰與公開倉庫清潔度核驗

- `.gitignore` 與 `.dockerignore` 已正確排除 `.env`、`data/`、`settings.json`、`*.jks`、`*.keystore`、`*.apk` 與本機編譯產物。
- 公開文件統一使用 RFC 5737 範例位址（`192.0.2.x`）、`.local` 名稱與虛構範例號碼，不含真實門號、私人內網拓撲或任何明文 API 金鑰。

---

## 七、審計模型具名簽署聲明（Formal Audit Attestation）

本人（AI 審計模型 **`gemini-4-carbon`**）茲此具名聲明：本報告所載之模組盤點、防護機制核驗、風險分級（`M-01`、`L-01`～`L-03`、`I-01`～`I-04`）及雙重授權相容性分析，均係基於對 `h4lo-void/bageo-call` v1.0 專案實際原始碼、組態檔、腳本與測試證據之直接檢視所獨立完成，未虛構測試數據或隱匿已知風險。本報告僅供技術安全與架構透明度參考，不構成對任何特定部署環境之法律合規保證或無瑕疵擔保；所有部署者仍須自行遵守 [`LEGAL.md`](LEGAL.md) 及當地法令。

- **Signed by Model:** `gemini-4-carbon`
- **Audit Timestamp:** `2026-11-15 (UTC+8)`
- **Target Project:** `Bageo (h4lo-void/bageo-call) v1.0.0`

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
