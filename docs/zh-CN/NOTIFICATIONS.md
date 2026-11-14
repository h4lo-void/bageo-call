# 通知

有来电时、拦截号码打来时、通话中拒接了插播的第二通时、AI 回拨或 AI 代打结束时，主程序都会发一条消息；通话结束几秒后，再发摘要。等你决定的日程预约随摘要一起发送，Telegram 另外会收到带 **同意**／**不同意** 按钮的卡片。配置位置在“设置 → 通知”：“**添加渠道**”、选择服务、填好它的字段、“**发送测试**”、“**保存更改**”。

- 渠道想加多少就加多少；**每条消息都会发到所有已启用的渠道**。
- 地址和 token 保存在你电脑上的 `data/calls/config/settings.json` 里。网页控制台之后不会再完整显示它们，只给出提示；编辑时把字段留空，就会保留原来保存的值。
- 最上面的“发送通知”关掉之后，所有通知都会停止，但渠道不会被删除。
- 消息用“通知语言”撰写（默认与 AI 说话的语言相同），与通话用的是什么语言无关。

> **现状：** Telegram 每天都在使用。其他服务是按它们的公开文档实现的，请求内容有单元测试覆盖，但还没有用真实账号试过。如果哪一个失败了，“发送测试”按钮会显示该服务的返回内容——请反馈给我们。

## 消息长什么样

```text
☎️ Call summary
Number: 0912345678
Phone: Pixel 8
Time: 11/12 18:02

Caller: Jay (friend)
About: lunch tomorrow at noon?
AI said: sounded keen, did not fix the time
To do: call Jay back to confirm
📅 Calendar: Lunch with Jay (11/13 12:00)
https://calendar.google.com/calendar/render?action=TEMPLATE&…
Transcript / recording: http://192.0.2.10:8787/calls/#call=…
```

上面是语言设为 English 的安装环境发出的内容；实际的措辞会跟随控制台里设置的语言。

