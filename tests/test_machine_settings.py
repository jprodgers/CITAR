"""The machine settings dialog (design/phase3-features.md P3.8; Appendix D, 3-18).

- ``Report``: what a helper's report does to a machine, by section and mode (``citar.machine_settings``): the scan
  always kept with its time; hardware and power applied in ``auto`` and left alone in ``manual``; models merged in
  ``auto`` (unreported ones kept, marked), only their ``loaded`` flags in ``manual``; "apply" copying the scan whole
  or by section; the differences the dialog shows; the settings' checks.
- ``EndToEnd`` (gate 1), on ``tests/test_worker.py``'s end-to-end harness: a real uvicorn server and a real helper (the
  worker agent, its model call stubbed, its hardware from a file: ``--hardware``) over a real websocket, reconnecting
  with other hardware, models and power figures for each section and mode, and the dialog's "apply" over HTTP.
- ``Routes`` (gate 2 and P3.9.1): the settings routes the machine's owner's and administrators' (another member 404,
  a member a group lends it to 403, a visitor 401), the trust and class routes administrators' (anyone else 404);
  classification following the hardware unless pinned; what the owner and an administrator are shown; bad bodies;
  each change committed before the answer, with its audit row; a machine whose LLM is off refused by admission and
  the scheduler.
- ``Scheduler`` (gate 3): slots and swaps changed through the dialog's route change the prompt scheduler's capacity and
  its loaded-model rule at once (with the scheduler's own periodic reread turned off).
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import copy
import json
import os
import shutil
import socket
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

_TMP = Path(tempfile.mkdtemp(prefix="citar_machine_settings_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from sqlalchemy import event, select

from citar import db, ledger, machine_settings as M, pool, servers, settings
from citar.auth import accounts, policy, sessions, tokens
from citar.db.models import AuditLog, Base, HardwareClass, Server, ServerGroup, User, WorkerToken
from citar.pool import admission, scheduler
from citar.server import workers as W

settings.reset()


def tearDownModule():
    db.dispose()
    shutil.rmtree(_TMP, ignore_errors=True)


def _reset_db():
    db.dispose()
    db.configure(os.environ["CITAR_DB_URL"])
    Base.metadata.drop_all(db.engine())
    db.create_all()
    policy.invalidate()
    servers.invalidate()


def scan(*, gpu="Test GPU", vram=8, cpu="Test CPU", cores=8, ram=32, idle=40, cpu_w=90, gpu_w=150,
         host="settings-box") -> dict:
    """A helper's hardware scan, as ``hwinfo.collect`` writes one."""
    return {"format": "citar-hardware", "collector_version": 1, "collected_at": "2026-10-13T20:00:00",
            "hostname": host, "os": {"system": "Linux", "release": "6.8", "arch": "x86_64"},
            "system": {"form": "desktop"}, "cpu": {"model": cpu, "cores": cores, "threads": cores * 2},
            "memory": {"ram_gb": ram, "unified": False}, "gpus": [{"name": gpu, "vram_gb": vram}],
            "physical_disks": [{"name": "SSD", "size_gb": 1000, "type": "SSD"}], "volumes": [], "runtimes": [],
            "power": {"gpu_sampling": False, "suggested": {"idle_w": idle, "cpu_max_w": cpu_w, "gpu_max_w": gpu_w,
                                                           "source": "guess", "note": "a guess"}}}


def odd_scans() -> dict:
    """Scans no page could show as they came, one fault each (so one cannot hide another): figures no JSON answer
    carries (NaN, Infinity: Starlette refuses to encode them) or no float holds (an integer of 400 digits: ``float()``
    and ``math.isfinite`` overflow on it), each where a reader computes with it, and parts of other shapes. Python's
    JSON reader takes all of them from a helper's hello."""
    out = {}

    def odd(name, change):
        s = scan()
        change(s)
        out[name] = s
    odd("a GPU's memory NaN", lambda s: s["gpus"][0].update(vram_gb=float("nan")))
    odd("the RAM infinite", lambda s: s["memory"].update(ram_gb=float("inf")))
    odd("a GPU's memory of 400 digits", lambda s: s["gpus"][0].update(vram_gb=10 ** 400))
    odd("the RAM of 400 digits", lambda s: s["memory"].update(ram_gb=10 ** 400))
    odd("a disk of 400 digits", lambda s: s["physical_disks"][0].update(size_gb=10 ** 400))
    odd("the cores of 400 digits", lambda s: s["cpu"].update(cores=10 ** 400))
    odd("a suggested figure of 400 digits", lambda s: s["power"]["suggested"].update(idle_w=10 ** 400))
    odd("a suggested figure NaN", lambda s: s["power"]["suggested"].update(gpu_max_w=float("nan")))
    odd("a figure no reader computes with, -Infinity", lambda s: s.update(extra={"temps": [float("-inf")]}))
    odd("the memory a text", lambda s: s.update(memory="lots"))
    odd("a GPU a text", lambda s: s.update(gpus=["a GPU", *s["gpus"]]))
    odd("the GPUs a text", lambda s: s.update(gpus="one"))
    odd("the CPU a list", lambda s: s.update(cpu=["x"]))
    odd("the suggestion a list", lambda s: s["power"].update(suggested=[1, 2]))
    return out


def odd_scan() -> dict:
    """Every fault of ``odd_scans`` in one scan."""
    s = scan()
    s["gpus"] = [{"name": "Odd GPU", "vram_gb": float("nan")}, "not a GPU", {"name": "Huge GPU", "vram_gb": 10 ** 400}]
    s["memory"] = "lots"
    s["cpu"].update(threads=float("inf"), cores="twelve")
    s["physical_disks"] = [{"name": "SSD", "size_gb": 10 ** 400, "type": "SSD"}]
    s["runtimes"] = "none"
    s["power"]["suggested"].update(idle_w=float("nan"), cpu_max_w=10 ** 400)
    s["extra"] = {"temps": [float("-inf"), 1.5, 10 ** 400], "deep": nest("abcdefghijkl", 1)}
    return s


def nest(keys: str, leaf) -> dict:
    """``{keys[0]: {keys[1]: ... leaf}}``."""
    for key in reversed(keys):
        leaf = {key: leaf}
    return leaf


#: ``odd_scan`` as it is kept: the figures that do not read dropped (None), the wrong shapes emptied, the rest as sent.
ODD_CLEAN = dict(scan(), gpus=[{"name": "Odd GPU", "vram_gb": None}, {"name": "Huge GPU", "vram_gb": None}], memory={},
                 cpu={"model": "Test CPU", "cores": None, "threads": None},
                 physical_disks=[{"name": "SSD", "size_gb": None, "type": "SSD"}], runtimes=[],
                 power={"gpu_sampling": False, "suggested": {"idle_w": None, "cpu_max_w": None, "gpu_max_w": 150,
                                                             "source": "guess", "note": "a guess"}},
                 # extra is at depth 1 and deep at 2, so its tenth key's value (at 12) is past HARDWARE_DEPTH
                 extra={"temps": [None, 1.5, None], "deep": nest("abcdefghij", None)})


def encodes(value) -> bool:
    """Whether ``value`` encodes as the JSON an answer carries (no NaN, no Infinity)."""
    try:
        json.dumps(value, allow_nan=False)
        return True
    except ValueError:
        return False


HAND_SET = {"source": "manual", "cpu": {"model": "Hand-set CPU", "cores": 4, "threads": 4},
            "memory": {"ram_gb": 16, "unified": False}, "gpus": [{"name": "Hand-set GPU", "vram_gb": 12}],
            "physical_disks": [], "system": {"form": "desktop"}}


def model(key, *, loaded=False, label=None, context=None) -> dict:
    """A model as a helper reports it (``protocol.ModelInfo.client``)."""
    out = {"key": key, "label": key if label is None else label, "loaded": loaded}
    if context:
        out["context"] = context
    return out


def catalogue(row) -> dict:
    """A machine's catalogue as its row stores it: {key: entry}."""
    return {m["key"]: m for m in (row.config or {}).get("models") or []}


# ---------------------------------------------------------------------------- hardware details, cleaned

