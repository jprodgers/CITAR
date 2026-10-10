"""Saves v2 in the server (crates/citar-engine/DESIGN.md P2.5, P2.5.3; package 2-11's gates 1 to 7).

A session's saves are ``.citar`` containers (a small header, the session's record and metrics, the state) beside the
journal that holds the game's history, one record per save's new chunk. A save is taken under the session's lock (a
snapshot of the state, its new history pending in the journal) and written off it by the session's writer thread:
the chunks appended and synced first, then the container that names them. A session writes one journal, its timeline:
a load continues the save's journal when nothing names more of it and it opens clean, cutting off records no save
names, and forks it otherwise. These tests play bot games under the session driver and check each guarantee.
"""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import gc
import gzip
import json
import shutil
import statistics
import struct
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

from citar import engine_api
from citar.server import session as sess
from tests import has_test_ops

needs_test_ops = unittest.skipUnless(has_test_ops(), "needs the engine's test operations (a test-ops build)")
DUEL = {"map_size": "duel", "seed": 41, "barbarians": "off"}
#: testkit's late server fixture: a game on a small map at turn 280 (test_server_drive's)
LATE = Path(__file__).resolve().parent.parent / "crates" / "citar-testkit" / "testdata" / "server"


def wait(cond, timeout: float) -> bool:
    t0 = time.time()
    while time.time() - t0 < timeout:
        if cond():
            return True
        time.sleep(0.01)
    return bool(cond())


def records(path: Path) -> list[tuple[int, int]]:
    """Each record of a journal file as (its first byte, its payload's first byte) (DESIGN.md P2.5.2)."""
    data = path.read_bytes()
    assert data[:8] == b"CITARJNL", path
    out, at = [], 10
    while at + 18 <= len(data):
        length = struct.unpack_from("<I", data, at)[0]
        out.append((at, at + 18))
        at += 18 + length
    return out


def with_journal(src: Path, dst: Path, name: str):
    """A copy of the save ``src`` at ``dst`` whose header names the journal ``name`` instead, its records and head as
    they were (DESIGN.md P2.5.1: magic, the header's length, the header, the body's frame)."""
    data = src.read_bytes()
    n = struct.unpack_from("<I", data, 8)[0]
    header = json.loads(data[12:12 + n])
    header["journal"]["file"] = name
    raw = json.dumps(header).encode()
    dst.write_bytes(data[:8] + struct.pack("<I", len(raw)) + raw + data[12 + n:])


class SavesCase(unittest.TestCase):
    def setUp(self):
        self.m = sess.SessionManager()
        self.ids = set()

    def tearDown(self):
        for sid in list(self.m.sessions):
            self.m.delete(sid)
        for sid in self.ids:
            shutil.rmtree(sess.SAVE_DIR / sid, ignore_errors=True)

    def game(self, seats: list, config: dict = DUEL) -> "sess.GameSession":
        s = self.m.create(dict(config), seats, name=self.id().rsplit(".", 1)[-1], track=False, start=False)
        self.ids.add(s.id)
        return s

    def play_to(self, s, turn: int, timeout: float = 300):
        """Let the bots play until ``turn``, then pause and let the turn in progress finish."""
        if s._driver is None:
            s.start()
        s.set_paused(False)
        self.assertTrue(wait(lambda: s.game.turn >= turn or s.game.phase != "playing", timeout),
                        f"turn {s.game.turn} of {turn}: {s.errors}")
        self.settle(s)

    def settle(self, s):
        """Pause, let the turn in progress finish, and let the writer write every save taken."""
        s.set_paused(True)
        with s.lock:
            pass
        self.assertTrue(s.flush_saves(60), "the writer caught up")

    def autosave_now(self, s):
        """An autosave of the game as it stands (paused), written."""
        with s.lock:
            s.autosave(force=True)
        self.assertTrue(s.flush_saves(60))

    @staticmethod
    def as_it_is(s) -> dict:
        """What a reloaded game must give back."""
        with s.lock:
            g = s.game
            return {"digest": g._g.digest(), "events": g.events(), "replay": g.replay_data(), "stats": g.stats(),
                    "thoughts": g.thoughts(), "turn": g.turn}

    def header(self, s, name: str = "autosave") -> dict:
        return engine_api.save_header(s.folder / f"{name}.citar")


class AutosaveChainTests(SavesCase):
    """Gate 1: a game autosaved every round reloads from its autosave as it played, history and all."""

    def test_four_bots_autosaved_every_round_for_100_rounds_reload_as_they_played(self):
        s = self.game([{"type": "bot"}] * 4, {"map_size": "small", "seed": 5003})
        taken = []
        real = s._snapshot

        def snapshot(name, autosave):
            if autosave:                         # an autosave is taken with the lock held: this is its turn
                taken.append(s.game.turn)
            return real(name, autosave)
        s._snapshot = snapshot
        self.play_to(s, 101, timeout=600)
        self.assertEqual(s.game.phase, "playing")
        self.assertEqual(set(range(2, s.game.turn + 1)) - set(taken), set(), "every round's autosave was taken")
        self.autosave_now(s)                     # the state as it stands, the round's turns played so far
        self.assertEqual(s.errors, [])
        live = self.as_it_is(s)
        h = self.header(s)
        self.assertEqual(h["summary"]["turn"], live["turn"])
        self.assertEqual(h["journal"]["file"], "journal.cjnl")
        self.assertEqual(h["journal"]["records"], s.journal.records)
        self.assertGreaterEqual(h["journal"]["records"], 100, "a record per round at least")
        with self.assertNoLogs("citar.engine", level="WARNING"):    # the history came back whole
            back = self.m.load(s.folder / "autosave.citar")
        self.assertTrue(s.stopped, "the running session was stopped first")
        self.assertEqual(back.id, s.id)
        got = self.as_it_is(back)
        self.assertEqual(got["digest"], live["digest"])
        self.assertEqual(got["events"], live["events"])
        self.assertEqual(got["replay"], live["replay"])
        self.assertEqual(got["stats"], live["stats"])
        self.assertEqual(back.journal.path.name, "journal.cjnl", "the timeline goes on in its journal")


