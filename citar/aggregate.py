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
    never silently a mix of the two. `owner_of` names each game's owner, so a figure can be
    withheld when too few accounts are behind it (`publishable`).
    """
    viewer: Optional[User]
    include_own: bool = True
    include_pooled: bool = True
    game_ids: set = field(default_factory=set)
    own_ids: set = field(default_factory=set)
    pooled_ids: set = field(default_factory=set)
    contributors: set = field(default_factory=set)
    owner_of: dict = field(default_factory=dict)

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


def _games(session, kinds: Optional[Iterable] = None):
    """Every live game row as (id, owner id, whether it pools), in one query: a game pools when its row says
    `pool` and its owner is an account that has not opted out (``users.data_sharing``) or left. Opting out
    is the account's word over all of its games, the ones it made before as much as the ones to come, so it
    is read here, when an average is made, and never copied onto the rows."""
    stmt = (select(Game.id, Game.owner_id, Game.data_sharing, User.data_sharing, User.status)
            .join(User, User.id == Game.owner_id, isouter=True)
            .where(Game.deleted_at.is_(None)))
    if kinds:
        stmt = stmt.where(Game.kind.in_(list(kinds)))
    for gid, owner_id, game_sharing, owner_sharing, owner_status in session.execute(stmt):
        pools = bool(owner_id) and game_sharing == "pool" and owner_sharing == "pool" and owner_status != "deleted"
        yield gid, owner_id, pools


def scope_for(session, viewer: Optional[User], *, include_own: bool = True,
              include_pooled: bool = True, kinds: Optional[Iterable] = None) -> Scope:
    """Work out which games this viewer's analysis may read: the viewer's own (pooled or not), and everybody
    else's that pool (``_games``). Rows only: a save with no row in the games index is nobody's, and pools
    nothing."""
    scope = Scope(viewer=viewer, include_own=include_own, include_pooled=include_pooled)
    for gid, owner_id, pools in _games(session, kinds):
        if viewer is not None and owner_id == viewer.id:
            # Your own games are always readable by you, pooled or not. Private mode keeps your
            # results out of everybody else's averages; it does not hide them from you.
            if include_own:
                scope.own_ids.add(gid)
                scope.game_ids.add(gid)
                scope.owner_of[gid] = owner_id
            continue
        if include_pooled and pools:
            scope.pooled_ids.add(gid)
            scope.game_ids.add(gid)
            scope.contributors.add(owner_id)
            scope.owner_of[gid] = owner_id
    return scope


def pooled_game_ids(session, *, kinds: Optional[Iterable] = None) -> set:
    """Every game contributing to the public averages, regardless of viewer."""
    return {gid for gid, _owner, pools in _games(session, kinds) if pools}


def publishable(scope: Scope, games_by_label: dict, *, threshold: Optional[int] = None) -> tuple:
    """Which of each label's games a figure for this viewer may be made from (``scope``'s games, grouped by a
    label such as a model): the viewer's own always, and everybody else's only when at least ``threshold``
    (``min_contributors``) other accounts are behind them. Counting the others alone matters: a figure
    made of the viewer's games and one other account's is that account's games with the viewer's
    subtracted out.

    Returns ``({label: games}, {label: contributors})``: the second names the labels whose pooled part was
    withheld, with how many accounts it had, for the page to say "not enough data yet" rather than imply
    there is none."""
    limit = threshold if threshold is not None else min_contributors()
    allowed, withheld = {}, {}
    for label, gids in games_by_label.items():
        gids = set(gids) & scope.game_ids
        own = gids & scope.own_ids
        others = gids - own
        owners = {scope.owner_of.get(g) for g in others} - {None}
        if others and len(owners) < limit:
            withheld[label] = len(owners)
            others = set()
        allowed[label] = own | others
    return allowed, withheld


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
    for _gid, owner_id, pools in _games(session):
        total += 1
        if pools:
            pooled += 1
            contributors.add(owner_id)
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
