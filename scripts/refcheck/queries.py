"""The deterministic questions record.py asks of every recorded state, and the Python engine functions that answer.

Each group of questions is answered on its own fresh load of the recorded state (``common.load_game``), with every
cache cold, in the order of ``GROUPS``. So an answer depends only on the state file and on the order of calls inside
its own group, which is what another engine has to reproduce. Where a sample is needed (tiles, units, paths,
proposals) it is drawn from ``random.Random("refcheck|<seed>|<turn>|<group>")``, and the chosen inputs are written
next to each answer, so nobody has to reproduce the sampling.

None of these questions touches the game's RNG. A difference in an answer is therefore a porting mistake or a
deliberate fix (refcheck/intended.toml), never luck.

Every group's output names the functions that produced it under ``"fn"``, as ``module.function`` inside
``citar.engine`` (or ``bots.basic`` for the bot), so the Rust side knows what each number means.
"""
from __future__ import annotations

import json
import random
import time

import common

TILE_SAMPLE = 150
REACHABLE_UNITS = 40
PATHS = 40
PATH_RADIUS = 12
COMBAT_TARGETS = 2
COMBAT_PAIRS = 300
DEAL_PAIRS = 6
DEALS_PER_PAIR = 4


def _majors(g) -> list[int]:
    """Living major civilizations, by id."""
    return [p.id for p in g.majors()]


def _civs(g) -> list[int]:
    """Living civilizations and city-states (not barbarians), by id."""
    return [p.id for p in g.s.players if p.alive and p.kind != "barbarian"]


def _units(g) -> list:
    """Every unit, by id."""
    return sorted(g.s.units.values(), key=lambda u: u.id)


def _cities(g) -> list:
    """Every city, by id."""
    return sorted(g.s.cities.values(), key=lambda c: c.id)


def _sample(rng: random.Random, items: list, n: int) -> list:
    """Up to *n* of *items*, chosen by *rng* and kept in their original order."""
    if len(items) <= n:
        return list(items)
    keep = set(rng.sample(range(len(items)), n))
    return [x for i, x in enumerate(items) if i in keep]


def _snap(x):
    """An answer as plain JSON values, copied when it is given: many engine results are cache entries, and the
    recorded answer must be the one the call returned, whatever later calls in the group do to the cache."""
    return json.loads(common.dumps(x))


def _error(fn, *args):
    """Call an engine function that may refuse: its result, or {"error": the refusal text}."""
    from citar.engine.game import ActionError
    try:
        return fn(*args)
    except ActionError as e:
        return {"error": str(e)}


# ----------------------------------------------------------------------------
# groups
# ----------------------------------------------------------------------------
def tile_yields(g, rng) -> dict:
    """Yields of every owned tile as its city works it, and of a sample of unowned tiles as nobody and as the
    first living major civilization see them."""
    from citar.engine import tiles as T
    owned, sample = [], []
    unowned = []
    for idx, t in enumerate(g.s.tiles):
        if t.owner is None:
            unowned.append(idx)
            continue
        city = g.city(t.city) if t.city is not None else None
        owned.append({"idx": idx, "pid": t.owner, "city": city.id if city else None,
                      "yields": _snap(T.tile_stats(g, idx, t.owner, city))})
    viewer = (_majors(g) or [None])[0]
    for idx in _sample(rng, unowned, TILE_SAMPLE):
        sample.append({"idx": idx, "pid": None, "city": None, "yields": _snap(T.tile_stats(g, idx, None, None))})
        if viewer is not None:
            sample.append({"idx": idx, "pid": viewer, "city": None,
                           "yields": _snap(T.tile_stats(g, idx, viewer, None))})
    return {"fn": {"yields": "tiles.tile_stats(g, idx, pid, city)"}, "owned": owned, "sample": sample}


