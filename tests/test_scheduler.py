"""The prompt scheduler's rules (citar/pool/scheduler.py; design/phase3-features.md P3.3.2-P3.3.4, P3.3.9), package
3-07's gates 1, 2, 6 and 7 and the scheduler's step overhead (P3.9.7).

Most of it drives a machine's non-blocking core (``submit``, ``take``, ``finish``, ``step_ticket``) on a fake clock,
so a schedule of hundreds of events runs in milliseconds and every rule is checked as an invariant at every grant
(``AuditedMachine``) and after every event (``audit``). The threaded parts (the busy refusal, the locks, the step
overhead) use the real blocking layer. Gates 3, 4 and 5 (a real helper, the dry-run machine, metering) are in
``tests/test_scheduler_e2e.py``.
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and registry; must be imported before citar)

import copy
import random
import statistics
import threading
import time
import types
import unittest
from datetime import datetime, timedelta
from unittest import mock

from citar.agents.providers.base import Conversation, StepResult, ToolCall
from citar.pool import scheduler as S


class FakeClock:
    """A clock the test moves."""

    def __init__(self, t: float = 10_000.0):
        self.t = t

    def __call__(self) -> float:
        return self.t

    def advance(self, dt: float) -> None:
        self.t += dt


class AuditedMachine(S.Machine):
    """A machine that checks P3.3.3's rules at every grant and every ticket step."""

    def __init__(self, *a, **kw):
        super().__init__(*a, **kw)
        self.grants: list = []          # (binding, request seq, clock) in grant order

    def _grant(self, r, now, slot=True):
        b = r.binding
        if slot:
            cap = self.capacity()
            assert cap is not None and len(self.slots) + 1 <= cap, "rule 1: a grant over capacity"
            if b.cap and b.consumer is not None:
                assert self._running(b.consumer) < b.cap, "a consumer over its grant's cap"
            if b.is_lease:
                for w in self.waiting:
                    assert not (w.binding.is_lease and w.seq < r.seq and not self._at_cap(w.binding)), \
                        "rule 2: a lease request served before an older one"
            else:
                assert not any(w.binding.is_lease and not self._at_cap(w.binding) for w in self.waiting), \
                    "rule 2: a ticket started while a lease request waits"
                assert not self._bench_live(), "rule 3a: a ticket started while a benchmark lease is live"
                assert len(self.slots) + 1 <= cap - self._reserved(), "rule 4: a ticket took a reserved slot"
        super()._grant(r, now, slot)
        self.grants.append((b, r.seq, now))

    def step_ticket(self, ticket):
        grant = super().step_ticket(ticket)
        if ticket.borrowing:
            assert self.sched.clock() - ticket.started <= S.BORROW_SECONDS[ticket.ticket_class], \
                "rule 5: a borrowing ticket stepped past its class's cap"
        return grant


def audit(m: S.Machine) -> None:
    """The invariants that hold between events."""
    with m.lock:
        assert len(m.slots) <= m.settings.slots, "rule 1: in flight above capacity"
        tickets = [s for s in m.slots if not s.binding.is_lease]
        for s in m.slots:
            lease = s.binding
            if lease.is_lease and lease.kind == "benchmark" and s.requesting and tickets:
                assert lease.contaminated, "a ticket request in flight during a benchmark step, uncontaminated"
        for consumer in {s.binding.consumer for s in m.slots}:
            caps = [s.binding.cap for s in m.slots if s.binding.consumer == consumer and s.binding.cap]
            if consumer is not None and caps:
                assert m._running(consumer) <= min(caps), "a consumer over its grant's cap"


def make(clock=None, *, audited: bool = True, drain: float = S.BENCHMARK_DRAIN_SECONDS,
         helper: dict | None = None, **settings):
    """A scheduler on a fake clock with one kind of machine, no background thread."""
    clock = clock or FakeClock()
    base = {"known": True, "enabled": True, "reach": "direct", "provider": "dryrun", "name": "box", "slots": 2}
    base.update(settings)
    st = S.MachineSettings(**base)
    state = helper or {"online": True, "helper_slots": None, "helper_loaded": frozenset(), "contexts": {}}
    sched = S.Scheduler(clock=clock, wall=clock, settings=lambda sid: copy.deepcopy(st),
                        helper=lambda sid: dict(state), drain_seconds=drain, background=False)
    if audited:
        sched.machine_class = AuditedMachine
    return sched, clock


def granted(m, r):
    """``r``'s grant, None while it waits (raises why it ended)."""
    return m.take(r)


# ---------------------------------------------------------------------------- gate 1: random schedules

