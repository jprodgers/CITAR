"""The helper update relay (design/phase4-helper.md P4.5.9; package 4-13's gate 5) and the extension's check of a
manifest, by the same Rust as the helper's.

- ``Verify``: ``engine_api.verify_manifest`` refuses everything while the build trusts no key (``no_key``: no key ships
  in Phase 4), takes a manifest signed by a key ``CITAR_HELPER_UPDATE_KEY`` names (the test operations only), and
  refuses a bad signature, another key's, a changed byte and a manifest that does not read; versions compare as semver.
- ``Relay``: fetched at most hourly (a fake clock and a counting source), a failed fetch and an unverifiable manifest
  each keep the last good one, an unverifiable one is never relayed, a manifest of another protocol is refused, a new
  one is pushed and the same one is not; the source from the settings (a file wins over a URL; none in local mode;
  ``https`` only); its thread fetches at start.
- ``Hub``: a real hub's welcome carries the relayed manifest (null without one); one the server learns of while helpers
  are connected goes to the older helper and not to one as new as it; ``update_status`` is kept per machine across a
  reconnect, and the Servers page's view says the latest signed helper and whether it is newer; an update's hand-off
  (the old socket closed, then the new binary's hello) adds no ``replaced``.
"""
from __future__ import annotations

import json
import os
import tempfile
import unittest
from pathlib import Path

import tests  # noqa: F401
from tests import has_test_ops

