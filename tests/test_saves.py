"""Saves v2 in the server (crates/citar-engine/DESIGN.md P2.5, P2.5.3; package 2-11's gates 1 to 7).

A session's saves are ``.citar`` containers (a small header, the session's record and metrics, the state) beside the
journal that holds the game's history, one record per save's new chunk. A save is taken under the session's lock (a
snapshot of the state, its new history pending in the journal) and written off it by the session's writer thread:
the chunks appended and synced first, then the container that names them. A session writes one journal, its timeline:
a load continues the save's journal when nothing names more of it and it opens clean, cutting off records no save
names, and forks it otherwise. These tests play bot games under the session driver and check each guarantee.
"""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import gzip
import shutil
import struct
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

from citar import engine_api
from citar.server import session as sess
from tests.backends import has_test_ops, rust_only

needs_test_ops = unittest.skipUnless(has_test_ops(), "needs the engine's test operations (a test-ops build)")
DUEL = {"map_size": "duel", "seed": 41, "barbarians": "off"}


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


@rust_only
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
            job = real(name, autosave)
            if autosave:
                taken.append(job.snap.turn)
            return job
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


@needs_test_ops
class ListingTests(SavesCase):
    """Gates 6 and 7: a gargantuan game's snapshot under the lock, and listing 50 gargantuan saves by headers alone."""

    def test_listing_50_gargantuan_saves_reads_headers_only(self):
        from citar import _engine
        g = engine_api.EngineGame.new({"map_size": "gargantuan", "seed": 4, "players": [{"controller": "bot"}] * 8})
        s = sess.GameSession(g, [sess.Seat(player=p, type="bot") for p in range(8)], name="Gargantuan")
        self.ids.add(s.id)
        times = []
        for _ in range(5):                       # gate 6 in the binding: the snapshot is all the lock waits for
            with s.lock:
                s.game.add_thought(0, "a note", "note")
                t0 = time.perf_counter()
                s.game.save_snapshot(s._timeline())
                times.append(time.perf_counter() - t0)
        print(f"\nsave_snapshot of a gargantuan game at turn {g.turn}: best {min(times) * 1000:.2f} ms", flush=True)
        self.assertLess(min(times), 0.100)
        path = s.save("gargantuan")
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
        self.assertEqual(len([e for e in listed if e["name"] == "gargantuan"]), 50)
        self.assertTrue(all(e["turn"] == g.turn and len(e["players"]) == 8 for e in listed if "turn" in e))
        print(f"listing 50 gargantuan saves ({size / 1e6:.2f} MB each): {took * 1000:.0f} ms", flush=True)


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