class RandomSchedules(unittest.TestCase):
    """500 seeded random schedules of lease requests (lobby and benchmark), tickets, cancels with v1 draining, step
    times and benchmark leases appearing mid-schedule, with the rules checked after every event."""

    SEEDS = 500
    EVENTS = 120

    def test_the_rules_hold_over_500_random_schedules(self):
        stats = {"grants": 0, "tickets": 0, "drained": 0, "late": 0, "benchmarks": 0, "contaminated": 0,
                 "yielded": 0, "busy": 0, "expired": 0}
        for seed in range(self.SEEDS):
            with self.subTest(seed=seed):
                self._run(seed, stats)
        # the schedules exercised what they were meant to
        for k in ("grants", "tickets", "drained", "late", "benchmarks", "busy", "expired"):
            self.assertGreater(stats[k], 0, k)

    def _run(self, seed: int, stats: dict) -> None:
        rng = random.Random(seed)
        sched, clock = make(slots=rng.choice([1, 2, 3]), allow_swaps=rng.random() < 0.3,
                            loaded=frozenset({"m1"}) if rng.random() < 0.7 else None,
                            drain=rng.choice([30.0, 120.0]))
        m = sched.machine("box")
        consumers = ["a", "b", "c", None]
        caps = {c: rng.choice([0, 0, 1, 2]) for c in consumers if c}
        leases = [sched.lease("box", "m1", kind="game", consumer=c, cap=caps.get(c, 0))
                  for c in rng.sample(consumers, rng.randint(1, 3))]
        pending: list = []          # (binding, request)
        inflight: list = []         # (binding, grant)
        draining: list = []         # request ids
        tickets: list = []
        bench = None

        def collect():
            for b, r in list(pending):
                try:
                    g = granted(m, r)
                except S.SchedulerError as e:
                    pending.remove((b, r))
                    if isinstance(e, S.TicketExpired):
                        stats["expired"] += 1
                    continue
                if g is not None:
                    pending.remove((b, r))
                    inflight.append((b, g))
                    stats["grants"] += 1

        def end(b, g, *, unfinished: bool):
            if unfinished:
                m.finish(g, unfinished=(g.request_id, rng.uniform(0.0, 90.0)))
                draining.append(g.request_id)
                stats["drained"] += 1
            else:
                m.finish(g, gen_s=rng.uniform(1.0, 30.0))

        def ticket_inflight(t):
            return next(((b, g) for b, g in inflight if b is t), None)

        for _ in range(self.EVENTS):
            op = rng.random()
            live = [lease for lease in leases if not lease.released]
            if op < 0.18 and live:                                   # a lease request
                lease = rng.choice(live)
                if sum(1 for b, _ in pending + inflight if b is lease) < 2:
                    pending.append((lease, m.submit(lease)))
            elif op < 0.26 and live:                                 # a seat starts or stops its turn
                lease = rng.choice(live)
                lease.activate() if rng.random() < 0.5 else lease.idle()
            elif op < 0.42 and inflight:                             # a call ends: answered, or abandoned
                b, g = inflight.pop(rng.randrange(len(inflight)))
                end(b, g, unfinished=rng.random() < 0.2)
            elif op < 0.47 and draining:                             # a late frame
                m.late_frame(draining.pop(rng.randrange(len(draining))))
                stats["late"] += 1
            elif op < 0.60:                                          # a new ticket
                c = rng.choice(consumers)
                t = sched.ticket("box", rng.choice(["m1", "m1", "m2"]), cls=rng.choice(S.TICKET_CLASSES),
                                 consumer=c, priority=rng.randint(-5, 5), cap=caps.get(c, 0) if c else 0,
                                 start_within=rng.choice([None, 20.0, 300.0]),
                                 wait_for_benchmark=rng.random() < 0.3, steps=rng.randint(1, 4))
                stats["tickets"] += 1
                if t.state == "queued":
                    tickets.append(t)
                    pending.append((t, m.submit(t)))
            elif op < 0.70:                                          # a running ticket's next step, or its end
                ready = [t for t in tickets if t.state == "running" and ticket_inflight(t) is None]
                if ready:
                    t = rng.choice(ready)
                    if rng.random() < 0.7:
                        try:
                            inflight.append((t, m.step_ticket(t)))
                        except S.SchedulerError as e:
                            stats["yielded"] += e.code == "yielded"
                    else:
                        t.close()
            elif op < 0.75 and tickets:                              # a ticket cancelled
                t = rng.choice(tickets)
                m.cancel_ticket(t, "cancelled")
                hit = ticket_inflight(t)
                if hit is not None:                                  # its conversation closes: its request ends
                    inflight.remove(hit)
                    end(*hit, unfinished=rng.random() < 0.6)
            elif op < 0.80 and inflight:                             # a busy refusal anyway
                hit = rng.choice(inflight)
                inflight.remove(hit)
                b, g = hit
                m.busy(g)
                pending.append((b, g.request))
                stats["busy"] += 1
            elif op < 0.83 and bench is None and _ > 15:             # a benchmark lease becomes live
                c = rng.choice(consumers)
                bench = sched.lease("box", "m1", kind="benchmark", consumer=c, cap=caps.get(c, 0) if c else 0)
                leases.append(bench)
                stats["benchmarks"] += 1
                for b, g in list(inflight):                          # its cancelled tickets' requests end
                    if not b.is_lease and b.state == "cancelled":
                        inflight.remove((b, g))
                        end(b, g, unfinished=rng.random() < 0.6)
            elif op < 0.85 and live and rng.random() < 0.3:          # a seat released
                rng.choice(live).release()
            else:                                                    # time passes
                clock.advance(rng.choice([0.5, 2.0, 10.0, 40.0, 130.0]))
                sched.tick()
            collect()
            audit(m)
            if bench is not None and bench.contaminated:
                stats["contaminated"] += 1

        # nothing starves: with everything finished and time passing, every lease request is served
        if bench is not None:
            bench.release()
        for _ in range(100):
            for b, g in list(inflight):
                inflight.remove((b, g))
                end(b, g, unfinished=False)
            for t in tickets:
                if t.state == "running":
                    t.close()
            clock.advance(200.0)
            sched.tick()
            collect()
            audit(m)
            if not any(b.is_lease and not b.released for b, _ in pending):
                break
        self.assertFalse([b for b, _ in pending if b.is_lease and not b.released], "a lease request starved")


