"""The Python runner of the rule scripts in tests/rules/ (the language is tests/rules/README.md).

It plays a script on the engine through ``citar.engine_api`` alone (``EngineGame`` and the bot functions), as the Rust
runner (crates/citar-testkit/src/script) plays it on the Rust engine; tests/rules/_selftest.toml keeps the two in step.
Steps marked ``intended`` expect the Rust engine's deliberate difference and are skipped here. A script's ``needs``
names the package that makes it pass on Rust; this runner plays it regardless.
"""
from __future__ import annotations

import functools
import json
import re
import tomllib
from pathlib import Path
from typing import Any, Optional

from citar import engine_api
from citar.engine_api import ActionError, EngineGame

RULES = Path(__file__).resolve().parent / "rules"
ROOT = RULES.parent.parent
#: the parameter schema of the bot version basic-1, which bot steps' params are checked against
BOT_SCHEMA = ROOT / "crates" / "citar-bot" / "params" / "basic-1.json"

TOP = ("about", "from", "needs", "map", "start", "config", "step")
KINDS = ("op", "ops", "tool", "check", "new_game", "set", "repeat", "bot")
# `as` and `error` belong to the kinds that use them, so a check that says `error` is refused rather than passing
# without looking.
COMMON = ("note", "must_fail", "intended", "coerce")
MATCHERS = ("absent", "is_null", "eq", "ne", "gt", "ge", "lt", "le", "approx", "contains", "not_contains", "len",
            "matches", "any", "none", "subset")
WITH = ("tol",)
OWN = {"op": ("args", "as", "error"), "ops": ("as", "error"), "tool": ("player", "args", "as", "error"),
       "check": ("path", "as"), "new_game": ("error",), "repeat": ("steps",),
       "bot": ("player", "negotiation", "version", "aggression", "params", "diplomacy", "as", "error")}
#: a script's `needs`: the Phase 2 package that makes it pass on the Rust engine
NEEDS = re.compile(r"[0-9]-[0-9]{2}[a-z]?")
#: what a bot step asks the seat's bot
BOT_ASKS = ("turn", "respond", "advice")
BOT_VERSIONS = ("basic-1", "idle")
#: the bot's draws a turn step must pin (DESIGN.md P2.3.5): its params give each of these
PINS = ("tech_noise", "ranged_chance", "peace_offer_chance", "friend_chance", "friend_chance_aggr", "war_chance",
        "war_chance_aggr")


class ScriptError(Exception):
    """A step that failed, or a script that is not well formed."""


def discover() -> list[Path]:
    """Every script, sorted by name."""
    return sorted(p for p in RULES.glob("*.toml") if p.stem != "intended")


def plain(v: Any) -> Any:
    """A value as JSON has it: what the Rust engine hands back (dicts with string keys, lists, scalars)."""
    return json.loads(json.dumps(v))


def intended_ids() -> set:
    """The deliberate differences scripts may cite."""
    out = set()
    for f in (ROOT / "refcheck" / "intended.toml", RULES / "intended.toml"):
        with open(f, "rb") as fh:
            for entry in tomllib.load(fh).get("differences", []):
                out.add(entry.get("id"))
    return out


# ------------------------------------------------------------------------------------------------ values

def same(a, b) -> bool:
    """Whether two values are equal as JSON: numbers by value, a boolean never a number, objects as maps."""
    if isinstance(a, bool) or isinstance(b, bool):
        return isinstance(a, bool) and isinstance(b, bool) and a == b
    if isinstance(a, (int, float)) and isinstance(b, (int, float)):
        return a == b
    if isinstance(a, list) and isinstance(b, list):
        return len(a) == len(b) and all(same(x, y) for x, y in zip(a, b))
    if isinstance(a, dict) and isinstance(b, dict):
        return len(a) == len(b) and all(k in b and same(v, b[k]) for k, v in a.items())
    return type(a) is type(b) and a == b


def show(v) -> str:
    """A value as compact JSON, for messages."""
    text = json.dumps(v, separators=(",", ":"), ensure_ascii=False)
    return text if len(text) <= 300 else text[:300] + "..."


def _is_num(v) -> bool:
    return isinstance(v, (int, float)) and not isinstance(v, bool)


def _num(v) -> float:
    if not _is_num(v):
        raise ScriptError(f"expected a number, got {show(v)}")
    return v


# ------------------------------------------------------------------------------------------------ paths

_BARE = re.compile(r"[A-Za-z0-9_-]+")


