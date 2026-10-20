"""What ``installer/citar.spec`` holds the Windows bundle to (design/phase5-release.md P5.6), apart from it so the
tests run it without PyInstaller.

The bundle is built **from the installed wheel, never the checkout**: PyInstaller runs in a venv with the Windows
wheel installed (``[all]``), as ``python -P -m PyInstaller <absolute path to citar.spec>`` from a folder outside the
checkout. ``-m`` from the checkout's root, a ``CITAR_EXT_DIR`` (the dev loop's extension folder, which
``citar/__init__.py`` puts first) or a checkout on ``sys.path`` would each let the analysis take the checkout's
``citar/`` or another build's ``_engine.pyd``; so the spec refuses to build unless ``citar`` and ``citar._engine``
resolve to the installed package, and fails after the analysis unless every module, extension and data file it took
of ``citar`` is that package's.

The bundle ships **nothing ``PYTHON-LICENSES.txt`` does not cover**. Every file the analysis takes (modules,
extensions and other binaries, data, the scripts the executables run) is held to where it came from
(:func:`check_sources`): a file under the venv's site-packages must belong, by the installed distributions' ``RECORD``,
to ``citar`` or its ``citar[all]`` closure (``installer/python_licenses.py``'s), or be one of the run-time files of
PyInstaller and its community hooks that the executables carry (:data:`RUNTIME_FILES`); anything else must come from
the Python the bundle embeds (``sys.base_prefix``, its own site-packages excepted), Windows' folder or PyInstaller's
work folder. A build tool's package the analysis reached (setuptools, through cffi's build-time shim, was one), or a
DLL found in some folder on the build machine's ``PATH`` (a JDK's copies of the Universal CRT were), fails the build
naming it. The analysis searches only the venv, the embedded Python and Windows for DLLs (:func:`dll_path`), and the
Universal CRT's DLLs are never bundled (:func:`without_ucrt`): the installer requires Windows 10, where the system's
UCRT is always used and an application's own copy ignored. :func:`audit` holds a built bundle to the same rule, from
its files and its executables' archives (``python -P installer/spec_rules.py audit DIST/CITAR``, run in the venv by
``scripts/ci/check_bundle.py``).

The notices go at the bundle's root, beside ``citar.exe``: ``LICENSE``, ``NOTICE.md`` and ``THIRD-PARTY-LICENSES.txt``
from the installed wheel's metadata (what the wheel carries; the checkout's ``packaging/`` only for a wheel without
the third), ``README.md`` from the checkout, and ``PYTHON-LICENSES.txt`` (``installer/python_licenses.py``).
"""
from __future__ import annotations

import argparse
import importlib.metadata as md
import importlib.util
import os
import re
import shutil
import sys
import sysconfig
from pathlib import Path, PurePath, PurePosixPath
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


#: What the analysis takes as code (modules and the extension), not as data; a stub (``.pyi``) is never needed.
CODE_SUFFIXES = {".py", ".pyc", ".pyo", ".pyd", ".so", ".pyi"}
#: Python files that are data: Alembic reads the migrations by file, and nothing imports them.
DATA_CODE = ("migrations/",)


def package_data(package: Path) -> set:
    """Every file of the installed package the bundle must carry as data, by its path in the package: all but code
    (the migrations' ``.py`` files are data) and byte-code caches. ``citar.paths`` and the job store read them beside
    the package (the web client, the migrations, the hardware collectors, ``jobs/costs.json``)."""
    out = set()
    for path in package.rglob("*"):
        if not path.is_file() or "__pycache__" in path.parts:
            continue
        rel = path.relative_to(package).as_posix()
        if path.suffix.lower() not in CODE_SUFFIXES or (rel.startswith(DATA_CODE) and path.suffix == ".py"):
            out.add(rel)
    return out


def check_analysis(pure: Iterable, binaries: Iterable, datas: Iterable, package: Path, checkout: Path,
                   required: Iterable = ()) -> list:
    """What is wrong with an analysis's TOCs (``(name, source, kind)`` entries): a ``citar`` module, extension or data
    file whose source is not the installed ``package`` (or is the checkout's), no engine extension, and a file of
    ``required`` (:func:`package_data`'s paths) not among its data. Empty when all is well."""
    datas = list(datas)
    problems = []
    taken = {"/".join(_parts(name)[1:]) for name, _, _ in datas if _parts(name)[:1] == ("citar",)}
    for rel in sorted(set(required) - taken):
        problems.append(f"the package's data file {rel} is not in the bundle")
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


# ---- what the bundle may carry: citar[all]'s closure, the embedded Python, Windows, PyInstaller's run-time ----

