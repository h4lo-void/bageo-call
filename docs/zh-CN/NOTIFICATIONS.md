# 通知：通话摘要推送到 Telegram、企业微信、钉钉、飞书、邮件等 32 种渠道

AI 电话助理帮你接完电话，挂断几秒后，手机就收到一段摘要：谁打来、要做什么、AI 怎么回复、你要处理什么。Bageo 也能在来电时、拦截号码或插播被挡下时、AI 回拨或 AI 代打结束时通知你。支持 32 种服务，从 Telegram、LINE、Teams、Signal 到企业微信、钉钉、飞书、Server酱、PushPlus、WxPusher、Home Assistant、邮件，以及你自己的 Webhook。

**一分钟配好：**“设置 → 通知”→“**添加渠道**”→ 选服务 → 填字段 →“**发送测试**”→“**保存更改**”。

- 渠道想加多少都行，**每条消息都会发到所有已启用的渠道**。每个渠道可以起个“名称”（例如“家人群”），用“启用”临时关掉而不必删除。
- 地址和令牌保存在你电脑上的 `data/calls/config/settings.json`。网页控制台之后不会再完整显示，只给提示；编辑时字段留空，就保留原来的值。
- 最上面的“发送通知”关掉后，通话相关消息和 Telegram 预约卡片都会停，但渠道不会被删除。脚本通过 API 发的消息（见[下方](#发送你自己的消息)），例如转发的短信，仍会照发。
- 消息用“通知和摘要的语言”撰写（默认“与 AI 说话的语言相同”），跟通话用什么语言无关。
- 太长的消息会按各服务的上限截短（例如 Pushover 1,000 字符，Discord 和企业微信 1,900 字符）。

> **测试程度：** Telegram 是用得最多的渠道。其他服务按各自的公开文档实现，请求内容有单元测试覆盖，但在真实账号上用得还不多。哪一个失败了，“发送测试”会显示该服务的返回内容，欢迎反馈。
> 1.0 之后新增的服务（Microsoft Teams、Google Chat、Rocket.Chat、Zulip、Webex、Nextcloud Talk、Synology Chat、Signal、Pushbullet、PushPlus、PushDeer、WxPusher、Chanify、SMTP 邮件、Home Assistant、IFTTT）都还没有在真实账号上试过。

## 消息长什么样

一条来自联系人的通话摘要：

```text
☎️ 通话摘要 · 小杰
号码：0912345678
手机：Pixel 8
时间：11月12日 18:02

对方：小杰（朋友）
事情：明天中午要不要一起吃饭？
AI 回复：说听起来不错，没有约定时间
要处理：回电给小杰确认
📅 日历：和小杰吃午饭 (11/13 12:00)
https://calendar.google.com/calendar/render?action=TEMPLATE&…
通话记录／录音：http://192.0.2.10:8787/calls/#call=…
```

- **标题**：来电者在“联系人”里时，后面会加名字。
- **日历那几行**：通话中约定了日期、而且“建议日历日程”开着时才会出现（[设置参考](SETTINGS.md#摘要与日历)）。链接会打开你第一次按“添加到日历”时选的那一家（`calendar_app`）；还没选时不带日历链接，打开最后一行的通话页按“添加到日历”选一家。详见下方[日程预约](#日程预约)。
- **最后一行**：主程序有其他设备能打开的地址时才会出现：设置里的“这个控制台的地址”，或控制台对局域网开放时自动带入的地址（Docker 里不会自动带入）。见[从其他设备使用控制台](SETTINGS.md#从其他设备使用控制台)。

## 你会收到哪些消息

每条消息开头都有图标，一眼就能分清：

| 图标与标题 | 什么时候 | Webhook `event` |
|---|---|---|
| 📞 来电 | 电话被接起来。 | `incoming` |
| ⛔ 拒接号码来电 | 拦截列表里的号码打来。会写明是响铃时直接拒接，还是接起来播放语音，以及你当初填的原因。 | `blocked` |
| ☎️ 通话摘要 | 通话结束几秒后：谁、什么事、AI 怎么回复、你要处理什么，以及链接。 | `summary` |
| 📵 通话中插播未接 | 通话中又有第二通打进来，手机把它拒接了（对方听到忙音）。知道号码的话记得回电。 | `call_waiting` |
| 📞 AI 回拨：〈号码〉 | AI 打给来电者告知预约结果：结果如何（接通并告知、没人接、拨不出去，或这个号码不能回拨）和那通电话的摘要。 | `ai_callback` |
| 📞 AI 代打：〈名字或号码〉 | 你交办的 AI 代打结束了：结果、细节、需要你决定的选项、下一步、谈定的时间，以及“代打”页的链接。 | `ai_errand` |
| 📅 预约待你确认 | 对方跟 AI 约了时间；Telegram 会收到互动按钮卡片，其他所有通知渠道也会收到带一键“同意／不同意”签名按钮或链接的通知（见[日程预约](#日程预约)）。 | `appointment_pending` / `summary` |
| ⭐ 白名单来电直接转接 | 白名单号码打来、且设置为“直接转接真实号码”时，手机自动挂断以触发运营商呼叫转移并即时推送通知。 | `whitelist_direct_transfer` |
| 🚨 白名单紧急事件通知 | 白名单号码打来、且 AI 在通话中听出对方有紧急事件时，AI 主动告知你的真实转接号码并发送紧急警报。 | `whitelist_urgent_transfer` |
| ✅ 测试通知 | 你按了“发送测试”。 | `test` |
| 新短信 | 可选的[短信转发工具](../../tools/sms_forward/README.md)（英文）转来的短信。 | `event` |

插播未接的消息：

```text
📵 通话中插播未接：0912345678（Pixel 9）
时间：11月14日 18:02
```

AI 代打的结果，例如 AI 帮你订好的餐厅：

```text
📞 AI 代打：海底捞
号码：02123456789
手机：Pixel 9
时间：11月14日 18:05

办好了：已订明天 7 点半 4 位，名字王先生
细节：座位保留 15 分钟。
📅 约定时间：11月15日 19:30 海底捞晚饭
http://bageo.local:8787/calls/#errands
```

AI 在你来不及回答的问题上先答应了的话，结果会是“要你决定”，并多一行请你确认，不行要打回去改。

## 日程预约

开启“日程预约”（[设置参考](SETTINGS.md#摘要与日历)）后，对方跟 AI 约好时间的那通电话，摘要下面会多一行（取代日历那几行），每个渠道都能收到：

```text
📅 预约待你确认：11月14日（周六）15:00–16:00 看房 — 到网页确认：http://bageo.local/calls/#call=…
```

**在网页上决定：**“通话”页最上方的“**待处理事项**”，或那通电话的详情，选择怎么告诉对方（**发短信**、**AI 回拨**或**不通知对方**），再按 **同意** 或 **不同意**。链接会打开那通电话；控制台没有其他设备能打开的地址（“这个控制台的地址”）时，这一行改为“到网页控制台确认”。完全没设置通知渠道时不会推送，预约照样在网页上等你。主程序不会自己往任何日历里写东西。

**在 Telegram 上一键决定。** 用 `TELEGRAM_BOT_TOKEN` 和 `TELEGRAM_CHAT_ID` 指定了 Telegram Bot（见[下方](#改用环境变量配置)）、“发送通知”也开着时，等你决定的预约另外会有一张卡片。只在网页控制台添加的 Telegram 渠道，能收到上面那行提醒，但收不到卡片。

```text
📅 预约待确认

号码：0912345678
时间：11月14日（周六）15:00–16:00
事由：看房
称呼：陈先生
手机：Pixel 8
短信：按同意或不同意后会发短信告诉对方
网页确认：http://bageo.local/calls/#call=…
[同意] [不同意]
```

“短信”这一行按这个号码的“默认回复方式”写，也可能是“回复方式：按同意或不同意后，AI 会打电话告诉对方”（固话，或没开短信）、“不会发（设置里“发短信告诉对方”没开）”、“对方号码收不到短信，请自己回电”，或“未显示号码，没办法通知对方”。在卡片上点按，用的是“默认回复方式”；想换方式，请到网页上决定。

点按之后，Bot 会回复结果：

```text
✅ 已同意预约
11月14日（周六）15:00–16:00 看房
已发短信给 0912345678
📅 添加到日历：https://calendar.google.com/calendar/render?action=TEMPLATE&…
```

其他可能的回复：“❌ 已婉拒预约”、“AI 正在回拨给 0912345678”、“短信没发出：手机没连线”／“手机 App 太旧，不支持发短信”／“失败代码 no_service”。短信没发出时，网页上那条预约会出现“**重发短信**”，可以用原来那部或另一部在线的手机再发。

**“添加到日历”哪家都能用。** Bot 回复、通知和网页里的链接，会打开你第一次按“添加到日历”时选的日历（设置 `calendar_app`）：Google 日历、Outlook.com、Microsoft 365 或 Yahoo 日历已填好的“新建日程”表单；选 Apple 日历或 `.ics` 时，则是这个日程在“这个控制台的地址”下的 `.ics` 文件。还没选时，Bot 回复带的是这通电话在网页的页面（没有控制台地址就不带），按“添加到日历”选一家，之后就记住。这个 `.ics` 链接带的是只能打开这一个文件的密钥（HMAC），不是你的控制台令牌，从聊天里点开也安全；没填控制台地址就没有 `.ics` 链接。主程序不连接任何日历账号。

**只有第一次决定才算**，不管在哪里决定（网页、卡片或 [API](API.md#日程预约与任务)）；再决定一次（即使是同时按）会回复“这个预约已经处理过了”。在网页或 API 决定后，卡片上的按钮会被去掉，Bot 也会在聊天里回复同样的结果；你决定之后才送达的卡片没有按钮。Telegram 连不上也不影响决定。

告诉对方结果的 AI 回拨，说的是那通电话的语言：等对方接起来出声，说明是谁打来、说一次你的回复，然后道别。它的通知是“📞 AI 回拨：〈号码〉”；拨不出去也会通知你。

推销、诈骗电话不会留下预约。通话后才被判定为这类电话（摘要标为“推销电话”“疑似诈骗”；银行反诈专线、警方提醒诈骗这类电话不算）时，预约会作废、不发卡片，摘要会写：

```text
📅 判定是推销电话，已取消通话中排入的预约：11月14日（周六）15:00–16:00 看房
```

## 各项服务

挑一个你每天都会看的 App：

| 服务 | 需要准备 | 常见用途 |
|---|---|---|
| [Telegram](#telegram) | Bot 令牌、聊天 ID | 通用；预约卡片只有它有 |
| [Discord](#discord) | Webhook 地址 | Discord 服务器 |
| [Slack / Mattermost](#slack--mattermost) | Incoming Webhook 地址 | 工作群 |
| [Microsoft Teams](#microsoft-teams) | Workflow Webhook 地址 | 用 Microsoft 365 的公司 |
| [Google Chat](#google-chat) | Webhook 地址 | Google Workspace |
| [Rocket.Chat](#rocketchat) | Webhook 地址 | 自建团队聊天 |
| [Zulip](#zulip) | 服务器地址、Bot 邮箱和 API 密钥、频道 | 开源团队聊天 |
| [Webex](#webex) | Bot 访问令牌、房间 ID 或邮箱 | Cisco Webex |
| [Nextcloud Talk](#nextcloud-talk) | 服务器地址、用户和应用密码、会话令牌 | 自建 Nextcloud |
| [Synology Chat](#synology-chat) | Webhook 地址 | 群晖 NAS 用户 |
| [LINE](#line) | Channel access token、用户或群组 ID | 台湾、日本、泰国 |
| [Signal](#signal) | 自己部署的 signal-cli-rest-api、发送号码、收件人 | Signal 用户 |
| [ntfy](#ntfy) | 主题 | 自建、Android 推送 |
| [Gotify](#gotify) | 服务器地址、App 令牌 | 自建 |
| [Bark (iOS)](#bark-ios) | 设备密钥 | iPhone 推送 |
| [Pushover](#pushover) | API 令牌、用户密钥 | iOS 与 Android 推送 |
| [Pushbullet](#pushbullet) | 访问令牌 | Android 与电脑推送 |
| [企业微信群机器人（WeCom）](#企业微信群机器人wecom) | Webhook 地址 | 企业微信 |
| [钉钉机器人（DingTalk）](#钉钉机器人dingtalk) | Webhook 地址、加签密钥 | 钉钉 |
| [飞书／Lark 机器人](#飞书lark-机器人) | Webhook 地址 | 飞书、Lark |
| [Server酱（ServerChan）](#server酱serverchan) | SendKey | 推送到微信 |
| [PushPlus](#pushplus) | Token | 推送到微信（推送加） |
| [PushDeer](#pushdeer) | PushKey | iOS 与 Android 推送，可自建 |
| [WxPusher](#wxpusher) | SPT，或 appToken 加 UID | 推送到微信 |
| [Chanify](#chanify) | Token | iOS 与 Android 推送，可自建 |
| [Matrix](#matrix) | Homeserver 地址、访问令牌、房间 ID | Element 等 Matrix App |
| [电子邮件（Gmail）](#电子邮件gmail) | Gmail 地址、应用专用密码 | 邮件 |
| [电子邮件（SMTP）](#电子邮件smtp) | SMTP 地址、密码 | 其他任何邮箱服务 |
| [Home Assistant](#home-assistant) | 服务器地址、访问令牌、通知服务 | 智能家居、Companion App |
| [IFTTT Webhooks](#ifttt-webhooks) | Webhooks 密钥、事件名称 | IFTTT Applet |
| [Apprise API](#apprise-api) | 通知地址 | 再接上百种服务 |
| [Webhook (JSON)](#webhook-json) | 地址 | n8n、Zapier、Make、Home Assistant、你自己的程序 |

### Telegram

1. 在 Telegram 里跟 **@BotFather** 对话：`/newbot`，起个名字 → 拿到一个像 `123456789:AA…` 的 **bot token**。
2. 打开跟新机器人的聊天窗口，按 **Start**（机器人不能主动先给你发消息）。
3. 找出你的 **chat ID**：跟 **@userinfobot** 对话，它会回复你的数字 ID。要发到群组，就把机器人拉进群，改用群组 ID（负数，例如 `-1001234567890`）。

| 字段 | 填什么 |
|---|---|
| Bot 令牌 | @BotFather 给的 token |
| 聊天 ID（Chat ID） | 你的数字 ID，或群组 ID |

想收到带按钮的预约卡片，请把同一个 Bot 也用 `TELEGRAM_BOT_TOKEN` 和 `TELEGRAM_CHAT_ID` 指定；同一条消息不会收到两次。

想在这个聊天室查通话记录、让 AI 代打或处理待办，见[在聊天室操作](CHAT_COMMANDS.md)（要在聊天室各自打开才会生效）。

### Discord

频道设置 →“整合 → Webhook → 新 Webhook → 复制 Webhook 网址”。

| 字段 | 填什么 |
|---|---|
| Webhook 地址 | `https://discord.com/api/webhooks/…` |

### Slack / Mattermost

Slack：到 <https://api.slack.com/apps> 创建 App →“Incoming Webhooks”→ 启用 →“Add New Webhook to Workspace”→ 复制地址。Mattermost：“Integrations → Incoming Webhooks”。

| 字段 | 填什么 |
|---|---|
| Incoming Webhook 地址 | `https://hooks.slack.com/services/…` |

### Microsoft Teams

旧的 Office 365 连接器（`webhook.office.com` 开头的地址）已经停用。这里用的是 **Workflows（工作流）** 的 Webhook，发出的是 Adaptive Card。

1. 在 Teams 打开频道的“**…**”菜单 →“工作流”。
2. 选“收到 Webhook 请求时发布到频道”，选好团队和频道，完成设置。
3. 复制它显示的地址。地址里带有签名，请像密码一样保管。

| 字段 | 填什么 |
|---|---|
| Workflow Webhook 地址 | 第 3 步的地址 |

消息的每一行在卡片上各占一行，第一行是粗体。

### Google Chat

需要 Google Workspace 账号，个人 Google 账号不能添加 Webhook。打开聊天室 → 点聊天室名称 →“应用和集成 → Webhook → 添加 Webhook”→ 复制地址。

| 字段 | 填什么 |
|---|---|
| Webhook 地址 | `https://chat.googleapis.com/v1/spaces/…/messages?key=…&token=…` |

### Rocket.Chat

“管理 → 工作区 → 集成 → 新建 → 传入（Incoming）”：启用、选频道和发帖用的用户，保存后复制“Webhook URL”。

| 字段 | 填什么 |
|---|---|
| Webhook 地址 | `https://chat.example.org/hooks/…/…` |

### Zulip

在“个人设置 → Bot → 新建 Bot”创建一个 Bot（类型选“Incoming webhook”就够了），并让它订阅要发的频道。Bot 的邮箱和 API 密钥在 Bot 卡片上。

| 字段 | 填什么 |
|---|---|
| 服务器地址 | 你的组织，例如 `https://example.zulipchat.com` |
| Bot 邮箱:API 密钥 | 两者用冒号连起来：`calls-bot@example.zulipchat.com:abc123…` |
| 频道 > 话题，或邮箱 | `calls > phone`；只填频道（`calls`）会发到话题“Bageo”；填一个以上的邮箱（用逗号分隔）则发私信 |

消息超过 2,400 字符会截短，以免超过 Zulip 10,000 字节的上限。

### Webex

到 <https://developer.webex.com/my-apps> 创建 Bot，复制它的 **Bot 访问令牌**（只显示一次），再把 Bot 加进聊天室。房间 ID 可以登录 developer.webex.com，用 Bot 的令牌运行“List Rooms”查到；要发给某个人，就改填对方的邮箱。

| 字段 | 填什么 |
|---|---|
| Bot 访问令牌 | Bot 的 token |
| 房间 ID 或邮箱 | `Y2lzY29zcGFyazovL3Vz…` 或 `someone@example.com` |

消息超过 1,800 字符会截短（Webex 最多接收 7,439 字节）。

### Nextcloud Talk

以一个 Nextcloud 用户的身份发消息，单独建一个 Bageo 专用用户最清楚。在“个人设置 → 安全 → 设备与会话”为它创建**应用密码**，再把这个用户加进会话。会话令牌是会话链接的最后一段（`https://cloud.example.org/call/abc123xy` → `abc123xy`）。

| 字段 | 填什么 |
|---|---|
| 服务器地址 | `https://cloud.example.org` |
| 用户:应用密码 | `bageo:xxxxx-xxxxx-xxxxx-xxxxx-xxxxx` |
| 会话令牌（token） | `abc123xy` |

### Synology Chat

在 Synology Chat：点头像 →“集成 → 传入的 Webhook → 新建”，选频道、保存，然后复制 Webhook 地址。

| 字段 | 填什么 |
|---|---|
| Webhook 地址 | `https://nas.example.org:5001/webapi/entry.cgi?api=SYNO.Chat.External&method=incoming&version=2&token=…` |

主程序会校验 HTTPS 证书。NAS 还在用自签名证书的话，在自家网络里改用 5000 端口的 `http://` 地址。

### LINE

LINE Notify 已于 2025 年停止服务；这里用的是 **Messaging API**。

1. 到 <https://developers.line.biz/console/> 创建 provider 和“Messaging API”channel（会同时创建一个 LINE 官方账号）。
2. 在该 channel 的“Messaging API”页签签发 **channel access token（long-lived）**。
3. 用 LINE App 把这个官方账号加为好友（同一页签有二维码）。
4. 你的**用户 ID**（`U…`，33 个字符）在“Basic settings”页签的“Your user ID”。要发到群组，就把官方账号拉进群，改用群组 ID（`C…`），群组 ID 要从 webhook 事件里获取。

| 字段 | 填什么 |
|---|---|
| Channel access token | 那个 long-lived token |
| 用户或群组 ID | `U…` 或 `C…` |

官方账号的免费套餐，每月可发的推送条数有限。

### Signal

Signal 没有 Bot API，所以要通过你自己部署的 [signal-cli-rest-api](https://github.com/bbernhard/signal-cli-rest-api)（一个 Docker 容器）。把它关联成你自己 Signal 账号的第二台设备，或用另一个号码注册。

| 字段 | 填什么 |
|---|---|
| API 服务器地址 | 容器的地址，例如 `http://signal-api:8080` |
| 发送号码 | signal-cli 用的号码，国际格式（`+15550100`） |
| 收件人 | 号码（`+15550101`）或群组 ID（`group.…`），用逗号分隔 |

消息超过 2,000 字符会截短。

### ntfy

安装 ntfy App，订阅一个**不容易被猜到**的主题名称：在公共服务器上，知道名称的人都能读到消息。

| 字段 | 填什么 |
|---|---|
| 服务器 | 留空就是 `https://ntfy.sh`，或填你自己的服务器 |
| 主题（Topic） | 主题名称 |
| 访问令牌（可选） | 只有自建服务器上受保护的主题才需要（`tk_…`） |

### Gotify

在 Gotify：“Apps → Create application”→ 复制它的 token。

| 字段 | 填什么 |
|---|---|
| 服务器地址 | 你的 Gotify 服务器，例如 `https://gotify.example.org` |
| App 令牌 | 该应用的 token |

### Bark (iOS)

打开 iPhone 上的 Bark App，示例地址是这样的：`https://api.day.app/<device key>/…`。

| 字段 | 填什么 |
|---|---|
| 服务器 | 留空就是 `https://api.day.app`，或填你自己的 bark-server |
| 设备密钥 | App 里的那串 key |

### Pushover

到 <https://pushover.net/apps/build> 创建一个应用。

| 字段 | 填什么 |
|---|---|
| API 令牌 | 该应用的 token |
| 用户密钥（User key） | Pushover 首页上你的 user key |

### Pushbullet

pushbullet.com →“Settings → Account → Create Access Token”。免费账号每月可推送的条数有限。

| 字段 | 填什么 |
|---|---|
| 访问令牌 | `o.…` |
| 设备 ID 或邮箱（可选） | 留空＝你所有的设备；填设备 ID（`iden`）只推到那台；填邮箱则推给另一位 Pushbullet 用户 |

### 企业微信群机器人（WeCom）

在群里“添加群机器人”→ 复制 webhook 地址。

| 字段 | 填什么 |
|---|---|
| Webhook 地址 | `https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=…` |

### 钉钉机器人（DingTalk）

群设置 →“机器人”→“自定义”→ 安全设置选“**加签**”，把 webhook 和密钥（`SEC…`）都复制下来。如果改用关键词作为安全设置，每条消息都必须包含其中一个关键词；加签更简单。

| 字段 | 填什么 |
|---|---|
| Webhook 地址 | `https://oapi.dingtalk.com/robot/send?access_token=…` |
| 加签密钥（可选） | `SEC…` 那个值；机器人没设就留空 |

### 飞书／Lark 机器人

群 →“设置 → 群机器人 → 添加机器人 → 自定义机器人”→ 复制 webhook 地址（`open.feishu.cn` 或 `open.larksuite.com`）。不要开签名校验，不支持；自定义关键词或 IP 白名单都可以。

| 字段 | 填什么 |
|---|---|
| Webhook 地址 | `https://open.feishu.cn/open-apis/bot/v2/hook/…` |

### Server酱（ServerChan）

从 Server酱网站复制你的 SendKey。

| 字段 | 填什么 |
|---|---|
| SendKey | Turbo 版是 `SCT…`，Server酱³ 是 `sctp…` |

### PushPlus

用微信登录 <https://www.pushplus.plus>，复制你的 **token**。消息会从 PushPlus 的微信公众号送达。

| 字段 | 填什么 |
|---|---|
| Token | 你的 token |
| 群组编码（可选） | 填群组编码就发给该群组；留空只发给自己 |

### PushDeer

安装 PushDeer 并登录，在“Key”里新增一个 **Key** 并复制。

| 字段 | 填什么 |
|---|---|
| 服务器 | 留空就是 `https://api2.pushdeer.com`，或填你自己的 PushDeer 服务器 |
| PushKey | `PDU…` |

### WxPusher

有两种方式，都在 <https://wxpusher.zjiekou.com>：

- **极简推送：**用微信扫码关注，获取你的 SPT（`SPT_…`）。收件留空。
- **应用：**创建应用获取 **appToken**（`AT_…`）。扫过它二维码的人会有用户 ID（`UID_…`）；主题则是数字 ID。

| 字段 | 填什么 |
|---|---|
| appToken（AT_…）或 SPT | `SPT_…` 或 `AT_…` |
| UID 或主题 ID | 只有 appToken 才要填：`UID_…` 和主题数字，用逗号分隔 |

微信上的摘要只显示标题前 20 个字。

### Chanify

在 Chanify 应用里为一个发送者创建 **token** 并复制。

| 字段 | 填什么 |
|---|---|
| 服务器 | 留空就是 `https://api.chanify.net`，或填你自己的 Chanify 节点 |
| Token | 应用给的 token |

### Matrix

创建一个机器人账号，邀请它进房间并让它加入。房间 ID 在“Room settings → Advanced”（`!abc…:example.org`）。房间必须是未加密的。

| 字段 | 填什么 |
|---|---|
| Homeserver 地址 | `https://matrix.example.org` |
| 访问令牌 | 机器人账号的 access token |
| 房间 ID | `!…:example.org` |

### 电子邮件（Gmail）

通过 Gmail 的 SMTP 服务器，用**应用专用密码**发送（Google 账号 →“安全性 → 两步验证 → 应用专用密码”；要先开启两步验证）。发件人名称是“Bageo”。

| 字段 | 填什么 |
|---|---|
| Gmail 地址 | 发件账号 |
| 应用专用密码 | 16 个字符的应用专用密码 |
| 发送到（留空＝发给自己） | 收件人 |

### 电子邮件（SMTP）

任何支持 TLS 的邮件服务器都行：QQ 邮箱、163、iCloud、Yahoo、Fastmail、公司的或你自己搭的。服务器和登录账号写在同一个字段：

```text
smtp://you@example.com@smtp.example.com:587     STARTTLS（大多数邮箱服务）
smtps://you@example.com@smtp.example.com:465    一连接就走 TLS
```

最后一个 `@` 前面是登录账号，同时也是发件地址；账号本身的 `@` 也可以写成 `%40`。465 端口一律是一连接就走 TLS。没写端口就用 587（`smtps://` 则是 465）。发件人名称是“Bageo”。

| 字段 | 填什么 |
|---|---|
| SMTP 地址（smtp://账号@主机:端口） | 如上 |
| 密码 | 账号密码，或邮箱服务要求的应用专用密码／授权码（QQ 邮箱、163、iCloud、Yahoo、Fastmail 都要） |
| 发送到（留空＝发给登录账号） | 收件人 |

限制：登录账号必须是邮箱地址；服务器要接受 `AUTH LOGIN`；证书必须来自公开的证书机构（自签名证书会被拒绝）；不支持没有 TLS 的连接。发 Gmail 的话，[Gmail](#电子邮件gmail) 渠道要填的字段更少。

### Home Assistant

通过 Home Assistant 的 **notify** 动作发送，例如发到手机上的 Companion App。

1. 在 Home Assistant：个人资料 →“安全”→“长期访问令牌”→“创建令牌”。
2. 在“开发者工具 → 动作”找服务：搜索 `notify.`（Companion App 会新增 `notify.mobile_app_<手机名称>`）。

| 字段 | 填什么 |
|---|---|
| 服务器地址 | 留空就是 `http://homeassistant.local:8123`，或填你的地址 |
| 长期访问令牌 | 第 1 步的令牌 |
| 通知服务 | `mobile_app_pixel_9`，前面带不带 `notify.` 都可以 |

消息超过 1,000 字符会截短，因为手机推送只能带大约 4 KB。

### IFTTT Webhooks

1. 到 <https://ifttt.com/maker_webhooks> 连接 Webhooks；“Documentation”会显示你的**密钥**。
2. 创建 Applet：*If* Webhooks →“Receive a web request”，事件名称例如 `bageo_call`，*Then* 选你想做的事。`{{Value1}}` 是标题、`{{Value2}}` 是内容、`{{Value3}}` 是事件（`summary`、`incoming`…）。

| 字段 | 填什么 |
|---|---|
| Webhooks 密钥 | 第 1 步的密钥 |
| 事件名称 | `bageo_call` |

使用 Webhooks 的 Applet 可能需要 IFTTT 付费套餐。

### Apprise API

如果你已经在运行 [Apprise API](https://github.com/caronc/apprise-api)，它还能接上百种服务：

| 字段 | 填什么 |
|---|---|
| 通知地址（…/notify/KEY） | `http://apprise.local:8000/notify/<your config key>` |
| 标签（可选） | 要发送到的标签 |

### Webhook (JSON)

给 n8n、Zapier、Make、Home Assistant、Node-RED 或你自己的程序用。主程序会用 `POST` 把 JSON 发到你的地址；填了“Bearer 令牌（可选）”时，会加上 `Authorization: Bearer <token>`。下面是语言设为 English 时的内容：

```json
{
  "event": "summary",
  "title": "Call summary 0912345678",
  "body": "☎️ Call summary\nNumber: 0912345678\n…",
  "text": "(same as body)",
  "data": {
    "caller_id": "0912345678",
    "device": "Pixel 8",
    "time": "Nov 12, 18:02",
    "summary": "Caller: Jay (friend)\nAbout: …",
    "contact": ""
  }
}
```

| `event` | 什么时候 | `data` |
|---|---|---|
| `incoming` | 电话被接起来 | `caller_id`、`device`、`time` |
| `blocked` | 拦截号码打来 | `caller_id`、`device`、`time`、`reason`、`declined`（`true`：响铃时拒接；`false`：接起来播放语音） |
| `summary` | 通话结束几秒后 | `caller_id`、`device`、`time`、`summary`、`contact`（联系人名字，不是联系人就是空字符串） |
| `call_waiting` | 通话中第二通打进来被拒接 | `caller_id`、`device`、`time`、`sim_country` |
| `ai_callback` | 日程预约的 AI 回拨结束，或拨不出去 | `caller_id`、`device`、`time`、`summary` |
| `ai_errand` | AI 代打结束，或没能开始 | `number`、`device`、`time`、`summary` |
| `appointment_pending` | 对方跟 AI 约了时间，等待你决定 | `task_id`、`caller_id`、`device`、`title`、`start_at`、`accept_url`、`decline_url` |
| `whitelist_direct_transfer` | 白名单来电直接挂断触发运营商呼叫转移 | `caller_id`、`device`、`time`、`forward_to`、`contact` |
| `whitelist_urgent_transfer` | 白名单来电中 AI 侦测到紧急事件并告知真实转接号码 | `caller_id`、`device`、`time`、`forward_to`、`reason`、`contact` |
| `voicemail` | 保存了一条语音留言（旧的菜单模式） | `caller_id`、`device`、`time`、`seconds` |
| `event` | 通过 API 发送的消息（`/calls/api/notify/send`） | `null` |
| `test` | “发送测试” | `{}` |

收到 `2xx` 响应就算送达。通话的其他内容（文字记录、录音），请用通话 ID 从 [API](API.md) 获取。

## 发送你自己的消息

你网络里的脚本也能用同一组渠道，例如车库门传感器：

```text
POST /calls/api/notify/send        {"title": "Garage", "body": "Door left open"}
```

跟其他 API 一样要带控制台令牌（见 [API](API.md)）。`body` 必填；`title` 不填就是“Bageo”。至少一个渠道收下时返回 `{"ok": true}`。“发送通知”关着时，这类消息照样会发。可选的[短信转发工具](../../tools/sms_forward/README.md)（英文）就是用这个方式转发手机收到的短信。

## 改用环境变量配置

给容器和脚本部署用：有三条通知路径可以在启动时指定，不必进网页控制台。它们会和控制台的渠道一起使用：

| 环境变量 | 路径 |
|---|---|
| `TELEGRAM_BOT_TOKEN` + `TELEGRAM_CHAT_ID` | Telegram Bot；预约卡片也由它发出。控制台里有同一个 Bot 和聊天时，不会重复发。 |
| `NOTIFY_WEBHOOK_URL` | 上面的 [JSON Webhook](#webhook-json)，不带令牌 |
| `NOTIFY_GATEWAY_URL` + `NOTIFY_GATEWAY_TAG` | Apprise API 的基础地址；消息发到 `<URL>/notify/<tag>`，标签留空时是 `normal` |
