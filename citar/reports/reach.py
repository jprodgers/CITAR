"""What a report may gather: everything for an administrator's, only what its owner may see for a member's
(design/phase3-features.md P3.2.4).

Report building was administrator-only, so ``data.gather`` read every activity of the usage ledger, every save,
run and experiment in scope, and priced every machine. Opened to members (3-02), a report takes its owner:

- **activities** (games, benchmark games, probe runs, lab games, report narratives) only when the owner may see
  the thing they belong to: the game's row in the games index, or the run, probe run, experiment or report item
  (``access.visible_filter``: their own, shared with them, members' ones, published ones);
- **machine costs** (configurations, tariffs, depreciation, power, hardware) only on the machines the owner owns:
  an activity that also ran on the server's machines or another member's keeps its outcome and its usage there,
  but not what it cost, and the report says so without naming them. The costs are worked out over the whole
  ledger first, so a minute the owner's machine shared with others' work is split as it is for anyone;
- **wider figures** only as pooled aggregates (``aggregate.scope_for`` with its minimum-contributor threshold),
  in the models section when the report asks for them (its ``pooling``: ``own``, ``pooled`` or ``both``).

An administrator's report, and one with no owner (a script's, a test's), sees everything, as before. Python lines
this replaces: ``data.gather``'s unfiltered ``U.read`` / ``costing.compute`` / ``S.with_pooled`` (``data.py:174-250``
at ``3069d25``).
"""
from __future__ import annotations

import logging
from dataclasses import dataclass, field
from typing import Optional

from .. import costing

log = logging.getLogger("citar.reports")

#: The owned item kinds an activity of the usage ledger can belong to.
ITEM_KINDS = ("benchmark_run", "probe_run", "lab_experiment", "report")


