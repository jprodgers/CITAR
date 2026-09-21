"""Religion: pantheons, founding & enhancing religions, pressure/followers, spreading, great prophets.
Port of UnCiv's ReligionManager, CityReligionManager, Religion and UnitActionsReligion (MPL-2.0)."""
from __future__ import annotations

from typing import Optional, TYPE_CHECKING

from . import unique_types as U
from .game import ActionError
from .uniques import Ctx, UniqueMap, applies, city_matches, STAT_KEY

if TYPE_CHECKING:
    from .game import Game

NONE = "None"
STATE_ORDER = ["none", "pantheon", "founding", "religion", "enhancing", "enhanced"]


def state_ge(p, s: str) -> bool:
    """Whether a player has reached at least this stage of religion: pantheon, religion, enhanced."""
    return STATE_ORDER.index(p.religion_state) >= STATE_ORDER.index(s)


# ---------------------------------------------------------------------------------------------------------------
# Religion objects
# ---------------------------------------------------------------------------------------------------------------
def rel(g: "Game", name: Optional[str]) -> Optional[dict]:
    """A religion by name, or None."""
    return g.s.religions.get(name) if name else None


def is_major(g: "Game", name: Optional[str]) -> bool:
    """Whether this is a full religion rather than a pantheon - it has a founder belief."""
    r = rel(g, name)
    return bool(r) and any(g.rules.beliefs[b]["type"] == "Founder" for b in r["founder_beliefs"])


def is_enhanced(g: "Game", name: Optional[str]) -> bool:
    """Whether this religion has been enhanced with a second pair of beliefs."""
    r = rel(g, name)
    return bool(r) and any(g.rules.beliefs[b]["type"] == "Enhancer" for b in r["founder_beliefs"])


def all_beliefs(g: "Game", name: str) -> list[str]:
    """Every belief a religion holds, founder and follower together."""
    r = rel(g, name)
    if not r:
        return []
    B = g.rules.beliefs
    order = {"Pantheon": 0, "Founder": 1, "Follower": 2, "Enhancer": 3}
    return sorted(r["founder_beliefs"] + r["follower_beliefs"], key=lambda b: order.get(B[b]["type"], 9))


def display_name(g: "Game", name: Optional[str]) -> Optional[str]:
    """The name a religion is shown under, which its founder may have chosen."""
    r = rel(g, name)
    return (r.get("display") or name) if r else None


def founder_umap(g: "Game", name: str) -> Optional[UniqueMap]:
    """The uniques a religion grants its founder, cached."""
    key = ("founder_umap", name)
    if key in g._ycache:
        return g._ycache[key]
    r = rel(g, name)
    m = None
    if r and r["founder_beliefs"]:
        m = UniqueMap(u for b in r["founder_beliefs"] for u in g.rules.beliefs[b]["_umap"].all)
    g._ycache[key] = m
    return m


def follower_umap(g: "Game", name: str) -> Optional[UniqueMap]:
    """The uniques a religion grants in cities that follow it, cached."""
    key = ("follower_umap", name)
    if key in g._ycache:
        return g._ycache[key]
    r = rel(g, name)
    m = None
    if r and r["follower_beliefs"]:
        m = UniqueMap(u for b in r["follower_beliefs"] for u in g.rules.beliefs[b]["_umap"].all)
    g._ycache[key] = m
    return m


def civ_beliefs(g: "Game", pid: int) -> list[str]:
    """The beliefs of this civilization's own religion."""
    p = g.player(pid)
    return all_beliefs(g, p.religion) if p.religion else []


def beliefs_taken(g: "Game") -> set:
    """Every belief already claimed by somebody, which is what makes them a race."""
    return {b for r in g.s.religions.values() for b in r["founder_beliefs"] + r["follower_beliefs"]}


def beliefs_available(g: "Game", btype: str) -> list[str]:
    """Beliefs of a type that nobody has taken yet."""
    taken = beliefs_taken(g)
    return [b for b, d in g.rules.beliefs.items() if (btype == "Any" or d["type"] == btype) and b not in taken]


