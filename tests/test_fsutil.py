"""Readers of files another thread or process replaces (citar/fsutil.py). On Windows, for the moment ``os.replace``
takes, the old file is pending deletion and opening it fails with PermissionError; the suite's merge run of 3-10 caught
a probe run's ``run.json`` read so (``test_pooled_work.Probes``), so a reader tries again, as the writer does. One test
per family of such files: each reader is refused its file once and must read it again."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import collections
import contextlib
import gzip
import io
import json
import os
import secrets
import shutil
import tempfile
import types
import unittest
from pathlib import Path
from unittest import mock

from citar import engine_api, fsutil, items, keystore, lab, probes
from citar.bots import profiles as bot_profiles, ratings
from citar.pool import registry_import
from citar.reports import ReportRunner, data as report_data
from citar.server import benchmarks, bots_api, users_api


def refusing(times: int, name: str, method: str = "read_text"):
    """A ``Path.read_text`` (or ``Path.<method>``) that refuses the file called *name* its first *times* reads, as
    Windows does while the file is being replaced, and the calls it saw."""
    real = getattr(Path, method)
    seen = []

    def read(self, *args, **kwargs):
        if self.name == name:
            seen.append(self)
            if len(seen) <= times:
                raise PermissionError(13, "Permission denied", str(self))
        return real(self, *args, **kwargs)
    return read, seen


class Refused(unittest.TestCase):
    """A temporary folder, and ``read_refused_once``."""

    def setUp(self):
        self.dir = Path(tempfile.mkdtemp(prefix="citar-fsutil-"))
        self.addCleanup(shutil.rmtree, self.dir, ignore_errors=True)

    def read_refused_once(self, name: str, read, method: str = "read_text"):
        """What ``read()`` returns while the file called *name* refuses its first read, checking it was read again."""
        refuse, seen = refusing(1, name, method)
        with mock.patch.object(Path, method, refuse), mock.patch.object(fsutil.time, "sleep"):
            out = read()
        self.assertGreaterEqual(len(seen), 2, f"{name} was not read again")
        return out


class ReadText(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp(prefix="citar-fsutil-"))
        self.addCleanup(shutil.rmtree, self.dir, ignore_errors=True)

    def test_a_read_refused_for_a_moment_is_tried_again(self):
        p = self.dir / "a.json"
        fsutil.write_text(p, '{"x": 1}')
        read_text, seen = refusing(2, "a.json")
        with mock.patch.object(Path, "read_text", read_text), mock.patch.object(fsutil.time, "sleep") as slept:
            self.assertEqual(fsutil.read_text(p), '{"x": 1}')
        self.assertEqual(len(seen), 3)
        self.assertEqual(slept.call_count, 2)

    def test_a_lasting_refusal_still_raises(self):
        p = self.dir / "a.json"
        fsutil.write_text(p, "{}")
        read_text, seen = refusing(10**6, "a.json")
        with mock.patch.object(Path, "read_text", read_text), mock.patch.object(fsutil.time, "sleep"):
            with self.assertRaises(PermissionError):
                fsutil.read_text(p)
        self.assertEqual(len(seen), 40)

    def test_a_binary_read_refused_for_a_moment_is_tried_again(self):
        p = self.dir / "a.bin"
        p.write_bytes(b"\x00\x01")
        read_bytes, seen = refusing(1, "a.bin", "read_bytes")
        with mock.patch.object(Path, "read_bytes", read_bytes), mock.patch.object(fsutil.time, "sleep"):
            self.assertEqual(fsutil.read_bytes(p), b"\x00\x01")
        self.assertEqual(len(seen), 2)

    def test_a_missing_file_is_not_waited_for(self):
        with mock.patch.object(fsutil.time, "sleep") as slept:
            with self.assertRaises(FileNotFoundError):
                fsutil.read_text(self.dir / "none.json")
        slept.assert_not_called()


class ProbeFiles(unittest.TestCase):
    """A probe run's file is rewritten by its runner while the page, the queue and the runner's other threads read it;
    a probe's file by its editor while the Probes page lists it."""

    def test_a_run_read_while_its_file_is_replaced(self):
        runner = probes.ProbeRunner.__new__(probes.ProbeRunner)   # its files only: no threads, no queue
        rid = f"fsutil-run-{secrets.token_hex(3)}"
        self.addCleanup(shutil.rmtree, probes.RUNS / rid, ignore_errors=True)
        runner._write({"id": rid, "status": "waiting (machine busy)"})
        for read in (lambda: runner.get_run(rid), lambda: next(r for r in runner.list_runs() if r["id"] == rid)):
            read_text, seen = refusing(1, "run.json")
            with mock.patch.object(Path, "read_text", read_text), mock.patch.object(fsutil.time, "sleep"):
                self.assertEqual(read()["status"], "waiting (machine busy)")
            self.assertGreaterEqual(len(seen), 2)

    def test_a_probe_read_while_its_file_is_replaced(self):
        pid = f"fsutil-probe-{secrets.token_hex(3)}"
        path = probes._path(pid)
        path.parent.mkdir(parents=True, exist_ok=True)
        fsutil.write_text(path, json.dumps({"id": pid, "name": "fsutil probe", "cases": []}))
        self.addCleanup(probes.delete_probe, pid)
        for read in (lambda: probes.load_probe(pid), lambda: next(p for p in probes.list_probes() if p["id"] == pid)):
            read_text, seen = refusing(1, path.name)
            with mock.patch.object(Path, "read_text", read_text), mock.patch.object(fsutil.time, "sleep"):
                self.assertEqual(read()["id"], pid)
            self.assertGreaterEqual(len(seen), 2)


