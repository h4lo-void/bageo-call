# Bageo v1.6.9

- **Ready-made Docker images.** Starting with 1.6.9, each release is also on Docker Hub as `bageo/bageo-call` and on GitHub as `ghcr.io/h4lo-void/bageo-call`, for amd64 and arm64. Each release has two tags: its version, like `1.6.9`, and `latest`. See [Getting started](docs/en/GETTING_STARTED.md#with-docker).
- **現成的 Docker 映像。** 從 1.6.9 起，每一版也會放上 Docker Hub（`bageo/bageo-call`）和 GitHub（`ghcr.io/h4lo-void/bageo-call`），支援 amd64 和 arm64。每一版有兩個標籤：版本號（例如 `1.6.9`）和 `latest`。見[開始使用](docs/zh-TW/GETTING_STARTED.md#用-docker)。
- **现成的 Docker 镜像。** 从 1.6.9 起，每一版也会放到 Docker Hub（`bageo/bageo-call`）和 GitHub（`ghcr.io/h4lo-void/bageo-call`），支持 amd64 和 arm64。每一版有两个标签：版本号（例如 `1.6.9`）和 `latest`。见[开始使用](docs/zh-CN/GETTING_STARTED.md#用-docker)。
- **Forward to your phone, and choose who.** Before, only whitelisted callers could be forwarded. Under *Who can be forwarded* you now tick *Whitelist* (on by default), *Other contacts* or *Numbers not in your contacts* (hidden numbers too). Forwarding is one switch, *Turn on forwarding*, off by default, with two ways: *Don't answer: send to your phone*, or *AI answers, puts you through if urgent*, where the AI asks the caller to hold and the phone calls you (with no number filled in you only get an alert). People on the Blacklist or the Stall list, callbacks, AI errands and your own outgoing calls are never forwarded. Once a call is judged a sales or scam call, Bageo never calls you, and a number not in your contacts can make it call you at most once in 30 minutes. Turning it on first shows five short points to agree to. This does not replace emergency services. See [Settings](docs/en/SETTINGS.md#how-it-answers).
- **轉接到你的手機，誰打來可以轉由你決定。** 以前只有白名單能轉，現在可以在「誰打來可以轉」勾選「白名單」（預設勾選）、「其他聯絡人」或「不在聯絡人裡的號碼」（包含未顯示號碼）。轉接是一個「開啟轉接」開關，預設關閉，有兩種方式：「不接，直接轉到你的手機」，或「AI 先接，有急事才轉給你」：AI 會請對方稍等並打給你，沒填你的號碼就只發通知。黑名單、陪聊名單的人，以及回撥、AI 代打和你自己撥出的電話都不會轉。已經判定是推銷或詐騙的電話不會打給你；不在聯絡人裡的號碼，30 分鐘內最多讓 Bageo 打給你一次。打開之前，會先列出五點說明請你確認。這不能代替報警或叫救護車。見[設定參考](docs/zh-TW/SETTINGS.md#接聽方式)。
- **转接到你的手机，谁打来可以转由你决定。** 以前只有白名单能转，现在可以在“谁打来可以转”勾选“白名单”（默认勾选）、“其他联系人”或“不在联系人里的号码”（包括未显示号码）。转接是一个“开启转接”开关，默认关闭，有两种方式：“不接，直接转到你的手机”，或“AI 先接，有急事才转给你”：AI 会请对方稍等并打给你，没填你的号码就只发通知。黑名单、陪聊名单里的人，以及回拨、AI 代打和你自己拨出的电话都不会转。已经判定是推销或诈骗的电话不会打给你；不在联系人里的号码，30 分钟内最多让 Bageo 打给你一次。打开之前，会先列出五点说明请你确认。这不能代替报警或叫救护车。见[设置参考](docs/zh-CN/SETTINGS.md#接听方式)。
- **No background sound while Bageo calls you about something urgent, and the summary knows how it ended.** While the phone dials you, the background sound stops, so you no longer hear room noise when you pick up. It comes back only if you don't answer and the AI goes back to the caller. The call summary now says whether you were put through, could not be reached, or only got an alert, and a missed urgent call reminds you to call back.
- **有急事打給你時不再有背景音，摘要也知道結果。** 手機撥給你的時候背景音會停，你接起來不會再聽到居家雜音；只有你沒接、AI 回去跟對方講話時才恢復。通話摘要會寫出這通是接上你、沒找到你，還是只發了通知；沒接到的急事會提醒你盡快回電。
- **有急事打给你时不再有背景音，摘要也知道结果。** 手机拨给你的时候背景音会停，你接起来不会再听到居家杂音；只有你没接、AI 回去跟对方说话时才恢复。通话摘要会写出这通是接上你、没找到你，还是只发了通知；没接到的急事会提醒你尽快回电。
- **Pair a phone by IP address, no USB needed (handy for Docker).** On the console's *System* page, choose *Get a pairing code*; in the phone app, tap *Connect by IP address* and type Bageo's IP address and the 6-digit code. The code lasts 10 minutes and works once. Docker only needs the console port, under whatever number you publish it. See [Phone](docs/en/PHONE.md).
- **輸入 IP 位址就能配對手機，不用 USB（Docker 也適用）。** 在控制台「系統」頁按「產生配對碼」，再到手機 App 點「用 IP 位址連線」，輸入 Bageo 的 IP 位址和 6 位數配對碼。配對碼 10 分鐘內有效、只能用一次。Docker 只要開控制台那一個埠，對外換成別的埠號也可以。見[手機](docs/zh-TW/PHONE.md)。
- **输入 IP 地址就能配对手机，不用 USB（Docker 也适用）。** 在控制台“系统”页按“生成配对码”，再到手机 App 点“用 IP 地址连接”，输入 Bageo 的 IP 地址和 6 位数配对码。配对码 10 分钟内有效、只能用一次。Docker 只要开放控制台那一个端口，对外换成别的端口号也可以。见[手机](docs/zh-CN/PHONE.md)。
- **Bageo Status keeps showing `bageo.local`.** Once the console address has opened on this computer, one slow name lookup no longer switches the window back to `127.0.0.1:8787`; only three failed checks in a row do.
- **Bageo Status 的控制台網址固定顯示 `bageo.local`。** 這台電腦打開過一次之後，不會因為一次查詢太慢就跳回 `127.0.0.1:8787`，要連續三次都連不上才會換。
- **Bageo Status 的控制台网址固定显示 `bageo.local`。** 这台电脑打开过一次之后，不会因为一次查询太慢就跳回 `127.0.0.1:8787`，要连续三次都连不上才会换。
- **Bageo Status tells you when a call summary is ready.** Next to *When a call comes in* and *When a text message arrives* there is now *When a call summary is ready*, on by default. The notice shows who called and the key points, up to four lines; click it to open that call. See [Getting started](docs/en/GETTING_STARTED.md#see-the-status-in-the-system-tray-or-menu-bar).
- **Bageo Status 會在通話摘要寫好時通知你。**「有來電時」「收到簡訊時」旁邊多了「通話摘要寫好時」，預設開啟。通知會顯示誰打來和這通電話的重點，最多四行，點一下就打開那通電話。見[開始使用](docs/zh-TW/GETTING_STARTED.md#在系統匣或選單列看狀態)。
- **Bageo Status 会在通话摘要写好时通知你。**“有来电时”“收到短信时”旁边多了“通话摘要写好时”，默认开启。通知会显示谁打来和这通电话的重点，最多四行，点一下就打开那通电话。见[开始使用](docs/zh-CN/GETTING_STARTED.md#在系统托盘或菜单栏看状态)。
- **A safer console on your network, and backup files you can delete.** The console answers only to this computer's own names and addresses and to the one under *Address in notifications*, so a website cannot reach it through a name it controls. A new console password needs at least 8 characters, and another device that keeps entering wrong passwords waits a minute after 10. On macOS, `bageo.local` keeps working after the first two minutes: the system's Bonjour now answers it. Each file under *Backup files* can be deleted. See [Settings](docs/en/SETTINGS.md#using-the-console-from-other-devices).
- **區網上的控制台更安全，備份檔也能刪除。** 控制台只接受這台電腦自己的名稱、位址，以及「通知裡的網址」，其他網站沒辦法借自己的網域連進來。新設的控制台密碼至少 8 個字元；其他裝置連續打錯 10 次要等 1 分鐘。macOS 上的 `bageo.local` 過了前兩分鐘也照常能用，改由系統的 Bonjour 回應。「備份檔」的每一份都能刪除。見[設定參考](docs/zh-TW/SETTINGS.md#從其他裝置使用控制台)。
- **局域网上的控制台更安全，备份文件也能删除。** 控制台只接受这台电脑自己的名称、地址，以及“通知里的地址”，其他网站没办法借自己的域名连进来。新设置的控制台密码至少 8 个字符；其他设备连续输错 10 次要等 1 分钟。macOS 上的 `bageo.local` 过了前两分钟也照常能用，改由系统的 Bonjour 应答。“备份文件”的每一份都能删除。见[设置参考](docs/zh-CN/SETTINGS.md#从其他设备使用控制台)。
- **Open the console at `http://bageo.local/` on this computer and on phones and computers on the same Wi-Fi.** With `AGENT_HOST` not set (the installers' default), the console listens on your network, also at port 80 when it can (`CONSOLE_PORT_80=off` keeps that port closed). Other devices get in only once a console password is set; until then they see a page that says so. Links in notifications use this address by default; change it under *Settings → Notifications → Address in notifications*. Bageo Status and *System → About* show the address. On a new install the console takes your browser's language. Windows asks once to allow Bageo on the network: choose *Private networks*. See [Settings](docs/en/SETTINGS.md#using-the-console-from-other-devices) and [Troubleshooting](docs/en/TROUBLESHOOTING.md#faq).
- **這台電腦、同一個 Wi-Fi 的手機和電腦，都能用 `http://bageo.local/` 打開控制台。** 沒設 `AGENT_HOST`（安裝版的預設）時，控制台對同一個網路開放，能的話也開在 80 埠（`CONSOLE_PORT_80=off` 可以不開）。其他裝置要等設好控制台密碼才進得去，之前只會看到「請先設密碼」。通知裡的連結預設就用這個網址，可以在「設定 → 通知 → 通知裡的網址」改。Bageo Status 和「系統 → 關於」都會顯示這個網址。全新安裝時，控制台會跟著瀏覽器的語言。Windows 第一次會問要不要允許 Bageo 存取網路，請選「私人網路」。見[設定參考](docs/zh-TW/SETTINGS.md#從其他裝置使用控制台)與[疑難排解](docs/zh-TW/TROUBLESHOOTING.md#常見問題)。
- **这台电脑、同一个 Wi-Fi 下的手机和电脑，都能用 `http://bageo.local/` 打开控制台。** 没设 `AGENT_HOST`（安装版的默认）时，控制台对同一个网络开放，能的话也开在 80 端口（`CONSOLE_PORT_80=off` 可以不开）。其他设备要等设置好控制台密码才进得去，之前只会看到“请先设置密码”。通知里的链接默认就用这个地址，可以在“设置 → 通知 → 通知里的地址”修改。Bageo Status 和“系统 → 关于”都会显示这个地址。全新安装时，控制台会跟着浏览器的语言。Windows 第一次会问要不要允许 Bageo 访问网络，请选“专用网络”。见[设置参考](docs/zh-CN/SETTINGS.md#从其他设备使用控制台)与[故障排查](docs/zh-CN/TROUBLESHOOTING.md#常见问题)。
- **Switching back to a normal phone no longer gets stuck on Do Not Disturb (Android 15 and later).** The app turned on a Do Not Disturb mode of its own (named after Bageo in *Settings → Modes*) that the module could not turn off, so the switch failed with "Do Not Disturb could not be changed" and the phone stayed in AI mode. The app no longer turns that mode on and turns off one an older version left, and the module turns Do Not Disturb on by itself after a reboot, so AI mode stays just as silent. If another mode still keeps Do Not Disturb on, the app names it and says where to turn it off, without retrying by itself. Android 14 and earlier work as before. Update the phone module to get it. See [Phone](docs/en/PHONE.md#if-a-switch-does-not-finish).
- **切回一般手機不會再卡在勿擾（Android 15 以上）。** App 會自己開一個用 Bageo 命名的勿擾模式（在「設定 → 模式」裡），模組關不到它，所以切換失敗、顯示「無法更改勿擾模式」，手機一直停在 AI 模式。現在 App 不再開這個模式，舊版留下的也會先關掉；重新開機後改由模組自己打開勿擾，AI 模式一樣不會響。如果還有別的模式讓勿擾開著，App 會說是哪一個、去哪裡關，也不會自己一直重試。Android 14 以下照舊。更新手機模組就會生效。見[手機](docs/zh-TW/PHONE.md#切換沒完成時)。
- **切回普通手机不会再卡在勿扰（Android 15 及以上）。** 应用会自己开一个以 Bageo 命名的勿扰模式（在“设置 → 模式”里），模块关不掉它，所以切换失败、提示“无法更改勿扰模式”，手机一直停在 AI 模式。现在应用不再开这个模式，旧版留下的也会先关掉；重启后改由模块自己打开勿扰，AI 模式一样不会响。如果还有别的模式让勿扰开着，应用会说是哪一个、去哪里关，也不会自己一直重试。Android 14 及以下照旧。更新手机模块就会生效。见[手机](docs/zh-CN/PHONE.md#切换没完成时)。
- **Back up your settings automatically, and move to a new computer in a few clicks.** *Settings → Backup & restore* backs up every day, week (the default) or month to a folder you choose, with *Back up now*, *Export settings* and *Import settings* beside it. *Export settings* and *Import settings* let you tick what to take: only the settings, the settings with your keys, passwords and connections (where notifications go and who can control Bageo remotely), or also call records, texts and *To do* items. Importing a file without them never changes those on this computer, and the console warns you before a restore changes where notifications go. Before anything is restored, Bageo shows what is inside and how big it is, and first saves your current settings as a separate file. Backups never leave your computer, and automatic and manual ones hold your keys, passwords and connections, so keep them private; without a console password they only download on the computer that runs Bageo. You can also encrypt them with a password of your own (*Encrypt backup files*, or *Encrypt this file with a password* when exporting): the password stays on this computer for automatic backups, and an encrypted file opens only with it. A restore never shortens how long records are kept. See [Settings](docs/en/SETTINGS.md#backup--restore) and [API](docs/en/API.md#backup-and-restore).
- **設定可以自動備份，換電腦幾個步驟就搞定。**「設定 → 備份與還原」可以每天、每週（預設）或每月備份到你指定的資料夾，旁邊有「立即備份」「匯出設定」「匯入設定」。匯出和匯入都可以勾選要帶的項目：只要設定、設定加金鑰、密碼和連線（通知送去哪裡、誰可以遙控 Bageo），或再加通話紀錄、簡訊和待處理事項；匯入沒有這一項的檔案，不會改動這台電腦的金鑰和連線，還原會改變通知去向時也會先提醒。還原之前會先列出備份裡有什麼、檔案多大，並先把現在的設定另存一份。備份只存在你的電腦上，自動和手動備份裡有金鑰、密碼和連線，要自己收好；沒設控制台密碼時，只能在執行 Bageo 的這台電腦上下載。也可以用自己的密碼加密（「備份檔加密」，或匯出時勾「用密碼加密這個檔案」）：自動備份用的密碼存在這台電腦，加密的檔案只有這個密碼打得開。還原不會縮短保留天數。見[設定參考](docs/zh-TW/SETTINGS.md#備份與還原)與 [API](docs/zh-TW/API.md#備份與還原)。
- **设置可以自动备份，换电脑几步就搞定。**“设置 → 备份与还原”可以每天、每周（默认）或每月备份到你指定的文件夹，旁边有“立即备份”“导出设置”“导入设置”。导出和导入都可以勾选要带的项目：只要设置、设置加密钥、密码和连接（通知发到哪里、谁可以远程控制 Bageo），或再加通话记录、短信和待处理事项；导入没有这一项的文件，不会改动这台电脑的密钥和连接，还原会改变通知去向时也会先提醒。还原之前会先列出备份里有什么、文件多大，并先把现在的设置另存一份。备份只存在你的电脑上，自动和手动备份里有密钥、密码和连接，要自己收好；没设控制台密码时，只能在运行 Bageo 的这台电脑上下载。也可以用自己的密码加密（“备份文件加密”，或导出时勾选“用密码加密这个文件”）：自动备份用的密码存在这台电脑，加密的文件只有这个密码打得开。还原不会缩短保留天数。见[设置参考](docs/zh-CN/SETTINGS.md#备份与还原)与 [API](docs/zh-CN/API.md#备份与还原)。
- **Scam openers that sound like a follow-up are stalled again.** Generic phrases such as "regarding your request", "following up on your…", "returning your call", 「您申請的」 and 「調查結果」, and the same kind in Spanish, French, German, Portuguese, Italian, Indonesian, Malay, Tagalog, Turkish and Vietnamese, no longer make Bageo treat a call as a reply to your own matter on their own, so a loan or fake-police opener that starts with them is still stalled. A call about a complaint or repair you reported is recognised as before.
- **聽起來像「回覆你的事」的詐騙開場會照常陪聊。** 「regarding your request」「following up on your…」「returning your call」「您申請的」「調查結果」這類泛稱，以及西、法、德、葡、義、印尼、馬來、他加祿、土耳其、越南語的同類說法，單獨出現不再讓 Bageo 把整通電話當成回覆你自己的事，用它們開場的貸款或假檢警電話仍會陪聊。回覆你申訴或報修的電話照樣認得。
- **听起来像“回复你的事”的诈骗开场会照常陪聊。** “regarding your request”“following up on your…”“returning your call”“您申请的”“调查结果”这类泛称，以及西、法、德、葡、意、印尼、马来、他加禄、土耳其、越南语的同类说法，单独出现不再让 Bageo 把整通电话当成回复你自己的事，用它们开场的贷款或假检警电话仍会陪聊。回复你投诉或报修的电话照样能识别。
- **Italian callers are handled like the other languages, and the log keeps calls private.** Italian goodbyes, thanks, "please hold", voice menus, voicemail and busy messages are recognised, and "Ciao, sono Marco" is no longer taken for a goodbye. Replies to your own complaint or request are recognised in more Russian, Japanese, Korean, Hindi and Arabic phrasings, so they are not stalled as sales calls. A Japanese 机 (desk) in a transcript stays as written. On a Taiwan SIM, a call from a hidden number now waits for the carrier's international-call warning before the AI speaks. With `GEMINI_PERSONA_MODE` turned off, the older relay flow (written only in Traditional Chinese) is kept for Traditional Chinese; in any other language the AI assistant answers in your language. The log shows only how many characters were said on a call; set `LOG_CALL_TEXT=1` to log the words while troubleshooting. See [Settings](docs/en/SETTINGS.md).
- **義大利語來電跟其他語言一樣處理，log 也不再記通話內容。** 義大利語的道別、道謝、「請稍候」、語音選單、語音信箱、占線都認得，「Ciao, sono Marco」不再被當成道別。俄、日、韓、印地、阿拉伯語回覆你自己申訴或申請的說法認得更多，不會被當成推銷而陪聊。逐字稿裡日文的「机」（書桌）照原樣顯示。台灣 SIM 卡接到未顯示號碼時，AI 會先等電信的國際電話提示音播完再開口。把 `GEMINI_PERSONA_MODE` 關掉時，只有繁體中文會走舊的轉達流程（它只有繁體中文版）；其他語言改由 AI 助理用你的語言接聽。log 只記通話中講了幾個字；排查問題時設 `LOG_CALL_TEXT=1` 才會記下內容。見[設定](docs/zh-TW/SETTINGS.md)。
- **意大利语来电跟其他语言一样处理，日志也不再记录通话内容。** 意大利语的道别、道谢、“请稍候”、语音菜单、语音信箱、占线都能识别，“Ciao, sono Marco”不再被当成道别。俄、日、韩、印地、阿拉伯语回复你自己投诉或申请的说法识别得更多，不会被当成推销而陪聊。逐字稿里日文的“机”（书桌）照原样显示。台湾 SIM 卡接到未显示号码时，AI 会先等运营商的国际电话提示音播完再开口。把 `GEMINI_PERSONA_MODE` 关掉时，只有繁体中文会走旧的转达流程（它只有繁体中文版）；其他语言改由 AI 助理用你的语言接听。日志只记录通话中说了几个字；排查问题时设 `LOG_CALL_TEXT=1` 才会记下内容。见[设置](docs/zh-CN/SETTINGS.md)。
- **A number that keeps calling no longer floods you, and less is given away.** For each number, Bageo sends at most two *Incoming call*, *Blocked number called* or *Missed while on another call* notifications a minute, then one message saying how many more calls came. Urgent alerts, appointments, call summaries and SMS are never held back. With no console password set, `/health` gives its full details only to the computer Bageo runs on; other devices get the short status, and the console works as before. Logs keep only the last four digits of a caller's number. See [Notifications](docs/en/NOTIFICATIONS.md) and [API](docs/en/API.md).
- **同一個號碼狂打不再洗版，對外透露的資訊也變少。** 同一個號碼一分鐘內，「來電」「黑名單來電」「通話中插播未接」各最多通知兩次，之後用一則訊息告訴你又打了幾通；急事警報、預約、通話摘要和簡訊一定會通知。沒設控制台密碼時，`/health` 只有 Bageo 所在的這台電腦拿得到完整內容，其他裝置只拿到短版狀態，控制台照常使用。log 裡的來電號碼只留末四碼。見[通知](docs/zh-TW/NOTIFICATIONS.md)與 [API](docs/zh-TW/API.md)。
- **同一个号码不停打来不再刷屏，对外透露的信息也变少。** 同一个号码一分钟内，“来电”“黑名单来电”“通话中插播未接”各最多通知两次，之后用一条消息告诉你又打了几通；紧急警报、预约、通话摘要和短信一定会通知。没设控制台密码时，`/health` 只有 Bageo 所在的这台电脑拿得到完整内容，其他设备只拿到简版状态，控制台照常使用。日志里的来电号码只留末四位。见[通知](docs/zh-CN/NOTIFICATIONS.md)与 [API](docs/zh-CN/API.md)。
- **Fewer calls cut off by mistake, in more languages.** Bageo no longer takes everyday words for a goodbye: Portuguese *falou* ("he said"), English "I'll take care of it" and "That's all right", Chinese 「我明天要去拜拜」 (going to the temple), Tamil and Telugu "I'm coming", and phrases like them in French, Spanish, German and Polish. A caller on a local number who speaks another language is now written down in that language, so the summary stays right. Turkish callers are handled like the other languages: goodbyes, emergencies and voice menus. Words that the speech-to-text sends in pieces no longer run together.
- **更多語言不會被誤掛電話。** Bageo 不再把日常用語當成再見：葡萄牙語的 falou（他說）、英語的「I'll take care of it」「That's all right」、中文的「我明天要去拜拜」「你不要瞎掰」、泰米爾語和泰盧固語的「我過來」，以及法語、西班牙語、德語、波蘭語的類似說法。用本地號碼打來、講別種語言的人，逐字稿會照他講的語言寫，摘要才不會錯。土耳其語來電跟其他語言一樣處理：道別、急事、語音選單都認得。語音轉文字分段送來的字不再黏在一起。
- **更多语言不会被误挂电话。** Bageo 不再把日常用语当成再见：葡萄牙语的 falou（他说）、英语的“I'll take care of it”“That's all right”、中文的“我明天要去拜拜”“你不要瞎掰”、泰米尔语和泰卢固语的“我过来”，以及法语、西班牙语、德语、波兰语的类似说法。用本地号码打来、说别种语言的人，逐字稿会照他说的语言写，摘要才不会错。土耳其语来电跟其他语言一样处理：道别、急事、语音菜单都认得。语音转文字分段送来的字不再粘在一起。
- **Cleaner, neutral interview prompts for both answer styles, separate custom prompts, and full first-person mode.** *Settings → Prompt* now provides dedicated nine-language interview prompts for both *AI assistant* (assistant persona, how to refer to you, message-taking and hand-off rules) and *As me* (your personality, speaking habits, callers and boundaries) — with no unrelated regional examples — returning a single ready-to-paste prompt block without extra notes or setting reminders. *AI assistant* and *As me* also save their custom prompts separately so switching styles never mixes instructions. Answering *As me* and calling as **You in person** no longer add a built-in AI-disclosure line (only *AI assistant* and **Your assistant** do). See [Prompts](docs/en/PROMPTS.md).
- **「AI 助理」與「本人」各有專屬九語訪談提示詞、分開儲存自訂提示詞，並移除「本人」身分的承認 AI 規則。**「設定 → 提示詞」現在為「AI 助理」（助理形象、怎麼稱呼你、記留言重點與轉達界線）與「本人」（你的說話個性、語氣習慣、常見來電與應對界線）各自提供約 8 題中立九語訪談提示詞，不再出現無關地區舉例，最後只輸出一段可直接貼上的提示詞，不再附帶多餘的資料區塊與設定提醒。兩種接聽身分的自訂提示詞分開儲存，切換不會混用；選「本人」接聽及代打時不再加上內建的承認 AI 規則（僅「AI 助理」與「助理」身分保留）。見[提示詞](docs/zh-TW/PROMPTS.md)。
- **“AI 助理”与“本人”各有专属九语访谈提示词、分开保存自定义提示词，并移除“本人”身份的承认 AI 规则。**“设置 → 提示词”现在为“AI 助理”（助理形象、怎么称呼你、记留言重点与转达界线）与“本人”（你的说话个性、语气习惯、常见来电与应对界线）各自提供约 8 题中立九语访谈提示词，不再出现无关地区举例，最后只输出一段可直接粘贴的提示词，不再附带多余的资料区块与设置提醒。两种接听身份的自定义提示词分开保存，切换不会混用；选“本人”接听及代打时不再加上内置的承认 AI 规则（仅“AI 助理”与“助理”身份保留）。见[提示词](docs/zh-CN/PROMPTS.md)。
- **Plug the phone in once, then it works over Wi-Fi.** The first time a phone connects by USB, Bageo gives it its Wi-Fi address and a pairing key; unplugged, the phone connects over Wi-Fi by itself. The System page shows whether each phone is on USB or Wi-Fi, and *Settings → Advanced* can turn this off or pair the phones again (needs the matching phone app; entering the address by hand still works). See [Phone setup](docs/en/PHONE.md#wi-fi).
- **手機用 USB 接一次，之後就能走 Wi-Fi。** 手機第一次用 USB 連上時，Bageo 會把自己的 Wi-Fi 位址和配對碼交給手機；拔掉線後，手機會自己改用 Wi-Fi 連回來。「系統」頁會顯示每支手機是用 USB 還是 Wi-Fi，「設定 → 進階」可以關掉這個功能或重新配對手機（要搭配新版手機 App；手動填網址照樣能用）。見[手機設定](docs/zh-TW/PHONE.md#wi-fi)。
- **手机用 USB 连一次，之后就能走 Wi-Fi。** 手机第一次用 USB 连上时，Bageo 会把自己的 Wi-Fi 地址和配对码交给手机；拔掉线后，手机会自己改用 Wi-Fi 连回来。“系统”页会显示每台手机是用 USB 还是 Wi-Fi，“设置 → 高级”可以关掉这个功能或重新配对手机（要搭配新版手机应用；手动填写地址照样能用）。见[手机设置](docs/zh-CN/PHONE.md#wi-fi)。
- **Windows installer and macOS dmg: one click to start.** On Windows, run `bageo-call-<version>-windows-x64-setup.exe` (no administrator rights needed) and click the **Bageo** shortcut. On macOS, open the `.dmg` and drag Bageo into Applications. Bageo runs in the background with an icon in the system tray or menu bar, and your data stays when you update or remove the program. See [Getting started](docs/en/GETTING_STARTED.md).
- **Windows 安裝程式與 macOS dmg：點一下就啟動。** Windows 執行 `bageo-call-<版本>-windows-x64-setup.exe`（不需要系統管理員權限），再點「Bageo」捷徑；macOS 打開 `.dmg`，把 Bageo 拖進「應用程式」。Bageo 在背景執行，系統匣或選單列會有圖示；更新或移除程式時資料都會留著。見[開始使用](docs/zh-TW/GETTING_STARTED.md)。
- **Windows 安装程序与 macOS dmg：点一下就启动。** Windows 运行 `bageo-call-<版本>-windows-x64-setup.exe`（不需要管理员权限），再点“Bageo”快捷方式；macOS 打开 `.dmg`，把 Bageo 拖进“应用程序”。Bageo 在后台运行，系统托盘或菜单栏会有图标；更新或卸载程序时数据都会留着。见[开始使用](docs/zh-CN/GETTING_STARTED.md)。
- **Calls end more naturally in every language.** Bageo knows how people wrap up in the nine languages plus Cantonese, Thai, Vietnamese and Indonesian, and hangs up only after the caller has said goodbye (Cantonese is only listened for; the AI answers in Mandarin). A greeting that doubles as a goodbye (such as Korean 수고하십니다) no longer ends a call at the start, and short agreements like なるほど, Genau or Vale no longer interrupt the AI. Each region has its own wait before hanging up, unless you set the wait yourself. AI errands end as before. See [Prompts](docs/en/PROMPTS.md).
- **各國語言的通話收尾更自然。** 九種語言加上粵語、泰語、越南語、印尼語，Bageo 都認得「準備要掛了」的說法和道別詞，對方說完道別才掛斷（粵語只用來聽出對方要掛了，AI 照用普通話回）。問候兼道別的詞（例如韓文 수고하십니다）一開場不會再害通話被誤掛；なるほど、Genau、Vale 這類附和也不會打斷 AI。各地區有自己預設的掛斷等待時間，你自己設過就照你的。AI 代打照舊。見[提示詞](docs/zh-TW/PROMPTS.md)。
- **各国语言的通话收尾更自然。** 九种语言加上粤语、泰语、越南语、印尼语，Bageo 都认得“准备要挂了”的说法和道别词，对方说完道别才挂断（粤语只用来听出对方要挂了，AI 照用普通话回）。问候兼道别的词（例如韩文 수고하십니다）一开场不会再害通话被误挂；なるほど、Genau、Vale 这类附和也不会打断 AI。各地区有自己默认的挂断等待时间，你自己设过就按你的。AI 代打照旧。见[提示词](docs/zh-CN/PROMPTS.md)。
- **Shorter prompts by SIM region, with local habits.** Each call now carries only the everyday habits of the region of the SIM that took it, such as where couriers leave parcels, how times and numbers are repeated and which scams are common there. Replies do not get slower. The region only sets the language the AI opens in: a caller who speaks another language Gemini supports is answered in it. The AI speaks only languages Gemini supports, and its Chinese is Mandarin: Taiwanese, Hakka, Cantonese and other dialects Gemini does not speak are answered in Mandarin, and Hong Kong and Macau SIMs open in Mandarin. Appointment rules stay short until the caller brings up a time. See [Prompts](docs/en/PROMPTS.md).
- **提示詞依 SIM 卡地區精簡，加上當地生活習慣。** 每通電話只帶接聽那張卡所在地區的生活習慣，例如快遞放哪、時間數字怎麼複誦、當地常見詐騙，回話不會變慢。地區只決定開場先講哪種語言，對方講別種 Gemini 支援的語言就跟著換。AI 只講 Gemini 支援的語言，中文一律講普通話：台語、客語、粵語這類 Gemini 不支援的方言用普通話自然回，香港、澳門卡也用普通話接。預約說明平常只放精簡版，對方提到約時間才補完整版。見[提示詞](docs/zh-TW/PROMPTS.md)。
- **提示词按 SIM 卡地区精简，加上当地生活习惯。** 每通电话只带接听那张卡所在地区的生活习惯，例如快递放哪、时间数字怎么复述、当地常见诈骗，回话不会变慢。地区只决定开场先说哪种语言，对方说别的 Gemini 支持的语言就跟着换。AI 只说 Gemini 支持的语言，中文一律说普通话：闽南语、客家话、粤语这类 Gemini 不支持的方言用普通话自然回，香港、澳门卡也用普通话接。预约说明平常只放精简版，对方提到约时间才补完整版。见[提示词](docs/zh-CN/PROMPTS.md)。
- **Ordinary calls get the right approach for what they are about.** Bageo tells a courier, a real bank checking a charge, an important notice, a request to set a time, family or friend chat and a wrong number apart, and adds one short hint in the pause after the AI finishes a sentence, so nothing slows down. For a bank, the AI repeats the amount, shop and time, neither confirms nor denies the charge, agrees to no card block and ends with "I'll call the official line myself later". A wrong number never hears the owner's name. See [Prompts](docs/en/PROMPTS.md).
- **一般來電依情境應對。** Bageo 會分辨快遞、真銀行核對、正事通知、約時間、親友閒聊、打錯電話，並在 AI 講完一句的空檔補一句提示，不多花時間。銀行核對時，AI 會複誦金額、商店和時間，不承認也不否認，不答應停卡，最後說「晚點我自己打官方客服確認」；打錯電話不會報出主人的姓名。見[提示詞](docs/zh-TW/PROMPTS.md)。
- **一般来电按情境应对。** Bageo 会分辨快递、真银行核对、正事通知、约时间、亲友闲聊、打错电话，并在 AI 说完一句的空隙补一句提示，不多花时间。银行核对时，AI 会复述金额、商店和时间，不承认也不否认，不答应停卡，最后说“晚点我自己打官方客服确认”；打错电话不会报出主人的姓名。见[提示词](docs/zh-CN/PROMPTS.md)。
- **Sales and scam calls get a stall plan for their kind.** Investment, insurance, loan, fake official, bank or store, debt collection (never admits a debt) and harassment calls each have their own script: sound keen at first, run into one problem after another, and when the caller tries to hang up, say "wait, one more question". The scam word list now covers Taiwan "cancel the installment" ATM calls, Japan ATM tax refunds, Korean prosecutor "safe account" calls, US IRS and Medicare gift cards, and European customs fee links and TAN codes. See [Prompts](docs/en/PROMPTS.md).
- **推銷與詐騙電話依類型陪聊。** 投資、保險、信貸、冒充官方、銀行或賣場、催收（不認債）、騷擾各有自己的拖延劇本：先裝有興趣，再一步步卡關，對方要掛時拋出「欸先別掛，我還有一個問題」。詐騙詞庫新增台灣 ATM 解除分期、日本 ATM 退稅、韓國檢察廳安全帳戶、美國 IRS 與 Medicare 禮品卡、歐洲海關補稅連結與 TAN 碼。見[提示詞](docs/zh-TW/PROMPTS.md)。
- **推销与诈骗电话按类型陪聊。** 投资、保险、信贷、冒充官方、银行或商场、催收（不认债）、骚扰各有自己的拖延剧本：先装有兴趣，再一步步卡关，对方要挂时抛出“欸先别挂，我还有一个问题”。诈骗词库新增台湾 ATM 解除分期、日本 ATM 退税、韩国检察厅安全账户、美国 IRS 与 Medicare 礼品卡、欧洲海关补税链接与 TAN 码。见[提示词](docs/zh-CN/PROMPTS.md)。
- **AI errands ask what is missing before they dial.** When you start an errand, Bageo sees within a second what is missing (for a table: how many people, which phone number to leave, the booking name), asks at most 3 questions, and lets you "Add and call" or "Call anyway". It also notes the order you would give way in (if 7:30 is full, 8:30) and opens with the polite phrases of the place you call. Confirmation cards in your chat list the same questions: reply to answer, or reply "cancel" and nothing is dialed. Scripts can use `POST /calls/api/errands/preflight`. See [API](docs/en/API.md).
- **AI 代打撥號前先補問。** 新增代打時，Bageo 會在 1 秒內看出還缺什麼（例如訂位的人數、留哪支電話、訂位名字），最多問 3 題，你可以「補好撥出」或「直接撥出」。它也會整理你願意退讓的順序（例如 7 點半不行就 8 點半），並用當地的商務開場禮貌語。聊天室的確認卡會列出同樣的問題，直接回覆就算補答，回「取消」就不撥。程式可用 `POST /calls/api/errands/preflight`。見 [API](docs/zh-TW/API.md)。
- **AI 代打拨号前先补问。** 新增代打时，Bageo 会在 1 秒内看出还缺什么（例如订位的人数、留哪个电话、订位名字），最多问 3 题，你可以“补好拨出”或“直接拨出”。它也会整理你愿意退让的顺序（例如 7 点半不行就 8 点半），并用当地的商务开场礼貌语。聊天室的确认卡会列出同样的问题，直接回复就算补答，回“取消”就不拨。程序可用 `POST /calls/api/errands/preflight`。见 [API](docs/zh-CN/API.md)。

# Bageo v1.4.0

- **SIM changes show up right away.** When you put in, take out or swap a SIM card, the phone tells Bageo within a few seconds, so the System page and AI errands show the new card. The app can now read the card's own number when the carrier provides it (needs the new phone module).
- **換 SIM 卡後馬上更新。** 插卡、拔卡或換卡後，手機幾秒內就會告訴 Bageo，「系統」頁和「AI 代打」會顯示新的卡；電信公司有提供門號時，App 也能直接讀到這張卡的號碼（要更新手機模組）。
- **换 SIM 卡后马上更新。** 插卡、拔卡或换卡后，手机几秒内就会告诉 Bageo，“系统”页和“AI 代打”会显示新的卡；运营商有提供号码时，App 也能直接读到这张卡的号码（要更新手机模块）。
- **Refined low-glare console, SIM line numbers, recording download, call deletion and offline phone removal.** The console now uses a calm low-contrast look with clean text controls instead of decorative icons. Each phone's SIM numbers are shown on the System page (and can be entered manually when a SIM card does not expose its number), incoming calls show which line was called, and AI errands let you pick which number to call from. You can also download any call's WAV recording, delete individual calls or all finished call history, and remove offline phones. See [API](docs/en/API.md).
- **柔和低對比控制台介面、顯示與設定手機門號、錄音下載、刪除通話紀錄與移除離線手機。** 控制台改成柔和耐看的低對比配色並拿掉裝飾圖示；「系統」頁可查看每支手機各張 SIM 的門號（讀不到時可直接手動輸入），來電會顯示打給哪支門號，「AI 代打」也能直接選要用哪台手機的哪個號碼撥出；另可下載通話錄音 WAV、單筆或全部刪除通話紀錄、刪除離線舊手機。見 [API](docs/zh-TW/API.md)。
- **柔和低对比度控制台界面、显示与设置手机号码、录音下载、删除通话记录与移除离线手机。** 控制台改成柔和耐看的低对比度配色并去掉装饰图标；“系统”页可查看每部手机各张 SIM 的号码（读不到时可直接手动输入），来电会显示打给哪个号码，“AI 代打”也能直接选要用哪部手机的哪个号码拨出；另可下载通话录音 WAV、单条或全部删除通话记录、删除离线旧手机。见 [API](docs/zh-CN/API.md)。
- **OpenCC Chinese conversion and cleaner license files.** Chinese script conversion now uses pure-Rust OpenCC (`ver.1.4.2`, `Apache-2.0`) dictionaries instead of `zhconv`. [`LICENSE`](LICENSE) is now the verbatim GNU AGPL v3.0 text, dual-licensing and AGPL Section 7 additional terms live in [`LICENSING.md`](LICENSING.md), [`LEGAL.md`](LEGAL.md) is rewritten as a concise global disclaimer and usage notice, and [`NOTICE.md`](NOTICE.md) and [`LICENSES/Apache-2.0.txt`](LICENSES/Apache-2.0.txt) list OpenCC and the Gradle wrapper.
- **簡繁轉換改用 OpenCC，並整理授權與免責文件。** 簡繁轉換改由純 Rust 讀取內建的 OpenCC（`ver.1.4.2`，`Apache-2.0`）字典，移除 `zhconv` 依賴。[`LICENSE`](LICENSE) 改放未刪改的 GNU AGPL v3.0 正式全文，雙重授權與第 7 條附加條款獨立至 [`LICENSING.md`](LICENSING.md)，[`LEGAL.md`](LEGAL.md) 重寫為精簡的免責聲明與使用須知，並在 [`NOTICE.md`](NOTICE.md) 與 [`LICENSES/Apache-2.0.txt`](LICENSES/Apache-2.0.txt) 補齊 OpenCC 與 Gradle wrapper 授權聲明。
- **简繁转换改用 OpenCC，并整理许可证与免责文档。** 简繁转换改由纯 Rust 读取内置的 OpenCC（`ver.1.4.2`，`Apache-2.0`）字典，移除 `zhconv` 依赖。[`LICENSE`](LICENSE) 改放未删改的 GNU AGPL v3.0 正式全文，双重授权与第 7 条附加条款独立至 [`LICENSING.md`](LICENSING.md)，[`LEGAL.md`](LEGAL.md) 重写为精简的免责声明与使用须知，并在 [`NOTICE.md`](NOTICE.md) 与 [`LICENSES/Apache-2.0.txt`](LICENSES/Apache-2.0.txt) 补齐 OpenCC 与 Gradle wrapper 授权声明。

# Bageo v1.3.0

- **Chat commands now work in Discord, Slack and Matrix, as well as Telegram.** Check calls, have the AI call someone and decide pending items from any of the four. Slack and Matrix take commands as "@your bot /help". These three are only tested with simulated servers so far. See [Chat commands](docs/en/CHAT_COMMANDS.md).
- **聊天室指令支援 Discord、Slack 和 Matrix，Telegram 也照舊可用。** 四種聊天軟體都能查通話、叫 AI 代打電話、處理待處理事項。Slack 和 Matrix 要寫成「@你的機器人 /help」。這三個目前只用模擬伺服器測試過。見[在聊天室操作](docs/zh-TW/CHAT_COMMANDS.md)。
- **聊天室指令支持 Discord、Slack 和 Matrix，Telegram 也照常可用。** 四种聊天软件都能查通话、让 AI 代打电话、处理待处理事项。Slack 和 Matrix 要写成“@你的机器人 /help”。这三个目前只用模拟服务器测试过。见[在聊天室操作](docs/zh-CN/CHAT_COMMANDS.md)。
- **Whitelisted callers with an emergency can bring you into the call (trial, off by default).** When a whitelisted caller says it is urgent, the AI asks them to stay on the line, calls your own phone and joins the two calls so you can talk. If you do not answer, the AI says you will call back. It understands urgency and the "please hold on" line in nine languages. It needs "Only when it is urgent" and your own number, and has been tested with simulated calls so far. See [Settings](docs/en/SETTINGS.md).
- **白名單來電有急事，可以把你接進通話（試驗功能，預設關閉）。** 白名單的人說有急事時，AI 請他別掛，撥給你自己的手機，接通後把兩通電話合在一起，你就能直接跟他說話。你沒接的話，AI 會告訴對方你會回電。九種語言都聽得懂「急事」和「請別掛」。要先選「有急事才轉」並填好你的號碼；目前只用模擬通話測試過。見[設定](docs/zh-TW/SETTINGS.md)。
- **白名单来电有急事，可以把你接进通话（试验功能，默认关闭）。** 白名单的人说有急事时，AI 请他别挂，拨给你自己的手机，接通后把两通电话合在一起，你就能直接跟他说话。你没接的话，AI 会告诉对方你会回电。九种语言都听得懂“急事”和“请别挂”。要先选“有急事才转”并填好你的号码；目前只用模拟通话测试过。见[设置](docs/zh-CN/SETTINGS.md)。
- **The AI no longer says it will put the call through.** For an urgent whitelisted caller it used to say things like "I'll transfer you", which it cannot do. It now says it is on it and you will call back.
- **AI 不再隨口說「幫你轉接」。** 白名單的人有急事時，AI 以前會說「我幫你轉接」這類做不到的話；現在只說馬上處理、會回電給他。
- **AI 不再随口说“帮你转接”。** 白名单的人有急事时，AI 以前会说“我帮你转接”这类做不到的话；现在只说马上处理、会回电给他。
- **Safer links and access.** One-tap links in notifications (accept, decline, block) now open a confirmation page and act only when you confirm; they expire after 7 days and are signed with a random key instead of your password. The console sends its password in a header, not in the web address. Chat commands, the MCP server and group chats are locked down further, and CSV exports and contact notes can no longer carry spreadsheet formulas or hidden instructions to the AI.
- **連結與存取更安全。** 通知裡的一鍵連結（同意、婉拒、加入黑名單）改成先開確認頁，按下確認才會執行；連結 7 天後失效，並用隨機金鑰簽章，不再用你的密碼。控制台的密碼改放在標頭，不再出現在網址裡。聊天室指令、MCP 伺服器和群組聊天室的限制更嚴；CSV 匯出和聯絡人備註也不能再夾帶試算表公式或偷偷指示 AI 的內容。
- **链接与访问更安全。** 通知里的一键链接（同意、婉拒、加入黑名单）改成先打开确认页，点击确认才会执行；链接 7 天后失效，并用随机密钥签名，不再用你的密码。控制台的密码改放在标头，不再出现在网址里。聊天室指令、MCP 服务器和群组聊天室的限制更严；CSV 导出和联系人备注也不能再夹带电子表格公式或偷偷指示 AI 的内容。
- **Old notification links stop working.** The one-tap links and calendar links in notifications sent before this version no longer work. Use the console, or wait for the next notification.
- **舊通知裡的連結會失效。** 這一版以前發出的通知，裡面的一鍵連結和行事曆連結都不能用了。請改在控制台操作，或等下一則通知。
- **旧通知里的链接会失效。** 这一版以前发出的通知，里面的一键链接和日历链接都不能用了。请改在控制台操作，或等下一条通知。
- **Numbers from any country work.** Numbers without a leading zero (China, Spain, Portugal, Singapore, Hong Kong, Russia and more) now work for call-backs, texts and AI errands. The default country comes from the SIM, then the language, and the blacklist reads a number the way the answering SIM dials it.
- **各國的號碼都能用。** 沒有開頭 0 的號碼（中國、西班牙、葡萄牙、新加坡、香港、俄羅斯等）現在也能回電、發簡訊、用在 AI 代打。預設國別先看 SIM、再看語言；黑名單照接聽那張 SIM 的撥號方式讀號碼。
- **各国的号码都能用。** 没有开头 0 的号码（中国、西班牙、葡萄牙、新加坡、香港、俄罗斯等）现在也能回电、发短信、用在 AI 代打。默认国别先看 SIM、再看语言；黑名单按接听那张 SIM 的拨号方式读号码。
- **Fewer mistakes in other languages.** Japanese and Simplified Chinese lines are no longer turned into Traditional Chinese, and a Japanese "sorehadewa" no longer ends a call mid-sentence. Thai, Russian, Arabic and Hindi calls hang up after the goodbye, and call-backs use the caller's language. Scam keywords match whole words and ignore accents, and "Reply in the caller's language" is back in the console, on by default. Contact lists saved as Japanese Shift_JIS files import correctly.
- **其他語言的錯誤變少。** 日文和簡體中文不會再被轉成繁體；日文的「それでは」不再把通話講到一半就掛掉。泰文、俄文、阿拉伯文、印地文的電話在道別後會掛斷，回電也用對方的語言。詐騙關鍵字改成整個字比對、忽略重音；「用對方的語言回答」回到控制台，預設開啟。日文 Shift_JIS 存的通訊錄也能正確匯入。
- **其他语言的错误变少。** 日文和简体中文不会再被转成繁体；日文的“それでは”不再把通话讲到一半就挂掉。泰文、俄文、阿拉伯文、印地文的电话在道别后会挂断，回电也用对方的语言。诈骗关键词改成整个词比对、忽略重音；“用对方的语言回答”回到控制台，默认开启。日文 Shift_JIS 存的通讯录也能正确导入。
- **Chat rooms and notifications.** Slack slash commands, Slack and Discord notifications with only a bot token, Discord private messages, Matrix auto-join with a warning for encrypted rooms, and room aliases. Group chats need a list of people allowed to give commands; private chats do not. Long notifications are cut to fit services that count bytes, and ntfy topic links work. See [Chat commands](docs/en/CHAT_COMMANDS.md).
- **聊天室與通知。** 新增 Slack 斜線指令、只填 Bot 權杖也能發 Slack 和 Discord 通知、Discord 私訊、Matrix 自動進房（加密房會提醒）和房間別名。群組聊天室要先填可以下指令的人，私訊不用。太長的通知會依服務的計算方式裁短，ntfy 主題連結也能用。見[在聊天室操作](docs/zh-TW/CHAT_COMMANDS.md)。
- **聊天室与通知。** 新增 Slack 斜杠指令、只填 Bot 令牌也能发 Slack 和 Discord 通知、Discord 私信、Matrix 自动进房（加密房会提醒）和房间别名。群组聊天室要先填可以下指令的人，私聊不用。太长的通知会按服务的计算方式裁短，ntfy 主题链接也能用。见[在聊天室操作](docs/zh-CN/CHAT_COMMANDS.md)。
- **Phone and text forwarding fixes.** The phone module asks for call permission at boot, reports its SIM list, ends merged calls cleanly, refuses call waiting during a merge and does not dial when the chosen SIM is missing. An urgent call that ends early no longer locks the phone for hours. The text forwarder handles messages without a SIM and says why when its password is rejected. Update the phone module and the forwarder to get these.
- **手機與簡訊轉發修正。** 手機模組開機就取得撥號權限、回報 SIM 清單、正確結束合併的通話、合併時拒絕插播，找不到指定的 SIM 就不撥。急事通話提早結束，不再把手機鎖上好幾個小時。簡訊轉發能處理沒有 SIM 資訊的簡訊，密碼被拒絕時會說明原因。要更新手機模組和簡訊轉發才會生效。
- **手机与短信转发修正。** 手机模块开机就获得拨号权限、报告 SIM 列表、正确结束合并的通话、合并时拒绝插播，找不到指定的 SIM 就不拨。急事通话提前结束，不再把手机锁上好几个小时。短信转发能处理没有 SIM 信息的短信，密码被拒绝时会说明原因。要更新手机模块和短信转发才会生效。
- **Contributor agreement and legal terms.** New [CLA.md](CLA.md) contributor license agreement. [LEGAL.md](LEGAL.md) now carries the privacy terms and the ban on illegal use, with the same eight articles in English, 繁體中文 and 简体中文; the Traditional Chinese text governs.
- **新增貢獻者授權協議與法律條款。** 新增貢獻者授權協議 [CLA.md](CLA.md)。[LEGAL.md](LEGAL.md) 加入隱私條款和禁止違法使用的規定，英文、繁體中文、簡體中文的八條內容一致，以繁體中文為準。
- **新增贡献者授权协议与法律条款。** 新增贡献者授权协议 [CLA.md](CLA.md)。[LEGAL.md](LEGAL.md) 加入隐私条款和禁止违法使用的规定，英文、繁体中文、简体中文的八条内容一致，以繁体中文为准。

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

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
