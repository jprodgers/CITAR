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
    admin     the above plus anything reserved to administrators

Each implies the ones before it for the object's owner, but not in general: a spectator link grants
`view` and nothing else, and an ACL entry grants exactly what it says.

Visibility
----------
    private    owner (and admins) only
    allowlist  owner plus explicit ACL entries
    link       anyone holding the unguessable slug — never listed, never indexed
    public     anyone at all, listed

Two rules that are easy to get wrong and are therefore stated once here, in code, rather than
repeated per route:

*Watching and playing are separate.* A game's `visibility` governs who may watch. Playing is the
seat holders plus explicit `play` grants, and is never implied by a public spectator link. So "open
to the internet if the link is good" does not mean strangers can take your cities.

*Moderators are not universal readers.* A moderator can act on what has been published — unpublish
a public game, take down a public report — but has no route to somebody's private games. Giving
moderators blanket read access is how a moderation tool turns into a surveillance tool.
"""
from __future__ import annotations

import logging
from typing import Optional, Set

from fastapi import HTTPException
from sqlalchemy import or_, select

from ..db.models import AclEntry, Game, Report, Server, ServerGroup, User

log = logging.getLogger("citar.access")

VIEW, PLAY, MANAGE, ADMIN = "view", "play", "manage", "admin"
ALL = (VIEW, PLAY, MANAGE, ADMIN)

#: Which model class maps to which object_type string in acl_entries.
OBJECT_TYPES = {Game: "game", Report: "report", ServerGroup: "server_group", Server: "server"}


class Access:
    """The answer, plus enough context to explain a refusal."""

    __slots__ = ("permissions", "object_type", "object_id", "viewer_id", "visibility", "reason")

    def __init__(self, permissions: Set[str], *, object_type: str = "", object_id: str = "",
                 viewer_id: Optional[str] = None, visibility: str = "", reason: str = ""):
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
                "can_manage": MANAGE in self.permissions}


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
        ADMIN: "That is reserved to administrators.",
    }.get(permission, f"You do not have {permission} access to this {noun}.")


# ---------------------------------------------------------------------------- the gate

def on(session, viewer: Optional[User], obj, *, slug: Optional[str] = None,
       seat_holder: bool = False) -> Access:
    """Resolve what `viewer` may do with `obj`.

    `slug` is the secret from a shared link, when the caller presented one. `seat_holder` is set by
    game routes when the caller proved they hold a seat token, which grants play regardless of the
    account.
    """
    if obj is None:
        return Access(set(), reason="No such item.")

    object_type = OBJECT_TYPES.get(type(obj))
    if object_type is None:
        raise TypeError(f"access.on() does not know about {type(obj).__name__}.")

    object_id = obj.id
    owner_id = getattr(obj, "owner_id", None)
    visibility = getattr(obj, "visibility", "private")
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

    if viewer is not None:
        if viewer.role == "admin":
            # Administrators run the server; there is no object they cannot reach. That power is
            # what the audit log exists to make visible.
            granted |= set(ALL)
        elif owner_id and viewer.id == owner_id:
            granted |= {VIEW, PLAY, MANAGE}
        elif viewer.role == "moderator" and (
                visibility == "public" or (visibility == "link" and slug and _slug_matches(obj, slug))):
            # Enough to take published content down, and no more. A moderator has no path to a
            # private game, and reaches an *unlisted* one only by holding the link — which is what
            # a report about it would contain. Blanket access to unlisted content would make the
            # moderation role a way to browse everything anybody had ever shared.
            granted |= {VIEW, MANAGE}

        granted |= _acl_permissions(session, object_type, object_id, viewer.id)

    if visibility == "public":
        granted.add(VIEW)
    elif visibility == "link" and slug and _slug_matches(obj, slug):
        granted.add(VIEW)

    # A suspended account keeps nothing, whatever it owns or has been granted.
    if viewer is not None and viewer.status in ("suspended", "deleted"):
        granted = set()

    reason = ""
    if not granted and viewer is None and visibility != "public":
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
        if row.permission == MANAGE:
            granted |= {VIEW, PLAY, MANAGE}
        elif row.permission == PLAY:
            granted |= {VIEW, PLAY}
        elif row.permission == VIEW:
            granted.add(VIEW)
    return granted


def _server_access(session, viewer: Optional[User], server: Server, *, slug: Optional[str]) -> Access:
    """A server's visibility comes from its group; its *configuration* stays private to its owner.

    Being allowed to run a model on somebody's machine is not being allowed to read its API key
    reference, its electricity tariff or what it cost them. So a grant yields `view` only, and
    `manage` is the owner and administrators alone.
    """
    viewer_id = viewer.id if viewer is not None else None
    granted: Set[str] = set()
    if viewer is not None:
        if viewer.role == "admin":
            granted |= set(ALL)
        elif viewer.id == server.owner_id:
            granted |= {VIEW, PLAY, MANAGE}
        elif server.group_id:
            group = session.get(ServerGroup, server.group_id)
            if group is not None:
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
                  visibility=("public" if server.group_id else "private"))


# ---------------------------------------------------------------------------- list filtering

def visible_filter(model, viewer: Optional[User]):
    """A SQLAlchemy condition for "rows this viewer may see", for list endpoints.

    Doing this in SQL rather than fetching everything and filtering in Python is not only faster —
    it is the difference between a listing that accidentally leaks a name or a turn count in a
    payload the UI happens not to render, and one where the row never leaves the database.

    `link` rows are excluded on purpose: a link-shared object is reachable by its URL but is never
    *listed*, which is what "unlisted" means.
    """
    if viewer is not None and viewer.role == "admin":
        return model.deleted_at.is_(None) if hasattr(model, "deleted_at") else True

    conditions = [model.visibility == "public"]
    if viewer is not None:
        object_type = OBJECT_TYPES[model]
        conditions.append(model.owner_id == viewer.id)
        conditions.append(model.id.in_(
            select(AclEntry.object_id).where(AclEntry.object_type == object_type,
                                             AclEntry.user_id == viewer.id)))
        if viewer.role == "moderator":
            conditions.append(model.visibility == "public")
    clause = or_(*conditions)
    if hasattr(model, "deleted_at"):
        from sqlalchemy import and_
        return and_(clause, model.deleted_at.is_(None))
    return clause


def visible(session, model, viewer: Optional[User], *, limit: int = 200, order_by=None) -> list:
    """Filter a list of objects to the ones this viewer may see."""
    stmt = select(model).where(visible_filter(model, viewer)).limit(limit)
    if order_by is not None:
        stmt = stmt.order_by(order_by)
    return list(session.scalars(stmt))


# ---------------------------------------------------------------------------- sharing

def set_visibility(session, obj, visibility: str, *, actor: User) -> None:
    """Change who can see something, enforcing the one capability that has consequences off-server."""
    from . import audit, policy

    if visibility not in ("private", "allowlist", "link", "public"):
        raise HTTPException(400, "Visibility is private, allowlist, link or public.")
    if visibility == "public" and not policy.can(actor, "publish_public"):
        raise HTTPException(403, "Your account is not allowed to publish to the public internet. "
                                 "An administrator can grant that.")
    before = obj.visibility
    if before == visibility:
        return
    obj.visibility = visibility
    audit.record(session, f"{OBJECT_TYPES[type(obj)]}.visibility", actor=actor,
                 object_type=OBJECT_TYPES[type(obj)], object_id=obj.id,
                 **{"from": before, "to": visibility})


def rotate_slug(session, obj, *, actor: User) -> str:
    """Issue a new share link, invalidating every copy of the old one."""
    import secrets

    from . import audit
    obj.slug = secrets.token_urlsafe(16)
    audit.record(session, f"{OBJECT_TYPES[type(obj)]}.slug_rotated", actor=actor,
                 object_type=OBJECT_TYPES[type(obj)], object_id=obj.id)
    return obj.slug


def grant(session, obj, user: User, permission: str, *, actor: User) -> AclEntry:
    """Add somebody to an object's allowlist."""
    from . import audit

    if permission not in (VIEW, PLAY, MANAGE):
        raise HTTPException(400, "Permission is view, play or manage.")
    object_type = OBJECT_TYPES[type(obj)]
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

    object_type = OBJECT_TYPES[type(obj)]
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


def shared_with(session, obj) -> list:
    """Who has explicit access, for the sharing dialog."""
    object_type = OBJECT_TYPES[type(obj)]
    rows = session.scalars(select(AclEntry).where(AclEntry.object_type == object_type,
                                                  AclEntry.object_id == obj.id))
    out: dict = {}
    for row in rows:
        user = session.get(User, row.user_id)
        if user is None:
            continue
        entry = out.setdefault(user.id, {"user": user.public(), "permissions": []})
        entry["permissions"].append(row.permission)
    return list(out.values())


def share_url(obj, base: str) -> Optional[str]:
    """The link to hand out, or None when the object is not link-shared."""
    if getattr(obj, "visibility", "") not in ("link", "public"):
        return None
    kind = {"game": "game", "report": "reports", "server_group": "servers"}.get(
        OBJECT_TYPES.get(type(obj), ""), "")
    if not kind:
        return None
    return f"{base.rstrip('/')}/#/{kind}/{obj.id}?k={obj.slug}"
