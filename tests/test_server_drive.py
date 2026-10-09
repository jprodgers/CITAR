"""The server on the Rust engine (crates/citar-engine/DESIGN.md P2.7.1; package 2-09's gates 2 to 5).

Bot seats play through ``EngineGame.drive`` and answer through ``EngineGame.answer``, and every drive and answer goes
through the session's side effects: the version, the metrics rows, the responders of the seats a chat waits on, the
broadcasts and the autosave. An internal error of the engine stops the session where it stands. ``/view`` and
``/replay`` return the engine's bytes as they are.
"""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import base64
import gzip
import json
import os
import shutil
import statistics
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

from citar import engine_api
from citar.agents.bot_agent import BotAgent
from citar.server import session as sess
from tests.backends import has_test_ops, rust_only

ROOT = Path(__file__).resolve().parent.parent
#: The engine-format copy of the late fixture (small-continents-normal-s1025/t280), which
#: crates/citar-testkit/tests/engine/server_fixture.rs makes and checks.
LATE = ROOT / "crates" / "citar-testkit" / "testdata" / "server"
#: Settings under which basic-1 offers its friendship to every civilization it has met, on every turn.
EAGER_FRIENDSHIP = {"diplo_every": 1, "friend_chance": 1.0, "friend_chance_aggr": 0.0, "friend_max_ratio": 1000.0}
DRY = {"provider": "dryrun", "model": "dry-run", "dry_run_delay": 0, "dry_run_negotiation": "accept"}
needs_test_ops = unittest.skipUnless(has_test_ops(), "needs the engine's test operations (a test-ops build)")


def wait(cond, timeout: float) -> bool:
    t0 = time.time()
    while time.time() - t0 < timeout:
        if cond():
            return True
        time.sleep(0.02)
    return bool(cond())


class Recorder:
    """A session subscriber that keeps every broadcast, with when it came."""
    def __init__(self):
        self.lock = threading.Lock()
        self.msgs: list = []

    def __call__(self, msg: dict):
        with self.lock:
            self.msgs.append((time.perf_counter(), msg))

    def all(self) -> list:
        with self.lock:
            return list(self.msgs)


def autosave_turn(s) -> int:
    """The turn the session's autosave on disk holds, once its writer has written the saves taken so far (read from its
    header, again if it is being replaced as it is read)."""
    assert s.flush_saves(60), "the writer caught up"
    path = sess.SAVE_DIR / s.id / "autosave.citar"
    for _ in range(40):
        try:
            return engine_api.save_header(path)["summary"]["turn"]
        except (OSError, ValueError):
            time.sleep(0.05)
    raise AssertionError(f"the autosave of {s.id} could not be read")


class ServerCase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # The app reads the accounts database on every request; run on its own, this module would otherwise meet an
        # empty one (the test package has pointed CITAR_DB_URL at a temporary file, as the API modules do).
        from citar import db, settings
        from citar.server import app as appmod
        from fastapi.testclient import TestClient
        settings.reset()
        db.configure(os.environ["CITAR_DB_URL"])
        db.create_all()
        cls.app = appmod
        cls.client = TestClient(appmod.app)

    def setUp(self):
        self.made = []

    def tearDown(self):
        for s in self.made:
            self.app.manager.delete(s.id)
            if s._driver is not None:
                s._driver.join(10)
            shutil.rmtree(sess.SAVE_DIR / s.id, ignore_errors=True)

    def game(self, config: dict, seats: list, start: bool = True) -> "sess.GameSession":
        s = self.app.manager.create(config, seats, name=self.id().rsplit(".", 1)[-1], track=False, start=False)
        self.made.append(s)
        if start:
            s.start()
        return s

    def get(self, s, route: str, token=None):
        return self.client.get(f"/api/games/{s.id}{route}", params={"token": token or s.spectator_token})


