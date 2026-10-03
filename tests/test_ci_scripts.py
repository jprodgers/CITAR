"""The CI glue in scripts/ci/: unpack_ext.py, which puts the build-ext job's extension into the checkout for the test
jobs, and requirements.py, which lists what to install without building the package (crates/citar-engine/DESIGN.md
P2.6.8)."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import importlib.machinery
import importlib.util
import tempfile
import tomllib
import unittest
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def load(name: str):
    spec = importlib.util.spec_from_file_location(f"ci_{name}", ROOT / "scripts" / "ci" / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


unpack_ext = load("unpack_ext")
requirements = load("requirements")

#: A suffix this Python loads, and one it never does (the other family's).
HERE = importlib.machinery.EXTENSION_SUFFIXES[-1]
ELSEWHERE = ".abi3.so" if HERE == ".pyd" else ".pyd"


def wheel(folder: Path, members: dict, name: str = "citar-0.1.5-cp311-abi3-test.whl") -> Path:
    path = folder / name
    with zipfile.ZipFile(path, "w") as z:
        for member, data in members.items():
            z.writestr(member, data)
    return path


class UnpackExt(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="citar-unpack-"))
        self.addCleanup(__import__("shutil").rmtree, self.tmp, ignore_errors=True)
        self.package = self.tmp / "citar"
        self.package.mkdir()

    def test_only_the_library_is_written_and_older_ones_go(self):
        (self.package / "_engine.pyi").write_text("stub")
        (self.package / "_engine.cpython-311-x86_64-linux-gnu.so").write_bytes(b"old")
        (self.package / "_engine.pyd").write_bytes(b"older")
        w = wheel(self.tmp, {"citar/__init__.py": "x", "citar/_engine.pyi": "wheel stub",
                             f"citar/_engine{HERE}": b"new", "citar-0.1.5.dist-info/WHEEL": "Tag: x"})
        target = unpack_ext.unpack(w, self.package)
        self.assertEqual(target, self.package / f"_engine{HERE}")
        self.assertEqual(target.read_bytes(), b"new")
        self.assertEqual(sorted(p.name for p in self.package.iterdir()), sorted({"_engine.pyi", target.name}))
        self.assertEqual((self.package / "_engine.pyi").read_text(), "stub", "the checkout's stub is left alone")

    def test_another_platforms_wheel_is_refused_and_nothing_is_touched(self):
        (self.package / f"_engine{HERE}").write_bytes(b"current")
        w = wheel(self.tmp, {f"citar/_engine{ELSEWHERE}": b"foreign"})
        with self.assertRaisesRegex(unpack_ext.Refused, "another platform"):
            unpack_ext.unpack(w, self.package)
        self.assertEqual((self.package / f"_engine{HERE}").read_bytes(), b"current")

    def test_a_wheel_needs_exactly_one_library(self):
        for members in ({"citar/__init__.py": "x"},
                        {f"citar/_engine{HERE}": b"a", "citar/_engine.abi3.so": b"b", "citar/_engine.pyd": b"c"}):
            with self.subTest(members=sorted(members)):
                with self.assertRaisesRegex(unpack_ext.Refused, "expected one citar/_engine library"):
                    unpack_ext.unpack(wheel(self.tmp, members), self.package)
        # A library deeper down is not the module.
        w = wheel(self.tmp, {f"citar/_engine/inner{HERE}": b"x"})
        with self.assertRaises(unpack_ext.Refused):
            unpack_ext.unpack(w, self.package)
        broken = self.tmp / "broken.whl"
        broken.write_bytes(b"not a zip")
        with self.assertRaisesRegex(unpack_ext.Refused, "not a wheel"):
            unpack_ext.unpack(broken, self.package)

    def test_a_directory_must_hold_exactly_one_wheel(self):
        with self.assertRaisesRegex(unpack_ext.Refused, "found none"):
            unpack_ext.find_wheel(self.tmp)
        one = wheel(self.tmp, {})
        self.assertEqual(unpack_ext.find_wheel(self.tmp), one)
        self.assertEqual(unpack_ext.find_wheel(one), one)
        wheel(self.tmp, {}, name="citar-0.1.5-cp311-abi3-other.whl")
        with self.assertRaisesRegex(unpack_ext.Refused, "expected exactly one wheel"):
            unpack_ext.find_wheel(self.tmp)
        with self.assertRaisesRegex(unpack_ext.Refused, "no such"):
            unpack_ext.find_wheel(self.tmp / "missing")

    def test_a_library_that_does_not_import_is_refused_and_removed(self):
        # A real interpreter, in a checkout of its own whose citar package holds the wheel's library: bytes that are no
        # library fail the import, and the refusal takes the file away again.
        (self.package / "__init__.py").write_text("")
        w = wheel(self.tmp, {f"citar/_engine{HERE}": b"not a library"})
        with self.assertRaisesRegex(unpack_ext.Refused, "does not import"):
            unpack_ext.install(w, package=self.package)
        # The import may leave a __pycache__ of the package's __init__ behind; no library stays.
        self.assertEqual([p.name for p in self.package.iterdir() if p.name.startswith("_engine")], [])

    def test_the_import_check_decides_and_hears_test_ops(self):
        w = wheel(self.tmp, {f"citar/_engine{HERE}": b"lib"})
        calls = []
        original = unpack_ext.check_import
        self.addCleanup(setattr, unpack_ext, "check_import", original)

        def refuse(root, test_ops):
            calls.append((root, test_ops))
            raise unpack_ext.Refused("the extension was built without the test operations")

        unpack_ext.check_import = refuse
        with self.assertRaises(unpack_ext.Refused):
            unpack_ext.install(w, test_ops=True, package=self.package)
        self.assertEqual(calls, [(self.tmp, True)])
        self.assertEqual(list(self.package.iterdir()), [])
        unpack_ext.check_import = lambda root, test_ops: "label path True"
        self.assertEqual(unpack_ext.install(w, package=self.package),
                         (w, self.package / f"_engine{HERE}", "label path True"))

    def test_a_usage_error_and_a_refusal_exit_with_their_codes(self):
        import contextlib
        import io
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(unpack_ext.main([str(self.tmp / "missing")]), 1)
            with self.assertRaises(SystemExit) as stop:
                unpack_ext.main(["--no-such-flag"])
        self.assertEqual(stop.exception.code, 2)


class Requirements(unittest.TestCase):
    def test_all_is_the_dependencies_and_every_extra_it_names(self):
        data = tomllib.loads((ROOT / "pyproject.toml").read_text(encoding="utf-8"))
        project = data["project"]
        got = requirements.requirements(data, ["all"])
        want = list(project["dependencies"])
        for extra in ("anthropic", "openai", "mcp", "oauth", "keyring", "worker"):
            want += [r for r in project["optional-dependencies"][extra] if r not in want]
        self.assertEqual(got, want)
        self.assertFalse([r for r in got if r.lower().startswith("citar")])
        self.assertEqual(requirements.requirements(data, []), project["dependencies"])

    def test_own_extras_expand_once_and_unknown_ones_are_errors(self):
        data = {"project": {"name": "citar", "dependencies": ["a"],
                            "optional-dependencies": {"x": ["b", "a"], "y": ["citar[x]", "c"],
                                                      "loop": ["Citar [ loop , y ]", "d"]}}}
        self.assertEqual(requirements.requirements(data, ["loop"]), ["a", "b", "c", "d"])
        self.assertEqual(requirements.requirements(data, ["y", "x"]), ["a", "b", "c"])
        with self.assertRaises(KeyError):
            requirements.requirements(data, ["z"])
        data["project"]["optional-dependencies"]["bad"] = ["citar[z]"]
        with self.assertRaises(KeyError):
            requirements.requirements(data, ["bad"])


if __name__ == "__main__":
    unittest.main()
