"""The helper update relay (design/phase4-helper.md P4.5.9): the signed manifest of the CITAR helper of this server's
own version, fetched off any request path, verified, and handed to the helpers this server serves.

- **Where from:** ``settings.helper_manifest_file`` (a manifest on disk, its signature beside it as ``<file>.sig``: the
  tests' and an offline server's), else ``settings.helper_manifest_url`` (``{version}`` this server's version; its
  signature at the same address with ``.sig``; by default GitHub's release download in server mode, none in local
  mode). Neither: nothing is relayed, and helpers report updates as their own build allows.
- **When:** at start and then at most once an hour (``FETCH_SECONDS``), on a thread of its own (``Relay.start``),
  never on a request path. A fetch that fails, or a manifest that does not verify, keeps the last good one; either is
  logged at most once an hour. **A build that trusts no update key fetches nothing** (4-13's fix round): no manifest
  could verify, so it neither asks GitHub hourly nor warns hourly; it says so once, at INFO.
- **What:** only a manifest that verifies against the update keys the extension embeds (``engine_api.verify_manifest``:
  the helper's own ``update-keys.pub``; a build with the test operations also trusts ``CITAR_HELPER_UPDATE_KEY``, so the
  suite can use a throwaway key and no shipped wheel can be fooled by the environment), whose protocol is the hub's
  and whose engine build is this server's (helpers follow their server, P4.3.8: a server built from changed sources
  never moves its helpers onto a build it does not run), is relayed. Phase 4 ships no key, so a release server relays
  nothing until the owner's Phase 5 key is in.
- **To whom:** every welcome carries the current manifest (``welcome.update``: the helper decides what it means for
  it); a newly accepted one goes as an ``update`` frame to each connected helper whose version is older
  (``engine_api.helper_version_newer``), at once.

The hub keeps each machine's last ``update_status`` (``WorkerHub.update_states``) beside this relay's manifest for the
Servers page (``WorkerConnection.client``).
"""
from __future__ import annotations

import logging
import threading
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional

log = logging.getLogger("citar.helper_updates")

#: The shortest time between two fetches.
FETCH_SECONDS = 3600.0
#: How often a failing fetch is logged.
FAIL_LOG_SECONDS = 3600.0
#: The largest manifest read (PROTOCOL.md: ``update.manifest`` is at most 65,536 characters), and signature.
MAX_BYTES = 65536
#: How long one download may take.
TIMEOUT_SECONDS = 30.0


class FetchFailed(Exception):
    """The manifest or its signature could not be read."""


@dataclass(frozen=True)
class Relayed:
    """A manifest that verified: its text as published and its signature (what the frames carry), and what it says."""
    manifest: str
    signature: str
    version: str
    build: str
    released_at: str
    protocol: int
    at: float

    def offer(self) -> dict:
        """``welcome.update`` and the ``update`` frame's fields."""
        return {"manifest": self.manifest, "signature": self.signature}

    def view(self) -> dict:
        """What the Servers page shows of it."""
        return {"version": self.version, "build": self.build, "released_at": self.released_at}


def _read_file(path: Path) -> str:
    try:
        raw = path.read_bytes()
    except OSError as e:
        raise FetchFailed(f"{path}: {e}") from None
    if len(raw) > MAX_BYTES:
        raise FetchFailed(f"{path} is over {MAX_BYTES} bytes")
    try:
        return raw.decode("utf-8")
    except UnicodeDecodeError:
        raise FetchFailed(f"{path} is not UTF-8") from None


def file_source(path: str) -> Callable[[], tuple[str, str]]:
    """A manifest file and its ``.sig`` beside it."""
    def read() -> tuple[str, str]:
        p = Path(path)
        return _read_file(p), _read_file(Path(str(p) + ".sig")).strip()
    return read


def _get(url: str) -> str:
    req = urllib.request.Request(url, headers={"User-Agent": "citar-server (helper update relay)"})
    try:
        # https only (or http to this machine): settings checks the address
        with urllib.request.urlopen(req, timeout=TIMEOUT_SECONDS) as r:
            raw = r.read(MAX_BYTES + 1)
    except (urllib.error.URLError, OSError, ValueError) as e:
        raise FetchFailed(f"{url}: {e}") from None
    if len(raw) > MAX_BYTES:
        raise FetchFailed(f"{url} is over {MAX_BYTES} bytes")
    try:
        return raw.decode("utf-8")
    except UnicodeDecodeError:
        raise FetchFailed(f"{url} is not UTF-8") from None


def url_source(url: str) -> Callable[[], tuple[str, str]]:
    """The manifest at ``url`` and its signature at ``url + ".sig"``."""
    def read() -> tuple[str, str]:
        return _get(url), _get(url + ".sig").strip()
    return read


def configured_source() -> Optional[Callable[[], tuple[str, str]]]:
    """Where this server's settings say the manifest is, or None."""
    from .. import __version__, settings
    s = settings.get()
    if s.helper_manifest_file:
        return file_source(s.helper_manifest_file)
    if s.helper_manifest_url:
        return url_source(s.helper_manifest_url.replace("{version}", str(__version__)))
    return None