# ---------------------------------------------------------------------------------------------------------------
# Cities: pressures & followers
# ---------------------------------------------------------------------------------------------------------------
def _pressures(city) -> dict:
    """The city's religious pressure by religion, seeded with "none" on first use."""
    if not city.pressures:
        city.pressures[NONE] = 100
    return city.pressures


def followers(g: "Game", city) -> dict:
    """religion -> followers (excluding 'no religion')."""
    pr = _pressures(city)
    out: dict = {}
    if city.pop <= 0:
        return out
    total = sum(pr.values())
    per = total / city.pop if city.pop else 1
    rem = {}
    for r, v in pr.items():
        n = int(v / per) if per else 0
        out[r] = out.get(r, 0) + n
        rem[r] = v - n * per
    left = city.pop - sum(out.values())
    while left > 0:
        if not rem:
            out[NONE] = out.get(NONE, 0) + left
            break
        best = max(rem, key=lambda k: rem[k])
        out[best] = out.get(best, 0) + 1
        rem[best] = 0.0
        left -= 1
    out.pop(NONE, None)
    return {k: v for k, v in out.items() if v > 0}


def majority_religion(g: "Game", city) -> Optional[str]:
    """The religion most of this city's citizens follow, if any.

    This is the religion that actually does anything: follower beliefs apply to the majority religion
    of a city, not to every religion present in it.
    """
    if not g.religion_enabled:
        return None
    f = followers(g, city)
    if not f:
        return None
    best = max(f, key=lambda k: f[k])
    return best if f[best] >= city.pop / 2 else None


def followers_of_majority(g: "Game", city) -> int:
    """How many citizens follow the city's majority religion."""
    m = majority_religion(g, city)
    return followers(g, city).get(m, 0) if m else 0


def city_follower_umap(g: "Game", city) -> Optional[UniqueMap]:
    """The follower uniques active in this city, from its majority religion."""
    m = majority_religion(g, city)
    return follower_umap(g, m) if m else None


def add_pressure(g: "Game", city, religion: str, amount: int, update: bool = True):
    """Add religious pressure to a city and, unless told not to, recompute its followers.

    The ``update`` flag exists for bulk work: a spread that touches twenty cities should recompute once
    per city, not once per unit of pressure.
    """
    if not g.religion_enabled:
        return
    before = majority_religion(g, city) if update else None
    pr = _pressures(city)
    pr[religion] = pr.get(religion, 0) + int(amount)
    if update:
        _after_update(g, city, before)


def _after_update(g, city, before):
    """Announce a change of majority religion, and apply what follows from it."""
    after = majority_religion(g, city)
    if after != before:
        g.invalidate()
        if after is not None:
            _on_adoption(g, city, after)


def _on_adoption(g, city, religion):
    """Handle a city adopting a religion: the notification and the founder's benefits."""
    r = rel(g, religion)
    g.emit("religion", f"{city.name} converted to {display_name(g, religion)}.", [city.owner], idx=city.idx)
    if religion in city.religions_adopted or not r:
        return
    founder = r["founder"]
    total: dict = {}
    for u in g.civ_uniques(founder, U.StatsWhenAdoptingReligion):
        mult = g.speed["modifier"] if u.has_mod("(modified by game speed)") else 1
        for k, v in u.stats.items():
            total[k] = total.get(k, 0) + v * mult
    for k, v in total.items():
        g.add_stat(founder, k, int(v))
    city.religions_adopted.append(religion)


def remove_all_except(g: "Game", city, religion: str):
    """Wipe out every other religion's pressure in a city, as an inquisitor does."""
    pr = _pressures(city)
    before = majority_religion(g, city)
    keep = pr.get(religion, 0)
    none = pr.get(NONE, 0)
    city.pressures = {NONE: 100}
    city.pressures[religion] = keep
    if none:
        city.pressures[NONE] = none
    _after_update(g, city, before)


def on_population_change(g: "Game", city, delta: int):
    """Adjust a city's followers when it grows or shrinks.

    New citizens follow the majority, which is why a religion that takes a city keeps it as the city
    grows.
    """
    if delta > 0:
        m = majority_religion(g, city) or NONE
        add_pressure(g, city, m, 100 * delta)


