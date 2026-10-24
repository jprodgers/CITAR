"""The Windows installer and bundle, as far as this machine can check them (design/phase5-release.md P5.6, package
5-05; Inno Setup's compile and the silent install run on CI's Windows runner only, P5.18.1 rule 3):

- **installer/citar.iss:** a per-user install (no elevation; ``/CURRENTUSER`` taken on the command line), the three
  defines CI passes (``AppVersion``, ``AppVersionNumeric``, ``BundleDir``) each with a hand build's default and used
  where Inno needs it, ``OutputBaseFilename`` the release's asset name, the PATH task unchecked, nothing started by a
  silent install, the uninstaller's question about the person's data answered No by default, and the AppId the
  bundle's checks read the uninstall key by.
- **installer/python_licenses.py:** the closure of ``citar[all]`` walked through the installed metadata (markers,
  extras, a package's own extras), the build tools left out, the licence texts (CITAR's kept copy for a package whose
  metadata holds none, such as dnspython, and a refusal without one), CPython's, and PyInstaller's bootloader and
  run-time hooks' and its community hooks' run-time hooks'.
- **installer/spec_rules.py:** what citar.spec refuses: a ``citar`` or engine that is not the installed wheel's, an
  analysis that took anything of ``citar`` from elsewhere or any other file from outside ``citar[all]``'s closure,
  the Python the bundle embeds, Windows and PyInstaller's run-time files (setuptools, a DLL from a folder on the build
  machine's PATH), output inside the checkout; the Universal CRT left out, DLLs looked for only in the embedded Python
  and Windows; the audit of a built bundle; and the notices it places.
- **scripts/ci/check_bundle.py:** the zip and the sums, the isolated runs, the uninstall's check, PYTHON-LICENSES.txt's
  index, and what a bundle carries (no UCRT, no metadata of an unlisted distribution).
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


def dist_info(site: Path, name: str, version: str, requires=(), meta=(), files=None, record=True,
              package=()) -> Path:
    """A distribution's metadata folder under ``site``: METADATA with its requirements and extra fields, ``files``
    (relative path -> text) inside the folder, ``package`` (paths relative to ``site``) written beside it, and a
    RECORD naming them all."""
    folder = site / f"{name.replace('-', '_')}-{version}.dist-info"
    folder.mkdir(parents=True)
    lines = ["Metadata-Version: 2.4", f"Name: {name}", f"Version: {version}", *meta,
             *(f"Requires-Dist: {r}" for r in requires)]
    (folder / "METADATA").write_text("\n".join(lines) + "\n", encoding="utf-8")
    for rel, text in (files or {}).items():
        (folder / rel).parent.mkdir(parents=True, exist_ok=True)
        (folder / rel).write_text(text, encoding="utf-8", newline="")
    for rel in package:
        (site / rel).parent.mkdir(parents=True, exist_ok=True)
        (site / rel).write_bytes(rel.encode())
    if record:
        rows = [f"{folder.name}/{rel},," for rel in ["METADATA", *(files or {})]] + [f"{rel},," for rel in package]
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
                  files={"COPYING.txt": "GPL, with the bootloader exception; the run-time hooks Apache-2.0\n"})
        dist_info(s, "pyinstaller-hooks-contrib", "2026.8", meta=["License-File: LICENSE"],
                  files={"licenses/LICENSE": "GPL for the hooks, Apache-2.0 for the run-time hooks\n"})
        self.dists = PL.installed([str(s)])
        # delta's metadata holds no licence file: CITAR keeps a copy of its project's
        self.kept_dir = s / "kept"
        self.kept_dir.mkdir()
        (self.kept_dir / "delta.txt").write_text("Apache License, delta's copy\r\n", encoding="utf-8")
        (self.kept_dir / "zeta.txt").write_text("zeta's licence\n", encoding="utf-8")
        self.kept = {"delta": "delta.txt", "zeta": "zeta.txt"}

    def render(self, found=None, runtime=None):
        found = PL.closure(self.dists) if found is None else found
        return PL.render(found, ("the installation", "PSF LICENSE AGREEMENT\r\n"),
                         PL.runtime_licences(self.dists) if runtime is None else runtime, "3.12.10",
                         self.kept_dir, self.kept)

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

    def test_the_file_lists_the_closure_cpython_and_pyinstallers_run_time_and_no_build_tool(self):
        text = self.render()
        self.assertEqual(text, self.render())
        self.assertNotIn("\r", text)
        index = text.split("\nIndex\n-----\n", 1)[1].split("\n\n", 1)[0].splitlines()
        self.assertEqual(index, ["alpha 1.0: MIT", "Beta 2.0: BSD; BSD License",
                                 "delta 3.0: Apache Software License", "epsilon 4.0: MIT",
                                 f"zeta 5.0: {PL.licence_of(self.dists['zeta'])}",
                                 "CPython 3.12.10: PSF-2.0 (Python Software Foundation License Version 2)",
                                 "PyInstaller 6.22.3's bootloader and run-time hooks: "
                                 "GPL-2.0-or-later WITH Bootloader-exception; Apache-2.0",
                                 "pyinstaller-hooks-contrib 2026.8's run-time hooks: Apache-2.0"])
        for absent in ("pillow", "\neta 9.0", "gamma", "omega", "\npyinstaller 6.22.3:",
                       "\npyinstaller-hooks-contrib 2026.8:"):
            self.assertNotIn(absent, text)
        self.assertIn("---- alpha 1.0: licenses/LICENSE ----\n" + MIT.rstrip("\n"), text)
        self.assertIn("---- epsilon 4.0: licenses/COPYING: the same text as alpha 1.0: licenses/LICENSE ----", text)
        self.assertIn("---- Beta 2.0: LICENSE.txt ----\nBSD text\n", text)
        self.assertIn("---- CPython 3.12.10: LICENSE.txt ----\nPSF LICENSE AGREEMENT\n", text)
        self.assertIn("\nPyInstaller 6.22.3's bootloader and run-time hooks\n", text)
        self.assertIn("---- PyInstaller 6.22.3: COPYING.txt ----\nGPL, with the bootloader exception; the run-time "
                      "hooks Apache-2.0\n", text)
        self.assertIn("\npyinstaller-hooks-contrib 2026.8's run-time hooks\n", text)
        self.assertIn("---- pyinstaller-hooks-contrib 2026.8: licenses/LICENSE ----\nGPL for the hooks, Apache-2.0 "
                      "for the run-time hooks\n", text)

    def test_a_package_with_no_licence_file_takes_citars_copy_and_without_one_is_refused(self):
        """dnspython 2.9.0's wheel carries no licence file, and the ISC licence asks for its notice in every copy."""
        text = self.render()
        self.assertIn("Files: none in its installed metadata; CITAR's copy of its licence file, "
                      "installer/licenses/delta.txt\n---- delta 3.0: CITAR's copy of its licence file, "
                      "installer/licenses/delta.txt ----\nApache License, delta's copy\n", text)
        self.assertNotIn("Files: none: its installed metadata holds no licence file", text)
        with self.assertRaisesRegex(PL.Failed, r"delta 3\.0 \(Apache Software License\) holds no licence file in its "
                                               r"installed metadata and installer/licenses/ keeps no copy of it"):
            PL.render(PL.closure(self.dists), ("the installation", "PSF"), None, "3.12.10", self.kept_dir, {})
        (self.kept_dir / "delta.txt").unlink()
        with self.assertRaisesRegex(PL.Failed, "delta 3.0"):
            self.render()

    def test_citars_kept_licence_texts(self):
        """Each kept text is there, and dnspython's is its ISC licence with both of its copyright notices."""
        self.assertEqual(PL.KEPT, {"dnspython": "dnspython.txt"})
        for name, file in PL.KEPT.items():
            self.assertTrue((PL.KEPT_DIR / file).is_file(), file)
            self.assertEqual(PL.canonical(name), name)
        text = (PL.KEPT_DIR / "dnspython.txt").read_text(encoding="utf-8")
        self.assertTrue(text.startswith("ISC License\n\nCopyright (C) Dnspython Contributors\n"))
        self.assertIn("Copyright (C) 2001-2017 Nominum, Inc.\nCopyright (C) Google Inc.\n", text)
        words = " ".join(text.split())
        self.assertEqual(words.count("provided that the above copyright notice and this permission notice appear in "
                                     "all copies"), 2)

    def test_without_pyinstaller_there_is_no_bootloader_licence(self):
        dists = dict(self.dists)
        del dists["pyinstaller"]
        with self.assertRaisesRegex(PL.Failed, "PyInstaller is not installed"):
            PL.runtime_licences(dists)
        # without the community hooks there is only PyInstaller's section
        dists = dict(self.dists)
        del dists["pyinstaller-hooks-contrib"]
        self.assertEqual([part[0] for part in PL.runtime_licences(dists)], ["PyInstaller"])
        self.assertNotIn("pyinstaller-hooks-contrib", self.render(runtime=PL.runtime_licences(dists)))

    def test_this_interpreters_cpython_licence(self):
        where, text = PL.cpython_licence()
        self.assertIn(where, ("the installation", "the standard library"))
        self.assertIn("PYTHON SOFTWARE FOUNDATION LICENSE", text.upper())

    def test_the_build_writes_the_file(self):
        text, names = PL.build([str(self.site)], kept_dir=self.kept_dir, kept=self.kept)
        self.assertEqual(names, ["alpha", "beta", "delta", "epsilon", "zeta"])
        self.assertTrue(text.startswith(PL.HEADER.rstrip("\n") + "\n\nIndex\n-----\nalpha 1.0: MIT\n"))
        self.assertIn("\nCPython ", text)
        self.assertIn("PyInstaller 6.22.3's bootloader and run-time hooks", text)
        self.assertEqual(PL.shipped(self.dists), {"citar", "alpha", "beta", "delta", "epsilon", "zeta"})


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
        # The Windows asset's program is citar.exe on any system that writes the zip; a bundle without it is refused.
        (bundle / "citar.exe").unlink()
        self.assertEqual(CHECK_BUNDLE.main(["zip", "--dir", str(bundle), "--version", "0.1.6-rc.2", "--out", str(out)]), 1)
        self.assertFalse((out / "CITAR-0.1.6-rc.2-windows-x64.zip").exists())

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

    def test_the_licence_index_as_python_licenses_writes_it(self):
        site = self.tmp / "site"
        needed = ["fastapi", "uvicorn", "SQLAlchemy", "anthropic", "openai", "mcp", "Authlib", "keyring"]
        dist_info(site, "citar", "0.1.6", requires=needed)
        for name in needed:
            dist_info(site, name, "1.0", meta=["License-Expression: MIT", "License-File: LICENSE"],
                      files={"licenses/LICENSE": MIT})
        dist_info(site, "pyinstaller", "6.22.3", files={"COPYING.txt": "GPL with the exception; Apache-2.0\n"})
        dist_info(site, "pyinstaller-hooks-contrib", "2026.8", files={"LICENSE": "GPL; Apache-2.0\n"})
        text, _ = PL.build([str(site)])
        names, hooks = CHECK_BUNDLE.licence_index(text)
        self.assertEqual(sorted(names), sorted(n.lower() for n in needed))
        self.assertTrue(hooks)
        # 0.1.5's and 5-05's first index line for the bootloader is not this file's
        old = text.replace("'s bootloader and run-time hooks: ", "'s bootloader: ")
        with self.assertRaisesRegex(CHECK_BUNDLE.Failed, "names no PyInstaller bootloader and run-time hooks"):
            CHECK_BUNDLE.licence_index(old)
        tool = text.replace("\nCPython ", "\nsetuptools 80.0: MIT\nCPython ")
        with self.assertRaisesRegex(CHECK_BUNDLE.Failed, r"build tools the bundle does not ship: \['setuptools'\]"):
            CHECK_BUNDLE.licence_index(tool)
        with self.assertRaisesRegex(CHECK_BUNDLE.Failed, "does not list keyring"):
            CHECK_BUNDLE.licence_index(text.replace("\nkeyring 1.0: MIT", ""))

    def test_the_bundle_carries_no_ucrt_and_no_unlisted_metadata(self):
        """The first bundle carried 43 Universal CRT DLLs from a JDK's folder and setuptools' vendored packages."""
        bundle = self.tmp / "CITAR"
        for rel in ("citar.exe", "_internal/python312.dll", "_internal/VCRUNTIME140.dll",
                    "_internal/argon2_cffi-25.1.0.dist-info/METADATA", "_internal/jaraco.classes-3.4.0.dist-info/RECORD",
                    "_internal/citar-0.1.6.dist-info/METADATA"):
            (bundle / rel).parent.mkdir(parents=True, exist_ok=True)
            (bundle / rel).write_bytes(b"x")
        names = ["argon2-cffi", "jaraco.classes"]
        self.assertEqual(CHECK_BUNDLE.carried(bundle, names), 3)
        vendored = bundle / "_internal" / "setuptools" / "_vendor" / "packaging-24.2.dist-info" / "METADATA"
        vendored.parent.mkdir(parents=True)
        vendored.write_bytes(b"x")
        with self.assertRaisesRegex(CHECK_BUNDLE.Failed, "_internal/setuptools/_vendor/packaging-24.2.dist-info"):
            CHECK_BUNDLE.carried(bundle, names)
        vendored.unlink()
        for name in ("api-ms-win-crt-math-l1-1-0.dll", "UCRTBASE.DLL"):
            (bundle / "_internal" / name).write_bytes(b"x")
            with self.assertRaisesRegex(CHECK_BUNDLE.Failed, "1 Universal CRT DLL"):
                CHECK_BUNDLE.carried(bundle, names)
            (bundle / "_internal" / name).unlink()
        self.assertEqual(CHECK_BUNDLE.carried(bundle, names), 3)

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

    def test_the_universal_crt_is_left_out(self):
        """The first bundle took ucrtbase.dll and 42 api-ms-win-*.dll from a JDK's folder on this laptop's PATH."""
        binaries = toc(("api-ms-win-crt-math-l1-1-0.dll", "C:/jdk/bin/api-ms-win-crt-math-l1-1-0.dll", "BINARY"),
                       ("API-MS-WIN-CORE-FILE-L1-1-0.DLL", "C:/jdk/bin/x.dll", "BINARY"),
                       ("ucrtbase.dll", "C:/Windows/System32/ucrtbase.dll", "BINARY"),
                       ("python312.dll", "C:/Python312/python312.dll", "BINARY"),
                       ("VCRUNTIME140.dll", "C:/Python312/VCRUNTIME140.dll", "BINARY"),
                       ("win32\\api-ms-not-ucrt.pyd", "C:/x/api-ms-not-ucrt.pyd", "EXTENSION"))
        kept, dropped = SR.without_ucrt(binaries)
        self.assertEqual([n for n, _, _ in kept], ["python312.dll", "VCRUNTIME140.dll", "win32\\api-ms-not-ucrt.pyd"])
        self.assertEqual(len(dropped), 3)

    def test_dlls_are_looked_for_in_the_embedded_python_and_windows_only(self):
        # This system's root (C:\ on Windows, / elsewhere): a drive's colon would split on POSIX's path separator.
        root = os.path.abspath(os.sep)
        base = os.path.join(root, "Python312")
        windows = os.path.join(root, "Windows")
        self.assertEqual(SR.dll_path(base, windows).split(os.pathsep),
                         [base, os.path.join(base, "DLLs"), os.path.join(windows, "System32"), windows])
        self.assertEqual(SR.dll_path(base, None).split(os.pathsep), [base, os.path.join(base, "DLLs")])

    def sources(self):
        """A venv's site-packages with citar, a package of its closure, setuptools (a build tool) and PyInstaller's
        two distributions, the Python it embeds, and a JDK's folder."""
        site = self.tmp / "venv" / "Lib" / "site-packages"
        dist_info(site, "citar", "0.1.6", package=["citar/__init__.py", "citar/_engine.pyd"])
        dist_info(site, "argon2-cffi", "25.1.0", package=["argon2/__init__.py"])
        dist_info(site, "setuptools", "80.0", package=["setuptools/__init__.py", "_distutils_hack/__init__.py",
                                                       "setuptools/_vendor/packaging-24.2.dist-info/METADATA"])
        dist_info(site, "pyinstaller", "6.22.3", package=["PyInstaller/hooks/rthooks/pyi_rth_inspect.py",
                                                          "PyInstaller/loader/pyimod01_archive.py",
                                                          "PyInstaller/utils/hooks/__init__.py"])
        dist_info(site, "pyinstaller-hooks-contrib", "2026.8",
                  package=["_pyinstaller_hooks_contrib/rthooks/pyi_rth_pythoncom.py"])
        (site / "stray.py").write_text("", encoding="utf-8")
        base = self.tmp / "Python312"
        (base / "DLLs").mkdir(parents=True)
        owned = SR.owners(md.distributions(path=[str(site)]))
        return site, base, owned

    def test_every_file_comes_from_the_closure_the_embedded_python_windows_or_pyinstallers_run_time(self):
        site, base, owned = self.sources()
        shipped = {"citar", "argon2-cffi"}
        rules = {"owned": owned, "shipped": shipped, "site_packages": [site], "roots": [base, self.tmp / "Windows"]}
        good = {"module": toc(("citar", site / "citar" / "__init__.py", "PYMODULE"),
                              ("argon2", site / "argon2" / "__init__.py", "PYMODULE"),
                              ("asyncio", base / "Lib" / "asyncio" / "__init__.py", "PYMODULE"),
                              ("opentelemetry", "-", "PYMODULE"), ("nothing", None, "PYMODULE")),
                "binary": toc(("_ssl.pyd", base / "DLLs" / "_ssl.pyd", "EXTENSION"),
                              ("citar\\_engine.pyd", site / "citar" / "_engine.pyd", "EXTENSION"),
                              ("msvcp140.dll", self.tmp / "Windows" / "System32" / "msvcp140.dll", "BINARY"))}
        self.assertEqual(SR.check_sources(good, **rules), [])
        # PyInstaller's run-time files (its loader and run-time hooks, the community hooks' run-time hooks) are
        # covered by PYTHON-LICENSES.txt; the rest of PyInstaller is not
        scripts = {"script": toc(("pyi_rth_inspect", site / "PyInstaller/hooks/rthooks/pyi_rth_inspect.py", "PYSOURCE"),
                                 ("pyi_rth_pythoncom", site / "_pyinstaller_hooks_contrib/rthooks/pyi_rth_pythoncom.py",
                                  "PYSOURCE"))}
        self.assertEqual(SR.check_sources(scripts, **rules), [])
        bad = {"module": toc(("setuptools", site / "setuptools" / "__init__.py", "PYMODULE"),
                             ("_distutils_hack", site / "_distutils_hack" / "__init__.py", "PYMODULE"),
                             ("PyInstaller.utils.hooks", site / "PyInstaller/utils/hooks/__init__.py", "PYMODULE"),
                             ("stray", site / "stray.py", "PYMODULE"),
                             ("pip", base / "Lib" / "site-packages" / "pip" / "__init__.py", "PYMODULE")),
               "data file": toc(("setuptools\\_vendor\\packaging-24.2.dist-info\\METADATA",
                                 site / "setuptools/_vendor/packaging-24.2.dist-info/METADATA", "DATA")),
               "binary": toc(("libssl-3.dll", self.tmp / "jdk" / "bin" / "libssl-3.dll", "BINARY"),
                             ("msvcp140.dll", self.tmp / "jdk" / "bin" / "msvcp140.dll", "BINARY"))}
        problems = SR.check_sources(bad, **rules)
        self.assertEqual(len(problems), 5, problems)
        self.assertTrue(any(p.startswith("the module stray comes from") and "no installed distribution's RECORD" in p
                            for p in problems), problems)
        self.assertTrue(any(p.startswith("the module pip comes from") and "not the venv's" in p for p in problems))
        self.assertIn("1 file(s) of pyinstaller, which PYTHON-LICENSES.txt does not list (it is not in citar[all]'s "
                      "closure), e.g. PyInstaller.utils.hooks: exclude it in citar.spec", problems)
        self.assertIn("3 file(s) of setuptools, which PYTHON-LICENSES.txt does not list (it is not in citar[all]'s "
                      "closure), e.g. _distutils_hack, setuptools, setuptools\\_vendor\\packaging-24.2.dist-info\\METADATA:"
                      " exclude it in citar.spec", problems)
        self.assertIn(f"2 binary(s) from {self.tmp / 'jdk' / 'bin'}, outside the venv, the Python the bundle embeds and "
                      f"Windows (a folder on the build machine's PATH?), e.g. libssl-3.dll, msvcp140.dll", problems)

    def test_a_built_bundle_is_audited_from_its_files_and_its_archives(self):
        site, _, owned = self.sources()
        bundle = self.tmp / "dist" / "CITAR"
        for rel in ("citar.exe", "_internal/citar/_engine.pyd", "_internal/python312.dll", "_internal/base_library.zip"):
            (bundle / rel).parent.mkdir(parents=True, exist_ok=True)
            (bundle / rel).write_bytes(b"x")
        modules = ["citar", "argon2", "asyncio", "pyimod01_archive", "pyi_rth_inspect", "PYZ.pyz"]
        rules = {"owned": owned, "shipped": {"citar", "argon2-cffi"}, "site_packages": [site]}
        problems, counts = SR.audit(bundle, modules, **rules)
        self.assertEqual(problems, [])
        self.assertEqual(counts, {"citar": 2, "argon2-cffi": 1})
        # what the first bundle carried: setuptools' modules and vendored metadata, and the UCRT
        vendored = bundle / "_internal" / "setuptools" / "_vendor" / "packaging-24.2.dist-info" / "METADATA"
        vendored.parent.mkdir(parents=True)
        vendored.write_bytes(b"x")
        (bundle / "_internal" / "api-ms-win-crt-math-l1-1-0.dll").write_bytes(b"x")
        (bundle / "_internal" / "ucrtbase.dll").write_bytes(b"x")
        problems, _ = SR.audit(bundle, [*modules, "setuptools", "_distutils_hack"], **rules)
        self.assertEqual(problems, [
            "the bundle holds 2 Universal CRT DLL(s), e.g. _internal/api-ms-win-crt-math-l1-1-0.dll, "
            "_internal/ucrtbase.dll",
            "the bundle carries 3 file(s) or module(s) of setuptools, which PYTHON-LICENSES.txt does not list, e.g. "
            "setuptools/_vendor/packaging-24.2.dist-info/METADATA, _distutils_hack, setuptools"])

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
