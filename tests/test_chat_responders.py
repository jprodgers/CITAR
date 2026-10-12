"""The seats' chat answers (design/phase3-features.md P3.5.3; package 3-11's gates 3 to 5).

- Responders (``citar/agents/responders.py``): a line per seat, one chat at a time, on two pools: model answers on
  ``CITAR_RESPONDERS`` threads, bot answers on their own, so a bot never waits behind models stuck on a busy machine;
  a model seat keeps one conversation per chat (``ChatThread``), and the responders' keys go when the chats close.
- A halt never declines: a suspension, a seat change or a server restart mid-answer leaves the chat open, and the seat
  answers it when it comes back, without waiting for the chat clock.
- Late replies: an answer that comes during a model's turn after the tool's own wait is in the model's next message,
  and an accepted, declined or expired chat's note asks for no answer.
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and registry; must be imported before citar)

import re
import shutil
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

from citar import db, servers
from citar.agents import prompts, responders
from citar.agents.providers import make_conversation as real_make_conversation
from citar.agents.providers.base import Conversation, StepResult, ToolCall
from citar.db.models import Base
from citar.pool import scheduler as S
from citar.server import session as session_mod
from citar.server.session import SAVE_DIR, SessionManager

_URL = "sqlite:///" + (Path(tempfile.mkdtemp(prefix="citar_chat_responders_")) / "test.db").as_posix()


def setUpModule():
    """This module's own database, with the test registry's machines (its model seats play ``sv_gpu``, one slot)."""
    db.dispose()
    db.configure(_URL)
    Base.metadata.drop_all(db.engine())
    db.create_all()
    servers.invalidate()


def until(fn, timeout: float = 15.0, step: float = 0.01):
    """Poll ``fn`` until it returns something true (returned) or ``timeout`` passes (its last value)."""
    deadline = time.monotonic() + timeout
    while True:
        value = fn()
        if value or time.monotonic() > deadline:
            return value
        time.sleep(step)


class Recording(Conversation):
    """A model that replies to each message of a chat (``respond_negotiation`` reply), recording what it was told,
    on which threads it ran, how many of its seat's answers ran at once, and how many conversations each (seat,
    chat) had. ``gate`` (an Event), when set on the class, holds every step until it is set; ``hang`` holds the
    steps of the seats it names until their conversation is closed."""
    lock = threading.Lock()
    made: dict = {}             # (seat, chat) -> conversations made for it
    threads: set = set()
    running: dict = {}          # seat -> its answers running now
    most: dict = {}             # seat -> the most that ran at once
    texts: list = []
    gate: threading.Event | None = None
    hang: set = set()

    @classmethod
    def reset(cls):
        cls.made, cls.threads, cls.running, cls.most, cls.texts = {}, set(), {}, {}, []
        cls.gate, cls.hang = None, set()

    def __init__(self, cfg, system, tools):
        self.cfg = cfg
        self.seat = cfg.get("seat")
        self.tools = {t["name"] for t in tools}
        self.usage = {"input_tokens": 1, "output_tokens": 1}
        self.nid = None
        self.pending = False
        self.closed = threading.Event()
        self.system = system

    def add_user_text(self, text):
        with Recording.lock:
            Recording.texts.append((self.seat, text))
        found = re.search(r"negotiation #(\d+)", text)
        if found and self.nid is None:
            self.nid = int(found.group(1))
            with Recording.lock:
                Recording.made[(self.seat, self.nid)] = Recording.made.get((self.seat, self.nid), 0) + 1
        self.pending = True

    def add_tool_results(self, results):
        pass

    def close(self):
        self.closed.set()

    def step(self):
        with Recording.lock:
            Recording.threads.add(threading.current_thread().name)
            Recording.running[self.seat] = Recording.running.get(self.seat, 0) + 1
            Recording.most[self.seat] = max(Recording.most.get(self.seat, 0), Recording.running[self.seat])
        try:
            if self.seat in Recording.hang:
                self.closed.wait(30)
                raise RuntimeError("cancelled")
            if Recording.gate is not None:
                while not Recording.gate.wait(0.05):
                    if self.closed.is_set():
                        raise RuntimeError("cancelled")
            time.sleep(0.01)
            if self.pending and self.nid is not None and "respond_negotiation" in self.tools:
                self.pending = False
                return StepResult(text="answer", tool_calls=[ToolCall("r", "respond_negotiation", {
                    "negotiation_id": self.nid, "action": "reply", "message": f"seat {self.seat} answers"})])
            return StepResult(text="nothing")
        finally:
            with Recording.lock:
                Recording.running[self.seat] -= 1


class Case(unittest.TestCase):
    def setUp(self):
        Recording.reset()
        S.reset()
        self.addCleanup(S.reset)
        self.manager = SessionManager()
        patcher = mock.patch("citar.agents.llm_agent.make_conversation", Recording)
        patcher.start()
        self.addCleanup(patcher.stop)

    def tearDown(self):
        if Recording.gate is not None:
            Recording.gate.set()
        for sid in list(self.manager.sessions):
            self.manager.delete(sid)
            shutil.rmtree(SAVE_DIR / sid, ignore_errors=True)

    def game(self, seats: list, start: bool = False, **config):
        s = self.manager.create({"map_size": "small", "seed": 5, "barbarians": "off", "city_states": 0, **config},
                                seats, track=False, start=start)
        with s.lock:
            n = len(seats)
            s.game.apply_ops([{"op": "meet", "a": a, "b": b} for a in range(n) for b in range(a + 1, n)])
        return s

    @staticmethod
    def model(seat: int, **more) -> dict:
        return {"type": "llm", "llm": {"provider": "mock", "seat": seat, **more}}

    @staticmethod
    def open(s, a: int, b: int, message: str = "Shall we talk?") -> int:
        with s.lock:
            return s.game.open_negotiation_as(a, b, message)["negotiation_id"]


class Responders(Case):
    """Gate 3."""

    def test_forty_chats_with_eight_model_seats_on_a_pool_of_four(self):
        with mock.patch.dict("os.environ", {"CITAR_RESPONDERS": "4"}):
            responders.reset()
            self.addCleanup(responders.reset)
            people, models = list(range(4)), list(range(4, 12))
            s = self.game([{"type": "human"}] * 4 + [self.model(p) for p in models])
            chats = [self.open(s, p, m) for p in people for m in models]          # 32 people's chats
            chats += [self.open(s, m, models[(i + 1) % 8]) for i, m in enumerate(models)]   # 8 between models
            self.assertEqual(len(chats), 40)
            s.dispatch_waiting()
            # every chat with a person comes back to that person; those between models go back and forth until
            # their message cap closes them
            self.assertTrue(until(lambda: all(
                (n["status"] != "open" or n["awaiting"] in people) for n in s.game.negotiations()), 60),
                [(n["id"], n["status"], n["awaiting"]) for n in s.game.negotiations() if n["status"] == "open"])
        self.assertTrue(Recording.threads)
        self.assertTrue(all(name.startswith("model-responder") for name in Recording.threads), Recording.threads)
        self.assertLessEqual(len(Recording.threads), 4, Recording.threads)
        self.assertEqual(set(Recording.most.values()), {1}, "one chat at a time per seat")
        people_chats = [n for n in s.game.negotiations() if n["initiator"] in people]
        self.assertTrue(all(any(h["by"] == n["responder"] for h in n["history"]) for n in people_chats))
        # each person answers again: the model's second exchange in a chat goes on in the same conversation
        for n in people_chats:
            s.call_tool(n["initiator"], "respond_negotiation",
                        {"negotiation_id": n["id"], "action": "reply", "message": "And then?"})
        self.assertTrue(until(lambda: all(
            sum(1 for h in s.game.negotiation(n["id"])["history"] if h["by"] == n["responder"]) >= 2
            for n in people_chats), 30))
        self.assertEqual(set(Recording.made.values()), {1},
                         "one conversation per chat for each seat, whatever the exchanges")
        self.assertGreaterEqual(len(Recording.made), 40)
        # the chats close: no responder key is left behind
        for n in people_chats:
            s.call_tool(n["initiator"], "respond_negotiation",
                        {"negotiation_id": n["id"], "action": "reject", "message": "Enough."})
        self.assertTrue(until(lambda: not any(n["status"] == "open" for n in s.game.negotiations()), 30))
        self.assertTrue(until(lambda: not s._responding, 10), s._responding)

    def test_an_answer_that_closes_the_chat_is_counted_as_answered(self):
        """The model's accept closes the chat (and with it the chat's conversation, mid-exchange): still its answer."""
        patcher = mock.patch("citar.agents.llm_agent.make_conversation", real_make_conversation)   # the dry run
        patcher.start()
        self.addCleanup(patcher.stop)
        s = self.game([{"type": "human"}, {"type": "llm", "llm": {"provider": "dryrun", "dry_run_delay": 0,
                                                                   "dry_run_negotiation": "accept"}}])
        r = s.call_tool(0, "open_negotiation", {"to": 1, "message": "Maps for maps?", "give": [{"type": "share_map"}],
                                                "receive": [{"type": "share_map"}]})
        nid = r["result"]["negotiation_id"]
        self.assertTrue(until(lambda: s.game.negotiation_head(nid)["status"] == "accepted", 10))
        self.assertTrue(until(lambda: s.metrics.data["negotiations"], 5))
        rows = [x for x in s.metrics.data["negotiations"] if x["nid"] == nid]
        self.assertEqual([(x["player"], x["outcome"], x["cause"]) for x in rows], [(1, "answered", None)])
        self.assertNotIn(nid, s.agents[1]._threads, "the closed chat's conversation was let go")

    def test_a_bot_answers_at_once_while_sixteen_model_answers_wait_for_a_one_slot_machine(self):
        responders.reset()
        self.addCleanup(responders.reset)
        Recording.gate = threading.Event()               # the first answer to get the slot holds it
        seats = [{"type": "human"}] + [{"type": "llm", "llm": {"server_id": "sv_gpu", "model": "dry-run"}}
                                       for _ in range(16)] + [{"type": "bot"}]
        s = self.game(seats)
        for m in range(1, 17):
            self.open(s, 0, m)
        s.dispatch_waiting()
        machine = S.get().machine("sv_gpu")
        self.assertTrue(until(lambda: machine.status()["in_flight"] == 1 and machine.status()["waiting"] == 15, 30),
                        machine.status())
        t0 = time.perf_counter()
        r = s.call_tool(0, "open_negotiation", {"to": 17, "message": "Friends?"})
        nid = r["result"]["negotiation_id"]
        self.assertTrue(until(lambda: s.game.negotiation_head(nid)["awaiting"] != 17, 5))
        took = time.perf_counter() - t0
        self.assertLess(took, 1.0, "the bot's answer did not wait behind the models")
        Recording.gate.set()
        self.assertTrue(until(lambda: all(s.game.negotiation_head(n)["awaiting"] == 0 for n in range(1, 17)), 60))


class Pools(unittest.TestCase):
    """The responders' pools start a thread for every task no free thread will take, up to their size."""

    def test_tasks_submitted_while_a_thread_is_free_each_get_a_thread(self):
        pool = responders.Pool(8, "test-responder")
        pool.submit(lambda: None)
        self.assertTrue(until(lambda: pool._idle == 1 and pool._queued == 0, 5), "one thread, free again")
        release, started = threading.Event(), []
        for _ in range(8):
            pool.submit(lambda: (started.append(1), release.wait(10)))
        self.assertTrue(until(lambda: len(started) == 8, 5), f"{len(started)} of 8 started")
        self.assertEqual(pool.threads, 8)
        pool.submit(lambda: started.append(1))
        time.sleep(0.2)
        self.assertEqual(len(started), 8, "the ninth waits: the pool is full")
        release.set()
        self.assertTrue(until(lambda: len(started) == 9, 5))
        self.assertEqual(pool.threads, 8)


