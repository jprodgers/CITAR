"""Score, elimination, victory milestones (Scientific, Cultural, Domination, Diplomatic, Time), the United Nations
vote, per-turn statistics and replay frames.

Milestones, score and voting follow UnCiv's Victory/Milestone, VictoryManager, Civilization.calculateScoreBreakdown
and TurnManager.handleDiplomaticVictoryFlags (MPL-2.0).
"""
from __future__ import annotations

import base64
from collections import Counter
from typing import Optional, TYPE_CHECKING

from . import unique_types as U
from .game import ActionError

if TYPE_CHECKING:
    from .game import Game


# ----------------------------------------------------------------------------
# Score
# ----------------------------------------------------------------------------
def score(g: "Game", pid: int) -> dict:
    """A civilization's score, broken down by where it comes from."""
    from .cities import city_tiles
    from .tiles import is_water
    k = g.rules.k
    p = g.player(pid)
    cities = g.player_cities(pid)
    msm = 1276 / g.grid.size
    if msm > 1:
        msm = (msm - 1) / 3 + 1
    parts = {
        "cities": len(cities) * 10 * msm,
        "population": sum(c.pop for c in cities) * k["score_from_population"] * msm,
        "tiles": sum(1 for c in cities for i in city_tiles(g, c) if not is_water(g, i)) * msm,
        "wonders": k["score_from_wonders"] * sum(1 for c in cities for b in c.buildings if g.rules.buildings[b].get("isWonder")),
        "technologies": len(p.techs) * 4.0,
        "future_tech": p.future_techs * 10.0,
    }
    parts = {a: round(b, 1) for a, b in parts.items()}
    parts["total"] = int(sum(parts.values()))
    return parts


def military_strength(g: "Game", pid: int) -> int:
    """The combined strength of a civilization's army, for comparisons and the graphs."""
    total = 0
    for u in g.player_units(pid):
        ud = g.rules.units[u.type]
        total += max(ud.get("strength", 0), ud.get("rangedStrength", 0)) * u.hp / 100
    return int(total)


# ----------------------------------------------------------------------------
# Spaceship
# ----------------------------------------------------------------------------
def required_parts(g: "Game") -> Counter:
    """The spaceship parts a scientific victory needs."""
    return Counter(g.rules.victories.get("Scientific", {}).get("requiredSpaceshipParts", []))


def spaceship_status(g: "Game", pid: int) -> dict:
    """How far along a civilization's spaceship is."""
    have = g.s.spaceship.get(pid, {})
    need = required_parts(g)
    apollo = any(any(g.rules.buildings[b]["_umap"].has_tag(U.EnablesConstructionOfSpaceshipParts) for b in c.buildings)
                 for c in g.player_cities(pid))
    parts = {k: {"added": min(have.get(k, 0), n), "needed": n} for k, n in need.items()}
    return {"apollo_program": apollo, "parts": parts,
            "complete": all(x["added"] >= x["needed"] for x in parts.values())}


def add_to_spaceship(g: "Game", unit) -> dict:
    """Unit action for spaceship parts: 'Can be added to [The Spaceship] in the Capital'."""
    ud = g.rules.units[unit.type]
    if not ud["_umap"].get(U.AddInCapital):
        raise ActionError(f"A {unit.type} cannot be added to the spaceship.")
    p = g.player(unit.owner)
    cap = g.city(p.capital) if p.capital is not None else None
    if cap is None or unit.idx != cap.idx:
        raise ActionError("Spaceship parts must be in your capital to be added.")
    if unit.moves <= 0:
        raise ActionError("The unit has no movement left this turn.")
    ship = g.s.spaceship.setdefault(unit.owner, {})
    ship[unit.type] = ship.get(unit.type, 0) + 1
    pid = unit.owner
    g.remove_unit(unit)
    g.emit("spaceship", f"{p.name} added a {unit.type} to its spaceship.", None, player=pid)
    check_victory(g, pid)
    return {"added": unit.type, "spaceship": spaceship_status(g, pid)}


