"""What ``installer/citar.spec`` holds the Windows bundle to (design/phase5-release.md P5.6), apart from it so the
tests run it without PyInstaller.

The bundle is built **from the installed wheel, never the checkout**: PyInstaller runs in a venv with the Windows
wheel installed (``[all]``), as ``python -P -m PyInstaller <absolute path to citar.spec>`` from a folder outside the
checkout. ``-m`` from the checkout's root, a ``CITAR_EXT_DIR`` (the dev loop's extension folder, which
``citar/__init__.py`` puts first) or a checkout on ``sys.path`` would each let the analysis take the checkout's
``citar/`` or another build's ``_engine.pyd``; so the spec refuses to build unless ``citar`` and ``citar._engine``
resolve to the installed package, and fails after the analysis unless every module, extension and data file it took
of ``citar`` is that package's.

The notices go at the bundle's root, beside ``citar.exe``: ``LICENSE``, ``NOTICE.md`` and ``THIRD-PARTY-LICENSES.txt``
from the installed wheel's metadata (what the wheel carries; the checkout's ``packaging/`` only for a wheel without
the third), ``README.md`` from the checkout, and ``PYTHON-LICENSES.txt`` (``installer/python_licenses.py``).
"""
from __future__ import annotations

import importlib.metadata as md
import importlib.util
import os
import shutil
from pathlib import Path, PurePath
from typing import Iterable, Optional

#: The bundle's folder under PyInstaller's ``--distpath``.
BUNDLE = "CITAR"
#: The notices at the bundle's root, and where each comes from: the installed wheel's metadata (its PEP 639 path
#: under ``licenses/``), else the checkout.
WHEEL_NOTICES = {"LICENSE": "LICENSE", "NOTICE.md": "NOTICE.md",
                 "THIRD-PARTY-LICENSES.txt": "packaging/THIRD-PARTY-LICENSES.txt"}
CHECKOUT_NOTICES = {"README.md": "README.md", "THIRD-PARTY-LICENSES.txt": "packaging/THIRD-PARTY-LICENSES.txt"}
PYTHON_LICENSES = "PYTHON-LICENSES.txt"
#: The engine's extension in the bundle, as PyInstaller names its destination.
ENGINE = "citar/_engine.pyd"


class Refused(SystemExit):
    """The spec will not build: it says why, and PyInstaller stops."""

    def __init__(self, why: str):
        super().__init__(f"citar.spec: {why}")


def within(inner: Path, outer: Path) -> bool:
    """Whether ``inner`` is ``outer`` or inside it, both resolved (case-folded on Windows)."""
    a = os.path.normcase(os.path.realpath(os.path.abspath(str(inner))))
    b = os.path.normcase(os.path.realpath(os.path.abspath(str(outer))))
    return a == b or a.startswith(b.rstrip("\\/") + os.sep)


def installed_package(checkout: Path, ext_dir: Optional[str] = None) -> Path:
    """The folder of the ``citar`` package this interpreter imports, refused when it is the checkout's or when its
    ``citar._engine`` is not inside it (``CITAR_EXT_DIR``, ``ext_dir`` here, would put another build's first)."""
    if ext_dir:
        raise Refused(f"CITAR_EXT_DIR is set ({ext_dir}): the bundle takes the installed wheel's engine, never the dev "
                      f"loop's; build it in a venv of its own with the variable unset")
    spec = importlib.util.find_spec("citar")
    if spec is None or not spec.origin:
        raise Refused("citar is not installed in this interpreter: install the Windows wheel with [all] first")
    package = Path(spec.origin).resolve().parent
    if within(package, checkout):
        raise Refused(f"citar resolves to the checkout ({package}), not the installed wheel: run "
                      f"`python -P -m PyInstaller <absolute path to citar.spec>` from a folder outside the checkout, "
                      f"in the venv the wheel is installed in")
    engine = importlib.util.find_spec("citar._engine")
    if engine is None or not engine.origin or not within(Path(engine.origin), package):
        raise Refused(f"citar._engine resolves to {engine.origin if engine else None}, not the installed package "
                      f"{package}")
    return package


def _parts(name: str) -> tuple:
    return PurePath(name.replace("\\", "/")).parts


def check_analysis(pure: Iterable, binaries: Iterable, datas: Iterable, package: Path, checkout: Path) -> list:
    """What is wrong with an analysis's TOCs (``(name, source, kind)`` entries): a ``citar`` module, extension or data
    file whose source is not the installed ``package`` (or is the checkout's), and no engine extension. Empty when all
    is well."""
    problems = []
    for name, source, _ in pure:
        if name == "citar" or name.startswith("citar."):
            if not source or not within(Path(source), package) or within(Path(source), checkout):
                problems.append(f"the module {name} comes from {source}, not the installed package {package}")
    engine = []
    for kind, toc in (("extension", binaries), ("data file", datas)):
        for name, source, _ in toc:
            parts = _parts(name)
            if not parts or parts[0] != "citar":
                continue
            if "/".join(parts).lower() == ENGINE:
                engine.append(source)
            if not source or not within(Path(source), package) or within(Path(source), checkout):
                problems.append(f"the {kind} {name} comes from {source}, not the installed package {package}")
    if not engine:
        problems.append(f"the analysis took no {ENGINE}: the bundle would have no engine")
    elif len(engine) > 1:
        problems.append(f"the analysis took {ENGINE} {len(engine)} times: {engine}")
    if not any(name == "citar" for name, _, _ in pure):
        problems.append("the analysis took no citar package at all")
    return problems


def notices(checkout: Path, dist: Optional[md.Distribution] = None) -> dict:
    """``{name at the bundle's root: source file}`` for the wheel's notices and the README (not PYTHON-LICENSES.txt,
    which the spec writes). Raises Refused when one is missing."""
    if dist is None:
        dist = md.distribution("citar")
    out: dict = {}
    for name, rel in WHEEL_NOTICES.items():
        located = dist.locate_file(f"{_dist_info(dist)}/licenses/{rel}")
        if Path(located).is_file():
            out[name] = Path(located)
    for name, rel in CHECKOUT_NOTICES.items():
        if name not in out and (checkout / rel).is_file():
            out[name] = checkout / rel
    missing = [n for n in (*WHEEL_NOTICES, *CHECKOUT_NOTICES) if n not in out]
    if missing:
        raise Refused(f"no {', '.join(missing)} in the installed wheel or the checkout")
    return out


def _dist_info(dist: md.Distribution) -> str:
    """The name of a distribution's metadata folder (``citar-0.1.6.dist-info``)."""
    for file in dist.files or ():
        top = _parts(str(file))[0]
        if top.endswith(".dist-info"):
            return top
    raise Refused(f"{dist.metadata['Name']} {dist.version} has no .dist-info folder in its record")


def place(bundle: Path, files: dict) -> list:
    """Copies each notice to the bundle's root (``bundle`` is ``<distpath>/CITAR``); the names placed."""
    if not any((bundle / name).is_file() for name in ("citar.exe", "citar")):
        raise Refused(f"{bundle} holds no citar.exe: the bundle was not collected there")
    for name, source in sorted(files.items()):
        shutil.copyfile(source, bundle / name)
    return sorted(files)


def outside(checkout: Path, **folders: str) -> None:
    """Refuses a PyInstaller output folder inside the checkout (design/phase5-release.md P5.18.3: nothing builds into
    the synced checkout): pass ``--distpath`` and ``--workpath`` outside it."""
    for what, folder in folders.items():
        if folder and within(Path(folder), checkout):
            raise Refused(f"{what} {folder} is inside the checkout {checkout}: give --distpath and --workpath "
                          f"outside it")
