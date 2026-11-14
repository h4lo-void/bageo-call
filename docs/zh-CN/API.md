# API

网页控制台是一个建立在小型 JSON API 上的单页应用，页面能做的事，脚本都能做：读取通话和文字记录、修改设置、决定日程预约、管理联系人、把手机切换成 AI 接听或普通手机，以及让 AI 替你打电话。本页说明 Bageo 1.0 的每一条路由。

- **基础地址：** 主程序监听的位置，默认是 `http://127.0.0.1:8787`。下面的路由除了 `/health` 和手机用的 WebSocket，都以 `/calls/api/` 开头。
- **格式：** UTF-8 编码的 JSON；路由另有说明的除外（WAV、CSV、vCard、iCalendar）。发送 JSON 请求体时请带上 `Content-Type: application/json`。
- **时间：** 通话记录用 UTC 的 RFC 3339（`2026-11-14T07:02:11Z`）；日程预约用本地时间（`TZ`），格式 `YYYY-MM-DDTHH:MM`；AI 代打用带时区偏移的本地时间（`2026-11-15T19:00:00+08:00`）。
- **兼容性：** 字段名来自 [`crates/agent/src`](../../crates/agent/src) 里的 Rust 结构体。以后的版本可能新增字段，不认识的字段请直接忽略。

## 访问

没有设置 `CALLS_UI_TOKEN` 时，任何能连到这个端口的人都能使用 API（默认只有这台电脑自己）。设置后，每个请求都要用下面其中一种方式带上令牌：

```text
GET /calls/api/list?token=<token>
Authorization: Bearer <token>
X-Calls-Token: <token>
```

