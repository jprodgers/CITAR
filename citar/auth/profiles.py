"""Profiles, the members' directory and avatars (design/phase3-features.md P3.2.6).

An account shows other people a handle (always), and a display name, an avatar, a bio, up to three links, the things
it owns and, from package 3-10, its compute ratio, each field `private`, `server` (members) or `global` (anyone) as
the account chooses (`users.field_visibility`, defaults in `models.USER_FIELD_DEFAULTS`). The `profile` field is the
page's own gate: a field shows when the viewer reaches both its level and the profile's. The rule itself lives on
the account (`User.visible_fields`, and `User.public` for a viewer), so everywhere an account appears applies the same one;
this module is what the routes add around it:

- the edits an account makes to its own profile (``update``: the bio's and the links' limits, known levels only,
  nothing `global` on probation);
- the profile page (``require_visible``, the one gate of ``/api/users/{handle}`` and its avatar, and ``to_client``
  with the items the viewer may see), "view as a member" and "view as a visitor" (``rank_for``: a narrower view,
  never a wider one);
- the directory (``directory``: listed accounts in good standing, newest first, a search that matches a display name
  only where the caller may see it, so a hidden name is never revealed by its hits);
- avatars on disk (``store_avatar``, ``avatar_file``, ``remove_replaced``): ``<data dir>/avatars/<user id>-<sha256[:12]>
  .<ext>``, written there and nowhere else, the format and size checked by ``citar.auth.images`` first and the
  metadata (Exif, XMP, text) left out.
"""
from __future__ import annotations

import hashlib
import logging
import os
import re
import secrets
import unicodedata
from pathlib import Path
from typing import Optional
from urllib.parse import urlsplit

from fastapi import HTTPException
from sqlalchemy import and_, func, or_, select

from .. import paths
from ..db.models import (FIELD_LEVELS, USER_FIELD_DEFAULTS, Game as GameRow, OwnedItem, User)
from . import access, audit, images

log = logging.getLogger("citar.profiles")

FIELDS = tuple(USER_FIELD_DEFAULTS)
BIO_MAX = 1000
LINKS_MAX = 3
LABEL_MAX = 40
URL_MAX = 300
#: Directory page size, and items listed per kind on a profile.
PAGE = 50
ITEMS_PER_KIND = 20
#: The furthest a page of accounts may start (the directory's and the admin page's ``offset``): far past any real
#: server, and well inside the 64-bit integer SQLite takes, so a caller's number never reaches it unbounded.
OFFSET_MAX = 1_000_000
#: Who "view as" may ask to be: never wider than the caller already is.
AS_RANK = {"member": 1, "visitor": 0}
#: The refusal for a `global` field on probation (P3.2.6).
PROBATION_GLOBAL = "New accounts can show their profile to members only."

#: Characters a name or a bio may not hold: control characters, and the bidirectional overrides and isolates that
#: make text read differently from what it is.
_BIDI = dict.fromkeys(map(ord, "\u202a\u202b\u202c\u202d\u202e\u2066\u2067\u2068\u2069\u200e\u200f"))


class ProfileError(ValueError):
    """An edit the account may not make; the message says why and is safe to show. ``status`` is the HTTP answer."""

    def __init__(self, message: str, status: int = 400):
        super().__init__(message)
        self.status = status


# ---------------------------------------------------------------------------- what an account may say about itself

def clean_text(text, *, keep_newlines: bool = False) -> str:
    """``text`` with control characters (newlines kept when asked, carriage returns folded into them) and
    bidirectional overrides removed, and the ends trimmed."""
    if not isinstance(text, str):
        raise ProfileError("That is not text.")
    text = text.replace("\r\n", "\n").replace("\r", "\n").translate(_BIDI)
    kept = "".join(ch for ch in text
                   if (keep_newlines and ch == "\n") or unicodedata.category(ch) != "Cc")
    if keep_newlines:
        kept = re.sub(r"\n{3,}", "\n\n", kept)
    return kept.strip()


def clean_bio(text) -> str:
    """A bio: control characters stripped (line breaks kept), at most ``BIO_MAX`` characters."""
    bio = clean_text(text, keep_newlines=True)
    if len(bio) > BIO_MAX:
        raise ProfileError(f"A bio is at most {BIO_MAX:,} characters; that one is {len(bio):,}.")
    return bio


