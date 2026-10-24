# PyInstaller build of the Windows bundle, from the installed wheel (design/phase5-release.md P5.6).
#
#   py -3.12 -m venv BUNDLE-VENV
#   BUNDLE-VENV\Scripts\python -m pip install --only-binary=:all: "<the Windows wheel>[all]" pyinstaller==<pinned>
#   cd <a folder outside the checkout>
#   BUNDLE-VENV\Scripts\python -P -m PyInstaller <absolute path to installer\citar.spec> --noconfirm ^
#       --distpath DIST --workpath WORK
#
# package.yml's windows job runs exactly that (PyInstaller's version is pinned there). The bundle is the wheel's code:
# `citar` and its engine, `citar._engine` (`_engine.pyd`, the ruleset compiled in), are collected from the venv's
# site-packages, never from the checkout. So PyInstaller runs with -P (no working directory on sys.path) from a folder
# outside the checkout, nothing here puts the checkout on sys.path or pathex (the entry scripts are read from
# installer/, which holds no `citar/`), and installer/spec_rules.py refuses to build unless `citar` and
# `citar._engine` resolve to the installed package, then fails unless the analysis took every module, extension and
# data file of `citar` from it, and every other file from a distribution of citar[all]'s closure (the ones
# PYTHON-LICENSES.txt lists), the Python it embeds, Windows or PyInstaller's run-time files: no build tool's package
# and no DLL from a folder on the build machine's PATH.
#
# Produces one folder, DIST/CITAR, containing two executables that share a single copy of Python and the libraries:
#
#   citar-play.exe  windowed. What the Start Menu shortcut runs: starts the server, opens the
#                   browser, and reports a failure in a dialog rather than a console nobody sees.
#   citar.exe       the ordinary command line, for `citar setup`, `citar doctor`, `citar bench`
#                   and everything else; and `citar.exe jobs-host`, the lab's job host, which both
#                   start (citar/jobs/host.py), since a frozen program takes no `-m`.
#
# and at its root the notices: LICENSE, NOTICE.md and THIRD-PARTY-LICENSES.txt (the wheel's), README.md, and
# PYTHON-LICENSES.txt, the licences of the Python runtime and packages the bundle ships (installer/python_licenses.py,
# written here from this venv).
#
# The launcher is not called CITAR.exe, which would read better, because Windows filesystems are
# case-insensitive: CITAR.exe and citar.exe are the same filename, and COLLECT silently writes one
# over the other, leaving a build with a missing executable and no error anywhere.
#
# One-folder rather than one-file, deliberately. A one-file build unpacks itself to a temporary
# directory on every launch, which costs seconds on a cold start, trips antivirus heuristics, and
# breaks any code that resolves a path relative to the executable. CITAR serves a web client and
# runs its migrations from disk, so it does the second of those constantly.
import importlib.util
import os
import sys
from pathlib import Path

from PyInstaller.utils.hooks import collect_data_files, collect_submodules

SPEC_DIR = Path(SPECPATH).resolve()
CHECKOUT = SPEC_DIR.parent


