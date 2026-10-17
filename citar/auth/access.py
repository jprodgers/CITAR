"""One place that answers: what may this viewer do with this object?

Every route asks this and nothing hand-rolls a check. That is the whole design. Authorization bugs
come overwhelmingly from the fourteenth place that decides who may read something, written months
after the first thirteen and subtly different; a single function that every caller is forced through
cannot drift from itself.

    perms = access.on(s, viewer, game)
    perms.require("play")          # raises HTTPException(403) with a reason worth reading

The permission ladder
---------------------
    view      see it, watch it, read its results
    play      take a seat, act in it
    manage    rename, reconfigure, change sharing, delete
    moderate  unpublish it: set it private, rotate its link, with a reason the owner sees
    admin     the above plus anything reserved to administrators

Each implies the ones before it for the object's owner, but not in general: a spectator link grants
`view` and nothing else, and an ACL entry grants exactly what it says. `moderate` stands apart: it is a
moderator's only extra, and it is not `manage`.

Visibility (design/phase3-features.md P3.2.3)
----------
    private    owner (and admins) only
    allowlist  owner plus explicit ACL entries
    link       anyone holding the unguessable slug — never listed, never indexed
    server     every signed-in member in good standing (probation or active), no slug needed; listed for members
    global     anyone on the internet, listed; making something global needs `publish_global`

ACL entries count at every level, so sharing with one person never needs a wider level. `VISIBILITY` is
the tuple and `check_visibility` the one validator: every place that sets or compares a visibility uses
them (tests/test_access.py scans the code for a stray string).

Two rules that are easy to get wrong and are therefore stated once here, in code, rather than
repeated per route:

*Watching and playing are separate.* A game's `visibility` governs who may watch. Playing is the
seat holders plus explicit `play` grants, and is never implied by a public spectator link. So "open
to the internet if the link is good" does not mean strangers can take your cities.

*Moderators are not universal readers, and they do not manage.* A moderator can take down what has
been published to the internet — a `global` object, or a `link` object whose link they were given —
and nothing else: `MODERATE` allows unpublishing, while every control (stop, delete, sharing, seats,
pause) needs `MANAGE`, which only the owner, an ACL manager and administrators have. A moderator has no
route to somebody's private games, and on a `server` object nothing a member lacks. Giving moderators
blanket read access is how a moderation tool turns into a surveillance tool.
"""
from __future__ import annotations

import logging
from typing import Optional, Set

from fastapi import HTTPException
from sqlalchemy import or_, select

from ..db import models
from ..db.models import AclEntry, Game, OwnedItem, Server, ServerGroup, User

log = logging.getLogger("citar.access")

VIEW, PLAY, MANAGE, MODERATE, ADMIN = "view", "play", "manage", "moderate", "admin"
ALL = (VIEW, PLAY, MANAGE, MODERATE, ADMIN)

#: Which model class maps to which object_type string in acl_entries. An owned item's is its row's
#: `kind` (`object_type_of`).
OBJECT_TYPES = {Game: "game", ServerGroup: "server_group", Server: "server"}

#: The visibility ladder, narrowest first (`models.VISIBILITY`, defined beside the tables).
VISIBILITY = models.VISIBILITY
PRIVATE, ALLOWLIST, LINK, SERVER, GLOBAL = VISIBILITY

#: Account statuses that see `server` objects: signed in and in good standing.
MEMBER_STATUSES = ("probation", "active")


class BadVisibility(HTTPException, ValueError):
    """A visibility that is not on the ladder: a 400 to an HTTP caller, a ValueError to anyone else."""

    def __init__(self, value):
        HTTPException.__init__(self, 400, f"Visibility is one of {', '.join(VISIBILITY)}, not {value!r}.")
        ValueError.__init__(self, self.detail)


def check_visibility(value) -> str:
    """``value`` when it is on the ladder; raises ``BadVisibility`` otherwise (``public`` included: it
    became ``global``)."""
    if value not in VISIBILITY:
        raise BadVisibility(value)
    return value


