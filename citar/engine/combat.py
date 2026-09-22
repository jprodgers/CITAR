"""Combat: modifiers, damage, melee/ranged attacks, city bombardment, air interception & sweeps, nukes, captures.
Port of UnCiv's Battle, BattleDamage, BattleConstants, CityCombatant, MapUnitCombatant, AirInterception,
BattleUnitCapture, GreatGeneralImplementation, TargetHelper and Nuke (MPL-2.0).
"""
from __future__ import annotations

import random
from typing import Optional, TYPE_CHECKING

from . import unique_types as U
from .game import ActionError
from .state import Unit, City
from .uniques import Ctx, unit_matches, city_matches, tile_matches, STAT_KEY
from . import tiles as T

if TYPE_CHECKING:
    from .game import Game

LANDING_MALUS = -50
BOARDING_MALUS = -50
RIVER_MALUS = -20
FLANKING = 10.0
MISSING_RESOURCE_MALUS = -25
FORTIFICATION = 20
WOUNDED_RATIO = 300.0
DAMAGE_TO_CIVILIAN = 40


class Combatant:
    """One side of a fight: either a unit or a city, never both.

    Cities fight in this game - they have strength, they take damage, they shoot back - so almost every
    combat rule would otherwise need two versions. Wrapping both in one type means the damage formula,
    the modifier stack and the experience rules are written once, and the handful of genuine
    differences are the ``is_city`` branches you can see.
    """
    __slots__ = ("unit", "city", "owner")

    def __init__(self, unit: Optional[Unit] = None, city: Optional[City] = None):
        self.unit = unit
        self.city = city
        self.owner = unit.owner if unit is not None else city.owner

    @property
    def idx(self) -> int:
        """The tile this combatant occupies."""
        return self.unit.idx if self.unit is not None else self.city.idx

    def ud(self, g) -> Optional[dict]:
        """The ruleset definition of the unit, or None for a city."""
        return g.rules.units[self.unit.type] if self.unit is not None else None

    def is_city(self) -> bool:
        """Whether this side is a city."""
        return self.city is not None

    def is_ranged(self, g) -> bool:
        """Whether it attacks at a distance. Cities always do."""
        return self.city is not None or self.ud(g)["_ranged"]

    def is_melee(self, g) -> bool:
        """Whether it must be adjacent to attack, and so moves into the tile if it wins."""
        return self.unit is not None and self.ud(g)["_melee"]

    def is_air(self, g) -> bool:
        """Whether it is an aircraft, which changes almost every rule that follows."""
        return self.unit is not None and self.ud(g)["_domain"] == "Air"

    def is_civilian(self, g) -> bool:
        """Whether it is a civilian, which is captured rather than killed."""
        return self.unit is not None and not self.ud(g)["_military"]

    def is_land(self, g) -> bool:
        """Whether it is a land unit."""
        return self.unit is not None and self.ud(g)["_domain"] == "Land"

    def hp(self, g) -> int:
        """Current hit points: a unit's health, or a city's."""
        return self.unit.hp if self.unit is not None else self.city.health

    def defeated(self, g) -> bool:
        """Whether this side has lost the fight.

        A city is defeated at 1 health rather than 0, because a city is never destroyed by damage - it is
        captured by a melee unit moving in, which is what makes the last hit different from the others.
        """
        if self.unit is not None:
            return self.unit.hp <= 0 or g.unit(self.unit.id) is None
        return self.city.health <= 1

    def matches(self, g, f: str) -> bool:
        """Whether this combatant matches a unique's filter, such as "Land units" or "City"."""
        if self.city is not None:
            from .uniques import multi_filter
            return multi_filter(f, lambda s: s == "City" or s in ("All", "all") or city_matches(g, self.city, s))
        return unit_matches(g, self.unit, f)

    def name(self, g) -> str:
        """What to call this side in a notification."""
        return self.city.name if self.city is not None else self.unit.type

    def take_damage(self, g, dmg: int):
        """Apply damage, flooring a city at 1 health and recording when it was hit.

        The floor is the same rule as :meth:`defeated`: damage brings a city to the brink and no further.
        The turn stamp is what city healing keys off, so a city under repeated attack never recovers.
        """
        if self.unit is not None:
            self.unit.hp = max(0, self.unit.hp - dmg)
        else:
            self.city.health = max(1, self.city.health - dmg)
            self.city.damaged_turn = g.turn


def combatant_at(g: "Game", idx: int) -> Optional[Combatant]:
    """Whatever would defend this tile: a city first, then a military unit, then a civilian.

    The order is the priority of defence. A city with a unit in it is defended by the city, which is
    why taking a defended city means beating the city's strength rather than the garrison's.
    """
    c = g.city_at(idx)
    if c is not None:
        return Combatant(city=c)
    m = g.military_at(idx)
    if m is not None:
        return Combatant(unit=m)
    cv = g.civilian_at(idx)
    if cv is not None:
        return Combatant(unit=cv)
    return None


def _ctx(g, ours: Combatant, theirs: Combatant, action: str, attacked_tile: Optional[int] = None) -> Ctx:
    """Build the context a unique is evaluated in for this fight, from one side's point of view."""
    if attacked_tile is None:
        attacked_tile = theirs.idx if action == "attack" else ours.idx
    return Ctx(g, civ=ours.owner, city=ours.city, unit=ours.unit, tile=ours.idx, our=ours, their=theirs,
               attacked_tile=attacked_tile, action=action)


def _src(u) -> str:
    """A readable name for whatever granted a modifier, for the breakdown shown to the player."""
    return u.src_name or u.src_type or "Bonus"


# ---------------------------------------------------------------------------------------------------------------
# Strength
# ---------------------------------------------------------------------------------------------------------------
def city_strength(g: "Game", city: City, theirs: Optional[Combatant] = None, action: str = "defend") -> int:
    """A city's combat strength: its base, its population, its garrison, its buildings and its era.

    Technology counts even without buildings, which is why an undefended modern city is still dangerous
    to a classical army.
    """
    k = g.rules.k
    s = k["city_strength_base"] + city.pop * k["city_strength_per_pop"]
    for x in T.terrain_uniques(g, city.idx, U.GrantsCityStrength):
        s += x.n(0)
    n_techs = len(g.rules.techs)
    pct = len(g.player(city.owner).techs) / n_techs if n_techs else 0.5
    s += (pct * k["city_strength_from_techs_multiplier"]) ** k["city_strength_from_techs_exponent"] * \
        k["city_strength_from_techs_full_multiplier"]
    m = g.military_at(city.idx)
    if m is not None:
        s += g.rules.units[m.type]["strength"] * (m.hp / 100) * k["city_strength_from_garrison"]
    bs = sum(g.rules.buildings[b].get("cityStrength", 0) for b in city.buildings)
    ctx = Ctx(g, civ=city.owner, city=city, our=Combatant(city=city), their=theirs, action=action)
    for x in g.civ_uniques(city.owner, U.BetterDefensiveBuildings, ctx):
        bs *= 1 + x.n(0) / 100
    s += bs
    from .cities import city_uniques
    for x in city_uniques(g, city, U.StrengthAmount, ctx):
        s += x.n(0)
    return int(round(s))


