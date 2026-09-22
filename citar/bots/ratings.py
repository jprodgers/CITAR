"""Ratings for bot profiles, from every recorded game: who beats whom, and how that changes over time.

**What is rated.** An *entry* is a fingerprint (the exact code, parameter overrides and aggression that played; see
``citar.bots.profiles``) at a difficulty level. A Deity bot and a Prince bot running the same code are different
entries - which makes the ladder experiments a handicap scale to place models on.

**How.** Each game's final ranking (by score) is split into pairwise results: every pair of seats with different
entries is one comparison, won by the higher score (equal scores tie). A game of n players contributes each pair
with weight 1/(n-1), so every seat counts about one game's worth. A Bradley-Terry model is fitted to all
comparisons at once (minorisation-maximisation), with a prior of ``PRIOR_GAMES`` drawn games against a 1500-rated
anchor so that an entry with a handful of games stays near 1500 instead of shooting to infinity. Ratings are on the
Elo scale: 400 points is a 10:1 expected win ratio in a head-to-head. The standard error comes from the Fisher
information of the fit, and ignores the correlations between pairs of one game, so treat it as a lower bound.

Fitting everything at once (rather than updating Elo game by game) makes a rating independent of the order games
finished in, which matters when a dozen experiments run in parallel. **History** is the same fit on the games
finished up to the end of each day.

**Where the games come from.** The lab's results (``saves/lab/results``). Results from before fingerprints were
recorded are mapped through their experiment's seat list (engine, parameters, aggression by label).
"""
from __future__ import annotations

import json
import math
import statistics
from collections import Counter, defaultdict
from datetime import datetime
from typing import Optional

from . import profiles

PRIOR_GAMES = 2.0            # drawn games against a 1500 anchor every entry starts with
ELO_SCALE = 400 / math.log(10)
_cache: dict = {}


# ----------------------------------------------------------------------------------------------------------------------
# collecting games
# ----------------------------------------------------------------------------------------------------------------------
def _lab_files():
    """(experiment spec, results path) for every lab experiment, queued or done."""
    from .. import lab
    out = []
    for d in (lab.QUEUE, lab.DONE):
        if not d.exists():
            continue
        for p in d.glob("*.json"):
            try:
                spec = json.loads(p.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError):
                continue
            out.append((spec, lab.RESULTS / f"{spec.get('name')}.jsonl"))
    return out


def _signature() -> tuple:
    """Changes whenever a result, an experiment or a profile changes: the cache key."""
    from .. import lab
    sig = []
    for d in (lab.QUEUE, lab.DONE, lab.RESULTS, profiles.PROFILES_DIR):
        if d.exists():
            for p in d.iterdir():
                try:
                    st = p.stat()
                    sig.append((p.name, st.st_mtime_ns, st.st_size))
                except OSError:
                    pass
    return tuple(sorted(sig))


def _seat_identity(exp: dict, player: dict) -> tuple:
    """(code, overrides, aggression) of a result's seat, from the result itself or its experiment's seat list."""
    if exp.get("factors"):
        params = dict(exp.get("base_params") or {})
        params.update(player.get("levels") or {})
        return player.get("bot") or exp.get("bot") or "basic", params, exp.get("aggression")
    seat = next((s for s in exp.get("seats", []) if s.get("label") == player.get("label")), None) or {}
    return player.get("bot") or seat.get("bot") or "basic", seat.get("params") or {}, seat.get("aggression")


