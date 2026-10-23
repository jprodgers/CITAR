"""The helper manifest's real signature, on a tag (design/phase5-release.md P5.4.2 and P5.11, package 5-09).

    python3 scripts/ci/release_sign.py sign --xtask FILE --manifest FILE --keys FILE --key-env NAME --out DIR
        [--github-output FILE]
    python3 scripts/ci/release_sign.py expect --keys FILE --signature DIR --release DIR [--github-output FILE]

release.yml's ``sign-helper-manifest`` job runs ``sign`` on a tag, in the environment ``helper-signing`` (its
reviewer approves the run), in its one step whose only ``env:`` is the secret ``CITAR_UPDATE_SIGNING_KEY``, on the
runner's own Python. So this script is the standard library alone, and it builds and installs nothing: it runs the
xtask binary package.yml's ``helper-manifest`` job built from the locked sources and used to write and prove the
manifest (the artifact ``pkg-xtask-linux-x64``), never cargo, offline. The key never meets a build or an installer.

``sign``:

1. **The binary is the one package.yml built:** its SHA-256 must be the hex digest in ``FILE.sha256`` beside it (the
   artifact carries both); it is then made executable (an artifact keeps no file mode).
2. **What signs is decided by the committed ``update-keys.pub`` and the secret** (P5.4.2's table). The key file's
   lines are read as citar-jobs reads them (blank lines and ``#`` lines past), each one a public key's form (standard
   base64 of 32 bytes; xtask then holds it to an ed25519 key), at most two distinct ones; the secret is there when the
   variable ``--key-env`` names holds anything but blanks:

   =================  ==========  ========================================================================
   update-keys.pub    the secret  outcome
   =================  ==========  ========================================================================
   empty              absent      a notice; no signature; the release's helpers report updates unavailable
   empty              present     refused: commit the public key first (no helper would trust the signature)
   a key              absent      refused: helpers built from this tree would trust a key no release signs with
   a key              present     ``xtask sign-manifest --key-env``, then ``xtask verify-manifest --pub`` against
                                  the committed lines: refused unless it verifies ("the secret is not the
                                  committed key")
   =================  ==========  ========================================================================

3. ``--out DIR`` receives ``signing.json`` (whether it signed, how many keys the tree trusts, and the line that
   verified) and, when signed, ``citar-helper-manifest.json.sig``. release.yml uploads the folder as
   ``rel-helper-signature`` either way, so the jobs after it can tell "nothing was signed" from "nothing arrived".

The secret is never printed, and never written anywhere: xtask reads it from the environment (``--key-env``).

``expect`` (release.yml's ``check-signature``, which holds no secret): the signature in ``--signature`` (the
``rel-helper-signature`` download) must be there exactly when the committed ``update-keys.pub`` holds a key, whatever
the signing job said; when it is, it is copied into the release folder beside the manifest it signs. The independent
verifier (``scripts/ci/verify_manifest.py``) then checks it against each committed line.

With ``--github-output FILE`` each command appends ``signed=true`` or ``signed=false``. Exit status 0 when the rules
hold (signed, or the notice), 1 when they refuse (each refusal names its cause), 2 for a usage error or a run that
cannot start (no binary, no manifest).
"""
from __future__ import annotations

import argparse
import base64
import binascii
import hashlib
import json
import os
import shutil
import stat
import subprocess
import sys
from pathlib import Path

MANIFEST = "citar-helper-manifest.json"
SIGNATURE = MANIFEST + ".sig"
REPORT = "signing.json"
#: The most keys update-keys.pub may hold (``citar_jobs::update::MAX_KEYS``; verify_manifest.py's copy).
MAX_KEYS = 2
#: How long one xtask command may take: it signs or verifies one small file.
XTASK_TIMEOUT = 60


class Usage(Exception):
    """The command cannot run: exit 2."""


class Refused(Exception):
    """The rules refuse: exit 1."""


def actions() -> bool:
    return os.environ.get("GITHUB_ACTIONS") == "true"


def say(text: str) -> None:
    print(f"release_sign: {text}", flush=True)


def notice(title: str, text: str) -> None:
    say(text)
    if actions():
        print(f"::notice title={title}::{text}", flush=True)


