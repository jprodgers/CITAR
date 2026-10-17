"""One protocol in the tree (design/phase4-helper.md P4.11, the owner's decision of 2026-10-14; package 4-05's gate 9).

The hub carries exactly one protocol: protocol 2. So:

- no ``citar.worker`` (the Python worker's package), ``citar-worker`` (its command and its 0.1.5 system unit) or
  ``PROTOCOL_VERSION = 1`` is left in any file git tracks, outside the archive notes: the design's history, the
  changelog, and the few places that must name what a 0.1.5 machine runs (each allowed line listed below, with why);
- ``citar/server`` handles no protocol 1 frame beyond the refusal: the hub's frame dispatch names only protocol 2's
  helper frame types, nothing there imports protocol 1's module or names its own frames and fields (``chunk``,
  ``max_concurrent`` in a hello, ``worker_version``, ``duration``, ``stop_reason``), and the one place a protocol 1
  hello is answered is the v1 refusal.
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)

import ast
import re
import subprocess
import unittest
from pathlib import Path

from citar.server import helper_protocol as HP

ROOT = Path(__file__).resolve().parents[1]
RETIRED = re.compile(r"citar\.worker(?![\w])|citar-worker|PROTOCOL_VERSION = 1")

#: Whole folders of history: the design (each package's as-built notes name what it replaced).
ARCHIVE = ("design/",)
#: Files where only these lines may name the retired worker, and why.
ALLOWED = {
    "CHANGELOG.md": ("the 0.1.6 entry tells a 0.1.5 owner how to disable the old unit, and older entries are history",
                     None),
    "crates/citar-helper/PROTOCOL.md": ("the v1 refusal's section names the 0.1.5 system unit an owner disables",
                                        ("systemctl disable --now citar-worker",)),
    "crates/citar-helper/protocol/v2/frames/valid/server.error.v015.json": (
        "a 0.1.5 server's own words, recorded (the helper reads them as refused protocol)",
        ("Update citar-worker.",)),
    "crates/citar-helper/protocol/v2/transcripts/handshake-v015-server.json": (
        "a 0.1.5 server's own words, recorded", ("Update citar-worker.",)),
    "docs/server/WORKERS.md": ("the runbook for a machine still running the 0.1.5 system unit",
                               ("systemctl disable --now citar-worker",)),
    "wiki/Workers.md": ("generated from docs/server/WORKERS.md", ("systemctl disable --now citar-worker",)),
    "wiki/Changelog.md": ("generated from CHANGELOG.md", None),
    "install.sh": ("--uninstall removes the link a 0.1.5 install made", ("for link in citar citar-admin citar-worker",)),
    "tests/test_one_protocol.py": ("this test", None),
}


def tracked() -> list:
    out = subprocess.run(["git", "ls-files", "-z"], cwd=ROOT, capture_output=True, check=True).stdout
    return [p for p in out.decode("utf-8").split("\0") if p]


@unittest.skipUnless((ROOT / ".git").exists(), "not a git checkout (an extracted source distribution)")
class NoProtocolOne(unittest.TestCase):

    def test_the_retired_worker_is_named_only_in_the_archive_notes(self):
        found = []
        for path in tracked():
            if path.startswith(ARCHIVE):
                continue
            file = ROOT / path
            try:
                text = file.read_text(encoding="utf-8")
            except (UnicodeDecodeError, OSError):
                continue
            allowed = ALLOWED.get(path)
            for n, line in enumerate(text.splitlines(), 1):
                if not RETIRED.search(line):
                    continue
                if allowed is not None and (allowed[1] is None or any(a in line for a in allowed[1])):
                    continue
                found.append(f"{path}:{n}: {line.strip()[:120]}")
        self.assertEqual(found, [])

    def test_the_allowances_are_still_needed(self):
        """An allowance that no longer matches anything is removed, so the list cannot grow stale."""
        for path, (_why, lines) in ALLOWED.items():
            if lines is None:
                continue
            text = (ROOT / path).read_text(encoding="utf-8")
            for line in lines:
                self.assertIn(line, text, f"{path}: the allowance {line!r} matches nothing")


class TheHubSpeaksProtocolTwo(unittest.TestCase):

    def source(self, name: str) -> str:
        return (ROOT / "citar" / "server" / name).read_text(encoding="utf-8")

    def test_the_frame_dispatch_names_only_protocol_two_frames(self):
        tree = ast.parse(self.source("workers.py"))
        frame = next(n for n in ast.walk(tree) if isinstance(n, ast.AsyncFunctionDef) and n.name == "_frame")
        handled = set()
        for node in ast.walk(frame):
            if isinstance(node, ast.Compare) and isinstance(node.left, ast.Name) and node.left.id == "t":
                for c in node.comparators:
                    if isinstance(c, ast.Constant) and isinstance(c.value, str):
                        handled.add(c.value)
        self.assertGreater(len(handled), 8, "the dispatch was found")
        self.assertEqual(sorted(handled - set(HP.HELPER_FRAMES)), [], "a frame type protocol 2's helper never sends")

    def test_nothing_in_the_server_reads_protocol_one(self):
        # protocol 1's module and its own frame and fields (a hello's max_concurrent and worker_version, a Done's
        # duration, stop_reason and malformed); app.py's game metrics have a "malformed" of their own
        modules = r"""worker\.protocol|from \.\.worker\b|PROTOCOL_VERSION|["']chunk["']|["']worker_version["']"""
        fields = re.compile(modules + r"""|["'](max_concurrent|stop_reason|malformed|duration)["']""")
        for name, words in (("workers.py", fields), ("helper_protocol.py", fields), ("app.py", re.compile(modules))):
            with self.subTest(name):
                hits = [line.strip() for line in self.source(name).splitlines() if words.search(line)]
                self.assertEqual(hits, [], name)
        provider = (ROOT / "citar" / "agents" / "providers" / "worker_provider.py").read_text(encoding="utf-8")
        self.assertNotIn("tool_mode=", provider, "the request carries no tool mode: the server parses the reply")
        self.assertIn("parse_completion", provider)

    def test_a_protocol_one_hello_meets_only_the_refusal(self):
        """The refusal is the handshake's first check and a single branch: ``serve`` sends the v1 refusal only where
        the hello's parse failed with code ``protocol``."""
        src = self.source("workers.py")
        self.assertEqual(src.count("HP.v1_refusal()"), 1)
        i = src.index("HP.v1_refusal()")
        self.assertIn('if exc.code == "protocol":', src[max(0, i - 600):i])
        self.assertEqual(HP.HUB_PROTOCOLS, (2,))


if __name__ == "__main__":
    unittest.main()
