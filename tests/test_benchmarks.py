"""Benchmark scheduler tests using the dry-run provider (no model server, no GPU)."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import shutil
import tempfile
import time
import unittest
from datetime import datetime, timedelta
from pathlib import Path
from unittest import mock

from citar import servers as REG
from citar.server.benchmarks import BenchmarkScheduler, normalize_suite, performance
from citar.server.session import SessionManager, SAVE_DIR


def dry_server(i, models, max_parallel=1, delay=0.0, restricted=None):
    """A dry-run server in the (temporary) registry with the given pretend models."""
    return REG.upsert({"id": f"sv_benchdry{i}", "name": f"Dry {i}", "kind": "test",
                       "connection": {"provider": "dryrun", "max_parallel": max_parallel, "dry_run_delay": delay},
                       "restricted_hours": restricted or {}, "models": [{"id": f"m_{m}_{i}", "key": f"{m}-{i}"} for m in models]})


def dry_suite(mode="sequential", models=("dry-a", "dry-b"), turn_limit=3, servers=1, max_parallel=1, delay=0.0, restricted=None):
    groups = []
    for i in range(servers):
        sv = dry_server(i, models, max_parallel, delay, restricted)
        groups.append({"server_id": sv["id"], "models": [{"model_id": m["id"]} for m in sv["models"]]})
    return {
        "name": "Test suite", "mode": mode, "servers": groups,
        "scenarios": [{"name": "Tiny duel", "map_size": "duel", "map_type": "pangaea", "seed": 5, "opponents": 1,
                       "turn_limit": turn_limit, "barbarians": "off", "max_turn_minutes": 5}],
    }


def wait_for(pred, timeout=90, step=0.2, sch=None):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if sch is not None:
            sch.tick()
        if pred():
            return True
        time.sleep(step)
    return False


def why(sch, run):
    """Everything the scheduler knows about why a run is not where the test expected it.

    A job that never starts fails an `assertTrue(wait_for(...))` as "False is not true", and the
    reason - which the scheduler does record - sits in `job["error"]` and in `sch.log`, neither of
    which unittest ever prints. `_launch_job` marks a job failed and returns quietly, so without
    this the only evidence left behind is that a timeout elapsed. Used as the message of every
    wait a test does on job state.
    """
    jobs = ", ".join(f"{j['label']}={j['status']}" + (f" ({j['error']})" if j.get("error") else "")
                     for j in run["jobs"])
    tail = " | ".join(entry["msg"] for entry in sch.log[-8:])
    return "\n".join([f"run={run['status']} jobs: {jobs}",
                      f"restricted={sorted(sch._restricted)}",
                      f"log: {tail}"])


def started(sch, run, job, timeout=30):
    """Wait until a job has left the queue, and return whatever it became.

    Deliberately not a wait for ``"running"``. A job that fails during launch goes straight to
    ``failed``, and a caller waiting for ``running`` then burns its whole timeout before saying
    nothing useful; a job can also pass through ``running`` between two polls. Waiting for "no
    longer starting up" and asserting on what it became reports the real state either way.
    """
    wait_for(lambda: job["status"] not in ("queued", "loading"), timeout=timeout, sch=sch)
    return job["status"]


class BenchmarkTests(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp(prefix="citar-bench-"))
        self.manager = SessionManager()
        self.games = set()

    def tearDown(self):
        for sid in list(self.manager.sessions):
            self.games.add(sid)
            self.manager.delete(sid)
        for sch in getattr(self, "_schedulers", []):
            sch.stop()
            for run in sch.runs.values():
                self.games.update(j["game_id"] for j in run["jobs"] if j.get("game_id"))
        for gid in self.games:
            shutil.rmtree(SAVE_DIR / gid, ignore_errors=True)
        shutil.rmtree(self.dir, ignore_errors=True)

    def scheduler(self, manager=None):
        sch = BenchmarkScheduler(manager or self.manager, self.dir, autostart=False)
        self._schedulers = getattr(self, "_schedulers", []) + [sch]
        return sch

    def test_suite_roundtrip_and_job_count(self):
        sch = self.scheduler()
        saved = sch.save_suite(dry_suite())
        self.assertEqual(sch.get_suite(saved["id"])["name"], "Test suite")
        listed = sch.list_suites()
        self.assertEqual(listed[0]["jobs"], 2)
        # swapping the models of a saved suite is just an edit + save
        suite = sch.get_suite(saved["id"])
        suite["servers"][0]["models"] = [{"model_id": "m_dry-b_0"}]
        sch.save_suite(suite)
        self.assertEqual(sch.list_suites()[0]["servers"][0]["models"], ["dry-b-0"])
        sch.delete_suite(saved["id"])
        self.assertEqual(sch.list_suites(), [])

    def test_scenario_map_options_reach_the_game(self):
        sch = self.scheduler()
        suite = dry_suite(models=("dry-a",))
        suite["scenarios"][0].update({"map_edges": "wrap_x", "river_density": 0,
                                      "resources": {"strategic": {"each": {"Uranium": {"mode": "off"}}}}})
        run = sch.create_run(suite)
        job = run["jobs"][0]
        self.assertTrue(wait_for(lambda: job.get("game_id") and self.manager.get(job["game_id"]), sch=sch))
        m = self.manager.get(job["game_id"]).game.export_map()      # tiles as [terrain, features, wonder, river, resource, ...]
        self.assertTrue(m["wrap_x"])
        self.assertFalse(any(t[3] for t in m["tiles"]))
        self.assertFalse(any(t[4] == "Uranium" for t in m["tiles"]))

    def test_a_job_ends_when_its_model_is_eliminated(self):
        sch = self.scheduler()
        run = sch.create_run(dry_suite(models=("dry-a",), turn_limit=200, delay=0.05))
        job = run["jobs"][0]
        self.assertTrue(wait_for(lambda: job["status"] == "running" and self.manager.get(job["game_id"]), sch=sch))
        s = self.manager.get(job["game_id"])
        with s.lock:
            s.game.python_game.player(0).alive = False
        self.assertTrue(wait_for(lambda: job["status"] == "done", sch=sch, timeout=30))
        self.assertEqual(job["result"]["outcome"], "eliminated")
        self.assertEqual(job["result"]["performance"], 0)
        self.assertTrue(s.stopped, "the bots should not play out a settled game")

    def test_sequential_run_plays_each_model_to_the_turn_limit(self):
        sch = self.scheduler()
        run = sch.create_run(dry_suite())
        seen_parallel = []

        def done():
            active = [j for j in run["jobs"] if j["status"] in ("loading", "running")]
            seen_parallel.append(len(active))
            return run["status"] == "done"

        self.assertTrue(wait_for(done, sch=sch), [j["status"] for j in run["jobs"]])
        self.assertLessEqual(max(seen_parallel), 1, "sequential runs play one job at a time")
        for job in run["jobs"]:
            self.assertEqual(job["status"], "done", job.get("error"))
            self.assertEqual(job["result"]["phase"], "over")
            self.assertGreaterEqual(job["result"]["turns_played"], 3)
            self.assertIsNotNone(job["result"]["performance"])
            self.assertTrue((SAVE_DIR / job["game_id"] / "benchmark.citar").exists())
        summary = sch.list_runs()[0]["summary"]
        self.assertEqual(summary["counts"], {"done": 2})
        self.assertEqual(summary["turns_done"], summary["turns_total"])

    def test_parallel_run_uses_every_server(self):
        sch = self.scheduler()
        run = sch.create_run(dry_suite(mode="parallel", models=("m",), servers=2, turn_limit=12))
        both = wait_for(lambda: sum(1 for j in run["jobs"] if j["status"] in ("loading", "running")) == 2, timeout=40, step=0.05, sch=sch)
        self.assertTrue(both, "jobs on different servers should run at the same time: " + why(sch, run))
        self.assertTrue(wait_for(lambda: run["status"] == "done", timeout=300, sch=sch))

    def test_restricted_hours_pause_and_resume(self):
        sch = self.scheduler()
        clock = {"now": datetime(2026, 1, 1, 0, 30)}
        sch._clock = lambda: clock["now"]
        # A long game, deliberately: this test watches a job being paused and resumed, so the game
        # has to still be playing while it looks. At turn_limit=30 a dry run finished in under
        # twenty seconds on a fast machine and the job was already `done` on the first check.
        run = sch.create_run(dry_suite(models=("q",), turn_limit=200, delay=0.02, restricted={
            "enabled": True, "unload_models": False, "grace_minutes": 0,
            "windows": [{"days": list(range(7)), "start": "01:00", "end": "02:00"}]}))
        job = run["jobs"][0]
        self.assertEqual(started(sch, run, job, timeout=20), "running", why(sch, run))
        game = self.manager.get(job["game_id"])
        self.assertTrue(wait_for(lambda: game.game.turn >= 2, timeout=30, sch=sch))

        clock["now"] = datetime(2026, 1, 1, 1, 15)       # the server's restricted hours begin
        sch.tick()
        self.assertIn(job["server_id"], sch._restricted)
        self.assertEqual((job["status"], job["pause_reason"]), ("paused", "restricted"))
        self.assertTrue(game.paused)
        turn = game.game.turn
        time.sleep(1.0)
        sch.tick()
        self.assertEqual(game.game.turn, turn, "no turns are played during quiet hours")
        interrupted = [r for r in game.metrics.data["turns"] if r.get("interrupted")]
        self.assertTrue(all(r["end_reason"] == "interrupted" for r in interrupted))

        clock["now"] = datetime(2026, 1, 1, 2, 5)        # quiet hours over
        self.assertTrue(wait_for(lambda: job["status"] == "running" and not game.paused, timeout=10, sch=sch))
        self.assertTrue(wait_for(lambda: game.game.turn > turn, timeout=30, sch=sch))

    def test_benchmark_game_ignores_lobby_quiet_hours_from_its_first_turn(self):
        """Quiet hours for a benchmark come from its scheduler, and its clock, only. The game used to start
        before it was marked as a benchmark, so the model's first turn went through the lobby's quiet-hours
        check on the real wall clock: between 01:00 and 02:00 test_restricted_hours_pause_and_resume
        found its job paused, with the scheduler's `_restricted` empty."""
        sch = self.scheduler()
        lobby_quiet = mock.patch.object(REG, "restricted_now", lambda sid: datetime.now() + timedelta(hours=1))
        with lobby_quiet:
            run = sch.create_run(dry_suite(models=("lq",), turn_limit=200, delay=0.02))
            job = run["jobs"][0]
            self.assertEqual(started(sch, run, job, timeout=20), "running", why(sch, run))
            game = self.manager.get(job["game_id"])
            self.assertTrue(wait_for(lambda: game.game.turn >= 2, timeout=30, sch=sch), why(sch, run))
            self.assertFalse(game.paused)
            self.assertEqual(job["status"], "running", why(sch, run))

    def test_restricted_hours_follow_the_machine_the_game_uses(self):
        """A seat moved to a re-registered machine (new id) must pause with that machine's hours, not the old id's."""
        sch = self.scheduler()
        run = sch.create_run(dry_suite(models=("q",), turn_limit=200, delay=0.02))
        job = run["jobs"][0]
        self.assertEqual(started(sch, run, job, timeout=20), "running", why(sch, run))
        game = self.manager.get(job["game_id"])
        quiet = {"on": True}
        sch.restricted_until = lambda sid: datetime.now() + timedelta(hours=1) if quiet["on"] and sid == "sv_moved" else None
        with game.lock:
            game.seats[0].llm = dict(game.seats[0].llm, server_id="sv_moved")
        with mock.patch.object(REG, "restriction_config", lambda sid: {"grace_minutes": 0}):
            sch.tick()
        self.assertEqual((job["status"], job["pause_reason"]), ("paused", "restricted"))
        self.assertEqual(job["machine_id"], "sv_moved")
        quiet["on"] = False
        self.assertTrue(wait_for(lambda: job["status"] in ("running", "resuming", "loading"), timeout=10, sch=sch))

    def test_higher_priority_work_takes_the_machine_and_gives_it_back(self):
        """A report put at the top of the queue waited for the whole running game to finish."""
        from citar.pool import queue as work_queue
        sch = self.scheduler()
        sch.PREEMPT_GRACE = 0
        run = sch.create_run(dry_suite(models=("q",), turn_limit=200, delay=0.02))
        job = run["jobs"][0]
        self.assertEqual(started(sch, run, job, timeout=20), "running", why(sch, run))
        game = self.manager.get(job["game_id"])
        machine = job["server_key"]
        other = {"items": []}
        work_queue.register("report", lambda: other["items"])
        try:
            same = {"kind": "report", "id": "r0", "group": "r0", "label": "report “same”", "server_id": machine,
                    "created": time.time()}
            other["items"] = [same]
            sch.tick()
            self.assertEqual(job["status"], "running", "equal priority never interrupts running work")
            work_queue.set_priority("report", "r1", 5)
            other["items"] = [dict(same, id="r1", group="r1", label="report “urgent”")]
            self.assertTrue(wait_for(lambda: job["status"] == "paused", timeout=20, sch=sch), why(sch, run))
            self.assertEqual(job["pause_reason"], "preempted")
            self.assertTrue(game.paused)
            self.assertEqual(game.pause_reason["kind"], "queue")
            from citar.pool import seats as pool_seats
            self.assertEqual(pool_seats.occupied(machine), [], "a game that made way does not hold the machine")
            self.assertIn(job["id"], [it["id"] for it in work_queue.waiting(machine)], "it waits in the queue at its rank")
            other["items"] = []                          # the urgent work has run
            self.assertTrue(wait_for(lambda: job["status"] == "running" and not game.paused, timeout=20, sch=sch), why(sch, run))
        finally:
            work_queue.set_priority("report", "r1", 0)
            work_queue._providers.pop("report", None)

    def test_overnight_restricted_window(self):
        sv = REG.normalize_server({"id": "sv_x", "name": "x", "restricted_hours": {"enabled": True, "windows": [
            {"days": [4], "start": "22:30", "end": "07:00"}]}})                  # Fridays only
        fri = datetime(2026, 1, 2)                                                 # 2026-01-02 is a Friday
        self.assertIsNotNone(REG.restricted(sv, fri.replace(hour=23)))
        self.assertEqual(REG.restricted(sv, datetime(2026, 1, 3, 6, 59)), datetime(2026, 1, 3, 7, 0))
        self.assertIsNone(REG.restricted(sv, datetime(2026, 1, 3, 7, 0)))
        self.assertIsNone(REG.restricted(sv, datetime(2026, 1, 1, 23, 0)), "Thursday night is not restricted")
        self.assertEqual(REG.normalize_server({"restricted_hours": {"windows": [{"start": "25:00"}]}})
                         ["restricted_hours"]["windows"][0]["start"], "21:00")

    def test_user_pause_resume_and_skip(self):
        sch = self.scheduler()
        # Long enough that the first job is still playing when the test pauses it; see the note in
        # test_restricted_hours_pause_and_resume.
        run = sch.create_run(dry_suite(models=("p", "s"), turn_limit=200, delay=0.02))
        first, second = run["jobs"]
        self.assertEqual(started(sch, run, first, timeout=20), "running", why(sch, run))
        sch.control_run(run["id"], "pause")
        self.assertEqual((run["status"], first["status"], first["pause_reason"]), ("paused", "paused", "user"))
        self.assertEqual(second["status"], "queued")
        sch.control_run(run["id"], "resume")
        self.assertTrue(wait_for(lambda: first["status"] == "running", timeout=10, sch=sch), why(sch, run))
        sch.control_job(run["id"], first["id"], "skip")
        self.assertEqual(first["status"], "cancelled")
        self.assertTrue(wait_for(lambda: second["status"] == "running", timeout=20, sch=sch),
                        "the next job starts after a skip: " + why(sch, run))
        sch.control_run(run["id"], "cancel")
        self.assertEqual(run["status"], "cancelled")

    def test_restart_reloads_games_in_progress(self):
        sch = self.scheduler()
        run = sch.create_run(dry_suite(models=("r",), turn_limit=30, delay=0.02))
        job = run["jobs"][0]
        self.assertTrue(wait_for(lambda: job["status"] == "running" and self.manager.get(job["game_id"]).game.turn >= 2,
                                 timeout=30, step=0.02, sch=sch), job)
        gid = job["game_id"]
        self.manager.get(gid).autosave(force=True)
        self.assertTrue(wait_for(lambda: (SAVE_DIR / gid / "autosave.citar").exists(), timeout=10))
        sch._flush()
        sch._dirty.add(run["id"])
        sch._flush()
        # simulate the server going away: drop the game from memory without touching the run file
        self.manager.get(gid).stop()
        self.manager.sessions.pop(gid)

        manager2 = SessionManager()
        sch2 = BenchmarkScheduler(manager2, self.dir, autostart=False)
        self._schedulers.append(sch2)
        sch2._recover()
        run2 = sch2.runs[run["id"]]
        self.assertIsNotNone(manager2.get(gid), "the game is reloaded from its autosave")
        self.assertTrue(wait_for(lambda: run2["status"] == "done", sch=sch2), [j["status"] for j in run2["jobs"]])
        for sid in list(manager2.sessions):
            manager2.delete(sid)

    def test_performance_measure(self):
        self.assertEqual(performance({0: 10, 1: 10}, 0, "playing", None, {0: True, 1: True}), 50.0)
        self.assertEqual(performance({0: 30, 1: 10}, 0, "playing", None, {0: True, 1: True}), 75.0)
        self.assertEqual(performance({0: 30, 1: 10}, 0, "over", 0, {0: True, 1: True}), 100.0)
        self.assertEqual(performance({0: 0, 1: 10}, 0, "over", 1, {0: False, 1: True}), 0.0)

    def test_model_scores_use_benchmark_games(self):
        from citar.server.scoring import model_scores
        sch = self.scheduler()
        run = sch.create_run(dry_suite(models=("scored",), turn_limit=3))
        self.assertTrue(wait_for(lambda: run["status"] == "done", sch=sch))
        weights = sch.get_settings()["score_weights"]
        rows = {r["model"]: r for r in model_scores(self.manager, weights)["models"]}
        self.assertNotIn("scored-0", rows, "dry-run seats test a setup and never count toward model scores")
        # the same game played by a real model does count
        game = self.manager.get(run["jobs"][0]["game_id"])
        game.seats[0].llm["provider"] = "openai_compatible"
        rows = {r["model"]: r for r in model_scores(self.manager, weights)["models"]}
        row = rows["scored-0"]
        self.assertEqual(row["benchmark_games"], 1)
        self.assertGreaterEqual(row["benchmark_turns"], 3)
        self.assertIsNotNone(row["overall"])
        self.assertTrue(0 <= row["overall"] <= 100)
        # once the game is only a save on disk, it scores the same (read through engine_api.state_summary)
        from citar.server.scoring import _digest_save, _reports
        live = _reports(self.manager)[game.id]
        saved = _digest_save(game.save("scored"))
        self.assertEqual((saved["turn"], set(saved["summary"])), (live["turn"], set(live["summary"])))
        for k in ("model", "turns", "phase"):
            self.assertEqual(saved["benchmark"][k], live["benchmark"][k], k)
        # a save is scored from its last per-turn stats row (a running game from the score as it stands)
        last = game.game.stats(1)[0]["players"]
        self.assertEqual(saved["benchmark"]["score"], last["0"]["score"])
        self.assertEqual(saved["benchmark"]["best_bot_score"], max(v["score"] for k, v in last.items() if k != "0"))
        self.assertTrue(0 <= saved["benchmark"]["performance"] <= 100)

    def test_normalize_fills_defaults(self):
        s = normalize_suite({"servers": [{"server_id": "sv_x", "models": ["m_a", {"model_id": "m_b", "enabled": False}]},
                                         {"name": "old inline server", "models": ["c"]}]})
        self.assertEqual(len(s["servers"]), 1, "pre-registry inline servers are dropped")
        self.assertEqual([m["model_id"] for m in s["servers"][0]["models"]], ["m_a", "m_b"])
        self.assertEqual(len(s["scenarios"]), 1)
        self.assertEqual(s["scenarios"][0]["turn_limit"], 330)      # UnCiv Quick speed


if __name__ == "__main__":
    unittest.main()