class HaltsNeverDecline(Case):
    """Gate 4: a suspension and its resume, a seat change and a server restart mid-answer."""

    def test_a_suspension_mid_answer_leaves_the_chat_open_and_the_resume_answers_it(self):
        Recording.hang = {1}
        s = self.game([{"type": "human"}, self.model(1)])
        r = s.call_tool(0, "open_negotiation", {"to": 1, "message": "Peace?"})
        nid = r["result"]["negotiation_id"]
        self.assertTrue(until(lambda: s._answering.get(1)), "the model's answer is under way")
        s.suspend({"kind": "restricted", "message": "quiet hours", "since": time.time()})
        self.assertTrue(until(lambda: not s._answering, 10), "the halted answer returned")
        n = s.game.negotiation(nid)
        self.assertEqual((n["status"], n["awaiting"], len(n["history"])), ("open", 1, 1), "not declined")
        Recording.hang = set()
        s.resume()
        self.assertTrue(until(lambda: s.game.negotiation_head(nid)["awaiting"] == 0, 10))
        n = s.game.negotiation(nid)
        self.assertEqual([(h["by"], h["action"], h["message"]) for h in n["history"]][1:],
                         [(1, "reply", "seat 1 answers")])

    def test_a_chat_that_comes_during_quiet_hours_waits_for_the_resume_and_one_during_a_persons_pause_does_not(self):
        s = self.game([{"type": "human"}, self.model(1), self.model(2)])
        s.suspend({"kind": "restricted", "message": "quiet hours", "since": time.time()})
        r = s.call_tool(0, "open_negotiation", {"to": 1, "message": "Peace?"})
        held = r["result"]["negotiation_id"]
        time.sleep(0.5)
        self.assertEqual(s.game.negotiation_head(held)["awaiting"], 1, "its machine is in its quiet hours: it waits")
        s.resume()
        self.assertTrue(until(lambda: s.game.negotiation_head(held)["awaiting"] == 0, 10), "answered on the resume")
        s.set_paused(True)                                 # a person's pause holds no answer
        r = s.call_tool(0, "open_negotiation", {"to": 2, "message": "Peace?"})
        nid = r["result"]["negotiation_id"]
        self.assertTrue(until(lambda: s.game.negotiation_head(nid)["awaiting"] == 0, 10))

    def test_a_seat_changed_mid_answer_is_answered_by_its_new_agent(self):
        Recording.hang = {1}
        s = self.game([{"type": "human"}, self.model(1)])
        r = s.call_tool(0, "open_negotiation", {"to": 1, "message": "Peace?"})
        nid = r["result"]["negotiation_id"]
        self.assertTrue(until(lambda: s._answering.get(1)))
        s.update_seat(1, llm={"provider": "mock", "seat": 101})
        self.assertTrue(until(lambda: s.game.negotiation_head(nid)["awaiting"] == 0, 10))
        n = s.game.negotiation(nid)
        self.assertEqual([(h["by"], h["action"], h["message"]) for h in n["history"]][1:],
                         [(1, "reply", "seat 101 answers")], "the new agent answered; nobody declined")

    def test_a_chat_waiting_on_a_model_across_a_restart_is_answered_without_its_timeout(self):
        folder = Path(tempfile.mkdtemp(prefix="citar-chat-restore-"))
        self.addCleanup(shutil.rmtree, folder, ignore_errors=True)
        with mock.patch.object(session_mod, "SAVE_DIR", folder):
            Recording.hang = {1}
            g = self.game([{"type": "human"}, self.model(1)], start=True)
            r = g.call_tool(0, "open_negotiation", {"to": 1, "message": "Peace?"})
            nid = r["result"]["negotiation_id"]
            self.assertTrue(until(lambda: g._answering.get(1)))
            g.autosave(force=True)
            self.assertTrue(g.flush_saves(10))
            # the server goes away: the game is dropped from memory with its live mark left on the disk
            g.stop()
            self.manager.sessions.pop(g.id)
            self.assertEqual(g.game.negotiation(nid)["status"], "open", "a stop never declines")
            Recording.hang = set()
            manager2 = SessionManager()
            t0 = time.perf_counter()
            manager2.restore_live()
            self.addCleanup(lambda: [manager2.delete(sid) for sid in list(manager2.sessions)])
            s = manager2.get(g.id)
            self.assertIsNotNone(s)
            self.assertTrue(until(lambda: s.game.negotiation_head(nid)["awaiting"] == 0, 10),
                            s.game.negotiation(nid))
            self.assertLess(time.perf_counter() - t0, 10, "answered, not timed out (the model's timeout is 600 s)")
            self.assertEqual(s.game.negotiation(nid)["history"][-1]["message"], "seat 1 answers")


