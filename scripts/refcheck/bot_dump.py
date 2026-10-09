"""Record the deterministic sub-decisions of the Python bot on the reference states, for the refcheck group
``bot_decisions`` and ``cargo refcheck bot-agreement`` (crates/citar-engine DESIGN.md P2.3.11, packages 2-00b, 2-01b,
2-03 and 2-05).

    PYTHONHASHSEED=0 python scripts/refcheck/bot_dump.py
        # the 12 committed states: writes refcheck/bot_decisions.json.gz (committed)
    PYTHONHASHSEED=0 python scripts/refcheck/bot_dump.py --fixtures refcheck/corpus
        # the local corpus: writes where CITAR_BOT_DUMP says, else refcheck/corpus/bot_decisions.json.gz
    python scripts/refcheck/bot_dump.py --check
        # records the committed states again in memory and compares with the file

The script re-runs itself with PYTHONHASHSEED=0 when it is not set, since a few of the bot's ties fall to the order of
a set of names, and writes gzip with a zero timestamp: two runs give the same bytes.

Each state is loaded afresh. For every living major civilization a fresh ``BasicBot(seed=0)`` with ``tech_noise`` 0, at
the default aggression (0.4) and parameters, is asked each question; its tool calls are recorded instead of made
(``ex`` patched), and each question starts from cleared caches (``Game.clear_static``), as advisor_dump.py's do. The
kinds, by the stage of the port that answers them:

Stage 1 (2-01b): the empire's economy.
  * ``context``: ``BasicBot.context`` (basic.py:777-835): army_target, supply, gpt, hap, era, wars, offense, exposed,
    lux_owned, pending_res, the hostile units and the military;
  * ``tech_values``: ``_tech_value`` of every technology the civilization lacks and can research, in both modes
    (``tech_mode`` classic and potential);
  * ``next_research``: in both modes, ``choose_research`` as if nothing were being researched: the free technology it
    would take (or null) and the first step of the path it would set (or null); and ``preferred_free``, the free
    technology it would take if it held one (``free_techs`` raised to 1 for the question; the mode does not matter):
    the most expensive available, the first of equals in ``available_techs``' order, a question the saved states,
    holding no free technology, never ask;
  * ``empire``: ``empire_choices`` (basic.py:1011-1039) without spies: the first policy it would adopt, the free great
    person it would choose, the pantheon belief it would found (each null when there is none to take);
    ``preferred_policy``, the policy it would adopt if it could afford one (``policies.can_adopt_any`` patched true),
    ``preferred_great_person``, the great person it would choose if it held a free one (``free_great_people`` raised
    to 1), and ``preferred_pantheon``, the belief it would found a pantheon with if it could
    (``religion.can_found_pantheon`` patched to allow a major in a game with religion), so the three are recorded on
    every state and not only on the few turns a civilization can act on them;
  * ``cities``: for each city, its threat (from the context), ``city_defense``, ``in_danger`` and ``needs_garrison``;
  * ``sites``: ``expansion_sites``, best first, as [x, y];
  * ``spare``: ``_spare_units``' unit ids, in order.

Stage 2 (2-03): units and fighting.
  * ``attacks``: for each military land or sea unit but scouts, readied first (the ``ready_unit`` test operation, so
    every unit has its full moves; the civilization's units are put back afterwards), the tile ``_attack_best``
    would attack, [x, y], or null; a fresh bot has no war plan, so no siege city is preferred;
  * ``war_target``: at war with a major civilization, ``_war_target``'s plan: the city and the rally point as [x, y],
    ``advance`` and ``siege_ready``; null in peace or without a target.

Stage 3 (2-05): diplomacy.
  * ``reachable``: ``_reachable_city`` for every other living major, by its id: a city id or null;
  * ``lux_trade``: the negotiations ``trade_luxuries`` would open (``to``, ``give``, ``receive``), in order;
  * ``advice``: ``advice`` without a negotiation, and with each open negotiation the civilization is a party to.
    The states are saved at a turn's start and hold none; with one, only ``deal_value`` changes, and it is
    ``round(evaluate(...), 1)`` of the proposal, which refcheck's ``deal_checks`` holds as ``bot_value``.

The choices are compared as agreement rates over the items where either engine's answer says something (a choice
that is not null, a list that is not empty, a flag that is true; P2.3.11), so a port that never answers cannot pass
on the items where Python did. The script prints each choice's base rate: the items whose answer says something, of
those asked (``CHOICES``).

A question that raises is recorded as ``{"error": ...}`` and counted; the script then exits 1.
"""
from __future__ import annotations

