"""The compute ledger and the hardware classes over HTTP (design/phase3-features.md P3.4.3, P3.4.4).

    GET    /api/ledger/me                       the caller's given and used compute and the ratio, by machine and by
                                                activity kind, and their own use of their own machines; ?range=24h|7d|
                                                30d|all (default 30d)
    GET    /api/users/{handle}/ratio            an account's totals and ratio, for anyone its `ratio` profile field
                                                shows to (administrators always); else 404; ?range= as above
    GET    /api/admin/hardware-classes          every class, and the machines each prices now
    POST   /api/admin/hardware-classes          a class: {kind, name, rate, match, priority, is_default}
    PUT    /api/admin/hardware-classes/{id}     change any of those but its kind
    DELETE /api/admin/hardware-classes/{id}     remove a class that is no kind's default

A member's ratio follows the field's visibility (``profiles.require_visible(..., field="ratio")``: 404 alike for no such
account, a hidden field and an account not in good standing), so the route is open to visitors for an account that made
its ratio `global`. The class routes are administrators' (``require_role("admin", hide=True)``: a member gets 404). A
class's new rate applies to events written from then on; ``citar admin reprice`` recomputes a period.
"""
from __future__ import annotations

import re
from typing import Optional

from fastapi import APIRouter, Body, Depends, HTTPException, Query, Request
from sqlalchemy import select
from sqlalchemy.orm import Session

from .. import ledger
from ..auth import audit, profiles, ratelimit
from ..auth.deps import Principal, get_db, principal, require_role, require_user
from ..db.models import HARDWARE_KINDS, HardwareClass, Server, User

router = APIRouter()
require_admin_hidden = require_role("admin", hide=True)

#: What a class's ``match`` may say (``ledger.matches``), and how long its texts may be.
MATCH_KEYS = {"gpu_name": str, "cpu_name": str, "vram_gb_min": float, "cores_min": float}
NAME_MAX, PATTERN_MAX = 80, 200
RATE_MAX = 1e9
PRIORITY_RANGE = (-1000, 1000)


def _range(range_: str) -> str:
    if range_ not in ledger.RANGES:
        raise HTTPException(400, f"A range is one of {', '.join(ledger.RANGES)}.")
    return range_


@router.get("/api/ledger/me")
def ledger_me(range_: str = Query("30d", alias="range"), me: User = Depends(require_user),
              s: Session = Depends(get_db, scope="function")):
    """The caller's compute (``ledger.breakdown``): given and used, the ratio, where they came from."""
    return ledger.breakdown(s, me, _range(range_))


@router.get("/api/users/{handle}/ratio")
def user_ratio(handle: str, range_: str = Query("30d", alias="range"), p: Principal = Depends(principal),
               s: Session = Depends(get_db, scope="function")):
    """An account's given, used and ratio (``ledger.summary``) when its `ratio` field shows to the caller (P3.2.6:
    `private` by default, so the account itself and administrators; `server` members; `global` anyone); else 404."""
    user = profiles.require_visible(s, handle, p.user, field="ratio")
    return {"handle": user.handle, **ledger.summary(s, user.id, _range(range_))}


# ---------------------------------------------------------------------------- hardware classes

