"""UnCiv-style "uniques": the small rule language the ruleset uses for almost every special effect.

A unique is a sentence with [parameters] and optional <conditionals>, e.g.
    "[+20]% Strength <for [Mounted] units> <when attacking>"
Its *placeholder* ("[]% Strength") identifies the effect; the engine asks "which uniques with placeholder X apply
here?" and reads their parameters. This module parses uniques, evaluates conditionals against a Ctx (the game
situation being asked about), and implements the filter mini-language ("{Military} {Land}", "non-[Air]", ...).

Ported from UnCiv (Unique.kt, Conditionals.kt, GameContext.kt, MultiFilter.kt and the various matchesFilter
functions), MPL-2.0.
"""
from __future__ import annotations

from typing import TYPE_CHECKING, Callable, Iterable, Optional

if TYPE_CHECKING:
    from .game import Game

STAT_NAMES = ("Production", "Food", "Gold", "Science", "Culture", "Happiness", "Faith")
STAT_KEY = {s: s.lower() for s in STAT_NAMES}
CIV_WIDE_STATS = {"gold", "science", "culture", "faith", "happiness"}
STAT_OF_KEY = {v: k for k, v in STAT_KEY.items()}


# ---------------------------------------------------------------------------------------------------------------
# Parsing
# ---------------------------------------------------------------------------------------------------------------
def split_modifiers(text: str) -> tuple[str, list[str]]:
    """'X <a> <b>' -> ('X', ['a', 'b']). Angle brackets inside [] are part of parameters."""
    main, mods = [], []
    depth_sq = 0
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        if c == "[":
            depth_sq += 1
        elif c == "]":
            depth_sq = max(0, depth_sq - 1)
        if c == "<" and depth_sq == 0:
            j = i + 1
            d = 0
            while j < n:
                if text[j] == "[":
                    d += 1
                elif text[j] == "]":
                    d -= 1
                elif text[j] == ">" and d <= 0:
                    break
                j += 1
            mods.append(text[i + 1:j].strip())
            i = j + 1
            continue
        main.append(c)
        i += 1
    return "".join(main).strip(), mods


def placeholder(text: str) -> tuple[str, list[str]]:
    """Top-level [params] -> ('[]% Strength', ['+20'])."""
    out, params = [], []
    depth, start = 0, -1
    for i, c in enumerate(text):
        if c == "[":
            if depth == 0:
                start = i + 1
                out.append("[]")
            depth += 1
            continue
        if c == "]" and depth > 0:
            depth -= 1
            if depth == 0:
                params.append(text[start:i])
            continue
        if depth == 0:
            out.append(c)
    return "".join(out), params


def parse_stats(text: str) -> Optional[dict]:
    """'+1 Food, +2 Gold' -> {'food': 1.0, 'gold': 2.0}; None if not a stats string."""
    out = {}
    for part in text.split(","):
        bits = part.strip().split(" ", 1)
        if len(bits) != 2 or bits[1] not in STAT_KEY:
            return None
        try:
            out[STAT_KEY[bits[1]]] = out.get(STAT_KEY[bits[1]], 0.0) + float(bits[0])
        except ValueError:
            return None
    return out or None


def num(s) -> float:
    """Read a number out of a unique's parameter, treating anything unparseable as zero.

    Parameters arrive as text - ``"+15"``, ``"2"``, occasionally a word - and a malformed one in a
    ruleset should make a rule do nothing rather than crash a game in progress.
    """
    try:
        return float(str(s).replace("+", ""))
    except ValueError:
        return 0.0


class Unique:
    """One rule, parsed: its placeholder text, its parameters, and its conditions.

    UnCiv writes rules as sentences - ``[+15]% Strength <when attacking>``. Parsing splits that into a
    *placeholder* (the sentence with its values replaced by ``[]``, which is what the engine matches
    on), the *parameters* (the values), and the *modifiers* (the ``<…>`` conditions, themselves
    uniques).

    Matching on the placeholder rather than the whole text is what makes the ruleset data: every rule
    of the same shape is handled by one piece of code, whatever its numbers.

    ``src_type`` and ``src_name`` record where the rule came from, which is how a stat breakdown can say
    "+2 from Temple" rather than "+2 from somewhere".
    """
    __slots__ = ("text", "ph", "params", "mods", "src_type", "src_name", "_stats", "is_local", "mod_phs", "timed")

    def __init__(self, text: str, src_type: Optional[str] = None, src_name: Optional[str] = None, _is_mod=False):
        self.text = text
        main, mods = split_modifiers(text)
        self.ph, self.params = placeholder(main)
        self.mods = [Unique(m, _is_mod=True) for m in mods] if not _is_mod else []
        self.mod_phs = {m.ph for m in self.mods}
        self.src_type = src_type
        self.src_name = src_name
        self._stats = False
        self.is_local = "in this city" in self.params or "in this city" in self.mod_phs
        self.timed = "for [] turns" in self.mod_phs

    @property
    def stats(self) -> dict:
        """The stats this unique grants, parsed from its parameters and cached."""
        if self._stats is False:
            self._stats = next((s for s in (parse_stats(p) for p in self.params) if s), None) or {}
        return self._stats

    def mod(self, ph: str) -> Optional["Unique"]:
        """The first condition with this placeholder, or None."""
        for m in self.mods:
            if m.ph == ph:
                return m
        return None

    def has_mod(self, ph: str) -> bool:
        """Whether this unique carries a condition with this placeholder."""
        return ph in self.mod_phs

    def p(self, i: int) -> str:
        """Parameter *i* as text."""
        return self.params[i]

    def n(self, i: int) -> float:
        """Parameter *i* as a number."""
        return num(self.params[i])

    def __repr__(self):
        return f"Unique({self.text!r})"


