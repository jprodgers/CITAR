"""Units: uniques, promotions & XP, healing, upgrades, ability uses, placement, capture, turn processing.
Port of UnCiv's MapUnit, UnitPromotions, UnitUpgradeManager, UnitTurnManager, UnitManager placement and
UnitActionModifiers (MPL-2.0)."""
from __future__ import annotations

from typing import Optional, TYPE_CHECKING

from . import unique_types as U
from .game import ActionError
from .state import Unit
from .uniques import Ctx, Unique, UniqueMap, applies, unit_matches, base_unit_matches
from . import tiles as T

if TYPE_CHECKING:
    from .game import Game


# ---------------------------------------------------------------------------------------------------------------
# Uniques
# ---------------------------------------------------------------------------------------------------------------
def unit_umap(g: "Game", u: Unit) -> UniqueMap:
    """The uniques applying to one unit: its type plus its promotions, cached by both."""
    key = ("uumap", u.type, tuple(sorted(u.promotions)))
    m = g._static.get(key)
    if m is None:
        R = g.rules
        us = list(R.units[u.type]["_umap"].all)
        for pr in u.promotions:
            if pr in R.promotions:
                us.extend(R.promotions[pr]["_umap"].all)
        m = UniqueMap(us)
        g._static[key] = m
    return m


def unit_ctx(g: "Game", u: Unit) -> Ctx:
    """The context a unique is evaluated against for this unit."""
    return Ctx(g, civ=u.owner, unit=u, tile=u.idx)


def unit_uniques(g: "Game", u: Unit, ph: str, with_civ: bool = False, ctx: Optional[Ctx] = None) -> list[Unique]:
    """Uniques of a placeholder applying to this unit, optionally including civilization-wide ones."""
    if ctx is None:
        ctx = unit_ctx(g, u)
    out = unit_umap(g, u).matching(ph, ctx)
    if with_civ:
        out = out + g.civ_uniques(u.owner, ph, ctx)
    return out


def unit_has(g: "Game", u: Unit, ph: str, with_civ: bool = False) -> bool:
    """Whether any such unique applies."""
    return bool(unit_uniques(g, u, ph, with_civ))


def can_garrison(g: "Game", u: Unit) -> bool:
    """Whether this unit counts as a garrison, which affects city strength and happiness."""
    ud = g.rules.units[u.type]
    return ud["_military"] and ud["_domain"] == "Land"


def is_military(g: "Game", u: Unit) -> bool:
    """Whether this is a military unit."""
    return g.rules.units[u.type]["_military"]


# ---------------------------------------------------------------------------------------------------------------
# Creation & placement
# ---------------------------------------------------------------------------------------------------------------
def on_created(g: "Game", u: Unit):
    """Base promotions and civ-granted promotions (TileMap.placeUnitNearTile)."""
    R = g.rules
    u.original_owner = u.owner if u.original_owner is None else u.original_owner
    for pr in R.units[u.type].get("promotions", []):
        add_promotion(g, u, pr, free=True)
    if g.player(u.owner).kind != "barbarian":
        for x in g.civ_uniques(u.owner, U.UnitsGainPromotion, Ctx(g, civ=u.owner, unit=u, tile=u.idx)):
            if unit_matches(g, u, x.p(0)) and x.p(1) in R.promotions:
                add_promotion(g, u, x.p(1), free=True)
    elif R.nations["Barbarians"]:
        for x in R.nations["Barbarians"]["_umap"].get(U.UnitsGainPromotion):
            if unit_matches(g, u, x.p(0)) and x.p(1) in R.promotions:
                add_promotion(g, u, x.p(1), free=True)


