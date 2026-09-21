"""Technology research (port of UnCiv's TechManager, MPL-2.0)."""
from __future__ import annotations

from typing import Optional, TYPE_CHECKING

from . import unique_types as U
from .game import ActionError
from .uniques import Ctx, applies, tech_matches

if TYPE_CHECKING:
    from .game import Game


def is_repeatable(g: "Game", tech: str) -> bool:
    """Whether a technology can be researched more than once, as future technologies can."""
    return g.rules.techs[tech]["_umap"].has_tag(U.ResearchableMultipleTimes)


def science_modifier(g: "Game", pid: int, tech: str) -> float:
    """Techs known by civs you have met are cheaper (UnCiv getScienceModifier)."""
    known = sum(1 for q in g.majors() if q.id != pid and g.has_met(pid, q.id) and g.has_tech(q.id, tech))
    remaining = max(1, len(g.majors()))
    return 1 + known / remaining * 0.3


def tech_cost(g: "Game", pid: int, tech: str) -> int:
    """What a technology costs this civilization, after speed, difficulty and uniques."""
    key = ("tech_cost", pid, tech)
    v = g._ycache.get(key)
    if v is not None:
        return v
    R = g.rules
    cost = float(R.techs[tech]["cost"])
    if g.is_humanlike(pid):
        from .economy import difficulty
        cost *= difficulty(g, pid)["researchCostModifier"]
    cost *= g.speed["scienceCostModifier"]
    cost /= science_modifier(g, pid, tech)
    pre = R.map_size_predefined(g.s.width, g.s.height)
    cost *= pre["tech_cost_multiplier"]
    city_mod = (sum(1 for c in g.player_cities(pid) if not c.puppet) - 1) * pre["tech_cost_per_city"]
    for u in g.civ_uniques(pid, U.LessTechCostFromCities):
        city_mod *= 1 - u.n(0) / 100
    for u in g.civ_uniques(pid, U.LessTechCost):
        cost *= 1 + u.n(0) / 100
    cost *= 1 + city_mod
    v = int(cost)
    g._ycache[key] = v
    return v


def is_unresearchable(g: "Game", pid: int, tech: str) -> bool:
    """Whether a technology can never be researched in this game."""
    td = g.rules.techs[tech]
    ctx = Ctx(g, civ=pid)
    if any(not applies(u, ctx) for u in td["_umap"].get(U.OnlyAvailable)):
        return True
    return td["_umap"].has(U.Unavailable, ctx)


def can_research(g: "Game", pid: int, tech: str) -> bool:
    """Whether this civilization could research a technology now."""
    td = g.rules.techs.get(tech)
    if td is None or is_unresearchable(g, pid, tech):
        return False
    if g.has_tech(pid, tech) and not is_repeatable(g, tech):
        return False
    return all(g.has_tech(pid, p) for p in td["prerequisites"])


def available_techs(g: "Game", pid: int) -> list[str]:
    """Technologies whose prerequisites are met and which are not yet known."""
    return [t for t in g.rules.tech_order if can_research(g, pid, t)]


def all_researched(g: "Game", pid: int) -> bool:
    """Whether there is nothing left to research."""
    return all(g.has_tech(pid, t) or not can_research(g, pid, t) for t in g.rules.techs)


def path_to(g: "Game", pid: int, goal: str) -> list[str]:
    """The order in which to research a technology's missing prerequisites.

    What makes naming a distant technology a single decision rather than a plan the player has to
    maintain.
    """
    R = g.rules
    if is_unresearchable(g, pid, goal):
        return []
    out, stack, seen = [], [goal], set()
    while stack:
        t = stack.pop(0)
        if is_unresearchable(g, pid, t):
            return []
        if not is_repeatable(g, t) and (g.has_tech(pid, t) or t in seen):
            continue
        seen.add(t)
        stack.extend(R.techs[t]["prerequisites"])
        out.append(t)
    return sorted(out, key=lambda t: (R.techs[t]["column"], R.tech_order.index(t)))


def current(g: "Game", pid: int) -> Optional[str]:
    """What this civilization is researching now."""
    q = g.player(pid).research_queue
    return q[0] if q else None


def set_research(g: "Game", pid: int, tech: str) -> dict:
    """Set the current research, or a goal whose prerequisites are researched first."""
    name = g.rules.resolve("tech", tech)
    if name is None:
        raise ActionError(f"Unknown technology '{tech}'. Use names like 'Bronze Working'.")
    p = g.player(pid)
    if g.has_tech(pid, name) and not is_repeatable(g, name):
        raise ActionError(f"You already know {name}.")
    path = path_to(g, pid, name)
    if not path:
        raise ActionError(f"{name} cannot be researched.")
    p.research_queue = path
    p.research_goal = name if len(path) > 1 else None
    update_research_progress(g, pid)
    out = {"researching": p.research_queue[0] if p.research_queue else None, "turns": turns_left(g, pid)}
    if len(path) > 1:
        out.update({"goal": name, "path": path})
    return out