def remove_unknown_pantheons(g: "Game", city):
    """Drop pressure from pantheons that no longer exist."""
    for r in list(_pressures(city)):
        if r == NONE:
            continue
        rr = rel(g, r)
        if rr and not is_major(g, r) and rr["founder"] != city.owner:
            del city.pressures[r]
    g.invalidate()


def _spread_range(g, city) -> int:
    """How far this city's religion reaches, before distance reduces it."""
    from .cities import city_uniques
    rng_ = 10 + sum(int(u.n(0)) for u in city_uniques(g, city, U.ReligionSpreadDistance))
    m = majority_religion(g, city)
    if m:
        rng_ += sum(int(u.n(0)) for u in g.civ_uniques(rel(g, m)["founder"], U.ReligionSpreadDistance))
    return rng_


def _pressure_to(g, src, target) -> int:
    """The pressure one city exerts on another, falling with distance."""
    from .cities import city_uniques
    pressure = float(g.speed["religiousPressureAdjacentCity"])
    for u in city_uniques(g, src, U.NaturalReligionSpreadStrength):
        if city_matches(g, target, u.p(1)):
            pressure *= 1 + u.n(0) / 100
    m = majority_religion(g, src)
    if m:
        for u in g.civ_uniques(rel(g, m)["founder"], U.NaturalReligionSpreadStrength):
            if city_matches(g, target, u.p(1)):
                pressure *= 1 + u.n(0) / 100
    return int(pressure)


def pressures_from_surroundings(g: "Game", city) -> dict:
    """Pressure arriving at this city from every religion within range.

    Holy cities exert pressure of their own, which is what makes taking one strategically different
    from taking any other city.
    """
    out: dict = {}
    if city.holy_city_of and not _blocked(g, city):
        out[city.holy_city_of] = 5 * g.speed["religiousPressureAdjacentCity"]
    for other in g.s.cities.values():
        if other.id == city.id:
            continue
        m = majority_religion(g, other)
        if not m or not is_major(g, m):
            continue
        if g.grid.distance(other.idx, city.idx) > _spread_range(g, other):
            continue
        out[m] = out.get(m, 0) + _pressure_to(g, other, city)
    return out


def _blocked(g, city) -> bool:
    """Whether a holy city's pressure has been suppressed."""
    r = rel(g, city.holy_city_of)
    return bool(r and r.get("blocked_holy"))


def city_end_turn(g: "Game", city):
    """Apply one turn of religious pressure to a city and update who follows what."""
    if not g.religion_enabled:
        return
    before = majority_religion(g, city)
    for r, amt in pressures_from_surroundings(g, city).items():
        add_pressure(g, city, r, amt, update=False)
    _after_update(g, city, before)


def is_holy_city(g: "Game", city) -> bool:
    """Whether this city is a religion's holy city and still functioning as one."""
    return city.holy_city_of is not None and not _blocked(g, city)


def protected_by_inquisitor(g: "Game", city, from_religion: Optional[str] = None) -> bool:
    """Whether an inquisitor stands close enough to block a religion from spreading here."""
    for i in g.grid.within(city.idx, 1):
        for u in g.units_at(i):
            if u.religion and (from_religion is None or u.religion != from_religion) and \
                    g.rules.units[u.type]["_umap"].has_tag(U.PreventSpreadingReligion):
                return True
    return False


# ---------------------------------------------------------------------------------------------------------------
# Civ-level
# ---------------------------------------------------------------------------------------------------------------
def cities_following(g: "Game", religion: str) -> int:
    """How many cities have this religion as their majority."""
    return sum(1 for c in g.s.cities.values() if majority_religion(g, c) == religion)


def followers_of(g: "Game", religion: str, city_filter: str, pid: int) -> int:
    """Total followers of a religion in cities matching a filter."""
    return sum(followers(g, c).get(religion, 0) for c in g.s.cities.values() if city_matches(g, c, city_filter, pid))


