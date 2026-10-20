"""scripts/ci/gate_run.py, the harness every package's gates run a release helper or a server through on the laptop
(design/phase5-release.md P5.18.1 rules 2 and 11, package 5-03): it refuses a helper without --home inside the
package's folder, one that names a file outside it, or one that would reach beyond loopback or leave something
behind, and a server not bound to 127.0.0.1, on the owner's port, or with its state, its database or its API keys
outside the folder; what it runs gets an environment with no inherited CITAR_ variable, and a server no OS credential
store and per-user bases of its own; and a server it stops leaves no process behind.

Stand-ins play the helper and the server (Python scripts), so these run on every OS and touch nothing real.
"""
import importlib.util
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "ci" / "gate_run.py"


def load():
    spec = importlib.util.spec_from_file_location("citar_ci_gate_run_tests", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


gate_run = load()

#: A stand-in helper: writes what it was given (its arguments and the CITAR_ variables and per-user base it sees)
#: into its home, and with `sleep` starts a child and waits, so a timeout has a tree to stop.
HELPER = """import json, os, subprocess, sys, time
from pathlib import Path
args = sys.argv[1:]
home = Path(args[args.index("--home") + 1])
home.mkdir(parents=True, exist_ok=True)
seen = {k: v for k, v in os.environ.items() if k.startswith("CITAR_")}
seen["LOCALAPPDATA"] = os.environ.get("LOCALAPPDATA")
seen["XDG_DATA_HOME"] = os.environ.get("XDG_DATA_HOME")
(home / "seen.json").write_text(json.dumps({"args": args, "env": seen}))
if "sleep" in args:
    child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(600)"])
    (home / "child.pid").write_text(str(child.pid))
    time.sleep(600)
sys.exit(int(args[args.index("--exit") + 1]) if "--exit" in args else 0)
"""

#: A stand-in server: binds what CITAR_HOST and CITAR_PORT say, records its environment in its state folder, starts
#: a child that would outlive it, and serves until it is stopped.
SERVER = """import http.server, json, os, subprocess, sys
from pathlib import Path
state = Path(os.environ["CITAR_STATE_DIR"])
child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(600)"])
seen = {k: v for k, v in os.environ.items() if k.startswith("CITAR_")}
for name in ("PYTHON_KEYRING_BACKEND", "APPDATA", "LOCALAPPDATA", "XDG_CONFIG_HOME", "XDG_DATA_HOME"):
    seen[name] = os.environ.get(name)
(state / "seen.json").write_text(json.dumps({"env": seen, "child": child.pid, "pid": os.getpid()}))
class Answer(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(404)
        self.end_headers()
    def log_message(self, *args):
        pass
http.server.HTTPServer((os.environ["CITAR_HOST"], int(os.environ["CITAR_PORT"])), Answer).serve_forever()
"""

#: What the real server's key store makes of the environment it is given (citar/keystore.py): where its encrypted
#: file is, whether it has an OS credential store, and the keyring backend keyring itself chose.
KEYSTORE_PROBE = """import json, sys
sys.path.insert(0, sys.argv[1])
from citar import keystore
try:
    import keyring
    chosen = type(keyring.get_keyring()).__module__ + "." + type(keyring.get_keyring()).__name__
except ImportError:
    chosen = None
print(json.dumps({"file": str(keystore._file_path()), "keyring": keystore._keyring() is not None,
                  "name": keystore._keyring_name(keystore._keyring()), "chosen": chosen,
                  "available": keystore.backends()["keyring"]["available"]}))
"""


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Base(unittest.TestCase):
    def setUp(self):
        self.under = Path(tempfile.mkdtemp(prefix="citar-gate-run-"))
        self.addCleanup(shutil.rmtree, self.under, ignore_errors=True)
        self.helper = self.under / "helper.py"
        self.helper.write_text(HELPER, encoding="utf-8")
        self.home = self.under / "home"

    def refused(self, why, args, under=None, binary=None):
        with self.assertRaisesRegex(gate_run.Refused, why):
            gate_run.check_helper(list(args), gate_run.check_under(under or self.under), binary)


class TheHelperIsRefused(Base):
    def test_without_a_home_with_two_or_with_one_outside_the_folder(self):
        self.refused("--home DIR, given once", ["--version"])
        self.refused("--home DIR, given once", ["--version", "--home", str(self.home), "--home", str(self.home)])
        self.refused("is not inside", ["--version", "--home", tempfile.gettempdir()])
        self.refused("is not inside", ["--version", "--home", str(self.under)])
        self.refused("is not inside", ["--version", "--home", str(self.under / ".." / "elsewhere")])
        base = gate_run.platform_base()
        if base is not None:
            self.refused("is not inside", ["--version", "--home", str(base / "helper")])

    def test_an_abbreviation_of_an_option_it_reads(self):
        for word in ("--hom", "--ho=x", "--serv", "--base", "--conf", "--det", "--superv", "--hard", "--ca"):
            with self.subTest(word=word):
                self.refused("write it in full", ["--home", str(self.home), word, "x"])

    def test_a_file_it_names_outside_the_folder(self):
        # scan FILE writes the hardware report there; update apply and rollback put a binary in the binary's place
        home = ["--home", str(self.home)]
        outside = Path(tempfile.gettempdir()) / "citar-gate-run-outside.json"
        self.refused("scan's FILE .* is not inside", home + ["scan", str(outside)])
        self.refused("scan's FILE .* is not inside", ["scan", "--home", str(self.home), "--no-power", str(outside)])
        for name in ("--hardware", "--ca-file", "--config"):
            with self.subTest(option=name):
                self.refused(f"{name} .* is not inside", home + [name, str(outside), "--version"])
                self.refused(f"{name} .* is not inside", home + [f"{name}={outside}", "--version"])
        elsewhere = Path(tempfile.gettempdir()) / "citar-helper-elsewhere"
        for words in (["update", "apply"], ["update", "rollback"]):
            with self.subTest(words=words):
                self.refused("replaces the binary it runs", home + words)
                self.refused(f"the binary {' '.join(words)} replaces, .* is not inside", home + words,
                             binary=elsewhere)
                self.assertEqual(gate_run.check_helper(home + words, gate_run.check_under(self.under),
                                                       self.under / "bin" / "citar-helper"), gate_run.real(self.home))
        under = gate_run.check_under(self.under)
        for args in (["scan"], ["scan", str(self.under / "hw.json")], ["--hardware", str(self.under / "hw.json")],
                     ["--ca-file", str(self.under / "ca.pem"), "--version"], ["update", "check"]):
            with self.subTest(args=args):
                self.assertEqual(gate_run.check_helper(home + args, under, elsewhere), gate_run.real(self.home))

    def test_anything_beyond_loopback_or_left_behind(self):
        home = ["--home", str(self.home)]
        self.refused("not a loopback address", home + ["--server", "https://citar.jimmieprodgers.com"])
        self.refused("not a loopback address", home + ["--server=wss://example.com:443"])
        self.refused("not a loopback address", home + ["--base-url", "http://192.168.1.20:1234/v1"])
        self.refused("outlives the run", home + ["run", "--detach"])
        self.refused("outlives the run", home + ["--supervisor"])
        for words in (["service", "install", "--user"], ["service", "uninstall", "--system"], ["service", "run"]):
            with self.subTest(words=words):
                self.refused("only --dry-run", home + words)
        self.refused("is not inside", home + ["--config", str(Path(tempfile.gettempdir()) / "x.json")])
        self.home.mkdir()
        (self.home / "helper.json").write_text(json.dumps({"server_url": "https://citar.example.com", "token": "t"}))
        self.refused("helper.json's server_url", home + ["update", "check"])

    def test_what_it_allows(self):
        home = ["--home", str(self.home)]
        for args in (["--version"], ["--self-test"], ["update", "check"], ["service", "install", "--dry-run"],
                     ["service", "status"], ["--server", "http://127.0.0.1:9", "--token", "x"],
                     ["--server", "ws://localhost:8765"], ["--base-url", "http://[::1]:1234/v1"],
                     ["--name", "service", "--self-test"]):
            with self.subTest(args=args):
                self.assertEqual(gate_run.check_helper(home + args, gate_run.check_under(self.under)),
                                 gate_run.real(self.home))

    def test_a_folder_that_is_the_checkout_or_holds_it_or_is_missing(self):
        for under, why in ((ROOT, "overlaps this checkout"), (ROOT / "scripts", "overlaps this checkout"),
                           (ROOT.parent, "overlaps this checkout"), (self.under / "missing", "is not a folder"),
                           (None, "give --under")):
            with self.subTest(under=under), self.assertRaisesRegex(gate_run.Refused, why):
                gate_run.check_under(under)


class TheHelperRuns(Base):
    def test_with_its_home_and_no_inherited_citar_variable(self):
        os.environ["CITAR_WORKER_SERVER"] = "https://citar.example.com"
        self.addCleanup(os.environ.pop, "CITAR_WORKER_SERVER", None)
        # -S: the stand-in reads no site file (a venv's citar-dev.pth sets CITAR_EXT_DIR in every interpreter it starts)
        done = gate_run.run_helper(sys.executable, ["-S", str(self.helper), "--home", str(self.home), "--exit", "3"],
                                   self.under, timeout=60)
        self.assertEqual(done.returncode, 3, done.stderr)
        seen = json.loads((self.home / "seen.json").read_text())
        self.assertEqual(seen["env"].pop("CITAR_DATA_DIR"), str(gate_run.real(self.home)))
        base = seen["env"].pop("LOCALAPPDATA" if os.name == "nt" else "XDG_DATA_HOME")
        if base is not None:
            self.assertTrue(gate_run.within(Path(base), self.under), base)
        self.assertEqual({k: v for k, v in seen["env"].items() if k.startswith("CITAR_")}, {})

    def test_the_command_line_refuses_and_passes_the_exit_status_through(self):
        out = subprocess.run([sys.executable, str(SCRIPT), "helper", "--under", str(self.under), "--",
                              sys.executable, str(self.helper), "--version"], capture_output=True, text=True)
        self.assertEqual(out.returncode, 2, out.stdout + out.stderr)
        self.assertIn("gate_run: refused:", out.stderr)
        self.assertFalse(self.home.exists(), "nothing ran")
        out = subprocess.run([sys.executable, str(SCRIPT), "helper", "--under", str(self.under), "--",
                              sys.executable, str(self.helper), "--home", str(self.home), "--exit", "5"],
                             capture_output=True, text=True)
        self.assertEqual(out.returncode, 5, out.stdout + out.stderr)
        env = {k: v for k, v in os.environ.items() if k != "CARGO_TARGET_DIR"}
        out = subprocess.run([sys.executable, str(SCRIPT), "helper", "--", sys.executable, str(self.helper), "--home",
                              str(self.home)], capture_output=True, text=True, env=env)
        self.assertEqual(out.returncode, 2, "no --under and no CARGO_TARGET_DIR")
        # a report scan would write outside the folder: refused, and nothing is written there
        outside = Path(tempfile.mkdtemp(prefix="citar-gate-run-outside-"))
        self.addCleanup(shutil.rmtree, outside, ignore_errors=True)
        out = subprocess.run([sys.executable, str(SCRIPT), "helper", "--under", str(self.under), "--",
                              sys.executable, str(self.helper), "--home", str(self.home / "scan"), "scan",
                              str(outside / "hw.json")], capture_output=True, text=True)
        self.assertEqual(out.returncode, 2, out.stdout + out.stderr)
        self.assertIn("scan's FILE", out.stderr)
        self.assertEqual(list(outside.iterdir()), [], "nothing ran")
        self.assertFalse((self.home / "scan").exists(), "nothing ran")

    def test_a_timeout_stops_the_whole_tree(self):
        began = time.monotonic()
        done = gate_run.run_helper(sys.executable, [str(self.helper), "--home", str(self.home), "sleep"],
                                   self.under, timeout=8)
        self.assertEqual(done.returncode, gate_run.EXIT_TIMEOUT)
        self.assertIn("did not end within", done.stderr)
        self.assertLess(time.monotonic() - began, 120)
        child = int((self.home / "child.pid").read_text())
        deadline = time.monotonic() + 30
        while child in gate_run.process_table() and time.monotonic() < deadline:
            time.sleep(0.5)
        self.assertNotIn(child, gate_run.process_table(), "the helper's child was stopped too")


class TheProcessTable(unittest.TestCase):
    """Each process with its parent and its start, read on every OS (Windows' snapshot, Linux's /proc, macOS's ps), so
    a process that took a stopped one's id is not counted as left, nor as one of a tree."""

    def test_this_process_its_parent_and_its_start(self):
        table = gate_run.process_table()
        parent, began = table[os.getpid()]
        self.assertEqual(parent, os.getppid())
        self.assertNotEqual(began, 0, "the start is read on this OS")
        self.assertEqual(gate_run.left_of({os.getpid(): began}), [os.getpid()])
        # the same id with another start is another process: one that took the id of a process that ended
        self.assertEqual(gate_run.left_of({os.getpid(): began - 1}), [])
        self.assertEqual(gate_run.left_of({os.getpid(): began + 1}), [])

    def test_a_tree_holds_a_child_started_after_its_parent(self):
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
        self.addCleanup(child.wait, 30)
        self.addCleanup(child.kill)
        deadline = time.monotonic() + 30
        tree = gate_run.snapshot_tree(os.getpid())
        while child.pid not in tree and time.monotonic() < deadline:
            time.sleep(0.2)
            tree = gate_run.snapshot_tree(os.getpid())
        self.assertIn(child.pid, tree)
        self.assertGreaterEqual(tree[child.pid], tree[os.getpid()])
        self.assertNotEqual(tree[child.pid], 0)


class TheServer(Base):
    def setUp(self):
        super().setUp()
        self.server = self.under / "server.py"
        self.server.write_text(SERVER, encoding="utf-8")
        self.state = self.under / "state"

    def refused(self, why, port=None, state=None, extra=(), command=()):
        with self.assertRaisesRegex(gate_run.Refused, why):
            gate_run.check_server(port or free_port(), state or self.state, gate_run.check_under(self.under),
                                  list(extra), [sys.executable, str(self.server), *command])

    def test_the_owners_port_another_host_or_port_and_state_outside_are_refused(self):
        self.refused("owner's server", port=gate_run.OWNER_PORT)
        self.refused("binds 127.0.0.1 only", extra=["CITAR_HOST=0.0.0.0"])
        self.refused("binds 127.0.0.1 only", command=["--host", "0.0.0.0"])
        self.refused("binds 127.0.0.1 only", command=["--host=::"])
        self.refused("the port is --port's", port=18799, extra=["CITAR_PORT=8765"])
        self.refused("is not --port's", port=18799, command=["--port", "8765"])
        self.refused("the state variables are", extra=["CITAR_DATA_DIR=C:/elsewhere"])
        self.refused("is not inside", state=Path(tempfile.gettempdir()) / "state-elsewhere")
        self.refused("above 1024", port=80)
        with socket.socket() as busy:
            busy.bind(("127.0.0.1", 0))
            busy.listen()
            self.refused("in use", port=busy.getsockname()[1])
        out = subprocess.run([sys.executable, str(SCRIPT), "server", "--under", str(self.under), "--state",
                              str(self.state), "--", sys.executable, str(self.server)], capture_output=True, text=True)
        self.assertEqual(out.returncode, 2, "--port is required")

    def test_a_key_store_database_or_per_user_base_outside_the_folder_is_refused(self):
        outside = Path(tempfile.gettempdir()) / "citar-gate-run-outside"
        for item in ("PYTHON_KEYRING_BACKEND=keyring.backends.Windows.WinVaultKeyring",
                     "PYTHON_KEYRING_BACKEND=keyring.backends.fail.Keyring", f"APPDATA={outside}",
                     f"LOCALAPPDATA={outside}", f"XDG_CONFIG_HOME={outside}", f"XDG_DATA_HOME={outside}"):
            with self.subTest(item=item):
                self.refused("the key store and the per-user bases are --state's", extra=[item])
        self.refused("key file .* is not inside", extra=[f"CITAR_KEYS_FILE={outside / 'keys.enc'}"])
        for url in (f"sqlite:///{(outside / 'citar.db').as_posix()}", "postgresql://citar@127.0.0.1/citar",
                    "postgresql+psycopg://citar@db.example.com/citar", f"sqlite:///file:{self.under.as_posix()}/x.db"
                    "?mode=rwc&uri=true", "sqlite://host/x.db", str(self.under / "citar.db")):
            with self.subTest(url=url):
                self.refused("CITAR_DB_URL", extra=[f"CITAR_DB_URL={url}"])
        under = gate_run.check_under(self.under)
        for item in (f"CITAR_DB_URL=sqlite:///{(self.under / 'db' / 'citar.db').as_posix()}", "CITAR_DB_URL=sqlite://",
                     "CITAR_DB_URL=sqlite:///:memory:", f"CITAR_KEYS_FILE={self.under / 'keys' / 'keys.enc'}"):
            with self.subTest(item=item):
                env = gate_run.check_server(free_port(), self.state, under, [item], [sys.executable])
                name, _, value = item.partition("=")
                self.assertEqual(env[name], value)

    def test_the_key_store_and_the_per_user_bases_are_the_states(self):
        # what citar/keystore.py makes of the server's environment: its file under --state, no OS credential store
        os.environ["CITAR_KEYS_FILE"] = str(Path(tempfile.gettempdir()) / "keys.enc")  # inherited: dropped
        self.addCleanup(os.environ.pop, "CITAR_KEYS_FILE", None)
        os.environ["PYTHON_KEYRING_BACKEND"] = "keyring.backends.chainer.ChainerBackend"  # inherited: replaced
        self.addCleanup(os.environ.pop, "PYTHON_KEYRING_BACKEND", None)
        env = gate_run.check_server(free_port(), self.state, gate_run.check_under(self.under), [], [sys.executable])
        state = gate_run.real(self.state)
        self.assertEqual(Path(env["CITAR_KEYS_FILE"]), state / "config" / "keys.enc")
        self.assertEqual(env["PYTHON_KEYRING_BACKEND"], "keyring.backends.fail.Keyring")
        for name, sub in gate_run.USER_BASES.items():
            self.assertEqual(Path(env[name]), state / "user" / sub, name)
            self.assertTrue(Path(env[name]).is_dir(), name)
        self.assertEqual(set(gate_run.USER_BASES), {"APPDATA", "LOCALAPPDATA"} if os.name == "nt" else
                         set() if sys.platform == "darwin" else {"XDG_CONFIG_HOME", "XDG_DATA_HOME"})
        out = subprocess.run([sys.executable, "-c", KEYSTORE_PROBE, str(ROOT)], env=env, capture_output=True,
                             text=True, timeout=120)
        self.assertEqual(out.returncode, 0, out.stderr)
        seen = json.loads(out.stdout.strip().splitlines()[-1])
        self.assertTrue(gate_run.within(Path(seen["file"]), state), seen["file"])
        self.assertFalse(seen["keyring"], seen)
        self.assertFalse(seen["available"], "the Servers page offers no OS credential store")
        self.assertEqual(seen["name"], "OS credential store (unavailable)")
        if seen["chosen"] is not None:
            self.assertEqual(seen["chosen"], "keyring.backends.fail.Keyring")

    def test_a_ready_path_git_bash_rewrote_is_refused(self):
        for ready in ("C:/Program Files/Git/api/auth/config", "api/auth/config", "/api/x y"):
            with self.subTest(ready=ready), self.assertRaisesRegex(gate_run.Refused, "not a URL's path"):
                gate_run.serve([sys.executable, str(self.server)], free_port(), self.state, self.under, [], ready,
                               None, 60)
        self.assertFalse((self.state / "seen.json").exists(), "nothing started")

    def test_a_failure_while_it_waits_still_stops_the_server(self):
        port = free_port()

        def broken(url):
            if (self.state / "seen.json").exists():
                raise RuntimeError("the readiness check broke")
            return False

        original = gate_run.answers
        gate_run.answers = broken
        self.addCleanup(setattr, gate_run, "answers", original)
        code = gate_run.serve([sys.executable, str(self.server)], port, self.state, self.under, [], "/x", None, 120)
        self.assertEqual(code, 1)
        seen = json.loads((self.state / "seen.json").read_text())
        table = gate_run.process_table()
        self.assertNotIn(seen["pid"], table, "the server was stopped")
        self.assertNotIn(seen["child"], table, "and its child")
        self.assertFalse(gate_run.port_open(port))

    def test_it_serves_on_loopback_and_stops_leaving_nothing(self):
        port = free_port()
        os.environ["CITAR_HOST"] = "0.0.0.0"  # an inherited host never reaches the server
        self.addCleanup(os.environ.pop, "CITAR_HOST", None)
        proc = subprocess.Popen([sys.executable, str(SCRIPT), "server", "--under", str(self.under), "--port",
                                 str(port), "--state", str(self.state), "--env", "CITAR_MODE=server", "--ready", "/x",
                                 "--timeout", "300", "--", sys.executable, str(self.server)],
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        self.addCleanup(lambda: proc.poll() is None and proc.kill())
        lines = []
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            line = proc.stdout.readline()
            if not line:
                break
            lines.append(line)
            if line.startswith("gate_run: ready"):
                break
        self.assertTrue(lines and lines[-1].startswith(f"gate_run: ready http://127.0.0.1:{port}/x"), "".join(lines))
        seen = json.loads((self.state / "seen.json").read_text())
        env = seen["env"]
        self.assertEqual((env["CITAR_HOST"], env["CITAR_PORT"], env["CITAR_MODE"]), ("127.0.0.1", str(port), "server"))
        for name, sub in gate_run.STATE_VARIABLES.items():
            self.assertEqual(Path(env[name]), gate_run.real(self.state) / sub, name)
            self.assertTrue(Path(env[name]).is_dir(), name)
        self.assertEqual(Path(env["CITAR_KEYS_FILE"]), gate_run.real(self.state) / "config" / "keys.enc")
        self.assertEqual(env["PYTHON_KEYRING_BACKEND"], "keyring.backends.fail.Keyring")
        for name, sub in gate_run.USER_BASES.items():
            self.assertEqual(Path(env[name]), gate_run.real(self.state) / "user" / sub, name)
        (self.state / "gate_run.stop").write_text("")
        rest, _ = proc.communicate(timeout=180)
        self.assertEqual(proc.returncode, 0, "".join(lines) + rest)
        self.assertIn("stopped on request", rest)
        self.assertIn("none left", rest)
        table = gate_run.process_table()
        self.assertNotIn(seen["child"], table, "the server's child was stopped")
        self.assertNotIn(seen["pid"], table)
        self.assertFalse(gate_run.port_open(port))
        self.assertFalse((self.state / "gate_run.stop").exists(), "the stop file is taken away")


class Addresses(unittest.TestCase):
    def test_loopback_only(self):
        for url in ("http://127.0.0.1:9", "ws://127.0.0.2/x", "localhost:1234", "https://LOCALHOST", "http://[::1]:9",
                    "127.0.0.1:8765"):
            self.assertTrue(gate_run.is_loopback(url), url)
        for url in ("https://citar.jimmieprodgers.com", "http://10.0.0.5", "http://0.0.0.0:9", "http://[::]:9",
                    "http://127.0.0.1.example.com", "", "http://"):
            self.assertFalse(gate_run.is_loopback(url), url)
