"""The Windows bundle's ``PYTHON-LICENSES.txt``: the licences of what it ships of Python (design/phase5-release.md
P5.5, P5.6).

    python -P installer/python_licenses.py --out PYTHON-LICENSES.txt

Run with the bundle's own interpreter, in the venv the bundle is built from (the Windows wheel installed with
``[all]``, PyInstaller beside it). The wheel's own notices (``LICENSE``, ``NOTICE.md`` and ``THIRD-PARTY-LICENSES.txt``,
the Rust crates its extension links) travel beside this file; this one covers the rest of the bundle:

- **The Python packages it ships:** the installed dependency closure of ``citar[all]``, walked through
  ``importlib.metadata`` from ``citar``'s own requirements (each requirement's markers evaluated for this interpreter
  and the extras asked for, a package's extras followed as it names them). So PyInstaller, pillow, pip, setuptools and
  the other build tools, which the venv holds but the bundle does not ship, are left out (``installer/spec_rules.py``
  fails a build whose analysis took a file of any distribution outside this closure). Each package's name, version
  and licence (its ``License-Expression``, else its ``License`` field and licence classifiers) and the text of every
  licence or notice file in its installed metadata (PEP 639's ``licenses/`` folder, or an older wheel's files beside
  its ``METADATA``). A package whose installed metadata holds no licence file takes CITAR's copy of its project's
  licence file from ``installer/licenses/<name>.txt`` (:data:`KEPT`); one with neither is refused, so the next such
  package fails the build instead of shipping without its notice. It errs on the side of more: PyInstaller ships only
  the modules something imports, while this lists every distribution the closure names.
- **CPython itself,** whose runtime the bundle carries: its ``LICENSE.txt`` from the interpreter's installation
  (``sys.base_prefix``; on Windows it includes the licences of the libraries CPython bundles, OpenSSL and SQLite
  among them, and the terms of the Microsoft runtime it redistributes).
- **What PyInstaller puts in the executables:** its bootloader and loader, the start of ``citar.exe`` and
  ``citar-play.exe`` (the GPL with its bootloader exception, which lets the bootloader start programs under any
  licence), and the run-time hooks of PyInstaller and of its community hooks (``pyinstaller-hooks-contrib``), which
  the executables run before CITAR's code (the Apache License 2.0); the licence files each ships.

A text printed once is not printed again: a later package with the same bytes names where it is. The file holds no
path, time or machine detail, so two runs over the same venv write the same bytes.

Exit 0 when written, 1 when the closure cannot be walked or a licence is missing (a requirement not installed, a
shipped package with no licence text, no CPython or PyInstaller licence), 2 on a usage error.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.metadata as md
import sys
import sysconfig
from pathlib import Path, PurePosixPath
from typing import Iterable, Optional

#: The distribution whose closure is walked, and the extra the bundle installs it with.
ROOT, EXTRAS = "citar", ("all",)
#: A file in a distribution's metadata is a licence or notice text when its name starts so (as cargo xtask licenses
#: reads a crate's), or it sits in a folder of one of the folder names.
TEXT_PREFIXES = ("license", "licence", "copying", "copyright", "notice", "unlicense")
TEXT_FOLDERS = {"license", "licenses", "licence", "licences"}
#: The distribution that makes the bundle's executables, whose bootloader, loader and run-time hooks they carry.
PYINSTALLER = "pyinstaller"
#: PyInstaller's community hooks, whose run-time hooks the executables carry too (its other files are not shipped).
HOOKS_CONTRIB = "pyinstaller-hooks-contrib"
#: Where CPython keeps its licence, relative to its installation (Windows) or its standard library (elsewhere).
CPYTHON_LICENSE = "LICENSE.txt"
#: CITAR's copies of the licence files of shipped packages whose installed metadata holds none, by canonical name:
#: ``installer/licenses/<name>.txt``, each its project's own file, byte for byte, and where it was taken from.
#: - dnspython (ISC, through email-validator): its ``LICENSE``, as dnspython 2.8.0's wheel carries it
#:   (``dist-info/licenses/LICENSE``, SHA-256 c3ea3ff5...d741cd), the same text as the v2.9.0 tag's ``LICENSE``;
#:   2.9.0's wheel carries no licence file. The ISC licence asks for its notice in every copy.
KEPT_DIR = Path(__file__).resolve().parent / "licenses"
KEPT = {"dnspython": "dnspython.txt"}

HEADER = """\
Python licences: what CITAR's Windows bundle ships of Python
=============================================================

