"""Great people (points, births, free picks, actions) and golden ages.
Port of UnCiv's GreatPersonManager, GreatPersonPointsBreakdown, GoldenAgeManager and UnitActionsGreatPerson (MPL-2.0).
"""
from __future__ import annotations

from typing import Optional, TYPE_CHECKING

from . import unique_types as U
from .game import ActionError
from .uniques import city_matches

if TYPE_CHECKING:
    from .game import Game


# ---------------------------------------------------------------------------------------------------------------
# Points
# ---------------------------------------------------------------------------------------------------------------
def city_gpp_bonus(g: "Game", city) -> int:
    """Percentage bonuses applying to all great person points in a city."""
    from .cities import city_uniques
    total = 0
    for u in city_uniques(g, city, U.GreatPersonPointPercentage):
        if city_matches(g, city, u.p(1)):
            total += int(u.n(0))
    pid = city.owner
    for q in g.s.players:
        rel = g.relation(pid, q.id) if q.id != pid else None
        if rel and rel.get("friendship_until", 0) >= g.turn:
            for u in g.civ_uniques(pid, U.GreatPersonBoostWithFriendship) + g.civ_uniques(q.id, U.GreatPersonBoostWithFriendship):
                total += int(u.n(0))
    return total


def city_gpp(g: "Game", city) -> dict:
    """Great person points this city generates next turn (GreatPersonPointsBreakdown.sum)."""
    R = g.rules
    base: dict = {}
    for name, n in city.specialists.items():
        sp = R.specialists.get(name)
        if sp:
            for k, v in (sp.get("greatPersonPoints") or {}).items():
                base[k] = base.get(k, 0) + v * n
    for b in city.buildings:
        for k, v in (R.buildings[b].get("greatPersonPoints") or {}).items():
            base[k] = base.get(k, 0) + v
    if not base:
        return {}
    all_pct = city_gpp_bonus(g, city)
    specific: dict = {}
    from .uniques import Ctx
    for u in g.civ_uniques(city.owner, U.GreatPersonEarnedFaster, Ctx(g, city=city)):
        if u.p(0) in base:
            specific[u.p(0)] = specific.get(u.p(0), 0) + int(u.n(1))
    out = {}
    for k, v in base.items():
        fixed = v * 1000
        fixed += fixed * (all_pct + specific.get(k, 0)) // 100
        val = (fixed + 500) // 1000
        if k in R.units and val:
            out[k] = val
    return out


def pool_key(g: "Game", pid: int, gp: str) -> str:
    """The key a great person's point pool is stored under."""
    from .cities import equivalent_unit
    ud = g.rules.units.get(equivalent_unit(g, pid, gp)) or g.rules.units.get(gp)
    if ud:
        for u in ud["_umap"].get(U.GPPointPool):
            return u.p(0)
    return ""


def points_required(g: "Game", pid: int, gp: str) -> int:
    """Points needed for the next great person of a type, which rises each time."""
    p = g.player(pid)
    key = pool_key(g, pid, gp)
    base = p.flags.setdefault("gp_threshold", {}).get(key, 0) or 100
    p.flags["gp_threshold"][key] = base
    return int(base * g.speed["modifier"])


def great_people_types(g: "Game", pid: int) -> list[str]:
    """The kinds of great person this civilization can generate."""
    from .cities import equivalent_unit
    out = []
    for n in g.rules.great_person_units:
        if g.rules.units[n].get("uniqueTo"):
            continue
        e = equivalent_unit(g, pid, n)
        ud = g.rules.units[e]
        if not g.religion_enabled and ud["_umap"].has_tag(U.ReligiousUnit):
            continue
        if e not in out:
            out.append(e)
    return out


def _new_great_person(g, pid) -> Optional[str]:
    """The great person a civilization has just earned, if any."""
    p = g.player(pid)
    for unit, value in list(p.gg_points.items()):
        need = p.gg_threshold.get(unit) or 200
        p.gg_threshold[unit] = need
        if value >= need:
            p.gg_points[unit] -= need
            p.gg_threshold[unit] = need + 50
            return unit
    for gp, value in list(p.gp_points.items()):
        need = points_required(g, pid, gp)
        if value >= need:
            p.gp_points[gp] -= need
            key = pool_key(g, pid, gp)
            p.flags["gp_threshold"][key] = p.flags["gp_threshold"].get(key, 100) * 2
            return gp
    return None


def start_turn(g: "Game", pid: int):
    """Birth of great people (Civilization.startTurn)."""
    from . import units as unitmod
    p = g.player(pid)
    if p.kind != "major":
        return
    while True:
        gp = _new_great_person(g, pid)
        if gp is None:
            break
        cap = g.city(p.capital) if p.capital is not None else None
        if cap is None:
            break
        from .cities import equivalent_unit
        gp = equivalent_unit(g, pid, gp)
        u = unitmod.add_unit_in_city(g, cap, gp)
        if u is not None:
            p.great_people_earned += 1
            g.emit("great_person_born", f"A {gp} has been born in {cap.name}!", [pid], idx=cap.idx, unit=u.id)
            from . import triggers
            triggers.fire(g, pid, U.TriggerUponGainingUnit, filt=lambda x: True, unit=u)