# ---------------------------------------------------------------------------- gate 1: constructed cases

class Rules(unittest.TestCase):

    def test_priority_then_round_robin_then_fifo(self):
        sched, clock = make(slots=1)
        m = sched.machine("box")

        def finish(t, r):
            """The ticket's one step answered, and the ticket closed: its slot goes to the next."""
            m.finish(granted(m, r), gen_s=1)
            t.close()
        first = sched.ticket("box", "m", cls="chat", consumer="a")
        finish(first, m.submit(first))                      # a was served, at this time
        clock.advance(5)
        holder = sched.ticket("box", "m", cls="test", consumer="z")
        current = (holder, m.submit(holder))
        self.assertEqual(holder.state, "running")
        order = {}
        for name, cls, consumer in (("t1", "report", "a"), ("t2", "chat", "b"), ("t3", "chat", "a"),
                                    ("t4", "chat", "c"), ("t5", "chat", "b")):
            t = sched.ticket("box", "m", cls=cls, consumer=consumer)
            order[name] = (t, m.submit(t))
        started = []
        for _ in range(5):
            clock.advance(1)
            finish(*current)
            name = next(name for name, (t, r) in order.items() if t.state == "running")
            started.append(name)
            current = order[name]
        # chat (20) before report (-10); among chats, b and c never served before a (served earlier), b before c by
        # arrival; then b again only after a, who was served longest ago
        self.assertEqual(started, ["t2", "t4", "t3", "t5", "t1"])

    def test_an_idle_lobby_lease_lends_its_slot_and_gets_it_back_within_one_capped_step(self):
        sched, clock = make(slots=1)
        m = sched.machine("box")
        lease = sched.lease("box", "m", kind="game", consumer="owner")
        lease.idle()
        t = sched.ticket("box", "m", cls="report", consumer="reader")
        g = granted(m, m.submit(t))
        self.assertTrue(t.borrowing, "the ticket runs on the idle lease's slot")
        self.assertEqual(g.max_tokens, S.BORROW_MAX_TOKENS["report"], "a borrowing step's max_tokens is capped")
        lease.activate()                                    # the seat's turn comes
        r = m.submit(lease)
        self.assertIsNone(granted(m, r), "it waits for the step in progress")
        clock.advance(40)                                   # the ticket's step ends
        m.finish(g, gen_s=40)
        with self.assertRaises(S.MachineRefused) as caught:
            m.step_ticket(t)                                # its next step is refused: the slot goes back
        self.assertEqual(caught.exception.code, "yielded")
        back = granted(m, r)
        self.assertIsNotNone(back)
        self.assertEqual(back.queue_s, 40, "the seat waited one capped step")
        # with nobody waiting, a borrowing ticket still stops at its class's time cap
        m.finish(back, gen_s=1)
        lease.idle()
        t2 = sched.ticket("box", "m", cls="test", consumer="reader", steps=20)
        g2 = granted(m, m.submit(t2))
        self.assertTrue(t2.borrowing)
        m.finish(g2, gen_s=1)
        clock.advance(S.BORROW_SECONDS["test"] + 1)
        with self.assertRaises(S.MachineRefused) as caught:
            m.step_ticket(t2)
        self.assertEqual(caught.exception.code, "borrow_cap")
        self.assertEqual(len(m.slots), 0)

    def test_a_ticket_that_does_not_start_in_time_expires(self):
        sched, clock = make(slots=1)
        m = sched.machine("box")
        lease = sched.lease("box", "m", kind="game")
        held = granted(m, m.submit(lease))
        t = sched.ticket("box", "m", cls="chat", consumer="a", start_within=30)
        r = m.submit(t)
        clock.advance(31)
        sched.tick()
        with self.assertRaises(S.TicketExpired):
            granted(m, r)
        self.assertEqual(t.state, "expired")
        m.finish(held, gen_s=1)
        self.assertEqual(len(m.slots), 0, "nothing started after its deadline")

    def test_cancelling_a_queued_and_a_running_ticket(self):
        sched, clock = make(slots=1)
        m = sched.machine("box")
        running = sched.ticket("box", "m", cls="report", consumer="a")
        g = granted(m, m.submit(running))
        queued = sched.ticket("box", "m", cls="chat", consumer="b")
        r = m.submit(queued)
        queued.cancel()
        with self.assertRaises(S.Cancelled):
            granted(m, r)
        self.assertEqual(queued.state, "cancelled")
        running.cancel()                                    # its conversation closes; on a helper the request drains
        self.assertEqual(running.state, "cancelled")
        m.finish(g, unfinished=(g.request_id, 60.0))
        self.assertTrue(m.slots and m.slots[0].draining, "the abandoned request keeps its slot while it drains")
        lease = sched.lease("box", "m", kind="game")
        waiting = m.submit(lease)
        self.assertIsNone(granted(m, waiting), "the next request waits for the drain")
        self.assertTrue(m.late_frame(g.request_id), "the helper's late answer frees the slot")
        got = granted(m, waiting)
        self.assertIsNotNone(got)
        # a drain whose late frame never comes ends at the request's timeout plus the grace
        m.finish(got, unfinished=("never", 30.0))
        r2 = m.submit(lease)
        self.assertIsNone(granted(m, r2))
        clock.advance(31)
        sched.tick()
        self.assertIsNotNone(granted(m, r2))

    def test_a_late_frame_that_comes_before_the_caller_marks_the_drain(self):
        sched, clock = make(slots=1)
        m = sched.machine("box")
        lease = sched.lease("box", "m", kind="game")
        g = granted(m, m.submit(lease))
        m.late_frame(g.request_id)                          # the helper answered while the caller was unwinding
        m.finish(g, unfinished=(g.request_id, 60.0))
        self.assertEqual(m.slots, [], "the slot is free at once")

    def test_a_model_not_loaded_is_refused_unless_swaps_are_allowed_or_a_lease_uses_it(self):
        sched, _ = make(loaded=frozenset({"loaded-model"}))
        refused = sched.ticket("box", "other-model", cls="chat", consumer="a")
        self.assertEqual((refused.state, refused.error.code), ("refused", "not_loaded"))
        self.assertEqual(sched.ticket("box", "loaded-model", cls="chat", consumer="a").state, "queued")
        sched.lease("box", "other-model", kind="game")
        self.assertEqual(sched.ticket("box", "other-model", cls="chat", consumer="a").state, "queued",
                         "a live lease uses that model")
        swaps, _ = make(loaded=frozenset({"loaded-model"}), allow_swaps=True)
        self.assertEqual(swaps.ticket("box", "other-model", cls="chat", consumer="a").state, "queued")
        helper, _ = make(reach="worker", helper={"online": True, "helper_slots": 2,
                                                 "helper_loaded": frozenset({"on-gpu"}), "contexts": {}})
        self.assertEqual(helper.ticket("box", "on-gpu", cls="chat").state, "queued")
        self.assertEqual(helper.ticket("box", "on-disk", cls="chat").error.code, "not_loaded",
                         "a helper's reported loaded flags decide")

    def test_quiet_hours_refuse_a_ticket_with_their_end(self):
        until = datetime.now() + timedelta(hours=2)
        sched, _ = make(quiet_until=until)
        t = sched.ticket("box", "m", cls="report", consumer="a")
        self.assertEqual((t.state, t.error.code, t.retry_at), ("refused", "quiet_hours", until))
        self.assertTrue(t.error.temporary)

    def test_the_eta_rises_with_position(self):
        sched, clock = make(slots=1)
        m = sched.machine("box")
        holder = sched.lease("box", "m", kind="game")
        granted(m, m.submit(holder))
        seen: dict = {}
        tickets = []
        for i in range(4):
            t = sched.ticket("box", "m", cls="report", consumer=f"c{i}",
                             on_status=lambda p, i=i: seen.__setitem__(i, p))
            tickets.append(t)
            m.submit(t)
        sched.deliver()
        positions = [seen[i]["position"] for i in range(4)]
        etas = [seen[i]["eta_s"] for i in range(4)]
        self.assertEqual(positions, [1, 2, 3, 4])
        self.assertEqual(etas, sorted(etas))
        self.assertTrue(all(b > a for a, b in zip(etas, etas[1:])), etas)

    def test_a_consumer_capped_at_one_never_has_two_requests_running(self):
        sched, clock = make(slots=3)
        m = sched.machine("box")
        mine = sched.lease("box", "m", kind="game", consumer="a", cap=1)
        g = granted(m, m.submit(mine))
        self.assertIsNotNone(g)
        second = m.submit(mine)
        ticket = sched.ticket("box", "m", cls="chat", consumer="a", cap=1)
        r = m.submit(ticket)
        other = sched.ticket("box", "m", cls="chat", consumer="b")
        o = m.submit(other)
        self.assertIsNone(granted(m, second), "her second request waits")
        self.assertIsNone(granted(m, r), "and so does her ticket")
        self.assertIsNotNone(granted(m, o), "another member's goes past them")
        self.assertEqual(m._running("a"), 1)
        m.finish(g, gen_s=1)
        self.assertIsNotNone(granted(m, second), "her lease request first (rule 2)")
        self.assertIsNone(granted(m, r))
        self.assertEqual(m._running("a"), 1)

    def test_a_reserved_slot_is_kept_from_tickets_but_not_from_lease_requests(self):
        sched, _ = make(slots=2)
        m = sched.machine("box")
        a = sched.lease("box", "m", kind="game", consumer="a")
        b = sched.lease("box", "m", kind="game", consumer="b")
        a.activate()                                        # mid-turn, between its steps
        g = granted(m, m.submit(b))                         # another seat's request: one slot
        t = sched.ticket("box", "m", cls="test", consumer="c")
        r = m.submit(t)
        self.assertIsNone(granted(m, r), "the active seat's slot is reserved from tickets")
        self.assertIsNotNone(granted(m, m.submit(b)), "a lease request may take it")
        m.finish(g, gen_s=1)
        self.assertIsNone(granted(m, r), "a slot is free, but the active seat's is still reserved")
        a.idle()
        self.assertIsNotNone(granted(m, r), "lent once the seat waits on something else")
        self.assertTrue(t.borrowing)

    def test_a_benchmark_lease_drains_pooled_requests_before_its_first_step(self):
        sched, clock = make(slots=2)
        m = sched.machine("box")
        running = sched.ticket("box", "m", cls="report", consumer="a")
        g = granted(m, m.submit(running))
        queued = sched.ticket("box", "m", cls="chat", consumer="b")
        sched.lease("box", "m", kind="game").activate()
        q = m.submit(queued)
        patient = sched.ticket("box", "m", cls="probe", consumer="c", wait_for_benchmark=True)
        p = m.submit(patient)
        bench = sched.lease("box", "m", kind="benchmark", consumer="admin")
        with self.assertRaises(S.MachineRefused) as caught:
            granted(m, q)
        self.assertEqual(caught.exception.code, "benchmark", "a queued ticket that does not wait is refused")
        self.assertEqual(running.state, "cancelled", "a running one is cancelled")
        self.assertEqual(sched.ticket("box", "m", cls="chat").error.code, "benchmark", "a new one is refused")
        m.finish(g, unfinished=(g.request_id, 300.0))      # its request drains on the helper
        first = m.submit(bench)
        clock.advance(25)
        sched.tick()
        self.assertIsNone(granted(m, first), "the benchmark's first request waits for the drain")
        m.late_frame(g.request_id)
        got = granted(m, first)
        self.assertIsNotNone(got)
        self.assertEqual((bench.drain_s, bench.contaminated), (25, False))
        self.assertIsNone(granted(m, p), "a ticket that waits for benchmarks waits while one is live")
        m.finish(got, gen_s=5)
        bench.release()
        self.assertIsNotNone(granted(m, p), "and starts once it is gone")

    def test_a_drain_cut_short_by_its_bound_marks_the_benchmark_contaminated(self):
        sched, clock = make(slots=2, drain=120.0)
        m = sched.machine("box")
        t = sched.ticket("box", "m", cls="report", consumer="a")
        g = granted(m, m.submit(t))
        bench = sched.lease("box", "m", kind="benchmark")
        m.finish(g, unfinished=(g.request_id, 900.0))      # a helper that never answers
        first = m.submit(bench)
        clock.advance(121)
        sched.tick()
        self.assertIsNotNone(granted(m, first), "the bound reached, it goes ahead")
        self.assertTrue(bench.contaminated)
        self.assertEqual(bench.drain_s, 121)

    def test_a_disabled_machine_refuses_everything_and_an_offline_helper_lets_leases_through(self):
        sched, _ = make(enabled=False)
        m = sched.machine("box")
        lease = sched.lease("box", "m", kind="game")
        with self.assertRaises(S.MachineRefused) as caught:
            granted(m, m.submit(lease))
        self.assertEqual(caught.exception.code, "disabled")
        self.assertEqual(sched.ticket("box", "m", cls="chat").error.code, "disabled")
        state = {"online": False, "helper_slots": None, "helper_loaded": frozenset(), "contexts": {}}
        helper, _ = make(reach="worker", slots=1, helper=state)
        hm = helper.machine("box")
        hl = helper.lease("box", "m", kind="game")
        g1, g2 = granted(hm, hm.submit(hl)), granted(hm, hm.submit(hl))
        self.assertTrue(g1.pass_through and g2.pass_through, "capacity unknown: the provider's error decides")
        offline = helper.ticket("box", "m", cls="chat")
        self.assertEqual(offline.error.code, "offline")
        self.assertTrue(offline.error.temporary)
        hm.apply_helper({"online": True, "helper_slots": 1, "helper_loaded": frozenset({"m"})})
        g3 = granted(hm, hm.submit(hl))
        self.assertFalse(g3.pass_through)
        self.assertIsNone(granted(hm, hm.submit(hl)), "online: its one slot counts")

    def test_the_helpers_own_ceiling_caps_the_rows_slots(self):
        sched, _ = make(reach="worker", slots=4, helper={"online": True, "helper_slots": 2,
                                                         "helper_loaded": frozenset(), "contexts": {}})
        m = sched.machine("box")
        self.assertEqual(m.capacity(), 2)

    def test_a_busy_refusal_holds_the_capacity_until_a_frame_frees_a_slot(self):
        sched, clock = make(slots=3)
        m = sched.machine("box")
        lease = sched.lease("box", "m", kind="game")
        g1, g2, g3 = (granted(m, m.submit(lease)) for _ in range(3))
        m.busy(g3)                                          # the helper had only two slots after all
        self.assertEqual(m.capacity(), 2)
        self.assertIsNone(granted(m, g3.request), "back at the head of the queue")
        m.finish(g1, gen_s=1)                               # a frame frees a slot: the capacity comes back
        self.assertIsNone(m.busy_cap)
        self.assertIsNotNone(granted(m, g3.request))
        # with no frame at all the lowering lapses after a while (no sleep: a waiter's tick, or the scheduler's)
        g4 = granted(m, m.submit(lease))
        m.busy(g4)
        clock.advance(S.BUSY_HOLD_SECONDS + 0.1)
        sched.tick()
        self.assertIsNone(m.busy_cap)

    def test_a_ticket_uses_at_most_its_steps(self):
        sched, _ = make(slots=1)
        m = sched.machine("box")
        t = sched.ticket("box", "m", cls="diplomat", consumer="a", steps=2)
        g = granted(m, m.submit(t))
        m.finish(g, gen_s=1)
        m.finish(m.step_ticket(t), gen_s=1)
        with self.assertRaises(S.MachineRefused) as caught:
            m.step_ticket(t)
        self.assertEqual(caught.exception.code, "steps")
        self.assertEqual(m.slots, [])


