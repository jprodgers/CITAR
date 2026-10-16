"""The CPU jobs' routes (design/phase4-helper.md P4.5.10): an experiment's games as jobs, and the dispatcher for
administrators.

    GET  /api/lab/jobs?exp=NAME     VIEW on the experiment: its games' jobs and their accepted runs
    GET  /api/machines/{id}/jobs    MANAGE on the machine (its owner, administrators): its recent CPU runs, what its
                                    helper lends in effect, and its trust counters
    GET  /api/admin/jobs            administrators: the dispatcher, queued jobs by experiment, runs per executor
    POST /api/admin/jobs/pause      administrators: the Lab page's Stop (``citar lab stop``): no new game starts
    POST /api/admin/jobs/resume     administrators: start new games again (``citar lab resume``)
    POST /api/admin/jobs/retry      administrators: an experiment's failed games (``?exp=NAME``; every queued
                                    experiment's without it) played again, their attempts back to 0

**Who sees what** (P4.5.8 rule 2, P3.9.1): an executor is named where the caller may view its machine (the server's
own job host is "this server", its machine's name and id administrators'), "a member's machine" otherwise, in the
dispatcher's executors and in why a game waits (:func:`told_executor`, :func:`told_waiting`); a job is ``done`` from
the moment its primary's result is committed, whatever verification follows (replicas and the ``settling`` state are
administrators' alone: 4-11), and ``crashed`` when its result is a crash. An experiment the caller may not see is "not
found", as its report is.
"""
from __future__ import annotations

from typing import Optional

from fastapi import APIRouter, Depends, HTTPException, Query, Request
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


#: What a member is told of an executor (P4.5.10: the dispatcher's health; the hosts' details, their machines' ids,
#: their notes and their logs are administrators').
EXECUTOR_TOLD = ("name", "kind", "on", "state", "threads", "running", "paused", "benchmark", "available")
#: What a member is told of a job host that stopped (its death's words quote its log).
HOST_STOPPED = "its job host stopped; it starts again shortly"


def told_executor(e: dict, name) -> dict:
    """An executor of the dispatcher's view as a member is told it: named by ``name`` (``_namer``'s: "this server"
    for the server's own job host), without its machine's id, its host's notes, or why its host stopped."""
    out = {k: e.get(k) for k in EXECUTOR_TOLD}
    out["name"] = name(e.get("machine"), e.get("kind"))["name"]
    if e.get("state") == "failed" and out.get("available"):
        out["available"] = HOST_STOPPED
    return out


def told_waiting(exp: dict, name) -> Optional[str]:
    """Why an experiment's games wait, each executor named by ``name`` as the viewer may be told it (the
    dispatcher's ``waiting_on``); its words as they are when no executor is named in them."""
    parts = exp.get("waiting_on") or []
    if not parts:
        return exp.get("waiting")
    return "; ".join(f"{name(p.get('machine'), p.get('kind'))['name']}: {p.get('why')}" for p in parts)


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


#: The most runs ``GET /api/machines/{id}/jobs`` lists.
MACHINE_RUNS = 200


