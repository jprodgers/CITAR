"""scripts/ci/check_workflows.py and scripts/ci/action_pins.py (design/phase5-release.md P5.17, package 5-02a).

The checker passes on the tree's workflows; each rule fails on a copy of them with one thing planted, naming its file,
its job and its rule (gate 1 of 5-02a, and more). package.yml's rules are tried on a small fixture of it written over
the real one in a copy (the fixture passes as written), and on the real one, which 5-02b added; what P5.3 says of
package.yml that no rule does (the five wheels, the smoke on 3.11 and 3.14, the sdist's build, the moved jobs) is
held by ThePackageWorkflow. The parts the rules stand on (the expression reader, the shell reader, the matrix, an
upload path) have tests of their own, and so has the pin tool, offline, with GitHub stood in for.
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
        self.assertIn("rules 1 to 15 hold", out.stdout)
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
        release = yaml.safe_load((WORKFLOWS / "release.yml").read_text(encoding="utf-8"))
        self.assertEqual(list(release["jobs"]), ["phase-5"], "release.yml is only its gate until 5-09")
        self.assertIn("exit 1", release["jobs"]["phase-5"]["steps"][-1]["run"])


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
            # each on its own architecture: the Linux ones in the manylinux_2_28 container of the runner's
            want_container = f"quay.io/pypa/manylinux_2_28_{leg['machine']}:latest" if leg["manylinux"] != "off" else ""
            self.assertEqual(leg["container"], want_container, platform)
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
        installer = self.jobs["installer"]
        self.assertEqual({leg["platform"] for leg in installer["strategy"]["matrix"]["include"]},
                         {"linux-x64", "macos-arm64"})
        self.assertEqual([s["with"]["name"] for s in self.using(installer, "actions/download-artifact")],
                         ["pkg-wheel-${{ matrix.platform }}"])
        self.assertIn('install.sh --from "$(ls wheels/*.whl)" --no-setup', self.runs(installer))
        docker = self.jobs["docker"]
        self.assertEqual([(s["with"]["name"], s["with"]["path"]) for s in self.using(docker, "actions/download-artifact")],
                         [("pkg-wheel-linux-x64", "wheelhouse")])

    def test_the_rules_hold_the_real_file(self):
        plants = (
            ("    runs-on: ${{ matrix.runner }}\n    timeout-minutes: 90\n",
             "    runs-on: ${{ matrix.runner }}\n    timeout-minutes: 90\n    environment: staging\n", 3, "wheels"),
            ('python scripts/ci/check_dist.py sdist dist --version "$PEP440"',
             "python scripts/ci/check_dist.py sdist dist --version ${{ inputs.version }}", 5, "sdist"),
            ("name: pkg-sdist\n          path: dist/*.tar.gz", "name: sdist\n          path: dist/*.tar.gz", 13,
             "sdist"),
            ("          name: pkg-wheel-linux-x64\n          path: wheelhouse",
             "          pattern: helper-*\n          path: wheelhouse", 13, "docker"),
            ("          path: constraints/*.txt", "          path: ${{ runner.temp }}", 9, "constraints"),
        )
        for old, new, rule, job in plants:
            with self.subTest(rule=rule, job=job):
                tree = Tree(self)
                tree.replace("package.yml", old, new)
                found = tree.findings()
                self.assertTrue([f for f in found if f.rule == rule and f.job == job and f.file == "package.yml"],
                                "\n".join(map(str, found)))


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

    def assertPasses(self, tree):
        found = tree.findings()
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
                self.assertPasses(tree)

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
        tree.replace("release.yml", "permissions:\n  contents: read\n",
                     "concurrency:\n  group: tests-release-${{ github.ref }}\n\npermissions:\n  contents: read\n")
        tree.add_job("release.yml", "tests", """
uses: ./.github/workflows/test.yml
permissions:
  contents: read
""")
        self.assertFinds(tree, 4, "release.yml", "tests", "wait on itself")

    # ---- rule 5
    def test_rule_5_inputs_in_a_run(self):
        tree = Tree(self)
        tree.replace("release.yml", "          exit 1\n", "          echo ${{ inputs.version }}\n          exit 1\n")
        self.assertFinds(tree, 5, "release.yml", "phase-5", "inputs.version")

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
                tree.replace("release.yml", "          exit 1\n", f"          exit 1\n      - run: {run}\n")
                self.assertFinds(tree, 8, "release.yml", "phase-5")

    def test_rule_8_a_build_in_a_heredoc_a_shell_reads(self):
        for script in ("bash -s <<EOF\n          cargo build --release\n          EOF",
                       "sh <<< 'cargo build --release'", "pwsh -Command \"cargo build --release\""):
            with self.subTest(script=script):
                tree = Tree(self)
                tree.replace("release.yml", "          exit 1\n",
                             f"          exit 1\n      - run: |\n          {script}\n")
                self.assertFinds(tree, 8, "release.yml", "phase-5", "runs cargo")

    def test_rule_8_and_15_a_step_in_a_shell_this_check_cannot_read(self):
        plant = "      - shell: python\n        run: |\n          import subprocess\n" \
                "          subprocess.run(['cargo', 'build'])\n"
        tree = Tree(self)
        tree.replace("release.yml", "          exit 1\n", "          exit 1\n" + plant)
        self.assertFinds(tree, 8, "release.yml", "phase-5", "'python'")
        tree = Tree(self)  # the job's defaults count too
        tree.replace("release.yml", "    runs-on: ubuntu-latest\n",
                     "    runs-on: ubuntu-latest\n    defaults:\n      run:\n        shell: perl {0}\n")
        self.assertFinds(tree, 8, "release.yml", "phase-5", "'perl'")
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
                tree.replace("release.yml", "          exit 1\n",
                             f"          exit 1\n      - shell: {shell}\n        run: echo done\n")
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
        tree.replace("release.yml", "          exit 1\n", "          exit 1\n      - run: gh workflow run docs.yml\n")
        self.assertFinds(tree, 10, "release.yml", "phase-5")

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

    # ---- rule 12
    def test_rule_12_secrets_inherit_on_a_calling_job(self):
        tree = Tree(self)
        tree.add_job("release.yml", "tests", """
uses: ./.github/workflows/test.yml
permissions:
  contents: read
secrets: inherit
""")
        self.assertFinds(tree, 12, "release.yml", "tests", "inherit")

    def test_rule_12_a_calling_job_without_contents_read(self):
        tree = Tree(self)
        tree.add_job("release.yml", "tests", """
uses: ./.github/workflows/test.yml
""")
        self.assertFinds(tree, 12, "release.yml", "tests", "contents: read")
        tree = Tree(self)
        tree.add_job("release.yml", "tests", """
uses: ./.github/workflows/test.yml
permissions:
  contents: read
with:
  mode: dry-run
""")
        self.assertPasses(tree)

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
        self.assertPasses(tree)
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
        self.assertEqual({f.rule for f in found}, {15}, found)

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
