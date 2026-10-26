"""Hybrid benchmarks: hybrid jobs and their control games, the diplomacy recorder, and the benchmark scheduler's rules
for hybrid jobs (design/phase3-features.md P3.6.9, P3.6.10; package 3-16's gates 1, 2 and 4).

- ``Suite`` (gate 1): a hybrid suite (one scenario, the dry-run diplomat, 3 seeds, 60 turns) runs 3 hybrid jobs and 3
  control games; a second model's run of the suite reuses the 3 cached controls; a control game run twice gives the
  same event list.
- ``Recorder`` (gate 2): the recorder on a scripted game equals a hand-made table, an accepted deal keeping the value of
  its proposal as made; in a control game the deals bots made and settled inside one drive are valued; a pair whose
  hybrid seat has every category the bot's gives a zero score difference and equal deal values; a hybrid seat's rows
  survive a save and a load.
- ``Scheduling`` and ``BenchmarkQueue`` (gate 4): a hybrid job is not started on a machine with a live benchmark lease,
  a full job not beside a running hybrid job of the same scheduler (nor a hybrid job beside a full one); in fake time a
  hybrid job's chat answer waiting 30 minutes behind a full benchmark is answered by the model, not the fallback.

The fix round's: a control whose record does not read is played again, a job that cannot pair lets its control go, a
control plays the bots pinned when its hybrid game started, a record that fails stops once and pairs nothing, the mode
rule by the machine a job really plays on, a hybrid job's model loaded beside the machine's work, a chat whose model
never started is not the model's timeout, and the reports' Benchmarks section showing a hybrid job's pair.
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and registry; must be imported before citar)

import copy
import json
import shutil
import tempfile
import threading
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

from citar import db, engine_api, servers as REG
from citar.agents import hybrid, responders
from citar.agents.hybrid_agent import valuation_bot
from citar.agents.providers.base import StepResult, ToolCall
from citar.bots import profiles
from citar.db.models import Base
from citar.diplomacy_stats import DiplomacyRecorder, fallbacks_of, final_of, pair_line, summarize
from citar.pool import scheduler as S
from citar.server import controls
from citar.server.benchmarks import BenchmarkScheduler, normalize_suite
from citar.server.session import SAVE_DIR, SessionManager
from tests import has_test_ops
from tests.chatfake import use
from tests.test_hybrid import Fake, neighbours

_URL = "sqlite:///" + (Path(tempfile.mkdtemp(prefix="citar_hybrid_bench_")) / "test.db").as_posix()


def setUpModule():
    """This module's own database, with the test registry's machines (``sv_gpu``: one slot)."""
    db.dispose()
    db.configure(_URL)
    Base.metadata.drop_all(db.engine())
    db.create_all()
    REG.invalidate()


def until(fn, timeout: float = 30.0, step: float = 0.02, sch=None):
    """Poll ``fn`` (ticking ``sch`` first) until it returns something true, or ``timeout`` passes."""
    deadline = time.monotonic() + timeout
    while True:
        if sch is not None:
            sch.tick()
        value = fn()
        if value or time.monotonic() > deadline:
            return value
        time.sleep(step)


def machine(name: str, slots: int = 3, delay: float = 0.0) -> dict:
    """A dry-run machine of its own, with two models."""
    return REG.upsert({"id": f"sv_{name}", "name": f"Dry {name}", "kind": "test",
                       "connection": {"provider": "dryrun", "max_parallel": slots, "dry_run_delay": delay},
                       "models": [{"id": f"m_{name}_a", "key": "dry-a"}, {"id": f"m_{name}_b", "key": "dry-b"}]})


def scenario(mode: str = "hybrid", **more) -> dict:
    """A small pangaea with three bots: civilizations meet, so there is diplomacy to record."""
    sc = {"name": f"Small pangaea ({mode})", "mode": mode, "map_size": "small", "map_type": "pangaea", "seed": 21,
          "opponents": 3, "turn_limit": 60, "barbarians": "off", "city_states": 2, "max_turn_minutes": 5}
    if mode == "hybrid":
        sc["hybrid"] = {"owners": "all_but_war", "prompt_every": 5}
    sc.update(more)
    return sc


def suite(sv: dict, model: str, scenarios: list, *, repeats: int = 1, mode: str = "parallel") -> dict:
    return {"name": "Hybrid suite", "mode": mode, "repeats": repeats,
            "servers": [{"server_id": sv["id"], "models": [{"model_id": model}]}], "scenarios": scenarios}


class Case(unittest.TestCase):
    def setUp(self):
        S.reset()
        self.addCleanup(S.reset)
        responders.reset()
        self.addCleanup(responders.reset)
        self.manager = SessionManager()
        self.dir = Path(tempfile.mkdtemp(prefix="citar-hybrid-bench-"))
        self.schedulers: list = []

    def tearDown(self):
        games = set(self.manager.sessions)
        for sid in list(self.manager.sessions):
            self.manager.delete(sid)
        for sch in self.schedulers:
            sch.stop()
            games.update(j["game_id"] for r in sch.runs.values() for j in r["jobs"] if j.get("game_id"))
        for gid in games:
            shutil.rmtree(SAVE_DIR / gid, ignore_errors=True)
        shutil.rmtree(self.dir, ignore_errors=True)

    def scheduler(self) -> BenchmarkScheduler:
        sch = BenchmarkScheduler(self.manager, self.dir, autostart=False)
        self.schedulers.append(sch)
        return sch

    def finished(self, sch, run, timeout: float = 120.0) -> bool:
        """The run is done and each of its hybrid jobs has its pair line."""
        return bool(until(lambda: run["status"] == "done" and all(
            (j.get("result") or {}).get("pair") for j in run["jobs"] if j.get("mode") == "hybrid"), timeout, 0.05,
            sch=sch))


# ---------------------------------------------------------------------------- the suite (gate 1)
class Suite(Case):
    def test_normalize_keeps_a_hybrid_scenario_and_refuses_bad_settings(self):
        s = normalize_suite({"scenarios": [scenario(), scenario("full"), {"name": "plain"}]})
        h, f, plain = s["scenarios"]
        self.assertEqual((h["mode"], h["hybrid"]), ("hybrid", {"owners": "all_but_war", "bot_profile": None,
                                                               "handicap": "ai", "prompt_every": 5,
                                                               "phase_seconds": 180.0}))
        self.assertEqual((f["mode"], "hybrid" in f), ("full", False))
        self.assertEqual(plain["mode"], "full")
        grid = normalize_suite({"scenarios": [scenario(hybrid={"owners": {"trades": "llm", "un": "llm"},
                                                               "handicap": "human", "bot_profile": "aggressive"})]})
        self.assertEqual(grid["scenarios"][0]["hybrid"]["owners"], hybrid.owners_map({"trades": "llm", "un": "llm"}))
        self.assertEqual(grid["scenarios"][0]["hybrid"]["handicap"], "human")
        for bad, says in (({"owners": {}}, "nothing for its model"), ({"owners": "most"}, "Unknown preset"),
                          ({"handicap": "robot"}, "handicap"), ({"prompt_every": 0}, "prompt_every"),
                          ({"bot_profile": 3}, "bot_profile"), ("everything", "an object")):
            with self.subTest(bad=bad):
                with self.assertRaises(ValueError) as e:
                    normalize_suite({"scenarios": [scenario(hybrid=bad)]})
                self.assertIn(says, str(e.exception))
                self.assertIn("Small pangaea (hybrid)", str(e.exception))

    def test_three_jobs_three_controls_and_a_second_models_run_reuses_them(self):
        sv = machine("suite")
        sch = self.scheduler()
        run_a = sch.create_run(suite(sv, "m_suite_a", [scenario()], repeats=3))
        self.assertTrue(self.finished(sch, run_a), [(j["status"], j.get("error"), j.get("control"))
                                                    for j in run_a["jobs"]])
        self.assertEqual([j["status"] for j in run_a["jobs"]], ["done"] * 3)
        self.assertEqual(sorted(j["seed"] for j in run_a["jobs"]), [21, 22, 23])
        keys = [j["control"]["id"] for j in run_a["jobs"]]
        self.assertEqual(len(set(keys)), 3, "one control per seed")
        self.assertEqual(sch.controls.played, 3)
        for job in run_a["jobs"]:
            res = job["result"]
            self.assertEqual(job["control"]["status"], "done")
            self.assertTrue(res["diplomacy"]["rows"], "the hybrid seat's diplomacy was recorded")
            self.assertEqual(res["pair"]["score_diff"]["end"],
                             res["diplomacy"]["summary"]["final"]["score"]
                             - sch.controls.get(job["control"]["id"])["summary"]["final"]["score"])
            self.assertIn("model", res["fallbacks"])
            game = self.manager.get(job["game_id"])
            if game is not None:
                self.assertEqual(game.seats[0].type, "hybrid")
                self.assertFalse(game.seats[0].hybrid["advisor"], "the advisor is off in a benchmark")
        # the jobs' model seats made deals the bot valued, and the model acted in its categories
        self.assertTrue(any(j["result"]["pair"]["deals"] for j in run_a["jobs"]))
        # the runs' list carries each job's pair line and record summary, not the record's rows (the run's file does)
        listed = next(r for r in sch.list_runs() if r["id"] == run_a["id"])
        for job, shown in zip(run_a["jobs"], listed["jobs"]):
            self.assertEqual(shown["result"]["pair"], job["result"]["pair"])
            self.assertEqual(shown["result"]["diplomacy"], {"summary": job["result"]["diplomacy"]["summary"]})
            self.assertTrue(job["result"]["diplomacy"]["rows"], "the run itself keeps them")
        self.assertEqual(listed["scenarios"][run_a["jobs"][0]["scenario_id"]]["mode"], "hybrid")
        run_b = sch.create_run(suite(sv, "m_suite_b", [scenario()], repeats=3))
        self.assertTrue(self.finished(sch, run_b))
        self.assertEqual([j["control"]["id"] for j in run_b["jobs"]], keys, "the second model's jobs share them")
        self.assertEqual(sch.controls.played, 3, "the cached controls were reused, none played again")
        pairs = sch.pairs()
        self.assertEqual(sorted(p["model"] for p in pairs), ["dry-a"] * 3 + ["dry-b"] * 3)
        self.assertEqual({p["owners"] for p in pairs}, {"all_but_war"})
        # a control game played twice from its spec gives the same events
        spec = json.loads((sch.controls.dir / f"{keys[0]}.spec.json").read_text(encoding="utf-8"))
        first, second = [], []
        a = controls.run_control(spec, events=first)
        b = controls.run_control(spec, events=second)
        self.assertGreater(len(first), 300)
        self.assertEqual(first, second)
        self.assertEqual((a["events_sha256"], a["rows"]), (b["events_sha256"], b["rows"]))
        self.assertEqual(a["events_sha256"], sch.controls.get(keys[0])["events_sha256"])

    def test_hybrid_jobs_stay_out_of_the_model_scores(self):
        from citar.server.scoring import _reports
        sv = machine("scores")
        sch = self.scheduler()
        run = sch.create_run(suite(sv, "m_scores_a", [scenario(turn_limit=8)]))
        self.assertTrue(self.finished(sch, run))
        gid = run["jobs"][0]["game_id"]
        self.assertIsNone(_reports(self.manager, {gid}).get(gid, {}).get("benchmark"))

    def test_a_record_that_cannot_be_read_leaves_the_job_done_without_a_pair(self):
        sv = machine("unread")
        sch = self.scheduler()
        with mock.patch.object(BenchmarkScheduler, "_hybrid_result", side_effect=RuntimeError("record lost")):
            run = sch.create_run(suite(sv, "m_unread_a", [scenario(turn_limit=6)]))
            self.assertTrue(until(lambda: run["status"] == "done", 60, 0.05, sch=sch), run["jobs"][0])
        job = run["jobs"][0]
        self.assertEqual(job["status"], "done")
        self.assertNotIn("pair", job["result"])
        self.assertTrue(any("record lost" in x["msg"] for x in sch.log), sch.log[-3:])
        for _ in range(3):
            sch.tick()              # no tick fails on it again
        self.assertNotIn("pair", job["result"])

    def test_a_restart_asks_for_a_control_that_was_not_played(self):
        sv = machine("restart")
        sch = self.scheduler()
        with mock.patch.object(controls.ControlGames, "_loop", lambda self: None):
            run = sch.create_run(suite(sv, "m_restart_a", [scenario(turn_limit=8)]))
            self.assertTrue(until(lambda: run["status"] == "done", 60, 0.05, sch=sch))
        key = run["jobs"][0]["control"]["id"]
        self.assertIsNone(sch.controls.get(key))
        sch.stop()
        again = BenchmarkScheduler(self.manager, self.dir, autostart=False)
        self.schedulers.append(again)
        job = again.runs[run["id"]]["jobs"][0]
        self.assertTrue(until(lambda: (job.get("result") or {}).get("pair"), 60, 0.05, sch=again))
        self.assertEqual(job["control"], {"id": key, "status": "done"})

    def test_a_control_whose_record_does_not_read_is_played_again(self):
        """A record of another layout, or a damaged one, is no record: its status is not ``done`` (the job had shown
        "control game played" for ever with no pair line), so the job asks for it again and its pair follows."""
        sv = machine("unreadable")
        sch = self.scheduler()
        with mock.patch.object(controls.ControlGames, "_loop", lambda self: None):
            run = sch.create_run(suite(sv, "m_unreadable_a", [scenario(turn_limit=8)]))
            self.assertTrue(until(lambda: run["status"] == "done", 60, 0.05, sch=sch))
        key = run["jobs"][0]["control"]["id"]
        sch.stop()
        path = sch.controls.path(key)
        for text in ("{not json", json.dumps({"format": controls.FORMAT + 1, "summary": {}})):
            with self.subTest(record=text):
                path.write_text(text, encoding="utf-8")
                fresh = controls.ControlGames(sch.controls.dir)
                self.assertIsNone(fresh.get(key))
                self.assertEqual(fresh.status(key), {"status": "missing"})
        again = BenchmarkScheduler(self.manager, self.dir, autostart=False)
        self.schedulers.append(again)
        job = again.runs[run["id"]]["jobs"][0]
        self.assertTrue(until(lambda: (job.get("result") or {}).get("pair"), 60, 0.05, sch=again), job["control"])
        self.assertEqual(job["control"], {"id": key, "status": "done"})
        self.assertEqual(again.controls.played, 1, "played again, its record written over the one that did not read")
        self.assertEqual(again.controls.get(key)["format"], controls.FORMAT)

    def test_a_job_that_cannot_pair_lets_its_control_go(self):
        """A skipped job's control still queued is dropped rather than played for nobody, a job that can no longer pair
        is not followed at each tick, and after a restart nobody asks for its control again."""
        sv = machine("letgo", slots=1, delay=0.05)
        sch = self.scheduler()
        with mock.patch.object(controls.ControlGames, "_loop", lambda self: None):
            run = sch.create_run(suite(sv, "m_letgo_a", [scenario(turn_limit=400)]))
            job = run["jobs"][0]
            self.assertTrue(until(lambda: (job.get("control") or {}).get("id"), 30, 0.05, sch=sch), job)
            key = job["control"]["id"]
            sch.tick()
            self.assertEqual(sch.controls._queue, [key], "wanted while its job plays")
            sch.control_job(run["id"], job["id"], "skip")
            self.assertEqual(job["status"], "cancelled")
            self.assertEqual(sch.controls._queue, [], "the skipped job's control is not played")
            with mock.patch.object(sch.controls, "status", wraps=sch.controls.status) as status:
                for _ in range(3):
                    sch.tick()
            self.assertEqual(status.call_count, 0, "a job that cannot pair is not followed")
        sch.stop()
        again = BenchmarkScheduler(self.manager, self.dir, autostart=False)
        self.schedulers.append(again)
        for _ in range(3):
            again.tick()
        self.assertEqual((again.controls._queue, again.controls._running, again.controls.played), ([], None, 0),
                         "nobody asks for it after a restart")


# ---------------------------------------------------------------------------- the recorder (gate 2)
class Recorder(Case):
    @unittest.skipUnless(has_test_ops(), "needs the engine's test operations (a test-ops build)")
    def test_the_recorder_on_a_scripted_game_equals_a_hand_made_table(self):
        g = engine_api.EngineGame.new({"map_size": "small", "seed": 5, "barbarians": "off", "city_states": 0,
                                       "turn_limit": 50, "players": [{"controller": "human"}] * 4})
        g.apply_ops([{"op": "meet", "a": a, "b": b} for a in range(4) for b in range(a + 1, 4)]
                    + [{"op": "set_player", "player": "all", "gold": 600}])
        bot = valuation_bot({})
        rec = DiplomacyRecorder(0, bot)
        g.subscribe(rec.on_event)
        value = {}

        def worth(key, other, give, receive):
            value[key] = round(g.bot_evaluate(0, bot, other, give, receive), 2)

        gold100, gold200, mp = [{"type": "gold", "amount": 100}], [{"type": "gold", "amount": 200}], [{"type": "share_map"}]
        # 1: we offer Korea 100 gold for its map; it counters (its map for 200); we accept its counter
        n1 = g.execute(0, "open_negotiation", {"to": 1, "message": "map?", "give": gold100, "receive": mp})
        n1 = n1["negotiation_id"]
        worth("open1", 1, gold100, mp)
        rec.observe(g)
        g.execute(1, "respond_negotiation", {"negotiation_id": n1, "action": "counter", "message": "more",
                                             "give": mp, "receive": gold200})
        worth("counter1", 1, gold200, mp)
        rec.observe(g)
        real = g.bot_evaluate
        g.bot_evaluate = mock.Mock(return_value=9999.0)        # the game moved on: nothing is valued again
        g.execute(0, "respond_negotiation", {"negotiation_id": n1, "action": "accept", "message": "ok"})
        rec.observe(g)
        self.assertEqual(g.bot_evaluate.call_count, 0, "an accept keeps the value of the proposal it accepted")
        g.bot_evaluate = real
        # 2: the Celts offer friendship; we decline
        dof = [{"type": "declaration_of_friendship"}]
        n2 = g.open_negotiation_as(2, 0, "friends?", dof, dof)["negotiation_id"]
        worth("open2", 2, dof, dof)
        rec.observe(g)
        g.execute(0, "respond_negotiation", {"negotiation_id": n2, "action": "reject", "message": "no"})
        rec.observe(g)
        # 3: we offer the fourth 50 gold for its map; it accepts
        gold50 = [{"type": "gold", "amount": 50}]
        n3 = g.execute(0, "open_negotiation", {"to": 3, "message": "your map?", "give": gold50, "receive": mp})
        n3 = n3["negotiation_id"]
        worth("open3", 3, gold50, mp)
        rec.observe(g)
        g.execute(3, "respond_negotiation", {"negotiation_id": n3, "action": "accept", "message": "yes"})
        rec.observe(g)
        # 4: we declare war on the Celts and take their only city; Korea declares war on us, then offers peace
        g.execute(0, "declare_war", {"player_id": 2, "message": "war"})
        settler = next(u for u in g.execute(2, "get_units", {}) if str(u["type"]).lower() == "settler")
        city = g.apply_ops([{"op": "found_city", "player": 2, "x": settler["x"], "y": settler["y"],
                             "name": "Target", "pop": 1}])[0]["city_id"]
        for x, y in neighbours(settler["x"], settler["y"]):
            try:
                g.apply_ops([{"op": "add_unit", "player": 0, "unit": "Swordsman", "x": x, "y": y, "count": 12}])
                break
            except engine_api.ActionError:
                continue
        for uid in [u["id"] for u in g.execute(0, "get_units", {}) if u["type"] == "Swordsman"]:
            g.test_ops([{"op": "attack_as", "unit": uid, "x": settler["x"], "y": settler["y"]}])
            if any(c["id"] == city for c in g.execute(0, "get_cities", {})):
                break
        rec.observe(g)
        g.test_ops([{"op": "force_turn", "player": 1}])
        g.execute(1, "declare_war", {"player_id": 0, "message": "war on you"})
        peace = [{"type": "peace_treaty"}]
        n4 = g.execute(1, "open_negotiation", {"to": 0, "message": "peace?", "give": peace, "receive": peace})
        n4 = n4["negotiation_id"]
        worth("open4", 1, peace, peace)
        rec.observe(g)
        g.execute(0, "respond_negotiation", {"negotiation_id": n4, "action": "accept", "message": "ok"})
        rec.observe(g)
        entries = [(r["nid"], r["i"], r["by"], r["with"], r["action"], r["proposal"], r["value"])
                   for r in rec.mine() if r["kind"] == "entry"]
        self.assertEqual(entries, [
            (n1, 1, 0, 1, "open", True, value["open1"]),
            (n1, 2, 1, 1, "counter", True, value["counter1"]),
            (n1, 3, 0, 1, "accept", False, value["counter1"]),
            (n2, 1, 2, 2, "open", True, value["open2"]),
            (n2, 2, 0, 2, "reject", False, None),
            (n3, 1, 0, 3, "open", True, value["open3"]),
            (n3, 2, 3, 3, "accept", False, value["open3"]),
            (n4, 1, 1, 1, "open", True, value["open4"]),
            (n4, 2, 0, 1, "accept", False, value["open4"])])
        events = [(r["kind"], r["with"]) for r in rec.mine() if r["kind"] != "entry"]
        self.assertEqual(events, [("war", 2), ("eliminated", 2), ("capture", 2), ("war", 1), ("peace", 1)])
        summary = summarize(rec.rows, 0, final_of(g, 0))
        deals = [value["counter1"], value["open3"], value["open4"]]
        self.assertEqual({k: summary[k] for k in ("proposals_made", "proposals_received", "accepted_own",
                                                  "acceptance_rate", "deals", "deals_valued", "deal_value_total",
                                                  "deal_value_mean", "wars_declared", "wars_declared_on", "captures",
                                                  "losses", "peace")},
                         {"proposals_made": 2, "proposals_received": 3, "accepted_own": 1, "acceptance_rate": 0.5,
                          "deals": 3, "deals_valued": 3, "deal_value_total": round(sum(deals), 2),
                          "deal_value_mean": round(sum(deals) / 3, 2), "wars_declared": 1, "wars_declared_on": 1,
                          "captures": 1, "losses": 0, "peace": 1})
        self.assertEqual([(d["nid"], d["proposed_by"], d["value"]) for d in summary["deal_list"]],
                         [(n1, "them", value["counter1"]), (n3, "us", value["open3"]), (n4, "them", value["open4"])])
        self.assertEqual([(w["with"], w["by"], w["outcome"], w["captured"], w["lost"]) for w in summary["wars"]],
                         [(2, "us", "eliminated them", 1, 0), (1, "them", "peace", 0, 0)])
        self.assertEqual((summary["final"]["alive"], summary["final"]["won"]), (True, False))

    def test_a_controls_seat_keeps_the_hybrid_seats_handicap_and_level_and_a_bot_seats_decisions(self):
        """P3.6.9: the control copies the hybrid seat's handicap and difficulty level; its automatic decisions are a bot
        seat's (the engine votes and annexes for it), since those are what the model takes over in the other arm."""
        seats = [{"type": "hybrid", "llm": {"provider": "dryrun"}, "handicap": "human", "difficulty": "King",
                  "bot": {"profile": "standard", "aggression": 0.7}, "hybrid": {"owners": "everything"}},
                 {"type": "bot", "bot": {"aggression": 0.2}}]
        s = self.manager.create({"map_size": "duel", "seed": 4, "difficulty": "Emperor"}, seats, track=False,
                                start=False)
        spec = controls.control_spec(s.engine_config, s.seats, 0)
        me, other = spec["config"]["players"]
        hybrid_auto = s.engine_config["players"][0]["auto"]
        self.assertEqual(hybrid_auto, {"un_vote": False, "conquest": False, "free_picks": True}, "the hybrid arm's")
        self.assertEqual((me["controller"], me["handicap"], me["difficulty"], me["auto"]),
                         ("hybrid", "human", "King", {"un_vote": True, "conquest": True, "free_picks": True}))
        self.assertEqual(other, s.engine_config["players"][1], "every other seat as the hybrid game's")
        self.assertEqual(spec["config"]["difficulty"], "Emperor")
        # each seat's bot pinned as the game plays it (the seat with no profile plays the standard bot)
        self.assertEqual({k: (v["profile"], v["aggression"]) for k, v in spec["bots"].items()},
                         {"0": ("standard", 0.7), "1": ("standard", 0.2)})
        self.assertEqual({v["engine"] for v in spec["bots"].values()}, {profiles.resolve("standard", pin=True)["engine"]})
        g = engine_api.EngineGame.new(spec["config"])
        p0 = g.player(0)
        self.assertEqual((p0["handicap"], p0["difficulty"], p0["auto"]),
                         ("human", "King", {"un_vote": True, "conquest": True, "free_picks": True}))

    def test_a_control_plays_the_bots_pinned_when_its_hybrid_game_started(self):
        """The control may play long after its hybrid game started: a profile edited or deleted meanwhile changes
        neither its id nor what it plays, and a pinned bot this build cannot make fails it rather than play another."""
        p = profiles.save({"name": "Control pin", "engine": "basic", "params": {"war_prep_rate": 2}})
        self.addCleanup(lambda: profiles.exists(p["id"]) and profiles.delete(p["id"]))
        seats = [{"type": "hybrid", "llm": {"provider": "dryrun"}, "bot": {"profile": p["id"], "aggression": 0.6},
                  "hybrid": {"owners": "trade_only"}},
                 {"type": "bot", "bot": {"profile": p["id"], "aggression": 0.3}}]
        s = self.manager.create({"map_size": "duel", "seed": 4, "turn_limit": 6}, seats, track=False, start=False)
        spec = controls.control_spec(s.engine_config, s.seats, 0)
        key = controls.control_key(spec)
        played = {pid: engine_api.bot_fingerprint(b) for pid, b in controls._bots(spec).items()}
        self.assertEqual(played[0], engine_api.bot_fingerprint(s.get_agent(0).fallback_bot),
                         "the hybrid seat's bot as its game plays it")
        self.assertEqual(played[1], engine_api.bot_fingerprint(s.get_agent(1).bot))
        profiles.save({**profiles.get(p["id"]), "params": {"war_prep_rate": 3}}, note="edited meanwhile")
        self.assertNotEqual(engine_api.bot_fingerprint(valuation_bot({"profile": p["id"]})), played[0],
                            "the profile plays otherwise now")
        self.assertEqual(controls.control_key(spec), key)
        self.assertEqual({pid: engine_api.bot_fingerprint(b) for pid, b in controls._bots(spec).items()}, played)
        profiles.delete(p["id"])
        self.assertEqual(controls.control_key(spec), key, "nor once it is deleted")
        record = controls.run_control(spec)
        self.assertEqual(record["phase"], "over")
        for change in ({"engine": "basic-999"}, {"engine": None}):
            bad = copy.deepcopy(spec)
            bad["bots"]["1"].update(change)
            with self.subTest(change=change), self.assertRaises(ValueError):
                controls.run_control(bad)
        older = copy.deepcopy(spec)
        older["bots"]["1"] = {"profile": None, "aggression": 0.3}         # a spec of the layout before the pins
        with self.assertRaises(ValueError) as e:
            controls.run_control(older)
        self.assertIn("older layout", str(e.exception))

    def test_a_record_that_fails_stops_once_and_pairs_nothing(self):
        """An unexpected failure stops the record for the rest of the game (said once, not at every drive, answer and
        tool call after it), marks its rows (a loaded game's recorder stays stopped), and its job pairs nothing, as a
        control whose record failed fails: rows missed or valued late are not the control's measure."""
        s = self._bots_game(turn_limit=12)
        real = DiplomacyRecorder.observe
        looks = {"n": 0}

        def observe(rec, g):
            looks["n"] += 1
            if looks["n"] == 3:
                raise KeyError("an entry the record did not expect")
            return real(rec, g)
        with mock.patch.object(DiplomacyRecorder, "observe", observe):
            s.start()
            self.assertTrue(until(lambda: s.game.phase != "playing", 120, 0.05), (s.game.turn, s.errors[:1]))
        self.assertEqual([e["where"] for e in s.errors], ["diplomacy record"], "said once")
        self.assertEqual(looks["n"], 3, "nothing read after it")
        with s.lock:
            rec = s._recorder(0)
            rows = list(rec.mine())
        self.assertEqual(rec.broken, "KeyError")
        self.assertEqual([r for r in rows if r["kind"] == "broken"], [{"player": 0, "kind": "broken",
                                                                       "turn": rows[-1]["turn"], "error": "KeyError"}])
        self.assertEqual(DiplomacyRecorder(0, None, list(s.metrics.data["diplomacy"])).broken, "KeyError",
                         "a loaded game's record stays stopped")
        s.benchmark = {"llm_player": 0}
        result = BenchmarkScheduler._hybrid_result(s)
        self.assertEqual(result["diplomacy"]["incomplete"], "KeyError")
        job = {"mode": "hybrid", "status": "done", "control": {"id": "ctl_" + "0" * 20, "status": "done"},
               "result": result}
        sch = self.scheduler()
        with mock.patch.object(sch.controls, "get", return_value={"summary": {}}):
            self.assertFalse(sch._pair(job), "no pair line from an incomplete record")
        self.assertFalse(BenchmarkScheduler._awaits_control(job), "nor is its control followed")
        self.assertTrue(BenchmarkScheduler._awaits_control(
            {**job, "result": {"diplomacy": {"rows": [], "summary": {}}}}), "a whole one's is")

    def test_a_control_values_the_deals_bots_make_and_settle_inside_one_drive(self):
        spec = self._spec_of(self._bots_game(turn_limit=40), ops=self.OPS)
        made_in: dict = {}
        real = DiplomacyRecorder.observe
        calls = {"n": 0}

        def observe(rec, g):
            calls["n"] += 1
            before = len(rec.rows)
            real(rec, g)
            for row in rec.rows[before:]:
                if row["kind"] == "entry":
                    made_in.setdefault(row["nid"], set()).add(calls["n"])
        with mock.patch.object(DiplomacyRecorder, "observe", observe):
            record = controls.run_control(spec)
        inside = [nid for nid, seen in made_in.items() if len(seen) == 1]
        rows = [r for r in record["rows"] if r["kind"] == "entry" and r["nid"] in inside]
        self.assertTrue(inside, "negotiations opened and settled inside one drive")
        deals = [r for r in rows if r["action"] == "accept"]
        self.assertTrue(deals, "some of them deals")
        self.assertTrue(all(r["value"] is not None for r in rows if r["proposal"] or r["action"] == "accept"),
                        [r for r in rows if r["value"] is None])
        self.assertEqual(record["summary"]["deals_valued"], record["summary"]["deals"])

    OPS = [{"op": "meet", "a": 0, "b": "all"}, {"op": "set_player", "player": "all", "gold": 600}]

    def _bots_game(self, turn_limit: int = 60):
        """A hybrid seat (made test-only: every category its bot's) and three bots, everyone met, not started."""
        seats = [{"type": "hybrid", "llm": {"provider": "dryrun", "dry_run_delay": 0}, "bot": {"profile": "standard"},
                  "hybrid": {"owners": "trade_only", "prompt_every": 5}},
                 {"type": "bot"}, {"type": "bot"}, {"type": "bot"}]
        s = self.manager.create({"map_size": "small", "seed": 11, "barbarians": "off", "city_states": 2,
                                 "difficulty": "Emperor", "turn_limit": turn_limit}, seats, track=False, start=False)
        # the test-only configuration (normalize_hybrid refuses it to anyone else): nothing for the model
        s.seats[0].hybrid = {**s.seats[0].hybrid, "owners": hybrid.owners_map({})}
        s.game.set_controller(0, "hybrid", "ai", {"un_vote": True, "conquest": True, "free_picks": True})
        with s.lock:
            s.game.apply_ops(self.OPS)
        return s

    @staticmethod
    def _spec_of(s, ops=None) -> dict:
        return controls.control_spec(s.engine_config, s.seats, 0, ops)

    def _play_out(self, s, timeout: float = 120.0) -> None:
        s.start()
        self.assertTrue(until(lambda: s.game.phase != "playing", timeout, 0.05), (s.game.turn, s.errors[:1]))
        self.assertEqual(s.errors, [])

    def test_a_pair_whose_hybrid_seat_has_every_category_the_bots_differs_in_nothing(self):
        s = self._bots_game()
        spec = self._spec_of(s, ops=self.OPS)
        self._play_out(s)
        with s.lock:
            rows = s._recorder(0).mine()
            mine = summarize(rows, 0, final_of(s.game, 0))
        record = controls.run_control(spec)
        self.assertGreater(mine["deals"], 3, "the seat made deals")
        self.assertEqual(rows, record["rows"], "the same proposals, valued the same, at the same points")
        line = pair_line(mine, record["summary"])
        self.assertEqual(line["score_diff"], {"end": 0})
        self.assertEqual((line["deal_value_total"], line["deal_value_mean"], line["deals"]),
                         (line["control"]["deal_value_total"], line["control"]["deal_value_mean"],
                          line["control"]["deals"]))

    def test_a_failing_record_never_stops_an_events_own_effects(self):
        s = self._bots_game(turn_limit=10)
        with mock.patch.object(DiplomacyRecorder, "on_event", side_effect=RuntimeError("record broke")), \
                mock.patch.object(DiplomacyRecorder, "observe", side_effect=RuntimeError("record broke")):
            with s.lock:
                s.game.emit("eliminated", "a test elimination", player=2)
                before = (s.game.turn, s.game.current)
                s._after_action(*before)
        self.assertIn(2, s._seats_out, "the elimination still let the seat's lease go")
        self.assertEqual([e["where"] for e in s.errors], ["diplomacy record"], "said once: the record stopped there")
        self.assertEqual(s._recorder(0).broken, "RuntimeError")

    def _people_turns(self, s, rounds: int) -> None:
        """Play the person's seat (3) for ``rounds`` turns: decline whatever waits on it, end its turn, and wait for
        its next one (the bots, the hybrid seat's bot among them, play between); a game held at the person's turn
        stands still, whatever the machine's speed. Returns at the person's turn (or the game's end)."""
        def at_person() -> bool:
            with s.lock:
                for n in s.game.open_negotiation_heads(3):
                    if n["awaiting"] == 3:
                        s.call_tool(3, "respond_negotiation", {"negotiation_id": n["id"], "action": "reject",
                                                               "message": "No."})
                return s.game.current == 3 or s.game.phase != "playing"
        for _ in range(rounds):
            self.assertTrue(until(at_person, 60, 0.01), (s.game.turn, s.game.current, s.errors[:1]))
            if s.game.phase != "playing":
                return
            turn = s.game.turn
            self.assertTrue(until(lambda: s.call_tool(3, "end_turn", {})["ok"], 30), "the person's turn ended")
            self.assertTrue(until(lambda t=turn: s.game.turn > t or s.game.phase != "playing", 60, 0.01))
        self.assertTrue(until(at_person, 60, 0.01), (s.game.turn, s.game.current, s.errors[:1]))

    def test_a_hybrid_seats_rows_survive_a_save_and_a_load(self):
        """A hybrid seat, two bots and a person: the game stands at the person's turn when it is saved, so the rows
        saved are exactly those the recorder held."""
        seats = [{"type": "hybrid", "llm": {"provider": "dryrun", "dry_run_delay": 0}, "bot": {"profile": "standard"},
                  "hybrid": {"owners": "trade_only", "prompt_every": 50}},
                 {"type": "bot"}, {"type": "bot"}, {"type": "human"}]
        s = self.manager.create({"map_size": "small", "seed": 11, "barbarians": "off", "city_states": 2,
                                 "turn_limit": 200}, seats, track=False, start=False)
        s.seats[0].hybrid = {**s.seats[0].hybrid, "owners": hybrid.owners_map({})}     # test-only: no model
        s.game.set_controller(0, "hybrid", "ai", {"un_vote": True, "conquest": True, "free_picks": True})
        with s.lock:
            s.game.apply_ops(self.OPS)
        s.start()
        self._people_turns(s, 15)
        with s.lock:
            self.assertEqual(s.game.current, 3)
            before = list(s._recorder(0).mine())
        self.assertTrue(before)
        path = s.save("mid")
        self.manager.delete(s.id)
        loaded = self.manager.load(path)
        with loaded.lock:
            self.assertEqual(loaded._recorder(0).mine(), before, "the rows came back with the save")
        loaded.resume()
        self._people_turns(loaded, 25)
        with loaded.lock:
            after = loaded._recorder(0).mine()
        self.assertEqual(after[:len(before)], before)
        keys = [(r["nid"], r["i"]) for r in after if r["kind"] == "entry"]
        self.assertEqual(len(keys), len(set(keys)), "no entry recorded twice")
        self.assertGreater(len(after), len(before), "it carried on recording")


