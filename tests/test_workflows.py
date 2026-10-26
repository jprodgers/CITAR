"""scripts/ci/check_workflows.py and scripts/ci/action_pins.py (design/phase5-release.md P5.17, package 5-02a).

The checker passes on the tree's workflows; each rule fails on a copy of them with one thing planted, naming its file,
its job and its rule (gate 1 of 5-02a, and more). package.yml's rules are tried on a small fixture of it written over
the real one in a copy (the fixture passes as written), and on the real one, which 5-02b added; what P5.3 says of
package.yml that no rule does (the five wheels, the smoke on 3.11 and 3.14, the sdist's build, the moved jobs) is
held by ThePackageWorkflow, and so is P5.4's part (5-03: the helpers' ten builds on five runners, the manifest signed
with a throwaway key and verified twice, helper.yml's artifacts built where the release builds them), P5.7's (5-06:
the image for both architectures, each built natively from its wheel, checked, saved and kept), and P5.9's and P5.10's
(5-08: the release assembled from every build's artifact by name, and the package managers run from it). The parts the
rules stand on (the expression reader, the shell reader, the matrix, an upload path) have tests of their own, and so
has the pin tool, offline, with GitHub stood in for.
"""
import importlib.util
import io
import shutil
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS = ROOT / ".github" / "workflows"


def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / "ci" / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module  # dataclasses look their module up while the class is made
    spec.loader.exec_module(module)
    return module


cw = load("check_workflows")
pins = load("action_pins")

SHA = "0123456789abcdef0123456789abcdef01234567"
CHECKOUT = "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1"
TAG_GUARD = "github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')"
TAG_ENV = "${{ github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v') && 'helper-signing' || '' }}"
#: The last line of release.yml's meta job: plants that need a step in a release.yml job go after it.
META_END = '        run: python scripts/release.py prepare --mode "$MODE" --version "$VERSION" --date "$DATE"\n'

#: A package.yml in 5-02b's shape, small: it passes every rule as written (Tree writes it over the real one).
PACKAGE_YML = f"""name: package

on:
  pull_request:
  workflow_dispatch:
  workflow_call:
    inputs:
      mode: {{ type: string, default: ci }}
      version: {{ type: string, default: "" }}
      date: {{ type: string, default: "" }}
      label: {{ type: string, default: "" }}

concurrency:
  group: package-${{{{ github.workflow }}}}-${{{{ github.ref }}}}
  cancel-in-progress: true

permissions:
  contents: read

env:
  MATURIN_VERSION: v1.15.0
  RELEASE_MODE: ${{{{ inputs.mode || 'ci' }}}}

jobs:
  wheels:
    runs-on: ${{{{ matrix.os }}}}
    strategy:
      matrix:
        include:
          - {{ os: windows-latest, platform: windows-x64 }}
          - {{ os: ubuntu-24.04, platform: linux-x64 }}
    steps:
      - uses: {CHECKOUT}
      - uses: actions/setup-python@5fda3b95a4ea91299a34e894583c3862153e4b97 # v7.0.0
        with:
          python-version: "3.12"
      - name: prepare
        shell: bash
        run: python scripts/release.py prepare --mode "$RELEASE_MODE"
      - uses: PyO3/maturin-action@e83996d129638aa358a18fbd1dfb82f0b0fb5d3b # v1.51.0
        with:
          maturin-version: ${{{{ env.MATURIN_VERSION }}}}
          command: build
          args: --release --locked --out dist
      - uses: actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9 # v7.0.2
        with:
          name: pkg-wheel-${{{{ matrix.platform }}}}
          path: dist/*.whl

  assemble:
    needs: wheels
    runs-on: ubuntu-24.04
    steps:
      - uses: {CHECKOUT}
      - run: python3 scripts/release.py prepare --mode "$RELEASE_MODE"
      - uses: actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2
        with:
          name: pkg-wheel-linux-x64
          path: one
      - uses: actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2
        with:
          pattern: pkg-*
          path: all
      - run: ls -R one all
"""


#: The key the Linux helper builds' container cache is kept under, in package.yml's helpers and helper.yml's artifact
#: (the same, so the two share it): the image, rustup's version, the lock and the toolchain.
CONTAINER_CACHE_KEY = ("helper-container-${{ matrix.container }}-rustup-${{ env.RUSTUP_INIT_VERSION }}-"
                       "${{ hashFiles('Cargo.lock', 'rust-toolchain.toml') }}")


def job_text(job_id, body):
    """A job to append to a workflow's jobs, its body indented under it."""
    return f"\n  {job_id}:\n" + "".join(f"    {line}\n" if line else "\n" for line in body.strip("\n").split("\n"))


class Tree:
    """A copy of the tree's workflows (and, when asked, package.yml's fixture) in a temporary folder."""

    def __init__(self, test, package=False):
        self.dir = tempfile.TemporaryDirectory()
        test.addCleanup(self.dir.cleanup)
        self.root = Path(self.dir.name)
        self.folder = self.root / ".github" / "workflows"
        self.folder.mkdir(parents=True)
        for path in WORKFLOWS.iterdir():
            if path.suffix in (".yml", ".yaml"):
                shutil.copy(path, self.folder / path.name)
        if package:
            self.write("package.yml", PACKAGE_YML)

    def read(self, name):
        return (self.folder / name).read_text(encoding="utf-8")

    def write(self, name, text):
        (self.folder / name).write_text(text, encoding="utf-8", newline="\n")

    def replace(self, name, old, new, count=1):
        text = self.read(name)
        assert text.count(old) >= count >= 1, f"{name} has {text.count(old)} of {old!r}"
        self.write(name, text.replace(old, new, count))

    def add_job(self, name, job_id, body):
        self.write(name, self.read(name).rstrip("\n") + "\n" + job_text(job_id, body))

    def replace_in_job(self, name, job_id, old, new):
        """``old`` replaced by ``new`` once, inside the job ``job_id``'s block only."""
        text = self.read(name)
        start = text.index(f"\n  {job_id}:\n")
        following = [text.index(f"\n  {other}:\n") for other in self.jobs(name) if f"\n  {other}:\n" in text
                     and text.index(f"\n  {other}:\n") > start]
        end = min(following, default=len(text))
        block = text[start:end]
        assert old in block, f"{job_id} in {name} has no {old!r}"
        self.write(name, text[:start] + block.replace(old, new, 1) + text[end:])

    def jobs(self, name):
        import yaml
        return list(yaml.safe_load(self.read(name))["jobs"])

    def findings(self):
        return cw.check(self.root)[1]


class TheTree(unittest.TestCase):
    def test_the_workflows_as_they_are_pass(self):
        workflows, findings = cw.check(ROOT)
        self.assertEqual(findings, [], "\n".join(map(str, findings)))
        names = {wf.name for wf in workflows}
        self.assertTrue({"release.yml", "test.yml", "package.yml", "docs.yml", "rust.yml", "helper.yml"} <= names,
                        names)

    def test_package_ymls_fixture_passes_beside_them(self):
        tree = Tree(self, package=True)
        self.assertEqual(tree.findings(), [], "\n".join(map(str, tree.findings())))

    def test_the_command_line(self):
        out = subprocess.run([sys.executable, str(ROOT / "scripts" / "ci" / "check_workflows.py")],
                             capture_output=True, text=True, encoding="utf-8", cwd=ROOT)
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertIn("rules 1 to 16 hold", out.stdout)
        tree = Tree(self)
        tree.replace("test.yml", "name: ext-${{ runner.os }}", "name: pkg-x")
        out = subprocess.run([sys.executable, str(ROOT / "scripts" / "ci" / "check_workflows.py"),
                              "--root", str(tree.root)], capture_output=True, text=True, encoding="utf-8")
        self.assertEqual(out.returncode, 1, out.stdout + out.stderr)
        self.assertIn("test.yml: job build-ext: rule 13:", out.stdout)

    def test_every_job_of_test_yml_prepares_and_the_linux_suite_runs_on_314(self):
        # The parts of P5.17's test.yml row a rule does not name: the 3.14 leg, the call's inputs, lint's checks.
        import yaml
        data = yaml.safe_load((WORKFLOWS / "test.yml").read_text(encoding="utf-8"))
        legs = data["jobs"]["test"]["strategy"]["matrix"]["include"]
        self.assertIn({"os": "ubuntu-latest", "python": "3.14"}, legs)
        self.assertEqual(set(data[True]["workflow_call"]["inputs"]), {"mode", "version", "date"})
        lint = [s.get("run", "") for s in data["jobs"]["lint"]["steps"]]
        self.assertIn("python scripts/ci/check_workflows.py", lint)
        self.assertIn("python scripts/release.py check", lint)
        self.assertIn("python scripts/ci/stale_terms.py", lint)
        release = yaml.safe_load((WORKFLOWS / "release.yml").read_text(encoding="utf-8"))
        self.assertEqual(list(release["jobs"]), list(cw.RELEASE_JOBS), "release.yml is P5.11's table, in its order")


