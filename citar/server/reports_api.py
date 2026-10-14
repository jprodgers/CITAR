"""HTTP API for the Reports page: the report builder, its runs and the generated HTML.

A report is an owned item of kind ``report`` (design/phase3-features.md P3.2.4): building one needs ``run_reports``
and it is the caller's; reading it needs VIEW on it (so a `global` report reads with no account at all, and a
`link` one with its key); rerunning and deleting need MANAGE. The report runner writes the row
(``ReportRunner.start``).

- **What a report gathers** is its owner's reach (``reports.reach.Reach``): everything for an administrator's, and for
  a member's the things they may see, with costs on their own machines only. The builder's options follow the
  caller's reach (``reports.scope_options``).
- **What a reader is given** beyond the report itself: a report's managers get its whole metadata and
  specification; anyone else it reaches gets neither a failed build's traceback nor its owner (``_meta_for``), and
  of its specification only what they could have chosen themselves (``_spec_for``).
- **Sharing** goes through ``items_api``'s generic routes (``/api/items/report/{ref}/sharing``, ``.../unpublish``),
  and the page for people with no account is ``share_api``'s ``/r/{slug}``.
"""
from __future__ import annotations

from typing import Optional

from fastapi import APIRouter, Depends, HTTPException, Request
from fastapi.responses import FileResponse, HTMLResponse
from sqlalchemy.orm import Session

from .. import items
from ..auth import access
from ..auth.deps import Principal, get_db, principal, require_cap, require_user
from ..reports import NARRATIVE_PURPOSE  # the kind of work a narrative is admitted for
from . import work_access

router = APIRouter()
_runner = None


def reports_runner():
    """The report runner, created on first use."""
    global _runner
    if _runner is None:
        from ..reports import ReportRunner
        _runner = ReportRunner()
    return _runner


def _call(fn, *a, **kw):
    """Call a runner function, turning its errors into HTTP status codes."""
    try:
        return fn(*a, **kw)
    except KeyError as e:
        raise HTTPException(404, f"Not found: {e}")
    except ValueError as e:
        raise HTTPException(400, str(e))


def _key(request: Request) -> Optional[str]:
    """The share key a request carries, if any."""
    return (request.query_params.get("k") or request.query_params.get("key")
            or request.headers.get("x-citar-share-key"))


def _is_admin(user) -> bool:
    """An administrator in good standing."""
    return user is not None and user.role == "admin" and user.status == "active"


def _narrative_gate(s: Session, spec: dict, user) -> None:
    """A report's written analysis runs on the machine its (normalized) spec names: a member's names one
    (P3.2.9, ``servers.member_llm``) and is admitted to it (``pool.seats.admit``: their own machine, or a group's they
    hold a grant on, the site's own included; a machine they cannot see is refused without being named). When it may
    run is the runner's business."""
    from .. import servers
    from ..pool import seats as pool_seats
    narrative = spec["narrative"]
    if not narrative.get("enabled") or _is_admin(user):
        return
    try:
        checked = servers.member_llm({"server_id": narrative.get("server_id")}, user, required=True)
    except servers.SeatBlockError as e:
        raise HTTPException(e.status, str(e)) from None
    try:
        pool_seats.admit(s, user, [checked.get("server_id")], NARRATIVE_PURPOSE)
    except ValueError as e:
        raise HTTPException(403, str(e)) from None


@router.get("/api/reports")
def reports_list(request: Request, p: Principal = Depends(principal), s: Session = Depends(get_db, scope="function")):
    """The reports the caller may see (and the one a share key opens), newest first, each with what they may do to
    it."""
    key = _key(request)
    seen = items.listed(s, "report", p.user, slug=key)
    out = [m for m in reports_runner().list() if seen is None or m.get("id") in seen]
    perms = items.permissions_by_ref(s, "report", p.user, [m["id"] for m in out], slug=key)
    name = work_access.namer(s, p.user, admin=_is_admin(p.user))
    return [_meta_for(work_access.with_waiting(dict(m, **perms.get(m["id"], {})), name)) for m in out]


#: What a report's metadata keeps for its managers only: a failed build's traceback (the server's code and paths)
#: and the account that owns it.
MANAGERS_ONLY = ("trace", "owner_id")


def _meta_for(meta: dict, manages: bool = None) -> dict:
    """A report's metadata as a caller sees it: without ``MANAGERS_ONLY`` for one who does not manage it (a member it
    is shared with, a visitor holding its link)."""
    if manages is None:
        manages = bool(meta.get("can_manage"))
    return meta if manages else {k: v for k, v in meta.items() if k not in MANAGERS_ONLY}


@router.get("/api/reports/options")
def reports_options(me=Depends(require_user)):
    """What the report builder can pick from: machines, models, and recent items. A member is offered what their
    report could gather (``reach.Reach``: the activities of the things they may see) and the machines they may see
    (their own, and those shared with them: the site's own only when they are)."""
    from ..reports import scope_options
    from ..reports.reach import Reach
    return scope_options(Reach.of(me.id))


@router.post("/api/reports")
def reports_start(body: dict, me=Depends(require_cap("run_reports")), s: Session = Depends(get_db, scope="function")):
    """Start building a report: the caller's."""
    from ..reports import normalize_spec
    spec = _call(normalize_spec, body)
    _narrative_gate(s, spec, me)
    return _call(reports_runner().start, spec, owner_id=me.id, session=s)


#: The kinds of item a report's scope may name (``reports.data._in_scope``), as the owned item kind each one is.
#: A scope's ``game`` is a game's row and its ``activity`` an activity of the usage ledger.
SCOPE_ITEM_KINDS = {"benchmark_run": "benchmark_run", "probe": "probe", "probe_run": "probe_run",
                    "lab_experiment": "lab_experiment", "suite": "benchmark_suite"}


