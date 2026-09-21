"""The standard rule-based player: the sparring partner for AI models.

It plays exclusively through the public tool registry (like any AI would), so it also exercises the API. Each turn it
builds a picture of the empire (threats to each city, army size, economy, expansion room) and then:

  * researches toward the most valuable technology for its situation (beeline scoring over prerequisite paths),
  * adopts social policies, founds a pantheon and religion, uses great people and free picks,
  * bombards enemies with its cities, fights with favorable odds, answers threats and clears barbarian camps,
  * keeps settlers and workers out of danger, expands while happiness allows,
  * chooses production by need; buildings are valued like UnCiv's ConstructionAutomation, by simulating the city's
    stats with the building added,
  * keeps gold positive, buys what's urgent, upgrades units, courts city-states, sends spies,
  * wages wars it can win, makes peace when a war goes badly, and trades surplus luxuries.

`aggression` (0-1) shifts army size and willingness to start wars.
"""
from __future__ import annotations

import random
from typing import Optional

from ..engine import tools
from ..engine import unique_types as U
from ..engine.game import Game, ActionError

STAT_WEIGHTS = {"food": 1.2, "production": 1.3, "gold": 0.9, "science": 1.1, "culture": 0.8, "faith": 0.6,
                "happiness": 1.8}
POLICY_ORDER = {
    "peaceful": ["Tradition", "Liberty", "Piety", "Patronage", "Rationalism", "Commerce", "Freedom", "Order", "Honor",
                 "Autocracy"],
    "aggressive": ["Honor", "Tradition", "Liberty", "Commerce", "Rationalism", "Autocracy", "Patronage", "Piety",
                   "Order", "Freedom"],
}
BELIEF_PREFS = {       # belief_mode "prefs": ranked choices (happiness and growth first); unlisted ones come last
    "Pantheon": ["Goddess of Love", "Fertility Rites", "Religious Settlements", "God of Craftsman", "Sacred Waters",
                 "Ancestor Worship", "Monument to the Gods", "Goddess of the Hunt", "Stone Circles", "Oral Tradition"],
    "Founder": ["Ceremonial Burial", "Tithe", "Church Property", "Peace Loving", "Pilgrimage", "Initiation Rites",
                "Interfaith Dialogue", "World Church", "Papal Primacy"],
    "Follower": ["Pagodas", "Religious Center", "Feed the World", "Mosques", "Cathedrals", "Religious Community",
                 "Swords into Ploughshares", "Peace Gardens", "Divine inspiration", "Guruship", "Asceticism",
                 "Choral Music", "Liturgical Drama", "Monasteries", "Religious Art", "Holy Warriors"],
    "Enhancer": ["Religious Texts", "Itinerant Preachers", "Missionary Zeal", "Reliquary", "Messiah", "Holy Order",
                 "Just War", "Religious Unity", "Defender of the Faith"],
}
POLICY_ORDERS = {      # named alternatives for experiments (params policy_order_peaceful / _aggressive)
    "default": None,
    "rationalism_early": ["Tradition", "Liberty", "Rationalism", "Patronage", "Commerce", "Piety", "Freedom", "Order",
                          "Honor", "Autocracy"],
    "liberty_first": ["Liberty", "Tradition", "Rationalism", "Commerce", "Patronage", "Piety", "Freedom", "Order",
                      "Honor", "Autocracy"],
}
PANTHEON_PREFS = ["Fertility Rites", "God of Craftsman", "Religious Settlements", "Tradition", "Ancestor Worship",
                  "Goddess of Love", "Monument to the Gods", "God of War", "Messenger of the Gods"]


# Tunable knobs. Lab experiments override them per seat (BasicBot(params={...})) to A/B test settings without
# editing code; keep the defaults at the best values found (see docs/BOT_TUNING.md).
DEFAULT_PARAMS: dict = {    # v1 defaults (2026-09-18): ab2, fac1, fac2 — see docs/BOT_TUNING.md
    # building valuation: weight per point of each stat the building adds (simulated in the city)
    "w_food": 1.2, "w_production": 1.3, "w_gold": 0.9, "w_science": 1.1, "w_culture": 0.8, "w_faith": 0.6,
    "w_happiness": 1.8,
    "unique_bonus": 0.5,          # per rule text the simulation can't see
    "wonder_mult": 1.2, "wonder_bonus": 2.0,
    "wonder_min_pop": 0,          # UnCiv: wonders only when the empire has 12+ population ...
    "wonder_avg_prod": False,     # ... and only in cities producing at least the empire average
    # expansion
    "settler_min_hap": 2,         # build settlers while happiness >= this ...
    "settler_min_hap_small": 0,   # ... or >= this with fewer than 3 cities
    "settler_prio": 130, "settler_prio_late": 75, "settler_prio_per_city": 6,
    # "classic" = fixed priorities; "unciv" = UnCiv ConstructionAutomation: best value per remaining production
    "prod_mode": "unciv",
    "u_food": 3.6, "u_production": 2.0, "u_gold": 0.67, "u_science": 2.0, "u_culture": 1.0, "u_faith": 1.0,
    "u_happiness": 1.0, "u_happiness_low": 6.0,     # x3 while happiness < 10 or < number of cities (UnCiv)
    "u_settler": 30.0, "u_worker": 1.0, "u_military": 1.0, "u_offense": 3.0, "u_wonder_bonus": 4.0,
    "u_wonder_gate": True,        # UnCiv: wonders only in above-average-production cities of a 12+ population empire
    "lux_trade_every": 3,         # turns between luxury-trade offers
    # research: "classic" = flat building stats; "potential" = estimated empire-wide yield of what the tech unlocks
    "tech_mode": "classic", "tech_cost_exp": 0.8,
    "policy_order_peaceful": "rationalism_early", "policy_order_aggressive": None,    # None = POLICY_ORDER
    "site_new_lux": 0,            # city-site bonus per luxury type we don't own yet
    "war_long_turns": 25,         # after this many turns a war counts as long (the bot then looks for peace)
    "siege_city_pref": 3.0,       # attack-value multiplier for the besieged city
    "siege_move_first": True,    # ranged units in a siege move into range/sight of the city before shooting
    "cs_gift_mode": "classic",    # "typed": gift the city-state type we need where a gift crosses a threshold
    "small_city_focus": None,     # e.g. "food": non-capital cities below small_city_pop grow first
    "small_city_pop": 8,
    "workers_per_city": 1.8,      # UnCiv's target (unciv production mode)
    "worker_unimproved": 0.0,     # extra wanted workers per worked, unimproved land tile
    "bv_cache_turns": 5,          # reuse a city's building values for up to this many turns (0 = always simulate)
    "prep_gather": False,         # gather the field army at a rally point before declaring war, then advance at once
    "unhappy_avoid_growth": False,  # while unhappy, the biggest quarter of cities (size 6+) stop growing
    "war_prep_rate": 1.0,         # multiplier on the per-turn chance of starting war preparations
    "belief_mode": "prefs",       # "prefs": ranked belief choices (BELIEF_PREFS) instead of UnCiv AI weights
    "faith_buildings": True,     # buy faith-only buildings (Pagoda, Mosque, Cathedral) before missionaries
}


def cm_tiles(g, c) -> list:
    """The tiles a city owns."""
    from ..engine.cities import city_tiles
    return city_tiles(g, c)


def _ud(g, u) -> dict:
    """A unit's ruleset definition."""
    return g.rules.units[u.type]


def _is_recon(ud) -> bool:
    """Whether a unit type is a scout."""
    return ud.get("unitType") == "Scout"


def _power(ud) -> float:
    """A unit type's combat weight, the larger of its melee and ranged strength."""
    return max(ud.get("strength", 0), ud.get("rangedStrength", 0))