# ----------------------------------------------------------------------------
# Diplomatic vote (United Nations)
# ----------------------------------------------------------------------------
def _un(g) -> dict:
    """The United Nations state, created on first use."""
    un = g.s.un
    un.setdefault("next_vote", None)       # turn the next vote is held (None = not scheduled)
    un.setdefault("votes", {})              # voter pid -> candidate pid or None (abstain)
    un.setdefault("results", None)          # last result
    un.setdefault("won", [])                # pids that have ever won the vote
    return un


def turns_between_votes(g: "Game") -> int:
    """How often the world leader vote is held, scaled by game speed."""
    return int(15 * g.speed["modifier"])


def schedule_vote(g: "Game", pid: int):
    """Schedule the next vote, as the United Nations is completed."""
    un = _un(g)
    un["next_vote"] = g.turn + turns_between_votes(g)
    un["votes"] = {}
    g.emit("un_vote", f"The United Nations will hold a vote for world leader on turn {un['next_vote']}.", None)


def un_owner(g: "Game") -> Optional[int]:
    """The civilization that built the United Nations, which gets an extra vote."""
    for c in g.s.cities.values():
        if not g.player(c.owner).alive or g.player(c.owner).kind == "barbarian":
            continue
        if any(g.rules.buildings[b]["_umap"].has_tag(U.OneTimeTriggerVoting) for b in c.buildings):
            return c.owner
    return None


def _voters(g) -> list:
    """Everyone entitled to vote - every living civilization and city-state."""
    return [p for p in g.s.players if p.alive and p.kind != "barbarian"]


def votes_needed(g: "Game") -> int:
    """How many votes a diplomatic victory needs."""
    n = len(_voters(g)) + (1 if un_owner(g) is not None else 0)
    if n > 28:
        return n * 35 // 100
    return n * (67 - int(1.1 * n)) // 100 + 1


def vote_open(g: "Game") -> bool:
    """Whether voting is open right now.

    Opens the turn before it is counted, which is why casting a vote is usable out of turn.
    """
    un = _un(g)
    return un["next_vote"] is not None and g.turn >= un["next_vote"] - 1 and not un.get("processed_turn") == g.turn


def cast_vote(g: "Game", pid: int, candidate) -> dict:
    """Record a vote for a candidate, or an abstention."""
    un = _un(g)
    if un["next_vote"] is None:
        raise ActionError("No United Nations vote is scheduled (the United Nations wonder schedules it).")
    if g.turn < un["next_vote"] - 1:
        raise ActionError(f"Voting opens on turn {un['next_vote'] - 1}.")
    if candidate is not None and str(candidate).lower() not in ("abstain", "none", ""):
        candidate = int(candidate)
        cp = g.player(candidate)
        if cp.kind != "major" or not cp.alive:
            raise ActionError("You can only vote for a living major civilization.")
    else:
        candidate = None
    un["votes"][str(pid)] = candidate
    return {"voted_for": g.player(candidate).name if candidate is not None else "abstain"}


def _ai_vote(g, p):
    """How an AI civilization votes, from its opinion of the candidates."""
    from .diplomacy import opinion
    if p.kind == "city_state":
        return p.ally
    if p.kind == "major" and p.controller in ("human", "llm", "mcp"):
        return None
    known = [q.id for q in g.majors() if q.id != p.id and g.has_met(p.id, q.id)]
    if not known:
        return None
    best = max(opinion(g, p.id, q) for q in known)
    rng = g.state_rng("un_vote", p.id, g.turn)
    if best < -80 or (best < -40 and best + rng.randrange(40) < -40):
        return None
    return rng.choice([q for q in known if opinion(g, p.id, q) == best])