def clean_url(url) -> str:
    """An http or https address of at most ``URL_MAX`` characters, with a host and no account in it."""
    if not isinstance(url, str):
        raise ProfileError("A link's address is text.")
    url = url.strip()
    if not url:
        raise ProfileError("A link needs an address.")
    if len(url) > URL_MAX:
        raise ProfileError(f"A link's address is at most {URL_MAX} characters.")
    if any(unicodedata.category(ch) in ("Cc", "Zs", "Zl", "Zp") for ch in url) or url != url.translate(_BIDI):
        raise ProfileError("A link's address has no spaces or control characters in it.")
    try:
        parts = urlsplit(url)
        port = parts.port            # raises on a port that is not a number
    except ValueError:
        raise ProfileError("That link's address is not a web address.") from None
    if parts.scheme.lower() not in ("http", "https"):
        raise ProfileError("A link starts with http:// or https://.")
    if not parts.hostname:
        raise ProfileError("A link names a site (https://example.com/...).")
    if parts.username is not None or parts.password is not None:
        raise ProfileError("A link may not carry a user name or password.")
    del port
    return url


def clean_links(links) -> list:
    """At most ``LINKS_MAX`` links, each ``{label, url}``: a label of at most ``LABEL_MAX`` characters (it may be
    empty: the page shows the address's site), an address as ``clean_url`` takes it."""
    if not isinstance(links, list):
        raise ProfileError("Links are a list of {label, url}.")
    if len(links) > LINKS_MAX:
        raise ProfileError(f"A profile has at most {LINKS_MAX} links.")
    out = []
    for link in links:
        if not isinstance(link, dict) or set(link) - {"label", "url"}:
            raise ProfileError("Each link is {label, url}.")
        label = clean_text(link.get("label") or "")
        if len(label) > LABEL_MAX:
            raise ProfileError(f"A link's label is at most {LABEL_MAX} characters.")
        out.append({"label": label, "url": clean_url(link.get("url"))})
    return out


def clean_levels(user: User, value) -> dict:
    """The account's field levels with ``value`` (some of ``FIELDS``, each one of ``FIELD_LEVELS``) applied over its
    current choices. Making a field `global` is refused while the account is on probation (403); a field it made
    `global` before it was put on probation may stay so (it shows to members only meanwhile, ``User.field_levels``),
    so that the Account page, which sends every field, still saves."""
    if not isinstance(value, dict):
        raise ProfileError("field_visibility is an object of {field: level}.")
    unknown = sorted(set(value) - set(FIELDS))
    if unknown:
        raise ProfileError(f"Unknown profile field {unknown[0]!r}; the fields are {', '.join(FIELDS)}.")
    for field, level in value.items():
        if level not in FIELD_LEVELS:
            raise ProfileError(f"A field is {', '.join(FIELD_LEVELS)}, not {level!r} ({field}).")
    if user.status == "probation":
        was = user.chosen_levels()
        if any(level == "global" and was[field] != "global" for field, level in value.items()):
            raise ProfileError(PROBATION_GLOBAL, status=403)
    current = user.field_visibility if isinstance(user.field_visibility, dict) else {}
    merged = {f: v for f, v in current.items() if f in FIELDS and v in FIELD_LEVELS}
    merged.update(value)
    return merged


def update(session, user: User, *, display_name=None, bio=None, links=None, field_visibility=None, listed=None,
           ip: Optional[str] = None) -> list:
    """Apply the account's own edits to its profile, all checked before any is made (``ProfileError``). Returns the
    fields that changed; one audit row names them (the levels with their old and new values, the text never)."""
    changes: dict = {}
    if display_name is not None:
        name = clean_text(display_name)[:80]
        if name != (user.display_name or ""):
            changes["display_name"] = name
    if bio is not None:
        text = clean_bio(bio)
        if text != (user.bio or ""):
            changes["bio"] = text
    if links is not None:
        cleaned = clean_links(links)
        if cleaned != list(user.links or []):
            changes["links"] = cleaned
    if field_visibility is not None:
        levels = clean_levels(user, field_visibility)
        if levels != (user.field_visibility or {}):
            changes["field_visibility"] = levels
    if listed is not None:
        if not isinstance(listed, bool):
            raise ProfileError("listed is true or false.")
        if listed != bool(user.listed):
            changes["listed"] = listed
    if not changes:
        return []
    before = user.chosen_levels()
    for field, value in changes.items():
        setattr(user, field, value)
    detail = {"fields": sorted(changes)}
    if "field_visibility" in changes:
        after = user.chosen_levels()
        detail["levels"] = {f: {"from": before[f], "to": after[f]} for f in FIELDS if before[f] != after[f]}
    if "listed" in changes:
        detail["listed"] = changes["listed"]
    audit.record(session, "user.profile_updated", actor=user, object_type="user", object_id=user.id, ip=ip, **detail)
    return sorted(changes)


# ---------------------------------------------------------------------------- the profile page