class ThePackageWorkflow(unittest.TestCase):
    """package.yml (5-02b) as design/phase5-release.md P5.3 lays it out, in what no rule says; and the rules hold the
    real file, as a plant in each of its jobs shows."""

    @classmethod
    def setUpClass(cls):
        import yaml
        cls.data = yaml.safe_load((WORKFLOWS / "package.yml").read_text(encoding="utf-8"))
        cls.tests = yaml.safe_load((WORKFLOWS / "test.yml").read_text(encoding="utf-8"))
        cls.jobs = cls.data["jobs"]

    @staticmethod
    def runs(job: dict) -> str:
        return "\n".join(s.get("run", "") for s in job["steps"])

    @staticmethod
    def using(job: dict, action: str) -> list:
        return [s for s in job["steps"] if str(s.get("uses", "")).startswith(action + "@")]

    def assert_pinned_rustup(self, step: dict, env: dict):
        """A container build installs rustup from its archive by version, run only once its SHA-256 is its leg's."""
        run = step["run"]
        self.assertRegex(env["RUSTUP_INIT_VERSION"], r"^\d+\.\d+\.\d+$")
        self.assertEqual(step["env"]["RUSTUP_SHA256"], "${{ matrix.rustup_sha256 }}")
        self.assertIn("-e RUSTUP_INIT_VERSION -e RUSTUP_SHA256", run)
        self.assertNotIn("sh.rustup.rs", run)
        fetch = run.index('"https://static.rust-lang.org/rustup/archive/$RUSTUP_INIT_VERSION/$TARGET/rustup-init"')
        checked = run.index('echo "$RUSTUP_SHA256  /tmp/rustup-init" | sha256sum -c -')
        ran = run.index("/tmp/rustup-init -y --no-modify-path --profile minimal --default-toolchain none")
        self.assertLess(fetch, checked)
        self.assertLess(checked, ran, "the installer runs only once its hash is checked")
        self.assertIn("export CARGO_HOME=/io/target/container-cargo RUSTUP_HOME=/io/target/container-rustup", run)
        self.assertIn("export CARGO_TARGET_DIR=/io/target/container", run)

    def test_no_cache_reaches_a_release_build(self):
        # rust-cache and actions/cache restore what an earlier run saved: a run of its own (mode ci) only, so a
        # release's or a dry run's wheels, helpers and the xtask its signing job runs with the key are built from the
        # locked sources alone (P5.4.2)
        caches = 0
        for job_id, job in self.jobs.items():
            for step in job["steps"]:
                uses = str(step.get("uses", ""))
                if not uses.startswith(("Swatinem/rust-cache@", "actions/cache@")):
                    continue
                caches += 1
                with self.subTest(job=job_id, step=uses):
                    condition = str(step.get("if", ""))
                    self.assertIn("needs.meta.outputs.mode == 'ci'", [c.strip() for c in condition.split("&&")])
                    self.assertNotIn("||", condition)
        self.assertEqual(caches, 4, "wheels, helpers (twice) and helper-manifest")

    def test_its_triggers_inputs_group_and_permissions(self):
        on = self.data[True]
        self.assertEqual(set(on), {"push", "pull_request", "workflow_dispatch", "workflow_call"})
        self.assertEqual(on["push"], {"branches": ["main"]})
        self.assertEqual(set(on["workflow_call"]["inputs"]), {"mode", "version", "date", "label"})
        self.assertEqual(self.data["concurrency"]["group"], "package-${{ github.workflow }}-${{ github.ref }}")
        self.assertEqual(self.data["permissions"], {"contents": "read"})

    def test_one_maturin_for_the_test_and_the_release_wheels(self):
        self.assertEqual(self.data["env"]["MATURIN_VERSION"], self.tests["env"]["MATURIN_VERSION"])

    def test_every_job_takes_the_meta_jobs_values(self):
        for job_id, job in self.jobs.items():
            if job_id == "meta":
                continue
            with self.subTest(job=job_id):
                needs = [job["needs"]] if isinstance(job["needs"], str) else job["needs"]
                self.assertIn("meta", needs)
                for name, output in (("RELEASE_MODE", "mode"), ("RELEASE_VERSION", "version"),
                                     ("RELEASE_DATE", "date")):
                    self.assertEqual(job["env"][name], f"${{{{ needs.meta.outputs.{output} }}}}")
                if job_id not in ("wheels", "sdist", "sdist-build"):
                    continue
                self.assertEqual(job["env"]["PEP440"], "${{ needs.meta.outputs.pep440 }}")
        meta = self.runs(self.jobs["meta"])
        self.assertIn('--sha "$GITHUB_SHA"', meta)
        self.assertIn('--github-output "$GITHUB_OUTPUT"', meta)
        self.assertIn('"$RELEASE_LABEL" != "$label"', meta, "a caller's label must be this run's")

    def test_the_five_wheels_are_p532s_table(self):
        release = load_release()
        table = {  # platform: runner, target, tag, library, manylinux, macOS deployment target, uname -m
            "windows-x64": ("windows-latest", "x86_64-pc-windows-msvc", "cp311-abi3-win_amd64", "citar/_engine.pyd",
                            "off", "", "x86_64"),
            "macos-arm64": ("macos-15", "aarch64-apple-darwin", "cp311-abi3-macosx_11_0_arm64",
                            "citar/_engine.abi3.so", "off", "11.0", "arm64"),
            "macos-x64": ("macos-15-intel", "x86_64-apple-darwin", "cp311-abi3-macosx_11_0_x86_64",
                          "citar/_engine.abi3.so", "off", "11.0", "x86_64"),
            "linux-x64": ("ubuntu-24.04", "x86_64-unknown-linux-gnu", "cp311-abi3-manylinux_2_28_x86_64",
                          "citar/_engine.abi3.so", "2_28", "", "x86_64"),
            "linux-arm64": ("ubuntu-24.04-arm", "aarch64-unknown-linux-gnu", "cp311-abi3-manylinux_2_28_aarch64",
                            "citar/_engine.abi3.so", "2_28", "", "aarch64"),
        }
        legs = {leg["platform"]: leg for leg in self.jobs["wheels"]["strategy"]["matrix"]["include"]}
        self.assertEqual(set(legs), set(table))
        for platform, want in table.items():
            leg = legs[platform]
            got = (leg["runner"], leg["target"], leg["tag"], leg["library"], leg["manylinux"], leg["macos_target"],
                   leg["machine"])
            self.assertEqual(got, want, platform)
            # each on its own architecture: the Linux ones in the manylinux_2_28 container of the runner's, pinned
            # by its digest (rule 11)
            if leg["manylinux"] == "off":
                self.assertEqual(leg["container"], "", platform)
            else:
                self.assertRegex(leg["container"], rf"^quay\.io/pypa/manylinux_2_28_{leg['machine']}@sha256:[0-9a-f]{{64}}$",
                                 platform)
        # release.py's asset list (P5.10) names the same five wheels
        self.assertEqual(sorted(leg["tag"].removeprefix("cp311-abi3-") for leg in legs.values()),
                         sorted(release.WHEEL_TAGS))
        self.assertEqual(set(legs), set(release.HELPER_PLATFORMS))

    def test_every_wheel_carries_the_label_and_is_checked_and_smoked_on_311_and_314(self):
        job = self.jobs["wheels"]
        self.assertEqual(job["env"]["CITAR_BUILD_ID"], "${{ needs.meta.outputs.label }}")
        build = self.using(job, "PyO3/maturin-action")
        self.assertEqual(len(build), 1)
        self.assertEqual(build[0]["with"]["args"], "--release --locked --out dist")
        self.assertEqual(build[0]["with"]["docker-options"], "-e CITAR_BUILD_ID")
        self.assertEqual(build[0]["env"]["MACOSX_DEPLOYMENT_TARGET"], "${{ matrix.macos_target }}")
        # The native legs install rust-toolchain.toml's release after prepare and before the cache reads `rustc -vV`
        # for its key, as test.yml's build-ext does.
        steps = job["steps"]
        at = {name: next(i for i, s in enumerate(steps) if pick(s)) for name, pick in (
            ("prepare", lambda s: "scripts/release.py prepare" in s.get("run", "")),
            ("toolchain", lambda s: s.get("run", "").strip() == "rustup toolchain install"),
            ("cache", lambda s: str(s.get("uses", "")).startswith("Swatinem/rust-cache@")),
            ("build", lambda s: str(s.get("uses", "")).startswith("PyO3/maturin-action@")))}
        self.assertEqual(sorted(at, key=at.get), ["prepare", "toolchain", "cache", "build"])
        self.assertEqual(steps[at["toolchain"]].get("if"), "matrix.container == ''")
        self.assertEqual(steps[at["cache"]].get("if"), "matrix.container == '' && needs.meta.outputs.mode == 'ci'")
        runs = self.runs(job)
        self.assertIn('check_dist.py wheel dist --tag "$TAG" --library "$LIBRARY" --version "$PEP440"', runs)
        self.assertIn("twine check dist/*.whl", runs)
        self.assertIn('[ "$got" = "$WANT" ]', runs, "the job asserts its runner's architecture")
        interpreters = {s.get("id"): s["with"] for s in self.using(job, "actions/setup-python") if s.get("id")}
        self.assertEqual({k: v["python-version"] for k, v in interpreters.items()}, {"py311": "3.11", "py314": "3.14"})
        self.assertTrue(all(v["update-environment"] is False for v in interpreters.values()))
        for py, step_id in (("3.11", "py311"), ("3.14", "py314")):
            smoke = [s for s in job["steps"] if f'--out "smoke/smoke-$PLATFORM-{py}.json"' in s.get("run", "")]
            self.assertEqual(len(smoke), 1, py)
            self.assertEqual(smoke[0]["env"]["PY"], f"${{{{ steps.{step_id}.outputs.python-path }}}}")
            self.assertIn("scripts/ci/smoke_wheel.py smoke --wheel dist", smoke[0]["run"])
            self.assertIn('--label "$CITAR_BUILD_ID"', smoke[0]["run"])
        uploads = sorted(s["with"]["name"] for s in self.using(job, "actions/upload-artifact"))
        self.assertEqual(uploads, ["pkg-smoke-${{ matrix.platform }}-3.11", "pkg-smoke-${{ matrix.platform }}-3.14",
                                   "pkg-wheel-${{ matrix.platform }}"])

    def test_the_sdist_its_build_as_a_user_and_the_constraints(self):
        sdist = self.jobs["sdist"]
        self.assertEqual(self.using(sdist, "PyO3/maturin-action")[0]["with"]["command"], "sdist")
        self.assertIn('check_dist.py sdist dist --version "$PEP440"', self.runs(sdist))
        self.assertIn("twine check dist/*.tar.gz", self.runs(sdist))
        build = self.jobs["sdist-build"]
        self.assertNotIn("CITAR_BUILD_ID", build["env"], "a user's build carries no label")
        self.assertNotIn("CITAR_BUILD_ID", str(self.data["env"]))
        runs = self.runs(build)
        self.assertIn("env -u CITAR_BUILD_ID python -m pip wheel --disable-pip-version-check --no-deps", runs)
        self.assertIn("--label unknown", runs)
        self.assertEqual([s["with"]["name"] for s in self.using(build, "actions/download-artifact")], ["pkg-sdist"])
        constraints = self.jobs["constraints"]
        self.assertIn('--out "constraints/citar-$PEP440-server-constraints.txt"', self.runs(constraints))
        self.assertEqual([s["with"]["name"] for s in self.using(constraints, "actions/download-artifact")],
                         ["pkg-wheel-linux-x64"])
        self.assertEqual(self.using(constraints, "actions/setup-python")[0]["with"]["python-version"], "3.12")
        self.assertEqual([s["with"]["name"] for s in self.using(constraints, "actions/upload-artifact")],
                         ["pkg-constraints"])

    def test_the_moved_jobs_left_test_yml_for_the_release_wheels(self):
        self.assertFalse({"package", "installer", "docker"} & set(self.tests["jobs"]))
        # 5-06 replaced the moved docker job by the image matrix (test_the_image_is_built_natively_...), and 5-07 the
        # moved installer job by the install matrix (test_the_install_scripts_run_on_every_platform_...)
        self.assertNotIn("docker", self.jobs)
        self.assertNotIn("installer", self.jobs)

    def test_the_install_scripts_run_on_every_platform_against_its_own_files(self):
        """5-07 (P5.8): install.sh on Linux x64 and arm64 and both Macs, install.ps1 on Windows, each on the runner
        its wheel and helper are built on, against those two files (the helper from a loopback server), through
        install_check.py; the default route that writes the profile or PATH taken too, a runner's being its own."""
        job = self.jobs["install"]
        self.assertEqual(sorted(job["needs"]), ["helpers", "meta", "wheels"])
        self.assertEqual(job["if"], "${{ !cancelled() && needs.meta.result == 'success' }}")
        self.assertNotIn("environment", job)
        self.assertEqual(job["env"]["LABEL"], "${{ needs.meta.outputs.label }}")
        legs = {leg["platform"]: leg for leg in job["strategy"]["matrix"]["include"]}
        wheels = {leg["platform"]: leg for leg in self.jobs["wheels"]["strategy"]["matrix"]["include"]}
        helpers = {leg["platform"]: leg for leg in self.jobs["helpers"]["strategy"]["matrix"]["include"]}
        self.assertEqual(set(legs), set(load_release().HELPER_PLATFORMS))
        for platform, leg in legs.items():
            with self.subTest(platform=platform):
                for built in (wheels[platform], helpers[platform]):
                    self.assertEqual((leg["runner"], leg["machine"]), (built["runner"], built["machine"]))
                self.assertEqual(leg["asset"], helpers[platform]["asset"])
                self.assertEqual(leg["script"], "ps1" if platform.startswith("windows") else "sh")
        downloads = [(s["with"]["name"], s["with"]["path"]) for s in self.using(job, "actions/download-artifact")]
        self.assertEqual(downloads, [("pkg-wheel-${{ matrix.platform }}", "wheel"),
                                     ("pkg-helper-${{ matrix.platform }}", "helper")])
        runs = self.runs(job)
        self.assertIn('[ "$got" = "$WANT" ]', runs, "the job asserts its runner's architecture")
        check = [s for s in job["steps"] if "scripts/ci/install_check.py" in s.get("run", "")]
        self.assertEqual(len(check), 1)
        for flag in ('"$SCRIPT"', '--wheel "$(ls wheel/*.whl)"', '--helper "helper/$ASSET"',
                     '--version "$RELEASE_VERSION"', '--label "$LABEL"', '--under "$RUNNER_TEMP"',
                     '--work "$RUNNER_TEMP/install-check"', "--path-route", '--out "report/install-$PLATFORM.json"'):
            self.assertIn(flag, check[0]["run"])
        self.assertEqual(check[0]["env"]["SCRIPT"], "${{ matrix.script }}")
        self.assertEqual(self.using(job, "actions/setup-python")[0]["with"]["python-version"], "3.12")
        # install.ps1 runs through gate_run (install_check.py does that); no step installs, pushes or publishes
        self.assertNotIn("install.ps1", runs)
        self.assertNotIn("install.sh", runs)
        uploads = self.using(job, "actions/upload-artifact")
        self.assertEqual([(s["with"]["name"], s["with"]["path"]) for s in uploads],
                         [("pkg-install-report-${{ matrix.platform }}", "report/install-${{ matrix.platform }}.json")])
        self.assertEqual([s["uses"].split("@")[0] for s in job["steps"] if s.get("uses")],
                         ["actions/checkout", "actions/setup-python", "actions/download-artifact",
                          "actions/download-artifact", "actions/upload-artifact"])

    def test_the_image_is_built_natively_for_both_architectures_checked_and_kept(self):
        """5-06 (P5.7): linux/amd64 and linux/arm64, each on the runner its platform's wheel is built on, from that
        wheel and the release's constraints; run and checked by check_image.py; saved and loaded again; the file kept
        for release.yml on a dry run and a tag only. Nothing pushes, emulates or reads a cache."""
        job = self.jobs["image"]
        self.assertEqual(sorted(job["needs"]), ["constraints", "meta", "wheels"])
        self.assertEqual(job["if"], "${{ !cancelled() && needs.meta.result == 'success' }}")
        self.assertNotIn("environment", job)
        self.assertEqual(job["env"]["TAG"],
                         "ghcr.io/jprodgers/citar:${{ needs.meta.outputs.version }}-${{ matrix.arch }}")
        self.assertEqual(job["env"]["LABEL"], "${{ needs.meta.outputs.label }}")
        legs = {leg["arch"]: leg for leg in job["strategy"]["matrix"]["include"]}
        self.assertEqual({arch: (leg["platform"], leg["runner"], leg["machine"]) for arch, leg in legs.items()},
                         {"amd64": ("linux-x64", "ubuntu-24.04", "x86_64"),
                          "arm64": ("linux-arm64", "ubuntu-24.04-arm", "aarch64")})
        wheels = {leg["platform"]: leg for leg in self.jobs["wheels"]["strategy"]["matrix"]["include"]}
        for arch, leg in legs.items():
            with self.subTest(arch=arch):
                wheel = wheels[leg["platform"]]
                self.assertEqual((leg["runner"], leg["machine"]), (wheel["runner"], wheel["machine"]))
        self.assertEqual([(s["with"]["name"], s["with"]["path"]) for s in self.using(job, "actions/download-artifact")],
                         [("pkg-wheel-${{ matrix.platform }}", "wheelhouse"), ("pkg-constraints", "constraints")])
        steps, runs = job["steps"], self.runs(job)
        self.assertIn('[ "$got" = "$WANT" ]', runs, "the job asserts its runner's architecture")
        self.assertIn('cp "constraints/citar-$PEP440-server-constraints.txt" wheelhouse/constraints.txt', runs)
        at = {name: next(i for i, s in enumerate(steps) if pick(s.get("run", ""))) for name, pick in (
            ("prepare", lambda r: "scripts/release.py prepare" in r),
            ("context", lambda r: "wheelhouse/constraints.txt" in r and "cp " in r),
            ("build", lambda r: "docker buildx build" in r),
            ("run", lambda r: "check_image.py run" in r),
            ("save", lambda r: "check_image.py save" in r))}
        self.assertEqual(sorted(at, key=at.get), ["prepare", "context", "build", "run", "save"])
        build = steps[at["build"]]["run"]
        for flag in ("--load", '--platform "linux/$ARCH"', "--provenance=false", "--sbom=false",
                     '--label "org.opencontainers.image.version=$RELEASE_VERSION"', '--tag "$TAG" .'):
            self.assertIn(flag, build)
        check = steps[at["run"]]["run"]
        for flag in ('--engine docker --image "$TAG" --arch "$ARCH"', '--version "$RELEASE_VERSION"',
                     '--label "$LABEL"', '--wheel "$(ls wheelhouse/*.whl)"', "--constraints wheelhouse/constraints.txt",
                     '--out "report/image-$ARCH.json"'):
            self.assertIn(flag, check)
        saved = steps[at["save"]]["run"]
        self.assertIn('--out "image/citar-image-$RELEASE_VERSION-$ARCH.tar.zst" --report "report/image-$ARCH.json"',
                      saved)
        # nothing pushes, logs in, emulates another architecture or reads a cache
        for word in ("--push", "docker push", "docker login", "cache-from", "cache-to", "qemu", "binfmt"):
            self.assertNotIn(word, runs)
        self.assertEqual([s["uses"].split("@")[0] for s in steps if s.get("uses")],
                         ["actions/checkout", "actions/download-artifact", "actions/download-artifact",
                          "actions/upload-artifact", "actions/upload-artifact"])
        compose = [s for s in steps if "docker compose" in s.get("run", "")]
        self.assertEqual([s.get("if") for s in compose], ["matrix.arch == 'amd64'"])
        self.assertIn("docker compose -f docker-compose.yml config --quiet", compose[0]["run"])
        # the report on every run (5-08's assemble reads it); the image itself on a dry run and a tag only
        uploads = {s["with"]["name"]: s for s in self.using(job, "actions/upload-artifact")}
        self.assertEqual(set(uploads), {"pkg-image-report-${{ matrix.arch }}", "pkg-image-${{ matrix.arch }}"})
        report, image = uploads["pkg-image-report-${{ matrix.arch }}"], uploads["pkg-image-${{ matrix.arch }}"]
        self.assertNotIn("if", report)
        self.assertEqual(report["with"]["path"], "report/image-${{ matrix.arch }}.json")
        self.assertEqual(image["if"], "needs.meta.outputs.mode != 'ci'")
        self.assertEqual(image["with"]["path"],
                         "image/citar-image-${{ needs.meta.outputs.version }}-${{ matrix.arch }}.tar.zst")

    def test_the_helpers_are_p541s_ten_builds_on_five_runners(self):
        job = self.jobs["helpers"]
        wheels = {leg["platform"]: leg for leg in self.jobs["wheels"]["strategy"]["matrix"]["include"]}
        legs = {leg["platform"]: leg for leg in job["strategy"]["matrix"]["include"]}
        targets = {"windows-x64": "x86_64-pc-windows-msvc", "macos-arm64": "aarch64-apple-darwin",
                   "macos-x64": "x86_64-apple-darwin", "linux-x64": "x86_64-unknown-linux-gnu",
                   "linux-arm64": "aarch64-unknown-linux-gnu"}
        self.assertEqual(set(legs), set(targets))
        for platform, leg in legs.items():
            with self.subTest(platform=platform):
                self.assertEqual(leg["target"], targets[platform])
                self.assertEqual(leg["asset"], f"citar-helper-{platform}" + (".exe" if platform == "windows-x64" else ""))
                # on the wheel's own runner and architecture, the Linux ones in the wheel's image by the same digest
                self.assertEqual((leg["runner"], leg["machine"], leg["container"]),
                                 (wheels[platform]["runner"], wheels[platform]["machine"], wheels[platform]["container"]))
        self.assertEqual(job["env"]["CITAR_BUILD_ID"], "${{ needs.meta.outputs.label }}")
        self.assertEqual(job["env"]["MACOSX_DEPLOYMENT_TARGET"], "11.0")
        steps = job["steps"]

        def at(pick):
            found = [i for i, s in enumerate(steps) if pick(s)]
            self.assertEqual(len(found), 1, [s.get("name") for s in steps])
            return found[0]

        prepare = at(lambda s: "scripts/release.py prepare" in s.get("run", ""))
        toolchain = at(lambda s: s.get("run", "").strip() == "rustup toolchain install")
        cache = at(lambda s: str(s.get("uses", "")).startswith("Swatinem/rust-cache@"))
        kept = at(lambda s: str(s.get("uses", "")).startswith("actions/cache@"))
        native = at(lambda s: "cargo build" in s.get("run", "") and "docker run" not in s.get("run", ""))
        container = at(lambda s: "docker run" in s.get("run", "") and "cargo build" in s.get("run", ""))
        glibc = at(lambda s: "helper_release.py glibc" in s.get("run", ""))
        stage = at(lambda s: "helper_release.py stage" in s.get("run", ""))
        check = at(lambda s: "helper_release.py check" in s.get("run", ""))
        self.assertLess(prepare, toolchain)
        self.assertLess(toolchain, cache)
        self.assertLess(cache, kept)
        self.assertLess(kept, native)
        self.assertLess(native, container)
        self.assertLess(container, glibc)
        self.assertLess(glibc, stage)
        self.assertLess(stage, check)
        for i in (toolchain, native):
            self.assertEqual(steps[i].get("if"), "matrix.container == ''", steps[i].get("name"))
        for i in (container, glibc):
            self.assertEqual(steps[i].get("if"), "matrix.container != ''", steps[i].get("name"))
        # the caches, a run of its own's only: what a release or a dry run ships is built from the locked sources
        self.assertEqual(steps[cache]["if"], "matrix.container == '' && needs.meta.outputs.mode == 'ci'")
        self.assertEqual(steps[kept]["if"], "matrix.container != '' && needs.meta.outputs.mode == 'ci'")
        self.assertEqual(steps[kept]["with"]["path"], "target/container\ntarget/container-cargo\ntarget/container-rustup\n")
        self.assertEqual(steps[kept]["with"]["key"], CONTAINER_CACHE_KEY)
        # both builds, each literally (cargo xtask check reads every line: no test feature, no list in a variable)
        for i in (native, container):
            run = steps[i]["run"]
            self.assertIn('cargo build --release --locked -p citar-helper --target "$TARGET"\n', run)
            self.assertIn('cargo build --release --locked -p citar-helper --target "$TARGET" --features packaged\n', run)
        run = steps[container]["run"]
        self.assertEqual(steps[container]["env"]["IMAGE"], "${{ matrix.container }}")
        self.assertIn('-e CITAR_BUILD_ID', run, "the label reaches the container")
        self.assertIn('"$IMAGE"', run)
        self.assertIn("rustup toolchain install", run, "rust-toolchain.toml's toolchain, installed in the image")
        self.assert_pinned_rustup(steps[container], job["env"])
        self.assertIn('cp "$CARGO_TARGET_DIR/$TARGET/release/citar-helper" build/packaged/\n', run)
        self.assertIn('chown -R "$OWNER" build target', run, "the builds and the cached folders, the runner's again")
        for platform, leg in legs.items():
            with self.subTest(platform=platform):
                self.assertRegex(leg["rustup_sha256"], r"^[0-9a-f]{64}$" if leg["container"] else r"^$")
        self.assertIn("--max 2.28 build/self/citar-helper build/packaged/citar-helper", steps[glibc]["run"])
        self.assertIn('--version "$RELEASE_VERSION" --out stage', steps[stage]["run"])
        run = steps[check]["run"]
        for words in ('--label "$CITAR_BUILD_ID"', '--under "$RUNNER_TEMP"', '--work "$RUNNER_TEMP/helper-check"',
                      '--out "report/helper-$PLATFORM.json"', '--macos-min "$MACOSX_DEPLOYMENT_TARGET"'):
            self.assertIn(words, run)
        fedora = [s for s in steps if "FEDORA_IMAGE" in (s.get("env") or {})]
        self.assertEqual(len(fedora), 1)
        self.assertEqual(fedora[0]["if"], "matrix.platform == 'linux-x64'")
        # Docker Hub's official image: quay.io/fedora/fedora deletes a digest within a day of the daily rebuild.
        self.assertRegex(fedora[0]["env"]["FEDORA_IMAGE"], r"^docker\.io/library/fedora@sha256:[0-9a-f]{64}$")
        self.assertIn('grep -qi "certificate"', fedora[0]["run"])
        self.assertEqual(fedora[0]["run"].count("--home /tmp/citar-helper-home"), 2, "every run has a home")
        uploads = {s["with"]["name"]: s["with"]["path"] for s in self.using(job, "actions/upload-artifact")}
        self.assertEqual(uploads, {
            "pkg-helper-${{ matrix.platform }}": "stage/${{ matrix.asset }}\nstage/${{ matrix.asset }}.sha256\n",
            "pkg-helper-packaged-${{ matrix.platform }}":
                "stage/citar-helper-${{ needs.meta.outputs.version }}-${{ matrix.platform }}.*",
            "pkg-helper-report-${{ matrix.platform }}": "report/helper-${{ matrix.platform }}.json"})

    def test_the_windows_bundle_and_installer_come_from_the_wheel(self):
        """P5.6: the bundle from this run's Windows wheel in a fresh venv (never the checkout), PyInstaller pinned and
        run with -P from outside the checkout, the bundle's checks, Inno Setup with the three defines, the silent
        per-user install, run and uninstall, the zip; the release's two Windows assets uploaded."""
        job = self.jobs["windows"]
        self.assertEqual(job["needs"], ["meta", "wheels"])
        self.assertEqual(job["runs-on"], "windows-latest")
        self.assertEqual(job["if"], "${{ !cancelled() && needs.meta.result == 'success' }}")
        env = job["env"]
        self.assertEqual((env["NUMERIC"], env["LABEL"]),
                         ("${{ needs.meta.outputs.numeric }}", "${{ needs.meta.outputs.label }}"))
        for name in ("PYINSTALLER_VERSION", "PYINSTALLER_HOOKS_VERSION", "INNO_SETUP_VERSION"):
            self.assertRegex(env[name], r"^\d+\.\d+(\.\d+)?$", name)
        self.assertNotIn("CITAR_BUILD_ID", env, "nothing is compiled here: the label is the wheel's")
        self.assertEqual([s["with"]["python-version"] for s in self.using(job, "actions/setup-python")], ["3.12"])
        self.assertEqual([(s["with"]["name"], s["with"]["path"]) for s in self.using(job, "actions/download-artifact")],
                         [("pkg-wheel-windows-x64", "wheel")])
        steps = job["steps"]

        def at(text):
            found = [i for i, s in enumerate(steps) if text in s.get("run", "")]
            self.assertEqual(len(found), 1, text)
            return found[0]

        order = [at("scripts/release.py prepare"), at('python -m venv "$RUNNER_TEMP/bundle-venv"'),
                 at("-P -m PyInstaller"), at("check_bundle.py bundle"), at("choco install innosetup"),
                 at("/DAppVersion="), at("/VERYSILENT"), at("check_bundle.py zip")]
        self.assertEqual(order, sorted(order))
        venv = steps[order[1]]["run"]
        self.assertIn('--only-binary=:all: "$(ls wheel/*.whl)[all]"', venv)
        self.assertIn('"pyinstaller==$PYINSTALLER_VERSION" "pyinstaller-hooks-contrib==$PYINSTALLER_HOOKS_VERSION"',
                      venv)
        bundle = steps[order[2]]["run"]
        self.assertLess(bundle.index('cd "$RUNNER_TEMP/bundle"'), bundle.index("-P -m PyInstaller"),
                        "PyInstaller runs from a folder outside the checkout")
        self.assertIn('-P -m PyInstaller "$GITHUB_WORKSPACE/installer/citar.spec"', bundle)
        self.assertIn('--distpath "$RUNNER_TEMP/bundle/dist" --workpath "$RUNNER_TEMP/bundle/work"', bundle)
        check = steps[order[3]]["run"]
        for words in ('--version "$RELEASE_VERSION"', '--label "$LABEL"',
                      '--python "$RUNNER_TEMP/bundle-venv/Scripts/python.exe"', "--out report/windows-bundle.json"):
            self.assertIn(words, check)
        compile_ = steps[order[5]]["run"]
        for words in ('"/DAppVersion=$env:RELEASE_VERSION"', '"/DAppVersionNumeric=$env:NUMERIC"',
                      '"/DBundleDir=$env:RUNNER_TEMP\\bundle\\dist\\CITAR"', '"/O$env:RUNNER_TEMP\\installer"',
                      "installer\\citar.iss"):
            self.assertIn(words, compile_)
        install = steps[order[6]]["run"]
        for words in ("/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /CURRENTUSER", '/TASKS=`"`"',
                      "check_bundle.py installed", 'unins000.exe" -Wait -PassThru -ArgumentList "/VERYSILENT',
                      "check_bundle.py uninstalled", "keep-me.txt"):
            self.assertIn(words, install)
        zip_ = steps[order[7]]["run"]
        self.assertIn('cp "$RUNNER_TEMP/installer/CITAR-$RELEASE_VERSION-setup.exe" windows/', zip_)
        self.assertIn('check_bundle.py sums "windows/CITAR-$RELEASE_VERSION-setup.exe"', zip_)
        for step in steps:
            self.assertNotIn("make_icon", step.get("run", ""), "the icon is committed; nothing writes the checkout")
        uploads = {s["with"]["name"]: s["with"]["path"] for s in self.using(job, "actions/upload-artifact")}
        self.assertEqual(set(uploads), {"pkg-windows", "pkg-windows-report"})
        self.assertEqual(uploads["pkg-windows-report"], "report/windows-*.json")
        # exactly the release's Windows assets (scripts/release.py's windows group), each with its .sha256
        release = load_release()
        want = [a["name"].replace("9.9.9", "${{ needs.meta.outputs.version }}") for a in release.assets("9.9.9")
                if a["group"] == "windows"]
        self.assertEqual(len(want), 2, want)
        self.assertEqual(sorted(uploads["pkg-windows"].splitlines()),
                         sorted(f"windows/{name}{sha}" for name in want for sha in ("", ".sha256")))

    def test_the_helper_manifest_is_signed_with_a_throwaway_key_and_verified_twice(self):
        job = self.jobs["helper-manifest"]
        self.assertEqual(job["needs"], ["meta", "helpers"])
        self.assertEqual(job["runs-on"], "ubuntu-24.04")
        self.assertNotIn("environment", job)
        self.assertEqual([(s["with"]["name"], s["with"]["path"]) for s in self.using(job, "actions/download-artifact")],
                         [(f"pkg-helper-{p}", "assets") for p in ("windows-x64", "macos-arm64", "macos-x64", "linux-x64",
                                                                 "linux-arm64")])
        run = self.runs(job)
        # xtask built once, and that binary runs every step (it is the one uploaded)
        self.assertEqual(run.count("cargo build"), 1)
        self.assertIn("cargo build --release --locked -p xtask", run)
        self.assertNotIn("cargo xtask", run)
        self.assertNotIn("cargo run", run)
        for command in ("helper-manifest --assets assets", "keygen --out \"$key\"", "sign-manifest --key",
                        "verify-manifest --pub dry-run/dry-run-signing.pub"):
            self.assertIn(f"xtask-bin/xtask {command}", run)
        self.assertIn('key="$RUNNER_TEMP/throwaway"', run, "the key is made in the runner's temporary folder")
        self.assertIn("python scripts/ci/verify_manifest.py --pub dry-run/dry-run-signing.pub", run)
        self.assertIn('[ "$rc" = 1 ]', run, "a changed byte and another key each fail both verifiers")
        self.assertIn("for verifier in xtask python", run)
        self.assertIn("verify_manifest.py --check-keys crates/citar-jobs/update-keys.pub", run)
        self.assertIn('grep -q "^updates: unavailable"', run)
        self.assertIn("scripts/ci/gate_run.py helper --under \"$RUNNER_TEMP\"", run)
        # the xtask the release's signing job runs with the key is never linked from a cache
        self.assertEqual([s.get("if") for s in self.using(job, "Swatinem/rust-cache")],
                         ["needs.meta.outputs.mode == 'ci'"])
        uploads = {s["with"]["name"]: s["with"]["path"] for s in self.using(job, "actions/upload-artifact")}
        self.assertEqual(uploads, {
            "pkg-helper-manifest": "manifest/citar-helper-manifest.json",
            "pkg-helper-manifest-dry-run": "dry-run/citar-helper-manifest.json.sig\ndry-run/dry-run-signing.pub\n",
            "pkg-xtask-linux-x64": "xtask-bin/xtask\nxtask-bin/xtask.sha256\n"})

    def test_the_notices_are_held_to_the_lock_and_uploaded_for_the_release(self):
        """5-04 (P5.5): the third-party notices a release carries are proved current in the release's own run (release.yml
        calls no rust.yml), from the locked sources alone, and uploaded under the release asset's name."""
        job = self.jobs["notices"]
        self.assertEqual(job["needs"], "meta")
        self.assertNotIn("environment", job)
        run = self.runs(job)
        self.assertLess(run.index("cargo fetch --locked"), run.index("cargo xtask licenses --check"))
        self.assertFalse(self.using(job, "Swatinem/rust-cache") or self.using(job, "actions/cache"))
        uploads = [(s["with"]["name"], s["with"]["path"]) for s in self.using(job, "actions/upload-artifact")]
        self.assertEqual(uploads, [("pkg-third-party-licenses", "packaging/THIRD-PARTY-LICENSES.txt")])
        release = load_release()
        notices = [a["name"] for a in release.assets("0.1.6") if a["group"] == "notices"]
        self.assertEqual(notices, [Path(uploads[0][1]).name])
        # the packaged helpers' archives carry the same file beside the binary
        self.assertEqual(helper_release_texts()["THIRD-PARTY-LICENSES.txt"], uploads[0][1])

    def test_helper_ymls_artifacts_build_where_the_release_builds(self):
        import yaml
        helper = yaml.safe_load((WORKFLOWS / "helper.yml").read_text(encoding="utf-8"))
        artifact = helper["jobs"]["artifact"]
        legs = {leg["target"]: leg for leg in artifact["strategy"]["matrix"]["include"]}
        release = {leg["target"]: leg for leg in self.jobs["helpers"]["strategy"]["matrix"]["include"]}
        self.assertEqual({t: (leg["os"], leg["asset"], leg["container"]) for t, leg in legs.items()},
                         {t: (leg["runner"], leg["asset"], leg["container"]) for t, leg in release.items()})
        # the same rustup-init, by version and SHA-256 a target
        self.assertEqual({t: leg["rustup_sha256"] for t, leg in legs.items()},
                         {t: leg["rustup_sha256"] for t, leg in release.items()})
        self.assertEqual(artifact["env"]["RUSTUP_INIT_VERSION"], self.jobs["helpers"]["env"]["RUSTUP_INIT_VERSION"])
        self.assertEqual(artifact["env"]["MACOSX_DEPLOYMENT_TARGET"], "11.0")
        steps = artifact["steps"]
        glibc = [s for s in steps if "helper_release.py glibc --max 2.28" in s.get("run", "")]
        self.assertEqual([s.get("if") for s in glibc], ["matrix.container != ''"])
        macos = [s for s in steps if "helper_release.py macos" in s.get("run", "")]
        self.assertEqual([(s.get("if"), s["run"]) for s in macos],
                         [("runner.os == 'macOS'",
                           'python3 scripts/ci/helper_release.py macos --min "$MACOSX_DEPLOYMENT_TARGET" "dist/$ASSET"')])
        container = [s for s in steps if "docker run" in s.get("run", "")]
        self.assertEqual(len(container), 1)
        self.assert_pinned_rustup(container[0], artifact["env"])
        self.assertIn('chown -R "$OWNER" dist target', container[0]["run"])
        kept = self.using(artifact, "actions/cache")
        self.assertEqual([(s["if"], s["with"]["path"], s["with"]["key"]) for s in kept],
                         [("matrix.container != ''", "target/container\ntarget/container-cargo\ntarget/container-rustup\n",
                           CONTAINER_CACHE_KEY)])
        runs = "\n".join(s.get("run", "") for s in steps)
        self.assertIn('"$IMAGE"', runs)
        for flag in ("--version", "--self-test", "service install --dry-run"):
            self.assertIn(f'{flag} --home "$RUNNER_TEMP/citar-dry-run"', runs, flag)
        # the workspace the container mounts holds no token: nothing in helper.yml needs one
        for job_id, job in helper["jobs"].items():
            for step in self.using(job, "actions/checkout"):
                with self.subTest(job=job_id):
                    self.assertIs((step.get("with") or {}).get("persist-credentials"), False)
        # no workflow pipes a network installer into a shell
        for path in sorted(WORKFLOWS.glob("*.yml")):
            with self.subTest(workflow=path.name):
                self.assertNotIn("sh.rustup.rs", path.read_text(encoding="utf-8"))
        triggers = helper[True]
        for event in ("push", "pull_request"):
            self.assertIn("scripts/ci/helper_release.py", triggers[event]["paths"])
            self.assertIn("scripts/ci/gate_run.py", triggers[event]["paths"])

    def uploads_of(self, job_id: str) -> set:
        """The artifact names a job uploads, each matrix leg's."""
        job = self.jobs[job_id]
        names = set()
        for step in self.using(job, "actions/upload-artifact"):
            for index, leg in enumerate(cw.matrix_legs(job)):
                names.add(cw.expand(step["with"]["name"], leg, None, index))
        return names

    def test_assemble_downloads_every_release_file_and_report_by_name(self):
        """5-08 (P5.10): every build first; each artifact by its exact name, the release's files into one folder and
        the reports into another; each uploaded by a job of this workflow on every run (none on a dry run or a tag
        alone); then assemble_release.py, twine, the manifests' structure and sha256sum -c; and the three uploads."""
        release = load_release()
        job = self.jobs["assemble"]
        self.assertEqual(set(job["needs"]), set(self.jobs) - {"assemble", "managers"},
                         "assemble needs every build and every check")
        self.assertNotIn("if", job, "a failed build or check leaves nothing to assemble")
        self.assertEqual(job["env"]["LABEL"], "${{ needs.meta.outputs.label }}")
        downloads = [(s["with"]["name"], s["with"]["path"]) for s in self.using(job, "actions/download-artifact")]
        platforms = release.HELPER_PLATFORMS
        files = {f"pkg-wheel-{p}" for p in platforms} | {f"pkg-helper-{p}" for p in platforms} | \
            {f"pkg-helper-packaged-{p}" for p in platforms} | \
            {"pkg-sdist", "pkg-constraints", "pkg-windows", "pkg-helper-manifest", "pkg-third-party-licenses"}
        reports = {f"pkg-smoke-{p}-{py}" for p in platforms for py in ("3.11", "3.14")} | \
            {f"pkg-helper-report-{p}" for p in platforms} | {f"pkg-install-report-{p}" for p in platforms} | \
            {"pkg-smoke-sdist", "pkg-windows-report", "pkg-image-report-amd64", "pkg-image-report-arm64"}
        self.assertEqual(sorted(n for n, path in downloads if path == "release"), sorted(files))
        self.assertEqual(sorted(n for n, path in downloads if path == "reports"), sorted(reports))
        self.assertEqual(len(downloads), len(files) + len(reports), "two folders, each name once")
        uploaded = set()
        for job_id, other in self.jobs.items():
            if job_id in ("assemble", "managers"):
                continue
            for step in self.using(other, "actions/upload-artifact"):
                if "mode != 'ci'" in str(step.get("if", "")):
                    continue          # pkg-image-<arch>: a dry run's and a tag's only, and no release file
                for index, leg in enumerate(cw.matrix_legs(other)):
                    uploaded.add(cw.expand(step["with"]["name"], leg, None, index))
        self.assertLessEqual(files | reports, uploaded, "every artifact assemble reads is uploaded on every run")
        run = self.runs(job)
        self.assertIn('assemble_release.py assemble --version "$RELEASE_VERSION" --label "$LABEL"', run)
        self.assertIn("--release release --reports reports --preview preview", run)
        self.assertIn('check_managers.py structure --dir preview/managers --version "$RELEASE_VERSION"', run)
        self.assertIn("--assets release", run)
        self.assertIn('twine check "release/citar-$PEP440.tar.gz" release/citar-"$PEP440"-cp311-abi3-*.whl', run)
        self.assertIn("(cd release && sha256sum -c SHA256SUMS)", run)
        self.assertLess(run.index("assemble_release.py assemble"), run.index("sha256sum -c"))
        uploads = {s["with"]["name"]: s["with"]["path"] for s in self.using(job, "actions/upload-artifact")}
        self.assertEqual(uploads, {
            "pkg-release-assets": "release",
            "pkg-python-dists": "release/citar-${{ needs.meta.outputs.pep440 }}.tar.gz\n"
                                "release/citar-${{ needs.meta.outputs.pep440 }}-cp311-abi3-*.whl\n",
            "pkg-release-preview": "preview"})
        # The release's files are compressed already: uploaded as they are, not zipped again
        levels = {s["with"]["name"]: s["with"].get("compression-level") for s in self.using(job,
                                                                                         "actions/upload-artifact")}
        self.assertEqual(levels, {"pkg-release-assets": 0, "pkg-python-dists": 0, "pkg-release-preview": None})
        # assemble_release.py reads exactly the reports downloaded (each artifact's one file, the bundle's three)
        assemble = load("assemble_release")
        self.assertEqual(len(assemble.expected_reports()), len(reports))

    def test_assemble_runs_nothing_from_pypi_before_its_uploads(self):
        """5-08's review: what assemble uploads is what release.yml attests and publishes, so until the uploads the
        job runs only the checkout's scripts and coreutils (assemble_release.py, sha256sum -c), and twine and PyYAML
        are installed (wheels only) after them; their checks still fail the job."""
        steps = self.jobs["assemble"]["steps"]
        uploads = [i for i, s in enumerate(steps) if str(s.get("uses", "")).startswith("actions/upload-artifact@")]
        self.assertEqual(len(uploads), 3)
        self.assertEqual(uploads, list(range(uploads[0], uploads[0] + 3)), "the three uploads together")
        before, after = steps[:uploads[0]], steps[uploads[-1] + 1:]
        actions = {str(s["uses"]).split("@")[0] for s in before if "uses" in s}
        self.assertEqual(actions, {"actions/checkout", "actions/setup-python", "actions/download-artifact"})
        runs_before = self.runs({"steps": before})
        for word in ("pip", "twine", "uv ", "pipx", "check_managers.py"):
            self.assertNotIn(word, runs_before, f"{word} runs before the uploads")
        self.assertIn("assemble_release.py assemble", runs_before)
        self.assertIn("(cd release && sha256sum -c SHA256SUMS)", runs_before)
        self.assertLess(runs_before.index("assemble_release.py assemble"), runs_before.index("sha256sum -c"))
        runs_after = self.runs({"steps": after})
        self.assertIn("pip install --disable-pip-version-check --only-binary=:all: --upgrade twine pyyaml", runs_after)
        self.assertLess(runs_after.index("pip install"), runs_after.index("check_managers.py structure"))
        self.assertLess(runs_after.index("pip install"), runs_after.index("twine check"))
        for step in after:
            self.assertNotIn("if", step, "the checks after the uploads run, and fail the job, on every run")
            self.assertNotIn("continue-on-error", step)

    def test_the_managers_install_from_the_assembled_release(self):
        """5-08 (P5.9): Scoop and winget on Windows, Homebrew on both Macs and Linux x64, each on the runner its
        platform's builds come from, from pkg-release-assets; Scoop's installer pinned by commit and SHA-256 and run
        with -RunAsAdmin only once checked; a report per platform."""
        job = self.jobs["managers"]
        self.assertEqual(job["needs"], ["meta", "assemble"])
        legs = {leg["platform"]: leg["runner"] for leg in job["strategy"]["matrix"]["include"]}
        wheels = {leg["platform"]: leg["runner"] for leg in self.jobs["wheels"]["strategy"]["matrix"]["include"]}
        self.assertEqual(sorted(legs), ["linux-x64", "macos-arm64", "macos-x64", "windows-x64"])
        self.assertEqual(legs, {p: wheels[p] for p in legs})
        downloads = [(s["with"]["name"], s["with"]["path"]) for s in self.using(job, "actions/download-artifact")]
        self.assertEqual(downloads, [("pkg-release-assets", "release"), ("pkg-release-preview", "preview")])
        self.assertRegex(job["env"]["SCOOP_INSTALL_COMMIT"], r"^[0-9a-f]{40}$")
        self.assertRegex(job["env"]["SCOOP_INSTALL_SHA256"], r"^[0-9a-f]{64}$")
        steps = {s.get("name"): s for s in job["steps"] if s.get("name")}
        scoop = steps["Scoop, from its pinned installer"]
        self.assertEqual(scoop["if"], "matrix.platform == 'windows-x64'")
        run = scoop["run"]
        self.assertIn("ScoopInstaller/Install/$env:SCOOP_INSTALL_COMMIT/install.ps1", run)
        self.assertLess(run.index("Get-FileHash -Algorithm SHA256"), run.index("& $installer -RunAsAdmin"),
                        "the installer runs only once its hash is checked")
        windows = {name: s for name, s in steps.items() if s.get("if") == "matrix.platform == 'windows-x64'"}
        others = {name: s for name, s in steps.items() if s.get("if") == "matrix.platform != 'windows-x64'"}
        runs = "\n".join(s["run"] for s in windows.values())
        for words in ("check_managers.py scoop --release release", "check_managers.py winget-schemas --dir "
                      "preview/managers", "check_managers.py winget --release release --manifests preview/managers"):
            self.assertIn(words, runs)
        self.assertEqual(len(others), 1)
        self.assertIn("check_managers.py brew --release release", next(iter(others.values()))["run"])
        for step in (*windows.values(), *others.values()):
            if "check_managers.py" in step["run"] and "winget-schemas" not in step["run"]:
                self.assertIn('--label "$LABEL"', step["run"])
        self.assertEqual(self.uploads_of("managers"), {f"pkg-managers-report-{p}" for p in legs})

    def test_the_rules_hold_the_real_file(self):
        plants = (
            ("    runs-on: ${{ matrix.runner }}\n    timeout-minutes: 90\n",
             "    runs-on: ${{ matrix.runner }}\n    timeout-minutes: 90\n    environment: staging\n", 3, "wheels"),
            ('python scripts/ci/check_dist.py sdist dist --version "$PEP440"',
             "python scripts/ci/check_dist.py sdist dist --version ${{ inputs.version }}", 5, "sdist"),
            ("name: pkg-sdist\n          path: dist/*.tar.gz", "name: sdist\n          path: dist/*.tar.gz", 13,
             "sdist"),
            ("          name: pkg-wheel-${{ matrix.platform }}\n          path: wheelhouse",
             "          pattern: helper-*\n          path: wheelhouse", 13, "image"),
            ("          path: constraints/*.txt", "          path: ${{ runner.temp }}", 9, "constraints"),
            ("container: quay.io/pypa/manylinux_2_28_x86_64@sha256:",
             "container: quay.io/pypa/manylinux_2_28_x86_64:latest #", 11, "wheels"),
            # 5-03: the helpers' builds and the manifest's signing
            ('container: "quay.io/pypa/manylinux_2_28_aarch64@sha256:b873',
             'container: "quay.io/pypa/manylinux_2_28_aarch64:latest", x: "', 11, "helpers"),
            ("FEDORA_IMAGE: docker.io/library/fedora@sha256:be9d", "FEDORA_IMAGE: docker.io/library/fedora:44 #", 11,
             "helpers"),
            ("            dry-run/dry-run-signing.pub\n",
             "            ${{ runner.temp }}/throwaway/citar-update-signing.pub\n", 9, "helper-manifest"),
            ("          path: manifest/citar-helper-manifest.json",
             "          path: ${{ runner.temp }}/throwaway/*", 9, "helper-manifest"),
            ("    runs-on: ubuntu-24.04\n    timeout-minutes: 30\n",
             "    runs-on: ubuntu-24.04\n    timeout-minutes: 30\n    environment: helper-signing\n", 3,
             "helper-manifest"),
            ("          name: pkg-helper-manifest\n", "          name: helper-manifest\n", 13, "helper-manifest"),
            # 5-04: the notices
            ("          name: pkg-third-party-licenses\n", "          name: third-party-licenses\n", 13, "notices"),
            ("    name: third-party notices\n    needs: meta\n",
             "    name: third-party notices\n    needs: meta\n    environment: notices\n", 3, "notices"),
            # 5-06: the image is built and kept, never pushed
            ('            --tag "$TAG" .\n', '            --tag "$TAG" --push .\n', 3, "image"),
            ('          docker image ls "$TAG"\n', '          docker push "$TAG"\n', 3, "image"),
            ("          name: pkg-image-${{ matrix.arch }}\n", "          name: image-${{ matrix.arch }}\n", 13,
             "image"),
            ("          path: report/image-${{ matrix.arch }}.json", "          path: ${{ runner.temp }}", 9, "image"),
            # 5-07: the install scripts' checks publish nothing and name their artifacts
            ("          name: pkg-install-report-${{ matrix.platform }}\n",
             "          name: install-report-${{ matrix.platform }}\n", 13, "install"),
            ("          path: report/install-${{ matrix.platform }}.json", "          path: ${{ runner.temp }}", 9,
             "install"),
            ("    name: install scripts (${{ matrix.platform }})\n",
             "    name: install scripts (${{ matrix.platform }})\n    environment: release\n", 3, "install"),
            ('            --path-route --out "report/install-$PLATFORM.json"\n',
             '            --path-route --out "report/install-$PLATFORM.json" --version ${{ inputs.version }}\n', 5,
             "install"),
            # 5-08: the release assembled, and the package managers, publish nothing and name their artifacts
            ("          name: pkg-release-assets\n", "          name: release-assets\n", 13, "assemble"),
            ("          name: pkg-install-report-linux-arm64\n          path: reports\n",
             "          pattern: pkg-install-report-*\n          path: reports\n", 13, "assemble"),
            ("          name: pkg-release-preview\n          path: preview\n          if-no-files-found",
             "          name: pkg-release-preview\n          path: ${{ runner.temp }}\n          if-no-files-found",
             9, "assemble"),
            ("    name: the release, assembled\n", "    name: the release, assembled\n    environment: pypi\n", 3,
             "assemble"),
            ('          python -m twine check "release/citar-$PEP440.tar.gz"',
             '          python -m twine upload "release/citar-$PEP440.tar.gz"', 2, "assemble"),
            ("    name: package managers (${{ matrix.platform }})\n",
             "    name: package managers (${{ matrix.platform }})\n    environment: release\n", 3, "managers"),
            ("          name: pkg-managers-report-${{ matrix.platform }}\n",
             "          name: managers-report-${{ matrix.platform }}\n", 13, "managers"),
            ('            --work "$RUNNER_TEMP/brew" --out report/managers-brew.json\n',
             '            --work "$RUNNER_TEMP/brew" --out report/managers-brew.json\n          brew tap-new x/y && '
             'cd "$(brew --repository x/y)" && git push\n', 2, "managers"),
        )
        for old, new, rule, job in plants:
            with self.subTest(rule=rule, job=job):
                tree = Tree(self)
                tree.replace("package.yml", old, new)
                found = tree.findings()
                self.assertTrue([f for f in found if f.rule == rule and f.job == job and f.file == "package.yml"],
                                "\n".join(map(str, found)))
        tree = Tree(self)
        tree.replace("helper.yml", 'container: "quay.io/pypa/manylinux_2_28_x86_64@sha256:1e7b',
                     'container: "quay.io/pypa/manylinux_2_28_x86_64:2026.10.16-1", x: "')
        found = tree.findings()
        self.assertTrue([f for f in found if f.rule == 11 and f.job == "artifact" and f.file == "helper.yml"],
                        "\n".join(map(str, found)))


