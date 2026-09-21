"""Server sharing: availability windows, budgets and admission control.

The window tests are the ones worth reading. "Midnight to 6am in the owner's zone" has to stay at
the owner's midnight through daylight-saving changes and be rendered correctly for somebody in
another country — that is the part of the brief most likely to be silently wrong, and the failure
mode is a server quietly running at 3am when its owner said not to.
"""
from __future__ import annotations

import os
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

import tests  # noqa: F401

_TMP = Path(tempfile.mkdtemp(prefix="citar_pool_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from sqlalchemy import select
from citar import db, settings
from citar.auth import accounts, policy
from citar.db.models import (AccessGrant, Base,
                             Budget, BudgetEntry, Server, ServerGroup, User)
from citar.pool import admission, budgets, windows
import citar.pool as pool

settings.reset()
db.configure(os.environ["CITAR_DB_URL"])
db.create_all()

NY = "America/New_York"
LONDON = "Europe/London"
TOKYO = "Asia/Tokyo"


def _reset():
    db.dispose()
    db.configure(os.environ["CITAR_DB_URL"])
    Base.metadata.drop_all(db.engine())
    db.create_all()
    policy.invalidate()


def utc(y, m, d, h=0, mi=0):
    return datetime(y, m, d, h, mi, tzinfo=timezone.utc)


class Windows(unittest.TestCase):
    """Pure window arithmetic — no database."""

    #: The brief's example: midnight to 6am, every day, in the owner's zone.
    MIDNIGHT_TO_SIX = [(d, 0, 360) for d in range(7)]

    def test_no_windows_means_always_available(self):
        state = windows.evaluate([], NY)
        self.assertTrue(state.open)
        self.assertTrue(state.always)

    def test_wrapping_window_is_split_at_midnight(self):
        # Monday 22:00-02:00 is Monday 22:00-24:00 plus Tuesday 00:00-02:00.
        rows = windows.normalize(0, 22 * 60, 2 * 60)
        self.assertEqual(rows, [(0, 1320, 1440), (1, 0, 120)])

    def test_zero_length_window_is_dropped(self):
        self.assertEqual(windows.normalize(0, 300, 300), [])

    def test_overlapping_windows_merge(self):
        rows = windows.normalize_many([(0, 0, 360), (0, 300, 480)])
        self.assertEqual(rows, [(0, 0, 480)])

    def test_open_inside_the_window(self):
        # 02:00 in New York on a winter day = 07:00 UTC.
        state = windows.evaluate(self.MIDNIGHT_TO_SIX, NY, utc(2026, 1, 15, 7, 0))
        self.assertTrue(state.open)

    def test_closed_outside_the_window(self):
        # 12:00 New York = 17:00 UTC.
        state = windows.evaluate(self.MIDNIGHT_TO_SIX, NY, utc(2026, 1, 15, 17, 0))
        self.assertFalse(state.open)
        self.assertIsNotNone(state.next_open)

    def test_midnight_to_six_holds_across_the_dst_boundary(self):
        """The heart of it: the window must track the owner's clock, not a fixed UTC offset.

        In January, 02:00 New York is 07:00 UTC. In July it is 06:00 UTC. A window stored as a UTC
        offset would be an hour wrong for half the year; one stored as wall-clock time is right in
        both.
        """
        for month, offset_hours in ((1, 5), (7, 4)):       # EST is UTC-5, EDT is UTC-4
            for local_hour in (0, 3, 5):                   # inside
                moment = utc(2026, month, 15, (local_hour + offset_hours) % 24)
                self.assertTrue(windows.evaluate(self.MIDNIGHT_TO_SIX, NY, moment).open,
                                f"month {month}, local {local_hour}:00 should be OPEN")
            for local_hour in (7, 12, 23):                 # outside
                moment = utc(2026, month, 15, (local_hour + offset_hours) % 24)
                self.assertFalse(windows.evaluate(self.MIDNIGHT_TO_SIX, NY, moment).open,
                                 f"month {month}, local {local_hour}:00 should be CLOSED")

    def test_next_open_is_reported_in_real_time(self):
        # Noon in New York; the window reopens at the next midnight, twelve hours later.
        now = utc(2026, 1, 15, 17, 0)
        state = windows.evaluate(self.MIDNIGHT_TO_SIX, NY, now)
        self.assertFalse(state.open)
        hours = (state.next_open - now).total_seconds() / 3600
        # 17:00 UTC is 12:00 in New York; the next midnight there is 05:00 UTC the following day.
        self.assertAlmostEqual(hours, 12.0, delta=0.1)

    def test_window_is_translated_into_the_viewers_zone(self):
        """'their local time automatically calculated and translated for people in other zones'."""
        described = windows.describe_for_viewer(self.MIDNIGHT_TO_SIX, NY, TOKYO,
                                                utc(2026, 1, 15, 12))
        self.assertIn("00:00–06:00", described["owner_text"])
        # New York midnight is 14:00 the same day in Tokyo (+14 in winter).
        self.assertIn("14:00", described["viewer_text"])
        self.assertFalse(described["same_zone"])

    def test_same_zone_is_flagged_so_the_ui_can_skip_the_translation(self):
        described = windows.describe_for_viewer(self.MIDNIGHT_TO_SIX, NY, NY)
        self.assertTrue(described["same_zone"])

    def test_weekday_ranges_render_compactly(self):
        self.assertEqual(windows.describe([(d, 0, 360) for d in range(7)], NY),
                         "Every day 00:00–06:00")
        self.assertEqual(windows.describe([(d, 540, 1020) for d in range(5)], NY),
                         "Mon–Fri 09:00–17:00")

    def test_unknown_timezone_falls_back_rather_than_raising(self):
        # Time zones arrive from browsers; a bad one must not break an otherwise fine request.
        self.assertTrue(windows.evaluate([], "Mars/Olympus_Mons").open)
        self.assertFalse(windows.valid_zone("Mars/Olympus_Mons"))


class BudgetPeriods(unittest.TestCase):
    def test_month_boundary_is_the_owners_midnight_not_utc(self):
        # 2026-10-01 03:00 UTC is still 2026-09-30 23:00 in New York, so it belongs to September.
        self.assertEqual(budgets.period_key("month", utc(2026, 10, 1, 3, 0), NY), "2026-09")
        self.assertEqual(budgets.period_key("month", utc(2026, 10, 1, 3, 0), "UTC"), "2026-10")

    def test_period_bounds_round_trip(self):
        start, end = budgets.period_bounds("month", "2026-09", NY)
        self.assertEqual(budgets.period_key("month", start, NY), "2026-09")
        self.assertEqual(budgets.period_key("month", end - timedelta(minutes=1), NY), "2026-09")
        self.assertEqual(budgets.period_key("month", end, NY), "2026-10")

    def test_total_has_a_single_bucket(self):
        self.assertEqual(budgets.period_key("total", utc(2020, 1, 1)), "all")
        self.assertEqual(budgets.period_key("total", utc(2030, 6, 6)), "all")

    def test_day_and_week_keys(self):
        self.assertEqual(budgets.period_key("day", utc(2026, 9, 20, 12), "UTC"), "2026-09-20")
        self.assertTrue(budgets.period_key("week", utc(2026, 9, 20, 12), "UTC").startswith("2026-W"))


class PoolFixtures(unittest.TestCase):
    """Shared setup: an owner with a group and a server, and a second user."""

    def setUp(self):
        _reset()
        with db.session() as s:
            self.owner = accounts.create_user(s, handle="hostess", email="host@example.com",
                                              password="a long enough phrase", status="active",
                                              email_verified=True, tz=NY)
            self.guest = accounts.create_user(s, handle="visitor", email="visitor@example.com",
                                              password="a long enough phrase", status="active",
                                              email_verified=True, tz=TOKYO)
            self.owner_id, self.guest_id = self.owner.id, self.guest.id
            group = pool.create_group(s, self.owner, name="Overnight box")
            self.group_id = group.id
            server = Server(owner_id=self.owner.id, group_id=group.id, name="Big GPU",
                            kind="owned", provider="lmstudio", config={"id": "sv_1"},
                            max_concurrent=1)
            s.add(server)
            s.flush()
            self.server_id = server.id

    def _grant(self, s, **kw):
        kw.setdefault("subject_type", "everyone")
        return pool.create_grant(s, s.get(User, self.owner_id),
                                 s.get(ServerGroup, self.group_id), **kw)


class Admission(PoolFixtures):
    def test_owner_may_always_use_their_own_server(self):
        with db.session() as s:
            decision = admission.check(s, s.get(User, self.owner_id),
                                       s.get(Server, self.server_id))
            self.assertTrue(decision.allowed, decision.reason)

    def test_stranger_without_a_grant_is_refused(self):
        with db.session() as s:
            decision = admission.check(s, s.get(User, self.guest_id),
                                       s.get(Server, self.server_id))
            self.assertFalse(decision.allowed)
            self.assertEqual(decision.code, "no_grant")

    def test_grant_to_everyone_admits_a_stranger(self):
        with db.session() as s:
            self._grant(s)
        with db.session() as s:
            decision = admission.check(s, s.get(User, self.guest_id), s.get(Server, self.server_id))
            self.assertTrue(decision.allowed, decision.reason)

    def test_purpose_restriction_is_enforced_and_explained(self):
        with db.session() as s:
            self._grant(s, purposes=["scenario"])
        with db.session() as s:
            guest, server = s.get(User, self.guest_id), s.get(Server, self.server_id)
            self.assertTrue(admission.check(s, guest, server, "scenario").allowed)
            refused = admission.check(s, guest, server, "benchmark")
            self.assertFalse(refused.allowed)
            self.assertEqual(refused.code, "purpose")
            self.assertIn("scenarios", refused.reason)

    def test_the_briefs_overnight_scenario_box(self):
        """'available only for scenarios from midnight to 6am with a budget of $100 per month'."""
        with db.session() as s:
            self._grant(s, purposes=["scenario"],
                        window_rows=[(d, 0, 360) for d in range(7)],
                        budget={"period": "month", "amount": 100.0, "behavior": "hard"})

        with db.session() as s:
            guest, server = s.get(User, self.guest_id), s.get(Server, self.server_id)
            # 02:00 in New York, in winter -> allowed.
            self.assertTrue(admission.check(s, guest, server, "scenario",
                                            now=utc(2026, 1, 15, 7)).allowed)
            # 02:00 New York in summer is a different UTC hour, and must still be allowed.
            self.assertTrue(admission.check(s, guest, server, "scenario",
                                            now=utc(2026, 7, 15, 6)).allowed)
            # Noon in New York -> refused, with a time to come back.
            refused = admission.check(s, guest, server, "scenario", now=utc(2026, 1, 15, 17))
            self.assertFalse(refused.allowed)
            self.assertEqual(refused.code, "window")
            self.assertTrue(refused.temporary)
            self.assertIsNotNone(refused.retry_at)
            # The refusal is quotable and mentions the owner's zone.
            self.assertIn("00:00–06:00", refused.reason)

    def test_refusal_explains_in_the_viewers_own_zone_too(self):
        with db.session() as s:
            self._grant(s, window_rows=[(d, 0, 360) for d in range(7)])
        with db.session() as s:
            guest = s.get(User, self.guest_id)          # Tokyo
            refused = admission.check(s, guest, s.get(Server, self.server_id),
                                      now=utc(2026, 1, 15, 17))
            self.assertFalse(refused.allowed)
            self.assertIn("your", refused.reason)

    def test_exhausted_hard_budget_blocks_and_says_when_it_resets(self):
        with db.session() as s:
            grant = self._grant(s, budget={"period": "month", "amount": 10.0, "behavior": "hard"})
            budget = s.scalar(select(Budget).where(Budget.grant_id == grant.id))
            s.add(BudgetEntry(budget_id=budget.id, activity_id="act1", reserved=0.0, actual=12.0,
                              period_key=budgets.period_key("month", None, NY)))
        with db.session() as s:
            refused = admission.check(s, s.get(User, self.guest_id), s.get(Server, self.server_id))
            self.assertFalse(refused.allowed)
            self.assertEqual(refused.code, "budget")
            self.assertIn("used up", refused.reason)

    def test_soft_budget_warns_but_admits(self):
        with db.session() as s:
            grant = self._grant(s, budget={"period": "month", "amount": 10.0, "behavior": "soft"})
            budget = s.scalar(select(Budget).where(Budget.grant_id == grant.id))
            s.add(BudgetEntry(budget_id=budget.id, activity_id="act1", reserved=0.0, actual=99.0,
                              period_key=budgets.period_key("month", None, NY)))
        with db.session() as s:
            decision = admission.check(s, s.get(User, self.guest_id), s.get(Server, self.server_id))
            self.assertTrue(decision.allowed)

    def test_reservation_counts_against_the_cap_before_any_cost_is_known(self):
        """The whole point of reserving: a long game must not be able to blow past the cap while
        its real cost is still unknown."""
        with db.session() as s:
            grant = self._grant(s, budget={"period": "month", "amount": 10.0, "behavior": "hard"})
            grant_id = grant.id
            budgets.reserve(s, grant, "act-running", amount=9.99)
        with db.session() as s:
            grant = s.get(AccessGrant, grant_id)
            state = budgets.state(s, grant)
            self.assertAlmostEqual(state.reserved, 9.99, places=2)
            # Not technically exhausted (9.99 < 10.00), but far too little left to admit new work.
            self.assertLess(state.remaining, budgets.MIN_RESERVE)
            refused = admission.check(s, s.get(User, self.guest_id), s.get(Server, self.server_id))
            self.assertEqual(refused.code, "budget")

    def test_releasing_a_reservation_frees_the_headroom(self):
        with db.session() as s:
            grant = self._grant(s, budget={"period": "month", "amount": 10.0})
            grant_id = grant.id
            budgets.reserve(s, grant, "act-doomed", amount=9.99)
        with db.session() as s:
            budgets.release(s, "act-doomed")
        with db.session() as s:
            self.assertFalse(budgets.state(s, s.get(AccessGrant, grant_id)).exhausted)

    def test_concurrency_limit_is_reported_as_busy(self):
        with db.session() as s:
            self._grant(s)
        with db.session() as s:
            refused = admission.check(s, s.get(User, self.guest_id), s.get(Server, self.server_id),
                                      in_flight=1)
            self.assertFalse(refused.allowed)
            self.assertEqual(refused.code, "concurrency")

    def test_offline_server_is_refused_with_a_useful_message(self):
        with db.session() as s:
            self._grant(s)
        with db.session() as s:
            refused = admission.check(s, s.get(User, self.guest_id), s.get(Server, self.server_id),
                                      online=False)
            self.assertEqual(refused.code, "offline")
            self.assertIn("citar-worker", refused.reason)

    def test_disabled_server_is_refused_even_for_its_owner(self):
        with db.session() as s:
            s.get(Server, self.server_id).enabled = False
        with db.session() as s:
            decision = admission.check(s, s.get(User, self.owner_id), s.get(Server, self.server_id))
            self.assertFalse(decision.allowed)
            self.assertEqual(decision.code, "disabled")

    def test_expired_grant_stops_working(self):
        with db.session() as s:
            self._grant(s, expires_at=datetime.now(timezone.utc) - timedelta(hours=1))
        with db.session() as s:
            decision = admission.check(s, s.get(User, self.guest_id), s.get(Server, self.server_id))
            self.assertFalse(decision.allowed)

    def test_suspended_user_loses_access(self):
        with db.session() as s:
            self._grant(s)
            s.get(User, self.guest_id).status = "suspended"
        with db.session() as s:
            decision = admission.check(s, s.get(User, self.guest_id), s.get(Server, self.server_id))
            self.assertFalse(decision.allowed)

    def test_the_general_pool_example(self):
        """'One user might add a server for the general pool available at all hours.'"""
        with db.session() as s:
            self._grant(s, concurrency=2)
        with db.session() as s:
            guest, server = s.get(User, self.guest_id), s.get(Server, self.server_id)
            for purpose in admission.PURPOSES:
                self.assertTrue(admission.check(s, guest, server, purpose, in_flight=0).allowed,
                                f"{purpose} should be allowed in the general pool")
            # Two slots granted, so one in flight is still fine.
            self.assertTrue(admission.check(s, guest, server, in_flight=1).allowed)
            self.assertFalse(admission.check(s, guest, server, in_flight=2).allowed)


class Visibility(PoolFixtures):
    def test_non_owner_never_sees_the_server_configuration(self):
        """A grant lets you run a model on somebody's machine. It does not let you read their
        electricity tariff, their hardware costs or their key configuration."""
        with db.session() as s:
            self._grant(s)
            s.get(Server, self.server_id).config = {
                "id": "sv_1", "name": "Big GPU", "kind": "owned",
                "costs": [{"electricity_plan_id": "secret", "hourly": 12.34}],
                "connection": {"provider": "lmstudio", "key": {"backend": "keyring"}},
                "models": [{"key": "qwen", "label": "Qwen"}],
            }
        with db.session() as s:
            guest_view = pool.public_server(s, s.get(Server, self.server_id), s.get(User, self.guest_id))
            owner_view = pool.public_server(s, s.get(Server, self.server_id), s.get(User, self.owner_id))
        self.assertNotIn("config", guest_view)
        self.assertNotIn("12.34", str(guest_view))
        self.assertIn("config", owner_view)
        # The guest does learn what it can run, which is the point of sharing it.
        self.assertEqual([m["key"] for m in guest_view["models"]], ["qwen"])


if __name__ == "__main__":
    unittest.main()
