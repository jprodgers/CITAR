"""The chat clock (design/phase3-features.md P3.5.2; package 3-11's gates 1 and 2), in fake time.

The session times every open chat by the side it waits on (``citar/server/chat.py``): a model seat past the game's
``chat_model_reply_seconds`` declines, "(no reply in time)"; a person past ``chat_person_reply_seconds`` has the chat
closed as expired. A pause stops the clock, a reply starts it again, a load starts every chat afresh, and in a
benchmark game a chat whose answer waits behind a benchmark is not timed until the answer starts. The agents wait on
the clock (a bot's turn, a model's end_turn), keep a safety cap of twice the game's longest timeout, and the driver
starts the clock's thread again if it dies.

Every timeout here passes in fake time (``tests/chatfake.py``), except the thread's restart, which needs the real
thread.
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and registry; must be imported before citar)

import os
import shutil
import statistics
import threading
import time
import unittest
from unittest import mock

from citar import engine_api
from citar.agents.providers.base import Conversation, StepResult, ToolCall
from citar.server import chat as chat_mod
from citar.server.session import SAVE_DIR, SessionManager
from tests.chatfake import use

EAGER_FRIENDSHIP = {"diplo_every": 1, "friend_chance": 1.0, "friend_chance_aggr": 0.0, "friend_max_ratio": 1000.0}
NO_REPLY = chat_mod.NO_REPLY


def wait(cond, timeout: float = 10.0) -> bool:
    """Real time: until ``cond()`` holds, or ``timeout`` seconds."""
    t0 = time.monotonic()
    while time.monotonic() - t0 < timeout:
        if cond():
            return True
        time.sleep(0.01)
    return bool(cond())


class Hanging(Conversation):
    """A model that never answers: each step waits until its conversation is closed (a cancel)."""
    made: list = []

    def __init__(self, cfg, system, tools):
        self.cfg = cfg
        self.usage = {"input_tokens": 0, "output_tokens": 0}
        self.closed = threading.Event()
        Hanging.made.append(self)

    def add_user_text(self, text):
        pass

    def add_tool_results(self, results):
        pass

    def step(self):
        self.closed.wait(30)
        raise RuntimeError("cancelled")

    def close(self):
        self.closed.set()


class Opener(Conversation):
    """A model whose turn opens a chat with player 1 and then ends its turn; its chat answers reply."""

    def __init__(self, cfg, system, tools):
        self.cfg = cfg
        self.tools = {t["name"] for t in tools}
        self.usage = {"input_tokens": 1, "output_tokens": 1}
        self.steps = 0
        self.texts: list = []

    def add_user_text(self, text):
        self.texts.append(text)

    def add_tool_results(self, results):
        pass

    def step(self):
        self.steps += 1
        if "end_turn" not in self.tools:
            return StepResult(text="ok")
        if self.steps == 1:
            return StepResult(text="talk", tool_calls=[ToolCall("o", "open_negotiation", {
                "to": 1, "message": "Your map for mine?", "give": [{"type": "share_map"}],
                "receive": [{"type": "share_map"}]})])
        return StepResult(text="done", tool_calls=[ToolCall("e", "end_turn", {})])


class Base(unittest.TestCase):
    def setUp(self):
        self.manager = SessionManager()

    def tearDown(self):
        for sid in list(self.manager.sessions):
            self.manager.delete(sid)
            shutil.rmtree(SAVE_DIR / sid, ignore_errors=True)

    def game(self, seats: list, **config):
        """A game of ``seats``, everyone met, not started (no chat clock thread: the tests tick it in fake time)."""
        s = self.manager.create({"map_size": "small", "seed": 3, "barbarians": "off", "city_states": 0, **config},
                                [{"type": t} if isinstance(t, str) else t for t in seats], track=False, start=False)
        with s.lock:
            n = len(seats)
            s.game.apply_ops([{"op": "meet", "a": a, "b": b} for a in range(n) for b in range(a + 1, n)])
        return s

    @staticmethod
    def open(s, a: int, b: int, message: str = "Shall we talk?") -> int:
        """``a`` opens a chat with ``b``, whoever's turn it is (as a scenario's or a probe's counterparty does)."""
        with s.lock:
            return s.game.open_negotiation_as(a, b, message)["negotiation_id"]

    @staticmethod
    def answer(s, pid: int, nid: int, action: str = "reply", message: str = "Go on.") -> dict:
        return s.call_tool(pid, "respond_negotiation", {"negotiation_id": nid, "action": action, "message": message})

    @staticmethod
    def neg(s, nid: int) -> dict:
        return s.game.negotiation(nid)