def _clean(body: dict, *, current: Optional[HardwareClass] = None) -> dict:
    """A class's fields from a request body, checked; for a change, only the fields given (the kind never changes)."""
    if not isinstance(body, dict):
        raise HTTPException(400, "A class is an object.")
    unknown = sorted(set(body) - {"kind", "name", "rate", "match", "priority", "is_default"})
    if unknown:
        raise HTTPException(400, f"Unknown field {unknown[0]!r}.")
    out: dict = {}
    if current is None:
        if body.get("kind") not in HARDWARE_KINDS:
            raise HTTPException(400, f"A class's kind is one of {', '.join(HARDWARE_KINDS)}.")
        out["kind"] = body["kind"]
    elif "kind" in body and body["kind"] != current.kind:
        raise HTTPException(400, "A class's kind does not change; make a new class instead.")
    if "name" in body or current is None:
        name = body.get("name")
        if not isinstance(name, str) or not name.strip() or len(name.strip()) > NAME_MAX:
            raise HTTPException(400, f"A class needs a name of at most {NAME_MAX} characters.")
        out["name"] = name.strip()
    if "rate" in body or current is None:
        rate = body.get("rate")
        # the range compared first: NaN and infinity fall outside it, and ``math.isfinite`` of an integer of hundreds
        # of digits (which the JSON reader takes) would overflow rather than answer
        if isinstance(rate, bool) or not isinstance(rate, (int, float)) or not 0 <= rate <= RATE_MAX:
            raise HTTPException(400, "A rate is a number of credits from 0 to 1,000,000,000.")
        out["rate"] = float(rate)
    if "match" in body:
        match = body.get("match") or {}
        if not isinstance(match, dict):
            raise HTTPException(400, "A class's match is an object.")
        clean = {}
        for k, v in match.items():
            if k not in MATCH_KEYS:
                raise HTTPException(400, f"A match says {', '.join(MATCH_KEYS)}; not {k!r}.")
            if v in (None, ""):
                continue
            if MATCH_KEYS[k] is str:
                if not isinstance(v, str) or len(v) > PATTERN_MAX:
                    raise HTTPException(400, f"{k} is a regular expression of at most {PATTERN_MAX} characters.")
                try:
                    re.compile(v)
                except re.error as e:
                    raise HTTPException(400, f"{k} is not a regular expression: {e}.") from None
                clean[k] = v
            else:
                if isinstance(v, bool) or not isinstance(v, (int, float)) or not 0 <= v <= 1e6:
                    raise HTTPException(400, f"{k} is a number from 0 up.")
                clean[k] = float(v)
        out["match"] = clean
    elif current is None:
        out["match"] = {}
    if "priority" in body or current is None:
        pr = body.get("priority", 0)
        if isinstance(pr, bool) or not isinstance(pr, int) or not PRIORITY_RANGE[0] <= pr <= PRIORITY_RANGE[1]:
            raise HTTPException(400, "A priority is a whole number from -1000 to 1000.")
        out["priority"] = pr
    if "is_default" in body or current is None:
        d = body.get("is_default", False)
        if not isinstance(d, bool):
            raise HTTPException(400, "is_default is true or false.")
        out["is_default"] = d
    return out


def _classes_changed(s: Session) -> None:
    """The classes changed: every machine not pinned is classified again, and the ledger prices with them from the
    next event on."""
    s.flush()
    ledger.reclassify(s)
    led = ledger.current()
    if led is not None:
        led.invalidate_classes()


def _client(c: HardwareClass, machines: list) -> dict:
    return {**ledger.class_dict(c), "updated_at": c.updated_at.isoformat() if c.updated_at else None,
            "updated_by": c.updated_by, "machines": machines}


@router.get("/api/admin/hardware-classes")
def list_classes(me: User = Depends(require_admin_hidden), s: Session = Depends(get_db, scope="function")):
    """Every class with the machines it prices now (``ledger.class_of``: a pinned class, else the hardware's)."""
    from .. import servers
    rows = list(s.scalars(select(HardwareClass)))
    classes = [ledger.class_dict(c) for c in rows]
    priced: dict = {c.id: [] for c in rows}
    for row in s.scalars(select(Server).order_by(Server.name)):
        facts = servers.machine_facts(row.id) or {"hardware": (row.config or {}).get("hardware") or {},
                                                  "class_override": row.class_override,
                                                  "gpu_class_id": row.gpu_class_id, "cpu_class_id": row.cpu_class_id}
        kinds = ("api",) if row.kind == "api" else ("gpu", "cpu")
        for kind in kinds:
            c = ledger.class_of(facts, kind, classes)
            if c is not None:
                priced[c["id"]].append({"id": row.id, "name": row.name, "pinned": bool(row.class_override)})
    order = {k: i for i, k in enumerate(HARDWARE_KINDS)}
    rows.sort(key=lambda c: (order.get(c.kind, 9), not c.is_default, -(c.priority or 0), c.name))
    return {"classes": [_client(c, priced.get(c.id, [])) for c in rows], "kinds": list(HARDWARE_KINDS)}


