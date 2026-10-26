"""``qa_phase4.py llm-real``'s checks, tested where the real model is not there (design/phase4-helper.md 4-05, gate 8;
as ``crates/citar-helper/tests/real_model.rs`` tests 4-04 gate 4's).

The part's checks are one function, ``exercise``: LM Studio lists ``google/gemma-4-e2b`` loaded before anything is
sent and again after; the real helper in front of it, through a hub of its own and the prompt scheduler, answers a
request with tools with a raw message the server parses; a long generation cancelled at 2 s is acknowledged within a
second with nothing draining; and three short requests after it take no longer than 1.5 times the median of three
before it plus a second. Here it runs against a stand-in for LM Studio (``qa_phase4.stand_in``: the scripted endpoint
with one model generating one answer at a time), so the checks themselves are tested in every run:

- they **pass against a stand-in that stops a cancelled generation** when its client goes, as LM Studio does, and the
  stand-in's side agrees (eight completions, all streamed; the long one's client gone within a second of the cancel
  with at most one event after it, cut short mid-answer);
- they **fail against one that runs on after a cancel**: the cancel is still acknowledged at once (the helper's side
  is right), but the first answer after it waits for the abandoned generation, over the bound;
- they **fail when the long generation ends before the cancel**: it tests no cancel, and its check says so at once;
- they **fail with nothing sent against a model that is not loaded**: no completion reaches it and no helper starts;
- they **fail against a model unloaded during the run**, at the look after it;
- they **send nothing more to a model unloaded after its first completion, or whose engine fails mid-run** (4-15's
  review): the helper reaches the stand-in only through the QA gate (``qa_phase3.ModelGate``), which forwards a
  completion only while the model is listed loaded and closes at the first failure, and the run ends there.

The binary is the one ``cargo xtask develop`` builds into ``CITAR_EXT_DIR``, as in ``tests/test_helper_e2e.py``:
without it the module skips, except on CI, where it fails.
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)

import importlib.util
import os
import shutil
import tempfile
import unittest
from unittest import mock
from pathlib import Path

from citar import db, servers, settings
from citar.db.models import Base
from citar.pool import scheduler as S
from tests import helperfake as H

ROOT = Path(__file__).resolve().parent.parent
_TMP = Path(tempfile.mkdtemp(prefix="citar_helper_llm_real_"))
_URL = "sqlite:///" + (_TMP / "test.db").as_posix()
CI = bool(os.environ.get("CI"))
#: The stand-in's long generation here: 100 words over 6 s, so a run that does not stop it holds the model about 4 s
#: past the cancel at 2 s (the QA script's own stand-in takes 40 s for 1,500).
LONG_S, LONG_WORDS = 6.0, 100
_SAVED: dict = {}


def _qa4():
    spec = importlib.util.spec_from_file_location("qa_phase4_for_llm_real", ROOT / "scripts" / "qa_phase4.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


qa4 = _qa4()
MODEL = qa4.REAL_MODEL


def setUpModule():
    H.require_helper_binary("the llm-real checks against a stand-in")   # skips locally without one; fails on CI
    _SAVED.update({k: os.environ.get(k) for k in ("CITAR_DATA_DIR", "CITAR_DB_URL")})
    os.environ["CITAR_DATA_DIR"] = str(_TMP)
    os.environ["CITAR_DB_URL"] = _URL
    settings.reset()
    db.dispose()
    db.configure(_URL)
    Base.metadata.drop_all(db.engine())
    db.create_all()
    servers.invalidate()
    S.reset()


def tearDownModule():
    # the pool's connections closed here, not left to the interpreter's exit (two "unclosed database" warnings under
    # -X dev when the module ran alone), and the environment as it was for the modules after this one
    S.reset()
    db.dispose()
    for k, v in _SAVED.items():
        if v is None:
            os.environ.pop(k, None)
        else:
            os.environ[k] = v
    settings.reset()
    servers.invalidate()
    shutil.rmtree(_TMP, ignore_errors=True)


class StandIn(unittest.TestCase):

    def stand_in(self, **kw):
        ep = qa4.stand_in(**{"long_s": LONG_S, "long_words": LONG_WORDS, **kw})
        self.addCleanup(ep.close)
        return ep

    def exercise(self, ep) -> tuple:
        """The part's checks against ``ep``: (the checks, as the harness records them; what the run measured; its
        state directory)."""
        checks: list = []

        def check(name: str, ok: bool, detail: str = "", **numbers) -> bool:
            checks.append({"check": name, "ok": bool(ok), "detail": detail, **numbers})
            return bool(ok)
        data: dict = {}
        state = Path(tempfile.mkdtemp(prefix="run-", dir=_TMP))
        qa4.exercise(check, ep.url, state, data, label="the stand-in", ack_bound=1.0 * (3 if CI else 1))
        return checks, data, state

    @staticmethod
    def said(checks: list) -> str:
        return "\n".join(f"[{'ok' if c['ok'] else 'FAIL'}] {c['check']}: {c['detail']}" for c in checks)

    def test_the_checks_pass_against_a_stand_in_that_stops_a_cancelled_generation(self):
        ep = self.stand_in()
        checks, data, _state = self.exercise(ep)
        self.assertEqual([c["ok"] for c in checks], [True] * 6, self.said(checks))
        self.assertEqual(data["tool_call"]["names"], ["get_empire"], "the stand-in's streamed call, parsed here")
        # the stand-in's side: the cancel reached it, mid-answer
        side = qa4.endpoint_side(ep, data["cancel_at"])
        self.assertEqual((side["completions"], side["streamed"], side["first_tools"]), (8, 8, 2), side)
        self.assertIsNotNone(side["gone_after_cancel_s"], f"the long answer's client did not go: {side}")
        self.assertLess(side["gone_after_cancel_s"], 1.0 * (3 if CI else 1), side)
        self.assertLessEqual(side["events_after_cancel"], 1, f"events after the cancel: {side}")
        # a long answer under way (its words 60 ms apart: about 33 out by the cancel), cut short
        self.assertTrue(10 <= side["long_events"] < LONG_WORDS, f"not cut short mid-answer: {side}")
        self.assertIsNone(side["ran_on_s"], "a stand-in that stops does not run on")
        # the gate in front of it forwarded every completion, the cancelled one counted as its client's, no failure
        gate = data["gate"]
        self.assertEqual((gate["forwarded"], gate["refused"], gate["cancelled"], gate["tripped"]), (8, 0, 1, None),
                         gate)

    def test_the_checks_fail_against_a_stand_in_that_runs_on_after_a_cancel(self):
        ep = self.stand_in(runs_on=True)
        checks, data, _state = self.exercise(ep)
        failed = [c["check"] for c in checks if not c["ok"]]
        self.assertEqual(len(checks), 6, self.said(checks))
        self.assertEqual(len(failed), 1, self.said(checks))
        self.assertTrue(failed[0].startswith("three short requests after the cancel"), self.said(checks))
        # the helper's side is right: the cancel is acknowledged at once; the model is what is held
        self.assertTrue(checks[3]["ok"] and checks[3]["check"].startswith("a 1,500-token generation cancelled"),
                        self.said(checks))
        self.assertGreater(data["after_s"][0], data["bound_s"], data)
        side = qa4.endpoint_side(ep, data["cancel_at"])
        self.assertEqual(side["completions"], 8, side)
        self.assertGreater(side["ran_on_s"] or 0, 2.0, f"the abandoned generation held the model: {side}")

    def test_the_checks_fail_when_the_long_generation_ends_before_the_cancel(self):
        # a generation over before the cancel tests no cancel: its check fails at once, saying so, and the rest still
        # run. Over in 0.2 s once it starts, and the cancel 10 s after the request here: the 2 s of the gate count
        # from before its ticket, the hub and the helper, and CI's macOS runner had not had the answer back by then
        # twice (test.yml runs 38717379734 and 38986642828), so the check it is meant to fail passed.
        ep = self.stand_in(long_s=0.2)
        with mock.patch.object(qa4, "CANCEL_AT", 10.0):
            checks, data, _state = self.exercise(ep)
        failed = [c for c in checks if not c["ok"]]
        self.assertEqual(len(checks), 6, self.said(checks))
        self.assertEqual(len(failed), 1, self.said(checks))
        self.assertTrue(failed[0]["check"].startswith("a 1,500-token generation cancelled"), self.said(checks))
        self.assertIn("answered before the cancel", failed[0]["detail"])
        self.assertIsNone(data["ack_s"])

    def test_a_model_not_loaded_fails_the_checks_with_nothing_sent_to_it(self):
        ep = self.stand_in(loaded=False)
        checks, data, state = self.exercise(ep)
        self.assertEqual([c["ok"] for c in checks], [False], self.said(checks))
        self.assertIn("load it there first", checks[0]["detail"])
        self.assertEqual((ep.arrivals, ep.completions()), ([], []), "a completion went to a model that is not loaded")
        self.assertFalse((state / "helper-llm-real").exists(), "no helper was started")

    def test_a_model_unloaded_during_the_run_fails_the_checks(self):
        ep = self.stand_in()
        ep.unload_at = 8                         # LM Studio's model goes when the last short request comes
        checks, data, _state = self.exercise(ep)
        self.assertEqual([c["ok"] for c in checks], [True] * 5 + [False], self.said(checks))
        self.assertEqual(checks[-1]["check"], f"{MODEL} is still loaded after the part")
        self.assertIn("not-loaded", data["after"])
        self.assertEqual(len(ep.completions()), 8)

    def test_a_model_unloaded_after_its_first_completion_is_sent_nothing_more(self):
        # 4-15's review: a model that goes under the run (its engine crashed, an idle timeout) would be loaded again
        # just in time by the next completion that names it. This stand-in answers completions after the unload, as
        # such an LM Studio would (loading the model): none reaches it, the model's state read before the requests
        # before the cancel ending the run there, and the gate reading it before every completion.
        ep = self.stand_in()
        ep.unload_at = 1
        checks, data, _state = self.exercise(ep)
        self.assertEqual([c["ok"] for c in checks], [True, True, True, False, False], self.said(checks))
        self.assertTrue(checks[3]["check"].startswith("three short requests after the cancel"), self.said(checks))
        self.assertIn("nothing more was sent before the requests before the cancel", checks[3]["detail"])
        self.assertEqual(len(ep.arrivals), 1, "a completion reached the model after it was unloaded")
        self.assertEqual(data["gate"]["forwarded"], 1, data["gate"])
        self.assertIn("not-loaded", data["gate"]["tripped"]["why"])

    def test_a_model_whose_engine_fails_mid_run_is_sent_nothing_after_it(self):
        # LM Studio's crash in 4-15's first real run: a completion's stream carried an error event, the model was
        # unloaded, and the helper forwarded eleven more completions within a second. Here the second completion (the
        # first short request) fails so, and the stand-in refuses later ones as LM Studio did with its just-in-time
        # loading off: the gate closes at the error event, the run ends at that request, and nothing more reaches the
        # stand-in, which would have loaded the model again had its just-in-time loading been on.
        ep = self.stand_in()
        ep.fail_at, ep.refuse_unloaded = 2, True
        checks, data, _state = self.exercise(ep)
        self.assertEqual([c["ok"] for c in checks], [True, True, True, False, False], self.said(checks))
        self.assertTrue(checks[3]["check"].startswith("three short requests after the cancel"), self.said(checks))
        self.assertIn("a short request before the cancel failed", checks[3]["detail"])
        self.assertIn("the gate closed: a completion failed: an error event", checks[3]["detail"])
        self.assertEqual((len(ep.arrivals), ep.refused), (2, []), "a completion reached the model after it failed")
        gate = data["gate"]
        self.assertEqual((gate["forwarded"], gate["cancelled"]), (2, 0), gate)
        self.assertIn("terminated", gate["tripped"]["why"])


if __name__ == "__main__":
    unittest.main()
