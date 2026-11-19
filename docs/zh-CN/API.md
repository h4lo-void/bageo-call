# API 参考：用程序控制你的 AI 电话助理

Bageo 网页控制台做的每件事，都是通过你自己电脑上的一个小型 JSON API 完成的，所以写个脚本也能做到：

- 读取通话记录、文字记录和摘要，修改设置。
- 同意或婉拒来电者跟 AI 约的时间，管理联系人。
- 把手机在“AI 接听”和一般手机之间切换。
- 让 AI 替你打电话。

这一页涵盖 Bageo 1.6 控制台和手机会用到的每一条路由，也包括已 root 的 Android 手机使用的 WebSocket 协议。

| | |
|---|---|
| **基础地址** | Bageo 监听的位置：默认在局域网监听 `8787` 端口（可用时也会监听 `80` 端口供 `http://bageo.local/` 使用），设为 `AGENT_HOST=127.0.0.1` 时则只在 `http://127.0.0.1:8787`。API 路由都以 `/calls/api/` 开头，例外是 `/health` 和手机用的 WebSocket。控制台本身在 `/calls/`（`/` 会跳转过去）。 |
| **AI 代理** | `POST /mcp` 是 Model Context Protocol 服务器，有自己的 Token，默认关闭：见 [MCP](MCP.md)。 |
| **格式** | UTF-8 的 JSON；路由另有说明的除外（WAV、CSV、vCard、iCalendar）。发送 JSON 请求体时请带 `Content-Type: application/json`。 |
| **时间** | 通话记录：UTC 的 RFC 3339（`2026-11-14T07:02:11Z`）。日程预约：当地时间（`TZ`），格式 `YYYY-MM-DDTHH:MM`。AI 代打：带时差的当地时间（`2026-11-15T19:00:00+08:00`）。 |
| **兼容性** | 字段名来自 [`crates/agent/src`](../../crates/agent/src) 里的 Rust 结构体。以后的版本可能新增字段，不认识的字段请直接忽略。 |

## 先试试看

Bageo 在这台电脑上运行时，下面几行可以直接用：

```bash
curl -s http://127.0.0.1:8787/health
curl -s "http://127.0.0.1:8787/calls/api/list?limit=5"
curl -s http://127.0.0.1:8787/calls/api/stats
```