class UniqueMap:
    """Uniques of one or more ruleset objects, indexed by placeholder."""
    __slots__ = ("by_ph", "all")

    def __init__(self, uniques: Iterable[Unique] = ()):
        self.by_ph: dict[str, list[Unique]] = {}
        self.all: list[Unique] = []
        for u in uniques:
            self.add(u)

    def add(self, u: Unique):
        """Index one unique."""
        self.by_ph.setdefault(u.ph, []).append(u)
        self.all.append(u)

    def get(self, ph: str) -> list[Unique]:
        """Every unique with this placeholder, conditions not yet evaluated."""
        return self.by_ph.get(ph, ())

    def has_tag(self, tag: str) -> bool:
        """Whether any unique with this placeholder exists, ignoring conditions.

        For the rules that are simply present or absent - "is a nuclear weapon", "cannot be purchased" -
        where evaluating conditions would be wasted work.
        """
        return tag in self.by_ph

    def matching(self, ph: str, ctx: "Ctx") -> list[Unique]:
        """Every unique with this placeholder whose conditions hold in *ctx*.

        Timed uniques are excluded: they are applied as one-off triggers when they fire, and counting them
        here as well would apply them twice.
        """
        lst = self.by_ph.get(ph)
        if not lst:
            return []
        return [u for u in lst if not u.timed and applies(u, ctx)]

    def has(self, ph: str, ctx: "Ctx") -> bool:
        """Whether any unique with this placeholder applies in *ctx*, stopping at the first."""
        lst = self.by_ph.get(ph)
        return bool(lst) and any(not u.timed and applies(u, ctx) for u in lst)


def parse_list(texts, src_type: str, src_name: str) -> list[Unique]:
    """Parse a list of rule texts into uniques, remembering what they came from."""
    return [Unique(t, src_type, src_name) for t in texts or ()]


# ---------------------------------------------------------------------------------------------------------------
# Context
# ---------------------------------------------------------------------------------------------------------------
class Ctx:
    """What a unique is being evaluated for (GameContext in UnCiv). Tiles are indices, civs are player ids."""
    __slots__ = ("g", "civ", "city", "unit", "tile", "our", "their", "attacked_tile", "action", "other_civ",
                 "ignore", "region")

    def __init__(self, g: "Game", civ: Optional[int] = None, city=None, unit=None, tile: Optional[int] = None,
                 our=None, their=None, attacked_tile: Optional[int] = None, action: Optional[str] = None,
                 other_civ: Optional[int] = None, ignore: bool = False, region: Optional[str] = None):
        self.g = g
        self.city = city
        self.unit = unit
        self.our = our            # combat.Combatant
        self.their = their
        self.attacked_tile = attacked_tile
        self.action = action      # "attack" | "defend"
        self.other_civ = other_civ
        self.ignore = ignore
        self.region = region
        if civ is None:
            if city is not None:
                civ = city.owner
            elif unit is not None:
                civ = unit.owner
            elif our is not None:
                civ = our.owner
        self.civ = civ
        if tile is None:
            if unit is not None:
                tile = unit.idx
            elif city is not None:
                tile = city.idx
            elif our is not None:
                tile = our.idx
        self.tile = tile

    @property
    def rel_unit(self):
        """The unit a rule means: the combatant's, if this is a fight, otherwise the one supplied."""
        if self.our is not None and self.our.unit is not None:
            return self.our.unit
        return self.unit

    @property
    def rel_tile(self) -> Optional[int]:
        """The tile a rule means: the one under attack, if this is a fight, otherwise the one supplied."""
        return self.attacked_tile if self.attacked_tile is not None else self.tile

    @property
    def rel_city(self):
        """The city a rule means, preferring an explicit one over a combatant's."""
        if self.city is not None:
            return self.city
        if self.our is not None and self.our.city is not None:
            return self.our.city
        t = self.tile
        if t is None or self.g is None:
            return None
        cid = self.g.s.tiles[t].city
        c = self.g.s.cities.get(cid) if cid is not None else None
        if c is not None and c.owner == self.civ:
            return c
        return None

    def with_(self, **kw) -> "Ctx":
        """A copy of this context with some fields replaced."""
        c = Ctx.__new__(Ctx)
        for k in Ctx.__slots__:
            setattr(c, k, kw[k] if k in kw else getattr(self, k))
        return c


def civ_ctx(g, pid, **kw) -> Ctx:
    """A context for a civilization-wide question."""
    return Ctx(g, civ=pid, **kw)


# ---------------------------------------------------------------------------------------------------------------
# Multi-filter
# ---------------------------------------------------------------------------------------------------------------
def multi_filter(text: str, fn: Callable[[str], bool]) -> bool:
    """Evaluate a filter that may be a conjunction or a negation.

    UnCiv filters can be ``{Land} {Wounded}`` - both must hold - or ``non-[Land]``. Handling that here
    means every ``*_matches`` function gets the combining forms for free and only has to answer for a
    single term.
    """
    if text.startswith("{") and text.endswith("}") and "} {" in text:
        return all(multi_filter(part, fn) for part in _and_parts(text))
    if text.startswith("non-[") and text.endswith("]"):
        return not multi_filter(text[5:-1], fn)
    return fn(text)


def _and_parts(text: str) -> list[str]:
    """Split ``{a} {b}`` into its parts, respecting nesting."""
    inner = text[1:-1]
    parts, depth, cur = [], 0, []
    i = 0
    while i < len(inner):
        c = inner[i]
        if depth == 0 and inner.startswith("} {", i):
            parts.append("".join(cur))
            cur = []
            i += 3
            continue
        if c in "[{":
            depth += 1
        elif c in "]}":
            depth -= 1
        cur.append(c)
        i += 1
    parts.append("".join(cur))
    return parts


# ---------------------------------------------------------------------------------------------------------------
# Filters (UniqueParameterType implementations)
# ---------------------------------------------------------------------------------------------------------------
ALL = ("All", "all")


def terrain_matches(R, terrain: str, f: str) -> bool:
    """Whether a terrain satisfies a filter."""
    td = R.terrains.get(terrain)
    if td is None:
        return False
    return multi_filter(f, lambda s: _terrain_single(R, td, s))