def place_unit_near(g: "Game", pid: int, utype: str, idx: int, max_tries: int = 10) -> Optional[Unit]:
    """Place a new unit on or near a tile (TileMap.placeUnitNearTile)."""
    from .movement import can_stand, can_pass_through
    ud = g.rules.units[utype]
    if can_stand(g, pid, ud, idx):
        return g.create_unit(pid, utype, idx)
    checked = {idx}
    frontier = [n for n in g.grid.neighbors(idx) if can_pass_through(g, pid, ud, n)]
    for _ in range(max_tries):
        restricted = ud["_domain"] == "Land"
        prim = [t for t in frontier if not restricted or T.is_land(g, t)]
        sec = [t for t in frontier if restricted and not T.is_land(g, t)]
        for t in prim + sec:
            if can_stand(g, pid, ud, t):
                return g.create_unit(pid, utype, t)
        checked.update(frontier)
        nxt = []
        for t in frontier:
            for n in g.grid.neighbors(t):
                if n not in checked and n not in nxt and can_pass_through(g, pid, ud, n):
                    nxt.append(n)
        frontier = nxt
        if not frontier:
            break
    return None


def add_unit_in_city(g: "Game", city, utype: str) -> Optional[Unit]:
    """UnitManager.addUnit: water units go to a naval city; place on/near the city tile."""
    pid = city.owner
    ud = g.rules.units[utype]
    target = city
    if ud["_domain"] == "Water" and not (T.is_water(g, city.idx) or T.adjacent_to_coast(g, city.idx)):
        naval = [c for c in g.player_cities(pid) if T.adjacent_to_coast(g, c.idx)]
        if not naval:
            return None
        target = naval[0]
    u = place_unit_near(g, pid, utype, target.idx)
    if u is None:
        return None
    u.origin_city = target.id
    if ud["_umap"].has_tag(U.ReligiousUnit) and g.religion_enabled:
        from . import religion
        p = g.player(pid)
        if not ud["_umap"].has_tag(U.TakeReligionOverBirthCity) or not religion.is_major(g, p.religion):
            u.religion = religion.majority_religion(g, target)
        else:
            u.religion = p.religion
    from . import triggers
    triggers.fire(g, pid, U.TriggerUponGainingUnit, filt=lambda x: base_unit_matches(g.rules, utype, x.p(0)), unit=u)
    return u


def add_construction_bonuses(g: "Game", u: Unit, city):
    """Apply the experience and promotions a city grants to units it builds."""
    from .cities import city_uniques
    from .uniques import city_matches
    R = g.rules
    xp = 0
    for x in city_uniques(g, city, U.UnitStartingExperience):
        if unit_matches(g, u, x.p(0)) and city_matches(g, city, x.p(2)):
            xp += int(x.n(1))
    u.xp = xp
    for x in city_uniques(g, city, U.UnitStartingPromotions):
        if not city_matches(g, city, x.p(1)):
            continue
        pr = x.params[-1]
        relevant = x.p(0) == "relevant" and pr in R.promotions and R.units[u.type]["unitType"] in R.promotions[pr]["unitTypes"]
        if relevant or unit_matches(g, u, x.p(0)):
            add_promotion(g, u, pr, free=True)


def starting_units(g: "Game", pid: int, era: str) -> list[str]:
    """The units a civilization begins with, for the era the game starts in."""
    from .cities import equivalent_unit
    R = g.rules
    e = R.eras[era]
    p = g.player(pid)
    settler = next((n for n, u in R.units.items() if u["_umap"].get(U.FoundCity) and not u.get("uniqueTo")), "Settler")
    out = [settler] * e.get("startingSettlerCount", 1) + ["Worker"] * e.get("startingWorkerCount", 0) + \
          ["Era Starting Unit"] * e.get("startingMilitaryUnitCount", 1)
    gd = g.seat_difficulty(pid)
    if p.kind == "major":
        out += gd.get("playerBonusStartingUnits" if g.is_humanlike(pid) else "aiMajorCivBonusStartingUnits", [])
    else:
        out += gd.get("aiCityStateBonusStartingUnits", [])
    if p.kind == "city_state":
        out = [settler]
    res = []
    for n in out:
        if n == "Era Starting Unit":
            n = e.get("startingMilitaryUnit", "Warrior")
        if n in R.units:
            res.append(equivalent_unit(g, pid, n))
    return res


