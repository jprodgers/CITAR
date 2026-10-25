"""Make the deploy bundle a server is deployed from (design/phase5-release.md P5.13.5): the laptop, never the server,
fetches the release's Linux wheel and checks it, and the bundle carries everything ``citar-deploy`` needs.

    python scripts/deploy/bundle.py V [--arch x86_64|aarch64] [--out DIR] [--ref REF]
    python scripts/deploy/bundle.py V --from-dir DIR [--arch ...] [--out DIR] [--ref REF]

From the published release (the first form): ``gh release download vV`` fetches the wheel
(``citar-P-cp311-abi3-manylinux_2_28_<arch>.whl``), ``citar-P-server-constraints.txt`` and the release's ``SHA256SUMS``;
both files must be listed there with their hashes; ``gh attestation verify`` must accept both (the release's provenance,
P5.12). With ``--from-dir`` the two files come from a folder instead: a dry run's ``pkg-release-assets`` (whose
``SHA256SUMS`` or ``.sha256`` files are checked the same way) or a local build (nothing to check against, and it says
so); no attestation exists for either, and the bundle says that too.

Then ``git archive`` of ``deploy/`` and ``env.example`` at the release's tag (``vV``; with ``--from-dir``, ``HEAD``;
``--ref`` names another), a ``BUNDLE`` file (the version, where the wheel came from, whether its attestation was
verified, the commit ``deploy/`` came from) and a ``SHA256SUMS`` over every other file, packed as
``citar-deploy-V/`` in ``citar-deploy-V.tar.gz`` (the same bytes for the same inputs) with ``citar-deploy-V.tar.gz.sha256``
beside it, under ``--out`` (default ``C:/dev/target`` on Windows, ``~/citar-deploy`` elsewhere; never inside a git
working tree). On the server: unpack it, then ``sudo bash citar-deploy-V/deploy/citar-deploy.sh ... --bundle
citar-deploy-V --plan``.

Exit status 0 when the bundle is written, 1 when something is refused (a hash, an attestation, a missing file: the
message names it), 2 for a usage error. Standard library only.

``CITAR_BUNDLE_GH`` (tests only) names the ``gh`` to run (a ``.py`` file is run with this Python).
"""
from __future__ import annotations

import argparse
import gzip
import hashlib
import importlib.util
import io
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve()
CHECKOUT = HERE.parents[2]
REPO = "jprodgers/CITAR"
ARCHES = ("x86_64", "aarch64")
#: What git archive puts in the bundle beside the wheel: the deploy folder and the settings' example.
ARCHIVED = ("deploy", "env.example")
#: The files citar-deploy needs from deploy/ (deploy/citar-deploy.sh's BUNDLE_NEEDS).
NEEDED = ("deploy/citar-deploy.sh", "deploy/citar.service", "deploy/citar-backup.service", "deploy/citar-backup.timer",
          "deploy/citar-admin.sh", "deploy/citar-backup.sh", "deploy/citar-secrets.sh", "deploy/nginx-citar.conf",
          "env.example")


