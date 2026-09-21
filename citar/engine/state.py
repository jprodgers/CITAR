"""Serializable game state. Everything the engine needs lives here; nothing else is persisted.

Ruleset objects are referred to by display name ("Warrior", "Bronze Working", "Grassland"), like UnCiv.
"""
from __future__ import annotations

import base64
from dataclasses import dataclass, field, asdict, fields
from typing import Any, Optional

PLAYER_COLORS = ["#e04040", "#3c78d8", "#e8c547", "#8e44ad", "#27ae60", "#e67e22", "#17becf", "#f06292",
                 "#8d6e63", "#9ccc65", "#5c6bc0", "#ff7043", "#26a69a", "#d4e157", "#ab47bc", "#78909c",
                 "#b71c1c", "#0d47a1", "#f9a825", "#1b5e20", "#ff80ab", "#00e5ff", "#6d4c41", "#c0ca33"]
CITY_STATE_COLORS = {"Cultured": "#a58cff", "Maritime": "#4fd68a", "Mercantile": "#f2d43d", "Militaristic": "#e85f5f",
                     "Religious": "#f5f5f5"}
BARBARIAN_COLOR = "#2b2b2b"


@dataclass
class Tile:
    """One map tile. Stored as a list rather than a dictionary, because there are up to 16,000 of them."""
    terrain: str                                    # base terrain: Grassland, Coast, Mountain, ...
    features: list = field(default_factory=list)    # terrain features: Hill, Forest, Jungle, Marsh, Oasis, ...
    wonder: Optional[str] = None                    # natural wonder
    river: int = 0                                  # 6-bit mask: river on edge d (see hexmap directions)
    resource: Optional[str] = None
    resource_amount: int = 0                        # strategic deposits
    improvement: Optional[str] = None               # incl. "Ancient ruins", "Barbarian encampment", great improvements
    pillaged: bool = False
    route: Optional[str] = None                     # "Road" | "Railroad"
    route_pillaged: bool = False
    owner: Optional[int] = None
    city: Optional[int] = None                      # id of the city that owns (can work) this tile
    fallout: bool = False
    build: Optional[list] = None                    # improvement queue: [[improvement, turns left], ...]

    _ORDER = ("terrain", "features", "wonder", "river", "resource", "resource_amount", "improvement", "pillaged",
              "route", "route_pillaged", "owner", "city", "fallout", "build")

    def to_list(self) -> list:
        """The compact form written to a save."""
        return [getattr(self, k) for k in self._ORDER]

    @classmethod
    def from_list(cls, data: list) -> "Tile":
        """Read a tile back from its compact form."""
        return cls(**dict(zip(cls._ORDER, data)))

    @property
    def hills(self) -> bool:
        """Whether this tile is hills, which several rules ask constantly."""
        return "Hill" in self.features

    @property
    def feature(self) -> Optional[str]:
        """The (last) non-hill feature, e.g. Forest on a forested hill."""
        for f in reversed(self.features):
            if f != "Hill":
                return f
        return None


@dataclass
class Unit:
    """One unit: its type, owner, position, health, experience, promotions and orders."""
    id: int
    type: str
    owner: int
    idx: int
    hp: int = 100
    moves: int = 0                      # in move_scale units (60 = one movement point)
    xp: int = 0
    promotions: list = field(default_factory=list)
    fortify: int = 0                    # turns fortified (0, 1, 2+)
    activity: Optional[str] = None      # fortify | sleep | build | goto | explore | automate | heal | setup
    build: Optional[dict] = None        # {"target": str, "progress": int, "total": int}
    goto: Optional[int] = None
    attacks: int = 0                    # attacks made this turn
    acted: bool = False
    name: Optional[str] = None
    camp: Optional[int] = None          # barbarian home camp id
    created_turn: int = 0
    interceptions: int = 0              # interceptions made this turn
    religion: Optional[str] = None      # religious units: the religion they carry
    religious_strength: int = 0
    abilities_used: dict = field(default_factory=dict)   # ability -> times used
    status: list = field(default_factory=list)           # temporary statuses (e.g. "Set Up")
    due_heal: bool = False
    carried_by: Optional[int] = None
    pending_promotions: int = 0         # free promotion picks
    origin_city: Optional[int] = None
    promotion_count: int = 0            # promotions bought with XP (sets the next XP threshold)
    religious_strength_lost: int = 0
    original_owner: Optional[int] = None