# ---------------------------------------------------------------------------- the scheduler's rules (gate 4)
class Scheduling(Case):
    def test_a_hybrid_job_waits_while_a_benchmark_lease_is_live_on_its_machine(self):
        sv = machine("held", slots=3)
        lease = S.get().lease(sv["id"], "dry-a", kind="benchmark", ref=("benchmark", "run_other"),
                              label="benchmark run “someone else's”")
        self.addCleanup(lease.release)
        sch = self.scheduler()
        run = sch.create_run(suite(sv, "m_held_a", [scenario(turn_limit=8)]))
        job = run["jobs"][0]
        for _ in range(5):
            sch.tick()
        self.assertEqual(job["status"], "queued")
        self.assertIn("plays a full benchmark", job.get("waiting") or "")
        self.assertIsNone(job.get("game_id"))
        lease.release()
        self.assertTrue(until(lambda: job["status"] not in ("queued",), 30, 0.05, sch=sch), job)
        self.assertTrue(self.finished(sch, run))

    def _two_modes(self, first: str, second: str):
        sv = machine(f"modes{first[0]}{second[0]}", slots=3, delay=0.05)
        sch = self.scheduler()
        long_one = scenario(first, turn_limit=400, name="long")
        short_one = scenario(second, turn_limit=6, name="short")
        run = sch.create_run(suite(sv, f"m_modes{first[0]}{second[0]}_a", [long_one, short_one]))
        a, b = run["jobs"]
        self.assertTrue(until(lambda: a["status"] in ("running", "loading"), 30, 0.05, sch=sch))
        for _ in range(5):
            sch.tick()
            time.sleep(0.05)
        self.assertEqual(b["status"], "queued", "it waits for the other mode's job")
        self.assertIn(f"plays a {'hybrid' if first == 'hybrid' else 'full'} benchmark", b.get("waiting") or "")
        sch.control_job(run["id"], a["id"], "skip")
        self.assertTrue(until(lambda: b["status"] not in ("queued",), 30, 0.05, sch=sch), b)
        self.assertTrue(until(lambda: b["status"] == "done", 120, 0.05, sch=sch), b)

    def test_a_full_job_is_not_started_beside_a_running_hybrid_job(self):
        self._two_modes("hybrid", "full")

    def test_a_hybrid_job_is_not_started_beside_a_running_full_job(self):
        self._two_modes("full", "hybrid")

    def test_hybrid_jobs_count_against_the_machines_job_limit(self):
        sv = machine("limit", slots=1, delay=0.05)
        sch = self.scheduler()
        run = sch.create_run(suite(sv, "m_limit_a", [scenario(turn_limit=200, name="one"),
                                                     scenario(turn_limit=6, name="two")]))
        a, b = run["jobs"]
        self.assertTrue(until(lambda: a["status"] in ("running", "loading"), 30, 0.05, sch=sch))
        for _ in range(5):
            sch.tick()
        self.assertEqual(b["status"], "queued", "one job at a time on a one-slot machine")
        sch.control_job(run["id"], a["id"], "skip")
        self.assertTrue(until(lambda: b["status"] == "done", 120, 0.05, sch=sch), b)


    def test_the_mode_rule_reads_the_machine_each_job_really_plays_on(self):
        """A machine registered again has a new id, which a job's game seat takes (``_machine``) while its suite still
        names the old one (``server_key``): a full job resuming there after making way still waits beside the hybrid
        job playing on it, and a hybrid job beside a full one."""
        sch = self.scheduler()

        def job(jid, mode, status, reason=None):
            return {"id": jid, "mode": mode, "status": status, "pause_reason": reason, "server_key": "sv_old",
                    "server_id": "sv_old", "machine_id": "sv_new", "game_id": None, "server_name": "The box"}
        hybrid_job, full_job = job("job_h", "hybrid", "running"), job("job_f", "full", "paused", "preempted")
        run = {"id": "run_m", "name": "machines", "jobs": [hybrid_job, full_job]}
        sch.runs[run["id"]] = run
        self.addCleanup(sch.runs.pop, run["id"], None)
        why = sch._mode_wait(run, full_job, sch._machine(full_job), [])
        self.assertIsNotNone(why, "the full job does not resume beside the hybrid one")
        self.assertIn("plays a hybrid benchmark", why["prefix"])
        hybrid_job["status"], full_job["status"], full_job["pause_reason"] = "paused", "running", None
        hybrid_job["pause_reason"] = "preempted"
        self.assertIsNotNone(sch._mode_wait(run, hybrid_job, sch._machine(hybrid_job), []),
                             "nor the hybrid one beside the full one")
        self.assertIsNone(sch._mode_wait(run, hybrid_job, "sv_other", []), "another machine is free of it")

    def test_a_hybrid_job_loads_its_model_beside_the_machine_s_work(self):
        """A hybrid job starts beside lobby games and pooled prompts on its machine, so its model is loaded beside
        theirs (``shared``): nothing is unloaded under them. A full job loads as its profile says."""
        sch = self.scheduler()
        with mock.patch.object(sch, "_server", return_value={}), \
                mock.patch.object(sch, "_model_cfg", return_value={"model": "m"}), \
                mock.patch.object(REG, "get", return_value={"id": "sv"}), \
                mock.patch.object(REG, "model_entry", return_value={}), \
                mock.patch.object(REG, "profile", return_value={"exclusive": True}), \
                mock.patch.object(REG, "ensure_model", return_value=0.0) as ensure:
            sch._ensure_model({}, {"mode": "hybrid", "server_id": "sv"})
            sch._ensure_model({}, {"mode": "full", "server_id": "sv"})
            sch._ensure_model({}, {"server_id": "sv"})
        self.assertEqual([c.kwargs.get("shared") for c in ensure.call_args_list], [True, False, False])

    def test_a_shared_load_unloads_nothing(self):
        from citar.server import lmstudio
        client = mock.Mock()
        sv = {"name": "Box", "connection": {"provider": "lmstudio", "manage_loading": True,
                                            "base_url": "http://localhost:1234/v1"}}
        prof = {"context": 8192, "exclusive": True}
        with mock.patch.object(REG, "_helper_refusal", return_value=None), \
                mock.patch.object(REG, "sdk", return_value=client), \
                mock.patch.object(REG, "lmstudio_devices", return_value={}), \
                mock.patch.object(REG, "forget_lmstudio_state"), \
                mock.patch.object(REG, "sdk_config", return_value={}), \
                mock.patch.object(lmstudio, "lms_path", return_value=None), \
                mock.patch.object(lmstudio, "is_local", return_value=True), \
                mock.patch.object(lmstudio, "models", return_value={}), \
                mock.patch.object(REG, "loaded_state") as state:
            # loaded with a smaller context beside a lobby game's model: a full job reloads it alone
            state.return_value = {"loaded": True, "context": 4096, "others": ["lobby-model"]}
            self.assertEqual(REG.ensure_model(sv, "m", prof, shared=True), 0.0)
            client.llm.unload.assert_not_called()
            client.llm.load_new_instance.assert_not_called()
            REG.ensure_model(sv, "m", prof)
            self.assertEqual(sorted(c.args[0] for c in client.llm.unload.call_args_list), ["lobby-model", "m"])
            client.reset_mock()
            # not loaded: loaded beside the lobby game's model, whatever the profile's "exclusive"
            state.return_value = {"loaded": False, "context": None, "others": ["lobby-model"]}
            REG.ensure_model(sv, "m", prof, shared=True)
            client.llm.unload.assert_not_called()
            self.assertEqual(client.llm.load_new_instance.call_args.args, ("m",))


