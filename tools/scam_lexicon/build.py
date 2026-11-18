#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
# SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

"""Build and sync the Bageo scam-call phrase lexicon from data/*.yaml.

Usage:
  python3 build.py            # Build out/lexicon.json, out/scam_db.local.json, out/詐騙電話話術詞庫.md
  python3 build.py --sync-db  # Also merge curated keywords into ../../crates/agent/data/scam_db.json
"""
import argparse
import datetime
import glob
import json
import os
import re
import subprocess
import sys
import yaml

try:
    from opencc import OpenCC
    _s2t = OpenCC("s2t").convert
    _s2twp = OpenCC("s2twp").convert
    _HAS_OPENCC = True
except ImportError:
    _s2t = lambda s: s
    _s2twp = lambda s: s
    _HAS_OPENCC = False

ROOT = os.path.dirname(os.path.abspath(__file__))
DEFAULT_REPO = os.path.abspath(os.path.join(ROOT, "..", ".."))
REPO = os.environ.get("BAGEO_REPO", DEFAULT_REPO)
OUT = os.path.join(ROOT, "out")

LANG_ORDER = ["zh-TW", "zh-CN", "en", "ja", "ko", "es", "fr", "de", "pt"]
LANG_ZH = {
    "zh-TW": "繁體中文（台灣，含港澳粵語 [HK]）",
    "zh-CN": "簡體中文（中國大陸，含新加坡華語）",
    "en": "英文",
    "ja": "日文",
    "ko": "韓文",
    "es": "西班牙文",
    "fr": "法文",
    "de": "德文",
    "pt": "葡萄牙文",
}
REGION_ZH = {
    "HK": "香港／澳門", "UK": "英國", "CA": "加拿大", "AU": "澳洲", "SG": "新加坡",
    "AR": "阿根廷", "MX": "墨西哥", "BE": "比利時", "CH": "瑞士", "AT": "奧地利",
    "PT": "葡萄牙", "US": "美國", "ES": "西班牙", "NZ": "紐西蘭", "MO": "澳門",
    "CO": "哥倫比亞", "CL": "智利", "PE": "秘魯", "BR": "巴西",
}

# Generic single nouns excluded when merging directly into built-in scam_db.json
# so everyday courier, utility or bank callbacks never get false-flagged.
BUILTIN_EXCLUDE = {
    "風險管控中心", "风控中心", "銀行客服", "银行客服", "包裹", "快遞", "快递",
    "網銀", "网银", "銀行卡", "银行卡", "車禍", "车祸",
    "解除分期", "分期付款", "取消分期",
    "colis", "paket", "encomenda", "dea", "gas", "utility", "fedex", "package",
    "택배", "carte bancaire", "chronopost", "sparkasse", "stadtwerke", "strom",
    "dhl", "correios", "ups", "usps", "royal mail", "post office", "laposte",
    "la poste", "dpd", "hermes", "gls", "endesa", "iberdrola", "edf", "engie",
    "enel", "cemig", "sabesp", "agua", "luz", "water", "electricity", "power",
    "bank", "police", "polizei", "policia", "policía", "banque", "banco",
    "hospital", "doctor", "lawyer", "avocat", "anwalt", "abogado", "advogado",
    "mama", "mamá", "maman", "oma", "opa", "enkel", "sohn", "tochter",
    "엄마", "아빠", "아들", "딸", "경찰", "검찰", "은행", "카드",
    "警察", "銀行", "病院", "弁護士", "息子", "母さん", "俺だよ",
    "媽", "爸", "兒子", "女兒", "律師", "醫院", "派出所", "法院", "海關",
    "妈", "爸", "儿子", "女儿", "律师", "医院", "海关", "公安", "检察院",
}


def words(text):
    t = text.lower().replace("’", "'")
    ws = [w.strip("'") for w in re.split(r"[^\w']+", t)]
    return [w for w in ws if w and w != "_"]


def norm(text, mode_name):
    t = text.strip()
    if mode_name == "han":
        return (
            _s2t(t)
            .lower()
            .replace("賬", "帳")
            .replace("加頼", "加賴")
            .replace("賣貨變", "賣貨便")
            .replace("好賣加", "好賣+")
        )
    if mode_name == "word":
        return " ".join(words(t))
    if mode_name == "run":
        return t.lower()
    return "".join(t.lower().split())