# ---------------------------------------------------------------------------------------------------------------
# Promotions & XP
# ---------------------------------------------------------------------------------------------------------------
def xp_for_next(g: "Game", u: Unit) -> int:
    """Experience needed for this unit's next promotion."""
    mod = 1.0
    for x in g.civ_uniques(u.owner, U.XPForPromotionModifier):
        mod *= 1 + x.n(0) / 100
    return int((u.promotion_count + 1) * 10 * mod)


def available_promotions(g: "Game", u: Unit) -> list[str]:
    """Promotions this unit could take now, given its type and what it already has."""
    R = g.rules
    ut = R.units[u.type]["unitType"]
    ctx = unit_ctx(g, u)
    out = []
    for n, pr in R.promotions.items():
        if n in u.promotions or ut not in pr["unitTypes"]:
            continue
        if pr["prerequisites"] and not any(p in u.promotions for p in pr["prerequisites"]):
            continue
        if pr["_umap"].has(U.Unavailable, ctx):
            continue
        if any(not applies(x, ctx) for x in pr["_umap"].get(U.OnlyAvailable)):
            continue
        out.append(n)
    return out


def can_promote(g: "Game", u: Unit) -> bool:
    """Whether the unit has enough experience and something to spend it on."""
    av = available_promotions(g, u)
    if not av:
        return False
    if u.xp >= xp_for_next(g, u) or u.pending_promotions > 0:
        return True
    return any(g.rules.promotions[p]["_umap"].has_tag(U.FreePromotion) for p in av)


def add_promotion(g: "Game", u: Unit, name: str, free: bool = False):
    """Give a unit a promotion and apply everything that follows from it."""
    R = g.rules
    pr = R.promotions.get(name)
    if pr is None or name in u.promotions:
        return
    if not free:
        if not pr["_umap"].has_tag(U.FreePromotion):
            if u.pending_promotions > 0:
                u.pending_promotions -= 1
            else:
                u.xp -= xp_for_next(g, u)
                u.promotion_count += 1
        from . import triggers
        triggers.fire(g, u.owner, U.TriggerUponPromotion, unit=u)
    if not pr["_umap"].has_tag(U.SkipPromotion):
        u.promotions.append(name)
    # direct effects (heal etc.)
    ctx = unit_ctx(g, u)
    from . import triggers
    for x in pr["_umap"].all:
        if triggers.is_triggerable(x) and not triggers.has_trigger_conditional(x) and applies(x, ctx):
            triggers.trigger(g, x, u.owner, unit=u)
    g.invalidate()


def promote(g: "Game", u: Unit, name: str) -> dict:
    """Spend experience on a promotion, resolving the name first."""
    n = g.rules.resolve("promotion", name)
    av = available_promotions(g, u)
    if n not in av:
        raise ActionError(f"{name} is not available for this unit. Available: {', '.join(av) or 'none'}")
    if not can_promote(g, u):
        raise ActionError(f"Not enough XP ({u.xp}/{xp_for_next(g, u)}).")
    add_promotion(g, u, n)
    return {"promoted": n, "xp": u.xp, "next_at": xp_for_next(g, u)}


def add_xp(g: "Game", u: Unit, amount: int, vs_barbarian: bool = False, city_state_enemy: bool = False):
    """Battle.addXp: combat XP, capped vs barbarians, with XP modifiers; feeds great general points."""
    if amount <= 0:
        return
    R = g.rules
    cap = R.k["max_xp_from_barbarians"]
    mod = 1.0
    for x in unit_uniques(g, u, U.PercentageXPGain, with_civ=True):
        mod += x.n(0) / 100
    gain = int(amount * mod)
    if vs_barbarian:
        total_prior = u.xp + u.promotion_count * (u.promotion_count + 1) * 5
        gain = max(0, min(gain, cap - total_prior))
    if gain <= 0:
        return
    before = can_promote(g, u)
    u.xp += gain
    from . import great_people
    if g.player(u.owner).kind == "major" and not vs_barbarian:
        great_people.add_combat_points(g, u.owner, u.type, int(amount * mod))
    if not before and can_promote(g, u):
        g.emit("promotion_ready", f"{u.type} #{u.id} can be promoted.", [u.owner], idx=u.idx, unit=u.id)


