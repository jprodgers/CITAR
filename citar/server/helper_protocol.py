"""Protocol 2 between the CITAR helper and this server's hub, the server's side.

``crates/citar-helper/PROTOCOL.md`` is the normative text and ``crates/citar-helper/protocol/v2/`` its fixtures; the
helper's side is ``crates/citar-helper/src/protocol``. Both sides check every frame they receive against the same
field tables, refuse a bad one with the same ``ProtocolError`` code and path, and ignore a frame type they do not
accept from that sender; ``tests/test_helper_protocol.py`` holds this module to the fixtures and to the frame table.

- ``parse_helper_frame`` / ``parse_server_frame``: a frame's text, checked (the 8 MiB cap, a 64-level depth cap,
  JSON as ``serde_json`` reads it, the type, every field) into a dict of the fields the protocol knows, with ``t``
  and ``ts``; an ``Ignored`` for a type the receiver does not accept from that sender.
- ``helper_frame`` / ``server_frame`` and the builders below them: a frame's text from its fields, checked the same
  way, stamped with ``ts``.

This replaces protocol 1 (``citar/worker/protocol.py``) when 4-05 switches the hub to protocol 2 and deletes the
Python worker; until then nothing on the server speaks it.
"""
from __future__ import annotations

import base64
import binascii
import json
import math
import re
import time
from typing import Any, NamedTuple, Optional

#: The protocol a helper speaks, ``hello.protocol``; the hub accepts exactly these.
PROTOCOL = 2
HUB_PROTOCOLS = (2,)

#: The largest frame either side accepts (bytes of UTF-8), and how deep it may nest (the frame itself is depth 1).
MAX_FRAME_BYTES = 8 * 1024 * 1024
MAX_DEPTH = 64

#: The v1 refusal's message, word for word (PROTOCOL.md, "The v1 refusal").
V1_REFUSAL_MESSAGE = ("This server runs CITAR 0.1.6, which needs the new CITAR helper (protocol 2). Download it from "
                      "the Servers page of this server, or from https://github.com/jprodgers/CITAR/releases/latest, "
                      "and run it with a token issued there.")

#: The codes the server closes a helper's socket with (PROTOCOL.md, "Close codes").
CLOSE_BAD_HELLO = 4400
CLOSE_REFUSED = 4401
CLOSE_SHUTDOWN = 4403
CLOSE_STALE = 4408
CLOSE_REPLACED = 4409
CLOSE_PROTOCOL = 4426
CLOSE_RATE = 4429

#: The protocol errors, in the order the checks run.
ERROR_CODES = ("too_large", "too_deep", "not_json", "not_object", "no_type", "protocol", "missing", "type", "value",
               "unknown")

HELPER = "helper"
SERVER = "server"


class ProtocolError(ValueError):
    """A frame that cannot be accepted: ``code`` (one of ``ERROR_CODES``) and ``path``, the offending value's place
    from the frame's root (``machine.public_key``; ``""`` for the frame itself). The receiver logs it and drops the
    frame."""

    def __init__(self, code: str, path: str, message: str):
        super().__init__(f"{code} at {path or 'the frame'}: {message}")
        self.code = code
        self.path = path
        self.message = message


class Ignored(NamedTuple):
    """A frame of a type the receiver does not accept from that sender: logged once per type, never answered."""
    t: str


# ---------------------------------------------------------------------------- reading JSON as serde_json does

_I64_MIN, _U64_MAX = -(2 ** 63), 2 ** 64 - 1
_SURROGATE = re.compile("[\ud800-\udfff]")


def _parse_int(s: str):
    """An integer literal as ``serde_json`` reads it: within -2^63..2^64-1 an integer, ``-0`` and anything beyond a
    float (so never an ``int`` of the protocol); one beyond a float's range is not JSON."""
    n = int(s)
    if n == 0 and s.startswith("-"):
        return -0.0
    if _I64_MIN <= n <= _U64_MAX:
        return n
    return _parse_float(s)


def _parse_float(s: str) -> float:
    x = float(s)
    if math.isinf(x):
        raise ValueError(f"the number {s[:40]} is too large for a 64-bit float")
    return x


