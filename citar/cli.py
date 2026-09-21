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
    citar where               print every directory CITAR reads or writes

Sub-commands are thin: each one hands its remaining arguments to the module that already owned that
interface, so ``citar bench --all-models`` and ``python -m citar.bench --all-models`` are the same
program. ``python -m citar.server`` also still works, because the deployed systemd unit invokes it
that way and a packaging change should not require touching a running server.
"""
from __future__ import annotations

import argparse
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

    # Not a command: either an option for the top-level parser (``--version``, ``--help``) or a
    # mistake. argparse prints the right thing and exits in both cases.
    _build_parser().parse_args(argv)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