def base_attack(g: "Game", a: Combatant, d: Optional[Combatant]) -> float:
    """Attack strength before modifiers. A city attacks at three quarters of its defensive strength."""
    if a.city is not None:
        return round(city_strength(g, a.city, d, "attack") * 0.75)
    ud = a.ud(g)
    from .units import unit_uniques
    extra = sum(x.n(0) for x in unit_uniques(g, a.unit, U.StrengthAmount, ctx=_ctx(g, a, d, "attack") if d else None)) if d else 0
    return (ud["rangedStrength"] if ud["_ranged"] else ud["strength"]) + extra


def base_defense(g: "Game", d: Combatant, a: Optional[Combatant]) -> float:
    """Defence strength before modifiers."""
    if d.city is not None:
        if d.defeated(g):
            return 1
        return city_strength(g, d.city, a, "defend")
    ud = d.ud(g)
    from .movement import is_embarked
    from .units import unit_uniques
    extra = sum(x.n(0) for x in unit_uniques(g, d.unit, U.StrengthAmount, ctx=_ctx(g, d, a, "defend") if a else None)) if a else 0
    if is_embarked(g, d.unit) and ud["_military"]:
        from .research import player_era
        return g.rules.eras[g.rules.era_list[player_era(g, d.owner)]]["embarkDefense"]
    if ud["_ranged"] and a is not None and a.is_ranged(g):
        return ud["rangedStrength"] + extra
    return ud["strength"] + extra


def _great_general_bonus(g, ours: Combatant, enemy: Combatant, action: str) -> tuple[str, int]:
    """The best great-general bonus in range, returned with the name of its source.

    Only the best applies rather than the sum, so stacking generals on one battle does nothing - which
    is the rule that keeps them spread across a front.
    """
    from .units import unit_uniques
    best = ("", 0)
    for gen in g.player_units(ours.owner):
        for x in unit_uniques(g, gen, U.StrengthBonusInRadius,
                              ctx=Ctx(g, civ=ours.owner, our=ours, their=enemy, action=action)):
            radius = int(x.n(2))
            if g.grid.distance(gen.idx, ours.idx) > radius:
                continue
            if x.p(1) != "Military" and not unit_matches(g, ours.unit, x.p(1)):
                continue
            bonus = int(x.n(0))
            if bonus > best[1]:
                best = (gen.type, bonus)
    if best[1] and ours.unit is not None:
        from .units import unit_has
        if unit_has(g, ours.unit, U.GreatGeneralProvidesDoubleCombatBonus, with_civ=True):
            gd = g.rules.units.get(best[0])
            if gd and any(x.p(0) == "War" for x in gd["_umap"].get(U.GreatPerson)):
                best = (best[0], best[1] * 2)
    return best


def _general_modifiers(g, ours: Combatant, enemy: Combatant, action: str, from_tile: int) -> dict:
    """Modifiers that apply the same way whether attacking or defending."""
    mods: dict = {}
    ctx = _ctx(g, ours, enemy, action)
    if ours.unit is not None:
        from .units import unit_uniques
        for x in unit_uniques(g, ours.unit, U.Strength, with_civ=True, ctx=ctx):
            k = _src(x)
            mods[k] = mods.get(k, 0) + int(x.n(0))
        p = g.player(ours.owner)
        cap = g.city(p.capital) if p.capital is not None else None
        for x in unit_uniques(g, ours.unit, U.StrengthNearCapital, with_civ=True, ctx=ctx):
            if cap is None:
                break
            eff = int(x.n(0)) - 3 * g.grid.distance(ours.idx, cap.idx)
            if eff > 0:
                mods[_src(x)] = mods.get(_src(x), 0) + eff
        adj = [o for n in g.grid.neighbors(ours.idx) for o in g.units_at(n)]
        if enemy.unit is not None and enemy.idx not in g.grid.neighbors(ours.idx) and from_tile in g.grid.neighbors(ours.idx):
            adj.append(enemy.unit)
        worst = None
        for o in adj:
            if not g.at_war(o.owner, ours.owner):
                continue
            for x in unit_uniques(g, o, U.StrengthForAdjacentEnemies):
                if ours.matches(g, x.p(1)) and tile_matches(g, ours.idx, x.p(2), ours.owner):
                    if worst is None or x.n(0) < worst:
                        worst = x.n(0)
        if worst is not None:
            mods["Adjacent enemy units"] = int(worst)
        if g.player(ours.owner).kind != "barbarian":
            ud = ours.ud(g)
            res = ud.get("requiredResource")
            if res and g.resource_amount(ours.owner, res) < 0:
                mods["Missing resource"] = MISSING_RESOURCE_MALUS
        name, bonus = _great_general_bonus(g, ours, enemy, action)
        if bonus:
            mods[name] = bonus
    else:
        from .cities import city_uniques
        for x in city_uniques(g, ours.city, U.StrengthForCities, ctx):
            mods[_src(x)] = mods.get(_src(x), 0) + int(x.n(0))
    if g.is_barbarian(enemy.owner):
        from .economy import barbarian_difficulty
        mods["Difficulty"] = int(barbarian_difficulty(g)["barbarianBonus"] * 100)
    return mods


def attack_modifiers(g: "Game", a: Combatant, d: Combatant, from_tile: int) -> dict:
    """Every modifier applying to this attack, keyed by a readable source name.

    A dictionary rather than a number, because the attack preview shows the breakdown - and a player
    who can see "+33% flanking, -25% attacking across a river" learns the rules, where a single final
    number teaches nothing.
    """
    mods = _general_modifiers(g, a, d, "attack", from_tile)
    if a.unit is not None:
        from .movement import is_embarked, river_between, has_connection
        from .units import unit_has, unit_uniques
        u = a.unit
        land_target = T.is_land(g, d.idx)
        across_coast = unit_has(g, u, U.AttackAcrossCoast)
        if is_embarked(g, u) and land_target and not across_coast:
            mods["Landing"] = LANDING_MALUS
        if a.is_land(g) and not T.is_water(g, from_tile) and a.is_melee(g) and T.is_water(g, d.idx) and not across_coast \
                and g.city_at(d.idx) is None:
            mods["Boarding"] = BOARDING_MALUS
        if not a.is_air(g) and a.is_melee(g) and T.is_water(g, from_tile) and not T.is_water(g, d.idx) and \
                not across_coast and not d.is_city():
            mods["Landing"] = LANDING_MALUS
        if a.is_melee(g) and g.grid.distance(from_tile, d.idx) == 1 and river_between(g, from_tile, d.idx) and \
                not unit_has(g, u, U.AttackAcrossRiver) and not (
                has_connection(g, u.owner, from_tile) and has_connection(g, u.owner, d.idx) and
                g.civ_has(u.owner, U.RoadsConnectAcrossRivers)):
            mods["Across river"] = RIVER_MALUS
        if u.activity == "air_sweep":
            for x in unit_uniques(g, u, U.StrengthWhenAirsweep):
                mods[_src(x)] = mods.get(_src(x), 0) + int(x.n(0))
        if a.is_melee(g):
            n = 0
            for nb in g.grid.neighbors(d.idx):
                m = g.military_at(nb)
                if m is not None and m.id != u.id and m.owner == u.owner and g.rules.units[m.type]["_melee"]:
                    n += 1
            if n:
                fb = FLANKING
                for x in unit_uniques(g, u, U.FlankAttackBonus, with_civ=True, ctx=_ctx(g, a, d, "attack")):
                    fb *= 1 + x.n(0) / 100
                mods["Flanking"] = int(fb * n)
    return mods


