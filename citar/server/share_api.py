"""Sharing reports and datasets, and reading what has been shared.

Reports get the same visibility ladder as games — private, allowlist, link, public — set
independently of the games they draw on. That separation is the point: an analysis can be published
while the games behind it stay private, which is the normal case for somebody who wants to show a
conclusion without handing over their raw results.

A public report is readable with no account at all. That is the only route in this application that
serves somebody else's content to an anonymous stranger, so it is deliberately narrow: it renders
the stored HTML and nothing else, it is marked `noindex`, and it never includes the underlying
game list unless the owner also shared those.
"""
from __future__ import annotations

import logging
from typing import Optional

from fastapi import APIRouter, Depends, HTTPException, Request
from fastapi.responses import HTMLResponse
from pydantic import BaseModel
from sqlalchemy import select
from sqlalchemy.orm import Session

from .. import aggregate, settings
from ..auth import access, accounts, audit
from ..auth.deps import Principal, get_db, principal, require_user
from ..db.models import Report, User

log = logging.getLogger("citar.share_api")
router = APIRouter()


def _share_key(request: Request) -> Optional[str]:
    """The share key a request carries, if any."""
    return (request.query_params.get("k") or request.query_params.get("key")
            or request.headers.get("x-citar-share-key"))


def _report(s: Session, report_id: str, viewer: Optional[User], permission: str,
            request: Request) -> Report:
    """A report the viewer has a permission on, or 404."""
    row = s.get(Report, report_id)
    if row is None:
        raise HTTPException(404, "No such report.")
    access.on(s, viewer, row, slug=_share_key(request)).require(permission)
    return row


def _to_client(s: Session, row: Report, perms: access.Access) -> dict:
    """A report as the viewer may see it, with their permissions on it."""
    owner = s.get(User, row.owner_id) if row.owner_id else None
    out = {
        "id": row.id, "name": row.name, "visibility": row.visibility, "scope": row.scope,
        "owner": owner.public() if owner else None,
        "created_at": row.created_at.isoformat() if row.created_at else None,
        **perms.client(),
    }
    if access.MANAGE in perms:
        base = settings.get().public_origin
        if row.visibility in ("link", "public"):
            out["share_url"] = f"{base}/#/reports/{row.id}?k={row.slug}"
        if row.visibility == "public":
            out["public_url"] = f"{base}/r/{row.id}"
        out["shared_with"] = access.shared_with(s, row)
    return out


# ---------------------------------------------------------------------------- listing

@router.get("/api/shared/reports")
def list_reports(request: Request, p: Principal = Depends(principal),
                 s: Session = Depends(get_db)):
    """Reports this caller may see: their own, anything shared with them, and public ones."""
    rows = access.visible(s, Report, p.user, order_by=Report.created_at.desc())
    return {"reports": [_to_client(s, r, access.on(s, p.user, r)) for r in rows]}


@router.get("/api/shared/reports/{report_id}")
def get_report(report_id: str, request: Request, p: Principal = Depends(principal),
               s: Session = Depends(get_db)):
    """One shared report, if the viewer may see it."""
    row = _report(s, report_id, p.user, access.VIEW, request)
    return _to_client(s, row, access.on(s, p.user, row, slug=_share_key(request)))


# ---------------------------------------------------------------------------- sharing

class ShareBody(BaseModel):
    """A report's visibility and its public link."""
    visibility: Optional[str] = None
    rotate_slug: bool = False


@router.put("/api/shared/reports/{report_id}/sharing")
def set_sharing(report_id: str, body: ShareBody, request: Request,
                p: Principal = Depends(principal), s: Session = Depends(get_db)):
    """Change a report's visibility."""
    me = require_user(request, p)
    row = _report(s, report_id, me, access.MANAGE, request)
    if body.visibility is not None:
        access.set_visibility(s, row, body.visibility, actor=me)
    if body.rotate_slug:
        access.rotate_slug(s, row, actor=me)
    return _to_client(s, row, access.on(s, me, row))


class ShareWithBody(BaseModel):
    """The account to share a report with."""
    handle: str
    permission: str = "view"


@router.post("/api/shared/reports/{report_id}/users")
def share_with(report_id: str, body: ShareWithBody, request: Request,
               p: Principal = Depends(principal), s: Session = Depends(get_db)):
    """Share a report with one account."""
    me = require_user(request, p)
    row = _report(s, report_id, me, access.MANAGE, request)
    target = accounts.by_handle(s, body.handle)
    if target is None or target.status in ("deleted", "suspended"):
        raise HTTPException(404, f"No account called '{body.handle}'.")
    access.grant(s, row, target, body.permission, actor=me)
    return {"shared_with": access.shared_with(s, row)}


@router.delete("/api/shared/reports/{report_id}/users/{handle}")
def unshare_with(report_id: str, handle: str, request: Request,
                 p: Principal = Depends(principal), s: Session = Depends(get_db)):
    """Stop sharing a report with an account."""
    me = require_user(request, p)
    row = _report(s, report_id, me, access.MANAGE, request)
    target = accounts.by_handle(s, handle)
    if target is None:
        raise HTTPException(404, f"No account called '{handle}'.")
    access.revoke(s, row, target, actor=me)
    return {"shared_with": access.shared_with(s, row)}


