# 手機設定

手機是通往電話網路的閘道：它插著你的 SIM 卡，無聲地接起來電，把對方的聲音送給主程式，再把 AI 的聲音播進通話裡。它本身不做任何 AI 運算。

關於建置 App、它的權限與通訊協定，全部寫在 [`android/README.md`](../../android/README.md)（英文）。這一頁是從使用者的角度來看。

## 支援哪些手機

- **實測過：Google Pixel 7、7a、8、9**，以 Magisk 或 KernelSU root。
- **其他手機：風險自負。** App 需要手機的音訊硬體層提供兩件事，而很多手機並不開放給 App，即使是特權 App 也一樣：在通話中錄下對方的聲音，以及把聲音播「進」通話裡。只要缺一樣，電話會被接起來，但有一方什麼都聽不到。目前還沒有其他可用機型的清單——如果你試了，請回報結果。
- Android 10 或更新版本。

有一項行為會因手機而異，主程式會在每通電話一開始自己判斷。如果手機的通話錄音裡不含 AI 自己的聲音（我們實際使用的 Pixel 9），對方可以在 AI 講話時插話，背景音也會整通電話持續播放。如果含有（我們實際使用的 Pixel 7 和 7a），AI 自己的聲音會從它聽到的內容裡濾掉，所以它會先把句子講完才繼續聽，背景音也只在 AI 講話時播放。

## 請用備用手機

安裝模組之後，手機會變成一台專門接電話的機器：

- App 會成為預設電話應用程式和來電過濾應用程式，Google 的電話 App 會被停用（不會出現通話畫面）；
- 「零打擾」會開啟，鈴聲和通知音量歸零；
- 通話中麥克風靜音、喇叭關閉、聽筒音量調到最小——房間裡的聲音不會被聽到，也不會有任何聲音播出來。

你還是可以用另一個撥號 App 從這支手機打電話出去，但別期待它像一般手機那樣好用。以上這些都可以復原：見[還原](#還原)。

## 安裝

1. 把手機 **root**（Magisk 或 KernelSU）。這裡不說明。
2. 如果你曾經把這個 App 當一般 APK 安裝過，請先解除安裝。絕對不要兩種同時存在。
3. 把 `pixelcallai_uplink-<version>.zip` 複製到手機，打開 Magisk 或 KernelSU App →「模組」（Modules）→「從本機安裝」（Install from storage）→ 選那個 zip → 重新開機。
4. 打開 **Myna**。照著檢查清單做；每一行都有一個按鈕：

   | 項目 | 要做什麼 |
   |---|---|
   | 已安裝為特權系統應用程式 | 重新開機後應該會顯示正常。如果沒有，表示模組沒有生效。 |
   | 特權權限 | 由模組授予；不用點任何東西。 |
   | 應用程式權限 | 「**授予權限**」（麥克風、電話、通話紀錄、通知）。 |
   | 預設電話應用程式／來電過濾應用程式 | 「**設定**」。 |
   | 「零打擾」存取權 | 開啟設定頁面並允許。 |
   | 不受電池效能最佳化限制 | 允許這項豁免，Android 才不會把服務停掉。 |

   Root 管理程式詢問時，請允許 root。
5. 讓「**AI 接聽**」保持開啟。

## 連接到電腦

### USB（建議）

1. 在手機上：「設定 → 關於手機 →」連點「版本號碼」七次；然後到「系統 → 開發人員選項 →」開啟 **USB 偵錯**。
2. 把手機接到執行主程式的那台電腦。
3. 在手機上的「**允許 USB 偵錯嗎？**」視窗按允許（勾選「一律允許」）。

這樣就好了。主程式會找到手機，把 App 的本機位址（`ws://127.0.0.1:8765`）透過 USB 線對應到自己身上，並啟動 App 的服務。它每十五秒檢查一次，所以拔掉再插回去、或任何一邊重新啟動，都會自己恢復。USB 還能讓手機保持充電，沒有 Wi-Fi 也能用。

一台電腦可以接好幾支手機；每支手機各自接聽自己的號碼。

### Wi-Fi

手機沒辦法用線接到電腦時，才用這個方式。

1. 啟動主程式時，讓它在網路上監聽並要求權杖：在 `.env` 裡設定 `AGENT_HOST=0.0.0.0`、`DEVICE_TOKEN=<a long random string>` 和 `CALLS_UI_TOKEN=<another one>`（兩個各填一長串不同的隨機字元；見[設定參考](SETTINGS.md)）。
2. 在 App 的「連線」區塊輸入 `ws://<computer's address>:8787/ws?token=<DEVICE_TOKEN>`（其中 computer's address 換成電腦的位址），然後儲存。「**測試連線**」會告訴你連不連得上。

這條連線沒有加密（`ws://`）：只能在你自己的網路裡使用，或在前面架一個 TLS 反向代理並改用 `wss://`。通話走 USB 會比較穩定。

## 日常使用

- 服務執行時，App 會顯示一則不打擾的常駐通知；重新開機後也會自己啟動。
- 想暫停接聽，可以在 App 裡關掉「**AI 接聽**」（電話會無聲地響、不會被接起），或在網頁控制台關掉「由 AI 接聽來電」（電話會被接起來再掛斷）。
- 你自己用這支手機撥出去的電話，App 不會介入。

## 還原

1. 在 Magisk / KernelSU 裡停用或移除 **Myna Uplink** 模組，然後重新開機。如果手機開機開不到那一步，請用 root 管理程式的安全模式，或在 recovery 裡以 root 執行：`touch /data/adb/modules/pixelcallai_uplink/disable`。
2. 移除模組**不會**把 Google 的電話 App 帶回來，也不會關掉「零打擾」——這些 Android 都會記住。重新啟用電話 App：

   ```text
   adb shell su -c "pm enable com.google.android.dialer"
   adb shell su -c "pm enable com.google.android.dialer/com.android.incallui.InCallServiceImpl"
   adb shell su -c "pm enable com.google.android.dialer/com.android.incallui.InCallActivity"
   adb shell su -c "cmd role add-role-holder android.app.role.DIALER com.google.android.dialer"
   adb shell su -c "telecom set-default-dialer com.google.android.dialer"
   ```

3. 到系統設定關掉「零打擾」，並把音量調回來。

## 不能用的時候

請看[疑難排解](TROUBLESHOOTING.md)，以及 [`android/README.md`](../../android/README.md#troubleshooting)（英文）最後面的表格。
