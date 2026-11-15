<p align="center">
  <img src="docs/images/banner.zh-CN.svg" width="900" alt="Bageo（八哥）：你的手机号，AI 帮你接。接电话、拦诈骗、代打电话。">
</p>

<p align="center">
  <a href="README.md">English</a> · <a href="README.zh-TW.md">繁體中文</a> · <strong>简体中文</strong>
</p>

<p align="center">
  <strong>AI 接电话、替你打电话的 AI 电话助理（Gemini Live＋你自己的手机号）</strong><br>
  自托管、开源。一部插着你 SIM 卡、已 root 的备用 Android 手机，电脑上的一个程序，再加上你自己的 Gemini 密钥。
</p>

<p align="center">
  <a href="https://github.com/h4lo-void/bageo-call/actions/workflows/ci.yml"><img src="https://github.com/h4lo-void/bageo-call/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/h4lo-void/bageo-call/releases/tag/v1.0.0"><img src="https://img.shields.io/badge/release-v1.0.0-brightgreen.svg" alt="Release v1.0.0"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-AGPL--3.0--only-blue.svg" alt="许可证：AGPL-3.0-only"></a>
  <a href="COMMERCIAL_LICENSE.md"><img src="https://img.shields.io/badge/商业授权-双重授权-gold.svg" alt="提供商业授权"></a>
</p>

<table>
  <tr>
    <td width="33%" align="center" valign="top">
      <img src="docs/images/icons/answer.svg" width="56" height="56" alt=""><br>
      <strong>接电话</strong><br>
      响一声就接，说话像真人，等对方说再见才挂。
    </td>
    <td width="33%" align="center" valign="top">
      <img src="docs/images/icons/stall.svg" width="56" height="56" alt=""><br>
      <strong>拦诈骗</strong><br>
      认出推销和诈骗电话，装成问题特别多的客户，最长缠住对方 30 分钟。
    </td>
    <td width="33%" align="center" valign="top">
      <img src="docs/images/icons/errand.svg" width="56" height="56" alt=""><br>
      <strong>代打电话</strong><br>
      写一句“订明天晚上 7 点 4 位”，它就替你打；拿不准会先问你。
    </td>
  </tr>
</table>

<p align="center"><sub>名字取自八哥（bāgē）：一种会学身边的人说话的鸟。</sub></p>

## 诈骗电话打来？八哥陪他慢慢聊

<p align="center">
  <img src="docs/images/scene-stall.zh-CN.svg" width="760" alt="一通贷款推销电话。对方说你有一笔低息信用贷款额度批下来了，AI 问利息是按月还是按年；对方要核对身份证号，AI 继续提问。27 分钟后对方想挂电话，AI 说：诶等一下，我还有一个问题。结果：陪聊 27 分钟，没有给出任何个人信息，录音和摘要已保存。">
</p>

AI 一确定对方是推销或诈骗，就变成一个有兴趣、反应慢、爱问细节的客户：一直问、故意把数字听错，最长缠住对方 30 分钟，一项真实信息都不给。你会收到录音、文字记录和一段摘要。正常电话不会被误伤：银行核对刷卡、诊所、快递、家人。陪聊模式默认开启，一个开关就能关掉。[三道防线怎么判断](docs/zh-CN/FEATURES.md)

## 懒得打电话？写一句话就行

<p align="center">
  <img src="docs/images/scene-errand.zh-CN.svg" width="760" alt="一次 AI 代打。你交代：订明天晚上 7 点 4 位，7 点半也行。AI 以小明助理的身份打给餐厅；7 点满了，餐厅问 7 点半可不可以，AI 答应。结果：办好了，明天晚上 7 点半、4 位，可以一键添加到日历。">
</p>

订位、改期、问店家有没有货或几点开门，都可以交代。对方提到你没交代的事，AI 会先在屏幕上问你；它绝不自己答应任何事：你来不及回，它就说会跟你确认，然后客气收尾。[AI 代打的完整说明](docs/zh-CN/FEATURES.md)

## 工作原理

<p align="center">
  <img src="docs/images/how-it-works.zh-CN.svg" width="900" alt="Bageo 的工作原理：对方通过运营商网络打到你的备用手机（已 root、插着你的 SIM 卡），手机用 USB 或 Wi-Fi 把通话声音交给你电脑上的 Bageo 主程序，主程序再和 Gemini Live 对话。录音留在你的硬盘，通知发到 16 种渠道，一切在网页控制台里管理。">
</p>

手机只负责接通电话，其他事都在你自己的电脑上处理。手机还在响，AI 就已经连上，所以接起后约 **0.43 秒**开口（实测中位数）。一通电话的完整过程：[架构](docs/zh-CN/ARCHITECTURE.md)。

## 为什么选 Bageo

| | 架在 SIP 线路或虚拟号码上的 AI 电话服务 | Bageo |
|---|---|---|
| **号码** | 服务商提供的号码，或把你的来电转移过去 | 你原来的手机号，AI 拨出时显示的也是它 |
| **费用** | 月费或按分钟计费 | 没有平台月费，只付 Gemini 用量和运营商的号码费用 |
| **数据** | 存在服务商的服务器上 | 你硬盘上 `data/` 文件夹里的普通文件 |
| **代价** | 注册账号就能用 | 要一部已 root 的备用 Android 手机，和一台一直开着的电脑 |