class Cleaning(unittest.TestCase):
    """``servers.clean_hardware``: what every way hardware details come in (a helper's scan, a collector's file, the
    owner's save) is kept as, so no page that shows the machine fails on them."""

    def test_a_good_scan_is_kept_as_it_came(self):
        good = scan()
        self.assertEqual(servers.clean_hardware(good), good)
        self.assertEqual(servers.clean_hardware(HAND_SET), HAND_SET)
        self.assertIsNot(servers.clean_hardware(good)["gpus"], good["gpus"], "a copy: the caller's dict is not changed")

    def test_figures_that_do_not_read_are_dropped_and_wrong_shapes_emptied(self):
        odd = odd_scan()
        self.assertEqual(servers.clean_hardware(odd), ODD_CLEAN)
        self.assertEqual(odd["memory"], "lots", "the caller's dict is not changed")
        for name, s in odd_scans().items():
            with self.subTest(name):
                self.assertTrue(encodes(servers.clean_hardware(s)))
        self.assertEqual(servers.clean_hardware({"memory": {"ram_gb": "32"}, "cpu": {"threads": " 16 "}}),
                         {"memory": {"ram_gb": 32.0}, "cpu": {"threads": 16.0}}, "a figure in text is read")
        self.assertEqual(servers.clean_hardware({"memory": {"ram_gb": True}, "gpus": [{"vram_gb": "a lot"}]}),
                         {"memory": {"ram_gb": None}, "gpus": [{"vram_gb": None}]})
        self.assertEqual(servers.clean_hardware({"n": 2 ** 53, "m": -(2 ** 53) - 1, "f": 1e300}),
                         {"n": 2 ** 53, "m": None, "f": None}, "within HARDWARE_FIGURE_MAX only")
        for not_hardware in (None, "x", [1], 3):
            self.assertEqual(servers.clean_hardware(not_hardware), {})

    def test_every_way_in_keeps_it_clean(self):
        """The registry's save of a machine (``normalize_server``) and a collector's file (``apply_hardware``)."""
        sv = servers.normalize_server({"name": "x", "hardware": odd_scan()})
        self.assertEqual(sv["hardware"], ODD_CLEAN)
        self.assertEqual(servers.normalize_server({"name": "x", "hardware": "lots"})["hardware"], {"source": "none"})
        applied = servers.apply_hardware(servers.normalize_server({"name": "x"}), odd_scan(), "imported")
        self.assertEqual(applied["hardware"], dict(ODD_CLEAN, source="imported"))
        self.assertEqual({k: applied["power"][k] for k in ("idle_w", "cpu_max_w", "gpu_max_w")},
                         {"idle_w": None, "cpu_max_w": None, "gpu_max_w": 150}, "only a suggestion that reads")
        self.assertTrue(encodes(servers.normalize_server(applied)))
        for v, want in ((10 ** 400, 7), (float("nan"), 7), (float("inf"), 7), ("1e999", 7), ("5", 5.0)):
            self.assertEqual(servers._num(v, 7), want, v if not isinstance(v, int) else "10**400")

    def test_the_readers_take_what_a_row_written_before_holds(self):
        """Costing's thread count (it divides by it) and the stored capabilities, from what a row may hold."""
        from citar import costing
        for threads, want in ((16, 16), ("16", 16), (0.5, 1), (-4, 1), (0, 8), (None, 8), ("many", 8),
                              (float("nan"), 8), (10 ** 400, 8), (True, 8)):
            with self.subTest(threads=str(threads)[:20]):
                self.assertEqual(costing._threads({"hardware": {"cpu": {"threads": threads}}}), want)
        self.assertEqual(costing._threads({"hardware": {"cpu": "x"}}), 8)
        cpu = M.capabilities({"cpu": {"threads": 10 ** 400, "memory_mb": float("nan"), "cpu_percent": "50"}})["cpu"]
        self.assertEqual((cpu["threads"], cpu["memory_mb"], cpu["cpu_percent"]), (0, 0, 50))


# ---------------------------------------------------------------------------- the report, on a row

