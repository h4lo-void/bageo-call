# Bageo 詐騙電話話術詞庫與資料庫工具 (`tools/scam_lexicon`)

本目錄維護 Bageo 支援的 9 種主要語言（`zh-TW`、`zh-CN`、`en`、`ja`、`ko`、`es`、`fr`、`de`、`pt`）的詐騙電話話術分類、原文例句、關鍵字與公部門反詐來源，並提供自動匯出與同步至內建資料庫 `crates/agent/data/scam_db.json` 的流程。

## 目錄結構

- `data/categories.yaml`：共用的 32 個詐騙分類定義（說明、通話流程、偵測訊號、合法來電對照、對應 `scam_db.json` 的 `bageo_typology`）
- `data/lang/<lang>.yaml`：各語言例句、強關鍵字（`strong`，權重 2）、一般關鍵字（`keywords`，權重 1）、詐騙專屬要求（`hard`）、施壓手法與來源網址
- `data/gaps.md`：語言覆蓋範圍與維護說明
- `build.py`：建置與同步腳本

## 擴充與更新流程

1. 在 `data/lang/<lang>.yaml` 新增或更新該語言的例句與關鍵字（標記在 `hard` 清單中的詞代表「只有詐騙才會提出的要求」，如索取驗證碼、安裝遠端軟體、轉入監管／安全帳戶、ATM 解除分期等）。
2. 執行建置與同步：
   ```bash
   python3 tools/scam_lexicon/build.py --sync-db
   ```
   腳本會自動排除重複詞、過短的單字與日常快遞／水電／銀行通知常見的通用名詞，並避開會遮蔽短關鍵字計分的冗長超字串。
3. 執行 Rust 單元測試驗證資料庫結構、多語言比對索引與零誤判不變量：
   ```bash
   cargo test --manifest-path crates/agent/Cargo.toml scam_db
   cargo test --manifest-path crates/agent/Cargo.toml stall
   ```

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
