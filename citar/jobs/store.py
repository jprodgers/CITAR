"""The job store (design/phase4-helper.md P4.5.4): what a CPU job costs, and the database's jobs as the routes and the
command line read them.

The tables are revision ``0016c_helper_jobs``'s (``citar.db.models``: ``CpuJob``, ``CpuJobRun``, ``JobCost``). The
dispatcher (:mod:`citar.jobs.dispatcher`) is the only writer of the jobs and their runs (P4.7.4); routes and
``citar lab status`` read them.

**The cost table** (:class:`Costs`, the ``job_costs`` rows): per kind of job, map size, seat count and bucket of
``ROUND_BUCKET`` rounds of its turn limit, ``cpu_s`` (the median CPU seconds of the reference machines' runs: the local
executor's and the verifiers'; the **reference cost** a deadline is computed from and a credit is bounded by, P4.5.5,
P4.5.7) and ``peak_mb`` (the 95th percentile of the same machines' counted peaks: the memory a new job is offered with;
a helper's peak is its own word, and a few inflated ones would price a shape out of every machine). It is
seeded from ``citar/jobs/costs.json``, measured on this repository's golden jobs and a set of full games per map size
(``scripts/measure_job_costs.py``), so the first jobs already have an estimate; each finished run then adds its figures
(the median and the percentile over the cell's last ``SAMPLES`` runs, the stored figure counting as one when the
server starts). A shape with no cell of its own is estimated from the nearest one of its family, scaled by rounds
(``ROUNDS_EXPONENT``: late rounds cost more, with more cities and units), seats and map area.

Replaces: the lab runner's ``--max-minutes`` (150, whatever the game), which neither knew a game's cost nor how fast
the machine playing it was.
"""
from __future__ import annotations

import json
import logging
import math
import statistics
import threading
from collections import deque
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

log = logging.getLogger("citar.jobs.store")

#: The cost table's buckets of rounds: a job's turn limit falls in the bucket starting at its multiple of this.
ROUND_BUCKET = 50
#: The committed measurement the table is seeded from.
COSTS_FILE = Path(__file__).with_name("costs.json")
#: How a game's CPU grows with the rounds it plays, when a cell is scaled to another limit: the per-round cost grows as
#: the civilizations do, so a game to round R costs about R ** 2 (the lab's runner saw elapsed time grow as about
#: turn ** 2.5 within one game; a game that ends early by victory stops that growth).
ROUNDS_EXPONENT = 2.0
#: How many runs a cell's median and 95th percentile are taken over.
SAMPLES = 49
#: What a job of no known shape is estimated at.
FALLBACK = {"cpu_s": 60.0, "peak_mb": 64.0}
#: The families a cell may be borrowed across: a ranking game is a lab game.
FAMILY = {"lab_game": "lab", "ranking_game": "lab", "reference_game": "reference"}


def utcnow() -> datetime:
    return datetime.now(timezone.utc)


def bucket(rounds: int) -> int:
    """The first round of the bucket ``rounds`` falls in."""
    return max(0, int(rounds)) // ROUND_BUCKET * ROUND_BUCKET


_RULES: dict = {}


def _rules() -> dict:
    """The ruleset's speeds' last turns and its map sizes (areas and default seats), read once."""
    if not _RULES:
        from citar import engine_api
        rc = engine_api.rules_client()
        _RULES["max_turns"] = dict(rc.get("max_turns") or {})
        _RULES["sizes"] = engine_api.map_sizes()
    return _RULES


def shape(kind: str, spec: dict) -> tuple:
    """A job's cell: (kind, map size, seats, rounds), its rounds the turn limit its spec plays to (the speed's last turn
    when it gives none) and its seats the spec's (a reference game's the size's own)."""
    rules = _rules()
    if kind == "reference_game":
        size = str(spec.get("size") or "")
        seats = int((rules["sizes"].get(size) or {}).get("players") or 0)
        limit = spec.get("turn_limit") or rules["max_turns"].get(spec.get("speed"), 330)
    else:
        size = str(spec.get("size") or "")
        seats = len(spec.get("seats") or [])
        limit = spec.get("turns") or rules["max_turns"].get(spec.get("speed"), 330)
    return kind, size, seats, int(limit)


def _area(size: str) -> float:
    s = _rules()["sizes"].get(size) or {}
    return float((s.get("width") or 60) * (s.get("height") or 38))


def _p95(xs: list) -> float:
    xs = sorted(xs)
    pos = (len(xs) - 1) * 0.95
    lo, hi = math.floor(pos), math.ceil(pos)
    return xs[lo] + (xs[hi] - xs[lo]) * (pos - lo)