def _terrain_single(R, td: dict, f: str) -> bool:
    """Match one term against a terrain definition."""
    if f in ALL or f == "Terrain":
        return True
    if f == "Impassable":
        return bool(td.get("impassable"))
    if f == "Open terrain":
        return not td["_rough"]
    if f == "Rough terrain":
        return td["_rough"]
    if f == "Natural Wonder":
        return td["type"] == "NaturalWonder"
    if f == "Terrain Feature":
        return td["type"] == "TerrainFeature"
    if f == td["name"] or f == td["type"]:
        return True
    return td["_umap"].has_tag(f)


def improvement_matches(R, imp: str, f: str) -> bool:
    """Whether an improvement satisfies a filter."""
    d = R.improvements.get(imp)
    if d is None:
        return False
    return multi_filter(f, lambda s: s in ALL or s == "Improvement" or (s == "All Road" and d.get("_road"))
                        or (s in ("Great Improvement", "Great") and d["_umap"].has_tag("Great Improvement"))
                        or s == imp or s == d.get("replaces") or d["_umap"].has_tag(s))


def resource_matches(R, res: str, f: str) -> bool:
    """Whether a resource satisfies a filter, by name, type or unique."""
    d = R.resources.get(res)
    if d is None:
        return False

    def single(s):
        """Match one term against this resource."""
        if s in (res, "any", "all", "All") or s == d["resourceType"]:
            return True
        if s.endswith(" resource") and s[:-9] == d["resourceType"]:
            return True
        if any(s == k.capitalize() for k in (d.get("improvementStats") or {})):
            return True
        return d["_umap"].has_tag(s)
    return multi_filter(f, single)


def tile_matches(g: "Game", idx: int, f: str, civ: Optional[int] = None) -> bool:
    """Whether a tile satisfies a filter, including what is on it."""
    return multi_filter(f, lambda s: _tile_single(g, idx, s, civ))


def tile_terrain_matches(g: "Game", idx: int, f: str, civ: Optional[int] = None) -> bool:
    """Whether a tile's terrain alone satisfies a filter, ignoring improvements and units."""
    return multi_filter(f, lambda s: _tile_terrain_single(g, idx, s, civ))


def _tile_single(g, idx, f, civ) -> bool:
    """Match one term against a tile."""
    if _tile_terrain_single(g, idx, f, civ):
        return True
    t = g.s.tiles[idx]
    imp = t.improvement if not t.pillaged else None
    if f == "unimproved":
        return imp is None
    if f == "improved":
        return imp is not None
    if f == "pillaged":
        return t.pillaged or t.route_pillaged
    if f == "worked":
        c = g.s.cities.get(t.city) if t.city is not None else None
        return c is not None and idx in c.worked
    if imp and improvement_matches(g.rules, imp, f):
        return True
    if t.route and not t.route_pillaged and improvement_matches(g.rules, t.route, f):
        return True
    return False


def _tile_terrain_single(g, idx, f, civ) -> bool:
    """Match one term against a tile's terrain and features."""
    from . import tiles as T
    t = g.s.tiles[idx]
    R = g.rules
    if f in ALL or f == "Terrain":
        return True
    if f == "Water":
        return T.is_water(g, idx)
    if f == "Land":
        return not T.is_water(g, idx)
    if f == "Coastal":
        return not T.is_water(g, idx) and T.adjacent_to_coast(g, idx)
    if f == "River":
        return t.river
    if f == "Unowned":
        return t.owner is None
    if f == "your":
        return civ is not None and t.owner == civ
    if f in ("Foreign Land", "Foreign"):
        return civ is not None and not T.is_friendly_territory(g, idx, civ)
    if f in ("Friendly Land", "Friendly"):
        return civ is not None and T.is_friendly_territory(g, idx, civ)
    if f in ("Enemy Land", "Enemy"):
        return civ is not None and t.owner is not None and g.at_war(civ, t.owner)
    if f == "resource":
        return t.resource is not None and civ is not None and T.resource_visible(g, civ, t.resource)
    if f == "Water resource":
        return T.is_water(g, idx) and t.resource is not None and civ is not None and T.resource_visible(g, civ, t.resource)
    if f == "Featureless":
        return not t.features
    if f == "Open terrain":
        return not T.is_rough(g, idx)
    if f in ("Fresh water", "Fresh Water"):
        return T.fresh_water(g, idx)
    if f == "Elevated":
        return t.terrain == "Mountain" or "Hill" in t.features
    if f == t.terrain:
        return True
    if t.resource and f == t.resource:
        return civ is None or T.resource_visible(g, civ, t.resource)
    for ter in T.all_terrains(t):
        if _terrain_single(R, R.terrains[ter], f):
            return True
    if t.owner is not None and civ_matches(g, t.owner, f, civ, multi=False):
        return True
    if t.resource:
        rd = R.resources[t.resource]
        ok = f == t.resource or rd["_umap"].has_tag(f) or (f.endswith(" resource") and f[:-9] == rd["resourceType"])
        if ok:
            return civ is None or T.resource_visible(g, civ, t.resource)
    return False


def base_unit_matches(R, utype: str, f: str) -> bool:
    """Whether a unit *type* satisfies a filter, without reference to a particular unit."""
    ud = R.units.get(utype)
    if ud is None:
        return False
    cache = ud["_filter_cache"]
    r = cache.get(f)
    if r is None:
        r = multi_filter(f, lambda s: _base_unit_single(R, ud, s))
        cache[f] = r
    return r


