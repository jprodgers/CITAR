"""Test operations: what a rule script does to a game that no player or editor may.

They replace the internals Python's tests poked (``g.remove_unit``, ``p.auto[...] = ...``, ``g.s.turn = ...``), so
that a script means the same on both engines. The Rust engine's side is ``crates/citar-engine/src/api/testops.rs``,
and ``tests/rules/README.md`` documents each. Reached through ``EngineGame.test_ops`` (not in the facade's
``__all__``: tests only), which also does ``reload``, since that replaces the game.

Only the operations the Rust engine has ported are here: a script using another could not pass on both.
"""
from __future__ import annotations

from typing import Callable

from .game import ActionError, Game
from .state import AUTO_DECISIONS, BOT_MANAGED, HUMANLIKE_CONTROLLERS

OPS: dict[str, tuple[Callable, str]] = {}


def op(name: str, doc: str):
    """Register a test operation with its parameters."""
    def deco(fn):
        """Record the operation and its parameters."""
        OPS[name] = (fn, doc)
        return fn
    return deco


def ops_help() -> list[dict]:
    """Every test operation with its parameters, sorted by name (``reload`` included, which EngineGame does)."""
    rows = [{"op": k, "params": doc} for k, (_, doc) in OPS.items()]
    rows.append({"op": "reload", "params": "the game is saved and the save loaded, as a host does"})
    return sorted(rows, key=lambda r: r["op"])


def names() -> set:
    """The test operations' names."""
    return set(OPS) | {"reload"}


@op("clear_units", "player (id, list of ids, or 'all' for every player, the barbarians included): every unit of "
                   "those players is removed")
def _clear_units(g: Game, o: dict):
    """Remove every unit of the players named."""
    from .scenario import _players
    v = o.get("player")
    pids = [p.id for p in g.s.players] if v == "all" else _players(g, v, majors_only=False)
    removed = sorted(u.id for pid in pids for u in g.player_units(pid))
    for uid in removed:
        u = g.unit(uid)
        if u is not None:
            g.remove_unit(u)
    return {"removed": removed}


@op("set_turn", "turn (1 or more): the game's turn number")
def _set_turn(g: Game, o: dict):
    """Set the turn number."""
    try:
        turn = int(o.get("turn"))
    except (TypeError, ValueError):
        turn = 0
    if turn < 1:
        raise ActionError("turn must be a whole number, 1 or more.")
    g.s.turn = turn
    g.invalidate()
    return {"turn": turn}


@op("unmeet", "a, b: the two no longer know each other")
def _unmeet(g: Game, o: dict):
    """Two players forget they have met."""
    from .scenario import _pid
    a, b = _pid(g, o.get("a"), majors_only=False), _pid(g, o.get("b"), majors_only=False)
    if a == b:
        raise ActionError("A player cannot forget itself.")
    for x, y in ((a, b), (b, a)):
        met = g.player(x).met
        if y in met:
            met.remove(y)
    g.invalidate()
    return {}


@op("set_controller", "player, controller; optional handicap, auto: hands the seat to another driver")
def _set_controller(g: Game, o: dict):
    """Hand a seat to another driver, with the host's checks of the handicap and the automatic decisions."""
    from .scenario import _pid
    pid = _pid(g, o.get("player"), majors_only=False)
    controller = o.get("controller")
    if controller not in HUMANLIKE_CONTROLLERS + BOT_MANAGED:
        raise ActionError(f"Unknown controller {controller!r}.")
    try:
        g.player(pid).set_controller(controller, o.get("handicap"), o.get("auto"))
    except ValueError as e:
        raise ActionError(str(e))
    g.invalidate()
    return {}


@op("set_auto", "player, decision (un_vote, conquest or free_picks), on (bool): the engine takes the decision for "
                "the civilization, or not, until its controller changes")