def _no_constant(s: str):
    raise ValueError(f"{s} is not JSON")


def _nests_deeper_than(text: str, limit: int) -> bool:
    """The raw text's arrays and objects nest deeper than ``limit``, counted outside strings (the helper's scan)."""
    depth, in_string, escaped = 0, False, False
    for ch in text:
        if in_string:
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == '"':
                in_string = False
            continue
        if ch == '"':
            in_string = True
        elif ch in "[{":
            depth += 1
            if depth > limit:
                return True
        elif ch in "]}":
            depth = max(0, depth - 1)
    return False


def _walk(value) -> tuple[int, bool]:
    """How deep a parsed value nests, and whether any of its strings holds a lone surrogate (which ``serde_json``
    refuses and Python's ``json`` lets through)."""
    deepest, surrogate = 0, False
    stack = [(value, 1)]
    while stack:
        v, depth = stack.pop()
        if isinstance(v, dict):
            deepest = max(deepest, depth)
            for k, x in v.items():
                surrogate = surrogate or bool(_SURROGATE.search(k))
                if isinstance(x, (dict, list)):
                    stack.append((x, depth + 1))
                elif isinstance(x, str) and not surrogate:
                    surrogate = bool(_SURROGATE.search(x))
        elif isinstance(v, list):
            deepest = max(deepest, depth)
            for x in v:
                if isinstance(x, (dict, list)):
                    stack.append((x, depth + 1))
                elif isinstance(x, str) and not surrogate:
                    surrogate = bool(_SURROGATE.search(x))
        elif isinstance(v, str):
            surrogate = surrogate or bool(_SURROGATE.search(v))
    return deepest, surrogate


def _too_large(text: str) -> bool:
    """Over ``MAX_FRAME_BYTES`` bytes of UTF-8, encoding only when the character count cannot tell (a character is
    one to four bytes)."""
    if len(text) > MAX_FRAME_BYTES:
        return True
    if len(text) * 4 <= MAX_FRAME_BYTES:
        return False
    return len(text.encode("utf-8", "surrogatepass")) > MAX_FRAME_BYTES


def envelope(raw: str) -> tuple[dict, str]:
    """The checks every frame passes before its type is known: its size, its depth, JSON, an object, a type.
    Returns the object and its type; raises ``ProtocolError`` (``too_large``, ``too_deep``, ``not_json``,
    ``not_object``, ``no_type``)."""
    if not isinstance(raw, str):
        raise ProtocolError("not_json", "", "a frame is text")
    if _too_large(raw):
        raise ProtocolError("too_large", "", f"over the {MAX_FRAME_BYTES}-byte cap")
    try:
        data = json.loads(raw, parse_int=_parse_int, parse_float=_parse_float, parse_constant=_no_constant)
    except RecursionError:
        raise ProtocolError("too_deep", "", f"nests more than {MAX_DEPTH} deep") from None
    except (ValueError, TypeError) as exc:
        if _nests_deeper_than(raw, MAX_DEPTH):
            raise ProtocolError("too_deep", "", f"nests more than {MAX_DEPTH} deep") from None
        raise ProtocolError("not_json", "", str(exc)) from None
    depth, surrogate = _walk(data)
    if depth > MAX_DEPTH:
        raise ProtocolError("too_deep", "", f"nests more than {MAX_DEPTH} deep")
    if surrogate:
        raise ProtocolError("not_json", "", "a lone surrogate in a string")
    if not isinstance(data, dict):
        raise ProtocolError("not_object", "", "not a JSON object")
    t = data.get("t")
    if not isinstance(t, str) or not t:
        raise ProtocolError("no_type", "t", "no frame type")
    return data, t


# ---------------------------------------------------------------------------- the types of the field tables

def _at(path: tuple, key) -> tuple:
    return path + (key,)


def _render(path: tuple) -> str:
    return ".".join(str(p) for p in path)


def _fail(code: str, path: tuple, message: str):
    raise ProtocolError(code, _render(path), message)


def _kind(v) -> str:
    if v is None:
        return "null"
    if isinstance(v, bool):
        return "a boolean"
    if isinstance(v, float):
        return "a float"
    if isinstance(v, int):
        return "an integer"
    if isinstance(v, str):
        return "a string"
    if isinstance(v, list):
        return "an array"
    return "an object" if isinstance(v, dict) else type(v).__name__


