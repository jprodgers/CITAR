"""The CI glue in scripts/ci/: unpack_ext.py, which puts the build-ext job's extension into the checkout for the test
jobs, requirements.py, which lists what to install without building the package, and check_dist.py, which holds a
built wheel or source distribution to the checkout (crates/citar-engine/DESIGN.md P2.6.8)."""
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
check_dist = load("check_dist")

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


#: A checkout's tracked files, and a [tool.maturin] table over them, for check_dist's tests.
TRACKED = {
    "citar/__init__.py", "citar/_engine.pyi", "citar/web/index.html", "citar/web/js/app.js",
    "citar/data/collectors/collect.sh", "tests/test_a.py", "tests/output/keep.txt", "scripts/ci/x.py",
    "pyproject.toml", "Cargo.toml", "Cargo.lock", "crates/citar-py/Cargo.toml", "crates/citar-py/src/lib.rs",
    "crates/citar-engine/data/game.json", "crates/citar-engine/data/ruleset/units.json",
    "refcheck/intended.toml", "refcheck/README.md", "README.md",
}
MATURIN = {
    "include": [{"path": "citar/web/**/*", "format": ["sdist", "wheel"]},
                {"path": "citar/data/collectors/*", "format": ["sdist", "wheel"]},
                {"path": "tests/**/*", "format": "sdist"},
                {"path": "refcheck/intended.toml", "format": "sdist"}],
    "exclude": ["**/__pycache__/**", {"path": "tests/output/**/*", "format": "sdist"}],
}
TAG = "cp311-abi3-manylinux_2_28_x86_64"


