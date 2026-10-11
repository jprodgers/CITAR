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


def _narrative_gate(spec: dict, user) -> None:
    """A report's written analysis runs on a machine the spec names: a member's names one they may use, never
    the site's own (P3.2.9, ``servers.member_llm``: a registry machine is an administrator's until it is shared).
    Who may use a pooled machine for pooled prompts is admission's, package 3-09's."""
    from .. import servers
    narrative = (spec or {}).get("narrative") or {}
    if not isinstance(narrative, dict) or not narrative.get("enabled") or _is_admin(user):
        return
    try:
        servers.member_llm({"server_id": narrative.get("server_id")}, user, required=True)
    except servers.SeatBlockError as e:
        raise HTTPException(e.status, str(e)) from None


@router.get("/api/reports")
def reports_list(p: Principal = Depends(principal), s: Session = Depends(get_db)):
    """The reports the caller may see, newest first, each with what they may do to it."""
    seen = items.listed(s, "report", p.user)
    out = [m for m in reports_runner().list() if seen is None or m.get("id") in seen]
    perms = items.permissions_by_ref(s, "report", p.user, [m["id"] for m in out])
    return [dict(m, **perms.get(m["id"], {})) for m in out]


@router.get("/api/reports/options")
def reports_options(me=Depends(require_user), s: Session = Depends(get_db)):
    """What the report builder can pick from: machines, models, and recent items. A member is offered the items
    they may see and the machines they may use (their own and shared ones; of the site's registry, the dry-run
    ones)."""
    from ..reports import scope_options
    opts = scope_options()
    if _is_admin(me):
        return opts
    from .. import servers
    from ..db.models import Server
    from . import ownership
    games = ownership.visible_ids(s, me) or set()
    kinds = {"benchmark_run": "benchmark_run", "probe": "probe", "probe_run": "probe_run",
             "lab_experiment": "lab_experiment"}
    seen = {kind: items.visible_refs(s, kind, me) for kind in kinds}
    opts["items"] = [i for i in opts.get("items", [])
                     if (i.get("kind") == "game" and i.get("id") in games)
                     or (i.get("kind") in seen and i.get("id") in seen[i["kind"]])]
    machines = []
    for sv in opts.get("servers", []):
        row = s.get(Server, sv["id"])
        if row is not None:
            if access.VIEW in access.on(s, me, row):
                machines.append(sv)
        elif ((servers.find(sv["id"]) or {}).get("connection") or {}).get("provider") == "dryrun":
            machines.append(sv)
    opts["servers"] = machines
    return opts


@router.post("/api/reports")
def reports_start(body: dict, me=Depends(require_cap("run_reports")), s: Session = Depends(get_db)):
    """Start building a report: the caller's."""
    _narrative_gate(body, me)
    return _call(reports_runner().start, body, owner_id=me.id, session=s)


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
    items.require(s, "report", rid, me, access.MANAGE, slug=_key(request))
    r = reports_runner()
    spec = _call(r.spec, rid)
    _narrative_gate(spec, me)
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
