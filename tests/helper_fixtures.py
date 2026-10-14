"""The conformance fixtures of protocol 2, from the server's side (PROTOCOL.md, "The conformance fixtures").

Reads ``crates/citar-helper/protocol/v2/``: the frame fixtures, the transcripts (checked as strictly as the helper's
loader checks them: an unknown key, a step with no action or two, a frame of a type its sender never sends, a matcher
spelled wrong, an ``<id:NAME>`` without a value, a setup key, an act or a state word outside the vocabulary are all
refused), their matcher language (``matches`` and ``realize``), the acknowledgement a correct helper makes of the
site's settings (``expected_ack``), and PROTOCOL.md's frame table. The helper's copy is
``crates/citar-helper/src/conformance``; ``tests/test_helper_protocol.py`` holds both to the same files, and from 4-05
the fake helper plays the transcripts against the real hub with this module.
"""
from __future__ import annotations

import base64
import json
import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Optional

from citar.server import helper_protocol as HP

ROOT = Path(__file__).resolve().parents[1]
CRATE = ROOT / "crates" / "citar-helper"
FIXTURES = CRATE / "protocol" / "v2"
PROTOCOL_MD = CRATE / "PROTOCOL.md"


# ---------------------------------------------------------------------------- frame fixtures

@dataclass
class FrameFixture:
    """One frame fixture: ``name`` is its file name without ``.json`` (``helper.hello.cpu-only``)."""
    name: str
    sender: str
    category: str
    text: str
    frame: Optional[dict] = None
    expect: Optional[dict] = None
    code: str = ""
    path: str = ""

    @property
    def canonical(self) -> bool:
        """A type's canonical example, ``<from>.<type>.json``."""
        return self.category == "valid" and len(self.name.split(".")) == 2


def _keys_only(data: dict, allowed: tuple, where: str):
    unknown = [k for k in data if k not in allowed]
    if unknown:
        raise ValueError(f"{where}: unknown key {unknown[0]!r}")


def _dumps(frame) -> str:
    """A fixture's frame as text, as the helper's side writes it (compact, characters kept)."""
    return json.dumps(frame, separators=(",", ":"), ensure_ascii=False)


def load_frame_fixture(category: str, path: Path) -> FrameFixture:
    name = path.stem
    sender = name.split(".")[0]
    if sender not in (HP.HELPER, HP.SERVER):
        raise ValueError(f"{path}: the name must start with helper. or server.")
    data = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise ValueError(f"{path}: not an object")
    frame = data.get("frame")
    if category == "valid":
        _keys_only(data, ("frame", "expect"), str(path))
        t = frame.get("t") if isinstance(frame, dict) else None
        if name.split(".")[1] != t:
            raise ValueError(f"{path}: a valid fixture is named <from>.<t>..., and t is {t!r}")
        return FrameFixture(name, sender, category, _dumps(frame), frame=frame, expect=data.get("expect", frame))
    if category == "invalid":
        _keys_only(data, ("frame", "raw", "pad_bytes", "code", "path", "why"), str(path))
        if "raw" in data and "frame" not in data and "pad_bytes" not in data:
            text = data["raw"]
        elif "frame" in data and "raw" not in data:
            text = _dumps(frame) + " " * int(data.get("pad_bytes", 0))
        else:
            raise ValueError(f"{path}: give exactly one of frame and raw")
        if data.get("code") not in HP.ERROR_CODES or not isinstance(data.get("path"), str) or not data.get("why"):
            raise ValueError(f"{path}: an invalid fixture has a code, a path and a why")
        return FrameFixture(name, sender, category, text, frame=frame, code=data["code"], path=data["path"])
    if category == "ignored":
        _keys_only(data, ("frame", "why"), str(path))
        return FrameFixture(name, sender, category, _dumps(frame), frame=frame)
    raise ValueError(f"no fixture category {category!r}")


def frame_fixtures(category: str) -> list[FrameFixture]:
    """Every frame fixture of a category, sorted by name."""
    return [load_frame_fixture(category, p) for p in sorted((FIXTURES / "frames" / category).glob("*.json"))]


