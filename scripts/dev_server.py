"""Run a throwaway CITAR instance for development, isolated from the real one.

The point is to be able to exercise accounts, invites and the email flows without touching the
saves, the server registry or the database that the working instance uses — and without stopping a
benchmark run that may have been going for hours.

    python scripts/dev_server.py                  local mode, auto-login, port 8799
    python scripts/dev_server.py --server         server mode with a real login screen
    python scripts/dev_server.py --reset          start from an empty database

Everything it writes goes under a scratch directory, and mail is printed to the terminal instead of
being sent, so verification and password-reset links can be followed by pasting them in.
"""
from __future__ import annotations

import argparse
import os
import shutil
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_DIR = Path(tempfile.gettempdir()) / "citar-dev"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--port", type=int, default=8799)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--dir", default=str(DEFAULT_DIR), help="where this instance keeps its state")
    parser.add_argument("--server", action="store_true",
                        help="server mode: real login screen, no auto-login")
    parser.add_argument("--reset", action="store_true", help="delete the instance's state first")
    parser.add_argument("--registration", default="open", choices=["open", "invite", "closed"])
    args = parser.parse_args()

    base = Path(args.dir)
    if args.reset and base.exists():
        shutil.rmtree(base, ignore_errors=True)
        print(f"Cleared {base}")
    (base / "saves").mkdir(parents=True, exist_ok=True)
    (base / "config").mkdir(parents=True, exist_ok=True)

    os.environ["CITAR_DATA_DIR"] = str(base)
    os.environ["CITAR_SAVE_DIR"] = str(base / "saves")
    os.environ["CITAR_CONFIG_DIR"] = str(base / "config")
    os.environ["CITAR_DB_URL"] = "sqlite:///" + (base / "citar.db").as_posix()
    os.environ["CITAR_HOST"] = args.host
    os.environ["CITAR_PORT"] = str(args.port)
    os.environ["CITAR_REGISTRATION"] = args.registration
    os.environ.setdefault("CITAR_DEBUG", "1")

    if args.server:
        os.environ["CITAR_MODE"] = "server"
        # Browsers are opened at localhost, and an Origin of http://localhost:8800 does not match
        # http://127.0.0.1:8800 — same machine, different origin, and every write would be refused.
        host_for_origin = "localhost" if args.host in ("127.0.0.1", "0.0.0.0") else args.host
        os.environ["CITAR_PUBLIC_ORIGIN"] = f"http://{host_for_origin}:{args.port}"
        # A development box has no TLS; relaxing this is exactly what the flag is for, and server
        # mode refuses to start otherwise.
        os.environ["CITAR_REQUIRE_HTTPS"] = "0"
        os.environ["CITAR_BEHIND_PROXY"] = "0"
        os.environ.setdefault("CITAR_SECRET_KEY", "dev-only-secret-key-not-for-any-real-deployment")
        # Server mode refuses the log transport, so point at a sink that is obviously not real.
        os.environ.setdefault("CITAR_SMTP_HOST", "localhost")
        os.environ.setdefault("CITAR_MAIL_FROM", "citar@localhost")
    else:
        os.environ["CITAR_MODE"] = "local"
        os.environ["CITAR_MAIL_TRANSPORT"] = "log"     # links printed to the terminal

    sys.path.insert(0, str(ROOT))
    print(f"CITAR dev instance\n  state : {base}\n  mode  : {os.environ['CITAR_MODE']}"
          f"\n  url   : http://{args.host}:{args.port}\n", flush=True)

    import uvicorn
    uvicorn.run("citar.server.app:app", host=args.host, port=args.port, log_level="info")
    return 0


if __name__ == "__main__":
    sys.exit(main())