def key_lines(path: Path) -> list[tuple[int, bytes]]:
    """``update-keys.pub``'s keys, each with its line, as citar-jobs reads the file: blank lines and ``#`` lines past,
    each other line a public key's form (standard base64 of 32 bytes). A line's text is never echoed, so a private
    key's file given by mistake is not printed."""
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as e:
        raise Usage(f"the key file {path}: {e}") from None
    keys = []
    for number, line in enumerate(text.splitlines(), 1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        try:
            raw = base64.b64decode(line, validate=True)
        except (binascii.Error, ValueError):
            raw = b""
        if len(raw) != 32:
            raise Refused(f"{path} line {number} is not a public key (standard base64 of an ed25519 key's 32 bytes)")
        keys.append((number, raw))
    distinct = len({raw for _, raw in keys})
    if distinct > MAX_KEYS:
        raise Refused(f"{path} holds {distinct} keys, over the {MAX_KEYS} a helper reads")
    return keys


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def proven_xtask(path: Path) -> Path:
    """The xtask binary, once its SHA-256 is the one package.yml wrote beside it; made executable."""
    if not path.is_file():
        raise Usage(f"no xtask binary at {path} (the pkg-xtask-linux-x64 artifact)")
    side = path.with_name(path.name + ".sha256")
    try:
        want = side.read_text(encoding="ascii").split()[0].lower()
    except (OSError, UnicodeDecodeError, IndexError) as e:
        raise Refused(f"{side}: no SHA-256 beside the xtask binary ({e or 'empty'})") from None
    got = sha256(path)
    if got != want:
        raise Refused(f"{path}'s SHA-256 is {got}, and {side.name} says {want}: it is not the binary package.yml "
                      f"built and proved")
    mode = path.stat().st_mode
    path.chmod(mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    say(f"the xtask binary is the one package.yml built (sha256 {got})")
    return path.resolve()


def xtask(binary: Path, *args: str) -> subprocess.CompletedProcess:
    """One xtask command, offline, with this process's environment (the secret's variable among it for
    ``--key-env``). Its output is shown: xtask prints public keys and file names, never a secret."""
    try:
        done = subprocess.run([str(binary), *args], stdin=subprocess.DEVNULL, capture_output=True, text=True,
                              encoding="utf-8", errors="replace", timeout=XTASK_TIMEOUT)
    except subprocess.TimeoutExpired:
        raise Refused(f"xtask {args[0]} did not finish within {XTASK_TIMEOUT} s") from None
    except OSError as e:
        raise Usage(f"xtask {args[0]} could not start: {e}") from None
    for line in (done.stdout + done.stderr).splitlines():
        print(f"  xtask {args[0]}: {line}", flush=True)
    return done


def write_report(out: Path, report: dict) -> None:
    (out / REPORT).write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8", newline="\n")


def sign(xtask_path: Path, manifest: Path, keys_file: Path, key_env: str, out: Path) -> bool:
    """P5.4.2's four states; True when the manifest was signed (the signature in ``out``), False for the notice."""
    if not key_env or not key_env.replace("_", "").isalnum():
        raise Usage(f"--key-env {key_env!r}: the name of the variable that holds the key")
    if not manifest.is_file():
        raise Usage(f"no manifest at {manifest} (the pkg-helper-manifest artifact)")
    keys = key_lines(keys_file)
    secret = bool(os.environ.get(key_env, "").strip())
    out.mkdir(parents=True, exist_ok=True)
    for stale in (out / SIGNATURE, out / REPORT):
        stale.unlink(missing_ok=True)
    report = {"signed": False, "keys": len({raw for _, raw in keys}), "secret": secret, "verified_by_line": None}
    if not keys and not secret:
        write_report(out, report)
        notice("helper signing", f"{keys_file} holds no public key and {key_env} is not set: the manifest is not "
                                 f"signed, and this release's helpers report updates unavailable (RELEASING.md step 1 "
                                 f"says how to set signing up)")
        return False
    if not keys:
        raise Refused(f"the secret {key_env} is set but {keys_file} holds no public key: commit the public key first "
                      f"(RELEASING.md step 1), or no helper would trust the signature")
    if not secret:
        raise Refused(f"{keys_file} holds {report['keys']} public key(s) but the secret {key_env} is not set: helpers "
                      f"built from this tree would trust a key no release signs with (the environment helper-signing "
                      f"holds the secret, RELEASING.md step 1)")
    binary = proven_xtask(xtask_path)
    sig = out / SIGNATURE
    done = xtask(binary, "sign-manifest", "--key-env", key_env, "--out", str(sig), str(manifest))
    if done.returncode != 0:
        sig.unlink(missing_ok=True)
        raise Refused(f"xtask sign-manifest exited {done.returncode}: the secret {key_env} is not an update-signing "
                      f"key file (cargo xtask keygen's citar-update-signing.key, whole)")
    done = xtask(binary, "verify-manifest", "--pub", str(keys_file), str(manifest), str(sig))
    if done.returncode == 1:
        sig.unlink(missing_ok=True)
        raise Refused(f"the signature does not verify against any key in {keys_file}: the secret {key_env} is not "
                      f"the committed key")
    if done.returncode != 0:
        sig.unlink(missing_ok=True)
        raise Refused(f"xtask verify-manifest refused {keys_file} (exit {done.returncode}): its lines must be public "
                      f"keys")
    line = next((n for n, _ in keys if f"on line {n} of" in done.stdout), None)
    report.update(signed=True, verified_by_line=line)
    write_report(out, report)
    say(f"signed {manifest.name}: the signature verifies against line {line} of {keys_file}")
    return True


def expect(keys_file: Path, signature: Path, release: Path) -> bool:
    """Whether the release is signed, held to the committed key file; the signature copied into ``release``."""
    keys = key_lines(keys_file)
    given = signature / SIGNATURE
    has = given.is_file()
    target = release / SIGNATURE
    if not (release / MANIFEST).is_file():
        raise Usage(f"{release} holds no {MANIFEST}")
    if not keys:
        if has:
            raise Refused(f"{given} exists, but {keys_file} holds no public key: no helper would trust it")
        if target.exists():
            raise Refused(f"{target} exists, but {keys_file} holds no public key")
        say(f"unsigned: {keys_file} holds no public key, and no signature came")
        return False
    if not has:
        raise Refused(f"{keys_file} holds {len({raw for _, raw in keys})} public key(s), but sign-helper-manifest "
                      f"left no {SIGNATURE} in {signature}")
    if target.exists() and target.read_bytes() != given.read_bytes():
        raise Refused(f"{target} is not the signature sign-helper-manifest made")
    shutil.copyfile(given, target)
    say(f"signed: {SIGNATURE} copied into {release}, for the verifiers and the release")
    return True


def github_output(path: Path | None, signed: bool) -> None:
    if path is not None:
        with open(path, "a", encoding="utf-8", newline="\n") as f:
            f.write(f"signed={'true' if signed else 'false'}\n")


def main(argv: list[str] | None = None) -> int:
    top = argparse.ArgumentParser(prog="release_sign.py", description=__doc__.split("\n\n")[0], allow_abbrev=False)
    sub = top.add_subparsers(dest="command", required=True)
    p = sub.add_parser("sign", allow_abbrev=False, help="sign the manifest as update-keys.pub and the secret say")
    p.add_argument("--xtask", type=Path, required=True, help="package.yml's xtask binary (FILE.sha256 beside it)")
    p.add_argument("--manifest", type=Path, required=True)
    p.add_argument("--keys", type=Path, required=True, help="the committed update-keys.pub")
    p.add_argument("--key-env", required=True, help="the variable holding the secret key file's text")
    p.add_argument("--out", type=Path, required=True, help="where signing.json and the signature go")
    p.add_argument("--github-output", type=Path)
    p = sub.add_parser("expect", allow_abbrev=False, help="the signature is there exactly when a key is committed")
    p.add_argument("--keys", type=Path, required=True, help="the committed update-keys.pub")
    p.add_argument("--signature", type=Path, required=True, help="the rel-helper-signature download")
    p.add_argument("--release", type=Path, required=True, help="the release folder (the manifest is in it)")
    p.add_argument("--github-output", type=Path)
    args = top.parse_args(argv)
    try:
        if args.command == "sign":
            signed = sign(args.xtask, args.manifest, args.keys, args.key_env, args.out)
        else:
            signed = expect(args.keys, args.signature, args.release)
        github_output(args.github_output, signed)
    except Usage as e:
        print(f"release_sign: {e}", file=sys.stderr)
        return 2
    except Refused as e:
        print(f"release_sign: REFUSED: {e}", file=sys.stderr)
        if actions():
            print(f"::error title=helper signing::{e}", flush=True)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
