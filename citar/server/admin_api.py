"""HTTP API for the Servers page (server registry, keys, hardware, model loading, electricity plans) and the Reports
page (report builder, runs, generated HTML)."""
from __future__ import annotations

import threading
from pathlib import Path
from typing import Optional

from fastapi import APIRouter, Depends, HTTPException
from fastapi.responses import FileResponse, HTMLResponse
from pydantic import BaseModel

from .. import costing, hwinfo, keystore, paths, servers as S
from ..auth.deps import require_admin

# Every route on this router is administrator-only.
#
# It exposes the server registry — hardware inventory, OS and kernel versions, hostnames, cost
# periods, electricity tariffs and which key backend each server uses — plus the report builder and
# the model loader. A deployment probe found all of it readable by anonymous visitors, which is an
# information disclosure on a public site even though none of it is a credential.
#
# Declaring the requirement on the router means a route added later is protected by default rather
# than by the author remembering. Per-owner servers (Phase 3) will relax this to ownership checks;
# admin-only is right until then because the registry is still single-tenant.
router = APIRouter(dependencies=[Depends(require_admin)])


def _call(fn, *a, **kw):
    """Call a registry function, turning its errors into HTTP status codes."""
    try:
        return fn(*a, **kw)
    except KeyError as e:
        raise HTTPException(404, f"Not found: {e}")
    except (S.ServerError, ValueError) as e:
        raise HTTPException(400, str(e))


def _server(sid: str) -> dict:
    """A server from the registry, or a 404."""
    return _call(S.get, sid)


# ----------------------------------------------------------------------------- servers
@router.get("/api/servers")
def servers_list():
    """The whole server registry."""
    reg = S.load()
    return {"currency": reg["currency"], "currency_symbol": reg["currency_symbol"], "host_server_id": reg.get("host_server_id"),
            "servers": [S.public(sv, reg) for sv in reg["servers"]], "electricity_plans": reg["electricity_plans"],
            "key_backends": keystore.backends(), "restricted": S.restriction_status(),
            "anthropic_prices": S.ANTHROPIC_PRICES}


@router.get("/api/servers/new")
def servers_new(kind: str = "owned", provider: str = "lmstudio"):
    """A blank server of a kind and provider, for the editor to start from."""
    sv = S.normalize_server(S.default_server(kind if kind in S.KINDS else "owned", provider if provider in S.PROVIDERS else "none"))
    reg = S.load()
    if kind in ("owned", "leased") and reg["electricity_plans"]:
        sv["costs"][0]["electricity_plan_id"] = reg["electricity_plans"][0]["id"]
    if provider == "anthropic":
        sv["connection"]["key"] = {"backend": "keyring" if keystore.backends()["keyring"]["available"] else "env",
                                   "env": "ANTHROPIC_API_KEY"}
    return sv


@router.get("/api/servers/restricted")
def servers_restricted():
    """Which servers are inside a restricted window right now."""
    return {"restricted": S.restriction_status()}


@router.post("/api/servers")
def servers_save(body: dict):
    """Create or replace a server."""
    sv = _call(S.upsert, body)
    return S.public(sv)


@router.delete("/api/servers/{sid}")
def servers_delete(sid: str):
    """Remove a server."""
    sv = _server(sid)
    backend = sv["connection"]["key"]["backend"]
    if backend in ("keyring", "file"):
        keystore.delete(sid, backend)
    S.delete(sid)
    return {"deleted": sid}


@router.put("/api/servers-settings")
def servers_settings(body: dict):
    """Change registry-wide settings: currency, and which server is the host."""
    reg = _call(S.set_settings, body)
    return {k: reg[k] for k in ("currency", "currency_symbol", "host_server_id")}


@router.post("/api/servers/{sid}/detect")
def servers_detect(sid: str):
    """Ask a server what models it has, and fill in its catalogue."""
    return S.detect_models(_server(sid))