def end_turn(g: "Game", pid: int):
    """Accumulate great-person points and produce anyone who has been earned."""
    p = g.player(pid)
    if p.kind != "major":
        return
    for c in g.player_cities(pid):
        for k, v in city_gpp(g, c).items():
            p.gp_points[k] = p.gp_points.get(k, 0) + v


def add_combat_points(g: "Game", pid: int, unit_type: str, xp: int):
    """Great General/Admiral points from combat experience (Battle.addXp)."""
    p = g.player(pid)
    if p.kind != "major":
        return
    R = g.rules
    from .uniques import Ctx
    for gp in R.great_person_units:
        ud = R.units[gp]
        for u in ud["_umap"].get(U.GreatPersonFromCombat):
            Ctx(g, civ=pid)
            ok = True
            for m in u.mods:
                if m.ph == "for [] units":
                    from .uniques import base_unit_matches
                    ok = ok and base_unit_matches(R, unit_type, m.p(0))
            if not ok:
                continue
            gain = xp
            for eu in g.civ_uniques(pid, U.GreatPersonEarnedFaster):
                if eu.p(0) == gp:
                    gain += xp * eu.n(1) / 100
            p.gg_points[gp] = p.gg_points.get(gp, 0) + int(gain)


def choose_free(g: "Game", pid: int, gp: str) -> dict:
    """Take a great person that has been granted for free."""
    from . import units as unitmod
    p = g.player(pid)
    if p.free_great_people <= 0:
        raise ActionError("You have no free Great Person to choose.")
    opts = great_people_types(g, pid)
    maya = p.flags.get("maya_limited", 0) > 0
    if maya:
        pool = p.flags.get("long_count_pool") or opts
        opts = [o for o in opts if o in pool]
    name = g.rules.resolve("unit", gp)
    from .cities import equivalent_unit
    if name:
        name = equivalent_unit(g, pid, name)
    if name not in opts:
        raise ActionError(f"Choose one of: {', '.join(opts)}.")
    cap = g.city(p.capital) if p.capital is not None else None
    if cap is None:
        raise ActionError("You need a capital to receive a Great Person.")
    u = unitmod.add_unit_in_city(g, cap, name)
    if u is None:
        raise ActionError("There is no room near the capital for the Great Person.")
    p.free_great_people -= 1
    if maya:
        p.flags["maya_limited"] -= 1
        pool = p.flags.get("long_count_pool") or opts
        p.flags["long_count_pool"] = [o for o in pool if o != name]
    return {"great_person": name, "unit_id": u.id, "at": g.xy(u.idx)}


def ai_choose_free(g: "Game", pid: int):
    """Choose a free great person for an AI civilization."""
    p = g.player(pid)
    while p.free_great_people > 0:
        opts = great_people_types(g, pid)
        pref = [o for o in ("Great Scientist", "Great Engineer", "Great Merchant", "Great Artist", "Great Prophet") if o in opts]
        try:
            choose_free(g, pid, (pref or opts)[0])
        except ActionError:
            break