def _base_unit_single(R, ud, f) -> bool:
    """Match one term against a unit type."""
    if f in ALL:
        return True
    if f == "Melee":
        return ud["_melee"]
    if f == "Ranged":
        return ud["_ranged"]
    if f == "Civilian":
        return not ud["_military"]
    if f == "Military":
        return ud["_military"]
    if f == "Land":
        return ud["_domain"] == "Land"
    if f == "Water":
        return ud["_domain"] == "Water"
    if f == "Air":
        return ud["_domain"] == "Air"
    if f == "non-air":
        return ud["_domain"] != "Air"
    if f == "Nuclear Weapon":
        return ud["_umap"].has_tag("Nuclear weapon of Strength []") or "Nuclear weapon of Strength []" in ud["_umap"].by_ph
    if f == "Great Person":
        return "Great Person - []" in ud["_umap"].by_ph
    if f == "Religious":
        return ud["_umap"].has_tag("Religious Unit")
    if f == ud["unitType"] or f == ud["name"] or f == ud.get("replaces"):
        return True
    tech = ud.get("requiredTech")
    if tech and tech in R.techs and tech_matches(R, tech, f):
        return True
    if f.endswith(" units"):
        base = f[:-6].lower().capitalize()
        if base != f and _base_unit_single(R, ud, base):
            return True
    return ud["_umap"].has_tag(f)


def unit_matches(g: "Game", unit, f: str, ctx: Optional[Ctx] = None) -> bool:
    """Whether a specific unit satisfies a filter, including its current state."""
    return multi_filter(f, lambda s: _unit_single(g, unit, s, ctx))


def _unit_single(g, unit, f, ctx) -> bool:
    """Match one term against a unit, including transient states such as wounded or embarked."""
    if f == "other":
        return ctx is None or ctx.unit is not unit
    if f in ("Wounded", "wounded units"):
        return unit.hp < 100
    if f in ("Barbarians", "Barbarian"):
        return g.is_barbarian(unit.owner)
    if f == "City-State":
        return g.player(unit.owner).kind == "city_state"
    if f == "Embarked":
        from .movement import is_embarked
        return is_embarked(g, unit)
    if f == "Non-City":
        return True
    if _base_unit_single(g.rules, g.rules.units[unit.type], f):
        return True
    if civ_matches(g, unit.owner, f, ctx.civ if ctx else None, multi=False):
        return True
    if f in unit.promotions:
        return True
    for pr in unit.promotions:
        if g.rules.promotions[pr]["_umap"].has_tag(f):
            return True
    if f in unit.status:
        return True
    return False


def civ_matches(g: "Game", pid: int, f: str, viewer: Optional[int] = None, multi: bool = True) -> bool:
    """Whether a civilization satisfies a filter."""
    def single(s):
        """Match one term against this civilization."""
        p = g.player(pid)
        if s in ALL:
            return True
        if s == "Human player":
            return p.controller in ("human", "llm", "mcp")
        if s == "AI player":
            return p.controller not in ("human", "llm", "mcp")
        if s == "Major":
            return p.kind == "major"
        if s in ("City-States", "City-State"):
            return p.kind == "city_state"
        if s == "Barbarian" or s == "Barbarians":
            return p.kind == "barbarian"
        if s == "Open Borders":
            return viewer is not None and g.has_open_borders(pid, viewer)
        if s == "Friendly":
            return viewer is not None and (viewer == pid or g.is_friend(viewer, pid))
        if s == "Hostile":
            return viewer is not None and g.at_war(viewer, pid)
        if s == "Known":
            return viewer is not None and (viewer == pid or g.has_met(viewer, pid))
        if s == p.nation:
            return True
        nd = g.rules.nations.get(p.nation)
        return bool(nd) and nd["_umap"].has_tag(s)
    return multi_filter(f, single) if multi else single(f)


def era_matches(R, era: str, f: str) -> bool:
    """Whether an era satisfies a filter, including the ``pre-[era]`` form."""
    if f in ("any era", era):
        return True
    if f.startswith("pre-[") and f.endswith("]"):
        target = R.eras.get(f[5:-1])
        return target is not None and R.eras[era]["number"] < target["number"]
    if f.startswith("post-[") and f.endswith("]"):
        target = R.eras.get(f[6:-1])
        return target is not None and R.eras[era]["number"] > target["number"]
    return False


def tech_matches(R, tech: str, f: str) -> bool:
    """Whether a technology satisfies a filter, by name, era or unique."""
    td = R.techs[tech]
    return multi_filter(f, lambda s: s in ALL or s == tech or era_matches(R, td["era"], s) or td["_umap"].has_tag(s))


def building_matches(R, bname: str, f: str) -> bool:
    """Whether a building satisfies a filter."""
    bd = R.buildings.get(bname)
    if bd is None:
        return False
    cache = bd["_filter_cache"]
    r = cache.get(f)
    if r is None:
        r = multi_filter(f, lambda s: _building_single(R, bd, s))
        cache[f] = r
    return r


def _building_single(R, bd, f) -> bool:
    """Match one term against a building."""
    if f in ALL:
        return True
    wonder = bd["_any_wonder"]
    if f in ("Building", "Buildings"):
        return not wonder
    if f in ("Wonder", "Wonders"):
        return wonder
    if f in ("National Wonder", "National"):
        return bool(bd.get("isNationalWonder"))
    if f in ("World Wonder", "World"):
        return bool(bd.get("isWonder"))
    if f == bd["name"] or f == bd.get("replaces"):
        return True
    tech = bd.get("requiredTech")
    if tech and tech in R.techs and tech_matches(R, tech, f):
        return True
    if f in STAT_KEY:
        return STAT_KEY[f] in bd["_stat_related"]
    return bd["_umap"].has_tag(f)


def policy_matches(R, pol: str, f: str) -> bool:
    """Whether a policy or branch satisfies a filter."""
    pd = R.policies.get(pol) or R.policy_branches.get(pol)
    if pd is None:
        return False
    branch = pd.get("branch", pol)
    return multi_filter(f, lambda s: s in ALL or s == pol or s == f"[{branch}] branch" or pd["_umap"].has_tag(s))


def city_matches(g: "Game", city, f: str, viewer: Optional[int] = None) -> bool:
    """Whether a city satisfies a filter, from a viewer's point of view."""
    if viewer is None:
        viewer = city.owner
    return multi_filter(f, lambda s: _city_single(g, city, s, viewer))


