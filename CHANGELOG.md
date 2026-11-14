# Bageo v1.0.0

**English** · [繁體中文](#繁體中文) · [简体中文](#简体中文)

**It answers your phone. Now it makes your calls too.**

Bageo is an open-source AI phone assistant for the phone number you already
have, and 1.0 is its first stable release. Your calls are still picked up in
about 0.4 seconds by a natural Gemini Live voice that lets itself be
interrupted. What is new: **the AI dials out for you**, asks you when it is
unsure, and reports back. Appointment requests become a to-do list, contacts
import from almost any file, transcripts keep every language with a
translation, and sales and scam calls now meet three layers of checks.

Everything still runs on your computer, with your SIM and your own Gemini key:
no SIP trunk, no rented number, no platform fee.

## Highlights

- **AI errands (new).** Pick a contact or type a number, write what needs doing
  (book a table, move an appointment, find a time with a friend, ask a shop
  something) and choose whether the AI calls as your assistant or as you. Follow
  the live captions, answer the AI's questions on a 40-second card, type
  instructions mid-call, and get a structured result with one notification. If
  you are away, the AI never accepts its own options: the result waits for your
  decision, and one button sends the AI back with your answer and the earlier
  conversation.
- **Safe dialing.** Errands refuse emergency, short-code, premium-rate and
  international-prefix numbers. The agent learns which country's SIM each phone
  holds, dials only on a SIM of the number's country, and hangs up before
  connecting if the phone reports a different SIM.
- **Appointments as a to-do list.** The AI takes the request without agreeing to
  it. You press Accept or Decline on the console or on a Telegram card and pick
  how the caller hears back: a text from the SIM that took the call, an AI
  call-back, or nothing. One tap adds the time to Google Calendar, Outlook.com,
  Microsoft 365, Apple Calendar, Yahoo Calendar or an `.ics` file; the agent
  holds no calendar credentials.
- **AI call-backs that wait for a person.** The AI stays silent through the
  ringback (it is filtered out), waits until the other side picks up and speaks,
  gives your answer once in the language of the original call, and hangs up.
- **Contacts from almost anything.** vCard 2.1/3.0/4.0, Google or Outlook CSV,
  TSV, plain text, `.xlsx` and `.docx` import locally with automatic encoding
  detection; PDFs, business cards and screenshots go through Gemini Flash-Lite.
  Preview, merge duplicates, export vCard. Known callers bring their name and
  your note into the call and are protected in the scam review.
- **Three layers against sales and scams.** A follow-up whitelist, weighted
  multilingual keywords (insurance, loans, soft sales, ATM tricks, gift cards,
  chat-app scams) and a semantic second look by `gemini-flash-lite` with
  `too_early` re-checks and contact protection. A bank checking a card charge is
  not a scam: the AI neither confirms nor denies the charge, and the summary
  tells you to call your bank.
- **Languages.** Each transcript line stays in the language it was spoken, with
  a translation into your notification language. New setting *Other names
  callers use for you* (`owner_aliases`). On a Taiwanese SIM, the AI now always
  waits out the carrier's warning on calls from abroad by itself.
- **A better interview prompt** in all nine languages: it collects the exact
  words you say and the ones you never would, so the prompt it writes sounds
  less scripted.
- **Summaries on your Gemini key by default** (its lowest-cost model). Choose
  *Another service* for DeepSeek, OpenAI, OpenRouter or a local Ollama.

## Changes that affect existing setups

- **Google Calendar connection removed.** Entries are added with one tap to the
  calendar you choose. Saved Google Calendar credentials are ignored and dropped
  at the next save. An appointment mode of *auto* from an older version works as
  **Ask me first**.
- **Appointment texts go out from the SIM that took the call**, or from the
  phone you pick under **Send the text again**.
- **The country calling code left the console.** It comes from `COUNTRY_CODE`
  (or `country_code` in `settings.json`); with 繁體中文 and nothing set, `886`
  is assumed. A value saved by an older version still wins; delete that line to
  use the environment variable.
- **The carrier-warning setting is gone.** The AI waits for the warning only when
  a Taiwanese SIM takes a call from abroad, decided automatically.
- **Summary service** has two choices: *Gemini* (default) or *Another service*.
  An older *DeepSeek* setting becomes *Another service*; *Automatic* becomes
  *Another service* when a base URL or key is set, otherwise *Gemini*.

## Downloads

| File | For |
|---|---|
| `bageo-call-1.0.0-windows-x64.zip` | Windows 10/11 |
| `bageo-call-1.0.0-macos-arm64.tar.gz` | macOS on Apple Silicon |
| `bageo-call-1.0.0-macos-x64.tar.gz` | macOS on Intel |
| `bageo-call-1.0.0-linux-x64.tar.gz` | Linux x86-64 |
| `pixelcallai_uplink-1.0.0.zip` | The phone module, for Magisk or KernelSU |

Each desktop package holds the agent, the 1,080 bundled voice clips and
background sounds, the start scripts and the documentation. Docker users build
from the repository with `docker compose up -d --build`.

## Upgrading from 0.2.x

1. Stop the agent and back up the `data/` folder (and `.env` if you have one).
2. Unpack the 1.0.0 package and move `data/` and `.env` into it, or unpack it
   over the old folder. Docker: pull the new code and run
   `docker compose up -d --build`; the data volume stays.
3. Make sure no call is in progress, install the new phone module over the old
   one in Magisk or KernelSU, and reboot the phone. It is signed with the same
   project key, so the app keeps its data.
4. Start the agent. Settings, call records and contacts carry over. Let each
   phone take one call before its first AI errand, so the agent learns its SIMs.

## How 1.0 was tested

- More than 520 unit and integration tests run in CI on every change.
- 234 simulated calls against the real agent, with callers speaking 28
  languages, and 15 simulated AI errand scenarios, all passing.
- Real calls on Pixel 7, 7a and 9: answering, interruptions, sales and scam
  stalling, appointment texts, an AI call-back that connected and gave its
  answer, and normal phone mode.

**Not yet seen on a real phone:** an AI errand dialing out, and the ringback
filter against a real ringback. Notification channels other than Telegram are
tested against each service's documented format. Reports from other phones and
languages help most; see [CONTRIBUTING.md](CONTRIBUTING.md).

Documentation: [README](README.md) ·
[Getting started](docs/en/GETTING_STARTED.md) ·
[Architecture](docs/en/ARCHITECTURE.md) ·
[Writing the prompt](docs/en/PROMPTS.md) · [Roadmap](docs/en/ROADMAP.md)

---

## 繁體中文

[English](#bageo-v100) · **繁體中文** · [简体中文](#简体中文)

**它幫你接電話，現在也幫你打電話。**

Bageo 是用你原本門號運作的開源 AI 電話助理，1.0 是第一個正式穩定版。來電照樣在大約 0.4 秒內，由自然、可以被插話的 Gemini Live 語音接起。這一版的新東西是：**AI 會替你打電話**，拿不準就先問你，打完回報結果。來電想約時間會變成待處理事項，通訊錄幾乎什麼檔案都能匯入，逐字稿保留每種語言並附翻譯，推銷與詐騙電話要過三道關卡。

一切仍然在你自己的電腦上，用你的 SIM 卡和你自己的 Gemini 金鑰運作：不需要 SIP 線路、不用租號碼、沒有平台月費。

### 重點

- **AI 代打（新功能）。** 選聯絡人或輸入號碼，寫下要辦的事（訂位、改期、跟朋友約時間、問店家問題），再選 AI 用你的助理還是你本人的身分打。你可以看即時字幕、在 40 秒倒數的提問卡上回答 AI、通話中打字交代，最後收到一份結構化結果和一則通知。你不在的時候，AI 絕不自己拍板：結果會等你決定，按一個按鈕就能讓 AI 帶著你的答覆和上一通的內容再打一次。
- **安全撥號。** 代打拒撥緊急電話、短碼、高額付費電話和國際冠碼號碼。主程式會記住每支手機插的是哪國的 SIM 卡，只用號碼所屬國家的 SIM 卡撥出；手機回報的 SIM 卡不對，接通前就掛斷。
- **行程預約變成待處理事項。** AI 記下請求，但不當場答應。你在控制台或 Telegram 卡片上按「同意」或「不同意」，再選對方怎麼收到答覆：從接那通電話的 SIM 卡傳簡訊、AI 回電，或不通知。一鍵加入 Google 日曆、Outlook.com、Microsoft 365、Apple 日曆、Yahoo 日曆或 `.ics` 檔；主程式不保存任何行事曆憑證。
- **會等真人接起的 AI 回電。** 回鈴音期間 AI 保持安靜（回鈴音會被濾掉），等對方接起並開口，用原本那通電話的語言把你的答覆講一次，然後掛斷。
- **什麼格式都能匯入的通訊錄。** vCard 2.1/3.0/4.0、Google 或 Outlook CSV、TSV、純文字、`.xlsx`、`.docx` 在本機匯入，自動判斷編碼；PDF、名片和截圖交給 Gemini Flash-Lite 辨識。可以先預覽、合併重複、匯出 vCard。熟人來電時，名字和你的備註會帶進通話，防詐複審也會保護他們。
- **三道關卡擋推銷與詐騙。** 回覆白名單、多語加權關鍵字（保險、貸款、軟性推銷、ATM 操作、點數卡、通訊軟體詐騙），以及 `gemini-flash-lite` 的語意複審（含 `too_early` 延後再審與聯絡人保護）。銀行打來核對刷卡不算詐騙：AI 不替你承認或否認那筆消費，摘要會提醒你自己打給銀行。
- **語言。** 逐字稿每一句保留原本的語言，並附上翻成你通知語言的對照。新增設定「其他人怎麼叫你」（`owner_aliases`）。台灣 SIM 卡接到國外來電時，AI 一律自己等電信的防詐提示音播完。
- **更好的訪談提示詞**，九種語言都有：它會問出你真的會說的原話和絕對不說的詞，寫出來的提示詞比較不像照稿念。
- **摘要預設用你的 Gemini 金鑰**（最便宜的模型）。想用 DeepSeek、OpenAI、OpenRouter 或本機 Ollama，選「其他服務」。

### 會影響現有設定的變更

- **移除 Google 日曆連線。** 行程改成一鍵加入你選的行事曆。之前存的 Google 日曆憑證會被忽略，下次儲存時刪除。舊版存的預約模式「自動」會當成「**先問我**」。
- **預約簡訊從接那通電話的 SIM 卡發出**，或從你在「重傳簡訊」選的手機發出。
- **國碼移出控制台。** 改從 `COUNTRY_CODE`（或 `settings.json` 的 `country_code`）讀取；語言是繁體中文又沒設定時，預設 `886`。舊版存下的值仍然優先；刪掉那一行就會改用環境變數。
- **國際提示音的設定拿掉了。** 只有台灣 SIM 卡接到國外來電時，AI 才會等提示音播完，由程式自動判斷。
- **摘要服務**只剩兩個選項：「Gemini」（預設）或「其他服務」。舊的「DeepSeek」會變成「其他服務」；「自動」在有設定 API 網址或金鑰時變成「其他服務」，否則變成「Gemini」。

### 下載

| 檔案 | 適用 |
|---|---|
| `bageo-call-1.0.0-windows-x64.zip` | Windows 10/11 |
| `bageo-call-1.0.0-macos-arm64.tar.gz` | Apple Silicon 的 macOS |
| `bageo-call-1.0.0-macos-x64.tar.gz` | Intel 的 macOS |
| `bageo-call-1.0.0-linux-x64.tar.gz` | Linux x86-64 |
| `pixelcallai_uplink-1.0.0.zip` | 手機模組，裝在 Magisk 或 KernelSU |

每個桌面套件都包含主程式、1,080 個內建語音檔與背景音、啟動腳本和文件。Docker 使用者在 repository 執行 `docker compose up -d --build` 建置。

### 從 0.2.x 升級

1. 停止主程式，備份 `data/` 資料夾（有 `.env` 的話也一起）。
2. 解壓縮 1.0.0 套件，把 `data/` 和 `.env` 搬進去；或直接解壓縮蓋過舊資料夾。Docker：拉新程式碼後執行 `docker compose up -d --build`，資料磁碟區會保留。
3. 確認沒有通話進行中，在 Magisk 或 KernelSU 把新的手機模組直接裝在舊的上面，再重新開機。它用同一把專案金鑰簽章，App 資料會保留。
4. 啟動主程式。設定、通話紀錄和聯絡人都會沿用。每支手機第一次 AI 代打前，先讓它接一通電話，主程式才知道它的 SIM 卡。

### 1.0 怎麼測的

- CI 在每次變更都會跑超過 520 個單元與整合測試。
- 對真正的主程式跑了 234 通模擬通話，來電者講 28 種語言；15 種 AI 代打模擬情境全部通過。
- Pixel 7、7a、9 實機真實通話：接聽、插話、推銷與詐騙陪聊、預約簡訊、接通並講出答覆的 AI 回電，以及一般手機模式。

**還沒在真手機上看到的：** AI 代打實際撥出，以及回鈴音過濾面對真實回鈴音。Telegram 以外的通知管道是依各服務文件記載的格式測試。其他手機和語言的實測回報最有幫助，請見 [CONTRIBUTING.md](CONTRIBUTING.md)。

文件：[README](README.zh-TW.md) · [開始使用](docs/zh-TW/GETTING_STARTED.md) · [架構](docs/zh-TW/ARCHITECTURE.md) · [撰寫提示詞](docs/zh-TW/PROMPTS.md) · [發展藍圖](docs/zh-TW/ROADMAP.md)

---

## 简体中文

[English](#bageo-v100) · [繁體中文](#繁體中文) · **简体中文**

**它替你接电话，现在也替你打电话。**

Bageo 是用你原本手机号运行的开源 AI 电话助理，1.0 是第一个正式稳定版。来电依然在约 0.4 秒内，由自然、可以随时打断的 Gemini Live 语音接起。这一版的新东西：**AI 会替你打电话**，拿不准就先问你，打完汇报结果。来电想约时间会变成待处理事项，通讯录几乎什么文件都能导入，文字记录保留每种语言并附翻译，推销和诈骗电话要过三道关。

一切仍然在你自己的电脑上，用你的 SIM 卡和你自己的 Gemini 密钥运行：不需要 SIP 线路、不用租号码、没有平台月费。

### 亮点

- **AI 代打（新功能）。** 选联系人或输入号码，写下要办的事（订座、改期、和朋友约时间、问商家问题），再选 AI 以你的助理还是你本人的身份拨出。你可以看实时字幕、在 40 秒倒计时的提问卡上回答 AI、通话中打字交代，最后收到一份结构化结果和一条通知。你不在时，AI 绝不自己拍板：结果会等你决定，点一个按钮就能让 AI 带着你的答复和上一通的内容再打一次。
- **安全拨号。** 代打拒拨紧急电话、短号、高额付费号码和国际冠码号码。主程序会记住每部手机插的是哪国的 SIM 卡，只用号码所属国家的 SIM 卡拨出；手机回报的 SIM 卡不对，接通前就挂断。
- **日程预约变成待处理事项。** AI 记下请求，但不当场答应。你在控制台或 Telegram 卡片上点“同意”或“不同意”，再选对方怎么收到答复：用接听那通电话的 SIM 卡发短信、AI 回拨，或不通知。一键添加到 Google 日历、Outlook.com、Microsoft 365、Apple 日历、Yahoo 日历或 `.ics` 文件；主程序不保存任何日历凭据。
- **会等真人接起的 AI 回拨。** 回铃音期间 AI 保持安静（回铃音会被滤掉），等对方接起并开口，用原通话的语言把你的答复说一次，然后挂断。
- **什么格式都能导入的通讯录。** vCard 2.1/3.0/4.0、Google 或 Outlook CSV、TSV、纯文本、`.xlsx`、`.docx` 在本地导入，自动识别编码；PDF、名片和截图交给 Gemini Flash-Lite 识别。可以先预览、合并重复项、导出 vCard。熟人来电时，名字和你的备注会带进通话，防诈复审也会保护他们。
- **三道关拦截推销与诈骗。** 回复白名单、多语加权关键词（保险、贷款、软性电销、ATM 操作、礼品卡、社交软件诈骗），以及 `gemini-flash-lite` 的语义复审（含 `too_early` 延后再审与联系人保护）。银行打来核对刷卡不算诈骗：AI 不替你承认或否认那笔消费，摘要会提醒你自己打给银行。
- **语言。** 文字记录每一句保留原来的语言，并附上翻成你通知语言的对照。新增设置“其他人怎么称呼你”（`owner_aliases`）。台湾 SIM 卡接到境外来电时，AI 一律自己等运营商的防诈提示音播完。
- **更好的访谈提示词**，九种语言都有：它会问出你平时真会说的原话和绝不会说的词，写出来的提示词更不像背稿。
- **摘要默认用你的 Gemini 密钥**（最便宜的模型）。想用 DeepSeek、OpenAI、OpenRouter 或本地 Ollama，选“其他服务”。

### 影响现有设置的变更

- **移除 Google 日历连接。** 日程改为一键加入你选择的日历。旧版保存的 Google 日历凭据会被忽略，下次保存时清除。旧版保存的预约模式“自动”会按“**先问我**”运行。
- **预约短信从接听那通电话的 SIM 卡发出**，或从你在“重发短信”里选的手机发出。
- **国际电话区号移出控制台。** 改从 `COUNTRY_CODE`（或 `settings.json` 的 `country_code`）读取；语言为繁體中文且未设置时默认 `886`。旧版保存的值仍然优先；删掉那一行就会改用环境变量。
- **移除国际提示音开关。** 只有台湾 SIM 卡接到境外来电时，AI 才会等提示音播完，由程序自动判断。
- **摘要服务**只剩两个选项：“Gemini”（默认）或“其他服务”。旧版“DeepSeek”会变成“其他服务”；“自动”在填有 API 地址或密钥时变成“其他服务”，否则变成“Gemini”。

### 下载

| 文件 | 适用 |
|---|---|
| `bageo-call-1.0.0-windows-x64.zip` | Windows 10/11 |
| `bageo-call-1.0.0-macos-arm64.tar.gz` | Apple Silicon 的 macOS |
| `bageo-call-1.0.0-macos-x64.tar.gz` | Intel 的 macOS |
| `bageo-call-1.0.0-linux-x64.tar.gz` | Linux x86-64 |
| `pixelcallai_uplink-1.0.0.zip` | 手机模块，安装在 Magisk 或 KernelSU |

每个桌面套件都包含主程序、1,080 个内置语音文件与背景音、启动脚本和文档。Docker 用户在仓库根目录执行 `docker compose up -d --build` 构建。

### 从 0.2.x 升级

1. 停止主程序，备份 `data/` 文件夹（有 `.env` 的话也一并备份）。
2. 解压 1.0.0 套件，把 `data/` 和 `.env` 移进去；或直接解压覆盖旧文件夹。Docker：拉取新代码后执行 `docker compose up -d --build`，数据卷会保留。
3. 确认没有正在进行的通话，在 Magisk 或 KernelSU 中把新的手机模块直接覆盖安装，然后重启手机。模块用同一把项目密钥签名，应用数据会保留。
4. 启动主程序。设置、通话记录和联系人都会沿用。每部手机第一次 AI 代打前，先让它接一通电话，主程序才知道它的 SIM 卡。

### 1.0 怎么测的

- CI 在每次变更时都会跑 520 多项单元与集成测试。
- 对真实主程序跑了 234 通模拟通话，来电者说 28 种语言；15 种 AI 代打模拟场景全部通过。
- Pixel 7、7a、9 实机真实通话：接听、打断、推销与诈骗陪聊、预约短信、接通并说出答复的 AI 回拨，以及普通手机模式。

**还没在真手机上看到的：** AI 代打实际拨出，以及回铃音过滤面对真实回铃音。Telegram 以外的通知渠道是按各服务文档记载的格式测试。其他手机和语言的实测反馈最有帮助，详见 [CONTRIBUTING.md](CONTRIBUTING.md)。

文档：[README](README.zh-CN.md) · [开始使用](docs/zh-CN/GETTING_STARTED.md) · [架构](docs/zh-CN/ARCHITECTURE.md) · [编写提示词](docs/zh-CN/PROMPTS.md) · [路线图](docs/zh-CN/ROADMAP.md)
