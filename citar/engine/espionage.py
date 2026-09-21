"""Espionage: spies, tech stealing, city-state election rigging, coups and counter-intelligence.
Port of UnCiv's EspionageManager, Spy and CityStateFunctions.holdElections (MPL-2.0).

Spies live in `player.spies` as dicts: {"name", "rank", "city" (city id or None = hideout), "action", "turns",
"progress"}. Spies are not map units; they are moved with the `move_spy` tool.
"""
from __future__ import annotations

from typing import Optional, TYPE_CHECKING

from . import unique_types as U
from .game import ActionError

if TYPE_CHECKING:
    from .game import Game

NONE, MOVING, NETWORK, SURVEILLANCE, STEALING, RIGGING, COUP, COUNTER, DEAD = (
    "None", "Moving", "Establishing Network", "Observing City", "Stealing Tech", "Rigging Elections", "Coup",
    "Counter-intelligence", "Dead")
SET_UP = {SURVEILLANCE, STEALING, RIGGING, COUP, COUNTER}
COUNTDOWN = {MOVING, NETWORK, DEAD}


def _k(g, name):
    """A tuning constant from the ruleset."""
    return g.rules.k[name]


def spy_city(g: "Game", spy: dict):
    """The city a spy is in, or None if it is at the hideout."""
    return g.city(spy["city"]) if spy.get("city") is not None else None


def starting_rank(g: "Game", pid: int) -> int:
    """The rank new spies start at, which some civilizations improve."""
    return 1 + sum(int(x.n(0)) for x in g.civ_uniques(pid, U.SpyStartingLevel))


def _spy_name(g: "Game", pid: int) -> str:
    """An unused name for a new spy."""
    used = {s["name"] for s in g.player(pid).spies}
    n = 1
    while f"Agent {n}" in used:
        n += 1
    return f"Agent {n}"


def add_spy(g: "Game", pid: int) -> dict:
    """Give a civilization a new spy."""
    spy = {"name": _spy_name(g, pid), "rank": starting_rank(g, pid), "city": None, "action": NONE, "turns": 0,
           "progress": 0}
    g.player(pid).spies.append(spy)
    g.emit("spy", f"We have recruited {spy['name']} as a spy!", [pid])
    return spy


def on_trigger(g: "Game", u, pid: int) -> bool:
    """Handle a trigger that grants a spy, such as reaching an era."""
    from .research import player_era
    p = g.player(pid)
    if p.kind != "major" or not g.espionage_enabled:
        return False
    if u.ph == U.OneTimeGainSpy:
        add_spy(g, pid)
        return True
    if u.ph == U.OneTimeSpiesLevelUp:
        for s in p.spies:
            level_up(g, pid, s, int(u.n(0)))
        return True
    if u.ph == U.OneTimeGlobalSpiesWhenEnteringEra:
        era = g.rules.era_list[player_era(g, pid)]
        for q in g.majors():
            earned = q.flags.setdefault("eras_spy_earned", [])
            if era not in earned:
                earned.append(era)
                add_spy(g, q.id)
        return True
    return False


def level_up(g: "Game", pid: int, spy: dict, amount: int = 1):
    """Promote a spy, up to the maximum rank."""
    mx = _k(g, "max_spy_rank")
    if spy["rank"] >= mx:
        return
    n = min(amount, mx - spy["rank"])
    spy["rank"] += n
    g.emit("spy", f"Your spy {spy['name']} has leveled up!" if n == 1 else
           f"Your spy {spy['name']} has leveled up {n} times!", [pid])


def effective_rank(g: "Game", pid: int, spy: dict) -> int:
    """A spy's rank including bonuses from the city it is in."""
    from .cities import city_uniques
    from .uniques import city_matches
    mx = _k(g, "max_spy_rank")
    r = spy["rank"]
    c = spy_city(g, spy)
    if c is not None:
        for x in city_uniques(g, c, U.CounterIntelligenceSpyRankBonus):
            if city_matches(g, c, x.p(0), pid) and x.p(2).lower() == spy["action"].lower():
                r += int(x.n(1))
    return max(1, min(mx, r))


def skill_percent(g: "Game", pid: int, spy: dict) -> int:
    """A spy's skill as a percentage, from its effective rank."""
    return effective_rank(g, pid, spy) * _k(g, "spy_rank_skill_percent_bonus")