def hold_vote(g: "Game"):
    """Count the votes and declare a diplomatic victory if anyone reached the threshold."""
    un = _un(g)
    for p in _voters(g):
        if str(p.id) not in un["votes"]:
            if p.kind == "major" and p.controller in ("human", "llm", "mcp"):
                un["votes"][str(p.id)] = None     # did not vote: abstain
            else:
                un["votes"][str(p.id)] = _ai_vote(g, p)
    owner = un_owner(g)
    tally = Counter()
    for voter, cand in un["votes"].items():
        if cand is not None:
            tally[cand] += 2 if int(voter) == owner else 1
    needed = votes_needed(g)
    text = "No valid votes were cast."
    winner = None
    if tally:
        top = max(tally.values())
        leaders = [c for c, v in tally.items() if v == top]
        if top < needed:
            text = f"No world leader was elected (minimum {needed} votes; best {top})."
        elif len(leaders) > 1:
            text = "No world leader was elected (tie for first place)."
        else:
            winner = leaders[0]
            text = f"{g.player(winner).name} has been elected world leader with {top} votes!"
            if winner not in un["won"]:
                un["won"].append(winner)
    un["results"] = {"turn": g.turn, "tally": {g.player(c).name: v for c, v in tally.most_common()},
                     "votes_needed": needed, "winner": winner}
    un["processed_turn"] = g.turn
    g.emit("un_vote", "United Nations vote: " + text, None, results=un["results"])
    un["votes"] = {}
    un["next_vote"] = g.turn + turns_between_votes(g)


# ----------------------------------------------------------------------------
# Milestones
# ----------------------------------------------------------------------------
def _built_anywhere(g, name) -> bool:
    """Whether a wonder has been built by anybody."""
    return any(name in c.buildings for c in g.s.cities.values())


def _built_by(g, pid, name) -> bool:
    """Whether a particular civilization built it."""
    return any(name in c.buildings for c in g.player_cities(pid))


def _original_capitals_owned(g, pid) -> int:
    """How many civilizations' original capitals this one holds. The domination condition."""
    return sum(1 for c in g.player_cities(pid) if c.original_capital and g.player(c.founder).kind == "major")


def _civs_with_capitals(g) -> set:
    """Civilizations that still hold their own original capital."""
    s = {c.founder for c in g.s.cities.values() if c.original_capital and g.player(c.founder).kind == "major"}
    s |= {p.id for p in g.majors()}
    return s


def milestone_done(g: "Game", pid: int, m: str) -> bool:
    """Whether a victory milestone has been achieved."""
    from .policies import completed_branches
    from .uniques import placeholder
    ph, params = placeholder(m)
    if ph == "Build []":
        return _built_by(g, pid, params[0])
    if ph == "Add all [] in capital":
        return spaceship_status(g, pid)["complete"]
    if ph == "Destroy all players":
        return [p.id for p in g.majors()] == [pid]
    if ph == "Capture all capitals":
        return _original_capitals_owned(g, pid) == len(_civs_with_capitals(g))
    if ph == "Complete [] Policy branches":
        return completed_branches(g, pid) >= int(params[0])
    if ph == "Anyone should build []":
        return _built_anywhere(g, params[0])
    if ph == "Win diplomatic vote":
        return pid in _un(g)["won"]
    if ph == "Have highest score after max turns":
        return g.turn > g.total_turns() and pid == max(g.majors(), key=lambda p: score(g, p.id)["total"]).id
    return False


def enabled_victories(g: "Game") -> list[str]:
    """The victory types enabled in this game."""
    return [v for v in g.rules.victories if g.victory_enabled(v)]


def victory_progress(g: "Game", pid: int) -> dict:
    """How far a civilization is toward each victory type."""
    out = {}
    for v in enabled_victories(g):
        ms = g.rules.victories[v]["milestones"]
        done = []
        for m in ms:
            ok = milestone_done(g, pid, m)
            done.append({"milestone": m, "done": ok})
            if not ok:
                break
        out[v] = {"milestones": done, "total": len(ms), "completed": sum(1 for d in done if d["done"])}
    return out


