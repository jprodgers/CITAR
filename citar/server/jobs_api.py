"""The CPU jobs' routes (design/phase4-helper.md P4.5.10): an experiment's games as jobs, and the dispatcher for
administrators.

    GET  /api/lab/jobs?exp=NAME     VIEW on the experiment: its games' jobs and their accepted runs
    GET  /api/machines/{id}/jobs    MANAGE on the machine (its owner, administrators): its recent CPU runs, what its
                                    helper lends in effect, and its trust counters
    GET  /api/admin/jobs            administrators: the dispatcher, queued jobs by experiment, runs per executor,
                                    the verifiers and the caps on new machines
    GET  /api/admin/verification    administrators: the verification settings, quarantined machines and owners with
                                    their evidence, recent mismatches and withdrawals, verifiers, flagged machines
    PUT  /api/admin/verification    administrators: change the verification settings (bounded)
    POST /api/admin/jobs/pause      administrators: the Lab page's Stop (``citar lab stop``): no new game starts
    POST /api/admin/jobs/resume     administrators: start new games again (``citar lab resume``)
    POST /api/admin/jobs/retry      administrators: an experiment's failed games (``?exp=NAME``; every queued
                                    experiment's without it) played again, their attempts back to 0

**Who sees what** (P4.5.8 rule 2, P3.9.1): an executor is named where the caller may view its machine (the server's
own job host is "this server", its machine's name and id administrators'), "a member's machine" otherwise, in the
dispatcher's executors and in why a game waits (:func:`told_executor`, :func:`told_waiting`); a job is ``done`` from
the moment its primary's result is committed, whatever verification follows (replicas, tiebreaks and the ``settling``
state are administrators' alone), and ``crashed`` when its result is a crash; once it settles, its accepted run's
verification (``reference``, ``verified``, ``unverified``) shows. A game whose result was withdrawn shows as the job
that plays it again (P4.5.8 rule 8). An experiment the caller may not see is "not found", as its report is.
"""
from __future__ import annotations

from typing import Optional

from fastapi import APIRouter, Body, Depends, HTTPException, Query, Request
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
EXECUTOR_TOLD = ("name", "kind", "on", "state", "threads", "running", "paused", "benchmark", "available", "online")
#: What a member is told of a job host that stopped (its death's words quote its log).
HOST_STOPPED = "its job host stopped; it starts again shortly"


def told_executor(e: dict, name) -> dict:
    """An executor of the dispatcher's view as a member is told it: named by ``name`` (``_namer``'s: "this server"
    for the server's own job host), without its machine's id, its host's notes, or why its host stopped."""
    out = {k: e.get(k) for k in EXECUTOR_TOLD}
    out["name"] = name(e.get("machine"), e.get("kind"))["name"]
    # its verification runs are administrators' alone (P4.5.8 rule 2): a member's count leaves them out
    out["running"] = max(0, int(e.get("running") or 0) - int(e.get("verifying") or 0))
    if e.get("state") == "failed" and out.get("available"):
        out["available"] = HOST_STOPPED
    return out


