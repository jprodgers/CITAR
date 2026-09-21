"""Bearer tokens: generation, hashing and constant-time comparison.

Every long-lived secret CITAR hands out — session cookies, email links, worker credentials — follows
the same rule: the random value goes to the holder once, and only its SHA-256 hash is stored. A
stolen database backup then yields no usable credential.

SHA-256 rather than argon2 is deliberate here. These tokens are 256 bits of CSPRNG output, so there
is no dictionary to attack and nothing for a slow hash to buy; and they are verified on every single
request, where a deliberately slow hash would be a denial-of-service lever pointed at ourselves.
Passwords are the opposite case and use argon2id — see passwords.py.
"""
from __future__ import annotations

import hashlib
import hmac
import secrets
from typing import Tuple

#: 32 bytes of entropy, URL-safe. Long enough that guessing is not a threat model.
TOKEN_BYTES = 32


def generate() -> str:
    """A new random token."""
    return secrets.token_urlsafe(TOKEN_BYTES)


def hash_token(token: str) -> str:
    """The stored form of a token.

    Tokens are stored hashed for the same reason passwords are: a database that leaks should not hand
    over working credentials.
    """
    return hashlib.sha256(token.encode("utf-8")).hexdigest()


def new_pair() -> Tuple[str, str]:
    """(token to hand out, hash to store)."""
    token = generate()
    return token, hash_token(token)


def matches(token: str, stored_hash: str) -> bool:
    """Whether a presented token matches a stored hash, compared in constant time."""
    if not token or not stored_hash:
        return False
    return hmac.compare_digest(hash_token(token), stored_hash)


def prefix(token: str, n: int = 8) -> str:
    """A short, non-secret fragment so the UI can tell two tokens apart in a list."""
    return token[:n]


def invite_code() -> str:
    """A shorter, friendlier code — it gets typed and pasted into chat windows, not clicked.

    Still 120+ bits of entropy, and every attempt to redeem one is rate limited and audited.
    """
    alphabet = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789"   # no I/O/0/1: they get misread
    raw = "".join(secrets.choice(alphabet) for _ in range(20))
    return "-".join(raw[i:i + 5] for i in range(0, 20, 5))


def normalize_invite_code(code: str) -> str:
    """Accept what people actually paste: lowercase, missing dashes, stray spaces."""
    cleaned = "".join(ch for ch in (code or "").upper() if ch.isalnum())
    if len(cleaned) != 20:
        return cleaned
    return "-".join(cleaned[i:i + 5] for i in range(0, 20, 5))