def victory_achieved(g: "Game", pid: int) -> Optional[str]:
    """The victory this civilization has achieved, if any."""
    if g.player(pid).kind != "major" or not g.player(pid).alive:
        return None
    for v in enabled_victories(g):
        if all(milestone_done(g, pid, m) for m in g.rules.victories[v]["milestones"]):
            return v
    if g.civ_has(pid, U.TriggersVictory):
        return "Neutral"
    return None


VICTORY_TEXT = {
    "Scientific": "{n} launched its spaceship to Alpha Centauri — Scientific Victory!",
    "Cultural": "{n} completed the Utopia Project — Cultural Victory!",
    "Domination": "{n} controls every original capital — Domination Victory!",
    "Diplomatic": "{n} was elected world leader by the United Nations — Diplomatic Victory!",
    "Time": "The final turn has passed. {n} wins with the highest score — Time Victory!",
    "Neutral": "{n} has won the game!",
}


def declare_winner(g: "Game", pid: int, vtype: str, text: Optional[str] = None):
    """End the game with a winner."""
    if g.s.phase != "playing":
        return
    g.s.phase = "over"
    g.s.winner = pid
    g.s.victory = vtype
    g.emit("victory", text or VICTORY_TEXT.get(vtype, "{n} wins!").format(n=g.player(pid).name), None,
           winner=pid, victory=vtype)


def check_victory(g: "Game", pid: Optional[int] = None) -> bool:
    """Check whether anybody has won, and end the game if so."""
    if g.s.phase != "playing":
        return True
    order = [pid] if pid is not None else [p.id for p in g.majors()]
    for q in order:
        v = victory_achieved(g, q)
        if v:
            declare_winner(g, q, v)
            return True
    return False


def check_domination(g: "Game"):
    """Check the domination condition: one civilization holding every original capital."""
    if g.victory_enabled("Domination"):
        for p in g.majors():
            if milestone_done(g, p.id, "Capture all capitals"):
                declare_winner(g, p.id, "Domination")
                return
    alive = g.majors()
    if len(alive) == 1 and len(g.majors(alive_only=False)) > 1 and g.s.phase == "playing" \
            and g.victory_enabled("Domination"):
        declare_winner(g, alive[0].id, "Domination", f"{alive[0].name} is the last civilization standing — Domination Victory!")


def check_turn_limit(g: "Game"):
    """End the game on score if the turn limit has been reached."""
    if g.s.phase != "playing" or g.turn <= g.total_turns():
        return
    alive = g.majors()
    if not alive:
        return
    best = max(alive, key=lambda p: score(g, p.id)["total"])
    if g.victory_enabled("Time"):
        declare_winner(g, best.id, "Time", VICTORY_TEXT["Time"].format(n=best.name) +
                       f" (score {score(g, best.id)['total']})")
    else:
        g.s.phase = "over"
        g.emit("game_over", "The turn limit has been reached. The game ends with no winner.", None)


# ----------------------------------------------------------------------------
# Elimination
# ----------------------------------------------------------------------------
def is_defeated(g: "Game", pid: int) -> bool:
    """Whether a civilization has been eliminated."""
    p = g.player(pid)
    if p.kind == "barbarian" or g.player_cities(pid):
        return False
    if p.founded_city:
        return True
    return not g.player_units(pid) and g.turn > 0


def check_elimination(g: "Game", pid: int, by: Optional[int] = None):
    """Check whether any civilization has been eliminated, and record it."""
    from . import espionage, city_states
    p = g.player(pid)
    if not p.alive or not is_defeated(g, pid):
        return
    p.alive = False
    p.eliminated_turn = g.turn
    for u in list(g.player_units(pid)):
        g.remove_unit(u)
    for n in g.s.negotiations:
        if n["status"] == "open" and pid in (n["initiator"], n["responder"]):
            n["status"] = "cancelled"
    for d in g.s.deals:
        if d.get("active") and pid in d["parties"]:
            d["active"] = False
    espionage.remove_all_spies(g, pid)
    if p.kind == "city_state":
        if by is not None:
            city_states.on_destroyed(g, pid, by)
        p.ally = None
    g.invalidate()
    g.emit("eliminated", f"{p.name} has been destroyed!", None, player=pid)
    if p.kind == "major":
        check_domination(g)


