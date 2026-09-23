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


def aggression(g: "Game") -> int:
    """How aggressive barbarians are, 0-100.

    The game config's ``barbarian_aggression`` when set, otherwise the level's default. It drives how far
    barbarians look for targets, what odds they accept, whether they would rather pillage or fight, and how
    fast and how many units their camps put out. At 0 they mostly raid what is next to them; at 100 they
    hunt everything in reach.
    """
    lv = level(g)
    if lv is None:
        return 0
    v = g.s.config.get("barbarian_aggression")
    if v is None:
        v = lv.get("aggression", 50)
    try:
        v = int(v)
    except (TypeError, ValueError):
        v = 50
    return max(0, min(100, v))


def _aggr(g) -> float:
    """Aggression as a fraction, 0.0-1.0."""
    return aggression(g) / 100


def sack_cooldown(g: "Game") -> int:
    """Turns a sacked city is left alone before barbarians will sack it again: 10 at aggression 0, 5 at 100."""
    return int(round(10 - 5 * _aggr(g)))


def recently_sacked(g: "Game", city) -> bool:
    """Whether barbarians sacked this city too recently to bother with it again yet."""
    return g.turn - city.sacked_turn < sack_cooldown(g)


def seek_radius(g: "Game") -> int:
    """How far (in tiles) barbarians look for cities, units and improvements to go after."""
    return int(round(10 * _aggr(g)))


def attack_bar(g: "Game") -> float:
    """The lowest attack score barbarians accept: 0 at aggression 0, falling as they grow bolder."""
    return -35 * _aggr(g)


def _loss_weight(g) -> float:
    """How much barbarians mind the damage they take, relative to the damage they deal."""
    return 1.3 - 0.6 * _aggr(g)


def _pillage_weight(g) -> float:
    """How much barbarians value pillaging against fighting: timid raiders loot, bold ones fight."""
    return 1.4 - 0.6 * _aggr(g)


def _siege_size(g) -> int:
    """How many barbarians gather around a healthy city before attacking it at bad odds."""
    return max(1, int(round(3 - 3 * _aggr(g))))


def _max_near_camp(g) -> int:
    """How many barbarians may already be around a camp for it to spawn another (2 at aggression 50)."""
    return max(1, int(round(4 * _aggr(g))))


def _know_the_land(g: "Game"):
    """Barbarians know the terrain everywhere.

    Their player never explores (visibility skips them), and movement only plans through explored tiles,
    so without this they could not move at all beyond attacking what stood next to them.
    """
    bid = g.barbarian_id
    if bid is None:
        return
    p = g.player(bid)
    if len(p.explored) != g.grid.size or not all(p.explored):
        p.explored = bytearray(b"\x01") * g.grid.size


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
    _know_the_land(g)
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
        if near > _max_near_camp(g):
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
    cd *= 1.3 - 0.6 * _aggr(g)          # 1.0 at aggression 50; bolder barbarians breed faster
    camp["countdown"] = max(0, int(cd * g.speed["barbarianModifier"]))


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
# Sacking cities
# ----------------------------------------------------------------------------
def _sackable_buildings(g: "Game", city) -> list[str]:
    """Buildings a sack may burn: never wonders, national wonders, the palace or free buildings."""
    R = g.rules
    free = set(g.player(city.owner).free_buildings.get(str(city.id), [])) | set(city.free_buildings or [])
    out = []
    for b in city.buildings:
        bd = R.buildings.get(b)
        if bd is None or b in free:
            continue
        if bd.get("isWonder") or bd.get("isNationalWonder") or bd.get("_any_wonder"):
            continue
        if bd["_umap"].has_tag(U.IndicatesCapital):
            continue
        out.append(b)
    return sorted(out)


