"""Spend caps on shared machines, in credits (design/phase3-features.md P3.4.6).

No money moves anywhere. A budget is **accounting and admission control**: it stops work being
scheduled, and it pauses work that crosses the line. Nobody is charged and nothing is billed.

A budget belongs to a grant and is counted in credits, the compute ledger's unit (`Budget.unit`), per
period (a day, a week, a month or in total) in the machine owner's time zone, hard or soft. It is kept
per request, against the ledger:

*Bind.* A lease or a ticket is bound to the **grant** that admitted it (``Keeper.bind``), not to the
budget that grant had then: each request asks the keeper for the grant's budget as it is now
(``Keeper.budget_of``), so a budget added to, changed on, replaced on or removed from a grant holds the
work already running under it from its next request.

*Reserve.* When a request of a lease or a ticket under a grant with a budget is given its slot, the
scheduler reserves an estimate (``Keeper.reserve``): the machine's mean credits per step for that model
(at least ``MIN_RESERVE``), or on an online API the price of the request's ``max_tokens`` of output plus
its input, times the class rate. A hard budget whose spent plus reserved would pass its amount refuses
the request (``BudgetExceeded``, with the period's end as ``retry_at``): a lobby game pauses until the
period turns, a ticket is refused and its caller falls back. A hard budget on an API whose model has no
price refuses every request, rather than letting unpriced steps through at 0. Everything here runs in
memory, under the keeper's own lock: the request path never reads the database.

*Settle.* The ledger's listener prices each step and settles its hold in memory at once
(``Keeper.settle``, on the stepping thread): the step's credits count as spent in its period and its
reservation goes, so the next request is checked against what this one cost. The ledger's writer then
writes the event and the entry of (budget, activity, period) in one transaction (``settle_entries``) and
tells the keeper (``Keeper.written``), which until then counts those credits as settled but unwritten,
so that a budget read again from the database meanwhile counts each credit once. A step that never ran
releases its reservation.

*Soft* budgets never refuse: the request that takes one past its amount carries a warning (once a
period), which the scheduler hands to the binding's ``on_budget`` (a game's log says so) and an audit
row records; the machine owner's Servers page shows the meter over its amount.

*A period ends at the owner's midnight, not UTC's.* "1,000 credits per month" means their month. An
owner in New York gets a month that runs from the 1st at 00:00 Eastern; computing it in UTC would move
the boundary by five hours and, twice a year, by a different five hours.

``refresh_from_ledger`` rebuilds a period's entries from ``compute_events``: an administrator's repair
(``citar admin refresh-budgets``, often another process). It leaves a mark (``REFRESHED_KEY``) that the
server's ledger reads at its next rollup pass (``Keeper.sync``), which then reads every budget's spend
again from the rebuilt entries.
"""
from __future__ import annotations

import logging
import re
import secrets
import threading
import time
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from typing import Callable, Optional

from sqlalchemy import func, select

from ..db.models import BUDGET_UNITS, AccessGrant, Budget, BudgetEntry, ServerGroup, User
from .windows import zone

log = logging.getLogger("citar.budgets")

PERIODS = ("day", "week", "month", "total")

#: The least a request reserves, in credits (P3.4.6: "at least 1"), and the least a hard budget must have left to
#: admit new work at all (``admission.check``).
MIN_RESERVE = 1.0

#: The ``settings_kv`` key ``refresh_from_ledger`` marks, so a running server reads the rebuilt entries (``Keeper.sync``).
REFRESHED_KEY = "budgets_refreshed"

#: How many times a budget's spend is read again when the ledger's writer committed meanwhile (``Keeper._load``).
LOAD_TRIES = 6

#: The form of a period's key, per kind of period (``refresh_from_ledger``'s ``key``).
KEY_FORMS = {"day": re.compile(r"^\d{4}-\d{2}-\d{2}$"), "week": re.compile(r"^\d{4}-W\d{2}$"),
             "month": re.compile(r"^\d{4}-\d{2}$"), "total": re.compile(r"^all$")}


