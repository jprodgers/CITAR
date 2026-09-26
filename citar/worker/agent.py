"""The CITAR worker agent: serves a local model to a CITAR server over an outbound connection.

Run it on the machine that has the GPU:

    python -m citar.worker --server https://citar.example.com --token CODE

It dials out, so nothing at home needs a port forward, a firewall hole or a static address. The
only thing the server ever learns is what this agent chooses to tell it.

Three properties are the point of running an agent rather than exposing an endpoint:

*Keys stay home.* If this worker fronts a paid API, the key is read from the local environment and
never leaves the machine. The server knows only that a model exists and what it costs per token.

*The owner has the last word.* Quiet hours and the concurrency limit are enforced here, locally. The
server does its own admission control first, but a bug there cannot make this machine run at 3am —
this process simply answers "closed".

*Failure is local and recoverable.* A dropped connection reconnects with backoff; a server restart
is invisible; an in-flight request that dies with the socket is reported rather than silently lost.
"""
from __future__ import annotations

import asyncio
import logging
import platform
import random
import signal
import socket
import time
from dataclasses import dataclass, field
from datetime import datetime
from typing import Optional

from . import protocol as P

log = logging.getLogger("citar.worker")

WORKER_VERSION = "1.0"
#: Reconnect backoff: quick at first so a server restart is barely noticed, then backing off so a
#: server that is genuinely down is not hammered by every worker its owner runs.
BACKOFF_START = 2.0
BACKOFF_MAX = 60.0

#: The request parameters a server may set. Where to connect, which provider and which key are this
#: machine's settings; a server that could pass `base_url` or `api_key_env` in params could point the
#: helper at another host on the owner's network, or have it send the owner's API key somewhere else.
SERVER_PARAMS = ("temperature", "max_tokens", "reasoning_effort", "top_p", "seed")


#: Where Linux distributions keep their CA bundle. A frozen helper carries its own OpenSSL, which only
#: looks where the build machine kept it (Ubuntu's /usr/lib/ssl); Fedora, Arch and friends keep it
#: elsewhere, and there the helper failed every TLS handshake.
SYSTEM_CA_BUNDLES = ("/etc/ssl/certs/ca-certificates.crt", "/etc/pki/tls/certs/ca-bundle.crt",
                     "/etc/ssl/ca-bundle.pem", "/etc/ssl/cert.pem", "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem")


def tls_context():
    """A verifying TLS context that finds CA certificates wherever this machine keeps them.

    The platform defaults (which honour SSL_CERT_FILE and SSL_CERT_DIR), plus the certifi bundle that
    ships with the helper, plus the usual Linux system bundles. Verification is never relaxed; there
    are just more places to find the roots.
    """
    import os
    import ssl
    ctx = ssl.create_default_context()
    extra = list(SYSTEM_CA_BUNDLES)
    try:
        import certifi
        extra.insert(0, certifi.where())
    except ImportError:
        pass
    for path in extra:
        if os.path.isfile(path):
            try:
                ctx.load_verify_locations(cafile=path)
            except (OSError, ssl.SSLError):
                pass
    return ctx


class Rejected(Exception):
    """The server answered the hello with an error: a refused token or an incompatible protocol.

    The one failure that retrying cannot fix. Everything else - connection refused while the server
    restarts, a timeout, a dropped socket - is worth trying again, and is.
    """


@dataclass
class WorkerConfig:
    """Everything the worker needs: where to connect, with what token, and what to serve."""
    server_url: str
    token: str
    base_url: str = "http://localhost:1234/v1"
    provider: str = "lmstudio"
    api_key_env: str = ""
    max_concurrent: int = 1
    name: str = ""
    #: Local quiet hours as (weekday, start_min, end_min) in THIS machine's zone. Enforced here so
    #: the owner's rule holds even if the server forgets it.
    quiet_hours: list = field(default_factory=list)
    models: Optional[list] = None
    insecure: bool = False
    collect_hardware: bool = True

    @property
    def ws_url(self) -> str:
        """The WebSocket URL derived from the server URL.

        ``https`` becomes ``wss``, and the path is appended. Worth doing here rather than asking for it:
        the URL people know is the one they type in a browser.
        """
        url = self.server_url.rstrip("/")
        if url.startswith("https://"):
            url = "wss://" + url[len("https://"):]
        elif url.startswith("http://"):
            url = "ws://" + url[len("http://"):]
        return url + "/ws/worker"