class Report(unittest.TestCase):
    """``citar.machine_settings`` on rows of a database: the hello's report by section and mode, apply, the
    differences, the settings' checks."""

    def setUp(self):
        _reset_db()
        with db.session() as s:
            row = Server(name="Box", kind="owned", provider="lmstudio", reach="worker",
                         config={"hardware": HAND_SET, "power": {"idle_w": 11, "cpu_max_w": 22, "gpu_max_w": 33,
                                                                  "source": "measured"}})
            s.add(row)
            s.flush()
            self.sid = row.id

    def report(self, **kw) -> dict:
        with db.session() as s:
            return M.record_report(s.get(Server, self.sid), **kw)

    def row(self) -> Server:
        with db.session() as s:
            row = s.get(Server, self.sid)
            s.expunge(row)
            return row

    def set_modes(self, **modes):
        with db.session() as s:
            row = s.get(Server, self.sid)
            for section, mode in modes.items():
                setattr(row, f"{section}_mode", mode)

    def test_the_defaults_are_hardware_and_models_auto_and_power_manual(self):
        row = self.row()
        self.assertEqual(M.modes(row), {"hardware": "auto", "models": "auto", "power": "manual"})
        self.assertEqual(M.capabilities(row.capabilities),
                         {"llm": {"enabled": True, "allow_swaps": False},
                          "cpu": {"enabled": False, "threads": 0, "cpu_percent": 0, "memory_mb": 0, "work": [],
                                  "yield_to_llm": True}})

    def test_hardware_auto_applies_the_scan_and_power_manual_keeps_the_figures(self):
        out = self.report(hardware=scan(), models=[model("m1", loaded=True)])
        row = self.row()
        self.assertEqual(out, {"kept": ["hardware", "models"], "applied": ["hardware", "models"]})
        self.assertEqual(row.hardware_detected["hardware"], scan())
        self.assertTrue(row.hardware_detected["hardware_at"])
        self.assertIsNotNone(row.detected_at)
        hw = row.config["hardware"]
        self.assertEqual((hw["source"], hw["cpu"]["model"], hw["gpus"]), ("helper", "Test CPU", scan()["gpus"]))
        self.assertEqual(row.config["power"], {"idle_w": 11, "cpu_max_w": 22, "gpu_max_w": 33, "source": "measured"})
        self.assertEqual(servers.hardware_summary(hw).split(" · ")[0], "Test CPU (8C/16T)",
                         "a helper's scan reads as hardware details (it had no source tag before)")

    def test_hardware_manual_leaves_the_hand_set_hardware_and_keeps_the_scan(self):
        self.set_modes(hardware="manual")
        first = self.report(hardware=scan())
        at = self.row().hardware_detected["hardware_at"]
        time.sleep(0.01)
        second = self.report(hardware=scan(gpu="Bigger GPU", vram=24))
        row = self.row()
        self.assertEqual((first["applied"], second["applied"]), ([], []))
        self.assertEqual(row.config["hardware"], HAND_SET)
        self.assertEqual(row.hardware_detected["hardware"]["gpus"], [{"name": "Bigger GPU", "vram_gb": 24}])
        self.assertGreater(row.hardware_detected["hardware_at"], at, "the scan's time moves with each report")

    def test_power_auto_applies_the_suggested_figures_as_a_guess(self):
        self.set_modes(power="auto", hardware="manual")
        out = self.report(hardware=scan(idle=41, cpu_w=91, gpu_w=151))
        row = self.row()
        self.assertEqual(out["applied"], ["power"])
        self.assertEqual(row.config["power"], {"idle_w": 41, "cpu_max_w": 91, "gpu_max_w": 151, "source": "guess"})
        self.assertEqual(row.config["hardware"], HAND_SET)

    def test_a_report_without_a_scan_keeps_the_last_one(self):
        self.report(hardware=scan())
        before = self.row().hardware_detected["hardware"]
        self.report(hardware=None, models=[model("m1")])         # a helper started with --no-hardware
        self.report(hardware={"format": "not-ours"})              # not a scan: nothing kept, nothing applied
        row = self.row()
        self.assertEqual(row.hardware_detected["hardware"], before)
        self.assertEqual(row.config["hardware"]["cpu"]["model"], "Test CPU")

    def test_an_oversized_scan_is_not_kept(self):
        big = scan()
        big["runtimes"] = [{"name": "x" * 1000, "models": []} for _ in range(400)]
        with self.assertLogs("citar.machine_settings", "WARNING"):
            out = self.report(hardware=big)
        self.assertEqual(out, {"kept": [], "applied": []})
        self.assertIsNone(self.row().hardware_detected)

    def test_a_scan_is_kept_clean_of_figures_no_answer_can_carry(self):
        """A scan with NaN, Infinity, integers of 400 digits and parts of other shapes is kept (the rest of it is a
        good report) without them, applied so (hardware auto), and its suggested figures that do not read are no power
        figures (power auto takes the one that does)."""
        self.set_modes(power="auto")
        out = self.report(hardware=odd_scan())
        row = self.row()
        self.assertEqual(out, {"kept": ["hardware"], "applied": ["hardware", "power"]})
        self.assertEqual(row.hardware_detected["hardware"], ODD_CLEAN)
        self.assertEqual(row.config["hardware"], dict(ODD_CLEAN, source="helper"))
        self.assertEqual(row.config["power"], {"idle_w": 11, "cpu_max_w": 22, "gpu_max_w": 150, "source": "guess"})
        self.assertTrue(encodes(row.hardware_detected) and encodes(row.config))
        self.assertEqual(M.suggested_power(odd_scan()), {"gpu_max_w": 150}, "read from a scan as sent, too")
        with db.session() as s:
            read = M.client(s, s.get(Server, self.sid), None)
        self.assertTrue(encodes(read), "the dialog's read encodes")
        self.assertEqual(M.hardware_facts(odd_scan())["GPUs"], "Odd GPU; Huge GPU",
                         "the dialog's comparison reads a scan as sent without overflowing")

    def test_apply_of_everything_takes_the_sections_the_report_has(self):
        """Apply with no section named copies each section the last report has something for: a report of models
        alone (a helper started with --no-hardware), a scan that suggests no power figures. A section named with
        nothing to copy is still refused, and nothing changes."""
        self.set_modes(models="manual")
        self.report(models=[model("m", loaded=True)])
        with db.session() as s:
            self.assertEqual(M.apply_scan(s.get(Server, self.sid)), ["models"])
        self.assertTrue(catalogue(self.row())["m"]["loaded"])
        self.set_modes(hardware="manual")
        no_power = scan(gpu="Scanned GPU")
        del no_power["power"]["suggested"]
        self.report(hardware=no_power)
        with db.session() as s:
            self.assertEqual(M.apply_scan(s.get(Server, self.sid), []), ["hardware", "models"])
        row = self.row()
        self.assertEqual(row.config["hardware"]["gpus"], [{"name": "Scanned GPU", "vram_gb": 8}])
        self.assertEqual(row.config["power"]["source"], "measured", "no power figures to copy: left alone")
        with db.session() as s:
            with self.assertRaises(M.SettingsError) as caught:
                M.apply_scan(s.get(Server, self.sid), ["power"])
        self.assertEqual(caught.exception.status, 409)

    def test_models_auto_merge_keep_unreported_ones_and_their_owners_settings(self):
        self.report(models=[model("a", loaded=True, context=8192), model("b")])
        with db.session() as s:
            row = s.get(Server, self.sid)
            cfg = copy.deepcopy(row.config)
            for m in cfg["models"]:
                if m["key"] == "a":
                    m.update(label="Owner's name for a", inference={"tool_mode": "json"})
                if m["key"] == "b":
                    m["enabled"] = False
            row.config = cfg
        self.report(models=[model("a", context=16384), model("c", loaded=True, label="Model C")])
        got = catalogue(self.row())
        self.assertEqual(list(got), ["a", "b", "c"], "kept in place, the new one after them")
        self.assertEqual({k: (m.get("loaded"), m.get("reported")) for k, m in got.items()},
                         {"a": (False, True), "b": (False, False), "c": (True, True)})
        self.assertEqual(got["a"]["label"], "Owner's name for a", "a label that is only the key replaces nothing")
        self.assertEqual(got["a"]["inference"], {"tool_mode": "json"})
        self.assertEqual(got["a"]["context"], 16384)
        self.assertIs(got["b"]["enabled"], False)
        self.assertEqual(got["c"]["label"], "Model C")
        listed = {m["key"]: m for m in servers.get(self.sid)["models"]}
        self.assertEqual((listed["b"]["reported"], listed["c"]["loaded"], listed["a"]["context"]), (False, True, 16384),
                         "the registry's view carries what the helper said")

    def test_the_registry_keeps_what_a_helper_said_of_a_model_and_nothing_else(self):
        """``normalize_server`` keeps a model's ``loaded``, ``reported`` and ``context`` where a report put them, of
        the right type; a catalogue no helper reported to reads as it always did."""
        sv = servers.normalize_server({"name": "x", "models": [
            {"key": "good", "loaded": True, "reported": False, "context": 32768},
            {"key": "bad", "loaded": "yes", "reported": 1, "context": 0},
            {"key": "worse", "context": True}, {"key": "plain"}]})
        got = {m["key"]: {k: m[k] for k in ("loaded", "reported", "context") if k in m} for m in sv["models"]}
        self.assertEqual(got, {"good": {"loaded": True, "reported": False, "context": 32768}, "bad": {}, "worse": {},
                               "plain": {}})

    def test_models_manual_update_only_the_loaded_flags_of_those_listed(self):
        self.report(models=[model("a"), model("b", loaded=True)])
        self.set_modes(models="manual")
        out = self.report(models=[model("a", loaded=True, label="New name", context=4096), model("d", loaded=True)])
        got = catalogue(self.row())
        self.assertEqual(out["applied"], ["loaded"])
        self.assertEqual(list(got), ["a", "b"], "nothing added, nothing removed")
        self.assertEqual({k: m["loaded"] for k, m in got.items()}, {"a": True, "b": False})
        self.assertEqual((got["a"].get("label"), got["a"].get("context"), got["a"]["reported"]), ("", None, True))
        self.assertEqual([m["key"] for m in self.row().hardware_detected["models"]], ["a", "d"])

    def test_apply_copies_the_scan_whole_or_by_section(self):
        self.set_modes(hardware="manual", models="manual", power="manual")
        self.report(hardware=scan(idle=7, cpu_w=8, gpu_w=9), models=[model("x", loaded=True)])
        with db.session() as s:
            self.assertEqual(M.apply_scan(s.get(Server, self.sid), ["power"]), ["power"])
        row = self.row()
        self.assertEqual((row.config["power"]["idle_w"], row.config["power"]["source"]), (7, "guess"))
        self.assertEqual(row.config["hardware"], HAND_SET)
        self.assertNotIn("x", catalogue(row))
        with db.session() as s:
            self.assertEqual(M.apply_scan(s.get(Server, self.sid), []), ["hardware", "models", "power"])
        row = self.row()
        self.assertEqual(row.config["hardware"]["source"], "helper")
        self.assertEqual(catalogue(row)["x"]["loaded"], True)

    def test_apply_with_nothing_to_copy_is_refused_and_changes_nothing(self):
        with db.session() as s:
            row = s.get(Server, self.sid)
            for sections in (["hardware"], ["models"], ["power"], None):
                with self.assertRaises(M.SettingsError) as caught:
                    M.apply_scan(row, sections)
                self.assertEqual(caught.exception.status, 409)
            for bad in (["everything"], "hardware", [1]):
                with self.assertRaises(M.SettingsError) as caught:
                    M.apply_scan(row, bad)
                self.assertEqual(caught.exception.status, 400)
        self.assertEqual(self.row().config["hardware"], HAND_SET)

    def test_the_differences_name_what_the_scan_would_change(self):
        self.set_modes(hardware="manual", models="manual")
        self.report(hardware=scan(gpu="Scanned GPU", vram=24, cpu="Hand-set CPU", cores=4, ram=16),
                    models=[model("a", loaded=True), model("n")])
        with db.session() as s:
            row = s.get(Server, self.sid)
            cfg = dict(row.config, models=[{"key": "a"}, {"key": "gone"}])
            row.config = cfg
        row = self.row()
        diff = M.differences(row.config, row.hardware_detected)
        hw = {d["what"]: (d["configured"], d["detected"]) for d in diff["hardware"]}
        self.assertEqual(hw["GPUs"], ("Hand-set GPU (12 GB)", "Scanned GPU (24 GB)"))
        self.assertEqual(hw["Threads"], ("4", "8"))
        self.assertNotIn("CPU", hw, "what agrees is not listed")
        self.assertEqual({d["what"] for d in diff["power"]}, {label for _k, label in M.POWER_FIELDS})
        models = {d["what"]: (d["configured"], d["detected"]) for d in diff["models"]}
        self.assertEqual(models, {"a": ("not loaded", "reported, loaded"), "n": ("not in the catalogue", "reported"),
                                  "gone": ("in the catalogue", "not reported")})
        with db.session() as s:
            M.apply_scan(s.get(Server, self.sid))
        row = self.row()
        self.assertEqual(M.differences(row.config, row.hardware_detected)["hardware"], [])
        self.assertEqual(M.differences(row.config, row.hardware_detected)["power"], [])
        self.assertEqual(M.differences(row.config, row.hardware_detected)["models"], [],
                         "a model kept as not reported is no difference once applied")

    def test_the_settings_are_checked_before_anything_changes(self):
        bad = [[], {"slots": 0}, {"slots": 17}, {"slots": "2"}, {"slots": 1.5}, {"slots": True}, {"other": 1},
               {"capabilities": {"llm": {"enabled": "yes"}}}, {"capabilities": {"gpu": {}}},
               {"capabilities": {"llm": {"allow_swaps": 1}}}, {"capabilities": {"cpu": {"threads": -1}}},
               {"capabilities": {"cpu": {"cpu_percent": 101}}}, {"capabilities": {"cpu": {"work": ["mining"]}}},
               {"capabilities": {"cpu": {"work": "lab_game"}}}, {"modes": {"hardware": "sometimes"}},
               {"modes": {"cpu": "auto"}}, {"slots": 2, "modes": {"power": "on"}},
               # what the JSON reader takes and no float holds, and the floats that are no number
               {"slots": 10 ** 400}, {"capabilities": {"cpu": {"threads": 10 ** 400}}},
               {"capabilities": {"cpu": {"memory_mb": 10 ** 400}}}, {"slots": float("nan")}, {"slots": float("inf")},
               {"capabilities": {"cpu": {"cpu_percent": float("-inf")}}}]
        with db.session() as s:
            row = s.get(Server, self.sid)
            for body in bad:
                with self.subTest(body=str(body)[:80]), self.assertRaises(M.SettingsError):
                    M.update(row, body)
            self.assertEqual((row.max_concurrent, row.capabilities, M.modes(row)),
                             (1, {}, M.DEFAULT_MODES), "a refused change leaves everything as it was")
            row.kind = "api"
            with self.assertRaises(M.SettingsError):
                M.update(row, {"capabilities": {"cpu": {"enabled": True}}})
            row.kind = "owned"
            changed = M.update(row, {"slots": 3, "capabilities": {"cpu": {"enabled": True, "threads": 6,
                                                                          "work": ["reference_game", "lab_game"]}},
                                     "modes": {"power": "auto"}})
            self.assertEqual(set(changed), {"slots", "cpu.enabled", "cpu.threads", "cpu.work", "power_mode"})
            self.assertEqual(row.capabilities["cpu"]["work"], ["lab_game", "reference_game"])
            self.assertEqual(M.update(row, {"slots": 3}), {}, "the same value is no change")


