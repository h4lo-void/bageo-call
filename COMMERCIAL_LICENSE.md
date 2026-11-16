# 雙重授權與商業授權條款 / Dual Licensing & Commercial License Terms

[繁體中文](#繁體中文) · [English](#english) · [简体中文](#简体中文)

---

## 繁體中文

著作權所有 (C) 2026 **h4lo-void**（<https://github.com/h4lo-void>）。保留一切權利。

**Bageo（八哥，`bageo-call`）** 採用對原創作者最具保障之**「雙重授權（Dual Licensing）」模式**發佈：

1. **社群開源授權 — GNU Affero 通用公共授權條款第 3.0 版（僅限 `AGPL-3.0-only`）**：免費提供予個人、學術研究及願意完整履行開源義務之社群使用者，惟須嚴格遵守 [`LICENSE`](LICENSE)（援引 GNU AGPL v3.0 正式條文）及依第 7 條附加於 [`LEGAL.md`](LEGAL.md)、[`NOTICE.md`](NOTICE.md) 之補充規定。
2. **專有／商業授權（Commercial License）**：若企業、電信／二類電信業者、客服外包商、SaaS 雲端平台、硬體設備商或任何商業營運者不願或無法依 `AGPL-3.0-only` 公開完整衍生原始碼，**必須**事先向原作者（**h4lo-void**）購買並簽署書面專有／商業授權。

### 一、免費社群授權（`AGPL-3.0-only`）之嚴格條件

本專案僅以 **AGPL 第 3.0 單一版本（`AGPL-3.0-only`）** 授權，明確排除第 14 條「或任何後續版本（or any later version）」之自動適用；未來是否採納其他授權版本，完全由著作權人 **h4lo-void** 單獨決定。免費使用、修改或散布本軟體者，必須遵守以下核心義務：

- **網路與電信互動之強制開源義務（AGPL-3.0 第 13 條）：** 只要你修改了 Bageo 任何部分（包含 Rust 主程式、語音與防詐邏輯、網頁控制台、Android 特權 App、Magisk／KernelSU 模組、Xposed 模組或簡訊轉發工具），並讓任何使用者或第三方透過電腦網路或電信網路與該修改版互動（包含透過網頁控制台、HTTP／WebSocket API、通知機器人、VoIP 閘道，或透過實體 SIM／PSTN 自動接聽與撥打電話、收發簡訊），你**必須**向所有互動對象醒目提供並免費開放你整個修改版系統之完整對應原始碼（Complete Corresponding Source），且同樣以 `AGPL-3.0-only` 授權。
- **衍生作品與硬體封裝之整體開源義務（AGPL-3.0 第 4–6 條）：** 任何整合、連結、封裝或預刷本軟體之衍生程式、容器映像檔、客製化 Android ROM 或實體電話盒／語音閘道硬體，於散布或交付時均須整體以 `AGPL-3.0-only` 開放完整原始碼與安裝資訊。
- **保留作者署名、法律免責聲明、商標保留與下游賠償（AGPL-3.0 第 7 條）：**
  1. 必須完整保留所有著作權聲明、原作者署名（`h4lo-void`／Bageo）、無擔保聲明及 [`LEGAL.md`](LEGAL.md) 法律責任條款，嚴禁刪除、隱藏、改寫或冒充原創者（第 7(b)、7(c) 條）；
  2. 未經書面許可，不得以原作者名義為衍生產品背書或進行商業宣傳（第 7(d) 條）；
  3. 本開源授權**不授予**「Bageo」、「八哥」、「bageo-call」名稱或專案視覺圖示之任何商標權或品牌權利（第 7(e) 條）；
  4. 任何人於散布本軟體或修改版本時，若自行向下游收受人提供任何契約擔保或承擔責任，須就因此直接加諸於原作者與貢獻者之任何責任，負擔全額抗辯與損害賠償義務（第 7(f) 條）。

### 二、取得商業授權（何時必須購買專有／商業授權）

凡屬下列任一商業或閉源使用情境，且營運商或廠商**不願依 `AGPL-3.0-only` 向公眾與用戶公開完整原始碼**者，均必須事先向原作者（**h4lo-void**）取得書面商業授權：

1. **雲端代管／SaaS／商業代接與代打服務：** 將 Bageo（或其修改版、整合版）作為收費或商業營運之 AI 電話秘書、企業雲端總機、客服外包系統、預約訂位平台或多租戶代管服務，而保持後端、前端或業務整合程式碼閉源。
2. **閉源商業軟體或硬體設備封裝銷售：** 將 Bageo 任何程式碼或模組嵌入、連結、打包或預刷於閉源商業軟體、專有韌體、客製化手機或商業硬體盒／閘道設備中銷售、出租或交付客戶。
3. **OEM／白牌品牌授權與客製授權條款：** 需於商業產品中移除開源互動式法律提示、使用客製品牌，或需要企業內部合規所需之專有商業授權書。

未取得 **h4lo-void** 事前簽署之有效書面商業授權而擅自進行閉源散布、閉源網路／電信營運或移除授權與免責聲明者，其依 `AGPL-3.0-only` 取得之一切權利將依第 8 條**自動終止**，構成侵害著作權；著作權人將依法請求停止侵害、銷毀侵權物及請求損害賠償。

### 三、商業授權可包含之內容

商業授權之實際內容以雙方簽署之書面合約為準；本文件不構成要約或任何承諾。可洽談之項目包含：

- **閉源使用權：** 於合約範圍內使用、修改及整合 Bageo，無須依 `AGPL-3.0-only` 公開你的原始碼。
- **官方技術支援與服務水準（SLA）：** 由原作者提供之技術支援；回應時間、支援管道與服務範圍載明於合約。
- **授權與智慧財產權之書面文件：** 供企業法遵、採購與稽核使用之授權證明，以及著作權人有權授予該授權之書面保證。外部貢獻之程式碼均依[貢獻者授權協議](CLA.md)取得授權，故整份程式碼皆可納入商業授權。

商業授權不改變 [`LEGAL.md`](LEGAL.md) 之守法要求：部署是否符合當地電信、通話錄音與個人資料法規，仍由營運者自行負責；一旦將本軟體用於 `LEGAL.md` 第三條所列用途，商業授權立即終止。

### 四、如何洽詢與取得商業授權

請透過 GitHub 聯絡著作權人 **[@h4lo-void](https://github.com/h4lo-void)**（可透過個人頁面聯絡管道或於本專案建立商業授權洽詢聯繫），並附上：

- 公司／法人全銜、註冊國家／地區與官方網站；
- 預計應用場景（SaaS 服務、企業部署、硬體預載或 OEM 整合）；
- 預計規模（門號線路數、設備台數或商業用戶規模）。

> **合法合規鐵律：** 無論依據免費社群授權（`AGPL-3.0-only`）或付費商業授權，均**絕不授權**任何違法竊聽錄音、電信詐欺、冒名頂替、非法大量自動撥號或違反各國電信法規與個資法規之行為。一切部署與使用行為均須嚴格遵守 [`LEGAL.md`](LEGAL.md) 之免責與守法條款。

---

## English

Copyright (C) 2026 **h4lo-void** (<https://github.com/h4lo-void>). All rights reserved.

**Bageo (`bageo-call`)** is distributed under a **Dual-Licensing Model**:

1. **Open-Source Community License — GNU Affero General Public License v3.0 Only (`AGPL-3.0-only`):** Free for personal, academic, research, and open-source community use in strict compliance with [`LICENSE`](LICENSE) (incorporating GNU AGPL v3.0, 19 November 2007) and the Section 7 supplemental terms in [`LEGAL.md`](LEGAL.md) and [`NOTICE.md`](NOTICE.md).
2. **Commercial / Proprietary License:** Available via separate paid written agreement with the copyright holder (**h4lo-void**) for companies, telecom/VoIP operators, call-center providers, SaaS vendors, hardware OEMs, or commercial entities that do not wish to disclose their source code under `AGPL-3.0-only`.

### 1. Strict Conditions of `AGPL-3.0-only`

Bageo is licensed strictly under Version 3 of the GNU Affero General Public License **only** (`AGPL-3.0-only`); the Section 14 "or any later version" option is expressly withheld and reserved solely to **h4lo-void**.

- **Remote Network & Telephony Interaction (Section 13):** Modifying Bageo and allowing any user or third party to interact with the modified program over a computer or telecommunications network (including web console, HTTP/WebSocket APIs, chat/notification bots, VoIP bridges, or automated PSTN/cellular calls and SMS) requires you to prominently offer and provide the Complete Corresponding Source Code of the entire modified system free of charge under `AGPL-3.0-only`.
- **Copyleft on Embedded & Hardware Distributions (Sections 4–6):** Distributing Bageo inside software bundles, containers, custom Android ROMs, or pre-flashed hardware appliances requires licensing the combined work under `AGPL-3.0-only` with full source code and Installation Information.
- **Attribution, Legal Notices, Trademarks & Indemnification (Section 7):** Under AGPL-3.0 Section 7(b)–(f), all copyright notices, author attributions (`h4lo-void`), warranty disclaimers, and [`LEGAL.md`](LEGAL.md) notices must remain intact; origin may not be misrepresented; no trademark rights in **Bageo**, **八哥**, or **bageo-call** are granted; and anyone conveying Bageo with contractual warranties or liability assumptions must indemnify and hold harmless the original author and contributors.

### 2. Obtaining a Commercial License

A separate signed **Commercial License** from **h4lo-void** is mandatory prior to:

- Operating a modified or integrated Bageo system as a closed-source hosted SaaS, commercial AI receptionist, BPO/call-center platform, or managed telephony service;
- Embedding, linking, or shipping Bageo components inside closed-source software, proprietary firmware, or commercial hardware devices; or
- White-label / OEM distribution or custom enterprise licensing.

Unauthorized closed-source commercial deployment or removal of legal notices automatically terminates `AGPL-3.0-only` rights under Section 8 and constitutes copyright infringement. To inquire about a Commercial License, contact **[@h4lo-void](https://github.com/h4lo-void)** with your organization details, intended product model, and deployment scale. All deployments remain bound by [`LEGAL.md`](LEGAL.md).

### 3. What a Commercial License Can Include

The signed written agreement decides what a Commercial License contains; nothing in this file is an offer or a promise. Items that can be agreed include:

- **Closed-source use:** Using, modifying, and integrating Bageo within the agreed scope without publishing your source code under `AGPL-3.0-only`.
- **Official technical support with a service level (SLA):** Support from the original author, with response times, contact channels, and scope written into the agreement.
- **Written license and intellectual-property assurances:** Proof of license for compliance, procurement, and audit reviews, and the copyright holder's written assurance of having the right to grant the license. Outside contributions come in under the [Contributor License Agreement](CLA.md), so the whole code base can be covered by a Commercial License.

A Commercial License does not change what [`LEGAL.md`](LEGAL.md) requires: whether a deployment complies with local telecom, call-recording, and personal-data law remains the operator's responsibility, and a Commercial License ends at once if Bageo is put to any use prohibited in Section 3 of `LEGAL.md`.

---

## 简体中文

版权所有 (C) 2026 **h4lo-void**（<https://github.com/h4lo-void>）。保留所有权利。

**Bageo（八哥，`bageo-call`）** 采用**“双重授权（Dual Licensing）”模式**发布：

1. **社区开源授权 — GNU Affero 通用公共许可证第 3.0 版（仅限 `AGPL-3.0-only`）**：免费提供给个人、学术研究及完整履行开源义务的社区用户，须严格遵守 [`LICENSE`](LICENSE) 以及 [`LEGAL.md`](LEGAL.md)、[`NOTICE.md`](NOTICE.md) 中的第 7 条补充声明。
2. **专有／商业授权（Commercial License）**：若企业、电信／增值电信运营商、呼叫中心、SaaS 平台、硬件厂商或商业运营者不愿按 `AGPL-3.0-only` 公开完整衍生源代码，**必须**事先向原作者（**h4lo-void**）购买并签署书面商业授权。

### 一、免费社区授权（`AGPL-3.0-only`）核心义务

- **网络与电信交互强制开源（第 13 条）：** 只要修改了 Bageo 并允许他人通过计算机网络或电信网络与之交互（含控制台、HTTP／WebSocket API、机器人通知、VoIP 或实卡电话自动接打与短信收发），必须向所有交互对象免费提供完整对应源代码并按 `AGPL-3.0-only` 授权。
- **衍生品与硬件封装整体开源（第 4–6 条）：** 任何集成或预装本软件的应用、镜像、定制 ROM 或硬件盒子，分发时须整体按 `AGPL-3.0-only` 开放全部源代码与安装信息。
- **保留署名与免责条款、商标保留及下游赔偿（第 7 条）：** 必须完整保留 `h4lo-void` 版权署名与 [`LEGAL.md`](LEGAL.md) 免责声明；不授予“Bageo”、“八哥”、“bageo-call”商标权；任何向下游承诺担保或责任的分发者须对原作者承担全额抗辩与赔偿责任。

### 二、取得商业授权

如需闭源运营商业 SaaS／云总机／代接代打服务、闭源集成至商业软件或硬件设备销售，或获取 OEM 白牌授权，请事先通过 GitHub 联系版权人 **[@h4lo-void](https://github.com/h4lo-void)** 商谈书面商业授权（注明企业全称、应用场景及线路／设备规模）。未经书面商业授权擅自闭源商用的，其开源许可依 AGPL-3.0 第 8 条自动终止并构成侵权。任何授权均严禁用于违法犯罪或违规电信行为，详见 [`LEGAL.md`](LEGAL.md)。

### 三、商业授权可以包含的内容

商业授权的实际内容以双方签署的书面合同为准；本文件不构成要约或任何承诺。可以商谈的项目包括：

- **闭源使用权：** 在合同范围内使用、修改及集成 Bageo，无须按 `AGPL-3.0-only` 公开你的源代码。
- **官方技术支持与服务水平（SLA）：** 由原作者提供的技术支持；响应时间、支持渠道与服务范围写入合同。
- **授权与知识产权的书面文件：** 供企业合规、采购与审计使用的授权证明，以及版权人有权授予该授权的书面保证。外部贡献的代码均依[贡献者许可协议](CLA.md)取得许可，因此整份代码都可以纳入商业授权。

商业授权不改变 [`LEGAL.md`](LEGAL.md) 的守法要求：部署是否符合当地电信、通话录音与个人信息法规，仍由运营者自行负责；一旦将本软件用于 `LEGAL.md` 第三条所列用途，商业授权立即终止。
