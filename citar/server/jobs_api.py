"""The CPU jobs' routes (design/phase4-helper.md P4.5.10): an experiment's games as jobs, and the dispatcher for
administrators.

    GET  /api/lab/jobs?exp=NAME     VIEW on the experiment: its games' jobs and their accepted runs
    GET  /api/admin/jobs            administrators: the dispatcher, queued jobs by experiment, runs per executor
    POST /api/admin/jobs/pause      administrators: the Lab page's Stop (``citar lab stop``): no new game starts
    POST /api/admin/jobs/resume     administrators: start new games again (``citar lab resume``)

**Who sees what** (P4.5.8 rule 2, P3.9.1): an executor is named where the caller may view its machine (the server's
own job host is "this server"), "a member's machine" otherwise; a job is ``done`` from the moment its primary's result
is committed, whatever verification follows (replicas and the ``settling`` state are administrators' alone: 4-11), and
``crashed`` when its result is a crash. An experiment the caller may not see is "not found", as its report is.
"""
from __future__ import annotations

from typing import Optional

from fastapi import APIRouter, Depends, Query, Request
from sqlalchemy.orm import Session

from .. import items, lab
from ..auth import access, audit
from ..auth.deps import Principal, get_db, principal, require_role
from ..db.models import Server, User

router = APIRouter()
require_admin_hidden = require_role("admin", hide=True)

#: A job's state as an experiment's viewers see it (P4.5.8 rule 2): its result committed is ``done``, whatever
#: verification follows; administrators see the states themselves.
SHOWN = {"queued": "queued", "running": "running", "settling": "done", "accepted": "done", "crashed": "crashed",
         "failed": "failed", "withdrawn": "queued", "cancelled": "cancelled"}


def _share_key(request: Request) -> Optional[str]:
    return request.query_params.get("k") or request.query_params.get("key") or request.headers.get("x-citar-share-key")


def _namer(s: Session, viewer: Optional[User]):
    """A machine's name as ``viewer`` may be told it: the server's own job host is "this server"; a helper machine
    its name where the viewer may see it, else "a member's machine"."""
    from .. import pool
    admin = viewer is not None and viewer.role == "admin"
    cache: dict = {}

    def name(machine: Optional[str], executor: Optional[str]) -> dict:
        if not machine:
            return {"name": None, "kind": executor}
        if machine not in cache:
            row = s.get(Server, machine)
            if executor == "local":
                label = f"this server ({row.name})" if admin and row is not None else "this server"
            elif row is not None and (admin or pool.visible(s, viewer, row)):
                label = row.name
            else:
                label = "a member's machine"
            cache[machine] = label
        return {"name": cache[machine], "kind": executor}
    return name


