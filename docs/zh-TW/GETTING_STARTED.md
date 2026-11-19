# 開始使用：讓 AI 電話助理接聽你的真實門號

這份指南帶你從下載開始，一路做到 AI 用你自己的門號接起第一通電話，再讓 AI 第一次替你打電話。Bageo 是自架的：一支 root 過的 Android 手機插著你的 SIM 卡，電腦上的一個程式把每通電話即時串到 Gemini Live，所有錄音和逐字稿都留在你硬碟上的資料夾。請預留大約一小時，大部分時間花在準備手機。

1. [需要準備什麼](#1-需要準備什麼)
2. [安裝並啟動 Bageo](#2-安裝並啟動-bageo)
3. [填好基本設定](#3-填好基本設定)
4. [準備手機](#4-準備手機)
5. [打第一通測試電話](#5-打第一通測試電話)
6. [設定通知](#6-設定通知)
7. [聯絡人、行程預約與 AI 代打](#7-聯絡人行程預約與-ai-代打)
8. [讓它一直開著](#8-讓它一直開著)

## 1. 需要準備什麼

| | |
|---|---|
| **一支備用 Android 手機** | 已用 Magisk 或 KernelSU root，Android 10 以上，插著能接電話的 SIM 卡。在 Google Pixel 7、7a、8、9 實測過（見[支援哪些手機](PHONE.md#支援哪些手機)）。這裡不教 root：請照你那個型號的教學做，並記得解鎖 bootloader 會清空手機。 |
| **一台電腦** | Windows 10/11、macOS（Apple Silicon 或 Intel）或 Linux，要接電話的時段都得開著，再加一條接手機的 USB 線。也可以用 Docker，手機改走 Wi-Fi。 |
| **一把 Gemini API 金鑰** | 到 <https://aistudio.google.com/apikey> 建立並複製。Bageo 只需要這一把：即時通話、摘要、防詐複審、聯絡人匯入都用它。Google 依用量計費；想設上限，可以在 Google 帳戶裡設定預算。 |

## 2. 安裝並啟動 Bageo

電腦上只要跑一個 Bageo 程式：它跟手機和 Gemini 溝通、保存通話紀錄，也提供網頁控制台。不需要資料庫，也不用另外裝其他服務。

### 下載發行版（最簡單）

1. 到 [Releases](https://github.com/h4lo-void/bageo-call/releases) 頁面下載你系統的檔案：

   | 系統 | 檔案 |
   |---|---|
   | Windows | `bageo-call-<版本>-windows-x64-setup.exe` |
   | macOS（Apple 晶片） | `bageo-call-<版本>-macos-arm64.dmg` |
   | macOS（Intel） | `bageo-call-<版本>-macos-x64.dmg` |
   | Linux | `bageo-call-<版本>-linux-x64.tar.gz` |

   顯示 Bageo 狀況的小圖示（Bageo Status）已經包在安裝檔裡，不用另外下載。
2. 安裝並啟動：
   - **Windows：** 按兩下 `setup.exe`。不需要系統管理員權限，會裝到 `%LOCALAPPDATA%\Programs\Bageo`。再點桌面或開始選單的「Bageo」捷徑。如果跳出「Windows 已保護您的電腦」，按「其他資訊 → 仍要執行」（程式沒有付費簽章）。通話、錄音和設定存在安裝資料夾裡的 `data`，移除程式不會刪掉它。
   - **macOS：** 打開 `.dmg`，把 **Bageo** 拖進「應用程式」，再點一下它。通話、錄音和設定存在 `~/Library/Application Support/Bageo`，更新 App 不會動到。第一次如果 macOS 說無法驗證開發者：在 Finder 對 Bageo 按右鍵 →「打開」，或到「系統設定 → 隱私權與安全性」按「強制打開」。
   - **Linux：** 解壓縮後，在終端機執行 `./start.sh`。
3. Windows 和 macOS 不會出現命令視窗。系統匣（Windows）或選單列（macOS）會多一個圖示，Bageo 跟著它在背景執行。在圖示的選單按「結束 Bageo」才會停。
4. 第一次會自動用瀏覽器打開網頁控制台，並問要不要設控制台密碼；要用手機或別台電腦打開控制台就要設。之後點圖示就能開控制台，或直接開 <http://bageo.local/>（這台電腦也能用 <http://localhost:8787/>）。Windows 第一次會問要不要允許 Bageo 存取網路，請選「私人網路」允許。
5. USB 連線要用 `adb`（Google 的 Android platform-tools）。沒裝的話，點圖示後按「下載」，它會放進程式旁邊的 `platform-tools` 資料夾。
6. 進階：想用 `.env` 的話，Windows 放在安裝資料夾，macOS 放在 `~/Library/Application Support/Bageo/.env`。Windows 安裝資料夾裡另有 `start.cmd` 和 `start.ps1`，可以用命令列啟動 Bageo；Mac 的 App 裡沒有啟動腳本。想在終端機看輸出，用 `start.cmd -Console`（Windows）或 `./start.sh --console`（Linux）。

Linux，以及 Windows 加了 `-Console` 的時候，啟動腳本會在終端機裡做同樣的事：詢問要不要下載 `adb`、打開網頁控制台，並在桌面放一個「**Bageo 控制台**」捷徑（不想要捷徑的話，Windows 加 `-NoShortcut`、其他系統加 `--no-shortcut`，或在 `.env` 設 `BAGEO_NO_SHORTCUT=1`）。那個終端機視窗請保持開著：關掉它，Bageo 就停了。

### 從原始碼

```text
git clone https://github.com/h4lo-void/bageo-call.git
cd bageo-call
./start.sh            # Windows: .\start.cmd
```

裝好 [Rust](https://rustup.rs) 後，啟動腳本第一次會先建置程式（要幾分鐘）再啟動。拉了新程式碼之後，加 `--build`（PowerShell：`-Build`）重新建置。

### 用 Docker

```text
cp .env.example .env        # 設定 CALLS_UI_TOKEN 和 DEVICE_TOKEN
docker compose up -d --build
```

不想自己建置的話，可以用現成的映像 `bageo/bageo-call`（Docker Hub，另有 `ghcr.io/h4lo-void/bageo-call`），支援 amd64 和 arm64：把 `docker-compose.yml` 的 `build: .` 那一行刪掉、`image:` 改成 `bageo/bageo-call:latest`，再執行 `docker compose up -d`。

容器裡的 Bageo 對區網開放，所以 `.env` 裡這兩個值一定要設：`CALLS_UI_TOKEN`（控制台密碼）和 `DEVICE_TOKEN`（手機連線用）。

- 控制台在 `http://<主機>:8787/`，打開時輸入 `CALLS_UI_TOKEN` 的值。
- 手機用 IP 位址連線，不用插 USB：
  1. 控制台打開「系統」頁，在「**用 IP 位址配對手機**」按「**產生配對碼**」。
  2. 手機的 Bageo App 點「**用 IP 位址連線**」，輸入 Docker 主機的 IP（例如 `192.168.1.20`）和配對碼。配對碼 10 分鐘內有效。

  手機和控制台用同一個埠，所以只要開 8787。主機上的埠換成別的號碼也可以（例如 `9000:8787`），手機輸入的位址就填你對外開的那個埠（`192.168.1.20:9000`）。手機和 Docker 主機要在同一個網路。詳見[手機設定 → Wi-Fi](PHONE.md#wi-fi)。手動填 `ws://<主機>:8787/ws?token=<DEVICE_TOKEN>` 也照樣能用。
- 在 `.env` 設 `PUBLIC_URL`（例如 `http://192.168.1.20:8787`），通知裡的連結才打得開。容器裡不能用 `bageo.local`。
- 時區設定寫在 `docker-compose.yml` 的註解裡。

### 資料存在哪裡

全部都在你啟動程式的那個資料夾（Docker 是 `/data/calls`；macOS App 是 `~/Library/Application Support/Bageo`）：

| 路徑 | 內容 |
|---|---|
| `data/calls/` | 通話紀錄、錄音、逐字稿、聯絡人、待處理事項、AI 代打紀錄、轉發簡訊（`sms/`）與備份檔（`backups/`） |
| `data/calls/config/settings.json` | 你在網頁控制台設定的所有內容，**包括你的 API 金鑰** |
| `.env` | 選用的啟動選項（連接埠、密碼）；見[設定參考](SETTINGS.md) |

`data/` 要備份或刪除都隨你（也能用控制台內建的「**備份與還原**」）。不會存到其他任何地方。

## 3. 填好基本設定

在網頁控制台打開「設定」。

1. **語言**：AI 講話用的語言。（控制台本身的顯示語言，是頁首的切換鈕。）
2. **你的名字**：來電的人怎麼稱呼你（「Alex」、「陳小姐」、「王先生」）。
3. **接聽身分**
   - 「AI 助理」：「您好，這裡是 Alex 的 AI 助理，他現在不方便接電話，請問有什麼需要我轉達的嗎？」來電的人一聽就懂，在法律上也比較安全。
   - 「本人」：AI 照你的習慣說「喂？」，以你的身分講話。它不會替你答應任何事，來電的人通常會以為就是你。請先讀[隱私、安全與法律](PRIVACY_AND_LEGAL.md)。
4. **Gemini API 金鑰**：貼上並儲存。摘要預設也用這把金鑰，不用另外設定。
5. **聲音**：按清單旁邊的「試聽」，聽這個聲音用你的語言講幾句。範例隨程式內建，試聽不花錢。

「**更多選項**」裡還有兩個常用的：「其他人怎麼叫你」（英文名、綽號，AI 聽到就知道是在找你）和「通知和摘要的語言」（例如 AI 用英文接電話，你看的通知維持繁體中文）。

儲存。「系統」頁的檢查清單會列出還缺什麼。

## 4. 準備手機

下面是精簡版；詳細說明與還原方式見[手機設定](PHONE.md)。

1. 把發行版附的 `bageo-phone-module-<version>.zip` 複製到手機，在 Magisk 或 KernelSU App 裡安裝成模組。重新開機。
2. 打開 **Bageo** App，把「**設定步驟**」一項一項做完：權限、預設電話 App、來電過濾、勿擾模式權限、電池最佳化例外。App 本身不需要 root 權限，這些事由模組代勞。
3. 開啟 **USB 偵錯**：「設定 → 關於手機」，連點「版本號碼」七次，再到「系統 → 開發人員選項 → USB 偵錯」。
4. 把手機接上電腦。在手機上的「**允許 USB 偵錯嗎？**」按允許，並勾選「一律允許透過這台電腦進行」。

大約 15 秒內，Bageo 會自己設好 USB 連線並啟動 App 的通話服務。網頁控制台的「系統 → 手機」會列出這支手機，顯示「待命中，可以接電話」就好了。

之後可以拔掉線：只要手機和電腦在同一個網路，手機會自己改用 Wi-Fi 連線（見[手機設定 → Wi-Fi](PHONE.md#wi-fi)）。Windows 防火牆跳出詢問時，請允許 Bageo 使用私人網路。

> 從現在起，這支手機就是一台接電話專用機：它不會響出聲，原本的電話 App 被隱藏，通話時麥克風和喇叭都靜音，房間裡的聲音不會被聽到。這是刻意的設計。想暫時當一般手機用，在「系統 → 手機」或 App 裡切成[一般手機模式](PHONE.md#一般手機模式)，隨時都能切回來。

## 5. 打第一通測試電話

用另一支手機撥這支手機的號碼。

1. 你這邊會聽到響一聲，大約三秒（「響多久再接」；0 = 馬上接）。這一聲的時間裡，AI 已經連上並準備好第一句話。
2. AI 接起來。跟它聊、打斷它、換一種語言問它，然後說再見。
3. 掛斷幾秒後，這通電話會出現在「通話」裡，附上錄音（整通、只有對方、只有 AI）、逐字稿和四行摘要：對方、事情、AI 回覆、要處理。用其他語言講的句子保留原文，下面附翻譯。

**試試看：** 再打一次，假裝推銷保險。「推銷與詐騙電話 → 陪他聊、拖時間」開著時（預設就是開），AI 會裝作有興趣、一項一項問細節，把你留在線上，但不會給任何資料，也不會答應任何事。

如果沒人接，請看[疑難排解](TROUBLESHOOTING.md)；「系統」頁通常會告訴你缺了哪一塊。

## 6. 設定通知

到「設定 → 通知」，選一個服務、按「**新增管道**」，填好欄位（每個欄位都有提示告訴你去哪裡找），再按「**傳送測試**」。想加幾個都可以，每則訊息會送到全部管道。共支援 32 種服務；各服務的設定方式見[通知](NOTIFICATIONS.md)。

通知裡的連結會直接打開那通電話，預設用 `http://bageo.local/`（80 埠打不開時是 `http://bageo.local:8787/`）；「系統 → 關於」看得到目前用哪一個，還附 QR code。用手機點連結之前，要先設控制台密碼（見[從其他裝置使用控制台](SETTINGS.md#從其他裝置使用控制台)）。

只有用 VPN、反向代理或 Docker 連到這台電腦時，才需要到「設定 → 通知」填「**通知裡的網址**」。

想在自己手機上開控制台：掃那個 QR code，再把頁面加到主畫面（iPhone：「分享 → 加入主畫面」；Android：Chrome 選單 ⋮ →「加到主畫面」）。之後點 Bageo 圖示就能打開。

## 7. 聯絡人、行程預約與 AI 代打

接電話用不到這些功能，每一項都只要幾分鐘。

### 聯絡人

到「聯絡人 → 匯入」，丟一個檔案進去：

- 從手機匯出的 vCard（`.vcf`），或 Google 聯絡人、Outlook 的 CSV；
- 存成 CSV 或 TSV 的試算表，或 Excel `.xlsx`、Word `.docx` 裡的表格；
- 每行一個名字加號碼的純文字；
- PDF、名片照片或通訊錄截圖（由 Gemini 辨識）。

存檔前每一筆都會先給你看，重複的可以選擇合併或略過。聯絡人的備註（「我妹」「房東」）會在那個人打來時告訴 AI。在某通電話的詳細資料裡按「**加入聯絡人**」，也能把來電者存起來。

每位聯絡人可以分類：一般聯絡人、白名單（重要親友，可以轉接給你—要讓其他人也能轉接，可在設定調整「誰打來可以轉」）、黑名單（直接拒接）、陪聊名單（一接起就陪聊拖時間）。

### 行程預約

到「設定 → 摘要與行事曆」，把「行程預約」改成「先問我」。之後：

1. 來電的人想約時間，AI 會說確認後再回覆。
2. 通話結束後，這筆預約出現在「通話」頁最上面的「**待處理事項**」，通知裡也有。
3. 選怎麼告訴對方（**傳簡訊**、**AI 回電**或**不通知對方**），再按「**同意**」或「**不同意**」。要傳簡訊，設定裡的「傳簡訊告訴對方」要開著。
4. 同意之後按「**加入行事曆**」。第一次會問你用哪個行事曆（Google、Outlook.com、Microsoft 365、Apple、Yahoo 或 `.ics`），之後按下去就直接加入。

### 第一次 AI 代打

1.4 以上版本的手機 App 連線時會自動上報它插了哪些 SIM 卡（只有舊版 App 才需要先打一通電話給它）。接著打開「**AI 代打**」：

1. **打給誰**：輸入聯絡人名字或號碼。沒有 `+` 的號碼視為你所在國家的號碼；國外號碼請用 `+` 開頭。AI 只會用跟號碼同一國的 SIM 卡撥打。
2. **要處理的事**：寫清楚要辦什麼、哪些條件可以接受，例如：「訂明天晚上 7 點 4 位，姓王，非吸菸區；7 點半到 8 點也可以。」
3. **用什麼身分打**：你的助理（預設）或你本人。
4. 按「**開始打**」，並保持頁面開著。即時字幕會顯示雙方在說什麼。如果餐廳只剩 8 點半，AI 會跳出一張 40 秒倒數的提問卡問你；「**交代 AI**」也能隨時給它指示。

結果會出現在「交辦紀錄」裡，並只發一則通知。結果寫「要你決定」時，從選項挑一個或自己寫回覆，再讓 AI 打一次。

## 8. 讓它一直開著

Bageo 在跑，才接得到電話。

- **Windows：** 點 Bageo 的圖示，在「設定」打開「開機時自動啟動」。插電時關閉睡眠。
- **macOS：** 點 Bageo 的圖示，在「設定」打開「開機時自動啟動」，並防止睡眠（「電池／能源 → 防止自動進入睡眠」）。
- **Linux：** 建立一個 systemd 使用者服務：

  ```ini
  # ~/.config/systemd/user/bageo-call.service
  [Unit]
  Description=Bageo agent
  After=network-online.target

  [Service]
  WorkingDirectory=%h/bageo-call
  ExecStart=%h/bageo-call/start.sh --no-browser --yes
  Restart=on-failure

  [Install]
  WantedBy=default.target
  ```

  再執行 `systemctl --user enable --now bageo-call`，以及 `loginctl enable-linger $USER`，讓它在沒登入時也會跑。
- **Docker：** `docker-compose.yml` 裡的 `restart: unless-stopped` 已經會在重開機後把它帶起來。

手機請一直接著充電器。手機或電腦重新開機後，兩邊都會自己重新連上。

### 在系統匣或選單列看狀態

在 Windows 和 macOS，Bageo 的圖示（叫 **Bageo Status**）會跟著 Bageo 一起啟動，待在系統匣（Windows）或選單列（macOS）。圖示外框是綠色代表一切正常，黃色代表有地方要留意（例如控制台還有項目沒設定、有手機沒連線，或缺 Android 工具），紅色代表連不上 Bageo 或沒有手機連線。

- **Windows：** 新圖示會先收在時鐘旁的 `^` 裡，把它拖到工作列上就會一直看得到。
- **macOS：** 圖示在選單列右邊。

點圖示可以看到控制台網址和手機，也可以直接開啟控制台。在圖示上按右鍵選「結束 Bageo」，Bageo 會停止，圖示也會消失。

在「設定」可以填網址、輸入控制台的密碼、打開「開機時自動啟動」，以及選擇要跳哪些小通知：有來電時、收到簡訊時、通話摘要寫好時。點一下通知就開啟控制台。

通話摘要的通知會在通話結束、Bageo 寫好摘要後跳出，標題是聯絡人名稱（沒有就顯示號碼），內容是這通電話的重點，點下去直接開那通電話的紀錄。來電和簡訊通知預設關閉，通話摘要通知預設開啟。簡訊通知要搭配同一版的[簡訊轉發](../../tools/sms_forward/README.md)。

## 接下來

- 讓它講話像你：[撰寫提示詞](PROMPTS.md)
- 所有可以調整的東西：[設定參考](SETTINGS.md)
- 內部怎麼運作：[架構](ARCHITECTURE.md)


<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
