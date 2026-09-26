"""Record the Python engine's answers to the query tools and the other views on the fixture states, for the Rust
port of the views and the briefing (crates/citar-engine DESIGN.md 8.1, packages 1d-02 and 1d-03).

    PYTHONHASHSEED=0 python scripts/refcheck/query_tools.py            # writes refcheck/query_tools.json.gz
    PYTHONHASHSEED=0 python scripts/refcheck/query_tools.py --check    # re-records and compares with the committed file
    PYTHONHASHSEED=0 python scripts/refcheck/query_tools.py --corpus refcheck/corpus --out C:/dev/target/query_tools.json.gz

The refcheck group ``views`` compares what the browser receives (``views.client_view``) for two civilizations of each
state. The query tools build their answers from more of the views than that: a unit's and a city's full detail, a
tile, the tech tree, policies, religion, great people, espionage, city-states and victory. For each state
(refcheck/fixtures-mini and fixtures-late, or a corpus folder), loaded as refcheck loads it, the file holds under the
state's name ``<case>/t<turn>``:

* ``calls``: for the first two living major civilizations, every query tool the views answer, called through
  ``tools.execute`` as a model calls it: the tools without arguments (``get_policies`` for the first only);
  ``get_tech_tree`` with the filters ``all`` and ``known`` for the first, and one that matches nothing for the second;
  ``get_diplomacy`` with a short message limit; ``get_unit`` for each of the civilization's units and ``get_city`` for
  each of its cities (the first 12 of each, by id); ``get_tile`` on 24 tiles drawn from the whole map and on the
  tiles of those units and cities. Each is ``{"pid", "tool", "args"}`` with ``"ok"`` (the answer) or ``"error"``;
* ``god_view``: ``views.client_view(g, None)``, what a spectator's browser receives, without its events and with
  every 25th tile;
* ``facade``: ``EngineGame.empire_summary`` for each of those civilizations, ``standings``, and ``path_preview`` for
  up to six of each one's units toward a tile drawn near it;
* package 1d-03 adds to ``calls`` the ASCII map (``get_map``: around the capital, with the legend, around a unit
  with a small radius, the widest, and off the map), and, on the first state only since the ruleset is the same in
  all, ``get_rules`` for every topic, with a name that resolves loosely and one that does not; and ``maps``: the
  state's terrain as a map (``maps.map_from_game``, with every 25th tile), its summary, and what ``maps.validate``
  says of it; and ``scenario``: ``scenario.overview``, ``default_seats``, and ``normalize_seats`` of a list of seats
  that exercises each of its rules, and of one it refuses.

The Rust test (crates/citar-refcheck/tests/query_tools.rs) loads each state with ``Game::from_python``, asks the same
questions, and compares the answers with refcheck's comparator, the differences explained by id from
refcheck/intended.toml or tests/rules/intended.toml. With CITAR_QUERY_TOOLS set to a recording of a corpus (and
CITAR_REFCHECK_CORPUS to the corpus), it checks that one instead.
"""
from __future__ import annotations

import argparse
import gzip
import json
import random
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import common  # noqa: E402

FIXTURES = [ROOT / "refcheck" / "fixtures-mini", ROOT / "refcheck" / "fixtures-late"]
OUT = ROOT / "refcheck" / "query_tools.json.gz"

# The query tools whose answers the views build, called without arguments.
PLAIN = ["get_units", "get_cities", "get_empire", "get_players", "get_diplomacy", "get_city_states", "get_tech_tree",
         "get_policies", "get_religion", "get_great_people", "get_espionage", "get_victory_status"]
# Asked of the first civilization only: the whole ruleset's techs and policies, the same for each.
HEAVY = {"get_policies"}
# Every this-many-th tile of a spectator's view is kept (the map whole is most of the file).
GOD_TILES = 25
EACH = 12
TILES = 24
PATHS = 6
# get_rules: each topic, then each table topic with a loosely written name and a name it does not have.
RULE_TOPICS = ["units", "buildings", "techs", "improvements", "resources", "promotions", "terrains", "terrain",
               "policies", "beliefs", "specialists", "eras", "nations", "city_state_types", "speeds", "difficulties",
               "deal_items", "combat", "overview", " Units ", "astrology"]
