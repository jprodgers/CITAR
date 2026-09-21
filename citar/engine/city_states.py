"""City-states: influence & allies, friend/ally bonuses, gifts, tribute, protection, quests, and the minor-civ AI.
Port of UnCiv's CityStateFunctions, DiplomacyManager (city-state parts), DiplomacyTurnManager and QuestManager
(MPL-2.0)."""
from __future__ import annotations

from typing import Optional, TYPE_CHECKING

from . import unique_types as U
from .game import ActionError
from .uniques import Ctx, UniqueMap, applies, unit_matches

if TYPE_CHECKING:
    from .game import Game

MIN_INFLUENCE = -60.0
FRIEND, ALLY = 30.0, 60.0
PERSONALITIES = ["Friendly", "Neutral", "Hostile", "Irrational"]


def _cs(g, cs: int):
    """The city-state player with this id."""
    return g.player(cs)


def _pair(g, cs: int, major: int) -> dict:
    """The stored relationship between one city-state and one major civilization.

    Created on first access, because most pairs never interact and storing them all would put a
    city-state's relationship with every civilization it has never met into every save.
    """
    return g.player(cs).flags.setdefault("pairs", {}).setdefault(str(major), {})


# ---------------------------------------------------------------------------------------------------------------
# Setup
# ---------------------------------------------------------------------------------------------------------------
def init_city_state(g: "Game", pid: int, used_majors: set):
    """Set up a city-state at the start of a game: its type, personality and first quest."""
    R = g.rules
    p = g.player(pid)
    rng = g.state_rng("cs_init", pid)
    ct = R.city_state_types[p.cs_type]
    types = {u.ph for u in ct["_friend"].all} | {u.ph for u in ct["_ally"].all}
    nd = R.nations[p.nation]
    p.cs_personality = nd.get("personality") if nd.get("personality") in PERSONALITIES else rng.choice(PERSONALITIES)
    if U.CityStateUniqueLuxury in types:
        merc = [r for r, d in R.resources.items() if d["_umap"].has_tag(U.CityStateOnlyResource)]
        p.cs_resource = rng.choice(merc) if merc else None
    if U.CityStateMilitaryUnits in types:
        era_n = R.eras[g.s.config.get("starting_era", "Ancient era")]["number"]
        cands = []
        for n, u in R.units.items():
            nat = u.get("uniqueTo")
            if not nat or R.nations.get(nat, {}).get("kind") != "major" or nat in used_majors:
                continue
            if u["_domain"] != "Land" or not u["_military"]:
                continue
            if u["_era"] < era_n:
                continue
            cands.append(n)
        p.cs_unique_unit = rng.choice(sorted(cands)) if cands else None


# ---------------------------------------------------------------------------------------------------------------
# Influence & relationships
# ---------------------------------------------------------------------------------------------------------------
def influence(g: "Game", cs: int, major: int) -> float:
    """A major civilization's influence with a city-state, floored at the minimum.

    War returns the floor regardless of what was stored, so influence built up before a war does not
    quietly persist through it.
    """
    if g.at_war(cs, major):
        return MIN_INFLUENCE
    return g.player(cs).influence.get(str(major), 0.0)


def raw_influence(g, cs, major) -> float:
    """The stored influence, ignoring war - the number that decays toward the resting point."""
    return g.player(cs).influence.get(str(major), 0.0)


def set_influence(g: "Game", cs: int, major: int, amount: float):
    """Set influence and recompute who the city-state's ally is."""
    p = g.player(cs)
    p.influence[str(major)] = max(float(amount), MIN_INFLUENCE)
    update_ally(g, cs)
    g.invalidate()


def add_influence(g: "Game", cs: int, major: int, amount: float):
    """Change influence by an amount."""
    set_influence(g, cs, major, raw_influence(g, cs, major) + amount)


def relationship(g: "Game", cs: int, major: int) -> str:
    """The named relationship - Ally, Friend, Neutral, Hostile - at the current influence."""
    inf = influence(g, cs, major)
    p = g.player(cs)
    if inf <= -30:
        return "Unforgivable"
    if inf < 0:
        return "Enemy"
    if inf >= ALLY and p.ally == major:
        return "Ally"
    if inf >= FRIEND:
        return "Friend"
    if tribute_willingness(g, cs, major) > 0:
        return "Afraid"
    return "Neutral"


def is_friend_level(g: "Game", cs: int, major: int) -> bool:
    """Whether influence is at least the friendship threshold."""
    return g.player(cs).kind == "city_state" and influence(g, cs, major) >= FRIEND


def update_ally(g: "Game", cs: int):
    """Recompute which civilization is this city-state's ally, and announce a change.

    Only the highest influence above the threshold counts, so an alliance is always a contest rather
    than a shared benefit - which is what makes buying influence a race rather than an investment.
    """
    p = g.player(cs)
    if p.kind != "city_state":
        return
    old = p.ally
    best, best_v = None, None
    for q in g.majors():
        if not g.has_met(cs, q.id):
            continue
        v = influence(g, cs, q.id)
        if best_v is None or v > best_v:
            best, best_v = q.id, v
    new = best if best_v is not None and best_v >= ALLY else None
    if new == old:
        return
    p.ally = new
    g.invalidate()
    if new is not None:
        g.emit("cs_ally", f"{g.player(new).name} is now allied with {p.name}.", [new], player=cs)
        for u in g.civ_uniques(new, U.CityStateCanBeBoughtForGold):
            _pair(g, cs, new)["marriage_cooldown"] = int(u.n(0))
        from . import diplomacy
        for enemy in g.s.players:
            if enemy.alive and enemy.id not in (cs, new) and g.at_war(enemy.id, new) and not g.is_barbarian(enemy.id) \
                    and not g.at_war(cs, enemy.id):
                if not g.has_met(cs, enemy.id):
                    g.meet(cs, enemy.id)
                diplomacy.set_war(g, cs, enemy.id, reason="city-state alliance")
    if old is not None and g.player(cs).alive:
        g.emit("cs_ally_lost", f"{g.player(old).name} lost its alliance with {p.name}.", [old], player=cs)


def allied_city_states(g: "Game", major: int) -> list[int]:
    """Every city-state allied to this civilization."""
    return [q.id for q in g.s.players if q.kind == "city_state" and q.alive and q.ally == major]


def bonus_umaps(g: "Game", major: int) -> list[UniqueMap]:
    """The unique maps granting this civilization bonuses from its city-states."""
    out = []
    for q in g.s.players:
        if q.kind != "city_state" or not q.alive or not g.has_met(major, q.id):
            continue
        ct = g.rules.city_state_types.get(q.cs_type)
        if not ct:
            continue
        if q.ally == major:
            out.append(ct["_ally"])
        elif influence(g, q.id, major) >= FRIEND:
            out.append(ct["_friend"])
    return out