def _spec_for(s: Session, spec: dict, user, manages: bool) -> dict:
    """A report's specification as a caller sees it. Its managers get all of it. Anyone else it reaches (a member
    it is shared with, a visitor holding its link) gets what they could have chosen themselves: the items of its
    scope they may see (``_scope_items``), the machines they may see (``_scope_machines``), and of its narrative
    only whether there is one, since its instructions and the machine, model and profile that wrote it are its
    owner's. The shape stays a specification's, so "Edit copy" loads it into the builder."""
    if manages:
        return spec
    viewer = user if user is not None and user.active else None
    scope = spec.get("scope") or {}
    narrative = spec.get("narrative") or {}
    return {**spec,
            "scope": {**scope, "items": _scope_items(s, scope.get("items") or [], viewer),
                      "servers": _scope_machines(s, scope.get("servers") or [], viewer)},
            "narrative": {"enabled": bool(narrative.get("enabled")), "server_id": None, "model_id": None,
                          "profile_id": None, "instructions": ""}}


def _scope_items(s: Session, chosen: list, viewer) -> list:
    """The items of a scope ``viewer`` may see (a game by its row, an owned item by its kind; ``access``'s
    listing filter, so for nobody in good standing the published ones); the rest, an activity id included, left
    out, names and ids alike."""
    from . import ownership
    seen: dict = {}
    out = []
    for it in chosen:
        kind = it.get("kind") if isinstance(it, dict) else None
        if kind == "game":
            if "game" not in seen:
                seen["game"] = ownership.visible_ids(s, viewer) or set()
            ok = it.get("id") in seen["game"]
        elif kind in SCOPE_ITEM_KINDS:
            if kind not in seen:
                seen[kind] = items.visible_refs(s, SCOPE_ITEM_KINDS[kind], viewer)
            ok = it.get("id") in seen[kind]
        else:
            ok = False
        if ok:
            out.append(it)
    return out


def _scope_machines(s: Session, chosen: list, viewer) -> list:
    """The machines of a scope ``viewer`` may see: one they hold VIEW on (every machine is a row, P3.2.8)."""
    from ..db.models import Server
    out = []
    for sid in chosen:
        row = s.get(Server, sid) if isinstance(sid, str) else None
        if row is not None and access.VIEW in access.on(s, viewer, row):
            out.append(sid)
    return out


@router.get("/api/reports/{rid}")
def reports_get(rid: str, request: Request, p: Principal = Depends(principal), s: Session = Depends(get_db, scope="function")):
    """One report's metadata, progress and specification, for whoever may see it: what of them belongs to its
    managers (``_meta_for``, ``_spec_for``) only to them."""
    _row, perms = items.require(s, "report", rid, p.user, access.VIEW, slug=_key(request))
    r = reports_runner()
    manages = access.MANAGE in perms
    # what a queued report waits for names only the work the caller may see (P3.9.1)
    meta = work_access.with_waiting(_call(r.meta, rid), work_access.namer(s, p.user, admin=_is_admin(p.user)))
    return {"meta": _meta_for(meta, manages), "spec": _spec_for(s, _call(r.spec, rid), p.user, manages),
            **perms.client()}


@router.post("/api/reports/{rid}/rerun")
def reports_rerun(rid: str, request: Request, me=Depends(require_cap("run_reports")), s: Session = Depends(get_db, scope="function")):
    """Build a fresh report from the same specification, with current data: for whoever manages the report, and
    the new one is theirs."""
    from ..reports import normalize_spec
    items.require(s, "report", rid, me, access.MANAGE, slug=_key(request))
    r = reports_runner()
    spec = _call(normalize_spec, _call(r.spec, rid))
    _narrative_gate(s, spec, me)
    return _call(r.start, spec, owner_id=me.id, session=s)


@router.get("/api/reports/{rid}/html")
def reports_html(rid: str, request: Request, p: Principal = Depends(principal), s: Session = Depends(get_db, scope="function")):
    """A report's HTML, for viewing in place, for whoever may see it."""
    items.require(s, "report", rid, p.user, access.VIEW, slug=_key(request))
    path = _call(reports_runner().html_path, rid)
    # Shown in a frame on the Reports page, so this page (and only this one) may be framed by the site itself.
    # A report is static HTML and inline SVG with no scripts; its own policy says so.
    return HTMLResponse(path.read_text(encoding="utf-8"), headers={
        "Cache-Control": "no-cache", "X-Frame-Options": "SAMEORIGIN",
        "Content-Security-Policy": "default-src 'none'; style-src 'unsafe-inline'; img-src data:; "
                                   "font-src data:; frame-ancestors 'self'; base-uri 'none'; form-action 'none'"})


@router.get("/api/reports/{rid}/download")
def reports_download(rid: str, request: Request, p: Principal = Depends(principal), s: Session = Depends(get_db, scope="function")):
    """A report as a self-contained file, for whoever may see it."""
    items.require(s, "report", rid, p.user, access.VIEW, slug=_key(request))
    r = reports_runner()
    path = _call(r.html_path, rid)
    meta = r.meta(rid)
    safe = "".join(ch if ch.isalnum() or ch in "-_ " else "_" for ch in meta.get("title") or "report").strip() or "report"
    return FileResponse(path, media_type="text/html", filename=f"{safe} {rid[:15]}.html")


@router.delete("/api/reports/{rid}")
def reports_delete(rid: str, request: Request, me=Depends(require_user), s: Session = Depends(get_db, scope="function")):
    """Delete a report the caller manages."""
    items.require(s, "report", rid, me, access.MANAGE, slug=_key(request))
    _call(reports_runner().delete, rid)
    items.delete(s, "report", rid, actor=me)
    return {"deleted": rid}