设置了控制台密码（`CALLS_UI_TOKEN`）的话，加上 `-H "Authorization: Bearer $CALLS_UI_TOKEN"`（见[访问](#访问)）。常见的用法：

| 你想要 | 用这个 |
|---|---|
| 把新的文字记录导入笔记或客户管理系统 | `GET /calls/api/list?has_transcript=true`，再对每一条 `GET /calls/api/call/{id}` |
| 用脚本打电话订位 | `POST /calls/api/errands`，轮询 `…/errands/{id}/live`，用 `…/say` 回答 AI |
| 用你自己的按钮同意预约 | `POST /calls/api/tasks/{id}/confirm` |
| 在家时让手机照常响、自己接 | `POST /calls/api/devices/{serial}/mode`，请求体 `{"mode": "normal"}` |
| 把任何消息转到你的通知渠道 | `POST /calls/api/notify/send` |
| 监控 Bageo | `GET /health`（简版状态不带密码也能读） |

## 访问

默认（未设置 `AGENT_HOST`）会对局域网监听：本机在还没设置密码前就能调用，其他设备连进来时则必须带控制台密码（`CALLS_UI_TOKEN` 或设置中保存的密码），并且会检查 `Host` 请求头与限制密码错误次数。设置密码后，每个请求都要用下面其中一种方式带上密码：

```text
GET /calls/api/list?token=<token>
Authorization: Bearer <token>
X-Calls-Token: <token>
```

密码错误或缺失会得到 HTTP 401，内容是 `{"error": "unauthorized", "code": "unauthorized", "hint": …}`。`/health` 不带密码也会响应，但只返回监控用的几个字段（见[手机与健康状态](#手机与健康状态)）。完整的 `/health` 数据也要带密码；控制台用 `GET /calls/api/health` 读取。

```bash
curl -s -H "Authorization: Bearer $CALLS_UI_TOKEN" \
  "http://127.0.0.1:8787/calls/api/list?limit=5"
```

## 错误与状态码

错误响应是 `{ok: false, code, error}`。程序请看 `code`：它是固定的英文代码，网页控制台会用你的语言显示；`error` 是详细说明，大多是英文。有两组路由用自己的格式：手机模式切换返回 `{ok, mode, error_code}`；AI 代打的路由在 `error` 和 `code` 放同一个代码。

| HTTP | 什么时候 | 常见的 `code` |
|---|---|---|
| 200 | 完成。 | — |
| 400 | 请求有误：不是 JSON、没带 `Content-Type: application/json`、id 或值不对。 | `bad_request`、`invalid_id`、`missing_field`、`invalid_contact`、AI 代打的代码 |
| 401 | 缺少密码或密码错误。 | `unauthorized` |
| 404 | 找不到这通电话、待处理事项、联系人、代打，或找不到文件。 | `not_found`、`task_not_found`、`contact_not_found`、`no_audio`、`no_calendar` |
| 409 | 这个修改跟已保存的数据冲突。 | `contact_exists`、`sms_not_retryable`、`call_in_progress`、`device_online`、`device_in_call` |
| 413／415 | 导入的文件太大／格式不认识。 | `file_too_large`、`too_many_rows`、`unsupported_file` |
| 500 | Bageo 保存或读取自己的文件失败。 | `save_failed`、`contacts_save_failed`、`import_failed` |
| 502 | 外部服务失败（通知渠道、Gemini）。 | `auth_failed`、`network_error`、`ai_failed`、`preview_failed` |

### 防止其他网页冒用

控制台没有登录 Cookie，但你局域网里某台电脑的浏览器打开别的网页时，那个网页仍可能偷偷向它发请求。

所以凡是会拨电话、发短信、做决定或修改已保存数据的路由，一律要求 `Content-Type: application/json`，缺少就返回 HTTP 400 `bad_request`。别的网站要发这个请求头必须先经过 CORS 预检，而 Bageo 从不响应预检。适用的路由：

- `POST /calls/api/initial-password` 和 `POST /calls/api/phone-pairing/renew`；
- `POST /calls/api/call/{id}/scam-feedback`；
- `POST /calls/api/tasks/{id}/confirm` 和 `…/ignore`；
- `POST /calls/api/appointments/{id}/retry-sms`；
- `POST /calls/api/calls/clear`；
- `POST /calls/api/sms/send`、`…/retry`、`…/clear` 和 `DELETE /calls/api/sms/thread`；
- `POST /calls/api/devices/{serial}/mode` 和 `PUT /calls/api/devices/{serial}/sim-number`；
- `POST /calls/api/contacts`、`PUT /calls/api/contacts/{id}`、`POST /calls/api/contacts/batch`、`POST /calls/api/contacts/block`（`POST /calls/api/contacts/import` 则改为要带 `X-Filename` 请求头）；
- `POST /calls/api/errands`、`…/{id}/cancel`、`…/{id}/retry`、`…/{id}/say`；
- `POST /calls/api/backup/run`、`…/export`、`…/file-ticket`、`…/restore`、`…/delete`、`…/unlock`、`…/apply`、`…/discard`、`…/open-folder`（`POST /calls/api/backup/import` 则改成要带 `X-Filename` 标头）。

允许空请求体的路由，空请求体视为 `{}`。这是第二道防线；第一道是控制台对局域网开放时一定要设置密码（`CALLS_UI_TOKEN`）。

## 通话

| 方法与路径 | 内容 |
|---|---|
| `GET /calls/api/list` | 通话记录，最新的在前。查询参数和响应格式见下方。 |
| `GET /calls/api/call/{id}` | 单条通话：`{ok, call, notes, contact, calendar_link, calendar_links, scam_names}`。字段说明见下方。 |
| `DELETE /calls/api/call/{id}` | 删除一条已结束的通话记录、录音 WAV、`_notes.json` 和相关待处理事项：`{ok, id, tasks_deleted}`。仍在响铃或通话中返回 HTTP 409 `call_in_progress`。 |
| `POST /calls/api/calls/clear` | 删除所有已结束的通话记录、录音 WAV、`_notes.json` 和相关待处理事项（进行中的通话会保留）：`{ok, deleted, tasks_deleted}`。 |
| `POST /calls/api/call/{id}/scam-feedback` | 你对这通的标记：JSON `{"verdict": "scam" \| "sales" \| "safe"}`，可另带内置的 `typology`。记在记录上（`scam_feedback`），号码没隐藏的话也记在这个号码上，它之后打来会参考。返回 `{ok, call, number}`；HTTP 400 `invalid_verdict`／`invalid_typology`。 |
| `GET /calls/api/call/{id}/event.ics` | 日历建议的 iCalendar 文件；这通电话没有建议时返回 HTTP 404 `no_calendar`。用控制台密码打开，或用 `?k=<密钥>`：通知链接里带的是只能打开这一个文件的 HMAC 密钥。 |
| `GET /calls/api/audio/{id}/listen` | 你当时会听到的整通电话（对方和 AI 混在一起），WAV，支持 range 请求，可以拖动播放位置（加 `?download=1` 会附上 `Content-Disposition: attachment`）。 |
| `GET /calls/api/audio/{id}/remote` · `/tts` · `/vm` · `/audio/{id}` | 只有对方 · 只有 AI · 旧版菜单模式的语音留言 · 原始混音录音。文件不存在时返回 HTTP 404 `no_audio`。 |
| `GET /calls/api/live` | 进行中的通话和已连接的手机：`{ok, count, calls, control_idle_clients, control_idle_serials, normal_mode_serials}`。 |
| `GET /calls/api/stats` | `{ok, stats: {total, today, voicemails, blocked}}`。 |
| `GET /calls/api/export.csv` | 筛选后的列表，CSV 格式：查询参数同 `list`，最多 2000 行（`limit` 可以再调低）。响应头 `X-Calls-Export-Total` 和 `X-Calls-Export-Rows` 分别是符合多少条、写出多少条。 |

`GET /calls/api/list` 的内容：

- 查询参数：`limit`（最多 500；不带就返回全部）、`offset`、`q`（对号码、手机、事件、文字记录和摘要做文本搜索）、`caller`、`serial`、`status`、`has_transcript`、`has_voicemail`。
- 响应：`{ok, calls, total, limit, offset, tasks_by_call, contacts_by_number}`。
- `tasks_by_call` 是 `{<通话 id>: {id, status, summary}}`：这一页每通电话最新的一条待处理事项。
- `contacts_by_number` 是 `{<caller_id>: {id, name}}`：这一页里是联系人的来电者。

`GET /calls/api/call/{id}` 的内容：

- `contact` 是 `{id, name, note}` 或 `null`。
- `calendar_links` = `{google, outlook, office365, yahoo}`：各网页日历已填好这通电话日历建议的“新建活动”表单。
- `calendar_link` 是 Google 那个，留给旧版使用。
- `scam_names`：判断出的话术类型在控制台各语言里的名称，没有就是 `null`。

`GET /calls/api/live`：`normal_mode_serials` 是已连接、但切换成一般手机的手机；网页不把它们算进待命手机。

### 通话记录

每条通话记录（`call`，以及 `calls` 里的每一条）带有的字段包括：

| 字段 | 含义 |
|---|---|
| `id` | 记录 id，`/call/{id}` 和 `/audio/{id}` 路由都用它。 |
| `serial` | 接这通（或拨出这通）的手机。 |
| `caller_id` | 手机上报的对方号码；隐藏号码时为空字符串。 |
| `started_at`／`ended_at` | RFC 3339，UTC。 |
| `duration_ms` | 通话时长。 |
| `status` | `ringing`、`active`、`ended`。旧版的记录可能还有 `blocked_spam`。 |
| `events` | 依次发生的事，例如 `active`、`ended`。事件名称和含义见下方。 |
| `scam` | 判定是推销或诈骗时才有：`{kind, typology, confidence, source, released}`。各字段见下方。 |
| `scam_feedback` | 你在通话页的标记：`scam`、`sales` 或 `safe`。 |
| `summary` | 写好后的摘要文字。 |
| `appointment` | 对方跟 AI 约了时间时：通话结束那一刻的预约（见[日程预约与待处理事项](#日程预约与任务)）。之后的变化（你的决定、短信和回拨结果）更新到 `notes.appointment`。 |
| `sim_country`、`sim_sub_id` | 这通电话是哪张 SIM 卡接的（手机有上报时）。 |
| `audio_path`、`remote_audio_path`、`tts_audio_path`、`voicemail_path` | 有哪些录音文件（数据文件夹里的路径）。 |

`events` 的事件名称：

- `persona_stall_spam`：判定是推销或诈骗。
- `persona_scam_detected`：陪聊关闭时判定是推销或诈骗。
- `persona_stall_released`：之后确认是正常电话、改回正常应对。
- `reject_list`／`reject_list_declined`：黑名单上的号码（播语音／直接拒接）。
- `call_waiting_rejected`：通话中拒接的第二通。
- `ivr_hangup`：自动语音菜单／机器人来电提前挂断。
- `whitelist_urgent_transfer`：白名单急事来电转接给你。
- `ai_callback`：日程预约的 AI 回拨。
- `ai_errand`：AI 代打。
- 其他事件：`active`、`persona_ai_fail_hangup`、`ended`。

`scam` 的字段：

- `kind`：`scam` 或 `sales`。
- `typology`：内置的话术类型。
- `source`：`review`（复审判的）、`hard_words`（复审没回答，但对方说了只有诈骗才会说的话）或 `keywords`（复审一直失败，只靠关键词）。
- `released`：之后再看一次，放回正常应对。

### 摘要、文字记录与翻译

摘要写好之前 `notes` 是 `null`，之后包含：

| 字段 | 含义 |
|---|---|
| `summary` | 用通知语言写的摘要。 |
| `dialogue` | 文字记录，一行一次发言，开头是说话人标签。标签用 Bageo 设置的语言，对照见下方；每一行保留当时说的语言。 |
| `dialogue_translations` | 有翻译时才出现：跟 `dialogue` 一行对一条（同样的索引）。用别的语言说的那一行，这里是翻成通知语言的译文；本来就是你的语言、太短或没翻译的是 `null`。网页控制台把译文显示在原文下面。 |
| `recording_text` | 对方声音另外做的语音转文字，作为备份（AI 拖时间陪聊的推销、诈骗电话为空）。 |
| `end_hint`、`source`、`updated_at` | 通话怎么结束、谁写的、什么时候写的。 |
| `calendar` | 通话中约定了日期时：`{title, start, end, all_day}`（当地时间，`YYYY-MM-DDTHH:MM`）。 |
| `appointment` | 预约目前的状态。 |

`dialogue` 的说话人标签（按 Bageo 的语言）：

- 简体中文：`对方：`／`接听：`
- 繁体中文：`對方：`／`接聽：`
- 英文：`Caller: `／`AI: `
- 日文：`相手：`／`AI：`
- 韩文：`상대: `／`AI: `
- 西班牙文：`Llamante: `／`IA: `
- 法文：`Appelant : `／`IA : `
- 德文：`Anrufer: `／`KI: `
- 葡萄牙文：`Chamador: `／`IA: `

旧版保存的记录不论语言都是 `對方：`／`接聽：`。

```json
{
  "dialogue": "对方：Hello, is Alex there?\n接听：Yes, speaking. Who's calling?",
  "dialogue_translations": ["你好，Alex 在吗？", "我就是，请问哪位？"]
}
```

打来的人在你的联系人里时，通话列表会显示名字（`contacts_by_number`），通话详情也有（`contact`），摘要通知的标题会在号码后面加上名字，普通电话的 AI 提示词也会知道是谁打来的。

## 设置

| 方法与路径 | 内容 |
|---|---|
| `GET /calls/api/settings` | `{ok, settings, storage_path, console_url, console_ip_url}`。字段说明见下方。 |
| `PUT /calls/api/settings` | 一个 JSON 对象，只放要修改的字段。响应同 `GET`。下一通电话就生效，不用重启。 |

`GET /calls/api/settings` 响应里的字段：

- `console_url`：当前使用的控制台地址（你填的，或自动带入的）；只有这台电脑能打开时是空的。
- `console_ip_url`：把 `.local` 名称换成局域网 IP 的同一个地址。
- 机密字段以掩码返回（`"abc…wxyz"`），附一个 `<name>_set` 标志。
- 通知渠道返回时 `url`／`token` 是空的，另外附 `url_hint`、`token_hint`、`url_set`、`token_set`。
- 只读的辅助字段：`default_prompt`、`voices`、`ambience_kinds`、`languages`、`countries`（“各 SIM 卡的主要语言”可选的国家代码）、`seen_sim_countries`（Bageo 启动后手机上报过的 SIM 卡国家）、`notify_kinds`、`sample_lines`、`summary_ready`。

值得了解的字段（全部都在 [`settings_store.rs`](../../crates/agent/src/settings_store.rs) 的 `RuntimeSettings` 和[设置参考](SETTINGS.md)里）：

| 字段 | 值 |
|---|---|
| `owner_name` | 你的名字，用在 `{owner}`。 |
| `owner_aliases` | 别人怎么称呼你，最多 10 个，每个最多 40 字。空白的、重复的和跟 `owner_name` 相同的会被去掉。 |
| `answer_style` | `assistant`／`persona`（AI 以你本人身份接听）。 |
| `gemini_assistant_prompt`／`gemini_persona_prompt` | `assistant`／`persona` 各自的自定义提示词（留空 = 内置）。 |
| `errand_identity` | `assistant`（默认）／`owner`：AI 代打时说自己是谁。其他值存为 `assistant`。 |
| `summary_provider` | `gemini`（默认）／`custom`。`custom` 使用 `summary_api_base`、`summary_model`（填了地址就必填）和 `summary_api_key`（那家服务的密钥）。旧版的值和旧的设置名称会在设置加载时转换一次。 |
| `calendar_app` | `google`、`outlook`、`office365`、`apple`、`yahoo`、`ics` 或空（还没选）。其他值存为空。 |
| `appointment_mode` | `off`（默认）／`confirm`。旧的 `auto` 视为 `confirm`。 |
| `appointment_sms` | `true`／`false`：要不要发短信告诉对方你的决定。 |
| `appointment_reply` | `auto`（默认）、`sms` 或 `call`。 |
| `reject_numbers` | 旧版的黑名单写法，`PUT` 仍然接受：`{number, reason}` 的列表。号码会加进黑名单（普通联系人改成黑名单；白名单、陪聊名单上的保持原样），不保存进设置。读回来的是旧版保存的列表，`reject_numbers_moved` 为 `true` 后就不再生效。 |
| `reject_mode` | 黑名单来电怎么拒接：`decline`（默认：响铃时直接拒接）或 `announce`（接起来播放语音）。 |
| `whitelist_mode` | `off`（默认：照常由 AI 接听）、`direct`（直接挂断触发运营商遇忙转移并推送通知）或 `emergency`（AI 接听并在对方有急事时告知 `whitelist_forward_number` 并发送通知）。只对 `transfer_whitelist`、`transfer_contacts`、`transfer_others` 允许的来电生效。 |
| `whitelist_forward_number` | 直接转接或有急事转接时使用的真实电话号码。 |
| `transfer_whitelist`、`transfer_contacts`、`transfer_others` | 哪些人可以转：白名单（默认 `true`）、其他联系人（默认 `false`）、不在联系人里的号码，包括未显示号码（默认 `false`）。黑名单和陪聊名单永远不转。不在联系人里的号码 30 分钟内最多拨给主人一次；判定为推销或诈骗的来电不发急事通知、不拨号。 |
| `ui_password` | 外部访问网页控制台与 API 的密码（读取时掩码；`"ui_password_clear": true` 可清空）。 |
| `country_code` | 只有数字（`86`、`1`）。控制台不显示；这里有值时优先于 `COUNTRY_CODE`。 |
| `gemini_thinking_level` | `low`（默认）／`medium`／`high`，只对 thinking 模型有效。 |

`PUT` 的规则：

- 机密字段（`gemini_api_key`、`summary_api_key`）没带或是 `""` 时，保留原来保存的值；`"<name>_clear": true` 会清空它。
- `notify_channels` 整体替换。一行 `{id, kind, name, enabled, url, token, target}` 的 `url` 或 `token` 为空时，保留 `id` 和 `kind` 相同那一行原来保存的值；没有 `id` 的行是新增的；没列出来的行会被删除。
- 旧版保存的、这一版已经没有的字段，读入时忽略，下次保存就消失：Google 日历凭据（`gcal_*`）、`calendar_auto_add`、`intl_carrier_prompt`，以及其他在 1.0 移除的设置。
- 写入磁盘失败时返回 HTTP 500 `save_failed`。

```bash
curl -s -X PUT "http://127.0.0.1:8787/calls/api/settings" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"language": "ja", "owner_aliases": ["Alex", "小明"], "errand_identity": "owner",
       "reject_numbers": [{"number": "0800*", "reason": "电话营销"}]}'
```

## 通知

| 方法与路径 | 内容 |
|---|---|
| `POST /calls/api/notify/test` | 把测试消息发到那一个渠道（已保存的机密按 `id` 补上）。请求体 `{"channel": {kind, url, token, target, id?}}`。响应见下方。 |
| `POST /calls/api/notify/send` | 请求体 `{"title": "...", "body": "..."}`。把你的消息发到每一个已启用的渠道。可另外带 `"kind": "sms"` 与短信的字段：Bageo 会把短信存进控制台的“短信”页，桌面状态小程序也用它显示短信通知。规则见下方。 |
| `GET /calls/api/notify/recent?after=<id>` | 由旧到新列出发过 `notify/send` 的消息：`{ok, last_id, events: [{id, at_ms, kind, title, from, text}]}`。`events` 只放 `id` 大于 `after` 的；没带 `after` 或不是数字时 `events` 是空的，`last_id` 让客户端知道从哪里开始。只留最近 50 条，只放在内存里，重启就清空。 |
| `GET /calls/api/sms?q=&before=<id>&limit=<n>` | 由新到旧列出存下来的短信：`{ok, total, retention_days, has_more, messages: [{id, at_ms, dir, from, from_name, to, text, serial, device, status}]}`。`q` 搜索发件人、联系人名称与内容；`before` 只列比这个 `id` 旧的；`limit` 是 1 到 200（默认 50）。`total` 是全部短信的数目，不受搜索影响。超过“记录保留几天”的短信会删掉，最多留 5000 条。 |
| `DELETE /calls/api/sms/{id}` | 删除一条短信：`{ok: true}`，找不到返回 HTTP 404 `not_found`。 |
| `POST /calls/api/sms/clear` | 删除全部短信：`{ok: true, removed: N}`。 |
| `GET /calls/api/sms/threads?q=` | 由新到旧列出对话，一个号码一条：`{ok, total, retention_days, threads: [{key, number, name, last_at, last_text, last_dir, last_status, count, last_in_at, can_reply}], lines: [{serial, sub_id, device, number, carrier, slot, can_send}]}`。`0912…` 与 `+886912…` 算同一条对话（号码按收到它的那张 SIM 卡的国家判读）。`q` 搜索名称、号码与每一条内容。发件人是“BankInfo”这种名称时 `can_reply` 是 false。`lines` 是可以用来发短信的 SIM 卡；`can_send` 表示那台手机连着、App 也能发短信。 |
| `GET /calls/api/sms/thread?number=<号码或 key>&before=<id>&limit=<n>` | 由旧到新列出一条对话：`{ok, key, number, name, can_reply, has_more, reply, messages: [{id, at_ms, dir, from, to, text, serial, sub_id, device, status, error}]}`。`dir` 是 `in` 或 `out`；发出的短信 `status` 是 `sending`、`sent`、`failed`（`error` 是手机返回的代码）或 `timeout`。`reply`（`{serial, sub_id}` 或 `null`）是回复默认用的号码：收到最近一条短信的那台手机与 SIM 卡，不能用时改用第一个能发的。还没有对话的号码返回空的对话。 |
| `POST /calls/api/sms/send` | 请求体 `{"to", "text", "serial", "sub_id"}`（`lines` 其中一条；`text` 1 到 500 个字）。让那台手机发出，最多等 30 秒它的回复：`{ok, message}`，发出才是 `ok`。HTTP 400 `bad_number`、`bad_line` 或 `bad_text`。这条短信会留在对话里，不会发到通知渠道。 |
| `POST /calls/api/sms/{id}/retry` | 用同一个号码重发 `failed` 或 `timeout` 的短信，响应同 `send`。其他短信返回 HTTP 409 `not_retryable`。 |
| `DELETE /calls/api/sms/thread?number=<号码或 key>` | 删除一条对话：`{ok: true, removed: N}`。 |
| `GET /calls/api/quick-action` · `POST /calls/api/quick-action` | 32 种通知渠道内附的单次签名互动按钮路由（`?act=confirm\|ignore\|block\|stall\|white&target=<id或号码>&sig=<hmac>`）。打开链接（`GET`）只会先显示确认页，点页面上的按钮（`POST`）才会执行；链接 7 天后失效。通过 HMAC-SHA256 验证，不用登录控制台，支持全部 9 种语言。 |
| `POST /calls/api/control/inbound` | 跨平台通用指令 Webhook，用聊天指令控制 Bageo。请求体例如 `{"text": "记录 5", "channel_id": "line", "sender_id": "owner", "reply": false, "token": "..."}`（也兼容 `user`／`channel`）。 |
| `GET /calls/api/auth-status` · `POST /calls/api/initial-password` | 查询是否已设密码与是否显示首次设置密码弹窗（`{ok, password_set, authenticated, prompt_initial, language}`），以及首次创建或跳过密码（`{"password": "..."}` 或 `{"skip": true}`，仅在尚未设置任何密码／Token 时可用）。 |

`POST /calls/api/notify/test` 的响应：

- 200 `{ok: true}`：测试消息已发出。
- 400 `unknown_channel`。
- 502 `{ok: false, code, field, kind, error}`：`code` 是出错原因，`field` 是 `url`／`token`／`target` 哪一栏（或空白），`error` 是该服务的回复。
- `code` 的值：`missing_field`、`bad_url`、`bad_value`、`auth_failed`、`service_not_found`、`http_error`、`service_error`、`network_error`、`send_failed`、`bot_problem`（Discord 不让机器人连上）、`room_encrypted`（Matrix 房间开了加密）、`blocked_address`（Bageo 不会发送的地址）。

`POST /calls/api/notify/send` 的规则：

- `title` 为空就用“Bageo”；`body` 为空返回 HTTP 400 `empty_body`。
- 只要有一个渠道接受就返回 `{ok: true}`。
- 短信可另外带 `"kind": "sms"`、`"from"`（发件人）、`"text"`（短信内容）、`"to"`（收件号码）、`"serial"` 与 `"device"`（手机）、`"received_ms"`（收到的时间，Unix 毫秒）、`"ref"`（你自己给这条短信的标识）、`"sub_id"`（收到它的 SIM 卡）。Bageo 会把它存进控制台的“短信”页，同一个 `ref` 只存一次；桌面状态小程序用它显示短信通知。旧的请求照常运作。
- [短信转发工具](../../tools/sms_forward/README.md)（英文）就是用这条路由。

`POST /calls/api/control/inbound` 的规则：

- 支持 Slack、Discord、LINE、企业微信、钉钉、飞书、Mattermost、Rocket.Chat、Zulip、Matrix、Nextcloud Talk、Synology Chat、Home Assistant、n8n 或 curl。
- 可执行全部 18 个聊天指令（包含 `block`、`stall`、`white`、`unblock`），并以通知语言返回 `{ok, text, buttons, pushed}`。
- 用 MCP 的 token 调用时，只能做 `mcp_calls` 允许的事（见 [MCP](MCP.md)）。设为 `off`（默认）时，只能查看，不能用指令改任何东西。

你自己的 webhook 会收到的 JSON 和每一种事件，在[通知](NOTIFICATIONS.md#webhook-json)里有说明。

## 语音和背景音试听

`GET /calls/api/preview` 返回一段 WAV：用某个声音说几句电话用语，背后垫着背景音。每个查询参数不带时都使用已保存的设置：

| 参数 | 含义 |
|---|---|
| `voice` | 声音名称，或 `none` 表示只要背景音。 |
| `ambience` | `off` 或某一种背景音。 |
| `level` | 1–3。 |
| `regen=true` | 重新生成，不用已保存的样本。 |
| `clip=reject` | 用这个声音念的拒接语音。 |
| `lang` | `ja`、`zh-TW`… |

内置的 30 个声音在 9 种语言下都已预先录好试听和拒接语音（`assets/voices/` 里共 1,080 段），所以试听不用连接 Gemini。只有 `regen=true` 或没有预录的声音才会开一次连接。

错误：400 `unknown_voice`／`unknown_background`、404 `background_missing`（背景音文件没加载）、502 `preview_failed`。

## 日程预约与任务

有人打来说想约周六下午三点来看房。`appointment_mode` 设为 `confirm` 时（见[设置](SETTINGS.md#摘要与日历)），AI 会记下时间但不先答应。

通话结束后留下一条待处理事项（API 里叫 task，`intent` 为 `appointment`），等你同意或不同意。同一个 `appointment` 对象会出现在通话记录、它的 notes 和待处理事项上：

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
| `start`、`end` | 当地时间（`TZ`）。对方只是顺口提的备选时间不会覆盖谈好的时间；同一通电话另外约的第二件事会成为另一条待处理事项。 |
| `title`、`caller_name` | 约的是什么事、对方怎么自称（对方自己的说法）。 |
| `status` | `pending`（等你决定）、`confirmed`、`declined`；`booked` 只出现在旧版的记录里。 |
| `reply` | 怎么告诉对方：`sms`、`call` 或 `none`。 |
| `sms` | `sent`，或短信没发出的原因。代码见下方。 |
| `call` | AI 回拨的进度，或没拨出的原因。代码见下方。 |
| `call_lang` | 那通电话说的语言（`zh-CN`、`en`…）。AI 回拨就说这个语言，等对方接起来出声后才说你的回复，只说一次。没有这个字段的旧记录按设置 `language`。 |
| `tg_card` | 发给这条预约的 Telegram 卡片 `{chat_id, message_id}`；你在别处决定后，用来去掉卡片上的按钮。 |
| `tag` | 这条预约固定的 id。 |

`sms` 的值：

- `sent`：已发出。
- 没发出的原因：`offline`（手机没连接）、`old_app`（App 不能发短信）、`timeout`、`busy`（同一条短信正在发）、`no_number`（收不到短信的号码）。
- 手机返回的代码：`no_permission`、`bad_request`、`no_service`、`radio_off`、`generic_failure`、`exception`。

网页和 Telegram 回复会把这些写成白话。

`call` 的值：

- `queued`（排在打给同一个号码的前一通回拨后面）、`dialing`、`connected`（手机已拨号，AI 等对方接起来出声）。
- `answered`（对方已得知结果）、`no_answer`（70 秒内没人出声）、`failed`、`done`（通话结束、结果不明）。
- 没拨出的原因：`no_phone`、`send_failed`、`no_number`、`not_callable`（国外、免费电话、增值付费或特殊短号）。
- 手机的上报也记在待处理事项的 `events`（`callback_place_call_ack`、`callback_place_call_failed`、`callback_outbound_ready`、`callback_done`、`callback_failed`）。

空的字段不会出现。

| 方法与路径 | 内容 |
|---|---|
| `GET /calls/api/tasks` | 待处理事项，最新的在前（最多 200 条）：`{ok, tasks}`。内容见下方。 |
| `GET /calls/api/tasks/{id}/event.ics` | 预约的 iCalendar 文件（Apple 日历和其他 App）。用控制台密码或链接里的 `?k=<HMAC 密钥>` 打开；这条待处理事项没有预约时返回 HTTP 404 `no_calendar`。 |
| `POST /calls/api/tasks/{id}/confirm` | **同意**，跟在网页或 Telegram 卡片上点一样。请求体：空的、`{}` 或 `{"reply": "sms" \| "call" \| "none"}`。响应 `{ok, task, message}`。 |
| `POST /calls/api/tasks/{id}/ignore` | **不同意**：用同样的方式告诉对方，礼貌婉拒。请求体、响应和规则同 `…/confirm`。 |
| `POST /calls/api/appointments/{id}/retry-sms` | 已决定的预约短信没发出时再发一次。请求体：空的、`{}` 或 `{"serial": "<手机>"}`。响应 `{ok, sms, message}`。 |

`GET /calls/api/tasks` 的内容：

- 预约的待处理事项另外带 `calendar_links`（`{google, outlook, office365, yahoo}`）。
- `reply` = `{sms, call, default}`：这位来电者能不能收短信、能不能回拨，以及“默认回复方式”会选哪个。
- `sms_route` = `{serial, label}`：你没另选手机时短信从哪部发（接这通电话的手机）；`label` 是它在 `device_labels` 里的名称，没有就是 id。
- 来电者是联系人时还有 `contact_name`。
- 网页上的“待处理事项”列的就是 `appointment.status` 为 `pending` 的项目。

`POST /calls/api/tasks/{id}/confirm` 的内容：

- 没带 `reply` 时按“默认回复方式”；选了这位来电者用不了的方式就不通知。
- `message` 是用白话写的结果，跟 Telegram Bot 回的同一段，附上你选的 `calendar_app` 的“添加到日历”链接。
- 还没选 `calendar_app` 时，附的是这通电话在网页里的页面，在那里点“添加到日历”选一家；没有控制台地址就不附。
- 发短信最多可能等 90 秒；回拨交给手机拨出（在 `appointment.call` 看进度）。

`POST /calls/api/appointments/{id}/retry-sms` 的内容：

- `serial` 可以换另一部已连接的手机；没带就是接这通电话的那部。
- 发出去了 `ok` 才是 true，`sms` 是新的代码。
- 还在等、已经发出或本来就不能发（`no_number`）时，返回 HTTP 409 `sms_not_retryable`（附当前的 `sms`）。
- 其他错误：404 `task_not_found`；400 `invalid_serial`／`bad_request`。

决定的规则：

- **只有第一次决定算数。** 第二次同意或不同意，包括第一次还在处理时发来的、或在 Telegram 卡片上点的，都会返回“已经处理过了”，什么都不改。
- 这条预约发过 Telegram 卡片时，卡片上的按钮会被去掉，并把 `message` 发到那个聊天里（尽力而为；不论成功与否，决定都有效）。
- 短信和回拨都从接到那通电话的 SIM 卡（`sub_id`）发出。短信只写时间和星期几，不写约的是什么事。
- 推销和诈骗电话不会留下预约；事后才判定为这类电话的，预约会被去掉。

```bash
curl -s -X POST "http://127.0.0.1:8787/calls/api/tasks/$TASK/confirm" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"reply": "call"}'
```

```json
{"ok": true, "task": {"id": "…", "appointment": {"status": "confirmed", "reply": "call", "call": "dialing"}},
 "message": "已同意预约\n11月14日（周六）15:00–16:00 看房\nAI 正在回拨给 0912345678\n添加到日历：https://calendar.google.com/…"}
```

## AI 代打

AI 代打就是 AI 替你打出去的电话。给它一个号码（或一位联系人）和要办的事：订位、跟朋友约时间、问商家一件事。一部 AI 模式的手机拨出去，AI 负责说，你拿结果。用 API 订一家餐厅的流程大致是这样：

1. `POST /calls/api/errands`，带上号码和“订明天晚上 7 点 4 位，名字写王先生。7 点半到 8 点也可以。”代打状态变成 `dialing`。
2. 每隔几秒读一次 `GET …/errands/{id}/live` 看实时字幕。餐厅说 7 点满了，问 7 点半还是 8 点 15。
3. 明显在你交代范围内的选择，AI 会自己答应；拿不准时它会跟对方说要确认一下，实时画面上出现一个带选项的 `question`。
4. 用 `POST …/errands/{id}/say` 回答，或打一句指示（“跟他说 8 点也可以”）。
5. 通话结束后代打变成 `done` 并附上 `result`，你会收到一条通知。

**通话中。**

- AI 遇到你的交代没说清楚的事，会跟对方说要确认一下，然后问你（`ask_owner` 函数）。
- 你正在看这条代打时（6 秒内有人读过它的实时画面），问题会显示在那里，最多等 40 秒。
- 没人在看、或你没来得及回答，AI 会问清楚哪些选项可以，说你确认后再回电，然后礼貌地结束通话。
- 等答案的时候，AI 不会先替这个问题做决定；对方先说再见的话，问题就到此为止。
- 你在通话中输入的文字会当作指示交给 AI。

**通话后。** 摘要模型会对照交代内容读这通电话，你会收到一条通知（标题“AI 代打电话：<名字或号码>”，用通知语言；webhook 事件 `ai_errand`）。AI 在你没回答的问题上自己答应的事，结果一定不是 `done`，而是 `need_owner`，提醒你确认，不合适就回电修改。

**用谁的身份打。** 设置 `errand_identity`（默认 `assistant`，你的助理；或 `owner`，你本人），每条代打也可以单独指定。选 `assistant` 时，被认真问到会表明是 AI 助理；不论哪一种，都不透露交代内容以外的个人信息，也不答应任何付款、订金或转账。

**能打哪些号码。**

- 不打紧急号码、短号（7 位以下）或增值付费号码（例如台湾 020x 如 0204、0209，+1 900／976，日本 0990，英国 09…）；免费电话可以打。
- 没有加号的号码当作你所在国家的号码（`country_code`）。
- 以国际冠码开头的号码（`00`、台湾的 `01x` 和 `00x`、北美的 `011`、日本的 `010`）一律 `not_allowed`：国外号码请用 `+` 写。
- 台湾号码长途冠码 0 后面接 1 的也是 `not_allowed`。

**用哪部手机、哪张卡。**

- 电话从一部已连接、在 AI 模式、而且有该号码所属国家 SIM 卡的手机拨出。
- Bageo 从手机控制连接 `hello`／`status` 上报的 `sims` 列表学会每部手机有哪些 SIM（subscription id、国家、运营商与号码；旧版 App 则来自 `incoming` 和 media `hello` 里的 `sim_sub_id`、`sim_country`）。
- 这些记在 `phone_sims.json`。
- Bageo 从没听说过 SIM 的手机不接代打（`sim_unknown`）：重新连接那部手机（旧版 App 则先打一通电话给它）。
- 手机收到的是 E.164 格式的号码和要用的 SIM（`sub_id`）。
- 那通电话自己的 media `hello` 显示用的是另一张 SIM、或没上报时，Bageo 会在 AI 开口前挂断（`failed`、`call_code` `wrong_sim`，会通知你），并把那张卡从这部手机的记录里去掉，等之后有来电上报它再加回来。

**排队与上限。**

| 情况 | 会怎样 |
|---|---|
| 手机正在通话 | 代打排队（`queued`），超过 10 分钟算失败（`phone_busy`）。 |
| 同一个号码还有另一通 | 一个号码一次只打一通，跟日程预约的 AI 回拨共用排队。排在后面超过 25 分钟算失败（`number_busy`）。 |
| 通话时长 | 最长 10 分钟；9 分半开始收尾。 |
| 语音菜单 | AI 最多等 90 秒等真人接听；它不能按键（`keypad_menu`）。 |
| 运营商录音（“您拨打的号码是空号”） | 直接挂断（`carrier_message`）。 |
| 没人接或失败 | 不会自动重拨：要不要“再打一次”由你决定。打开 `redial` 见下一行。 |
| “拨到通”（`redial`） | 没人接、占线、对方关机或进语音信箱（不留言）：代打排队（`queued`，`call_code` 是 `redial_wait`，时间在 `redial_at`），一分钟后再拨，总共最多 30 次。有人接起来、拨满 30 次（`no_answer`）、号码不通、语音菜单或手机出问题就停。只在最后通知一次。 |
| 拨号中你点了取消 | 手机和号码立刻释放给下一通；万一电话还是接通了，会在 AI 开口前挂断。 |

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
| `task` | 你自己写的交代内容（最多 1,000 字）。 |
| `identity` | `assistant` 或 `owner`。 |
| `serial`、`device_label` | 拨出的手机和它的名称。 |
| `status` | `queued`、`dialing`（电话在响，还没有人出声）、`on_call`（对方接起来出声了）、`done`、`no_answer`、`failed`、`cancelled`。后四个是最终状态。 |
| `call_code` | 拨号进行到哪一步。值见下方。 |
| `attempts` | 从 1 开始，每点一次“再打一次”或每重拨一次加 1。 |
| `redial` | 打开了“拨到通”。没打开就不出现。 |
| `redial_at` | 下一次什么时候拨（`call_code` 是 `redial_wait` 时）。其他时候不出现。 |
| `redial_start` | 这一轮重拨从第几次开始（点“再打一次”会重新算 30 次）。是 0 就不出现。 |
| `record_id` | 这一次的通话记录（`GET /calls/api/call/{id}`：文字记录和录音）。 |
| `follow_up_of` | 这通是接着哪一条之前的代打；那一条最后 20 行文字记录会放进 AI 的脚本。 |
| `questions` | `[{id, text, options, answer, answered_by, asked_at}]`：AI 问过你的问题，选项最多 4 个。 |
| `result` | `null` 或 `{outcome, summary, details, follow_up, options, event, unconfirmed}`。字段见下方。 |
| `history` | 之前几次的尝试，`[{attempt, status, result, record_id, ended_at, questions}]`。 |

`call_code` 的值：`dialing`、`queued`、`answered`、`no_answer`、`failed`、`phone_busy`、`number_busy`、`no_phone`、`send_failed`、`keypad_menu`、`carrier_message`、`wrong_sim`、`agent_restarted`、`cancelled`、`redial_wait`、`voicemail`。

`questions` 里的 `answered_by`：`owner`（你回答了）、`timeout`（你在看但没来得及回答）、`absent`（没人在看）；还在等时是 `null`。

`result` 的字段：

- `outcome`：`done`、`partial`、`not_done`、`need_owner`（选项在 `options`，最多 8 个）、`no_answer`、`voicemail`、`failed`。
- `event`：`null` 或 `{start, end, title}`（带时差的当地时间；没谈定结束时间就没有 `end`）。
- `unconfirmed`：`details` 或 `event` 里 AI 复述时对方没有明确确认的时间、日期、号码或名字（最多 8 项）；没有就不出现。

代打保存在数据文件夹的 `errands.json`（保留最新 500 条）。Bageo 重启时还在进行中的代打会变成 `failed`（`call_code` `agent_restarted`）；打开“拨到通”的会接着拨，除非对方已经接起来过。

### 代打路由

| 方法与路径 | 内容 |
|---|---|
| `GET /calls/api/errands` | `{ok, errands}`，最新的在前，最多 200 条。 |
| `POST /calls/api/errands` | 开始一条。请求体 `{number, name, contact_id, task, identity, serial, follow_up_of, answers, redial}`（`redial` 是 `true` 就打开“拨到通”，默认 `false`），响应 `{ok, errand}`。 |
| `POST /calls/api/errands/preflight` | 拨出前看这条代打还缺什么。请求体 `{task, lang}`，响应 `{ok, kind, missing, ladder, source, ms}`。不会拨号。内容见下方。 |
| `POST /calls/api/errands/{id}/cancel` | 排队中：取消。拨号中或通话中：挂断。已经结束的原样返回。`{ok, errand}`。 |
| `POST /calls/api/errands/{id}/retry` | 同一条再打一次（`attempts` 加 1，上一次移到 `history`）。`{ok, errand}`。还在进行中（或上一通还没结束）：400 `busy`；手机相关的代码同新建。 |
| `DELETE /calls/api/errands/{id}` | 从列表删除（通话记录会保留）。`{ok}`；拨号中或通话中：400 `busy`。 |
| `GET /calls/api/errands/{id}/live?after=<seq>` | 实时字幕和等你回答的问题：`{status, lines, question, watching}`。内容见下方。 |
| `POST /calls/api/errands/{id}/say` | 请求体 `{text, question_id}`（最多 300 字）。带 `question_id`：回答那个问题。不带：有问题在等就作为它的答案，否则作为给 AI 的指示（“跟他说 8 点也可以”）。`{ok}`。 |

`POST /calls/api/errands` 的内容：

- 必填：`task`，以及号码（或给联系人、之前的代打，用它们的号码）。
- `identity` 留空就按 `errand_identity`；`serial` 留空就用任何合适的手机。
- 响应里的状态是 `dialing`，手机或号码忙线时是 `queued`。
- `answers` 可以不带：你回答补问后的内容，格式 `{key: 文字}`。key 就是 `preflight` 返回的那些，另有 `note` 放自由文字；每项最多 80 字（`note` 120 字），不认识的 key 会被丢掉。它们会以一小段文字放进这通电话的脚本交给 AI。

`POST /calls/api/errands/preflight` 的内容：

- 发送 `task`（必填，最多 1,000 字）和 `lang`（可以不带，例如 `en`、`zh-TW`：补问用的语言；留空就用通知语言）。
- `kind`：`restaurant`、`appointment`、`order`、`change` 或 `contact`。
- `missing`：最多 3 条 `{key, question}`，最重要的在前；什么都不缺就是空的。各类型的 key：`restaurant` `time`、`party`、`phone`、`name`；`appointment` `time`、`item`、`contact`、`backup`；`order` `spec`、`pickup`、`substitute`；`change` `orig_time`、`contact`；`contact` 没有。
- `ladder`：你愿意接受的时间，最好的在前（`["19:30", "20:30"]`），最多 4 个；交办里没有写到两个以上的时间就是空的。
- `source` 和 `ms`：`local`、`model` 或 `timeout`，以及花了多少毫秒。
- Bageo 自己读得懂的交办，几毫秒内就在本机回答。读不懂的（别种文字、夹很多数字的长文）会用 Gemini 密钥问一个小模型（`gemini-flash-lite-latest`），最多等 0.9 秒；时间内没答（`source` 为 `timeout`）或没有密钥，就不补问、照常拨出。
- 在聊天室，确认卡会列出同样的问题。直接回复那张卡就是补答，你的回复会存成 `answers.note`，AI 带着它拨出；回“取消”就不拨。

`GET /calls/api/errands/{id}/live?after=<seq>` 的内容：

- `lines`：`seq` 之后的字幕，`[{seq, who, text, at}]`；`who` 是 `ai`、`callee`、`owner` 或 `system`（保留最新 400 行；1.2 秒没有新字就算一行结束）。
- `question`：`null` 或 `{id, text, options, asked_at, expires_at}`，等你回答的问题。
- `watching`：你是否算正在看。每读一次就算你正在看 6 秒，所以跟着通话时每隔几秒读一次。

错误代码（HTTP 400 `{ok: false, error, code}`，两者相同）：

| 代码 | 路由 | 含义 |
|---|---|---|
| `no_number` | 新建 | 没有号码，也没有可以取号码的联系人或之前的代打。 |
| `bad_number` | 新建 | 不是电话号码（有字母、`+` 不在开头、超过 15 位、对那个国家来说太短）。 |
| `not_allowed` | 新建 | 紧急号码、短号、增值付费号码，或用国际冠码代替 `+`。 |
| `no_task` | 新建、补问 | `task` 为空。 |
| `task_too_long` | 新建、补问 | `task` 超过 1,000 字。 |
| `no_phone` | 新建、再打 | 没有已连接、处于 AI 模式的手机（或指定的 `serial` 不是这样的手机）。 |
| `sim_unknown` | 新建、再打 | 没有合适的手机，而且至少有一部的 SIM Bageo 还不知道：先给它打一通。 |
| `no_sim_for_country` | 新建、再打 | 没有手机有该号码所属国家的 SIM。 |
| `not_found` | 新建 | `contact_id` 或 `follow_up_of` 不存在（这里返回 400）。 |
| `busy` | 再打、删除 | 这条还在拨号或通话中。 |
| `not_on_call` | say | 电话不在通话中（或已经结束）。 |
| `empty_text`／`text_too_long` | say | 没有内容／超过 300 字。 |
| `question_closed` | say | 那个问题已经回答、超时，或不是正在等的那一个。 |
| `bad_request` | 所有 `POST` | 没带 `Content-Type: application/json`，或请求体不是 JSON 对象。 |

所有 `…/errands/{id}…` 路径，代打 id 不存在或格式不对时一律返回 HTTP 404 `not_found`。

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

联系人给打来的号码配上名字，保存在通话记录文件夹（`CALLS_DATA_DIR`）的 `contacts.json`。

号码不管怎么写都能对上：`+886912345678`、`0912-345-678`、`886912345678` 是同一个号码。没有国码的号码按你的国家（`country_code`）解读；来电则按接到这通电话的 SIM 卡所属国家。

联系人也用在 AI 代打（选联系人就不用输入号码）和推销判断：已保存的联系人说到钱，不会被当成推销或诈骗。

一位联系人是 `{id, name, phones: [...], note, kind, source, updated_at}`。

- 最多 10 个号码，名字最多 80 字、备注最多 500 字。
- `kind`：`contact`（默认，一般联系人）、`whitelist`（白名单，家人或重要的人，配合 `whitelist_mode` 使用）、`blacklist`（黑名单，来电直接拒接）或 `stall`（陪聊名单，来电直接进入 AI 陪聊反制模式）。
- `source`：`manual`、`vcf`、`csv`、`text` 或 `ai`（Gemini 读出来的）。
- 只有黑名单的号码可以在结尾加 `*`（`0800*`；开头的 0 不算，至少三位，所以 `080*`、`0000*` 不行），拦下同一个开头的所有号码；其他名单用 `*` 会返回 400 `invalid_contact`，`field` 是 `prefix`。
- 一个号码只属于一位联系人；通讯录最多 10,000 位（`too_many_contacts`）。

| 方法与路径 | 内容 |
|---|---|
| `GET /calls/api/contacts` | 查询参数 `q`（名字、备注或号码，号码怎么写都行）、`kind`（`contact` \| `whitelist` \| `blacklist` \| `stall`）、`limit`、`offset`。响应 `{ok, contacts, total, offset, count}`：符合的结果（共 `total` 条），以及全部联系人数（`count`）。 |
| `POST /calls/api/contacts` | 请求体 `{name, phones: [...], note, kind?}`，响应 `{ok, contact}`。错误见下方。 |
| `POST /calls/api/contacts/block` | 请求体 `{number, reason?}`：把号码（或结尾 `*` 的开头）加进黑名单。已经是某位联系人的就把他改成黑名单（没有备注才补上 `reason`），否则新增一位，名字用 `reason`，没有就用号码。响应 `{ok, contact, already}`；`already` 表示本来就在黑名单。 |
| `PUT /calls/api/contacts/{id}` | 同样的请求体，替换名字、号码、备注和名单分类 `kind`。404 `contact_not_found`，409、400 同上。 |
| `DELETE /calls/api/contacts/{id}` | `{ok}`。 |
| `POST /calls/api/contacts/import` | 请求体就是文件本身（最大 10 MB），文件名做 URL 编码后放在 `X-Filename`。只读、**不保存**（预览）。内容见下方。 |
| `POST /calls/api/contacts/batch` | 请求体 `{contacts: [{name, phones, note, source}], duplicates: "merge" \| "skip"}`：导入时你要保留的那些。跟已保存联系人重复的，合并进去（补上新号码和备注；默认）或跳过。响应 `{ok, added, merged, skipped, count}`。 |
| `GET /calls/api/contacts/export.vcf` | 全部联系人导出为 vCard 3.0 文件。 |

`POST /calls/api/contacts` 的错误：

- 号码已经是别的联系人的：HTTP 409 `contact_exists`，附 `contact: {id, name}`。
- 没有名字或没有号码：400 `invalid_contact`，附出问题的 `field`。

`POST /calls/api/contacts/import` 的内容：

- 没带 `X-Filename` 返回 400 `bad_request`。
- 预览响应：`{ok, file, format, ai, entries: [{name, phones, note, source, duplicate}], skipped}`。
- `format`：`vcf`、`csv`、`text`、`xlsx`、`docx`、`pdf` 或 `image`。`ai`：是 Gemini 读的。
- `duplicate`：`null`，或这条重复的已保存联系人 `{id, name, by: "phone" | "name"}`。
- `skipped`：没有名字或号码而跳过的条数。
- 错误：400 `empty_file`、413 `file_too_large`／`too_many_rows`、415 `unsupported_file`、400 `need_gemini_key`、502 `ai_failed`（Gemini 的回复在 `error`）。

Bageo 自己能读懂的格式：

- vCard 2.1、3.0、4.0（含 quoted-printable、各种 `CHARSET`）。
- 有表头（Google 通讯录、Outlook，或列名像 *姓名*、*电话*、Name、Phone）或没有表头的 CSV、TSV，编码 UTF-8、UTF-16 或电脑的编码（Big5、GBK、Shift_JIS、EUC-KR、Windows-1252）。
- 一行一个名字加号码的纯文本。
- `.xlsx`、`.docx` 里的表格。

PDF、图片（打印出来的名单照片、通讯录截图）和读不出人的文件，交给 Gemini 的视觉模型（`gemini-flash-lite-latest`，使用设置里的 Gemini 密钥）。

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

## 备份与还原

十二个路由用来备份和还原 Bageo 自己的数据，对应控制台设置里的“备份与还原”（见[设置参考](SETTINGS.md#备份与还原)）。任何数据都不会发送到你的电脑以外。它们遵循控制台的密码规则，每个 `POST` 也要遵循上面的 `Content-Type: application/json` 规则（`import` 跟 `contacts/import` 一样，body 是文件本身，另外带 `X-Filename` 标头）。`GET …/download/{ticket}` 只要下载码，下载网址里不会出现任何密码。

四个设置用 `PUT /calls/api/settings` 保存：`backup_schedule`（`off`、`daily`、默认的 `weekly`，或 `monthly`）、`backup_dir`（留空：数据文件夹下面的 `backups`；否则要填绝对路径，Bageo 会建好文件夹并试写一个文件，写不进去就返回 HTTP 400 `backup_dir_unwritable`）、`backup_keep`（1 到 100，默认 10，只算自动备份）、`backup_include_calls`（默认 `false`；`true` 时自动和手动备份也会放进通话记录、录音、短信和待处理事项）。另外两个是加密：`backup_encrypt`（默认 `false`；`true` 时手动、自动和 `before_import` 备份都加密）、`backup_password`（至少 8 个字符，否则 HTTP 400 `backup_password_short`；不会返回，只返回 `backup_password_set`；`backup_password_clear: true` 会删掉）。没有密码却开 `backup_encrypt`，或加密开着时删掉密码，返回 HTTP 400 `backup_password_missing`。密码跟其他设置一起存在这台电脑，自动备份才用得到；加密保护的是放到别处的那份，例如移动硬盘、云端或网络文件夹。它属于 `keys`。

| 方法与路径 | 内容 |
|---|---|
| `GET /calls/api/backup` | 当前状态：`{dir, default_dir, dir_ok, can_open_folder, can_download, running, last, next_at, backups}`。 |
| `POST /calls/api/backup/run` | Body `{}`。马上做一份手动备份：`{entry}`，是 `backups` 里的一条。已经有备份在跑：HTTP 409 `busy`。失败：HTTP 500 `dir_unwritable`、`disk_full` 或 `io`。 |
| `POST /calls/api/backup/export` | Body `{items, password}`。做一个文件，内容是 `items` 指定的项目：`settings`、`keys`、`contacts`、`numbers`、`phones`、`errands`、`calls`、`sms`、`tasks`。没带 `items` 就是 `settings`、`keys`、`contacts`、`numbers`、`phones`、`errands`。有不认识的 id 或列表是空的：HTTP 400 `bad_items`。有 `password`（至少 8 个字符，否则 HTTP 400 `backup_password_short`）就加密成 `.bageo`，没有就是 zip。返回 `{ticket, name}`，再用 `GET …/download/{ticket}` 下载。含 `calls` 时文件可能很大。含 `keys` 又没设控制台密码时，只有在 Bageo 所在的这台电脑用 `localhost` 打开才行，否则 HTTP 403 `need_password`。 |
| `POST /calls/api/backup/file-ticket` | Body `{name}`。拿备份文件夹里某个文件的下载码：`{ticket, name}`。`name` 必须是文件夹里真的有的备份文件名（否则 HTTP 404 `not_found`）。备份文件里有密钥：没设控制台密码时，规则跟含 `keys` 的导出一样（HTTP 403 `need_password`）。 |
| `GET /calls/api/backup/download/{ticket}` | 下载某个下载码的文件。下载码是 256 位的随机数，2 分钟内有效、只能用一次，之后返回 HTTP 404 `not_found`。导出的文件发完或下载码过期就删掉。 |
| `POST /calls/api/backup/import` | Body 是文件本身（最大 200 MB），文件名 URL 编码后放 `X-Filename`。可以是备份 zip、加密的备份（`.bageo`），或旧版单独的 `settings.json`。**不会还原任何东西**：文件暂存 15 分钟，返回预览。加密的文件只返回 `{token, source_name, encrypted: true, needs_password: true}`，没有 `items`，要先用 `unlock` 解开。最多 3 份预览等着：再来一份就丢掉最旧的（它的 token 之后返回 `expired`）。同时最多 2 个上传在跑，再多返回 HTTP 409 `busy`。 |
| `POST /calls/api/backup/restore` | Body `{name}`。对备份文件夹里的文件做同样的预览（没有 200 MB 的上传上限）。加密的文件会先用这台存的备份密码试，打得开就直接返回预览，打不开就跟导入一样返回 `needs_password`。 |
| `POST /calls/api/backup/delete` | Body `{name}`。删掉备份文件夹里这个备份文件，返回 `{ok}`，删了不能恢复。`name` 只能是备份文件名（`.zip` 或 `.bageo`），带路径返回 HTTP 400 `bad_request`；不是文件夹里的普通文件（不存在、符号链接）返回 HTTP 404 `not_found`；正在备份或还原时返回 HTTP 409 `busy`。没设控制台密码时，规则跟 `file-ticket` 一样（HTTP 403 `need_password`）。 |
| `POST /calls/api/backup/unlock` | Body `{token, password}`。用密码解开预览里的加密文件，返回完整的预览。密码不对：HTTP 400 `wrong_password`。一次只试一个，每次至少隔一秒；不管是哪份预览，连续错 5 次以后每次等待加倍，最长 60 秒。同一份预览错 5 次就丢掉（HTTP 400 `expired`）。没设控制台密码时只能在运行 Bageo 的这台电脑上解（HTTP 403 `need_password`）。 |
| `POST /calls/api/backup/apply` | Body `{token, items}`。`items` 必须是预览 `items` 的子集、不能是空的，否则 HTTP 400 `bad_items`。只还原这些项目。`{ok, before_import, restart_needed, warnings}`。 |
| `POST /calls/api/backup/discard` | Body `{token}`。丢掉不还原的预览和它的上传文件：`{ok}`（token 不存在或已经没了也一样）。控制台关掉还原窗口时会调用。 |
| `POST /calls/api/backup/open-folder` | Body `{}`。用文件资源管理器或 Finder 打开备份文件夹。只有从 Bageo 所在的这台电脑连接、而且是 Windows 或 macOS 才行，否则 HTTP 403 `not_local`。 |

细节：

- `GET /calls/api/backup`：`dir` 是正在用的文件夹，`default_dir` 是默认文件夹，`dir_ok` 是写不写得进去，`can_open_folder` 是 `open-folder` 能不能用，`can_download` 是在这里能不能下载含密钥的文件，`running` 是不是正在备份，`next_at` 是下一次自动备份的时间（RFC 3339，或 `null`）。`last` 是最近一次自动或手动备份的 `{at, ok, name, size, error}`，没有就是 `null`。`backups` 是文件夹里的文件，新到旧，每条 `{name, at, size, kind, app_version, include_calls, encrypted}`；`kind` 是 `scheduled`、`manual`、`before_import` 或 `export`。加密的文件没有密码什么都读不到，所以 `app_version` 和 `include_calls` 是 `null`。
- 备份文件是 zip。根目录的 `bageo-backup.json` 放 `{format: "bageo-backup", version: 1, app_version, created_at, kind, include_calls, items, files: [{path, size, sha256}]}`，`items` 是文件里真的有的项目 id。其他文件都在 `data/` 下面，路径是数据文件夹里的相对路径（`data/config/settings.json`）。不放的有：`backups` 文件夹本身、临时文件、调试文件、重新启动标记这类一次性的文件，以及缓存。自动、手动和 `before_import` 备份一律含 `settings`、`keys`、`contacts`、`numbers`、`phones`、`errands`，`backup_include_calls` 为 `true` 时再加 `calls`、`sms`、`tasks`。导出只放勾选的项目：选了 `settings` 没选 `keys`，`settings.json` 不含整组 `keys`；选了 `keys` 没选 `settings`，只放这一组和手机配对密钥（`phone-pairing.key`）。`keys` 是密钥、密码和连接：Gemini 和其他服务的密钥、通知和聊天机器人的令牌、控制台密码、MCP token，以及所有决定数据发到哪里、谁可以远程控制 Bageo 的设置（整个通知渠道，含发送对象和谁可以下指令；Telegram 聊天和远程控制；通知网关；发信账号；摘要服务；TRTC 桥接和端点；转接号码和急事要不要直接转过去；Bageo 可以回拨的号码；MCP 访问），再加上手机配对密钥。完整列表是 `crates/agent/src/settings_store.rs` 的 `CREDENTIAL_FIELDS` 和 `CONNECTION_FIELDS`。
- 文件名用本机时间：`bageo-backup-YYYYMMDD-HHMMSS-auto.zip`、`…-manual.zip`、`…-before-import.zip`，导出的是 `bageo-export-YYYYMMDD-HHMMSS.zip`（`kind` 是 `export`，跟其他文件一起列出，Bageo 不会自动删）。加密的文件结尾是 `.bageo`，不是 `.zip`。
- 加密的文件（`.bageo`）就是同一个 zip 加密后的样子，有密码就能用一般工具自己解开：
  - 文件头 62 字节（整数都是 little-endian）：`BAGEOENC`（8 字节）；格式版本 `1`（1）；密钥推导 `1` = Argon2id、版本 0x13（1）；内存 KiB（4，65536 = 64 MiB）；次数（4，3）；并行数（4，1）；salt（16，随机）；加密法 `1` = XChaCha20-Poly1305 的 STREAM 做法（1）；每块大小（4，1048576）；nonce 前缀（19，随机）。
  - 密钥：用 Argon2id 从密码（UTF-8，去掉前后空白）和 salt 算出 32 字节。
  - 接着是 zip 一块一块加密：每块是每块大小那么多字节（最后一块比较短，也可能是空的），用 XChaCha20-Poly1305 加密，后面接 16 字节的 tag。每块的 24 字节 nonce 是前缀、这块从 0 起算的编号（4 字节，big-endian）、再一个字节：最后一块是 1，其他是 0。整个文件头是每一块的 associated data。
  - 要求超过 256 MiB 内存、超过 4 次或超过 4 个并行数的文件，Bageo 直接拒绝；任何一块被改、被截断或少了，都返回 `damaged`（第一块的话是 `wrong_password`）。
- 预览是 `{token, source_name, created_at, app_version, kind, items, has_calls, calls_count, changes_password, changes_connections, total_size, warnings, encrypted, needs_password}`。`items` 的每条是 `{id}` 或 `{id, count}`，列出文件里真的有的项目：`settings`、`keys`、`contacts`、`numbers`、`phones`、`errands`、`calls`、`sms`、`tasks`。旧版单独的设置文件有密钥或密码字段时，也会列出 `keys`。只有还原 `keys` 会换掉控制台密码（换成别的或变成没有）时，`changes_password` 才是 `true`；会改变通知发到哪里、转接号码或谁可以远程控制时，`changes_connections` 才是 `true`。两个都是拿这台现在的跟还原后的结果比（算法跟 `apply` 一样）。`total_size` 是可以还原的文件解开后的大小（字节）。`warnings` 可能有 `newer_version`、`plain_settings_file`（旧版单独的设置文件：只还原设置，有密钥的话一起还原）。预览不会去碰文件里写的备份文件夹。
- `apply` 只还原勾选的 `items`。选 `settings` 没选 `keys`：换掉其他设置，这台电脑整组 `keys` 都保留。选了 `keys`：文件里有值的字段换掉这台的，文件里没有或空的字段保留这台的。只有从备份文件夹还原这台自己的 `before_import` 备份时，整组完全按文件（没有或空的字段一律清空或回到默认），所以能把导入带进来的密钥和连接全部拿掉；上传的文件一律不走这条。选 `keys` 没选 `settings`：只换这一组，文件里有手机配对密钥才换。文件里没有 `keys`，或没选 `keys`，这台的整组都不会变。`retention_days` 和 `backup_keep` 只会保持或变长：文件的值比较大才采用，文件没写就保留这台的。`backup_dir` 和 `public_url` 一律保持这台电脑的设置。还原的通话记录和待处理事项，只保留这次真的还原了的录音路径（`YYYY-MM-DD/<name>`），其他路径一律清空。解开之前 Bageo 会先确认硬盘空间够（不够返回 HTTP 500 `disk_full`）。`calls`、`sms`、`tasks` 是加进现有的，不会删掉任何东西。动手之前 Bageo 会先把现在的设置做成一份 `before_import` 备份（选的文件夹写不进去就放默认文件夹），文件名放在 `before_import`。`restart_needed` 表示有些变更要重新打开 Bageo 才会全部生效。
- 预览路由的错误（HTTP 400）：`not_backup`、`damaged`、`too_new`（来自比较新的 Bageo）、`too_large`。上限看文件实际是什么，不看 manifest 怎么说：`calls`、`sms`、`tasks` 以外的项目每个文件解开最多 20 MB、合计 50 MB，通话记录合计最多 8 GB；解开后比压缩时大 200 倍以上的文件算 `damaged`。已经有 2 个上传在跑：HTTP 409 `busy`。`unlock` 密码不对返回 HTTP 400 `wrong_password`，错 5 次返回 `expired`。`apply` 在通话中返回 HTTP 409 `call_active`；`items` 是空的或有预览没列的 id，返回 HTTP 400 `bad_items`；token 超过 15 分钟或已经用过，返回 HTTP 400 `expired`。

## 手机与健康状态

| 方法与路径 | 内容 |
|---|---|
| `GET /calls/api/devices` | 手机列表和连接状态。响应格式见下方。 |
| `POST /calls/api/devices/{serial}/mode` | 请求体 `{"mode": "ai" \| "normal"}`。让手机切换，最多等 15 秒等它回复。返回 `{ok, mode, error_code}`（400 时另附说明 `error`）。 |
| `DELETE /calls/api/devices/{serial}` | 移除一台未连接的旧手机（同时清除手机列表、已存名称与 SIM 记录，保留过往通话记录）：`{ok, serial}`。手机仍在线或通话中返回 HTTP 409 `device_online`／`device_in_call`。 |
| `PUT /calls/api/devices/{serial}/sim-number` | 请求体 `{"sub_id": <整数>, "number": "..."}`。设置（留空表示清除）这部手机某张 SIM 的号码，让来电与 AI 代打直接显示并按号码挑选：`{ok, serial, sims}`。 |
| `POST /calls/api/phone-pairing/renew` | 更换 Wi-Fi 配对密钥（`phone-pairing.key`）并断开通过 Wi-Fi 连接的手机，每部手机重新插上 USB 一次即可获取新密钥：`{ok: true}`（失败返回 HTTP 500 `save_failed`）。密钥本身不会返回。 |
| `GET /health` | 排查问题用的大型 JSON 对象：版本、语言、哪些东西已配置、已连接的手机、进行中的通话；不含密钥或密码。要带密码才拿得到（见下方说明最后一点）。 |
| `GET /calls/api/health` | 控制台用，内容和 `/health` 的完整版相同。跟其他 `/calls/api/` 路由一样：设置了密码而没带返回 HTTP 401，没设密码就开放。 |

`GET /calls/api/devices` 的内容：

- 响应：`{ok, usb: {enabled, adb_found, adb_path, error, phones: [{serial, state, model, reverse, app_installed}]}, connected: [...], labels: {<phone id>: <名称>}, calls_in_progress, phones: [...]}`。
- 最外层的 `phones` 是 Bageo 启动后在 control 连接上见过的每部手机：`{serial, connected, mode, supports_mode, model, app_version, in_call, last_seen_ms}`。
- `mode` 是 `ai`（AI 接听）或 `normal`（一般手机，见[手机设置](PHONE.md#普通手机模式)）。
- 比这个功能旧的 App，`supports_mode` 是 false，永远是 `ai`。

`GET /health` 的内容：

- `control_idle_clients`／`control_idle_serials` 统计所有连上的手机；`normal_mode_serials` 列出切换成一般手机的那几部。
- `local_time` 是 Bageo 按 `TZ` 的时钟。
- `summary_ready`（`summary_api_key_set` 是同一个值）表示有没有设置摘要服务。
- `setup_missing` 列出控制台“都准备好了吗？”还没完成的项目：`ai_off`、`gemini_key`、`owner_name`、`notify`、`summary`（只有名称，没有设置值）；全部设置好是空数组。
- `GET /health` 只有带了控制台密码的请求才拿到完整内容。没设密码时，只有这台电脑自己发的请求拿得到（经过反向代理的不算；用 Docker 默认网络时，从宿主机发的请求也算别的电脑）。其他来源只返回 `ok`、`version`、`local_time`、`language`、`notify_language`、`active_call_count`、`control_idle_clients` 和 `setup_missing`，监控用已经足够。

切换模式后，`mode` 是手机现在的模式（不知道时为 `null`）；成功时 `error_code` 为 `null`，否则是下面其中之一：

| `error_code` | 含义 |
|---|---|
| `offline` | 手机没连上。 |
| `unsupported` | App 比模式切换功能旧。 |
| `in_call` | 有电话在响或通话中；结束后再试。 |
| `busy` | 上一次切换还在进行。 |
| `no_root` | Bageo 模块的 root 小助手没有接手：模块没有在运行。 |
| `save_failed`、`dialer_failed`、`screening_failed`、`dnd_failed`、`volume_failed`、`failed` | 没成功的那一步。再切换一次会把每一步重做一遍。 |
| `timeout` | 还没确认，不等于失败：切换本身可能让手机 App 重启，它重新连上后才回报。稍后再读一次 `GET /calls/api/devices`。 |
| `bad_request`／`invalid_mode` | HTTP 400：不是 JSON、没带 `Content-Type: application/json`、没有字符串 `mode`、serial 不是由字母数字、`-`、`_` 组成；或 `mode` 不认识。 |

```bash
curl -s -X POST "http://127.0.0.1:8787/calls/api/devices/pixel-12345/mode" \
  -H "Authorization: Bearer $CALLS_UI_TOKEN" -H "Content-Type: application/json" \
  -d '{"mode": "normal"}'
# {"ok": true, "mode": "normal", "error_code": null}
```

## 手机这一端（WebSocket）

手机通过 WebSocket 连接到 `/` 或 `/ws`（两者相同）。

- 每部手机保持一条长期的 *control* 连接，每通电话另开一条 *media* 连接。
- 两种连接都双向传送 JSON 文本帧和原始的 16 kHz 单声道 16 位小端序 PCM 二进制帧（手机每 60 ms 发一帧）。
- 来自这台电脑本身的连接（USB 通道）不需 Token 一律接受；Wi-Fi 连接可连 `PHONE_WIFI_PORT`（默认 `8788`，仅提供 `/ws`）并带上 USB 配对时发放的 `phone-pairing.key` 或 `DEVICE_TOKEN`，或连到主端口并在设置了 `DEVICE_TOKEN` 时带上 `?token=<DEVICE_TOKEN>`。

逐字段的完整消息列表在 [`android/README.md`](../../android/README.md#websocket-protocol)（英文）。任何会说这套协议的程序都能代替手机，所以要加上 SIP 或软电话前端，完全不用改动通话逻辑。双方都会忽略不认识的内容，所以旧 App、旧版 Bageo照样能用。

### Control 连接

| 方向 | 消息 | 含义 |
|---|---|---|
| 手机 → Bageo | `hello` | `serial`、`mode` = `CONTROL_IDLE`、`model`、`app_version`、`phone_mode`（`ai`／`normal`）、`in_call`、`supports_sms`、`supports_reject`、`sims`（`[{sub_id, country, slot?, carrier?, number?}]`，1.4 以上 App 会自动上报）。缺少字段的含义见下方。 |
| 手机 → Bageo | `ping` | 每 15 秒一次。 |
| 手机 → Bageo | `status` | `{mode, in_call}`（SIM 变动时另含 `sims`）：模式、通话状态或 SIM 列表变了。 |
| 手机 → Bageo | `incoming`、`incoming_cancel` | 有电话在响（`caller_id`，隐藏号码时为空字符串；`sim_country`；知道时带 `sim_sub_id`）／接起来之前就结束了。 |
| 手机 → Bageo | `reject_failed` | 收到 `reject: true` 的 `answer_plan` 但未能拒接该通来电。 |
| Bageo → 手机 | `answer_plan` | `{delay_ms, prewarm, reject}`：响多久再接，或 `reject: true` 响铃中直接拒接（`reject_mode` 为 `decline` 的黑名单号码）。 |
| 手机 → Bageo | `call_waiting_rejected` | `{serial, caller_id, sim_country, sim_sub_id}`：通话中又有第二通打进来，手机立刻拒接（AI 模式）。会留下通话记录并发出“通话中插播未接”通知。 |
| Bageo → 手机／手机 → Bageo | `set_mode`／`set_mode_result` | `{mode, id}`／`{id, ok, mode, error}`：远程切换模式；`error` 用上面的代码。 |
| Bageo → 手机 | `agent_info` | `{console_url}` 给 App 的“**打开网页控制台**”按钮用（`public_url` 或局域网地址）；通过受信任的 USB 通道时另附 `wifi: {urls: [...]}` 提供 Wi-Fi 配对地址（`ws://<局域网-IP>:<PHONE_WIFI_PORT>/ws?token=<key>`，开启局域网名称时另附 `ws://<名称>.local:<PHONE_WIFI_PORT>/ws?token=<key>`）。 |
| Bageo → 手机 | `place_call` | `{number, call_id, task_id, sub_id}`：拨出电话（AI 回拨或 AI 代打），用 subscription id 为 `sub_id` 的 SIM 拨（找不到就用手机默认的）。忙线时见下方。 |
| 手机 → Bageo | `place_call_ack`、`place_call_failed`、`outbound_ready`、`call_end` | 拨出的电话已拨号／拨不出去／接通／结束。 |
| Bageo → 手机 | `send_sms` | `{id, to, text, sub_id}`：给来电者发短信（日程预约）。字段限制见下方。 |
| 手机 → Bageo | `send_sms_result` | `{id, ok, error}`，每一段都上报后才发。超时和重发的规则见下方。 |
| 手机 → Bageo | `send_sms_delivered` | `{id, ok}`：运营商给出送达回执时才发（手机最多等一小时）。 |

`hello` 缺少字段时：

- 没有 `phone_mode`：比模式切换旧的 App（永远是 `ai`）。
- 没有 `supports_sms`：不会让它发短信。
- 没有 `supports_reject`：黑名单号码改成接起来播放语音。

`place_call`：手机有电话在响或通话中时不拨，回复 `place_call_failed`，`reason` 为 `busy`。

`send_sms` 的字段：

- `id`：1–64 个字符。
- `to`：只有数字，开头可以有 `+`。
- `text`：最多 500 个字符。
- `sub_id`：指定 SIM（手机上报的 `sim_sub_id`），没有就用默认的短信 SIM。
- 已经发过的 id 再来一次，只会再回复一次结果，不会重复发送。

`send_sms_result` 的规则：

- 手机最多等 60 秒（超时回 `timeout`）；之后才收到回报，会再发一次真正的结果。
- `error`：`no_permission`、`bad_request`、`no_service`、`radio_off`、`generic_failure`、`timeout`、`exception`。
- Bageo 最多等 90 秒。
- 重新连接后手机会把最近 20 个 id 的结果再发一次，Bageo 用 `id` 去掉重复的。

### Media 连接

| 方向 | 消息 | 含义 |
|---|---|---|
| 手机 → Bageo | `hello` | `serial`、`caller_id`、`sample_rate`（16000）、`mode` = `SILENT_LOCAL`、`sim_country`、`sim_sub_id`、`model`、`app_version`。拨出电话的 SIM 比对见下方。 |
| 手机 → Bageo | `ringing`、`call_end` | 对方号码（之后才知道时会再发）／通话结束。 |
| Bageo → 手机 | `tts_start`、`tts_end`、`hangup` | 接下来有声音／声音结束／挂断。 |
| Bageo → 手机 | `add_call`、`add_call_abort` | `{id, number, merge, answer_timeout_ms}`／`{id}`：白名单急事转接时让手机拨给你并合并成三方通话（或取消并把来电者切回通话）。 |
| 手机 → Bageo | `add_call_result` | `{id, status, reason?}`（`status` 为 `dialing`／`active`／`merged`／`failed`）：三方拨号与合并的进度或结果。 |
| 双向 | 二进制 | 对方的声音（手机 → Bageo）和 AI 的声音（Bageo → 手机），格式同上。 |

Bageo 怎么处理边界情况：

- 拨出的电话：Bageo 会拿 media `hello` 的 `sim_sub_id` 比对它指定的 SIM（代打不符就是 `wrong_sim`）。
- 短信结果在 Bageo 不再等待之后才到，照样会记录；手机之后用同一个 `id` 发来的真正结果会取代 `timeout`。同样的结果再来、或已有确定结果的短信收到不同结果，都不会改。
- 黑名单号码只有在 `reject_mode` 为 `decline`、并且手机声明过 `supports_reject` 时才让手机拒接；否则接起来播放语音，通知会写清楚是哪一种。
- AI 回拨和 AI 代打都带 `sub_id`，双卡手机会用正确的号码拨出。

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
