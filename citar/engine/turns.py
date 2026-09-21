"""Turn processing order. Follows UnCiv's TurnManager.startTurn / endTurn (MPL-2.0):

start of a civ's turn: research progress, great people births, religion (prophets), Maya calendar, visibility,
flags (city-state great person gifts, revolts), "upon turn start" triggers, cities' start of turn, units' start of turn.

end of a civ's turn: "upon turn end" triggers, culture -> policies, city-state quests, bankruptcy + gold, science,
faith, spies, great person points, cities' end of turn (production, borders, growth, razing), timed uniques, golden
ages, worker builds, units' end of turn (healing, fortification).
"""
from __future__ import annotations

from typing import TYPE_CHECKING

from . import unique_types as U

if TYPE_CHECKING:
    from .game import Game


def start_player_turn(g: "Game", pid: int):
    """Everything that happens when a player's turn begins.

    Units refresh and carry out standing orders, cities produce and grow, research and culture
    accumulate, religion and espionage advance, and the notifications the player will read are
    generated. By the time control reaches the player, the world has already moved.
    """
    from . import (barbarians, units, cities, research, automation, visibility, great_people, religion, city_states,
                   triggers, victory)
    p = g.player(pid)
    if not p.alive:
        return
    if p.kind == "barbarian":
        for u in list(g.player_units(pid)):
            units.start_turn(g, u)
        barbarians.take_turn(g)
        visibility.refresh(g)
        return
    g.invalidate()
    if g.player_cities(pid):
        research.update_research_progress(g, pid)
        great_people.start_turn(g, pid)
        if g.religion_enabled:
            religion.start_turn(g, pid)
        great_people.maya_long_count(g, pid)
    visibility.refresh(g)
    if p.kind == "major":
        city_states.great_person_gift_tick(g, pid)
        _update_revolts(g, pid)
    triggers.fire(g, pid, U.TriggerUponTurnStart)
    for c in list(g.player_cities(pid)):
        if g.city(c.id) is not None:
            cities.start_turn(g, c)
    for u in list(g.player_units(pid)):
        if u.id in g.s.units:
            units.start_turn(g, u)
    g.invalidate()
    if p.kind == "city_state":
        city_states.take_turn(g, pid)
    else:
        automation.run_unit_orders(g, pid)
    visibility.refresh(g)
    victory.check_victory(g, pid)
    if p.kind == "major" and p.alive and not p.research_queue and research.available_techs(g, pid) \
            and g.player_cities(pid):
        g.emit("research_needed", "Choose a technology to research.", [pid])
    if p.kind == "major":
        g.emit("turn_start", f"Turn {g.turn} ({g.year_text()}): {p.name}'s turn.", None, player=pid)


def end_player_turn(g: "Game", pid: int):
    """Everything that happens when a player's turn ends, and hand over to the next."""
    from . import (diplomacy, economy, policies, research, religion, espionage, great_people, cities, units, workers,
                   city_states, triggers, victory)
    p = g.player(pid)
    if p.kind == "major":
        diplomacy.expire_negotiations(g, pid)
    if not p.alive:
        return
    if p.kind == "barbarian":
        for u in list(g.player_units(pid)):
            if u.id in g.s.units:
                units.end_turn(g, u)
        return
    triggers.fire(g, pid, U.TriggerUponTurnEnd)
    g.invalidate()
    stats = economy.civ_stats(g, pid)
    culture, science, faith, gold = stats["culture"], stats["science"], stats["faith"], stats["gold"]
    p.flags["last_stats"] = {k: round(v, 1) for k, v in stats.items()}
    policies.end_turn(g, pid, culture)
    p.flags["total_culture"] = p.flags.get("total_culture", 0) + int(culture)
    if p.kind == "city_state":
        city_states.end_turn(g, pid)
    economy.process_gold(g, pid, gold)
    if g.player_cities(pid):
        research.end_turn(g, pid, science)
    if g.religion_enabled:
        religion.end_turn(g, pid, faith)
    p.flags["total_faith"] = p.flags.get("total_faith", 0) + int(faith)
    if p.kind == "major":
        espionage.end_turn(g, pid)
        great_people.end_turn(g, pid)
    for c in sorted(g.player_cities(pid), key=lambda c: not c.razing):
        if g.city(c.id) is not None:
            cities.end_turn(g, c)
    _temp_uniques_end_turn(g, pid)
    g.invalidate()
    if p.kind == "major":
        great_people.golden_age_end_turn(g, pid, economy.happiness(g, pid)["total"])
    workers.progress_builds(g, pid)
    for u in list(g.player_units(pid)):
        if u.id in g.s.units:
            units.end_turn(g, u)
    g.invalidate()
    victory.check_victory(g, pid)
    if p.kind == "major":
        g.emit("turn_end", f"{p.name} ended their turn.", None, player=pid)


