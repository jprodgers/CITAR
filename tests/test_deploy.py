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
    """The environment for a script under test: no CITAR_* variable from outside, no colour."""
    env = {k: v for k, v in os.environ.items() if not k.upper().startswith("CITAR_")}
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
        state = self.tmp / "given"
        (state / "saves" / "lab").mkdir(parents=True)
        (state / "citar.db").write_bytes(b"")
        (state / "saves" / "lab" / "run.json").write_bytes(b"{}")
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
            self.server._give_to_service_user(state)
        self.assertEqual(sorted(Path(p).name for p, _, _ in given), ["citar.db", "given", "lab", "run.json", "saves"])
        self.assertEqual({(u, g) for _, u, g in given}, {("citar", "citar")})


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

    def sh(self, *args: str) -> tuple[int, str]:
        env = bash_env(HOME=msys(self.home), TMPDIR=msys(self.tmp / "tmp"), SHELL="/bin/bash",
                       XDG_CONFIG_HOME=msys(self.home / ".config"), XDG_DATA_HOME=msys(self.home / ".local" / "share"),
                       CITAR_INSTALL_TEST_FACTS=self.LINUX)
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

    @unittest.skipIf(os.name == "nt", "symbolic links (Git Bash's ln -s copies)")
    def test_a_running_release_is_kept_and_its_configuration_left(self):
        (self.prefix / "releases" / "0.1.6" / "venv" / "bin").mkdir(parents=True)
        (self.prefix / ".citar-installer").write_text("installed by install.sh\n", encoding="utf-8")
        os.symlink("releases/0.1.6", self.prefix / "current")
        before = sorted(str(p) for p in self.prefix.rglob("*"))
        code, out = self.sh("--server", "--plan", "--version", "0.1.7")
        self.assertEqual(code, 0, out)
        self.assertIn(f"running     {self.prefix}/current -> releases/0.1.6, kept beside the new release", out)
        self.assertIn(f"current     {self.prefix}/current -> releases/0.1.7", out)
        self.assertIn("the configuration and the service stay as they are; restart it to run 0.1.7", out)
        self.assertNotIn("setup --server writes", out)
        self.assertEqual(sorted(str(p) for p in self.prefix.rglob("*")), before)

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


class InstallShText(unittest.TestCase):
    """What install.sh --server does that only a real server install shows."""

    def test_the_server_install_links_through_current_and_not_citar_admin(self):
        text = INSTALL_SH.read_text(encoding="utf-8")
        server = text[text.index("install_server() {"):text.index("# Hands over to `citar setup`")]
        self.assertIn('for command_name in citar citar-mcp; do\n\t\t$SUDO ln -sfn "$CURRENT/venv/bin/$command_name"',
                      server)
        self.assertIn('$SUDO "$PYTHON" -m venv "$release/venv"', server)
        self.assertIn('switch_current "$p"', server)
        self.assertIn('run_setup "$CURRENT/venv/bin/citar"', server)
        self.assertIn('--only-binary=citar \\\n\t\t\t--prefer-binary "$staged[$EXTRAS]"', server)
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