import argparse
import contextlib
import gzip
import json
import os
import sys
import traceback
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import common
from citar.bots.basic import BasicBot, _is_recon, _ud
from citar.engine import policies, religion, research
from citar.engine import testops

OUT = ROOT / "refcheck" / "bot_decisions.json.gz"
FIXTURES = [ROOT / "refcheck" / "fixtures-mini", ROOT / "refcheck" / "fixtures-late"]
ENV = "CITAR_BOT_DUMP"

STAGES = {
    "1": ["context", "tech_values", "next_research", "empire", "cities", "sites", "spare"],
    "2": ["attacks", "war_target"],
    "3": ["reachable", "lux_trade", "advice"],
}
KINDS = [k for kinds in STAGES.values() for k in kinds]
#: the bot every question is asked of: Python's live bot with its noise off (DESIGN.md P2.3.11)
BOT_PARAMS = {"tech_noise": 0}


def states(folders):
    """Every ``<case>/t<turn>.json.gz`` of the folders, by case and turn."""
    out = []
    for folder in folders:
        for case in sorted(p for p in folder.iterdir() if p.is_dir()):
            for f in case.glob("t*.json.gz"):
                out.append((case.name, int(f.name[1:].split(".")[0]), f))
    return sorted(out, key=lambda x: (x[0], x[1]))


class Recorder:
    """A bot whose tool calls are recorded instead of made: each call gives None, as a refusal does."""

    def __init__(self, **params):
        self.bot = BasicBot(seed=0, params=dict(BOT_PARAMS, **params))
        self.calls: list = []
        self.bot.ex = self.ex
        # spies draw from the diplomacy stream, and are not a question here
        self.bot.set_diplomacy({"espionage": "llm"})

    def ex(self, _g, _pid, _tool, **args):
        self.calls.append((_tool, args))
        return None

    def made(self, tool: str) -> list:
        """The arguments of each call of one tool, in order."""
        return [a for t, a in self.calls if t == tool]


def xy(g, idx):
    """A tile as [x, y]."""
    return None if idx is None else list(g.grid.xy(idx))


@contextlib.contextmanager
def any_policy_affordable():
    """``policies.can_adopt_any`` answers True for a civilization with anything adoptable, whatever its culture."""
    real = policies.can_adopt_any
    policies.can_adopt_any = lambda g, pid: g.player(pid).kind == "major" and bool(policies.adoptable_policies(g, pid))
    try:
        yield
    finally:
        policies.can_adopt_any = real


@contextlib.contextmanager
def any_pantheon_affordable():
    """``religion.can_found_pantheon`` allows any major civilization in a game with religion, whatever its faith and
    whether it has a religion already; the bot itself still looks for a belief nobody has taken."""
    real = religion.can_found_pantheon
    religion.can_found_pantheon = lambda g, pid: None if g.religion_enabled and g.player(pid).kind == "major" \
        else "not a question here"
    try:
        yield
    finally:
        religion.can_found_pantheon = real


@contextlib.contextmanager
def holding_one(g, pid, field: str):
    """The civilization holds at least one free pick of a kind (``free_techs``, ``free_great_people``) to choose, and
    its own count is put back after."""
    p = g.player(pid)
    held = getattr(p, field)
    setattr(p, field, max(1, held))
    try:
        yield
    finally:
        setattr(p, field, held)


@contextlib.contextmanager
def nothing_researched():
    """``research.current`` answers None, so choose_research decides as if the queue were empty."""
    real = research.current
    research.current = lambda g, pid: None
    try:
        yield
    finally:
        research.current = real


# ------------------------------------------------------------------------------------------------ stage 1

def context(g, pid):
    ctx = Recorder().bot.context(g, pid)
    return {
        "army_target": ctx["army_target"], "supply": ctx["supply"], "gpt": ctx["gpt"], "hap": ctx["hap"],
        "era": ctx["era"], "wars": sorted(ctx["wars"]), "offense": ctx["offense"],
        "exposed": None if ctx["exposed"] is None else sorted(ctx["exposed"]),
        "lux_owned": sorted(ctx["lux_owned"]), "pending_res": sorted(ctx["pending_res"]),
        "hostile": sorted(u.id for u in ctx["hostile"]), "military": sorted(u.id for u in ctx["military"]),
    }