def _temp_uniques_end_turn(g: "Game", pid: int):
    """Expire temporary uniques whose duration has run out."""
    p = g.player(pid)
    keep = []
    for t in p.temp_uniques:
        t["turns"] -= 1
        if t["turns"] > 0:
            keep.append(t)
    if len(keep) != len(p.temp_uniques):
        p.temp_uniques = keep
        g.invalidate()


def _update_revolts(g: "Game", pid: int):
    """TurnManager.updateRevolts / doRevoltSpawn: very unhappy civs spawn rebel (barbarian) units."""
    p = g.player(pid)
    barb = next((q for q in g.s.players if q.kind == "barbarian"), None)
    if barb is None or not g.civ_has(pid, U.SpawnRebels):
        p.flags.pop("revolt_in", None)
        return
    if "revolt_in" not in p.flags:
        rng = g.state_rng("revolt_delay", pid, g.turn)
        p.flags["revolt_in"] = max(1, int((g.rules.k["base_turns_until_revolt"] + rng.randrange(3))
                                          * max(1.0, g.speed["modifier"])))
        return
    p.flags["revolt_in"] -= 1
    if p.flags["revolt_in"] > 0:
        return
    del p.flags["revolt_in"]
    _spawn_revolt(g, pid, barb.id)


def _spawn_revolt(g: "Game", pid: int, barb: int):
    """Spawn rebels in a civilization that is deeply unhappy."""
    from .cities import city_tiles
    from .combat import tile_defense_bonus
    from . import tiles as T
    from .units import place_unit_near
    rng = g.state_rng("revolt", pid, g.turn)
    cs = g.player_cities(pid)
    if not cs:
        return
    count = 1 + rng.randrange(100 + 20 * (len(cs) - 1)) // 100
    city = max(cs, key=lambda c: rng.randrange(c.pop + 10))

    def rate(i):
        """How likely a revolt is, given how unhappy things are."""
        t = g.s.tiles[i]
        if T.is_water(g, i) or g.units_at(i) or g.city_at(i) is not None or T.is_impassable(g, i):
            return -1
        s = 10
        if not t.improvement:
            s += 4 + (3 if t.resource else 0)
        if tile_defense_bonus(g, i) > 0:
            s += 4
        return s
    tile = max(city_tiles(g, city), key=rate)
    R = g.rules
    options = [n for n, ud in R.units.items() if not ud.get("uniqueTo") and ud["_melee"] and ud["_domain"] == "Land"
               and not ud["_umap"].has_tag(U.CannotAttack) and g.has_tech(pid, ud.get("requiredTech"))
               and not (ud.get("obsoleteTech") and g.has_tech(pid, ud["obsoleteTech"]))]
    if not options:
        return
    utype = max(options, key=lambda n: rng.randrange(1000))
    for _ in range(count):
        place_unit_near(g, barb, utype, tile)
    g.emit("revolt", "Your citizens are revolting due to very high unhappiness!", [pid], idx=tile)


def end_round(g: "Game"):
    """Everything that happens once per round, after every player has moved."""
    from . import diplomacy, victory
    for p in g.s.players:
        if p.alive and p.kind != "barbarian":
            victory.check_elimination(g, p.id)
    if g.s.phase != "playing":
        return
    diplomacy.process_round(g)
    victory.record_stats(g)
    victory.record_frame(g)
    g.s.turn += 1
    victory.end_round(g)