CITAR's Windows bundle (CITAR-<version>-setup.exe, CITAR-<version>-windows-x64.zip) carries a Python runtime and
the Python packages CITAR runs on, frozen by PyInstaller into citar.exe and citar-play.exe. This file gives their
licences. CITAR's own licence is LICENSE (MPL-2.0), its notices NOTICE.md, and the Rust crates its engine links are
in THIRD-PARTY-LICENSES.txt, all three beside this file.

It is written by installer/python_licenses.py from the venv the bundle is built from:
- every Python package of the installed dependency closure of citar[all], in name order, with its version, its
  licence as its metadata states it, and every licence or notice file its installed metadata holds, or, for a
  package whose metadata holds none, CITAR's copy of its project's licence file (the closure errs on the side of
  more: the bundle ships only the modules something imports);
- CPython's own licence, for the runtime (with the licences of the libraries CPython bundles on Windows);
- what PyInstaller puts in citar.exe and citar-play.exe: its bootloader and loader (the GPL with its bootloader
  exception, which allows them in programs under any licence) and the run-time hooks of PyInstaller and of its
  community hooks (the Apache License 2.0).
The rest of PyInstaller, pip, setuptools and the other tools the bundle is built with are not shipped, and are not
listed.

A text printed once is not printed again: a later file with the same bytes names where it is printed in full.
"""


class Failed(Exception):
    """The closure cannot be listed: a requirement not installed, a licence not found."""


def canonical(name: str) -> str:
    """A distribution's name as PEP 503 compares it."""
    import re

    return re.sub(r"[-_.]+", "-", name).lower()


def installed(path: Optional[list] = None) -> dict:
    """Every installed distribution by canonical name (``path`` instead of ``sys.path`` when given); the first found
    wins, as an import would."""
    out: dict = {}
    for dist in md.distributions(**({"path": path} if path is not None else {})):
        name = dist.metadata["Name"]
        if name and canonical(name) not in out:
            out[canonical(name)] = dist
    return out


def requirement_class():
    """PEP 508's reader, ``packaging``'s: the installed one (PyInstaller needs it, so the bundle's venv has it), else the
    copy pip carries (every venv has pip)."""
    try:
        from packaging.requirements import Requirement
    except ImportError:
        from pip._vendor.packaging.requirements import Requirement
    return Requirement


def closure(dists: dict, root: str = ROOT, extras: Iterable[str] = EXTRAS,
            environment: Optional[dict] = None) -> dict:
    """The distributions ``root[extras]`` needs here, root excluded, by canonical name: each requirement of a
    distribution reached, whose marker holds for this interpreter (``environment`` over packaging's default) with no
    extra or with one asked of that distribution, followed with the extras it names. Raises Failed for a requirement
    that holds and is not installed."""
    Requirement = requirement_class()

    want = [(canonical(root), frozenset(extras))]
    done: dict = {}                 # canonical name -> the extras already followed
    found: dict = {}
    while want:
        name, asked = want.pop()
        dist = dists.get(name)
        if dist is None:
            raise Failed(f"{name} is required but not installed")
        new = asked - done.get(name, frozenset())
        first = name not in done
        if not first and not new:
            continue
        done[name] = done.get(name, frozenset()) | asked
        if name != canonical(root):
            found[name] = dist
        for text in dist.requires or ():
            req = Requirement(text)
            # a requirement with no marker holds once, on the first visit; one under an extra, when that extra is new
            consider = ("",) + tuple(sorted(new)) if first else tuple(sorted(new))
            if req.marker is not None and not any(
                    req.marker.evaluate({**(environment or {}), "extra": extra}) for extra in consider):
                continue
            if req.marker is None and not first:
                continue
            want.append((canonical(req.name), frozenset(req.extras)))
    return found


def shipped(dists: dict) -> set:
    """The canonical names of the distributions the bundle may carry files of: ``citar`` and its closure."""
    return set(closure(dists)) | {canonical(ROOT)}


def licence_of(dist) -> str:
    """A distribution's licence as its metadata states it: ``License-Expression``; else its ``License`` field (its
    first line when it holds a whole text) and its licence classifiers; else that it states none."""
    meta = dist.metadata
    expression = (meta.get("License-Expression") or "").strip()
    if expression:
        return expression
    parts = []
    field = (meta.get("License") or "").strip()
    if field and field.upper() != "UNKNOWN":
        first = field.splitlines()[0].strip()
        parts.append(first if len(first) <= 120 else first[:117] + "...")
    for classifier in meta.get_all("Classifier") or ():
        if classifier.startswith("License ::"):
            parts.append(classifier.split("::")[-1].strip())
    return "; ".join(dict.fromkeys(parts)) or "not stated in its metadata"