def collect() -> tuple[list[dict], dict]:
    """Every rated game (oldest first) and what is known about each entry that played."""
    games, entries = [], {}
    fp_cache: dict = {}
    for exp, path in _lab_files():
        if not path.exists():
            continue
        factorial = bool(exp.get("factors"))
        from .. import lab
        for line in path.read_text(encoding="utf-8").splitlines():
            try:
                r = lab.complete_players(exp, json.loads(line))
            except (json.JSONDecodeError, KeyError, TypeError, ValueError):
                continue
            if r.get("crash") or not r.get("players") or len(r["players"]) < 2:
                continue
            seats = []
            for k, pl in r["players"].items():
                code, params, agg = _seat_identity(exp, pl)
                fp = pl.get("fingerprint")
                if not fp:
                    key = (code, json.dumps(params, sort_keys=True, default=str), agg)
                    if key not in fp_cache:
                        fp_cache[key] = "idle" if code == "idle" else profiles.fingerprint(code, params, agg)
                    fp = fp_cache[key]
                diff = pl.get("difficulty") or exp.get("difficulty") or "Prince"
                eid = f"{fp}@{diff}"
                e = entries.get(eid)
                if e is None:
                    e = entries[eid] = {"id": eid, "fingerprint": fp, "difficulty": diff, "code": code,
                                        "params": params, "aggression": agg, "labels": Counter(), "experiments": set(),
                                        "profile": pl.get("profile"), "profile_rev": pl.get("profile_rev"),
                                        "factorial": factorial, "first": None, "last": None}
                e["labels"][pl.get("label") or "?"] += 1
                e["experiments"].add(exp["name"])
                e["factorial"] = e["factorial"] and factorial
                if pl.get("profile") and not e.get("profile"):
                    e["profile"], e["profile_rev"] = pl["profile"], pl.get("profile_rev")
                seats.append({"entry": eid, "share": pl.get("score_share") or 0.0, "rank": pl.get("rank"),
                              "alive": pl.get("alive"), "techs": pl.get("techs"), "cities": pl.get("cities"),
                              "captured": (pl.get("events") or {}).get("captured_city", 0),
                              "won": r.get("winner") is not None and str(r.get("winner")) == k,
                              "victory": r.get("victory") if r.get("winner") is not None and str(r.get("winner")) == k
                              else None})
            when = r.get("finished") or exp.get("submitted") or ""
            games.append({"id": f"lab:{exp['name']}:{r.get('i')}", "exp": exp["name"], "finished": when,
                          "turns": r.get("turns"), "map": r.get("map"), "factorial": factorial, "seats": seats})
            for s in seats:
                e = entries[s["entry"]]
                e["first"] = min(e["first"] or when, when)
                e["last"] = max(e["last"] or when, when)
    games.sort(key=lambda g: g["finished"])
    return games, entries


# ----------------------------------------------------------------------------------------------------------------------
# the fit
# ----------------------------------------------------------------------------------------------------------------------
def comparisons(games: list[dict], include=None) -> dict:
    """Weighted pairwise results: {(a, b): [a's wins, games]} with a < b, ties counted half to each side."""
    pairs: dict = defaultdict(lambda: [0.0, 0.0])
    for g in games:
        seats = [s for s in g["seats"] if include is None or s["entry"] in include]
        n = len(g["seats"])
        if n < 2:
            continue
        w = 1.0 / (n - 1)
        for i in range(len(seats)):
            for j in range(i + 1, len(seats)):
                a, b = seats[i], seats[j]
                if a["entry"] == b["entry"]:
                    continue
                x, y = (a, b) if a["entry"] < b["entry"] else (b, a)
                cell = pairs[(x["entry"], y["entry"])]
                cell[1] += w
                if abs(x["share"] - y["share"]) < 1e-9:
                    cell[0] += w / 2
                elif x["share"] > y["share"]:
                    cell[0] += w
    return pairs


