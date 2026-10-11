"""Ownership and sharing for games: the bridge between a live GameSession and its database row.

A GameSession is in-memory state driven by a turn thread. The `games` table is the index over it:
who owns it, who may see it, whether its results feed the public averages. Keeping them separate
means the engine stays unaware that accounts exist — the turn driver has no business importing
permission code — and it means a game's sharing survives a restart even though the session does not.

The row is created when a game is created and kept in step at the points that matter
(design/phase3-features.md P3.2.5): once a round, from the session's save writer after the round's
autosave is on the disk (off the game's lock, so it costs the game nothing), and when the session
stops. Its status is `playing` while the game is, `finished` once it is over, `closed` when the
session stopped before the end, and `playing` again when it is loaded. It is not written on every
engine event: a 330-turn benchmark game would otherwise be a few thousand needless writes. A game
loaded from disk without a row gets one (`index_loaded`), and at boot the rows left `playing` with no
live session are closed (`close_orphans`).

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

#: The statuses that count as a live game: a lobby game (`kind = game`) in one of them counts against
#: its owner's game limit, and at boot one with no session is closed.
LIVE_STATUSES = ("setup", "playing")


def status_for(phase: Optional[str], stopped: bool = False) -> str:
    """A row's status for the engine's phase (`playing` or `over`): `finished` once the game is over;
    while it plays, `playing`, or `closed` when its session stopped before the end."""
    if phase == "over":
        return "finished"
    if phase == "playing":
        return "closed" if stopped else "playing"
    return "setup"


def register(session, gs, owner: Optional[User], *, kind: str = "game",
             visibility: str = access.PRIVATE, data_sharing: Optional[str] = None,
             save_path: Optional[str] = None) -> GameRow:
    """Create (or refresh) the index row for a live session, and give the session its owner."""
    access.check_visibility(visibility)
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
    _apply(row, getattr(gs.game, "turn", 0), getattr(gs.game, "phase", None), stopped=False)
    row.deleted_at = None
    if save_path:
        row.save_path = save_path
    row.meta = {"seats": [{"player": s.player, "type": s.type, "name": s.name} for s in gs.seats]}
    session.flush()
    gs.owner_id = row.owner_id
    return row


def _apply(row: GameRow, turn: int, phase: Optional[str], *, stopped: bool) -> None:
    """Put a turn and the status of a phase on a row."""
    now = datetime.now(timezone.utc)
    row.turn = int(turn or 0)
    row.status = status_for(phase, stopped)
    if row.status == "finished":
        if row.finished_at is None:
            row.finished_at = now
    else:
        row.finished_at = None
    row.updated_at = now


def sync(session, gid: str, *, turn: int, phase: Optional[str], stopped: bool = False) -> Optional[GameRow]:
    """Put a game's turn and status on its row (none for a game without one)."""
    row = session.get(GameRow, gid)
    if row is None:
        return None
    _apply(row, turn, phase, stopped=stopped)
    return row


def sync_quietly(gid: str, *, turn: int, phase: Optional[str], stopped: bool = False) -> None:
    """``sync`` in a database session of its own, from the session's save writer (after a round's
    autosave) or from its stop, never with the game's lock held.

    Never raises: a game in progress must not stop because the index could not be written. Does nothing in a
    process that has not opened its database (a command-line tool playing games without a server).
    """
    if not db.configured():
        return
    try:
        with db.session() as s:
            sync(s, gid, turn=turn, phase=phase, stopped=stopped)
    except Exception:
        log.debug("could not sync game row for %s", gid, exc_info=True)


def index_loaded(gs) -> Optional[str]:
    """A session just loaded from disk (the load route, ``restore_live``, a benchmark's reload): give
    a game without a row one (``register_existing``, owned by nobody, private), mark its row as
    loaded (`playing`, or `finished` for a game that is over), and return the row's owner for the
    session. Never raises: an index that cannot be written leaves the game owned by nobody, which
    only administrators reach. Nothing in a process that has not opened its database."""
    if not db.configured():
        return None
    try:
        with db.session() as s:
            row = s.get(GameRow, gs.id)
            if row is None:
                row = register_existing(s, gs, None)
            else:
                _apply(row, gs.game.turn, gs.game.phase, stopped=False)
            return row.owner_id if row is not None else None
    except Exception:
        log.warning("could not index loaded game %s", getattr(gs, "id", "?"), exc_info=True)
        return None


def close_orphans(live_ids) -> list:
    """Close every row left `setup` or `playing` with no live session (a failed restore, a deleted
    save folder, a crash between registration and the first save), so no row counts against a
    game limit for a game that is not there. Run at boot once the lobby games, the benchmarks and
    the probes are back; logs a line per row. Returns the ids closed."""
    from sqlalchemy import select as _select
    live = set(live_ids)
    closed = []
    with db.session() as s:
        for row in s.scalars(_select(GameRow).where(GameRow.status.in_(LIVE_STATUSES),
                                                    GameRow.deleted_at.is_(None))):
            if row.id in live:
                continue
            row.status = "closed"
            row.updated_at = datetime.now(timezone.utc)
            closed.append(row.id)
            log.warning("game %s (%s) was %s with no live session: marked closed", row.id, row.name,
                        "being played" if row.turn else "set up")
    return closed


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
    """The sharing block the browser shows next to a game. The share URL (``access.share_url``) goes
    to whoever manages the game: for `link` it carries the key, for `server` and `global` it does not."""
    if row is None:
        return {"owner_id": None, "visibility": access.PRIVATE, "data_sharing": "pool", **perms.client()}
    out = {
        "owner_id": row.owner_id,
        "visibility": row.visibility,
        "data_sharing": row.data_sharing,
        "kind": row.kind,
        "status": row.status,
        **perms.client(),
    }
    if access.MANAGE in perms and base_url:
        url = access.share_url(row, base_url)
        if url:
            out["share_url"] = url
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
    """Index a session that has no row yet: a game loaded from a save whose row is gone (the database
    was renamed aside for 0.1.6's, or the save came from elsewhere). Returns None when it has one."""
    if session.get(GameRow, gs.id) is not None:
        return None
    kind = "benchmark" if getattr(gs, "benchmark", None) else "game"
    return register(session, gs, owner, kind=kind, visibility=access.PRIVATE)
