"""Special unit actions, listed and executed by id (UnCiv's UnitActions / UnitActionsFromUniques, MPL-2.0).

`unit_actions(g, u)` lists what a unit can do right now (founding cities and religions, spreading religion, great
person abilities, great improvements, golden ages and other one-time effects, spaceship parts...). Movement, combat,
worker builds and simple orders (fortify, sleep, ...) have their own tools.
"""
from __future__ import annotations

from typing import Optional, TYPE_CHECKING

from . import unique_types as U
from .game import ActionError

if TYPE_CHECKING:
    from .game import Game
    from .state import Unit

HANDLED = {U.FoundCity, U.MayFoundReligion, U.MayEnhanceReligion, U.CanSpreadReligion, U.CanRemoveHeresy,
           U.CanHurryResearch, U.CanSpeedupConstruction, U.CanSpeedupWonderConstruction, U.CanHurryPolicy,
           U.CanTradeWithCityStateForGoldAndInfluence, U.ConstructImprovementInstantly, U.CreateWaterImprovements,
           U.AddInCapital, U.MayParadrop}


def _action(aid: str, name: str, ok: Optional[str] = None, params: Optional[dict] = None, **extra) -> dict:
    """Register a unit action with the conditions under which it is offered."""
    d = {"id": aid, "name": name, "available": ok is None}
    if ok is not None:
        d["reason"] = ok
    if params:
        d["params"] = params
    d.update(extra)
    return d


def _check(fn) -> Optional[str]:
    """Whether a unit may perform an action here, and why not if it may not."""
    try:
        r = fn()
        return r if isinstance(r, str) else None
    except ActionError as e:
        return str(e)


def unit_actions(g: "Game", u: "Unit") -> list[dict]:
    """Every action this unit could take right now, with the ids to call them by.

    What ``get_unit`` returns and what makes the single ``unit_action`` tool discoverable: an agent
    reads what is possible rather than memorising a list that grows with the ruleset.
    """
    from .units import usable_action, unit_umap
    from .cities import found_check
    from . import religion, workers, research, triggers
    out = []
    pid = u.owner
    moves = u.moves > 0
    no_moves = None if moves else "No movement left this turn."
    if usable_action(g, u, U.FoundCity):
        out.append(_action("found_city", "Found a city here", no_moves or found_check(g, pid, u.idx),
                           {"name": "optional city name"}))
    if g.religion_enabled:
        if usable_action(g, u, U.MayFoundReligion):
            reason = religion.can_found_religion(g, pid)
            c = g.city_at(u.idx)
            if reason is None and (c is None or c.owner != pid):
                reason = "Move the Great Prophet into one of your cities."
            out.append(_action("found_religion", "Found a religion", no_moves or reason,
                               {"name": "religion name", "beliefs": "list of belief names (see get_religion)"}))
        if usable_action(g, u, U.MayEnhanceReligion):
            p = g.player(pid)
            reason = None if p.religion_state == "religion" else "You must have founded a (not yet enhanced) religion."
            out.append(_action("enhance_religion", "Enhance your religion", no_moves or reason,
                               {"beliefs": "list of belief names (see get_religion)"}))
        if usable_action(g, u, U.CanSpreadReligion):
            t = g.s.tiles[u.idx]
            reason = None if t.city is not None else "Move next to or into a city's territory to spread religion."
            out.append(_action("spread_religion", f"Spread {religion.display_name(g, u.religion) if u.religion else 'religion'}",
                               no_moves or reason))
        if usable_action(g, u, U.CanRemoveHeresy):
            out.append(_action("remove_heresy", "Remove other religions from this city", no_moves))
    if usable_action(g, u, U.CanHurryResearch):
        out.append(_action("hurry_research", f"Hurry research (+{research.science_from_great_scientist(g, pid)} science)",
                           no_moves or (None if research.current(g, pid) else "Choose a technology first.")))
    if usable_action(g, u, U.CanSpeedupConstruction) or usable_action(g, u, U.CanSpeedupWonderConstruction):
        c = g.city_at(u.idx)
        out.append(_action("hurry_construction", "Hurry the building or wonder in production here",
                           no_moves or (None if c is not None and c.owner == pid else "Move into one of your cities.")))
    if usable_action(g, u, U.CanTradeWithCityStateForGoldAndInfluence):
        o = g.s.tiles[u.idx].owner
        ok = o is not None and g.player(o).kind == "city_state" and not g.at_war(o, pid)
        out.append(_action("trade_mission", "Trade mission (gold and influence)",
                           no_moves or (None if ok else "Move into the territory of a city-state you are at peace with.")))
    if usable_action(g, u, U.CanHurryPolicy):
        out.append(_action("political_treatise", "Generate a large amount of culture", no_moves))
    for opt in workers.great_improvement_options(g, u) + workers._water_option(g, u):
        out.append(_action("create:" + opt["name"], f"Create {opt['name']} here",
                           no_moves or ("; ".join(opt["blocked"]) if opt.get("blocked") else None)))
    x = usable_action(g, u, U.MayParadrop)
    if x is not None:
        from .movement import max_moves
        out.append(_action("paradrop", f"Paradrop to a [{x.p(0)}] tile up to {x.n(1)} tiles away",
                           None if u.moves >= max_moves(g, u) else "Paradropping needs the unit's full movement.",
                           {"x": "target column", "y": "target row"}))
    if g.rules.units[u.type]["_umap"].get(U.AddInCapital):
        p = g.player(pid)
        cap = g.city(p.capital) if p.capital is not None else None
        out.append(_action("add_to_spaceship", "Add to the spaceship",
                           no_moves or (None if cap is not None and cap.idx == u.idx else "Move into your capital.")))
    for k, x in enumerate(unit_umap(g, u).all):
        if x.ph in HANDLED or not triggers.is_triggerable(x) or triggers.has_trigger_conditional(x):
            continue
        if not any(m.ph in ("by consuming this unit", "[] times", "once", "for [] movement") for m in x.mods):
            continue
        if usable_action(g, u, x.ph) is not x:
            continue
        out.append(_action(f"trigger:{k}", x.text.split(" <")[0], no_moves))
    return out


