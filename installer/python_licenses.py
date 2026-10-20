"""The Windows bundle's ``PYTHON-LICENSES.txt``: the licences of what it ships of Python (design/phase5-release.md
P5.5, P5.6).

    python -P installer/python_licenses.py --out PYTHON-LICENSES.txt

Run with the bundle's own interpreter, in the venv the bundle is built from (the Windows wheel installed with
``[all]``, PyInstaller beside it). The wheel's own notices (``LICENSE``, ``NOTICE.md`` and ``THIRD-PARTY-LICENSES.txt``,
the Rust crates its extension links) travel beside this file; this one covers the rest of the bundle:

- **The Python packages it ships:** the installed dependency closure of ``citar[all]``, walked through
  ``importlib.metadata`` from ``citar``'s own requirements (each requirement's markers evaluated for this interpreter
  and the extras asked for, a package's extras followed as it names them). So PyInstaller, pillow, pip and the other
  build tools, which the venv holds but the bundle does not ship, are left out. Each package's name, version and
  licence (its ``License-Expression``, else its ``License`` field and licence classifiers) and the text of every
  licence or notice file in its installed metadata (PEP 639's ``licenses/`` folder, or an older wheel's files beside
  its ``METADATA``). It errs on the side of more: PyInstaller ships only the modules something imports, while this
  lists every distribution the closure names.
- **CPython itself,** whose runtime the bundle carries: its ``LICENSE.txt`` from the interpreter's installation
  (``sys.base_prefix``; on Windows it includes the licences of the libraries CPython bundles, OpenSSL and SQLite
  among them, and the terms of the Microsoft runtime it redistributes).
- **PyInstaller's bootloader,** the start of ``citar.exe`` and ``citar-play.exe``: the licence PyInstaller ships, the
  GPL with its bootloader exception, which lets the bootloader start programs under any licence.

A text printed once is not printed again: a later package with the same bytes names where it is. The file holds no
path, time or machine detail, so two runs over the same venv write the same bytes.

Exit 0 when written, 1 when the closure cannot be walked (a requirement not installed, no CPython or PyInstaller
licence), 2 on a usage error.
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
#: The distribution that makes the bundle's executables, whose bootloader they run.
PYINSTALLER = "pyinstaller"
#: Where CPython keeps its licence, relative to its installation (Windows) or its standard library (elsewhere).
CPYTHON_LICENSE = "LICENSE.txt"

HEADER = """\
Python licences: what CITAR's Windows bundle ships of Python
=============================================================

CITAR's Windows bundle (CITAR-<version>-setup.exe, CITAR-<version>-windows-x64.zip) carries a Python runtime and
the Python packages CITAR runs on, frozen by PyInstaller into citar.exe and citar-play.exe. This file gives their
licences. CITAR's own licence is LICENSE (MPL-2.0), its notices NOTICE.md, and the Rust crates its engine links are
in THIRD-PARTY-LICENSES.txt, all three beside this file.

It is written by installer/python_licenses.py from the venv the bundle is built from:
- every Python package of the installed dependency closure of citar[all], in name order, with its version, its
  licence as its metadata states it, and every licence or notice file its installed metadata holds (the closure
  errs on the side of more: the bundle ships only the modules something imports);
- CPython's own licence, for the runtime (with the licences of the libraries CPython bundles on Windows);
- the licence of PyInstaller's bootloader, the start of citar.exe and citar-play.exe (the GPL with its bootloader
  exception, which allows it in programs under any licence).
PyInstaller itself, pip and the other tools the bundle is built with are not shipped, and are not listed.

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


def closure(dists: dict, root: str = ROOT, extras: Iterable[str] = EXTRAS,
            environment: Optional[dict] = None) -> dict:
    """The distributions ``root[extras]`` needs here, root excluded, by canonical name: each requirement of a
    distribution reached, whose marker holds for this interpreter (``environment`` over packaging's default) with no
    extra or with one asked of that distribution, followed with the extras it names. Raises Failed for a requirement
    that holds and is not installed."""
    from packaging.requirements import Requirement

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