class Timeouts(Base):
    """Gate 1: each kind of side's timeout, the pause, the restart on a reply, the defaults."""

    def test_a_chat_waiting_on_a_model_seat_is_declined_for_it_at_its_timeout(self):
        s = self.game(["human", "mcp"])                  # no chat keys: the defaults (600 s for a model)
        clock = use(s)
        nid = self.open(s, 0, 1)
        clock.advance(599)
        self.assertEqual((self.neg(s, nid)["status"], self.neg(s, nid)["awaiting"]), ("open", 1))
        clock.advance(1)
        n = self.neg(s, nid)
        last = n["history"][-1]
        self.assertEqual((n["status"], last["by"], last["action"], last["message"]), ("rejected", 1, "reject", NO_REPLY))
        rows = [r for r in s.metrics.data["negotiations"] if r["nid"] == nid]
        self.assertEqual([(r["player"], r["outcome"], r["cause"]) for r in rows], [(1, "declined_late", "timeout")])

    def test_a_model_answer_still_running_at_the_deadline_is_cancelled_then_declined(self):
        Hanging.made = []
        with mock.patch("citar.agents.llm_agent.make_conversation", Hanging):
            s = self.game(["human", {"type": "llm", "llm": {"provider": "mock"}}], chat_model_reply_seconds=120)
            clock = use(s)
            res = s.call_tool(0, "open_negotiation", {"to": 1, "message": "Peace?"})     # dispatches the responder
            nid = res["result"]["negotiation_id"]
            self.assertTrue(wait(lambda: Hanging.made), "the model's answer is under way")
            clock.advance(120)
            self.assertTrue(wait(lambda: not s._answering), "the cancelled answer returned")
        n = self.neg(s, nid)
        self.assertEqual(n["status"], "rejected")
        self.assertEqual([h["message"] for h in n["history"] if h["action"] == "reject"], [NO_REPLY],
                         "declined once, by the clock: never '(no response)'")
        self.assertTrue(Hanging.made[0].closed.is_set(), "the answer's request was cancelled first")

    def test_a_chat_waiting_on_a_person_closes_as_expired_whoever_opened_it(self):
        s = self.game(["human", "human", "mcp"])
        clock = use(s)
        people = self.open(s, 0, 1)                      # a person waiting on a person
        model = self.open(s, 2, 0)                       # an AI waiting on a person
        clock.advance(299)
        self.assertEqual({self.neg(s, people)["status"], self.neg(s, model)["status"]}, {"open"})
        clock.advance(1)
        for nid in (people, model):
            n = self.neg(s, nid)
            self.assertEqual((n["status"], n["history"][-1]["action"], n["history"][-1]["note"]),
                             ("expired", "close", NO_REPLY), n)

    def test_a_ten_minute_pause_does_not_expire_a_five_minute_chat(self):
        s = self.game(["human", "human"])
        clock = use(s)
        nid = self.open(s, 0, 1)
        clock.advance(100)
        s.set_paused(True)
        clock.advance(600)
        self.assertEqual(self.neg(s, nid)["status"], "open", "the clock stands still while the game is paused")
        self.assertIsNone(s.chat.deadline_of(nid)["deadline"], "no deadline is told while paused")
        s.set_paused(False)
        self.assertAlmostEqual(s.chat.deadline_of(nid)["left_s"], 200.0, delta=0.5, msg="it carries on where it stood")
        clock.advance(199)
        self.assertEqual(self.neg(s, nid)["status"], "open")
        clock.advance(1)
        self.assertEqual(self.neg(s, nid)["status"], "expired")

    def test_a_reply_restarts_the_clock(self):
        s = self.game(["human", "human"])
        clock = use(s)
        nid = self.open(s, 0, 1)
        clock.at(200, lambda: self.answer(s, 1, nid, "reply", "Tell me more."))
        clock.advance(200)
        self.assertEqual((self.neg(s, nid)["status"], self.neg(s, nid)["awaiting"]), ("open", 0))
        clock.advance(299)                              # 499: the first clock would have run out at 300
        self.assertEqual(self.neg(s, nid)["status"], "open")
        clock.advance(1)
        self.assertEqual(self.neg(s, nid)["status"], "expired")

    def test_an_answer_landing_as_the_deadline_fires_leaves_one_outcome(self):
        # the answer first: the clock, due to act, finds the chat moved and does nothing
        s = self.game(["human", "human"])
        clock = use(s)
        nid = self.open(s, 0, 1, "Friends?")
        real = s.expire_chat

        def answer_then_expire(n, entries):
            self.answer(s, 1, n, "reject", "No.")
            return real(n, entries)
        with mock.patch.object(s, "expire_chat", answer_then_expire):
            clock.advance(300)
        n = self.neg(s, nid)
        self.assertEqual((n["status"], [h["action"] for h in n["history"]]), ("rejected", ["open", "reject"]))
        # the clock first: the late answer is refused by the engine, and the chat stays expired
        s2 = self.game(["human", "human"])
        clock2 = use(s2)
        nid2 = self.open(s2, 0, 1, "Friends?")
        clock2.advance(300)
        late = self.answer(s2, 1, nid2, "reject", "No.")
        n2 = self.neg(s2, nid2)
        self.assertFalse(late["ok"])
        self.assertEqual((n2["status"], [h["action"] for h in n2["history"]]), ("expired", ["open", "close"]))

    def test_a_game_without_the_keys_uses_the_defaults_and_one_with_them_its_own(self):
        s = self.game(["human", "human"])
        cfg = s.info()["config"]
        self.assertEqual((cfg["chat_model_reply_seconds"], cfg["chat_person_reply_seconds"], cfg["max_chat_messages"]),
                         (600.0, 300.0, 30))
        self.assertEqual((s.chat.timeout("model"), s.chat.timeout("person"), s.chat.timeout("bot")), (600.0, 300.0, None))
        self.assertEqual(s.chat.safety_cap(), 1200.0)
        s2 = self.game(["human", "human"], chat_model_reply_seconds=90, chat_person_reply_seconds=45,
                       diplomacy={"max_chat_messages": 6})
        cfg2 = s2.info()["config"]
        self.assertEqual((cfg2["chat_model_reply_seconds"], cfg2["chat_person_reply_seconds"],
                          cfg2["max_chat_messages"]), (90.0, 45.0, 6))
        self.assertEqual(s2.game.max_chat_messages(), 6, "the engine enforces the lobby's cap")
        clock = use(s2)
        nid = self.open(s2, 0, 1)
        clock.advance(45)
        self.assertEqual(self.neg(s2, nid)["status"], "expired")

    def test_the_message_cap_reported_is_the_one_the_engine_plays(self):
        """A game that sets no cap plays its ruleset's (a modded ruleset may have its own): the game reports that one,
        not the shipped default."""
        s = self.game(["human", "human"])
        with mock.patch.object(s.game, "max_chat_messages", return_value=12):
            s.chat._settings = None
            self.assertEqual(s.info()["config"]["max_chat_messages"], 12)
        self.assertEqual(chat_mod.settings({}, 12)["max_chat_messages"], 12)
        self.assertEqual(chat_mod.settings({"diplomacy": {"max_chat_messages": 6}}, 9)["max_chat_messages"], 9,
                         "the engine's word is the cap")

    def test_an_action_that_failed_at_the_deadline_is_tried_again(self):
        """A person-to-person chat has no agent's safety cap: an expiry that failed (an error, then the engine's
        refusal) is tried again a little later, until the chat closes."""
        s = self.game(["human", "human"])
        clock = use(s)
        nid = self.open(s, 0, 1)
        real = s.expire_chat
        tries = []

        def flaky(n, entries):
            tries.append(clock.now())
            if len(tries) == 1:
                raise RuntimeError("the database is busy")
            if len(tries) == 2:
                return False                                # as the engine refusing it would
            return real(n, entries)
        with mock.patch.object(s, "expire_chat", flaky), self.assertLogs("citar.chat", level="ERROR"):
            clock.advance(300)
            self.assertEqual(self.neg(s, nid)["status"], "open")
            clock.advance(10)
        self.assertEqual(self.neg(s, nid)["status"], "expired", tries)
        self.assertEqual(tries, [300.0, 301.0, 303.0], "tried again after 1 s, then 2 s")
        self.assertTrue(any(e.get("where") == "chat clock" for e in s.errors))

    def test_a_chat_the_engine_will_not_close_is_tried_again_at_most_every_minute(self):
        s = self.game(["human", "human"])
        clock = use(s)
        nid = self.open(s, 0, 1)
        tries = []
        with mock.patch.object(s, "expire_chat", lambda n, entries: tries.append(clock.now()) or False):
            clock.advance(300 + 600, step=1.0)
        gaps = [b - a for a, b in zip(tries, tries[1:])]
        self.assertEqual(gaps[:7], [1.0, 2.0, 4.0, 8.0, 16.0, 32.0, 60.0])
        self.assertTrue(all(g == 60.0 for g in gaps[6:]), gaps)
        self.assertEqual(self.neg(s, nid)["status"], "open")

    def test_the_settings_are_read_and_held_within_bounds(self):
        st = chat_mod.settings
        self.assertEqual(st(None), {"chat_model_reply_seconds": 600.0, "chat_person_reply_seconds": 300.0,
                                    "max_chat_messages": 30})
        odd = st({"chat_model_reply_seconds": "soon", "chat_person_reply_seconds": True,
                  "diplomacy": {"max_chat_messages": "many"}})
        self.assertEqual(odd, st(None))
        bounded = st({"chat_model_reply_seconds": -5, "chat_person_reply_seconds": 10 ** 9,
                      "diplomacy": {"max_chat_messages": 1}})
        self.assertEqual((bounded["chat_model_reply_seconds"], bounded["chat_person_reply_seconds"],
                          bounded["max_chat_messages"]), (chat_mod.MIN_REPLY_SECONDS, chat_mod.MAX_REPLY_SECONDS, 2))
        self.assertEqual(st({"chat_model_reply_seconds": float("nan")})["chat_model_reply_seconds"], 600.0)

    def test_a_chat_the_clock_has_not_looked_at_yet_is_timed_when_asked(self):
        """Where a chat's clock stands is never "not timed" for want of a look: asked about a chat it has not seen, the
        clock looks first (an answer's first message tells its real deadline)."""
        s = self.game(["human", "mcp"])
        use(s)
        nid = self.open(s, 0, 1)
        s.chat.chats.clear()                            # as before the clock's first look at a loaded game
        timing = s.chat.deadline_of(nid)
        self.assertEqual((timing["waiting_on"], timing["kind"], timing["left_s"]), (1, "model", 600.0))
        self.assertEqual(s.chat.deadline_of(nid + 99), {}, "a chat the game does not have")

    def test_a_bot_is_never_timed(self):
        s = self.game(["human", "bot"])
        clock = use(s)
        with mock.patch.object(s, "_dispatch_negotiation_interrupts"):      # nobody answers for the bot
            nid = self.open(s, 0, 1)
            clock.advance(5000)
        self.assertEqual(self.neg(s, nid)["status"], "open")
        self.assertEqual(s.chat.deadline_of(nid)["kind"], "bot")
        self.assertIsNone(s.chat.deadline_of(nid)["deadline"])