def object_type_of(obj) -> str:
    """The object_type an object's ACL entries carry: a game, a server group, a server, or an owned
    item's kind."""
    if isinstance(obj, OwnedItem):
        return obj.kind
    object_type = OBJECT_TYPES.get(type(obj))
    if object_type is None:
        raise TypeError(f"access does not know about {type(obj).__name__}.")
    return object_type


def is_member(viewer: Optional[User]) -> bool:
    """Whether a viewer is a signed-in member in good standing, who sees `server` objects."""
    return viewer is not None and viewer.status in MEMBER_STATUSES


class Access:
    """The answer, plus enough context to explain a refusal."""

    __slots__ = ("permissions", "object_type", "object_id", "viewer_id", "visibility", "reason")

    def __init__(self, permissions: Set[str], *, object_type: str = "", object_id: str = "",
                 viewer_id: Optional[str] = None, visibility: str = PRIVATE, reason: str = ""):
        self.permissions = permissions
        self.object_type = object_type
        self.object_id = object_id
        self.viewer_id = viewer_id
        self.visibility = visibility
        self.reason = reason

    def __contains__(self, permission: str) -> bool:
        return permission in self.permissions

    def __bool__(self) -> bool:
        return bool(self.permissions)

    def can(self, permission: str) -> bool:
        """Whether this viewer may perform a named action on the object."""
        return permission in self.permissions

    def require(self, permission: str) -> "Access":
        """Raise unless the viewer has it. 404 rather than 403 when they cannot even see it.

        Answering 403 for something a stranger has no `view` on confirms the thing exists, which
        turns any id-guessing loop into a way to enumerate other people's private games.
        """
        if permission in self.permissions:
            return self
        noun = (self.object_type or "item").replace("_", " ")
        if VIEW not in self.permissions:
            raise HTTPException(404, f"No such {noun}.")
        raise HTTPException(403, self.reason or _refusal(noun, permission, self.viewer_id))

    def client(self) -> dict:
        """What the browser needs to decide which controls to show.

        Deliberately does not include `visibility`. This object is a snapshot taken when the
        permission check ran; if a caller then *changes* the visibility and merges this dict over
        the row, the stale value silently wins and the response contradicts the database. The row
        is the only authority on its own visibility.
        """
        return {"can_view": VIEW in self.permissions, "can_play": PLAY in self.permissions,
                "can_manage": MANAGE in self.permissions,
                "can_moderate": MODERATE in self.permissions or MANAGE in self.permissions}


def _refusal(noun: str, permission: str, viewer_id: Optional[str]) -> str:
    """The HTTP error for a refusal.

    404 rather than 403 when the viewer cannot see the object at all: a 403 confirms it exists, which
    turns guessing identifiers into enumeration.
    """
    if viewer_id is None:
        return f"Sign in to {permission} this {noun}."
    return {
        PLAY: f"You can watch this {noun} but not play in it. The owner has to give you a seat.",
        MANAGE: f"Only the owner of this {noun} can change it.",
        MODERATE: f"Only a moderator or the owner can unpublish this {noun}.",
        ADMIN: "That is reserved to administrators.",
    }.get(permission, f"You do not have {permission} access to this {noun}.")


# ---------------------------------------------------------------------------- the gate