def _literal(text: str):
    try:
        return json.loads(text)
    except ValueError:
        return text


def _segments(text: str, i: int, whole: bool) -> tuple[list, int]:
    segs = []
    if whole:
        m = _BARE.match(text, i)
        if m:
            segs.append(("key", m.group()))
            i = m.end()
    while True:
        if text.startswith(".", i) and _BARE.match(text, i + 1):
            m = _BARE.match(text, i + 1)
            segs.append(("key", m.group()))
            i = m.end()
        elif text.startswith("[", i):
            k, in_str = i + 1, False
            while k < len(text):
                ch = text[k]
                if ch == "\\" and in_str:
                    k += 1
                elif ch == '"':
                    in_str = not in_str
                elif ch == "]" and not in_str:
                    break
                k += 1
            if k >= len(text):
                raise ScriptError(f"path `{text}`: `[` is never closed")
            body = text[i + 1:k]
            i = k + 1
            bad = ScriptError(f"path `{text}`: bad segment `[{body}]`")
            if body.startswith('"'):
                try:
                    segs.append(("key", json.loads(body)))
                except ValueError:
                    raise bad
            elif body.isdigit() and body.isascii():
                segs.append(("index", int(body)))
            elif "=" in body:
                name, value = body.split("=", 1)
                if name.startswith("#"):
                    if not (name[1:].isdigit() and name[1:].isascii()):
                        raise bad
                    segs.append(("pos", int(name[1:]), _literal(value)))
                elif name and _BARE.fullmatch(name):
                    segs.append(("field", name, _literal(value)))
                else:
                    raise bad
            else:
                raise bad
        else:
            return segs, i


def parse_path(text: str) -> list:
    """A whole path: see tests/rules/README.md."""
    segs, i = _segments(text, 0, True)
    if i < len(text):
        raise ScriptError(f"path `{text}`: unexpected `{text[i:]}`")
    if not segs and text:
        raise ScriptError(f"path `{text}`: no segment")
    return segs


_MISSING = object()


def get(v, segs):
    """The value at a path, or _MISSING."""
    cur = v
    for s in segs:
        if s[0] == "key" and isinstance(cur, dict):
            cur = cur.get(s[1], _MISSING)
        elif s[0] == "index" and isinstance(cur, list):
            cur = cur[s[1]] if s[1] < len(cur) else _MISSING
        elif s[0] == "field" and isinstance(cur, list):
            cur = next((x for x in cur if isinstance(x, dict) and s[1] in x and same(x[s[1]], s[2])), _MISSING)
        elif s[0] == "pos" and isinstance(cur, list):
            cur = next((x for x in cur if isinstance(x, list) and s[1] < len(x) and same(x[s[1]], s[2])), _MISSING)
        else:
            return _MISSING
        if cur is _MISSING:
            return _MISSING
    return cur


# ------------------------------------------------------------------------------------------------ matchers

def check(subject, spec: dict):
    """Checks the matchers of ``spec`` against ``subject`` (_MISSING where the path led nowhere)."""
    used = [m for m in MATCHERS if m in spec]
    if not used:
        raise ScriptError("a check needs at least one matcher")
    if "absent" in spec:
        want = spec["absent"]
        if not isinstance(want, bool):
            raise ScriptError("absent must be true or false")
        if len(used) > 1 and want:
            raise ScriptError("absent = true takes no other matcher")
        if want != (subject is _MISSING):
            if subject is _MISSING:
                raise ScriptError("expected a value there, found nothing")
            raise ScriptError(f"expected nothing there, found {show(subject)}")
        if want:
            return
    if subject is _MISSING:
        raise ScriptError("found nothing at the path")
    v = subject
    for m in used:
        arg = spec[m]
        if m == "absent":
            ok = True
        elif m == "is_null":
            if not isinstance(arg, bool):
                raise ScriptError("is_null must be true or false")
            ok = (v is None) == arg
        elif m == "eq":
            ok = same(v, arg)
        elif m == "ne":
            ok = not same(v, arg)
        elif m in ("gt", "ge", "lt", "le"):
            a, b = _num(v), _num(arg)
            ok = {"gt": a > b, "ge": a >= b, "lt": a < b, "le": a <= b}[m]
        elif m == "approx":
            tol = _num(spec["tol"]) if "tol" in spec else 1e-6
            a, b = _num(v), _num(arg)
            ok = abs(a - b) <= tol * max(1.0, abs(a), abs(b))
        elif m in ("contains", "not_contains"):
            found = _contains(v, arg)
            ok = found if m == "contains" else not found
        elif m == "len":
            if not isinstance(v, (list, str, dict)):
                raise ScriptError(f"len needs a list, a string or an object, not {show(v)}")
            ok = isinstance(arg, int) and not isinstance(arg, bool) and len(v) == arg
        elif m == "matches":
            if not isinstance(arg, str):
                raise ScriptError("matches takes a regular expression")
            if not isinstance(v, str):
                raise ScriptError(f"matches needs a string, not {show(v)}")
            ok = re.search(arg, v) is not None
        elif m in ("any", "none"):
            if not isinstance(arg, dict):
                raise ScriptError("any and none take a table of matchers")
            if not isinstance(v, list):
                raise ScriptError(f"{m} needs a list, not {show(v)}")
            found = any(_element_passes(item, arg) for item in v)
            ok = found if m == "any" else not found
        else:
            ok = _subset(v, arg)
        if not ok:
            tol = f" (tol {show(spec['tol'])})" if m == "approx" and "tol" in spec else ""
            raise ScriptError(f"expected {m} {show(arg)}{tol}, got {show(v)}")


