"""What can this machine do? — the questions the wizard answers before asking any.

Setup goes badly when the software makes the person describe their own computer. Everything here is
something CITAR can find out for itself: which model servers are already running, what the GPU is,
how much of a model will fit in it. A wizard that opens with "I found LM Studio with 6 models" is a
different experience from one that opens with "enter your base URL".

Nothing in this module changes anything, and every probe has a short timeout: a model server that is
not running should cost the wizard a fraction of a second, not a stall on a dead TCP port.
"""
from __future__ import annotations

import socket
from dataclasses import dataclass, field
from typing import Optional

#: Local endpoints worth probing, in the order they are offered. Each entry is
#: ``(provider, label, base URL, the page to send somebody who has not installed it)``.
KNOWN_ENDPOINTS = [
    ("lmstudio", "LM Studio", "http://localhost:1234/v1", "https://lmstudio.ai"),
    ("ollama", "Ollama", "http://localhost:11434/v1", "https://ollama.com"),
    ("openai_compatible", "llama.cpp server", "http://localhost:8080/v1", "https://github.com/ggml-org/llama.cpp"),
    ("openai_compatible", "vLLM", "http://localhost:8000/v1", "https://docs.vllm.ai"),
    ("openai_compatible", "Jan", "http://localhost:1337/v1", "https://jan.ai"),
    ("openai_compatible", "text-generation-webui", "http://localhost:5000/v1",
     "https://github.com/oobabooga/text-generation-webui"),
]

#: Model suggestions by usable VRAM, in GB. The sizes are for a 4-bit quantisation at a context
#: length CITAR can actually use — a briefing plus a turn's tool calls runs to a few thousand
#: tokens, so anything under about 8k of context is not worth suggesting.
VRAM_SUGGESTIONS = [
    (0, 4, "qwen3-1.7b", "Small enough for almost anything, including CPU-only. Expect weak play."),
    (4, 7, "gemma-3-4b-it", "Fits a 6 GB laptop GPU at 32k context. The lower bound for a full game."),
    (7, 11, "qwen3-8b", "A good balance on an 8 GB card. Plays a coherent early game."),
    (11, 17, "qwen3-14b", "Comfortable on 12-16 GB. Noticeably better long-term planning."),
    (17, 25, "gemma-3-27b-it", "For 24 GB cards. Among the strongest that fits on one consumer GPU."),
    (25, 10_000, "qwen3-32b", "Plenty of room. Raise the context length rather than the model size."),
]


@dataclass
class Endpoint:
    """A model server found (or not found) on this machine."""

    provider: str
    label: str
    base_url: str
    install_url: str
    reachable: bool = False
    models: list[str] = field(default_factory=list)
    error: str = ""

    @property
    def summary(self) -> str:
        """This endpoint in one line, as the wizard shows it."""
        if not self.reachable:
            return f"{self.label}: not running"
        if not self.models:
            return f"{self.label}: running, but no model is loaded"
        head = ", ".join(self.models[:3])
        more = f" (+{len(self.models) - 3} more)" if len(self.models) > 3 else ""
        return f"{self.label}: {len(self.models)} model(s) - {head}{more}"


def port_open(host: str, port: int, timeout: float = 0.35) -> bool:
    """Is something listening? Checked first, because a closed port answers instantly.

    An HTTP request to a closed port takes as long as the connect timeout, and the wizard probes
    half a dozen of them. Testing the socket first keeps detection under a second in the normal
    case, where none of them are running.
    """
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True
    except OSError:
        return False


def _split(base_url: str) -> tuple[str, int]:
    """The host and port of a base URL."""
    from urllib.parse import urlparse

    parsed = urlparse(base_url)
    return parsed.hostname or "localhost", parsed.port or (443 if parsed.scheme == "https" else 80)


def probe(provider: str, label: str, base_url: str, install_url: str = "",
          timeout: float = 2.0) -> Endpoint:
    """Ask one endpoint what models it has. Never raises."""
    endpoint = Endpoint(provider, label, base_url, install_url)
    host, port = _split(base_url)
    if not port_open(host, port):
        return endpoint
    try:
        import httpx

        response = httpx.get(base_url.rstrip("/") + "/models", timeout=timeout)
        if response.status_code >= 400:
            endpoint.error = f"HTTP {response.status_code}"
            return endpoint
        payload = response.json() or {}
        endpoint.reachable = True
        endpoint.models = [m.get("id", "") for m in payload.get("data", []) if m.get("id")]
    except Exception as exc:
        endpoint.error = f"{type(exc).__name__}: {exc}"
    return endpoint


