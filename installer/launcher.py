"""The double-click entry point for the Windows build.

Somebody who installed CITAR from a downloaded installer has no terminal, and if the program fails
they see a window that appears and vanishes. So this launcher exists alongside the ordinary console
``citar.exe`` with one job: start the server, open the browser, and make sure that whatever happens
is *visible* — in a log file that keeps growing across runs, and in a message box when the failure
is fatal.

It is deliberately thin. Everything it does is available from the command line as
``citar serve --open``; the only things added here are the crash reporting and picking a port that
is actually free, which matters when the usual failure is "I already had CITAR running".
"""
from __future__ import annotations

import os
import socket
import sys
import threading
import time
import traceback
import webbrowser
from pathlib import Path

#: Ports tried in order. 8765 first so the documented URL is usually right.
PORTS = (8765, 8766, 8767, 8768, 8769)


def log_path() -> Path:
    """Where the launcher writes what happened. Beside the user's other CITAR state."""
    base = Path(os.environ.get("LOCALAPPDATA") or Path.home() / "AppData" / "Local") / "CITAR"
    base.mkdir(parents=True, exist_ok=True)
    return base / "launcher.log"


def log(message: str) -> None:
    try:
        with open(log_path(), "a", encoding="utf-8") as handle:
            handle.write(f"{time.strftime('%Y-%m-%d %H:%M:%S')} {message}\n")
    except OSError:
        pass                                                    # never fail because logging failed


def message_box(title: str, text: str) -> None:
    """Show a native dialog, because there is no console to print to.

    Falls back to standard error if even that fails — in a windowed build nobody will see it, but
    the log file has the same text, and failing silently here would hide the original problem
    behind a second one.
    """
    try:
        import ctypes

        ctypes.windll.user32.MessageBoxW(None, text, title, 0x10)  # MB_ICONERROR
    except Exception:
        print(f"{title}: {text}", file=sys.stderr)


def free_port() -> int:
    """The first port nothing is listening on.

    A second copy of CITAR is the most likely reason the usual port is taken, and refusing to start
    would be the wrong answer: two games at once is a reasonable thing to want.
    """
    for port in PORTS:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
            probe.settimeout(0.3)
            if probe.connect_ex(("127.0.0.1", port)) != 0:
                return port
    return PORTS[0]


def open_when_ready(url: str, timeout: float = 30.0) -> None:
    """Open the browser once the server answers, not before.

    Opening immediately shows a connection error for the second or two the server takes to start,
    which reads as "it is broken" rather than "it is starting".
    """
    deadline = time.time() + timeout
    while time.time() < deadline:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
            probe.settimeout(0.3)
            if probe.connect_ex(("127.0.0.1", int(url.rsplit(":", 1)[1].rstrip("/")))) == 0:
                webbrowser.open(url)
                log(f"opened {url}")
                return
        time.sleep(0.25)
    log(f"server did not answer within {timeout:g}s; opening {url} anyway")
    webbrowser.open(url)


def main() -> int:
    log("launcher starting")
    try:
        port = free_port()
        url = f"http://127.0.0.1:{port}/"

        # Local mode is the whole point of this build: loopback only, one operator, signed in
        # automatically. Set here rather than left to chance so a stray environment variable from
        # some other program cannot put a desktop install into server mode.
        os.environ.setdefault("CITAR_MODE", "local")
        os.environ["CITAR_HOST"] = "127.0.0.1"
        os.environ["CITAR_PORT"] = str(port)

        from citar import __version__

        log(f"CITAR {__version__} on port {port}")

        threading.Thread(target=open_when_ready, args=(url,), daemon=True).start()

        import uvicorn

        uvicorn.run("citar.server.app:app", host="127.0.0.1", port=port, log_level="warning",
                    proxy_headers=False, forwarded_allow_ips=None)
        log("server stopped")
        return 0
    except SystemExit:
        raise
    except BaseException as exc:
        detail = traceback.format_exc()
        log(f"FAILED: {detail}")
        message_box(
            "CITAR could not start",
            f"{type(exc).__name__}: {exc}\n\n"
            f"The full details are in:\n{log_path()}\n\n"
            "If you report this, please include that file.")
        return 1


if __name__ == "__main__":
    sys.exit(main())