def tile_defense_bonus(g: "Game", idx: int, unit: Optional[Unit] = None) -> float:
    """The defensive bonus of the terrain a unit is standing on."""
    t = g.s.tiles[idx]
    R = g.rules
    bonus = R.terrains[t.terrain].get("defenceBonus", 0.0)
    if t.features:
        other = max(R.terrains[f].get("defenceBonus", 0.0) for f in t.features)
        if other != 0:
            bonus = other
    if t.wonder:
        bonus += R.terrains[t.wonder].get("defenceBonus", 0.0)
    imp = T.unpillaged_improvement(t)
    if imp:
        ctx = Ctx(g, unit=unit, tile=idx) if unit is not None else Ctx(g, tile=idx)
        for x in R.improvements[imp]["_umap"].matching(U.DefensiveBonus, ctx):
            bonus += x.n(0) / 100
    return bonus


def defense_modifiers(g: "Game", a: Combatant, d: Combatant, from_tile: int) -> dict:
    """Every modifier applying to this defence, keyed by source."""
    mods = _general_modifiers(g, d, a, "defend", from_tile)
    if d.unit is not None:
        from .movement import is_embarked
        from .units import unit_has
        if not is_embarked(g, d.unit):
            tb = tile_defense_bonus(g, d.idx, d.unit)
            no_bonus = unit_has(g, d.unit, U.NoDefensiveTerrainBonus, with_civ=True)
            no_pen = unit_has(g, d.unit, U.NoDefensiveTerrainPenalty, with_civ=True) if hasattr(U, "NoDefensiveTerrainPenalty") else False
            if (not no_bonus and tb > 0) or (not no_pen and tb < 0):
                mods["Tile"] = int(round(tb * 100))
            if d.unit.activity in ("fortify", "fortify_heal") and d.unit.fortify:
                mods["Fortification"] = FORTIFICATION * min(2, d.unit.fortify)
    return mods


def _final(mods: dict) -> float:
    """Collapse a modifier dictionary into one multiplier."""
    return 1 + sum(mods.values()) / 100


def attacking_strength(g, a, d, from_tile) -> float:
    """Final attack strength, never below one."""
    return max(1.0, base_attack(g, a, d) * _final(attack_modifiers(g, a, d, from_tile)))


def defending_strength(g, a, d, from_tile) -> float:
    """Final defence strength, never below one."""
    return max(1.0, base_defense(g, d, a) * _final(defense_modifiers(g, a, d, from_tile)))


def _wounded_ratio(g, c: Combatant) -> float:
    """How much a unit's damage is reduced by its own wounds.

    A unit at half health hits at roughly half strength, unless a promotion says otherwise. This is
    what makes finishing a wounded unit worthwhile and retreating one sensible.
    """
    if c.unit is None:
        return 1.0
    from .units import unit_has
    if unit_has(g, c.unit, U.NoDamagePenaltyWoundedUnits, with_civ=True):
        return 1.0
    return 1 - (100 - c.hp(g)) / WOUNDED_RATIO


def _damage_modifier(ratio: float, to_attacker: bool, rnd: float) -> float:
    """Turn a strength ratio into a damage multiplier.

    The fourth-power curve is UnCiv's, and its effect is that small strength advantages matter little
    and large ones are decisive - which is why a stack of obsolete units is not a substitute for a
    current one.
    """
    stronger = ratio if ratio >= 1 else 1 / ratio
    rm = (((stronger + 3) / 4) ** 4 + 1) / 2
    if (to_attacker and ratio > 1) or (not to_attacker and ratio < 1):
        rm = 1 / rm
    return (24 + 12 * rnd) * rm


def damage_to_defender(g, a, d, from_tile, rnd: float) -> int:
    """Damage the defender takes. Civilians take a fixed amount, since they do not fight."""
    if d.is_civilian(g):
        return DAMAGE_TO_CIVILIAN
    ratio = attacking_strength(g, a, d, from_tile) / defending_strength(g, a, d, from_tile)
    return int(round(_damage_modifier(ratio, False, rnd) * _wounded_ratio(g, a)))


def damage_to_attacker(g, a, d, from_tile, rnd: float) -> int:
    """Damage the attacker takes back - none if it is shooting from a distance."""
    if a.is_ranged(g) and not a.is_air(g):
        return 0
    if d.is_civilian(g):
        return 0
    ratio = attacking_strength(g, a, d, from_tile) / defending_strength(g, a, d, from_tile)
    return int(round(_damage_modifier(ratio, True, rnd) * _wounded_ratio(g, d)))


# ---------------------------------------------------------------------------------------------------------------
# Targeting
# ---------------------------------------------------------------------------------------------------------------
def can_attack_now(g: "Game", u: Unit) -> Optional[str]:
    """Why this unit cannot attack, or None if it can."""
    ud = g.rules.units[u.type]
    from .units import max_attacks
    if not ud["_military"]:
        return "Civilian units cannot attack."
    if u.moves <= 0:
        return f"{u.type} #{u.id} has no movement left this turn."
    if u.attacks >= max_attacks(g, u):
        return f"{u.type} #{u.id} has already attacked this turn."
    return None


def contains_attackable_enemy(g: "Game", idx: int, att: Combatant) -> Optional[str]:
    """Why the enemy on this tile cannot be attacked from here, or None.

    Handles the awkward cases: embarked units that cannot fight at sea, land units that cannot strike
    ships, and the distinction between a tile with an enemy and a tile with an enemy you can reach.
    """
    from .movement import is_embarked, civ_can_embark
    from .units import unit_has, unit_uniques
    if att.unit is not None and is_embarked(g, att.unit) and not unit_has(g, att.unit, U.AttackOnSea):
        if T.is_water(g, idx) or att.is_ranged(g):
            return "Embarked units can only make melee attacks onto land."
    tc = combatant_at(g, idx)
    if tc is None:
        return "There is nothing to attack there."
    if tc.owner == att.owner:
        return "That is your own."
    if not g.at_war(att.owner, tc.owner):
        return f"You are not at war with {g.player(tc.owner).name}. Declare war first."
    if att.unit is not None and att.is_land(g) and att.is_melee(g) and T.is_water(g, idx) and \
            not civ_can_embark(g, att.owner):
        return "Land units cannot attack water tiles yet."
    if att.unit is not None:
        ctx = Ctx(g, civ=att.owner, unit=att.unit, tile=idx, our=att, their=tc, action="attack", attacked_tile=idx)
        if unit_has(g, att.unit, U.CannotAttack) and unit_uniques(g, att.unit, U.CannotAttack, ctx=ctx):
            return f"{att.unit.type} cannot attack."
        only_u = unit_uniques(g, att.unit, U.CanOnlyAttackUnits, ctx=ctx)
        if only_u and not any(tc.matches(g, x.p(0)) for x in only_u):
            return f"{att.unit.type} can only attack {', '.join(x.p(0) for x in only_u)} targets."
        only_t = unit_uniques(g, att.unit, U.CanOnlyAttackTiles, ctx=ctx)
        if only_t and not any(tile_matches(g, idx, x.p(0), att.owner) for x in only_t):
            return f"{att.unit.type} can only attack {', '.join(x.p(0) for x in only_t)} tiles."
    return None