令牌错误或缺失会得到 HTTP 401。`/health` 不带令牌也会响应，但只返回监控用的几个字段（见[手机与健康状态](#手机与健康状态)）。

```bash
curl -s -H "Authorization: Bearer $CALLS_UI_TOKEN" \
  "http://127.0.0.1:8787/calls/api/list?limit=5"
```

## 错误与状态码

错误响应是 `{ok: false, code, error}`：`code` 是固定的英文代码，可以拿来判断（网页控制台会用你的语言显示），`error` 是详细说明（大多是英文）。有两组路由用自己的格式：`POST /calls/api/devices/{serial}/mode` 一律返回 `{ok, mode, error_code}`；AI 代打的路由在 `error` 和 `code` 里放同一个代码。

| HTTP | 什么时候 | 常见的 `code` |
|---|---|---|
| 200 | 完成。 | — |
| 400 | 请求有误：不是 JSON、缺少 `Content-Type: application/json`、id 或取值不对。 | `bad_request`、`invalid_id`、`missing_field`、`invalid_contact`、AI 代打的代码 |
| 401 | 缺少令牌或令牌错误。 | `unauthorized` |
| 404 | 找不到这通电话、任务、联系人或代打。 | `not_found`、`task_not_found`、`contact_not_found`、`no_audio` |
| 409 | 这次修改与已保存的数据冲突。 | `contact_exists`、`sms_not_retryable` |
| 413／415 | 导入的文件太大／格式不认识。 | `file_too_large`、`unsupported_file` |
| 502 | 外部服务失败（通知渠道、Gemini）。 | `auth_failed`、`network_error`、`ai_failed`、`preview_failed` |

### 防止其他网页冒用

控制台没有登录 Cookie，但你局域网里的浏览器仍可能被别的网页诱导去向它发请求。所以凡是会拨电话、发短信、做决定或改动已保存数据的路由，一律要求 `Content-Type: application/json`（缺少时返回 HTTP 400 `bad_request`）。别的网站的页面要发送这个请求头，必须先经过 CORS 预检，而主程序从不响应预检。适用的路由：

- `POST /calls/api/tasks/{id}/confirm` 和 `…/ignore`；
- `POST /calls/api/appointments/{id}/retry-sms`；
- `POST /calls/api/devices/{serial}/mode`；
- `POST /calls/api/contacts`、`PUT /calls/api/contacts/{id}`、`POST /calls/api/contacts/batch`（`POST /calls/api/contacts/import` 则需要 `X-Filename` 请求头）；
- `POST /calls/api/errands`、`…/{id}/cancel`、`…/{id}/retry`、`…/{id}/say`。

允许空请求体的路由，空请求体按 `{}` 处理。这是第二道防线；第一道是控制台对局域网开放时必须设置的 `CALLS_UI_TOKEN`。

## 通话

| 方法和路径 | 内容 |
|---|---|
| `GET /calls/api/list` | 通话记录，最新的在前。查询参数：`limit`、`offset`、`q`（对号码、手机、事件、文字记录做全文搜索）、`caller`、`serial`、`status`、`has_transcript`。返回：`{ok, calls: [...], total, limit, offset, contacts_by_number}`；`contacts_by_number` 是 `{<caller_id>: {id, name}}`，列出这一页里属于联系人的来电者。 |
| `GET /calls/api/call/{id}` | 单通电话：`{ok, call, notes, contact, calendar_link, calendar_links}`。`contact` 是 `{id, name, note}` 或 `null`。`calendar_links` = `{google, outlook, office365, yahoo}`：各网页日历已填好这通电话日历建议的“新建日程”表单（`calendar_link` 是 Google 那个，保留给旧版使用）。 |
| `GET /calls/api/call/{id}/event.ics` | 日历建议，iCalendar 文件。该通话没有建议时返回 HTTP 404。用控制台令牌打开，或用 `?k=<密钥>`：通知链接带的是只能打开这一个文件的 HMAC 密钥，不是令牌。 |
| `GET /calls/api/audio/{id}/listen` | 你当时会听到的整通电话（对方和 AI 混在一起），WAV，支持 Range 请求。 |
| `GET /calls/api/audio/{id}/remote` · `/tts` · `/audio/{id}` | 只有对方 · 只有 AI · 原始混音录音。文件不存在时返回 HTTP 404 `no_audio`。 |
| `GET /calls/api/live` | 进行中的通话和已连接的手机：`{ok, count, calls, control_idle_clients, control_idle_serials, normal_mode_serials}`。`normal_mode_serials` 是已连接、但切换成普通手机的手机；网页不把它们算作待命手机。 |
| `GET /calls/api/stats` | `{ok, stats: {total, today, voicemails, blocked}}`。 |
| `GET /calls/api/export.csv` | 筛选后的列表，CSV 格式（查询参数同 `list`，最多 2000 行）。 |

### 通话记录

通话记录（`call`，以及 `calls` 里的每一条）包含的字段有：

| 字段 | 含义 |
|---|---|
| `id` | 记录 id，`/call/{id}` 和 `/audio/{id}` 路由都用它。 |
| `serial` | 接听（或拨出）这通电话的手机。 |
| `caller_id` | 手机上报的对方号码；未显示号码时为空字符串。 |
| `started_at`／`ended_at` | RFC 3339，UTC。 |
| `duration_ms` | 通话时长。 |
| `status` | `ringing`、`active`、`ended`。 |
| `events` | 依次发生的事——例如 `active`、`persona_stall_spam`（判定为推销／诈骗）、`reject_list`（拦截号码）、`call_waiting_rejected`（通话中拒接的第二通）、`ai_callback`（日程预约的 AI 回拨）、`ai_errand`（AI 代打）、`persona_ai_fail_hangup`、`ended`。 |
| `summary` | 写好后的摘要文字。 |
| `appointment` | 对方跟 AI 约了时间时：通话结束那一刻的预约（见[日程预约与任务](#日程预约与任务)）。之后的变化（你的决定、短信和回拨结果）会更新到 `notes.appointment`。 |
| 音频标记 | 有哪些录音文件。 |

### 摘要、文字记录与翻译

摘要写好之前 `notes` 为 `null`，之后包含：

| 字段 | 含义 |
|---|---|
| `summary` | 用通知语言写的摘要。 |
| `dialogue` | 文字记录，一行一次发言，前面加上主程序语言的说话人标签和分隔符：简体中文是 `对方：`／`接听：`，繁体中文 `對方：`／`接聽：`，英语 `Caller: `／`AI: `，日语 `相手：`／`AI：`，韩语 `상대: `／`AI: `，西班牙语 `Llamante: `／`IA: `，法语 `Appelant : `／`IA : `，德语 `Anrufer: `／`KI: `，葡萄牙语 `Chamador: `／`IA: `。旧版保存的记录无论什么语言都是 `對方：`／`接聽：`。每一行保留说话时的语言。 |
| `dialogue_translations` | 有翻译时才出现：一个数组，与 `dialogue` 一行对一项（相同索引）。用别的语言说的那一行，这里是翻译成通知语言的译文；本来就是你的语言、太短或没翻译的那一行为 `null`。网页控制台会把译文显示在原文下面。 |
| `end_hint`、`source`、`updated_at` | 通话怎么结束、由什么生成、什么时候生成。 |
| `calendar` | 通话中约定了日期时：`{title, start, end, all_day}`（本地时间，`YYYY-MM-DDTHH:MM`）。 |
| `appointment` | 预约当前的状态。 |

```json
{
  "dialogue": "对方：Hello, is Alex there?\n接听：Yes, speaking. Who's calling?",
  "dialogue_translations": ["你好，Alex 在吗？", "我就是，请问哪位？"]
}
```

来电者在联系人里时：通话列表显示名字（`contacts_by_number`）、通话详情也有（`contact`）、通话摘要通知的标题在号码后面加上名字，普通电话的 AI 提示词也会说明是谁打来的。

## 设置

| 方法和路径 | 内容 |
|---|---|
| `GET /calls/api/settings` | `{ok, settings, storage_path, console_url, console_ip_url}`。`console_url` 是当前使用的控制台地址（你填的，或自动生成的；只有这台电脑能打开时为空），`console_ip_url` 是把 `.local` 名称换成局域网 IP 的同一地址。机密字段以掩码形式返回（`"abc…wxyz"`），并附带 `<name>_set` 标记；通知渠道返回时 `url`／`token` 为空，另附 `url_hint`、`token_hint`、`url_set`、`token_set`。只读的辅助字段：`default_prompt`、`voices`、`ambience_kinds`、`languages`、`countries`（“各 SIM 卡的主要语言”可选的国家代码）、`seen_sim_countries`（主程序启动后手机上报过的 SIM 卡国家）、`notify_kinds`、`sample_lines`、`summary_ready`。 |
| `PUT /calls/api/settings` | 一个 JSON 对象，只放要修改的字段。返回同 `GET`。下一通电话就生效，无需重启。 |

值得了解的字段（全部都在 [`settings_store.rs`](../../crates/agent/src/settings_store.rs) 的 `RuntimeSettings` 和[设置参考](SETTINGS.md)里有说明）：

| 字段 | 取值 |
|---|---|
| `owner_name` | 你的名字，用于 `{owner}`。 |
| `owner_aliases` | 字符串数组，最多 10 个：其他人怎么叫你。多出的和空白的会被丢弃。 |
| `answer_style` | `assistant`／`persona`（本人）。 |
| `errand_identity` | `assistant`（默认）／`owner`：AI 代打时说自己是谁；其他值保存为 `assistant`。 |
| `summary_provider` | `gemini`（默认）／`custom`。`custom` 使用 `summary_api_base`、`summary_model`（填了地址就必填）和 `deepseek_api_key`（该服务的密钥）。旧的 `auto`／`deepseek` 会在设置加载时转换一次。 |
| `calendar_app` | `google`、`outlook`、`office365`、`apple`、`yahoo`、`ics` 或空（还没选）；其他值保存为空。 |
| `appointment_mode` | `off`／`confirm`（旧的 `auto` 按 `confirm` 处理）。 |
| `appointment_sms` | `true`／`false`：是否发短信告诉对方你的决定。 |
| `appointment_reply` | `auto`、`sms` 或 `call`。 |
| `reject_numbers`、`reject_mode` | `{number, reason}` 的列表；`decline`（响铃时拒接）或接起播放语音。 |
| `country_code` | 只有数字（`886`、`1`）。控制台不显示；这里有值时优先于 `COUNTRY_CODE`。 |
| `gemini_thinking_level` | `low`／`medium`／`high`（只对 thinking 模型有效）。 |

`PUT` 的规则：

- 机密字段（`gemini_api_key`、`deepseek_api_key`）缺省或为 `""` 时，保留原来保存的值；`"<name>_clear": true` 则清空它。
- `notify_channels` 整体替换。一行 `{id, kind, name, enabled, url, token, target}` 的 `url` 或 `token` 为空时，保留 `id` 和 `kind` 都相同的那一行原来保存的值；没有 `id` 的行是新增的；你没列出来的行会被删除。
- 旧版保存、本版已经没有的键，读入时忽略，下次保存时清除：Google 日历凭据（`gcal_*`）、`calendar_auto_add`、`intl_carrier_prompt`，以及已移除的模拟日历的测试号码与测试短信设置。

```bash
curl -s -X PUT "http://127.0.0.1:8787/calls/api/settings" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"language": "ja", "owner_aliases": ["Alex", "小明"], "errand_identity": "owner",
       "reject_numbers": [{"number": "0800*", "reason": "电话营销"}]}'
```

## 通知

| 方法和路径 | 内容 |
|---|---|
| `POST /calls/api/notify/test` | 请求体 `{"channel": {kind, url, token, target, id?}}`。把测试消息发到这一个渠道（已保存的机密按 `id` 补上）。返回 HTTP 200 `{ok: true}`，或 502 `{ok: false, code, field, kind, error}`：`code` 是出错原因（`missing_field`、`bad_url`、`bad_value`、`auth_failed`、`service_not_found`、`http_error`、`service_error`、`network_error`、`send_failed`），`field` 是 `url`／`token`／`target` 中的哪一项（或为空），`error` 是该服务的响应。 |
| `POST /calls/api/notify/send` | 请求体 `{"title": "...", "body": "..."}`。把你的消息发到每个已启用的渠道。只要有一个渠道接受，就返回 `{ok: true}`。[短信转发工具](../../tools/sms_forward/README.md)（英文）用的就是这条路由。 |

你自己的 webhook 会收到的 JSON 和每一种事件，在[通知](NOTIFICATIONS.md#webhook-json)里有说明。

## 语音和背景音试听

`GET /calls/api/preview` 返回一个 WAV。查询参数：`voice`（声音名称，或用 `none` 表示只要背景音）、`ambience`（`off` 或某种背景音）、`level`（1–3）、`regen=true`（重新生成，不用已保存的示例）、`clip=reject`（用该声音念的拦截号码语音）、`lang`（`ja`、`zh-TW`…；留空 = 已保存的语言）。内置的 30 个声音在 9 种语言下都预先录好了试听和拒接语音——`assets/voices/` 里共 1,080 段——所以试听不需要连接 Gemini；只有 `regen=true` 或没有预录的声音才会建立一次连接。错误：502 `preview_failed`。

## 日程预约与任务

`appointment_mode` 设为 `confirm` 时（见[设置](SETTINGS.md#摘要与日历)），对方跟 AI 约好时间的那通电话会留下一条任务（`intent` 为 `appointment`），通话记录、它的 notes 和任务上都有一个 `appointment` 对象：

```json
{
  "start": "2026-11-14T15:00", "end": "2026-11-14T16:00",
  "title": "看房", "caller_name": "陈先生",
  "mode": "confirm", "status": "confirmed",
  "reply": "sms", "sms": "sent", "call_lang": "zh-CN",
  "tag": "6f1c2a9e-3b7d-4e0a-9c55-2d8f1b7e4a10",
  "tg_card": {"chat_id": "123456789", "message_id": 42}
}
```

| 字段 | 含义 |
|---|---|
| `start`、`end` | 本地时间（`TZ`）。对方只是顺口提的备选时间不会覆盖谈好的时间；同一通电话另外约的第二件事会变成另一条任务。 |
| `title`、`caller_name` | 约的是什么事、对方怎么自称（来电者自己的话）。 |
| `status` | `pending`（等你决定）、`confirmed`、`declined`；`booked` 只会出现在旧版的记录里。 |
| `reply` | 怎么告诉对方：`sms`、`call` 或 `none`。 |
| `sms` | `sent`，或短信没发出的原因：`offline`（手机未连接）、`old_app`（App 不能发短信）、`timeout`、`busy`（同一条短信正在发送）、`no_number`（收不到短信的号码），或手机返回的代码：`no_permission`、`bad_request`、`no_service`、`radio_off`、`generic_failure`、`exception`。网页和 Telegram 回复会用大白话写。 |
| `call` | AI 回拨：`queued`（等拨给同一个号码的前一通回拨结束）、`dialing`、`connected`（手机已拨号，AI 等对方接起来出声）、`answered`（对方已得知结果）、`no_answer`（70 秒内没人出声）、`failed`、`done`（通话结束、结果不明），或没拨出的原因：`no_phone`、`send_failed`、`no_number`、`not_callable`（境外、免费电话、增值或特殊号码）。手机的回报也记录在任务的 `events`（`callback_place_call_ack`、`callback_place_call_failed`、`callback_outbound_ready`、`callback_done`、`callback_failed`）。 |
| `call_lang` | 那通电话说的语言（`zh-CN`、`en`…）。AI 回拨就说这个语言，对方接起来出声后才说你的回复，只说一次。没有这个字段的旧记录按设置 `language`。 |
| `tg_card` | 为这条预约发出的 Telegram 卡片 `{chat_id, message_id}`；在别处决定后用来去掉卡片上的按钮。 |
| `tag` | 这条预约固定的 id。 |

空的字段不会出现。

| 方法和路径 | 内容 |
|---|---|
| `GET /calls/api/tasks` | 任务，最新的在前（最多 200 条）：`{ok, tasks}`。预约任务另外带有 `calendar_links`（`{google, outlook, office365, yahoo}`）、`reply` = `{sms, call, default}`（这位来电者能不能收短信、能不能回拨，以及“默认回复方式”选的方式），以及 `sms_route` = `{serial, label}`：你没另选手机时短信从哪部手机发出（接这通电话的手机；`label` 是它在 `device_labels` 里的名称，没有就是 id）。网页的“待处理事项”就是 `appointment.status` 为 `pending` 的任务。 |
| `GET /calls/api/tasks/{id}/event.ics` | 预约的 iCalendar 文件（Apple 日历和其他应用）。用控制台令牌或链接里的 `?k=<HMAC 密钥>` 打开。 |
| `POST /calls/api/tasks/{id}/confirm` | **同意**，与在网页或 Telegram 卡片上点按相同。请求体：空、`{}` 或 `{"reply": "sms" \| "call" \| "none"}`（缺省：按“默认回复方式”；选了这位来电者用不了的方式就不通知）。返回 `{ok, task, message}`；`message` 是用大白话写的结果，与 Telegram Bot 回复的同一段，附上你所选 `calendar_app` 的“添加到日历”链接。发短信最多可能等 90 秒；回拨交给手机拨出（在 `appointment.call` 查看进度）。 |
| `POST /calls/api/tasks/{id}/ignore` | **不同意**：用同样的方式告诉对方，礼貌婉拒。请求体、返回与规则同上。 |
| `POST /calls/api/appointments/{id}/retry-sms` | 已决定的预约短信没发出时，再发一次。请求体：空、`{}` 或 `{"serial": "<手机>"}`（另一部已连接的手机；缺省：接这通电话的手机）。返回 `{ok, sms, message}`：发出时 `ok` 为 true，`sms` 是新的代码。还没决定、已经发出或本来就不会发（`no_number`）时返回 HTTP 409 `sms_not_retryable`（附当前的 `sms`）；404 `task_not_found`；400 `invalid_serial`／`bad_request`。 |

决定的规则：

- **只有第一次决定才算。** 之后再同意或不同意——包括第一次还在处理时发来的，或在 Telegram 卡片上点的——都只返回“已经处理过了”，不会有任何变化。
- 这条预约发过 Telegram 卡片时，卡片上的按钮会被去掉，并把 `message` 回复到那个聊天里（尽力而为，Telegram 失败不影响决定）。
- 短信和回拨都从那通电话打进来的 SIM 发出（`sub_id`）。短信只写时间（含星期），不写约的是什么事。
- 推销、诈骗电话不会留下预约；通话后才被判定为这类电话的，预约也会作废。

```bash
curl -s -X POST "http://127.0.0.1:8787/calls/api/tasks/$TASK/confirm" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"reply": "call"}'
```

```json
{"ok": true, "task": {"id": "…", "appointment": {"status": "confirmed", "reply": "call", "call": "dialing"}},
 "message": "✅ 已同意预约\n11月14日（周六）15:00–16:00 看房\nAI 正在回拨给 0912345678\n📅 添加到日历：https://calendar.google.com/…"}
```

## AI 代打

让 AI 替你打电话：给它号码（或联系人）和要办的事——订座、跟朋友约时间、问商家事情——一部 AI 模式的手机就会拨出。

**通话中。** 网页控制台可以看实时字幕。遇到你没交代清楚的事，AI 会先跟对方说要确认一下，再问你（`ask_owner` 函数）：你正在看这一条（6 秒内有人读取过它的实时画面），问题就出现在网页上，最多等 40 秒；没在看或超时，AI 会问对方有哪些可行的选项、说你确认后再回电，然后礼貌收尾。AI 问你时会停下来等答案，这段时间对这件事什么都不答应；对方先道别，这个问题就到此为止。在你交代的范围内的选择（“7 点半到 8 点也可以”）AI 会自己答应，不会来问你。通话中你在网页上打的字，会作为指示交给 AI。

**通话后。** 摘要模型根据交办内容整理结果，发一条通知（标题“📞 AI 代打：〈名字或号码〉”，用通知语言；webhook 事件为 `ai_errand`）。你没回答的事如果 AI 先答应了，结果一律不会是 `done`，而是 `need_owner`：请你确认，不行再打回去改。

**用什么身份。** 设置 `errand_identity`（`assistant` 助理，默认；`owner` 本人），每条也能单独选。无论哪种，被认真问是不是真人时都会承认是 AI，不提供交办以外的个人信息，不答应付款、订金、转账。

**能拨哪些号码。** 不拨紧急号码、7 位以下的短号和增值付费号码（例如台湾 020x 开头的 0204、0209，+1 900／976，日本 0990，英国 09…）；免费电话可以拨。没有 `+` 的号码一律当成本国号码（`country_code`）；如果开头是国际冠码（`00`、台湾的 `01x`／`00x`、北美 `011`、日本 `010`）就是 `not_allowed`（境外号码请写 `+`），台湾号码去掉开头的 0 后以 1 开头也会拦下。

**用哪部手机、哪张 SIM。** 从一部已连接、处于 AI 模式、有该国 SIM 的手机拨出。主程序从手机上报的来电（`incoming` 和 media `hello` 里的 `sim_sub_id`、`sim_country`）记下每部手机有哪些 SIM（subscription id 与国家），控制连接的 `hello` 带有 `sims: [{sub_id, country}]` 列表时也会采用（1.0 版 App 还不会发送）；这些保存在 `phone_sims.json`。完全不知道有哪些 SIM 的手机不接代打（`sim_unknown`：先给它打一通）。发给手机的是 E.164 格式的号码和要用的 SIM（`sub_id`）；这通电话自己的上报（media `hello`）显示用的是别的 SIM、或没有上报时，主程序会在 AI 开口前立即挂断（`failed`、`call_code` `wrong_sim`，会通知你），并先把那张卡从这部手机的记录里去掉，等以后有来电上报它再加回来。

**排队与上限。** 手机通话中：这一条排队（`queued`），超过 10 分钟算失败（`phone_busy`）。同一个号码一次只打一通，与日程预约的 AI 回拨共用排队；排在同号码前一通后面的最多等 25 分钟（`number_busy`）。一通最长 10 分钟（9 分半开始收尾）。语音菜单：AI 最多等 90 秒等人工接听（它不能按键；`keypad_menu`）；运营商录音（“您拨打的号码是空号”）：直接挂断（`carrier_message`）。不会自动重拨：要“再打一次”由你来点。取消拨号中的代打，手机和号码会立即释放给下一通；万一那通电话还是接通了，会在 AI 开口前挂断。

### 代打对象

```json
{
  "id": "e3f1a29c0b7d4e55", "created_at": "2026-11-14T18:02:11+08:00", "updated_at": "2026-11-14T18:05:40+08:00",
  "number": "02123456789", "name": "海底捞", "contact_id": "",
  "task": "订明天晚上 7 点 4 位，名字写王先生。7 点半到 8 点也可以。",
  "identity": "assistant", "serial": "pixel-12345", "device_label": "Pixel 9",
  "status": "done", "call_code": "answered", "attempts": 1, "record_id": "c8b2…",
  "follow_up_of": "",
  "questions": [{"id": "q1", "text": "7 点满了，7 点半还是 8 点 15？", "options": ["7 点半", "8 点 15"],
                 "answer": "7 点半", "answered_by": "owner", "asked_at": "2026-11-14T18:03:05+08:00"}],
  "result": {"outcome": "done", "summary": "已订明天 7 点半 4 位，名字王先生",
             "details": "座位保留 15 分钟。", "follow_up": "", "options": [],
             "event": {"start": "2026-11-15T19:30:00+08:00", "title": "海底捞晚饭"}},
  "history": []
}
```

| 字段 | 含义 |
|---|---|
| `id` | `e` 加 16 个十六进制字符。 |
| `number` | 你写的号码，去掉空格和连字符。 |
| `name`、`contact_id` | 要用的名字（没填就用联系人的名字），以及来自哪位联系人。 |
| `task` | 你自己写的交办内容（最多 1,000 字）。 |
| `identity` | `assistant` 或 `owner`。 |
| `serial`、`device_label` | 拨出的手机和它的名称。 |
| `status` | `queued`、`dialing`（电话在响，还没有人出声）、`on_call`（对方接起来出声了）、`done`、`no_answer`、`failed`、`cancelled`。后四个是最终状态。 |
| `call_code` | 拨号进行到哪一步：`dialing`、`queued`、`answered`、`no_answer`、`failed`、`phone_busy`、`number_busy`、`no_phone`、`send_failed`、`keypad_menu`、`carrier_message`、`wrong_sim`、`agent_restarted`、`cancelled`。 |
| `attempts` | 从 1 开始，每点一次“再打一次”加 1。 |
| `record_id` | 这一次的通话记录（`GET /calls/api/call/{id}`：文字记录和录音）。 |
| `follow_up_of` | 这通是接着哪一条先前的代打（那一条最后 20 行文字记录会放进 AI 的脚本）。 |
| `questions` | `[{id, text, options, answer, answered_by, asked_at}]`：AI 问过你的问题。`answered_by`：`owner`（你回答了）、`timeout`（你在看但没来得及回答）、`absent`（没人在看）；还在等时为 `null`。选项最多 4 个。 |
| `result` | `null` 或 `{outcome, summary, details, follow_up, options, event}`。`outcome`：`done`、`partial`、`not_done`、`need_owner`（选项在 `options`，最多 8 个）、`no_answer`、`voicemail`、`failed`。`event`：`null` 或 `{start, end, title}`（带时区偏移的本地时间；没谈定结束时间就没有 `end`）。 |
| `history` | 之前几次的尝试，`[{attempt, status, result, record_id, ended_at, questions}]`。 |

代打保存在数据文件夹的 `errands.json`（保留最新 500 条）。主程序重启时还在进行中的代打会变成 `failed`（`call_code` `agent_restarted`）。

### 代打路由

| 方法和路径 | 内容 |
|---|---|
| `GET /calls/api/errands` | `{ok, errands}`，最新的在前，最多 200 条。 |
| `POST /calls/api/errands` | 新建一条。请求体 `{number, name, contact_id, task, identity, serial, follow_up_of}`：`task` 和号码必填（或给出联系人、先前的代打，用它们的号码）；`identity` 留空 = `errand_identity`；`serial` 留空 = 任何合适的手机。返回 `{ok, errand}`（状态 `dialing`，手机或号码忙时为 `queued`）。 |
| `POST /calls/api/errands/{id}/cancel` | 排队中：取消；拨号中或通话中：挂断。已经结束的原样返回。`{ok, errand}`。 |
| `POST /calls/api/errands/{id}/retry` | 同一条再打一次（`attempts` 加 1，上一次移到 `history`）。`{ok, errand}`；还在进行中（或上一通还没结束）：400 `busy`；与手机相关的代码同新建。 |
| `DELETE /calls/api/errands/{id}` | 从列表删除（通话记录会保留）。`{ok}`；拨号中或通话中：400 `busy`。 |
| `GET /calls/api/errands/{id}/live?after=<seq>` | `{status, lines, question, watching}`。`lines`：`seq` 之后的字幕，`[{seq, who, text, at}]`，`who` 为 `ai`、`callee`、`owner` 或 `system`（保留最新 400 行；1.2 秒没有新字就算一行结束）。`question`：`null` 或 `{id, text, options, asked_at, expires_at}`，等你回答的问题。`watching`：你是否算“正在看”。读取这条路由会让你算作正在看 6 秒，所以页面打开时每隔几秒读一次。 |
| `POST /calls/api/errands/{id}/say` | 请求体 `{text, question_id}`（最多 300 字）：带 `question_id` 就是回答那个问题；不带时，有问题在等就当作它的答案，否则作为给 AI 的指示（“跟他说 8 点也可以”）。`{ok}`。 |

错误码（HTTP 400 `{ok: false, error, code}`，两者同值）：

| 代码 | 路由 | 含义 |
|---|---|---|
| `no_number` | 新建 | 没有号码，也没有可以取号码的联系人或先前代打。 |
| `bad_number` | 新建 | 不是电话号码（有字母、`+` 不在开头、超过 15 位、按那个国家来说太短）。 |
| `not_allowed` | 新建 | 紧急号码、短号、增值付费号码，或用国际冠码代替 `+`。 |
| `no_task` | 新建 | `task` 为空。 |
| `task_too_long` | 新建 | `task` 超过 1,000 字。 |
| `no_phone` | 新建、再打 | 没有已连接、处于 AI 模式的手机（或指定的 `serial` 不是这样的手机）。 |
| `sim_unknown` | 新建、再打 | 没有合适的手机，而且至少有一部的 SIM 主程序还不知道：先给它打一通。 |
| `no_sim_for_country` | 新建、再打 | 没有手机有这个号码所属国家的 SIM。 |
| `not_found` | 新建 | `contact_id` 或 `follow_up_of` 不存在（这里返回 400）。 |
| `busy` | 再打、删除 | 这一条还在拨号或通话中。 |
| `not_on_call` | say | 电话不在通话中（或已经结束）。 |
| `empty_text`／`text_too_long` | say | 没有内容／超过 300 字。 |
| `question_closed` | say | 那个问题已经回答、超时，或不是正在等的那一个。 |
| `bad_request` | 所有 `POST` | 缺少 `Content-Type: application/json`，或请求体不是 JSON 对象。 |

所有 `…/errands/{id}…` 路径，代打 id 不存在（或格式不对）时一律返回 HTTP 404 `not_found`。

```bash
curl -s -X POST "http://127.0.0.1:8787/calls/api/errands" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"number": "021-2345-6789", "name": "海底捞",
       "task": "订明天晚上 7 点 4 位，名字写王先生。7 点半到 8 点也可以。"}'

curl -s "http://127.0.0.1:8787/calls/api/errands/e3f1a29c0b7d4e55/live?after=0" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN"

curl -s -X POST "http://127.0.0.1:8787/calls/api/errands/e3f1a29c0b7d4e55/say" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"text": "7 点半可以", "question_id": "q1"}'
```

```json
{"status": "on_call",
 "lines": [{"seq": 3, "who": "callee", "text": "7 点已经满了，7 点半或者 8 点 15 可以吗？", "at": "2026-11-14T18:03:01+08:00"},
           {"seq": 4, "who": "ai", "text": "我确认一下哈。", "at": "2026-11-14T18:03:04+08:00"}],
 "question": {"id": "q1", "text": "7 点满了，7 点半还是 8 点 15？", "options": ["7 点半", "8 点 15"],
              "asked_at": "2026-11-14T18:03:05+08:00", "expires_at": "2026-11-14T18:03:45+08:00"},
 "watching": true}
```

## 联系人

给打来的号码配上名字，保存在通话记录文件夹（`CALLS_DATA_DIR`）的 `contacts.json`。号码无论怎么写都能对上：`+886912345678`、`0912-345-678`、`886912345678` 是同一个号码；没有国家代码的号码按你的国家（`country_code`）解读，通话则按接这通电话的 SIM 卡所属国家解读。联系人也用于 AI 代打（选联系人就不用输入号码）和推销判断：已保存的联系人谈到钱，不会被当成推销或诈骗。

一位联系人是 `{id, name, phones: [...], note, source, updated_at}`；`source` 是 `manual`、`vcf`、`csv`、`text` 或 `ai`（由 Gemini 读取）。一个号码只属于一位联系人。

| 方法和路径 | 内容 |
|---|---|
| `GET /calls/api/contacts` | 查询参数 `q`（名字、备注或号码，号码怎么写都行）、`limit`、`offset`。返回 `{ok, contacts, total, offset, count}`：按名字排序的匹配结果（共 `total` 条），以及全部联系人数（`count`）。 |
| `POST /calls/api/contacts` | 请求体 `{name, phones: [...], note}`，返回 `{ok, contact}`。号码已经属于别的联系人：HTTP 409 `contact_exists`，附 `contact: {id, name}`；没有名字或没有号码：400 `invalid_contact`。 |
| `PUT /calls/api/contacts/{id}` | 同样的请求体，替换名字、号码和备注。404 `contact_not_found`、409、400 同上。 |
| `DELETE /calls/api/contacts/{id}` | `{ok}`。 |
| `POST /calls/api/contacts/import` | 请求体就是文件本身（最大 10 MB），文件名经 URL 编码后放在 `X-Filename`。只读取、**不保存**（预览）：`{ok, file, format, ai, entries: [{name, phones, note, source, duplicate}], skipped}`。`format`：`vcf`、`csv`、`text`、`xlsx`、`docx`、`pdf` 或 `image`；`ai`：由 Gemini 读取；`duplicate`：`null`，或这条重复的已保存联系人 `{id, name, by: "phone" \| "name"}`；`skipped`：因为没有名字或号码而跳过的条数。错误：400 `empty_file`、413 `file_too_large`、415 `unsupported_file`、400 `need_gemini_key`、502 `ai_failed`（Gemini 的回复在 `error`）。 |
| `POST /calls/api/contacts/batch` | 请求体 `{contacts: [{name, phones, note, source}], duplicates: "merge" \| "skip"}`：导入时勾选要保留的那些。与已保存联系人重复的，合并进去（补上新号码和备注）或跳过。返回 `{ok, added, merged, skipped, count}`。 |
| `GET /calls/api/contacts/export.vcf` | 全部联系人导出为 vCard 3.0 文件。 |

主程序自己能读的格式：vCard 2.1、3.0、4.0（包括 quoted-printable、各种 `CHARSET`）；有表头（Google 通讯录、Outlook，或列名类似 *姓名*、*电话*、Name、Phone）或没有表头的 CSV、TSV，编码为 UTF-8、UTF-16 或电脑的本地编码（Big5、GBK、Shift_JIS、EUC-KR、Windows-1252）；每行一个“名字 号码”的纯文本；`.xlsx`、`.docx` 里的表格。PDF、图片（打印名单的照片、通讯录截图）和读不出人名的文件交给 Gemini 的视觉模型（`gemini-flash-lite-latest`，使用设置里的 Gemini 密钥）。

```bash
curl -s -X POST "http://127.0.0.1:8787/calls/api/contacts/import" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" \
  -H "X-Filename: $(printf %s '家人名单.pdf' | jq -sRr @uri)" \
  --data-binary @"家人名单.pdf"
```

```text
POST /calls/api/contacts
Content-Type: application/json
{"name": "林小美", "phones": ["0912 000 000", "+1 202 555 0143"], "note": "我姐"}

{"ok": true, "contact": {"id": "4f0c9a…", "name": "林小美", "phones": ["0912 000 000", "+1 202 555 0143"],
 "note": "我姐", "source": "manual", "updated_at": "2026-11-14T08:00:00Z"}}
```

## 手机与健康状态

| 方法和路径 | 内容 |
|---|---|
| `GET /calls/api/devices` | `{ok, usb: {enabled, adb_found, adb_path, error, phones: [{serial, state, model, reverse, app_installed}]}, connected: [...], labels: {<phone id>: <model>}, calls_in_progress, phones: [...]}`。最外层的 `phones` 是主程序启动后在 control 连接上见过的每部手机：`{serial, connected, mode, supports_mode, model, app_version, in_call, last_seen_ms}`。`mode` 为 `ai`（AI 接听）或 `normal`（普通手机，见[手机设置](PHONE.md#普通手机模式)）；比这个功能旧的 App，`supports_mode` 为 false，始终是 `ai`。 |
| `POST /calls/api/devices/{serial}/mode` | 请求体 `{"mode": "ai" \| "normal"}`。让手机切换，最多等 15 秒等它回复。一律返回 `{ok, mode, error_code}`（见下方）。 |
| `GET /health` | 一个很大的扁平 JSON 对象，用于排查问题：版本、语言、哪些已配置、已连接的手机、进行中的通话；不含密钥或令牌。`control_idle_clients`／`control_idle_serials` 统计所有已连接的手机；`normal_mode_serials` 列出切换成普通手机的那几部；`local_time` 是主程序按 `TZ` 的时钟。设置了控制台令牌而请求没带时，只返回 `ok`、`version`、`language`、`active_call_count` 和 `control_idle_clients`——用于监控已经足够。 |

`POST /calls/api/devices/{serial}/mode` 的细节：`mode` 是切换后手机的模式（未知时为 `null`）；成功时 `error_code` 为 `null`，否则是下面之一：

| `error_code` | 含义 |
|---|---|
| `offline` | 手机未连接。 |
| `unsupported` | App 比模式切换功能旧。 |
| `in_call` | 有电话在响或通话中；结束后再试。 |
| `busy` | 上一次切换还在进行。 |
| `no_root` | Bageo 模块的 root 助手没有接手：模块没有运行。 |
| `save_failed`、`dialer_failed`、`screening_failed`、`dnd_failed`、`volume_failed`、`failed` | 没成功的那一步。再切换一次会把每一步重做一遍。 |
| `timeout` | 尚未确认——不是失败：切换本身可能让手机 App 重启，它重新连上后才上报。稍后再读一次 `GET /calls/api/devices`。 |
| `bad_request`／`invalid_mode` | HTTP 400：不是 JSON、缺少 `Content-Type: application/json`、没有字符串 `mode`、serial 不是由字母、数字、`-`、`_` 组成；或 `mode` 不认识。 |

```bash
curl -s -X POST "http://127.0.0.1:8787/calls/api/devices/pixel-12345/mode" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"mode": "normal"}'
# {"ok": true, "mode": "normal", "error_code": null}
```

## 手机这一端（WebSocket）

手机通过 WebSocket 连接到 `/` 或 `/ws`（两者相同）：每部手机一条长期保持的 *control* 连接，每通电话一条 *media* 连接，双向传输 JSON 文本帧和原始的 16 kHz 单声道 16 位小端 PCM 二进制帧（手机每 60 ms 发送一帧）。来自这台电脑本身的连接（USB 通道）一律接受；其他连接在设置了令牌时必须带上 `?token=<DEVICE_TOKEN>`。

逐字段的完整消息列表在 [`android/README.md`](../../android/README.md#websocket-protocol)（英文）。任何会说这套协议的程序都可以代替手机——所以要加上 SIP 或软电话前端，完全不用改动通话逻辑。双方都会忽略不认识的内容，所以旧 App、旧主程序照样能用。

### Control 连接

| 方向 | 消息 | 含义 |
|---|---|---|
| 手机 → 主程序 | `hello` | `serial`、`mode` = `CONTROL_IDLE`、`model`、`app_version`、`phone_mode`（`ai`／`normal`）、`in_call`、`supports_sms`、`supports_reject`。没有 `phone_mode` = 比模式切换旧的 App（始终是 `ai`）。没有 `supports_sms` = 不会让它发短信。没有 `supports_reject` = 拦截号码改为接起来播放语音。 |
| 手机 → 主程序 | `ping` | 每 15 秒一次。 |
| 手机 → 主程序 | `status` | `{mode, in_call}`：模式或通话状态变了。 |
| 手机 → 主程序 | `incoming`、`incoming_cancel` | 有电话在响（`caller_id`，未显示号码时为空字符串；`sim_country`；已知时带 `sim_sub_id`）／接起来之前就结束了。 |
| 主程序 → 手机 | `answer_plan` | `{delay_ms, prewarm, reject}`：响多久再接，或 `reject: true` 响铃中直接拒接（`reject_mode` 为 `decline` 的拦截号码）。 |
| 手机 → 主程序 | `call_waiting_rejected` | `{serial, caller_id, sim_country, sim_sub_id}`：通话中又有第二通打进来，手机立即拒接（AI 模式）。会留下通话记录并发出“📵”通知。 |
| 主程序 → 手机／手机 → 主程序 | `set_mode`／`set_mode_result` | `{mode, id}`／`{id, ok, mode, error}`：远程切换模式；`error` 使用上面的代码。 |
| 主程序 → 手机 | `agent_info` | `{console_url}`：供 App 的“**打开网页控制台**”按钮使用。主程序知道地址时才在 hello 之后发送：`public_url`，或控制台对局域网开放时的局域网 IP 地址。 |
| 主程序 → 手机 | `place_call` | `{number, call_id, task_id, sub_id}`：拨出电话（AI 回拨或 AI 代打），用 subscription id 为 `sub_id` 的 SIM 拨号（找不到那张 SIM 就用手机默认的）。手机有电话在响或通话中时不拨，返回 `place_call_failed`，`reason` 为 `busy`。 |
| 手机 → 主程序 | `place_call_ack`、`place_call_failed`、`outbound_ready`、`call_end` | 拨出的电话已拨号／拨不出去／接通／结束。 |
| 主程序 → 手机 | `send_sms` | `{id, to, text, sub_id}`：给来电者发短信（日程预约）。`id` 1–64 个字符；`to` 只有数字，开头可以有 `+`；`text` 最多 500 个字符；`sub_id` 指定 SIM（手机上报的 `sim_sub_id`），没有就用默认短信 SIM。已经发出过的 id 再来一次只会再返回一次结果，不会重复发送。 |
| 手机 → 主程序 | `send_sms_result` | `{id, ok, error}`：每一段都回报后才返回（手机最多等 60 秒，超时返回 `timeout`；之后才收到回报会再发一次真正的结果）。`error`：`no_permission`、`bad_request`、`no_service`、`radio_off`、`generic_failure`、`timeout`、`exception`。主程序最多等 90 秒。重连后手机会把最近 20 个 id 的结果再发一次，主程序按 `id` 去重。 |
| 手机 → 主程序 | `send_sms_delivered` | `{id, ok}`：运营商给出送达回执时才发送（手机最多等一小时）。 |

### Media 连接

| 方向 | 消息 | 含义 |
|---|---|---|
| 手机 → 主程序 | `hello` | `serial`、`caller_id`、`sample_rate`（16000）、`mode` = `SILENT_LOCAL`、`sim_country`、`sim_sub_id`、`model`、`app_version`。拨出的电话，主程序会拿 `sim_sub_id` 比对它指定的 SIM（代打不符即 `wrong_sim`）。 |
| 手机 → 主程序 | `ringing`、`call_end` | 对方号码（之后才知道时会再发）／通话结束。 |
| 主程序 → 手机 | `tts_start`、`tts_end`、`hangup` | 接下来有声音／声音结束／挂断。 |
| 双向 | 二进制 | 对方的声音（手机 → 主程序）和 AI 的声音（主程序 → 手机），格式同上。 |

主程序这一侧：短信结果在主程序不再等待之后才到，照样会记录；手机之后用同一个 `id` 发来的真正结果会取代 `timeout`；同样的结果再来，或已有确定结果的短信收到不同结果，都不会改变。拦截号码只有在 `reject_mode` 为 `decline`、且手机声明了 `supports_reject` 时才让手机拒接；否则接起来播放语音，通知会写清楚是哪一种。AI 回拨和 AI 代打都带 `sub_id`，双卡手机会用正确的号码拨出。