class TimelineTests(SavesCase):
    """Gate 2: a write stopped before its container, and loading an older save, each keep every save loadable."""

    @needs_test_ops
    def test_a_write_stopped_before_its_container_is_cut_off_when_the_autosave_loads(self):
        s = self.game([{"type": "bot"}] * 2)
        self.play_to(s, 6)
        self.autosave_now(s)
        before = self.header(s)
        live = self.as_it_is(s)
        with s.lock:
            s.game.add_thought(0, "after the last autosave", "note")
        s.journal._hooks(stop_before_container=1)
        self.autosave_now(s)
        self.assertEqual(s.journal.records, before["journal"]["records"] + 1, "its chunk is on the disk")
        self.assertEqual(self.header(s), before, "and no save names it")
        self.assertEqual([e["where"] for e in s.errors], ["autosave"])
        journal = s.folder / "journal.cjnl"
        self.assertGreater(journal.stat().st_size, before["journal"]["bytes"])

        back = self.m.load(s.folder / "autosave.citar")
        self.assertEqual(back.journal.path.name, "journal.cjnl", "continued, not forked")
        self.assertEqual(back.journal.records, before["journal"]["records"], "the extra record is cut off")
        self.assertEqual(journal.stat().st_size, before["journal"]["bytes"])
        self.assertEqual(self.as_it_is(back)["digest"], live["digest"])
        self.assertNotIn("after the last autosave", [t["text"] for t in back.game.thoughts()])
        # and the timeline plays on and loads again
        self.play_to(back, back.game.turn + 3)
        self.autosave_now(back)
        again = self.as_it_is(back)
        with self.assertNoLogs("citar.engine", level="WARNING"):
            reloaded = self.m.load(back.folder / "autosave.citar")
        self.assertEqual(self.as_it_is(reloaded)["digest"], again["digest"])
        self.assertEqual(self.as_it_is(reloaded)["events"], again["events"])

    def test_an_older_save_forks_a_new_timeline_and_both_play_on(self):
        s = self.game([{"type": "bot"}] * 2)
        self.play_to(s, 8)
        early = s.save("turn-early")
        at_early = self.as_it_is(s)
        self.play_to(s, s.game.turn + 6)
        late = s.save("turn-late")
        at_late = self.as_it_is(s)
        self.autosave_now(s)
        self.assertGreater(self.header(s, "turn-late")["journal"]["records"],
                           self.header(s, "turn-early")["journal"]["records"])
        s.stop()                                 # its journal can be read once its session lets go (the load does too)
        old = (s.folder / "journal.cjnl").read_bytes()

        # turn-early: the autosave and turn-late name more of journal.cjnl, so a new timeline forks
        a = self.m.load(early)
        self.assertEqual(a.journal.path.name, "journal-2.cjnl")
        self.assertEqual((s.folder / "journal.cjnl").read_bytes(), old, "the old timeline's journal is untouched")
        self.assertEqual(self.as_it_is(a)["digest"], at_early["digest"])
        self.assertEqual(self.as_it_is(a)["events"], at_early["events"])
        self.play_to(a, at_early["turn"] + 4)
        fork_save = a.save("turn-fork")
        at_fork = self.as_it_is(a)
        self.play_to(a, a.game.turn + 2)
        self.autosave_now(a)
        self.assertEqual(self.header(a)["journal"]["file"], "journal-2.cjnl")
        at_auto = self.as_it_is(a)
        b = self.m.load(a.folder / "autosave.citar")
        self.assertEqual(b.journal.path.name, "journal-2.cjnl", "the new timeline goes on")
        self.assertEqual(self.as_it_is(b)["digest"], at_auto["digest"])
        self.play_to(b, b.game.turn + 2)

        # the old timeline: nothing names more of journal.cjnl now than turn-late, which continues it
        c = self.m.load(late)
        self.assertEqual(c.journal.path.name, "journal.cjnl")
        self.assertEqual(self.as_it_is(c)["digest"], at_late["digest"])
        self.assertEqual(self.as_it_is(c)["events"], at_late["events"])
        self.play_to(c, at_late["turn"] + 3)
        self.autosave_now(c)
        at_c = self.as_it_is(c)
        d = self.m.load(c.folder / "autosave.citar")
        self.assertEqual(self.as_it_is(d)["digest"], at_c["digest"])

        # and the new timeline's named save still loads, on its own journal
        e = self.m.load(fork_save)
        self.assertEqual(e.journal.path.name, "journal-2.cjnl")
        self.assertEqual(self.as_it_is(e)["digest"], at_fork["digest"])
        self.assertEqual(self.as_it_is(e)["events"], at_fork["events"])
        self.assertEqual(sorted(p.name for p in s.folder.glob("*.cjnl")), ["journal-2.cjnl", "journal.cjnl"])


