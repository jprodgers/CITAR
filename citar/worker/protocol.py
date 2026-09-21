"""The wire protocol between a `citar-worker` and the CITAR server.

The connection is inverted on purpose. A GPU at home sits behind NAT and cannot be dialled into, so
the worker dials **out** over a websocket and the server sends work down the connection the worker
opened. Nothing at the owner's house is exposed to the internet.

    worker                                    server (the hub)
      |  --- hello (token, catalog) ------->  |
      |  <-- welcome (server id, settings) -  |
      |                                       |
      |  <-- request (id, messages, tools) -  |
      |  --- done (id, text, tool_calls) -->  |
      |                                       |
      |  <-- ping -------------------------   |
      |  --- pong ------------------------->  |

Design points that matter:

*The worker is stateless.* Conversation state lives on the server, and every request carries the
whole message list. A worker can therefore be restarted, replaced or moved to another machine
mid-game without losing anything, and two workers for the same server are interchangeable.

*Frames are versioned and forward-compatible.* Unknown frame types are ignored rather than fatal,
and unknown fields are preserved, so an older worker keeps functioning against a newer server for
anything it already understands.

*The worker may always refuse.* It enforces its own concurrency limit and its own quiet hours
locally and answers `busy` or `closed`. The server's admission control is the first line, but the
machine's owner has the last word — a bug on the server can never run somebody's GPU at 3am.
"""
from __future__ import annotations

import json
import time
from dataclasses import asdict, dataclass, field
from typing import Any, Optional

#: Bumped when a change is not backward compatible. The server refuses a major it does not know.
PROTOCOL_VERSION = 1

#: worker -> server
HELLO = "hello"
CATALOG = "catalog"
DONE = "done"
CHUNK = "chunk"
ERROR = "error"
PONG = "pong"
STATUS = "status"

#: server -> worker
WELCOME = "welcome"
REQUEST = "request"
CANCEL = "cancel"
PING = "ping"
SHUTDOWN = "shutdown"

#: Why a worker refused a request. These are not failures of the request itself, and the server
#: should try elsewhere or queue rather than surfacing them as an error to the player.
REFUSALS = ("busy", "closed", "unknown_model", "stopping")

MAX_FRAME_BYTES = 8 * 1024 * 1024      # a long game's message list, with headroom


class ProtocolError(ValueError):
    """A frame that cannot be understood. Closes the connection."""


def frame(kind: str, **fields) -> str:
    """Encode one frame. Compact separators because message lists get large."""
    payload = {"t": kind, "ts": round(time.time(), 3), **fields}
    return json.dumps(payload, separators=(",", ":"), default=str)


def parse(raw: str) -> dict:
    """Decode a frame, rejecting anything that is not a JSON object with a type."""
    if len(raw) > MAX_FRAME_BYTES:
        raise ProtocolError(f"Frame is larger than {MAX_FRAME_BYTES} bytes.")
    try:
        data = json.loads(raw)
    except (ValueError, TypeError) as exc:
        raise ProtocolError(f"Frame is not valid JSON: {exc}") from exc
    if not isinstance(data, dict):
        raise ProtocolError("Frame is not a JSON object.")
    if not data.get("t"):
        raise ProtocolError("Frame has no type.")
    return data


# ---------------------------------------------------------------------------- payloads

@dataclass
class ModelInfo:
    """One model a worker can serve."""
    key: str
    label: str = ""
    context: Optional[int] = None
    loaded: bool = False
    #: For API-backed workers, so the server can price tokens without holding the key.
    pricing: Optional[dict] = None

    def client(self) -> dict:
        """The model as the server records it."""
        return {k: v for k, v in asdict(self).items() if v is not None}


@dataclass
class Hello:
    """The worker's opening frame: who it is, and what it can serve."""
    token: str
    version: int = PROTOCOL_VERSION
    worker_version: str = ""
    hostname: str = ""
    provider: str = ""
    models: list = field(default_factory=list)
    max_concurrent: int = 1
    #: Hardware as collected by citar/hwinfo.py, so the server can cost the machine without the
    #: owner typing it in twice.
    hardware: Optional[dict] = None
    platform: str = ""


@dataclass
class Welcome:
    """The server's reply: accepted, with the settings the worker should use."""
    server_id: str
    server_name: str
    ping_seconds: int = 20
    #: Echoed back so the worker can warn when it is older than the server expects.
    protocol_version: int = PROTOCOL_VERSION


@dataclass
class Request:
    """One completion. Carries everything needed — the worker holds no state between requests."""
    id: str
    model: str
    messages: list
    tools: list = field(default_factory=list)
    tool_mode: str = "native"
    params: dict = field(default_factory=dict)
    #: Seconds the worker may spend. It must answer with an error rather than exceed this, so the
    #: turn driver's own deadline is never the thing that discovers the request is stuck.
    timeout: float = 900.0
    activity: str = ""


@dataclass
class Done:
    """A completed request and its result."""
    id: str
    text: str = ""
    thinking: str = ""
    tool_calls: list = field(default_factory=list)
    stop_reason: str = ""
    malformed: int = 0
    usage: dict = field(default_factory=dict)
    #: Seconds the model actually spent generating, for the usage ledger.
    duration: float = 0.0


@dataclass
class Failure:
    """A request that failed, and why."""
    id: str
    message: str
    #: One of REFUSALS, or "" for a genuine error.
    refusal: str = ""
    retryable: bool = False


def hello_frame(h: Hello) -> str:
    """Encode a hello frame."""
    return frame(HELLO, **asdict(h))


def welcome_frame(w: Welcome) -> str:
    """Encode a welcome frame."""
    return frame(WELCOME, **asdict(w))


def request_frame(r: Request) -> str:
    """Encode a request frame."""
    return frame(REQUEST, **asdict(r))


def done_frame(d: Done) -> str:
    """Encode a completion frame."""
    return frame(DONE, **asdict(d))


def error_frame(f: Failure) -> str:
    """Encode a failure frame."""
    return frame(ERROR, **asdict(f))


def redact(data: Any) -> Any:
    """Strip anything secret before a frame is logged.

    Worker tokens travel in `hello`, and a debug log is not a place for a live credential.
    """
    if isinstance(data, dict):
        return {k: ("<redacted>" if k in ("token", "api_key", "authorization", "key") else redact(v))
                for k, v in data.items()}
    if isinstance(data, list):
        return [redact(v) for v in data]
    if isinstance(data, str) and len(data) > 300:
        return data[:300] + f"… ({len(data)} chars)"
    return data


def summarize(data: dict) -> str:
    """A one-line description of a frame, for logs."""
    kind = data.get("t", "?")
    if kind == REQUEST:
        return f"request {data.get('id')} model={data.get('model')} messages={len(data.get('messages') or [])}"
    if kind == DONE:
        usage = data.get("usage") or {}
        return (f"done {data.get('id')} tools={len(data.get('tool_calls') or [])} "
                f"in={usage.get('input_tokens', 0)} out={usage.get('output_tokens', 0)}")
    if kind == ERROR:
        return f"error {data.get('id')} refusal={data.get('refusal') or '-'}: {data.get('message', '')[:120]}"
    if kind == HELLO:
        return f"hello v{data.get('version')} models={len(data.get('models') or [])} host={data.get('hostname')}"
    return kind