# ---------------------------------------------------------------------------------------------------------------
# Movement points, sight, range
# ---------------------------------------------------------------------------------------------------------------
def max_movement(g: "Game", u: Unit) -> int:
    """Whole movement points (MapUnit.getMaxMovement)."""
    from .movement import is_embarked
    ud = g.rules.units[u.type]
    mv = 2 if is_embarked(g, u) else ud["movement"]
    mv += sum(int(x.n(0)) for x in unit_uniques(g, u, U.Movement, with_civ=True))
    mv = max(1, mv)
    for other in g.units_at(u.idx):
        if other.id == u.id:
            continue
        if any(unit_matches(g, u, x.p(0)) for x in unit_uniques(g, other, U.TransferMovement)):
            base = g.rules.units[other.type]["movement"] + sum(int(x.n(0)) for x in unit_uniques(g, other, U.Movement, with_civ=True))
            mv = max(mv, base)
    return mv


def sight(g: "Game", u: Unit) -> int:
    """How far this unit can see, including terrain and promotions."""
    r = 2
    ctx = unit_ctx(g, u)
    r += sum(int(x.n(0)) for x in unit_uniques(g, u, U.Sight, with_civ=True, ctx=ctx))
    r += sum(int(x.n(0)) for x in T.terrain_uniques(g, u.idx, U.Sight, ctx))
    return max(1, r)


def attack_range(g: "Game", u: Unit) -> int:
    """How far this unit can attack."""
    ud = g.rules.units[u.type]
    if ud["_melee"]:
        return 1
    return ud["range"] + sum(int(x.n(0)) for x in unit_uniques(g, u, U.Range, with_civ=True))


def max_attacks(g: "Game", u: Unit) -> int:
    """How many times this unit may attack in one turn."""
    return 1 + sum(int(x.n(0)) for x in unit_uniques(g, u, U.AdditionalAttacks, with_civ=True))


# ---------------------------------------------------------------------------------------------------------------
# Healing & damage
# ---------------------------------------------------------------------------------------------------------------
def heal_for_tile(g: "Game", u: Unit, idx: int) -> int:
    """How much this unit would heal on a given tile."""
    ud = g.rules.units[u.type]
    friendly = T.is_friendly_territory(g, idx, u.owner)
    water = T.is_water(g, idx)
    if g.city_at(idx) is not None:
        h = 25
    elif water and friendly and (ud["_domain"] == "Water" or u.carried_by is not None):
        h = 20
    elif water:
        h = 0
    elif friendly:
        h = 20
    else:
        h = 10
    may = h > 0 or (water and unit_has(g, u, U.HealsOutsideFriendlyTerritory, with_civ=True))
    if not may:
        return h
    h += sum(int(x.n(0)) for x in unit_uniques(g, u, U.Heal, with_civ=True, ctx=Ctx(g, civ=u.owner, unit=u, tile=idx)))
    for n in g.grid.within(idx, 1):
        c = g.city_at(n)
        if c is None:
            continue
        from .cities import city_uniques
        for x in city_uniques(g, c, U.CityHealingUnits):
            if unit_matches(g, u, x.p(0)) and (c.owner == u.owner):
                h += int(x.n(1))
        break
    best = 0
    for n in g.grid.neighbors(idx) + [idx]:
        for o in g.units_at(n):
            if o.owner == u.owner and o.id != u.id:
                best = max(best, sum(int(x.n(0)) for x in unit_uniques(g, o, U.HealAdjacentUnits)))
    h += best
    return h


def heal_amount_here(g: "Game", u: Unit) -> int:
    """How much it will heal where it is standing."""
    from .movement import is_embarked
    if is_embarked(g, u) or u.hp >= 100:
        return 0
    if unit_has(g, u, U.HealOnlyByPillaging, with_civ=True) or \
            (g.is_barbarian(u.owner) and g.rules.nations["Barbarians"]["_umap"].has_tag(U.HealOnlyByPillaging)):
        return 0
    return heal_for_tile(g, u, u.idx)