def _is_text(rel: PurePosixPath) -> bool:
    name = rel.name.lower()
    return name.startswith(TEXT_PREFIXES) or any(part.lower() in TEXT_FOLDERS for part in rel.parts[:-1])


def licence_files(dist) -> list:
    """``(path in its metadata folder, text)`` for each licence or notice file a distribution's installed metadata
    holds, in path order: its ``License-File`` entries (PEP 639 keeps them under ``licenses/``; an older wheel beside
    ``METADATA``), and any other file there named as a licence or notice, or in a ``licenses`` folder."""
    found: dict = {}
    for entry in dist.metadata.get_all("License-File") or ():
        for rel in (f"licenses/{entry}", entry):
            text = dist.read_text(rel)
            if text is not None:
                found[rel] = text
                break
    for file in dist.files or ():
        parts = PurePosixPath(str(file).replace("\\", "/")).parts
        if len(parts) < 2 or not parts[0].endswith((".dist-info", ".egg-info")):
            continue
        rel = PurePosixPath(*parts[1:])
        if str(rel) in found or not _is_text(rel):
            continue
        text = dist.read_text(str(rel))
        if text is not None:
            found[str(rel)] = text
    return sorted(found.items())


def kept_files(name: str, kept_dir: Path = KEPT_DIR, kept: Optional[dict] = None) -> list:
    """``[(where, text)]``: CITAR's copy of a distribution's licence file (``installer/licenses/<file>``), or ``[]``."""
    file = (KEPT if kept is None else kept).get(canonical(name))
    if not file or not (kept_dir / file).is_file():
        return []
    return [(f"CITAR's copy of its licence file, installer/licenses/{file}",
             (kept_dir / file).read_text(encoding="utf-8"))]


def texts_of(dist, kept_dir: Path = KEPT_DIR, kept: Optional[dict] = None) -> tuple:
    """``(files, kept)``: a shipped distribution's licence texts, from its installed metadata, else CITAR's copy
    (``kept`` True). Raises Failed when it has neither: the bundle would ship it without its licence's notice."""
    files = licence_files(dist)
    if files:
        return files, False
    files = kept_files(dist.metadata["Name"], kept_dir, kept)
    if files:
        return files, True
    raise Failed(f"{dist.metadata['Name']} {dist.version} ({licence_of(dist)}) holds no licence file in its installed "
                 f"metadata and installer/licenses/ keeps no copy of it: put its project's licence file there, "
                 f"verbatim at this version, and name it in python_licenses.KEPT")


def cpython_licence() -> tuple:
    """``(where, text)`` of this interpreter's ``LICENSE.txt``: in its installation (Windows), else beside its standard
    library. Raises Failed when neither holds it."""
    for folder, where in ((Path(sys.base_prefix), "the installation"),
                          (Path(sysconfig.get_path("stdlib")), "the standard library")):
        path = folder / CPYTHON_LICENSE
        if path.is_file():
            return where, path.read_text(encoding="utf-8", errors="replace")
    raise Failed(f"no {CPYTHON_LICENSE} in {sys.base_prefix} or beside the standard library")


#: What each of PyInstaller's distributions puts in the executables: (its name as written here, the index line's
#: subject and licence, the section's account of it).
RUNTIME = {
    PYINSTALLER: ("PyInstaller", "bootloader and run-time hooks",
                  "GPL-2.0-or-later WITH Bootloader-exception; Apache-2.0",
                  "the licence PyInstaller ships: its bootloader and loader, the start of citar.exe and "
                  "citar-play.exe, under the GPL with the bootloader exception, and its run-time hooks, which the "
                  "executables also carry, under the Apache License 2.0 (both in the text below)"),
    HOOKS_CONTRIB: ("pyinstaller-hooks-contrib", "run-time hooks", "Apache-2.0",
                    "the run-time hooks of PyInstaller's community hooks, which citar.exe and citar-play.exe carry, "
                    "under the Apache License 2.0 (in the text below; its other files, under the GPL, are not "
                    "shipped)"),
}