def told_waiting(exp: dict, name) -> Optional[str]:
    """Why an experiment's games wait, each executor named by ``name`` as the viewer may be told it (the
    dispatcher's ``waiting_on``); its words as they are when no executor is named in them."""
    parts = exp.get("waiting_on") or []
    if not parts:
        return exp.get("waiting")
    # a part naming no machine (the helpers not connected, counted) is its words alone
    return "; ".join(f"{name(p.get('machine'), p.get('kind'))['name']}: {p.get('why')}" if p.get("machine")
                     else str(p.get("why")) for p in parts)


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
    listed = store.experiment_jobs(s, exp)
    if not admin:
        # a withdrawn result's game shows as the job that plays it again (one row a game)
        later = {store.game_of(j.ref) for j, _runs in listed if j.state != "withdrawn"}
        listed = [(j, r) for j, r in listed if j.state != "withdrawn" or store.game_of(j.ref) not in later]
    for job, runs in listed:
        state = job.state if admin else SHOWN.get(job.state, job.state)
        counts[state] = counts.get(state, 0) + 1
        accepted = next((r for r in runs if r.id == job.accepted_run), None)
        if accepted is None and job.state == "settling":
            # its primary's result, committed: what a member is shown of it until it settles (rule 2)
            accepted = next((r for r in reversed(runs) if r.role == "primary" and r.state == "done"), None)
        if not admin:
            runs = [r for r in runs if r.role == "primary"]
        last = runs[-1] if runs else None
        entry = {"i": store.game_of(job.ref), "job": job.id, "kind": job.kind, "state": state,
                 "attempts": job.attempts, "mem_mb": job.mem_mb, "priority": job.priority}
        if accepted is not None and job.state != "withdrawn":
            settled = job.state in ("accepted", "crashed")
            entry.update({"executor": name(accepted.machine_id, accepted.executor),
                          # once it settles (to an administrator too: a settling job's primary is no verdict yet)
                          "verification": verification_of(accepted) if settled else None,
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
        if admin and job.state == "settling" and not any(
                r.role != "primary" and r.state in ("offered", "running") for r in runs):
            # its result waits for a replica no verifier plays yet (P4.5.11: why nothing settles it)
            entry["waiting"] = "waiting for a verifier"
        if admin:
            entry["runs"] = [{"run": r.id, "machine": r.machine_id, "executor": r.executor, "role": r.role,
                              "state": r.state, "verdict": r.verdict, "error": r.error,
                              "offered": _iso(r.offered_at), "ended": _iso(r.ended_at)} for r in runs]
        out.append(entry)
    out.sort(key=lambda e: (e["i"] is None, e["i"]))
    return {"experiment": exp, "jobs": out, "counts": counts}


def verification_of(run) -> str:
    """A settled job's accepted run's verification, as its line says it (P4.5.7): ``reference`` (this server played
    it), ``verified`` (a replica reproduced it), ``unverified`` (accepted without one), ``rejected`` (withdrawn)."""
    if run.executor == "local":
        return "reference"
    return {"match": "verified", "rejected": "rejected"}.get(run.verdict or "", "unverified")


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
    failure). Replicas and tiebreaks are administrators' alone (P4.5.8 rule 2)."""
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
                                                    "verifier", "speed", "promoted", "replicate")})
        # what the credits' bounds and the class check noticed of it (P4.5.7)
        counters["flags"] = {k: v for k, v in (row.flags or {}).items() if k in CPU_FLAGS}
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
    waits: a job whose memory no executor has stays queued, saying so), the runs per executor, the verifiers (this
    server's job host and the trusted machines marked ``verifier``, online or not, each with its verification runs in
    flight and its latest ones' verdicts, a void one among them), the caps on ``new`` machines (P4.5.8 rule 3: which
    are held and why, each owner's new machines with work in hand, this server's share of new machines' replicas), and
    the cost table."""
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
            "verifiers": _verifiers(s, disp, view.get("running") or []),
            "caps": _caps(s, d, disp),
            "experiments": view.get("experiments") or {},
            "costs": d.costs.rows() if d is not None else []}


#: The flags the credits' bounds and the class check put on a machine's row (P4.5.7), shown to administrators.
CPU_FLAGS = ("overreported_cpu", "capped_cpu", "class_suspect", "overreports")
#: How many of a verifier's latest verification runs, and of the recent mismatches and withdrawals, are listed.
RECENT = 50


def _owner_of(s: Session, row) -> Optional[dict]:
    if row is None or not row.owner_id:
        return None
    u = s.get(User, row.owner_id)
    return {"id": u.id, "handle": u.handle, "trust_tier": u.trust_tier or "new"} if u is not None else None


def _verifiers(s: Session, disp: dict, running: list) -> list:
    """The verifiers (P4.5.8 rule 2): this server's job host (verifying while its canary and self-check pass) and the
    trusted machines an administrator marked ``verifier``, each with its verification runs in flight and its latest
    verification runs' verdicts (``match``, ``mismatch``, ``reference``; ``unchecked`` with ``void`` for a replica whose
    machine stopped verifying while it played)."""
    from sqlalchemy import select

    from ..db.models import CpuJob, CpuJobRun
    from ..jobs import trust as T
    live = {e.get("machine"): e for e in disp.get("executors") or []}
    out = []
    for e in disp.get("executors") or []:
        if e.get("kind") == "local":
            out.append({"machine": e.get("machine"), "name": e.get("name"), "kind": "local", "on": e.get("on"),
                        "verifiable": e.get("verifiable"), "why": e.get("verify_reason"), "online": True})
    for row in s.scalars(select(Server).where(Server.trust_tier == "trusted")):
        if not T.is_verifier(row.trust_tier, row.trust):
            continue
        e = live.get(row.id) or {}
        out.append({"machine": row.id, "name": row.name, "kind": "remote", "owner": _owner_of(s, row),
                    "verifiable": e.get("verifiable"), "online": bool(e.get("online")), "state": e.get("state"),
                    "since": (row.trust or {}).get("since")})
    for v in out:
        v["in_flight"] = [{"run": r["run"], "job": r["job"], "exp": r["exp"], "i": r["i"], "role": r["role"],
                           "turn": r.get("turn")} for r in running
                          if r.get("machine") == v["machine"] and r.get("role") != "primary"]
        latest = s.execute(select(CpuJobRun, CpuJob).join(CpuJob, CpuJob.id == CpuJobRun.job_id)
                           .where(CpuJobRun.machine_id == v["machine"], CpuJobRun.role.in_(("replica", "tiebreak")),
                                  CpuJobRun.state == "done")
                           .order_by(CpuJobRun.ended_at.desc()).limit(10))
        v["latest"] = [{"run": r.id, "exp": j.item_ref, "i": _game(j.ref), "role": r.role, "verdict": r.verdict,
                        "void": (r.error or {}).get("code") == "void", "ended": _iso(r.ended_at)}
                       for r, j in latest]
    return out


def _caps(s: Session, d, disp: dict) -> dict:
    """The caps on ``new`` machines (P4.5.8 rule 3): the settings, each new helper machine with its owner, its work in
    hand and why it is held when it is (its owner's other new machines busy, or its results waiting for a verifier),
    and this server's share of new machines' replicas against its other runs while both waited."""
    from ..jobs import trust as T
    sets = T.settings(s, fresh=True)
    hands = d.hands_view() if d is not None else {}
    machines = []
    for e in disp.get("executors") or []:
        if e.get("kind") == "local":
            continue
        row = s.get(Server, e.get("machine"))
        if row is None or (row.trust_tier or "new") != "new":
            continue
        why = e.get("available") or ""
        held = why if why.startswith(("waiting for a verifier", "its owner's other new machines")) else None
        machines.append({"machine": row.id, "name": row.name, "owner": _owner_of(s, row),
                         "in_hand": int(hands.get("in_hand", {}).get(row.id, 0)),
                         "waiting_replicas": int(hands.get("settling", {}).get(row.id, 0)), "held": held})
    return {"new_per_owner": sets["new_per_owner"], "new_verify_share": sets["new_verify_share"],
            "new_machines": machines, "share": hands.get("share", {})}


@router.get("/api/admin/verification")
def admin_verification(me: User = Depends(require_admin_hidden), s: Session = Depends(get_db, scope="function")):
    """Verification as administrators see it (P4.5.10, the ``#/admin/verification`` page): the settings with their
    defaults and bounds; the quarantined machines with their evidence (the verdict in words, the round where two chains
    parted, the withdrawn jobs) and the quarantined owners with why; the recent mismatches and withdrawals; the
    verifiers; and the machines the credits' bounds or the class check flagged, with their measured speed."""
    return verification_view(s)


@router.put("/api/admin/verification")
def admin_verification_change(request: Request, body: dict = Body(...), me: User = Depends(require_admin_hidden),
                              s: Session = Depends(get_db, scope="function")):
    """Change any of the verification settings (P4.5.8): the rates and the share from 0 to 1, the counts from 1 to
    10,000 (``trust.BOUNDS``); an unknown name or a value out of its bounds is 400 and changes nothing. One audit row
    naming what changed; the dispatcher reads them again at once."""
    from ..jobs import dispatcher as jobs_dispatcher, trust as T
    from ..auth import ratelimit
    if not isinstance(body, dict) or not body:
        raise HTTPException(400, f"The body names one or more of: {', '.join(T.DEFAULTS)}.")
    try:
        changes = T.clean(body)
    except T.SettingsError as exc:
        raise HTTPException(400, str(exc)) from None
    before = T.settings(s, fresh=True)
    after = T.save_settings(s, changes)
    changed = {k: {"from": before[k], "to": after[k]} for k in changes if before[k] != after[k]}
    if changed:
        audit.record(s, "verification.settings", actor=me, ip=ratelimit.client_ip(request), changed=changed)
    s.commit()
    T.forget()
    jobs_dispatcher.poke()
    return verification_view(s)


def verification_view(s: Session) -> dict:
    """``GET /api/admin/verification``'s answer (the route's doc)."""
    from sqlalchemy import select

    from ..db.models import AuditLog, CpuJob, CpuJobRun
    from ..jobs import dispatcher as jobs_dispatcher, trust as T
    names: dict = {}

    def machine_name(mid: Optional[str]) -> Optional[str]:
        if mid and mid not in names:
            row = s.get(Server, mid)
            names[mid] = row.name if row is not None else mid
        return names.get(mid) if mid else None
    quarantined, suspects, tiers = [], [], {}
    for row in s.scalars(select(Server).where(Server.reach == "worker").order_by(Server.name)):
        tier = row.trust_tier or "new"
        tiers[tier] = tiers.get(tier, 0) + 1
        trust = dict(row.trust or {})
        if tier == "quarantined":
            quarantined.append({"machine": row.id, "name": row.name, "owner": _owner_of(s, row),
                                "quarantined_at": trust.get("quarantined_at"), "evidence": trust.get("last_mismatch"),
                                "mismatches": int(trust.get("mismatches") or 0),
                                "claim_failures": int(trust.get("claim_failures") or 0)})
        flags = {k: v for k, v in (row.flags or {}).items() if k in CPU_FLAGS}
        if flags:
            suspects.append({"machine": row.id, "name": row.name, "owner": _owner_of(s, row), "tier": tier,
                             "speed": T.speed_of(trust), "flags": flags})
    owners = []
    for u in s.scalars(select(User).where(User.trust_tier == "quarantined").order_by(User.handle)):
        why = s.scalar(select(AuditLog).where(AuditLog.action == "user.trust", AuditLog.object_id == u.id)
                       .order_by(AuditLog.id.desc()).limit(1))
        detail = dict(why.detail or {}) if why is not None else {}
        owners.append({"id": u.id, "handle": u.handle, "display_name": u.display_name,
                       "why": detail.get("why"), "run": detail.get("machine_run"),
                       "at": _iso(why.at) if why is not None else None})
    mismatches = []
    for r, j in s.execute(select(CpuJobRun, CpuJob).join(CpuJob, CpuJob.id == CpuJobRun.job_id)
                          .where(CpuJobRun.verdict.in_(("mismatch", "rejected")))
                          .order_by(CpuJobRun.ended_at.desc()).limit(RECENT * 4)):
        ev = r.error if isinstance(r.error, dict) else {}
        if not ev.get("verdict"):
            continue                # a withdrawal or a re-opened match, listed with the withdrawals
        mismatches.append({"run": r.id, "machine": r.machine_id, "name": machine_name(r.machine_id),
                           "role": r.role, "exp": j.item_ref, "i": _game(j.ref), "verdict": ev.get("verdict"),
                           "words": ev.get("words"), "round": ev.get("round"), "against": ev.get("against"),
                           "at": ev.get("at") or _iso(r.ended_at)})
        if len(mismatches) >= RECENT:
            break
    withdrawals = []
    for j in s.scalars(select(CpuJob).where(CpuJob.state == "withdrawn").order_by(CpuJob.updated_at.desc())
                       .limit(RECENT)):
        prow = s.scalar(select(CpuJobRun).where(CpuJobRun.job_id == j.id, CpuJobRun.role == "primary",
                                                CpuJobRun.verdict.in_(("rejected", "mismatch")))
                        .order_by(CpuJobRun.ended_at.desc()).limit(1))
        withdrawals.append({"job": j.id, "exp": j.item_ref, "i": _game(j.ref),
                            "machine": prow.machine_id if prow is not None else None,
                            "name": machine_name(prow.machine_id) if prow is not None else None,
                            "run": prow.id if prow is not None else None,
                            "why": ((prow.error or {}).get("why") if prow is not None else None),
                            "at": _iso(j.updated_at)})
    disp = (jobs_dispatcher.view().get("dispatcher") or {})
    return {"settings": T.settings(s, fresh=True), "defaults": dict(T.DEFAULTS),
            "bounds": {k: list(v) for k, v in T.BOUNDS.items()}, "tiers": tiers,
            "quarantined": {"machines": quarantined, "owners": owners},
            "mismatches": mismatches, "withdrawals": withdrawals,
            "verifiers": _verifiers(s, disp, jobs_dispatcher.view().get("running") or []),
            "flagged": suspects}


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