def efficiency(g: "Game", pid: int, spy: dict) -> float:
    """How fast this spy works here, after the city's own modifiers."""
    from .cities import city_uniques
    from .uniques import city_matches
    c = spy_city(g, spy)
    friendly, enemy = [], []
    if c is None:
        friendly = g.civ_uniques(pid, U.SpyEffectiveness)
    elif c.owner == pid:
        friendly = city_uniques(g, c, U.SpyEffectiveness)
    else:
        friendly = g.civ_uniques(pid, U.SpyEffectiveness)
        enemy = city_uniques(g, c, U.EnemySpyEffectiveness)
    t = (100 + sum(x.n(0) for x in friendly if c is None or city_matches(g, c, x.p(1), pid))) / 100
    t *= (100 + sum(x.n(0) for x in enemy if c is not None and city_matches(g, c, x.p(1)))) / 100
    return max(0.0, t)


def spy_in_city(g: "Game", pid: int, city) -> Optional[dict]:
    """This civilization's spy in a city, if it has one there."""
    return next((s for s in g.player(pid).spies if s.get("city") == city.id), None)


def spies_in_city(g: "Game", city) -> list[tuple[int, dict]]:
    """Every spy in a city, with whose it is."""
    return [(p.id, s) for p in g.s.players for s in p.spies if s.get("city") == city.id]


def techs_to_steal(g: "Game", pid: int, other: int) -> list[str]:
    """Technologies this civilization could steal from another."""
    from .research import can_research
    return [t for t in g.player(other).techs if not g.has_tech(pid, t) and can_research(g, pid, t)]


def _seed(g, spy, city, pid):
    """A deterministic generator for one spy action, so replays reproduce."""
    return g.state_rng("spy", spy["name"], pid, city.idx, g.turn)


def _set(spy, action, turns=0):
    """Set a spy's action and countdown."""
    spy["action"] = action
    spy["turns"] = turns


def move_to(g: "Game", spy: dict, city):
    """Move a spy to a city, or back to the hideout."""
    if city is None:
        spy["city"] = None
        _set(spy, NONE)
        return
    spy["city"] = city.id
    _set(spy, MOVING, 1)


def can_move_to(g: "Game", pid: int, spy: dict, city) -> Optional[str]:
    """Why a spy cannot go to this city, or None."""
    if spy["action"] == DEAD:
        return f"{spy['name']} is dead; a replacement is being recruited."
    if spy.get("city") == city.id:
        return None
    if not g.player(pid).explored[city.idx]:
        return "You have not explored that city."
    if g.player(city.owner).kind == "barbarian":
        return "Spies cannot be sent to barbarian cities."
    if spy_in_city(g, pid, city) is not None:
        return "You already have a spy in that city."
    return None


def city_removed(g: "Game", city, reason: str = "captured"):
    """Deal with the spies in a city that has just been captured or destroyed."""
    for pid, s in spies_in_city(g, city):
        g.emit("spy", f"After the city of {city.name} was {reason}, your spy {s['name']} has fled back to our hideout.",
               [pid])
        move_to(g, s, None)


def remove_all_spies(g: "Game", pid: int):
    """Recall every spy a civilization has, as when it is eliminated."""
    for s in g.player(pid).spies:
        move_to(g, s, None)


def _can_steal(g, pid, spy) -> str:
    """Why a spy cannot steal technology here, or an empty string if it can."""
    from .cities import city_stats
    c = spy_city(g, spy)
    if not techs_to_steal(g, pid, c.owner):
        return "none"
    if city_stats(g, c)["total"]["science"] <= 0:
        return "no_science"
    return "yes"


