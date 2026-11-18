#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

"""Update and validate Bageo's country-specific phone scam database.

Uses the Gemini API with Google Search grounding (`google_search`) to look up
recent official anti-fraud advisories for each supported country, filters all
candidates through four anti-false-positive & stall-safety gates, merges them
additively into `crates/agent/data/country_scams.json`, and runs
`cargo test -p agent` (rolling back automatically if any test fails).

Usage:
  # Offline validation + Rust test suite:
  python3 tools/scam_intel/update_country_scams.py --validate-only

  # Preview latest local scam updates via Gemini Google Search (no file write):
  GEMINI_API_KEY=... python3 tools/scam_intel/update_country_scams.py --countries TW,JP --dry-run

  # Update all 12 countries before a release and verify with cargo test:
  GEMINI_API_KEY=... python3 tools/scam_intel/update_country_scams.py
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[2]
SCAM_DB_PATH = REPO_ROOT / "crates" / "agent" / "data" / "scam_db.json"
COUNTRY_DB_PATH = REPO_ROOT / "crates" / "agent" / "data" / "country_scams.json"

DEFAULT_MODELS = ["gemini-2.5-flash", "gemini-2.0-flash"]

COUNTRY_SEARCH_HINTS: dict[str, str] = {
    "TW": "台灣 165全民防騙網 打詐儀錶板 刑事警察局 Whoscall 最新電話詐騙手法",
    "CN": "中国 国家反诈中心 公安部刑侦局 最新电信网络诈骗手法",
    "US": "United States FTC FCC FBI IC3 Hiya latest phone call scams",
    "GB": "United Kingdom Action Fraud Ofcom UK Finance HMRC latest phone scams",
    "JP": "日本 警察庁 SOS47 国民生活センター 特殊詐欺 ニセ警察 最新手口",
    "KR": "대한민국 금융감독원 경찰청 보이스피싱 신종 수법 주의보",
    "DE": "Deutschland Polizei-Beratung Bundesnetzagentur Verbraucherzentrale Telefonbetrug aktuelle Maschen",
    "ES": "España INCIBE OCU Policía Nacional Guardia Civil estafas telefónicas recientes",
    "FR": "France Cybermalveillance DGCCRF Banque de France arnaques téléphoniques récentes",
    "BR": "Brasil Febraban Anatel Banco Central golpes telefônicos PIX recentes",
    "PT": "Portugal Polícia Judiciária Banco de Portugal SIBS burlas telefónicas MB WAY recentes",
    "ID": "Indonesia OJK Satgas PASTI Komdigi penipuan telepon terbaru",
}

ANTI_STALL_MARKERS = (
    "165",
    "110",
    "112",
    "911",
    "報警",
    "报警",
    "掛斷",
    "挂断",
    "警察不會",
    "警察不会",
    "警察は電話で",
    "검찰은 전화로",
    "hang up",
    "auflegen",
    "call the official",
    "llamar al número oficial",
    "appeler le numéro officiel",
    "ligar para o número oficial",
)

GENERIC_CONTEXT_ONLY = {
    "bank", "amazon", "microsoft", "apple", "police", "tax", "water", "power", "electricity",
    "銀行", "银行", "台電", "台水", "電費", "水費", "包裹", "郵局", "中華郵政", "健保",
    "警察", "検察", "市役所", "검찰", "경찰", "endesa", "iberdrola", "enedis", "pix",
    "mb way", "mbway", "ojk", "pajak", "hmrc", "medicare", "irs", "zelle", "bizum",
}

POLITE_NORMAL_CORPUS: list[str] = [
    "您好，這裡是自來水公司，溫馨提醒您這期水費記得在月底前到超商或官方App繳納喔，祝您順心再見",
    "您好，這裡是台電客服，通知您明天上午九點巷口進行例行線路維護，謝謝配合",
    "您好，我是玉山銀行風控中心，跟您核對剛剛一筆超商兩百元刷卡是您本人消費嗎？好的謝謝您",
    "您好，物业中心提醒您本月的物业费和水费账单已经发到信箱了，月底前缴纳即可",
    "Hi, calling from City Water with a friendly reminder that your monthly water bill is due next Friday. Have a wonderful day!",
    "Hi, this is Dr. Miller's dental clinic confirming your cleaning appointment tomorrow at 2 PM.",
    "水道局からのお知らせです。今月分の水道料金のお支払い期限は来週金曜日となっております。失礼いたします。",
    "관리사무소입니다. 이번 달 수도요금 납부 기한이 다음 주 금요일까지라 안내드립니다. 좋은 하루 되세요.",
    "Hola, le llamamos de la clínica dental para recordar su cita mañana a las diez. Que tenga un excelente día.",
    "Bonjour, c'est le cabinet dentaire pour vous rappeler votre rendez-vous de demain à quatorze heures. Excellente journée.",
    "Guten Tag, hier ist die Zahnarztpraxis zur Erinnerung an Ihren Termin morgen um zehn Uhr. Einen schönen Tag noch.",
    "Olá, aqui é da clínica odontológica apenas lembrando da sua consulta amanhã às três da tarde. Tenha um ótimo dia.",
    "Halo, kami dari klinik gigi mengingatkan jadwal kontrol Anda besok pukul sepuluh pagi. Terima kasih.",
]

VALID_ID_RE = re.compile(r"^[a-z0-9_]+$")
YEAR_RANGE_RE = re.compile(r"\b(?:19|20)\d{2}\s*[-–/]\s*(?:19|20)\d{2}\b")
PHONE_NUMBER_RE = re.compile(r"(?:\+?\d[\d\s\-()]{5,}\d)")
EMAIL_RE = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}")
MAX_CONTEXT_KEYWORDS = 24
MAX_RED_FLAG_KEYWORDS = 16


def has_pii_or_phone(text: str) -> bool:
    """Reject phone-like sequences (>= 7 digits, excluding year ranges) and email addresses."""
    cleaned = YEAR_RANGE_RE.sub(" ", text)
    if EMAIL_RE.search(cleaned):
        return True
    for m in PHONE_NUMBER_RE.finditer(cleaned):
        if sum(ch.isdigit() for ch in m.group(0)) >= 7:
            return True
    return False


def normalize_word_kw(text: str) -> str:
    cleaned = text.lower().replace("’", "'")
    parts = [p.strip("'") for p in re.split(r"[^\w']+", cleaned, flags=re.UNICODE) if p.strip("'")]
    return " ".join(parts)


def load_base_schema() -> tuple[dict[str, bool], dict[str, str]]:
    raw = json.loads(SCAM_DB_PATH.read_text(encoding="utf-8"))
    typologies = {t["id"]: bool(t["hard"]) for t in raw["typologies"]}
    lang_modes = {g["lang"]: g["match"] for g in raw["keywords"]}
    return typologies, lang_modes


def hits_normal_corpus(kw: str, mode: str) -> bool:
    needle = f" {normalize_word_kw(kw)} " if mode == "word" else kw.strip().lower()
    if not needle.strip():
        return True
    for line in POLITE_NORMAL_CORPUS:
        hay = f" {normalize_word_kw(line)} " if mode == "word" else line.lower()
        if needle in hay:
            return True
    return False


def sanitize_scam_entry(
    scam: dict[str, Any],
    typologies: dict[str, bool],
    lang_modes: dict[str, str],
) -> tuple[dict[str, Any] | None, list[str]]:
    notes: list[str] = []
    sid = str(scam.get("id", "")).strip().lower()
    sid = re.sub(r"[^a-z0-9_]+", "_", sid).strip("_")
    if not sid or not VALID_ID_RE.match(sid):
        return None, [f"dropped scam with invalid id: {scam.get('id')!r}"]

    typ = str(scam.get("typology", "")).strip()
    if typ not in typologies:
        return None, [f"{sid}: unknown typology {typ!r}"]
    typ_hard = typologies[typ]

    lang = str(scam.get("lang", "")).strip()
    if lang not in lang_modes:
        return None, [f"{sid}: unsupported lang {lang!r}"]
    mode = lang_modes[lang]

    signals_en = str(scam.get("signals_en", "")).strip()[:255]
    legit_en = str(scam.get("legit_en", "")).strip()[:255]
    name = str(scam.get("name", "")).strip()
    name_en = str(scam.get("name_en", "")).strip()
    if not (signals_en and legit_en and name and name_en):
        return None, [f"{sid}: missing required descriptive fields"]
    for field_name, val in (("name", name), ("name_en", name_en), ("signals_en", signals_en), ("legit_en", legit_en)):
        if has_pii_or_phone(val):
            return None, [f"{sid}: dropped because {field_name} contains phone/email PII"]

    ctx_out: list[str] = []
    seen_kw: set[str] = set()

    def add_ctx(raw_kw: str) -> None:
        if len(ctx_out) >= MAX_CONTEXT_KEYWORDS:
            return
        val = normalize_word_kw(raw_kw) if mode == "word" else raw_kw.strip()
        if mode in ("word", "run"):
            val = val.lower()
        if has_pii_or_phone(val):
            notes.append(f"{sid}: dropped context_keyword containing phone/email PII")
            return
        min_len = 3 if mode == "word" else 2
        if len(val) >= min_len and val not in seen_kw:
            seen_kw.add(val)
            ctx_out.append(val)

    for kw in scam.get("context_keywords", []):
        if isinstance(kw, str):
            add_ctx(kw)

    rf_out: list[dict[str, Any]] = []
    for rf in scam.get("red_flag_keywords", []):
        if len(rf_out) >= MAX_RED_FLAG_KEYWORDS:
            break
        if not isinstance(rf, dict):
            continue
        raw_text = str(rf.get("text", "")).strip()
        val = normalize_word_kw(raw_text) if mode == "word" else raw_text
        if mode in ("word", "run"):
            val = val.lower()
        if has_pii_or_phone(val):
            notes.append(f"{sid}: dropped red_flag containing phone/email PII")
            continue
        min_len = 5 if mode == "word" else 3
        if len(val) < min_len:
            notes.append(f"{sid}: downgraded short red_flag {val!r} -> context")
            add_ctx(val)
            continue
        if val.lower() in GENERIC_CONTEXT_ONLY:
            notes.append(f"{sid}: downgraded generic brand/term {val!r} -> context")
            add_ctx(val)
            continue
        if hits_normal_corpus(val, mode):
            notes.append(f"{sid}: rejected red_flag {val!r} (matched polite normal-call corpus)")
            continue
        if val in seen_kw:
            continue
        seen_kw.add(val)
        weight = int(rf.get("weight", 2))
        weight = 2 if weight >= 2 else 1
        hard = bool(rf.get("hard", False)) and typ_hard
        rf_out.append({"text": val, "weight": weight, "hard": hard})

    bait_out: list[str] = []
    for bt in scam.get("bait_tactics", []):
        t = str(bt).strip()
        if not t:
            continue
        if has_pii_or_phone(t):
            notes.append(f"{sid}: stripped bait_tactic containing phone/email PII")
            continue
        low = t.lower()
        if any(m in low for m in ANTI_STALL_MARKERS):
            notes.append(f"{sid}: stripped anti-stalling giveaway tactic: {t!r}")
            continue
        bait_out.append(t)

    if not bait_out:
        return None, notes + [f"{sid}: dropped because zero safe bait_tactics remained"]

    prevalence = str(scam.get("prevalence", "high")).strip().lower()
    if prevalence not in ("very_high", "high", "medium"):
        prevalence = "high"

    sources = [str(s).strip() for s in scam.get("sources", []) if str(s).strip() and not has_pii_or_phone(str(s))][:6]

    return {
        "id": sid,
        "typology": typ,
        "name": name,
        "name_en": name_en,
        "prevalence": prevalence,
        "lang": lang,
        "context_keywords": ctx_out,
        "red_flag_keywords": rf_out,
        "signals_en": signals_en,
        "legit_en": legit_en,
        "bait_tactics": bait_out[:4],
        "sources": sources,
    }, notes


def extract_json_object(text: str) -> dict[str, Any]:
    cleaned = text.strip()
    fence = re.search(r"```(?:json)?\s*(\{.*\})\s*```", cleaned, flags=re.DOTALL)
    if fence:
        cleaned = fence.group(1)
    else:
        start = cleaned.find("{")
        end = cleaned.rfind("}")
        if start != -1 and end != -1 and end > start:
            cleaned = cleaned[start : end + 1]
    return json.loads(cleaned)


def query_country_updates(
    api_key: str,
    code: str,
    profile: dict[str, Any],
    typologies: dict[str, bool],
    models: list[str],
) -> tuple[dict[str, Any], list[str]]:
    existing_summary = [
        {"id": s["id"], "typology": s["typology"], "name_en": s["name_en"]}
        for s in profile.get("scams", [])
    ]
    valid_types = [f"{tid} (hard={hard})" for tid, hard in typologies.items()]
    primary_lang = profile.get("languages", ["en"])[0]
    hint = COUNTRY_SEARCH_HINTS.get(code, f"{profile['name']} latest phone call scams official anti-fraud")

    prompt = (
        f"Use Google Search to check the latest (past 90 days / 2025-2026) official anti-fraud advisories "
        f"and high-prevalence phone call scams in {profile['name']} ({code}, calling code {profile['calling_code']}).\n"
        f"Search hint: {hint}\n\n"
        f"Existing tracked scams for {code}: {json.dumps(existing_summary, ensure_ascii=False)}\n"
        f"Allowed canonical typology IDs: {', '.join(valid_types)}\n\n"
        f"Return STRICT JSON ONLY (no markdown prose outside JSON) with shape:\n"
        f'{{"prevalence_note": "updated 1-2 sentence English summary",\n'
        f'  "new_or_updated_scams": [\n'
        f'    {{\n'
        f'      "id": "{code.lower()}_short_snake_id",\n'
        f'      "typology": "one_allowed_typology_id",\n'
        f'      "name": "Local name in {primary_lang}",\n'
        f'      "name_en": "Concise English name",\n'
        f'      "prevalence": "very_high|high|medium",\n'
        f'      "lang": "{primary_lang}",\n'
        f'      "context_keywords": ["institution or topic words (weight 1)"],\n'
        f'      "red_flag_keywords": [{{"text": "specific multi-word scam demand phrase", "weight": 2, "hard": false}}],\n'
        f'      "signals_en": "<=220 chars: what the scammer claims and demands",\n'
        f'      "legit_en": "<=220 chars: what an ordinary legitimate look-alike call sounds like",\n'
        f'      "bait_tactics": ["In {primary_lang}: pretend to cooperate slowly / misplace glasses / app stuck loading to waste scammer time — NEVER mention police hotlines (165/110/112/911), NEVER say police do not ask for money, NEVER say hang up"],\n'
        f'      "sources": ["Official agency or report name"]\n'
        f'    }}\n'
        f'  ]\n'
        f'}}\n'
        f"Include at most 2 genuinely distinct new or newly evolved phone scam patterns for {code}."
    )

    last_err = "no model tried"
    for model in models:
        url = f"https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent"
        body = {
            "contents": [{"role": "user", "parts": [{"text": prompt}]}],
            "tools": [{"google_search": {}}],
            "generationConfig": {"temperature": 0.1, "maxOutputTokens": 2048},
        }
        req = urllib.request.Request(
            url,
            data=json.dumps(body).encode("utf-8"),
            headers={
                "Content-Type": "application/json",
                "x-goog-api-key": api_key,
            },
            method="POST",
        )
        try:
            with urllib.request.urlopen(req, timeout=45) as resp:
                payload = json.loads(resp.read().decode("utf-8"))
            parts = payload.get("candidates", [{}])[0].get("content", {}).get("parts", [])
            text = "\n".join(p.get("text", "") for p in parts if "text" in p)
            parsed = extract_json_object(text)
            chunks = (
                payload.get("candidates", [{}])[0]
                .get("groundingMetadata", {})
                .get("groundingChunks", [])
            )
            grounded_titles = []
            for ch in chunks:
                title = ch.get("web", {}).get("title", "").strip()
                if title and title not in grounded_titles:
                    grounded_titles.append(title)
            return parsed, grounded_titles[:5]
        except urllib.error.HTTPError as e:
            err_body = e.read().decode("utf-8", errors="replace")[:160]
            last_err = f"{model} HTTP {e.code}: {err_body}"
        except Exception as e:  # noqa: BLE001
            last_err = f"{model}: {e}"
    raise RuntimeError(f"Gemini search query failed for {code}: {last_err}")


def merge_scam_into_country(profile: dict[str, Any], clean: dict[str, Any]) -> str:
    for existing in profile["scams"]:
        if existing["id"] == clean["id"]:
            ctx_room = max(0, MAX_CONTEXT_KEYWORDS - len(existing["context_keywords"]))
            added_ctx = [k for k in clean["context_keywords"] if k not in existing["context_keywords"]][:ctx_room]
            existing["context_keywords"].extend(added_ctx)
            existing_rf = {r["text"] for r in existing["red_flag_keywords"]}
            rf_room = max(0, MAX_RED_FLAG_KEYWORDS - len(existing["red_flag_keywords"]))
            added_rf = [r for r in clean["red_flag_keywords"] if r["text"] not in existing_rf][:rf_room]
            existing["red_flag_keywords"].extend(added_rf)
            for bt in clean["bait_tactics"]:
                if bt not in existing["bait_tactics"] and len(existing["bait_tactics"]) < 5:
                    existing["bait_tactics"].append(bt)
            for src in clean["sources"]:
                if src not in existing["sources"] and len(existing["sources"]) < 8:
                    existing["sources"].append(src)
            return f"updated {clean['id']} (+{len(added_ctx)} ctx, +{len(added_rf)} red_flags)"
    profile["scams"].append(clean)
    return f"added new pattern {clean['id']} ({clean['name_en']})"


def validate_database(db: dict[str, Any], typologies: dict[str, bool], lang_modes: dict[str, str]) -> list[str]:
    errors: list[str] = []
    seen_ids: set[str] = set()
    for code, cp in db.get("countries", {}).items():
        for field_name in ("prevalence_note", "cross_border_note"):
            if has_pii_or_phone(cp.get(field_name, "")):
                errors.append(f"{code}: {field_name} contains phone/email PII")
        for sc in cp.get("scams", []):
            sid = sc.get("id", "")
            if sid in seen_ids:
                errors.append(f"{code}: duplicate scam id {sid}")
            seen_ids.add(sid)
            typ = sc.get("typology", "")
            if typ not in typologies:
                errors.append(f"{sid}: unknown typology {typ}")
                continue
            lang = sc.get("lang", "")
            if lang not in lang_modes:
                errors.append(f"{sid}: unknown lang {lang}")
                continue
            if len(sc.get("signals_en", "")) > 260 or len(sc.get("legit_en", "")) > 260:
                errors.append(f"{sid}: signals_en and legit_en must be <= 260 chars")
            for field_name in ("name", "name_en", "signals_en", "legit_en"):
                if has_pii_or_phone(sc.get(field_name, "")):
                    errors.append(f"{sid}: {field_name} contains phone/email PII")
            mode = lang_modes[lang]
            for ck in sc.get("context_keywords", []):
                if has_pii_or_phone(ck):
                    errors.append(f"{sid}: context_keyword {ck!r} contains phone/email PII")
            for rf in sc.get("red_flag_keywords", []):
                if has_pii_or_phone(rf.get("text", "")):
                    errors.append(f"{sid}: red_flag {rf.get('text')!r} contains phone/email PII")
                if rf.get("hard") and not typologies[typ]:
                    errors.append(f"{sid}: red_flag {rf.get('text')!r} is hard on non-hard typology {typ}")
                if hits_normal_corpus(rf.get("text", ""), mode):
                    errors.append(f"{sid}: red_flag {rf.get('text')!r} false-positives normal call corpus")
            for bt in sc.get("bait_tactics", []):
                if has_pii_or_phone(bt):
                    errors.append(f"{sid}: bait_tactic contains phone/email PII: {bt!r}")
                low = bt.lower()
                if any(m in low for m in ANTI_STALL_MARKERS):
                    errors.append(f"{sid}: bait_tactic contains anti-stalling giveaway: {bt!r}")
            for src in sc.get("sources", []):
                if has_pii_or_phone(src):
                    errors.append(f"{sid}: source {src!r} contains phone/email PII")
    return errors


def run_cargo_tests() -> bool:
    cmd = ["cargo", "test", "-p", "agent", "--manifest-path", str(REPO_ROOT / "Cargo.toml"), "scam_db"]
    print(f"[test] Running: {' '.join(cmd)}")
    try:
        proc = subprocess.run(cmd, cwd=str(REPO_ROOT), check=False)
        return proc.returncode == 0
    except OSError as e:
        print(f"[ERROR] Failed to launch cargo test: {e}", file=sys.stderr)
        return False


def main() -> int:
    parser = argparse.ArgumentParser(description="Update and validate Bageo's country_scams.json")
    parser.add_argument("--countries", default="", help="Comma-separated country codes (e.g. TW,JP,US); default = all")
    parser.add_argument("--validate-only", action="store_true", help="Validate existing JSON and run tests offline")
    parser.add_argument(
        "--import-json",
        default="",
        help="Import updates from a JSON file (or '-' for stdin) gathered by the coding agent's built-in web search (no API key needed)",
    )
    parser.add_argument("--dry-run", action="store_true", help="Validate/preview updates without modifying files")
    parser.add_argument("--skip-cargo-test", action="store_true", help="Skip running cargo test after validation/write")
    parser.add_argument("--api-key-env", default="GEMINI_API_KEY", help="Optional env var holding Gemini API key when not using --import-json")
    parser.add_argument("--model", default="", help="Override Gemini model name for Google Search grounding")
    args = parser.parse_args()

    typologies, lang_modes = load_base_schema()
    original_bytes = COUNTRY_DB_PATH.read_bytes()
    db = json.loads(original_bytes.decode("utf-8"))

    errors = validate_database(db, typologies, lang_modes)
    if errors:
        for err in errors:
            print(f"[ERROR] {err}", file=sys.stderr)
        return 1
    total_scams = sum(len(c["scams"]) for c in db["countries"].values())
    print(f"[ok] Validated {len(db['countries'])} countries, {total_scams} regional scam patterns.")

    if args.validate_only:
        if not args.skip_cargo_test and not run_cargo_tests():
            return 1
        return 0

    changes: list[str] = []
    if args.import_json:
        raw_import = sys.stdin.read() if args.import_json == "-" else Path(args.import_json).read_text(encoding="utf-8")
        imported = json.loads(raw_import)
        for code, payload in imported.get("countries", {}).items():
            alias = "GB" if code.upper() == "UK" else code.upper()
            if alias not in db["countries"]:
                print(f"[warn] Unknown country code {code!r}; skipping.")
                continue
            profile = db["countries"][alias]
            if isinstance(payload.get("prevalence_note"), str) and len(payload["prevalence_note"].strip()) >= 20:
                profile["prevalence_note"] = payload["prevalence_note"].strip()[:320]
            for raw_scam in payload.get("new_or_updated_scams", []):
                if not isinstance(raw_scam, dict):
                    continue
                clean, notes = sanitize_scam_entry(raw_scam, typologies, lang_modes)
                for n in notes:
                    print(f"       [gate] {n}")
                if clean is None:
                    continue
                action = merge_scam_into_country(profile, clean)
                changes.append(f"{alias}: {action}")
                print(f"[import] {alias}: {action}")
    else:
        api_key = os.environ.get(args.api_key_env, "").strip()
        if not api_key:
            print(
                f"[ERROR] Provide --import-json <file|-> (recommended with AI assistant built-in web search, no key needed) "
                f"or set {args.api_key_env}.",
                file=sys.stderr,
            )
            return 2

        target_codes = (
            [c.strip().upper() for c in args.countries.split(",") if c.strip()]
            if args.countries.strip()
            else list(db["countries"].keys())
        )
        models = [args.model.strip()] if args.model.strip() else DEFAULT_MODELS

        for code in target_codes:
            alias = "GB" if code == "UK" else code
            if alias not in db["countries"]:
                print(f"[warn] Unknown country code {code!r}; skipping.")
                continue
            profile = db["countries"][alias]
            print(f"[sync] Searching latest official scam advisories for {alias} ({profile['name']})...")
            result, grounded = query_country_updates(api_key, alias, profile, typologies, models)
            if grounded:
                print(f"       Grounded web sources: {', '.join(grounded)}")
            if isinstance(result.get("prevalence_note"), str) and len(result["prevalence_note"].strip()) >= 20:
                profile["prevalence_note"] = result["prevalence_note"].strip()[:320]
            for raw_scam in result.get("new_or_updated_scams", []):
                if not isinstance(raw_scam, dict):
                    continue
                clean, notes = sanitize_scam_entry(raw_scam, typologies, lang_modes)
                for n in notes:
                    print(f"       [gate] {n}")
                if clean is None:
                    continue
                action = merge_scam_into_country(profile, clean)
                changes.append(f"{alias}: {action}")
                print(f"       [merge] {action}")

    post_errors = validate_database(db, typologies, lang_modes)
    if post_errors:
        for err in post_errors:
            print(f"[ERROR] Post-merge validation failed: {err}", file=sys.stderr)
        return 1

    if args.dry_run:
        print(f"[dry-run] {len(changes)} update(s) ready; country_scams.json not modified.")
        return 0

    db["last_updated"] = dt.date.today().isoformat()
    try:
        COUNTRY_DB_PATH.write_text(json.dumps(db, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        print(f"[write] Updated {COUNTRY_DB_PATH.relative_to(REPO_ROOT)} ({len(changes)} merge action(s)).")

        if not args.skip_cargo_test and not run_cargo_tests():
            print("[rollback] cargo test failed; restoring previous country_scams.json!", file=sys.stderr)
            COUNTRY_DB_PATH.write_bytes(original_bytes)
            return 1
    except Exception as e:
        print(f"[rollback] error during write/verify ({e}); restoring previous country_scams.json!", file=sys.stderr)
        COUNTRY_DB_PATH.write_bytes(original_bytes)
        return 1

    print("[done] Country scam database updated and verified.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
