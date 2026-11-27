# Bageo v2.2.0

**Each SIM gets its own language plan, mid-call language switching waits until it is sure, and trusted callers are never stalled.**

- **Which file to download.** `bageo-phone-standalone-2.2.0.zip` is Bageo AI Call Agent, the phone-only edition: all of Bageo runs on the phone, no computer needed. `bageo-phone-module-2.2.0.zip` is for Bageo on a computer: use it with the Windows, macOS or Linux installer, or with Docker. Install only one of the two; installing one removes the other at the next reboot. Flash the zip in KernelSU or Magisk and reboot; do not unpack it or install `base.apk` as a regular app.
- **Languages and regions per SIM, and steadier switching.** Each SIM can have a main language and region plus up to two second languages with regions. Mid-call switching waits for a clear full sentence (two in a row or an explicit request for other languages), and polite refusals like "can you not speak…" no longer trigger a switch.
- **Better everyday call recognition and scam checks.** Couriers, bank charge checks, notices, scheduling, family chat and wrong numbers are recognized without being flagged on keywords alone. Contacts, Whitelist callers and numbers marked *It was not* are never stalled or marked as spam or scam (the Stall list still wins if listed there), and the second-look check waits up to 3 seconds with one retry when busy.
- **AI errands confirm before dialing internationally.** When the picked SIM is from a different country than the number, Bageo asks you to confirm before dialing.
- **Hong Kong `+852` and pre-paid SIM alerts, plus 90 redrawn regional scenes.** Hong Kong SIMs wait for the `+852` three-language alert and recognize the new pre-paid SIM warning; 90 regional illustrations are redrawn and carried inside language packs.

## 繁體中文

**每張 SIM 卡都有自己的語言設定、通話中確定才換語言，通訊錄與白名單來電不再被陪聊。**

- **要下載哪個檔。** `bageo-phone-standalone-2.2.0.zip` 是手機獨立版 Bageo AI Call Agent：整個 Bageo 都在手機上，不用電腦。`bageo-phone-module-2.2.0.zip` 是配電腦用的：搭配 Windows、macOS、Linux 安裝檔或 Docker 使用。兩個只能裝一個，裝其中一個會在下次重開機時移除另一個。在 KernelSU 或 Magisk 刷入 zip 後重開機，不要解壓縮，也不要把 `base.apk` 當一般 App 安裝。
- **各 SIM 卡的語言與地區，以及更穩的語言切換。** 每張 SIM 卡可設定主要語言和地區，外加最多兩個附地區的副語言。通話中要講完整一句或明確要求才切換，對方說「不要講某種語言」不再誤切。
- **日常情境識別與防詐複查更準確。** 快遞、銀行核對、正事通知、約時間、親友閒聊與打錯電話不再單靠關鍵字就判成推銷或詐騙；聯絡人、白名單與標成「不是」的號碼不再被陪聊或標成詐騙（若也在陪聊名單則以陪聊名單為準），防詐複查改為最多等 3 秒並在忙碌時重試一次。
- **AI 代打跨國撥號前先確認。** 選用的 SIM 卡與對方號碼不同國家時，Bageo 會先請你確認才撥出。
- **支援香港 `+852` 與新儲值卡電信提示音，以及 90 個地區重繪插圖。** 香港 SIM 卡會等 `+852` 三語提示音並認得新儲值卡提示；90 個地區的特色插圖已重繪並隨語言包下載。

Full notes / 完整說明：[CHANGELOG.md](https://github.com/h4lo-void/bageo-call/blob/v2.2.0/CHANGELOG.md)

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