def _set_auto(g: Game, o: dict):
    """Change one automatic decision of a seat for now, as play does."""
    from .scenario import _pid
    pid = _pid(g, o.get("player"), majors_only=False)
    decision = o.get("decision")
    if decision not in AUTO_DECISIONS:
        raise ActionError(f"Unknown decision {decision!r}.")
    if not isinstance(o.get("on"), bool):
        raise ActionError("on must be true or false.")
    g.player(pid).auto[decision] = o["on"]
    g.invalidate()
    return {}


def _clock(g: Game) -> dict:
    """Where the game is in time, as the turn operations report it."""
    return {"turn": g.turn, "current": g.s.current}


@op("end_turn", "optional player (the current one by default): that player's turn ends, and play moves on to the "
                "next major civilization's")
def _end_turn(g: Game, o: dict):
    """End a player's turn, the current one's unless another is named, as the host does."""
    from .scenario import _pid
    pid = g.s.current if o.get("player") is None else _pid(g, o.get("player"), majors_only=False)
    g.end_turn(pid)
    return _clock(g)


@op("end_round", "every remaining turn of the round ends, and the round with them")
def _end_round(g: Game, o: dict):
    """End every turn left in the round, and the round."""
    start = g.turn
    while g.s.phase == "playing" and g.turn == start:
        g.end_turn(g.s.current)
    return _clock(g)


@op("force_turn", "player: it is that player's turn now, started")
def _force_turn(g: Game, o: dict):
    """Make it a player's turn now and start it (EngineGame.force_turn).

    Refused, as the Rust engine refuses it, for a player who has been eliminated and in a game that is over, which
    EngineGame.force_turn allowed (tests/rules/intended.toml: force-turn-only-for-the-living).
    """
    from .scenario import _pid
    pid = _pid(g, o.get("player"), majors_only=False)
    if not g.player(pid).alive:
        raise ActionError(f"{g.player(pid).name} has been eliminated and plays no turns.")
    if g.s.phase != "playing":
        raise ActionError("The game is over.")
    if g.s.current != pid:
        g.s.current = pid
        g.s.turn_started = False
        g.begin_turn()
        # A turn begun anew is its driver's to play again: Rust's begin_turn clears the drive's mark too.
        g._drive_mark = None
    return _clock(g)


@op("complete_construction", "city: what it is building completes now")
def _complete_construction(g: Game, o: dict):
    """Finish what a city builds now, whatever production it has stored, as its turn would."""
    from . import cities
    try:
        c = g.city(int(o.get("city")))
    except (TypeError, ValueError):
        c = None
    if c is None:
        raise ActionError("No such city.")
    name = cities.current_construction(c)
    if name is None or name in cities.PERPETUAL:
        raise ActionError(f"{c.name} is building nothing that completes.")
    if not cities.complete_construction(g, c, name):
        raise ActionError("No room to place the unit.")
    return {"completed": name}


def _unit(g: Game, o: dict):
    """The unit an operation names by ``unit``."""
    try:
        u = g.unit(int(o.get("unit")))
    except (TypeError, ValueError):
        u = None
    if u is None:
        raise ActionError("No such unit.")
    return u


def _whole(o: dict, key: str) -> int:
    """A whole-number parameter, refused with the Rust engine's sentence."""
    try:
        return int(o[key])
    except (TypeError, ValueError):
        raise ActionError(f"{key} must be a whole number in range, not {o[key]!r}.")


@op("set_unit", "unit; any of hp, moves, xp, x and y, promotions (the list it then has), carrier (a unit on its "
                "tile that carries it, or null)")