def on(session, viewer: Optional[User], obj, *, slug: Optional[str] = None,
       seat_holder: bool = False, acl: Optional[Set[str]] = None) -> Access:
    """Resolve what `viewer` may do with `obj`.

    `slug` is the secret from a shared link, when the caller presented one. `seat_holder` is set by
    game routes when the caller proved they hold a seat token, which grants play regardless of the
    account. `acl` is the viewer's allowlist permissions on `obj` when the caller already read them
    for a whole listing (`acl_permissions_for`); otherwise they are read here.
    """
    if obj is None:
        return Access(set(), reason="No such item.")

    object_type = object_type_of(obj)
    object_id = obj.id
    owner_id = getattr(obj, "owner_id", None)
    visibility = getattr(obj, "visibility", PRIVATE)
    viewer_id = viewer.id if viewer is not None else None
    granted: Set[str] = set()

    # A server is not shared directly; it inherits whatever its group allows.
    if isinstance(obj, Server):
        return _server_access(session, viewer, obj, slug=slug)

    if getattr(obj, "deleted_at", None) is not None and not (viewer and viewer.role == "admin"):
        return Access(set(), object_type=object_type, object_id=object_id, viewer_id=viewer_id,
                      reason="That has been deleted.")

    if seat_holder:
        granted |= {VIEW, PLAY}

    published = visibility == GLOBAL or (visibility == LINK and bool(slug) and _slug_matches(obj, slug))
    if viewer is not None:
        if viewer.role == "admin":
            # Administrators run the server; there is no object they cannot reach. That power is
            # what the audit log exists to make visible.
            granted |= set(ALL)
        elif owner_id and viewer.id == owner_id:
            granted |= {VIEW, PLAY, MANAGE}
        if viewer.role == "moderator" and published:
            # Enough to take published content down, and no more: unpublishing is MODERATE, never
            # MANAGE. A moderator has no path to a private game, and reaches an *unlisted* one only
            # by holding the link, which is what a report about it would contain. On a `server`
            # object a moderator is a member like any other.
            granted |= {VIEW, MODERATE}

        granted |= acl if acl is not None else _acl_permissions(session, object_type, object_id, viewer.id)

    if published or (visibility == SERVER and is_member(viewer)):
        granted.add(VIEW)

    # A suspended account keeps nothing, whatever it owns or has been granted.
    if viewer is not None and viewer.status in ("suspended", "deleted"):
        granted = set()

    reason = ""
    if not granted and viewer is None and visibility != GLOBAL:
        reason = "Sign in to see this."
    return Access(granted, object_type=object_type, object_id=object_id, viewer_id=viewer_id,
                  visibility=visibility, reason=reason)


def _slug_matches(obj, slug: str) -> bool:
    """Whether a share slug matches, compared in constant time."""
    import hmac
    actual = getattr(obj, "slug", "") or ""
    return bool(actual) and hmac.compare_digest(actual, slug)


def _has_grant(session, viewer: User, group_id: str) -> bool:
    """Whether this viewer holds any live grant on a server group.

    Deliberately narrow: it answers "may they use it", not "on what terms". The terms are the
    admission controller's job, and duplicating them here would be two places to keep in step.
    """
    from datetime import datetime, timezone

    from ..db.models import AccessGrant
    now = datetime.now(timezone.utc)
    rows = session.scalars(select(AccessGrant).where(
        AccessGrant.group_id == group_id, AccessGrant.enabled.is_(True)))
    return any(
        (g.expires_at is None or g.expires_at > now)
        and (g.subject_type == "everyone"
             or (g.subject_type == "user" and g.subject_user_id == viewer.id))
        for g in rows)


def _acl_permissions(session, object_type: str, object_id: str, user_id: str) -> Set[str]:
    """Explicit grants, expanded so that a higher permission implies the lower ones.

    Granting somebody `manage` without `view` would be nonsense, and forcing the caller to add three
    rows to express one idea is how ACL tables end up inconsistent.
    """
    rows = session.scalars(select(AclEntry).where(
        AclEntry.object_type == object_type, AclEntry.object_id == object_id,
        AclEntry.user_id == user_id))
    granted: Set[str] = set()
    for row in rows:
        granted |= _IMPLIES.get(row.permission, set())
    return granted


#: What each allowlist permission grants: a higher one implies the lower ones.
_IMPLIES = {MANAGE: {VIEW, PLAY, MANAGE}, PLAY: {VIEW, PLAY}, VIEW: {VIEW}}