def canonical_examples() -> dict[tuple[str, str], dict]:
    """The canonical example of each frame type, by (sender, type)."""
    return {(f.sender, f.frame["t"]): f.frame for f in frame_fixtures("valid") if f.canonical}


# ---------------------------------------------------------------------------- the frame table

def frame_table(markdown: str) -> list[tuple[str, str]]:
    """PROTOCOL.md's frame table: each row's (type, sender), in order. ``ValueError`` if it cannot be read."""
    lines = markdown.splitlines()
    try:
        start = next(i for i, line in enumerate(lines) if line.strip() == "## The frame table")
    except StopIteration:
        raise ValueError('PROTOCOL.md has no "## The frame table" section') from None
    rest = lines[start + 1:]
    i = next((k for k, line in enumerate(rest) if line.startswith("|")), None)
    if i is None:
        raise ValueError("the frame table has no header")
    header = [c.strip() for c in rest[i].strip("|").split("|")]
    if header[:2] != ["Frame", "From"]:
        raise ValueError(f"the frame table's header is {rest[i]!r}, not | Frame | From | ...")
    rows = []
    for line in rest[i + 2:]:
        if not line.startswith("|"):
            break
        cells = [c.strip() for c in line.strip("|").split("|")]
        t = cells[0]
        if not (len(t) > 2 and t.startswith("`") and t.endswith("`")):
            raise ValueError(f"a frame table row without a `type`: {line!r}")
        if len(cells) < 2 or cells[1] not in (HP.HELPER, HP.SERVER):
            raise ValueError(f"a frame table row whose From is not helper or server: {line!r}")
        rows.append((t[1:-1], cells[1]))
    if not rows:
        raise ValueError("the frame table has no rows")
    return rows


# ---------------------------------------------------------------------------- the matcher language

KINDS = ("any", "bool", "int", "number", "string", "hex12", "hex16", "hex64", "base64", "id", "golden")
_NAME = re.compile(r"[a-z0-9_]+")
_PART = re.compile(r"[A-Za-z0-9_-]+")
_HEX = {"hex12": 12, "hex16": 16, "hex64": 64}


class MatcherError(ValueError):
    """A matcher spelled wrong."""


@dataclass(frozen=True)
class Matcher:
    kind: str
    lo: Any = None
    hi: Any = None
    name: str = ""
    golden: tuple = ()

    def accepts(self, v) -> bool:
        """Whether ``v`` is a value this matcher stands for, ignoring bindings (an id takes anything but null; a
        golden value is not known here)."""
        if self.kind == "any":
            return True
        if self.kind == "bool":
            return isinstance(v, bool)
        if self.kind == "int":
            n = HP.as_integer(v)
            return n is not None and (self.lo is None or n >= self.lo) and (self.hi is None or n <= self.hi)
        if self.kind == "number":
            x = HP.as_number(v)
            return x is not None and (self.lo is None or x >= self.lo) and (self.hi is None or x <= self.hi)
        if self.kind == "string":
            return isinstance(v, str)
        if self.kind in _HEX:
            return isinstance(v, str) and len(v) == _HEX[self.kind] and bool(re.fullmatch(r"[0-9a-f]*", v))
        if self.kind == "base64":
            return isinstance(v, str) and bool(v) and HP.is_base64(v)
        if self.kind == "id":
            return v is not None
        return False

    def default(self):
        """The value realizing this matcher when nothing better is known."""
        if self.kind == "bool":
            return False
        if self.kind in ("int", "number"):
            zero = 0 if self.kind == "int" else 0.0
            if self.lo is not None:
                return self.lo
            return min(self.hi, zero) if self.hi is not None else zero
        if self.kind == "string":
            return ""
        if self.kind in _HEX:
            return "0" * _HEX[self.kind]
        if self.kind == "base64":
            return base64.b64encode(bytes(32)).decode("ascii")
        return None


