"""Offline tests for sms_forward.py.

The phones are fakes that answer like `adb shell`, and the agent is a small HTTP server
on 127.0.0.1 with a port picked by the system. No real adb is run and nothing outside
this machine is contacted. All phone numbers, serials and texts are made up.

    python -m unittest            (from tools/sms_forward)
    python test_sms_forward.py
"""
from __future__ import annotations

import datetime as dt
import http.server
import json
import os
import re
import shutil
import socket
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

# Settings of the machine running the tests must not leak in, and nothing may be written
# to the real state folder: clean the environment before the module reads it.
_TMP = tempfile.mkdtemp(prefix="sms-forward-test-")
for _name in list(os.environ):
    if _name.startswith("SMS_") or _name in ("AGENT_URL", "AGENT_TOKEN", "ADB_BIN"):
        del os.environ[_name]
os.environ["SMS_HOME"] = _TMP
os.environ["AGENT_URL"] = "http://127.0.0.1:9"  # nothing listens there; tests use FakeAgent

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import sms_forward as sf  # noqa: E402


def _no_subprocess(cmd, timeout):
    raise AssertionError(f"a test tried to run a real program: {cmd[:2]}")


sf._run = _no_subprocess
sf.SEND_GAP_SEC = 0
sf.print = lambda *args, **kwargs: None  # the log still goes to its file; keep the test output readable

P1, P2 = "TESTSERIAL0001", "TESTSERIAL0002"


def tearDownModule() -> None:
    shutil.rmtree(_TMP, ignore_errors=True)


class FakePhone:
    """Answers the shell commands sms_forward sends to one phone."""

    def __init__(self, model: str, root: bool = True) -> None:
        self.model = model
        self.root = root           # `su` works
        self.unlocked = True       # unlocked since boot
        self.shell_denied = False  # the plain shell may not read the SMS provider
        self.nodir = False         # no hook queue folder
        self.queue: dict[str, tuple[int, str]] = {}
        self.inbox: list[dict] = []

    @staticmethod
    def rows(rows: list[dict]) -> str:
        if not rows:
            return "No result found.\n"
        return "".join(f"Row: {i} _id={r['id']}, address={r['addr']}, date={r['date']}, "
                       f"sub_id={r['sub']}, body={r['body']}\n" for i, r in enumerate(rows))

    def run(self, cmd: str, root: bool) -> str:
        if cmd == "getprop ro.product.model":
            return self.model + "\n"
        if cmd == "getprop sys.user.0.ce_available":
            return "true\n" if self.unlocked else "false\n"
        if "@@FILE" in cmd:
            if self.nodir:
                return "@@NODIR\n"
            return "".join(f"@@FILE {n} {m}\n{j}\n" for n, (m, j) in sorted(self.queue.items())) + "@@END\n"
        if "rm -f" in cmd:
            for name in cmd.split("rm -f", 1)[1].split():
                self.queue.pop(name, None)
            return ""
        if "content://telephony/siminfo" in cmd:
            if not root:
                return "Error while accessing provider:telephony\n"
            return ("Row: 0 _id=3, sim_id=-1, icc_id=8900000000000000003, number=+15555550190, carrier_name=Old\n"
                    "Row: 1 _id=10, sim_id=0, icc_id=8900000000000000010, number=+15555550100, "
                    "carrier_name=Test Mobile\n")
        if "content://sms/inbox" in cmd:
            if self.shell_denied and not root:
                return "Error while accessing provider:sms\njava.lang.SecurityException: denied\n"
            if not self.unlocked:
                return "No result found.\n"
            if "--where" not in cmd:
                return "".join(f"Row: {i} _id={r['id']}, date={r['date']}\n" for i, r in enumerate(self.inbox)) \
                    or "No result found.\n"
            m = re.search(r"_id>(\d+) OR date>(\d+)", cmd)
            last_id, last_date = int(m.group(1)), int(m.group(2))
            new = [r for r in self.inbox if r["id"] > last_id or r["date"] > last_date]
            return self.rows(sorted(new, key=lambda r: r["id"]))
        return ""


class FakeAdb:
    def __init__(self, phones: dict[str, FakePhone]) -> None:
        self.phones = phones
        self.states = {s: "device" for s in phones}
        self.cmds: list[tuple[str, str]] = []  # (serial, shell command)

    def __call__(self, args: list[str], timeout: float = 20):
        if args == ["devices"]:
            return 0, "List of devices attached\n" + "".join(f"{s}\t{st}\n" for s, st in self.states.items()) + "\n", ""
        assert args[0] == "-s" and args[2] == "shell" and len(args) == 4, args
        assert "kill-server" not in " ".join(args)
        serial, cmd = args[1], args[3]
        self.cmds.append((serial, cmd))
        phone = self.phones[serial]
        if cmd.startswith("su -c '"):
            if not phone.root:
                return 127, "", "/system/bin/sh: su: inaccessible or not found\n"
            return 0, phone.run(cmd[len("su -c '"):-1], True), ""
        return 0, phone.run(cmd, False), ""

    def su_queue_reads(self, serial: str) -> int:
        return sum(1 for s, c in self.cmds if s == serial and c.startswith("su -c") and "@@FILE" in c)


class FakeAgent:
    """Stands in for the Myna agent: GET /health and POST /calls/api/notify/send."""

    def __init__(self) -> None:
        self.requests: list[dict] = []
        self.notify: tuple[int, object] = (200, {"ok": True})
        self.health: tuple[int, object] = (200, {"active_call_count": 0, "language": "en"})
        agent = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def reply(self, status: int, payload: object) -> None:
                raw = payload.encode() if isinstance(payload, str) else json.dumps(payload).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(raw)))
                self.end_headers()
                self.wfile.write(raw)

            def record(self, body: bytes) -> None:
                agent.requests.append({"method": self.command, "path": self.path,
                                       "headers": {k.lower(): v for k, v in self.headers.items()},
                                       "body": body})

            def do_GET(self) -> None:
                self.record(b"")
                if self.path == "/health":
                    self.reply(*agent.health)
                else:
                    self.reply(404, {"error": "not found"})

            def do_POST(self) -> None:
                self.record(self.rfile.read(int(self.headers.get("Content-Length") or 0)))
                if self.path == "/calls/api/notify/send":
                    self.reply(*agent.notify)
                else:
                    self.reply(404, {"error": "not found"})

            def log_message(self, *args) -> None:
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def sent(self) -> list[dict]:
        return [json.loads(r["body"].decode("utf-8")) for r in self.requests if r["method"] == "POST"]

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(5)


