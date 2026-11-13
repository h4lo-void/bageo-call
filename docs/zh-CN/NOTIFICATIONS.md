# 通知

有来电时、拦截号码打来时，主程序会发一条消息；通话结束几秒后，再发摘要。配置位置在“设置 → 通知”：“**添加渠道**”、选择服务、填好它的字段、“**发送测试**”、“**保存更改**”。

- 渠道想加多少就加多少；**每条消息都会发到所有已启用的渠道**。
- 地址和 token 保存在你电脑上的 `data/calls/config/settings.json` 里。网页控制台之后不会再完整显示它们，只给出提示；编辑时把字段留空，就会保留原来保存的值。
- 最上面的“发送通知”关掉之后，所有通知都会停止，但渠道不会被删除。
- 消息会用“设置 → 语言”里选的语言来写。

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
Transcript / recording: http://192.168.1.20:8787/calls/#call=…
```

上面是语言设为 English 的安装环境发出的内容；实际的措辞会跟随控制台里设置的语言。

最后一行在主程序有控制台地址时才会出现：设置里的“这个控制台的地址”，或控制台对局域网开放时自动带入的地址（Docker 里不会自动带入）。日历那几行只有在通话中约定了日期时才会出现（[设置参考](SETTINGS.md#摘要与日历)）。

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
| `blocked` | 拦截号码打来 | `caller_id`、`device`、`time`、`reason` |
| `summary` | 通话结束几秒后 | `caller_id`、`device`、`time`、`summary` |
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
