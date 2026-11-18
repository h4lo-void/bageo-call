# 雙重授權與商業授權條款 / Dual Licensing & Commercial License Terms

[繁體中文](#繁體中文) · [English](#english) · [简体中文](#简体中文)

---

## 繁體中文

著作權所有 (C) 2026 **h4lo-void**（<https://github.com/h4lo-void>）。保留一切權利。

**Bageo（八哥，`bageo-call`）** 採用**雙重授權（Dual Licensing）**模式發佈（詳見 [`LICENSING.md`](LICENSING.md)）：

1. **社群開源授權 — GNU Affero 通用公共授權條款第 3.0 版（僅限 `AGPL-3.0-only`）**：免費提供予個人、學術研究及願意履行開源義務之社群使用者，須遵守 [`LICENSE`](LICENSE)（GNU AGPL v3.0 正式全文）及依第 7 條載於 [`LICENSING.md`](LICENSING.md)、[`NOTICE.md`](NOTICE.md) 之附加條款。
2. **專有／商業授權（Commercial License）**：若企業、電信／二類電信業者、客服外包商、SaaS 雲端平台、硬體設備商或任何商業營運者不願或無法依 `AGPL-3.0-only` 公開完整衍生原始碼，**必須**事先向原作者（**h4lo-void**）購買並簽署書面商業授權。

### 一、免費社群授權（`AGPL-3.0-only`）核心義務

本專案僅以 **AGPL 第 3.0 單一版本（`AGPL-3.0-only`）** 授權，不授予第 14 條「或任何後續版本」之選項；是否採納其他授權版本，由著作權人 **h4lo-void** 單獨決定。依 `AGPL-3.0-only` 使用、修改或散布本軟體者，須遵守以下義務：

- **網路與電信互動之開源義務（AGPL-3.0 第 13 條）：** 只要你修改了 Bageo 任何部分（包含 Bageo 程式、語音與防詐邏輯、控制台、手機 App、Root 模組、Xposed 模組或簡訊轉發工具），並讓使用者或第三方透過電腦網路或電信網路與該修改版互動（包含透過控制台、HTTP／WebSocket API、通知機器人、VoIP 閘道，或透過 SIM／電話線路自動接聽與撥打電話、收發簡訊），必須向所有互動對象免費提供修改版之完整對應原始碼（Complete Corresponding Source），且同樣以 `AGPL-3.0-only` 授權。
- **衍生作品與硬體封裝之開源義務（AGPL-3.0 第 4–6 條）：** 任何整合、連結、封裝或預刷本軟體之衍生程式、容器映像檔、客製化 Android ROM 或實體電話盒／語音閘道硬體，於散布或交付時均須整體以 `AGPL-3.0-only` 開放完整原始碼與安裝資訊。
- **第 7 條附加條款（AGPL-3.0 第 7(a)–(f) 條，完整文字見 [`LICENSING.md`](LICENSING.md)）：**
  1. 在適用法律允許的最大範圍內適用補充免責與責任限制（第 7(a) 條）；
  2. 必須保留著作權聲明、免責聲明與原作者署名 **`h4lo-void`**（第 7(b) 條）；
  3. 修改版須明確標示為修改版，不得冒稱為原版（第 7(c) 條）；
  4. 未經書面同意，不得以原作者名義為修改版背書或宣傳（第 7(d) 條）；
  5. 不授予「Bageo」、「八哥」、「bageo-call」之商標與名稱使用權（第 7(e) 條）；
  6. 轉散布者若自行向下游承諾擔保或責任，須賠償原作者與貢獻者因此直接承受之責任（第 7(f) 條）。

### 二、何時需要取得商業授權

凡屬下列任一情境，且營運商或廠商**不願依 `AGPL-3.0-only` 公開完整對應原始碼**者，均須事先向 **h4lo-void** 取得書面商業授權：

1. **雲端代管／SaaS／商業代接與代打服務：** 將修改或整合後之 Bageo 作為閉源之商業 AI 電話秘書、企業雲端總機、客服外包系統、預約平台或多租戶代管服務營運。
2. **閉源商業軟體或硬體設備封裝銷售：** 將 Bageo 嵌入、連結、打包或預刷於閉源商業軟體、專有韌體、客製化手機或商業硬體設備中銷售、出租或交付客戶。
3. **OEM／白牌品牌授權與客製授權條款：** 需在商業產品中使用客製品牌、調整開源介面標示，或需要企業採購合規所需之專有商業授權書。

未取得有效書面商業授權而不履行 `AGPL-3.0-only` 義務者，其依 `AGPL-3.0-only` 取得之權利依該授權第 8 條處理。

### 三、商業授權可包含之內容

商業授權之實際內容以雙方簽署之書面合約為準；本文件不構成要約或任何承諾。可洽談之項目包含：

- **閉源使用權：** 於合約範圍內使用、修改及整合 Bageo，無須依 `AGPL-3.0-only` 公開你的原始碼。
- **官方技術支援與服務水準（SLA）：** 由原作者提供之技術支援；回應時間、支援管道與服務範圍載明於合約。
- **授權與智慧財產權之書面文件：** 供企業法遵、採購與稽核使用之授權證明，以及著作權人有權授予該授權之書面保證。外部貢獻之程式碼均依[貢獻者授權協議](CLA.md)取得授權，故整份程式碼皆可納入商業授權。

部署是否符合當地電信、通話錄音、AI 揭露與個人資料法規，仍由營運者自行負責（見 [`LEGAL.md`](LEGAL.md)）；商業授權合約得約定於違法使用時終止該商業授權。

### 四、如何洽詢商業授權

請透過 GitHub 聯絡著作權人 **[@h4lo-void](https://github.com/h4lo-void)**，並附上：

- 公司／法人全銜、註冊國家／地區與官方網站；
- 預計應用場景（SaaS 服務、企業部署、硬體預載或 OEM 整合）；
- 預計規模（門號線路數、設備台數或商業用戶規模）。

---

## English

Copyright (C) 2026 **h4lo-void** (<https://github.com/h4lo-void>). All rights reserved.

**Bageo (`bageo-call`)** is distributed under a **Dual-Licensing Model** (see [`LICENSING.md`](LICENSING.md)):

1. **Open-Source Community License — GNU Affero General Public License v3.0 Only (`AGPL-3.0-only`):** Free for personal, academic, research, and open-source community use in compliance with [`LICENSE`](LICENSE) (verbatim GNU AGPL v3.0 text) and the Section 7 additional terms in [`LICENSING.md`](LICENSING.md) and [`NOTICE.md`](NOTICE.md).
2. **Commercial / Proprietary License:** Available via separate paid written agreement with the copyright holder (**h4lo-void**) for companies, telecom/VoIP operators, call-center providers, SaaS vendors, hardware OEMs, or commercial entities that do not wish to disclose their source code under `AGPL-3.0-only`.

### 1. Core Obligations under `AGPL-3.0-only`

Bageo is licensed under Version 3 of the GNU Affero General Public License **only** (`AGPL-3.0-only`); the Section 14 "or any later version" option is not granted and is reserved solely to **h4lo-void**.

- **Remote Network & Telephony Interaction (Section 13):** Modifying Bageo and allowing users or third parties to interact with the modified program over a computer or telecommunications network (including the web console, HTTP/WebSocket APIs, chat/notification bots, VoIP bridges, or automated cellular/PSTN calls and text messages) requires offering the Complete Corresponding Source Code free of charge under `AGPL-3.0-only`.
- **Copyleft on Bundled & Hardware Distributions (Sections 4–6):** Distributing Bageo inside software bundles, containers, custom Android ROMs, or pre-flashed hardware appliances requires licensing the combined work under `AGPL-3.0-only` with full source code and Installation Information.
- **Section 7 Additional Terms (Sections 7(a)–(f), full text in [`LICENSING.md`](LICENSING.md)):** Supplemental warranty disclaimer and liability limitation to the maximum extent permitted by applicable law (7(a)); preservation of copyright notices and `h4lo-void` attribution (7(b)); clear marking of modified versions without misrepresenting origin (7(c)); no endorsement without written permission (7(d)); no trademark grant in **Bageo**, **八哥**, or **bageo-call** (7(e)); and indemnification of the author and contributors by redistributors who assume contractual warranties or liability toward recipients (7(f)).

### 2. When a Commercial License Is Needed

A separate signed **Commercial License** from **h4lo-void** is required before:

- Operating a modified or integrated Bageo system as a closed-source hosted SaaS, commercial AI receptionist, call-center platform, or managed telephony service;
- Embedding, linking, or shipping Bageo components inside closed-source software, proprietary firmware, or commercial hardware devices; or
- White-label / OEM distribution or custom enterprise licensing.

Failure to comply with `AGPL-3.0-only` without a valid written Commercial License is governed by Section 8 of `AGPL-3.0-only`. To inquire about a Commercial License, contact **[@h4lo-void](https://github.com/h4lo-void)** with your organization details, intended product model, and deployment scale.

### 3. What a Commercial License Can Include

The signed written agreement decides what a Commercial License contains; nothing in this file is an offer or a promise. Items that can be agreed include:

- **Closed-source use:** Using, modifying, and integrating Bageo within the agreed scope without publishing your source code under `AGPL-3.0-only`.
- **Official technical support with a service level (SLA):** Support from the original author, with response times, contact channels, and scope written into the agreement.
- **Written license and intellectual-property assurances:** Proof of license for compliance, procurement, and audit reviews, and the copyright holder's written assurance of having the right to grant the license. Outside contributions come in under the [Contributor License Agreement](CLA.md), so the whole code base can be covered by a Commercial License.

Compliance with local telecom, call-recording, AI-disclosure, and personal-data laws remains the operator's sole responsibility (see [`LEGAL.md`](LEGAL.md)); a Commercial License agreement may provide for termination upon unlawful use.

---

## 简体中文

版权所有 (C) 2026 **h4lo-void**（<https://github.com/h4lo-void>）。保留所有权利。

**Bageo（八哥，`bageo-call`）** 采用**双重授权（Dual Licensing）**模式发布（详见 [`LICENSING.md`](LICENSING.md)）：

1. **社区开源授权 — GNU Affero 通用公共许可证第 3.0 版（仅限 `AGPL-3.0-only`）**：免费提供给个人、学术研究及履行开源义务的社区用户，须遵守 [`LICENSE`](LICENSE)（GNU AGPL v3.0 正式全文）及 [`LICENSING.md`](LICENSING.md)、[`NOTICE.md`](NOTICE.md) 中的第 7 条附加条款。
2. **专有／商业授权（Commercial License）**：若企业、电信／增值电信运营商、呼叫中心、SaaS 平台、硬件厂商或商业运营者不愿按 `AGPL-3.0-only` 公开完整衍生源代码，**必须**事先向原作者（**h4lo-void**）购买并签署书面商业授权。

### 一、免费社区授权（`AGPL-3.0-only`）核心义务

- **网络与电信交互强制开源（第 13 条）：** 只要修改了 Bageo 并允许他人通过计算机网络或电信网络与之交互（含控制台、HTTP／WebSocket API、机器人通知、VoIP 或实卡电话自动接打与短信收发），必须向所有交互对象免费提供完整对应源代码并按 `AGPL-3.0-only` 授权。
- **衍生品与硬件封装整体开源（第 4–6 条）：** 任何集成或预装本软件的应用、镜像、定制 ROM 或硬件盒子，分发时须整体按 `AGPL-3.0-only` 开放全部源代码与安装信息。
- **第 7 条附加条款（第 7(a)–(f) 条，完整文字见 [`LICENSING.md`](LICENSING.md)）：** 在适用法律允许的最大范围内适用补充免责与责任限制；保留 `h4lo-void` 版权署名与免责声明；修改版须明确标示且不得冒称原版；未经书面同意不得用原作者名义背书宣传；不授予“Bageo”、“八哥”、“bageo-call”商标与名称使用权；向下游承诺担保或责任的转分发者须赔偿原作者与贡献者因此直接承受的责任。

### 二、取得商业授权

如需闭源运营商业 SaaS／云总机／代接代打服务、闭源集成至商业软件或硬件设备销售，或获取 OEM 白牌授权，请事先通过 GitHub 联系版权人 **[@h4lo-void](https://github.com/h4lo-void)** 商谈书面商业授权（注明企业全称、应用场景及线路／设备规模）。未取得书面商业授权而不履行 `AGPL-3.0-only` 义务的，按 AGPL-3.0 第 8 条处理。

### 三、商业授权可以包含的内容

商业授权的实际内容以双方签署的书面合同为准；本文件不构成要约或任何承诺。可以商谈的项目包括：

- **闭源使用权：** 在合同范围内使用、修改及集成 Bageo，无须按 `AGPL-3.0-only` 公开你的源代码。
- **官方技术支持与服务水平（SLA）：** 由原作者提供的技术支持；响应时间、支持渠道与服务范围写入合同。
- **授权与知识产权的书面文件：** 供企业合规、采购与审计使用的授权证明，以及版权人有权授予该授权的书面保证。外部贡献的代码均依[贡献者许可协议](CLA.md)取得许可，因此整份代码都可以纳入商业授权。

部署是否符合当地电信、通话录音、AI 披露与个人信息法规，仍由运营者自行负责（见 [`LEGAL.md`](LEGAL.md)）；商业授权合同可约定在违法使用时终止该商业授权。

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