## 还能做这些

<table>
  <tr>
    <td width="33%" align="center" valign="top">
      <img src="docs/images/icons/calendar.svg" width="56" height="56" alt=""><br>
      <strong>预约等你点头</strong><br>
      开启日程预约后，对方想约时间会先进你的待处理事项；你点同意或不同意，AI 再发短信或回拨告诉他。
    </td>
    <td width="33%" align="center" valign="top">
      <img src="docs/images/icons/contacts.svg" width="56" height="56" alt=""><br>
      <strong>认得谁打来</strong><br>
      通讯录可以从 vCard、CSV、Excel、Word、PDF，甚至名片照片导入。
    </td>
    <td width="33%" align="center" valign="top">
      <img src="docs/images/icons/languages.svg" width="56" height="56" alt=""><br>
      <strong>9 种语言、30 种声音</strong><br>
      对方说什么语言就跟着换，文字记录附翻译。
    </td>
  </tr>
  <tr>
    <td width="33%" align="center" valign="top">
      <img src="docs/images/icons/sound.svg" width="56" height="56" alt=""><br>
      <strong>听起来像真的在打电话</strong><br>
      可以垫背景音：居家、办公室、街边、夜市等 12 种。
    </td>
    <td width="33%" align="center" valign="top">
      <img src="docs/images/icons/notify.svg" width="56" height="56" alt=""><br>
      <strong>16 种通知渠道</strong><br>
      每通电话结束，摘要发到 Telegram、Discord、飞书、钉钉等。
    </td>
    <td width="33%" align="center" valign="top">
      <img src="docs/images/icons/local.svg" width="56" height="56" alt=""><br>
      <strong>数据留在自己手里</strong><br>
      录音和文字记录都是你硬盘上的普通文件，不用在我们这里注册账号。
    </td>
  </tr>
</table>

每一项的完整说明：[功能总览](docs/zh-CN/FEATURES.md)。

## 需要准备什么

