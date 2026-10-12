"""What a viewer may be told about other work (design/phase3-features.md P3.2.4, P3.3.8, P3.9.1).

The Queue page, the benchmark runs, the probe runs and the reports name the work that holds a model machine or goes
first on it: somebody's game, run or report, which the viewer may not be allowed to see. ``access_to`` is what a viewer
may do with one piece of queued work; ``namer`` names a holder to a viewer who may see its work and calls it "someone's
<kind>" otherwise; ``with_waiting`` tells a job's, a run's or a report's waiting note (``pool.seats.note``) to one
viewer. The runners store their notes' text naming no holder, so a reader that is not given the viewer's rendering
(a log, a file) is told nothing either.
"""
from __future__ import annotations

from typing import Callable, Optional

from .. import items
from ..auth import access
from ..pool import seats as pool_seats
from . import ownership

#: The shared queue's kinds and the item each one's id (its run, or the report) names; a game is its row in the games
#: index.
QUEUE_ITEMS = {"benchmark": "benchmark_run", "probe": "probe_run", "report": "report", "lab": "lab_experiment"}
#: How work the viewer may not see is named, by the shared queue's kind (``pool.seats.SOMEONES``).
SOMEONES = pool_seats.SOMEONES
#: The work holding a machine, each holder once (``pool.seats.holders``).
holders = pool_seats.holders


def access_to(sdb, user, kind: Optional[str], ident: Optional[str]) -> access.Access:
    """What ``user`` may do with one piece of queued work: its item's permissions, a game's through its row; nothing
    for work with no kind or id (a lease that names none)."""
    if not kind or not ident:
        return access.Access(set())
    if kind == "game":
        row = ownership.row_for(sdb, ident)
        return access.on(sdb, user, row) if row is not None else access.Access(set())
    item = QUEUE_ITEMS.get(kind)
    if item is None:
        return access.Access(set())
    return items.resolve(sdb, item, ident, user)[1]


def namer(sdb, user, *, admin: bool = False, cache: Optional[dict] = None) -> Callable[[dict], str]:
    """How ``user`` is told of a holder (``pool.seats.holders``, ``queue_holder``): by its label when they may see its
    work (an administrator sees all), else as "someone's <kind>". ``cache`` keeps the permissions read, by (kind, id),
    across the entries of one response."""
    cache = {} if cache is None else cache

    def name(holder: dict) -> str:
        label = holder.get("label")
        if not label:
            return pool_seats.anonymous(holder)
        if admin:
            return label
        key = (holder.get("kind"), holder.get("id"))
        if key not in cache:
            cache[key] = access_to(sdb, user, *key)
        return label if access.VIEW in cache[key] else pool_seats.anonymous(holder)
    return name


def with_waiting(entry: dict, name: Callable[[dict], str]) -> dict:
    """``entry`` (a benchmark job, a probe run, a report's meta, a queue item) with its waiting note told as ``name``
    tells it, without the note's structure; a report's "Waiting: ..." progress, written with the note, is told the same
    way. An entry without a note is returned as it is."""
    if "waiting_note" not in entry:
        return entry
    out = {k: v for k, v in entry.items() if k != "waiting_note"}
    note = entry.get("waiting_note")
    if note:
        told = pool_seats.note_text(note, name)
        stored = entry.get("waiting")
        out["waiting"] = told
        if stored and out.get("progress") == f"Waiting: {stored}":
            out["progress"] = f"Waiting: {told}"
    return out
