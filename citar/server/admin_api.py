"""HTTP API for the machines' configurations: the registry's routes (a machine's catalogue, load profiles, hardware,
costs and key; model loading on the host; electricity plans and the host-wide settings). The Reports page's routes
are ``reports_api``'s.

Every machine is a row (design/phase3-features.md P3.2.8), and each route has its own gate rather than one for the
router, because what a route touches decides who may call it:

* a machine's own configuration (``GET /api/servers`` lists, ``POST /api/servers`` creates or saves, ``DELETE``,
  ``/hardware``, ``/rates``, ``/key``): its owner or an administrator (MANAGE on the row; another member's machine is
  404; the site's machines, the host and its online APIs, are administrators' whoever is named their owner). A
  machine the server dials itself (``reach = direct``) is an administrator's to create or switch to, unless its
  provider is the dry run (403 with the reason, ``pool.check_reach``); so are the ``env`` key backend, which names
  one of the server's own environment variables (``pool.check_key_backend``), the site's electricity plans in a
  machine's costs (``pool.check_plans``) and a new machine's id. ``/key`` is only for a machine the server dials: a
  helper machine has no key on the server (404). A body of the wrong shape is a 400 (``servers.normalize_server``);
* the routes that dial a machine or drive LM Studio on the CITAR host (``/detect``, ``/state``, ``/load``,
  ``/unload``, ``/loading``, ``/loaded-config``, ``/estimate``): the owner or an administrator **and** a machine the
  server dials, or the host; for anyone but an administrator, only the dry run, whose provider dials nothing. A
  helper machine's models come from its helper: these routes answer 404 for it without spawning ``lms``, loading
  LM Studio's SDK or sending a request;
* what reads the CITAR host itself (``/collect`` on the host row, ``/api/lmstudio/devices``) and what is host-wide
  (``/api/servers-settings``, ``/api/keys/*``, ``/api/electricity-plans*``): administrators; anyone else gets 404, as
  if the route were not there;
* ``/api/hardware-collector/{name}`` serves the collector script to any member: nothing of the host.

A change that takes a machine away from its helper (deleting it) drops the helper's connection once committed.
"""
from __future__ import annotations

import threading
from pathlib import Path
from typing import Optional

from fastapi import APIRouter, Body, Depends, HTTPException
from fastapi.responses import FileResponse
from pydantic import BaseModel
from sqlalchemy.orm import Session

from .. import hwinfo, keystore, paths, pool, servers as S
from ..auth import access, audit
from ..auth.deps import check_cap, get_db, require_role, require_user
from ..db.models import Server, User

router = APIRouter()

#: Administrators only, and a 404 for anyone else signed in: the host-wide routes (P3.2.8).
require_admin_hidden = require_role("admin", hide=True)


def _call(fn, *a, **kw):
    """Call a registry function, turning its errors into HTTP status codes."""
    try:
        return fn(*a, **kw)
    except KeyError as e:
        raise HTTPException(404, f"Not found: {e}")
    except pool.ReachRefused as e:
        raise HTTPException(403, str(e))
    except (S.ServerError, ValueError) as e:
        raise HTTPException(400, str(e))


def _is_admin(user: Optional[User]) -> bool:
    """An administrator in good standing: ``require_user`` already refused the rest."""
    return user is not None and user.role == "admin"


def _row(s: Session, sid: str, me: User, permission: str = access.MANAGE) -> Server:
    """A machine's row the caller has ``permission`` on, or a 404 that says nothing of whether it exists. The site's
    machines (the host, the site's online APIs: ``access.is_site_machine``) are administrators' whoever is named their
    owner (``access.on``)."""
    row = s.get(Server, sid) if isinstance(sid, str) else None
    if row is None or permission not in access.on(s, me, row):
        raise HTTPException(404, "No such server.")
    return row


def _not_a_helper(row: Server) -> None:
    """404 for a helper machine (``reach = worker``) on a route about what only a machine the server dials has: the
    server never reads a helper machine's key, its helper does."""
    if (row.reach or "worker") == "worker":
        raise HTTPException(404, "No such server.")


def _server(s: Session, sid: str, me: User) -> dict:
    """A machine its caller manages, as the registry has it (committed), or a 404."""
    _row(s, sid, me)
    sv = S.find(sid)
    if sv is None:
        raise HTTPException(404, "No such server.")
    return sv


