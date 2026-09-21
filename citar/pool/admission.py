"""Admission control: may this person run this work on that server, right now?

Every LLM seat in a game passes through `check()` before the game starts. The checks run in a fixed
order, cheapest and most absolute first, and each failure produces a **specific, quotable reason**:

    grant exists  ->  purpose allowed  ->  inside a window  ->  budget remains
                  ->  concurrency slot free  ->  server actually online

A generic "access denied" is close to useless here. The interesting cases are all temporary — the
overnight box opens at midnight, the budget resets on the 1st, somebody else has the only slot — and
a person who is told *which* one and *when it clears* can decide whether to queue or pick another
server. So `Decision` carries a `retry_at`, and the lobby offers to queue instead of failing.

The owner's machine has the last word. These checks run on the server, but a worker also enforces
its own windows and concurrency locally, so a bug here cannot run somebody's GPU at 3am against
their wishes.
"""
from __future__ import annotations

import logging
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Optional

from sqlalchemy import select

from ..db.models import AccessGrant, AvailabilityWindow, Server, ServerGroup, User
from . import budgets, windows

log = logging.getLogger("citar.admission")

PURPOSES = ("game", "benchmark", "probe", "scenario", "lab")

PURPOSE_LABELS = {
    "game": "ordinary games", "benchmark": "benchmarks", "probe": "probes",
    "scenario": "scenarios", "lab": "lab runs",
}


@dataclass
class Decision:
    """The answer, and enough context to act on a refusal."""
    allowed: bool
    reason: str = ""
    #: Set when the refusal is temporary. The lobby offers to queue until then.
    retry_at: Optional[datetime] = None
    code: str = ""
    grant: Optional[AccessGrant] = None
    server: Optional[Server] = None
    budget: Optional[budgets.BudgetState] = None

    @property
    def temporary(self) -> bool:
        """Whether a refusal is temporary - a window, a busy machine - rather than permanent.

        The difference decides whether the caller should wait or give up, and a scheduler that cannot tell
        them apart either retries forever or abandons work it could have done.
        """
        return self.retry_at is not None

    def client(self) -> dict:
        """The decision as the client shows it."""
        out = {"allowed": self.allowed, "reason": self.reason, "code": self.code,
               "temporary": self.temporary}
        if self.retry_at:
            out["retry_at"] = self.retry_at.isoformat()
        if self.server is not None:
            out["server"] = {"id": self.server.id, "name": self.server.name}
        if self.budget is not None:
            out["budget"] = self.budget.client()
        return out

    def raise_for_status(self):
        """Raise the right HTTP error for a refusal."""
        from fastapi import HTTPException
        if self.allowed:
            return self
        # 409 rather than 403 for a temporary refusal: nothing about the caller's identity is wrong,
        # the request simply cannot be satisfied yet, and a client can reasonably retry.
        raise HTTPException(409 if self.temporary else 403, self.reason)


def _allow(**kw) -> Decision:
    """An allowing decision."""
    return Decision(allowed=True, **kw)


def _deny(code: str, reason: str, **kw) -> Decision:
    """A refusing decision, with its reason."""
    return Decision(allowed=False, code=code, reason=reason, **kw)


# ---------------------------------------------------------------------------- grants

def grants_for(session, user: Optional[User], group_id: str) -> list:
    """Every live grant this user could use on a group, best first.

    A user may match more than one grant — one addressed to them personally and one to everyone.
    They are sorted so the most generous applies: explicit beats open, higher priority beats lower.
    """
    if user is None:
        return []
    rows = list(session.scalars(select(AccessGrant).where(
        AccessGrant.group_id == group_id, AccessGrant.enabled.is_(True))))
    now = datetime.now(timezone.utc)
    usable = [g for g in rows
              if (g.expires_at is None or g.expires_at > now)
              and (g.subject_type == "everyone"
                   or (g.subject_type == "user" and g.subject_user_id == user.id))]
    usable.sort(key=lambda g: (g.subject_type != "user", -(g.priority or 0)))
    return usable


def _purpose_ok(grant: AccessGrant, purpose: str) -> bool:
    """An empty purpose list means every purpose, which is the common case."""
    allowed = grant.purposes or []
    return not allowed or purpose in allowed