def faith_for_pantheon(g: "Game", pid: int, additional: int = 0) -> int:
    """Faith needed to found a pantheon, which rises as others are founded."""
    k = g.rules.k
    n = additional + sum(1 for p in g.majors(alive_only=False) if p.religion is not None)
    return int(round((k["pantheon_base"] + n * k["pantheon_growth"]) * g.speed["faithCostModifier"]))


def max_religions(g: "Game") -> int:
    """How many religions this game allows, from the ruleset and the number of players."""
    k = g.rules.k
    return min(len(g.rules.religion_names), k["religion_limit_base"] + int(len(g.majors(alive_only=False)) * k["religion_limit_multiplier"]))


def founded_religions(g: "Game") -> int:
    """How many have been founded so far."""
    return sum(1 for p in g.majors(alive_only=False) if p.religion and state_ge(p, "religion"))


def remaining_foundable(g: "Game") -> int:
    """How many more religions could still be founded, given beliefs and the limit.

    Beliefs run out before the limit does in a crowded game, which is the real constraint late.
    """
    return min(max_religions(g) - founded_religions(g),
               len(beliefs_available(g, "Follower")), len(beliefs_available(g, "Founder")))


def prophet_unit(g: "Game", pid: int) -> Optional[str]:
    """The great prophet unit for this civilization."""
    from .cities import equivalent_unit
    for n, u in g.rules.units.items():
        if u["_umap"].get(U.MayFoundReligion) and not u.get("uniqueTo"):
            return equivalent_unit(g, pid, n)
    return None


def prophets_earned(g: "Game", pid: int) -> int:
    """How many prophets this civilization has already had, which raises the next one's cost."""
    pu = prophet_unit(g, pid)
    return g.player(pid).bought_increasing.get(pu, 0) if pu else 0


def faith_for_next_prophet(g: "Game", pid: int) -> int:
    """Faith needed for the next great prophet."""
    n = prophets_earned(g, pid)
    cost = (200 + 100 * n * (n + 1) / 2) * g.speed["faithCostModifier"]
    for u in g.civ_uniques(pid, U.FaithCostOfGreatProphetChange):
        cost *= 1 + u.n(0) / 100
    return int(cost)


def can_generate_prophet(g: "Game", pid: int, ignore_faith: bool = False) -> bool:
    """Whether this civilization is due a great prophet."""
    p = g.player(pid)
    if not g.religion_enabled or p.kind != "major":
        return False
    if p.religion is None or p.religion_state == "none":
        return False
    if prophet_unit(g, pid) is None:
        return False
    if not ignore_faith and p.faith < faith_for_next_prophet(g, pid):
        return False
    if g.civ_has(pid, U.MayNotGenerateGreatProphet):
        return False
    if p.religion_state == "pantheon" and remaining_foundable(g) == 0:
        return False
    return True


def start_turn(g: "Game", pid: int):
    """Religious events at the start of a turn, chiefly the arrival of a prophet."""
    if can_generate_prophet(g, pid):
        _generate_prophet(g, pid)


def _generate_prophet(g, pid):
    """Create a great prophet and charge the faith it cost."""
    from . import units as unitmod
    p = g.player(pid)
    pu = prophet_unit(g, pid)
    cost = faith_for_next_prophet(g, pid)
    chance = (5 + p.faith - cost) / 100
    if g.state_rng("prophet", g.turn, pid).random() >= chance:
        return
    if not state_ge(p, "religion"):
        birth = g.city(p.capital)
    else:
        hc = holy_city(g, p.religion)
        birth = hc if hc is not None and hc.owner == pid else g.city(p.capital)
    if birth is None:
        return
    u = unitmod.add_unit_in_city(g, birth, pu)
    if u is None:
        return
    unitmod.add_construction_bonuses(g, u, birth)
    u.religion = p.religion
    p.faith -= cost
    p.bought_increasing[pu] = p.bought_increasing.get(pu, 0) + 1
    p.great_prophets_earned += 1
    g.emit("great_person_born", f"A Great Prophet has appeared in {birth.name}!", [pid], idx=birth.idx, unit=u.id)