class BenchmarkQueue(Case):
    """In a hybrid benchmark game the chat clock does not count a chat while its model's answer waits behind a
    benchmark (P3.5.2, P3.6.9): half an hour behind a full benchmark, the model still answers."""

    def setUp(self):
        super().setUp()
        Fake.reset()
        for where in ("citar.agents.llm_agent.make_conversation", "citar.agents.hybrid_agent.make_conversation"):
            patcher = mock.patch(where, Fake)
            patcher.start()
            self.addCleanup(patcher.stop)

    def test_a_hybrid_jobs_chat_answer_waits_out_a_full_benchmark_and_its_model_answers(self):
        s = self.manager.create({"map_size": "small", "seed": 3, "barbarians": "off", "city_states": 0,
                                 "chat_model_reply_seconds": 600},
                                [{"type": "human"},
                                 {"type": "hybrid", "llm": {"server_id": "sv_gpu", "model": "dry-run"},
                                  "hybrid": {"owners": "trade_only"}}], track=False, start=False)
        s.benchmark = {"run_id": "run_h", "job_id": "job_h", "mode": "hybrid", "llm_player": 1}
        with s.lock:
            s.game.apply_ops([{"op": "meet", "a": 0, "b": 1}, {"op": "set_player", "player": "all", "gold": 600}])
        clock = use(s)
        bench = S.get().lease("sv_gpu", "dry-run", kind="benchmark", ref=("benchmark", "run_full"),
                              label="a full benchmark")
        self.addCleanup(bench.release)
        r = s.call_tool(0, "open_negotiation", {"to": 1, "message": "Gold for your map?",
                                                "give": [{"type": "gold", "amount": 50}],
                                                "receive": [{"type": "share_map"}]})
        self.assertTrue(r["ok"], r)
        nid = r["result"]["negotiation_id"]
        self.assertTrue(until(lambda: s.chat.deadline_of(nid).get("held") == "benchmark", 15),
                        "the answer's ticket waits behind the benchmark, and the clock stands still")
        clock.advance(1800)
        with s.lock:
            head = s.game.negotiation_head(nid)
        self.assertEqual((head["status"], head["awaiting"]), ("open", 1), "still the model's to answer")
        self.assertEqual([x for x in s.metrics.data["negotiations"] if x["nid"] == nid], [], "no fallback")
        self.assertEqual([m for m in Fake.made if m["nid"] == nid and m["conv"].steps], [], "not asked yet")
        bench.release()
        self.assertTrue(until(lambda: s.game.negotiation_head(nid)["status"] != "open", 15))
        self.assertEqual(s.game.negotiation(nid)["status"], "accepted")
        self.assertEqual([(x["outcome"], x["cause"]) for x in s.metrics.data["negotiations"] if x["nid"] == nid],
                         [("answered", None)], "answered by the model, not the fallback")
        last = s.game.negotiation(nid)["history"][-1]
        self.assertEqual((last["by"], last["message"]), (1, "model: accept"))

    # a chat whose time runs out: the model's timeout once its prompt started, the machine's before (P3.6.9)
    def _game(self, benchmark: bool = True, machine: str = "sv_gpu"):
        s = self.manager.create({"map_size": "small", "seed": 3, "barbarians": "off", "city_states": 0,
                                 "chat_model_reply_seconds": 600},
                                [{"type": "human"},
                                 {"type": "hybrid", "llm": {"server_id": machine, "model": "dry-run"},
                                  "hybrid": {"owners": "trade_only"}}], track=False, start=False)
        if benchmark:
            s.benchmark = {"run_id": "run_h", "job_id": "job_h", "mode": "hybrid", "llm_player": 1}
        with s.lock:
            s.game.apply_ops([{"op": "meet", "a": 0, "b": 1}, {"op": "set_player", "player": "all", "gold": 600}])
        return s, use(s)

    def _offer(self, s) -> int:
        r = s.call_tool(0, "open_negotiation", {"to": 1, "message": "Gold for your map?",
                                                "give": [{"type": "gold", "amount": 50}],
                                                "receive": [{"type": "share_map"}]})
        self.assertTrue(r["ok"], r)
        return r["result"]["negotiation_id"]

    def _busy(self):
        """Another member's pooled prompt (a report's narrative) running on ``sv_gpu``'s one slot."""
        holder = S.get().ticket("sv_gpu", "dry-run", cls="report", consumer="someone_else",
                                label="a report's narrative")
        grant = holder.acquire()
        self.addCleanup(holder.close)
        self.addCleanup(holder.finish, grant)
        return holder, grant

    @staticmethod
    def _rows(s, nid: int) -> list:
        return [(x["outcome"], x["cause"]) for x in s.metrics.data.get("negotiations") or [] if x["nid"] == nid]

    def _answered(self, s, nid: int) -> bool:
        return bool(until(lambda: self._rows(s, nid), 15))

    def _queued(self, s, nid: int) -> bool:
        def queued():
            c = s.chat.chats.get(nid)
            return c is not None and (c.answering or ("",))[0] == "queued"
        return bool(until(queued, 15))

    def test_a_chat_whose_model_never_started_is_the_machine_s(self):
        """Its answer's ticket queued behind other pooled work (not a benchmark: the clock runs): at the deadline the
        fallback bot answers with the cause ``not_started``, which the leaderboard does not count against the model."""
        s, clock = self._game()
        self._busy()
        nid = self._offer(s)
        self.assertTrue(self._queued(s, nid), "queued behind the report")
        self.assertIsNone(s.chat.deadline_of(nid).get("held"), "not behind a benchmark: the clock runs")
        clock.advance(600)
        self.assertTrue(self._answered(s, nid))
        self.assertEqual(self._rows(s, nid), [("fallback", "not_started")])
        self.assertEqual(s.game.negotiation(nid)["history"][-1]["by"], 1, "the fallback bot answered")
        self.assertEqual([m for m in Fake.made if m["nid"] == nid and m["conv"].steps], [], "the model never asked")
        fb = fallbacks_of(s.metrics.data, 1)
        self.assertEqual((fb["model"], fb["other"]), (0, {"not_started": 1}))

    def test_a_model_silent_once_its_prompt_started_is_the_model_s_timeout(self):
        Fake.mode = "hang"
        s, clock = self._game()
        nid = self._offer(s)
        self.assertTrue(until(lambda: any(m["nid"] == nid and m["conv"].started.is_set() for m in Fake.made), 15))
        clock.advance(600)
        self.assertTrue(self._answered(s, nid))
        self.assertEqual(self._rows(s, nid), [("fallback", "timeout")])
        self.assertEqual(fallbacks_of(s.metrics.data, 1)["model"], 1)

    def test_in_a_lobby_game_the_queue_counts_as_the_model_s_time(self):
        """The owner's rule: a lobby game's model timeout includes its queue, and its cause stays ``timeout``."""
        s, clock = self._game(benchmark=False)
        self._busy()
        nid = self._offer(s)
        self.assertTrue(self._queued(s, nid))
        clock.advance(600)
        self.assertTrue(self._answered(s, nid))
        self.assertEqual(self._rows(s, nid), [("fallback", "timeout")])

    def _phase(self, s, pid: int = 1) -> threading.Thread:
        agent = s.get_agent(pid)
        t = threading.Thread(target=agent._phase, args=(s, pid, ["a test"]), daemon=True)
        t.start()
        self.addCleanup(t.join, 15)
        self.assertTrue(until(lambda: agent.phase_running, 10), "the phase holds the seat's voice")
        return t

    def test_a_chat_held_for_a_phase_that_never_started_is_the_machine_s(self):
        """A chat that comes while the seat's diplomat phase waits for its slot is the phase's (one voice): its time
        that runs out before the phase's prompt starts is the machine's queue, not the model's."""
        s, clock = self._game()
        holder, grant = self._busy()
        t = self._phase(s)
        nid = self._offer(s)
        clock.advance(600)
        self.assertTrue(self._answered(s, nid))
        self.assertEqual(self._rows(s, nid), [("fallback", "not_started")])
        holder.finish(grant)
        holder.close()
        t.join(15)
        self.assertFalse(t.is_alive())

    def test_the_phase_tells_the_clock_only_of_the_chat_as_it_was_told(self):
        """A chat that moved after the phase was told of it is a new state its model has not seen: it does not count as
        started (nor does another seat's word for it)."""
        s, _clock = self._game(benchmark=True)
        with mock.patch.object(responders, "submit"):         # nobody answers it: the test tells the clock itself
            nid = self._offer(s)
        with s.lock:
            entries = s.game.negotiation_head(nid)["entries"]
        s.chat.note()
        c = s.chat.chats[nid]
        for pid, told in ((1, entries - 1), (0, entries)):
            s.chat.model_told(nid, pid, told)
            self.assertFalse(c.started, (pid, told))
        s.chat.model_told(nid, 1, entries)
        self.assertTrue(c.started)

    def test_a_chat_a_running_phase_was_told_of_is_the_model_s(self):
        """A chat the phase's model was told of (a step that ran with it) and left unanswered past its time is the
        model's timeout."""
        s, clock = self._game()
        arrived, third, release = threading.Event(), threading.Event(), threading.Event()

        def phase(conv):
            if conv.steps == 1:
                arrived.wait(10)            # the chat comes while the model thinks
                return StepResult(text="look", tool_calls=[ToolCall("a", "get_diplomacy", {})])
            if conv.steps == 2:             # told of the chat before this step
                return StepResult(text="look again", tool_calls=[ToolCall("b", "get_diplomacy", {})])
            third.set()
            release.wait(10)
            return StepResult(text="done")
        Fake.phase = phase
        t = self._phase(s)
        nid = self._offer(s)
        arrived.set()
        self.assertTrue(third.wait(10))
        conv = next(m["conv"] for m in Fake.made if m["phase"])
        self.assertTrue(any(f"negotiation #{nid}" in text for text in conv.texts), "the phase was told the chat")
        clock.advance(600)
        self.assertTrue(self._answered(s, nid))
        self.assertEqual(self._rows(s, nid), [("fallback", "timeout")])
        release.set()
        t.join(15)
        self.assertFalse(t.is_alive())