def find_endpoints(include_unreachable: bool = False) -> list[Endpoint]:
    """Probe every well-known local model server.

    Duplicates are collapsed by base URL: Ollama's OpenAI-compatible endpoint and LM Studio can be
    configured onto the same port, and offering the same URL twice under two names would be a
    confusing way to start.
    """
    from concurrent.futures import ThreadPoolExecutor

    targets = []
    seen: set[str] = set()
    for provider, label, base_url, install_url in KNOWN_ENDPOINTS:
        if base_url in seen:
            continue
        seen.add(base_url)
        targets.append((provider, label, base_url, install_url))

    # Probed in parallel. Serially this is the slowest thing the wizard does, because a model
    # server that is running can take a couple of seconds to list its catalogue and the wizard
    # would pay that for each one in turn.
    with ThreadPoolExecutor(max_workers=len(targets)) as pool:
        results = list(pool.map(lambda t: probe(*t), targets))
    return [e for e in results if e.reachable or include_unreachable]


def hardware() -> dict:
    """This machine's CPU, RAM and GPUs, via the same collector the Servers page uses."""
    from .. import hwinfo

    try:
        return hwinfo.collect()
    except Exception:
        return {}


def usable_vram_gb(info: Optional[dict] = None) -> float:
    """The largest single GPU's VRAM, in GB.

    The largest *single* card, not the total: a model has to fit in one of them unless the runtime
    is set up to split it, which is not something to assume during first-time setup. Integrated
    graphics are skipped — their "VRAM" is a slice of system RAM and produces a suggestion the
    machine cannot honour.
    """
    info = hardware() if info is None else info
    best = 0.0
    for gpu in info.get("gpus") or []:
        vendor = (gpu.get("vendor") or "").lower()
        name = (gpu.get("name") or "").lower()
        if "intel" in vendor and "arc" not in name:
            continue                                            # integrated; not a target
        if gpu.get("vram_gb"):
            best = max(best, float(gpu["vram_gb"]))
    if best:
        return best
    # Apple silicon shares memory between CPU and GPU, and roughly two-thirds of it can be used for
    # a model before the system starts swapping.
    memory = info.get("memory") or {}
    if memory.get("unified") and memory.get("ram_gb"):
        return round(float(memory["ram_gb"]) * 0.66, 1)
    return 0.0


def suggest_model(vram_gb: Optional[float] = None) -> tuple[str, str]:
    """A model to download, and why, for the VRAM this machine has.

    Returns ``(name, reason)``. These are starting points rather than recommendations: the Models
    page ranks what actually plays well once there are games to compare.
    """
    vram = usable_vram_gb() if vram_gb is None else vram_gb
    for low, high, name, reason in VRAM_SUGGESTIONS:
        if low <= vram < high:
            return name, reason
    return VRAM_SUGGESTIONS[0][2], VRAM_SUGGESTIONS[0][3]


def describe_hardware(info: Optional[dict] = None) -> list[str]:
    """Human-readable lines about this machine, for the wizard's opening screen."""
    info = hardware() if info is None else info
    lines = []
    cpu = info.get("cpu") or {}
    if cpu.get("model"):
        threads = cpu.get("threads")
        lines.append(f"CPU: {cpu['model']}" + (f" ({threads} threads)" if threads else ""))
    memory = info.get("memory") or {}
    if memory.get("ram_gb"):
        kind = "Unified memory" if memory.get("unified") else "RAM"
        lines.append(f"{kind}: {memory['ram_gb']} GB")
    for gpu in info.get("gpus") or []:
        vram = f", {gpu['vram_gb']} GB VRAM" if gpu.get("vram_gb") else ""
        lines.append(f"GPU: {gpu.get('name', 'unknown')}{vram}")
    if not lines:
        lines.append("Could not read this machine's hardware; that only affects suggestions.")
    return lines


def port_free(port: int, host: str = "127.0.0.1") -> bool:
    """Can CITAR bind here? Used to pick a default port that will actually start."""
    return not port_open(host, port, timeout=0.2)


def first_free_port(start: int = 8765, tries: int = 20) -> int:
    """The first free port at or after *start*, so a second CITAR does not collide with the first."""
    for offset in range(tries):
        if port_free(start + offset):
            return start + offset
    return start