def rank_for(user: User, viewer: Optional[User], as_: Optional[str] = None) -> int:
    """The viewer's rank on ``user`` (``User.viewer_rank``), narrowed to "a member" or "a visitor" when ``as_`` asks
    for one: how the account's own "view as" links see its page. It never widens: a visitor asking to see as a
    member is still a visitor."""
    rank = user.viewer_rank(viewer)
    if as_:
        if as_ not in AS_RANK:
            raise HTTPException(400, f"View as is {' or '.join(AS_RANK)}.")
        rank = min(rank, AS_RANK[as_])
    return rank


def require_visible(session, handle: str, viewer: Optional[User], field: str = "profile",
                    rank: Optional[int] = None) -> User:
    """The account called ``handle`` when its ``field`` shows to ``viewer`` (``profile``: the page itself), else
    404, the same for an account that does not exist, one whose profile is hidden, and one not in good standing:
    the answer never says which. ``rank`` narrows the viewer (``rank_for``)."""
    from . import accounts
    user = accounts.by_handle(session, handle) if isinstance(handle, str) else None
    if user is None or not user.shows(field, viewer, rank=rank):
        raise HTTPException(404, "No such profile.")
    return user


def _items_filter(model, viewer: Optional[User], rank: int, actual: int):
    """Which rows of ``model`` a profile lists to ``viewer``: what the viewer may list (``access.visible_filter``),
    or, for a "view as" narrower than the viewer is, what any member (`server` and `global`) or any visitor
    (`global`) may."""
    if rank == actual:
        return access.visible_filter(model, viewer)
    levels = (access.GLOBAL,) if rank == 0 else (access.GLOBAL, access.SERVER)
    return and_(model.visibility.in_(levels), model.deleted_at.is_(None))


def items_of(session, user: User, viewer: Optional[User], rank: int) -> list:
    """What ``user`` owns that ``viewer`` may see, newest first, at most ``ITEMS_PER_KIND`` of each kind: lobby games
    (benchmark and probe games belong to their runs, which are items) and every kind of owned item."""
    actual = user.viewer_rank(viewer)
    out = []
    games = session.scalars(select(GameRow).where(GameRow.owner_id == user.id, GameRow.kind == "game",
                                                   _items_filter(GameRow, viewer, rank, actual))
                            .order_by(GameRow.created_at.desc(), GameRow.id).limit(ITEMS_PER_KIND))
    for g in games:
        out.append({"kind": "game", "id": g.id, "name": g.name or g.id, "status": g.status,
                    "visibility": g.visibility, "url": access.page_path(g),
                    "created_at": g.created_at.isoformat() if g.created_at else None})
    for kind in sorted({k for (k,) in session.execute(select(OwnedItem.kind).where(
            OwnedItem.owner_id == user.id, OwnedItem.deleted_at.is_(None)).distinct())}):
        rows = session.scalars(select(OwnedItem).where(OwnedItem.owner_id == user.id, OwnedItem.kind == kind,
                                                       _items_filter(OwnedItem, viewer, rank, actual))
                               .order_by(OwnedItem.created_at.desc(), OwnedItem.id).limit(ITEMS_PER_KIND))
        for row in rows:
            out.append({"kind": row.kind, "id": row.ref, "name": row.name or row.ref, "visibility": row.visibility,
                        "url": access.page_path(row),
                        "created_at": row.created_at.isoformat() if row.created_at else None})
    return out


#: How a page names the viewer it was built for.
_SEEN_AS = {0: "visitor", 1: "member"}


def to_client(session, user: User, viewer: Optional[User], rank: int) -> dict:
    """The profile as ``viewer`` (at ``rank``) sees it: the fields that show and nothing of the rest, the items it
    may see when `items` shows. Package 3-10 adds the ratio here when `ratio` shows."""
    shown = user.visible_fields(viewer, rank=rank)
    you = viewer is not None and viewer.id == user.id
    out = {**user.public(viewer, rank=rank), "fields": sorted(shown), "is_you": you,
           "seen_as": _SEEN_AS.get(rank, "you" if you else "administrator")}
    if "bio" in shown:
        out["bio"] = user.bio or ""
    if "links" in shown:
        out["links"] = list(user.links or [])
    if "items" in shown:
        out["items"] = items_of(session, user, viewer, rank)
    if you or (viewer is not None and viewer.role == "admin" and rank == 2):
        out["levels"] = user.chosen_levels()
        out["listed"] = bool(user.listed)
    return out


# ---------------------------------------------------------------------------- the directory

def _like(text: str) -> str:
    """A LIKE pattern matching ``text`` anywhere, its wildcards escaped."""
    return "%" + text.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_") + "%"