def cpython_licence() -> tuple:
    """``(where, text)`` of this interpreter's ``LICENSE.txt``: in its installation (Windows), else beside its standard
    library. Raises Failed when neither holds it."""
    for folder, where in ((Path(sys.base_prefix), "the installation"),
                          (Path(sysconfig.get_path("stdlib")), "the standard library")):
        path = folder / CPYTHON_LICENSE
        if path.is_file():
            return where, path.read_text(encoding="utf-8", errors="replace")
    raise Failed(f"no {CPYTHON_LICENSE} in {sys.base_prefix} or beside the standard library")


def bootloader_licence(dists: dict) -> tuple:
    """``(PyInstaller's version, [(file, text)])``: the licence PyInstaller ships, which covers its bootloader. Raises
    Failed when PyInstaller is not installed or ships none."""
    dist = dists.get(PYINSTALLER)
    if dist is None:
        raise Failed("PyInstaller is not installed: its bootloader's licence is read from it")
    files = licence_files(dist)
    if not files:
        raise Failed(f"PyInstaller {dist.version} ships no licence file in its metadata")
    return dist.version, files


def _clean(text: str) -> str:
    """A text with LF line endings, no byte-order mark and no blank lines around it."""
    return text.lstrip("﻿").replace("\r\n", "\n").replace("\r", "\n").strip("\n") + "\n"


def render(found: dict, cpython: tuple, bootloader: Optional[tuple], python_version: str) -> str:
    """The file: the header, the index, a section per distribution in name order, CPython's and the bootloader's."""
    out = [HEADER, "", "Index", "-----"]
    rows = sorted(found.values(), key=lambda d: canonical(d.metadata["Name"]))
    for dist in rows:
        out.append(f"{dist.metadata['Name']} {dist.version}: {licence_of(dist)}")
    out.append(f"CPython {python_version}: PSF-2.0 (Python Software Foundation License Version 2)")
    if bootloader is not None:
        out.append(f"PyInstaller {bootloader[0]}'s bootloader: GPL-2.0-or-later WITH Bootloader-exception")
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
        files = licence_files(dist)
        out.extend(["", "=" * 117, name, "=" * 117, f"Licence: {licence_of(dist)}"])
        home = dist.metadata.get("Home-page") or next(
            (u.split(",", 1)[1].strip() for u in dist.metadata.get_all("Project-URL") or ()
             if u.lower().startswith(("homepage", "source", "repository"))), "")
        if home:
            out.append(f"Project: {home}")
        out.append("Files: " + (", ".join(rel for rel, _ in files) if files else
                                "none: its installed metadata holds no licence file"))
        texts(name, files)
    out.extend(["", "=" * 117, f"CPython {python_version}", "=" * 117, f"Licence: from {cpython[0]}'s {CPYTHON_LICENSE}"])
    texts(f"CPython {python_version}", [(CPYTHON_LICENSE, cpython[1])])
    if bootloader is not None:
        out.extend(["", "=" * 117, f"PyInstaller {bootloader[0]}'s bootloader", "=" * 117,
                    "Licence: the licence PyInstaller ships, whose bootloader exception covers citar.exe and "
                    "citar-play.exe"])
        texts(f"PyInstaller {bootloader[0]}", bootloader[1])
    return "\n".join(out) + "\n"


def build(path: Optional[list] = None, bootloader: bool = True) -> tuple:
    """``(the file's text, the distributions listed)`` for this interpreter's installed closure of ``citar[all]``."""
    dists = installed(path)
    found = closure(dists)
    boot = bootloader_licence(dists) if bootloader else None
    version = ".".join(str(n) for n in sys.version_info[:3])
    return render(found, cpython_licence(), boot, version), sorted(found)


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
          f"PyInstaller's bootloader's; {len(text.encode('utf-8')):,} bytes")
    return 0


if __name__ == "__main__":
    sys.exit(main())
