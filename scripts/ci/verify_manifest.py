"""The helper manifest's signature, checked by code that shares nothing with the signer (design/phase5-release.md
P5.4.2, package 5-03).

    python scripts/ci/verify_manifest.py --pub FILE MANIFEST [SIG]
    python scripts/ci/verify_manifest.py --check-keys FILE

``cargo xtask sign-manifest`` signs ``citar-helper-manifest.json`` with ed25519 (``ed25519-dalek``) over the domain
``CITAR-HELPER-MANIFEST-V1`` and a zero byte followed by the manifest's bytes exactly as published, and writes the
64-byte signature in standard base64 to ``MANIFEST.sig``; ``cargo xtask verify-manifest`` and every helper check it
with the same library. This checks the same format with the ``cryptography`` package's Ed25519 (OpenSSL), so a
mistake the signer and its verifier share (the domain, the bytes signed, the encoding) is caught here.

``--pub FILE`` is a key file in ``crates/citar-jobs/update-keys.pub``'s format: one standard-base64 ed25519 public key
a line, blank lines and lines starting with ``#`` read past. The signature passes when one of its keys made it; then
the signed file must be a manifest a helper reads (at most 65,536 bytes, JSON, ``v`` 1, some assets). The key's line
is named. ``--check-keys FILE`` only reads the key file: each line a well-formed public key, at most two (the
current key and its successor), and prints how many it holds (none is not an error: the tree holds none until the
owner commits the public key).

A line of a key file is never echoed, so a private key's file passed by mistake is not printed. Exit status: 0
verified (or the keys read), 1 the signature does not verify, the signed file is not a manifest, or a key line is
malformed, 2 the command cannot run (its arguments, an unreadable key file, no ``cryptography``).
"""
from __future__ import annotations

import argparse
import base64
import binascii
import json
import sys
from pathlib import Path

#: What is signed before the manifest's bytes (``citar_jobs::update::MANIFEST_DOMAIN``, xtask's copy).
MANIFEST_DOMAIN = b"CITAR-HELPER-MANIFEST-V1\0"
#: The largest manifest a helper reads (``citar_jobs::update::MANIFEST_MAX_BYTES``).
MANIFEST_MAX_BYTES = 65_536
#: The most keys ``update-keys.pub`` may hold (``citar_jobs::update::MAX_KEYS``).
MAX_KEYS = 2


class Unusable(Exception):
    """The command cannot run: exit 2."""


class Failed(Exception):
    """The check ran and failed: exit 1."""


def b64(text: str) -> bytes:
    """Standard base64, strictly: no other alphabet, no stray characters."""
    return base64.b64decode(text.strip(), validate=True)


