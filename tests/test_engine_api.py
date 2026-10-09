"""The engine facade: what it hands out is plain data, saves round-trip through it, and headless games run on it."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Optional

from citar import engine_api
from citar.engine_api import ActionError, EngineGame
from tests import has_test_ops


def duel(**kw) -> EngineGame:
    cfg = {"map_size": "duel", "seed": 21, "barbarians": "off", "players": [{"controller": "human"}, {"controller": "bot"}]}
    cfg.update(kw)
    return EngineGame.new(cfg)


class PlainDataTests(unittest.TestCase):
    def test_negotiations_are_copies(self):
        g = duel()
        g.meet(0, 1)
        nid = g.execute(0, "open_negotiation", {"to": 1, "message": "Hello."})["negotiation_id"]
        n = g.negotiation(nid)
        n["status"] = "accepted"
        n["history"].clear()
        g.open_negotiations()[0]["history"].append({"forged": True})
        again = g.negotiation(nid)
        self.assertEqual((again["status"], len(again["history"])), ("open", 1))
        self.assertEqual([m["id"] for m in g.open_negotiations(1)], [nid])
        self.assertEqual(g.open_negotiations(1), g.negotiations(1))
        with self.assertRaises(ActionError):
            g.negotiation(99)

    def test_player_rows_and_the_controller(self):
        g = duel()
        row = g.player(1)
        self.assertEqual((row["controller"], row["handicap"], row["auto"]["conquest"]), ("bot", "ai", True))
        row["auto"]["conquest"] = False                   # a copy: the game is untouched
        self.assertTrue(g.player(1)["auto"]["conquest"])
        g.set_controller(1, "llm")
        self.assertEqual((g.player(1)["controller"], g.player(1)["handicap"]), ("llm", "human"))
        self.assertEqual([p["id"] for p in g.majors()], [0, 1])
        self.assertTrue(g.set_difficulty(0, "deity"))
        self.assertEqual(g.player(0)["difficulty"], "Deity")
        self.assertFalse(g.set_difficulty(0, "impossible"))

    def test_summary_and_standings(self):
        g = duel()
        s = g.summary()
        self.assertEqual((s["turn"], s["current"], s["phase"]), (1, 0, "playing"))
        self.assertEqual(s["turn_limit"], g.turn_limit)
        st = g.standings()
        self.assertEqual(set(st), {0, 1})
        self.assertEqual(set(st[0]), {"score", "cities", "units", "population", "techs", "gold", "era"})

    def test_config_is_a_copy(self):
        g = duel()
        g.config["seed"] = 999
        self.assertEqual(g.config["seed"], 21)

    def test_negotiation_views_and_the_empire_are_copies(self):
        g = duel()
        g.meet(0, 1)
        g.apply_ops([{"op": "set_player", "player": 0, "gold": 100}])
        nid = g.execute(0, "open_negotiation", {"to": 1, "message": "Gold for you.",
                                                "give": [{"type": "gold", "amount": 30}]})["negotiation_id"]
        view = g.negotiation_view(nid, 0)
        view["current_proposal"]["you_give"][0]["amount"] = 1
        self.assertEqual(g.negotiation(nid)["proposal"]["0"][0]["amount"], 30)
        emp = g.empire_summary(0)
        total = emp["happiness"]["total"]
        emp["happiness"]["total"] = -99
        self.assertEqual(g.empire_summary(0)["happiness"]["total"], total)

    def test_negotiation_heads(self):
        g = duel()
        g.meet(0, 1)
        nid = g.execute(0, "open_negotiation", {"to": 1, "message": "Hello."})["negotiation_id"]
        head = {"id": nid, "initiator": 0, "responder": 1, "status": "open", "awaiting": 1, "entries": 1}
        self.assertEqual(g.negotiation_head(nid), head)
        self.assertEqual(g.open_negotiation_heads(), [head])
        self.assertEqual(g.open_negotiation_heads(1), [head])
        g.close_negotiation(nid, "expired", "(no reply in time)")
        self.assertEqual(g.open_negotiation_heads(), [])
        self.assertEqual((g.negotiation_head(nid)["status"], g.negotiation_head(nid)["entries"]), ("expired", 2))
        with self.assertRaises(ActionError):
            g.negotiation_head(99)


class SaveTests(unittest.TestCase):
    def test_a_save_round_trips(self):
        g = duel()
        g.execute(0, "end_turn")
        data = g.to_save()
        back = EngineGame.from_save(data)
        self.assertEqual(back.summary(), g.summary())
        self.assertEqual(back.view(0)["tiles"], g.view(0)["tiles"])

    def test_state_summary_reads_a_save(self):
        g = duel()
        for _ in range(4):
            g.execute(g.current, "end_turn")
        summ = engine_api.state_summary(g.to_save()["state"])
        self.assertEqual((summ["turn"], summ["phase"], summ["map_size"]), (g.turn, "playing", "duel"))
        self.assertEqual((summ["winner"], summ["winner_id"]), (None, None))
        self.assertEqual(summ["names"][1], g.player_name(1))
        self.assertEqual([m["id"] for m in summ["majors"]], [0, 1])
        last = g.stats(1)[0]["players"]
        self.assertEqual(summ["scores"], {pid: last[str(pid)]["score"] for pid in (0, 1)})
        self.assertEqual(engine_api.state_summary(duel().to_save()["state"])["scores"], {}, "no stats row yet")

    def test_state_dict_is_independent(self):
        g = duel()
        copy = EngineGame.from_state(g.state_dict())
        copy.apply_ops([{"op": "set_player", "player": 0, "gold": 777}])
        self.assertEqual(copy.standing(0)["gold"], 777)
        self.assertNotEqual(g.standing(0)["gold"], 777)


class ToolAndOpTests(unittest.TestCase):
    def test_tool_kinds_and_schemas(self):
        self.assertEqual((engine_api.tool_kind("end_turn"), engine_api.tool_kind("get_briefing")), ("action", "query"))
        self.assertIsNone(engine_api.tool_kind("no_such_tool"))
        names = {t["name"] for t in engine_api.tool_list()}
        self.assertIn("respond_negotiation", names)
        self.assertTrue(all("input_schema" in t for t in engine_api.tool_list("query")))

    def test_debug_and_path_preview(self):
        g = duel()
        with self.assertRaises(ValueError):
            g.debug("free_victory")
        g.debug("gold")
        self.assertEqual(g.standing(0)["gold"], 500)
        theirs = g.view(1)["units"][0]
        self.assertEqual(g.path_preview(0, theirs["id"], 0, 0), {"path": None})

    def test_a_probe_opens_a_negotiation_out_of_turn(self):
        g = duel()
        g.meet(0, 1)
        r = g.open_negotiation_as(1, 0, "Peace?")
        self.assertEqual(g.current, 0, "the turn is only lent for the call")
        self.assertEqual(g.negotiation(r["negotiation_id"])["initiator"], 1)


class BotTests(unittest.TestCase):
    def test_bot_instances(self):
        idle = engine_api.bot_instance("idle")
        bot = engine_api.bot_instance("basic", aggression=0.7, params={"counter_rounds": 1})
        self.assertEqual(bot.aggression, 0.7)
        self.assertEqual((idle.version, bot.version), ("idle", "basic-1"))
        for bad in ("os", "basic.py", "../basic", ""):
            with self.assertRaises(ValueError):
                engine_api.bot_instance(bad)
        with self.assertRaises(TypeError):
            engine_api.bot_instance("basic", seed=3)     # a bot draws from its game's seed: it has none of its own

    def test_bots_are_compiled_versions(self):
        # A bot is a version and its parameters (DESIGN.md P2.8.5): "basic" names the latest, which carries the
        # overrides cleaned against its schema; the frozen snapshots were archived with 0.1.5.
        versions = engine_api.bot_versions()
        latest = next(v["id"] for v in versions if v["latest"])
        bot = engine_api.bot_instance("basic", params={"counter_rounds": 1})
        self.assertEqual(bot.version, latest)
        self.assertEqual(engine_api.bot_instance(latest).version, latest)
        self.assertEqual(engine_api.bot_clean_params(latest, {"counter_rounds": 1}), {"counter_rounds": 1})
        self.assertEqual(engine_api.bot_schema()["engine"], latest)
        for frozen in ("frozen_d95d50cb", "snapshot-0922"):
            with self.assertRaises(ValueError):
                engine_api.bot_instance(frozen)
        with self.assertRaises(ValueError):
            engine_api.bot_instance("basic", params={"no_such_parameter": 1})
        fp = engine_api.bot_fingerprint(bot)
        self.assertRegex(fp, r"^[0-9a-f]{12}$")
        self.assertNotEqual(fp, engine_api.bot_fingerprint(engine_api.bot_instance("basic")))
        info = engine_api.build_info()
        self.assertEqual(set(info), {"version", "build_id", "label", "rules", "engine_code", "bot_code",
                                     "ruleset_dir"})

    def test_the_diplomacy_switch(self):
        bot = engine_api.bot_instance("basic")
        engine_api.bot_set_diplomacy(bot, {"trades": "llm"})
        self.assertFalse(engine_api.bot_owns_negotiation(bot, {"proposal": {"0": [{"type": "gold", "amount": 5}],
                                                                           "1": []}}))
        self.assertTrue(engine_api.bot_owns_negotiation(bot, {"proposal": None}))
        with self.assertRaises(ValueError):
            engine_api.bot_set_diplomacy(bot, {"trade": "llm"})

    def test_a_headless_game_runs_on_the_facade(self):
        turns, events = [], []
        r = engine_api.run_game({"config": {"map_size": "duel", "seed": 4, "barbarians": "off", "turn_limit": 12,
                                            "players": [{"controller": "bot"}, {"controller": "bot"}]},
                                 "bots": {0: engine_api.bot_instance("basic"),
                                          1: engine_api.bot_instance("idle")}},
                                on_turn=lambda info: turns.append(info["turn"]), on_event=events.append)
        self.assertEqual((r["phase"], r["errors"]), ("over", []))
        self.assertEqual(r["turns"], r["turn"] - 1)
        self.assertEqual(turns, sorted(set(turns)))
        self.assertEqual(turns[0], 1)
        self.assertTrue(any(e["type"] == "city_founded" for e in events))
        majors = [p for p in r["players"] if p["kind"] == "major"]
        self.assertEqual(len(majors), 2)
        self.assertTrue(all(p["cities"] >= 1 and p["score"] > 0 for p in majors))
        self.assertEqual(len(r["stats"]), r["turns"])

    def test_a_panicking_bot_is_recorded_or_raised(self):
        # A bot does not raise: a panic is its crash, recorded as the runner's crash line, or raised as EngineCrash
        # with raise_errors (never an ActionError: a crash is no refusal). A bot that is no compiled one is refused.
        if not has_test_ops():
            self.skipTest("the panic needs a build with the test operations")

        def spec(**kw):
            return {"config": {"map_size": "duel", "seed": 4, "barbarians": "off", "turn_limit": 8,
                               "players": [{"controller": "bot"}, {"controller": "bot"}]},
                    "bots": {0: engine_api.bot_instance("basic"), 1: engine_api.bot_instance("idle")},
                    "test_panic": {"player": 0, "turn": 3}, **kw}

        r = engine_api.run_game(spec(labels={0: "careless"}))
        self.assertEqual(r["phase"], "playing", "a crash ends a game where it stands")
        self.assertEqual(len(r["errors"]), 1, r["errors"])
        self.assertTrue(r["errors"][0].startswith("T3 P0 careless: panic: "), r["errors"])
        with self.assertRaises(engine_api.EngineCrash) as e:
            engine_api.run_game(spec(raise_errors=True))
        self.assertNotIsInstance(e.exception, ActionError)
        with self.assertRaises(TypeError):
            engine_api.run_game({"config": spec()["config"], "bots": {0: object()}})


#: What `citar doctor` says of the engine and the ruleset, run in a child. The child may first hide the extension, as an
#: install without it would be.
_DOCTOR = """
import sys
if sys.argv[1] == "hide":
    class NoExtension:
        def find_spec(self, name, path=None, target=None):
            if name == "citar._engine":
                raise ModuleNotFoundError(f"No module named {name!r}", name=name)
    sys.meta_path.insert(0, NoExtension())