class Cell:
    """One shape's figures: what the table holds, and the last runs they come from."""

    __slots__ = ("cpu_s", "peak_mb", "samples", "cpus", "peaks")

    def __init__(self, cpu_s: float, peak_mb: float, samples: int):
        self.cpu_s, self.peak_mb, self.samples = float(cpu_s), float(peak_mb), int(samples)
        self.cpus: deque = deque([self.cpu_s] if self.cpu_s > 0 else [], maxlen=SAMPLES)
        self.peaks: deque = deque([self.peak_mb] if self.peak_mb > 0 else [], maxlen=SAMPLES)


class Costs:
    """The cost table in memory, read from ``job_costs`` (seeded from the committed measurement where it has no row)
    and updated as runs finish; :meth:`write` puts the changed cells back. Thread-safe: the dispatcher records,
    routes estimate."""

    def __init__(self):
        self._lock = threading.Lock()
        self.cells: dict = {}
        self._dirty: set = set()

    @staticmethod
    def committed() -> list:
        """The committed measurement's cells (``costs.json``)."""
        try:
            data = json.loads(COSTS_FILE.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            log.warning("could not read the committed job costs %s", COSTS_FILE, exc_info=True)
            return []
        return [c for c in data.get("cells") or [] if isinstance(c, dict)]

    def load(self, session) -> int:
        """Read the table, adding the committed cells it lacks (in this session: the caller commits). Returns the
        cells added."""
        from sqlalchemy import select
        from citar.db.models import JobCost
        cells = {}
        for row in session.scalars(select(JobCost)):
            cells[(row.kind, row.size, row.seats, row.rounds)] = Cell(row.cpu_s, row.peak_mb, row.samples)
        added = 0
        for c in self.committed():
            try:
                key = (str(c["kind"]), str(c["size"]), int(c["seats"]), bucket(int(c["rounds"])))
                cpu, peak, n = float(c["cpu_s"]), float(c["peak_mb"]), int(c.get("samples") or 1)
            except (KeyError, TypeError, ValueError):
                continue
            if key in cells:
                continue
            cells[key] = Cell(cpu, peak, n)
            session.add(JobCost(kind=key[0], size=key[1], seats=key[2], rounds=key[3], cpu_s=cpu, peak_mb=peak,
                                samples=n, updated_at=utcnow()))
            added += 1
        with self._lock:
            self.cells = cells
            self._dirty.clear()
        return added

    def estimate(self, kind: str, size: str, seats: int, rounds: int) -> dict:
        """What a job of this shape costs: ``{"cpu_s", "peak_mb", "exact"}``, ``exact`` when its own cell has it."""
        key = (kind, size, int(seats), bucket(rounds))
        with self._lock:
            cell = self.cells.get(key)
            if cell is not None and cell.cpu_s > 0:
                return {"cpu_s": cell.cpu_s, "peak_mb": cell.peak_mb, "exact": True}
            family = FAMILY.get(kind)
            near = [(k, c) for k, c in self.cells.items() if FAMILY.get(k[0]) == family and c.cpu_s > 0]
        if not near:
            return {**FALLBACK, "exact": False}
        mid = bucket(rounds) + ROUND_BUCKET / 2

        def distance(item):
            (k_kind, k_size, k_seats, k_rounds), _ = item
            return (k_kind != kind, k_size != size, k_seats != seats, abs(math.log((k_rounds + ROUND_BUCKET / 2) / mid)))
        (k_kind, k_size, k_seats, k_rounds), c = min(near, key=distance)
        r = mid / (k_rounds + ROUND_BUCKET / 2)
        s = (seats or 1) / (k_seats or 1)
        a = _area(size) / _area(k_size)
        return {"cpu_s": c.cpu_s * r ** ROUNDS_EXPONENT * s * a,
                "peak_mb": c.peak_mb * math.sqrt(max(r, 1.0)) * math.sqrt(s) * a, "exact": False}

    def of(self, kind: str, spec: dict) -> dict:
        """:meth:`estimate` for a job's spec."""
        return self.estimate(*shape(kind, spec))

    def record(self, kind: str, spec: dict, *, cpu_s: Optional[float], peak_mb: Optional[float]) -> None:
        """A finished run's figures into its cell, from a reference machine's run only (the local executor, a
        verifier: the dispatcher passes None for the others' figures, which are their own word)."""
        k, size, seats, rounds = shape(kind, spec)
        key = (k, size, seats, bucket(rounds))
        with self._lock:
            cell = self.cells.get(key)
            if cell is None:
                cell = self.cells[key] = Cell(0.0, 0.0, 0)
            if cpu_s is not None and cpu_s > 0:
                cell.cpus.append(float(cpu_s))
                cell.cpu_s = statistics.median(cell.cpus)
            if peak_mb is not None and peak_mb > 0:
                cell.peaks.append(float(peak_mb))
                cell.peak_mb = _p95(list(cell.peaks))
            cell.samples += 1
            self._dirty.add(key)

    def write(self, session) -> int:
        """Put the cells changed since the last write back in ``job_costs`` (in this session). Returns how many."""
        from citar.db.models import JobCost
        with self._lock:
            keys, self._dirty = list(self._dirty), set()
            rows = [(k, self.cells[k].cpu_s, self.cells[k].peak_mb, self.cells[k].samples) for k in keys]
        for (kind, size, seats, rounds), cpu, peak, n in rows:
            row = session.get(JobCost, (kind, size, seats, rounds))
            if row is None:
                row = JobCost(kind=kind, size=size, seats=seats, rounds=rounds)
                session.add(row)
            row.cpu_s, row.peak_mb, row.samples, row.updated_at = cpu, peak, n, utcnow()
        return len(rows)

    def rows(self) -> list:
        """Every cell, for the administrators' view."""
        with self._lock:
            return [{"kind": k[0], "size": k[1], "seats": k[2], "rounds": k[3], "cpu_s": round(c.cpu_s, 3),
                     "peak_mb": round(c.peak_mb, 1), "samples": c.samples} for k, c in sorted(self.cells.items())]


# ---------------------------------------------------------------------------- reading the jobs

#: The job states a game counts as settled in: its result accepted (a crash is a result, P4.4.2).
SETTLED = ("accepted", "crashed")
#: The job states that hold a game: waiting, playing, or settling (its result in, a replica checking it: P4.5.8).
LIVE = ("queued", "running", "settling")


def experiment_jobs(session, name: str) -> list:
    """An experiment's jobs (every state but ``cancelled``), oldest first, with their latest run."""
    from sqlalchemy import select
    from citar.db.models import CpuJob, CpuJobRun
    jobs = list(session.scalars(select(CpuJob).where(CpuJob.item_kind == "lab_experiment", CpuJob.item_ref == name,
                                                     CpuJob.state != "cancelled").order_by(CpuJob.created_at)))
    runs: dict = {}
    if jobs:
        for r in session.scalars(select(CpuJobRun).where(CpuJobRun.job_id.in_([j.id for j in jobs]))
                                 .order_by(CpuJobRun.offered_at)):
            runs.setdefault(r.job_id, []).append(r)
    return [(j, runs.get(j.id, [])) for j in jobs]


def withdrawn_games(session) -> dict:
    """Each experiment's games whose result was withdrawn (P4.5.8 rule 8: its machine caught) and played again, as
    the Lab page counts them: ``{experiment: games}``."""
    from sqlalchemy import func, select
    from citar.db.models import CpuJob
    q = (select(CpuJob.item_ref, func.count(func.distinct(CpuJob.ref)))
         .where(CpuJob.state == "withdrawn", CpuJob.item_kind == "lab_experiment").group_by(CpuJob.item_ref))
    return {name: int(n) for name, n in session.execute(q) if name}


def game_of(ref: str) -> Optional[int]:
    """The game a job's ``ref`` (``lab:<experiment>:<game>``) names."""
    try:
        return int(ref.rsplit(":", 1)[1])
    except (IndexError, ValueError):
        return None


def offline_status() -> Optional[dict]:
    """``citar lab status``'s view of the server's jobs, read from its database when it is here (as
    ``items.register_offline`` opens it): per experiment, its jobs by state, and the runs playing. None without one."""
    from citar import db, items
    from sqlalchemy import func, select
    from citar.db.models import CpuJob, CpuJobRun
    try:
        if items._offline_db() is not None:
            return None
        with db.session() as s:
            counts: dict = {}
            for ref, state, n in s.execute(select(CpuJob.item_ref, CpuJob.state, func.count())
                                           .group_by(CpuJob.item_ref, CpuJob.state)):
                counts.setdefault(ref or "?", {})[state] = n
            running = [{"ref": j.ref, "machine": r.machine_id,
                        "started_at": (r.started_at or r.offered_at).isoformat(timespec="seconds")}
                       for r, j in s.execute(select(CpuJobRun, CpuJob).join(CpuJob, CpuJob.id == CpuJobRun.job_id)
                                             .where(CpuJobRun.state.in_(("offered", "running"))))]
        return {"experiments": counts, "running": running}
    except Exception:
        log.warning("could not read the server's jobs", exc_info=True)
        return None
