"""Every rule script in tests/rules/ through the bindings, one test each (tests/rulescript.py is the runner, and
tests/rules/README.md the language): the Rust harness plays the same files (crates/citar-testkit/tests/rules.rs)."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import json
import unittest
from pathlib import Path

from tests import has_test_ops, rulescript


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


if __name__ == "__main__":
    unittest.main()