def _one(s: Session, class_id: str) -> HardwareClass:
    row = s.get(HardwareClass, class_id)
    if row is None:
        raise HTTPException(404, "No such class.")
    return row


def _set_default(s: Session, row: HardwareClass) -> None:
    """``row`` becomes its kind's one default."""
    for other in s.scalars(select(HardwareClass).where(HardwareClass.kind == row.kind, HardwareClass.id != row.id)):
        other.is_default = False


@router.post("/api/admin/hardware-classes")
def create_class(request: Request, body: dict = Body(...), me: User = Depends(require_admin_hidden),
                 s: Session = Depends(get_db, scope="function")):
    """A new class; one marked the default replaces its kind's default."""
    fields = _clean(body)
    row = HardwareClass(**fields, updated_by=me.id)
    s.add(row)
    s.flush()
    if row.is_default:
        _set_default(s, row)
    audit.record(s, "hardware_class.created", actor=me, object_type="hardware_class", object_id=row.id,
                 ip=ratelimit.client_ip(request), **fields)
    _classes_changed(s)
    s.commit()
    return _client(row, [])


@router.put("/api/admin/hardware-classes/{class_id}")
def update_class(class_id: str, request: Request, body: dict = Body(...), me: User = Depends(require_admin_hidden),
                 s: Session = Depends(get_db, scope="function")):
    """Change a class (its kind stays). Its kind always keeps one default: unmarking the default is refused (409);
    mark another class the default instead."""
    row = _one(s, class_id)
    fields = _clean(body, current=row)
    if row.is_default and fields.get("is_default") is False:
        raise HTTPException(409, "Every kind keeps a default class: mark another one the default instead.")
    before = {k: getattr(row, k) for k in fields}
    for k, v in fields.items():
        setattr(row, k, v)
    row.updated_by = me.id
    if row.is_default:
        _set_default(s, row)
    changed = {k: {"from": before[k], "to": v} for k, v in fields.items() if before[k] != v}
    if changed:
        audit.record(s, "hardware_class.updated", actor=me, object_type="hardware_class", object_id=row.id,
                     ip=ratelimit.client_ip(request), changed=changed)
    _classes_changed(s)
    s.commit()
    return _client(row, [])


@router.delete("/api/admin/hardware-classes/{class_id}")
def delete_class(class_id: str, request: Request, me: User = Depends(require_admin_hidden),
                 s: Session = Depends(get_db, scope="function")):
    """Remove a class that is not its kind's default (409 for the default); the machines it priced fall to the next
    class that fits them, and a machine pinned to it is no longer pinned."""
    row = _one(s, class_id)
    if row.is_default:
        raise HTTPException(409, "A kind's default class cannot be removed: mark another one the default first.")
    for sv in s.scalars(select(Server).where((Server.gpu_class_id == row.id) | (Server.cpu_class_id == row.id))):
        if sv.gpu_class_id == row.id:
            sv.gpu_class_id = None
        if sv.cpu_class_id == row.id:
            sv.cpu_class_id = None
        if sv.class_override and sv.gpu_class_id is None and sv.cpu_class_id is None:
            sv.class_override = False
    audit.record(s, "hardware_class.deleted", actor=me, object_type="hardware_class", object_id=row.id,
                 ip=ratelimit.client_ip(request), name=row.name, kind=row.kind, rate=row.rate)
    s.delete(row)
    _classes_changed(s)
    s.commit()
    return {"deleted": class_id}