@dataclass
class City:
    """One city: its population, buildings, production queue, worked tiles and status."""
    id: int
    name: str
    owner: int
    idx: int
    pop: int = 1
    food: float = 0.0
    culture: float = 0.0                # stored culture toward the next border tile
    tiles_claimed: int = 0
    buildings: list = field(default_factory=list)
    queue: list = field(default_factory=list)            # [{"kind": "unit"|"building"|"perpetual", "id": name}]
    progress: dict = field(default_factory=dict)         # construction name -> production invested
    overflow: float = 0.0
    health: int = 200                   # current HP (max depends on buildings)
    worked: list = field(default_factory=list)
    locked: list = field(default_factory=list)
    specialists: dict = field(default_factory=dict)      # specialist name -> count
    manual_specialists: bool = False
    focus: str = "balanced"
    founded_turn: int = 0
    founder: int = 0                    # civ that founded the city
    previous_owner: Optional[int] = None
    original_capital: bool = False
    puppet: bool = False
    resistance: int = 0                 # turns of resistance left
    attacked: bool = False
    razing: bool = False
    tiles_bought: int = 0
    damaged_turn: int = -1
    auto_production: bool = False
    pressures: dict = field(default_factory=dict)         # religion -> pressure (followers are derived)
    religions_adopted: list = field(default_factory=list)
    holy_city_of: Optional[str] = None
    wltkd: int = 0                      # We Love The King Day turns left
    demanded_resource: Optional[str] = None
    demand_countdown: int = 0
    free_buildings: list = field(default_factory=list)    # buildings provided for free (no maintenance)
    bought_this_turn: list = field(default_factory=list)
    avoid_growth: bool = False
    turn_acquired: int = 0
    spaceship_parts: dict = field(default_factory=dict)


