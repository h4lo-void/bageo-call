## 語言覆蓋

- **只做了 9 種正式支援語言**。內建 `scam_db.json` 另外有 18 種只用來比對關鍵字的語言（ar、bn、hi、id、it、mr、ms、nl、pl、ro、ru、ta、te、th、tl、tr、uk、vi），這份詞庫**沒有**收。如果要擴充，建議優先做 vi、id、th、tl：這些國家的反詐單位資料多，而且在台灣的移工也常被詐騙鎖定。
- **各語言的深度不一樣**：zh-TW 最完整（24 類）；de（11 類）、ko（11 類）、fr（12 類）、pt（12 類）、es（13 類）主要只收當地最常見的手法，有些分類每類只有 2–3 句。
- **港澳粵語**放在 zh-TW 底下，標成 [HK]（因為 bageo 的 han 比對本來就會先轉成繁體）。**新加坡華語**放在 zh-CN 底下。
- **葡萄牙文**以巴西用語為主（bageo 的 pt 介面是 pt-BR），葡萄牙本地的說法標成 [PT]。**西班牙文**混合了墨西哥／阿根廷和西班牙的說法，有加地區標記。

## 分類與 bageo 對照

- **感情詐騙（romance）**：bageo 沒有對應的 typology，所以沒有匯出到 `scam_db.local.json`。建議新增 `romance_scam` typology。
- **機器人語音按鍵（robocall_ivr）**和 **「Can you hear me?」錄 yes 詐騙（yes_recording）**：比較適合用 bageo 既有的 `ivr_recording` 判斷和號碼規則處理，所以只收在 lexicon.json，沒有匯出。
- **多個分類共用一個 typology**：family_emergency、virtual_kidnapping、ai_voice_clone、boss_acquaintance 都對應到 `fake_relative_money`；police_prosecutor 對應 `fake_official`。
- **hard 標記很保守**：只有當 typology 本身是 hard、而且這個詞是「只有詐騙才會提出的要求」（唸驗證碼、裝遠端軟體、安全帳戶、交卡等）時才會標成 hard。「保證獲利」這類宣傳用語只給權重 2，不標 hard。
- **權重 1 的詞比較通用**（例如 bank、police、mama）：它們只會加權，不會單獨觸發判斷；但仍建議先用實際通話紀錄測一次誤判率，再整份匯入。

## 來源查核

- 2026-11-16（Asia/Taipei）用 curl 檢查了全部 94 個來源網址。大部分回應 200。以下網站會擋 curl（回 403、逾時或 5xx），但內容已經在研究時用其他方式讀過或用搜尋確認過：
  - fcc.gov（403）
  - canada.ca 英文 CRA（逾時，WebFetch 可以讀）
  - elpais.com（403）
  - osi.es（逾時）
  - police.pref.nagasaki.jp（逾時）
  - pm.ro.gov.br（403）
  - sicnoticias.pt（逾時；同一則 PSP 新聞已經用 noticiasaominuto.com 交叉確認）
  - cj.gov.cn（逾時）
  - jyj.shiyan.gov.cn（500）
  - phc.moj.gov.tw PDF（逾時）
- **可能已經失效或搬家**：
  - thepaper.cn（`newsDetail_forward_13809946`，curl 回 404，搜尋引擎還有索引）
  - ga.wuxi.gov.cn（`2023/01/30/3871815`，回 404）
  - 引用這兩個來源的例句都是改寫。
- bcb.gov.br 和 consumer.ftc.gov 的部分頁面會擋自動化請求，所以 FTC 改引用 Data Spotlight PDF。

## 例句性質

- 大部分例句是依照官方宣導內容**改寫或綜合**的典型說法，只有少數標成「原文」。原文例句只出現在官方或新聞明確用引號引用騙徒說法的地方。
- 詐騙話術變化很快，尤其是 AI 語音複製、假檢警視訊、投資群組這幾類。建議每季對照 165 打詐儀錶板、警察廳、FTC、Action Fraud、警察庁 SOS47 等來源更新一次。

## 關鍵字命中率（自我測試，樣本內）

用 Python 模擬 bageo 的比對規則（han／word／run），測試每一句例句至少命中一個關鍵字的比例：

| 語言 | 只用內建 DB | 內建 + 本詞庫 |
|---|---|---|
| zh-TW | 59% | 87% |
| zh-CN | 56% | 91% |
| en | 49% | 86% |
| ja | 47% | 88% |
| ko | 40% | 94% |
| es | 43% | 90% |
| fr | 34% | 94% |
| de | 28% | 90% |
| pt | 55% | 94% |

注意：這些關鍵字是從同一批例句整理出來的，所以這個數字是**樣本內**的上限，不代表實際通話的命中率。另外，`out/scam_db.local.json` 已經用 bageo 自己的 `scam_db::parse_local` 實際載入測試過：1246 個詞全部成功載入，0 個被跳過，其中 74 個是 hard。

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