def _set_unit(g: Game, o: dict):
    """Set a unit's fields, as the tests poked them: health, movement (move-scale units), experience, its tile,
    its promotions and the unit that carries it."""
    from .scenario import _idx, _name
    u = _unit(g, o)
    hp = max(1, min(100, _whole(o, "hp"))) if o.get("hp") is not None else None
    moves = max(0, _whole(o, "moves")) if o.get("moves") is not None else None
    xp = max(0, _whole(o, "xp")) if o.get("xp") is not None else None
    at = _idx(g, o) if ("x" in o or "y" in o) else None
    promos = None
    if o.get("promotions") is not None:
        if not isinstance(o["promotions"], list):
            raise ActionError("promotions must be a list of promotion names.")
        promos = [_name(g, "promotion", p) for p in o["promotions"]]
    if at is not None:
        g.place_unit(u, at)
    if "carrier" in o:
        if o["carrier"] is None:
            u.carried_by = None
        else:
            c = g.unit(_whole(o, "carrier"))
            if c is None:
                raise ActionError("No such carrier.")
            if c.idx != u.idx:
                raise ActionError(f"The game refused (unit {u.id} is on tile {u.idx}, its carrier {c.id} on another).")
            u.carried_by = c.id
    if hp is not None:
        u.hp = hp
    if moves is not None:
        u.moves = moves
    if xp is not None:
        u.xp = xp
    if promos is not None:
        u.promotions = list(dict.fromkeys(promos))
    g.invalidate()
    return {}


@op("ready_unit", "unit: full moves and no orders")
def _ready_unit(g: Game, o: dict):
    """Ready a unit to act: its full movement, and no orders, attacks or action this turn."""
    from .movement import max_moves
    u = _unit(g, o)
    u.moves = max_moves(g, u)
    u.activity, u.goto, u.path, u.order_wait, u.attacks, u.acted = None, None, None, 0, 0, False
    g.invalidate()
    return {"moves": u.moves}


@op("found_religion", "unit, name, beliefs: the great prophet founds a religion where it stands, and is spent")
def _found_religion(g: Game, o: dict):
    """A great prophet founds a religion where it stands, as its unit action does."""
    from . import religion
    u = _unit(g, o)
    return religion.found_religion(g, u.owner, u, str(o.get("name") or ""), list(o.get("beliefs") or []))


@op("enhance_religion", "unit, beliefs: the great prophet enhances its owner's religion where it stands, and is spent")
def _enhance_religion(g: Game, o: dict):
    """A great prophet enhances its owner's religion where it stands, as its unit action does."""
    from . import religion
    u = _unit(g, o)
    return religion.enhance_religion(g, u.owner, u, list(o.get("beliefs") or []))


@op("enter_ruins", "unit: the unit explores the ancient ruins it stands on")
def _enter_ruins(g: Game, o: dict):
    """A unit explores the ancient ruins it stands on, as moving onto them does."""
    from . import ruins
    u = _unit(g, o)
    if g.s.tiles[u.idx].improvement != ruins.RUINS:
        raise ActionError("There are no ancient ruins here.")
    return {"found": bool(ruins.enter(g, u, u.idx))}


@op("attack_as", "unit, x, y: the unit attacks the tile, whoever's turn it is")
def _attack_as(g: Game, o: dict):
    """A unit attacks a tile as the ``attack`` tool would have it, whoever's turn it is: a nuclear weapon detonates,
    an aircraft strikes, anything else attacks."""
    from . import combat
    from . import unique_types as U
    from .scenario import _idx
    u = _unit(g, o)
    idx = _idx(g, o)
    ud = g.udef(u)
    if ud["_umap"].get(U.NuclearWeapon):
        return combat.nuke(g, u, idx)
    if ud["_domain"] == "Air":
        return combat.air_strike(g, u, idx)
    return combat.attack(g, u, idx)


@op("capture_civilian", "unit (or player, the barbarians included), x, y: the unit, or the player, takes the "
                        "civilian on the tile")
