"""Connect this machine's models to a CITAR server somewhere else.

The worker agent exists because the machine with the GPU is usually the machine you cannot expose:
a desktop behind a home router, with no static address and no port forwarding. So the connection is
made the other way round — the worker dials out to the server over WebSocket, authenticates with a
token, and then serves model requests down that same connection. Nothing needs to be open at the
house.

This flow collects the three things the agent needs (server, token, which local endpoint to serve),
proves they work before writing anything, and offers to install the agent as a service so it comes
back after a reboot.

The token is a credential for somebody else's server, so it is treated like one: prompted without
echo, written to a file only the owner can read, and never passed on a command line where it would
land in the shell history and the process list.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

from . import detect, prompts


@dataclass
class WorkerPlan:
    """What the worker flow decided."""

    server_url: str = ""
    token: str = ""
    provider: str = "lmstudio"
    base_url: str = "http://localhost:1234/v1"
    name: str = ""
    max_concurrent: int = 1
    quiet_hours: list = field(default_factory=list)
    config_path: Path = Path()
    install_service: bool = False

    def summary(self) -> list[str]:
        """Everything this plan will do, shown before anything is written."""
        lines = [
            f"Serve models from {self.base_url} ({self.provider})",
            f"To {self.server_url}",
            f"As '{self.name}', up to {self.max_concurrent} request(s) at a time",
            f"Write {self.config_path} (mode 0600 - it holds the token)",
        ]
        if self.quiet_hours:
            lines.append(f"Stay idle during {len(self.quiet_hours)} quiet window(s)")
        if self.install_service:
            lines.append("Install a service so the worker starts with the machine")
        return lines


def run(args) -> int:
    """Ask the worker questions, verify them, then write the configuration."""
    from .. import paths

    plan = WorkerPlan(config_path=paths.config_path("worker.json"))

    prompts.heading("Which CITAR server?")
    prompts.note("The address people use in a browser. The worker dials out to it, so this machine")
    prompts.note("needs no open ports and no forwarding.")
    plan.server_url = prompts.ask("Server URL", args.server or "https://citar.example.com",
                                  flag="--server").rstrip("/")

    prompts.say("")
    prompts.note("The token comes from the server's operator: they run")
    prompts.note("  citar admin add-server --name 'your machine'")
    prompts.note("and send you the code it prints.")
    plan.token = args.token or prompts.ask_secret("Worker token", flag="--token")

    prompts.heading("Which models?")
    found = detect.find_endpoints()
    if found:
        for endpoint in found:
            prompts.note(endpoint.summary)
        chosen = found[0]
        if len(found) > 1:
            options = [(str(i), f"{e.label} - {e.base_url}", e.summary) for i, e in enumerate(found)]
            chosen = found[int(prompts.ask_choice("Serve which one?", options, default="0"))]
        plan.provider, plan.base_url = chosen.provider, chosen.base_url
    else:
        prompts.note("No model server found on this machine.")
        prompts.note("Start LM Studio or Ollama first, or give the endpoint by hand.")
        plan.base_url = prompts.ask("Base URL", args.base_url or "http://localhost:1234/v1")
        plan.provider = prompts.ask_choice(
            "Which kind?",
            [("lmstudio", "LM Studio", "CITAR can load and unload models on it."),
             ("ollama", "Ollama", "Models are loaded on demand by Ollama."),
             ("openai_compatible", "Something else OpenAI-compatible",
              "llama.cpp, vLLM, text-generation-webui and the like.")],
            default="lmstudio")

    import socket

    plan.name = prompts.ask("A name for this machine", args.name or socket.gethostname())
    plan.max_concurrent = int(prompts.ask("How many requests at once?", str(args.max_concurrent or 1)))

    prompts.say("")
    prompts.note("Quiet hours stop this machine taking work during a window - useful when the GPU")
    prompts.note("is in a room somebody sleeps in. Leave blank for none.")
    quiet = prompts.ask("Quiet hours, e.g. 21:00-06:00", "")
    if quiet:
        plan.quiet_hours = [quiet]

    if sys.platform != "win32" and _is_root() and shutil.which("systemctl"):
        plan.install_service = prompts.ask_yes_no("\nStart the worker automatically at boot?", True)

    _verify(plan)

    if not prompts.confirm_plan("CITAR will:", plan.summary()):
        prompts.say("Nothing was changed.")
        return 1

    _write(plan)
    if plan.install_service:
        _install_service(plan)
    _finish(plan)
    return 0


def _verify(plan: WorkerPlan) -> None:
    """Check the endpoint and the server before writing a configuration that cannot work."""
    prompts.say("")
    endpoint = detect.probe(plan.provider, "the model endpoint", plan.base_url)
    if endpoint.reachable:
        prompts.note(f"Model endpoint: {len(endpoint.models)} model(s) available.")
    else:
        prompts.note(f"Model endpoint at {plan.base_url} did not answer. The worker will keep "
                     "retrying, so this is fine if you have not started it yet.")
    try:
        import httpx

        response = httpx.get(f"{plan.server_url}/api/auth/config", timeout=5.0)
        prompts.note(f"CITAR server: reachable (HTTP {response.status_code}).")
    except Exception as exc:
        prompts.note(f"Could not reach {plan.server_url} ({type(exc).__name__}). "
                     "Check the URL; the token is not tested until the worker connects.")


def _write(plan: WorkerPlan) -> None:
    """Write worker.json, readable only by its owner because it holds the token."""
    config = {
        "server_url": plan.server_url,
        "token": plan.token,
        "provider": plan.provider,
        "base_url": plan.base_url,
        "name": plan.name,
        "max_concurrent": plan.max_concurrent,
    }
    if plan.quiet_hours:
        config["quiet_hours"] = plan.quiet_hours

    plan.config_path.parent.mkdir(parents=True, exist_ok=True)
    descriptor = os.open(plan.config_path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as handle:
        json.dump(config, handle, indent=2)
        handle.write("\n")
    prompts.note(f"Wrote {plan.config_path} (0600)")


def _install_service(plan: WorkerPlan) -> None:
    """A systemd unit for the worker.

    ``Restart=always`` rather than ``on-failure``: a worker whose home connection drops should come
    back when it returns, and a clean exit from a dropped WebSocket is not a failure.
    """
    executable = shutil.which("citar-worker") or f"{sys.executable} -m citar.worker"
    unit = f"""[Unit]
Description=CITAR worker - serves this machine's models to {plan.server_url}
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={executable} --config {plan.config_path}
Restart=always
RestartSec=15
NoNewPrivileges=yes

[Install]
WantedBy=multi-user.target
"""
    path = Path("/etc/systemd/system/citar-worker.service")
    path.write_text(unit, encoding="utf-8", newline="\n")
    subprocess.run(["systemctl", "daemon-reload"], check=False)
    subprocess.run(["systemctl", "enable", "--now", "citar-worker"], check=False)
    prompts.note(f"Wrote {path} and started the service.")


def _finish(plan: WorkerPlan) -> None:
    """Print what was done and how to start the worker."""
    prompts.heading("Done")
    if plan.install_service:
        prompts.note("systemctl status citar-worker      is it running")
        prompts.note("journalctl -u citar-worker -f      what it is doing")
    else:
        prompts.note("Start the worker with:")
        prompts.note(f"  citar-worker --config {plan.config_path}")
    prompts.say("")
    prompts.note(f"It will appear on the Servers page of {plan.server_url} once it connects.")


def _is_root() -> bool:
    """Whether this is running as root."""
    return hasattr(os, "geteuid") and os.geteuid() == 0
