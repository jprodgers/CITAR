"""A published report's own page, and the pooling settings.

A report is an owned item of kind `report` (`owned_items`, design/phase3-features.md P3.2.4): its routes are
``reports_api``'s and its sharing is the generic item routes' (``items_api``: ``/api/items/report/{ref}/sharing``),
which the web client's sharing dialog uses for every kind. What stays here is the one page that shows a report to
somebody with no account, and the account's pooling choice.

``/r/{slug}`` is a report published to the internet (`global`), by its id or its share key, or one shared by link
(`link`), by its share key. That is the only route in this application that serves somebody else's content to an
anonymous stranger, so it is deliberately narrow: it renders the stored HTML and nothing else, it is marked
`noindex`, and it answers 404 for everything else, the report's own owner included (who reads it in the app).

Python lines this replaces: the ``/api/shared/reports*`` routes and ``/r/{report_id}`` of ``share_api.py`` at
``3069d25`` (the report sharing routes, now ``items_api``'s).
"""
from __future__ import annotations

import html as _html
import logging

from fastapi import APIRouter, Depends, HTTPException, Request
from fastapi.responses import HTMLResponse
from pydantic import BaseModel
from sqlalchemy import or_, select
from sqlalchemy.orm import Session

from .. import aggregate, items
from ..auth import access, audit
from ..auth.deps import Principal, get_db, principal, require_user
from ..db.models import OwnedItem

log = logging.getLogger("citar.share_api")
router = APIRouter()

REPORT = "report"


def public_report_row(s: Session, key: str):
    """The live report ``/r/{slug}`` serves, or None: a `link` or `global` report whose share key is ``key``, or a
    `global` report whose id is ``key``. The key is compared by the database's index, as every slug is."""
    if not key or len(key) > 120:
        return None
    return s.scalar(select(OwnedItem).where(
        OwnedItem.kind == REPORT, OwnedItem.deleted_at.is_(None),
        or_((OwnedItem.slug == key) & OwnedItem.visibility.in_((access.LINK, access.GLOBAL)),
            (OwnedItem.ref == key) & (OwnedItem.visibility == access.GLOBAL))).limit(1))


@router.get("/r/{slug}", response_class=HTMLResponse)
def public_report(slug: str, s: Session = Depends(get_db)):
    """A published report, readable without an account: a `global` one by its id or share key, a `link` one by its
    share key (P3.2.4); 404 for anything else.

    The only route that serves one person's content to an anonymous stranger, so it does exactly
    one thing: render the stored HTML. No game list, no metadata beyond what the report itself
    contains, and noindex so a shared link does not become a search result.
    """
    row = public_report_row(s, slug)
    if row is None:
        raise HTTPException(404, "No such report.")

    html = ""
    candidate = items.file_path(row)       # the row keeps it relative to the state directory
    if candidate is not None:
        # The stored path is written by the report builder, but it is still a path from the
        # database being turned into a file read — resolve it and refuse anything outside the
        # reports directory rather than trusting the row.
        try:
            from .reports_api import reports_runner
            root = reports_runner().dir.resolve()
            resolved = candidate.resolve()
            if root in resolved.parents and resolved.is_file():
                html = resolved.read_text(encoding="utf-8", errors="replace")
            else:
                log.warning("report %s points outside the reports directory: %s", row.ref, row.path)
        except Exception:
            log.exception("could not read report %s", row.ref)

    if not html:
        html = ("<!doctype html><meta charset='utf-8'><title>Report</title>"
                f"<body style='font-family:sans-serif;padding:40px'><h1>{_html.escape(row.name or 'Report')}</h1>"
                "<p>This report has not been generated yet.</p>")

    return HTMLResponse(html, headers={
        "X-Robots-Tag": "noindex, nofollow",
        "Referrer-Policy": "no-referrer",
        # A report is authored HTML from this server, but it is rendered at an origin that also
        # serves the application, so keep it from reaching back into the API.
        "Content-Security-Policy": "default-src 'none'; style-src 'unsafe-inline'; img-src data:; "
                                   "font-src data:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
        "Cache-Control": "no-cache",
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
    what makes it a real choice rather than a setting for the future only (``aggregate.scope_for`` reads
    the account's choice when an average is made). Opting in again pools the games whose own setting
    allows it; it is also the default a new game takes.
    """
    me = require_user(request, p)
    if body.data_sharing not in ("pool", "private"):
        raise HTTPException(400, "data_sharing is 'pool' or 'private'.")
    before, me.data_sharing = me.data_sharing, body.data_sharing
    if before != body.data_sharing:
        audit.record(s, "user.data_sharing", actor=me, object_type="user", object_id=me.id,
                     ip=p.ip, **{"from": before, "to": body.data_sharing})
    return {"data_sharing": me.data_sharing,
            "note": ("Opted out, none of your games feeds anybody else's averages, the ones you played "
                     "before included; you still see your own figures. Opted in, each game follows its own "
                     "setting (new games take this one; change a single game from its sharing panel).")}


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