def validate_attack(g: "Game", u: Unit, idx: int) -> Combatant:
    """Check an attack completely and return what would be fought, or raise.

    Shared by :func:`preview` and the attack itself, so a preview cannot say something an attack would
    refuse.
    """
    from . import visibility
    from .units import attack_range, unit_has
    reason = can_attack_now(g, u)
    if reason:
        raise ActionError(reason)
    ud = g.rules.units[u.type]
    if ud["_domain"] == "Air":
        raise ActionError("Aircraft attack with air_strike.")
    if not g.is_barbarian(u.owner) and not visibility.is_visible(g, u.owner, idx):
        raise ActionError("You cannot see that tile.")
    a = Combatant(unit=u)
    reason = contains_attackable_enemy(g, idx, a)
    if reason:
        raise ActionError(reason)
    dist = g.grid.distance(u.idx, idx)
    if ud["_ranged"]:
        rng_ = attack_range(g, u)
        if dist > rng_:
            raise ActionError(f"Target is {dist} tiles away; range is {rng_}.")
        if dist > 1 and not unit_has(g, u, U.IndirectFire, with_civ=True) and not visibility.has_los(g, u.idx, idx):
            raise ActionError("No line of sight to the target (hills, forest, jungle or mountains in the way).")
        if unit_has(g, u, U.MustSetUp) and "Set Up" not in u.status and u.moves < g.rules.move_scale * 1 + 1:
            raise ActionError(f"{u.type} must set up before attacking (needs 1 movement to set up and some left to fire).")
    else:
        if dist != 1:
            raise ActionError("Melee units can only attack adjacent tiles (move next to the target first).")
        if ud["_domain"] == "Water" and T.is_land(g, idx) and g.city_at(idx) is None:
            pass
    d = combatant_at(g, idx)
    if d.city is not None and d.defeated(g) and not a.is_melee(g):
        raise ActionError(f"{d.city.name} has no defenses left: attack with a melee unit to capture it.")
    return d


def preview(g: "Game", u: Unit, idx: int) -> dict:
    """Predict a fight without starting one: strengths, modifiers and the damage range.

    The range comes from evaluating the damage formula at both ends of the random roll, so the numbers
    shown bracket what can actually happen rather than describing an average that may never occur.
    """
    d = validate_attack(g, u, idx)
    a = Combatant(unit=u)
    frm = u.idx
    am = attack_modifiers(g, a, d, frm)
    dm = defense_modifiers(g, a, d, frm)
    lo_d, hi_d = damage_to_defender(g, a, d, frm, 0.0), damage_to_defender(g, a, d, frm, 1.0)
    lo_a, hi_a = damage_to_attacker(g, a, d, frm, 0.0), damage_to_attacker(g, a, d, frm, 1.0)
    res = {"attacker_strength": round(attacking_strength(g, a, d, frm), 1),
           "defender_strength": round(defending_strength(g, a, d, frm), 1),
           "attacker_modifiers": [f"{k} {v:+d}%" for k, v in am.items()],
           "defender_modifiers": [f"{k} {v:+d}%" for k, v in dm.items()],
           "ranged": a.is_ranged(g),
           "damage_to_defender": [lo_d, hi_d], "damage_to_attacker": [lo_a, hi_a],
           "defender": d.name(g), "defender_hp": d.hp(g)}
    if d.city is not None:
        res["target"] = "city"
        if d.defeated(g):
            res["note"] = "City defenses are down: a melee attack will capture it."
    else:
        res["target"] = "unit"
    return res


# ---------------------------------------------------------------------------------------------------------------
# Resolution
# ---------------------------------------------------------------------------------------------------------------
def _rng(g, *keys) -> random.Random:
    """A deterministic generator for one battle, derived from the game's state.

    Keyed by the turn and the participants so that a replay reproduces exactly, rather than drawing
    from a stream whose position depends on every unrelated thing that happened first.
    """
    return g.state_rng("battle", g.turn, *keys, g.rng.random())


def _take_damage(g, a: Combatant, d: Combatant, from_tile: int) -> tuple[int, int]:
    """Battle.takeDamage. Returns (damage dealt to defender, damage dealt to attacker)."""
    r1, r2 = g.rng.random(), g.rng.random()
    pd = damage_to_defender(g, a, d, from_tile, r2)
    pa = damage_to_attacker(g, a, d, from_tile, r1)
    ah, dh = a.hp(g), d.hp(g)
    if d.unit is not None and d.is_civilian(g) and a.is_melee(g):
        from .units import capture_civilian
        capture_civilian(g, a.unit, d.unit)
    elif a.is_ranged(g) and not a.is_air(g):
        d.take_damage(g, pd)
    else:
        rng = g.rng
        while pd + pa > 0:
            if rng.randrange(pd + pa) < pd:
                pd -= 1
                d.take_damage(g, 1)
                if d.defeated(g) and (d.unit is None or d.unit.hp <= 0):
                    break
                if d.city is not None and d.city.health <= 1:
                    break
            else:
                pa -= 1
                a.take_damage(g, 1)
                if a.hp(g) <= 0 if a.unit is not None else a.city.health <= 1:
                    break
    dealt_to_d = dh - d.hp(g)
    dealt_to_a = ah - a.hp(g)
    _plunder_from_damage(g, a, d, dealt_to_d)
    return dealt_to_d, dealt_to_a


def _plunder_from_damage(g, a: Combatant, d: Combatant, dmg: int):
    """Gold taken by units that plunder in proportion to the damage they deal."""
    if a.unit is None or dmg <= 0:
        return
    from .units import unit_uniques
    for x in unit_uniques(g, a.unit, U.DamageUnitsPlunder, with_civ=True):
        if not d.matches(g, x.p(1)):
            continue
        amt = int(x.n(0) / 100 * dmg)
        k = STAT_KEY.get(x.p(2))
        if k and amt:
            g.add_stat(a.owner, k, amt)
            g.emit("plunder", f"{g.player(a.owner).name}'s {a.unit.type} plundered {amt} {x.p(2)} from {d.name(g)}.",
                   [a.owner], idx=d.idx)


def _kill_unit(g, victim: Unit, killer: Optional[int], text: str):
    """Remove a defeated unit and tell both sides."""
    idx, owner = victim.idx, victim.owner
    g.remove_unit(victim)
    g.emit("unit_killed", text, [owner] + ([killer] if killer is not None else []), idx=idx, unit_type=victim.type,
           owner=owner, killer=killer)
    from . import triggers
    triggers.fire(g, owner, U.TriggerUponLosingUnit, filt=lambda m: unit_matches(g, victim, m.p(0)), include_unit=False)
    from . import victory
    victory.check_elimination(g, owner)


