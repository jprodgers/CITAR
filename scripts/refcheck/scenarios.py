"""Scenario setups for the reference corpus: states that ordinary early bot games rarely reach.

A setup edits a running all-bot game at the start of a round, when the first major civilization's turn has just
begun, using the scenario editor's operations (citar.engine.scenario) and the same tools a player calls. It then
hands the game back to the bots, and record.py keeps taking checkpoints, so the corpus holds both the edited state
and what the rules made of it a few turns later.

Every step is attempted and logged in the case's meta ("setup"), and a step that fails does not stop the others: a
map where, say, no tile next to the rival's town is free still yields a useful state.
"""
from __future__ import annotations

import common


def _log(steps: list, name: str, fn):
    """Run one setup step, recording its result or its failure (one line in the file, the traceback on the
    console)."""
    try:
        res = fn()
        steps.append({"step": name, "ok": True, "result": res})
        return res
    except Exception as e:
        steps.append({"step": name, "ok": False, "error": common.error_text(e)})
        common.print_trace(f"scenario step '{name}' failed:")
        return None


def _free_land_near(g, center: int, lo: int, hi: int, pid: int, utype: str):
    """A tile *lo* to *hi* steps from *center* where *pid* could stand a *utype*, nearest first."""
    from citar.engine import movement
    ud = g.rules.units[utype]
    for r in range(lo, hi + 1):
        for idx in g.grid.ring(center, r):
            if g.city_at(idx) is None and not g.units_at(idx) and movement.can_stand(g, pid, ud, idx):
                return idx
    return None


def _xy(g, idx: int) -> dict:
    """x and y of a tile, for scenario operations."""
    x, y = g.grid.xy(idx)
    return {"x": x, "y": y}


