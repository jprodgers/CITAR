"""The staging rehearsal's tooling (design/phase5-release.md P5.14, package 5-11): ``scripts/deploy/staging.sh``,
``staging_check.py`` and ``staging_wheels.sh``. The rehearsal itself runs only as root inside WSL, for about 35
minutes, so what these tests hold is what would otherwise break it silently until the next rehearsal:

- staging_check.py's pure parts: the percentile and the response-time figures, the report over a planted timeline
  (each span CITAR did not answer named by its step; the OOM check failing when the count rose), the load's spans,
  and ``measure --weights``;
- the couplings to the code base the planted releases need: the line ``staging_wheels.sh``'s plant expects once in
  ``citar/server/boot.py``'s ``startup()``, and the migrations' header that its ``add_migration`` and ``staging.sh``'s
  ``head_revision`` read with a regular expression (each script's own Python run on a copy of the real migrations,
  against Python's own reading of the files);
- the maturin container ``staging_wheels.sh`` builds in: package.yml's MATURIN_VERSION, pinned by digest;
- on Linux, both scripts parse, their usage errors exit 2, and ``staging_wheels.sh`` refuses a ``--clone`` or an
  ``--out`` under ``/mnt`` (Windows' disks in WSL), as given or as it resolves, before anything is written.

Standard library only, so the WSL gate runs this module with the system Python
(``python3 -m unittest discover -s tests -p test_staging.py``).
"""
from __future__ import annotations

import ast
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent
STAGING_SH = ROOT / "scripts" / "deploy" / "staging.sh"
STAGING_WHEELS = ROOT / "scripts" / "deploy" / "staging_wheels.sh"
STAGING_CHECK = ROOT / "scripts" / "deploy" / "staging_check.py"
BOOT = ROOT / "citar" / "server" / "boot.py"
MIGRATIONS = ROOT / "citar" / "migrations"
LINUX_BASH = sys.platform.startswith("linux") and bool(shutil.which("bash")) and bool(shutil.which("git"))


def _load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


check = _load("citar_staging_check_for_tests", STAGING_CHECK)


def scratch(prefix: str) -> Path:
    """A temporary folder: under CARGO_TARGET_DIR when it is set (a package's own folder on the laptop), else the
    system's; resolved, as the scripts resolve what they are given (macOS's /var link, a Windows runner's 8.3 name)."""
    base = os.environ.get("CARGO_TARGET_DIR")
    if base:
        Path(base).mkdir(parents=True, exist_ok=True)
    return Path(tempfile.mkdtemp(prefix=prefix, dir=base or None)).resolve()


def heredoc(script: Path, function: str) -> str:
    """The Python that shell function ``function`` of ``script`` runs from its ``<<'PY'`` here-document."""
    text = script.read_text(encoding="utf-8")
    start = text.index(f"\n{function}() {{")
    body = text.index("<<'PY'\n", start) + len("<<'PY'\n")
    return text[body:text.index("\nPY\n", body) + 1]


def run_python(code: str, *args: str) -> subprocess.CompletedProcess:
    """A script's here-document run as the script runs it (``python -I - ARGS``)."""
    return subprocess.run([sys.executable, "-I", "-", *args], input=code, capture_output=True, text=True,
                          encoding="utf-8", timeout=60, check=False)


def python_heads(versions: Path) -> list[str]:
    """The migration heads as Python itself reads the files (each module's ``revision`` and ``down_revision``
    assignments, whatever their annotation or quoting; a merge's tuple counts each of its parents)."""
    revs, downs = set(), set()
    for f in versions.glob("*.py"):
        for node in ast.parse(f.read_text(encoding="utf-8")).body:
            if isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
                name, value = node.target.id, node.value
            elif isinstance(node, ast.Assign) and len(node.targets) == 1 and isinstance(node.targets[0], ast.Name):
                name, value = node.targets[0].id, node.value
            else:
                continue
            if name not in ("revision", "down_revision") or value is None:
                continue
            got = ast.literal_eval(value)
            if name == "revision":
                revs.add(got)
            elif isinstance(got, (tuple, list)):
                downs.update(got)
            elif got is not None:
                downs.add(got)
    return sorted(revs - downs)