@dataclass
class Player:
    """One civilization: its technologies, policies, religion, spies, gold and relationships."""
    id: int
    name: str                           # civilization display name (players may rename)
    color: str
    nation: str = "BenchmarkCiv"        # ruleset nation (bonuses)
    kind: str = "major"                 # major | city_state | barbarian
    controller: str = "human"           # human | llm | mcp | bot | minor | barbarian (for difficulty handicaps)
    difficulty: Optional[str] = None    # this seat's difficulty (None = the game difficulty)
    leader: str = ""
    alive: bool = True
    gold: float = 0.0
    # research
    techs: list = field(default_factory=list)
    research_queue: list = field(default_factory=list)   # [current, next, ...] (goal path)
    research_goal: Optional[str] = None
    research_progress: dict = field(default_factory=dict)
    overflow_science: float = 0.0
    free_techs: int = 0
    future_techs: int = 0
    # culture & policies
    culture: float = 0.0
    policies: list = field(default_factory=list)          # adopted policies and branches (incl. "... Complete")
    policies_adopted_count: int = 0                       # policies counted for cost (UnCiv numberOfAdoptedPolicies)
    free_policies: int = 0
    # religion
    faith: float = 0.0
    religion_state: str = "none"                         # none | pantheon | founding | religion | enhancing | enhanced
    religion: Optional[str] = None                       # religion (or pantheon) this civ founded
    great_prophets_earned: int = 0
    faith_buys: dict = field(default_factory=dict)       # item -> times bought with increasing cost
    # great people & golden ages
    gp_points: dict = field(default_factory=dict)        # great person group -> points
    gp_threshold: float = 100.0                          # points for next great person (non-general)
    gg_points: dict = field(default_factory=dict)        # combat-earned GP (General/Admiral) -> points
    gg_threshold: dict = field(default_factory=dict)
    free_great_people: int = 0
    great_people_earned: int = 0
    golden_age_points: float = 0.0
    golden_age_turns: int = 0
    golden_ages: int = 0
    # misc civ
    temp_uniques: list = field(default_factory=list)     # [{"text": str, "turns": int}]
    built_increasing: dict = field(default_factory=dict) # item -> times built (Cost increases when built)
    bought_increasing: dict = field(default_factory=dict)
    free_buildings: dict = field(default_factory=dict)   # city id -> [building]
    free_stat_buildings: list = field(default_factory=list)
    free_specific_buildings: list = field(default_factory=list)
    natural_wonders: list = field(default_factory=list)
    spies: list = field(default_factory=list)            # see espionage.py
    spy_eras: list = field(default_factory=list)
    # visibility / contacts
    explored: bytearray = field(default_factory=bytearray)
    memory: dict = field(default_factory=dict)           # tile idx -> last seen snapshot
    met: list = field(default_factory=list)
    capital: Optional[int] = None
    original_capital: Optional[int] = None
    notes: str = ""
    founded_city: bool = False
    eliminated_turn: Optional[int] = None
    city_counter: int = 0
    # city-state only
    cs_type: Optional[str] = None
    cs_personality: Optional[str] = None                 # Friendly | Neutral | Hostile | Irrational
    cs_resource: Optional[str] = None                    # unique luxury (Mercantile)
    cs_unique_unit: Optional[str] = None
    influence: dict = field(default_factory=dict)        # major id (str) -> influence
    ally: Optional[int] = None
    protectors: list = field(default_factory=list)
    quests: list = field(default_factory=list)
    cs_unit_timer: dict = field(default_factory=dict)    # major id (str) -> turns until next gifted unit
    tribute_turn: dict = field(default_factory=dict)     # major id (str) -> last turn bullied
    ruins_rewards: list = field(default_factory=list)
    flags: dict = field(default_factory=dict)            # misc countdowns

    def to_dict(self) -> dict:
        """The form written to a save."""
        d = {f.name: getattr(self, f.name) for f in fields(self)}
        d["explored"] = base64.b64encode(bytes(self.explored)).decode("ascii")
        d["memory"] = {str(k): v for k, v in self.memory.items()}
        return d

    @classmethod
    def from_dict(cls, d: dict) -> "Player":
        """Read a player back from a save."""
        d = dict(d)
        d["explored"] = bytearray(base64.b64decode(d["explored"]))
        d["memory"] = {int(k): v for k, v in d["memory"].items()}
        known = {f.name for f in fields(cls)}
        return cls(**{k: v for k, v in d.items() if k in known})


def _load_dc(cls, d: dict):
    """Rebuild a dataclass from a dictionary, ignoring fields it no longer has.

    Ignoring unknown fields is what lets an older save load into a newer CITAR: a field that was
    removed is dropped rather than raising.
    """
    known = {f.name for f in fields(cls)}
    return cls(**{k: v for k, v in d.items() if k in known})