# ---------------------------------------------------------------------------- a real helper reconnecting

def _soon(check, limit: float = 10.0):
    """``check()``'s value once it is true, within ``limit`` seconds, else its last value."""
    t0 = time.perf_counter()
    while time.perf_counter() - t0 < limit:
        value = check()
        if value:
            return value
        time.sleep(0.02)
    return check()


class _Server:
    """A real uvicorn server for the app on a free port, with signed-in accounts (tests/test_worker.py's
    ``Revocation`` harness). The app's start makes process-wide workers that would outlive the server: the host's
    power sampler (which runs ``nvidia-smi`` every few seconds where there is one, and later modules watch for outbound
    calls) is not started, the games other modules left marked open are not brought back, and the benchmark
    scheduler, the probe and report runners it made are stopped and put back as they were (test_scheduler_e2e's
    ``HelperBase``)."""

    @classmethod
    def start(cls, owner, handles: dict) -> None:
        import httpx
        import uvicorn
        from citar import usage
        from citar.server import app as appmod, reports_api
        from citar.server.session import SessionManager
        app = appmod.app
        _reset_db()
        scheduler.reset()
        owner.saved = (appmod.scheduler, appmod._probe_runner, reports_api._runner)
        owner.headers, owner.ids = {}, {}
        with db.session() as s:
            for handle, role in handles.items():
                user = accounts.create_user(s, handle=handle, email=f"{handle}@x.cc", password="a long enough phrase",
                                            role=role, status="active", email_verified=True)
                row, raw = sessions.start(s, user)
                owner.headers[handle] = {"cookie": f"{sessions.COOKIE_NAME}={raw}",
                                         sessions.CSRF_HEADER: row.csrf_token, "origin": "https://citar.test"}
                owner.ids[handle] = user.id
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            owner.port = probe.getsockname()[1]
        owner.server = uvicorn.Server(uvicorn.Config(app, host="127.0.0.1", port=owner.port, log_level="error"))
        owner.thread = threading.Thread(target=owner.server.run, daemon=True)
        with (mock.patch.object(SessionManager, "restore_live", lambda self: []),
              mock.patch.object(usage, "start_power_sampler", lambda follow_host=False: None)):
            owner.thread.start()
            for _ in range(150):
                if owner.server.started:
                    break
                time.sleep(0.1)
        owner.http = httpx.Client(base_url=f"http://127.0.0.1:{owner.port}", timeout=30)

    @classmethod
    def stop(cls, owner) -> None:
        from citar.server import app as appmod, reports_api
        owner.http.close()
        owner.server.should_exit = True
        owner.thread.join(timeout=10)
        if appmod.scheduler is not None and appmod.scheduler is not owner.saved[0]:
            appmod.scheduler.stop()
        appmod.scheduler, appmod._probe_runner, reports_api._runner = owner.saved
        scheduler.reset()