def hook_json(addr: str, body: str) -> str:
    return json.dumps({"v": 1, "addr": addr, "body": body, "ts": 1}, ensure_ascii=False)


def qname(ch: str) -> str:
    return ch * 32 + ".json"


def tick(fw: sf.Forwarder, inbox: bool = True) -> None:
    fw.next_devices = 0
    if inbox:
        fw.next_inbox = 0
    fw.tick()


class Base(unittest.TestCase):
    """Every test gets its own state folder and default settings."""

    def setUp(self) -> None:
        self.home = Path(tempfile.mkdtemp(dir=_TMP))
        for name, value in {
            "STATE": self.home / "state.json", "HEALTH": self.home / "health.json",
            "LOCK": self.home / "sms_forward.lock", "LOG": self.home / "sms_forward.log",
            "SERIALS": [], "SIM_NUMBERS": {}, "USE_SU": True, "FLOOD_CAP": 20,
            "SMS_LANG": "", "AGENT_TOKEN": "", "AGENT_URL": "http://127.0.0.1:9",
        }.items():
            self.patch(name, value)

    def patch(self, name: str, value) -> None:
        p = mock.patch.object(sf, name, value)
        p.start()
        self.addCleanup(p.stop)

    def log_text(self) -> str:
        return sf.LOG.read_text(encoding="utf-8") if sf.LOG.exists() else ""

    def forwarder(self, adb, **kw):
        """A forwarder with a recording delivery; returns (forwarder, sent, switch)."""
        sent: list[tuple[str, str]] = []
        up = {"v": True}

        def deliver(title: str, body: str):
            if not up["v"]:
                return False, "agent: no answer"
            sent.append((title, body))
            return True, "agent:200"

        kw.setdefault("in_call", lambda: False)
        kw.setdefault("lang", lambda: "en")
        return sf.Forwarder(adb=adb, deliver_fn=deliver, **kw), sent, up


