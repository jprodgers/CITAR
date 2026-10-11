"""Owned items: every file-backed job type has an owner and a visibility (design/phase3-features.md P3.2.4).

Benchmark suites and runs, probes and probe runs, lab experiments, scenarios, maps, bot profiles and reports keep
their files where they always were. What they gain is a row in ``owned_items`` (kind, ref: the item's id on disk),
written by whatever creates the item and marked deleted by whatever deletes it, which says who owns it and who may
see it, on the same ladder as a game (``access.on``; an ACL entry's object type is the row's kind). This module is
the only writer of those rows, and the one gate every item route goes through:

    row, perms = items.require(sdb, "probe", pid, me, access.MANAGE)     # 404 for a stranger, 403 for a viewer
    refs = items.visible_refs(sdb, "map", me)                             # what a listing may show

A file with no row is nobody's, so no route serves it: ``require`` answers 404 for everyone. Such files exist only
where something wrote one without registering it (a crash between the two, a file copied in by hand, a lab
experiment queued from the command line with no database at hand, everything from before 0.1.6); ``adopt_orphans``
registers them, to the account the file itself names when it names one, else to the first administrator, private,
with one audit row. It runs at every boot that has an administrator, and from ``accounts.on_first_admin`` when the
first one is made on a fresh server-mode database (``policy`` marker ``orphans_adopted``). Until then those files
answer 404, so a half-written file never becomes an unowned, unguarded object.

System items are the shipped content (the built-in bot profiles): ``owner_id`` null, ``global``, managed by
administrators only, registered at boot.

Python lines this replaces: the item routes' bare ``require_user`` (app.py's benchmarks, maps, scenarios, probes,
probe runs, lab and queue routes; bots_api.py's admin-only writes; admin_api.py's admin-only reports).
"""
from __future__ import annotations

import gzip
import json
import logging
import secrets
from datetime import datetime, timezone
from pathlib import Path
from typing import Iterator, Optional, Tuple

from fastapi import HTTPException
from sqlalchemy import select

from . import db, paths
from .auth import access, audit
from .db.models import ITEM_KINDS, AclEntry, OwnedItem, User

log = logging.getLogger("citar.items")

KINDS = ITEM_KINDS

#: How each kind is named in a message ("No such benchmark run.").
NOUNS = {"benchmark_suite": "benchmark suite", "benchmark_run": "benchmark run", "probe": "probe",
         "probe_run": "probe run", "lab_experiment": "lab experiment", "scenario": "scenario", "map": "map",
         "bot_profile": "bot profile", "report": "report"}

#: The capability that creating an item of each kind needs (P3.2.4). Changing one needs MANAGE on it instead.
CREATE_CAPS = {"benchmark_suite": "run_benchmarks", "benchmark_run": "run_benchmarks", "probe": "run_probes",
               "probe_run": "run_probes", "lab_experiment": "run_lab", "scenario": "create_games",
               "map": "create_games", "bot_profile": "edit_bots", "report": "run_reports"}


def check_kind(kind: str) -> str:
    """``kind`` when it is an item kind; a 404 otherwise (a URL naming no kind names nothing)."""
    if kind not in KINDS:
        raise HTTPException(404, "No such item.")
    return kind


def noun(kind: str) -> str:
    """A kind as a message names it."""
    return NOUNS.get(kind, "item")


def _now() -> datetime:
    """The current time, UTC."""
    return datetime.now(timezone.utc)


def _owner_id(owner) -> Optional[str]:
    """An owner given as an account or an id, as an id."""
    if owner is None:
        return None
    return owner.id if isinstance(owner, User) else str(owner)


def relpath(path) -> Optional[str]:
    """A file's path as a row keeps it: relative to the state directory, with forward slashes, or absolute when the
    file lives elsewhere (``CITAR_BENCH_DIR`` may point outside it)."""
    if path is None:
        return None
    p = Path(path).resolve()
    try:
        return p.relative_to(paths.state_dir().resolve()).as_posix()
    except ValueError:
        return p.as_posix()


def file_path(row: OwnedItem) -> Optional[Path]:
    """Where a row's file is (``relpath`` undone), or None for a row that names none."""
    if not row.path:
        return None
    p = Path(row.path)
    return p if p.is_absolute() else paths.state_dir() / p


