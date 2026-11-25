# Bageo v2.0.0

**Bageo on your phone alone, no computer, no API fees.** Bageo AI Call Agent runs entirely on a rooted phone. With a free Google AI Studio Gemini key there are no API fees; in our tests the free limit covered 10 or more calls a day (Google sets the free limit, and it may change).

- **Install on a rooted phone.** Flash `bageo-phone-standalone-2.0.0.zip` in KernelSU or Magisk and reboot. Do not unpack it or install `base.apk` as a regular app.
- **Tested on a Google Pixel 9 (Android 16) with KernelSU.**
- **New module ID: `bageo_ai_call_agent`.** If the computer-linked module (`pixelcallai_uplink`) is already installed, the other one is removed at the next reboot, and the same the other way round. The app and its settings stay.
- **One switch back to a normal phone.** Turn off *AI answering* and the system Phone app, ringers and Do Not Disturb come back.
- **Also in 2.0.0:** calls follow the caller's language, scam stalling repeats itself less, and phone pairing and saved keys are better protected. The computer, NAS and Docker editions are updated to 2.0.0 too.

## 繁體中文

**只靠手機就能用 Bageo，不用電腦，沒有 API 費用。** Bageo AI Call Agent 整個跑在已 root 的手機上。用 Google AI Studio 的免費 Gemini 金鑰就沒有 API 費用；我們實測免費額度每天 10 通以上夠用（免費額度由 Google 決定，可能會變）。

- **裝在已 root 的手機上。** 在 KernelSU 或 Magisk 刷入 `bageo-phone-standalone-2.0.0.zip` 後重開機。不要解壓縮，也不要把 `base.apk` 當一般 App 安裝。
- **已在 Google Pixel 9（Android 16）加 KernelSU 上實測。**
- **新的模組 ID：`bageo_ai_call_agent`。** 手機上已有電腦連動版模組（`pixelcallai_uplink`）時，另一版會在下次重開機移除，反過來裝也一樣。App 和設定都會保留。
- **一個開關換回一般手機。** 關掉「AI 接聽」，系統電話 App、響鈴和勿擾就會恢復。
- **2.0.0 還有：** 電話跟著來電的人用的語言走、詐騙陪聊更少重複、手機配對和存好的金鑰保護更完整。電腦、NAS、Docker 版也一併更新到 2.0.0。

Full notes / 完整說明：[CHANGELOG.md](https://github.com/h4lo-void/bageo-call/blob/v2.0.0/CHANGELOG.md)

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