class ParserTests(Base):
    def test_inbox_rows(self) -> None:
        out = ("Row: 0 _id=7, address=+15555550123, date=1700000000111, sub_id=10, body=first line, x=1\n"
               "Row: 1 _id=8 tail, address=y, body=z\n"
               "Row: 1 _id=9, address=NULL, date=1700000000999, sub_id=-1, body=NULL\n")
        rows = sf.parse_inbox(out)
        self.assertEqual([r["id"] for r in rows], [7, 9])
        self.assertEqual(rows[0]["body"], "first line, x=1\nRow: 1 _id=8 tail, address=y, body=z")
        self.assertEqual((rows[0]["addr"], rows[0]["sub"]), ("+15555550123", 10))
        self.assertEqual((rows[1]["addr"], rows[1]["body"]), ("", ""))
        self.assertEqual(sf.parse_inbox("No result found.\n"), [])
        self.assertIsNone(sf.parse_inbox("Error while accessing provider:sms\n"))

    def test_queue_listing(self) -> None:
        q = sf.parse_queue("@@FILE " + qname("a") + " 200\n{\"addr\":\"1\",\"body\":\"b\"}\n"
                           "@@FILE " + qname("b") + " 100\nnot json\n@@FILE ../x 5\n{}\n@@END\n")
        self.assertEqual([(n[0], m) for n, m, _ in q], [("b", 100), ("a", 200)])
        self.assertIsNone(q[0][2])
        self.assertEqual(q[1][2], {"addr": "1", "body": "b"})
        self.assertEqual(sf.parse_queue("@@NODIR\n"), [])
        self.assertIsNone(sf.parse_queue("@@FILE " + qname("a") + " 1\n{}\n"))

    def test_queue_markers_inside_a_message_are_not_markers(self) -> None:
        body = hook_json("+15555550123", "@@NODIR @@END")
        self.assertIsNone(sf.parse_queue(f"@@FILE {qname('a')} 1\n{body}\n"))
        q = sf.parse_queue(f"@@FILE {qname('a')} 1\n{body}\n@@END\n")
        self.assertEqual(q[0][2]["body"], "@@NODIR @@END")

    def test_unreadable_queue_reason_never_quotes_messages(self) -> None:
        partial = f"@@FILE {qname('a')} 1\n" + hook_json("+15555550123", "PRIVATE-TEXT") + "\n"
        queue, why = sf.read_queue(lambda args, timeout=20: (1, partial, ""), P1)
        self.assertIsNone(queue)
        self.assertNotIn("PRIVATE-TEXT", why)
        self.assertNotIn("5555550123", why)
        queue, why = sf.read_queue(lambda args, timeout=20: (127, "", "sh: su: not found\n"), P1)
        self.assertEqual((queue, why), (None, "sh: su: not found"))

    def test_sim_labels(self) -> None:
        sims = ("Row: 0 _id=3, sim_id=-1, icc_id=8900000000000000003, number=NULL, carrier_name=Old\n"
                "Row: 1 _id=10, sim_id=0, icc_id=8900000000000000010, number=, carrier_name=Test Mobile\n"
                "Row: 2 _id=11, sim_id=1, icc_id=8900000000000000011, number=+15555550111, carrier_name=Other\n"
                "Row: 3 _id=12, sim_id=-1, icc_id=8901, number=+15555550112, carrier_name=Gone\n")
        subs, active = sf.parse_sims(sims)
        self.assertEqual(subs, {3: "Old", 10: "Test Mobile", 11: "+15555550111", 12: "+15555550112"})
        self.assertEqual(active, ["Test Mobile", "+15555550111"])
        self.patch("SIM_NUMBERS", sf._pairs(" 8900000000000000010 = +15555550110 ; bad ,=x"))
        self.assertEqual(sf.SIM_NUMBERS, {"8900000000000000010": "+15555550110"})
        subs, active = sf.parse_sims(sims)
        self.assertEqual(subs[10], "+15555550110")
        self.assertEqual(active, ["+15555550110", "+15555550111"])
        self.assertIsNone(sf.parse_sims("Error while accessing provider:telephony\n"))

    def test_dedup_key(self) -> None:
        # National and international spelling of one number, whitespace differences in the text.
        self.assertEqual(sf.sms_key(P1, "+15555550123", "a\nb  c"), sf.sms_key(P1, "(555) 555-0123", "a b c"))
        self.assertNotEqual(sf.sms_key(P1, "ExampleBank", "x"), sf.sms_key(P2, "ExampleBank", "x"))
        self.assertNotEqual(sf.sms_key(P1, "+15555550123", "x"), sf.sms_key(P1, "+15555550123", "y"))
        self.assertNotEqual(sf.sms_key(P1, "+15555550123", "x"), sf.sms_key(P1, "+15555550124", "x"))
        self.assertEqual(sf.sms_key(P1, "ExampleBank", "x"), sf.sms_key(P1, " examplebank ", "x"))

    def test_sender_is_masked_for_the_log(self) -> None:
        self.assertEqual(sf.mask_sender("+15555550123"), "...0123")
        self.assertEqual(sf.mask_sender("ExampleBank"), "[11 chars]")
        self.assertEqual(sf.mask_sender("12345"), "[5 chars]")
        self.assertEqual(sf.mask_sender(""), "[0 chars]")

    def test_message_text(self) -> None:
        now = time.time()
        title, text = sf.format_sms("en", "+15555550123", "+15555550100", "Line A\n  line B  ",
                                    P1, "Pixel 7", int(now * 1000), now)
        self.assertEqual(title, "New SMS")
        self.assertEqual(text.split("\n"), ["From: +15555550123", "To: +15555550100", "Message: Line A",
                                            "  line B  ", f"Device: Pixel 7 ({P1})"])
        _, late = sf.format_sms("en", "", "", "x" * 4000, P1, "", int((now - 3600) * 1000), now)
        self.assertIn("From: (unknown)", late)
        self.assertNotIn("To:", late)
        self.assertIn("x" * sf.BODY_MAX + "… (truncated)", late)
        self.assertNotIn("x" * (sf.BODY_MAX + 1), late)
        self.assertRegex(late, r"Received: \d{4}-\d\d-\d\d \d\d:\d\d \(forwarded late\)")
        self.assertTrue(late.endswith(f"Device: {P1}"))

    def test_times_use_the_local_time_zone(self) -> None:
        ts = 1700000000
        parsed = dt.datetime.fromisoformat(sf.iso(ts))
        self.assertIsNotNone(parsed.utcoffset())
        self.assertEqual(parsed.timestamp(), ts)
        self.assertEqual(parsed.utcoffset(), dt.datetime.fromtimestamp(ts).astimezone().utcoffset())
        _, late = sf.format_sms("en", "a", "", "b", P1, "", ts * 1000, ts + 3600)
        self.assertIn(time.strftime("%Y-%m-%d %H:%M", time.localtime(ts)), late)

    def test_errors_are_logged_without_their_message(self) -> None:
        try:
            raise ValueError("PRIVATE-TEXT from +15555550123")
        except ValueError as e:
            tag = sf.err_tag(e)
        self.assertRegex(tag, r"^ValueError at line \d+$")

        phone = FakePhone("Pixel 7")
        adb = FakeAdb({P1: phone})
        fw, _, _ = self.forwarder(adb)
        with mock.patch.object(sf, "phone_model", side_effect=RuntimeError("PRIVATE-TEXT")):
            tick(fw)  # one phone failing does not stop the cycle
        self.assertRegex(self.log_text(), rf"ERROR {P1} RuntimeError at line \d+")
        self.assertNotIn("PRIVATE-TEXT", self.log_text())

    def test_number_settings(self) -> None:
        with mock.patch.dict(os.environ, {"SMS_TEST_NUM": "7.5"}):
            self.assertEqual(sf._env_num("SMS_TEST_NUM", 4, 1, 60), 7.5)
        for bad, want in (("0", 1), ("9999", 60), ("inf", 60), ("nan", 4), ("soon", 4), ("", 4)):
            with mock.patch.dict(os.environ, {"SMS_TEST_NUM": bad}):
                self.assertEqual(sf._env_num("SMS_TEST_NUM", 4, 1, 60), want, bad)
        for value, want in (("0", False), ("off", False), ("No", False), ("1", True), ("", True)):
            with mock.patch.dict(os.environ, {"SMS_TEST_FLAG": value}):
                self.assertEqual(sf._env_flag("SMS_TEST_FLAG", True), want, value)

    def test_env_file(self) -> None:
        path = self.home / "sms_forward.env"
        path.write_text("# comment\nSMS_TEST_NEW = 'from file'\nSMS_TEST_SET=from file\nnot a setting\n",
                        encoding="utf-8")
        with mock.patch.dict(os.environ, {"SMS_TEST_SET": "from environment"}):
            os.environ.pop("SMS_TEST_NEW", None)
            sf._load_env(path)
            self.assertEqual(os.environ["SMS_TEST_NEW"], "from file")
            self.assertEqual(os.environ["SMS_TEST_SET"], "from environment")
        sf._load_env(self.home / "missing.env")  # no file is fine