@router.get("/api/machines/{sid}/jobs")
def machine_jobs(sid: str, request: Request, limit: int = Query(50, ge=1, le=MACHINE_RUNS),
                 p: Principal = Depends(principal), s: Session = Depends(get_db, scope="function")):
    """A machine's CPU runs, for its owner and administrators (MANAGE on its row: anyone a group lends it to gets 403,
    anyone else 404, a visitor 401): its recent runs, newest first (state, kind, the run's role, times and figures; a
    run's verdict only once its job settled, P4.5.8 rule 2), each experiment named where the caller may view it ("a
    member's experiment" otherwise); what its helper lends now (the CPU in effect, each limit hard or soft, the memory
    a run may use, or why it lends nothing); and its trust counters (an administrator also sees the last claim
    failure). Replicas, from 4-11, are administrators' alone."""
    from sqlalchemy import select
    from ..auth.deps import require_user
    from ..db.models import CpuJob, CpuJobRun
    from ..jobs import remote
    from ..server.workers import hub
    me = require_user(request, p)
    row = s.get(Server, sid) if isinstance(sid, str) else None
    if row is None:
        raise HTTPException(404, "No such server.")
    access.on(s, me, row).require(access.MANAGE)
    admin = me.role == "admin"
    q = (select(CpuJobRun, CpuJob).join(CpuJob, CpuJob.id == CpuJobRun.job_id)
         .where(CpuJobRun.machine_id == sid).order_by(CpuJobRun.offered_at.desc()).limit(limit))
    pairs = list(s.execute(q))
    seen: dict = {}

    def experiment(job) -> str:
        ref = job.item_ref or ""
        if ref not in seen:
            _row, perms = items.resolve(s, "lab_experiment", ref, me)
            seen[ref] = ref if (admin or perms.can(access.VIEW)) else "a member's experiment"
        return seen[ref]
    runs = []
    for run, job in pairs:
        if run.role != "primary" and not admin:
            continue
        settled = job.state in ("accepted", "crashed", "withdrawn")
        entry = {"run": run.id, "kind": job.kind, "state": run.state, "experiment": experiment(job),
                 "game": _game(job.ref), "offered": _iso(run.offered_at), "started": _iso(run.started_at),
                 "ended": _iso(run.ended_at), "cpu_s": round(run.cpu_s or 0.0, 2),
                 "wall_s": round(run.wall_s or 0.0, 2), "paused_s": round(run.paused_s or 0.0, 2),
                 "peak_mb": round(run.peak_mb or 0.0, 1)}
        if settled or admin:
            entry["verdict"] = run.verdict
        if admin:
            entry.update({"role": run.role, "job_state": job.state, "error": run.error})
        runs.append(entry)
    trust = dict(row.trust or {})
    counters = {k: trust.get(k, 0) for k in ("verified", "streak", "mismatches", "abandons", "claim_failures",
                                             "helper_faults")}
    if admin:
        counters.update({k: trust.get(k) for k in ("last_mismatch", "last_claim_failure", "last_helper_fault", "since",
                                                    "verifier", "speed")})
    conn = hub().get(sid)
    executor = remote.registry().find(sid)
    lends = None
    if conn is not None:
        eff = conn.effective() or {}
        lends = {"cpu": eff.get("cpu"), "limits": (conn.ack or {}).get("limits"),
                 "memory_per_run_mb": conn.memory_per_run_mb,
                 "lent": bool((conn.hello.get("cpu") or {}).get("enabled")),
                 "verifiable": bool((conn.hello.get("cpu") or {}).get("verifiable")),
                 "jobs_build": conn.jobs_build,
                 "state": executor.state if executor is not None else "off",
                 "why": (executor.reason if executor is not None
                         else "its helper plays no CPU job (a build without jobs)") or None}
    return {"machine": {"id": row.id, "name": row.name, "trust_tier": row.trust_tier or "new"},
            "online": conn is not None, "lends": lends, "trust": counters, "runs": runs}


def _game(ref: str) -> Optional[int]:
    from ..jobs import store
    return store.game_of(ref or "")


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


@router.post("/api/admin/jobs/retry")
def admin_jobs_retry(exp: Optional[str] = Query(None, min_length=1, max_length=120),
                     me: User = Depends(require_admin_hidden), s: Session = Depends(get_db, scope="function")):
    """Play an experiment's failed games again (every queued experiment's without ``exp``), their attempts back to 0:
    a game fails for good after three retryable failures (a host's death, a deadline a sleeping machine passed), and
    otherwise only a restart of the server tries it again. The dispatcher does it at its next tick, on its thread."""
    from ..jobs import dispatcher as jobs_dispatcher
    if exp is not None and not (lab.QUEUE / f"{exp}.json").is_file():
        raise HTTPException(404, "No queued experiment of that name.")
    d = jobs_dispatcher.current()
    if d is None:
        raise HTTPException(409, "The lab's dispatcher is not running in this server.")
    d.retry(exp)
    audit.record(s, "lab.retried", actor=me, exp=exp)
    return {"requested": True, "exp": exp}