def as_integer(v) -> Optional[int]:
    """The integer a value is, if it is one the protocol can carry (not a boolean, not beyond 64 bits)."""
    if type(v) is int and _I64_MIN <= v <= _U64_MAX:
        return v
    return None


def as_number(v) -> Optional[float]:
    """The finite number a value is, as a float, if it is one (not a boolean)."""
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        return None
    try:
        x = float(v)
    except OverflowError:
        return None
    return x if math.isfinite(x) else None


class Ty:
    """What a value must be. ``read`` checks it and returns what a receiver keeps of it."""

    def read(self, v, path: tuple):
        raise NotImplementedError


class Int(Ty):
    def __init__(self, lo: int = 0, hi: int = 2 ** 53 - 1):
        self.lo, self.hi = lo, hi

    def read(self, v, path):
        n = as_integer(v)
        if n is None:
            _fail("type", path, f"expected an int, found {_kind(v)}")
        if not self.lo <= n <= self.hi:
            _fail("value", path, f"{n} is not within {self.lo}..{self.hi}")
        return n


class Num(Ty):
    def __init__(self, lo: float = 0.0, hi: float = 1e15):
        self.lo, self.hi = lo, hi

    def read(self, v, path):
        x = as_number(v)
        if x is None:
            _fail("type", path, f"expected a number, found {_kind(v)}")
        if not self.lo <= x <= self.hi:
            _fail("value", path, f"{x} is not within {self.lo}..{self.hi}")
        return v


class Bool(Ty):
    def read(self, v, path):
        if not isinstance(v, bool):
            _fail("type", path, f"expected a boolean, found {_kind(v)}")
        return v


def _string(v, path) -> str:
    if not isinstance(v, str):
        _fail("type", path, f"expected a string, found {_kind(v)}")
    return v


class Str(Ty):
    def __init__(self, lo: int = 0, hi: Optional[int] = None):
        self.lo, self.hi = lo, hi

    def read(self, v, path):
        s = _string(v, path)
        if len(s) < self.lo or (self.hi is not None and len(s) > self.hi):
            _fail("value", path, f"a string of {len(s)} characters, not {self.lo} to {self.hi}")
        return s


_HEX = re.compile(r"[0-9a-f]*")
_ID = re.compile(r"[A-Za-z0-9_-]{1,64}")


class Hex(Ty):
    def __init__(self, n: int):
        self.n = n

    def read(self, v, path):
        s = _string(v, path)
        if len(s) != self.n or not _HEX.fullmatch(s):
            _fail("value", path, f"not {self.n} lowercase hex digits")
        return s


def is_base64(s: str, n: Optional[int] = None) -> bool:
    """Canonical standard base64 (with its padding), of exactly ``n`` bytes when given."""
    try:
        raw = base64.b64decode(s, validate=True)
    except (binascii.Error, ValueError):
        return False
    return (n is None or len(raw) == n) and base64.b64encode(raw).decode("ascii") == s


class B64(Ty):
    def __init__(self, n: int):
        self.n = n

    def read(self, v, path):
        if not is_base64(_string(v, path), self.n):
            _fail("value", path, f"not standard base64 of {self.n} bytes")
        return v


class Id(Ty):
    """A request or machine id, or (with a prefix) a job or run id."""

    def __init__(self, prefix: str = ""):
        self.prefix = prefix

    def read(self, v, path):
        s = _string(v, path)
        rest = s[len(self.prefix):] if s.startswith(self.prefix) else None
        if rest is None or not rest or len(s) > 64 or not _ID.fullmatch(rest):
            _fail("value", path, f"not an id ({self.prefix or ''}A-Z a-z 0-9 _ -, at most 64)")
        return s


class OneOf(Ty):
    def __init__(self, *values: str):
        self.values = values

    def read(self, v, path):
        s = _string(v, path)
        if s not in self.values:
            _fail("value", path, f"{s!r} is not one of {', '.join(self.values)}")
        return s