class CheckDist(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="citar-dist-"))
        self.addCleanup(__import__("shutil").rmtree, self.tmp, ignore_errors=True)

    def a_wheel(self, drop=(), add=None, tag=TAG, name_tag=TAG, wheel_meta=None):
        members = {p: "x" for p in TRACKED if p.startswith("citar/") and p not in drop}
        members["citar/_engine.abi3.so"] = b"lib"
        info = "citar-0.1.5.dist-info/"
        members.update({info + "WHEEL": wheel_meta or f"Wheel-Version: 1.0\nTag: {tag}\n",
                        info + "METADATA": "Name: citar", info + "RECORD": ""})
        for member in drop:
            members.pop(member, None)
        members.update(add or {})
        return wheel(self.tmp, members, name=f"citar-0.1.5-{name_tag}.whl")

    def a_sdist(self, drop=(), add=(), top="citar-0.1.5", name="citar-0.1.5.tar.gz"):
        import io
        import tarfile
        path = self.tmp / name
        files = [p for p in sorted(TRACKED) if p not in drop and p != "tests/output/keep.txt"] + ["PKG-INFO"]
        with tarfile.open(path, "w:gz") as archive:
            for member in [f"{top}/{p}" for p in files if p not in drop] + list(add):
                data = member.encode()
                entry = tarfile.TarInfo(member)
                entry.size = len(data)
                archive.addfile(entry, io.BytesIO(data))
        return path

    def wheel_problems(self, path, **kw):
        return check_dist.check_wheel(path, TRACKED, MATURIN, **kw)

    def test_a_wheel_as_built_passes(self):
        path = self.a_wheel()
        self.assertEqual(self.wheel_problems(path), [])
        self.assertEqual(self.wheel_problems(path, tag=TAG, library="citar/_engine.abi3.so"), [])

    def test_a_wheel_missing_a_file_or_holding_a_stray_one_fails(self):
        problems = self.wheel_problems(self.a_wheel(drop=["citar/web/js/app.js"],
                                                    add={"citar/notes.txt": "x", "citar/__pycache__/a.pyc": "x"}))
        self.assertEqual(problems, ["missing (git tracks it under citar/): citar/web/js/app.js",
                                    "stray (git does not track it): citar/__pycache__/a.pyc",
                                    "stray (git does not track it): citar/notes.txt"])
        outside = self.wheel_problems(self.a_wheel(add={"tests/test_a.py": "x", "citar-0.1.5.data/x": "x"}))
        self.assertEqual(outside, ["outside citar/ and the dist-info: citar-0.1.5.data/x",
                                   "outside citar/ and the dist-info: tests/test_a.py"])

    def test_a_wheel_needs_its_one_library(self):
        none = self.wheel_problems(self.a_wheel(drop=["citar/_engine.abi3.so"]))
        self.assertEqual(none, ["expected one citar/_engine library, found none"])
        two = self.wheel_problems(self.a_wheel(add={"citar/_engine.pyd": b"lib"}))
        self.assertEqual(two, ["expected one citar/_engine library, found citar/_engine.abi3.so, citar/_engine.pyd"])
        self.assertEqual(self.wheel_problems(self.a_wheel(), library="citar/_engine.pyd"),
                         ["its library is citar/_engine.abi3.so, not citar/_engine.pyd"])

    def test_a_wheel_has_one_tag_and_its_name_agrees(self):
        self.assertEqual(self.wheel_problems(self.a_wheel(), tag="cp311-abi3-win_amd64"),
                         [f"its tag is {TAG}, not cp311-abi3-win_amd64"])
        other = "cp312-cp312-manylinux_2_28_x86_64"
        self.assertEqual(self.wheel_problems(self.a_wheel(tag=other, name_tag=other)),
                         [f"its tag is {other}, not an abi3 one for Python 3.11 and later"])
        self.assertEqual(self.wheel_problems(self.a_wheel(name_tag="cp311-abi3-linux_x86_64")),
                         [f"its file name says cp311-abi3-linux_x86_64, its WHEEL {TAG}"])
        two = self.wheel_problems(self.a_wheel(wheel_meta=f"Tag: {TAG}\nTag: cp311-abi3-linux_x86_64\n"))
        self.assertEqual(two, [f"expected one Tag in citar-0.1.5.dist-info/WHEEL, found [{TAG!r}, "
                               "'cp311-abi3-linux_x86_64']"])
        self.assertEqual(self.wheel_problems(self.a_wheel(drop=["citar-0.1.5.dist-info/RECORD"])),
                         ["no citar-0.1.5.dist-info/RECORD"])

    def test_a_wheel_holding_a_member_twice_fails(self):
        import warnings
        path = self.a_wheel()
        with warnings.catch_warnings(), zipfile.ZipFile(path, "a") as z:
            warnings.simplefilter("ignore")   # zipfile warns of the duplicate it is asked to write
            z.writestr("citar/__init__.py", "again")
        self.assertEqual(self.wheel_problems(path), ["citar/__init__.py is in it 2 times"])
        broken = self.tmp / "citar-0.1.5-x-y-z.whl"
        broken.write_bytes(b"not a zip")
        self.assertRegex(self.wheel_problems(broken)[0], "not a wheel")
        self.assertRegex(self.wheel_problems(self.tmp / "citar.whl")[0], "not a wheel's file name")

    def test_a_sdist_as_built_passes(self):
        self.assertEqual(check_dist.check_sdist(self.a_sdist(), TRACKED, MATURIN), [])

    def test_a_sdist_with_a_stray_or_a_missing_file_fails(self):
        problems = check_dist.check_sdist(
            self.a_sdist(drop=["citar/web/index.html", "tests/test_a.py", "Cargo.lock", "PKG-INFO"],
                         add=["citar-0.1.5/refcheck/corpus/game.json", "elsewhere/x"]),
            TRACKED, MATURIN)
        self.assertEqual(problems, [
            "elsewhere/x is outside citar-0.1.5/",
            "no PKG-INFO",
            "stray (git does not track it): refcheck/corpus/game.json",
            "missing (git tracks it under citar/): citar/web/index.html",
            "missing (an include names it): tests/test_a.py",
            "missing (a build from it starts there): Cargo.lock",
        ])
        # Not named by an include (refcheck/README.md), or taken away by an exclude (tests/output/): not missed.
        self.assertEqual(check_dist.check_sdist(self.a_sdist(drop=["refcheck/README.md"]), TRACKED, MATURIN), [])
        self.assertEqual(check_dist.check_sdist(self.a_sdist(top="citar-0.1.4"), TRACKED, MATURIN)[0],
                         "citar-0.1.4/Cargo.lock is outside citar-0.1.5/")

    def test_a_sdist_without_the_ruleset_fails_and_a_wheel_with_it_too(self):
        """The engine compiles the ruleset in from crates/citar-engine/data/, which the source distribution must carry
        though no include names it; a wheel carries none of it (citar/data holds only the collectors)."""
        problems = check_dist.check_sdist(self.a_sdist(drop=["crates/citar-engine/data/game.json"]), TRACKED, MATURIN)
        self.assertEqual(problems, ["missing (a build from it compiles it in): crates/citar-engine/data/game.json"])
        problems = self.wheel_problems(self.a_wheel(add={"citar/data/ruleset/units.json": "{}"}))
        self.assertEqual(problems, ["stray (git does not track it): citar/data/ruleset/units.json"])

    def test_globs_read_as_maturin_reads_them(self):
        def matches(pattern, path):
            return bool(check_dist.glob_regex(pattern).fullmatch(path))
        self.assertTrue(matches("citar/web/**/*", "citar/web/index.html"))
        self.assertTrue(matches("citar/web/**/*", "citar/web/js/app.js"))
        self.assertFalse(matches("citar/web/**/*", "citar/webx/index.html"))
        self.assertTrue(matches("**/__pycache__/**", "citar/__pycache__/a.pyc"))
        self.assertTrue(matches("**/__pycache__/**", "__pycache__/a.pyc"))
        self.assertTrue(matches("citar/data/collectors/*", "citar/data/collectors/collect.sh"))
        self.assertFalse(matches("citar/data/collectors/*", "citar/data/collectors/sub/collect.sh"))
        self.assertTrue(matches("**/*.pyc", "a/b/c.pyc"))
        self.assertTrue(matches("alembic.ini", "alembic.ini"))
        self.assertFalse(matches("alembic.ini", "alembic_ini"))
        with self.assertRaisesRegex(check_dist.Refused, "does not read"):
            check_dist.glob_regex("citar/[ab].py")

    def test_the_real_pyproject_reads(self):
        maturin = tomllib.loads((ROOT / "pyproject.toml").read_text(encoding="utf-8"))["tool"]["maturin"]
        tracked = {"rust-toolchain.toml", "citar/web/index.html", "tests/test_a.py", "installer/output/setup.exe",
                   "installer/citar.iss", "refcheck/corpus/x.json", "citar/__pycache__/a.pyc"}
        self.assertEqual(check_dist.named(tracked, maturin, "sdist"),
                         {"rust-toolchain.toml", "citar/web/index.html", "tests/test_a.py", "installer/citar.iss"})
        self.assertEqual(check_dist.named(tracked, maturin, "wheel"), {"citar/web/index.html"})

    def test_exit_codes(self):
        import contextlib
        import io
        original = check_dist.tracked_files
        self.addCleanup(setattr, check_dist, "tracked_files", original)
        check_dist.tracked_files = lambda root=None: {p for p in TRACKED if p.startswith("citar/")}
        good = self.a_wheel()
        with contextlib.redirect_stdout(io.StringIO()) as out, contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(check_dist.main(["wheel", str(self.tmp), "--tag", TAG,
                                              "--library", "citar/_engine.abi3.so"]), 0)
            self.assertEqual(check_dist.main(["wheel", str(good), "--library", "citar/_engine.pyd"]), 1)
            self.assertEqual(check_dist.main(["sdist", str(self.tmp)]), 2)   # no .tar.gz there
            self.assertEqual(check_dist.main(["wheel", str(self.tmp / "missing")]), 2)
            with self.assertRaises(SystemExit) as stop:
                check_dist.main([])
        self.assertEqual(stop.exception.code, 2)
        self.assertIn(good.name, out.getvalue())

    @unittest.skipUnless((ROOT / ".git").exists(), "not a git checkout (an extracted source distribution)")
    def test_tracked_files_are_gits(self):
        tracked = check_dist.tracked_files()
        self.assertIn("pyproject.toml", tracked)
        self.assertIn("citar/__init__.py", tracked)
        self.assertFalse([p for p in tracked if "\\" in p or p.startswith("/")])


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