# ---------------------------------------------------------------------------- the check

def check(session, user: Optional[User], server: Server, purpose: str = "game", *,
          now: Optional[datetime] = None, online: Optional[bool] = None,
          in_flight: Optional[int] = None) -> Decision:
    """Whether `user` may run `purpose` on `server` at `now`."""
    now = now or datetime.now(timezone.utc)

    if user is None:
        return _deny("anonymous", "Sign in to use a shared server.")
    if user.status in ("suspended", "deleted", "pending"):
        return _deny("account", "This account cannot run work on shared servers.")
    if purpose not in PURPOSES:
        return _deny("purpose", f"Unknown kind of work: {purpose!r}.")
    if not server.enabled:
        return _deny("disabled", f"“{server.name}” has been switched off by its owner.",
                     server=server)

    # The owner may always use their own hardware, and administrators are not gated by grants. Both
    # still respect the server being switched off, above.
    if server.owner_id == user.id or user.role == "admin":
        return _check_capacity(session, server, None, online=online, in_flight=in_flight, now=now)

    if not server.group_id:
        return _deny("ungrouped",
                     f"“{server.name}” is not shared. Its owner has to put it in a server group "
                     "before anybody else can use it.", server=server)

    group = session.get(ServerGroup, server.group_id)
    if group is None or group.archived_at is not None:
        return _deny("ungrouped", f"“{server.name}” is not in an active server group.", server=server)

    candidates = grants_for(session, user, group.id)
    if not candidates:
        return _deny("no_grant",
                     f"You do not have permission to use “{group.name}”. "
                     "Its owner can grant you access.", server=server)

    # Try each matching grant and keep the most informative refusal: being told "that group is for
    # scenarios only" is more useful than "no grant", even though both end in a refusal.
    best_denial: Optional[Decision] = None
    for grant in candidates:
        decision = _check_grant(session, user, server, grant, purpose, now=now,
                                online=online, in_flight=in_flight)
        if decision.allowed:
            return decision
        if best_denial is None or _rank(decision.code) > _rank(best_denial.code):
            best_denial = decision
    return best_denial or _deny("no_grant", "You do not have permission to use that server.",
                                server=server)


#: How informative each refusal is. A higher-ranked reason is shown in preference to a lower one,
#: because it tells the person more about what would actually let them through.
_RANK = {"no_grant": 0, "purpose": 1, "concurrency": 2, "offline": 3, "window": 4, "budget": 5}


def _rank(code: str) -> int:
    """How preferable one candidate machine is over another."""
    return _RANK.get(code, 0)


def _check_grant(session, user: User, server: Server, grant: AccessGrant, purpose: str, *,
                 now: datetime, online: Optional[bool], in_flight: Optional[int]) -> Decision:
    """Whether a grant currently permits this use: window, budget and capability."""
    group = session.get(ServerGroup, grant.group_id)
    group_name = group.name if group else "that group"

    if not _purpose_ok(grant, purpose):
        allowed = ", ".join(PURPOSE_LABELS.get(p, p) for p in (grant.purposes or []))
        return _deny("purpose",
                     f"Your access to “{group_name}” is limited to {allowed}, "
                     f"and this is {PURPOSE_LABELS.get(purpose, purpose)}.",
                     grant=grant, server=server)

    window_rows = list(session.scalars(select(AvailabilityWindow)
                                       .where(AvailabilityWindow.grant_id == grant.id)))
    tz = budgets.owner_tz(session, grant)
    state = windows.evaluate(window_rows, tz, now)
    if not state.open:
        viewer = windows.describe_for_viewer(window_rows, tz, user.tz or "UTC", now)
        return _deny("window",
                     f"“{group_name}” is available {viewer['owner_text']} {tz}"
                     + (f" (your {viewer['viewer_text']})" if not viewer["same_zone"] else "")
                     + (f". Next opens {windows._relative(state.next_open, now)}."
                        if state.next_open else "."),
                     retry_at=state.next_open, grant=grant, server=server)

    budget_state = budgets.state(session, grant, when=now)
    # Not just "is it exhausted" but "is there room for one more activity". A budget with a penny
    # left would otherwise admit a game that immediately blows through it, and the cap would be
    # exceeded by design rather than by accident.
    if (budget_state.budget is not None and budget_state.budget.behavior == "hard"
            and budget_state.limit > 0
            and budget_state.remaining < budgets.MIN_RESERVE):
        _, period_end = budgets.period_bounds(budget_state.period, budget_state.key, tz)
        used_up = budget_state.exhausted
        return _deny("budget",
                     (f"The budget for “{group_name}” is used up "
                      if used_up else
                      f"The budget for “{group_name}” has too little left to start new work ")
                     + f"({budget_state.currency} {budget_state.committed:.2f} of "
                     f"{budget_state.limit:.2f} {budgets.period_label(budget_state.period, budget_state.key)}). "
                     + (f"It resets {windows._relative(period_end, now)}."
                        if budget_state.period != "total"
                        else "Its owner would have to raise it."),
                     retry_at=period_end if budget_state.period != "total" else None,
                     grant=grant, server=server, budget=budget_state)

    decision = _check_capacity(session, server, grant, online=online, in_flight=in_flight, now=now)
    decision.budget = budget_state
    # A window that is open now but closes soon still admits the work; the scheduler pauses it at
    # the boundary rather than refusing to start something that has hours of useful time left.
    if decision.allowed and state.until is not None:
        decision.retry_at = state.until
    return decision