class Exactly(Ty):
    def __init__(self, k: int):
        self.k = k

    def read(self, v, path):
        if as_integer(v) != self.k:
            _fail("value" if as_number(v) is not None else "type", path, f"expected exactly {self.k}")
        return v


class Obj(Ty):
    """Any object, kept whole: data the protocol carries without reading."""

    def read(self, v, path):
        if not isinstance(v, dict):
            _fail("type", path, f"expected an object, found {_kind(v)}")
        return v


class List(Ty):
    def __init__(self, of: Ty, lo: int = 0, hi: int = 1024):
        self.of, self.lo, self.hi = of, lo, hi

    def read(self, v, path):
        if not isinstance(v, list):
            _fail("type", path, f"expected an array, found {_kind(v)}")
        if not self.lo <= len(v) <= self.hi:
            _fail("value", path, f"{len(v)} items, not {self.lo} to {self.hi}")
        return [self.of.read(x, _at(path, i)) for i, x in enumerate(v)]


class Nullable(Ty):
    def __init__(self, of: Ty):
        self.of = of

    def read(self, v, path):
        return None if v is None else self.of.read(v, path)


class Field(NamedTuple):
    name: str
    ty: Ty
    optional: bool = False


def opt(name: str, ty: Ty) -> Field:
    """A field that may be absent (when present it has its type: null is not absent)."""
    return Field(name, ty, True)


class Struct(Ty):
    """An object with these fields; a reader drops the others (``keep="drop"``), refuses them (``"refuse"``: a
    closed object) or keeps them whole (``"keep"``: an open one)."""

    def __init__(self, *fields: Field, keep: str = "drop"):
        self.fields, self.keep = fields, keep
        self.names = {f.name for f in fields}

    def read(self, v, path):
        if not isinstance(v, dict):
            _fail("type", path, f"expected an object, found {_kind(v)}")
        out = dict(v) if self.keep == "keep" else {}
        for f in self.fields:
            here = _at(path, f.name)
            if f.name not in v:
                if not f.optional:
                    _fail("missing", here, "a required field is absent")
                continue
            x = v[f.name]
            if x is None and not isinstance(f.ty, Nullable):
                _fail("type", here, "null where it is not allowed")
            out[f.name] = f.ty.read(x, here)
        if self.keep == "refuse":
            for k in v:
                if k not in self.names:
                    _fail("unknown", _at(path, k), "a field this object does not have")
        return out


def describe(ty: Ty):
    """What a type is, as JSON: ``protocol/v2/schema.json`` holds every frame's description, and both sides' tests
    compare theirs with it, so a bound written differently on one side is found even where no fixture reaches it.
    Unbounded string lengths are null. The helper's is ``check::describe``."""
    if isinstance(ty, Int):
        return {"int": [ty.lo, ty.hi]}
    if isinstance(ty, Num):
        return {"number": [ty.lo, ty.hi]}
    if isinstance(ty, Bool):
        return "bool"
    if isinstance(ty, Str):
        return {"string": [ty.lo, ty.hi]}
    if isinstance(ty, Hex):
        return {"hex": ty.n}
    if isinstance(ty, B64):
        return {"base64": ty.n}
    if isinstance(ty, Id):
        return {"": "id", "cj_": "job", "cr_": "run"}[ty.prefix]
    if isinstance(ty, OneOf):
        return {"one_of": list(ty.values)}
    if isinstance(ty, Exactly):
        return {"exactly": ty.k}
    if isinstance(ty, Obj):
        return "object"
    if isinstance(ty, List):
        return {"list": describe(ty.of), "min": ty.lo, "max": ty.hi}
    if isinstance(ty, Nullable):
        return {"nullable": describe(ty.of)}
    if isinstance(ty, Struct):
        kind = {"drop": "struct", "refuse": "closed", "keep": "open"}[ty.keep]
        return {kind: [[f.name, describe(f.ty), f.optional] for f in ty.fields]}
    raise TypeError(f"no description for {type(ty).__name__}")


def Closed(*fields: Field) -> Struct:
    """An object that refuses a field it does not have (the job claim, whose bytes are signed)."""
    return Struct(*fields, keep="refuse")


def Open(*fields: Field) -> Struct:
    """An object whose listed fields are checked and whose others are kept whole (a chat message, the params)."""
    return Struct(*fields, keep="keep")


