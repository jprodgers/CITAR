"""``citar setup --worker``: since 0.1.6 a pointer to the CITAR helper.

This flow used to configure the Python worker (``worker.json``) and install its systemd unit. The worker was replaced
by the CITAR helper (``citar-helper``), which has its own first-run setup (``citar-helper setup``) and its own service
installation (``citar-helper service install``), so this says where to download it and exits 2
(``citar.helper_pointer``; design/phase4-helper.md P4.2.8).
"""
from __future__ import annotations

from .. import helper_pointer


def run(args) -> int:
    """Print where to download the helper; exit status 2."""
    return helper_pointer.main()