# ---------------------------------------------------------------------------- the conversation wrapper

class Inner(Conversation):
    """A provider's conversation stand-in: ``script`` is a list of callables or exceptions, one per step."""

    def __init__(self, script, *, step_s: float = 0.0):
        self.script = list(script)
        self.usage = {"input_tokens": 0, "output_tokens": 0, "reasoning_tokens": 0}
        self.cfg: dict = {"provider": "worker", "model": "m"}
        self.calls = 0
        self.step_s = step_s
        self.closed = False

    def add_user_text(self, text):
        pass

    def add_tool_results(self, results):
        pass

    def step(self):
        self.calls += 1
        item = self.script.pop(0)
        if isinstance(item, BaseException):
            raise item
        if self.step_s:
            time.sleep(self.step_s)
        self.usage["input_tokens"] += 100
        self.usage["output_tokens"] += 10
        return item() if callable(item) else StepResult(text="ok")

    def close(self):
        self.closed = True


class Busy(ConnectionError):
    """What a helper's ``busy`` refusal reaches the wrapper as (``WorkerConnectionError(refusal="busy")``)."""
    refusal = "busy"


class NoBusyWait(unittest.TestCase):
    """Gate 2: a fake hub answering ``busy`` three times makes no ``time.sleep`` call, meters exactly one request,
    and completes it when a frame frees a slot."""

    def test_busy_three_times_then_done_with_no_sleep(self):
        sched = S.Scheduler(settings=lambda sid: S.MachineSettings(known=True, slots=4, name="box"),
                            background=False)
        events = []
        sched.add_listener(events.append)
        m = sched.machine("box")
        others = sched.lease("box", "m", kind="game", consumer="o")
        held = [m.take(m.submit(others)) for _ in range(3)]      # three requests in flight
        lease = sched.lease("box", "m", kind="game", consumer="me")
        inner = Inner([Busy("busy (3/3)"), Busy("busy (2/2)"), Busy("busy (1/1)"), lambda: StepResult(text="done")])
        conv = S.ScheduledConversation(inner, lease)
        sleeps, out = [], {}
        real_sleep = time.sleep
        worker = threading.current_thread()

        def recording_sleep(seconds):
            if threading.current_thread() is not worker:
                sleeps.append(seconds)
            return real_sleep(seconds)

        def run():
            out["result"] = conv.step()

        with mock.patch("time.sleep", recording_sleep):
            t = threading.Thread(target=run)
            t.start()
            for expected in (1, 2, 3):
                deadline = time.monotonic() + 5
                while m.counters["busy_refusals"] < expected and time.monotonic() < deadline:
                    real_sleep(0.005)
                self.assertEqual(m.counters["busy_refusals"], expected)
                self.assertEqual(m.capacity(), 4 - expected, "held at what else is in flight")
                m.finish(held[expected - 1], gen_s=1)            # a frame frees a slot
            t.join(5)
        self.assertFalse(t.is_alive())
        self.assertEqual(out["result"].text, "done")
        self.assertEqual(sleeps, [], "the request waited on the machine's condition, never in a sleep")
        self.assertEqual(inner.calls, 4)
        self.assertEqual(len(events), 1, "one request metered")
        self.assertEqual(events[0].usage["input_tokens"], 100)
        self.assertGreater(conv.last_queue_s, 0)
        self.assertEqual(m.slots, [])


