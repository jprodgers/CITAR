"""Protocol 2's conformance transcripts from the hub's side (design/phase4-helper.md P4.2.9; package 4-05's gate 1).

Every transcript whose ``plays.hub`` is 4-05 is played against the real hub (the app under uvicorn, a real WebSocket)
by ``tests/helperfake.HubPlayer``: the handshake with every ``refused`` code and the v1 refusal, settings (capped, not
lent, pushed, an overclaim clamped), catalog, status, completions (streamed, not streamed, usage absent, busy, closed,
unknown model, an endpoint error, a timeout, a broken ``done``, a stopping helper), cancel (before the first token,
mid-stream, after ``done``, never acknowledged), the lost connection, keep-alive and shutdown. Each is a test of its own,
named after its file. The helper's side plays the same files (``crates/citar-helper/tests/conformance.rs``).

A transcript of a later package (4-06, 4-10, 4-13) is not played here; ``test_the_package_plays_its_transcripts`` holds
the count, and the player refuses any act or state word it cannot do rather than skip it.
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)

import os
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

TRANSCRIPTS = H.hub_transcripts("4-05")


def setUpModule():
    os.environ["CITAR_DATA_DIR"] = str(_TMP)
    os.environ["CITAR_DB_URL"] = _URL
    settings.reset()
    db.dispose()
    db.configure(_URL)
    Base.metadata.drop_all(db.engine())
    db.create_all()
    servers.invalidate()
    S.reset()


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
        """The 40 transcripts 4-05 plays from the hub's side are all here, and the player knows every word they
        use."""
        self.assertEqual(len(TRANSCRIPTS), 40)
        for t in TRANSCRIPTS:
            self.assertIs(H.played(t), True, t.name)
        # a word of a later package is refused, not skipped
        later = next(t for t in F.transcripts() if t.plays.get("hub") == "4-10")
        self.assertIsNot(H.played(later), True)


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
