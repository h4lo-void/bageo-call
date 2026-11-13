# API

网页控制台只有一个页面，背后使用一组小型的 JSON API。你也可以在脚本里使用同一组 API。基础地址：主程序监听的位置，默认是 `http://127.0.0.1:8787`。

## 访问

没有设置 `CALLS_UI_TOKEN` 时，任何能访问这个端口的人都可以使用 API（默认只有这台电脑自己）。设置了之后，每个请求都要用下面任意一种方式带上令牌：

```text
GET /calls/api/list?token=<token>
Authorization: Bearer <token>
X-Calls-Token: <token>
```

令牌错误或缺失，会得到 `401`。

## 通话

| 方法和路径 | 内容 |
|---|---|
| `GET /calls/api/list` | 通话记录，最新的在前。查询参数：`limit`、`offset`、`q`（对号码、手机、事件、文字记录做文本搜索）、`caller`、`serial`、`status`、`has_transcript`。响应：`{ok, calls: [...], total, limit, offset}`。 |
| `GET /calls/api/call/{id}` | 单个通话：`{ok, call, notes, calendar_link}`。`notes`（摘要写好之前是 null）包含 `summary`、`dialogue`、`end_hint`、`source`、`updated_at`，通话中约定了日期时还有 `calendar` = `{title, start, end, all_day}`（本地时间，`YYYY-MM-DDTHH:MM`）。 |
| `GET /calls/api/call/{id}/event.ics` | 日历建议，iCalendar 文件格式。该通话没有建议时返回 `404`。 |
| `GET /calls/api/audio/{id}/listen` | 你当时会听到的整通电话（对方和 AI 混在一起），WAV，支持 range 请求。 |
| `GET /calls/api/audio/{id}/remote` · `/tts` · `/audio/{id}` | 只有对方 · 只有 AI · 原始的混音录音。 |
| `GET /calls/api/live` | 进行中的通话和已连接的手机：`{ok, count, calls, control_idle_clients, control_idle_serials}`。 |
| `GET /calls/api/stats` | `{ok, stats: {total, today, voicemails, blocked}}`。 |
| `GET /calls/api/export.csv` | 筛选后的列表，CSV 格式（查询参数同 `list`，最多 2000 行）。 |

通话记录（`call`，以及 `calls` 里的每一条）包含的字段有：`id`、`serial`（手机）、`caller_id`、`started_at` / `ended_at`（RFC 3339，UTC）、`duration_ms`、`status`（`ringing`、`active`、`ended`）、`events`（按顺序发生的事——例如 `active`、`persona_stall_spam`、`reject_list`、`persona_ai_fail_hangup`、`ended`）、`summary`，以及有哪些音频文件。

在 `notes.dialogue` 里，每一行是一次发言，前面带有保存时使用的说话人标签 `對方：`（来电者）或 `接聽：`（AI）。无论语言是什么，这两个标签都是固定的；显示时请自行翻译。

## 设置

| 方法和路径 | 内容 |
|---|---|
| `GET /calls/api/settings` | `{ok, settings, storage_path}`。机密字段会以掩码形式返回（`"abc…wxyz"`），并带一个 `<name>_set` 标志；通知渠道返回时 `url` / `token` 是空的，另外附带 `url_hint`、`token_hint`、`url_set`、`token_set`。还有几个只读的辅助字段：`default_prompt`、`voices`、`ambience_kinds`、`languages`、`notify_kinds`、`sample_lines`、`summary_ready`。 |
| `PUT /calls/api/settings` | 一个 JSON 对象，只放要修改的字段。响应同 `GET`。 |

`PUT` 的规则：

- 机密字段（`gemini_api_key`、`deepseek_api_key`、`gcal_client_secret`、`gcal_refresh_token`）缺失或为 `""` 时，保留原来保存的值；`"<name>_clear": true` 则会清空它。
- `notify_channels` 是整体替换。一行 `{id, kind, name, enabled, url, token, target}` 的 `url` 或 `token` 为空时，会保留 `id` 和 `kind` 相同的那一行原来保存的值；没有 `id` 的行是新增的；你没列出来的行会被删除。
- `reject_numbers` 是一个由 `{number, reason}` 组成的列表。

示例——切换语言并添加一个拦截号码：

```text
PUT /calls/api/settings
{"language": "ja", "reject_numbers": [{"number": "0800*", "reason": "telemarketing"}]}
```

字段名就是 [`crates/agent/src/settings_store.rs`](../../crates/agent/src/settings_store.rs) 里 `RuntimeSettings` 的字段名，每一个在那里都有说明。

## 通知

