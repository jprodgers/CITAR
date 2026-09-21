"""API key storage for servers. Keys never live in the project folder (it may sync to the cloud) and are never sent
back to the browser: the web form posts a key once, and afterwards the UI only learns whether one is stored.

Backends (chosen per server):

    keyring  the operating system's credential store via the `keyring` package: Windows Credential Manager,
             macOS Keychain, or the Linux Secret Service / KWallet. The default when available.
    env      an environment variable named in the server config (nothing is stored by CITAR).
    file     a file in the user's config directory (outside the project) encrypted with a passphrase
             (scrypt + Fernet). For headless Linux boxes without a keyring. The passphrase comes from
             CITAR_KEYS_PASSPHRASE or is entered in the web UI once per server start (kept in memory only).
"""
from __future__ import annotations

import base64
import json
import os
import sys
import threading
from pathlib import Path
from typing import Optional

SERVICE = "CITAR"
_lock = threading.Lock()
_passphrase: Optional[str] = os.environ.get("CITAR_KEYS_PASSPHRASE") or None


def backends() -> dict:
    """Which backends work on this machine, for the Servers page."""
    kr = _keyring()
    return {
        "keyring": {"available": kr is not None, "name": _keyring_name(kr),
                    "note": None if kr else "Install the 'keyring' package (pip install keyring) to use the OS credential store."},
        "env": {"available": True, "name": "Environment variable"},
        "file": {"available": _fernet_available(), "name": f"Encrypted file ({_file_path()})",
                 "unlocked": _passphrase is not None, "exists": _file_path().exists(),
                 "note": None if _fernet_available() else "Needs the 'cryptography' package."},
    }


def _keyring():
    """The keyring module, or None where there is no OS credential store."""
    try:
        import keyring
        from keyring.backends import fail
        kr = keyring.get_keyring()
        if isinstance(kr, fail.Keyring) or "null" in type(kr).__module__.lower():
            return None
        return kr
    except Exception:
        return None


def _keyring_name(kr) -> str:
    """Which credential store backend is in use, for the Servers page to report."""
    if kr is None:
        return "OS credential store (unavailable)"
    mod = type(kr).__module__.lower()
    if "windows" in mod:
        return "Windows Credential Manager"
    if "macos" in mod or "osx" in mod:
        return "macOS Keychain"
    if "secretservice" in mod or "libsecret" in mod:
        return "Secret Service (GNOME Keyring)"
    if "kwallet" in mod:
        return "KWallet"
    return type(kr).__name__


def _config_dir() -> Path:
    """The per-user configuration directory, outside the project folder."""
    if sys.platform.startswith("win"):
        base = Path(os.environ.get("APPDATA") or Path.home() / "AppData" / "Roaming")
    elif sys.platform == "darwin":
        base = Path.home() / "Library" / "Application Support"
    else:
        base = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config")
    return base / "citar"


def _file_path() -> Path:
    """Where the encrypted key file lives."""
    return Path(os.environ.get("CITAR_KEYS_FILE") or _config_dir() / "keys.enc")


def _fernet_available() -> bool:
    """Whether the encryption library is installed."""
    try:
        import cryptography  # noqa: F401
        return True
    except ImportError:
        return False


# ----------------------------------------------------------------------------- encrypted file
def _fernet(passphrase: str, salt: bytes):
    """A cipher derived from a passphrase with scrypt."""
    from cryptography.fernet import Fernet
    from cryptography.hazmat.primitives.kdf.scrypt import Scrypt
    key = Scrypt(salt=salt, length=32, n=2 ** 15, r=8, p=1).derive(passphrase.encode("utf-8"))
    return Fernet(base64.urlsafe_b64encode(key))


def _file_read(passphrase: str) -> dict:
    """Decrypt and read the key file."""
    p = _file_path()
    if not p.exists():
        return {}
    blob = json.loads(p.read_text(encoding="utf-8"))
    salt = base64.b64decode(blob["salt"])
    from cryptography.fernet import InvalidToken
    try:
        data = _fernet(passphrase, salt).decrypt(blob["data"].encode("ascii"))
    except InvalidToken:
        raise ValueError("Wrong passphrase for the encrypted key file.")
    return json.loads(data.decode("utf-8"))