@needs_test_ops
class FailedAppendTests(SavesCase):
    """Gate 3: appends that fail write no container, keep their chunks, and the next save appends them in order."""

    def test_three_failed_appends_then_a_save_with_the_whole_history(self):
        s = self.game([{"type": "bot"}] * 2)
        self.play_to(s, 5)
        good = (s.folder / "autosave.citar").read_bytes()
        s.journal._hooks(fail_appends=3)
        for i in range(3):
            with s.lock:
                s.game.add_thought(0, f"note {i}", "note")    # history, which the next save's chunk carries
            self.autosave_now(s)
            self.assertEqual((s.folder / "autosave.citar").read_bytes(), good, "no container is written")
            self.assertEqual(s.journal.pending, i + 1, "the chunks stay pending")
        self.assertEqual([e["where"] for e in s.errors], ["autosave"] * 3)
        # a named save that fails says so, and leaves nothing
        s.journal._hooks(fail_appends=1)
        with s.lock:
            s.game.add_thought(1, "note 3", "note")
        with self.assertRaises(OSError):
            s.save("named")
        self.assertFalse((s.folder / "named.citar").exists())
        self.assertEqual(s.journal.pending, 4)
        # the next save appends them, in order
        records_before = s.journal.records
        self.autosave_now(s)
        self.assertEqual(s.journal.pending, 0)
        self.assertEqual(s.journal.records, records_before + 4)
        self.assertEqual(self.header(s)["journal"]["records"], s.journal.records)
        live = self.as_it_is(s)
        with self.assertNoLogs("citar.engine", level="WARNING"):       # the whole chronicle
            back = self.m.load(s.folder / "autosave.citar")
        got = self.as_it_is(back)
        self.assertEqual([t["text"] for t in got["thoughts"] if t["text"].startswith("note")],
                         ["note 0", "note 1", "note 2", "note 3"])
        self.assertEqual(got["events"], live["events"])
        self.assertEqual(got["digest"], live["digest"])


