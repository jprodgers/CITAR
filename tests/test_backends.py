"""The suite's backend markers are honest (crates/citar-engine/DESIGN.md P2.7.4).

Every ``@python_engine_only`` names a successor that exists and runs on Rust: a rule script, a Rust test (a function
with a test attribute) or a Python test that is neither Python-engine-only nor pending, itself or through its class.
A rule script whose ``needs`` names a bot package still to come (``backends.BOT_PACKAGES_TO_COME``) is the one
exception: both runners skip it on Rust until that package lands and removes the header, so it holds only for such a
package, and test_successors_waiting_on_a_package lists those successors in the run's output. Every
``@rust_pending`` names a package still to come (2-11), so a package that lands removes its markers, and none names
a package that has. The markers are read from the test modules without running them.
"""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import functools
import importlib
import inspect
import re
import unittest
import unittest.mock
from pathlib import Path

from tests import backends, rulescript

ROOT = Path(__file__).resolve().parent.parent
TESTS = ROOT / "tests"
CRATES = ROOT / "crates"


_FN = re.compile(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)")
#: A whole-line comment (doc comments too), which may hold any text, braces and "fn" included.
_LINE_COMMENT = re.compile(r"^[ \t]*//.*$", re.M)
#: The attributes that make a function a test: libtest's, and those of the test crates' macros.
_TEST_ATTR = re.compile(r"#\[\s*(?:test|rstest|tokio::test|test_case|proptest)\b")


@functools.cache
def _rust_test_names() -> frozenset:
    """The name of every Rust test in the crates: a function with a test attribute (a Rust test is named by its
    function). Its attributes are what stands between the item before it (a ``;``, ``{`` or ``}``) and its ``fn``."""
    names = set()
    for path in CRATES.rglob("*.rs"):
        if "target" in path.parts:
            continue
        text = _LINE_COMMENT.sub("", path.read_text(encoding="utf-8", errors="replace"))
        for m in _FN.finditer(text):
            head = text[:m.start()]
            attrs = head[max(head.rfind(";"), head.rfind("{"), head.rfind("}")) + 1:]
            if _TEST_ATTR.search(attrs):
                names.add(m.group(1))
    return frozenset(names)


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


def _script_needs(successor: str):
    """The ``needs`` of a rule-script successor (a bot package that makes it pass on Rust), or None."""
    return rulescript.load(ROOT / successor).needs


def successor_holds(successor: str, rust: set) -> bool:
    """Whether a python_engine_only successor exists and runs on Rust: a rule script under tests/rules/ (with no
    ``needs``, or one naming a bot package still to come), a Rust test (``rust``, their names), or a Python test that
    is neither Python-engine-only nor pending, itself or through its class."""
    if successor.endswith(".toml"):
        if not (successor.startswith("tests/rules/") and (ROOT / successor).is_file()):
            return False
        needs = _script_needs(successor)
        return needs is None or needs in backends.BOT_PACKAGES_TO_COME
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

    def test_successors_waiting_on_a_package(self):
        # Successors that both runners skip on Rust until their bot package lands: listed here, in the run's output,
        # since no test checks the behaviour on Rust until then.
        waiting = sorted({f"{successor} (needs {_script_needs(successor)})" for _, kind, successor in self.found
                          if kind == "python_engine_only" and successor.endswith(".toml")
                          and successor_holds(successor, set()) and _script_needs(successor) is not None})
        if waiting:
            self.skipTest(f"{len(waiting)} successors run on Rust only once their package lands: {', '.join(waiting)}")

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
            @backends.rust_pending("2-11")
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
        self.assertFalse(successor_holds("parse", rust), "a function that is no test")
        # a script both runners skip on Rust holds only while the bot package it needs is still to come (no script
        # names one since 2-05, the last bot package, so a script's header is stood in for)
        successor = "tests/rules/bot_selftest.toml"
        self.assertTrue(successor_holds(successor, rust))
        with unittest.mock.patch(f"{__name__}._script_needs", return_value="2-05"):
            self.assertFalse(successor_holds(successor, rust))
            with unittest.mock.patch.object(backends, "BOT_PACKAGES_TO_COME", ("2-05",)):
                self.assertTrue(successor_holds(successor, rust))
        self.assertTrue(successor_holds("tests.test_facade_games.SettingsTests.test_a_seats_difficulty_survives_a_save",
                                        rust))
        # a successor that does not run on Rust is none: one Python-engine-only, one pending (by its class; no test is
        # pending since 2-09, so a pending class is stood in for)
        self.assertFalse(successor_holds("tests.test_engine.HexTests.test_line_endpoints", rust))
        successor = "tests.test_editor.MapTests.test_big_sizes_exist"
        self.assertTrue(successor_holds(successor, rust))

        class Pending:
            _backend = (("rust_pending", "2-11"),)

            def test_big_sizes_exist(self):
                pass
        real = _python_test
        with unittest.mock.patch(f"{__name__}._python_test",
                                 side_effect=lambda t: Pending if t.endswith(".MapTests") else real(t)):
            self.assertFalse(successor_holds(successor, rust))
        self.assertIn("arguments_are_coerced_as_python_coerced_them", rust)
        self.assertNotIn("no_such_rust_test_anywhere", rust)
        self.assertIsNotNone(_python_test("tests.test_backends.MarkerTests.test_pending_names_a_package_to_come"))
        self.assertIsNone(_python_test("tests.test_backends.MarkerTests.test_nothing"))
        self.assertIsNone(_python_test("tests.no_such_module.Thing"))


def python_reference() -> list[str]:
    """What tests/python_reference.txt lists: a test module or test id per line, ``#`` starting a comment."""
    text = (TESTS / "python_reference.txt").read_text(encoding="utf-8")
    return [line.split("#", 1)[0].strip() for line in text.splitlines() if line.split("#", 1)[0].strip()]


class PythonReferenceTests(unittest.TestCase):
    """The Python reference subset CI's Python-backend job runs (DESIGN.md P2.6.6): what it lists is there."""

    def test_every_entry_names_a_test_module_or_test(self):
        listed = python_reference()
        self.assertTrue(listed)
        self.assertEqual(len(listed), len(set(listed)), "an entry listed twice")
        for entry in listed:
            with self.subTest(entry=entry):
                self.assertTrue(entry.startswith("tests.test_"), entry)
                self.assertIsNotNone(_python_test(entry), f"{entry} names no test module or test")

    def test_it_holds_the_python_engines_own_checks(self):
        # the design's three: the rule scripts' Python runner (with the Python engine's recordings), the parity test
        # and the parameter-schema test
        self.assertLessEqual({"tests.test_rule_scripts", "tests.test_facade_parity", "tests.test_bot_params"},
                             set(python_reference()))


if __name__ == "__main__":
    unittest.main()