class Figures(unittest.TestCase):
    def test_percentile_is_the_nearest_rank(self):
        values = [i / 1000 for i in range(20, 0, -1)]          # 1 to 20 ms, in no order the function relies on
        self.assertIsNone(check.percentile([], 95))
        self.assertEqual(check.percentile(values, 95), 0.019)
        self.assertEqual(check.percentile(values, 50), 0.010)
        self.assertEqual(check.percentile(values, 100), 0.020)
        self.assertEqual(check.percentile([0.5], 1), 0.5)

    def test_times_are_milliseconds(self):
        self.assertEqual(check.times([]), {"n": 0})
        got = check.times([0.0421, 0.0440, 0.0419, 0.0702])
        self.assertEqual(got, {"n": 4, "p50_ms": 42.1, "p95_ms": 70.2, "max_ms": 70.2})

    def test_load_between_takes_the_requests_started_in_the_spans(self):
        load = check.Load("http://127.0.0.1:9/", 0.25, "test")    # never started: its samples are planted
        load.samples = [(1.0, 0.040, 200), (2.0, 0.050, 200), (2.5, 0.060, 0), (5.0, 0.070, 200), (9.0, 0.080, 500)]
        self.assertEqual(load.between([(2.0, 2.5)]), [0.050, 0.060])
        self.assertEqual(load.between([(0.0, 1.0), (4.0, 9.0)]), [0.040, 0.070, 0.080])
        self.assertEqual(load.between([(3.0, 4.0)]), [])
        self.assertEqual(load.failed_between([(0.0, 10.0)]), 2)
        self.assertEqual(load.failed_between([(4.0, 6.0)]), 0)

    def test_measure_reads_its_weights_in_either_spelling(self):
        base = ["measure", "--out", "m.json", "--site", "http://citar-staging.test:8088",
                "--standin", "http://127.0.0.1:8082/", "--password-file", "pw"]
        self.assertEqual(check.parser().parse_args(base + ["--weights", "50 35 20"]).weights, [50, 35, 20])
        self.assertEqual(check.parser().parse_args(base + ["--weights", "50, 20"]).weights, [50, 20])
        self.assertEqual(check.parser().parse_args(base + ["--weights", " 35 "]).weights, [35])
        self.assertEqual(check.parser().parse_args(base).weights, [50])


