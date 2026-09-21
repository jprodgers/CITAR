"""Spend caps on shared servers.

No money moves anywhere. A budget is **accounting and admission control**: it stops work being
scheduled, and it pauses work that crosses the line. Nobody is charged and nothing is billed.

Cost comes from the existing usage ledger priced by `citar/costing.py`, which already handles
electricity tariffs, straight-line depreciation, lease costs, hourly rates for rented machines and
per-token API prices. Deriving budgets from it rather than recording a number at the time means
correcting a wrong electricity rate later also corrects every budget that was measured with it.

Two things here are easy to get wrong and are handled deliberately:

*A period ends at the owner's midnight, not UTC's.* "$100 per month" means their month. An owner in
New York gets a month that runs from the 1st at 00:00 Eastern; computing it in UTC would move the
boundary by five hours and, twice a year, by a different five hours.

*Cost is only known afterwards.* A game started with $2 of headroom can spend $40 before anything
notices, because the ledger lands minutes later. So every activity opens a **reservation** against a
conservative estimate at launch, and that estimate is replaced by the real figure as the ledger
arrives. Without the reservation the cap is advisory; with it, the worst case is bounded by one
estimate rather than by an entire unattended run.
"""
from __future__ import annotations

import logging
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from typing import Optional

from sqlalchemy import select

from ..db.models import AccessGrant, Budget, BudgetEntry, Server, ServerGroup, User
from .windows import zone

log = logging.getLogger("citar.budgets")

PERIODS = ("day", "week", "month", "total")

#: What to reserve when nothing better is known: one hour of the server's own hourly cost. Deliberately
#: conservative — reserving too little is how a cap gets blown, and reserving too much only delays
#: work that the true-up releases within a minute or two.
DEFAULT_RESERVE_HOURS = 1.0
#: Floor for an API-priced server, whose hourly cost is zero but whose token cost is not.
MIN_RESERVE = 0.25


# ---------------------------------------------------------------------------- periods

def period_key(period: str, when: Optional[datetime] = None, tz: str = "UTC") -> str:
    """The bucket an instant falls in, computed in the owner's zone.

    `total` has one bucket forever, which is what makes a lifetime cap expressible.
    """
    if period == "total":
        return "all"
    when = (when or datetime.now(timezone.utc)).astimezone(zone(tz))
    if period == "day":
        return when.strftime("%Y-%m-%d")
    if period == "week":
        iso = when.isocalendar()
        return f"{iso.year}-W{iso.week:02d}"
    return when.strftime("%Y-%m")


def period_bounds(period: str, key: str, tz: str = "UTC") -> tuple:
    """(start, end) of a period as aware UTC instants, from its owner-local boundaries."""
    z = zone(tz)
    if period == "total":
        return datetime(1970, 1, 1, tzinfo=timezone.utc), datetime(2999, 1, 1, tzinfo=timezone.utc)
    if period == "day":
        start_local = datetime.strptime(key, "%Y-%m-%d").replace(tzinfo=z)
        end_local = start_local + timedelta(days=1)
    elif period == "week":
        year, week = key.split("-W")
        start_local = datetime.fromisocalendar(int(year), int(week), 1).replace(tzinfo=z)
        end_local = start_local + timedelta(days=7)
    else:
        start_local = datetime.strptime(key, "%Y-%m").replace(tzinfo=z)
        # Month arithmetic by hand: timedelta has no notion of a month.
        if start_local.month == 12:
            end_local = start_local.replace(year=start_local.year + 1, month=1)
        else:
            end_local = start_local.replace(month=start_local.month + 1)
    return start_local.astimezone(timezone.utc), end_local.astimezone(timezone.utc)


def period_label(period: str, key: str) -> str:
    """A budget period as a label, in the owner's own time zone."""
    return {"day": f"on {key}", "week": f"in week {key}", "month": f"in {key}",
            "total": "in total"}.get(period, key)


# ---------------------------------------------------------------------------- state