from citar import doctor
r = doctor.Report()
doctor._check_engine(r)
doctor._check_ruleset(r)
print("failures", r.failures, "warnings", r.warnings)
"""

ROOT = Path(__file__).resolve().parent.parent


def child(*args: str, env: Optional[dict] = None) -> subprocess.CompletedProcess:
    """A Python child in the checkout, its environment this one's but CITAR_RULESET_DIR, with ``env`` over it."""
    base = {k: v for k, v in os.environ.items() if k != "CITAR_RULESET_DIR"}
    return subprocess.run([sys.executable, *args], capture_output=True, text=True, env={**base, **(env or {})},
                          timeout=300, cwd=ROOT)


class DoctorTests(unittest.TestCase):
    """What `citar doctor` says of the engine: its build, or why it does not load."""

    def test_the_doctor_names_a_missing_extension(self):
        r = child("-c", _DOCTOR, "hide")
        self.assertIn("[ FAIL ] engine: does not load (cannot import name '_engine'", r.stdout)
        self.assertIn("The engine's extension (citar._engine) is missing or out of date", r.stdout)
        self.assertIn("ruleset: not checked: the engine does not load", r.stdout)
        self.assertIn("failures 1 warnings 1", r.stdout)

    def test_the_doctor_names_the_build_and_the_ruleset(self):
        r = child("-c", _DOCTOR, "as is")
        self.assertRegex(r.stdout, r"\[  ok  \] engine: build [0-9a-f]{12} \(.+\), version ")
        rules = engine_api.build_info()["rules"][:12]
        self.assertIn(f"compiled into the engine (ruleset {rules})", r.stdout)
        self.assertIn("failures 0 warnings 0", r.stdout)