def _dialled(s: Session, sid: str, me: User) -> dict:
    """A machine whose route dials it or drives LM Studio on the host: the owner's or an administrator's, and one
    the server dials (``direct``) or the host; for a member, only the dry run, whose provider dials nothing. 404
    otherwise (a helper machine's models are its helper's to report), before anything is spawned, loaded or sent."""
    row = _row(s, sid, me)
    if (row.reach or "worker") != "direct" and not row.is_host:
        raise HTTPException(404, "No such server.")
    sv = S.find(sid)
    if sv is None:
        raise HTTPException(404, "No such server.")
    if not _is_admin(me) and sv["connection"]["provider"] != "dryrun":
        raise HTTPException(404, "No such server.")
    return sv


def _plans_for(me: User, plans: list, machines: list) -> list:
    """The electricity plans as the caller may see them: an administrator's whole tariffs; for a member, only the
    names of those an administrator put in the member's own machines' costs (a member names none,
    ``pool.check_plans``), never what the household pays."""
    if _is_admin(me):
        return plans
    named = {c.get("electricity_plan_id") for sv in machines for c in sv.get("costs") or []}
    return [{"id": p["id"], "name": p["name"]} for p in plans if p["id"] in named]


def _backends_for(me: User) -> dict:
    """The key backends the caller may choose, without the host's paths for a member (and never ``env``, which names
    one of the server's own environment variables)."""
    backends = keystore.backends()
    if _is_admin(me):
        return backends
    return {"keyring": {k: v for k, v in backends["keyring"].items() if k in ("available", "name", "note")},
            "file": {k: v for k, v in backends["file"].items() if k in ("available", "unlocked", "note")}}


# ----------------------------------------------------------------------------- servers
@router.get("/api/servers")
def servers_list(me: User = Depends(require_user), s: Session = Depends(get_db, scope="function")):
    """The machines the caller manages (every one for an administrator), with the electricity plans and the key
    backends they may choose."""
    reg = S.load()
    if _is_admin(me):
        mine = reg["servers"]
    else:
        owned = {row.id for row in s.query(Server).filter(Server.owner_id == me.id)
                 if not access.is_site_machine(row)}
        mine = [sv for sv in reg["servers"] if sv["id"] in owned]
    listed = [S.public(sv, reg) for sv in mine]
    if not _is_admin(me):
        # a member names no electricity plan (pool.check_plans): an issue asking them to is not theirs to act on
        for sv in listed:
            sv["issues"] = [i for i in sv["issues"] if i != S.NO_PLAN_ISSUE]
    return {"currency": reg["currency"], "currency_symbol": reg["currency_symbol"],
            "host_server_id": reg.get("host_server_id"), "servers": listed,
            "electricity_plans": _plans_for(me, reg["electricity_plans"], mine), "key_backends": _backends_for(me),
            "restricted": pool.visible_restrictions(s, me), "anthropic_prices": S.ANTHROPIC_PRICES,
            "can_dial": _is_admin(me)}


@router.get("/api/servers/new")
def servers_new(kind: str = "owned", provider: str = "lmstudio", me: User = Depends(require_user)):
    """A blank server of a kind and provider, for the editor to start from. A member's machine is a helper machine
    unless it is the dry run (``reach``)."""
    check_cap(me, "register_servers")
    sv = S.normalize_server(S.default_server(kind if kind in S.KINDS else "owned", provider if provider in S.PROVIDERS else "none"))
    reg = S.load()
    if kind in ("owned", "leased") and reg["electricity_plans"] and _is_admin(me):
        # the site's plans are an administrator's to price a machine with (pool.check_plans)
        sv["costs"][0]["electricity_plan_id"] = reg["electricity_plans"][0]["id"]
    if provider == "anthropic":
        sv["connection"]["key"] = {"backend": "keyring" if keystore.backends()["keyring"]["available"] else "env",
                                   "env": "ANTHROPIC_API_KEY"}
    sv["reach"] = "direct" if _is_admin(me) or provider == "dryrun" else "worker"
    if not _is_admin(me) and sv["connection"]["key"]["backend"] == "env":
        sv["connection"]["key"] = {"backend": "none", "env": ""}
    return sv


@router.get("/api/servers/restricted")
def servers_restricted(me: User = Depends(require_user), s: Session = Depends(get_db, scope="function")):
    """Which of the machines the caller may see are inside a restricted window right now."""
    return {"restricted": pool.visible_restrictions(s, me)}


