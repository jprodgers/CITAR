"""Shapes of plain data under the facade's type classes, for tests/test_facade_parity.py (DESIGN.md P2.6.6).

Two backends' answers to the same call are compared by shape, not by value: the two engines play different games from
the same seed. A shape keeps what a caller relies on:

- **Type classes.** int and float are one number class (JSON has one), bool is its own, str, bytes; ``None`` matches
  any type (a value the docstring makes optional is None in one game and set in the other).
- **Records** (dicts with fixed keys) keep their key sets, which must be equal, and each key's shape. A key that some
  of a collection's records have and others lack is optional there, and either side may lack it (a unit's
  ``fortified_turns``, a fortified unit's alone).
- **Maps** (dicts keyed by ids, or by names that are no snake-case field: a ruleset's or a game's) keep the kind of
  their keys, int, id-as-text or name, which must agree, and their values' merged shape.
- **Collections** (lists, tuples, sets) keep their elements' merged shape; an empty collection or map matches any
  element type. A list of records tagged by a ``type`` field in snake case (events, deal items) is merged tag by
  tag, and only tags both sides have are compared: which kinds of event a game had is the game's, not the
  backend's. (A ``type`` that is a ruleset's name, a unit's, is data: those records merge as one.)
"""
from __future__ import annotations

import re
from typing import Any

NONE = ("none",)
EMPTY = ("empty",)


#: A record's field: the facade's dicts name their fields in snake case. A dict keyed otherwise is keyed by the
#: ruleset's or the game's names (a source of yields, a victory, a tech) and is a map.
_FIELD = re.compile(r"[a-z_][a-z0-9_]*")


def _is_id_text(k) -> bool:
    return isinstance(k, str) and k.lstrip("-").isdigit()


def shape(v: Any):
    """The shape of a value."""
    if v is None:
        return NONE
    if isinstance(v, bool):
        return ("bool",)
    if isinstance(v, (int, float)):
        return ("num",)
    if isinstance(v, str):
        return ("str",)
    if isinstance(v, (bytes, bytearray)):
        return ("bytes",)
    if isinstance(v, (list, tuple, set, frozenset)):
        items = list(v)
        if not items:
            return EMPTY
        kind = "set" if isinstance(v, (set, frozenset)) else "list"
        if all(isinstance(x, dict) and isinstance(x.get("type"), str) and _FIELD.fullmatch(x["type"]) for x in items):
            tags: dict = {}
            for x in items:
                tags[x["type"]] = merge(tags.get(x["type"], NONE), shape(x))
            return ("tagged", tags)
        out = NONE
        for x in items:
            out = merge(out, shape(x))
        return (kind, out)
    if isinstance(v, dict):
        if not v:
            return EMPTY
        if all(isinstance(k, int) and not isinstance(k, bool) for k in v):
            key = "int"
        elif all(_is_id_text(k) for k in v):
            key = "text"
        elif all(isinstance(k, str) and _FIELD.fullmatch(k) for k in v):
            return ("dict", {k: shape(x) for k, x in v.items()}, frozenset())
        else:
            key = "name"
        out = NONE
        for x in v.values():
            out = merge(out, shape(x))
        return ("map", key, out)
    return ("other", type(v).__name__)


def merge(a, b):
    """One shape for two values found in the same place (a collection's elements, a map's values)."""
    if a == NONE or a == EMPTY:
        return b if b != NONE else a
    if b == NONE or b == EMPTY:
        return a
    if a == b:
        return a
    if a[0] == b[0] == "dict":
        keys = {**a[1], **b[1]}
        optional = frozenset(set(a[1]) ^ set(b[1])) | a[2] | b[2]
        return ("dict", {k: merge(a[1].get(k, NONE), b[1].get(k, NONE)) for k in keys}, optional)
    if a[0] == b[0] and a[0] in ("list", "set"):
        return (a[0], merge(a[1], b[1]))
    if a[0] == b[0] == "map" and a[1] == b[1]:
        return ("map", a[1], merge(a[2], b[2]))
    if a[0] == b[0] == "tagged":
        tags = {**a[1], **b[1]}
        return ("tagged", {t: merge(a[1].get(t, NONE), b[1].get(t, NONE)) for t in tags})
    members = []
    for m in _members(a) + _members(b):
        if m not in members:
            members.append(m)
    return ("union", tuple(members))


def _members(s) -> list:
    """A shape as the list of what a union of it holds."""
    return list(s[1]) if s[0] == "union" else [s]


def _same_members(a, b) -> bool:
    left, right = _members(a), _members(b)
    return all(m in right for m in left) and all(m in left for m in right)


def show(s) -> str:
    """A shape, short, for a message."""
    if s[0] in ("dict",):
        return "dict{" + ",".join(sorted(s[1])) + "}"
    if s[0] in ("list", "set"):
        return f"{s[0]}[{show(s[1])}]"
    if s[0] == "map":
        return f"map<{s[1]}>[{show(s[2])}]"
    if s[0] == "tagged":
        return "tagged{" + ",".join(sorted(s[1])) + "}"
    if s[0] == "union":
        return "|".join(sorted(show(x) for x in s[1]))
    return s[0]


def differences(a, b, path: str = "$") -> list[str]:
    """Where two shapes disagree, as "path: what" lines; none when they match under the type classes."""
    if a == NONE or b == NONE or a == b:
        return []
    if a == EMPTY or b == EMPTY:
        other = b if a == EMPTY else a
        if other[0] in ("list", "set", "map", "tagged", "dict"):
            return []
        return [f"{path}: an empty collection against {show(other)}"]
    if a[0] != b[0]:
        if (a[0] == "union" or b[0] == "union") and _same_members(a, b):
            return []
        return [f"{path}: {show(a)} against {show(b)}"]
    kind = a[0]
    if kind == "dict":
        out = []
        optional = a[2] | b[2]
        only_a = sorted(set(a[1]) - set(b[1]) - optional)
        only_b = sorted(set(b[1]) - set(a[1]) - optional)
        if only_a or only_b:
            out.append(f"{path}: keys only on the first: {only_a}; only on the second: {only_b}")
        for k in sorted(set(a[1]) & set(b[1])):
            out += differences(a[1][k], b[1][k], f"{path}.{k}")
        return out
    if kind in ("list", "set"):
        return differences(a[1], b[1], f"{path}[]")
    if kind == "map":
        out = [] if a[1] == b[1] else [f"{path}: keys are {a[1]} against {b[1]}"]
        return out + differences(a[2], b[2], f"{path}{{}}")
    if kind == "tagged":
        out = []
        for t in sorted(set(a[1]) & set(b[1])):
            out += differences(a[1][t], b[1][t], f"{path}[type={t}]")
        return out
    if kind == "union":
        return [] if _same_members(a, b) else [f"{path}: {show(a)} against {show(b)}"]
    return [f"{path}: {show(a)} against {show(b)}"]


def compare(a: Any, b: Any, path: str = "$") -> list[str]:
    """Where two values' shapes disagree."""
    return differences(shape(a), shape(b), path)
