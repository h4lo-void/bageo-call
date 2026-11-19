# Country Scam Intelligence Updater (`tools/scam_intel`)

Updates and validates [`crates/agent/data/country_scams.json`](../../crates/agent/data/country_scams.json) before releases using Gemini API with Google Search grounding (`google_search`).

## Supported Countries
`TW`, `CN`, `US`, `GB` (`UK`), `JP`, `KR`, `DE`, `ES`, `FR`, `BR`, `PT`, `ID` (`+62` cross-border), `IT`, `IN`.

## Commands

1. **Recommended (Zero API Key — AI Assistant Built-in Web Search + `--import-json`)**:
   Ask your coding assistant to search the latest local phone scams for the supported countries and pipe the findings into:
   ```bash
   python3 tools/scam_intel/update_country_scams.py --import-json -
   ```
   The script sanitizes every entry through all four safety gates, merges into `country_scams.json`, and runs `cargo test -p agent` (auto-rolling back on any failure).

2. **Offline schema + false-positive guard + Rust unit tests**:
   ```bash
   python3 tools/scam_intel/update_country_scams.py --validate-only
   ```

3. **Optional standalone CLI search (using `GEMINI_API_KEY` env var)**:
   ```bash
   GEMINI_API_KEY=... python3 tools/scam_intel/update_country_scams.py --countries TW,JP,US --dry-run
   ```

## Safety Gates
- **Zero Call-Latency Impact**: Normal calls keep the exact same Gemini Live prompt size; keywords use a precomputed two-character bigram index (`< 0.02 ms` per utterance); regional review runs asynchronously in the background with a `<= 780` char cap.
- **Anti-False-Positive Guard**: Bare company/utility names (`台電`, `Amazon`, `Endesa`, `Enedis`, `PIX`, etc.) are restricted to `context_keywords` (`weight=1`), and every `red_flag_keyword` is tested against a multi-language normal-call corpus.
- **Stall-Safe Bait Sanitizer**: Rejects tactics mentioning anti-fraud hotlines (`165`, `110`, `112`, `911`) or hanging up so scammers are kept on the line instead of hanging up early.
- **Automatic Rollback**: Restores the previous `country_scams.json` if `cargo test -p agent` fails.

<!-- SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call> -->
