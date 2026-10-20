"""The Windows installer and bundle, as far as this machine can check them (design/phase5-release.md P5.6, package
5-05; Inno Setup's compile and the silent install run on CI's Windows runner only, P5.18.1 rule 3):

- **installer/citar.iss:** a per-user install (no elevation; ``/CURRENTUSER`` taken on the command line), the three
  defines CI passes (``AppVersion``, ``AppVersionNumeric``, ``BundleDir``) each with a hand build's default and used
  where Inno needs it, ``OutputBaseFilename`` the release's asset name, the PATH task unchecked, nothing started by a
  silent install, the uninstaller's question about the person's data answered No by default, and the AppId the
  bundle's checks read the uninstall key by.
- **installer/python_licenses.py:** the closure of ``citar[all]`` walked through the installed metadata (markers,
  extras, a package's own extras), the build tools left out, the licence texts, CPython's and the bootloader's.
- **installer/spec_rules.py:** what citar.spec refuses: a ``citar`` or engine that is not the installed wheel's, an
  analysis that took anything of ``citar`` from elsewhere, output inside the checkout; and the notices it places.
"""
from __future__ import annotations

import importlib.metadata as md
import importlib.util
import os
import re
import shutil
import sys
import tempfile
import unittest
import zipfile
from unittest import mock
from pathlib import Path

from citar import __version__

ROOT = Path(__file__).resolve().parent.parent
ISS = ROOT / "installer" / "citar.iss"
APP_ID = "{9C1D2A54-5F1B-4A9D-9B5E-7C3F9A2E1D40}"