def fit(pairs: dict, prior: float = PRIOR_GAMES, iterations: int = 2000) -> dict:
    """Bradley-Terry strengths by MM, with every entry also having drawn `prior` games against a fixed anchor of
    strength 1 (rating 1500). Returns {entry: (rating, standard error)}."""
    ids = sorted({e for k in pairs for e in k})
    if not ids:
        return {}
    wins = defaultdict(float)
    opp = defaultdict(list)            # entry -> [(other, games)]
    for (a, b), (wa, n) in pairs.items():
        wins[a] += wa
        wins[b] += n - wa
        opp[a].append((b, n))
        opp[b].append((a, n))
    gamma = dict.fromkeys(ids, 1.0)
    for _ in range(iterations):
        delta = 0.0
        new = {}
        for e in ids:
            denom = sum(n / (gamma[e] + gamma[o]) for o, n in opp[e]) + prior / (gamma[e] + 1.0)
            v = (wins[e] + prior / 2) / denom
            new[e] = v
            delta = max(delta, abs(math.log(v) - math.log(gamma[e])))
        gamma = new
        if delta < 1e-10:
            break
    out = {}
    for e in ids:
        info = sum(n * gamma[e] * gamma[o] / (gamma[e] + gamma[o]) ** 2 for o, n in opp[e]) \
            + prior * gamma[e] / (gamma[e] + 1.0) ** 2
        out[e] = (1500 + ELO_SCALE * math.log(gamma[e]), ELO_SCALE / math.sqrt(info) if info > 0 else None)
    return out


def expected(ra: float, rb: float) -> float:
    """Chance that a player rated ra finishes ahead of one rated rb."""
    return 1 / (1 + 10 ** ((rb - ra) / 400))


# ----------------------------------------------------------------------------------------------------------------------
# presentation
# ----------------------------------------------------------------------------------------------------------------------
def _name(e: dict, revs: dict, pinned: dict, engines: dict) -> tuple:
    """(display name, profile id, revision) for an entry."""
    diff = "" if e["difficulty"] == "Prince" else f" @ {e['difficulty']}"
    if e["fingerprint"] in revs:
        pid, name, rev = revs[e["fingerprint"]]
        return f"{name}{' r' + str(rev) if rev and rev > 1 else ''}{diff}", pid, rev
    if e.get("profile"):
        try:
            p = profiles.get(e["profile"])
            return f"{p['name']} r{e.get('profile_rev') or '?'}{diff}", p["id"], e.get("profile_rev")
        except profiles.ProfileError:
            pass
    code = e["code"]
    when = (engines.get(code) or {}).get("created")
    stamp = f"{code[7:] if code.startswith('frozen_') else code}" + (f", {when[5:10]}" if when else "")
    if not e["params"] and e["aggression"] is None:
        if code in pinned:
            return f"{pinned[code][1]}{diff}", pinned[code][0], 1
        return f"Standard · code {stamp}{diff}", "standard", None       # the live bot as it was then
    label = e["labels"].most_common(1)[0][0] if e["labels"] else "bot"
    return f"{label} · {stamp}{diff}", None, None


def _entry_stats(games: list[dict]) -> dict:
    """Per-entry outcome statistics: games, wins, score share and score index (share x players, 1 = par)."""
    acc = defaultdict(lambda: defaultdict(list))
    for g in games:
        n = len(g["seats"])
        for s in g["seats"]:
            a = acc[s["entry"]]
            a["share"].append(s["share"])
            a["index"].append(s["share"] * n)
            a["won"].append(1.0 if s["won"] else 0.0)
            a["first"].append(1.0 if s["rank"] == 0 else 0.0)
            a["alive"].append(1.0 if s["alive"] else 0.0)
            for k in ("techs", "cities", "captured"):
                if s.get(k) is not None:
                    a[k].append(s[k])
            if s["victory"]:
                a["victories"].append(s["victory"])
    out = {}
    for eid, a in acc.items():
        out[eid] = {"seats": len(a["share"]), "win_rate": round(statistics.mean(a["won"]), 3),
                    "first_rate": round(statistics.mean(a["first"]), 3),
                    "score_share": round(statistics.mean(a["share"]), 4),
                    "score_index": round(statistics.mean(a["index"]), 3),
                    "survival": round(statistics.mean(a["alive"]), 3),
                    **{k: round(statistics.mean(a[k]), 1) for k in ("techs", "cities", "captured") if a[k]},
                    "victories": dict(Counter(a["victories"]))}
    return out