def _bounds(arg: str, cast, spelled: str):
    if ".." not in arg:
        raise MatcherError(f"{spelled}: {arg!r} is not LO..HI")
    lo, hi = arg.split("..", 1)
    try:
        return (cast(lo) if lo else None), (cast(hi) if hi else None)
    except ValueError:
        raise MatcherError(f"{spelled}: {arg!r} is not a bound") from None


def parse_matcher(s) -> Optional[Matcher]:
    """The matcher ``s`` spells, ``None`` for a literal; ``MatcherError`` for a matcher spelled wrong."""
    if not (isinstance(s, str) and s.startswith("<") and s.endswith(">")):
        return None
    inner = s[1:-1]
    kind, sep, arg = inner.partition(":")
    if kind not in KINDS:
        return None
    if kind in ("any", "bool", "string", "base64") or kind in _HEX:
        if sep:
            raise MatcherError(f"{s}: <{kind}> takes no argument")
        return Matcher(kind)
    if kind in ("int", "number"):
        if not sep:
            return Matcher(kind)
        lo, hi = _bounds(arg, int if kind == "int" else float, s)
        return Matcher(kind, lo=lo, hi=hi)
    if kind == "id":
        if not (sep and _NAME.fullmatch(arg)):
            raise MatcherError(f"{s}: <id:NAME> needs a name of a-z 0-9 _")
        return Matcher("id", name=arg)
    parts = arg.split("/") if sep else []
    if len(parts) < 2 or not all(_PART.fullmatch(p) for p in parts):
        raise MatcherError(f"{s}: <golden:SET/NAME[/FIELD]> needs a set and a name")
    return Matcher("golden", golden=tuple(parts))


Golden = Callable[[str, tuple], Any]


def no_golden(set_name: str, path: tuple):
    """No golden set: every golden matcher fails to match, and realizes from the canonical example."""
    return None


class Mismatch(AssertionError):
    def __init__(self, path: str, why: str):
        super().__init__(f"at {path or 'the root'}: {why}")
        self.path, self.why = path, why


def json_eq(a, b) -> bool:
    """JSON values equal, numbers compared by value (2 equals 2.0), booleans never numbers."""
    if isinstance(a, bool) or isinstance(b, bool):
        return type(a) is type(b) and a == b
    if isinstance(a, (int, float)) and isinstance(b, (int, float)):
        return a == b
    if isinstance(a, list) and isinstance(b, list):
        return len(a) == len(b) and all(json_eq(x, y) for x, y in zip(a, b))
    if isinstance(a, dict) and isinstance(b, dict):
        return a.keys() == b.keys() and all(json_eq(v, b[k]) for k, v in a.items())
    return type(a) is type(b) and a == b


def _at(path: str, key) -> str:
    return f"{path}.{key}" if path else str(key)


def matches(pattern, actual, bindings: dict, golden: Golden = no_golden):
    """Raises ``Mismatch`` where ``actual`` does not match ``pattern``. Bindings made by a match are kept only when the
    whole value matches."""
    scratch = dict(bindings)
    _match(pattern, actual, scratch, golden, "")
    bindings.clear()
    bindings.update(scratch)


def _match(pattern, actual, b: dict, golden: Golden, path: str):
    try:
        m = parse_matcher(pattern)
    except MatcherError as exc:
        raise Mismatch(path, str(exc)) from None
    if m is not None:
        if m.kind == "id":
            if m.name in b:
                if not json_eq(b[m.name], actual):
                    raise Mismatch(path, f"{actual!r} is not {m.name}'s binding {b[m.name]!r}")
            elif actual is None:
                raise Mismatch(path, f"{m.name} cannot be bound to null")
            else:
                b[m.name] = actual
            return
        if m.kind == "golden":
            want = golden(m.golden[0], m.golden[1:])
            if want is None:
                raise Mismatch(path, f"no golden value for {pattern}")
            if not json_eq(want, actual):
                raise Mismatch(path, f"{actual!r} is not the golden {want!r}")
            return
        if not m.accepts(actual):
            raise Mismatch(path, f"{actual!r} does not match {pattern}")
        return
    if isinstance(pattern, dict) and isinstance(actual, dict):
        for k, pv in pattern.items():
            here = _at(path, k)
            if k not in actual:
                raise Mismatch(here, "absent")
            _match(pv, actual[k], b, golden, here)
        return
    if isinstance(pattern, list) and isinstance(actual, list):
        if len(pattern) != len(actual):
            raise Mismatch(path, f"{len(actual)} items, not {len(pattern)}")
        for i, (pv, av) in enumerate(zip(pattern, actual)):
            _match(pv, av, b, golden, _at(path, i))
        return
    if not json_eq(pattern, actual):
        raise Mismatch(path, f"{actual!r} is not {pattern!r}")