@router.post("/api/servers")
def servers_save(body: dict = Body(...), me: User = Depends(require_user), s: Session = Depends(get_db, scope="function")):
    """Create or save a machine. Saving one needs MANAGE on it (404 otherwise); creating one needs
    ``register_servers``. Only an administrator makes a machine the server dials (``reach``, by default ``direct`` for
    a new one, as the registry's always were) unless its provider is the dry run, chooses the host, names an
    environment variable for its key, prices it with the site's electricity plans, or chooses a new machine's id
    (a member's gets a new one: an id once a removed machine's would take over what still names it, its seats and its
    usage)."""
    sid = body.get("id")
    row = s.get(Server, sid) if isinstance(sid, str) and sid else None
    if row is not None:
        if access.MANAGE not in access.on(s, me, row):
            raise HTTPException(404, "No such server.")
    else:
        check_cap(me, "register_servers")
        if not _is_admin(me):
            body = {k: v for k, v in body.items() if k != "id"}
    reach = body.get("reach") if body.get("reach") in S.REACHES else None
    normalized = _call(S.normalize_server, body)
    body = {**body, "id": normalized["id"]}
    new_reach = reach or (row.reach if row is not None else "direct")
    _call(pool.check_reach, me, new_reach, normalized["connection"]["provider"])
    _call(pool.check_key_backend, me, normalized)
    _call(pool.check_plans, me, normalized, row.config if row is not None else None)
    if not _is_admin(me) and (body.get("is_host") or (body.get("is_host") is False and row is not None and row.is_host)):
        raise HTTPException(403, "Only an administrator chooses which machine is the host.")
    sv = _call(S.upsert, body, actor=me, reach=reach, session=s)
    audit.record(s, "server.updated" if row is not None else "server.created", actor=me, object_type="server",
                 object_id=sv["id"], name=sv["name"], reach=new_reach)
    s.commit()
    _settings_changed(sv["id"])
    return S.public(S.get(sv["id"]))


def _settings_changed(server_id: str) -> None:
    """A machine saved through the registry may have new slots (``connection.max_parallel``): the scheduler reads it
    again and its connected helper is sent what changed (design/phase4-helper.md P4.5.1; no wait here)."""
    from .machines_api import settings_changed
    settings_changed(server_id)


@router.delete("/api/servers/{sid}")
def servers_delete(sid: str, me: User = Depends(require_user), s: Session = Depends(get_db, scope="function")):
    """Remove a machine its caller manages: its row, then (once that is committed) its helper's connection and the key
    its stored configuration keeps (``servers.forget_key``: from what the row stores, which the registry's view of a
    helper machine does not show)."""
    from .workers import hub
    row = _row(s, sid, me)
    config, name = dict(row.config or {}), row.name
    audit.record(s, "server.deleted", actor=me, object_type="server", object_id=sid, name=name)
    S.delete(sid, session=s)
    s.commit()
    hub().drop(sid, "its machine was deleted")
    S.forget_key(sid, config)
    return {"deleted": sid}


@router.put("/api/servers-settings")
def servers_settings(body: dict = Body(...), me: User = Depends(require_admin_hidden)):
    """Change registry-wide settings: currency, and which server is the host."""
    reg = _call(S.set_settings, body)
    return {k: reg[k] for k in ("currency", "currency_symbol", "host_server_id")}


@router.post("/api/servers/{sid}/detect")
def servers_detect(sid: str, me: User = Depends(require_user), s: Session = Depends(get_db, scope="function")):
    """Ask a server what models it has, and fill in its catalogue."""
    return S.detect_models(_dialled(s, sid, me))


@router.get("/api/servers/{sid}/state")
def servers_state(sid: str, me: User = Depends(require_user), s: Session = Depends(get_db, scope="function")):
    """What a server is doing now: which models are loaded, and how busy it is."""
    sv = _dialled(s, sid, me)
    if sv["connection"]["provider"] != "lmstudio":
        return {"supported": False}
    out = {"supported": True, "models": {}}
    ctx: dict = {}
    for m in sv["models"]:
        out["models"][m["key"]] = S.loaded_state(sv, m["key"], ctx)
    return out


class ModelAction(BaseModel):
    """A request to load or unload a model, with an optional profile."""
    model_id: str
    profile_id: Optional[str] = None


