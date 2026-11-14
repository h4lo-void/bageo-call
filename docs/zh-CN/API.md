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

出错时返回 `{ok: false, code, error}`：`code` 是固定的英文代码，可以用来判断（`not_found`、`invalid_id`、`no_audio`、`preview_failed`、`save_failed`、`missing_field`、`auth_failed`…；网页控制台会用你的语言显示），`error` 是详细说明（大多是英文）。例外：`POST /calls/api/devices/{serial}/mode` 总是返回 `{ok, mode, error_code}`（见下方）。

## 通话

| 方法和路径 | 内容 |
|---|---|
| `GET /calls/api/list` | 通话记录，最新的在前。查询参数：`limit`、`offset`、`q`（对号码、手机、事件、文字记录做文本搜索）、`caller`、`serial`、`status`、`has_transcript`。响应：`{ok, calls: [...], total, limit, offset}`。 |
| `GET /calls/api/call/{id}` | 单个通话：`{ok, call, notes, calendar_link}`。`notes`（摘要写好之前是 null）包含 `summary`、`dialogue`、`end_hint`、`source`、`updated_at`，通话中约定了日期时还有 `calendar` = `{title, start, end, all_day}`（本地时间，`YYYY-MM-DDTHH:MM`）。 |
| `GET /calls/api/call/{id}/event.ics` | 日历建议，iCalendar 文件格式。该通话没有建议时返回 `404`。 |
| `GET /calls/api/audio/{id}/listen` | 你当时会听到的整通电话（对方和 AI 混在一起），WAV，支持 range 请求。 |
| `GET /calls/api/audio/{id}/remote` · `/tts` · `/audio/{id}` | 只有对方 · 只有 AI · 原始的混音录音。 |
| `GET /calls/api/live` | 进行中的通话和已连接的手机：`{ok, count, calls, control_idle_clients, control_idle_serials, normal_mode_serials}`。`normal_mode_serials` 是已连接、但切成普通手机的手机；网页不把它们算进待命手机。 |
| `GET /calls/api/stats` | `{ok, stats: {total, today, voicemails, blocked}}`。 |
| `GET /calls/api/export.csv` | 筛选后的列表，CSV 格式（查询参数同 `list`，最多 2000 行）。 |

