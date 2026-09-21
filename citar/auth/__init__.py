"""Accounts, sessions and permissions for CITAR.

    accounts    creating accounts, naming them, proving who owns them
    sessions    the cookie, its lifetime, CSRF and origin checks
    passwords   argon2id hashing and the strength rules
    tokens      random bearer tokens, stored only as hashes
    oauth       Google, GitHub, Discord and Microsoft sign-in
    invites     invite codes, and the roles they can confer
    policy      runtime server policy and per-role capabilities
    ratelimit   limits on the routes reachable without an account
    captcha     hCaptcha verification for open signup
    mailer      verification, reset and invitation email
    audit       the append-only record of anything consequential
    deps        FastAPI dependencies: who is calling, and may they do this
    access      one function that answers "what may this viewer do with this object"

See docs/ACCOUNTS.md for how these fit together.
"""
from __future__ import annotations

from . import (accounts, audit, captcha, deps, invites, mailer, oauth, passwords, policy,
               ratelimit, sessions, tokens)

__all__ = ["accounts", "audit", "captcha", "deps", "invites", "mailer", "oauth", "passwords",
           "policy", "ratelimit", "sessions", "tokens"]