def heal_by(g: "Game", u: Unit, amount: int):
    """Heal a unit, applying any doubling it has."""
    if unit_has(g, u, U.HealingEffectsDoubled, with_civ=True):
        amount *= 2
    u.hp = min(100, u.hp + amount)


def take_damage(g: "Game", u: Unit, amount: int, note: Optional[str] = None) -> bool:
    """Returns True if the unit died."""
    u.hp = max(0, min(100, u.hp - amount))
    if u.hp <= 0:
        g.remove_unit(u)
        if note:
            g.emit("unit_killed", note, [u.owner], idx=u.idx)
        return True
    return False


def terrain_damage(g: "Game", idx: int) -> int:
    """Damage the terrain itself does to units standing on it."""
    return sum(int(x.n(0)) for x in T.terrain_uniques(g, idx, U.DamagesContainingUnits))


# ---------------------------------------------------------------------------------------------------------------
# Unit action uniques (UnitActionModifiers)
# ---------------------------------------------------------------------------------------------------------------
def _usages_left(g, u: Unit, x: Unique) -> Optional[int]:
    """How many uses of a limited ability this unit has left."""
    total = None
    for m in x.mods:
        if m.ph == "[] times":
            total = int(m.n(0))
        elif m.ph == "once":
            total = 1
    if total is None:
        return None
    for extra in unit_umap(g, u).get(x.ph):
        for m in extra.mods:
            if m.ph == "[] additional time(s)":
                total += int(m.n(0))
    key = x.ph + "|" + "|".join(x.params)
    return total - u.abilities_used.get(key, 0)


def usable_action(g: "Game", u: Unit, ph: str) -> Optional[Unique]:
    """The unique for an action this unit can still perform, or None."""
    ctx = unit_ctx(g, u)
    for x in unit_umap(g, u).get(ph):
        if x.has_mod("[] additional time(s)"):
            continue
        if not applies(x, ctx):
            continue
        left = _usages_left(g, u, x)
        if left is not None and left <= 0:
            continue
        need = 1
        if u.moves < need * 1 and not x.has_mod("by consuming this unit"):
            if u.moves <= 0:
                continue
        return x
    return None


def usable_action_uniques(g: "Game", u: Unit, ph: str) -> list[Unique]:
    """Every usable action unique of this kind (UnitActionModifiers.getUsableUnitActionUniques)."""
    ctx = unit_ctx(g, u)
    out = []
    for x in unit_umap(g, u).get(ph):
        if x.has_mod("[] additional time(s)") or not applies(x, ctx):
            continue
        left = _usages_left(g, u, x)
        if left is not None and left <= 0:
            continue
        out.append(x)
    return out


def consume_action(g: "Game", u: Unit, x: Unique):
    """Spend one use of a limited ability."""
    cost = None
    for m in x.mods:
        if m.ph == "for [] movement":
            cost = int(m.n(0))
    sc = g.rules.move_scale
    u.moves = max(0, u.moves - (cost if cost is not None else 1) * sc)
    for m in x.mods:
        if m.ph == "by consuming this unit":
            consume(g, u)
            return
        if m.ph in ("[] times", "once"):
            left = _usages_left(g, u, x)
            if left == 1 and x.has_mod("after which this unit is consumed"):
                consume(g, u)
                return
            key = x.ph + "|" + "|".join(x.params)
            u.abilities_used[key] = u.abilities_used.get(key, 0) + 1
    if not x.mods or all(m.ph not in ("[] times", "once", "by consuming this unit") for m in x.mods):
        pass


def consume(g: "Game", u: Unit):
    """Destroy a unit that has been used up - a settler founding a city, a prophet spending itself."""
    from . import triggers
    triggers.fire(g, u.owner, U.TriggerUponExpendingUnit, filt=lambda m: unit_matches(g, u, m.p(0)), include_unit=False,
                  note=f"due to expending our {u.type}")
    g.remove_unit(u)