def key_fits(period: str, key: str) -> bool:
    """Whether ``key`` is a key of a ``period`` budget (``2026-10-12`` a day's, ``2026-W41`` a week's, ``2026-10`` a
    month's, ``all`` a total's)."""
    form = KEY_FORMS.get(period)
    return bool(form is not None and key and form.match(key))


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
    unit: str = "credits"
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
        """The budget state as the client shows it. A soft budget past its amount says so (``warning``): the note on
        the machine owner's Servers page."""
        if self.budget is None:
            return {"limited": False}
        over = self.limit > 0 and self.committed >= self.limit
        return {"limited": True, "period": self.period, "period_key": self.key,
                "limit": round(self.limit, 4), "spent": round(self.spent, 4),
                "reserved": round(self.reserved, 4), "committed": round(self.committed, 4),
                "remaining": round(self.remaining, 4), "fraction": round(self.fraction, 4),
                "unit": self.unit, "behavior": self.budget.behavior,
                "exhausted": self.exhausted,
                "warning": (f"Past its soft budget {period_label(self.period, self.key)}: work goes on."
                            if over and self.budget.behavior == "soft" else None),
                "label": f"{self.committed:.2f} of {self.limit:.2f} {self.unit} "
                         f"{period_label(self.period, self.key)}"}


def owner_tz(session, grant: AccessGrant) -> str:
    """A budget's period boundaries follow the *server owner's* clock, not the user's.

    The cap protects the owner's machine, so it is their month that matters. A user in Tokyo
    spending against a New York owner's budget rolls over at New York's midnight.
    """
    group = session.get(ServerGroup, grant.group_id)
    if group is None:
        return "UTC"
    owner = session.get(User, group.owner_id) if group.owner_id else None
    return owner.tz if owner is not None else "UTC"


def state(session, grant: AccessGrant, *, when: Optional[datetime] = None) -> BudgetState:
    """Where a grant's budget stands right now: the period's settled entries, plus what this process has reserved
    for requests running under it (``Keeper``)."""
    budget = session.scalar(select(Budget).where(Budget.grant_id == grant.id))
    if budget is None:
        return BudgetState(budget=None, limit=0.0, spent=0.0, reserved=0.0)

    tz = owner_tz(session, grant)
    key = period_key(budget.period, when, tz)
    rows = list(session.scalars(select(BudgetEntry).where(
        BudgetEntry.budget_id == budget.id, BudgetEntry.period_key == key)))
    spent = sum(r.actual for r in rows if r.actual is not None)
    # an entry from before per-request settling (one reservation per activity) counts at its estimate while open
    reserved = sum(r.reserved for r in rows if r.actual is None and r.closed_at is None)
    reserved += keeper().reserved(budget.id, key)
    return BudgetState(budget=budget, limit=float(budget.amount or 0.0), spent=spent,
                       reserved=reserved, unit=budget.unit or "credits",
                       period=budget.period, key=key)


# ---------------------------------------------------------------------------- the keeper: reservations in memory

class BudgetExceeded(Exception):
    """A hard budget refuses a request. ``retry_at`` (aware UTC) is when its period ends (None for a total budget,
    or an API model with no price: waiting does not help)."""

    def __init__(self, message: str, *, budget_id: str, key: str, retry_at: Optional[datetime] = None):
        super().__init__(message)
        self.budget_id = budget_id
        self.key = key
        self.retry_at = retry_at


@dataclass
class Reservation:
    """One request's hold on a budget: settled by the ledger's listener when the step is metered (its entry written by
    the ledger's writer), or released."""
    budget_id: str
    activity: str
    consumer: Optional[str]
    key: str
    amount: float
    id: str = field(default_factory=lambda: secrets.token_hex(6))
    #: Set when the request took a soft budget past its amount (once a period): the words to tell the consumer.
    warning: Optional[str] = None
    done: bool = False