def realize(pattern, bindings: dict, values: dict, golden: Golden = no_golden, example=None):
    """The frame the scripted side sends for ``pattern`` (PROTOCOL.md, "Realizing"). ``ValueError`` for an
    ``<id:NAME>`` with neither a binding nor a value."""
    m = parse_matcher(pattern)
    if m is not None:
        if m.kind == "id":
            if m.name not in bindings:
                if m.name not in values:
                    raise ValueError(f"<id:{m.name}> has no binding and no value")
                bindings[m.name] = values[m.name]
            return bindings[m.name]
        if m.kind == "golden":
            want = golden(m.golden[0], m.golden[1:])
            if want is None:
                want = example
            if want is None:
                raise ValueError(f"{pattern}: no golden value and no example")
            return want
        return example if example is not None and m.accepts(example) else m.default()
    if isinstance(pattern, dict):
        ex = example if isinstance(example, dict) else {}
        return {k: realize(v, bindings, values, golden, ex.get(k)) for k, v in pattern.items()}
    if isinstance(pattern, list):
        ex = example if isinstance(example, list) else []
        return [realize(v, bindings, values, golden, ex[i] if i < len(ex) else (ex[0] if ex else None))
                for i, v in enumerate(pattern)]
    return pattern


def matchers_in(v, path: str = ""):
    """Every matcher spelled in ``v``, as (path, Matcher or MatcherError)."""
    out = []
    if isinstance(v, str):
        try:
            m = parse_matcher(v)
        except MatcherError as exc:
            out.append((path, exc))
        else:
            if m is not None:
                out.append((path, m))
    elif isinstance(v, list):
        for i, x in enumerate(v):
            out.extend(matchers_in(x, _at(path, i)))
    elif isinstance(v, dict):
        for k, x in v.items():
            out.extend(matchers_in(x, _at(path, k)))
    return out


# ---------------------------------------------------------------------------- transcripts

#: The frames that come on their own clocks (PROTOCOL.md, "Background frames"): a player skips them unless the next
#: step expects their type, and a quiet wait allows them.
BACKGROUND = ("ping", "pong", "status", "power")
HELPER_SETUP = ("config", "clock", "home", "service", "endpoint", "update_key", "host")
HELPER_CONFIG = ("max_concurrent", "models", "quiet", "cpu", "updates", "no_llm", "stream", "no_power")
HELPER_HOST = ("verifiable", "start")
HUB_SETUP = ("machine", "token", "rate", "jobs", "update", "clock", "ping_s", "local_executor", "verification",
             "answer_pings")
HUB_MACHINE = ("settings", "key", "trust")
#: What can happen around each side (``act``), and what a ``state`` step may expect of it (PROTOCOL.md, "Actions" and
#: "State expectations").
HELPER_ACTS = ("quiet_begins",)
HUB_ACTS = ("submit_request", "force_request", "cancel_request", "drop_connection", "queue_job", "force_offer",
            "lab_stop", "abort_run", "expire_run", "set_settings", "rescan", "reset_key", "revoke_token",
            "disable_machine", "second_connection", "benchmark_start", "benchmark_end", "manifest_arrives")