def tech_values(g, pid):
    out = {}
    for mode in ("classic", "potential"):
        g.clear_static()
        bot = Recorder(tech_mode=mode).bot
        ctx = bot.context(g, pid)
        memo: dict = {}
        out[mode] = {t: bot._tech_value(g, pid, t, ctx, memo) for t in g.rules.techs
                     if not g.has_tech(pid, t) and not research.is_unresearchable(g, pid, t)}
    return out


def next_research(g, pid):
    out = {}
    for mode in ("classic", "potential"):
        g.clear_static()
        rec = Recorder(tech_mode=mode)
        with nothing_researched():
            rec.bot.choose_research(g, pid)
        free = rec.made("choose_free_tech")
        chosen = rec.made("set_research")
        out[mode] = {"free": free[0]["tech"] if free else None, "tech": chosen[0]["tech"] if chosen else None}
    g.clear_static()
    rec = Recorder()
    with holding_one(g, pid, "free_techs"):
        rec.bot.choose_research(g, pid)
    free = rec.made("choose_free_tech")
    out["preferred_free"] = free[0]["tech"] if free else None
    return out


def empire(g, pid):
    rec = Recorder()
    rec.bot.empire_choices(g, pid, rec.bot.context(g, pid))
    policy = rec.made("adopt_policy")
    person = rec.made("choose_great_person")
    pantheon = rec.made("found_pantheon")
    g.clear_static()
    rec = Recorder()
    with any_policy_affordable(), any_pantheon_affordable(), holding_one(g, pid, "free_great_people"):
        rec.bot.empire_choices(g, pid, rec.bot.context(g, pid))
    preferred = rec.made("adopt_policy")
    preferred_person = rec.made("choose_great_person")
    preferred_pantheon = rec.made("found_pantheon")
    return {"policy": policy[0]["policy"] if policy else None,
            "preferred_policy": preferred[0]["policy"] if preferred else None,
            "great_person": person[0]["great_person"] if person else None,
            "preferred_great_person": preferred_person[0]["great_person"] if preferred_person else None,
            "pantheon": pantheon[0]["belief"] if pantheon else None,
            "preferred_pantheon": preferred_pantheon[0]["belief"] if preferred_pantheon else None}


def cities(g, pid):
    bot = Recorder().bot
    ctx = bot.context(g, pid)
    return [{"city": c.id, "threat": ctx["threat"][c.id], "defense": bot.city_defense(g, c),
             "danger": bot.in_danger(g, c, ctx), "garrison": bot.needs_garrison(c, ctx)}
            for c in sorted(ctx["cities"], key=lambda c: c.id)]


def sites(g, pid):
    bot = Recorder().bot
    return [xy(g, i) for i in bot.expansion_sites(g, pid, bot.context(g, pid))]


def spare(g, pid):
    bot = Recorder().bot
    return [u.id for u in bot._spare_units(g, pid, bot.context(g, pid))]


# ------------------------------------------------------------------------------------------------ stage 2

def fighters(g, pid) -> list:
    """The units ``_attack_best`` is asked about: military, on land or at sea, not scouts, by id."""
    return sorted((u for u in g.player_units(pid)
                   if _ud(g, u)["_military"] and not _is_recon(_ud(g, u)) and _ud(g, u)["_domain"] in ("Land", "Water")),
                  key=lambda u: u.id)


#: what the ready_unit test operation changes, and the recording puts back
READY_FIELDS = ("moves", "activity", "goto", "path", "order_wait", "attacks", "acted")


def attacks(g, pid):
    units = fighters(g, pid)
    saved = [(u, {f: getattr(u, f) for f in READY_FIELDS}) for u in units]
    try:
        for u in units:
            testops.apply_one(g, 1, {"op": "ready_unit", "unit": u.id})
        g.clear_static()
        rec = Recorder()
        out = []
        for u in units:
            rec.calls.clear()
            rec.bot._attack_best(g, pid, u)
            made = rec.made("attack")
            out.append({"unit": u.id, "target": [made[0]["x"], made[0]["y"]] if made else None})
        return out
    finally:
        for u, fields in saved:
            for f, v in fields.items():
                setattr(u, f, v)
        g.invalidate()


def war_target(g, pid):
    bot = Recorder().bot
    ctx = bot.context(g, pid)
    if not ctx["wars"] or not ctx["cities"]:
        return None
    plan = bot._war_target(g, pid, ctx)
    if plan is None:
        return None
    return {"city": xy(g, plan["city"]), "rally": xy(g, plan.get("rally")), "advance": bool(plan["advance"]),
            "siege_ready": bool(plan.get("siege_ready"))}


# ------------------------------------------------------------------------------------------------ stage 3