#: The ruleset the engine compiles in, in the data layout CITAR_RULESET_DIR names a copy of.
DATA = ROOT / "crates" / "citar-engine" / "data"
#: A civilization the shipped ruleset lacks, as a mod adds one (docs/MODDING.md).
TESTLANDIA = {"name": "Testlandia", "leaderName": "Test Leader", "adjective": "Testlandian", "startBias": [],
              "preferredVictoryType": "Neutral", "kind": "major",
              "cities": ["Testopolis", "Mockford", "Stubton", "Fixture Bay", "Assertia", "Harness Hill"]}

#: A process that plays the modded ruleset: what it is, a game that seats Testlandia and one that does not, saved.
_MODDED = """
import json, sys
from pathlib import Path
from citar import engine_api
out = Path(sys.argv[1])
config = {"map_size": "duel", "seed": 7, "barbarians": "off", "players": [{"controller": "bot"}, {"controller": "bot"}]}
mod = engine_api.EngineGame.new(dict(config, players=[{"controller": "bot", "nation": "Testlandia"},
                                                     {"controller": "bot", "nation": "Rome"}]))
for _ in range(4):
    mod.execute(mod.current, "end_turn")
plain = engine_api.EngineGame.new(dict(config, players=[{"controller": "bot", "nation": "Rome"},
                                                       {"controller": "bot", "nation": "Greece"}]))
journal, _ = engine_api.open_journal(out / "journal.cjnl")
mod.save_snapshot(journal).write(out / "testlandia.citar", journal, {"id": "mod", "name": "Testlandia"}, {})
journal.close()
json.dump({"info": engine_api.build_info(), "counts": engine_api.ruleset_counts(), "nation": mod.player(0)["nation"],
           "turn": mod.turn, "fingerprint": engine_api.bot_fingerprint(engine_api.bot_instance("basic")),
           "mod": mod.to_save(), "plain": plain.to_save()},
          open(out / "modded.json", "w", encoding="utf-8"))
"""


