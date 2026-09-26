"""One-time unique effects ("Free [Great Prophet] appears", "Free Social Policy", ...) and trigger conditions
("<upon discovering [Writing] technology>"). Port of UnCiv's UniqueTriggerActivation (MPL-2.0)."""
from __future__ import annotations

from typing import Callable, Optional, TYPE_CHECKING

from . import unique_types as U
from .uniques import Ctx, Unique, applies, city_matches, unit_matches, tile_matches, era_matches, STAT_KEY

if TYPE_CHECKING:
    from .game import Game

TRIGGERABLE = {
    U.OneTimeFreeUnit, U.OneTimeAmountFreeUnits, U.OneTimeFreeUnitRuins, U.OneTimeFreePolicy,
    U.OneTimeAmountFreePolicies, U.OneTimeEnterGoldenAge, U.OneTimeEnterGoldenAgeTurns, U.OneTimeFreeGreatPerson,
    U.OneTimeGainPopulation, U.OneTimeGainPopulationRandomCity, U.OneTimeDiscoverTech, U.OneTimeAdoptPolicyOrBelief,
    U.OneTimeFreeTech, U.OneTimeAmountFreeTechs, U.OneTimeFreeTechRuins, U.OneTimeRevealEntireMap,
    U.OneTimeFreeBelief, U.OneTimeTriggerVoting, U.OneTimeGainStat, U.OneTimeGainStatRange, U.OneTimeGainPantheon,
    U.OneTimeGainProphet, U.OneTimeGainTechPercent, U.OneTimeTakeOverTilesInRadius, U.OneTimeTakeOverTilesInCity,
    U.OneTimeRevealSpecificMapTiles, U.OneTimeRevealCrudeMap, U.OneTimeGlobalSpiesWhenEnteringEra,
    U.OneTimeSpiesLevelUp, U.OneTimeGainSpy, U.UnitsGainPromotion, U.FreeStatBuildings, U.FreeSpecificBuildings,
    U.GainFreeBuildings, U.CityStateCanGiftGreatPeople, U.OneTimeUnitHeal, U.OneTimeUnitDamage, U.OneTimeUnitGainXP,
    U.OneTimeUnitUpgrade, U.OneTimeUnitSpecialUpgrade, U.OneTimeUnitGainPromotion, U.OneTimeUnitGainMovement,
    U.OneTimeUnitLoseMovement, U.OneTimeUnitDestroyed,
}


def is_triggerable(u: Unique) -> bool:
    """Whether a unique fires as a one-off effect rather than applying continuously."""
    return u.ph in TRIGGERABLE or u.timed


def has_trigger_conditional(u: Unique) -> bool:
    """Whether a unique carries a condition that says when it fires."""
    return any(m.ph.startswith("upon ") for m in u.mods)


def fire(g: "Game", pid: int, trigger_ph: str, filt: Optional[Callable[[Unique], bool]] = None, city=None, unit=None,
         tile: Optional[int] = None, note: Optional[str] = None, include_unit: bool = True):
    """Fire every civ (and unit) unique carrying the trigger condition `trigger_ph` (forEachTriggeredUnique)."""
    ctx = Ctx(g, civ=pid, city=city, unit=unit, tile=tile)
    found = []
    for m in g.civ_umaps(pid):
        for u in m.all:
            trig = next((x for x in u.mods if x.ph == trigger_ph), None)
            if trig is None or (filt and not filt(trig)) or not applies(u, ctx):
                continue
            found.append(u)
    if city is not None:
        from .cities import local_umaps
        for m in local_umaps(g, city):
            for u in m.all:
                trig = next((x for x in u.mods if x.ph == trigger_ph), None)
                if trig is None or (filt and not filt(trig)) or not applies(u, ctx):
                    continue
                found.append(u)
    if unit is not None and include_unit:
        from .units import unit_umap
        for u in unit_umap(g, unit).all:
            trig = next((x for x in u.mods if x.ph == trigger_ph), None)
            if trig is None or (filt and not filt(trig)) or not applies(u, ctx):
                continue
            found.append(u)
    for u in found:
        trigger(g, u, pid, city=city, unit=unit, tile=tile, note=note)


def _cities_for(g, pid, city, f):
    """The cities a triggered effect applies to."""
    if f == "in this city":
        return [city] if city is not None else []
    return [c for c in g.player_cities(pid) if city_matches(g, c, f)]


