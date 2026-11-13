"""Forward SMS received by Android phones to your Myna notification channels.

Optional add-on. For every phone that `adb devices` reports, two sources are read:

  * hook queue (optional, needs root): the Myna app's LSPosed hook drops one
    JSON file per incoming SMS into QUEUE_DIR on the phone, even while the phone is
    still locked after a reboot. Checked every SMS_PUSH_SEC; a file is deleted only
    after its message was delivered.
  * inbox: rows of content://sms/inbox past a saved (_id, date) cursor, read with
    `adb shell content query` every SMS_INBOX_SEC. Skipped while the agent has a call
    in progress.

A message seen by both sources is sent once (phone + sender + text, for SMS_DEDUP_SEC).
What is already on a phone when it is first seen is never forwarded: its queued files
are discarded and the inbox cursor starts at the newest row.

Delivery: POST {AGENT_URL}/calls/api/notify/send, which hands the message to every
notification channel configured in the agent's web console. A message that could not
be delivered stays where it is and is tried again.

adb: one binary is used throughout and `adb kill-server` is never called, because
restarting the adb server drops the agent's USB link to the phones.

The log never contains message texts or complete phone numbers.

  sms_forward.py             run until stopped
  sms_forward.py --once      one cycle
  sms_forward.py --dry-run   one cycle; nothing is sent, deleted or saved

Standard library only. Settings: environment variables, or KEY=VALUE lines in
<SMS_HOME>/sms_forward.env (see sms_forward.env.example and README.md).
"""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
import unicodedata
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Callable

HOME = Path(os.environ.get("SMS_HOME") or Path.home() / ".myna-call" / "sms").expanduser()


def _load_env(path: Path) -> None:
    """KEY=VALUE lines; variables already set in the environment win."""
    try:
        text = path.read_text(encoding="utf-8-sig")
    except OSError:
        return
    for line in text.splitlines():
        line = line.strip()
        if line and not line.startswith("#") and "=" in line:
            key, val = line.split("=", 1)
            os.environ.setdefault(key.strip(), val.strip().strip("\"'"))


_load_env(Path(os.environ.get("SMS_ENV_FILE") or HOME / "sms_forward.env").expanduser())


def _pairs(spec: str) -> dict[str, str]:
    """'a=1,b=2' -> {'A': '1', 'B': '2'}"""
    out = {}
    for part in re.split(r"[,;]", spec):
        key, _, val = part.partition("=")
        if key.strip() and val.strip():
            out[key.strip().upper()] = val.strip()
    return out


def _env_num(name: str, default: float, low: float, high: float) -> float:
    try:
        val = float(os.environ.get(name, "").strip() or default)
    except ValueError:
        return default
    return min(max(val, low), high) if val == val else default  # NaN is not a setting


def _env_flag(name: str, default: bool) -> bool:
    val = os.environ.get(name, "").strip().lower()
    if not val:
        return default
    return val not in ("0", "false", "no", "off")


STATE = HOME / "state.json"
HEALTH = HOME / "health.json"
LOCK = HOME / "sms_forward.lock"
LOG = HOME / "sms_forward.log"
LOG_MAX = 2 * 1024 * 1024

AGENT_URL = (os.environ.get("AGENT_URL", "").strip() or "http://127.0.0.1:8787").rstrip("/")
AGENT_TOKEN = os.environ.get("AGENT_TOKEN", "").strip()
SMS_LANG = os.environ.get("SMS_LANG", "").strip()
# Empty = every phone adb reports.
SERIALS = [s.strip() for s in os.environ.get("SMS_SERIALS", "").split(",") if s.strip()]
# ICCID=label for SIM cards that do not report their own number. Keyed by the card, so
# the label follows it into another phone.
SIM_NUMBERS = _pairs(os.environ.get("SMS_SIM_NUMBERS", ""))
# Off = never run `su` on the phone: no hook queue, inbox through the plain shell only.
USE_SU = _env_flag("SMS_USE_SU", True)
PUSH_SEC = _env_num("SMS_PUSH_SEC", 4, 1, 3600)
INBOX_SEC = _env_num("SMS_INBOX_SEC", 60, 5, 86400)
DEDUP_SEC = _env_num("SMS_DEDUP_SEC", 24 * 3600, 60, 30 * 86400)
# More new inbox rows than this at once means the cursor is stale; they are skipped.
FLOOD_CAP = int(_env_num("SMS_FLOOD_CAP", 20, 1, 10000))

DEVICES_SEC = 30
ADB_REFRESH_SEC = 300
SIM_SEC = 6 * 3600
SIM_RETRY_SEC = 600
QUEUE_RETRY_MAX_SEC = 300
IN_CALL_RECHECK_SEC = 15
BEAT_SEC = 1800
LATE_SEC = 300  # add the receive time when a message is forwarded later than this
SEND_GAP_SEC = 1.5
SENDS_PER_TICK = 15
BODY_MAX = 3000  # the agent cuts a whole notification at 3500 characters
LANG_REFRESH_SEC = 60  # the agent's language (and time zone) are asked again this often
LANG_RETRY_SEC = 30
LANG_TRIES = 2  # quick tries in a row before waiting LANG_RETRY_SEC
LANG_WAIT_SEC = 180  # messages wait this long for the agent's language, then go out in English
HEALTH_TIMEOUT = 4.0
SEND_TIMEOUT = 30.0

QUEUE_DIR = "/data/user_de/0/com.android.phone/cache/pixel-sms-push"
QUEUE_NAME = re.compile(r"[0-9a-f]{16,64}\.json")
LIST_QUEUE = (f"[ -d {QUEUE_DIR} ] || {{ echo @@NODIR; exit 0; }}; cd {QUEUE_DIR}; "
              "for f in *.json; do [ -e $f ] || continue; echo @@FILE $f $(stat -c %Y $f); cat $f; echo; done; "
              "echo @@END")
