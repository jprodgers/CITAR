"""Profiles, the members' directory and avatars over HTTP (design/phase3-features.md P3.2.6).

    GET    /api/users                   the directory: members only; listed accounts in good standing, newest first,
                                        ?q= on the handle (and the display name where the caller may see it),
                                        ?offset= by 50
    GET    /api/users/{handle}          a profile, for anyone its `profile` field shows to (?as=member|visitor
                                        narrows the view, for the account's own "view as" links); else 404
    GET    /api/users/{handle}/avatar   the uploaded avatar, for anyone its `avatar` field shows to; else 404
    POST   /api/auth/me/avatar          upload one's avatar (multipart, one file, at most 256 KB; PNG, JPEG or WebP
                                        of at most 1024x1024, checked by its header)
    DELETE /api/auth/me/avatar          remove it

The profile and its avatar are open to visitors when the account made them `global`, so neither route takes an
account: each resolves the account through ``profiles.require_visible``, which answers 404 alike for an account that
does not exist, one whose field is hidden from the caller and one not in good standing.
"""
from __future__ import annotations

import logging
from typing import Optional

from fastapi import APIRouter, Depends, HTTPException, Query, Request, Response
from fastapi.concurrency import run_in_threadpool
from sqlalchemy.orm import Session

from ..auth import audit, images, profiles
from ..auth.deps import Principal, get_db, principal, require_user

log = logging.getLogger("citar.users_api")
router = APIRouter()

#: How long a browser may keep an avatar. Its address carries the file's hash, so a new upload is fetched at once;
#: the lifetime only bounds how long a picture made private stays in a browser that already had it.
AVATAR_MAX_AGE = 3600


@router.get("/api/users")
def directory(q: str = "", offset: int = 0, me=Depends(require_user), s: Session = Depends(get_db)):
    """The members' directory (``profiles.directory``)."""
    return profiles.directory(s, me, q=q, offset=offset)


@router.get("/api/users/{handle}")
def profile(handle: str, as_: Optional[str] = Query(None, alias="as"), p: Principal = Depends(principal),
            s: Session = Depends(get_db)):
    """A profile as the caller may see it (``profiles.to_client``), or 404 when its `profile` field does not show to
    them. ``?as=member`` or ``?as=visitor`` shows it as a member or a visitor would see it, never wider than the
    caller may."""
    user = profiles.require_visible(s, handle, p.user)
    rank = profiles.rank_for(user, p.user, as_)
    if rank != user.viewer_rank(p.user):
        # "view as" a narrower viewer: the page itself must show to them too
        profiles.require_visible(s, handle, p.user, rank=rank)
    return profiles.to_client(s, user, p.user, rank)


@router.get("/api/users/{handle}/avatar")
def avatar(handle: str, p: Principal = Depends(principal), s: Session = Depends(get_db)):
    """The account's uploaded avatar, when its `avatar` field shows to the caller; 404 otherwise, and for an account
    with no upload (its sign-in picture, if any, is its provider's address, not this route). Served with the type
    its header proved, never sniffed, and a policy that lets it load and run nothing."""
    user = profiles.require_visible(s, handle, p.user, field="avatar")
    path = profiles.avatar_file(user)
    if path is None:
        raise HTTPException(404, "No such profile.")
    mime = images.MIME_BY_EXT[path.suffix.lstrip(".")]
    public = "global" == user.field_levels()["avatar"] == user.field_levels()["profile"]
    return Response(path.read_bytes(), media_type=mime, headers={
        "X-Content-Type-Options": "nosniff",
        "Content-Security-Policy": "default-src 'none'; sandbox",
        "Content-Disposition": "inline",
        "Cache-Control": f"{'public' if public else 'private'}, max-age={AVATAR_MAX_AGE}",
    })