def _release_module():
    spec = importlib.util.spec_from_file_location("citar_release_for_bundle", CHECKOUT / "scripts" / "release.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


release = _release_module()


class Refused(Exception):
    """Something the bundle will not be made with; the message says what and why."""


def say(text: str) -> None:
    print(text, flush=True)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def names(version: str, arch: str) -> tuple[str, str]:
    """The release's wheel for Linux on ``arch`` and its constraints file (P5.10's names)."""
    p = release.Version.parse(version).pep440
    wheel = f"citar-{p}-cp311-abi3-manylinux_2_28_{arch}.whl"
    constraints = f"citar-{p}-server-constraints.txt"
    assets = {a["name"] for a in release.assets(version)}
    assert wheel in assets and constraints in assets, (wheel, constraints)
    return wheel, constraints


def read_sums(path: Path) -> dict[str, str]:
    """A SHA256SUMS file as {name: hash} (``<hash>  <name>`` or ``<hash> *<name>`` lines)."""
    sums = {}
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not line.strip():
            continue
        digest, _, name = line.partition(" ")
        name = name.lstrip(" *")
        if len(digest) != 64 or not name:
            raise Refused(f"{path.name}:{number} is not '<sha256>  <file>': {line}")
        sums[name] = digest.lower()
    return sums


def check_against(folder: Path, files: tuple[str, ...], sums_name: str = "SHA256SUMS", required: bool = True) -> str:
    """Each of ``files`` in ``folder`` against the folder's SHA256SUMS (or, without one, each file's ``.sha256``).
    Returns what was checked; raises Refused naming a file whose hash differs or that no sum lists."""
    sums_file = folder / sums_name
    if sums_file.is_file():
        sums = read_sums(sums_file)
        for name in files:
            if name not in sums:
                raise Refused(f"{name} is not in {sums_file}")
            got = sha256(folder / name)
            if got != sums[name]:
                raise Refused(f"{name}: its SHA-256 is {got}, and {sums_file} says {sums[name]}")
        return f"each as {sums_name} says"
    found = 0
    for name in files:
        beside = folder / f"{name}.sha256"
        if beside.is_file():
            want = beside.read_text(encoding="utf-8").split()[0].lower()
            got = sha256(folder / name)
            if got != want:
                raise Refused(f"{name}: its SHA-256 is {got}, and {beside.name} says {want}")
            found += 1
    if found == len(files):
        return "each as its .sha256 says"
    if required:
        raise Refused(f"{folder} has no SHA256SUMS to check {', '.join(files)} against")
    if found:
        raise Refused(f"{folder} has a .sha256 for some of {', '.join(files)} and not the others")
    return "nothing to check them against (a local build)"


def gh_command() -> list[str]:
    given = os.environ.get("CITAR_BUNDLE_GH")
    if given:
        return [sys.executable, given] if given.endswith(".py") else [given]
    found = shutil.which("gh")
    if not found:
        raise Refused("gh (GitHub's command line) is not on PATH: it downloads the release and verifies its "
                      "attestations")
    return [found]


def gh(*args: str) -> str:
    done = subprocess.run([*gh_command(), *args], capture_output=True, text=True, timeout=1800)
    if done.returncode != 0:
        raise Refused(f"gh {' '.join(args)} failed (exit {done.returncode}): "
                      f"{(done.stderr or done.stdout).strip()[-800:]}")
    return done.stdout


def git(*args: str, repo: Path, binary: bool = False):
    done = subprocess.run(["git", "-C", str(repo), *args], capture_output=True, timeout=300)
    if done.returncode != 0:
        raise Refused(f"git {' '.join(args)} failed: {done.stderr.decode('utf-8', 'replace').strip()[-600:]}")
    return done.stdout if binary else done.stdout.decode("utf-8").strip()


def in_git_tree(path: Path) -> bool:
    probe = path
    while not probe.exists():
        probe = probe.parent
    done = subprocess.run(["git", "-C", str(probe), "rev-parse", "--is-inside-work-tree"], capture_output=True,
                          text=True, timeout=60)
    return done.returncode == 0 and done.stdout.strip() == "true"


def default_out() -> Path:
    return Path("C:/dev/target") if os.name == "nt" else Path.home() / "citar-deploy"


def fetch_published(version: str, wheel: str, constraints: str, into: Path) -> tuple[str, str]:
    """The release's wheel, constraints and SHA256SUMS downloaded by gh, checked, and their attestations verified."""
    tag = f"v{version}"
    gh("release", "download", tag, "--repo", REPO, "--dir", str(into), "--pattern", wheel, "--pattern", constraints,
       "--pattern", "SHA256SUMS")
    for name in (wheel, constraints, "SHA256SUMS"):
        if not (into / name).is_file():
            raise Refused(f"gh release download {tag} did not write {name}")
    checked = check_against(into, (wheel, constraints))
    for name in (wheel, constraints):
        gh("attestation", "verify", str(into / name), "--repo", REPO)
    return (f"release {tag} of {REPO} (gh release download), {checked} in the release's SHA256SUMS",
            f"verified (gh attestation verify --repo {REPO}) for {wheel} and {constraints}")


def fetch_from_dir(source: Path, wheel: str, constraints: str, into: Path) -> tuple[str, str]:
    for name in (wheel, constraints):
        if not (source / name).is_file():
            raise Refused(f"{source} has no {name}")
    checked = check_against(source, (wheel, constraints), required=False)
    for name in (wheel, constraints):
        shutil.copy2(source / name, into / name)
    return (f"the folder {source}, {checked}",
            "none: a dry run's artifacts and a local build have no attestation (only a tagged release does, P5.12)")


def archive_deploy(ref: str, repo: Path, into: Path) -> str:
    """deploy/ and env.example from ``git archive`` at ``ref`` (the commit ``deploy/`` came from, returned)."""
    commit = git("rev-parse", "--verify", f"{ref}^{{commit}}", repo=repo)
    data = git("archive", "--format=tar", commit, *ARCHIVED, repo=repo, binary=True)
    with tarfile.open(fileobj=io.BytesIO(data)) as tar:
        for member in tar.getmembers():
            if member.name.startswith("/") or ".." in Path(member.name).parts:
                raise Refused(f"git archive gave {member.name}")
        if hasattr(tarfile, "data_filter"):
            tar.extractall(into, filter="data")
        else:                                       # a Python older than 3.11.4: the names were checked above
            tar.extractall(into)
    for name in NEEDED:
        if not (into / name).is_file():
            raise Refused(f"{ref} has no {name}: that commit cannot deploy (citar-deploy came in 0.1.6)")
    return commit


def write_sums(folder: Path) -> None:
    lines = [f"{sha256(p)}  {p.relative_to(folder).as_posix()}" for p in sorted(folder.rglob("*"))
             if p.is_file() and p.relative_to(folder).as_posix() != "SHA256SUMS"]
    (folder / "SHA256SUMS").write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")


def pack(folder: Path, target: Path, mtime: int) -> None:
    """``folder`` as ``target`` (.tar.gz) under its own name: sorted, owned by root, dated ``mtime``, so the same
    inputs give the same bytes."""
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w", format=tarfile.PAX_FORMAT) as tar:
        for path in [folder, *sorted(folder.rglob("*"))]:
            arcname = folder.name if path == folder else f"{folder.name}/{path.relative_to(folder).as_posix()}"
            info = tar.gettarinfo(str(path), arcname)
            info.uid = info.gid = 0
            info.uname = info.gname = "root"
            info.mtime = mtime
            if path.is_dir():
                info.mode = 0o755
                tar.addfile(info)
            else:
                info.mode = 0o755 if path.suffix == ".sh" else 0o644
                with open(path, "rb") as handle:
                    tar.addfile(info, handle)
    with open(target, "wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as packed:
        packed.write(buffer.getvalue())


def make(version: str, arch: str, out: Path, ref: str | None, from_dir: Path | None, repo: Path) -> Path:
    wheel, constraints = names(version, arch)
    p = release.Version.parse(version).pep440
    if in_git_tree(out):
        raise Refused(f"{out} is inside a git working tree: a bundle is never written into a checkout (--out)")
    out.mkdir(parents=True, exist_ok=True)
    name = f"citar-deploy-{version}"
    target = out / f"{name}.tar.gz"
    with tempfile.TemporaryDirectory(prefix="citar-bundle-", dir=out) as work:
        folder = Path(work) / name
        folder.mkdir()
        if from_dir is None:
            downloads = Path(work) / "downloads"
            downloads.mkdir()
            source, attestation = fetch_published(version, wheel, constraints, downloads)
            for file in (wheel, constraints):
                shutil.copy2(downloads / file, folder / file)
            ref = ref or f"v{version}"
        else:
            source, attestation = fetch_from_dir(from_dir, wheel, constraints, folder)
            ref = ref or "HEAD"
        say(f"ok  {wheel} and {constraints}: from {source}")
        say(f"ok  attestation: {attestation}")
        commit = archive_deploy(ref, repo, folder)
        say(f"ok  deploy/ and env.example: git archive {ref} ({commit[:12]})")
        (folder / "BUNDLE").write_text(
            f"version={version}\npep440={p}\nwheel={wheel}\nsource={source}\nattestation={attestation}\n"
            f"deploy=git archive {ref} ({commit})\n", encoding="utf-8", newline="\n")
        write_sums(folder)
        mtime = int(git("log", "-1", "--format=%ct", commit, repo=repo))
        staged = Path(work) / target.name
        pack(folder, staged, mtime)
        os.replace(staged, target)
    digest = sha256(target)
    (out / f"{target.name}.sha256").write_text(f"{digest}  {target.name}\n", encoding="utf-8", newline="\n")
    say(f"wrote {target}")
    say(f"      SHA-256 {digest} ({target.name}.sha256 beside it)")
    say(f"On the server: tar -xzf {target.name}, then sudo bash {name}/deploy/citar-deploy.sh "
        f"(cutover|install) --bundle {name} --plan")
    return target


def parser() -> argparse.ArgumentParser:
    ap = argparse.ArgumentParser(prog="bundle.py", description=__doc__.split("\n\n", 1)[0])
    ap.add_argument("version", help="the release, as its tag names it without the v (0.1.6, 0.1.6-rc.1)")
    ap.add_argument("--arch", choices=ARCHES, default="x86_64", help="the server's architecture (default x86_64)")
    ap.add_argument("--from-dir", type=Path, help="take the wheel and constraints from this folder (a dry run's "
                                                  "pkg-release-assets, or a local build) instead of the release")
    ap.add_argument("--out", type=Path, help="where the bundle goes (default C:/dev/target on Windows, "
                                             "~/citar-deploy elsewhere)")
    ap.add_argument("--ref", help="the commit deploy/ and env.example come from (default the tag vV; with "
                                  "--from-dir, HEAD)")
    ap.add_argument("--repo-dir", type=Path, default=CHECKOUT, help=argparse.SUPPRESS)
    return ap


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        release.Version.parse(args.version)
    except release.Refused as exc:
        print(f"bundle.py: {exc}", file=sys.stderr)
        return 2
    try:
        make(args.version, args.arch, (args.out or default_out()).resolve(), args.ref,
             args.from_dir.resolve() if args.from_dir else None, args.repo_dir)
    except Refused as exc:
        print(f"bundle.py: refused: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