def acl_permissions_for(session, object_type: str, object_ids, user_id: str) -> dict:
    """`_acl_permissions` for many objects of one type in one query, for a listing: {object id: permissions},
    with an object that has no entry for the viewer left out."""
    out: dict = {}
    ids = list(object_ids)
    if not ids:
        return out
    for row in session.scalars(select(AclEntry).where(
            AclEntry.object_type == object_type, AclEntry.object_id.in_(ids), AclEntry.user_id == user_id)):
        out.setdefault(row.object_id, set()).update(_IMPLIES.get(row.permission, set()))
    return out


def is_site_machine(server: Server) -> bool:
    """Whether a machine is the site's: the CITAR host, or one the site provides (``server_resource``: its online
    APIs, whose keys are the site's). Its ``owner_id`` (the administrator who imported or registered it) gives no
    rights of its own: an administrator in good standing manages it, and anyone else reaches it only through a
    grant, so an administrator later made a member keeps nothing of it (design/phase3-features.md P3.2.8)."""
    return bool(getattr(server, "is_host", False) or getattr(server, "server_resource", False))


def _server_access(session, viewer: Optional[User], server: Server, *, slug: Optional[str]) -> Access:
    """A server's visibility comes from its group; its *configuration* stays private to its owner.

    Being allowed to run a model on somebody's machine is not being allowed to read its API key
    reference, its electricity tariff or what it cost them. So a grant yields `view` only, and
    `manage` is the owner and administrators alone. The site's machines (``is_site_machine``) have no
    owner but the administrators.
    """
    viewer_id = viewer.id if viewer is not None else None
    granted: Set[str] = set()
    group = session.get(ServerGroup, server.group_id) if server.group_id else None
    if viewer is not None:
        if viewer.role == "admin":
            granted |= set(ALL)
        elif server.owner_id and viewer.id == server.owner_id and not is_site_machine(server):
            granted |= {VIEW, PLAY, MANAGE}
        elif group is not None:
            if VIEW in on(session, viewer, group, slug=slug).permissions:
                granted.add(VIEW)
            elif _has_grant(session, viewer, group.id):
                # Being granted *use* of a group implies being able to see what is in it.
                # Without this a person can run a model on a server that the API then claims
                # does not exist, which is incoherent and makes the UI impossible to write.
                granted.add(VIEW)
    if viewer is not None and viewer.status in ("suspended", "deleted"):
        granted = set()
    return Access(granted, object_type="server", object_id=server.id, viewer_id=viewer_id,
                  visibility=group.visibility if group is not None else PRIVATE)


# ---------------------------------------------------------------------------- list filtering

def visible_filter(model, viewer: Optional[User]):
    """A SQLAlchemy condition for "rows this viewer may see", for list endpoints.

    Doing this in SQL rather than fetching everything and filtering in Python is not only faster —
    it is the difference between a listing that accidentally leaks a name or a turn count in a
    payload the UI happens not to render, and one where the row never leaves the database.

    `global` rows are listed for everyone, `server` rows for members in good standing, and a viewer's
    own rows and the rows shared with them by an ACL entry for that viewer; administrators list
    everything. `link` rows are excluded on purpose: a link-shared object is reachable by its URL but
    is never *listed*, which is what "unlisted" means. A moderator lists what a member lists.
    """
    from sqlalchemy import and_, literal

    if viewer is not None and viewer.role == "admin":
        return model.deleted_at.is_(None) if hasattr(model, "deleted_at") else literal(True)

    conditions = [model.visibility == GLOBAL]
    if viewer is not None and viewer.status not in ("suspended", "deleted"):
        object_type = model.kind if model is OwnedItem else OBJECT_TYPES[model]
        conditions.append(model.owner_id == viewer.id)
        conditions.append(model.id.in_(
            select(AclEntry.object_id).where(AclEntry.object_type == object_type,
                                             AclEntry.user_id == viewer.id)))
        if is_member(viewer):
            conditions.append(model.visibility == SERVER)
    clause = or_(*conditions)
    if hasattr(model, "deleted_at"):
        return and_(clause, model.deleted_at.is_(None))
    return clause