def sack_city(g: "Game", city, unit=None) -> dict:
    """Barbarians never take cities: one they bring down is sacked instead (Civilization V style).

    They carry off a share of the owner's gold, may kill a citizen and may burn a building (never a wonder).
    The city keeps its owner and is left with a little health. How hard a sack hits grows with aggression.
    """
    from .cities import add_population, remove_building, max_health
    if recently_sacked(g, city):
        # already picked clean: the raiders are driven off without taking anything more
        city.health = max(city.health, 2)
        return {"sacked_city": None, "note": f"{city.name} was sacked too recently to have anything left to take."}
    a = _aggr(g)
    cp = g.player(city.owner)
    rng = g.state_rng("barb_sack", g.turn, city.id, len(g.s.events))
    have = max(0, int(cp.gold))
    stolen = int(have * (0.25 + 0.25 * a))
    stolen = min(have, max(stolen, min(have, 25)), int((150 + 350 * a) * g.speed.get("goldCostModifier", 1)))
    cp.gold -= stolen
    killed = False
    if city.pop > 1 and rng.random() < 0.3 + 0.4 * a:
        add_population(g, city, -1)
        killed = True
    burned = None
    opts = _sackable_buildings(g, city)
    if opts and rng.random() < 0.25 + 0.35 * a:
        burned = rng.choice(opts)
        remove_building(g, city, burned)
    city.health = max(2, max_health(g, city) // 4)       # the raiders leave; it takes a new assault to sack it again
    city.sacked_turn = g.turn
    parts = [f"stole {stolen} gold"] if stolen else []
    if killed:
        parts.append("killed a citizen")
    if burned:
        parts.append(f"burned the {burned}")
    if parts:
        text = f"Barbarians sacked {city.name}: they " + (", ".join(parts[:-1]) + " and " + parts[-1] if len(parts) > 1 else parts[0]) + "!"
    else:
        text = f"Barbarians sacked {city.name}, but found nothing worth carrying off."
    g.emit("city_sacked", text, [city.owner], idx=city.idx, gold=stolen, citizen_killed=killed, building=burned)
    g.invalidate()
    return {"sacked_city": city.name, "gold_stolen": stolen, "citizen_killed": killed, "building_destroyed": burned}


# ----------------------------------------------------------------------------
# Turn & automation (BarbarianAutomation)
# ----------------------------------------------------------------------------
def take_turn(g: "Game"):
    """Play the barbarians' turn: move every unit, then spawn from camps."""
    bid = g.barbarian_id
    if bid is None or level(g) is None:
        return
    _know_the_land(g)
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
    """Give one barbarian unit its orders: heal by pillaging, attack or pillage, hunt, or wander."""
    ud = g.rules.units[u.type]
    if not ud["_military"]:
        return _automate_civilian(g, u)
    if g.s.tiles[u.idx].improvement == CAMP:
        # the camp's guard stays home
        if _try_attack(g, u, stay=True):
            return
        u.activity = "fortify"
        return
    a = _aggr(g)
    if u.hp < 50 - 20 * a:
        # wounded: pillage (which heals) before anything else
        if _try_pillage(g, u, only_here=True) and u.moves <= 0:
            return
        if u.moves > 0 and _try_pillage(g, u) and u.moves <= 0:
            return
    for _ in range(3):
        if u.moves <= 0 or g.unit(u.id) is None:
            return
        if not _act_here(g, u):
            break
    if g.unit(u.id) is None or u.moves <= 0:
        return
    if _seek(g, u):
        if g.unit(u.id) is not None and u.moves > 0:
            _act_here(g, u)
        return
    _wander(g, u)


def _act_here(g, u) -> bool:
    """Attack or pillage within reach this turn, whichever is worth more. True if the unit did something."""
    atk = _best_attack(g, u)
    pil = _best_pillage(g, u)
    pv = pil[0] * 15 * _pillage_weight(g) if pil else None
    tries = [("attack", atk), ("pillage", pil)]
    if atk is not None and pv is not None and pv > atk[0]:
        tries.reverse()
    for kind, what in tries:
        if what is None or g.unit(u.id) is None or u.moves <= 0:
            continue
        if (_do_attack(g, u, what) if kind == "attack" else _do_pillage(g, u, what[1])):
            return True
    return False


def _automate_civilian(g, u):
    """Take a captured civilian to the nearest encampment."""
    from .movement import find_path, move_toward
    if g.s.tiles[u.idx].improvement == CAMP:
        u.activity = "sleep"
        return
    camps = sorted((c["idx"] for c in g.s.camps.values() if not c.get("destroyed")),
                   key=lambda i: (g.grid.distance(u.idx, i), i))
    for i in camps:
        if g.civilian_at(i) is None and find_path(g, u, i) is not None:
            move_toward(g, u, i, set_goto=False)
            return
    _wander(g, u)


def _barbs_near(g, idx: int, radius: int) -> int:
    """How many barbarian military units stand within a radius of a tile."""
    bid = g.barbarian_id
    return sum(1 for i in g.grid.within(idx, radius) if (m := g.military_at(i)) is not None and m.owner == bid)


def _attack_targets(g, u) -> list[tuple[float, int, int]]:
    """(score, target tile, tile to attack from). Melee units may move next to a target first.

    Scores are in hit points: damage dealt, less damage taken weighted by how much barbarians mind it at
    this aggression. Civilians are free captures, a defenceless city is a sack, and a kill counts double.
    """
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
    loss = _loss_weight(g)
    bold = _aggr(g) >= 0.9
    out = []
    seen = set()
    for frm, left in sorted(reach.items()):
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
                if recently_sacked(g, g.city_at(idx)):
                    continue                        # picked clean: there is nothing left worth taking yet
                if pv.get("note"):
                    if ranged:
                        continue
                    score = 1000.0                  # defences down: sack it
                else:
                    # a siege: with others gathered round, each blow is worth more (they will follow it up)
                    score = dd - da * loss + 12 * max(0, _barbs_near(g, idx, 2) - 1)
            else:
                civ = g.military_at(idx) is None and g.civilian_at(idx) is not None
                if civ and not ranged:
                    score = 150.0                   # a free capture
                else:
                    kills = dd >= pv["defender_hp"]
                    if not kills and not bold and pv["damage_to_attacker"][0] >= u.hp:
                        continue                    # certain death for nothing
                    score = dd * (2 if kills else 1) - da * loss
            out.append((score, idx, frm))
    return out


def _best_attack(g, u, stay: bool = False) -> Optional[tuple[float, int, int]]:
    """The attack this unit should make now, or None if nothing is worth it at this aggression."""
    targets = _attack_targets(g, u)
    if stay:
        targets = [t for t in targets if t[2] == u.idx]
    if not targets:
        return None
    ranged = g.rules.units[u.type]["_ranged"]
    bar = attack_bar(g)
    need = _siege_size(g)
    ok = []
    for t in targets:
        score, idx = t[0], t[1]
        if score >= 1000 or ranged:
            ok.append(t)
            continue
        if score < 0 and g.city_at(idx) is not None and _barbs_near(g, idx, 2) < need:
            continue                                # wait for the others before storming a healthy city
        if score > bar:
            ok.append(t)
    if not ok:
        return None
    return max(ok, key=lambda t: (t[0], -t[1], -t[2]))


def _do_attack(g, u, target) -> bool:
    """Move to the attack tile if needed, then attack."""
    from . import combat
    from .movement import move_toward
    score, idx, frm = target
    if frm != u.idx:
        move_toward(g, u, frm, set_goto=False)
        if g.unit(u.id) is None or u.idx != frm:
            return False
    try:
        combat.attack(g, u, idx)
        return True
    except ActionError:
        return False


def _try_attack(g, u, stay: bool = False) -> bool:
    """Attack something in reach if it is worth attacking."""
    t = _best_attack(g, u, stay=stay)
    return t is not None and _do_attack(g, u, t)


def pillage_value(g: "Game", pid: int, idx: int) -> int:
    """How much barbarians want to pillage a tile: 3 for an improvement on a luxury or strategic resource,
    2 for any other improvement, 1 for a road or railroad, 0 when there is nothing they may pillage."""
    from .workers import improvement_to_pillage
    t = g.s.tiles[idx]
    if t.owner is None or t.owner == pid or g.city_at(idx) is not None or t.improvement == CAMP:
        return 0
    what = improvement_to_pillage(g, idx)
    if what is None:
        return 0
    if what == t.improvement:
        rt = g.rules.resources[t.resource]["resourceType"] if t.resource in g.rules.resources else None
        return 3 if rt in ("Luxury", "Strategic") else 2
    return 1


def _best_pillage(g, u, only_here: bool = False) -> Optional[tuple[int, int]]:
    """(value, tile) of the most valuable thing to pillage this turn, nearest first among equals."""
    from .movement import reachable_this_turn
    ud = g.rules.units[u.type]
    if ud["_domain"] != "Land" or u.moves <= 0:
        return None
    from .units import unit_has
    if unit_has(g, u, U.CannotPillage, with_civ=True):
        return None
    tiles = [u.idx] if only_here else [i for i, left in reachable_this_turn(g, u).items() if left > 0] + [u.idx]
    best = None
    for i in tiles:
        if i != u.idx and g.military_at(i) is not None:
            continue
        v = pillage_value(g, u.owner, i)
        if v <= 0:
            continue
        key = (v, -g.grid.distance(u.idx, i), -i)
        if best is None or key > best[0]:
            best = (key, i)
    return (best[0][0], best[1]) if best else None


def _do_pillage(g, u, idx: int) -> bool:
    """Move to a tile and pillage it (which heals the unit)."""
    from . import workers
    from .movement import move_toward
    if idx != u.idx:
        move_toward(g, u, idx, set_goto=False)
        if g.unit(u.id) is None or u.idx != idx:
            return False
    try:
        workers.pillage(g, u)
        return True
    except ActionError:
        return False


def _try_pillage(g, u, only_here: bool = False) -> bool:
    """Pillage the most valuable improvement here or within reach: resources first, then others, then roads."""
    p = _best_pillage(g, u, only_here=only_here)
    return p is not None and _do_pillage(g, u, p[1])


def _seek(g, u) -> bool:
    """Head for the most tempting target within the search radius: a civilian to capture, a city to sack or
    besiege, a unit to fight, an improvement to pillage. True if the unit moved toward one."""
    from .movement import find_path, move_toward, can_stand
    radius = seek_radius(g)
    if radius <= 0 or u.moves <= 0:
        return False
    a = _aggr(g)
    bid = u.owner
    ud = g.rules.units[u.type]
    land = ud["_domain"] == "Land"
    melee = ud["_melee"]
    pw = _pillage_weight(g)
    cands = []
    for idx in g.grid.within(u.idx, radius):
        if idx == u.idx:
            continue
        d = g.grid.distance(u.idx, idx)
        city = g.city_at(idx)
        if city is not None:
            if city.owner != bid and not recently_sacked(g, city):
                # barbarians already around a city draw the others in: that is a siege
                v = 40 + 40 * a + 12 * _barbs_near(g, idx, 2)
                cands.append((v - 4 * d, idx, "adjacent"))
            continue
        mil = g.military_at(idx)
        if mil is not None:
            if mil.owner != bid and not g.rules.units[mil.type]["_domain"] == "Air":
                v = 15 + 30 * a + (100 - mil.hp) * 0.3
                cands.append((v - 4 * d, idx, "adjacent"))
            continue
        civ = g.civilian_at(idx)
        if civ is not None and civ.owner != bid:
            if melee:
                cands.append((70 + 20 * a - 4 * d, idx, "onto"))
            continue
        if land:
            pv = pillage_value(g, bid, idx)
            if pv:
                cands.append((pv * 20 * pw - 4 * d, idx, "onto"))
    cands.sort(key=lambda c: (-c[0], c[1]))
    max_turns = max(2, radius // 2 + 1)
    for score, idx, how in cands[:5]:
        if how == "onto":
            dests = [idx]
        else:
            dests = sorted((n for n in g.grid.neighbors(idx) if can_stand(g, bid, ud, n, u)),
                           key=lambda n: (g.grid.distance(u.idx, n), n))[:2]
            if u.idx in g.grid.neighbors(idx):
                continue                            # already there; nothing better to do than wait
        for dest in dests:
            if find_path(g, u, dest, max_turns=max_turns) is None:
                continue
            try:
                move_toward(g, u, dest, set_goto=False)
            except ActionError:
                continue
            return True
    return False


def _wander(g, u):
    """UnitAutomation.wander: move to a random reachable tile."""
    from .movement import reachable_this_turn, move_toward, can_stand
    ud = g.rules.units[u.type]
    reach = sorted(i for i, left in reachable_this_turn(g, u).items() if i != u.idx and can_stand(g, u.owner, ud, i, u))
    if not reach:
        return
    rng = g.state_rng("wander", u.id, g.turn)
    target = rng.choice(reach)
    move_toward(g, u, target, set_goto=False)