class Wrapper(unittest.TestCase):

    def test_the_request_timeout_is_set_once_the_slot_is_granted(self):
        clock = FakeClock()
        sched, _ = make(clock, slots=1, audited=False)
        m = sched.machine("box")
        other = sched.lease("box", "m", kind="game")
        held = m.take(m.submit(other))
        lease = sched.lease("box", "m", kind="game")
        inner = Inner([lambda: StepResult(text="ok")])
        conv = S.ScheduledConversation(inner, lease)
        seen = []
        conv.budget = lambda queued: seen.append(queued) or 600.0 + queued
        t = threading.Thread(target=conv.step)
        t.start()
        time.sleep(0.05)
        clock.advance(120)                                  # queued two minutes
        m.finish(held, gen_s=1)
        t.join(5)
        self.assertEqual(seen, [120.0])
        self.assertEqual(inner.request_timeout, 720.0, "the queue did not shorten the request's own time")
        self.assertEqual(conv.last_queue_s, 120.0)

    def test_closing_a_waiting_conversation_cancels_its_request(self):
        sched = S.Scheduler(settings=lambda sid: S.MachineSettings(known=True, slots=1, name="box"),
                            background=False)
        m = sched.machine("box")
        m.take(m.submit(sched.lease("box", "m", kind="game")))
        conv = S.ScheduledConversation(Inner([]), sched.lease("box", "m", kind="game"))
        out = {}

        def run():
            try:
                conv.step()
            except S.Cancelled as e:
                out["e"] = e
        t = threading.Thread(target=run)
        t.start()
        time.sleep(0.05)
        t0 = time.perf_counter()
        conv.close()
        t.join(5)
        self.assertIn("e", out)
        self.assertLess(time.perf_counter() - t0, 1.5)
        self.assertEqual(m.waiting, [])

    def test_tokens_are_capped_and_an_unscheduled_binding_still_meters(self):
        sched = S.Scheduler(settings=lambda sid: S.MachineSettings(known=True, slots=1, name="box"),
                            background=False)
        events = []
        sched.add_listener(events.append)
        inner = Inner([lambda: StepResult(text="ok")])
        inner.cfg["max_tokens"] = 5
        conv = S.ScheduledConversation(inner, sched.lease(None, "m", kind="game"))
        conv.step()
        self.assertEqual(events[0].usage["output_tokens"], 5)
        self.assertTrue(events[0].flags.get("capped_tokens"))
        self.assertIsNone(events[0].machine)


