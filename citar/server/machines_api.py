"""The machine settings dialog's routes (design/phase3-features.md P3.8.4; ``citar.machine_settings``).

    GET  /api/machines/{sid}/settings          the dialog's read: capabilities, slots (and what the prompt scheduler
                                               has in effect), modes, the helper's last report and how the
                                               configuration differs from it, class and trust tier
    PUT  /api/machines/{sid}/settings          change any of ``slots``, ``capabilities`` (``llm``, ``cpu``) and
                                               ``modes`` (``hardware``, ``models``, ``power``: ``auto`` | ``manual``)
    POST /api/machines/{sid}/hardware/apply    copy the last report into the configuration: ``{"sections": [...]}``
                                               (``hardware``, ``models``, ``power``; none or empty: all of them)
    PUT  /api/admin/machines/{sid}/trust       ``{"trust_tier": "new" | "provisional" | "trusted" | "quarantined"}``
    PUT  /api/admin/machines/{sid}/class       ``{"pinned": true, "gpu_class_id"?, "cpu_class_id"?}`` pins the
                                               classes it is priced by; ``{"pinned": false}`` lets them follow its
                                               hardware again

The first three are the machine's owner's and administrators' (MANAGE on its row: the site's machines, the host and its
online APIs, are administrators' whoever is named their owner): anyone a group lends it to gets 403, anyone else 404,
a visitor 401. The last two are administrators' alone (``require_role("admin", hide=True)``: anyone else signed in gets
404). Each change writes one audit row and commits before it answers (the dialog reads it back at once); a change of
slots or capabilities reaches the prompt scheduler at once (``Scheduler.refresh``), a pin the compute ledger from the
next event on.
"""
from __future__ import annotations

import logging
from typing import Optional

from fastapi import APIRouter, Body, Depends, HTTPException, Request
from sqlalchemy import select
from sqlalchemy.orm import Session

from .. import ledger, machine_settings as M, servers
from ..auth import access, audit, ratelimit
from ..auth.deps import Principal, get_db, principal, require_role, require_user
from ..db.models import TRUST_TIERS, HardwareClass, Server, User

log = logging.getLogger("citar.machines_api")
router = APIRouter()
require_admin_hidden = require_role("admin", hide=True)


def _server(s: Session, sid: str, me: User, permission: str) -> Server:
    """A machine the caller has ``permission`` on: 404 when they may not see it at all (a 403 would confirm that it
    exists), 403 when they may see it but not this (a member a group lends it to)."""
    row = s.get(Server, sid) if isinstance(sid, str) else None
    if row is None:
        raise HTTPException(404, "No such server.")
    access.on(s, me, row).require(permission)
    return row


def _in_effect(server_id: str) -> Optional[dict]:
    """What the prompt scheduler has in effect for the machine now: its capacity, slots, the helper's own ceiling,
    whether it takes model work and lets pooled prompts swap models (``Machine.settings_view``). None when it cannot
    say."""
    try:
        from ..pool import scheduler
        m = scheduler.get().machine(server_id)
        return m.settings_view() if m is not None else None
    except Exception:
        log.warning("could not read machine %s's scheduler state", server_id, exc_info=True)
        return None


def _refresh(server_id: str) -> None:
    """Have the prompt scheduler read the machine's row again now (its slots, its swaps, LLM on or off): the change
    takes effect for the next request, not after the scheduler's own reread. Never raises."""
    try:
        from ..pool import scheduler
        sched = scheduler.current()
        if sched is not None:
            sched.refresh(server_id)
    except Exception:
        log.warning("could not refresh machine %s in the scheduler", server_id, exc_info=True)


def _answer(s: Session, row: Server, me: User) -> dict:
    return M.client(s, row, me, in_effect=_in_effect(row.id))


def _fail(exc: M.SettingsError) -> HTTPException:
    return HTTPException(exc.status, str(exc))


@router.get("/api/machines/{sid}/settings")
def settings_read(sid: str, request: Request, p: Principal = Depends(principal), s: Session = Depends(get_db)):
    """The machine settings dialog's read of a machine its caller manages."""
    me = require_user(request, p)
    row = _server(s, sid, me, access.MANAGE)
    return _answer(s, row, me)


@router.put("/api/machines/{sid}/settings")
def settings_change(sid: str, request: Request, body: dict = Body(...), p: Principal = Depends(principal),
                    s: Session = Depends(get_db)):
    """Change a machine's slots, capabilities or modes (any of them; the rest stay)."""
    me = require_user(request, p)
    row = _server(s, sid, me, access.MANAGE)
    try:
        changed = M.update(row, body)
    except M.SettingsError as exc:
        raise _fail(exc) from None
    if changed:
        audit.record(s, "server.settings", actor=me, object_type="server", object_id=row.id,
                     ip=ratelimit.client_ip(request), changed=changed)
    s.commit()
    _refresh(row.id)
    return _answer(s, row, me)