def reachable(g, pid):
    bot = Recorder().bot
    ctx = bot.context(g, pid)
    out = {}
    for q in g.majors():
        if q.id != pid and q.alive:
            c = bot._reachable_city(g, pid, q.id, ctx)
            out[str(q.id)] = None if c is None else c.id
    return out


def lux_trade(g, pid):
    rec = Recorder()
    rec.bot.trade_luxuries(g, pid)
    return [{"to": a["to"], "give": a["give"], "receive": a["receive"]} for a in rec.made("open_negotiation")]


def advice(g, pid):
    bot = Recorder().bot
    out = {"none": bot.advice(g, pid), "negotiations": {}}
    for n in g.s.negotiations:
        if n["status"] == "open" and pid in (n["initiator"], n["responder"]):
            g.clear_static()
            out["negotiations"][str(n["id"])] = bot.advice(g, pid, n["id"])
    return out


ASK = {"context": context, "tech_values": tech_values, "next_research": next_research, "empire": empire,
       "cities": cities, "sites": sites, "spare": spare, "attacks": attacks, "war_target": war_target,
       "reachable": reachable, "lux_trade": lux_trade, "advice": advice}


def record(path: Path, errors: list) -> list:
    """Every kind for every living major of one state."""
    doc = common.read_gz(path)
    g = common.load_game(json.dumps(doc["state"]))
    majors = [p.id for p in g.majors() if p.alive]
    rows = {pid: {"player": pid} for pid in majors}
    # the readied units of stage 2 are put back, but each civilization's attacks are asked after every other
    # question of the state, so nothing else ever sees a readied unit
    order = [k for k in KINDS if k != "attacks"] + ["attacks"]
    for kind in order:
        for pid in majors:
            g.clear_static()
            try:
                rows[pid][kind] = json.loads(common.dumps(ASK[kind](g, pid)))
            except Exception as e:
                errors.append(f"{path.parent.name}/{path.name} player {pid} {kind}: {common.error_text(e)}")
                common.print_trace(errors[-1])
                rows[pid][kind] = {"error": common.error_text(e)}
    return [rows[pid] for pid in majors]


def dump(folders) -> tuple[dict, list]:
    errors: list = []
    out = []
    timer = common.Timer()
    for case, turn, path in states(folders):
        majors = record(path, errors)
        out.append({"case": case, "turn": turn, "majors": majors})
        print(f"{case}/t{turn}: {len(majors)} civilizations ({timer()} s)", flush=True)
    doc = {
        "format": 1,
        "about": "The Python bot's deterministic sub-decisions on each state, for each living major civilization: "
                 "a fresh BasicBot(seed=0) with tech_noise 0, its tool calls recorded instead of made. "
                 "scripts/refcheck/bot_dump.py documents each kind.",
        "stages": STAGES,
        "states": out,
    }
    return doc, errors


def text(doc: dict) -> str:
    return json.dumps(doc, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False) + "\n"


def says_something(v) -> bool:
    """Whether an answer is more than nothing: not null, not empty, and not a table of nothing but those."""
    if isinstance(v, dict):
        return any(says_something(x) for x in v.values())
    return v not in (None, [])


def counts(doc: dict) -> dict:
    """For each kind: the civilizations asked, those whose answer says something, and the items answered (cities,
    sites, spare units, attacking units, reachable cities, trade offers, negotiations advised on)."""
    out = {k: {"asked": 0, "answered": 0, "items": 0} for k in KINDS}
    for s in doc["states"]:
        for m in s["majors"]:
            for k in KINDS:
                v = m.get(k)
                out[k]["asked"] += 1
                out[k]["answered"] += says_something(v)
                if k == "attacks" and isinstance(v, list):
                    out[k]["items"] += sum(1 for a in v if a["target"] is not None)
                elif k == "reachable" and isinstance(v, dict):
                    out[k]["items"] += sum(1 for c in v.values() if c is not None)
                elif k == "advice" and isinstance(v, dict) and "negotiations" in v:
                    out[k]["items"] += len(v["negotiations"])
                elif isinstance(v, list):
                    out[k]["items"] += len(v)
    return out


def _says(v) -> bool:
    """Whether one item's answer says something: a choice that is not null, a list that is not empty, a true flag."""
    return v not in (None, [], False, {})


def _each(m: dict, kind: str):
    """The answer of one kind, or nothing when the question raised."""
    v = m.get(kind)
    return None if isinstance(v, dict) and "error" in v else v


def _advice(m: dict) -> dict:
    """The advice without a negotiation (the states hold none), or nothing when the question raised."""
    return (_each(m, "advice") or {}).get("none") or {}