class DeviceTests(Base):
    LISTING = ("* daemon not running; starting now at tcp:5037\n"
               "* daemon started successfully\n"
               "List of devices attached\n"
               f"{P1}\tdevice\n"
               f"{P2}\tunauthorized\n"
               "emulator-5554\toffline\n"
               "TESTSERIAL0003          device usb:1-2 product:test model:Test_Phone transport_id:4\n"
               "\n")

    def test_parse_adb_devices(self) -> None:
        self.assertEqual(sf.parse_devices(self.LISTING), {
            P1: "device", P2: "unauthorized", "emulator-5554": "offline", "TESTSERIAL0003": "device"})
        self.assertEqual(sf.parse_devices("List of devices attached\n\n"), {})
        self.assertIsNone(sf.parse_devices("error: cannot connect to daemon\n"))
        self.assertEqual(sf.adb_devices(lambda args, timeout=20: (0, self.LISTING, "")),
                         sf.parse_devices(self.LISTING))
        self.assertIsNone(sf.adb_devices(lambda args, timeout=20: (1, "", "no adb")))

    def test_every_phone_in_state_device_without_sms_serials(self) -> None:
        present = sf.parse_devices(self.LISTING)
        self.assertEqual(sf.select_ready(present, []), [P1, "TESTSERIAL0003"])

    def test_sms_serials_narrows_the_phones(self) -> None:
        present = sf.parse_devices(self.LISTING)
        self.assertEqual(sf.select_ready(present, ["TESTSERIAL0003"]), ["TESTSERIAL0003"])
        self.assertEqual(sf.select_ready(present, ["TESTSERIAL0003", P1]), ["TESTSERIAL0003", P1])
        self.assertEqual(sf.select_ready(present, [P2, "NOTATTACHED0001"]), [])  # unauthorized / absent

    def test_forwarder_only_touches_selected_phones(self) -> None:
        adb = FakeAdb({P1: FakePhone("Pixel 7"), P2: FakePhone("Pixel 9")})
        self.patch("SERIALS", [P2])
        fw, sent, _ = self.forwarder(adb)
        tick(fw)
        adb.phones[P1].inbox.append({"id": 1, "addr": "+15555550123", "date": int(time.time() * 1000),
                                     "sub": 10, "body": "for the other phone"})
        adb.phones[P2].inbox.append({"id": 1, "addr": "+15555550124", "date": int(time.time() * 1000),
                                     "sub": 10, "body": "for the selected phone"})
        tick(fw)
        self.assertEqual({s for s, _ in adb.cmds}, {P2})
        self.assertEqual(len(sent), 1)
        self.assertIn("for the selected phone", sent[0][1])
        self.assertEqual(list(sf.read_json(sf.HEALTH)["phones"]), [P2])

    def test_phones_that_are_not_ready_are_left_alone(self) -> None:
        adb = FakeAdb({P1: FakePhone("Pixel 7"), P2: FakePhone("Pixel 9")})
        adb.states[P2] = "unauthorized"
        fw, _, _ = self.forwarder(adb)
        tick(fw)
        self.assertEqual({s for s, _ in adb.cmds}, {P1})
        health = sf.read_json(sf.HEALTH)["phones"]
        self.assertEqual(health[P2]["adb"], "unauthorized")
        self.assertTrue(health[P1]["seeded"])


class AdbBinaryTests(Base):
    """Where adb is looked for. Only paths are resolved; nothing is executed."""

    def setUp(self) -> None:
        super().setUp()
        self.exe = "adb.exe" if os.name == "nt" else "adb"
        self.mine = self.home / "mine" / self.exe
        self.other = self.home / "other" / self.exe
        self.sdk = self.home / "sdk"
        for p in (self.mine, self.other, self.sdk / "platform-tools" / self.exe):
            p.parent.mkdir(parents=True)
            p.write_bytes(b"")
        env = mock.patch.dict(os.environ)
        env.start()
        self.addCleanup(env.stop)
        for name in ("ADB_BIN", "ANDROID_HOME", "ANDROID_SDK_ROOT"):
            os.environ.pop(name, None)

    def test_adb_bin_wins_and_is_not_second_guessed(self) -> None:
        os.environ["ADB_BIN"] = str(self.mine)
        self.assertEqual(sf.find_adb(str(self.other)), str(self.mine))
        os.environ["ADB_BIN"] = str(self.home / "missing" / self.exe)
        with mock.patch.object(sf.shutil, "which", return_value=None):
            self.assertEqual(sf.find_adb(str(self.other)), "")

    def test_agents_adb_then_path_then_sdk(self) -> None:
        with mock.patch.object(sf.shutil, "which", return_value=str(self.mine)):
            self.assertEqual(sf.find_adb(str(self.other)), str(self.other))
            self.assertEqual(sf.find_adb(str(self.home / "gone" / self.exe)), str(self.mine))
            self.assertEqual(sf.find_adb(), str(self.mine))
            self.assertEqual(sf.adb_candidates()[0], str(self.mine))
        os.environ["ANDROID_HOME"] = str(self.sdk)
        with mock.patch.object(sf.shutil, "which", return_value=None):
            self.assertEqual(sf.find_adb(), str(self.sdk / "platform-tools" / self.exe))
            names = sf.adb_candidates()
        self.assertIn(str(Path.cwd() / "platform-tools" / self.exe), names)
        self.assertIn(str(Path(sf.__file__).resolve().parents[2] / "platform-tools" / self.exe), names)

    def test_binary_in_use_is_kept(self) -> None:
        hint = {"v": str(self.other)}
        adb = sf.Adb(lambda: hint["v"])
        self.assertEqual(adb(["devices"]), (1, "", "no adb"))  # nothing chosen yet, nothing run
        with mock.patch.object(sf.shutil, "which", return_value=str(self.mine)):
            adb.refresh()
            self.assertEqual(adb.path, str(self.other))
            hint["v"] = ""  # agent stopped answering
            adb.refresh()
            self.assertEqual(adb.path, str(self.other))
            self.other.unlink()
            adb.refresh()
            self.assertEqual(adb.path, str(self.mine))

    def test_agent_reports_its_adb(self) -> None:
        agent = FakeAgent()
        self.addCleanup(agent.close)
        self.patch("AGENT_URL", agent.url)
        agent.health = (200, {"usb_bridge": {"adb_path": str(self.mine)}})
        self.assertEqual(sf.agent_adb_path(), str(self.mine))
        agent.health = (200, {"usb_bridge": {"adb_path": "adb"}})  # not a full path: not usable here
        self.assertEqual(sf.agent_adb_path(), "")
        agent.health = (200, {})
        self.assertEqual(sf.agent_adb_path(), "")


