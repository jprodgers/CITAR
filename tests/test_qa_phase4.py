"""``scripts/qa_phase4.py``'s own pieces that need no server (design/phase4-helper.md, Appendix E, 4-15): the exit
part's check of a helper's signed claim, made by another implementation than the server's (``cryptography``'s
ed25519 over the claim's compact JSON, its fields in the signed order), against claims the extension signs as a
helper signs its own; the lab log's lines that say why a result was replicated; a results line's result taken apart
from its measurements; and ``all``: its refusals, and its two runs (the parts written for a server on Windows, then
``service``, ``exit`` and ``real`` with the server in WSL), each in a folder of its own, summed up in
``qa-all.json``."""
from __future__ import annotations

import argparse
import contextlib
import importlib.util
import io
import json
import os
import shutil
import tempfile
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