def _city_single(g, city, f, viewer) -> bool:
    """Match one term against a city."""
    from . import cities as C
    if f in ("in this city", "in all cities") or f in ALL:
        return True
    if f in ("in your cities", "Your"):
        return viewer == city.owner
    if f in ("in all coastal cities", "Coastal"):
        return g.is_coastal(city.idx)
    if f in ("in capital", "Capital"):
        return g.player(city.owner).capital == city.id
    if f in ("in all non-occupied cities", "Non-occupied"):
        return not C.has_annex_unhappiness(g, city) or city.puppet
    if f == "in all cities with a world wonder":
        return any(g.rules.buildings[b].get("isWonder") for b in city.buildings)
    if f == "in all cities connected to capital":
        return C.connected_to_capital(g, city)
    if f in ("in all cities with a garrison", "Garrisoned"):
        return C.is_garrisoned(g, city)
    if f == "in all cities in which the majority religion is a major religion":
        from . import religion
        r = religion.majority_religion(g, city)
        from .religion import is_major
        return r is not None and is_major(g, r)
    if f == "in all cities in which the majority religion is an enhanced religion":
        from . import religion
        r = religion.majority_religion(g, city)
        return r is not None and g.s.religions[r].get("enhanced", False)
    if f == "in non-enemy foreign cities":
        return viewer is not None and viewer != city.owner and not g.at_war(city.owner, viewer)
    if f in ("in enemy cities", "Enemy"):
        return g.at_war(city.owner, viewer if viewer is not None else city.owner)
    if f in ("in foreign cities", "Foreign"):
        return viewer is not None and viewer != city.owner
    if f in ("in annexed cities", "Annexed"):
        return city.founder != city.owner and not city.puppet
    if f in ("in puppeted cities", "Puppeted"):
        return city.puppet
    if f in ("in resisting cities", "Resisting"):
        return city.resistance > 0
    if f in ("in cities being razed", "Razing"):
        return city.razing
    if f in ("in holy cities", "Holy"):
        return city.holy_city_of is not None
    if f == "in City-State cities":
        return g.player(city.owner).kind == "city_state"
    if f == "in cities following this religion":
        return True
    if f == "in cities following our religion":
        from . import religion
        r = religion.majority_religion(g, city)
        return r is not None and g.player(viewer).religion == r
    return civ_matches(g, city.owner, f, viewer, multi=False)


def population_amount(g: "Game", city, f: str) -> int:
    """The population a rule means: total, specialists, or a particular kind."""
    if f == "Specialists":
        return sum(city.specialists.values())
    if f == "Population":
        return city.pop
    if f in ("Followers of the Majority Religion", "Followers of this Religion"):
        from . import religion
        return religion.followers_of_majority(g, city)
    if f == "Unemployed":
        from . import cities as C
        return C.free_population(city)
    return city.specialists.get(f, 0)


# ---------------------------------------------------------------------------------------------------------------
# Countables
# ---------------------------------------------------------------------------------------------------------------
def countable(ctx: Ctx, text: str) -> Optional[int]:
    """Evaluate a countable expression - a number, or something like "[Cities]".

    This is what lets a rule say "+1 per city" without the engine knowing in advance which things are
    countable.
    """
    g = ctx.g
    try:
        return int(text)
    except ValueError:
        pass
    if text == "turns":
        return g.s.turn
    pid = ctx.civ
    if text == "Cities":
        return len(g.player_cities(pid)) if pid is not None else None
    if text == "Units":
        return len(g.player_units(pid)) if pid is not None else None
    if text == "Completed Policy branches":
        from . import policies
        return policies.completed_branches(g, pid) if pid is not None else None
    if text in STAT_KEY:
        if ctx.city is not None and text in ("Food", "Production"):
            return int(ctx.city.food if text == "Food" else 0)
        return int(g.stat_reserve(pid, STAT_KEY[text])) if pid is not None else None
    ph, params = placeholder(text)
    if ph == "[] Units" and pid is not None:
        return sum(1 for u in g.player_units(pid) if unit_matches(g, u, params[0]))
    if ph == "[] Cities" and pid is not None:
        return sum(1 for c in g.player_cities(pid) if city_matches(g, c, params[0]))
    if ph == "Remaining [] Civilizations":
        return sum(1 for p in g.s.players if p.alive and civ_matches(g, p.id, params[0], pid))
    if ph == "[] Buildings" and pid is not None:
        return sum(1 for c in g.player_cities(pid) for b in c.buildings if building_matches(g.rules, b, params[0]))
    return None


# ---------------------------------------------------------------------------------------------------------------
# Conditionals
# ---------------------------------------------------------------------------------------------------------------
def applies(u: Unique, ctx: Optional[Ctx]) -> bool:
    """Whether every condition on a unique holds in this context.

    The heart of the interpreter. A unique with no conditions always applies; otherwise each ``<…>`` is
    looked up and evaluated, and an unrecognised condition makes the rule *not* apply - failing closed,
    because a rule whose condition is not understood is a rule whose behaviour is unknown.
    """
    if not u.mods:
        return True
    if ctx is None or ctx.ignore:
        return True
    for m in u.mods:
        fn = _COND.get(m.ph)
        if fn is None:
            if m.ph in _META or m.ph.startswith("upon "):
                continue          # trigger conditions and meta modifiers do not filter
            return False          # unknown conditional: never applies (same as UnCiv's `else -> false`)
        if not fn(u, m, ctx):
            return False
    return True


def _era_cmp(ctx, era_name, cmp) -> bool:
    """Compare a civilization's era against one named in a condition."""
    if ctx.civ is None:
        return False
    R = ctx.g.rules
    if era_name not in R.eras:
        return False
    from .research import player_era
    return cmp(player_era(ctx.g, ctx.civ), R.eras[era_name]["number"])