# ---------------------------------------------------------------------------- the frames, field by field

INT = Int()
NUM = Num()
TEXT = Str(0, 4000)
SHORT = Str(0, 64)
HEX12, HEX64 = Hex(12), Hex(64)
RUN, JOB, ID = Id("cr_"), Id("cj_"), Id()
RUNS = List(RUN, 0, 1024)
KINDS_OF_WORK = ("lab_game", "ranking_game", "reference_game")
KIND = OneOf(*KINDS_OF_WORK)
KINDS = List(KIND, 0, 3)
BOTS = List(Str(1, 64), 0, 64)
SLOTS, THREADS, PERCENT, MEMORY_MB = Int(0, 16), Int(0, 1024), Int(0, 100), Int(0, 16 * 1024 * 1024)
FRACTION = Num(0.0, 1.0)
HARDNESS = OneOf("hard", "soft")
CAPPED = ("llm.enabled", "llm.slots", "cpu.enabled", "cpu.threads", "cpu.cpu_percent", "cpu.memory_mb", "cpu.work")

MODEL = Struct(Field("key", Str(1, 200)), Field("label", Str(0, 200)), Field("context", Nullable(INT)),
               Field("loaded", Bool()))
MODELS = List(MODEL, 0, 500)
LIMITS = Struct(Field("cpu", HARDNESS), Field("memory", HARDNESS))
SITE_CPU = Struct(Field("enabled", Bool()), Field("threads", THREADS), Field("cpu_percent", PERCENT),
                  Field("memory_mb", MEMORY_MB), Field("work", KINDS), Field("yield_to_llm", Bool()))
SETTINGS = Struct(Field("llm", Struct(Field("enabled", Bool()), Field("slots", SLOTS), Field("allow_swaps", Bool()))),
                  Field("cpu", SITE_CPU))
UPDATE = Struct(Field("manifest", Str(1, 65536)), Field("signature", B64(64)))

HELLO = Struct(
    Field("protocol", Exactly(2)),
    Field("token", Str(1, 512)),
    Field("helper", Struct(Field("version", Str(1, 64)), Field("build", HEX12), Field("jobs_build", Nullable(HEX12)),
                           Field("target", Str(1, 64)), Field("label", Str(0, 200)))),
    Field("machine", Struct(Field("hostname", Str(0, 255)), Field("name", Str(0, 120)), Field("os", Str(1, 32)),
                            Field("arch", Str(1, 32)), Field("public_key", B64(32)))),
    Field("llm", Struct(Field("enabled", Bool()),
                        Field("provider", Nullable(OneOf("lmstudio", "ollama", "openai_compatible", "openai"))),
                        Field("slots", SLOTS), Field("stream", Bool()), Field("models", MODELS))),
    Field("cpu", Struct(Field("enabled", Bool()),
                        Field("ceiling", Struct(Field("threads", THREADS), Field("cpu_percent", PERCENT),
                                                Field("memory_mb", MEMORY_MB), Field("work", KINDS))),
                        Field("limits", LIMITS), Field("verifiable", Bool()), Field("bots", BOTS),
                        Field("ruleset", HEX64))),
    Field("report", Struct(Field("hardware", Nullable(Obj())),
                           Field("hardware_source", OneOf("scan", "file", "none")))),
    Field("running", RUNS),
    Field("results", RUNS),
    Field("updates", OneOf("auto", "notify", "off")),
    Field("quiet_until", Nullable(NUM)),
)

WELCOME = Struct(
    Field("protocol", Exactly(2)),
    Field("machine", Struct(Field("id", ID), Field("name", Str(0, 120)))),
    Field("settings", SETTINGS),
    Field("ping_s", Int(5, 300)),
    Field("server", Struct(Field("version", Str(1, 64)), Field("build", HEX12), Field("jobs_build", Nullable(HEX12)))),
    Field("update", Nullable(UPDATE)),
    Field("jobs", Struct(Field("keep", RUNS), Field("drop", RUNS), Field("resend", RUNS))),
)

REFUSED = Struct(Field("code", OneOf("token", "key", "protocol", "rate", "build")), Field("message", TEXT),
                 opt("retry_after_s", NUM))