@dataclass
class Account:
    """A budget as the keeper holds it: its terms, the current period's settled spend, and the reservations open."""
    budget_id: str
    grant_id: str
    amount: float
    behavior: str
    period: str
    tz: str
    group: str
    key: str
    spent: float
    open: dict = field(default_factory=dict)          # reservation id -> Reservation
    warned: Optional[str] = None                      # the period a soft budget's warning went out in

    def reserved(self, key: Optional[str] = None) -> float:
        """What the open reservations of period ``key`` (the current one by default) hold."""
        key = key or self.key
        return sum(r.amount for r in self.open.values() if r.key == key)


class Keeper:
    """Every budget of a grant this process has made a binding under, in memory (P3.4.6): reserved and settled on the
    request path without the database; the ledger's writer persists the entries. ``clock`` gives the aware UTC now
    (tests move it).

    A binding names its grant (``bind``); ``grants`` maps each such grant to its budget now, kept up to date by the
    grant routes (``reload_grant``) and the ledger's rollup pass (``sync``). An account's spend is the database's
    entries of its period plus what was settled here and not written yet (``_unwritten``): it is read from the database
    when the account is first loaded, when its period changes kind or zone, and after an administrator's repair;
    otherwise it is kept in memory, an edit of the budget's amount or behaviour changing only its terms."""

    def __init__(self, clock: Optional[Callable[[], datetime]] = None):
        self.lock = threading.Lock()
        self.accounts: dict[str, Account] = {}
        #: grant id -> the id of its budget now, or None: every grant a binding was made under here.
        self.grants: dict[str, Optional[str]] = {}
        self.clock = clock or (lambda: datetime.now(timezone.utc))
        #: reservation id -> (budget, period key, credits): settled here, their entries not committed by the writer yet.
        self._unwritten: dict[str, tuple] = {}
        self._writing = 0           # the ledger's writer's batches in a transaction now
        self._written = 0           # its batches finished (committed or failed) so far
        #: The last ``REFRESHED_KEY`` mark ``sync`` read (None: none read yet).
        self.refreshed: Optional[str] = None

    # ---- loading (database reads: off every lock but the keeper's own, never on the request path)
    def bind(self, session, grant: Optional[AccessGrant]) -> Optional[str]:
        """What a binding (a lease, a ticket) made under ``grant`` keeps: the grant's id (None for no grant), its
        budget's account loaded here. Its requests reserve against whatever budget the grant has when they run
        (``budget_of``). Called where a binding is made, which may read the database."""
        if grant is None:
            return None
        self.handle(session, grant)
        return grant.id

    def handle(self, session, grant: Optional[AccessGrant]) -> Optional[str]:
        """The id of ``grant``'s budget, its account loaded (or kept) here, and the grant followed from now on; None
        when it has none."""
        if grant is None:
            return None
        budget = session.scalar(select(Budget).where(Budget.grant_id == grant.id))
        if budget is not None:
            with self.lock:
                known = budget.id in self.accounts
            if not known:
                self._load(session, budget, grant)
        with self.lock:
            self.grants[grant.id] = budget.id if budget is not None else None
        return budget.id if budget is not None else None

    def budget_of(self, grant_id: Optional[str]) -> Optional[str]:
        """The budget a request bound to ``grant_id`` reserves against now, or None (no grant, or no budget)."""
        if not grant_id:
            return None
        with self.lock:
            return self.grants.get(grant_id)

    def reload(self, session, budget_id: str, *, fresh: bool = False) -> None:
        """Read a budget again (its owner changed it; ``fresh``: its spend too, after a repair); forget a deleted
        one."""
        budget = session.get(Budget, budget_id)
        grant = session.get(AccessGrant, budget.grant_id) if budget is not None else None
        if budget is None or grant is None:
            with self.lock:
                self.accounts.pop(budget_id, None)
            return
        self._load(session, budget, grant, fresh=fresh)

    def reload_grant(self, session, grant_id: str, *, fresh: bool = False) -> None:
        """A grant's budget changed, was replaced or went (or the grant did): read whatever it has now, so the work
        bound to it is held to that from its next request. Nothing to do for a grant no binding here was made
        under."""
        grant = session.get(AccessGrant, grant_id)
        budget = (session.scalar(select(Budget).where(Budget.grant_id == grant_id)) if grant is not None else None)
        with self.lock:
            followed = grant_id in self.grants
            gone = [bid for bid, a in self.accounts.items()
                    if a.grant_id == grant_id and (budget is None or bid != budget.id)]
            for bid in gone:
                self.accounts.pop(bid, None)
        if not followed and not gone:
            return
        if budget is not None:
            self._load(session, budget, grant, fresh=fresh)
        with self.lock:
            if grant is None:
                self.grants.pop(grant_id, None)         # revoked: nothing is bound to it any more that has a budget
            else:
                self.grants[grant_id] = budget.id if budget is not None else None

    def sync(self, session) -> int:
        """Read again what every followed grant's budget is and its terms (the ledger's rollup pass, every few
        minutes): an edit this process did not hear of holds from then on. After an administrator's repair
        (``refresh_from_ledger``'s mark changed) every account's spend is read again from the rebuilt entries.
        Returns the grants read."""
        from ..db.models import Setting
        row = session.get(Setting, REFRESHED_KEY)
        mark = str((row.value or {}).get("v")) if row is not None and isinstance(row.value, dict) else None
        fresh = mark != self.refreshed
        with self.lock:
            grants = list(self.grants)
        for gid in grants:
            self.reload_grant(session, gid, fresh=fresh)
        self.refreshed = mark
        return len(grants)

    def _load(self, session, budget: Budget, grant: AccessGrant, *, fresh: bool = False) -> None:
        """Load a budget's account. One already here keeps its spend, its holds and its warning unless ``fresh`` or
        its period changed kind or zone (then the spend is read from the database, as for a new one)."""
        tz = owner_tz(session, grant)
        group = session.get(ServerGroup, grant.group_id)
        name = group.name if group is not None else "that group"
        period = budget.period or "month"
        amount, behavior = float(budget.amount or 0.0), budget.behavior or "hard"
        with self.lock:
            old = self.accounts.get(budget.id)
            if old is not None and not fresh and old.period == period and old.tz == tz:
                old.grant_id, old.amount, old.behavior, old.group = grant.id, amount, behavior, name
                return
        # The spend: the period's entries in the database plus what was settled here and is not written yet. The
        # entries are read in a transaction of their own, begun after the writer's state was noted, and read again
        # when the writer finished or began a batch meanwhile (whose credits could then be counted twice, or not at
        # all); the account is put in place under the same lock as that check, so a step settled meanwhile is in the
        # sum it starts from. After ``LOAD_TRIES`` busy reads the last is kept, which can only count a batch twice:
        # it errs on the side of the budget.
        from .. import db
        for attempt in range(LOAD_TRIES):
            with self.lock:
                key = period_key(period, self.clock(), tz)
                gen, busy = self._written, self._writing
            q = select(func.coalesce(func.sum(BudgetEntry.actual), 0.0)).where(
                BudgetEntry.budget_id == budget.id, BudgetEntry.period_key == key)
            if db.configured():
                with db.session() as s:
                    stored = float(s.scalar(q) or 0.0)
            else:
                stored = float(session.scalar(q) or 0.0)
            with self.lock:
                quiet = busy == 0 and self._writing == 0 and self._written == gen
                if quiet or attempt == LOAD_TRIES - 1:
                    pending = sum(c for b, k, c in self._unwritten.values() if b == budget.id and k == key)
                    old = self.accounts.get(budget.id)
                    self.accounts[budget.id] = Account(
                        budget_id=budget.id, grant_id=grant.id, amount=amount, behavior=behavior, period=period,
                        tz=tz, group=name, key=key, spent=stored + pending,
                        open=old.open if old is not None else {},
                        warned=old.warned if old is not None and old.key == key else None)
                    return
            time.sleep(0.005 * (attempt + 1))

    def forget(self, budget_id: str) -> None:
        with self.lock:
            self.accounts.pop(budget_id, None)

    # ---- the ledger's writer
    def writing(self) -> None:
        """The ledger's writer begins a batch's transaction."""
        with self.lock:
            self._writing += 1

    def written(self, holds: list) -> None:
        """The ledger's writer finished a batch (committed, or failed and gave it up): the entries of ``holds`` (the
        reservations it settled) are the database's now, or lost with their events."""
        with self.lock:
            for r in holds:
                if r is not None:
                    self._unwritten.pop(r.id, None)
            self._writing = max(0, self._writing - 1)
            self._written += 1

    # ---- the request path (in memory only)
    def _turn(self, a: Account) -> None:
        """The account's current period: a turned period starts with nothing spent. Keeper's lock held."""
        key = period_key(a.period, self.clock(), a.tz)
        if key != a.key:
            a.key, a.spent = key, 0.0

    def current_key(self, budget_id: str) -> Optional[str]:
        """The budget's current period (a game paused for it resumes when this changes), or None for no such one."""
        with self.lock:
            a = self.accounts.get(budget_id)
            if a is None:
                return None
            self._turn(a)
            return a.key

    def reserve(self, budget_id: Optional[str], activity: Optional[str], consumer: Optional[str], estimate: float,
                *, unpriced: bool = False, force: bool = False) -> Optional[Reservation]:
        """Hold ``estimate`` credits (at least ``MIN_RESERVE``) of a budget for one request, or raise
        ``BudgetExceeded`` when a hard budget's spent plus reserved would pass its amount (or ``unpriced``: an API
        model with no price, which a hard budget cannot keep). None when there is no such budget. ``force`` never
        refuses: the hold of work already done (a CPU run carried across the server's restart, whose hold was in
        memory), so its credits are settled into the budget all the same."""
        if not budget_id:
            return None
        with self.lock:
            a = self.accounts.get(budget_id)
            if a is None:
                return None
            self._turn(a)
            amount = max(MIN_RESERVE, float(estimate or 0.0))
            committed = a.spent + a.reserved()
            hard = a.behavior == "hard" and a.amount > 0 and not force
            if hard and unpriced:
                raise BudgetExceeded(f"The budget for “{a.group}” cannot be kept: this model has no price, so its "
                                     "budget cannot be kept.", budget_id=a.budget_id, key=a.key)
            if hard and committed + amount > a.amount:
                end = period_bounds(a.period, a.key, a.tz)[1] if a.period != "total" else None
                raise BudgetExceeded(
                    f"The budget for “{a.group}” is used up ({committed:.2f} of {a.amount:.2f} credits "
                    f"{period_label(a.period, a.key)}, and this request would take about {amount:.2f} more). "
                    + ("It resets when the period ends." if end is not None else "Its owner would have to raise it."),
                    budget_id=a.budget_id, key=a.key, retry_at=end)
            r = Reservation(budget_id=a.budget_id, activity=activity or "", consumer=consumer, key=a.key,
                            amount=amount)
            if a.behavior == "soft" and a.amount > 0 and committed + amount >= a.amount and a.warned != a.key:
                a.warned = a.key
                r.warning = (f"Past the soft budget for “{a.group}” ({committed + amount:.2f} of {a.amount:.2f} "
                             f"credits {period_label(a.period, a.key)}); the work goes on.")
            a.open[r.id] = r
            return r

    def settle(self, r: Optional[Reservation], credits: float) -> None:
        """The request's step was metered at ``credits`` (the ledger's listener): they count as spent in its period
        and the hold goes. Its entry is the ledger's writer's to write; until it says so (``written``) the credits are
        counted as settled but unwritten."""
        if r is None or r.done:
            return
        credits = max(0.0, float(credits or 0.0))
        with self.lock:
            r.done = True
            self._unwritten[r.id] = (r.budget_id, r.key, credits)
            a = self.accounts.get(r.budget_id)
            if a is None:
                return
            a.open.pop(r.id, None)
            self._turn(a)
            if r.key == a.key:
                a.spent += credits

    def refund(self, budget_id: str, key: str, credits: float) -> None:
        """Credits settled in period ``key`` of a budget taken back (a CPU run rejected by verification, or bounded
        again lower once its replica gave its reference cost, P4.5.7): its entry was lowered by the ledger's writer, and
        the account's spend now is too, when that period is current. A negative ``credits`` is a charge (a run bounded
        again higher: the work was done, whatever the budget has left)."""
        with self.lock:
            a = self.accounts.get(budget_id)
            if a is None:
                return
            self._turn(a)
            if key == a.key:
                a.spent = max(0.0, a.spent - float(credits or 0.0))

    def release(self, r: Optional[Reservation]) -> None:
        """The request never ran (or its event could not be written): the hold goes, nothing is spent."""
        if r is None or r.done:
            return
        with self.lock:
            r.done = True
            a = self.accounts.get(r.budget_id)
            if a is not None:
                a.open.pop(r.id, None)

    def reserved(self, budget_id: str, key: Optional[str] = None) -> float:
        """What the open reservations of a budget hold (in period ``key``, the current one by default)."""
        with self.lock:
            a = self.accounts.get(budget_id)
            return a.reserved(key) if a is not None else 0.0

    def account(self, budget_id: str) -> Optional[dict]:
        """A budget's live state, for tests and status: amount, spent, reserved, period."""
        with self.lock:
            a = self.accounts.get(budget_id)
            if a is None:
                return None
            self._turn(a)
            return {"amount": a.amount, "spent": a.spent, "reserved": a.reserved(), "key": a.key,
                    "behavior": a.behavior, "period": a.period}