def runtime_licences(dists: dict) -> list:
    """``[(name, version, subject, licence, account, [(file, text)])]`` for what PyInstaller puts in the executables:
    PyInstaller's bootloader, loader and run-time hooks, and its community hooks' run-time hooks when installed (as
    PyInstaller requires). Raises Failed when PyInstaller is not installed or either ships no licence file."""
    if PYINSTALLER not in dists:
        raise Failed("PyInstaller is not installed: its bootloader's licence is read from it")
    out = []
    for key, (name, subject, licence, account) in RUNTIME.items():
        dist = dists.get(key)
        if dist is None:
            continue
        files = licence_files(dist)
        if not files:
            raise Failed(f"{name} {dist.version} ships no licence file in its metadata")
        out.append((name, dist.version, subject, licence, account, files))
    return out


def _clean(text: str) -> str:
    """A text with LF line endings, no byte-order mark and no blank lines around it."""
    return text.lstrip("\ufeff").replace("\r\n", "\n").replace("\r", "\n").strip("\n") + "\n"


def render(found: dict, cpython: tuple, runtime: Optional[list], python_version: str, kept_dir: Path = KEPT_DIR,
           kept: Optional[dict] = None) -> str:
    """The file: the header, the index, a section per distribution in name order, CPython's, and PyInstaller's
    run-time parts. Raises Failed for a distribution with no licence text (:func:`texts_of`)."""
    out = [HEADER.rstrip("\n"), "", "Index", "-----"]
    rows = sorted(found.values(), key=lambda d: canonical(d.metadata["Name"]))
    for dist in rows:
        out.append(f"{dist.metadata['Name']} {dist.version}: {licence_of(dist)}")
    out.append(f"CPython {python_version}: PSF-2.0 (Python Software Foundation License Version 2)")
    for name, version, subject, licence, _, _ in runtime or ():
        out.append(f"{name} {version}'s {subject}: {licence}")
    printed: dict = {}

    def texts(owner: str, files: list) -> None:
        for rel, text in files:
            body = _clean(text)
            key = hashlib.sha256(body.encode("utf-8")).hexdigest()
            if key in printed:
                out.append(f"---- {owner}: {rel}: the same text as {printed[key]} ----")
                continue
            printed[key] = f"{owner}: {rel}"
            out.extend([f"---- {owner}: {rel} ----", body.rstrip("\n")])

    for dist in rows:
        name = f"{dist.metadata['Name']} {dist.version}"
        files, from_citar = texts_of(dist, kept_dir, kept)
        out.extend(["", "=" * 117, name, "=" * 117, f"Licence: {licence_of(dist)}"])
        home = dist.metadata.get("Home-page") or next(
            (u.split(",", 1)[1].strip() for u in dist.metadata.get_all("Project-URL") or ()
             if u.lower().startswith(("homepage", "source", "repository"))), "")
        if home:
            out.append(f"Project: {home}")
        out.append("Files: " + ("none in its installed metadata; " if from_citar else "")
                   + ", ".join(rel for rel, _ in files))
        texts(name, files)
    out.extend(["", "=" * 117, f"CPython {python_version}", "=" * 117, f"Licence: from {cpython[0]}'s {CPYTHON_LICENSE}"])
    texts(f"CPython {python_version}", [(CPYTHON_LICENSE, cpython[1])])
    for name, version, subject, _, account, files in runtime or ():
        out.extend(["", "=" * 117, f"{name} {version}'s {subject}", "=" * 117, f"Licence: {account}"])
        texts(f"{name} {version}", files)
    return "\n".join(out) + "\n"


def build(path: Optional[list] = None, runtime: bool = True, kept_dir: Path = KEPT_DIR,
          kept: Optional[dict] = None) -> tuple:
    """``(the file's text, the distributions listed)`` for this interpreter's installed closure of ``citar[all]``."""
    dists = installed(path)
    found = closure(dists)
    parts = runtime_licences(dists) if runtime else None
    version = ".".join(str(n) for n in sys.version_info[:3])
    return render(found, cpython_licence(), parts, version, kept_dir, kept), sorted(found)


def main(argv: Optional[list] = None) -> int:
    ap = argparse.ArgumentParser(prog="python_licenses.py", description=__doc__.split("\n\n", 1)[0])
    ap.add_argument("--out", type=Path, required=True, help="the file to write (PYTHON-LICENSES.txt)")
    args = ap.parse_args(argv)
    try:
        text, names = build()
    except Failed as e:
        print(f"python_licenses: {e}", file=sys.stderr)
        return 1
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(text, encoding="utf-8", newline="\n")
    print(f"python_licenses: {args.out}: {len(names)} distributions of citar[all]'s closure, CPython's licence and "
          f"PyInstaller's run-time parts'; {len(text.encode('utf-8')):,} bytes")
    return 0


if __name__ == "__main__":
    sys.exit(main())