class ForwarderTests(Base):
    def test_end_to_end(self) -> None:
        p1, p2 = FakePhone("Pixel 7"), FakePhone("Pixel 9")
        p2.nodir = True
        p2.shell_denied = True
        old = int((time.time() - 86400 * 30) * 1000)
        for i in range(1, 6):
            p1.inbox.append({"id": i, "addr": "+15555550120", "date": old + i, "sub": 10, "body": f"OLD-TEXT {i}"})
        p1.queue[qname("1")] = (1, hook_json("+15555550120", "OLD-QUEUED"))
        p2.inbox.append({"id": 40, "addr": "+15555550121", "date": old, "sub": 21, "body": "OLD-OTHER"})
        adb = FakeAdb({P1: p1, P2: p2})
        fw, sent, up = self.forwarder(adb)

        # First sight: everything already there is skipped.
        tick(fw)
        self.assertEqual(sent, [])
        self.assertEqual(p1.queue, {})
        s1, s2 = fw.state["phones"][P1], fw.state["phones"][P2]
        self.assertTrue(s1["seeded"] and s1["queue_seeded"])
        self.assertEqual((s1["inbox_id"], s1["model"]), (5, "Pixel 7"))
        self.assertTrue(s2["seeded"])
        self.assertEqual(s2["inbox_id"], 40)  # read through su: the plain shell is refused

        # New SMS: hook file + inbox row (other number spelling, same text) -> sent once.
        now_ms = int(time.time() * 1000)
        p1.queue[qname("2")] = (int(time.time()), hook_json("+15555550123", "TEST-CODE 000000\nsecond line"))
        p1.inbox.append({"id": 6, "addr": "5555550123", "date": now_ms, "sub": 10,
                         "body": "TEST-CODE 000000\nsecond line"})
        tick(fw)
        self.assertEqual(len(sent), 1)
        self.assertEqual(sent[0][0], "New SMS")
        self.assertIn("From: +15555550123", sent[0][1])
        self.assertIn("Message: TEST-CODE 000000\nsecond line", sent[0][1])
        self.assertIn("To: +15555550100", sent[0][1])
        self.assertNotIn("forwarded late", sent[0][1])
        self.assertEqual((p1.queue, s1["inbox_id"]), ({}, 6))

        # Delivery down: hook file stays, inbox cursor stays; delivered after recovery.
        up["v"] = False
        p1.queue[qname("3")] = (int(time.time()), hook_json("+15555550124", "HELLO-TEXT"))
        p1.inbox.append({"id": 7, "addr": "+15555550124", "date": now_ms + 1, "sub": 10, "body": "HELLO-TEXT"})
        tick(fw)
        self.assertEqual((len(sent), len(p1.queue), s1["inbox_id"]), (1, 1, 6))
        self.assertIs(sf.read_json(sf.HEALTH)["notify_ok"], False)
        up["v"] = True
        tick(fw)
        self.assertEqual((len(sent), p1.queue, s1["inbox_id"]), (2, {}, 7))

        # Inbox-only SMS (no hook folder on the second phone); the SIM label comes from sub_id.
        p2.inbox.append({"id": 41, "addr": "ExampleBank", "date": now_ms, "sub": 10, "body": "BANK-TEXT 111111"})
        tick(fw)
        self.assertEqual(len(sent), 3)
        self.assertIn("From: ExampleBank", sent[2][1])
        self.assertIn(f"Device: Pixel 9 ({P2})", sent[2][1])

        # Reused _id (newest row deleted, id handed out again): still new by date.
        p1.inbox = [r for r in p1.inbox if r["id"] != 7]
        p1.inbox.append({"id": 7, "addr": "+15555550125", "date": now_ms + 50, "sub": 10, "body": "REUSED-ID"})
        tick(fw)
        self.assertEqual(len(sent), 4)
        self.assertIn("REUSED-ID", sent[3][1])

        # Queue-only path between inbox polls; the same text again in the window is not re-sent.
        p1.queue[qname("4")] = (int(time.time()), hook_json("+15555550126", "QUEUE-ONLY"))
        tick(fw, inbox=False)
        p1.queue[qname("5")] = (int(time.time()), hook_json("+15555550126", "QUEUE-ONLY"))
        tick(fw, inbox=False)
        self.assertEqual((len(sent), p1.queue), (5, {}))

        # A file that is not JSON is dropped, not retried forever.
        p1.queue[qname("6")] = (int(time.time()), "{broken")
        tick(fw, inbox=False)
        self.assertEqual((len(sent), p1.queue), (5, {}))

        # Stale cursor flood: skipped with one notice, not 25 SMS.
        for i in range(100, 125):
            p1.inbox.append({"id": i, "addr": "+15555550127", "date": now_ms + i, "sub": 10, "body": f"FLOOD {i}"})
        tick(fw)
        self.assertEqual(len(sent), 6)
        self.assertEqual(sent[5][0], "SMS forwarder")
        self.assertIn(f"Pixel 7 ({P1}): 25 messages", sent[5][1])
        self.assertNotIn("FLOOD 1", sent[5][1])
        self.assertEqual(s1["inbox_id"], 124)

        # State survives a restart; nothing is replayed.
        fw2, sent2, _ = self.forwarder(adb)
        tick(fw2)
        self.assertEqual(sent2, [])
        health = sf.read_json(sf.HEALTH)
        self.assertTrue(health["phones"][P1]["seeded"])
        self.assertEqual(health["phones"][P1]["inbox_id"], 124)
        self.assertEqual(health["phones"][P1]["adb"], "device")

        # Privacy: no message text and no complete phone number in the log or the saved state.
        log = self.log_text()
        self.assertIn("forwarded", log)
        self.assertIn("from=...0123", log)
        for name in ("state.json", "health.json"):
            saved = (self.home / name).read_text(encoding="utf-8")
            for secret in ("TEST-CODE", "second line", "HELLO-TEXT", "BANK-TEXT", "REUSED-ID", "QUEUE-ONLY",
                           "FLOOD 1", "OLD-", "ExampleBank", "broken", "5555550100", "5555550120",
                           "5555550121", "5555550123", "5555550124", "5555550125", "5555550126", "5555550127"):
                self.assertNotIn(secret, log, f"leaked into the log: {secret}")
                self.assertNotIn(secret, saved, f"leaked into {name}: {secret}")

    def test_backlog_of_a_newly_seen_phone_is_not_forwarded(self) -> None:
        p1 = FakePhone("Pixel 7")
        adb = FakeAdb({P1: p1})
        fw, sent, _ = self.forwarder(adb)
        tick(fw)
        now_ms = int(time.time() * 1000)

        # A second phone is plugged in later, with messages from before (some only minutes old).
        p2 = FakePhone("Pixel 9")
        for i in range(1, 4):
            p2.inbox.append({"id": i, "addr": "+15555550130", "date": now_ms - i * 60000, "sub": 10,
                             "body": f"BACKLOG-INBOX {i}"})
        p2.queue[qname("a")] = (int(time.time()) - 30, hook_json("+15555550131", "BACKLOG-QUEUE"))
        adb.phones[P2] = p2
        adb.states[P2] = "device"
        tick(fw)
        tick(fw)
        self.assertEqual(sent, [])
        self.assertEqual(p2.queue, {})
        self.assertEqual(fw.state["phones"][P2]["inbox_id"], 3)

        # What arrives from now on is forwarded, on both sources.
        p2.inbox.append({"id": 4, "addr": "+15555550132", "date": now_ms + 1000, "sub": 10, "body": "FRESH-INBOX"})
        p2.queue[qname("b")] = (int(time.time()), hook_json("+15555550133", "FRESH-QUEUE"))
        tick(fw)
        self.assertEqual(len(sent), 2)
        text = "\n".join(body for _, body in sent)
        self.assertIn("FRESH-INBOX", text)
        self.assertIn("FRESH-QUEUE", text)
        self.assertNotIn("BACKLOG", text)

        # A fresh start with an empty state folder treats every phone as new again.
        sf.STATE.unlink()
        p1.inbox.append({"id": 1, "addr": "+15555550134", "date": now_ms, "sub": 10, "body": "BACKLOG-AGAIN"})
        fw2, sent2, _ = self.forwarder(adb)
        tick(fw2)
        tick(fw2)
        self.assertEqual(sent2, [])

    def test_phone_without_root_is_read_through_the_inbox(self) -> None:
        phone = FakePhone("Test Phone", root=False)
        phone.inbox.append({"id": 1, "addr": "+15555550140", "date": 1, "sub": 1, "body": "BACKLOG"})
        adb = FakeAdb({P1: phone})
        fw, sent, _ = self.forwarder(adb)
        tick(fw)
        ph = fw.state["phones"][P1]
        self.assertTrue(ph["seeded"])
        self.assertNotIn("queue_seeded", ph)
        self.assertEqual(ph["inbox_id"], 1)
        self.assertEqual(fw.status[P1]["queue"], "unavailable")

        phone.inbox.append({"id": 2, "addr": "+15555550141", "date": int(time.time() * 1000), "sub": 1,
                            "body": "NO-ROOT-TEXT"})
        tick(fw)
        self.assertEqual(len(sent), 1)
        self.assertEqual(sent[0][1].split("\n"), ["From: +15555550141", "Message: NO-ROOT-TEXT",
                                                  f"Device: Test Phone ({P1})"])
        # `su` is not hammered every cycle on a phone that has none.
        reads = adb.su_queue_reads(P1)
        for _ in range(5):
            tick(fw)
        self.assertEqual(adb.su_queue_reads(P1), reads)
        self.assertIn("not readable (/system/bin/sh: su: inaccessible or not found)", self.log_text())

    def test_hook_queue_that_becomes_readable_later_starts_without_backlog(self) -> None:
        phone = FakePhone("Test Phone", root=False)
        adb = FakeAdb({P1: phone})
        fw, sent, _ = self.forwarder(adb)
        tick(fw)
        phone.root = True
        phone.queue[qname("a")] = (int(time.time()), hook_json("+15555550150", "QUEUED-BEFORE-ROOT"))
        fw.queue_fail.clear()  # the retry delay has passed
        tick(fw, inbox=False)
        self.assertEqual((sent, phone.queue), ([], {}))
        self.assertTrue(fw.state["phones"][P1]["queue_seeded"])
        phone.queue[qname("b")] = (int(time.time()), hook_json("+15555550151", "QUEUED-AFTER-ROOT"))
        tick(fw, inbox=False)
        self.assertEqual(len(sent), 1)
        self.assertIn("QUEUED-AFTER-ROOT", sent[0][1])

    def test_su_can_be_switched_off(self) -> None:
        self.patch("USE_SU", False)
        phone = FakePhone("Pixel 7")
        phone.queue[qname("a")] = (int(time.time()), hook_json("+15555550160", "IN-QUEUE"))
        adb = FakeAdb({P1: phone})
        fw, sent, _ = self.forwarder(adb)
        tick(fw)
        phone.inbox.append({"id": 1, "addr": "+15555550161", "date": int(time.time() * 1000), "sub": 10,
                            "body": "IN-INBOX"})
        tick(fw)
        self.assertEqual(len(sent), 1)
        self.assertIn("IN-INBOX", sent[0][1])
        self.assertEqual([c for _, c in adb.cmds if "su " in c], [])
        self.assertEqual(len(phone.queue), 1)
        self.assertEqual(sf.read_json(sf.HEALTH)["phones"][P1]["queue"], "off")

    def test_inbox_is_not_polled_during_a_call(self) -> None:
        phone = FakePhone("Pixel 7")
        adb = FakeAdb({P1: phone})
        busy = {"v": False}
        fw, sent, _ = self.forwarder(adb, in_call=lambda: busy["v"])
        tick(fw)
        busy["v"] = True
        now_ms = int(time.time() * 1000)
        phone.inbox.append({"id": 1, "addr": "+15555550170", "date": now_ms, "sub": 10, "body": "DURING-CALL"})
        phone.queue[qname("a")] = (int(time.time()), hook_json("+15555550171", "HOOK-DURING-CALL"))
        before = len(adb.cmds)
        tick(fw)
        self.assertEqual([body for _, body in sent if "DURING-CALL" in body and "HOOK" not in body], [])
        self.assertEqual(len(sent), 1)  # the hook queue is still read
        self.assertEqual([c for _, c in adb.cmds[before:] if "content://sms/inbox" in c], [])
        self.assertLessEqual(fw.next_inbox - time.time(), sf.IN_CALL_RECHECK_SEC)
        busy["v"] = False
        tick(fw)
        self.assertEqual(len(sent), 2)
        self.assertIn("Message: DURING-CALL", sent[1][1])

    def test_locked_phone_is_not_taken_for_an_empty_inbox(self) -> None:
        phone = FakePhone("Pixel 7")
        phone.unlocked = False
        phone.inbox.append({"id": 9, "addr": "+15555550180", "date": 5, "sub": 10, "body": "BEFORE-UNLOCK"})
        adb = FakeAdb({P1: phone})
        fw, sent, _ = self.forwarder(adb)
        tick(fw)
        tick(fw)
        self.assertIsNone(fw.state["phones"][P1].get("inbox_id"))
        phone.unlocked = True
        tick(fw)
        self.assertEqual((fw.state["phones"][P1]["inbox_id"], sent), (9, []))

    def test_dry_run_changes_nothing(self) -> None:
        phone = FakePhone("Pixel 7")
        adb = FakeAdb({P1: phone})
        fw, sent, _ = self.forwarder(adb)
        tick(fw)
        state_before = sf.STATE.read_bytes()
        health_before = sf.HEALTH.read_bytes()
        phone.queue[qname("a")] = (int(time.time()), hook_json("+15555550185", "DRY-HOOK"))
        phone.inbox.append({"id": 1, "addr": "+15555550186", "date": int(time.time() * 1000), "sub": 10,
                            "body": "DRY-INBOX"})
        dry, dry_sent, _ = self.forwarder(adb, dry_run=True)
        tick(dry)
        self.assertEqual(dry_sent, [])
        self.assertEqual(len(phone.queue), 1)
        self.assertEqual(sf.STATE.read_bytes(), state_before)
        self.assertEqual(sf.HEALTH.read_bytes(), health_before)
        log = self.log_text()
        self.assertEqual(log.count("dry-run would forward"), 2)
        self.assertNotIn("DRY-", log)
        # A phone never seen before: the dry run only reports what would be skipped.
        phone2 = FakePhone("Pixel 9")
        phone2.queue[qname("b")] = (1, hook_json("+15555550187", "DRY-BACKLOG"))
        adb.phones[P2], adb.states[P2] = phone2, "device"
        dry2, dry_sent2, _ = self.forwarder(adb, dry_run=True)
        tick(dry2)
        self.assertEqual((dry_sent2, len(phone2.queue)), ([], 1))
        self.assertEqual(sf.STATE.read_bytes(), state_before)

    def test_only_one_instance(self) -> None:
        first = sf.acquire_lock()
        self.assertIsNotNone(first)
        self.assertIsNone(sf.acquire_lock())
        first.close()
        again = sf.acquire_lock()
        self.assertIsNotNone(again)
        again.close()

    def test_main_once_exits_when_locked(self) -> None:
        holder = sf.acquire_lock()
        self.addCleanup(holder.close)
        with mock.patch.object(sf, "Forwarder", side_effect=AssertionError("must not start")):
            self.assertEqual(sf.main(["--once"]), 0)
        self.assertIn("holds the lock", self.log_text())


