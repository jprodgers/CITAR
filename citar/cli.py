"""The ``citar`` command.

One entry point in front of everything CITAR can do, so that an installed copy needs no knowledge of
the module layout::

    citar                     start the server and open the browser
    citar serve               the same, without opening a browser
    citar setup               interactive first-time configuration
    citar doctor              check this installation and print what it found
    citar admin …             operator commands (accounts, invitations, worker tokens)
    citar worker …            run the worker agent that serves local models to a remote CITAR
    citar mcp …               the MCP bridge, for Claude Code and other MCP clients
    citar bench …             command-line model benchmark
    citar sim …               one headless bot-vs-bot game in the console
    citar balance …           parallel bot games, for balancing the scripted bot
    citar lab …               the long-running bot experiment runner
    citar ruleset check DIR   check a modded ruleset before playing it (CITAR_RULESET_DIR)
    citar where               print every directory CITAR reads or writes

Sub-commands are thin: each one hands its remaining arguments to the module that already owned that
interface, so ``citar bench --all-models`` and ``python -m citar.bench --all-models`` are the same
program. ``python -m citar`` is ``citar``. ``python -m citar.server`` also still works, because the
deployed systemd unit invokes it that way and a packaging change should not require touching a
running server.
"""
from __future__ import annotations

import argparse
import os
import sys
from typing import Callable, Optional, Sequence

from . import __version__

#: Sub-commands that delegate: name -> (import path, attribute, one-line help).
DELEGATES = {
    "admin": ("citar.server.admin_cli", "main", "accounts, invitations, policies and worker tokens"),
    "worker": ("citar.worker.__main__", "main", "serve this machine's models to a remote CITAR server"),
    "mcp": ("citar.agents.mcp_server", "main", "MCP bridge for an external AI client"),
    "bench": ("citar.bench", "main", "benchmark models from the command line"),
    "sim": ("citar.sim", "main", "play one headless bot-vs-bot game"),
    "balance": ("citar.balance", "main", "run many bot games and report on balance"),
    "lab": ("citar.lab", "main", "queue and run long bot experiments"),
}


def _delegate(module_path: str, attr: str, prog: str, argv: Sequence[str]) -> int:
    """Call another module's ``main`` with *argv*, as though it had been invoked directly.

    The delegated mains read ``sys.argv`` (they predate this wrapper and are also used as
    ``python -m`` entry points), so it is rewritten for the duration of the call. ``prog`` becomes
    the name in their ``--help`` output, which is why it reads ``citar bench`` and not ``bench``.
    """
    import importlib

    module = importlib.import_module(module_path)
    main: Callable = getattr(module, attr)
    saved = sys.argv
    sys.argv = [prog, *argv]
    try:
        result = main()
    except KeyboardInterrupt:
        return 130
    finally:
        sys.argv = saved
    return int(result) if isinstance(result, int) else 0


def _serve(argv: Sequence[str], open_browser: bool) -> int:
    """Start the game server. ``--open`` is added for the bare ``citar`` invocation."""
    args = list(argv)
    if open_browser and "--open" not in args and "--no-open" not in args:
        args.append("--open")
    args = [a for a in args if a != "--no-open"]
    return _delegate("citar.server.__main__", "main", "citar serve", args)


def _where() -> int:
    """Print every resolved directory. The first thing to check when files appear in odd places."""
    from . import paths

    width = max(len(k) for k in paths.describe())
    for key, value in paths.describe().items():
        print(f"{key:<{width}}  {value}")
    return 0


#: The variable naming a modded ruleset's directory, which the engine reads when it loads (docs/MODDING.md).
RULESET_DIR = "CITAR_RULESET_DIR"


def _ruleset_parser() -> argparse.ArgumentParser:
    """``citar ruleset``'s own arguments."""
    parser = argparse.ArgumentParser(
        prog="citar ruleset",
        description="Check a ruleset directory (the data layout: ruleset/, custom/, game.json) as the engine would "
                    f"load it from {RULESET_DIR}, without playing it. Exit 0 when it loads, 1 when it does not, 2 "
                    "when the directory cannot be read.",
    )
    sub = parser.add_subparsers(dest="action", metavar="<action>", required=True)
    check = sub.add_parser("check", help="load a ruleset directory and print every problem found")
    check.add_argument("directory", help="a copy of crates/citar-engine/data with your changes")
    return parser


