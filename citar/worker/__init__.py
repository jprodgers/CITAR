"""The CITAR worker agent.

Runs on a machine with a model and dials OUT to a CITAR server, so a GPU behind a home router can
serve games without anything at that end being reachable from the internet.

    python -m citar.worker --server https://citar.example.com --token CODE

    protocol    the wire format, shared with the server
    agent       the connection loop and request handling
"""
from . import protocol  # noqa: F401