class LanguageTests(Base):
    def test_language_codes(self) -> None:
        for given, want in {"en": "en", "zh-TW": "zh-TW", "zh_tw": "zh-TW", "zh-Hant": "zh-TW", "zh": "zh-TW",
                            "zh-HK": "zh-TW", "zh-CN": "zh-CN", "zh_Hans": "zh-CN", "zh-SG": "zh-CN",
                            "JA": "ja", "ja-JP": "ja", "ko-KR": "ko", "es-MX": "es", "fr_CA": "fr",
                            "de-AT": "de", "pt-BR": "pt", "xx": "en", "": "en", "  ": "en"}.items():
            self.assertEqual(sf.normalize_lang(given), want, given)

    def test_all_nine_languages_are_complete(self) -> None:
        self.assertEqual(sorted(sf.TEXTS), sorted(["en", "zh-TW", "zh-CN", "ja", "ko", "es", "fr", "de", "pt"]))
        now = time.time()
        titles = set()
        for lang, texts in sf.TEXTS.items():
            self.assertEqual(set(texts), set(sf.TEXTS["en"]), lang)
            self.assertTrue(all(v.strip() for v in texts.values()), lang)
            title, text = sf.format_sms(lang, "+15555550123", "+15555550100", "Body stays as it is: 你好 ¡hola!",
                                        P1, "Pixel 7", int((now - 3600) * 1000), now)
            titles.add(title)
            lines = text.split("\n")
            self.assertEqual(len(lines), 5, lang)
            for line, key, value in zip(lines, ("from", "to", "text", "received", "device"),
                                        ("+15555550123", "+15555550100", "Body stays as it is: 你好 ¡hola!",
                                         f"({texts['late']})", f"Pixel 7 ({P1})")):
                self.assertTrue(line.startswith(texts[key] + texts["sep"]), (lang, line))
                self.assertTrue(line.endswith(value), (lang, line))
            flood_title, flood = sf.format_flood(lang, P1, "Pixel 7", 25)
            self.assertEqual(flood_title, texts["flood_title"])
            self.assertIn(f"Pixel 7 ({P1})", flood)
            self.assertIn("25", flood)
        self.assertEqual(len(titles), 9)
        self.assertEqual(sf.format_sms("no-such", "a", "", "b", P1, "", 0, now)[0], "New SMS")

    def test_sms_lang_wins_over_the_agent(self) -> None:
        asked = []
        lang = sf.Language("zh_CN", lambda: asked.append(1) or "ja")
        self.assertEqual(lang.code(), "zh-CN")
        self.assertEqual(asked, [])
        self.assertEqual(sf.Language("klingon", lambda: "ja").code(), "en")

    def test_agent_language_with_english_fallback(self) -> None:
        answer = {"v": None}
        lang = sf.Language("", lambda: answer["v"])
        self.assertEqual(lang.code(), "en")  # agent not running
        answer["v"] = "pt-BR"
        self.assertEqual(lang.code(), "en")  # asked again only after a pause
        lang.next_check = 0
        self.assertEqual(lang.code(), "pt")
        answer["v"] = None  # agent stops: the last known language stays
        lang.next_check = 0
        self.assertEqual(lang.code(), "pt")
        answer["v"] = "zh-TW"  # changed in the web console
        lang.next_check = 0
        self.assertEqual(lang.code(), "zh-TW")
        self.assertEqual(sf.Language("").code(), "en")

        def broken():
            raise OSError("down")

        self.assertEqual(sf.Language("", broken).code(), "en")

    def test_forwarded_message_uses_the_chosen_language(self) -> None:
        phone = FakePhone("Pixel 7")
        adb = FakeAdb({P1: phone})
        fw, sent, _ = self.forwarder(adb, lang=sf.Language("zh-TW").code)
        tick(fw)
        phone.inbox.append({"id": 1, "addr": "+15555550123", "date": int(time.time() * 1000), "sub": 10,
                            "body": "Text is not translated"})
        tick(fw)
        self.assertEqual(sent[0][0], "新簡訊")
        self.assertEqual(sent[0][1].split("\n"), ["寄件人：+15555550123", "收件號碼：+15555550100",
                                                  "內容：Text is not translated", f"裝置：Pixel 7 ({P1})"])