@dataclass
class Reach:
    """What one report may gather. The default reaches everything (an administrator's report, a script's)."""
    everything: bool = True
    owner_id: Optional[str] = None
    #: game ids whose rows the owner may see
    games: set = field(default_factory=set)
    #: refs the owner may see, per owned item kind (``ITEM_KINDS``)
    items: dict = field(default_factory=dict)
    #: ids of the machines the owner owns: the only ones whose costs the report shows
    machines: set = field(default_factory=set)
    #: names of the machines the owner may see (their own, and those shared with them): labels only
    names: dict = field(default_factory=dict)

    # ------------------------------------------------------------------ building one
    @classmethod
    def of(cls, owner_id: Optional[str]) -> "Reach":
        """The reach of a report owned by ``owner_id``: everything for none (a script's report) or for an
        administrator in good standing, and where the process has no database (a command-line tool, which has no
        accounts to keep apart); otherwise what that account may see, read once when the report is built. An
        account that is not in good standing (suspended, deleted, gone, still pending) sees what an anonymous visitor
        would: published things only, and no machine of its own (its role, an administrator's included, counts for
        nothing then)."""
        from .. import db
        if not owner_id or not db.configured():
            return cls()
        from ..db.models import User
        with db.session() as s:
            owner = s.get(User, owner_id)
            standing = owner is not None and owner.active
            # an administrator is one in ``access.on``'s sense (the role, on probation too) only in good standing
            if standing and owner.role == "admin":
                return cls()
            return cls._member(s, owner if standing else None, owner_id)

    @classmethod
    def _member(cls, s, viewer, owner_id: str) -> "Reach":
        """A member's reach (``of``), from the database session ``s``."""
        from sqlalchemy import select
        from .. import items
        from ..auth import access
        from ..db.models import Server
        from ..server import ownership
        games = ownership.visible_ids(s, viewer)
        machines: set = set()
        names: dict = {}
        if viewer is not None:
            for row in s.scalars(select(Server)):
                if row.owner_id == viewer.id:
                    machines.add(row.id)
                    names[row.id] = row.name
                elif access.VIEW in access.on(s, viewer, row):
                    names[row.id] = row.name
        return cls(everything=False, owner_id=owner_id, games=set(games or ()),
                   items={kind: items.visible_refs(s, kind, viewer) for kind in ITEM_KINDS},
                   machines=machines, names=names)

    # ------------------------------------------------------------------ asking it
    def sees(self, info: dict) -> bool:
        """Whether an activity of the usage ledger (its ``act`` row) belongs to something the owner may see."""
        if self.everything:
            return True
        kind = info.get("kind")
        ref = info.get("ref") or {}
        parent = info.get("parent") or {}
        if kind in ("game", "benchmark"):
            if ref.get("game_id") in self.games:
                return True
            # a benchmark game's row follows its run, and the run's allowlist reaches its games too
            return parent.get("kind") == "benchmark_run" and parent.get("id") in self.items.get("benchmark_run", ())
        if kind == "probe":
            return ref.get("probe_run") in self.items.get("probe_run", ())
        if kind == "lab":
            return parent.get("kind") == "lab_experiment" and parent.get("id") in self.items.get("lab_experiment", ())
        if kind == "report":
            return parent.get("id") in self.items.get("report", ())
        return False

    def owns(self, server_id: Optional[str]) -> bool:
        """Whether the report shows a machine's costs."""
        return self.everything or server_id in self.machines

    # ------------------------------------------------------------------ applying it
    def ledger(self, ledger: dict) -> dict:
        """The usage ledger cut to the activities the owner may see (the spans and the activity rows; power samples
        only for their own machines)."""
        if self.everything:
            return ledger
        acts = {k: a for k, a in ledger["acts"].items() if self.sees(a)}
        return {"acts": acts, "spans": [sp for sp in ledger["spans"] if sp.get("act") in acts],
                "power": {srv: rows for srv, rows in (ledger.get("power") or {}).items() if srv in self.machines}}

    def registry(self, reg: dict) -> dict:
        """The registry as the report shows it: only the owner's machines (their configurations, power and costs)."""
        if self.everything:
            return reg
        return {**reg, "servers": [sv for sv in reg["servers"] if sv["id"] in self.machines], "aliases": {}}

    def labels(self, servers_by_id: dict) -> dict:
        """Machine names a report may print (a seat's machine in a configuration's label): the owner's own machines
        with everything the cost sections read, and the others they may see by name only."""
        if self.everything:
            return servers_by_id
        out = {sid: sv for sid, sv in servers_by_id.items() if sid in self.machines}
        for sid, name in self.names.items():
            out.setdefault(sid, {"id": sid, "name": name, "kind": "other", "power": {}, "models": [], "costs": [],
                                 "components": [], "connection": {}})
        return out

    def cost(self, cost: dict) -> dict:
        """``costing.compute``'s result cut to what the report may show: the activities the owner may see, each with
        its costs on the owner's machines only (its totals worked out again from them); the per-machine, per-day
        and per-hour figures of the owner's machines; the notes about those machines (``_notes``), and one that
        says how many activities ran elsewhere too."""
        if self.everything:
            return cost
        acts, elsewhere = {}, 0
        for act_id, a in cost["acts"].items():
            if not self.sees(a["info"]):
                continue
            mine = {srv: c for srv, c in a["servers"].items() if srv in self.machines}
            if len(mine) < len(a["servers"]):
                elsewhere += 1
            total = costing._new_cost()
            for c in mine.values():
                for k in total:
                    total[k] += c[k]
            # ``ran_on`` keeps where it ran for the report's own machine filter (``data._in_scope``), never printed
            acts[act_id] = {**a, "servers": mine, "total": total, "ran_on": sorted(a["servers"])}
        notes = self._notes(cost, acts)
        if elsewhere:
            notes.append(f"Costs are counted on the report owner's own machines only: {elsewhere} of these activities "
                         "also ran on machines that are not theirs (the server's own, or other members'), and what they "
                         "cost there is not part of this report.")
        return {**cost, "acts": acts,
                "servers": {srv: v for srv, v in cost["servers"].items() if srv in self.machines},
                "days": {d: {srv: v for srv, v in day.items() if srv in self.machines} for d, day in cost["days"].items()},
                "hours": {h: {srv: v for srv, v in hr.items() if srv in self.machines}
                          for h, hr in (cost.get("hours") or {}).items()},
                "notes": notes}

    def _notes(self, cost: dict, acts: dict) -> list:
        """The pricing notes a member's report may print: those ``costing.compute`` filed under the owner's own
        machines, and the one about usage with no machine when an activity of theirs had some. Never a note about
        another machine, matched by its name or otherwise: a note names its machine and the model used there."""
        by_server = cost.get("notes_by_server") or {}
        keep = {n for srv in self.machines for n in by_server.get(srv, ())}
        if any("unassigned" in a.get("ran_on", ()) for a in acts.values()):
            keep.update(by_server.get("unassigned", ()))
        return sorted(keep)


def pooled_models(owner_id: Optional[str]) -> Optional[list]:
    """The pooled figures a report may add to its models section (``pooling`` ``pooled`` or ``both``): per model, the
    metrics over other accounts' pooled games (``aggregate.scope_for`` without the owner's own, which the report's
    activities already cover), a model with too few accounts behind it withheld (``aggregate.publishable``). None
    where the process has no database (no accounts, so nothing is pooled)."""
    from types import SimpleNamespace
    from .. import aggregate, db
    if not db.configured():
        return None
    from ..db.models import User
    from ..server import scoring
    from ..server.session import all_sessions
    with db.session() as s:
        owner = s.get(User, owner_id) if owner_id else None
        scope = aggregate.scope_for(s, owner, include_own=False)
    live = SimpleNamespace(sessions={gs.id: gs for gs in all_sessions() if not gs.stopped})
    return scoring.compare_models(live, scope=scope)