# ---------------------------------------------------------------------------- rows

def get(session, kind: str, ref: str, *, deleted: bool = False) -> Optional[OwnedItem]:
    """An item's row by its kind and id on disk; a deleted one only with ``deleted``."""
    row = session.scalar(select(OwnedItem).where(OwnedItem.kind == kind, OwnedItem.ref == str(ref)))
    if row is not None and row.deleted_at is not None and not deleted:
        return None
    return row


def register(session, kind: str, ref: str, owner, name: str = "", *, path=None,
             visibility: str = access.PRIVATE, meta: Optional[dict] = None, collection: bool = False,
             actor: Optional[User] = None) -> OwnedItem:
    """The row for an item just written to disk.

    A new item gets a row owned by ``owner`` (an account, an id, or None for a system item) at ``visibility``. An
    item that has a live row keeps its owner, visibility, sharing and slug, and only takes the new name, path and
    meta: saving changes the item, never who owns it (the route checked MANAGE). An id whose item was deleted is a
    new item: nothing of the old one carries over, its allowlist and its share link included. ``actor`` is recorded
    in the audit row of a new item (the owner when not given)."""
    check_kind(kind)
    access.check_visibility(visibility)
    row = get(session, kind, ref, deleted=True)
    if row is not None and row.deleted_at is None:
        if name:
            row.name = str(name)[:160]
        if path is not None:
            row.path = relpath(path)
        if meta is not None:
            row.meta = dict(meta)
        row.updated_at = _now()
        session.flush()
        return row
    if row is None:
        row = OwnedItem(kind=kind, ref=str(ref))
        session.add(row)
    else:
        for entry in session.scalars(select(AclEntry).where(AclEntry.object_type == kind,
                                                            AclEntry.object_id == row.id)):
            session.delete(entry)
        row.slug = secrets.token_urlsafe(16)
        row.deleted_at = None
        row.created_at = _now()
    row.owner_id = _owner_id(owner)
    row.name = str(name or ref)[:160]
    row.visibility = visibility
    row.path = relpath(path)
    row.meta = dict(meta or {})
    row.collection = bool(collection)
    row.updated_at = _now()
    session.flush()
    who = actor if actor is not None else (owner if isinstance(owner, User)
                                           else session.get(User, row.owner_id) if row.owner_id else None)
    audit.record(session, f"{kind}.created", actor=who, object_type=kind, object_id=row.id, ref=row.ref,
                 owner=row.owner_id, visibility=visibility)
    return row


def rename(session, kind: str, ref: str, name: str) -> Optional[OwnedItem]:
    """Give an item's row a new name (None when it has none)."""
    row = get(session, kind, ref)
    if row is not None and name:
        row.name = str(name)[:160]
        row.updated_at = _now()
    return row


def delete(session, kind: str, ref: str, *, actor: Optional[User] = None) -> Optional[OwnedItem]:
    """Mark an item's row deleted (``deleted_at``): it leaves every listing and every route answers 404. The caller
    removes the files."""
    row = get(session, kind, ref)
    if row is None:
        return None
    row.deleted_at = _now()
    audit.record(session, f"{kind}.deleted", actor=actor, object_type=kind, object_id=row.id, ref=row.ref)
    return row


def delete_quietly(kind: str, ref: str) -> None:
    """``delete`` in a database session of its own, for producers that remove items outside a request (a run removed
    by the scheduler). Never raises; nothing in a process that has not opened its database."""
    if not db.configured():
        return
    try:
        with db.session() as s:
            delete(s, kind, ref)
    except Exception:
        log.warning("could not mark %s %s deleted", kind, ref, exc_info=True)


# ---------------------------------------------------------------------------- the gate

def resolve(session, kind: str, ref: str, viewer: Optional[User], *,
            slug: Optional[str] = None) -> Tuple[Optional[OwnedItem], access.Access]:
    """What ``viewer`` may do with an item: its live row (None when it has none) and the permissions. An item with no
    row grants nothing, to administrators either: it is nobody's until it is adopted."""
    row = get(session, kind, ref)
    if row is None:
        return None, access.Access(set(), object_type=kind, object_id=str(ref),
                                   viewer_id=viewer.id if viewer is not None else None)
    return row, access.on(session, viewer, row, slug=slug)


