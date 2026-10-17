"""No private key in the repository (design/phase4-helper.md P4.7.1 rule 3; package 4-13's gate 6).

The helper's update channel signs with a key that lives in a GitHub Actions secret (the owner's answer of 2026-10-14),
and every test makes its throwaway keys at run time; ``cargo xtask keygen`` and ``sign-manifest`` refuse a path inside
the repository. This holds the tree to it: no file git tracks, and none it would add (``git ls-files --cached
--others --exclude-standard``), is named ``*.key``, holds a PEM private key, or holds an ed25519 secret as the
helper's machine key and ``keygen``'s update-signing key write it (a ``"secret"`` of 32 bytes in base64). The
scanner's own patterns are checked against samples made at run time, so this file never holds one.
"""
from __future__ import annotations

import base64
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
#: The largest file read (the tree's largest text files are a few MB; bigger ones are data, read past).
MAX_BYTES = 8 << 20

#: A PEM private key's armour (BEGIN, any words such as RSA or ENCRYPTED, then PRIVATE KEY, between five dashes).
PEM = re.compile("-----BEGIN " + r"(?:[A-Z0-9]+ )*" + "PRIVATE KEY-----")
#: An ed25519 secret in JSON, as ``helper/machine-key.json`` and ``citar-update-signing.key`` hold it.
SECRET = re.compile(r'"secret"\s*:\s*"[A-Za-z0-9+/]{43}="')


def findings(path: Path, rel: str) -> list:
    """What makes a file a private key's, by its name or its text."""
    out = []
    if rel.lower().endswith(".key"):
        out.append(f"{rel}: named *.key")
    try:
        if path.stat().st_size > MAX_BYTES:
            return out
        raw = path.read_bytes()
    except OSError:
        return out
    if b"\0" in raw[:8192]:
        return out                    # a binary file
    text = raw.decode("utf-8", errors="ignore")
    if PEM.search(text):
        out.append(f"{rel}: a PEM private key")
    if SECRET.search(text):
        out.append(f"{rel}: an ed25519 secret in JSON")
    return out


def listed_files() -> list:
    """Every file git tracks, and every one it would add (not ignored), relative to the root."""
    out = subprocess.run(["git", "-C", str(ROOT), "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
                         capture_output=True, timeout=120, check=True).stdout
    return sorted({p for p in out.decode("utf-8", errors="replace").split("\0") if p})


@unittest.skipUnless(shutil.which("git") and (ROOT / ".git").exists(), "needs git and the checkout")
class NoPrivateKey(unittest.TestCase):
    def test_the_scanner_finds_each_kind(self):
        secret = base64.b64encode(os.urandom(32)).decode()
        samples = {
            "a.key": "anything",
            "pem.txt": "-----BEGIN " + "PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEI\n-----END " + "PRIVATE KEY-----\n",
            "rsa.txt": "-----BEGIN RSA " + "PRIVATE KEY-----\n",
            "machine-key.json": '{"v": 1, "public": "x", "' + 'secret": "' + secret + '"}',
            "keygen.json": '{"v":1,"kind":"citar-helper-update-signing-key","public":"x","' + 'secret":"' + secret + '"}',
        }
        folder = Path(tempfile.mkdtemp(prefix="citar-keyscan-"))
        try:
            for name, text in samples.items():
                (folder / name).write_text(text, encoding="utf-8")
                self.assertTrue(findings(folder / name, name), name)
            (folder / "public.txt").write_text("-----BEGIN PUBLIC KEY-----\n" + secret, encoding="utf-8")
            self.assertEqual(findings(folder / "public.txt", "public.txt"), [], "a public key is no secret")
        finally:
            shutil.rmtree(folder, ignore_errors=True)

    def test_no_file_in_the_tree_holds_a_private_key(self):
        found = []
        files = listed_files()
        self.assertGreater(len(files), 500, "git listed the tree")
        for rel in files:
            path = ROOT / rel
            if path.is_file():
                found.extend(findings(path, rel))
        self.assertEqual(found, [], "no private key belongs in the repository (design/phase4-helper.md P4.7.1)")

    def test_the_committed_update_keys_are_none(self):
        """Phase 4 ships no update key: crates/citar-jobs/update-keys.pub is empty (the owner's Phase 5 adds the
        public half of the key whose private half lives in the helper-signing environment's secret)."""
        text = (ROOT / "crates" / "citar-jobs" / "update-keys.pub").read_text(encoding="utf-8")
        self.assertEqual(text.strip(), "")


if __name__ == "__main__":
    unittest.main()