def _stat_amount(ctx, name) -> Optional[float]:
    """The current amount of a stat or resource, for a condition that compares against one."""
    g = ctx.g
    if name in g.rules.resources:
        from . import economy
        if ctx.civ is None:
            return 0
        return economy.resource_amount(g, ctx.civ, name)
    if name in STAT_KEY:
        if name == "Happiness":
            from . import economy
            return economy.happiness_for_conditionals(g, ctx.civ) if ctx.civ is not None else 0
        return g.stat_reserve(ctx.civ, STAT_KEY[name]) if ctx.civ is not None else 0
    return None


def _speed_mod(ctx, u, name) -> float:
    """The game-speed multiplier, for uniques marked as modified by speed."""
    if not u.has_mod("(modified by game speed)"):
        return 1.0
    return ctx.g.speed["modifier"]


def _chance(u, m, ctx) -> bool:
    """Roll a probabilistic condition.

    The generator is derived from the game's state and the unique's own text, so the same rule in the
    same situation rolls the same way in a replay. A shared random stream would not survive one extra
    call anywhere in the turn.
    """
    g = ctx.g
    rng = g.state_rng("chance", g.s.turn, u.text, ctx.civ, ctx.tile, getattr(ctx.rel_unit, "id", None))
    return rng.random() < m.n(0) / 100


def _civ(ctx):
    """The context's civilization, or None."""
    return ctx.g.player(ctx.civ) if ctx.civ is not None else None


def _has_building(g, pid, b) -> bool:
    """Whether a civilization has this building anywhere, counting unique replacements."""
    return any(b in c.buildings or _equiv(g, c, b) for c in g.player_cities(pid))


def _equiv(g, city, b) -> bool:
    """Whether a city has a building that replaces the named one."""
    return any(g.rules.buildings[x].get("replaces") == b for x in city.buildings)


def _city_has(g, city, b) -> bool:
    """Whether a city has this building or its replacement."""
    return b in city.buildings or _equiv(g, city, b)


def _pol_or_belief(ctx, name) -> bool:
    """Whether a civilization has adopted a named policy or belief."""
    p = _civ(ctx)
    if p is None:
        return False
    if name in p.policies:
        return True
    from . import religion
    return name in religion.civ_beliefs(ctx.g, ctx.civ)


def _cmp_countables(ctx, a, b, op) -> bool:
    """Compare two countable expressions, failing when either cannot be evaluated."""
    x, y = countable(ctx, a), countable(ctx, b)
    return x is not None and y is not None and op(x, y)


def _vs_combatant(ctx, f) -> bool:
    """Whether the opposing combatant matches a filter, for conditions such as <vs [Land] units>."""
    t = ctx.their
    if t is None:
        return False
    return t.matches(ctx.g, f)


def _forest_ok(ctx):
    """Whether there is a tile in context at all, for conditions that need one."""
    return ctx.rel_tile is not None