class EndToEnd(unittest.TestCase):
    """Gate 1, on a real server and a real helper over a real websocket: for each section and mode, the helper's hello
    with other hardware, models and power figures applies them in ``auto`` and leaves its owner's values in ``manual``,
    always keeping the scan and its time; models in ``auto`` merge and keep the unreported; "apply" copies the scan,
    whole or by section, through the dialog's route."""

    @classmethod
    def setUpClass(cls):
        _Server.start(cls, {"lena": "user", "ada": "admin"})
        cls.files = Path(tempfile.mkdtemp(prefix="citar_hw_", dir=_TMP))

    @classmethod
    def tearDownClass(cls):
        _Server.stop(cls)

    def setUp(self):
        with db.session() as s:
            server = Server(owner_id=self.ids["lena"], name=f"Lena's box {self.id()[-12:]}", kind="owned",
                            provider="lmstudio", reach="worker", config={})
            s.add(server)
            s.flush()
            self.sid = server.id
            self.token, hashed = tokens.new_pair()
            s.add(WorkerToken(server_id=server.id, token_hash=hashed, prefix=tokens.prefix(self.token), label="t"))
        self.hw_file = self.files / f"{self.sid}.json"
        self.worker = None

    def tearDown(self):
        self.disconnect()

    def call(self, who, method, path, body=None):
        return self.http.request(method, path, json=body, headers=self.headers[who])

    def row(self) -> Server:
        with db.session() as s:
            row = s.get(Server, self.sid)
            s.expunge(row)
            return row

    def connect(self, hardware: dict, models: list) -> Server:
        """(Re)connect the machine's helper (the fake helper of ``tests/helperfake``, over a real socket), its hello
        reporting ``hardware`` (as its ``--hardware`` file would) and ``models``; returns the row once the server has
        recorded the report."""
        from tests import helperfake as H
        self.disconnect()
        before = self.row().detected_at
        fields = H.hello(token=self.token, slots=2, hostname="settings box", version="1.0",
                         models=[H.model(m["key"], m.get("loaded", False), m.get("context"), m.get("label") or "")
                                 for m in models])
        fields["report"] = {"hardware": hardware, "hardware_source": "file"}
        helper = H.FakeHelper(f"ws://127.0.0.1:{self.port}/ws/worker")
        helper.handshake(fields)
        H.Responder(helper, slots=2)            # it answers what the server sends it, settings pushed included
        self.worker = helper
        row = _soon(lambda: (lambda r: r if r.detected_at is not None and r.detected_at != before else None)(self.row()))
        self.assertIsNotNone(row, "the helper's report was not recorded")
        return row

    def disconnect(self):
        if self.worker is None:
            return
        helper, self.worker = self.worker, None
        helper.close()
        _soon(lambda: not W.hub().is_online(self.sid), 5)

    def settings(self, **body) -> dict:
        r = self.call("lena", "PUT", f"/api/machines/{self.sid}/settings", body)
        self.assertEqual(r.status_code, 200, r.text)
        return r.json()

    def save_registry_view(self, change) -> None:
        """Save the machine as its owner's dialogs do: its registry view (``GET /api/servers``), changed, posted back
        whole (``POST /api/servers``)."""
        listing = self.call("lena", "GET", "/api/servers").json()["servers"]
        view = next(sv for sv in listing if sv["id"] == self.sid)
        change(view)
        r = self.call("lena", "POST", "/api/servers", view)
        self.assertEqual(r.status_code, 200, r.text)

    def test_hardware_auto_applies_each_scan_and_manual_keeps_a_hand_set_entry(self):
        first = self.connect(scan(gpu="First GPU", vram=8), [model("m")])
        self.assertEqual(first.config["hardware"]["gpus"], [{"name": "First GPU", "vram_gb": 8}])
        self.assertEqual(first.config["hardware"]["source"], "helper")
        self.assertEqual(first.hardware_detected["helper"], {"hostname": "settings box", "version": "1.0"})
        # the owner types her own hardware in (the Hardware & power dialog) and keeps it: hardware manual
        self.save_registry_view(lambda v: v.update(hardware=copy.deepcopy(HAND_SET)))
        self.assertEqual(self.settings(modes={"hardware": "manual"})["modes"]["hardware"], "manual")
        second = self.connect(scan(gpu="Second GPU", vram=24), [model("m")])
        self.assertEqual(second.config["hardware"]["gpus"], HAND_SET["gpus"], "a manual entry survives a reconnect")
        self.assertEqual(second.hardware_detected["hardware"]["gpus"], [{"name": "Second GPU", "vram_gb": 24}])
        self.assertGreater(second.detected_at, first.detected_at)
        self.assertGreater(second.hardware_detected["hardware_at"], first.hardware_detected["hardware_at"])
        read = self.call("lena", "GET", f"/api/machines/{self.sid}/settings").json()
        self.assertIn({"what": "GPUs", "configured": "Hand-set GPU (12 GB)", "detected": "Second GPU (24 GB)"},
                      read["differences"]["hardware"])
        self.assertEqual(read["report"]["hardware_summary"].split(" · ")[1], "Second GPU 24 GB")
        # back to auto: the next scan is applied
        self.settings(modes={"hardware": "auto"})
        third = self.connect(scan(gpu="Third GPU", vram=16), [model("m")])
        self.assertEqual(third.config["hardware"]["gpus"], [{"name": "Third GPU", "vram_gb": 16}])

    def test_power_manual_keeps_the_owners_figures_and_auto_takes_the_scans(self):
        self.save_registry_view(lambda v: v["power"].update(idle_w=12, cpu_max_w=34, gpu_max_w=56, source="measured"))
        row = self.connect(scan(idle=40, cpu_w=90, gpu_w=150), [model("m")])
        self.assertEqual(M.modes(row)["power"], "manual")
        self.assertEqual({k: row.config["power"][k] for k in ("idle_w", "cpu_max_w", "gpu_max_w", "source")},
                         {"idle_w": 12, "cpu_max_w": 34, "gpu_max_w": 56, "source": "measured"})
        self.assertEqual(M.suggested_power(row.hardware_detected["hardware"]),
                         {"idle_w": 40, "cpu_max_w": 90, "gpu_max_w": 150})
        self.settings(modes={"power": "auto"})
        row = self.connect(scan(idle=41, cpu_w=91, gpu_w=151), [model("m")])
        self.assertEqual({k: row.config["power"][k] for k in ("idle_w", "cpu_max_w", "gpu_max_w", "source")},
                         {"idle_w": 41, "cpu_max_w": 91, "gpu_max_w": 151, "source": "guess"})

    def test_models_auto_merge_and_manual_only_update_what_is_loaded(self):
        row = self.connect(scan(), [model("a", loaded=True, context=8192), model("b")])
        self.assertEqual({k: (m["loaded"], m["reported"]) for k, m in catalogue(row).items()},
                         {"a": (True, True), "b": (False, True)})
        # her display name for a, and b no longer offered: the Connection & models dialog
        self.save_registry_view(lambda v: [m.update(label="Her A") if m["key"] == "a" else m.update(enabled=False)
                                           for m in v["models"]])
        row = self.connect(scan(), [model("a"), model("c", loaded=True)])
        got = catalogue(row)
        self.assertEqual(list(got), ["a", "b", "c"], "the unreported one is kept, the new one added")
        self.assertEqual((got["a"]["label"], got["a"]["loaded"], got["a"].get("context")), ("Her A", False, 8192))
        self.assertEqual((got["b"]["reported"], got["b"]["enabled"]), (False, False))
        self.assertEqual((got["c"]["reported"], got["c"]["loaded"]), (True, True))
        self.settings(modes={"models": "manual"})
        row = self.connect(scan(), [model("b", loaded=True), model("d", loaded=True)])
        got = catalogue(row)
        self.assertEqual(list(got), ["a", "b", "c"], "manual: nothing added or removed")
        self.assertEqual({k: m["loaded"] for k, m in got.items()}, {"a": False, "b": True, "c": False})
        self.assertEqual([m["key"] for m in row.hardware_detected["models"]], ["b", "d"], "the report is kept")
        # apply the models section: d comes in, and a and c are marked as no longer reported
        r = self.call("lena", "POST", f"/api/machines/{self.sid}/hardware/apply", {"sections": ["models"]})
        self.assertEqual(r.status_code, 200, r.text)
        got = catalogue(self.row())
        self.assertEqual(list(got), ["a", "b", "c", "d"])
        self.assertEqual({k: m["reported"] for k, m in got.items()}, {"a": False, "b": True, "c": False, "d": True})
        self.assertEqual(r.json()["differences"]["models"], [])

    def test_apply_copies_the_last_scan_whole_or_by_section(self):
        self.settings(modes={"hardware": "manual", "models": "manual", "power": "manual"})
        self.save_registry_view(lambda v: v.update(hardware=copy.deepcopy(HAND_SET)))
        row = self.connect(scan(gpu="Scanned", vram=20, idle=5, cpu_w=6, gpu_w=7), [model("s", loaded=True)])
        self.assertEqual(row.config["hardware"]["gpus"], HAND_SET["gpus"])
        self.assertNotIn("s", catalogue(row))
        self.disconnect()                                   # apply needs no helper: it copies the kept scan
        r = self.call("lena", "POST", f"/api/machines/{self.sid}/hardware/apply", {"sections": ["hardware"]})
        self.assertEqual(r.status_code, 200, r.text)
        row = self.row()
        self.assertEqual((row.config["hardware"]["gpus"], row.config["hardware"]["source"]),
                         ([{"name": "Scanned", "vram_gb": 20}], "helper"))
        self.assertIsNone(row.config.get("power", {}).get("idle_w"), "one section only")
        self.assertNotIn("s", catalogue(row))
        r = self.call("lena", "POST", f"/api/machines/{self.sid}/hardware/apply")
        self.assertEqual(r.status_code, 200, r.text)
        row = self.row()
        self.assertEqual(row.config["power"]["idle_w"], 5)
        self.assertTrue(catalogue(row)["s"]["loaded"])
        self.assertEqual(r.json()["differences"], {"hardware": [], "models": [], "power": []})
        with db.session() as s:
            audits = [a.detail.get("sections") for a in s.scalars(select(AuditLog).where(
                AuditLog.action == "server.scan_applied", AuditLog.object_id == self.sid))]
        self.assertEqual(sorted(audits, key=len), [["hardware"], ["hardware", "models", "power"]])


# ---------------------------------------------------------------------------- the routes