def holy_city(g: "Game", religion: Optional[str]):
    """The city a religion was founded in, if it still exists."""
    if not religion:
        return None
    for c in g.s.cities.values():
        if c.holy_city_of == religion and not _blocked(g, c):
            return c
    return None


def end_turn(g: "Game", pid: int, faith: float):
    """Add this turn's faith and handle anything it pays for."""
    g.player(pid).faith += int(faith)


# ---------------------------------------------------------------------------------------------------------------
# Actions
# ---------------------------------------------------------------------------------------------------------------
def can_found_pantheon(g: "Game", pid: int) -> Optional[str]:
    """Why this civilization cannot found a pantheon, or None."""
    p = g.player(pid)
    if not g.religion_enabled:
        return "Religion is disabled in this game."
    if p.kind != "major":
        return "Only major civilizations may found pantheons."
    if STATE_ORDER.index(p.religion_state) > STATE_ORDER.index("pantheon"):
        return "You have already founded a religion."
    if not beliefs_available(g, "Pantheon"):
        return "No pantheon beliefs remain."
    if any(q.religion_state == "enhanced" for q in g.majors()) and \
            sum(1 for q in g.majors() if q.religion_state != "none") >= max_religions(g):
        return "No more pantheons can be founded."
    free = p.flags.get("free_beliefs", {}).get("Pantheon", 0)
    if p.religion_state == "none" and p.faith >= faith_for_pantheon(g, pid):
        return None
    if free > 0:
        return None
    return f"A pantheon costs {faith_for_pantheon(g, pid)} faith; you have {int(p.faith)}."


def _new_religion(g, name, founder, display=None):
    """Create the religion record itself."""
    g.s.religions[name] = {"name": name, "display": display or name, "founder": founder,
                           "founder_beliefs": [], "follower_beliefs": [], "blocked_holy": False}
    return g.s.religions[name]


def _add_beliefs(g, r, beliefs):
    """Attach beliefs to a religion and apply whatever they trigger."""
    for b in beliefs:
        t = g.rules.beliefs[b]["type"]
        if t in ("Founder", "Enhancer"):
            if b not in r["founder_beliefs"]:
                r["founder_beliefs"].append(b)
        elif b not in r["follower_beliefs"]:
            r["follower_beliefs"].append(b)
    g.invalidate()


def found_pantheon(g: "Game", pid: int, belief: str) -> dict:
    """Found a pantheon by choosing a belief."""
    reason = can_found_pantheon(g, pid)
    if reason:
        raise ActionError(reason)
    b = g.rules.resolve("belief", belief)
    if b is None or g.rules.beliefs[b]["type"] != "Pantheon":
        raise ActionError(f"'{belief}' is not a pantheon belief. Available: {', '.join(beliefs_available(g, 'Pantheon'))}")
    if b not in beliefs_available(g, "Pantheon"):
        raise ActionError(f"{b} has already been chosen by another civilization.")
    p = g.player(pid)
    free = p.flags.setdefault("free_beliefs", {})
    if free.get("Pantheon", 0) > 0 and not (p.religion_state == "none" and p.faith >= faith_for_pantheon(g, pid)):
        free["Pantheon"] -= 1
    else:
        p.faith -= faith_for_pantheon(g, pid)
    if p.religion_state == "none":
        r = _new_religion(g, b, pid)
        p.religion = b
        for c in g.player_cities(pid):
            add_pressure(g, c, b, 200 * c.pop)
    else:
        r = rel(g, p.religion)
    _add_beliefs(g, r, [b])
    if p.religion_state == "none":
        p.religion_state = "pantheon"
        from . import triggers
        triggers.fire(g, pid, U.TriggerUponFoundingPantheon)
    _belief_triggers(g, pid, [b])
    g.emit("pantheon", f"{p.name} founded the {b} pantheon.", None, player=pid, belief=b)
    return {"pantheon": b, "faith_left": int(p.faith)}


