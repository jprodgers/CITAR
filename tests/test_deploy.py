"""The server's release layout (design/phase5-release.md P5.13.1 and P5.13.2, package 5-10a): ``deploy/``'s unit,
backup and admin wrapper, the copies ``citar setup --server`` writes them from, and ``install.sh --server``'s plan.

- The package's copies (``citar/wizard/deploy/``) are deploy/'s files byte for byte, and the wizard writes them
  unchanged for the release layout's own paths: one definition of the unit and its sandbox.
- citar-admin passes citar.service's own user, configuration and sandbox to ``systemd-run``, in
  ``$CITAR_DEPLOY_SLICE`` when it is set and in no slice otherwise, and hands every argument over as it is.
- citar-backup snapshots the database consistently (with the server's write-ahead log held open), tars the whole
  state directory without the live database files, names the release that wrote it, and calls a backup of a
  corrupted database UNTRUSTWORTHY, pruning nothing (Linux, with the sqlite3 command and GNU tar: CI's Linux legs
  and the package's WSL gate).
- ``install.sh --server --plan`` names the layout and changes nothing (always with a temporary HOME, ``--prefix``,
  ``--bin-dir`` and ``--no-profile``, as every install.sh run in the tests).

The tests here need only the standard library, so the WSL gate runs this module with the system Python
(``python3 -m unittest discover -s tests -p test_deploy.py``).
"""
from __future__ import annotations

import gzip
import importlib.util
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
from pathlib import Path, PurePosixPath
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent
DEPLOY = ROOT / "deploy"
INSTALL_SH = ROOT / "install.sh"


def _load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


units = _load("citar_wizard_units_for_tests", ROOT / "citar" / "wizard" / "units.py")


def scratch(prefix: str) -> Path:
    """A temporary folder: under CARGO_TARGET_DIR when it is set (a package's own folder on the laptop), else the
    system's."""
    base = os.environ.get("CARGO_TARGET_DIR")
    if base:
        Path(base).mkdir(parents=True, exist_ok=True)
    return Path(tempfile.mkdtemp(prefix=prefix, dir=base or None))


def find_bash() -> str | None:
    """bash: on Windows Git Bash's, found beside git, never System32's bash.exe (WSL's, which would run elsewhere)."""
    if os.name != "nt":
        return shutil.which("bash")
    git = shutil.which("git")
    if not git:
        return None
    for base in Path(git).resolve().parents:
        candidate = base / "bin" / "bash.exe"
        if candidate.is_file() and "system32" not in str(candidate).lower():
            return str(candidate)
    return None


BASH = find_bash()


def msys(path: Path | str) -> str:
    """A path as bash reads it: on Windows Git Bash's /c/... form, elsewhere as it is."""
    text = str(path)
    if os.name != "nt":
        return text
    text = text.replace("\\", "/")
    if re.match(r"^[A-Za-z]:/", text):
        text = "/" + text[0].lower() + text[2:]
    return text


def bash_env(**extra: str) -> dict[str, str]:
    """The environment for a script under test: no CITAR_* variable from outside, no colour, and no shell options
    from the caller. An exported SHELLOPTS hands every bash below it the options the script sets: citar-admin.sh's
    `set -u` then reached the stand-in systemd-run, which stopped on its unset variable."""
    env = {k: v for k, v in os.environ.items()
           if not k.upper().startswith("CITAR_") and k.upper() not in ("SHELLOPTS", "BASHOPTS")}
    env["NO_COLOR"] = "1"
    env.update(extra)
    return env


def unit_text(name: str) -> str:
    return (DEPLOY / name).read_text(encoding="utf-8")


# ------------------------------------------------------------------------------------------------ one definition


class OneDefinition(unittest.TestCase):
    """deploy/'s files, the package's copies, and the wizard's rendering of them."""

    def test_the_package_carries_each_file_byte_for_byte(self):
        for name in units.FILES:
            with self.subTest(name):
                copy, original = (units.DEPLOY / name).read_bytes(), (DEPLOY / name).read_bytes()
                self.assertEqual(copy, original, f"citar/wizard/deploy/{name} differs from deploy/{name}: "
                                 f"copy deploy/{name} over it")
                self.assertNotIn(b"\r\n", original, f"deploy/{name} has CRLF line ends")
        self.assertEqual(sorted(p.name for p in units.DEPLOY.iterdir()), sorted(units.FILES))

    def test_the_layout_s_own_paths_render_deploy_s_files_unchanged(self):
        for name, text in units.rendered(units.Install()).items():
            with self.subTest(name):
                self.assertEqual(text, unit_text(name))

    def test_the_wizard_s_unit_and_deploy_s_hold_the_same_service_directives(self):
        """Gate 3's first half: the same [Service] directives, in the same order; another server's paths change
        only the directives that name them."""
        ours = units.directives(unit_text("citar.service"), "Service")
        self.assertEqual(units.directives(units.service_unit(units.Install()), "Service"), ours)
        other = units.Install(env_file=PurePosixPath("/srv/citar/env"), state_dir=PurePosixPath("/srv/citar/state"),
                              citar="/srv/venv/bin/citar", citar_admin="/srv/venv/bin/citar-admin",
                              documentation="https://games.example.org")
        theirs = units.directives(units.service_unit(other), "Service")
        self.assertEqual([k for k, _ in theirs], [k for k, _ in ours])
        changed = {k: v for (k, v), (_, w) in zip(theirs, ours) if v != w}
        self.assertEqual(changed, {
            "ExecStart": "/srv/venv/bin/citar serve",
            "WorkingDirectory": "/srv/citar/state",
            "Environment": "CITAR_STATE_DIR=/srv/citar/state",
            "EnvironmentFile": "/srv/citar/env",
            "ReadWritePaths": "/srv/citar/state",
        })
        self.assertIn("Documentation=https://games.example.org", units.service_unit(other))

    def test_the_unit_runs_the_layout_s_console_script_with_the_weights(self):
        service = dict(units.directives(unit_text("citar.service"), "Service"))
        self.assertEqual(service["ExecStart"], "/opt/citar/current/venv/bin/citar serve")
        self.assertEqual(service["WorkingDirectory"], "/var/lib/citar")
        self.assertEqual(service["ReadWritePaths"], "/var/lib/citar")
        self.assertEqual(service["EnvironmentFile"], "/etc/citar/citar.env")
        self.assertEqual(service["Environment"], "CITAR_STATE_DIR=/var/lib/citar")
        self.assertEqual((service["CPUWeight"], service["IOWeight"]), ("50", "50"))
        self.assertEqual((service["MemoryMax"], service["MemoryHigh"], service["OOMScoreAdjust"]),
                         ("400M", "320M", "500"))
        text = unit_text("citar.service")
        self.assertIn("stays under MemoryHigh\n# with 40 MB to spare", text, "the MemoryHigh rule beside the limits")
        self.assertNotIn("-m citar", service["ExecStart"])

    def test_the_other_scripts_take_this_server_s_paths(self):
        other = units.Install(env_file=PurePosixPath("/srv/citar/env"), state_dir=PurePosixPath("/srv/citar/state"),
                              citar_admin="/srv/venv/bin/citar-admin", current=PurePosixPath("/srv/citar/current"))
        admin, backup = units.admin_script(other), units.backup_script(other)
        for line in ('ENV_FILE="/srv/citar/env"', 'STATE_DIR="/srv/citar/state"',
                     'PROGRAM="/srv/venv/bin/citar-admin"'):
            self.assertIn("\n" + line + "\n", admin)
        for line in ('ENV_FILE="/srv/citar/env"', 'STATE_DIR="/srv/citar/state"', 'CURRENT="/srv/citar/current"'):
            self.assertIn("\n" + line + "\n", backup)
        self.assertIsNone(units.admin_script(units.Install(citar_admin=None)))

    def test_a_path_a_unit_or_a_script_would_misread_is_refused(self):
        for bad in ("/var/lib/my citar", "/var/lib/ci\"tar", "/var/lib/$HOME", "/var/lib/50%", "relative/dir",
                    "/var/lib/back\\slash", "/var/lib/it's"):
            with self.subTest(bad):
                self.assertIsNotNone(units.path_problem(bad))
                with self.assertRaises(ValueError):
                    units.service_unit(units.Install(state_dir=bad))
        for good in ("/var/lib/citar", "/srv/citar-2/state_1.0", "/opt/c+t@x"):
            self.assertIsNone(units.path_problem(good), good)

    def test_a_directive_deploy_s_file_lost_is_an_error_not_a_unit_without_it(self):
        text = unit_text("citar.service")
        with self.assertRaisesRegex(ValueError, "no WorkingDirectory="):
            units.set_directives(text.replace("WorkingDirectory=", "#WorkingDirectory="), "Service",
                                 {"WorkingDirectory": "/x"})
        with self.assertRaisesRegex(ValueError, "twice"):
            units.set_directives(text + "\n[Service]\nWorkingDirectory=/y\n", "Service", {"WorkingDirectory": "/x"})
        with self.assertRaisesRegex(ValueError, 'one ENV_FILE='):
            units.set_variables("x\n", {"ENV_FILE": "/x"})

    def test_a_copy_in_the_release_layout_names_current_not_its_release(self):
        tmp = scratch("citar-layout-")
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        venv = tmp / "releases" / "0.1.6" / "venv"
        (venv / "bin").mkdir(parents=True)
        try:
            os.symlink(Path("releases") / "0.1.6", tmp / "current", target_is_directory=True)
        except OSError as exc:
            self.skipTest(f"no symbolic links here ({exc})")
        with mock.patch.object(sys, "prefix", str(venv)):
            found = units.this_install(units.ENV_FILE, units.STATE_DIR, "https://x.example")
        self.assertEqual(found.citar, str(tmp / "current" / "venv" / "bin" / "citar"))
        self.assertEqual(found.citar_admin, str(tmp / "current" / "venv" / "bin" / "citar-admin"))
        self.assertEqual(found.current, tmp / "current")
        # a venv elsewhere: its own console scripts (here none, so the interpreter with -P)
        other = tmp / "elsewhere"
        (other / "bin").mkdir(parents=True)
        with mock.patch.object(sys, "prefix", str(other)), mock.patch.object(shutil, "which", return_value=None):
            found = units.this_install(units.ENV_FILE, units.STATE_DIR, "https://x.example")
        self.assertEqual(found.citar, f"{sys.executable} -P -m citar")
        self.assertIsNone(found.citar_admin)


    def test_env_example_holds_no_comment_after_a_value(self):
        """systemd reads everything after the = as the value (inline comments are not its), so a copy of env.example
        loaded by the unit's EnvironmentFile= must have none: `CITAR_SESSION_DAYS=30   # note` stops the server."""
        for number, line in enumerate((ROOT / "env.example").read_text(encoding="utf-8").splitlines(), 1):
            if re.match(r"^[A-Z][A-Z0-9_]*=", line):
                value = line.split("=", 1)[1]
                with self.subTest(line=number):
                    self.assertNotIn("#", value, f"env.example:{number}: {line}")
                    self.assertEqual(value, value.strip(), f"env.example:{number}: whitespace around the value")