def _earn_from_killing(g, killer: Combatant, dead: Combatant):
    """Gold, faith or culture earned for a kill, where a unique grants it."""
    ud = dead.ud(g)
    strength = max(ud["strength"], ud["rangedStrength"])
    cost = ud["cost"]
    ctx = Ctx(g, civ=killer.owner, our=killer, their=dead)
    if killer.unit is not None:
        from .units import unit_uniques
        us = unit_uniques(g, killer.unit, U.KillUnitPlunder, with_civ=True, ctx=ctx)
    else:
        us = g.civ_uniques(killer.owner, U.KillUnitPlunder, ctx)
    for c in g.s.cities.values():
        if g.grid.distance(c.idx, killer.idx) <= 4:
            from .cities import city_uniques
            near = city_uniques(g, c, U.KillUnitPlunderNearCity, ctx)
            if near:
                us = us + near
                break
    for x in us:
        if not dead.matches(g, x.p(1)):
            continue
        src = cost if x.p(2) == "Cost" else strength
        amt = int(src * x.n(0) / 100)
        k = STAT_KEY.get(x.p(3))
        if k and amt:
            g.add_stat(killer.owner, k, amt)
    if g.is_barbarian(dead.owner) and g.player(killer.owner).kind == "major":
        from . import city_states
        city_states.barbarian_killed_near(g, killer.owner, dead.idx)
    from . import city_states
    city_states.on_military_unit_killed(g, killer.owner, dead.owner)


def _heal_after_kill(g, c: Combatant):
    """Healing granted to a unit that has just killed something."""
    if c.unit is None:
        return
    from .units import unit_uniques, heal_by
    for x in unit_uniques(g, c.unit, U.HealsAfterKilling, with_civ=True):
        heal_by(g, c.unit, int(x.n(0)))


def _add_xp(g, c: Combatant, amount: int, other: Combatant):
    """Award experience, noting whether the enemy was a barbarian.

    Guarded by a liveness check: a unit that died in the same exchange cannot be promoted for it.
    """
    if c.unit is None or g.unit(c.unit.id) is None:
        return
    from .units import add_xp
    add_xp(g, c.unit, amount, vs_barbarian=g.is_barbarian(other.owner))


def _withdraw(g, a: Combatant, d: Combatant) -> bool:
    """Try to move a defending unit out of the way instead of letting it die.

    The rule that keeps scouts and similar units alive. The destination must be somewhere the unit
    could legitimately stand, which is what the inner check is for.
    """
    if not (a.unit is not None and a.is_melee(g) and d.unit is not None):
        return False
    from .units import unit_uniques
    from .movement import is_embarked, can_stand
    ctx = Ctx(g, civ=d.owner, our=d, their=a, tile=d.idx, unit=d.unit)
    ws = unit_uniques(g, d.unit, U.WithdrawsBeforeMeleeCombat, ctx=ctx)
    if not ws or is_embarked(g, d.unit):
        return False
    frm, atk = d.idx, a.idx
    ud = d.ud(g)

    def ok(t):
        """Whether the unit could legitimately retreat onto this tile."""
        if not can_stand(g, d.owner, ud, t, d.unit):
            return False
        if d.is_land(g) and not T.is_land(g, t):
            return False
        c = g.city_at(t)
        return c is None or c.owner == d.owner
    first = [t for t in g.grid.neighbors(frm) if t != atk and t not in g.grid.neighbors(atk) and ok(t)]
    second = [t for t in g.grid.neighbors(frm) if t in g.grid.neighbors(atk) and ok(t)]
    rng = g.rng
    to = rng.choice(first) if first else (rng.choice(second) if second else None)
    if to is None:
        return False
    g.place_unit(d.unit, to)
    g.emit("combat", f"{d.unit.type} withdrew from a {a.unit.type}.", [d.owner, a.owner], idx=to)
    return True


def _reduce_attacker_moves(g, a: Combatant, d: Combatant):
    """Spend the attacker's movement or attack, according to what it is."""
    if a.unit is None:
        if a.city is not None:
            a.city.attacked = True
        return
    u = a.unit
    if g.unit(u.id) is None:
        return
    if d.is_civilian(g) and u.idx == d.idx:
        return
    from .units import unit_has, max_attacks
    u.attacks += 1
    u.acted = True
    if unit_has(g, u, U.CanMoveAfterAttacking) or max_attacks(g, u) > u.attacks:
        if not a.is_air(g) and not (a.is_melee(g) and d.defeated(g)):
            u.moves = max(0, u.moves - g.rules.move_scale)
    else:
        u.moves = 0
    if u.activity in ("fortify", "fortify_heal", "sleep", "sleep_heal", "goto", "explore"):
        u.activity = None


def _try_capture_military(g, a: Combatant, d: Combatant) -> bool:
    """Capture a defeated unit instead of destroying it, where a unique allows it."""
    if a.unit is None or d.unit is None:
        return False
    if not d.defeated(g) or d.is_civilian(g):
        return False
    from .units import unit_uniques, place_unit_near
    ctx = Ctx(g, civ=d.owner, unit=d.unit, our=d, their=a, attacked_tile=d.idx)
    if unit_uniques(g, d.unit, U.Uncapturable, ctx=ctx):
        return False
    captured = False
    prize = [x for x in unit_uniques(g, a.unit, U.KillUnitCapture) if d.matches(g, x.p(0))]
    if prize:
        chance = min(0.8, 0.1 + base_attack(g, a, d) / max(1, base_defense(g, d, a)) * 0.4)
        if g.rng.random() <= chance:
            captured = True
    if a.is_melee(g):
        for x in unit_uniques(g, a.unit, U.GainFromDefeatingUnit, with_civ=True,
                              ctx=Ctx(g, civ=a.owner, our=a, their=d)):
            if unit_matches(g, d.unit, x.p(0)):
                g.player(a.owner).gold += int(x.n(1))
                captured = True
    if not captured:
        return False
    nu = place_unit_near(g, a.owner, d.unit.type, d.idx)
    if nu is None:
        return False
    nu.moves = 0
    nu.hp = 50
    g.emit("unit_captured", f"{g.player(a.owner).name} captured an enemy {d.unit.type}!", [a.owner, d.owner], idx=d.idx)
    return True


def attack(g: "Game", u: Unit, idx: int) -> dict:
    """Melee or ranged unit attack (Battle.attack)."""
    from .units import unit_has
    d = validate_attack(g, u, idx)
    a = Combatant(unit=u)
    if unit_has(g, u, U.MustSetUp) and "Set Up" not in u.status:
        u.status.append("Set Up")
        u.moves = max(0, u.moves - g.rules.move_scale)
    return resolve(g, a, d)


