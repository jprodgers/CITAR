"""Server registry, key store, usage ledger, cost engine and reports (temporary registry from tests/__init__.py)."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import os
import shutil
import tempfile
import time
import unittest
from datetime import datetime
from pathlib import Path

from citar import costing, keystore, servers as S, usage as U

T0 = datetime(2026, 3, 10, 12, 0).timestamp()         # a weekday in March (31 days)


def ledger(spans, acts=None, power=None):
    return {"acts": acts or {}, "spans": spans, "power": power or {}}


class CostTests(unittest.TestCase):
    def test_depreciation_energy_and_fees(self):
        reg = S.load()
        spans = [{"act": "a", "srv": "sv_gpu", "t0": T0, "t1": T0 + 3600, "held": 3600, "busy": 1800, "cpu": 0,
                  "model": "dry-run", "in": 1000, "out": 200}]
        r = costing.compute(T0, T0 + 3600, reg, ledger(spans, {"a": {"id": "a", "kind": "game"}}))
        c = r["acts"]["a"]["total"]
        self.assertAlmostEqual(c["depreciation"], 0.001, places=6)       # 87.66 over 10 years = 0.001/h
        self.assertAlmostEqual(c["kwh_idle"], 0.05, places=6)            # 50 W for an hour
        self.assertAlmostEqual(c["kwh_dynamic"], 0.15, places=6)         # GPU busy half the time: 300 W x 0.5
        self.assertAlmostEqual(c["energy_idle"] + c["energy_dynamic"], 0.2 * 0.22, places=6)   # 0.20/kWh + 10/500 fee
        self.assertAlmostEqual(costing.total(c, "marginal"), 0.001 + 0.15 * 0.22, places=6)
        self.assertAlmostEqual(c["energy_estimated_h"], 1.0)
        self.assertEqual(c["in"], 1000)

    def test_servers_page_machines_are_priced_under_old_ids_too(self):
        """A machine registered through the helper used to be left out of every cost report."""
        from unittest import mock
        plan = S.load()["electricity_plans"][0]["id"]
        machine = {"id": "sv_desk", "name": "Desk", "kind": "owned", "config": {
            "power": {"idle_w": 20, "cpu_max_w": 60, "gpu_max_w": 100},
            "components": [{"name": "Desk", "price": 876.6, "purchased": "2026-01-01", "lifespan_years": 1}],
            "costs": [{"from": "2000-01-01", "electricity_plan_id": plan}],
            "former_ids": ["sv_desk_old"]}}
        with mock.patch("citar.pool.seats.all_machines", lambda: [machine]):
            reg = S.with_pooled()
        self.assertEqual(reg["aliases"], {"sv_desk_old": "sv_desk"})
        spans = [{"act": a, "srv": srv, "t0": T0, "t1": T0 + 3600, "held": 3600, "busy": 1800, "cpu": 0, "model": "m"}
                 for a, srv in (("new", "sv_desk"), ("old", "sv_desk_old"))]
        r = costing.compute(T0, T0 + 3600, reg, ledger(spans))
        for a in ("new", "old"):
            c = r["acts"][a]["total"]
            self.assertAlmostEqual(c["kwh_idle"], 0.01, places=6)          # 20 W for an hour, split by two holders
            self.assertAlmostEqual(c["kwh_dynamic"], 0.05, places=6)       # both generating half the time: 100 W all hour, split
            self.assertAlmostEqual(c["depreciation"], 0.05, places=6)      # 876.6 / 8766 h, split by two
            self.assertGreater(c["energy_idle"] + c["energy_dynamic"], 0)
        self.assertFalse(any("deleted server" in n for n in r["notes"]))

    def test_concurrent_holders_split_the_calendar(self):
        reg = S.load()
        spans = [{"act": a, "srv": "sv_host", "t0": T0, "t1": T0 + 3600, "held": 3600, "cpu": 900, "busy": 0} for a in ("a", "b")]
        r = costing.compute(T0, T0 + 7200, reg, ledger(spans))
        for a in ("a", "b"):
            self.assertAlmostEqual(r["acts"][a]["total"]["depreciation"], 0.005, places=6)    # 0.01/h shared by two
        cal = r["servers"]["sv_host"]["calendar"]
        self.assertAlmostEqual(cal["depreciation"], 0.02, places=6)       # two hours of calendar time
        self.assertAlmostEqual(r["servers"]["sv_host"]["utilization"], 0.5, places=3)

    def test_api_tokens_and_measured_power(self):
        reg = S.load()
        spans = [{"act": "x", "srv": "sv_api", "t0": T0, "t1": T0 + 60, "held": 60, "busy": 30, "model": "claude-opus-5",
                  "in": 1_000_000, "out": 100_000, "cr": 2_000_000, "cw": 0}]
        r = costing.compute(T0, T0 + 60, reg, ledger(spans))
        self.assertAlmostEqual(r["acts"]["x"]["total"]["tokens"], 5.0 + 2.5 + 1.0, places=6)
        # measured power on the host: 50% CPU for a minute
        spans = [{"act": "m", "srv": "sv_host", "t0": T0, "t1": T0 + 60, "held": 60, "cpu": 30, "busy": 0}]
        pw = {"sv_host": [{"k": "pw", "srv": "sv_host", "t0": T0, "t1": T0 + 60, "n": 12, "cpu_util": 0.5, "gpu_w": None}]}
        r = costing.compute(T0, T0 + 60, reg, ledger(spans, power=pw))
        c = r["acts"]["m"]["total"]
        self.assertAlmostEqual(c["kwh_dynamic"], 20 / 60 / 1000, places=9)    # 0.5 x 40 W for one minute
        self.assertAlmostEqual(c["energy_measured_h"], 60 / 3600)

    def test_effective_dated_rates(self):
        reg = S.snapshot()
        plan = reg["electricity_plans"][0]
        plan["periods"].append({"from": "2026-03-11", "type": "flat", "rate_kwh": 1.0, "fixed_monthly": 0,
                                "fee_allocation": "none"})
        reg["electricity_plans"][0] = S.normalize_plan(plan)
        before = costing.fixed_rates(S.get("sv_gpu"), reg, datetime(2026, 3, 10, 23))
        after = costing.fixed_rates(S.get("sv_gpu"), reg, datetime(2026, 3, 11, 1))
        self.assertAlmostEqual(before["kwh"], 0.22)
        self.assertAlmostEqual(after["kwh"], 1.0)

    def test_tou_and_tiered_prices(self):
        plan = S.normalize_plan({"periods": [{"type": "tou", "rate_kwh": 0.1, "tou": [
            {"days": [0, 1, 2, 3, 4], "start": "16:00", "end": "21:00", "rate_kwh": 0.4}]}]})
        self.assertAlmostEqual(costing.energy_price(plan, datetime(2026, 3, 10, 17))[0], 0.4)
        self.assertAlmostEqual(costing.energy_price(plan, datetime(2026, 3, 10, 22))[0], 0.1)
        self.assertAlmostEqual(costing.energy_price(plan, datetime(2026, 3, 14, 17))[0], 0.1)   # Saturday
        tiered = S.normalize_plan({"periods": [{"type": "tiered", "household_kwh_month": 900, "tiers": [
            {"up_to_kwh": 500, "rate_kwh": 0.1}, {"up_to_kwh": None, "rate_kwh": 0.3}]}]})
        self.assertAlmostEqual(costing.energy_price(tiered, datetime(2026, 3, 10))[0], 0.3)

    def test_component_lifespans(self):
        sv = S.normalize_server({"kind": "owned", "components": [
            {"name": "old GPU", "price": 8766, "purchased": "2020-01-01", "lifespan_years": 1},
            {"name": "new PC", "price": 8766 * 4, "purchased": "2026-01-01", "lifespan_years": 4, "resale": 0}]})
        self.assertAlmostEqual(costing.depreciation_rate(sv, datetime(2026, 3, 1)), 1.0 / 1, places=6)
        self.assertAlmostEqual(costing.depreciation_rate(sv, datetime(2026, 3, 1), lifespan_override=2), 2.0, places=6)


class RegistryTests(unittest.TestCase):
    def test_resolve_seat_reference(self):
        ref = S.seat_ref("sv_dryrun", "m_dry", persona="calm")
        cfg = S.resolve_llm(ref)
        self.assertEqual((cfg["provider"], cfg["model"], cfg["server_id"], cfg["persona"]), ("dryrun", "dry-run", "sv_dryrun", "calm"))
        with self.assertRaises(S.ServerError):
            S.seat_ref("sv_dryrun", "nope")
        passthrough = {"provider": "dryrun", "model": "x"}
        self.assertEqual(S.resolve_llm(passthrough), passthrough)

    def test_profiles_and_load_args(self):
        sv = S.normalize_server({"connection": {"provider": "lmstudio"}, "models": [{"key": "q/q", "profiles": [
            {"id": "p_big", "name": "Big", "context": 65536, "gpu": "0.5", "parallel": 2, "ttl": 600, "extra": "--speculative-draft-mtp --evil"}],
            "default_profile": "p_big"}]})
        m = sv["models"][0]
        args = S.load_args(m["key"], S.profile(m, None))
        self.assertEqual(args, ["load", "q/q", "-y", "-c", "65536", "--gpu", "0.5", "--parallel", "2", "--ttl", "600",
                                "--speculative-draft-mtp"])

    def test_hardware_import(self):
        from citar import hwinfo
        hw = {"format": "citar-hardware", "cpu": {"model": "X", "threads": 16}, "gpus": [{"name": "G", "vram_gb": 24}],
              "memory": {"ram_gb": 64}, "power": {"suggested": {"idle_w": 60, "cpu_max_w": 125, "gpu_max_w": 300}}}
        sv = S.apply_hardware(S.normalize_server({"name": "d"}), hw, "imported")
        self.assertEqual((sv["power"]["idle_w"], sv["power"]["gpu_max_w"], sv["power"]["source"]), (60, 300, "guess"))
        self.assertIn("64 GB RAM", S.hardware_summary(sv["hardware"]))
        with self.assertRaises(S.ServerError):
            S.apply_hardware(sv, {"cpu": {}})
        self.assertIn("idle_w", hwinfo.guess_power({"system": {"form": "laptop"}, "cpu": {"model": "i5-13420H", "threads": 12},
                                                    "memory": {}, "gpus": []}))


class KeystoreTests(unittest.TestCase):
    def test_encrypted_file_backend(self):
        if not keystore.backends()["file"]["available"]:
            self.skipTest("cryptography not installed")
        sv = {"id": "sv_k", "connection": {"key": {"backend": "file"}}}
        with self.assertRaises(ValueError):
            keystore.store("sv_k", "file", "secret")          # locked
        keystore.unlock("pass phrase")
        keystore.store("sv_k", "file", "sk-test-123456789")
        self.assertEqual(keystore.get(sv), "sk-test-123456789")
        self.assertEqual(keystore.status(sv)["hint"], "…6789")
        self.assertNotIn(b"sk-test", Path(os.environ["CITAR_KEYS_FILE"]).read_bytes())
        keystore.lock()
        self.assertIsNone(keystore.get(sv))
        with self.assertRaises(ValueError):
            keystore.unlock("wrong")
        keystore.unlock("pass phrase")
        keystore.delete("sv_k", "file")
        self.assertIsNone(keystore.get(sv))
        keystore.lock()

    def test_env_backend(self):
        os.environ["CITAR_TEST_KEY"] = "abc"
        self.assertEqual(keystore.get({"id": "x", "connection": {"key": {"backend": "env", "env": "CITAR_TEST_KEY"}}}), "abc")


class LedgerAndReportTests(unittest.TestCase):
    def test_game_is_metered_and_reported(self):
        from citar.server.session import SessionManager
        from citar import reports
        mgr = SessionManager()
        seats = [{"type": "llm", "llm": S.seat_ref("sv_gpu", "m_gpu", dry_run_delay=0.05)}, {"type": "bot"}]
        s = mgr.create({"map_size": "duel", "map_type": "pangaea", "seed": 3, "turn_limit": 3, "barbarians": "off"}, seats, "metered")
        deadline = time.time() + 120
        tr = U.tracker()
        while s.game.s.phase == "playing" and time.time() < deadline:
            time.sleep(0.2)
            tr._sample()
        self.assertNotEqual(s.game.s.phase, "playing")
        mgr.delete(s.id)
        tr.flush()
        led = U.read()
        act = f"game:{s.id}"
        self.assertIn(act, led["acts"])
        mine = [sp for sp in led["spans"] if sp["act"] == act]
        self.assertTrue(any(sp["srv"] == "sv_gpu" and sp.get("busy", 0) > 0 and sp.get("in", 0) > 0 for sp in mine), mine)
        self.assertTrue(any(sp["srv"] == "sv_host" for sp in mine))
        runner = reports.ReportRunner(Path(tempfile.mkdtemp(prefix="citar-reports-")))
        m = runner.start({"title": "Test", "preset": "everything", "sections": reports.ALL_SECTIONS,
                          "scope": {"items": [{"kind": "game", "id": s.id}]},
                          "narrative": {"enabled": True, "server_id": "sv_dryrun", "model_id": "m_dry"}})
        deadline = time.time() + 120
        while runner.meta(m["id"])["status"] in ("queued", "running") and time.time() < deadline:
            time.sleep(0.2)
        meta = runner.meta(m["id"])
        self.assertEqual(meta["status"], "done", meta.get("trace") or meta.get("error"))
        page = runner.html_path(m["id"]).read_text(encoding="utf-8")
        self.assertIn("dry-run @ Test GPU box", page)
        self.assertIn("Dry run narrative", page)
        self.assertNotIn("This section failed", page)
        self.assertEqual(meta["summary"]["activities"], 1)
        self.assertGreater(meta["summary"]["total"], 0)
        shutil.rmtree(runner.dir, ignore_errors=True)


class ReportQueueTests(unittest.TestCase):
    def test_a_report_waits_in_the_machine_queue(self):
        """A report whose analysis is written by a model used to run at once, busy machine or not, and was not in
        the queue at all."""
        from unittest import mock
        from citar import reports
        from citar.pool import queue as work_queue
        busy = {"on": True}
        runner = reports.ReportRunner(Path(tempfile.mkdtemp(prefix="citar-reports-")))
        runner.POLL_SECONDS = 0.2
        with mock.patch("citar.pool.seats.occupied", lambda sid, exclude=frozenset(): ["game “x”"] if busy["on"] and sid == "sv_dryrun" else []):
            plain = runner.start({"title": "No model", "sections": ["summary"]})
            m = runner.start({"title": "Written", "sections": ["summary"],
                              "narrative": {"enabled": True, "server_id": "sv_dryrun", "model_id": "m_dry"}})
            deadline = time.time() + 60
            while runner.meta(plain["id"])["status"] != "done" and time.time() < deadline:
                time.sleep(0.1)
            self.assertEqual(runner.meta(plain["id"])["status"], "done", "a report without a model does not wait")
            time.sleep(0.6)
            meta = runner.meta(m["id"])
            self.assertEqual(meta["status"], "queued")
            self.assertIn("in use by", meta["waiting"])
            queued = [it for it in work_queue.waiting("sv_dryrun") if it["kind"] == "report"]
            self.assertEqual([it["id"] for it in queued], [m["id"]])
            busy["on"] = False
            while runner.meta(m["id"])["status"] in ("queued", "running") and time.time() < deadline:
                time.sleep(0.1)
        meta = runner.meta(m["id"])
        self.assertEqual(meta["status"], "done", meta.get("error"))
        self.assertIn("Dry run narrative", runner.html_path(m["id"]).read_text(encoding="utf-8"))
        shutil.rmtree(runner.dir, ignore_errors=True)


if __name__ == "__main__":
    unittest.main()
