"""Text views for language-model players: turn briefing, alerts and ASCII hex maps."""
from __future__ import annotations

from typing import Optional

from . import tiles as T
from . import unique_types as U
from .game import Game, ActionError

TERRAIN_CHAR = {"Grassland": "G", "Plains": "P", "Desert": "D", "Tundra": "T", "Snow": "S", "Mountain": "M",
                "Coast": "c", "Ocean": "o", "Lakes": "l"}
FEATURE_CHAR = {"Hill": "h", "Forest": "f", "Jungle": "j", "Marsh": "m", "Flood plains": "F", "Oasis": "O",
                "Ice": "i", "Atoll": "a", "Fallout": "x"}

MAP_LEGEND = (
    "Legend: each cell is 3 chars [terrain][feature][marker]. Odd rows (y odd) are shifted right by half a cell; "
    "hex neighbours of (x,y): same row x-1,x+1; for EVEN y: (x-1,y-1),(x,y-1),(x-1,y+1),(x,y+1); "
    "for ODD y: (x,y-1),(x+1,y-1),(x,y+1),(x+1,y+1).\n"
    "Terrain: G grassland, P plains, D desert, T tundra, S snow, M mountain (impassable), c coast, o ocean, l lake, "
    "* natural wonder. Feature: h hills, f forest, j jungle, m marsh, F flood plains, O oasis, i ice, a atoll, "
    "x fallout, r river (no other feature), '.' none. A forested hill shows its forest.\n"
    "Markers: @ your city, C foreign city, U your military unit, w your civilian, E enemy (at war) unit, "
    "N neutral foreign unit, B barbarian unit, X barbarian camp, ! ancient ruins, $ resource, + improvement, "
    "= road/railroad, space nothing. Blank '   ' = unexplored; lowercase cells are remembered, not currently seen."
)


def _unit_code(g: Game, viewer: int, units) -> Optional[str]:
    """The one or two characters that represent units on the ASCII map."""
    from .visibility import unit_visible_to
    best = None
    for u in units:
        if not unit_visible_to(g, viewer, u):
            continue
        ud = g.udef(u)
        if g.is_barbarian(u.owner):
            return "B"
        if u.owner == viewer:
            code = "U" if ud["_military"] else "w"
        elif g.at_war(viewer, u.owner):
            return "E"
        else:
            code = "N"
        if best is None or code == "U":
            best = code
    return best