class BenchmarkQueue(Base):
    """Gate 1: in a benchmark game a chat whose answer is a prompt queued behind a benchmark is not timed until the
    prompt starts, then counts from its start; in a lobby game the queue counts (the owner's rule)."""

    def _machine(self):
        from citar.pool import scheduler as S
        sched = S.Scheduler(settings=lambda sid: S.MachineSettings(known=True, slots=1, name="box"),
                            background=False)
        return sched

    def _ticket_waiting_behind_a_benchmark(self, s, nid: int, sched):
        """A benchmark lease live on the machine and the chat's answer, a ticket that waits for benchmarks, queued
        behind it, its status told to the chat as a seat's answer tells it."""
        bench = sched.lease("box", "m", kind="benchmark")
        ticket = sched.ticket("box", "m", cls="chat", wait_for_benchmark=True,
                              on_status=lambda payload: s.chat.answer_status(nid, 1, payload))
        m = sched.machine("box")
        request = m.submit(ticket)
        sched.deliver()
        return bench, ticket, m, request

    def test_a_chat_behind_a_benchmark_in_a_benchmark_game_counts_from_the_prompts_start(self):
        s = self.game(["human", "mcp"], chat_model_reply_seconds=600)
        s.benchmark = {"run_id": "r1", "job_id": "j1"}
        clock = use(s)
        nid = self.open(s, 0, 1)
        sched = self._machine()
        bench, ticket, m, request = self._ticket_waiting_behind_a_benchmark(s, nid, sched)
        self.assertEqual(request.state, "waiting")
        self.assertEqual(s.chat.deadline_of(nid).get("held"), "benchmark")
        clock.advance(1800)                               # half an hour behind the benchmark
        self.assertEqual(self.neg(s, nid)["status"], "open", "not timed while it waits behind the benchmark")
        bench.release()                                    # the benchmark is done: the prompt starts
        self.assertIsNotNone(m.take(request))
        sched.deliver()
        self.assertNotIn("held", s.chat.deadline_of(nid))
        clock.advance(599)
        self.assertEqual(self.neg(s, nid)["status"], "open", "counted from the prompt's start")
        clock.advance(1)
        self.assertEqual(self.neg(s, nid)["status"], "rejected")
        ticket.close()

    def test_in_a_lobby_game_the_queue_counts(self):
        s = self.game(["human", "mcp"], chat_model_reply_seconds=600)
        clock = use(s)
        nid = self.open(s, 0, 1)
        sched = self._machine()
        _bench, ticket, _m, _request = self._ticket_waiting_behind_a_benchmark(s, nid, sched)
        self.assertNotIn("held", s.chat.deadline_of(nid))
        clock.advance(600)
        self.assertEqual(self.neg(s, nid)["status"], "rejected")
        ticket.cancel()