def city_stats(g, rng) -> dict:
    """Everything the engine derives for each city: the full stats breakdown and the numbers around it."""
    from citar.engine import cities as C, combat, religion
    out = []
    for c in _cities(g):
        e = {"id": c.id, "owner": c.owner, "name": c.name,
             "stats": _snap(C.city_stats(g, c)),
             "food_to_next_pop": C.food_to_next_pop(g, c),
             "maintenance": C.maintenance(g, c),
             "max_health": C.max_health(g, c),
             "strength": combat.city_strength(g, c),
             "workable": C.workable_tiles(g, c),
             "connected_to_capital": C.connected_to_capital(g, c)}
        cur = C.current_construction(c)
        if cur:
            e["current"] = {"item": cur, "turns": C.turns_to_build(g, c, cur)}
        if g.religion_enabled:
            e["followers"] = religion.followers(g, c)
            e["majority"] = religion.majority_religion(g, c)
            e["pressure_in"] = religion.pressures_from_surroundings(g, c)
        out.append(_snap(e))
    return {"fn": {"stats": "cities.city_stats(g, city)", "food_to_next_pop": "cities.food_to_next_pop",
                   "maintenance": "cities.maintenance", "max_health": "cities.max_health",
                   "strength": "combat.city_strength(g, city)", "workable": "cities.workable_tiles",
                   "connected_to_capital": "cities.connected_to_capital",
                   "current": "cities.current_construction + cities.turns_to_build",
                   "followers": "religion.followers", "majority": "religion.majority_religion",
                   "pressure_in": "religion.pressures_from_surroundings"},
            "cities": out}


def civs(g, rng) -> dict:
    """Per civilization: happiness, stats for next turn, resources, the unique index, research and policy costs,
    score and the victory milestones."""
    from citar.engine import economy as E, research, policies, victory
    out = []
    for pid in _civs(g):
        p = g.player(pid)
        e = {"pid": pid, "kind": p.kind,
             "happiness": _snap(E.happiness(g, pid)),
             "civ_stats": _snap(E.civ_stats(g, pid)),
             "stat_map": _snap(E.stat_map(g, pid)),
             "gold_per_turn": E.gold_per_turn(g, pid),
             "resource_supply": _snap(E.resource_supply(g, pid)),
             "detailed_resources": [list(x) for x in E.detailed_resources(g, pid)],
             "unique_index": {ph: len(v) for ph, v in sorted(E.civ_index(g, pid).items())},
             "unit_maintenance": E.unit_maintenance(g, pid),
             "unit_supply": E.unit_supply(g, pid),
             "era": research.player_era(g, pid),
             "tech_cost": {t: research.tech_cost(g, pid, t) for t in research.available_techs(g, pid)},
             "policy_cost": policies.culture_cost(g, pid)}
        if p.kind == "major":
            e["adoptable_policies"] = policies.adoptable_policies(g, pid)
            e["score"] = victory.score(g, pid)
            e["military_strength"] = victory.military_strength(g, pid)
            e["victory_progress"] = victory.victory_progress(g, pid)
        out.append(e)
    world = {"world_era": research.world_era(g), "un_owner": victory.un_owner(g),
             "votes_needed": victory.votes_needed(g), "vote_open": victory.vote_open(g)}
    return {"fn": {"happiness": "economy.happiness", "civ_stats": "economy.civ_stats", "stat_map": "economy.stat_map",
                   "gold_per_turn": "economy.gold_per_turn", "resource_supply": "economy.resource_supply",
                   "detailed_resources": "economy.detailed_resources",
                   "unique_index": "economy.civ_index (placeholder -> number of uniques)",
                   "unit_maintenance": "economy.unit_maintenance", "unit_supply": "economy.unit_supply",
                   "era": "research.player_era",
                   "tech_cost": "research.tech_cost for each of research.available_techs",
                   "policy_cost": "policies.culture_cost(g, pid)", "adoptable_policies": "policies.adoptable_policies",
                   "score": "victory.score", "military_strength": "victory.military_strength",
                   "victory_progress": "victory.victory_progress",
                   "world": "research.world_era, victory.un_owner, victory.votes_needed, victory.vote_open"},
            "civs": out, "world": world}


