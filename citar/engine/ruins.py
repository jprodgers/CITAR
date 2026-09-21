"""Ancient ruins: a major civ's unit entering ruins receives a random reward.
Port of UnCiv's RuinsManager and RuinReward (MPL-2.0). The last two rewards are not repeated.
"""
from __future__ import annotations

from typing import TYPE_CHECKING

from . import unique_types as U
from .uniques import Ctx, applies

if TYPE_CHECKING:
    from .game import Game
    from .state import Unit

RUINS = "Ancient ruins"


def _unavailable_by_settings(g: "Game", reward: dict) -> bool:
    """Ruin outcomes this game's settings rule out."""
    if g.difficulty_name() in reward.get("excludedDifficulties", []):
        return True
    return False


def _possible(g: "Game", pid: int, reward: dict, unit: "Unit", last: list) -> bool:
    """The ruin outcomes available to this civilization here."""
    if reward["name"] in last or _unavailable_by_settings(g, reward):
        return False
    ctx = Ctx(g, civ=pid, unit=unit, tile=unit.idx)
    for x in reward["_umap"].all:
        if x.ph == U.Unavailable and applies(x, ctx):
            return False
        if x.ph == U.OnlyAvailable and not applies(x, ctx):
            return False
    return True


def enter(g: "Game", unit: "Unit", idx: int) -> bool:
    """MapUnit.getAncientRuinBonus + RuinsManager.selectNextRuinsReward."""
    from .triggers import trigger
    g.s.tiles[idx].improvement = None
    g.invalidate()
    pid = unit.owner
    p = g.player(pid)
    last = p.flags.setdefault("last_ruins", ["", ""])
    candidates = []
    for r in g.rules.ruins.values():
        if _possible(g, pid, r, unit, last):
            candidates += [r] * int(r.get("weight", 1))
    rng = g.state_rng("ruins", idx, pid)
    rng.shuffle(candidates)
    ctx = Ctx(g, civ=pid, unit=unit, tile=idx)
    for r in candidates:
        any_effect = False
        for x in r["_umap"].all:
            if x.ph in (U.Unavailable, U.OnlyAvailable) or not applies(x, ctx):
                continue
            if trigger(g, x, pid, unit=unit if g.unit(unit.id) else None, tile=idx,
                       note=f"from the ruins ({r['name']})"):
                any_effect = True
        if any_effect:
            last[0], last[1] = last[1], r["name"]
            g.emit("ruins", f"{p.name}'s {unit.type} explored ancient ruins and found {r['name']}.", [pid], idx=idx,
                   reward=r["name"])
            return True
    g.emit("ruins", f"{p.name}'s {unit.type} explored ancient ruins but found nothing of value.", [pid], idx=idx)
    return False