def resolve(g: "Game", a: Combatant, d: Combatant) -> dict:
    """Fight one round between two combatants and report everything that happened.

    The centre of the module. In order: damage both ways, plunder, deaths, experience, healing,
    withdrawal, capture, city capture, and the movement the attacker spent. The result is a dictionary
    rather than a state change alone, because the caller - a browser, a model or a bot - needs to be
    told what happened in terms it can act on.
    """
    from . import visibility, triggers
    frm = a.idx
    at_idx = d.idx
    res: dict = {"attacker": a.name(g), "defender": d.name(g), "ranged": a.is_ranged(g)}
    if a.unit is not None and a.is_air(g):
        idmg = try_intercept(g, a, at_idx, d.owner, d)
        if idmg:
            res["intercepted"] = idmg
        if a.unit.hp <= 0:
            _kill_unit(g, a.unit, d.owner, f"{g.player(a.owner).name}'s {a.unit.type} was shot down.")
            res["attacker_killed"] = True
            return res
    if _withdraw(g, a, d):
        _reduce_attacker_moves(g, a, d)
        res["withdrew"] = True
        visibility.refresh(g)
        return res
    already_defeated_city = d.city is not None and d.defeated(g)
    was_civilian = d.is_civilian(g)
    victim_unit = d.unit
    dd, da = _take_damage(g, a, d, frm)
    res.update({"damage_to_defender": dd, "damage_to_attacker": da})
    if d.city is not None:
        res["city_hp"] = d.city.health
    elif victim_unit is not None and g.unit(victim_unit.id) is not None:
        res["defender_hp"] = victim_unit.hp
    captured_military = _try_capture_military(g, a, d)
    ap, dp = g.player(a.owner), g.player(d.owner)
    # deaths
    defender_dead = d.unit is not None and not was_civilian and d.unit.hp <= 0
    attacker_dead = a.unit is not None and a.unit.hp <= 0
    if was_civilian and d.unit is not None:
        res["captured"] = victim_unit.type
    if defender_dead:
        res["defender_killed"] = True
        _kill_unit(g, d.unit, a.owner, f"{ap.name}'s {a.name(g)} destroyed {dp.name}'s {d.unit.type} at {g.fmt_xy(at_idx)}.")
    if attacker_dead:
        res["attacker_killed"] = True
        _kill_unit(g, a.unit, d.owner, f"{ap.name}'s {a.unit.type} died attacking {dp.name}'s {d.name(g)}.")
    if not defender_dead and not attacker_dead and not was_civilian:
        g.emit("combat", f"{ap.name}'s {a.name(g)} attacked {dp.name}'s {d.name(g)} at {g.fmt_xy(at_idx)} "
                         f"(-{dd} HP" + (f", took -{da})" if da else ")"), [a.owner, d.owner], idx=at_idx)
    if d.unit is None and d.city is not None and g.is_barbarian(d.owner) is False and \
            g.s.tiles[at_idx].improvement == "Barbarian encampment":
        pass
    # city defeated?
    city_msg = _handle_city_defeated(g, a, d)
    if city_msg:
        res.update(city_msg)
    # post-kill uniques
    if defender_dead:
        _earn_from_killing(g, a, d)
        _heal_after_kill(g, a)
        if a.unit is not None and g.unit(a.unit.id) is not None:
            triggers.fire(g, a.owner, U.TriggerUponDefeatingUnit, filt=lambda m: unit_matches(g, victim_unit, m.p(0)),
                          unit=a.unit)
    elif attacker_dead and d.unit is not None:
        _earn_from_killing(g, d, a)
        _heal_after_kill(g, d)
    if a.unit is not None and g.unit(a.unit.id) is not None:
        from .units import unit_has
        if unit_has(g, a.unit, U.SelfDestructs):
            g.remove_unit(a.unit)
        elif a.unit.activity == "goto":
            a.unit.activity = None
            a.unit.goto = None
    # melee advances into the emptied tile
    if not captured_military and a.unit is not None and g.unit(a.unit.id) is not None and a.is_melee(g) and \
            (defender_dead or res.get("captured_city")) and not res.get("captured_city"):
        from .movement import can_stand, on_enter_tile
        ud = a.ud(g)
        if g.city_at(at_idx) is None and can_stand(g, a.owner, ud, at_idx, a.unit) and a.unit.moves > 0:
            civ = g.civilian_at(at_idx)
            if civ is not None and civ.owner != a.owner:
                from .units import capture_civilian
                capture_civilian(g, a.unit, civ)
            g.place_unit(a.unit, at_idx)
            on_enter_tile(g, a.unit, at_idx)
            res["advanced"] = True
    _reduce_attacker_moves(g, a, d)
    if not already_defeated_city and not was_civilian:
        if a.is_air(g):
            _add_xp(g, a, 4, d)
            _add_xp(g, d, 2, a)
        elif a.is_ranged(g):
            _add_xp(g, a, 3 if d.is_city() else 2, d)
            _add_xp(g, d, 2, a)
        else:
            _add_xp(g, a, 5, d)
            _add_xp(g, d, 4, a)
    if g.is_barbarian(d.owner) and g.s.tiles[at_idx].improvement == "Barbarian encampment":
        from . import barbarians
        barbarians.camp_attacked(g, at_idx)
    visibility.refresh(g)
    return res


def _handle_city_defeated(g, a: Combatant, d: Combatant) -> Optional[dict]:
    """Capture a city brought to the brink, if the attacker is a melee unit that may."""
    if d.city is None or not d.defeated(g) or a.unit is None or not a.is_melee(g):
        return None
    city = d.city
    if g.is_barbarian(a.owner):
        # barbarians never capture or raze: they sack the city and leave it to its owner
        from . import barbarians
        return barbarians.sack_city(g, city, a.unit)
    from .units import unit_has
    if unit_has(g, a.unit, U.CannotCaptureCities, with_civ=True):
        return None
    from . import conquest
    return conquest.conquer(g, city, a.unit)


# ---------------------------------------------------------------------------------------------------------------
# Cities
# ---------------------------------------------------------------------------------------------------------------
def can_bombard(g: "Game", city: City) -> Optional[str]:
    """Why this city cannot bombard, or None if it can. Once per turn, and never in resistance."""
    if city.attacked:
        return f"{city.name} has already attacked this turn."
    if city.resistance > 0:
        return f"{city.name} is in resistance."
    return None


def bombard_targets(g: "Game", city: City) -> list[int]:
    """Every enemy this city could shoot at right now."""
    from . import visibility
    rng = g.rules.k["base_city_bombard_range"]
    a = Combatant(city=city)
    out = []
    for i in g.grid.within(city.idx, rng):
        if i == city.idx or not visibility.is_visible(g, city.owner, i):
            continue
        if contains_attackable_enemy(g, i, a) is None:
            out.append(i)
    return out


def city_bombard(g: "Game", city: City, idx: int) -> dict:
    """Fire a city's attack at a tile."""
    from . import visibility
    reason = can_bombard(g, city)
    if reason:
        raise ActionError(reason)
    if g.grid.distance(city.idx, idx) > g.rules.k["base_city_bombard_range"]:
        raise ActionError(f"Target is out of the city's range ({g.rules.k['base_city_bombard_range']}).")
    if not visibility.is_visible(g, city.owner, idx):
        raise ActionError("You cannot see that tile.")
    a = Combatant(city=city)
    reason = contains_attackable_enemy(g, idx, a)
    if reason:
        raise ActionError(reason)
    d = combatant_at(g, idx)
    return resolve(g, a, d)