def _element_passes(item, inner: dict) -> bool:
    p = inner.get("path")
    segs = parse_path(p) if isinstance(p, str) else []
    try:
        check(get(item, segs), inner)
        return True
    except ScriptError:
        return False


def _contains(v, arg) -> bool:
    if isinstance(v, list):
        return any(same(x, arg) for x in v)
    if isinstance(v, str):
        if not isinstance(arg, str):
            raise ScriptError("a string contains only strings")
        return arg in v
    if isinstance(v, dict):
        if not isinstance(arg, str):
            raise ScriptError("an object contains only keys, which are strings")
        return arg in v
    raise ScriptError(f"contains needs a list, a string or an object, not {show(v)}")


def _subset(v, part) -> bool:
    if isinstance(v, dict) and isinstance(part, dict):
        return all(k in v and same(v[k], want) for k, want in part.items())
    if isinstance(v, list) and isinstance(part, list):
        return all(any(same(got, want) for got in v) for want in part)
    raise ScriptError(f"subset compares two objects or two lists, not {show(v)} and {show(part)}")


# ------------------------------------------------------------------------------------------------ expressions

class _Expr:
    """expr := product (("+"|"-") product)*; product := unary (("*"|"/"|"//"|"%") unary)*;
    unary := "-" unary | atom (see tests/rules/README.md)."""

    def __init__(self, text: str, runner: "Runner"):
        self.s, self.i, self.r = text, 0, runner

    def err(self, what: str) -> ScriptError:
        return ScriptError(f"expression `{self.s}`: {what} at `{self.s[self.i:]}`")

    def space(self):
        while self.s.startswith(" ", self.i):
            self.i += 1

    def eat(self, tok: str) -> bool:
        self.space()
        if self.s.startswith(tok, self.i):
            self.i += len(tok)
            return True
        return False

    def run(self):
        v = self.expr()
        self.space()
        if self.i < len(self.s):
            raise ScriptError(f"expression `{self.s}`: unexpected `{self.s[self.i:]}`")
        return v

    def expr(self):
        v = self.product()
        while True:
            if self.eat("+"):
                w = self.product()
                v = v + w if isinstance(v, str) and isinstance(w, str) else _arith(v, w, "+")
            elif self.eat("-"):
                v = _arith(v, self.product(), "-")
            else:
                return v

    def product(self):
        v = self.unary()
        while True:
            if self.eat("//"):
                op = "//"
            elif self.eat("*"):
                op = "*"
            elif self.eat("/"):
                op = "/"
            elif self.eat("%"):
                op = "%"
            else:
                return v
            v = _arith(v, self.unary(), op)

    def unary(self):
        if self.eat("-"):
            return _arith(0, self.unary(), "-")
        return self.atom()

    def atom(self):
        self.space()
        rest = self.s[self.i:]
        if self.eat("("):
            v = self.expr()
            if not self.eat(")"):
                raise self.err("expected `)`")
            return v
        if rest.startswith("'"):
            end = rest.find("'", 1)
            if end < 0:
                raise self.err("unclosed string")
            self.i += end + 1
            return rest[1:end]
        m = re.match(r"[0-9.][0-9.eE]*", rest)
        if m:
            text = m.group()
            self.i += len(text)
            if text.isdigit():
                return int(text)
            try:
                return float(text)
            except ValueError:
                raise self.err("bad number")
        m = re.match(r"[A-Za-z_][A-Za-z0-9_]*", rest)
        if m:
            name = m.group()
            self.i += len(name)
            if name in ("null", "true", "false"):
                return {"null": None, "true": True, "false": False}[name]
            if self.s.startswith("(", self.i):
                self.i += 1
                arg = self.expr()
                if not self.eat(")"):
                    raise self.err("expected `)`")
                if name == "x":
                    return self.r.tile(arg)[0]
                if name == "y":
                    return self.r.tile(arg)[1]
                if name == "len":
                    if not isinstance(arg, (list, str, dict)):
                        raise ScriptError(f"len of {show(arg)}: not a list, string or object")
                    return len(arg)
                raise ScriptError(f"expression `{self.s}`: no function {name}")
            segs, self.i = _segments(self.s, self.i, False)
            if name not in self.r.vars:
                raise ScriptError(f"expression `{self.s}`: no variable {name}")
            v = get(self.r.vars[name], segs)
            if v is _MISSING:
                raise ScriptError(f"expression `{self.s}`: {name} has nothing at that path")
            return v
        raise self.err("expected a value")