def _capture_civilian(g: Game, o: dict):
    """A unit, or a player (the barbarians among them), takes the civilian on a tile, as the tests poked it: the
    captured unit's new id, or None when it was destroyed instead. Only the captor's owner decides what becomes of
    the civilian (units.capture_civilian reads nothing else of the captor)."""
    from types import SimpleNamespace
    from . import units
    from .scenario import _idx
    if o.get("unit") is not None or o.get("player") is None:
        owner = _unit(g, o).owner
    else:
        owner = _whole(o, "player")
        if not 0 <= owner < len(g.s.players):
            raise ActionError("No such player.")
    v = g.civilian_at(_idx(g, o))
    if v is None:
        raise ActionError("There is no civilian there.")
    before = {x.id for x in g.player_units(owner)}
    units.capture_civilian(g, SimpleNamespace(owner=owner), v)
    new = sorted(x.id for x in g.player_units(owner) if x.id not in before)
    return {"unit": new[0] if new else None}


@op("automate", "player: the player's units carry out their standing orders now (moves, exploring, automated workers, "
                "sleepers waking), as at the start of its turn")
def _automate(g: Game, o: dict):
    """A player's units carry out their standing orders now, as at the start of its turn."""
    from . import automation
    from .scenario import _pid
    automation.run_unit_orders(g, _pid(g, o.get("player")))
    return {}


@op("progress_builds", "player; optional turns (1 by default): the player's workers do that many turns of work, as at "
                       "the end of its turns")
def _progress_builds(g: Game, o: dict):
    """A player's workers do some turns of work, as at the end of each of its turns; their movement is left as it is."""
    from . import workers
    from .scenario import _pid
    pid = _pid(g, o.get("player"))
    turns = _whole(o, "turns") if o.get("turns") is not None else 1
    for _ in range(max(0, turns)):
        workers.progress_builds(g, pid)
    return {}


@op("set_difficulty", "player, difficulty (a level's name): the seat's own difficulty, as the host sets it; ok is "
                      "false, and nothing changes, for a name that is no level")
def _set_difficulty(g: Game, o: dict):
    """Give a seat its own difficulty, as EngineGame.set_difficulty does."""
    from .scenario import _pid
    pid = _pid(g, o.get("player"), majors_only=False)
    level = g.rules.resolve("difficulty", str(o.get("difficulty")))
    if not level:
        return {"ok": False}
    g.player(pid).difficulty = level
    g.invalidate()
    return {"ok": True}


@op("debug", "action (meet_all, reveal or gold): the host's developer shortcut")
def _debug(g: Game, o: dict):
    """A developer shortcut, as EngineGame.debug takes it."""
    from . import visibility
    action = o.get("action")
    if action == "meet_all":
        for a in g.majors():
            for b in g.majors():
                g.meet(a.id, b.id)
    elif action == "reveal":
        for p in g.majors():
            visibility.reveal_tiles(g, p.id, range(g.grid.size))
    elif action == "gold":
        for p in g.majors():
            p.gold += 500
    else:
        raise ActionError("Unknown debug action.")
    g.invalidate()
    return {}


def _drive_answers(g: Game, drivers: set, deferring: set, answer, asked: dict):
    """Put every open negotiation waiting on a driven seat to its driver, round after round while the answers bring
    more. ``asked`` holds, by negotiation, the entries it had when last put to a driver, the seat it waited on, and
    whether that driver left it to the host: a negotiation is put to a driver once in a drive for each entry it has,
    and one that has closed is forgotten."""
    from . import tools
    for nid in [k for k in asked if g.s.negotiations[k - 1]["status"] != "open"]:
        del asked[nid]
    for _ in range(64):
        waiting = [(n["id"], n["awaiting"], len(n["history"])) for n in g.s.negotiations
                   if n["status"] == "open" and n["awaiting"] in drivers
                   and asked.get(n["id"], (None, None, False))[:2] != (len(n["history"]), n["awaiting"])]
        if not waiting:
            return
        for nid, who, length in waiting:
            if g.s.phase != "playing":
                return
            n = g.s.negotiations[nid - 1]
            if n["status"] != "open" or n["awaiting"] != who or len(n["history"]) != length:
                continue
            asked[nid] = (length, who, who in deferring)
            if who in deferring or answer is None:
                continue
            try:
                tools.execute(g, who, "respond_negotiation",
                              {"negotiation_id": nid, "action": answer, "message": "(test driver)"})
            except ActionError:
                pass