def _load(name):
    """A helper module of installer/, loaded by its path: installer/ is not put on sys.path."""
    spec = importlib.util.spec_from_file_location(f"citar_installer_{name}", SPEC_DIR / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


rules = _load("spec_rules")
python_licenses = _load("python_licenses")

rules.outside(CHECKOUT, **{"--distpath": DISTPATH, "--workpath": workpath})
PACKAGE = rules.installed_package(CHECKOUT, os.environ.get("CITAR_EXT_DIR"))
print(f"citar.spec: citar from {PACKAGE}")

# What the bundle may carry of the venv: citar and its citar[all] closure, the distributions PYTHON-LICENSES.txt
# lists (checked after the analysis, file by file, with PyInstaller's own run-time files).
try:
    DISTS = python_licenses.installed()
    SHIPPED = python_licenses.shipped(DISTS)
except python_licenses.Failed as e:
    raise rules.Refused(f"citar[all]'s closure: {e}") from None

# The DLLs a binary needs are looked for beside it, then on PATH: only the Python the bundle embeds and Windows' own
# folders, so that no tool's folder on the build machine's PATH (this laptop's JDK lent the first bundle 43 DLLs)
# decides what the bundle carries.
if os.name == "nt":
    os.environ["PATH"] = rules.dll_path(sys.base_prefix, os.environ.get("SystemRoot"))
    print(f"citar.spec: DLLs looked for on {os.environ['PATH']}")

# Everything CITAR reads at runtime that is not Python, all of the installed package's: the browser
# client, the Alembic migration environment, the hardware collectors offered on the Servers page, the
# lab's committed job costs (jobs/costs.json). `citar.paths` and the job store resolve these relative
# to the package, which is why they have to land beside it. Every data file the package has, not a
# list of them: 0.1.5's list missed jobs/costs.json once the package gained it, and the check below
# (spec_rules.package_data) now fails a bundle that leaves one out. The engine's stub (.pyi) is not
# needed at run time.
datas = collect_data_files("citar", excludes=["**/*.pyi"])

# The migration modules are .py files that nothing imports: Alembic discovers them by scanning the
# versions directory at runtime. collect_data_files skips .py by default - it is collecting *data* -
# so without include_py_files the bundle ships a migration environment with no migrations in it, and
# the first start of a fresh install creates an empty database and then fails to populate it.
datas += collect_data_files("citar", includes=["migrations/**/*.py"], include_py_files=True)

# Imports PyInstaller's static analysis cannot see.
hiddenimports = [
    # The whole of CITAR, rather than a list of the modules that are imported by name.
    #
    # Nearly every entry point is reached through a string: uvicorn is given
    # "citar.server.app:app", the `citar` command imports each sub-command by module path, Alembic
    # loads migrations by file, and providers are imported only when a seat uses one. A curated
    # list of those was wrong within an hour - `citar mcp` shipped in a build whose bundle had no
    # `citar.agents.mcp_server` in it, and nothing failed until somebody ran the command. The
    # package is a few megabytes of pure Python; collecting all of it is the cheap, correct answer.
    *collect_submodules("citar"),
    # The engine, named outright: everything above reaches it only through citar.engine_api.
    "citar._engine",
    # uvicorn resolves its protocol and loop implementations by name at runtime.
    *collect_submodules("uvicorn"),
    # SQLAlchemy picks a DBAPI from the URL scheme.
    "sqlalchemy.dialects.sqlite",
    # keyring finds its OS backend by entry point.
    "keyring.backends.Windows",
    "win32timezone",
    # Imported lazily, so static analysis does not see them and the bundle ships without them.
    # `mcp` is worth including because letting Claude Code take a seat is a headline feature of a
    # desktop install; `authlib` because a desktop copy can be pointed at server mode.
    # mcp.cli is excluded: it imports typer, which is an optional MCP extra CITAR does not use, and
    # collecting it fails the whole build with an error about a dependency nothing here needs.
    *collect_submodules("mcp", filter=lambda name: not name.startswith("mcp.cli")),
    *collect_submodules("authlib"),
]

excludes = [
    # Never used, and between them they add well over a hundred megabytes.
    "tkinter", "matplotlib", "numpy", "pandas", "scipy", "PIL", "pytest", "IPython",
    "notebook",
    # Build tools the venv holds, which the bundle must not carry: cffi's build-time shim (cffi._shimmed_dist_utils)
    # imports setuptools, which would bring setuptools, its vendored packages, packaging and PyInstaller's
    # pyi_rth_setuptools run-time hook into both executables, none of them in citar[all]'s closure or
    # PYTHON-LICENSES.txt. Nothing imports them at run time (cffi compiles nothing in the bundle).
    "setuptools", "pkg_resources", "_distutils_hack",
]

console_analysis = Analysis(
    [str(SPEC_DIR / "cli_entry.py")],
    pathex=[],
    binaries=[],
    datas=datas,
    hiddenimports=hiddenimports,
    hookspath=[],
    runtime_hooks=[],
    excludes=excludes,
    noarchive=False,
)

windowed_analysis = Analysis(
    [str(SPEC_DIR / "launcher.py")],
    pathex=[],
    binaries=[],
    datas=datas,
    hiddenimports=hiddenimports,
    hookspath=[],
    runtime_hooks=[],
    excludes=excludes,
    noarchive=False,
)

REQUIRED = rules.package_data(PACKAGE)
OWNED = rules.owners(DISTS.values())
SITE_PACKAGES = rules.venv_site_packages()
# Where the rest may come from: the Python the bundle embeds (its own site-packages excepted), Windows, and
# PyInstaller's work folder (base_library.zip); the entry scripts from installer/.
ROOTS = [sys.base_prefix, os.environ.get("SystemRoot") or "", workpath]
ucrt = set()
for label, analysis in (("citar.exe", console_analysis), ("citar-play.exe", windowed_analysis)):
    # The Universal CRT is Windows 10's own (the installer's minimum): see spec_rules.UCRT.
    analysis.binaries, dropped = rules.without_ucrt(analysis.binaries)
    ucrt.update(name for name, _, _ in dropped)
    problems = rules.check_analysis(analysis.pure, analysis.binaries, analysis.datas, PACKAGE, CHECKOUT, REQUIRED)
    problems += rules.check_sources(
        {"module": analysis.pure, "binary": analysis.binaries, "data file": analysis.datas},
        owned=OWNED, shipped=SHIPPED, site_packages=SITE_PACKAGES, roots=[r for r in ROOTS if r])
    problems += rules.check_sources({"script": analysis.scripts}, owned=OWNED, shipped=SHIPPED,
                                    site_packages=SITE_PACKAGES, roots=[str(SPEC_DIR)])
    if problems:
        raise rules.Refused(f"{label}'s analysis is not the installed wheel's and its closure's:\n  "
                            + "\n  ".join(problems))
print(f"citar.spec: every citar module, its engine and its {len(REQUIRED)} data files from {PACKAGE}")
print(f"citar.spec: every other file from citar[all]'s closure ({len(SHIPPED) - 1} distributions), the embedded "
      f"Python, Windows or PyInstaller's run-time files; {len(ucrt)} Universal CRT DLLs left out")

# Both executables are collected into one folder, which deduplicates the DLLs, the engine's
# extension and the web client, so there is a single copy of Python and the libraries on disk.
#
# PyInstaller's MERGE() is the documented way to share dependencies between executables, and it is
# not used here on purpose: it expects each executable to be COLLECTed into its own folder and to
# reach across to a sibling for the shared files, which is exactly what an installer should not
# ship. The cost of doing it this way is that each executable embeds its own archive of pure-Python
# modules - a few megabytes, against a folder that is already most of a Python runtime.

console_pyz = PYZ(console_analysis.pure, console_analysis.zipped_data)
windowed_pyz = PYZ(windowed_analysis.pure, windowed_analysis.zipped_data)

ICON = str(SPEC_DIR / "citar.ico")
icon_arg = ICON if Path(ICON).exists() else None

console_exe = EXE(
    console_pyz, console_analysis.scripts, [],
    exclude_binaries=True,
    name="citar",
    console=True,
    icon=icon_arg,
    disable_windowed_traceback=False,
)

windowed_exe = EXE(
    windowed_pyz, windowed_analysis.scripts, [],
    exclude_binaries=True,
    name="citar-play",
    console=False,
    icon=icon_arg,
    disable_windowed_traceback=False,
)

COLLECT(
    console_exe, console_analysis.binaries, console_analysis.datas,
    windowed_exe, windowed_analysis.binaries, windowed_analysis.datas,
    strip=False,
    upx=False,          # UPX-compressed binaries are a reliable way to be flagged by antivirus
    name=rules.BUNDLE,
)

# The notices, at the bundle's root beside the executables (PyInstaller puts datas under _internal/): the wheel's three
# and the README, and the Python licences written from this venv.
BUNDLE_DIR = Path(DISTPATH) / rules.BUNDLE
try:
    text, listed = python_licenses.build()
except python_licenses.Failed as e:
    raise rules.Refused(f"{rules.PYTHON_LICENSES}: {e}") from None
(BUNDLE_DIR / rules.PYTHON_LICENSES).write_text(text, encoding="utf-8", newline="\n")
placed = rules.place(BUNDLE_DIR, rules.notices(CHECKOUT))
print(f"citar.spec: {', '.join(placed)} and {rules.PYTHON_LICENSES} ({len(listed)} distributions) at {BUNDLE_DIR}")