# ---------------------------------------------------------------------------------------------------------------
# Upgrades
# ---------------------------------------------------------------------------------------------------------------
def upgrade_targets(g: "Game", u: Unit, special: bool = False) -> list[str]:
    """What this unit could upgrade into."""
    from .cities import equivalent_unit
    R = g.rules
    ud = R.units[u.type]
    ctx = unit_ctx(g, u)
    if special:
        sp = ud["_umap"].matching(U.RuinsUpgrade, ctx)
        if sp:
            return [equivalent_unit(g, u.owner, sp[0].p(0))]
    out = [x.p(0) for x in ud["_umap"].matching(U.CanUpgrade, ctx)]
    if ud.get("upgradesTo"):
        out.append(ud["upgradesTo"])
    return [equivalent_unit(g, u.owner, n) for n in out if n in R.units]


def upgrade_cost(g: "Game", u: Unit, target: str) -> int:
    """Gold to upgrade this unit, from the difference in production cost."""
    R = g.rules
    c = R.k["unit_upgrade_cost"]
    civ_mod = 1.0
    for x in g.civ_uniques(u.owner, U.UnitUpgradeCost, unit_ctx(g, u)):
        civ_mod *= 1 + x.n(0) / 100
    cost = c["base"] + max(0.0, c["per_production"] * (R.units[target]["cost"] - R.units[u.type]["cost"]))
    cost *= 1 + R.units[target]["_era"] * c["era_multiplier"]
    cost = (cost * civ_mod) ** c["exponent"]
    cost *= g.speed["modifier"]
    return int(cost / c["round_to"]) * c["round_to"]


def _upgrade_blockers(g, u: Unit, target: str, ignore_requirements: bool, ignore_resources: bool) -> list[str]:
    """Rejection reasons of the target unit type for the civ (UnitUpgradeManager.canUpgrade)."""
    R = g.rules
    td = R.units[target]
    p = g.player(u.owner)
    out = []
    if not ignore_requirements and td.get("requiredTech") and not g.has_tech(u.owner, td["requiredTech"]):
        out.append(f"{target} requires {td['requiredTech']}.")
    if td.get("uniqueTo") and td["uniqueTo"] != p.nation:
        out.append(f"{target} is unique to {td['uniqueTo']}.")
    if not ignore_resources and td.get("requiredResource"):
        have = g.resource_amount(u.owner, td["requiredResource"])
        own = 1 if R.units[u.type].get("requiredResource") == td["requiredResource"] else 0
        if have + own < 1:
            out.append(f"{target} needs {td['requiredResource']}.")
    if not g.nukes_enabled and td["_umap"].get(U.NuclearWeapon):
        out.append("Nuclear weapons are disabled.")
    return out


def check_upgrade(g: "Game", u: Unit) -> tuple[Optional[str], Optional[str], int]:
    """(target, reason_if_not_possible, cost)."""
    targets = upgrade_targets(g, u)
    if not targets:
        return None, f"{u.type} has no upgrade.", 0
    last_reason = None
    for t in targets:
        blockers = _upgrade_blockers(g, u, t, False, False)
        cost = upgrade_cost(g, u, t)
        if blockers:
            last_reason = blockers[0]
            continue
        from .movement import is_embarked
        if g.s.tiles[u.idx].owner != u.owner:
            return t, "Units can only upgrade inside your own territory.", cost
        if is_embarked(g, u):
            return t, "Embarked units cannot upgrade.", cost
        if u.moves <= 0:
            return t, "The unit has no movement left.", cost
        if g.player(u.owner).gold < cost:
            return t, f"Upgrading to {t} costs {cost} gold; you have {int(g.player(u.owner).gold)}.", cost
        return t, None, cost
    return targets[0], last_reason, upgrade_cost(g, u, targets[0])


def _perform_upgrade(g: "Game", u: Unit, target: str) -> Optional[Unit]:
    """Replace a unit with its upgraded version, keeping what carries over."""
    idx, owner = u.idx, u.owner
    stats = (u.hp, list(u.promotions), u.xp, u.promotion_count, u.name, u.religion, u.abilities_used, u.original_owner)
    g.remove_unit(u)
    nu = place_unit_near(g, owner, target, idx)
    if nu is None:
        back = place_unit_near(g, owner, u.type, idx)
        if back is not None:
            back.hp, back.promotions, back.xp, back.promotion_count = stats[0], stats[1], stats[2], stats[3]
        return None
    nu.hp, nu.xp, nu.promotion_count, nu.name, nu.religion = stats[0], stats[2], stats[3], stats[4], stats[5]
    for pr in stats[1]:
        if pr not in nu.promotions:
            nu.promotions.append(pr)
    nu.original_owner = stats[7]
    nu.moves = 0
    g.invalidate()
    return nu