class AnswerStatuses(Base):
    """The clock's looks never put a ``waiting`` over the answering side's own status: a model still generating stays
    ``thinking`` (a queued answer ``queued``) for both sides, which the chat window shows (P3.5.6, package 3-12), until
    its answer ends (``done``: ``waiting`` again), the chat moves, or a pause changes the deadline."""

    def test_thinking_stands_through_the_clocks_looks_until_the_answer_ends(self):
        s = self.game(["human", "mcp"])
        clock = use(s)
        heard = []
        s.subscribers.append(lambda msg: heard.append(msg) if msg.get("type") == "negotiation_status" else None)
        nid = self.open(s, 0, 1)
        states = lambda: [(m["state"], m["waiting_on"], m["paused"]) for m in heard if m["negotiation"] == nid]  # noqa: E731
        clock.advance(2)
        s.chat.answer_status(nid, 1, {"state": "queued", "position": 2, "eta_s": 30.0})
        clock.advance(5)
        s.chat.answer_status(nid, 1, {"state": "thinking"})
        clock.advance(20)                                     # twenty looks while the model generates
        self.assertEqual(states(), [("waiting", 1, False), ("queued", 1, False), ("thinking", 1, False)])
        s.chat.answer_status(nid, 1, {"state": "done"})
        clock.advance(3)
        self.assertEqual(states()[3:], [("waiting", 1, False)], "the answer ended: waiting, once")
        s.set_paused(True)
        s.set_paused(False)
        clock.advance(2)
        self.assertEqual(states()[4:], [("waiting", 1, True), ("waiting", 1, False)], "a pause tells the deadline again")
        self.answer(s, 1, nid)
        clock.advance(2)
        self.assertEqual(states()[6:], [("waiting", 0, False)], "the chat moved: it waits on the other side")