# ---------------------------------------------------------------------------- gate 7: locks

def _session(seats=None):
    """A real game session, not started, on a small map. Close it with ``_close``."""
    from citar.server.session import SessionManager
    manager = SessionManager()
    s = manager.create({"map_size": "duel", "seed": 7, "barbarians": "off"},
                       seats or [{"type": "llm", "llm": {"provider": "dryrun", "dry_run_delay": 30}},
                                 {"type": "bot"}], track=False, start=False)
    s.test_manager = manager
    return s


def _close(s) -> None:
    """Close a game as the lobby does: stopped, and its live mark removed, so no later test's server restores it."""
    s.test_manager.delete(s.id)


class Locks(unittest.TestCase):

    def setUp(self):
        S.reset()
        self.addCleanup(S.reset)

    def test_acquire_under_a_session_lock_raises_in_test_mode(self):
        self.assertTrue(S.test_locks(), "the suite runs with CITAR_TEST_LOCKS")
        s = _session()
        sched = S.Scheduler(settings=lambda sid: S.MachineSettings(known=True, slots=1, name="box"),
                            background=False)
        lease = sched.lease("box", "m", kind="game")
        with s.lock:
            with self.assertRaises(AssertionError) as caught:
                lease.acquire()
        self.assertIn("session lock", str(caught.exception))
        self.assertIsNotNone(lease.acquire(), "and fine without it")
        _close(s)

    def _blocked_agent(self, s, sched):
        """The seat's agent with a conversation waiting for its machine (another holds the one slot)."""
        m = sched.machine("sv_gpu")
        holder = sched.lease("sv_gpu", "m", kind="game")
        m.take(m.submit(holder))
        agent = s.get_agent(0)
        self.assertIs(agent.binding.machine, m)
        conv = agent._conversation()
        agent._active.add(conv)
        out = {}

        def run():
            try:
                conv.step()
            except Exception as e:
                out["e"] = e
        t = threading.Thread(target=run, daemon=True)
        t.start()
        deadline = time.monotonic() + 5
        while not m.waiting and time.monotonic() < deadline:
            time.sleep(0.01)
        self.assertTrue(m.waiting, "the seat's request waits for the machine")
        return t, out, m

    def test_suspend_update_seat_and_a_crash_cancel_a_waiting_conversation_under_the_lock_without_blocking(self):
        sched = S.get()
        for how in ("suspend", "update_seat", "crash"):
            with self.subTest(how=how):
                s = _session([{"type": "llm", "llm": {"server_id": "sv_gpu", "model": "dry-run"}}, {"type": "bot"}])
                t, out, m = self._blocked_agent(s, sched)
                t0 = time.perf_counter()
                with s.lock:
                    if how == "suspend":
                        s.suspend({"kind": "test"})
                    elif how == "update_seat":
                        s.update_seat(0, llm={"server_id": "sv_dryrun", "model": "dry-run"})
                    else:
                        s._crashed("a test's crash")
                took = time.perf_counter() - t0
                t.join(5)
                self.assertLess(took, 0.5, f"{how} blocked under the lock for {took:.2f} s")
                self.assertFalse(t.is_alive())
                self.assertIsInstance(out.get("e"), S.Cancelled)
                self.assertEqual([r for r in m.waiting if r.binding.server_id == "sv_gpu" and r.binding in
                                  s.leases.values()], [])
                _close(s)
                for lease in list(m.leases):
                    lease.release()

    def test_a_status_callback_taking_a_session_lock_never_meets_a_thread_holding_it_and_waiting_for_the_machine(self):
        s = _session()
        sched = S.Scheduler(settings=lambda sid: S.MachineSettings(known=True, slots=1, name="box"))
        self.addCleanup(sched.stop)
        m = sched.machine("box")
        holder = sched.lease("box", "m", kind="game")
        g = m.take(m.submit(holder))
        delivered = threading.Event()

        def on_status(payload):
            with s.lock:                                    # a callback that takes the game's lock
                delivered.set()
        t = sched.ticket("box", "m", cls="chat", on_status=on_status)
        lease = sched.lease("box", "m", kind="game")
        done = threading.Event()

        def holds_the_game():
            with s.lock:
                m.submit(t)                                 # queues a status callback (under the machine lock)
                time.sleep(0.2)                             # the notifier now wants the game's lock...
                for _ in range(50):
                    lease.activate()                        # ...while this thread wants the machine's
                    lease.idle()
            done.set()
        worker = threading.Thread(target=holds_the_game)
        worker.start()
        self.assertTrue(done.wait(5), "the thread holding the game's lock got the machine's")
        self.assertTrue(delivered.wait(5), "the callback ran once the game's lock was free")
        worker.join(5)
        m.finish(g, gen_s=1)
        _close(s)

    def test_the_notifier_survives_a_failing_callback_and_is_started_again(self):
        sched = S.Scheduler(settings=lambda sid: S.MachineSettings(known=True, slots=1, name="box"))
        self.addCleanup(sched.stop)
        got = threading.Event()
        with self.assertLogs("citar.scheduler", level="ERROR"):
            sched._notify(lambda p: 1 / 0, None)
            sched._notify(lambda p: got.set(), None)
            self.assertTrue(got.wait(5))
        dead = sched._thread
        sched._stop.set()
        dead.join(5)
        sched._stop.clear()
        again = threading.Event()
        sched._notify(lambda p: again.set(), None)
        self.assertTrue(again.wait(5), "a stopped notifier is started again")