def buildable(g, rng) -> dict:
    """What each major civilization's cities can build, what it costs in production, and whether and for how
    much it can be bought with gold or faith."""
    from citar.engine import cities as C
    out = []
    majors = set(_majors(g))
    for c in _cities(g):
        if c.owner not in majors:
            continue
        items = C.buildable_items(g, c)
        costs = {}
        for kind in ("units", "buildings", "wonders"):
            for name in items[kind]:
                costs[name] = {"production": C.production_cost(g, c.owner, name, c),
                               "turns": C.turns_to_build(g, c, name),
                               "gold": list(C.purchase_check(g, c, name, "Gold")),
                               "faith": list(C.purchase_check(g, c, name, "Faith"))}
        out.append({"city": c.id, "owner": c.owner, "items": items, "costs": costs})
    return {"fn": {"items": "cities.buildable_items", "production": "cities.production_cost(g, pid, name, city)",
                   "turns": "cities.turns_to_build", "gold": "cities.purchase_check(g, city, name, 'Gold') -> "
                   "[refusal or null, cost or null]", "faith": "cities.purchase_check(g, city, name, 'Faith')"},
            "cities": out}


def movement(g, rng) -> dict:
    """Where units with moves left can get this turn, and seeded paths with their turns and step costs."""
    from citar.engine import movement as M
    reach = []
    movers = [u for u in _units(g) if u.moves > 0 and not M.is_air(g.rules.units[u.type])]
    for u in _sample(rng, movers, REACHABLE_UNITS):
        r = M.reachable_this_turn(g, u)
        reach.append({"unit": u.id, "type": u.type, "owner": u.owner, "from": u.idx, "moves": u.moves,
                      "max_moves": M.max_moves(g, u), "reachable": sorted([k, v] for k, v in r.items())})
    paths = []
    walkers = [u for u in _units(g) if not M.is_air(g.rules.units[u.type])]
    for _ in range(PATHS if walkers else 0):
        u = walkers[rng.randrange(len(walkers))]
        near = [i for i in g.grid.within(u.idx, PATH_RADIUS) if i != u.idx]
        target = near[rng.randrange(len(near))]
        path = M.find_path(g, u, target)
        e = {"unit": u.id, "type": u.type, "owner": u.owner, "from": u.idx, "to": target, "moves": u.moves,
             "path": path}
        if path:
            e["turns"] = M.path_turns(g, u, path)
            e["step_costs"] = [M.enter_cost(g, u, a, b) for a, b in zip(path, path[1:])]
        paths.append(e)
    return {"fn": {"reachable": "movement.reachable_this_turn (tile -> movement left, in move_scale units)",
                   "max_moves": "movement.max_moves", "path": "movement.find_path(g, unit, target) (max_turns 40)",
                   "turns": "movement.path_turns", "step_costs": "movement.enter_cost(g, unit, a, b) per step"},
            "reachable": reach, "paths": paths}


def visible(g, rng) -> dict:
    """The tiles each living major civilization can see."""
    from citar.engine import visibility
    return {"fn": {"tiles": "visibility.visible_tiles(g, pid), sorted"},
            "civs": [{"pid": pid, "tiles": sorted(visibility.visible_tiles(g, pid))} for pid in _majors(g)]}


ROLLS = (0.0, 0.5, 1.0)


def _fight(g, a, d, frm: int) -> dict:
    """Strengths, modifiers and damage for one attacker against one defender, with the random roll fixed."""
    from citar.engine import combat as K
    return {"attacker_strength": K.attacking_strength(g, a, d, frm),
            "defender_strength": K.defending_strength(g, a, d, frm),
            "attack_modifiers": K.attack_modifiers(g, a, d, frm),
            "defense_modifiers": K.defense_modifiers(g, a, d, frm),
            "damage_to_defender": [K.damage_to_defender(g, a, d, frm, r) for r in ROLLS],
            "damage_to_attacker": [K.damage_to_attacker(g, a, d, frm, r) for r in ROLLS]}