RULE_NAMES = [("units", "warrior"), ("units", "Great Scientist"), ("buildings", "the_great_library"),
              ("techs", "bronze working"), ("improvements", "Farm"), ("resources", "IRON"),
              ("promotions", "Shock I"), ("terrains", "Grassland"), ("terrain", "Hill"), ("policies", "Tradition"),
              ("policies", "legalism"), ("beliefs", "Tithe"), ("specialists", "Scientist"), ("eras", "Medieval era"),
              ("nations", "Rome"), ("city_state_types", "Maritime"), ("city_state_types", "maritime"),
              ("speeds", "quick"), ("difficulties", "Prince"), ("units", "Unicorn"), ("techs", 5)]
# normalize_seats: a list that exercises each rule, and one whose handicap it refuses.
SEATS = [{"type": "llm", "label": "A label much longer than the sixty characters a seat label keeps, cut",
          "llm": {"model": "m", "api_key": "secret"}, "bot": {"style": "x"}, "handicap": "ai",
          "auto": {"un_vote": False}},
         "not a seat", {"type": "nobody", "label": 42}]
BAD_SEATS = [{"handicap": "god"}]


def states(folders):
    """Every fixture file under the folders, by case then turn."""
    out = []
    for folder in folders:
        for case in sorted(p for p in Path(folder).iterdir() if p.is_dir()):
            for f in sorted(case.glob("t*.json.gz"), key=lambda p: int(p.name[1:].split(".")[0])):
                out.append((f"{case.name}/{f.name.split('.')[0]}", f))
    return out


def call(g, pid: int, tool: str, args: dict) -> dict:
    """One tool call's answer, or its refusal."""
    from citar.engine import tools
    from citar.engine.game import ActionError
    e = {"pid": pid, "tool": tool, "args": args}
    try:
        e["ok"] = json.loads(common.dumps(tools.execute(g, pid, tool, dict(args))))
    except ActionError as err:
        e["error"] = str(err)
    return e