def world_war(g) -> list[dict]:
    """Two major civilizations in the Atomic era at war, with everything the plan wants a state to exercise.

    The civilization whose turn it is (A) and the next one (B) are advanced to the Information era's threshold and
    shown the whole map. A founds a religion in its capital and spreads it into B's territory, sends a spy to B's
    capital, builds the United Nations with a vote due next turn, declares war on B, captures a town B has just
    founded, drops an atomic bomb near B's capital (fallout, a damaged city), keeps a nuclear missile in reserve,
    and puts armies in contact on both fronts. B gets a spy in A's capital. Both get aircraft: A a bomber and a
    fighter, B a fighter and an anti-aircraft gun to intercept them.
    """
    from citar.engine import scenario as S, tools, religion, espionage, victory, cities as C, visibility
    steps: list[dict] = []
    majors = [p.id for p in g.majors()]
    a = g.s.current
    if a not in majors or len(majors) < 2:
        return [{"step": "choose players", "ok": False, "error": "needs two living majors, one of them to move"}]
    b = next(q for q in majors if q != a)
    cap_a = g.city(g.player(a).capital) if g.player(a).capital is not None else None
    cap_b = g.city(g.player(b).capital) if g.player(b).capital is not None else None
    if cap_a is None or cap_b is None:
        return [{"step": "choose players", "ok": False, "error": "both civilizations need a capital"}]
    steps.append({"step": "players", "ok": True, "result": {"a": a, "b": b, "capital_a": cap_a.id,
                                                             "capital_b": cap_b.id}})
    run = lambda name, fn: _log(steps, name, fn)  # noqa: E731

    run("advance eras", lambda: S.apply_ops(g, [
        {"op": "grant_era", "player": [a, b], "era": "Information era"},
        {"op": "set_player", "player": a, "gold": 20000, "faith": 3000},
        {"op": "set_player", "player": b, "gold": 5000, "faith": 1000},
        {"op": "reveal", "player": [a, b], "meet": True}]))

    def found_religion():
        """A founds a pantheon, then a Great Prophet in its capital founds a religion, beliefs chosen by the
        engine's own AI choice."""
        out = {}
        if g.player(a).religion_state == "none":
            belief = religion.ai_choose_beliefs(g, a, {"Pantheon": 1})[0]
            out["pantheon"] = tools.execute(g, a, "found_pantheon", {"belief": belief})
        prophet = S.apply_ops(g, [{"op": "add_unit", "player": a, "unit": "Great Prophet", **_xy(g, cap_a.idx)}])
        uid = prophet[0]["unit_ids"][0]
        beliefs = religion.ai_choose_beliefs(g, a, religion.beliefs_to_choose(g, a, False))
        out["religion"] = tools.execute(g, a, "unit_action", {"unit_id": uid, "action": "found_religion",
                                                              "name": "Refcheck", "beliefs": beliefs})
        return out
    if g.religion_enabled:
        run("found religion", found_religion)

    def missionary():
        """A missionary carrying A's religion, standing in B's capital's territory, spreads it there."""
        spot = next((i for i in g.grid.within(cap_b.idx, 2) if g.s.tiles[i].city == cap_b.id and
                     g.city_at(i) is None and not g.units_at(i) and g.is_land(i)), None)
        if spot is None:
            raise RuntimeError("no free land tile in B's capital's territory")
        res = S.apply_ops(g, [{"op": "add_unit", "player": a, "unit": "Missionary", **_xy(g, spot)}])
        u = g.unit(res[0]["unit_ids"][0])
        u.religion = g.player(a).religion
        return tools.execute(g, a, "unit_action", {"unit_id": u.id, "action": "spread_religion"})
    if g.religion_enabled and g.player(a).religion_state == "religion":
        run("spread religion", missionary)

    def spies():
        """A sends a spy to B's capital; B one to A's (moved directly: it is not B's turn)."""
        out = {}
        for pid, target in ((a, cap_b), (b, cap_a)):
            if not g.player(pid).spies:
                espionage.add_spy(g, pid)
            name = g.player(pid).spies[0]["name"]
            if pid == a:
                out[pid] = tools.execute(g, a, "move_spy", {"spy": name, "city_id": target.id})
            else:
                out[pid] = espionage.move_spy(g, pid, name, target.id)
        return out
    if g.espionage_enabled:
        run("spies", spies)

    def united_nations():
        """A completes the United Nations and the world leader vote opens now and is held next turn."""
        S.apply_ops(g, [{"op": "set_city", "city": cap_a.id, "add_buildings": ["United Nations"]}])
        victory.schedule_vote(g, a)
        g.s.un["next_vote"] = g.turn + 1
        return {"next_vote": g.s.un["next_vote"], "votes_needed": victory.votes_needed(g)}
    run("united nations", united_nations)

    run("declare war", lambda: S.apply_ops(g, [{"op": "set_relation", "a": a, "b": b, "state": "war"}]))

    def capture():
        """B founds a town beside its capital; its walls are down; A's infantry next to it takes it."""
        site = next((i for r in range(3, 7) for i in g.grid.ring(cap_b.idx, r) if C.found_check(g, b, i) is None),
                    None)
        if site is None:
            raise RuntimeError("no site for a new town of B's")
        # six citizens: a capture costs some, and a town left under four would be razed rather than puppeted
        town = S.apply_ops(g, [{"op": "found_city", "player": b, **_xy(g, site), "pop": 6}])[0]["city_id"]
        g.city(town).health = 1
        spot = _free_land_near(g, site, 1, 1, a, "Infantry")
        if spot is None:
            raise RuntimeError("no free tile beside B's town")
        uid = S.apply_ops(g, [{"op": "add_unit", "player": a, "unit": "Infantry", **_xy(g, spot)}])[0]["unit_ids"][0]
        visibility.refresh(g, force=True)
        res = tools.execute(g, a, "attack", {"unit_id": uid, **_xy(g, site)})
        return {"town": town, "owner_now": g.city(town).owner if g.city(town) else None, "attack": res}
    captured = run("capture a town", capture)

    def nukes():
        """An atomic bomb from the captured town (or A's capital) detonates two tiles from B's capital; a nuclear
        missile stays in A's capital."""
        base = cap_a
        if captured and captured.get("owner_now") == a:
            base = g.city(captured["town"])
        S.apply_ops(g, [{"op": "add_unit", "player": a, "unit": "Nuclear Missile", **_xy(g, cap_a.idx)}])
        bomb = S.apply_ops(g, [{"op": "add_unit", "player": a, "unit": "Atomic Bomb",
                                **_xy(g, base.idx)}])[0]["unit_ids"][0]
        target = next((i for i in g.grid.ring(cap_b.idx, 2) if g.city_at(i) is None and
                       g.grid.distance(base.idx, i) <= 10), None)
        if target is None:
            raise RuntimeError("no target in range")
        return tools.execute(g, a, "attack", {"unit_id": bomb, **_xy(g, target)})
    if g.nukes_enabled:
        run("nukes", nukes)

    def front():
        """Armies in contact, so there are fights to preview for both sides: A's artillery and tank two tiles from
        B's capital, B's infantry and machine gun beside the town A took (or A's capital). Placed after the bomb,
        which would otherwise take A's own army with it."""
        placed = []
        for pid, center, lo, kinds in ((a, cap_b.idx, 2, ("Artillery", "Tank")),
                                       (b, (g.city(captured["town"]).idx if captured and g.city(captured["town"])
                                            else cap_a.idx), 1, ("Infantry", "Machine Gun"))):
            for kind in kinds:
                spot = _free_land_near(g, center, lo, lo + 2, pid, kind)
                if spot is not None:
                    placed += S.apply_ops(g, [{"op": "add_unit", "player": pid, "unit": kind, **_xy(g, spot)}])
        visibility.refresh(g, force=True)
        return placed
    run("front line", front)

    def air_war():
        """Aircraft on both sides, so there are air strikes and interceptions to record: A's bomber and fighter
        in the town it took (or its capital), within reach of B's capital and army; B's fighter in its capital
        and an anti-aircraft gun beside it to intercept them."""
        base = g.city(captured["town"]) if captured and g.city(captured["town"]) and \
            g.city(captured["town"]).owner == a else cap_a
        placed = S.apply_ops(g, [{"op": "add_unit", "player": a, "unit": kind, **_xy(g, base.idx)}
                                 for kind in ("Bomber", "Fighter")])
        placed += S.apply_ops(g, [{"op": "add_unit", "player": b, "unit": "Fighter", **_xy(g, cap_b.idx)}])
        spot = _free_land_near(g, cap_b.idx, 1, 3, b, "Anti-Aircraft Gun")
        if spot is not None:
            placed += S.apply_ops(g, [{"op": "add_unit", "player": b, "unit": "Anti-Aircraft Gun", **_xy(g, spot)}])
        visibility.refresh(g, force=True)
        return placed
    run("air war", air_war)

    def refresh():
        """Recompute what everyone sees after all the edits."""
        S.apply_ops(g, [])
    run("refresh", refresh)
    return steps


SETUPS = {"world_war": world_war}
