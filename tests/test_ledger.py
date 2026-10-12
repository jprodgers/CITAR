"""The compute ledger: events, credits, rollups, budgets and the ratio (design/phase3-features.md P3.4; package 3-10's
gates 1 to 7).

- **Who gave and who used** (gate 1): consumer and provider for each kind of activity (a lobby game, a benchmark run, a
  probe run, a report, a lab experiment, the server's own data collection, a machine's test) on a member's machine, the
  host and an online API; given and used per member equal a table worked out by hand; self-use (a member's own work on
  their own machine, the server's on its own) counts on neither side; credits are seconds times the class rate in force
  when written, an API step's its token cost times the rate; a rate change applies from then on and ``reprice``
  recomputes a period; an over-reporting helper's events carry the observed time as ``gen_s`` and the flag.
- **The host's CPU per unit of work** (gate 2): two games whose answers run on one thread in one window are each charged
  their own ``thread_time``; the sums equal what was measured; a driver thread's time outside its units is sampled
  without counting a unit twice.
- **Rollups** (gate 3): after random events over three days every rollup equals the sum of its events; the 24 h, 7 d,
  30 d and all-time totals equal the raw sums, before and after the pass, and a read in the current hour includes
  events not rolled up yet.
- **Budgets** (gate 4, fake clocks): a hard budget admits requests until spent plus reserved would pass it, then
  refuses with the period's end; on an online API the request that would pass it is refused, and every request when
  the model has no price; a lobby game pauses with the budget's reason and resumes when the period turns; a soft budget
  only warns; ``refresh_from_ledger`` rebuilds the entries the requests settled.
- **Costing** (gate 5): a synthetic ledger priced before and after the switch gives equal totals with no queue time,
  and less busy energy with it.
- **The ratio's visibility and the routes** (gate 6, P3.9.1): ``private`` gives another member 404 and an
  administrator the totals, ``server`` members, ``global`` visitors; ``/api/ledger/me`` is the caller's own; the
  hardware classes are administrators' (a member 404, a visitor 401).
- **Write volume** (gate 7): 10,000 events from 8 threads written in under 2 s with no session lock held; the writer
  drains on shutdown and survives a failed batch.
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and registry; must be imported before citar)

import json
import os
import random
import secrets
import shutil
import tempfile
import threading
import time
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest import mock

_TMP = Path(tempfile.mkdtemp(prefix="citar_ledger_"))
_URL = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from sqlalchemy import func, select

from citar import costing, db, items, ledger, pool, servers, settings, usage
from citar.agents.providers.base import Conversation, StepResult
from citar.auth import accounts, policy, sessions
from citar.db.models import (AccessGrant, Base, Budget, BudgetEntry, ComputeEvent, ComputeRollup, Game, HardwareClass,
                             Server, User)
from citar.pool import budgets
from citar.pool import scheduler as S
from citar.pool import seats as pool_seats

settings.reset()

DRY = {"connection": {"provider": "dryrun"}, "models": [{"key": "dry-run"}]}
UTC = timezone.utc


def tearDownModule():
    ledger.reset()                    # the process's ledger as a server has it, for the modules after this one
    db.dispose()
    shutil.rmtree(_TMP, ignore_errors=True)


def until(fn, timeout: float = 30.0, step: float = 0.05):
    deadline = time.perf_counter() + timeout
    while True:
        value = fn()
        if value or time.perf_counter() > deadline:
            return value
        time.sleep(step)


class FakeClock:
    """A clock the test moves (the scheduler's)."""

    def __init__(self, t: float = 10_000.0):
        self.t = t

    def __call__(self) -> float:
        return self.t

    def advance(self, dt: float) -> None:
        self.t += dt


class Scripted(Conversation):
    """A provider's conversation: each step takes ``seconds`` of the scheduler's fake clock, reports ``usage`` and,
    like a helper's, a reported and an observed time when given."""

    def __init__(self, clock, seconds=2.0, usage=None, reported=None, observed=None, model="dry-run", max_tokens=None):
        self.clock, self.seconds, self.per_step = clock, seconds, dict(usage or {})
        self.usage = {"input_tokens": 0, "output_tokens": 0}
        self.cfg = {"model": model, **({"max_tokens": max_tokens} if max_tokens else {})}
        self.messages = []
        self.served_model = model
        if reported is not None:
            self.last_reported_s = reported
        if observed is not None:
            self.last_observed_s = observed

    def add_user_text(self, text):
        pass

    def add_tool_results(self, results):
        pass

    def step(self):
        if self.clock is not None:
            self.clock.advance(self.seconds)
        for k, v in self.per_step.items():
            self.usage[k] = self.usage.get(k, 0) + v
        return StepResult(text="ok", tool_calls=[])


def event(machine, activity, consumer, gen=1.0, *, cls="game", task="full", usage=None, at=None, reported=None,
          observed=None, flags=None, model="dry-run"):
    """A scheduler's ``StepEvent`` as a listener receives it."""
    return S.StepEvent(machine=machine, model=model, activity=activity, consumer=consumer, task_class=task, cls=cls,
                       gen_s=gen, reported_s=reported, observed_s=observed, queue_s=0.0, usage=usage or {},
                       request_id=secrets.token_hex(6), at=at if at is not None else time.time(), flags=flags or {})


class World(unittest.TestCase):
    """A fresh database (the suite's machines seeded), a fresh scheduler, ledger and budget keeper, and: alice and bob,
    members who each own a dry-run machine (``sv_alice``, ``sv_bob``); bob's in his group, granted to alice and carol
    for games and pooled prompts; carol, a member with no machine; zara, an administrator. Each has a session for the
    routes."""

    PEOPLE = (("alice", "user"), ("bob", "user"), ("carol", "user"), ("zara", "admin"))
    BACKGROUND = False

    def setUp(self):
        # games other modules left playing feed the process's usage tracker, whose flush hands the ledger their CPU:
        # stopped (and the ledger made afresh) before this database is, so no stray event lands in it
        if usage._tracker is not None:
            usage._tracker.stop()
            usage._tracker = None
        self.led = ledger.reset(background=self.BACKGROUND)
        self.addCleanup(lambda: ledger.reset(background=False))
        db.dispose()
        db.configure(_URL)
        Base.metadata.drop_all(db.engine())
        db.create_all()
        policy.invalidate()
        S.reset()
        self.addCleanup(S.reset)
        self.now = datetime(2026, 10, 12, 15, 0, tzinfo=UTC)
        self.keeper = budgets.reset_keeper(clock=lambda: self.now)
        self.addCleanup(budgets.reset_keeper)
        self.ids, self.headers = {}, {}
        with db.session() as s:
            for handle, role in self.PEOPLE:
                user = accounts.create_user(s, handle=handle, email=f"{handle}@example.com",
                                            password="a long enough phrase", role=role, status="active",
                                            email_verified=True)
                user.max_concurrent_games = 20
                row, raw = sessions.start(s, user)
                self.headers[handle] = {"cookie": f"{sessions.COOKIE_NAME}={raw}",
                                        sessions.CSRF_HEADER: row.csrf_token, "origin": "https://citar.test"}
                self.ids[handle] = user.id
            bob = s.get(User, self.ids["bob"])
            group = pool.create_group(s, bob, name="Bob's", visibility="private")
            self.group = group.id
            for who in ("alice", "carol"):
                pool.create_grant(s, bob, group, subject_type="user", subject_handle=who, purposes=["game", "pooled"])
            s.add(Server(id="sv_bob", owner_id=bob.id, name="Bob box", kind="owned", provider="dryrun",
                         reach="direct", group_id=group.id, max_concurrent=1, config=json.loads(json.dumps(DRY))))
            s.add(Server(id="sv_alice", owner_id=self.ids["alice"], name="Alice box", kind="owned", provider="dryrun",
                         reach="direct", max_concurrent=1, config=json.loads(json.dumps(DRY))))
        servers.invalidate()
        self.headers["anon"] = {"origin": "https://citar.test"}

    def rate(self, class_id: str, rate: float) -> None:
        with db.session() as s:
            s.get(HardwareClass, class_id).rate = rate
        self.led.invalidate_classes()

    def write(self, *items_) -> int:
        """Write events (``StepEvent``s, or ``("cpu", activity, seconds)``) through the ledger now."""
        for it in items_:
            if isinstance(it, tuple):
                self.led.cpu(it[1], it[2], at=it[3] if len(it) > 3 else None)
            else:
                self.led.step(it)
        return self.led.drain()

    def events(self, **where) -> list:
        with db.session() as s:
            q = select(ComputeEvent)
            for k, v in where.items():
                q = q.where(getattr(ComputeEvent, k) == v)
            rows = list(s.scalars(q.order_by(ComputeEvent.id)))
            for r in rows:
                s.expunge(r)
            return rows

    def totals(self, since=None, now=None) -> dict:
        with db.session() as s:
            return ledger.totals(s, list(self.ids.values()), since, now=now)


# ---------------------------------------------------------------------------- gate 1: who gave and who used

class GivenAndUsed(World):
    def setUp(self):
        super().setUp()
        self.lab = _TMP / f"lab-{secrets.token_hex(3)}"
        self.lab.mkdir()
        self.addCleanup(shutil.rmtree, self.lab, True)
        a, b, z = self.ids["alice"], self.ids["bob"], self.ids["zara"]
        with db.session() as s:
            s.add(Game(id="g_lobby", owner_id=a, name="lobby", kind="game"))
            s.add(Game(id="g_bench", owner_id=a, name="bench", kind="benchmark",
                       meta={"item": ["benchmark_run", "br_1"]}))
            s.add(Game(id="g_coll", owner_id=z, name="collection", kind="benchmark",
                       meta={"item": ["benchmark_run", "br_c"]}))
            alice, bob, zara = s.get(User, a), s.get(User, b), s.get(User, z)
            items.register(s, "benchmark_run", "br_1", alice, "run", audit_row=False)
            items.register(s, "benchmark_run", "br_c", zara, "collection run", collection=True, audit_row=False)
            items.register(s, "probe_run", "pr_1", alice, "probe run", audit_row=False)
            items.register(s, "report", "rp_1", bob, "report", audit_row=False)
            items.register(s, "lab_experiment", "exp1", alice, "experiment", audit_row=False)
            # the site's machines belong to its first administrator after an import: still the server's in the ledger
            for sid in ("sv_api", "sv_host"):
                s.get(Server, sid).owner_id = z
        servers.invalidate()
        with db.session() as s:
            self.assertTrue(all(s.get(Server, sid).server_resource for sid in ("sv_api", "sv_host")))
        (self.lab / "exp1.jsonl").write_text(json.dumps(
            {"exp": "exp1", "i": 0, "cpu_s": 100.0, "seconds": 120, "finished": "2026-10-12T10:00:00+00:00"}) + "\n"
            + json.dumps({"exp": "exp1", "i": 1, "crash": "boom"}) + "\n", encoding="utf-8")

    def the_table(self):
        """Every kind of activity on every kind of machine, and what each must be (consumer, provider, kind, credits,
        self-use)."""
        a, b, c = self.ids["alice"], self.ids["bob"], self.ids["carol"]
        api = {"input_tokens": 1_000_000, "output_tokens": 100_000, "cache_read_input_tokens": 2_000_000}
        rows = [
            # (the event, consumer, provider, kind, activity kind, credits, self-use)
            (event("sv_bob", "game:g_lobby", a, 10.0), a, b, "gpu", "game", 10.0, False),
            (event("sv_alice", "game:g_lobby", a, 5.0), a, a, "gpu", "game", 5.0, True),
            (event("sv_gpu", "game:g_lobby", a, 7.0), a, None, "gpu", "game", 7.0, False),
            (("cpu", "game:g_lobby", 20.0), a, None, "cpu", "game", 1.0, False),
            (event("sv_bob", "game:g_bench", a, 3.0, cls="benchmark"), a, b, "gpu", "benchmark", 3.0, False),
            (event("sv_bob", "probe:pr_1", a, 2.0, cls="probe", task="partial"), a, b, "gpu", "probe", 2.0, False),
            # 1M input at $5, 100k output at $25, 2M cache reads at $0.50 per million: $8.50, at 1,000 credits a dollar
            (event("sv_api", "probe:pr_1", a, 4.0, cls="probe", task="partial", usage=api, model="claude-opus-5"),
             a, None, "api", "probe", 8500.0, False),
            (event("sv_alice", "report:rp_1", b, 4.0, cls="report", task="partial"), b, a, "gpu", "report", 4.0,
             False),
            (("cpu", "report:rp_1", 2.0), b, None, "cpu", "report", 0.1, False),
            (event("sv_bob", "game:g_coll", self.ids["carol"], 6.0, cls="benchmark"), None, b, "gpu", "benchmark",
             6.0, False),
            (event("sv_gpu", "game:g_coll", None, 9.0, cls="benchmark"), None, None, "gpu", "benchmark", 9.0, True),
            (("cpu", "game:g_coll", 40.0), None, None, "cpu", "benchmark", 2.0, True),
            (event("sv_bob", None, c, 1.0, cls="test", task="partial"), c, b, "gpu", "test", 1.0, False),
        ]
        return rows

    def test_consumer_provider_and_credits_for_every_activity_on_every_machine(self):
        table = self.the_table()
        self.write(*[row[0] for row in table])
        with db.session() as s:
            self.assertEqual(ledger.ingest_lab(s, self.led, self.lab), 1, "the lab's one finished game; the crash none")
            self.assertEqual(ledger.ingest_lab(s, self.led, self.lab), 0, "and only once")
            self.assertEqual(ledger.ingest_lab(s, None, self.lab), 0, "even when its file is read again")
        got = self.events()
        self.assertEqual(len(got), len(table) + 1)
        for (src, consumer, provider, kind, akind, credits, self_use), ev in zip(table, got):
            with self.subTest(activity=ev.activity, machine=ev.machine_id):
                self.assertEqual((ev.consumer_id, ev.provider_id, ev.kind, ev.activity_kind, ev.self_use),
                                 (consumer, provider, kind, akind, self_use))
                self.assertAlmostEqual(ev.credits, credits, places=6)
                self.assertEqual(ev.verification, "verified" if kind == "cpu" else "unverified")
                self.assertEqual(ev.task_class, "cpu" if kind == "cpu" else src.task_class)
        test = got[len(table) - 1]
        self.assertTrue(test.activity.startswith("test:"), "a machine's test is an activity of its own")
        lab = got[-1]
        self.assertEqual((lab.activity, lab.consumer_id, lab.provider_id, lab.kind, lab.cpu_core_s, lab.credits,
                          lab.machine_id, lab.request_id),
                         ("lab:exp1:0", self.ids["alice"], None, "cpu", 100.0, 5.0, "sv_host", "lab:exp1:0"))
        api = got[6]
        self.assertEqual((api.tokens_in, api.tokens_out, api.tokens_cache_read), (1_000_000, 100_000, 2_000_000))

        # given and used per member: the table by hand
        a, b, c, z = (self.ids[k] for k in ("alice", "bob", "carol", "zara"))
        want = {a: {"used": 10 + 7 + 1.0 + 3 + 2 + 8500 + 5, "given": 4.0},
                b: {"used": 4 + 0.1, "given": 10 + 3 + 2 + 6 + 1},
                c: {"used": 1.0, "given": 0.0}, z: {"used": 0.0, "given": 0.0}}
        for when in ("before the rollups", "after them"):
            t = self.totals()
            for uid, sides in want.items():
                for side, credits in sides.items():
                    with self.subTest(when=when, user=uid, side=side):
                        self.assertAlmostEqual(t[uid][side]["credits"], credits, places=6)
            with db.session() as s:
                ledger.rollup(s)
        t = self.totals()
        self.assertEqual(t[a]["used"]["by_kind"]["api"]["credits"], 8500.0)
        self.assertAlmostEqual(t[a]["used"]["cpu_s"], 20 + 100, places=6)
        self.assertAlmostEqual(t[a]["given"]["gpu_s"], 4.0)
        with db.session() as s:
            alice = s.get(User, a)
            mine = ledger.breakdown(s, alice, "all")
        self.assertEqual(mine["own_use"]["events"], 1, "her game on her own machine: on neither side")
        self.assertAlmostEqual(mine["own_use"]["credits"], 5.0)
        names = {m["name"] for m in mine["by_machine"]}
        self.assertIn("Bob box", names, "a machine she may use is named")
        self.assertIn("a machine of the site's", names)
        self.assertEqual({k["kind"] for k in mine["by_activity"]}, {"game", "benchmark", "probe", "report", "lab"})

    def test_a_rate_applies_from_then_on_and_reprice_recomputes_a_period(self):
        a = self.ids["alice"]
        start = datetime.now(UTC) - timedelta(seconds=5)
        self.write(event("sv_bob", "game:g_lobby", a, 10.0), event("sv_api", "game:g_lobby", a, 1.0,
                                                                   usage={"output_tokens": 1000}, model="claude-opus-5"))
        self.rate("hc_gpu", 2.0)
        self.rate("hc_api", 500.0)
        self.write(event("sv_bob", "game:g_lobby", a, 10.0))
        got = [e.credits for e in self.events()]
        self.assertEqual(got, [10.0, 25.0, 20.0], "the new GPU rate from the event after it on")
        with db.session() as s:
            ledger.rollup(s)
            before = ledger.totals(s, [a])[a]["used"]["credits"]
            result = ledger.reprice(s, start)
        self.assertEqual(result, {"events": 3, "changed": 2, "hours": 1})
        self.assertEqual([e.credits for e in self.events()], [20.0, 12.5, 20.0])
        self.assertEqual(before, 55.0)
        self.assertEqual(self.totals()[a]["used"]["credits"], 52.5)
        with db.session() as s:
            rolled = s.scalar(select(func.sum(ComputeRollup.credits)).where(ComputeRollup.user_id == a,
                                                                            ComputeRollup.side == "used"))
        self.assertEqual(rolled, 52.5, "the hour's rollups follow the repriced events")
        with db.session() as s:
            self.assertEqual(ledger.reprice(s, start)["changed"], 0, "a second reprice changes nothing")

    def test_an_account_the_database_does_not_hold_never_costs_the_batch(self):
        """A binding's consumer whose account is gone (work with no row of its own: a machine's test) is nobody: its
        event is kept on the server's side, and the batch's other events with it."""
        a = self.ids["alice"]
        self.assertEqual(self.write(event("sv_bob", None, "u_gone", 1.0, cls="test", task="partial"),
                                    event("sv_bob", "game:g_lobby", a, 2.0)), 2)
        ghost, kept = self.events()
        self.assertEqual((ghost.consumer_id, ghost.provider_id, ghost.self_use), (None, self.ids["bob"], False))
        self.assertEqual((kept.consumer_id, kept.credits), (a, 2.0))
        self.assertEqual(self.led.failures, 0)

    def test_an_api_model_with_no_price_is_credited_nothing_and_flagged(self):
        self.write(event("sv_api", "game:g_lobby", self.ids["alice"], 2.0, usage={"output_tokens": 500},
                         model="mystery-model"))
        ev = self.events()[0]
        self.assertEqual((ev.kind, ev.credits, ev.flags), ("api", 0.0, {"unpriced": True}))

    def test_an_overreporting_helpers_events_carry_the_observed_time(self):
        """Through the scheduler's conversation: the helper reports ten seconds, the server saw three; the event's
        ``gen_s`` (and its credits) is the observed time, and it carries both times and the flag."""
        clock = FakeClock()
        sched = S.Scheduler(clock=clock, wall=time.time, background=False)
        sched.add_listener(ledger.listener)
        lease = sched.lease("sv_bob", "dry-run", activity="game:g_lobby", consumer=self.ids["alice"])
        conv = S.ScheduledConversation(Scripted(clock, 3.0, reported=10.0, observed=3.0), lease)
        conv.step()
        honest = S.ScheduledConversation(Scripted(clock, 2.0, reported=1.9, observed=2.0), lease)
        honest.step()
        self.led.drain()
        over, ok = self.events()
        self.assertEqual((over.gen_s, over.reported_s, over.observed_s, over.credits, over.flags),
                         (3.0, 10.0, 3.0, 3.0, {"overreported": True}))
        self.assertEqual((ok.gen_s, ok.reported_s, ok.observed_s, ok.flags), (1.9, 1.9, 2.0, {}))
        lease.release()

    def test_a_machines_class_follows_its_hardware_unless_pinned(self):
        """A faster GPU class matched by name and memory prices a machine with that hardware, recomputed when its
        hardware changes; a pinned class stays; an API machine takes the API class."""
        with db.session() as s:
            s.add(HardwareClass(id="hc_fast", kind="gpu", name="Fast", rate=3.0, priority=10,
                                match={"gpu_name": "RTX 5090", "vram_gb_min": 24}))
            s.add(HardwareClass(id="hc_mid", kind="gpu", name="Mid", rate=1.5, priority=5, match={"vram_gb_min": 12}))
            s.add(HardwareClass(id="hc_epyc", kind="cpu", name="Epyc", rate=0.02, match={"cpu_name": "EPYC",
                                                                                          "cores_min": 32}))
        with db.session() as s:
            row = s.get(Server, "sv_bob")
            row.config = {**row.config, "hardware": {"gpus": [{"name": "NVIDIA GeForce RTX 5090", "vram_gb": 32}],
                                                     "cpu": {"model": "AMD EPYC 9654", "cores": 96}}}
        with db.session() as s:
            row = s.get(Server, "sv_bob")
            self.assertEqual((row.gpu_class_id, row.cpu_class_id), ("hc_fast", "hc_epyc"), "classified on its write")
        servers.invalidate()
        self.led.invalidate_classes()
        a = self.ids["alice"]
        self.write(event("sv_bob", "game:g_lobby", a, 2.0))
        with db.session() as s:
            row = s.get(Server, "sv_bob")
            row.config = {**row.config, "hardware": {"gpus": [{"name": "NVIDIA RTX 4070", "vram_gb": 12}]}}
        servers.invalidate()
        self.write(event("sv_bob", "game:g_lobby", a, 2.0))
        with db.session() as s:
            row = s.get(Server, "sv_bob")
            self.assertEqual(row.gpu_class_id, "hc_mid")
            row.gpu_class_id, row.class_override = "hc_fast", True          # an administrator pins it
            row.config = {**row.config, "hardware": {"gpus": [{"name": "GT 710", "vram_gb": 1}]}}
        servers.invalidate()
        self.write(event("sv_bob", "game:g_lobby", a, 2.0), event("sv_alice", "game:g_lobby", a, 2.0))
        self.assertEqual([e.credits for e in self.events()], [6.0, 3.0, 6.0, 2.0])
        with db.session() as s:
            self.assertEqual(s.get(Server, "sv_bob").gpu_class_id, "hc_fast", "pinned: the hardware does not move it")
            self.assertEqual(s.get(Server, "sv_api").gpu_class_id, "hc_api")


# ---------------------------------------------------------------------------- gate 2: CPU per unit of work

class CpuAttribution(World):
    def burn(self, seconds: float) -> float:
        """Use about ``seconds`` of this thread's CPU; returns what ``thread_time`` measured of it."""
        t0 = time.thread_time()
        x = 0
        while time.thread_time() - t0 < seconds:
            x += 1
        return time.thread_time() - t0

    def test_two_games_answers_on_one_thread_are_each_charged_their_own_time(self):
        """Two sessions' chat answers run on the same responder thread within one flush window, interleaved: each
        game is charged its own answers' ``thread_time``, and the two together are what the thread used."""
        from citar.server.session import SessionManager
        mgr = SessionManager()
        seats = [{"type": "human"}, {"type": "bot"}]
        a = mgr.create({"map_size": "duel", "seed": 11, "barbarians": "off"}, seats, "A", track=False, start=False)
        b = mgr.create({"map_size": "duel", "seed": 12, "barbarians": "off"}, seats, "B", track=False, start=False)
        self.addCleanup(mgr.delete, a.id)
        self.addCleanup(mgr.delete, b.id)
        tracker = usage.Tracker("test")
        measured = {a.cpu_activity: 0.0, b.cpu_activity: 0.0}

        class Answering:
            """A seat's agent whose answer burns some CPU and says how much."""
            def __init__(self, test, act, seconds):
                self.test, self.act, self.seconds = test, act, seconds

            def respond_negotiation(self, session, pid, nid):
                measured[self.act] += self.test.burn(self.seconds)

        a.agents[1] = Answering(self, a.cpu_activity, 0.12)
        b.agents[1] = Answering(self, b.cpu_activity, 0.25)
        total = {}

        def responder():
            t0 = time.thread_time()
            for s in (a, b, a, b, a):
                s._run_responder(1, 0, (0, 0))
            total["thread"] = time.thread_time() - t0
        with mock.patch.object(usage, "_tracker", tracker), \
                mock.patch.object(type(a), "_reject_if_unanswered", lambda *args: None):
            t = threading.Thread(target=responder)
            t.start()
            t.join()
        charged = dict(tracker.cpu_acc)
        tick = max(time.get_clock_info("thread_time").resolution, 0.001)
        for act in (a.cpu_activity, b.cpu_activity):
            with self.subTest(game=act):
                self.assertGreater(charged[act], 0.2)
                self.assertAlmostEqual(charged[act], measured[act], delta=4 * tick + 0.02)
        self.assertGreater(charged[b.cpu_activity], charged[a.cpu_activity], "0.75 s against 0.36 s")
        self.assertAlmostEqual(sum(charged.values()), total["thread"], delta=6 * tick + 0.03,
                               msg="the units add up to what the thread used")
        # the window's CPU goes to the compute ledger, one event a game
        with mock.patch.object(usage, "append", lambda *args: None):
            tracker.flush()
        self.led.drain()
        cpu = {e.activity: e.cpu_core_s for e in self.events(kind="cpu")}
        self.assertEqual(set(cpu), {a.cpu_activity, b.cpu_activity})
        for act, seconds in cpu.items():
            self.assertAlmostEqual(seconds, charged[act], places=5)

    def test_a_unit_inside_another_is_the_outer_ones(self):
        """An engine call inside a drive (or a chat answer) on one thread is part of the outer unit: charged once, to
        the outer unit's activity."""
        tracker = usage.Tracker("test")
        with mock.patch.object(usage, "_tracker", tracker):
            t0 = time.thread_time()
            with ledger.cpu_unit("game:outer"):
                self.burn(0.05)
                with ledger.cpu_unit("game:inner"):
                    self.burn(0.1)
            used = time.thread_time() - t0
        self.assertNotIn("game:inner", tracker.cpu_acc)
        tick = max(time.get_clock_info("thread_time").resolution, 0.001)
        self.assertAlmostEqual(tracker.cpu_acc["game:outer"], used, delta=2 * tick + 0.01)
        with mock.patch.object(usage, "_tracker", tracker), ledger.cpu_unit(None):
            self.burn(0.01)                                 # no activity: nothing charged
        self.assertEqual(set(tracker.cpu_acc), {"game:outer"})

    def test_a_units_time_is_never_sampled_again_on_its_driver_thread(self):
        """The five-second sampling of a game's driver thread charges only its time outside its units: a unit open at
        a sample leaves the thread for a later one, so a unit straddling a sample is counted once."""
        tracker = usage.Tracker("test")
        tid = 4242
        totals = {tid: 10.0}
        tracker._thread_times = lambda: dict(totals)
        tracker.holders["k"] = ("game:x", lambda: {"sv_host": {"threads": [tid]}})
        tracker._sample()                                   # first sight: the baseline
        totals[tid] = 11.0                                  # one second outside any unit
        tracker._sample()
        self.assertAlmostEqual(tracker.cpu_acc["game:x"], 1.0)
        tracker.unit_started(tid, 0.0)
        totals[tid] = 11.5                                  # half a unit by the next sample
        tracker._sample()
        self.assertAlmostEqual(tracker.cpu_acc["game:x"], 1.0, msg="left for later while a unit runs")
        totals[tid] = 13.25                                 # the unit used 2.0 in all; 0.25 outside it after
        tracker.unit_ended(tid, "game:x", 2.0)
        tracker._sample()
        self.assertAlmostEqual(tracker.cpu_acc["game:x"], 1.0 + 2.0 + 0.25)


# ---------------------------------------------------------------------------- gate 3: rollups

class Rollups(World):
    def test_rollups_and_totals_equal_the_raw_sums(self):
        rng = random.Random(310)
        a, b, c = self.ids["alice"], self.ids["bob"], self.ids["carol"]
        now = datetime.now(UTC)
        machines = [("sv_bob", b), ("sv_alice", a), ("sv_gpu", None), ("sv_api", None)]
        consumers = [a, b, c, None]
        made = []
        for i in range(600):
            sid, _owner = rng.choice(machines)
            at = (now - timedelta(seconds=rng.uniform(60, 3 * 86400))).timestamp()
            if rng.random() < 0.2:
                made.append(("cpu", rng.choice(["game:x", "game:y", "report:r"]), rng.uniform(0.1, 30), at))
            else:
                made.append(event(sid, rng.choice(["game:x", "game:y", "probe:p", None]), rng.choice(consumers),
                                  rng.uniform(0.01, 20), at=at, cls=rng.choice(["game", "test", "probe"]),
                                  usage={"input_tokens": rng.randint(0, 5000), "output_tokens": rng.randint(0, 900)},
                                  model="claude-opus-5" if sid == "sv_api" else "dry-run"))
        self.write(*made)
        with db.session() as s:
            # the cpu events name no consumer of their own: give a share of them one, so both sides have cpu work
            for ev in s.scalars(select(ComputeEvent).where(ComputeEvent.kind == "cpu")):
                if ev.id % 2:
                    ev.consumer_id, ev.self_use = rng.choice([a, b, c]), False
        self.assertGreater(len(self.events()), 550)

        def raw(since=None, upto=None) -> dict:
            out = {}
            for ev in self.events():
                if ev.self_use or (since is not None and ev.at < since) or (upto is not None and ev.at >= upto):
                    continue
                for side, who in (("given", ev.provider_id), ("used", ev.consumer_id)):
                    if who:
                        t = out.setdefault((who, side), [0.0, 0.0, 0.0, 0, 0, 0])
                        t[0] += ev.gen_s if ev.kind != "cpu" else 0.0
                        t[1] += ev.cpu_core_s
                        t[2] += ev.credits
                        t[3] += ev.tokens_in
                        t[4] += ev.tokens_out
                        t[5] += 1
            return out

        def check(when: str):
            for name, span in ledger.RANGES.items():
                since = None if span is None else now - timedelta(seconds=span)
                want = raw(since)
                got = self.totals(since, now=now)
                for uid in (a, b, c):
                    for side in ledger.SIDES:
                        t = got[uid][side]
                        w = want.get((uid, side), [0.0, 0.0, 0.0, 0, 0, 0])
                        with self.subTest(when=when, range=name, user=uid, side=side):
                            self.assertAlmostEqual(t["gpu_s"], w[0], places=6)
                            self.assertAlmostEqual(t["cpu_s"], w[1], places=6)
                            self.assertAlmostEqual(t["credits"], w[2], places=6)
                            self.assertEqual((t["tokens_in"], t["tokens_out"], t["events"]), (w[3], w[4], w[5]))
        check("before any pass")
        with mock.patch.object(ledger, "MAX_DIRTY_HOURS", 5):
            check("before any pass, the hours not rolled up too many to name")
        with db.session() as s:
            hours = ledger.rollup(s)
        self.assertGreater(hours, 50, "three days of hours")
        # every rollup equals the sum of its hour's events
        with db.session() as s:
            rollups = list(s.scalars(select(ComputeRollup)))
        self.assertTrue(rollups)
        for r in rollups:
            want = raw(r.hour, r.hour + timedelta(hours=1))
            hour_events = [e for e in self.events() if r.hour <= e.at < r.hour + timedelta(hours=1) and not e.self_use
                           and e.kind == r.kind and (e.provider_id if r.side == "given" else e.consumer_id) == r.user_id]
            with self.subTest(hour=r.hour.isoformat(), user=r.user_id, side=r.side, kind=r.kind):
                self.assertAlmostEqual(r.credits, sum(e.credits for e in hour_events), places=6)
                self.assertAlmostEqual(r.gen_s, sum(e.gen_s for e in hour_events), places=6)
                self.assertAlmostEqual(r.cpu_core_s, sum(e.cpu_core_s for e in hour_events), places=6)
                self.assertEqual(r.events, len(hour_events))
                self.assertTrue(want)
        check("after the pass")
        # new events, in the current hour and in an hour already rolled up, read at once (no pass yet)
        self.write(event("sv_bob", "game:x", a, 3.0), event("sv_bob", "game:x", a, 4.0,
                                                           at=(now - timedelta(hours=30)).timestamp()))
        now = datetime.now(UTC)
        check("with events not rolled up")
        with db.session() as s:
            self.assertEqual(ledger.rollup(s), 2, "the pass finds the two hours touched")
            self.assertEqual(ledger.rollup(s), 0, "and nothing the second time")
        check("rolled up again")

    def test_the_rollup_thread_passes_survive_a_failure(self):
        with mock.patch.object(ledger, "rollup", side_effect=RuntimeError("disk on fire")):
            with self.assertLogs("citar.ledger", "ERROR"):
                led = ledger.Ledger(background=True)
                with mock.patch.object(ledger, "ROLLUP_SECONDS", 0.05):
                    led.ensure_threads()
                    time.sleep(0.3)
                self.assertTrue(led._roller.is_alive(), "the loop carries on")
                led.stop(2.0)


# ---------------------------------------------------------------------------- gate 4: budgets

class Budgets(World):
    def grant_budget(self, amount, *, behavior="hard", period="day", who="alice", group=None) -> str:
        with db.session() as s:
            grant = s.scalar(select(AccessGrant).where(AccessGrant.group_id == (group or self.group),
                                                       AccessGrant.subject_user_id == self.ids[who]))
            bid = budgets.set_budget(s, grant, period=period, amount=amount, behavior=behavior).id
        with db.session() as s:
            self.assertEqual(self.keeper.handle(s, s.get(AccessGrant, grant.id)), bid)
        return bid

    def scheduler(self):
        clock = FakeClock()
        sched = S.Scheduler(clock=clock, wall=time.time, background=False)
        sched.add_listener(ledger.listener)
        return sched, clock

    def test_a_hard_budget_admits_until_spent_and_reserved_would_pass_it(self):
        bid = self.grant_budget(5.0)
        cap, budget = pool_seats.consumer_terms(self.ids["alice"], "sv_bob", "game")
        self.assertEqual(budget, bid, "the lease carries the grant's budget")
        sched, clock = self.scheduler()
        lease = sched.lease("sv_bob", "dry-run", activity="game:g1", consumer=self.ids["alice"], budget=budget)
        conv = S.ScheduledConversation(Scripted(clock, 2.0), lease)
        conv.step()                                         # reserves 1 (no mean yet), spends 2
        self.led.drain()
        self.assertEqual(self.keeper.account(bid)["spent"], 2.0)
        conv.step()                                         # reserves 2 (the mean), 4 <= 5: spends 2 more
        self.led.drain()
        self.assertEqual(self.keeper.account(bid)["spent"], 4.0)
        with self.assertRaises(S.MachineRefused) as refused:
            conv.step()                                     # 4 + 2 would pass 5
        e = refused.exception
        self.assertEqual(e.code, "budget")
        end = budgets.period_bounds("day", "2026-10-12", "UTC")[1]
        self.assertEqual(e.retry_at, end.astimezone().replace(tzinfo=None), "the period's end")
        self.assertEqual(e.budget, {"id": bid, "key": "2026-10-12"})
        self.assertIn("used up", str(e))
        self.assertEqual(sched.machine("sv_bob").status()["in_flight"], 0, "its slot given back")
        self.assertEqual(self.keeper.account(bid)["reserved"], 0.0)
        self.assertEqual(len(self.events()), 2, "the refused request never ran")
        with db.session() as s:
            entries = [(e.activity_id, e.period_key, e.actual) for e in s.scalars(select(BudgetEntry))]
            state = budgets.state(s, s.scalar(select(AccessGrant).where(AccessGrant.id == s.get(Budget, bid).grant_id)))
        self.assertEqual(entries, [("game:g1", "2026-10-12", 4.0)])
        self.assertEqual((state.spent, state.reserved), (4.0, 0.0))
        # the period turns: the budget admits again
        self.now += timedelta(days=1)
        conv.step()
        self.led.drain()
        self.assertEqual(self.keeper.account(bid)["spent"], 2.0)
        lease.release()

    def test_refresh_from_ledger_rebuilds_the_settled_entries(self):
        bid = self.grant_budget(100.0)
        sched, clock = self.scheduler()
        for act in ("game:g1", "game:g2"):
            lease = sched.lease("sv_bob", "dry-run", activity=act, consumer=self.ids["alice"], budget=bid)
            conv = S.ScheduledConversation(Scripted(clock, 1.5 if act == "game:g1" else 2.5), lease)
            for _ in range(3):
                conv.step()
            lease.release()
        carol = sched.lease("sv_bob", "dry-run", activity="game:g3", consumer=self.ids["carol"])
        S.ScheduledConversation(Scripted(clock, 9.0), carol).step()       # not on alice's budget
        self.led.drain()

        def entries():
            with db.session() as s:
                return sorted((e.activity_id, e.period_key, round(e.actual, 6), e.user_id)
                              for e in s.scalars(select(BudgetEntry).where(BudgetEntry.budget_id == bid)))
        settled = entries()
        self.assertEqual(settled, [("game:g1", "2026-10-12", 4.5, self.ids["alice"]),
                                   ("game:g2", "2026-10-12", 7.5, self.ids["alice"])])
        # the events land on the real clock's day: rebuild that period
        today = budgets.period_key("day", datetime.now(UTC), "UTC")
        with db.session() as s:
            for e in s.scalars(select(BudgetEntry)):
                e.period_key = today
                if e.activity_id == "game:g2":
                    s.delete(e)                             # one lost
            want = sorted((a, today, v, u) for a, _k, v, u in settled)
        with db.session() as s:
            self.assertEqual(budgets.refresh_from_ledger(s, s.get(Budget, bid), key=today), 2)
        self.assertEqual(entries(), want)

    def test_a_hard_budget_on_an_online_api_refuses_the_request_that_would_pass_it(self):
        """The reserve on an API machine is the price of the request's ``max_tokens`` plus its input, times the class
        rate; and with no price for the model, every request is refused."""
        with db.session() as s:
            zara = s.get(User, self.ids["zara"])
            group = pool.create_group(s, zara, name="The site's API")
            s.get(Server, "sv_api").group_id = group.id
            pool.create_grant(s, zara, group, subject_type="user", subject_handle="alice", purposes=["game"],
                              budget={"period": "day", "amount": 30.0})
            gid = group.id
        servers.invalidate()
        _cap, bid = pool_seats.consumer_terms(self.ids["alice"], "sv_api", "game")
        self.assertIsNotNone(bid)
        sched, clock = self.scheduler()
        lease = sched.lease("sv_api", "claude-opus-5", activity="game:g1", consumer=self.ids["alice"], budget=bid)
        # 2,000 in at $5 and 1,000 out at $25 a million: $0.035, 35 credits
        conv = S.ScheduledConversation(Scripted(clock, 1.0, usage={"input_tokens": 2000, "output_tokens": 1000},
                                                model="claude-opus-5", max_tokens=1000), lease)
        conv.step()                         # its estimate: 1,000 out, nothing in yet: 25 credits, within 30
        self.led.drain()
        self.assertAlmostEqual(self.keeper.account(bid)["spent"], 35.0)
        with self.assertRaises(S.MachineRefused) as refused:
            conv.step()                     # 35 spent: anything more passes 30
        self.assertEqual(refused.exception.code, "budget")
        lease.release()
        self.now += timedelta(days=1)
        mystery = sched.lease("sv_api", "mystery", activity="game:g2", consumer=self.ids["alice"], budget=bid)
        conv = S.ScheduledConversation(Scripted(clock, 1.0, model="mystery", max_tokens=10), mystery)
        with self.assertRaises(S.MachineRefused) as refused:
            conv.step()
        self.assertIn("has no price", str(refused.exception))
        self.assertIsNone(refused.exception.retry_at, "waiting does not help")
        with db.session() as s:
            grant = s.scalar(select(AccessGrant).where(AccessGrant.group_id == gid))
            budgets.set_budget(s, grant, period="day", amount=30.0, behavior="soft")
        with db.session() as s:
            self.keeper.reload(s, bid)
        conv.step()                         # a soft budget lets the unpriced request through
        self.led.drain()
        self.assertEqual(self.events()[-1].flags, {"unpriced": True})
        mystery.release()

    def test_a_soft_budget_only_warns(self):
        bid = self.grant_budget(3.0, behavior="soft")
        sched, clock = self.scheduler()
        heard = []
        lease = sched.lease("sv_bob", "dry-run", activity="game:g1", consumer=self.ids["alice"], budget=bid,
                            on_budget=heard.append)
        conv = S.ScheduledConversation(Scripted(clock, 2.0), lease)
        for _ in range(4):
            conv.step()
            self.led.drain()
        sched.deliver()
        self.assertEqual(self.keeper.account(bid)["spent"], 8.0, "nothing refused")
        self.assertEqual(len(heard), 1, "told once a period")
        self.assertIn("soft budget", heard[0]["warning"])
        with db.session() as s:
            grant = s.get(AccessGrant, s.get(Budget, bid).grant_id)
            client = budgets.state(s, grant).client()
        self.assertIn("soft budget", client["warning"], "the machine owner's Servers page says so")
        lease.release()


class BudgetedGame(World):
    BACKGROUND = True

    def test_a_lobby_game_pauses_for_its_budget_and_resumes_when_the_period_turns(self):
        """alice's lobby game on bob's machine, under her grant's hard budget of 12 credits a day (a GPU second worth
        100 credits here, a dry-run step about 5): its seat plays until the next request would pass the budget, the game
        pauses with the budget's reason, and when the day turns it resumes and plays on, its spend counted in the new
        day."""
        from citar.server.session import GameSession, SessionManager
        self.rate("hc_gpu", 100.0)
        with db.session() as s:
            grant = s.scalar(select(AccessGrant).where(AccessGrant.subject_user_id == self.ids["alice"]))
            bid = budgets.set_budget(s, grant, period="day", amount=12.0).id
        mgr = SessionManager()
        with mock.patch.object(GameSession, "BUDGET_POLL_SECONDS", 0.05):
            g = mgr.create({"map_size": "duel", "seed": 21, "barbarians": "off"},
                           [{"type": "llm", "llm": {"server_id": "sv_bob", "model": "dry-run", "dry_run_delay": 0.05}},
                            {"type": "bot"}], "budgeted", owner_id=self.ids["alice"])
            self.addCleanup(mgr.delete, g.id)
            paused = until(lambda: g.paused and (g.pause_reason or {}).get("kind") == "budget", 60)
            self.assertTrue(paused, g.pause_reason)
            reason = g.pause_reason
            self.assertEqual((reason["budget"], reason["period"]), (bid, "2026-10-12"))
            self.assertIn("used up", reason["message"])
            self.assertIsNotNone(reason["until"])
            first_day = self.keeper.account(bid)["spent"]
            self.assertGreater(first_day, 0)
            self.assertLessEqual(first_day, 12.0 + 7.0, "at most one step past it")
            time.sleep(0.3)
            self.assertTrue(g.paused, "it stays paused while the day lasts")
            self.now += timedelta(days=1)
            resumed = until(lambda: any(e.get("type") == "game_resumed" for e in g.game.events()), 30)
            self.assertTrue(resumed)
            spent = until(lambda: self.led.flush(5) and (self.keeper.account(bid) or {}).get("spent", 0) > 0, 30)
            self.assertTrue(spent, "the new day's requests are counted in it")
        with db.session() as s:
            keys = {e.period_key for e in s.scalars(select(BudgetEntry).where(BudgetEntry.budget_id == bid))}
        self.assertEqual(keys, {"2026-10-12", "2026-10-13"})


# ---------------------------------------------------------------------------- gate 5: costing

class Costing(World):
    def test_costing_before_and_after_the_switch(self):
        """The same work priced from old spans (held, busy, CPU and tokens in the usage ledger) and from held-only
        spans plus the compute ledger's events: equal with no queue time; with queue time, which the old metering
        counted as busy, the busy energy falls."""
        T0 = datetime(2026, 3, 10, 12, 0).timestamp()
        acts = {"game:x": {"id": "game:x", "kind": "game", "name": "x"}, "game:y": {"id": "game:y", "kind": "game"}}
        held = [{"act": "game:x", "srv": "sv_gpu", "t0": T0, "t1": T0 + 60, "held": 60},
                {"act": "game:x", "srv": "sv_host", "t0": T0, "t1": T0 + 60, "held": 60},
                {"act": "game:y", "srv": "sv_api", "t0": T0, "t1": T0 + 60, "held": 60}]

        def old(queue_s: float) -> list:
            return [{**held[0], "model": "dry-run", "busy": 30.0 + queue_s, "cpu": 0, "in": 1000, "out": 100,
                     "req": 1},
                    {**held[1], "busy": 0, "cpu": 12.0},
                    {**held[2], "model": "claude-opus-5", "busy": 4.0, "in": 50_000, "out": 2000, "cr": 10_000,
                     "req": 1}]
        self.write(event("sv_gpu", "game:x", None, 30.0, at=T0 + 50, usage={"input_tokens": 1000, "output_tokens": 100}),
                   ("cpu", "game:x", 12.0, T0 + 60),
                   event("sv_api", "game:y", None, 4.0, at=T0 + 40, model="claude-opus-5",
                         usage={"input_tokens": 50_000, "output_tokens": 2000, "cache_read_input_tokens": 10_000}))
        work = usage.work_rows(T0 - 60, T0 + 120)
        self.assertEqual(len(work), 3)
        reg = servers.with_pooled()
        before = costing.compute(T0, T0 + 60, reg, {"acts": acts, "spans": old(0.0), "power": {}})
        after = costing.compute(T0, T0 + 60, reg, {"acts": acts, "spans": held, "power": {}, "work": work})
        for act in acts:
            for k, v in before["acts"][act]["total"].items():
                with self.subTest(act=act, field=k):
                    self.assertAlmostEqual(after["acts"][act]["total"][k], v, places=9)
        self.assertGreater(before["acts"]["game:y"]["total"]["tokens"], 0, "the API's tokens are priced")
        self.assertGreater(before["acts"]["game:x"]["total"]["energy_dynamic"], 0)
        queued = costing.compute(T0, T0 + 60, reg, {"acts": acts, "spans": old(20.0), "power": {}})
        x_old, x_new = queued["acts"]["game:x"]["total"], after["acts"]["game:x"]["total"]
        self.assertGreater(x_old["energy_dynamic"], x_new["energy_dynamic"], "queue time is no longer busy energy")
        self.assertGreater(x_old["busy_h"], x_new["busy_h"])
        self.assertAlmostEqual(x_old["depreciation"], x_new["depreciation"], places=9, msg="what was held is unchanged")


# ---------------------------------------------------------------------------- gate 6: the routes

class Routes(World):
    def setUp(self):
        super().setUp()
        from fastapi.testclient import TestClient
        from citar.server import app as appmod
        self.client = TestClient(appmod.app, base_url="https://citar.test")
        self.addCleanup(self.client.close)
        a, b = self.ids["alice"], self.ids["bob"]
        self.write(event("sv_bob", "game:g1", a, 10.0), event("sv_alice", "report:r1", b, 4.0, cls="report",
                                                              task="partial"))

    def call(self, who, method, path, body=None):
        return self.client.request(method, path, json=body, headers=self.headers[who])

    def levels(self, who, **levels):
        with db.session() as s:
            user = s.get(User, self.ids[who])
            user.field_visibility = {**(user.field_visibility or {}), **levels}

    def test_the_ratio_follows_its_fields_visibility(self):
        path = "/api/users/alice/ratio"
        # private (the default): the account and administrators only
        self.assertEqual(self.call("bob", "GET", path).status_code, 404)
        self.assertEqual(self.call("anon", "GET", path).status_code, 404)
        mine = self.call("alice", "GET", path)
        self.assertEqual(mine.status_code, 200, mine.text)
        admin = self.call("zara", "GET", path).json()
        self.assertEqual((admin["given"]["credits"], admin["used"]["credits"]), (4.0, 10.0))
        self.assertEqual(admin["ratio"], {"value": 0.4, "text": "0.40"})
        self.assertNotIn("ratio", self.call("bob", "GET", "/api/users/alice").json(), "nor on her profile")
        self.assertIn("ratio", self.call("zara", "GET", "/api/users/alice").json())
        # server: members too
        self.levels("alice", ratio="server")
        got = self.call("bob", "GET", path)
        self.assertEqual(got.status_code, 200)
        self.assertEqual(got.json()["used"]["credits"], 10.0)
        self.assertEqual(self.call("bob", "GET", "/api/users/alice").json()["ratio"]["range"], "30d")
        self.assertEqual(self.call("anon", "GET", path).status_code, 404)
        # global (with the profile itself global): anyone
        self.levels("alice", ratio="global", profile="global")
        self.assertEqual(self.call("anon", "GET", path).status_code, 200)
        self.assertEqual(self.call("anon", "GET", path + "?range=7d").json()["range"], "7d")
        self.assertEqual(self.call("anon", "GET", path + "?range=1y").status_code, 400)
        self.assertEqual(self.call("anon", "GET", "/api/users/nobody/ratio").status_code, 404)

    def test_ledger_me_is_the_callers_own(self):
        self.assertEqual(self.call("anon", "GET", "/api/ledger/me").status_code, 401)
        alice = self.call("alice", "GET", "/api/ledger/me").json()
        self.assertEqual((alice["given"]["credits"], alice["used"]["credits"]), (4.0, 10.0))
        self.assertEqual({m["name"] for m in alice["by_machine"]}, {"Bob box", "Alice box"})
        carol = self.call("carol", "GET", "/api/ledger/me")
        self.assertEqual(carol.status_code, 200)
        body = carol.json()
        self.assertEqual((body["given"]["credits"], body["used"]["credits"], body["by_machine"]), (0.0, 0.0, []))
        self.assertNotIn("Alice box", carol.text)
        bob = self.call("bob", "GET", "/api/ledger/me?range=24h").json()
        names = {m["name"] for m in bob["by_machine"]}
        self.assertEqual(names, {"Bob box", "another member's machine"}, "alice's private machine is not named to him")
        self.assertEqual(self.call("alice", "GET", "/api/ledger/me?range=forever").status_code, 400)

    def test_the_admin_users_page_shows_each_accounts_30_days(self):
        rows = {u["handle"]: u for u in self.call("zara", "GET", "/api/admin/users").json()["users"]}
        self.assertEqual(rows["alice"]["compute_30d"], {"given": 4.0, "used": 10.0, "ratio": {"value": 0.4,
                                                                                                "text": "0.40"}})
        self.assertEqual(rows["carol"]["compute_30d"]["ratio"], {"value": None, "text": "nothing yet"})
        self.assertEqual(self.call("bob", "GET", "/api/admin/users").status_code, 404)

    def test_hardware_classes_are_the_administrators(self):
        base = "/api/admin/hardware-classes"
        self.assertEqual(self.call("anon", "GET", base).status_code, 401)
        for method, path, body in (("GET", base, None), ("POST", base, {"kind": "gpu", "name": "x", "rate": 1}),
                                   ("PUT", f"{base}/hc_gpu", {"rate": 9}), ("DELETE", f"{base}/hc_gpu", None)):
            with self.subTest(method=method):
                self.assertEqual(self.call("bob", method, path, body).status_code, 404, "a member: as if none")
        listed = self.call("zara", "GET", base).json()
        self.assertEqual([(c["id"], c["is_default"]) for c in listed["classes"]],
                         [("hc_gpu", True), ("hc_cpu", True), ("hc_api", True)])
        gpu = next(c for c in listed["classes"] if c["id"] == "hc_gpu")
        self.assertIn("Bob box", {m["name"] for m in gpu["machines"]})
        for body in ({"kind": "tpu", "name": "x", "rate": 1}, {"kind": "gpu", "name": "", "rate": 1},
                     {"kind": "gpu", "name": "x", "rate": -1}, {"kind": "gpu", "name": "x", "rate": float("1e12")},
                     {"kind": "gpu", "name": "x", "rate": 1, "match": {"gpu_name": "("}},
                     {"kind": "gpu", "name": "x", "rate": 1, "match": {"ram": 3}},
                     {"kind": "gpu", "name": "x", "rate": 1, "priority": 5000},
                     {"kind": "gpu", "name": "x", "rate": True}, {"kind": "gpu", "name": "x", "rate": 1, "x": 1}):
            with self.subTest(body=body):
                self.assertEqual(self.call("zara", "POST", base, body).status_code, 400)
        made = self.call("zara", "POST", base, {"kind": "gpu", "name": "Big GPU", "rate": 2.5, "priority": 3,
                                                "match": {"vram_gb_min": 20}})
        self.assertEqual(made.status_code, 200, made.text)
        cid = made.json()["id"]
        with db.session() as s:
            row = s.get(Server, "sv_bob")
            row.config = {**row.config, "hardware": {"gpus": [{"name": "A6000", "vram_gb": 48}]}}
        servers.invalidate()
        machines = next(c for c in self.call("zara", "GET", base).json()["classes"] if c["id"] == cid)["machines"]
        self.assertEqual([m["name"] for m in machines], ["Bob box"])
        self.write(event("sv_bob", "game:g1", self.ids["alice"], 2.0))
        self.assertEqual(self.events()[-1].credits, 5.0, "priced by the class its hardware fits")
        self.assertEqual(self.call("zara", "PUT", f"{base}/{cid}", {"kind": "cpu"}).status_code, 400)
        self.assertEqual(self.call("zara", "PUT", f"{base}/{cid}", {"rate": 4.0}).status_code, 200)
        self.write(event("sv_bob", "game:g1", self.ids["alice"], 2.0))
        self.assertEqual(self.events()[-1].credits, 8.0, "the new rate from then on")
        self.assertEqual(self.call("zara", "PUT", f"{base}/hc_gpu", {"is_default": False}).status_code, 409)
        self.assertEqual(self.call("zara", "DELETE", f"{base}/hc_gpu").status_code, 409)
        self.assertEqual(self.call("zara", "PUT", f"{base}/{cid}", {"is_default": True}).status_code, 200)
        listed = {c["id"]: c["is_default"] for c in self.call("zara", "GET", base).json()["classes"]}
        self.assertEqual((listed[cid], listed["hc_gpu"]), (True, False), "one default a kind")
        self.assertEqual(self.call("zara", "PUT", f"{base}/hc_gpu", {"is_default": True}).status_code, 200)
        self.assertEqual(self.call("zara", "DELETE", f"{base}/{cid}").status_code, 200)
        self.assertEqual(self.call("zara", "DELETE", f"{base}/{cid}").status_code, 404)
        with db.session() as s:
            actions = [a for (a,) in s.execute(select(func.count()).select_from(
                __import__("citar.db.models", fromlist=["AuditLog"]).AuditLog).where(
                __import__("citar.db.models", fromlist=["AuditLog"]).AuditLog.object_type == "hardware_class"))]
        self.assertGreaterEqual(actions[0], 5, "every change audited")


# ---------------------------------------------------------------------------- gate 7: write volume

class WriteVolume(World):
    BACKGROUND = True

    def test_ten_thousand_events_from_eight_threads_in_under_two_seconds(self):
        """8 producers hand the ledger 1,250 events each, as the scheduler's listener does after each step; the writer
        has them all in the database in under 2 s while a game session's lock is held throughout (by this thread): the
        writer never takes one, and no producer waits on the database."""
        from citar.server.session import SessionManager
        mgr = SessionManager()
        g = mgr.create({"map_size": "duel", "seed": 31, "barbarians": "off"}, [{"type": "human"}, {"type": "bot"}],
                       "locked", track=False, start=False)
        self.addCleanup(mgr.delete, g.id)
        a = self.ids["alice"]
        prepared = [[event("sv_bob", f"game:g{t}", a, 0.5) for _ in range(1250)] for t in range(8)]
        self.led.price(prepared[0][0])          # the caches a step is priced from, as a running server has them
        self.led.ensure_threads()
        slowest = []
        go = threading.Barrier(9)

        def produce(evs):
            go.wait()
            worst = 0.0
            for ev in evs:
                t0 = time.perf_counter()
                self.led.step(ev)
                worst = max(worst, time.perf_counter() - t0)
            slowest.append(worst)
        threads = [threading.Thread(target=produce, args=(evs,)) for evs in prepared]
        for t in threads:
            t.start()
        with g.lock:                             # held all along: the writer never needs it
            go.wait()
            t0 = time.perf_counter()
            for t in threads:
                t.join()
            self.assertTrue(self.led.flush(10))
            took = time.perf_counter() - t0
        with db.session() as s:
            n = s.scalar(select(func.count()).select_from(ComputeEvent))
        print(f"\nledger: 10,000 events from 8 threads written in {took:.2f} s; slowest hand-off "
              f"{max(slowest) * 1000:.1f} ms")
        self.assertEqual(n, 10_000)
        self.assertLess(took, 2.0)
        self.assertLess(max(slowest), 0.5, "a producer never waits on the database")

    def test_the_writer_drains_on_shutdown_and_survives_a_failed_batch(self):
        a = self.ids["alice"]
        real = self.led._rows
        calls = []

        def failing_once(session, batch):
            calls.append(len(batch))
            if len(calls) == 1:
                raise RuntimeError("the database went away")
            return real(session, batch)
        with mock.patch.object(self.led, "_rows", failing_once), self.assertLogs("citar.ledger", "WARNING"):
            self.led.step(event("sv_bob", "game:lost", a, 1.0))
            self.assertTrue(self.led.flush(5))
        self.assertEqual(self.led.failures, 1)
        for i in range(500):
            self.led.step(event("sv_bob", "game:kept", a, 1.0))
        self.assertTrue(self.led.stop(10))
        with db.session() as s:
            self.assertEqual(s.scalar(select(func.count()).select_from(ComputeEvent)
                                      .where(ComputeEvent.activity == "game:kept")), 500)
        self.assertFalse(self.led._writer.is_alive() and self.led.q.qsize(), "nothing left behind")


if __name__ == "__main__":
    unittest.main()