#: The Universal CRT's DLLs: its API-set forwarders and ucrtbase.dll. PyInstaller collects them from wherever it finds
#: them first on ``PATH`` (this laptop's build took 43 from a JDK's folder), for Windows before 10. On Windows 10 and
#: later the UCRT is part of the system, which is always used and an application's own copy ignored (Microsoft's
#: "Universal CRT deployment"; PyInstaller's depend/dylib.py says the same), and the installer requires Windows 10
#: (citar.iss's MinVersion), so the bundle carries none.
UCRT = re.compile(r"^(api-ms-win-[a-z0-9-]+|ucrtbase)\.dll$", re.I)
#: The files of PyInstaller's own distributions that the executables carry, under the licences PYTHON-LICENSES.txt
#: prints for them: PyInstaller's loader (the bootloader's GPL with its exception) and its run-time hooks and modules
#: (Apache-2.0), as its COPYING.txt names these folders; the community hooks' run-time hooks (Apache-2.0, its
#: LICENSE). Nothing else of either is shipped.
RUNTIME_FILES = {"pyinstaller": ("PyInstaller/loader/", "PyInstaller/hooks/rthooks/", "PyInstaller/fake-modules/"),
                 "pyinstaller-hooks-contrib": ("_pyinstaller_hooks_contrib/rthooks/",)}


def _canonical(name: str) -> str:
    return re.sub(r"[-_.]+", "-", name).lower()


def _key(path) -> str:
    """A path as files are compared here: absolute, case-folded on Windows (not resolved: cheap for thousands)."""
    return os.path.normcase(os.path.abspath(str(path)))


def _under(key: str, folder: str) -> bool:
    return key == folder or key.startswith(folder.rstrip("\\/") + os.sep)


def without_ucrt(binaries: Iterable) -> tuple:
    """``(kept, dropped)``: an analysis's binaries less the Universal CRT's DLLs (:data:`UCRT`, by the file's name)."""
    kept, dropped = [], []
    for entry in binaries:
        (dropped if UCRT.match(_parts(entry[0])[-1]) else kept).append(entry)
    return kept, dropped


def dll_path(base: str, system_root: Optional[str]) -> str:
    """The ``PATH`` the analysis searches for the DLLs a binary needs (PyInstaller looks beside the binary, then on
    ``PATH``): the Python the bundle embeds (its folder and its ``DLLs``) and Windows' own folders, so that no folder
    of the build machine's (a JDK's, Git's, a runner image's tools) lends the bundle a DLL."""
    folders = [base, os.path.join(base, "DLLs")]
    if system_root:
        folders += [os.path.join(system_root, "System32"), system_root]
    return os.pathsep.join(dict.fromkeys(folders))


def venv_site_packages() -> list:
    """This interpreter's own site-packages folders (a venv's), where the wheel and its closure are installed."""
    paths = sysconfig.get_paths()
    return sorted({paths["purelib"], paths["platlib"]})


def owners(dists: Iterable) -> dict:
    """``{a file's path (:func:`_key`): (its distribution's canonical name, its path in the install folder)}`` for
    every file the installed distributions' ``RECORD`` files name."""
    out: dict = {}
    for dist in dists:
        name = _canonical(dist.metadata["Name"] or "")
        for file in dist.files or ():
            out.setdefault(_key(dist.locate_file(file)), (name, PurePosixPath(str(file).replace("\\", "/")).as_posix()))
    return out


def _allowed(owner: tuple, shipped: set, runtime: dict) -> bool:
    dist, rel = owner
    return dist in shipped or rel.startswith(runtime.get(dist, ()))


def check_sources(tocs: dict, *, owned: dict, shipped: Iterable, site_packages: Iterable, roots: Iterable,
                  runtime: Optional[dict] = None) -> list:
    """What is wrong with where an analysis's files come from (``tocs``: ``{what they are: [(name, source, kind)]}``):
    a file of the venv's ``site_packages`` that no installed distribution's RECORD names, or whose distribution is
    neither ``shipped`` (``citar`` and its closure, which PYTHON-LICENSES.txt lists) nor the file one of ``runtime``'s
    (:data:`RUNTIME_FILES`); a file of another site-packages; a file outside ``site_packages`` and ``roots`` (the
    embedded Python, Windows, the work folder), such as a DLL found in some folder on the build machine's PATH. Empty
    when all is well. A distribution that must not be shipped is named once, with how many of its files and a few."""
    shipped, runtime = set(shipped), RUNTIME_FILES if runtime is None else runtime
    packages = [_key(p) for p in site_packages]
    roots = [_key(r) for r in roots]
    foreign: dict = {}                                      # distribution -> names
    outside: dict = {}                                      # (what, folder) -> names
    problems = []
    for what, entries in tocs.items():
        for name, source, _ in entries:
            if not source or source == "-":
                continue                                    # a namespace package: no file
            key = _key(source)
            if any(_under(key, p) for p in packages):
                owner = owned.get(key)
                if owner is None:
                    problems.append(f"the {what} {name} comes from {source}, which no installed distribution's "
                                    f"RECORD names")
                elif not _allowed(owner, shipped, runtime):
                    foreign.setdefault(owner[0], []).append(name)
            elif "site-packages" in (part.lower() for part in PurePath(key).parts):
                problems.append(f"the {what} {name} comes from {source}, a site-packages that is not the venv's")
            elif not any(_under(key, r) for r in roots):
                outside.setdefault((what, os.path.dirname(str(source))), []).append(name)
    for dist, names in sorted(foreign.items()):
        problems.append(f"{len(names)} file(s) of {dist}, which PYTHON-LICENSES.txt does not list (it is not in "
                        f"citar[all]'s closure), e.g. {', '.join(sorted(names)[:3])}: exclude it in citar.spec")
    for (what, folder), names in sorted(outside.items()):
        problems.append(f"{len(names)} {what}(s) from {folder}, outside the venv, the Python the bundle embeds and "
                        f"Windows (a folder on the build machine's PATH?), e.g. {', '.join(sorted(names)[:3])}")
    return problems