def _belief_triggers(g, pid, beliefs):
    """Fire the one-off effects some beliefs have when taken."""
    from . import triggers
    ctx = Ctx(g, civ=pid)
    for b in beliefs:
        for u in g.rules.beliefs[b]["_umap"].all:
            if triggers.has_trigger_conditional(u) or not triggers.is_triggerable(u) or not applies(u, ctx):
                continue
            triggers.trigger(g, u, pid)
        triggers.fire(g, pid, U.TriggerUponAdoptingPolicyOrBelief, filt=lambda u, b=b: u.p(0) == b)


def beliefs_to_choose(g: "Game", pid: int, enhancing: bool) -> dict:
    """How many beliefs of each type this civilization must pick now.

    The counts differ between founding and enhancing, and uniques can add extra choices, so this is
    computed rather than fixed.
    """
    p = g.player(pid)
    avail = {t: len(beliefs_available(g, t)) for t in ("Pantheon", "Founder", "Follower", "Enhancer")}
    avail["Any"] = len(beliefs_available(g, "Any"))
    out: dict = {}

    def take(t, n):
        """Claim up to *n* beliefs of a type, limited by what remains."""
        k = min(n, avail[t])
        if k <= 0:
            return
        out[t] = out.get(t, 0) + k
        avail[t] -= k
        if t != "Any":
            avail["Any"] -= k

    action = "enhancing" if enhancing else "founding"
    if enhancing:
        take("Enhancer", 1)
    else:
        take("Founder", 1)
        if p.flags.get("choose_pantheon_belief"):
            take("Pantheon", 1)
    take("Follower", 1)
    for u in g.civ_uniques(pid, U.FreeExtraBeliefs):
        if u.p(2) == action:
            take(u.p(1), int(u.n(0)))
    for u in g.civ_uniques(pid, U.FreeExtraAnyBeliefs):
        if u.p(1) == action:
            take("Any", int(u.n(0)))
    for t, n in p.flags.get("free_beliefs", {}).items():
        if n > 0:
            take(t, n)
    return out


def _validate_choice(g, pid, beliefs, needed: dict) -> list[str]:
    """Check that a set of chosen beliefs is legal, or raise saying why."""
    B = g.rules.beliefs
    resolved = []
    for b in beliefs:
        n = g.rules.resolve("belief", b)
        if n is None:
            raise ActionError(f"Unknown belief '{b}'.")
        resolved.append(n)
    taken = beliefs_taken(g)
    for b in resolved:
        if b in taken:
            raise ActionError(f"{b} has already been chosen by another religion.")
    counts: dict = {}
    for b in resolved:
        counts[B[b]["type"]] = counts.get(B[b]["type"], 0) + 1
    need = dict(needed)
    any_slots = need.pop("Any", 0)
    for t, n in need.items():
        have = counts.get(t, 0)
        if have < n:
            raise ActionError(f"Choose {n} {t} belief(s). Available {t} beliefs: {', '.join(beliefs_available(g, t)[:30])}")
    extra = sum(counts.values()) - sum(need.values())
    if extra > any_slots:
        raise ActionError(f"Too many beliefs: choose exactly {', '.join(f'{n} {t}' for t, n in needed.items())}.")
    if extra < any_slots:
        raise ActionError(f"Choose {any_slots} more belief(s) of any type.")
    return resolved


def can_found_religion(g: "Game", pid: int) -> Optional[str]:
    """Why this civilization cannot found a religion, or None."""
    p = g.player(pid)
    if not g.religion_enabled:
        return "Religion is disabled in this game."
    if state_ge(p, "religion"):
        return "You have already founded a religion."
    if p.kind != "major":
        return "Only major civilizations may found religions."
    if remaining_foundable(g) == 0:
        return "No more religions can be founded."
    return None