class Relay:
    """The manifest this server relays, and the thread that fetches it (the module's doc). ``source`` reads the
    manifest's text and its signature (or raises ``FetchFailed``); ``clock`` is wall-clock seconds; ``push`` gets each
    newly accepted manifest (the hub sends it to older helpers)."""

    def __init__(self, source: Optional[Callable[[], tuple[str, str]]] = None, *,
                 clock: Callable[[], float] = time.time, push: Optional[Callable[[Relayed], None]] = None):
        self.source = source
        self.clock = clock
        self.push = push
        self._lock = threading.Lock()
        self._current: Optional[Relayed] = None
        self._last_fetch: Optional[float] = None
        self._last_logged: Optional[float] = None
        #: Fetches made, and manifests refused (for the tests and the administrators' view).
        self.fetches = 0
        self.refusals = 0
        self._stop = threading.Event()
        self._thread: Optional[threading.Thread] = None
        self._keyless_said = False

    # -------------------------------------------------------------- what is relayed
    def current(self) -> Optional[Relayed]:
        with self._lock:
            return self._current

    def offer(self) -> Optional[dict]:
        """``welcome.update``: the current manifest, or None."""
        cur = self.current()
        return cur.offer() if cur is not None else None

    def clear(self) -> None:
        """Forget the manifest (tests; a server whose source is gone keeps its last good one otherwise)."""
        with self._lock:
            self._current = None
            self._last_fetch = None

    def accept(self, manifest: str, signature: str) -> Relayed:
        """Verify ``manifest`` and make it the one relayed; pushed to older helpers when it is new. Raises ValueError
        (``engine_api.ManifestRefused``) when it does not verify, or speaks another protocol than the hub's: the last
        good one stays."""
        from .. import engine_api
        from . import helper_protocol as HP
        m = engine_api.verify_manifest(manifest, signature)
        if int(m.get("protocol") or 0) != HP.PROTOCOL:
            raise ValueError(f"the manifest is for protocol {m.get('protocol')}, and this hub speaks {HP.PROTOCOL}")
        own = str(engine_api.build_info().get("build_id") or "")
        if str(m.get("build") or "") != own:
            raise ValueError(f"the manifest is for engine build {m.get('build')}, and this server runs "
                             f"{own or 'one it cannot tell'}: its helpers follow its own build")
        relayed = Relayed(manifest=manifest, signature=signature.strip(), version=str(m["version"]),
                          build=str(m["build"]), released_at=str(m.get("released_at") or ""),
                          protocol=int(m["protocol"]), at=self.clock())
        with self._lock:
            old = self._current
            self._current = relayed
        if old is None or (old.manifest, old.signature) != (relayed.manifest, relayed.signature):
            log.info("relaying CITAR helper %s (build %s) to this server's helpers", relayed.version, relayed.build)
            if self.push is not None:
                try:
                    self.push(relayed)
                except Exception:
                    log.exception("could not send the new helper manifest to the connected helpers")
        return relayed

    # -------------------------------------------------------------- fetching
    def trusts_a_key(self) -> bool:
        """Whether the extension trusts any update key: without one no manifest could verify, so nothing is fetched
        (said once, at INFO)."""
        from .. import engine_api
        try:
            trusted = engine_api.update_keys_trusted() > 0
        except Exception:
            trusted = False
        if not trusted and not self._keyless_said:
            self._keyless_said = True
            log.info("this build trusts no helper update key: no helper manifest is fetched or relayed")
        return trusted

    def due(self, now: Optional[float] = None) -> bool:
        """Whether a fetch is due: never made, or the last at least ``FETCH_SECONDS`` ago."""
        now = self.clock() if now is None else now
        with self._lock:
            last = self._last_fetch
        return last is None or now - last >= FETCH_SECONDS or now < last

    def check(self) -> Optional[Relayed]:
        """One fetch when one is due (at most hourly): the manifest accepted, or None (none due, the fetch failed, or
        the manifest was refused: the last good one stays, and the failure is logged at most hourly)."""
        if self.source is None or not self.due() or not self.trusts_a_key():
            return None
        now = self.clock()
        with self._lock:
            self._last_fetch = now
        self.fetches += 1
        try:
            manifest, signature = self.source()
            return self.accept(manifest, signature)
        except Exception as e:
            if not isinstance(e, FetchFailed):
                self.refusals += 1
            self._log_failure(now, e)
            return None

    def _log_failure(self, now: float, e: Exception) -> None:
        if self._last_logged is not None and now - self._last_logged < FAIL_LOG_SECONDS:
            return
        self._last_logged = now
        kept = self.current()
        log.warning("could not take the helper update manifest (%s)%s", e,
                    f"; still relaying {kept.version}" if kept else "")

    # -------------------------------------------------------------- the thread
    def start(self) -> None:
        """Fetch now and then hourly on a daemon thread, which survives its own exceptions; no thread when there is
        no source, or the build trusts no update key."""
        if self.source is None or (self._thread is not None and self._thread.is_alive()) or not self.trusts_a_key():
            return
        self._stop.clear()
        self._thread = threading.Thread(target=self._loop, name="citar-helper-updates", daemon=True)
        self._thread.start()

    def stop(self, timeout: float = 5.0) -> None:
        self._stop.set()
        t = self._thread
        if t is not None:
            t.join(timeout)
        self._thread = None

    def _loop(self) -> None:
        while not self._stop.is_set():
            try:
                self.check()
            except Exception:
                log.exception("the helper update relay failed; it tries again")
            self._stop.wait(min(FETCH_SECONDS, 60.0))


_relay: Optional[Relay] = None
_relay_lock = threading.Lock()


def relay() -> Relay:
    """The process's relay: its source from the settings, its pushes to the hub's helpers."""
    global _relay
    with _relay_lock:
        if _relay is None:
            _relay = Relay(configured_source(), push=_push_to_hub)
        return _relay


def reset() -> None:
    """Forget the process's relay (tests), stopping its thread."""
    global _relay
    with _relay_lock:
        old, _relay = _relay, None
    if old is not None:
        old.stop()


def _push_to_hub(relayed: Relayed) -> None:
    from .workers import hub
    hub().push_update(relayed)
