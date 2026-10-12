"""The QA harness's refusals and its instance (scripts/qa_phase3.py; design/phase3-features.md P3.9.4, package 3-03's
gate 3): the dev server's port, a port in use, a directory that holds anything without the harness's marker, the
owner's dev server's directory, CITAR's own data directory and a source checkout each exit 2 with nothing touched,
before a server could start; a marked directory is emptied and used; every path of the instance is under its
directory. The harness's run itself (``python scripts/qa_phase3.py accounts``) is S8, outside this suite: it starts a
server and waits on the usage ledger's minute.
"""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import importlib.util
import io
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock

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
        """The real ``C:/dev/citar-play``, through the script: refused, and nothing of the harness's there. Its
        files are not compared: the owner's dev server may be writing them (autosaves, the database's WAL, the
        usage ledger) while this runs. ``test_the_owners_directory_is_left_as_it_was`` compares every file of a
        stand-in."""
        def names() -> set:
            return {p.name for p in qa.OWNER_PLAY_DIR.iterdir()} if qa.OWNER_PLAY_DIR.is_dir() else set()
        before, existed = names(), qa.OWNER_PLAY_DIR.exists()
        for where in (qa.OWNER_PLAY_DIR, qa.OWNER_PLAY_DIR / "inside"):
            with self.subTest(where=str(where)):
                r = self.run_qa("--dir", str(where))
                self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
                self.assertIn("owner's dev server", r.stdout)
        after = names()
        self.assertEqual(qa.OWNER_PLAY_DIR.exists(), existed)
        self.assertFalse((after - before) & {qa.MARKER, "server.log", "qa-accounts.json", "inside"},
                         "the harness made nothing there")
        if before:
            self.assertTrue(after & before, "and emptied nothing")

    def test_the_owners_directory_is_left_as_it_was(self):
        """The same refusal with a stand-in for the owner's directory, every file compared before and after."""
        stand_in = self.tmp / "citar-play"
        (stand_in / "saves").mkdir(parents=True)
        (stand_in / "citar.db").write_bytes(b"the owner's")
        (stand_in / "saves" / "game.citar").write_bytes(b"a save")
        (stand_in / qa.MARKER).write_text("even marked", encoding="utf-8")
        before = _snapshot(stand_in)
        with mock.patch.object(qa, "OWNER_PLAY_DIR", stand_in), redirect_stdout(io.StringIO()) as out:
            code = qa.main(["accounts", "--dir", str(stand_in)])
        self.assertEqual(code, 2, out.getvalue())
        self.assertIn("owner's dev server", out.getvalue())
        self.assertEqual(_snapshot(stand_in), before, "every file is as it was")

    def test_a_part_not_written_yet(self):
        r = subprocess.run([sys.executable, str(SCRIPT), "hybrid", "--dir", str(self.tmp / "x")], cwd=str(ROOT),
                           capture_output=True, text=True, timeout=120)
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertIn("not written yet", r.stdout)
        self.assertFalse((self.tmp / "x").exists())


