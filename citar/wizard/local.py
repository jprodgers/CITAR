"""Set CITAR up for one person on one computer.

This is the flow behind "I just want to play a few games against a model". It has to end with
something playable, which in practice means one of four outcomes:

1. A model server that is already running on this machine is registered, and a game can start now.
2. An API key is stored in the OS credential store and a hosted model is registered.
3. Nothing is registered, and CITAR is set up for games against the scripted bots, which need no
   model at all — a perfectly good way to see what the game is before deciding whether to install a
   model runner.
4. The person is told exactly what to install, which model to pull for the GPU they actually have,
   and the one command to run afterwards.

Outcome 4 is the one most setup wizards get wrong by dead-ending at "no provider found". Somebody
who has never run a local model does not know that LM Studio exists, let alone which of the hundreds
of files on a model page fits in 6 GB of VRAM, so :mod:`citar.wizard.detect` works that out and this
module says it plainly.

Local mode needs no environment file, no TLS and no accounts: CITAR binds to loopback and signs the
operator in automatically. So the only thing written here is the server registry.
"""
from __future__ import annotations

from dataclasses import dataclass, field

from .. import servers as registry
from . import detect, prompts


@dataclass
class LocalPlan:
    """What the local flow decided to do, before anything is written."""

    endpoints: list[detect.Endpoint] = field(default_factory=list)
    api_provider: str = ""
    api_key: str = ""
    api_model: str = ""
    api_base_url: str = ""
    port: int = 8765
    bots_only: bool = False
    guidance: list[str] = field(default_factory=list)

    def summary(self) -> list[str]:
        """Everything this plan will do, as the lines shown before anything is written."""
        lines = []
        for endpoint in self.endpoints:
            lines.append(f"Register {endpoint.label} at {endpoint.base_url} "
                         f"({len(endpoint.models)} model(s))")
        if self.api_provider:
            lines.append(f"Register the {self.api_provider} API with model {self.api_model}")
            lines.append("Store the API key in this computer's credential store (never in the project)")
        if self.bots_only:
            lines.append("Register no model: games against the scripted bots")
        lines.append(f"Use port {self.port}")
        return lines


def run(args) -> int:
    """Ask the local-setup questions, then apply the answers. Returns an exit status."""
    prompts.heading("This computer")
    for line in detect.describe_hardware():
        prompts.note(line)

    prompts.say("\nLooking for model servers on this machine...")
    found = detect.find_endpoints()
    plan = LocalPlan(port=args.port or detect.first_free_port())

    if found:
        _choose_from_found(plan, found, args)
    else:
        prompts.note("None found.")
        _nothing_found(plan, args)

    if not (plan.endpoints or plan.api_provider or plan.bots_only):
        prompts.say("\nNothing to configure. Run `citar setup` again when you have a model ready.")
        return 1

    if not prompts.confirm_plan("CITAR will:", plan.summary()):
        prompts.say("Nothing was changed.")
        return 1

    _apply(plan)
    _finish(plan, args)
    return 0


# --------------------------------------------------------------------------- questions

def _choose_from_found(plan: LocalPlan, found: list[detect.Endpoint], args) -> None:
    """Offer the model servers that are already running."""
    prompts.say("")
    for endpoint in found:
        prompts.note(endpoint.summary)

    if len(found) == 1 and prompts.ask_yes_no(f"\nUse {found[0].label}?", True):
        plan.endpoints = [found[0]]
        return

    options = [(str(i), f"{e.label} - {e.base_url}", e.summary) for i, e in enumerate(found)]
    options.append(("all", "All of them", "Register every one; pick per seat when you start a game."))
    options.append(("none", "None - something else", "An API key, or scripted bots only."))
    choice = prompts.ask_choice("Which should CITAR use?", options, default="0")

    if choice == "all":
        plan.endpoints = list(found)
    elif choice == "none":
        _nothing_found(plan, args)
    else:
        plan.endpoints = [found[int(choice)]]


def _nothing_found(plan: LocalPlan, args) -> None:
    """No local model server is running: offer the three ways forward."""
    model, reason = detect.suggest_model()
    vram = detect.usable_vram_gb()

    choice = prompts.ask_choice(
        "How would you like to play?",
        [
            ("install", "Run models on this computer",
             "Free and private, and the reason CITAR exists.\n"
             "I will tell you what to install and which model to start with."),
            ("api", "Use a hosted model (Anthropic, or any OpenAI-compatible API)",
             "Strongest play, no GPU needed. You pay the provider per token."),
            ("bots", "Against the built-in scripted bots for now",
             "No model at all. The game is fully playable this way, and you can add a model later."),
        ],
        default="install")

    if choice == "install":
        plan.guidance = _install_guidance(model, reason, vram)
        for line in plan.guidance:
            prompts.say(line)
        if prompts.ask_yes_no("\nHave you started it? Look again", False):
            found = detect.find_endpoints()
            if found:
                plan.endpoints = found
                prompts.note(f"Found {found[0].label}.")
                return
            prompts.note("Still nothing. CITAR will set up for bot games; add the model later "
                         "on the Servers page.")
        plan.bots_only = True
    elif choice == "api":
        _ask_api_key(plan, args)
    else:
        plan.bots_only = True