class Report(unittest.TestCase):
    """``staging_check.py report`` over a planted timeline and steps."""

    def setUp(self):
        self.dir = scratch("citar-staging-report-")
        self.addCleanup(shutil.rmtree, self.dir, True)
        self.cgroup = self.dir / "cgroup"
        self.cgroup.mkdir()
        (self.cgroup / "memory.events").write_text("low 0\nhigh 0\nmax 12\noom 1\noom_kill 1\n", encoding="utf-8")
        patcher = mock.patch.object(check, "cgroup_of", lambda unit: self.cgroup)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.addCleanup(check.FAILURES.clear)
        check.FAILURES.clear()

    def plant(self, rows: list[dict], phases: list[tuple[float, str]]) -> None:
        with open(self.dir / "watch.jsonl", "w", encoding="utf-8") as out:
            for row in rows:
                out.write(json.dumps(row) + "\n")
        with open(self.dir / "phases.jsonl", "w", encoding="utf-8") as out:
            for t, name in phases:
                out.write(json.dumps({"t": t, "phase": name}) + "\n")

    def report(self) -> tuple[int, dict]:
        with mock.patch("sys.stdout"):
            status = check.main(["report", "--logs", str(self.dir)])
        return status, json.loads((self.dir / "report.json").read_text(encoding="utf-8"))

    @staticmethod
    def row(t: float, up: bool | None, oom: int, mem_mb: int = 500, units: dict | None = None) -> dict:
        row = {"t": t, "slice": {"mem": mem_mb << 20, "swap": 0, "oom_kill": oom}, "units": units or {}}
        if up is not None:
            row["up"] = up
        return row

    def test_downtime_is_named_by_the_step_it_began_in(self):
        mib = 1 << 20
        pip = {"run-u7.scope": {"mem": 200 * mib, "peak": 310 * mib}}
        self.plant([
            self.row(100.0, True, 1),
            self.row(101.0, False, 1),                       # cutover: down for 2.5 s
            self.row(102.0, None, 1, units=pip),             # a sample without CITAR's answer: not a change
            self.row(103.5, True, 1, mem_mb=900, units=pip),
            self.row(200.0, False, 1),                       # rollback-restored: down for 2 s
            self.row(202.0, True, 1),
            self.row(300.0, False, 1),                       # uncutover, and still down at the last sample
            self.row(301.5, False, 1),
        ], [(99.0, "cutover"), (150.0, "measure"), (199.0, "rollback-restored"), (299.0, "uncutover")])
        status, report = self.report()
        self.assertEqual(status, 0)
        self.assertEqual(report["downtime"], [{"phase": "cutover", "seconds": 2.5},
                                              {"phase": "rollback-restored", "seconds": 2.0},
                                              {"phase": "uncutover", "seconds": 1.5, "to_the_end": True}])
        self.assertEqual(report["samples"], 8)
        self.assertEqual(report["slice_peak_mb"], 900.0)
        self.assertEqual(report["slice_peak_during"], "cutover")
        self.assertEqual(report["slice_peak_units_mb"], {"run-u7.scope": 200.0})
        self.assertEqual(report["pip_scopes"], {"run-u7.scope": {"seconds": 1.5, "peak_mb": 310.0}})
        self.assertEqual(report["slice_memory_events"]["max"], 12)
        self.assertEqual((report["oom_kill_first"], report["oom_kill_last"]), (1, 1))
        self.assertEqual(check.FAILURES, [])

    def test_an_oom_kill_during_the_run_fails_it(self):
        self.plant([self.row(10.0, True, 1), self.row(11.0, True, 1), self.row(12.0, True, 2)], [(9.0, "measure")])
        status, report = self.report()
        self.assertEqual(status, 1)
        self.assertEqual((report["oom_kill_first"], report["oom_kill_last"]), (1, 2))
        self.assertEqual(check.FAILURES, ["no OOM kill in the slice during the run"])


