"""The QA harness's refusals and its instance (scripts/qa_phase3.py; design/phase3-features.md P3.9.4, package 3-03's
gate 3): the dev server's port, a port in use, a directory that holds anything without the harness's marker, the
owner's dev server's directory, CITAR's own data directory and a source checkout each exit 2 with nothing touched,
before a server could start; a marked directory is emptied and used; every path of the instance is under its
directory. The harness's run itself (``python scripts/qa_phase3.py accounts``) is S8, outside this suite: it starts a
server and waits on the usage ledger's minute. The ``minimap`` part's helpers, which its checks lean on, are tested
here too (``MinimapHelpers``).
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

    def test_a_part_that_does_not_exist(self):
        r = subprocess.run([sys.executable, str(SCRIPT), "everything", "--dir", str(self.tmp / "x")], cwd=str(ROOT),
                           capture_output=True, text=True, timeout=120)
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertIn("invalid choice", r.stderr)
        self.assertFalse((self.tmp / "x").exists())

    def test_every_part_is_written_and_all_runs_them_in_order(self):
        """Phase 3's parts are all written (P3.9.4, 3-19): none is left for later, and ``all`` runs the scripted parts
        first, then ``exit``, then ``real`` (the real model, after the scripted parts)."""
        self.assertEqual(qa.LATER, {})
        self.assertEqual(list(qa.PARTS), ["accounts", "queue", "ledger", "chat", "hybrid", "minimap", "machines",
                                          "exit", "real"])
        self.assertEqual(qa.EVERYONE, ("all", "exit"), "a third member for all and for exit on its own")


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


class AllRun(unittest.TestCase):
    """``all`` (3-19, gate 1): every part on one server, each check under the part that made it, each part's count
    and time at the top of ``qa-all.json``, and each part's own file beside it (written as it ends), as a run of that
    part alone writes it (in-process, the server, the accounts and the parts stubbed)."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="citar-qa-test-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.dir = self.tmp / "qa"

    def test_every_check_under_its_part_and_each_parts_file(self):
        seen = {}

        def first(h):
            h.report.data["first"] = {"id": 1}
            h.report.check("one of the first part's", True)

        def second(h):
            seen["first_file"] = json.loads((self.dir / "qa-first.json").read_text(encoding="utf-8"))
            h.report.check("one of the second part's", True)
            h.report.check("another of the second part's", False, "it failed")
        parts = {"first": first, "second": second}
        with mock.patch.object(qa.Server, "start", return_value=None), \
                mock.patch.object(qa, "make_accounts", return_value={}), \
                mock.patch.object(qa, "raise_game_limits", return_value=None) as limits, \
                mock.patch.object(qa.Server, "tracebacks", return_value=0), \
                mock.patch.dict(qa.PARTS, parts, clear=True), redirect_stdout(io.StringIO()):
            code = qa.main(["all", "--dir", str(self.dir)])
        self.assertEqual(code, 1)
        limits.assert_called_once()
        whole = json.loads((self.dir / "qa-all.json").read_text(encoding="utf-8"))
        self.assertEqual([c.get("part") for c in whole["checks"]], ["first", "second", "second", None],
                         "the harness's own last check is under no part")
        self.assertEqual({k: (v["checks"], v["passed"], v["failed"]) for k, v in whole["parts"].items()},
                         {"first": (1, 1, []), "second": (2, 1, ["another of the second part's"])})
        self.assertEqual(seen["first_file"]["checks"][0]["check"], "one of the first part's",
                         "the first part's file was there while the second ran")
        second_file = json.loads((self.dir / "qa-second.json").read_text(encoding="utf-8"))
        self.assertEqual((second_file["of"], second_file["ok"], len(second_file["checks"])), ("all", False, 2))
        self.assertEqual(second_file["first"], {"id": 1}, "beside the run's data")


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

    def test_a_diplomat_phase_looks_proposes_votes_answers_and_stops(self):
        """A hybrid seat's diplomat phase (P3.9.4, as the dry-run diplomat plays it): it looks, then proposes maps for
        maps to a civilization it met and is not talking to, abstains in an open vote, annexes its puppet and answers
        the chat it was told of; later it answers only the chats it is told of, and then stops."""
        endpoint = qa.FakeEndpoint(delay=0)
        names = ("get_diplomacy", "get_empire", "get_cities", "open_negotiation", "un_vote", "city_status",
                 "respond_negotiation", "send_message")
        tools = [{"type": "function", "function": {"name": n}} for n in names]
        phase = ("DIPLOMAT PHASE, turn 4. You were called because: it is 3 turns since the last phase.\n\n"
                 "CHAT: Rome is waiting for your answer in negotiation #9: maps?; the proposal from your side: "
                 "you give share_map, you receive share_map. 50 s left. Answer with respond_negotiation, with a "
                 "message.")
        messages = [{"role": "system", "content": "You are the diplomat of Egypt."}, {"role": "user", "content": phase}]

        def step() -> list:
            reply, calls = endpoint.scripted_reply({"model": "m", "messages": messages, "tools": tools})
            messages.append(reply["choices"][0]["message"])
            return [(c["function"]["name"], json.loads(c["function"]["arguments"])) for c in calls]
        try:
            self.assertEqual(qa.FakeEndpoint.describe({"messages": messages, "tools": tools})["kind"], "diplomat")
            looked = step()
            self.assertEqual([n for n, _ in looked], ["get_diplomacy", "get_empire", "get_cities"])
            results = [{"players": [{"id": 1, "alive": True}, {"id": 2, "alive": True, "at_war": True},
                                    {"id": 3, "alive": True}], "open_negotiations": [{"with": 1}],
                        "united_nations": {"voting_open": True, "your_vote": "not cast"}},
                       {"gold": 100}, [{"id": 7, "puppet": True}]]
            for call, result in zip(messages[-1]["tool_calls"], results):
                messages.append({"role": "tool", "tool_call_id": call["id"], "content": json.dumps(result)})
            acts = dict(step())
            self.assertEqual(acts["open_negotiation"]["to"], 3, "met, not at war, not talking already")
            self.assertEqual(acts["open_negotiation"]["give"], [{"type": "share_map"}])
            self.assertEqual(acts["un_vote"], {"candidate": "abstain"})
            self.assertEqual(acts["city_status"], {"city_id": 7, "status": "annex"})
            self.assertEqual((acts["respond_negotiation"]["negotiation_id"], acts["respond_negotiation"]["action"]),
                             (9, "accept"))
            for call in messages[-1]["tool_calls"]:
                messages.append({"role": "tool", "tool_call_id": call["id"], "content": "{}"})
            messages.append({"role": "user", "content": "CHAT: Rome is waiting for your answer in negotiation #11: "
                                                        "hello; the proposal from your side: none (no deal on the "
                                                        "table). 40 s left."})
            self.assertEqual(step(), [("respond_negotiation", {"negotiation_id": 11, "action": "reply",
                                                               "message": "(qa endpoint) reply"})])
            messages.append({"role": "tool", "tool_call_id": messages[-1]["tool_calls"][0]["id"], "content": "{}"})
            self.assertEqual(step(), [], "nothing more to do: the phase stops")
        finally:
            endpoint.close()

    def test_a_turn_closes_a_chat_that_came_back_to_it(self):
        endpoint = qa.FakeEndpoint(delay=0)
        try:
            note = ("NEGOTIATION UPDATE: Rome answered negotiation #5: no; the proposal from your side: you give "
                    "gold.")
            tools = self.TOOLS + [{"type": "function", "function": {"name": "respond_negotiation"}}]
            _reply, calls = endpoint.scripted_reply({"messages": [{"role": "user", "content": note}], "tools": tools})
            first = json.loads(calls[0]["function"]["arguments"])
            self.assertEqual((calls[0]["function"]["name"], first["negotiation_id"], first["action"]),
                             ("respond_negotiation", 5, "reject"))
            self.assertIn("get_units", [c["function"]["name"] for c in calls], "and the turn goes on")
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