# ---------------------------------------------------------------------------------------------------------------
# Air
# ---------------------------------------------------------------------------------------------------------------
def intercept_chance(g, u: Unit) -> int:
    """The percentage chance this unit intercepts an air attack."""
    from .units import unit_uniques
    return sum(int(x.n(0)) for x in unit_uniques(g, u, U.ChanceInterceptAirAttacks))


def can_intercept(g, u: Unit, tile: int) -> bool:
    """Whether this unit is in a position to intercept over the given tile."""
    ud = g.rules.units[u.type]
    if intercept_chance(g, u) == 0:
        return False
    if ud["_domain"] == "Air" and u.moves <= 0:
        return False
    from .units import unit_uniques
    maxi = 1 + sum(int(x.n(0)) for x in unit_uniques(g, u, U.ExtraInterceptionsPerTurn))
    if u.interceptions >= maxi:
        return False
    rng = ud.get("interceptRange", 0) + sum(int(x.n(0)) for x in unit_uniques(g, u, U.AirInterceptionRange, with_civ=True)) \
        if hasattr(U, "AirInterceptionRange") else ud.get("interceptRange", 0)
    return g.grid.distance(u.idx, tile) <= rng


def try_intercept(g: "Game", a: Combatant, tile: int, civ: int, d: Optional[Combatant]) -> int:
    """Give defending interceptors a chance at an incoming air attack, returning the damage done."""
    from .units import unit_uniques, unit_has
    if unit_has(g, a.unit, U.CannotBeIntercepted):
        return 0
    cands = [u for u in g.player_units(civ) if can_intercept(g, u, tile) and (d is None or d.unit is None or u.id != d.unit.id)]
    if not cands:
        return 0
    cands.sort(key=lambda u: -intercept_chance(g, u))
    icpt = cands[0]
    icpt.interceptions += 1
    if g.rng.random() > intercept_chance(g, icpt) / 100:
        return 0
    ic = Combatant(unit=icpt)
    dmg = damage_to_defender(g, ic, a, icpt.idx, g.rng.random())
    factor = 1 + sum(x.n(0) for x in unit_uniques(g, icpt, U.DamageWhenIntercepting)) / 100
    for x in unit_uniques(g, a.unit, U.DamageFromInterceptionReduced):
        factor *= 1 - x.n(0) / 100
    dmg = min(int(dmg * factor), a.unit.hp)
    a.unit.hp -= dmg
    if dmg > 0:
        _add_xp(g, ic, 2, a)
    g.emit("combat", f"{g.player(icpt.owner).name}'s {icpt.type} intercepted {g.player(a.owner).name}'s "
                     f"{a.unit.type} (-{dmg} HP).", [icpt.owner, a.owner], idx=tile)
    return dmg


def air_strike(g: "Game", u: Unit, idx: int) -> dict:
    """An aircraft attacks a tile, after any interception."""
    from . import visibility
    from .units import attack_range
    ud = g.rules.units[u.type]
    if ud["_domain"] != "Air":
        raise ActionError("Only aircraft make air strikes.")
    if ud["_umap"].get(U.NuclearWeapon):
        return nuke(g, u, idx)
    reason = can_attack_now(g, u)
    if reason:
        raise ActionError(reason)
    if g.grid.distance(u.idx, idx) > attack_range(g, u):
        raise ActionError(f"Target out of range ({attack_range(g, u)}).")
    if not visibility.is_visible(g, u.owner, idx):
        raise ActionError("You cannot see that tile.")
    a = Combatant(unit=u)
    reason = contains_attackable_enemy(g, idx, a)
    if reason:
        raise ActionError(reason)
    d = combatant_at(g, idx)
    return resolve(g, a, d)


def air_sweep(g: "Game", u: Unit, idx: int) -> dict:
    """A fighter clears interceptors over a tile, so that bombers can follow safely."""
    from .units import unit_has, attack_range, max_attacks
    if not unit_has(g, u, U.CanAirsweep):
        raise ActionError("This unit cannot perform air sweeps.")
    reason = can_attack_now(g, u)
    if reason:
        raise ActionError(reason)
    if g.grid.distance(u.idx, idx) > attack_range(g, u):
        raise ActionError(f"Target out of range ({attack_range(g, u)}).")
    u.attacks += 1
    if not (unit_has(g, u, U.CanMoveAfterAttacking) or max_attacks(g, u) > u.attacks):
        u.moves = 0
    enemies = [q.id for q in g.s.players if g.at_war(u.owner, q.id)]
    cands = [x for x in g.s.units.values() if x.owner in enemies and can_intercept(g, x, idx)]
    if any(g.rules.units[x.type]["_domain"] == "Air" for x in cands):
        cands = [x for x in cands if g.rules.units[x.type]["_domain"] == "Air"]
    if not cands:
        return {"air_sweep": "Nothing tried to intercept."}
    g.rng.shuffle(cands)
    cands.sort(key=lambda x: -intercept_chance(g, x))
    icpt = cands[0]
    icpt.interceptions += 1
    if g.rules.units[icpt.type]["_domain"] != "Air":
        return {"air_sweep": f"Ground interceptor {icpt.type} fired and missed; it is spent for this turn."}
    u.activity = "air_sweep"
    a, d = Combatant(unit=u), Combatant(unit=icpt)
    dd, da = _take_damage(g, a, d, u.idx)
    u.activity = None
    _add_xp(g, d, 5, a)
    _add_xp(g, a, 5, d)
    res = {"air_sweep": icpt.type, "damage_to_interceptor": dd, "damage_to_attacker": da}
    if icpt.hp <= 0:
        _kill_unit(g, icpt, u.owner, f"{g.player(u.owner).name}'s {u.type} shot down an intercepting {icpt.type}.")
        res["interceptor_killed"] = True
    if u.hp <= 0:
        _kill_unit(g, u, icpt.owner, f"{g.player(icpt.owner).name}'s {icpt.type} shot down a sweeping {u.type}.")
        res["attacker_killed"] = True
    return res


def rebase(g: "Game", u: Unit, idx: int) -> dict:
    """Move an aircraft to another city or carrier."""
    from .movement import stack_reason
    from .units import attack_range
    ud = g.rules.units[u.type]
    if ud["_domain"] != "Air":
        raise ActionError("Only aircraft can rebase.")
    if u.moves <= 0 or u.attacks:
        raise ActionError("This aircraft has already acted this turn.")
    if g.grid.distance(u.idx, idx) > attack_range(g, u) * 2:
        raise ActionError(f"Rebase range is {attack_range(g, u) * 2} tiles.")
    reason = stack_reason(g, u.owner, ud, idx, u.id)
    if reason:
        raise ActionError(reason)
    carrier = None
    if g.city_at(idx) is None:
        from .movement import _can_carry
        carrier = next((o for o in g.units_at(idx) if o.owner == u.owner and _can_carry(g, o, ud, u.id)), None)
    g.place_unit(u, idx)
    u.carried_by = carrier.id if carrier else None
    u.moves = 0
    u.acted = True
    return {"rebased_to": g.xy(idx), "carrier": carrier.type if carrier else None}