def _check_capacity(session, server: Server, grant: Optional[AccessGrant], *,
                    online: Optional[bool], in_flight: Optional[int],
                    now: datetime) -> Decision:
    """Slots and liveness. Separated because owners and admins skip the grant checks but not these."""
    if online is False:
        return _deny("offline",
                     f"“{server.name}” is offline — its worker is not connected. "
                     "The owner needs to start citar-worker on that machine.",
                     server=server, grant=grant)

    limit = grant.concurrency if (grant and grant.concurrency) else (server.max_concurrent or 1)
    if in_flight is not None and in_flight >= limit:
        return _deny("concurrency",
                     f"“{server.name}” is busy: {in_flight} of {limit} slots in use. "
                     "It will be picked up when one frees.",
                     server=server, grant=grant)
    return _allow(server=server, grant=grant)


# ---------------------------------------------------------------------------- choosing

def usable_servers(session, user: Optional[User], purpose: str = "game", *,
                   now: Optional[datetime] = None, liveness=None, load=None) -> list:
    """Every server this user could run `purpose` on, with the decision attached.

    Returns refused servers too, each with its reason — the lobby shows them greyed out with "opens
    in 4h 12m" rather than hiding them, so somebody can see what exists and why they cannot use it.
    """
    now = now or datetime.now(timezone.utc)
    out = []
    for server in session.scalars(select(Server).order_by(Server.name)):
        online = liveness(server.id) if liveness else None
        in_flight = load(server.id) if load else None
        decision = check(session, user, server, purpose, now=now, online=online, in_flight=in_flight)
        out.append((server, decision))
    # Usable first, then the most nearly usable.
    out.sort(key=lambda pair: (not pair[1].allowed,
                               pair[1].retry_at or datetime.max.replace(tzinfo=timezone.utc)))
    return out


def pick(session, user: Optional[User], group_id: str, purpose: str = "game", *,
         now: Optional[datetime] = None, liveness=None, load=None) -> Decision:
    """Choose a server from a group for this work, preferring the least loaded usable one."""
    servers = list(session.scalars(select(Server).where(Server.group_id == group_id)))
    if not servers:
        return _deny("empty", "That server group has no servers in it.")
    best: Optional[Decision] = None
    best_load = None
    fallback: Optional[Decision] = None
    for server in servers:
        online = liveness(server.id) if liveness else None
        in_flight = load(server.id) if load else None
        decision = check(session, user, server, purpose, now=now, online=online, in_flight=in_flight)
        if decision.allowed:
            current = in_flight if in_flight is not None else 0
            if best is None or current < best_load:
                best, best_load = decision, current
        elif fallback is None or _rank(decision.code) > _rank(fallback.code):
            fallback = decision
    return best or fallback or _deny("no_grant", "No server in that group is available to you.")
