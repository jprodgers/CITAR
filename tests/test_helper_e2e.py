"""The real CITAR helper against the real hub, through the prompt scheduler (design/phase4-helper.md P4.5.1-P4.5.3;
package 4-05's gate 7).

The binary is the one ``cargo xtask develop`` builds into ``CITAR_EXT_DIR`` (``citar-helper``, with its development
guard: loopback servers only, an explicit ``--home``); test.yml's build-ext job builds it on CI's three OSes. Without
it the module skips. Each helper runs in a home of its own under the system's temporary directory and serves the QA
harness's scripted endpoint (``scripts/qa_phase3.FakeEndpoint``), which streams as LM Studio does and records when its
client went.

- **The key:** its first hello registers its machine key, and the Servers page's fingerprint of it equals the one the
  binary's ``--version`` prints for its home.
- **Parsed on the server:** a seat plays three turns with the scripted endpoint's tool calls, each turn ended by its own
  ``end_turn`` (a lease's steps), and a ticket's step comes back as parsed calls: the helper returned the completions
  raw, and ``completion.parse_completion`` read them here.
- **Cancel:** a cancelled ticket's slot is free within 1 s with nothing draining; the endpoint's log shows its client
  gone within 1 s of the cancel and no event after it but the one that may have been on its way.
- **Revoke:** revoking its token drops it within a second, and it exits 78 without reconnecting.
- **Restart:** it reconnects by itself after the server restarts.
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)

import importlib.util
import os
import tempfile
import threading
import time
import unittest
from pathlib import Path

from citar import db, servers, settings
from citar.auth import accounts, tokens
from citar.db.models import Base, Server, WorkerToken
from citar.pool import scheduler as S
from citar.server import workers as W
from tests import helperfake as H

ROOT = Path(__file__).resolve().parent.parent
_TMP = Path(tempfile.mkdtemp(prefix="citar_helper_e2e_"))
_URL = "sqlite:///" + (_TMP / "test.db").as_posix()
CI = bool(os.environ.get("CI"))


def _qa():
    spec = importlib.util.spec_from_file_location("qa_phase3_for_e2e", ROOT / "scripts" / "qa_phase3.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


qa = _qa()
MODEL = qa.FakeEndpoint.MODEL


def setUpModule():
    if H.helper_binary() is None:
        raise unittest.SkipTest("no citar-helper in CITAR_EXT_DIR: cargo xtask develop builds it")
    os.environ["CITAR_DATA_DIR"] = str(_TMP)
    os.environ["CITAR_DB_URL"] = _URL
    settings.reset()
    db.dispose()
    db.configure(_URL)
    Base.metadata.drop_all(db.engine())
    db.create_all()
    servers.invalidate()
    S.reset()


def _until(check, limit: float = 10.0, step: float = 0.01):
    return H.until(check, limit, step)


class RealHelper(unittest.TestCase):

    @classmethod
    def setUpClass(cls):
        cls.server = H.HubServer()
        with db.session() as s:
            owner = accounts.create_user(s, handle="e2e", email="e2e@x.cc", password="a long enough phrase",
                                         status="active", email_verified=True)
            cls.owner_id = owner.id

    @classmethod
    def tearDownClass(cls):
        cls.server.stop()
        S.reset()

    def setUp(self):
        self.events: list = []
        S.get().add_listener(self.events.append)
        self.addCleanup(S.get().remove_listener, self.events.append)

    def machine(self, name: str, slots: int = 1) -> tuple:
        with db.session() as s:
            row = Server(owner_id=self.owner_id, name=name, kind="owned", provider="openai_compatible",
                         reach="worker", max_concurrent=slots, config={"models": [{"key": MODEL}]})
            s.add(row)
            s.flush()
            raw, hashed = tokens.new_pair()
            token = WorkerToken(server_id=row.id, token_hash=hashed, prefix=tokens.prefix(raw), label="e2e")
            s.add(token)
            s.flush()
            sid, tid = row.id, token.id
        servers.invalidate()
        return sid, raw, tid

    def endpoint(self, delay: float = 0.05):
        endpoint = qa.FakeEndpoint(delay=delay)
        self.addCleanup(endpoint.close)
        return endpoint

    def helper(self, sid: str, token: str, endpoint, slots: int = 1) -> H.RealHelper:
        helper = H.RealHelper(self.server.base, token, endpoint.url, name=f"e2e {sid}", slots=slots)
        self.addCleanup(helper.stop)
        ok = _until(lambda: W.hub().get(sid) is not None and W.hub().get(sid).ack is not None, 30)
        self.assertTrue(ok, f"the helper did not connect:\n{helper.text()[-3000:]}")
        return helper

    def conversation(self, sid: str, binding, tools=(), text: str = "Reply with exactly: CITAR OK"):
        from citar.agents.providers import make_conversation
        conv = make_conversation({"provider": "worker", "server_id": sid, "model": MODEL, "timeout": 60},
                                 "You are a test.", list(tools), binding=binding)
        conv.add_user_text(text)
        return conv

    # -------------------------------------------------------------- the key
    def test_its_key_is_registered_and_the_card_shows_its_fingerprint(self):
        sid, token, _tid = self.machine("keyed")
        helper = self.helper(sid, token, self.endpoint())
        from citar import engine_api
        with db.session() as s:
            key = s.get(Server, sid).public_key
        self.assertTrue(key, "its first hello registered its key")
        printed = helper.fingerprint()
        self.assertEqual(len(printed), 16, helper.version())
        self.assertEqual(engine_api.key_fingerprint(key), printed, "the server's fingerprint is the helper's")
        card = W.hub().get(sid).client()
        self.assertEqual((card["protocol"], card["key_fingerprint"]), (2, printed))
        with db.session() as s:
            self.assertEqual((s.get(Server, sid).helper or {}).get("key_fingerprint"), printed)

    # -------------------------------------------------------------- parsed on the server
    def test_a_seat_plays_three_turns_and_a_tickets_step_is_parsed_here(self):
        from citar.server.session import SessionManager
        sid, token, _tid = self.machine("player")
        endpoint = self.endpoint(0.05)
        self.helper(sid, token, endpoint)
        manager = SessionManager()
        s = manager.create({"map_size": "duel", "seed": 11, "barbarians": "off"},
                           [{"type": "llm", "llm": {"server_id": sid, "model": MODEL}}, {"type": "bot"}])
        self.addCleanup(manager.delete, s.id)
        self.assertTrue(_until(lambda: s.game.turn >= 4, 180), "the model seat played three turns")
        s.set_paused(True)
        turns = [r for r in s.metrics.data["turns"] if r["player"] == 0 and r["ended"] and r["turn"] <= 3]
        self.assertEqual([r["end_reason"] for r in turns], ["end_turn"] * 3, "each ended by its own end_turn")
        self.assertTrue(all(r["model_steps"] >= 2 for r in turns), [r["model_steps"] for r in turns])
        streamed = [r for r in endpoint.completions() if r.get("kind") == "turn"]
        self.assertTrue(streamed and all(r["stream"] for r in streamed), "the helper streamed every completion")
        self.assertIn("end_turn", [c for r in streamed for c in r["calls"]])
        game_steps = [e for e in self.events if e.machine == sid and e.cls == "game"]
        self.assertGreaterEqual(len(game_steps), 6)
        self.assertTrue(all(e.reported_s is not None and e.reported_s <= e.observed_s + 0.05 for e in game_steps))
        # a ticket's step: the endpoint's call comes back parsed
        ticket = S.get().ticket(sid, MODEL, cls="test", label="parsed")
        self.addCleanup(ticket.close)
        step = self.conversation(sid, ticket, tools=[H.END_TURN], text="end your turn").step()
        self.assertEqual([c.name for c in step.tool_calls], ["end_turn"])

    # -------------------------------------------------------------- cancel
    def test_a_cancelled_tickets_slot_is_free_at_once_and_its_generation_stopped(self):
        sid, token, _tid = self.machine("cancelled")
        endpoint = self.endpoint(10.0)
        self.helper(sid, token, endpoint)
        sched = S.get()
        m = sched.machine(sid)
        before = len(endpoint.arrivals)
        ticket = sched.ticket(sid, MODEL, cls="test", label="to cancel")
        conv = self.conversation(sid, ticket, text="say many words: one two three four five six seven eight nine")
        out: dict = {}

        def run():
            try:
                out["result"] = conv.step()
            except BaseException as exc:
                out["error"] = exc
        th = threading.Thread(target=run, daemon=True)
        th.start()
        self.assertTrue(_until(lambda: len(endpoint.arrivals) > before, 15), "the request reached the endpoint")
        # mid-stream: its first words are out
        self.assertTrue(_until(lambda: endpoint.live and endpoint.live[-1]["events"], 15), "the stream began")
        t_cancel = time.time()
        p_cancel = time.perf_counter()
        ticket.cancel()
        th.join(5)
        self.assertIsInstance(out.get("error"), Exception)
        freed = _until(lambda: m.slots == [] and time.perf_counter(), 5)
        self.assertTrue(freed, "the slot was freed")
        bound = 1.0 * (3 if CI else 1)
        self.assertLess(freed - p_cancel, bound, "within a second of the cancel")
        self.assertEqual(m.counters["draining"], 0, "nothing drained")
        entry = _until(lambda: next((r for r in endpoint.streamed() if r.get("gone")), None), 5)
        self.assertIsNotNone(entry, "the endpoint saw its client go")
        self.assertLess(entry["gone"] - t_cancel, bound, "its client went within a second of the cancel")
        after = [t for t in entry["events"] if t > t_cancel]
        self.assertLessEqual(len(after), 1, "no event after the cancel but the one on its way")
        self.assertGreaterEqual(entry["tokens"], 1)
        # the next request is served at once, not refused busy
        lease = sched.lease(sid, MODEL, kind="game", label="next")
        self.addCleanup(lease.release)
        endpoint.delay = 0.05
        self.conversation(sid, lease).step()
        self.assertEqual(m.counters["busy_refusals"], 0)

    # -------------------------------------------------------------- revoke
    def test_a_revoke_drops_it_and_it_exits_78_without_reconnecting(self):
        sid, token, tid = self.machine("revoked")
        helper = self.helper(sid, token, self.endpoint())
        from datetime import datetime, timezone
        t0 = time.perf_counter()
        with db.session() as s:
            s.get(WorkerToken, tid).revoked_at = datetime.now(timezone.utc)
        W.hub().drop(sid, "its worker token was revoked", token_id=tid)
        self.assertTrue(_until(lambda: not W.hub().is_online(sid), 2))
        code = helper.exited(10)
        took = time.perf_counter() - t0
        self.assertEqual(code, 78, helper.text()[-2000:])
        self.assertLess(took, 1.0 * (3 if CI else 1), f"it stopped {took:.2f} s after the revoke")
        time.sleep(1.0)
        self.assertFalse(W.hub().is_online(sid), "and did not come back")

    # -------------------------------------------------------------- restart (last: it restarts the server)
    def test_zz_it_reconnects_by_itself_after_a_server_restart(self):
        sid, token, _tid = self.machine("restarted")
        helper = self.helper(sid, token, self.endpoint())
        first = W.hub().get(sid).serial
        port = self.server.port
        self.server.stop()
        self.assertTrue(_until(lambda: not W.hub().is_online(sid), 10))
        type(self).server = H.HubServer(port=port)
        self.assertTrue(_until(lambda: W.hub().get(sid) is not None and W.hub().get(sid).serial != first, 60),
                        f"it did not reconnect:\n{helper.text()[-3000:]}")
        self.assertIsNone(helper.proc.poll(), "the same process, still running")


if __name__ == "__main__":
    unittest.main()
