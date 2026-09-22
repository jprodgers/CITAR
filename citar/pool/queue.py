"""One queue for work that waits for a model machine: benchmark jobs, probe runs and reports whose analysis
is written by a model, in priority order.

The benchmark scheduler and the probe runner each keep their own list of waiting work, and each decides
when to start its own. What they share is this: every waiting item has a *rank* - its priority, then how
long it has waited - and an item may only start on a machine when nothing from the other producer ranks
ahead of it for that same machine. Within one producer the existing order holds (a benchmark run's jobs
in turn; probe runs by rank).

Running work is in the same order. When something waiting has a *higher* priority than what is using a
machine, the running work yields at its next safe point - a benchmark or lobby game after the model's turn,
a probe run between cases - and goes back in the queue at its own rank; it carries on when nothing above it
is waiting (see ``preempting`` and ``first_ahead``). Equal priorities never interrupt each other: they wait
their turn, oldest first.

Priority belongs to a benchmark run or a probe run as a whole: higher goes first, 0 is normal, and
the value is kept in ``saves/queue-priorities.json`` so it survives restarts. The lab's experiments are
a separate queue - they run on this server's CPU, not on a model machine - but the queue page edits
their priority the same way, in the experiment's own file.
"""
from __future__ import annotations

import json
import threading
from typing import Callable, Optional

from .. import paths

KINDS = ("benchmark", "probe", "report", "game")
_providers: dict[str, Callable[[], list]] = {}
_running: dict[str, Callable[[], list]] = {}
_lock = threading.Lock()
_cache: Optional[dict] = None


def _file():
    """Where priorities are kept."""
    return paths.saves_path("queue-priorities.json")


def _load() -> dict:
    """Every priority that has been set, by "kind:id"."""
    global _cache
    if _cache is None:
        try:
            _cache = json.loads(_file().read_text(encoding="utf-8"))
        except (OSError, ValueError):
            _cache = {}
    return _cache


def priority(kind: str, group: str) -> int:
    """A benchmark run's or probe run's priority (0 unless someone changed it)."""
    with _lock:
        return int(_load().get(f"{kind}:{group}", 0))


def set_priority(kind: str, group: str, value: int) -> int:
    """Change a run's priority. Higher goes first; the range is -100..100."""
    value = max(-100, min(100, int(value)))
    with _lock:
        data = _load()
        if value:
            data[f"{kind}:{group}"] = value
        else:
            data.pop(f"{kind}:{group}", None)
        path = _file()
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp = path.with_suffix(".tmp")
        tmp.write_text(json.dumps(data, indent=1), encoding="utf-8")
        tmp.replace(path)
    return value


def register(kind: str, waiting: Callable[[], list], running: Optional[Callable[[], list]] = None) -> None:
    """Tell the queue how to list a producer's waiting items (and, for the queue page, its running ones).

    Each item is a dict with ``kind``, ``id`` (the item), ``group`` (the run it belongs to), ``label``,
    ``server_id`` and ``created``; the queue adds ``priority``.
    """
    _providers[kind] = waiting
    if running is not None:
        _running[kind] = running


def rank(item: dict) -> tuple:
    """Sort key: higher priority first, then whatever has waited longest."""
    return (-int(item.get("priority", 0)), float(item.get("created") or 0))


def waiting(server_id: Optional[str] = None, skip: Optional[str] = None) -> list[dict]:
    """Everything waiting (for one machine, or all), best first. ``skip`` leaves one producer out."""
    out = []
    for kind, fn in list(_providers.items()):
        if kind == skip:
            continue
        try:
            items = fn() or []
        except Exception:
            continue
        for it in items:
            if server_id is not None and it.get("server_id") != server_id:
                continue
            it = dict(it)
            it["priority"] = priority(it["kind"], it["group"])
            out.append(it)
    out.sort(key=rank)
    return out


def running(server_id: Optional[str] = None) -> list[dict]:
    """What is using a machine (or every machine) now, best first, with priorities."""
    out = []
    for kind, fn in list(_running.items()):
        try:
            items = fn() or []
        except Exception:
            continue
        for it in items:
            if server_id is not None and it.get("server_id") != server_id:
                continue
            it = dict(it)
            it["priority"] = priority(it["kind"], it["group"])
            out.append(it)
    out.sort(key=rank)
    return out


def preempting(server_id: Optional[str], kind: str, item_id: str, item_priority: int) -> Optional[dict]:
    """Waiting work with a higher priority than a running item, which that item should now make way for."""
    if not server_id:
        return None
    for it in waiting(server_id):
        if it["kind"] == kind and it["id"] == item_id:
            continue
        return it if it["priority"] > item_priority else None
    return None


def first_ahead(server_id: Optional[str], item_rank: tuple, exclude: tuple = ()) -> Optional[dict]:
    """The best waiting item (any producer) that ranks ahead of this one on a machine, if any."""
    if not server_id:
        return None
    for it in waiting(server_id):
        if (it["kind"], it["id"]) == tuple(exclude):
            continue
        return it if rank(it) < item_rank else None
    return None


def ahead_of(server_id: str, kind: str, item_rank: tuple) -> Optional[dict]:
    """The first item from *another* producer that should start on this machine before one of this rank."""
    for it in waiting(server_id, skip=kind):
        if rank(it) < item_rank:
            return it
    return None
