# 開始使用

從零開始，到 AI 接起第一通電話。請預留大約一小時，大部分時間花在手機上。

1. [需要準備什麼](#1-需要準備什麼)
2. [安裝並啟動主程式](#2-安裝並啟動主程式)
3. [填好基本設定](#3-填好基本設定)
4. [準備手機](#4-準備手機)
5. [第一通電話](#5-第一通電話)
6. [通知](#6-通知)
7. [讓它一直開著](#7-讓它一直開著)

## 1. 需要準備什麼

- 一支**備用的 Android 手機**，裝有能接電話的 SIM 卡，並且已用 Magisk 或 KernelSU **root**。在 Pixel 7、7a、8、9 上實測過。這裡不說明怎麼 root；請照著你那個型號的教學做，並記得 root 會把手機清空。
- 一台**電腦**（Windows 10/11、macOS、Linux），需要接電話的時段要一直開著；還要一條接手機的 USB 線。
- 一把 **Gemini API 金鑰**：到 <https://aistudio.google.com/apikey> 登入，建立金鑰並複製下來。Live 音訊由 Google 依用量計費；想設上限的話，可以在你的 Google 帳戶裡設定預算。

## 2. 安裝並啟動主程式

「主程式」（agent）就是電腦上的那個程式：它負責跟手機和 Gemini 溝通，也提供網頁控制台。

### 簡單的方法：下載發行版

1. 從 Releases 頁面下載適合你系統的套件，解壓縮到任何位置（例如 `Documents/myna-call`）。
2. 啟動：
   - **Windows：** 按兩下 `start.cmd`。如果 Windows SmartScreen 警告發行者不明，請選「其他資訊 → 仍要執行」（More info → Run anyway）。
   - **macOS：** 按兩下 `start.command`。第一次執行時，macOS 可能會因為程式沒有經過公證（notarize）而拒絕開啟：請在 `start.command` 上按右鍵 →「打開」；如果被擋下的是主程式本身，請到「系統設定 → 隱私權與安全性」允許它。也可以在終端機裡，於該資料夾執行一次 `xattr -dr com.apple.quarantine .`。
   - **Linux：** 在終端機執行 `./start.sh`。
3. 腳本會尋找 `adb`（Google 的 Android platform-tools，USB 連線需要它）。如果沒有安裝，腳本會詢問要不要從 Google 下載，放到程式旁邊的 `platform-tools` 資料夾。
4. 瀏覽器會打開網頁控制台 <http://localhost:8787/>。終端機視窗請保持開啟：把它關掉，主程式就停了。
5. 主程式第一次啟動成功時，腳本會在桌面放一個「**Myna 控制台**」捷徑（macOS 是 `.webloc` 檔；Linux 是應用程式選單裡的項目，也會複製到桌面資料夾）。下次用它打開控制台就好。捷徑還在就不會重做；不想要的話，啟動時加 `-NoShortcut`（Windows）／`--no-shortcut`，或在 `.env` 設 `MYNA_NO_SHORTCUT=1`。

### 從原始碼

```text
git clone <this repository>
cd myna-call
./start.sh            # Windows: .\start.cmd
```

裝好 [Rust](https://rustup.rs) 之後，啟動腳本第一次會先建置主程式（需要幾分鐘），然後啟動它。拉了新的程式碼之後，用 `--build`（PowerShell：`-Build`）重新建置。

### 東西存在哪裡

所有東西都放在你啟動程式的那個資料夾裡：

| | |
|---|---|
| `data/calls/` | 通話紀錄、錄音、逐字稿 |
| `data/calls/config/settings.json` | 你在網頁控制台設定的內容，**包括你的 API 金鑰** |
| `.env` | 選用的啟動選項（連接埠、權杖）——見[設定參考](SETTINGS.md) |

`data/` 要備份或刪除都隨你。其他地方不會存任何東西。

## 3. 填好基本設定

打開網頁控制台的「設定」。

1. **語言**——AI 講話用的語言；通知和摘要也用這個語言。（控制台本身的介面語言，是頁首的那個切換選單。）
2. **你的名字**——來電的人怎麼稱呼你（「Alex」、「Ms. Chen」、「王先生」）。
3. **接聽身分**
   - 「AI 助理」：「您好，我是 Alex 的 AI 助理，他現在不方便接電話——有什麼需要我轉達的嗎？」對來電者來說清楚明白，在法律上也是比較保險的選擇。
   - 「本人」：AI 會像你一樣用「喂？」接起來，並以你的身分講話。它絕不會替你聲稱任何事，有人認真問起時也會承認自己是 AI，但來電者通常會以為就是你本人。請先讀[隱私、安全與法律](PRIVACY_AND_LEGAL.md)。
4. **Gemini API 金鑰**——貼上後儲存。
5. **聲音**——按清單旁邊的 ▶，可以聽每個聲音用你的語言講幾句話（每個聲音第一次播放要等幾秒；之後就有快取）。

儲存。「系統」頁的檢查清單會顯示還缺什麼。

## 4. 準備手機

細節與還原方式：[手機設定](PHONE.md)。簡單來說：

1. 把（發行版裡的）`pixelcallai_uplink-<version>.zip` 複製到手機，在 Magisk 或 KernelSU App 裡當作模組安裝。重新開機。
2. 打開 **Myna** App，照著檢查清單一項一項點，直到每一行都顯示「正常」（權限、預設電話應用程式、來電過濾、「零打擾」存取權、不受電池效能最佳化限制）。被詢問時，請允許 root。
3. 開啟 **USB 偵錯**（「設定 → 關於手機 →」連點「版本號碼」七次，然後「系統 → 開發人員選項 → USB 偵錯」）。
4. 把手機接上電腦。在手機上的「**允許 USB 偵錯嗎？**」視窗按允許，並勾選「一律允許透過這台電腦進行」。

大約十五秒內，主程式會自己建立 USB 連線，並啟動 App 的通話服務。網頁控制台的「系統 → 手機」會顯示這支手機：先是「USB 連接」，接著是「已連上主程式」。

> 這支手機現在是一台專門接電話的機器：它不會響鈴出聲，自己的電話 App 被隱藏，通話中麥克風和喇叭都保持靜音，房間裡的聲音不會被聽到。這是刻意設計的。

## 5. 第一通電話

用另一支手機撥這支手機的號碼。

- 撥打的那一方會聽到響一聲（大約三秒，可以調整）。
- AI 接起來。跟它講話、打斷它、說再見。
- 通話結束幾秒後，它會出現在「通話」裡，附上錄音、逐字稿和摘要。

如果沒有接聽，請看[疑難排解](TROUBLESHOOTING.md)——「系統」頁通常會指出缺了哪一塊。

## 6. 通知

到「設定 → 通知 → 新增管道」，選一個服務，填好欄位（每個欄位都有提示告訴你怎麼取得），然後按「**傳送測試**」。可以新增好幾個；每則訊息都會傳到全部的管道。各服務的設定說明：[通知](NOTIFICATIONS.md)。

要讓通知裡的連結能打開那通電話，請把控制台開放給你的網路——見[從其他裝置使用](SETTINGS.md#從其他裝置使用控制台)。這樣連結會自動指向 `http://myna.local:8787/`（或這台電腦的區網 IP）；只有你用別的方式連到這台電腦（VPN 名稱、反向代理、Docker）時，才需要填「**這個控制台的網址**」。

要在手機上用：在電腦上打開控制台的「系統」頁，「關於」會顯示控制台網址和 QR code。用手機掃描，再把這個網頁加到主畫面（iPhone：「分享 → 加入主畫面」；Android：Chrome 選單 ⋮ →「加到主畫面」）。下次點 Myna 圖示就打開。

## 7. 讓它一直開著

主程式要在執行中，電話才會有人接。

- **Windows：** 在「啟動」資料夾（`Win+R` → `shell:startup`）放一個 `start.cmd` 的捷徑；或在「工作排程器」建立一個「登入時」執行 `start.cmd -NoBrowser` 的工作。接著電源時請停用睡眠。
- **macOS：**「系統設定 → 一般 → 登入項目」→ 加入 `start.command`；並避免電腦睡眠（「電池／能源 → 避免自動進入睡眠」）。
- **Linux：** 用 systemd 的使用者單元：

  ```ini
  # ~/.config/systemd/user/myna-call.service
  [Unit]
  Description=Myna agent
  After=network-online.target

  [Service]
  WorkingDirectory=%h/myna-call
  ExecStart=%h/myna-call/start.sh --no-browser --yes
  Restart=on-failure

  [Install]
  WantedBy=default.target
  ```

  執行 `systemctl --user enable --now myna-call` 和 `loginctl enable-linger $USER`，這樣不用登入也會執行。

手機請一直接著充電器。手機或電腦重新開機後，兩邊會自己重新連線。

## 接下來

- 讓它講話像你：[撰寫提示詞](PROMPTS.md)
- 所有可以調整的項目：[設定參考](SETTINGS.md)
