"""The suite's backend markers are honest (crates/citar-engine/DESIGN.md P2.7.4).

Every ``@python_engine_only`` names a successor that exists and runs on Rust: a rule script, a Rust test or a Python
test that is neither Python-engine-only nor pending, itself or through its class. Every ``@rust_pending`` names a package still to come (2-09, 2-10 or
2-11), so a package that lands removes its markers, and none names a package that has. The markers are read from the
test modules without running them.
"""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import importlib
import inspect
import re
import unittest
from pathlib import Path

from tests import backends

ROOT = Path(__file__).resolve().parent.parent
TESTS = ROOT / "tests"
CRATES = ROOT / "crates"


def _rust_test_names() -> set:
    """Every function name in the crates' Rust sources: a Rust test is named by its function."""
    names = set()
    fn = re.compile(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)")
    for path in CRATES.rglob("*.rs"):
        if "target" in path.parts:
            continue
        names.update(fn.findall(path.read_text(encoding="utf-8", errors="replace")))
    return names


def markers() -> list[tuple[str, str, object]]:
    """Every marker in the suite: (the test's id, the marker's kind, what it names)."""
    out = []
    for path in sorted(TESTS.glob("test_*.py")):
        mod = importlib.import_module(f"tests.{path.stem}")
        for cname, cls in vars(mod).items():
            if not (inspect.isclass(cls) and issubclass(cls, unittest.TestCase) and cls.__module__ == mod.__name__):
                continue
            for kind, value in vars(cls).get("_backend", ()):
                out.append((f"tests.{path.stem}.{cname}", kind, value))
            for name, member in vars(cls).items():
                for kind, value in getattr(member, "_backend", ()):
                    out.append((f"tests.{path.stem}.{cname}.{name}", kind, value))
    return out


def _python_test(test_id: str):
    """The test object a Python test id names, or None."""
    parts = test_id.split(".")
    for cut in range(len(parts), 1, -1):
        try:
            obj = importlib.import_module(".".join(parts[:cut]))
        except ImportError:
            continue
        for attr in parts[cut:]:
            obj = getattr(obj, attr, None)
            if obj is None:
                return None
        return obj
    return None


def successor_holds(successor: str, rust: set) -> bool:
    """Whether a python_engine_only successor exists and runs on Rust: a rule script under tests/rules/, a function
    of the crates (``rust``, the names), or a Python test that is neither Python-engine-only nor pending, itself or
    through its class."""
    if successor.endswith(".toml"):
        return successor.startswith("tests/rules/") and (ROOT / successor).is_file()
    if successor.startswith("tests."):
        target = _python_test(successor)
        cls = _python_test(successor.rsplit(".", 1)[0])
        marks = {k for obj in (target, cls) for k, _ in getattr(obj, "_backend", ())}
        return target is not None and not {"python_engine_only", "rust_pending"} & marks
    return re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", successor) is not None and successor in rust


class MarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.found = markers()

    def test_the_markers_are_found(self):
        kinds = {k for _, k, _ in self.found}
        self.assertTrue(self.found, "the finder sees no marker at all")
        self.assertLessEqual(kinds, {"python_engine_only", "rust_pending", "rust_only"}, self.found)

    def test_every_successor_exists_and_runs_on_rust(self):
        rust = _rust_test_names()
        bad = [f"{test_id}: {successor}" for test_id, kind, successor in self.found
               if kind == "python_engine_only" and not successor_holds(successor, rust)]
        self.assertEqual(bad, [], "python_engine_only successors that do not exist (or do not run on Rust)")

    def test_pending_names_a_package_to_come(self):
        bad = [f"{t}: {v}" for t, k, v in self.found if k == "rust_pending" and v not in backends.PENDING_PACKAGES]
        self.assertEqual(bad, [])

    def test_the_markers_check_what_they_are_given(self):
        for bad in ("", "  ", None):
            with self.assertRaises(ValueError):
                backends.python_engine_only(bad)
        for bad in ("later", "2-9", 209):
            with self.assertRaises(ValueError):
                backends.rust_pending(bad)

        class Probe(unittest.TestCase):
            @backends.rust_pending("2-09")
            @backends.python_engine_only("tests.test_backends.MarkerTests.test_pending_names_a_package_to_come")
            def test_x(self):
                pass

            @backends.rust_only
            def test_y(self):
                pass

        self.assertEqual([k for k, _ in Probe.test_x._backend], ["python_engine_only", "rust_pending"])
        self.assertEqual(Probe.test_y._backend, (("rust_only", None),))
        self.assertEqual(bool(getattr(Probe.test_x, "__unittest_skip__", False)), backends.RUST)
        self.assertEqual(bool(getattr(Probe.test_y, "__unittest_skip__", False)), not backends.RUST)

    def test_the_successor_finders_tell_real_from_missing(self):
        rust = _rust_test_names()
        self.assertTrue(successor_holds("tests/rules/_selftest.toml", rust))
        self.assertFalse(successor_holds("tests/rules/no_such_script.toml", rust))
        self.assertFalse(successor_holds("tests/test_engine.py", rust), "a script lives in tests/rules")
        self.assertTrue(successor_holds("arguments_are_coerced_as_python_coerced_them", rust))
        self.assertFalse(successor_holds("no_such_rust_test_anywhere", rust))
        self.assertTrue(successor_holds("tests.test_facade_games.SettingsTests.test_a_seats_difficulty_survives_a_save",
                                        rust))
        # a successor that does not run on Rust is none: one Python-engine-only, one pending (by its class)
        self.assertFalse(successor_holds("tests.test_engine.HexTests.test_line_endpoints", rust))
        self.assertFalse(successor_holds("tests.test_editor.MapTests.test_big_sizes_exist", rust))
        self.assertIn("arguments_are_coerced_as_python_coerced_them", rust)
        self.assertNotIn("no_such_rust_test_anywhere", rust)
        self.assertIsNotNone(_python_test("tests.test_backends.MarkerTests.test_pending_names_a_package_to_come"))
        self.assertIsNone(_python_test("tests.test_backends.MarkerTests.test_nothing"))
        self.assertIsNone(_python_test("tests.no_such_module.Thing"))


if __name__ == "__main__":
    unittest.main()
