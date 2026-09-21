# PyInstaller build for the Windows installer.
#
#   python -m PyInstaller installer/citar.spec --noconfirm
#
# Produces one folder, `dist/CITAR`, containing two executables that share a single copy of Python
# and the libraries:
#
#   citar-play.exe  windowed. What the Start Menu shortcut runs: starts the server, opens the
#                   browser, and reports a failure in a dialog rather than a console nobody sees.
#   citar.exe       the ordinary command line, for `citar setup`, `citar doctor`, `citar bench`
#                   and everything else.
#
# The launcher is not called CITAR.exe, which would read better, because Windows filesystems are
# case-insensitive: CITAR.exe and citar.exe are the same filename, and COLLECT silently writes one
# over the other, leaving a build with a missing executable and no error anywhere.
#
# One-folder rather than one-file, deliberately. A one-file build unpacks itself to a temporary
# directory on every launch, which costs seconds on a cold start, trips antivirus heuristics, and
# breaks any code that resolves a path relative to the executable. CITAR reads a ruleset and serves
# a web client from disk, so it does the second of those constantly.
import sys
from pathlib import Path

from PyInstaller.utils.hooks import collect_data_files, collect_submodules

SPEC_DIR = Path(SPECPATH).resolve()
ROOT = SPEC_DIR.parent
sys.path.insert(0, str(ROOT))

# Everything CITAR reads at runtime that is not Python: the ruleset JSON, the browser client, the
# Alembic migration environment, the hardware collectors offered on the Servers page. `citar.paths`
# resolves all of these relative to the package, which is why they have to land beside it here.
datas = collect_data_files("citar", includes=[
    "data/**/*.json",
    "data/**/*.md",
    "data/collectors/*",
    "web/**/*",
    "migrations/**/*.mako",
])

# The migration modules are .py files that nothing imports: Alembic discovers them by scanning the
# versions directory at runtime. collect_data_files skips .py by default - it is collecting *data* -
# so without include_py_files the wheel ships a migration environment with no migrations in it, and
# the first start of a fresh install creates an empty database and then fails to populate it.
datas += collect_data_files("citar", includes=["migrations/**/*.py"], include_py_files=True)
datas += [(str(ROOT / "LICENSE"), "."), (str(ROOT / "NOTICE.md"), "."),
          (str(ROOT / "README.md"), ".")]

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
    "notebook", "setuptools._distutils",
]

console_analysis = Analysis(
    [str(ROOT / "installer" / "cli_entry.py")],
    pathex=[str(ROOT)],
    binaries=[],
    datas=datas,
    hiddenimports=hiddenimports,
    hookspath=[],
    runtime_hooks=[],
    excludes=excludes,
    noarchive=False,
)

windowed_analysis = Analysis(
    [str(ROOT / "installer" / "launcher.py")],
    pathex=[str(ROOT)],
    binaries=[],
    datas=datas,
    hiddenimports=hiddenimports,
    hookspath=[],
    runtime_hooks=[],
    excludes=excludes,
    noarchive=False,
)

# Both executables are collected into one folder, which deduplicates the DLLs, the ruleset and the
# web client, so there is a single copy of Python and the libraries on disk.
#
# PyInstaller's MERGE() is the documented way to share dependencies between executables, and it is
# not used here on purpose: it expects each executable to be COLLECTed into its own folder and to
# reach across to a sibling for the shared files, which is exactly what an installer should not
# ship. The cost of doing it this way is that each executable embeds its own archive of pure-Python
# modules - a few megabytes, against a folder that is already most of a Python runtime.

console_pyz = PYZ(console_analysis.pure, console_analysis.zipped_data)
windowed_pyz = PYZ(windowed_analysis.pure, windowed_analysis.zipped_data)

ICON = str(ROOT / "installer" / "citar.ico")
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
    name="CITAR",
)