class Scripted(Conversation):
    """A model's turn played from a list of steps (each returns the calls to make), recording each message it is
    told: what the late replies are checked on."""
    script: list = []
    told: list = []

    def __init__(self, cfg, system, tools):
        self.cfg = cfg
        self.tools = {t["name"] for t in tools}
        self.usage = {"input_tokens": 1, "output_tokens": 1}
        self.steps = list(Scripted.script) if "end_turn" in self.tools else []

    def add_user_text(self, text):
        Scripted.told.append(text)

    def add_tool_results(self, results):
        pass

    def step(self):
        if self.steps:
            calls = self.steps.pop(0)()
            return StepResult(text="step", tool_calls=[ToolCall(f"c{i}", n, a) for i, (n, a) in enumerate(calls)])
        if "end_turn" in self.tools:
            return StepResult(text="done", tool_calls=[ToolCall("e", "end_turn", {})])
        return StepResult(text="nothing")


class LateReplies(unittest.TestCase):
    """Gate 5."""

    def setUp(self):
        S.reset()
        self.addCleanup(S.reset)
        self.manager = SessionManager()
        Scripted.told = []

    def tearDown(self):
        for sid in list(self.manager.sessions):
            self.manager.delete(sid)
            shutil.rmtree(SAVE_DIR / sid, ignore_errors=True)

    def game(self, people: int):
        seats = [{"type": "llm", "llm": {"provider": "mock", "negotiation_wait_seconds": 0.05}}]
        seats += [{"type": "human"}] * people
        s = self.manager.create({"map_size": "small", "seed": 5, "barbarians": "off", "city_states": 0}, seats,
                                track=False, start=False)
        with s.lock:
            s.game.apply_ops([{"op": "meet", "a": 0, "b": b} for b in range(1, people + 1)])
        return s

    def test_a_persons_reply_after_the_tools_wait_is_in_the_models_next_message(self):
        s = self.game(1)
        state = {}

        def opens():
            return [("open_negotiation", {"to": 1, "message": "Maps for maps?", "give": [{"type": "share_map"}],
                                          "receive": [{"type": "share_map"}]})]

        def meanwhile():
            # the tool's wait is over and the model goes on with its turn; the person answers now
            state["nid"] = s.game.open_negotiation_heads(0)[0]["id"]
            state["reply"] = s.call_tool(1, "respond_negotiation", {
                "negotiation_id": state["nid"], "action": "reply", "message": "Add some gold."})
            return [("get_empire", {})]

        def answers():
            state["told"] = list(Scripted.told)
            return [("respond_negotiation", {"negotiation_id": state["nid"], "action": "reject",
                                             "message": "No gold."})]
        Scripted.script = [opens, meanwhile, answers]
        with mock.patch("citar.agents.llm_agent.make_conversation", Scripted):
            s.get_agent(0).play_turn(s, 0)
        self.assertTrue(state["reply"]["ok"], state)
        news = [t for t in state["told"] if t.startswith("NEGOTIATION UPDATE")]
        self.assertEqual(len(news), 1, state["told"])
        self.assertIn(f"answered negotiation #{state['nid']}", news[0])
        self.assertIn('replied: "Add some gold."', news[0])
        self.assertIn("the proposal from your side:", news[0])
        self.assertIn("Answer with respond_negotiation, now or before end_turn.", news[0])
        self.assertNotEqual(s.game.current, 0, "the model answered, and its turn ended")

    def test_accepted_declined_and_expired_chats_ask_for_no_answer(self):
        s = self.game(3)
        state = {}

        def opens():
            return [("open_negotiation", {"to": q, "message": f"Maps, {q}?", "give": [{"type": "share_map"}],
                                          "receive": [{"type": "share_map"}]}) for q in (1, 2, 3)]

        def meanwhile():
            heads = {n["responder"]: n["id"] for n in s.game.open_negotiation_heads(0)}
            state["ids"] = heads
            s.call_tool(1, "respond_negotiation", {"negotiation_id": heads[1], "action": "accept", "message": "Yes."})
            s.call_tool(2, "respond_negotiation", {"negotiation_id": heads[2], "action": "reject",
                                                   "message": "Not today."})
            with s.lock:
                s.expire_chat(heads[3], s.game.negotiation_head(heads[3])["entries"])
            return [("get_empire", {})]

        def look():
            state["told"] = list(Scripted.told)
            return [("log_thought", {"text": "noted"})]
        Scripted.script = [opens, meanwhile, look]
        with mock.patch("citar.agents.llm_agent.make_conversation", Scripted):
            s.get_agent(0).play_turn(s, 0)
        news = [t for t in state["told"] if t.startswith("NEGOTIATION UPDATE")]
        self.assertEqual(len(news), 1, state["told"])
        notes = news[0].split("\n\n")
        ids = state["ids"]
        self.assertEqual(len(notes), 3, notes)
        self.assertIn(f"accepted negotiation #{ids[1]}: the deal is in effect. No answer is needed.", notes[0])
        self.assertIn(f"negotiation #{ids[2]} with", notes[1])
        self.assertIn("is closed (declined: Not today.). No answer is needed.", notes[1])
        self.assertIn("is closed (expired: (no reply in time)). No answer is needed.", notes[2])
        for note in notes:
            self.assertNotIn("Answer with respond_negotiation", note)

    def test_a_chat_somebody_else_opened_is_its_responders_until_it_closes(self):
        """During the model's turn a chat another seat opened, waiting on the model, is answered by the seat's
        responder: the turn is not asked to answer it too, only told when it closes."""
        s = self.game(1)
        # the responder itself is not under test here: the model's answers are the test's own calls
        with mock.patch("citar.agents.llm_agent.make_conversation", Scripted), \
                mock.patch.object(s, "_dispatch_negotiation_interrupts"):
            Scripted.script = []
            agent = s.get_agent(0)
            with s.lock:
                nid = s.game.open_negotiation_as(1, 0, "Peace?")["negotiation_id"]
                agent._marks = agent._chat_marks(s, 0)
            s.call_tool(0, "respond_negotiation", {"negotiation_id": nid, "action": "reply", "message": "Why?"})
            s.call_tool(1, "respond_negotiation", {"negotiation_id": nid, "action": "reply", "message": "Because."})
            self.assertEqual(agent._chat_news(s, 0), [], "waiting on the model, but its responder's to answer")
            s.call_tool(1, "respond_negotiation", {"negotiation_id": nid, "action": "reject", "message": "Bye."})
            news = agent._chat_news(s, 0)
        self.assertEqual(len(news), 1)
        self.assertIn("is closed (withdrawn: Bye.). No answer is needed.", news[0])

    def test_an_answer_the_machine_cut_off_is_asked_again_as_it_stood(self):
        """A chat's answer cut off by its machine (a probe's answer borrowing a lobby seat's slot) is asked again on
        the same conversation with the same message, not told "nothing new"."""
        from citar.pool.scheduler import MachineRefused
        told, tries = [], []

        class CutOnce(Conversation):
            def __init__(self, cfg, system, tools):
                self.usage = {"input_tokens": 1, "output_tokens": 1}
                self.nid = None

            def add_user_text(self, text):
                told.append(text)
                self.nid = self.nid or int(re.search(r"negotiation #(\d+)", text).group(1))

            def add_tool_results(self, results):
                pass

            def step(self):
                tries.append(1)
                if len(tries) == 1:
                    raise MachineRefused("A game playing on this machine needs its slot back.", code="yielded")
                return StepResult(text="ok", tool_calls=[ToolCall("r", "respond_negotiation", {
                    "negotiation_id": self.nid, "action": "reply", "message": "Here I am."})])
        s = self.game(1)
        with mock.patch("citar.agents.llm_agent.make_conversation", CutOnce):
            agent = s.prompt_agent(0)
            with s.lock:
                nid = s.game.open_negotiation_as(1, 0, "Peace?")["negotiation_id"]
            first = agent.respond_negotiation(s, 0, nid)
            second = agent.respond_negotiation(s, 0, nid)
        self.assertEqual((first, second), ("failed", "answered"))
        self.assertEqual(len(told), 1, told)
        rows = [(x["outcome"], x["cause"]) for x in s.metrics.data["negotiations"] if x["nid"] == nid]
        self.assertEqual(rows, [("fallback", "refused"), ("answered", None)])

    def test_a_chat_the_other_side_opened_and_withdrew_is_told_as_withdrawn(self):
        view = {"id": 4, "with_name": "Rome", "status": "rejected", "you_initiated": False,
                "history": [{"seq": 1, "you": False, "action": "open", "message": "Peace?"},
                            {"seq": 2, "you": False, "action": "reject", "message": "Never mind."}]}
        self.assertEqual(prompts.late_note(view),
                         "NEGOTIATION UPDATE: negotiation #4 with Rome is closed (withdrawn: Never mind.). "
                         "No answer is needed.")
        view["you_initiated"] = True
        self.assertIn("(declined: Never mind.)", prompts.late_note(view))


if __name__ == "__main__":
    unittest.main()
