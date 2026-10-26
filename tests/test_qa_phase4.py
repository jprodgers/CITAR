"""``scripts/qa_phase4.py``'s own pieces that need no server (design/phase4-helper.md, Appendix E, 4-15): the exit
part's check of a helper's signed claim, made by another implementation than the server's (``cryptography``'s
ed25519 over the claim's compact JSON, its fields in the signed order), against claims the extension signs as a
helper signs its own; the lab log's lines that say why a result was replicated; a results line's result taken apart
from its measurements; the exit's checks that each helper's results were replicated while its machine was new and
sampled after (tied to its promotion), and that the ledger has one event a run; the gate the real parts put between
the helper and LM Studio (``qa_phase3.ModelGate``); and ``all``: its refusals, and its two runs (the parts written for
a server on Windows, then ``service``, ``exit`` and ``real`` with the server in WSL), each in a folder of its own,
summed up in ``qa-all.json``."""
from __future__ import annotations

import argparse
import contextlib
import importlib.util
import io
import json
import os
import shutil
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent


def _qa4():
    spec = importlib.util.spec_from_file_location("qa_phase4_for_its_tests", ROOT / "scripts" / "qa_phase4.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


qa4 = _qa4()


class ClaimSignature(unittest.TestCase):
    """The exit's check of an accepted line's signature agrees with the helper's signing (P4.4.4)."""

    def setUp(self):
        from tests import has_test_ops
        if not has_test_ops():
            self.skipTest("the extension's test operations sign the throwaway claims")
        try:
            import cryptography  # noqa: F401
        except ImportError:
            self.skipTest("the cryptography package checks the signature")

    @staticmethod
    def claim(**changed) -> dict:
        from tests import helperfake as HF
        claim = {"v": 1, "job": "cj_qa", "run": "cr_qa", "machine": "sv_qa", "kind": "lab_game", "spec_hash": "a" * 64,
                 "jobs_build": "b" * 12, "bots": ["basic-1"], "result_hash": "c" * 64, "chain_head": "d" * 64,
                 "rounds": 40, "key": HF.test_key()}
        claim.update(changed)
        return claim

    def test_a_claim_signed_as_a_helper_signs_it_verifies(self):
        from tests import helperfake as HF
        claim = self.claim()
        self.assertTrue(qa4._claim_signed(claim, HF.sign_claim(claim), claim["key"]))

    def test_the_fields_are_read_in_the_signed_order_whatever_the_stored_order(self):
        from tests import helperfake as HF
        claim = self.claim()
        stored = dict(reversed(list(claim.items())))
        self.assertTrue(qa4._claim_signed(stored, HF.sign_claim(claim), claim["key"]))

    def test_any_field_changed_another_key_or_a_bad_signature_does_not_verify(self):
        from tests import helperfake as HF
        claim = self.claim()
        signature = HF.sign_claim(claim)
        for field in qa4.CLAIM_FIELDS:
            if field == "key":
                continue
            value = claim[field]
            other = (value + 1 if isinstance(value, int) else ["basic-2"] if isinstance(value, list)
                     else value[:-1] + ("0" if value[-1] != "0" else "1"))
            self.assertFalse(qa4._claim_signed({**claim, field: other}, signature, claim["key"]), field)
        self.assertFalse(qa4._claim_signed(claim, signature, HF.test_key(HF.TEST_KEY_SEED + 1)))
        self.assertFalse(qa4._claim_signed(claim, "not base64!", claim["key"]))
        self.assertFalse(qa4._claim_signed({k: v for k, v in claim.items() if k != "rounds"}, signature,
                                           claim["key"]))


class Lines(unittest.TestCase):
    def test_the_lab_logs_replica_lines_are_read_by_experiment_and_game(self):
        log = "\n".join([
            "2026-10-18 05:00:01 qa-exit-ab12 #3: T40 none won by B (1s on QA exit Windows ab12); checked by a replica "
            "before it is accepted (new)",
            "2026-10-18 05:00:02 qa-exit-ab12 #4: T40 none won by A (0s on this server)",
            "2026-10-18 05:00:03 qa-exit-ab12 #9: crashed on QA exit WSL ab12; checked by a replica before it is "
            "accepted (sampled)",
            "2026-10-18 05:00:04 qa-exit-ab12 #3: T40 none won by B (1s on QA exit WSL ab12); checked by a replica "
            "before it is accepted (asked)"])
        self.assertEqual(qa4._replicated_why(log), {("qa-exit-ab12", 3): ["new", "asked"],
                                                    ("qa-exit-ab12", 9): ["sampled"]})

    def test_a_lines_result_is_the_line_without_its_measurements_and_executor(self):
        line = {"exp": "x", "i": 2, "players": {"0": {}}, "turns": 40, "seconds": 1.2, "started_ts": 5.0, "cpu_s": 1.1,
                "finished": "2026-10-18T05:00:00", "executor": {"machine": "sv", "run": "cr", "verification": "x"}}
        self.assertEqual(qa4._result_of(line), {"exp": "x", "i": 2, "players": {"0": {}}, "turns": 40})

    def test_database_times_are_utc_unless_they_name_a_zone(self):
        self.assertEqual(qa4._when("2026-10-18T09:00:00"), qa4._when("2026-10-18T05:00:00-04:00"))
        self.assertEqual(qa4._when(None), 0.0)


class ReplicationOrder(unittest.TestCase):
    """The exit's check that a helper machine's results were replicated while it was new and sampled after
    (``_replication_order``, P4.5.8 rules 1 and 5), each result put before or after the promotion by the order of the
    ledger's events, the promoting replica's among them (4-15's review: a result of a new machine accepted without a
    replica passed whenever its sample fell over the rate)."""

    HOST = "sv_host"
    PROMOTED = 100
    RATE = 0.5

    def primary(self, event, why=None, *, sample: float = 0.9, verdict=None, local: bool = True) -> dict:
        replicas = [] if why is None else [{"executor": "local" if local else "remote",
                                            "machine": self.HOST if local else "sv_other", "verdict": "reference"}]
        return {"ref": f"lab:x:{event}", "event": event, "why": why, "replicas": replicas,
                "verdict": verdict or ("match" if why else "unchecked"), "sample": sample}

    def order(self, primaries: list, promoted=PROMOTED) -> tuple:
        return qa4._replication_order(primaries, promoted, self.RATE, self.HOST)

    def test_a_machine_replicated_while_new_and_sampled_after_keeps_the_rules(self):
        counts, wrong = self.order([
            self.primary(90, "new"), self.primary(95, "new"), self.primary(99, "new"),
            self.primary(101, sample=0.7), self.primary(102, "sampled", sample=0.2), self.primary(110, sample=0.8)])
        self.assertEqual(wrong, [])
        self.assertEqual(counts, {"new": 3, "sampled": 1, "unsampled": 2})

    def test_a_new_machines_result_accepted_without_a_replica_is_caught(self):
        # the review's planted bug: one result of a new machine accepted unreplicated (no verifier free, say), its
        # sample over the rate, five others matching; however close to the promotion it was decided
        primaries = [self.primary(80 + i, "new") for i in range(5)] + [self.primary(99, sample=0.9)]
        _counts, wrong = self.order(primaries)
        self.assertEqual([(w["job"], w["decided"]) for w in wrong], [("lab:x:99", "while new")])

    def test_a_result_decided_on_the_wrong_side_of_the_promotion_is_caught(self):
        _counts, wrong = self.order([self.primary(99, "sampled", sample=0.2), self.primary(101, "new")])
        self.assertEqual([(w["job"], w["decided"]) for w in wrong],
                         [("lab:x:99", "while new"), ("lab:x:101", "once provisional")])

    def test_a_sample_on_the_wrong_side_of_the_rate_a_bad_replica_or_no_event_is_caught(self):
        _counts, wrong = self.order([
            self.primary(105, "sampled", sample=0.7), self.primary(106, sample=0.2),
            self.primary(90, "new", local=False), self.primary(91, "new", verdict="mismatch"),
            self.primary(107, "asked", sample=0.9), self.primary(None, "new")])
        self.assertEqual(len(wrong), 6, wrong)
        self.assertEqual(wrong[-1]["decided"], "no event")

    def test_a_machine_never_promoted_has_every_result_replicated_as_new(self):
        _counts, wrong = self.order([self.primary(10, "new"), self.primary(2000, sample=0.9)], promoted=None)
        self.assertEqual([w["job"] for w in wrong], ["lab:x:2000"])


class EventsPerRun(unittest.TestCase):
    """One ledger event a run that ended done (``_events_per_run``; 4-15's review: counted by request in a dict, an
    event written twice passed)."""

    RUNS = [{"id": "a", "state": "done"}, {"id": "b", "state": "done"}, {"id": "c", "state": "aborted"}]

    def test_one_event_a_done_run(self):
        self.assertEqual(qa4._events_per_run([{"request": "b"}, {"request": "a"}], self.RUNS),
                         {"events": 2, "runs": 2, "duplicated": [], "stray": [], "missing": []})

    def test_an_event_written_twice_one_of_no_done_run_and_a_run_without_one_show(self):
        got = qa4._events_per_run([{"request": "a"}, {"request": "a"}, {"request": "c"}], self.RUNS)
        self.assertEqual(got, {"events": 3, "runs": 2, "duplicated": ["a"], "stray": ["c"], "missing": ["b"]})


class Gate(unittest.TestCase):
    """``qa_phase3.ModelGate``, between the real parts' helper and LM Studio (4-15's review: after LM Studio's engine
    crashed, the helper forwarded eleven more completions within a second, and with just-in-time loading on they would
    have loaded the model again), here in front of the stand-in for LM Studio (``qa_phase4.stand_in``)."""

    def setUp(self):
        self.ep = qa4.stand_in(long_s=6.0, long_words=100)
        self.addCleanup(self.ep.close)
        self.gate = qa4.q3.ModelGate(self.ep.url, qa4.REAL_MODEL)
        self.addCleanup(self.gate.close)

    def complete(self, prompt: str = "Reply with exactly: OK", *, stream: bool = True):
        import httpx
        return httpx.post(f"{self.gate.url}/chat/completions", timeout=30,
                          json={"model": qa4.REAL_MODEL, "stream": stream,
                                "messages": [{"role": "user", "content": prompt}]})

    def test_listings_and_answers_pass_through_as_the_model_sends_them(self):
        import httpx
        listing = httpx.get(self.gate.url.rsplit("/v1", 1)[0] + "/api/v0/models", timeout=10)
        self.assertEqual([(m["id"], m["state"]) for m in listing.json()["data"]], [(qa4.REAL_MODEL, "loaded")])
        streamed = self.complete()
        self.assertEqual(streamed.status_code, 200)
        self.assertIn("text/event-stream", streamed.headers["content-type"])
        self.assertTrue(streamed.text.rstrip().endswith("data: [DONE]"), streamed.text[-200:])
        plain = self.complete(stream=False)
        self.assertEqual(plain.json()["object"], "chat.completion")
        self.assertEqual(len(self.ep.completions()), 2)
        summary = self.gate.summary()
        self.assertEqual((summary["forwarded"], summary["refused"], summary["tripped"]), (2, 0, None), summary)

    def test_a_model_not_listed_loaded_is_sent_no_completion_and_the_gate_stays_closed(self):
        self.ep.models = [(qa4.REAL_MODEL, False)]
        refused = self.complete()
        self.assertEqual(refused.status_code, 503)
        self.assertIn("not-loaded", refused.json()["error"]["message"])
        self.assertTrue(self.gate.closed.is_set())
        # loaded again, the gate stays closed: a part that saw its model go stops there
        self.ep.models = [(qa4.REAL_MODEL, True)]
        self.assertEqual(self.complete().status_code, 503)
        self.assertEqual(self.ep.arrivals, [], "a completion reached a model that was not loaded")
        self.assertEqual(self.gate.summary()["refused"], 2)

    def test_a_completion_that_fails_closes_the_gate_before_the_next(self):
        # LM Studio's crash in 4-15's first real run: the stream's error event. This stand-in answers what comes after
        # (as LM Studio with just-in-time loading on would, loading the model again): nothing more reaches it
        self.ep.fail_at = 1
        first = self.complete()
        self.assertEqual(first.status_code, 200, "relayed as the model sent it")
        self.assertIn('"terminated"', first.text)
        # closed before the failure's bytes went on: the helper cannot hear of it first
        self.assertIsNotNone(self.gate.tripped, "the client had the failure before the gate closed")
        self.assertIn("an error event", self.gate.tripped["why"])
        self.assertEqual(self.complete().status_code, 503)
        self.assertEqual(len(self.ep.arrivals), 1)
        self.assertEqual(len(self.gate.summary()["failures"]), 1)

    def test_a_cancel_closes_the_models_connection_at_once_and_is_no_failure(self):
        import socket
        from urllib.parse import urlparse
        body = json.dumps({"model": qa4.REAL_MODEL, "stream": True,
                           "messages": [{"role": "user", "content": qa4.LONG_PROMPT}]}).encode()
        sock = socket.create_connection(("127.0.0.1", urlparse(self.gate.url).port), timeout=10)
        sock.sendall(b"POST /v1/chat/completions HTTP/1.1\r\nHost: qa\r\nContent-Type: application/json\r\n"
                     b"Content-Length: %d\r\n\r\n" % len(body) + body)
        got = b""
        while got.count(b"data: ") < 5:
            more = sock.recv(65536)
            self.assertTrue(more, "the answer ended before its fifth event")
            got += more
        closed = time.time()
        sock.close()
        entry = qa4.wait_until(lambda: next((e for e in self.ep.live if e.get("gone")), None), 5, 0.01)
        self.assertIsNotNone(entry, "the model's connection stayed open after its client went")
        self.assertLess(entry["gone"] - closed, 1.0)
        self.assertLess(entry["tokens"], 100, "the answer was not cut short")
        self.assertTrue(qa4.wait_until(lambda: self.gate.summary()["cancelled"] == 1, 5, 0.01))
        self.assertIsNone(self.gate.tripped, "a cancel is no failure")
        self.assertEqual(self.complete().status_code, 200, "the gate stays open after a cancel")

    def test_the_guard_closes_the_gate_and_stops_the_helper_when_the_model_goes(self):
        stopped = []
        self.gate.on_trip(lambda: stopped.append(time.monotonic()))
        self.gate.watch()
        self.assertEqual(self.complete().status_code, 200)
        went = time.monotonic()
        self.ep.unload_at = len(self.ep.arrivals)        # unloaded now, between completions
        self.assertTrue(qa4.wait_until(lambda: stopped, 5, 0.01), "the guard did not see the model go")
        self.assertLess(stopped[0] - went, self.gate.POLL + 1.0)
        self.assertEqual(self.complete().status_code, 503)
        self.assertEqual((len(stopped), len(self.ep.arrivals)), (1, 1))

    def test_the_guard_reads_the_helpers_log_for_an_endpoint_failure(self):
        tmp = Path(tempfile.mkdtemp(prefix="citar_qa4_gate_"))
        self.addCleanup(shutil.rmtree, tmp, True)
        log = tmp / "helper-1.log"
        log.write_text("2026-10-18 04:51:12 INFO    request 0a done in 1.0 s (1 tool call)\n", encoding="utf-8")
        stopped = []
        self.gate.on_trip(lambda: stopped.append(1))
        self.gate.watch(log)
        self.assertIsNone(qa4.wait_until(lambda: self.gate.tripped, 0.5, 0.05), "a done request closed the gate")
        with open(log, "a", encoding="utf-8") as fh:
            fh.write("2026-10-18 04:51:13 INFO    request 4acb failed (endpoint): the endpoint reported an error: "
                     "terminated\n")
        self.assertTrue(qa4.wait_until(lambda: stopped, 5, 0.01), "the guard did not read the failure")
        self.assertIn("request 4acb failed (endpoint)", self.gate.tripped["why"])
        self.assertEqual(stopped, [1])

    def test_it_stands_in_front_of_this_machines_own_server_only(self):
        for url in ("http://example.com:1234/v1", "https://localhost:1234/v1", "http://192.168.1.2:1234/v1"):
            with self.assertRaises(ValueError, msg=url):
                qa4.q3.ModelGate(url, qa4.REAL_MODEL)

    def test_its_reading_of_an_answer(self):
        def read(*pieces: bytes, closed: bool = True) -> tuple:
            answer = qa4.q3._Answer()
            for p in pieces:
                for i in range(len(p)):             # a byte at a time: every boundary falls somewhere
                    answer.feed(p[i:i + 1])
            return answer.complete, answer.verdict(closed=closed)
        error = b'{"error": {"message": "No models loaded."}}'
        self.assertEqual(read(b"HTTP/1.1 400 Bad Request\r\nContent-Length: %d\r\n\r\n" % len(error) + error)[1],
                         ("failed", 'HTTP 400: {"error": {"message": "No models loaded."}}'))
        self.assertEqual(read(b"HTTP/1.1 200 OK\r\nContent-Length: %d\r\n\r\n" % len(error) + error)[1][0], "failed")
        events = [b'data: {"choices": [{"delta": {"content": "OK"}}]}\n\n', b"data: [DONE]\n\n"]
        head = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n"
        chunks = b"".join(b"%x\r\n%s\r\n" % (len(e), e) for e in events)
        self.assertEqual(read(head, chunks, b"0\r\n\r\n", closed=False), (True, ("ok", "")))
        self.assertEqual(read(head, chunks, closed=True), (False, ("failed", "its answer was cut short")))
        crashed = b'data: {"error":{"message":"terminated"},"message":"terminated"}\n\n'
        done, (verdict, why) = read(head, b"%x\r\n%s\r\n0\r\n\r\n" % (len(crashed), crashed), closed=False)
        self.assertEqual((done, verdict), (True, "failed"))
        self.assertIn("an error event", why)
        self.assertEqual(read(b"")[1], ("failed", "LM Studio closed the connection before it answered"))
        # a failure shows as soon as its bytes do, before the answer ends
        early = qa4.q3._Answer()
        early.feed(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 99\r\n\r\n{")
        self.assertEqual((early.complete, early.failure()), (False, "HTTP 400: {"))
        early = qa4.q3._Answer()
        early.feed(head + b"%x\r\n%s\r\n" % (len(crashed), crashed))
        self.assertFalse(early.complete)
        self.assertIn("an error event", early.failure())


class All(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="citar_qa4_all_"))
        self.addCleanup(shutil.rmtree, self.tmp, True)

    def main(self, *argv) -> tuple:
        out = io.StringIO()
        # the process's environment put back whatever main does with it: the suite's own switch
        # (CITAR_TEST_NO_DISPATCHER) must survive for the modules after this one
        with mock.patch.dict(os.environ), contextlib.redirect_stdout(out):
            code = qa4.main(list(argv))
        return code, out.getvalue()

    def test_the_parts_that_need_wsl_are_refused_without_it_touching_nothing(self):
        target = self.tmp / "run"
        for argv in (["exit"], ["service"], ["all"], ["all", "--skip", "service"],
                     ["exit", "--wsl", "--wsl-root", "/home/claude/x", "--wsl-python", "/home/claude/v/bin/python"]):
            code, said = self.main(*argv, "--dir", str(target))
            self.assertEqual(code, 2, argv)
            self.assertIn("--wsl-helper", said, argv)
        code, said = self.main("helper", "--wsl", "--dir", str(target))
        self.assertEqual(code, 2)
        self.assertIn("--wsl belongs to", said)
        self.assertFalse(target.exists(), "a refusal touches nothing")
        # nor the process's environment: the suite's switch that keeps its servers from playing the lab stays set (a
        # refusal that cleared it made every later module's app start a job dispatcher of its own)
        before = os.environ.get("CITAR_TEST_NO_DISPATCHER")
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(qa4.main(["exit", "--dir", str(target)]), 2)
        self.assertEqual(os.environ.get("CITAR_TEST_NO_DISPATCHER"), before)

    def test_all_runs_the_windows_parts_then_the_wsl_parts_each_in_a_folder_of_its_own(self):
        seen = []

        def run(sub):
            seen.append({"dir": sub.dir, "parts": list(qa4.q3.PARTS), "wsl": sub.wsl, "part": sub.part})
            checks = [{"check": f"{p} ran", "ok": True} for p in qa4.q3.PARTS]
            return 0, {"parts": {p: {} for p in qa4.q3.PARTS}, "checks": checks, "seconds": 1.0}
        top = self.tmp / "all-run"
        args = argparse.Namespace(part="all", dir=str(top), skip=["llm-real"], wsl=True, port=0, keep=False)
        with mock.patch.object(qa4, "_run", side_effect=run), contextlib.redirect_stdout(io.StringIO()):
            code = qa4._run_all(args)
        self.assertEqual(code, 0)
        self.assertEqual([s["parts"] for s in seen],
                         [["helper", "scan", "lab", "remote", "verify", "update"], ["service", "exit", "real"]])
        self.assertEqual([s["wsl"] for s in seen], [False, True])
        self.assertEqual({s["part"] for s in seen}, {"all"})
        self.assertEqual([Path(s["dir"]).name for s in seen], ["all-run-windows", "all-run-wsl"])
        self.assertTrue(all(Path(s["dir"]).parent == top.resolve() for s in seen))
        self.assertIs(qa4.q3.PARTS, qa4.PARTS, "the harness's parts are put back")
        summary = json.loads((top / "qa-all.json").read_text(encoding="utf-8"))
        self.assertEqual((summary["ok"], summary["skipped"]), (True, ["llm-real"]))
        self.assertEqual({k: v["passed"] for k, v in summary["runs"].items()}, {"windows": 6, "wsl": 3})

    def test_all_fails_when_either_run_fails(self):
        def run(sub):
            ok = not sub.wsl
            return (0 if ok else 1), {"parts": {}, "checks": [{"check": "x", "ok": ok}]}
        args = argparse.Namespace(part="all", dir=str(self.tmp / "failing"), skip=[], wsl=True, port=0, keep=False)
        with mock.patch.object(qa4, "_run", side_effect=run), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(qa4._run_all(args), 1)
        summary = json.loads((self.tmp / "failing" / "qa-all.json").read_text(encoding="utf-8"))
        self.assertEqual((summary["ok"], summary["runs"]["wsl"]["failed"]), (False, ["x"]))


if __name__ == "__main__":
    unittest.main()