def upgrade(g: "Game", u: Unit) -> dict:
    """Upgrade a unit for gold, after checking it may."""
    target, reason, cost = check_upgrade(g, u)
    if reason:
        raise ActionError(reason)
    old = u.type
    nu = _perform_upgrade(g, u, target)
    if nu is None:
        raise ActionError("The upgraded unit could not be placed.")
    g.player(nu.owner).gold -= cost
    return {"upgraded": old, "to": target, "unit_id": nu.id, "gold_spent": cost}


def free_upgrade(g: "Game", u: Unit, special: bool = False) -> bool:
    """Upgrade a unit without charge, where a unique grants it."""
    for t in upgrade_targets(g, u, special):
        if not _upgrade_blockers(g, u, t, True, True):
            return _perform_upgrade(g, u, t) is not None
    return False


# ---------------------------------------------------------------------------------------------------------------
# Capture
# ---------------------------------------------------------------------------------------------------------------
def capture_civilian(g: "Game", captor: Unit, victim: Unit):
    """Battle.captureCivilianUnit: settlers become workers; great people/religious units are destroyed."""
    R = g.rules
    vd = R.units[victim.type]
    old = victim.owner
    if vd["_umap"].has_tag(U.Uncapturable) or vd["_great_person"] or vd["_umap"].has_tag(U.ReligiousUnit):
        g.remove_unit(victim)
        g.emit("unit_killed", f"{g.player(captor.owner).name} destroyed {g.player(old).name}'s {victim.type}.",
               [old, captor.owner], idx=victim.idx)
        return
    if g.is_barbarian(captor.owner) or vd["_umap"].get(U.FoundCity):
        # barbarians keep settlers as workers; any civ capturing a settler gets a worker
        pass
    new_type = victim.type
    if vd["_umap"].get(U.FoundCity):
        new_type = "Worker" if "Worker" in R.units else victim.type
    idx = victim.idx
    g.remove_unit(victim)
    if g.is_barbarian(captor.owner):
        nu = g.create_unit(captor.owner, new_type, idx)
    else:
        from .cities import equivalent_unit
        nu = g.create_unit(captor.owner, equivalent_unit(g, captor.owner, new_type) if new_type != victim.type else new_type, idx)
    nu.moves = 0
    nu.original_owner = victim.original_owner
    g.emit("unit_captured", f"{g.player(captor.owner).name} captured {g.player(old).name}'s {victim.type}!",
           [old, captor.owner], idx=idx)


# ---------------------------------------------------------------------------------------------------------------
# Turn processing
# ---------------------------------------------------------------------------------------------------------------
def start_turn(g: "Game", u: Unit):
    """Reset a unit's movement and attacks at the start of its owner's turn."""
    from .movement import max_moves
    u.moves = max_moves(g, u)
    u.attacks = 0
    u.interceptions = 0
    u.acted = False
    if u.activity == "sleep":
        from .visibility import visible_tiles
        vis = visible_tiles(g, u.owner)
        for n in g.grid.within(u.idx, 3):
            m = g.military_at(n)
            if m and n in vis and g.at_war(u.owner, m.owner):
                u.activity = None
                g.emit("unit_woke", f"{u.type} #{u.id} woke up: enemy nearby.", [u.owner], idx=u.idx, unit=u.id)
                break
    if u.activity in ("heal", "fortify_heal", "sleep_heal") and u.hp >= 100:
        u.activity = None
        g.emit("unit_woke", f"{u.type} #{u.id} has fully healed.", [u.owner], idx=u.idx, unit=u.id)
    owner = g.s.tiles[u.idx].owner
    if owner is not None and owner != u.owner and not g.can_enter_territory(u.owner, u.idx) and \
            not unit_has(g, u, U.CanEnterForeignTiles) and g.player(owner).kind != "city_state":
        from .movement import teleport_to_closest
        teleport_to_closest(g, u)