_loading: dict = {}


@router.post("/api/servers/{sid}/load")
def servers_load(sid: str, body: ModelAction, me: User = Depends(require_user), s: Session = Depends(get_db, scope="function")):
    """Load a model with a profile now (in the background; poll /state)."""
    sv = _dialled(s, sid, me)
    m = S.model_entry(sv, body.model_id)
    if m is None:
        raise HTTPException(404, "No such model on this server.")
    if S.restricted(sv):
        raise HTTPException(409, f"{sv['name']} is in its restricted hours.")

    def work():
        """Do the loading, reporting progress as it goes."""
        _loading[(sid, m["key"])] = {"status": "loading"}
        try:
            sv2 = dict(sv, connection=dict(sv["connection"], manage_loading=True))
            secs = S.ensure_model(sv2, m["key"], S.profile(m, body.profile_id))
            _loading[(sid, m["key"])] = {"status": "done", "seconds": secs}
        except Exception as e:
            _loading[(sid, m["key"])] = {"status": "failed", "error": str(e)}
    threading.Thread(target=work, daemon=True).start()
    return {"started": True}


@router.post("/api/servers/{sid}/unload")
def servers_unload(sid: str, body: Optional[ModelAction] = None, me: User = Depends(require_user),
                   s: Session = Depends(get_db, scope="function")):
    """Unload a model, or every model on a server."""
    sv = _dialled(s, sid, me)
    keys = None
    if body and body.model_id:
        m = S.model_entry(sv, body.model_id)
        keys = [m["key"]] if m else None
    S.unload_models(sv, keys)
    return {"ok": True}


@router.get("/api/servers/{sid}/loading")
def servers_loading(sid: str, me: User = Depends(require_user), s: Session = Depends(get_db, scope="function")):
    """Progress of a load in flight."""
    _dialled(s, sid, me)
    return {k[1]: v for k, v in _loading.items() if k[0] == sid}


@router.post("/api/servers/{sid}/loaded-config")
def servers_loaded_config(sid: str, body: ModelAction, me: User = Depends(require_user),
                          s: Session = Depends(get_db, scope="function")):
    """The settings the model is loaded with right now, as a load profile (to copy an LM Studio preset into CITAR)."""
    sv = _dialled(s, sid, me)
    m = S.model_entry(sv, body.model_id)
    if m is None:
        raise HTTPException(404, "No such model on this server.")
    cfg = S.loaded_config(sv, m["key"])
    if cfg is None:
        raise HTTPException(409, f"{m['key']} isn't loaded right now (load it in LM Studio with your settings first), "
                                 f"or LM Studio's Python SDK isn't installed.")
    return {"config": cfg, "profile": S.profile_from_config(cfg, f"From LM Studio ({m['key'].split('/')[-1]})")}


@router.post("/api/servers/{sid}/estimate")
def servers_estimate(sid: str, body: ModelAction, me: User = Depends(require_user), s: Session = Depends(get_db, scope="function")):
    """How much memory a model would need with a given profile, before trying it."""
    sv = _dialled(s, sid, me)
    m = S.model_entry(sv, body.model_id)
    if m is None:
        raise HTTPException(404, "No such model on this server.")
    return S.estimate(sv, m["key"], S.profile(m, body.profile_id))


@router.post("/api/servers/{sid}/collect")
def servers_collect(sid: str, me: User = Depends(require_admin_hidden), s: Session = Depends(get_db, scope="function")):
    """Collect the CITAR host's hardware into its row: only the host's, since it reads the computer CITAR runs on."""
    row = _row(s, sid, me)
    if not row.is_host:
        raise HTTPException(404, "No such server.")
    sv = _server(s, sid, me)
    hw = hwinfo.collect()
    saved = _call(S.upsert, S.apply_hardware(sv, hw, "collected"), actor=me)
    _settings_changed(saved["id"])
    return S.public(S.get(saved["id"]))


@router.post("/api/servers/{sid}/hardware")
def servers_import_hardware(sid: str, body: dict = Body(...), me: User = Depends(require_user),
                            s: Session = Depends(get_db, scope="function")):
    """Import hardware collected on another machine (the collector script's file) into a machine's configuration."""
    sv = _server(s, sid, me)
    saved = _call(S.upsert, _call(S.apply_hardware, sv, body, "imported"), actor=me)
    _settings_changed(saved["id"])
    return S.public(S.get(saved["id"]))