# ---------------------------------------------------------------------------- gate 6: time accounting

class TimeAccounting(unittest.TestCase):
    """A turn with 25 minutes of queueing and chat waits and one minute of generation ends by ``end_turn``, not
    ``time_limit``, though its limit is 20 minutes (fake clock)."""

    def test_queueing_and_chat_waits_never_count_against_the_turn(self):
        from citar.agents.llm_agent import LLMAgent
        clock = FakeClock(1_000_000.0)
        s = _session([{"type": "llm", "llm": {"provider": "dryrun"}}, {"type": "human"}])
        with s.lock:
            s.game.meet(0, 1)

        class Queued:
            """A lease whose every request queues three minutes on the fake clock."""
            is_lease, task_class, cls, machine, server_id = True, "full", "game", None, None
            model, activity, consumer = "m", None, None
            sched = types.SimpleNamespace(clock=clock, wall=clock, emit=lambda e: None)

            def acquire(self, conv=None, halted=None):
                clock.advance(180)
                return S.Grant(None, "rid", 180.0, pass_through=True)

            def finish(self, *a, **kw):
                pass

            def activate(self):
                pass

            def idle(self):
                pass

        steps = [
            [ToolCall("c1", "open_negotiation", {"to": 1, "message": "maps?", "give": [{"type": "share_map"}],
                                                 "receive": [{"type": "share_map"}]})],
            [ToolCall("c2", "get_empire", {})], [ToolCall("c3", "log_thought", {"text": "waiting"})],
            [ToolCall("c4", "get_players", {})], [ToolCall("c5", "end_turn", {})]]

        class Scripted(Conversation):
            def __init__(self, cfg, system, tools):
                self.usage = {"input_tokens": 0, "output_tokens": 0}
                self.cfg = cfg

            def add_user_text(self, text):
                pass

            def add_tool_results(self, results):
                pass

            def step(self):
                clock.advance(12)                           # 5 steps: one minute of generation
                return StepResult(text="", tool_calls=steps.pop(0))

        def chat_wait(pid, nid, timeout, halted=None, hold_paused=False):
            clock.advance(600)                              # ten minutes waiting for the person
            return 600.0
        s._await_reply = chat_wait
        agent = LLMAgent({"provider": "dryrun", "max_turn_seconds": 1200}, binding=Queued())
        agent._clock = clock
        s.agents[0] = agent
        t0 = clock()
        with mock.patch("citar.agents.llm_agent.make_conversation", Scripted):
            agent.play_turn(s, 0)
        rec = next(r for r in s.metrics.data["turns"] if r["player"] == 0)
        self.assertEqual(clock() - t0, 5 * 180 + 600 + 60, "25 minutes waited, one generated")
        self.assertEqual(rec["end_reason"], "end_turn")
        self.assertEqual(rec["queue_s"], 900.0)
        self.assertEqual(rec["model_s"], 60.0, "the model's own time: generation only")
        _close(s)