def _install_guidance(model: str, reason: str, vram: float) -> list[str]:
    """The exact steps for this machine. Printed, and also written to the notes file."""
    where = f"{vram:g} GB of usable VRAM" if vram else "no dedicated GPU that I could detect"
    return [
        "",
        f"This machine has {where}.",
        "",
        "  1. Install a model runner. Either works with CITAR:",
        "       LM Studio   https://lmstudio.ai      - a GUI, easiest if you have not done this before",
        "       Ollama      https://ollama.com       - a command-line tool",
        "",
        f"  2. Download a model that fits. For this machine: {model}",
        f"     {reason}",
        "",
        "       In LM Studio:  search for it, download, then turn on the local server (the",
        "                      Developer tab). It listens on http://localhost:1234.",
        f"       With Ollama:   ollama pull {model}",
        "",
        "  3. Run `citar setup` again. It will find the running server by itself.",
        "",
        "  Anything OpenAI-compatible works too - llama.cpp, vLLM, text-generation-webui.",
        "  Add it on the Servers page with its base URL.",
    ]


def _ask_api_key(plan: LocalPlan, args) -> None:
    """Collect a hosted-API key, and store it where it will not end up in the project folder."""
    provider = prompts.ask_choice(
        "Which API?",
        [("anthropic", "Anthropic (Claude)", "claude-opus-5, claude-sonnet-5 and the rest."),
         ("openai_compatible", "Any OpenAI-compatible API",
          "OpenAI, OpenRouter, Together, Groq, or anything else that speaks the same protocol.")],
        default="anthropic")
    plan.api_provider = provider

    if provider == "anthropic":
        plan.api_model = prompts.ask("Model", args.model or "claude-sonnet-5")
    else:
        plan.api_base_url = prompts.ask("Base URL", args.base_url or "https://api.openai.com/v1")
        plan.api_model = prompts.ask("Model", args.model or "gpt-4o-mini")

    prompts.note("The key is stored in this computer's credential store (Windows Credential "
                 "Manager, macOS Keychain, or the Linux Secret Service).")
    prompts.note("It is never written into the project folder, a save file or a report.")
    plan.api_key = prompts.ask_secret("API key", flag="--api-key")


# --------------------------------------------------------------------------- apply

def _apply(plan: LocalPlan) -> None:
    """Write the registry entries. The first thing written in the whole flow."""
    host_set = registry.load().get("host_server_id")

    for endpoint in plan.endpoints:
        server = registry.default_server(kind="owned", provider=endpoint.provider)
        server["name"] = f"{endpoint.label} (this computer)"
        server["description"] = "Added by `citar setup`."
        server["connection"]["base_url"] = endpoint.base_url
        server["connection"]["manage_loading"] = endpoint.provider == "lmstudio"
        server["models"] = [registry.default_model(key, endpoint.provider)
                            for key in endpoint.models if not _is_embedding(key)]
        server["hardware"] = detect.hardware() or {"source": "none"}
        # The machine running CITAR is also the machine running the engine and the bots, so it is
        # the one whose CPU time and power get attributed to a game.
        server["is_host"] = not host_set
        saved = registry.upsert(server)
        host_set = host_set or saved["id"]

    if plan.api_provider:
        server = registry.default_server(kind="api", provider=plan.api_provider)
        server["name"] = "Anthropic API" if plan.api_provider == "anthropic" else "Hosted API"
        server["description"] = "Added by `citar setup`."
        server["connection"]["base_url"] = plan.api_base_url
        server["connection"]["key"] = {"backend": "keyring", "env": ""}
        server["models"] = [registry.default_model(plan.api_model, plan.api_provider)]
        saved = registry.upsert(server)
        if plan.api_key:
            _store_key(saved["id"], plan.api_key)


def _is_embedding(model_key: str) -> bool:
    """Embedding models cannot play a turn, so they are filtered out of the catalogue.

    They turn up in every LM Studio and Ollama listing, and a seat configured with one fails in a
    way that reads as a CITAR bug rather than a wrong choice.
    """
    lowered = model_key.lower()
    return any(marker in lowered for marker in ("embed", "-rerank", "bge-", "e5-", "gte-"))


def _store_key(server_id: str, key: str) -> None:
    """Put the API key in the OS credential store, and say so if that is not possible."""
    from .. import keystore

    try:
        keystore.store(server_id, "keyring", key)
        prompts.note("Key stored in the OS credential store.")
    except Exception as exc:
        prompts.note(f"Could not reach the credential store ({type(exc).__name__}: {exc}).")
        prompts.note("Set it on the Servers page instead, or put it in an environment variable "
                     "and point the server's key backend at that variable.")


def _finish(plan: LocalPlan, args) -> None:
    """Say what happened and, if asked, start the server."""
    from .. import paths

    prompts.heading("Done")
    prompts.note(f"Settings:   {paths.config_path('servers.json')}")
    prompts.note(f"Saved games: {paths.saves_path()}")
    prompts.say("")
    prompts.note(f"Start CITAR with:   citar serve --port {plan.port}")
    prompts.note(f"Then open:          http://127.0.0.1:{plan.port}/")
    if plan.bots_only:
        prompts.say("")
        prompts.note("No model is configured. Create a game with scripted-bot seats to play now,")
        prompts.note("and add a model later on the Servers page.")

    if args.start or (not prompts.NON_INTERACTIVE and prompts.ask_yes_no("\nStart CITAR now?", True)):
        from ..server.__main__ import main as serve

        import sys
        saved_argv = sys.argv
        sys.argv = ["citar serve", "--port", str(plan.port), "--open"]
        try:
            serve()
        finally:
            sys.argv = saved_argv
