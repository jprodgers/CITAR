"""Pooled results: the server-wide model averages, and who contributes to them.

From the brief: "Users can (by default) allow their collected data to feed into the server
averaging, or they can lock their data and do private analysis."

Three rules make that safe rather than merely implemented:

*Filtering happens at query time, not at write time.* A game records its results the same way
whichever mode its owner is in; whether it counts toward a public average is decided when the
average is computed. So flipping to private retroactively withdraws a person's history from every
aggregate, and flipping back retroactively restores it — which is what somebody changing their mind
reasonably expects, and the opposite of what a write-time filter would give them.

*A public average never reveals one person's numbers.* An aggregate over a single contributor is
that contributor's data with a label on it. Every published figure carries a minimum-contributor
threshold, and a bucket below it is withheld rather than shown — including from the person asking,
because "show it to me if I'm the only one in it" is how the threshold gets defeated.

*Your own data is always yours.* Private mode excludes you from the pool; it does not hide your
results from you. A private analysis reads everything the account owns plus whatever pooled data
exists, and says which is which.
"""
from __future__ import annotations

import logging
from dataclasses import dataclass, field
from typing import Iterable, Optional

from sqlalchemy import select

from .db.models import Game, User

log = logging.getLogger("citar.aggregate")

#: Distinct accounts that must contribute before a pooled figure is published. Below this, the
#: "average" is one person's results wearing a hat.
DEFAULT_MIN_CONTRIBUTORS = 3


def min_contributors() -> int:
    """How many distinct contributors a pooled average needs before it is shown.

    Below the threshold an average is one person's data with a label on it, which is not what anybody
    opted into when they agreed to pooling.
    """
    from .auth import policy
    try:
        return max(1, int(policy.get("aggregate_min_contributors") or DEFAULT_MIN_CONTRIBUTORS))
    except Exception:
        return DEFAULT_MIN_CONTRIBUTORS


# ---------------------------------------------------------------------------- scope

@dataclass
class Scope:
    """Which games an analysis may read, and why.

    `own` is everything the viewer owns, whatever its sharing setting. `pooled` is everybody
    else's games that are marked as contributing. A report says which it used, so a number is
    never silently a mix of the two.
    """
    viewer: Optional[User]
    include_own: bool = True
    include_pooled: bool = True
    game_ids: set = field(default_factory=set)
    own_ids: set = field(default_factory=set)
    pooled_ids: set = field(default_factory=set)
    contributors: set = field(default_factory=set)

    @property
    def enough_contributors(self) -> bool:
        """Whether this scope has enough contributors to be shown."""
        return len(self.contributors) >= min_contributors()

    def describe(self) -> str:
        """What this scope covers, as a sentence."""
        parts = []
        if self.include_own and self.own_ids:
            parts.append(f"{len(self.own_ids)} of your games")
        if self.include_pooled and self.pooled_ids:
            parts.append(f"{len(self.pooled_ids)} pooled from "
                         f"{len(self.contributors)} contributor(s)")
        return " plus ".join(parts) if parts else "no games"

    def client(self) -> dict:
        """The scope as the client shows it."""
        return {"own": len(self.own_ids), "pooled": len(self.pooled_ids),
                "contributors": len(self.contributors), "total": len(self.game_ids),
                "enough_contributors": self.enough_contributors,
                "min_contributors": min_contributors(),
                "description": self.describe()}


def scope_for(session, viewer: Optional[User], *, include_own: bool = True,
              include_pooled: bool = True, kinds: Optional[Iterable] = None) -> Scope:
    """Work out which games this viewer's analysis may read."""
    scope = Scope(viewer=viewer, include_own=include_own, include_pooled=include_pooled)
    stmt = select(Game).where(Game.deleted_at.is_(None))
    if kinds:
        stmt = stmt.where(Game.kind.in_(list(kinds)))

    for game in session.scalars(stmt):
        mine = bool(viewer and game.owner_id == viewer.id)
        if mine:
            # Your own games are always readable by you, pooled or not. Private mode keeps your
            # results out of everybody else's averages; it does not hide them from you.
            if include_own:
                scope.own_ids.add(game.id)
                scope.game_ids.add(game.id)
            continue
        if include_pooled and game.data_sharing == "pool" and game.owner_id:
            scope.pooled_ids.add(game.id)
            scope.game_ids.add(game.id)
            scope.contributors.add(game.owner_id)
    return scope


def pooled_game_ids(session, *, kinds: Optional[Iterable] = None) -> set:
    """Every game contributing to the public averages, regardless of viewer."""
    stmt = select(Game).where(Game.deleted_at.is_(None), Game.data_sharing == "pool",
                              Game.owner_id.is_not(None))
    if kinds:
        stmt = stmt.where(Game.kind.in_(list(kinds)))
    return {g.id for g in session.scalars(stmt)}


# ---------------------------------------------------------------------------- withholding

def withhold(buckets: dict, *, contributor_of, threshold: Optional[int] = None) -> dict:
    """Drop any bucket that too few distinct accounts contributed to.

    `buckets` maps a label (a model, say) to a list of records; `contributor_of` returns the owning
    account for a record. A bucket below the threshold is replaced by a marker rather than removed
    entirely, so the UI can say "not enough data yet" instead of silently implying none exists.
    """
    limit = threshold if threshold is not None else min_contributors()
    out = {}
    for label, records in buckets.items():
        owners = {contributor_of(r) for r in records}
        owners.discard(None)
        if len(owners) >= limit:
            out[label] = {"published": True, "records": records, "contributors": len(owners)}
        else:
            out[label] = {"published": False, "contributors": len(owners),
                          "reason": f"Needs results from at least {limit} accounts "
                                    f"before it can be shown as an average."}
    return out


def summarize_sharing(session) -> dict:
    """How much of the server is pooled — for the admin console and the about page."""
    total = pooled = private = 0
    contributors = set()
    for game in session.scalars(select(Game).where(Game.deleted_at.is_(None))):
        total += 1
        if game.data_sharing == "pool" and game.owner_id:
            pooled += 1
            contributors.add(game.owner_id)
        else:
            private += 1
    accounts = list(session.scalars(select(User).where(User.status.in_(("active", "probation")))))
    return {
        "games_total": total,
        "games_pooled": pooled,
        "games_private": private,
        "contributors": len(contributors),
        "min_contributors": min_contributors(),
        "accounts_pooling": sum(1 for u in accounts if u.data_sharing == "pool"),
        "accounts_private": sum(1 for u in accounts if u.data_sharing == "private"),
    }