def _defender(d) -> dict:
    """Which unit or city defends."""
    return {"city": d.city.id} if d.city is not None else {"unit": d.unit.id}


def _interception(g, a, d, idx: int) -> dict:
    """What an air strike on *idx* meets before its fight, as combat.try_intercept sees it but without its random
    draws: every unit of the defender's that could intercept, with its chance and the damage it would deal at the
    fixed rolls, and the factor applied to that damage. try_intercept picks the candidate with the highest chance
    (the first in g.player_units order on a tie)."""
    from citar.engine import combat as K, unique_types as U
    from citar.engine.units import unit_has, unit_uniques
    if unit_has(g, a.unit, U.CannotBeIntercepted):
        return {"immune": True}
    out = []
    for u in g.player_units(d.owner):
        if not K.can_intercept(g, u, idx) or (d.unit is not None and u.id == d.unit.id):
            continue
        ic = K.Combatant(unit=u)
        factor = 1 + sum(x.n(0) for x in unit_uniques(g, u, U.DamageWhenIntercepting)) / 100
        for x in unit_uniques(g, a.unit, U.DamageFromInterceptionReduced):
            factor *= 1 - x.n(0) / 100
        out.append({"unit": u.id, "type": u.type, "chance": K.intercept_chance(g, u), "factor": factor,
                    "damage": [K.damage_to_defender(g, ic, a, u.idx, r) for r in ROLLS]})
    return {"immune": False, "candidates": out}


def combat_previews(g, rng) -> dict:
    """Every unit and city with something to attack (up to COMBAT_TARGETS targets each, COMBAT_PAIRS fights in
    all): the fight's numbers at rolls 0, 0.5 and 1, and the engine's own preview, which also checks that the
    attack is allowed right now. Aircraft (not nuclear weapons, which do not fight) get their interception
    instead of a preview, which the engine refuses them."""
    from citar.engine import combat as K, visibility, movement as M, unique_types as U
    from citar.engine.units import attack_range
    pairs, attackers = [], 0
    for u in _units(g):
        ud = g.rules.units[u.type]
        if not ud["_military"] or ud["_umap"].get(U.NuclearWeapon):
            continue
        kind = "air" if M.is_air(ud) else "unit"
        a = K.Combatant(unit=u)
        targets = []
        for idx in g.grid.within(u.idx, attack_range(g, u)):
            if idx == u.idx or K.combatant_at(g, idx) is None:
                continue
            if not g.is_barbarian(u.owner) and not visibility.is_visible(g, u.owner, idx):
                continue
            if K.contains_attackable_enemy(g, idx, a) is None:
                targets.append((kind, u, idx))
        attackers += bool(targets)
        pairs += _sample(rng, targets, COMBAT_TARGETS)
    for c in _cities(g):
        if not g.is_barbarian(c.owner):
            targets = [("city", c, idx) for idx in K.bombard_targets(g, c)]
            attackers += bool(targets)
            pairs += _sample(rng, targets, COMBAT_TARGETS)
    out = []
    for kind, who, idx in _sample(rng, pairs, COMBAT_PAIRS):
        d = K.combatant_at(g, idx)
        if kind == "city":
            a, frm = K.Combatant(city=who), who.idx
            e = {"attacker": {"city": who.id, "owner": who.owner, "from": frm}}
        else:
            a, frm = K.Combatant(unit=who), who.idx
            e = {"attacker": {"unit": who.id, "type": who.type, "owner": who.owner, "from": frm}}
        e.update({"target": idx, "defender": _defender(d), **_fight(g, a, d, frm)})
        if kind == "unit":
            e["preview"] = _error(K.preview, g, who, idx)
        elif kind == "air":
            e["can_attack_now"] = K.can_attack_now(g, who)
            e["interception"] = _interception(g, a, d, idx)
        out.append(e)
    return {"fn": {"pairs": "military units except nuclear weapons: tiles within units.attack_range that the owner "
                            "sees and combat.contains_attackable_enemy accepts; cities: combat.bombard_targets. At "
                            f"most {COMBAT_TARGETS} targets per attacker and {COMBAT_PAIRS} fights, sampled",
                   "attacker_strength": "combat.attacking_strength(g, a, d, from_tile)",
                   "defender_strength": "combat.defending_strength", "attack_modifiers": "combat.attack_modifiers",
                   "defense_modifiers": "combat.defense_modifiers",
                   "damage_to_defender": "combat.damage_to_defender at rnd 0.0, 0.5, 1.0",
                   "damage_to_attacker": "combat.damage_to_attacker at rnd 0.0, 0.5, 1.0",
                   "preview": "ground and sea units: combat.preview(g, unit, idx), or its refusal",
                   "can_attack_now": "aircraft: combat.can_attack_now (the one check of combat.air_strike that "
                                     "the target list does not already make), refusal or null",
                   "interception": "aircraft: immune if the attacker has 'Cannot be intercepted'; else each "
                                   "candidate of combat.try_intercept (combat.can_intercept over the target, not "
                                   "the defending unit itself) with combat.intercept_chance, try_intercept's damage "
                                   "factor, and combat.damage_to_defender(g, interceptor, aircraft, its tile, rnd) "
                                   "at rnd 0.0, 0.5, 1.0"},
            "attackers": attackers, "fights": out}