HELPER_STATE = ("in_flight", "exit", "held_results", "running", "downloads", "logged")
HUB_STATE = ("slot_free", "draining", "flags", "step", "requeued", "request", "llm_slots", "cpu_threads", "cpu_percent",
             "memory_mb", "work", "cpu_enabled", "quiet", "models", "loaded", "last_report", "power_rows", "key",
             "audit", "token_lookups", "audit_rows", "rate_count", "run", "job", "result_lines", "backoff",
             "mem_mb_raised", "claim_failures", "events", "update_status")
ACTS = {"helper": HELPER_ACTS, "hub": HUB_ACTS}
STATES = {"helper": HELPER_STATE, "hub": HUB_STATE}
#: The longest close reason, in bytes (a WebSocket close frame's limit).
MAX_CLOSE_REASON = 123
ENDPOINT_KEYS = ("models", "respond", "expect_request", "absent", "expect_disconnect_within_ms",
                 "expect_no_tokens_after")
RESPOND_KEYS = ("stream", "delay_ms", "json", "status", "body", "hang")
HOST_KEYS = ("progress", "finish", "crash_host", "hang")
TOP_KEYS = ("name", "about", "refs", "plays", "one_side", "values", "setup", "steps")
ACTIONS = ("s2h", "h2s", "close", "drop", "connect", "wait", "endpoint", "host", "act", "state")
MODIFIERS = ("within_s", "note", "invalid", "lost")
_PACKAGE = re.compile(r"4-\d\d")


@dataclass
class Step:
    action: str
    body: Any
    within_s: Optional[float] = None
    note: str = ""
    invalid: str = ""
    #: A frame lost with the connection: the side under test sends it, the scripted side does not.
    lost: bool = False

    @property
    def sender(self) -> Optional[str]:
        """Who sends the frame of an ``s2h`` or ``h2s`` step."""
        return {"s2h": HP.SERVER, "h2s": HP.HELPER}.get(self.action)


@dataclass
class Transcript:
    name: str
    about: str
    refs: list
    plays: dict
    one_side: str
    values: dict
    setup: dict
    steps: list = field(default_factory=list)

    def frames(self):
        """Every frame step: (index, sender, pattern, the code it is broken on purpose with, or "")."""
        return [(i, s.sender, s.body, s.invalid) for i, s in enumerate(self.steps) if s.sender]


def _object(v, what: str) -> dict:
    if not isinstance(v, dict):
        raise ValueError(f"{what} must be an object")
    return v


def _text(v, what: str) -> str:
    if not isinstance(v, str) or not v:
        raise ValueError(f"{what} must be a non-empty string")
    return v


def _unknown(data: dict, allowed: tuple, what: str):
    for k in data:
        if k not in allowed:
            raise ValueError(f"{what} has the unknown key {k!r} (allowed: {', '.join(allowed)})")


def _positive(v, what: str) -> float:
    if isinstance(v, bool) or not isinstance(v, (int, float)) or v <= 0:
        raise ValueError(f"{what} must be positive")
    return float(v)