def found_religion(g: "Game", pid: int, unit, name: str, beliefs: list, display: Optional[str] = None) -> dict:
    """Use a Great Prophet (in one of your cities) to found a religion with the chosen beliefs."""
    from . import units as unitmod
    reason = can_found_religion(g, pid)
    if reason:
        raise ActionError(reason)
    city = g.city_at(unit.idx)
    if city is None or city.owner != pid:
        raise ActionError("A religion must be founded in one of your cities (move the Great Prophet into one).")
    if is_holy_city(g, city):
        raise ActionError(f"{city.name} is already a holy city.")
    uu = unitmod.usable_action(g, unit, U.MayFoundReligion)
    if uu is None:
        raise ActionError("This unit cannot found a religion.")
    rname = next((r for r in g.rules.religion_names if r.lower() == str(name).strip().lower()), None)
    used = set(g.s.religions)
    if rname in used:
        raise ActionError(f"{rname} has already been founded.")
    if rname is None:
        # any name is allowed: it becomes the display name of the next free religion slot
        free = [r for r in g.rules.religion_names if r not in used]
        if not free:
            raise ActionError("No more religions can be founded.")
        rname, display = free[0], (display or str(name).strip()[:40] or None)
    p = g.player(pid)
    if p.religion_state == "none":
        p.flags["choose_pantheon_belief"] = True
    needed = beliefs_to_choose(g, pid, False)
    chosen = _validate_choice(g, pid, beliefs, needed)
    old = rel(g, p.religion)
    r = _new_religion(g, rname, pid, display=(display or rname)[:40])
    if old:
        _add_beliefs(g, r, old["founder_beliefs"] + old["follower_beliefs"])
    _add_beliefs(g, r, chosen)
    p.religion = rname
    p.religion_state = "religion"
    p.flags["free_beliefs"] = {}
    p.flags.pop("choose_pantheon_belief", None)
    city.holy_city_of = rname
    add_pressure(g, city, rname, city.pop * 500)
    for x in g.player_units(pid):
        if g.rules.units[x.type]["_umap"].has_tag(U.ReligiousUnit) and g.rules.units[x.type]["_umap"].has_tag(U.TakeReligionOverBirthCity):
            x.religion = rname
    unitmod.consume_action(g, unit, uu)
    from . import triggers
    triggers.fire(g, pid, U.TriggerUponFoundingReligion)
    _belief_triggers(g, pid, chosen)
    g.emit("religion_founded", f"{p.name} founded {r['display']} in {city.name}!", None, idx=city.idx, player=pid,
           religion=rname)
    return {"founded": r["display"], "religion": rname, "beliefs": all_beliefs(g, rname), "holy_city": city.name}


def enhance_religion(g: "Game", pid: int, unit, beliefs: list) -> dict:
    """Enhance an existing religion with a second pair of beliefs."""
    from . import units as unitmod
    p = g.player(pid)
    if not g.religion_enabled or p.religion_state != "religion":
        raise ActionError("You can only enhance a founded (not yet enhanced) religion.")
    if not beliefs_available(g, "Follower") or not beliefs_available(g, "Enhancer"):
        raise ActionError("No beliefs remain to enhance a religion.")
    if g.city_at(unit.idx) is None:
        raise ActionError("Enhance your religion from a city tile.")
    uu = unitmod.usable_action(g, unit, U.MayEnhanceReligion)
    if uu is None:
        raise ActionError("This unit cannot enhance a religion.")
    needed = beliefs_to_choose(g, pid, True)
    chosen = _validate_choice(g, pid, beliefs, needed)
    r = rel(g, p.religion)
    _add_beliefs(g, r, chosen)
    p.religion_state = "enhanced"
    p.flags["free_beliefs"] = {}
    unitmod.consume_action(g, unit, uu)
    from . import triggers
    triggers.fire(g, pid, U.TriggerUponEnhancingReligion)
    _belief_triggers(g, pid, chosen)
    g.emit("religion_enhanced", f"{p.name} enhanced {r['display']}.", None, player=pid)
    return {"enhanced": r["display"], "beliefs": all_beliefs(g, p.religion)}


def grant_free_belief(g: "Game", pid: int, btype: str) -> bool:
    """Give a civilization a free belief of a type, where a unique grants one."""
    p = g.player(pid)
    free = p.flags.setdefault("free_beliefs", {})
    free[btype] = free.get(btype, 0) + 1
    return True


def spread_pressure(g: "Game", unit) -> int:
    """How much pressure one use of a missionary or prophet applies."""
    from .units import unit_uniques
    pressure = float(g.rules.units[unit.type].get("religiousStrength", 0))
    for u in unit_uniques(g, unit, U.SpreadReligionStrength, with_civ=True):
        pressure *= 1 + u.n(0) / 100
    return int(pressure)


