"""Core Game object: owns the state, occupancy index, caches, events, and turn flow.

Game rules live in sibling modules (cities, combat, movement, ...) as functions taking the Game.
"""
from __future__ import annotations

import hashlib
import random
import re
from typing import Callable, Optional

from .hexmap import HexGrid
from .rules import Rules, get_rules
from .state import (GameState, Player, Tile, Unit, City, PLAYER_COLORS, BARBARIAN_COLOR, CITY_STATE_COLORS,
                    seat_overrides)

_POSSESSIVE_S = re.compile(r"(?<=\w)s's\b")
_COORDS = re.compile(r"\(-?\d+,\s*-?\d+\)")
# Event data keys whose value is a player id, and so would identify a civilization the viewer has not met.
_EVENT_PID_KEYS = ("player", "a", "b", "attacker", "defender", "sender", "awaiting", "owner", "killer", "winner",
                   "old_owner", "new_owner")
UNKNOWN_CIV = "Unknown Civilization"
UNKNOWN_CS = "Unknown City-State"


class ActionError(Exception):
    """Raised when a player action is invalid. The message is shown to the player/AI."""


U_DISABLES_RELIGION = "Starting in this era disables religion"

DEFAULT_CONFIG = {
    "map_size": "small",
    "map_type": "continents",
    "width": None,
    "height": None,
    "seed": None,
    "speed": None,                  # UnCiv game speed: Quick | Standard | Epic | Marathon (default Standard)
    "difficulty": None,             # Settler ... Deity (default Prince); seats may override it ("difficulty" per player)
    "barbarian_difficulty": None,   # barbarian strength (None = the game difficulty)
    "ai_base_values": "unciv",      # "monotonic": easier AIs use Prince base values (see economy.difficulty)
    "starting_era": "Ancient era",
    "barbarians": "normal",         # off | normal | raging
    "barbarian_aggression": None,   # 0-100; None -> the barbarian level's default (see game.json barbarians.levels)
    "turn_limit": None,             # None -> the speed's time-victory turn (Standard 500, Quick 330, ...)
    "victories": {"Scientific": True, "Cultural": True, "Domination": True, "Diplomatic": True, "Time": True},
    "city_states": None,            # number of city-states (None -> map size default)
    "religion": True,
    "espionage": True,
    "nuclear_weapons": True,
    "tech_trading": True,
    "ruins": True,
    "map_edges": "ice_caps",        # ice_caps | wrap_x | wrap_y | wrap_both | boxed (see mapgen.EDGE_MODES)
    "river_density": 1.0,           # 0 = no rivers, 1 = normal, 2 = twice as many ...
    "resources": None,              # resource density and per-resource off/cap/share (see mapgen.MapOptions)
    "on_disconnect": "pause",       # an AI model's server stays unreachable: "pause" the game or "skip" its turn
    "reconnect_seconds": 180,       # how long a seat keeps retrying an unreachable server before that applies
    "players": [],                  # [{"name", "color", "leader", "nation", "controller", "handicap", "auto"}]
}



def _hex_rgb(c: str):
    """An '#rrggbb' colour as an (r, g, b) tuple, or None if it isn't one."""
    c = (c or "").strip().lower()
    if not re.fullmatch(r"#[0-9a-f]{6}", c):
        return None
    return tuple(int(c[k:k + 2], 16) for k in (1, 3, 5))


def colors_clash(a: str, b: str) -> bool:
    """Whether two colours are too close to tell apart on the map (a "redmean" weighted RGB distance)."""
    x, y = _hex_rgb(a), _hex_rgb(b)
    if x is None or y is None:
        return False
    rm = (x[0] + y[0]) / 2
    dr, dg, db = x[0] - y[0], x[1] - y[1], x[2] - y[2]
    return ((2 + rm / 256) * dr * dr + 4 * dg * dg + (2 + (255 - rm) / 256) * db * db) ** 0.5 < 60


def unique_colors(wanted: list) -> list[str]:
    """One colour per civilization, first come first served.

    Requested colours are granted in seat order unless an earlier civilization already has it (or one too close to
    it); civilizations with no colour, or a clashing one, then get the first palette colours nobody has taken.
    """
    out: list = []
    for c in wanted:
        c = (c or "").strip().lower()
        ok = _hex_rgb(c) is not None and not any(o and colors_clash(c, o) for o in out)
        out.append(c if ok else None)
    for i, c in enumerate(out):
        if c is None:
            taken = [o for o in out if o]
            out[i] = next((p for p in PLAYER_COLORS if not any(colors_clash(p, o) for o in taken)),
                          PLAYER_COLORS[i % len(PLAYER_COLORS)])
    return out


