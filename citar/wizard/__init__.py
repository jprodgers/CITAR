"""First-time setup: work out what this machine can do, then configure CITAR for it.

The wizard exists because CITAR spans two very different installations. Somebody who wants to play a
few games against a model on their own PC needs a model endpoint and nothing else — no domain, no
TLS, no accounts. Somebody standing up a public server needs all of that and will get a broken site
if any one piece is missing. Asking every question to everybody guarantees that both audiences
answer questions that do not apply to them, so the first question decides which of the three flows
runs and the rest follow from it:

``citar.wizard.local``    one person, this computer, models on localhost or an API key.
``citar.wizard.server``   a public deployment: domain, TLS, sign-in, the first administrator.
``citar.wizard.worker``   this machine's GPU serving models to a CITAR server somewhere else.

Two rules hold across all three:

**Nothing is written until the end.** Each flow collects a plan, shows it, and asks once. A wizard
that edits as it goes leaves a half-configured machine behind when somebody presses Ctrl-C.

**Every question has a non-interactive equivalent.** The installers (`install.sh`, the Windows
installer, the Docker image) run the same code with ``--non-interactive`` and flags, so there is one
implementation of "set CITAR up", not one per packaging format.
"""
from __future__ import annotations

from . import detect, prompts

__all__ = ["detect", "prompts"]