def _step(data) -> Step:
    data = _object(data, "a step")
    _unknown(data, ACTIONS + MODIFIERS, "the step")
    actions = [a for a in ACTIONS if a in data]
    if len(actions) != 1:
        raise ValueError(f"a step has exactly one action, not {actions}")
    kind = actions[0]
    body = data[kind]
    if kind in ("s2h", "h2s"):
        sender = HP.SERVER if kind == "s2h" else HP.HELPER
        t = _object(body, "a frame").get("t")
        if not isinstance(t, str):
            raise ValueError("a frame needs a literal t")
        if t not in HP.FRAMES[sender]:
            raise ValueError(f"the {sender} never sends {t!r}")
    elif kind == "close":
        _unknown(_object(body, "close"), ("by", "code", "reason"), "close")
        if body.get("by") not in (HP.SERVER, HP.HELPER):
            raise ValueError('close.by must be "server" or "helper"')
        if type(body.get("code")) is not int or not 0 <= body["code"] <= 65535:
            raise ValueError("close.code must be a close code")
        reason = body.get("reason", "")
        if not isinstance(reason, str) or len(reason.encode("utf-8")) > MAX_CLOSE_REASON:
            raise ValueError(f"close.reason must be a string of at most {MAX_CLOSE_REASON} bytes")
    elif kind == "drop":
        _unknown(_object(body, "drop"), ("by",), "drop")
        if body.get("by") not in (HP.SERVER, HP.HELPER):
            raise ValueError('drop.by must be "server" or "helper"')
    elif kind == "connect":
        if _object(body, "connect"):
            raise ValueError("connect takes {}")
    elif kind == "wait":
        _unknown(_object(body, "wait"), ("s", "quiet"), "wait")
        _positive(body.get("s"), "wait.s")
        if not isinstance(body.get("quiet", False), bool):
            raise ValueError("wait.quiet must be a boolean")
    elif kind == "endpoint":
        _unknown(_object(body, "endpoint"), ENDPOINT_KEYS, "endpoint")
        if "respond" in body:
            _unknown(_object(body["respond"], "endpoint.respond"), RESPOND_KEYS, "endpoint.respond")
    elif kind == "host":
        _unknown(_object(body, "host"), HOST_KEYS, "host")
    elif kind == "act":
        _unknown(_object(body, "act"), ("side", "do"), "act")
        if body.get("side") not in ("helper", "hub"):
            raise ValueError('act.side must be "helper" or "hub"')
        word = act_word(body.get("do"))
        if word not in ACTS[body["side"]]:
            raise ValueError(f"act.do {word!r} is not a word of that side (allowed: {', '.join(ACTS[body['side']])})")
    else:   # state
        _unknown(_object(body, "state"), ("side", "expect"), "state")
        if body.get("side") not in ("helper", "hub"):
            raise ValueError('state.side must be "helper" or "hub"')
        _unknown(_object(body.get("expect"), "state.expect"), STATES[body["side"]], "state.expect")
    within = _positive(data["within_s"], "within_s") if "within_s" in data else None
    note = _text(data["note"], "note") if "note" in data else ""
    invalid = ""
    if "invalid" in data:
        if kind not in ("s2h", "h2s"):
            raise ValueError("only a frame step can be invalid")
        if data["invalid"] not in HP.ERROR_CODES:
            raise ValueError("invalid must be a protocol error code")
        invalid = data["invalid"]
    lost = data.get("lost", False)
    if "lost" in data and (lost is not True or kind not in ("s2h", "h2s")):
        raise ValueError("lost is true, on a frame step only")
    return Step(kind, body, within, note, invalid, lost)


def act_word(do) -> str:
    """The word an ``act`` step's ``do`` names: a string, or the one key of an object."""
    if isinstance(do, str):
        return do
    if isinstance(do, dict) and len(do) == 1:
        return next(iter(do))
    raise ValueError("act.do must be a word or an object of one word")