def _left_to_host(n: dict, asked: dict) -> bool:
    """Whether the driver of the seat negotiation ``n`` waits on left it to the host, and it has not moved since."""
    a = asked.get(n["id"])
    return a is not None and a[2] and a[0] == len(n["history"]) and a[1] == n["awaiting"]


@op("drive", "drivers (the players a test driver plays: it does nothing with its turns); optional answer (what the "
             "driver answers a negotiation waiting on it: reject by default, accept, reply, or none to leave it), "
             "defer (drivers that leave what waits on them to the host, as a hybrid seat's bot leaves it to its "
             "model), seat_limit: the host drives the game until it has something to do; why it stopped")
def _drive(g: Game, o: dict):
    """The Rust engine's Game::drive with a test driver at each seat named, which Python never had: the host plays
    the driven seats' turns until it has something to do. A driver plays nothing and answers what waits on it, or
    leaves it to the host (``defer``); the game stops at a seat with no driver (external), after a hybrid seat's
    driver has played (hybrid_diplomat), when the seat whose driver has played is in a negotiation waiting on the
    host, on a seat with no driver or one whose driver left it to the host (awaiting_reply), after seat_limit driven
    turns (seat_limit), and when the game is over (game_over). Where a stop inside a turn left the game is kept on
    the game object, and carried across a ``reload`` (EngineGame.test_ops), so the next drive goes on from there."""
    from .scenario import _pid

    def seats(key):
        """The players named under ``key``."""
        v = o.get(key)
        return set() if v is None else set(_pid(g, x, majors_only=True) for x in (v if isinstance(v, list) else [v]))

    drivers = seats("drivers")
    deferring = seats("defer") & drivers
    answer = o.get("answer", "reject")
    if answer == "none":
        answer = None
    elif answer not in ("reject", "accept", "reply"):
        raise ActionError(f"answer must be reject, accept, reply or none, not '{answer}'.")
    try:
        limit = int(o.get("seat_limit") or 0)
    except (TypeError, ValueError):
        limit = -1
    if limit < 0:
        raise ActionError("seat_limit must be a whole number, 0 or more.")
    ended = 0
    asked = {}

    def stop(name, player=None, nids=()):
        """What the operation reports."""
        return {"stop": name, "player": player, "negotiations": list(nids), "turn": g.turn, "current": g.s.current}

    while True:
        if g.s.phase != "playing" or not g.majors():
            return stop("game_over")
        pid = g.s.current
        if not g.s.turn_started:
            g.begin_turn()
            continue
        p = g.player(pid)
        if p.kind != "major" or not p.alive:
            g.end_turn(pid)
            continue
        _drive_answers(g, drivers, deferring, answer, asked)
        if g.s.phase != "playing" or g.s.current != pid:
            continue
        if pid not in drivers:
            return stop("external", pid)
        mark = getattr(g, "_drive_mark", None)
        if not (mark and mark[0] == g.turn and mark[1] == pid):
            if limit and ended >= limit:
                return stop("seat_limit")
            g._drive_mark = (g.turn, pid, False)
            continue
        if p.controller == "hybrid" and not mark[2]:
            g._drive_mark = (g.turn, pid, True)
            return stop("hybrid_diplomat", pid)
        nids = [n["id"] for n in g.s.negotiations if n["status"] == "open" and pid in (n["initiator"], n["responder"])
                and n["awaiting"] is not None
                and ((n["awaiting"] != pid and n["awaiting"] not in drivers) or _left_to_host(n, asked))]
        if nids:
            return stop("awaiting_reply", pid, nids)
        g._drive_mark = None
        g.end_turn(pid)
        ended += 1