@dataclass
class GameState:
    """Everything about a game that is saved.

    The boundary between this and :class:`~citar.engine.game.Game` is the boundary between what is
    persisted and what is derived. Anything here survives a save and a reload; anything on ``Game`` -
    the occupancy index, the caches - is rebuilt. Keeping that line sharp is what makes a game a value
    rather than a process.
    """
    config: dict
    width: int
    height: int
    tiles: list
    players: list
    units: dict = field(default_factory=dict)
    cities: dict = field(default_factory=dict)
    turn: int = 1
    current: int = 0
    next_id: int = 1
    rng_state: Any = None
    phase: str = "playing"              # playing | over
    winner: Optional[int] = None
    victory: Optional[str] = None
    relations: dict = field(default_factory=dict)       # "a,b" (a<b) -> see diplomacy.new_relation
    open_borders: dict = field(default_factory=dict)    # "a>b" (a lets b in) -> until turn
    deals: list = field(default_factory=list)
    negotiations: list = field(default_factory=list)
    messages: list = field(default_factory=list)
    thoughts: list = field(default_factory=list)
    events: list = field(default_factory=list)
    camps: dict = field(default_factory=dict)           # camp id -> {"idx": int, "timer": int, ...}
    stats: list = field(default_factory=list)
    turn_started: bool = False
    barbarian_state: dict = field(default_factory=dict)
    capture_ids: dict = field(default_factory=dict)
    religions: dict = field(default_factory=dict)       # name -> {"founder", "kind": pantheon|religion, "beliefs", "holy_city", "enhanced"}
    wonders_built: dict = field(default_factory=dict)   # world wonder -> city id
    un: dict = field(default_factory=dict)              # diplomatic victory voting state
    spaceship: dict = field(default_factory=dict)       # pid -> {part: count}
    continents: list = field(default_factory=list)      # tile idx -> continent id (-1 for water)
    first_discovered: dict = field(default_factory=dict)  # natural wonder -> pid

    def to_dict(self) -> dict:
        """The whole game as a plain dictionary, ready to serialise."""
        return {
            "config": self.config, "width": self.width, "height": self.height,
            "tiles": [t.to_list() for t in self.tiles],
            "players": [p.to_dict() for p in self.players],
            "units": {str(k): asdict(v) for k, v in self.units.items()},
            "cities": {str(k): asdict(v) for k, v in self.cities.items()},
            "turn": self.turn, "current": self.current, "next_id": self.next_id, "rng_state": self.rng_state,
            "phase": self.phase, "winner": self.winner, "victory": self.victory, "relations": self.relations,
            "open_borders": self.open_borders, "deals": self.deals, "negotiations": self.negotiations,
            "messages": self.messages, "thoughts": self.thoughts, "events": self.events,
            "camps": {str(k): v for k, v in self.camps.items()}, "stats": self.stats,
            "turn_started": self.turn_started, "barbarian_state": self.barbarian_state,
            "capture_ids": self.capture_ids, "religions": self.religions, "wonders_built": self.wonders_built,
            "un": self.un, "spaceship": {str(k): v for k, v in self.spaceship.items()},
            "continents": self.continents, "first_discovered": self.first_discovered,
        }

    @classmethod
    def from_dict(cls, d: dict) -> "GameState":
        """Rebuild a game state from a save."""
        return cls(
            config=d["config"], width=d["width"], height=d["height"],
            tiles=[Tile.from_list(t) for t in d["tiles"]],
            players=[Player.from_dict(p) for p in d["players"]],
            units={int(k): _load_dc(Unit, v) for k, v in d["units"].items()},
            cities={int(k): _load_dc(City, v) for k, v in d["cities"].items()},
            turn=d["turn"], current=d["current"], next_id=d["next_id"], rng_state=d["rng_state"],
            phase=d["phase"], winner=d["winner"], victory=d["victory"], relations=d["relations"],
            open_borders=d["open_borders"], deals=d["deals"], negotiations=d["negotiations"],
            messages=d["messages"], thoughts=d["thoughts"], events=d["events"],
            camps={int(k): v for k, v in d["camps"].items()}, stats=d["stats"],
            turn_started=d.get("turn_started", False), barbarian_state=d.get("barbarian_state", {}),
            capture_ids=d.get("capture_ids", {}), religions=d.get("religions", {}),
            wonders_built=d.get("wonders_built", {}), un=d.get("un", {}),
            spaceship={int(k): v for k, v in d.get("spaceship", {}).items()},
            continents=d.get("continents", []), first_discovered=d.get("first_discovered", {}),
        )
