"""Fog-filtered views of the game for a given player (or an omniscient spectator when pid is None)."""
from __future__ import annotations

from typing import Optional

from . import tiles as T
from . import unique_types as U
from .game import Game, ActionError


def _visible(g: Game, pid: Optional[int]) -> Optional[set]:
    """The tiles a viewer can currently see, or None for an omniscient viewer."""
    from .visibility import visible_tiles
    return None if pid is None else visible_tiles(g, pid)


def can_see(g: Game, pid: Optional[int], idx: int) -> bool:
    """Whether a viewer can see a tile right now."""
    vis = _visible(g, pid)
    return vis is None or idx in vis


def knows_tile(g: Game, pid: Optional[int], idx: int) -> bool:
    """Whether a viewer has ever explored a tile.

    The distinction between this and :func:`can_see` is fog of war: an explored tile is shown as it was
    last seen, which is why a view can contain a city that has since been destroyed.
    """
    return pid is None or g.player(pid).explored[idx] != 0


def _xy(g, idx) -> list:
    """A tile index as an ``[x, y]`` pair."""
    return list(g.grid.xy(idx))


def _round(d: dict, nd: int = 1) -> dict:
    """Round a stat dictionary for display, dropping zeroes."""
    return {k: round(v, nd) for k, v in d.items() if v}


def _uniques(d: dict) -> list:
    """Rule text shown to players (UnCiv's AI weighting hints are left out)."""
    return [u for u in d.get("uniques", []) if "for AI decisions" not in u]


# ----------------------------------------------------------------------------
# Units
# ----------------------------------------------------------------------------
DOMAIN = {"Land": "land", "Water": "sea", "Air": "air"}
CLASS_BY_TYPE = {"Civilian": "civilian", "Civilian Water": "civilian", "Sword": "melee", "Gunpowder": "melee",
                 "Melee": "melee", "Archery": "ranged", "Ranged Gunpowder": "ranged", "Ranged": "ranged",
                 "Mounted": "mounted", "Armored": "armor", "Armor": "armor", "Siege": "siege", "Scout": "recon",
                 "Melee Water": "naval_melee", "Ranged Water": "naval_ranged", "Submarine": "naval_ranged",
                 "Aircraft Carrier": "naval_ranged", "Fighter": "air", "Bomber": "air", "Atomic Bomber": "air",
                 "Missile": "air", "Helicopter": "armor"}


def unit_class(ud: dict) -> str:
    """A coarse unit class for display (map glyphs), from the UnCiv unit type."""
    if not ud["_military"]:
        return "civilian"
    return CLASS_BY_TYPE.get(ud.get("unitType"), "melee")


def unit_info(g: Game, u, viewer: Optional[int], detail: bool = False) -> dict:
    """A unit as a viewer sees it, with full detail for its owner."""
    from . import movement, units as unitmod
    ud = g.udef(u)
    x, y = g.grid.xy(u.idx)
    own = viewer is None or viewer == u.owner
    d = {"id": u.id, "type": u.type, "name": u.name or u.type, "owner": u.owner, "x": x, "y": y, "hp": u.hp,
         "military": ud["_military"], "domain": DOMAIN.get(ud["_domain"], "land"), "unit_type": ud.get("unitType"),
         "class": unit_class(ud)}
    if ud["_great_person"]:
        d["great_person"] = True
    if u.religion and g.religion_enabled:
        from .religion import display_name
        d["religion"] = display_name(g, u.religion)
    if own:
        sc = g.rules.move_scale
        d.update({"moves": round(u.moves / sc, 2), "max_moves": round(movement.max_moves(g, u) / sc, 2),
                  "activity": u.activity, "xp": u.xp, "promotions": list(u.promotions),
                  "can_promote": unitmod.can_promote(g, u), "promotion_ready": unitmod.can_promote(g, u),
                  "attacks_made": u.attacks,
                  "embarked": movement.is_embarked(g, u)})
        if u.fortify:
            d["fortified_turns"] = u.fortify
        if u.status:
            d["status"] = list(u.status)
        t = g.s.tiles[u.idx]
        if u.activity in ("build", "automate") and t.build:
            d["building"] = [{"improvement": n, "turns_left": k} for n, k in t.build if k >= 0]
        if u.goto is not None:
            d["goto"] = _xy(g, u.goto)
        if u.religion and ud["_umap"].has_tag(U.ReligiousUnit):
            d["religious_strength"] = u.religious_strength
    if detail:
        _unit_detail(g, u, d, own)
    return d