class PlantedReleases(unittest.TestCase):
    """What ``staging_wheels.sh`` changes in a scratch clone, run on copies of the real files."""

    def setUp(self):
        self.root = scratch("citar-staging-plant-")
        self.addCleanup(shutil.rmtree, self.root, True)

    def test_the_plant_finds_its_line_once_in_startup(self):
        code = heredoc(STAGING_WHEELS, "plant")
        anchor = ast.literal_eval(re.search(r"^anchor = (.+)$", code, re.M)[1])
        text = BOOT.read_text(encoding="utf-8")
        self.assertEqual(text.count(anchor), 1, f"{BOOT}: the plant's line {anchor!r}")
        tree = ast.parse(text)
        self.assertTrue(any(isinstance(n, ast.ClassDef) and n.name == "BootError" for n in tree.body),
                        "boot.py defines the BootError the plant raises")
        for how in ("after", "before"):
            with self.subTest(how=how):
                (self.root / "citar" / "server").mkdir(parents=True, exist_ok=True)
                planted = self.root / "citar" / "server" / "boot.py"
                shutil.copyfile(BOOT, planted)
                got = run_python(code, str(self.root), how)
                self.assertEqual(got.returncode, 0, got.stderr)
                self.assertIn(f"the server exits {how} it migrates", got.stdout)
                tree = ast.parse(planted.read_text(encoding="utf-8"))
                body = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == "startup").body
                kinds = ["raise" if isinstance(n, ast.Raise) else
                         "migrate" if isinstance(n, ast.Assign) and ast.unparse(n.value) == "migrate()" else "other"
                         for n in body]
                where = kinds.index("migrate")
                self.assertEqual(kinds.count("raise"), 1, kinds)
                self.assertEqual(kinds.index("raise"), where + 1 if how == "after" else where - 1, kinds)

    def test_the_plant_refuses_a_boot_without_its_line(self):
        (self.root / "citar" / "server").mkdir(parents=True)
        (self.root / "citar" / "server" / "boot.py").write_text("def startup():\n    migrate()\n", encoding="utf-8")
        got = run_python(heredoc(STAGING_WHEELS, "plant"), str(self.root), "after")
        self.assertNotEqual(got.returncode, 0)
        self.assertIn("is not where the plant expects it", got.stderr)

    def test_the_migrations_have_one_head_and_each_planted_one_follows_it(self):
        versions = self.root / "citar" / "migrations" / "versions"
        shutil.copytree(MIGRATIONS / "versions", versions, ignore=shutil.ignore_patterns("__pycache__"))
        head = python_heads(MIGRATIONS / "versions")
        self.assertEqual(len(head), 1, f"the real migrations have one head: {head}")
        read_head = heredoc(STAGING_SH, "head_revision")
        add = heredoc(STAGING_WHEELS, "add_migration")

        def scripts_head() -> str:
            got = run_python(read_head, str(versions))
            self.assertEqual(got.returncode, 0, got.stderr)
            return got.stdout.strip()

        self.assertEqual(scripts_head(), head[0], "staging.sh's head_revision reads the real migrations' head")
        before = head[0]
        for rev in ("9000a_staging_upgrade", "9000b_staging_failure"):
            got = run_python(add, str(self.root), rev)
            self.assertEqual(got.returncode, 0, got.stderr)
            self.assertEqual(got.stdout.strip(), f"migration {rev} after {before}")
            written = (versions / f"{rev}.py").read_text(encoding="utf-8")
            compile(written, f"{rev}.py", "exec")
            self.assertEqual(python_heads(versions), [rev])
            self.assertEqual(scripts_head(), rev)
            before = rev

    def test_a_second_head_stops_the_plant(self):
        versions = self.root / "citar" / "migrations" / "versions"
        versions.mkdir(parents=True)
        for rev in ("aaa", "bbb"):
            (versions / f"{rev}.py").write_text(f'revision: str = "{rev}"\ndown_revision = None\n', encoding="utf-8")
        got = run_python(heredoc(STAGING_WHEELS, "add_migration"), str(self.root), "9000a_staging_upgrade")
        self.assertNotEqual(got.returncode, 0)
        self.assertIn("not one head", got.stderr)
        self.assertFalse((versions / "9000a_staging_upgrade.py").exists())
        self.assertEqual(run_python(heredoc(STAGING_SH, "head_revision"), str(versions)).stdout.strip(), "")


class Container(unittest.TestCase):
    def test_the_wheels_are_built_with_package_yml_s_maturin_pinned_by_digest(self):
        text = STAGING_WHEELS.read_text(encoding="utf-8")
        maturin = re.search(r'^MATURIN="([^"]+)"$', text, re.M)[1]
        image = re.search(r'^IMAGE="([^"]+)"$', text, re.M)[1]
        package = (ROOT / ".github" / "workflows" / "package.yml").read_text(encoding="utf-8")
        self.assertEqual(maturin, re.search(r"^  MATURIN_VERSION: (\S+)$", package, re.M)[1])
        self.assertRegex(image, r"^ghcr\.io/pyo3/maturin@sha256:[0-9a-f]{64}$")