def _proposals(g, a: int, b: int) -> list[tuple[list, list]]:
    """Candidate deals between two civilizations, as (a gives, b gives): one of each kind of item."""
    from citar.engine import economy as E, research
    pa, pb = g.player(a), g.player(b)
    out = [([{"type": "gold", "amount": 60}], [{"type": "open_borders", "turns": 30}]),
           ([{"type": "gold_per_turn", "amount": 3, "turns": 30}], [{"type": "embassy"}]),
           ([{"type": "embassy"}], [{"type": "embassy"}]),
           ([{"type": "declaration_of_friendship"}], []),
           ([{"type": "research_agreement"}], []),
           ([{"type": "share_map"}], [{"type": "gold", "amount": 40}]),
           ([{"type": "peace_treaty"}], []),
           ([{"type": "gold", "amount": 99999}], [])]
    lux = sorted(r for r, d in E.luxury_resources(g, b).items() if d["net"] > 0)
    if lux:
        out.append(([{"type": "gold", "amount": 50}], [{"type": "resource", "resource": lux[0], "amount": 1}]))
    techs = [t for t in pa.techs if research.can_research(g, b, t)]
    if techs:
        out.append(([{"type": "tech", "tech": sorted(techs)[0]}], [{"type": "gold", "amount": 100}]))
    others = [q for q in _majors(g) if q not in (a, b)]
    if others:
        out.append(([{"type": "declare_war", "target": others[0]}], [{"type": "gold", "amount": 80}]))
    towns = [c.id for c in g.player_cities(a) if c.id != pa.capital]
    if towns:
        out.append(([{"type": "city", "city_id": towns[0]}], [{"type": "gold", "amount": 300}]))
    if pb.capital is not None:
        out.append(([], [{"type": "city", "city_id": pb.capital}]))
    return out