# ---------------------------------------------------------------------------------------------------------------
# Nukes
# ---------------------------------------------------------------------------------------------------------------
def nuke(g: "Game", u: Unit, idx: int) -> dict:
    """Detonate a nuclear weapon: damage in a radius, destroyed improvements, fallout, and the diplomatic consequences.
    """
    from .units import attack_range, unit_uniques
    from . import diplomacy, visibility
    if not g.nukes_enabled:
        raise ActionError("Nuclear weapons are disabled in this game.")
    ns = unit_uniques(g, u, U.NuclearWeapon)
    if not ns:
        raise ActionError("This unit is not a nuclear weapon.")
    if u.idx == idx:
        raise ActionError("A nuke cannot target its own tile.")
    if not g.player(u.owner).explored[idx]:
        raise ActionError("Target an explored tile.")
    if g.grid.distance(u.idx, idx) > attack_range(g, u):
        raise ActionError(f"Target out of range ({attack_range(g, u)}).")
    reason = can_attack_now(g, u)
    if reason:
        raise ActionError(reason)
    strength = int(ns[0].n(0))
    br = unit_uniques(g, u, U.BlastRadius)
    radius = int(br[0].n(0)) if br else 2
    hit = g.grid.within(idx, radius)
    pid = u.owner
    for civ in {g.s.tiles[i].owner for i in hit} | {o.owner for i in hit for o in g.units_at(i)}:
        if civ is None or civ == pid or g.is_barbarian(civ):
            continue
        if g.has_met(pid, civ) and not g.at_war(pid, civ):
            rel = g.relation(pid, civ)
            if rel and rel.get("treaty_until", 0) >= g.turn:
                raise ActionError(f"You have a peace treaty with {g.player(civ).name} and cannot attack them yet.")
    declared = []
    for civ in sorted({g.s.tiles[i].owner for i in hit if g.s.tiles[i].owner is not None} |
                      {o.owner for i in hit for o in g.units_at(i)}):
        if civ == pid or g.is_barbarian(civ):
            continue
        if g.has_met(pid, civ) and not g.at_war(pid, civ):
            diplomacy.declare_war(g, pid, civ, via_nuke=True)
            declared.append(civ)
    a = Combatant(unit=u)
    if g.rules.units[u.type]["_domain"] == "Air":
        for civ in sorted({o.owner for i in hit for o in g.units_at(i) if o.owner != pid}):
            try_intercept(g, a, idx, civ, None)
            if u.hp <= 0:
                _kill_unit(g, u, civ, f"{g.player(pid).name}'s {u.type} was shot down.")
                return {"nuke": "intercepted", "declared_war_on": [g.player(c).name for c in declared]}
    lacking = 1.0
    res_ = g.rules.units[u.type].get("requiredResource")
    if res_ and g.resource_amount(pid, res_) < 0:
        lacking = 0.5
    for i in hit:
        _nuke_tile(g, u, i, strength, i == idx, lacking)
    g.emit("nuke", f"{g.player(pid).name} detonated a {u.type} at {g.fmt_xy(idx)}!", None, idx=idx, player=pid)
    if unit_uniques(g, u, U.SelfDestructs) and g.unit(u.id) is not None:
        g.remove_unit(u)
    elif g.unit(u.id) is not None:
        u.attacks += 1
        u.moves = 0
    for q in g.majors():
        if q.id != pid and g.has_met(q.id, pid):
            diplomacy.add_opinion(g, q.id, pid, "used_nukes", -50)
    visibility.refresh(g)
    return {"nuke": u.type, "target": g.xy(idx), "declared_war_on": [g.player(c).name for c in declared]}


def _nuke_tile(g, u, idx, strength, ground_zero, lacking):
    """Apply a nuclear strike to one tile: units, population, buildings and the ground itself.

    Ground zero is treated differently from the surrounding ring, and a city is destroyed outright only
    when the strike would take it below one population - a nuke usually cripples a city rather than
    erasing it.
    """
    from .cities import city_uniques, add_population, destroy_city
    t = g.s.tiles[idx]
    rng = g.rng
    building_mod = 1.0
    c = g.city_at(idx)
    if c is not None:
        for x in city_uniques(g, c, U.GarrisonDamageFromNukes):
            if city_matches(g, c, x.p(1)):
                building_mod *= 1 + x.n(0) / 100
        from . import conquest
        if (strength > 2 or (strength > 1 and c.pop < 5)) and conquest.can_be_destroyed(g, c, just_captured=True):
            destroy_city(g, c)
        else:
            c.health = max(1, c.health - int(c.health * 0.5 * lacking))
            pop_mod = 1.0
            for x in city_uniques(g, c, U.PopulationLossFromNukes):
                if city_matches(g, c, x.p(1)):
                    pop_mod *= 1 + x.n(0) / 100
            frac = {0: 0.0, 1: (30 + rng.randrange(20) + rng.randrange(20)) / 100,
                    2: (60 + rng.randrange(10) + rng.randrange(10)) / 100}.get(strength, 1.0)
            loss = int(c.pop * pop_mod * frac)
            if loss:
                add_population(g, c, -loss)
    for o in list(g.units_at(idx)):
        if ground_zero or strength >= 2:
            dmg = 100
        elif strength == 1:
            dmg = 30 + rng.randrange(40) + rng.randrange(40)
        else:
            dmg = 20 + rng.randrange(30)
        dmg = int(dmg * building_mod * lacking + 1e-6)
        if not g.rules.units[o.type]["_military"]:
            if o.hp - dmg <= 40:
                g.remove_unit(o)
        else:
            o.hp -= dmg
            if o.hp <= 0:
                _kill_unit(g, o, u.owner, f"A nuclear blast destroyed {g.player(o.owner).name}'s {o.type}.")
    if g.city_at(idx) is not None:
        return
    destroyable = [f for f in t.features if g.rules.terrains[f]["_umap"].get(U.DestroyableByNukesChance)]
    if destroyable:
        for f in destroyable:
            ch = g.rules.terrains[f]["_umap"].get(U.DestroyableByNukesChance)[0].n(0) / 100
            if (ch > 0 and ground_zero) or rng.random() < ch:
                t.features.remove(f)
                _pillage_and_fallout(g, t, idx)
    elif ground_zero or rng.random() < 0.5:
        _pillage_and_fallout(g, t, idx)
    g.clear_static()


def _pillage_and_fallout(g, t, idx):
    """Wreck a tile's improvement and leave fallout, unless the improvement cannot be removed."""
    imp = T.unpillaged_improvement(t)
    if imp and not g.rules.improvements[imp]["_umap"].has_tag(U.Irremovable):
        if g.rules.improvements[imp]["_umap"].has_tag(U.Unpillagable):
            t.improvement = None
        else:
            t.pillaged = True
    if T.unpillaged_route(t):
        t.route_pillaged = True
    if T.is_water(g, idx) or T.is_impassable(g, idx) or "Fallout" in t.features:
        return
    t.features.append("Fallout")
