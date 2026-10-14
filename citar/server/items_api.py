"""Sharing an owned item, and taking one down (design/phase3-features.md P3.2.4, P3.2.3).

The generic routes for every kind of owned item (a benchmark suite or run, a probe or probe run, a lab experiment, a
scenario, a map, a bot profile, a report), addressed as ``/api/items/{kind}/{ref}``, ``ref`` being the item's id on
disk as its own routes name it:

    GET    .../sharing                    who can see it, by what link, who it is shared with, its last unpublish
    PUT    .../sharing                    its visibility on the ladder (`global` needs `publish_global`)
    POST   .../sharing/users              share it with an account (view, play or manage)
    DELETE .../sharing/users/{handle}     stop sharing it with one
    POST   .../sharing/rotate             a new share link; every copy of the old one stops working
    POST   .../unpublish                  take it down: private, a new link, the reason in the audit log

Every route needs MANAGE on the item (its owner, an ACL manager, an administrator), except unpublish, which a
moderator may also do to an item published to the internet or a link they were given (MODERATE). The game sharing
routes (``app.py``) are the same thing for games; the web client's sharing dialog (package 3-03) uses both.
"""
from __future__ import annotations

from typing import Optional

from fastapi import APIRouter, Depends, HTTPException, Request
from pydantic import BaseModel
from sqlalchemy.orm import Session

from .. import items
from ..auth import access, accounts
from ..auth.deps import get_db, require_user

router = APIRouter()


def _key(request: Request) -> Optional[str]:
    """The share key a request carries, if any."""
    return (request.query_params.get("k") or request.query_params.get("key")
            or request.headers.get("x-citar-share-key"))


@router.get("/api/items/{kind}/{ref}/sharing")
def get_sharing(kind: str, ref: str, request: Request, me=Depends(require_user), s: Session = Depends(get_db, scope="function")):
    """An item's sharing, for whoever manages it."""
    row, perms = items.require(s, kind, ref, me, access.MANAGE, slug=_key(request))
    return items.client(s, row, perms, me)


class VisibilityBody(BaseModel):
    """An item's new visibility."""
    visibility: str


@router.put("/api/items/{kind}/{ref}/sharing")
def set_sharing(kind: str, ref: str, body: VisibilityBody, request: Request, me=Depends(require_user),
                s: Session = Depends(get_db, scope="function")):
    """Change who can see an item (``access.set_visibility``: on the ladder, `global` needs `publish_global`)."""
    row, perms = items.require(s, kind, ref, me, access.MANAGE, slug=_key(request))
    before = row.visibility
    access.set_visibility(s, row, body.visibility, actor=me)
    if row.visibility != before:
        items.visibility_changed(s, row)
    return items.client(s, row, perms, me)


class ShareWithBody(BaseModel):
    """The account to share an item with, and what they may do."""
    handle: str
    permission: str = "view"


@router.post("/api/items/{kind}/{ref}/sharing/users")
def share_with(kind: str, ref: str, body: ShareWithBody, request: Request, me=Depends(require_user),
               s: Session = Depends(get_db, scope="function")):
    """Share an item with one account."""
    row, _perms = items.require(s, kind, ref, me, access.MANAGE, slug=_key(request))
    target = accounts.by_handle(s, body.handle)
    if target is None or target.status in ("deleted", "suspended"):
        raise HTTPException(404, f"No account called '{body.handle}'.")
    access.grant(s, row, target, body.permission, actor=me)
    return {"shared_with": access.shared_with(s, row, me)}


@router.delete("/api/items/{kind}/{ref}/sharing/users/{handle}")
def unshare_with(kind: str, ref: str, handle: str, request: Request, me=Depends(require_user),
                 s: Session = Depends(get_db, scope="function")):
    """Stop sharing an item with an account."""
    row, _perms = items.require(s, kind, ref, me, access.MANAGE, slug=_key(request))
    target = accounts.by_handle(s, handle)
    if target is None:
        raise HTTPException(404, f"No account called '{handle}'.")
    access.revoke(s, row, target, actor=me)
    return {"shared_with": access.shared_with(s, row, me)}


@router.post("/api/items/{kind}/{ref}/sharing/rotate")
def rotate(kind: str, ref: str, request: Request, me=Depends(require_user), s: Session = Depends(get_db, scope="function")):
    """Give an item a new share link; every copy of the old one stops working."""
    row, perms = items.require(s, kind, ref, me, access.MANAGE, slug=_key(request))
    access.rotate_slug(s, row, actor=me)
    return items.client(s, row, perms, me)


class UnpublishBody(BaseModel):
    """Why an item is being taken down: its owner sees it."""
    reason: str = ""


@router.post("/api/items/{kind}/{ref}/unpublish")
def unpublish(kind: str, ref: str, body: UnpublishBody, request: Request, me=Depends(require_user),
              s: Session = Depends(get_db, scope="function")):
    """Take an item down (P3.2.3): it becomes private, its share link is rotated so every copy of it dies, and the
    audit log keeps who did it and why, which its owner sees. For a moderator on an item published to the internet
    (or a link they were given), and for whoever manages it; nothing else of the item is a moderator's to touch."""
    row, perms = items.require(s, kind, ref, me, access.VIEW, slug=_key(request))
    if access.MODERATE not in perms and access.MANAGE not in perms:
        perms.require(access.MODERATE)
    access.unpublish(s, row, actor=me, reason=body.reason)
    items.visibility_changed(s, row)
    # committed before the answer (the request's session would commit only after it is sent): the item's owner, told
    # at once, reads it taken down with the reason, never the moment before
    s.commit()
    return items.client(s, row, access.on(s, me, row), me)
