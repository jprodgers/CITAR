"""scripts/server_soak.py's guards (package 2-13): the soak empties its state directory and starts a server on its port,
so it must never take a directory someone plays from, or a port a server listens on (the dev server's own above all).
The soak itself is a long run of its own (CONTRIBUTING, "Long runs"), not a test."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import importlib.util
import shutil
import socket
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def load():
    spec = importlib.util.spec_from_file_location("server_soak", ROOT / "scripts" / "server_soak.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


try:
    soak, MISSING = load(), None
except ModuleNotFoundError as e:        # the script's own needs: the mcp and worker extras, which the dev extra has
    soak, MISSING = None, e.name
needs_extras = unittest.skipIf(soak is None, f"scripts/server_soak.py needs the mcp and worker extras ({MISSING})")


@needs_extras
class StateDirectory(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="citar-soak-guard-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)

    def test_a_directory_no_soak_left_is_refused_and_left_as_it_was(self):
        played = self.tmp / "citar-play"
        (played / "saves" / "g1").mkdir(parents=True)
        (played / "citar.db").write_text("the owner's games")
        with self.assertRaises(SystemExit) as refused:
            soak.claim_state(played)
        self.assertIn("refusing to empty it", str(refused.exception))
        self.assertEqual((played / "citar.db").read_text(), "the owner's games")
        self.assertTrue((played / "saves" / "g1").is_dir())
        self.assertFalse((played / soak.MARKER).exists())

    def test_a_new_or_empty_directory_is_taken_and_marked(self):
        for state in (self.tmp / "new" / "state", self.tmp / "empty"):
            if state.name == "empty":
                state.mkdir()
            soak.claim_state(state)
            self.assertEqual([p.name for p in state.iterdir()], [soak.MARKER])

    def test_an_earlier_soaks_directory_is_emptied_again(self):
        state = self.tmp / "soak"
        soak.claim_state(state)
        (state / "saves").mkdir()
        (state / "citar.db").write_text("the last run's")
        soak.claim_state(state)
        self.assertEqual([p.name for p in state.iterdir()], [soak.MARKER])

    def test_a_file_is_refused(self):
        f = self.tmp / "a-file"
        f.write_text("not a directory")
        with self.assertRaises(SystemExit):
            soak.claim_state(f)
        self.assertEqual(f.read_text(), "not a directory")


@needs_extras
class Port(unittest.TestCase):
    def test_the_dev_servers_port_is_refused(self):
        with self.assertRaises(SystemExit) as refused:
            soak.check_port(soak.DEV_SERVER_PORT)
        self.assertIn("dev server", str(refused.exception))

    def test_a_port_something_listens_on_is_refused_and_a_free_one_taken(self):
        with socket.socket() as listening:
            listening.bind(("127.0.0.1", 0))
            listening.listen(1)
            port = listening.getsockname()[1]
            with self.assertRaises(SystemExit) as refused:
                soak.check_port(port)
            self.assertIn("already listens", str(refused.exception))
        soak.check_port(soak.free_port())


if __name__ == "__main__":
    unittest.main()