@op("refresh_visibility", "what every civilization sees is brought up to date")
def _refresh_visibility(g: Game, o: dict):
    """Bring what everyone sees up to date."""
    from . import visibility
    visibility.refresh(g, force=True)
    return {}


@op("add_spy", "player: a new spy in the hideout")
def _add_spy(g: Game, o: dict):
    """Give a major civilization a new spy in its hideout, whether or not espionage is on."""
    from . import espionage
    from .scenario import _pid
    pid = _pid(g, o.get("player"), majors_only=False)
    if g.player(pid).kind != "major":
        raise ActionError("Only a major civilization has spies.")
    return {"spy": espionage.add_spy(g, pid)["name"]}


@op("close_negotiation", "negotiation, status, note; optional by: closes it from outside")
def _close_negotiation(g: Game, o: dict):
    """Close a negotiation from outside it, as a host's timeout does; the negotiation as inspect gives it."""
    from . import diplomacy
    from .inspect import _negotiation, _whole
    from .scenario import _pid
    by = _pid(g, o.get("by"), majors_only=False) if o.get("by") is not None else None
    n = diplomacy.close_negotiation(g, _whole(o.get("negotiation")), str(o.get("status")), str(o.get("note") or ""),
                                    by)
    return _negotiation(n)


@op("open_negotiation_as", "player, to, message; optional give, receive: opens a negotiation out of turn")
def _open_negotiation_as(g: Game, o: dict):
    """Open a negotiation for a player whether or not it is its turn (EngineGame.open_negotiation_as); what the tool
    reports."""
    from . import diplomacy
    from .scenario import _pid
    pid = _pid(g, o.get("player"), majors_only=True)
    try:
        to = int(o.get("to"))
    except (TypeError, ValueError):
        raise ActionError("'to' must be a player id.")
    saved = g.s.current
    g.s.current = pid
    try:
        return diplomacy.open_negotiation(g, pid, to, o.get("message"), o.get("give"), o.get("receive"))
    finally:
        g.s.current = saved


@op("add_barbarian", "unit, x, y (or at); optional hp: a barbarian unit on the tile, whatever the scenario "
                     "operations allow; its id")
def _add_barbarian(g: Game, o: dict):
    """A barbarian unit on a tile, which ``add_unit`` refuses to make; ``unit_id``."""
    from .scenario import _idx, _name
    bid = g.barbarian_id
    if bid is None:
        raise ActionError("The game has no barbarians.")
    u = g.create_unit(bid, _name(g, "unit", o.get("unit")), _idx(g, o))
    if o.get("hp") is not None:
        u.hp = max(1, min(100, _whole(o, "hp")))
    g.invalidate()
    return {"unit_id": u.id}


@op("barbarian_act", "optional unit: the barbarians take a turn now (their units start their turn and act, then "
                     "their camps); with a unit, only that barbarian acts, with the moves it has")
def _barbarian_act(g: Game, o: dict):
    """The barbarians take a turn now, as the start of their turn has them; with a unit, only that barbarian acts
    (``barbarians._automate``, as the tests poked it), with the moves it has."""
    from . import barbarians, units, visibility
    bid = g.barbarian_id
    if bid is None:
        raise ActionError("The game has no barbarians.")
    visibility.refresh(g)
    if o.get("unit") is not None:
        u = _unit(g, o)
        if u.owner != bid:
            raise ActionError("That is not a barbarian unit.")
        try:
            barbarians._automate(g, u)
        except ActionError:
            u.moves = 0
    else:
        for u in list(g.player_units(bid)):
            units.start_turn(g, u)
        barbarians.take_turn(g)
    visibility.refresh(g)
    return {}


@op("clear_camps", "every barbarian camp is removed, with its improvement")
def _clear_camps(g: Game, o: dict):
    """Remove every barbarian camp and its improvement, as a bare game has none; the tiles."""
    from . import barbarians
    removed = []
    for cid, c in sorted(g.s.camps.items()):
        t = g.s.tiles[c["idx"]]
        if t.improvement == barbarians.CAMP:
            t.improvement = None
        removed.append(list(g.grid.xy(c["idx"])))
    g.s.camps.clear()
    g.invalidate()
    return {"removed": removed}