| 方法和路径 | 内容 |
|---|---|
| `POST /calls/api/notify/test` | Body `{"channel": {kind, url, token, target, id?}}`。把测试消息发到那一个渠道（已保存的机密会按 `id` 补上）。返回 `200 {ok: true}`，或者 `502 {ok: false, error}` 并附上该服务的返回内容。 |
| `POST /calls/api/notify/send` | Body `{"title": "...", "body": "..."}`。把你的消息发到每一个已启用的渠道。只要有一个渠道接受，就返回 `{ok: true}`。 |

你自己的 webhook 会收到的 JSON，在[通知](NOTIFICATIONS.md#webhook-json)里有说明。

## 语音和背景音试听

`GET /calls/api/preview` 会返回一个 WAV。查询参数：`voice`（声音名称，或者用 `none` 表示只要背景音）、`ambience`（`off` 或某一种背景音）、`level`（1–3）、`regen=true`（重新生成，不用缓存的示例）、`clip=reject`（用该声音播报的拦截号码语音）。某个声音的第一次请求会开启一次短暂的 Gemini 会话。

## 手机与健康状态

| 方法和路径 | 内容 |
|---|---|
| `GET /calls/api/devices` | `{ok, usb: {enabled, adb_found, adb_path, error, phones: [{serial, state, model, reverse, app_installed}]}, connected: [...], labels: {<phone id>: <model>}, calls_in_progress, phones: [...]}`。最外层的 `phones` 是主程序启动后在 control 连接上见过的每部手机：`{serial, connected, mode, supports_mode, model, app_version, in_call, last_seen_ms}`。`mode` 是 `ai`（AI 接听）或 `normal`（普通手机，见[手机设置](PHONE.md#普通手机模式)）；早于这个功能的应用，`supports_mode` 为 false，始终是 `ai`。 |
| `POST /calls/api/devices/{serial}/mode` | Body `{"mode": "ai" \| "normal"}`。让手机切换，最多等 15 秒等它回复。总是返回 `{ok, mode, error_code}`：`mode` 是切换后手机的模式（未知时为 `null`），成功时 `error_code` 为 `null`，否则是这些代码之一：`offline`（没连接）、`unsupported`（应用太旧）、`in_call`、`timeout`、`busy`（上一次切换还在进行）、`no_root`、`save_failed`、`dialer_failed`、`screening_failed`、`dnd_failed`、`volume_failed`、`invalid_mode`、`failed`。Body 里的 `mode` 无法识别时返回 400 和 `invalid_mode`。 |
| `GET /health` | 一个很大的扁平 JSON 对象，用于排查问题：版本、语言、哪些东西已配置、已连接的手机、进行中的通话；不含密钥或 token。`control_idle_clients`／`control_idle_serials` 统计的是所有已连接的手机；`normal_mode_serials` 列出切成普通手机、AI 不接它们电话的那几部。设置了控制台令牌而请求没有带上时，只会返回 `ok`、`version`、`language`、`active_call_count` 和 `control_idle_clients`（用于监控已经足够）。 |

## 手机这一端

手机通过 WebSocket 连接到 `/` 或 `/ws`（两者相同）：每部手机一条长期保持的“control”连接，每通接听的电话一条“media”连接，双向传输 JSON 文本帧和原始的 16 kHz 单声道 16 位 PCM 二进制帧。从这台电脑本身发起的连接（USB 隧道）一律接受；其他连接在设置了该令牌时，必须带上 `?token=<DEVICE_TOKEN>`。

消息列表在 [`android/README.md`](../../android/README.md#websocket-protocol)（英文）里。任何会说这个协议的东西都可以代替手机——所以要加上 SIP 或软电话前端，可以完全不动通话逻辑。

手机模式用到 control 连接上的这些消息（双方都会忽略不认识的内容，所以旧应用、旧主程序照样能用）：

| 方向 | 消息 | 含义 |
|---|---|---|
| 手机 → 主程序 | `hello` 加上 `phone_mode`（`ai`／`normal`）和 `in_call` | 连接时手机的模式。`mode` 仍然是 `CONTROL_IDLE`（连接类型）。没有 `phone_mode` = 旧应用：`ai`，不能从这里切换。 |
| 手机 → 主程序 | `{"type": "status", "mode": "ai" \| "normal", "in_call": bool}` | 模式或通话状态变了。 |
| 主程序 → 手机 | `{"type": "set_mode", "mode": "ai" \| "normal", "id": "<uuid>"}` | 切换（由上面的路由发出）。 |
| 手机 → 主程序 | `{"type": "set_mode_result", "id": "<同一个>", "ok": bool, "mode": "...", "error": "<代码>"}` | 结果；成功时 `error` 为空字符串，代码同上。 |
| 主程序 → 手机 | `{"type": "agent_info", "console_url": "http://…/calls/"}` | 紧跟在 hello 之后，只有知道控制台地址（`public_url`）时才发。应用会因此显示“打开网页控制台”按钮。 |