def _unit_detail(g: Game, u, d: dict, own: bool):
    """Add everything an owner needs to give a unit orders: reachable tiles, targets, actions.

    This is what makes one query enough to play a unit's turn, rather than a query per question - which
    matters most for a model, where every question is a round trip.
    """
    from . import movement, units as unitmod, combat, workers
    from .actions import unit_actions
    ud = g.udef(u)
    d["strength"] = ud.get("strength", 0)
    if ud.get("rangedStrength"):
        d["ranged_strength"] = ud["rangedStrength"]
        d["range"] = unitmod.attack_range(g, u)
    d["abilities"] = _uniques(ud)
    if not own:
        return
    d["actions"] = unit_actions(g, u)
    if ud["_umap"].get(U.FoundCity):
        from .automation import suggest_city_sites
        d["suggested_city_sites"] = [{"x": g.grid.xy(i)[0], "y": g.grid.xy(i)[1], "score": round(s, 1)}
                                     for i, s in suggest_city_sites(g, u.owner, u.idx, radius=8, count=3)]
    opts = [o for o in workers.build_options(g, u) if not o.get("instant")]
    if opts:
        d["build_options"] = opts
    if ud["_umap"].get(U.CreateWaterImprovements):
        spots = [i for i, tt in enumerate(g.s.tiles) if tt.owner == u.owner and tt.resource and T.is_water(g, i)
                 and not tt.improvement and T.resource_visible(g, u.owner, tt.resource)]
        spots.sort(key=lambda i: g.grid.distance(i, u.idx))
        d["suggested_sites"] = [{"x": g.grid.xy(i)[0], "y": g.grid.xy(i)[1], "resource": g.s.tiles[i].resource}
                                for i in spots[:3]]
    target, err, cost = unitmod.check_upgrade(g, u)
    if target:
        d["upgrade"] = {"to": target, "gold": cost, "possible": err is None, "reason": err}
    if unitmod.can_promote(g, u):
        d["available_promotions"] = sorted(unitmod.available_promotions(g, u))
    d["xp_for_next_promotion"] = unitmod.xp_for_next(g, u)
    reach = movement.reachable_this_turn(g, u)
    d["reachable_this_turn"] = [_xy(g, i) for i in sorted(reach)][:80]
    targets = []
    if ud["_military"] and combat.can_attack_now(g, u) is None and ud["_domain"] != "Air":
        radius = unitmod.attack_range(g, u) if ud["_ranged"] else 1
        for idx in g.grid.within(u.idx, radius)[1:]:
            try:
                pv = combat.preview(g, u, idx)
            except ActionError:
                continue
            targets.append({"x": g.grid.xy(idx)[0], "y": g.grid.xy(idx)[1],
                            **{k: v for k, v in pv.items() if k in ("target", "defender", "damage_to_defender",
                                                                   "damage_to_attacker", "defender_hp", "note")}})
    if targets:
        d["attack_targets"] = targets