def ascii_map(g: Game, pid: int, cx: Optional[int] = None, cy: Optional[int] = None, radius: int = 8) -> str:
    """Render the area around a point as ASCII, for a player with no screen.

    The map a language model actually reads. It shows terrain, features, units, cities, camps, ruins
    and resources, at a size that fits in a briefing without dominating it.
    """
    from .visibility import visible_tiles
    from .views import _tile_state
    p = g.player(pid)
    vis = visible_tiles(g, pid)
    if cx is None or cy is None:
        a = _anchor(g, pid)
        cx, cy = g.grid.xy(a if a is not None else 0)
    radius = max(2, min(int(radius), 20))
    x0, x1 = max(0, cx - radius * 2), min(g.s.width - 1, cx + radius * 2)
    y0, y1 = max(0, cy - radius), min(g.s.height - 1, cy + radius)
    lines = ["      " + "".join(f"{x:<3}" if x % 5 == 0 else "   " for x in range(x0, x1 + 1)).rstrip()]
    for y in range(y0, y1 + 1):
        row = f"y={y:<3} " + (" " if y & 1 else "")
        for x in range(x0, x1 + 1):
            idx = g.grid.idx(x, y)
            if not p.explored[idx]:
                row += "   "
                continue
            t = g.s.tiles[idx]
            visible = idx in vis
            feats, imp, route, owner, pill, _ = _tile_state(g, idx, pid)
            tc = "*" if t.wonder else TERRAIN_CHAR.get(t.terrain, "?")
            fc = "."
            for f in reversed(feats):
                if f in FEATURE_CHAR:
                    fc = FEATURE_CHAR[f]
                    break
            if fc == "." and t.river:
                fc = "r"
            marker = " "
            city = g.city_at(idx)
            known_city = city is not None and (visible or (isinstance(p.memory.get(idx), dict) and p.memory[idx].get("c")))
            if known_city:
                marker = "@" if city.owner == pid else "C"
            else:
                uc = _unit_code(g, pid, g.units_at(idx)) if visible else None
                if uc:
                    marker = uc
                elif imp == "Barbarian encampment":
                    marker = "X"
                elif imp == "Ancient ruins":
                    marker = "!"
                elif t.resource and T.resource_visible(g, pid, t.resource):
                    marker = "$"
                elif imp:
                    marker = "+"
                elif route:
                    marker = "="
            if not visible:
                tc = tc.lower()
            row += tc + fc + marker
        lines.append(row.rstrip())
    ents = []
    for c in g.s.cities.values():
        x, y = g.grid.xy(c.idx)
        if x0 <= x <= x1 and y0 <= y <= y1 and c.idx in vis:
            ents.append(f"  city '{c.name}' #{c.id} ({x},{y}) owner {g.player(c.owner).name} pop {c.pop}")
    from .visibility import unit_visible_to
    for u in g.s.units.values():
        x, y = g.grid.xy(u.idx)
        if x0 <= x <= x1 and y0 <= y <= y1 and u.idx in vis and unit_visible_to(g, pid, u):
            owner = "yours" if u.owner == pid else g.player(u.owner).name
            ents.append(f"  unit #{u.id} {u.type} ({x},{y}) {owner} hp {u.hp}")
    for idx in g.grid.within(g.grid.idx(cx, cy), radius * 2):
        x, y = g.grid.xy(idx)
        t = g.s.tiles[idx]
        if not (x0 <= x <= x1 and y0 <= y <= y1) or not p.explored[idx]:
            continue
        if t.wonder:
            ents.append(f"  natural wonder {t.wonder} ({x},{y})")
        if t.resource and T.resource_visible(g, pid, t.resource):
            owner = (" (yours)" if t.owner == pid else f" (owned by {g.player(t.owner).name})") if t.owner is not None else ""
            imp = f", improved: {t.improvement}" if t.improvement and idx in vis and not t.pillaged else ""
            amt = f" x{t.resource_amount}" if t.resource_amount else ""
            ents.append(f"  resource {t.resource}{amt} ({x},{y}){owner}{imp}")
    out = f"Map around ({cx},{cy}), x {x0}-{x1}, y {y0}-{y1}:\n" + "\n".join(lines)
    if ents:
        out += "\nIn this window:\n" + "\n".join(ents[:120])
    return out


def _anchor(g: Game, pid: int) -> Optional[int]:
    """The tile a briefing should centre on - usually the capital."""
    p = g.player(pid)
    cap = g.city(p.capital) if p.capital is not None else None
    if cap and cap.owner == pid:
        return cap.idx
    units = g.player_units(pid)
    if not units:
        return None
    settler = next((u for u in units if g.udef(u)["_umap"].get(U.FoundCity)), None)
    return (settler or units[0]).idx


def _where(g: Game, frm: int, to: int) -> str:
    """A short description of where one tile is relative to another."""
    x, y = g.grid.xy(to)
    d = g.grid.distance(frm, to)
    return f"({x},{y}) {d} tiles {g.grid.direction_name(frm, to)}" if d else f"({x},{y}) here"


# ----------------------------------------------------------------------------
# Alerts
# ----------------------------------------------------------------------------
def bombard_targets(g: Game, pid: int) -> list[tuple]:
    """(city, target idx, expected damage, kills) for each of pid's cities that can still bombard this turn."""
    from . import combat
    out = []
    for c in g.player_cities(pid):
        if combat.can_bombard(g, c) is not None:
            continue
        best = None
        a = combat.Combatant(city=c)
        for idx in combat.bombard_targets(g, c):
            d = combat.combatant_at(g, idx)
            if d is None:
                continue
            dmg = combat.damage_to_defender(g, a, d, c.idx, 0.5)
            key = (dmg >= d.hp(g), dmg)
            if best is None or key > best[0]:
                best = (key, idx, dmg, dmg >= d.hp(g))
        if best:
            out.append((c, best[1], best[2], best[3]))
    return out