INBOX_QUERY = ("content query --uri content://sms/inbox --projection _id:address:date:sub_id:body "
               "--where {where} --sort _id")
INBOX_MAX_QUERY = "content query --uri content://sms/inbox --projection _id:date"
SIM_QUERY = ("content query --uri content://telephony/siminfo "
             "--projection _id:sim_id:icc_id:number:carrier_name")
INBOX_ROW = re.compile(r"^Row: \d+ _id=(\d+), address=(.*?), date=(\d+), sub_id=(-?\d+), body=", re.M)
SIM_ROW = re.compile(r"^Row: \d+ _id=(\d+), sim_id=(-?\d+), icc_id=(.*?), number=(.*?), carrier_name=(.*)$", re.M)

# Labels of the forwarded message. The SMS text itself is never translated.
TEXTS: dict[str, dict[str, str]] = {
    "en": {
        "title": "New SMS", "sep": ": ", "from": "From", "to": "To", "text": "Message",
        "received": "Received", "late": "forwarded late", "device": "Device",
        "unknown": "(unknown)", "truncated": "(truncated)", "flood_title": "SMS forwarder",
        "flood": "{device}: {count} messages that were never forwarded appeared in the inbox "
                 "at once. They were skipped; a backlog is not forwarded.",
    },
    "zh-TW": {
        "title": "新簡訊", "sep": "：", "from": "寄件人", "to": "收件號碼", "text": "內容",
        "received": "收到時間", "late": "延遲轉發", "device": "裝置",
        "unknown": "（不明）", "truncated": "（已截斷）", "flood_title": "簡訊轉發",
        "flood": "{device}：收件匣一次出現 {count} 則尚未轉發的簡訊，已略過，不會補發。",
    },
    "zh-CN": {
        "title": "新短信", "sep": "：", "from": "发件人", "to": "接收号码", "text": "内容",
        "received": "接收时间", "late": "延迟转发", "device": "设备",
        "unknown": "（未知）", "truncated": "（已截断）", "flood_title": "短信转发",
        "flood": "{device}：收件箱一次出现 {count} 条尚未转发的短信，已跳过，不会补发。",
    },
    "ja": {
        "title": "新着SMS", "sep": "：", "from": "差出人", "to": "宛先", "text": "本文",
        "received": "受信日時", "late": "遅れて転送", "device": "端末",
        "unknown": "（不明）", "truncated": "（省略）", "flood_title": "SMS転送",
        "flood": "{device}：未転送のSMSが受信トレイに一度に{count}件見つかりました。"
                 "これらは転送せずにスキップしました。",
    },
    "ko": {
        "title": "새 문자", "sep": ": ", "from": "보낸 사람", "to": "받은 번호", "text": "내용",
        "received": "수신 시각", "late": "지연 전달", "device": "기기",
        "unknown": "(알 수 없음)", "truncated": "(잘림)", "flood_title": "문자 전달",
        "flood": "{device}: 받은편지함에 전달되지 않은 문자 {count}건이 한꺼번에 나타났습니다. "
                 "건너뛰었으며 나중에 전달하지 않습니다.",
    },
    "es": {
        "title": "Nuevo SMS", "sep": ": ", "from": "De", "to": "Para", "text": "Mensaje",
        "received": "Recibido", "late": "reenviado con retraso", "device": "Dispositivo",
        "unknown": "(desconocido)", "truncated": "(truncado)", "flood_title": "Reenvío de SMS",
        "flood": "{device}: aparecieron a la vez {count} mensajes sin reenviar en la bandeja de "
                 "entrada. Se omitieron; los mensajes atrasados no se reenvían.",
    },
    "fr": {
        "title": "Nouveau SMS", "sep": " : ", "from": "De", "to": "À", "text": "Message",
        "received": "Reçu", "late": "transféré en retard", "device": "Appareil",
        "unknown": "(inconnu)", "truncated": "(tronqué)", "flood_title": "Transfert de SMS",
        "flood": "{device} : {count} messages non transférés sont apparus d'un coup dans la boîte "
                 "de réception. Ils ont été ignorés ; les anciens messages ne sont pas transférés.",
    },
    "de": {
        "title": "Neue SMS", "sep": ": ", "from": "Von", "to": "An", "text": "Nachricht",
        "received": "Empfangen", "late": "verspätet weitergeleitet", "device": "Gerät",
        "unknown": "(unbekannt)", "truncated": "(gekürzt)", "flood_title": "SMS-Weiterleitung",
        "flood": "{device}: {count} nicht weitergeleitete Nachrichten sind auf einmal im "
                 "Posteingang erschienen. Sie wurden übersprungen; ältere Nachrichten werden "
                 "nicht nachträglich weitergeleitet.",
    },
    "pt": {
        "title": "Novo SMS", "sep": ": ", "from": "De", "to": "Para", "text": "Mensagem",
        "received": "Recebido", "late": "encaminhado com atraso", "device": "Dispositivo",
        "unknown": "(desconhecido)", "truncated": "(truncado)", "flood_title": "Encaminhamento de SMS",
        "flood": "{device}: {count} mensagens não encaminhadas apareceram de uma só vez na caixa "
                 "de entrada. Foram ignoradas; mensagens antigas não são encaminhadas.",
    },
}