def _file_write(passphrase: str, keys: dict):
    """Encrypt and write the key file."""
    p = _file_path()
    p.parent.mkdir(parents=True, exist_ok=True)
    salt = base64.b64decode(json.loads(p.read_text(encoding="utf-8"))["salt"]) if p.exists() else os.urandom(16)
    token = _fernet(passphrase, salt).encrypt(json.dumps(keys).encode("utf-8")).decode("ascii")
    tmp = p.with_suffix(".tmp")
    tmp.write_text(json.dumps({"format": "citar-keys", "salt": base64.b64encode(salt).decode("ascii"), "data": token}),
                   encoding="utf-8")
    os.replace(tmp, p)
    try:
        os.chmod(p, 0o600)
    except OSError:
        pass


def unlock(passphrase: str) -> bool:
    """Remember the file passphrase for this server process (checked against the file when one exists)."""
    global _passphrase
    if not passphrase:
        raise ValueError("Enter a passphrase.")
    if _file_path().exists():
        _file_read(passphrase)      # raises on a wrong passphrase
    _passphrase = passphrase
    return True


def lock():
    """Forget the passphrase, so the key file cannot be read again without it."""
    global _passphrase
    _passphrase = None


# ----------------------------------------------------------------------------- per-server API
def _account(server_id: str) -> str:
    """The account name a server's key is stored under."""
    return f"server:{server_id}"


def store(server_id: str, backend: str, secret: str):
    """Store a key for a server. The env backend can't store anything (set the variable yourself)."""
    secret = (secret or "").strip()
    if not secret:
        raise ValueError("The key is empty.")
    with _lock:
        if backend == "keyring":
            kr = _keyring()
            if kr is None:
                raise ValueError("No OS credential store is available here; use an environment variable or the encrypted file.")
            kr.set_password(SERVICE, _account(server_id), secret)
        elif backend == "file":
            if _passphrase is None:
                raise ValueError("Unlock the encrypted key file with its passphrase first.")
            keys = _file_read(_passphrase)
            keys[_account(server_id)] = secret
            _file_write(_passphrase, keys)
        else:
            raise ValueError("Keys for the env backend are read from the environment variable; nothing to store.")


def delete(server_id: str, backend: str):
    """Remove a stored key."""
    with _lock:
        if backend == "keyring":
            kr = _keyring()
            if kr is not None:
                try:
                    kr.delete_password(SERVICE, _account(server_id))
                except Exception:
                    pass
        elif backend == "file" and _passphrase is not None:
            keys = _file_read(_passphrase)
            if keys.pop(_account(server_id), None) is not None:
                _file_write(_passphrase, keys)


def get(server: dict) -> Optional[str]:
    """The API key for a server config (its `connection.key` block), or None."""
    k = ((server.get("connection") or {}).get("key") or {})
    backend = k.get("backend") or "none"
    try:
        if backend == "keyring":
            kr = _keyring()
            return kr.get_password(SERVICE, _account(server["id"])) if kr else None
        if backend == "env":
            return os.environ.get(k.get("env") or "") or None
        if backend == "file":
            if _passphrase is None:
                return None
            return _file_read(_passphrase).get(_account(server["id"]))
    except Exception:
        return None
    return None


def status(server: dict) -> dict:
    """What the UI may know: whether a key is present (never the key itself), plus a masked hint."""
    k = ((server.get("connection") or {}).get("key") or {})
    backend = k.get("backend") or "none"
    if backend == "none":
        return {"backend": "none", "present": False}
    key = get(server)
    out = {"backend": backend, "present": bool(key)}
    if key and len(key) > 8:
        out["hint"] = "…" + key[-4:]
    if backend == "env":
        out["env"] = k.get("env")
    if backend == "file" and _passphrase is None:
        out["locked"] = True
    return out