通话记录（`call`，以及 `calls` 里的每一条）包含的字段有：`id`、`serial`（手机）、`caller_id`、`started_at` / `ended_at`（RFC 3339，UTC）、`duration_ms`、`status`（`ringing`、`active`、`ended`）、`events`（按顺序发生的事——例如 `active`、`persona_stall_spam`、`reject_list`、`persona_ai_fail_hangup`、`ended`）、`summary`、有哪些音频文件，对方跟 AI 约了时间时还有 `appointment`（通话结束时的样子，见[日程预约](#日程预约)）；通话后的变化（你的决定、短信结果）会更新在 `notes.appointment`。

在 `notes.dialogue` 里，每一行是一次发言，前面带有主程序语言的说话人标签和分隔符：简体中文是 `对方：`／`接听：`，繁体中文 `對方：`／`接聽：`，英文 `Caller: `／`AI: `，日文 `相手：`／`AI：`，韩文 `상대: `／`AI: `，西班牙文 `Llamante: `／`IA: `，法文 `Appelant : `／`IA : `，德文 `Anrufer: `／`KI: `，葡萄牙文 `Chamador: `／`IA: `。旧版本保存的记录无论语言都是 `對方：`／`接聽：`。

## 设置

| 方法和路径 | 内容 |
|---|---|
| `GET /calls/api/settings` | `{ok, settings, storage_path, console_url, console_ip_url}`：`console_url` 是当前使用的控制台地址（你填的，或自动带入的；只有这台电脑能打开时为空），`console_ip_url` 是把 `.local` 名称换成局域网 IP 的同一个地址。机密字段会以掩码形式返回（`"abc…wxyz"`），并带一个 `<name>_set` 标志；通知渠道返回时 `url` / `token` 是空的，另外附带 `url_hint`、`token_hint`、`url_set`、`token_set`。还有几个只读的辅助字段：`default_prompt`、`voices`、`ambience_kinds`、`languages`、`countries`（“各 SIM 卡的主要语言”可选的国家代码）、`seen_sim_countries`（主程序启动后手机上报过的 SIM 卡国家）、`notify_kinds`、`sample_lines`、`summary_ready`。 |
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
| `POST /calls/api/notify/test` | Body `{"channel": {kind, url, token, target, id?}}`。把测试消息发到那一个渠道（已保存的机密会按 `id` 补上）。返回 `200 {ok: true}`，或者 `502 {ok: false, code, field, kind, error}`：`code` 是出错原因（`missing_field`、`bad_url`、`bad_value`、`auth_failed`、`service_not_found`、`http_error`、`service_error`、`network_error`、`send_failed`），`field` 是 `url` / `token` / `target` 中的哪一项（或为空），`error` 是该服务的返回内容。 |
| `POST /calls/api/notify/send` | Body `{"title": "...", "body": "..."}`。把你的消息发到每一个已启用的渠道。只要有一个渠道接受，就返回 `{ok: true}`。 |

你自己的 webhook 会收到的 JSON，在[通知](NOTIFICATIONS.md#webhook-json)里有说明。

## 语音和背景音试听

`GET /calls/api/preview` 会返回一个 WAV。查询参数：`voice`（声音名称，或者用 `none` 表示只要背景音）、`ambience`（`off` 或某一种背景音）、`level`（1–3）、`regen=true`（重新生成，不用缓存的示例）、`clip=reject`（用该声音播报的拦截号码语音）、`lang`（`ja`、`zh-TW`…；留空 = 已保存的语言）。内置的每个声音在每种语言都已预先录好（`assets/voices/`），试听不用连 Gemini；只有 `regen=true` 或没有预录的声音才会开一次短暂的会话。

## 日程预约

`appointment_mode` 设为 `confirm` 或 `auto` 时（见[设置](SETTINGS.md#摘要与日历)），对方跟 AI 约好时间的那通电话会留下一条任务（`intent` 为 `appointment`），通话记录、它的 notes 和任务上都有一个 `appointment` 对象：

```json
{
  "start": "2026-11-14T15:00", "end": "2026-11-14T16:00",
  "title": "看房", "caller_name": "陈先生",
  "mode": "confirm", "status": "pending",
  "tag": "6f1c2a9e-3b7d-4e0a-9c55-2d8f1b7e4a10"
}
```

时间是本地时间（`TZ`）。`status`：`pending`（等你决定）、`booked`（通话中已写进日历）、`confirmed`、`declined`。`event_id`：写进 Google 日历后的日程 ID。`auto_error`：“有空直接答应”没能排入的原因。`replaced`：同一通电话里先前已写进日历、后来改掉的时间。`sms`：`sent`，或短信没发出的原因（`offline`、`old_app`、`timeout`、`busy`、`no_number`，或手机返回的错误代码）。`tag`：这通电话在它写进日历的日程上留的标记（见下句）。`tg_card`：发给这条预约的 Telegram 卡片 `{chat_id, message_id}`，在别处决定后用来去掉卡片上的按钮。空的字段不会出现。主程序写进日历的日程都带私有扩展属性 `bageo_call=<appointment.tag>`，写入时没收到响应的那条之后能找回来，不会写两次。

| 方法和路径 | 内容 |
|---|---|
| `GET /calls/api/tasks` | 任务，最新的在前（最多 200 条）：`{ok, tasks}`。网页的“待确认的预约”就是 `appointment.status` 为 `pending` 的任务。 |
| `POST /calls/api/tasks/{id}/confirm` | 对预约来说等于在网页或 Telegram 卡片上按 **✅ 确定**——写进 Google 日历，接着（开启 `appointment_sms` 时）给对方发短信，可能要等最多 90 秒。响应 `{ok, task, message}`；`message` 是结果，网页显示的和 Telegram Bot 回复的是同一段。这条预约发过 Telegram 卡片时，卡片上的按钮会去掉，并把 `message` 回复到那个聊天（尽力而为，Telegram 失败不影响决定）。只有第一次决定才算：之后再确定或婉拒——包括第一次还在处理时发来的，或在 Telegram 卡片上按的——都只回复“已经处理过了”，不会有任何变化。请求要带 `Content-Type: application/json`（不用 body），否则返回 HTTP 400 `bad_request`，这样别的网页无法通过你局域网里的浏览器替你决定。 |
| `POST /calls/api/tasks/{id}/ignore` | 对预约来说等于 **❌ 婉拒**（通话中已经写进日历的那条会删掉；开启 `appointment_sms` 时发短信）。响应与规则同上。 |

## 手机与健康状态

| 方法和路径 | 内容 |
|---|---|
| `GET /calls/api/devices` | `{ok, usb: {enabled, adb_found, adb_path, error, phones: [{serial, state, model, reverse, app_installed}]}, connected: [...], labels: {<phone id>: <model>}, calls_in_progress, phones: [...]}`。最外层的 `phones` 是主程序启动后在 control 连接上见过的每部手机：`{serial, connected, mode, supports_mode, model, app_version, in_call, last_seen_ms}`。`mode` 是 `ai`（AI 接听）或 `normal`（普通手机，见[手机设置](PHONE.md#普通手机模式)）；早于这个功能的应用，`supports_mode` 为 false，始终是 `ai`。 |
| `POST /calls/api/devices/{serial}/mode` | Body `{"mode": "ai" \| "normal"}`。让手机切换，最多等 15 秒等它回复。总是返回 `{ok, mode, error_code}`：`mode` 是切换后手机的模式（未知时为 `null`），成功时 `error_code` 为 `null`，否则是这些代码之一：`offline`（没连接）、`unsupported`（应用太旧）、`in_call`、`busy`（上一次切换还在进行）、`no_root`（手机上的 Bageo 模块没有在运行）、`save_failed`、`dialer_failed`、`screening_failed`、`dnd_failed`、`volume_failed`、`failed`，或 `timeout`。`timeout` 的意思是“还没确认”，不是失败：切换本身可能让手机应用重新启动，它重新连上后才报告结果，所以稍后再读一次模式（`GET /calls/api/devices`）。Body 由主程序自己解析：不是 JSON、没有字符串 `mode`、或 serial 不是字母、数字、`-`、`_` 组成时，返回 HTTP 400、同样格式的 JSON，`error_code` 为 `bad_request`；`mode` 无法识别时返回 400 和 `invalid_mode`。请求必须带 `Content-Type: application/json`（没有也返回 400 `bad_request`），这样别的网页没法通过你局域网里的浏览器偷偷切换手机。 |
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
| 主程序 → 手机 | `{"type": "agent_info", "console_url": "http://…/calls/"}` | 紧跟在 hello 之后，主程序有控制台地址时才发：你填的地址（`public_url`）；没填而控制台对局域网开放时，用局域网 IP 的地址（有些手机打不开 `.local` 名称）。应用会因此显示“打开网页控制台”按钮。 |

发给来电者的短信（日程预约）也走同一条连接：

| 方向 | 消息 | 含义 |
|---|---|---|
| 手机 → 主程序 | `hello` 加上 `"supports_sms": true` | 应用能替主程序发短信。没有这个字段 = 旧应用：不会让它发短信。 |
| 主程序 → 手机 | `{"type": "send_sms", "id": "<最多 64 个字符>", "to": "+886912345678", "text": "<UTF-8，最多 500 个字符>"}`，可以再加 `"sub_id": <整数>` | 只发给接那通电话的手机。`to` 只有数字，开头可以有 `+`。`sub_id` 指定用哪张 SIM 发（就是手机回报的 `sim_sub_id`）；没有就用手机默认的短信 SIM。已经发出过的 id 再来一次，手机只会再回复一次结果，不会重复发送。 |
| 手机 → 主程序 | `{"type": "send_sms_result", "id": "<同一个>", "ok": true}` 或 `{"type": "send_sms_result", "id": "<同一个>", "ok": false, "error": "<代码>"}` | 短信每一段都回报发出后才回复（手机最多等 60 秒，超时回复 `timeout`；之后才收到回报，会再发一次真正的结果）。`error` 是 `no_permission`, `bad_request`, `no_service`, `radio_off`, `generic_failure`, `timeout`, `exception` 之一。主程序最多等 90 秒，没有回复就算失败。control 连接重连后，手机会把最近 20 个 id 的结果再发一次，主程序用 `id` 去掉重复的。 |
| 手机 → 主程序 | `{"type": "send_sms_delivered", "id": "<同一个>", "ok": true \| false}` | 运营商回报的送达结果：每一段都送到对方是 `true`，有一段确定送不到是 `false`。运营商有回报才会发（手机最多等一小时）；重连后也会跟结果一起再发一次。 |

通话也用同一条连接，多了这些可以省略的字段和消息（旧应用、旧主程序不发也不认识，照样能用）：

| 方向 | 消息 | 含义 |
|---|---|---|
| 手机 → 主程序 | `hello` 加上 `"supports_reject": true` | 应用认识 `answer_plan` 的 `reject`，会在响铃时拒接。没有这个字段 = 旧应用，让它拒接它还是会接起来，所以主程序改成接起来播放语音。 |
| 手机 → 主程序 | `incoming` 和 media 连接的 `hello` 加上 `"sim_sub_id": <整数>` | 这通电话打进来的那张 SIM 在 Android 里的订阅 id，跟 `sim_country` 一起发。不知道就不发。隐藏号码时 `caller_id` 是空字符串（不会是上一通的号码）。 |
| 主程序 → 手机 | `answer_plan` 加上 `"reject": true` | 响铃中直接拒接，不接起来。电话结束后照样发 `incoming_cancel`。晚到也有效，只要电话还在响。 |
| 手机 → 主程序 | `{"type": "call_waiting_rejected", "serial": "...", "caller_id": "...", "sim_country": "tw", "sim_sub_id": <整数>}` | 通话中又有第二通打进来，手机立刻拒接（AI 模式）。字段是被拒那一通的信息；不知道 `sim_sub_id` 就不发。 |
| 主程序 → 手机 | `place_call` 加上 `"sub_id": <整数>` | 用那张 SIM 拨出。找不到那张 SIM 就用手机默认的。手机有电话在响或通话中时不拨，回复 `place_call_failed`，`"reason": "busy"`。 |

主程序这边：主程序不再等之后才到的结果照样会记下来；手机之后用同一个 `id` 送来的真正结果会取代 `timeout`（还在等的发送会继续等它），同样的结果再来、或已经有确定结果的短信收到不同结果，都不会改。拦截号码只有在 `reject_mode` 是 `decline`、而且手机说过 `supports_reject` 时才让手机拒接；否则接起来播放语音，通知会写清楚是哪一种。收到 `call_waiting_rejected` 会留下通话记录并发出通知。回拨从接原来那通电话的手机打出去时才带 `sub_id`。