def maya_long_count(g: "Game", pid: int):
    """Maya: a free great person at the end of every b'ak'tun once the required tech is known."""
    p = g.player(pid)
    for u in g.civ_uniques(pid, U.MayanGainGreatPerson):
        if not g.has_tech(pid, u.p(1)):
            continue
        turn = g.turn
        # the long count ticks every 394 years; approximate by turns via the speed's calendar
        year = g.year(turn)
        prev = g.year(turn - 1)
        if int((year + 3114) // 394) != int((prev + 3114) // 394):
            if not p.flags.get("long_count_pool"):
                p.flags["long_count_pool"] = great_people_types(g, pid)
            p.free_great_people += 1
            p.flags["maya_limited"] = p.flags.get("maya_limited", 0) + 1
            g.emit("great_person_available", "A new b'ak'tun has begun: a Great Person joins you!", [pid])


# ---------------------------------------------------------------------------------------------------------------
# Golden ages
# ---------------------------------------------------------------------------------------------------------------
def happiness_for_golden_age(g: "Game", pid: int) -> int:
    """Surplus happiness accumulated toward the next golden age."""
    p = g.player(pid)
    cost = 500 + p.golden_ages * 250
    cost *= 1 + len(g.player_cities(pid)) / 100
    cost *= g.speed["modifier"]
    return int(cost)


def golden_age_length(g: "Game", pid: int, turns: int) -> int:
    """How long a golden age lasts, after the modifiers that extend it."""
    t = float(turns)
    for u in g.civ_uniques(pid, U.GoldenAgeLength):
        t *= 1 + u.n(0) / 100
    t *= g.speed["goldenAgeLengthModifier"]
    return int(t)


def enter_golden_age(g: "Game", pid: int, turns: Optional[int] = None):
    """Begin a golden age."""
    p = g.player(pid)
    p.golden_age_turns += golden_age_length(g, pid, 10 if turns is None else turns)
    g.emit("golden_age", f"{p.name} has entered a Golden Age!", [pid], player=pid)
    from . import triggers
    triggers.fire(g, pid, U.TriggerUponEnteringGoldenAge)
    g.invalidate()


def golden_age_end_turn(g: "Game", pid: int, happiness: int):
    """Advance a golden age, or accumulate happiness toward the next one."""
    p = g.player(pid)
    if p.golden_age_turns <= 0:
        p.golden_age_points = max(0, p.golden_age_points + happiness)
    if p.golden_age_turns > 0:
        p.golden_age_turns -= 1
        if p.golden_age_turns == 0:
            g.emit("golden_age_end", f"{p.name}'s Golden Age has ended.", [pid])
            g.invalidate()
    elif p.golden_age_points >= happiness_for_golden_age(g, pid):
        p.golden_age_points -= happiness_for_golden_age(g, pid)
        enter_golden_age(g, pid)
        p.golden_ages += 1


# ---------------------------------------------------------------------------------------------------------------
# Great person actions
# ---------------------------------------------------------------------------------------------------------------
def hurry_research(g: "Game", unit) -> dict:
    """Spend a great scientist on research."""
    from . import research, units as unitmod
    pid = unit.owner
    if not g.rules.units[unit.type]["_umap"].get(U.CanHurryResearch):
        raise ActionError("This unit cannot hurry research.")
    if unit.moves <= 0:
        raise ActionError("The unit has no movement left.")
    cur = research.current(g, pid)
    if cur is None:
        raise ActionError("Choose a technology to research first.")
    if g.rules.techs[cur]["_umap"].has_tag(U.CannotBeHurried):
        raise ActionError(f"{cur} cannot be hurried.")
    sci = research.science_from_great_scientist(g, pid)
    research.add_science(g, pid, sci)
    unitmod.consume(g, unit)
    triggers_expend(g, unit)
    return {"science_added": sci, "researching": research.current(g, pid)}


def hurry_construction(g: "Game", unit) -> dict:
    """Spend a great engineer on a city's current construction."""
    from . import units as unitmod
    from .cities import current_construction, remaining_work, construct_if_enough
    if not g.rules.units[unit.type]["_umap"].get(U.CanSpeedupConstruction):
        raise ActionError("This unit cannot hurry construction.")
    city = g.city_at(unit.idx)
    if city is None or city.owner != unit.owner:
        raise ActionError("Move the Great Engineer into one of your cities.")
    if unit.moves <= 0:
        raise ActionError("The unit has no movement left.")
    cur = current_construction(city)
    if cur is None or cur not in g.rules.buildings:
        raise ActionError(f"{city.name} must be building a building or wonder to hurry it.")
    if g.rules.buildings[cur]["_umap"].has_tag(U.CannotBeHurried):
        raise ActionError(f"{cur} cannot be hurried.")
    add = int(min((300 + 30 * city.pop) * g.speed["productionCostModifier"], remaining_work(g, city, cur) - 1))
    if add <= 0:
        raise ActionError(f"{cur} is nearly complete already.")
    city.progress[cur] = city.progress.get(cur, 0) + add
    construct_if_enough(g, city)
    unitmod.consume(g, unit)
    triggers_expend(g, unit)
    return {"production_added": add, "city": city.name, "item": cur}


def trade_mission(g: "Game", unit) -> dict:
    """Spend a great merchant on a trade mission to a city-state."""
    from . import units as unitmod, city_states
    from .research import player_era
    ud = g.rules.units[unit.type]
    us = ud["_umap"].get(U.CanTradeWithCityStateForGoldAndInfluence)
    if not us:
        raise ActionError("This unit cannot conduct trade missions.")
    t = g.s.tiles[unit.idx]
    owner = t.owner
    if owner is None or g.player(owner).kind != "city_state" or owner == unit.owner or g.at_war(owner, unit.owner):
        raise ActionError("Move the Great Merchant into the territory of a city-state you are at peace with.")
    if unit.moves <= 0:
        raise ActionError("The unit has no movement left.")
    gold = (350 + 50 * player_era(g, unit.owner)) * g.speed["goldCostModifier"]
    from .units import unit_uniques
    for u in unit_uniques(g, unit, U.PercentGoldFromTradeMissions, with_civ=True):
        gold *= 1 + u.n(0) / 100
    gold = int(gold)
    g.player(unit.owner).gold += gold
    infl = us[0].n(0)
    city_states.add_influence(g, owner, unit.owner, infl)
    unitmod.consume(g, unit)
    triggers_expend(g, unit)
    g.emit("trade_mission", f"Your trade mission to {g.player(owner).name} earned {gold} gold and {int(infl)} influence.",
           [unit.owner])
    return {"gold": gold, "influence": infl, "city_state": g.player(owner).name}


def triggers_expend(g: "Game", unit):
    """Fire whatever a great person's expenditure triggers."""
    from . import triggers
    from .uniques import unit_matches
    triggers.fire(g, unit.owner, U.TriggerUponExpendingUnit, filt=lambda x: unit_matches(g, unit, x.p(0)),
                  include_unit=False, note=f"due to expending a {unit.type}")