@op("create_camp", "x, y (or at): a barbarian camp on the tile; its id")
def _create_camp(g: Game, o: dict):
    """Put a barbarian camp on a tile; its id."""
    from . import barbarians
    from .scenario import _idx
    return barbarians.create_camp(g, _idx(g, o))


@op("add_quest", "city_state, player, quest (its name); optional scope (individual or global, the row's by default), "
                 "target (a player id; a resource, wonder, great person or natural wonder by name; a contest's starting "
                 "score; the investment percent; for Spread Religion the player whose religion it is), x and y (or "
                 "at: the camp to clear): the city-state gives the major the quest now, whether or not it fits; its "
                 "text")
def _add_quest(g: Game, o: dict):
    """A city-state gives a major a quest now, as its turn would (``city_states._assign``), whether or not the quest
    fits the game; ``quest``, its text."""
    from . import city_states
    from .scenario import _idx, _name, _pid
    cs = _pid(g, o.get("city_state"), majors_only=False)
    if g.player(cs).kind != "city_state":
        raise ActionError(f"Player {cs} is not a city-state.")
    major = _pid(g, o.get("player"), majors_only=True)
    name = o.get("quest")
    row = g.rules.quests.get(name) if isinstance(name, str) else None
    if row is None:
        raise ActionError(f"Unknown quest {name!r}.")
    scope = o.get("scope")
    if scope is None:
        scope = "global" if row.get("type") == "Global" else "individual"
    elif scope not in ("individual", "global"):
        raise ActionError(f"scope must be individual or global, not '{scope}'.")
    t = o.get("target")
    if name == "Clear Barbarian Camp":
        data = _idx(g, o)
    elif name == "Connect Resource":
        data = _name(g, "resource", t)
    elif name == "Construct Wonder":
        data = _name(g, "building", t)
    elif name == "Acquire Great Person":
        data = _name(g, "unit", t)
    elif name == "Find Natural Wonder":
        data = _name(g, "terrain", t)
    elif name in ("Conquer City State", "Bully City State", "Find Player", "Give Gold", "Pledge to Protect",
                  "Denounce Civilization"):
        data = _pid(g, t, majors_only=False)
    elif name == "Spread Religion":
        founder = _pid(g, t, majors_only=False)
        data = g.player(founder).religion
        if not data:
            raise ActionError(f"Player {founder} has founded no religion.")
    elif name in ("Contest Culture", "Contest Faith", "Contest Technologies"):
        data = 0 if t is None else _whole(o, "target")
    elif name == "Invest":
        data = int(row.get("params", [50])[0]) if t is None else _whole(o, "target")
    else:
        data = ""
    city_states._assign(g, cs, name, major, (data, ""), scope)
    return {"quest": city_states.quest_text(g, g.player(cs).quests[-1])}


@op("sack_city", "city: the barbarians sack it; what they took")
def _sack_city(g: Game, o: dict):
    """The barbarians sack a city; what they took, as an attack reports it."""
    from . import barbarians
    c = g.city(_whole(o, "city"))
    if c is None:
        raise ActionError("No such city.")
    return barbarians.sack_city(g, c)


def apply_one(g: Game, n: int, o) -> dict:
    """Run the ``n``-th test operation of a list; the error names it, as apply_ops' does."""
    if not isinstance(o, dict) or o.get("op") not in OPS:
        what = o.get("op") if isinstance(o, dict) else o
        raise ActionError(f"Test operation {n}: unknown op {what!r}. Known: {', '.join(sorted(names()))}.")
    try:
        return OPS[o["op"]][0](g, o) or {}
    except ActionError as e:
        raise ActionError(f"Test operation {n} ({o['op']}): {e}")