#: The choices an agreement rate is computed over, with the item each compares (P2.3.11): for one major's answers,
#: the items as (key, answer). Values (tech values, defences, threats, the context's numbers) are compared with a
#: tolerance instead.
CHOICES = {
    "next_research.tech": ("civilization and mode", lambda m: [
        (mode, (_each(m, "next_research") or {}).get(mode, {}).get("tech")) for mode in ("classic", "potential")]),
    "next_research.free": ("civilization and mode", lambda m: [
        (mode, (_each(m, "next_research") or {}).get(mode, {}).get("free")) for mode in ("classic", "potential")]),
    "next_research.preferred_free": ("civilization", lambda m: [
        ("preferred_free", (_each(m, "next_research") or {}).get("preferred_free"))]),
    **{f"empire.{k}": ("civilization", lambda m, k=k: [(k, (_each(m, "empire") or {}).get(k))])
       for k in ("policy", "preferred_policy", "great_person", "preferred_great_person", "pantheon",
                 "preferred_pantheon")},
    "cities.danger": ("city", lambda m: [(c["city"], c["danger"]) for c in _each(m, "cities") or []]),
    "cities.garrison": ("city", lambda m: [(c["city"], c["garrison"]) for c in _each(m, "cities") or []]),
    "sites (top 3 as a set)": ("civilization", lambda m: [("sites", (_each(m, "sites") or [])[:3])]),
    "spare": ("civilization", lambda m: [("spare", _each(m, "spare"))]),
    "attacks": ("unit", lambda m: [(a["unit"], a["target"]) for a in _each(m, "attacks") or []]),
    "war_target": ("civilization", lambda m: [("war_target", _each(m, "war_target"))]),
    "reachable": ("rival", lambda m: list((_each(m, "reachable") or {}).items())),
    "lux_trade": ("civilization", lambda m: [("lux_trade", _each(m, "lux_trade"))]),
    "advice.war_readiness": ("rival", lambda m: [(w["player"], w) for w in _advice(m).get("war_readiness") or []]),
    "advice.spare_luxuries": ("civilization", lambda m: [("spare_luxuries", _advice(m).get("spare_luxuries"))]),
    "advice.wants": ("civilization", lambda m: [("wants", _advice(m).get("wants"))]),
}


def rates(doc: dict) -> dict:
    """For each choice: what it compares, the items whose answer says something, and the items asked."""
    out = {c: {"item": item, "saying": 0, "asked": 0} for c, (item, _) in CHOICES.items()}
    for s in doc["states"]:
        for m in s["majors"]:
            for c, (_, items) in CHOICES.items():
                for _, v in items(m):
                    out[c]["asked"] += 1
                    out[c]["saying"] += _says(v)
    return out


def main(argv=None):
    common.ensure_hash_seed()
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--fixtures", action="append", help="a fixture folder (default: the committed ones)")
    ap.add_argument("--out", help=f"where to write (default: {OUT.relative_to(ROOT)} for the committed states; "
                                  f"{ENV}, else <first folder>/bot_decisions.json.gz, for others)")
    ap.add_argument("--check", action="store_true", help="compare with the file instead of writing it")
    args = ap.parse_args(argv)
    if args.fixtures:
        folders = [Path(f) if Path(f).is_absolute() else ROOT / f for f in args.fixtures]
        out = Path(args.out or os.environ.get(ENV) or folders[0] / "bot_decisions.json.gz")
    else:
        folders, out = FIXTURES, Path(args.out) if args.out else OUT
    doc, errors = dump(folders)
    body = text(doc)
    for kind, n in counts(doc).items():
        print(f"  {kind}: {n['asked']} asked, {n['answered']} saying something, {n['items']} items")
    print("base rates of the choices (items whose answer says something, of those asked):")
    for choice, n in rates(doc).items():
        print(f"  {choice}: {n['saying']} of {n['asked']} (by {n['item']})")
    if errors:
        print(f"{len(errors)} questions raised:", *errors, sep="\n  ")
    if args.check:
        have = gzip.decompress(out.read_bytes()).decode("utf-8") if out.exists() else None
        same = have == body
        print(f"{out}: {'current' if same else 'differs from a new recording'}")
        return 0 if same and not errors else 1
    common.write_gz(out, body)
    print(f"wrote {out}: {sum(len(s['majors']) for s in doc['states'])} civilizations in {len(doc['states'])} states")
    return 1 if errors else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception:
        traceback.print_exc()
        sys.exit(2)