@rust_only
class SideEffectTests(ServerCase):
    """Gate 2: what a turn does in the session, bot seats' turns included."""

    def test_an_all_bot_game_has_every_side_effect_of_its_turns(self):
        s = self.game({"map_size": "small", "seed": 5001}, [{"type": "bot"}] * 4, start=False)
        rec = Recorder()
        s.subscribers.append(rec)
        s.start()
        self.assertTrue(wait(lambda: s.game.turn >= 21, 180), f"20 rounds in 3 minutes ({s.game.turn}): {s.errors}")
        # after 20 rounds, while the game plays on, every round's autosave has been taken: once the writer has caught
        # up, the autosave holds the round the game was in, or a later one
        turn = s.game.turn
        self.assertGreaterEqual(autosave_turn(s), turn)
        s.set_paused(True)
        with s.lock:                                   # the turn in progress has finished
            turn = s.game.turn
        self.assertEqual(autosave_turn(s), turn)
        self.assertEqual(s.errors, [])

        # the session version rises every round, with an update per bot turn: a drive a turn, each through _after_action
        updates = [m for _, m in rec.all() if m["type"] == "update"]
        first_update = {}
        for m in updates:
            first_update.setdefault(m["turn"], m["version"])
        rounds = sorted(t for t in first_update if t <= 21)
        self.assertEqual(rounds, list(range(1, 22)))
        for a, b in zip(rounds, rounds[1:]):
            # round 1's first update came after its first seat's drive; every later round's after the last seat's
            self.assertGreaterEqual(first_update[b] - first_update[a], 3 if a == 1 else 4, f"round {a}")
        self.assertGreaterEqual(len([m for m in updates if m["turn"] <= 20]), 4 * 20 - 1)
        self.assertEqual([m["version"] for m in updates], sorted({m["version"] for m in updates}))

        # metrics: a turn row per bot turn, with its end and its actions
        rows = [r for r in s.metrics.data["turns"] if r["turn"] <= 20]
        self.assertEqual(sorted((r["turn"], r["player"]) for r in rows),
                         sorted((t, p) for t in range(1, 21) for p in range(4)))
        for r in rows:
            self.assertEqual((r["controller"], r["end_reason"]), ("bot", "end_turn"), r)
            self.assertEqual(r["tool_calls"], 0, "a bot's actions do not pass through call_tool")
        for p in range(4):
            taken = sum(a[0] for r in rows if r["player"] == p for a in (r.get("bot_actions") or {}).values())
            self.assertGreater(taken, 0, f"player {p}'s actions are counted on its rows")
        report = s.metrics_report()["summary"][0]
        self.assertGreater(report["avg_bot_actions"], 0)
        self.assertIn("unit_action", report["bot_actions"], "basic-1 founds its cities through unit_action")
        csv = self.client.get(f"/api/games/{s.id}/metrics.csv", params={"token": s.spectator_token})
        self.assertEqual(csv.status_code, 200, csv.text)
        self.assertTrue(csv.text.splitlines()[0].endswith(",bot_actions"), csv.text.splitlines()[0])
        self.assertIn("unit_action", csv.text)
        self.assertEqual(self.get(s, "/view").status_code, 200)

    def test_a_chat_a_bot_opens_with_a_model_seat_is_answered_before_the_bot_turn_ends(self):
        s = self.game({"map_size": "duel", "seed": 3, "barbarians": "off"},
                      [{"type": "bot"}, {"type": "llm", "llm": dict(DRY)}], start=False)
        with s.lock:
            s.game.apply_ops([{"op": "meet", "a": 0, "b": 1}])
        s.get_agent(0).bot = engine_api.bot_instance("basic", params=EAGER_FRIENDSHIP)
        rec = Recorder()
        s.subscribers.append(rec)
        responders = []
        real = sess.GameSession._run_responder

        def spy(self_, pid, nid, key):
            responders.append((pid, nid))
            return real(self_, pid, nid, key)

        with mock.patch.object(sess.GameSession, "_run_responder", spy):
            s.start()
            self.assertTrue(wait(lambda: s.game.current != 0 or s.game.turn > 1, 60), s.errors)
            s.set_paused(True)
        n = next(n for n in s.game.negotiations() if n["initiator"] == 0)
        self.assertEqual((n["responder"], n["status"]), (1, "accepted"), n)
        self.assertIn((1, n["id"]), responders, "the model seat's responder was started")
        msgs = rec.all()
        answered = next(t for t, m in msgs if m["type"] == "event" and m["event"]["type"] == "deal")
        ended = next(t for t, m in msgs if m["type"] == "turn" and m["current_player"] != 0)
        self.assertLess(answered, ended, "the bot's turn ended after the model's answer, not before it")
        rec0 = next(r for r in s.metrics.data["turns"] if r["player"] == 0 and r["turn"] == 1)
        self.assertEqual(rec0["end_reason"], "end_turn")
        self.assertIn("open_negotiation", rec0["bot_actions"])
        self.assertLess(rec0["wall_s"], 30, "not the bot's 90-second wait")
        self.assertEqual(s.errors, [])

    def test_a_bot_takes_up_each_answer_as_it_comes_within_one_wait_for_its_turn(self):
        """A chat that comes back to the bot is answered at once, not after its other chats have settled, and the
        turn's waits share one budget (P2.7.1: the agent "drives again when woken")."""
        s = self.game({"map_size": "small", "seed": 11, "barbarians": "off"},
                      [{"type": "bot"}, {"type": "llm", "llm": dict(DRY, dry_run_negotiation="counter")},
                       {"type": "mcp"}, {"type": "human"}], start=False)
        with s.lock:
            s.game.apply_ops([{"op": "meet", "a": 0, "b": 1}, {"op": "meet", "a": 0, "b": 2}])
        s.get_agent(0).bot = engine_api.bot_instance("basic", params=EAGER_FRIENDSHIP)
        closed = []                      # (negotiation, status, how many entries the model seat's chat had by then)
        real = sess.GameSession.close_negotiation

        def spy(self_, nid, status, note, by=None):
            with self_.lock:
                theirs = [n for n in self_.game.negotiations() if n["initiator"] == 0 and n["responder"] == 1]
                closed.append((nid, status, len(theirs[0]["history"]) if theirs else 0))
            return real(self_, nid, status, note, by)

        budget = 3.0
        with mock.patch.object(sess.GameSession, "close_negotiation", spy), \
                mock.patch.object(BotAgent, "REPLY_WAIT_SECONDS", budget):
            t0 = time.perf_counter()
            s.start()
            self.assertTrue(wait(lambda: s.game.current != 0 or s.game.turn > 1, 60), s.errors)
            took = time.perf_counter() - t0
            s.set_paused(True)
        chats = {n["responder"]: n for n in s.game.negotiations() if n["initiator"] == 0}
        self.assertEqual(sorted(chats), [1, 2], "the bot offered its friendship to both seats")
        silent = chats[2]
        self.assertEqual(silent["status"], "expired", silent)
        expiry = next(c for c in closed if c[0] == silent["id"])
        self.assertGreaterEqual(expiry[2], 3, "the model seat's counter was answered before the silent seat's chat "
                                              f"expired: {closed}, {chats[1]['history']}")
        self.assertLess(took, budget + 2.5, "the turn's waits share one budget")
        rec0 = next(r for r in s.metrics.data["turns"] if r["player"] == 0 and r["turn"] == 1)
        self.assertEqual(rec0["end_reason"], "end_turn")
        self.assertEqual(s.errors, [])

    def test_a_humans_proposal_answered_by_a_bot_is_broadcast(self):
        s = self.game({"map_size": "duel", "seed": 3, "barbarians": "off"}, [{"type": "human"}, {"type": "bot"}])
        with s.lock:
            s.game.apply_ops([{"op": "meet", "a": 0, "b": 1}, {"op": "set_player", "player": 0, "gold": 100}])
        rec = Recorder()
        s.subscribers.append(rec)
        r = self.client.post(f"/api/games/{s.id}/tool", params={"token": s.seats[0].token},
                             json={"tool": "open_negotiation",
                                   "args": {"to": 1, "message": "Your map for 10 gold?",
                                            "give": [{"type": "gold", "amount": 10}],
                                            "receive": [{"type": "share_map"}]}})
        self.assertTrue(r.json()["ok"], r.json())
        nid = r.json()["result"]["negotiation_id"]
        self.assertTrue(wait(lambda: s.game.negotiation_head(nid)["awaiting"] != 1, 30), "the bot answered")
        self.assertTrue(wait(lambda: any(m["type"] == "update" and m["version"] >= s.version for _, m in rec.all()),
                             10))
        msgs = [m for _, m in rec.all()]
        events = [i for i, m in enumerate(msgs) if m["type"] == "event" and m["event"]["type"] == "negotiation"]
        self.assertGreaterEqual(len(events), 2, "the proposal, then the bot's answer")
        answer = events[1]
        self.assertTrue(any(m["type"] == "update" for m in msgs[answer + 1:]),
                        "the bot's answer went through _after_action: an update follows it")
        self.assertEqual(s.errors, [])


