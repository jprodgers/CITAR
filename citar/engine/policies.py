"""Social policies (port of UnCiv's PolicyManager, MPL-2.0).

Culture accumulates civ-wide; adopting a branch opener or a member policy costs
25 + (3n)^2.01 culture (n = policies adopted so far), scaled by the number of cities, map size, uniques, difficulty
and game speed. Completing all five members of a branch adopts its "<Branch> Complete" finisher for free.
"""
from __future__ import annotations

from typing import Optional, TYPE_CHECKING

from . import unique_types as U
from .game import ActionError
from .uniques import Ctx, applies

if TYPE_CHECKING:
    from .game import Game


def policy_def(g: "Game", name: str) -> Optional[dict]:
    """A policy or branch's ruleset definition."""
    R = g.rules
    return R.policy_branches.get(name) or R.policies.get(name)


def is_branch(g: "Game", name: str) -> bool:
    """Whether a name refers to a whole branch rather than a policy in one."""
    return name in g.rules.policy_branches


def branch_of(g: "Game", name: str) -> str:
    """The branch a policy belongs to."""
    return name if is_branch(g, name) else g.rules.policies[name]["branch"]


def culture_cost(g: "Game", pid: int, n: Optional[int] = None) -> int:
    """Culture for the next policy, which rises with how many have been adopted."""
    p = g.player(pid)
    if n is None:
        n = p.policies_adopted_count
    cost = 25 + (n * 3) ** 2.01
    pre = g.rules.map_size_predefined(g.s.width, g.s.height)
    city_mod = pre["policy_cost_per_city"] * (sum(1 for c in g.player_cities(pid) if not c.puppet) - 1)
    for u in g.civ_uniques(pid, U.LessPolicyCostFromCities):
        city_mod *= 1 - u.n(0) / 100
    for u in g.civ_uniques(pid, U.LessPolicyCost):
        cost *= 1 + u.n(0) / 100
    if g.is_humanlike(pid):
        from .economy import difficulty
        cost *= difficulty(g, pid)["policyCostModifier"]
    cost *= g.speed["cultureCostModifier"]
    c = int(round(cost * (1 + city_mod)))
    return c - (c % 5)


def adoptable(g: "Game", pid: int, name: str, check_era: bool = True) -> Optional[str]:
    """None if the policy can be adopted now (ignoring cost), else the reason."""
    R = g.rules
    p = g.player(pid)
    d = policy_def(g, name)
    if d is None:
        return f"Unknown policy '{name}'."
    if name in p.policies:
        return f"{name} is already adopted."
    if d.get("is_finisher"):
        return "Branch finishers are gained automatically when a branch is complete."
    req = d.get("requires", [])
    missing = [r for r in req if r not in p.policies]
    if missing:
        return f"{name} requires {', '.join(missing)}."
    branch = R.policy_branches[branch_of(g, name)]
    from .research import player_era
    if check_era and R.eras[branch["era"]]["number"] > player_era(g, pid):
        return f"The {branch['name']} branch unlocks in the {branch['era']}."
    ctx = Ctx(g, civ=pid)
    for u in d["_umap"].get(U.OnlyAvailable):
        if not applies(u, ctx):
            blocked = [m.p(0) for m in u.mods if m.ph == "before adopting []"]
            return f"{name} is not available" + (f" after adopting {', '.join(blocked)}." if blocked else ".")
    if d["_umap"].has(U.Unavailable, ctx):
        return f"{name} is unavailable."
    return None


def adoptable_policies(g: "Game", pid: int) -> list[str]:
    """Policies and branches this civilization could adopt now."""
    R = g.rules
    out = []
    for n in list(R.policy_branches) + list(R.policies):
        if adoptable(g, pid, n) is None:
            out.append(n)
    return out


def can_adopt_any(g: "Game", pid: int) -> bool:
    """Whether there is anything adoptable at all."""
    p = g.player(pid)
    if p.kind != "major":
        return False
    if p.free_policies == 0 and p.culture < culture_cost(g, pid):
        return False
    return bool(adoptable_policies(g, pid))


def completed_branches(g: "Game", pid: int) -> int:
    """How many branches this civilization has finished, which several rules count."""
    p = g.player(pid)
    return sum(1 for b in g.rules.policy_branches if f"{b} Complete" in p.policies)


def adopt(g: "Game", pid: int, name: str, free: bool = False, _completion: bool = False) -> dict:
    """Adopt a policy or open a branch, spending the culture it costs."""
    from . import triggers
    R = g.rules
    p = g.player(pid)
    if not _completion:
        resolved = R.resolve("policy", name)
        if resolved is None:
            raise ActionError(f"Unknown policy '{name}'. Use names like 'Tradition' or 'Aristocracy'.")
        name = resolved
        reason = adoptable(g, pid, name)
        if reason:
            raise ActionError(reason)
        if p.free_policies > 0:
            p.free_policies -= 1
        else:
            cost = culture_cost(g, pid)
            if p.culture < cost:
                raise ActionError(f"Adopting a policy costs {cost} culture; you have {int(p.culture)}.")
            p.culture -= cost
            p.policies_adopted_count += 1
    p.policies.append(name)
    g.invalidate()
    d = policy_def(g, name)
    if not _completion:
        br = branch_of(g, name)
        members = R.policy_branches[br]["members"]
        if all(m in p.policies for m in members) and f"{br} Complete" not in p.policies:
            adopt(g, pid, f"{br} Complete", _completion=True)
    ctx = Ctx(g, civ=pid)
    for u in d["_umap"].all:
        if not triggers.is_triggerable(u) or triggers.has_trigger_conditional(u) or not applies(u, ctx):
            continue
        triggers.trigger(g, u, pid, note=f"due to adopting {name}")
    triggers.fire(g, pid, U.TriggerUponAdoptingPolicyOrBelief, filt=lambda u: u.p(0) == name,
                  note=f"due to adopting {name}")
    from .cities import assign_citizens, try_add_free_buildings
    try_add_free_buildings(g, pid)
    for c in g.player_cities(pid):
        assign_citizens(g, c)
    g.emit("policy", f"{p.name} adopted {name}.", None if _completion else [pid], policy=name, player=pid)
    return {"adopted": name, "culture_left": int(p.culture), "next_cost": culture_cost(g, pid),
            "free_policies": p.free_policies}


def end_turn(g: "Game", pid: int, culture: float):
    """Accumulate culture and announce when a policy can be adopted."""
    p = g.player(pid)
    could = can_adopt_any(g, pid)
    p.culture += int(culture)
    hist = p.flags.setdefault("culture_last8", [0] * 8)
    hist[g.turn % 8] = int(culture)
    if not could and can_adopt_any(g, pid):
        g.emit("policy_available", f"{p.name} can adopt a new social policy.", [pid])


def culture_from_great_writer(g: "Game", pid: int) -> int:
    """The culture a great writer is worth now, which scales with the game."""
    hist = g.player(pid).flags.get("culture_last8", [0] * 8)
    return int(sum(hist) * g.speed["cultureCostModifier"])
