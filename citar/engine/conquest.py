"""Moving cities between civilizations: conquest (puppet by default), annexing, liberating, razing.
Port of UnCiv's CityConquestFunctions and Battle.conquerCity (MPL-2.0).

A captured city starts as a puppet of its conqueror (UnCiv shows the human a popup; CITAR lets the player decide
afterwards with annex_city / raze_city / liberate_city). Scripted bots decide immediately like UnCiv's AI.
"""
from __future__ import annotations

from typing import TYPE_CHECKING

from . import unique_types as U
from .game import ActionError
from .uniques import Ctx, STAT_KEY

if TYPE_CHECKING:
    from .game import Game
    from .state import City, Unit


def can_be_destroyed(g: "Game", city: "City", just_captured: bool = False) -> bool:
    """Whether a city may be razed. Original capitals and holy cities never can."""
    from . import religion
    if city.original_capital:
        return False
    if religion.is_holy_city(g, city):
        return False
    if g.player(city.owner).capital == city.id and not just_captured:
        return False
    return True


def _gold_for_capture(g, city, conqueror: int) -> int:
    """Gold plundered when a city falls, scaled by its size."""
    rng = g.state_rng("capture_gold", city.idx, g.turn)
    base = 20 + 10 * city.pop + rng.randrange(40)
    turn_mod = max(0, min(50, g.turn - city.turn_acquired)) / 50
    city_mod = 1.0
    from .cities import city_uniques
    for x in city_uniques(g, city, U.GoldFromCapturingCity):
        city_mod *= 1 + x.n(0) / 100
    civ_mod = 1.0
    for x in g.civ_uniques(conqueror, U.GoldFromEncampmentsAndCities):
        civ_mod *= 1 + x.n(0) / 100
    return int(base * turn_mod * city_mod * civ_mod)


def _destroy_buildings_on_capture(g, city):
    """Destroy the buildings a capture does not leave standing."""
    R = g.rules
    rng = g.state_rng("capture_buildings", city.idx, g.turn)
    for b in list(city.buildings):
        bd = R.buildings[b]
        if bd["_umap"].has_tag(U.NotDestroyedWhenCityCaptured) or bd.get("isWonder"):
            continue
        if bd["_umap"].has_tag(U.IndicatesCapital):
            continue
        if bd["_umap"].has_tag(U.DestroyedWhenCityCaptured) or rng.randrange(100) < 34:
            city.buildings.remove(b)


def move_to_civ(g: "Game", city: "City", new_owner: int):
    """CityConquestFunctions.moveToCiv."""
    from .cities import (add_building, capital_indicator, equivalent_building, try_add_free_buildings, city_tiles)
    from . import religion, espionage
    R = g.rules
    old = city.owner
    op, np_ = g.player(old), g.player(new_owner)
    was_capital = op.capital == city.id
    # the old palace goes; the old owner gets a new capital
    for b in list(city.buildings):
        if R.buildings[b]["_umap"].has_tag(U.IndicatesCapital):
            city.buildings.remove(b)
    city.owner = new_owner
    for i in city_tiles(g, city):
        g.s.tiles[i].owner = new_owner
    city.turn_acquired = g.turn
    city.previous_owner = old
    city.wltkd = 0
    city.demanded_resource = None
    free = set(op.free_buildings.pop(str(city.id), []))
    for b in list(city.buildings):
        if b in free:
            city.buildings.remove(b)
    for b in list(city.buildings):
        bd = R.buildings[b]
        if bd.get("isNationalWonder") and not bd["_umap"].has_tag(U.NotDestroyedWhenCityCaptured):
            city.buildings.remove(b)
            continue
        for x in bd["_umap"].get(U.MaxNumberBuildable):
            from .cities import contains_building
            n = sum(1 for c in g.player_cities(new_owner) if contains_building(g, c, b) or b in c.queue)
            if n >= x.n(0) and b in city.buildings:
                city.buildings.remove(b)
    espionage.city_removed(g, city)
    g.invalidate()
    if was_capital:
        op.capital = None
        rest = [c for c in g.player_cities(old) if c.id != city.id]
        if rest:
            newcap = max(rest, key=lambda c: c.pop)
            add_building(g, newcap, capital_indicator(g, old), try_free=False)
            op.capital = newcap.id
            g.emit("capital_moved", f"{op.name} moved its capital to {newcap.name}.", [old], idx=newcap.idx)
    if not [c for c in g.player_cities(new_owner) if c.id != city.id] or np_.capital is None or g.city(np_.capital) is None:
        add_building(g, city, capital_indicator(g, new_owner), try_free=False)
        np_.capital = city.id
    city.razing = False
    for b in list(city.buildings):
        eb = equivalent_building(g, new_owner, b)
        if eb != b:
            city.buildings.remove(b)
            if eb not in city.buildings:
                city.buildings.append(eb)
    if g.religion_enabled:
        religion.remove_unknown_pantheons(g, city)
    if g.civ_has(new_owner, U.MayNotAnnexCities) if hasattr(U, "MayNotAnnexCities") else False:
        city.puppet = True
        city.queue = []
    np_.founded_city = True
    try_add_free_buildings(g, new_owner)
    g.invalidate()