def visible(session, model, viewer: Optional[User], *, limit: int = 200, order_by=None) -> list:
    """Filter a list of objects to the ones this viewer may see."""
    stmt = select(model).where(visible_filter(model, viewer)).limit(limit)
    if order_by is not None:
        stmt = stmt.order_by(order_by)
    return list(session.scalars(stmt))


# ---------------------------------------------------------------------------- sharing

def check_publish(actor: Optional[User], visibility: str) -> str:
    """Whether ``actor`` may give something this visibility: it must be on the ladder, and `global` needs
    `publish_global` (`server` needs nothing: it stays among members). Raises the refusal; returns it."""
    from . import policy

    check_visibility(visibility)
    if visibility == GLOBAL and not policy.can(actor, "publish_global"):
        raise HTTPException(403, "Your account is not allowed to publish to the whole internet. "
                                 "Members-only sharing still works; an administrator can grant the rest.")
    return visibility


def set_visibility(session, obj, visibility: str, *, actor: User) -> None:
    """Change who can see something, enforcing the one capability that has consequences off-server:
    `global` needs `publish_global`; `server` needs nothing (members only)."""
    from . import audit

    check_publish(actor, visibility)
    before = obj.visibility
    if before == visibility:
        return
    obj.visibility = visibility
    object_type = object_type_of(obj)
    audit.record(session, f"{object_type}.visibility", actor=actor, object_type=object_type,
                 object_id=obj.id, **{"from": before, "to": visibility})


def rotate_slug(session, obj, *, actor: User) -> str:
    """Issue a new share link, invalidating every copy of the old one."""
    import secrets

    from . import audit
    obj.slug = secrets.token_urlsafe(16)
    object_type = object_type_of(obj)
    audit.record(session, f"{object_type}.slug_rotated", actor=actor, object_type=object_type,
                 object_id=obj.id)
    return obj.slug


def unpublish(session, obj, *, actor: User, reason: str = "") -> None:
    """Take something down (P3.2.3): private, a new slug so every shared link dies, and an audit row
    with the reason, which the owner sees on the item (``last_unpublished``), and the role the actor took it
    down in. The caller has checked MODERATE or MANAGE."""
    import secrets

    from . import audit
    before = obj.visibility
    obj.visibility = PRIVATE
    obj.slug = secrets.token_urlsafe(16)
    object_type = object_type_of(obj)
    audit.record(session, f"{object_type}.unpublished", actor=actor, object_type=object_type,
                 object_id=obj.id, reason=(reason or "").strip()[:1000], by_role=actor.role, **{"from": before})


def last_unpublished(session, obj, viewer_id: Optional[str] = None) -> Optional[dict]:
    """The latest unpublish of an object (when, why, and by whom as ``unpublished_by`` words it), for
    whoever manages it; None if it never was, or was published again since. Never the actor's handle: a
    moderator's name shown to the member whose game they took down invites a quarrel with that person,
    and administrators have the audit log."""
    from ..db.models import AuditLog
    object_type = object_type_of(obj)
    row = session.scalar(select(AuditLog).where(
        AuditLog.object_type == object_type, AuditLog.object_id == obj.id,
        AuditLog.action.in_((f"{object_type}.unpublished", f"{object_type}.visibility")))
        .order_by(AuditLog.id.desc()).limit(1))
    if row is None or row.action != f"{object_type}.unpublished":
        return None
    return {"at": row.at.isoformat() if row.at else None, "by": unpublished_by(row, obj, viewer_id),
            "reason": (row.detail or {}).get("reason", "")}