@rust_only
@needs_test_ops
class CrashTests(ServerCase):
    """Gate 3: an internal error of the engine (the ``panic`` test operation) stops the session where it stands."""

    def _all_bots_paused(self):
        s = self.game({"map_size": "duel", "seed": 7, "barbarians": "off"}, [{"type": "bot"}] * 2)
        self.assertTrue(wait(lambda: s.game.turn >= 3, 60), s.errors)
        s.set_paused(True)
        with s.lock:                               # the turn in progress has finished
            pass
        self.assertTrue(s.flush_saves(30))         # and its autosave is written
        good = (sess.SAVE_DIR / s.id / "autosave.citar").read_bytes()
        return s, good

    def _check_crashed(self, s, good: bytes):
        self.assertIsNotNone(s.crashed)
        self.assertIn("panic", s.crashed["message"])
        self.assertTrue(s.paused)
        self.assertEqual(s.pause_reason["kind"], "crashed")
        s._driver.join(10)
        self.assertFalse(s._driver.is_alive(), "the driver stopped")
        self.assertEqual(s.agents, {}, "the agents were cancelled")
        # the last good autosave is intact, and nothing autosaves over it; the crash save goes through the writer
        folder = sess.SAVE_DIR / s.id
        s.autosave(force=True)
        self.assertTrue(s.flush_saves(30))
        self.assertEqual((folder / "autosave.citar").read_bytes(), good)
        # the crash save is written once, with the crash in it
        crash = folder / f"crash-{s.crashed['turn']:03d}.citar"
        self.assertTrue(crash.exists())
        self.assertEqual(sorted(p.name for p in folder.glob("crash-*.citar")), [crash.name])
        self.assertEqual(engine_api.read_save(crash).session["crashed"]["turn"], s.crashed["turn"])
        # reads still answer
        info = s.info()
        self.assertEqual(info["crashed"]["turn"], s.crashed["turn"])
        summary = self.get(s, "/summary")
        self.assertEqual(summary.status_code, 200, summary.text)
        self.assertEqual(summary.json()["crashed"]["turn"], s.crashed["turn"])
        view = self.get(s, "/view")
        self.assertEqual(view.status_code, 200, view.text)
        self.assertEqual(json.loads(view.content)["session"]["crashed"]["turn"], s.crashed["turn"])
        replay = self.get(s, "/replay")
        self.assertEqual(replay.status_code, 200, replay.text)
        self.assertTrue(json.loads(replay.content)["frames"])
        # nothing plays on, and an agent waiting for its turn is told so rather than waiting for good
        self.assertEqual(s.call_tool(0, "end_turn", {}), {"ok": False, "error": sess.CRASHED})
        for pid in range(len(s.seats)):
            self.assertEqual(s.wait_for_turn(pid, 1), {"status": "crashed", "turn": s.crashed["turn"],
                                                       "message": sess.CRASHED})
        s.set_paused(False)
        s.resume()
        self.assertTrue(s.paused)
        self.assertEqual([e["where"] for e in s.errors], ["engine"])
        # both saves load once the crashed session has let go of its journal: the autosave to play on, the crash save
        # as a crashed session, to read
        s.stop()
        restored = sess.GameSession.from_save(folder / "autosave.citar", read_only=True)
        self.assertIsNone(restored.crashed)
        loaded = sess.GameSession.from_save(crash, read_only=True)
        self.assertTrue(loaded.crashed and loaded.paused)
        self.assertEqual([p for p in range(len(loaded.seats)) if loaded.metrics.current(p)], [],
                         "a crash save loads with no turn in progress")

    def test_a_panic_in_a_tool_call(self):
        s, good = self._all_bots_paused()
        rec = Recorder()
        s.subscribers.append(rec)

        def panics(pid, tool, args=None):
            return s.game.test_ops([{"op": "panic"}])

        with mock.patch.object(s.game, "emit", side_effect=AssertionError("emit on a crashed game")) as emit, \
                mock.patch.object(s.game, "execute", side_effect=panics):
            r = s.call_tool(0, "get_empire", {})
        self.assertEqual(r, {"ok": False, "error": sess.CRASHED})
        emit.assert_not_called()
        self.assertEqual([m["type"] for _, m in rec.all() if m["type"] == "crashed"], ["crashed"])
        self._check_crashed(s, good)

    def test_a_panic_in_a_bot_drive(self):
        s, good = self._all_bots_paused()
        rec = Recorder()
        s.subscribers.append(rec)

        def panics(bots, seat_limit=0):
            return s.game.test_ops([{"op": "panic"}])

        with mock.patch.object(s.game, "emit", side_effect=AssertionError("emit on a crashed game")) as emit, \
                mock.patch.object(s.game, "drive", side_effect=panics):
            s.set_paused(False)
            self.assertTrue(wait(lambda: s.crashed is not None, 30))
            s._driver.join(10)
        emit.assert_not_called()
        self.assertEqual([m["type"] for _, m in rec.all() if m["type"] == "crashed"], ["crashed"])
        self._check_crashed(s, good)

    def test_a_crash_in_a_route_crashes_its_session(self):
        s, good = self._all_bots_paused()
        with mock.patch.object(s.game, "view_json", side_effect=engine_api.EngineCrash("panic: in a view")):
            r = self.get(s, "/view")
        self.assertEqual(r.status_code, 500)
        self.assertEqual(r.json()["detail"], sess.CRASHED)
        self.assertEqual(s.crashed["message"], "panic: in a view")