_TMP = Path(tempfile.mkdtemp(prefix="citar_updates_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()

from citar import db, engine_api, servers, settings
from citar.auth import accounts, tokens
from citar.db.models import Base, Server, WorkerToken
from citar.pool import scheduler as S
from citar.server import helper_updates as U
from citar.server import workers as W
from tests import helperfake as H

SEED = 61


class _Key:
    """``CITAR_HELPER_UPDATE_KEY`` naming the throwaway key of ``seed`` (or nothing) for a block."""

    def __init__(self, seed=SEED):
        self.seed = seed

    def __enter__(self):
        self.saved = os.environ.get("CITAR_HELPER_UPDATE_KEY")
        if self.seed is None:
            os.environ.pop("CITAR_HELPER_UPDATE_KEY", None)
        else:
            os.environ["CITAR_HELPER_UPDATE_KEY"] = H.update_public_key(self.seed)
        return self

    def __exit__(self, *exc):
        if self.saved is None:
            os.environ.pop("CITAR_HELPER_UPDATE_KEY", None)
        else:
            os.environ["CITAR_HELPER_UPDATE_KEY"] = self.saved


@unittest.skipUnless(has_test_ops(), "needs the extension's test operations (cargo xtask develop)")
def committed_keys() -> int:
    """How many update keys crates/citar-jobs/update-keys.pub holds (none in Phase 4; the owner's one-time setup adds
    one): what the extension trusts besides ``CITAR_HELPER_UPDATE_KEY``."""
    text = (Path(__file__).resolve().parent.parent / "crates" / "citar-jobs" / "update-keys.pub").read_text(
        encoding="utf-8")
    return len({line.strip() for line in text.splitlines() if line.strip() and not line.strip().startswith("#")})


class Verify(unittest.TestCase):
    def test_the_keys_trusted_and_a_trusted_one_verifies(self):
        text, sig = H.signed_manifest("0.2.0", seed=SEED)
        with _Key(None):
            self.assertEqual(engine_api.update_keys_trusted(), committed_keys(), "update-keys.pub's, none in Phase 4")
            with self.assertRaises(engine_api.ManifestRefused) as e:
                engine_api.verify_manifest(text, sig)
            # no key: nothing verifies; the owner's key committed: the throwaway key's signature is not its
            self.assertEqual(e.exception.code, "signature" if committed_keys() else "no_key")
        with _Key(SEED):
            self.assertEqual(engine_api.update_keys_trusted(), committed_keys() + 1)
            m = engine_api.verify_manifest(text, sig)
            self.assertEqual((m["v"], m["version"], m["protocol"]), (1, "0.2.0", 2))
            self.assertEqual(m["build"], engine_api.build_info()["build_id"])
            asset = m["assets"]["x86_64-pc-windows-msvc"]
            self.assertEqual(len(asset["sha256"]), 64)
            # another key's, a changed byte, a bad signature, a manifest that does not read
            _, other = H.signed_manifest("0.2.0", seed=SEED + 1)
            for bad_text, bad_sig, code in ((text, other, "signature"), (text.replace("0.2.0", "0.2.1"), sig, "signature"),
                                            (text, "AAAA", "signature")):
                with self.assertRaises(engine_api.ManifestRefused) as e:
                    engine_api.verify_manifest(bad_text, bad_sig)
                self.assertEqual(e.exception.code, code)
            junk = '{"v": 1, "version": "seven"}'
            with self.assertRaises(engine_api.ManifestRefused) as e:
                engine_api.verify_manifest(junk, H.sign_manifest_text(junk, SEED))
            self.assertEqual(e.exception.code, "invalid")
            with self.assertRaises(engine_api.ManifestRefused):
                engine_api.verify_manifest(text, None)

    def test_versions_compare_as_semver(self):
        newer = engine_api.helper_version_newer
        self.assertIs(newer("0.1.6-test.2", "0.1.6-test.1"), True)
        self.assertIs(newer("0.1.6", "0.1.6-test.2"), True)
        self.assertIs(newer("0.1.6", "0.1.6"), False)
        self.assertIs(newer("0.1.5", "0.1.6"), False)
        self.assertIsNone(newer("seven", "0.1.6"))


class _Clock:
    def __init__(self):
        self.now = 1_800_000_000.0

    def __call__(self):
        return self.now


@unittest.skipUnless(has_test_ops(), "needs the extension's test operations (cargo xtask develop)")
class Relay(unittest.TestCase):
    def setUp(self):
        self.key = _Key(SEED)
        self.key.__enter__()
        self.addCleanup(self.key.__exit__)
        self.served = [H.signed_manifest("0.2.0", seed=SEED)]
        self.fetched = 0
        self.failing = False
        self.pushed = []
        self.clock = _Clock()

        def source():
            self.fetched += 1
            if self.failing:
                raise U.FetchFailed("GitHub is down")
            return self.served[-1]
        self.relay = U.Relay(source, clock=self.clock, push=self.pushed.append)

    def test_fetched_at_most_hourly_and_a_failure_keeps_the_last_good_one(self):
        r = self.relay
        got = r.check()
        self.assertEqual(got.version, "0.2.0")
        self.assertEqual(r.offer(), {"manifest": self.served[0][0], "signature": self.served[0][1]})
        self.assertEqual([p.version for p in self.pushed], ["0.2.0"])
        # within the hour: nothing fetched, whatever the source holds now
        self.served.append(H.signed_manifest("0.3.0", seed=SEED))
        self.clock.now += U.FETCH_SECONDS - 1
        self.assertIsNone(r.check())
        self.assertEqual(self.fetched, 1)
        # an hour on: fetched, accepted, pushed
        self.clock.now += 1
        self.assertEqual(r.check().version, "0.3.0")
        self.assertEqual(self.fetched, 2)
        # a failed fetch keeps it
        self.failing = True
        self.clock.now += U.FETCH_SECONDS
        with self.assertLogs("citar.helper_updates", "WARNING") as logged:
            self.assertIsNone(r.check())
        self.assertIn("still relaying 0.3.0", "\n".join(logged.output))
        self.assertEqual(r.current().version, "0.3.0")
        # and the next failure within the hour is not logged again
        self.clock.now += U.FETCH_SECONDS
        r._last_logged = self.clock.now - 10
        with self.assertNoLogs("citar.helper_updates", "WARNING"):
            self.assertIsNone(r.check())
        # the same manifest fetched again is not pushed again
        self.failing = False
        self.clock.now += U.FETCH_SECONDS
        self.assertEqual(r.check().version, "0.3.0")
        self.assertEqual([p.version for p in self.pushed], ["0.2.0", "0.3.0"])

    def test_an_unverifiable_manifest_is_never_relayed(self):
        r = self.relay
        text, sig = self.served[0]
        # another key's signature: refused before anything was relayed
        self.served.append((text, H.signed_manifest("0.2.0", seed=SEED + 9)[1]))
        self.assertIsNone(r.check())
        self.assertIsNone(r.current())
        self.assertEqual(r.refusals, 1)
        self.assertEqual(self.pushed, [])
        # a good one, then a tampered one: the good one stays
        self.served.append((text, sig))
        self.clock.now += U.FETCH_SECONDS
        self.assertEqual(r.check().version, "0.2.0")
        self.served.append((text.replace("0.2.0", "0.9.0"), sig))
        self.clock.now += U.FETCH_SECONDS
        self.assertIsNone(r.check())
        self.assertEqual(r.current().version, "0.2.0")
        self.assertEqual(r.offer()["manifest"], text)
        # a manifest of another protocol is not this hub's to relay
        other = json.loads(text)
        other["protocol"] = 3
        other_text = json.dumps(other)
        with self.assertRaises(ValueError):
            r.accept(other_text, H.sign_manifest_text(other_text, SEED))
        self.assertEqual(r.current().version, "0.2.0")
        # nor one of another engine build: helpers follow their server's own build
        other = json.loads(text)
        other["build"] = "0123456789ab" if other["build"] != "0123456789ab" else "ba9876543210"
        other_text = json.dumps(other)
        with self.assertRaises(ValueError) as e:
            r.accept(other_text, H.sign_manifest_text(other_text, SEED))
        self.assertIn("engine build", str(e.exception))
        self.assertEqual(r.current().version, "0.2.0")
        self.assertEqual([p.version for p in self.pushed], ["0.2.0"])

    def test_a_build_that_trusts_no_key_fetches_nothing(self):
        """No key, no manifest could verify: the relay neither fetches (GitHub, hourly) nor warns; it says so once, at
        INFO, and starts no thread."""
        from unittest import mock
        with mock.patch.object(engine_api, "update_keys_trusted", return_value=0):
            with self.assertLogs("citar.helper_updates", "INFO") as logged:
                self.assertIsNone(self.relay.check())
                self.clock.now += U.FETCH_SECONDS
                self.assertIsNone(self.relay.check())
                self.relay.start()
            self.assertIsNone(self.relay._thread, "no thread")
        self.assertEqual(self.fetched, 0, "nothing fetched")
        self.assertEqual(self.relay.fetches, 0)
        lines = [line for line in logged.output if "trusts no helper update key" in line]
        self.assertEqual(len(lines), 1, logged.output)
        self.assertTrue(all(line.startswith("INFO") for line in logged.output), logged.output)
        # a key trusted again (a test operation's): fetched as usual
        self.assertEqual(self.relay.check().version, "0.2.0")
        self.assertEqual(self.fetched, 1)

    def test_the_source_comes_from_the_settings(self):
        folder = Path(tempfile.mkdtemp(prefix="citar_manifest_"))
        manifest = folder / "citar-helper-manifest.json"
        text, sig = H.signed_manifest("0.2.0", seed=SEED)
        manifest.write_bytes(text.encode("utf-8"))      # the bytes as published: the signature is over them
        (folder / "citar-helper-manifest.json.sig").write_text(sig + "\n", encoding="utf-8")
        try:
            settings.set_for_test(helper_manifest_file=str(manifest))
            source = U.configured_source()
            self.assertEqual(source(), (text, sig))
            settings.set_for_test(helper_manifest_url="off")
            self.assertIsNone(U.configured_source())
            settings.set_for_test(helper_manifest_url="https://example.invalid/v{version}/m.json")
            self.assertIsNotNone(U.configured_source())
            with self.assertRaises(settings.SettingsError):
                settings.set_for_test(helper_manifest_url="http://example.invalid/m.json")
            # the default: none in local mode, GitHub's in server mode
            saved = os.environ.pop("CITAR_HELPER_MANIFEST_URL", None)
            try:
                self.assertEqual(settings.load(mode="local").helper_manifest_url, "")
                self.assertEqual(settings.load(mode="server", secret_key="k" * 40,
                                               public_origin="https://citar.test").helper_manifest_url,
                                 settings.HELPER_MANIFEST_URL)
            finally:
                if saved is not None:
                    os.environ["CITAR_HELPER_MANIFEST_URL"] = saved
            # a thread of its own fetches at start
            settings.set_for_test(helper_manifest_file=str(manifest))
            relay = U.Relay(U.configured_source())
            relay.start()
            try:
                self.assertTrue(H.until(lambda: relay.current() is not None, 10), "fetched at start")
                self.assertEqual(relay.current().version, "0.2.0")
            finally:
                relay.stop()
        finally:
            settings.reset()


def _reset_db():
    db.dispose()
    db.configure(os.environ["CITAR_DB_URL"])
    Base.metadata.drop_all(db.engine())
    db.create_all()
    servers.invalidate()


def _machine(owner_id: str, name: str) -> tuple:
    with db.session() as s:
        server = Server(owner_id=owner_id, name=name, kind="owned", provider="lmstudio", reach="worker",
                        max_concurrent=1, config={"models": [{"key": "model-a"}]})
        s.add(server)
        s.flush()
        raw, hashed = tokens.new_pair()
        s.add(WorkerToken(server_id=server.id, token_hash=hashed, prefix=tokens.prefix(raw), label="t"))
        servers.invalidate()
        return server.id, raw


@unittest.skipUnless(has_test_ops(), "needs the extension's test operations (cargo xtask develop)")
class Hub(unittest.TestCase):
    """A real hub (the app under uvicorn) with the process's relay."""

    @classmethod
    def setUpClass(cls):
        _reset_db()
        S.reset()
        with db.session() as s:
            owner = accounts.create_user(s, handle="ula", email="ula@x.cc", password="a long enough phrase",
                                         role="user", status="active", email_verified=True)
            cls.owner = owner.id
        cls.key = _Key(SEED)
        cls.key.__enter__()
        cls.server = H.HubServer()

    @classmethod
    def tearDownClass(cls):
        cls.server.stop()
        U.relay().clear()
        cls.key.__exit__()
        S.reset()
        db.dispose()

    def setUp(self):
        U.relay().clear()
        W.hub().limiter.clear()

    def connect(self, token: str, version: str, key_seed: int) -> tuple:
        helper = H.FakeHelper(self.server.url)
        self.addCleanup(helper.close)
        welcome = helper.handshake(H.hello(token=token, key=H.key_b64(key_seed), version=version))
        return helper, welcome

    def test_welcomes_carry_the_relayed_manifest(self):
        sid, token = _machine(self.owner, "welcome")
        helper, welcome = self.connect(token, "0.1.6", 3)
        self.assertIsNone(welcome["update"], "no manifest relayed: null")
        helper.close()
        text, sig = H.signed_manifest("0.2.0", seed=SEED)
        U.relay().accept(text, sig)
        helper, welcome = self.connect(token, "0.1.6", 3)
        self.assertEqual(welcome["update"], {"manifest": text, "signature": sig})

    def test_a_new_manifest_goes_to_older_helpers_only(self):
        old_sid, old_token = _machine(self.owner, "older")
        new_sid, new_token = _machine(self.owner, "as new")
        older, _ = self.connect(old_token, "0.1.6", 4)
        as_new, _ = self.connect(new_token, "0.2.0", 5)
        text, sig = H.signed_manifest("0.2.0", seed=SEED)
        U.relay().accept(text, sig)
        frame = older.recv(timeout=5, skip=("ping", "settings"))
        self.assertEqual((frame["t"], frame["manifest"], frame["signature"]), ("update", text, sig))
        with self.assertRaises(TimeoutError):
            as_new.recv(timeout=1.5, skip=("ping", "settings"))
        # the same manifest again: nothing more
        U.relay().accept(text, sig)
        with self.assertRaises(TimeoutError):
            older.recv(timeout=1.0, skip=("ping", "settings"))

    def test_update_status_is_kept_across_a_reconnect_and_shown(self):
        sid, token = _machine(self.owner, "status")
        text, sig = H.signed_manifest("0.2.0", seed=SEED)
        U.relay().accept(text, sig)
        helper, _ = self.connect(token, "0.1.6", 6)
        helper.send("update_status", state="restarting", version="0.2.0", message="restarting into 0.2.0")
        self.assertTrue(H.until(lambda: (W.hub().update_states.get(sid) or {}).get("state") == "restarting", 5))
        view = W.hub().get(sid).client()["update"]
        self.assertEqual(view["latest"]["version"], "0.2.0")
        self.assertTrue(view["available"])
        self.assertEqual(view["status"]["state"], "restarting")
        # the hand-off: the old socket closes first, then the new binary says hello and applied
        replaced = W.hub().replaced
        helper.close(code=1001)
        self.assertTrue(H.until(lambda: W.hub().get(sid) is None, 5), "the old connection is gone")
        helper, _ = self.connect(token, "0.2.0", 6)
        helper.send("update_status", state="applied", version="0.2.0", message="applied")
        self.assertTrue(H.until(lambda: (W.hub().update_states.get(sid) or {}).get("state") == "applied", 5))
        view = W.hub().get(sid).client()["update"]
        self.assertFalse(view["available"], "the helper is the latest now")
        self.assertEqual(W.hub().replaced, replaced, "an update's hand-off is never a replaced connection")

    def test_update_status_lines_are_logged_once_and_a_helpers_own_release_quietly(self):
        """Every status is kept, but only news is logged: a state or version other than the machine's last, at most
        one line a state a minute; a helper refusing the release it already runs (each welcome relays it) at DEBUG."""
        sid, token = _machine(self.owner, "chatty")
        helper, _ = self.connect(token, "0.1.6", 7)
        with self.assertLogs("citar.workers", "INFO") as logged:
            for n in range(40):
                helper.send("update_status", state="refused" if n % 2 else "failed", version="0.2.0",
                            message=f"say it again ({n})")
            helper.send("update_status", state="applied", version="0.2.0", message="the end")
            self.assertTrue(H.until(lambda: (W.hub().update_states.get(sid) or {}).get("state") == "applied", 5))
        self.assertEqual(len(W.hub().update_log[sid]), 32, "every status kept, the last 32")
        said = [line for line in logged.output if ": update " in line]
        self.assertEqual(len(said), 3, said)          # failed, refused and applied once each
        # a helper at 0.2.0 refusing 0.2.0 (already at it): kept, logged at DEBUG only
        sid2, token2 = _machine(self.owner, "at the release")
        at_it, _ = self.connect(token2, "0.2.0", 8)
        with self.assertLogs("citar.workers", "DEBUG") as logged:
            at_it.send("update_status", state="refused", version="0.2.0", message="already at 0.2.0")
            self.assertTrue(H.until(lambda: (W.hub().update_states.get(sid2) or {}).get("state") == "refused", 5))
        said = [line for line in logged.output if ": update " in line]
        self.assertEqual([line.split(":", 1)[0] for line in said], ["DEBUG"], logged.output)


def tearDownModule():
    db.dispose()


if __name__ == "__main__":
    unittest.main()