def spread_religion(g: "Game", unit) -> dict:
    """Spread a religion into a city with a missionary or prophet."""
    from . import units as unitmod
    pid = unit.owner
    p = g.player(pid)
    if p.kind != "major" or not g.religion_enabled:
        raise ActionError("Religion cannot be spread.")
    if not unit.religion or not is_major(g, unit.religion):
        raise ActionError("This unit carries no religion.")
    uu = unitmod.usable_action(g, unit, U.CanSpreadReligion)
    if uu is None:
        raise ActionError("This unit cannot spread religion (no uses or movement left).")
    t = g.s.tiles[unit.idx]
    city = g.city(t.city) if t.city is not None else None
    if city is None:
        raise ActionError("Move into a city's territory to spread religion.")
    if majority_religion(g, city) == unit.religion:
        raise ActionError(f"{city.name} already follows {display_name(g, unit.religion)}.")
    if protected_by_inquisitor(g, city, unit.religion):
        raise ActionError(f"An inquisitor protects {city.name}.")
    others = sum(v for k, v in followers(g, city).items() if k != unit.religion)
    for u in unitmod.unit_uniques(g, unit, U.StatsWhenSpreading, with_civ=True):
        g.add_stat(pid, STAT_KEY.get(u.p(1), u.p(1).lower()), others * u.n(0))
    before = majority_religion(g, city)
    add_pressure(g, city, unit.religion, spread_pressure(g, unit))
    if g.rules.units[unit.type]["_umap"].has_tag(U.RemoveOtherReligions):
        remove_all_except(g, city, unit.religion)
    after = majority_religion(g, city)
    if after != before and after is not None and city.owner != pid:
        g.emit("religion", f"{p.name}'s {unit.type} converted {city.name} to {display_name(g, after)}!",
               [city.owner, pid], idx=city.idx)
    unitmod.consume_action(g, unit, uu)
    return {"spread": display_name(g, unit.religion), "city": city.name, "majority": display_name(g, after)}


def remove_heresy(g: "Game", unit) -> dict:
    """Use an inquisitor to clear other religions out of a city."""
    from . import units as unitmod
    if not unit.religion or not is_major(g, unit.religion):
        raise ActionError("This unit carries no religion.")
    t = g.s.tiles[unit.idx]
    city = g.city(t.city) if t.city is not None else None
    if city is None or city.owner != unit.owner:
        raise ActionError("Inquisitors remove heresy in your own cities' territory.")
    if not any(k != unit.religion and k != NONE for k in _pressures(city)):
        raise ActionError(f"There is no other religion in {city.name}.")
    uu = unitmod.usable_action(g, unit, U.CanRemoveHeresy)
    if uu is None:
        raise ActionError("This unit cannot remove heresy.")
    remove_all_except(g, city, unit.religion)
    if city.holy_city_of:
        r = rel(g, city.holy_city_of)
        if city.holy_city_of != unit.religion and not r.get("blocked_holy"):
            r["blocked_holy"] = True
        elif city.holy_city_of == unit.religion and r.get("blocked_holy"):
            r["blocked_holy"] = False
    unitmod.consume_action(g, unit, uu)
    return {"removed_heresy": city.name}


def ai_choose_beliefs(g: "Game", pid: int, needed: dict) -> list[str]:
    """Simple deterministic belief picker for bots (weights from AI-choice uniques)."""
    out = []
    taken = beliefs_taken(g)
    for t, n in needed.items():
        pool = [b for b in beliefs_available(g, t) if b not in out and b not in taken]
        pool.sort(key=lambda b: (-_belief_weight(g, pid, b), b))
        out.extend(pool[:n])
    return out


def _belief_weight(g, pid, b) -> float:
    """Score a belief for an AI choosing between them."""
    ctx = Ctx(g, civ=pid)
    w = 1.0
    for u in g.rules.beliefs[b]["_umap"].get(U.AiChoiceWeight):
        if applies(u, ctx):
            w *= 1 + u.n(0) / 100
    return w