def resources_for_ally(g: "Game", cs: int) -> list[tuple[str, str, int]]:
    """The resources an allied city-state shares with its ally."""
    from .economy import city_resources
    agg: dict = {}
    for c in g.player_cities(cs):
        for r, _, a in city_resources(g, c):
            if a > 0:
                agg[r] = agg.get(r, 0) + a
    return [(r, "City-States", a) for r, a in agg.items()]


def resting_point(g: "Game", cs: int, major: int) -> float:
    """The influence this relationship decays toward, raised by policies and buildings.

    The number that decides whether influence is a purchase or a subscription: above the resting point
    it decays, below it recovers, so maintaining an alliance costs something every few turns unless the
    resting point has been raised.
    """
    rp = 0.0
    for u in g.civ_uniques(major, U.CityStateRestingPoint):
        rp += u.n(0)
    from . import religion
    cap = g.city(g.player(cs).capital) if g.player(cs).capital is not None else None
    if cap is not None:
        for u in g.civ_uniques(major, U.RestingPointOfCityStatesFollowingReligionChange):
            if g.player(major).religion and g.player(major).religion == religion.majority_religion(g, cap):
                rp += u.n(0)
    if major in g.player(cs).protectors:
        rp += 10
    if _pair(g, cs, major).get("wary"):
        rp -= 20
    return rp


def _degrade(g, cs, major) -> float:
    """How much influence is lost this turn, when it is above the resting point."""
    if influence(g, cs, major) <= resting_point(g, cs, major):
        return 0.0
    p = g.player(cs)
    dec = 1.5 if p.cs_personality == "Hostile" else (2.0 if is_aggressor(g, major) else 1.0)
    pct = sum(u.n(0) for u in g.civ_uniques(major, U.CityStateInfluenceDegradation))
    from . import religion
    cap = g.city(p.capital) if p.capital is not None else None
    if cap is not None and g.player(major).religion and religion.majority_religion(g, cap) == g.player(major).religion:
        pct -= 25
    for q in g.majors():
        if q.id != major:
            pct += sum(u.n(0) for u in g.civ_uniques(q.id, U.OtherCivsCityStateRelationsDegradeFaster))
    return max(0.0, dec) * (1 + max(-100.0, pct) / 100)


def _recovery(g, cs, major) -> float:
    """How much influence is regained this turn, when it is below the resting point."""
    if influence(g, cs, major) >= resting_point(g, cs, major):
        return 0.0
    pct = 100.0 if g.civ_has(major, U.CityStateInfluenceRecoversTwiceNormalRate) else 0.0
    from . import religion
    p = g.player(cs)
    cap = g.city(p.capital) if p.capital is not None else None
    if cap is not None and g.player(major).religion and religion.majority_religion(g, cap) == g.player(major).religion:
        pct += 50
    return 1.0 * (1 + max(0.0, pct) / 100)


def is_aggressor(g: "Game", major: int) -> bool:
    """Whether this civilization has attacked a city-state at all."""
    return g.player(major).flags.get("cs_attacks", 0) >= 1


def is_warmonger(g: "Game", major: int) -> bool:
    """Whether it has done so often enough for every city-state to hold it against them."""
    return g.player(major).flags.get("cs_attacks", 0) >= 3


# ---------------------------------------------------------------------------------------------------------------
# Turn processing
# ---------------------------------------------------------------------------------------------------------------
def end_turn(g: "Game", cs: int):
    """City-state end of turn: influence drift, unit gifts, flags, quests, elections, free techs."""
    p = g.player(cs)
    if not p.alive:
        return
    for q in g.majors():
        if not g.has_met(cs, q.id):
            continue
        pair = _pair(g, cs, q.id)
        before = relationship(g, cs, q.id)
        rp = resting_point(g, cs, q.id)
        cur = raw_influence(g, cs, q.id)
        if cur > rp:
            set_influence(g, cs, q.id, max(rp, cur - _degrade(g, cs, q.id)))
        elif cur < rp:
            set_influence(g, cs, q.id, min(rp, cur + _recovery(g, cs, q.id)))
        after = relationship(g, cs, q.id)
        if before in ("Friend", "Ally") and after not in ("Friend", "Ally"):
            g.emit("cs_relationship", f"Your relationship with {p.name} degraded.", [q.id], player=cs)
        for k in ("bullied", "pledged", "withdrew", "border_conflict", "anger_free", "recently_attacked",
                  "marriage_cooldown", "notified_afraid"):
            if pair.get(k, 0) > 0:
                pair[k] -= 1
                if pair[k] == 0:
                    del pair[k]
        _military_unit_gift(g, cs, q.id, pair)
    _update_border_intrusion(g, cs)
    _free_techs(g, cs)
    _quests_end_turn(g, cs)
    wars = p.flags.get("war_quests", {})
    for k in list(wars):
        if not g.at_war(cs, int(k)) or not g.player(int(k)).alive:
            del wars[k]
    if p.flags.get("recently_bullied", 0) > 0:
        p.flags["recently_bullied"] -= 1
    if g.espionage_enabled:
        from . import espionage
        espionage.city_state_election_tick(g, cs)


def _military_unit_gift(g, cs, major, pair):
    """Decide whether a city-state gives its friend or ally a military unit this turn."""
    p = g.player(cs)
    lvl = relationship(g, cs, major)
    if lvl not in ("Friend", "Ally"):
        pair.pop("unit_timer", None)
        return
    ct = g.rules.city_state_types[p.cs_type]
    m = ct["_ally"] if lvl == "Ally" else ct["_friend"]
    us = [u for u in m.get(U.CityStateMilitaryUnits) if applies(u, Ctx(g, civ=major))]
    if not us:
        pair.pop("unit_timer", None)
        return
    rng = g.state_rng("cs_unit", cs, major, g.turn)
    for u in us:
        n = int(u.n(0))
        if "unit_timer" not in pair or pair["unit_timer"] > n:
            pair["unit_timer"] = n + rng.choice([-1, 0, 1])
    pair["unit_timer"] -= 1
    if any(g.at_war(major, e.id) and g.at_war(cs, e.id) for e in g.s.players if e.alive):
        for u in g.civ_uniques(major, U.CityStateMoreGiftedUnits):
            pair["unit_timer"] -= int(u.n(0)) - 1
    if pair["unit_timer"] <= 0:
        pair.pop("unit_timer", None)
        give_military_unit(g, cs, major)