def _arith(a, b, op: str):
    if not (_is_num(a) and _is_num(b)):
        raise ScriptError(f"cannot compute {show(a)} {op} {show(b)}")
    if op in ("/", "//", "%") and b == 0:
        raise ScriptError("division by zero")
    ops = {"+": lambda x, y: x + y, "-": lambda x, y: x - y, "*": lambda x, y: x * y, "/": lambda x, y: x / y,
           "//": lambda x, y: x // y, "%": lambda x, y: x % y}
    if isinstance(a, int) and isinstance(b, int) and op != "/":
        return ops[op](a, b)
    return ops[op](float(a), float(b))


# ------------------------------------------------------------------------------------------------ tiles

_EVEN = [(1, 0), (0, -1), (-1, -1), (-1, 0), (-1, 1), (0, 1)]
_ODD = [(1, 0), (1, -1), (0, -1), (-1, 0), (0, 1), (1, 1)]
_DIRS = ["e", "ne", "nw", "w", "sw", "se"]


def resolve_ref(ref: str, anchors: dict, width: int, height: int) -> tuple[int, int]:
    """An anchor, "(x,y)", or a walk from either: see tests/rules/README.md."""
    parts = ref.split(">")
    base = parts[0].strip()
    at = anchors.get(base)
    if at is None:
        m = re.fullmatch(r"\(\s*(-?\d+)\s*,\s*(-?\d+)\s*\)", base)
        if not m:
            raise ScriptError(f"`{ref}` is no tile: {base!r} is neither an anchor ({', '.join(sorted(anchors))}) "
                              f"nor (x,y)")
        at = (int(m.group(1)), int(m.group(2)))
    x, y = at
    for step in parts[1:]:
        step = step.strip()
        if step not in _DIRS:
            raise ScriptError(f"`{ref}`: {step!r} is no direction ({', '.join(_DIRS)})")
        dx, dy = (_EVEN if y % 2 == 0 else _ODD)[_DIRS.index(step)]
        x, y = x + dx, y + dy
        if not (0 <= x < width and 0 <= y < height):
            raise ScriptError(f"`{ref}` walks off the map")
    return x, y


# ------------------------------------------------------------------------------------------------ lint

_NUMERIC = re.compile(r"\s*[+-]?(?=[0-9.]*[0-9])[0-9]*\.?[0-9]*\s*")


def number_as_string(v, at: str) -> Optional[str]:
    """Where a value types a number as a string (expressions aside)."""
    if isinstance(v, str) and not v.startswith("=") and v.isascii() and _NUMERIC.fullmatch(v):
        return (f"{at} is the number {json.dumps(v)} typed as a string; write it as a number, or add coerce = true "
                f"to test the coercion on purpose")
    if isinstance(v, list):
        for i, x in enumerate(v):
            found = number_as_string(x, f"{at}[{i}]")
            if found:
                return found
    if isinstance(v, dict):
        for k, x in v.items():
            found = number_as_string(x, f"{at}.{k}")
            if found:
                return found
    return None


# ------------------------------------------------------------------------------------------------ bots

@functools.cache
def bot_parameters() -> dict:
    """basic-1's parameters by key, from its schema (crates/citar-bot/params/basic-1.json)."""
    doc = json.loads(BOT_SCHEMA.read_text(encoding="utf-8"))
    return {s["key"]: s for g in doc["groups"] for s in g["params"]}


def unpinned_draws(params: dict) -> Optional[str]:
    """Why a bot turn with these params could draw a result a script checks, or None when every draw is pinned.

    The bot's draws (DESIGN.md P2.3.5) are a tech's noise, ranged or melee, a peace offer, a friendship and a war's
    preparation; each is pinned when its chance is 0 or 1, so the draw's value cannot matter. The spies' draw only
    breaks ties between capitals: a script that moves spies has one capital to send them to.
    """
    missing = [k for k in PINS if k not in params]
    if missing:
        return f"params must give {', '.join(missing)}"
    for k in PINS:
        if not _is_num(params[k]):
            return f"{k} is a number"
    if params["tech_noise"] != 0:
        return "tech_noise is 0"
    for k in ("ranged_chance", "peace_offer_chance", "friend_chance", "war_chance"):
        if params[k] not in (0, 1):
            return f"{k} is 0 or 1"
    for k in ("friend_chance_aggr", "war_chance_aggr"):
        if params[k] != 0:
            return f"{k} is 0"
    rate = params.get("war_prep_rate", bot_parameters()["war_prep_rate"]["default"])
    if not _is_num(rate) or 0 < params["war_chance"] * rate < 1:
        return "war_prep_rate keeps the war chance 0 or at least 1"
    return None


# ------------------------------------------------------------------------------------------------ scripts

class Script:
    """A script, read and checked for its shape."""

    def __init__(self, name: str, doc: dict):
        unknown = [k for k in doc if k not in TOP]
        if unknown:
            raise ScriptError(f"{name}: unknown key {unknown[0]!r} (a script has {', '.join(TOP)})")
        about = doc.get("about")
        if not isinstance(about, str) or not about.strip():
            raise ScriptError(f"{name}: `about` must say what the script pins down")
        start = doc.get("start", "bare")
        if start not in ("bare", "full"):
            raise ScriptError(f'{name}: start is "bare" or "full"')
        config = doc.get("config", {})
        steps = doc.get("step", [])
        if not isinstance(config, dict):
            raise ScriptError(f"{name}: config is a table")
        if not isinstance(steps, list):
            raise ScriptError(f"{name}: steps are [[step]] tables")
        needs = doc.get("needs")
        if needs is not None and not (isinstance(needs, str) and NEEDS.fullmatch(needs)):
            raise ScriptError(f'{name}: needs names a package, as "2-01a"')
        self.name, self.about, self.map = name, about, doc.get("map", "arena")
        self.bare, self.config, self.steps = start == "bare", config, steps
        self.needs = needs


def load(path: Path) -> Script:
    """Read a script."""
    with open(path, "rb") as fh:
        return Script(path.stem, tomllib.load(fh))


class Runner:
    """Plays one script on the Python engine."""

    def __init__(self, script: Script):
        self.script = script
        doc = json.loads((RULES / "maps" / f"{script.map}.json").read_text(encoding="utf-8"))
        self.anchors = {k: tuple(v) for k, v in doc.pop("anchors", {}).items()}
        self.doc, self.width, self.height = doc, doc["width"], doc["height"]
        self.vars: dict = {}
        self.bots: dict = {}             # the seats' bots, by (player, version, aggression, params)
        self.intended = intended_ids()
        self.game = self.make_game({})
        self.test_op_names = {o["op"] for o in self.game.inspect({"what": "ops"})["test"]}

    def run(self):
        for i, step in enumerate(self.script.steps):
            self.step(step, f"step {i + 1}")

    def make_game(self, overrides: dict) -> EngineGame:
        cfg: dict = {"seed": 1, "players": [{}, {}]}
        if self.script.bare:
            cfg.update({"city_states": 0, "barbarians": "off", "ruins": False})
        cfg.update(plain(self.script.config))
        cfg.update(plain(overrides))
        if isinstance(cfg.get("players"), list):
            for p in cfg["players"]:
                if isinstance(p, dict) and not p.get("nation"):
                    p["nation"] = "BenchmarkCiv"
        cfg["map"] = plain(self.doc)
        g = EngineGame.new(cfg)
        if self.script.bare:
            g.test_ops([{"op": "clear_units", "player": "all"}, {"op": "clear_camps"}])
        return g

    # ---- steps

    def step(self, s, label: str):
        if not isinstance(s, dict):
            raise ScriptError(f"{label}: a step is a table")
        want = s.get("must_fail", False)
        if want is False:
            self.step_inner(s, label)
            return
        try:
            self.step_inner(s, label)
        except ScriptError as e:
            if want is True or (isinstance(want, str) and want in str(e)):
                return
            if isinstance(want, str):
                raise ScriptError(f"{label}: failed, but not with {json.dumps(want)}: {e}")
            raise ScriptError(f"{label}: must_fail is true or a text")
        raise ScriptError(f"{label}: must fail, but passed")

    def step_inner(self, s: dict, label: str):
        kinds = [k for k in KINDS if k in s]
        if len(kinds) != 1:
            raise ScriptError(f"{label}: a step has exactly one of {', '.join(KINDS)}")
        kind = kinds[0]
        for k in s:
            if not (k == kind or k in COMMON or k in OWN.get(kind, ())
                    or (kind == "check" and (k in MATCHERS or k in WITH))):
                raise ScriptError(f"{label}: a {kind} step has no key {json.dumps(k)}")
        if "intended" in s:
            if s["intended"] not in self.intended:
                raise ScriptError(f"{label}: intended = {json.dumps(s['intended'])} is in neither "
                                  f"refcheck/intended.toml nor tests/rules/intended.toml")
            return
        if s.get("coerce") is not True:
            for key in ("args", "ops", "new_game", "params"):
                if key in s:
                    bad = number_as_string(s[key], key)
                    if bad:
                        raise ScriptError(f"{label}: {bad}")
            if isinstance(s.get("check"), dict):
                bad = number_as_string(s["check"], "check")
                if bad:
                    raise ScriptError(f"{label}: {bad}")
        getattr(self, "do_" + kind)(s, label)

    def do_op(self, s: dict, label: str):
        name = s["op"]
        if not isinstance(name, str):
            raise ScriptError(f"{label}: op is a name")
        o = {"op": name}
        o.update(self.args(s.get("args"), label))
        try:
            if name in self.test_op_names:
                done = self.game.test_ops([o])[0]
            else:
                done = self.game.apply_ops([o])[0]
            self.outcome(s, label, plain(done), None)
        except (ActionError, ValueError) as e:
            self.outcome(s, label, None, str(e))

    def do_ops(self, s: dict, label: str):
        if not isinstance(s["ops"], list):
            raise ScriptError(f"{label}: ops is a list of op tables")
        ops = [self.args(o, label) for o in s["ops"]]
        try:
            self.outcome(s, label, plain(self.game.apply_ops(ops)), None)
        except (ActionError, ValueError) as e:
            self.outcome(s, label, None, str(e))

    def do_tool(self, s: dict, label: str):
        name = s["tool"]
        if not isinstance(name, str):
            raise ScriptError(f"{label}: tool is a name")
        if "player" in s:
            v = self.value(s["player"], label)
            try:
                pid = int(v)
            except (TypeError, ValueError):
                raise ScriptError(f"{label}: player {show(v)} is no player id")
        else:
            pid = self.game.current
        args = self.args(s.get("args"), label)
        try:
            self.outcome(s, label, plain(self.game.execute(pid, name, args)), None)
        except (ActionError, ValueError) as e:
            self.outcome(s, label, None, str(e))

    def outcome(self, s: dict, label: str, done, error: Optional[str]):
        want = s.get("error", False)
        if want is False:
            if error is not None:
                raise ScriptError(f"{label}: {error}")
            if isinstance(s.get("as"), str):
                self.vars[s["as"]] = done
            return
        if error is None:
            raise ScriptError(f"{label}: expected an error, but it succeeded: {show(done)}")
        if want is True:
            return
        if isinstance(want, str):
            if want not in error:
                raise ScriptError(f"{label}: expected an error with {json.dumps(want)}, got: {error}")
            return
        raise ScriptError(f"{label}: error is true or a text ({error})")

    def do_check(self, s: dict, label: str):
        subject = s["check"]
        if isinstance(subject, str):
            if subject not in self.vars:
                raise ScriptError(f"{label}: no variable {subject}")
            what, value = subject, self.vars[subject]
        elif isinstance(subject, dict):
            q = self.args(subject, label)
            what = q.get("what", "?")
            try:
                value = plain(self.game.inspect(q))
            except (ActionError, ValueError) as e:
                raise ScriptError(f"{label}: {e}")
        else:
            raise ScriptError(f"{label}: check is a query table or a variable")
        p = s.get("path")
        if p is not None and not isinstance(p, str):
            raise ScriptError(f"{label}: path is a string")
        try:
            segs = parse_path(p) if p is not None else []
        except ScriptError as e:
            raise ScriptError(f"{label}: {e}")
        at = get(value, segs)
        spec = {k: self.value(v, label) for k, v in s.items() if k in MATCHERS or k in WITH}
        place = f" {p}" if p is not None else ""
        try:
            check(at, spec)
        except ScriptError as e:
            raise ScriptError(f"{label} ({what}{place}): {e}")
        if isinstance(s.get("as"), str) and at is not _MISSING:
            self.vars[s["as"]] = at

    def do_new_game(self, s: dict, label: str):
        over = self.value(s["new_game"], label)
        if not isinstance(over, dict):
            raise ScriptError(f"{label}: new_game is a table of settings")
        try:
            g = self.make_game(over)
        except (ActionError, ValueError) as e:
            self.outcome(s, label, None, str(e))
            return
        if s.get("error", False) is not False:
            raise ScriptError(f"{label}: expected the settings to be refused")
        self.game = g
        self.bots = {}

    def do_set(self, s: dict, label: str):
        if not isinstance(s["set"], dict):
            raise ScriptError(f"{label}: set is a table")
        for k, v in s["set"].items():
            self.vars[k] = self.value(v, label)

    def do_repeat(self, s: dict, label: str):
        n = self.value(s["repeat"], label)
        if not _is_num(n) or n < 0 or int(n) != n:
            raise ScriptError(f"{label}: repeat is a count")
        for rnd in range(1, int(n) + 1):
            for j, st in enumerate(s.get("steps", [])):
                self.step(st, f"{label} (round {rnd}, step {j + 1})")

    def do_bot(self, s: dict, label: str):
        """A seat's bot plays its turn, answers a negotiation, or gives its advice (tests/rules/README.md, `bot`)."""
        ask = s["bot"]
        if ask not in BOT_ASKS:
            raise ScriptError(f"{label}: bot is one of {', '.join(BOT_ASKS)}, not {show(ask)}")
        if "player" not in s:
            raise ScriptError(f"{label}: a bot step names its player")
        pid = self.value(s["player"], label)
        if not _is_num(pid) or int(pid) != pid:
            raise ScriptError(f"{label}: player {show(pid)} is no player id")
        pid = int(pid)
        version = self.value(s.get("version", "basic-1"), label)
        if version not in BOT_VERSIONS:
            raise ScriptError(f"{label}: version is one of {', '.join(BOT_VERSIONS)}, not {show(version)}")
        aggression = self.value(s.get("aggression", 0.4), label)
        if not _is_num(aggression):
            raise ScriptError(f"{label}: aggression is a number")
        params = self.value(s.get("params", {}), label)
        owners = self.value(s.get("diplomacy", {}), label)
        if not isinstance(params, dict) or not isinstance(owners, dict):
            raise ScriptError(f"{label}: params and diplomacy are tables")
        if version == "idle" and (params or owners):
            raise ScriptError(f"{label}: the idle bot takes no params and no diplomacy")
        known = bot_parameters()
        unknown = [k for k in params if k not in known]
        if unknown:
            raise ScriptError(f"{label}: params: {unknown[0]} is not a parameter of basic-1")
        if ask == "turn" and version != "idle":
            unpinned = unpinned_draws(params)
            if unpinned:
                raise ScriptError(f"{label}: a bot turn pins its draws: {unpinned}")
        if ask == "turn" and "negotiation" in s:
            raise ScriptError(f"{label}: a bot turn takes no negotiation")
        if ask == "advice" and version == "idle":
            raise ScriptError(f"{label}: the idle bot gives no advice")
        if ask == "respond" and "negotiation" not in s:
            raise ScriptError(f"{label}: a bot respond step names its negotiation")
        nid = self.value(s["negotiation"], label) if "negotiation" in s else None
        if nid is not None and (not _is_num(nid) or int(nid) != nid):
            raise ScriptError(f"{label}: negotiation {show(nid)} is no negotiation id")
        nid = None if nid is None else int(nid)
        bot = self.seat_bot(pid, version, float(aggression), params)
        if version != "idle":
            try:
                engine_api.bot_set_diplomacy(bot, owners)
            except ValueError as e:
                raise ScriptError(f"{label}: diplomacy: {e}")
        g = self.game
        if ask == "turn":
            if g.phase != "playing" or g.current != pid:
                self.outcome(s, label, None, f"It is not player {pid}'s turn.")
                return
            try:
                g.play_bot_turn(pid, bot, end_turn=True)
                if g.phase == "playing" and g.current == pid:
                    g.execute(pid, "end_turn", {})          # the idle bot plays without ending its turn
            except ActionError as e:
                raise ScriptError(f"{label}: the bot's turn did not end: {e}")
            self.outcome(s, label, {"turn": g.turn, "current": g.current}, None)
            return
        if ask == "respond":
            try:
                n = g.negotiation(nid)
            except ActionError as e:
                self.outcome(s, label, None, str(e))
                return
            if n["status"] != "open" or n["awaiting"] != pid:
                self.outcome(s, label, None, f"Negotiation {nid} does not wait on player {pid}.")
                return
            if not engine_api.bot_owns_negotiation(bot, n):
                self.outcome(s, label, {"outcome": "deferred"}, None)
                return
            g.bot_respond(pid, nid, bot)
            self.outcome(s, label, {"outcome": "done"}, None)
            return
        try:
            advice = g.bot_advice(pid, bot, nid)
        except ActionError as e:
            self.outcome(s, label, None, str(e))
            return
        self.outcome(s, label, plain(advice), None)

    def seat_bot(self, pid: int, version: str, aggression: float, params: dict):
        """The seat's bot for a step: the one its earlier steps used, while the version, aggression and params
        stay the same, else a new one.

        A game keeps its seats' bots from turn to turn, and what a bot remembers (an escort, a war it prepares, a
        site it gave up) lasts with it, as the Rust bot's memory lasts in the seat's DriverMemory whatever the
        handle. A script that relies on that memory gives a seat the same version, aggression and params in every
        step. The seed is ignored on the Rust engine, which keys the bot's draws by the game's seed (DESIGN.md
        P2.3.5); here it only has to be fixed, since a turn step pins every draw a script could see.
        """
        key = (pid, version, aggression, json.dumps(params, sort_keys=True))
        bot = self.bots.get(key)
        if bot is None:
            engine = "idle" if version == "idle" else "basic"
            bot = engine_api.bot_instance(engine, seed=0, aggression=aggression, params=params or None)
            self.bots = {k: b for k, b in self.bots.items() if k[0] != pid}
            self.bots[key] = bot
        return bot

    # ---- values

    def args(self, v, label: str) -> dict:
        out = {} if v is None else self.value(v, label)
        if not isinstance(out, dict):
            raise ScriptError(f"{label}: args is a table")
        if "at" in out:
            at = out.pop("at")
            if "x" in out or "y" in out:
                raise ScriptError(f"{label}: at gives x and y; do not give them too")
            try:
                out["x"], out["y"] = self.tile(at)
            except ScriptError as e:
                raise ScriptError(f"{label}: {e}")
        return out

    def value(self, v, label: str):
        if isinstance(v, str) and v.startswith("=="):
            return v[1:]
        if isinstance(v, str) and v.startswith("="):
            try:
                return _Expr(v[1:], self).run()
            except ScriptError as e:
                raise ScriptError(f"{label}: {e}")
        if isinstance(v, list):
            return [self.value(x, label) for x in v]
        if isinstance(v, dict):
            return {k: self.value(x, label) for k, x in v.items()}
        return v

    def tile(self, ref) -> tuple[int, int]:
        """A tile reference: text for the map, or a selector table for find_tiles."""
        if isinstance(ref, str):
            return resolve_ref(ref, self.anchors, self.width, self.height)
        if isinstance(ref, dict):
            q = dict(ref)
            pick = q.pop("pick", 0)
            if "at" in q:
                q["x"], q["y"] = self.tile(q.pop("at"))
            q["what"] = "find_tiles"
            try:
                found = self.game.inspect(q)
            except (ActionError, ValueError) as e:
                raise ScriptError(str(e))
            if not isinstance(pick, int) or not 0 <= pick < len(found):
                raise ScriptError(f"the selector {show(ref)} finds no tile {pick}")
            return found[pick]["x"], found[pick]["y"]
        raise ScriptError(f"{show(ref)} is no tile reference")


def run(path: Path):
    """Play a script; raises ScriptError saying which step failed and why."""
    script = load(path)
    Runner(script).run()
