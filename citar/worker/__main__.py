"""Run a CITAR worker.

    python -m citar.worker --server https://citar.example.com --token ABCD-EFGH-...

Settings can also come from the environment (CITAR_WORKER_SERVER, CITAR_WORKER_TOKEN,
CITAR_WORKER_BASE_URL …) or from a config file, so the token does not have to appear in a shell
history or a process list:

    python -m citar.worker --config worker.json

The standalone "CITAR helper" download is this same program. Started with nothing on the command line
(double-clicked, say), it asks for the server and token once, saves them to ``helper.json`` in the
per-user CITAR folder, and connects straight away on every later start.

A minimal worker.json:

    {
      "server_url": "https://citar.example.com",
      "token": "ABCD-EFGH-IJKL-MNOP",
      "base_url": "http://localhost:1234/v1",
      "max_concurrent": 1,
      "quiet_hours": [[0, 0, 360], [1, 0, 360]]
    }
"""
from __future__ import annotations

import argparse
import asyncio
import json
import logging
import os
import sys
from pathlib import Path

from .agent import Worker, WorkerConfig, install_signal_handlers


def _parse_quiet(values) -> list:
    """Accept "22:00-06:00" (every day) or "Mon 22:00-06:00" for a single weekday."""
    days = {"mon": 0, "tue": 1, "wed": 2, "thu": 3, "fri": 4, "sat": 5, "sun": 6}
    out = []
    for raw in values or []:
        if isinstance(raw, (list, tuple)):
            out.append(tuple(raw))
            continue
        text = str(raw).strip()
        weekdays = list(range(7))
        parts = text.split()
        if len(parts) == 2:
            key = parts[0][:3].lower()
            if key not in days:
                raise SystemExit(f"Unknown weekday in quiet hours: {parts[0]!r}")
            weekdays, text = [days[key]], parts[1]
        if "-" not in text:
            raise SystemExit(f"Quiet hours look like 22:00-06:00, not {raw!r}")
        start, end = text.split("-", 1)

        def minutes(value: str) -> int:
            """Parse ``HH:MM`` into minutes since midnight."""
            hour, _, minute = value.strip().partition(":")
            return int(hour) * 60 + int(minute or 0)

        s, e = minutes(start), minutes(end)
        for weekday in weekdays:
            # A window that wraps midnight is split, so the check is a simple containment test.
            if s < e:
                out.append((weekday, s, e))
            else:
                out.append((weekday, s, 1440))
                out.append(((weekday + 1) % 7, 0, e))
    return out


def saved_config_path() -> Path:
    """Where the helper keeps the server and token it was given on its first run."""
    from ..paths import _user_base
    return _user_base() / "helper.json"


def _first_run(data: dict) -> dict:
    """Ask for the server and token on a terminal, and offer to remember them.

    Only when nothing was given on the command line, in the environment or in a saved file, and only
    when there is someone at the keyboard to answer.
    """
    print("CITAR helper - first run")
    print("On your CITAR server, open Servers, pick this machine (or add it), and issue a worker token.\n")
    server = input("CITAR server address (e.g. https://citar.example.com): ").strip()
    token = input("Worker token: ").strip()
    if not server or not token:
        raise SystemExit("A server address and a token are both needed.")
    if not server.startswith(("http://", "https://")):
        server = "https://" + server
    data = dict(data, server_url=server, token=token)
    path = saved_config_path()
    if input(f"Remember these in {path}? [Y/n] ").strip().lower() in ("", "y", "yes"):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(data, indent=2), encoding="utf-8")
        try:
            path.chmod(0o600)
        except OSError:
            pass
        print(f"Saved. Delete {path} to forget them.\n")
    return data