def _conquer_common(g, city, conqueror: int, receiver: int):
    """Everything that happens to a city when it changes hands, whatever is done with it afterwards.

    Population loss, destroyed buildings, resistance, and the units and tiles that come with it. The
    choice between annexing, puppeting and razing happens after this and only changes what follows.
    """
    from .cities import add_population, assign_citizens, max_health
    from . import victory, triggers
    old = city.owner
    gold = _gold_for_capture(g, city, conqueror)
    g.player(conqueror).gold += gold
    reconquered_in_resistance = city.previous_owner == receiver and city.resistance > 0
    _destroy_buildings_on_capture(g, city)
    move_to_civ(g, city, receiver)
    city.health = max_health(g, city) // 2
    city.avoid_growth = False
    city.focus = "balanced"
    if city.pop > 1:
        add_population(g, city, -1 - city.pop // 4)
    assign_citizens(g, city, reset=True)
    if not reconquered_in_resistance and city.founder != receiver:
        city.resistance = city.pop
    else:
        city.resistance = 0
    triggers.fire(g, old, U.TriggerUponLosingCity)
    victory.check_elimination(g, old, by=conqueror)
    return gold


def conquer(g: "Game", city: "City", unit: "Unit") -> dict:
    """A melee unit took a defeated city (Battle.conquerCity)."""
    from . import victory, triggers, diplomacy
    from .units import capture_civilian, unit_uniques
    attacker = unit.owner
    old = city.owner
    old_name = g.player(old).name
    for o in list(g.units_at(city.idx)):
        if o.owner == attacker:
            continue
        od = g.rules.units[o.type]
        if od["_domain"] == "Air" or od["_military"]:
            g.remove_unit(o)
        else:
            capture_civilian(g, unit, o)
    ctx = Ctx(g, civ=attacker, city=city, unit=unit, attacked_tile=city.idx)
    from .cities import city_stats
    stats = city_stats(g, city)["total"]
    for x in unit_uniques(g, unit, U.CaptureCityPlunder, with_civ=True, ctx=ctx):
        k = STAT_KEY.get(x.p(2))
        if k:
            g.add_stat(attacker, k, int(x.n(0) * stats.get(STAT_KEY.get(x.p(1), "culture"), 0)))
    g.place_unit(unit, city.idx)
    diplomacy.on_city_captured(g, attacker, old, city)
    result = {"captured_city": city.name, "from": old_name}
    if city.original_capital and city.founder == attacker:
        gold = _conquer_common(g, city, attacker, attacker)
        city.puppet = False
        result["result"] = "recaptured"
    else:
        gold = _conquer_common(g, city, attacker, attacker)
        city.puppet = True
        city.queue = []
        result["result"] = "puppet"
        if g.player(attacker).controller == "bot":
            _auto_conquer(g, attacker, city)
            result["result"] = "razing" if city.razing else ("annexed" if not city.puppet else "puppet")
    result["gold_plundered"] = gold
    g.emit("city_captured", f"{g.player(attacker).name} captured {city.name} from {old_name}! ({gold} gold plundered)",
           None, idx=city.idx, city=city.id, old_owner=old, new_owner=attacker)
    triggers.fire(g, attacker, U.TriggerUponConqueringCity, city=city, unit=unit)
    victory.check_domination(g)
    return result


def _auto_conquer(g, pid, city):
    """Battle.automateCityConquer (without the liberation branch for simplicity of bots)."""
    founder = city.founder
    if g.player(founder).kind == "city_state" and founder != pid and not g.at_war(pid, founder) and g.player(founder).alive is False:
        liberate(g, pid, city)
        return
    if (city.pop < 4 or g.player(pid).kind == "city_state") and city.founder != pid and can_be_destroyed(g, city, True):
        city.puppet = False
        city.razing = True


def annex(g: "Game", pid: int, city: "City") -> dict:
    """Take full control of a conquered city, with the unhappiness that brings."""
    if city.owner != pid:
        raise ActionError("That is not your city.")
    if not city.puppet:
        raise ActionError(f"{city.name} is not a puppet.")
    city.puppet = False
    city.avoid_growth = False
    city.focus = "balanced"
    g.invalidate()
    return {"annexed": city.name}


def puppet(g: "Game", pid: int, city: "City") -> dict:
    """Keep a conquered city as a puppet: less unhappiness, no control over what it builds."""
    if city.owner != pid:
        raise ActionError("That is not your city.")
    if city.founder == pid:
        raise ActionError("You cannot make a puppet of a city you founded.")
    city.puppet = True
    city.queue = []
    g.invalidate()
    return {"puppet": city.name}


def raze(g: "Game", pid: int, city: "City", stop: bool = False) -> dict:
    """Burn a conquered city down, one population a turn, or stop doing so."""
    if city.owner != pid:
        raise ActionError("That is not your city.")
    if stop:
        city.razing = False
        return {"stopped_razing": city.name}
    if not can_be_destroyed(g, city, just_captured=True):
        raise ActionError(f"{city.name} cannot be razed (original capitals and holy cities cannot be razed).")
    if city.founder == pid:
        raise ActionError("You can only raze cities you captured.")
    city.razing = True
    city.puppet = False
    return {"razing": city.name, "turns": city.pop}


def liberate(g: "Game", pid: int, city: "City") -> dict:
    """Return a captured city to its founder (CityConquestFunctions.liberateCity)."""
    from .cities import add_building, capital_indicator
    from . import diplomacy, city_states
    if city.owner != pid:
        raise ActionError("That is not your city.")
    founder = city.founder
    if founder == pid or city.previous_owner is None:
        raise ActionError("Only cities captured from another civilization can be liberated.")
    if g.player(founder).kind == "barbarian":
        raise ActionError("That city cannot be liberated.")
    fp = g.player(founder)
    resurrect = not fp.alive
    if resurrect:
        fp.alive = True
        fp.eliminated_turn = None
    _conquer_common(g, city, pid, founder)
    city.puppet = False
    if len(g.player_cities(founder)) == 1:
        add_building(g, city, capital_indicator(g, founder), try_free=False)
        fp.capital = city.id
    if fp.kind == "major":
        rel = g.relation(pid, founder)
        if rel and rel.get("war"):
            diplomacy.make_peace(g, pid, founder)
        diplomacy.add_opinion(g, founder, pid, "liberated_city", 10 + 10 * city.pop)
        g.s.open_borders[f"{founder}>{pid}"] = g.turn + g.speed["dealDuration"]
    else:
        others = [city_states.influence(g, founder, q.id) for q in g.majors() if q.id != pid]
        new = max(max(others, default=-60), city_states.influence(g, founder, pid)) + 105
        city_states.set_influence(g, founder, pid, max(60, new))
        if g.at_war(pid, founder):
            diplomacy.make_peace(g, pid, founder)
    for o in list(g.units_at(city.idx)):
        from .movement import teleport_to_closest
        if o.owner != founder:
            teleport_to_closest(g, o)
    g.emit("liberated", f"{g.player(pid).name} liberated {city.name}, returning it to {fp.name}!", None, idx=city.idx)
    return {"liberated": city.name, "returned_to": fp.name}