@rust_only
class ViewBytesTests(ServerCase):
    """Gate 4: /view and /replay return the engine's bytes, never parsed and dumped again."""

    def test_the_view_and_the_replay_are_the_engines_bytes(self):
        s = self.game({"map_size": "duel", "seed": 3, "barbarians": "off"}, [{"type": "human"}, {"type": "bot"}],
                      start=False)
        made = []
        real_view, real_replay = s.game.view_json, s.game.replay_json

        def view_json(*a, **k):
            made.append(real_view(*a, **k))
            return made[-1]

        def replay_json(*a, **k):
            made.append(real_replay(*a, **k))
            return made[-1]

        parsed = []
        real_loads = json.loads

        def loads(s_, *a, **k):
            parsed.append(s_)
            return real_loads(s_, *a, **k)

        with mock.patch.object(s.game, "view_json", side_effect=view_json), \
                mock.patch.object(s.game, "replay_json", side_effect=replay_json), \
                mock.patch("json.loads", side_effect=loads):
            seat = self.get(s, "/view", s.seats[0].token)
            replay = None
            if has_test_ops():                    # the recap of a game somebody plays waits for its end
                with s.lock:
                    s.game.test_ops([{"op": "end_game"}])
                replay = self.get(s, "/replay", s.seats[0].token)
        self.assertEqual(seat.status_code, 200)
        self.assertEqual(seat.headers["content-type"], "application/json")
        self.assertEqual(seat.content, made[0], "exactly view_json's bytes")
        self.assertFalse(any(p is made[0] or p == made[0] for p in parsed), "json.loads never saw the view")
        if replay is not None:
            self.assertEqual(replay.status_code, 200, replay.text)
            self.assertEqual(replay.content, made[1], "exactly replay_json's bytes")
            self.assertFalse(any(p is made[1] or p == made[1] for p in parsed), "json.loads never saw the replay")
        v = real_loads(seat.content)
        self.assertEqual((v["you"], v["seat"]["player"], v["version"]), (0, 0, s.version))
        self.assertEqual(v["session"]["id"], s.id)

    def test_the_late_fixture_god_view_is_fast(self):
        state = json.loads(gzip.decompress((LATE / "late-t280.state.json.gz").read_bytes()))
        chunk = gzip.decompress((LATE / "late-t280.journal.gz").read_bytes())
        g = engine_api.EngineGame.from_save({"state": state, "journal": base64.b64encode(chunk).decode()})
        self.assertEqual(g.turn, 280)
        majors = g.majors(alive_only=False)
        self.assertEqual([p["id"] for p in majors], list(range(len(majors))))
        s = sess.GameSession(g, [sess.Seat(player=p["id"], type="bot") for p in majors], name="late fixture")
        s.paused = True
        self.app.manager.sessions[s.id] = s
        self.made.append(s)
        # The client keeps one event loop for every request, as a TestClient entered with `with` keeps it (but without
        # the app's startup, which would start the benchmark scheduler and restore live games): left to itself it
        # starts a thread and an event loop per request, about 100 ms on Windows, which no server pays.
        import anyio.from_thread
        with anyio.from_thread.start_blocking_portal(**self.client.async_backend) as portal:
            self.client.portal = portal
            try:
                for _ in range(3):                # warm up: the first requests open the database and the routes
                    self.assertEqual(self.get(s, "/view").status_code, 200)
                times = []
                for _ in range(20):
                    t0 = time.perf_counter()
                    r = self.get(s, "/view")
                    times.append(time.perf_counter() - t0)
                    self.assertEqual(r.status_code, 200)
            finally:
                self.client.portal = None
        self.assertGreater(len(r.content), 100_000, "the whole god view of a late game")
        self.assertIn(b'"empires"', r.content)
        median = statistics.median(times)
        print(f"\nthe late fixture's god view through TestClient: median {median * 1000:.1f} ms, "
              f"max {max(times) * 1000:.1f} ms, {len(r.content) / 1e6:.2f} MB", flush=True)
        # The budget is the laptop's (package 2-09's gate 4); a shared CI runner, two to three times slower and
        # never idle, is held to three times it. A route that parses the view again is caught by the bytes test above,
        # not by the clock.
        self.assertLessEqual(median, 0.060 if os.environ.get("CI") else 0.020)