def rankings(include_factorial: bool = False, history: bool = True) -> dict:
    """The leaderboard, rating history and head-to-head records, computed from every recorded game (cached until
    a result, an experiment or a profile changes)."""
    key = (_signature(), include_factorial, history)
    if _cache.get("key") == key:
        return _cache["value"]
    games, entries = collect()
    rated = [g for g in games if include_factorial or not g["factorial"]]
    keep = {s["entry"] for g in rated for s in g["seats"]}
    pairs = comparisons(rated, keep)
    ratings = fit(pairs)
    compared = Counter()
    for (a, b), (_, n) in pairs.items():
        compared[a] += n
        compared[b] += n
    stats = _entry_stats(rated)
    revs = profiles.revision_fingerprints()
    pinned = {p["engine"]: (p["id"], p["name"]) for p in profiles.BUILTIN if p["engine"].startswith("frozen_")
              and not p["params"] and p["aggression"] is None}
    engines = {e["id"]: e for e in profiles.engines()}
    board = []
    for eid in keep:
        e = entries[eid]
        name, pid, rev = _name(e, revs, pinned, engines)
        r, se = ratings.get(eid, (1500.0, None))
        board.append({"id": eid, "name": name, "profile": pid, "rev": rev, "fingerprint": e["fingerprint"],
                      "difficulty": e["difficulty"], "code": e["code"], "params": e["params"],
                      "aggression": e["aggression"], "labels": dict(e["labels"].most_common(5)),
                      "experiments": sorted(e["experiments"]), "factorial": e["factorial"],
                      "first": e["first"], "last": e["last"], "rating": round(r, 1),
                      "se": round(se, 1) if se else None, "compared": round(compared[eid], 2),
                      "rated": compared[eid] > 0, **stats.get(eid, {})})
    board.sort(key=lambda b: (not b["rated"], -b["rating"]))
    for i, b in enumerate(board):
        b["rank"] = i + 1
    hist = _history(rated, keep) if history else {}
    value = {"generated": datetime.now().isoformat(timespec="seconds"), "games": len(rated),
             "entries": board, "history": hist, "head_to_head": _head_to_head(rated, keep),
             "method": {"model": "Bradley-Terry on pairwise finishing order, Elo scale", "prior_games": PRIOR_GAMES,
                        "pair_weight": "1/(players-1)"}}
    _cache.update(key=key, value=value)
    return value


def _history(games: list[dict], keep: set) -> dict:
    """entry -> [(day, rating, se, games so far)] from refitting on the games finished by the end of each day."""
    days = sorted({g["finished"][:10] for g in games if g["finished"]})
    out = defaultdict(list)
    played = Counter()
    i = 0
    for day in days:
        while i < len(games) and games[i]["finished"][:10] <= day:
            for s in games[i]["seats"]:
                played[s["entry"]] += 1
            i += 1
        upto = games[:i]
        for eid, (r, se) in fit(comparisons(upto, keep)).items():
            out[eid].append([day, round(r, 1), round(se, 1) if se else None, played[eid]])
    return dict(out)


def _head_to_head(games: list[dict], keep: set) -> dict:
    """"a|b" -> games together, a ahead, b ahead, ties, mean score-share difference (a - b)."""
    acc = defaultdict(lambda: {"games": 0, "a": 0, "b": 0, "ties": 0, "diff": []})
    for g in games:
        by = defaultdict(list)
        for s in g["seats"]:
            if s["entry"] in keep:
                by[s["entry"]].append(s["share"])
        ids = sorted(by)
        for i in range(len(ids)):
            for j in range(i + 1, len(ids)):
                a, b = ids[i], ids[j]
                ma, mb = statistics.mean(by[a]), statistics.mean(by[b])
                c = acc[f"{a}|{b}"]
                c["games"] += 1
                c["diff"].append(ma - mb)
                if abs(ma - mb) < 1e-9:
                    c["ties"] += 1
                elif ma > mb:
                    c["a"] += 1
                else:
                    c["b"] += 1
    out = {}
    for k, c in acc.items():
        d = c.pop("diff")
        m = statistics.mean(d)
        ci = 1.96 * statistics.stdev(d) / math.sqrt(len(d)) if len(d) > 1 else None
        out[k] = {**c, "share_diff": round(m, 4), "ci95": round(ci, 4) if ci else None,
                  "significant": bool(ci and abs(m) > ci)}
    return out


