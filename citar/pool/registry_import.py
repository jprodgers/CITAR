"""The one-time import of ``config/servers.json`` into the ``servers`` table (design/phase3-features.md P3.2.8).

The registry of CITAR before 0.1.6 was that file, dialled by the server and edited by administrators; since 0.1.6
every machine is a row. The import runs once, behind the ``registry_imported`` marker in ``settings_kv``:

* at the server's start when an administrator exists (``on_boot``, in a transaction of its own, before any game comes
  back, so restored seats find their machines). A fresh server-mode database has none: the boot imports nothing and
  says why in its log;
* and from ``accounts.on_first_admin``, which every path that makes an administrator calls (``admin_cli create-admin``,
  ``create-user --role admin`` and ``set-role``, the setup wizard, an invitation with the admin role, the local
  owner's creation): ``run_once`` in a savepoint of that transaction, the file renamed once it commits
  (``rename_after_commit``).

The machines come in under the file's own ids, owned by the first administrator (in local mode the local owner)
(``citar.pool.import_registry``), and the file is renamed ``servers.json.imported-<date>`` beside it: never deleted,
and never read again. With no file the first run's registry is made instead, as the file used to be on its first read
(``servers.bootstrap``: this computer as the host, its LM Link peers, the Anthropic API, the dry run).
"""
from __future__ import annotations

import json
import logging
import os
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

from sqlalchemy import event

log = logging.getLogger("citar.registry_import")

MARKER = "registry_imported"


class ImportFailed(RuntimeError):
    """The file is there and could not be read: nothing is imported, the marker stays unset, and the next start or
    administrator tries again."""


def registry_file() -> Path:
    """``config/servers.json``, where the registry of CITAR before 0.1.6 lived."""
    from .. import servers
    return servers.PATH


def read_file(path: Path) -> dict:
    """The registry in ``path``. Raises ImportFailed when it does not read."""
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise ImportFailed(f"{path} could not be read ({exc}); it is imported once it reads") from exc
    if not isinstance(data, dict) or not isinstance(data.get("servers") or [], list):
        raise ImportFailed(f"{path} is not a CITAR server registry; it is imported once it is")
    return data


def run_once(session, owner, *, data: Optional[dict] = None, source: Optional[str] = None,
             audit_row: bool = True) -> Optional[dict]:
    """Import the registry into ``session``'s transaction unless the marker says it was: ``data`` when given (the test
    suite's machines), else ``config/servers.json``, else the first run's registry. Sets the marker and writes one
    audit row (``audit_row``: the test suite's seed writes none, so every module starts with an empty log). Returns
    what was done (``file``: the file to rename once this commits, or None), or None when it had been done already.
    Raises ImportFailed or a PoolError without writing anything the caller must undo: run it in a savepoint or a
    transaction of its own."""
    from .. import pool, servers
    from ..auth import audit, policy
    if policy.marker(session, MARKER):
        return None
    path = registry_file()
    file = None
    if data is not None:
        source = source or "given"
    elif path.exists():
        data, file, source = read_file(path), path, str(path)
    else:
        data, source = servers.bootstrap(), "first run"
    result = pool.import_registry(session, owner, registry_data=data)
    record = {"at": datetime.now(timezone.utc).isoformat(timespec="seconds"), "source": source,
              "servers": result["imported"], "owner_id": owner.id if owner else None}
    policy.set_marker(session, MARKER, record)
    if audit_row:
        audit.record(session, "migration.registry_imported", actor=owner, actor_label=None if owner else "system",
                     object_type="server", count=len(result["imported"]), servers=result["imported"],
                     already_present=result["already_present"], source=source)
    session.flush()
    return {**result, **record, "file": file}


def renamed_name(path: Path) -> Path:
    """Where the imported file goes: ``servers.json.imported-<YYYYmmdd-HHMMSS>`` beside it, never over an earlier one."""
    stamp = datetime.now().strftime("%Y%m%d-%H%M%S")
    target = path.with_name(f"{path.name}.imported-{stamp}")
    n = 2
    while target.exists():
        target = path.with_name(f"{path.name}.imported-{stamp}-{n}")
        n += 1
    return target


def rename_file(path: Optional[Path]) -> Optional[Path]:
    """Rename an imported file aside (never delete it). A rename that fails (another program holds it) is logged:
    the marker is set, so the file is only never read again."""
    if path is None or not Path(path).exists():
        return None
    target = renamed_name(Path(path))
    try:
        os.replace(path, target)
    except OSError:
        log.warning("%s was imported into the database, but it could not be renamed; it is not read any more, and "
                    "can be removed or renamed by hand", path, exc_info=True)
        return None
    log.warning("The server registry %s was imported into the database (every machine is a row now) and renamed "
                "to %s.", path, target)
    return target


def rename_after_commit(session, result: Optional[dict]) -> None:
    """Rename ``result``'s file once ``session``'s outermost transaction commits (a savepoint's release, which fires
    the same event, is not that; and never when it rolls back, so the next start or administrator still finds the
    file to import)."""
    if not result or not result.get("file"):
        return
    path = result["file"]
    # the listeners stay on the session (a request's or a command's, closed soon after) and do nothing once the
    # outermost transaction ended: one cannot be removed while the event that calls it is being dispatched
    state = {"open": True}

    def committed(s):
        if state["open"] and not s.in_nested_transaction():
            state["open"] = False
            rename_file(path)

    def rolled_back(s):
        if not s.in_nested_transaction():
            state["open"] = False
    event.listen(session, "after_commit", committed)
    event.listen(session, "after_rollback", rolled_back)


def on_boot() -> Optional[dict]:
    """At the server's start, before any game comes back: import the registry when it has not been and an
    administrator exists (in local mode the owner, made just before). Never raises: a registry that will not import
    is no reason for the server not to start; the log says why, and the next start tries again. Returns what was
    imported, or None."""
    from .. import db, items
    from ..auth import policy
    try:
        with db.session() as s:
            done = policy.marker(s, MARKER)
            if done:
                if registry_file().exists():
                    log.warning("%s is not read any more: the server registry was imported into the database on %s. "
                                "Remove or rename the file.", registry_file(), (done or {}).get("at") or "an earlier start")
                return None
            owner = items.first_admin(s)
            if owner is None:
                log.warning("The server registry is not imported yet: there is no administrator to own its machines, "
                            "so no machine is usable. It is imported when the first administrator is made "
                            "(admin_cli create-admin).")
                return None
            result = run_once(s, owner)
    except Exception:
        log.warning("importing the server registry failed; no machine of it is usable, and the next start tries "
                    "again", exc_info=True)
        return None
    if result:
        rename_file(result.get("file"))
        log.info("imported %d machine(s) from %s", len(result["imported"]), result["source"])
    return result