def do_unit_action(g: "Game", u: "Unit", action: str, name: Optional[str] = None, beliefs=None,
                   target: Optional[int] = None) -> dict:
    """Perform a unit's special action, whatever that unit happens to be."""
    from .units import usable_action, consume_action, unit_umap
    from . import religion, workers, great_people, cities, victory, triggers, policies
    pid = u.owner
    available = {a["id"]: a for a in unit_actions(g, u)}
    a = available.get(action)
    if a is None:
        names = ", ".join(available) or "none"
        raise ActionError(f"{u.type} has no action '{action}'. Its actions: {names}.")
    if not a["available"]:
        raise ActionError(a.get("reason") or "Not possible right now.")
    if action == "found_city":
        c = cities.found_city(g, pid, u.idx, name, unit=u)
        return {"city_id": c.id, "name": c.name, "at": list(g.xy(c.idx))}
    if action == "found_religion":
        if not name:
            raise ActionError("Give the religion a name.")
        return religion.found_religion(g, pid, u, name, list(beliefs or []))
    if action == "enhance_religion":
        return religion.enhance_religion(g, pid, u, list(beliefs or []))
    if action == "spread_religion":
        return religion.spread_religion(g, u)
    if action == "remove_heresy":
        return religion.remove_heresy(g, u)
    if action == "hurry_research":
        return great_people.hurry_research(g, u)
    if action == "hurry_construction":
        return great_people.hurry_construction(g, u)
    if action == "trade_mission":
        return great_people.trade_mission(g, u)
    if action == "political_treatise":
        x = usable_action(g, u, U.CanHurryPolicy)
        culture = policies.culture_from_great_writer(g, pid)
        g.player(pid).culture += culture
        consume_action(g, u, x)
        great_people.triggers_expend(g, u)
        return {"culture_added": culture}
    if action.startswith("create:"):
        imp = action.split(":", 1)[1]
        for opt in workers.great_improvement_options(g, u) + workers._water_option(g, u):
            if opt["name"] == imp:
                res = opt["action"]()
                if g.rules.improvements[imp]["_umap"].has_tag("Great Improvement"):
                    great_people.triggers_expend(g, u)
                return res
        raise ActionError(f"{imp} cannot be created here.")
    if action == "add_to_spaceship":
        return victory.add_to_spaceship(g, u)
    if action == "paradrop":
        return paradrop(g, u, target)
    if action.startswith("trigger:"):
        k = int(action.split(":", 1)[1])
        x = unit_umap(g, u).all[k]
        if not triggers.trigger(g, x, pid, unit=u, tile=u.idx, note=f"by a {u.type}"):
            raise ActionError("That had no effect right now; the unit was not used.")
        if g.unit(u.id) is not None:
            consume_action(g, u, x)
            if g.unit(u.id) is None and g.rules.units[u.type]["_great_person"]:
                great_people.triggers_expend(g, u)
        return {"done": a["name"]}
    raise ActionError(f"Unknown action '{action}'.")


def paradrop_problem(g: "Game", u: "Unit", idx: int) -> Optional[str]:
    """UnCiv paradrop: a visible-or-explored tile matching the filter, within range, that the unit may enter."""
    from .units import usable_action
    from .movement import can_stand
    from .uniques import tile_matches
    x = usable_action(g, u, U.MayParadrop)
    if x is None:
        return "This unit cannot paradrop from here."
    if not 0 <= idx < g.grid.size:
        return "That tile is off the map."
    if idx == u.idx:
        return "The unit is already there."
    if g.grid.distance(u.idx, idx) > x.n(1):
        return f"Paradrops reach at most {x.n(1)} tiles."
    if not g.player(u.owner).explored[idx]:
        return "You cannot paradrop into unexplored territory."
    if not tile_matches(g, idx, x.p(0), u.owner):
        return f"The target must be a [{x.p(0)}] tile."
    c = g.city_at(idx)
    if c is not None and c.owner != u.owner:
        return "You cannot paradrop into a foreign city."
    if not can_stand(g, u.owner, g.rules.units[u.type], idx, u):
        return "The unit cannot enter that tile (occupied, impassable or closed borders)."
    return None


def paradrop(g: "Game", u: "Unit", idx: Optional[int]) -> dict:
    """Drop a paratrooper onto a tile within its range."""
    from .movement import on_enter_tile
    if idx is None:
        raise ActionError("Give the target tile (x, y) for the paradrop.")
    err = paradrop_problem(g, u, idx)
    if err:
        raise ActionError(err)
    start = u.idx
    g.place_unit(u, idx)
    u.moves = 0
    u.acted = True
    if u.activity in ("fortify", "fortify_heal", "sleep", "sleep_heal", "goto"):
        u.activity, u.goto = None, None
    on_enter_tile(g, u, idx)
    return {"from": list(g.xy(start)), "to": list(g.xy(idx))}