def give_military_unit(g: "Game", cs: int, major: int):
    """Give a major civilization a unit from a city-state, choosing something it can use."""
    from .units import place_unit_near, add_construction_bonuses
    from .cities import rejection_reasons
    R = g.rules
    p = g.player(cs)
    cap = g.city(p.capital) if p.capital is not None else None
    mcities = g.player_cities(major)
    if cap is None or not mcities:
        return
    target = min(mcities, key=lambda c: g.grid.distance(c.idx, cap.idx))
    unit = None
    uu = p.cs_unique_unit
    if uu and g.has_tech(major, R.units[uu].get("requiredTech")) and not (
            R.units[uu].get("obsoleteTech") and g.has_tech(major, R.units[uu]["obsoleteTech"])):
        unit = uu
    if unit is None:
        rng = g.state_rng("cs_gift_unit", cs, major, g.turn)
        cands = [n for n, d in R.units.items() if d["_military"] and d["_domain"] == "Land" and not d.get("uniqueTo")
                 and not rejection_reasons(g, target, n, pid=major)]
        cands = [n for n in cands if not R.units[n].get("requiredResource")
                 or g.resource_amount(major, R.units[n]["requiredResource"]) > 0]
        if not cands:
            return
        unit = rng.choice(sorted(cands))
    u = place_unit_near(g, major, unit, target.idx)
    if u is None:
        return
    add_construction_bonuses(g, u, cap)
    for x in g.civ_uniques(major, U.CityStateGiftedUnitsStartWithXp):
        u.xp += int(x.n(0))
    g.emit("cs_gift", f"{p.name} gave you a {unit} near {target.name}!", [major], idx=u.idx, unit=u.id)


def turns_for_gp_gift(g: "Game", major: int) -> int:
    """How long until a city-state gives a great person, with the randomness that makes it a surprise."""
    rng = g.state_rng("cs_gp", major, g.turn)
    return int((37 + rng.randrange(7)) * g.speed["modifier"])


def great_person_gift_tick(g: "Game", major: int):
    """Patronage finisher: allied city-states occasionally gift great people."""
    p = g.player(major)
    if "cs_gp_gift" not in p.flags:
        return
    allies = allied_city_states(g, major)
    if allies:
        p.flags["cs_gp_gift"] -= 1
    if allies and p.flags["cs_gp_gift"] < min(len(allies), 10) and g.player_cities(major):
        rng = g.state_rng("cs_gp_giver", major, g.turn)
        giver = rng.choice(allies)
        gps = [n for n in g.rules.great_person_units if not g.rules.units[n]["_umap"].get(U.MayFoundReligion)
               and not g.rules.units[n].get("uniqueTo")]
        if gps:
            from .units import place_unit_near
            cap = g.city(g.player(giver).capital)
            target = min(g.player_cities(major), key=lambda c: g.grid.distance(c.idx, cap.idx)) if cap else g.player_cities(major)[0]
            gp = rng.choice(sorted(gps))
            if place_unit_near(g, major, gp, target.idx):
                g.emit("cs_gift", f"{g.player(giver).name} gave you a {gp} as a gift!", [major], idx=target.idx)
        p.flags["cs_gp_gift"] = turns_for_gp_gift(g, major)


def _update_border_intrusion(g, cs):
    """Track units loitering in a city-state's territory, which costs influence."""
    if threatening_barbarians(g, cs) > 0:
        return
    for q in g.majors():
        if not g.has_met(cs, q.id) or g.at_war(cs, q.id):
            continue
        if g.civ_has(q.id, U.CityStateTerritoryAlwaysFriendly):
            continue
        pair = _pair(g, cs, q.id)
        if pair.get("anger_free"):
            continue
        n = sum(1 for u in g.player_units(q.id) if g.rules.units[u.type]["_military"] and g.s.tiles[u.idx].owner == cs)
        if n and relationship(g, cs, q.id) not in ("Friend", "Ally"):
            add_influence(g, cs, q.id, -10)
            if not pair.get("border_conflict"):
                pair["border_conflict"] = 10
                g.emit("cs_border", f"{g.player(cs).name} is angered by your military units in its territory (-10 influence).",
                       [q.id], player=cs)


def _free_techs(g, cs):
    """Give a city-state the technologies most of the major civilizations already have.

    City-states do not research. Without this they would still be fighting with warriors in the
    industrial era, which makes conquering one free rather than a decision.
    """
    from . import research
    majors = g.majors()
    for t in g.rules.tech_order:
        if research.is_repeatable(g, t) or not research.can_research(g, cs, t):
            continue
        if sum(1 for m in majors if g.has_tech(m.id, t)) > len(majors) / 2:
            research.add_tech_silently(g, cs, t)


def threatening_barbarians(g: "Game", cs: int) -> int:
    """How many barbarians are near enough to this city-state to count as a threat."""
    bid = g.barbarian_id
    if bid is None:
        return 0
    p = g.player(cs)
    cap = g.city(p.capital) if p.capital is not None else None
    if cap is None:
        return 0
    return sum(1 for u in g.player_units(bid) if g.rules.units[u.type]["_military"] and g.grid.distance(u.idx, cap.idx) <= 6)


def barbarian_killed_near(g: "Game", major: int, idx: int):
    """Credit a civilization with influence for killing barbarians near a city-state."""
    for q in g.city_states():
        cap = g.city(q.capital) if q.capital is not None else None
        if cap is None or not g.has_met(major, q.id) or g.at_war(major, q.id):
            continue
        if g.grid.distance(cap.idx, idx) <= 6:
            add_influence(g, q.id, major, 12)
            pair = _pair(g, q.id, major)
            pair["anger_free"] = pair.get("anger_free", 0) + 5
            g.emit("cs_grateful", f"{q.name} is grateful that you killed a barbarian threatening them (+12 influence).",
                   [major], player=q.id)


def on_meet(g: "Game", a: int, b: int):
    """First contact: city-states give 15 gold (30 to the first major) and 4 faith if religious."""
    for cs, major in ((a, b), (b, a)):
        if g.player(cs).kind == "city_state" and g.player(major).kind == "major":
            if is_aggressor(g, major):
                continue
            met_majors = sum(1 for x in g.player(cs).met if g.player(x).kind == "major")
            gold = 30 if met_majors == 1 else 15
            g.player(major).gold += gold
            text = f"{g.player(cs).name} gave you {gold} gold as a token of goodwill."
            if _provides_stat(g, cs, "faith"):
                g.player(major).faith += 4
                text += " (+4 faith)"
            g.emit("cs_meet", text, [major], player=cs)
            _pair(g, cs, major)


def _provides_stat(g, cs, stat) -> bool:
    """Whether this city-state's type grants a given stat to its ally."""
    ct = g.rules.city_state_types.get(g.player(cs).cs_type)
    return bool(ct) and any(u.stats.get(stat, 0) > 0 for u in ct["_ally"].all)


# ---------------------------------------------------------------------------------------------------------------
# Actions by majors
# ---------------------------------------------------------------------------------------------------------------
def _check_cs(g, major, cs):
    """Validate that the target is a living city-state this civilization has met, or raise."""
    if cs is None or cs >= len(g.s.players) or g.player(cs).kind != "city_state" or not g.player(cs).alive:
        raise ActionError("That is not a living city-state.")
    if not g.has_met(major, cs):
        raise ActionError("You have not met that city-state.")


def influence_from_gold(g: "Game", cs: int, donor: int, gold: int) -> int:
    """How much influence a gold gift buys, which falls as the game goes on.

    The scaling by game progress is what stops late-game gold from simply buying every city-state.
    """
    inf = gold ** 1.01 / 9.8
    sp = g.speed
    progress = min(g.turn / (400 * sp["modifier"]), 1)
    inf *= 1 - (2 / 3) * progress
    inf *= sp["goldGiftModifier"]
    for u in g.civ_uniques(donor, U.CityStateGoldGiftsProvideMoreInfluence):
        inf *= 1 + u.n(0) / 100
    inf *= _investment_multiplier(g, cs, donor)
    inf -= inf % 5
    return int(max(5.0, inf))