# ---------------------------------------------------------------------------- the reports
class Reports(unittest.TestCase):
    """A report's Benchmarks section and its model figures: a hybrid job's game is its bot's play, so the job is shown
    with its pair line, and its seat's performance, wins and turns are not the model's."""

    def test_the_benchmarks_section_shows_a_hybrid_job_s_pair(self):
        from citar.reports import render
        result = {"outcome": "lost", "turn": 61, "turn_limit": 60, "performance": 71.0, "avg_turn_s": 0.4}
        jobs = [{"id": "j1", "model": "dry-a", "label": "dry-a", "server_name": "Box", "scenario_name": "Pangaea",
                 "status": "done", "mode": "hybrid", "owners": "all_but_war",
                 "result": {**result, "pair": {"score_diff": {"end": 12}, "deals": 3}}},
                {"id": "j2", "model": "dry-a", "label": "dry-a", "server_name": "Box", "scenario_name": "Pangaea",
                 "status": "done", "mode": "hybrid", "owners": "custom: trades, talk",
                 "result": {**result, "diplomacy": {"summary": {}, "incomplete": "KeyError"}}},
                {"id": "j3", "model": "dry-b", "label": "dry-b", "server_name": "Box", "scenario_name": "Pangaea",
                 "status": "done", "mode": "full", "result": {**result, "performance": 55.0}}]
        run = {"name": "Hybrid run", "status": "done", "created": time.time(), "jobs": jobs,
               "summary": {"turns_done": 180, "turns_total": 180}}
        html = render.s_benchmarks(SimpleNamespace(data={"benchmark_runs": [run], "acts": []},
                                                   money=lambda v: f"${v:.2f}"))
        self.assertIn("dry-a · hybrid: all but war", html)
        self.assertIn("+12 vs control · 3 deals", html)
        self.assertIn("dry-a · hybrid: custom: trades, talk", html)
        self.assertIn("no pair (record incomplete)", html)
        self.assertNotIn("<td>71</td>", html, "the bot's performance is not shown as the model's")
        self.assertIn("<td>55</td>", html)
        self.assertIn("its control game", html)

    def test_a_hybrid_seat_is_not_in_the_model_figures(self):
        from citar.reports.data import config_rollup

        def seat(cls, perf, outcome):
            return {"class": cls, "progress": {"performance": perf, "outcome": outcome},
                    "metrics": {"turns": 10, "avg_turn_s": 5.0, "error_rate": 0.1, "end_reasons": {"end_turn": 10}}}
        acts = [{"config": "dry-a @ Box", "kind": "benchmark", "total": 1.0, "total_marginal": 1.0, "servers": {},
                 "ref": {}, "game": {"turn": 11, "phase": "ended", "seats": [seat(cls, perf, outcome)]}}
                for cls, perf, outcome in (("full", 40.0, "lost"), ("partial", 90.0, "won"))]
        c = config_rollup({"acts": acts, "probe_runs": [], "servers_by_id": {}})["dry-a @ Box"]
        self.assertEqual((c["perf"], c["wins"], c["model_turns"], c["games"]), ([40.0], 0, 10, 2))


if __name__ == "__main__":
    unittest.main()