def helper_release_texts() -> dict:
    """scripts/ci/helper_release.py's ARCHIVE_TEXTS (each notice's name in a packaged archive, and its file), read
    from the source without importing it."""
    import ast
    tree = ast.parse((ROOT / "scripts" / "ci" / "helper_release.py").read_text(encoding="utf-8"))
    for node in tree.body:
        if isinstance(node, ast.Assign) and [getattr(t, "id", None) for t in node.targets] == ["ARCHIVE_TEXTS"]:
            return ast.literal_eval(node.value)
    raise AssertionError("helper_release.py has no ARCHIVE_TEXTS")


def load_release():
    spec = importlib.util.spec_from_file_location("citar_release_for_workflows", ROOT / "scripts" / "release.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class Planted(unittest.TestCase):
    """Each fixture fails naming its file, job and rule."""

    def assertFinds(self, tree, rule, file, job=None, text=None):
        found = tree.findings()
        hits = [f for f in found if f.rule == rule and f.file == file and (job is None or f.job == job)
                and (text is None or text in f.message)]
        self.assertTrue(hits, f"no rule {rule} finding for {file} job {job}; found:\n" + "\n".join(map(str, found)))
        for f in hits:
            self.assertIn(f"rule {rule}:", str(f))
            if job:
                self.assertIn(f"job {job}:", str(f))
        return found

    def assertPasses(self, tree, besides=()):
        """No finding (``besides``: rules set aside, e.g. 16 where a test adds a job P5.11's table does not have, which
        rule 16 names: its own plants test that)."""
        found = [f for f in tree.findings() if f.rule not in besides]
        self.assertEqual(found, [], "\n".join(map(str, found)))

    # ---- rule 1
    def test_rule_1_a_workflow_without_contents_read_at_the_top(self):
        tree = Tree(self)
        tree.replace("rust.yml", "permissions:\n  contents: read\n", "permissions: write-all\n")
        self.assertFinds(tree, 1, "rust.yml")

    def test_rule_1_a_workflow_that_does_not_parse(self):
        tree = Tree(self)
        tree.write("broken.yml", "name: broken\non: [push\njobs: {}\n")
        self.assertFinds(tree, 1, "broken.yml", text="does not parse")

    # ---- rule 2
    def test_rule_2_a_publishing_step_without_the_guard(self):
        tree = Tree(self)
        tree.add_job("release.yml", "publish", f"""
runs-on: ubuntu-latest
permissions:
  contents: write
steps:
  - uses: softprops/action-gh-release@{SHA} # v3.0.0
""")
        self.assertFinds(tree, 2, "release.yml", "publish", "has no if:")

    def test_rule_2_the_guard_without_the_event_check(self):
        tree = Tree(self)
        tree.add_job("release.yml", "publish", """
if: startsWith(github.ref, 'refs/tags/v')
runs-on: ubuntu-latest
steps:
  - run: gh release create "$TAG" release/*
""")
        self.assertFinds(tree, 2, "release.yml", "publish", "github.event_name == 'push'")

    def test_rule_2_a_guard_under_always_or_an_or(self):
        for condition in (f"always() && {TAG_GUARD}", f"${{{{ !cancelled() && {TAG_GUARD} }}}}",
                          f"{TAG_GUARD} || github.event_name == 'workflow_dispatch'",
                          f"failure() && {TAG_GUARD}"):
            with self.subTest(condition=condition):
                tree = Tree(self)
                tree.add_job("release.yml", "publish", f"""
if: {condition}
runs-on: ubuntu-latest
permissions:
  id-token: write
steps:
  - uses: pypa/gh-action-pypi-publish@{SHA} # v1.13.0
""")
                self.assertFinds(tree, 2, "release.yml", "publish")

    def test_rule_2_the_guarded_job_passes(self):
        for condition in (TAG_GUARD, f"${{{{ {TAG_GUARD} }}}}", f"needs.meta.result == 'success' && {TAG_GUARD}",
                          "startsWith(github.ref, 'refs/tags/v') && github.event_name == 'push'"):
            with self.subTest(condition=condition):
                tree = Tree(self)
                tree.add_job("release.yml", "publish", f"""
if: {condition}
runs-on: ubuntu-latest
permissions:
  contents: write
steps:
  - uses: {CHECKOUT}
    with:
      persist-credentials: false
  - run: gh release create "$TAG" --draft release/*
""")
                self.assertPasses(tree, besides=(16,))

    def test_rule_2_docs_ymls_wiki_step_without_its_main_guard(self):
        tree = Tree(self)
        tree.replace("docs.yml", "        if: github.ref == 'refs/heads/main'\n", "")
        self.assertFinds(tree, 2, "docs.yml", "wiki", "refs/heads/main")

    def test_rule_2_docs_ymls_deploy_without_its_main_guard(self):
        tree = Tree(self)
        tree.replace("docs.yml", "    if: github.ref == 'refs/heads/main'\n", "    if: always()\n")
        self.assertFinds(tree, 2, "docs.yml", "deploy")

    def test_rule_2_and_3_a_heredoc_a_shell_reads_in_test_yml(self):
        for script in ("bash <<'EOF'\n          gh release create v1 dist/*\n          EOF",
                       "cat <<EOF | bash\n          gh release create v1 dist/*\n          EOF"):
            with self.subTest(script=script):
                tree = Tree(self)
                tree.replace("test.yml", "      - run: ruff check --output-format=github .\n",
                             f"      - run: ruff check --output-format=github .\n      - run: |\n"
                             f"          {script}\n")
                found = self.assertFinds(tree, 3, "test.yml", "lint", "gh release create")
                self.assertTrue(any(f.rule == 2 and f.job == "lint" for f in found), found)
        tree = Tree(self)  # a heredoc no shell reads is data
        tree.replace("test.yml", "      - run: ruff check --output-format=github .\n",
                     "      - run: ruff check --output-format=github .\n      - run: |\n"
                     "          cat > notes.txt <<'EOF'\n          gh release create v1 dist/*\n          EOF\n")
        self.assertPasses(tree)

    def test_rule_2_no_other_workflow_publishes(self):
        for name, plant in (("rust.yml", "      - run: git push origin HEAD:refs/heads/x\n"),
                            ("helper.yml", "      - run: gh api -X POST repos/x/y/dispatches\n"),
                            ("nightly.yml", "      - run: docker push ghcr.io/x/y:z\n")):
            with self.subTest(name=name):
                tree = Tree(self)
                text = tree.read(name)
                at = text.index("    steps:\n") + len("    steps:\n")
                tree.write(name, text[:at] + plant + text[at:])
                self.assertFinds(tree, 2, name)

    # ---- rule 3
    def test_rule_3_an_environment_named_in_test_yml(self):
        tree = Tree(self)
        tree.replace("test.yml", "  lint:\n    runs-on: ubuntu-latest\n",
                     "  lint:\n    runs-on: ubuntu-latest\n    environment: staging\n")
        found = self.assertFinds(tree, 3, "test.yml", "lint")
        self.assertTrue(any(f.rule == 14 and f.job == "lint" for f in found), found)

    def test_rule_3_a_secret_named_in_test_yml_even_github_token(self):
        for token in ("secrets.GITHUB_TOKEN", "github.token", "github['token']"):
            with self.subTest(token=token):
                tree = Tree(self)
                tree.replace("test.yml", "      - run: ruff check --output-format=github .\n",
                             "      - run: ruff check --output-format=github .\n        env:\n"
                             f"          GH_TOKEN: ${{{{ {token} }}}}\n")
                self.assertFinds(tree, 3, "test.yml", text="GITHUB_TOKEN")

    def test_rule_3_package_yml_publishes_nothing(self):
        tree = Tree(self, package=True)
        tree.replace("package.yml", "      - run: ls -R one all\n", "      - run: twine upload all/*\n")
        self.assertFinds(tree, 3, "package.yml", "assemble", "twine upload")

    # ---- rule 4
    def test_rule_4_a_called_workflows_group_without_github_workflow(self):
        tree = Tree(self)
        tree.replace("test.yml", "group: tests-${{ github.workflow }}-${{ github.ref }}",
                     "group: tests-${{ github.ref }}")
        self.assertFinds(tree, 4, "test.yml", text="github.workflow")

    def test_rule_4_a_caller_whose_group_is_the_called_ones(self):
        tree = Tree(self)
        tree.replace("release.yml", "  group: release-${{ github.ref }}\n", "  group: tests-release-${{ github.ref }}\n")
        self.assertFinds(tree, 4, "release.yml", "tests", "wait on itself")

    # ---- rule 5
    def test_rule_5_inputs_in_a_run(self):
        tree = Tree(self)
        tree.replace("release.yml", META_END, META_END + "      - run: echo ${{ inputs.version }}\n")
        self.assertFinds(tree, 5, "release.yml", "meta", "inputs.version")

    def test_rule_5_the_head_ref_and_the_event_inputs(self):
        for expr in ("github.head_ref", "github.event.inputs.version", "github.event.pull_request.title"):
            with self.subTest(expr=expr):
                tree = Tree(self)
                tree.replace("rust.yml", "run: cargo fmt --all --check", f"run: echo \"${{{{ {expr} }}}}\"")
                self.assertFinds(tree, 5, "rust.yml", "lint")

    # ---- rule 6
    def test_rule_6_a_maturin_step_without_the_pin(self):
        for pin in ("maturin-version: v1.15.0", "maturin-version: latest"):
            with self.subTest(pin=pin):
                tree = Tree(self)
                tree.replace("test.yml", "maturin-version: ${{ env.MATURIN_VERSION }}", pin)
                self.assertFinds(tree, 6, "test.yml", "build-ext")
        tree = Tree(self)
        tree.replace("test.yml", "          maturin-version: ${{ env.MATURIN_VERSION }}\n", "")
        self.assertFinds(tree, 6, "test.yml", "build-ext")
        tree = Tree(self)
        tree.replace("test.yml", "  MATURIN_VERSION: v1.15.0\n", "")
        self.assertFinds(tree, 6, "test.yml", "build-ext", "does not define")
        tree = Tree(self)
        tree.replace("package.yml", "  MATURIN_VERSION: v1.15.0\n", "")
        for job in ("wheels", "sdist"):
            self.assertFinds(tree, 6, "package.yml", job, "does not define")

    # ---- rule 7
    def test_rule_7_a_build_before_prepare(self):
        tree = Tree(self)
        text = tree.read("test.yml")
        at = text.index("      - name: The version this run tests\n")
        tree.write("test.yml", text[:at] + "      - run: cargo build --locked\n\n" + text[at:])
        self.assertFinds(tree, 7, "test.yml", "build-ext", "runs before scripts/release.py prepare")

    def test_rule_7_a_job_without_prepare_and_a_conditional_one(self):
        tree = Tree(self, package=True)
        tree.replace("package.yml", "      - run: python3 scripts/release.py prepare --mode \"$RELEASE_MODE\"\n", "")
        self.assertFinds(tree, 7, "package.yml", "assemble", "runs no scripts/release.py prepare")
        tree = Tree(self, package=True)
        tree.replace("package.yml", "      - name: prepare\n", "      - name: prepare\n        if: false\n")
        self.assertFinds(tree, 7, "package.yml", "wheels", "conditional")

    # ---- rule 8
    def test_rule_8_a_build_step_in_release_yml(self):
        for run in ("cargo build --release --locked -p citar-helper", "python -m PyInstaller installer/citar.spec",
                    "docker build -t x .", "python -m build"):
            with self.subTest(run=run):
                tree = Tree(self)
                tree.replace("release.yml", META_END, META_END + f"      - run: {run}\n")
                self.assertFinds(tree, 8, "release.yml", "meta")

    def test_rule_8_a_build_in_a_heredoc_a_shell_reads(self):
        for script in ("bash -s <<EOF\n          cargo build --release\n          EOF",
                       "sh <<< 'cargo build --release'", "pwsh -Command \"cargo build --release\""):
            with self.subTest(script=script):
                tree = Tree(self)
                tree.replace("release.yml", META_END, META_END + f"      - run: |\n          {script}\n")
                self.assertFinds(tree, 8, "release.yml", "meta", "runs cargo")

    def test_rule_8_and_15_a_step_in_a_shell_this_check_cannot_read(self):
        plant = "      - shell: python\n        run: |\n          import subprocess\n" \
                "          subprocess.run(['cargo', 'build'])\n"
        tree = Tree(self)
        tree.replace("release.yml", META_END, META_END + plant)
        self.assertFinds(tree, 8, "release.yml", "meta", "'python'")
        tree = Tree(self)  # the job's defaults count too, for a step that names no shell
        tree.replace_in_job("release.yml", "meta", "    runs-on: ubuntu-24.04\n",
                            "    runs-on: ubuntu-24.04\n    defaults:\n      run:\n        shell: perl {0}\n")
        tree.replace("release.yml", META_END, META_END + "      - run: print 'done'\n")
        self.assertFinds(tree, 8, "release.yml", "meta", "'perl'")
        tree = Tree(self)
        tree.add_job("release.yml", "publish-pypi", f"""
if: {TAG_GUARD}
runs-on: ubuntu-latest
environment: pypi
permissions:
  id-token: write
steps:
  - shell: python
    run: |
      import subprocess
      subprocess.run(['pip', 'install', 'twine'])
  - uses: pypa/gh-action-pypi-publish@{SHA} # v1.13.0
""")
        found = self.assertFinds(tree, 15, "release.yml", "publish-pypi", "cannot be read")
        self.assertTrue(any(f.rule == 8 and f.job == "publish-pypi" for f in found), found)
        tree = Tree(self)  # a step reading a secret, anywhere
        tree.replace("docs.yml", "        env:\n          WIKI_TOKEN:",
                     "        shell: python {0}\n        env:\n          WIKI_TOKEN:")
        self.assertFinds(tree, 15, "docs.yml", "wiki", "'python'")
        for shell in ("bash --noprofile --norc -eo pipefail {0}", "pwsh", "sh", "cmd", "powershell"):
            with self.subTest(shell=shell):
                tree = Tree(self)
                tree.replace("release.yml", META_END, META_END + f"      - shell: {shell}\n        run: echo done\n")
                self.assertPasses(tree)

    # ---- rule 9
    def test_rule_9_an_upload_of_a_key(self):
        for path in ("wheels/*.key", "wheels/update-signing.key", "${{ runner.temp }}/throwaway/citar.pub",
                     "${{ runner.temp }}", "wheels/*"):
            with self.subTest(path=path):
                tree = Tree(self)
                tree.replace("test.yml", "path: wheels/*.whl", f"path: {path}")
                self.assertFinds(tree, 9, "test.yml", "build-ext")

    # ---- rule 10
    def test_rule_10_build_wiki_push_in_a_workflow(self):
        tree = Tree(self)
        tree.replace("docs.yml", "run: python scripts/build_wiki.py\n", "run: python scripts/build_wiki.py --push\n")
        self.assertFinds(tree, 10, "docs.yml", "wiki", "build_wiki.py --push")

    def test_rule_10_release_yml_calling_nightly_or_docs(self):
        tree = Tree(self)
        tree.add_job("release.yml", "nightly", """
uses: ./.github/workflows/nightly.yml
permissions:
  contents: read
""")
        self.assertFinds(tree, 10, "release.yml", "nightly")
        tree = Tree(self)
        tree.replace("release.yml", META_END, META_END + "      - run: gh workflow run docs.yml\n")
        self.assertFinds(tree, 10, "release.yml", "meta")

    # ---- rule 11
    def test_rule_11_an_action_referenced_by_tag(self):
        tree = Tree(self)
        tree.replace("test.yml", f"uses: {CHECKOUT}", "uses: actions/checkout@v7")
        self.assertFinds(tree, 11, "test.yml", "build-ext", "actions/checkout@v7")

    def test_rule_11_a_pin_without_its_version_and_one_out_of_reach(self):
        tree = Tree(self)
        tree.replace("test.yml", f"uses: {CHECKOUT}", "uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1")
        self.assertFinds(tree, 11, "test.yml", "build-ext", "no version")
        tree = Tree(self)
        tree.add_job("rust.yml", "flow", f"""
runs-on: ubuntu-latest
steps: [{{uses: "actions/checkout@{SHA}"}}]
""")
        self.assertFinds(tree, 11, "rust.yml", "flow", "cannot be read")
        tree = Tree(self)
        tree.add_job("rust.yml", "image", """
runs-on: ubuntu-latest
steps:
  - uses: docker://alpine:3.20
""")
        self.assertFinds(tree, 11, "rust.yml", "image", "sha256")

    def test_rule_11_the_image_a_maturin_step_builds_in(self):
        digest = "quay.io/pypa/manylinux_2_28_x86_64@sha256:" + "1e" * 32

        def workflow(linux: dict, step: str = "") -> str:
            leg = ", ".join(f'{k}: "{v}"' for k, v in linux.items())
            return f"""name: images
on: pull_request
permissions:
  contents: read
env:
  MATURIN_VERSION: v1.15.0
jobs:
  wheel:
    runs-on: ${{{{ matrix.runner }}}}
    strategy:
      matrix:
        include:
          - {{ platform: linux, runner: ubuntu-24.04, {leg} }}
          # maturin-action builds on the runner on Windows and macOS, whatever these say
          - {{ platform: windows, runner: windows-latest, manylinux: "2_28", container: "" }}
          - {{ platform: macos, runner: macos-15, manylinux: "2_28", container: auto }}
    steps:
      - uses: PyO3/maturin-action@e83996d129638aa358a18fbd1dfb82f0b0fb5d3b # v1.51.0
        with:
          maturin-version: ${{{{ env.MATURIN_VERSION }}}}
          manylinux: ${{{{ matrix.manylinux }}}}
          container: ${{{{ matrix.container }}}}
          target: ${{{{ matrix.target }}}}
{step}"""
        cases = (  # the Linux leg's matrix values, and what rule 11 says of them (None: it holds)
            ({"manylinux": "2_28", "container": digest}, None),
            ({"manylinux": "2_28", "container": "off"}, None),
            ({"manylinux": "off", "container": ""}, None),
            ({"manylinux": "", "container": "", "target": "x86_64-unknown-linux-gnu"}, None),
            ({"manylinux": "2_28", "container": "quay.io/pypa/manylinux_2_28_x86_64:latest"}, "is not pinned"),
            ({"manylinux": "2_28", "container": "quay.io/pypa/manylinux_2_28_x86_64@sha256:abc"}, "is not pinned"),
            ({"manylinux": "2_28", "container": ""}, "own image for manylinux 2_28"),
            ({"manylinux": "manylinux_2_28"}, "own image for manylinux 2_28"),       # no container key: empty
            ({"manylinux": "off", "container": "auto"}, "own image"),
            ({"manylinux": "", "container": "", "target": "aarch64-unknown-linux-gnu"}, "own image for manylinux auto"),
        )
        for leg, problem in cases:
            with self.subTest(leg=leg):
                tree = Tree(self)
                tree.write("images.yml", workflow(leg))
                if problem is None:
                    self.assertPasses(tree)
                    continue
                found = self.assertFinds(tree, 11, "images.yml", "wheel", problem)
                self.assertEqual([f for f in found if f.file == "images.yml"],
                                 [f for f in found if f.file == "images.yml" and "(leg linux)" in f.message])
        # the source distribution and a publish: sdist never builds in a container, publish asks for one
        tree = Tree(self)
        text = workflow({"manylinux": "", "container": ""}).replace("          target:", "          command: sdist\n"
                                                                                       "          target:")
        tree.write("images.yml", text)
        self.assertPasses(tree)
        tree.write("images.yml", text.replace("command: sdist", "command: publish"))
        self.assertFinds(tree, 11, "images.yml", "wheel", "own image for manylinux auto")
        # a target given in args is read as the action reads it
        tree.write("images.yml", workflow({"manylinux": "", "container": ""}).replace(
            "          target: ${{ matrix.target }}\n", "          args: --release --target=aarch64-unknown-linux-gnu\n"))
        self.assertFinds(tree, 11, "images.yml", "wheel", "own image for manylinux auto")

    def test_rule_11_a_jobs_container_and_services(self):
        digest = "@sha256:" + "ab" * 32
        body = """
runs-on: ubuntu-24.04
container: {container}
services:
  db:
    image: {db}
  cache: {cache}
steps:
  - run: echo ok
"""
        tree = Tree(self)
        tree.add_job("rust.yml", "inside", body.format(container="node:22", db="postgres:17",
                                                       cache="{ image: 'redis:8' }"))
        found = self.assertFinds(tree, 11, "rust.yml", "inside", "its container node:22")
        self.assertFinds(tree, 11, "rust.yml", "inside", "its service db postgres:17")
        self.assertFinds(tree, 11, "rust.yml", "inside", "its service cache redis:8")
        self.assertEqual(len([f for f in found if f.job == "inside"]), 3)
        tree = Tree(self)
        tree.add_job("rust.yml", "inside", body.format(container="{ image: 'node" + digest + "' }",
                                                       db="postgres" + digest, cache="{ image: 'redis" + digest + "' }"))
        self.assertPasses(tree)

    def test_rule_11_an_image_a_script_runs_named_by_key(self):
        # A script's `docker run "$IMAGE"` is not read; the image is written where rule 11 reads it (5-03).
        digest = "@sha256:" + "cd" * 32
        body = """
runs-on: ${{{{ matrix.runner }}}}
env:
  BASE_IMAGE: {job_env}
strategy:
  matrix:
    include:
      - {{ runner: ubuntu-24.04, container: "{leg}", tls_image: "{other}" }}
      - {{ runner: windows-latest, container: "", tls_image: "" }}
steps:
  - env:
      IMAGE: ${{{{ matrix.container }}}}
      FEDORA_IMAGE: {step_env}
    run: docker run --rm "$IMAGE" true && docker run --rm "$FEDORA_IMAGE" true
"""
        pinned = {"job_env": "debian" + digest, "leg": "quay.io/pypa/manylinux_2_28_x86_64" + digest,
                  "other": "fedora" + digest, "step_env": "quay.io/fedora/fedora" + digest}
        tree = Tree(self)
        tree.add_job("rust.yml", "scripted", body.format(**pinned))
        self.assertPasses(tree)
        for key, moving, where in (("job_env", "debian:13", "its env BASE_IMAGE"),
                                   ("leg", "quay.io/pypa/manylinux_2_28_x86_64:latest", "its matrix's container"),
                                   ("other", "fedora:44", "its matrix's tls_image"),
                                   ("step_env", "quay.io/fedora/fedora:44", "env FEDORA_IMAGE")):
            with self.subTest(key=key):
                tree = Tree(self)
                tree.add_job("rust.yml", "scripted", body.format(**{**pinned, key: moving}))
                found = self.assertFinds(tree, 11, "rust.yml", "scripted", where)
                self.assertEqual(len([f for f in found if f.job == "scripted"]), 1, "\n".join(map(str, found)))

    # ---- rule 12
    def test_rule_12_secrets_inherit_on_a_calling_job(self):
        for job in ("tests", "package"):
            with self.subTest(job=job):
                tree = Tree(self)
                tree.replace_in_job("release.yml", job, "    permissions:\n      contents: read\n",
                                    "    permissions:\n      contents: read\n    secrets: inherit\n")
                self.assertFinds(tree, 12, "release.yml", job, "inherit")

    def test_rule_12_a_calling_job_without_contents_read(self):
        tree = Tree(self)
        tree.replace_in_job("release.yml", "tests", "    permissions:\n      contents: read\n", "")
        self.assertFinds(tree, 12, "release.yml", "tests", "contents: read")
        tree = Tree(self)
        tree.replace_in_job("release.yml", "package", "      contents: read\n", "      contents: read\n      packages: write\n")
        self.assertFinds(tree, 12, "release.yml", "package", "contents: read")
        self.assertPasses(Tree(self))  # the real calling jobs: contents: read, inputs through with:, no secrets

    # ---- rule 13
    def test_rule_13_an_unnamed_and_a_patterned_download_in_package_yml(self):
        tree = Tree(self, package=True)
        tree.replace("package.yml", "          pattern: pkg-*\n", "")
        self.assertFinds(tree, 13, "package.yml", "assemble", "every artifact")
        tree = Tree(self, package=True)
        tree.replace("package.yml", "pattern: pkg-*", "pattern: helper-*")
        self.assertFinds(tree, 13, "package.yml", "assemble", "helper-*")

    def test_rule_13_an_upload_named_pkg_x_in_test_yml(self):
        tree = Tree(self)
        tree.replace("test.yml", "name: ext-${{ runner.os }}", "name: pkg-x")
        self.assertFinds(tree, 13, "test.yml", "build-ext", "pkg-x")

    def test_rule_13_package_ymls_own_prefix_and_one_name_per_leg(self):
        tree = Tree(self, package=True)
        tree.replace("package.yml", "name: pkg-wheel-${{ matrix.platform }}", "name: wheel-${{ matrix.platform }}")
        self.assertFinds(tree, 13, "package.yml", "wheels", "pkg-")
        tree = Tree(self, package=True)
        tree.replace("package.yml", "name: pkg-wheel-${{ matrix.platform }}", "name: pkg-wheel")
        self.assertFinds(tree, 13, "package.yml", "wheels", "more than once")
        tree = Tree(self, package=True)
        tree.replace("package.yml", "name: pkg-wheel-${{ matrix.platform }}", "name: pkg-wheel-${{ runner.os }}")
        self.assertPasses(tree)

    def test_rule_13_one_namespace_across_the_three(self):
        tree = Tree(self, package=True)
        tree.replace("package.yml", "name: pkg-wheel-${{ matrix.platform }}", "name: ext-${{ runner.os }}")
        found = self.assertFinds(tree, 13, "package.yml", "wheels", "pkg-")
        self.assertTrue(any("'ext-Linux' is uploaded more than once" in f.message for f in found), found)

    # ---- rule 14
    def test_rule_14_a_literal_helper_signing_in_release_yml(self):
        tree = Tree(self)
        tree.add_job("release.yml", "sign", f"""
if: {TAG_GUARD}
runs-on: ubuntu-latest
environment: helper-signing
steps:
  - run: echo signing
""")
        found = self.assertFinds(tree, 14, "release.yml", "sign", "helper-signing")
        self.assertFalse([f for f in found if f.rule == 2], found)

    def test_rule_14_the_tag_expression_passes_and_a_weakened_one_does_not(self):
        tree = Tree(self)
        tree.add_job("release.yml", "sign", f"""
if: {TAG_GUARD}
runs-on: ubuntu-latest
environment: {TAG_ENV}
steps:
  - run: echo signing
""")
        self.assertPasses(tree, besides=(16,))
        for env in ("${{ startsWith(github.ref, 'refs/tags/v') && 'helper-signing' || '' }}",
                    "${{ github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v') && 'helper-signing' }}",
                    "${{ github.event_name == 'push' && 'helper-signing' || '' }}",
                    TAG_ENV.replace("'helper-signing'", "'staging'"),
                    TAG_ENV.replace("'helper-signing'", "'helper_signing'")):
            with self.subTest(env=env):
                tree = Tree(self)
                tree.add_job("release.yml", "sign", f"""
if: {TAG_GUARD}
runs-on: ubuntu-latest
environment: "{env}"
steps:
  - run: echo signing
""")
                self.assertFinds(tree, 14, "release.yml", "sign")

    def test_rule_14_pypi_and_github_pages_only_in_their_jobs(self):
        tree = Tree(self)
        tree.add_job("release.yml", "upload", f"""
if: {TAG_GUARD}
runs-on: ubuntu-latest
environment: pypi
steps:
  - run: echo upload
""")
        self.assertFinds(tree, 14, "release.yml", "upload", "pypi")
        tree = Tree(self)
        tree.replace("docs.yml", "      name: github-pages\n", "      name: production\n")
        self.assertFinds(tree, 14, "docs.yml", "deploy", "production")
        tree = Tree(self)
        tree.add_job("rust.yml", "pages", """
runs-on: ubuntu-latest
environment: github-pages
steps:
  - run: echo pages
""")
        self.assertFinds(tree, 14, "rust.yml", "pages")

    # ---- rule 15
    def test_rule_15_cargo_build_in_a_step_that_reads_a_secret(self):
        tree = Tree(self)
        tree.replace("docs.yml", "          git clone ", "          cargo build --release\n          git clone ")
        self.assertFinds(tree, 15, "docs.yml", "wiki", "runs cargo")

    def test_rule_15_pip_install_in_a_release_job_holding_id_token(self):
        tree = Tree(self)
        tree.add_job("release.yml", "publish-pypi", f"""
if: {TAG_GUARD}
runs-on: ubuntu-latest
environment: pypi
permissions:
  id-token: write
steps:
  - run: pip install twine
  - uses: pypa/gh-action-pypi-publish@{SHA} # v1.13.0
""")
        found = self.assertFinds(tree, 15, "release.yml", "publish-pypi", "runs pip")
        self.assertEqual({f.rule for f in found} - {16}, {15}, found)  # 16: the planted job replaces P5.11's

    def test_rule_15_pip_by_another_name_in_a_release_job(self):
        for run in ("pip3.12 install twine", "pip3 install twine", "sudo -E pip install twine",
                    "sudo -u runner python3.12 -m pip install twine", "bash -s <<'EOF'\n      pip install twine\n      EOF"):
            with self.subTest(run=run):
                tree = Tree(self)
                tree.add_job("release.yml", "publish-pypi", f"""
if: {TAG_GUARD}
runs-on: ubuntu-latest
environment: pypi
permissions:
  id-token: write
steps:
  - run: |
      {run}
  - uses: pypa/gh-action-pypi-publish@{SHA} # v1.13.0
""")
                self.assertFinds(tree, 15, "release.yml", "publish-pypi", "runs pip")

    def test_rule_15_setup_actions_credentials_and_secrets_in_actions(self):
        tree = Tree(self)
        tree.add_job("release.yml", "image", f"""
if: {TAG_GUARD}
runs-on: ubuntu-latest
permissions:
  packages: write
steps:
  - uses: {CHECKOUT}
  - uses: actions/setup-python@5fda3b95a4ea91299a34e894583c3862153e4b97 # v7.0.0
  - uses: docker/login-action@{SHA} # v4.0.0
    with:
      password: ${{{{ secrets.REGISTRY_TOKEN }}}}
""")
        found = self.assertFinds(tree, 15, "release.yml", "image")
        messages = " | ".join(f.message for f in found if f.rule == 15)
        for what in ("persist-credentials: false", "actions/setup-python", "in an action's step"):
            self.assertIn(what, messages)


class TheReleaseWorkflow(unittest.TestCase):
    """release.yml as P5.11 writes it (package 5-09), beyond what rule 16 holds: what each job runs."""

    @classmethod
    def setUpClass(cls):
        import yaml
        cls.data = yaml.safe_load((WORKFLOWS / "release.yml").read_text(encoding="utf-8"))
        cls.jobs = cls.data["jobs"]
        cls.wf = cw.Workflow("release.yml", WORKFLOWS / "release.yml",
                             (WORKFLOWS / "release.yml").read_text(encoding="utf-8"), cls.data)

    def uses(self, job_id):
        return [cw.action_of(s.get("uses")) for s in self.jobs[job_id]["steps"] if s.get("uses")]

    def runs(self, job_id) -> str:
        return "\n".join(s.get("run", "") for s in self.jobs[job_id]["steps"])

    def downloads(self, job_id) -> dict:
        return {s["with"]["name"]: s["with"]["path"] for s in self.jobs[job_id]["steps"]
                if cw.action_of(s.get("uses")) == "actions/download-artifact"}

    def test_its_triggers_and_meta(self):
        on = self.data[True]
        self.assertEqual(on["push"], {"tags": ["v*"]})
        self.assertEqual(list(on["workflow_dispatch"]["inputs"]), ["version"])
        meta = self.runs("meta")
        self.assertIn('mode=tag', meta)
        self.assertIn('version=${GITHUB_REF_NAME#v}', meta)
        self.assertIn('mode=dry-run', meta)
        self.assertIn('python scripts/release.py meta --mode "$mode" --version "$version"', meta)
        self.assertIn('python scripts/release.py prepare --mode "$MODE" --version "$VERSION" --date "$DATE"', meta)
        self.assertEqual(set(self.jobs["meta"]["outputs"]),
                         {"version", "pep440", "numeric", "label", "date", "mode", "prerelease"})

    def test_the_calls_pass_the_release_and_nothing_else(self):
        for job_id, inputs in (("tests", {"mode", "version", "date"}), ("package", {"mode", "version", "date",
                                                                                    "label"})):
            job = self.jobs[job_id]
            self.assertEqual(set(job["with"]), inputs, job_id)
            for key in inputs:
                self.assertEqual(job["with"][key], f"${{{{ needs.meta.outputs.{key} }}}}")
            self.assertNotIn("secrets", job)

    def test_the_signing_job_builds_and_installs_nothing(self):
        job = self.jobs["sign-helper-manifest"]
        self.assertEqual(job["environment"], TAG_ENV)
        self.assertEqual(self.uses("sign-helper-manifest"), ["actions/checkout", "actions/download-artifact",
                                                             "actions/download-artifact", "actions/upload-artifact"])
        self.assertEqual(self.downloads("sign-helper-manifest"), {"pkg-helper-manifest": "manifest",
                                                                  "pkg-xtask-linux-x64": "xtask-bin"})
        (step,) = [s for s in job["steps"] if "run" in s]
        self.assertEqual(step["env"], {"CITAR_UPDATE_SIGNING_KEY": "${{ secrets.CITAR_UPDATE_SIGNING_KEY }}"})
        self.assertTrue(step["run"].startswith("python3 scripts/ci/release_sign.py sign --xtask xtask-bin/xtask"))
        self.assertIn("--keys crates/citar-jobs/update-keys.pub --key-env CITAR_UPDATE_SIGNING_KEY", step["run"])
        upload = job["steps"][-1]["with"]
        self.assertEqual((upload["name"], upload["path"]), ("rel-helper-signature", "signature"))

    def test_check_signature_sums_and_uploads_before_anything_from_pypi(self):
        steps = self.jobs["check-signature"]["steps"]
        names = [s.get("uses", "").split("@")[0] or s.get("run", "").split()[0] for s in steps]
        upload = next(i for i, s in enumerate(steps) if cw.action_of(s.get("uses")) == "actions/upload-artifact")
        pip = next(i for i, s in enumerate(steps) if "pip install" in s.get("run", ""))
        self.assertLess(upload, pip, names)
        setup = next(i for i, s in enumerate(steps) if cw.action_of(s.get("uses")) == "actions/setup-python")
        self.assertLess(upload, setup, "the sums and the expectation run on the runner's own python3")
        self.assertIn("--require-hashes", steps[pip]["run"])
        self.assertIn("--only-binary :all: -r scripts/ci/verify-manifest-requirements.txt", steps[pip]["run"])
        before = "\n".join(s.get("run", "") for s in steps[:upload])
        self.assertIn("release_sign.py expect", before)
        self.assertIn("assemble_release.py sums", before)
        self.assertNotIn("pip", before)
        self.assertEqual(steps[upload]["with"]["name"], "rel-release-sums")
        after = self.runs("check-signature")
        self.assertIn("verify_manifest.py --check-keys crates/citar-jobs/update-keys.pub", after)
        self.assertIn("verify_manifest.py --pub crates/citar-jobs/update-keys.pub", after)

    def test_the_verifiers_requirements_are_hash_pinned(self):
        text = (ROOT / "scripts" / "ci" / "verify-manifest-requirements.txt").read_text(encoding="utf-8")
        reqs = {}
        for block in text.replace("\\\n", " ").splitlines():
            if block.strip() and not block.startswith("#"):
                name, _, rest = block.partition("==")
                version = rest.split()[0]
                hashes = [w for w in rest.split() if w.startswith("--hash=sha256:")]
                self.assertTrue(hashes, name)
                self.assertTrue(all(len(h) == len("--hash=sha256:") + 64 for h in hashes), name)
                reqs[name.strip()] = version
        self.assertEqual(set(reqs), {"cryptography", "cffi", "pycparser"})
        self.assertGreaterEqual(int(reqs["cryptography"].split(".")[0]), 42, "pyproject's floor")

    def test_the_publishing_jobs_use_only_what_they_publish_with(self):
        self.assertEqual(self.uses("publish-pypi"), ["actions/download-artifact", "pypa/gh-action-pypi-publish"])
        pypi = self.jobs["publish-pypi"]["steps"][-1]["with"]
        self.assertEqual((pypi["packages-dir"], pypi["skip-existing"]), ("dist/", True))
        self.assertEqual(self.downloads("publish-pypi"), {"pkg-python-dists": "dist"})
        self.assertEqual(self.uses("attest-files")[-1], "actions/attest-build-provenance")
        self.assertEqual(self.uses("attest-image"), ["actions/attest-build-provenance"])
        image = self.jobs["attest-image"]["steps"][-1]["with"]
        self.assertEqual(image["create-storage-record"], False)
        self.assertEqual(self.downloads("publish-image"), {
            "pkg-image-amd64": "image", "pkg-image-arm64": "image", "pkg-image-report-amd64": "report",
            "pkg-image-report-arm64": "report"})
        self.assertIn("check_image.py load --engine docker", self.runs("publish-image"))
        self.assertEqual(self.jobs["publish-image"]["outputs"], {"digest": "${{ steps.join.outputs.digest }}"})

    def test_the_image_pushed_is_the_one_package_yml_tags(self):
        # package.yml's image job tags and saves the image as TAG; publish-image pushes "$IMAGE_NAME:$VERSION-$arch".
        # The two names are written apart, so they are held together here, and check_image.py load holds the
        # report's name to the pushed one on every dry run and tag.
        import yaml
        package = yaml.safe_load((WORKFLOWS / "package.yml").read_text(encoding="utf-8"))
        tag = package["jobs"]["image"]["env"]["TAG"]
        pushed = (f"{self.data['env']['IMAGE_NAME']}:${{{{ needs.meta.outputs.version }}}}-"
                  f"${{{{ matrix.arch }}}}")
        self.assertEqual(tag, pushed)
        self.assertEqual(self.data["env"]["IMAGE_NAME"], "ghcr.io/jprodgers/citar")
        self.assertEqual(self.jobs["publish-image"]["env"]["VERSION"], "${{ needs.meta.outputs.version }}")
        self.assertEqual(self.jobs["dry-run-summary"]["env"]["VERSION"], "${{ needs.meta.outputs.version }}")
        for job_id in ("publish-image", "dry-run-summary"):
            loads = [line for line in self.runs(job_id).replace("\\\n", " ").splitlines() if "check_image.py load" in line]
            self.assertEqual(len(loads), 1, job_id)
            self.assertIn('--image "$IMAGE_NAME:$VERSION-$arch"', loads[0], job_id)
        self.assertIn('docker push "$IMAGE_NAME:$VERSION-$arch"', self.runs("publish-image"))
        for job_id in ("publish-image", "attest-image", "image-tags"):
            self.assertIn('docker login ghcr.io --username "$GITHUB_ACTOR" --password-stdin', self.runs(job_id))

    def test_the_release_is_whole_before_it_is_public(self):
        runs = self.runs("github-release")
        self.assertIn("assemble_release.py final", runs)
        self.assertIn("--list upload.txt", runs)
        self.assertIn('gh release create "$tag" --draft --verify-tag', runs)
        self.assertIn("gh release upload \"$tag\" --clobber", runs, "a re-run finishes its draft")
        self.assertLess(runs.index("diff want.txt have.txt"), runs.index("--draft=false"))
        self.assertIn("latest=--latest=false", runs)
        tags = self.runs("image-tags")
        self.assertLess(tags.index('if [ "$PRERELEASE" = true ]'), tags.index("imagetools create"))

    def test_the_dry_runs_summary_rehearses_the_tag_only_jobs_file_steps(self):
        self.assertEqual(set(self.downloads("dry-run-summary")), {
            "pkg-release-preview", "pkg-release-assets", "pkg-image-amd64", "pkg-image-arm64",
            "pkg-image-report-amd64", "pkg-image-report-arm64"})
        runs = self.runs("dry-run-summary")
        for command in ("check_image.py load", "assemble_release.py sums", "assemble_release.py final",
                        "assemble_release.py summary", '"$GITHUB_STEP_SUMMARY"', "diff summary/assets.txt",
                        "cmp summary/sums/notes.md preview/notes.md"):
            self.assertIn(command, runs)
        self.assertNotIn("environment", self.jobs["dry-run-summary"])
        # Its checkout is bumped first, as every build's was, or a dry run's notes (the CHANGELOG's V section)
        # could not be written there.
        steps = self.jobs["dry-run-summary"]["steps"]
        self.assertEqual(cw.action_of(steps[0].get("uses")), "actions/checkout")
        self.assertTrue(cw.is_prepare(steps[1]), steps[1])
        self.assertIn('--date "$DATE"', steps[1]["run"])


class ReleasePlanted(unittest.TestCase):
    """Gate 1 of package 5-09: each plant in release.yml fails naming its job and rule."""

    assertFinds = Planted.assertFinds
    assertPasses = Planted.assertPasses

    def test_the_real_file_passes_rule_16(self):
        self.assertPasses(Tree(self))

    def test_a_publishing_job_guarded_by_a_tag_ref_alone(self):
        tree = Tree(self)
        tree.replace_in_job("release.yml", "publish-pypi", f"    if: {TAG_GUARD}\n",
                            "    if: startsWith(github.ref, 'refs/tags/')\n")
        self.assertFinds(tree, 2, "release.yml", "publish-pypi", "github.event_name == 'push'")
        self.assertFinds(tree, 16, "release.yml", "publish-pypi", "tag-only")

    def test_publish_pypi_not_needing_attest_files(self):
        tree = Tree(self)
        tree.replace_in_job("release.yml", "publish-pypi", "needs: [meta, attest-files]",
                            "needs: [meta, check-signature]")
        self.assertFinds(tree, 16, "release.yml", "publish-pypi", "not ['attest-files']")

    def test_attest_files_after_publish_pypi(self):
        tree = Tree(self)
        tree.replace_in_job("release.yml", "attest-files", "needs: [meta, check-signature]",
                            "needs: [meta, publish-pypi]")
        tree.replace_in_job("release.yml", "publish-pypi", "needs: [meta, attest-files]",
                            "needs: [meta, check-signature]")
        self.assertFinds(tree, 16, "release.yml", "attest-files", "not ['check-signature']")
        self.assertFinds(tree, 16, "release.yml", "publish-pypi", "not ['attest-files']")

    def test_latest_or_the_series_pushed_in_publish_image(self):
        for plant in ('            docker push "$IMAGE_NAME:latest"\n',
                      '            docker push "$IMAGE_NAME:$series"\n',
                      # the same pushes in other forms: each is a push
                      '            docker image push "$IMAGE_NAME:latest"\n',
                      '            podman image push "$IMAGE_NAME:latest"\n',
                      '            docker manifest push "$IMAGE_NAME:latest"\n',
                      '            docker --config "$RUNNER_TEMP/docker" push "$IMAGE_NAME:latest"\n',
                      '            docker push --platform linux/amd64 "$IMAGE_NAME:latest"\n',
                      # the series as an expansion of VERSION, and a repository without a tag (:latest, or every tag)
                      '            docker push "$IMAGE_NAME:${VERSION%.*}"\n',
                      '            docker push "$IMAGE_NAME:${VERSION}-rc"\n',
                      '            docker push --all-tags "$IMAGE_NAME"\n'):
            with self.subTest(plant=plant):
                tree = Tree(self)
                tree.replace_in_job("release.yml", "publish-image", '            docker push "$IMAGE_NAME:$VERSION-$arch"\n',
                                    '            docker push "$IMAGE_NAME:$VERSION-$arch"\n' + plant)
                self.assertFinds(tree, 16, "release.yml", "publish-image", "pushes the version's tags alone")
        for plant in ('            podman push "$IMAGE_NAME:$VERSION-$arch" "docker://$IMAGE_NAME:latest"\n',
                      '            docker push "ghcr.io/jprodgers/citar-image:$VERSION-$arch"\n'):
            with self.subTest(plant=plant):
                tree = Tree(self)
                tree.replace_in_job("release.yml", "publish-image", '            docker push "$IMAGE_NAME:$VERSION-$arch"\n',
                                    '            docker push "$IMAGE_NAME:$VERSION-$arch"\n' + plant)
                self.assertFinds(tree, 16, "release.yml", "publish-image", "is $IMAGE_NAME")
        tree = Tree(self)
        tree.replace_in_job("release.yml", "publish-image", '            docker push "$IMAGE_NAME:$VERSION-$arch"\n',
                            '            docker push "$IMAGE_NAME:$VERSION-$arch"\n'
                            '            skopeo copy "docker-daemon:$IMAGE_NAME:$VERSION-$arch" '
                            '"docker://$IMAGE_NAME:latest"\n')
        self.assertFinds(tree, 16, "release.yml", "publish-image", "runs skopeo")
        tree = Tree(self)
        tree.replace_in_job("release.yml", "publish-image", '--tag "$IMAGE_NAME:$VERSION"',
                            '--tag "$IMAGE_NAME:$VERSION" --tag "$IMAGE_NAME:latest"')
        self.assertFinds(tree, 16, "release.yml", "publish-image", ":latest")

    def test_publish_images_version_is_meta_s_alone(self):
        tree = Tree(self)
        tree.replace_in_job("release.yml", "publish-image", "      VERSION: ${{ needs.meta.outputs.version }}\n",
                            "      VERSION: latest\n")
        self.assertFinds(tree, 16, "release.yml", "publish-image", "not ${{ needs.meta.outputs.version }}")
        for assignment, named in (("VERSION=\"${VERSION%.*}\"", "VERSION"), ("export VERSION=0.1", "VERSION"),
                                  ("read -r IMAGE_NAME < image-name.txt", "IMAGE_NAME"),
                                  ("for VERSION in 0.1; do :; done", "VERSION")):
            with self.subTest(assignment=assignment):
                tree = Tree(self)
                tree.replace_in_job("release.yml", "publish-image", "          for arch in amd64 arm64; do\n"
                                    "            docker push",
                                    f"          {assignment}\n          for arch in amd64 arm64; do\n"
                                    "            docker push")
                self.assertFinds(tree, 16, "release.yml", "publish-image", f"assigns {named}")
        tree = Tree(self)
        tree.replace_in_job("release.yml", "publish-image", "        id: join\n        shell: bash\n",
                            "        id: join\n        shell: bash\n        env:\n          IMAGE_NAME: ghcr.io/x/y\n")
        self.assertFinds(tree, 16, "release.yml", "publish-image", "sets IMAGE_NAME in its env")

    def test_another_repository_in_image_tags_or_a_push_elsewhere(self):
        tree = Tree(self)
        tree.replace_in_job("release.yml", "image-tags", '--tag "$IMAGE_NAME:latest"',
                            '--tag "ghcr.io/jprodgers/other:latest"')
        self.assertFinds(tree, 16, "release.yml", "image-tags", "is $IMAGE_NAME")
        tree = Tree(self)
        tree.replace_in_job("release.yml", "dry-run-summary", "          cat summary/summary.md\n",
                            "          cat summary/summary.md\n          docker image push \"$IMAGE_NAME:$VERSION-amd64\"\n")
        self.assertFinds(tree, 16, "release.yml", "dry-run-summary", "only publish-image and image-tags push")

    def test_attest_image_without_packages_write(self):
        tree = Tree(self)
        tree.replace_in_job("release.yml", "attest-image", "      packages: write\n", "")
        self.assertFinds(tree, 16, "release.yml", "attest-image", "not P5.11's ['attestations', 'id-token', 'packages']")

    def test_a_docker_build_step_in_release_yml(self):
        tree = Tree(self)
        tree.replace_in_job("release.yml", "publish-image", "      - name: Each image the one tested\n",
                            "      - run: docker build -t ghcr.io/jprodgers/citar:x .\n"
                            "      - name: Each image the one tested\n")
        self.assertFinds(tree, 8, "release.yml", "publish-image", "docker build")
        self.assertFinds(tree, 15, "release.yml", "publish-image", "docker build")

    def test_github_release_without_its_draft_first(self):
        tree = Tree(self)
        tree.replace_in_job("release.yml", "github-release", 'gh release create "$tag" --draft --verify-tag',
                            'gh release create "$tag" --verify-tag')
        self.assertFinds(tree, 16, "release.yml", "github-release", "without --draft")
        tree = Tree(self)  # made public before the files are up
        tree.replace_in_job("release.yml", "github-release", "      - name: A draft with every file and the notes\n",
                            "      - run: gh release edit \"v$VERSION\" --draft=false\n"
                            "      - name: A draft with every file and the notes\n")
        self.assertFinds(tree, 16, "release.yml", "github-release", "not after every file is uploaded")

    def test_the_signing_job_with_a_cargo_step_setup_python_or_kept_credentials(self):
        anchor = "      - uses: actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8.0.2\n" \
                 "        with:\n          name: pkg-helper-manifest\n"
        tree = Tree(self)
        tree.replace_in_job("release.yml", "sign-helper-manifest", anchor,
                            "      - run: cargo build --release --locked -p xtask\n" + anchor)
        self.assertFinds(tree, 15, "release.yml", "sign-helper-manifest", "runs cargo")
        self.assertFinds(tree, 8, "release.yml", "sign-helper-manifest", "runs cargo")
        tree = Tree(self)
        tree.replace_in_job("release.yml", "sign-helper-manifest", anchor,
                            "      - uses: actions/setup-python@5fda3b95a4ea91299a34e894583c3862153e4b97 # v7.0.0\n"
                            + anchor)
        self.assertFinds(tree, 15, "release.yml", "sign-helper-manifest", "actions/setup-python")
        tree = Tree(self)
        tree.replace_in_job("release.yml", "sign-helper-manifest", "        with:\n          persist-credentials: false\n",
                            "")
        self.assertFinds(tree, 15, "release.yml", "sign-helper-manifest", "persist-credentials: false")

    def test_a_calling_job_with_secrets_inherit(self):
        tree = Tree(self)
        tree.replace_in_job("release.yml", "package", "    permissions:\n      contents: read\n",
                            "    permissions:\n      contents: read\n    secrets: inherit\n")
        self.assertFinds(tree, 12, "release.yml", "package", "inherit")

    # ---- the rest of rule 16
    def test_a_job_outside_the_table_and_one_missing(self):
        tree = Tree(self)
        tree.add_job("release.yml", "publish-early", f"""
needs: package
if: {TAG_GUARD}
runs-on: ubuntu-latest
permissions:
  contents: write
steps:
  - run: gh release upload "v1" dist/*
""")
        self.assertFinds(tree, 16, "release.yml", "publish-early", "not one of P5.11's jobs")
        self.assertFinds(tree, 16, "release.yml", "publish-early", "only github-release makes the release")
        tree = Tree(self)
        text = tree.read("release.yml")
        start = text.index("\n  image-tags:\n")
        end = text.index("\n  dry-run-summary:\n")
        tree.write("release.yml", text[:start] + text[end:])
        self.assertFinds(tree, 16, "release.yml", "image-tags", "is missing")

    def test_conditions_and_environments(self):
        tree = Tree(self)
        tree.replace_in_job("release.yml", "dry-run-summary", "    if: github.event_name == 'workflow_dispatch'\n",
                            "    if: always()\n")
        self.assertFinds(tree, 16, "release.yml", "dry-run-summary", "workflow_dispatch")
        tree = Tree(self)
        tree.replace_in_job("release.yml", "meta", "    runs-on: ubuntu-24.04\n",
                            f"    if: {TAG_GUARD}\n    runs-on: ubuntu-24.04\n")
        self.assertFinds(tree, 16, "release.yml", "meta", "takes no if:")
        tree = Tree(self)
        tree.replace_in_job("release.yml", "sign-helper-manifest", f"    environment: {TAG_ENV}\n", "")
        self.assertFinds(tree, 16, "release.yml", "sign-helper-manifest", "helper-signing through the tag expression")
        tree = Tree(self)
        tree.replace_in_job("release.yml", "publish-pypi", "    environment: pypi\n", "")
        self.assertFinds(tree, 16, "release.yml", "publish-pypi", "not pypi")
        tree = Tree(self)
        tree.replace_in_job("release.yml", "check-signature", "    runs-on: ubuntu-24.04\n",
                            f"    environment: {TAG_ENV}\n    runs-on: ubuntu-24.04\n")
        self.assertFinds(tree, 16, "release.yml", "check-signature", "P5.11 gives it none")

    def test_permissions_beyond_the_table(self):
        tree = Tree(self)
        tree.replace_in_job("release.yml", "github-release", "      contents: write\n",
                            "      contents: write\n      packages: write\n")
        self.assertFinds(tree, 16, "release.yml", "github-release", "not P5.11's ['contents']")
        tree = Tree(self)
        tree.replace_in_job("release.yml", "dry-run-summary", "    runs-on: ubuntu-24.04\n",
                            "    runs-on: ubuntu-24.04\n    permissions:\n      contents: write\n")
        self.assertFinds(tree, 16, "release.yml", "dry-run-summary", "not P5.11's none")

    def test_the_secret_in_its_one_step_alone(self):
        step_env = "        env:\n          CITAR_UPDATE_SIGNING_KEY: ${{ secrets.CITAR_UPDATE_SIGNING_KEY }}\n"
        tree = Tree(self)
        tree.replace_in_job("release.yml", "sign-helper-manifest", step_env,
                            step_env + "          HOME_DIR: /tmp\n")
        self.assertFinds(tree, 16, "release.yml", "sign-helper-manifest", "alone")
        tree = Tree(self)
        tree.replace_in_job("release.yml", "sign-helper-manifest", "    timeout-minutes: 15\n",
                            "    timeout-minutes: 15\n    env:\n"
                            "      CITAR_UPDATE_SIGNING_KEY: ${{ secrets.CITAR_UPDATE_SIGNING_KEY }}\n")
        self.assertFinds(tree, 16, "release.yml", "sign-helper-manifest", "at the job's level")
        tree = Tree(self)
        tree.replace_in_job("release.yml", "check-signature", "    env:\n",
                            "    env:\n      TOKEN: ${{ secrets.PYPI_TOKEN }}\n")
        self.assertFinds(tree, 16, "release.yml", "check-signature", "reads one secret")

    def test_attest_files_attests_the_finished_folder(self):
        tree = Tree(self)
        tree.replace_in_job("release.yml", "attest-files", "          subject-path: release/*\n",
                            "          subject-path: release/*.whl\n")
        self.assertFinds(tree, 16, "release.yml", "attest-files", "not the folder pkg-release-assets fills")
        tree = Tree(self)
        tree.replace_in_job("release.yml", "attest-files",
                            '        run: python3 scripts/ci/assemble_release.py final --version "$VERSION" '
                            '--release release --sums sums\n', "        run: ls release\n")
        self.assertFinds(tree, 16, "release.yml", "attest-files", "comes before assemble_release.py final")
        tree = Tree(self)
        tree.replace_in_job("release.yml", "attest-image", "          push-to-registry: true\n", "")
        self.assertFinds(tree, 16, "release.yml", "attest-image", "push-to-registry")

    def test_the_concurrency_is_never_cancelled(self):
        tree = Tree(self)
        tree.replace("release.yml", "  cancel-in-progress: false\n", "  cancel-in-progress: true\n")
        found = self.assertFinds(tree, 16, "release.yml", None, "never cut off mid-publish")
        self.assertTrue([f for f in found if f.rule == 16 and f.job is None], found)


class Expressions(unittest.TestCase):
    def test_the_guards(self):
        self.assertIsNone(cw.guard_problem(TAG_GUARD, "tag"))
        self.assertIsNone(cw.guard_problem(f"${{{{ ({TAG_GUARD}) && success() }}}}", "tag"))
        self.assertIsNone(cw.guard_problem("github.ref == 'refs/heads/main'", "main"))
        self.assertIsNone(cw.guard_problem("'refs/heads/main' == github.ref && github.event_name != 'x'", "main"))
        for bad, kind in ((None, "tag"), ("startsWith(github.ref, 'refs/tags/')", "tag"),
                          (f"!({TAG_GUARD})", "tag"), (f"({TAG_GUARD}) || true", "tag"),
                          (f"{TAG_GUARD} && always()", "tag"), ("github.ref != 'refs/heads/main'", "main"),
                          ("github.ref == 'refs/heads/main' || github.event_name == 'workflow_dispatch'", "main"),
                          (f"x-${{{{ {TAG_GUARD} }}}}", "tag"), (True, "tag")):
            with self.subTest(bad=bad):
                self.assertIsNotNone(cw.guard_problem(bad, kind))

    def test_the_tag_expression(self):
        self.assertEqual(cw.tag_expression_name(TAG_ENV), "helper-signing")
        self.assertEqual(cw.TAG_ENVIRONMENTS, ("helper-signing",))
        self.assertIsNone(cw.tag_expression_name("helper-signing"))
        self.assertIsNone(cw.tag_expression_name(TAG_ENV.replace("'push'", "'workflow_dispatch'")))

    def test_the_reader(self):
        node = cw.parse_expr("a.b['C'] == 'it''s' && !f(x, 1) || (y < 2)")
        self.assertEqual(node[0], "or")
        self.assertEqual(node[1][0][1][0], ("cmp", "==", ("ctx", "a.b.c"), ("lit", "it's")))
        self.assertEqual(node[1][0][1][1], ("not", ("call", "f", [("ctx", "x"), ("lit", 1.0)])))
        with self.assertRaises(cw.ExprError):
            cw.parse_expr("a == ")


class Shell(unittest.TestCase):
    def commands(self, text):
        return [cw.command_words(c) for c in cw.shell_commands(text)]

    def test_words_quotes_comments_and_separators(self):
        self.assertEqual(self.commands('echo "a b" c # git push\nFOO=1 sudo git push && cd x; ls | wc'),
                         [["echo", "a b", "c"], ["git", "push"], ["cd", "x"], ["ls"], ["wc"]])
        self.assertEqual(self.commands("echo ${X#y} 'it''s'"), [["echo", "${X#y}", "its"]])

    def test_text_is_not_a_command_but_bash_c_is(self):
        text = "echo \"run 'python scripts/build_wiki.py --push' to publish\"\nbash -c 'git push'\neval twine upload x"
        argvs = self.commands(text)
        self.assertFalse(any(cw.wiki_push(a) for a in argvs))
        self.assertIn(["git", "push"], argvs)
        self.assertTrue(any(cw.publishing_command(a) == "twine upload" for a in argvs))

    def test_heredocs_and_expressions(self):
        argvs = self.commands("python3 - <<'PY'\nimport os; os.system('git push')\nPY\n"
                              "echo ${{ runner.os == 'Windows' && 'x' || 'y' }} done")
        self.assertEqual(argvs, [["python3", "-"], ["echo", "__EXPR__", "done"]])
        # a heredoc a shell reads is its script: the shell's own, or one its output is piped into
        for script in ("bash <<'EOF'\ngh release create v1 dist/*\nEOF", "bash -s <<EOF\ngh release create v1\nEOF",
                       "sudo bash -e <<-\"EOF\"\n\tgh release create v1\n\tEOF",
                       "cat <<EOF | sed s/x/y/ | bash -s -- arg\ngh release create v1\nEOF",
                       "x=1 sh 2>&1 <<EOF\ngh release create v1\nEOF"):
            with self.subTest(script=script):
                self.assertTrue(any(cw.publishing_command(a) == "gh release create" for a in self.commands(script)),
                                self.commands(script))
        # and other heredocs are data: a file written, a script file's stdin, a -c script's stdin
        for script in ("cat > notes.txt <<EOF\ngh release create v1\nEOF", "bash x.sh <<EOF\ngh release create v1\nEOF",
                       "bash -c 'read x' <<EOF\ngh release create v1\nEOF", "cat <<EOF && bash\ngh release create\nEOF"):
            with self.subTest(script=script):
                self.assertFalse(any(cw.publishing_command(a) for a in self.commands(script)), self.commands(script))
        # two heredocs on a line, each body in turn; the commands after them are read again
        self.assertEqual(self.commands("diff - /dev/fd/3 <<A 3<<B\ngit push\nA\ngit push\nB\ncargo build"),
                         [["diff", "-", "/dev/fd/3", "3"], ["cargo", "build"]])
        # a << in quotes or a comment starts no heredoc, and arithmetic's << none either
        self.assertEqual(self.commands("echo \"use <<STOP to end\" # or <<END\ncargo build\nx=$((1 << 2))\npip x"),
                         [["echo", "use <<STOP to end"], ["cargo", "build"], [], ["1", "2"], ["pip", "x"]])

    def test_scripts_given_to_a_shell_and_launchers(self):
        for script, inner in (("bash -lc 'cargo build'", ["cargo", "build"]),
                              ("sh <<< \"cargo build\"", ["cargo", "build"]),
                              ("pwsh -NoProfile -Command \"cargo build; echo built\"", ["cargo", "build"]),
                              ("powershell -c cargo build", ["cargo", "build"]),
                              ("cmd /s /c cargo build", ["cargo", "build"]),
                              ("eval cargo build", ["cargo", "build"]),
                              ("sudo -E -u runner pip install x", ["pip", "install", "x"]),
                              ("sudo -- pip install x", ["pip", "install", "x"]),
                              ("timeout 10m env A=1 nice -n 5 cargo build", ["cargo", "build"])):
            with self.subTest(script=script):
                self.assertIn(inner, self.commands(script))
        self.assertNotIn(["cargo", "build"], self.commands("grep x <<< 'cargo build'"))
        # a redirection is part of its command, not a separator
        self.assertEqual(self.commands("cargo build 2>&1 | tee log &>/dev/null && ls >&2 & wait"),
                         [["cargo", "build", "2>&1"], ["tee", "log", "&>/dev/null"], ["ls", ">&2"], ["wait"]])
        self.assertTrue(cw.shell_reads_stdin(["bash"]))
        self.assertTrue(cw.shell_reads_stdin(["bash", "-o", "pipefail", ">", "log"]))
        self.assertFalse(cw.shell_reads_stdin(["bash", "-o", "pipefail", "x.sh"]))
        self.assertFalse(cw.shell_reads_stdin(["cat"]))

    def test_tools_and_publishing(self):
        self.assertEqual(cw.tool_of(["python", "-m", "pip", "install", "x"]), "pip")
        self.assertEqual(cw.tool_of(["C:\\py\\python.exe", "-m", "PyInstaller"]), "pyinstaller")
        self.assertEqual(cw.tool_of(["/usr/bin/cargo", "build"]), "cargo")
        self.assertIsNone(cw.publishing_command(["gh", "api", "repos/x/y/environments"]))
        self.assertIsNone(cw.publishing_command(["gh", "api", "-X", "GET", "search", "-f", "q=x"]))
        self.assertIsNotNone(cw.publishing_command(["gh", "api", "repos/x/y/releases", "-f", "tag_name=v1"]))
        self.assertIsNotNone(cw.publishing_command(["gh", "api", "--method=DELETE", "repos/x/y/releases/1"]))
        self.assertIsNone(cw.publishing_command(["gh", "release", "download", "v1"]))
        self.assertIsNone(cw.publishing_command(["gh", "pr", "view", "5"]))
        self.assertIsNotNone(cw.publishing_command(["docker", "buildx", "imagetools", "create", "-t", "x", "y"]))
        self.assertIsNotNone(cw.publishing_command(["git", "-C", "wiki", "push"]))
        self.assertIsNone(cw.publishing_command(["git", "log", "--grep", "push"]))
        self.assertEqual(cw.publishing_command(["gh", "-R", "o/r", "release", "create", "v1"]), "gh release create")
        self.assertIsNotNone(cw.publishing_command(["gh", "--repo=o/r", "api", "-X", "PATCH", "repos/o/r"]))
        self.assertEqual(cw.tool_of(["python", "scripts/x.py", "-m", "pip"]), "python")
        self.assertEqual(cw.tool_of(["python3", "-X", "dev", "-m", "pip", "install"]), "pip")
        self.assertIn(["cargo", "build"], [cw.command_words(c) for c in cw.shell_commands("bash -lc 'cargo build'")])
        self.assertTrue(cw.is_prepare({"run": "py -3.12 -I scripts/release.py prepare --mode ci"}))
        self.assertFalse(cw.is_prepare({"run": "echo python scripts/release.py prepare"}))
        self.assertEqual(cw.build_command(["docker", "buildx", "build", "."]), "docker build")
        self.assertIsNone(cw.build_command(["docker", "run", "x"]))
        for pip in ("pip", "pip3", "pip3.12", "/usr/bin/pip3.12", "pip3.12.exe", "pip-3.12"):
            with self.subTest(pip=pip):
                self.assertEqual(cw.tool_of([pip, "install"]), "pip")
        self.assertEqual(cw.build_command(["pip3.11", "wheel", "."]), "pip wheel")
        self.assertEqual(cw.tool_of(["pipx", "run", "build"]), "pipx")
        self.assertEqual(cw.secrets_read({"env": {"T": "${{ github.token }}", "U": "${{ GITHUB.Token }}"}}),
                         {"GITHUB_TOKEN"})
        self.assertEqual(cw.secrets_read({"env": {"T": "${{ github.token_x }}", "U": "github.token"}}), set())


class Parts(unittest.TestCase):
    def test_matrix_legs(self):
        legs = cw.matrix_legs({"strategy": {"matrix": {"os": ["a", "b"], "py": ["1", "2"],
                                                       "exclude": [{"os": "b", "py": "2"}],
                                                       "include": [{"os": "a", "extra": "x"}, {"os": "c"}]}}})
        self.assertEqual(legs, [{"os": "a", "py": "1", "extra": "x"}, {"os": "a", "py": "2", "extra": "x"},
                                {"os": "b", "py": "1"}, {"os": "c"}])
        self.assertEqual(cw.matrix_legs({}), [{}])
        self.assertEqual(cw.expand("ext-${{ runner.os }}-${{ matrix.os }}-${{ github.sha }}", {"os": "a"}, "Linux", 0),
                         "ext-Linux-a-${{ github.sha }}")

    def test_upload_paths(self):
        for path in ("dist/*.whl", "helper/", "golden-${{ matrix.target }}.json", "!x/*.key", "site"):
            self.assertIsNone(cw.path_holds_key(path), path)
        for path in ("a.key", "out/**", "$RUNNER_TEMP/throwaway", "${{ runner.temp }}/*", "../x", "/"):
            self.assertIsNotNone(cw.path_holds_key(path), path)


class ActionPins(unittest.TestCase):
    """The pin tool, offline: GitHub stood in for by a table of what its API would answer."""

    COMMIT = "a" * 40
    OTHER = "b" * 40

    def fake(self, refs):
        """A getter answering ``git/ref/tags``, ``git/matching-refs``, ``git/tags`` and ``commits`` from ``refs``,
        a dict of tag or branch name to commit (an annotated tag is a dict {"tag": object sha, "commit": sha})."""
        def get(endpoint, missing_ok=False):
            parts = endpoint.split("/")
            if "matching-refs" in parts:
                prefix = endpoint.split("matching-refs/tags/", 1)[1]
                return [self.ref(name, value) for name, value in refs.items()
                        if name.startswith(prefix) and not name.startswith("heads:")]
            if "git" in parts and parts[-3] == "ref":
                kind, name = parts[-2], parts[-1]
                key = name if kind == "tags" else f"heads:{name}"
                if key in refs:
                    return self.ref(name, refs[key])
                return None if missing_ok else self.fail(endpoint)
            if "git" in parts and parts[-2] == "tags":
                tagged = next(v["commit"] for v in refs.values() if isinstance(v, dict) and v["tag"] == parts[-1])
                return {"object": {"type": "commit", "sha": tagged}}
            if parts[-2] == "commits":
                known = {v["commit"] if isinstance(v, dict) else v for v in refs.values()}
                return {"sha": parts[-1]} if parts[-1] in known else None
            raise AssertionError(endpoint)
        return get

    @staticmethod
    def ref(name, value):
        if isinstance(value, dict):
            return {"ref": f"refs/tags/{name}", "object": {"type": "tag", "sha": value["tag"]}}
        return {"ref": f"refs/tags/{name}", "object": {"type": "commit", "sha": value}}

    def tree(self, body):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        folder = Path(tmp.name) / ".github" / "workflows"
        folder.mkdir(parents=True)
        (folder / "x.yml").write_text(body, encoding="utf-8", newline="\n")
        return Path(tmp.name), folder / "x.yml"

    def test_pin_writes_the_commit_and_the_most_precise_version(self):
        refs = {"v7": self.COMMIT, "v7.0.0": self.OTHER, "v7.0.1": {"tag": "c" * 40, "commit": self.COMMIT},
                "v7.0.1-rc.1": self.COMMIT, "heads:main": self.OTHER}
        root, path = self.tree("jobs:\n  a:\n    steps:\n      - uses: actions/checkout@v7\n"
                               "      - uses: \"o/r/sub@main\"  # keep?\n      - uses: ./local\n")
        with redirect_stdout(io.StringIO()):
            self.assertEqual(pins.pin(root, write=True, get=self.fake(refs)), 0)
        text = path.read_text(encoding="utf-8")
        self.assertIn(f"      - uses: actions/checkout@{self.COMMIT} # v7.0.1\n", text)
        self.assertIn(f"      - uses: o/r/sub@{self.OTHER} # v7.0.0\n", text)
        self.assertIn("      - uses: ./local\n", text)
        out = io.StringIO()
        with redirect_stdout(out):
            status, rows = pins.verify(root, get=self.fake(refs))
        self.assertEqual(status, 0, out.getvalue())
        self.assertEqual([r["ok"] for r in rows], [True, True])

    def test_verify_refuses_a_wrong_tag_a_foreign_commit_and_a_bare_pin(self):
        refs = {"v7.0.1": self.COMMIT, "v7.0.0": self.OTHER}
        root, _ = self.tree(f"jobs:\n  a:\n    steps:\n      - uses: a/b@{self.COMMIT} # v7.0.0\n"
                            f"      - uses: a/b@{'d' * 40} # v7.0.1\n      - uses: a/b@{self.COMMIT}\n"
                            "      - uses: a/b@v7\n")
        with redirect_stdout(io.StringIO()):
            status, rows = pins.verify(root, get=self.fake(refs))
        self.assertEqual(status, 1)
        self.assertEqual([r["ok"] for r in rows], [False, False, False, False])
        self.assertIn("is the commit", rows[0]["why"])
        self.assertIn("is not a commit of a/b", rows[1]["why"])
        self.assertIn("no version", rows[2]["why"])
        self.assertIn("not pinned", rows[3]["why"])

    def test_the_tree_is_pinned_as_check_workflows_reads_it(self):
        uses = pins.all_uses(ROOT)
        self.assertTrue(uses)
        for use in uses:
            self.assertTrue(use.pinned, use.ref)
            self.assertIsNotNone(use.version, use.ref)
        self.assertEqual(len({u.repo for u in uses}), len({u.repo.lower() for u in uses}))


if __name__ == "__main__":
    unittest.main()