def gift_gold(g: "Game", major: int, cs: int, gold: int) -> dict:
    """Give a city-state gold in exchange for influence."""
    _check_cs(g, major, cs)
    if g.at_war(major, cs):
        raise ActionError("You cannot give gifts to a city-state you are at war with.")
    gold = int(gold)
    if gold not in (250, 500, 1000) and not 1 <= gold:
        raise ActionError("Gift an amount of gold (UnCiv offers 250, 500 or 1000).")
    p = g.player(major)
    if p.gold < gold:
        raise ActionError(f"You only have {int(p.gold)} gold.")
    inf = influence_from_gold(g, cs, major, gold)
    p.gold -= gold
    g.player(cs).gold += gold
    add_influence(g, cs, major, inf)
    _complete_quests(g, cs, major, "Give Gold")
    return {"city_state": g.player(cs).name, "gold": gold, "influence_gained": inf,
            "influence": round(raw_influence(g, cs, major), 1), "relationship": relationship(g, cs, major)}


def gift_unit(g: "Game", major: int, unit) -> dict:
    """Give a city-state a unit standing in its territory, in exchange for influence."""
    t = g.s.tiles[unit.idx]
    cs = t.owner
    if cs is None or g.player(cs).kind != "city_state":
        raise ActionError("Move the unit into a city-state's territory to gift it.")
    if g.at_war(major, cs):
        raise ActionError("No gifts to a city-state you are at war with.")
    ud = g.rules.units[unit.type]
    from .units import unit_uniques
    special = [u for u in unit_uniques(g, unit, U.GainInfluenceWithUnitGiftToCityState, with_civ=True)
               if unit_matches(g, unit, u.p(1))]
    if not ud["_military"] and not special:
        raise ActionError("City-states only accept military units (or units your civilization may gift).")
    if unit.moves <= 0:
        raise ActionError("The unit has no movement left.")
    gain = 5.0 + (special[0].n(0) - 5 if special else 0)
    add_influence(g, cs, major, gain)
    if ud["_great_person"]:
        g.remove_unit(unit)
    else:
        g.change_owner(unit, cs)
    return {"gifted": unit.type, "city_state": g.player(cs).name, "influence_gained": gain}


def can_pledge(g: "Game", cs: int, major: int) -> Optional[str]:
    """Why this civilization cannot pledge to protect this city-state, or None."""
    p = g.player(cs)
    pair = _pair(g, cs, major)
    if pair.get("withdrew"):
        return f"You withdrew protection recently ({pair['withdrew']} turns left)."
    if influence(g, cs, major) < 0:
        return "Influence must be at least 0."
    if g.at_war(cs, major):
        return "You are at war with them."
    if major in p.protectors:
        return "You already protect them."
    return None


def pledge(g: "Game", major: int, cs: int) -> dict:
    """Pledge to protect a city-state, which it expects to be honoured."""
    _check_cs(g, major, cs)
    reason = can_pledge(g, cs, major)
    if reason:
        raise ActionError(reason)
    g.player(cs).protectors.append(major)
    _pair(g, cs, major)["pledged"] = 10
    _complete_quests(g, cs, major, "Pledge to Protect")
    g.invalidate()
    return {"pledged_protection": g.player(cs).name}


def withdraw_protection(g: "Game", major: int, cs: int, forced: bool = False) -> dict:
    """Withdraw protection, at a cost to influence.

    Also called with ``forced`` when a civilization fails to defend a city-state it had pledged to -
    the same consequence, arrived at by breaking the promise rather than by renouncing it.
    """
    _check_cs(g, major, cs)
    p = g.player(cs)
    pair = _pair(g, cs, major)
    if major not in p.protectors:
        raise ActionError("You are not protecting them.")
    if not forced and pair.get("pledged"):
        raise ActionError(f"You pledged recently and cannot withdraw for {pair['pledged']} more turns.")
    p.protectors.remove(major)
    pair["withdrew"] = 20
    add_influence(g, cs, major, -20)
    return {"withdrew_protection": p.name}


def tribute_willingness(g: "Game", cs: int, major: int, worker: bool = False) -> int:
    """How willing a city-state is to pay tribute: positive means it will."""
    return sum(tribute_modifiers(g, cs, major, worker).values())