@router.get("/api/lab/jobs")
def lab_jobs(request: Request, exp: str = Query(..., min_length=1, max_length=120), p: Principal = Depends(principal),
             s: Session = Depends(get_db, scope="function")):
    """An experiment's games as jobs, for whoever may see the experiment: each game's job (its state as P4.5.8 rule 2
    shows it, attempts, memory estimate), its playing run's round, and its accepted run (executor, verification,
    times); games with no job yet are left out (they wait in the queue's files)."""
    from ..jobs import dispatcher as jobs_dispatcher, store
    items.require(s, "lab_experiment", exp, p.user, access.VIEW, slug=_share_key(request))
    admin = p.user is not None and p.user.role == "admin"
    live = {r["job"]: r for r in jobs_dispatcher.view().get("running") or []}
    name = _namer(s, p.user)
    out = []
    counts: dict = {}
    for job, runs in store.experiment_jobs(s, exp):
        state = job.state if admin else SHOWN.get(job.state, job.state)
        counts[state] = counts.get(state, 0) + 1
        accepted = next((r for r in runs if r.id == job.accepted_run), None)
        last = runs[-1] if runs else None
        entry = {"i": store.game_of(job.ref), "job": job.id, "kind": job.kind, "state": state,
                 "attempts": job.attempts, "mem_mb": job.mem_mb, "priority": job.priority}
        if accepted is not None:
            entry.update({"executor": name(accepted.machine_id, accepted.executor),
                          "verification": "reference" if accepted.executor == "local" else "unverified",
                          "started": _iso(accepted.started_at), "ended": _iso(accepted.ended_at),
                          "cpu_s": round(accepted.cpu_s or 0.0, 2), "wall_s": round(accepted.wall_s or 0.0, 2),
                          "peak_mb": round(accepted.peak_mb or 0.0, 1)})
        elif job.state == "running" and job.id in live:
            r = live[job.id]
            entry.update({"executor": name(r["machine"], "local" if last is not None and last.executor == "local"
                                           else "remote"),
                          "round": r.get("turn"), "of": r.get("limit"), "paused": r.get("paused"),
                          "minutes": round(r.get("minutes") or 0.0, 2)})
        if job.state == "failed" and last is not None and last.error:
            entry["error"] = (last.error or {}).get("message")
        if admin:
            entry["runs"] = [{"run": r.id, "machine": r.machine_id, "executor": r.executor, "role": r.role,
                              "state": r.state, "verdict": r.verdict, "error": r.error,
                              "offered": _iso(r.offered_at), "ended": _iso(r.ended_at)} for r in runs]
        out.append(entry)
    out.sort(key=lambda e: (e["i"] is None, e["i"]))
    return {"experiment": exp, "jobs": out, "counts": counts}


def _iso(t) -> Optional[str]:
    return t.isoformat(timespec="seconds") if t is not None else None


@router.get("/api/admin/jobs")
def admin_jobs(me: User = Depends(require_admin_hidden), s: Session = Depends(get_db, scope="function")):
    """The dispatcher as administrators see it (P4.5.10): its health and executors (each with why it takes nothing
    now), whether the lab is paused, the queued jobs by experiment in the order they would run (each with why it
    waits: a job whose memory no executor has stays queued, saying so), the runs per executor, and the cost table."""
    from ..jobs import dispatcher as jobs_dispatcher
    d = jobs_dispatcher.current()
    view = jobs_dispatcher.view()
    queued = d.queued_view() if d is not None else []
    by_exp: dict = {}
    for q in queued:
        by_exp.setdefault(q["exp"], []).append(q)
    running: dict = {}
    for r in view.get("running") or []:
        running.setdefault(r["machine"], []).append(r)
    disp = view.get("dispatcher") or {}
    return {"dispatcher": disp, "paused": lab.paused(), "queued": by_exp, "running": running,
            "verifiers": [{"machine": e.get("machine"), "name": e.get("name"), "verifiable": e.get("verifiable")}
                          for e in disp.get("executors") or [] if e.get("kind") == "local"],
            "experiments": view.get("experiments") or {},
            "costs": d.costs.rows() if d is not None else []}


@router.post("/api/admin/jobs/pause")
def admin_jobs_pause(me: User = Depends(require_admin_hidden), s: Session = Depends(get_db, scope="function")):
    """Pause the lab (the Lab page's Stop, ``citar lab stop``): the games playing finish, and no new one starts until
    it is resumed."""
    from ..jobs import dispatcher as jobs_dispatcher
    data = lab.pause(by=me.handle)
    audit.record(s, "lab.paused", actor=me)
    jobs_dispatcher.poke()
    return {"paused": data}


@router.post("/api/admin/jobs/resume")
def admin_jobs_resume(me: User = Depends(require_admin_hidden), s: Session = Depends(get_db, scope="function")):
    """End the pause: new games start again."""
    from ..jobs import dispatcher as jobs_dispatcher
    was = lab.resume()
    if was:
        audit.record(s, "lab.resumed", actor=me)
    jobs_dispatcher.poke()
    return {"paused": None, "was_paused": was}