@router.post("/api/auth/me/avatar")
async def upload_avatar(request: Request, me=Depends(require_user), s: Session = Depends(get_db)):
    """Upload the caller's avatar: a multipart form with one file (any field name), at most ``images.MAX_BYTES``. The
    body is read only up to the limit, so an oversized upload costs the server no more than that; the file is
    checked (``images.inspect``) before anything is written, and written without its metadata (Exif, XMP, text,
    comments), only under ``<data dir>/avatars/``. A body that is not a well-formed form is a 400. The previous file
    is removed once the change is committed."""
    size = request.headers.get("content-length")
    # a multipart body carries its boundaries and part headers around the file: allow them a little room
    limit = images.MAX_BYTES + 16 * 1024
    if size is not None and size.isdigit() and int(size) > limit:
        raise HTTPException(413, f"That file is too large. {images.RULES}")
    if not (request.headers.get("content-type") or "").lower().startswith("multipart/form-data"):
        raise HTTPException(400, f"Send the picture as a multipart form with one file. {images.RULES}")
    body = bytearray()
    async for chunk in request.stream():
        body += chunk
        if len(body) > limit:
            raise HTTPException(413, f"That file is too large. {images.RULES}")
    data = await _the_file(request, bytes(body))
    # the check, the write and the commit block (a disk, SQLite's lock): off the event loop
    return await run_in_threadpool(_save_avatar, s, me, data)


def _save_avatar(s: Session, me, data: bytes) -> dict:
    """Check and store an upload as the caller's avatar, commit, then remove the file it replaced."""
    previous = me.avatar_path
    try:
        info = profiles.store_avatar(me, data)
    except images.ImageError as exc:
        raise HTTPException(400, str(exc))
    audit.record(s, "user.avatar_set", actor=me, object_type="user", object_id=me.id,
                 format=info.format, width=info.width, height=info.height, bytes=len(info.data))
    s.commit()
    profiles.remove_replaced(previous, me.avatar_path)
    return {"ok": True, "user": me.account()}


@router.delete("/api/auth/me/avatar")
def delete_avatar(request: Request, me=Depends(require_user), s: Session = Depends(get_db)):
    """Remove the caller's uploaded avatar (the sign-in provider's picture, if any, shows again)."""
    previous = me.avatar_path
    me.avatar_path = None
    if previous:
        audit.record(s, "user.avatar_removed", actor=me, object_type="user", object_id=me.id)
    s.commit()
    profiles.remove_replaced(previous, None)
    return {"ok": True, "user": me.account()}


async def _the_file(request: Request, body: bytes) -> bytes:
    """The one file of a multipart form ``body`` (already read, and capped), as sent; 400 for a body that is not such
    a form. Parsed by Starlette's own form parser, which keeps a file this small in memory (``spool_max_size`` is
    1 MiB), so nothing of the upload touches the disk before it is checked. Starlette raises its own
    ``MultiPartException`` for what it checks (no boundary, too many parts); the parser underneath raises a
    ``ValueError`` (python_multipart's ``FormParserError``) for a body that does not parse, such as one whose boundary
    is not the header's or bytes that are no form, and closes what it opened before either reaches here."""
    from starlette.datastructures import UploadFile
    from starlette.formparsers import MultiPartException, MultiPartParser

    async def once():
        yield body

    try:
        form = await MultiPartParser(request.headers, once(), max_files=1, max_fields=8,
                                     max_part_size=images.MAX_BYTES + 1024).parse()
    except MultiPartException as exc:
        raise HTTPException(400, f"That upload could not be read ({exc.message}). {images.RULES}") from None
    except ValueError:
        raise HTTPException(400, f"That upload could not be read (it is not a well-formed form). {images.RULES}") \
            from None
    try:
        files = [v for _k, v in form.multi_items() if isinstance(v, UploadFile)]
        if len(files) != 1:
            raise HTTPException(400, f"Send exactly one file. {images.RULES}")
        return await files[0].read()
    finally:
        await form.close()
