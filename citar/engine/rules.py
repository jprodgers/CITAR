"""Loads the rule set: UnCiv-derived data in citar/data/ruleset, CITAR additions in citar/data/custom and CITAR
constants in citar/data/game.json. Every ruleset object is keyed by its display name (as UnCiv does, because
uniques refer to objects by name) and also has a snake_case "id" that tools accept."""
from __future__ import annotations

import json
import re
from functools import lru_cache
from pathlib import Path
from typing import Optional

from . import unique_types as U
from .uniques import Unique, UniqueMap, parse_list, STAT_KEY
from .state import PLAYER_COLORS
from .. import paths

DATA_DIR = paths.package_data()
YIELDS = ["food", "production", "gold", "science", "culture", "happiness", "faith"]
RULES_VERSION = 2


def _load(path: Path):
    """Read one ruleset JSON file."""
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def norm(text: str) -> str:
    """Normalise a name for loose lookup: lower case, no punctuation."""
    return re.sub(r"[^0-9a-z]+", "", str(text).lower())


class Rules:
    """The whole ruleset, loaded once and read-only thereafter.

    Every unit, building, technology, policy, belief, terrain, resource, promotion, improvement, era,
    speed, difficulty and nation, with their uniques parsed into unique maps and several indexes
    precomputed.

    Loaded once at import and never mutated. That is the single exception to the engine doing no I/O,
    and it is what allows everything else to be pure: a rule lookup is a dictionary access rather than
    a file read.
    """
    def __init__(self, data_dir: Path | str = DATA_DIR):
        data_dir = Path(data_dir)
        rs = data_dir / "ruleset"
        L = lambda f: _load(rs / f)  # noqa: E731

        t = L("techs.json")
        self.tech_columns: list = t["columns"]
        self.techs: dict = t["techs"]
        self.eras: dict = L("eras.json")
        self.era_list: list[str] = sorted(self.eras, key=lambda e: self.eras[e]["number"])
        self.buildings: dict = L("buildings.json")
        self.units: dict = L("units.json")
        self.unit_types: dict = L("unit_types.json")
        self.promotions: dict = L("promotions.json")
        self.terrains: dict = L("terrains.json")
        self.resources: dict = L("resources.json")
        self.improvements: dict = L("improvements.json")
        self.beliefs: dict = L("beliefs.json")
        self.religion_names: list = L("religions.json")
        self.specialists: dict = L("specialists.json")
        self.city_state_types: dict = L("city_state_types.json")
        self.difficulties: dict = L("difficulties.json")
        self.difficulty_list: list[str] = list(self.difficulties)
        self.speeds: dict = L("speeds.json")
        self.victories: dict = L("victories.json")
        self.quests: dict = L("quests.json")
        self.ruins: dict = L("ruins.json")
        self.personalities: dict = L("personalities.json")
        p = L("policies.json")
        self.policy_branches: dict = p["branches"]
        self.policies: dict = p["policies"]
        self.nations: dict = L("nations.json")
        custom = data_dir / "custom" / "nations.json"
        if custom.exists():
            for k, v in _load(custom).items():
                if not k.startswith("_"):
                    v.setdefault("id", re.sub(r"[^0-9a-z]+", "_", k.lower()).strip("_"))
                    self.nations[k] = v
        self.global_uniques = UniqueMap(parse_list(L("global_uniques.json")["uniques"], "Global", "Global"))
        self.const: dict = _load(data_dir / "game.json")
        self.k: dict = self.const["constants"]
        self.move_scale: int = self.const["move_scale"]

        self._prepare()
        self._index()
        self._validate()

    # ------------------------------------------------------------------
    def _umap(self, obj: dict, kind: str, extra: list | None = None) -> UniqueMap:
        """Parse an object's uniques into a unique map."""
        us = parse_list(obj.get("uniques"), kind, obj["name"])
        if extra:
            us += extra
        obj["_umap"] = UniqueMap(us)
        return obj["_umap"]

    def _prepare(self):
        """Precompute everything the engine asks for repeatedly: indexes, domains, flags, unique maps.

        Done once at load. The alternative - deriving these on demand - was measurably slower in the
        inner loops of combat and city yields.
        """
        for name, e in self.eras.items():
            self._umap(e, "Era")
        self.era_index = {e: self.eras[e]["number"] for e in self.eras}
        for name, t in self.techs.items():
            self._umap(t, "Tech")
            t["_era"] = self.era_index[t["era"]]
        self.tech_order = sorted(self.techs, key=lambda n: (self.techs[n]["column"], n))
        for name, ut in self.unit_types.items():
            self._umap(ut, "UnitType")
        for name, u in self.units.items():
            ut = self.unit_types[u["unitType"]]
            type_uniques = [Unique(x.text, "UnitType", ut["name"]) for x in ut["_umap"].all]
            self._umap(u, "Unit", type_uniques)
            u.setdefault("strength", 0)
            u.setdefault("rangedStrength", 0)
            u.setdefault("range", 2)
            u.setdefault("cost", 0)
            u["_domain"] = ut["movementType"]
            u["_ranged"] = u["rangedStrength"] > 0
            u["_melee"] = not u["_ranged"] and u["strength"] > 0
            u["_military"] = u["_ranged"] or u["_melee"]
            u["_filter_cache"] = {}
            u["_era"] = self.techs[u["requiredTech"]]["_era"] if u.get("requiredTech") in self.techs else 0
            u["_great_person"] = bool(u["_umap"].get(U.GreatPerson))
        for name, pr in self.promotions.items():
            self._umap(pr, "Promotion")
            pr.setdefault("prerequisites", [])
            pr.setdefault("unitTypes", [])
        for name, tr in self.terrains.items():
            self._umap(tr, "Terrain")
            tr["_rough"] = tr["_umap"].has_tag(U.RoughTerrain)
            tr.setdefault("movementCost", 1)
        for name, r in self.resources.items():
            self._umap(r, "Resource")
        for name, im in self.improvements.items():
            self._umap(im, "Improvement")
            im["_road"] = name in ("Road", "Railroad")
            im["_great"] = im["_umap"].has_tag(U.GreatImprovement)
        for name, b in self.buildings.items():
            self._umap(b, "Building")
            b["_any_wonder"] = bool(b.get("isWonder") or b.get("isNationalWonder"))
            b["_filter_cache"] = {}
            b.setdefault("cost", -1)
            b["_stat_related"] = self._stat_related(b)
        for name, bl in self.beliefs.items():
            self._umap(bl, "Belief")
        for name, br in self.policy_branches.items():
            self._umap(br, "Policy")
            br["branch"] = name
        for name, po in self.policies.items():
            self._umap(po, "Policy")
        for name, n in self.nations.items():
            self._umap(n, "Nation")
        for name, ct in self.city_state_types.items():
            ct["_friend"] = UniqueMap(parse_list(ct.get("friendBonusUniques"), "CityState", name))
            ct["_ally"] = UniqueMap(parse_list(ct.get("allyBonusUniques"), "CityState", name))
            self._umap(ct, "CityStateType")
        for name, ru in self.ruins.items():
            self._umap(ru, "Ruins")
        for name, sp in self.specialists.items():
            sp["_stats"] = {k: float(sp[k]) for k in ("food", "production", "gold", "science", "culture", "faith", "happiness")
                            if k in sp}

    def _stat_related(self, b: dict) -> set:
        """Which stats a building affects, for the AI's building valuation."""
        out = set()
        for k in ("food", "production", "gold", "science", "culture", "faith", "happiness"):
            if b.get(k, 0) > 0 or (b.get("percentStatBonus") or {}).get(k, 0) > 0:
                out.add(k)
        for ph in (U.Stats, U.StatsFromTiles, U.StatsPerPopulation):
            for u in b["_umap"].get(ph):
                out |= {k for k, v in u.stats.items() if v > 0}
        if b["_umap"].has_tag(U.RemovesAnnexUnhappiness):
            out.add("happiness")
        return out

    def _index(self):
        """Lookups by name or id, unlock tables, upgrade chains, unique units/buildings per nation."""
        self._by_norm: dict[str, dict[str, str]] = {}
        for kind, table in self.tables().items():
            idx = {}
            for name, obj in table.items():
                idx[norm(name)] = name
                if obj.get("id"):
                    idx[norm(obj["id"])] = name
            self._by_norm[kind] = idx
        self.unlocks: dict[str, dict] = {t: {"units": [], "buildings": [], "improvements": [], "reveals": [], "policies": []}
                                          for t in self.techs}
        for n, u in self.units.items():
            if u.get("requiredTech") in self.unlocks and not u.get("uniqueTo"):
                self.unlocks[u["requiredTech"]]["units"].append(n)
        for n, b in self.buildings.items():
            if b.get("requiredTech") in self.unlocks and not b.get("uniqueTo"):
                self.unlocks[b["requiredTech"]]["buildings"].append(n)
        for n, im in self.improvements.items():
            if im.get("techRequired") in self.unlocks and not im.get("uniqueTo"):
                self.unlocks[im["techRequired"]]["improvements"].append(n)
        for n, r in self.resources.items():
            if r.get("revealedBy") in self.unlocks:
                self.unlocks[r["revealedBy"]]["reveals"].append(n)
        self.upgrade_from: dict[str, list[str]] = {}
        for n, u in self.units.items():
            if u.get("upgradesTo"):
                self.upgrade_from.setdefault(u["upgradesTo"], []).append(n)
        self.unique_units: dict[str, dict[str, str]] = {}       # nation -> replaced -> unique
        self.unique_buildings: dict[str, dict[str, str]] = {}
        self.unique_improvements: dict[str, list[str]] = {}
        for n, u in self.units.items():
            if u.get("uniqueTo"):
                self.unique_units.setdefault(u["uniqueTo"], {})[u.get("replaces") or n] = n
        for n, b in self.buildings.items():
            if b.get("uniqueTo"):
                self.unique_buildings.setdefault(b["uniqueTo"], {})[b.get("replaces") or n] = n
        for n, im in self.improvements.items():
            if im.get("uniqueTo"):
                self.unique_improvements.setdefault(im["uniqueTo"], []).append(n)
        self.major_nations = [n for n, d in self.nations.items() if d.get("kind") == "major"
                              and not d["_umap"].has_tag(U.WillNotBeChosenForNewGames)]
        self.city_state_nations = [n for n, d in self.nations.items() if d.get("kind") == "city_state"
                                   and not d["_umap"].has_tag(U.WillNotBeChosenForNewGames)]
        self.great_person_units = [n for n, u in self.units.items() if u["_great_person"]]
        self.spaceship_parts = [n for n, u in self.units.items() if u["_umap"].has_tag(U.SpaceshipPart)]
        self.max_turns = {s: sp["turns"][-1]["untilTurn"] for s, sp in self.speeds.items()}

    def tables(self) -> dict[str, dict]:
        """Every ruleset table by name, for lookups and the rules browser."""
        return {"tech": self.techs, "unit": self.units, "building": self.buildings, "promotion": self.promotions,
                "terrain": self.terrains, "resource": self.resources, "improvement": self.improvements,
                "belief": self.beliefs, "policy": {**self.policy_branches, **self.policies}, "nation": self.nations,
                "era": self.eras, "specialist": self.specialists, "speed": self.speeds, "difficulty": self.difficulties,
                "unit_type": self.unit_types, "victory": self.victories}

    def _validate(self):
        """Check the ruleset for references that do not resolve.

        Runs at load, because a ruleset with a dangling reference fails later in a way that looks like a
        bug in the engine rather than a mistake in the data.
        """
        errors = []
        for n, t in self.techs.items():
            for p in t["prerequisites"]:
                if p not in self.techs:
                    errors.append(f"tech {n}: unknown prerequisite {p}")
        for n, u in self.units.items():
            for key, table in (("requiredTech", self.techs), ("obsoleteTech", self.techs), ("upgradesTo", self.units),
                               ("requiredResource", self.resources), ("replaces", self.units)):
                if u.get(key) and u[key] not in table:
                    errors.append(f"unit {n}: unknown {key} {u[key]}")
        for n, b in self.buildings.items():
            for key, table in (("requiredTech", self.techs), ("requiredBuilding", self.buildings),
                               ("requiredResource", self.resources), ("replaces", self.buildings)):
                if b.get(key) and b[key] not in table:
                    errors.append(f"building {n}: unknown {key} {b[key]}")
        if errors:
            raise ValueError("Rule data errors:\n" + "\n".join(errors))

    # ------------------------------------------------------------------
    def resolve(self, kind: str, text) -> Optional[str]:
        """Name of the ruleset object matching a name or id ('Bronze Working', 'bronze_working', 'bronzeworking')."""
        if text is None:
            return None
        table = self.tables()[kind]
        if text in table:
            return text
        return self._by_norm[kind].get(norm(text))

    def tech_era(self, tech: str) -> int:
        """The era index a technology belongs to."""
        return self.techs[tech]["_era"]

    def speed(self, name: Optional[str]) -> dict:
        """A game speed's definition."""
        return self.speeds.get(name) or self.speeds[self.const["default_speed"]]

    def difficulty(self, name: Optional[str]) -> dict:
        """A difficulty's definition."""
        return self.difficulties.get(name) or self.difficulties[self.const["default_difficulty"]]

    def difficulty_index(self, name: str) -> int:
        """A difficulty as an index, for comparisons."""
        return self.difficulty_list.index(name) if name in self.difficulty_list else 0

    def is_water_terrain(self, terrain: str) -> bool:
        """Whether a terrain is water."""
        return self.terrains[terrain]["type"] == "Water"

    def map_size_predefined(self, width: int, height: int) -> dict:
        """UnCiv's getPredefinedOrNextSmaller for a rectangle: equivalent hexagonal radius."""
        import math
        area = width * height
        radius = (math.sqrt(12 * area - 3) - 3) / 6            # inverse of 1 + 3r(r+1)
        best = self.const["map_size_predefined"][0]
        for p in self.const["map_size_predefined"]:
            if p["radius"] <= radius:
                best = p
        return best

    def to_client(self) -> dict:
        """The rule set for clients and AI agents (internal fields removed)."""
        def strip(d):
            """Remove the internal fields from an object before sending it to the browser."""
            return {k: v for k, v in d.items() if not k.startswith("_")}

        def table(t):
            """Prepare one table for the client."""
            return {k: strip(v) for k, v in t.items()}
        return {
            "version": RULES_VERSION,
            "eras": table(self.eras), "era_list": self.era_list,
            "techs": table(self.techs), "tech_order": self.tech_order, "unlocks": self.unlocks,
            "units": table(self.units), "unit_types": table(self.unit_types), "promotions": table(self.promotions),
            "buildings": table(self.buildings), "terrains": table(self.terrains), "resources": table(self.resources),
            "improvements": table(self.improvements), "beliefs": table(self.beliefs),
            "policy_branches": table(self.policy_branches), "policies": table(self.policies),
            "nations": {k: strip({kk: vv for kk, vv in v.items() if kk != "cities"}) for k, v in self.nations.items()},
            "major_nations": self.major_nations, "specialists": table(self.specialists),
            "city_state_types": {k: strip(v) for k, v in self.city_state_types.items()},
            "difficulties": table(self.difficulties), "difficulty_list": self.difficulty_list,
            "speeds": table(self.speeds), "victories": table(self.victories),
            "move_scale": self.move_scale, "map_sizes": self.const["map_sizes"], "map_types": self.const["map_types"],
            "max_players": self.const["max_players"], "player_colors": PLAYER_COLORS,
            "default_speed": self.const["default_speed"], "default_difficulty": self.const["default_difficulty"],
            "benchmark_speed": self.const["benchmark_speed"], "max_turns": self.max_turns,
            "barbarian_levels": {k: (v["name"] if v else "Off") for k, v in self.const["barbarians"]["levels"].items()},
        }


@lru_cache(maxsize=4)
def get_rules(data_dir: str = str(DATA_DIR)) -> Rules:
    """The ruleset, loaded once and cached."""
    return Rules(data_dir)


def stat_key(name: str) -> str:
    """The internal key for a stat name, such as ``Science`` to ``science``."""
    return STAT_KEY.get(name, name.lower())