def deal_checks(g, rng) -> dict:
    """Seeded proposals between met major civilizations: whether the rules accept each side's items, how the deal
    reads, and what a fresh default bot thinks each side is worth to it."""
    from citar.engine import diplomacy as D
    from citar.engine.game import ActionError
    from citar.bots.basic import BasicBot
    bot = BasicBot(seed=0)
    majors = _majors(g)
    pairs = [(a, b) for a in majors for b in majors if a < b and g.has_met(a, b)]
    out = []
    for a, b in _sample(rng, pairs, DEAL_PAIRS):
        for give, receive in _sample(rng, _proposals(g, a, b), DEALS_PER_PAIR):
            e = {"a": a, "b": b, "give": give, "receive": receive, "ra_cost": D.ra_cost(g, a, b)}
            try:
                prop = D._make_proposal(g, a, b, give, receive)
                e["proposal"] = prop
                if prop is not None:
                    D.validate_items(g, a, b, prop[str(a)], prop)
                    D.validate_items(g, b, a, prop[str(b)], prop)
                e["valid"] = True
            except ActionError as err:
                e["valid"] = False
                e["error"] = str(err)
            prop = e.get("proposal")
            if prop:
                e["describe"] = {str(a): D.describe_items(g, prop[str(a)]), str(b): D.describe_items(g, prop[str(b)])}
                e["bot_value"] = {str(a): bot.evaluate(g, a, b, prop[str(a)], prop[str(b)]),
                                  str(b): bot.evaluate(g, b, a, prop[str(b)], prop[str(a)])}
            out.append(e)
    return {"fn": {"proposal": "diplomacy._make_proposal(g, a, b, give, receive)",
                   "valid": "diplomacy.validate_items for a's side, then b's; error is the first refusal",
                   "describe": "diplomacy.describe_items", "ra_cost": "diplomacy.ra_cost",
                   "bot_value": "bots.basic.BasicBot(seed=0).evaluate(g, pid, other, give, receive): a fresh bot "
                                "with default parameters and no memory of wars it planned"},
            "deals": out}


def _tool_calls(g) -> list[tuple[int, str, dict]]:
    """About thirty tool calls that should be refused, each for a different reason."""
    majors = _majors(g)
    me = g.s.current if g.s.current in majors else (majors or [0])[0]
    other = next((q for q in majors if q != me), me)
    unit = next((u for u in sorted(g.player_units(me), key=lambda u: u.id)), None)
    mil = next((u for u in sorted(g.player_units(me), key=lambda u: u.id) if g.rules.units[u.type]["_military"]),
               unit)
    city = next((c for c in sorted(g.player_cities(me), key=lambda c: c.id)), None)
    foreign = next((u for u in sorted(g.s.units.values(), key=lambda u: u.id) if u.owner != me), None)
    uid = unit.id if unit else 999999
    mid = mil.id if mil else 999999
    cid = city.id if city else 999999
    far = g.grid.xy(max(0, g.grid.size - 1 - (unit.idx if unit else 0)))
    wonder = next(n for n, b in g.rules.buildings.items() if b.get("isWonder"))
    return [
        (me, "fly_to_the_moon", {}),
        (len(g.s.players) + 5, "get_empire", {}),
        (me, "get_city", {"city_id": 999999}),
        (me, "get_rules", {"topic": "astrology"}),
        (me, "preview_attack", {"unit_id": uid, "x": far[0], "y": far[1]}),
        (other, "set_research", {"tech": "Pottery"}) if other != me else (me, "set_research", {}),
        (me, "move_unit", {"unit_id": uid}),
        (me, "move_unit", {"unit_id": "abc", "x": 0, "y": 0}),
        (me, "move_unit", {"unit_id": foreign.id if foreign else 999999, "x": 0, "y": 0}),
        (me, "move_unit", {"unit_id": uid, "x": -3, "y": 99999}),
        (me, "set_production", {"city_id": 999999, "item": "Warrior"}),
        (me, "set_production", {"city_id": cid, "item": "Death Star"}),
        (me, "set_production", {"city_id": cid, "item": "Giant Death Robot"}),
        (me, "set_research", {"tech": "Warp Drive"}),
        (me, "adopt_policy", {"policy": "Tyranny"}),
        (me, "declare_war", {"player_id": 999}),
        (me, "denounce", {"player_id": me}),
        (me, "open_negotiation", {"to": other, "message": "   "}),
        (me, "open_negotiation", {"to": me, "message": "Hello, me."}),
        (me, "respond_negotiation", {"negotiation_id": 99999, "action": "accept", "message": "Fine."}),
        (me, "buy", {"city_id": cid, "item": wonder}),
        (me, "found_city", {"unit_id": mid}),
        (me, "build_improvement", {"unit_id": mid, "improvement": "Farm"}),
        (me, "attack", {"unit_id": mid, "x": far[0], "y": far[1]}),
        (me, "city_state_action", {"player_id": other, "action": "gift", "amount": 50}),
        (me, "move_spy", {"spy": "Nobody", "city_id": cid}),
        (me, "un_vote", {"candidate": other}),
        (me, "promote_unit", {"unit_id": mid, "promotion": "Wings of Icarus"}),
        (me, "work_tile", {"city_id": cid, "x": -1, "y": -1}),
        (me, "unit_action", {"unit_id": mid, "action": "found_religion", "name": "Refcheck"}),
    ]