def alert_items(g: Game, pid: int) -> list[dict]:
    """Things that need attention this turn. Each item has `text` (for people), `llm` (with the exact tool calls, for
    AI players), a `type`, and where relevant a map anchor (x, y) and the city/unit concerned."""
    from . import economy, visibility, policies, research, religion, victory, cities as cm
    from .units import can_promote
    p = g.player(pid)
    out: list[dict] = []

    def add(kind, text, llm=None, idx=None, **ids):
        """Add one alert, with the tool call that would deal with it."""
        item = {"type": kind, "text": text, "llm": llm or text, **ids}
        if idx is not None:
            item["x"], item["y"] = g.grid.xy(idx)
        out.append(item)

    gpt = economy.civ_stats(g, pid)["gold"]
    if gpt < 0:
        if p.gold > 0:
            head = f"Gold is falling ({gpt:+.0f}/turn): the treasury runs out in about {int(p.gold // -gpt)} turns."
        else:
            head = f"The treasury is empty and gold is falling ({gpt:+.0f}/turn): science suffers, and at -200 gold " \
                   f"units are disbanded."
        add("gold", f"{head} Fix: fewer units, Markets, gold focus, trade luxuries for gold.",
            f"{head} Fix: disband obsolete units (unit_order disband), build Markets, set_city_focus gold, trade "
            f"luxuries for gold.")
    hap = economy.happiness(g, pid)
    if hap["total"] < -10:
        add("happiness", f"Empire VERY UNHAPPY ({hap['total']}): cities stop growing, combat penalties, rebels may "
                         f"appear. Fix: Temples/Colosseums, new luxury types, no new cities for now.")
    elif hap["total"] < 0:
        add("happiness", f"Empire unhappy ({hap['total']}): city growth is slowed and Settlers cost more time. Fix: "
                         f"happiness buildings, new luxury types (improve them or trade), fewer new cities.")
    if p.golden_age_turns > 0:
        add("golden_age", f"Golden Age: {p.golden_age_turns} turns left (+production, +gold, +culture).")
    vis = visibility.visible_tiles(g, pid)
    hostile = [u for u in g.s.units.values() if u.idx in vis and g.at_war(pid, u.owner) and g.udef(u)["_military"]
               and visibility.unit_visible_to(g, pid, u)]
    bombard = {c.id: (idx, dmg, kills) for c, idx, dmg, kills in bombard_targets(g, pid)}
    for c in g.player_cities(pid):
        near = [u for u in hostile if g.grid.distance(u.idx, c.idx) <= 3]
        if near:
            garrison = g.military_at(c.idx)
            who = ", ".join(f"{g.player(u.owner).name} {u.type} {g.fmt_xy(u.idx)} hp {u.hp}" for u in near[:4])
            line = (f"City {c.name} #{c.id} (HP {c.health}) is threatened by {len(near)} hostile unit(s): {who}. "
                    f"Garrison: {garrison.type if garrison and garrison.owner == pid else 'NONE'}.")
            human = llm = line
            if c.id in bombard:
                idx, dmg, kills = bombard[c.id]
                x, y = g.grid.xy(idx)
                llm = line + f" Bombard now: city_attack city_id={c.id} x={x} y={y} (~{dmg} damage{', kills it' if kills else ''})."
                human = line + f" It can bombard {g.fmt_xy(idx)} now (~{dmg} damage)."
            add("threat", human, llm, c.idx, city=c.id)
        elif c.id in bombard:
            idx, dmg, kills = bombard[c.id]
            x, y = g.grid.xy(idx)
            add("bombard", f"City {c.name} can bombard an enemy at {g.fmt_xy(idx)} (~{dmg} damage).",
                f"City {c.name} #{c.id} can bombard: city_attack city_id={c.id} x={x} y={y} (~{dmg} damage).",
                c.idx, city=c.id)
        if cm.city_stats(g, c)["total"]["food"] < 0:
            add("starving", f"City {c.name} is starving: work more food tiles or build farms.",
                f"City {c.name} #{c.id} is starving: set_city_focus food, or build farms.", c.idx, city=c.id)
        if c.puppet and c.founder != pid and c.turn_acquired >= g.turn - 1:
            add("conquest", f"You captured {c.name}; it is a puppet. You may annex, raze or liberate it.",
                f"You captured {c.name} #{c.id} (now a puppet). Decide: city_status city_id={c.id} "
                f"status=annex|raze|liberate, or leave it as a puppet.", c.idx, city=c.id)
        if not c.queue and not c.puppet:
            add("idle_city", f"City {c.name} has nothing in production.",
                f"City {c.name} #{c.id} has nothing in production: set_production.", c.idx, city=c.id)
    for u in g.player_units(pid):
        if g.udef(u)["_military"] or g.city_at(u.idx) or g.military_at(u.idx):
            continue
        danger = [e for e in hostile if g.grid.distance(e.idx, u.idx) <= 2 and g.udef(e)["_domain"] == "Land"]
        if danger:
            e = danger[0]
            add("civilian_danger", f"{u.type} #{u.id} at {g.fmt_xy(u.idx)} is {g.grid.distance(e.idx, u.idx)} tile(s) "
                                   f"from a hostile {e.type}: undefended civilians get captured.", idx=u.idx, unit=u.id)
    if research.current(g, pid) is None and research.available_techs(g, pid) and g.player_cities(pid):
        add("research", "Choose a technology to research.", "No research set: set_research tech=<name>.")
    if p.free_techs > 0:
        add("free_tech", f"You may choose {p.free_techs} free technolog{'y' if p.free_techs == 1 else 'ies'}.",
            "Free technology available: choose_free_tech tech=<an available tech>.")
    if policies.can_adopt_any(g, pid):
        cost = policies.culture_cost(g, pid)
        add("policy", f"You can adopt a social policy ({'free' if p.free_policies else f'{cost} culture'}).",
            "You can adopt a social policy: adopt_policy policy=<name> (get_policies lists options).")
    if p.free_great_people > 0:
        add("great_person", "You may choose a free Great Person.",
            "Free Great Person available: choose_great_person great_person=<Great Scientist|Great Engineer|...>.")
    if g.religion_enabled and religion.can_found_pantheon(g, pid) is None:
        add("pantheon", "You have enough faith to found a pantheon.",
            "Enough faith for a pantheon: found_pantheon belief=<name> (get_religion lists beliefs).")
    promos = [u for u in g.player_units(pid) if can_promote(g, u)]
    if promos:
        add("promotion", "Units ready for promotion: " + ", ".join(f"{u.type} #{u.id}" for u in promos[:6]),
            "Promotions available (promote_unit): " + ", ".join(f"#{u.id} {u.type}" for u in promos[:6]))
    if g.espionage_enabled:
        idle = [s["name"] for s in p.spies if s["action"] == "None"]
        if idle:
            add("spy", f"Idle spies: {', '.join(idle)}.", f"Idle spies ({', '.join(idle)}): move_spy spy=<name> city_id=<id>.")
    if victory.vote_open(g) and str(pid) not in victory._un(g)["votes"]:
        add("un_vote", "The United Nations vote is open: cast your vote.",
            "United Nations vote open: un_vote candidate=<player id or 'abstain'>.")
    for n in g.s.negotiations:
        if n["status"] == "open" and n.get("awaiting") == pid:
            other = n["initiator"] if n["responder"] == pid else n["responder"]
            add("negotiation", f"{g.player(other).name} awaits your reply in negotiation #{n['id']}.",
                f"Negotiation #{n['id']} with {g.player(other).name} awaits your response: respond_negotiation.")
    return out


