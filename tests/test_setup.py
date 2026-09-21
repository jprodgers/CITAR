"""First-run setup and the operator console.

Setup code is the least-exercised code in most projects — it runs once, on a machine nobody is
watching, usually by somebody who has no idea what it should have done. So the parts that can be
tested without a terminal are tested here:

* detection, including the case that matters most, where nothing is installed;
* the rule that decides whether to offer setup at all;
* the registry writes the wizard performs;
* that the setup routes are gated like every other route — a setup endpoint that skipped
  authorisation would be the most useful thing on a public server to an attacker.
"""
from __future__ import annotations

import os
import shutil
import tempfile
import unittest
from pathlib import Path

import tests  # noqa: F401

_TMP = Path(tempfile.mkdtemp(prefix="citar_setup_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "local")

from citar import db, servers as registry, settings
from citar.auth import policy
from citar.server import setup_api
from citar.wizard import detect

settings.reset()
db.configure(os.environ["CITAR_DB_URL"])
db.create_all()


class Detection(unittest.TestCase):
    def test_a_closed_port_is_not_reachable(self):
        # Port 9 (discard) is reserved and never listening, so this is the "nothing installed"
        # path — the one a new user hits, and the one that must not hang.
        endpoint = detect.probe("openai_compatible", "nothing", "http://127.0.0.1:9/v1")
        self.assertFalse(endpoint.reachable)
        self.assertEqual(endpoint.models, [])
        self.assertIn("not running", endpoint.summary)

    def test_probe_never_raises(self):
        # Detection runs before anything is configured; an exception here would abort setup.
        for url in ("http://127.0.0.1:9/v1", "http://not-a-host.invalid/v1", "gibberish"):
            self.assertFalse(detect.probe("openai_compatible", "x", url).reachable)

    def test_suggestions_cover_every_machine(self):
        # Including 0 GB, which is what a server with no GPU reports, and an implausibly large
        # card. A gap in the table would mean no suggestion at all for somebody.
        for vram in (0, 3.9, 4, 6, 8, 12, 16, 24, 48, 512):
            name, reason = detect.suggest_model(vram)
            self.assertTrue(name and reason, f"no suggestion for {vram} GB")

    def test_integrated_graphics_are_not_counted(self):
        """Intel integrated graphics report "VRAM" that is really a slice of system memory.

        Believing it produces a model suggestion the machine cannot honour, which fails at load
        time with an out-of-memory error that reads as a CITAR bug.
        """
        info = {"gpus": [{"name": "Intel(R) UHD Graphics", "vendor": "Intel", "vram_gb": 1.0}]}
        self.assertEqual(detect.usable_vram_gb(info), 0.0)

    def test_the_largest_single_card_is_what_counts(self):
        # Not the total: a model has to fit in one of them unless the runtime is set up to split
        # it, which is not something to assume during first-time setup.
        info = {"gpus": [{"name": "A", "vendor": "NVIDIA", "vram_gb": 8.0},
                         {"name": "B", "vendor": "NVIDIA", "vram_gb": 12.0}]}
        self.assertEqual(detect.usable_vram_gb(info), 12.0)

    def test_apple_unified_memory_is_usable(self):
        info = {"gpus": [], "memory": {"unified": True, "ram_gb": 32}}
        self.assertGreater(detect.usable_vram_gb(info), 15)

    def test_describe_hardware_always_says_something(self):
        self.assertTrue(detect.describe_hardware({}))


class OfferingSetup(unittest.TestCase):
    """The test for "does this installation need setting up?"."""

    def setUp(self):
        self._saved = registry.snapshot()

    def tearDown(self):
        registry.save(self._saved)

    def test_placeholder_servers_do_not_count_as_a_model(self):
        """A fresh registry ships a dry-run server and an API entry with no key.

        Neither can take a seat, so "the registry file exists" is the wrong test for whether
        somebody has finished setting up — it would skip the wizard for everyone.
        """
        registry.save({**registry.empty_registry(), "servers": [
            registry.default_server(kind="test", provider="dryrun"),
            {**registry.default_server(kind="owned", provider="lmstudio"),
             "connection": {"provider": "lmstudio", "base_url": "", "key": {"backend": "none"}},
             "models": [registry.default_model("something")]},
        ]})
        self.assertEqual(setup_api._playable_models(), 0)

    def test_a_reachable_server_with_a_model_counts(self):
        server = registry.default_server(kind="owned", provider="lmstudio")
        server["connection"]["base_url"] = "http://localhost:1234/v1"
        server["models"] = [registry.default_model("some-model")]
        registry.save({**registry.empty_registry(), "servers": [registry.normalize_server(server)]})
        self.assertEqual(setup_api._playable_models(), 1)

    def test_a_disabled_model_does_not_count(self):
        server = registry.default_server(kind="owned", provider="lmstudio")
        server["connection"]["base_url"] = "http://localhost:1234/v1"
        model = registry.default_model("some-model")
        model["enabled"] = False
        server["models"] = [model]
        registry.save({**registry.empty_registry(), "servers": [registry.normalize_server(server)]})
        self.assertEqual(setup_api._playable_models(), 0)

    def test_embedding_models_are_filtered_out(self):
        """They appear in every LM Studio and Ollama catalogue and cannot play a turn.

        A seat configured with one fails in a way that reads as a CITAR bug rather than as a wrong
        choice, so the wizard never offers them.
        """
        for key in ("text-embedding-nomic-embed-text-v1.5", "bge-large", "gte-base", "e5-mistral"):
            self.assertTrue(setup_api._is_embedding(key), key)
        for key in ("qwen3-8b", "gemma-3-4b-it", "claude-opus-5"):
            self.assertFalse(setup_api._is_embedding(key), key)


class UnattendedWizard(unittest.TestCase):
    """The path the installers take, and the one nobody exercises by hand.

    `citar setup --local --non-interactive` runs on a machine with nobody watching. The case that
    matters is the one where nothing is installed: it has to finish, say something useful, and exit
    zero, rather than block on a question or fail because there is no model.
    """

    def _run(self, endpoints):
        import tempfile as tf

        from citar.wizard import cli, detect, prompts

        saved_endpoints = detect.KNOWN_ENDPOINTS
        saved_state = os.environ.get("CITAR_STATE_DIR")
        saved_config = os.environ.get("CITAR_CONFIG_DIR")
        saved_save = os.environ.get("CITAR_SAVE_DIR")
        tmp = tf.mkdtemp(prefix="citar-wizard-")
        try:
            detect.KNOWN_ENDPOINTS = endpoints
            # The test package redirects saves and config; point both at a throwaway directory so
            # the wizard cannot write into the shared test registry.
            os.environ["CITAR_STATE_DIR"] = tmp
            os.environ.pop("CITAR_CONFIG_DIR", None)
            os.environ.pop("CITAR_SAVE_DIR", None)
            import citar.servers as registry_module

            registry_module.CONFIG_DIR = Path(tmp) / "config"
            registry_module.PATH = registry_module.CONFIG_DIR / "servers.json"
            return cli.main(["--local", "--non-interactive"])
        finally:
            detect.KNOWN_ENDPOINTS = saved_endpoints
            prompts.NON_INTERACTIVE = False
            for key, value in (("CITAR_STATE_DIR", saved_state), ("CITAR_CONFIG_DIR", saved_config),
                               ("CITAR_SAVE_DIR", saved_save)):
                if value is None:
                    os.environ.pop(key, None)
                else:
                    os.environ[key] = value
            import citar.servers as registry_module

            registry_module.CONFIG_DIR = Path(os.environ["CITAR_CONFIG_DIR"])
            registry_module.PATH = registry_module.CONFIG_DIR / "servers.json"
            shutil.rmtree(tmp, ignore_errors=True)

    def test_finishes_with_no_model_server_anywhere(self):
        # Port 9 is reserved and never listening: this is a machine with nothing installed.
        status = self._run([("openai_compatible", "Nothing", "http://127.0.0.1:9/v1", "https://example.com")])
        self.assertEqual(status, 0, "unattended setup must succeed even with no model available")


class Routes(unittest.TestCase):
    """Every setup route refuses an anonymous caller.

    Local mode signs the operator in automatically, so these run against a server-mode app where
    there is nobody to sign in as.
    """

    def _server_client(self):
        import importlib

        from fastapi.testclient import TestClient

        saved = {k: os.environ.get(k) for k in
                 ("CITAR_MODE", "CITAR_PUBLIC_ORIGIN", "CITAR_SECRET_KEY",
                  "CITAR_REQUIRE_HTTPS", "CITAR_BEHIND_PROXY")}
        os.environ.update({
            "CITAR_MODE": "server",
            "CITAR_PUBLIC_ORIGIN": "https://citar.test",
            "CITAR_SECRET_KEY": "test-secret-key-that-is-long-enough-to-pass",
            "CITAR_REQUIRE_HTTPS": "0",
            "CITAR_BEHIND_PROXY": "0",
        })
        settings.reset()
        from citar.server import app as appmod

        importlib.reload(appmod)
        return TestClient(appmod.app, base_url="https://citar.test"), saved

    def _restore(self, saved):
        for key, value in saved.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        settings.reset()

    def test_anonymous_callers_are_refused(self):
        client, saved = self._server_client()
        try:
            for method, path in [("get", "/api/setup/state"),
                                 ("post", "/api/setup/scan"),
                                 ("post", "/api/setup/dismiss"),
                                 ("get", "/api/setup/console"),
                                 ("post", "/api/setup/policy")]:
                kwargs = {"json": {}} if method == "post" else {}
                response = getattr(client, method)(path, **kwargs)
                self.assertIn(response.status_code, (401, 403, 422),
                              f"{method.upper()} {path} answered {response.status_code}")
        finally:
            self._restore(saved)


class SetupDismissed(unittest.TestCase):
    def test_the_flag_exists_and_defaults_to_false(self):
        """Without a recorded flag, somebody who chose to play against the bots would be offered
        setup on every page load forever — the test it would otherwise use still answers "no
        model"."""
        self.assertIn("setup_dismissed", policy.DEFAULTS)
        self.assertFalse(policy.DEFAULTS["setup_dismissed"])

    def test_the_flag_can_be_set(self):
        policy.set("setup_dismissed", True)
        try:
            self.assertTrue(policy.get("setup_dismissed"))
        finally:
            policy.set("setup_dismissed", False)


if __name__ == "__main__":
    unittest.main()
