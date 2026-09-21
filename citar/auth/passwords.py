"""Password hashing and strength rules.

argon2id, with parameters at the argon2-cffi defaults (64 MiB, 3 passes, 4 lanes) which sit
comfortably above the OWASP floor. Verification transparently re-hashes when the parameters change,
so raising the cost later upgrades accounts as people log in rather than needing a reset.

On strength: length is what actually matters, so the rule is a 10-character minimum with no
composition requirements — forcing a symbol and a digit produces `Password1!` and nothing else. What
is worth blocking is the small set of passwords that attackers try first, and anything derived from
the account's own handle or email, which is the one guess a targeted attacker always makes.
"""
from __future__ import annotations

from typing import Optional

from argon2 import PasswordHasher
from argon2.exceptions import InvalidHashError, VerificationError, VerifyMismatchError

MIN_LENGTH = 10
MAX_LENGTH = 1024      # argon2 handles long inputs fine; this just stops a 10 MB upload

_hasher = PasswordHasher()

#: The passwords that appear at the top of every breach corpus. This is not a substitute for a full
#: k-anonymity check against Have I Been Pwned — which we deliberately do not do, because it would
#: mean a network call on the signup path and a third party learning a prefix of every password
#: chosen here — but it removes the guesses that any credential-stuffing run starts with.
COMMON = frozenset("""
123456 password 123456789 12345678 12345 qwerty abc123 password1 1234567 111111 1234567890 123123
987654321 qwertyuiop mynoob 123321 666666 18atcskd2w 7777777 1q2w3e4r 654321 555555 3rjs1la7qe
google 1q2w3e4r5t 123qwe zxcvbnm 1q2w3e letmein login princess qwertyuiopasdfghjklzxcvbnm solo
passw0rd starwars monkey dragon sunshine iloveyou trustno1 baseball football superman batman
master hello freedom whatever qazwsx welcome admin administrator root toor changeme default
letmein123 password123 pass123 test123 guest qwerty123 iloveyou1 michael jennifer computer
shadow jordan harley ranger buster soccer hockey killer george andrew charlie thomas robert
access flower 121212 696969 ashley bailey pepper daniel hunter summer chelsea matthew
citar civilization civ5 civ6
""".split())


class PasswordError(ValueError):
    """Rejected password. The message is safe to show the person choosing it."""


def hash_password(password: str) -> str:
    """Hash a password with argon2id."""
    check(password)
    return _hasher.hash(password)


def verify(stored_hash: Optional[str], password: str) -> bool:
    """Whether the password matches. Never raises for a wrong password or a malformed hash.

    Accounts that sign in only through SSO have no hash at all; those return False rather than
    letting an empty password in.
    """
    if not stored_hash or not password:
        return False
    try:
        return _hasher.verify(stored_hash, password)
    except (VerifyMismatchError, VerificationError, InvalidHashError):
        return False


def needs_rehash(stored_hash: str) -> bool:
    """Whether a stored hash was made with weaker parameters and should be upgraded.

    Checked at sign-in, when the password is in hand: that is the only moment a hash can be upgraded
    without asking anybody to do anything.
    """
    try:
        return _hasher.check_needs_rehash(stored_hash)
    except (InvalidHashError, ValueError):
        return False


def check(password: str, *, handle: str = "", email: str = "") -> None:
    """Raise PasswordError if this password should not be accepted."""
    if not password:
        raise PasswordError("Enter a password.")
    if len(password) < MIN_LENGTH:
        raise PasswordError(f"Use at least {MIN_LENGTH} characters. Length matters far more than "
                            "punctuation — a short phrase you will remember beats a mangled word.")
    if len(password) > MAX_LENGTH:
        raise PasswordError(f"That password is over {MAX_LENGTH} characters.")
    if password.strip() != password and not password.strip():
        raise PasswordError("That password is only whitespace.")

    folded = password.lower()
    if folded in COMMON:
        raise PasswordError("That password appears at the top of every leaked-password list. "
                            "Pick something else.")
    if folded.strip("0123456789!") in COMMON:
        raise PasswordError("That is a common password with a few characters tacked on, which is "
                            "the first thing an attacker tries. Pick something else.")

    for label, value in (("username", handle), ("email address", (email or "").split("@")[0])):
        value = (value or "").lower()
        if len(value) >= 3 and value in folded:
            raise PasswordError(f"Your password cannot contain your {label}.")

    if len(set(password)) <= 2:
        raise PasswordError("That password repeats one or two characters.")
    if folded in ("abcdefghij", "0123456789", "qwertyuiop") or _is_run(folded):
        raise PasswordError("That password is a straight run of characters.")


def _is_run(s: str) -> bool:
    """A password that is entirely ascending or descending consecutive characters."""
    if len(s) < MIN_LENGTH:
        return False
    deltas = {ord(b) - ord(a) for a, b in zip(s, s[1:])}
    return deltas in ({1}, {-1})