def require(session, kind: str, ref: str, viewer: Optional[User], permission: str = access.VIEW, *,
            slug: Optional[str] = None) -> Tuple[OwnedItem, access.Access]:
    """An item ``viewer`` has ``permission`` on, with the permissions; 404 when they cannot see it (or it has no row),
    403 when they can see it but not do this (``Access.require``)."""
    check_kind(kind)
    row, perms = resolve(session, kind, ref, viewer, slug=slug)
    perms.require(permission)
    return row, perms


def check_create(user: User, kind: str) -> None:
    """The capability a new item of ``kind`` needs (``CREATE_CAPS``), as the 403 ``require_cap`` gives."""
    from .auth.deps import check_cap
    check_cap(user, CREATE_CAPS[check_kind(kind)])


#: What follows an item's visibility when it changes (its sharing route, an unpublish), by kind: a benchmark run's
#: games follow the run (registered by the server's app, which knows the runs).
VISIBILITY_HOOKS: dict = {}


def visibility_changed(session, row: OwnedItem) -> None:
    """Run the kind's hook after ``row``'s visibility changed."""
    hook = VISIBILITY_HOOKS.get(row.kind)
    if hook is not None:
        hook(session, row)


def writable(session, kind: str, ref: str, user: User, *, exists: bool) -> Optional[OwnedItem]:
    """The gate on saving an item under ``ref``, before anything is written: replacing a live item needs MANAGE on it
    (404 when the caller cannot see it, 403 when they only see it); a new one needs the kind's capability
    (``CREATE_CAPS``). ``exists`` says whether a file is already there: a file with no row is nobody's to take, so it
    is a 404 until adoption gives it an owner. Returns the live row, or None for a new item (the caller registers it
    once its file is written)."""
    row = get(session, kind, ref)
    if row is not None:
        access.on(session, user, row).require(access.MANAGE)
        return row
    if exists:
        raise HTTPException(404, f"No such {noun(kind)}.")
    check_create(user, kind)
    return None


def visible_refs(session, kind: str, viewer: Optional[User]) -> set:
    """The ids on disk of the items of ``kind`` a listing may show ``viewer`` (``access.visible_filter``: `global`
    ones for everyone, `server` ones for members, their own and those shared with them; every live one for
    administrators; `link` ones never)."""
    return set(session.scalars(select(OwnedItem.ref).where(OwnedItem.kind == kind,
                                                           access.visible_filter(OwnedItem, viewer))))


def listed(session, kind: str, viewer: Optional[User]) -> Optional[set]:
    """What a listing of ``kind`` shows ``viewer``: ``visible_refs``, or None (everything, files that have no row
    yet included) for an administrator in good standing."""
    if viewer is not None and viewer.role == "admin" and viewer.status == "active":
        return None
    return visible_refs(session, kind, viewer)


def visible(session, kind: str, viewer: Optional[User], *, limit: int = 500) -> list:
    """The rows of ``kind`` a listing may show ``viewer``, newest first."""
    return list(session.scalars(select(OwnedItem).where(OwnedItem.kind == kind,
                                                        access.visible_filter(OwnedItem, viewer))
                                .order_by(OwnedItem.created_at.desc()).limit(limit)))


def permissions_by_ref(session, kind: str, viewer: Optional[User], refs) -> dict:
    """``Access.client()`` for each of ``refs`` that has a row, for a listing to say which controls to show."""
    out = {}
    for ref in refs:
        row = get(session, kind, ref)
        if row is not None:
            out[ref] = access.on(session, viewer, row).client()
    return out


def owner_of(session, kind: str, ref: str) -> Optional[str]:
    """The id of an item's owner (None for none, or no row)."""
    row = get(session, kind, ref)
    return row.owner_id if row is not None else None


def visibility_of(session, kind: str, ref: str) -> str:
    """An item's visibility (`private` when it has no row)."""
    row = get(session, kind, ref)
    return row.visibility if row is not None else access.PRIVATE