def turns_left(g: "Game", pid: int, tech: Optional[str] = None) -> Optional[int]:
    """Turns until the current research completes at the current rate."""
    from .economy import civ_stats
    p = g.player(pid)
    tech = tech or current(g, pid)
    if not tech:
        return None
    remaining = tech_cost(g, pid, tech) - p.research_progress.get(tech, 0) - (p.overflow_science if can_research(g, pid, tech) else 0)
    sci = civ_stats(g, pid)["science"]
    if remaining <= 0:
        return 0
    if sci <= 0:
        return None
    return max(1, -(-int(remaining) // max(1, int(sci))))


def limit_overflow(g: "Game", pid: int, overflow: float) -> float:
    """Cap science carried over from a completed technology, so nothing is banked indefinitely."""
    from .economy import civ_stats
    cur = current(g, pid)
    cost = g.rules.techs[cur]["cost"] if cur else 0
    return min(overflow, max(civ_stats(g, pid)["science"] * 5, cost))


def end_turn(g: "Game", pid: int, science: float):
    """Apply a turn's science and complete anything that finished."""
    p = g.player(pid)
    hist = p.flags.setdefault("science_last8", [0] * 8)
    hist[g.turn % 8] = int(science)
    if current(g, pid) is None:
        p.overflow_science += 0
        return
    add = int(science)
    ra = p.flags.get("ra_science", 0)
    if ra:
        mod = 0.5 + sum(u.n(0) / 200 for u in g.civ_uniques(pid, U.ScienceFromResearchAgreements))
        boost = int(ra / 3 * mod)
        add += boost
        p.flags["ra_science"] = 0
        g.emit("tech", f"{p.name} gained {boost} science from research agreements.", [pid])
    if p.overflow_science:
        add += int(p.overflow_science)
        p.overflow_science = 0
    add_science(g, pid, add)


def add_science(g: "Game", pid: int, amount: float):
    """Add science to the current research, handling completion and overflow."""
    p = g.player(pid)
    cur = current(g, pid)
    if cur is None:
        p.overflow_science += amount
        return
    p.research_progress[cur] = p.research_progress.get(cur, 0) + amount
    cost = tech_cost(g, pid, cur)
    if p.research_progress[cur] < cost:
        return
    extra = p.research_progress[cur] - cost
    p.overflow_science += limit_overflow(g, pid, extra)
    add_tech(g, pid, cur)


def update_research_progress(g: "Game", pid: int):
    """Recalculate progress after something changed the cost or the rate."""
    p = g.player(pid)
    cur = current(g, pid)
    if cur is None:
        return
    real = p.overflow_science
    if p.research_progress.get(cur, 0) + real >= tech_cost(g, pid, cur):
        p.overflow_science = 0
        add_science(g, pid, real)


def free_tech(g: "Game", pid: int, tech: str) -> dict:
    """Grant a technology chosen from those currently researchable."""
    p = g.player(pid)
    name = g.rules.resolve("tech", tech)
    if p.free_techs <= 0:
        raise ActionError("You have no free technologies to choose.")
    if name is None or not can_research(g, pid, name):
        raise ActionError(f"{tech} cannot be chosen now (it must be researchable next).")
    p.free_techs -= 1
    add_tech(g, pid, name, source="free")
    return {"learned": name, "free_techs_left": p.free_techs}


def add_tech_silently(g: "Game", pid: int, tech: str):
    """Add a technology with no announcement, for scenario setup and similar."""
    if tech in g.rules.techs and not g.has_tech(pid, tech):
        g.player(pid).techs.append(tech)
        g.invalidate()


def player_era(g: "Game", pid: int) -> int:
    """Civ era (TechManager.updateEra): max era of researched techs, or the earliest era still unfinished if later."""
    p = g.player(pid)
    key = ("era", pid, len(p.techs))
    v = g._static.get(key)
    if v is not None:
        return v
    R = g.rules
    if not p.techs:
        v = 0
    else:
        max_col = max(R.techs[t]["column"] for t in p.techs)
        max_era = R.techs[next(t for t in p.techs if R.techs[t]["column"] == max_col)]["_era"]
        known = set(p.techs)
        remaining = [R.techs[t] for t in R.techs if t not in known]
        if not remaining:
            v = max_era
        else:
            min_era = min(remaining, key=lambda td: td["column"])["_era"]
            v = max_era if min_era <= max_era else min_era
    g._static[key] = v
    return v


def world_era(g: "Game") -> int:
    """The era most civilizations have reached, which some rules key off."""
    eras = sorted(player_era(g, p.id) for p in g.majors())
    return eras[len(eras) // 2] if eras else 0


def add_tech(g: "Game", pid: int, tech: str, source: str = "research"):
    """Grant a technology and apply everything that follows: era changes, obsolescence, triggers."""
    from . import triggers
    R = g.rules
    p = g.player(pid)
    before = player_era(g, pid)
    new = tech not in p.techs
    if is_repeatable(g, tech):
        p.future_techs += 1
        if tech not in p.techs:
            p.techs.append(tech)
    else:
        if tech not in p.techs:
            p.techs.append(tech)
        if tech in p.research_queue:
            p.research_queue.remove(tech)
    p.research_progress.pop(tech, None)
    g.invalidate()
    how = {"research": "researched", "ruins": "found in ruins", "trade": "acquired through a deal",
           "free": "chose as a free technology", "great_person": "discovered with a Great Scientist",
           "espionage": "stole", "research_agreement": "researched"}.get(source, source)
    un = R.unlocks.get(tech, {})
    bits = []
    for k in ("units", "buildings", "improvements"):
        if un.get(k):
            bits.append(f"{k}: " + ", ".join(un[k]))
    if un.get("reveals"):
        bits.append("reveals: " + ", ".join(un["reveals"]))
    if new or is_repeatable(g, tech):
        label = tech if not is_repeatable(g, tech) else f"{tech} {p.future_techs}"
        g.emit("tech", f"{p.name} {how} {label}." + (f" Unlocks {'; '.join(bits)}." if bits else ""), [pid], tech=tech)
    ctx = Ctx(g, civ=pid)
    for u in R.techs[tech]["_umap"].all:
        if triggers.has_trigger_conditional(u) or not triggers.is_triggerable(u) or not applies(u, ctx):
            continue
        triggers.trigger(g, u, pid, note=f"due to researching {tech}")
    triggers.fire(g, pid, U.TriggerUponResearch, filt=lambda u: tech_matches(R, tech, u.p(0)),
                  note=f"due to researching {tech}")
    _obsolete_queue(g, pid, tech)
    after = player_era(g, pid)
    if after > before:
        _enter_era(g, pid, before, after)
    for c in g.player_cities(pid):
        from .cities import assign_citizens
        assign_citizens(g, c)
    g.invalidate()
    update_research_progress(g, pid)


def _obsolete_queue(g, pid, tech):
    """Remove from production queues anything the new technology has made obsolete."""
    from .cities import equivalent_unit
    R = g.rules
    for c in g.player_cities(pid):
        newq = []
        changed = []
        for item in c.queue:
            ud = R.units.get(item)
            if ud and ud.get("obsoleteTech") == tech:
                up = ud.get("upgradesTo")
                repl = equivalent_unit(g, pid, up) if up else None
                if repl:
                    newq.append(repl)
                changed.append((item, repl))
            else:
                newq.append(item)
        if changed:
            c.queue = newq
            for old, new in changed:
                g.emit("production_invalid", f"{c.name} changed production from {old} to {new}." if new else
                       f"{old} became obsolete and was removed from {c.name}'s queue.", [pid], idx=c.idx)


def _enter_era(g, pid, before, after):
    """Handle a civilization entering a new era."""
    from . import triggers
    R = g.rules
    p = g.player(pid)
    era = R.era_list[after]
    if p.kind == "major":
        g.emit("era", f"{p.name} has entered the {era}.", None, player=pid, era=after)
        for br, bd in R.policy_branches.items():
            if bd.get("era") == era:
                g.emit("policy_available", f"The {br} policy branch is now available.", [pid])
    for n in range(before + 1, after + 1):
        ename = R.era_list[n]
        ctx = Ctx(g, civ=pid)
        for u in R.eras[ename]["_umap"].all:
            if triggers.has_trigger_conditional(u) or not applies(u, ctx):
                continue
            triggers.trigger(g, u, pid, note=f"due to entering the {ename}")
        triggers.fire(g, pid, U.TriggerUponEnteringEra, filt=lambda u, e=ename: u.p(0) == e,
                      note=f"due to entering the {ename}")


def median_available_cost(g: "Game", pid: int) -> float:
    """The median cost of what is currently researchable, used for pricing things against research."""
    costs = sorted(tech_cost(g, pid, t) for t in g.rules.techs if can_research(g, pid, t))
    if not costs:
        return 0.0
    n = len(costs)
    return costs[n // 2] if n % 2 else (costs[n // 2 - 1] + costs[n // 2]) / 2


def research_agreement_boost(g: "Game", pid: int):
    """Korean unique: half the median cost of researchable techs."""
    boost = round(0.5 * median_available_cost(g, pid))
    if boost:
        add_science(g, pid, boost)


def science_from_great_scientist(g: "Game", pid: int) -> int:
    """How much science a great scientist is worth now, which scales with the era."""
    hist = g.player(pid).flags.get("science_last8", [0] * 8)
    return int(sum(hist) * g.speed["scienceCostModifier"])