def end_turn(g: "Game", u: Unit):
    """UnitTurnManager.endTurn: fortify counter, healing, religious strength loss, citadel & terrain damage."""
    from .movement import max_moves
    moved = u.moves < max_moves(g, u) or u.acted
    if not moved and u.activity in ("fortify", "fortify_heal") and u.fortify < 2:
        u.fortify += 1
    if u.activity not in ("fortify", "fortify_heal"):
        u.fortify = 0
    if (not moved and u.attacks == 0) or unit_has(g, u, U.HealsEvenAfterAction):
        amt = heal_amount_here(g, u)
        if amt > 0:
            heal_by(g, u, amt)
    ud = g.rules.units[u.type]
    t = g.s.tiles[u.idx]
    if ud["_umap"].has_tag(U.ReligiousUnit) and t.owner is not None and g.player(t.owner).kind != "city_state" \
            and not g.can_enter_territory(u.owner, u.idx):
        lost = [int(x.n(0)) for x in unit_uniques(g, u, U.CanEnterForeignTilesButLosesReligiousStrength)]
        if lost:
            u.religious_strength_lost += min(lost)
        if u.religious_strength_lost >= ud.get("religiousStrength", 0) > 0:
            g.emit("unit_killed", f"Your {u.type} lost its faith after spending too long in foreign territory.",
                   [u.owner], idx=u.idx)
            g.remove_unit(u)
            return
    # citadel damage
    best, dmg = None, 0
    for n in g.grid.neighbors(u.idx):
        nt = g.s.tiles[n]
        imp = T.unpillaged_improvement(nt)
        if nt.owner is None or imp is None or not g.at_war(u.owner, nt.owner):
            continue
        d = sum(int(x.n(0)) for x in g.rules.improvements[imp]["_umap"].get(U.DamagesAdjacentEnemyUnits))
        if d > dmg:
            best, dmg = n, d
    if dmg:
        if take_damage(g, u, dmg):
            g.emit("unit_killed", f"An enemy {g.s.tiles[best].improvement} destroyed {g.player(u.owner).name}'s {u.type}.",
                   [u.owner, g.s.tiles[best].owner], idx=u.idx)
            return
    td = terrain_damage(g, u.idx)
    if td and not unit_has(g, u, U.CanPassImpassable):
        if take_damage(g, u, td):
            g.emit("unit_killed", f"{g.player(u.owner).name}'s {u.type} took {td} terrain damage and was destroyed.",
                   [u.owner], idx=u.idx)
            return
    for st in list(u.status):
        pass


def air_capacity_ok(g: "Game", city, pid: int, ignore_uid: Optional[int] = None) -> bool:
    """City.getMaxAirUnits: 6 + carry-extra uniques, counting uncarried aircraft only."""
    from .cities import city_uniques
    cap = g.rules.k["city_air_unit_capacity"]
    cap += sum(int(x.n(0)) for x in city_uniques(g, city, U.CarryExtraAirUnits) if x.p(1) == "Air")
    here = [a for a in g.air_units_at(city.idx) if a.carried_by is None and a.id != ignore_uid]
    return len(here) < cap


def disband_gold(g: "Game", u: Unit) -> int:
    """Gold refunded for disbanding a unit inside your own borders."""
    from .cities import base_gold_cost
    return int(base_gold_cost(g, u.owner, u.type, None)) // 20


def disband(g: "Game", u: Unit) -> dict:
    """MapUnit.disband: units disbanded inside their own territory refund some gold."""
    gold = disband_gold(g, u) if g.s.tiles[u.idx].owner == u.owner else 0
    g.player(u.owner).gold += gold
    for c in [x for x in g.units_at(u.idx) if x.carried_by == u.id]:
        g.remove_unit(c)
    g.remove_unit(u)
    return {"disbanded": u.type, "gold": gold}
