"""Where the CITAR helper is: what the commands that ran the Python worker say since 0.1.6.

The Python worker, which served a machine's models to a CITAR server over protocol 1, was replaced by the CITAR helper
(``citar-helper``, crates/citar-helper), a Rust binary that speaks protocol 2; a 0.1.6 server refuses the old worker
(design/phase4-helper.md P4.2.8). ``citar worker``, ``citar setup --worker`` and the install scripts' worker modes now
print where to download the helper and exit 2 (argparse's code for a command line that cannot be served). Downloading
it for the user is Phase 5's (the install scripts and the package managers).
"""
from __future__ import annotations

import sys
from typing import Optional, Sequence

#: Where every published helper is (the Servers page of a server links the build for each platform).
RELEASES = "https://github.com/jprodgers/CITAR/releases/latest"

MESSAGE = (
    "Since CITAR 0.1.6, a machine serves its models (and lends its CPU) through the CITAR helper, a single program "
    "that replaced the Python worker; a 0.1.6 server refuses the old worker.\n"
    "Download the helper for this machine from the Servers page of your CITAR server (it links the build for each "
    f"platform), or from {RELEASES}, then run\n"
    "    citar-helper setup\n"
    "with a worker token issued on that Servers page. It reads the old worker's settings file (helper.json), so "
    "its endpoint, models and quiet hours carry over.")


def main(argv: Optional[Sequence[str]] = None) -> int:
    """Print where to download the helper; exit status 2."""
    print(MESSAGE, file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