def transcript_from(data) -> Transcript:
    """Checks and reads a transcript's JSON; ``ValueError`` says what is wrong with it."""
    top = _object(data, "a transcript")
    _unknown(top, TOP_KEYS, "the transcript")
    name, about = _text(top.get("name"), "name"), _text(top.get("about"), "about")
    refs = top.get("refs")
    if not (isinstance(refs, list) and refs and all(isinstance(r, str) for r in refs)):
        raise ValueError("refs must be a non-empty list of design sections")
    plays = _object(top.get("plays"), "plays")
    _unknown(plays, ("helper", "hub"), "plays")
    for side in ("helper", "hub"):
        p = plays.get(side)
        if not (p is None or (isinstance(p, str) and _PACKAGE.fullmatch(p))):
            raise ValueError(f'plays.{side} must be a package ("4-05") or null')
    one_side = _text(top["one_side"], "one_side") if "one_side" in top else ""
    if plays.get("helper") is None and plays.get("hub") is None:
        raise ValueError("no side plays it")
    if plays.get("helper") and plays.get("hub") and one_side:
        raise ValueError("both sides play it, so it has no one_side")
    if (plays.get("helper") is None or plays.get("hub") is None) and not one_side:
        raise ValueError("one side does not play it: one_side must say why")
    values = _object(top.get("values"), "values")
    setup = _object(top.get("setup"), "setup")
    _unknown(setup, ("helper", "hub"), "setup")
    helper, hub = _object(setup.get("helper"), "setup.helper"), _object(setup.get("hub"), "setup.hub")
    _unknown(helper, HELPER_SETUP, "setup.helper")
    _unknown(hub, HUB_SETUP, "setup.hub")
    if "config" in helper:
        _unknown(_object(helper["config"], "setup.helper.config"), HELPER_CONFIG, "setup.helper.config")
    if "host" in helper:
        _unknown(_object(helper["host"], "setup.helper.host"), HELPER_HOST, "setup.helper.host")
    if "machine" in hub:
        _unknown(_object(hub["machine"], "setup.hub.machine"), HUB_MACHINE, "setup.hub.machine")
    raw_steps = top.get("steps")
    if not (isinstance(raw_steps, list) and raw_steps):
        raise ValueError("steps must be a non-empty list")
    steps = []
    for i, s in enumerate(raw_steps):
        try:
            steps.append(_step(s))
        except ValueError as exc:
            raise ValueError(f"step {i}: {exc}") from None
    for path, m in matchers_in(raw_steps, "steps") + matchers_in(setup, "setup"):
        if isinstance(m, MatcherError):
            raise ValueError(f"{path}: {m}")
        if m.kind == "id" and m.name not in values:
            raise ValueError(f"{path}: <id:{m.name}> has no example in values")
    return Transcript(name, about, refs, plays, one_side, values, setup, steps)


def load_transcript(path: Path) -> Transcript:
    try:
        t = transcript_from(json.loads(path.read_text(encoding="utf-8")))
    except ValueError as exc:
        raise ValueError(f"{path}: {exc}") from None
    if t.name != path.stem:
        raise ValueError(f"{path}: the file is not named after the transcript, {t.name!r}")
    return t


def transcripts() -> list[Transcript]:
    """Every transcript, sorted by name."""
    return [load_transcript(p) for p in sorted((FIXTURES / "transcripts").glob("*.json"))]


# ---------------------------------------------------------------------------- the acknowledgement of settings

def expected_ack(site: dict, config: dict) -> dict:
    """The ``effective`` and ``capped`` a correct helper with this configuration (a transcript's
    ``setup.helper.config``) acknowledges for the site's settings (PROTOCOL.md, "Settings"): each figure the smaller of
    the site's and the ceiling, the site's 0 meaning the ceiling; the kinds both list; CPU work on only when the site
    and the machine's own switch say on; model work on only when the site says on and the helper has a model
    endpoint."""
    capped = set()
    no_llm = bool(config.get("no_llm"))
    local = config["cpu"]

    def figure(name, asked, ceiling):
        if asked == 0:
            return ceiling
        if ceiling < asked:
            capped.add(name)
            return ceiling
        return asked

    if site["llm"]["enabled"] and no_llm:
        capped.add("llm.enabled")
    if site["cpu"]["enabled"] and not local["enabled"]:
        capped.add("cpu.enabled")
    if any(k not in local["work"] for k in site["cpu"]["work"]):
        capped.add("cpu.work")
    effective = {
        "llm": {"enabled": site["llm"]["enabled"] and not no_llm,
                "slots": figure("llm.slots", site["llm"]["slots"], 0 if no_llm else config.get("max_concurrent", 1))},
        "cpu": {"enabled": site["cpu"]["enabled"] and local["enabled"],
                "threads": figure("cpu.threads", site["cpu"]["threads"], local["threads"]),
                "cpu_percent": figure("cpu.cpu_percent", site["cpu"]["cpu_percent"], local["cpu_percent"]),
                "memory_mb": figure("cpu.memory_mb", site["cpu"]["memory_mb"], local["memory_mb"]),
                "work": [k for k in HP.KINDS_OF_WORK if k in site["cpu"]["work"] and k in local["work"]],
                "yield_to_llm": site["cpu"]["yield_to_llm"]}}
    return {"effective": effective, "capped": [c for c in HP.CAPPED if c in capped]}