def record(name: str, path: Path, first: bool = False) -> dict:
    """One state's answers; the ruleset's own, which every state shares, on the ``first`` only."""
    from citar import engine_api
    from citar.engine import maps, scenario, views
    from citar.engine.game import ActionError
    with gzip.open(path, "rt", encoding="utf-8") as fh:
        doc = json.load(fh)
    g = common.load_game(json.dumps(doc["state"]))
    rng = random.Random(f"query_tools|{name}")
    majors = [p.id for p in g.majors()][:2]
    calls = []
    for n, pid in enumerate(majors):
        for tool in PLAIN:
            if n == 0 or tool not in HEAVY:
                calls.append(call(g, pid, tool, {}))
        for f in ("all", "known") if n == 0 else ("nonsense",):
            calls.append(call(g, pid, "get_tech_tree", {"filter": f}))
        calls.append(call(g, pid, "get_diplomacy", {"message_limit": 3}))
        units = sorted(g.player_units(pid), key=lambda u: u.id)[:EACH]
        for u in units:
            calls.append(call(g, pid, "get_unit", {"unit_id": u.id}))
        cities = sorted(g.player_cities(pid), key=lambda c: c.id)[:EACH]
        for c in cities:
            calls.append(call(g, pid, "get_city", {"city_id": c.id}))
        tiles = sorted(set(rng.sample(range(g.grid.size), min(TILES, g.grid.size)))
                       | {u.idx for u in units} | {c.idx for c in cities})
        for idx in tiles:
            x, y = g.grid.xy(idx)
            calls.append(call(g, pid, "get_tile", {"x": x, "y": y}))
        calls.append(call(g, pid, "get_map", {}))
        if n == 0:
            calls.append(call(g, pid, "get_map", {"legend": True, "radius": 0}))
            calls.append(call(g, pid, "get_map", {"x": g.s.width + 3, "y": 0}))
            calls.append(call(g, pid, "get_map", {"radius": 40}))
        if units:
            x, y = g.grid.xy(units[-1].idx)
            calls.append(call(g, pid, "get_map", {"x": x, "y": y, "radius": 3}))
            calls.append(call(g, pid, "get_map", {"x": x, "radius": 1}))
    if first:
        pid = majors[0]
        for topic in RULE_TOPICS:
            calls.append(call(g, pid, "get_rules", {"topic": topic}))
        for topic, rname in RULE_NAMES:
            calls.append(call(g, pid, "get_rules", {"topic": topic, "name": rname}))
    god = json.loads(common.dumps(views.client_view(g, None)))
    god.pop("events", None)
    god["tiles"] = god["tiles"][::GOD_TILES]
    eg = engine_api.EngineGame(g)
    facade = {"empire_summary": {str(pid): json.loads(common.dumps(eg.empire_summary(pid))) for pid in majors},
              "standings": json.loads(common.dumps({str(k): v for k, v in eg.standings().items()})),
              "path_preview": []}
    for pid in majors:
        for u in sorted(g.player_units(pid), key=lambda u: u.id)[:PATHS]:
            near = g.grid.within(u.idx, 6)
            to = near[rng.randrange(len(near))]
            x, y = g.grid.xy(to)
            facade["path_preview"].append({"pid": pid, "unit": u.id, "x": x, "y": y,
                                           "answer": json.loads(common.dumps(eg.path_preview(pid, u.id, x, y)))})
    exported = maps.map_from_game(g)
    clean, warnings = maps.validate(g.rules, exported)
    sampled = dict(exported)
    sampled["tiles"] = exported["tiles"][::GOD_TILES]
    mapped = {"export": sampled, "summary": maps.summary(exported), "warnings": warnings,
              "round_trip": clean == {**exported, "id": clean["id"]}}
    normalized = []
    for seats in (SEATS, BAD_SEATS):
        try:
            normalized.append({"seats": seats, "ok": scenario.normalize_seats(g, seats)})
        except ActionError as e:
            normalized.append({"seats": seats, "error": str(e)})
    scen = {"overview": json.loads(common.dumps(scenario.overview(g))),
            "default_seats": scenario.default_seats(g), "normalize_seats": normalized}
    return {"calls": calls, "god_view": god, "facade": facade, "maps": mapped, "scenario": scen}


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--check", action="store_true", help="re-record and compare with the committed file")
    ap.add_argument("--corpus", help="a corpus folder to record instead of the committed fixtures")
    ap.add_argument("--out", help="where to write (the committed file by default)")
    a = ap.parse_args(argv)
    folders = [Path(a.corpus)] if a.corpus else FIXTURES
    found = states(folders)
    out = {name: record(name, path, first=(i == 0)) for i, (name, path) in enumerate(found)}
    text = json.dumps(out, separators=(",", ":"), sort_keys=True, ensure_ascii=False) + "\n"
    target = Path(a.out) if a.out else OUT
    if a.check:
        old = gzip.decompress(target.read_bytes()).decode("utf-8") if target.exists() else ""
        if old != text:
            tmp = Path(tempfile.gettempdir()) / "query_tools.json.gz"
            common.write_gz(tmp, text)
            print(f"{target} differs from a fresh recording ({tmp})")
            return 1
        print(f"{target}: unchanged ({len(out)} states)")
        return 0
    common.write_gz(target, text)
    print(f"wrote {target}: {len(out)} states, {sum(len(v['calls']) for v in out.values())} calls")
    return 0


if __name__ == "__main__":
    common.ensure_hash_seed()
    sys.exit(main())
