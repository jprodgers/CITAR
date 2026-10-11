"""HTTP API for the Reports page: the report builder, its runs and the generated HTML.

These routes were administrator-only (on ``admin_api``'s router). A report is now an owned item of kind ``report``
(design/phase3-features.md P3.2.4): building one needs ``run_reports`` and it is the caller's; reading it needs VIEW
on it (so a `global` report's page reads with no account at all, as ``/r/`` does); rerunning and deleting need
MANAGE. The report runner writes the row (``ReportRunner.start``). What a member's report *gathers* is still
everything until package 3-03 scopes ``citar/reports/data.py`` to its owner, and the sharing routes for reports are
``items_api``'s generic ones (``share_api``'s report routes give way to them in 3-03).
"""
from __future__ import annotations

from typing import Optional

from fastapi import APIRouter, Depends, HTTPException, Request
from fastapi.responses import FileResponse, HTMLResponse
from sqlalchemy.orm import Session

from .. import items
from ..auth import access
from ..auth.deps import Principal, get_db, principal, require_cap, require_user

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


#: The kind of work a report's narrative is for admission (``admission.PURPOSES``): one prompt on a machine, as a
#: game's turn is. Package 3-09 gives pooled prompts a purpose of their own.
NARRATIVE_PURPOSE = "game"


def _narrative_gate(s: Session, spec: dict, user) -> None:
    """A report's written analysis runs on the machine its (normalized) spec names: a member's names one they may
    use, never the site's own (P3.2.9, ``servers.member_llm``: a registry machine is an administrator's until it is
    shared), and is admitted to it (``pool.seats.admit``: their own machine, or a group's they hold a grant on; a
    machine they cannot see is refused without being named). When it may run is the runner's business."""
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
def reports_list(request: Request, p: Principal = Depends(principal), s: Session = Depends(get_db)):
    """The reports the caller may see (and the one a share key opens), newest first, each with what they may do to
    it."""
    key = _key(request)
    seen = items.listed(s, "report", p.user, slug=key)
    out = [m for m in reports_runner().list() if seen is None or m.get("id") in seen]
    perms = items.permissions_by_ref(s, "report", p.user, [m["id"] for m in out], slug=key)
    return [dict(m, **perms.get(m["id"], {})) for m in out]


@router.get("/api/reports/options")
def reports_options(me=Depends(require_user)):
    """What the report builder can pick from: machines, models, and recent items. A member is offered what their
    report could gather (``reach.Reach``: the activities of the things they may see) and the machines they may see
    (their own and shared ones; of the site's registry, the dry-run ones)."""
    from ..reports import scope_options
    from ..reports.reach import Reach
    return scope_options(Reach.of(me.id))


@router.post("/api/reports")
def reports_start(body: dict, me=Depends(require_cap("run_reports")), s: Session = Depends(get_db)):
    """Start building a report: the caller's."""
    from ..reports import normalize_spec
    spec = _call(normalize_spec, body)
    _narrative_gate(s, spec, me)
    return _call(reports_runner().start, spec, owner_id=me.id, session=s)


@router.get("/api/reports/{rid}")
def reports_get(rid: str, request: Request, p: Principal = Depends(principal), s: Session = Depends(get_db)):
    """One report's metadata and progress, for whoever may see it."""
    _row, perms = items.require(s, "report", rid, p.user, access.VIEW, slug=_key(request))
    r = reports_runner()
    return {"meta": _call(r.meta, rid), "spec": _call(r.spec, rid), **perms.client()}


@router.post("/api/reports/{rid}/rerun")
def reports_rerun(rid: str, request: Request, me=Depends(require_cap("run_reports")), s: Session = Depends(get_db)):
    """Build a fresh report from the same specification, with current data: for whoever manages the report, and
    the new one is theirs."""
    from ..reports import normalize_spec
    items.require(s, "report", rid, me, access.MANAGE, slug=_key(request))
    r = reports_runner()
    spec = _call(normalize_spec, _call(r.spec, rid))
    _narrative_gate(s, spec, me)
    return _call(r.start, spec, owner_id=me.id, session=s)


@router.get("/api/reports/{rid}/html")
def reports_html(rid: str, request: Request, p: Principal = Depends(principal), s: Session = Depends(get_db)):
    """A report's HTML, for viewing in place, for whoever may see it."""
    items.require(s, "report", rid, p.user, access.VIEW, slug=_key(request))
    path = _call(reports_runner().html_path, rid)
    # Shown in a frame on the Reports page, so this page (and only this one) may be framed by the site itself.
    # A report is static HTML and inline SVG with no scripts; its own policy says so.
    return HTMLResponse(path.read_text(encoding="utf-8"), headers={
        "Cache-Control": "no-cache", "X-Frame-Options": "SAMEORIGIN",
        "Content-Security-Policy": "default-src 'none'; style-src 'unsafe-inline'; img-src data: https:; "
                                   "font-src data:; frame-ancestors 'self'; base-uri 'none'; form-action 'none'"})


@router.get("/api/reports/{rid}/download")
def reports_download(rid: str, request: Request, p: Principal = Depends(principal), s: Session = Depends(get_db)):
    """A report as a self-contained file, for whoever may see it."""
    items.require(s, "report", rid, p.user, access.VIEW, slug=_key(request))
    r = reports_runner()
    path = _call(r.html_path, rid)
    meta = r.meta(rid)
    safe = "".join(ch if ch.isalnum() or ch in "-_ " else "_" for ch in meta.get("title") or "report").strip() or "report"
    return FileResponse(path, media_type="text/html", filename=f"{safe} {rid[:15]}.html")


@router.delete("/api/reports/{rid}")
def reports_delete(rid: str, request: Request, me=Depends(require_user), s: Session = Depends(get_db)):
    """Delete a report the caller manages."""
    items.require(s, "report", rid, me, access.MANAGE, slug=_key(request))
    _call(reports_runner().delete, rid)
    items.delete(s, "report", rid, actor=me)
    return {"deleted": rid}