# ----------------------------------------------------------------------------
# Cities
# ----------------------------------------------------------------------------
def city_info(g: Game, c, viewer: Optional[int], detail: bool = False) -> dict:
    """A city as a viewer sees it, with full detail for its owner."""
    from . import combat, cities as cm
    x, y = g.grid.xy(c.idx)
    p = g.player(c.owner)
    d = {"id": c.id, "name": c.name, "owner": c.owner, "x": x, "y": y, "pop": c.pop,
         "capital": p.capital == c.id, "hp": c.health, "max_hp": cm.max_health(g, c),
         "strength": round(combat.city_strength(g, c) / 1, 1), "original_capital": c.original_capital,
         "puppet": c.puppet, "razing": c.razing, "resistance": c.resistance}
    if g.religion_enabled:
        from .religion import majority_religion, display_name
        mr = majority_religion(g, c)
        if mr:
            d["religion"] = display_name(g, mr)
    own = viewer is None or viewer == c.owner
    if not own:
        return d
    st = cm.city_stats(g, c)
    total = st["total"]
    surplus = total["food"]
    need = cm.food_to_next_pop(g, c)
    cm.current_construction(c)
    d.update({
        "yields": _round({k: total[k] for k in ("food", "production", "gold", "science", "culture", "faith")}),
        "happiness": round(sum(cm.city_happiness(g, c).values()), 1),
        "food_stored": round(c.food, 1), "food_to_grow": need,
        "turns_to_grow": (-(-(need - c.food) // surplus) if surplus > 0 else None),
        "starving": surplus < 0, "avoid_growth": c.avoid_growth,
        "queue": [{"item": q, "cost": cm.production_cost(g, c.owner, q, c) if q not in cm.PERPETUAL else None,
                   "progress": int(c.progress.get(q, 0)),
                   "turns": cm.turns_to_build(g, c, q) if q not in cm.PERPETUAL else None} for q in c.queue],
        "buildings": list(c.buildings), "focus": c.focus, "auto_production": c.auto_production,
        "specialists": dict(c.specialists),
        "culture_stored": round(c.culture, 1), "culture_for_next_tile": cm.culture_to_next_tile(g, c),
        "worked_tiles": [_xy(g, i) for i in c.worked], "locked_tiles": [_xy(g, i) for i in c.locked],
        "connected_to_capital": c.id in cm.connected_cities(g, c.owner),
    })
    if c.wltkd > 0:
        d["we_love_the_king_day_turns"] = c.wltkd
    elif c.demanded_resource:
        d["demands_resource"] = c.demanded_resource
    if detail:
        d["yield_breakdown"] = {src: _round(s) for src, s in st["final"].items() if _round(s)}
        d["happiness_breakdown"] = _round(cm.city_happiness(g, c))
        d["max_specialists"] = cm.max_specialists(g, c)
        items = cm.buildable_items(g, c)
        can = {}
        for k, names in items.items():
            rows = []
            for n in names:
                row = {"item": n}
                if n not in cm.PERPETUAL:
                    row["cost"] = cm.production_cost(g, c.owner, n, c)
                    row["turns"] = cm.turns_to_build(g, c, n)
                for stat in ("Gold", "Faith"):
                    reason, cost = cm.purchase_check(g, c, n, stat)
                    if cost and (reason is None or "enough" in reason.lower()):
                        row[f"buy_{stat.lower()}"] = cost
                rows.append(row)
            if rows:
                can[k] = rows
        d["can_build"] = can
        faith_only = []
        for n in g.rules.units:
            if n in items.get("units", []):
                continue
            reason, cost = cm.purchase_check(g, c, n, "Faith")
            if reason is None:
                faith_only.append({"item": n, "buy_faith": cost})
        if faith_only:
            d["buy_with_faith"] = faith_only
        d["tiles"] = []
        for i in cm.city_tiles(g, c):
            ty = T.tile_stats(g, i, c.owner, c)
            d["tiles"].append({"x": g.grid.xy(i)[0], "y": g.grid.xy(i)[1], "worked": i in c.worked or i == c.idx,
                               "yields": _round(ty)})
        buy = []
        for i in cm.choosable_tiles(g, c):
            if cm.can_buy_tile(g, c, i) is None:
                buy.append({"x": g.grid.xy(i)[0], "y": g.grid.xy(i)[1], "gold": cm.buy_tile_cost(g, c, i)})
        d["buyable_tiles"] = sorted(buy, key=lambda b: b["gold"])[:24]
        if g.religion_enabled:
            from .religion import followers, display_name
            f = followers(g, c)
            d["religious_followers"] = {display_name(g, k): v for k, v in f.items() if v}
    return d


# ----------------------------------------------------------------------------
# Tiles
# ----------------------------------------------------------------------------
def _tile_state(g: Game, idx: int, viewer: Optional[int]) -> tuple:
    """(features, improvement, route, owner, pillaged, route_pillaged) as the viewer knows them."""
    t = g.s.tiles[idx]
    if viewer is None or can_see(g, viewer, idx):
        return list(t.features), t.improvement, t.route, t.owner, t.pillaged, t.route_pillaged
    mem = g.player(viewer).memory.get(idx)
    if isinstance(mem, dict):
        return mem.get("f", list(t.features)), mem.get("i"), mem.get("r"), mem.get("o"), mem.get("p", False), False
    return list(t.features), None, None, None, False, False


def tile_info(g: Game, idx: int, viewer: Optional[int]) -> dict:
    """A tile as a viewer sees it, honouring what they have explored."""
    x, y = g.grid.xy(idx)
    if not knows_tile(g, viewer, idx):
        return {"x": x, "y": y, "explored": False}
    t = g.s.tiles[idx]
    visible = can_see(g, viewer, idx)
    feats, imp, route, owner, pill, rpill = _tile_state(g, idx, viewer)
    d = {"x": x, "y": y, "explored": True, "visible": visible, "terrain": t.terrain, "features": feats}
    if t.wonder:
        d["natural_wonder"] = t.wonder
    if t.river:
        d["river_edges"] = [n for k, n in enumerate(("E", "NE", "NW", "W", "SW", "SE")) if t.river & (1 << k)]
    if imp:
        d["improvement"] = imp + (" (pillaged)" if pill else "")
    if route:
        d["route"] = route + (" (pillaged)" if rpill else "")
    if owner is not None:
        d["owner"] = owner
    if t.resource and T.resource_visible(g, viewer, t.resource):
        d["resource"] = t.resource
        if t.resource_amount:
            d["resource_amount"] = t.resource_amount
    d["yields"] = _round(T.tile_stats(g, idx, viewer))
    d["movement_cost"] = max([g.rules.terrains[x_].get("movementCost", 1) for x_ in T.all_terrains(t)] or [1])
    d["defense_bonus_percent"] = int(round(sum(g.rules.terrains[x_].get("defenceBonus", 0) for x_ in T.all_terrains(t)) * 100))
    c = g.city_at(idx)
    if c and (visible or viewer is None):
        d["city"] = {"id": c.id, "name": c.name, "owner": c.owner}
    if visible or viewer is None:
        from .visibility import unit_visible_to
        units = [u for u in g.units_at(idx) if viewer is None or unit_visible_to(g, viewer, u)]
        if units:
            d["units"] = [unit_info(g, u, viewer) for u in units]
    if t.build and (viewer is None or owner == viewer):
        d["work_in_progress"] = [{"improvement": n, "turns_left": k} for n, k in t.build if k >= 0]
    return d


# ----------------------------------------------------------------------------
# Players & empire
# ----------------------------------------------------------------------------
def empire_info(g: Game, pid: int) -> dict:
    """The empire summary: yields, happiness, resources, score and victory progress."""
    from . import economy, research, victory, great_people
    p = g.player(pid)
    st = economy.civ_stats(g, pid)
    sm = economy.stat_map(g, pid)
    hap = economy.happiness(g, pid)
    strat = {k: v for k, v in economy.strategic_resources(g, pid).items()
             if T.resource_visible(g, pid, k) and any(v.values())}
    lux = {k: v for k, v in economy.luxury_resources(g, pid).items() if any(v.values())}
    cur = research.current(g, pid)
    d = {
        "id": pid, "name": p.name, "leader": p.leader, "nation": p.nation, "color": p.color,
        "gold": int(p.gold), "culture": int(p.culture), "faith": int(p.faith),
        "per_turn": _round({k: st[k] for k in ("gold", "science", "culture", "faith")}),
        "per_turn_breakdown": {src: _round({k: v for k, v in s.items() if k in ("gold", "science", "culture", "faith")})
                               for src, s in sm.items()},
        "happiness": hap,
        "golden_age": {"turns_left": p.golden_age_turns, "progress": int(p.golden_age_points),
                       "needed": great_people.happiness_for_golden_age(g, pid)},
        "researching": cur, "research_queue": list(p.research_queue),
        "research_progress": int(p.research_progress.get(cur, 0)) if cur else 0,
        "research_cost": research.tech_cost(g, pid, cur) if cur else None,
        "research_turns": research.turns_left(g, pid) if cur else None,
        "techs_known": len(p.techs), "free_techs": p.free_techs, "future_techs": p.future_techs,
        "era": g.rules.era_list[research.player_era(g, pid)],
        "strategic_resources": strat, "luxuries": lux,
        "unit_supply": {"units": len(g.player_units(pid)), "supply": economy.unit_supply(g, pid),
                        "production_penalty_percent": int(economy.unit_supply_penalty(g, pid))},
        "score": victory.score(g, pid)["total"], "capital": p.capital,
        "policies": list(p.policies), "free_policies": p.free_policies,
        "free_great_people": p.free_great_people,
    }
    if g.religion_enabled:
        from .religion import display_name
        d["religion"] = {"state": p.religion_state, "name": display_name(g, p.religion) if p.religion else None}
    if g.victory_enabled("Scientific"):
        d["spaceship"] = victory.spaceship_status(g, pid)
    return d


def players_overview(g: Game, viewer: Optional[int]) -> list[dict]:
    """Everyone the viewer has met, with what they know about each."""
    from . import victory, research, city_states
    out = []
    for p in g.s.players:
        met = viewer is None or p.id == viewer or g.has_met(viewer, p.id) or p.kind == "barbarian" \
            or g.s.phase != "playing"
        d = {"id": p.id, "kind": p.kind, "color": p.color, "alive": p.alive, "met": met}
        if met:
            d.update({"name": p.name, "leader": p.leader})
            if p.kind == "major":
                d["nation"] = p.nation
                d["difficulty"] = p.difficulty or g.s.config.get("difficulty")
                d["score"] = victory.score(g, p.id)["total"]
                d["era"] = g.rules.era_list[research.player_era(g, p.id)]
                d["cities"] = len(g.player_cities(p.id))
            elif p.kind == "city_state":
                d["city_state_type"] = p.cs_type
                if viewer is not None:
                    d["influence"] = round(city_states.influence(g, p.id, viewer), 1)
                    d["relationship"] = city_states.relationship(g, p.id, viewer)
                d["ally"] = g.player(p.ally).name if p.ally is not None and (viewer is None or g.has_met(viewer, p.ally) or p.ally == viewer) else (None if p.ally is None else "unknown")
            if viewer is not None and p.id != viewer and p.kind != "barbarian":
                rel = g.relation(viewer, p.id) or {}
                d["at_war"] = bool(rel.get("war"))
                if rel.get("treaty_until", 0) >= g.turn and not rel.get("war"):
                    d["peace_treaty_until"] = rel["treaty_until"]
                if p.kind == "major":
                    d.update(_agreements(g, viewer, p.id))
        else:
            d["name"] = "Unknown civilization" if p.kind == "major" else "Unknown city-state"
        out.append(d)
    return out


def _agreements(g: Game, me: int, other: int) -> dict:
    """The agreements in force between two civilizations."""
    from . import diplomacy
    rel = g.relation(me, other) or {}
    out = {}
    if g.s.open_borders.get(f"{me}>{other}"):
        out["open_borders_you_grant_until"] = g.s.open_borders[f"{me}>{other}"]
    if g.s.open_borders.get(f"{other}>{me}"):
        out["open_borders_they_grant_until"] = g.s.open_borders[f"{other}>{me}"]
    out["embassy_in_their_capital"] = diplomacy.has_embassy(g, me, other)
    out["their_embassy_with_you"] = diplomacy.has_embassy(g, other, me)
    if diplomacy.is_friends(g, me, other):
        out["friendship_until"] = rel.get("friendship_until")
    if diplomacy.has_pact(g, me, other):
        out["defensive_pact_until"] = rel.get("pact_until")
    if rel.get("ra_until", 0) >= g.turn:
        out["research_agreement_until"] = rel.get("ra_until")
    if diplomacy.denounced(g, me, other):
        out["you_denounced_them"] = True
    if diplomacy.denounced(g, other, me):
        out["they_denounced_you"] = True
    return out


def trade_options(g: Game, pid: int, other: int) -> dict:
    """What each side has that the other might want, for the deal builder's suggestions."""
    from . import economy, research, diplomacy
    lux_me, lux_them = economy.luxury_resources(g, pid), economy.luxury_resources(g, other)
    strat_me, strat_them = economy.strategic_resources(g, pid), economy.strategic_resources(g, other)
    out = {
        "their_gold": int(g.player(other).gold),
        "they_could_give": {"luxuries": sorted(r for r, v in lux_them.items() if v["net"] >= 2 and lux_me[r]["net"] <= 0),
                            "strategic": {r: v["available"] for r, v in strat_them.items() if v["available"] > 0
                                          and T.resource_visible(g, pid, r)}},
        "you_could_give": {"luxuries": sorted(r for r, v in lux_me.items() if v["net"] >= 2 and lux_them[r]["net"] <= 0),
                           "strategic": {r: v["available"] for r, v in strat_me.items() if v["available"] > 0}},
    }
    if g.s.config.get("tech_trading", True):
        out["they_could_give"]["techs"] = sorted(t for t in g.player(other).techs if research.can_research(g, pid, t))
        out["you_could_give"]["techs"] = sorted(t for t in g.player(pid).techs if research.can_research(g, other, t))
    possible = []
    for t in ("embassy", "open_borders", "declaration_of_friendship", "research_agreement", "defensive_pact"):
        try:
            prop = {str(pid): [{"type": t}], str(other): [{"type": t}]}
            items = diplomacy._normalize_items(g, [{"type": t}])
            diplomacy.validate_items(g, pid, other, items, prop)
            diplomacy.validate_items(g, other, pid, items, prop)
            possible.append(t)
        except ActionError:
            pass
    out["agreements_possible_now"] = possible
    if "research_agreement" in possible:
        out["research_agreement_cost_each"] = diplomacy.ra_cost(g, pid, other)
    return out


def diplomacy_info(g: Game, pid: int, message_limit: int = 30) -> dict:
    """Relations, negotiations, deals and recent messages, from one civilization's view."""
    from .diplomacy import negotiation_view, describe_items
    negs = [negotiation_view(g, n, pid) for n in g.s.negotiations if pid in (n["initiator"], n["responder"])]
    open_negs = [n for n in negs if n["status"] == "open"]
    recent = [n for n in negs if n["status"] != "open"][-5:]
    deals = []
    for d in g.s.deals:
        if pid not in d["parties"] or not d.get("active"):
            continue
        other = d["parties"][0] if d["parties"][1] == pid else d["parties"][1]
        deals.append({"id": d["id"], "with": other, "with_name": g.player(other).name, "turn": d["turn"],
                      "you_give": describe_items(g, d["terms"][str(pid)]),
                      "you_receive": describe_items(g, d["terms"][str(other)]),
                      "ongoing": [{"type": it["type"], "resource": it.get("resource"), "amount": it.get("amount"),
                                   "direction": "out" if it["from"] == pid else "in", "until_turn": it["until"]}
                                  for it in d["ongoing"] if it["until"] >= g.turn]})
    msgs = [m for m in g.s.messages if m["from"] == pid or pid in m["to"]][-message_limit:]
    players = [p for p in players_overview(g, pid) if p["id"] != pid and p["kind"] == "major" and p["met"]]
    for p in players:
        if p.get("alive"):
            p["trade_options"] = trade_options(g, pid, p["id"])
    out = {"players": players, "open_negotiations": open_negs, "recent_negotiations": recent, "active_deals": deals,
           "messages": [{"turn": m["turn"], "from": m["from"], "from_name": g.player(m["from"]).name,
                         "to": [g.player(t).name for t in m["to"]], "text": m["text"]} for m in msgs]}
    from .victory import _un, vote_open
    un = _un(g)
    if un.get("next_vote") is not None:
        out["united_nations"] = {"next_vote_turn": un["next_vote"], "voting_open": vote_open(g),
                                 "your_vote": un["votes"].get(str(pid), "not cast") if str(pid) in un["votes"] else "not cast",
                                 "last_result": un.get("results")}
    return out


def city_states_info(g: Game, pid: int) -> list[dict]:
    """Met city-states with influence, allies, quests and tribute."""
    from . import city_states as C
    out = []
    for q in g.city_states():
        if not g.has_met(pid, q.id):
            continue
        rel = C.relationship(g, q.id, pid)
        ct = g.rules.city_state_types.get(q.cs_type, {})
        d = {"id": q.id, "name": q.name, "type": q.cs_type, "personality": q.cs_personality,
             "influence": round(C.influence(g, q.id, pid), 1), "resting_point": C.resting_point(g, q.id, pid),
             "relationship": rel, "ally": g.player(q.ally).name if q.ally is not None else None,
             "you_protect": pid in q.protectors, "at_war": g.at_war(pid, q.id),
             "friend_bonuses": [x.text for x in ct.get("_friend", {}).all] if ct else [],
             "ally_bonuses": [x.text for x in ct.get("_ally", {}).all] if ct else []}
        cap = g.city(q.capital) if q.capital is not None else None
        if cap is not None and g.player(pid).explored[cap.idx]:
            d["capital"] = {"city_id": cap.id, "x": g.grid.xy(cap.idx)[0], "y": g.grid.xy(cap.idx)[1]}
        if q.cs_resource:
            d["unique_luxury"] = q.cs_resource
        if q.cs_unique_unit:
            d["gifts_unit"] = q.cs_unique_unit
        d["quests"] = [C.quest_text(g, x) for x in C.quests_for(g, pid) if x.get("cs") == q.id] \
            if hasattr(C, "quests_for") else []
        will = C.tribute_willingness(g, q.id, pid)
        d["tribute"] = {"would_pay": will > 0, "gold": C.tribute_gold_amount(g) if will > 0 else None,
                        "willingness": will}
        out.append(d)
    return out


def tech_tree(g: Game, pid: int) -> dict:
    """The technology tree with each technology's status, cost and what it unlocks."""
    from . import research
    p = g.player(pid)
    out = []
    for name, t in g.rules.techs.items():
        status = "known" if g.has_tech(pid, name) else ("available" if research.can_research(g, pid, name) else "locked")
        row = {"name": name, "era": t["era"], "status": status, "cost": research.tech_cost(g, pid, name),
               "prerequisites": t["prerequisites"]}
        if p.research_progress.get(name):
            row["progress"] = int(p.research_progress[name])
        un = g.rules.unlocks.get(name, {})
        if any(un.values()):
            row["unlocks"] = {k: v for k, v in un.items() if v}
        if _uniques(t):
            row["effects"] = _uniques(t)
        out.append(row)
    return {"researching": research.current(g, pid), "queue": list(p.research_queue), "techs": out}


def policies_info(g: Game, pid: int) -> dict:
    """Social policies: culture, costs, branches and what can be adopted."""
    from . import policies as P
    p = g.player(pid)
    R = g.rules
    eligible = set(P.adoptable_policies(g, pid))
    adoptable = eligible if P.can_adopt_any(g, pid) else set()
    branches = []
    for bname, b in R.policy_branches.items():
        status = "completed" if f"{bname} Complete" in p.policies else (
            "open" if bname in p.policies else ("adoptable" if bname in adoptable else
                                                ("available" if bname in eligible else "locked")))
        branches.append({"branch": bname, "era": b.get("era"), "status": status, "opening_effects": _uniques(b),
                         "policies": [{"name": m, "adopted": m in p.policies, "adoptable": m in adoptable,
                                       "requires": R.policies[m].get("requires", []),
                                       "effects": _uniques(R.policies[m])} for m in b["members"]],
                         "completion_effects": _uniques(R.policies.get(f"{bname} Complete", {}))})
    return {"culture": int(p.culture), "next_policy_cost": P.culture_cost(g, pid), "free_policies": p.free_policies,
            "adopted": list(p.policies), "adoptable_now": sorted(adoptable), "branches": branches}


def religion_info(g: Game, pid: int) -> dict:
    """Faith, pantheon and religion, available beliefs, and what faith can buy."""
    from . import religion as Rl
    p = g.player(pid)
    if not g.religion_enabled:
        return {"enabled": False}
    d = {"enabled": True, "faith": int(p.faith), "state": p.religion_state,
         "your_religion": Rl.display_name(g, p.religion) if p.religion else None}
    if p.religion:
        d["your_beliefs"] = Rl.all_beliefs(g, p.religion)
    if Rl.can_found_pantheon(g, pid) is None or p.religion_state == "none":
        d["faith_for_pantheon"] = Rl.faith_for_pantheon(g, pid)
        d["pantheon_beliefs_available"] = {b: _uniques(g.rules.beliefs[b]) for b in Rl.beliefs_available(g, "Pantheon")}
    if Rl.can_generate_prophet(g, pid, ignore_faith=True):
        d["faith_for_next_great_prophet"] = Rl.faith_for_next_prophet(g, pid)
    d["religions_remaining_to_found"] = Rl.remaining_foundable(g)
    if p.religion_state in ("pantheon", "founding", "religion", "enhancing"):
        need = Rl.beliefs_to_choose(g, pid, enhancing=p.religion_state in ("religion", "enhancing"))
        d["beliefs_to_choose_when_founding_or_enhancing"] = need
        d["available_beliefs"] = {bt: {b: _uniques(g.rules.beliefs[b]) for b in Rl.beliefs_available(g, bt)}
                                  for bt in need if bt != "Any"}
    world = []
    for name, r in g.s.religions.items():
        if not Rl.is_major(g, name):
            continue
        world.append({"religion": Rl.display_name(g, name), "founder": g.player(r["founder"]).name
                      if g.has_met(pid, r["founder"]) or r["founder"] == pid else "unknown",
                      "cities_following": Rl.cities_following(g, name),
                      "beliefs": Rl.all_beliefs(g, name)})
    d["world_religions"] = world
    return d


def great_people_info(g: Game, pid: int) -> dict:
    """Great-person progress and golden age status."""
    from . import great_people as GP
    p = g.player(pid)
    pools = []
    for gp in GP.great_people_types(g, pid):
        key = GP.pool_key(g, pid, gp)
        pts = p.gp_points.get(key, 0) if key in p.gp_points else p.gg_points.get(key, 0)
        pools.append({"great_person": gp, "points": int(pts), "needed": int(GP.points_required(g, pid, gp))})
    per_turn: dict = {}
    for c in g.player_cities(pid):
        for k, v in GP.city_gpp(g, c).items():
            per_turn[k] = per_turn.get(k, 0) + v
    return {"progress": pools, "points_per_turn": _round(per_turn), "free_great_people_to_choose": p.free_great_people,
            "great_people_earned": p.great_people_earned,
            "golden_age": {"turns_left": p.golden_age_turns, "progress": int(p.golden_age_points),
                           "needed": GP.happiness_for_golden_age(g, pid)}}


def victory_info(g: Game, pid: int) -> dict:
    """Progress toward every enabled victory condition."""
    from . import victory as V
    known = [q for q in g.majors(alive_only=False) if q.id == pid or g.has_met(pid, q.id)]
    d = {"turn": g.turn, "year": g.year_text(), "turn_limit": g.total_turns(),
         "enabled_victories": V.enabled_victories(g), "your_progress": V.victory_progress(g, pid),
         "your_score": V.score(g, pid),
         "scores": {q.name: V.score(g, q.id)["total"] for q in known if q.alive},
         "original_capitals": [{"city": c.name, "owner": g.player(c.owner).name
                                if g.has_met(pid, c.owner) or c.owner == pid else "unknown"}
                               for c in g.s.cities.values() if c.original_capital
                               and g.player(c.founder).kind == "major" and g.player(pid).explored[c.idx]]}
    if g.victory_enabled("Scientific"):
        d["spaceship"] = V.spaceship_status(g, pid)
    un = V._un(g)
    if un.get("next_vote") is not None:
        d["united_nations"] = {"next_vote_turn": un["next_vote"], "votes_needed": V.votes_needed(g),
                               "last_result": un.get("results")}
    return d


# ----------------------------------------------------------------------------
# Rules lookup
# ----------------------------------------------------------------------------
def _public(d: dict) -> dict:
    """Strip the internal fields from a ruleset object before it goes to a client."""
    return {k: v for k, v in d.items() if not k.startswith("_")}


def rules_lookup(g: Game, topic: str, name: Optional[str] = None):
    """Look up the ruleset: a whole topic, or one entry."""
    from .diplomacy import ITEM_TYPES
    R = g.rules
    topic = (topic or "").lower().strip()
    tables = {"units": ("unit", R.units), "buildings": ("building", R.buildings), "techs": ("tech", R.techs),
              "improvements": ("improvement", R.improvements), "resources": ("resource", R.resources),
              "promotions": ("promotion", R.promotions), "terrains": ("terrain", R.terrains),
              "terrain": ("terrain", R.terrains), "policies": ("policy", {**R.policy_branches, **R.policies}),
              "beliefs": ("belief", R.beliefs), "specialists": ("specialist", R.specialists),
              "eras": ("era", R.eras), "nations": ("nation", R.nations),
              "city_state_types": (None, R.city_state_types), "speeds": ("speed", R.speeds),
              "difficulties": ("difficulty", R.difficulties)}
    if topic == "deal_items":
        return ITEM_TYPES
    if topic == "combat":
        return RULES_COMBAT
    if topic == "overview":
        return RULES_OVERVIEW
    if topic not in tables:
        raise ActionError(f"Unknown topic '{topic}'. Topics: {', '.join(list(tables) + ['deal_items', 'combat', 'overview'])}.")
    kind, table = tables[topic]
    if name:
        key = R.resolve(kind, name) if kind else (name if name in table else None)
        if key is None or key not in table:
            raise ActionError(f"No {topic} entry '{name}'.")
        entry = _public(table[key])
        if topic == "techs":
            entry["unlocks"] = {k: v for k, v in R.unlocks.get(key, {}).items() if v}
        if topic == "city_state_types":
            entry = {"friend_bonuses": [x.text for x in table[key]["_friend"].all],
                     "ally_bonuses": [x.text for x in table[key]["_ally"].all]}
        return entry
    if topic == "units":
        return {k: {"cost": v.get("cost"), "strength": v.get("strength", 0), "ranged": v.get("rangedStrength", 0),
                    "range": v.get("range", 0), "movement": v.get("movement"), "type": v.get("unitType"),
                    "tech": v.get("requiredTech"), "resource": v.get("requiredResource"),
                    "unique_to": v.get("uniqueTo"), "upgrades_to": v.get("upgradesTo")} for k, v in table.items()}
    if topic == "buildings":
        return {k: {"cost": v.get("cost"), "tech": v.get("requiredTech"), "maintenance": v.get("maintenance", 0),
                    "wonder": bool(v.get("isWonder")), "national_wonder": bool(v.get("isNationalWonder")),
                    "unique_to": v.get("uniqueTo")} for k, v in table.items()}
    if topic == "techs":
        return {k: {"era": v["era"], "cost": v.get("cost"), "prerequisites": v["prerequisites"]} for k, v in table.items()}
    if topic in ("nations",):
        return {k: {"kind": v.get("kind"), "leader": v.get("leaderName"), "start_bias": v.get("startBias", []),
                    "uniques": v.get("uniques", [])} for k, v in table.items()}
    return {k: _public(v) for k, v in table.items()} if topic not in ("city_state_types",) else list(table)


RULES_OVERVIEW = """CITAR rules overview. The rules and numbers are UnCiv's "Civ V - Gods & Kings" ruleset.
- Hex map, odd-r offset coordinates (x=column, y=row). Fog of war: you only see near your units and territory.
- Turns are sequential. On your turn: choose research, set city production, move units, adopt policies, negotiate,
  then end_turn. Game speed (Quick/Standard/Epic/Marathon) scales costs and the turn limit.
- One military unit per tile (plus one civilian). Units have 100 HP. Melee attackers take counter-damage; ranged don't.
- Cities: found with Settlers (at least 3 tiles apart). Cities grow with surplus food, produce units, buildings and
  wonders, expand borders with culture, bombard nearby enemies, and are captured by melee units at 0 HP. Captured
  cities start as puppets; you may annex, raze or liberate them (city_status).
- Workers improve tiles (farms, mines, pastures...), build roads/railroads, and connect cities to the capital.
- Resources: strategic ones (Horses, Iron, Coal, Oil, Aluminum, Uranium) are revealed by techs and are needed by
  some units and buildings; each luxury type gives +4 happiness (and demand for We Love The King Day).
- Happiness: each city -3 and each citizen -1 (modified by difficulty); unhappy empires grow slowly, very unhappy
  empires stop growing and fight worse. Positive happiness accumulates toward Golden Ages.
- Culture buys social policies (10 branches; completing a branch gives a bonus). Faith founds a pantheon, earns
  Great Prophets who found and enhance religions, and buys religious units and some buildings.
- Great People come from specialists and wonders (Scientist, Engineer, Merchant, Artist, Prophet) and from combat
  (Generals, Admirals); each has a special action (unit_action).
- City-states: raise influence with gold, units and quests to become their Friend (bonuses) or Ally (more bonuses,
  their resources, their votes). Espionage: spies steal techs, rig city-state elections, stage coups.
- Diplomacy: messages are free and non-binding; deals made through negotiations are binding (gold, resources,
  open borders, embassies, friendship, research agreements, defensive pacts, peace, cities, techs).
- Victory: Domination (hold every original capital), Scientific (Apollo Program, then launch spaceship parts in your
  capital), Cultural (complete 5 policy branches and build the Utopia Project), Diplomatic (win the United Nations
  vote), or the highest score at the turn limit (Time).
"""

RULES_COMBAT = """Combat (UnCiv formulas): damage to the defender = 24 + 12r (at most) scaled by the strength ratio, with
randomness; wounded units fight worse. Modifiers: terrain defense (hills/forest/jungle +25%, marsh -15%),
fortification (+20% per turn, max +40%), flanking (+10% per adjacent friendly unit), attacking across a river or from
the sea -20%, Great General +15% within 2 tiles, promotions and unit-specific bonuses (e.g. Spearman vs mounted),
city strength from population, techs, garrison and walls. Ranged attacks need line of sight (unless indirect fire).
XP: melee attack 5, defend 4; ranged 2 (3 vs cities); capped at 30 vs barbarians. Promotions at 10, 30, 60, 100 XP...
Cities heal 20 HP per turn (less while under siege) and capture requires a melee unit."""


# ----------------------------------------------------------------------------
# Full client view
# ----------------------------------------------------------------------------
def client_view(g: Game, pid: Optional[int], event_limit: int = 150) -> dict:
    """The entire game as one player sees it, which is what the browser renders from."""
    vis = _visible(g, pid)
    tiles = []
    for idx, t in enumerate(g.s.tiles):
        if pid is not None and not g.player(pid).explored[idx]:
            continue
        visible = vis is None or idx in vis
        feats, imp, route, owner, pill, rpill = _tile_state(g, idx, pid)
        res = t.resource if (t.resource and T.resource_visible(g, pid, t.resource)) else None
        tiles.append([idx, t.terrain, feats, t.wonder, int(t.river or 0), res, imp, route, 1 if pill else 0,
                      1 if rpill else 0, owner, 1 if visible else 0])
    from .visibility import unit_visible_to
    units = [unit_info(g, u, pid) for u in g.s.units.values()
             if vis is None or (u.idx in vis and unit_visible_to(g, pid, u))]
    cities = []
    for c in g.s.cities.values():
        if vis is None or c.idx in vis:
            cities.append(city_info(g, c, pid))
        elif g.player(pid).explored[c.idx]:
            mem = g.player(pid).memory.get(c.idx)
            if isinstance(mem, dict) and mem.get("c"):
                x, y = g.grid.xy(c.idx)
                name, pop, owner = mem["c"]
                cities.append({"id": c.id, "name": name, "owner": owner, "x": x, "y": y, "pop": pop, "stale": True})
    view = {
        "turn": g.turn, "year": g.year_text(), "current_player": g.s.current, "phase": g.s.phase,
        "winner": g.s.winner, "victory": g.s.victory, "you": pid, "width": g.s.width, "height": g.s.height,
        "tiles": tiles, "units": units, "cities": cities, "players": players_overview(g, pid),
        "turn_limit": g.total_turns(),
        "config": {k: g.s.config.get(k) for k in ("map_size", "map_type", "speed", "difficulty", "barbarian_difficulty", "barbarians",
                                                  "turn_limit", "victories", "tech_trading", "religion", "espionage")},
        "events": g.events_for(pid)[-event_limit:],
    }
    if pid is not None:
        from .briefing import alert_items
        view["empire"] = empire_info(g, pid)
        view["diplomacy"] = diplomacy_info(g, pid)
        view["notes"] = g.player(pid).notes
        view["alerts"] = ([{k: v for k, v in a.items() if k != "llm"} for a in alert_items(g, pid)]
                          if g.player(pid).alive else [])
    else:
        view["stats"] = g.s.stats[-1] if g.s.stats else None
        view["empires"] = {p.id: empire_info(g, p.id) for p in g.majors()}
        view["thoughts"] = g.s.thoughts[-80:]
        view["messages"] = g.s.messages[-80:]
        view["negotiations"] = g.s.negotiations[-20:]
    return view