class LabFiles(Refused):
    """The runner replaces ``status.json`` every pass while the Lab page (``overview``) and ``lab status`` read it; the
    server replaces a queued experiment's file when its priority changes (``set_priority``) while the runner, the Lab
    and Bots pages and the ratings read it; ``submit`` writes it, and the runner moves it to done/ when it finishes."""

    def setUp(self):
        super().setUp()
        for name, sub in (("LAB", ""), ("QUEUE", "queue"), ("DONE", "done"), ("RESULTS", "results"), ("JOBS", "jobs")):
            patcher = mock.patch.object(lab, name, self.dir / sub)
            patcher.start()
            self.addCleanup(patcher.stop)
        lab._dirs()

    def test_the_runner_status_read_while_the_runner_replaces_it(self):
        fsutil.write_text(lab.LAB / "status.json", json.dumps({
            "pid": 4242, "updated": "2026-10-12T00:00:00", "stopped": False, "quiet_hours": False,
            "target_workers": 2, "running": [], "games_last_hour": 0, "queue": {}}))
        with mock.patch.object(lab, "_pid_alive", return_value=True):
            runner = self.read_refused_once("status.json", lab.overview)["runner"]
            self.assertEqual((runner["pid"], runner["alive"], runner["workers"]), (4242, True, 2))
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                self.read_refused_once("status.json", lab.cmd_status)
        self.assertIn("runner pid 4242 ALIVE", out.getvalue())

    def test_a_queued_experiment_read_while_its_priority_changes(self):
        name = "fsutil-exp.json"
        fsutil.write_text(lab.QUEUE / name, json.dumps({
            "name": "fsutil-exp", "games": 2, "priority": 0, "submitted": "2026-10-12T00:00:00",
            "seats": [{"profile": "fsutil-bot", "label": "fsutil bot"}, {"bot": "basic", "label": "basic"}]}))
        self.assertEqual([e["name"] for e in self.read_refused_once(name, lab.load_queue)], ["fsutil-exp"])
        self.assertEqual([e["name"] for e in self.read_refused_once(name, lab.overview)["experiments"]], ["fsutil-exp"])
        self.assertEqual([e["name"] for e in self.read_refused_once(name, lambda: bots_api._experiments_with("fsutil-bot"))],
                         ["fsutil-exp"])
        self.assertEqual([spec["name"] for spec, _ in self.read_refused_once(name, ratings._lab_files)], ["fsutil-exp"])
        self.assertEqual(self.read_refused_once(name, lambda: lab.set_priority("fsutil-exp", 7)), 7)
        self.assertEqual(json.loads((lab.QUEUE / name).read_text(encoding="utf-8"))["priority"], 7)

    def test_an_experiment_finished_between_the_listing_and_its_read(self):
        """Not a replace but a rename: the runner moves a finished experiment's file to done/ whenever it finishes.
        Listed in the queue and gone when read, it is shown from done/, not a failed page."""
        name = "fsutil-exp.json"
        fsutil.write_text(lab.QUEUE / name, json.dumps({
            "name": "fsutil-exp", "games": 2, "submitted": "2026-10-12T00:00:00", "seats": []}))
        real = Path.read_text

        def read_text(path, *args, **kwargs):
            if path.parent == lab.QUEUE and path.name == name:
                fsutil.replace(path, lab.DONE / name)       # the runner, finishing it
            return real(path, *args, **kwargs)
        with mock.patch.object(Path, "read_text", read_text):
            got = lab.overview()["experiments"]
        self.assertEqual([(e["name"], e["state"]) for e in got], [("fsutil-exp", "complete")])

    def test_an_experiment_finished_after_its_read_shows_once(self):
        """Moved to done/ after its queue file was read and before done/ is listed, it would show twice, queued and
        complete: the overview reads the folders again. One queued again under the finished one's name (``lab
        submit`` allows it) is another experiment, and both show."""
        name = "fsutil-exp.json"
        spec = {"name": "fsutil-exp", "games": 2, "submitted": "2026-10-12T00:00:00", "seats": []}
        fsutil.write_text(lab.QUEUE / name, json.dumps(spec))
        real = Path.read_text

        def read_text(path, *args, **kwargs):
            text = real(path, *args, **kwargs)
            if path.parent == lab.QUEUE and path.name == name:
                fsutil.replace(path, lab.DONE / name)       # the runner, finishing it just after
            return text
        with mock.patch.object(Path, "read_text", read_text):
            got = lab.overview()["experiments"]
        self.assertEqual([(e["name"], e["state"]) for e in got], [("fsutil-exp", "complete")])
        fsutil.write_text(lab.QUEUE / name, json.dumps({**spec, "submitted": "2026-10-12T01:00:00"}))
        got = lab.overview()["experiments"]
        self.assertEqual([(e["name"], e["state"]) for e in got], [("fsutil-exp", "queued"), ("fsutil-exp", "complete")])

    def test_an_experiment_read_while_it_is_submitted(self):
        """``submit`` writes the file whole, by a temporary file and a rename: the Lab page polling meanwhile never
        reads half of it."""
        real = Path.write_text
        polled = []

        def write_text(path, text, *args, **kwargs):
            real(path, text[: len(text) // 2], *args, **kwargs)        # half of it written so far
            polled.append(lab.overview()["experiments"])
            return real(path, text, *args, **kwargs)
        with mock.patch.object(Path, "write_text", write_text):
            lab.submit({"name": "fsutil-exp", "games": 2, "seats": [{"bot": "basic"}, {"bot": "basic"}]})
        self.assertEqual(polled, [[]])
        self.assertEqual([e["name"] for e in lab.overview()["experiments"]], ["fsutil-exp"])

    def test_a_finished_experiment_is_renamed_over_a_done_one_of_its_name(self):
        """The runner moves a finished experiment to done/ by a rename, also over an earlier experiment of its name
        there (``lab submit`` may reuse a finished one's name), so a reader of done/ sees one whole file or the other.
        A move that met a file there copied over it in place, readable half written."""
        name = "fsutil-exp.json"
        spec = {"name": "fsutil-exp", "games": 1, "seats": []}
        fsutil.write_text(lab.DONE / name, json.dumps({**spec, "submitted": "2026-10-01T00:00:00"}))
        fsutil.write_text(lab.QUEUE / name, json.dumps({**spec, "submitted": "2026-10-12T00:00:00"}))
        (lab.RESULTS / "fsutil-exp.jsonl").write_text(json.dumps({"i": 0}) + "\n", encoding="utf-8")
        finished = (lab.QUEUE / name).stat().st_ino
        args = types.SimpleNamespace(workers=1, night_workers=1, max_minutes=60, exit_when_idle=False)
        self.assertFalse(lab._runner_step(args, {}, collections.Counter(), [], 0, 1))
        self.assertFalse((lab.QUEUE / name).exists())
        self.assertEqual(json.loads((lab.DONE / name).read_text(encoding="utf-8"))["submitted"], "2026-10-12T00:00:00")
        self.assertEqual((lab.DONE / name).stat().st_ino, finished, "copied over the done file, not renamed into place")

    def test_a_done_experiment_reported_while_its_file_is_replaced(self):
        """The Lab page's report of an experiment reads its spec from done/, where a re-queued name's finishing
        replaces it."""
        fsutil.write_text(lab.DONE / "fsutil-exp.json", json.dumps({"name": "fsutil-exp", "games": 2, "seats": []}))
        text = self.read_refused_once("fsutil-exp.json", lambda: lab.report_text("fsutil-exp"))
        self.assertIn("=== fsutil-exp: 0 games of 2", text)


class BotProfileFiles(Refused):
    """A saved bot profile is replaced by its editor (``profiles.save``) while games, the lab and the Bots page read
    it; refused, it reads as no such profile."""

    def test_a_profile_read_while_its_file_is_replaced(self):
        pid = f"fsutil-{secrets.token_hex(3)}"
        path = bot_profiles._path(pid)
        path.parent.mkdir(parents=True, exist_ok=True)
        fsutil.write_text(path, json.dumps({"id": pid, "name": "fsutil bot", "engine": "basic", "params": {}}))
        self.addCleanup(path.unlink, missing_ok=True)
        self.assertEqual(self.read_refused_once(path.name, lambda: bot_profiles.get(pid))["name"], "fsutil bot")
        self.assertIn(pid, [p["id"] for p in self.read_refused_once(path.name, bot_profiles.list_profiles)])


class MapAndScenarioFiles(Refused):
    """A map or a scenario is replaced by the editor's save while the editor's lists, the lobby and the next save
    read it."""

    def test_a_map_read_while_its_file_is_replaced(self):
        mid = f"fsutil-map-{secrets.token_hex(3)}"
        blank = {**engine_api.blank_map(12, 10, "Grassland", "fsutil map"), "id": mid}
        saved, _ = engine_api.save_map(blank)
        self.addCleanup(engine_api.delete_map, mid)
        path = engine_api.map_file(mid)
        self.assertEqual(self.read_refused_once(path.name, lambda: engine_api.load_map(mid))["id"], mid)
        self.assertIn(mid, [m["id"] for m in self.read_refused_once(path.name, engine_api.list_maps)])
        # a save keeps the date the map was made, read from the file it replaces
        fsutil.write_text(path, json.dumps({**saved, "created": "2020-01-01T00:00:00"}))
        again, _ = self.read_refused_once(path.name, lambda: engine_api.save_map(blank))
        self.assertEqual(again["created"], "2020-01-01T00:00:00")

    def test_a_scenario_read_while_its_file_is_replaced(self):
        sid = f"fsutil-scn-{secrets.token_hex(3)}"
        game = engine_api.EngineGame.new({"map_size": "duel", "seed": 11, "barbarians": "off",
                                          "players": [{"controller": "bot"}, {"controller": "bot"}]})
        game.save_scenario(sid, "fsutil scenario")
        path = engine_api.scenario_file(sid)
        self.addCleanup(path.unlink, missing_ok=True)
        loaded = self.read_refused_once(path.name, lambda: engine_api.load_scenario(sid), "read_bytes")
        self.assertEqual(loaded["name"], "fsutil scenario")
        self.assertIn(sid, [s["id"] for s in self.read_refused_once(path.name, engine_api.list_scenarios, "read_bytes")])


class AdoptedFiles(Refused):
    """Adoption (``items.adopt_orphans``) also runs when the first administrator is made, with the server running:
    an item's file it reads may be being replaced."""

    def test_an_item_file_read_while_it_is_replaced(self):
        p = self.dir / "fsutil-bot.json"
        fsutil.write_text(p, json.dumps({"id": "fsutil-bot", "name": "fsutil bot"}))
        self.assertEqual(self.read_refused_once(p.name, lambda: items._describe("bot_profile", p)), ("fsutil bot", None))
        scn = self.dir / "fsutil.citarscn"
        scn.write_bytes(gzip.compress(json.dumps({"name": "fsutil scenario"}).encode("utf-8")))
        self.assertEqual(self.read_refused_once(scn.name, lambda: items._describe("scenario", scn), "read_bytes"),
                         ("fsutil scenario", None))


class KeyFile(Refused):
    """The encrypted key file is replaced when a key is stored, while model calls read it without the store's lock
    (``keystore.get``); refused, the key reads as missing."""

    def test_a_key_read_while_the_file_is_replaced(self):
        path = self.dir / "keys.enc"
        server = {"id": "sv_fsutil", "connection": {"key": {"backend": "file"}}}
        with mock.patch.object(keystore, "_file_path", return_value=path), \
                mock.patch.object(keystore, "_passphrase", "fsutil passphrase"):
            keystore.store("sv_fsutil", "file", "sk-fsutil-1")
            self.assertEqual(self.read_refused_once(path.name, lambda: keystore.get(server)), "sk-fsutil-1")
            # the next write reads the file's salt before it replaces it
            self.read_refused_once(path.name, lambda: keystore._file_write("fsutil passphrase",
                                                                           {"server:sv_fsutil": "sk-fsutil-2"}))
            self.assertEqual(keystore.get(server), "sk-fsutil-2")


class RegistryFile(Refused):
    """``config/servers.json`` may be being replaced by ``citar setup --local`` (``servers.register_offline``) as the
    server imports it for its first administrator."""

    def test_the_registry_read_while_a_tool_replaces_it(self):
        p = self.dir / "servers.json"
        fsutil.write_text(p, json.dumps({"format": "citar-servers", "version": 1, "servers": []}))
        self.assertEqual(self.read_refused_once(p.name, lambda: registry_import.read_file(p))["servers"], [])


class ReportFiles(Refused):
    """A report's ``meta.json`` is replaced at every step of its build while the Reports page reads it; a report
    being built gathers benchmark and probe runs whose files their schedulers replace as they go."""

    def test_a_report_read_while_its_record_is_replaced(self):
        runner = ReportRunner.__new__(ReportRunner)     # its files only: no threads
        runner.dir, runner._deleting, runner._places = self.dir, set(), {}
        rid = "20261012-000000-f5f5"
        (self.dir / rid).mkdir()
        fsutil.write_text(self.dir / rid / "meta.json", json.dumps({"id": rid, "title": "fsutil", "status": "running"}))
        self.assertEqual(self.read_refused_once("meta.json", lambda: runner.meta(rid))["title"], "fsutil")
        self.assertEqual([m["id"] for m in self.read_refused_once("meta.json", runner.list)], [rid])

    def test_runs_gathered_while_their_files_are_replaced(self):
        (self.dir / "bench" / "runs").mkdir(parents=True)
        fsutil.write_text(self.dir / "bench" / "runs" / "fsutil-run.json",
                          json.dumps({"id": "fsutil-run", "name": "fsutil run", "jobs": []}))
        with mock.patch.object(benchmarks, "BENCH_DIR", self.dir / "bench"):
            got = self.read_refused_once("fsutil-run.json", lambda: report_data._benchmark_runs({"fsutil-run"}))
        self.assertEqual([r["id"] for r in got], ["fsutil-run"])
        (self.dir / "probe-runs" / "fsutil-probe-run").mkdir(parents=True)
        fsutil.write_text(self.dir / "probe-runs" / "fsutil-probe-run" / "run.json", json.dumps({"id": "fsutil-probe-run"}))
        with mock.patch.object(probes, "RUNS", self.dir / "probe-runs"):
            got = self.read_refused_once("run.json", lambda: report_data._probe_runs({"fsutil-probe-run"}))
        self.assertEqual([r["id"] for r in got], ["fsutil-probe-run"])


class BenchmarkSuites(Refused):
    """A suite is replaced when it is saved while the Benchmarks page lists it or opens it."""

    def test_a_suite_read_while_its_file_is_replaced(self):
        sched = benchmarks.BenchmarkScheduler.__new__(benchmarks.BenchmarkScheduler)    # its files only
        sched.suites_dir = self.dir
        sched._write(self.dir / "fsutil-suite.json",
                     {"id": "fsutil-suite", "name": "fsutil suite", "servers": [], "scenarios": []})
        name = "fsutil-suite.json"
        self.assertEqual(self.read_refused_once(name, lambda: sched.get_suite("fsutil-suite"))["name"], "fsutil suite")
        self.assertEqual([s["id"] for s in self.read_refused_once(name, sched.list_suites)], ["fsutil-suite"])


class AvatarFile(Refused):
    """An avatar's file is named by its digest, so uploading the same image again replaces it while it may be being
    served."""

    def test_an_avatar_served_while_the_same_image_is_uploaded_again(self):
        png = self.dir / "u1-0123456789ab.png"
        png.write_bytes(b"\x89PNG\r\n\x1a\nfsutil")
        user = mock.Mock()
        user.field_levels.return_value = {"avatar": "global", "profile": "global"}
        with mock.patch.object(users_api.profiles, "require_visible", return_value=user), \
                mock.patch.object(users_api.profiles, "avatar_file", return_value=png):
            got = self.read_refused_once(png.name, lambda: users_api.avatar("pat", mock.Mock(), None), "read_bytes")
        self.assertEqual(got.body, b"\x89PNG\r\n\x1a\nfsutil")


if __name__ == "__main__":
    unittest.main()


class Listings(Refused):
    """Listings sorted newest first (``fsutil.newest_first``): a file the glob found and then deleted is left out, never
    raised out of the listing (CI, test.yml run 38415083488, macos / Python 3.12: the Reports list raised
    FileNotFoundError for a report whose build removed its folder between the glob and the sort's stat)."""

    def test_newest_first_and_the_gone_left_out(self):
        for i, name in enumerate(("old", "mid", "new")):
            p = self.dir / f"{name}.json"
            p.write_text("{}", encoding="utf-8")
            os.utime(p, (1_000_000 + i, 1_000_000 + i))
        listed = sorted(self.dir.glob("*.json")) + [self.dir / "gone.json"]
        got = fsutil.newest_first(listed)
        self.assertEqual([p.name for p, _ in got], ["new.json", "mid.json", "old.json"])
        self.assertEqual([st.st_mtime for _, st in got], [1_000_002, 1_000_001, 1_000_000])

    def test_a_stat_refused_for_a_moment_is_tried_again(self):
        p = self.dir / "a.json"
        p.write_text("{}", encoding="utf-8")
        refuse, seen = refusing(1, "a.json", "stat")
        with mock.patch.object(Path, "stat", refuse), mock.patch.object(fsutil.time, "sleep"):
            got = fsutil.newest_first([p])
        self.assertEqual([q for q, _ in got], [p])
        self.assertEqual(len(seen), 2)

    def test_a_report_deleted_between_the_glob_and_the_stat_is_left_out(self):
        runner = ReportRunner.__new__(ReportRunner)     # its files only: no threads
        runner.dir, runner._deleting, runner._places = self.dir, set(), {}
        for rid in ("20261013-000000-aaaa", "20261013-000000-bbbb"):
            (self.dir / rid).mkdir()
            fsutil.write_text(self.dir / rid / "meta.json", json.dumps({"id": rid, "title": rid, "status": "done"}))
        found = list(self.dir.glob("*/meta.json"))
        shutil.rmtree(self.dir / "20261013-000000-aaaa")        # its build let go of it after the glob
        with mock.patch.object(Path, "glob", lambda self_, pattern: iter(found)):
            self.assertEqual([m["id"] for m in runner.list()], ["20261013-000000-bbbb"])

    def test_a_save_deleted_between_the_glob_and_its_header_is_left_out(self):
        from citar.server import session as sess
        for gid in ("g1", "g2"):
            (self.dir / gid).mkdir()
            (self.dir / gid / "autosave.citar").write_bytes(b"x")

        def meta(p):
            if p.parent.name == "g1":
                raise FileNotFoundError(2, "No such file", str(p))      # deleted after the glob, before its header
            return {"name": "kept"}
        with mock.patch.object(sess, "SAVE_DIR", self.dir),                 mock.patch.object(sess.SessionManager, "save_meta", side_effect=meta):
            saves = sess.SessionManager.list_saves()
        self.assertEqual([(s["game_id"], s["name"]) for s in saves], [("g2", "kept")])
