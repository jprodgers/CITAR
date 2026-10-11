"""The QA harness's refusals and its instance (scripts/qa_phase3.py; design/phase3-features.md P3.9.4, package 3-03's
gate 3): the dev server's port, a port in use, a directory that holds anything without the harness's marker, the
owner's dev server's directory, CITAR's own data directory and a source checkout each exit 2 with nothing touched,
before a server could start; a marked directory is emptied and used; every path of the instance is under its
directory. The harness's run itself (``python scripts/qa_phase3.py accounts``) is S8, outside this suite: it starts a
server and waits on the usage ledger's minute.
"""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import importlib.util
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "qa_phase3.py"


def _load():
    spec = importlib.util.spec_from_file_location("qa_phase3_under_test", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


qa = _load()


def _snapshot(folder: Path) -> dict:
    """Every file under ``folder`` with its size and modification time (empty when it does not exist)."""
    if not folder.exists():
        return {}
    return {str(p.relative_to(folder)): (p.stat().st_size, p.stat().st_mtime_ns)
            for p in folder.rglob("*")}


class Refusals(unittest.TestCase):
    """Each refusal through the script itself: exit 2, the reason printed, nothing created or changed."""

    def run_qa(self, *args) -> subprocess.CompletedProcess:
        return subprocess.run([sys.executable, str(SCRIPT), "accounts", *args], cwd=str(ROOT), capture_output=True,
                              text=True, timeout=120)

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="citar-qa-test-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)

    def test_the_dev_servers_port(self):
        target = self.tmp / "new"
        r = self.run_qa("--port", str(qa.DEV_SERVER_PORT), "--dir", str(target))
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertIn("8799", r.stdout)
        self.assertFalse(target.exists(), "nothing was made")

    def test_a_port_in_use(self):
        target = self.tmp / "new"
        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            s.listen(1)
            r = self.run_qa("--port", str(s.getsockname()[1]), "--dir", str(target))
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertIn("already listens", r.stdout)
        self.assertFalse(target.exists())

    def test_a_directory_with_files_and_no_marker(self):
        target = self.tmp / "someones"
        target.mkdir()
        (target / "citar.db").write_bytes(b"not the harness's")
        (target / "saves").mkdir()
        (target / "saves" / "game.citar").write_bytes(b"a save")
        before = _snapshot(target)
        r = self.run_qa("--dir", str(target))
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertIn(qa.MARKER, r.stdout)
        self.assertEqual(_snapshot(target), before, "every file is as it was")

    def test_the_owners_dev_server_directory(self):
        before = _snapshot(qa.OWNER_PLAY_DIR)
        existed = qa.OWNER_PLAY_DIR.exists()
        r = self.run_qa("--dir", str(qa.OWNER_PLAY_DIR))
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertIn("owner's dev server", r.stdout)
        self.assertEqual(qa.OWNER_PLAY_DIR.exists(), existed)
        self.assertEqual(_snapshot(qa.OWNER_PLAY_DIR), before)
        r = self.run_qa("--dir", str(qa.OWNER_PLAY_DIR / "inside"))
        self.assertEqual(r.returncode, 2, "anything inside it too")

    def test_a_part_not_written_yet(self):
        r = subprocess.run([sys.executable, str(SCRIPT), "ledger", "--dir", str(self.tmp / "x")], cwd=str(ROOT),
                           capture_output=True, text=True, timeout=120)
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertIn("not written yet", r.stdout)
        self.assertFalse((self.tmp / "x").exists())


class Directories(unittest.TestCase):
    """``check_dir`` and ``claim`` on their own."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="citar-qa-test-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)

    def test_citars_own_data_directory_and_a_checkout_are_refused(self):
        for where in (qa.default_data_dir(), qa.default_data_dir() / "qa", ROOT, ROOT / "qa-here"):
            with self.subTest(where=str(where)):
                with self.assertRaises(qa.Refused):
                    qa.check_dir(where)
        checkout = self.tmp / "repo"
        (checkout / ".git").mkdir(parents=True)
        with self.assertRaises(qa.Refused):
            qa.check_dir(checkout / "deep" / "qa")

    def test_a_new_an_empty_and_a_marked_directory_are_taken(self):
        new, empty, marked = self.tmp / "new", self.tmp / "empty", self.tmp / "marked"
        empty.mkdir()
        marked.mkdir()
        (marked / qa.MARKER).write_text("x", encoding="utf-8")
        (marked / "server.log").write_text("an earlier run's", encoding="utf-8")
        for folder in (new, empty, marked):
            qa.check_dir(folder)
            qa.claim(folder)
            self.assertEqual(sorted(p.name for p in folder.iterdir()), [qa.MARKER], f"{folder} is emptied and marked")

    def test_the_instance_lives_under_its_directory(self):
        base = self.tmp / "instance"
        env = qa._dev_server().instance_env(base, 18765, server=True, registration="invite", environ={})
        for key in ("CITAR_DATA_DIR", "CITAR_STATE_DIR", "CITAR_SAVE_DIR", "CITAR_CONFIG_DIR", "CITAR_BENCH_DIR"):
            self.assertTrue(qa._inside(env[key], base), f"{key} = {env[key]}")
        self.assertTrue(env["CITAR_DB_URL"].endswith((base / "citar.db").as_posix()))
        self.assertEqual((env["CITAR_MODE"], env["CITAR_REGISTRATION"], env["CITAR_PUBLIC_ORIGIN"]),
                         ("server", "invite", "http://localhost:18765"))

    def test_the_fake_endpoint_records_every_connection(self):
        import httpx
        endpoint = qa.FakeEndpoint()
        try:
            self.assertEqual(endpoint.connections, [])
            r = httpx.get(f"{endpoint.url}/models", timeout=10)
            self.assertEqual(r.json()["data"][0]["id"], endpoint.MODEL)
            self.assertEqual(len(endpoint.connections), 1)
            self.assertEqual(endpoint.requests[0]["path"], "/v1/models")
        finally:
            endpoint.close()


class CreateUser(unittest.TestCase):
    """``admin_cli create-user``, which the harness makes its accounts with: active, verified, any role, the password
    from standard input; refused for a weak password or a taken handle."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="citar-cli-test-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.env = qa._dev_server().instance_env(self.tmp, 18766, server=True, environ=dict(os.environ))

    def cli(self, *args, stdin=""):
        return subprocess.run([sys.executable, "-m", "citar.server.admin_cli", *args], cwd=str(ROOT), env=self.env,
                              input=stdin, capture_output=True, text=True, timeout=180)

    def test_accounts_of_each_role_and_the_refusals(self):
        r = self.cli("create-user", "--handle", "qa-mod", "--email", "qa-mod@example.com", "--role", "moderator",
                     "--password-stdin", stdin="a long enough phrase\n")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        r = self.cli("create-user", "--handle", "qa-admin", "--email", "qa-admin@example.com", "--role", "admin",
                     "--password-stdin", stdin="another long phrase\n")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        listing = self.cli("list-users").stdout
        self.assertRegex(listing, r"qa-mod\s+moderator\s+active")
        self.assertRegex(listing, r"qa-admin\s+admin\s+active")
        taken = self.cli("create-user", "--handle", "qa-mod", "--email", "x@example.com", "--password-stdin",
                         stdin="a long enough phrase\n")
        self.assertEqual(taken.returncode, 1)
        weak = self.cli("create-user", "--handle", "qa-weak", "--email", "w@example.com", "--password-stdin",
                        stdin="short\n")
        self.assertEqual(weak.returncode, 1)
        self.assertNotIn("qa-weak", self.cli("list-users").stdout)


if __name__ == "__main__":
    unittest.main()