# ---------------------------------------------------------------------------- gate 5's unit part, P3.9.7

class Speed(unittest.TestCase):

    def test_a_model_with_long_queues_and_short_generation_scores_as_fast(self):
        from citar.server import scoring
        from citar.server.metrics import Metrics
        rows = {}
        for model, gen, queue in (("patient", 3.0, 1500.0), ("slow", 900.0, 0.0)):
            m = Metrics()
            m.begin_turn(0, 1, "llm", model)
            m.model_step(0, gen, queue_s=queue)
            m.end_turn(0)
            rows[model] = m.summary({0: {"name": "x", "controller": "llm", "model": model}})[0]
        self.assertEqual((rows["patient"]["avg_model_s"], rows["patient"]["avg_queue_s"]), (3.0, 1500.0))
        fast, slow = scoring.model_speed(rows["patient"]), scoring.model_speed(rows["slow"])
        self.assertEqual(fast, 100.0, "scored on its generation time, not its 25 minutes in the queue")
        self.assertLess(slow, 50.0)
        self.assertLess(scoring.speed_score(rows["patient"]["avg_turn_s"] + 1500.0), fast)


class StepOverhead(unittest.TestCase):
    """P3.9.7: under 1 ms per step with nothing queued (the median of 1,000 acquire-release pairs)."""

    def test_an_acquire_and_release_with_nothing_queued_takes_under_a_millisecond(self):
        sched = S.Scheduler(settings=lambda sid: S.MachineSettings(known=True, slots=4, name="box"),
                            background=False)
        lease = sched.lease("box", "m", kind="game", consumer="a")
        lease.activate()
        times = []
        for _ in range(1000):
            t0 = time.perf_counter()
            g = lease.acquire()
            lease.finish(g, gen_s=1.0)
            times.append(time.perf_counter() - t0)
        median = statistics.median(times)
        print(f"\nscheduler acquire-release: median {median * 1e6:.1f} us over 1000 pairs", flush=True)
        self.assertLess(median, 0.001)


if __name__ == "__main__":
    unittest.main()
