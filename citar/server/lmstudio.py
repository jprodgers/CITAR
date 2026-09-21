"""LM Studio helpers: model state via its native REST API, loading/unloading via the `lms` CLI (local servers only)."""
from __future__ import annotations

import shutil
import subprocess
import time
from pathlib import Path
from urllib.parse import urlparse


def api_root(base_url: str) -> str:
    """The API root of an LM Studio server."""
    root = (base_url or "").rstrip("/")
    return root[:-3] if root.endswith("/v1") else root


def models(base_url: str, timeout: float = 5) -> dict:
    """LM Studio's native model list as {id: info}; empty for other servers or when unreachable."""
    import httpx
    try:
        r = httpx.get(f"{api_root(base_url)}/api/v0/models", timeout=timeout)
        return {m["id"]: m for m in r.json().get("data", [])} if r.status_code == 200 else {}
    except Exception:
        return {}


def is_local(base_url: str) -> bool:
    """Whether this endpoint is on the machine CITAR is running on."""
    host = (urlparse(base_url or "").hostname or "").lower()
    return host in ("localhost", "127.0.0.1", "::1", "0.0.0.0")


def lms_path() -> str | None:
    """The path to the ``lms`` command-line tool, if it is installed."""
    found = shutil.which("lms")
    if found:
        return found
    for name in ("lms.exe", "lms"):
        candidate = Path.home() / ".lmstudio" / "bin" / name
        if candidate.exists():
            return str(candidate)
    return None


def _run(args: list[str], timeout: float) -> subprocess.CompletedProcess:
    """Run an ``lms`` command with a timeout and no stdin."""
    return subprocess.run(args, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=timeout)


def is_remote_model(model: str) -> bool:
    """True when LM Studio runs this model on a linked device (LM Link), e.g. another PC, rather than this machine."""
    import json
    lms = lms_path()
    if not lms:
        return False
    try:
        res = subprocess.run([lms, "ls", "--json"], capture_output=True, timeout=60)
        data = json.loads(res.stdout.decode("utf-8-sig"))
    except (OSError, ValueError, subprocess.SubprocessError):
        return False
    return any(m.get("modelKey") == model and m.get("deviceIdentifier") for m in data if isinstance(m, dict))


def unload(model: str) -> None:
    """Unload one model."""
    lms = lms_path()
    if lms:
        _run([lms, "unload", model], timeout=300)


def can_manage(base_url: str) -> bool:
    """Whether CITAR can load and unload models on this server."""
    return is_local(base_url) and lms_path() is not None


def unload_all() -> None:
    """Unload every model, freeing the GPU."""
    lms = lms_path()
    if lms:
        _run([lms, "unload", "--all"], timeout=300)


def ensure_loaded(base_url: str, model: str, context: int = 0, exclusive: bool = True, gpu: str = "") -> float:
    """Make sure `model` is loaded (with at least `context` tokens when given). With `exclusive`, every other model is
    unloaded first so a single GPU isn't shared. Returns the seconds spent loading (0 if it was already loaded)."""
    info = models(base_url).get(model)
    loaded = [i for i, m in models(base_url).items() if m.get("state") == "loaded"]
    if info and info.get("state") == "loaded" and (not context or (info.get("loaded_context_length") or 0) >= context) \
            and (not exclusive or loaded == [model]):
        return 0.0
    lms = lms_path()
    if not lms or not is_local(base_url):
        return 0.0  # can't manage this server: rely on just-in-time loading
    started = time.time()
    if exclusive:
        _run([lms, "unload", "--all"], timeout=300)
    elif info and info.get("state") == "loaded":
        _run([lms, "unload", model], timeout=300)
    args = [lms, "load", model, "-y"] + (["-c", str(int(context))] if context else []) + (["--gpu", gpu] if gpu else [])
    res = _run(args, timeout=3600)
    if res.returncode != 0:
        raise RuntimeError(f"lms load {model} failed: {(res.stderr or res.stdout).strip()[-400:]}")
    return round(time.time() - started, 1)


def tool_mode_for(base_url: str, model: str, requested: str = "auto") -> str:
    """'auto' picks native tool calls when LM Studio says the model supports them, otherwise the JSON protocol."""
    if requested and requested != "auto":
        return requested
    info = models(base_url)
    if not info:
        return "native"
    return "native" if "tool_use" in (info.get(model, {}).get("capabilities") or []) else "json"
