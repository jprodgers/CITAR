"""The package managers' manifests, written from a release's files, and a published release checked against its sums
(design/phase5-release.md P5.9 and P5.10, package 5-08).

    python scripts/release_checksums.py V --from-dir DIR [--base-url URL] [--date YYYY-MM-DD] (--out DIR | --write)
    python scripts/release_checksums.py V [--base-url URL] [--date YYYY-MM-DD] (--out DIR | --write)
    python scripts/release_checksums.py V --from-dir DIR --write-pypi-json FILE
    python scripts/release_checksums.py --verify V [--base-url URL] [--pypi-json FILE]

Homebrew, Scoop and winget each pin the SHA-256 of a release file, so their manifests are written from the release's
files, never by hand. Each manifest is a template under ``packaging/`` (``packaging/**/*.in``; the manifest is the
template's path without ``.in``) in this vocabulary:

- ``{{version}}``: the release's version, V; ``{{date}}``: its date (``--date``, else the date of V's section in
  ``CHANGELOG.md``);
- ``{{url:NAME}}``: NAME's download URL, the base URL (``--base-url``; by default the release's own download folder,
  ``https://github.com/jprodgers/CITAR/releases/download/vV/``) and NAME;
- ``{{sha256:NAME}}`` and ``{{SHA256:NAME}}``: NAME's SHA-256 in lower or upper case hex (winget writes it upper).

NAME is read after ``{{version}}`` is filled in (``{{url:citar-helper-{{version}}-windows-x64.zip}}``) and must be one
of the release's assets (``scripts/release.py assets V``, P5.10's names); anything else in double braces is refused,
naming the template and its line. So no manifest can point at a file a release does not have, and none names PyPI's
sdist (whose URL 404s until PyPI has caught up).

- With ``--from-dir DIR`` the hashes are the files' in DIR: CI's route (package.yml's ``assemble``), before anything
  is published. Without it they are the published release's ``SHA256SUMS``, read from the base URL (after
  publishing, P5.15 step 8).
- ``--base-url URL`` points the manifests elsewhere: a dry run's loopback server (package.yml's ``managers``). It is
  https, or http to a loopback address only.
- ``--out DIR`` writes the manifests under DIR in ``packaging/``'s layout; ``--write`` writes them beside their
  templates in ``packaging/`` for review (those files are ignored by git: one release's numbers, never source).
- ``--write-pypi-json FILE`` (with ``--from-dir``) writes the release's wheels and sdist in the shape of PyPI's JSON
  API (``https://pypi.org/pypi/citar/<P>/json``: ``urls``, each with its ``filename`` and ``digests``), the stand-in
  ``--verify --pypi-json`` reads before anything is on PyPI.
- ``--verify V`` downloads the release's ``SHA256SUMS`` (from ``--base-url``, by default GitHub's), which must list
  exactly the release's assets (with the manifest's ``.sig`` or without), then every asset and its ``.sha256``, each
  held to the sums; then PyPI's files for V (PyPI's JSON API, or ``--pypi-json FILE``), which must be exactly the
  release's wheels and sdist, each with the release's hash. Every problem names its file. P5.15's step 7 runs it
  after publishing, and its dry-run form (a loopback ``--base-url`` and ``--pypi-json``) is rehearsed before then.

Standard library only. Exit status 0 when done (and with ``--verify``, every file holds); 1 when a file is missing,
differs or a template is refused, each named; 2 for a usage error.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import ipaddress
import json
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REPO = "jprodgers/CITAR"
#: Where a release's files are downloaded from (``{version}`` without the leading v).
RELEASE_URL = f"https://github.com/{REPO}/releases/download/v{{version}}/"
#: PyPI's JSON API for one version of the project.
PYPI_URL = "https://pypi.org/pypi/citar/{pep440}/json"
SUMS = "SHA256SUMS"
SUFFIX = ".in"
#: A placeholder naming an asset, read after ``{{version}}`` and ``{{date}}`` are filled in.
ASSET_PLACEHOLDER = re.compile(r"\{\{(sha256|SHA256|url):([^{}\s]+)\}\}")
#: One line of SHA256SUMS as sha256sum writes it (two spaces, or a space and ``*`` in binary mode).
SUMS_LINE = re.compile(r"([0-9a-f]{64}) [ *](\S+)")
TIMEOUT = 120.0


class Problem(Exception):
    """A file missing or different, or a template refused: exit 1, naming it."""


class Usage(Exception):
    """The command cannot run as given: exit 2."""


def _release_py():
    """scripts/release.py, as a module (the scripts are not a package): the asset list, the version and the changelog."""
    name = "citar_release_for_checksums"
    if name in sys.modules:
        return sys.modules[name]
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / "release.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module      # dataclasses look their module up while a class is made
    spec.loader.exec_module(module)
    return module


release = _release_py()


def say(text: str) -> None:
    print(f"release_checksums: {text}", flush=True)


# ------------------------------------------------------------------------------------------------ files and URLs


def sha256_of_file(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def loopback(host: str) -> bool:
    if host == "localhost":
        return True
    try:
        return ipaddress.ip_address(host.strip("[]")).is_loopback
    except ValueError:
        return False


def base_url(given: str | None, version: str) -> str:
    """The folder the manifests point into and ``--verify`` downloads from, ending in ``/``: https, or http to a
    loopback address (a dry run's server), and never with a user name in it."""
    url = given if given is not None else RELEASE_URL.format(version=version)
    parts = urllib.parse.urlsplit(url)
    if parts.scheme not in ("https", "http") or not parts.hostname:
        raise Usage(f"--base-url {url}: an https URL, or http to a loopback address")
    if parts.scheme == "http" and not loopback(parts.hostname):
        raise Usage(f"--base-url {url}: plain http is for a loopback address only (127.0.0.1, ::1, localhost)")
    if parts.username is not None or parts.query or parts.fragment:
        raise Usage(f"--base-url {url}: no user name, query or fragment")
    return url if url.endswith("/") else url + "/"


def fetch(url: str, timeout: float = TIMEOUT) -> bytes:
    with urllib.request.urlopen(url, timeout=timeout) as response:
        return response.read()


def sha256_of_url(url: str, timeout: float = TIMEOUT) -> tuple[str, int]:
    """The SHA-256 and size of a download, never kept whole in memory."""
    digest, size = hashlib.sha256(), 0
    with urllib.request.urlopen(url, timeout=timeout) as response:
        for chunk in iter(lambda: response.read(1 << 20), b""):
            digest.update(chunk)
            size += len(chunk)
    return digest.hexdigest(), size


def read_sums(text: str, where: str) -> dict[str, str]:
    """SHA256SUMS's lines, name -> hash; a line that does not read, or a name given twice, is a problem."""
    sums: dict[str, str] = {}
    for number, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            continue
        m = SUMS_LINE.fullmatch(line.rstrip("\r"))
        if not m:
            raise Problem(f"{where}:{number}: not a sha256sum line: {line!r}")
        if m[2] in sums:
            raise Problem(f"{where}:{number}: {m[2]} is listed twice")
        sums[m[2]] = m[1]
    return sums


def release_names(version: str, signed: bool = False) -> list[str]:
    """The release's assets (no ``.sha256`` file, no SHA256SUMS), in P5.10's order."""
    return [a["name"] for a in release.assets(version, signed)]


def python_dists(version: str) -> list[str]:
    """The wheels and the sdist: what PyPI holds for V."""
    return [a["name"] for a in release.assets(version)
            if a["group"] == "python" and a["name"].endswith((".whl", ".tar.gz"))]


# ------------------------------------------------------------------------------------------------ the templates


def templates(root: Path) -> list[Path]:
    found = sorted((root / "packaging").rglob(f"*{SUFFIX}"))
    if not found:
        raise Usage(f"no templates under {root / 'packaging'} (packaging/**/*{SUFFIX})")
    return found


def release_date(root: Path, version: str, given: str | None) -> str:
    if given is not None:
        if not release.valid_date(given):
            raise Usage(f"--date {given}: not a date, YYYY-MM-DD")
        return given
    try:
        text = release.Text(root, release.CHANGELOG)
    except OSError as e:
        raise Problem(f"{release.CHANGELOG}: {e}; give --date") from None
    found, _ = release.sections(text)
    for section in found:
        if section.name == version:
            if section.date and release.valid_date(section.date):
                return section.date
            raise Problem(f"{release.CHANGELOG}:{section.index + 1}: the section of {version} has no date; give "
                          f"--date")
    raise Problem(f"{release.CHANGELOG} has no section '## [{version}]': give --date")


def referenced(text: str, version: str, where: str) -> list[str]:
    """The assets a template names, refused when one is no asset of V."""
    names = set(release_names(version, signed=True))
    found = []
    for number, line in enumerate(text.replace("{{version}}", version).splitlines(), 1):
        for m in ASSET_PLACEHOLDER.finditer(line):
            if m[2] not in names:
                raise Problem(f"{where}:{number}: {m[0]} names {m[2]}, which is no asset of {version} "
                              f"(scripts/release.py assets {version})")
            found.append(m[2])
    return found


def render(text: str, version: str, date: str, hashes: dict[str, str], base: str, where: str) -> str:
    """A template filled in. Every placeholder must be one of the vocabulary, every asset one of V's."""
    referenced(text, version, where)
    text = text.replace("{{version}}", version).replace("{{date}}", date)

    def fill(m: re.Match) -> str:
        kind, name = m[1], m[2]
        if kind == "url":
            return base + name
        if name not in hashes:
            raise Problem(f"{where}: no SHA-256 for {name}")
        return hashes[name].upper() if kind == "SHA256" else hashes[name]

    out = ASSET_PLACEHOLDER.sub(fill, text)
    for number, line in enumerate(out.splitlines(), 1):
        if "{{" in line or "}}" in line:
            raise Problem(f"{where}:{number}: a placeholder this script does not know: {line.strip()}")
    return out


def needed(root: Path, version: str) -> list[str]:
    """Every asset the templates name, once each, in P5.10's order."""
    names: set[str] = set()
    for path in templates(root):
        names.update(referenced(path.read_text(encoding="utf-8"), version, str(path.relative_to(root))))
    return [n for n in release_names(version, signed=True) if n in names]


def hashes_from_dir(folder: Path, names: list[str]) -> dict[str, str]:
    missing = [n for n in names if not (folder / n).is_file()]
    if missing:
        raise Problem(f"{folder} lacks {', '.join(missing)}")
    return {n: sha256_of_file(folder / n) for n in names}


def hashes_from_release(base: str, names: list[str]) -> dict[str, str]:
    try:
        sums = read_sums(fetch(base + SUMS).decode("utf-8"), base + SUMS)
    except (urllib.error.URLError, OSError, UnicodeDecodeError) as e:
        raise Problem(f"{base}{SUMS}: {e}") from None
    missing = [n for n in names if n not in sums]
    if missing:
        raise Problem(f"{base}{SUMS} lists no {', '.join(missing)}")
    return {n: sums[n] for n in names}


def write_manifests(root: Path, version: str, date: str, hashes: dict[str, str], base: str,
                    out: Path) -> list[Path]:
    written = []
    for path in templates(root):
        rel = path.relative_to(root / "packaging")
        text = path.read_text(encoding="utf-8")
        target = out / rel.with_name(rel.name[: -len(SUFFIX)])
        body = render(text, version, date, hashes, base, str(path.relative_to(root)))
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(body, encoding="utf-8", newline="\n")
        written.append(target)
    return written


def pypi_json(folder: Path, version: str) -> dict:
    """The release's wheels and sdist in DIR, in the shape of PyPI's JSON API for one version."""
    v = release.Version.parse(version)
    urls = []
    for name in python_dists(version):
        path = folder / name
        if not path.is_file():
            raise Problem(f"{folder} lacks {name}")
        data = path.read_bytes()
        urls.append({"filename": name, "packagetype": "sdist" if name.endswith(".tar.gz") else "bdist_wheel",
                     "size": len(data), "url": f"https://files.pythonhosted.org/packages/{name}",
                     "digests": {"sha256": hashlib.sha256(data).hexdigest(), "md5": hashlib.md5(data).hexdigest()}})
    return {"info": {"name": "citar", "version": v.pep440}, "urls": urls}


# ------------------------------------------------------------------------------------------------ --verify


def pypi_hashes(data: object, where: str) -> dict[str, str]:
    """filename -> sha256 from PyPI's JSON for one version (its ``urls``)."""
    urls = data.get("urls") if isinstance(data, dict) else None
    if not isinstance(urls, list):
        raise Problem(f"{where}: no 'urls' list (PyPI's JSON API for one version)")
    out: dict[str, str] = {}
    for item in urls:
        name = item.get("filename") if isinstance(item, dict) else None
        digest = (item.get("digests") or {}).get("sha256") if isinstance(item, dict) else None
        if not isinstance(name, str) or not isinstance(digest, str):
            raise Problem(f"{where}: an entry of 'urls' without its filename and sha256: {item!r}")
        out[name] = digest.lower()
    return out


def verify(version: str, base: str, pypi: Path | None, timeout: float = TIMEOUT) -> list[str]:
    """Every problem with the published release (``base``) and PyPI's files, each naming its file."""
    problems: list[str] = []
    try:
        sums = read_sums(fetch(base + SUMS, timeout).decode("utf-8"), SUMS)
    except (urllib.error.URLError, OSError, UnicodeDecodeError) as e:
        return [f"{SUMS}: cannot be downloaded from {base}: {e}"]
    except Problem as e:
        return [str(e)]
    signed = "citar-helper-manifest.json.sig" in sums
    names = release_names(version, signed)
    for name in names:
        if name not in sums:
            problems.append(f"{name}: {SUMS} does not list it")
    for name in sums:
        if name not in names:
            problems.append(f"{name}: {SUMS} lists it, and it is no asset of {version}")
    for name in names:
        before = len(problems)
        try:
            digest, size = sha256_of_url(base + name, timeout)
        except (urllib.error.URLError, OSError) as e:
            problems.append(f"{name}: cannot be downloaded: {e}")
            continue
        if name in sums and digest != sums[name]:
            problems.append(f"{name}: its SHA-256 is {digest}, and {SUMS} says {sums[name]}")
        try:
            side = fetch(base + name + ".sha256", timeout).decode("ascii").split()
        except (urllib.error.URLError, OSError, UnicodeDecodeError) as e:
            problems.append(f"{name}.sha256: cannot be downloaded: {e}")
            continue
        if not side or side[0].lower() != digest:
            problems.append(f"{name}.sha256: says {side[0] if side else 'nothing'}, and the file's SHA-256 is "
                            f"{digest}")
        if len(problems) == before:
            say(f"ok: {name} ({size:,} bytes) {digest}")
    v = release.Version.parse(version)
    where = str(pypi) if pypi is not None else PYPI_URL.format(pep440=v.pep440)
    try:
        data = json.loads(pypi.read_text(encoding="utf-8") if pypi is not None else fetch(where, timeout))
        on_pypi = pypi_hashes(data, where)
    except (urllib.error.URLError, OSError, ValueError) as e:
        problems.append(f"PyPI: {where}: {e}")
        return problems
    except Problem as e:
        problems.append(str(e))
        return problems
    dists = python_dists(version)
    for name in dists:
        if name not in on_pypi:
            problems.append(f"{name}: PyPI ({where}) has no such file")
        elif name in sums and on_pypi[name] != sums[name]:
            problems.append(f"{name}: PyPI's SHA-256 is {on_pypi[name]}, and the release's {sums[name]}")
        else:
            say(f"ok: {name} on PyPI is the release's")
    for name in on_pypi:
        if name not in dists:
            problems.append(f"{name}: PyPI has it, and the release has no such wheel or sdist")
    return problems


# ------------------------------------------------------------------------------------------------ command line


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="release_checksums.py", description=__doc__.split("\n\n")[0],
                                allow_abbrev=False)
    p.add_argument("version", nargs="?", help="the release's version (V), without a leading v")
    p.add_argument("--verify", metavar="V", help="check the published release V and PyPI's files")
    p.add_argument("--from-dir", type=Path, help="hash the release's files in DIR (else the published SHA256SUMS)")
    p.add_argument("--base-url", help="where the manifests point and --verify downloads from")
    p.add_argument("--date", help="the release's date (default: its CHANGELOG section's)")
    p.add_argument("--out", type=Path, help="write the manifests under DIR")
    p.add_argument("--write", action="store_true", help="write the manifests beside their templates in packaging/")
    p.add_argument("--write-pypi-json", type=Path, metavar="FILE",
                   help="with --from-dir: the wheels and sdist in PyPI's JSON shape")
    p.add_argument("--pypi-json", type=Path, metavar="FILE", help="with --verify: PyPI's JSON from FILE")
    p.add_argument("--root", type=Path, default=ROOT, help="the checkout (default: this script's)")
    return p


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    root = args.root.resolve()
    try:
        if args.verify is not None:
            if args.version or args.from_dir or args.out or args.write or args.write_pypi_json or args.date:
                raise Usage("--verify V takes only --base-url and --pypi-json")
            release.Version.parse(args.verify)
            base = base_url(args.base_url, args.verify)
            say(f"verifying {args.verify} at {base}")
            problems = verify(args.verify, base, args.pypi_json)
            for problem in problems:
                print(f"release_checksums: FAILED: {problem}", file=sys.stderr, flush=True)
            if problems:
                print(f"release_checksums: {len(problems)} problem(s) with {args.verify}", file=sys.stderr)
                return 1
            say(f"{args.verify}: every asset holds to {SUMS} and its .sha256, and PyPI's files are the release's")
            return 0
        if not args.version:
            raise Usage("give the release's version, or --verify V")
        if args.pypi_json is not None:
            raise Usage("--pypi-json goes with --verify")
        version = release.Version.parse(args.version).text
        if args.write_pypi_json is not None:
            if args.from_dir is None or args.out or args.write:
                raise Usage("--write-pypi-json FILE goes with --from-dir DIR alone")
            data = pypi_json(args.from_dir, version)
            args.write_pypi_json.parent.mkdir(parents=True, exist_ok=True)
            args.write_pypi_json.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8", newline="\n")
            say(f"wrote {args.write_pypi_json} ({len(data['urls'])} files)")
            return 0
        if args.out is not None and args.write:
            raise Usage("--out DIR or --write, not both")
        base = base_url(args.base_url, version)
        names = needed(root, version)
        # Without --from-dir the hashes are the published release's: its SHA256SUMS, where the manifests point.
        hashes = hashes_from_dir(args.from_dir, names) if args.from_dir else hashes_from_release(base, names)
        date = release_date(root, version, args.date)
        width = max(len(n) for n in names)
        for name in names:
            print(f"  {name:<{width}}  {hashes[name]}")
        out = root / "packaging" if args.write else args.out
        if out is None:
            say("nothing written: give --out DIR, or --write for packaging/")
            return 0
        for path in write_manifests(root, version, date, hashes, base, out):
            say(f"wrote {path}")
        say(f"{version} ({date}), its files at {base}")
        return 0
    except release.Refused as e:
        print(f"release_checksums: {e}", file=sys.stderr)
        return 2
    except Usage as e:
        print(f"release_checksums: {e}", file=sys.stderr)
        return 2
    except Problem as e:
        print(f"release_checksums: FAILED: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