def tool_errors(g, rng) -> dict:
    """The refusal text of deliberately invalid tool calls. A call that unexpectedly succeeds is recorded as such
    and the game reloaded, so later calls still see the recorded state."""
    from citar.engine import tools
    from citar.engine.game import ActionError
    text = common.state_text(g)
    out = []
    for pid, name, args in _tool_calls(g):
        e = {"pid": pid, "tool": name, "args": args}
        try:
            e["ok"] = json.loads(common.dumps(tools.execute(g, pid, name, dict(args))))
            g = common.load_game(text)
        except ActionError as err:
            e["error"] = str(err)
        out.append(e)
    return {"fn": {"error": "tools.execute(g, pid, tool, args) raising ActionError"}, "calls": out}


def views(g, rng) -> dict:
    """What the browser receives for the first two living major civilizations."""
    from citar.engine import views as V
    return {"fn": {"view": "views.client_view(g, pid)"},
            "civs": [{"pid": pid, "view": V.client_view(g, pid)} for pid in _majors(g)[:2]]}


def briefings(g, rng) -> dict:
    """The turn briefing and turn progress text a model reads, for the first two living major civilizations."""
    from citar.engine import briefing as B
    return {"fn": {"briefing": "briefing.briefing(g, pid)", "turn_progress": "briefing.turn_progress(g, pid)"},
            "civs": [{"pid": pid, "briefing": B.briefing(g, pid), "turn_progress": B.turn_progress(g, pid)}
                     for pid in _majors(g)[:2]]}


GROUPS = (("tile_yields", tile_yields), ("city_stats", city_stats), ("civs", civs), ("buildable", buildable),
          ("movement", movement), ("visible", visible), ("combat_previews", combat_previews),
          ("deal_checks", deal_checks), ("tool_errors", tool_errors), ("views", views), ("briefing", briefings))


def answer_all(text: str, seed: int, turn: int) -> tuple[dict, dict, dict]:
    """Answer every group on its own fresh load of the state *text*.

    Returns (answers by group, side effects, seconds by group). Side effects name, per group, the top-level state
    fields that answering changed (for example a city's religious pressures being seeded on first read); another
    engine has to know about those, since it will be asked the same questions in the same order.

    A group that raises is recorded as {"crash": one line, see common.error_text} and the others still run: a
    Python engine bug found by a question is worth keeping, and it must not cost the rest of a long recording. Its
    traceback goes to the console.
    """
    base = json.loads(text)
    base_parts = {k: common.dumps(v) for k, v in base.items()}
    answers, side, secs = {}, {}, {}
    for name, fn in GROUPS:
        t0 = time.time()
        g = common.load_game(text)
        rng = random.Random(f"refcheck|{seed}|{turn}|{name}")
        try:
            answers[name] = json.loads(common.dumps(fn(g, rng)))
        except Exception as e:
            answers[name] = {"crash": common.error_text(e)}
            common.print_trace(f"query group {name} raised (seed {seed}, turn {turn}):")
        after = g.s.to_dict()
        changed = sorted(k for k, v in after.items() if common.dumps(v) != base_parts.get(k))
        if changed:
            side[name] = changed
        secs[name] = round(time.time() - t0, 2)
    return answers, side, secs