_COND: dict[str, Callable[[Unique, Unique, Ctx], bool]] = {
    "with []% chance": _chance,
    "every [] turns": lambda u, m, c: c.g.s.turn % max(1, int(m.n(0))) == 0,
    "before turn number []": lambda u, m, c: c.g.s.turn < m.n(0),
    "after turn number []": lambda u, m, c: c.g.s.turn >= m.n(0),
    "if tutorials are enabled": lambda u, m, c: False,
    "if tutorial [] is completed": lambda u, m, c: False,
    "for [] players": lambda u, m, c: c.civ is not None and civ_matches(c.g, c.civ, m.p(0), c.civ),
    "when at war": lambda u, m, c: c.civ is not None and c.g.is_at_war_any(c.civ),
    "when not at war": lambda u, m, c: c.civ is not None and not c.g.is_at_war_any(c.civ),
    "with []": lambda u, m, c: (_stat_amount(c, m.p(0)) or 0) > 0,
    "without []": lambda u, m, c: (_stat_amount(c, m.p(0)) or 0) <= 0,
    "when above [] []": lambda u, m, c: (_stat_amount(c, m.p(1)) is not None and
                                        _stat_amount(c, m.p(1)) > m.n(0) * _speed_mod(c, u, m.p(1))),
    "when below [] []": lambda u, m, c: (_stat_amount(c, m.p(1)) is not None and
                                        _stat_amount(c, m.p(1)) < m.n(0) * _speed_mod(c, u, m.p(1))),
    "when between [] and [] []": lambda u, m, c: (_stat_amount(c, m.p(2)) is not None and
                                                  m.n(0) <= _stat_amount(c, m.p(2)) <= m.n(1)),
    "while the empire is happy": lambda u, m, c: c.civ is not None and _happiness(c) >= 0,
    "during a Golden Age": lambda u, m, c: c.civ is not None and c.g.player(c.civ).golden_age_turns > 0,
    "outside a Golden Age": lambda u, m, c: c.civ is not None and c.g.player(c.civ).golden_age_turns <= 0,
    "before the []": lambda u, m, c: _era_cmp(c, m.p(0), lambda a, b: a < b),
    "starting from the []": lambda u, m, c: _era_cmp(c, m.p(0), lambda a, b: a >= b),
    "during the []": lambda u, m, c: _era_cmp(c, m.p(0), lambda a, b: a == b),
    "if starting in the []": lambda u, m, c: c.g.s.config.get("starting_era", "Ancient era") == m.p(0),
    "on [] game speed": lambda u, m, c: c.g.speed["name"] == m.p(0),
    "on [] difficulty": lambda u, m, c: c.g.difficulty_name(c.civ) == m.p(0),
    "on [] difficulty or higher": lambda u, m, c: c.g.difficulty_index(c.civ) >= c.g.rules.difficulty_index(m.p(0)),
    "on [] difficulty or lower": lambda u, m, c: c.g.difficulty_index(c.civ) <= c.g.rules.difficulty_index(m.p(0)),
    "when [] Victory is enabled": lambda u, m, c: c.g.victory_enabled(m.p(0)),
    "when [] Victory is disabled": lambda u, m, c: not c.g.victory_enabled(m.p(0)),
    "when religion is enabled": lambda u, m, c: c.g.religion_enabled,
    "when religion is disabled": lambda u, m, c: not c.g.religion_enabled,
    "when espionage is enabled": lambda u, m, c: c.g.espionage_enabled,
    "when espionage is disabled": lambda u, m, c: not c.g.espionage_enabled,
    "when nuclear weapons are enabled": lambda u, m, c: c.g.nukes_enabled,
    "when nuclear weapons are disabled": lambda u, m, c: not c.g.nukes_enabled,
    "after discovering []": lambda u, m, c: c.civ is not None and _tech_cond(c, m.p(0), True),
    "before discovering []": lambda u, m, c: c.civ is not None and _tech_cond(c, m.p(0), False),
    "while researching []": lambda u, m, c: c.civ is not None and bool(_civ(c).research_queue) and
        tech_matches(c.g.rules, _civ(c).research_queue[0], m.p(0)),
    "if no other Civilization has adopted []": lambda u, m, c: not any(
        p.alive and p.kind == "major" and m.p(0) in p.policies for p in c.g.s.players),
    "after adopting []": lambda u, m, c: _pol_or_belief(c, m.p(0)),
    "before adopting []": lambda u, m, c: not _pol_or_belief(c, m.p(0)),
    "before founding a Pantheon": lambda u, m, c: c.civ is not None and _civ(c).religion_state == "none",
    "after founding a Pantheon": lambda u, m, c: c.civ is not None and _civ(c).religion_state != "none",
    "before founding a religion": lambda u, m, c: c.civ is not None and _civ(c).religion_state in ("none", "pantheon", "founding"),
    "after founding a religion": lambda u, m, c: c.civ is not None and _civ(c).religion_state in ("religion", "enhancing", "enhanced"),
    "before enhancing a religion": lambda u, m, c: c.civ is not None and _civ(c).religion_state != "enhanced",
    "after enhancing a religion": lambda u, m, c: c.civ is not None and _civ(c).religion_state == "enhanced",
    "after generating a Great Prophet": lambda u, m, c: c.civ is not None and _civ(c).great_prophets_earned > 0,
    "if [] is constructed": lambda u, m, c: c.civ is not None and _has_building(c.g, c.civ, m.p(0)),
    "if [] is not constructed": lambda u, m, c: c.civ is not None and not _has_building(c.g, c.civ, m.p(0)),
    "if [] is constructed in all [] cities": lambda u, m, c: c.civ is not None and all(
        _city_has(c.g, x, m.p(0)) for x in c.g.player_cities(c.civ) if city_matches(c.g, x, m.p(1))),
    "if [] is constructed in at least [] of [] cities": lambda u, m, c: c.civ is not None and sum(
        1 for x in c.g.player_cities(c.civ) if _city_has(c.g, x, m.p(0)) and city_matches(c.g, x, m.p(2))) >= m.n(1),
    "if [] is constructed by anybody": lambda u, m, c: any(_city_has(c.g, x, m.p(0)) for x in c.g.s.cities.values()),
    "if [] is not constructed by anybody": lambda u, m, c: not any(_city_has(c.g, x, m.p(0)) for x in c.g.s.cities.values()),
    "in this city": lambda u, m, c: c.rel_city is not None,
    "in [] cities": lambda u, m, c: c.rel_city is not None and city_matches(c.g, c.rel_city, m.p(0), c.civ),
    "in cities connected to the capital": lambda u, m, c: c.rel_city is not None and _connected(c),
    "in cities with a []": lambda u, m, c: c.rel_city is not None and _city_has(c.g, c.rel_city, m.p(0)),
    "in cities without a []": lambda u, m, c: c.rel_city is not None and not _city_has(c.g, c.rel_city, m.p(0)),
    "in cities with at least [] []": lambda u, m, c: c.rel_city is not None and
        population_amount(c.g, c.rel_city, m.p(1)) >= m.n(0),
    "in cities with [] []": lambda u, m, c: c.rel_city is not None and population_amount(c.g, c.rel_city, m.p(1)) == m.n(0),
    "in cities with between [] and [] []": lambda u, m, c: c.rel_city is not None and
        m.n(0) <= population_amount(c.g, c.rel_city, m.p(2)) <= m.n(1),
    "in cities with less than [] []": lambda u, m, c: c.rel_city is not None and population_amount(c.g, c.rel_city, m.p(1)) < m.n(0),
    "with a garrison": lambda u, m, c: c.rel_city is not None and _garrisoned(c),
    "vs cities": lambda u, m, c: c.their is not None and c.their.city is not None,
    "vs [] units": lambda u, m, c: _vs_combatant(c, m.p(0)),
    "vs []": lambda u, m, c: _vs_combatant(c, m.p(0)),
    "for [] units": lambda u, m, c: c.rel_unit is not None and unit_matches(c.g, c.rel_unit, m.p(0), c),
    "when [] units": lambda u, m, c: c.rel_unit is not None and unit_matches(c.g, c.rel_unit, m.p(0), c),
    "when []": lambda u, m, c: c.rel_unit is not None and unit_matches(c.g, c.rel_unit, m.p(0), c),
    "for units with []": lambda u, m, c: c.rel_unit is not None and (m.p(0) in c.rel_unit.promotions or m.p(0) in c.rel_unit.status),
    "for units without []": lambda u, m, c: c.rel_unit is not None and not (m.p(0) in c.rel_unit.promotions or m.p(0) in c.rel_unit.status),
    "when attacking": lambda u, m, c: c.action == "attack",
    "when defending": lambda u, m, c: c.action == "defend",
    "when above [] HP": lambda u, m, c: _hp(c) is not None and _hp(c) > m.n(0),
    "when below [] HP": lambda u, m, c: _hp(c) is not None and _hp(c) < m.n(0),
    "if it hasn't used other actions yet": lambda u, m, c: c.unit is None or not c.unit.abilities_used,
    "when stacked with a [] unit": lambda u, m, c: c.rel_unit is not None and any(
        o is not c.rel_unit and unit_matches(c.g, o, m.p(0)) for o in c.g.units_at(c.rel_unit.idx)),
    "when not stacked with a [] unit": lambda u, m, c: c.rel_unit is None or not any(
        o is not c.rel_unit and unit_matches(c.g, o, m.p(0)) for o in c.g.units_at(c.rel_unit.idx)),
    "in [] tiles": lambda u, m, c: c.rel_tile is not None and tile_matches(c.g, c.rel_tile, m.p(0), c.civ),
    "in tiles without []": lambda u, m, c: c.rel_tile is not None and not tile_matches(c.g, c.rel_tile, m.p(0), c.civ),
    "in tiles adjacent to []": lambda u, m, c: c.rel_tile is not None and _adjacent_to(c, m.p(0)),
    "in tiles not adjacent to []": lambda u, m, c: c.rel_tile is not None and not _adjacent_to(c, m.p(0)),
    "when fighting in [] tiles": lambda u, m, c: c.attacked_tile is not None and tile_matches(c.g, c.attacked_tile, m.p(0), c.civ),
    "within [] tiles of a []": lambda u, m, c: c.rel_tile is not None and any(
        tile_matches(c.g, i, m.p(1), c.civ) for i in c.g.grid.within(c.rel_tile, int(m.n(0)))),
    "when fighting units from a Civilization with more Cities than you": lambda u, m, c: c.their is not None and
        len(c.g.player_cities(c.civ)) < len(c.g.player_cities(c.their.owner)),
    "on foreign continents": lambda u, m, c: c.rel_tile is not None and _foreign_continent(c),
    "when adjacent to a [] unit": lambda u, m, c: c.rel_unit is not None and any(
        o.owner == c.civ and o is not c.rel_unit and unit_matches(c.g, o, m.p(0))
        for n in c.g.grid.neighbors(c.rel_unit.idx) for o in c.g.units_at(n)),
    "with [] to [] neighboring [] tiles": lambda u, m, c: c.rel_tile is not None and m.n(0) <= sum(
        1 for n in c.g.grid.neighbors(c.rel_tile) if tile_matches(c.g, n, m.p(2), c.civ)) <= m.n(1),
    "on water maps": lambda u, m, c: False,
    "in [] Regions": lambda u, m, c: c.region == m.p(0),
    "in all except [] Regions": lambda u, m, c: c.region != m.p(0),
    "when number of [] is equal to []": lambda u, m, c: _cmp_countables(c, m.p(0), m.p(1), lambda a, b: a == b),
    "when number of [] is different than []": lambda u, m, c: _cmp_countables(c, m.p(0), m.p(1), lambda a, b: a != b),
    "when number of [] is more than []": lambda u, m, c: _cmp_countables(c, m.p(0), m.p(1), lambda a, b: a > b),
    "when number of [] is less than []": lambda u, m, c: _cmp_countables(c, m.p(0), m.p(1), lambda a, b: a < b),
    "when number of [] is between [] and []": lambda u, m, c: (countable(c, m.p(0)) is not None and
        countable(c, m.p(1)) is not None and countable(c, m.p(2)) is not None and
        countable(c, m.p(1)) <= countable(c, m.p(0)) <= countable(c, m.p(2))),
}

