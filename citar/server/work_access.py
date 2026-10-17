"""What a viewer may be told about other work (design/phase3-features.md P3.2.4, P3.3.8, P3.9.1).

The Queue page, the benchmark runs, the probe runs and the reports name the work that holds a model machine or goes
first on it: somebody's game, run or report, which the viewer may not be allowed to see. ``access_to`` is what a viewer
may do with one piece of queued work; ``namer`` names a holder to a viewer who may see its work and calls it "someone's
<kind>" otherwise; ``with_waiting`` tells a job's, a run's or a report's waiting note (``pool.seats.note``) to one
viewer. The runners store their notes' text naming no holder, so a reader that is not given the viewer's rendering
(a log, a file) is told nothing either. On the Queue page a machine's leases and pooled prompts are told the same way
(``binding_rights``, ``told_lease``, ``told_ticket``; package 3-09).
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


#: How a pooled prompt the viewer may not see is named on the Queue page, by its class (P3.3.8: "someone's <class>").
SOMEONES_PROMPT = {"chat": "someone's chat", "diplomat": "someone's diplomat phase", "probe": "someone's probe",
                   "report": "someone's report", "test": "someone's test"}


def binding_rights(sdb, user, entry: dict, server_row, *, admin: bool, cache: dict) -> tuple:
    """What ``user`` may do with one lease or ticket on a machine (an entry of ``scheduler.Machine.view``), as
    ``(view, manage, cancel)`` (P3.3.8): ``view``, see its work's name and its member (their own prompt, or work they
    may see: ``access_to`` on its ``ref``); ``manage``, change its work's queue priority (MANAGE on the work, which a
    prompt of no work has none of); ``cancel``, cancel a ticket: the work's owner (MANAGE on it, or the prompt's own
    member for a prompt of no work), the machine's owner (MANAGE on its row) and administrators. ``cache`` keeps the
    permissions read across one response's entries."""
    if admin:
        return True, True, True
    ref = entry.get("ref") or [None, None]
    key = (ref[0], ref[1])
    if key not in cache:
        cache[key] = access_to(sdb, user, *key)
    perms = cache[key]
    own = user is not None and entry.get("consumer") is not None and entry.get("consumer") == user.id
    machine_key = ("server", getattr(server_row, "id", None))
    if machine_key not in cache:
        cache[machine_key] = access.on(sdb, user, server_row) if server_row is not None else access.Access(set())
    view = own or access.VIEW in perms
    manage = ref[0] is not None and access.MANAGE in perms
    cancel = manage or (own and ref[0] is None) or access.MANAGE in cache[machine_key]
    return view, manage, cancel


def told_lease(entry: dict, rights: tuple, member: Optional[str]) -> dict:
    """A machine's lease as one viewer is told it (``binding_rights``): the work's label and its member when they may
    see the work, else "someone's <kind>"; whether its seat is mid-turn (``active``), lending its slot between its
    turns (``lending``) or holding it lending nothing (``held``); a request of it waiting."""
    view = rights[0]
    kind = (entry.get("ref") or [None])[0]
    return {"kind": entry["kind"], "label": entry["label"] if view else pool_seats.SOMEONES.get(kind, "someone's work"),
            "member": member if view else None, "model": entry.get("model"),
            "state": "active" if entry["active"] else ("lending" if entry["lending"] else "held"),
            "in_flight": entry["in_flight"], "waiting": entry["waiting"]}


def told_ticket(entry: dict, rights: tuple, member: Optional[str]) -> dict:
    """A machine's running or waiting ticket as one viewer is told it (``binding_rights``): its class, the work's label
    and its member when they may see the work, else "someone's <class>"; its model, its work's queue priority, its
    place in the machine's line, how long it has waited and how long it may still wait to start, its ETA; the work
    (``ref``) for the priority buttons when they may change it, and its id when they may cancel it."""
    view, manage, cancel = rights
    out = {"class": entry["class"],
           "label": entry["label"] if view else SOMEONES_PROMPT.get(entry["class"], "someone's prompt"),
           "member": member if view else None, "model": entry.get("model"), "priority": entry.get("priority", 0),
           "state": entry.get("state"), "ref": entry.get("ref") if manage else None, "can_manage": bool(manage),
           "can_cancel": bool(cancel), "id": entry["id"] if cancel else None}
    for k in ("position", "eta_s", "waited_s", "starts_within_s", "running_s", "borrowing", "draining"):
        if k in entry:
            out[k] = entry[k]
    return out


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