@router.get("/api/servers/{sid}/rates")
def servers_rates(sid: str, me: User = Depends(require_user), s: Session = Depends(get_db, scope="function")):
    """What this server costs per hour, idle and busy."""
    sv = _server(s, sid, me)
    return pool.machine_rates(sv, S.load(), me)


@router.get("/api/hardware-collector/{name}")
def hardware_collector(name: str, me: User = Depends(require_user)):
    """Download a hardware collector script to run on another machine."""
    if name == "collect_hardware.py":
        return FileResponse(Path(hwinfo.__file__), media_type="text/x-python", filename="collect_hardware.py")
    if name in ("collect_hardware.ps1", "collect_hardware.sh"):
        return FileResponse(paths.collectors_dir() / name, media_type="text/plain", filename=name)
    raise HTTPException(404, "Unknown collector.")


@router.get("/api/lmstudio/devices")
def lmstudio_devices(me: User = Depends(require_admin_hidden)):
    """The machines reachable through this computer's LM Studio."""
    return S.lmstudio_devices()


class KeyBody(BaseModel):
    """An API key and the backend to store it in."""
    backend: str
    key: Optional[str] = None
    env: Optional[str] = None


@router.post("/api/servers/{sid}/key")
def servers_key(sid: str, body: KeyBody, me: User = Depends(require_user), s: Session = Depends(get_db, scope="function")):
    """Store (or point to) the API key of a machine the server dials. The key is never returned; only its status. The
    ``env`` backend names one of the server's own environment variables, so only an administrator may choose it. A
    helper machine has no key on the server (its helper holds whatever its model server needs): 404."""
    row = _row(s, sid, me)
    _not_a_helper(row)
    sv = _server(s, sid, me)
    if body.backend not in ("keyring", "env", "file", "none"):
        raise HTTPException(400, "Unknown key backend.")
    if body.backend == "env" and not _is_admin(me):
        raise HTTPException(403, "Only an administrator can point a machine's key at one of the server's environment "
                                 "variables. Store the key itself instead.")
    stored = S.stored_key(row.config)
    old = stored.get("backend") or "none"
    if body.backend in ("keyring", "file"):
        if body.key:
            _call(keystore.store, sid, body.backend, body.key)
        elif old != body.backend:
            raise HTTPException(400, "Paste the key to store it.")
    if old in ("keyring", "file") and old != body.backend:
        keystore.delete(sid, old)
    sv = dict(sv)
    env = (body.env or stored.get("env") or "").strip() if body.backend == "env" else ""
    sv["connection"] = dict(sv["connection"], key={"backend": body.backend, "env": env})
    saved = _call(S.upsert, sv, actor=me, session=s)
    audit.record(s, "server.key", actor=me, object_type="server", object_id=sid, backend=body.backend)
    s.commit()
    _settings_changed(sid)
    return {"key_status": keystore.status(saved)}


@router.delete("/api/servers/{sid}/key")
def servers_key_delete(sid: str, me: User = Depends(require_user), s: Session = Depends(get_db, scope="function")):
    """Delete the stored API key of a machine the server dials (the backend its row stores names where it is). A
    helper machine has none on the server: 404."""
    row = _row(s, sid, me)
    _not_a_helper(row)
    S.forget_key(sid, row.config)
    sv = _server(s, sid, me)
    return {"key_status": keystore.status(sv)}


class Passphrase(BaseModel):
    """The passphrase for the encrypted key file."""
    passphrase: str


@router.post("/api/keys/unlock")
def keys_unlock(body: Passphrase, me: User = Depends(require_admin_hidden)):
    """Unlock the encrypted key file for this server run."""
    _call(keystore.unlock, body.passphrase)
    return keystore.backends()


@router.post("/api/keys/lock")
def keys_lock(me: User = Depends(require_admin_hidden)):
    """Lock the encrypted key file again."""
    keystore.lock()
    return keystore.backends()


@router.post("/api/electricity-plans")
def plans_save(body: dict = Body(...), me: User = Depends(require_admin_hidden)):
    """Create or replace an electricity plan."""
    return _call(S.upsert_plan, body)


@router.delete("/api/electricity-plans/{pid}")
def plans_delete(pid: str, me: User = Depends(require_admin_hidden)):
    """Delete an electricity plan."""
    _call(S.delete_plan, pid)
    return {"deleted": pid}
