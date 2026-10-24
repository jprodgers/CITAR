"""``citar setup`` — pick a flow and run it.

The first question is which of the three installations this is, because every later question
depends on the answer. Getting that wrong is cheap: nothing is written until the end of a flow, and
the flows can be re-run as often as you like — the local one adds to the server registry rather than
replacing it, and the server one rewrites a configuration file whose contents it prints first.

Every flow can also run unattended, which is how the installers use it::

    citar setup --local --non-interactive
    citar setup --server --domain citar.example.com --non-interactive

In that mode a question with no answer supplied is an error naming the flag that would have answered
it, rather than a process blocking on a terminal nobody is watching.

``citar setup --worker`` configured the Python worker, which the CITAR helper replaced in 0.1.6: it now
says where to download the helper (whose own ``citar-helper setup`` asks its questions) and exits 2.
"""
from __future__ import annotations

import argparse
import sys
from typing import Optional, Sequence

from . import prompts

BANNER = r"""
   ____ ___ _____  _    ____
  / ___|_ _|_   _|/ \  |  _ \    Civ Inspired Tool for AI Research
 | |    | |  | | / _ \ | |_) |
 | |___ | |  | |/ ___ \|  _ <    A Civilization V-style game whose
  \____|___| |_/_/   \_\_| \_\   players can be language models.
"""


def _build_parser() -> argparse.ArgumentParser:
    """The setup wizard's argument parser, including the non-interactive equivalents."""
    parser = argparse.ArgumentParser(
        prog="citar setup",
        description="Configure CITAR for this machine.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__.split("Every flow")[-1].strip())

    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--local", action="store_true",
                      help="one person, this computer (the default)")
    mode.add_argument("--server", dest="server_mode", action="store_true",
                      help="a public server other people sign in to")
    mode.add_argument("--worker", action="store_true",
                      help="replaced by the CITAR helper (citar-helper): where to get it")

    parser.add_argument("--non-interactive", action="store_true",
                        help="never prompt; fail if an answer is missing")
    parser.add_argument("--port", type=int, help="the port CITAR listens on")

    local = parser.add_argument_group("local")
    local.add_argument("--model", help="model to register")
    local.add_argument("--base-url", help="an OpenAI-compatible endpoint to register")
    local.add_argument("--api-key", help="API key (prefer the prompt; this lands in shell history)")
    local.add_argument("--start", action="store_true", help="start CITAR when setup finishes")

    remote = parser.add_argument_group("server")
    remote.add_argument("--domain", help="the domain browsers will use")
    remote.add_argument("--host", help="bind address (default 127.0.0.1)")
    remote.add_argument("--proxy-hops", type=int, help="how many reverse proxies are in front")
    remote.add_argument("--state-dir", help="where saves, the database and runs are kept")
    remote.add_argument("--env-path", help="where to write the environment file")
    remote.add_argument("--refresh-units", action="store_true",
                        help="write the installed service's unit, citar-admin and backup again from this release, "
                             "keeping their paths; ask nothing, start nothing (install.sh's switch runs it)")

    return parser


def _choose_mode(args) -> str:
    """Which flow to run: an explicit flag, or the question that opens the wizard."""
    if args.server_mode:
        return "server"
    if args.worker:
        return "worker"
    if args.local:
        return "local"
    if prompts.NON_INTERACTIVE:
        return "local"

    return prompts.ask_choice(
        "What are you setting up?",
        [
            ("local", "CITAR on this computer, for me",
             "Play against models running here or against a hosted API.\n"
             "No accounts, no domain, no certificates - it binds to this machine only."),
            ("server", "A server other people sign in to",
             "A public deployment: a domain, TLS, accounts, invitations and budgets.\n"
             "Needs a machine with a name on the internet."),
            ("worker", "This machine's GPU, serving a CITAR server elsewhere",
             "That is the CITAR helper's job, a separate download: this says where to get it."),
        ],
        default="local")


def main(argv: Optional[Sequence[str]] = None) -> int:
    """Run the setup wizard. Returns the process exit status.

    ``argv`` of ``None`` means "read the command line", which is what a direct
    ``python -m citar.wizard.cli`` does; ``citar setup`` passes its remaining arguments explicitly.
    An empty list is not the same as ``None`` - it is a bare ``citar setup``, which asks everything.
    """
    args = _build_parser().parse_args(sys.argv[1:] if argv is None else list(argv))
    prompts.NON_INTERACTIVE = args.non_interactive

    if not prompts.NON_INTERACTIVE:
        print(BANNER)

    try:
        mode = _choose_mode(args)
        if mode == "server":
            from . import server as flow
        elif mode == "worker":
            from . import worker as flow
        else:
            from . import local as flow
        return flow.run(args)
    except prompts.Cancelled as stop:
        print(f"\n{stop}")
        return 130
    except prompts.NeedsAnswer as missing:
        print(f"\nerror: {missing}")
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