class CorruptJournalTests(SavesCase):
    """Gate 4: a damaged record forks the saves before it and refuses the ones past it, the journal left as it is."""

    def test_a_corrupt_record_in_the_middle_of_the_journal(self):
        s = self.game([{"type": "bot"}] * 2)
        self.play_to(s, 6)
        early = s.save("turn-early")
        at_early = self.as_it_is(s)
        self.play_to(s, s.game.turn + 6)
        late = s.save("turn-late")
        self.m.delete(s.id)                       # the game is closed: its journal is free
        folder = s.folder
        e, n = self.header(s, "turn-early")["journal"]["records"], self.header(s, "turn-late")["journal"]["records"]
        self.assertGreater(n - e, 2)
        journal = folder / "journal.cjnl"
        _, payload = records(journal)[(e + n) // 2]
        data = bytearray(journal.read_bytes())
        data[payload + 3] ^= 0x40
        journal.write_bytes(bytes(data))
        damaged = journal.read_bytes()
        # with no later save naming more of it, turn-early would continue journal.cjnl: it opens corrupt, so it forks
        aside = Path(self.id().rsplit(".", 1)[-1] + "-aside")
        aside = sess.SAVE_DIR / aside
        aside.mkdir()
        self.ids.add(aside.name)
        for name in ("turn-late.citar", "autosave.citar"):
            (folder / name).rename(aside / name)
        a = self.m.load(early)
        self.assertEqual(a.journal.path.name, "journal-2.cjnl", "forked from the good prefix")
        self.assertEqual(journal.read_bytes(), damaged, "the corrupt journal is left as it is")
        self.assertEqual(self.as_it_is(a)["digest"], at_early["digest"])
        self.assertEqual(self.as_it_is(a)["events"], at_early["events"])
        self.m.delete(a.id)
        # a save that names records past the damage is refused, saying why
        (aside / "turn-late.citar").rename(late)
        with self.assertRaises(ValueError) as cm:
            self.m.load(late)
        text = str(cm.exception)
        self.assertIn("turn-late.citar's history", text)
        self.assertIn("corrupt", text)
        self.assertIn("does not load", text)
        self.assertEqual(journal.read_bytes(), damaged)
        self.assertIsNone(self.m.get(s.id), "nothing was running, and nothing is")


class OneWriterTests(SavesCase):
    """Gate 5: one session per journal, and stop() waits for the write in flight."""

    def test_a_second_session_on_the_same_journal_is_refused(self):
        s = self.game([{"type": "bot"}] * 2)
        self.play_to(s, 4)
        for open_it in (lambda: sess.GameSession.from_save(s.folder / "autosave.citar"),
                        lambda: sess.GameSession.from_save(s.folder / "autosave.citar", read_only=True),
                        lambda: engine_api.open_journal(s.journal.path)):
            with self.assertRaises(ValueError) as cm:
                open_it()
            self.assertIn("in use by another session", str(cm.exception))
        self.assertTrue(engine_api.journal_in_use(s.journal.path))
        # the manager stops the running game first, so a load of its save takes the journal over
        back = self.m.load(s.folder / "autosave.citar")
        self.assertTrue(s.stopped and s.journal.is_closed)
        self.assertEqual(back.journal.path, s.journal.path)

    def test_a_load_of_a_save_the_stopping_game_writes_loads_what_it_wrote(self):
        s = self.game([{"type": "bot"}] * 2)
        self.play_to(s, 4)
        gate, real = threading.Event(), sess.SaveJob.write

        def held(job):
            gate.wait(10)
            real(job)
        loaded = []
        with mock.patch.object(sess.SaveJob, "write", held):
            with s.lock:
                s.game.add_thought(0, "the newest", "note")
                s.autosave(force=True)           # held: autosave.citar on the disk is still the one before it
            t = threading.Thread(target=lambda: loaded.append(self.m.load(s.folder / "autosave.citar")))
            t.start()
            self.assertTrue(wait(lambda: s.stopped, 10), "the load stops the game, whose writer waits")
            gate.set()
            t.join(60)
        self.assertEqual(len(loaded), 1)
        back = loaded[0]
        self.assertIn("the newest", [x["text"] for x in back.game.thoughts()], "the autosave as the stop left it")
        self.assertEqual(back.journal.path.name, "journal.cjnl", "its timeline goes on: no fork")

    def test_stop_returns_only_after_the_write_in_flight(self):
        s = self.game([{"type": "bot"}] * 2)
        self.play_to(s, 4)
        started, real = threading.Event(), sess.SaveJob.write

        def slow(job):
            started.set()
            time.sleep(0.6)
            real(job)
        with mock.patch.object(sess.SaveJob, "write", slow):
            with s.lock:
                s.game.add_thought(0, "the last word", "note")
                s.autosave(force=True)
            self.assertTrue(started.wait(10))
            t0 = time.perf_counter()
            self.m.delete(s.id)                  # stop(): drains the writer and closes the journal
            took = time.perf_counter() - t0
        self.assertGreater(took, 0.3, "it waited for the write")
        self.assertTrue(s.journal.is_closed)
        self.assertFalse(engine_api.journal_in_use(s.folder / "journal.cjnl"))
        h = self.header(s)
        self.assertEqual(h["journal"]["records"], s.journal.records, "the write in flight is on the disk")
        back = self.m.load(s.folder / "autosave.citar")
        self.assertIn("the last word", [t["text"] for t in back.game.thoughts()])
        with self.assertRaises(RuntimeError):
            s.save("too late")

    def test_queued_autosaves_coalesce_and_a_named_save_waits_for_its_own(self):
        s = self.game([{"type": "bot"}] * 2)
        self.play_to(s, 4)
        written, gate, real = [], threading.Event(), sess.SaveJob.write

        def held(job):
            gate.wait(10)
            written.append(job.path.name)
            real(job)
        with mock.patch.object(sess.SaveJob, "write", held):
            for i in range(4):                  # the first is written; the next three wait behind it
                with s.lock:
                    s.game.add_thought(0, f"round note {i}", "note")
                    s.autosave(force=True)
            named = threading.Thread(target=lambda: written.append(("returned", s.save("named").name)))
            named.start()
            time.sleep(0.2)
            self.assertNotIn(("returned", "named.citar"), written, "a named save waits for its own write")
            gate.set()
            named.join(10)
            self.assertTrue(s.flush_saves(10))
        # autosave 0 (in flight), then the newest queued autosave (1 and 2 were passed over), then the named save
        self.assertEqual(written, ["autosave.citar", "autosave.citar", "named.citar", ("returned", "named.citar")])
        self.assertEqual(self.header(s)["journal"]["records"], s.journal.records, "every chunk appended, in order")
        back = self.m.load(s.folder / "autosave.citar")
        self.assertEqual([t["text"] for t in back.game.thoughts() if t["text"].startswith("round note")],
                         [f"round note {i}" for i in range(4)])


    def test_flushing_waits_for_the_saves_taken_before_it_not_for_the_game_to_stop(self):
        s = self.game([{"type": "bot"}] * 2)
        self.play_to(s, 4)
        real = sess.SaveJob.write

        def slow(job):
            time.sleep(0.15)
            real(job)
        stop = threading.Event()

        def keep_saving():                       # a game that keeps taking autosaves faster than they are written
            i = 0
            while not stop.is_set():
                with s.lock:
                    s.game.add_thought(0, f"note {i}", "note")
                    s.autosave(force=True)
                i += 1
                time.sleep(0.02)
        with mock.patch.object(sess.SaveJob, "write", slow):
            t = threading.Thread(target=keep_saving)
            t.start()
            try:
                time.sleep(0.3)
                t0 = time.perf_counter()
                self.assertTrue(s.flush_saves(10))
                took = time.perf_counter() - t0
            finally:
                stop.set()
                t.join(10)
        self.assertLess(took, 1.0, "the saves taken before the flush, a write or two")
        self.assertTrue(s.flush_saves(10))

    def test_a_flush_waits_for_the_newer_autosave_that_carries_one_it_passed_over(self):
        # An autosave taken before the flush and passed over for a newer one is on the disk only once that newer one
        # is written. Counting it done when it was passed over let the flush return with the autosave rounds behind
        # the game (CI's windows runner, test_server_drive's all-bot game: turn 17 on the disk at turn 21).
        s = self.game([{"type": "bot"}] * 2)
        self.play_to(s, 4)
        real = sess.SaveJob.write
        entered, gates, written = threading.Event(), [threading.Event(), threading.Event()], []

        def held(job):
            entered.set()
            gates[min(len(written), 1)].wait(10)
            real(job)
            written.append(job.path.name)

        def note(i):
            with s.lock:
                s.game.add_thought(0, f"flush note {i}", "note")
                s.autosave(force=True)
        flushed, got = threading.Event(), []

        def flush():
            got.append(s.flush_saves(10))
            flushed.set()
        with mock.patch.object(sess.SaveJob, "write", held):
            note(0)                              # in flight, held
            self.assertTrue(entered.wait(10))
            note(1)                              # queued before the flush
            t = threading.Thread(target=flush)
            t.start()
            time.sleep(0.2)                      # the flush has counted the saves taken before it
            note(2)                              # taken after the flush began: it carries note 1's history
            gates[0].set()                       # note 0 is written, note 1 passed over for note 2, held
            self.assertFalse(flushed.wait(0.3), "the flush waits for the save that carries the one it passed over")
            gates[1].set()
            self.assertTrue(flushed.wait(10))
            t.join(10)
        self.assertEqual(got, [True])
        self.assertEqual(written, ["autosave.citar", "autosave.citar"])
        back = self.m.load(s.folder / "autosave.citar")
        self.assertEqual([t["text"] for t in back.game.thoughts() if t["text"].startswith("flush note")],
                         [f"flush note {i}" for i in range(3)])


@needs_test_ops
class ListingTests(SavesCase):
    """Gate 7: listing 50 saves of a late game reads their headers alone (asserted by the binding's counters: no body
    decoded). Each is the late fixture's state with the metrics of a late gargantuan game (24 seats' records for 330
    turns, about 4 MB of JSON, the larger part of such a save's body); a save of a real one, played to turn 330, is
    listed the same way in DESIGN.md's notes for 2-11."""

    TOOLS = ("move_unit", "found_city", "set_research", "set_production", "attack", "fortify", "build_improvement",
             "end_turn", "buy", "promote", "set_policy", "declare_war", "propose_deal", "embark", "pillage")

    def test_listing_50_saves_of_a_late_game_reads_headers_only(self):
        from citar import _engine
        state = json.loads(gzip.decompress((LATE / "late-t280.state.json.gz").read_bytes()))
        g = engine_api.EngineGame.from_state(state)
        majors = g.majors(alive_only=False)
        s = sess.GameSession(g, [sess.Seat(player=p["id"], type="bot") for p in majors], name="Late")
        self.ids.add(s.id)
        for turn in range(1, 331):
            for p in range(24):
                s.metrics.begin_turn(p, turn, "bot")
                s.metrics.bot_actions(p, {tool: [(i + turn) % 7 + 1, i % 3] for i, tool in enumerate(self.TOOLS)})
                s.metrics.end_turn(p)
        path = s.save("late")
        s.stop()
        size = path.stat().st_size
        for i in range(50):
            copy = sess.SAVE_DIR / f"{s.id}-{i:02d}"
            shutil.copytree(s.folder, copy)
            self.ids.add(copy.name)
        headers, bodies = _engine._saves_read()
        t0 = time.perf_counter()
        listed = [e for e in sess.SessionManager.list_saves() if e["game_id"].startswith(f"{s.id}-")]
        took = time.perf_counter() - t0
        after = _engine._saves_read()
        self.assertEqual(after[1], bodies, "no save's body was decoded")
        self.assertGreaterEqual(after[0] - headers, 50)
        self.assertEqual(len([e for e in listed if e["name"] == "late"]), 50)
        self.assertTrue(all(e["turn"] == 280 and len(e["players"]) == len(majors) for e in listed), listed[:1])
        self.assertTrue(all(e["players"][0]["seat"] == "bot" for e in listed))
        # a note, not a gate: the first listing after the copies also pays for the virus scanner reading each new file
        print(f"\nlisting 50 saves of a late game ({size / 1e6:.2f} MB each): {took * 1000:.0f} ms", flush=True)


class SaveLockTests(SavesCase):
    """Gate 6 at the session's level: everything a save does under the session's lock (the engine's snapshot, the
    session's record, the metrics) stays within P2.5.3's 10 ms budget to the end of a long game, every round's
    autosave taken, and does not grow with the game: the metrics, a record a seat a turn, are encoded once each."""

    def test_a_long_games_saves_hold_the_lock_within_the_budget(self):
        s = self.game([{"type": "bot"}] * 4, {"map_size": "small", "seed": 5003})
        held = []
        real = s._snapshot

        def snapshot(name, autosave):            # an autosave is taken with the lock held: this is all it holds it for
            t0 = time.perf_counter()
            job = real(name, autosave)
            held.append((s.game.turn, time.perf_counter() - t0))
            return job
        s._snapshot = snapshot
        self.play_to(s, 300, timeout=900)
        self.autosave_now(s)
        self.assertEqual(s.errors, [])
        self.assertGreaterEqual(len(held), 150, "a long game")
        late = [t for _, t in held[-50:]]
        print(f"\nsaves under the lock to turn {s.game.turn} (small, 4 bots, {len(s.metrics.data['turns'])} metrics "
              f"records): median of the last 50 {statistics.median(late) * 1000:.2f} ms, max "
              f"{max(t for _, t in held) * 1000:.2f} ms, the session's own count {s.save_lock['saves']} saves, "
              f"{s.save_lock['total_s'] * 1000:.0f} ms in all", flush=True)
        self.assertLess(statistics.median(late), 0.010, "the budget")
        self.assertLess(max(t for _, t in held), 0.100, "never above 100 ms")
        self.assertEqual(s.save_lock["saves"], len(held) + 1, "and the new game's first, before the count above")
        self.assertLess(s.save_lock["max_s"], 0.100)
        # the metrics' records are encoded once each: a save encodes the open turn's (and any settled since the last),
        # never the whole game's again
        encoded = []
        dumps = json.dumps

        def counting(obj, *a, **k):
            encoded.append(obj)
            return dumps(obj, *a, **k)

        def records(o) -> int:
            if isinstance(o, dict) and isinstance(o.get("turns"), list):
                return len(o["turns"])
            return 1 if isinstance(o, dict) and "player" in o and "turn" in o else 0
        with mock.patch("json.dumps", counting):
            self.autosave_now(s)
        self.assertLessEqual(sum(records(o) for o in encoded), len(s.seats))
        # and what the saves wrote is the session's metrics, whole
        saved = engine_api.read_save(s.folder / "autosave.citar").metrics
        self.assertEqual(saved, json.loads(json.dumps(s.metrics.data)))


class LiveReportTests(SavesCase):
    def test_a_report_of_a_running_lobby_game_changes_nothing_of_it(self):
        from citar.reports import data as report_data
        seats = [{"type": "llm", "llm": {"provider": "mock", "model": "m1"}},
                 {"type": "llm", "llm": {"provider": "mock", "model": "m2"}}, {"type": "bot"}]
        s = self.game(seats, {"map_size": "small", "seed": 41, "barbarians": "off"})
        self.assertTrue(s.registered)
        details = report_data._game_details(s.id)
        self.assertNotIn("error", details)
        self.assertEqual([(x["player"], x["model"]) for x in details["seats"]], [(0, "m1"), (1, "m2")])
        self.assertEqual([x["progress"]["civ"] for x in details["seats"]], [s.game.player_name(0), s.game.player_name(1)])
        self.assertIsNone(s.benchmark, "a lobby game stays one")
        s.mark_live()
        self.assertTrue((s.folder / s.LIVE_MARK).exists(), "so a restart brings it back")
        self.autosave_now(s)
        self.assertFalse(self.header(s)["session"]["benchmark"])
        # a benchmark game's record is left as it was too, whatever seat the report reads
        s.benchmark = {"llm_player": 1, "model": "m2", "run_id": "r1"}
        before = dict(s.benchmark)
        details = report_data._game_details(s.id)
        self.assertEqual(s.benchmark, before)
        self.assertEqual([x["progress"]["civ"] for x in details["seats"]], [s.game.player_name(0), s.game.player_name(1)])


class LettingGoTests(SavesCase):
    def test_a_written_save_is_let_go_and_a_dropped_session_closes_its_journal(self):
        g = engine_api.EngineGame.new({"map_size": "duel", "seed": 41, "players": [{"controller": "bot"}] * 2})
        s = sess.GameSession(g, [sess.Seat(player=p, type="bot") for p in range(2)], name="dropped")
        self.ids.add(s.id)
        job = s._snapshot("first", autosave=False)
        self.assertEqual(job.result(30), s.folder / "first.citar")
        self.assertEqual((job.snap, job.journal, job.session, job.metrics), (None, None, None, None),
                         "the writer keeps nothing of a written save")
        path, thread = s.journal.path, s._writer._thread
        self.assertTrue(engine_api.journal_in_use(path))
        del s, g
        gc.collect()
        self.assertTrue(wait(lambda: not thread.is_alive(), 10), "its writer thread ends")
        self.assertTrue(wait(lambda: not engine_api.journal_in_use(path), 10), "and its journal is let go")

    def test_a_save_taken_as_the_game_stops_is_its_timelines_last(self):
        s = self.game([{"type": "bot"}] * 2)
        self.play_to(s, 4)
        s.set_paused(False)                      # the bots play on as it stops
        self.assertTrue(wait(lambda: s.game.turn >= 6, 60))
        path = s.stop(save_as="benchmark")
        self.assertEqual(path, s.folder / "benchmark.citar")
        last = engine_api.save_header(path)["journal"]
        self.assertEqual(last["records"], s.journal.records, "it names every record")
        for p in s.folder.glob("*.citar"):
            self.assertLessEqual(engine_api.save_header(p)["journal"]["records"], last["records"], p.name)
        with self.assertRaises(RuntimeError):
            s.stop(save_as="again")
        back = self.m.load(path)
        self.assertEqual(back.journal.path.name, "journal.cjnl", "a later load goes on in its journal: no fork")
        self.assertEqual(sorted(p.name for p in s.folder.glob("*.cjnl")), ["journal.cjnl"])


class StartingPointTests(SavesCase):
    """Games that do not start as a new game: from a scenario's state (whose counts name a history it does not have),
    and a save that carries its own journal (a probe case's copy)."""

    def test_a_game_started_from_a_scenario_saves_and_loads(self):
        src = self.game([{"type": "bot"}] * 2)
        self.play_to(src, 6)
        scn = {"name": "From turn 6", "state": src.game.state_dict(), "seats": [{"type": "bot"}] * 2}
        s = self.m.create_from_scenario(scn, start=False)
        self.ids.add(s.id)
        self.play_to(s, s.game.turn + 4)
        self.autosave_now(s)
        self.assertEqual(s.errors, [], "its journal started over with the history the game holds")
        self.assertEqual(self.header(s)["journal"]["file"], "journal.cjnl")
        live = self.as_it_is(s)
        # the state counted the history of the game it came from; from its first save it counts its own, so it
        # loads whole
        with self.assertNoLogs("citar.engine", level="WARNING"):
            back = self.m.load(s.folder / "autosave.citar")
        got = self.as_it_is(back)
        self.assertEqual(got["digest"], live["digest"])
        self.assertEqual(got["events"], live["events"])
        self.assertEqual(got["stats"], live["stats"])

    def test_a_copy_with_its_own_journal_loads_by_forking_into_its_games_folder(self):
        s = self.game([{"type": "bot"}] * 2)
        self.play_to(s, 6)
        live = self.as_it_is(s)
        elsewhere = sess.SAVE_DIR / f"copies-{s.id}"
        self.ids.add(elsewhere.name)
        copy = s.save_copy(elsewhere / "case-1.citar")
        self.assertEqual(sorted(p.name for p in elsewhere.iterdir()), ["case-1.citar", "case-1.cjnl"])
        self.assertEqual(engine_api.save_header(copy)["journal"]["records"], 1, "the whole history, one record")
        self.assertEqual(s.journal.path.name, "journal.cjnl", "the game's own timeline is untouched")
        self.play_to(s, s.game.turn + 2)              # and it saves on as before
        self.assertEqual(s.errors, [])
        back = self.m.load(copy)
        self.assertEqual(back.id, s.id)
        self.assertEqual(back.journal.path.parent, s.folder, "its timeline goes on in the game's own folder")
        self.assertEqual(back.journal.path.name, "journal-2.cjnl")
        got = self.as_it_is(back)
        self.assertEqual(got["digest"], live["digest"])
        self.assertEqual(got["events"], live["events"])
        self.assertEqual(sorted(p.name for p in elsewhere.iterdir()), ["case-1.citar", "case-1.cjnl"])


class SpellingTests(SavesCase):
    def test_a_save_loaded_by_another_spelling_of_its_folder_saves_on(self):
        # The save route resolves the path it is given; the game's own folder is SAVE_DIR as configured, which may
        # be another spelling of it (macOS's /var is /private/var; a Windows 8.3 name; a `..`).
        s = self.game([{"type": "bot"}] * 2)
        self.play_to(s, 4)
        self.autosave_now(s)
        other = sess.SAVE_DIR / s.id / ".." / s.id / "autosave.citar"
        self.assertNotEqual(str(other.parent), str(s.folder))
        back = self.m.load(other)
        self.assertEqual(back.journal.path.parent, back.folder, "the journal by the game's own folder's path")
        self.play_to(back, back.game.turn + 2)
        self.autosave_now(back)
        self.assertEqual(back.errors, [])
        self.assertEqual(self.header(back)["summary"]["turn"], back.game.turn)

    def test_a_save_is_written_beside_its_journal_however_the_folder_is_spelled(self):
        s = self.game([{"type": "bot"}] * 2)
        self.play_to(s, 3)
        with s.lock:
            snap = s.game.save_snapshot(s._timeline())
        record, metrics = {"id": s.id, "name": "x", "seats": []}, {}
        snap.write(s.folder / ".." / s.id / "spelled.citar", s.journal, record, metrics)
        self.assertTrue((s.folder / "spelled.citar").exists())
        elsewhere = sess.SAVE_DIR / f"{s.id}-elsewhere"
        elsewhere.mkdir()
        self.ids.add(elsewhere.name)
        with self.assertRaises(ValueError):          # a container names its journal by a plain name beside it
            snap.write(elsewhere / "lost.citar", s.journal, record, metrics)
        self.assertFalse((elsewhere / "lost.citar").exists())


class FormatTests(SavesCase):
    def test_a_python_engine_save_is_refused_by_name(self):
        folder = sess.SAVE_DIR / "v1-game"
        folder.mkdir()
        self.ids.add(folder.name)
        old = folder / "autosave.citar"
        old.write_bytes(gzip.compress(b'{"format": "citar-save", "version": 1, "state": {}}'))
        with self.assertRaises(ValueError) as cm:
            engine_api.read_save(old)
        self.assertIn("saved by the Python engine; archived with 0.1.5", str(cm.exception))
        meta = sess.SessionManager.save_meta(old)
        self.assertTrue(meta["unreadable"])
        self.assertIn("archived with 0.1.5", meta["error"])
        with self.assertRaises(ValueError):
            self.m.load(old)

    def test_a_save_that_does_not_load_leaves_the_running_game_alone(self):
        s = self.game([{"type": "bot"}] * 2)
        self.play_to(s, 4)
        early = s.save("turn-early")
        bad = s.folder / "bad.citar"
        data = bytearray(early.read_bytes())
        data[-6] ^= 0xFF                         # its header reads; its body does not
        bad.write_bytes(bytes(data))
        engine_api.save_header(bad)
        lost = s.folder / "lost.citar"          # it reads, and names a journal that is not there
        with_journal(early, lost, "journal-9.cjnl")
        engine_api.read_save(lost)
        s.set_paused(False)                      # the game runs
        journal = s.journal
        for refused in (bad, lost):
            with self.assertRaises(ValueError) as cm:
                self.m.load(refused)
            self.assertIs(self.m.get(s.id), s, refused.name)
            self.assertFalse(s.stopped or s.paused or journal.is_closed, f"{refused.name}: the game plays on")
        self.assertIn("does not load", str(cm.exception))
        turn = s.game.turn
        self.assertTrue(wait(lambda: s.game.turn > turn, 60), "and its driver drives it")
        # a game closed before a failed load stays closed
        self.m.delete(s.id)
        with self.assertRaises(ValueError):
            self.m.load(bad)
        self.assertIsNone(self.m.get(s.id))

    def test_a_save_on_the_running_games_journal_that_does_not_load_brings_the_game_back(self):
        # turn-late is put back after the timeline it names has gone another way: it names records of journal.cjnl
        # (the running game's, whose OS lock keeps it from being read) that are not the ones there now
        s = self.game([{"type": "bot"}] * 2)
        self.play_to(s, 6)
        early = s.save("turn-early")
        self.play_to(s, s.game.turn + 6)
        late = s.save("turn-late")
        self.m.delete(s.id)
        aside = sess.SAVE_DIR / f"{s.id}-aside"
        aside.mkdir()
        self.ids.add(aside.name)
        for name in ("turn-late.citar", "autosave.citar"):
            (s.folder / name).rename(aside / name)
        a = self.m.load(early)                   # nothing names more of journal.cjnl: it goes on, cut to turn-early
        self.assertEqual(a.journal.path.name, "journal.cjnl")
        self.play_to(a, a.game.turn + 8)
        self.autosave_now(a)
        (aside / "turn-late.citar").rename(late)
        a.set_paused(False)                      # the game runs
        with self.assertRaises(ValueError) as cm:
            self.m.load(late)
        self.assertIn("turn-late.citar's history", str(cm.exception))
        self.assertTrue(a.stopped, "the running game was stopped to read its journal")
        back = self.m.get(a.id)
        self.assertIsNotNone(back, "and came back from its autosave")
        self.assertIsNot(back, a)
        self.assertFalse(back.paused, "running, as it was")
        self.assertGreaterEqual(back.game.turn, a.game.turn - 1)
        turn = back.game.turn
        self.assertTrue(wait(lambda: back.game.turn > turn, 60), "and its driver drives it")

    def test_a_python_engine_autosave_is_not_restored_nor_tried_again(self):
        import contextlib
        import io
        import tempfile
        root = Path(tempfile.mkdtemp(prefix="citar-restore-"))
        try:
            folder = root / "v1-live"
            folder.mkdir()
            (folder / "autosave.citar").write_bytes(gzip.compress(b'{"format": "citar-save", "version": 1, "state": {}}'))
            mark = folder / sess.GameSession.LIVE_MARK
            mark.write_text(json.dumps({"paused": False, "name": "Old", "at": 0}), encoding="utf-8")
            out, err = io.StringIO(), io.StringIO()
            with mock.patch.object(sess, "SAVE_DIR", root), contextlib.redirect_stdout(out), \
                    contextlib.redirect_stderr(err):
                self.assertEqual(self.m.restore_live(), [])
                self.assertEqual(self.m.restore_live(), [], "and not tried again")
            self.assertEqual(err.getvalue(), "", "no traceback")
            lines = out.getvalue().strip().splitlines()
            self.assertEqual(len(lines), 1, lines)
            self.assertIn("v1-live", lines[0])
            self.assertIn("saved by the Python engine; archived with 0.1.5", lines[0])
            self.assertFalse(mark.exists())
            self.assertTrue((folder / "autosave.citar").exists(), "the save itself stays")
        finally:
            shutil.rmtree(root, ignore_errors=True)

    def test_deleting_saves_removes_the_journals_no_save_names_but_never_a_session_s(self):
        s = self.game([{"type": "bot"}] * 2)
        self.play_to(s, 6)
        early = s.save("turn-early")
        self.play_to(s, s.game.turn + 4)
        a = self.m.load(early)                    # forks journal-2, which no save names yet
        self.assertEqual(a.journal.path.name, "journal-2.cjnl")

        def rel(p):
            return str(p.relative_to(sess.SAVE_DIR)).replace("\\", "/")
        sess.SessionManager.delete_save(rel(s.folder / "autosave.citar"))
        names = sorted(p.name for p in s.folder.glob("*.cjnl"))
        self.assertEqual(names, ["journal-2.cjnl", "journal.cjnl"], "turn-early names one, a session holds the other")
        sess.SessionManager.delete_save(rel(early))
        self.assertEqual(sorted(p.name for p in s.folder.glob("*.cjnl")), ["journal-2.cjnl"], "named by none")
        last = a.save("turn-a")
        self.m.delete(a.id)
        removed = sess.SessionManager.delete_save(rel(last), whole_game=True)
        self.assertEqual(removed, [rel(last)])
        self.assertFalse(s.folder.exists(), "the folder goes with its last save and journal")


if __name__ == "__main__":
    unittest.main()