@router.get("/api/servers/{sid}/state")
def servers_state(sid: str):
    """What a server is doing now: which models are loaded, and how busy it is."""
    sv = _server(sid)
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
def servers_load(sid: str, body: ModelAction):
    """Load a model with a profile now (in the background; poll /state)."""
    sv = _server(sid)
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
def servers_unload(sid: str, body: Optional[ModelAction] = None):
    """Unload a model, or every model on a server."""
    sv = _server(sid)
    keys = None
    if body and body.model_id:
        m = S.model_entry(sv, body.model_id)
        keys = [m["key"]] if m else None
    S.unload_models(sv, keys)
    return {"ok": True}


@router.get("/api/servers/{sid}/loading")
def servers_loading(sid: str):
    """Progress of a load in flight."""
    return {k[1]: v for k, v in _loading.items() if k[0] == sid}


@router.post("/api/servers/{sid}/loaded-config")
def servers_loaded_config(sid: str, body: ModelAction):
    """The settings the model is loaded with right now, as a load profile (to copy an LM Studio preset into CITAR)."""
    sv = _server(sid)
    m = S.model_entry(sv, body.model_id)
    if m is None:
        raise HTTPException(404, "No such model on this server.")
    cfg = S.loaded_config(sv, m["key"])
    if cfg is None:
        raise HTTPException(409, f"{m['key']} isn't loaded right now (load it in LM Studio with your settings first), "
                                 f"or LM Studio's Python SDK isn't installed.")
    return {"config": cfg, "profile": S.profile_from_config(cfg, f"From LM Studio ({m['key'].split('/')[-1]})")}


@router.post("/api/servers/{sid}/estimate")
def servers_estimate(sid: str, body: ModelAction):
    """How much memory a model would need with a given profile, before trying it."""
    sv = _server(sid)
    m = S.model_entry(sv, body.model_id)
    if m is None:
        raise HTTPException(404, "No such model on this server.")
    return S.estimate(sv, m["key"], S.profile(m, body.profile_id))


@router.post("/api/servers/{sid}/collect")
def servers_collect(sid: str):
    """Collect this machine's hardware into a server config (the Servers page offers it for the host)."""
    sv = _server(sid)
    hw = hwinfo.collect()
    return S.public(_call(S.upsert, S.apply_hardware(sv, hw, "collected")))


@router.post("/api/servers/{sid}/hardware")
def servers_import_hardware(sid: str, body: dict):
    """Import hardware collected on another machine."""
    sv = _server(sid)
    return S.public(_call(S.upsert, _call(S.apply_hardware, sv, body, "imported")))


@router.get("/api/servers/{sid}/rates")
def servers_rates(sid: str):
    """What this server costs per hour, idle and busy."""
    sv = _server(sid)
    reg = S.load()
    return {"idle": costing.hourly_profile(sv, reg, busy_fraction=0.0), "busy": costing.hourly_profile(sv, reg, busy_fraction=1.0)}


@router.get("/api/hardware-collector/{name}")
def hardware_collector(name: str):
    """Download a hardware collector script to run on another machine."""
    if name == "collect_hardware.py":
        return FileResponse(Path(hwinfo.__file__), media_type="text/x-python", filename="collect_hardware.py")
    if name in ("collect_hardware.ps1", "collect_hardware.sh"):
        return FileResponse(paths.collectors_dir() / name, media_type="text/plain", filename=name)
    raise HTTPException(404, "Unknown collector.")


@router.get("/api/lmstudio/devices")
def lmstudio_devices():
    """The machines reachable through this computer's LM Studio."""
    return S.lmstudio_devices()


class KeyBody(BaseModel):
    """An API key and the backend to store it in."""
    backend: str
    key: Optional[str] = None
    env: Optional[str] = None