# Modifiers that do not filter (triggers, costs, multipliers, display).
_META = {
    "(modified by game speed)", "by consuming this unit", "after which this unit is consumed", "[] times",
    "[] additional time(s)", "once", "for [] movement", "for [] turns", "upon expending a [] unit",
    "upon discovering [] technology", "upon entering the []", "hidden from users", "Civilopedia link []",
    "Suppress warning []", "upon adopting []", "upon founding a Pantheon", "upon founding a Religion",
    "upon enhancing a Religion", "upon conquering a city", "upon being defeated",
}


def _tech_cond(ctx, f, want: bool) -> bool:
    """Whether the civilization has, or has not, a technology or one matching a filter."""
    g = ctx.g
    techs = g._tech_set(ctx.civ)
    if f in g.rules.techs:
        return (f in techs) == want
    hit = any(tech_matches(g.rules, t, f) for t in techs)
    return hit == want


def _happiness(ctx) -> float:
    """The civilization's happiness, in the form conditionals compare against."""
    from . import economy
    return economy.happiness_for_conditionals(ctx.g, ctx.civ)


def _connected(ctx) -> bool:
    """Whether the city in context has a trade route to the capital."""
    from . import cities as C
    return C.connected_to_capital(ctx.g, ctx.rel_city)


def _garrisoned(ctx) -> bool:
    """Whether the city in context has a military unit in it."""
    from . import cities as C
    return C.is_garrisoned(ctx.g, ctx.rel_city)


def _hp(ctx) -> Optional[int]:
    """The hit points of whatever is in context: the unit, or the city."""
    u = ctx.rel_unit
    if u is not None:
        return u.hp
    if ctx.our is not None:
        return ctx.our.hp(ctx.g)
    return None


def _adjacent_to(ctx, f) -> bool:
    """Whether the tile in context is next to something matching a filter."""
    g = ctx.g
    idx = ctx.rel_tile
    if f == "River":
        return g.s.tiles[idx].river
    if f in ("Fresh water", "Fresh Water") and g.s.tiles[idx].river:
        return True
    return any(tile_matches(g, n, f, ctx.civ) for n in g.grid.neighbors(idx))


def _foreign_continent(ctx) -> bool:
    """Whether the tile in context is on a different landmass from the capital."""
    g = ctx.g
    p = g.player(ctx.civ)
    cap = g.city(p.capital) if p.capital is not None else None
    if cap is None:
        return True
    return g.continent(cap.idx) != g.continent(ctx.rel_tile)


def multiplier(u: Unique, ctx: Optional[Ctx]) -> int:
    """'for every [n] [countable]' style multipliers (not used by the G&K ruleset, kept for completeness)."""
    return 1