_KEEPER: Optional[Keeper] = None
_KEEPER_LOCK = threading.Lock()


def keeper() -> Keeper:
    """The process's budget keeper, made on first use."""
    global _KEEPER
    with _KEEPER_LOCK:
        if _KEEPER is None:
            _KEEPER = Keeper()
        return _KEEPER


def reset_keeper(clock: Optional[Callable[[], datetime]] = None) -> Keeper:
    """A fresh keeper (tests), on ``clock`` when given."""
    global _KEEPER
    with _KEEPER_LOCK:
        _KEEPER = Keeper(clock)
        return _KEEPER


# ---------------------------------------------------------------------------- settling (the ledger's writer)

def settle_entries(session, settled: list) -> None:
    """Add each settled request's credits to its entry of (budget, activity, period), made on first use; in the
    ledger writer's transaction, with the events. ``settled`` is ``[(Reservation, credits)]``."""
    totals: dict = {}
    for r, credits in settled:
        if r is None:
            continue
        key = (r.budget_id, r.activity, r.key)
        cur = totals.setdefault(key, [0.0, r.consumer])
        cur[0] += max(0.0, float(credits or 0.0))
    for (budget_id, activity, key), (credits, consumer) in totals.items():
        if session.get(Budget, budget_id) is None:
            continue                     # deleted meanwhile: nothing to count against
        entry = session.scalar(select(BudgetEntry).where(
            BudgetEntry.budget_id == budget_id, BudgetEntry.activity_id == activity[:64],
            BudgetEntry.period_key == key))
        if entry is None:
            entry = BudgetEntry(budget_id=budget_id, activity_id=activity[:64], user_id=consumer, reserved=0.0,
                                actual=0.0, period_key=key)
            session.add(entry)
        entry.actual = float(entry.actual or 0.0) + credits