class Game:
    """A game in progress: the state, plus everything that can be asked of it or done to it.

    ``Game`` owns a :class:`~citar.engine.state.GameState` - the part that is saved - and adds the
    things that are derived from it and must not be: an occupancy index from tile to units, several
    caches, and the event listeners.

    The split matters. Anything in ``self.s`` is serialised and reloaded; anything on the ``Game``
    itself is rebuilt on load. That is why moving a unit has to go through :meth:`create_unit` and
    :meth:`remove_unit` rather than editing the dictionaries directly, and why every yield calculation
    goes through :meth:`invalidate` when something changes.

    There is no I/O here and none anywhere else in the engine. A game is a value: to save it,
    serialise the state; to replay it, keep the values.
    """
    def __init__(self, state: GameState, rules: Optional[Rules] = None):
        self.s = state
        self.rules = rules or get_rules()
        self.grid = HexGrid(state.width, state.height, wrap_x=bool(state.config.get("wrap_x")),
                            wrap_y=bool(state.config.get("wrap_y")))
        self.rng = random.Random()
        if state.rng_state is not None:
            st = state.rng_state
            self.rng.setstate((st[0], tuple(st[1]), st[2]))
        self._occ: dict[int, list[int]] = {}
        self._vis: dict[int, set] = {}
        self._vis_dirty = True
        self._cache: dict = {}       # general caches (cleared on any change)
        self._ycache: dict = {}      # yield/economy caches (cleared when economy-relevant state changes)
        self._static: dict = {}      # terrain-only caches (cleared when terrain changes)
        self._jobcache: dict = {}    # worker automation: best job per tile, keyed by the tile's state
        self._viewcache: dict = {}   # unit id -> (signature, viewable tiles)
        self._hap_busy: set = set()  # civs whose happiness is being computed (conditionals then use _last_hap)
        self._last_hap: dict = {}    # pid -> last fully computed empire happiness
        self._ygen = 0               # bumped whenever _ycache is cleared (cache keys for derived per-unit data)
        self.listeners: list[Callable[[dict], None]] = []
        self._names: Optional[tuple] = None  # (key, regex, lookup) for tagging civ/city names in event text
        self.frames: list[dict] = []
        self.action_log: list[dict] = []
        self._rebuild_occupancy()

    # ------------------------------------------------------------------
    # Construction
    # ------------------------------------------------------------------
    @classmethod
    def new(cls, config: dict, rules: Optional[Rules] = None) -> "Game":
        """Create a game from a lobby configuration: generate the map, place the players, start turn one."""
        from . import mapgen, visibility, barbarians, research, city_states, triggers, units as unitmod
        from . import unique_types as U

        rules = rules or get_rules()
        cfg = dict(DEFAULT_CONFIG)
        cfg.update({k: v for k, v in config.items() if v is not None})
        vic = dict(DEFAULT_CONFIG["victories"])
        vic.update(cfg.get("victories") or {})
        cfg["victories"] = {_victory_name(k): bool(v) for k, v in vic.items()}
        if cfg.get("seed") is None:
            cfg["seed"] = random.randrange(1, 2**31)
        cfg["speed"] = rules.resolve("speed", cfg.get("speed")) or rules.const["default_speed"]
        cfg["difficulty"] = rules.resolve("difficulty", cfg.get("difficulty")) or rules.const["default_difficulty"]
        cfg["barbarian_difficulty"] = rules.resolve("difficulty", cfg.get("barbarian_difficulty")) or cfg["difficulty"]
        cfg["starting_era"] = rules.resolve("era", cfg.get("starting_era")) or "Ancient era"
        if cfg.get("barbarian_aggression") is not None:
            try:
                cfg["barbarian_aggression"] = max(0, min(100, int(float(cfg["barbarian_aggression"]))))
            except (TypeError, ValueError):
                raise ValueError("barbarian_aggression must be a number from 0 to 100.")
        if not cfg.get("turn_limit"):
            cfg["turn_limit"] = rules.max_turns[cfg["speed"]]
        custom = None
        if cfg.get("map"):
            # a map from the map editor: an id in saves/maps or the map itself
            from . import maps
            custom = cfg["map"] if isinstance(cfg["map"], dict) else maps.load_map(str(cfg["map"]))
            cfg["map"] = custom.get("id") or custom.get("name") or "custom"
            cfg["map_type"] = "custom"
            cfg["width"], cfg["height"] = int(custom["width"]), int(custom["height"])
            area = cfg["width"] * cfg["height"]
            cfg["map_size"] = min(rules.const["map_sizes"], key=lambda k: abs(
                rules.const["map_sizes"][k]["width"] * rules.const["map_sizes"][k]["height"] - area))
        size = rules.const["map_sizes"].get(cfg["map_size"], rules.const["map_sizes"]["small"])
        width = cfg.get("width") or size["width"]
        height = cfg.get("height") or size["height"]
        # The grid reads the wrap flags from the saved config, so they are settled here. A generated
        # map takes them from its edges; a map from the editor carries its own.
        if custom is not None:
            cfg["wrap_x"], cfg["wrap_y"] = bool(custom.get("wrap_x")), bool(custom.get("wrap_y")) and height % 2 == 0
            cfg["map_edges"] = None
        else:
            if cfg.get("map_edges") not in mapgen.EDGE_MODES:
                cfg["map_edges"] = mapgen.DEFAULT_EDGES
            cfg["wrap_x"], cfg["wrap_y"] = mapgen.edge_wraps(cfg["map_edges"])
            if cfg["wrap_y"] and height % 2:
                height += 1                 # odd rows are offset: only an even height tiles north-south
        default_players = len(custom.get("starts") or []) or size["players"] if custom else size["players"]
        players_cfg = list(cfg["players"]) or [{} for _ in range(default_players)]
        n = len(players_cfg)
        if not 1 <= n <= rules.const["max_players"]:
            raise ValueError(f"Games support 1 to {rules.const['max_players']} players")
        # checked before the map is generated, so a bad seat setting costs nothing
        overrides = [seat_overrides(pc.get("handicap"), pc.get("auto")) for pc in players_cfg]
        n_cs = cfg.get("city_states")
        if n_cs is None:
            n_cs = len(custom.get("cs_starts") or []) if custom else size.get("city_states", 0)
        cfg["city_states"] = int(n_cs)
        cfg["players"] = players_cfg
        cfg["width"], cfg["height"] = width, height

        rng = random.Random(cfg["seed"])
        # nations
        used = set()
        chosen = []
        for pc in players_cfg:
            nat = rules.resolve("nation", pc.get("nation")) if pc.get("nation") not in (None, "", "random", "Random") else None
            if nat and rules.nations[nat].get("kind") != "major":
                nat = None
            chosen.append(nat)
            if nat and nat != "BenchmarkCiv":
                used.add(nat)
        pool = [x for x in rules.major_nations if x not in used and x != "BenchmarkCiv"]
        rng.shuffle(pool)
        for i, nat in enumerate(chosen):
            if nat is None:
                chosen[i] = pool.pop() if pool else "BenchmarkCiv"
        cs_pool = list(rules.city_state_nations)
        rng.shuffle(cs_pool)
        cs_nations = cs_pool[:n_cs]

        if custom is not None:
            from . import maps
            tiles, starts, cs_starts, continents = maps.prepare(rules, custom, n, len(cs_nations), rng, chosen,
                                                                ruins=cfg["ruins"])
        else:
            tiles, starts, cs_starts, continents = mapgen.generate_map(
                rules, width, height, cfg["map_type"], n, len(cs_nations), rng, ruins=cfg["ruins"],
                nations=chosen, options=cfg)

        players = []
        colors = unique_colors([pc.get("color") for pc in players_cfg])
        for i, pc in enumerate(players_cfg):
            nd = rules.nations[chosen[i]]
            players.append(Player(
                id=i, name=pc.get("name") or (nd["name"] if chosen[i] != "BenchmarkCiv" else f"Civilization {i + 1}"),
                leader=pc.get("leader") or nd.get("leaderName", ""), nation=chosen[i],
                color=colors[i],
                controller=pc.get("controller") or "human", explored=bytearray(width * height),
                overrides=overrides[i],
                difficulty=rules.resolve("difficulty", pc.get("difficulty")) or cfg["difficulty"]))
        for j, csn in enumerate(cs_nations[:len(cs_starts)]):
            nd = rules.nations[csn]
            players.append(Player(
                id=len(players), name=csn, nation=csn, kind="city_state", controller="minor",
                color=CITY_STATE_COLORS.get(nd.get("cityStateType"), "#bbbbbb"), cs_type=nd.get("cityStateType"),
                explored=bytearray(width * height)))
        if cfg["barbarians"] and rules.const["barbarians"]["levels"].get(cfg["barbarians"]):
            players.append(Player(id=len(players), name="Barbarians", nation="Barbarians", color=BARBARIAN_COLOR,
                                  kind="barbarian", controller="barbarian", explored=bytearray(width * height)))
        state = GameState(config=cfg, width=width, height=height, tiles=tiles, players=players, continents=continents)
        g = cls(state, rules)
        g.rng = rng
        era = rules.eras[cfg["starting_era"]]
        # techs & starting stats
        for p in g.s.players:
            if p.kind == "barbarian":
                continue
            for t, td in rules.techs.items():
                if td["_umap"].has_tag(U.StartingTech) or td["_era"] < era["number"]:
                    research.add_tech_silently(g, p.id, t)
            if p.kind == "major" and not g.is_humanlike(p.id):
                for t in g.seat_difficulty(p.id).get("aiFreeTechs", []):
                    research.add_tech_silently(g, p.id, t)
            for u in g.civ_uniques(p.id, U.StartsWithTech):
                research.add_tech_silently(g, p.id, u.p(0))
            p.gold += int(era.get("startingGold", 0) * g.speed["goldCostModifier"])
            p.culture += int(era.get("startingCulture", 0) * g.speed["cultureCostModifier"])
        # city-states
        for p in g.s.players:
            if p.kind == "city_state":
                city_states.init_city_state(g, p.id, used_majors=set(chosen))
        # starting units
        cs_i = 0
        for p in g.s.players:
            if p.kind == "barbarian":
                continue
            if p.kind == "major":
                start = starts[p.id]
            else:
                start = cs_starts[cs_i]
                cs_i += 1
            units = unitmod.starting_units(g, p.id, cfg["starting_era"])
            for utype in units:
                spot = g.find_spawn_tile(start, utype, p.id)
                if spot is not None:
                    g.create_unit(p.id, utype, spot)
            p.flags["start"] = start
            if p.kind == "major":
                for u in list(rules.global_uniques.all) + list(rules.nations[p.nation]["_umap"].all):
                    from .uniques import applies, Ctx
                    if triggers.has_trigger_conditional(u) or not applies(u, Ctx(g, civ=p.id)):
                        continue
                    triggers.trigger(g, u, p.id, tile=start)
        # relations
        for p in g.s.players:
            if p.kind == "barbarian":
                continue
            for q in g.s.players:
                if q.kind != "barbarian" and q.id > p.id:
                    from .diplomacy import new_relation
                    g.s.relations[g.rel_key(p.id, q.id)] = new_relation()
        if g.barbarian_id is not None:
            barbarians.place_initial_camps(g)
        visibility.refresh(g)
        g.begin_turn()
        g.emit("game_start", f"The world begins. {n} civilizations and {len(cs_nations)} city-states stir.", None)
        return g

    # ------------------------------------------------------------------
    # Accessors
    # ------------------------------------------------------------------
    @property
    def turn(self) -> int:
        """The current turn number."""
        return self.s.turn

    @property
    def current_player(self) -> int:
        """Whose turn it is, as a player id."""
        return self.s.current

    @property
    def barbarian_id(self) -> Optional[int]:
        """The barbarian player's id, or None in a game without them."""
        for p in self.s.players:
            if p.kind == "barbarian":
                return p.id
        return None

    @property
    def speed(self) -> dict:
        """The game speed's definition, which scales nearly every cost in the game."""
        return self.rules.speed(self.s.config.get("speed"))

    @property
    def religion_enabled(self) -> bool:
        """Whether religion is in play, which some starting eras disable."""
        era = self.rules.eras.get(self.s.config.get("starting_era") or "Ancient era")
        if era and era["_umap"].get(U_DISABLES_RELIGION):
            return False
        return bool(self.s.config.get("religion", True))

    @property
    def espionage_enabled(self) -> bool:
        """Whether espionage is in play."""
        return bool(self.s.config.get("espionage", True))

    @property
    def nukes_enabled(self) -> bool:
        """Whether nuclear weapons may be built."""
        return bool(self.s.config.get("nuclear_weapons", True))

    def victory_enabled(self, name: str) -> bool:
        """Whether a victory type is enabled for this game."""
        return bool(self.s.config.get("victories", {}).get(_victory_name(name), True))

    def total_turns(self) -> int:
        """The turn the game ends on: the configured limit, or the speed's own."""
        lim = self.s.config.get("turn_limit")
        return int(lim) if lim else self.rules.max_turns.get(self.s.config.get("speed"), 500)

    def year(self, turn: Optional[int] = None) -> float:
        """Calendar year of a turn (GameInfo.getYear with the speed's year table; negative = BC). CITAR's turn 1 is
        UnCiv's turn 0."""
        turn = (self.turn if turn is None else turn) - 1
        sp = self.speed
        year, t = float(sp["startYear"]), 0
        for step in sp["turns"]:
            n = min(turn, step["untilTurn"]) - t
            if n <= 0:
                break
            year += n * step["yearsPerTurn"]
            t += n
        if turn > t:
            year += (turn - t) * sp["turns"][-1]["yearsPerTurn"]
        return year

    def year_text(self, turn: Optional[int] = None) -> str:
        """The in-game year as text, with BC and AD."""
        y = int(self.year(turn))
        return f"{-y} BC" if y < 0 else f"{y} AD"

    def game_difficulty(self) -> dict:
        """The game's difficulty definition."""
        return self.rules.difficulty(self.s.config.get("difficulty"))

    def seat_difficulty(self, pid: Optional[int] = None) -> dict:
        """The difficulty applying to one seat, which can differ per player."""
        from .economy import seat_difficulty
        return seat_difficulty(self, pid)

    def difficulty_name(self, pid: Optional[int] = None) -> str:
        """The name of a seat's difficulty."""
        from .economy import difficulty
        return difficulty(self, pid)["name"]

    def difficulty_index(self, pid: Optional[int] = None) -> int:
        """A seat's difficulty as an index, for comparisons."""
        return self.rules.difficulty_index(self.difficulty_name(pid))

    def is_humanlike(self, pid: int) -> bool:
        """Whether this seat gets a human's difficulty numbers (``Player.handicap == "human"``).

        The distinction that decides which difficulty bonuses apply. A language model playing a seat is
        "humanlike" by default: it gets a human's numbers, because giving it the AI's bonuses would make a
        benchmark against the scripted bot meaningless. It follows the seat's handicap rather than who plays
        it, so a hybrid seat can be given either.
        """
        from .economy import is_humanlike
        return is_humanlike(self, pid)

    def tile(self, idx: int) -> Tile:
        """The tile at an index."""
        return self.s.tiles[idx]

    def player(self, pid: int) -> Player:
        """The player with this id."""
        return self.s.players[pid]

    def majors(self, alive_only: bool = True) -> list[Player]:
        """Every major civilization, alive by default."""
        return [p for p in self.s.players if p.kind == "major" and (p.alive or not alive_only)]

    def city_states(self, alive_only: bool = True) -> list[Player]:
        """Every city-state, alive by default."""
        return [p for p in self.s.players if p.kind == "city_state" and (p.alive or not alive_only)]

    def is_barbarian(self, pid: Optional[int]) -> bool:
        """Whether this player is the barbarians."""
        return pid is not None and self.s.players[pid].kind == "barbarian"

    def is_city_state(self, pid: Optional[int]) -> bool:
        """Whether this player is a city-state."""
        return pid is not None and self.s.players[pid].kind == "city_state"

    def unit(self, uid: int) -> Optional[Unit]:
        """A unit by id, or None if it no longer exists.

        Returning None rather than raising matters: units are destroyed constantly, and every caller that
        holds a unit reference across a turn has to cope with it having died.
        """
        return self.s.units.get(uid)

    def city(self, cid) -> Optional[City]:
        """A city by id, or None."""
        return self.s.cities.get(cid) if cid is not None else None

    def units_at(self, idx: int) -> list[Unit]:
        """Every unit on a tile, from the occupancy index rather than by scanning."""
        return [self.s.units[u] for u in self._occ.get(idx, ())]

    def military_at(self, idx: int) -> Optional[Unit]:
        """The military land or sea unit on a tile, which is what defends it."""
        for u in self.units_at(idx):
            ud = self.rules.units[u.type]
            if ud["_military"] and ud["_domain"] != "Air":
                return u
        return None

    def civilian_at(self, idx: int) -> Optional[Unit]:
        """The civilian unit on a tile, which is what gets captured."""
        for u in self.units_at(idx):
            ud = self.rules.units[u.type]
            if not ud["_military"] and ud["_domain"] != "Air":
                return u
        return None

    def air_units_at(self, idx: int) -> list[Unit]:
        """Aircraft based on a tile, which stack separately from everything else."""
        return [u for u in self.units_at(idx) if self.rules.units[u.type]["_domain"] == "Air"]

    def city_at(self, idx: int) -> Optional[City]:
        """The city on a tile, or None."""
        t = self.s.tiles[idx]
        if t.city is not None:
            c = self.s.cities.get(t.city)
            if c and c.idx == idx:
                return c
        return None

    def player_units(self, pid: int) -> list[Unit]:
        """Every unit this player owns."""
        return [u for u in self.s.units.values() if u.owner == pid]

    def player_cities(self, pid: int) -> list[City]:
        """Every city this player owns."""
        return [c for c in self.s.cities.values() if c.owner == pid]

    def udef(self, unit: Unit) -> dict:
        """The ruleset definition of a unit's type."""
        return self.rules.units[unit.type]

    def is_water(self, idx: int) -> bool:
        """Whether a tile is water."""
        return self.rules.terrains[self.s.tiles[idx].terrain]["type"] == "Water"

    def is_land(self, idx: int) -> bool:
        """Whether a tile is land."""
        return not self.is_water(idx)

    def is_coastal(self, idx: int) -> bool:
        """Whether a tile touches the coast."""
        from .tiles import adjacent_to_coast
        return adjacent_to_coast(self, idx)

    def continent(self, idx: int) -> int:
        """Which landmass a tile belongs to, as an id."""
        c = self.s.continents
        return c[idx] if idx < len(c) else -1

    def has_tech(self, pid: int, tech: Optional[str]) -> bool:
        """Whether a player has researched a technology. A missing name counts as satisfied.

        The ``None`` case is not an oversight: ruleset entries use a missing ``requiredTech`` to mean "no
        requirement", and answering True keeps every caller from having to special-case it.
        """
        if tech is None:
            return True
        return tech in self._tech_set(pid)

    def _tech_set(self, pid: int) -> set:
        """The player's technologies as a set, cached and keyed by how many they have.

        Using the count as part of the key is a cheap way to invalidate: researching anything changes the
        length, so the cache cannot go stale without the key changing too.
        """
        key = ("techs", pid, len(self.s.players[pid].techs))
        cached = self._static.get(key)
        if cached is None:
            cached = set(self.s.players[pid].techs)
            self._static[key] = cached
        return cached

    def xy(self, idx: int) -> dict:
        """A tile index as ``{"x": ..., "y": ...}``, the form the API uses."""
        x, y = self.grid.xy(idx)
        return {"x": x, "y": y}

    def fmt_xy(self, idx: int) -> str:
        """A tile index as ``(x,y)``, for messages."""
        x, y = self.grid.xy(idx)
        return f"({x},{y})"

    def new_id(self) -> int:
        """The next unique id for a unit or city."""
        i = self.s.next_id
        self.s.next_id += 1
        return i

    def state_rng(self, *keys) -> random.Random:
        """Deterministic RNG derived from the game seed and the given keys (UnCiv stateBasedRandom)."""
        key = "|".join([str(self.s.config.get("seed"))] + [str(k) for k in keys])
        return random.Random(int.from_bytes(hashlib.blake2b(key.encode(), digest_size=8).digest(), "big"))

    # ------------------------------------------------------------------
    # Caches
    # ------------------------------------------------------------------
    def invalidate(self, yields: bool = True):
        """Call after anything that changes visibility or (with yields=True) the economy."""
        self._vis_dirty = True
        self._cache.clear()
        if yields:
            self._ycache.clear(); self._ygen += 1

    # yield-cache entries that don't depend on which tiles citizens work or on city focus
    _CITIZEN_SAFE_KEYS = frozenset({"civ_umaps", "civ_umaps_nores", "civ_index", "local_umaps", "ctiles", "owned",
                                    "resource_umap", "res_supply", "city_res", "civ_res", "tech_cost", "era", "techs",
                                    "connected", "coastal", "fresh", "follower_umap", "founder_umap", "transport",
                                    "supply_deficit", "tstats", "uumap", "mprof", "los", "vis_heights"})

    def invalidate_city(self, city: Optional[City] = None, citizens_only: bool = False):
        """Throw away cached yields, optionally keeping the parts citizen assignment does not affect.

        The ``citizens_only`` path exists because assigning citizens recomputes stats, which assigns
        citizens. Keeping the entries that cannot have changed breaks the loop without giving up the whole
        cache on every assignment.
        """
        if citizens_only:
            keep = {k: v for k, v in self._ycache.items() if type(k) is tuple and k[0] in self._CITIZEN_SAFE_KEYS}
            self._ycache.clear()
            self._ycache.update(keep)
        else:
            self._ycache.clear(); self._ygen += 1
        self._cache.clear()

    def clear_static(self):
        """Throw away the caches that survive a normal invalidation: line of sight, terrain, view data."""
        self._static.clear()          # also holds line-of-sight and terrain-height caches
        self._viewcache.clear()
        self.invalidate()

    def resources_on_map(self) -> set:
        """Every resource type present anywhere on the map, cached."""
        v = self._static.get("resources_on_map")
        if v is None:
            v = {t.resource for t in self.s.tiles if t.resource}
            self._static["resources_on_map"] = v
        return v

    # ------------------------------------------------------------------
    # Uniques & civ stats
    # ------------------------------------------------------------------
    def civ_umaps(self, pid: int):
        """The unique maps applying to a whole civilization."""
        from .economy import civ_umaps
        return civ_umaps(self, pid)

    def civ_uniques(self, pid: Optional[int], ph: str, ctx=None):
        """Civilization-wide uniques matching a placeholder."""
        from .economy import civ_uniques
        return civ_uniques(self, pid, ph, ctx)

    def civ_has(self, pid: Optional[int], ph: str, ctx=None) -> bool:
        """Whether a civilization-wide unique with this placeholder applies."""
        from .economy import civ_has
        return civ_has(self, pid, ph, ctx)

    def resource_amount(self, pid: int, res: str) -> int:
        """How much of a resource this player has available."""
        from .economy import resource_amount
        return resource_amount(self, pid, res)

    def stat_reserve(self, pid: int, stat: str) -> float:
        """A player's stockpile of a stat. Science has none - it is spent as it is produced."""
        p = self.player(pid)
        return {"gold": p.gold, "culture": p.culture, "faith": p.faith, "science": 0.0,
                "happiness": p.golden_age_points}.get(stat, 0.0)

    def add_stat(self, pid: int, stat: str, amount: float):
        """Add to a player's stockpile of a stat."""
        p = self.player(pid)
        if stat == "gold":
            p.gold += amount
        elif stat == "culture":
            p.culture += amount
        elif stat == "faith":
            p.faith += amount
        elif stat == "science":
            from .research import add_science
            add_science(self, pid, amount)
        elif stat == "happiness":
            p.golden_age_points += amount
        self._ycache.clear(); self._ygen += 1

    # ------------------------------------------------------------------
    # Diplomacy basics
    # ------------------------------------------------------------------
    @staticmethod
    def rel_key(a: int, b: int) -> str:
        """The canonical key for a pair of players, ordered so that (a,b) and (b,a) are the same relation."""
        return f"{min(a, b)},{max(a, b)}"

    def relation(self, a: int, b: int) -> Optional[dict]:
        """The relationship between two players, or None if they have never interacted."""
        return self.s.relations.get(self.rel_key(a, b))

    def at_war(self, a: Optional[int], b: Optional[int]) -> bool:
        """Whether two players are at war. Barbarians are at war with everybody, always."""
        if a is None or b is None or a == b:
            return False
        if self.is_barbarian(a) or self.is_barbarian(b):
            return True
        rel = self.s.relations.get(self.rel_key(a, b))
        return bool(rel and rel["war"])

    def is_at_war_any(self, pid: int) -> bool:
        """Whether this player is at war with anybody other than barbarians."""
        return any(self.at_war(pid, q.id) for q in self.s.players if q.id != pid and q.alive and q.kind != "barbarian")

    def is_friend(self, a: int, b: int) -> bool:
        """Friendly relationship: declared friendship, or city-state friend/ally level."""
        if self.is_city_state(a):
            from .city_states import is_friend_level
            return is_friend_level(self, a, b)
        if self.is_city_state(b):
            from .city_states import is_friend_level
            return is_friend_level(self, b, a)
        rel = self.relation(a, b)
        return bool(rel and rel.get("friendship_until", 0) >= self.s.turn)

    def has_met(self, a: int, b: int) -> bool:
        """Whether two players know of each other."""
        if a == b:
            return True
        return b in self.s.players[a].met

    def meet(self, a: int, b: int):
        """Record that two players have met, with everything that follows from a first contact."""
        if a == b or self.is_barbarian(a) or self.is_barbarian(b) or self.has_met(a, b):
            return
        pa, pb = self.s.players[a], self.s.players[b]
        pa.met.append(b)
        pb.met.append(a)
        from . import city_states
        city_states.on_meet(self, a, b)
        self.emit("first_contact", f"{pa.name} and {pb.name} have made contact.", [a, b], a=a, b=b)

    def has_open_borders(self, owner: int, visitor: int) -> bool:
        """Whether one player may currently pass through another's territory."""
        until = self.s.open_borders.get(f"{owner}>{visitor}")
        return until is not None and until >= self.s.turn

    def can_enter_territory(self, pid: int, idx: int) -> bool:
        """Whether this player's units may be on this tile at all."""
        owner = self.s.tiles[idx].owner
        if self.is_barbarian(pid) and owner is not None and not self.is_barbarian(owner):
            from .economy import barbarian_difficulty
            return self.s.turn - 1 >= barbarian_difficulty(self).get("turnBarbariansCanEnterPlayerTiles", 0)
        if owner is None or owner == pid or self.is_barbarian(pid) or self.is_barbarian(owner):
            return True
        if self.at_war(pid, owner):
            return True
        if self.is_city_state(owner) or self.is_city_state(pid):
            return True
        return self.has_open_borders(owner, pid)

    # ------------------------------------------------------------------
    # Units
    # ------------------------------------------------------------------
    def _rebuild_occupancy(self):
        """Rebuild the tile-to-units index from scratch.

        Called on load, because the index is derived state that is not saved - saving it would be a second
        representation of the same fact, and the two would eventually disagree.
        """
        self._occ = {}
        for u in self.s.units.values():
            self._occ.setdefault(u.idx, []).append(u.id)

    def create_unit(self, pid: int, utype: str, idx: int, xp: int = 0) -> Unit:
        """Create a unit, place it, and register it everywhere it needs to be."""
        from . import movement, units as unitmod
        u = Unit(id=self.new_id(), type=utype, owner=pid, idx=idx, xp=xp, created_turn=self.s.turn)
        ud = self.rules.units[utype]
        if ud.get("religiousStrength"):
            u.religious_strength = ud["religiousStrength"]
        self.s.units[u.id] = u
        self._occ.setdefault(idx, []).append(u.id)
        unitmod.on_created(self, u)
        u.moves = 0 if self.s.current != pid else movement.max_moves(self, u)
        self.invalidate()
        return u

    def remove_unit(self, unit: Unit):
        """Remove a unit from the game and from every index that refers to it."""
        if unit.id not in self.s.units:
            return
        del self.s.units[unit.id]
        lst = self._occ.get(unit.idx)
        if lst and unit.id in lst:
            lst.remove(unit.id)
            if not lst:
                del self._occ[unit.idx]
        for other in self.s.units.values():
            if other.carried_by == unit.id:
                other.carried_by = None
        self.invalidate()

    def place_unit(self, unit: Unit, idx: int):
        """Position update without movement rules."""
        lst = self._occ.get(unit.idx)
        if lst and unit.id in lst:
            lst.remove(unit.id)
            if not lst:
                del self._occ[unit.idx]
        unit.idx = idx
        self._occ.setdefault(idx, []).append(unit.id)
        for other in self.s.units.values():
            if other.carried_by == unit.id and other.idx != idx:
                old = self._occ.get(other.idx)
                if old and other.id in old:
                    old.remove(other.id)
                    if not old:
                        del self._occ[other.idx]
                other.idx = idx
                self._occ.setdefault(idx, []).append(other.id)
        self.invalidate(yields=False)

    def find_spawn_tile(self, near: int, utype: str, pid: int, max_radius: int = 3) -> Optional[int]:
        """A tile near *near* where this unit type could legitimately appear, or None."""
        from . import movement
        ud = self.rules.units[utype]
        for idx in self.grid.within(near, max_radius):
            if movement.can_stand(self, pid, ud, idx):
                return idx
        return None

    def change_owner(self, unit: Unit, new_owner: int):
        """Transfer a unit to another player, clearing the orders that belonged to the old one."""
        unit.owner = new_owner
        unit.activity = None
        unit.build = None
        unit.goto = None
        unit.fortify = 0
        unit.moves = 0
        self.invalidate()

    # ------------------------------------------------------------------
    # Events
    # ------------------------------------------------------------------
    PRIVATE_EVENTS = frozenset({"unit_built", "building_built", "city_growth", "city_starving", "city_idle",
                                "city_auto_production", "production_invalid", "production_blocked", "promotion_ready",
                                "orders_interrupted", "unit_woke", "explore_done", "build_cancelled", "city_razing",
                                "borders", "wltkd", "wltkd_end", "city_demand", "resistance_end", "wonder_refund",
                                "policy_available", "great_person_born", "faith", "spy"})

    def _name_index(self):
        """A regex matching every civilization, city-state, leader and city name, and what each one refers to.

        Rebuilt only when some name changes (a city founded, captured or renamed, a civ renamed).
        """
        key = (tuple((p.name, p.leader, p.kind) for p in self.s.players),
               tuple((c.name, c.owner) for c in self.s.cities.values()))
        cached = getattr(self, "_names", None)
        if cached is not None and cached[0] == key:
            return cached[1], cached[2]
        lookup: dict = {}
        for c in self.s.cities.values():  # lowest priority: a city named like a civ is read as the civ
            if c.name and not self.is_barbarian(c.owner):
                lookup[c.name] = (c.owner, "t")
        for p in self.s.players:
            if p.kind == "barbarian":
                continue
            if p.leader and len(p.leader) >= 3:
                lookup[p.leader] = (p.id, "l")
            if p.name:
                lookup[p.name] = (p.id, "c")
        rx = None
        if lookup:
            alts = "|".join(re.escape(n) for n in sorted(lookup, key=len, reverse=True))
            rx = re.compile(rf"(?<!\w)(?:{alts})(?!\w)")
        self._names = (key, rx, lookup)
        return rx, lookup

    def _event_refs(self, text: str, mentions: Optional[dict]) -> list:
        """Where the text names a civilization, leader or city: [start, end, player id, kind] spans."""
        rx, lookup = self._name_index()
        refs = []
        if rx is not None:
            for m in rx.finditer(text):
                pid, kind = lookup[m.group(0)]
                refs.append([m.start(), m.end(), pid, kind])
        for name, who in (mentions or {}).items():  # names the index cannot know (a civ's old name, a lost city)
            pid, kind = who if isinstance(who, tuple) else (who, "c")
            if not name:
                continue
            for m in re.finditer(rf"(?<!\w){re.escape(name)}(?!\w)", text):
                if not any(r[0] < m.end() and m.start() < r[1] for r in refs):
                    refs.append([m.start(), m.end(), pid, kind])
        refs.sort()
        return refs

    def emit(self, etype: str, text: str, players: Optional[list] = None, idx: Optional[int] = None,
             mentions: Optional[dict] = None, **data) -> dict:
        """players=None means public. Tile-anchored events add anyone who can see the tile (except private events).

        Civilization, city-state, leader and city names in the text are recorded as ``refs`` so that each
        viewer can be shown "Unknown Civilization" for civs they have not met (see event_view).
        *mentions* maps extra names the game no longer knows (a civ's old name, a destroyed city) to the player
        they denote, or to (player, "t") for a city.
        """
        from . import visibility
        text = _POSSESSIVE_S.sub("s'", text)
        audience = None if players is None else sorted(set(players))
        if audience is not None and idx is not None and etype not in self.PRIVATE_EVENTS:
            for p in self.s.players:
                if p.kind == "major" and p.id not in audience and idx in visibility.visible_tiles(self, p.id):
                    audience.append(p.id)
        ev = {"id": len(self.s.events) + 1, "turn": self.s.turn, "type": etype, "text": text,
              "players": audience, "idx": idx, "data": data}
        refs = self._event_refs(text, mentions)
        if refs:
            ev["refs"] = refs
        if idx is not None:
            ev["x"], ev["y"] = self.grid.xy(idx)
        self.s.events.append(ev)
        for fn in list(self.listeners):
            try:
                fn(ev)
            except Exception:
                pass
        return ev

    def events_for(self, pid: Optional[int], since_id: int = 0, limit: int = 200) -> list[dict]:
        """Notifications visible to a player, oldest first, since an event id.

        A player sees civilizations and city-states they have not met as "Unknown Civilization" /
        "Unknown City-State" (see event_view); pid None (spectators, replays) sees everything as it happened.
        """
        out = []
        known = self._known_to(pid)
        for ev in reversed(self.s.events):
            if ev["id"] <= since_id:
                break
            if pid is None or ev["players"] is None or pid in ev["players"]:
                out.append(ev if known is None else self._scrub_event(ev, known))
                if len(out) >= limit:
                    break
        out.reverse()
        return out

    def _known_to(self, pid: Optional[int]) -> Optional[set]:
        """The players whose identity this viewer knows (itself, whoever it has met, the barbarians); None = all."""
        if pid is None or not (0 <= pid < len(self.s.players)):
            return None
        known = set(self.s.players[pid].met)
        known.add(pid)
        known.update(p.id for p in self.s.players if p.kind == "barbarian")
        return known

    def event_view(self, ev: dict, pid: Optional[int]) -> dict:
        """One event as this player may see it: unmet civilizations anonymised, their locations dropped."""
        known = self._known_to(pid)
        return ev if known is None else self._scrub_event(ev, known)

    def _scrub_event(self, ev: dict, known: set) -> dict:
        """A copy of the event with every player outside *known* made anonymous (or the event itself if none)."""
        refs = ev.get("refs")
        data = ev.get("data") or {}
        hidden = {r[2] for r in refs if r[2] not in known} if refs else set()
        n = len(self.s.players)
        for k in _EVENT_PID_KEYS:
            v = data.get(k)
            if type(v) is int and v not in known and 0 <= v < n:
                hidden.add(v)
        res = data.get("results")
        if isinstance(res, dict) and (res.get("tally") or res.get("winner") is not None):
            names = {p.name: p.id for p in self.s.players}
            if res.get("winner") not in (None, *known) or any(names.get(nm, -1) not in known and nm in names
                                                              for nm in res.get("tally") or {}):
                hidden.add(-1)
        if not hidden:
            return ev
        out = dict(ev)
        text = ev["text"]
        if refs:
            parts, pos = [], 0
            for start, end, rp, kind in refs:
                if rp not in hidden or start < pos:
                    continue
                if kind == "t":
                    rep = "an unknown city"
                elif kind == "l":
                    rep = "an unknown leader"
                else:
                    rep = UNKNOWN_CS if self.is_city_state(rp) else UNKNOWN_CIV
                before = text[:start].rstrip()
                if rep[0].islower() and (not before or before[-1] in ".!?\""):
                    rep = rep[0].upper() + rep[1:]
                # "Aztecs' Warrior" -> "Unknown Civilization's Warrior"
                if (text[end:end + 1] == "'" and not text[end + 1:end + 2].isalnum() and text[start:end].endswith("s")
                        and not rep.endswith("s")):
                    rep += "'s"
                    end += 1
                parts.append(text[pos:start])
                parts.append(rep)
                pos = end
            parts.append(text[pos:])
            text = "".join(parts)
        text = _COORDS.sub("an unknown location", text)
        out["text"] = text
        out.pop("refs", None)
        if ev.get("idx") is not None:
            out["idx"] = None
            out.pop("x", None)
            out.pop("y", None)
        new = dict(data)
        for k in _EVENT_PID_KEYS:
            if new.get(k) in hidden and type(new.get(k)) is int:
                new[k] = None
        if isinstance(res, dict):
            names = {p.name: p.id for p in self.s.players}
            tally, i = {}, 0
            for nm, v in (res.get("tally") or {}).items():
                if nm in names and names[nm] not in known:
                    i += 1
                    nm = UNKNOWN_CS if self.is_city_state(names[nm]) else UNKNOWN_CIV
                    nm = nm if nm not in tally else f"{nm} ({i})"
                tally[nm] = v
            new["results"] = {**res, "tally": tally,
                              "winner": res.get("winner") if res.get("winner") in known else None}
        out["data"] = new
        return out

    # ------------------------------------------------------------------
    # Turn flow
    # ------------------------------------------------------------------
    def save_rng(self):
        """Persist the random generator's state into the saved game.

        Without this, loading a save and replaying would diverge from the original at the first dice roll,
        and a benchmark that resumed after a restart would not be the same experiment.
        """
        st = self.rng.getstate()
        self.s.rng_state = [st[0], list(st[1]), st[2]]

    def begin_turn(self):
        """Start the current player's turn, if it has not already begun."""
        from . import turns
        if self.s.turn_started or self.s.phase != "playing":
            return
        self.s.turn_started = True
        turns.start_player_turn(self, self.s.current)

    def end_turn(self, pid: int):
        """End a player's turn and advance to the next."""
        from . import turns
        if self.s.phase != "playing":
            raise ActionError("The game is over.")
        if pid != self.s.current:
            raise ActionError("It is not your turn.")
        turns.end_player_turn(self, pid)
        while self.s.phase == "playing":
            nxt = self.s.current + 1
            if nxt >= len(self.s.players):
                turns.end_round(self)
                if self.s.phase != "playing":
                    break
                nxt = 0
            self.s.current = nxt
            self.s.turn_started = False
            p = self.s.players[nxt]
            if not p.alive:
                continue
            self.begin_turn()
            if p.kind in ("barbarian", "city_state"):
                turns.end_player_turn(self, nxt)
                continue
            break
        self.save_rng()


def _victory_name(k: str) -> str:
    """Normalise the several names each victory type is known by."""
    return {"science": "Scientific", "scientific": "Scientific", "culture": "Cultural", "cultural": "Cultural",
            "domination": "Domination", "diplomatic": "Diplomatic", "diplomacy": "Diplomatic", "score": "Time",
            "time": "Time"}.get(str(k).lower(), k)