class MinimapHelpers(unittest.TestCase):
    """The ``minimap`` part's own helpers (3-17), which its checks lean on: ``_arc`` (the shortest interval covering
    some positions, across the seam of an axis that wraps, the engine's choice of ties) and ``_joined_home`` (the land
    a view lists joined to an anchor through land it lists, and its box: what the engine's home box must be)."""

    def test_the_shortest_arc(self):
        self.assertIsNone(qa._arc(set(), 10, True))
        self.assertEqual(qa._arc({0, 1, 8, 9}, 10, False), (0, 9))
        self.assertEqual(qa._arc({0, 1, 8, 9}, 10, True), (8, 1), "across the seam")
        self.assertEqual(qa._arc({2, 3, 4}, 10, True), (2, 4))
        self.assertEqual(qa._arc({2, 5}, 8, True), (2, 5), "two gaps as long: not across the seam")
        self.assertEqual(qa._arc({0, 4, 8, 11}, 12, True), (4, 0), "two inside: the first left out")
        self.assertEqual(qa._arc(set(range(6)), 6, True), (0, 5))

    def test_the_land_joined_to_the_anchor(self):
        # a 10 by 6 map: land at (2, 1) and (3, 1), sea at (4, 1), land again at (5, 1) to (7, 1), and at (9, 2) and
        # (0, 2), which join over the seam of a map that wraps east-west; nothing listed joins (5, 1) to (3, 1)
        def tile(x, y, terrain="Grassland"):
            return [y * 10 + x, terrain]
        view = {"width": 10, "height": 6, "wrap_x": False, "wrap_y": False,
                "tiles": [tile(2, 1), tile(3, 1), tile(4, 1, "Coast"), tile(5, 1), tile(6, 1), tile(7, 1),
                          tile(9, 2), tile(0, 2), tile(1, 1, "Ocean")]}
        box, tiles = qa._joined_home(view, [2, 1])
        self.assertEqual(box, {"x0": 2, "y0": 1, "x1": 3, "y1": 1}, "the sea between is not land")
        self.assertEqual(tiles, {12, 13})
        self.assertEqual(qa._joined_home(view, [6, 1])[0], {"x0": 5, "y0": 1, "x1": 7, "y1": 1})
        self.assertEqual(qa._joined_home(view, [4, 1]), (None, set()), "an anchor at sea")
        self.assertEqual(qa._joined_home(view, [8, 4]), (None, set()), "an anchor not explored")
        self.assertEqual(qa._joined_home(view, None), (None, set()))
        # (0, 2) is next to (9, 2) only over the seam
        self.assertEqual(qa._joined_home(view, [9, 2])[0], {"x0": 9, "y0": 2, "x1": 9, "y1": 2}, "no wrap: alone")
        wraps = {**view, "wrap_x": True}
        self.assertEqual(qa._joined_home(wraps, [9, 2])[0], {"x0": 9, "y0": 2, "x1": 0, "y1": 2}, "over the seam")


