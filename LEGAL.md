# 法律免責聲明與使用須知 / Legal Disclaimer & Usage Notice

[繁體中文](#繁體中文) · [English](#english) · [简体中文](#简体中文)

---

## 繁體中文

本文件說明 Bageo（八哥，`bageo-call`）之免責聲明、風險提示與使用須知。本文件不構成法律意見；如有法規疑問，請諮詢合格律師。授權條款見 [`LICENSE`](LICENSE)、[`LICENSING.md`](LICENSING.md) 與 [`COMMERCIAL_LICENSE.md`](COMMERCIAL_LICENSE.md)。

### 1. 按現狀提供與自架性質

- **按現狀提供（AS IS）：** 在適用法律允許的最大範圍內，Bageo 依「現狀」提供，原作者（**h4lo-void**）與貢獻者不提供任何明示、默示或法定擔保（包含適售性、特定目的適用性、持續可用性、正確性或法規合規保證），亦不對因安裝、設定或使用本軟體所生之任何直接或間接損失、通話與漫遊費、API 帳單、設備損壞、漏接來電、行政罰鍰或第三方爭議負責（另見 [`LICENSE`](LICENSE) 第 15–16 條與 [`LICENSING.md`](LICENSING.md) 第 7(a) 條）。
- **作者不維運服務、軟體不回傳資料：** Bageo 為供使用者自行架設之軟體。原作者不提供或維運任何公共伺服器、電話轉接閘道或代管服務。本軟體不含遙測或資料回傳機制，不會將你的通話音訊、錄音、逐字稿、簡訊、聯絡人或金鑰傳送給原作者；資料只存放在你自己的設備，並依你的設定傳送至你自行選擇的第三方服務（如 Google Gemini、摘要服務與通知管道）。

### 2. 部署者自行負責遵守當地法律

安裝或執行 Bageo 的人，須自行查明並遵守自己所在地及通話對方所在地之適用法律與電信契約，包含但不限於：

- **通話錄音與個人資料保護：** 確認是否須在錄音前告知對方或取得全體通話者同意（例如雙邊／全體同意錄音地區之規定，以及 GDPR、台灣《個人資料保護法》等資料保護法規），並妥善管理錄音與逐字稿之保存與刪除。
- **AI 身分揭露與外撥電話規範：** 確認當地對自動化語音、合成語音揭露及外撥電話之規定（例如美國 TCPA 與 FCC 合成語音規範、歐盟《人工智慧法》透明義務及各國電信法規）。
- **技術、硬體與緊急通訊風險：** 語言模型具機率性，可能聽錯、幻覺、誤判推銷／詐騙或講錯話；解鎖手機 Bootloader 與 Root 可能導致保固失效、設備故障或觸發電信風控；請勿將本軟體用於唯一求救、醫療照護或生命安全相關門號，亦請勿將未加密之控制台直接暴露於網際網路。

### 3. 正當使用須知

請將 Bageo 用於合法之個人或經授權的通訊管理。作為守法提醒與使用須知（非開源著作權授權之額外限制條件），請勿將本軟體用於以下違法或不當行為：

- 電信詐騙、金融詐財、網路釣魚，或冒充政府機關、金融機構、企業與他人身分；
- 違反當地法律之竊聽、未經同意錄音，或擅自散佈他人通話內容；
- 電話騷擾、惡意占線、濫發垃圾簡訊、未經同意之商業電話行銷或大量自動撥號（Robocalling）；
- 未經許可架設商業話務轉接機房（如 SIM Box／貓池），或干擾緊急報案救難專線。

開源授權（[`LICENSE`](LICENSE)）僅授予著作權與專利上之軟體權利，並不豁免任何使用者遵守當地刑法、電信法與隱私法規之義務。商業授權（[`COMMERCIAL_LICENSE.md`](COMMERCIAL_LICENSE.md)）另受雙方簽署之書面合約約束。

本文件以繁體中文版為準；英文版與簡體中文版為翻譯參考。

---

## English

This document sets out general disclaimers, operational risks, and usage notices for Bageo (`bageo-call`). Nothing here is legal advice; consult a qualified lawyer if you have questions about applicable law. License terms are in [`LICENSE`](LICENSE), [`LICENSING.md`](LICENSING.md), and [`COMMERCIAL_LICENSE.md`](COMMERCIAL_LICENSE.md).

### 1. Provided "AS IS" & Self-Hosted Nature

- **Provided "AS IS":** To the maximum extent permitted by applicable law, Bageo is provided "AS IS" without warranties of any kind (express, implied, or statutory, including merchantability, fitness for a particular purpose, availability, accuracy, or regulatory compliance). Neither the author (**h4lo-void**) nor any contributor is liable for any direct or indirect damages, carrier or roaming charges, cloud API bills, device damage, missed calls, regulatory penalties, or third-party claims arising from installing, configuring, or using the software (see also [`LICENSE`](LICENSE) Sections 15–16 and [`LICENSING.md`](LICENSING.md) Section 7(a)).
- **No hosted service; no data sent to the author:** Bageo is self-hosted software. The author does not operate any public server, call relay, or hosted service. The software contains no telemetry and sends no call audio, recordings, transcripts, text messages, contacts, or keys back to the author; data stays on your own equipment and goes only to the third-party services you configure (such as Google Gemini, your summary provider, and your notification channels).

### 2. Deployer Responsibility for Local Law

Whoever deploys or runs Bageo is solely responsible for finding out and complying with the laws and carrier terms that apply where they and the other party to a call are located, including:

- **Call recording & data protection:** Checking whether notice or two-party / all-party consent is required before recording a call, and complying with applicable privacy and data-protection laws (such as GDPR, California CIPA, or local privacy statutes).
- **AI disclosure & outbound calling rules:** Complying with rules on automated calls, synthetic/AI voice disclosure, and telemarketing (such as the US TCPA and FCC AI-voice rules, EU AI Act transparency duties, and national telecom regulations).
- **Technical, hardware & emergency-call risks:** Language models are probabilistic and can mishear, hallucinate, misclassify normal or scam calls, or make unintended statements. Unlocking bootloaders and rooting phones can void warranties, damage hardware, or trigger carrier anti-fraud controls. Never use Bageo on an emergency, medical, or life-safety phone line, and never expose the plain-HTTP console directly to the public internet.

### 3. Responsible-Use Notice

As a compliance reminder and usage notice (and not as an additional condition on the open-source license), do not use Bageo for unlawful activities such as:

- Telecom or financial fraud, phishing, or impersonating government agencies, banks, businesses, or other people;
- Unlawful wiretapping, recording without legally required consent, or unlawfully sharing private call recordings;
- Harassment, spam text messages, unconsented telemarketing, or mass automated dialing (robocalling);
- Operating unlicensed commercial SIM boxes / call-termination gateways, or interfering with emergency lines.

Open-source licensing ([`LICENSE`](LICENSE)) grants copyright and patent permissions only and does not exempt any user from criminal, telecommunications, wiretapping, or privacy laws. Any separate Commercial License ([`COMMERCIAL_LICENSE.md`](COMMERCIAL_LICENSE.md)) is governed by its signed written contract.

The Traditional Chinese text of this document governs; the English and Simplified Chinese sections are translations for convenience.

---

## 简体中文

本文件说明 Bageo（八哥，`bageo-call`）的免责声明、风险提示与使用须知。本文件不构成法律意见；如有法规疑问，请咨询合格律师。许可证条款见 [`LICENSE`](LICENSE)、[`LICENSING.md`](LICENSING.md) 与 [`COMMERCIAL_LICENSE.md`](COMMERCIAL_LICENSE.md)。

### 1. 按现状提供与自托管性质

- **按现状提供（AS IS）：** 在适用法律允许的最大范围内，Bageo 按“现状”提供，原作者（**h4lo-void**）与贡献者不提供任何明示、默示或法定担保（包括适销性、特定用途适用性、持续可用性、准确性或法规合规保证），也不对因安装、配置或使用本软件产生的任何直接或间接损失、通话与漫游费、API 账单、设备损坏、漏接来电、行政罚款或第三方争议负责（另见 [`LICENSE`](LICENSE) 第 15–16 条与 [`LICENSING.md`](LICENSING.md) 第 7(a) 条）。
- **作者不运维服务、软件不回传数据：** Bageo 是供用户自行部署的软件。原作者不提供或运维任何公共服务器、电话中继网关或托管服务。本软件不含遥测或数据回传机制，不会把你的通话音频、录音、文字记录、短信、联系人或密钥发送给原作者；数据只保存在你自己的设备上，并按你的设置发送至你自行选择的第三方服务（如 Google Gemini、摘要服务与通知渠道）。

### 2. 部署者自行负责遵守当地法律

安装或运行 Bageo 的人，须自行查明并遵守自己所在地及通话对方所在地的适用法律与运营商协议，包括但不限于：

- **通话录音与个人信息保护：** 确认录音前是否需要告知对方或取得全体通话方同意（例如双方／全体同意录音地区的规定，以及 GDPR、个人信息保护法规等），并妥善管理录音与文字记录的保存与删除。
- **AI 身份披露与外呼规范：** 确认当地对自动化语音、合成语音披露及外呼电话的规定（例如美国 TCPA 与 FCC 合成语音规范、欧盟《人工智能法》透明义务及各国电信法规）。
- **技术、硬件与紧急通信风险：** 语言模型具有概率性，可能听错、产生幻觉、误判推销／诈骗或说错话；解锁手机 Bootloader 与 Root 可能导致失去保修、设备故障或触发运营商风控；请勿将本软件用于唯一求救、医疗看护或生命安全相关号码，也请勿将未加密的控制台直接暴露到公网。

### 3. 正当使用须知

请将 Bageo 用于合法的个人或经授权的通信管理。作为守法提醒与使用须知（非开源著作权许可的附加限制条件），请勿将本软件用于以下违法或不当行为：

- 电信诈骗、金融诈骗、网络钓鱼，或冒充政府机关、金融机构、企业与他人身份；
- 违反当地法律的窃听、未经同意录音，或擅自散布他人通话内容；
- 电话骚扰、恶意占线、滥发垃圾短信、未经同意的商业电话营销或批量自动拨号（Robocalling）；
- 未经许可架设商业话务中继机房（如 SIM Box／猫池），或干扰紧急报警救援专线。

开源许可证（[`LICENSE`](LICENSE)）仅授予著作权与专利上的软件权利，并不豁免任何使用者遵守当地刑法、电信法与隐私法规的义务。商业授权（[`COMMERCIAL_LICENSE.md`](COMMERCIAL_LICENSE.md)）另受双方签署的书面合同约束。

本文件以繁体中文版为准；英文版与简体中文版为翻译参考。

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