@router.post("/api/machines/{sid}/hardware/apply")
def settings_apply(sid: str, request: Request, body: Optional[dict] = Body(None), p: Principal = Depends(principal),
                   s: Session = Depends(get_db)):
    """Copy the machine's last report (its helper's scan, its suggested power figures, its models), whole or by
    section, into its configuration, whatever its modes."""
    me = require_user(request, p)
    row = _server(s, sid, me, access.MANAGE)
    body = body or {}
    if not isinstance(body, dict) or set(body) - {"sections"}:
        raise HTTPException(400, "The body is {\"sections\": [...]}, or nothing for every section.")
    try:
        applied = M.apply_scan(row, body.get("sections"))
    except M.SettingsError as exc:
        raise _fail(exc) from None
    audit.record(s, "server.scan_applied", actor=me, object_type="server", object_id=row.id,
                 ip=ratelimit.client_ip(request), sections=applied)
    s.commit()
    return _answer(s, row, me)


@router.put("/api/admin/machines/{sid}/trust")
def machine_trust(sid: str, request: Request, body: dict = Body(...), me: User = Depends(require_admin_hidden),
                  s: Session = Depends(get_db)):
    """Set a machine's trust tier (administrators; Phase 4 sets it automatically and acts on it)."""
    row = _server(s, sid, me, access.MANAGE)
    if not isinstance(body, dict) or set(body) != {"trust_tier"} or body["trust_tier"] not in TRUST_TIERS:
        raise HTTPException(400, f"The body is {{\"trust_tier\": one of {', '.join(TRUST_TIERS)}}}.")
    before = row.trust_tier or "new"
    if body["trust_tier"] != before:
        row.trust_tier = body["trust_tier"]
        audit.record(s, "server.trust", actor=me, object_type="server", object_id=row.id,
                     ip=ratelimit.client_ip(request), changed={"from": before, "to": row.trust_tier})
    s.commit()
    return _answer(s, row, me)


def _class(s: Session, class_id, kinds: tuple, what: str) -> str:
    """A hardware class's id, checked: it exists and is of one of ``kinds``."""
    row = s.get(HardwareClass, class_id) if isinstance(class_id, str) else None
    if row is None or row.kind not in kinds:
        raise HTTPException(400, f"{what} is the id of a {' or '.join(kinds)} class.")
    return row.id


@router.put("/api/admin/machines/{sid}/class")
def machine_class(sid: str, request: Request, body: dict = Body(...), me: User = Depends(require_admin_hidden),
                  s: Session = Depends(get_db)):
    """Pin the hardware classes a machine is priced by, or let them follow its hardware again (administrators). A
    pin names a GPU class and a CPU class (an online API: its API class alone); a class it leaves out is the one the
    machine has now. The compute ledger prices by them from the next event on (``citar admin reprice`` recomputes a
    period)."""
    row = _server(s, sid, me, access.MANAGE)
    if not isinstance(body, dict) or not isinstance(body.get("pinned"), bool) \
            or set(body) - {"pinned", "gpu_class_id", "cpu_class_id"}:
        raise HTTPException(400, "The body is {\"pinned\": true or false, \"gpu_class_id\", \"cpu_class_id\"}.")
    api = (row.kind or "owned") == "api"
    before = {"pinned": bool(row.class_override), "gpu": row.gpu_class_id, "cpu": row.cpu_class_id}
    if body["pinned"]:
        if api and body.get("cpu_class_id") is not None:
            raise HTTPException(400, "An online API is priced by its API class alone.")
        classes = [ledger.class_dict(c) for c in s.scalars(select(HardwareClass))]
        facts = {"hardware": (row.config or {}).get("hardware") or {}, "class_override": bool(row.class_override),
                 "gpu_class_id": row.gpu_class_id, "cpu_class_id": row.cpu_class_id}
        now_gpu = ledger.class_of(facts, "api" if api else "gpu", classes)
        now_cpu = None if api else ledger.class_of(facts, "cpu", classes)
        gpu = (_class(s, body["gpu_class_id"], ("api",) if api else ("gpu",), "gpu_class_id")
               if body.get("gpu_class_id") is not None else (now_gpu["id"] if now_gpu else None))
        cpu = None if api else (_class(s, body["cpu_class_id"], ("cpu",), "cpu_class_id")
                                if body.get("cpu_class_id") is not None else (now_cpu["id"] if now_cpu else None))
        row.gpu_class_id, row.cpu_class_id, row.class_override = gpu, cpu, True
    else:
        if body.get("gpu_class_id") is not None or body.get("cpu_class_id") is not None:
            raise HTTPException(400, "Unpinned, the classes follow the hardware: name none.")
        row.class_override = False
        s.flush()
        ledger.reclassify(s, [row])
    after = {"pinned": bool(row.class_override), "gpu": row.gpu_class_id, "cpu": row.cpu_class_id}
    if after != before:
        audit.record(s, "server.class", actor=me, object_type="server", object_id=row.id,
                     ip=ratelimit.client_ip(request), changed={"from": before, "to": after})
    s.commit()
    servers.invalidate()            # the ledger prices from the registry's facts of the machine (``machine_facts``)
    return _answer(s, row, me)
