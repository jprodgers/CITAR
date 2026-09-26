"""Every rule script in tests/rules/ on the Python engine, one test each (tests/rulescript.py is the runner, and
tests/rules/README.md the language), and the table of tool-argument coercions both engines must agree on."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import json
import unittest
from pathlib import Path

from tests import rulescript


class RuleScripts(unittest.TestCase):
    """One test per script; the Rust runner plays the same files (crates/citar-testkit/tests/rules.rs)."""

    def test_there_are_scripts_and_a_self_test(self):
        names = [p.stem for p in rulescript.discover()]
        self.assertIn("_selftest", names)
        self.assertGreater(len(names), 4)

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


def _script_test(path: Path):
    def test(self):
        try:
            rulescript.run(path)
        except rulescript.ScriptError as e:
            self.fail(f"{path.name}: {e}")
    test.__doc__ = f"tests/rules/{path.name}"
    return test


for _path in rulescript.discover():
    setattr(RuleScripts, f"test_script_{_path.stem.lstrip('_')}", _script_test(_path))


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


if __name__ == "__main__":
    unittest.main()