@dataclass
class BudgetState:
    """Where a budget stands: spent, remaining, and whether it is exhausted."""
    budget: Optional[Budget]
    limit: float
    spent: float
    reserved: float
    currency: str = "USD"
    period: str = "month"
    key: str = ""

    @property
    def committed(self) -> float:
        """Everything counted against the cap: settled spend plus outstanding reservations."""
        return self.spent + self.reserved

    @property
    def remaining(self) -> float:
        """How much of the budget is left."""
        return max(0.0, self.limit - self.committed)

    @property
    def exhausted(self) -> bool:
        """Whether the budget has been used up."""
        return self.budget is not None and self.limit > 0 and self.committed >= self.limit

    @property
    def fraction(self) -> float:
        """How much of the budget has been used, as a fraction."""
        return min(1.0, self.committed / self.limit) if self.limit > 0 else 0.0

    def client(self) -> dict:
        """The budget state as the client shows it."""
        if self.budget is None:
            return {"limited": False}
        return {"limited": True, "period": self.period, "period_key": self.key,
                "limit": round(self.limit, 4), "spent": round(self.spent, 4),
                "reserved": round(self.reserved, 4), "committed": round(self.committed, 4),
                "remaining": round(self.remaining, 4), "fraction": round(self.fraction, 4),
                "currency": self.currency, "behavior": self.budget.behavior,
                "exhausted": self.exhausted,
                "label": f"{self.currency} {self.committed:.2f} of {self.limit:.2f} "
                         f"{period_label(self.period, self.key)}"}


def owner_tz(session, grant: AccessGrant) -> str:
    """A budget's period boundaries follow the *server owner's* clock, not the user's.

    The cap protects the owner's wallet, so it is their month that matters. A user in Tokyo
    spending against a New York owner's budget rolls over at New York's midnight.
    """
    group = session.get(ServerGroup, grant.group_id)
    if group is None:
        return "UTC"
    owner = session.get(User, group.owner_id)
    return owner.tz if owner is not None else "UTC"


def state(session, grant: AccessGrant, *, when: Optional[datetime] = None) -> BudgetState:
    """Where a grant's budget stands right now."""
    budget = session.scalar(select(Budget).where(Budget.grant_id == grant.id))
    if budget is None:
        return BudgetState(budget=None, limit=0.0, spent=0.0, reserved=0.0)

    tz = owner_tz(session, grant)
    key = period_key(budget.period, when, tz)
    rows = list(session.scalars(select(BudgetEntry).where(
        BudgetEntry.budget_id == budget.id, BudgetEntry.period_key == key)))
    spent = sum(r.actual for r in rows if r.actual is not None)
    # An entry that is still open counts at its estimate; that is the whole point of a reservation.
    reserved = sum(r.reserved for r in rows if r.actual is None and r.closed_at is None)
    return BudgetState(budget=budget, limit=float(budget.amount or 0.0), spent=spent,
                       reserved=reserved, currency=budget.currency or "USD",
                       period=budget.period, key=key)


# ---------------------------------------------------------------------------- reservations

def estimate_cost(session, server: Server, *, hours: float = DEFAULT_RESERVE_HOURS) -> float:
    """A conservative guess at what one activity on this server will cost.

    Uses the registry's own hourly profile, so a machine with a known electricity tariff and
    depreciation produces a real number rather than a guess.
    """
    try:
        from .. import costing, servers as registry
        config = server.config or {}
        if not config.get("power"):
            return MIN_RESERVE
        profile = costing.hourly_profile(config, registry.load(), busy_fraction=1.0)
        return max(MIN_RESERVE, float(profile.get("total") or 0.0) * hours)
    except Exception:
        # Costing depends on the registry file and a lot of optional configuration. A budget must
        # never be the reason a game cannot start, so fall back rather than propagate.
        log.debug("could not estimate cost for server %s", server.id, exc_info=True)
        return MIN_RESERVE