def client(session, row: OwnedItem, perms: access.Access) -> dict:
    """An item's sharing as the browser shows it: its owner, visibility, the caller's permissions; for whoever
    manages it, its share link, who it is shared with and its last unpublish."""
    from . import settings
    owner = session.get(User, row.owner_id) if row.owner_id else None
    out = {"kind": row.kind, "ref": row.ref, "id": row.id, "name": row.name, "visibility": row.visibility,
           "owner": owner.public() if owner is not None else None, "system": row.owner_id is None,
           "created_at": row.created_at.isoformat() if row.created_at else None, **perms.client()}
    if access.MANAGE in perms:
        url = access.share_url(row, settings.get().public_origin)
        if url:
            out["share_url"] = url
        out["shared_with"] = access.shared_with(session, row)
        out["unpublished"] = access.last_unpublished(session, row, perms.viewer_id)
    return out


# ---------------------------------------------------------------------------- system items

def system_items() -> Iterator[Tuple[str, str, str]]:
    """The shipped content, as (kind, ref, name): the built-in bot profiles. The shipped build has no example
    scenarios or maps."""
    from .bots import profiles
    for p in profiles.BUILTIN:
        yield "bot_profile", p["id"], p["name"]


def register_system(session) -> int:
    """Give every shipped item its row, owned by nobody and `global` (only administrators manage it); returns how
    many rows were made. A row the shipped id already has keeps its sharing, but never an owner."""
    made = 0
    for kind, ref, name in system_items():
        row = get(session, kind, ref, deleted=True)
        if row is None or row.deleted_at is not None:
            register(session, kind, ref, None, name, visibility=access.GLOBAL)
            made += 1
        elif row.owner_id is not None:
            log.warning("the shipped %s %s had an owner; it is the server's", kind, ref)
            row.owner_id = None
    return made


# ---------------------------------------------------------------------------- adoption

def _read_json(p: Path) -> dict:
    """A JSON file's object, or {} when it does not read as one."""
    try:
        d = json.loads(p.read_text(encoding="utf-8"))
        return d if isinstance(d, dict) else {}
    except (OSError, ValueError):
        return {}


def _files(kind: str) -> Iterator[Tuple[str, str, Path, Optional[str]]]:
    """Every item of ``kind`` on disk, as (ref, name, path, the owner id its file records or None)."""
    if kind in ("benchmark_suite", "benchmark_run"):
        folder = paths.bench_path("suites" if kind == "benchmark_suite" else "runs")
        for p in sorted(folder.glob("*.json")) if folder.exists() else ():
            d = _read_json(p)
            yield p.stem, d.get("name") or p.stem, p, d.get("owner_id")
    elif kind == "probe":
        from . import probes
        for p in sorted(probes.BASE.glob("*.json")) if probes.BASE.exists() else ():
            d = _read_json(p)
            yield p.stem, d.get("name") or p.stem, p, d.get("owner_id")
    elif kind == "probe_run":
        from . import probes
        for p in sorted(probes.RUNS.glob("*/run.json")) if probes.RUNS.exists() else ():
            d = _read_json(p)
            yield p.parent.name, d.get("name") or p.parent.name, p.parent, d.get("owner_id")
    elif kind == "lab_experiment":
        from . import lab
        for folder in (lab.QUEUE, lab.DONE):
            for p in sorted(folder.glob("*.json")) if folder.exists() else ():
                d = _read_json(p)
                yield p.stem, d.get("name") or p.stem, p, d.get("owner_id")
    elif kind == "scenario":
        from . import engine_api
        folder = engine_api.scenario_dir()
        for p in sorted(folder.glob("*.citarscn")) if folder.exists() else ():
            try:
                with gzip.open(p, "rt", encoding="utf-8") as f:
                    d = json.load(f)
            except (OSError, ValueError):
                d = {}
            yield p.stem, (d.get("name") if isinstance(d, dict) else None) or p.stem, p, None
    elif kind == "map":
        from . import engine_api
        folder = engine_api.map_dir()
        for p in sorted(folder.glob("*.json")) if folder.exists() else ():
            yield p.stem, _read_json(p).get("name") or p.stem, p, None
    elif kind == "bot_profile":
        from .bots import profiles
        builtin = {b["id"] for b in profiles.BUILTIN}
        folder = profiles.PROFILES_DIR
        for p in sorted(folder.glob("*.json")) if folder.exists() else ():
            if p.stem not in builtin:
                yield p.stem, _read_json(p).get("name") or p.stem, p, None
    elif kind == "report":
        from .reports import REPORTS
        for p in sorted(REPORTS.glob("*/meta.json")) if REPORTS.exists() else ():
            d = _read_json(p)
            yield p.parent.name, d.get("title") or p.parent.name, p.parent / "report.html", d.get("owner_id")