class AfterALoad(Base):
    """Gate 1: after a load every open chat's clock starts afresh."""

    def test_every_open_chat_starts_afresh_after_a_load(self):
        s = self.game(["human", "human", "human"])
        clock = use(s)
        a = self.open(s, 0, 1)
        b = self.open(s, 2, 0)
        clock.advance(250)                               # 50 s left on both
        path = s.save("before-load")
        loaded = self.manager.load(path)                  # stops the running game, comes back paused
        loaded.chat.stop()                                # its clock's thread: this test ticks it in fake time
        loaded.chat._thread.join(5)
        clock2 = use(loaded)
        loaded.set_paused(False)
        self.assertAlmostEqual(loaded.chat.deadline_of(a)["left_s"], 300.0, delta=0.5)
        clock2.advance(299)
        self.assertEqual({loaded.game.negotiation(a)["status"], loaded.game.negotiation(b)["status"]}, {"open"})
        clock2.advance(1)
        self.assertEqual({loaded.game.negotiation(a)["status"], loaded.game.negotiation(b)["status"]}, {"expired"})


class Waits(Base):
    """Gate 2: the agents' turns wait for the chats they opened until they move or the clock closes them; the
    clock's thread is started again by the driver; with the clock stopped for good the safety cap ends the turn."""

    def _bot_opener(self, s):
        """Seat 0's bot, eager to offer friendship to whoever it has met."""
        agent = s.get_agent(0)
        agent.bot = engine_api.bot_instance("basic", params=EAGER_FRIENDSHIP)
        return agent

    def test_a_bots_turn_takes_a_persons_answer_after_two_hundred_seconds(self):
        s = self.game(["bot", "human"])
        clock = use(s)
        agent = self._bot_opener(s)
        answered = {}

        def person():
            heads = [n for n in s.game.open_negotiation_heads(1) if n["awaiting"] == 1]
            answered["at"] = clock.now()
            for n in heads:
                answered["res"] = self.answer(s, 1, n["id"], "accept", "Gladly.")
        clock.at(200, person)
        with mock.patch.object(s, "_dispatch_negotiation_interrupts"):
            agent.play_turn(s, 0)
        self.assertEqual(answered.get("at"), 200)
        self.assertTrue(answered["res"]["ok"], answered)
        chats = s.game.negotiations(0)
        self.assertEqual([n["status"] for n in chats], ["accepted"], "the answer after 200 s was taken")
        self.assertNotEqual(s.game.current, 0, "and the turn ended")
        self.assertLess(clock.now(), 210)

    def test_a_bots_turn_ends_when_the_clock_closes_a_chat_nobody_answers(self):
        s = self.game(["bot", "human"])
        clock = use(s)
        agent = self._bot_opener(s)
        with mock.patch.object(s, "_dispatch_negotiation_interrupts"):
            agent.play_turn(s, 0)
        n = s.game.negotiations(0)[0]
        self.assertEqual((n["status"], n["history"][-1]["note"]), ("expired", NO_REPLY))
        self.assertAlmostEqual(clock.now(), 300, delta=2, msg="at the person's timeout")
        self.assertNotEqual(s.game.current, 0)

    def test_a_model_seats_turn_waits_the_same_way(self):
        with mock.patch("citar.agents.llm_agent.make_conversation", Opener):
            s = self.game([{"type": "llm", "llm": {"provider": "mock"}}, "human"])
            clock = use(s)
            agent = s.get_agent(0)
            agent._clock = clock.now
            taken = {}

            def person():
                for n in s.game.open_negotiation_heads(1):
                    taken["res"] = self.answer(s, 1, n["id"], "accept", "Agreed.")
            clock.at(200, person)
            agent.play_turn(s, 0)
        self.assertTrue(taken["res"]["ok"], taken)
        self.assertEqual([n["status"] for n in s.game.negotiations(0)], ["accepted"])
        self.assertNotEqual(s.game.current, 0, "the turn ended by end_turn")
        rec = next(r for r in s.metrics.data["turns"] if r["player"] == 0)
        self.assertEqual(rec["end_reason"], "end_turn")
        self.assertGreaterEqual(agent._waited, 199, "the chat's wait is not the model's time")

    def test_a_model_seats_turn_ends_at_the_timeout_when_the_person_never_answers(self):
        with mock.patch("citar.agents.llm_agent.make_conversation", Opener):
            s = self.game([{"type": "llm", "llm": {"provider": "mock"}}, "human"])
            clock = use(s)
            agent = s.get_agent(0)
            agent._clock = clock.now
            agent.play_turn(s, 0)
        n = s.game.negotiations(0)[0]
        self.assertEqual((n["status"], n["history"][-1]["note"]), ("expired", NO_REPLY))
        self.assertAlmostEqual(clock.now(), 300, delta=2)
        self.assertNotEqual(s.game.current, 0)

    def test_with_the_clock_stopped_for_good_the_safety_cap_ends_the_turn(self):
        s = self.game(["bot", "human"])
        clock = use(s, ticking=False)                    # the chat clock never looks
        agent = self._bot_opener(s)
        with mock.patch.object(s, "_dispatch_negotiation_interrupts"):
            agent.play_turn(s, 0)
        n = s.game.negotiations(0)[0]
        self.assertEqual((n["status"], n["history"][-1]["note"]), ("expired", NO_REPLY))
        self.assertAlmostEqual(clock.now(), s.chat.safety_cap(), delta=2, msg="twice the longest timeout")
        self.assertNotEqual(s.game.current, 0)

    def test_the_model_seats_safety_cap_too(self):
        with mock.patch("citar.agents.llm_agent.make_conversation", Opener):
            s = self.game([{"type": "llm", "llm": {"provider": "mock"}}, "human"])
            clock = use(s, ticking=False)
            agent = s.get_agent(0)
            agent._clock = clock.now
            agent.play_turn(s, 0)
        n = s.game.negotiations(0)[0]
        self.assertEqual((n["status"], n["history"][-1]["note"]), ("expired", NO_REPLY))
        self.assertGreaterEqual(clock.now(), s.chat.safety_cap() - 1)
        self.assertNotEqual(s.game.current, 0)

    def test_the_driver_starts_a_dead_clock_again_and_the_chat_still_closes(self):
        s = self.game(["human", "human"], chat_person_reply_seconds=1)
        s.start()
        self.assertTrue(wait(s.chat.alive))
        with self.assertLogs("citar.chat", level="WARNING") as logged:
            with mock.patch.object(s.chat, "tick", side_effect=SystemExit("killed")):
                self.assertTrue(wait(lambda: not s.chat.alive()), "the clock's thread died")
            self.assertTrue(wait(s.chat.alive, 5), "the driver started it again at its next look")
        self.assertTrue(any("starting it again" in line for line in logged.output))
        nid = self.open(s, 0, 1)
        self.assertTrue(wait(lambda: self.neg(s, nid)["status"] == "expired", 10), self.neg(s, nid))

    def test_the_clocks_loop_survives_an_exception(self):
        s = self.game(["human", "human"], chat_person_reply_seconds=1)
        real = s.chat.tick
        calls = {"n": 0}

        def flaky():
            calls["n"] += 1
            if calls["n"] == 1:
                raise RuntimeError("a clock on fire")
            return real()
        with mock.patch.object(s.chat, "tick", flaky), self.assertLogs("citar.chat", level="ERROR"):
            s.start()
            self.assertTrue(wait(lambda: calls["n"] >= 2, 10))
        self.assertTrue(any(e.get("where") == "chat clock" for e in s.errors))
        nid = self.open(s, 0, 1)
        self.assertTrue(wait(lambda: self.neg(s, nid)["status"] == "expired", 10))


class Performance(Base):
    """P3.9.7: with 20 open chats, one tick of the chat clock takes under 10 ms (the median of 100 ticks)."""

    def test_a_tick_with_twenty_open_chats(self):
        s = self.game(["human"] * 7)                      # 21 pairs
        clock = use(s)
        pairs = [(a, b) for a in range(7) for b in range(a + 1, 7)][:20]
        for a, b in pairs:
            self.open(s, a, b)
        self.assertEqual(len(s.game.open_negotiation_heads()), 20)
        times = []
        for _ in range(100):
            clock.t += 0.5
            t0 = time.perf_counter()
            s.chat.tick()
            times.append(time.perf_counter() - t0)
        median = statistics.median(times)
        print(f"\nthe chat clock's tick with 20 open chats: median {median * 1000:.3f} ms, "
              f"max {max(times) * 1000:.3f} ms", flush=True)
        self.assertLess(median, 0.030 if os.environ.get("CI") else 0.010)


if __name__ == "__main__":
    unittest.main()