class AgentTests(Base):
    def setUp(self) -> None:
        super().setUp()
        self.agent = FakeAgent()
        self.addCleanup(self.agent.close)
        self.patch("AGENT_URL", self.agent.url)

    def test_deliver_success(self) -> None:
        ok, via = sf.deliver("New SMS", "From: +15555550123\nMessage: 測試 ✓")
        self.assertTrue(ok, via)
        req = self.agent.requests[-1]
        self.assertEqual((req["method"], req["path"]), ("POST", "/calls/api/notify/send"))
        self.assertEqual(json.loads(req["body"].decode("utf-8")),
                         {"title": "New SMS", "body": "From: +15555550123\nMessage: 測試 ✓"})
        self.assertTrue(req["headers"]["content-type"].startswith("application/json"))
        self.assertNotIn("authorization", req["headers"])

    def test_bearer_token(self) -> None:
        self.patch("AGENT_TOKEN", "test-token-123")
        self.assertTrue(sf.deliver("t", "b")[0])
        self.assertEqual(self.agent.requests[-1]["headers"]["authorization"], "Bearer test-token-123")
        sf.agent_health()
        self.assertEqual(self.agent.requests[-1]["path"], "/health")
        self.assertEqual(self.agent.requests[-1]["headers"]["authorization"], "Bearer test-token-123")

    def test_ok_false_is_not_delivered(self) -> None:
        self.agent.notify = (200, {"ok": False})
        ok, via = sf.deliver("t", "b")
        self.assertFalse(ok)
        self.assertIn("no notification channel", via)

    def test_failures_are_not_delivered(self) -> None:
        for reply in ((401, {"error": "unauthorized"}), (400, {"ok": False, "error": "body is empty"}),
                      (500, {"error": "boom"}), (502, "<html>bad gateway</html>"), (200, "not json"),
                      (200, {"ok": "true"}), (200, ["ok"]), (200, {}), (204, "")):
            self.agent.notify = reply
            ok, via = sf.deliver("t", "b")
            self.assertFalse(ok, reply)
            self.assertTrue(via.startswith("agent"), via)
        self.agent.notify = (401, {"error": "unauthorized"})
        self.assertIn("AGENT_TOKEN", sf.deliver("t", "b")[1])

    def test_agent_down(self) -> None:
        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            port = s.getsockname()[1]
        self.patch("AGENT_URL", f"http://127.0.0.1:{port}")
        self.patch("HEALTH_TIMEOUT", 0.5)
        self.patch("SEND_TIMEOUT", 0.5)
        ok, via = sf.deliver("t", "b")
        self.assertFalse(ok)
        self.assertIn("no answer", via)
        self.assertIsNone(sf.agent_health())
        self.assertFalse(sf.agent_in_call())
        self.assertIsNone(sf.agent_language())
        self.assertEqual(sf.agent_adb_path(), "")

    def test_health_fields(self) -> None:
        self.agent.health = (200, {"active_call_count": 2, "language": "zh-TW"})
        self.assertTrue(sf.agent_in_call())
        self.assertEqual(sf.agent_language(), "zh-TW")
        self.assertEqual(self.agent.requests[-1]["method"], "GET")
        self.agent.health = (200, {"active_call_count": 0})
        self.assertFalse(sf.agent_in_call())
        self.assertIsNone(sf.agent_language())
        for odd in ((200, {"active_call_count": "many"}), (200, {"active_call_count": None}),
                    (200, "not json"), (200, [1]), (500, {"active_call_count": 3})):
            self.agent.health = odd
            self.assertFalse(sf.agent_in_call(), odd)

    def test_forwarder_against_the_agent(self) -> None:
        """The real delivery, call check and language lookup, end to end with the fake agent."""
        self.patch("AGENT_TOKEN", "test-token-123")
        self.agent.health = (200, {"active_call_count": 0, "language": "ja"})
        phone = FakePhone("Pixel 7")
        adb = FakeAdb({P1: phone})
        fw = sf.Forwarder(adb=adb)
        tick(fw)
        self.assertEqual(self.agent.sent(), [])

        # No channel takes it: the message stays queued and the inbox cursor does not move.
        self.agent.notify = (200, {"ok": False})
        now_ms = int(time.time() * 1000)
        phone.queue[qname("a")] = (int(time.time()), hook_json("+15555550123", "AGENT-TEXT 1"))
        phone.inbox.append({"id": 1, "addr": "+15555550124", "date": now_ms, "sub": 10, "body": "AGENT-TEXT 2"})
        tick(fw)
        self.assertEqual(len(phone.queue), 1)
        self.assertEqual(fw.state["phones"][P1]["inbox_id"], 0)
        self.assertEqual(fw.state["dedup"], {})
        self.assertIs(sf.read_json(sf.HEALTH)["notify_ok"], False)
        self.assertIn("ERROR deliver failed", self.log_text())
        attempts = len(self.agent.sent())
        self.assertGreaterEqual(attempts, 1)

        # A channel is set up: both are delivered, once each, with the agent's language.
        self.agent.notify = (200, {"ok": True})
        tick(fw)
        delivered = self.agent.sent()[attempts:]
        self.assertEqual(len(delivered), 2)
        self.assertEqual({d["title"] for d in delivered}, {"新着SMS"})
        self.assertIn("本文：AGENT-TEXT 1", delivered[0]["body"])
        self.assertIn("本文：AGENT-TEXT 2", delivered[1]["body"])
        self.assertEqual((phone.queue, fw.state["phones"][P1]["inbox_id"]), ({}, 1))
        self.assertIs(sf.read_json(sf.HEALTH)["notify_ok"], True)
        tick(fw)
        self.assertEqual(len(self.agent.sent()), attempts + 2)
        posts = [r for r in self.agent.requests if r["method"] == "POST"]
        self.assertTrue(all(r["headers"]["authorization"] == "Bearer test-token-123" for r in posts))

        # A call starts: the inbox waits, and is read again when the call is over.
        self.agent.health = (200, {"active_call_count": 1, "language": "ja"})
        phone.inbox.append({"id": 2, "addr": "+15555550125", "date": now_ms + 5, "sub": 10, "body": "AGENT-TEXT 3"})
        tick(fw)
        self.assertEqual(len(self.agent.sent()), attempts + 2)
        self.agent.health = (200, {"active_call_count": 0, "language": "ja"})
        tick(fw)
        self.assertEqual(len(self.agent.sent()), attempts + 3)
        self.assertNotIn("AGENT-TEXT", self.log_text())


if __name__ == "__main__":
    unittest.main()
