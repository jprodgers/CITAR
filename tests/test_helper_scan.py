"""The CITAR helper's hardware scan and power samples (design/phase4-helper.md P4.3.5, P4.5.7; package 4-06).

- ``PowerGuess`` (gate 1): ``tests/data/hwinfo_power_cases.json`` holds twelve ``hwinfo.collect`` outputs (two recorded
  on Windows and in WSL, ten derived so that every branch is taken) with ``hwinfo.guess_power``'s answer for each; the
  answers are still the collector's, and the Rust port gives the same (``crates/citar-helper/tests/power_guess.rs``).
- ``Scan`` (gate 1): ``citar-helper scan`` on this machine (``citar-fake-probe`` standing in for ``nvidia-smi``, the
  rest of the machine's probes real) is a scan ``machine_settings.valid_scan`` keeps whole (its cleaned copy equals
  it), in the collector's format, and its cores, threads and RAM (within 1%) are ``hwinfo.collect``'s on the same
  machine. ``hwinfo.collect`` gives no cores without psutil on Linux: the comparison then holds threads and RAM.
- ``Sensors`` (gates 2 and 3): the real binary against the real hub, scanning and measuring with ``citar-fake-probe``
  as its ``nvidia-smi`` (named by ``CITAR_HELPER_NVIDIA_SMI``, so the real ``nvidia-smi.exe`` in System32 is never run):
  its hello's scan is the machine's last report; a rescan brings a ``hardware`` frame within 5 s, kept whole; a minute
  of samples lands as the machine's power row, every sample the fake's draw, every reading a call in the fake's log;
  and costing prices the helper machine's energy from the row.

The binary is the one ``cargo xtask develop`` builds into ``CITAR_EXT_DIR`` with the fake beside it (test.yml's
build-ext job on CI's three OSes); without them ``Scan`` and ``Sensors`` skip locally and fail on CI.
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)

import json
import os
import subprocess
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

from citar import db, hwinfo, machine_settings, servers, settings
from citar.auth import accounts, tokens
from citar.db.models import Base, Server, WorkerToken
from citar.pool import scheduler as S
from citar.server import workers as W
from tests import helperfake as H

ROOT = Path(__file__).resolve().parent.parent
CASES = ROOT / "tests" / "data" / "hwinfo_power_cases.json"
_TMP = Path(tempfile.mkdtemp(prefix="citar_helper_scan_"))
_URL = "sqlite:///" + (_TMP / "test.db").as_posix()
#: The fake's answers (crates/citar-helper/src/bin/fake_probe.rs).
FAKE_GPU = "CITAR Fake GPU"
FAKE_WATTS = 123.45


def _binaries(what: str) -> tuple:
    """The helper and the fake probe beside it: skipped locally without them, failed on CI."""
    binary = H.require_helper_binary(what)
    fake = H.fake_probe()
    if fake is None:
        if os.environ.get("CI"):
            raise H.NoHelperBinary(f"{what}: no citar-fake-probe beside {binary}")
        raise unittest.SkipTest(f"{what}: no citar-fake-probe beside {binary} (cargo xtask develop builds it)")
    return binary, fake


class PowerGuess(unittest.TestCase):

    def test_the_recorded_answers_are_still_the_collectors(self):
        table = json.loads(CASES.read_text(encoding="utf-8"))
        self.assertEqual(len(table["cases"]), 12)
        for case in table["cases"]:
            with self.subTest(case["about"]):
                self.assertEqual(hwinfo.guess_power(case["info"]), case["guess"])
        # every branch is taken: the forms, unified memory, the CPU classes, limited and unlimited GPUs
        answers = {(c["guess"]["idle_w"], c["guess"]["cpu_max_w"]) for c in table["cases"]}
        self.assertTrue({(8, 30), (8, 60), (12, 25), (12, 55), (55, 65), (55, 125), (55, 200), (110, 280)} <= answers,
                        answers)


class Scan(unittest.TestCase):
    """``citar-helper scan`` on this machine against ``hwinfo.collect`` on the same machine."""

    @classmethod
    def setUpClass(cls):
        binary, fake = _binaries("the helper's scan")
        log = _TMP / "scan-probe.log"
        env = {k: v for k, v in os.environ.items() if not k.startswith(("CITAR_WORKER_", "CITAR_HELPER_"))}
        env.update(CITAR_HELPER_NVIDIA_SMI=fake, CITAR_FAKE_PROBE_LOG=str(log))
        started = time.perf_counter()
        out = subprocess.run([binary, "scan", "--no-llm"], capture_output=True, text=True, encoding="utf-8",
                             timeout=180, env=env, stdin=subprocess.DEVNULL)
        cls.seconds = time.perf_counter() - started
        if out.returncode != 0:
            raise AssertionError(f"citar-helper scan exited {out.returncode}: {out.stderr}")
        cls.scan = json.loads(out.stdout)
        cls.calls = log.read_text(encoding="utf-8").splitlines() if log.exists() else []
        # the collector, without its GPU and runtime probes (not compared; and LM Studio's CLI is not run)
        with mock.patch.object(hwinfo, "_nvidia", lambda: []), mock.patch.object(hwinfo, "_runtimes", lambda i: None):
            cls.collected = hwinfo.collect()

    def test_the_scan_is_kept_whole(self):
        kept = machine_settings.valid_scan(self.scan)
        self.assertIsNotNone(kept, "valid_scan refused it")
        self.assertEqual(kept, self.scan, "the cleaned copy lost or changed something")
        self.assertEqual((self.scan["format"], self.scan["collector"], self.scan["collector_version"]),
                         ("citar-hardware", "citar-helper", 2))
        for part in ("os", "system", "cpu", "memory", "power"):
            self.assertIsInstance(self.scan[part], dict, part)
        for part in ("gpus", "physical_disks", "volumes", "runtimes"):
            self.assertIsInstance(self.scan[part], list, part)
        self.assertEqual(set(self.scan["power"]["suggested"]), {"idle_w", "cpu_max_w", "gpu_max_w", "source", "note"})
        self.assertEqual(self.scan["power"]["suggested"], hwinfo.guess_power(self.scan),
                         "the scan's own guess is the collector's for the scan")

    def test_the_fake_stood_in_for_nvidia_smi(self):
        self.assertEqual(self.scan["gpus"][0]["name"], FAKE_GPU)
        self.assertEqual(self.scan["gpus"][0]["power_now_w"], FAKE_WATTS)
        self.assertEqual(len(self.calls), 1, self.calls)
        self.assertIn("--query-gpu=name,memory.total,driver_version,power.limit,power.draw,pci.bus_id", self.calls[0])

    def test_cores_threads_and_ram_are_the_collectors(self):
        mine, theirs = self.scan["cpu"], self.collected["cpu"]
        self.assertEqual(mine["threads"], theirs["threads"])
        if theirs.get("cores") is not None:            # hwinfo gives none on Linux without psutil
            self.assertEqual(mine["cores"], theirs["cores"])
        ram, their_ram = self.scan["memory"]["ram_gb"], self.collected["memory"]["ram_gb"]
        self.assertLessEqual(abs(ram - their_ram), 0.01 * their_ram, (ram, their_ram))
        self.assertEqual(self.scan["cpu"]["model"], theirs["model"])
        self.assertEqual(self.scan["system"].get("form"), self.collected["system"].get("form"))


class Sensors(unittest.TestCase):
    """The real binary scanning and measuring against the real hub, with the fake as its nvidia-smi."""

    @classmethod
    def setUpClass(cls):
        cls.binary, cls.fake = _binaries("the helper's sensors")
        os.environ["CITAR_DATA_DIR"] = str(_TMP)
        os.environ["CITAR_DB_URL"] = _URL
        settings.reset()
        db.dispose()
        db.configure(_URL)
        Base.metadata.drop_all(db.engine())
        db.create_all()
        servers.invalidate()
        S.reset()
        cls.server = H.HubServer()
        with db.session() as s:
            owner = accounts.create_user(s, handle="scanned", email="scanned@x.cc", password="a long enough phrase",
                                         status="active", email_verified=True)
            row = Server(owner_id=owner.id, name="measured", kind="owned", provider="openai_compatible",
                         reach="worker", max_concurrent=1,
                         config={"models": [{"key": "m"}], "power": {"idle_w": 20, "cpu_max_w": 60, "gpu_max_w": 200}})
            s.add(row)
            s.flush()
            raw, hashed = tokens.new_pair()
            s.add(WorkerToken(server_id=row.id, token_hash=hashed, prefix=tokens.prefix(raw), label="scan"))
            cls.sid, cls.token = row.id, raw
        servers.invalidate()

    @classmethod
    def tearDownClass(cls):
        cls.server.stop()
        S.reset()

    def _detected(self) -> dict:
        with db.session() as s:
            row = s.get(Server, self.sid)
            return dict(row.hardware_detected or {})

    def _rows(self) -> list:
        from citar import usage
        out = []
        for path in sorted(usage.USAGE_DIR.glob(f"power-{self.sid}-*.jsonl")):
            out += [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
        return out

    def test_its_scan_a_rescan_and_a_minute_of_samples_through_the_fake(self):
        from citar import costing, usage
        log = _TMP / "sensors-probe.log"
        helper = H.RealHelper(self.server.base, self.token, "http://127.0.0.1:9/v1", name="measured",
                              sensors=True, env={"CITAR_HELPER_NVIDIA_SMI": self.fake,
                                                 "CITAR_FAKE_PROBE_LOG": str(log)})
        self.addCleanup(helper.stop)
        connected = H.until(lambda: W.hub().get(self.sid) is not None and W.hub().get(self.sid).ack is not None, 60)
        self.assertTrue(connected, f"the helper did not connect:\n{helper.text()[-3000:]}")
        welcomed = time.perf_counter()

        # The hello's scan is the machine's last report, whole.
        self.assertTrue(H.until(lambda: self._detected().get("hardware"), 10), "the hello's scan was kept")
        first = self._detected()
        scan = first["hardware"]
        self.assertEqual((scan["collector"], scan["gpus"][0]["name"]), ("citar-helper", FAKE_GPU))
        self.assertEqual(machine_settings.valid_scan(scan), scan)

        # A rescan brings a hardware frame within 5 s (the machine's own probes but nvidia-smi are real).
        asked = time.perf_counter()
        self.assertTrue(W.hub().rescan(self.sid))
        rescanned = H.until(lambda: self._detected().get("hardware_at") != first.get("hardware_at"), 10)
        took = time.perf_counter() - asked
        self.assertTrue(rescanned, f"no hardware frame after the rescan:\n{helper.text()[-3000:]}")
        self.assertLess(took, 5.0, f"the rescan's frame took {took:.2f} s")
        self.assertEqual(machine_settings.valid_scan(self._detected()["hardware"]), self._detected()["hardware"])

        # A minute of samples: one power row of the machine, every sample the fake's draw.
        self.assertTrue(H.until(lambda: self._rows(), 80), f"no power row:\n{helper.text()[-3000:]}")
        minute = time.perf_counter() - welcomed
        rows = self._rows()
        self.assertEqual(len(rows), 1, rows)
        row = rows[0]
        self.assertEqual((row["srv"], row["n"], row["gpu_w"]), (self.sid, 6, FAKE_WATTS))
        self.assertTrue(row["cpu_util"] is not None and 0 <= row["cpu_util"] <= 1, row)
        self.assertGreater(minute, 55, "a minute's samples, not fewer")
        self.assertAlmostEqual(row["t1"] - row["t0"], 60.0, delta=1.0)

        # Every reading was a call of the fake's: two scans (the hello's, the rescan's) and the minute's six readings
        # (a seventh may be under way). The real nvidia-smi never answered: every sample is the fake's figure.
        calls = [line.split(" ", 1)[1] for line in log.read_text(encoding="utf-8").splitlines()]
        scans = [c for c in calls if c.startswith("--query-gpu=name,")]
        draws = [c for c in calls if c == "--query-gpu=power.draw --format=csv,noheader,nounits"]
        self.assertEqual(len(scans), 2, calls)
        self.assertIn(len(draws), (6, 7), calls)
        self.assertEqual(len(scans) + len(draws), len(calls), calls)

        # Costing prices the helper machine's energy from its row: a game holding it over that minute is measured.
        since, until = row["t0"] - 1, row["t1"] + 1
        ledger = usage.read(since, until)
        ledger["spans"].append({"act": "game:measured", "srv": self.sid, "t0": row["t0"], "t1": row["t1"],
                                "held": row["t1"] - row["t0"], "busy": 30.0})
        r = costing.compute(since, until, ledger=ledger)
        c = r["acts"]["game:measured"]["servers"][self.sid]
        self.assertGreater(c["energy_measured_h"], 0, c)
        self.assertEqual(c["energy_estimated_h"], 0, c)
        self.assertEqual(r["servers"][self.sid]["metered"]["minutes"], 1)


if __name__ == "__main__":
    unittest.main()