def alerts(g: Game, pid: int) -> list[str]:
    """The problems that need attention this turn, each with its usual fix.

    At the top of every briefing, because the difference between a model that plays adequately and one
    that collapses is usually whether it noticed its cities were starving. Each alert names the exact
    call that addresses it.
    """
    return [a["llm"] for a in alert_items(g, pid)]


# ----------------------------------------------------------------------------
# Options for units and cities
# ----------------------------------------------------------------------------
def _unit_options(g: Game, pid: int, units) -> list[str]:
    """What each idle unit could usefully do, summarised."""
    from . import workers, combat, automation, units as unitmod
    from .actions import unit_actions
    idle = [u for u in units if u.activity is None and u.moves > 0]
    if not idle:
        return []
    out = ["\nOPTIONS FOR UNITS NEEDING ORDERS:"]
    p = g.player(pid)
    for u in idle[:14]:
        ud = g.udef(u)
        bits = []
        acts = [a for a in unit_actions(g, u) if a["available"]]
        if acts:
            bits.append("unit_action: " + ", ".join(f"{a['id']} ({a['name']})" for a in acts[:6]))
        if ud["_umap"].get(U.FoundCity):
            sites = automation.suggest_city_sites(g, pid, u.idx, radius=6, count=3)
            if sites:
                bits.append("good city sites: " + ", ".join(_where(g, u.idx, i) for i, _ in sites))
        opts = [o for o in workers.build_options(g, u) if not o.get("instant")]
        if opts:
            bits.append("build here: " + ", ".join(f"{o['name']} ({o['turns']}t)" for o in opts[:8])
                        + "; or unit_order automate")
        if ud["_military"] and combat.can_attack_now(g, u) is None and ud["_domain"] != "Air":
            targets = []
            radius = unitmod.attack_range(g, u) if ud["_ranged"] else 1
            for idx in g.grid.within(u.idx, radius)[1:]:
                try:
                    pv = combat.preview(g, u, idx)
                except ActionError:
                    continue
                targets.append(f"{pv['defender']} {_where(g, u.idx, idx)}: deal {pv['damage_to_defender']}, "
                               f"take {pv['damage_to_attacker']}")
            if targets:
                bits.append("attack targets: " + "; ".join(targets[:4]))
        if unitmod.can_promote(g, u):
            bits.append("promotions: " + ", ".join(sorted(unitmod.available_promotions(g, u))[:8]))
        near = []
        for idx in g.grid.within(u.idx, 6)[1:]:
            if not p.explored[idx]:
                continue
            imp = g.s.tiles[idx].improvement
            if imp == "Ancient ruins":
                near.append(f"ancient ruins {_where(g, u.idx, idx)}")
            elif imp == "Barbarian encampment":
                near.append(f"barbarian camp {_where(g, u.idx, idx)}")
        if near:
            bits.append("nearby: " + "; ".join(near[:3]))
        if ud["_military"] and not any(b.startswith("attack") for b in bits):
            unexplored = sum(1 for idx in g.grid.within(u.idx, 4) if not p.explored[idx])
            if unexplored:
                bits.append(f"{unexplored} unexplored tiles within 4 (unit_order explore)")
        x, y = g.grid.xy(u.idx)
        out.append(f"  [#{u.id}] {u.type} ({x},{y}): " + (" | ".join(bits) if bits else "move_unit, fortify, sleep or skip"))
    return out


