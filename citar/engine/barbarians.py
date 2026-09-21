"""Barbarians: encampments that appear in the fog of war, spawn units on a countdown, and a raiding AI.
Port of UnCiv's BarbarianManager, BarbarianEncampment and BarbarianAutomation (MPL-2.0).

Encampments live in g.s.camps: id -> {"idx", "countdown", "spawned", "destroyed"}; the tile carries the
"Barbarian encampment" improvement. Destroyed camps linger for 15 turns to block new camps nearby.
"""
from __future__ import annotations

from typing import Optional, TYPE_CHECKING

from . import unique_types as U
from .game import ActionError

if TYPE_CHECKING:
    from .game import Game

CAMP = "Barbarian encampment"


def level(g: "Game") -> Optional[dict]:
    """The barbarian setting for this game, or None when they are off."""
    lv = g.s.config.get("barbarians") or "off"
    return g.rules.const["barbarians"]["levels"].get(lv)


def raging(g: "Game") -> bool:
    """Whether barbarians are set to raging, which spawns them faster and further."""
    lv = level(g)
    return bool(lv and lv.get("raging"))


def _camp_at(g, idx) -> Optional[int]:
    """The camp on a tile, if there is one."""
    return next((cid for cid, c in g.s.camps.items() if c["idx"] == idx and not c.get("destroyed")), None)


# ----------------------------------------------------------------------------
# Encampment placement
# ----------------------------------------------------------------------------
def _viewable_by_anyone(g: "Game") -> set:
    """Every tile any civilization can currently see.

    Camps are never placed where somebody is looking - a camp appearing in front of a player's eyes
    looks like a bug rather than like barbarians.
    """
    from .visibility import visible_tiles
    out = set()
    for p in g.s.players:
        if p.kind != "barbarian" and p.alive:
            out |= visible_tiles(g, p.id)
    return out


