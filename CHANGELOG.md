# Bageo v1.2.0

- **Contacts in four lists, and whitelisted callers can reach you.** Each contact is a plain contact, whitelisted, blacklisted (rejected) or on the stall list (kept talking from the first second). For whitelisted callers you choose: the AI answers; the AI answers and gives them your number when they say it is urgent; or the call goes straight to your own phone through your carrier's forward-when-busy. See [Features](docs/en/FEATURES.md) and [Settings](docs/en/SETTINGS.md#blocked-numbers-and-contact-lists).
- **聯絡人分四種名單，白名單來電可以轉給你。** 每位聯絡人可以是一般聯絡人、白名單、黑名單（拒接）或陪聊名單（一接起就陪聊）。白名單的人打來有三種做法：AI 接聽；AI 先接，對方說有急事才把你的號碼告訴他；或靠電信的忙線轉接直接轉到你的手機。見[功能](docs/zh-TW/FEATURES.md)與[設定](docs/zh-TW/SETTINGS.md)。
- **联系人分四种名单，白名单来电可以转给你。** 每位联系人可以是普通联系人、白名单、黑名单（拒接）或陪聊名单（一接起就陪聊）。白名单的人打来有三种做法：AI 接听；AI 先接，对方说有急事才把你的号码告诉他；或靠运营商的遇忙转移直接转到你的手机。见[功能](docs/zh-CN/FEATURES.md)与[设置](docs/zh-CN/SETTINGS.md)。
- **Console password.** Optional. Once set, the web console and the API ask for it (`ui_password`). See [Settings](docs/en/SETTINGS.md#advanced).
- **控制台密碼。** 可設可不設。設了以後，打開控制台和呼叫 API 都要輸入密碼（`ui_password`）。見[設定](docs/zh-TW/SETTINGS.md)。
- **控制台密码。** 可设可不设。设了以后，打开控制台和调用 API 都要输入密码（`ui_password`）。见[设置](docs/zh-CN/SETTINGS.md)。
- **One-tap actions in every notification channel.** Under a call summary: block, stall or whitelist the caller. On an appointment: accept or decline. Telegram shows buttons; other channels get signed links. Notifications are plain text now, without emoji. See [Notifications](docs/en/NOTIFICATIONS.md).
- **每個通知管道都能一鍵處理。** 通話摘要下方可以把對方加進黑名單、陪聊名單或白名單；預約可以接受或拒絕。Telegram 是按鈕，其他管道是簽章連結。通知改成純文字，不再有表情符號。見[通知](docs/zh-TW/NOTIFICATIONS.md)。
- **每个通知渠道都能一键处理。** 通话摘要下方可以把对方加进黑名单、陪聊名单或白名单；预约可以接受或拒绝。Telegram 是按钮，其他渠道是签名链接。通知改成纯文本，不再有表情符号。见[通知](docs/zh-CN/NOTIFICATIONS.md)。
- **Scam detection knows more and reacts sooner.** Local scam patterns for 12 countries; debt-collection and harassment calls are stalled too, while polite reminders from banks, clinics and couriers stay normal calls; a romance-scam category and about 800 more keywords in nine languages. When a caller asks for something only a scam asks for (a verification code, ATM steps, a "safe account", screen sharing, gift cards), stalling starts at once with no second check; saved contacts are still checked first. Keyword lookup uses an index and stays well under a millisecond per sentence.
- **詐騙辨識懂得更多、反應更快。** 內建 12 個國家的在地詐騙話術；催收和騷擾電話也會陪聊，銀行、診所、物流的一般提醒照常應對；新增感情詐騙類別，九種語言再加約 800 個關鍵字。對方一要只有詐騙才會要的東西（驗證碼、操作 ATM、「安全帳戶」、螢幕分享、點數卡），馬上開始陪聊，不再等第二次檢查；已存的聯絡人仍會先檢查。關鍵字查找改用索引，一句話遠低於一毫秒。
- **诈骗识别懂得更多、反应更快。** 内置 12 个国家的本地诈骗话术；催收和骚扰电话也会陪聊，银行、诊所、快递的普通提醒照常应对；新增感情诈骗类别，九种语言再加约 800 个关键词。对方一要只有诈骗才会要的东西（验证码、操作 ATM、“安全账户”、屏幕共享、礼品卡），马上开始陪聊，不再等第二次检查；已保存的联系人仍会先检查。关键词查找改用索引，一句话远低于一毫秒。
- **AI errands stay in the right language.** The AI switches language only after the other side has said a whole sentence in it, and only to that language. The carrier's warning phrase is no longer in the speech-to-text vocabulary of calls the AI dials.
- **AI 代打不再亂換語言。** 對方完整講了一句別的語言，AI 才會換過去，而且只換成那一種。AI 撥出去的電話，語音辨識詞彙不再帶電信的提示語。
- **AI 代打不再乱换语言。** 对方完整说了一句别的语言，AI 才会换过去，而且只换成那一种。AI 拨出去的电话，语音识别词汇不再带运营商的提示语。
- **Plainer console and guides.** Shorter hints in all nine console languages; rarely used settings sit under "More options" or Advanced. The guides in English, 繁體中文 and 简体中文 were rewritten to be read at a glance. Field names and saved values are unchanged.
- **控制台和說明文件改成白話。** 控制台九種語言的說明都縮短了，少用的設定收進「更多選項」或「進階」。英文、繁體中文、簡體中文的說明文件整份重寫，一眼就能看到重點。欄位名稱和已存的設定值都沒變。
- **控制台和说明文档改成白话。** 控制台九种语言的说明都缩短了，少用的设置收进“更多选项”或“高级”。英文、繁体中文、简体中文的说明文档整份重写，一眼就能看到重点。字段名称和已保存的设置值都没变。

# Bageo v1.1.0

- **16 more notification channels (32 in all).** Microsoft Teams (Workflows
  webhook with an Adaptive Card), Google Chat, Rocket.Chat, Zulip, Webex,
  Nextcloud Talk, Synology Chat, Signal (through signal-cli-rest-api),
  Pushbullet, PushPlus, PushDeer, WxPusher, Chanify, Home Assistant notify
  services, IFTTT Webhooks, and email through any SMTP server with TLS
  (STARTTLS or port 465). Built from each service's public documentation and
  covered by unit tests; not yet tried with live accounts. See
  [Notifications](docs/en/NOTIFICATIONS.md).
- **通知管道再加 16 種（共 32 種）。** Microsoft Teams（Workflows Webhook，送 Adaptive Card）、Google Chat、Rocket.Chat、Zulip、Webex、Nextcloud Talk、Synology Chat、Signal（透過 signal-cli-rest-api）、Pushbullet、PushPlus、PushDeer、WxPusher、Chanify、Home Assistant 通知服務、IFTTT Webhooks，以及任何支援 TLS 的 SMTP 郵件伺服器（STARTTLS 或 465 埠）。依各服務的公開文件實作並有單元測試，還沒有在真實帳號上試過。見[通知](docs/zh-TW/NOTIFICATIONS.md)。
- **通知渠道再加 16 种（共 32 种）。** Microsoft Teams（Workflows Webhook，发送 Adaptive Card）、Google Chat、Rocket.Chat、Zulip、Webex、Nextcloud Talk、Synology Chat、Signal（通过 signal-cli-rest-api）、Pushbullet、PushPlus、PushDeer、WxPusher、Chanify、Home Assistant 通知服务、IFTTT Webhooks，以及任意支持 TLS 的 SMTP 邮件服务器（STARTTLS 或 465 端口）。按各服务的公开文档实现并有单元测试，还没有在真实账号上试过。见[通知](docs/zh-CN/NOTIFICATIONS.md)。
- **Chat commands (Telegram).** From the chat your notifications go to, look up calls (`/calls`, `/call`), have the AI call someone (`/errand` or plain words), decide to-dos, answer the AI's questions during its call and switch a phone's mode. Off until switched on per chat; calls, mode switches and to-do decisions wait for your OK; other chats get no answer. See [docs/en/CHAT_COMMANDS.md](docs/en/CHAT_COMMANDS.md).
- **在聊天室操作（Telegram）。** 在收通知的聊天室就能查通話紀錄（`/calls`、`/call`）、叫 AI 代打（`/errand` 或直接打字）、處理待辦、回答 AI 通話中的問題、切換手機模式。要在每個聊天室各自打開；撥電話、切換模式、處理待辦都會先問你確認；其他聊天室一律不回覆。見 [docs/zh-TW/CHAT_COMMANDS.md](docs/zh-TW/CHAT_COMMANDS.md)。
- **在聊天室操作（Telegram）。** 在接收通知的聊天室就能查通话记录（`/calls`、`/call`）、让 AI 代打（`/errand` 或直接打字）、处理待办、回答 AI 通话中的问题、切换手机模式。要在每个聊天室各自打开；拨电话、切换模式、处理待办都会先问你确认；其他聊天室一律不回复。见 [docs/zh-CN/CHAT_COMMANDS.md](docs/zh-CN/CHAT_COMMANDS.md)。
- **MCP server for AI agents.** Claude, ChatGPT, Grok, Gemini CLI, OpenClaw and any other agent that uses remote MCP servers can read your call log, calls, to-dos and AI calls, and, if you allow it, ask for an AI call (`mcp_calls`: `confirm` waits for your yes in your command chat, `direct` queues it). Off and silent until enabled with a token; browser origins must be allowed; same checks, limits and key scrubbing as chat commands. See [docs/en/MCP.md](docs/en/MCP.md).
- **給 AI 代理用的 MCP 伺服器。** Claude、ChatGPT、Grok、Gemini CLI、OpenClaw（龍蝦）等能接遠端 MCP 的代理，都能查通話紀錄、單通內容、待辦和 AI 代打；你允許的話還能提出代打（`mcp_calls`：`confirm` 等你在聊天室按執行，`direct` 直接排入）。要設 token 才會開、之前完全不回應；網頁來源要列入允許清單；檢查、上限、遮掉金鑰都跟聊天室指令一樣。見 [docs/zh-TW/MCP.md](docs/zh-TW/MCP.md)。
- **给 AI 代理用的 MCP 服务器。** Claude、ChatGPT、Grok、Gemini CLI、OpenClaw（龙虾）等能接远程 MCP 的代理，都能查通话记录、单通内容、待办和 AI 代打；你允许的话还能提出代打（`mcp_calls`：`confirm` 等你在聊天室按执行，`direct` 直接排入）。要设 token 才会开、之前完全不响应；网页来源要列入允许列表；检查、上限、遮掉密钥都跟聊天室指令一样。见 [docs/zh-CN/MCP.md](docs/zh-CN/MCP.md)。
- **Sharper scam and sales detection, in 19 languages.** What sales and scam
  calls sound like is now a database (42 kinds of call, keywords in 19
  languages) instead of word lists in the code, and you can add your own words
  and numbers in `config/scam_db.local.json`. Calls without known words are
  looked at too, so new scripts are caught; an unsure answer waits for the
  caller's next turn; a review that fails convicts nobody unless they asked for
  scam-only things (a code, ATM steps, a safe account); a caller judged wrongly
  is let go back to a normal conversation. Each call's page shows what the call
  was judged and lets you mark it scam, sales or not; the number's next calls
  are judged with your mark. See [Settings](docs/en/SETTINGS.md#sales--scam-calls).
- **繁體中文：詐騙與推銷辨識更準，支援 19 種語言。** 推銷與詐騙的說法改成資料庫（42 種話術、19 種語言的關鍵字），不再寫死在程式裡，也可以在 `config/scam_db.local.json` 加自己的字眼和號碼。沒踩到已知字眼的電話也會檢查，新話術也抓得到；模型不確定時等對方下一句再看；複審失敗時，除非對方要驗證碼、操作 ATM、轉安全帳戶這類只有詐騙才會要的東西，不再一律當詐騙；誤判的來電會放回正常應對。通話頁會顯示判斷結果，可以標「是詐騙」「是推銷」「不是」，這個號碼之後打來會參考你的標記。
- **简体中文：诈骗与推销识别更准，支持 19 种语言。** 推销与诈骗的说法改成数据库（42 种话术、19 种语言的关键词），不再写死在程序里，也可以在 `config/scam_db.local.json` 加自己的词和号码。没碰到已知词的电话也会检查，新话术也能抓到；模型不确定时等对方下一句再看；复审失败时，除非对方要验证码、操作 ATM、转安全账户这类只有诈骗才会要的东西，不再一律当诈骗；误判的来电会放回正常应对。通话页会显示判断结果，可以标“是诈骗”“是推销”“不是”，这个号码之后打来会参考你的标记。
- **The AI asks times and dates back before it counts on them.** On AI errands
  it reads every time, date, quantity, price, address, phone number or booking
  name back as a question as soon as it comes up, and waits for a yes before
  any thanks or goodbye; the result and the notification mark anything the
  other side never clearly confirmed as ⚠️ *Not confirmed by the other side*.
  An appointment request is recorded only after the caller said yes to the time
  read back in full (`propose_appointment` gets `confirmed_by_caller`; without
  it the AI is sent back once to ask, and a time the caller hangs up on before
  that still goes to your to-do list, marked unconfirmed). On normal calls the AI asks a time, date,
  amount or phone number back once as a question. Stall Mode is unchanged.
- **繁體中文：時間、日期先問清楚再算數。** AI 代打時，對方一講到時間、日期、數量、金額、地址、電話號碼或訂位名字，AI 馬上用問句複誦、等對方說對，確認前不道謝、不道別；對方沒有明確確認的項目，結果和通知會標 ⚠️「未經對方確認」。行程預約要等來電者對完整複誦的時間說對才記下（`propose_appointment` 新增 `confirmed_by_caller`，沒帶會退回一次要 AI 先問；對方在補問前就掛斷的話，照樣放進待處理事項並標示未確認）。一般來電講到時間、日期、金額或電話號碼，AI 會用問句複誦一次。陪聊模式不變。
- **简体中文：时间、日期先问清楚再算数。** AI 代打时，对方一说到时间、日期、数量、金额、地址、电话号码或订位名字，AI 马上用问句复诵、等对方说对，确认前不道谢、不道别；对方没有明确确认的项目，结果和通知会标 ⚠️“未经对方确认”。日程预约要等来电者对完整复诵的时间说对才记下（`propose_appointment` 新增 `confirmed_by_caller`，没带会退回一次让 AI 先问；对方在补问前就挂断的话，照样放进待办事项并标注未确认）。普通来电说到时间、日期、金额或电话号码，AI 会用问句复诵一次。陪聊模式不变。

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


### 下載

| 檔案 | 適用 |
|---|---|
| `bageo-call-1.0.0-windows-x64.zip` | Windows 10/11 |
| `bageo-call-1.0.0-macos-arm64.tar.gz` | Apple Silicon 的 macOS |
| `bageo-call-1.0.0-macos-x64.tar.gz` | Intel 的 macOS |
| `bageo-call-1.0.0-linux-x64.tar.gz` | Linux x86-64 |
| `pixelcallai_uplink-1.0.0.zip` | 手機模組，裝在 Magisk 或 KernelSU |

每個桌面套件都包含主程式、1,080 個內建語音檔與背景音、啟動腳本和文件。Docker 使用者在 repository 執行 `docker compose up -d --build` 建置。


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


### 下载

| 文件 | 适用 |
|---|---|
| `bageo-call-1.0.0-windows-x64.zip` | Windows 10/11 |
| `bageo-call-1.0.0-macos-arm64.tar.gz` | Apple Silicon 的 macOS |
| `bageo-call-1.0.0-macos-x64.tar.gz` | Intel 的 macOS |
| `bageo-call-1.0.0-linux-x64.tar.gz` | Linux x86-64 |
| `pixelcallai_uplink-1.0.0.zip` | 手机模块，安装在 Magisk 或 KernelSU |

每个桌面套件都包含主程序、1,080 个内置语音文件与背景音、启动脚本和文档。Docker 用户在仓库根目录执行 `docker compose up -d --build` 构建。


### 1.0 怎么测的

- CI 在每次变更时都会跑 520 多项单元与集成测试。
- 对真实主程序跑了 234 通模拟通话，来电者说 28 种语言；15 种 AI 代打模拟场景全部通过。
- Pixel 7、7a、9 实机真实通话：接听、打断、推销与诈骗陪聊、预约短信、接通并说出答复的 AI 回拨，以及普通手机模式。

**还没在真手机上看到的：** AI 代打实际拨出，以及回铃音过滤面对真实回铃音。Telegram 以外的通知渠道是按各服务文档记载的格式测试。其他手机和语言的实测反馈最有帮助，详见 [CONTRIBUTING.md](CONTRIBUTING.md)。

文档：[README](README.zh-CN.md) · [开始使用](docs/zh-CN/GETTING_STARTED.md) · [架构](docs/zh-CN/ARCHITECTURE.md) · [编写提示词](docs/zh-CN/PROMPTS.md) · [路线图](docs/zh-CN/ROADMAP.md)