class Failures(unittest.TestCase):
    """Once the directory is the harness's, nothing is a refusal: a directory that cannot be emptied, a server
    that does not start, an account that cannot be made and a part that raises are each a failed check, with
    ``qa-<part>.json`` written, exit 1 (in-process, the server and the accounts stubbed)."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="citar-qa-test-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.dir = self.tmp / "qa"

    def main(self) -> tuple:
        with redirect_stdout(io.StringIO()) as out:
            code = qa.main(["accounts", "--dir", str(self.dir)])
        evidence = self.dir / "qa-accounts.json"
        return code, out.getvalue(), json.loads(evidence.read_text(encoding="utf-8")) if evidence.exists() else None

    def failed(self, evidence: dict) -> list:
        return [c for c in evidence["checks"] if not c["ok"]]

    def test_a_directory_that_cannot_be_emptied(self):
        self.dir.mkdir()
        (self.dir / qa.MARKER).write_text("x", encoding="utf-8")
        (self.dir / "server.log").write_text("held open by a kept server", encoding="utf-8")
        with mock.patch.object(qa.shutil, "rmtree", side_effect=PermissionError("in use")):
            code, out, evidence = self.main()
        self.assertEqual(code, 1, out)
        self.assertNotIn("refused", out)
        [check] = self.failed(evidence)
        self.assertIn("could not empty", check["detail"])

    def test_a_server_that_does_not_start(self):
        with mock.patch.object(qa.Server, "start", side_effect=qa.HarnessError("the server did not start (exit 3)")):
            code, out, evidence = self.main()
        self.assertEqual(code, 1, out)
        self.assertNotIn("refused", out)
        [check] = self.failed(evidence)
        self.assertIn("did not start", check["detail"])
        self.assertTrue((self.dir / qa.MARKER).is_file(), "the directory stays the harness's, for the next run")

    def test_an_account_that_cannot_be_made(self):
        made = subprocess.CompletedProcess([], 1, stdout="", stderr="That handle is taken.")
        with mock.patch.object(qa.Server, "start", return_value=None), \
                mock.patch.object(qa.Server, "cli", return_value=made):
            code, out, evidence = self.main()
        self.assertEqual(code, 1, out)
        [check] = self.failed(evidence)
        self.assertIn("create-user qa-admin failed", check["detail"])

    def test_a_part_that_raises_keeps_the_checks_before_it(self):
        def part(h):
            h.report.check("a first check", True)
            raise KeyError("id")
        accounts = mock.patch.object(qa, "make_accounts", return_value={})
        with mock.patch.object(qa.Server, "start", return_value=None), accounts, \
                mock.patch.dict(qa.PARTS, {"accounts": part}):
            code, out, evidence = self.main()
        self.assertEqual(code, 1, out)
        self.assertIn("a first check", [c["check"] for c in evidence["checks"] if c["ok"]])
        [check] = self.failed(evidence)
        self.assertEqual(check["check"], "the accounts part ran to its end")
        self.assertIn("KeyError", check["detail"])


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


class ScriptedEndpoint(unittest.TestCase):
    """The fake endpoint's scripted policy (P3.9.4): valid tool calls from the request's own tools and messages."""

    TOOLS = [{"type": "function", "function": {"name": n, "parameters": {"type": "object"}}}
             for n in ("get_units", "get_cities", "get_empire", "get_players", "found_city", "unit_order", "set_research",
                       "set_production", "open_negotiation", "un_vote", "end_turn")]

    def turn(self, endpoint, prompt: str, results=None) -> list:
        """The calls the policy answers a turn with: a briefing, then the reads' results when ``results`` is given."""
        messages = [{"role": "system", "content": "s"}, {"role": "user", "content": prompt}]
        if results is not None:
            calls = [{"id": f"c{i}", "type": "function", "function": {"name": n, "arguments": "{}"}}
                     for i, n in enumerate(results)]
            messages.append({"role": "assistant", "content": "", "tool_calls": calls})
            messages += [{"role": "tool", "tool_call_id": f"c{i}", "content": json.dumps(v)}
                         for i, v in enumerate(results.values())]
        _reply, calls = endpoint.scripted_reply({"model": "m", "messages": messages, "tools": self.TOOLS})
        return [(c["function"]["name"], json.loads(c["function"]["arguments"])) for c in calls]

    def test_a_turn_reads_orders_trades_and_ends(self):
        endpoint = qa.FakeEndpoint(delay=0)
        try:
            self.assertEqual([n for n, _ in self.turn(endpoint, "Your turn 1.")],
                             ["get_units", "get_cities", "get_empire", "get_players"])
            results = {"get_units": [{"id": 1, "type": "Settler", "class": "civilian"},
                                     {"id": 2, "type": "Warrior", "class": "melee", "activity": None}],
                       "get_cities": [], "get_empire": {"id": 0, "name": "England", "researching": None},
                       "get_players": [{"id": 0, "kind": "major", "met": True, "alive": True, "name": "England"},
                                       {"id": 1, "kind": "major", "met": True, "alive": True, "name": "Japan"},
                                       {"id": 2, "kind": "major", "met": False, "alive": True, "name": "Unknown"}]}
            calls = dict(self.turn(endpoint, "Your turn 1.", results))
            self.assertEqual(calls["found_city"], {"unit_id": 1})
            self.assertEqual(calls["unit_order"], {"unit_id": 2, "order": "explore"})
            self.assertEqual(calls["set_research"], {"tech": "pottery"})
            self.assertEqual(calls["open_negotiation"]["to"], 1, "a trade with the civilization it met")
            self.assertEqual(calls["open_negotiation"]["give"], [{"type": "share_map"}])
            again = dict(self.turn(endpoint, "Your turn 2.", results))
            self.assertNotIn("open_negotiation", again, "one trade (max_trades)")
            self.assertIn("un_vote", dict(self.turn(endpoint, "The United Nations vote is open.", results)))
            messages = [{"role": "user", "content": "go"}, {"role": "assistant", "content": "", "tool_calls": []},
                        {"role": "assistant", "content": ""}]
            _reply, calls = endpoint.scripted_reply({"messages": messages, "tools": self.TOOLS})
            self.assertEqual([c["function"]["name"] for c in calls], ["end_turn"])
        finally:
            endpoint.close()

    def test_it_answers_the_negotiation_it_is_asked_about(self):
        endpoint = qa.FakeEndpoint(delay=0)
        try:
            tools = [{"type": "function", "function": {"name": "respond_negotiation"}}]
            prompt = ('Answer negotiation #7: {"current_proposal": {"you_give": [{"type": "gold"}], '
                      '"you_receive": [{"type": "share_map"}, {"type": "embassy"}]}}')
            _reply, calls = endpoint.scripted_reply({"messages": [{"role": "user", "content": prompt}], "tools": tools})
            self.assertEqual(json.loads(calls[0]["function"]["arguments"])["negotiation_id"], 7)
            self.assertEqual(json.loads(calls[0]["function"]["arguments"])["action"], "accept")
        finally:
            endpoint.close()

    def test_an_openai_client_reads_its_answers_and_the_log_says_when(self):
        from citar.agents.providers.openai_provider import OpenAIConversation
        endpoint = qa.FakeEndpoint(delay=0.2)
        try:
            conversation = OpenAIConversation({"base_url": endpoint.url, "model": endpoint.MODEL, "tool_mode": "native"},
                                              "system", [{"name": "end_turn", "description": "End the turn.",
                                                          "input_schema": {"type": "object", "properties": {}}}])
            conversation.add_user_text("Your turn.")
            step = conversation.step()
            conversation.close()
            self.assertEqual([c.name for c in step.tool_calls], ["end_turn"])
            [served] = endpoint.completions()
            self.assertEqual((served["tools"], served["calls"]), (1, ["end_turn"]))
            self.assertGreaterEqual(served["seconds"], 0.2, "the delay per completion")
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