def place_camps(g: "Game", first: bool = False):
    """BarbarianManager.placeBarbarianEncampment."""
    from . import tiles as T
    rng = g.state_rng("barb_place", g.turn, len(g.s.camps))
    if not first and rng.random() < 0.5:
        return
    viewable = _viewable_by_anyone(g)
    fog = [i for i in range(g.grid.size) if T.is_land(g, i) and i not in viewable]
    per_camp = int(g.grid.size ** 0.4)
    to_add = len(fog) // max(1, per_camp) - sum(1 for c in g.s.camps.values() if not c.get("destroyed"))
    if first:
        to_add = max(to_add // 3, 1)
    elif to_add > 0:
        to_add = 1
    if to_add <= 0:
        return
    near_caps = set()
    for p in g.s.players:
        if p.kind == "major" and p.alive and p.capital is not None and g.city(p.capital):
            near_caps.update(g.grid.within(g.city(p.capital).idx, 4))
    near_camps = set()
    for c in g.s.camps.values():
        near_camps.update(g.grid.within(c["idx"], 4 if c.get("destroyed") else 7))
    R = g.rules
    viable = []
    for i in fog:
        t = g.s.tiles[i]
        if T.is_impassable(g, i) or t.resource or t.improvement or g.city_at(i) is not None or t.owner is not None:
            continue
        if g.units_at(i):
            continue
        if any(R.terrains[f]["_umap"].has_tag(U.RestrictedBuildableImprovements) for f in t.features):
            continue
        if not any(T.is_land(g, n) for n in g.grid.neighbors(i)):
            continue
        if i in near_caps or i in near_camps:
            continue
        viable.append(i)
    added = 0
    bias_coast = rng.randrange(6) == 0
    while added < to_add and viable:
        coast = [i for i in viable if T.adjacent_to_coast(g, i)] if bias_coast else []
        idx = rng.choice(coast or viable)
        create_camp(g, idx)
        for q in g.majors():
            if q.explored[idx] and g.civ_has(q.id, U.NotifiedOfBarbarianEncampments):
                g.emit("camp_spawned", "A new barbarian encampment has spawned!", [q.id], idx=idx)
        added += 1
        blocked = set(g.grid.within(idx, 7))
        viable = [i for i in viable if i not in blocked]
        bias_coast = rng.randrange(6) == 0


def create_camp(g: "Game", idx: int) -> int:
    """Put a barbarian camp on a tile."""
    cid = g.new_id()
    g.s.camps[cid] = {"idx": idx, "countdown": 0, "spawned": -1, "destroyed": False}
    t = g.s.tiles[idx]
    t.improvement = CAMP
    t.pillaged = False
    g.invalidate()
    return cid


def place_initial_camps(g: "Game"):
    """Scatter the starting camps, away from civilizations and out of sight."""
    if level(g) is None:
        return
    _update_barbarian_techs(g)
    place_camps(g, first=True)


# ----------------------------------------------------------------------------
# Spawning
# ----------------------------------------------------------------------------
def _update_barbarian_techs(g: "Game"):
    """Keep barbarian technology roughly level with the world's.

    Without it, barbarians stay in the ancient era and become scenery. With it, an ignored camp is a
    threat in every era.
    """
    bid = g.barbarian_id
    if bid is None:
        return
    common = None
    for p in g.s.players:
        if p.kind == "barbarian" or not p.alive:
            continue
        common = set(p.techs) if common is None else common & set(p.techs)
    techs = sorted(common or [])
    bp = g.player(bid)
    if bp.techs != techs:
        bp.techs = techs
        g.invalidate()


def force_evaluation(g: "Game", name: str) -> int:
    """BaseUnit.getForceEvaluation."""
    ud = g.rules.units[name]
    st, rs = ud.get("strength", 0), ud.get("rangedStrength", 0)
    if st == 0 and rs == 0:
        return 0
    power = st ** 1.5
    rp = rs ** 1.45
    if ud["_domain"] == "Water":
        rp /= 2
    if rp > 0:
        power = rp
    power *= ud.get("movement", 2) ** 0.3
    um = ud["_umap"]
    if um.has_tag(U.SelfDestructs):
        power /= 2
    if um.has_tag(U.NuclearWeapon):
        power += 4000
    best = 1.0
    for x in um.get(U.Strength):
        v = x.n(0)
        if v <= 0:
            continue
        if any(m.ph == "vs [] units" for m in x.mods):
            best = 1 + v / 4 / 100
        elif any(m.ph in ("vs cities", "when attacking", "when defending", "when fighting in [] tiles") for m in x.mods):
            best = 1 + v / 2 / 100
        else:
            best = 1 + v / 100
    power *= best
    return int(power)


def _barbarian_unit_options(g: "Game", naval: bool) -> list[str]:
    """The units barbarians could spawn now, land or naval."""
    bid = g.barbarian_id
    out = []
    for name, ud in g.rules.units.items():
        um = ud["_umap"]
        if not ud["_military"] or um.has_tag(U.CannotAttack) or um.has_tag(U.CannotBeBarbarian):
            continue
        if ud["_domain"] != ("Water" if naval else "Land"):
            continue
        if ud.get("uniqueTo") or um.has_tag(U.Unbuildable) or ud["_great_person"] or um.has_tag(U.NuclearWeapon):
            continue
        if not g.has_tech(bid, ud.get("requiredTech")):
            continue
        if ud.get("obsoleteTech") and g.has_tech(bid, ud["obsoleteTech"]):
            continue
        if any(x.ph == U.OnlyAvailable for x in um.all):
            continue
        out.append(name)
    return out


def _choose_unit(g: "Game", naval: bool) -> Optional[str]:
    """Pick a unit for a camp to spawn."""
    opts = _barbarian_unit_options(g, naval)
    if not opts:
        return None
    weights = [max(1, force_evaluation(g, n)) for n in opts]
    return g.state_rng("barb_unit", g.turn, len(g.s.units)).choices(opts, weights=weights)[0]


def spawn_barbarian(g: "Game", idx: int, allegiance: Optional[int] = None):
    """BarbarianManager.spawnBarbarian."""
    from . import tiles as T
    bid = g.barbarian_id
    owner = bid if allegiance is None else allegiance
    if owner is None:
        return None
    if owner == bid:
        if g.military_at(idx) is None:
            return _spawn(g, idx, False, owner)
        if g.turn < 10:
            return None
        near = sum(1 for i in g.grid.within(idx, 4) if (m := g.military_at(i)) is not None and m.owner == bid)
        if near > 2:
            return None
    can_naval = g.turn > 30
    valid = [n for n in g.grid.neighbors(idx) if not (T.is_impassable(g, n) or g.city_at(n) is not None or g.units_at(n)
             or (T.is_water(g, n) and not can_naval) or (T.is_water(g, n) and T.fresh_water(g, n)))]
    if not valid:
        return None
    rng = g.state_rng("barb_spawn", g.turn, idx)
    return _spawn(g, idx, T.is_water(g, rng.choice(valid)), owner)


def _spawn(g, idx, naval, owner):
    """Create a barbarian unit near a camp."""
    from .units import place_unit_near
    _update_barbarian_techs(g)
    utype = _choose_unit(g, naval)
    if utype is None:
        return None
    return place_unit_near(g, owner, utype, idx)


def _reset_countdown(g: "Game", camp: dict):
    """Set how long until this camp spawns again."""
    rng = g.state_rng("barb_countdown", g.turn, camp["idx"])
    cd = 8 + rng.randrange(5)
    if raging(g):
        cd //= 2
    from .economy import barbarian_difficulty
    cd += barbarian_difficulty(g).get("barbarianSpawnDelay", 0)
    cd -= min(camp["spawned"], 3)
    camp["countdown"] = int(cd * g.speed["barbarianModifier"])


def update_camps(g: "Game"):
    """BarbarianManager.updateEncampments."""
    for cid, camp in list(g.s.camps.items()):
        if g.s.tiles[camp["idx"]].improvement != CAMP and not camp.get("destroyed"):
            camp["destroyed"] = True
            camp["countdown"] = 15
        if camp.get("destroyed") and camp["countdown"] == 0:
            del g.s.camps[cid]
    place_camps(g)
    for camp in list(g.s.camps.values()):
        if camp["countdown"] > 0:
            camp["countdown"] -= 1
        elif not camp.get("destroyed") and spawn_barbarian(g, camp["idx"]) is not None:
            camp["spawned"] += 1
            _reset_countdown(g, camp)


def camp_attacked(g: "Game", idx: int):
    """Handle a camp being attacked, which may destroy it."""
    cid = _camp_at(g, idx)
    if cid is not None:
        g.s.camps[cid]["countdown"] //= 2


def remove_camp(g: "Game", idx: int):
    """The encampment improvement was removed by something other than a unit clearing it (e.g. a city claimed it)."""
    from . import city_states
    t = g.s.tiles[idx]
    if t.improvement == CAMP:
        t.improvement = None
    cid = _camp_at(g, idx)
    if cid is not None:
        g.s.camps[cid]["destroyed"] = True
        g.s.camps[cid]["countdown"] = 15
    city_states.camp_removed(g, idx)
    g.invalidate()


def clear_camp(g: "Game", idx: int, pid: int, unit=None) -> int:
    """MapUnit.clearEncampment."""
    from . import city_states
    from .economy import difficulty
    city_states.camp_cleared(g, idx, pid)
    t = g.s.tiles[idx]
    t.improvement = None
    cid = _camp_at(g, idx)
    if cid is not None:
        g.s.camps[cid]["destroyed"] = True
        g.s.camps[cid]["countdown"] = 15
    gold = float(difficulty(g, pid).get("clearBarbarianCampReward", 25))
    for x in g.civ_uniques(pid, U.GainFromEncampment):
        gold += x.n(0)
        u = spawn_barbarian(g, idx, allegiance=pid)
        if u is not None:
            u.hp = 100
            u.moves = 0
            g.emit("camp_recruit", f"An enemy {u.type} has joined us!", [pid], idx=u.idx, unit=u.id)
    gold *= g.speed["goldCostModifier"]
    for x in g.civ_uniques(pid, U.GoldFromEncampmentsAndCities):
        gold *= 1 + x.n(0) / 100
    gold = int(gold)
    g.player(pid).gold += gold
    g.invalidate()
    g.emit("camp_cleared", f"{g.player(pid).name} destroyed a barbarian encampment and looted {gold} gold.",
           [pid], idx=idx, gold=gold)
    return gold


# ----------------------------------------------------------------------------
# Turn & automation (BarbarianAutomation)
# ----------------------------------------------------------------------------
def take_turn(g: "Game"):
    """Play the barbarians' turn: spawn from camps, then move every unit."""
    bid = g.barbarian_id
    if bid is None or level(g) is None:
        return
    units = g.player_units(bid)
    rd = g.rules.units
    order = ([u for u in units if rd[u.type]["_ranged"]] + [u for u in units if rd[u.type]["_melee"]] +
             [u for u in units if not rd[u.type]["_ranged"] and not rd[u.type]["_melee"]])
    for u in order:
        if g.unit(u.id) is None:
            continue
        try:
            _automate(g, u)
        except ActionError:
            u.moves = 0
    update_camps(g)


def _automate(g, u):
    """Give one barbarian unit its orders: attack, pillage, or wander."""
    ud = g.rules.units[u.type]
    if not ud["_military"]:
        return _automate_civilian(g, u)
    if g.s.tiles[u.idx].improvement == CAMP:
        if _try_attack(g, u, stay=True):
            return
        u.activity = "fortify"
        return
    if u.hp < 50 and _try_pillage(g, u, only_here=True) and u.moves <= 0:
        return
    if _try_attack(g, u):
        return
    for _ in range(3):
        if not _try_pillage(g, u):
            break
        if u.moves <= 0:
            return
    _wander(g, u)


def _automate_civilian(g, u):
    """Move a captured civilian back toward a camp."""
    from .movement import find_path, move_toward
    if g.s.tiles[u.idx].improvement == CAMP:
        return
    camps = sorted((c["idx"] for c in g.s.camps.values() if not c.get("destroyed")),
                   key=lambda i: g.grid.distance(u.idx, i))
    for i in camps:
        if g.civilian_at(i) is None and find_path(g, u, i) is not None:
            move_toward(g, u, i, set_goto=False)
            return
    _wander(g, u)


def _attack_targets(g, u) -> list[tuple[float, int, int]]:
    """(score, target tile, tile to attack from). Melee units may move next to a target first."""
    from . import combat
    from .movement import reachable_this_turn, can_stand
    ud = g.rules.units[u.type]
    if combat.can_attack_now(g, u) is not None:
        return []
    ranged = ud["_ranged"]
    from .units import attack_range
    rng_ = attack_range(g, u) if ranged else 1
    reach = reachable_this_turn(g, u)
    reach.setdefault(u.idx, u.moves)
    out = []
    seen = set()
    for frm, left in reach.items():
        if left <= 0 and frm != u.idx:
            continue
        if frm != u.idx and not can_stand(g, u.owner, ud, frm, u):
            continue
        for idx in g.grid.within(frm, rng_):
            if idx == frm or (idx, frm) in seen:
                continue
            seen.add((idx, frm))
            att = combat.Combatant(unit=u)
            if combat.contains_attackable_enemy(g, idx, att) is not None:
                continue
            old = u.idx
            try:
                u.idx = frm
                pv = combat.preview(g, u, idx)
            except ActionError:
                continue
            finally:
                u.idx = old
            dd = sum(pv["damage_to_defender"]) / 2
            da = sum(pv["damage_to_attacker"]) / 2
            if pv["target"] == "city":
                score = dd - da * 1.5 + (1000 if pv.get("note") and not ranged else 0)
            else:
                kills = dd >= pv["defender_hp"]
                score = dd * (2 if kills else 1) - da * 1.2
            out.append((score, idx, frm))
    return out


def _try_attack(g, u, stay: bool = False) -> bool:
    """Attack something adjacent if it is worth attacking."""
    from . import combat
    from .movement import move_toward
    targets = _attack_targets(g, u)
    if stay:
        targets = [t for t in targets if t[2] == u.idx]
    if not targets:
        return False
    score, idx, frm = max(targets)
    if score <= 0 and not g.rules.units[u.type]["_ranged"]:
        return False
    if frm != u.idx:
        move_toward(g, u, frm, set_goto=False)
        if u.idx != frm:
            return False
    try:
        combat.attack(g, u, idx)
        return True
    except ActionError:
        return False


def _try_pillage(g, u, only_here: bool = False) -> bool:
    """Pillage an improvement here or nearby."""
    from . import workers
    from .movement import reachable_this_turn, move_toward
    ud = g.rules.units[u.type]
    if ud["_domain"] != "Land":
        return False

    def pillageable(i):
        """Whether a tile has something worth pillaging."""
        t = g.s.tiles[i]
        if t.owner is None or t.owner == u.owner or g.city_at(i) is not None:
            return False
        return (t.improvement and not t.pillaged and t.improvement != CAMP) or (t.route and not t.route_pillaged)
    tiles = [u.idx] if only_here else [i for i, left in reachable_this_turn(g, u).items() if left > 0] + [u.idx]
    tiles = [i for i in tiles if pillageable(i) and (i == u.idx or g.military_at(i) is None)]
    if not tiles:
        return False
    best = min(tiles, key=lambda i: g.grid.distance(u.idx, i))
    if best != u.idx:
        move_toward(g, u, best, set_goto=False)
        if u.idx != best:
            return False
    try:
        workers.pillage(g, u)
        return True
    except ActionError:
        return False


def _wander(g, u):
    """UnitAutomation.wander: move to a random reachable tile."""
    from .movement import reachable_this_turn, move_toward, can_stand
    ud = g.rules.units[u.type]
    reach = [i for i, left in reachable_this_turn(g, u).items() if i != u.idx and can_stand(g, u.owner, ud, i, u)]
    if not reach:
        return
    rng = g.state_rng("wander", u.id, g.turn)
    target = rng.choice(reach)
    move_toward(g, u, target, set_goto=False)