# ---------------------------------------------------------------------------- public read

@router.get("/r/{report_id}", response_class=HTMLResponse)
def public_report(report_id: str, request: Request, p: Principal = Depends(principal),
                  s: Session = Depends(get_db)):
    """A published report, readable without an account.

    The only route that serves one person's content to an anonymous stranger, so it does exactly
    one thing: render the stored HTML. No game list, no metadata beyond what the report itself
    contains, and noindex so a shared link does not become a search result.
    """
    from pathlib import Path

    row = s.get(Report, report_id)
    if row is None or row.deleted_at is not None:
        raise HTTPException(404, "No such report.")
    access.on(s, p.user, row, slug=_share_key(request)).require(access.VIEW)

    html = ""
    if row.path:
        candidate = Path(row.path)
        # The stored path is written by the report builder, but it is still a path from the
        # database being turned into a file read — resolve it and refuse anything outside the
        # reports directory rather than trusting the row.
        try:
            from ..server.session import SAVE_DIR
            root = (SAVE_DIR / "reports").resolve()
            resolved = candidate.resolve()
            if root in resolved.parents and resolved.is_file():
                html = resolved.read_text(encoding="utf-8", errors="replace")
            else:
                log.warning("report %s points outside the reports directory: %s", row.id, row.path)
        except Exception:
            log.exception("could not read report %s", row.id)

    if not html:
        html = ("<!doctype html><meta charset='utf-8'><title>Report</title>"
                f"<body style='font-family:sans-serif;padding:40px'><h1>{row.name or 'Report'}</h1>"
                "<p>This report has not been generated yet.</p>")

    return HTMLResponse(html, headers={
        "X-Robots-Tag": "noindex, nofollow",
        "Referrer-Policy": "no-referrer",
        # A report is authored HTML from this server, but it is rendered at an origin that also
        # serves the application, so keep it from reaching back into the API.
        "Content-Security-Policy": "default-src 'self'; script-src 'none'; frame-ancestors 'none'",
    })


# ---------------------------------------------------------------------------- pooling

@router.get("/api/shared/pooling")
def pooling(request: Request, p: Principal = Depends(principal), s: Session = Depends(get_db)):
    """What the pool contains, and what this account contributes to it."""
    me = require_user(request, p)
    summary = aggregate.summarize_sharing(s)
    scope = aggregate.scope_for(s, me)
    return {
        "summary": summary,
        "your_scope": scope.client(),
        "your_setting": me.data_sharing,
        "explanation": (
            "Pooled results feed the server-wide model averages. Private results are excluded from "
            "every aggregate and only you can analyse them. The filter is applied when an average "
            "is calculated, not when a game is recorded, so changing this applies to your whole "
            "history in both directions."),
        "threshold_note": (
            f"A pooled figure is only published once at least {summary['min_contributors']} "
            "different accounts have contributed to it, so an average can never be read back as "
            "one person's results."),
    }


class PoolingBody(BaseModel):
    """Whether this account's results may feed the pooled averages."""
    data_sharing: str


@router.put("/api/shared/pooling")
def set_pooling(body: PoolingBody, request: Request, p: Principal = Depends(principal),
                s: Session = Depends(get_db)):
    """Opt this account's data in or out of pooling.

    Opting out is retroactive: the account's history leaves everybody else's averages too, which is
    what makes it a real choice rather than a setting for the future only.
    """
    me = require_user(request, p)
    if body.data_sharing not in ("pool", "private"):
        raise HTTPException(400, "data_sharing is 'pool' or 'private'.")
    before, me.data_sharing = me.data_sharing, body.data_sharing
    if before != body.data_sharing:
        audit.record(s, "user.data_sharing", actor=me, object_type="user", object_id=me.id,
                     ip=p.ip, **{"from": before, "to": body.data_sharing})
    return {"data_sharing": me.data_sharing,
            "note": ("Your existing games keep their own setting; this is the default for new "
                     "ones. Change a single game from its sharing panel.")}


class ApplyAllBody(BaseModel):
    """A visibility change to apply to everything an account owns."""
    data_sharing: str


@router.post("/api/shared/pooling/apply-to-all")
def apply_to_all(body: ApplyAllBody, request: Request, p: Principal = Depends(principal),
                 s: Session = Depends(get_db)):
    """Set every one of this account's games to the same sharing mode.

    Separate from changing the default on purpose: "from now on" and "everything I have ever done"
    are different intentions, and silently doing the second when somebody asked for the first is
    the kind of surprise that loses trust.
    """
    from ..db.models import Game

    me = require_user(request, p)
    if body.data_sharing not in ("pool", "private"):
        raise HTTPException(400, "data_sharing is 'pool' or 'private'.")
    changed = 0
    for game in s.scalars(select(Game).where(Game.owner_id == me.id, Game.deleted_at.is_(None))):
        if game.data_sharing != body.data_sharing:
            game.data_sharing = body.data_sharing
            changed += 1
    audit.record(s, "user.data_sharing_bulk", actor=me, object_type="user", object_id=me.id,
                 ip=p.ip, to=body.data_sharing, games=changed)
    return {"changed": changed, "data_sharing": body.data_sharing}