def profile_summary(pid: str, board: Optional[list] = None) -> dict:
    """A profile's rating entries (all revisions, all difficulties), best first."""
    board = board if board is not None else rankings()["entries"]
    mine = [b for b in board if b.get("profile") == pid]
    return {"profile": pid, "entries": mine, "best": mine[0] if mine else None}


def profile_rating(p: dict, board: list) -> dict:
    """A profile with its rating, the most relevant Prince entry first:

    * exactly the current revision (same fingerprint): ``rating_is_current``;
    * else the same settings (parameters and aggression) on earlier code of the live bot - the profile's own
      rating from before the last code change, ``rating_same_settings``, the latest such code first;
    * else its best rated entry from an earlier revision (flagged by both being false).
    """
    mine = [b for b in board if b.get("profile") == p["id"]]
    rated = [b for b in mine if b.get("rated")]
    best = ([b for b in rated if b["difficulty"] == "Prince"] or rated or [None])[0]
    try:
        live = profiles.fingerprint(p["engine"], p.get("params"), p.get("aggression"))
    except profiles.ProfileError:
        live = None
    current = next((b for b in mine if b["fingerprint"] == live and b["difficulty"] == "Prince" and b.get("rated")),
                   None)
    same = None
    if current is None and p.get("engine") == "basic":
        agg = p.get("aggression")
        cands = [b for b in rated if b["difficulty"] == "Prince" and (b.get("params") or {}) == (p.get("params") or {})
                 and (b.get("aggression") is None and agg is None
                      or b.get("aggression") is not None and agg is not None and abs(b["aggression"] - agg) < 1e-6)]
        same = max(cands, key=lambda b: b.get("last") or "", default=None)
    return {**p, "fingerprint": live, "rating": current or same or best,
            "rating_is_current": current is not None, "rating_same_settings": current is not None or same is not None,
            "entries": len(mine)}


def ranked_profiles(board: Optional[list] = None) -> list[dict]:
    """Every profile with its rating, best first: rated profiles by rating (a rating from the current revision
    counts the same as one from an earlier version, which is flagged), then unrated ones with Standard first."""
    board = board if board is not None else rankings(history=False)["entries"]
    out = [profile_rating(p, board) for p in profiles.list_profiles()]

    def key(p):
        r = p["rating"]
        if r and r.get("rated"):
            return (0, -r["rating"], p["name"].lower())
        return (1, 0 if p["id"] == profiles.DEFAULT_PROFILE else 1, p["name"].lower())
    out.sort(key=key)
    for i, p in enumerate(out):
        p["rank"] = i + 1 if p["rating"] and p["rating"].get("rated") else None
    return out


def best_profile(board: Optional[list] = None) -> str:
    """The id "Best bot" stands for: the highest-rated profile whose rating describes its current settings (its
    current revision, or the same settings on earlier code of the live bot), else the highest-rated at all, else
    Standard. Never the idle bot or an archived profile."""
    try:
        ranked = [p for p in ranked_profiles(board) if p["engine"] != "idle" and not p.get("archived")
                  and p["rating"] and p["rating"].get("rated")]
    except Exception:           # ratings must never stop a game from being created
        return profiles.DEFAULT_PROFILE
    current = [p for p in ranked if p["rating_same_settings"]]
    return (current or ranked or [{"id": profiles.DEFAULT_PROFILE}])[0]["id"]