def load(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


PL = load(ROOT / "installer" / "python_licenses.py", "citar_test_python_licenses")
SR = load(ROOT / "installer" / "spec_rules.py", "citar_test_spec_rules")
RELEASE = load(ROOT / "scripts" / "release.py", "citar_test_release_for_iss")
CHECK_BUNDLE = load(ROOT / "scripts" / "ci" / "check_bundle.py", "citar_test_check_bundle")


def iss_text() -> str:
    return ISS.read_bytes().decode("utf-8")


def section(name: str) -> list:
    """The lines of an .iss section, continuation lines (a trailing backslash) joined."""
    lines, inside, held = [], False, ""
    for raw in iss_text().splitlines():
        line = raw.strip()
        if line.startswith("[") and line.endswith("]"):
            inside = line == f"[{name}]"
            continue
        if not inside or not line or line.startswith(";"):
            continue
        if line.endswith("\\"):
            held += line[:-1].strip() + " "
            continue
        lines.append(held + line)
        held = ""
    return lines


def setting(name: str) -> str:
    for line in section("Setup"):
        key, _, value = line.partition("=")
        if key.strip() == name:
            return value.strip()
    raise AssertionError(f"citar.iss sets no {name}")


class InnoScript(unittest.TestCase):
    def test_line_endings_are_crlf_throughout(self):
        raw = ISS.read_bytes()
        self.assertEqual(raw.count(b"\n"), raw.count(b"\r\n"), "every line of citar.iss ends CRLF")

    def test_it_installs_per_user_and_takes_currentuser_on_the_command_line(self):
        self.assertEqual(setting("PrivilegesRequired"), "lowest")
        self.assertIn("commandline", setting("PrivilegesRequiredOverridesAllowed").split())
        self.assertEqual(setting("DefaultDirName"), r"{autopf}\{#AppName}")

    def test_the_three_defines_have_defaults_and_are_used(self):
        text = iss_text()
        defaults = {}
        for name in ("AppVersion", "AppVersionNumeric", "BundleDir"):
            m = re.search(rf'^#ifndef {name}\r?\n\s*#define {name} "([^"]*)"\r?\n#endif\r?$', text, re.M)
            self.assertIsNotNone(m, f"no #ifndef {name} with its default")
            defaults[name] = m.group(1)
        self.assertEqual(defaults["AppVersion"], __version__)
        self.assertEqual(defaults["AppVersionNumeric"], re.sub(r"-.*$", "", __version__) + ".0")
        self.assertEqual(defaults["BundleDir"], r"..\dist\CITAR")
        self.assertEqual(setting("AppVersion"), "{#AppVersion}")
        self.assertEqual(setting("VersionInfoVersion"), "{#AppVersionNumeric}")
        files = section("Files")
        self.assertEqual(len(files), 1, files)
        self.assertTrue(files[0].startswith(r'Source: "{#BundleDir}\*"; DestDir: "{app}";'), files[0])
        self.assertIn("recursesubdirs", files[0])
        self.assertNotIn(r"dist\CITAR", files[0])

    def test_the_output_is_the_releases_asset_name(self):
        self.assertEqual(setting("OutputBaseFilename"), "CITAR-{#AppVersion}-setup")
        for version in ("0.1.6", "0.1.6-rc.1"):
            windows = [a["name"] for a in RELEASE.assets(version) if a["group"] == "windows"
                       and not a["name"].endswith(".sha256")]
            self.assertIn(f"CITAR-{version}-setup.exe", windows)

    def test_the_path_task_is_unchecked_and_a_silent_install_starts_nothing(self):
        tasks = {re.search(r'Name: "([^"]+)"', line).group(1): line for line in section("Tasks")}
        self.assertIn("addtopath", tasks)
        self.assertRegex(tasks["addtopath"], r"Flags:\s*unchecked")
        for line in section("Run"):
            self.assertIn("skipifsilent", line)

    def test_the_uninstaller_keeps_the_persons_data_unless_asked(self):
        code = "\n".join(section("Code"))
        m = re.search(r"if SuppressibleMsgBox\((?P<text>.*?)mbConfirmation, (?P<flags>[^,]*), (?P<silent>\w+)\) = IDYES "
                      r"then\s*DelTree\(StatePath", code, re.S)
        self.assertIsNotNone(m, "the data is deleted only on a Yes to the question")
        self.assertIn("MB_DEFBUTTON2", m.group("flags"), "No is the default button")
        self.assertIn("MB_YESNO", m.group("flags"))
        # a silent uninstall (/VERYSILENT /SUPPRESSMSGBOXES: winget, CI) is not asked, and keeps the data; a plain
        # MsgBox is never suppressed, and would wait for a click
        self.assertEqual(m.group("silent"), "IDNO")
        self.assertNotRegex(code, r"(?<!Suppressible)MsgBox\(")
        self.assertIn(r"StatePath := ExpandConstant('{localappdata}\CITAR');", code)

    def test_the_app_id_is_the_uninstall_keys(self):
        self.assertEqual(setting("AppId"), "{" + APP_ID)
        self.assertEqual(CHECK_BUNDLE.app_id(ISS), APP_ID)


def dist_info(site: Path, name: str, version: str, requires=(), meta=(), files=None, record=True) -> Path:
    """A distribution's metadata folder under ``site``: METADATA with its requirements and extra fields, ``files``
    (relative path -> text) inside the folder, and a RECORD naming them."""
    folder = site / f"{name.replace('-', '_')}-{version}.dist-info"
    folder.mkdir(parents=True)
    lines = ["Metadata-Version: 2.4", f"Name: {name}", f"Version: {version}", *meta,
             *(f"Requires-Dist: {r}" for r in requires)]
    (folder / "METADATA").write_text("\n".join(lines) + "\n", encoding="utf-8")
    for rel, text in (files or {}).items():
        (folder / rel).parent.mkdir(parents=True, exist_ok=True)
        (folder / rel).write_text(text, encoding="utf-8", newline="")
    if record:
        rows = [f"{folder.name}/{rel},," for rel in ["METADATA", *(files or {})]]
        (folder / "RECORD").write_text("\n".join(rows) + "\n", encoding="utf-8")
    return folder


MIT = "MIT License\n\nCopyright (c) the authors\n\nPermission is hereby granted...\n"


class PythonLicenses(unittest.TestCase):
    def setUp(self):
        self.site = Path(tempfile.mkdtemp(prefix="citar-pylic-"))
        self.addCleanup(shutil.rmtree, self.site, ignore_errors=True)
        s = self.site
        dist_info(s, "citar", "0.1.6", requires=[
            "alpha>=1", 'beta[x]; extra == "all"', 'gamma; sys_platform == "no-such-platform"',
            'citar[sub]; extra == "all"', 'delta; extra == "sub"', 'omega; extra == "server"'])
        dist_info(s, "alpha", "1.0", requires=["epsilon"],
                  meta=["License-Expression: MIT", "License-File: LICENSE"], files={"licenses/LICENSE": MIT})
        dist_info(s, "Beta", "2.0", requires=['zeta; extra == "x"', 'eta; extra == "y"'],
                  meta=["License: BSD", "Classifier: License :: OSI Approved :: BSD License"],
                  files={"LICENSE.txt": "BSD text\r\n", "NOTICE": "a notice\n", "entry_points.txt": "[x]\n"})
        dist_info(s, "delta", "3.0", meta=["Classifier: License :: OSI Approved :: Apache Software License"])
        dist_info(s, "epsilon", "4.0", meta=["License-Expression: MIT", "License-File: COPYING"],
                  files={"licenses/COPYING": MIT})
        dist_info(s, "zeta", "5.0", meta=["License: " + "Long licence text " * 20])
        for name in ("eta", "gamma", "omega", "pillow"):
            dist_info(s, name, "9.0", meta=["License-Expression: MIT"])
        dist_info(s, "pyinstaller", "6.22.3", meta=["License-Expression: GPL-2.0-or-later WITH Bootloader-exception"],
                  files={"COPYING.txt": "GPL, with the bootloader exception\n"})
        self.dists = PL.installed([str(s)])

    def test_the_closure_follows_markers_and_extras_and_leaves_the_rest_out(self):
        found = PL.closure(self.dists)
        self.assertEqual(sorted(found), ["alpha", "beta", "delta", "epsilon", "zeta"])
        # the server extra alone: no beta, no delta, but omega
        self.assertEqual(sorted(PL.closure(self.dists, extras=("server",))), ["alpha", "epsilon", "omega"])
        self.assertEqual(sorted(PL.closure(self.dists, extras=())), ["alpha", "epsilon"])

    def test_a_requirement_not_installed_is_refused(self):
        dists = dict(self.dists)
        del dists["epsilon"]
        with self.assertRaisesRegex(PL.Failed, "epsilon is required but not installed"):
            PL.closure(dists)

    def test_licences_as_the_metadata_states_them(self):
        d = self.dists
        self.assertEqual(PL.licence_of(d["alpha"]), "MIT")
        self.assertEqual(PL.licence_of(d["beta"]), "BSD; BSD License")
        self.assertEqual(PL.licence_of(d["delta"]), "Apache Software License")
        self.assertTrue(PL.licence_of(d["zeta"]).endswith("..."))
        self.assertLessEqual(len(PL.licence_of(d["zeta"])), 120)
        self.assertEqual([rel for rel, _ in PL.licence_files(d["alpha"])], ["licenses/LICENSE"])
        self.assertEqual([rel for rel, _ in PL.licence_files(d["beta"])], ["LICENSE.txt", "NOTICE"])
        self.assertEqual(PL.licence_files(d["delta"]), [])

    def test_the_file_lists_the_closure_cpython_and_the_bootloader_and_no_build_tool(self):
        found = PL.closure(self.dists)
        text = PL.render(found, ("the installation", "PSF LICENSE AGREEMENT\r\n"), PL.bootloader_licence(self.dists),
                         "3.12.10")
        self.assertEqual(text, PL.render(found, ("the installation", "PSF LICENSE AGREEMENT\r\n"),
                                         PL.bootloader_licence(self.dists), "3.12.10"))
        self.assertNotIn("\r", text)
        index = text.split("\nIndex\n-----\n", 1)[1].split("\n\n", 1)[0].splitlines()
        self.assertEqual(index, ["alpha 1.0: MIT", "Beta 2.0: BSD; BSD License",
                                 "delta 3.0: Apache Software License", "epsilon 4.0: MIT",
                                 f"zeta 5.0: {PL.licence_of(self.dists['zeta'])}",
                                 "CPython 3.12.10: PSF-2.0 (Python Software Foundation License Version 2)",
                                 "PyInstaller 6.22.3's bootloader: GPL-2.0-or-later WITH Bootloader-exception"])
        for absent in ("pillow", "\neta 9.0", "gamma", "omega", "\npyinstaller 6.22.3:"):
            self.assertNotIn(absent, text)
        self.assertIn("---- alpha 1.0: licenses/LICENSE ----\n" + MIT.rstrip("\n"), text)
        self.assertIn("---- epsilon 4.0: licenses/COPYING: the same text as alpha 1.0: licenses/LICENSE ----", text)
        self.assertIn("---- Beta 2.0: LICENSE.txt ----\nBSD text\n", text)
        self.assertIn("Files: none: its installed metadata holds no licence file", text)
        self.assertIn("---- CPython 3.12.10: LICENSE.txt ----\nPSF LICENSE AGREEMENT\n", text)
        self.assertIn("---- PyInstaller 6.22.3: COPYING.txt ----\nGPL, with the bootloader exception\n", text)

    def test_without_pyinstaller_there_is_no_bootloader_licence(self):
        dists = dict(self.dists)
        del dists["pyinstaller"]
        with self.assertRaisesRegex(PL.Failed, "PyInstaller is not installed"):
            PL.bootloader_licence(dists)

    def test_this_interpreters_cpython_licence(self):
        where, text = PL.cpython_licence()
        self.assertIn(where, ("the installation", "the standard library"))
        self.assertIn("PYTHON SOFTWARE FOUNDATION LICENSE", text.upper())

    def test_the_build_writes_the_file(self):
        text, names = PL.build([str(self.site)])
        self.assertEqual(names, ["alpha", "beta", "delta", "epsilon", "zeta"])
        self.assertTrue(text.startswith(PL.HEADER.rstrip("\n") + "\n\nIndex\n-----\nalpha 1.0: MIT\n"))
        self.assertIn("\nCPython ", text)
        self.assertIn("PyInstaller 6.22.3's bootloader", text)


class CheckBundle(unittest.TestCase):
    """scripts/ci/check_bundle.py's parts that need no bundle (the bundle's own checks are the package's gate and the
    windows job's)."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="citar-checkbundle-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)

    def test_the_zip_and_the_sums_are_the_releases_windows_assets(self):
        bundle = self.tmp / "dist" / "CITAR"
        for rel in ("citar.exe", "citar-play.exe", "LICENSE", "_internal/citar/_engine.pyd"):
            (bundle / rel).parent.mkdir(parents=True, exist_ok=True)
            (bundle / rel).write_bytes(rel.encode())
        out = self.tmp / "windows"
        self.assertEqual(CHECK_BUNDLE.main(["zip", "--dir", str(bundle), "--version", "0.1.6-rc.1", "--out", str(out)]), 0)
        setup = out / "CITAR-0.1.6-rc.1-setup.exe"
        setup.write_bytes(b"an installer")
        self.assertEqual(CHECK_BUNDLE.main(["sums", str(setup)]), 0)
        windows = [a["name"] for a in RELEASE.assets("0.1.6-rc.1") if a["group"] == "windows"]
        self.assertEqual(sorted(p.name for p in out.iterdir()),
                         sorted(f"{n}{s}" for n in windows for s in ("", ".sha256")))
        with zipfile.ZipFile(out / "CITAR-0.1.6-rc.1-windows-x64.zip") as z:
            self.assertEqual(sorted(z.namelist()), ["CITAR/LICENSE", "CITAR/_internal/citar/_engine.pyd",
                                                    "CITAR/citar-play.exe", "CITAR/citar.exe"])
            self.assertEqual({i.date_time for i in z.infolist()}, {CHECK_BUNDLE.ZIP_DATE})
        first = (out / "CITAR-0.1.6-rc.1-windows-x64.zip").read_bytes()
        CHECK_BUNDLE.main(["zip", "--dir", str(bundle), "--version", "0.1.6-rc.1", "--out", str(out)])
        self.assertEqual((out / "CITAR-0.1.6-rc.1-windows-x64.zip").read_bytes(), first, "the same files, the same zip")
        for name in windows:
            side = (out / f"{name}.sha256").read_text(encoding="ascii")
            self.assertEqual(side, CHECK_BUNDLE.sha256(out / name) + "\n")

    def test_the_runs_state_is_isolated(self):
        with mock.patch.dict(os.environ, {"CITAR_MODE": "server", "CITAR_DB_URL": "sqlite:///C:/real.db"}):
            env = CHECK_BUNDLE.isolated_env(self.tmp)
        self.assertNotIn("CITAR_MODE", env)
        self.assertNotIn("CITAR_DB_URL", env)
        for name in ("CITAR_STATE_DIR", "CITAR_DATA_DIR", "CITAR_SAVE_DIR", "CITAR_CONFIG_DIR", "CITAR_BENCH_DIR",
                     "CITAR_KEYS_FILE"):
            self.assertTrue(SR.within(Path(env[name]), self.tmp / "state"), name)
        self.assertEqual(env["PYTHON_KEYRING_BACKEND"], "keyring.backends.fail.Keyring")
        if os.name == "nt":
            for name in ("APPDATA", "LOCALAPPDATA"):
                self.assertTrue(SR.within(Path(env[name]), self.tmp / "state" / "user"), name)

    @unittest.skipUnless(os.name == "nt", "the uninstall key is Windows' registry")
    def test_an_uninstall_with_no_key_and_no_file_left_passes_and_a_file_left_fails(self):
        iss = self.tmp / "other.iss"
        iss.write_text("AppId={{00000000-0000-0000-0000-000000000005}\n", encoding="utf-8")
        self.assertIsNone(CHECK_BUNDLE.uninstall_key(iss))
        folder = self.tmp / "installed"
        (folder / "_internal").mkdir(parents=True)
        self.assertEqual(CHECK_BUNDLE.main(["uninstalled", "--dir", str(folder), "--iss", str(iss), "--wait", "0"]), 0)
        (folder / "citar.exe").write_bytes(b"left")
        self.assertEqual(CHECK_BUNDLE.main(["uninstalled", "--dir", str(folder), "--iss", str(iss), "--wait", "0"]), 1)


def toc(*entries) -> list:
    return [(name, str(source) if source else source, kind) for name, source, kind in entries]


class SpecRules(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="citar-spec-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.package = self.tmp / "venv" / "Lib" / "site-packages" / "citar"
        (self.package / "jobs").mkdir(parents=True)
        self.checkout = self.tmp / "checkout"
        (self.checkout / "citar").mkdir(parents=True)

    def good(self):
        p = self.package
        pure = toc(("citar", p / "__init__.py", "PYMODULE"), ("citar.jobs.host", p / "jobs" / "host.py", "PYMODULE"),
                   ("uvicorn", self.tmp / "venv" / "uvicorn" / "__init__.py", "PYMODULE"))
        binaries = toc(("citar\\_engine.pyd", p / "_engine.pyd", "EXTENSION"),
                       ("python312.dll", self.tmp / "python312.dll", "BINARY"))
        datas = toc(("citar\\web\\index.html", p / "web" / "index.html", "DATA"))
        return pure, binaries, datas

    def test_an_analysis_of_the_installed_package_passes(self):
        self.assertEqual(SR.check_analysis(*self.good(), self.package, self.checkout), [])

    def test_anything_of_citar_from_elsewhere_fails_naming_it(self):
        pure, binaries, datas = self.good()
        checkout_module = [("citar.paths", str(self.checkout / "citar" / "paths.py"), "PYMODULE")]
        problems = SR.check_analysis(pure + checkout_module, binaries, datas, self.package, self.checkout)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("the module citar.paths comes from", problems[0])
        other_engine = [("citar/_engine.pyd", str(self.tmp / "citar-ext" / "_engine.pyd"), "EXTENSION")]
        problems = SR.check_analysis(pure, other_engine, datas, self.package, self.checkout)
        self.assertTrue(any("the extension citar/_engine.pyd comes from" in p for p in problems), problems)
        stray_data = [("citar/web/x.js", str(self.checkout / "citar" / "web" / "x.js"), "DATA")]
        problems = SR.check_analysis(pure, binaries, datas + stray_data, self.package, self.checkout)
        self.assertTrue(any("the data file citar/web/x.js" in p for p in problems), problems)

    def test_every_data_file_of_the_package_must_be_in_the_bundle(self):
        """0.1.5's spec listed the data it took, and missed jobs/costs.json once the package gained it."""
        p = self.package
        for rel in ("web/index.html", "jobs/costs.json", "jobs/host.py", "migrations/versions/a1.py",
                    "migrations/script.py.mako", "_engine.pyd", "_engine.pyi", "jobs/__pycache__/host.cpython-312.pyc",
                    "data/collectors/collect_hardware.sh"):
            (p / rel).parent.mkdir(parents=True, exist_ok=True)
            (p / rel).write_bytes(b"x")
        required = SR.package_data(p)
        self.assertEqual(required, {"web/index.html", "jobs/costs.json", "migrations/versions/a1.py",
                                    "migrations/script.py.mako", "data/collectors/collect_hardware.sh"})
        pure, binaries, datas = self.good()
        datas = [(f"citar\\{rel.replace('/', chr(92))}", str(p / rel), "DATA") for rel in sorted(required)]
        self.assertEqual(SR.check_analysis(pure, binaries, datas, p, self.checkout, required), [])
        without = [d for d in datas if "costs.json" not in d[0]]
        self.assertEqual(SR.check_analysis(pure, binaries, without, p, self.checkout, required),
                         ["the package's data file jobs/costs.json is not in the bundle"])

    def test_no_engine_or_two_engines_or_no_package_fail(self):
        pure, binaries, datas = self.good()
        self.assertIn("the analysis took no citar/_engine.pyd: the bundle would have no engine",
                      SR.check_analysis(pure, binaries[1:], datas, self.package, self.checkout))
        twice = binaries + [("citar/_engine.pyd", str(self.package / "_engine.pyd"), "EXTENSION")]
        self.assertTrue(any("2 times" in p for p in SR.check_analysis(pure, twice, datas, self.package,
                                                                      self.checkout)))
        self.assertIn("the analysis took no citar package at all",
                      SR.check_analysis(pure[1:], binaries, datas, self.package, self.checkout))

    def test_the_checkouts_citar_and_the_dev_extension_are_refused(self):
        with self.assertRaisesRegex(SR.Refused, "CITAR_EXT_DIR is set"):
            SR.installed_package(ROOT, "C:/dev/target/x/citar-ext")
        # this suite imports citar from the checkout: exactly what the spec must refuse to bundle
        with self.assertRaisesRegex(SR.Refused, "citar resolves to the checkout"):
            SR.installed_package(ROOT, None)

    def test_output_inside_the_checkout_is_refused(self):
        with self.assertRaisesRegex(SR.Refused, "--distpath .* is inside the checkout"):
            SR.outside(self.checkout, **{"--distpath": str(self.checkout / "dist"), "--workpath": str(self.tmp)})
        SR.outside(self.checkout, **{"--distpath": str(self.tmp / "dist"), "--workpath": str(self.tmp / "work")})

    def test_the_notices_come_from_the_wheel_and_are_placed_at_the_root(self):
        site = self.tmp / "site"
        dist_info(site, "citar", "0.1.6", files={"licenses/LICENSE": "MPL", "licenses/NOTICE.md": "notice",
                                                 "licenses/packaging/THIRD-PARTY-LICENSES.txt": "crates"})
        (self.checkout / "README.md").write_text("readme", encoding="utf-8")
        dist = next(iter(md.distributions(path=[str(site)])))
        found = SR.notices(self.checkout, dist)
        self.assertEqual(sorted(found), ["LICENSE", "NOTICE.md", "README.md", "THIRD-PARTY-LICENSES.txt"])
        self.assertTrue(SR.within(found["LICENSE"], site))
        self.assertEqual(found["README.md"], self.checkout / "README.md")
        bundle = self.tmp / "dist" / "CITAR"
        bundle.mkdir(parents=True)
        with self.assertRaisesRegex(SR.Refused, "holds no citar.exe"):
            SR.place(bundle, found)
        (bundle / "citar.exe").write_bytes(b"")
        self.assertEqual(SR.place(bundle, found), sorted(found))
        self.assertEqual((bundle / "THIRD-PARTY-LICENSES.txt").read_text(encoding="utf-8"), "crates")
        # a wheel without the third notice takes the checkout's; with neither, the spec refuses
        site2 = self.tmp / "site2"
        dist_info(site2, "citar", "0.1.6", files={"licenses/LICENSE": "MPL", "licenses/NOTICE.md": "notice"})
        dist2 = next(iter(md.distributions(path=[str(site2)])))
        with self.assertRaisesRegex(SR.Refused, "no THIRD-PARTY-LICENSES.txt"):
            SR.notices(self.checkout, dist2)
        (self.checkout / "packaging").mkdir()
        (self.checkout / "packaging" / "THIRD-PARTY-LICENSES.txt").write_text("crates", encoding="utf-8")
        self.assertEqual(SR.notices(self.checkout, dist2)["THIRD-PARTY-LICENSES.txt"],
                         self.checkout / "packaging" / "THIRD-PARTY-LICENSES.txt")


if __name__ == "__main__":
    unittest.main()