class RealModelCheck(unittest.TestCase):
    """The ``real`` part's first check (3-19, gate 6): LM Studio's model list read (``/api/v0/models``), and only
    read; the model the part plays must be listed and loaded, or the part says what to do and plays nothing. A fake
    endpoint stands in for LM Studio (it answers ``/api/v0/models`` as LM Studio does)."""

    def setUp(self):
        self.lm = qa.FakeEndpoint(delay=0)
        self.addCleanup(self.lm.close)

    def test_loaded_listed_and_not_loaded(self):
        self.lm.models = [("google/gemma-4-e2b", True), ("other", False)]
        ready, said = qa.lmstudio_model(self.lm.url, "google/gemma-4-e2b")
        self.assertTrue(ready, said)
        self.lm.models = [("google/gemma-4-e2b", False)]
        ready, said = qa.lmstudio_model(self.lm.url, "google/gemma-4-e2b")
        self.assertFalse(ready)
        self.assertIn("never loads, unloads or swaps", said)
        self.lm.models = [("other", True)]
        ready, said = qa.lmstudio_model(self.lm.url, "google/gemma-4-e2b")
        self.assertFalse(ready)
        self.assertIn("not among", said)
        self.assertEqual({r["method"] for r in self.lm.requests}, {"GET"}, "it only reads")
        self.assertEqual({r["path"] for r in self.lm.requests}, {"/api/v0/models"})

    def test_nothing_listening(self):
        port = qa.free_port()
        ready, said = qa.lmstudio_model(f"http://127.0.0.1:{port}/v1", "google/gemma-4-e2b", timeout=3)
        self.assertFalse(ready)
        self.assertIn("does not answer", said)

    def test_the_part_stops_at_its_first_check_when_the_model_is_not_loaded(self):
        self.lm.models = [("google/gemma-4-e2b", False)]
        report = qa.Report("real")
        h = mock.Mock(report=report, accounts={"alice": mock.Mock(), "bob": mock.Mock()})
        with mock.patch.object(qa, "REAL_URL", self.lm.url), redirect_stdout(io.StringIO()):
            qa.part_real(h)
        self.assertEqual([c["ok"] for c in report.checks], [False])
        self.assertIn("load it there first", report.checks[0]["detail"])
        h.accounts["bob"].post.assert_not_called()

    def test_a_chat_the_model_failed_is_not_ended_by_the_chats_rules(self):
        """Gate 6: every chat answered by the model or ended by the chat clock's rule. The first exit runs counted
        the LLM seat's three declines after its model wrote its answers as text (``fallback``/``failed``) as
        recorded; they are the model's failure."""
        ok = [("answered", None), ("closed", None), ("declined_late", "timeout"), ("fallback", "timeout"),
              ("fallback", "not_started"), ("fallback", "queued_behind_benchmark"), ("fallback", "refused")]
        bad = [("fallback", "failed"), ("fallback", None), ("declined_late", "failed"), ("answered", "failed"),
               ("unanswered", None), (None, None)]
        for outcome, cause in ok:
            self.assertTrue(qa._chat_ended_by_rule({"outcome": outcome, "cause": cause}), (outcome, cause))
        for outcome, cause in bad:
            self.assertFalse(qa._chat_ended_by_rule({"outcome": outcome, "cause": cause}), (outcome, cause))


if __name__ == "__main__":
    unittest.main()