1. **一部备用 Android 手机**，插着你的 SIM 卡，用 Magisk 或 KernelSU root，Android 10 及以上。实测过 Google Pixel 7、7a、8、9。
2. **一台电脑**，需要接电话的时段开着：Windows 10/11、macOS、Linux 或 Docker。
3. **一根 USB 线**（也可以用 Wi-Fi）。
4. **一个 Gemini API 密钥**，在 [Google AI Studio](https://aistudio.google.com/apikey) 获取，Google 按用量计费。

## 快速开始

1. **下载**：从 [Releases](https://github.com/h4lo-void/bageo-call/releases) 下载适合你系统的安装包并解压。
2. **启动**：Windows 双击 `start.cmd`，macOS 双击 `start.command`，Linux 运行 `./start.sh`，控制台会在 <http://localhost:8787/> 打开。
3. **填写基本设置**：在“**设置**”里填上 Gemini API 密钥、你的名字和语言，按 ▶ 试听各个声音，选一个。
4. **准备手机**（只需一次）：在 Magisk 或 KernelSU 里安装模块 `pixelcallai_uplink-<version>.zip`，重启，打开 **Bageo** 应用把检查清单做完，插上 USB。
5. **打一通试试**：用另一部手机拨打它的号码，响一声，AI 就接了。

卡住了？每一步的详细说明：[开始使用](docs/zh-CN/GETTING_STARTED.md) · [手机设置](docs/zh-CN/PHONE.md)。

## 文档

| | |
|---|---|
| **上手** | [开始使用](docs/zh-CN/GETTING_STARTED.md) · [手机设置](docs/zh-CN/PHONE.md) · [故障排查与常见问题](docs/zh-CN/TROUBLESHOOTING.md) |
| **使用** | [功能总览](docs/zh-CN/FEATURES.md) · [编写提示词](docs/zh-CN/PROMPTS.md) · [设置参考](docs/zh-CN/SETTINGS.md) · [通知](docs/zh-CN/NOTIFICATIONS.md) |
| **深入** | [架构](docs/zh-CN/ARCHITECTURE.md) · [API](docs/zh-CN/API.md) · [路线图](docs/zh-CN/ROADMAP.md) · [v1.0 发布说明](CHANGELOG.md) · [`android/`（英文）](android/README.md) · [`tools/sms_forward/`（英文）](tools/sms_forward/README.md) |
| **法律** | [隐私、安全与法律](docs/zh-CN/PRIVACY_AND_LEGAL.md) · [法律免责声明与使用限制](LEGAL.md) · [双重授权与商业授权](COMMERCIAL_LICENSE.md) · [全项目技术与安全审计报告](AUDIT_REPORT.md) |

**测试状态**：超过 520 个自动化测试、234 通模拟通话（28 种语言），并在 Pixel 7、7a、9 上实机测过。[详细状态](docs/zh-CN/FEATURES.md#测试状态)。其他手机和语言的使用反馈最有帮助，请见 [CONTRIBUTING.md](CONTRIBUTING.md)（英文）。

## 常见问题

<details>
<summary><strong>AI 可以用我原来的手机号接电话吗？</strong></summary>

可以，这正是 Bageo 的用途。把 SIM 卡放进一部已 root 的备用 Android 手机，AI 就用你的号码接电话，也从这个号码拨出。想让 SIM 卡留在主力手机里，也可以把未接来电转移到备用机的号码：你的手机先响，没接的才交给 AI（这时 AI 代打和回拨显示的是备用机的号码）。

</details>

<details>
<summary><strong>一定要 root 吗？iPhone 能用吗？</strong></summary>

必须 root，iPhone 不行。Android 只允许有特权的系统应用录下通话并把声音播进通话，所以手机需要 Magisk 或 KernelSU 才能安装 Bageo 模块；iOS 完全不允许。

</details>

<details>
<summary><strong>支持哪些手机？</strong></summary>

实测过 Google Pixel 7、7a、8、9，需要 Android 10 及以上。其他已 root 的手机，要它的音频硬件层允许应用录下通话并把声音播进通话才能用；不允许的话，电话会接起来，但有一方听不到声音。见[支持哪些手机](docs/zh-CN/PHONE.md#支持哪些手机)。

</details>

<details>
<summary><strong>要花多少钱？</strong></summary>

个人与开源社区免费使用（`AGPL-3.0-only`；如需闭源商用或商业托管请[取得商业授权](COMMERCIAL_LICENSE.md)）。AI 处理的每通电话（不论来电还是拨出），Google 都会按用量收取 Gemini Live 的语音费用；摘要只是发给 Gemini 最便宜模型的一小段文字。试听声音和拒接语音都是预录的，不花钱。另外就是运营商的号码费用和电脑的电费；想设上限，可以在 Google 账号里设置预算。

</details>

<details>
<summary><strong>让 AI 接电话合法吗？</strong></summary>

要看你和通话对象在哪里。通话录音、披露 AI 身份、自动拨出的电话，各有各的规定。“AI 助理”身份第一句话就会表明是 AI；“本人”身份在被认真问到时才会说。开始使用前，请先读[隐私、安全与法律](docs/zh-CN/PRIVACY_AND_LEGAL.md)。

</details>

<details>
<summary><strong>能离线、不用 Google 吗？</strong></summary>

不能。实时语音用的是 Gemini Live，需要网络和 Gemini 密钥。摘要可以通过 Ollama 使用本地模型（见[摘要与日历](docs/zh-CN/SETTINGS.md#摘要与日历)）；完全本地的语音流程不在支持的配置里（见[路线图](docs/zh-CN/ROADMAP.md)）。

</details>

## 请负责任地使用

- **各地法律不同。** 通话录音，以及让 AI 在不告知对方的情况下与人通话，在很多地方都受到限制。“AI 助理”接听身份一开口就表明是 AI；以助理身份代打时，AI 会自称是你的助理；任何通话被认真问到时都会承认是 AI。请先弄清楚你和通话对象所在地的规定。
- **AI 不做任何承诺。** 它绝不答应付款、定金、合同、密码或验证码，预约也要等你决定。但它毕竟是 AI：摘要和代打结果一定要看。
- **紧急情况不要依赖它。** AI 代打绝不会拨打紧急电话。
- **Root 会使保修失效，也可能出问题。** 请用备用手机。

## 许可证与法律责任声明

本项目采用**双重授权（Dual Licensing）**模式发布（版权所有 © 2026 **h4lo-void**）：

- **社区开源授权：** [GNU Affero 通用公共许可证第 3.0 版（仅限 `AGPL-3.0-only`）](LICENSE)——个人与开源社区免费使用；任何修改版本只要允许他人通过网络或电话线路交互，即须按第 13 条免费公开完整衍生源代码。
- **取得商业授权：** 若企业、电信／云总机运营商、呼叫中心或硬件厂商不愿按 `AGPL-3.0-only` 开源，可向原作者购买专有／商业授权，详见 [**双重授权与商业授权条款（COMMERCIAL_LICENSE.md）**](COMMERCIAL_LICENSE.md)。

**法律责任隔离与风险自负：** 本项目仅为纯自托管开源代码，按“现状（AS IS）”提供且不附带任何担保。部署者本人为唯一的实际行为人与数据控制者，须完全独立承担当地通话录音、AI 身份披露、自动拨号、手机 Root 失去保修及资费账单的一切民刑事与行政责任，严禁任何违法诈骗、窃听或骚扰用途。使用前请务必详阅 [**法律免责声明（LEGAL.md）**](LEGAL.md)、[隐私、安全与法律](docs/zh-CN/PRIVACY_AND_LEGAL.md) 及 [`gemini-4-carbon` 具名全项目审计报告](AUDIT_REPORT.md)。第三方声明见 [NOTICE.md](NOTICE.md)。本项目与 Google LLC 无关；Pixel、Android 与 Gemini 是 Google LLC 的商标。