def first_admin(session) -> Optional[User]:
    """The account strays are adopted by: the local owner in local mode, else the oldest active administrator."""
    local = session.scalar(select(User).where(User.is_local_owner.is_(True), User.role == "admin",
                                              User.status == "active"))
    if local is not None:
        return local
    return session.scalar(select(User).where(User.role == "admin", User.status == "active")
                          .order_by(User.created_at, User.id).limit(1))


def adopt_orphans(session, owner: User) -> dict:
    """Register every item file with no row (P3.2.4): to the account its file records (a run's or a report's
    ``owner_id``) when that account exists, else to ``owner``; private. Also gives ``owner`` the games index's rows
    that have no owner (``ownership.adopt_orphans``). One audit row when anything was adopted, and the
    ``orphans_adopted`` marker either way. Returns the count per kind (and ``game``)."""
    from .auth import policy
    from .server import ownership
    counts: dict = {}
    for kind in KINDS:
        known = set(session.scalars(select(OwnedItem.ref).where(OwnedItem.kind == kind,
                                                                OwnedItem.deleted_at.is_(None))))
        for ref, name, path, recorded in _files(kind):
            if ref in known:
                continue
            holder = session.get(User, recorded) if recorded else None
            if holder is None or holder.status == "deleted":
                holder = owner
            register(session, kind, ref, holder, name, path=path, actor=owner)
            known.add(ref)
            counts[kind] = counts.get(kind, 0) + 1
    games = ownership.adopt_orphans(session, owner)
    if games:
        counts["game"] = games
    if counts:
        audit.record(session, "migration.items_adopted", actor=owner, **counts)
        log.warning("adopted %s for %s", ", ".join(f"{n} {k}" for k, n in sorted(counts.items())), owner.handle)
    policy.set_marker(session, "orphans_adopted", {"at": _now().isoformat(), "by": owner.id, "counts": counts})
    return counts


def register_offline(kind: str, ref: str, name: str, path, owner_handle: Optional[str] = None) -> str:
    """Register an item a command-line tool just wrote (``citar lab submit``), owned by ``owner_handle`` or else the
    first administrator, when the server's database is there to write to; returns what was done, for the tool to
    print. The database is opened only when it already exists with this version's tables: a tool never creates,
    migrates or renames one, which is the server's business at its start. Anything not registered here is adopted
    at the server's next start (``adopt_orphans``, to the owner its file records)."""
    from sqlalchemy import inspect
    from . import settings
    try:
        if not db.configured():
            url = settings.get().db_url
            if url.startswith("sqlite"):
                file = url.split("///", 1)[-1]
                if not file or file == ":memory:" or not Path(file).exists():
                    return "not registered: there is no server database here yet (the server adopts it when it starts)"
            db.configure(url)
        if "owned_items" not in inspect(db.engine()).get_table_names():
            return "not registered: the server database is from another version (the server adopts it when it starts)"
        with db.session() as s:
            owner = None
            if owner_handle:
                from .auth import accounts
                owner = accounts.by_handle(s, owner_handle)
                if owner is None:
                    return f"not registered: no account '{owner_handle}'"
            owner = owner or first_admin(s)
            if owner is None:
                return "not registered: the server has no administrator yet (it adopts this once one is made)"
            register(s, kind, ref, owner, name, path=path)
            return f"registered to {owner.handle}"
    except Exception as e:      # the file is written; registering it is a convenience the next start repeats
        log.warning("could not register %s %s", kind, ref, exc_info=True)
        return f"not registered ({type(e).__name__}: {e}); the server adopts it when it starts"


def on_boot() -> Optional[dict]:
    """At the server's start, once the runners are back: the system items' rows, then adoption when there is an
    administrator (a fresh server-mode database has none until ``create-admin`` or the setup flow makes one, which
    adopts through ``accounts.on_first_admin``). Returns what was adopted, None when nobody could adopt."""
    if not db.configured():
        return None
    with db.session() as s:
        register_system(s)
        admin = first_admin(s)
        if admin is None:
            log.info("No administrator yet: item files without an owner stay unreachable until one is made.")
            return None
        return adopt_orphans(s, admin)