class _Routes(unittest.TestCase):
    """The app through FastAPI's test client: alice's helper machine, lent through her group to gus; bob, another
    member; mona, a moderator; zara, an administrator."""

    def setUp(self):
        from fastapi.testclient import TestClient
        from citar.auth.deps import get_db
        from citar.server import app as appmod
        _reset_db()
        scheduler.reset()
        self.addCleanup(scheduler.reset)
        self.client = TestClient(appmod.app, base_url="https://citar.test")
        self.addCleanup(self.client.close)
        self.headers, self.ids = {}, {}
        with db.session() as s:
            for handle, role in (("alice", "user"), ("bob", "user"), ("gus", "user"), ("mona", "moderator"),
                                 ("zara", "admin")):
                user = accounts.create_user(s, handle=handle, email=f"{handle}@example.com",
                                            password="a long enough phrase", role=role, status="active",
                                            email_verified=True)
                row, raw = sessions.start(s, user)
                self.headers[handle] = {"cookie": f"{sessions.COOKIE_NAME}={raw}",
                                        sessions.CSRF_HEADER: row.csrf_token, "origin": "https://citar.test"}
                self.ids[handle] = user.id
            alice = s.get(User, self.ids["alice"])
            helper = pool.create_server(s, alice, {"name": "Alice GPU", "kind": "owned",
                                                   "connection": {"provider": "lmstudio"}})
            group = pool.create_group(s, alice, name="Alice's")
            helper.group_id = group.id
            pool.create_grant(s, alice, group, subject_type="user", subject_handle="gus", purposes=[])
            self.helper = helper.id
        # what the request's session still held when the route returned (it commits only after the answer is sent)
        self.pending: list = []

        def recording_db():
            with db.session() as s:
                flushed = {"n": 0}
                event.listen(s, "after_flush", lambda *_: flushed.__setitem__("n", flushed["n"] + 1))
                event.listen(s, "after_commit", lambda *_: flushed.__setitem__("n", 0))
                yield s
                self.pending.append(flushed["n"] > 0 or bool(s.new or s.dirty or s.deleted))
        appmod.app.dependency_overrides[get_db] = recording_db
        self.addCleanup(appmod.app.dependency_overrides.pop, get_db, None)

    def call(self, who, method, path, body=None):
        self.pending.clear()
        headers = self.headers.get(who, {"origin": "https://citar.test"})
        if body is not None and not encodes(body):
            # what a browser cannot send and a script can: Python's JSON writer emits NaN and Infinity (the test
            # client's refuses to), and the server's reader takes them
            return self.client.request(method, path, content=json.dumps(body).encode(),
                                       headers={**headers, "content-type": "application/json"})
        return self.client.request(method, path, json=body, headers=headers)

    def row(self, sid=None) -> Server:
        with db.session() as s:
            row = s.get(Server, sid or self.helper)
            s.expunge(row)
            return row

    def report(self, hardware=None, models=None) -> dict:
        """The helper's report, as its socket's handler records it."""
        return W.record_report(self.helper, hardware=hardware, models=models)