def ruleset_copy(where: Path, edits: dict) -> Path:
    """A copy of the shipped ruleset in ``where``, each file ``edits`` names changed by its function (on its JSON)."""
    out = where / "ruleset"
    shutil.copytree(DATA, out)
    for name, edit in edits.items():
        path = out / name
        doc = json.loads(path.read_text(encoding="utf-8"))
        edit(doc)
        path.write_text(json.dumps(doc, indent=1), encoding="utf-8")
    return out


def add_testlandia(nations: dict):
    nations["Testlandia"] = dict(TESTLANDIA)


def unknown_unique(buildings: dict):
    buildings["Library"]["uniques"].append("Makes every turn a Tuesday")


def broken_reference(units: dict):
    units["Archer"]["requiredTech"] = "Time Travel"


class RulesetDirTests(unittest.TestCase):
    """A modded ruleset without a Rust toolchain (DESIGN.md P2.8.4, docs/MODDING.md): CITAR_RULESET_DIR names a copy of
    the data that every game and ruleset function of the process uses, which build_info reports; `citar ruleset check`
    says what is wrong with one."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="citar-ruleset-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)

    def test_the_shipped_data_is_the_ruleset_compiled_in(self):
        report = engine_api.check_ruleset(DATA)
        self.assertEqual(report["errors"], [])
        self.assertEqual(report["id"], engine_api.build_info()["rules"])
        self.assertEqual(report["version"], engine_api.rules_version())
        self.assertEqual(report["counts"], engine_api.ruleset_counts())

    def test_a_process_plays_the_ruleset_the_variable_names(self):
        mod = ruleset_copy(self.tmp, {"custom/nations.json": add_testlandia})
        r = child("-c", _MODDED, str(self.tmp), env={"CITAR_RULESET_DIR": str(mod)})
        self.assertEqual(r.returncode, 0, r.stderr)
        got = json.loads((self.tmp / "modded.json").read_text(encoding="utf-8"))
        ours = engine_api.build_info()
        # its own ruleset, and so its own build and fingerprints: results never mix with the shipped ruleset's
        self.assertEqual(got["info"]["ruleset_dir"], str(mod))
        self.assertNotEqual(got["info"]["rules"], ours["rules"])
        self.assertEqual(got["info"]["rules"], engine_api.check_ruleset(mod)["id"])
        self.assertNotEqual(got["info"]["build_id"], ours["build_id"])
        self.assertEqual((got["info"]["engine_code"], got["info"]["bot_code"]), (ours["engine_code"], ours["bot_code"]))
        self.assertNotEqual(got["fingerprint"], engine_api.bot_fingerprint(engine_api.bot_instance("basic")))
        self.assertEqual(got["counts"]["nations"], engine_api.ruleset_counts()["nations"] + 1)
        # a game seats the added nation and plays
        self.assertEqual((got["nation"], got["turn"]), ("Testlandia", 3))
        # its saves are refused by a process without it, naming what this one lacks
        with self.assertRaisesRegex(ValueError, "Testlandia"):        # LoadError
            EngineGame.from_save(got["mod"])
        header = engine_api.save_header(self.tmp / "testlandia.citar")
        self.assertEqual(header["rules"], got["info"]["rules"])
        with self.assertRaisesRegex(ValueError, "Testlandia"):
            EngineGame.from_save(engine_api.read_save(self.tmp / "testlandia.citar"))
        # a save that names nothing the shipped ruleset lacks loads, with a warning
        with self.assertLogs("citar.engine", "WARNING") as logs:
            plain = EngineGame.from_save(got["plain"])
        self.assertIn("made with another ruleset", "\n".join(logs.output))
        self.assertEqual(plain.player(1)["nation"], "Greece")

    def test_a_ruleset_that_does_not_load_stops_the_engine_saying_why(self):
        bad = ruleset_copy(self.tmp, {"ruleset/buildings.json": unknown_unique})
        r = child("-c", "import citar.engine_api", env={"CITAR_RULESET_DIR": str(bad)})
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("ImportError: CITAR_RULESET_DIR=", r.stderr)
        self.assertIn("Makes every turn a Tuesday", r.stderr)
        r = child("-c", _DOCTOR, "as is", env={"CITAR_RULESET_DIR": str(bad)})
        self.assertIn("[ FAIL ] ruleset: CITAR_RULESET_DIR=", r.stdout)
        self.assertIn(f"Run `citar ruleset check {bad}`", r.stdout)
        self.assertIn("failures 1 warnings 1", r.stdout)
        # an empty value is no value: the ruleset compiled in
        r = child("-c", "from citar import engine_api; print(engine_api.build_info()['ruleset_dir'])",
                  env={"CITAR_RULESET_DIR": " "})
        self.assertEqual((r.returncode, r.stdout.strip()), (0, "None"), r.stderr)

    def test_the_check_command_reports_each_problem(self):
        def check(directory, env=None):
            return child("-m", "citar", "ruleset", "check", str(directory), env=env)

        r = check(DATA)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn(f"the ruleset loads: ruleset {engine_api.build_info()['rules'][:12]}", r.stdout)
        unique = ruleset_copy(self.tmp / "u", {"ruleset/buildings.json": unknown_unique})
        r = check(unique)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("the ruleset does not load: 1 problem", r.stdout)
        self.assertRegex(r.stdout, r"- ruleset/buildings.json: Library: .*Makes every turn a Tuesday.* \[UnknownUnique\]")
        reference = ruleset_copy(self.tmp / "r", {"ruleset/units.json": broken_reference})
        r = check(reference)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertRegex(r.stdout, r"- ruleset/units.json: Archer: .*Time Travel.* \[UnknownReference\]")
        # the variable is not the check's business: a broken one does not stop the check of another directory
        r = check(DATA, env={"CITAR_RULESET_DIR": str(unique)})
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        r = check(self.tmp / "nowhere")
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertIn("not a directory", r.stderr)


if __name__ == "__main__":
    unittest.main()