def _city_options(g: Game, pid: int, cities) -> list[str]:
    """What each city needs a decision about."""
    from . import cities as cm
    idle = [c for c in cities if not c.queue and not c.puppet]
    if not idle:
        return []
    out = ["\nIDLE CITIES — what they can build (item cost/turns):"]
    for c in idle[:6]:
        items = cm.buildable_items(g, c)
        parts = []
        for k in ("units", "buildings", "wonders", "other"):
            names = items.get(k) or []
            if names:
                parts.append(f"{k}: " + ", ".join(
                    f"{n} {cm.production_cost(g, pid, n, c)}/{cm.turns_to_build(g, c, n)}t" if n not in cm.PERPETUAL
                    else n for n in names[:14]))
        out.append(f"  [#{c.id}] {c.name}: " + " | ".join(parts))
    return out


def _points_of_interest(g: Game, pid: int, anchor: int) -> list[str]:
    """Notable things near the civilization worth mentioning in a briefing."""
    from .views import _tile_state
    p = g.player(pid)
    pois = []
    for idx in range(g.grid.size):
        if not p.explored[idx]:
            continue
        imp = _tile_state(g, idx, pid)[1]
        if imp == "Barbarian encampment":
            pois.append((g.grid.distance(anchor, idx), f"barbarian camp {_where(g, anchor, idx)}"))
        elif imp == "Ancient ruins":
            pois.append((g.grid.distance(anchor, idx), f"ancient ruins {_where(g, anchor, idx)}"))
        if g.s.tiles[idx].wonder:
            pois.append((g.grid.distance(anchor, idx), f"{g.s.tiles[idx].wonder} {_where(g, anchor, idx)}"))
    for c in g.s.cities.values():
        if c.owner != pid and p.explored[c.idx]:
            pois.append((g.grid.distance(anchor, c.idx), f"{g.player(c.owner).name} city {c.name} {_where(g, anchor, c.idx)}"))
    if not pois:
        return []
    pois.sort()
    return ["Known points of interest (from your capital/first unit): " + "; ".join(t for _, t in pois[:12])]


def _last_turn_end_event(g: Game, pid: int) -> int:
    """The event id at the end of the player's last turn, so a briefing can report what changed."""
    for ev in reversed(g.s.events):
        if ev["type"] == "turn_end" and ev["data"].get("player") == pid:
            return ev["id"]
    return 0