@router.post("/api/servers/{sid}/key")
def servers_key(sid: str, body: KeyBody):
    """Store (or point to) a server's API key. The key is never returned; only its status."""
    sv = _server(sid)
    if body.backend not in ("keyring", "env", "file", "none"):
        raise HTTPException(400, "Unknown key backend.")
    old = sv["connection"]["key"]["backend"]
    if body.backend in ("keyring", "file"):
        if body.key:
            _call(keystore.store, sid, body.backend, body.key)
        elif old != body.backend:
            raise HTTPException(400, "Paste the key to store it.")
    if old in ("keyring", "file") and old != body.backend:
        keystore.delete(sid, old)
    sv = dict(sv)
    sv["connection"] = dict(sv["connection"], key={"backend": body.backend, "env": (body.env or sv["connection"]["key"].get("env") or "").strip()})
    saved = _call(S.upsert, sv)
    return {"key_status": keystore.status(saved)}


@router.delete("/api/servers/{sid}/key")
def servers_key_delete(sid: str):
    """Delete a stored API key."""
    sv = _server(sid)
    backend = sv["connection"]["key"]["backend"]
    if backend in ("keyring", "file"):
        keystore.delete(sid, backend)
    return {"key_status": keystore.status(sv)}


class Passphrase(BaseModel):
    """The passphrase for the encrypted key file."""
    passphrase: str


@router.post("/api/keys/unlock")
def keys_unlock(body: Passphrase):
    """Unlock the encrypted key file for this server run."""
    _call(keystore.unlock, body.passphrase)
    return keystore.backends()


@router.post("/api/keys/lock")
def keys_lock():
    """Lock the encrypted key file again."""
    keystore.lock()
    return keystore.backends()


@router.post("/api/electricity-plans")
def plans_save(body: dict):
    """Create or replace an electricity plan."""
    return _call(S.upsert_plan, body)


@router.delete("/api/electricity-plans/{pid}")
def plans_delete(pid: str):
    """Delete an electricity plan."""
    _call(S.delete_plan, pid)
    return {"deleted": pid}


# ----------------------------------------------------------------------------- reports
_runner = None


def reports_runner():
    """The report runner, created on first use."""
    global _runner
    if _runner is None:
        from ..reports import ReportRunner
        _runner = ReportRunner()
    return _runner


@router.get("/api/reports")
def reports_list():
    """Every report, newest first."""
    return reports_runner().list()


@router.get("/api/reports/options")
def reports_options():
    """What can be reported on: activity types, servers, models, runs and experiments."""
    from ..reports import scope_options
    return scope_options()


@router.post("/api/reports")
def reports_start(body: dict):
    """Start building a report."""
    return _call(reports_runner().start, body)


@router.get("/api/reports/{rid}")
def reports_get(rid: str):
    """One report's metadata and progress."""
    r = reports_runner()
    return {"meta": _call(r.meta, rid), "spec": _call(r.spec, rid)}


@router.post("/api/reports/{rid}/rerun")
def reports_rerun(rid: str):
    """Build a fresh report from the same specification, with current data."""
    r = reports_runner()
    return _call(r.start, _call(r.spec, rid))


@router.get("/api/reports/{rid}/html")
def reports_html(rid: str):
    """A report's HTML, for viewing in place."""
    p = _call(reports_runner().html_path, rid)
    return HTMLResponse(p.read_text(encoding="utf-8"), headers={"Cache-Control": "no-cache"})


@router.get("/api/reports/{rid}/download")
def reports_download(rid: str):
    """A report as a self-contained file."""
    r = reports_runner()
    p = _call(r.html_path, rid)
    meta = r.meta(rid)
    safe = "".join(ch if ch.isalnum() or ch in "-_ " else "_" for ch in meta.get("title") or "report").strip() or "report"
    return FileResponse(p, media_type="text/html", filename=f"{safe} {rid[:15]}.html")


@router.delete("/api/reports/{rid}")
def reports_delete(rid: str):
    """Delete a report."""
    _call(reports_runner().delete, rid)
    return {"deleted": rid}
