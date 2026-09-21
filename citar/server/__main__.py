"""Run the CITAR server.

    python -m citar.server                      # uses the environment, or local-mode defaults
    python -m citar.server --port 8080 --open   # command line wins over the environment

Command-line arguments override the environment; the environment overrides the built-in defaults.
That ordering matters for the deployed service, which is configured entirely through
/etc/citar/citar.env and passes no arguments at all — an earlier version hard-coded the argparse
defaults and so ignored CITAR_HOST and CITAR_PORT completely.
"""
import argparse
import webbrowser

import uvicorn

from .. import settings


def main():
    """Start the CITAR server, with the command line overriding the environment."""
    ap = argparse.ArgumentParser(description="CITAR game server")
    # The defaults are None so we can tell "not given" from "given the same value the default is",
    # and fall back to the settings layer only when an argument was genuinely omitted.
    ap.add_argument("--host", default=None)
    ap.add_argument("--port", type=int, default=None)
    ap.add_argument("--open", action="store_true", help="open the web client in your browser")
    ap.add_argument("--debug", action="store_true", help="enable developer debug endpoints")
    ap.add_argument("--log-level", default=None,
                    help="uvicorn log level (default: info in server mode, warning locally)")
    a = ap.parse_args()

    if a.debug:
        import os
        os.environ["CITAR_DEBUG"] = "1"

    try:
        cfg = settings.get()
    except settings.SettingsError as exc:
        # A misconfigured deployment should say so in one readable line, not a traceback.
        raise SystemExit(f"\nCITAR cannot start:\n\n{exc}\n")

    host = a.host or cfg.host
    port = a.port or cfg.port
    # Warnings only is right for a desktop app nobody is watching; a server needs its access log and,
    # more to the point, needs startup failures to be visible.
    level = a.log_level or ("info" if cfg.server_mode else "warning")

    if a.open:
        webbrowser.open(f"http://{host}:{port}/")

    uvicorn.run("citar.server.app:app", host=host, port=port, log_level=level,
                # Behind nginx, so the client address comes from X-Forwarded-For. CITAR reads that
                # header itself (honouring CITAR_TRUSTED_PROXY_HOPS) rather than letting uvicorn
                # rewrite it, because uvicorn trusts the leftmost entry, which a client can forge.
                proxy_headers=False,
                forwarded_allow_ips=None)


if __name__ == "__main__":
    main()