def _level(field: str):
    """A field's chosen level, in SQL (absent: its default), for the directory's search."""
    return func.coalesce(User.field_visibility[field].as_string(), USER_FIELD_DEFAULTS[field])


def directory(session, viewer: User, q: str = "", offset: int = 0) -> dict:
    """The members' directory (``GET /api/users``): accounts that are ``listed`` and in good standing (probation or
    active), newest first, ``PAGE`` at a time from ``offset`` (held to 0..``OFFSET_MAX``), each with what shows to
    ``viewer`` (``User.public`` for the viewer, the bio's first line when the bio shows, and whether its page opens).
    ``q`` matches a handle, and a display name only where the caller may see it: the account's own, an
    administrator's view, or a member's of an account whose profile and display name are `server` or `global`, so
    searching for a hidden name finds nothing."""
    stmt = select(User).where(User.listed.is_(True), User.status.in_(("probation", "active")))
    q = clean_text(q or "")[:80].lower()
    if q:
        pattern = _like(q)
        by_handle = User.handle_lower.like(pattern, escape="\\")
        by_name = func.lower(User.display_name).like(pattern, escape="\\")
        if viewer.role != "admin":
            wide = ("server", "global")
            by_name = and_(by_name, or_(User.id == viewer.id,
                                        and_(_level("profile").in_(wide), _level("display_name").in_(wide))))
        stmt = stmt.where(or_(by_handle, by_name))
    offset = min(max(0, int(offset or 0)), OFFSET_MAX)
    rows = list(session.scalars(stmt.order_by(User.created_at.desc(), User.id).offset(offset).limit(PAGE + 1)))
    out = []
    for user in rows[:PAGE]:
        rank = user.viewer_rank(viewer)
        shown = user.visible_fields(viewer, rank=rank)
        entry = user.public(viewer, rank=rank)
        entry["profile"] = "profile" in shown
        if "bio" in shown and user.bio:
            entry["bio"] = user.bio.strip().split("\n", 1)[0][:200]
        out.append(entry)
    return {"users": out, "offset": offset, "next": offset + PAGE if len(rows) > PAGE else None}


# ---------------------------------------------------------------------------- avatars

def avatars_dir() -> Path:
    """``<data dir>/avatars``, created: the only place an avatar is written."""
    return paths.ensure(paths.data_dir() / "avatars")


def avatar_file(user: User) -> Optional[Path]:
    """The account's uploaded avatar on disk, or None: a stored path that names anything but a file of the avatars
    folder with an avatar's extension is none."""
    if not user.avatar_path:
        return None
    folder = avatars_dir().resolve()
    path = (paths.data_dir() / user.avatar_path).resolve()
    if path.parent != folder or path.suffix.lstrip(".") not in images.MIME_BY_EXT or not path.is_file():
        return None
    return path


def store_avatar(user: User, data: bytes) -> images.ImageInfo:
    """Check ``data`` (``images.inspect``, which raises ``ImageError`` with the rule), write it without its metadata
    (``ImageInfo.data``: no Exif, XMP, text or comments) as the account's avatar and point the account at it. The file
    goes to the avatars folder under a temporary name of its own first and is renamed into place, so a reader never
    sees half of it. The file it replaced stays until the change is committed (``remove_replaced``)."""
    info = images.inspect(data)
    data = info.data
    digest = hashlib.sha256(data).hexdigest()[:12]
    folder = avatars_dir()
    name = f"{user.id}-{digest}.{info.ext}"
    tmp = folder / f".{name}.{secrets.token_hex(4)}.part"
    try:
        tmp.write_bytes(data)
        os.replace(tmp, folder / name)
    finally:
        if tmp.exists():
            try:
                tmp.unlink()
            except OSError:
                pass
    user.avatar_path = f"avatars/{name}"
    return info


def remove_replaced(previous: Optional[str], current: Optional[str]) -> bool:
    """Remove the avatar file ``previous`` (a stored ``avatar_path``) once a committed change replaced it with
    ``current``: only that file, so a second upload of the same account's, landing at the same moment, never loses
    the file its row points at (the worst such a race leaves is an unused file). A path outside the avatars folder
    is never touched; a file another process holds is logged and left. Returns whether a file went."""
    if not previous or previous == current:
        return False
    folder = avatars_dir().resolve()
    path = (paths.data_dir() / previous).resolve()
    if path.parent != folder or not path.is_file():
        return False
    try:
        path.unlink()
        return True
    except OSError:
        log.warning("could not remove the replaced avatar %s", path, exc_info=True)
        return False