class Routes(_Routes):

    def test_the_settings_routes_are_the_owners_and_administrators(self):
        """P3.9.1: alice (the owner) and zara (an administrator) 200; bob (another member) 404; gus (whom her group
        lends it to) 403, since he may see the machine; a visitor 401. A machine that is not there is 404 to all."""
        self.report(hardware=scan(), models=[model("m")])
        routes = [("GET", f"/api/machines/{self.helper}/settings", None),
                  ("PUT", f"/api/machines/{self.helper}/settings", {"slots": 2}),
                  ("POST", f"/api/machines/{self.helper}/hardware/apply", {"sections": ["hardware"]})]
        for method, path, body in routes:
            with self.subTest(route=f"{method} {path}"):
                self.assertEqual(self.call("bob", method, path, body).status_code, 404)
                self.assertEqual(self.call("gus", method, path, body).status_code, 403)
                self.assertEqual(self.call("anon", method, path, body).status_code, 401)
                for who in ("alice", "zara"):
                    r = self.call(who, method, path, body)
                    self.assertEqual(r.status_code, 200, r.text)
                    self.assertEqual(self.pending, [False], "the change was still uncommitted when the route answered")
                self.assertEqual(self.call("zara", method, path.replace(self.helper, "sv_nothere"), body).status_code,
                                 404)

    def test_the_trust_and_class_routes_are_administrators(self):
        """Anyone but an administrator gets 404 on them (the owner included), a visitor 401; an administrator sets the
        trust tier and pins the class, each with an audit row, and the owner is shown both."""
        routes = [("PUT", f"/api/admin/machines/{self.helper}/trust", {"trust_tier": "trusted"}),
                  ("PUT", f"/api/admin/machines/{self.helper}/class", {"pinned": True, "gpu_class_id": "hc_gpu"})]
        for method, path, body in routes:
            with self.subTest(route=f"{method} {path}"):
                for who in ("alice", "bob", "gus"):
                    self.assertEqual(self.call(who, method, path, body).status_code, 404)
                self.assertEqual(self.call("anon", method, path, body).status_code, 401)
                r = self.call("zara", method, path, body)
                self.assertEqual(r.status_code, 200, r.text)
                self.assertEqual(self.pending, [False])
                self.assertEqual(self.call("zara", method, path.replace(self.helper, "sv_nothere"), body).status_code,
                                 404)
        mine = self.call("alice", "GET", f"/api/machines/{self.helper}/settings").json()
        self.assertEqual(mine["trust_tier"], "trusted")
        self.assertEqual((mine["class"]["gpu"]["id"], mine["class"]["pinned"]), ("hc_gpu", True))
        self.assertFalse(mine["can_admin"])
        self.assertNotIn("classes", mine)
        self.assertNotIn("flags", mine)
        with db.session() as s:
            actions = [a.action for a in s.scalars(select(AuditLog).where(AuditLog.object_id == self.helper))]
        self.assertEqual(actions.count("server.trust"), 1)
        self.assertEqual(actions.count("server.class"), 1)

    def test_a_moderator_is_a_member_like_any_other_here(self):
        """P3.9.1's moderator (a machine is not something a moderator takes down: on a server object they are a member
        like any other). In alice's private group mona gets 404 on all five routes, as bob does; once the group is
        visible to members, or published, she may see the machine but not manage it: the
        dialog's routes 403, the administrators' still 404 (its read, ``GET /api/pool/servers/{id}``, 404 and then 200).
        Nothing she sends changes it."""
        self.report(hardware=scan(), models=[model("m")])
        routes = [("GET", f"/api/machines/{self.helper}/settings", None),
                  ("PUT", f"/api/machines/{self.helper}/settings", {"slots": 2}),
                  ("POST", f"/api/machines/{self.helper}/hardware/apply", {"sections": ["hardware"]}),
                  ("PUT", f"/api/admin/machines/{self.helper}/trust", {"trust_tier": "trusted"}),
                  ("PUT", f"/api/admin/machines/{self.helper}/class", {"pinned": True})]
        before = self.row()
        self.assertEqual([self.call("mona", m, p, b).status_code for m, p, b in routes], [404] * 5)
        self.assertEqual(self.call("mona", "GET", f"/api/pool/servers/{self.helper}").status_code, 404)
        for visibility in ("server", "global"):
            with db.session() as s:
                s.get(ServerGroup, s.get(Server, self.helper).group_id).visibility = visibility
            with self.subTest(visibility=visibility):
                self.assertEqual([self.call("mona", m, p, b).status_code for m, p, b in routes],
                                 [403, 403, 403, 404, 404])
                self.assertEqual(self.call("mona", "GET", f"/api/pool/servers/{self.helper}").status_code, 200,
                                 "she may see it")
        after = self.row()
        for column in ("max_concurrent", "config", "trust_tier", "class_override", "gpu_class_id", "hardware_mode"):
            self.assertEqual(getattr(after, column), getattr(before, column), column)

    def test_bad_bodies_are_refused_and_change_nothing(self):
        cases = [("PUT", f"/api/machines/{self.helper}/settings", {"slots": 0}, 400),
                 ("PUT", f"/api/machines/{self.helper}/settings", {"modes": {"hardware": "always"}}, 400),
                 ("PUT", f"/api/machines/{self.helper}/settings", {"capabilities": {"cpu": {"work": ["mine"]}}}, 400),
                 ("PUT", f"/api/machines/{self.helper}/settings", {"nonsense": True}, 400),
                 ("PUT", f"/api/machines/{self.helper}/settings", {"slots": 10 ** 400}, 400),
                 ("PUT", f"/api/machines/{self.helper}/settings", {"capabilities": {"cpu": {"threads": 10 ** 400}}}, 400),
                 ("PUT", f"/api/machines/{self.helper}/settings", [1, 2], 422),
                 ("POST", f"/api/machines/{self.helper}/hardware/apply", {"sections": ["all"]}, 400),
                 ("POST", f"/api/machines/{self.helper}/hardware/apply", {"what": 1}, 400),
                 ("POST", f"/api/machines/{self.helper}/hardware/apply", {"sections": ["hardware"]}, 409),
                 ("PUT", f"/api/admin/machines/{self.helper}/trust", {"trust_tier": "royal"}, 400),
                 ("PUT", f"/api/admin/machines/{self.helper}/trust", {"trust_tier": "new", "x": 1}, 400),
                 ("PUT", f"/api/admin/machines/{self.helper}/class", {"pinned": "yes"}, 400),
                 ("PUT", f"/api/admin/machines/{self.helper}/class", {"pinned": True, "gpu_class_id": "hc_cpu"}, 400),
                 ("PUT", f"/api/admin/machines/{self.helper}/class", {"pinned": True, "cpu_class_id": "hc_none"}, 400),
                 ("PUT", f"/api/admin/machines/{self.helper}/class", {"pinned": False, "gpu_class_id": "hc_gpu"}, 400)]
        before = self.row()
        for method, path, body, code in cases:
            with self.subTest(route=f"{method} {path}", body=str(body)[:80]):
                self.assertEqual(self.call("zara", method, path, body).status_code, code)
        after = self.row()
        for column in ("max_concurrent", "capabilities", "hardware_mode", "models_mode", "power_mode", "config",
                       "trust_tier", "class_override", "gpu_class_id", "cpu_class_id"):
            self.assertEqual(getattr(after, column), getattr(before, column), column)

    def test_a_change_is_read_back_audited_and_reaches_machine_settings(self):
        r = self.call("alice", "PUT", f"/api/machines/{self.helper}/settings", {
            "slots": 3, "capabilities": {"llm": {"allow_swaps": True},
                                         "cpu": {"enabled": True, "threads": 8, "cpu_percent": 50, "memory_mb": 4096,
                                                 "work": ["lab_game"]}},
            "modes": {"hardware": "manual", "power": "auto"}})
        self.assertEqual(r.status_code, 200, r.text)
        got = r.json()
        self.assertEqual((got["slots"], got["modes"]), (3, {"hardware": "manual", "models": "auto", "power": "auto"}))
        self.assertEqual(got["capabilities"]["cpu"], {"enabled": True, "threads": 8, "cpu_percent": 50,
                                                      "memory_mb": 4096, "work": ["lab_game"],
                                                      "yield_to_llm": True})
        ms = servers.machine_settings(self.helper)
        self.assertEqual((ms["slots"], ms["allow_swaps"], ms["llm"]), (3, True, True))
        self.assertEqual(ms["cpu"], got["capabilities"]["cpu"], "what Phase 4's settings frame will carry")
        self.assertEqual(self.call("alice", "GET", f"/api/machines/{self.helper}/settings").json()["capabilities"],
                         got["capabilities"])
        with db.session() as s:
            audit = [a.detail for a in s.scalars(select(AuditLog).where(AuditLog.action == "server.settings"))]
        self.assertEqual(len(audit), 1)
        self.assertEqual(set(audit[0]["changed"]), {"slots", "llm.allow_swaps", "cpu.enabled", "cpu.threads",
                                                    "cpu.cpu_percent", "cpu.memory_mb", "cpu.work", "hardware_mode",
                                                    "power_mode"})
        listed = self.call("alice", "GET", "/api/servers").json()["servers"]
        self.assertEqual(next(sv for sv in listed if sv["id"] == self.helper)["modes"],
                         {"hardware": "manual", "models": "auto", "power": "auto"},
                         "the editors' view says what a helper's report may change")

    def test_classification_follows_the_hardware_unless_pinned(self):
        r = self.call("zara", "POST", "/api/admin/hardware-classes", {
            "kind": "gpu", "name": "24 GB GPUs", "rate": 2.0, "match": {"vram_gb_min": 24}, "priority": 10})
        self.assertEqual(r.status_code, 200, r.text)
        big = r.json()["id"]
        r = self.call("zara", "POST", "/api/admin/hardware-classes", {
            "kind": "cpu", "name": "Test CPUs", "rate": 0.1, "match": {"cpu_name": "^Test CPU$"}, "priority": 10})
        self.assertEqual(r.status_code, 200, r.text)
        test_cpus = r.json()["id"]

        def gpu_class() -> tuple:
            row = self.row()
            classes = [ledger.class_dict(c) for c in db_classes()]
            priced = ledger.class_of(servers.machine_facts(self.helper), "gpu", classes)
            return row.gpu_class_id, priced["id"], row.class_override

        def db_classes():
            with db.session() as s:
                rows = list(s.scalars(select(HardwareClass)))
                for c in rows:
                    s.expunge(c)
                return rows
        self.report(hardware=scan(vram=8))
        self.assertEqual(gpu_class(), ("hc_gpu", "hc_gpu", False))
        self.report(hardware=scan(vram=24))
        self.assertEqual(gpu_class(), (big, big, False), "a new scan (hardware auto) classifies it again")
        # hardware manual: a scan changes nothing, so neither does the class
        self.call("alice", "PUT", f"/api/machines/{self.helper}/settings", {"modes": {"hardware": "manual"}})
        self.report(hardware=scan(vram=8))
        self.assertEqual(gpu_class(), (big, big, False))
        self.call("alice", "PUT", f"/api/machines/{self.helper}/settings", {"modes": {"hardware": "auto"}})
        self.report(hardware=scan(vram=8))
        self.assertEqual(gpu_class(), ("hc_gpu", "hc_gpu", False))
        # pinned: the hardware no longer moves it
        r = self.call("zara", "PUT", f"/api/admin/machines/{self.helper}/class", {"pinned": True, "gpu_class_id": big})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual((r.json()["class"]["gpu"]["id"], r.json()["class"]["pinned"]), (big, True))
        self.assertEqual(r.json()["class"]["cpu"]["id"], test_cpus, "the class it leaves out is the one it had")
        self.report(hardware=scan(vram=4, cpu="Another CPU"))
        self.assertEqual(gpu_class(), (big, big, True))
        self.assertEqual(self.call("alice", "GET", f"/api/machines/{self.helper}/settings").json()["class"]["cpu"]["id"],
                         test_cpus, "pinned, its CPU class holds against a scan of another CPU too")
        # unpinned: it follows the hardware again at once
        r = self.call("zara", "PUT", f"/api/admin/machines/{self.helper}/class", {"pinned": False})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(gpu_class(), ("hc_gpu", "hc_gpu", False))
        self.assertFalse(r.json()["class"]["pinned"])

    def test_an_online_api_is_pinned_to_an_api_class(self):
        r = self.call("zara", "PUT", "/api/admin/machines/sv_api/class", {"pinned": True, "gpu_class_id": "hc_api"})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual((r.json()["class"]["gpu"]["id"], r.json()["class"]["cpu"]), ("hc_api", None))
        for body in ({"pinned": True, "gpu_class_id": "hc_gpu"}, {"pinned": True, "cpu_class_id": "hc_cpu"}):
            self.assertEqual(self.call("zara", "PUT", "/api/admin/machines/sv_api/class", body).status_code, 400)

    def test_the_over_report_flag_is_shown_to_administrators_only(self):
        with db.session() as s:
            s.get(Server, self.helper).flags = {"overreports": {"count": 3, "last": "2026-10-13T20:00:00+00:00"}}
        admin = self.call("zara", "GET", f"/api/machines/{self.helper}/settings").json()
        self.assertEqual(admin["flags"]["overreports"]["count"], 3)
        self.assertTrue(admin["can_admin"])
        self.assertEqual({c["id"] for c in admin["classes"]} >= {"hc_gpu", "hc_cpu", "hc_api"}, True)
        self.assertNotIn("flags", self.call("alice", "GET", f"/api/machines/{self.helper}/settings").json())
        listings = {who: {sv["id"]: sv for sv in self.call(who, "GET", "/api/pool/servers").json()["servers"]}
                    for who in ("alice", "zara", "gus")}
        self.assertEqual(listings["zara"][self.helper]["flags"]["overreports"]["count"], 3)
        self.assertNotIn("flags", listings["alice"][self.helper])
        self.assertNotIn("flags", listings["gus"][self.helper])

    def test_a_machine_whose_llm_is_off_takes_no_model_work(self):
        self.call("alice", "PUT", f"/api/machines/{self.helper}/settings", {"capabilities": {"llm": {"enabled": False}}})
        with db.session() as s:
            row = s.get(Server, self.helper)
            for who in ("alice", "gus"):
                decision = admission.check(s, s.get(User, self.ids[who]), row, "game", online=True)
                self.assertFalse(decision.allowed, who)
                self.assertIn("takes no model work", decision.reason)
            self.assertTrue(admission.check(s, s.get(User, self.ids["alice"]), row, "lab").allowed,
                            "the lab's work is not model work")
        ticket = scheduler.get().ticket(self.helper, "m", cls="test", consumer=self.ids["alice"])
        self.assertEqual((ticket.state, ticket.error.code), ("refused", "disabled"))
        self.assertIn("takes no model work", str(ticket.error))

    def pages(self) -> dict:
        """Every page that shows the machine, and the status each answers (or the exception it raised): alice's and
        zara's Servers page (``GET /api/servers``: an administrator's lists every machine), the pool's listing for
        gus (whom it is lent to) and zara, the dialog's three routes and the trust route."""
        settings, apply = f"/api/machines/{self.helper}/settings", f"/api/machines/{self.helper}/hardware/apply"
        out = {}
        for who, method, path, body in (
                ("alice", "GET", "/api/servers", None), ("zara", "GET", "/api/servers", None),
                ("gus", "GET", "/api/pool/servers", None), ("zara", "GET", "/api/pool/servers", None),
                ("alice", "GET", settings, None), ("alice", "PUT", settings, {"slots": 2}),
                ("alice", "POST", apply, {"sections": ["hardware"]}),
                ("zara", "PUT", f"/api/admin/machines/{self.helper}/trust", {"trust_tier": "trusted"})):
            try:
                r = self.call(who, method, path, body)
                out[f"{who} {method} {path}"] = r.status_code if r.status_code != 200 or encodes(r.json()) else "NaN"
            except Exception as exc:        # the test client raises what the app raised: a 500
                out[f"{who} {method} {path}"] = f"{type(exc).__name__}: {exc}"[:120]
        return out

    def test_odd_hardware_never_fails_a_page_that_shows_the_machine(self):
        """A helper's scan with figures no JSON answer carries or no float holds, or parts of other shapes (reachable by
        any member who runs a helper for their own machine): kept clean (``servers.clean_hardware``), and every page
        that shows the machine still answers, the administrators' whole Servers page among them. So do they for a row
        written before the cleaning (the old hello's raw copy), whose odd hardware no longer stops the classification
        of every machine when an administrator adds a class; and the owner's own save of such details is kept clean."""
        def failing() -> dict:
            return {page: status for page, status in self.pages().items() if status != 200}
        for name, odd in odd_scans().items():
            with self.subTest(name):
                with db.session() as s:
                    row = s.get(Server, self.helper)
                    row.config, row.hardware_detected = {}, None
                out = W.record_report(self.helper, hardware=odd, models=[model("m")])
                self.assertEqual(out["kept"], ["hardware", "models"])
                row = self.row()
                self.assertTrue(encodes(row.hardware_detected) and encodes(row.config))
                self.assertEqual(failing(), {})
        # a row as the old hello's raw copy left it: an administrator's new class classifies every machine again
        raw = odd_scan()
        raw["gpus"].append({"name": "Text GPU", "vram_gb": "24 GB"})
        with db.session() as s:
            s.get(Server, self.helper).config = {"hardware": dict(raw, source="helper"), "models": ["m", {"x": 1}]}
        r = self.call("zara", "POST", "/api/admin/hardware-classes", {
            "kind": "gpu", "name": "24 GB GPUs", "rate": 2.0, "match": {"vram_gb_min": 24}, "priority": 10})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(self.row().gpu_class_id, "hc_gpu",
                         "memory figures that do not read (NaN, 400 digits, '24 GB') are none: the default class")
        self.assertEqual(failing(), {})
        # her own save of such details (the Hardware & power dialog posts the registry's view back whole)
        view = next(sv for sv in self.call("alice", "GET", "/api/servers").json()["servers"] if sv["id"] == self.helper)
        view["hardware"] = odd_scan()
        r = self.call("alice", "POST", "/api/servers", view)
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(self.row().config["hardware"], ODD_CLEAN)
        self.assertEqual(failing(), {})

    def test_apply_of_everything_takes_what_the_report_has(self):
        """"Apply all of the last report" (no sections named) on a report of models alone (a helper started with
        --no-hardware) while models are manual: the button shows (the models differ), the models come in, the sections
        with nothing to copy are passed over and the audit row names what was applied; a section named with nothing to
        copy is still a 409."""
        settings, apply = f"/api/machines/{self.helper}/settings", f"/api/machines/{self.helper}/hardware/apply"
        self.call("alice", "PUT", settings, {"modes": {"models": "manual"}})
        self.report(models=[model("m", loaded=True)])
        self.assertTrue(self.call("alice", "GET", settings).json()["differences"]["models"])
        r = self.call("alice", "POST", apply, {})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertTrue(catalogue(self.row())["m"]["loaded"])
        self.assertEqual(r.json()["differences"]["models"], [])
        self.assertEqual(self.call("alice", "POST", apply, {"sections": ["hardware"]}).status_code, 409)
        with db.session() as s:
            applied = [a.detail.get("sections") for a in s.scalars(select(AuditLog).where(
                AuditLog.action == "server.scan_applied"))]
        self.assertEqual(applied, [["models"]])