V1_REFUSAL = Struct(opt("id", SHORT), Field("message", TEXT), opt("refusal", SHORT), opt("retryable", Bool()))
SETTINGS_ACK = Struct(
    Field("effective", Struct(Field("llm", Struct(Field("enabled", Bool()), Field("slots", SLOTS))),
                              Field("cpu", SITE_CPU))),
    Field("capped", List(OneOf(*CAPPED), 0, 7)),
    Field("limits", LIMITS),
)
EMPTY = Struct()

REQUEST = Struct(
    Field("id", ID),
    Field("model", Str(1, 200)),
    Field("messages", List(Open(Field("role", Str(1, 32))), 1, 10000)),
    Field("tools", Nullable(List(Obj(), 0, 512))),
    Field("params", Open(opt("temperature", Num(0.0, 2.0)), opt("max_tokens", Int(1, 1048576)),
                         opt("top_p", FRACTION), opt("seed", INT), opt("reasoning_effort", Str(1, 32)))),
    Field("timeout_s", Num(1.0, 86400.0)),
)
TOOL_CALL = Struct(Field("id", Str(0, 200)), Field("type", Str(0, 32)),
                   Field("function", Struct(Field("name", Str(0, 200)), Field("arguments", Str()))))
DONE = Struct(
    Field("id", ID),
    Field("message", Struct(Field("content", Nullable(Str())), Field("reasoning", Nullable(Str())),
                            Field("tool_calls", List(TOOL_CALL, 0, 512)))),
    Field("finish_reason", Nullable(Str(0, 64))),
    Field("usage", Nullable(Struct(Field("prompt_tokens", INT), Field("completion_tokens", INT),
                                   Field("reasoning_tokens", Nullable(INT)), Field("cached_tokens", Nullable(INT))))),
    Field("gen_s", NUM),
    Field("ttft_s", Nullable(NUM)),
    Field("streamed", Bool()),
)
REQUEST_ERROR = Struct(
    Field("id", ID),
    Field("code", OneOf("busy", "closed", "unknown_model", "stopping", "endpoint", "timeout", "bad_request")),
    Field("message", TEXT), Field("retryable", Bool()))

JOB_OFFER = Struct(
    Field("job", JOB), Field("run", RUN), Field("kind", KIND), Field("spec", Obj()), Field("spec_hash", HEX64),
    Field("jobs_build", HEX12), Field("deadline_s", Num(1.0, 86400.0)), Field("mem_mb", MEMORY_MB),
    Field("priority", Int(-100, 100)), Field("offer_ttl_s", Num(1.0, 3600.0)))
RUN_REF = Struct(Field("run", RUN))
JOB_REJECT = Struct(Field("run", RUN), Field("reason", OneOf("busy", "build", "kind", "quiet", "disabled", "limits",
                                                             "memory", "bad_spec", "unverifiable")))
JOB_PROGRESS = Struct(Field("run", RUN), Field("round", INT), Field("of", INT), Field("elapsed_s", NUM),
                      Field("cpu_s", NUM), Field("paused_s", NUM), Field("peak_mb", NUM))
CLAIM = Closed(
    Field("v", Exactly(1)), Field("job", JOB), Field("run", RUN), Field("machine", ID), Field("kind", KIND),
    Field("spec_hash", HEX64), Field("jobs_build", HEX12), Field("bots", BOTS), Field("result_hash", HEX64),
    Field("chain_head", HEX64), Field("rounds", INT), Field("key", B64(32)))
JOB_RESULT = Struct(
    Field("run", RUN),
    Field("outcome", Struct(Field("result", Obj()), Field("result_hash", HEX64),
                            Field("chain", Struct(Field("head", HEX64), Field("rounds", INT),
                                                  Field("digests", List(HEX64, 0, 10000)))),
                            Field("cpu_s", NUM), Field("wall_s", NUM), Field("paused_s", NUM), Field("peak_mb", NUM))),
    Field("claim", CLAIM),
    Field("signature", B64(64)),
)
JOB_FAILED = Struct(Field("run", RUN),
                    Field("code", OneOf("memory", "deadline", "host", "aborted_locally", "internal")),
                    Field("message", TEXT), Field("retryable", Bool()))