def turn_progress(g: Game, pid: int) -> str:
    """Compact, current to-do status for the player (used after each batch of actions)."""
    from . import research, policies
    from .units import can_promote
    from .cities import found_check
    g.player(pid)
    parts = []
    cities = g.player_cities(pid)
    parts.append(f"cities: {', '.join(f'{c.name} #{c.id}' for c in cities) if cities else 'none yet'}")
    cur = research.current(g, pid)
    if cur:
        parts.append(f"research: {cur} (set)")
    elif research.available_techs(g, pid):
        parts.append("research: NOT SET")
    idle_cities = [c for c in cities if not c.queue and not c.puppet]
    if idle_cities:
        parts.append("cities with no production: " + ", ".join(f"{c.name} #{c.id}" for c in idle_cities))
    need = []
    for u in g.player_units(pid):
        if u.activity is None and u.moves > 0:
            x, y = g.grid.xy(u.idx)
            extra = ""
            if g.udef(u)["_umap"].get(U.FoundCity) and found_check(g, pid, u.idx) is None:
                extra = " (can found a city here)"
            need.append(f"#{u.id} {u.type} at ({x},{y}){extra}")
    parts.append("units still needing orders: " + ("; ".join(need) if need else "none"))
    promos = [f"#{u.id}" for u in g.player_units(pid) if can_promote(g, u)]
    if promos:
        parts.append("promotions available: " + ", ".join(promos))
    if policies.can_adopt_any(g, pid):
        parts.append("a social policy can be adopted")
    shots = bombard_targets(g, pid)
    if shots:
        parts.append("cities that can still bombard (city_attack): " + ", ".join(
            f"{c.name} #{c.id} -> {g.fmt_xy(idx)}" for c, idx, _, _ in shots))
    done = not need and not idle_cities and (cur or not research.available_techs(g, pid))
    tail = " Everything is handled — call end_turn now." if done else " Handle what remains, then call end_turn."
    return "TURN PROGRESS (current state; this supersedes the briefing): " + " | ".join(parts) + "." + tail