# ------------------------------------------------------------------------------------------------ the wizard


class WizardInstall(unittest.TestCase):
    """``citar setup --server``'s systemd step, with every system path a temporary one and no command run."""

    def setUp(self):
        from citar.wizard import server

        self.server = server
        self.tmp = scratch("citar-wizard-units-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.targets = {name: self.tmp / "root" / str(path).lstrip("/") for name, path in units.TARGETS.items()}

    def install(self, citar_admin: str | None = "/opt/citar/current/venv/bin/citar-admin"):
        plan = self.server.ServerPlan(domain="citar.example.com", public_origin="https://citar.example.com",
                                      state_dir=self.tmp / "state", env_path=Path("/etc/citar/citar.env"))
        found = units.Install(citar_admin=citar_admin)
        runs = []
        with mock.patch.object(self.server.units, "TARGETS", self.targets), \
                mock.patch.object(self.server.units, "this_install", return_value=found), \
                mock.patch.object(self.server, "_user_exists", return_value=True), \
                mock.patch.object(self.server.shutil, "chown"), \
                mock.patch.object(self.server.subprocess, "run", side_effect=lambda argv, **kw: runs.append(argv)), \
                mock.patch.object(self.server.prompts, "note"):
            self.server._install_systemd(plan)
        return plan, runs

    def test_it_writes_deploy_s_files_and_enables_the_service_and_the_backup(self):
        plan, runs = self.install()
        for name, target in self.targets.items():
            with self.subTest(name):
                self.assertEqual(target.read_text(encoding="utf-8"), unit_text(name))
                if os.name != "nt":
                    self.assertEqual(target.stat().st_mode & 0o777, 0o755 if name.endswith(".sh") else 0o644)
        self.assertEqual(runs, [["systemctl", "daemon-reload"], ["systemctl", "enable", "--now", "citar"],
                                ["systemctl", "enable", "--now", "citar-backup.timer"]])
        self.assertTrue(plan.state_dir.is_dir())
        if os.name != "nt":
            self.assertEqual(plan.state_dir.stat().st_mode & 0o777, 0o750)
        self.assertEqual(sorted(p.name for p in self.targets["citar-admin.sh"].parent.iterdir()),
                         ["citar-admin", "citar-backup"], "no staged file left")

    def test_a_copy_without_citar_admin_writes_no_wrapper(self):
        self.install(citar_admin=None)
        self.assertFalse(self.targets["citar-admin.sh"].exists())
        self.assertTrue(self.targets["citar.service"].exists())

    @unittest.skipIf(os.name == "nt", "symbolic links")
    def test_a_link_where_citar_admin_goes_is_replaced_not_written_through(self):
        """install.sh linked citar-admin to the venv's own command before 0.1.6: writing through the link would
        overwrite that console script."""
        venv_command = self.tmp / "venv-citar-admin"
        venv_command.write_text("#!/bin/sh\n# the venv's own\n", encoding="utf-8")
        self.targets["citar-admin.sh"].parent.mkdir(parents=True)
        self.targets["citar-admin.sh"].symlink_to(venv_command)
        self.install()
        self.assertFalse(self.targets["citar-admin.sh"].is_symlink())
        self.assertEqual(venv_command.read_text(encoding="utf-8"), "#!/bin/sh\n# the venv's own\n")

    def test_what_root_made_in_the_state_directory_is_given_to_the_service(self):
        """Only what the in-process admin creation made, and the database's files: a root file that was there before
        stays root's, and so does the environment file, whatever folder it is in."""
        state = self.tmp / "given"
        (state / "saves").mkdir(parents=True)
        (state / "citar.db").write_bytes(b"")            # there before, root's from an earlier run: the service's
        (state / "operator-notes.txt").write_bytes(b"")  # there before, root's: left alone
        env = state / "citar.env"
        env.write_bytes(b"")
        before = self.server._paths_under(state)
        (state / "saves" / "lab").mkdir()
        (state / "saves" / "lab" / "run.json").write_bytes(b"{}")
        (state / "citar.db-wal").write_bytes(b"")
        mine = state / "saves" / "mine.json"
        mine.write_bytes(b"{}")
        real = os.lstat

        def lstat(path, *args, **kwargs):
            info = list(real(path, *args, **kwargs))
            info[4] = 1000 if Path(path) == mine else 0
            return os.stat_result(info)

        given = []
        with mock.patch.object(self.server.os, "lstat", side_effect=lstat), \
                mock.patch.object(self.server.shutil, "chown", side_effect=lambda p, u, g: given.append((p, u, g))):
            self.server._give_to_service_user(state, before, env)
        self.assertEqual(sorted(Path(p).name for p, _, _ in given), ["citar.db", "citar.db-wal", "lab", "run.json"])
        self.assertEqual({(u, g) for _, u, g in given}, {("citar", "citar")})

    def test_the_admin_command_is_the_wrapper_once_it_is_installed(self):
        plan = self.server.ServerPlan(install_systemd=True)
        with mock.patch.object(self.server.units, "TARGETS", self.targets):
            self.assertEqual(self.server._admin_command(plan), "citar admin")
            self.targets["citar-admin.sh"].parent.mkdir(parents=True)
            self.targets["citar-admin.sh"].write_text("#!/bin/sh\n", encoding="utf-8")
            self.assertEqual(self.server._admin_command(plan), "sudo citar-admin")
            self.assertEqual(self.server._admin_command(self.server.ServerPlan(install_systemd=False)), "citar admin")

    def test_a_failed_admin_creation_names_the_wrapper(self):
        self.targets["citar-admin.sh"].parent.mkdir(parents=True)
        self.targets["citar-admin.sh"].write_text("#!/bin/sh\n", encoding="utf-8")
        plan = self.server.ServerPlan(state_dir=self.tmp / "state", install_systemd=True,
                                      admin={"handle": "op", "email": "op@example.com", "password": ""})
        notes = []
        with mock.patch.object(self.server.units, "TARGETS", self.targets), \
                mock.patch.object(self.server, "_bootstrap_admin", side_effect=RuntimeError("no database here")), \
                mock.patch.object(self.server.prompts, "note", side_effect=notes.append), \
                mock.patch.dict(os.environ, {"CITAR_STATE_DIR": str(self.tmp / "state"),
                                             "CITAR_DATA_DIR": str(self.tmp / "state")}):
            self.server._create_admin(plan)
        self.assertIn("  sudo citar-admin create-admin --handle op --email op@example.com", notes)
        self.assertTrue(any(note.startswith("Could not create the account") for note in notes), notes)

    def test_refresh_units_writes_the_installed_unit_s_paths_from_this_release(self):
        other = units.Install(env_file=PurePosixPath("/srv/citar/env"), state_dir=PurePosixPath("/srv/citar/state"),
                              documentation="https://games.example.org")
        unit = self.targets["citar.service"]
        unit.parent.mkdir(parents=True)
        unit.write_text(units.service_unit(other).replace("CPUWeight=50", "CPUWeight=20"), encoding="utf-8")
        seen = []

        def this_install(env_file, state_dir, documentation):
            seen.append((PurePosixPath(Path(env_file).as_posix()), PurePosixPath(Path(state_dir).as_posix()),
                         documentation))
            return units.Install(env_file=seen[-1][0], state_dir=seen[-1][1], documentation=documentation)

        notes = []
        with mock.patch.object(self.server.units, "TARGETS", self.targets), \
                mock.patch.object(self.server.units, "this_install", side_effect=this_install), \
                mock.patch.object(self.server, "_is_root", return_value=True), \
                mock.patch.object(self.server.subprocess, "run") as run, \
                mock.patch.object(self.server.prompts, "note", side_effect=notes.append):
            self.assertEqual(self.server.refresh_units(), 0)
            self.assertEqual(seen, [(other.env_file, other.state_dir, "https://games.example.org")])
            for name, text in units.rendered(other).items():
                self.assertEqual(self.targets[name].read_text(encoding="utf-8"), text, name)
            self.assertIn("CPUWeight=50", unit.read_text(encoding="utf-8"), "this release's directives")
            run.assert_not_called()
            notes.clear()
            self.assertEqual(self.server.refresh_units(), 0)
            self.assertTrue(all(note.endswith(" is up to date") for note in notes), notes)
        with mock.patch.object(self.server.units, "TARGETS", self.targets), \
                mock.patch.object(self.server, "_is_root", return_value=False), \
                mock.patch.object(self.server.prompts, "note"):
            self.assertEqual(self.server.refresh_units(), 2, "not as root")
        unit.unlink()
        with mock.patch.object(self.server.units, "TARGETS", self.targets), \
                mock.patch.object(self.server, "_is_root", return_value=True), \
                mock.patch.object(self.server.prompts, "note"):
            self.assertEqual(self.server.refresh_units(), 2, "no unit installed")

    @unittest.skipIf(os.name == "nt", "a server's paths are POSIX ones")
    def test_a_state_directory_that_is_not_citar_s_alone_is_refused(self):
        problem = self.server.state_dir_problem
        self.assertIsNone(problem(self.tmp / "new" / "state"), "a new folder")
        (self.tmp / "empty" / "lost+found").mkdir(parents=True)
        self.assertIsNone(problem(self.tmp / "empty"), "an empty folder (a mount's lost+found aside)")
        (self.tmp / "ours" / "saves").mkdir(parents=True)
        (self.tmp / "ours" / "notes.txt").write_text("", encoding="utf-8")
        self.assertIsNone(problem(self.tmp / "ours"), "CITAR's state already")
        (self.tmp / "shared" / "www").mkdir(parents=True)
        self.assertIn("not CITAR's", problem(self.tmp / "shared"))
        (self.tmp / "file").write_text("", encoding="utf-8")
        self.assertIn("is not a folder", problem(self.tmp / "file"))
        layout = self.tmp / "opt" / "citar"
        with mock.patch.object(self.server, "_install_roots", return_value=[layout]):
            for overlapping in (layout, layout / "state", self.tmp / "opt"):
                with self.subTest(overlapping):
                    self.assertIn("overlaps CITAR's own install", problem(overlapping))
        self.assertIn("a space", problem("/var/lib/my citar"))
        roots = [str(root) for root in self.server._install_roots()]
        self.assertIn(str(Path(sys.prefix).resolve()), roots)
        self.assertIn(str(Path("/opt/citar")), roots)

    @unittest.skipIf(os.name == "nt", "a server's paths are POSIX ones")
    def test_an_unattended_run_with_a_shared_state_directory_changes_nothing(self):
        (self.tmp / "srv" / "www").mkdir(parents=True)
        args = mock.Mock(refresh_units=False, domain="citar.example.com", host=None, port=None, proxy_hops=None,
                         state_dir=str(self.tmp / "srv"), env_path=str(self.tmp / "citar.env"))
        notes = []
        with mock.patch.object(self.server, "_is_root", return_value=True), \
                mock.patch.object(self.server.prompts, "NON_INTERACTIVE", True), \
                mock.patch.object(self.server.prompts, "note", side_effect=notes.append), \
                mock.patch.object(self.server.prompts, "heading"), mock.patch.object(self.server.prompts, "say"), \
                mock.patch.object(self.server, "_write_env") as write:
            self.assertEqual(self.server.run(args), 1)
        write.assert_not_called()
        self.assertFalse((self.tmp / "citar.env").exists())
        self.assertTrue(any("not CITAR's" in note for note in notes), notes)


# ------------------------------------------------------------------------------------------------ citar-admin


def admin_properties() -> list[str]:
    """The ``-p "Key=Value"`` properties citar-admin.sh passes, as written there."""
    text = (DEPLOY / "citar-admin.sh").read_text(encoding="utf-8")
    block = text[text.index("properties=(\n"):]
    block = block[:block.index("\n)\n")]
    return re.findall(r'^\s+-p "([^"]+)"$', block, re.M)


class AdminSandbox(unittest.TestCase):
    """citar-admin's systemd-run properties are citar.service's: who it runs as, its configuration, and every
    directive of its file-system and privilege sandbox; not its limits, restart or logging."""

    def test_the_properties_are_the_unit_s(self):
        text = unit_text("citar.service")
        service = units.directives(text, "Service")
        sandbox_text = text[text.index("# --- filesystem ---"):text.index("# --- logging ---")]
        sandbox = units.directives("[Service]\n" + sandbox_text, "Service")
        wanted = [(k, v) for k, v in service if k in ("User", "Group", "WorkingDirectory", "Environment",
                                                       "EnvironmentFile")] + sandbox
        layout = {"$ROOT$STATE_DIR": "/var/lib/citar", "$ROOT$ENV_FILE": "/etc/citar/citar.env",
                  "$SERVICE_USER": "citar"}
        got = []
        for prop in admin_properties():
            for name, value in layout.items():
                prop = prop.replace(name, value)
            got.append(tuple(prop.split("=", 1)))
        self.assertEqual(sorted(got), sorted(wanted))
        self.assertEqual(len(got), len(set(got)))
        for limit in ("MemoryMax", "MemoryHigh", "CPUWeight", "Restart", "ExecStart"):
            self.assertNotIn(limit, dict(got))


@unittest.skipUnless(BASH, "no bash here (on Windows, Git Bash's)")
class AdminWrapper(unittest.TestCase):
    """citar-admin.sh in bash: its --plan, and a run under CITAR_DEPLOY_ROOT against a stand-in systemd-run."""

    def setUp(self):
        self.tmp = scratch("citar-admin-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.stub = self.tmp / "stub-bin"
        self.stub.mkdir()
        (self.stub / "systemd-run").write_text(
            '#!/bin/sh\n'
            'if [ "$1" = "--help" ]; then\n'
            '  if [ -n "$STUB_OLD_SYSTEMD" ]; then echo "  -P --pipe"; else echo "  --expand-environment=BOOL"; fi\n'
            '  exit 0\n'
            'fi\n'
            'for a in "$@"; do printf "%s\\n" "$a"; done > "$STUB_LOG"\n'
            'exit 0\n', encoding="ascii", newline="\n")
        (self.stub / "systemd-run").chmod(0o755)

    def admin(self, *args: str, slice_: str | None = None, root: Path | None = None, stub: bool = False,
              old: bool = False) -> tuple[int, str]:
        extra = {}
        if slice_ is not None:
            extra["CITAR_DEPLOY_SLICE"] = slice_
        if root is not None:
            extra["CITAR_DEPLOY_ROOT"] = msys(root)
        env = bash_env(**extra)
        if stub:
            env["PATH"] = str(self.stub) + os.pathsep + env.get("PATH", "")
            env["STUB_LOG"] = msys(self.tmp / "argv.txt")
            if old:
                env["STUB_OLD_SYSTEMD"] = "1"
        done = subprocess.run([BASH, msys(DEPLOY / "citar-admin.sh"), *args], env=env, capture_output=True,
                              text=True, encoding="utf-8", errors="replace", timeout=60, stdin=subprocess.DEVNULL)
        return done.returncode, done.stdout + done.stderr

    def test_the_plan_names_the_slice_only_when_it_is_set(self):
        """Gate 3's last part."""
        code, out = self.admin("--plan", "list-users", slice_="citar-staging.slice")
        self.assertEqual(code, 0, out)
        self.assertIn("--slice=citar-staging.slice", out)
        code, plain = self.admin("--plan", "list-users")
        self.assertEqual(code, 0, plain)
        self.assertNotIn("--slice", plain)
        code, empty = self.admin("--plan", "list-users", slice_="")
        self.assertNotIn("--slice", empty)
        for each in (out, plain):
            for word in ("systemd-run", "--quiet", "--wait", "--collect", "--pipe", "--service-type=exec",
                         "User=citar", "Group=citar", "WorkingDirectory=/var/lib/citar",
                         "EnvironmentFile=/etc/citar/citar.env", "ReadWritePaths=/var/lib/citar",
                         "ProtectSystem=strict", "-- /opt/citar/current/venv/bin/citar-admin list-users"):
                self.assertIn(word, each)
        self.assertEqual(out.replace(" --slice=citar-staging.slice", ""), plain)

    def test_a_run_hands_systemd_run_every_argument_as_it_is(self):
        root = self.tmp / "root"
        program = root / "opt" / "citar" / "current" / "venv" / "bin" / "citar-admin"
        program.parent.mkdir(parents=True)
        program.write_text("#!/bin/sh\n", encoding="ascii", newline="\n")
        program.chmod(0o755)
        (root / "etc" / "citar").mkdir(parents=True)
        (root / "etc" / "citar" / "citar.env").write_text("CITAR_MODE=server\n", encoding="ascii")
        args = ("set-status", "sam", "suspended", "--reason", "costs $5 and 100% of ${HOME}, it's \"spam\"")
        code, out = self.admin(*args, root=root, stub=True, slice_="citar-staging.slice")
        self.assertEqual(code, 0, out)
        argv = (self.tmp / "argv.txt").read_text(encoding="utf-8").split("\n")[:-1]
        self.assertEqual(argv[argv.index("--") + 1:], [msys(program), *args])
        self.assertIn("--expand-environment=no", argv)
        self.assertIn("--slice=citar-staging.slice", argv)
        self.assertIn("--pipe", argv)
        self.assertIn(f"EnvironmentFile={msys(root)}/etc/citar/citar.env", argv)
        code, out = self.admin("list-users", root=root, stub=True)
        self.assertEqual(code, 0, out)
        self.assertNotIn("--slice=citar-staging.slice",
                         (self.tmp / "argv.txt").read_text(encoding="utf-8"))

    def test_an_older_systemd_refuses_a_dollar_rather_than_expanding_it(self):
        root = self.tmp / "root"
        program = root / "opt" / "citar" / "current" / "venv" / "bin" / "citar-admin"
        program.parent.mkdir(parents=True)
        program.write_text("#!/bin/sh\n", encoding="ascii", newline="\n")
        program.chmod(0o755)
        (root / "etc" / "citar").mkdir(parents=True)
        (root / "etc" / "citar" / "citar.env").write_text("", encoding="ascii")
        code, out = self.admin("set-status", "sam", "suspended", "--reason", "$5", root=root, stub=True, old=True)
        self.assertEqual(code, 1, out)
        self.assertIn("older than 254", out)
        self.assertFalse((self.tmp / "argv.txt").exists())
        code, out = self.admin("list-users", root=root, stub=True, old=True)
        self.assertEqual(code, 0, out)
        self.assertNotIn("--expand-environment=no", (self.tmp / "argv.txt").read_text(encoding="utf-8"))

    def test_a_server_not_set_up_is_named(self):
        root = self.tmp / "empty-root"
        root.mkdir()
        code, out = self.admin("list-users", root=root, stub=True)
        self.assertEqual(code, 1, out)
        self.assertIn("/etc/citar/citar.env: is CITAR set up?", out)
        (root / "etc" / "citar").mkdir(parents=True)
        (root / "etc" / "citar" / "citar.env").write_text("", encoding="ascii")
        code, out = self.admin("list-users", root=root, stub=True)
        self.assertEqual(code, 1, out)
        self.assertIn("citar-admin: is CITAR installed there?", out)
        self.assertFalse((self.tmp / "argv.txt").exists())


# ------------------------------------------------------------------------------------------------ citar-backup


def _gnu_tar() -> bool:
    try:
        return "GNU tar" in subprocess.run(["tar", "--version"], capture_output=True, text=True, timeout=30).stdout
    except (OSError, subprocess.SubprocessError):
        return False


BACKUP_TOOLS = sys.platform.startswith("linux") and bool(BASH) and shutil.which("sqlite3") is not None and _gnu_tar()

#: A file planted in each folder the server writes under its state directory (citar/paths.py), and one beside the
#: database: the tarball must hold every one.
PLANTED = ("saves/game.citar", "saves/lab/run.json", "saves/maps/m.json", "saves/scenarios/s.citarscn",
           "saves/probes/p.json", "saves/reports/r.md", "saves/usage/ledger.jsonl", "saves/balance/b.json",
           "config/worker.json", "benchmarks/runs/x.json", "secret_key", "saves/old/citar.db")


@unittest.skipUnless(BACKUP_TOOLS, "Linux with bash, the sqlite3 command and GNU tar")
class Backup(unittest.TestCase):
    """citar-backup.sh under CITAR_DEPLOY_ROOT against a temporary state directory (gate 2)."""

    def setUp(self):
        self.root = scratch("citar-backup-")
        self.addCleanup(shutil.rmtree, self.root, ignore_errors=True)
        self.state = self.root / "var" / "lib" / "citar"
        for rel in PLANTED:
            path = self.state / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(f"planted: {rel}\n", encoding="utf-8")
        (self.root / "etc" / "citar").mkdir(parents=True)
        self.env = self.root / "etc" / "citar" / "citar.env"
        self.env.write_text("CITAR_MODE=server\nCITAR_SECRET_KEY=not-a-real-one\nCITAR_STATE_DIR=/var/lib/citar\n",
                            encoding="utf-8")
        (self.root / "opt" / "citar" / "releases" / "0.1.6").mkdir(parents=True)
        os.symlink("releases/0.1.6", self.root / "opt" / "citar" / "current")
        self.backups = self.root / "var" / "backups" / "citar"

    def database(self, rows: int = 50, hold: bool = False) -> sqlite3.Connection | None:
        """The server's database in WAL mode, migrated to a revision, with ``rows`` rows. With ``hold`` its
        connection stays open with the last rows in the write-ahead log only, as a running server leaves it."""
        db = sqlite3.connect(self.state / "citar.db")
        db.execute("PRAGMA journal_mode=WAL")
        db.execute("PRAGMA wal_autocheckpoint=0")
        db.execute("CREATE TABLE alembic_version (version_num VARCHAR(32) NOT NULL PRIMARY KEY)")
        db.execute("INSERT INTO alembic_version VALUES ('5f1e0c2a9b3d')")
        db.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, handle TEXT)")
        db.executemany("INSERT INTO users (handle) VALUES (?)", [(f"user{i:04d}" + "x" * 200,) for i in range(rows)])
        db.commit()
        if hold:
            db.execute("INSERT INTO users (handle) VALUES ('only-in-the-wal')")
            db.commit()
            return db
        db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        db.close()
        return None

    def run_backup(self, **env: str) -> tuple[int, str]:
        done = subprocess.run([BASH, str(DEPLOY / "citar-backup.sh")], env=bash_env(CITAR_DEPLOY_ROOT=str(self.root),
                              **env), capture_output=True, text=True, timeout=120)
        return done.returncode, done.stdout + done.stderr

    def made(self) -> list[Path]:
        return sorted(p for p in self.backups.iterdir() if p.is_dir())

    def test_a_backup_holds_the_whole_state_a_checked_snapshot_and_the_release(self):
        held = self.database(hold=True)
        self.addCleanup(held.close)
        self.assertTrue((self.state / "citar.db-wal").stat().st_size > 0)
        code, out = self.run_backup()
        self.assertEqual(code, 0, out)
        [backup] = self.made()
        self.assertEqual(sorted(p.name for p in backup.iterdir()), ["RELEASE", "citar.db", "citar.env",
                                                                     "state.tar.gz"])
        with tarfile.open(backup / "state.tar.gz") as tar:
            members = {m.name for m in tar.getmembers()}
            for rel in PLANTED:
                self.assertIn(f"./{rel}", members)
                self.assertEqual(tar.extractfile(f"./{rel}").read().decode(), f"planted: {rel}\n")
        for live in ("citar.db", "citar.db-wal", "citar.db-shm", "citar.db-journal"):
            self.assertNotIn(f"./{live}", members)
        self.assertIn("./saves/old/citar.db", members, "only the live database is left out")
        snapshot = sqlite3.connect(backup / "citar.db")
        self.addCleanup(snapshot.close)
        self.assertEqual(snapshot.execute("PRAGMA integrity_check").fetchone(), ("ok",))
        self.assertEqual(snapshot.execute("SELECT count(*) FROM users WHERE handle = 'only-in-the-wal'").fetchone(),
                         (1,), "the snapshot holds what was only in the write-ahead log")
        release = dict(line.split("=", 1) for line in (backup / "RELEASE").read_text().splitlines())
        self.assertEqual(release["release"], "releases/0.1.6")
        self.assertEqual(release["version"], "0.1.6")
        self.assertEqual(release["revision"], "5f1e0c2a9b3d")
        self.assertEqual(release["database"], "/var/lib/citar/citar.db")
        self.assertEqual(release["state"], "/var/lib/citar")
        self.assertEqual((backup / "citar.env").read_bytes(), self.env.read_bytes())
        self.assertEqual((backup / "citar.env").stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.backups.stat().st_mode & 0o777, 0o700)
        self.assertIn("backup complete:", out)

    def test_a_corrupted_database_makes_the_backup_untrustworthy_and_prunes_nothing(self):
        self.database(rows=400)
        size = (self.state / "citar.db").stat().st_size
        self.assertGreater(size, 8 * 4096)
        with open(self.state / "citar.db", "r+b") as f:
            f.seek(3 * 4096)
            f.write(os.urandom(4 * 4096))
        old = self.backups / "20000101-000000"
        old.mkdir(parents=True)
        month_ago = time.time() - 30 * 86400
        os.utime(old, (month_ago, month_ago))
        code, out = self.run_backup()
        self.assertEqual(code, 2, out)
        self.assertIn("UNTRUSTWORTHY", out)
        new = [p for p in self.made() if p != old]
        self.assertEqual(len(new), 1)
        reasons = (new[0] / "UNTRUSTWORTHY").read_text()
        self.assertTrue("integrity" in reasons or ".backup failed" in reasons, reasons)
        self.assertTrue(old.exists(), "an untrustworthy backup pruned an older one")
        # a good one prunes it
        (self.state / "citar.db").unlink()
        self.database()
        code, out = self.run_backup()
        self.assertEqual(code, 0, out)
        self.assertFalse(old.exists())

    def test_backups_in_the_same_second_are_two(self):
        self.database()
        for _ in range(3):
            code, out = self.run_backup()
            self.assertEqual(code, 0, out)
        self.assertEqual(len(self.made()), 3)

    def test_the_env_file_s_database_and_state_are_followed(self):
        other = self.root / "srv" / "state"
        shutil.move(str(self.state), str(other))
        self.state = other
        self.env.write_text("CITAR_STATE_DIR=\"/srv/state/\"\nCITAR_DB_URL=sqlite:////srv/state/data/main.db\n",
                            encoding="utf-8")
        (other / "data").mkdir()
        db = sqlite3.connect(other / "data" / "main.db")
        db.execute("CREATE TABLE t (x)")
        db.commit()
        db.close()
        code, out = self.run_backup()
        self.assertEqual(code, 0, out)
        [backup] = self.made()
        with tarfile.open(backup / "state.tar.gz") as tar:
            members = {m.name for m in tar.getmembers()}
        self.assertNotIn("./data/main.db", members)
        self.assertIn("./saves/game.citar", members)
        release = (backup / "RELEASE").read_text()
        self.assertIn("database=/srv/state/data/main.db\n", release)
        self.assertIn("state=/srv/state\n", release)

    def test_a_database_that_is_not_sqlite_is_named_and_the_rest_backed_up(self):
        self.env.write_text("CITAR_DB_URL=postgresql+psycopg://citar:pw@127.0.0.1:5432/citar\n", encoding="utf-8")
        code, out = self.run_backup()
        self.assertEqual(code, 0, out)
        [backup] = self.made()
        self.assertIn("database=external (postgresql+psycopg)", (backup / "RELEASE").read_text())
        self.assertNotIn("pw@", (backup / "RELEASE").read_text())
        self.assertFalse((backup / "citar.db").exists())
        with gzip.open(backup / "state.tar.gz") as raw:
            self.assertGreater(len(raw.read()), 0)

    def test_the_env_file_is_read_as_systemd_reads_it(self):
        """systemd 255 (probed in WSL): the last assignment wins, whitespace around the key and the = is dropped, and
        `export KEY=` and comment lines are not assignments. A first-match reader archived the wrong folder."""
        right = self.root / "srv" / "right"
        shutil.move(str(self.state), str(right))
        self.env.write_text("CITAR_STATE_DIR=/var/lib/citar\n"
                            "export CITAR_DB_URL=sqlite:////nowhere/x.db\n"
                            "# CITAR_STATE_DIR=/commented\n"
                            ";CITAR_STATE_DIR=/also-commented\n"
                            "  CITAR_STATE_DIR = /srv/right  \n"
                            "\tCITAR_DATA_DIR\t=\t'/srv/right'\n", encoding="utf-8")
        db = sqlite3.connect(right / "citar.db")
        db.execute("CREATE TABLE alembic_version (version_num VARCHAR(32))")
        db.execute("INSERT INTO alembic_version VALUES ('abc123')")
        db.commit()
        db.close()
        code, out = self.run_backup()
        self.assertEqual(code, 0, out)
        [backup] = self.made()
        release = dict(line.split("=", 1) for line in (backup / "RELEASE").read_text().splitlines())
        self.assertEqual((release["state"], release["data"], release["database"], release["revision"]),
                         ("/srv/right", "/srv/right", "/srv/right/citar.db", "abc123"))
        with tarfile.open(backup / "state.tar.gz") as tar:
            self.assertIn("./saves/game.citar", {m.name for m in tar.getmembers()})

    def test_directories_moved_outside_the_state_are_archived_too(self):
        """CITAR_SAVE_DIR, CITAR_CONFIG_DIR and CITAR_DATA_DIR outside the state directory (citar/paths.py): each its
        own tarball, the live database left out of the one that holds it; benchmarks follow moved saves."""
        outside = self.root / "srv"
        (outside / "games" / "lab").mkdir(parents=True)
        (outside / "games" / "lab" / "run.json").write_text("moved saves\n", encoding="utf-8")
        (outside / "benchmarks").mkdir()
        (outside / "benchmarks" / "suite.json").write_text("beside them\n", encoding="utf-8")
        (outside / "private").mkdir()
        (outside / "private" / "secret_key").write_text("key\n", encoding="utf-8")
        self.env.write_text("CITAR_STATE_DIR=/var/lib/citar\nCITAR_SAVE_DIR=/srv/games/\n"
                            "CITAR_DATA_DIR=/srv/private\nCITAR_CONFIG_DIR=config-elsewhere\n", encoding="utf-8")
        db = sqlite3.connect(outside / "private" / "citar.db")
        db.execute("CREATE TABLE t (x)")
        db.commit()
        db.close()
        code, out = self.run_backup()
        self.assertEqual(code, 0, out)
        [backup] = self.made()
        self.assertEqual(sorted(p.name for p in backup.iterdir()),
                         ["RELEASE", "benchmarks.tar.gz", "citar.db", "citar.env", "data.tar.gz", "saves.tar.gz",
                          "state.tar.gz"])
        names = {}
        for archive in ("saves", "benchmarks", "data"):
            with tarfile.open(backup / f"{archive}.tar.gz") as tar:
                names[archive] = {m.name for m in tar.getmembers()}
        self.assertIn("./lab/run.json", names["saves"])
        self.assertIn("./suite.json", names["benchmarks"])
        self.assertIn("./secret_key", names["data"])
        self.assertNotIn("./citar.db", names["data"])
        release = dict(line.split("=", 1) for line in (backup / "RELEASE").read_text().splitlines())
        self.assertEqual(release["saves"], "/srv/games")
        self.assertEqual(release["benchmarks"], "/srv/benchmarks")
        self.assertEqual(release["config"], "/var/lib/citar/config-elsewhere", "relative to the working directory")
        self.assertEqual(release["database"], "/srv/private/citar.db")
        self.assertEqual(release["archives"], "state.tar.gz data.tar.gz saves.tar.gz benchmarks.tar.gz",
                         "the configuration, inside the state directory, is in state.tar.gz")
        self.env.write_text("CITAR_STATE_DIR=/var/lib/citar\nCITAR_CONFIG_DIR=/srv/not-made-yet\n", encoding="utf-8")
        code, out = self.run_backup()
        self.assertEqual(code, 0, out)
        self.assertIn("no config directory at /srv/not-made-yet yet: nothing to archive there", out)

    def test_a_failed_offsite_copy_is_exit_3_after_a_whole_backup(self):
        stub = self.root / "stub-bin"
        stub.mkdir()
        (stub / "rsync").write_text("#!/bin/sh\necho 'rsync: connection refused' >&2\nexit 12\n", encoding="ascii")
        (stub / "rsync").chmod(0o755)
        self.database()
        code, out = self.run_backup(CITAR_BACKUP_RSYNC_TARGET="backup@offsite.invalid:/citar/",
                                    PATH=f"{stub}{os.pathsep}{os.environ.get('PATH', '')}")
        self.assertEqual(code, 3, out)
        [backup] = self.made()
        self.assertFalse((backup / "UNTRUSTWORTHY").exists())
        self.assertTrue((backup / "citar.db").is_file())
        lines = out.strip().splitlines()
        self.assertIn("OFFSITE COPY FAILED", lines[-1])
        self.assertNotIn("backup complete:", out)
        (stub / "rsync").write_text("#!/bin/sh\nexit 0\n", encoding="ascii")
        code, out = self.run_backup(CITAR_BACKUP_RSYNC_TARGET="backup@offsite.invalid:/citar/",
                                    PATH=f"{stub}{os.pathsep}{os.environ.get('PATH', '')}")
        self.assertEqual(code, 0, out)
        self.assertIn("backup complete:", out.strip().splitlines()[-1])

    def test_no_current_release_is_said_so(self):
        (self.root / "opt" / "citar" / "current").unlink()
        self.database()
        code, out = self.run_backup()
        self.assertEqual(code, 0, out)
        [backup] = self.made()
        self.assertIn("release=none (no /opt/citar/current)\nversion=unknown\n", (backup / "RELEASE").read_text())


# ------------------------------------------------------------------------------------------------ install.sh --plan


@unittest.skipUnless(BASH, "no bash here (on Windows, Git Bash's)")
class InstallPlan(unittest.TestCase):
    """``install.sh --server --plan``: the release layout named, nothing made."""

    LINUX = "system=Linux machine=x86_64 libc=glibc-2.39"

    def setUp(self):
        self.tmp = scratch("citar-install-plan-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.home, self.prefix, self.bin = self.tmp / "home", self.tmp / "prefix", self.tmp / "bin"
        self.home.mkdir()
        (self.tmp / "tmp").mkdir()

    def sh(self, *args: str, unit: str | None = None) -> tuple[int, str]:
        """install.sh with these flags. ``unit`` set, a stand-in ``systemctl`` first on PATH says the citar service is
        installed with that unit text (empty: that it is not)."""
        env = bash_env(HOME=msys(self.home), TMPDIR=msys(self.tmp / "tmp"), SHELL="/bin/bash",
                       XDG_CONFIG_HOME=msys(self.home / ".config"), XDG_DATA_HOME=msys(self.home / ".local" / "share"),
                       CITAR_INSTALL_TEST_FACTS=self.LINUX)
        if unit is not None:
            stub = self.tmp / "stub-bin"
            stub.mkdir(exist_ok=True)
            (self.tmp / "unit.txt").write_text(unit, encoding="utf-8", newline="\n")
            (stub / "systemctl").write_text(
                '#!/bin/sh\n[ "$1" = cat ] && [ -s "$STUB_UNIT" ] && exec cat "$STUB_UNIT"\nexit 1\n',
                encoding="ascii", newline="\n")
            (stub / "systemctl").chmod(0o755)
            env["PATH"] = f"{stub}{os.pathsep}{env.get('PATH', '')}"
            env["STUB_UNIT"] = msys(self.tmp / "unit.txt")
        argv = [BASH, msys(INSTALL_SH), *args, "--python", msys(sys.executable), "--prefix", msys(self.prefix),
                "--bin-dir", msys(self.bin), "--no-profile"]
        done = subprocess.run(argv, env=env, capture_output=True, text=True, encoding="utf-8", errors="replace",
                              timeout=120, stdin=subprocess.DEVNULL)
        return done.returncode, done.stdout + done.stderr

    def assertNothingMade(self):
        self.assertFalse(self.prefix.exists(), "the prefix was made")
        self.assertFalse(self.bin.exists(), "the bin folder was made")
        self.assertEqual(list(self.home.iterdir()), [], "HOME was written")
        self.assertEqual(list((self.tmp / "tmp").iterdir()), [], "a temporary folder was left")

    def test_the_server_plan_names_the_release_layout(self):
        """Gate 3's middle part."""
        code, out = self.sh("--server", "--plan", "--version", "0.1.6")
        self.assertEqual(code, 0, out)
        p, b = msys(self.prefix), msys(self.bin)
        for line in (f"release     {p}/releases/0.1.6/venv: a venv of its own, root's, read-only to the service",
                     f"{p}/releases/0.1.6/: the wheel, freeze.txt and SHA256SUMS beside it",
                     f"current     {p}/current -> releases/0.1.6, switched atomically",
                     f"commands    {b}/citar and {b}/citar-mcp -> {p}/current/venv/bin/",
                     "state       /var/lib/citar: citar:citar 0750, the service's state and working directory",
                     "config      /etc/citar/citar.env: root:citar 0640",
                     f"service     /etc/systemd/system/citar.service: ExecStart={p}/current/venv/bin/citar serve",
                     "admin       /usr/local/bin/citar-admin",
                     "backup      /usr/local/bin/citar-backup, with citar-backup.service and citar-backup.timer",
                     f"Then {p}/current/venv/bin/citar setup --server writes",
                     "wheel       citar[server]==0.1.6 from PyPI, a prebuilt wheel only"):
            self.assertIn(line, out)
        self.assertNothingMade()

    def test_the_version_comes_from_the_wheel_named(self):
        wheel = self.tmp / "citar-0.1.7rc1-cp311-abi3-manylinux_2_28_x86_64.whl"
        wheel.write_bytes(b"not read by a plan")
        code, out = self.sh("--server", "--plan", "--from", msys(wheel), "--no-setup")
        self.assertEqual(code, 0, out)
        self.assertIn("/releases/0.1.7rc1/venv", out)
        self.assertIn("Then nothing (--no-setup)", out)
        self.assertIn("wheel       " + msys(wheel), out)
        code, out = self.sh("--server", "--plan")
        self.assertEqual(code, 0, out)
        self.assertIn("/releases/<version>/venv", out)
        self.assertIn("the newest release on PyPI", out)
        self.assertNothingMade()

    def running(self, version: str = "0.1.6", complete: bool = True) -> list[str]:
        """A prefix whose current release is ``version``; returns its tree, to compare after a plan."""
        (self.prefix / "releases" / version / "venv" / "bin").mkdir(parents=True)
        if complete:
            (self.prefix / "releases" / version / ".complete").write_text("installed\n", encoding="utf-8")
        (self.prefix / ".citar-installer").write_text("installed by install.sh\n", encoding="utf-8")
        os.symlink(f"releases/{version}", self.prefix / "current")
        return self.tree()

    def tree(self) -> list[str]:
        return sorted(str(p) for p in self.prefix.rglob("*"))

    @unittest.skipIf(os.name == "nt", "symbolic links (Git Bash's ln -s copies)")
    def test_the_plan_of_a_newer_release_without_a_service_switches_alone(self):
        before = self.running()
        code, out = self.sh("--server", "--plan", "--version", "0.1.7", unit="")
        self.assertEqual(code, 0, out)
        self.assertIn(f"running     {self.prefix}/current -> releases/0.1.6, kept beside the new release", out)
        self.assertIn(f"current     {self.prefix}/current -> releases/0.1.7, switched atomically", out)
        self.assertIn("and nothing more: no citar service is installed here, so nothing is stopped or started.", out)
        self.assertNotIn("setup --server writes", out)
        self.assertEqual(self.tree(), before)

    @unittest.skipIf(os.name == "nt", "symbolic links (Git Bash's ln -s copies)")
    def test_the_plan_of_a_newer_release_with_the_service_is_the_stopped_switch(self):
        """Finding 1's flow, as the plan prints it: the run (InstallServer) takes these steps in this order."""
        before = self.running()
        code, out = self.sh("--server", "--plan", "--version", "0.1.7",
                            unit="[Service]\nEnvironmentFile=/etc/citar/citar.env\nWorkingDirectory=/var/lib/citar\n")
        self.assertEqual(code, 0, out)
        steps = ["The citar service is installed, so the switch is made with it stopped:",
                 "  1. systemctl stop citar",
                 "  2. systemctl start citar-backup: a new, trustworthy backup in /var/backups/citar",
                 f"  3. current     {self.prefix}/current -> releases/0.1.7, switched atomically",
                 f"  4. {self.prefix}/current/venv/bin/citar setup --server --refresh-units",
                 "  5. systemctl daemon-reload, then systemctl start citar",
                 "  6. /api/meta (CITAR_HOST and CITAR_PORT in /etc/citar/citar.env) answers as CITAR 0.1.7 within 60 s",
                 "  7. commands",
                 "If 0.1.7 does not answer: citar is stopped, the database is put back from step 2's backup"]
        at = [out.find(step) for step in steps]
        self.assertNotIn(-1, at, out)
        self.assertEqual(at, sorted(at), out)
        self.assertEqual(self.tree(), before)

    @unittest.skipIf(os.name == "nt", "symbolic links (Git Bash's ln -s copies)")
    def test_the_plan_of_an_older_release_is_the_run_s_refusal(self):
        before = self.running("0.1.7")
        code, out = self.sh("--server", "--plan", "--version", "0.1.5", unit="")
        self.assertEqual(code, 1, out)
        self.assertIn("CITAR 0.1.5 is older than the release that runs", out)
        self.assertNotIn("The plan", out)
        self.assertEqual(self.tree(), before)

    @unittest.skipIf(os.name == "nt", "symbolic links (Git Bash's ln -s copies)")
    def test_the_plan_of_the_running_release_keeps_it_and_runs_the_setup(self):
        before = self.running("0.1.6", complete=False)
        code, out = self.sh("--server", "--plan", "--version", "0.1.6", unit="")
        self.assertEqual(code, 0, out)
        self.assertIn("CITAR 0.1.6 is the release that runs, so the layout stays as it is:", out)
        self.assertIn("marked complete if its own citar --version says 0.1.6, else refused", out)
        self.assertIn(f"current     {self.prefix}/current -> releases/0.1.6, unchanged", out)
        self.assertIn(f"Then {self.prefix}/current/venv/bin/citar setup --server writes", out)
        self.assertNotIn("This script makes the release layout", out)
        self.assertEqual(self.tree(), before)

    def test_a_personal_plan_names_its_venv(self):
        code, out = self.sh("--plan", "--version", "0.1.6")
        self.assertEqual(code, 0, out)
        p = msys(self.prefix)
        self.assertIn(f"venv        {p}/venv", out)
        self.assertIn("citar[all]==0.1.6", out)
        self.assertIn("PATH        unchanged (--no-profile)", out)
        self.assertIn(f"then        {p}/venv/bin/citar setup --local", out)
        self.assertNothingMade()

    def test_plan_is_for_an_install(self):
        for extra, says in ((("--worker",), "--plan is for the game's install"),
                            (("--uninstall",), "--plan is for an install, not --uninstall")):
            with self.subTest(extra):
                code, out = self.sh("--plan", *extra)
                self.assertEqual(code, 1, out)
                self.assertIn(says, out)
                self.assertNothingMade()


# ------------------------------------------------------------------------------------------------ install.sh --server


#: The stand-in CITAR's console script: ``--version``; ``setup`` logged; ``serve`` migrates the stand-in database to
#: its own revision (refusing one a newer release migrated, as the server does, P5.0.9), then answers /api/meta as its
#: version on CITAR_PORT, or, built broken, exits after migrating (a release that crash-loops).
_FAKE_CLI = '''import http.server
import json
import os
import sqlite3
import sys

VERSION = {version!r}
BROKEN = {broken!r}


def log(line):
    with open(os.environ["FAKE_LOG"], "a", encoding="utf-8") as handle:
        handle.write(line + "\\n")


def key(version):
    return tuple(int(part) for part in version.split("."))


def main():
    args = sys.argv[1:]
    if "--version" in args:
        print("CITAR " + VERSION)
        return 0
    if args[:1] == ["setup"]:
        log("citar " + VERSION + ": " + " ".join(args))
        return 0
    if args[:1] != ["serve"]:
        return 0
    db = sqlite3.connect(os.environ["FAKE_DB"])
    db.execute("CREATE TABLE IF NOT EXISTS alembic_version (version_num VARCHAR(32) NOT NULL)")
    row = db.execute("SELECT version_num FROM alembic_version").fetchone()
    if row and key(row[0].split("-", 1)[1]) > key(VERSION):
        log("citar " + VERSION + ": refuses " + row[0])
        return 1
    db.execute("DELETE FROM alembic_version")
    db.execute("INSERT INTO alembic_version VALUES (?)", ("rev-" + VERSION,))
    db.commit()
    db.close()
    log("citar " + VERSION + ": migrated, " + ("crashing" if BROKEN else "serving"))
    if BROKEN:
        return 1

    class Meta(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            body = json.dumps({{"version": VERSION}}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):
            pass

    http.server.HTTPServer(("127.0.0.1", int(os.environ["CITAR_PORT"])), Meta).serve_forever()
'''

#: The stand-in systemctl: every call logged with where current points; `cat citar.service` the unit when one is
#: "installed"; citar-backup the real deploy/citar-backup.sh under CITAR_DEPLOY_ROOT (or a failure, asked for);
#: `start citar` the current release's `citar serve` in the background with the env file's settings, `stop` it.
_FAKE_SYSTEMCTL = r'''#!/bin/sh
echo "systemctl $* [current -> $(readlink "$STUB_PREFIX/current")]" >> "$FAKE_LOG"
case "$1" in
  cat) [ -f "$STUB_UNIT" ] && exec cat "$STUB_UNIT"; echo "No files found for $2." >&2; exit 1 ;;
  show) cat "$STUB_ROOT/backup-status" 2>/dev/null; exit 0 ;;
  stop)
    if [ -f "$STUB_ROOT/citar.pid" ]; then
      pid="$(cat "$STUB_ROOT/citar.pid")"
      kill "$pid" 2>/dev/null
      n=0
      while kill -0 "$pid" 2>/dev/null && [ "$n" -lt 50 ]; do sleep 0.1; n=$((n + 1)); done
      rm -f "$STUB_ROOT/citar.pid"
    fi
    exit 0 ;;
  start)
    case "$2" in
      citar-backup)
        if [ -n "${STUB_BACKUP_FAIL:-}" ]; then
          status=2
        else
          CITAR_DEPLOY_ROOT="$STUB_ROOT" bash "$STUB_BACKUP" >> "$STUB_ROOT/backup.log" 2>&1
          status=$?
        fi
        echo "$status" > "$STUB_ROOT/backup-status"
        if [ "$status" -eq 0 ]; then exit 0; fi
        exit 1 ;;
      citar)
        (
          set -a
          . "$STUB_ROOT/etc/citar/citar.env"
          set +a
          FAKE_DB="$STUB_ROOT/var/lib/citar/citar.db" exec "$STUB_PREFIX/current/venv/bin/citar" serve
        ) < /dev/null >> "$STUB_ROOT/serve.log" 2>&1 &
        echo $! > "$STUB_ROOT/citar.pid"
        exit 0 ;;
    esac ;;
esac
exit 0
'''


def fake_wheel(folder: Path, version: str, broken: bool = False) -> Path:
    """A stand-in CITAR wheel: pure Python, no dependencies, with the console scripts install.sh links and runs."""
    import base64
    import hashlib
    import zipfile

    info = f"citar-{version}.dist-info"
    files = {
        "citar/__init__.py": f"__version__ = {version!r}\n",
        "citar/cli.py": _FAKE_CLI.format(version=version, broken=broken),
        f"{info}/METADATA": f"Metadata-Version: 2.1\nName: citar\nVersion: {version}\nProvides-Extra: server\n",
        f"{info}/WHEEL": "Wheel-Version: 1.0\nGenerator: tests\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
        f"{info}/entry_points.txt": ("[console_scripts]\ncitar = citar.cli:main\ncitar-admin = citar.cli:main\n"
                                     "citar-mcp = citar.cli:main\n"),
    }
    record = []
    for path, text in files.items():
        digest = base64.urlsafe_b64encode(hashlib.sha256(text.encode()).digest()).rstrip(b"=").decode()
        record.append(f"{path},sha256={digest},{len(text.encode())}")
    files[f"{info}/RECORD"] = "\n".join(record + [f"{info}/RECORD,,"]) + "\n"
    target = folder / f"citar-{version}-py3-none-any.whl"
    with zipfile.ZipFile(target, "w") as wheel:
        for path, text in files.items():
            wheel.writestr(path, text)
    return target


def _can_make_venvs() -> bool:
    try:
        return subprocess.run([sys.executable, "-c", "import venv, ensurepip"], capture_output=True,
                              timeout=60).returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


@unittest.skipUnless(BACKUP_TOOLS and _can_make_venvs() and os.geteuid() != 0,
                     "Linux with bash, the sqlite3 command, GNU tar and a Python that makes venvs, not as root (as "
                     "root install.sh would not use the stand-in sudo, and its uninstall acts on /etc)")
class InstallServer(unittest.TestCase):
    """``install.sh --server`` for real, offline: stand-in wheels, a stand-in ``sudo`` (``exec "$@"``) and
    ``systemctl`` first on PATH, the real citar-backup under ``CITAR_DEPLOY_ROOT``, a temporary HOME, --prefix,
    --bin-dir and --no-profile. Nothing outside the temporary folder is touched."""

    @classmethod
    def setUpClass(cls):
        cls.wheels = scratch("citar-install-wheels-")
        for version in ("0.1.5", "0.1.6", "0.1.7", "0.1.9"):
            fake_wheel(cls.wheels, version)
        fake_wheel(cls.wheels, "0.1.8", broken=True)

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.wheels, ignore_errors=True)

    def setUp(self):
        self.tmp = scratch("citar-install-server-")
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.addCleanup(self.stop_server)
        # The prefix is where citar-backup, under CITAR_DEPLOY_ROOT, reads the release that runs (/opt/citar/current).
        self.root, self.stub, self.log = self.tmp / "root", self.tmp / "stub-bin", self.tmp / "calls.log"
        self.home, self.prefix, self.bin = self.tmp / "home", self.root / "opt" / "citar", self.tmp / "bin"
        for folder in (self.home, self.tmp / "tmp", self.stub, self.root / "etc" / "citar",
                       self.root / "var" / "lib" / "citar"):
            folder.mkdir(parents=True)
        (self.stub / "sudo").write_text('#!/bin/sh\nexec "$@"\n', encoding="ascii", newline="\n")
        (self.stub / "systemctl").write_text(_FAKE_SYSTEMCTL, encoding="ascii", newline="\n")
        for name in ("sudo", "systemctl"):
            (self.stub / name).chmod(0o755)
        self.log.write_text("", encoding="utf-8")

    def stop_server(self):
        pid = self.root / "citar.pid"
        if pid.exists():
            subprocess.run([str(self.stub / "systemctl"), "stop", "citar"], env=self.env(), timeout=30)

    def env(self, **extra: str) -> dict[str, str]:
        return bash_env(PATH=f"{self.stub}{os.pathsep}{os.environ.get('PATH', '')}", HOME=str(self.home),
                        TMPDIR=str(self.tmp / "tmp"), SHELL="/bin/bash", PIP_NO_INDEX="1",
                        PIP_DISABLE_PIP_VERSION_CHECK="1", PIP_NO_CACHE_DIR="1", FAKE_LOG=str(self.log),
                        STUB_PREFIX=str(self.prefix), STUB_ROOT=str(self.root), STUB_UNIT=str(self.root / "unit"),
                        STUB_BACKUP=str(DEPLOY / "citar-backup.sh"), CITAR_DEPLOY_ROOT=str(self.root),
                        CITAR_INSTALL_HEALTH_SECONDS="6", **extra)

    def install(self, version: str, *flags: str, **extra: str) -> tuple[int, str]:
        argv = [BASH, str(INSTALL_SH), "--server", *flags, "--python", sys.executable, "--prefix", str(self.prefix),
                "--bin-dir", str(self.bin), "--no-profile", "--no-setup"]
        if version:
            argv[3:3] = ["--from", str(self.wheels / f"citar-{version}-py3-none-any.whl")]
        done = subprocess.run(argv, env=self.env(**extra), capture_output=True, text=True, timeout=600,
                              stdin=subprocess.DEVNULL)
        return done.returncode, done.stdout + done.stderr

    def current(self) -> str:
        return os.readlink(self.prefix / "current")

    def tree(self) -> list[str]:
        return sorted(f"{p}->{os.readlink(p)}" if p.is_symlink() else str(p) for p in self.prefix.rglob("*")
                      if "site-packages" not in p.parts)

    def calls(self) -> list[str]:
        return self.log.read_text(encoding="utf-8").splitlines()

    def revision(self) -> str:
        db = sqlite3.connect(self.root / "var" / "lib" / "citar" / "citar.db")
        try:
            return db.execute("SELECT version_num FROM alembic_version").fetchone()[0]
        finally:
            db.close()

    def assertRelease(self, version: str):
        release = self.prefix / "releases" / version
        self.assertEqual(sorted(p.name for p in release.iterdir()),
                         [".complete", "SHA256SUMS", "citar-" + version + "-py3-none-any.whl", "freeze.txt", "venv"])
        check = subprocess.run(["sha256sum", "-c", "SHA256SUMS"], cwd=release, capture_output=True, text=True)
        self.assertEqual(check.returncode, 0, check.stdout + check.stderr)
        self.assertIn(f"citar @ file://{release}/citar-{version}-py3-none-any.whl",
                      (release / "freeze.txt").read_text(encoding="utf-8"))
        self.assertEqual((release / ".complete").read_text(encoding="utf-8"),
                         f"installed by install.sh: CITAR {version}\n")

    def test_a_first_install_a_refusal_a_running_release_without_its_mark_and_the_uninstall(self):
        code, out = self.install("0.1.6")
        self.assertEqual(code, 0, out)
        self.assertRelease("0.1.6")
        self.assertEqual(self.current(), "releases/0.1.6")
        for name in ("citar", "citar-mcp"):
            self.assertEqual(os.readlink(self.bin / name), f"{self.prefix}/current/venv/bin/{name}")
        self.assertFalse((self.bin / "citar-admin").exists(), "citar-admin is the wizard's wrapper, never linked")
        self.assertEqual(subprocess.run([str(self.bin / "citar"), "--version"], capture_output=True,
                                        text=True).stdout, "CITAR 0.1.6\n")
        self.assertIn(f"installed by install.sh: CITAR 0.1.6 ({self.prefix}/current)",
                      (self.prefix / ".citar-installer").read_text(encoding="utf-8"))
        self.assertEqual(list((self.tmp / "tmp").iterdir()), [], "a temporary folder was left")

        before = self.tree()
        code, out = self.install("0.1.5")
        self.assertEqual(code, 1, out)
        self.assertIn("CITAR 0.1.5 is older than the release that runs", out)
        self.assertEqual(self.tree(), before, "the refusal changed the layout")

        # The running release without .complete (citar-deploy's, or a hand-made one) is never made again.
        venv = self.prefix / "releases" / "0.1.6" / "venv"
        (self.prefix / "releases" / "0.1.6" / ".complete").unlink()
        (venv / "RUNNING").write_text("the server runs this\n", encoding="utf-8")
        code, out = self.install("0.1.6")
        self.assertEqual(code, 0, out)
        self.assertTrue((venv / "RUNNING").exists(), "the running release's venv was removed and made again")
        self.assertIn("is the release that runs (CITAR 0.1.6): kept as it is, and marked complete", out)
        self.assertEqual((self.prefix / "releases" / "0.1.6" / ".complete").read_text(encoding="utf-8"),
                         "marked by install.sh: CITAR 0.1.6, the release that runs\n")
        # ... and one whose citar does not say it is that release is refused, untouched.
        (self.prefix / "releases" / "0.1.6" / ".complete").unlink()
        (venv / "bin" / "citar").rename(venv / "bin" / "citar.moved")
        before = self.tree()
        code, out = self.install("0.1.6")
        self.assertEqual(code, 1, out)
        self.assertIn("never removes the release that runs", out)
        self.assertEqual(self.tree(), before)
        (venv / "bin" / "citar.moved").rename(venv / "bin" / "citar")

        # A half-made release that is not current is made again whole.
        half = self.prefix / "releases" / "0.1.7"
        (half / "venv").mkdir(parents=True)
        (half / "venv" / "HALF").write_text("", encoding="utf-8")
        code, out = self.install("0.1.7")      # no service: the switch alone
        self.assertEqual(code, 0, out)
        self.assertFalse((half / "venv" / "HALF").exists())
        self.assertRelease("0.1.7")
        self.assertEqual(self.current(), "releases/0.1.7")
        self.assertIn("No citar service is installed here, so nothing was stopped or started", out)
        self.assertFalse([line for line in self.calls() if " stop " in line or " start " in line], self.calls())

        code, out = self.install("", "--uninstall")
        self.assertEqual(code, 0, out)
        self.assertFalse(self.prefix.exists())
        self.assertEqual(list(self.bin.iterdir()), [])

    def service(self):
        """The citar service "installed" for the release that is current, and started."""
        port = _free_port()
        (self.root / "etc" / "citar" / "citar.env").write_text(
            f"CITAR_MODE=server\nCITAR_HOST=127.0.0.1\nCITAR_PORT={port}\nCITAR_STATE_DIR=/var/lib/citar\n",
            encoding="utf-8")
        (self.root / "unit").write_text(
            f"# {self.root}/etc/systemd/system/citar.service\n[Service]\n"
            f"ExecStart={self.prefix}/current/venv/bin/citar serve\nWorkingDirectory=/var/lib/citar\n"
            f"EnvironmentFile={self.root}/etc/citar/citar.env\n", encoding="utf-8")
        subprocess.run([str(self.stub / "systemctl"), "start", "citar"], env=self.env(), timeout=30, check=True)
        deadline = time.monotonic() + 20
        while "serving" not in self.log.read_text(encoding="utf-8"):
            self.assertLess(time.monotonic(), deadline, "the stand-in server did not start")
            time.sleep(0.2)

    def test_an_upgrade_with_the_service_installed_is_the_stopped_switch_and_a_failure_is_undone(self):
        code, out = self.install("0.1.6")
        self.assertEqual(code, 0, out)
        self.service()
        self.assertEqual(self.revision(), "rev-0.1.6")

        # 0.1.7: stopped, backed up, switched, its files written, started, answering as 0.1.7.
        self.log.write_text("", encoding="utf-8")
        code, out = self.install("0.1.7")
        self.assertEqual(code, 0, out)
        self.assertEqual(self.current(), "releases/0.1.7")
        self.assertEqual(self.revision(), "rev-0.1.7")
        self.assertIn("answers as CITAR 0.1.7", out)
        self.assertEqual([line for line in self.calls() if "serving" not in line], [
            "systemctl cat citar.service [current -> releases/0.1.6]",
            "systemctl stop citar [current -> releases/0.1.6]",
            "systemctl start citar-backup [current -> releases/0.1.6]",
            "citar 0.1.7: setup --server --refresh-units",
            "systemctl daemon-reload [current -> releases/0.1.7]",
            "systemctl start citar [current -> releases/0.1.7]",
        ])
        [backup] = sorted((self.root / "var" / "backups" / "citar").iterdir())
        release = dict(line.split("=", 1) for line in (backup / "RELEASE").read_text().splitlines())
        self.assertEqual((release["version"], release["revision"]), ("0.1.6", "rev-0.1.6"),
                         "the backup is of 0.1.6's database, taken before the switch")
        self.assertIn(f"the backup taken before the switch is {backup}", out)

        # 0.1.8 migrates and crashes: undone, the database put back, 0.1.7 answering again.
        self.log.write_text("", encoding="utf-8")
        code, out = self.install("0.1.8")
        self.assertEqual(code, 1, out)
        self.assertEqual(self.current(), "releases/0.1.7")
        self.assertEqual(self.revision(), "rev-0.1.7", "the database 0.1.8 migrated was not put back")
        self.assertIn("The database was restored: yes", out)
        self.assertIn("from rev-0.1.7 to rev-0.1.8", out)
        self.assertIn("citar runs 0.1.7 again", out)
        calls = [line for line in self.calls() if not line.startswith("systemctl show")]
        self.assertEqual(calls[:7], [
            "systemctl cat citar.service [current -> releases/0.1.7]",
            "systemctl stop citar [current -> releases/0.1.7]",
            "systemctl start citar-backup [current -> releases/0.1.7]",
            "citar 0.1.8: setup --server --refresh-units",
            "systemctl daemon-reload [current -> releases/0.1.8]",
            "systemctl start citar [current -> releases/0.1.8]",
            "citar 0.1.8: migrated, crashing",
        ])
        self.assertEqual(calls[7:], [
            "systemctl stop citar [current -> releases/0.1.8]",
            "citar 0.1.7: setup --server --refresh-units",
            "systemctl daemon-reload [current -> releases/0.1.7]",
            "systemctl start citar [current -> releases/0.1.7]",
            "citar 0.1.7: migrated, serving",
        ])
        self.assertRelease("0.1.8")

        # A backup that fails: nothing switched, 0.1.7 started again.
        self.log.write_text("", encoding="utf-8")
        code, out = self.install("0.1.9", STUB_BACKUP_FAIL="1")
        self.assertEqual(code, 1, out)
        self.assertIn("nothing was\nswitched and citar was started again on 0.1.7", out)
        self.assertEqual(self.current(), "releases/0.1.7")
        self.assertEqual(self.revision(), "rev-0.1.7")
        self.assertEqual([line for line in self.calls() if line.startswith("systemctl") and " show " not in line], [
            "systemctl cat citar.service [current -> releases/0.1.7]",
            "systemctl stop citar [current -> releases/0.1.7]",
            "systemctl start citar-backup [current -> releases/0.1.7]",
            "systemctl start citar [current -> releases/0.1.7]",
        ])


def _free_port() -> int:
    import socket

    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


class InstallShText(unittest.TestCase):
    """What install.sh --server does that only a real server install shows."""

    def test_the_server_install_links_through_current_and_not_citar_admin(self):
        text = INSTALL_SH.read_text(encoding="utf-8")
        server = text[text.index("a server: the release layout"):text.index("# Hands over to `citar setup`")]
        self.assertIn('for command_name in citar citar-mcp; do\n\t\t$SUDO ln -sfn "$CURRENT/venv/bin/$command_name"',
                      server)
        self.assertIn('$SUDO "$PYTHON" -m venv "$release/venv"', server)
        self.assertIn('switch_current "$p"', server)
        self.assertIn('run_setup "$CURRENT/venv/bin/citar"', server)
        self.assertIn('--only-binary=citar \\\n\t\t\t--prefer-binary "${staged}[$EXTRAS]"', server)
        self.assertIn("--no-cache-dir", server)
        self.assertIn('RELEASES="$PREFIX/releases"\nCURRENT="$PREFIX/current"\n', text)
        self.assertIn('os.replace(staged, link)', text)

    def test_the_uninstall_removes_only_the_wizard_s_own_files(self):
        text = INSTALL_SH.read_text(encoding="utf-8")
        marks = dict(line.split("|", 1) for line in
                     text[text.index('SERVER_FILES="') + 14:].split('"\n', 1)[0].splitlines())
        self.assertEqual(sorted(marks), ["/usr/local/bin/citar-admin", "/usr/local/bin/citar-backup"])
        for path, mark in marks.items():
            name = {"citar-admin": "citar-admin.sh", "citar-backup": "citar-backup.sh"}[Path(path).name]
            self.assertEqual(units.TARGETS[name], PurePosixPath(path))
            self.assertIn(mark, unit_text(name))


if __name__ == "__main__":
    unittest.main()