最后一行在主程序有控制台地址时才会出现：设置里的“这个控制台的地址”，或控制台对局域网开放时自动带入的地址（Docker 里不会自动带入）。日历那几行只有在通话中约定了日期时才会出现（[设置参考](SETTINGS.md#摘要与日历)）。
日历链接会打开你在“添加到日历”选的那一家（`calendar_app`；还没选时是 Google 日历）：已经填好的“新建日程”表单；选 Apple 日历或 `.ics` 时则是控制台地址下这个日程的文件，用只能打开这一个文件的密钥打开。来电者在联系人里时，标题会写成“☎️ 通话摘要 · 小杰”。

## 你会收到哪些消息

每条消息都会发到所有已启用的渠道，用“通知语言”撰写。每一条开头都有图标，一眼就能分清：

| 图标与标题 | 什么时候 | Webhook `event` |
|---|---|---|
| 📞 来电 | 电话被接起来。 | `incoming` |
| ⛔ 拦截号码来电 | 拦截列表里的号码打来；消息会写明是响铃时直接拒接，还是接起来播放语音，以及你当初填的原因。 | `blocked` |
| ☎️ 通话摘要 | 通话结束几秒后：谁、什么事、AI 怎么回复、你要处理什么，以及链接。来电者在联系人里时，标题后面会加上名字。 | `summary` |
| 📵 通话中插播未接 | 通话中又有第二通打进来，手机把它拒接了（对方听到忙音）。知道号码的话记得回电。 | `call_waiting` |
| 📞 AI 回拨：〈号码〉 | AI 打给来电者告知预约结果：结果如何（接通并告知、没人接、拨不出去）和那通电话的摘要。 | `ai_callback` |
| 📞 AI 代打：〈名字或号码〉 | 你交办的 AI 代打结束了：结果、细节、需要你决定的选项、下一步、谈定的时间，以及“代打”页的链接。 | `ai_errand` |
| 📅 预约待你确认 | 对方跟 AI 约了时间；Telegram 另外会收到带按钮的卡片（见[日程预约](#日程预约)）。 | 包含在 `summary` 里 |
| 新短信 | 可选的[短信转发工具](../../tools/sms_forward/README.md)（英文）转来的短信。 | `event` |

插播未接的消息是这样的：

```text
📵 通话中插播未接：0912345678（Pixel 9）
时间：11月14日 18:02
```

AI 代打的结果：

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

你没回答的问题如果 AI 先答应了，结果会是“要你决定”，并多一行“AI 在你没回答的问题上先做了决定（…），请确认；不行要打回去改”。

## 日程预约

开启“日程预约”（[设置参考](SETTINGS.md#摘要与日历)）后，对方跟 AI 约好时间的那通电话，摘要下面会多一行（取代日历那几行），你设置的每个通知渠道（Telegram、Apprise、Webhook…）都能收到：

```text
📅 预约待你确认：11月14日（周六）15:00–16:00 看房 — 到网页确认：http://bageo.local/calls/#call=…
```

在网页上决定：“通话”页最上方的“**待处理事项**”，或该通电话的详情，选择怎么告诉对方（**发短信**、**AI 回拨**或**不通知对方**），再按 **同意**／**不同意**。链接会打开那通电话；控制台没有其他设备能打开的地址时（设置里的“这个控制台的地址”），这一行改为“到网页控制台确认”。完全没有设置通知渠道时不会推送，预约照样在网页上等你。主程序不会自己往任何日历里写东西。

设置了 Telegram Bot（bot token 和 chat ID）、“发送通知”也开着时，等你决定的预约另外会有一张可以直接点按的卡片：

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

“短信”这一行按这个号码的“默认回复方式”写，也可能是“回复方式：按同意或不同意后，AI 会打电话告诉对方”（固话，或没开短信）、“不会发（设置里“发短信告诉对方”没开）”、“对方号码收不到短信，请自己回电”，或“未显示号码，没办法通知对方”。在卡片上点按使用“默认回复方式”；想换别的方式，请到网页上决定。

点按之后 Bot 会回复结果，例如：

```text
✅ 已同意预约
11月14日（周六）15:00–16:00 看房
已发短信给 0912345678
📅 添加到日历：https://calendar.google.com/calendar/render?action=TEMPLATE&…
```

或“❌ 已婉拒预约”、“AI 正在回拨给 0912345678”、“短信没发出：手机没连线”／“手机 App 太旧，不支持发短信”／“失败代码 no_service”。短信没发出时，网页上那条预约会出现“**重发短信**”，可以用原来那部或另一部在线的手机再发。

**“添加到日历”哪家都能用。** Bot 回复、通知和网页里的链接，会指向你第一次按“添加到日历”时选的日历（设置 `calendar_app`）：Google 日历、Outlook.com、Microsoft 365 或 Yahoo 日历已填好的“新建日程”表单；选 Apple 日历或 `.ics` 时，则是这个日程在“这个控制台的地址”下的 `.ics` 文件。这个 `.ics` 链接带的是只能打开这一个文件的密钥（HMAC），不是你的控制台令牌，从聊天里点开也安全；没填控制台地址就没有 `.ics` 链接。主程序不连接任何日历账号。

无论在哪里决定（网页、卡片或 [API](API.md#日程预约与任务)），只有第一次才算；再按一次（即使是同时按）会回复“这个预约已经处理过了”。在网页或 API 决定后，卡片上的按钮会被去掉，Bot 也会在聊天里回复同样的结果，卡片不会再让你决定已经决定过的事（卡片送达前你就决定了的话，送达的卡片没有按钮）。Telegram 连不上也不影响决定的结果。

告诉对方结果的 AI 回拨说的是那通电话的语言，等对方接起来出声，才说明是谁打来、说一次你的回复，然后道别挂断。它的通知是“📞 AI 回拨：〈号码〉”；拨不出去时也会发到你的通知渠道。

推销、诈骗电话不会留下预约。通话后才被判定为这类电话时——摘要标为“推销电话”“疑似诈骗”（银行防诈骗、警察通知这类电话不算）——预约会作废、不发卡片，摘要会写：

```text
📅 判定是推销电话，已取消通话中排入的预约：11月14日（周六）15:00–16:00 看房
```


## 各项服务

每项服务：需要创建什么，以及填到哪个字段。

### Telegram

1. 在 Telegram 里和 **@BotFather** 对话：`/newbot`，起个名字 → 你会拿到一个 **bot token**，形如 `123456789:AA…`。
2. 打开和新机器人的聊天窗口，按 **Start**（机器人不能主动先给你发消息）。
3. 找到你的 **chat ID**：和 **@userinfobot** 对话，它会回复你的数字 ID。要发到群组的话，把你的机器人拉进群，改用群组的 ID（是负数，例如 `-1001234567890`）。

| 字段 | 填什么 |
|---|---|
| Bot token | @BotFather 给的 token |
| Chat ID | 你的数字 ID，或群组 ID |

### Discord

频道 →“编辑频道 → 整合 → Webhook → 新建 Webhook → 复制 Webhook URL”。

| 字段 | 填什么 |
|---|---|
| Webhook URL | `https://discord.com/api/webhooks/…` |

### Slack / Mattermost

Slack：到 <https://api.slack.com/apps> 创建一个应用 →“Incoming Webhooks”→ 启用 →“Add New Webhook to Workspace”→ 复制地址。Mattermost 和 Rocket.Chat 的 incoming webhook 接受同样的格式。

| 字段 | 填什么 |
|---|---|
| Incoming webhook URL | `https://hooks.slack.com/services/…` |

### LINE

LINE Notify 已在 2025 年停止服务；这里用的是 **Messaging API**。

1. 到 <https://developers.line.biz/console/> 创建一个 provider 和一个“Messaging API”channel（这会创建一个 LINE 官方账号）。
2. 在该 channel 的“Messaging API”标签页签发 **channel access token（long-lived）**。
3. 用你的 LINE 应用把这个官方账号加为好友（同一个标签页上有二维码）。
4. 你的**用户 ID**（`U…`，33 个字符）在该 channel 的“Basic settings”标签页，“Your user ID”下面。如果要改发到群组，把这个账号邀请进群，并使用群组 ID（`C…`）——群组 ID 要从 webhook 事件里获取。

| 字段 | 填什么 |
|---|---|
| Channel access token | 那个 long-lived token |
| User / group ID | `U…` 或 `C…` |

官方账号的免费套餐，每月可发送的推送消息数量有限。

### ntfy

安装 ntfy 应用，订阅一个**名字不容易被猜到**的主题（在公共服务器上，任何知道名字的人都能读到消息）。

| 字段 | 填什么 |
|---|---|
| Server | 留空就是 `https://ntfy.sh`，或者填你自己服务器的地址 |
| Topic | 主题名 |
| Access token | 只有自己服务器上受保护的主题才需要（`tk_…`） |

### Gotify

在 Gotify 里：“Apps → Create application”→ 复制它的 token。

| 字段 | 填什么 |
|---|---|
| Server URL | `https://gotify.example.org` |
| App token | 该应用的 token |

### Bark (iOS)

打开 Bark 应用；它显示的链接是 `https://api.day.app/<device key>/…`。

| 字段 | 填什么 |
|---|---|
| Server | 留空就是 `https://api.day.app`，或者填你自己的 bark-server |
| Device key | 应用里的那个 key |

### Pushover

到 <https://pushover.net/apps/build> 创建一个应用。

| 字段 | 填什么 |
|---|---|
| API token | 该应用的 token |
| User key | Pushover 首页上你的 user key |

### 企业微信群机器人（WeCom group bot）

群 →“添加群机器人”→ 复制 webhook 地址。

| 字段 | 填什么 |
|---|---|
| Webhook URL | `https://qyapi.weixin.qq.com/cgi-bin/webhook/send?key=…` |

### 钉钉群机器人（DingTalk bot）

群设置 →“机器人”→“自定义”→ 安全设置选“**加签**”，把 webhook 和密钥（`SEC…`）都复制下来。如果机器人改用关键词做安全设置，每条消息里都必须包含其中一个关键词——加签更省事。

| 字段 | 填什么 |
|---|---|
| Webhook URL | `https://oapi.dingtalk.com/robot/send?access_token=…` |
| Signing secret | `SEC…` 那个值（机器人没有设就留空） |

### 飞书群机器人（Feishu / Lark bot）

群 →“设置 → 群机器人 → 添加机器人 → 自定义机器人”→ 复制 webhook 地址（`open.feishu.cn` 或 `open.larksuite.com`）。不支持开启了签名校验的机器人；请改用自定义关键词或 IP 白名单，或者都不设。

| 字段 | 填什么 |
|---|---|
| Webhook URL | `https://open.feishu.cn/open-apis/bot/v2/hook/…` |

### Server酱（ServerChan）

| 字段 | 填什么 |
|---|---|
| SendKey | 你的 SendKey（Turbo 版是 `SCT…`，ServerChan³ 是 `sctp…`） |

### Matrix

创建一个机器人账号，邀请它进房间，并用它加入该房间。房间 ID 在“Room settings → Advanced”下面（`!abc…:example.org`）。房间必须是未加密的。

| 字段 | 填什么 |
|---|---|
| Homeserver URL | `https://matrix.example.org` |
| Access token | 机器人账号的 access token |
| Room ID | `!…:example.org` |

### 电子邮件（Gmail）

通过 Gmail 的 SMTP 服务器，用**应用专用密码**发送（Google 账号 →“安全性 → 两步验证 → 应用专用密码”；需要先开启两步验证）。

| 字段 | 填什么 |
|---|---|
| Gmail address | 发件的账号 |
| App password | 16 个字符的应用专用密码 |
| Send to | 收件人；留空 = 同一个邮箱 |

### Apprise API

如果你已经在运行 [Apprise API](https://github.com/caronc/apprise-api)（它还能对接上百种其他服务）：

| 字段 | 填什么 |
|---|---|
| Notify URL | `http://apprise.local:8000/notify/<your config key>` |
| Tag | 可选的标签 |

### Webhook (JSON)

给 n8n、Zapier、Make、Home Assistant、Node-RED 或你自己的代码用。主程序会把 JSON 用 `POST` 发到你的 URL；设置了 token 的话，会加上 `Authorization: Bearer <token>`。

```json
{
  "event": "summary",
  "title": "Call summary 0912345678",
  "body": "☎️ Call summary\nNumber: 0912345678\n…",
  "text": "(same as body)",
  "data": {
    "caller_id": "0912345678",
    "device": "Pixel 8",
    "time": "11/12 18:02",
    "summary": "Caller: Jay (friend)\nAbout: …"
  }
}
```

| `event` | 什么时候 | `data` |
|---|---|---|
| `incoming` | 电话被接起来 | `caller_id`、`device`、`time` |
| `blocked` | 拦截号码打来 | `caller_id`、`device`、`time`、`reason`、`declined`（`true`：响铃时拒接；`false`：接起来播放语音） |
| `summary` | 通话结束几秒后 | `caller_id`、`device`、`time`、`summary`、`contact`（联系人名字，不是联系人则为空字符串） |
| `call_waiting` | 通话中第二通打进来被拒接 | `caller_id`、`device`、`time`、`sim_country` |
| `ai_callback` | 日程预约的 AI 回拨结束，或拨不出去 | `caller_id`、`device`、`time`、`summary` |
| `ai_errand` | AI 代打结束，或没能开始 | `number`、`device`、`time`、`summary` |
| `voicemail` | 保存了一条语音留言（旧的菜单模式） | `caller_id`、`device`、`time`、`seconds` |
| `event` | 通过 API 发送的消息（`/calls/api/notify/send`） | — |
| `test` | “发送测试” | — |

收到 `2xx` 响应就算送达。通话的其他所有内容（文字记录、录音），请用通话 ID 从 [API](API.md) 获取。

## 发送你自己的消息

你网络里的脚本也可以使用同样这些渠道：

```text
POST /calls/api/notify/send        {"title": "Garage", "body": "Door left open"}
```

见 [API](API.md)。可选的[短信转发工具](../../tools/sms_forward/README.md)（英文）就是用这种方式转发手机收到的短信。

## 改用环境变量配置

针对容器和脚本化部署：有三条通知路径可以在启动时指定，不必到网页控制台里配置：`TELEGRAM_BOT_TOKEN` + `TELEGRAM_CHAT_ID`、`NOTIFY_WEBHOOK_URL`（就是上面的 JSON webhook，不带 token），以及 `NOTIFY_GATEWAY_URL` + `NOTIFY_GATEWAY_TAG`（Apprise API 的基础地址）。它们会在网页控制台的渠道之外额外使用。