# ----------------------------------------------------------------------------
# End-of-round processing (called by turns.end_round)
# ----------------------------------------------------------------------------
def end_round(g: "Game"):
    """End-of-round victory checks: domination, the turn limit, and elimination."""
    un = _un(g)
    if un["next_vote"] is not None and g.turn >= un["next_vote"] and g.victory_enabled("Diplomatic"):
        hold_vote(g)
    check_victory(g)
    check_turn_limit(g)


# ----------------------------------------------------------------------------
# Statistics & replay
# ----------------------------------------------------------------------------
def record_stats(g: "Game"):
    """Record this round's per-civilization statistics for the graphs and the replay."""
    from . import economy, research
    entry = {"turn": g.turn, "players": {}}
    for p in g.s.players:
        if p.kind != "major":
            continue
        if not p.alive:
            entry["players"][str(p.id)] = {"alive": False, "score": 0}
            continue
        y = economy.civ_stats(g, p.id)
        entry["players"][str(p.id)] = {
            "alive": True,
            "score": score(g, p.id)["total"],
            "cities": len(g.player_cities(p.id)),
            "population": sum(c.pop for c in g.player_cities(p.id)),
            "land": len(economy.owned_tiles(g, p.id)),
            "techs": len(p.techs),
            "policies": len(p.policies),
            "military": military_strength(g, p.id),
            "gold": int(p.gold),
            "gold_per_turn": round(y["gold"], 1),
            "science": round(y["science"], 1),
            "culture": round(y["culture"], 1),
            "faith": round(y["faith"], 1),
            "production": round(y["production"], 1),
            "happiness": economy.happiness(g, p.id)["total"],
            "era": research.player_era(g, p.id),
            "units": len(g.player_units(p.id)),
            "golden_age": p.golden_age_turns > 0,
        }
    g.s.stats.append(entry)


def record_frame(g: "Game"):
    """Compact snapshot of the dynamic map for the recap viewer."""
    n = g.grid.size
    owner = bytearray(n)
    imp = bytearray(n)
    route = bytearray(n)
    feat = bytearray(n)
    imp_ids = {k: i + 1 for i, k in enumerate(g.rules.improvements)}
    feat_ids = {k: i + 1 for i, k in enumerate(g.rules.terrains)}
    for i, t in enumerate(g.s.tiles):
        owner[i] = 255 if t.owner is None else min(254, t.owner)
        imp[i] = imp_ids.get(t.improvement, 0) if t.improvement else 0
        route[i] = (1 if t.route == "Road" else 2 if t.route == "Railroad" else 0) + (4 if t.route_pillaged else 0)
        f = [x for x in t.features if x != "Hill"]
        feat[i] = feat_ids.get(f[-1], 0) if f else 0
    explored = {str(p.id): base64.b64encode(bytes(p.explored)).decode() for p in g.s.players if p.kind == "major"}
    frame = {
        "turn": g.turn,
        "owner": base64.b64encode(bytes(owner)).decode(),
        "improvement": base64.b64encode(bytes(imp)).decode(),
        "route": base64.b64encode(bytes(route)).decode(),
        "feature": base64.b64encode(bytes(feat)).decode(),
        "cities": [[c.id, c.name, c.owner, c.idx, c.pop, g.player(c.owner).capital == c.id] for c in g.s.cities.values()],
        "units": [[u.type, u.owner, u.idx, u.hp] for u in g.s.units.values()],
        "explored": explored,
        "event_range": [g.frames[-1]["event_range"][1] if g.frames else 0, len(g.s.events)],
    }
    g.frames.append(frame)