def build_config(args) -> WorkerConfig:
    """Build the worker's configuration from arguments, a file and the environment."""
    data: dict = {}
    if args.config:
        path = Path(args.config)
        if not path.exists():
            raise SystemExit(f"No such config file: {path}")
        data = json.loads(path.read_text(encoding="utf-8"))
    elif saved_config_path().exists():
        data = json.loads(saved_config_path().read_text(encoding="utf-8"))
    given = args.server_url or args.token or os.environ.get("CITAR_WORKER_SERVER") or os.environ.get("CITAR_WORKER_TOKEN")
    if not given and not (data.get("server_url") and data.get("token")) and sys.stdin and sys.stdin.isatty():
        data = _first_run(data)

    def pick(name, env, default=None):
        """The first of an argument, a file value and an environment variable that is set."""
        value = getattr(args, name, None)
        if value not in (None, "", [], 0):
            return value
        if data.get(name) not in (None, ""):
            return data[name]
        return os.environ.get(env) or default

    server_url = pick("server_url", "CITAR_WORKER_SERVER")
    token = pick("token", "CITAR_WORKER_TOKEN")
    if not server_url:
        raise SystemExit("Give --server (or CITAR_WORKER_SERVER): the CITAR server to connect to.")
    if not token:
        raise SystemExit("Give --token (or CITAR_WORKER_TOKEN): issue one on the Servers page.")

    quiet = args.quiet or data.get("quiet_hours") or []
    return WorkerConfig(
        server_url=server_url,
        token=token,
        base_url=pick("base_url", "CITAR_WORKER_BASE_URL", "http://localhost:1234/v1"),
        provider=pick("provider", "CITAR_WORKER_PROVIDER", "lmstudio"),
        api_key_env=pick("api_key_env", "CITAR_WORKER_API_KEY_ENV", "") or "",
        max_concurrent=int(pick("max_concurrent", "CITAR_WORKER_MAX_CONCURRENT", 1) or 1),
        name=pick("name", "CITAR_WORKER_NAME", "") or "",
        quiet_hours=_parse_quiet(quiet),
        models=args.model or data.get("models") or None,
        insecure=bool(args.insecure or data.get("insecure")),
        collect_hardware=not args.no_hardware,
    )


def main(argv=None) -> int:
    """The ``citar-worker`` command line."""
    parser = argparse.ArgumentParser(
        prog="citar-worker", description="Serve a local model to a CITAR server.",
        epilog="The connection is outbound: nothing on this machine needs to be reachable from "
               "the internet.")
    parser.add_argument("--server", dest="server_url", help="e.g. https://citar.example.com")
    parser.add_argument("--token", help="worker token, issued on the Servers page")
    parser.add_argument("--config", help="JSON file with these settings (keeps the token out of "
                                         "your shell history)")
    parser.add_argument("--base-url", dest="base_url",
                        help="local model endpoint (default http://localhost:1234/v1)")
    parser.add_argument("--provider", choices=["lmstudio", "ollama", "openai_compatible", "openai"],
                        help="what is serving the model locally")
    parser.add_argument("--api-key-env", dest="api_key_env",
                        help="environment variable holding the API key, if this worker fronts a "
                             "paid API. The key is read here and never sent to the server.")
    parser.add_argument("--max-concurrent", dest="max_concurrent", type=int,
                        help="how many requests this machine will take at once (default 1)")
    parser.add_argument("--name", help="what to call this machine on the server")
    parser.add_argument("--model", action="append",
                        help="serve only this model (repeatable; default is everything found)")
    parser.add_argument("--quiet", action="append", metavar="WINDOW",
                        help='local quiet hours, e.g. "22:00-06:00" or "Sat 00:00-09:00". '
                             "Enforced here, so the server cannot override it. Repeatable.")
    parser.add_argument("--insecure", action="store_true",
                        help="skip TLS verification (test servers only)")
    parser.add_argument("--no-hardware", action="store_true",
                        help="do not report this machine's hardware to the server")
    parser.add_argument("-v", "--verbose", action="store_true")
    args = parser.parse_args(argv)

    logging.basicConfig(
        level=logging.DEBUG if args.verbose else logging.INFO,
        format="%(asctime)s %(levelname)-7s %(message)s", datefmt="%H:%M:%S")
    logging.getLogger("websockets").setLevel(logging.WARNING)
    logging.getLogger("httpx").setLevel(logging.WARNING)

    try:
        import websockets  # noqa: F401
    except ImportError:
        raise SystemExit("The worker needs the 'websockets' package:\n    pip install websockets")

    try:
        config = build_config(args)
    except SystemExit as exc:
        # a double-clicked helper would otherwise vanish before anyone could read why
        if getattr(sys, "frozen", False) and sys.stdin and sys.stdin.isatty():
            print(exc.code if isinstance(exc.code, str) else "")
            input("Press Enter to close.")
        raise
    worker = Worker(config)

    print(f"CITAR worker {config.name or ''}".rstrip())
    print(f"  server : {config.server_url}")
    print(f"  models : {config.base_url} ({config.provider})")
    print(f"  slots  : {config.max_concurrent}")
    if config.quiet_hours:
        print(f"  quiet  : {len(config.quiet_hours)} window(s), enforced locally")
    print()

    try:
        asyncio.run(_run(worker))
    except KeyboardInterrupt:
        print("\nstopped")
    return 0


async def _run(worker: Worker):
    """Run the worker until it is stopped."""
    loop = asyncio.get_running_loop()
    install_signal_handlers(worker, loop)
    try:
        await worker.run()
    except asyncio.CancelledError:
        pass


if __name__ == "__main__":
    sys.exit(main())