class BasicBot:
    """The scripted opponent, and the yardstick every model score is measured against.

    A few thousand lines of heuristics with no learning in them, which is the point: it can be read,
    argued with and changed deliberately, and a benchmark denominated in it means something you can
    inspect.

    It plays the whole game - research by need, buildings chosen by simulating the city with each
    candidate, policies, religion, great people, espionage, city-states, war with siege units and rally
    points. ``aggression`` scales army size and willingness to start a war.

    Every parameter it uses lives in ``DEFAULT_PARAMS`` and can be overridden per seat, which is what
    the lab's factorial experiments vary. Its known ceiling is happiness: it stops expanding at two to
    five cities on a small map, and raising that is the open balance work.
    """
    def __init__(self, aggression: float = 0.4, seed: Optional[int] = None, target_cities: int = 0,
                 params: Optional[dict] = None):
        self.aggression = max(0.0, min(1.0, aggression))
        self.p = dict(DEFAULT_PARAMS)
        self.p.update(params or {})
        self.rng = random.Random(seed)
        self.target_cities = target_cities          # 0 = expand while there is room
        self._sites_cache: dict = {}
        self._bad_sites: dict = {}                  # (pid, idx) -> turn a settler failed to reach it
        self._boat_turn: dict = {}                  # city id -> turn a work boat was last queued
        self._war_plan: dict = {}                   # pid -> {"city": idx, "rally": idx, "advance": bool, ...}
        self._war_prep: dict = {}                   # pid -> {"player": q, "since": turn}
        self._garrisons: dict = {}
        self._escorts: dict = {}                    # settler id -> escorting military unit id
        self._retreats: dict = {}                   # (pid, site) -> times a settler turned back from it
        self._need_escort: dict = {}                # pid -> city idx where a settler waits for an escort
        self._bv_cache: dict = {}                   # (city id, building) -> (turn, signature, value)

    # ------------------------------------------------------------------
    def ex(self, g: Game, pid: int, _tool: str, **args):
        """Call a game tool, swallowing refusals.

        The bot proposes constantly and is refused often - a unit that cannot reach, a building that is no
        longer available - and treating every refusal as an error would mean checking each precondition
        twice. What it must never do is crash a game, so failures are counted and ignored.
        """
        try:
            return tools.execute(g, pid, _tool, args)
        except ActionError:
            return None

    def play_turn(self, g: Game, pid: int, end_turn: bool = True):
        """Play one complete turn."""
        if g.s.phase != "playing" or g.s.current != pid:
            return
        self.handle_negotiations(g, pid)
        ctx = self.context(g, pid)
        self.choose_research(g, pid, ctx)
        self.empire_choices(g, pid, ctx)
        self.city_bombard(g, pid)
        self.manage_units(g, pid, ctx)
        self.city_bombard(g, pid)
        ctx = self.context(g, pid)
        self.manage_cities(g, pid, ctx)
        self.manage_gold(g, pid, ctx)
        self.consider_diplomacy(g, pid, ctx)
        if end_turn and g.s.current == pid and g.s.phase == "playing":
            tools.execute(g, pid, "end_turn", {})

    # ------------------------------------------------------------------
    # situation
    # ------------------------------------------------------------------
    def context(self, g: Game, pid: int) -> dict:
        """Gather everything the turn's decisions need, once.

        The bot consults the same facts repeatedly - happiness, threats, army size, expansion sites - and
        computing them per decision was measurably slower than computing them per turn.
        """
        from ..engine import economy, research, visibility
        p = g.player(pid)
        cities = g.player_cities(pid)
        units = g.player_units(pid)
        vis = visibility.visible_tiles(g, pid)
        hostile = [u for u in g.s.units.values() if u.idx in vis and g.at_war(pid, u.owner) and _ud(g, u)["_military"]
                   and visibility.unit_visible_to(g, pid, u)]
        threat, near_enemies = {}, {}
        for c in cities:
            t, near = 0.0, []
            for u in hostile:
                d = g.grid.distance(u.idx, c.idx)
                if d <= 4:
                    t += _power(_ud(g, u)) * u.hp / 100 * (1.0 if d <= 2 else 0.6)
                    near.append(u)
            threat[c.id] = t
            near_enemies[c.id] = near
        military = [u for u in units if _ud(g, u)["_military"] and not _is_recon(_ud(g, u))]
        st = economy.civ_stats(g, pid)
        wars = [q for q in p.met if g.player(q).alive and g.at_war(pid, q) and g.player(q).kind == "major"]
        preparing = pid in self._war_prep
        army_target = int(len(cities) * (1.0 + 0.6 * self.aggression) + 1 + (len(cities) + 3 if wars or preparing else 0)
                          + sum(1 for c in cities if threat[c.id] > 0))
        supply = economy.unit_supply(g, pid)
        from ..engine import tiles as T
        hap = economy.happiness(g, pid)
        lux_owned = set(hap.get("luxury_types", []))
        pending_res = set()           # resources in our borders that lack their improvement
        for c in cities:
            for idx in cm_tiles(g, c):
                t = g.s.tiles[idx]
                if t.resource and T.resource_visible(g, pid, t.resource)                         and not T.resource_improved_by(g, t.resource, t.improvement):
                    pending_res.add(t.resource)
        return {"lux_owned": lux_owned, "pending_res": pending_res,
                "cities": cities, "units": units, "military": military, "hostile": hostile, "threat": threat,
                "near_enemies": near_enemies, "gpt": st["gold"], "hap": hap["total"],
                "era": research.player_era(g, pid), "wars": wars, "gold": p.gold, "supply": supply,
                "army_target": army_target, "vis": vis, "offense": bool(wars) or preparing}

    def city_defense(self, g: Game, city) -> float:
        """How well defended a city is, for deciding whether it needs another unit."""
        from ..engine import combat, cities as cm
        s = combat.city_strength(g, city) * city.health / max(1, cm.max_health(g, city))
        for idx in g.grid.within(city.idx, 1):
            m = g.military_at(idx)
            if m and m.owner == city.owner:
                s += _power(_ud(g, m)) * m.hp / 100
        return s

    # ------------------------------------------------------------------
    # research
    # ------------------------------------------------------------------
    def choose_research(self, g: Game, pid: int, ctx: Optional[dict] = None):
        """Pick what to research, beelining whatever is most valuable now."""
        from ..engine import research
        p = g.player(pid)
        if p.free_techs > 0:
            avail = research.available_techs(g, pid)
            if avail:
                self.ex(g, pid, "choose_free_tech",
                        tech=max(avail, key=lambda t: research.tech_cost(g, pid, t)))
        if research.current(g, pid):
            return
        ctx = ctx or self.context(g, pid)
        avail = research.available_techs(g, pid)
        if not avail:
            return
        best, best_v = None, -1.0
        memo: dict = {}
        for t in g.rules.techs:
            if g.has_tech(pid, t) or research.is_unresearchable(g, pid, t):
                continue
            v = self._tech_value(g, pid, t, ctx, memo)
            if v <= 0:
                continue
            path = research.path_to(g, pid, t)
            if not path:
                continue
            cost = sum(research.tech_cost(g, pid, x) for x in path)
            v += 0.5 * sum(self._tech_value(g, pid, x, ctx, memo) for x in path[:-1])
            score = v / (cost ** self.p["tech_cost_exp"]) * self.rng.uniform(0.9, 1.1)
            if score > best_v:
                best, best_v = path[0], score
        self.ex(g, pid, "set_research", tech=best or min(avail, key=lambda t: research.tech_cost(g, pid, t)))

    def _tech_value(self, g: Game, pid: int, tech: str, ctx: dict, memo: Optional[dict] = None) -> float:
        """The value of a technology, memoised across one decision."""
        memo = {} if memo is None else memo
        if tech in memo:
            return memo[tech]
        v = memo[tech] = self._tech_value_uncached(g, pid, tech, ctx, memo)
        return v

    def _building_potential(self, g: Game, pid: int, b: str, ctx: dict, memo: dict) -> float:
        """Rough empire-wide value of having building b available: its flat stats, its percentage bonuses applied to
        the empire's current yields, and per-population rules, in every city (or once for a wonder)."""
        import re
        R, P = g.rules, self.p
        bd = R.buildings[b]
        key = "__empire"
        emp = memo.get(key)
        if emp is None:
            from ..engine import economy
            st = economy.civ_stats(g, pid)
            cities = ctx["cities"] or []
            emp = memo[key] = {"n": max(1, len(cities)), "pop": sum(c.pop for c in cities) / max(1, len(cities)),
                               **{k: max(0.0, st.get(k, 0)) for k in ("food", "production", "gold", "science", "culture", "faith")}}
        w = {k: P["u_" + k] for k in ("food", "production", "gold", "science", "culture", "faith", "happiness")}
        per_city = sum(bd.get(k, 0) * w[k] for k in w)
        per_city -= bd.get("maintenance", 0) * w["gold"]
        for k, pct in (bd.get("percentStatBonus") or {}).items():
            if k in w:
                per_city += pct / 100 * emp.get(k, 0) / emp["n"] * w[k]
        for text in bd.get("uniques", []):
            m = re.match(r"\[\+(\d+) (\w+)\] per \[(\d+)\] population", text)
            if m and m.group(2).lower() in w:
                per_city += int(m.group(1)) * emp["pop"] / int(m.group(3)) * w[m.group(2).lower()]
        per_city += sum((bd.get("specialistSlots") or {}).values()) * 0.5
        mult = 1 if (bd.get("isWonder") or bd.get("isNationalWonder")) else emp["n"]
        return 2 + max(0.0, per_city) * mult

    def _tech_value_uncached(self, g: Game, pid: int, tech: str, ctx: dict, memo: dict) -> float:
        """Score a technology by what it unlocks and what the civilization currently needs.

        Includes luxuries it cannot yet improve, which is deliberate: the happiness ceiling makes the
        technology that lifts it worth more than its immediate yield suggests.
        """
        R = g.rules
        un = R.unlocks.get(tech, {})
        v = 1.0
        for b in un.get("buildings", []):
            bd = R.buildings[b]
            if bd.get("uniqueTo") and bd["uniqueTo"] != g.player(pid).nation:
                continue
            if self.p["tech_mode"] == "potential":
                v += self._building_potential(g, pid, b, ctx, memo)
                continue
            v += 3 + sum(bd.get(k, 0) * w for k, w in STAT_WEIGHTS.items()) + len(bd.get("uniques", []))
            if bd.get("isWonder"):
                v += 2
        best_now = memo.get("__best_now")
        if best_now is None:
            best_now = memo["__best_now"] = max(
                (_power(ud) for n, ud in R.units.items() if ud["_military"] and ud["_domain"] == "Land"
                 and g.has_tech(pid, ud.get("requiredTech"))), default=8)
        for n in un.get("units", []):
            ud = R.units[n]
            if ud.get("uniqueTo") and ud["uniqueTo"] != g.player(pid).nation:
                continue
            if not ud["_military"]:
                v += 3
                continue
            gain = _power(ud) / max(1, best_now)
            if gain > 1.05:
                v += (4 + 8 * (gain - 1)) * (2.2 if ctx["wars"] else 1.0) * (0.6 + self.aggression)
        v += 3 * len(un.get("improvements", [])) + 3 * len(un.get("reveals", []))
        from ..engine import tiles as T
        for res in ctx.get("pending_res", ()):
            rt = R.resources[res]["resourceType"]
            if any(T.resource_improved_by(g, res, imp) for imp in un.get("improvements", [])):
                if rt == "Luxury" and res not in ctx["lux_owned"]:
                    v += 14 if ctx["hap"] < len(ctx["cities"]) + 3 else 7
                elif rt == "Strategic":
                    v += 3
        v += 2 * len(R.techs[tech].get("uniques", []))
        if ctx["era"] >= 5 and any(R.units[n]["_umap"].get(U.SpaceshipPart) or R.units[n]["_umap"].get(U.AddInCapital)
                                   for n in un.get("units", [])):
            v += 25
        return v

    # ------------------------------------------------------------------
    # empire-wide choices: policies, religion, great people, spies
    # ------------------------------------------------------------------
    def empire_choices(self, g: Game, pid: int, ctx: dict):
        """Spend accumulated culture and faith: policies, beliefs, great people."""
        from ..engine import policies, religion
        p = g.player(pid)
        order = self.p.get("policy_order_aggressive" if self.aggression > 0.55 else "policy_order_peaceful")
        if isinstance(order, str):
            order = POLICY_ORDERS.get(order)
        order = order or POLICY_ORDER["aggressive" if self.aggression > 0.55 else "peaceful"]
        for _ in range(4):
            if not policies.can_adopt_any(g, pid):
                break
            options = policies.adoptable_policies(g, pid)
            if not options:
                break

            def rank(name):
                """Score one policy or branch."""
                br = policies.branch_of(g, name) if not policies.is_branch(g, name) else name
                base = order.index(br) if br in order else 50
                return (0 if br in p.policies else 1, base, name)   # finish open branches first
            if self.ex(g, pid, "adopt_policy", policy=min(options, key=rank)) is None:
                break
        if p.free_great_people > 0:
            gp = "Great Scientist" if ctx["era"] < 4 else "Great Engineer"
            self.ex(g, pid, "choose_great_person", great_person=gp)
        if g.religion_enabled and religion.can_found_pantheon(g, pid) is None:
            avail = religion.beliefs_available(g, "Pantheon")
            prefs = BELIEF_PREFS["Pantheon"] if self.p["belief_mode"] == "prefs" else PANTHEON_PREFS
            pick = next((b for b in prefs if b in avail), avail[0] if avail else None)
            if pick:
                self.ex(g, pid, "found_pantheon", belief=pick)
        if g.espionage_enabled:
            self._spies(g, pid, ctx)

    def _spies(self, g: Game, pid: int, ctx: dict):
        """Send spies somewhere useful, and stage a coup when it looks winnable."""
        p = g.player(pid)
        taken = {s.get("city") for s in p.spies}
        for s in p.spies:
            if s["action"] != "None":
                continue
            targets = []
            for q in g.majors():
                if q.id == pid or not g.has_met(pid, q.id):
                    continue
                cap = g.city(q.capital) if q.capital is not None else None
                if cap and p.explored[cap.idx] and cap.id not in taken:
                    lead = len(q.techs) - len(p.techs)
                    targets.append((lead + self.rng.random(), cap.id))
            if targets:
                _, cid = max(targets)
            else:
                cap = g.city(p.capital) if p.capital is not None else None
                cid = cap.id if cap is not None and cap.id not in taken else None
            if cid is not None and self.ex(g, pid, "move_spy", spy=s["name"], city_id=cid) is not None:
                taken.add(cid)

    # ------------------------------------------------------------------
    # cities
    # ------------------------------------------------------------------
    def expansion_sites(self, g: Game, pid: int, ctx: dict) -> list[int]:
        """Where this civilization should put its next cities, best first."""
        from ..engine.automation import city_site_score
        from ..engine import tiles as T
        from ..engine import cities as cm
        cached = self._sites_cache.get(pid)
        if cached and g.turn - cached[0] < 4:
            return [i for i in cached[1] if cm.found_check(g, pid, i) is None]
        cities = ctx["cities"]
        centers = [c.idx for c in cities] or [u.idx for u in ctx["units"] if _ud(g, u)["_umap"].get(U.FoundCity)]
        cont = g.s.continents
        reachable = {cont[c] for c in centers}
        seen, scored = set(), []
        for center in centers:
            for idx in g.grid.within(center, 9):
                if idx in seen:
                    continue
                seen.add(idx)
                if g.is_water(idx) or g.grid.distance(center, idx) < 4 or cont[idx] not in reachable:
                    continue
                if g.turn - self._bad_sites.get((pid, idx), -99) < 30:
                    continue
                s = city_site_score(g, pid, idx)
                if s is None or s < 14:
                    continue
                if self.p["site_new_lux"]:
                    # a luxury type we don't have yet is +4 happiness empire-wide: the resource expansion needs
                    new = {g.s.tiles[n].resource for n in g.grid.within(idx, 2) if g.s.tiles[n].resource
                           and g.s.tiles[n].owner in (None, pid) and T.resource_visible(g, pid, g.s.tiles[n].resource)
                           and g.rules.resources[g.s.tiles[n].resource]["resourceType"] == "Luxury"} - ctx.get("lux_owned", set())
                    s += self.p["site_new_lux"] * len(new)
                d = min(g.grid.distance(c, idx) for c in centers)
                scored.append((s - d * 1.5, idx))
        scored.sort(reverse=True)
        picked = []
        for s, idx in scored:
            if all(g.grid.distance(idx, o) >= 4 for o in picked):
                picked.append(idx)
            if len(picked) >= 6:
                break
        self._sites_cache[pid] = (g.turn, picked)
        return picked

    def _counts(self, g: Game, ctx: dict) -> tuple[dict, int]:
        """What the civilization already has, by unit kind, and its total army strength."""
        R = g.rules
        counts = {"settler": 0, "worker": 0, "recon": 0, "boat": 0}

        def kind(name):
            """Classify a unit type for counting."""
            ud = R.units.get(name)
            if not ud:
                return None
            if ud["_umap"].get(U.FoundCity):
                return "settler"
            if ud["_umap"].get(U.BuildImprovements):
                return "worker"
            if ud["_umap"].get(U.CreateWaterImprovements):
                return "boat"
            if _is_recon(ud):
                return "recon"
            return "army" if ud["_military"] else None
        army = len(ctx["military"])
        for u in ctx["units"]:
            k = kind(u.type)
            if k in counts:
                counts[k] += 1
        for c in ctx["cities"]:
            for q in c.queue:
                k = kind(q)
                if k in counts:
                    counts[k] += 1
                elif k == "army":
                    army += 1
        return counts, army

    def advise_production(self, g: Game, pid: int, city) -> Optional[str]:
        """What this bot would build next in one city (used for human players' 'auto production' cities)."""
        ctx = self.context(g, pid)
        counts, army = self._counts(g, ctx)
        danger = ctx["threat"].get(city.id, 0) > self.city_defense(g, city) * 0.5
        return self._choose_production(g, pid, city, ctx, counts, army, danger)

    def manage_cities(self, g: Game, pid: int, ctx: Optional[dict] = None):
        """Run every city: citizens, production and purchases."""
        ctx = ctx or self.context(g, pid)
        counts, army = self._counts(g, ctx)
        for c in sorted(ctx["cities"], key=lambda c: -ctx["threat"][c.id]):
            if c.puppet:
                continue
            danger = ctx["threat"][c.id] > self.city_defense(g, c) * 0.5
            if c.queue:
                head = c.queue[0]
                is_military = head in g.rules.units and g.rules.units[head]["_military"]
                if not (danger and not is_military):
                    continue
            choice = self._choose_production(g, pid, c, ctx, counts, army, danger)
            if choice and self.ex(g, pid, "set_production", city_id=c.id, item=choice) is not None:
                ud = g.rules.units.get(choice)
                if ud:
                    if ud["_umap"].get(U.FoundCity):
                        counts["settler"] += 1
                    elif ud["_umap"].get(U.BuildImprovements):
                        counts["worker"] += 1
                    elif ud["_umap"].get(U.CreateWaterImprovements):
                        counts["boat"] += 1
                        self._boat_turn[c.id] = g.turn
                    elif _is_recon(ud):
                        counts["recon"] += 1
                    elif ud["_military"]:
                        army += 1
            self._manage_city_tiles(g, pid, c, ctx)

    def _manage_city_tiles(self, g: Game, pid: int, c, ctx: dict):
        """Set a city's focus and citizen assignment for its situation."""
        from ..engine import cities as cm
        if c.razing or c.puppet:
            return
        if self.p["unhappy_avoid_growth"]:
            # while unhappy (-75% growth everywhere), stop the biggest cities from growing so the rest grow normally
            big = sorted(ctx["cities"], key=lambda x: -x.pop)[:max(1, len(ctx["cities"]) // 4)]
            stop = ctx["hap"] < 0 and c in big and c.pop >= 6
            if stop != bool(c.avoid_growth) and (stop or ctx["hap"] >= 2):
                self.ex(g, pid, "set_city_focus", city_id=c.id, avoid_growth=stop)
        want = "balanced"
        small = self.p["small_city_focus"]
        small = None if small == "none" else small
        if small and c.pop < self.p["small_city_pop"] and not cm.is_capital(g, c):
            want = small                     # small cities grow first
        if c.focus == "gold":
            return                           # set and cleared by manage_gold during deficits
        if c.focus != want:
            self.ex(g, pid, "set_city_focus", city_id=c.id, focus=want)

    def _choose_production(self, g: Game, pid: int, c, ctx: dict, counts: dict, army: int, danger: bool) -> Optional[str]:
        """Choose what a city builds, by whichever production heuristic is configured."""
        if self.p.get("prod_mode") == "unciv":
            return self._choose_production_unciv(g, pid, c, ctx, counts, army, danger)
        return self._choose_production_classic(g, pid, c, ctx, counts, army, danger)

    # ------------------------------------------------------------------
    # UnCiv-style production (ConstructionAutomation): every option gets a value ("choice modifier"); the city
    # builds the best value per point of remaining production. Emergencies, settlers and scouting are overrides.
    # ------------------------------------------------------------------
    def _choose_production_unciv(self, g: Game, pid: int, c, ctx: dict, counts: dict, army: int, danger: bool):
        """Choose production the way UnCiv's AI does: weighted preferences over everything buildable.

        The default, because it was measurably better than the older ranking - it considers the whole
        buildable set with modifiers rather than walking a priority list.
        """
        from ..engine import cities as cm
        from math import sqrt
        R, P = g.rules, self.p
        cities = ctx["cities"]
        n = len(cities)
        items = cm.buildable_items(g, c)
        units = set(items["units"])
        prods = {x.id: max(1.0, cm.city_stats(g, x)["total"]["production"]) for x in cities if not x.puppet}
        prod = prods.get(c.id, 1.0)
        over_avg = prod >= sum(prods.values()) / max(1, len(prods))
        total_pop = sum(x.pop for x in cities)
        defender = self.best_military(g, c, units, prefer_ranged=True)
        # --- overrides ---------------------------------------------------------------------------------
        if defender and danger:
            return defender
        if defender and g.military_at(c.idx) is None and (g.turn > 12 or ctx["hostile"]):
            return defender
        if defender and self._need_escort.get(pid) == c.idx:
            self._need_escort.pop(pid, None)
            return defender
        settler = next((u for u in units if R.units[u]["_umap"].get(U.FoundCity)), None)
        sites = self.expansion_sites(g, pid, ctx)
        choices: list[tuple[float, str]] = []

        def add(name, modifier):
            """Add a candidate with its weight."""
            if name and modifier > 0:
                choices.append((modifier / max(1.0, cm.remaining_work(g, c, name)), name))
        if settler and any(g.grid.distance(s, c.idx) <= 12 for s in sites) and n + counts["settler"] < (self.target_cities or 99) \
                and counts["settler"] < max(1, n // 3) and c.pop >= 2 \
                and (ctx["hap"] >= P["settler_min_hap"] or (ctx["hap"] >= P["settler_min_hap_small"] and n < 3)):
            add(settler, P["u_settler"])
        met = self._knows_rival_city(g, pid)
        scout = next((u for u in units if _is_recon(R.units[u])), None)
        if scout and counts["recon"] == 0 and not met and g.turn < 150:
            add(scout, 4.0)
        if not met and g.turn > 30 and counts["recon"] == 0 and army <= len(ctx["military"]) \
                and not any(x.activity == "explore" for x in ctx["units"]):
            add(defender if (g.turn > 45 or not scout) else scout, 6.0)
        # --- workers (UnCiv addWorkerChoice) -----------------------------------------------------------
        worker = next((u for u in units if R.units[u]["_umap"].get(U.BuildImprovements)), None)
        if worker:
            wpc = P["workers_per_city"]
            want = 1 if n <= 1 else (wpc * n if n <= 5 else wpc * 5 + wpc * 0.72 * (n - 5))
            unimproved = sum(1 for x in cities for i in x.worked if not g.s.tiles[i].improvement
                             and not g.is_water(i))
            want += P["worker_unimproved"] * unimproved
            if counts["worker"] < want:
                add(worker, P["u_worker"] * want / (counts["worker"] + 0.17))
        # --- work boats -------------------------------------------------------------------------------------
        boat = next((u for u in units if R.units[u]["_umap"].get(U.CreateWaterImprovements)), None)
        if boat and counts["boat"] == 0:
            from ..engine import tiles as T
            if any(g.s.tiles[i].owner == pid and g.s.tiles[i].resource and g.is_water(i) and not g.s.tiles[i].improvement
                   and T.resource_visible(g, pid, g.s.tiles[i].resource) for i in g.grid.within(c.idx, 6)):
                add(boat, 6.0)
        # --- military (UnCiv addMilitaryUnitChoice, plus war preparation) ------------------------------
        at_war = bool(ctx["wars"])
        mil = len(ctx["military"])
        if defender and (at_war or ctx["offense"] or over_avg) and len(ctx["units"]) < ctx["supply"] \
                and (at_war or ctx["offense"] or (ctx["gpt"] >= 0 and mil <= max(7, n * 5))) and ctx["gold"] > -50:
            modifier = 1 + sqrt(n / (mil + 1)) / 2
            if at_war:
                modifier *= 2
            if ctx["offense"] and army < ctx["army_target"]:
                modifier *= P["u_offense"] * (0.5 + self.aggression)
            if ctx["hostile"] and any(g.is_barbarian(e.owner) for e in ctx["hostile"]):
                modifier = max(modifier, 2.0)
            if not over_avg:
                modifier /= 5
            if army >= ctx["army_target"] and not at_war:
                modifier /= 3
            siege = sum(1 for u in ctx["military"] if _ud(g, u).get("unitType") == "Siege")
            want_siege = ctx["offense"] and siege * 3 < mil - n + 1
            field_melee = sum(1 for m in ctx["military"] if not _ud(g, m)["_ranged"] and not self._is_garrison(m))
            want_melee = ctx["offense"] and field_melee < 2
            unit = (self.best_military(g, c, units, offense=True, melee_only=True) if want_melee else None) \
                or (self.best_military(g, c, units, siege_only=True) if want_siege else None) \
                or self.best_military(g, c, units, prefer_ranged=self.rng.random() < 0.3, offense=ctx["offense"]) \
                or defender
            add(unit, P["u_military"] * modifier)
        # --- buildings ------------------------------------------------------------------------------------
        wonders = items["wonders"] if (not c.puppet and (not P["u_wonder_gate"] or (over_avg and total_pop >= 12))) else []
        for b in items["buildings"] + wonders:
            add(b, self._building_value_unciv(g, pid, c, b, ctx, over_avg))
        for name in items.get("other", []):
            if name not in cm.PERPETUAL and R.buildings.get(name) is not None:
                add(name, 40.0 if over_avg else 10.0)          # projects: Apollo, Manhattan, Utopia...
        for name in units:
            ud = R.units[name]
            if (ud["_umap"].get(U.AddInCapital) or ud["_umap"].get(U.SpaceshipPart)) and over_avg:
                add(name, 20.0)
        if not choices:
            for perp in ("Science", "Gold"):
                if perp in items.get("other", []):
                    return perp
            return defender
        return max(choices)[1]

    def _building_value_unciv(self, g: Game, pid: int, c, b: str, ctx: dict, over_avg: bool) -> float:
        """Cached _building_value_unciv_raw: values change slowly, and simulating every candidate building in every
        city each time a queue empties was a large share of bot time."""
        k = self.p["bv_cache_turns"]
        if not k:
            return self._building_value_unciv_raw(g, pid, c, b, ctx, over_avg)
        sig = (c.pop, len(c.buildings), ctx["hap"] < 0, ctx["hap"] < len(ctx["cities"]), bool(ctx["wars"]), over_avg,
               ctx["threat"].get(c.id, 0) > 0)
        hit = self._bv_cache.get((c.id, b))
        if hit is not None and hit[1] == sig and g.turn - hit[0] < k:
            return hit[2]
        v = self._building_value_unciv_raw(g, pid, c, b, ctx, over_avg)
        self._bv_cache[(c.id, b)] = (g.turn, sig, v)
        return v

    def _building_value_unciv_raw(self, g: Game, pid: int, c, b: str, ctx: dict, over_avg: bool) -> float:
        """UnCiv getValueOfBuilding: weighted stat difference + military, victory and one-time bonuses."""
        from ..engine import cities as cm
        R, P = g.rules, self.p
        bd = R.buildings[b]
        base = cm.city_stats(g, c)["total"]
        base_hap = sum(cm.city_happiness(g, c).values())
        try:
            after, hap = self._simulate(g, c, b)
        except Exception:
            return 0.0
        d = {k: after.get(k, 0) - base.get(k, 0) for k in ("food", "production", "gold", "science", "culture", "faith")}
        gold_w = P["u_gold"] * (3 if ctx["gold"] < 0 and ctx["gpt"] <= 0 else 1)
        cult_w = P["u_culture"] * (2 if base.get("culture", 0) < 2 else 1)
        hap_w = P["u_happiness"] * (P["u_happiness_low"] if ctx["hap"] < 10 or ctx["hap"] < len(ctx["cities"]) else 1)
        v = (d["food"] * P["u_food"] + d["production"] * P["u_production"] + d["gold"] * gold_w
             + d["science"] * P["u_science"] + d["culture"] * cult_w + d["faith"] * P["u_faith"] + (hap - base_hap) * hap_w)
        # carry-over food (Aqueduct/Granary-style): the food it saves per turn
        for u in bd["_umap"].get(U.CarryOverFood) if hasattr(U, "CarryOverFood") else ():
            v += max(0.0, base.get("food", 0)) * u.n(0) / 100 * P["u_food"] * 0.5
        v += sum(bd.get("greatPersonPoints", {}).values()) * 0.5
        v += sum(bd.get("specialistSlots", {}).values()) * 0.5
        war = 1.0 if ctx["wars"] else 0.5
        if ctx["threat"].get(c.id, 0) > 0:
            war *= 2
        if bd.get("cityHealth"):
            v += war * bd["cityHealth"] / max(1, cm.max_health(g, c)) * 4
        if bd.get("cityStrength"):
            from ..engine import combat
            v += war * bd["cityStrength"] / (combat.city_strength(g, c) + 3) * 4
        if bd.get("isWonder") or bd.get("isNationalWonder"):
            v += P["u_wonder_bonus"] if bd.get("isWonder") else P["u_wonder_bonus"] / 2
        if over_avg and any(x.ph in ("Triggers a Cultural Victory upon completion", "Triggers victory")
                            for x in bd["_umap"].all):
            v += 20
        return v

    def _choose_production_classic(self, g: Game, pid: int, c, ctx: dict, counts: dict, army: int, danger: bool) -> Optional[str]:
        """The older production heuristic: needs first, then value per turn.

        Kept because it is a useful control in lab experiments - a second opinion to measure the default
        against.
        """
        from ..engine import cities as cm
        R = g.rules
        options: list[tuple[float, str]] = []
        cities = ctx["cities"]
        n = len(cities)
        items = cm.buildable_items(g, c)
        units = set(items["units"])
        prod = max(1.0, cm.city_stats(g, c)["total"]["production"])

        def turns(name):
            """Turns for this city to finish an item."""
            return max(1.0, cm.remaining_work(g, c, name) / prod)

        defender = self.best_military(g, c, units, prefer_ranged=True)
        garrison = g.military_at(c.idx)
        if defender and self._need_escort.get(pid) == c.idx:     # a settler is waiting here for an escort
            self._need_escort.pop(pid, None)
            options.append((140, defender))
        if defender:
            if danger:
                options.append((1000, defender))
            elif garrison is None and (g.turn > 12 or ctx["hostile"]):
                options.append((300, defender))
            affordable = len(ctx["units"]) < ctx["supply"] and ctx["gpt"] >= 1
            target = ctx["army_target"]
            if army < target and (affordable or ctx["offense"]):
                siege = sum(1 for u in ctx["military"] if _ud(g, u).get("unitType") == "Siege")
                want_siege = ctx["offense"] and siege * 3 < len(ctx["military"]) - n + 1
                # only melee units capture cities: keep at least two in the field army
                field_melee = sum(1 for m in ctx["military"] if not _ud(g, m)["_ranged"] and not self._is_garrison(m))
                want_melee = ctx["offense"] and field_melee < 2
                attacker = (self.best_military(g, c, units, offense=True, melee_only=True) if want_melee else None) \
                    or (self.best_military(g, c, units, siege_only=True) if want_siege else None) \
                    or self.best_military(g, c, units, prefer_ranged=self.rng.random() < 0.3, offense=ctx["offense"]) \
                    or defender
                shortfall = 1 - army / max(1, target)
                base = (90 + 70 * self.aggression) if ctx["offense"] else (25 + 35 * self.aggression)
                options.append((base * (0.6 + 1.4 * shortfall), attacker))
        settler = next((u for u in units if R.units[u]["_umap"].get(U.FoundCity)), None)
        sites = self.expansion_sites(g, pid, ctx)
        max_cities = self.target_cities or 99
        P = self.p
        if settler and any(g.grid.distance(s, c.idx) <= 12 for s in sites) and n + counts["settler"] < max_cities \
                and counts["settler"] < max(1, n // 3) and not danger \
                and (ctx["hap"] >= P["settler_min_hap"] or (ctx["hap"] >= P["settler_min_hap_small"] and n < 3)):
            options.append(((P["settler_prio"] if g.turn < 90 else P["settler_prio_late"]) - n * P["settler_prio_per_city"],
                            settler))
        met_anyone = self._knows_rival_city(g, pid)        # a rival's city location is known (contact isn't enough)
        scout = next((u for u in units if _is_recon(R.units[u])), None)
        if scout and counts["recon"] == 0 and (not met_anyone or g.turn < 30) and g.turn < 150:
            options.append((80 if not met_anyone and g.turn > 15 else 30, scout))
        # still alone and nobody exploring (scouts often die to barbarians): send a sturdier unit to find the others
        if not met_anyone and g.turn > 30 and counts["recon"] == 0 and army <= len(ctx["military"]) \
                and not any(x.activity == "explore" for x in ctx["units"]):
            seeker = defender if (g.turn > 45 or not scout) else scout
            if seeker:
                options.append((150, seeker))
        worker = next((u for u in units if R.units[u]["_umap"].get(U.BuildImprovements)), None)
        if worker:
            want = n + (1 if n <= 2 else 0)
            if counts["worker"] < want:
                options.append((100 if counts["worker"] < max(1, n // 2) else 50, worker))
        boat = next((u for u in units if R.units[u]["_umap"].get(U.CreateWaterImprovements)), None)
        if boat and counts["boat"] == 0 and g.turn - self._boat_turn.get(c.id, -99) > 25:
            from ..engine import tiles as T
            for idx in g.grid.within(c.idx, 6):
                t = g.s.tiles[idx]
                if t.owner == pid and t.resource and g.is_water(idx) and not t.improvement \
                        and T.resource_visible(g, pid, t.resource):
                    options.append((45, boat))
                    break
        wonders = items["wonders"]
        if wonders and (sum(x.pop for x in cities) < P["wonder_min_pop"] or (P["wonder_avg_prod"] and prod <
                        sum(max(1.0, cm.city_stats(g, x)["total"]["production"]) for x in cities) / max(1, n))):
            wonders = []
        for b in items["buildings"] + wonders:
            v = self._building_value(g, pid, c, b, ctx)
            if v <= 0:
                continue
            options.append((v * 12 / (1 + turns(b) / 12), b))
        for name in items.get("other", []):
            if name in cm.PERPETUAL:
                continue
            bd = R.buildings.get(name)
            if bd is not None:
                options.append((350, name))       # projects (Apollo, Manhattan, Utopia...)
        for name in units:
            ud = R.units[name]
            if ud["_umap"].get(U.AddInCapital) or ud["_umap"].get(U.SpaceshipPart):
                options.append((400, name))
        if not options:
            if "Gold" in items.get("other", []):
                return "Gold"
            return self.best_military(g, c, units)
        options.sort(key=lambda o: -o[0])
        return options[0][1]

    def _simulate(self, g: Game, c, add: str) -> tuple[dict, float]:
        """City stats and city happiness with `add` built (UnCiv getStatDifferenceFromBuilding): the building is
        added temporarily and the caches are restored afterwards."""
        from ..engine import cities as cm
        saved_y, saved_c = g._ycache, g._cache
        g._ycache, g._cache = {}, {}
        c.buildings.append(add)
        try:
            st = cm.city_stats(g, c)["total"]
            hap = sum(cm.city_happiness(g, c).values())
        finally:
            c.buildings.remove(add)
            g._ycache, g._cache = saved_y, saved_c
        return st, hap

    def _building_value(self, g: Game, pid: int, c, b: str, ctx: dict) -> float:
        """Score a building by simulating the city with it built.

        More expensive than a static priority list and much better: the value of a granary depends on the
        city, and simulating answers that directly instead of approximating it.
        """
        from ..engine import cities as cm
        R = g.rules
        bd = R.buildings[b]
        base = cm.city_stats(g, c)["total"]
        base_hap = sum(cm.city_happiness(g, c).values())
        try:
            after, hap = self._simulate(g, c, b)
        except Exception:
            return 0.0
        P = self.p
        diff = {k: after.get(k, 0) - base.get(k, 0) for k in STAT_WEIGHTS if k != "happiness"}
        v = sum(diff[k] * P["w_" + k] for k in diff)
        dh = hap - base_hap
        want = len(ctx["cities"]) + 2
        v += dh * P["w_happiness"] * (3.0 if ctx["hap"] < 0 else (1.8 if ctx["hap"] < want else 0.4))
        if ctx["gpt"] < 0 and diff.get("gold", 0) > 0:
            v += diff["gold"] * 1.5
        v += sum(bd.get("greatPersonPoints", {}).values()) * 1.5
        v += sum(bd.get("specialistSlots", {}).values()) * 1.2
        if bd.get("cityStrength") or bd.get("cityHealth"):
            border = ctx["wars"] or ctx["threat"][c.id] > 0
            v += (bd.get("cityStrength", 0) / 2 + bd.get("cityHealth", 0) / 25) * (3.0 if border else 0.3)
        v += P["unique_bonus"] * len(bd.get("uniques", []))   # effects the simulation cannot see (free units, XP...)
        if bd.get("isWonder"):
            v = v * P["wonder_mult"] + P["wonder_bonus"]
        if c.pop <= 2 and not bd.get("isWonder"):
            v *= 0.8
        return v

    def best_military(self, g: Game, city, units: set, prefer_ranged: bool = False, offense: bool = False,
                      siege_only: bool = False, domain: str = "Land", melee_only: bool = False) -> Optional[str]:
        """The best military unit this city can build, for a role."""
        best, best_v = None, -1.0
        for name in units:
            ud = g.rules.units[name]
            if not ud["_military"] or _is_recon(ud) or ud["_domain"] != domain:
                continue
            if ud["_umap"].get(U.NuclearWeapon) or ud["_umap"].get(U.SelfDestructs):
                continue
            ut = ud.get("unitType")
            if siege_only and ut != "Siege":
                continue
            if melee_only and ud["_ranged"]:
                continue
            v = max(ud.get("strength", 0), ud.get("rangedStrength", 0) * (1.15 if prefer_ranged else 0.85))
            if ut == "Siege":
                v *= 1.0 if offense else 0.5
            if ut in ("Mounted", "Armored") and not offense:
                v *= 0.8
            v /= (ud.get("cost", 40) ** 0.35)
            if v > best_v:
                best, best_v = name, v
        return best

    def city_bombard(self, g: Game, pid: int):
        """Fire every city that has a target in range.

        Free damage that costs nothing but the call, and the most commonly forgotten action in the game -
        by models as much as by people.
        """
        from ..engine.briefing import bombard_targets
        for c, idx, dmg, kills in bombard_targets(g, pid):
            x, y = g.grid.xy(idx)
            self.ex(g, pid, "city_attack", city_id=c.id, x=x, y=y)

    # ------------------------------------------------------------------
    # gold
    # ------------------------------------------------------------------
    def manage_gold(self, g: Game, pid: int, ctx: dict):
        """Spend surplus gold: buying units and buildings, and courting city-states.

        Hoarding gold is one of the clearest failures of a naive bot, and one of the easiest to fix.
        """
        from ..engine import cities as cm, units as unitmod, city_states as CS
        p = g.player(pid)
        cities = ctx["cities"]
        for c in sorted(cities, key=lambda c: -ctx["threat"][c.id]):
            if ctx["threat"][c.id] <= self.city_defense(g, c) * 0.5:
                break
            if g.military_at(c.idx) is None:
                unit = self.best_military(g, c, set(cm.buildable_items(g, c)["units"]), prefer_ranged=True)
                if unit:
                    self.ex(g, pid, "buy", city_id=c.id, item=unit)
        if ctx["gpt"] < 0 and p.gold + ctx["gpt"] * 8 < 0:
            for u in self._spare_units(g, pid, ctx)[: max(1, int(-ctx["gpt"]))]:
                self.ex(g, pid, "unit_order", unit_id=u.id, order="disband")
            if cities and ctx["gpt"] < -3:
                big = max(cities, key=lambda c: c.pop)
                if big.focus != "gold":
                    self.ex(g, pid, "set_city_focus", city_id=big.id, focus="gold")
        elif ctx["gpt"] > 8:
            for c in cities:
                if c.focus == "gold":
                    self.ex(g, pid, "set_city_focus", city_id=c.id, focus="balanced")
        reserve = 60 + 25 * ctx["era"]
        for u in ctx["military"]:
            target, err, cost = unitmod.check_upgrade(g, u)
            if target and err is None and p.gold - cost > reserve:
                self.ex(g, pid, "upgrade_unit", unit_id=u.id)
        for c in sorted(cities, key=lambda c: cm.city_stats(g, c)["total"]["production"]):
            head = cm.current_construction(c)
            if not head or head in cm.PERPETUAL or c.puppet:
                continue
            reason, cost = cm.purchase_check(g, c, head, "Gold")
            if reason is not None or not cost:
                continue
            settler = head in g.rules.units and g.rules.units[head]["_umap"].get(U.FoundCity)
            worker = head in g.rules.units and g.rules.units[head]["_umap"].get(U.BuildImprovements)
            spare = p.gold - reserve
            cap = max(60 + 60 * ctx["era"] + (40 if settler else 0), spare * 0.6)
            if cost <= spare and cost <= cap and (settler or worker or head in g.rules.buildings or ctx["wars"])                     and not (head in g.rules.buildings and g.rules.buildings[head].get("isWonder")):
                if self.ex(g, pid, "buy", city_id=c.id, item=head) is not None:
                    self.manage_cities(g, pid)         # refill the emptied queue
        # court a city-state with spare gold
        if self.p["cs_gift_mode"] == "typed":
            self._gift_city_state(g, pid, ctx)
        elif p.gold > 350 + 80 * ctx["era"] and not ctx["wars"]:
            css = [q for q in g.city_states() if g.has_met(pid, q.id) and not g.at_war(pid, q.id)]
            if css:
                q = max(css, key=lambda q: (CS.influence(g, q.id, pid) >= 30, CS.influence(g, q.id, pid)))
                self.ex(g, pid, "city_state_action", player_id=q.id, action="gift_gold", amount=250)
        if g.religion_enabled:
            self._spend_faith(g, pid, ctx)

    def _gift_city_state(self, g: Game, pid: int, ctx: dict):
        """Gift gold where it buys the most: the city-state type we need (Mercantile when unhappy, Maritime for
        growth, Cultured for policies) and where the gift crosses the friend or ally threshold or keeps an alliance."""
        from ..engine import city_states as CS
        p = g.player(pid)
        reserve = 200 + 60 * ctx["era"]
        if p.gold < reserve + 150 or ctx["wars"]:
            return
        need_hap = ctx["hap"] < len(ctx["cities"]) + 2
        type_w = {"Mercantile": 3.0 if need_hap else 1.2, "Maritime": 2.0, "Cultured": 1.5, "Religious": 0.8,
                  "Militaristic": 0.6}
        amount = 250 if p.gold - reserve < 500 else 500
        best, best_v = None, 0.0
        for q in g.city_states():
            if not q.alive or not g.has_met(pid, q.id) or g.at_war(pid, q.id):
                continue
            inf = CS.influence(g, q.id, pid)
            gain = CS.influence_from_gold(g, q.id, pid, amount)
            after = inf + gain
            rivals = max((CS.influence(g, q.id, o.id) for o in g.majors() if o.id != pid and o.alive), default=0)
            v = 0.0
            if inf < CS.FRIEND <= after:
                v += 1.0
            if after >= CS.ALLY and (q.ally != pid) and after > rivals:
                v += 2.0
            if q.ally == pid and rivals > inf - 15:
                v += 1.5                                   # defend the alliance
            if q.ally == pid and inf - rivals > 30 and inf > CS.ALLY + 20:
                v = 0.0                                    # safely allied: no need to pay more
            v *= type_w.get(q.cs_type, 1.0)
            if v > best_v:
                best, best_v = q, v
        if best is not None:
            self.ex(g, pid, "city_state_action", player_id=best.id, action="gift_gold", amount=amount)

    def choose_beliefs(self, g: Game, pid: int, needed: dict) -> list[str]:
        """Pick religious beliefs, weighted by what this civilization can use."""
        from ..engine import religion
        if self.p["belief_mode"] != "prefs":
            return religion.ai_choose_beliefs(g, pid, needed)
        out, taken = [], religion.beliefs_taken(g)
        for t, n in needed.items():
            pool = [b for b in religion.beliefs_available(g, t) if b not in out and b not in taken]
            rank = BELIEF_PREFS.get(t) or [x for v in BELIEF_PREFS.values() for x in v]
            pool.sort(key=lambda b: (rank.index(b) if b in rank else 99, b))
            out.extend(pool[:n])
        return out

    def _spend_faith(self, g: Game, pid: int, ctx: dict):
        """Spend faith on units and buildings once there is enough."""
        from ..engine import cities as cm
        p = g.player(pid)
        if p.religion_state not in ("religion", "enhancing", "enhanced") or p.faith < 200:
            return
        if self.p["faith_buildings"]:
            # buildings our beliefs let us buy with faith (Pagoda, Mosque, Cathedral...) before more missionaries
            for c in sorted(ctx["cities"], key=lambda c: -c.pop):
                for name, bd in g.rules.buildings.items():
                    if bd.get("cost", 1) != 0 or name in c.buildings or bd.get("isWonder"):
                        continue
                    reason, cost = cm.purchase_check(g, c, name, "Faith")
                    if reason is None and cost and p.faith - cost > 50:
                        if self.ex(g, pid, "buy", city_id=c.id, item=name, currency="Faith") is not None:
                            return
        for c in ctx["cities"]:
            for name, ud in g.rules.units.items():
                if ud["_umap"].get(U.CanSpreadReligion) and not ud["_umap"].get(U.MayFoundReligion):
                    reason, cost = cm.purchase_check(g, c, name, "Faith")
                    if reason is None and cost and p.faith - cost > 100:
                        missionaries = sum(1 for u in ctx["units"] if u.type == name)
                        if missionaries < 2:
                            self.ex(g, pid, "buy", city_id=c.id, item=name, currency="Faith")
                        return

    def _spare_units(self, g: Game, pid: int, ctx: dict) -> list:
        """Units not committed to defence or a war, available for something else."""
        out = []
        for u in ctx["units"]:
            ud = _ud(g, u)
            if not ud["_military"]:
                continue
            city = g.city_at(u.idx)
            if city and city.owner == pid and ctx["threat"].get(city.id, 0) > 0:
                continue
            value = 0 if _is_recon(ud) else _power(ud)
            if ud.get("obsoleteTech") and g.has_tech(pid, ud["obsoleteTech"]):
                value *= 0.5
            if city and city.owner == pid and g.military_at(u.idx) is u:
                value += 30
            out.append((value, u))
        out.sort(key=lambda t: t[0])
        keep = max(len(ctx["cities"]), 1)
        left = len([1 for v, u in out if not _is_recon(_ud(g, u))])
        result = []
        for v, u in out:
            if not _is_recon(_ud(g, u)):
                if left <= keep:
                    break
                left -= 1
            result.append(u)
        return result

    # ------------------------------------------------------------------
    # units
    # ------------------------------------------------------------------
    def manage_units(self, g: Game, pid: int, ctx: Optional[dict] = None):
        """Give every unit its orders for the turn."""
        ctx = ctx or self.context(g, pid)
        self._garrisons = {c.id: m.id for c in ctx["cities"] for m in [g.military_at(c.idx)]
                           if m and m.owner == pid and _ud(g, m)["_domain"] == "Land"}
        order = sorted(g.player_units(pid), key=lambda u: (0 if _ud(g, u)["_ranged"] else 1, u.id))
        for u in order:
            if g.unit(u.id) is None or g.s.current != pid:
                continue
            ud = _ud(g, u)
            try:
                if ud["_umap"].get(U.FoundCity):
                    self.handle_settler(g, pid, u, ctx)
                elif ud["_umap"].get(U.BuildImprovements):
                    if u.activity not in ("automate", "build"):
                        self.ex(g, pid, "unit_order", unit_id=u.id, order="automate")
                elif ud["_umap"].get(U.CreateWaterImprovements):
                    self.handle_work_boat(g, pid, u)
                elif ud["_great_person"] or ud["_umap"].get(U.CanSpreadReligion) \
                        or ud["_umap"].get(U.AddInCapital) or ud["_umap"].get(U.CanRemoveHeresy):
                    self.handle_special(g, pid, u, ctx)
                elif _is_recon(ud):
                    self.handle_scout(g, pid, u, ctx)
                elif ud["_domain"] == "Air":
                    self.handle_air(g, pid, u)
                elif ud["_domain"] == "Water":
                    self.handle_naval(g, pid, u, ctx)
                elif ud["_military"]:
                    self.handle_military(g, pid, u, ctx)
            except ActionError:
                pass
            u = g.unit(u.id)
            if u is not None:
                self._promote(g, pid, u)

    def _promote(self, g: Game, pid: int, u):
        """Take a promotion when a unit has earned one."""
        from ..engine import units as unitmod
        guard = 0
        while unitmod.can_promote(g, u) and guard < 5:
            guard += 1
            opts = sorted(unitmod.available_promotions(g, u))
            if not opts:
                break
            city = g.city_at(u.idx)
            pick = None
            if city and city.owner == pid:
                pick = next((o for o in opts if o.startswith("Cover")), None)
            pick = pick or next((o for o in opts if o.startswith(("Shock", "Drill", "Accuracy", "Barrage", "Bombardment",
                                                                  "Targeting", "Interception"))), None) or opts[0]
            if self.ex(g, pid, "promote_unit", unit_id=u.id, promotion=pick) is None:
                break

    def _is_garrison(self, u) -> bool:
        """Whether a unit is assigned to garrison a city."""
        return u.id in self._garrisons.values()

    def _move(self, g: Game, pid: int, u, idx: int):
        """Move a unit toward a tile."""
        x, y = g.grid.xy(idx)
        return self.ex(g, pid, "move_unit", unit_id=u.id, x=x, y=y)

    def handle_settler(self, g: Game, pid: int, u, ctx: dict):
        """Send a settler to a site and found a city when it arrives."""
        from ..engine import cities as cm
        if not ctx["cities"] and cm.found_check(g, pid, u.idx) is None:
            self.ex(g, pid, "unit_action", unit_id=u.id, action="found_city")
            return
        if u.moves <= 0:
            return                  # already moved this turn (standing goto orders run at the start of the turn)
        sites = self.expansion_sites(g, pid, ctx)
        target = u.goto if u.goto is not None and cm.found_check(g, pid, u.goto) is None else None
        if target is None and sites:
            target = min(sites, key=lambda s: g.grid.distance(s, u.idx) + sites.index(s) * 1.5)
        if target is None:
            if cm.found_check(g, pid, u.idx) is None and ctx["cities"] and \
                    min(g.grid.distance(c.idx, u.idx) for c in ctx["cities"]) <= 10:
                self.ex(g, pid, "unit_action", unit_id=u.id, action="found_city")
            elif not g.city_at(u.idx) and ctx["cities"]:
                self._move(g, pid, u, min(ctx["cities"], key=lambda c: g.grid.distance(c.idx, u.idx)).idx)
            return
        if u.idx == target:
            self.ex(g, pid, "unit_action", unit_id=u.id, action="found_city")
            return
        # danger = a hostile unit that could reach the settler (or the site) next turn; an escort sharing the tile
        # makes the trip safe (UnCiv escorts settlers the same way)
        from ..engine.automation import _threat_reach
        danger = any(min(g.grid.distance(e.idx, u.idx), g.grid.distance(e.idx, target)) <= _threat_reach(g, e) + 1
                     for e in ctx["hostile"])
        if danger and not self._guarded(g, pid, u):
            esc = self._assign_escort(g, pid, u, ctx)
            if esc is None or not self._guarded(g, pid, u):
                key = (pid, target)
                self._retreats[key] = self._retreats.get(key, 0) + 1
                if self._retreats[key] >= 3:              # this site keeps being unsafe: pick another for a while
                    self._bad_sites[key] = g.turn
                    self._sites_cache.pop(pid, None)
                    self._retreats[key] = 0
                    u.goto = None
                if g.city_at(u.idx) is None and ctx["cities"]:
                    self._move(g, pid, u, min(ctx["cities"], key=lambda c: g.grid.distance(c.idx, u.idx)).idx)
                else:
                    self._need_escort[pid] = u.idx
                    self.ex(g, pid, "unit_order", unit_id=u.id, order="skip")
                return
        r = self._move(g, pid, u, target)
        self._follow(g, pid, u)
        if r is None:
            from ..engine import movement
            if g.unit(u.id) is not None and movement.find_path(g, u, target) is None:
                self._bad_sites[(pid, target)] = g.turn       # unreachable: try elsewhere for a while
                self._sites_cache.pop(pid, None)
                u.goto = None
        elif r.get("arrived") and g.unit(u.id) and u.moves > 0:
            self.ex(g, pid, "unit_action", unit_id=u.id, action="found_city")

    def _knows_rival_city(self, g: Game, pid: int) -> bool:
        """Whether this civilization has seen a rival city, which changes how it expands."""
        p = g.player(pid)
        return any(c.owner != pid and g.player(c.owner).kind == "major" and p.explored[c.idx]
                   for c in g.s.cities.values())

    def _guarded(self, g: Game, pid: int, u) -> bool:
        """Whether a civilian has an escort on its tile."""
        m = g.military_at(u.idx)
        return m is not None and m.owner == pid

    def _assign_escort(self, g: Game, pid: int, u, ctx: dict):
        """Bring a free (non-garrison) land military unit onto the settler's tile."""
        esc = g.unit(self._escorts.get(u.id)) if self._escorts.get(u.id) is not None else None
        if esc is None:
            free = [m for m in ctx["military"] if _ud(g, m)["_domain"] == "Land" and not self._is_garrison(m)
                    and m.id not in self._escorts.values() and m.hp >= 50 and g.grid.distance(m.idx, u.idx) <= 6]
            if not free:
                return None
            esc = min(free, key=lambda m: g.grid.distance(m.idx, u.idx))
            self._escorts[u.id] = esc.id
        if esc.idx != u.idx and esc.moves > 0:
            self._move(g, pid, esc, u.idx)
        return esc

    def _follow(self, g: Game, pid: int, u):
        """The settler moved: its escort (if any) moves onto the same tile."""
        eid = self._escorts.get(u.id)
        esc = g.unit(eid) if eid is not None else None
        if esc is None or g.unit(u.id) is None:
            self._escorts.pop(u.id, None)
            return
        if esc.idx != u.idx and esc.moves > 0:
            self._move(g, pid, esc, u.idx)

    def handle_work_boat(self, g: Game, pid: int, u):
        """Send a work boat to the nearest sea resource worth improving."""
        from ..engine.actions import unit_actions
        from ..engine import tiles as T
        acts = [a for a in unit_actions(g, u) if a["id"].startswith("create:") and a["available"]]
        if acts:
            self.ex(g, pid, "unit_action", unit_id=u.id, action=acts[0]["id"])
            return
        best = None
        for idx in g.grid.within(u.idx, 8):
            t = g.s.tiles[idx]
            if t.owner == pid and t.resource and g.is_water(idx) and not t.improvement \
                    and T.resource_visible(g, pid, t.resource):
                best = idx
                break
        if best is None:
            self.ex(g, pid, "unit_order", unit_id=u.id, order="disband")
            return
        self._move(g, pid, u, best)
        if g.unit(u.id) and u.idx == best:
            acts = [a for a in unit_actions(g, u) if a["id"].startswith("create:") and a["available"]]
            if acts:
                self.ex(g, pid, "unit_action", unit_id=u.id, action=acts[0]["id"])

    def handle_special(self, g: Game, pid: int, u, ctx: dict):
        """Great people, religious units and spaceship parts: use the best available action."""
        from ..engine.actions import unit_actions
        from ..engine import religion
        ud = _ud(g, u)
        p = g.player(pid)
        acts = {a["id"]: a for a in unit_actions(g, u)}
        ok = {k for k, a in acts.items() if a["available"]}
        cap = g.city(p.capital) if p.capital is not None else None
        if "add_to_spaceship" in acts:
            if "add_to_spaceship" in ok:
                self.ex(g, pid, "unit_action", unit_id=u.id, action="add_to_spaceship")
            elif cap is not None:
                self._move(g, pid, u, cap.idx)
            return
        if "found_religion" in acts:
            if "found_religion" in ok:
                need = religion.beliefs_to_choose(g, pid, enhancing=False)
                beliefs = self.choose_beliefs(g, pid, need)
                self.ex(g, pid, "unit_action", unit_id=u.id, action="found_religion",
                        name=f"Faith of {p.name}", beliefs=beliefs)
                return
            if religion.can_found_religion(g, pid) is None:
                # any own city will do (UnCiv); pick the nearest one without another civilian in it
                free = [c for c in ctx["cities"] if not any(x.id != u.id and not _ud(g, x)["_military"]
                                                              for x in g.units_at(c.idx))]
                if free:
                    self._move(g, pid, u, min(free, key=lambda c: g.grid.distance(c.idx, u.idx)).idx)
                    return
        if "enhance_religion" in ok:
            need = religion.beliefs_to_choose(g, pid, enhancing=True)
            self.ex(g, pid, "unit_action", unit_id=u.id, action="enhance_religion",
                    beliefs=self.choose_beliefs(g, pid, need))
            return
        for a in ("hurry_research", "trade_mission", "hurry_construction"):
            if a in ok and (a != "hurry_construction" or g.city_at(u.idx).queue):
                self.ex(g, pid, "unit_action", unit_id=u.id, action=a)
                return
        trig = [k for k in ok if k.startswith("trigger:")]
        if trig:
            self.ex(g, pid, "unit_action", unit_id=u.id, action=trig[0])
            return
        if "spread_religion" in acts or "remove_heresy" in acts:
            here = g.city(g.s.tiles[u.idx].city) if g.s.tiles[u.idx].city is not None else None
            if "spread_religion" in ok and here is not None and religion.majority_religion(g, here) != u.religion:
                self.ex(g, pid, "unit_action", unit_id=u.id, action="spread_religion")
                return
            target = None
            for c in sorted(g.s.cities.values(), key=lambda c: g.grid.distance(c.idx, u.idx)):
                if (c.owner == pid or g.player(c.owner).kind != "barbarian") and not g.at_war(pid, c.owner) \
                        and religion.majority_religion(g, c) != u.religion and p.explored[c.idx]:
                    target = c
                    break
            if target is not None and g.grid.distance(target.idx, u.idx) > 1:
                near = [n for n in g.grid.neighbors(target.idx) if not g.units_at(n) and not g.is_water(n)]
                if near:
                    self._move(g, pid, u, min(near, key=lambda n: g.grid.distance(n, u.idx)))
            return
        if ud.get("unitType") == "Civilian":
            creates = [k for k in ok if k.startswith("create:")]
            if creates and g.s.tiles[u.idx].owner == pid and not g.city_at(u.idx):
                self.ex(g, pid, "unit_action", unit_id=u.id, action=creates[0])
                return
            # walk to a free tile next to the capital to build the great improvement there
            if cap is not None:
                spots = [i for i in g.grid.within(cap.idx, 2)[1:] if g.s.tiles[i].owner == pid and not g.is_water(i)
                         and not g.s.tiles[i].improvement and not g.units_at(i)]
                if spots:
                    self._move(g, pid, u, min(spots, key=lambda i: g.grid.distance(i, u.idx)))
                    return
        # great generals and admirals follow the army
        army = [m for m in ctx["military"] if _ud(g, m)["_domain"] == _ud(g, u)["_domain"]]
        if army:
            front = min(army, key=lambda m: -len([e for e in ctx["hostile"] if g.grid.distance(e.idx, m.idx) <= 4]))
            if g.grid.distance(front.idx, u.idx) > 1:
                self._move(g, pid, u, front.idx if front.idx != u.idx else u.idx)
        elif u.activity is None:
            self.ex(g, pid, "unit_order", unit_id=u.id, order="sleep")

    def handle_scout(self, g: Game, pid: int, u, ctx: dict):
        """Keep a scout exploring."""
        if u.activity == "explore":
            return
        r = self.ex(g, pid, "unit_order", unit_id=u.id, order="explore")
        if r is None or not r.get("exploring"):
            if len(ctx["units"]) >= ctx["supply"] or g.turn > 60:
                self.ex(g, pid, "unit_order", unit_id=u.id, order="disband")
            else:
                self.ex(g, pid, "unit_order", unit_id=u.id, order="sleep")

    def handle_air(self, g: Game, pid: int, u):
        """Base and use aircraft."""
        from ..engine import units as unitmod
        for idx in g.grid.within(u.idx, unitmod.attack_range(g, u)):
            if idx == u.idx:
                continue
            owner = None
            c = g.city_at(idx)
            m = g.military_at(idx)
            if c is not None:
                owner = c.owner
            elif m is not None:
                owner = m.owner
            if owner is not None and g.at_war(pid, owner):
                x, y = g.grid.xy(idx)
                if self.ex(g, pid, "attack", unit_id=u.id, x=x, y=y):
                    return
        if u.activity != "sleep":
            self.ex(g, pid, "unit_order", unit_id=u.id, order="sleep")

    def handle_naval(self, g: Game, pid: int, u, ctx: dict):
        """Fight with, or reposition, a ship."""
        if self._attack_best(g, pid, u):
            return
        if u.hp < 60:
            if u.activity != "heal":
                self.ex(g, pid, "unit_order", unit_id=u.id, order="heal")
            return
        if u.activity is None:
            self.ex(g, pid, "unit_order", unit_id=u.id, order="sleep")

    def _attack_best(self, g: Game, pid: int, u, allow_city_capture: bool = True) -> bool:
        """Attack the best target in reach, if attacking is worthwhile.

        Predicted damage decides: a unit that would lose the exchange holds instead, which is the
        difference between an army and a queue of casualties.
        """
        from ..engine import combat, units as unitmod
        ud = _ud(g, u)
        plan = self._war_plan.get(pid)
        siege_city = plan["city"] if plan and plan.get("advance") else None
        if u.moves <= 0 or combat.can_attack_now(g, u) is not None:
            return False
        radius = unitmod.attack_range(g, u) if ud["_ranged"] else 1
        best, best_v = None, 0.0
        for idx in g.grid.within(u.idx, radius)[1:]:
            try:
                pv = combat.preview(g, u, idx)
            except ActionError:
                continue
            dd = sum(pv["damage_to_defender"]) / 2
            da = sum(pv["damage_to_attacker"]) / 2
            if pv["target"] == "city":
                if pv.get("note"):
                    if not ud["_ranged"] and allow_city_capture:
                        best, best_v = idx, 10000
                        break
                    continue
                v = dd * (1.5 if ud.get("unitType") == "Siege" else 1.0) - da * 1.6
                if idx == siege_city:
                    v *= self.p["siege_city_pref"]          # focus fire on the city we are besieging
                if not ud["_ranged"] and u.hp - da < 40:
                    continue
            else:
                kill = dd >= pv["defender_hp"]
                v = dd * (3 if kill else 1) - da * 1.3
                if not ud["_ranged"] and u.hp - da < 25:
                    continue
            if v > best_v:
                best, best_v = idx, v
        if best is None:
            return False
        x, y = g.grid.xy(best)
        return self.ex(g, pid, "attack", unit_id=u.id, x=x, y=y) is not None

    def handle_military(self, g: Game, pid: int, u, ctx: dict):
        """Give a land unit its orders: defend, besiege, escort or advance."""
        from ..engine import movement
        ud = _ud(g, u)
        if u.moves <= 0:
            return
        garrison = self._is_garrison(u)
        ward = next((sid for sid, eid in self._escorts.items() if eid == u.id), None)
        if ward is not None:
            if g.unit(ward) is not None:
                if g.unit(u.id) is not None and g.unit(ward) is not None and u.idx != g.unit(ward).idx and u.moves > 0:
                    self._move(g, pid, u, g.unit(ward).idx)
                return
            self._escorts.pop(ward, None)
        # ranged units in an active siege first move to a firing position on the target city
        plan = self._war_plan.get(pid) if ctx["wars"] else None
        if plan and plan.get("advance") and ud["_ranged"] and not garrison and self.p["siege_move_first"]:
            from ..engine import units as unitmod
            d = g.grid.distance(u.idx, plan["city"])
            rng = unitmod.attack_range(g, u)
            if 1 < d <= rng + 3:
                from ..engine import visibility
                if d > rng or not visibility.has_los(g, u.idx, plan["city"]):
                    dest = self._approach_tile(g, pid, u, plan["city"], siege=True)
                    if dest is not None and dest != u.idx:
                        self._move(g, pid, u, dest)
                        if g.unit(u.id) is None or u.moves <= 0:
                            return
        if self._attack_best(g, pid, u):
            if g.unit(u.id) is None or u.moves <= 0 or garrison:
                return
        if garrison:
            if u.activity is None:
                self.ex(g, pid, "unit_order", unit_id=u.id, order="fortify")
            return
        if u.hp < 50:
            cities = [c for c in ctx["cities"] if g.military_at(c.idx) is None]
            if cities and not g.city_at(u.idx):
                c = min(cities, key=lambda c: g.grid.distance(c.idx, u.idx))
                if g.grid.distance(c.idx, u.idx) <= 4:
                    self._move(g, pid, u, c.idx)
                    if g.unit(u.id) and u.idx == c.idx:
                        self._garrisons[c.id] = u.id
                    return
            if u.activity not in ("heal", "fortify"):
                self.ex(g, pid, "unit_order", unit_id=u.id, order="heal")
            return
        empty = [c for c in ctx["cities"] if c.id not in self._garrisons]
        if empty:
            c = min(empty, key=lambda c: g.grid.distance(c.idx, u.idx))
            if g.grid.distance(c.idx, u.idx) <= 10:
                self._garrisons[c.id] = u.id
                if u.idx != c.idx:
                    self._move(g, pid, u, c.idx)
                if g.unit(u.id) and u.idx == c.idx:
                    self.ex(g, pid, "unit_order", unit_id=u.id, order="fortify")
                return
        campaigning = bool(ctx["wars"]) and pid in self._war_plan
        threatened = [c for c in ctx["cities"]
                      if ctx["threat"][c.id] > (self.city_defense(g, c) * 0.4 if campaigning else 0)]
        if threatened:
            c = max(threatened, key=lambda c: ctx["threat"][c.id] - g.grid.distance(c.idx, u.idx) * 2)
            if g.grid.distance(c.idx, u.idx) <= (5 if campaigning else 8):
                enemies = ctx["near_enemies"][c.id]
                target = min(enemies, key=lambda e: g.grid.distance(e.idx, u.idx)).idx if enemies else c.idx
                dest = self._approach_tile(g, pid, u, target)
                if dest is not None:
                    self._move(g, pid, u, dest)
                    self._attack_best(g, pid, u)
                return
        if ctx["wars"] and ud["_domain"] == "Land":
            plan = self._war_target(g, pid, ctx)
            if plan is not None:
                if not plan["advance"]:
                    dest = plan["rally"] if g.grid.distance(u.idx, plan["rally"]) > 2 else u.idx
                    if dest != u.idx and not movement.can_stand(g, pid, ud, dest, u):
                        dest = self._approach_tile(g, pid, u, plan["rally"], ring=1)
                else:
                    ranged = ud["_ranged"]
                    close_in = ranged or plan["siege_ready"]
                    dest = self._approach_tile(g, pid, u, plan["city"], siege=ranged, ring=0 if close_in else 2)
                if dest is not None and dest != u.idx:
                    self._move(g, pid, u, dest)
                    self._attack_best(g, pid, u)
                    return
                if dest == u.idx:
                    if u.activity is None and not plan["advance"]:
                        self.ex(g, pid, "unit_order", unit_id=u.id, order="fortify")
                    return
        prep = self._war_prep.get(pid)
        if not ctx["wars"] and prep and prep.get("rally") is not None and ud["_domain"] == "Land":
            rally = prep["rally"]
            if g.grid.distance(u.idx, rally) > 2:
                dest = rally if movement.can_stand(g, pid, ud, rally, u) else self._approach_tile(g, pid, u, rally, ring=1)
                if dest is not None and dest != u.idx:
                    self._move(g, pid, u, dest)
                    return
            elif u.activity is None:
                self.ex(g, pid, "unit_order", unit_id=u.id, order="fortify")
            return
        camp = self._camp_target(g, pid, u, ctx)
        if camp is not None:
            defender = g.military_at(camp)
            dest = camp if defender is None else self._approach_tile(g, pid, u, camp)
            if dest is not None and dest != u.idx and self._move(g, pid, u, dest):
                self._attack_best(g, pid, u)
                return
        ruins = self._ruin_target(g, pid, u)
        if ruins is not None and self._move(g, pid, u, ruins):
            return
        g.player(pid)
        if g.turn > 30 and not self._knows_rival_city(g, pid) \
                and not any(x.activity == "explore" and _ud(g, x)["_military"] and not _is_recon(_ud(g, x))
                            for x in ctx["units"]):
            r = self.ex(g, pid, "unit_order", unit_id=u.id, order="explore")
            if r and r.get("exploring"):
                return
        if not g.city_at(u.idx) and ctx["cities"]:
            c = min(ctx["cities"], key=lambda c: g.grid.distance(c.idx, u.idx))
            spot = c.idx if g.military_at(c.idx) is None else self._approach_tile(g, pid, u, c.idx)
            if spot is not None and spot != u.idx and g.grid.distance(spot, u.idx) > 1 and self._move(g, pid, u, spot):
                return
        if u.activity is None:
            self.ex(g, pid, "unit_order", unit_id=u.id, order="fortify")

    def _camp_target(self, g: Game, pid: int, u, ctx: dict) -> Optional[int]:
        """A barbarian camp worth clearing, if there is one."""
        p = g.player(pid)
        best, best_d = None, 99
        for camp in g.s.camps.values():
            idx = camp["idx"]
            if camp.get("destroyed") or not p.explored[idx]:
                continue
            d_city = min((g.grid.distance(c.idx, idx) for c in ctx["cities"]), default=99)
            d = g.grid.distance(u.idx, idx)
            if d_city <= 14 and d <= 16 and d < best_d:
                best, best_d = idx, d
        return best

    def _ruin_target(self, g: Game, pid: int, u) -> Optional[int]:
        """Ancient ruins worth walking to."""
        p = g.player(pid)
        for idx in g.grid.within(u.idx, 5)[1:]:
            if p.explored[idx] and g.s.tiles[idx].improvement == "Ancient ruins" and not g.units_at(idx):
                return idx
        return None

    def _war_target(self, g: Game, pid: int, ctx: dict) -> Optional[dict]:
        """The city this civilization's war is aimed at, if it is at war by choice."""
        plan = self._war_plan.get(pid)
        p = g.player(pid)
        if plan:
            c = g.city_at(plan["city"])
            if c is None or c.owner == pid or not g.at_war(pid, c.owner) or not ctx["cities"]:
                plan = None
        if plan is None:
            best, best_score = None, 999
            for c in g.s.cities.values():
                if c.owner in ctx["wars"] and p.explored[c.idx]:
                    d = min((g.grid.distance(c.idx, mc.idx) for mc in ctx["cities"]), default=99)
                    score = d + c.pop * 0.5 - (6 if c.damaged_turn >= g.turn - 1 else 0)
                    if score < best_score:
                        best, best_score = c, score
            if best is None:
                return None
            plan = {"city": best.idx, "since": g.turn, "advance": False, "checked": -1}
            self._war_plan[pid] = plan
        if plan.get("checked") != g.turn:
            from ..engine import cities as cm
            plan["checked"] = g.turn
            target = plan["city"]
            stage = min(ctx["cities"], key=lambda c: g.grid.distance(c.idx, target))
            if plan.get("rally") is None:
                plan["rally"] = self._rally_point(g, pid, target, stage.idx)
            field = [u for u in ctx["military"] if _ud(g, u)["_domain"] == "Land" and not self._is_garrison(u)]
            rally_d = g.grid.distance(plan["rally"], target)
            if plan["advance"]:
                gathered = [u for u in field if g.grid.distance(u.idx, target) <= 4]
            else:
                gathered = [u for u in field if g.grid.distance(u.idx, plan["rally"]) <= 4
                            or g.grid.distance(u.idx, target) <= rally_d]
            need = max(3, min(8, int(len(field) * 0.6)))
            city = g.city_at(target)
            weak = city is not None and (city.health <= cm.max_health(g, city) * 0.5
                                         or self.military_power(g, city.owner) * 3 < self.military_power(g, pid))
            if not plan["advance"] and (len(gathered) >= need or (weak and len(field) >= 2)):
                plan["advance"] = True
            elif plan["advance"] and len(gathered) < 2 and not weak:
                plan["advance"] = False
                plan["rally"] = self._rally_point(g, pid, target, stage.idx)
            in_range = sum(1 for u in field if _ud(g, u)["_ranged"] and g.grid.distance(u.idx, target) <= 2)
            plan["siege_ready"] = bool(city) and (in_range >= 1 or city.health <= cm.max_health(g, city) * 0.5)
        return plan

    def _rally_point(self, g: Game, pid: int, target: int, stage: int) -> int:
        """Where to gather an army before attacking, near the target but not yet in reach.

        Gathering first is what makes the bot's wars work at all. Units sent one at a time arrive one at a
        time and die one at a time.
        """
        cont = g.s.continents
        best, best_v = stage, 1e9
        for idx in g.grid.within(target, 5):
            d = g.grid.distance(idx, target)
            t = g.s.tiles[idx]
            if d < 4 or cont[idx] != cont[target] or g.city_at(idx) or t.owner not in (None, pid):
                continue
            v = g.grid.distance(idx, stage) - (2 if t.hills or t.feature in ("Forest", "Jungle") else 0)
            if v < best_v:
                best, best_v = idx, v
        return best

    def _approach_tile(self, g: Game, pid: int, u, target: int, siege: bool = False, ring: int = 0) -> Optional[int]:
        """The tile a unit should move to next when closing on a target."""
        from ..engine import movement, visibility, units as unitmod
        ud = _ud(g, u)
        want = ring or (2 if siege and ud.get("range", 0) >= 2 else 1)
        # a ranged unit needs line of sight to shoot from 2+ tiles away (unless it has indirect fire)
        needs_los = siege and ud["_ranged"] and not unitmod.unit_has(g, u, U.IndirectFire, with_civ=True)

        def sees(n):
            """Whether a tile has line of sight to the target, for siege units that need it."""
            return not needs_los or g.grid.distance(n, target) <= 1 or visibility.has_los(g, n, target)
        if g.grid.distance(u.idx, target) <= want and sees(u.idx):
            return u.idx
        opts = []
        for r in ([want, 1] if want > 1 and not ring else [want]):
            opts = [n for n in g.grid.ring(target, r)
                    if (n == u.idx or movement.can_stand(g, pid, ud, n, u)) and sees(n)]
            if opts:
                break
        if not opts:
            opts = [n for n in g.grid.within(target, 3)[1:] if movement.can_stand(g, pid, ud, n, u)]
        if not opts:
            return None
        return min(opts, key=lambda n: (g.grid.distance(n, u.idx), -g.s.tiles[n].hills))

    # ------------------------------------------------------------------
    # diplomacy
    # ------------------------------------------------------------------
    def military_power(self, g: Game, pid: int) -> float:
        """This civilization's military strength, for comparing against neighbours."""
        from ..engine.victory import military_strength
        return military_strength(g, pid) + 1

    def consider_diplomacy(self, g: Game, pid: int, ctx: Optional[dict] = None):
        """Decide on wars, peace, denunciations and friendships."""
        from ..engine import diplomacy as D
        ctx = ctx or self.context(g, pid)
        p = g.player(pid)
        mine = self.military_power(g, pid)
        for q in list(p.met):
            if not g.player(q).alive or g.player(q).kind != "major":
                continue
            rel = g.relation(pid, q)
            if rel is None:
                continue
            theirs = self.military_power(g, q)
            if rel["war"]:
                lost = sum(1 for c in g.s.cities.values() if c.founder == pid and c.owner == q)
                long_war = g.turn - rel["since"] >= self.p["war_long_turns"]
                losing = mine < theirs * 0.9 or lost
                plan = self._war_plan.get(pid)
                tc = g.city_at(plan["city"]) if plan else None
                if tc is not None and tc.owner == q and not lost and mine >= theirs * 0.9:
                    from ..engine import cities as cm
                    near = sum(1 for m in ctx["military"] if g.grid.distance(m.idx, tc.idx) <= 3)
                    if near >= 2 and tc.health < cm.max_health(g, tc) * 0.8:
                        long_war = False            # the siege is progressing: keep going
                if g.turn - rel["since"] >= 10 and (losing or long_war) and self.rng.random() < 0.35:
                    self.ex(g, pid, "open_negotiation", to=q, message="This war profits no one. Let us make peace.",
                            give=[{"type": "peace_treaty"}], receive=[])
                continue
            if g.turn % 10 == (pid + q) % 10:
                # embassies first, then friendship with those we like
                if not D.has_embassy(g, pid, q) and g.civ_has(pid, U.EnablesEmbassies) and g.civ_has(q, U.EnablesEmbassies):
                    self.ex(g, pid, "open_negotiation", to=q, message="Let us exchange embassies.",
                            give=[{"type": "embassy"}], receive=[{"type": "embassy"}])
                elif not D.is_friends(g, pid, q) and D.opinion(g, pid, q) >= 0 and mine < theirs * 2 \
                        and self.rng.random() < 0.4 - 0.3 * self.aggression:
                    self.ex(g, pid, "open_negotiation", to=q, message="Let us declare our friendship.",
                            give=[{"type": "declaration_of_friendship"}], receive=[])
                elif D.is_friends(g, pid, q) and rel.get("ra_until", 0) < g.turn \
                        and p.gold > D.ra_cost(g, pid, q) + 100 and g.civ_has(pid, U.EnablesResearchAgreements):
                    self.ex(g, pid, "open_negotiation", to=q, message="A research agreement would benefit us both.",
                            give=[{"type": "research_agreement"}], receive=[])
            if g.turn > 50 and not ctx["wars"] and pid not in self._war_prep and rel["treaty_until"] < g.turn \
                    and sum(ctx["threat"].values()) < mine * 0.15 and not D.is_friends(g, pid, q):
                if mine > theirs * (1.35 - 0.5 * self.aggression) \
                        and self.rng.random() < (0.02 + 0.15 * self.aggression) * self.p["war_prep_rate"] \
                        and self._reachable_city(g, pid, q, ctx) is not None:
                    self._war_prep[pid] = {"player": q, "since": g.turn}
        prep = self._war_prep.get(pid)
        if prep and not ctx["wars"]:
            q = prep["player"]
            theirs = self.military_power(g, q)
            field = [u for u in ctx["military"] if _ud(g, u)["_domain"] == "Land" and g.military_at(u.idx) is u
                     and not (g.city_at(u.idx) and self._is_garrison(u))]
            target = self._reachable_city(g, pid, q, ctx) if g.player(q).alive else None
            rel = g.relation(pid, q) or {}
            if target is None or g.turn - prep["since"] > 40 or mine < theirs * (1.15 - 0.4 * self.aggression) \
                    or rel.get("treaty_until", 0) >= g.turn or D.is_friends(g, pid, q):
                self._war_prep.pop(pid, None)
            else:
                need = max(4, min(8, len(ctx["cities"]) // 2 + 2))
                if self.p["prep_gather"]:
                    if prep.get("target") != target.idx:
                        stage = min(ctx["cities"], key=lambda c: g.grid.distance(c.idx, target.idx))
                        prep["target"], prep["rally"] = target.idx, self._rally_point(g, pid, target.idx, stage.idx)
                    ready_units = [u for u in field if g.grid.distance(u.idx, prep["rally"]) <= 3]
                else:
                    ready_units = field
                overwhelming = len(ready_units) >= 2 and mine > theirs * 4
                if (len(ready_units) >= need or overwhelming) and mine > theirs * (1.35 - 0.4 * self.aggression):
                    if self.ex(g, pid, "declare_war", player_id=q, message="Your lands will be ours.") is not None:
                        gathered = self.p["prep_gather"]
                        self._war_plan[pid] = {"city": target.idx, "since": g.turn, "advance": gathered, "checked": -1,
                                               "rally": prep.get("rally")}
                    self._war_prep.pop(pid, None)
        elif prep:
            self._war_prep.pop(pid, None)
        k = max(1, int(self.p["lux_trade_every"]))
        if g.turn % k == pid % k:
            self.trade_luxuries(g, pid)

    def _reachable_city(self, g: Game, pid: int, q: int, ctx: dict):
        """A rival city this civilization could actually attack, given geography."""
        p = g.player(pid)
        cont = g.s.continents
        ours = {cont[c.idx] for c in ctx["cities"]}
        near = [c for c in g.s.cities.values() if c.owner == q and p.explored[c.idx] and cont[c.idx] in ours]
        return min(near, key=lambda c: (c.pop, min(g.grid.distance(c.idx, mc.idx) for mc in ctx["cities"]))) if near else None

    def trade_luxuries(self, g: Game, pid: int):
        """Offer luxury trades, which raise happiness on both sides."""
        from ..engine import economy
        p = g.player(pid)
        mine = economy.luxury_resources(g, pid)
        surplus = [r for r, d in mine.items() if d["net"] >= 2]
        if not surplus:
            return
        for q in p.met:
            other = g.player(q)
            if not other.alive or other.kind != "major" or g.at_war(pid, q):
                continue
            theirs = economy.luxury_resources(g, q)
            give = next((r for r in surplus if theirs[r]["net"] <= 0), None)
            if give is None:
                continue
            want = next((r for r, d in theirs.items() if d["net"] >= 2 and mine[r]["net"] <= 0), None)
            receive = [{"type": "resource", "resource": want, "amount": 1}] if want \
                else [{"type": "gold_per_turn", "amount": 3, "turns": g.speed["dealDuration"]}]
            if not want and g.player(q).gold < 20:
                continue
            self.ex(g, pid, "open_negotiation", to=q, message="We have luxuries to spare. Shall we trade?",
                    give=[{"type": "resource", "resource": give, "amount": 1}], receive=receive)
            return

    def evaluate(self, g: Game, pid: int, other: int, give: list, receive: list) -> float:
        """Value a proposed deal from this civilization's point of view.

        Positive means the deal is worth accepting. The same function values both sides, so the bot's own
        offers are ones it would accept - which is what stops it proposing things nobody would take.
        """
        from ..engine import economy, research, diplomacy as D
        mine, theirs = self.military_power(g, pid), self.military_power(g, other)
        dur = g.speed["dealDuration"]
        v = 0.0
        for items, sign in ((receive, 1), (give, -1)):
            for it in items:
                t = it["type"]
                if t == "gold":
                    v += sign * it["amount"]
                elif t == "gold_per_turn":
                    v += sign * it["amount"] * it.get("turns", dur) * 0.8
                elif t == "resource":
                    rd = g.rules.resources[it["resource"]]
                    if rd["resourceType"] == "Luxury":
                        have = economy.luxury_resources(g, pid)[it["resource"]]["net"]
                        v += sign * (60 if (sign > 0 and have <= 0) or (sign < 0 and have <= 1) else 15) * it.get("amount", 1)
                    else:
                        # strategic resources matter more as units need them: scale with the era
                        have = economy.strategic_resources(g, pid)[it["resource"]]["available"]
                        era = 1 + research.player_era(g, pid)
                        v += sign * (45 + 15 * era if have <= 0 else 12 * era) * it.get("amount", 1)
                elif t == "open_borders":
                    v += sign * 12
                elif t == "embassy":
                    v += sign * 8
                elif t == "peace_treaty":
                    plan = self._war_plan.get(pid)
                    attacking = plan is not None and g.city_at(plan["city"]) is not None \
                        and g.city_at(plan["city"]).owner == other
                    if attacking and g.turn - plan["since"] < 40 and mine > theirs * 0.8:
                        v -= 150
                    else:
                        v += 120 if mine <= theirs * 1.3 else -60
                elif t in ("declaration_of_friendship", "defensive_pact"):
                    if sign > 0:   # mutual items appear on both sides; count once
                        v += 25 if D.opinion(g, pid, other) >= 0 and pid not in self._war_prep else -80
                elif t == "research_agreement":
                    if sign > 0:
                        v += 40 if g.player(pid).gold > D.ra_cost(g, pid, other) + 50 else -100
                elif t == "declare_war":
                    v += sign * self._war_item_value(g, pid, it["target"], giving=sign < 0, mine=mine)
                elif t == "share_map":
                    v += sign * (25 if sign > 0 else 20)
                elif t == "city":
                    c = g.city(it["city_id"])
                    v += sign * (300 + 60 * (c.pop if c else 1))
                elif t == "tech":
                    cost = research.tech_cost(g, pid, it["tech"])
                    v += sign * cost * (0.6 if sign > 0 else 0.9)
        return v

    def _war_item_value(self, g: Game, pid: int, target: int, giving: bool, mine: float) -> float:
        """Worth of a "declare war on target" deal item. Giving one (we declare) costs the risk of fighting the
        target, the diplomatic price of attacking a city-state and the betrayal of a friend; receiving one (they
        declare) only helps against someone we fight or plan to fight."""
        from ..engine import diplomacy as D
        prep = self._war_prep.get(pid) or {}
        ours = g.at_war(pid, target) or prep.get("target") in {c.idx for c in g.player_cities(target)} or \
            any(g.city_at(p["city"]) is not None and g.city_at(p["city"]).owner == target
                for p in [self._war_plan.get(pid)] if p)
        if not giving:
            return 200 if ours else 20
        if g.at_war(pid, target):
            return 0
        theirs = self.military_power(g, target)
        cost = 250 + 250 * max(0.0, theirs / max(1.0, mine) - 0.5)
        if g.player(target).kind == "city_state":
            cost += 300                 # other city-states and the target's protectors take offence
        if D.is_friends(g, pid, target) or D.has_pact(g, pid, target):
            cost += 400
        if ours:
            cost *= 0.3                 # we meant to fight them anyway
        return cost * (1.3 - 0.6 * self.aggression)

    def handle_negotiations(self, g: Game, pid: int):
        """Answer every negotiation waiting on this civilization."""
        for n in list(g.s.negotiations):
            if n["status"] == "open" and n["awaiting"] == pid:
                self.respond(g, pid, n["id"])

    def respond(self, g: Game, pid: int, nid: int):
        """Accept, reject or counter one negotiation."""
        from ..engine.diplomacy import get_negotiation
        n = get_negotiation(g, nid)
        if n["status"] != "open" or n["awaiting"] != pid:
            return
        other = n["responder"] if pid == n["initiator"] else n["initiator"]
        if not n["proposal"]:
            if n["exchanges"] >= 3:
                self.ex(g, pid, "respond_negotiation", negotiation_id=nid, action="reject",
                        message="We have nothing further to discuss.")
            else:
                self.ex(g, pid, "respond_negotiation", negotiation_id=nid, action="reply",
                        message="Words are wind. Make a concrete proposal.")
            return
        if n["proposal_by"] == pid:
            self.ex(g, pid, "respond_negotiation", negotiation_id=nid, action="reply", message="Our offer stands.")
            return
        give = n["proposal"].get(str(pid), [])
        receive = n["proposal"].get(str(other), [])
        value = self.evaluate(g, pid, other, give, receive)
        if value >= 0:
            if self.ex(g, pid, "respond_negotiation", negotiation_id=nid, action="accept", message="Agreed.") is None:
                self.ex(g, pid, "respond_negotiation", negotiation_id=nid, action="reject",
                        message="We cannot fulfil those terms.")
        elif n["exchanges"] < 4 and value > -150 and g.player(other).gold >= -value:
            ask = int(-value) + 10
            self.ex(g, pid, "respond_negotiation", negotiation_id=nid, action="counter",
                    message=f"Add {ask} gold and we have a deal.", give=give,
                    receive=receive + [{"type": "gold", "amount": ask}])
        else:
            self.ex(g, pid, "respond_negotiation", negotiation_id=nid, action="reject", message="That does not interest us.")