def _ruleset(argv: Sequence[str]) -> int:
    """``citar ruleset check DIR``: what the engine makes of a ruleset directory, every problem with its file, object
    and kind. The check reads DIR whatever ``CITAR_RULESET_DIR`` says: the variable is dropped before the engine
    loads, so a broken directory named there cannot stop the check of a fixed one."""
    args = _ruleset_parser().parse_args(list(argv))
    os.environ.pop(RULESET_DIR, None)
    from . import engine_api

    try:
        report = engine_api.check_ruleset(args.directory)
    except OSError as exc:
        print(f"{args.directory}: {exc}", file=sys.stderr)
        return 2
    errors = report["errors"]
    if not errors:
        counts = ", ".join(f"{n} {kind}" for kind, n in report["counts"].items())
        print(f"{report['dir']}: the ruleset loads: ruleset {report['id'][:12]} (version {report['version']}), "
              f"{counts}.")
        print(f"To play it, set {RULESET_DIR}={report['dir']} and start CITAR again. Its games, saves and bot "
              "fingerprints carry its ruleset id, so they are told apart from the shipped ruleset's.")
        return 0
    print(f"{report['dir']}: the ruleset does not load: {len(errors)} problem{'s' if len(errors) > 1 else ''}")
    for e in errors:
        where = f"{e['file']}: {e['object']}" if e["object"] else e["file"]
        print(f"  - {where}: {e['text']} [{e['kind']}]")
    print("The loader checks in stages (the files, their fields, the references, the uniques, the filters, ...) and "
          "stops after the first stage that finds a problem: fix these and check again.")
    return 1


def _build_parser() -> argparse.ArgumentParser:
    """The top-level argument parser, used for ``--help`` and for rejecting unknown commands."""
    parser = argparse.ArgumentParser(
        prog="citar",
        description="CITAR - Civ Inspired Tool for AI Research.",
        epilog="Run `citar <command> --help` for a command's own options. "
               "With no command at all, CITAR starts the server and opens your browser.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--version", action="version", version=f"CITAR {__version__}")
    subparsers = parser.add_subparsers(dest="command", metavar="<command>")

    serve = subparsers.add_parser("serve", help="start the game server", add_help=False)
    serve.add_argument("rest", nargs=argparse.REMAINDER)

    subparsers.add_parser("setup", help="interactive first-time configuration", add_help=False)
    subparsers.add_parser("doctor", help="check this installation", add_help=False)
    subparsers.add_parser("where", help="print the directories CITAR uses")
    ruleset = subparsers.add_parser("ruleset", help="check a modded ruleset directory", add_help=False)
    ruleset.add_argument("rest", nargs=argparse.REMAINDER)

    for name, (_, _, help_text) in DELEGATES.items():
        sub = subparsers.add_parser(name, help=help_text, add_help=False)
        sub.add_argument("rest", nargs=argparse.REMAINDER)

    return parser


def main(argv: Optional[Sequence[str]] = None) -> int:
    """Dispatch a ``citar`` command line. Returns the process exit status."""
    argv = list(sys.argv[1:] if argv is None else argv)

    # No arguments is the home-user path: start the server and open the lobby. Anything that looks
    # like an option (``citar --version``) still goes through argparse.
    if not argv:
        return _serve([], open_browser=True)

    command, rest = argv[0], argv[1:]

    if command in DELEGATES:
        module_path, attr, _ = DELEGATES[command]
        return _delegate(module_path, attr, f"citar {command}", rest)
    if command == "serve":
        return _serve(rest, open_browser=False)
    if command == "setup":
        from .wizard import cli as wizard_cli

        return wizard_cli.main(rest)
    if command == "doctor":
        from .doctor import main as doctor_main

        return doctor_main(rest)
    if command == "where":
        return _where()
    if command == "ruleset":
        return _ruleset(rest)

    # Not a command: either an option for the top-level parser (``--version``, ``--help``) or a
    # mistake. argparse prints the right thing and exits in both cases.
    _build_parser().parse_args(argv)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
