# Bageo v1.0.0

**English** · [繁體中文](#繁體中文) · [简体中文](#简体中文)

Bageo 1.0 is the first stable release of the open-source AI voice for your own
phone number. It still answers your calls in about 0.4 seconds with a natural,
interruptible Gemini Live voice, and it now works in both directions: **the AI
makes calls for you**, asks you when it is unsure, and reports back. Appointments
became a to-do list with replies by text or AI call-back, contacts import from
almost any file, transcripts keep every language with a translation, and the
sales and scam defence gained a third layer.

Everything still runs on your own computer with your own SIM and your own
Gemini key: no SIP trunk, no rented number, no platform fee.

## Highlights

- **AI errands (new).** Pick a contact or type a number, write what needs doing
  (book a table, move an appointment, find a time with a friend, ask a shop
  something), choose whether the AI calls as your assistant or as you. Watch
  live captions, answer the AI's questions on a 40-second card, type
  instructions during the call, and get a structured result with one
  notification. When you are not there, the AI never accepts its own options:
  the result asks for your decision, and one button lets the AI call again with
  your reply and the earlier conversation.
- **Safe dialing.** Errands refuse emergency, short-code, premium-rate and
  international-prefix numbers, learn which SIM country each phone holds, dial
  only on a SIM of the number's country, and hang up before connecting if the
  phone reports another SIM.
- **Appointments as a to-do list.** The AI takes the request without agreeing;
  you press Accept or Decline on the console or on a Telegram card and choose
  how the caller hears back: a text from the SIM that took the call, an AI
  call-back, or nothing. One tap adds it to Google Calendar, Outlook.com,
  Microsoft 365, Apple Calendar, Yahoo Calendar or an `.ics` file. The agent
  holds no calendar credentials.
- **AI call-backs that wait for a person.** The AI stays silent through the
  ringback (filtered out), waits until the callee picks up and speaks, gives
  your answer once in the language of the original call, and hangs up.
- **Contacts.** Import vCard 2.1/3.0/4.0, Google or Outlook CSV, TSV, text,
  `.xlsx` and `.docx` locally with automatic encoding detection, or PDFs,
  business cards and screenshots through Gemini Flash-Lite. Preview, merge
  duplicates, export vCard. Known callers get their name and your note into the
  call, and are protected in the scam review.
- **Three layers against sales and scams.** A follow-up whitelist, weighted
  multilingual keywords (insurance, loans, soft sales, ATM, gift cards, chat-app
  scams) and a semantic second look by `gemini-flash-lite` with `too_early`
  re-checks and contact protection. A bank checking a card charge is not a scam;
  the AI neither confirms nor denies the charge and the summary tells you to
  call your bank.
- **Languages.** Transcripts keep each line in the language it was said, with a
  translation into your notification language. New *Other names callers use for
  you* (`owner_aliases`). The AI now always waits out the Taiwanese carriers'
  international-call warning on its own.
- **Interview prompt, upgraded** in all nine languages: it asks for the exact
  words you say and the ones you never would, and produces prompts that sound
  less scripted.
- **Summaries use your Gemini key by default** (its lowest-cost model); pick
  *Another service* for DeepSeek, OpenAI, OpenRouter or a local Ollama.

## Changes that affect existing setups

- **Google Calendar connection removed.** Calendar entries are added with one
  tap to the calendar you choose; saved Google Calendar credentials are ignored
  and dropped at the next save. An appointment mode of *auto* saved by an older
  version works as **Ask me first**.
- **Test settings removed.** The test-number demo calendar, the settings for
  the phone and SIM that texted test callers, and choosing a SIM by name are
  gone. Appointment texts go out from the SIM that took the call, or from the
  phone you pick under **Send the text again**.
- **Country calling code left the console.** It is read from `COUNTRY_CODE`
  (or `country_code` in `settings.json`); with 繁體中文 and nothing set,
  `886` is assumed. A value saved by an older version still wins; delete that
  line to use the environment variable.
- **The carrier-warning setting is gone.** The AI waits for the warning only
  when a Taiwanese SIM takes a call from abroad, decided automatically.
- **Summary service** has two choices: *Gemini* (default) or *Another service*.
  An older *DeepSeek* setting becomes *Another service*; *Automatic* becomes
  *Another service* when a base URL or key is set, else *Gemini*.

## Downloads

| File | For |
|---|---|
| `bageo-call-1.0.0-windows-x64.zip` | Windows 10/11 |
| `bageo-call-1.0.0-macos-arm64.tar.gz` | macOS on Apple Silicon |
| `bageo-call-1.0.0-macos-x64.tar.gz` | macOS on Intel |
| `bageo-call-1.0.0-linux-x64.tar.gz` | Linux x86-64 |
| `pixelcallai_uplink-1.0.0.zip` | The phone module, for Magisk or KernelSU |

Each desktop package holds the agent, the 1,080 bundled voice clips and
background sounds, the start scripts and the documents. Docker users build from
the repository with `docker compose up -d --build`.

## Upgrading from 0.2.x

1. Stop the agent and back up the `data/` folder (and `.env` if you have one).
2. Unpack the 1.0.0 package and move `data/` and `.env` into it, or unpack it
   over the old folder. Docker: pull the new code and run
   `docker compose up -d --build`; the data volume stays.
3. Install the new phone module over the old one in Magisk or KernelSU and
   reboot the phone. It is signed with the same project key, so the app keeps
   its data. Make sure no call is in progress.
4. Start the agent. Settings, call records and contacts carry over. Let each
   phone take one call before its first AI errand, so the agent learns its SIMs.

## How it was tested

- More than 520 unit and integration tests in CI.
- 234 simulated calls with callers in 28 languages against the real agent;
  15 simulated AI errand scenarios, all passing.
- Daily real calls in Traditional Chinese (Taiwan) on Pixel 7, 7a and 9; the
  Pixel 8 ran earlier releases. An appointment text from a US SIM was delivered;
  an AI call-back connected and gave its answer on a real call; normal phone mode
  was tried on a Pixel 7a.

**Not yet seen on real phones:** an AI errand dialing out, and the ringback
filter against a real ringback. Notification channels other than Telegram are
tested against each service's documented format. Reports are very welcome; see
[CONTRIBUTING.md](CONTRIBUTING.md).

Full documentation: [README](README.md) ·
[Getting started](docs/en/GETTING_STARTED.md) ·
[Architecture](docs/en/ARCHITECTURE.md) ·
[Writing the prompt](docs/en/PROMPTS.md) · [Roadmap](docs/en/ROADMAP.md)

---

## 繁體中文

[English](#bageo-v100) · **繁體中文** · [简体中文](#简体中文)

Bageo 1.0 是這套「讓 AI 語音接你自己電話號碼」開源專案的第一個正式穩定版。它照樣用自然、可以被插話的 Gemini Live 語音，大約 0.4 秒就接起你的來電；現在更是雙向的：**AI 會替你打電話**，拿不準時先問你，最後回報結果。行程預約變成待處理事項，可以用簡訊或 AI 回電答覆；通訊錄幾乎什麼檔案都能匯入；逐字稿保留每種語言並附翻譯；推銷與詐騙防線多了第三道。

一切仍然在你自己的電腦上、用你自己的 SIM 卡和 Gemini 金鑰運作：不需要 SIP 線路、不用租號碼、沒有平台月費。

## 重點

- **AI 代打（新功能）。** 選聯絡人或輸入號碼，寫下要處理的事（訂位、改期、跟朋友約時間、問店家問題），選擇 AI 用你的助理還是你本人的身分打。你可以看即時字幕、在 40 秒倒數的提問卡上回答 AI、通話中打字交代，最後收到一份結構化結果和一則通知。你不在的時候，AI 絕不採用自己提出的選項：結果會標成「要你決定」，按一個按鈕就能讓 AI 帶著你的回覆和上一通的內容再打一次。
- **安全撥號。** 代打拒撥緊急電話、短碼、高額付費電話和國際冠碼號碼；會學到每支手機插著哪國的 SIM 卡，只用號碼所屬國家的 SIM 卡撥出；手機回報的 SIM 卡不對時，在接通前就掛斷。
- **行程預約變成待處理事項。** AI 記下請求但不當場答應；你在控制台或 Telegram 卡片上按「同意」或「不同意」，並選擇對方怎麼收到答覆：從接那通電話的 SIM 卡傳簡訊、AI 回電，或不通知。一鍵加入 Google 日曆、Outlook.com、Microsoft 365、Apple 日曆、Yahoo 日曆或 `.ics` 檔。主程式不保存任何行事曆憑證。
- **會等真人接起的 AI 回電。** AI 在回鈴音期間保持安靜（回鈴音會被濾掉），等對方接起並開口，用原本那通電話的語言把你的答覆講一次，然後掛斷。
- **通訊錄。** 在本機匯入 vCard 2.1/3.0/4.0、Google 或 Outlook CSV、TSV、純文字、`.xlsx`、`.docx`，自動判斷編碼；PDF、名片和截圖交給 Gemini Flash-Lite 辨識。先預覽、合併重複、匯出 vCard。熟人來電時，名字和你的備註會帶進通話，防詐複審也會保護他們。
- **三道防線擋推銷與詐騙。** 回覆白名單、多語加權關鍵字（保險、貸款、軟性推銷、ATM、點數卡、通訊軟體詐騙），以及 `gemini-flash-lite` 的語意複審（含 `too_early` 延後再審與聯絡人保護）。銀行核對刷卡不算詐騙；AI 不替你承認或否認那筆消費，摘要會提醒你打給銀行確認。
- **語言。** 逐字稿每句保留原本的語言，並附上翻成你通知語言的對照。新增「其他人怎麼叫你」（`owner_aliases`）。台灣電信的國際電話防詐提示音，AI 現在一律自動等它播完。
- **訪談提示詞升級**，九種語言都有：會問你真的會說的原話和絕對不說的詞，產出的提示詞比較不像照稿念。
- **摘要預設用你的 Gemini 金鑰**（最便宜的模型）；想用 DeepSeek、OpenAI、OpenRouter 或本機 Ollama，選「其他服務」即可。

## 會影響現有設定的變更

- **移除 Google 日曆連線。** 行程改成一鍵加入你選的行事曆；之前存的 Google 日曆憑證會被忽略，下次儲存時刪除。舊版存的預約模式「自動」會當成「**先問我**」。
- **移除測試用設定。** 測試號碼的示範行事曆、替測試來電發簡訊的手機與 SIM 卡設定，以及依名稱選 SIM 卡的功能都拿掉了。預約簡訊從接那通電話的 SIM 卡發出，或從你在「重傳簡訊」選的手機發出。
- **國碼移出控制台。** 改從 `COUNTRY_CODE`（或 `settings.json` 的 `country_code`）讀取；語言是繁體中文又沒設定時，預設 `886`。舊版存下的值仍然優先；刪掉那一行就會改用環境變數。
- **國際提示音的設定拿掉了。** 只有台灣 SIM 卡接到國外來電時，AI 才會等提示音播完，由程式自動判斷。
- **摘要服務**只剩兩個選項：「Gemini」（預設）或「其他服務」。舊的「DeepSeek」會變成「其他服務」；「自動」在有設定 API 網址或金鑰時變成「其他服務」，否則變成「Gemini」。

## 下載

| 檔案 | 適用 |
|---|---|
| `bageo-call-1.0.0-windows-x64.zip` | Windows 10/11 |
| `bageo-call-1.0.0-macos-arm64.tar.gz` | Apple Silicon 的 macOS |
| `bageo-call-1.0.0-macos-x64.tar.gz` | Intel 的 macOS |
| `bageo-call-1.0.0-linux-x64.tar.gz` | Linux x86-64 |
| `pixelcallai_uplink-1.0.0.zip` | 手機模組，裝在 Magisk 或 KernelSU |

每個桌面套件都包含主程式、1,080 個內建語音檔與背景音、啟動腳本和文件。Docker 使用者從 repository 執行 `docker compose up -d --build` 建置。

## 從 0.2.x 升級

1. 停止主程式，備份 `data/` 資料夾（有 `.env` 的話也一起）。
2. 解壓縮 1.0.0 套件，把 `data/` 和 `.env` 搬進去；或直接解壓縮蓋過舊資料夾。Docker：拉新程式碼後執行 `docker compose up -d --build`，資料磁碟區會保留。
3. 在 Magisk 或 KernelSU 把新的手機模組直接裝在舊的上面，再重新開機。它用同一把專案金鑰簽章，App 資料會保留。請確認沒有通話進行中。
4. 啟動主程式。設定、通話紀錄和聯絡人都會沿用。每支手機第一次 AI 代打前，先讓它接一通電話，主程式才知道它的 SIM 卡。

## 怎麼驗證的

- CI 裡超過 520 個單元與整合測試。
- 對真正的主程式跑了 234 通模擬通話、來電者講 28 種語言；15 種 AI 代打模擬情境全部通過。
- 每天在 Pixel 7、7a、9 上用繁體中文（台灣）接真實來電；Pixel 8 跑過較早的版本。從美國 SIM 卡發出的預約簡訊確認送達；AI 回電在真實電話上接通並講出答覆；一般手機模式在 Pixel 7a 上試過。

**還沒在真手機上看到的：** AI 代打實際撥出，以及回鈴音過濾面對真實回鈴音。Telegram 以外的通知管道是依各服務文件記載的格式測試。非常歡迎回報，請見 [CONTRIBUTING.md](CONTRIBUTING.md)。

完整文件：[README](README.zh-TW.md) · [開始使用](docs/zh-TW/GETTING_STARTED.md) · [架構](docs/zh-TW/ARCHITECTURE.md) · [撰寫提示詞](docs/zh-TW/PROMPTS.md) · [發展藍圖](docs/zh-TW/ROADMAP.md)
---

## 简体中文

[English](#bageo-v100) · [繁體中文](#繁體中文) · **简体中文**

Bageo 1.0 是这套“用 AI 语音接听你自己手机号码”开源项目的第一个正式稳定版。它依然用自然、可随时打断的 Gemini Live 语音，约 0.4 秒接起你的来电；现在更是双向的：**AI 会替你打电话**，拿不准时先问你，结束后汇报结果。行程预约变成待处理事项，可用短信或 AI 回拨答复；通讯录几乎什么文件都能导入；逐字稿保留每种原语言并附翻译；推销与诈骗防线升级为三层。

一切依然在你自己的电脑上、用你自己的 SIM 卡与 Gemini 密钥运行：不需要 SIP 线路、不用租号码、没有平台月费。

### 亮点

- **AI 代打（新功能）。** 选联系人或输入号码，写下要办的事（订座、改期、和朋友约时间、问商家问题），选择 AI 以你的助理还是你本人的身份拨出。你可以看实时字幕、在 40 秒倒计时的提问卡上回答 AI、通话中打字交代，最后收到结构化结果与一条通知。你不在时，AI 绝不擅自拍板：结果会标为“要你决定”，点一个按钮就能让 AI 带着你的回复与上一通对话再打一次。
- **安全拨号。** 代打拒拨紧急电话、短号、高额付费号码与国际冠码号码；会学习每部手机插着哪国的 SIM 卡，只用号码所属国家的 SIM 卡拨出；若手机回报的实际 SIM 卡不符，在接通前立即挂断。
- **行程预约变成待处理事项。** AI 记下请求但不当场答应；你在控制台或 Telegram 卡片上点“同意”或“不同意”，并选择如何告知对方：用接听那通电话的 SIM 卡发短信、AI 回拨，或不通知。一键添加到 Google 日历、Outlook.com、Microsoft 365、Apple 日历、Yahoo 日历或下载 `.ics` 文件。主程序不保存任何日历凭据。
- **等待真人接听的 AI 回拨。** AI 在回铃音期间保持安静（自动滤除回铃音），等对方接起并开口后，用原通话语言把你的答复清楚说一次，随后挂断。
- **通讯录。** 在本地导入 vCard 2.1/3.0/4.0、Google 或 Outlook CSV、TSV、纯文本、`.xlsx`、`.docx`，自动识别编码；PDF、名片与截图交给 Gemini Flash-Lite 识别。支持预览、合并重复项、导出 vCard。熟人来电时自动带入姓名与备注，并在防诈复审中受保护。
- **三层防线拦截推销与诈骗。** 回复白名单、多语加权关键词（保险、贷款、软性电销、ATM、礼品点卡、社交软件诈骗），以及 `gemini-flash-lite` 语义复审（支持 `too_early` 延后再审与联系人保护）。银行核对刷卡不算诈骗；AI 不替你承认或否认该笔消费，摘要会提醒你自行致电银行核实。
- **语言。** 逐字稿每句保留原语言，并附上通知语言的对照翻译。新增“其他人怎么称呼你”（`owner_aliases`）。台湾运营商的国际来电防诈警示音现已一律自动等待播完。
- **访谈提示词全面升级**，覆盖全部九种语言：会询问你平时脱口而出的原话与绝不会说的词，生成的提示词更自然、不像背稿。
- **摘要默认使用你的 Gemini 密钥**（最省钱的模型）；想用 DeepSeek、OpenAI、OpenRouter 或本地 Ollama，选“其他服务”即可。

### 影响现有设置的变更

- **移除 Google 日历连接。** 日程改为一键加入你选择的日历；旧版保存的 Google 日历凭据会被忽略并在下次保存时清除。旧版保存的预约模式“自动”将按“**先问我**”运行。
- **移除测试专用设置。** 测试号码的演示日历、测试来电发短信的手机与 SIM 卡设置，以及按名称选 SIM 卡的功能均已移除。预约短信默认从接听该通电话的 SIM 卡发出，或从你在“重发短信”里选择的手机发出。
- **国际电话区号移出控制台界面。** 改从环境变量 `COUNTRY_CODE`（或 `settings.json` 的 `country_code`）读取；语言为繁體中文且未设置时默认 `886`。旧版保存的值仍优先。
- **移除国际提示音开关。** 仅当台湾 SIM 卡接到境外来电时，AI 才会自动等待提示音播完。
- **摘要服务**简化为两个选项：“Gemini”（默认）或“其他服务”。旧版“DeepSeek”自动转为“其他服务”；“自动”在填有 API 地址或密钥时转为“其他服务”，否则转为“Gemini”。

### 下载

| 文件 | 适用平台 |
|---|---|
| `bageo-call-1.0.0-windows-x64.zip` | Windows 10/11 |
| `bageo-call-1.0.0-macos-arm64.tar.gz` | Apple Silicon 的 macOS |
| `bageo-call-1.0.0-macos-x64.tar.gz` | Intel 的 macOS |
| `bageo-call-1.0.0-linux-x64.tar.gz` | Linux x86-64 |
| `pixelcallai_uplink-1.0.0.zip` | 手机模块，安装在 Magisk 或 KernelSU |

每个桌面套件都包含主程序、1,080 个内置语音文件与背景音、启动脚本和文档。Docker 用户在仓库根目录执行 `docker compose up -d --build` 构建。

### 从 0.2.x 升级

1. 停止主程序，备份 `data/` 文件夹（如果有 `.env` 也一并备份）。
2. 解压 1.0.0 套件，把 `data/` 和 `.env` 移进去；或直接解压覆盖旧文件夹。Docker：拉取新代码后执行 `docker compose up -d --build`，数据卷会保留。
3. 在 Magisk 或 KernelSU 中直接覆盖安装新的手机模块，然后重启手机。模块使用同一把项目密钥签名，应用数据会保留。安装前请确认没有正在进行的通话。
4. 启动主程序。设置、通话记录和联系人都会沿用。每部手机第一次执行 AI 代打前，请先让它接听一通电话，主程序才能记住它的 SIM 卡国家。

### 怎么验证的

- CI 中超过 520 项单元与集成测试。
- 对真实主程序跑了 234 通模拟通话、来电者覆盖 28 种语言；15 种 AI 代打模拟场景全部通过。
- 每天在 Pixel 7、7a、9 上用繁体中文（台湾）接听真实来电；Pixel 8 跑过较早的版本。从美国 SIM 卡发出的预约短信已确认送达；AI 回拨在真实电话上接通并说出答复；普通手机模式在 Pixel 7a 上实测通过。

**尚未在真手机上实测的：** AI 代打实际拨出，以及回铃音过滤面对真实回铃音。Telegram 以外的通知渠道是按各服务文档记载的格式测试。非常欢迎反馈使用情况，详见 [CONTRIBUTING.md](CONTRIBUTING.md)。

完整文档：[README](README.zh-CN.md) · [开始使用](docs/zh-CN/GETTING_STARTED.md) · [架构](docs/zh-CN/ARCHITECTURE.md) · [编写提示词](docs/zh-CN/PROMPTS.md) · [发展蓝图](docs/zh-CN/ROADMAP.md)