def refresh_from_ledger(session, budget: Optional[Budget] = None, *, key: Optional[str] = None,
                        when: Optional[datetime] = None) -> int:
    """Rebuild a period's entries of ``budget`` (every budget when None; the current period, or ``key``) from
    ``compute_events``: the administrator's repair, when an entry was lost (the server stopped between an event and its
    settling) or the credits were repriced. An event counts against a grant's budget when the grant admits its
    consumer for its kind of work on its machine: its consumer the grant's account (for a grant to everyone, any member
    without a grant of their own on the group), its machine in the grant's group and not theirs, its consumer no
    administrator, not self-use, and its purpose (``ledger.purpose_of``) among the grant's.

    ``key`` names a period of one kind (``2026-10-12`` a day, ``2026-W41`` a week, ``2026-10`` a month, ``all`` a
    total): with every budget it rebuilds only the budgets counted in periods of that kind (the others are left as they
    are); with one ``budget`` of another kind it is a ``ValueError``. Returns the entries written. It marks the
    rebuild (``REFRESHED_KEY``), so a running server reads every budget's spend again at its ledger's next rollup pass
    (``Keeper.sync``)."""
    from sqlalchemy import func
    from .. import ledger
    from ..db.models import ComputeEvent, Server, Setting
    if key is not None and not any(key_fits(p, key) for p in PERIODS):
        raise ValueError(f"{key!r} is no period: a day is 2026-10-12, a week 2026-W41, a month 2026-10, a total all.")
    if budget is not None and key is not None and not key_fits(budget.period, key):
        raise ValueError(f"{key!r} is not a period of this budget, which is counted by the {budget.period}.")
    budgets = [budget] if budget is not None else list(session.scalars(select(Budget)))
    if key is not None:
        budgets = [b for b in budgets if key_fits(b.period, key)]
    written = 0
    for b in budgets:
        grant = session.get(AccessGrant, b.grant_id)
        if grant is None:
            continue
        tz = owner_tz(session, grant)
        k = key or period_key(b.period, when, tz)
        start, end = period_bounds(b.period, k, tz)
        machines = dict(session.execute(
            select(Server.id, Server.owner_id).where(Server.group_id == grant.group_id)).all())
        if not machines:
            continue
        admins = {uid for (uid,) in session.execute(select(User.id).where(User.role == "admin"))}
        own_grants = {uid for (uid,) in session.execute(select(AccessGrant.subject_user_id).where(
            AccessGrant.group_id == grant.group_id, AccessGrant.subject_type == "user",
            AccessGrant.enabled.is_(True)))}
        q = select(ComputeEvent.activity, ComputeEvent.consumer_id, ComputeEvent.machine_id, ComputeEvent.task_class,
                   ComputeEvent.activity_kind, func.sum(ComputeEvent.credits)).where(
            ComputeEvent.at >= start, ComputeEvent.at < end, ComputeEvent.self_use.is_(False),
            ComputeEvent.machine_id.in_(list(machines)), ComputeEvent.consumer_id.isnot(None))
        if grant.subject_type == "user":
            q = q.where(ComputeEvent.consumer_id == grant.subject_user_id)
        q = q.group_by(ComputeEvent.activity, ComputeEvent.consumer_id, ComputeEvent.machine_id,
                       ComputeEvent.task_class, ComputeEvent.activity_kind)
        purposes = set(grant.purposes or [])
        per: dict = {}
        for activity, consumer, machine, task_class, kind, credits in session.execute(q):
            if consumer in admins or consumer == machines.get(machine):
                continue
            if grant.subject_type != "user" and consumer in own_grants:
                continue
            if purposes and ledger.purpose_of(task_class, kind) not in purposes:
                continue
            cur = per.setdefault(activity, [0.0, consumer])
            cur[0] += float(credits or 0.0)
        for entry in session.scalars(select(BudgetEntry).where(BudgetEntry.budget_id == b.id,
                                                               BudgetEntry.period_key == k)):
            session.delete(entry)
        session.flush()
        for activity, (credits, consumer) in per.items():
            session.add(BudgetEntry(budget_id=b.id, activity_id=activity[:64], user_id=consumer, reserved=0.0,
                                    actual=credits, period_key=k))
            written += 1
        session.flush()
    mark = session.get(Setting, REFRESHED_KEY)
    value = {"v": secrets.token_hex(8), "at": datetime.now(timezone.utc).isoformat(timespec="seconds")}
    if mark is None:
        session.add(Setting(key=REFRESHED_KEY, value=value))
    else:
        mark.value = value
    session.flush()
    return written


# ---------------------------------------------------------------------------- editing

def set_budget(session, grant: AccessGrant, *, period: str, amount: float, behavior: str = "hard",
               unit: str = "credits") -> Optional[Budget]:
    """Create, update or (with amount <= 0) remove a grant's budget, in credits (the only unit). The keeper reads it
    again once the change is committed (``Keeper.reload_grant``, the routes')."""
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
    if unit not in BUDGET_UNITS:
        raise ValueError(f"A budget is counted in {', '.join(BUDGET_UNITS)}.")
    budget.unit = unit
    budget.behavior = behavior
    session.flush()
    return budget