def trigger(g: "Game", u: Unique, pid: int, city=None, unit=None, tile: Optional[int] = None,
            note: Optional[str] = None) -> bool:
    """Apply a one-time effect. Returns True if something happened."""
    R = g.rules
    p = g.player(pid)
    if tile is None:
        tile = city.idx if city is not None else (unit.idx if unit is not None else None)
    if city is None and tile is not None:
        c = g.city(g.s.tiles[tile].city)
        if c is not None and c.owner == pid:
            city = c
    ph = u.ph
    suffix = f" ({note})" if note else ""
    if u.timed:
        turns = int(u.mod("for [] turns").n(0))
        text = u.text
        p.temp_uniques.append({"text": text, "turns": turns})
        g.invalidate()
        return True
    rng = g.state_rng("trigger", u.text, pid, tile, g.turn)
    chosen_city = city or (g.city(p.capital) if p.capital is not None else None)
    from . import units as unitmod
    from .cities import equivalent_unit, equivalent_building, add_population, complete_construction

    if ph == U.OneTimeFreeUnit or ph == U.OneTimeAmountFreeUnits or ph == U.OneTimeFreeUnitRuins:
        name = u.p(0) if ph != U.OneTimeAmountFreeUnits else u.p(1)
        if name not in R.units:
            return False
        name = equivalent_unit(g, pid, name)
        count = int(u.n(0)) if ph == U.OneTimeAmountFreeUnits else 1
        if p.kind == "city_state" and R.units[name]["_umap"].get(U.FoundCity):
            return False
        limits = [int(x.n(0)) for x in R.units[name]["_umap"].get(U.MaxNumberBuildable)]
        have = sum(1 for x in g.player_units(pid) if x.type == name)
        if limits:
            count = min(count, min(limits) - have)
        placed = 0
        for _ in range(max(0, count)):
            if ph == U.OneTimeFreeUnitRuins:
                at = tile if tile is not None else (chosen_city.idx if chosen_city else None)
                nu = unitmod.place_unit_near(g, pid, name, at) if at is not None else None
            elif city is not None or (tile is None and chosen_city is not None):
                nu = unitmod.add_unit_in_city(g, chosen_city, name)
            elif tile is not None:
                nu = unitmod.place_unit_near(g, pid, name, tile)
            elif g.player_units(pid):
                nu = unitmod.place_unit_near(g, pid, name, g.player_units(pid)[0].idx)
            else:
                nu = None
            if nu is not None:
                placed += 1
        if placed:
            g.emit("free_unit", f"{p.name} gained {placed} {name}{'s' if placed > 1 else ''}{suffix}.", [pid],
                   idx=tile)
        return placed > 0
    if ph == U.OneTimeFreePolicy:
        p.free_policies += 1
        g.emit("policy_available", f"{p.name} may choose a free social policy{suffix}.", [pid])
        return True
    if ph == U.OneTimeAmountFreePolicies:
        p.free_policies += int(u.n(0))
        g.emit("policy_available", f"{p.name} may choose {int(u.n(0))} free social policies{suffix}.", [pid])
        return True
    if ph == U.OneTimeAdoptPolicyOrBelief:
        from . import policies
        if u.p(0) in R.policies and u.p(0) not in p.policies:
            p.free_policies += 1
            policies.adopt(g, pid, u.p(0), free=True)
            return True
        return False
    if ph in (U.OneTimeEnterGoldenAge, U.OneTimeEnterGoldenAgeTurns):
        from . import great_people
        great_people.enter_golden_age(g, pid, int(u.n(0)) if ph == U.OneTimeEnterGoldenAgeTurns else None)
        return True
    if ph == U.OneTimeFreeGreatPerson:
        p.free_great_people += 1
        g.emit("great_person_available", f"{p.name} may choose a free Great Person{suffix}.", [pid])
        if p.auto.get("free_picks"):
            from . import great_people
            great_people.ai_choose_free(g, pid)
        return True
    if ph == U.OneTimeGainPopulation:
        cs = _cities_for(g, pid, city, u.p(1))
        for c in cs:
            add_population(g, c, int(u.n(0)))
        return bool(cs)
    if ph == U.OneTimeGainPopulationRandomCity:
        cs = g.player_cities(pid)
        if not cs:
            return False
        c = rng.choice(cs)
        add_population(g, c, int(u.n(0)))
        g.emit("ruins", f"Survivors joined {c.name} (+{int(u.n(0))} population){suffix}.", [pid], idx=c.idx)
        return True
    if ph in (U.OneTimeFreeTech, U.OneTimeAmountFreeTechs):
        n = 1 if ph == U.OneTimeFreeTech else int(u.n(0))
        p.free_techs += n
        g.emit("free_tech", f"{p.name} may choose {n} free technolog{'y' if n == 1 else 'ies'}{suffix}.", [pid])
        if p.auto.get("free_picks"):
            from . import research
            for _ in range(n):
                av = research.available_techs(g, pid)
                if av:
                    research.free_tech(g, pid, min(av, key=lambda t: research.tech_cost(g, pid, t)))
        return True
    if ph == U.OneTimeDiscoverTech:
        from . import research
        if g.has_tech(pid, u.p(0)):
            return False
        research.add_tech(g, pid, u.p(0), source="free")
        return True
    if ph == U.OneTimeFreeTechRuins:
        from . import research
        cands = [t for t in R.techs if era_matches(R, R.techs[t]["era"], u.p(1)) and research.can_research(g, pid, t)]
        if not cands:
            return False
        rng.shuffle(cands)
        for t in cands[: int(u.n(0))]:
            research.add_tech(g, pid, t, source="ruins")
        return True
    if ph == U.OneTimeRevealEntireMap:
        from . import visibility
        visibility.reveal_tiles(g, pid, range(g.grid.size))
        return True
    if ph == U.OneTimeGainStat:
        stat = STAT_KEY.get(u.p(1))
        if stat not in ("gold", "culture", "faith", "science", "happiness"):
            return False
        amt = u.n(0)
        if u.has_mod("(modified by game speed)"):
            amt = round(amt * _stat_speed(g, u.p(1)))
        g.add_stat(pid, stat, amt)
        g.emit("gain", f"{p.name} gained {int(amt)} {u.p(1)}{suffix}.", [pid], idx=tile)
        return True
    if ph == U.OneTimeGainStatRange:
        stat = STAT_KEY.get(u.p(2))
        lo, hi = sorted((int(u.n(0)), int(u.n(1))))
        amt = rng.randint(lo, hi)
        if u.has_mod("(modified by game speed)"):
            amt = round(amt * _stat_speed(g, u.p(2)))
        g.add_stat(pid, stat, amt)
        g.emit("gain", f"{p.name} gained {int(amt)} {u.p(2)}{suffix}.", [pid], idx=tile)
        return True
    if ph == U.OneTimeGainPantheon:
        from . import religion
        if p.religion_state != "none":
            return False
        amt = religion.faith_for_pantheon(g, pid, 2)
        if amt <= 0:
            return False
        p.faith += amt
        g.emit("faith", f"{p.name} gained {amt} faith{suffix}.", [pid], idx=tile)
        return True
    if ph == U.OneTimeGainProphet:
        from . import religion
        if religion.prophet_unit(g, pid) is None:
            return False
        amt = int(religion.faith_for_next_prophet(g, pid) * u.n(0) / 100)
        if amt <= 0:
            return False
        p.faith += amt
        g.emit("faith", f"{p.name} gained {amt} faith{suffix}.", [pid], idx=tile)
        return True
    if ph == U.OneTimeRevealSpecificMapTiles:
        if tile is None:
            return False
        from . import visibility
        cands = [i for i in g.grid.within(tile, int(u.n(2))) if not p.explored[i] and tile_matches(g, i, u.p(1))]
        if not cands:
            return False
        if u.p(0) not in ("All", "all"):
            rng.shuffle(cands)
            cands = cands[: int(u.n(0))]
        visibility.reveal_tiles(g, pid, cands)
        g.emit("ruins", f"{p.name} revealed {len(cands)} tiles{suffix}.", [pid], idx=tile)
        return True
    if ph == U.OneTimeRevealCrudeMap:
        if tile is None:
            return False
        from . import visibility
        ring = [i for i in g.grid.ring(tile, int(u.n(0))) if not p.explored[i]]
        if not ring:
            return False
        center = rng.choice(ring)
        chance = u.n(2) / 100
        visibility.reveal_tiles(g, pid, [i for i in g.grid.within(center, int(u.n(1))) if rng.random() < chance])
        return True
    if ph == U.OneTimeTriggerVoting:
        from . import victory
        victory.schedule_vote(g, pid)
        return True
    if ph in (U.OneTimeGlobalSpiesWhenEnteringEra, U.OneTimeSpiesLevelUp, U.OneTimeGainSpy):
        from . import espionage
        return espionage.on_trigger(g, u, pid)
    if ph == U.OneTimeTakeOverTilesInRadius:
        if tile is None or not g.player_cities(pid):
            return False
        from .cities import take_ownership
        tiles = [i for i in g.grid.within(tile, int(u.n(1))) if g.city_at(i) is None and tile_matches(g, i, u.p(0))
                 and g.s.tiles[i].owner != pid]
        if not tiles:
            return False
        near = [g.city(g.s.tiles[n].city) for i in tiles for n in [i] + g.grid.neighbors(i)
                if g.s.tiles[n].owner == pid and g.s.tiles[n].city is not None]
        near = [c for c in near if c is not None]
        pool = near or g.player_cities(pid)
        target = min(pool, key=lambda c: g.grid.distance(c.idx, tile) + (5 if c.razing else 0))
        for i in tiles:
            other = g.s.tiles[i].owner
            if other is not None and g.player(other).kind == "city_state":
                from . import city_states
                city_states.add_influence(g, other, pid, -15)
            take_ownership(g, target, i)
        return True
    if ph == U.OneTimeTakeOverTilesInCity:
        from .cities import expand_borders
        for c in _cities_for(g, pid, city, u.p(1)):
            for _ in range(int(u.n(0))):
                if expand_borders(g, c) is None:
                    break
        return True
    if ph == U.GainFreeBuildings:
        b = equivalent_building(g, pid, u.p(0))
        cs = _cities_for(g, pid, city, u.p(1))
        from .cities import _add_free, contains_building
        for c in cs:
            _add_free(g, pid, c, b)
            if not contains_building(g, c, b):
                complete_construction(g, c, b)
        return bool(cs)
    if ph == U.FreeStatBuildings:
        from .cities import add_free_stat_buildings
        add_free_stat_buildings(g, pid, u.p(0), int(u.n(1)))
        return True
    if ph == U.FreeSpecificBuildings:
        from .cities import add_free_specific_buildings
        add_free_specific_buildings(g, pid, u.p(0), int(u.n(1)))
        return True
    if ph == U.UnitsGainPromotion:
        pr = u.p(1)
        if pr not in R.promotions:
            return False
        types = R.promotions[pr]["unitTypes"]
        done = 0
        for x in g.player_units(pid):
            if unit_matches(g, x, u.p(0)) and (not types or R.units[x.type]["unitType"] in types) and pr not in x.promotions:
                unitmod.add_promotion(g, x, pr, free=True)
                done += 1
        return done > 0
    if ph == U.CityStateCanGiftGreatPeople:
        from . import city_states
        p.flags["cs_gp_gift"] = city_states.turns_for_gp_gift(g, pid) // 2
        return True
    if ph == U.OneTimeFreeBelief:
        from . import religion
        return religion.grant_free_belief(g, pid, u.p(0))
    if ph == U.OneTimeGainTechPercent:
        from . import research
        t = u.p(1)
        if t not in R.techs or g.has_tech(pid, t):
            return False
        p.research_progress[t] = p.research_progress.get(t, 0) + round(research.tech_cost(g, pid, t) * u.n(0) / 100)
        return True
    # unit-targeted effects
    if unit is None:
        return False
    if ph == U.OneTimeUnitHeal:
        if unit.hp >= 100:
            return False
        unit.hp = min(100, unit.hp + int(u.n(1)))
        return True
    if ph == U.OneTimeUnitDamage:
        unitmod.take_damage(g, unit, int(u.n(1)))
        return True
    if ph == U.OneTimeUnitGainXP:
        unit.xp += int(u.n(1))
        g.emit("ruins", f"{p.name}'s {unit.type} gained {int(u.n(1))} XP{suffix}.", [pid], idx=unit.idx)
        return True
    if ph in (U.OneTimeUnitUpgrade, U.OneTimeUnitSpecialUpgrade):
        return unitmod.free_upgrade(g, unit, special=ph == U.OneTimeUnitSpecialUpgrade)
    if ph == U.OneTimeUnitGainPromotion:
        if u.p(1) in R.promotions:
            unitmod.add_promotion(g, unit, u.p(1), free=True)
            return True
        return False
    if ph in (U.OneTimeUnitGainMovement, U.OneTimeUnitLoseMovement):
        delta = int(u.n(1) * g.rules.move_scale)
        unit.moves = max(0, unit.moves + (delta if ph == U.OneTimeUnitGainMovement else -delta))
        return True
    if ph == U.OneTimeUnitDestroyed:
        g.remove_unit(unit)
        return True
    return False


def _stat_speed(g, stat_name: str) -> float:
    """Scale a triggered stat gain by game speed."""
    sp = g.speed
    return {"Gold": sp["goldCostModifier"], "Culture": sp["cultureCostModifier"], "Faith": sp["faithCostModifier"],
            "Science": sp["scienceCostModifier"], "Production": sp["productionCostModifier"]}.get(stat_name, sp["modifier"])