@rust_only
class LongGameTests(ServerCase):
    """Gate 5: four bot seats on a small map play 100 rounds under the session driver, responders active."""

    def test_four_bots_play_a_hundred_rounds_with_no_round_over_ten_seconds(self):
        s = self.game({"map_size": "small", "seed": 5002}, [{"type": "bot"}] * 4, start=False)
        rec = Recorder()
        s.subscribers.append(rec)
        dispatched = []
        real = sess.GameSession._dispatch_negotiation_interrupts

        def dispatch(self_):
            dispatched.append(1)
            return real(self_)

        autosaves = []                  # each autosave's time under the lock: its snapshot (the writer does the rest)
        real_snapshot = s._snapshot

        def snapshot(name, autosave):
            t = time.perf_counter()
            try:
                return real_snapshot(name, autosave)
            finally:
                if autosave:
                    autosaves.append(time.perf_counter() - t)
        s._snapshot = snapshot

        with mock.patch.object(sess.GameSession, "_dispatch_negotiation_interrupts", dispatch):
            t0 = time.perf_counter()
            s.start()
            done = wait(lambda: s.game.turn >= 101 or s.game.phase != "playing", 900)
            s.set_paused(True)
        self.assertTrue(done, f"100 rounds in 15 minutes: turn {s.game.turn}")
        starts = {1: t0}
        for t, m in rec.all():
            if m["type"] == "turn":
                starts.setdefault(m["turn"], t)
        last = max(starts)
        rounds = [starts[r + 1] - starts[r] for r in range(1, min(last, 101))]
        self.assertGreaterEqual(len(rounds), 100 if s.game.phase == "playing" else 1)
        print(f"\n100 rounds of 4 bots on small under the session driver: {starts[min(last, 101)] - t0:.1f} s, "
              f"slowest round {max(rounds):.2f} s, median {statistics.median(rounds) * 1000:.0f} ms; "
              f"{len(autosaves)} autosaves, {sum(autosaves) * 1000:.0f} ms under the lock in all", flush=True)
        self.assertLess(max(rounds), 10.0)
        self.assertGreaterEqual(len(dispatched), 400, "the responders were dispatched after every drive")
        self.assertEqual(s.errors, [])
        self.assertGreaterEqual(len(autosaves), 100, "every round's autosave was taken")
        with s.lock:
            turn = s.game.turn
        self.assertEqual(autosave_turn(s), turn, "once written, the autosave is the round the game is in")


if __name__ == "__main__":
    unittest.main()
