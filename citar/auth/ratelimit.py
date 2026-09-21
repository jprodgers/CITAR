"""Rate limiting for the routes an attacker reaches without an account.

Two layers, on purpose:

*In-process token buckets* answer the hot path with no database round trip. They are exact within
one process and reset when it restarts.

*A database backstop* for the limits where a restart must not be an escape hatch — repeated failed
logins against one account, signup floods from one address. A process restart is cheap to cause if
anyone ever finds a way to crash a worker, so the counters that matter survive it.

Keys are scoped deliberately. Login is limited per account *and* per IP: per-account alone lets a
botnet spray one password across every account, and per-IP alone lets one host walk through
passwords for many accounts. Signup is limited per IP and per /24, because a single machine renting
a /24 is the cheapest way to look like 256 people.

Client IP comes from `client_ip()`, which honours X-Forwarded-For only for as many hops as
CITAR_TRUSTED_PROXY_HOPS says to trust. Reading the whole header would let anyone set their own IP
and make every limit here decorative.
"""
from __future__ import annotations

import threading
import time
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from typing import Optional

from .. import settings


@dataclass(frozen=True)
class Rule:
    """`limit` attempts per `window` seconds, then locked out for `block` seconds."""
    limit: int
    window: int
    block: int = 0

    def describe(self) -> str:
        """The rule as a sentence, for the error a limited caller receives."""
        unit = "minute" if self.window == 60 else f"{self.window // 60} minutes" if self.window % 60 == 0 else f"{self.window}s"
        return f"{self.limit} per {unit}"


#: The tuning here is meant to be invisible to a person using the site normally and tedious for a
#: script. Someone who forgot their password gets ten tries in fifteen minutes before a pause.
RULES = {
    "login_ip":        Rule(limit=30, window=900, block=900),
    "login_account":   Rule(limit=10, window=900, block=900),
    "signup_ip":       Rule(limit=5, window=3600, block=3600),
    "signup_subnet":   Rule(limit=15, window=3600, block=3600),
    "verify_resend":   Rule(limit=5, window=3600, block=1800),
    "reset_request":   Rule(limit=5, window=3600, block=1800),
    "reset_attempt":   Rule(limit=10, window=900, block=900),
    "invite_redeem":   Rule(limit=10, window=3600, block=3600),
    "oauth_start":     Rule(limit=30, window=900),
    "worker_auth":     Rule(limit=20, window=300, block=600),
    "api_write":       Rule(limit=300, window=60),
}


class RateLimited(Exception):
    """Raised when a caller has run out of attempts. `retry_after` is seconds."""

    def __init__(self, rule_name: str, retry_after: int):
        self.rule_name = rule_name
        self.retry_after = max(1, int(retry_after))
        minutes = max(1, self.retry_after // 60)
        super().__init__(f"Too many attempts. Try again in about {minutes} minute"
                         f"{'s' if minutes != 1 else ''}.")


# ------------------------------------------------------------------ in-process counters
_lock = threading.Lock()
_hits: dict = {}      # key -> (window_start, count)
_blocks: dict = {}    # key -> unblock timestamp


def _memory_check(key: str, rule: Rule) -> None:
    """Check a limit in memory, for a single process."""
    now = time.time()
    with _lock:
        until = _blocks.get(key)
        if until and until > now:
            raise RateLimited(key, until - now)
        if until:
            _blocks.pop(key, None)

        start, count = _hits.get(key, (now, 0))
        if now - start >= rule.window:
            start, count = now, 0
        count += 1
        _hits[key] = (start, count)
        if count > rule.limit:
            if rule.block:
                _blocks[key] = now + rule.block
                raise RateLimited(key, rule.block)
            raise RateLimited(key, rule.window - (now - start))


def _prune(now: Optional[float] = None) -> None:
    """Drop expired entries so the dictionaries cannot grow without bound from random keys."""
    now = now or time.time()
    with _lock:
        for key, until in list(_blocks.items()):
            if until <= now:
                _blocks.pop(key, None)
        for key, (start, _count) in list(_hits.items()):
            if now - start > 7200:
                _hits.pop(key, None)


# ------------------------------------------------------------------ durable backstop
def _db_check(session, key: str, rule: Rule) -> None:
    """Check a limit in the database, so several processes share one counter."""
    from ..db.models import RateLimit

    now = datetime.now(timezone.utc)
    row = session.get(RateLimit, key)
    if row is None:
        session.add(RateLimit(key=key, window_start=now, count=1))
        return
    if row.blocked_until and row.blocked_until > now:
        raise RateLimited(key, (row.blocked_until - now).total_seconds())
    if (now - row.window_start).total_seconds() >= rule.window:
        row.window_start, row.count, row.blocked_until = now, 1, None
        return
    row.count += 1
    if row.count > rule.limit:
        if rule.block:
            row.blocked_until = now + timedelta(seconds=rule.block)
            raise RateLimited(key, rule.block)
        raise RateLimited(key, rule.window - (now - row.window_start).total_seconds())


#: Which limits are worth a database write. The rest live in memory only.
DURABLE = {"login_account", "signup_ip", "signup_subnet", "reset_request", "invite_redeem", "worker_auth"}


def check(rule_name: str, subject: str, *, session=None) -> None:
    """Count one attempt against `rule_name` for `subject`. Raises RateLimited when exhausted.

    In local mode this is a no-op: the only client is the person who started the process, and
    locking yourself out of your own laptop is a bug, not a security feature.
    """
    if settings.get().local:
        return
    rule = RULES.get(rule_name)
    if rule is None:
        return
    key = f"{rule_name}:{subject}"
    _memory_check(key, rule)
    if session is not None and rule_name in DURABLE:
        _db_check(session, key, rule)


def clear(rule_name: str, subject: str, *, session=None) -> None:
    """Forget the attempts for a subject — called after a success, so a person who eventually
    remembers their password is not still serving out a lockout."""
    key = f"{rule_name}:{subject}"
    with _lock:
        _hits.pop(key, None)
        _blocks.pop(key, None)
    if session is not None:
        from ..db.models import RateLimit
        row = session.get(RateLimit, key)
        if row is not None:
            session.delete(row)


def client_ip(request) -> str:
    """The caller's address, trusting X-Forwarded-For only as far as configured.

    Caddy appends the real client to the header, so with one trusted hop the client is the last
    entry. Trusting the leftmost entry — the usual mistake — lets anyone put whatever they like in
    the header and sail past every limit in this module.
    """
    cfg = settings.get()
    direct = request.client.host if request.client else "unknown"
    if not cfg.behind_proxy:
        return direct
    forwarded = request.headers.get("x-forwarded-for", "")
    if not forwarded:
        return direct
    chain = [part.strip() for part in forwarded.split(",") if part.strip()]
    if not chain:
        return direct
    hops = max(1, cfg.trusted_proxy_hops)
    index = len(chain) - hops
    return chain[index] if 0 <= index < len(chain) else chain[0]


def subnet(ip: str) -> str:
    """The /24 (or /64 for IPv6) an address sits in, for limiting a rented block as one actor."""
    if ":" in ip:
        return ":".join(ip.split(":")[:4]) + "::/64"
    parts = ip.split(".")
    return ".".join(parts[:3]) + ".0/24" if len(parts) == 4 else ip


def status() -> dict:
    """What is currently blocked — for the admin console."""
    _prune()
    now = time.time()
    with _lock:
        return {"blocked": [{"key": k, "seconds": int(v - now)} for k, v in _blocks.items() if v > now],
                "tracked": len(_hits)}
