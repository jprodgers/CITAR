"""Protocol 2's conformance transcripts from the hub's side (design/phase4-helper.md P4.2.9; packages 4-05's gate 1,
4-06's and 4-10's gate 1).

Every transcript whose ``plays.hub`` is 4-05 or 4-06 is played against the real hub (the app under uvicorn, a real
WebSocket) by ``tests/helperfake.HubPlayer``: the handshake with every ``refused`` code and the v1 refusal, settings
(capped, not lent, pushed, an overclaim clamped), catalog, hardware (a rescan's frame kept as the last report, an
unchanged machine sending none), power (a minute's samples written as the machine's power row), status, completions
(streamed, not streamed, usage absent, busy, closed, unknown model, an endpoint error, a timeout, a broken ``done``, a
stopping helper), cancel (before the first token, mid-stream, after ``done``, never acknowledged), the lost connection,
keep-alive and shutdown, and the jobs (4-10: an offer to its acknowledgement, every reject reason, abort, a deadline,
memory and host failures, reconnects keeping and dropping a run, a reconnect after a sent result, a lost
acknowledgement, a held result not wanted, a duplicate result, a lost offer, a silent run, re-offers backing off, a
malformed job frame, a benchmark's pause, and from 4-10's fix round a pause across a reconnect (its lease ended, and
live), a stopping helper's runs, a job host's figures acknowledged again, and a result that crossed its experiment's
deletion), each against a job dispatcher of its own whose one executor is the helper. Each is a test of its own, named after its file. The helper's side plays the same files
(``crates/citar-helper/tests/conformance.rs``).

From 4-13 the update transcripts too (a welcome carrying the relayed manifest, one the server learns of pushed to
the helper, the helper's update_status kept): every transcript with a hub side is played here, and
``test_the_package_plays_its_transcripts`` holds the count; the player refuses any act or state word it cannot do
rather than skip it.
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)

import os
import shutil
import tempfile
import unittest
from pathlib import Path

from citar import db, servers, settings
from citar.db.models import Base
from citar.pool import scheduler as S
from tests import helper_fixtures as F
from tests import helperfake as H

_TMP = Path(tempfile.mkdtemp(prefix="citar_conformance_"))
_URL = "sqlite:///" + (_TMP / "test.db").as_posix()
_SAVED: dict = {}

TRANSCRIPTS = (H.hub_transcripts("4-05") + H.hub_transcripts("4-06") + H.hub_transcripts("4-10")
               + H.hub_transcripts("4-13"))


def setUpModule():
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
    # the pool's connections closed here, not left to the interpreter's exit ("unclosed database" warnings under
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


class Transcripts(unittest.TestCase):
    """One test per transcript, against one hub for the class."""

    @classmethod
    def setUpClass(cls):
        cls.server = H.HubServer()

    @classmethod
    def tearDownClass(cls):
        cls.server.stop()
        S.reset()

    def play(self, name: str) -> None:
        t = next(t for t in TRANSCRIPTS if t.name == name)
        H.HubPlayer(self.server, t).play()

    def test_the_package_plays_its_transcripts(self):
        """The 40 transcripts 4-05 plays from the hub's side, 4-06's 3, 4-10's 30 and 4-13's 3 are all here (every
        transcript the hub plays), and the player knows every word they use."""
        self.assertEqual(len(TRANSCRIPTS), 76)
        self.assertEqual(sum(1 for t in TRANSCRIPTS if t.plays.get("hub") == "4-10"), 30)
        self.assertEqual(sorted(t.name for t in TRANSCRIPTS if t.plays.get("hub") == "4-06"),
                         ["hardware-rescan", "hardware-unchanged", "power-minute"])
        self.assertEqual(sorted(t.name for t in TRANSCRIPTS if t.plays.get("hub") == "4-13"),
                         ["update-notify", "update-pushed", "update-unavailable"])
        self.assertEqual(sorted(t.name for t in TRANSCRIPTS),
                         sorted(t.name for t in F.transcripts() if t.plays.get("hub")))
        for t in TRANSCRIPTS:
            self.assertIs(H.played(t), True, t.name)
        # a word no package plays is refused, not skipped
        made_up = F.Transcript(name="made-up", about="", refs=[], plays={"hub": "4-99"}, one_side="", values={},
                               setup={}, steps=[F.Step(action="act", body={"side": "hub", "do": "manifest_vanishes"})])
        self.assertEqual(H.played(made_up), "manifest_vanishes")


def _make(name: str):
    def test(self):
        self.play(name)
    test.__doc__ = next(t.about for t in TRANSCRIPTS if t.name == name)
    return test


for _t in TRANSCRIPTS:
    setattr(Transcripts, "test_" + _t.name.replace("-", "_"), _make(_t.name))
del _t


if __name__ == "__main__":
    unittest.main()
