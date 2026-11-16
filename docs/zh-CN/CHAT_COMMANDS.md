# 在聊天室操作：用 Telegram 远程控制 Bageo

人不在控制台前面时，可以在接收通知的 Telegram 聊天室里操作 Bageo：

- 查通话记录。
- 让 AI 代打电话。
- 处理待处理事项。
- 回答 AI 通话中问你的问题。
- 把手机切到 AI 接听或普通手机。

目前支持 Telegram。之后可以接其他聊天软件和 AI 代理（见[给开发者](#给开发者)）。

## 怎么开启

聊天室指令默认**关闭**，要在每个聊天室各自打开。

1. 先配置好 Telegram 通知渠道（[通知 → Telegram](NOTIFICATIONS.md#telegram)）。
2. *设置 → 通知* → 那个 Telegram 渠道 → 勾选**允许在这个聊天室下指令**。
3. 可选：在**可以下指令的人**填 Telegram 用户 ID（问 **@userinfobot** 就知道自己的）。留空 = 这个聊天室里的人都可以；群组请填你自己的 ID。
4. 点**保存更改**，再给机器人发 `/help`。

用环境变量配置的机器人（`TELEGRAM_BOT_TOKEN` + `TELEGRAM_CHAT_ID`）：

- 在*设置 → 通知*的“在聊天室操作”里有自己的开关，或设 `TELEGRAM_CONTROL=true`。
- 要限定可以下指令的人，设 `TELEGRAM_CONTROL_USERS=123456789,987654321`。

在群组里，机器人只理会指令（`/calls` 或 `/calls@你的机器人`）和回复它自己消息的话，其他人聊天一律不理。

## 指令

`1`、`#2` 这种编号，指的是这个聊天室最近一次列出的清单（`/calls`、`/tasks`、`/errands`、`/status`）。

| 指令 | 做什么 |
|---|---|
| `/help` | 列出指令 |
| `/status` | Bageo 版本、每部手机的状态（AI 接听／普通手机／离线／通话中）、进行中的通话 |
| `/calls [条数] [today\|yesterday\|YYYY-MM-DD] [missed\|scam] [关键字或号码]` | 最近的通话，一通一行：时间、对方、摘要第一行。默认 5 条，最多 20 |
| `/call <#>` | 某一通：摘要、逐字稿开头（最多 600 字）、控制台链接 |
| `/tasks` | 等你处理的待处理事项（预约、留言） |
| `/confirm <#>`・`/ignore <#>` | 同意／忽略待处理事项，跟控制台上点一样 |
| `/errand <号码或联系人> <要办的事>` | 让 AI 帮你打电话，例如 `/errand 0912345678 订今晚七点两位`。名字有空格请加引号 |
| `/errands` | AI 代打的电话和结果 |
| `/cancel [#]` | 停止 AI 代打（不填编号 = 正在进行的那通） |
| `/say [#] <内容>` | 通话中交代 AI 一句话 |
| `/answer <内容>` | 回答 AI 问你的问题 |
| `/mode <手机> ai\|normal` | 切换手机模式（手机名称，或 `/status` 里的编号） |
| `/contacts <名字>` | 找联系人 |
| `/block <号码> [名称／原因]` | 把号码加入 **黑名单**（来电一律拒接） |
| `/stall <号码> [名称／备注]` | 把号码加入 **陪聊名单**（一接起就陪聊拖时间） |
| `/white <号码> [名称／备注]` | 把号码加入 **白名单**（重要亲友，可以直接转接，或有急事才转接） |
| `/unblock <号码>` | 从黑名单或陪聊名单移除，恢复正常接听 |

也可以直接打字，不用记指令，例如：

- “帮我打给 0912345678 订今晚七点两位”
- “昨天有谁打来”
- “把 0912345678 加进黑名单”
- “把 0223456789 加进陪聊名单”
- “把 0988111222 加进白名单”

这种普通文字会交给 Gemini 最便宜的模型（用*设置*里的密钥）转成上面的指令。没有密钥时，机器人只回一句提示，请你用 `/help`。

## 哪些事会先问你

这几件事会先回一条摘要，附 **执行／取消** 按钮（也可以回复 **1**／**2**），你点了才做：

- 拨电话。
- 切换手机模式。
- 同意或忽略待处理事项。
- 模型从你的普通文字里读出的“让 AI 在通话中说的话”或“回答 AI 的问题”。直接打 `/say` 或回复那条问题，则会立刻发出。

确认的规则：

- 只等 **2 分钟**，过了就作废。
- 只有下指令的人、在同一个聊天室才能确认，而且只能用一次。
- 新的请求会取代旧的。已经在控制台或卡片上处理过的待处理事项，不会被改掉。
- 拨电话前就先检查，跟控制台完全一样：紧急、短号、增值付费号码一律不拨，要有那个号码所属国家的 SIM 卡等等。

## AI 通话中问你的问题

AI 代打时拿不准、要你决定，问题会发到所有允许下指令的聊天室，AI 列出的选项会变成按钮。

- 40 秒内点选按钮，或回复那条消息。
- 控制台照样能答，先回答的算数。
- 现在可以从聊天室回答，所以就算没开控制台，AI 也会等你最多 40 秒。

AI 代打的结果照旧用普通通知发出。

## 安全

- 默认关闭，每个聊天室各自一个开关。
- 其他聊天室或其他人发来的消息**完全不回复**（只记在日志里），别人无从知道这个机器人存在。
- 每个聊天室有两个上限，都在*设置 → 通知*的“在聊天室操作”：
  - **每个聊天室每小时最多代打几通**：默认 5，最多 30，0 = 只能在控制台发起。
  - **每个聊天室每分钟最多几条消息**：默认 20，超过就安静到这一分钟结束。
- 代打的号码检查、SIM 卡规则、排队跟控制台发起的完全一样。
- 回复绝不包含密钥或密码：Bageo 持有的任何密钥都会在发出前被遮掉。逐字稿只给一小段。
- 只有你自己发的那句话会被当成指令。通话逐字稿、摘要、对方说的话都不会交给读你消息的模型，所以电话里说的话不会变成指令。
- 任何能在已开启指令的聊天室里发言的人都能用。请用你和机器人的私聊，或填上允许的用户 ID。
- **可以下指令的人**填的不是 ID（例如名字）也会保留，只是对不上任何人，名单不会因此变成“所有人”。
- 这份名单也适用于待处理事项卡片的按钮；同一个机器人和聊天室开了两次时，两边的名单都要符合。
- 指令菜单只设置在允许下指令的聊天室，陌生人打开机器人看不到。转发的消息不会被当成指令。

## 设置

| 设置 | 环境变量 | 默认 |
|---|---|---|
| 渠道：*允许在这个聊天室下指令*（`control`） | — | 关 |
| 渠道：*可以下指令的人*（`control_users`） | — | 聊天室里的人都可以 |
| `telegram_control` | `TELEGRAM_CONTROL` | 关 |
| `telegram_control_users` | `TELEGRAM_CONTROL_USERS` | 聊天室里的人都可以 |
| `control_errands_per_hour` | `CONTROL_ERRANDS_PER_HOUR` | 5（0–30） |
| `control_commands_per_minute` | `CONTROL_COMMANDS_PER_MINUTE` | 20（1–120） |

## 其他通知渠道的操作方式（一键按钮与通用指令 Webhook）

除了 Telegram 双向聊天外，**所有 32 种通知渠道**也都支持交互操作：

1. **通话摘要与预约通知的一键按钮（免输入密码、HMAC 签名保护）**
   - 在*设置 → 高级*填好“这个控制台的地址”（`public_url`）后，每通电话的摘要通知与预约确认通知都会附上单次签名的动作按钮或链接：
      - 通话摘要：**加入黑名单**、**设为陪聊**、**加入白名单**
      - 日程预约：**同意**、**不同意**
   - 各渠道的呈现方式：
      - **Telegram**：Inline Keyboard 按钮
      - **ntfy**：原生 Action 按钮（在手机通知栏直接点按）
      - **Home Assistant**：Companion App 通知 Action 按钮与点击跳转（`clickAction`）
      - **Microsoft Teams**：Adaptive Card `Action.OpenUrl` 按钮
      - **Bark / Pushover / Gotify / Chanify**：点击通知直接跳转链接（`url` / `click` / `link`）
      - **Discord / Slack / LINE / Matrix / 企业微信 / 钉钉 / 飞书 / Email**：消息内附带可点击的签名链接
      - **Webhook（JSON）**：结构化的 `actions: [{ "label": "...", "url": "..." }]` 数组
   - 这些按钮背后是 `GET/POST /calls/api/quick-action`。

2. **通用指令端点 `POST /calls/api/control/inbound`（让任何渠道都能下指令）**
   - Home Assistant、n8n、Make、Apple 快捷指令（Shortcuts）、Discord／Slack／LINE／企业微信／飞书自建机器人等，都可以直接 POST 文字指令或自然语言到：
      ```bash
      curl -X POST http://bageo.local:8787/calls/api/control/inbound \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer <控制台密码或 MCP token>" \
        -d '{"text": "/status", "channel_id": "ntfy", "reply": true}'
      ```
   - JSON 字段：
      - `text`：要下的指令。支持全部 18 个斜线指令（`/status`、`/calls`、`/errand`、`/block`、`/stall`、`/white`、`/unblock` 等）和 9 种语言的自然语言。
      - `channel_id`：要把回复推到哪个通知渠道。
      - `sender_id`：下指令的人。
      - `reply`：`true` 时，Bageo 会把执行结果推到那个通知渠道。
      - `token`：密码放在这里也可以。
   - `"reply": true` 只会推到“已启用、而且 id、名称或服务种类等于 `channel_id`”的那一个通知渠道；没给 `channel_id` 或对不上就不推。
   - 验证用控制台密码或 MCP 的 token，放在 `Authorization: Bearer`、`X-Calls-Token` 标头，或 JSON 的 `token` 字段。两个都没设置时不检查。
   - 返回的 JSON 有 `text`（回复内容）和 `pushed`（有没有推出去）。

## 给开发者

指令、检查、确认和上限都在 `crates/agent/src/control/`。要接新的聊天软件，只要写一层薄薄的转接：把消息转成 `control::Inbound` 交给 `Controller::handle`，或直接调用 `POST /calls/api/control/inbound`。

AI 代理（Claude、ChatGPT、Grok、OpenClaw（龙虾）…）可以通过内置的 MCP 服务器使用同一组动作：见 [MCP](MCP.md)。
