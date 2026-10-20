"""The server image (design/phase5-release.md P5.7, package 5-06): the Dockerfile, its build context, the Compose file
beside it, Dependabot's hold on its base, and what scripts/ci/check_image.py reads.

What the image does when it runs is check_image.py's to prove, on a runner of each architecture (package.yml's image
job) and under Podman in WSL (5-06's gate). These tests hold what can be read without an engine: one base for both
stages pinned by its index digest; wheels only, at the release's tested versions, nothing compiled and nothing from
Debian's archive; a runtime of the venv alone, run as 10001:10001 with its state in the volume; a health check by the
server's own Python with -I that fails when the server does; a build context of exactly the files the Dockerfile
copies (scripts/check_workflows.py's rule 11 reads workflows, never the Dockerfile, so this module holds its pin).
"""
import http.server
import importlib.util
import json
import os
import re
import subprocess
import sys
import threading
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOCKERFILE = ROOT / "Dockerfile"
CHECK_IMAGE = ROOT / "scripts" / "ci" / "check_image.py"


def load(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module      # dataclasses look their module up while the class is made
    spec.loader.exec_module(module)
    return module


ci = load(CHECK_IMAGE, "check_image_under_test")
BASE = re.compile(r"^python:3\.12-slim-trixie@sha256:[0-9a-f]{64}$")


def instructions(text: str) -> list:
    """The Dockerfile's instructions as (INSTRUCTION, arguments), continuation lines joined and comments dropped."""
    out, pending = [], ""
    for line in text.splitlines():
        if line.lstrip().startswith("#") or (not line.strip() and not pending):
            continue
        if line.rstrip().endswith("\\"):
            pending += line.rstrip()[:-1] + " "
            continue
        whole = (pending + line).strip()
        pending = ""
        word, _, rest = whole.partition(" ")
        out.append((word.upper(), rest.strip()))
    return out


def stages(found: list) -> list:
    """Each stage's name and its instructions, in order."""
    out = []
    for word, rest in found:
        if word == "FROM":
            ref, _, name = rest.partition(" AS ")
            out.append({"base": ref.strip(), "name": name.strip(), "body": []})
        elif out:
            out[-1]["body"].append((word, rest))
    return out


def env_values(body: list) -> dict:
    """The variables a stage's ENV instructions set (``KEY=value`` pairs, a value quoted or plain)."""
    values = {}
    for word, rest in body:
        if word == "ENV":
            for key, quoted, plain in re.findall(r'([A-Z_][A-Z0-9_]*)=(?:"([^"]*)"|(\S+))', rest):
                values[key] = quoted or plain
    return values


class TheDockerfile(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.text = DOCKERFILE.read_text(encoding="utf-8")
        cls.found = instructions(cls.text)
        cls.stages = stages(cls.found)
        cls.runtime = cls.stages[-1]["body"]

    def single(self, body: list, word: str) -> str:
        values = [rest for w, rest in body if w == word]
        self.assertEqual(len(values), 1, f"{word}: {values}")
        return values[0]

    def test_both_stages_are_one_base_pinned_by_its_index_digest(self):
        self.assertEqual([(s["name"]) for s in self.stages], ["builder", "runtime"])
        bases = {s["base"] for s in self.stages}
        self.assertEqual(len(bases), 1, "both stages on one base, so Dependabot moves them together")
        self.assertRegex(bases.pop(), BASE)
        self.assertNotIn("bookworm", self.text)
        # the comment above them says which tag and when
        self.assertIn("python:3.12-slim-trixie (Debian 13, glibc 2.41", self.text)

    def test_every_install_is_a_wheel_at_the_releases_versions_and_nothing_compiles(self):
        runs = " ".join(rest for word, rest in self.found if word == "RUN")
        installs = re.findall(r"pip install [^;&|]*", runs)
        self.assertEqual(len(installs), 2, installs)
        for command in installs:
            self.assertIn("--only-binary=:all:", command)
        deps, citar = installs
        self.assertIn('"$@" -r /tmp/requirements.txt', deps)
        self.assertIn("set -- --constraint /tmp/wheelhouse/constraints.txt", runs)
        self.assertIn("if [ -f /tmp/wheelhouse/constraints.txt ]", runs)
        self.assertIn("python scripts/ci/requirements.py server > /tmp/requirements.txt", runs)
        self.assertTrue(citar.rstrip().endswith("--no-deps --no-index --find-links /tmp/wheelhouse citar"), citar)
        self.assertIn("pip check", runs)
        # nothing from Debian's archive and no compiler: the system packages are the base image's
        for word in ("apt-get", "apt ", "build-essential", "gcc", "libffi-dev", "curl"):
            self.assertNotIn(word, runs)
        self.assertNotIn("--upgrade", runs, "pip, setuptools and wheel are the base's, not the index's of the day")

    def test_the_runtime_holds_the_venv_alone_and_runs_as_10001(self):
        copies = [rest for word, rest in self.runtime if word == "COPY"]
        self.assertEqual(copies, ["--from=builder /opt/venv /opt/venv"])
        self.assertEqual(self.single(self.runtime, "USER"), ci.USER)
        users = self.single(self.runtime, "RUN")
        self.assertIn("groupadd --system --gid 10001 citar", users)
        self.assertIn("useradd --system --uid 10001 --gid citar", users)
        self.assertIn("chown citar:citar /var/lib/citar", users)
        self.assertIn("chmod 0750 /var/lib/citar", users)
        self.assertEqual(ci.STATE_OWNER, "10001:10001:750")
        env = env_values(self.runtime)
        self.assertEqual(env["CITAR_STATE_DIR"], ci.STATE_DIR)
        self.assertEqual(json.loads(self.single(self.runtime, "VOLUME")), [ci.STATE_DIR])
        self.assertEqual(self.single(self.runtime, "WORKDIR"), ci.STATE_DIR)
        self.assertEqual(env["CITAR_PORT"], str(ci.PORT))
        self.assertEqual(self.single(self.runtime, "EXPOSE"), str(ci.PORT))
        self.assertEqual(env["CITAR_HOST"], "0.0.0.0")
        self.assertTrue(env["PATH"].startswith(f"{ci.VENV}/bin:"))
        self.assertEqual(json.loads(self.single(self.runtime, "ENTRYPOINT")), ["citar"])
        self.assertEqual(json.loads(self.single(self.runtime, "CMD")), ["serve"])
        labels = self.single(self.runtime, "LABEL")
        self.assertIn('org.opencontainers.image.source="https://github.com/jprodgers/CITAR"', labels)
        self.assertIn('org.opencontainers.image.licenses="MPL-2.0"', labels)

    def health(self) -> list:
        line = self.single(self.runtime, "HEALTHCHECK")
        self.assertIn("--start-period=40s", line)
        command = json.loads(line[line.index(" CMD ") + len(" CMD "):])
        return command

    def test_the_health_check_is_the_servers_own_python_isolated(self):
        command = self.health()
        self.assertEqual(command[:3], ["python", "-I", "-c"], "-I: nothing in the state directory is imported")
        self.assertIn("/api/auth/config", command[3])
        self.assertIn("CITAR_PORT", command[3])

    def test_the_health_check_passes_on_a_200_and_fails_otherwise(self):
        code = self.health()[3]
        answer = {"status": 200}

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                status = answer["status"] if self.path == "/api/auth/config" else 404
                self.send_response(status)
                self.end_headers()
                self.wfile.write(b"{}")

            def log_message(self, *args):
                pass

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        env = {**os.environ, "CITAR_PORT": str(server.server_address[1])}
        for status, exits in ((200, 0), (500, 1), (404, 1)):
            with self.subTest(status=status):
                answer["status"] = status
                done = subprocess.run([sys.executable, "-I", "-c", code], env=env, capture_output=True, timeout=60)
                self.assertEqual(done.returncode, exits, done.stderr.decode("utf-8", "replace")[-500:])

    def test_the_build_context_is_exactly_what_the_dockerfile_copies(self):
        lines = [line.strip() for line in (ROOT / ".dockerignore").read_text(encoding="utf-8").splitlines()
                 if line.strip() and not line.lstrip().startswith("#")]
        self.assertEqual(lines[0], "*", "an allow-list: everything out, then what the image is built from")
        allowed = {line[1:] for line in lines[1:]}
        self.assertTrue(all(line.startswith("!") for line in lines[1:]), lines)
        copied = set()
        for word, rest in self.found:
            if word == "COPY" and not rest.startswith("--from="):
                copied.update(source.rstrip("*") for source in rest.split()[:-1])
        self.assertEqual(allowed, copied)
        self.assertEqual(copied, {"pyproject.toml", "scripts/ci/requirements.py", "wheelhouse"})

    def test_compose_runs_the_image_package_yml_builds_with_its_state_in_a_volume(self):
        import yaml
        compose = yaml.safe_load((ROOT / "docker-compose.yml").read_text(encoding="utf-8"))
        service = compose["services"]["citar"]
        package = yaml.safe_load((ROOT / ".github" / "workflows" / "package.yml").read_text(encoding="utf-8"))
        tag = package["jobs"]["image"]["env"]["TAG"]
        self.assertEqual(service["image"].split(":")[0], tag.split(":")[0])
        self.assertEqual(service["environment"]["CITAR_STATE_DIR"], ci.STATE_DIR)
        self.assertEqual([v.split(":")[1] for v in service["volumes"]], [ci.STATE_DIR])
        self.assertEqual(service["expose"], [str(ci.PORT)])

    def test_dependabot_moves_the_digest_and_keeps_python_312(self):
        import yaml
        config = yaml.safe_load((ROOT / ".github" / "dependabot.yml").read_text(encoding="utf-8"))
        docker = [u for u in config["updates"] if u["package-ecosystem"] == "docker"]
        self.assertEqual([u["directory"] for u in docker], ["/"])
        self.assertEqual(docker[0]["ignore"], [{"dependency-name": "python", "update-types": [
            "version-update:semver-major", "version-update:semver-minor"]}])


#: An image as `docker image inspect` describes the one the Dockerfile builds.
DOCKER_INSPECT = {
    "Id": "sha256:" + "ab" * 32, "Architecture": "amd64", "Os": "linux", "Created": "2026-10-20T00:00:00Z",
    "Size": 250_000_000,
    "Config": {"User": "10001:10001", "ExposedPorts": {"8765/tcp": {}}, "Volumes": {"/var/lib/citar": {}},
               "Env": ["PATH=/opt/venv/bin:/usr/local/bin:/usr/bin", "CITAR_STATE_DIR=/var/lib/citar",
                       "CITAR_HOST=0.0.0.0", "CITAR_PORT=8765"],
               "Healthcheck": {"Test": ["CMD", "python", "-I", "-c", "pass"]},
               "Labels": {"org.opencontainers.image.version": "0.1.6"}},
}


def inspect(**changes) -> dict:
    data = json.loads(json.dumps(DOCKER_INSPECT))
    for path, value in changes.items():
        *parents, key = path.split("__")
        target = data
        for p in parents:
            target = target[p]
        if value is None:
            target.pop(key, None)
        else:
            target[key] = value
    return data


class CheckImageReadings(unittest.TestCase):
    """check_image.py's parts that need no engine."""

    def setUp(self):
        self.said = []
        original = ci.say
        ci.say = self.said.append
        self.addCleanup(setattr, ci, "say", original)

    def test_the_port_is_on_loopback_alone(self):
        self.assertEqual(ci.parse_port("127.0.0.1:32768\n"), 32768)
        self.assertEqual(ci.parse_port("127.0.0.1:41000\n\n127.0.0.1:41000\n"), 41000)
        for text in ("0.0.0.0:32768\n", "[::]:32768\n", "127.0.0.1:1\n127.0.0.1:2\n", "", "127.0.0.1:x\n",
                     "127.0.0.1:32768\n0.0.0.0:32768\n"):
            with self.subTest(text=text), self.assertRaises(ci.Failed):
                ci.parse_port(text)

    def test_an_image_id_as_either_engine_prints_it(self):
        self.assertEqual(ci.image_id("sha256:" + "ab" * 32), "ab" * 32)
        self.assertEqual(ci.image_id("ab" * 32 + "\n"), "ab" * 32)
        for raw in ("", "sha256:abc", "AB" * 32, "sha512:" + "ab" * 32):
            with self.subTest(raw=raw), self.assertRaises(ci.Failed):
                ci.image_id(raw)

    def test_the_image_as_built(self):
        facts = ci.image_facts(inspect(), "amd64", "0.1.6")
        self.assertEqual((facts["id"], facts["arch"], facts["user"]), ("ab" * 32, "amd64", "10001:10001"))
        self.assertEqual(facts["healthcheck"][:3], ["CMD", "python", "-I"])
        # Podman keeps the health check beside the configuration
        podman = inspect(Id="ab" * 32, Config__Healthcheck=None)
        podman["HealthCheck"] = {"Test": ["CMD", "python", "-I", "-c", "pass"]}
        self.assertEqual(ci.image_facts(podman, "amd64", "0.1.6")["id"], "ab" * 32)
        self.assertEqual(ci.image_facts(inspect(Architecture="arm64"), "arm64", "0.1.6")["arch"], "arm64")
        for name, data, arch, version in (
                ("another architecture", inspect(Architecture="arm64"), "amd64", "0.1.6"),
                ("another system", inspect(Os="windows"), "amd64", "0.1.6"),
                ("root", inspect(Config__User=""), "amd64", "0.1.6"),
                ("a user by name", inspect(Config__User="citar"), "amd64", "0.1.6"),
                ("no volume", inspect(Config__Volumes={}), "amd64", "0.1.6"),
                ("the state elsewhere", inspect(Config__Env=["CITAR_STATE_DIR=/data", "CITAR_PORT=8765"]), "amd64",
                 "0.1.6"),
                ("another port", inspect(Config__ExposedPorts={"80/tcp": {}}), "amd64", "0.1.6"),
                ("no health check", inspect(Config__Healthcheck=None), "amd64", "0.1.6"),
                ("a disabled health check", inspect(Config__Healthcheck={"Test": ["NONE"]}), "amd64", "0.1.6"),
                ("another version", inspect(), "amd64", "0.1.7"),
                ("no version label", inspect(Config__Labels={}), "amd64", "0.1.6")):
            with self.subTest(name), self.assertRaises(ci.Failed):
                ci.image_facts(data, arch, version)

    def test_the_doctors_jobs_line_must_be_verifiable(self):
        ok = "[  ok  ] engine: build 0b412a18fe95\n[  ok  ] jobs: verifiable: the job host passed its canary\n"
        self.assertTrue(ci.jobs_line(ok).startswith("verifiable"))
        for text in ("[ warn ] jobs: not verifiable: a modded ruleset\n", "[ FAIL ] jobs: the job host did not start\n",
                     "[  ok  ] engine: build x\n", ""):
            with self.subTest(text=text), self.assertRaises(ci.Failed):
                ci.jobs_line(text)

    def test_the_images_dependencies_are_the_constraints(self):
        constraints = ["fastapi==0.121.0", "PyYAML==6.0.3", "typing_extensions==4.15.0", "uvloop==0.22.1"]
        frozen = ["fastapi==0.121.0", "pyyaml==6.0.3", "typing-extensions==4.15.0", "uvloop==0.22.1"]
        self.assertEqual(ci.freeze_problems(constraints, frozen), ([], []))
        problems, notes = ci.freeze_problems(constraints, frozen[:-1] + ["uvloop==0.22.2", "six==1.17.0"])
        self.assertEqual(len(problems), 2, problems)
        self.assertTrue(any("six 1.17.0" in p and "nothing of that name" in p for p in problems))
        self.assertTrue(any("uvloop 0.22.2" in p and "0.22.1" in p for p in problems))
        self.assertEqual(notes, [])
        problems, notes = ci.freeze_problems(constraints, frozen[:-1])
        self.assertEqual((problems, notes), ([], ["uvloop 0.22.1 is pinned and not installed"]))

    def test_the_smoke_line(self):
        record = dict.fromkeys(ci.smoke_wheel.RECORD_KEYS, 1)
        out = f"smoke_wheel: ok: ...\n{ci.SMOKE_MARK}{json.dumps(record)}\n"
        self.assertEqual(ci.smoke_record(out), record)
        for text in ("", out + out, f"{ci.SMOKE_MARK}not json\n",
                     f"{ci.SMOKE_MARK}{json.dumps({'smoke': 1})}\n", f"{ci.SMOKE_MARK}[1]\n"):
            with self.subTest(text=text[:40]), self.assertRaises(ci.Failed):
                ci.smoke_record(text)

    def test_the_smokes_script(self):
        script = ci.smoke_script()
        self.assertIn("unset CITAR_HOST CITAR_PORT", script)
        for name, sub in ci.smoke_wheel.STATE_VARIABLES.items():
            want = f"{ci.SMOKE_DIR}/state" + ("" if sub == "." else f"/{sub}")
            self.assertIn(f"export {name}={want}\n", script)
        self.assertIn(f"{ci.VENV}/bin/python -P {ci.CHECK_MOUNT}/ci/smoke_wheel.py inside", script)
        for n in range(1, 5):
            self.assertIn(f'"${n}"', script, "values come in as the shell's arguments")
        self.assertNotRegex(script, r"\{[a-z]+\}", "every field filled in")
        self.assertTrue(script.startswith("set -eu\n"))


class CheckImageCommandLine(unittest.TestCase):
    def run_script(self, *argv, env=None):
        return subprocess.run([sys.executable, str(CHECK_IMAGE), *argv], capture_output=True, text=True,
                              encoding="utf-8", env=env, timeout=60)

    def test_usage_errors_exit_2_before_anything_runs(self):
        common = ["--image", "citar:x", "--arch", "amd64", "--version", "0.1.6", "--label", "ci+abc",
                  "--wheel", "x.whl", "--out", "report.json"]
        done = self.run_script("run", "--engine", "nerdctl", *common)
        self.assertEqual(done.returncode, 2, done.stderr)
        self.assertIn("one of docker, podman", done.stderr)
        bare = {k: v for k, v in os.environ.items() if k.upper() != "PATH"}
        bare["PATH"] = str(Path(sys.executable).parent / "no-such-folder")
        for engine in ("docker", "podman"):
            with self.subTest(engine=engine):
                done = self.run_script("run", "--engine", engine, *common, env=bare)
                self.assertEqual(done.returncode, 2, done.stderr)
                self.assertIn(f"no {engine} on PATH", done.stderr)
                done = self.run_script("save", "--engine", engine, "--image", "citar:x", "--out", "x.tar.zst",
                                       env=bare)
                self.assertEqual(done.returncode, 2, done.stderr)
        done = self.run_script("run", "--engine", "docker", "--arch", "s390x", *common[:2], *common[4:])
        self.assertEqual(done.returncode, 2, done.stderr)


if __name__ == "__main__":
    unittest.main()