class Worker:
    """The outbound agent: dials a CITAR server and serves model requests down that connection.

    Outbound because the machine with the GPU is usually the one that cannot be exposed - behind a home
    router, on a changing address. Reversing the direction removes port forwarding from the problem
    entirely.

    It reconnects with backoff, because a home connection drops and the right response is to come back
    rather than to stay down.
    """
    def __init__(self, config: WorkerConfig):
        self.cfg = config
        self.hostname = config.name or socket.gethostname()
        self.running = True
        self.in_flight = 0
        self._lock = asyncio.Lock()
        self.server_id: Optional[str] = None
        self.server_name = ""
        self._tasks: set = set()

    # ------------------------------------------------------------------ local model discovery
    async def discover_models(self) -> list:
        """Ask the local endpoint what it can serve.

        A configured list wins, because somebody may want to share one model out of twenty rather
        than everything that happens to be installed.
        """
        if self.cfg.models:
            return [P.ModelInfo(key=m, label=m).client() for m in self.cfg.models]
        try:
            import httpx
            base = self.cfg.base_url.rstrip("/")
            async with httpx.AsyncClient(timeout=10) as client:
                # LM Studio's richer endpoint reports load state and context length; fall back to
                # the standard one for Ollama, vLLM, llama.cpp and anything else OpenAI-shaped.
                root = base[:-3] if base.endswith("/v1") else base
                try:
                    response = await client.get(f"{root}/api/v0/models")
                    if response.status_code == 200:
                        return [P.ModelInfo(key=m["id"], label=m.get("id"),
                                            context=m.get("max_context_length"),
                                            loaded=(m.get("state") == "loaded")).client()
                                for m in response.json().get("data", [])]
                except Exception:
                    pass
                response = await client.get(f"{base}/models")
                response.raise_for_status()
                return [P.ModelInfo(key=m["id"], label=m.get("id")).client()
                        for m in response.json().get("data", [])]
        except Exception as exc:
            log.warning("Could not list models from %s: %s", self.cfg.base_url, exc)
            return []

    def _hardware(self) -> Optional[dict]:
        """Collect this machine's hardware to send with the hello frame."""
        if not self.cfg.collect_hardware:
            return None
        try:
            from .. import hwinfo
            return hwinfo.collect()
        except Exception:
            log.debug("hardware collection failed", exc_info=True)
            return None

    # ------------------------------------------------------------------ local policy
    def quiet_now(self) -> bool:
        """Whether this machine's own quiet hours are in force right now."""
        if not self.cfg.quiet_hours:
            return False
        now = datetime.now()
        minute = now.hour * 60 + now.minute
        for weekday, start, end in self.cfg.quiet_hours:
            if weekday == now.weekday() and start <= minute < end:
                return True
        return False

    def serves(self, model) -> bool:
        """Whether this worker will run `model` for the server.

        A configured list is the owner's allowlist and is enforced here, not just advertised: the
        server only ever sees the list, and nothing stops it asking for something else. With no
        list, the local endpoint decides.
        """
        if not self.cfg.models:
            return True
        return model in self.cfg.models

    # ------------------------------------------------------------------ handling work
    async def handle_request(self, websocket, data: dict):
        """Run one completion request and send back the result or the failure."""
        request_id = data.get("id") or ""
        try:
            if not self.serves(data.get("model")):
                await self._refuse(websocket, request_id, "unknown_model",
                                   f"{self.hostname} does not serve {data.get('model')!r}.",
                                   retryable=False)
                return
            if self.quiet_now():
                await self._refuse(websocket, request_id, "closed",
                                   f"{self.hostname} is in its local quiet hours.")
                return
            async with self._lock:
                if self.in_flight >= self.cfg.max_concurrent:
                    await self._refuse(websocket, request_id, "busy",
                                       f"{self.hostname} is at its concurrency limit "
                                       f"({self.cfg.max_concurrent}).")
                    return
                self.in_flight += 1
            try:
                started = time.time()
                # The completion is blocking and CPU/GPU-bound, so it runs on a thread and the
                # event loop stays free to answer pings and accept a cancel.
                result = await asyncio.to_thread(self._complete, data)
                result["duration"] = round(time.time() - started, 3)
                result["id"] = request_id
                await websocket.send(P.frame(P.DONE, **result))
                log.info("completed %s in %.1fs (%d tool calls)", request_id,
                         result["duration"], len(result.get("tool_calls") or []))
            finally:
                async with self._lock:
                    self.in_flight -= 1
        except Exception as exc:
            log.exception("request %s failed", request_id)
            await self._refuse(websocket, request_id, "", str(exc), retryable=False)

    async def _refuse(self, websocket, request_id: str, refusal: str, message: str,
                      retryable: bool = True):
        """Refuse a request, telling the server why."""
        log.info("refusing %s (%s): %s", request_id, refusal or "error", message)
        await websocket.send(P.error_frame(P.Failure(id=request_id, message=message,
                                                     refusal=refusal, retryable=retryable)))

    def _complete(self, data: dict) -> dict:
        """One completion against the local endpoint. Runs on a worker thread.

        Reuses the existing OpenAI-compatible provider rather than reimplementing it: that is where
        the tool-call parsing for small local models lives — native calls, the JSON protocol mode,
        XML forms and the repair pass for malformed output. Duplicating it here would mean two
        parsers drifting apart.
        """
        from ..agents.providers.openai_provider import OpenAIConversation

        cfg = {
            "provider": self.cfg.provider,
            "base_url": self.cfg.base_url,
            "model": data.get("model"),
            "tool_mode": data.get("tool_mode") or "native",
            "timeout": data.get("timeout") or 900,
        }
        if self.cfg.api_key_env:
            cfg["api_key_env"] = self.cfg.api_key_env
        params = data.get("params")
        if isinstance(params, dict):
            cfg.update({k: params[k] for k in SERVER_PARAMS if params.get(k) is not None})

        conversation = OpenAIConversation(cfg, system="", tools=data.get("tools") or [])
        # The server owns the conversation; replace the provider's freshly built message list with
        # the one that arrived, system prompt and all.
        conversation.messages = list(data.get("messages") or [])
        conversation.request_timeout = data.get("timeout")
        try:
            step = conversation.step()
        finally:
            conversation.close()

        return {
            "text": step.text,
            "thinking": step.thinking,
            "stop_reason": step.stop_reason,
            "malformed": step.malformed,
            "tool_calls": [{"id": c.id, "name": c.name, "args": c.args} for c in step.tool_calls],
            "usage": dict(conversation.usage),
        }

    # ------------------------------------------------------------------ the connection
    async def session(self):
        """One connection attempt, from hello to disconnect."""
        import websockets

        models = await self.discover_models()
        hello = P.Hello(
            token=self.cfg.token, worker_version=WORKER_VERSION, hostname=self.hostname,
            provider=self.cfg.provider, models=models,
            max_concurrent=self.cfg.max_concurrent, hardware=self._hardware(),
            platform=f"{platform.system()} {platform.release()}")

        ssl_context = None
        if self.cfg.ws_url.startswith("wss://"):
            import ssl
            if self.cfg.insecure:
                ssl_context = ssl.create_default_context()
                ssl_context.check_hostname = False
                ssl_context.verify_mode = ssl.CERT_NONE
                log.warning("TLS verification is OFF (--insecure). Use this only against a test server.")
            else:
                ssl_context = tls_context()

        # `ssl` must be OMITTED for the library to pick its own default for a ws:// URI — passing
        # ssl=None explicitly is rejected outright — so it is only set for wss://.
        connect_kwargs = {"max_size": P.MAX_FRAME_BYTES, "ping_interval": 20, "ping_timeout": 20}
        if ssl_context is not None:
            connect_kwargs["ssl"] = ssl_context

        log.info("connecting to %s", self.cfg.ws_url)
        async with websockets.connect(self.cfg.ws_url, **connect_kwargs) as websocket:
            await websocket.send(P.hello_frame(hello))
            raw = await asyncio.wait_for(websocket.recv(), timeout=30)
            welcome = P.parse(raw)
            if welcome.get("t") == P.ERROR:
                raise Rejected(welcome.get("message") or "The server refused this worker.")
            if welcome.get("t") != P.WELCOME:
                raise RuntimeError(f"Unexpected first frame: {welcome.get('t')}")

            self.server_id = welcome.get("server_id")
            self.server_name = welcome.get("server_name") or ""
            log.info("connected as “%s” (%d model%s, %d slot%s)", self.server_name,
                     len(models), "" if len(models) == 1 else "s",
                     self.cfg.max_concurrent, "" if self.cfg.max_concurrent == 1 else "s")
            if models:
                log.info("serving: %s", ", ".join(m["key"] for m in models[:8])
                         + (" …" if len(models) > 8 else ""))
            else:
                log.warning("No models found at %s — the server will see this worker as having "
                            "nothing to run.", self.cfg.base_url)

            async for raw in websocket:
                try:
                    data = P.parse(raw)
                except P.ProtocolError as exc:
                    log.warning("bad frame: %s", exc)
                    continue
                kind = data.get("t")
                if kind == P.PING:
                    await websocket.send(P.frame(P.PONG))
                elif kind == P.REQUEST:
                    # Not awaited: a long completion must not stop this loop answering pings.
                    task = asyncio.create_task(self.handle_request(websocket, data))
                    self._tasks.add(task)
                    task.add_done_callback(self._tasks.discard)
                elif kind == P.CANCEL:
                    log.info("server cancelled %s", data.get("id"))
                elif kind == P.SHUTDOWN:
                    log.info("server asked this worker to stop: %s", data.get("reason", ""))
                    self.running = False
                    return
                else:
                    log.debug("ignoring unknown frame %r", kind)

    async def run(self):
        """Connect, serve, and keep reconnecting until stopped."""
        backoff = BACKOFF_START
        while self.running:
            try:
                await self.session()
                backoff = BACKOFF_START
                if self.running:
                    log.warning("connection closed by the server; reconnecting")
            except asyncio.CancelledError:
                raise
            except Rejected as exc:
                # A bad token will never start working. Say so plainly instead of retrying
                # every minute forever and filling the owner's log.
                log.error("The server rejected this worker: %s", exc)
                log.error("Check the token. Issue a new one on the Servers page if needed.")
                self.running = False
                return
            except Exception as exc:
                # Anything else - including "connection refused", which is what a worker sees while
                # the CITAR server restarts - is temporary. Giving up here used to strand every
                # worker after a server deploy.
                message = str(exc) or exc.__class__.__name__
                status = getattr(getattr(exc, "response", None), "status_code", None)
                if status in (401, 403):
                    log.error("The server rejected this worker (HTTP %s). Check the token.", status)
                    self.running = False
                    return
                log.warning("connection failed (%s); retrying in %.0fs", message, backoff)
            if not self.running:
                return
            # Jitter so a dozen workers restarted together do not reconnect in lockstep.
            await asyncio.sleep(backoff + random.uniform(0, backoff * 0.3))
            backoff = min(BACKOFF_MAX, backoff * 2)

    def stop(self):
        """Stop the worker."""
        self.running = False


def install_signal_handlers(worker: Worker, loop):
    """Stop cleanly on Ctrl-C and on a service stop."""
    def _stop():
        """Ask the worker to stop."""
        log.info("stopping")
        worker.stop()
        for task in asyncio.all_tasks(loop):
            task.cancel()

    for name in ("SIGINT", "SIGTERM"):
        sig = getattr(signal, name, None)
        if sig is None:
            continue
        try:
            loop.add_signal_handler(sig, _stop)
        except NotImplementedError:
            # Windows has no add_signal_handler; KeyboardInterrupt covers Ctrl-C there.
            signal.signal(sig, lambda *_: _stop())