# ----------------------------------------------------------------------------
# Briefing
# ----------------------------------------------------------------------------
def briefing(g: Game, pid: int) -> str:
    """The whole turn briefing, as prose.

    This is the single most important piece of text in CITAR: it is what an AI player reads at the start
    of every turn, and its quality is most of the difference between a one-minute turn and a
    ten-minute one. It is written to be *sufficient* - empire status, cities, units with idle ones
    marked, events since last turn, diplomacy, alerts and a to-do list - so that an agent can act
    without a dozen follow-up queries.

    An earlier, terser version was measurably worse: turns took seven to fifteen minutes because the
    model spent them asking questions.
    """
    from . import economy, research, cities as cm, victory, movement, policies
    from .units import can_promote
    from .diplomacy import negotiation_view
    from . import diplomacy as D, city_states as CS
    p = g.player(pid)
    sc = g.rules.move_scale
    lines = []
    era = g.rules.era_list[research.player_era(g, pid)]
    turn_note = "YOUR TURN" if g.s.current == pid else f"waiting ({g.player(g.s.current).name}'s turn)"
    lines.append(f"=== TURN {g.turn}/{g.total_turns()} ({g.year_text()}) — {p.name} (player {pid}, {p.nation}) — "
                 f"{era} — {turn_note} ===")
    st = economy.civ_stats(g, pid)
    hap = economy.happiness(g, pid)
    cur = research.current(g, pid)
    rs = "nothing (choose with set_research!)"
    if cur:
        rs = f"{cur} ({int(p.research_progress.get(cur, 0))}/{research.tech_cost(g, pid, cur)}, " \
             f"{research.turns_left(g, pid) or '?'} turns)"
        if len(p.research_queue) > 1:
            rs += f", then {', '.join(p.research_queue[1:4])}"
    lines.append(f"Gold {int(p.gold)} ({st['gold']:+.0f}/turn) · Science {st['science']:.0f}/turn → {rs}")
    lines.append(f"Culture {int(p.culture)} ({st['culture']:+.0f}/turn; next policy {policies.culture_cost(g, pid)}) · "
                 f"Faith {int(p.faith)} ({st['faith']:+.0f}/turn) · Happiness {hap['total']} ({hap['status']}) · "
                 f"Golden age {'ACTIVE ' + str(p.golden_age_turns) + ' turns' if p.golden_age_turns else f'{int(p.golden_age_points)} pts'}")
    lines.append(f"Luxuries: {', '.join(hap['luxury_types']) or 'none'} · Score {victory.score(g, pid)['total']} · "
                 f"Policies: {', '.join(x for x in p.policies if not x.endswith(' Complete')) or 'none'}")
    strat = economy.strategic_resources(g, pid)
    sparts = [f"{r} {v['available']} free ({v['sources']} src, {v['used']} used"
              + (f", +{v['imported']} in" if v['imported'] else "") + (f", -{v['exported']} out" if v['exported'] else "") + ")"
              for r, v in strat.items() if T.resource_visible(g, pid, r) and (v["sources"] or v["used"] or v["imported"])]
    if sparts:
        lines.append("Strategic: " + " · ".join(sparts))
    if g.religion_enabled and p.religion:
        from .religion import display_name, all_beliefs
        lines.append(f"Religion: {display_name(g, p.religion)} ({p.religion_state}) — beliefs: {', '.join(all_beliefs(g, p.religion))}")
    warnings = alerts(g, pid)
    if warnings:
        lines.append("\nALERTS (handle these first):")
        lines.extend(f"  ! {w}" for w in warnings)

    cities = g.player_cities(pid)
    lines.append(f"\nCITIES ({len(cities)}):")
    if not cities:
        lines.append("  none — found a city with a Settler (found_city)!")
    for c in cities:
        tot = cm.city_stats(g, c)["total"]
        x, yy = g.grid.xy(c.idx)
        need = cm.food_to_next_pop(g, c)
        grow = f"grows in {-(-int(need - c.food) // max(1, int(tot['food'])))}" if tot["food"] > 0 else (
            "STARVING" if tot["food"] < 0 else "stagnant")
        head = cm.current_construction(c)
        if head:
            if head in cm.PERPETUAL:
                build = f"converting production to {head}"
            else:
                cost = cm.production_cost(g, pid, head, c)
                build = f"building {head} ({int(c.progress.get(head, 0))}/{cost}, {cm.turns_to_build(g, c, head)} turns)"
            if len(c.queue) > 1:
                build += " then " + ", ".join(c.queue[1:])
        else:
            build = "puppet (builds on its own)" if c.puppet else "IDLE — nothing in production!"
        tags = []
        if p.capital == c.id:
            tags.append("capital")
        if c.puppet:
            tags.append("puppet")
        if c.resistance:
            tags.append(f"resistance {c.resistance}")
        if c.razing:
            tags.append("RAZING")
        if c.health < cm.max_health(g, c):
            tags.append(f"HP {c.health}/{cm.max_health(g, c)}")
        if c.specialists:
            tags.append("specialists " + ", ".join(f"{k} {v}" for k, v in c.specialists.items()))
        lines.append(f"  [#{c.id}] {c.name} ({x},{yy}) pop {c.pop}{' ' + ', '.join(tags) if tags else ''} · "
                     f"food {tot['food']:+.0f} ({grow}) · prod {tot['production']:.0f} · gold {tot['gold']:.0f} · "
                     f"sci {tot['science']:.0f} · cul {tot['culture']:.0f} · faith {tot['faith']:.0f} · {build}")

    units = sorted(g.player_units(pid), key=lambda u: (g.udef(u)["_military"], u.id))
    lines.append(f"\nUNITS ({len(units)}) — '*' = needs orders:")
    for u in units:
        x, yy = g.grid.xy(u.idx)
        idle = u.activity is None and u.moves > 0
        act = u.activity or ("ready" if u.moves > 0 else "done")
        if u.activity in ("explore", "goto", "automate") and u.moves <= 0:
            act += " (already moved this turn)"
        t = g.s.tiles[u.idx]
        if u.activity in ("build", "automate") and t.build:
            act = f"{'automated, ' if u.activity == 'automate' else ''}building " + \
                  " then ".join(f"{n} ({k} turns)" for n, k in t.build if k >= 0)
        elif u.activity == "goto" and u.goto is not None:
            act = f"moving to {g.fmt_xy(u.goto)}"
        extra = []
        if u.hp < 100:
            extra.append(f"hp {u.hp}")
        if can_promote(g, u):
            extra.append("PROMOTION READY")
        if movement.is_embarked(g, u):
            extra.append("embarked")
        lines.append(f"  {'*' if idle else ' '} [#{u.id}] {u.type} ({x},{yy}) moves {round(u.moves / sc, 1)}/"
                     f"{round(movement.max_moves(g, u) / sc, 1)} · {act}{' · ' + ', '.join(extra) if extra else ''}")

    lines.extend(_unit_options(g, pid, units))
    lines.extend(_city_options(g, pid, cities))
    if not cur:
        avail = research.available_techs(g, pid)
        if avail:
            lines.append("\nAVAILABLE TECHS (name: cost → unlocks):")
            for t in sorted(avail, key=lambda t: research.tech_cost(g, pid, t))[:14]:
                un = g.rules.unlocks.get(t, {})
                what = [x for k in ("units", "buildings", "improvements") for x in un.get(k, [])] + \
                       [f"reveals {r}" for r in un.get("reveals", [])]
                lines.append(f"  {t}: {research.tech_cost(g, pid, t)} → {', '.join(what) or 'prerequisite for later techs'}")

    anchor = _anchor(g, pid)
    if anchor is not None:
        ax, ay = g.grid.xy(anchor)
        lines.append("\nLOCAL MAP (legend: get_map legend=true):")
        local = ascii_map(g, pid, ax, ay, 5).split("\nIn this window:\n")
        lines.append(local[0])
        if len(local) > 1:
            extras = [ln.strip() for ln in local[1].split("\n") if ln.strip().startswith(("resource", "unit", "city", "natural"))
                      and "yours hp" not in ln]
            if extras:
                lines.append("Also in this window: " + "; ".join(extras[:20]))
        lines.extend(_points_of_interest(g, pid, anchor))

    since = _last_turn_end_event(g, pid)
    evs = [e for e in g.events_for(pid, since) if e["type"] not in ("turn_start", "turn_end", "message")]
    lines.append("\nEVENTS since your last turn:")
    if not evs:
        lines.append("  (none)")
    for e in evs[-40:]:
        lines.append(f"  - T{e['turn']}: {e['text']}")

    lines.append("\nDIPLOMACY:")
    met = [q for q in p.met if g.player(q).alive and g.player(q).kind == "major"]
    if not met:
        lines.append("  You have not met any other civilization yet.")
    for q in met:
        qp = g.player(q)
        rel = g.relation(pid, q) or {}
        status = "AT WAR" if rel.get("war") else (
            f"peace treaty until T{rel['treaty_until']}" if rel.get("treaty_until", 0) >= g.turn else "peace")
        tags = []
        if g.has_open_borders(pid, q):
            tags.append("you grant open borders")
        if g.has_open_borders(q, pid):
            tags.append("they grant you open borders")
        if D.is_friends(g, pid, q):
            tags.append("friends")
        if D.has_pact(g, pid, q):
            tags.append("defensive pact")
        if rel.get("ra_until", 0) >= g.turn:
            tags.append("research agreement")
        if D.denounced(g, q, pid):
            tags.append("they denounced you")
        lines.append(f"  - player {q} {qp.name} ({qp.nation}): {status}; score {victory.score(g, q)['total']}; "
                     f"{len(g.player_cities(q))} cities{'; ' + ', '.join(tags) if tags else ''}")
    css = [q for q in g.city_states() if g.has_met(pid, q.id)]
    if css:
        lines.append("  City-states: " + "; ".join(
            f"{q.name} #{q.id} ({q.cs_type}, {CS.relationship(g, q.id, pid)}, influence {CS.influence(g, q.id, pid):.0f}"
            f"{', ally ' + g.player(q.ally).name if q.ally is not None and q.ally != pid else ''})" for q in css[:12]))
    msgs = [m for m in g.s.messages if pid in m["to"] and m["turn"] >= g.turn - 1]
    for m in msgs[-10:]:
        lines.append(f"  ✉ T{m['turn']} from {g.player(m['from']).name}: \"{m['text'][:600]}\"")
    for n in g.s.negotiations:
        if n["status"] == "open" and pid in (n["initiator"], n["responder"]):
            v = negotiation_view(g, n, pid)
            prop = v["current_proposal"]["summary"] if v["current_proposal"] else "no concrete proposal"
            who = "YOUR MOVE" if v["your_move"] else f"waiting for {v['with_name']}"
            lines.append(f"  ⚖ negotiation #{n['id']} with {v['with_name']} ({who}): {prop}")
    active = [d for d in g.s.deals if d.get("active") and pid in d["parties"]]
    if active:
        lines.append("  Active deals: " + " | ".join(d.get("summary", "") for d in active[-5:]))

    todo = []
    if not cur and research.available_techs(g, pid):
        todo.append("choose research (set_research)")
    if any(not c.queue and not c.puppet for c in cities):
        todo.append("set production in idle cities")
    if any(u.activity is None and u.moves > 0 for u in units):
        todo.append("give orders to units marked '*' (or fortify/sleep them)")
    if any(can_promote(g, u) for u in units):
        todo.append("promote units that are ready")
    if policies.can_adopt_any(g, pid):
        todo.append("adopt a social policy (adopt_policy)")
    if todo:
        lines.append("\nTO DO: " + "; ".join(todo) + ". Call end_turn when finished.")
    if p.notes:
        lines.append("\nYOUR NOTEBOOK:\n" + p.notes[-3000:])
    return "\n".join(lines)