def tribute_modifiers(g: "Game", cs: int, major: int, worker: bool = False) -> dict:
    """Why a city-state will or will not pay tribute, broken down by reason.

    A breakdown rather than a number so the player can see what would change it - usually "bring an
    army closer".
    """
    p = g.player(cs)
    mods: dict = {}
    cap = g.city(p.capital) if p.capital is not None else None
    if cap is None:
        return {"No Cities": -999}
    mods["Base value"] = -110
    if p.cs_personality == "Hostile":
        mods["Hostile"] = -10
    if p.ally is not None and p.ally != major:
        mods["Has Ally"] = -10
    if any(x != major for x in p.protectors):
        mods["Has Protector"] = -20
    if worker:
        mods["Demanding a Worker"] = -30
        if cap.pop < 4:
            mods["Demanding a Worker from small City-State"] = -300
    recent = p.flags.get("recently_bullied", 0)
    if recent > 10:
        mods["Very recently paid tribute"] = -300
    elif recent > 0:
        mods["Recently paid tribute"] = -40
    if influence(g, cs, major) < -30:
        mods["Influence below -30"] = -300
    if sum(mods.values()) < -200:
        return mods
    from .victory import military_strength
    majors = sorted(g.majors(), key=lambda q: -military_strength(g, q.id))
    rank = next((i for i, q in enumerate(majors) if q.id == major), len(majors))
    n = max(1, len(majors))
    mods["Military Rank"] = g.rules.k["tribute_global_modifier"] * (n - rank) // n
    if sum(mods.values()) < -100:
        return mods
    rng_ = max(5, min(10, g.s.height // 10))
    from .combat import city_strength
    near = 0
    own = city_strength(g, cap) ** 1.5
    for i in g.grid.within(cap.idx, rng_):
        if i == cap.idx:
            continue
        m = g.military_at(i)
        if m is None:
            continue
        force = _force(g, m)
        if m.owner == major:
            near += force
        elif m.owner == cs:
            own += force
    ratio = near / max(1.0, own)
    lm = g.rules.k["tribute_local_modifier"]
    mods["Military near City-State"] = lm if ratio > 3 else (lm * 4 // 5 if ratio > 2 else (lm * 3 // 5 if ratio > 1.5 else
                                         (lm * 2 // 5 if ratio > 1 else (lm // 5 if ratio > 0.5 else 0))))
    return mods


def _force(g, u) -> float:
    """A unit's military weight, used for comparing armies rather than counting units."""
    ud = g.rules.units[u.type]
    s = max(ud["strength"], ud["rangedStrength"])
    return (s ** 1.5) * u.hp / 100


def tribute_gold_amount(g: "Game") -> int:
    """How much gold a successful demand extracts, scaled by game speed."""
    sp = g.speed
    gold = int(10 * sp["goldGiftModifier"]) * 5
    gold += 5 * int(g.turn / sp["cityStateTributeScalingInterval"])
    return gold


def demand_tribute(g: "Game", major: int, cs: int, worker: bool = False) -> dict:
    """Demand gold or a worker from a city-state, at a large cost to influence."""
    _check_cs(g, major, cs)
    will = tribute_willingness(g, cs, major, worker)
    if will <= 0:
        mods = tribute_modifiers(g, cs, major, worker)
        raise ActionError(f"{g.player(cs).name} refuses to pay tribute (willingness {will}: "
                          + ", ".join(f"{k} {v:+d}" for k, v in mods.items()) + ").")
    p = g.player(cs)
    if worker:
        from .units import place_unit_near
        cap = g.city(p.capital)
        place_unit_near(g, major, "Worker", cap.idx)
        add_influence(g, cs, major, -50)
        res = {"tribute": "Worker"}
    else:
        gold = tribute_gold_amount(g)
        g.player(major).gold += gold
        add_influence(g, cs, major, -15)
        res = {"tribute": f"{gold} gold"}
    _bullied(g, cs, major)
    p.flags["recently_bullied"] = 20
    return res


def _bullied(g, cs, bully):
    """Record that a city-state was bullied, which other city-states hold against the bully."""
    from . import diplomacy
    for pr in g.player(cs).protectors:
        if g.has_met(pr, bully):
            diplomacy.add_opinion(g, pr, bully, "bullied_protected_minor", -15)
    _pair(g, cs, bully)["bullied"] = 20
    for q in g.city_states():
        _quest_event(g, q.id, "Bully City State", bully, target=cs)
    # revoke quests
    p = g.player(cs)
    before = len(p.quests)
    p.quests = [x for x in p.quests if not (x["assignee"] == bully and (x["kind"] == "individual" or x["name"] == "Invest"))]
    if len(p.quests) != before:
        g.emit("cs_quest", f"{p.name} cancelled its quests for you because you demanded tribute.", [bully], player=cs)


def on_attacked(g: "Game", cs: int, attacker: int):
    """CityStateFunctions.cityStateAttacked: protectors and allies are upset; the city-state grows wary."""
    from . import diplomacy
    if g.player(attacker).kind != "major":
        return
    ap = g.player(attacker)
    ap.flags["cs_attacks"] = ap.flags.get("cs_attacks", 0) + 1
    pair = _pair(g, cs, attacker)
    rng = g.state_rng("cs_attacked", cs, attacker, g.turn)
    if is_warmonger(g, attacker) or (is_aggressor(g, attacker) and rng.random() < 0.5):
        if not pair.get("wary"):
            pair["wary"] = True
            g.emit("cs_wary", f"City-states grow wary of your aggression (resting influence -20 with {g.player(cs).name}).",
                   [attacker])
    for pr in g.player(cs).protectors:
        if pr != attacker and g.has_met(pr, attacker):
            diplomacy.add_opinion(g, pr, attacker, "attacked_protected_minor", -20)
    al = g.player(cs).ally
    if al is not None and al != attacker and g.has_met(al, attacker):
        diplomacy.add_opinion(g, al, attacker, "attacked_allied_minor", -10)
    pair["recently_attacked"] = 2
    # war-with-major pseudo quest (QuestManager.wasAttackedBy)
    need = max(3, sum(1 for u in g.player_units(attacker) if g.rules.units[u.type]["_military"]) // 4)
    wars = g.player(cs).flags.setdefault("war_quests", {})
    if str(attacker) not in wars:
        wars[str(attacker)] = {"needed": need, "kills": {}}
        for q in g.majors():
            if q.id != attacker and g.has_met(cs, q.id) and not g.at_war(cs, q.id):
                g.emit("cs_quest", f"{g.player(cs).name} is being attacked by {ap.name}! Kill {need} of the attacker's "
                                   f"military units and they will be immensely grateful.", [q.id], player=cs)


def on_destroyed(g: "Game", cs: int, attacker: int):
    """Handle a city-state being destroyed: its alliances and quests end with it."""
    from . import diplomacy
    for pr in g.player(cs).protectors:
        if g.has_met(pr, attacker):
            diplomacy.add_opinion(g, pr, attacker, "destroyed_protected_minor", -40)
    for q in g.city_states():
        _quest_event(g, q.id, "Conquer City State", attacker, target=cs)


def on_military_unit_killed(g: "Game", killer: int, victim_owner: int):
    """War-with-major pseudo quest: city-states attacked by `victim_owner` reward units killed."""
    for q in g.city_states():
        wars = q.flags.get("war_quests", {})
        info = wars.get(str(victim_owner))
        if not info or not g.has_met(q.id, killer) or g.at_war(q.id, killer):
            continue
        kills = info.setdefault("kills", {})
        kills[str(killer)] = kills.get(str(killer), 0) + 1
        if kills[str(killer)] >= info["needed"]:
            add_influence(g, q.id, killer, 100)
            g.emit("cs_quest", f"{q.name} is deeply grateful for your help against {g.player(victim_owner).name} "
                               f"(+100 influence).", [killer], player=q.id)
            del wars[str(victim_owner)]


def marriage_cost(g: "Game", cs: int) -> int:
    """Gold needed to annex a long-standing allied city-state."""
    cost = int(500 * g.speed["goldCostModifier"])
    from .cities import base_gold_cost
    for u in g.player_units(cs):
        cost += int(base_gold_cost(g, cs, u.type, None)) // 20
    return cost // 5 * 5


def buyout(g: "Game", major: int, cs: int) -> dict:
    """Austria: marry into the ruling family of a long-time ally."""
    _check_cs(g, major, cs)
    p = g.player(cs)
    if p.ally != major or relationship(g, cs, major) != "Ally":
        raise ActionError("They must be your ally.")
    if not g.civ_has(major, U.CityStateCanBeBoughtForGold):
        raise ActionError("Your civilization cannot annex city-states.")
    if _pair(g, cs, major).get("marriage_cooldown"):
        raise ActionError(f"You must be allied for {_pair(g, cs, major)['marriage_cooldown']} more turns.")
    cost = marriage_cost(g, cs)
    if g.player(major).gold < cost:
        raise ActionError(f"This costs {cost} gold.")
    g.player(major).gold -= cost
    from .conquest import move_to_civ
    for u in list(g.player_units(cs)):
        g.change_owner(u, major)
    for c in list(g.player_cities(cs)):
        c.founder = major
        c.original_capital = False
        move_to_civ(g, c, major)
        c.puppet = True
    from . import victory
    victory.check_elimination(g, cs)
    g.emit("cs_married", f"{g.player(major).name} married into the ruling family of {p.name}.", None)
    return {"annexed": p.name, "gold_spent": cost}


# ---------------------------------------------------------------------------------------------------------------
# Quests
# ---------------------------------------------------------------------------------------------------------------
def _investment_multiplier(g, cs, donor) -> float:
    """How much a civilization's past gifts increase the value of its next one."""
    for q in g.player(cs).quests:
        if q["name"] == "Invest" and q["assignee"] == donor:
            return 1 + q.get("data1", 50) / 100
    return 1.0


def _quest_valid(g, cs, qname, major) -> Optional[tuple]:
    """Returns (data1, data2) if the quest can be given to `major`, else None."""
    p = g.player(cs)
    R = g.rules
    cap = g.city(p.capital) if p.capital is not None else None
    if cap is None or not g.has_met(cs, major) or g.at_war(cs, major) or not g.player(major).alive:
        return None
    rng = g.state_rng("quest", cs, major, qname, g.turn)
    if qname == "Route":
        mc = g.player_cities(major)
        if not mc:
            return None
        if _route_connected(g, major, cap):
            return None
        if any(g.continent(c.idx) == g.continent(cap.idx) and g.grid.distance(c.idx, cap.idx) <= 7 for c in mc):
            return ("", "")
        return None
    if qname == "Clear Barbarian Camp":
        camps = [i for i in g.grid.within(cap.idx, 8) if g.s.tiles[i].improvement == "Barbarian encampment"]
        return (camps[rng.randrange(len(camps))], "") if camps else None
    if qname == "Connect Resource":
        own_cs = {r for r, _, a in _res(g, cs)}
        own_m = {r for r, _, a in _res(g, major)}
        from .tiles import resource_visible
        cands = sorted(r for r in g.resources_on_map() if R.resources[r]["resourceType"] != "Bonus"
                       and resource_visible(g, major, r) and r not in own_cs and r not in own_m)
        return (rng.choice(cands), "") if cands else None
    if qname == "Construct Wonder":
        cands = []
        for n, b in R.buildings.items():
            if not b.get("isWonder") or b.get("uniqueTo") or not g.has_tech(major, b.get("requiredTech")):
                continue
            if n in g.s.wonders_built:
                continue
            if any(c.progress.get(n, 0) * 3 > 0 and c.progress.get(n, 0) * 3 > (b["cost"] - c.progress.get(n, 0))
                   for c in g.s.cities.values()):
                continue
            cands.append(n)
        return (rng.choice(sorted(cands)), "") if cands else None
    if qname == "Acquire Great Person":
        from .great_people import great_people_types
        have = {u.type for u in g.player_units(major) if R.units[u.type]["_great_person"]} | \
               {u.type for u in g.player_units(cs) if R.units[u.type]["_great_person"]}
        cands = sorted(t for t in great_people_types(g, major) if t not in have)
        return (rng.choice(cands), "") if cands else None
    if qname in ("Conquer City State", "Bully City State"):
        if qname == "Conquer City State" and p.cs_personality == "Friendly":
            return None
        others = [q for q in g.city_states() if q.id != cs and g.has_met(major, q.id) and g.has_met(cs, q.id)
                  and q.capital is not None and g.city(q.capital)]
        if not others:
            return None
        dists = {q.id: g.grid.distance(cap.idx, g.city(q.capital).idx) for q in others}
        closest = min(dists.values())
        if closest > 20:
            return None
        cands = sorted(k for k, v in dists.items() if v == closest)
        return (rng.choice(cands), "")
    if qname == "Find Player":
        cands = sorted(q.id for q in g.majors() if q.id != major and g.has_met(major, q.id)
                       and not any(g.player(major).explored[c.idx] for c in g.player_cities(q.id)))
        return (rng.choice(cands), "") if cands else None
    if qname == "Find Natural Wonder":
        all_nw = {t.wonder for t in g.s.tiles if t.wonder}
        cands = sorted(all_nw - set(g.player(major).natural_wonders) - set(p.natural_wonders))
        return (rng.choice(cands), "") if cands else None
    if qname in ("Give Gold", "Pledge to Protect", "Denounce Civilization"):
        bully = _most_recent_bully(g, cs)
        if bully is None:
            return None
        if qname == "Pledge to Protect" and major in p.protectors:
            return None
        if qname == "Denounce Civilization":
            rel = g.relation(major, bully)
            if bully == major or not rel or rel.get("war") or rel.get("denounced_by", {}).get(str(major)):
                return None
        return (bully, "")
    if qname == "Spread Religion":
        from . import religion
        pr = g.player(major).religion
        if not pr or not religion.is_major(g, pr) or religion.majority_religion(g, cap) == pr:
            return None
        return (pr, "")
    if qname == "Contest Culture":
        return (g.player(major).flags.get("total_culture", 0), "")
    if qname == "Contest Faith":
        if not g.religion_enabled:
            return None
        return (g.player(major).flags.get("total_faith", 0), "")
    if qname == "Contest Technologies":
        return (len(g.player(major).techs) + g.player(major).future_techs, "")
    if qname == "Invest":
        return (R.quests["Invest"].get("params", [50])[0], "")
    return None


def _res(g, pid):
    """The resource a quest is about, if any."""
    from .economy import detailed_resources
    return [x for x in detailed_resources(g, pid) if x[2] > 0]


def _route_connected(g, major, cs_cap) -> bool:
    """Whether a major civilization has a trade route to this city-state, for the route quest."""
    from .movement import has_connection
    p = g.player(major)
    cap = g.city(p.capital) if p.capital is not None else None
    if cap is None:
        return False
    seen = {cap.idx}
    stack = [cap.idx]
    while stack:
        cur = stack.pop()
        if cur == cs_cap.idx:
            return True
        for n in g.grid.neighbors(cur):
            if n in seen:
                continue
            if n == cs_cap.idx or has_connection(g, major, n):
                seen.add(n)
                stack.append(n)
    return False


def _most_recent_bully(g, cs) -> Optional[int]:
    """The last civilization to bully this city-state, whom it would like dealt with."""
    pairs = g.player(cs).flags.get("pairs", {})
    best, best_v = None, 0
    for k, v in pairs.items():
        b = v.get("bullied", 0)
        if b > best_v:
            best, best_v = int(k), b
    return best


def _quest_weight(g, cs, qname) -> float:
    """How likely this quest type is to be chosen, given the city-state's situation."""
    q = g.rules.quests[qname]
    p = g.player(cs)
    w = 1.0
    wt = q.get("weightForCityStateType", {})
    if p.cs_personality in wt:
        w *= wt[p.cs_personality]
    if p.cs_type in wt:
        w *= wt[p.cs_type]
    return w


def _weighted(rng, items, weights):
    """Pick a quest type at random, weighted by suitability."""
    total = sum(weights)
    if total <= 0:
        return items[0]
    r = rng.random() * total
    for it, w in zip(items, weights):
        r -= w
        if r <= 0:
            return it
    return items[-1]


def _quests_end_turn(g, cs):
    """Advance every quest: expire, complete, obsolete, and hand out new ones."""
    p = g.player(cs)
    if not g.player_cities(cs):
        return
    R = g.rules
    qs = p.flags.setdefault("quest_state", {"global": -1, "individual": {}})
    turn = g.turn
    rng = g.state_rng("quests", cs, turn)
    sp = g.speed["modifier"]
    if turn >= 30:
        if qs["global"] == -1:
            qs["global"] = int((rng.randrange(20) if turn == 30 else 40 + rng.randrange(25)) * sp)
        for m in g.majors():
            if qs["individual"].get(str(m.id), -1) == -1:
                qs["individual"][str(m.id)] = int((rng.randrange(20) if turn == 30 else 20 + rng.randrange(25)) * sp)
    if qs["global"] > 0:
        qs["global"] -= 1
    for k in qs["individual"]:
        if qs["individual"][k] > 0:
            qs["individual"][k] -= 1
    # remove quests for defeated / hostile assignees, handle expired globals
    p.quests = [x for x in p.quests if g.player(x["assignee"]).alive and not g.at_war(cs, x["assignee"])]
    expired_globals = {x["name"] for x in p.quests if x["kind"] == "global" and _expired(g, x)}
    for name in expired_globals:
        _resolve_contest(g, cs, name)
    # individual quests
    keep = []
    for x in p.quests:
        if x["kind"] != "individual":
            keep.append(x)
            continue
        if _complete(g, cs, x):
            _reward(g, cs, x)
        elif _obsolete(g, cs, x) or _expired(g, x):
            g.emit("cs_quest", f"{p.name} no longer needs your help with the {x['name']} quest.", [x["assignee"]], player=cs)
        else:
            keep.append(x)
    p.quests = keep
    # new global quest
    if qs["global"] == 0 and sum(1 for x in p.quests if x["kind"] == "global") < 1:
        majors = [m for m in g.majors() if g.has_met(cs, m.id) and not g.at_war(cs, m.id)]
        cands = [n for n, q in R.quests.items() if q.get("type") == "Global"
                 and sum(1 for m in majors if _quest_valid(g, cs, n, m.id) is not None) >= q.get("minimumCivs", 1)]
        if cands:
            n = _weighted(rng, cands, [_quest_weight(g, cs, c) for c in cands])
            for m in majors:
                data = _quest_valid(g, cs, n, m.id)
                if data is not None:
                    _assign(g, cs, n, m.id, data, "global")
            qs["global"] = -1
    # new individual quests
    for k, cd in list(qs["individual"].items()):
        m = int(k)
        if cd != 0 or not g.player(m).alive:
            continue
        if sum(1 for x in p.quests if x["kind"] == "individual" and x["assignee"] == m) >= 2:
            continue
        if _pair(g, cs, m).get("bullied"):
            continue
        cands = [n for n, q in R.quests.items() if q.get("type", "Individual") != "Global"
                 and not any(x["name"] == n and x["assignee"] == m for x in p.quests)
                 and _quest_valid(g, cs, n, m) is not None]
        if cands:
            n = _weighted(rng, cands, [_quest_weight(g, cs, c) for c in cands])
            _assign(g, cs, n, m, _quest_valid(g, cs, n, m), "individual")
            qs["individual"][k] = -1
    # barbarian invasion call for help / war pseudo quests
    if threatening_barbarians(g, cs) >= 2 and not p.flags.get("barb_help_cd"):
        for m in g.majors():
            if g.has_met(cs, m.id) and not g.at_war(cs, m.id):
                g.emit("cs_quest", f"{p.name} is being invaded by barbarians! Destroy barbarians near their territory "
                                   f"to earn influence.", [m.id], player=cs)
        p.flags["barb_help_cd"] = 30
    if p.flags.get("barb_help_cd"):
        p.flags["barb_help_cd"] -= 1


def _assign(g, cs, name, major, data, kind):
    """Give a city-state a new quest of a chosen type."""
    q = g.rules.quests[name]
    x = {"name": name, "assignee": major, "turn": g.turn, "kind": kind, "data1": data[0], "data2": data[1],
         "influence": q.get("influence", 40), "duration": int(g.speed["modifier"] * q.get("duration", 0))}
    g.player(cs).quests.append(x)
    g.emit("cs_quest", f"{g.player(cs).name} assigned you a new quest: {name}" + _quest_detail(g, x) + ".", [major],
           player=cs)


def quest_text(g, x) -> str:
    """A quest as a sentence a player can act on."""
    return x["name"] + _quest_detail(g, x)


def _quest_detail(g, x) -> str:
    """The structured form of a quest, for the client."""
    d = x["data1"]
    n = x["name"]
    if n == "Clear Barbarian Camp":
        return f" (camp at {g.fmt_xy(d)})"
    if n in ("Connect Resource", "Construct Wonder", "Acquire Great Person", "Find Natural Wonder"):
        return f" ({d})"
    if n in ("Conquer City State", "Bully City State", "Find Player", "Give Gold", "Pledge to Protect", "Denounce Civilization"):
        return f" ({g.player(d).name})" if isinstance(d, int) else ""
    if n == "Spread Religion":
        from . import religion
        return f" ({religion.display_name(g, d)})"
    if n == "Invest":
        return f" (gold gifts give {int(d)}% more influence)"
    return ""


def _expired(g, x) -> bool:
    """Whether a quest has run out of time."""
    return x["duration"] > 0 and g.turn >= x["turn"] + x["duration"]


def _complete(g, cs, x) -> bool:
    """Mark a quest complete for one civilization and pay the reward."""
    n, m, d = x["name"], x["assignee"], x["data1"]
    p = g.player(cs)
    cap = g.city(p.capital) if p.capital is not None else None
    R = g.rules
    if n == "Route":
        return cap is not None and _route_connected(g, m, cap)
    if n == "Construct Wonder":
        return any(d in c.buildings for c in g.player_cities(m))
    if n == "Connect Resource":
        return any(r == d for r, _, a in _res(g, m))
    if n == "Acquire Great Person":
        return any(u.type == d or R.units[u.type].get("replaces") == d for u in g.player_units(m))
    if n == "Find Player":
        return any(g.player(m).explored[c.idx] for c in g.player_cities(d)) if isinstance(d, int) else False
    if n == "Find Natural Wonder":
        return d in g.player(m).natural_wonders
    if n == "Pledge to Protect":
        return m in p.protectors
    if n == "Denounce Civilization":
        rel = g.relation(m, d)
        return bool(rel and rel.get("denounced_by", {}).get(str(m)))
    if n == "Spread Religion":
        from . import religion
        return cap is not None and religion.majority_religion(g, cap) == d
    return False


def _obsolete(g, cs, x) -> bool:
    """Whether a quest no longer makes sense - its target is gone, or somebody else did it."""
    n, d = x["name"], x["data1"]
    if n == "Clear Barbarian Camp":
        return g.s.tiles[d].improvement != "Barbarian encampment"
    if n == "Construct Wonder":
        return d in g.s.wonders_built and g.city(g.s.wonders_built[d]) is not None and \
            g.city(g.s.wonders_built[d]).owner != x["assignee"]
    if n in ("Conquer City State", "Bully City State", "Find Player", "Denounce Civilization"):
        return isinstance(d, int) and not g.player(d).alive
    return False


def _reward(g, cs, x):
    """The influence a completed quest is worth."""
    inf = x["influence"]
    add_influence(g, cs, x["assignee"], inf)
    if inf > 0:
        g.emit("cs_quest", f"{g.player(cs).name} rewarded you with {int(inf)} influence for completing the "
                           f"{x['name']} quest.", [x["assignee"]], player=cs)


def _complete_quests(g, cs, major, name):
    """Check every open quest and complete those whose conditions are now met."""
    p = g.player(cs)
    keep = []
    for x in p.quests:
        if x["name"] == name and x["assignee"] == major:
            _reward(g, cs, x)
        else:
            keep.append(x)
    p.quests = keep


def _quest_event(g, cs, name, major, target=None):
    """Announce something that happened to a quest."""
    p = g.player(cs)
    keep = []
    for x in p.quests:
        if x["name"] == name and x["assignee"] == major and (target is None or x["data1"] == target):
            _reward(g, cs, x)
        else:
            keep.append(x)
    p.quests = keep


def camp_cleared(g: "Game", idx: int, major: int):
    """Credit whoever cleared a barbarian camp a city-state was worried about."""
    for q in g.city_states():
        matching = [x for x in q.quests if x["name"] == "Clear Barbarian Camp" and x["data1"] == idx]
        win = next((x for x in matching if x["assignee"] == major), None)
        if win:
            _reward(g, q.id, win)
        q.quests = [x for x in q.quests if x not in matching]


def _resolve_contest(g, cs, name):
    """Decide a contest quest, where several civilizations competed and the best result wins."""
    p = g.player(cs)
    qs = [x for x in p.quests if x["name"] == name]

    def score(x):
        """One civilization's score in this contest."""
        m = g.player(x["assignee"])
        if name == "Contest Culture":
            return m.flags.get("total_culture", 0) - x["data1"]
        if name == "Contest Faith":
            return m.flags.get("total_faith", 0) - x["data1"]
        if name == "Contest Technologies":
            return len(m.techs) + m.future_techs - x["data1"]
        return 0
    best = max((score(x) for x in qs), default=0)
    for x in qs:
        s = score(x)
        if s > 0 and s == best:
            _reward(g, cs, x)
        else:
            g.emit("cs_quest", f"The {name} quest for {p.name} has ended.", [x["assignee"]], player=cs)
    p.quests = [x for x in p.quests if x["name"] != name]


def quests_for(g: "Game", major: int) -> list[dict]:
    """The quests a city-state is currently offering to a civilization."""
    out = []
    for q in g.city_states():
        if not g.has_met(major, q.id):
            continue
        for x in q.quests:
            if x["assignee"] == major:
                left = (x["turn"] + x["duration"] - g.turn) if x["duration"] else None
                out.append({"city_state": q.name, "city_state_id": q.id, "quest": quest_text(g, x),
                            "influence": x["influence"], "turns_left": left})
    return out


# ---------------------------------------------------------------------------------------------------------------
# Minor civ AI
# ---------------------------------------------------------------------------------------------------------------
def take_turn(g: "Game", cs: int):
    """Simple city-state behaviour: keep a garrison, build defenders/buildings, attack adjacent enemies."""
    from .cities import set_production, buildable_items
    from .combat import attack, city_bombard, bombard_targets, validate_attack
    from .movement import move_toward
    from . import units as unitmod
    p = g.player(cs)
    R = g.rules
    _found_with_settlers(g, cs)
    for c in g.player_cities(cs):
        # bombard
        for t in bombard_targets(g, c):
            try:
                city_bombard(g, c, t)
                break
            except Exception:
                continue
        from .economy import civ_stats
        gpt = civ_stats(g, cs)["gold"]
        if c.queue and gpt < 0 and p.gold < 50 and c.queue[0] in R.buildings and R.buildings[c.queue[0]].get("maintenance"):
            c.queue = []
        if not c.queue:
            items = buildable_items(g, c)
            mil = [u for u in items["units"] if R.units[u]["_military"] and R.units[u]["_domain"] == "Land"]
            n_mil = sum(1 for u in g.player_units(cs) if R.units[u.type]["_military"])
            affordable = [b for b in items["buildings"] if gpt - R.buildings[b].get("maintenance", 0) >= 0]
            choice = None
            if gpt < 0 and p.gold < 50:
                choice = "Gold" if "Gold" in items.get("other", []) else None
            elif n_mil < 2 + len(g.player_cities(cs)) and mil:
                choice = max(mil, key=lambda u: R.units[u].get("strength", 0) + R.units[u].get("rangedStrength", 0))
            elif affordable:
                pref = ["Walls", "Monument", "Granary", "Shrine", "Library", "Castle", "Temple", "Market"]
                choice = next((b for b in pref if b in affordable), affordable[0])
            elif "Gold" in items.get("other", []):
                choice = "Gold"
            if choice:
                try:
                    set_production(g, c, choice)
                except ActionError:
                    pass
    cap = g.city(p.capital) if p.capital is not None else None
    for u in list(g.player_units(cs)):
        if g.unit(u.id) is None:
            continue
        ud = R.units[u.type]
        if not ud["_military"]:
            continue
        # attack adjacent / in-range enemies
        done = False
        rng_ = unitmod.attack_range(g, u)
        for t in g.grid.within(u.idx, rng_):
            if t == u.idx:
                continue
            try:
                validate_attack(g, u, t)
            except ActionError:
                continue
            try:
                attack(g, u, t)
                done = True
                break
            except ActionError:
                continue
        if done or g.unit(u.id) is None or cap is None:
            continue
        if g.military_at(cap.idx) is None and u.idx != cap.idx:
            try:
                move_toward(g, u, cap.idx, set_goto=False)
            except ActionError:
                pass
        elif g.grid.distance(u.idx, cap.idx) > 3:
            try:
                move_toward(g, u, cap.idx, set_goto=False)
            except ActionError:
                pass
        else:
            u.activity = "fortify"


def camp_removed(g: "Game", idx: int):
    """A camp vanished without being cleared by a unit: its quests become obsolete."""
    for q in g.city_states():
        q.quests = [x for x in q.quests if not (x["name"] == "Clear Barbarian Camp" and x["data1"] == idx)]


def _found_with_settlers(g: "Game", cs: int):
    """Minor civs found their city where their settler stands (or on the nearest valid tile)."""
    from .cities import found_check, found_city
    from .game import ActionError
    for u in list(g.player_units(cs)):
        if not g.rules.units[u.type]["_umap"].get(U.FoundCity) or g.player_cities(cs):
            continue
        spots = [i for i in g.grid.within(u.idx, 2) if found_check(g, cs, i) is None]
        if not spots:
            continue
        idx = u.idx if u.idx in spots else spots[0]
        try:
            found_city(g, cs, idx, unit=u)
        except ActionError:
            continue