def reserve(session, grant: AccessGrant, activity_id: str, *, user_id: Optional[str] = None,
            amount: float = MIN_RESERVE, when: Optional[datetime] = None) -> Optional[BudgetEntry]:
    """Claim headroom before work starts. Returns None when the grant has no budget."""
    budget = session.scalar(select(Budget).where(Budget.grant_id == grant.id))
    if budget is None:
        return None
    existing = session.scalar(select(BudgetEntry).where(
        BudgetEntry.budget_id == budget.id, BudgetEntry.activity_id == activity_id))
    if existing is not None:
        return existing
    tz = owner_tz(session, grant)
    entry = BudgetEntry(
        budget_id=budget.id, activity_id=activity_id, user_id=user_id,
        reserved=max(0.0, float(amount)),
        # Fixed once, at open time, so a game running across a month boundary keeps counting against
        # the period it started in instead of silently jumping budgets mid-flight.
        period_key=period_key(budget.period, when, tz))
    session.add(entry)
    session.flush()
    return entry


def settle(session, activity_id: str, actual: float) -> int:
    """Replace an estimate with the measured cost."""
    rows = list(session.scalars(select(BudgetEntry).where(BudgetEntry.activity_id == activity_id)))
    for row in rows:
        row.actual = max(0.0, float(actual))
        row.closed_at = datetime.now(timezone.utc)
    return len(rows)


def release(session, activity_id: str) -> int:
    """Drop a reservation for work that never ran."""
    rows = list(session.scalars(select(BudgetEntry).where(
        BudgetEntry.activity_id == activity_id, BudgetEntry.actual.is_(None))))
    for row in rows:
        session.delete(row)
    return len(rows)


def refresh_from_ledger(session, *, since: Optional[float] = None) -> int:
    """True up open reservations against what the usage ledger actually recorded.

    Called periodically. This is what turns the cap from advisory into real: a long game's estimate
    is replaced by its running cost every time this runs, so the admission controller sees the
    truth rather than a number chosen at launch.
    """
    open_rows = list(session.scalars(select(BudgetEntry).where(BudgetEntry.closed_at.is_(None))))
    if not open_rows:
        return 0
    try:
        from .. import costing
        priced = costing.compute(since=since)
    except Exception:
        log.debug("could not price the usage ledger", exc_info=True)
        return 0

    acts = priced.get("acts") or {}
    updated = 0
    for row in open_rows:
        act = acts.get(row.activity_id)
        if not act:
            continue
        total = 0.0
        for cost in (act.get("servers") or {}).values():
            try:
                total += costing.total(cost)
            except Exception:
                continue
        if total > 0:
            # Keep it as a live reservation rather than settling: the activity may still be running,
            # and `reserved` is what the admission check reads for in-flight work.
            row.reserved = max(row.reserved if row.actual is None else 0.0, total)
            updated += 1
    return updated


def over_budget_activities(session) -> list:
    """Activity ids whose grant has a hard cap that is now exhausted — these get paused."""
    out = []
    for grant in session.scalars(select(AccessGrant).where(AccessGrant.enabled.is_(True))):
        st = state(session, grant)
        if st.budget is None or st.budget.behavior != "hard" or not st.exhausted:
            continue
        rows = session.scalars(select(BudgetEntry).where(
            BudgetEntry.budget_id == st.budget.id, BudgetEntry.period_key == st.key,
            BudgetEntry.closed_at.is_(None)))
        out += [r.activity_id for r in rows]
    return out


# ---------------------------------------------------------------------------- editing

def set_budget(session, grant: AccessGrant, *, period: str, amount: float, currency: str = "USD",
               behavior: str = "hard") -> Optional[Budget]:
    """Create, update or (with amount <= 0) remove a grant's budget."""
    if period not in PERIODS:
        raise ValueError(f"Budget period must be one of {', '.join(PERIODS)}.")
    if behavior not in ("hard", "soft"):
        raise ValueError("Budget behavior is 'hard' or 'soft'.")
    budget = session.scalar(select(Budget).where(Budget.grant_id == grant.id))
    if amount is None or float(amount) <= 0:
        if budget is not None:
            session.delete(budget)
        return None
    if budget is None:
        budget = Budget(grant_id=grant.id)
        session.add(budget)
    budget.period = period
    budget.amount = float(amount)
    budget.currency = (currency or "USD")[:8]
    budget.behavior = behavior
    session.flush()
    return budget
