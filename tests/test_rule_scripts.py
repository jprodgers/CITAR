"""Every rule script in tests/rules/ on the facade's backend, one test each (tests/rulescript.py is the runner, and
tests/rules/README.md the language), and the table of tool-argument coercions both engines must agree on.

On the Rust backend the same runner plays every script through the bindings, the ``intended`` steps included; a script
whose ``needs`` names a package is skipped there, as the Rust harness ignores it, until that package removes the
header. The checks of the Python engine's own recordings are that engine's alone."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import json
import unittest
from pathlib import Path

from tests import rulescript
from tests.backends import RUST, has_test_ops, python_engine_only


class RuleScripts(unittest.TestCase):
    """One test per script; the Rust runner plays the same files (crates/citar-testkit/tests/rules.rs)."""

    def test_there_are_scripts_and_a_self_test(self):
        names = [p.stem for p in rulescript.discover()]
        self.assertIn("_selftest", names)
        self.assertGreater(len(names), 4)
        # every script in the directory is a test of this class, counted from the directory
        tests_here = {n[len("test_script_"):] for n in dir(type(self)) if n.startswith("test_script_")}
        self.assertEqual(tests_here, {n.lstrip("_") for n in names})
        self.assertEqual(len(tests_here), len(list(rulescript.RULES.glob("*.toml"))) - 1, "intended.toml aside")

    @python_engine_only("the_operations_are_sorted_and_described_in_one_line")
    def test_every_test_operation_runs_its_own_function(self):
        # Two @op decorators stacked on one function register it under both names, and the other op's function is
        # lost (refresh_visibility once ran set_difficulty): each op must be the function named after it, once.
        from citar.engine import testops
        for name, (fn, _) in testops.OPS.items():
            self.assertEqual(fn.__name__, f"_{name}", name)
        self.assertEqual(len({id(fn) for fn, _ in testops.OPS.values()}), len(testops.OPS))

    def test_a_script_map_s_wrapping_copy_keeps_its_tiles(self):
        # maps/<name>_wrap.json is <name>.json wrapping both ways: its tiles, starts and anchors must stay the
        # other's, or a script on the copy tests another map than its anchors say. The Rust side checks the same
        # (crates/citar-testkit/tests/engine/maps.rs).
        maps = rulescript.RULES / "maps"
        copies = sorted(p.name[: -len("_wrap.json")] for p in maps.glob("*_wrap.json"))
        self.assertIn("arena", copies)
        for base in copies:
            plain = json.loads((maps / f"{base}.json").read_text(encoding="utf-8"))
            copy = json.loads((maps / f"{base}_wrap.json").read_text(encoding="utf-8"))
            self.assertEqual((copy["wrap_x"], copy["wrap_y"]), (True, True), base)
            for key in ("id", "name", "description", "wrap_x", "wrap_y"):
                plain.pop(key, None)
                copy.pop(key, None)
            self.assertTrue(plain == copy, f"{base}_wrap.json is no longer {base}.json wrapping")

    def test_a_script_with_an_unknown_key_is_refused(self):
        with self.assertRaises(rulescript.ScriptError):
            rulescript.Script("bad", {"about": "x", "stepz": []})
        with self.assertRaises(rulescript.ScriptError):
            rulescript.Script("bad", {"step": []})

    def test_needs_names_a_bot_package(self):
        # A script's `needs` is the Phase 2 package that makes it pass on the Rust engine, which then removes the
        # header (crates/citar-engine DESIGN.md P2.3.11): only the bot's ports carry scripts the Rust runner ignores.
        for bad in ("later", "2-1", "2-01ab", 3):
            with self.assertRaises(rulescript.ScriptError):
                rulescript.Script("bad", {"about": "x", "needs": bad})
        for path in rulescript.discover():
            needs = rulescript.load(path).needs
            if needs is not None:
                self.assertIn(needs, ("2-01a", "2-01b", "2-03", "2-05"), path.name)

    def test_every_bot_script_is_named_for_the_bot(self):
        # The bot's scripts are found by name (bot_*.toml), and every script with a bot step is one of them.
        import tomllib

        def has_bot_step(steps):
            return any(isinstance(s, dict) and ("bot" in s or has_bot_step(s.get("steps", []))) for s in steps)
        for path in rulescript.discover():
            with open(path, "rb") as fh:
                doc = tomllib.load(fh)
            if has_bot_step(doc.get("step", [])):
                self.assertTrue(path.stem.startswith("bot_"), path.name)

    def test_a_bot_turn_pins_every_draw(self):
        pinned = {"tech_noise": 0, "ranged_chance": 1, "peace_offer_chance": 0, "friend_chance": 1,
                  "friend_chance_aggr": 0, "war_chance": 1, "war_chance_aggr": 0}
        self.assertIsNone(rulescript.unpinned_draws(pinned))
        self.assertIsNone(rulescript.unpinned_draws(dict(pinned, war_prep_rate=2)))
        self.assertIsNone(rulescript.unpinned_draws(dict(pinned, war_chance=0, war_prep_rate=0.5)))
        self.assertIn("war_prep_rate", rulescript.unpinned_draws(dict(pinned, war_prep_rate=0.5)))
        self.assertIn("tech_noise", rulescript.unpinned_draws(dict(pinned, tech_noise=0.1)))
        self.assertIn("ranged_chance", rulescript.unpinned_draws(dict(pinned, ranged_chance=0.3)))
        self.assertIn("war_chance_aggr", rulescript.unpinned_draws(dict(pinned, war_chance_aggr=0.15)))
        for key in pinned:
            missing = dict(pinned)
            del missing[key]
            self.assertIn(key, rulescript.unpinned_draws(missing))


def _script_test(path: Path):
    def test(self):
        if RUST:
            needs = rulescript.load(path).needs
            if needs is not None:
                self.skipTest(f"needs package {needs} on the Rust engine")
            if not has_test_ops():
                self.skipTest("the scripts need a build of citar._engine with the test operations")
        try:
            rulescript.run(path)
        except rulescript.ScriptError as e:
            self.fail(f"{path.name}: {e}")
    test.__doc__ = f"tests/rules/{path.name}"
    return test


for _path in rulescript.discover():
    setattr(RuleScripts, f"test_script_{_path.stem.lstrip('_')}", _script_test(_path))


@python_engine_only("arguments_are_coerced_as_python_coerced_them")
class Normalize(unittest.TestCase):
    """tests/rules/normalize.json through tools.execute, the coercion the Rust engine's api::tools::normalize ports
    (tools.py:113-127): each case's tool is registered for the test, as a query that hands back what it received. A
    case marked ``intended`` is a deliberate difference only the Rust engine runs."""

    def test_the_coercion_table(self):
        from citar.engine import tools
        from citar.engine_api import ActionError
        table = json.loads((rulescript.RULES / "normalize.json").read_text(encoding="utf-8"))
        g = rulescript.Runner(rulescript.Script("normalize", {"about": "the coercion table"})).game
        names = []
        try:
            for name, spec in table["tools"].items():
                props = {k: ({"type": t} if t else {}) for k, t in spec["params"]}
                tool_name = f"_rulescript_{name}"
                tools.REGISTRY[tool_name] = tools.Tool(tool_name, "a test probe", props, list(spec["required"]),
                                                       lambda _g, _pid, **kw: kw, kind="query")
                names.append(tool_name)
            listed = rulescript.intended_ids()
            for case in table["cases"]:
                if "intended" in case:
                    self.assertIn(case["intended"], listed, case)
                    continue
                with self.subTest(case=case):
                    try:
                        got = rulescript.plain(g.execute(0, f"_rulescript_{case['tool']}", dict(case["args"])))
                    except ActionError as e:
                        self.assertIn("error", case, str(e))
                        self.assertEqual(str(e), case["error"])
                        continue
                    self.assertNotIn("error", case, got)
                    self.assertTrue(rulescript.same(got, case["out"]), f"{got} != {case['out']}")
        finally:
            for n in names:
                tools.REGISTRY.pop(n, None)


@python_engine_only("the_schemas_equal_python_s_tool_list")
class ToolList(unittest.TestCase):
    """tests/rules/tool_list.json is the Python engine's tool list as it stands, which the Rust registry's schemas
    must equal apart from its listed fixes (scripts/refcheck/tool_list.py; crates/citar-testkit/tests/engine/tools.rs).
    """

    def test_the_recorded_tool_list_is_current(self):
        import contextlib
        import io
        import scripts.refcheck.tool_list as recorder
        with contextlib.redirect_stdout(io.StringIO()) as out:
            status = recorder.main(["--check"])
        self.assertEqual(status, 0, out.getvalue())


@python_engine_only("the_query_tools_answer_as_python_s_did")
class QueryTools(unittest.TestCase):
    """refcheck/query_tools.json.gz is the Python engine's answers to the view queries on the committed fixtures, which
    the Rust engine's must equal apart from its listed fixes (scripts/refcheck/query_tools.py;
    crates/citar-refcheck/tests/query_tools.rs). The recording runs with PYTHONHASHSEED=0, in a process of its own."""

    def test_the_recorded_answers_are_current(self):
        import os
        import subprocess
        import sys
        script = Path(__file__).resolve().parents[1] / "scripts" / "refcheck" / "query_tools.py"
        env = dict(os.environ, PYTHONHASHSEED="0")
        done = subprocess.run([sys.executable, str(script), "--check"], env=env, capture_output=True, text=True)
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)


@python_engine_only("the_bot_chooses_as_pythons_did_on_the_committed_states")
class BotDecisions(unittest.TestCase):
    """refcheck/bot_decisions.json.gz is the Python bot's deterministic sub-decisions on the committed fixtures, which
    the Rust bot's are checked against (scripts/refcheck/bot_dump.py; DESIGN.md P2.3.11). The recording runs with
    PYTHONHASHSEED=0, in a process of its own, and gives the same bytes every time."""

    def test_the_recorded_decisions_are_current(self):
        import os
        import subprocess
        import sys
        script = Path(__file__).resolve().parents[1] / "scripts" / "refcheck" / "bot_dump.py"
        env = dict(os.environ, PYTHONHASHSEED="0")
        done = subprocess.run([sys.executable, str(script), "--check"], env=env, capture_output=True, text=True)
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)


if __name__ == "__main__":
    unittest.main()