def _steal_progress(g, pid, spy) -> int:
    """How much progress a spy makes toward a theft this turn."""
    from .cities import city_stats
    c = spy_city(g, spy)
    prog = city_stats(g, c)["total"]["science"]
    prog *= (spy["rank"] * _k(g, "spy_rank_steal_percent_bonus") + 75) / 100
    prog *= efficiency(g, pid, spy)
    spy["progress"] += int(prog)
    cost = max(g.rules.techs[t]["cost"] for t in techs_to_steal(g, pid, c.owner))
    cost *= _k(g, "spy_tech_steal_cost_modifier") * g.speed["scienceCostModifier"]
    remaining = cost - spy["progress"]
    if remaining <= 0:
        return 0
    return -(-remaining // max(prog, 1e-9))


def _steal_tech(g, pid, spy):
    """Complete a technology theft, with the chance of being caught."""
    from .research import add_tech
    from .diplomacy import add_opinion
    c = spy_city(g, spy)
    other = c.owner
    rng = _seed(g, spy, c, pid)
    options = techs_to_steal(g, pid, other)
    stolen = rng.choice(options) if options else None
    result = rng.randrange(300) - skill_percent(g, pid, spy)
    defender = spy_in_city(g, other, c)
    if defender:
        result += skill_percent(g, other, defender)
    me = g.player(pid).name
    if result >= 200:
        g.emit("spy", f"A spy from {me} was found and killed trying to steal technology in {c.name}!", [other], idx=c.idx)
    elif stolen and 0 <= result < 100:
        g.emit("spy", f"An unidentified spy stole the technology {stolen} from {c.name}!", [other], idx=c.idx)
    elif stolen and result >= 100:
        g.emit("spy", f"A spy from {me} stole the technology {stolen} from {c.name}!", [other], idx=c.idx)
    if result < 200 and stolen:
        add_tech(g, pid, stolen, source="espionage")
        g.emit("spy", f"Your spy {spy['name']} stole the technology {stolen} from {c.name}!", [pid], idx=c.idx)
        level_up(g, pid, spy)
    if result >= 200:
        g.emit("spy", f"Your spy {spy['name']} was killed trying to steal technology in {c.name}!", [pid], idx=c.idx)
        if defender:
            level_up(g, other, defender)
        kill(g, spy)
    else:
        spy["progress"] = 0
        _set(spy, STEALING)
    if result >= 100:
        add_opinion(g, other, pid, "spied_on_us", -15)


def kill(g: "Game", spy: dict):
    """Kill a spy: it leaves the city and is gone."""
    move_to(g, spy, None)
    _set(spy, DEAD, 5)
    spy["rank"] = 1


def can_coup(g: "Game", pid: int, spy: dict) -> bool:
    """Whether this spy is in a position to attempt a coup."""
    c = spy_city(g, spy)
    return c is not None and g.player(c.owner).kind == "city_state" and spy["action"] in SET_UP \
        and g.player(c.owner).ally != pid


def coup_chance(g: "Game", pid: int, spy: dict, include_unknown: bool = True) -> float:
    """The probability a coup succeeds, from rank and the current ally's influence."""
    from .city_states import influence
    c = spy_city(g, spy)
    cs = g.player(c.owner)
    diff = influence(g, cs.id, cs.ally) if cs.ally is not None else 60
    diff -= influence(g, cs.id, pid)
    pct = 50 - diff / 2
    defender = spy_in_city(g, cs.ally, c) if include_unknown and cs.ally is not None else None
    ranks = skill_percent(g, pid, spy) - (skill_percent(g, cs.ally, defender) if defender else 0)
    pct += ranks / 2
    return max(0.0, min(85.0, pct)) / 100


def _initiate_coup(g, pid, spy):
    """Resolve a coup attempt: a new ally, or a dead spy."""
    from . import city_states
    from .diplomacy import add_opinion
    if not can_coup(g, pid, spy):
        _set(spy, RIGGING, 10)
        return
    c = spy_city(g, spy)
    cs = g.player(c.owner)
    ally = cs.ally
    chance = coup_chance(g, pid, spy, True)
    me = g.player(pid).name
    if _seed(g, spy, c, pid).random() <= chance:
        prev = city_states.influence(g, cs.id, ally) if ally is not None else 80
        city_states.set_influence(g, cs.id, pid, prev)
        g.emit("spy", f"Your spy {spy['name']} successfully staged a coup in {cs.name}!", [pid], idx=c.idx)
        if ally is not None:
            city_states.add_influence(g, cs.id, ally, -20)
            g.emit("spy", f"A spy from {me} successfully staged a coup in our former ally {cs.name}!", [ally], idx=c.idx)
            add_opinion(g, ally, pid, "spied_on_us", -15)
        for q in g.majors():
            if q.id in (ally, pid) or not g.has_met(q.id, cs.id):
                continue
            g.emit("spy", f"A spy from {me} successfully staged a coup in {cs.name}!", [q.id], idx=c.idx)
            city_states.add_influence(g, cs.id, q.id, -10)
        _set(spy, RIGGING, 10)
        city_states.update_ally(g, cs.id)
    else:
        defender = spy_in_city(g, ally, c) if ally is not None else None
        city_states.add_influence(g, cs.id, pid, -20)
        if ally is not None:
            g.emit("spy", f"A spy from {me} failed to stage a coup in our ally {cs.name} and was killed!", [ally],
                   idx=c.idx)
            add_opinion(g, ally, pid, "spied_on_us", -10)
        g.emit("spy", f"Our spy {spy['name']} failed to stage a coup in {cs.name} and was killed!", [pid], idx=c.idx)
        kill(g, spy)
        if defender:
            level_up(g, ally, defender)


def _election_turns(g, cs: int) -> int:
    """Turns until this city-state's next election, which rigging influences."""
    return g.player(cs).flags.get("election_in", 1)


def spy_end_turn(g: "Game", pid: int, spy: dict):
    """Advance one spy's action by a turn."""
    a = spy["action"]
    if a in COUNTDOWN:
        spy["turns"] -= 1
        if spy["turns"] > 0:
            return
    if a == NONE:
        return
    c = spy_city(g, spy)
    if a != DEAD and c is None:
        move_to(g, spy, None)
        return
    if a == MOVING:
        if c.owner == pid:
            _set(spy, COUNTER, 10)
        else:
            _set(spy, NETWORK, 3)
    elif a == NETWORK:
        if g.player(c.owner).kind == "city_state":
            _set(spy, RIGGING, max(0, _election_turns(g, c.owner) - 1))
        elif c.owner == pid:
            _set(spy, COUNTER, 10)
        else:
            spy["progress"] = 0
            _set(spy, STEALING)
    elif a == SURVEILLANCE:
        if g.player(c.owner).kind == "major" and _can_steal(g, pid, spy) == "yes":
            _set(spy, STEALING)
    elif a == STEALING:
        r = _can_steal(g, pid, spy)
        if r != "yes":
            _set(spy, SURVEILLANCE)
            if r == "none":
                g.emit("spy", f"Your spy {spy['name']} cannot steal any more techs from {g.player(c.owner).name} as we've "
                              f"already researched all the technology they know!", [pid])
            return
        spy["turns"] = int(_steal_progress(g, pid, spy))
        if spy["turns"] == 0:
            _steal_tech(g, pid, spy)
    elif a == RIGGING:
        spy["turns"] = _election_turns(g, c.owner) - 1
    elif a == COUP:
        _initiate_coup(g, pid, spy)
    elif a == DEAD:
        old = spy["name"]
        spy["name"] = _spy_name(g, pid)
        _set(spy, NONE)
        spy["rank"] = starting_rank(g, pid)
        g.emit("spy", f"We have recruited a new spy named {spy['name']} after {old} was killed.", [pid])
    elif a == COUNTER:
        spy["turns"] -= 1


def end_turn(g: "Game", pid: int):
    """Advance every spy, and hold any elections that are due."""
    if not g.espionage_enabled:
        return
    for spy in list(g.player(pid).spies):
        spy_end_turn(g, pid, spy)


def city_state_election_tick(g: "Game", cs: int):
    """TurnManager: the TurnsTillCityStateElection flag counts down; elections are held when it reaches 0."""
    p = g.player(cs)
    n = _k(g, "city_state_election_turns")
    if "election_in" not in p.flags:
        p.flags["election_in"] = g.state_rng("election", cs).randrange(n + 1)
        return
    p.flags["election_in"] -= 1
    if p.flags["election_in"] <= 0:
        hold_elections(g, cs)


def hold_elections(g: "Game", cs: int):
    """Run a city-state's election, weighted by who has been rigging it."""
    from . import city_states
    p = g.player(cs)
    p.flags["election_in"] = _k(g, "city_state_election_turns")
    cap = g.city(p.capital) if p.capital is not None else None
    if cap is None:
        return
    spies = [(pid, s) for pid, s in spies_in_city(g, cap) if s["action"] == RIGGING]
    if not spies:
        return
    parties = [(pid, s) for pid, s in spies] + [(None, None)]
    weights = []
    for pid, s in parties:
        if s is None:
            weights.append(20.0)
        else:
            weights.append(max(0.0, city_states.influence(g, cs, pid) / 2 + skill_percent(g, pid, s) * efficiency(g, pid, s)))
    rng = g.state_rng("election", cs, g.turn)
    winner = rng.choices(parties, weights=weights)[0][0] if sum(weights) > 0 else None
    if winner is None:
        for pid, _s in spies:
            city_states.add_influence(g, cs, pid, -5)
            g.emit("spy", f"Your spy lost the election in {cap.name}!", [pid], idx=cap.idx)
        return
    ally = p.ally
    riggers = {pid for pid, _ in spies}
    for q in g.majors():
        if not g.has_met(cs, q.id):
            continue
        city_states.add_influence(g, cs, q.id, 20 if q.id == winner else -5)
        if q.id == winner:
            g.emit("spy", f"Your spy successfully rigged the election in {p.name}!", [q.id], idx=cap.idx)
        elif q.id in riggers:
            g.emit("spy", f"Your spy lost the election in {p.name} to {g.player(winner).name}!", [q.id], idx=cap.idx)
        elif q.id == ally:
            g.emit("spy", f"The election in {p.name} was rigged by {g.player(winner).name}!", [q.id], idx=cap.idx)


def visible_tiles(g: "Game", pid: int) -> set[int]:
    """Tiles a civilization can see through its spies."""
    out = set()
    for s in g.player(pid).spies:
        c = spy_city(g, s)
        if c is not None and s["action"] in SET_UP:
            out.update(g.grid.within(c.idx, 1))
    return out


# ----------------------------------------------------------------------------
# Player actions
# ----------------------------------------------------------------------------
def _get_spy(g, pid, name) -> dict:
    """One of this civilization's spies by name, raising if there is no such spy."""
    spies = g.player(pid).spies
    if not spies:
        raise ActionError("You have no spies. Spies are recruited when a civilization enters a new era "
                          "(from the Renaissance) and from some wonders.")
    for s in spies:
        if s["name"].lower() == str(name).strip().lower():
            return s
    raise ActionError(f"No spy named '{name}'. Your spies: {', '.join(s['name'] for s in spies)}.")


def move_spy(g: "Game", pid: int, name: str, city_id=None) -> dict:
    """Send a spy to a city, or recall it."""
    if not g.espionage_enabled:
        raise ActionError("Espionage is disabled in this game.")
    spy = _get_spy(g, pid, name)
    if city_id is None or str(city_id).lower() in ("hideout", "none", ""):
        if spy["action"] == DEAD:
            raise ActionError(f"{spy['name']} is dead.")
        move_to(g, spy, None)
        return {"spy": spy["name"], "location": "hideout"}
    c = g.city(int(city_id))
    if c is None:
        raise ActionError(f"No city with id {city_id}.")
    reason = can_move_to(g, pid, spy, c)
    if reason:
        raise ActionError(reason)
    if spy.get("city") == c.id:
        return {"spy": spy["name"], "location": c.name, "action": spy["action"]}
    move_to(g, spy, c)
    return {"spy": spy["name"], "moving_to": c.name, "arrives_in_turns": 1}


def stage_coup(g: "Game", pid: int, name: str) -> dict:
    """Order a spy to attempt a coup at the end of this turn."""
    spy = _get_spy(g, pid, name)
    if not can_coup(g, pid, spy):
        raise ActionError("A coup needs a set-up spy in the capital of a city-state that is not allied with you.")
    chance = coup_chance(g, pid, spy, False)
    _set(spy, COUP)
    return {"spy": spy["name"], "coup_at_end_of_turn": True, "estimated_success_chance": round(chance * 100)}


def spy_view(g: "Game", pid: int, spy: dict) -> dict:
    """One spy as the client shows it."""
    c = spy_city(g, spy)
    v = {"name": spy["name"], "rank": spy["rank"], "location": c.name if c else "hideout",
         "city_id": c.id if c else None, "action": spy["action"]}
    if spy["action"] in (MOVING, NETWORK, DEAD, STEALING, RIGGING) and spy["turns"] > 0:
        v["turns"] = spy["turns"]
    if c is not None and can_coup(g, pid, spy):
        v["coup_chance_percent"] = round(coup_chance(g, pid, spy, False) * 100)
    return v


def espionage_view(g: "Game", pid: int) -> dict:
    """Every spy this civilization has, and whether espionage is on at all."""
    return {"enabled": g.espionage_enabled, "spies": [spy_view(g, pid, s) for s in g.player(pid).spies]}