@unittest.skipUnless(LINUX_BASH, "Linux with bash and git (the scripts run in WSL only)")
class Scripts(unittest.TestCase):
    def setUp(self):
        self.home = scratch("citar-staging-home-")
        self.addCleanup(shutil.rmtree, self.home, True)

    def bash(self, script: Path, *args: str) -> subprocess.CompletedProcess:
        env = {**os.environ, "HOME": str(self.home)}
        return subprocess.run(["bash", str(script), *args], capture_output=True, text=True, timeout=60, env=env,
                              cwd=self.home, check=False)

    def test_both_scripts_parse(self):
        for script in (STAGING_SH, STAGING_WHEELS):
            got = subprocess.run(["bash", "-n", str(script)], capture_output=True, text=True, check=False)
            self.assertEqual(got.returncode, 0, f"{script.name}: {got.stderr}")

    def test_usage_errors_exit_2_before_anything_runs(self):
        cases = [
            (STAGING_SH, [], "which command"),
            (STAGING_SH, ["probes", "--cpu-quota", "0%"], "--cpu-quota"),
            (STAGING_SH, ["probes", "--cpu-quota", "100%"], "--cpu-quota"),
            (STAGING_SH, ["probes", "--cpu-period", "250"], "--cpu-period"),
            (STAGING_SH, ["setup", "--without-cpu-quota"], "--without-cpu-quota is probes' alone"),
            (STAGING_SH, ["run", "--weights", "60"], "--weights"),
            (STAGING_SH, ["setup", "--ballast-mb", "961"], "--ballast-mb"),
            (STAGING_SH, ["probes", "--bogus"], "unknown argument"),
            (STAGING_SH, ["setup", "run"], "one command at a time"),
            (STAGING_WHEELS, [], "--clone and --out are needed"),
            (STAGING_WHEELS, ["--clone", str(self.home)], "--clone and --out are needed"),
            (STAGING_WHEELS, ["--clone", str(self.home), "--bogus"], "unknown argument"),
        ]
        for script, args, says in cases:
            with self.subTest(script=script.name, args=args):
                got = self.bash(script, *args)
                self.assertEqual(got.returncode, 2, got.stdout + got.stderr)
                self.assertIn(says, got.stderr)

    def test_staging_wheels_never_writes_under_mnt(self):
        clone = self.home / "clone"
        clone.mkdir()
        git = ["git", "-C", str(clone), "-c", "user.name=t", "-c", "user.email=t@localhost"]
        subprocess.run(git + ["init", "--quiet"], check=True)
        (clone / "Cargo.toml").write_text('[workspace.package]\nversion = "0.1.6"\n', encoding="utf-8")
        subprocess.run(git + ["add", "Cargo.toml"], check=True)
        subprocess.run(git + ["commit", "--quiet", "-m", "t"], check=True)
        (self.home / "to-mnt").symlink_to("/mnt")
        name = f"citar-staging-test-{os.getpid()}"
        up = "/".join([".."] * len(self.home.parts[1:]))       # from HOME back to /
        cases = {
            "--out under /mnt (the reviewed hole: only --clone's start was looked at)": (
                ["--clone", str(clone), "--out", f"/mnt/{name}/out"], Path(f"/mnt/{name}")),
            "--out relative, resolving under /mnt": (
                ["--clone", str(clone), "--out", f"{up}/mnt/{name}/out"], Path(f"/mnt/{name}")),
            "--out through a link to /mnt": (
                ["--clone", str(clone), "--out", f"to-mnt/{name}/out"], Path(f"/mnt/{name}")),
            "--clone under /mnt": (
                ["--clone", f"/mnt/{name}", "--out", str(self.home / "out")], self.home / "out"),
        }
        for what, (args, never) in cases.items():
            with self.subTest(what):
                existed = never.exists()
                got = self.bash(STAGING_WHEELS, *args)
                if not existed and never.exists():
                    shutil.rmtree(never, ignore_errors=True)
                    self.fail(f"{never} was written")
                self.assertEqual(got.returncode, 1, got.stdout + got.stderr)
                self.assertIn("never under /mnt", got.stderr)
        self.assertEqual(sorted(p.name for p in self.home.iterdir()), ["clone", "to-mnt"], "nothing else written")


if __name__ == "__main__":
    unittest.main()