def public_keys(text: str) -> list[tuple[int, bytes]]:
    """Each public key of a key file with its line number. A malformed line fails by its number, its text unsaid."""
    keys = []
    for number, line in enumerate(text.splitlines(), 1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        try:
            raw = b64(line)
        except (binascii.Error, ValueError):
            raw = b""
        if len(raw) != 32:
            raise Failed(f"line {number} is not a public key (standard base64 of an ed25519 key's 32 bytes)")
        keys.append((number, raw))
    return keys


def ed25519():
    try:
        from cryptography.exceptions import InvalidSignature
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
    except ImportError as e:
        raise Unusable(f"this needs the cryptography package ({e}); pip install 'cryptography>=42.0'") from e
    return Ed25519PublicKey, InvalidSignature


def verified_by(manifest: bytes, signature: str, keys: list[tuple[int, bytes]]) -> int:
    """The line of the first key whose signature ``signature`` is over the domain and ``manifest``."""
    public_key, invalid = ed25519()
    try:
        sig = b64(signature)
    except (binascii.Error, ValueError) as e:
        raise Failed(f"the signature is not standard base64: {e}") from e
    if len(sig) != 64:
        raise Failed(f"the signature is {len(sig)} bytes, not 64")
    message = MANIFEST_DOMAIN + manifest
    for number, raw in keys:
        try:
            key = public_key.from_public_bytes(raw)
        except ValueError as e:
            raise Failed(f"line {number} is not an ed25519 public key ({e})") from e
        try:
            key.verify(sig, message)
        except invalid:
            continue
        return number
    raise Failed("the signature does not verify against any key given (a bad signature, a changed manifest, or "
                 "another key)")


def manifest_problem(data: bytes) -> str | None:
    """Why a signed file is not a manifest a helper reads, if it is not."""
    if len(data) > MANIFEST_MAX_BYTES:
        return f"it is {len(data)} bytes, over the {MANIFEST_MAX_BYTES} a helper reads"
    try:
        doc = json.loads(data.decode("utf-8"))
    except (UnicodeDecodeError, ValueError) as e:
        return f"it is not JSON: {e}"
    if not isinstance(doc, dict) or type(doc.get("v")) is not int or doc["v"] != 1:  # not true, not 1.0
        return "its v is not 1"
    assets = doc.get("assets")
    if not isinstance(assets, dict) or not assets:
        return "it lists no assets"
    return None


def read_text(path: Path, what: str) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as e:
        raise Unusable(f"{what} {path}: {e}") from e


def verify(pub: Path, manifest: Path, sig: Path) -> str:
    """The line saying which key verified ``manifest``; raises Failed or Unusable otherwise."""
    try:
        keys = public_keys(read_text(pub, "the key file"))
    except Failed as e:
        raise Unusable(f"{pub}: {e}") from e
    if not keys:
        raise Unusable(f"{pub} holds no public key")
    try:
        data = manifest.read_bytes()
        signature = sig.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as e:
        raise Failed(str(e)) from e
    try:
        line = verified_by(data, signature, keys)
    except Failed as e:
        raise Failed(f"{manifest} ({sig}): {e}") from e
    problem = manifest_problem(data)
    if problem:
        raise Failed(f"{manifest} is signed, but no helper reads it as a manifest: {problem}")
    key = base64.b64encode(dict(keys)[line]).decode("ascii")
    return f"verified: {manifest} is signed by the key on line {line} of {pub} ({key})"


def check_keys(path: Path) -> str:
    keys = public_keys(read_text(path, "the key file"))
    ed25519_key, _invalid = ed25519()
    for number, raw in keys:
        try:
            ed25519_key.from_public_bytes(raw)
        except ValueError as e:
            raise Failed(f"line {number} is not an ed25519 public key ({e})") from e
    if len(keys) > MAX_KEYS:
        raise Failed(f"{len(keys)} keys, over the {MAX_KEYS} a key file may hold")
    if not keys:
        return f"{path}: no public key (a build from it trusts none, so its helpers report updates unavailable)"
    return f"{path}: {len(keys)} public key(s), each well formed (lines {', '.join(str(n) for n, _ in keys)})"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="verify_manifest.py", description=__doc__.split("\n\n", 1)[0])
    ap.add_argument("--pub", type=Path, help="the public keys to trust (update-keys.pub's format)")
    ap.add_argument("--check-keys", type=Path, metavar="FILE", help="only read a key file: each line a public key")
    ap.add_argument("files", nargs="*", type=Path, metavar="MANIFEST [SIG]")
    args = ap.parse_args(argv)
    try:
        if args.check_keys is not None:
            if args.pub is not None or args.files:
                raise Unusable("--check-keys takes the key file alone")
            print(check_keys(args.check_keys))
            return 0
        if args.pub is None or not 1 <= len(args.files) <= 2:
            raise Unusable("usage: verify_manifest.py --pub FILE MANIFEST [SIG]")
        manifest = args.files[0]
        sig = args.files[1] if len(args.files) == 2 else manifest.with_name(manifest.name + ".sig")
        print(verify(args.pub, manifest, sig))
        return 0
    except Unusable as e:
        print(f"verify_manifest: {e}", file=sys.stderr)
        return 2
    except Failed as e:
        print(f"verify_manifest: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