#: What each side sends: its frame types and their fields (PROTOCOL.md, "The frame table").
HELPER_FRAMES: dict[str, Struct] = {
    "hello": HELLO,
    "settings_ack": SETTINGS_ACK,
    "catalog": Struct(Field("models", MODELS)),
    "hardware": Struct(Field("scan", Obj()), Field("source", OneOf("scan", "file"))),
    "power": Struct(Field("samples", List(Struct(Field("t", NUM), Field("gpu_w", Nullable(NUM)),
                                                 Field("cpu_util", Nullable(FRACTION))), 1, 360))),
    "status": Struct(Field("quiet_until", Nullable(NUM)), Field("llm_in_flight", Int(0, 64)),
                     Field("jobs_running", THREADS), Field("jobs_paused", THREADS)),
    "done": DONE,
    "error": REQUEST_ERROR,
    "cancelled": Struct(Field("id", ID), Field("gen_s", NUM), Field("tokens_out", INT)),
    "job_accept": RUN_REF,
    "job_reject": JOB_REJECT,
    "job_progress": JOB_PROGRESS,
    "job_result": JOB_RESULT,
    "job_failed": JOB_FAILED,
    "job_aborted": RUN_REF,
    "update_status": Struct(Field("state", OneOf("available", "downloading", "verified", "restarting", "applied",
                                                 "failed", "refused", "unavailable")),
                            Field("version", Nullable(SHORT)), Field("message", TEXT)),
    "ping": EMPTY,
    "pong": EMPTY,
}
SERVER_FRAMES: dict[str, Struct] = {
    "welcome": WELCOME,
    "refused": REFUSED,
    "error": V1_REFUSAL,
    "settings": SETTINGS,
    "rescan": EMPTY,
    "request": REQUEST,
    "cancel": Struct(Field("id", ID)),
    "job_offer": JOB_OFFER,
    "job_result_ack": RUN_REF,
    "job_abort": Struct(Field("run", RUN), Field("reason", Str(0, 200))),
    "jobs_pause": Struct(Field("reason", Str(0, 200))),
    "jobs_resume": EMPTY,
    "update": UPDATE,
    "ping": EMPTY,
    "pong": EMPTY,
    "shutdown": Struct(Field("reason", TEXT), Field("code", OneOf("revoked", "disabled", "deleted", "suspended",
                                                                  "replaced"))),
}
FRAMES = {HELPER: HELPER_FRAMES, SERVER: SERVER_FRAMES}


def schema() -> dict:
    """Every frame type each side sends, described: what ``protocol/v2/schema.json`` holds."""
    return {sender: {t: describe(spec) for t, spec in frames.items()} for sender, frames in FRAMES.items()}

_TS = Num()


# ---------------------------------------------------------------------------- parsing and building

def parse(raw: str, sender: str):
    """A frame ``sender`` sent, read: a dict of its known fields with ``t`` (and ``ts`` when it had one), or
    ``Ignored`` for a type this side does not accept from that sender. Raises ``ProtocolError``; for a ``hello``
    whose ``protocol`` is not exactly 2, code ``protocol``, before any other field is read."""
    data, t = envelope(raw)
    spec = FRAMES[sender].get(t)
    if spec is None:
        return Ignored(t)
    if sender == HELPER and t == "hello" and as_integer(data.get("protocol")) != PROTOCOL:
        raise ProtocolError("protocol", "protocol", "a hello that is not protocol 2: answered with the v1 refusal")
    out = {"t": t}
    if "ts" in data:
        out["ts"] = _TS.read(data["ts"], ("ts",))
    out.update(spec.read(data, ()))
    return out


def parse_helper_frame(raw: str):
    """A frame a helper sent: what the hub receives. See ``parse``."""
    return parse(raw, HELPER)


def parse_server_frame(raw: str):
    """A frame the server sent: what a helper receives. See ``parse``."""
    return parse(raw, SERVER)


