"""Ownership and sharing for games: the bridge between a live GameSession and its database row.

A GameSession is in-memory state driven by a turn thread. The `games` table is the index over it:
who owns it, who may see it, whether its results feed the public averages. Keeping them separate
means the engine stays unaware that accounts exist — the turn driver has no business importing
permission code — and it means a game's sharing survives a restart even though the session does not.

The row is created when a game is created and kept in step at the points that matter (turn
advanced, game finished, game deleted). It is not written on every engine event: a 330-turn
benchmark game would otherwise be a few thousand needless writes.

Two kinds of caller reach a game, and both must keep working:

*Accounts*, through a session cookie. Governed by `access.on()` like everything else.

*Seat tokens*, held by AI agents, MCP clients and anyone handed a seat link. These pre-date accounts
and have to keep working — an LLM player is not going to log in. A valid seat token proves control
of one seat and grants play on that seat alone; it is not a login and confers nothing else.
"""
from __future__ import annotations

import logging
from datetime import datetime, timezone
from typing import Optional, Tuple

from sqlalchemy import select

from .. import db
from ..auth import access, audit
from ..db.models import Game as GameRow
from ..db.models import User

log = logging.getLogger("citar.ownership")


def register(session, gs, owner: Optional[User], *, kind: str = "game",
             visibility: str = "private", data_sharing: Optional[str] = None,
             save_path: Optional[str] = None) -> GameRow:
    """Create (or refresh) the index row for a live session."""
    row = session.get(GameRow, gs.id)
    if row is None:
        row = GameRow(id=gs.id)
        session.add(row)
    row.owner_id = owner.id if owner is not None else None
    row.name = gs.name or f"Game {gs.id}"
    row.kind = kind
    row.visibility = visibility
    # A game inherits the owner's default, so somebody who has opted out of pooling does not have to
    # remember to opt out again for every game they start.
    row.data_sharing = data_sharing or (owner.data_sharing if owner is not None else "pool")
    row.status = getattr(gs.game.s, "phase", "setup") or "setup"
    row.turn = getattr(gs.game, "turn", 0)
    if save_path:
        row.save_path = save_path
    row.meta = {"seats": [{"player": s.player, "type": s.type, "name": s.name} for s in gs.seats]}
    session.flush()
    return row


def sync(session, gs) -> Optional[GameRow]:
    """Update turn and status from the live session."""
    row = session.get(GameRow, gs.id)
    if row is None:
        return None
    row.turn = getattr(gs.game, "turn", row.turn)
    phase = getattr(gs.game.s, "phase", None)
    if phase:
        row.status = phase
        if phase == "finished" and row.finished_at is None:
            row.finished_at = datetime.now(timezone.utc)
    row.updated_at = datetime.now(timezone.utc)
    return row


def sync_quietly(gs) -> None:
    """Best-effort sync from engine code that has no database session of its own.

    Never raises: a game in progress must not stop because the index could not be written.
    """
    try:
        with db.session() as s:
            sync(s, gs)
    except Exception:
        log.debug("could not sync game row for %s", getattr(gs, "id", "?"), exc_info=True)


def row_for(session, gid: str) -> Optional[GameRow]:
    """The ownership record for an object, created on first use."""
    return session.get(GameRow, gid)


def resolve(session, gs, viewer: Optional[User], *, slug: Optional[str] = None,
            seat_token: Optional[str] = None) -> Tuple[Optional[GameRow], access.Access]:
    """What may this caller do with this game?

    An unregistered game — one created before accounts existed, or by a code path that did not
    register it — is treated as owned by nobody and private. Administrators can still reach it, and
    the migration adopts it into an account.
    """
    row = session.get(GameRow, gs.id) if gs is not None else None
    seat = gs.seat_for_token(seat_token) if (gs is not None and seat_token) else None
    is_spectator = bool(gs is not None and seat_token and gs.is_spectator(seat_token))

    if row is None:
        # Nothing to consult. Grant on the token alone, plus admin.
        granted = set()
        if seat is not None:
            granted |= {access.VIEW, access.PLAY}
        if is_spectator:
            granted.add(access.VIEW)
        if viewer is not None and viewer.role == "admin":
            granted |= set(access.ALL)
        return None, access.Access(granted, object_type="game",
                                   object_id=getattr(gs, "id", ""),
                                   viewer_id=viewer.id if viewer else None,
                                   visibility="private")

    perms = access.on(session, viewer, row, slug=slug, seat_holder=seat is not None)
    if is_spectator:
        # The spectator token is a watch link for one game; it never implies play.
        perms.permissions.add(access.VIEW)
    return row, perms


def require(session, gs, viewer: Optional[User], permission: str, *, slug: Optional[str] = None,
            seat_token: Optional[str] = None) -> Tuple[Optional[GameRow], access.Access]:
    """Require a permission on an object, or raise the right refusal."""
    row, perms = resolve(session, gs, viewer, slug=slug, seat_token=seat_token)
    perms.require(permission)
    return row, perms


def visible_ids(session, viewer: Optional[User]) -> Optional[set]:
    """Ids of the games this viewer may list, or None meaning "everything" (administrators).

    Used by the games list, which iterates live sessions rather than the table: the in-memory
    session is the source of truth for what is running.
    """
    if viewer is not None and viewer.role == "admin":
        return None
    rows = session.scalars(select(GameRow.id).where(access.visible_filter(GameRow, viewer)))
    return set(rows)


def delete(session, gid: str, *, actor: Optional[User]) -> None:
    """Mark the index row deleted. The save files are handled by the caller."""
    row = session.get(GameRow, gid)
    if row is None:
        return
    row.deleted_at = datetime.now(timezone.utc)
    if actor is not None:
        audit.record(session, "game.deleted", actor=actor, object_type="game", object_id=gid)


def to_client(row: Optional[GameRow], perms: access.Access, *, base_url: str = "") -> dict:
    """The sharing block the browser shows next to a game."""
    if row is None:
        return {"owner_id": None, "visibility": "private", "data_sharing": "pool", **perms.client()}
    out = {
        "owner_id": row.owner_id,
        "visibility": row.visibility,
        "data_sharing": row.data_sharing,
        "kind": row.kind,
        **perms.client(),
    }
    if access.MANAGE in perms and row.visibility in ("link", "public") and base_url:
        out["share_url"] = f"{base_url.rstrip('/')}/#/game/{row.id}?k={row.slug}"
    return out


# ---------------------------------------------------------------------------- migration

def adopt_orphans(session, owner: User) -> int:
    """Give every unowned game to `owner`.

    Runs once when a single-user installation gains accounts: everything that existed before there
    were owners belongs to the person who has been running the server.
    """
    rows = list(session.scalars(select(GameRow).where(GameRow.owner_id.is_(None))))
    for row in rows:
        row.owner_id = owner.id
    if rows:
        audit.record(session, "migration.games_adopted", actor=owner, count=len(rows))
    return len(rows)


def register_existing(session, gs, owner: Optional[User]) -> Optional[GameRow]:
    """Index a session that has no row yet — a game loaded from a save written before accounts."""
    if session.get(GameRow, gs.id) is not None:
        return None
    kind = "benchmark" if getattr(gs, "benchmark", None) else "game"
    return register(session, gs, owner, kind=kind, visibility="private")