def log(msg: str) -> None:
    line = time.strftime("%Y-%m-%d %H:%M:%S ") + msg
    try:
        LOG.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        if LOG.exists() and LOG.stat().st_size > LOG_MAX:
            LOG.replace(LOG.with_name(LOG.name + ".1"))
        with LOG.open("a", encoding="utf-8") as f:
            f.write(line + "\n")
    except Exception:
        pass
    try:
        print(line, flush=True)
    except Exception:
        pass


def iso(ts: float) -> str:
    """Local time with its UTC offset."""
    return dt.datetime.fromtimestamp(ts).astimezone().isoformat(timespec="seconds")


def err_tag(e: BaseException) -> str:
    """An exception for the log: its type and line, not its message (which may quote data)."""
    tb = e.__traceback__
    while tb is not None and tb.tb_next is not None:
        tb = tb.tb_next
    return f"{type(e).__name__} at line {tb.tb_lineno}" if tb else type(e).__name__


def read_json(path: Path) -> dict:
    try:
        obj = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return obj if isinstance(obj, dict) else {}


def write_json(path: Path, obj: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_text(json.dumps(obj, ensure_ascii=False, indent=1), encoding="utf-8")
    os.replace(tmp, path)


# --- language --------------------------------------------------------------------------

def normalize_lang(code: str) -> str:
    """A language code as the agent reads it; anything unknown is English."""
    t = (code or "").strip().lower().replace("_", "-")
    if t in ("zh-tw", "zh-hant", "zh-hk", "zh"):
        return "zh-TW"
    if t in ("zh-cn", "zh-hans", "zh-sg"):
        return "zh-CN"
    base = t.split("-")[0]
    return base if base in TEXTS else "en"


class Language:
    """Language of the message labels: SMS_LANG, else the agent's, else English.

    Until the agent has told its language once, code() answers None: messages wait
    (they stay queued or unread and are tried again) for up to LANG_WAIT_SEC, and only
    then go out in English. The agent is asked again every LANG_REFRESH_SEC, so a change
    in the web console reaches the labels within a minute.
    """

    def __init__(self, explicit: str = "", fetch: Callable[[], str | None] | None = None) -> None:
        self.fixed = normalize_lang(explicit) if explicit.strip() else ""
        self.fetch = fetch
        self.value: str | None = None
        self.next_check = 0.0
        self.waiting_since: float | None = None

    def ask(self) -> str | None:
        for _ in range(LANG_TRIES):
            try:
                got = self.fetch() if self.fetch else None
            except Exception:
                got = None
            if got:
                return got
        return None

    def code(self) -> str | None:
        if self.fixed:
            return self.fixed
        if not self.fetch:
            return "en"
        now = time.time()
        if now >= self.next_check:
            got = self.ask()
            if got:
                self.value = normalize_lang(got)
                self.next_check = now + LANG_REFRESH_SEC
            else:  # agent not answering: keep what we have and ask again soon
                self.next_check = now + LANG_RETRY_SEC
        if self.value:
            return self.value
        if self.waiting_since is None:
            self.waiting_since = now
        return "en" if now - self.waiting_since >= LANG_WAIT_SEC else None


class AgentZone:
    """The agent's UTC offset, for the receive time of late messages; asked at most every
    LANG_REFRESH_SEC. None (this machine's own zone) until the agent has told it."""

    def __init__(self, fetch: Callable[[], dt.tzinfo | None] | None = None) -> None:
        self.fetch = fetch
        self.value: dt.tzinfo | None = None
        self.next_check = 0.0

    def get(self) -> dt.tzinfo | None:
        now = time.time()
        if self.fetch and now >= self.next_check:
            try:
                got = self.fetch()
            except Exception:
                got = None
            if got is not None:
                self.value = got
                self.next_check = now + LANG_REFRESH_SEC
            else:
                self.next_check = now + LANG_RETRY_SEC
        return self.value


# --- agent -----------------------------------------------------------------------------

def agent_request(path: str, payload: dict | None = None, timeout: float = 10.0) -> tuple[int, object]:
    """(HTTP status, parsed JSON or None). Status 0 = no answer."""
    headers = {"Accept": "application/json"}
    data = None
    if payload is not None:
        data = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        headers["Content-Type"] = "application/json; charset=utf-8"
    if AGENT_TOKEN:
        headers["Authorization"] = f"Bearer {AGENT_TOKEN}"
    req = urllib.request.Request(AGENT_URL + path, data=data, headers=headers,
                                 method="GET" if data is None else "POST")
    host = (urllib.parse.urlsplit(AGENT_URL).hostname or "").lower()
    if host in ("127.0.0.1", "localhost", "::1"):
        # An agent on this machine is never reached through a system proxy.
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    else:
        opener = urllib.request.build_opener()
    try:
        with opener.open(req, timeout=timeout) as r:
            status, raw = r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, None
    except Exception:
        return 0, None
    try:
        return status, json.loads(raw.decode("utf-8"))
    except ValueError:
        return status, None


def deliver(title: str, body: str) -> tuple[bool, str]:
    """Hand a message to the agent's notification channels. (delivered, reason)."""
    status, obj = agent_request("/calls/api/notify/send", {"title": title, "body": body}, SEND_TIMEOUT)
    if status == 200 and isinstance(obj, dict) and obj.get("ok") is True:
        return True, "agent:200"
    if status == 200:
        return False, "agent: no notification channel accepted it (set one up in the web console)"
    if status == 401:
        return False, "agent:401 (set AGENT_TOKEN to the agent's CALLS_UI_TOKEN)"
    if status == 0:
        return False, "agent: no answer (is it running? check AGENT_URL)"
    return False, f"agent:{status}"


def agent_health() -> dict | None:
    status, obj = agent_request("/health", timeout=HEALTH_TIMEOUT)
    return obj if status == 200 and isinstance(obj, dict) else None


def agent_in_call() -> bool:
    """A call is in progress. False when the agent does not answer."""
    health = agent_health()
    try:
        return int((health or {}).get("active_call_count") or 0) > 0
    except (TypeError, ValueError):
        return False


def agent_language() -> str | None:
    lang = (agent_health() or {}).get("language")
    return lang if isinstance(lang, str) and lang.strip() else None


def agent_utc_offset() -> dt.tzinfo | None:
    """The agent's time zone as a fixed UTC offset, from the local_time in its /health
    (e.g. 2026-11-13T07:52:22+08:00); None when it does not say."""
    stamp = (agent_health() or {}).get("local_time")
    if not isinstance(stamp, str) or not stamp.strip():
        return None
    try:
        parsed = dt.datetime.fromisoformat(stamp.strip().replace("Z", "+00:00"))
    except ValueError:
        return None
    return parsed.tzinfo if parsed.utcoffset() is not None else None


def agent_adb_path() -> str:
    """The adb binary an agent on this machine uses, when it reports a full path."""
    usb = (agent_health() or {}).get("usb_bridge")
    path = usb.get("adb_path") if isinstance(usb, dict) else ""
    return path if isinstance(path, str) and os.path.isabs(path) and os.path.isfile(path) else ""


# --- adb -------------------------------------------------------------------------------

def adb_candidates() -> list[str]:
    """Places adb usually lives, in the order they are tried after ADB_BIN."""
    exe = "adb.exe" if os.name == "nt" else "adb"
    out: list[str] = []
    on_path = shutil.which("adb")
    if on_path:
        out.append(on_path)
    sdks = [os.environ.get("ANDROID_HOME", ""), os.environ.get("ANDROID_SDK_ROOT", "")]
    if os.environ.get("LOCALAPPDATA"):
        sdks.append(os.path.join(os.environ["LOCALAPPDATA"], "Android", "Sdk"))
    home = Path.home()
    sdks += [str(home / "Library" / "Android" / "sdk"), str(home / "Android" / "Sdk")]
    out += [os.path.join(sdk, "platform-tools", exe) for sdk in sdks if sdk]
    here = Path(__file__).resolve()
    roots = ([here.parents[2]] if len(here.parents) > 2 else []) + [Path.cwd()]
    out += [str(root / "platform-tools" / exe) for root in roots]
    return out


def find_adb(agent_hint: str = "") -> str:
    """ADB_BIN if set (and nothing else), else the agent's adb, else the usual places."""
    explicit = os.path.expanduser(os.environ.get("ADB_BIN", "").strip().strip("\"'"))
    if explicit:
        return explicit if os.path.isfile(explicit) else (shutil.which(explicit) or "")
    if agent_hint and os.path.isfile(agent_hint):
        return agent_hint
    return next((p for p in adb_candidates() if os.path.isfile(p)), "")


def _run(cmd: list[str], timeout: float) -> tuple[int, str, str]:
    # No console window per call on Windows.
    extra = {"creationflags": 0x08000000} if os.name == "nt" else {}
    try:
        p = subprocess.run(cmd, capture_output=True, timeout=timeout, stdin=subprocess.DEVNULL, **extra)
    except Exception as e:
        return 1, "", type(e).__name__
    out = (p.stdout or b"").decode("utf-8", "replace").replace("\r\n", "\n")
    err = (p.stderr or b"").decode("utf-8", "replace").replace("\r\n", "\n")
    return p.returncode, out, err


class Adb:
    """Runs one adb binary. Never calls `adb kill-server`."""

    def __init__(self, hint: Callable[[], str] | None = None) -> None:
        self.path = ""
        self.hint = hint

    def refresh(self) -> None:
        hint = ""
        if self.hint and not os.environ.get("ADB_BIN", "").strip():
            try:
                hint = self.hint()
            except Exception:
                hint = ""
        if not hint and self.path and os.path.isfile(self.path):
            return  # keep the binary in use: switching between adb versions restarts the server
        chosen = find_adb(hint)
        if chosen != self.path:
            log(f"adb binary={chosen or '(not found)'}")
            self.path = chosen

    def __call__(self, args: list[str], timeout: float = 20) -> tuple[int, str, str]:
        if not self.path:
            return 1, "", "no adb"
        return _run([self.path, *args], timeout)


def su(adb, serial: str, script: str, timeout: float = 20) -> tuple[int, str, str]:
    assert "'" not in script
    return adb(["-s", serial, "shell", f"su -c '{script}'"], timeout)


def parse_devices(out: str) -> dict[str, str] | None:
    """`adb devices` output -> {serial: state}; None when it is not a device list."""
    lines = out.splitlines()
    start = next((i for i, line in enumerate(lines) if line.startswith("List of devices")), None)
    if start is None:
        return None
    found = {}
    for line in lines[start + 1:]:
        parts = line.split()
        if len(parts) >= 2 and not parts[0].startswith("*"):
            found[parts[0]] = parts[1]
    return found


def adb_devices(adb) -> dict[str, str] | None:
    code, out, _ = adb(["devices"], 15)
    return parse_devices(out) if code == 0 else None


def select_ready(present: dict[str, str], serials: list[str]) -> list[str]:
    """Phones to read: state `device`, and listed in SMS_SERIALS when that is set."""
    ready = [s for s, state in present.items() if state == "device"]
    return [s for s in serials if s in ready] if serials else ready


def phone_model(adb, serial: str) -> str:
    code, out, _ = adb(["-s", serial, "shell", "getprop ro.product.model"], 10)
    return out.strip() if code == 0 else ""


def ce_unlocked(adb, serial: str) -> bool:
    """The phone was unlocked since boot, so its SMS database is readable."""
    code, out, _ = adb(["-s", serial, "shell", "getprop sys.user.0.ce_available"], 10)
    return code == 0 and out.strip() == "true"


# --- device data -----------------------------------------------------------------------

def parse_queue(out: str) -> list[tuple[str, int, dict | None]] | None:
    """[(file, mtime, json or None)] oldest first; None if the listing did not finish."""
    lines = [line.strip() for line in out.split("\n")]
    if "@@NODIR" in lines:
        return []
    if "@@END" not in lines:
        return None
    items = []
    for i, line in enumerate(lines):
        m = re.fullmatch(r"@@FILE (\S+) (\d+)", line)
        if not m or not QUEUE_NAME.fullmatch(m.group(1)):
            continue
        try:
            obj = json.loads(lines[i + 1]) if i + 1 < len(lines) else None
        except ValueError:
            obj = None
        items.append((m.group(1), int(m.group(2)), obj if isinstance(obj, dict) else None))
    return sorted(items, key=lambda x: (x[1], x[0]))


def read_queue(adb, serial: str) -> tuple[list[tuple[str, int, dict | None]] | None, str]:
    """(queue, reason it could not be read). The reason never quotes stdout: that holds SMS."""
    code, out, err = su(adb, serial, LIST_QUEUE)
    queue = parse_queue(out)
    if queue is not None:
        return queue, ""
    first = next((line.strip() for line in err.splitlines() if line.strip()), "")
    return None, first[:120] or f"incomplete listing, exit {code}"


def queue_remove(adb, serial: str, names: list[str]) -> None:
    names = [n for n in names if QUEUE_NAME.fullmatch(n)]
    for i in range(0, len(names), 40):
        su(adb, serial, f"cd {QUEUE_DIR} && rm -f {' '.join(names[i:i + 40])}")


def parse_inbox(out: str) -> list[dict] | None:
    """Rows of INBOX_QUERY (body is the last column and may span lines); None on error."""
    ms = list(INBOX_ROW.finditer(out))
    if not ms:
        return [] if "No result found" in out else None
    rows = []
    for i, m in enumerate(ms):
        end = ms[i + 1].start() if i + 1 < len(ms) else len(out)
        body = out[m.end():end]
        if body.endswith("\n"):
            body = body[:-1]
        addr = m.group(2)
        rows.append({
            "id": int(m.group(1)),
            "addr": "" if addr == "NULL" else addr,
            "date": int(m.group(3)),
            "sub": int(m.group(4)),
            "body": "" if body == "NULL" else body,
        })
    return rows


def inbox_after(adb, serial: str, last_id: int, last_date: int) -> list[dict] | None:
    """Inbox rows past the cursor. `date>` also catches a reused _id after a deletion."""
    cond = f"_id>{int(last_id)} OR date>{int(last_date)}"
    _, out, _ = adb(["-s", serial, "shell", INBOX_QUERY.format(where=f"'{cond}'")], 30)
    rows = parse_inbox(out)
    if rows is None and USE_SU:
        _, out, _ = su(adb, serial, INBOX_QUERY.format(where=f'"{cond}"'), 30)
        rows = parse_inbox(out)
    return rows


def inbox_max(adb, serial: str) -> tuple[int, int] | None:
    """(max _id, max date) of the inbox; None if unreadable (e.g. not unlocked since boot)."""
    runners = [lambda: adb(["-s", serial, "shell", INBOX_MAX_QUERY], 60)]
    if USE_SU:
        runners.append(lambda: su(adb, serial, INBOX_MAX_QUERY, 60))
    for runner in runners:
        _, out, _ = runner()
        pairs = re.findall(r"^Row: \d+ _id=(\d+), date=(\d+)", out, re.M)
        if pairs:
            return max(int(a) for a, _ in pairs), max(int(b) for _, b in pairs)
        if "No result found" in out:
            return 0, 0
    return None


def parse_sims(out: str) -> tuple[dict[int, str], list[str]] | None:
    """({subscription id: label}, [labels of the SIMs in a slot]); None on error."""
    rows = SIM_ROW.findall(out)
    if not rows:
        return None
    subs: dict[int, str] = {}
    active: list[str] = []
    for sub_id, slot, iccid, number, carrier in rows:
        number = "" if number == "NULL" else number.strip()
        carrier = "" if carrier == "NULL" else carrier.strip()
        label = SIM_NUMBERS.get(iccid.strip().upper()) or number or carrier
        if label:
            subs[int(sub_id)] = label
            if int(slot) >= 0 and label not in active:
                active.append(label)
    return subs, active


def read_sims(adb, serial: str) -> tuple[dict[int, str], list[str]] | None:
    # Recent Android versions refuse this table to the shell user; root may read it.
    got = parse_sims(adb(["-s", serial, "shell", SIM_QUERY], 30)[1])
    if got is None and USE_SU:
        got = parse_sims(su(adb, serial, SIM_QUERY, 30)[1])
    return got


# --- message ---------------------------------------------------------------------------

def norm_sender(s: str) -> str:
    """Sender as compared for de-duplication.

    A phone number is reduced to its last seven digits, so that the national and the
    international spelling of one number (which the two sources may report) are equal.
    """
    t = re.sub(r"[\x00-\x1f\x7f]", "", s or "")
    digits = re.sub(r"\D", "", t)
    if len(digits) >= 7:
        return digits[-7:]
    return digits or re.sub(r"\s+", " ", t.lower()).strip()


def norm_body(s: str) -> str:
    t = unicodedata.normalize("NFKC", s or "")
    t = re.sub(r"[\x00-\x1f\x7f]", " ", t)
    return re.sub(r"\s+", " ", t).strip()


def sms_key(serial: str, sender: str, body: str) -> str:
    raw = f"{serial}\t{norm_sender(sender)}\t{norm_body(body)}"
    return hashlib.sha256(raw.encode("utf-8")).hexdigest()[:24]


def mask_sender(sender: str) -> str:
    """Sender for the log: the last four digits of a number, or only the length of a name."""
    digits = re.sub(r"\D", "", sender or "")
    if len(digits) >= 7:
        return "..." + digits[-4:]
    return f"[{len(sender or '')} chars]"


def format_stamp(ts: float, tz: dt.tzinfo | None = None) -> str:
    """A receive time: in the agent's zone when it is known, else this machine's."""
    if tz is not None:
        return dt.datetime.fromtimestamp(ts, tz).strftime("%Y-%m-%d %H:%M")
    return time.strftime("%Y-%m-%d %H:%M", time.localtime(ts))


def format_sms(lang: str, sender: str, line: str, body: str, serial: str, model: str,
               received_ms: int, now: float, tz: dt.tzinfo | None = None) -> tuple[str, str]:
    """(title, text) of the notification for one SMS. `tz` = the agent's time zone."""
    t = TEXTS.get(lang) or TEXTS["en"]
    sep = t["sep"]
    if len(body) > BODY_MAX:
        body = body[:BODY_MAX] + "… " + t["truncated"]
    out = [f"{t['from']}{sep}{sender or t['unknown']}"]
    if line:
        out.append(f"{t['to']}{sep}{line}")
    out.append(f"{t['text']}{sep}{body}")
    if received_ms and now - received_ms / 1000 > LATE_SEC:
        stamp = format_stamp(received_ms / 1000, tz)
        out.append(f"{t['received']}{sep}{stamp} ({t['late']})")
    out.append(f"{t['device']}{sep}{model} ({serial})" if model else f"{t['device']}{sep}{serial}")
    return t["title"], "\n".join(out)


def format_flood(lang: str, serial: str, model: str, count: int) -> tuple[str, str]:
    t = TEXTS.get(lang) or TEXTS["en"]
    device = f"{model} ({serial})" if model else serial
    return t["flood_title"], t["flood"].format(device=device, count=count)


# --- forwarder -------------------------------------------------------------------------

class Forwarder:
    def __init__(self, adb=None, deliver_fn=deliver, in_call=agent_in_call,
                 lang: Callable[[], str | None] | None = None,
                 zone: Callable[[], dt.tzinfo | None] | None = None, dry_run: bool = False) -> None:
        self.adb = adb or Adb(agent_adb_path)
        self.deliver = deliver_fn
        self.in_call = in_call
        self.lang = lang or Language(SMS_LANG, agent_language).code
        self.zone = zone or AgentZone(agent_utc_offset).get
        self.dry = dry_run
        self.state = read_json(STATE)
        self.state.setdefault("phones", {})
        self.state.setdefault("dedup", {})
        self.dirty = False
        self.present: dict[str, str] = {}
        self.status: dict[str, dict] = {}
        self.sims: dict[str, tuple[float, dict[int, str], list[str]]] = {}
        self.sim_retry: dict[str, float] = {}
        self.queue_fail: dict[str, tuple[int, float]] = {}
        self.warned: dict[str, float] = {}
        self.next_adb = self.next_devices = self.next_inbox = 0.0
        self.next_beat = time.time() + BEAT_SEC
        self.last_send = 0.0
        self.sends = 0
        self.started = time.time()
        self.sent_total = 0
        self.last_forward = 0.0
        self.notify_ok: bool | None = None

    def warn(self, key: str, msg: str, every: float = 600) -> None:
        now = time.time()
        if now - self.warned.get(key, 0) >= every:
            self.warned[key] = now
            log(msg)

    def phone(self, serial: str) -> dict:
        return self.state["phones"].setdefault(serial, {})

    def line_for(self, serial: str, sub: int | None) -> str:
        """Label of the SIM a message arrived on; '' when the phone does not tell."""
        now = time.time()
        cached = self.sims.get(serial)
        stale = not cached or now - cached[0] > SIM_SEC or (sub is not None and sub not in cached[1])
        if stale and now >= self.sim_retry.get(serial, 0):
            got = read_sims(self.adb, serial)
            if got:
                cached = (now, got[0], got[1])
                self.sims[serial] = cached
            else:
                self.sim_retry[serial] = now + SIM_RETRY_SEC
        if not cached:
            return ""
        if sub is not None and sub in cached[1]:
            return cached[1][sub]
        return ", ".join(cached[2])

    def forward(self, serial: str, src: str, ref: str, sender: str, body: str,
                received_ms: int, sub: int | None) -> bool:
        """True once delivered (or already delivered); False = try again later."""
        key = sms_key(serial, sender, body)
        seen = self.state["dedup"].get(key)
        who = mask_sender(sender)
        if seen and time.time() - seen < DEDUP_SEC:
            log(f"dedup skip {serial} {src}={ref} from={who}")
            return True
        if self.dry:
            log(f"dry-run would forward {serial} {src}={ref} from={who} len={len(body)}")
            return True
        if self.sends >= SENDS_PER_TICK:
            return False
        lang = self.lang()
        if not lang:
            # Not in a guessed language: the message stays and is tried again.
            self.warn("lang", "the agent's language is not known yet; messages wait for it "
                              f"(at most {int(LANG_WAIT_SEC)} s, then English)", 300)
            return False
        wait = SEND_GAP_SEC - (time.time() - self.last_send)
        if wait > 0:
            time.sleep(wait)
        late = bool(received_ms) and time.time() - received_ms / 1000 > LATE_SEC
        title, text = format_sms(lang, sender, self.line_for(serial, sub), body, serial,
                                 self.phone(serial).get("model", ""), received_ms, time.time(),
                                 self.zone() if late else None)
        ok, via = self.deliver(title, text)
        self.last_send = time.time()
        self.sends += 1
        self.notify_ok = ok
        if not ok:
            self.warn("deliver", f"ERROR deliver failed {serial} {src}={ref}: {via}; will retry", 120)
            return False
        self.state["dedup"][key] = int(time.time())
        self.dirty = True
        self.save()
        self.sent_total += 1
        self.last_forward = time.time()
        age = int(time.time() - received_ms / 1000) if received_ms else -1
        log(f"forwarded {serial} {src}={ref} via={via} from={who} age_s={age}")
        return True

    def queue_items(self, serial: str) -> list[tuple[str, int, dict | None]] | None:
        """The phone's hook queue; None while it cannot be read (no root, no answer)."""
        st = self.status.setdefault(serial, {})
        if not USE_SU:
            st["queue"] = "off"
            return None
        now = time.time()
        fails, retry_at = self.queue_fail.get(serial, (0, 0.0))
        if now < retry_at:
            return None
        queue, why = read_queue(self.adb, serial)
        if queue is None:
            fails = min(fails + 1, 10)
            self.queue_fail[serial] = (fails, now + min(PUSH_SEC * 2 ** fails, QUEUE_RETRY_MAX_SEC))
            st["queue"] = "unavailable"
            self.warn(f"{serial}:queue", f"hook queue {serial} not readable ({why}); "
                                         "the inbox is still polled", 3600)
            return None
        self.queue_fail.pop(serial, None)
        st["queue"] = len(queue)
        return queue

    def seed(self, serial: str) -> None:
        """First sight of a phone: skip whatever is already queued or in the inbox."""
        queue = self.queue_items(serial)
        mx = inbox_max(self.adb, serial)
        if mx == (0, 0) and not ce_unlocked(self.adb, serial):
            mx = None  # an empty answer from a locked phone is not a real empty inbox
        skipped = "unreadable" if queue is None else len(queue)
        if self.dry:
            log(f"dry-run seed {serial}: would skip queue={skipped} inbox_cursor={mx}")
            return
        ph = self.phone(serial)
        if queue is not None:
            queue_remove(self.adb, serial, [name for name, _, _ in queue])
            ph["queue_seeded"] = True
        ph["seeded"] = True
        ph["seeded_at"] = iso(time.time())
        if mx:
            ph["inbox_id"], ph["inbox_date"] = mx
        self.dirty = True
        log(f"seed {serial} ({ph.get('model', '')}) skipped_queue={skipped} inbox_cursor={mx} (no backlog)")

    def drain_queue(self, serial: str) -> None:
        queue = self.queue_items(serial)
        if queue is None:
            return
        ph = self.phone(serial)
        if not ph.get("queue_seeded"):
            # The queue became readable after the phone was first seen: same rule, no backlog.
            if self.dry:
                log(f"dry-run hook queue {serial}: first read, would skip {len(queue)} file(s)")
                return
            queue_remove(self.adb, serial, [name for name, _, _ in queue])
            ph["queue_seeded"] = True
            self.dirty = True
            log(f"hook queue {serial}: first read, skipped {len(queue)} file(s) (no backlog)")
            return
        for name, mtime, obj in queue:
            if obj is None:
                log(f"WARN queue {serial} {name}: not JSON, dropped")
                if not self.dry:
                    queue_remove(self.adb, serial, [name])
                continue
            sender, body = str(obj.get("addr") or ""), str(obj.get("body") or "")
            if not self.forward(serial, "hook", name[:12], sender, body, mtime * 1000, None):
                break
            if not self.dry:
                queue_remove(self.adb, serial, [name])

    def poll_inbox(self, serial: str) -> None:
        ph = self.phone(serial)
        st = self.status.setdefault(serial, {})
        if ph.get("inbox_id") is None:
            mx = inbox_max(self.adb, serial)
            if mx is None:
                st["inbox"] = "error"
                self.warn(f"{serial}:inbox", f"WARN inbox {serial} unreadable "
                                             "(locked since boot, or not allowed for adb on this phone)")
            elif (mx != (0, 0) or ce_unlocked(self.adb, serial)) and not self.dry:
                ph["inbox_id"], ph["inbox_date"] = mx
                self.dirty = True
                st["inbox"] = "ok"
                log(f"inbox cursor {serial} seeded _id={mx[0]} (no backlog)")
            return
        rows = inbox_after(self.adb, serial, ph["inbox_id"], ph.get("inbox_date", 0))
        if rows is None:
            st["inbox"] = "error"
            self.warn(f"{serial}:inbox", f"WARN inbox {serial} unreadable "
                                         "(locked since boot, or not allowed for adb on this phone)")
            return
        st["inbox"] = "ok"
        if len(rows) > FLOOD_CAP:
            lang = None if self.dry else self.lang()
            if not self.dry and not lang:
                return  # the rows stay; skipped and reported once the language is known
            log(f"WARN inbox {serial}: {len(rows)} rows past the cursor at once; skipped (no backlog)")
            if self.dry:
                return
            ph["inbox_id"] = max([ph["inbox_id"]] + [r["id"] for r in rows])
            ph["inbox_date"] = max([ph.get("inbox_date", 0)] + [r["date"] for r in rows])
            self.dirty = True
            self.deliver(*format_flood(lang or "en", serial, ph.get("model", ""), len(rows)))
            return
        for r in rows:
            if not self.forward(serial, "inbox", str(r["id"]), r["addr"], r["body"], r["date"], r["sub"]):
                break
            ph["inbox_id"] = max(ph["inbox_id"], r["id"])
            ph["inbox_date"] = max(ph.get("inbox_date", 0), r["date"])
            self.dirty = True

    def tick(self) -> None:
        now = time.time()
        self.sends = 0
        if isinstance(self.adb, Adb) and now >= self.next_adb:
            self.adb.refresh()
            self.next_adb = now + (ADB_REFRESH_SEC if self.adb.path else DEVICES_SEC)
        no_adb = isinstance(self.adb, Adb) and not self.adb.path
        if no_adb:
            self.warn("adb", "WARN adb not found: install Android platform-tools or set ADB_BIN")
        if now >= self.next_devices:
            found = adb_devices(self.adb)
            if found is None:
                if not no_adb:
                    self.warn("devices", "WARN adb devices failed")
            else:
                self.present = found
            self.next_devices = now + DEVICES_SEC
        ready = select_ready(self.present, SERIALS)
        for s in ready:
            try:
                ph = self.phone(s)
                if not ph.get("model"):
                    ph["model"] = phone_model(self.adb, s)
                    self.dirty = True
                if ph.get("seeded"):
                    self.drain_queue(s)
                else:
                    self.seed(s)
            except Exception as e:
                self.warn(f"{s}:tick", f"ERROR {s} {err_tag(e)}")
        if now >= self.next_inbox:
            if self.in_call():
                self.next_inbox = now + IN_CALL_RECHECK_SEC
            else:
                self.next_inbox = now + INBOX_SEC
                for s in ready:
                    if self.phone(s).get("seeded"):
                        try:
                            self.poll_inbox(s)
                        except Exception as e:
                            self.warn(f"{s}:inbox-x", f"ERROR inbox {s} {err_tag(e)}")
        dedup = self.state["dedup"]
        for k in [k for k, ts in dedup.items() if now - ts > DEDUP_SEC]:
            del dedup[k]
            self.dirty = True
        self.save()
        self.write_health()
        if now >= self.next_beat:
            self.next_beat = now + BEAT_SEC
            parts = [f"{s} queue={self.status.get(s, {}).get('queue')} inbox_id={self.phone(s).get('inbox_id')}"
                     for s in ready]
            log(f"alive phones={len(ready)} sent_total={self.sent_total} | " + " | ".join(parts))

    def save(self) -> None:
        if self.dirty and not self.dry:
            write_json(STATE, self.state)
            self.dirty = False

    def write_health(self) -> None:
        if self.dry:
            return
        phones = {}
        for s in list(dict.fromkeys([*self.state["phones"], *self.present])):
            if SERIALS and s not in SERIALS:
                continue
            ph = self.state["phones"].get(s) or {}
            phones[s] = {"adb": self.present.get(s, "absent"), "model": ph.get("model", ""),
                         "seeded": bool(ph.get("seeded")), "inbox_id": ph.get("inbox_id"),
                         **self.status.get(s, {})}
        try:
            write_json(HEALTH, {
                "pid": os.getpid(), "started_at": iso(self.started), "cycle_at": iso(time.time()),
                "sent_total": self.sent_total,
                "last_forward_at": iso(self.last_forward) if self.last_forward else None,
                "notify_ok": self.notify_ok, "phones": phones,
            })
        except OSError:
            pass


def acquire_lock():
    """An open, exclusively locked file; None when another instance holds it."""
    LOCK.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    f = open(LOCK, "a+b")
    try:
        if os.name == "nt":
            import msvcrt

            f.seek(0)
            msvcrt.locking(f.fileno(), msvcrt.LK_NBLCK, 1)
        else:
            import fcntl

            fcntl.flock(f.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        f.close()
        return None
    return f


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        description="Forward SMS from USB-connected Android phones to the Myna "
                    "notification channels. Settings are environment variables; see README.md.")
    parser.add_argument("--once", action="store_true", help="run one cycle and exit")
    parser.add_argument("--dry-run", action="store_true",
                        help="run one cycle without sending, deleting or saving anything")
    args = parser.parse_args(argv)
    lock = None
    if not args.dry_run:
        lock = acquire_lock()
        if lock is None:
            log("another sms_forward.py holds the lock; exit")
            return 0
    fw = Forwarder(dry_run=args.dry_run)
    url = urllib.parse.urlsplit(AGENT_URL)
    agent = f"{url.scheme}://{url.netloc.rpartition('@')[2]}"  # without any user:password part
    log(f"start pid={os.getpid()} dry_run={args.dry_run} once={args.once} "
        f"agent={agent} token={'yes' if AGENT_TOKEN else 'no'} "
        f"lang={SMS_LANG or 'agent'} serials={','.join(SERIALS) or 'all'} su={'on' if USE_SU else 'off'} "
        f"home={HOME}")
    fw.write_health()
    try:
        while True:
            try:
                fw.tick()
            except Exception as e:
                log(f"tick error {err_tag(e)}")
            if args.once or args.dry_run:
                return 0
            time.sleep(PUSH_SEC)
    except KeyboardInterrupt:
        log("stopped")
        return 0
    finally:
        if lock is not None:
            lock.close()


if __name__ == "__main__":
    try:
        raise SystemExit(main(sys.argv[1:]))
    except SystemExit:
        raise
    except Exception as e:
        log(f"fatal {err_tag(e)}")
        raise