def encode(sender: str, t: str, fields: dict, ts: Optional[float] = None) -> str:
    """A frame's text from its fields, checked as a receiver would check it (``ProtocolError`` if it would be
    refused, and ``unknown`` for a field the frame does not have: a builder's typo, which a receiver would drop),
    with ``t`` then ``ts`` (now, unless given) first."""
    spec = FRAMES[sender].get(t)
    if spec is None:
        raise ProtocolError("no_type", "t", f"the {sender} sends no {t!r} frame")
    for name in fields:
        if name not in spec.names:
            raise ProtocolError("unknown", name, f"a {t} frame has no field {name!r}")
    frame = {"t": t, "ts": round(time.time(), 3) if ts is None else ts, **fields}
    text = json.dumps(frame, separators=(",", ":"), ensure_ascii=False, allow_nan=False)
    parse(text, sender)
    return text


def helper_frame(t: str, ts: Optional[float] = None, **fields) -> str:
    """A frame a helper sends, checked: the fake helper's builder (4-05)."""
    return encode(HELPER, t, fields, ts)


def server_frame(t: str, ts: Optional[float] = None, **fields) -> str:
    """A frame the server sends, checked."""
    return encode(SERVER, t, fields, ts)


def v1_refusal() -> str:
    """The one frame a hello that is not protocol 2 gets, before close code 4426 (PROTOCOL.md, "The v1 refusal")."""
    return server_frame("error", id="", message=V1_REFUSAL_MESSAGE)


def welcome(machine_id: str, machine_name: str, settings: dict, ping_s: int, server: dict, update: Optional[dict],
            keep: list, drop: list, resend: list) -> str:
    return server_frame("welcome", protocol=PROTOCOL, machine={"id": machine_id, "name": machine_name},
                        settings=settings, ping_s=ping_s, server=server, update=update,
                        jobs={"keep": list(keep), "drop": list(drop), "resend": list(resend)})


def refused(code: str, message: str, retry_after_s: Optional[float] = None) -> str:
    fields: dict[str, Any] = {"code": code, "message": message}
    if retry_after_s is not None:
        fields["retry_after_s"] = retry_after_s
    return server_frame("refused", **fields)


def settings(llm: dict, cpu: dict) -> str:
    return server_frame("settings", llm=llm, cpu=cpu)


def rescan() -> str:
    return server_frame("rescan")


def request(request_id: str, model: str, messages: list, tools: Optional[list], params: dict,
            timeout_s: float) -> str:
    return server_frame("request", id=request_id, model=model, messages=messages, tools=tools, params=params,
                        timeout_s=timeout_s)


def cancel(request_id: str) -> str:
    return server_frame("cancel", id=request_id)


def job_offer(job: str, run: str, kind: str, spec: dict, spec_hash: str, jobs_build: str, deadline_s: float,
              mem_mb: int, priority: int, offer_ttl_s: float = 30) -> str:
    return server_frame("job_offer", job=job, run=run, kind=kind, spec=spec, spec_hash=spec_hash,
                        jobs_build=jobs_build, deadline_s=deadline_s, mem_mb=mem_mb, priority=priority,
                        offer_ttl_s=offer_ttl_s)


def job_result_ack(run: str) -> str:
    return server_frame("job_result_ack", run=run)


def job_abort(run: str, reason: str) -> str:
    return server_frame("job_abort", run=run, reason=reason)


def jobs_pause(reason: str = "benchmark") -> str:
    return server_frame("jobs_pause", reason=reason)


def jobs_resume() -> str:
    return server_frame("jobs_resume")


def update(manifest: str, signature: str) -> str:
    return server_frame("update", manifest=manifest, signature=signature)


def ping(sender: str = SERVER) -> str:
    return encode(sender, "ping", {})


def pong(sender: str = SERVER) -> str:
    return encode(sender, "pong", {})


def shutdown(code: str, reason: str) -> str:
    return server_frame("shutdown", reason=reason, code=code)


def redact(data: Any) -> Any:
    """A frame without its secrets, for a log: the token, keys and anything long."""
    if isinstance(data, dict):
        return {k: ("<redacted>" if k in ("token", "api_key", "authorization") else redact(v))
                for k, v in data.items()}
    if isinstance(data, list):
        return [redact(v) for v in data]
    if isinstance(data, str) and len(data) > 300:
        return data[:300] + f"... ({len(data)} chars)"
    return data
