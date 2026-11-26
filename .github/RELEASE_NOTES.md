# Bageo v2.1.0

**Not only Pixel: other rooted Android phones are supported too.** Tested: Google Pixel and the MediaTek Redmi K60 Ultra. On any other rooted phone, call it from another phone after installing and talk to the AI for about 20 seconds: Bageo adjusts by itself and shows the result in the console and the app. Samsung phones with Exynos chips mostly do not work.

- **Which file to download.** `bageo-phone-standalone-2.1.0.zip` is Bageo AI Call Agent, the phone-only edition: all of Bageo runs on the phone, no computer needed. `bageo-phone-module-2.1.0.zip` is for Bageo on a computer: use it with the Windows, macOS or Linux installer, or with Docker. Install only one of the two; installing one removes the other at the next reboot. Flash the zip in KernelSU or Magisk and reboot; do not unpack it or install `base.apk` as a regular app.
- **Phones adjust themselves.** If the AI's voice drops in a call, the phone switches to other settings and remembers them. A new phone asks for one test call, and the console then asks whether you heard the AI.
- **Steadier on MediaTek phones.** The AI's voice no longer breaks up on the Redmi K60 Ultra.
- **If the AI cannot answer,** the caller hears a "please call again later" message and you get the reason, for example a used-up Gemini quota.
- **International calls:** the AI greets first once the carrier's warning tone ends, and the console shows phones by name.
- **Texts you have read stay read,** also in another browser or the phone app.

## 繁體中文

**不只 Pixel：其他已 root 的 Android 手機也支援。** 實測過 Google Pixel 和聯發科的 Redmi K60 Ultra。其他已 root 的手機裝好後，用另一支電話打給它，跟 AI 講 20 秒左右：Bageo 會自動調整，並在控制台和 App 告訴你結果。三星 Exynos 晶片的手機多半不支援。

- **要下載哪個檔。** `bageo-phone-standalone-2.1.0.zip` 是手機獨立版 Bageo AI Call Agent：整個 Bageo 都在手機上，不用電腦。`bageo-phone-module-2.1.0.zip` 是配電腦用的：搭配 Windows、macOS、Linux 安裝檔或 Docker 使用。兩個只能裝一個，裝其中一個會在下次重開機時移除另一個。在 KernelSU 或 Magisk 刷入 zip 後重開機，不要解壓縮，也不要把 `base.apk` 當一般 App 安裝。
- **手機會自己調整。** 通話中 AI 的聲音掉了，手機會換一組設定並記住。新手機要先打一通測試電話，之後控制台會問你有沒有聽到 AI。
- **聯發科手機更穩。** Redmi K60 Ultra 上 AI 的聲音不再斷斷續續。
- **AI 接不了時，** 來電的人會聽到「請稍後再撥」的語音，你也會收到原因，例如 Gemini 額度用完。
- **國際電話：** 電信提示音播完後 AI 會先打招呼，控制台也用手機名稱顯示手機。
- **看過的簡訊維持已讀，** 換瀏覽器或用手機 App 看也一樣。

Full notes / 完整說明：[CHANGELOG.md](https://github.com/h4lo-void/bageo-call/blob/v2.1.0/CHANGELOG.md)

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