def unpublished_by(entry, obj, viewer_id: Optional[str] = None) -> str:
    """Who took an object down, as its managers are told: "you", "its owner", "an administrator",
    "a moderator", or "someone who manages it" (an account on its allowlist with manage)."""
    if viewer_id is not None and entry.actor_id == viewer_id:
        return "you"
    if entry.actor_id is not None and entry.actor_id == getattr(obj, "owner_id", None):
        return "its owner"
    role = (entry.detail or {}).get("by_role")
    if role == "admin":
        return "an administrator"
    if role == "moderator":
        return "a moderator"
    return "someone who manages it"


def grant(session, obj, user: User, permission: str, *, actor: User) -> AclEntry:
    """Add somebody to an object's allowlist."""
    from . import audit

    if permission not in (VIEW, PLAY, MANAGE):
        raise HTTPException(400, "Permission is view, play or manage.")
    object_type = object_type_of(obj)
    existing = session.scalar(select(AclEntry).where(
        AclEntry.object_type == object_type, AclEntry.object_id == obj.id,
        AclEntry.user_id == user.id, AclEntry.permission == permission))
    if existing is not None:
        return existing
    entry = AclEntry(object_type=object_type, object_id=obj.id, user_id=user.id,
                     permission=permission, granted_by=actor.id)
    session.add(entry)
    audit.record(session, f"{object_type}.shared", actor=actor, object_type=object_type,
                 object_id=obj.id, with_user=user.handle, permission=permission)
    return entry


def revoke(session, obj, user: User, *, actor: User, permission: Optional[str] = None) -> int:
    """Remove somebody's access. Without `permission`, removes all of theirs."""
    from . import audit

    object_type = object_type_of(obj)
    stmt = select(AclEntry).where(AclEntry.object_type == object_type,
                                  AclEntry.object_id == obj.id, AclEntry.user_id == user.id)
    if permission:
        stmt = stmt.where(AclEntry.permission == permission)
    rows = list(session.scalars(stmt))
    for row in rows:
        session.delete(row)
    if rows:
        audit.record(session, f"{object_type}.unshared", actor=actor, object_type=object_type,
                     object_id=obj.id, with_user=user.handle)
    return len(rows)


def shared_with(session, obj, viewer: Optional[User]) -> list:
    """Who has explicit access, for the sharing dialog, each account as ``viewer`` (whoever manages the
    object) may see it (``User.public``: a hidden display name or avatar stays hidden; the handle shows)."""
    object_type = object_type_of(obj)
    rows = session.scalars(select(AclEntry).where(AclEntry.object_type == object_type,
                                                  AclEntry.object_id == obj.id))
    out: dict = {}
    for row in rows:
        user = session.get(User, row.user_id)
        if user is None:
            continue
        entry = out.setdefault(user.id, {"user": user.public(viewer), "permissions": []})
        entry["permissions"].append(row.permission)
    return list(out.values())


#: Where an object opens in the web client, by its object_type (an owned item's kind).
_PAGES = {"game": "game", "server_group": "servers", "report": "reports", "benchmark_suite": "benchmarks",
          "benchmark_run": "benchmarks", "probe": "probes", "probe_run": "probes", "lab_experiment": "lab",
          "scenario": "scenarios", "map": "editor", "bot_profile": "bots"}


def page_path(obj) -> Optional[str]:
    """Where an object opens in the web client (``#/<page>/<id>``), or None for a kind with no page of its own.
    An owned item is addressed by its id on disk (its `ref`), as its page names it."""
    page = _PAGES.get(object_type_of(obj), "")
    if not page:
        return None
    ident = obj.ref if isinstance(obj, OwnedItem) else obj.id
    return f"#/{page}/{ident}"


def share_url(obj, base: str) -> Optional[str]:
    """The link to hand out (P3.2.3): for `link` it carries the key; for `server` and `global` it is
    the object's plain address, which members (or everyone) open without one. None below `link`."""
    visibility = getattr(obj, "visibility", PRIVATE)
    if visibility not in (LINK, SERVER, GLOBAL):
        return None
    path = page_path(obj)
    if not path:
        return None
    url = f"{base.rstrip('/')}/{path}"
    return f"{url}?k={obj.slug}" if visibility == LINK else url