def split_item(item, lang, errors):
    s = item.strip()
    verbatim = s.startswith("!")
    if verbatim:
        s = s[1:].strip()
    region = None
    m = re.match(r"^\[([A-Z]{2})\]\s*", s)
    if m:
        region, s = m.group(1), s[m.end():]
    if "｜" in s:
        text, zh = [x.strip() for x in s.split("｜", 1)]
    else:
        text, zh = s, None
        if lang not in ("zh-TW", "zh-CN"):
            errors.append(f"{lang}: no zh translation: {s[:40]}")
    if lang == "zh-CN" and zh is None:
        zh = _s2twp(text)
    return {
        "text": text,
        "zh": zh,
        "region": region,
        "basis": "verbatim" if verbatim else "paraphrased",
    }


def format_scam_db(db):
    lines = [
        "{",
        f'  "version": {db["version"]},',
        f'  "about": {json.dumps(db["about"], ensure_ascii=False)},',
        f'  "name_languages": {json.dumps(db["name_languages"], ensure_ascii=False, separators=(",", ":"))},',
        '  "typologies": [',
    ]
    for ti, t in enumerate(db["typologies"]):
        comma = "," if ti + 1 < len(db["typologies"]) else ""
        lines.append("    {")
        lines.append(
            f'      "id": {json.dumps(t["id"])}, "category": {json.dumps(t["category"])}, "hard": {json.dumps(t["hard"])},'
        )
        lines.append('      "names": {')
        name_items = list(t["names"].items())
        for ni, (k, v) in enumerate(name_items):
            ncomma = "," if ni + 1 < len(name_items) else ""
            lines.append(f"        {json.dumps(k, ensure_ascii=False)}: {json.dumps(v, ensure_ascii=False)}{ncomma}")
        lines.append("      },")
        lines.append(f'      "signals": {json.dumps(t["signals"], ensure_ascii=False)},')
        lines.append(f'      "legit": {json.dumps(t["legit"], ensure_ascii=False)}')
        lines.append(f"    }}{comma}")
    lines.append("  ],")
    lines.append('  "keywords": [')
    for gi, g in enumerate(db["keywords"]):
        gcomma = "," if gi + 1 < len(db["keywords"]) else ""
        lines.append("    {")
        lines.append(f'      "lang": {json.dumps(g["lang"])}, "match": {json.dumps(g["match"])}, "words": [')
        for wi, w in enumerate(g["words"]):
            wcomma = "," if wi + 1 < len(g["words"]) else ""
            lines.append(f"      {json.dumps(w, ensure_ascii=False, separators=(',', ':'))}{wcomma}")
        lines.append("    ]")
        lines.append(f"    }}{gcomma}")
    lines.append("  ],")
    lines.append('  "own_matter": [')
    for oi, o in enumerate(db["own_matter"]):
        ocomma = "," if oi + 1 < len(db["own_matter"]) else ""
        lines.append(
            f'    {{"lang": {json.dumps(o["lang"])}, "phrases": {json.dumps(o["phrases"], ensure_ascii=False)}}}{ocomma}'
        )
    lines.append("  ],")
    lines.append('  "number_rules": [')
    for ri, r in enumerate(db["number_rules"]):
        rcomma = "," if ri + 1 < len(db["number_rules"]) else ""
        lines.append(f"    {json.dumps(r, ensure_ascii=False)}{rcomma}")
    lines.append("  ]")
    lines.append("}")
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description="Build and sync the Bageo scam lexicon.")
    parser.add_argument("--sync-db", action="store_true", help="Merge curated keywords into crates/agent/data/scam_db.json")
    parser.add_argument("--skip-cargo-test", action="store_true", help="Skip running cargo test -p agent scam_db after --sync-db")
    args = parser.parse_args()

    if args.sync_db and not _HAS_OPENCC:
        print("ERROR: OpenCC is required for --sync-db so Traditional/Simplified deduplication works cleanly.", file=sys.stderr)
        sys.exit(1)

    os.makedirs(OUT, exist_ok=True)
    with open(os.path.join(ROOT, "data", "categories.yaml"), encoding="utf-8") as f:
        cats = yaml.safe_load(f)

    langs = {}
    for path in glob.glob(os.path.join(ROOT, "data", "lang", "*.yaml")):
        with open(path, encoding="utf-8") as f:
            d = yaml.safe_load(f)
            langs[d["lang"]] = d

    db_path = os.path.join(REPO, "crates", "agent", "data", "scam_db.json")
    with open(db_path, encoding="utf-8") as f:
        db = json.load(f)

    typ = {t["id"]: t for t in db["typologies"]}
    mode = {g["lang"]: g["match"] for g in db["keywords"]}

    builtin = set()
    for g in db["keywords"]:
        for w in g["words"]:
            builtin.add(norm(w[0], g["match"]))

    errors = []
    lexicon = {
        "about": {
            "title": "bageo-call 詐騙電話話術詞庫",
            "generated": datetime.datetime.now().strftime("%Y-%m-%d %H:%M"),
            "basis": "verbatim = 原文或幾乎原文引用自來源；paraphrased = 依來源描述改寫／綜合的典型說法（報導範例，非逐字錄音）",
            "keyword_weight": "2 = 單獨出現就很可疑（對應 bageo strong）；1 = 需要搭配其他訊號",
            "hard": "hard = 只有詐騙會要求的動作（只在 bageo typology 本身是 hard 時匯出成 hard）",
        },
        "languages": {},
    }
    counts = {}

    for lang in LANG_ORDER:
        d = langs[lang]
        L = {"name": d["name"], "regions": d["regions"], "sources": d["sources"], "categories": {}}
        np = nk = 0
        for cid, meta in cats.items():
            if cid not in d["categories"]:
                continue
            c = d["categories"][cid]
            for s in c.get("sources", []):
                if s not in d["sources"]:
                    errors.append(f"{lang}/{cid}: unknown source {s}")
            hard = set(c.get("hard", []))
            kws, seen = [], set()
            for w, items in (
                (2, c.get("strong", [])),
                (2, sorted(hard - set(c.get("strong", [])))),
                (1, c.get("keywords", [])),
            ):
                for k in items:
                    if k in seen:
                        continue
                    seen.add(k)
                    kws.append({"text": k, "weight": w, "hard": k in hard})
            entry = {
                "name": meta["name"],
                "bageo_typology": meta["bageo_typology"],
                "description": meta["description"],
                "flow": meta["flow"],
                "detection_signals": meta["detection_signals"],
                "legit_lookalike": meta.get("legit_lookalike"),
                "phrases": [split_item(p, lang, errors) for p in c.get("phrases", [])],
                "keywords": kws,
                "requests": [split_item(p, lang, errors) for p in c.get("requests", [])],
                "tactics": [split_item(p, lang, errors) for p in c.get("tactics", [])],
                "sources": [dict(id=s, **d["sources"][s]) for s in c.get("sources", [])],
            }
            if c.get("notes"):
                entry["notes"] = c["notes"]
            L["categories"][cid] = entry
            np += len(entry["phrases"])
            nk += len(kws)
        unknown = set(d["categories"]) - set(cats)
        if unknown:
            errors.append(f"{lang}: categories not in categories.yaml: {unknown}")
        vb = sum(p["basis"] == "verbatim" for c in L["categories"].values() for p in c["phrases"])
        counts[lang] = {
            "categories": len(L["categories"]),
            "phrases": np,
            "verbatim": vb,
            "keywords": nk,
            "requests": sum(len(c["requests"]) for c in L["categories"].values()),
            "tactics": sum(len(c["tactics"]) for c in L["categories"].values()),
            "sources": len(d["sources"]),
        }
        lexicon["languages"][lang] = L

    lexicon["about"]["counts"] = counts
    if errors:
        print("ERRORS:\n" + "\n".join(errors), file=sys.stderr)
        sys.exit(1)

    local, report, taken = [], [], {}
    for lang in LANG_ORDER:
        m = mode[lang]
        for cid, c in lexicon["languages"][lang]["categories"].items():
            t = c["bageo_typology"]
            if not t:
                report.append(f"SKIP {lang}/{cid}: no bageo typology")
                continue
            if t not in typ:
                report.append(f"SKIP {lang}/{cid}: typology {t} not in built-in DB")
                continue
            for k in c["keywords"]:
                n = norm(k["text"], m)
                if not n:
                    continue
                if m == "word" and k["weight"] == 1 and len(n) < 3:
                    report.append(f"SKIP {lang} {n!r}: too short")
                    continue
                if n in builtin:
                    report.append(f"DUP-BUILTIN {lang} {n!r}")
                    continue
                if n in taken:
                    prev = taken[n]
                    if k["weight"] > prev["weight"]:
                        prev.update(weight=k["weight"], hard=k["hard"] and typ[prev["typology"]]["hard"])
                    report.append(f"DUP-LOCAL {lang}/{cid} {n!r} (kept {prev['typology']})")
                    continue
                e = {
                    "text": n if m != "han" else k["text"],
                    "lang": lang,
                    "typology": t,
                    "weight": k["weight"],
                    "hard": bool(k["hard"] and typ[t]["hard"] and k["weight"] == 2),
                }
                taken[n] = e
                local.append(e)

    with open(os.path.join(OUT, "scam_db.local.json"), "w", encoding="utf-8") as f:
        json.dump({"keywords": local, "numbers": []}, f, ensure_ascii=False, indent=1)
    with open(os.path.join(OUT, "local_import_report.txt"), "w", encoding="utf-8") as f:
        f.write(f"exported {len(local)} keywords\n" + "\n".join(report) + "\n")

    lc = {}
    for e in local:
        lc.setdefault(e["lang"], [0, 0, 0])
        lc[e["lang"]][0] += 1
        lc[e["lang"]][1] += e["weight"] == 2
        lc[e["lang"]][2] += e["hard"]
    for lang in counts:
        counts[lang]["local_export"] = lc.get(lang, [0, 0, 0])[0]

    with open(os.path.join(OUT, "lexicon.json"), "w", encoding="utf-8") as f:
        json.dump(lexicon, f, ensure_ascii=False, indent=1)

    if args.sync_db:
        expected_keys = {"version", "about", "name_languages", "typologies", "keywords", "own_matter", "number_rules"}
        if set(db.keys()) != expected_keys:
            print(f"ERROR: unexpected top-level scam_db.json keys: {sorted(db.keys())}", file=sys.stderr)
            sys.exit(1)

        with open(db_path, "rb") as f:
            orig_bytes = f.read()

        lang_groups = {g["lang"]: g for g in db["keywords"]}
        global_seen = set(builtin)
        mode_existing = {"han": [], "word": [], "run": [], "compact": []}
        for g in db["keywords"]:
            gm = g["match"]
            for entry in g["words"]:
                nk = norm(entry[0], gm)
                mode_existing[gm].append((nk, entry[1], len(entry) == 4 and entry[3] == "hard"))

        added = 0
        for lang in LANG_ORDER:
            g = lang_groups[lang]
            gm = g["match"]
            for cid, c in lexicon["languages"][lang]["categories"].items():
                t_id = c["bageo_typology"]
                if not t_id or t_id not in typ:
                    continue
                t_obj = typ[t_id]
                for k in c["keywords"]:
                    raw_text = k["text"].strip()
                    if gm in ("word", "run"):
                        raw_text = raw_text.lower()
                    nk = norm(raw_text, gm)
                    if not nk or raw_text in BUILTIN_EXCLUDE or nk in BUILTIN_EXCLUDE:
                        continue
                    w = k["weight"]
                    is_hard = bool(k.get("hard") and t_obj["hard"] and w == 2)
                    if w == 1 and not is_hard:
                        if gm == "word" and len(nk) <= 4:
                            continue
                        if t_id != "romance_scam" and (" " not in nk and len(nk) <= 3):
                            continue
                    if nk in global_seen:
                        continue
                    skip = False
                    for ek, ew, ehard in mode_existing[gm]:
                        if ek in nk:
                            if not (is_hard and not ehard) or ek in (
                                "分期", "atm", "帳戶", "信用卡", "銀行", "方案", "繳款",
                                "凍結", "cajero automático", "cuotas", "warranty", "loan",
                            ):
                                skip = True
                                break
                        elif nk in ek and w == 1:
                            skip = True
                            break
                    if skip:
                        continue
                    entry = [raw_text, w, t_id, "hard"] if is_hard else [raw_text, w, t_id]
                    g["words"].append(entry)
                    global_seen.add(nk)
                    mode_existing[gm].append((nk, w, is_hard))
                    added += 1

        formatted = format_scam_db(db)
        parsed = json.loads(formatted)
        if set(parsed.keys()) != expected_keys:
            print("ERROR: formatted scam_db.json keys mismatch", file=sys.stderr)
            sys.exit(1)

        bak_path = db_path + ".bak"
        tmp_path = db_path + ".tmp"
        with open(bak_path, "wb") as f:
            f.write(orig_bytes)
        try:
            with open(tmp_path, "w", encoding="utf-8", newline="\n") as f:
                f.write(formatted)
            json.loads(open(tmp_path, encoding="utf-8").read())
            os.replace(tmp_path, db_path)
            if not args.skip_cargo_test:
                cmd = ["cargo", "test", "-p", "agent", "--manifest-path", os.path.join(REPO, "Cargo.toml"), "scam_db"]
                proc = subprocess.run(cmd, cwd=REPO, check=False)
                if proc.returncode != 0:
                    raise RuntimeError(f"cargo test exited {proc.returncode}")
        except Exception as e:
            if os.path.exists(tmp_path):
                os.remove(tmp_path)
            with open(db_path, "wb") as f:
                f.write(orig_bytes)
            print(f"ERROR: --sync-db rolled back from backup: {e}", file=sys.stderr)
            if os.path.exists(bak_path):
                os.remove(bak_path)
            sys.exit(1)
        if os.path.exists(bak_path):
            os.remove(bak_path)
        print(f"Synced {added} new keywords into {db_path}")

    print("Lexicon built:", json.dumps(counts, ensure_ascii=False))


if __name__ == "__main__":
    main()