def audit(bundle: Path, modules: Iterable, *, owned: dict, shipped: Iterable, site_packages: Iterable,
          runtime: Optional[dict] = None) -> tuple:
    """``(problems, {distribution: how many of its files and modules the bundle carries})`` for a built bundle, read
    from the bundle itself: each file under ``_internal/`` that is, by its path, a file of the venv's site-packages,
    and each module of ``modules`` (the executables' archives' entries) whose source is (``a.b``: ``a/b.py`` or
    ``a/b/__init__.py``), held to :func:`check_sources`'s rule; and no Universal CRT DLL anywhere in it."""
    shipped, runtime = set(shipped), RUNTIME_FILES if runtime is None else runtime
    folders = [Path(p) for p in site_packages]
    counts: dict = {}
    foreign: dict = {}
    problems = []

    def owner_of(rel: str) -> Optional[tuple]:
        for folder in folders:
            owner = owned.get(_key(folder / rel))
            if owner is not None:
                return owner
        return None

    def count(owner: tuple, what: str) -> None:
        if _allowed(owner, shipped, runtime):
            counts[owner[0]] = counts.get(owner[0], 0) + 1
        else:
            foreign.setdefault(owner[0], []).append(what)

    internal = bundle / "_internal"
    ucrt = []
    for path in sorted(bundle.rglob("*")):
        if not path.is_file():
            continue
        if UCRT.match(path.name):
            ucrt.append(path.relative_to(bundle).as_posix())
        elif internal in path.parents:
            rel = path.relative_to(internal).as_posix()
            owner = owner_of(rel)
            if owner is not None:
                count(owner, rel)
    for module in sorted(set(modules)):
        parts = module.split(".")
        for rel in ("/".join(parts) + ".py", "/".join([*parts, "__init__.py"])):
            owner = owner_of(rel)
            if owner is not None:
                count(owner, module)
                break
    if ucrt:
        problems.append(f"the bundle holds {len(ucrt)} Universal CRT DLL(s), e.g. {', '.join(ucrt[:3])}")
    for dist, what in sorted(foreign.items()):
        problems.append(f"the bundle carries {len(what)} file(s) or module(s) of {dist}, which PYTHON-LICENSES.txt "
                        f"does not list, e.g. {', '.join(what[:3])}")
    return problems, counts


def _python_licenses():
    """installer/python_licenses.py, beside this file, loaded by its path (installer/ is not on sys.path)."""
    spec = importlib.util.spec_from_file_location("citar_installer_python_licenses_for_audit",
                                                  Path(__file__).resolve().with_name("python_licenses.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main(argv: Optional[list] = None) -> int:
    ap = argparse.ArgumentParser(prog="spec_rules.py", description="The Windows bundle's rules (citar.spec's).")
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("audit", help="hold a built bundle to the rule; run with -P in the venv it was built in")
    p.add_argument("bundle", type=Path, help="the bundle's folder (DIST/CITAR)")
    args = ap.parse_args(argv)
    licences = _python_licenses()
    try:
        dists = licences.installed()
        shipped = licences.shipped(dists)
    except licences.Failed as e:
        print(f"spec_rules audit: {e}", file=sys.stderr)
        return 1
    from PyInstaller.archive.readers import pkg_archive_contents

    executables = sorted(p for p in args.bundle.iterdir() if p.suffix.lower() == ".exe")
    if not executables:
        print(f"spec_rules audit: no executable in {args.bundle}", file=sys.stderr)
        return 1
    modules = [name for exe in executables for name in pkg_archive_contents(str(exe))]
    problems, counts = audit(args.bundle, modules, owned=owners(md.distributions()), shipped=shipped,
                             site_packages=venv_site_packages())
    for line in problems:
        print(f"spec_rules audit: {line}", file=sys.stderr)
    if problems:
        return 1
    print(f"spec_rules audit: {args.bundle}: {', '.join(e.name for e in executables)}' archives and _internal hold "
          f"{sum(counts.values()):,} files and modules of {len(counts)} distributions, each citar, its citar[all] "
          f"closure or PyInstaller's run-time files, which PYTHON-LICENSES.txt lists; no Universal CRT DLL")
    print("  " + ", ".join(f"{name} {n}" for name, n in sorted(counts.items())))
    return 0


if __name__ == "__main__":
    sys.exit(main())