# ---------------------------------------------------------------------------- the prompt scheduler

class Scheduler(_Routes):
    """Gate 3: what the dialog sets of slots and swaps reaches the prompt scheduler at once."""

    def setUp(self):
        super().setUp()
        # a connected helper: four slots of its own, one model loaded and one not
        from tests import helperfake as H
        self.conn = H.StubConnection(self.helper, [H.model("warm"), H.model("cold", loaded=False)], slots=4)
        W.hub().register(self.conn)
        self.addCleanup(W.hub().unregister, self.conn)
        # only the dialog's route may refresh the machine: the scheduler's own reread (every 5 s) is off
        patcher = mock.patch.object(scheduler, "SETTINGS_SECONDS", 1e9)
        patcher.start()
        self.addCleanup(patcher.stop)

    def ticket(self, model_key):
        t = scheduler.get().ticket(self.helper, model_key, cls="test", consumer=self.ids["alice"])
        self.addCleanup(t.close)
        return t

    def test_slots_and_swaps_change_the_capacity_and_the_loaded_model_rule_at_once(self):
        m = scheduler.get().machine(self.helper)
        self.assertEqual(m.capacity(), 1)
        refused = self.ticket("cold")
        self.assertEqual((refused.state, refused.error.code), ("refused", "not_loaded"))
        self.assertEqual(self.ticket("warm").state, "queued")

        r = self.call("alice", "PUT", f"/api/machines/{self.helper}/settings",
                      {"slots": 3, "capabilities": {"llm": {"allow_swaps": True}}})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(m.capacity(), 3)
        # protocol 2: the helper's slots are what it acknowledged for the site's 3, its own ceiling (4) not reached
        self.assertEqual(r.json()["in_effect"], {"capacity": 3, "slots": 3, "helper_slots": 3, "llm": True,
                                                 "allow_swaps": True, "online": True, "known": True})
        self.assertEqual(self.ticket("cold").state, "queued", "swaps allowed: a model not loaded is taken")

        r = self.call("alice", "PUT", f"/api/machines/{self.helper}/settings",
                      {"slots": 8, "capabilities": {"llm": {"allow_swaps": False}}})
        self.assertEqual(m.capacity(), 4, "the helper's own ceiling still caps the slots")
        self.assertEqual(r.json()["in_effect"]["capacity"], 4)
        again = self.ticket("cold")
        self.assertEqual((again.state, again.error.code), ("refused", "not_loaded"))

        self.call("zara", "PUT", f"/api/machines/{self.helper}/settings", {"capabilities": {"llm": {"enabled": False}}})
        off = self.ticket("warm")
        self.assertEqual((off.state, off.error.code), ("refused", "disabled"))
        lease = scheduler.get().lease(self.helper, "warm", kind="game", consumer=self.ids["alice"])
        self.addCleanup(lease.release)
        with self.assertRaises(scheduler.MachineRefused) as caught:
            m.take(m.submit(lease))
        self.assertIn("takes no model work", str(caught.exception), "a seat's request is refused too")
        self.assertFalse(self.call("alice", "GET", f"/api/machines/{self.helper}/settings").json()["in_effect"]["llm"])

    def test_without_the_dialogs_refresh_the_change_would_wait_for_the_reread(self):
        """The control: the same change written to the row directly (not through the route) is not seen until the
        scheduler reads the machine again, which this test turned off; so the route's refresh is what makes it at
        once."""
        m = scheduler.get().machine(self.helper)
        with db.session() as s:
            s.get(Server, self.helper).max_concurrent = 3
        self.assertEqual(m.capacity(), 1)
        scheduler.get().refresh(self.helper)
        self.assertEqual(m.capacity(), 3)


if __name__ == "__main__":
    unittest.main()
