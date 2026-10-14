"""The CITAR schema.

Written to the intersection of SQLite and PostgreSQL: no dialect-specific column types, table names
that avoid reserved words in either (`users`, not `user`; `access_grants`, not `grant`), and
timezone-aware timestamps everywhere.

Two conventions worth knowing before reading:

*Rich config stays JSON.* A server's hardware, power draw, component depreciation, cost periods,
model catalog and load profiles are a deep, evolving structure that `citar/servers.py` already
validates, normalizes, prices and renders. Normalizing it into tables would mean rewriting all of
that for no query we actually need. Instead `Server.config` holds the validated dict, and only the
fields we filter or join on — owner, group, name, kind, provider — are real columns.

*Times are stored in UTC, displayed in somebody's zone.* The one exception is availability windows,
which are stored as wall-clock minutes plus the weekday and interpreted in the owning account's IANA
zone at evaluation time. That is what makes "midnight to 6am" stay at the owner's midnight across
DST, instead of drifting an hour twice a year.
"""
from __future__ import annotations

import secrets
from datetime import datetime, timezone
from typing import Optional

from sqlalchemy import (JSON, Boolean, DateTime, Float, ForeignKey, Index, Integer, String, Text,
                        TypeDecorator, UniqueConstraint)
from sqlalchemy.orm import DeclarativeBase, Mapped, mapped_column, relationship


class Base(DeclarativeBase):
    """The declarative base every table inherits from."""
    pass


def now() -> datetime:
    """The current time, in UTC, aware. The only way a timestamp is produced here."""
    return datetime.now(timezone.utc)


def new_id(prefix: str = "") -> str:
    """A new opaque id with a type prefix."""
    return prefix + secrets.token_hex(8)


class UTCDateTime(TypeDecorator):
    """A timestamp that is always timezone-aware UTC coming back out, on every database.

    SQLite has no timestamp type: it stores an ISO string and drops the offset, so a plain
    DateTime(timezone=True) column hands back a *naive* datetime. Postgres hands back an aware one.
    Comparing either against `datetime.now(timezone.utc)` then works on one database and raises
    TypeError on the other — a bug that only appears after deploying, which is the whole failure
    mode this schema is trying to avoid.

    So every timestamp goes through here: normalized to UTC on the way in, re-stamped as UTC on the
    way out. Anything naive arriving from a caller is assumed to be UTC, because that is the only
    thing this application ever produces.
    """
    impl = DateTime(timezone=True)
    cache_ok = True

    def process_bind_param(self, value, dialect):
        """Normalise a value to UTC on the way in."""
        if value is None:
            return None
        if value.tzinfo is None:
            return value.replace(tzinfo=timezone.utc)
        return value.astimezone(timezone.utc)

    def process_result_value(self, value, dialect):
        """Return an aware UTC value on the way out, whatever the driver gave us."""
        if value is None:
            return None
        if value.tzinfo is None:
            return value.replace(tzinfo=timezone.utc)
        return value.astimezone(timezone.utc)


def _ts(**kw) -> Mapped[datetime]:
    """A timestamp column with the right type and defaults."""
    return mapped_column(UTCDateTime(), **kw)


# ============================================================================ identity

ROLES = ("user", "moderator", "admin")
ROLE_RANK = {"user": 0, "moderator": 1, "admin": 2}

#: pending   signed up, email not yet verified — may look, may not create
#: probation verified but new; may play and watch, may not register servers, invite or publish
#: active    full access within their role and capabilities
#: suspended blocked by a moderator or admin; sessions are revoked, login is refused
#: deleted   self-deleted or removed; personal fields are blanked, owned rows are reassigned
USER_STATUSES = ("pending", "probation", "active", "suspended", "deleted")

DATA_SHARING = ("pool", "private")

#: Who a machine or an account is trusted as (P3.2.2): set by admins in Phase 3, automated in Phase 4.
TRUST_TIERS = ("new", "provisional", "trusted", "quarantined")

#: The profile fields whose visibility an account chooses (P3.2.6), and their defaults.
USER_FIELD_DEFAULTS = {"profile": "server", "display_name": "server", "avatar": "server", "bio": "server",
                       "links": "server", "items": "server", "ratio": "private"}

#: A profile field's levels, narrowest first: `private` the account and administrators, `server` every signed-in
#: member in good standing too, `global` anyone. Only these three: an account is no item, so it has no allowlist or
#: link of its own.
FIELD_LEVELS = ("private", "server", "global")
#: Who a viewer is to an account, as a field asks it: how wide a field may be and still show to them.
_FIELD_RANK = {"global": 0, "server": 1, "private": 2}
_VISITOR, _MEMBER, _SELF = 0, 1, 2

#: Per-account permission switches, on top of the coarse role. The defaults for each role live in
#: the runtime settings table so an admin sets policy once rather than per user; a value here is an
#: explicit override for one account.
CAPABILITIES = (
    "register_servers",     # may add inference servers and share them
    "run_reports",          # may build reports — required for private analysis to be useful
    "publish_global",       # may make a game, a report or another item readable by the whole internet
    "create_games",         # may start games at all
    "invite",               # may mint invite codes, up to invite_quota
    # Owned items (design/phase3-features.md P3.2.4): the job types a member may start.
    "run_benchmarks",       # may save benchmark suites and start runs
    "run_probes",           # may save probes and run them
    "run_lab",              # may queue lab experiments, which use the server's own CPU
    "edit_bots",            # may create, fork and edit bot profiles
    # Task classes (P3.3.7): the hooks future restrictions read.
    "full_llm",             # may seat a model that plays every turn
    "partial_llm",          # may use pooled prompts (a hybrid seat's diplomacy, a report's narrative)
)


class User(Base):
    """An account: who they are, what they may do, and how they sign in."""
    __tablename__ = "users"

    id: Mapped[str] = mapped_column(String(40), primary_key=True, default=lambda: new_id("u_"))
    handle: Mapped[str] = mapped_column(String(40))
    # Lowercased handle for uniqueness and lookup: SQLite's LIKE is case-insensitive for ASCII but
    # its `=` is not, and Postgres is case-sensitive for both. Comparing an explicit column is the
    # only thing that behaves identically on both.
    handle_lower: Mapped[str] = mapped_column(String(40), unique=True, index=True)
    display_name: Mapped[str] = mapped_column(String(80), default="")

    email: Mapped[Optional[str]] = mapped_column(String(320))
    email_lower: Mapped[Optional[str]] = mapped_column(String(320), unique=True, index=True)
    email_verified: Mapped[bool] = mapped_column(Boolean, default=False)
    # argon2id. Null for accounts that only ever sign in through SSO.
    password_hash: Mapped[Optional[str]] = mapped_column(String(255))

    role: Mapped[str] = mapped_column(String(16), default="user")
    status: Mapped[str] = mapped_column(String(16), default="pending")
    # IANA zone, detected from the browser at first login and editable. Drives availability windows,
    # budget period boundaries, and every timestamp the account is shown.
    tz: Mapped[str] = mapped_column(String(64), default="UTC")
    locale: Mapped[str] = mapped_column(String(16), default="en")
    avatar_url: Mapped[Optional[str]] = mapped_column(String(512))

    #: Default for games this account creates; each game can override it.
    data_sharing: Mapped[str] = mapped_column(String(16), default="pool")
    #: Explicit capability overrides, {capability: bool}. Absent keys fall back to the role default.
    caps: Mapped[dict] = mapped_column(JSON, default=dict)
    invite_quota: Mapped[int] = mapped_column(Integer, default=0)
    max_concurrent_games: Mapped[int] = mapped_column(Integer, default=2)
    #: 0 means the ruleset's maximum. A 24-civ Gargantuan game is a lot of CPU on a shared box.
    max_seats_per_game: Mapped[int] = mapped_column(Integer, default=0)

    #: True for the single account local mode logs in automatically. Never exists in server mode.
    is_local_owner: Mapped[bool] = mapped_column(Boolean, default=False)

    created_at: Mapped[datetime] = _ts(default=now)
    last_seen_at: Mapped[Optional[datetime]] = _ts(nullable=True)
    suspended_at: Mapped[Optional[datetime]] = _ts(nullable=True)
    suspended_by: Mapped[Optional[str]] = mapped_column(String(40))
    suspended_reason: Mapped[Optional[str]] = mapped_column(Text)
    invited_by: Mapped[Optional[str]] = mapped_column(String(40))

    # The profile (design/phase3-features.md P3.2.6), filled in by package 3-04.
    #: Free text, control characters stripped, at most 1,000 characters.
    bio: Mapped[str] = mapped_column(Text, default="")
    #: At most three {"label", "url"}, http or https.
    links: Mapped[list] = mapped_column(JSON, default=list)
    #: An uploaded avatar, relative to the data directory. `avatar_url` stays the sign-in provider's picture.
    avatar_path: Mapped[Optional[str]] = mapped_column(String(200))
    #: {field: private | server | global} for profile, display_name, avatar, bio, links, items and ratio; absent
    #: fields take their defaults (USER_FIELD_DEFAULTS).
    field_visibility: Mapped[dict] = mapped_column(JSON, default=dict)
    #: Whether the account appears in the members' directory. A hidden account still has its page.
    listed: Mapped[bool] = mapped_column(Boolean, default=True)
    #: new | provisional | trusted | quarantined (TRUST_TIERS): set by admins in Phase 3, automated in Phase 4.
    trust_tier: Mapped[str] = mapped_column(String(16), default="new")

    identities: Mapped[list["Identity"]] = relationship(back_populates="user", cascade="all, delete-orphan")

    @property
    def active(self) -> bool:
        """Whether this account may currently do anything at all."""
        return self.status in ("probation", "active")

    @property
    def name(self) -> str:
        """The name to show: their display name, or their handle."""
        return self.display_name or self.handle

    def at_least(self, role: str) -> bool:
        """Whether this account's role is at least the one named."""
        return ROLE_RANK.get(self.role, 0) >= ROLE_RANK[role]

    # ------------------------------------------------------------------ the profile's visibility (P3.2.6)

    def chosen_levels(self) -> dict:
        """Each profile field's level as the account chose it: its choice over the defaults, a value that is not a
        level read as `private` (a null as no choice, as the directory's search reads it in SQL). What its own
        settings show; ``field_levels`` is what applies."""
        chosen = self.field_visibility if isinstance(self.field_visibility, dict) else {}
        out = {}
        for field, default in USER_FIELD_DEFAULTS.items():
            value = chosen.get(field)
            out[field] = default if value is None else value if value in FIELD_LEVELS else "private"
        return out

    def field_levels(self) -> dict:
        """Each profile field's level as it applies now (``chosen_levels``), narrowed by the account's standing
        whatever was chosen: an account on probation shows nothing wider than `server` (making a field `global` is
        refused on probation, and an account put back on probation is held to the same), and an account not in good
        standing (pending, suspended, deleted) shows nothing to anyone but itself and administrators."""
        levels = self.chosen_levels()
        if self.status not in ("probation", "active"):
            return dict.fromkeys(levels, "private")
        if self.status == "probation":
            return {f: "server" if level == "global" else level for f, level in levels.items()}
        return levels

    def viewer_rank(self, viewer: Optional["User"]) -> int:
        """Who ``viewer`` is to this account: the account itself or an administrator (everything), a member in good
        standing (`server` and `global` fields), or a visitor (`global` only). A suspended, deleted or pending
        account sees what a visitor does."""
        if viewer is None or viewer.status not in ("probation", "active"):
            return _VISITOR
        if viewer.id == self.id or viewer.role == "admin":
            return _SELF
        return _MEMBER

    def visible_fields(self, viewer: Optional["User"], *, rank: Optional[int] = None) -> frozenset:
        """The profile fields of this account that show to ``viewer``: those whose level the viewer reaches, and
        none unless the viewer reaches the profile's own (the page's gate, so a hidden profile hides every field).
        ``rank`` is the viewer's rank when the caller already has it, or a narrower one ("view as a visitor")."""
        if rank is None:
            rank = self.viewer_rank(viewer)
        levels = self.field_levels()
        if rank < _FIELD_RANK[levels["profile"]]:
            return frozenset()
        return frozenset(f for f, level in levels.items() if rank >= _FIELD_RANK[level])

    def shows(self, field: str, viewer: Optional["User"], *, rank: Optional[int] = None) -> bool:
        """Whether ``field`` of this account shows to ``viewer`` (``visible_fields``)."""
        return field in self.visible_fields(viewer, rank=rank)

    @property
    def avatar_route(self) -> Optional[str]:
        """Where an uploaded avatar is served (with its content's hash, so a new one is fetched at once), or None."""
        if not self.avatar_path:
            return None
        from urllib.parse import quote
        stem = self.avatar_path.rsplit("/", 1)[-1].rsplit(".", 1)[0]
        return f"/api/users/{quote(self.handle, safe='')}/avatar?v={quote(stem.rsplit('-', 1)[-1], safe='')}"

    def public(self, viewer: Optional["User"], *, rank: Optional[int] = None) -> dict:
        """How the account appears to ``viewer`` (None: a visitor with no account), wherever it appears: seats,
        sharing lists, grants, owners, the directory (P3.2.6). The handle always; the display name and the avatar
        (the uploaded one's route, else the sign-in provider's picture) only when those fields show to ``viewer``,
        else the handle and null. Every caller names its viewer: there is no default, so a new caller cannot leak a
        hidden name by forgetting it (tests/test_profiles.py also lists the callers).

        No role and no status: whether somebody is a moderator, on probation or suspended is the account's own
        business and the administrators', not every viewer's (`account` carries both).
        """
        shown = self.visible_fields(viewer, rank=rank)
        return {"id": self.id, "handle": self.handle,
                "display_name": self.name if "display_name" in shown else self.handle,
                "avatar": (self.avatar_route or self.avatar_url) if "avatar" in shown else None}

    def account(self) -> dict:
        """The account as its owner and administrators see it: `public` plus its role, status, profile, settings and
        limits. Never for anyone else."""
        d = self.public(self, rank=_SELF)
        d.update(role=self.role, status=self.status, email=self.email, email_verified=self.email_verified,
                 tz=self.tz, data_sharing=self.data_sharing, caps=dict(self.caps or {}),
                 invite_quota=self.invite_quota, max_concurrent_games=self.max_concurrent_games,
                 max_seats_per_game=self.max_seats_per_game, trust_tier=self.trust_tier, listed=self.listed,
                 bio=self.bio or "", links=list(self.links or []), field_visibility=self.chosen_levels(),
                 avatar_url=self.avatar_url, avatar_uploaded=bool(self.avatar_path),
                 has_password=bool(self.password_hash),
                 created_at=self.created_at.isoformat() if self.created_at else None,
                 providers=[i.provider for i in self.identities])
        return d


class Identity(Base):
    """A single-sign-on account linked to a CITAR user. One user may have several."""
    __tablename__ = "identities"
    __table_args__ = (UniqueConstraint("provider", "provider_user_id", name="uq_identity_provider_subject"),)

    id: Mapped[str] = mapped_column(String(40), primary_key=True, default=lambda: new_id("i_"))
    user_id: Mapped[str] = mapped_column(ForeignKey("users.id", ondelete="CASCADE"), index=True)
    provider: Mapped[str] = mapped_column(String(32))
    provider_user_id: Mapped[str] = mapped_column(String(255))
    email: Mapped[Optional[str]] = mapped_column(String(320))
    #: Whether the *provider* asserted the email is verified. An unverified provider email is never
    #: allowed to auto-link to an existing CITAR account — that is account takeover.
    email_verified: Mapped[bool] = mapped_column(Boolean, default=False)
    display_name: Mapped[Optional[str]] = mapped_column(String(120))
    avatar_url: Mapped[Optional[str]] = mapped_column(String(512))
    created_at: Mapped[datetime] = _ts(default=now)
    last_login_at: Mapped[Optional[datetime]] = _ts(nullable=True)

    user: Mapped[User] = relationship(back_populates="identities")


class AuthSession(Base):
    """A logged-in browser. The cookie carries a random token; only its hash is stored, so a leaked
    database backup does not hand over live sessions."""
    __tablename__ = "auth_sessions"

    id: Mapped[str] = mapped_column(String(40), primary_key=True, default=lambda: new_id("s_"))
    user_id: Mapped[str] = mapped_column(ForeignKey("users.id", ondelete="CASCADE"), index=True)
    token_hash: Mapped[str] = mapped_column(String(64), unique=True, index=True)
    #: Paired with the session for double-submit CSRF checking on state-changing requests.
    csrf_token: Mapped[str] = mapped_column(String(64), default=lambda: secrets.token_urlsafe(32))
    created_at: Mapped[datetime] = _ts(default=now)
    last_seen_at: Mapped[datetime] = _ts(default=now)
    expires_at: Mapped[datetime] = _ts()
    revoked_at: Mapped[Optional[datetime]] = _ts(nullable=True)
    ip: Mapped[Optional[str]] = mapped_column(String(64))
    user_agent: Mapped[Optional[str]] = mapped_column(String(300))

    def live(self, idle_cutoff: Optional[datetime] = None) -> bool:
        """Whether this session is still valid, by absolute age and by idleness."""
        if self.revoked_at or self.expires_at <= now():
            return False
        return not (idle_cutoff and self.last_seen_at < idle_cutoff)


class Invite(Base):
    """An invite code. The code is stored as issued: it is a low-value bearer token that the creator
    has to be able to copy again to send it, and every use is recorded against the account it made."""
    __tablename__ = "invites"

    id: Mapped[str] = mapped_column(String(40), primary_key=True, default=lambda: new_id("inv_"))
    code: Mapped[str] = mapped_column(String(64), unique=True, index=True)
    created_by: Mapped[Optional[str]] = mapped_column(ForeignKey("users.id", ondelete="SET NULL"))
    #: Role the invited account is created with. Only an admin may mint anything above `user`.
    role: Mapped[str] = mapped_column(String(16), default="user")
    #: Optional pin: the invite only works for this address.
    email: Mapped[Optional[str]] = mapped_column(String(320))
    max_uses: Mapped[int] = mapped_column(Integer, default=1)
    uses: Mapped[int] = mapped_column(Integer, default=0)
    #: Invited accounts can skip probation — somebody vouched for them.
    skip_probation: Mapped[bool] = mapped_column(Boolean, default=True)
    note: Mapped[Optional[str]] = mapped_column(Text)
    created_at: Mapped[datetime] = _ts(default=now)
    expires_at: Mapped[Optional[datetime]] = _ts(nullable=True)
    revoked_at: Mapped[Optional[datetime]] = _ts(nullable=True)

    def usable(self) -> bool:
        """Whether this invitation can still be redeemed."""
        return (self.revoked_at is None and self.uses < self.max_uses
                and (self.expires_at is None or self.expires_at > now()))


class EmailToken(Base):
    """One-shot link sent by email: address verification, password reset, address change."""
    __tablename__ = "email_tokens"

    id: Mapped[str] = mapped_column(String(40), primary_key=True, default=lambda: new_id("et_"))
    user_id: Mapped[str] = mapped_column(ForeignKey("users.id", ondelete="CASCADE"), index=True)
    purpose: Mapped[str] = mapped_column(String(24))          # verify | reset | change_email
    token_hash: Mapped[str] = mapped_column(String(64), unique=True, index=True)
    #: For change_email: the address being moved to, verified before it replaces the current one.
    new_email: Mapped[Optional[str]] = mapped_column(String(320))
    created_at: Mapped[datetime] = _ts(default=now)
    expires_at: Mapped[datetime] = _ts()
    used_at: Mapped[Optional[datetime]] = _ts(nullable=True)


# ============================================================================ servers & sharing

SERVER_PURPOSES = ("game", "benchmark", "probe", "scenario", "lab")

#: The visibility ladder (design/phase3-features.md P3.2.3), narrowest first. `citar.auth.access` is the one place
#: that interprets it (`access.VISIBILITY` is this tuple, `access.check_visibility` its validator):
#:   private    the owner and admins
#:   allowlist  and the accounts with an ACL entry
#:   link       and anyone presenting the slug (never listed)
#:   server     and every signed-in member in good standing (listed for members)
#:   global     anyone on the internet (listed for everyone; needs `publish_global`)
VISIBILITY = ("private", "allowlist", "link", "server", "global")


class ServerGroup(Base):
    """The unit of sharing. Servers go in groups; grants are made against groups."""
    __tablename__ = "server_groups"

    id: Mapped[str] = mapped_column(String(40), primary_key=True, default=lambda: new_id("sg_"))
    #: Null once its owner's account is gone: deleting an account never deletes what others were granted.
    #: A group without an owner is managed by admins only.
    owner_id: Mapped[Optional[str]] = mapped_column(ForeignKey("users.id", ondelete="SET NULL"), index=True)
    name: Mapped[str] = mapped_column(String(120))
    description: Mapped[str] = mapped_column(Text, default="")
    visibility: Mapped[str] = mapped_column(String(16), default="private")
    #: Unguessable id for `link` visibility; rotating it invalidates every shared link at once.
    slug: Mapped[str] = mapped_column(String(48), unique=True, index=True,
                                      default=lambda: secrets.token_urlsafe(16))
    #: Whether anyone may request access, or the owner grants it unprompted.
    accepts_requests: Mapped[bool] = mapped_column(Boolean, default=True)
    created_at: Mapped[datetime] = _ts(default=now)
    archived_at: Mapped[Optional[datetime]] = _ts(nullable=True)


HARDWARE_KINDS = ("gpu", "cpu", "api")

#: The classes every database starts with (P3.4.3): one default per kind, at the energy-proxy rates (an
#: unclassified GPU generation-second 1.0 credit, a CPU core-second 0.05) and an online API's token cost at 1,000
#: credits per unit of currency. The baseline revision seeds them (written out there, as a migration must be), and
#: `citar.db.create_all` seeds a database made without migrations the same way.
HARDWARE_CLASS_SEEDS = (
    {"id": "hc_gpu", "kind": "gpu", "name": "GPU (unclassified)", "rate": 1.0},
    {"id": "hc_cpu", "kind": "cpu", "name": "CPU core (unclassified)", "rate": 0.05},
    {"id": "hc_api", "kind": "api", "name": "Online API", "rate": 1000.0},
)


class HardwareClass(Base):
    """A price class for compute (P3.4.3): credits per generation-second (`gpu`), per core-second (`cpu`) or per
    unit of the registry's currency of token cost (`api`). A machine takes the first class of each kind, by
    priority, whose `match` fits its hardware, else the kind's default. Package 3-10 prices with it."""
    __tablename__ = "hardware_classes"

    id: Mapped[str] = mapped_column(String(40), primary_key=True, default=lambda: new_id("hc_"))
    kind: Mapped[str] = mapped_column(String(8))                        # HARDWARE_KINDS
    name: Mapped[str] = mapped_column(String(80))
    rate: Mapped[float] = mapped_column(Float, default=0.0)
    #: {"gpu_name": regex, "vram_gb_min": n, "cpu_name": regex, "cores_min": n}; empty matches everything.
    match: Mapped[dict] = mapped_column(JSON, default=dict)
    priority: Mapped[int] = mapped_column(Integer, default=0)
    is_default: Mapped[bool] = mapped_column(Boolean, default=False)
    updated_at: Mapped[datetime] = _ts(default=now, onupdate=now)
    updated_by: Mapped[Optional[str]] = mapped_column(String(40))


class Server(Base):
    """One machine or API endpoint that can run a model.

    `config` is the validated dict from citar/servers.py — connection, hardware, power, components,
    cost periods, restricted hours and the model catalog. Keeping it whole means the existing cost
    engine, hardware collector and load-profile code keep working untouched.
    """
    __tablename__ = "servers"

    id: Mapped[str] = mapped_column(String(40), primary_key=True, default=lambda: new_id("sv_"))
    #: Null once its owner's account is gone, and for the host and the site's API rows when nobody owns them:
    #: deleting an account must never delete the host. A machine without an owner is managed by admins only.
    owner_id: Mapped[Optional[str]] = mapped_column(ForeignKey("users.id", ondelete="SET NULL"), index=True)
    group_id: Mapped[Optional[str]] = mapped_column(ForeignKey("server_groups.id", ondelete="SET NULL"), index=True)
    name: Mapped[str] = mapped_column(String(120))
    kind: Mapped[str] = mapped_column(String(16), default="owned")      # owned | leased | api | test
    provider: Mapped[str] = mapped_column(String(32), default="none")
    config: Mapped[dict] = mapped_column(JSON, default=dict)

    #: How this server is reached. `worker` means an agent dials out to us and we never connect in;
    #: `direct` means CITAR opens a connection to base_url itself (only sane on a trusted network).
    reach: Mapped[str] = mapped_column(String(16), default="worker")
    enabled: Mapped[bool] = mapped_column(Boolean, default=True)
    #: The site's one number for this machine's LLM slots (P3.2.8); a helper's own ceiling caps it.
    max_concurrent: Mapped[int] = mapped_column(Integer, default=1)

    # Filled in by the registry merge (3-05: is_host, server_resource), the scheduler (3-07), the ledger (3-10) and
    # the machine settings dialog (3-18); see design/phase3-features.md P3.2.2 and P3.8.2.
    #: The machine running CITAR.
    is_host: Mapped[bool] = mapped_column(Boolean, default=False)
    #: The site provides it (the host, an online API an admin registers), so it is the server's in the ledger.
    server_resource: Mapped[bool] = mapped_column(Boolean, default=False)
    #: {"llm": {"enabled", "allow_swaps"}, "cpu": {"enabled", "threads", "cpu_percent", "memory_mb", "work"}}.
    capabilities: Mapped[dict] = mapped_column(JSON, default=dict)
    #: auto | manual each: whether a helper's hello may change the hardware, the models and the power figures.
    hardware_mode: Mapped[str] = mapped_column(String(8), default="auto")
    models_mode: Mapped[str] = mapped_column(String(8), default="auto")
    power_mode: Mapped[str] = mapped_column(String(8), default="manual")
    #: The last hardware scan a helper reported, kept whatever the modes are, and when it came.
    hardware_detected: Mapped[Optional[dict]] = mapped_column(JSON)
    detected_at: Mapped[Optional[datetime]] = _ts(nullable=True)
    gpu_class_id: Mapped[Optional[str]] = mapped_column(ForeignKey("hardware_classes.id", ondelete="SET NULL"))
    cpu_class_id: Mapped[Optional[str]] = mapped_column(ForeignKey("hardware_classes.id", ondelete="SET NULL"))
    #: An admin pinned the classes: a hardware change does not reclassify the machine.
    class_override: Mapped[bool] = mapped_column(Boolean, default=False)
    trust_tier: Mapped[str] = mapped_column(String(16), default="new")             # TRUST_TIERS
    #: Set by the scheduler: {"overreports": {"count", "last"}} for a helper whose reported time exceeds what the
    #: server observed.
    flags: Mapped[dict] = mapped_column(JSON, default=dict)

    # The helper and proof of work (design/phase4-helper.md P4.5.1, P4.5.8; revision 0016c_helper_jobs).
    #: The machine key its helper registered with its first protocol 2 hello (base64 of 32 bytes): trust on first use,
    #: so a hello with another key is refused until the owner or an admin resets it (null again).
    public_key: Mapped[Optional[str]] = mapped_column(String(64))
    key_at: Mapped[Optional[datetime]] = _ts(nullable=True)
    #: The last hello's helper block, with the limits it acknowledged and its update state; null before any.
    helper: Mapped[Optional[dict]] = mapped_column(JSON)
    #: Proof of work's counters (P4.5.8): {verified, streak, mismatches, last_verified_at, last_mismatch, abandons,
    #: since, verifier, speed}; empty until the machine's first CPU job. A key reset never changes it.
    trust: Mapped[dict] = mapped_column(JSON, default=dict, server_default="{}")

    created_at: Mapped[datetime] = _ts(default=now)
    updated_at: Mapped[datetime] = _ts(default=now, onupdate=now)


class WorkerToken(Base):
    """Credential a `citar-worker` presents when it dials in. Scoped to one server, revocable, and
    stored only as a hash — losing the database does not let anyone impersonate a worker."""
    __tablename__ = "worker_tokens"

    id: Mapped[str] = mapped_column(String(40), primary_key=True, default=lambda: new_id("wt_"))
    server_id: Mapped[str] = mapped_column(ForeignKey("servers.id", ondelete="CASCADE"), index=True)
    token_hash: Mapped[str] = mapped_column(String(64), unique=True, index=True)
    #: First characters of the token, so the owner can tell two tokens apart in the UI.
    prefix: Mapped[str] = mapped_column(String(12))
    label: Mapped[str] = mapped_column(String(80), default="")
    created_at: Mapped[datetime] = _ts(default=now)
    created_by: Mapped[Optional[str]] = mapped_column(String(40))
    last_seen_at: Mapped[Optional[datetime]] = _ts(nullable=True)
    last_ip: Mapped[Optional[str]] = mapped_column(String(64))
    revoked_at: Mapped[Optional[datetime]] = _ts(nullable=True)


class AccessGrant(Base):
    """Permission for somebody to run work on a server group, with the conditions attached.

    `subject_type` is `user` for one account or `everyone` for the open pool. Conditions that are
    empty mean "no restriction": no windows means any hour, no budget means uncapped.
    """
    __tablename__ = "access_grants"

    id: Mapped[str] = mapped_column(String(40), primary_key=True, default=lambda: new_id("g_"))
    group_id: Mapped[str] = mapped_column(ForeignKey("server_groups.id", ondelete="CASCADE"), index=True)
    subject_type: Mapped[str] = mapped_column(String(16), default="user")    # user | everyone
    subject_user_id: Mapped[Optional[str]] = mapped_column(ForeignKey("users.id", ondelete="CASCADE"), index=True)

    #: Which kinds of work may use it, from SERVER_PURPOSES. Empty list means all purposes.
    purposes: Mapped[list] = mapped_column(JSON, default=list)
    #: Simultaneous games this subject may run on the group. 0 means inherit the group default.
    concurrency: Mapped[int] = mapped_column(Integer, default=0)
    #: Higher priority wins a contended slot in the queue.
    priority: Mapped[int] = mapped_column(Integer, default=0)
    #: Suspend a grant without deleting it (and without losing its budget history).
    enabled: Mapped[bool] = mapped_column(Boolean, default=True)
    note: Mapped[Optional[str]] = mapped_column(Text)

    created_by: Mapped[Optional[str]] = mapped_column(String(40))
    created_at: Mapped[datetime] = _ts(default=now)
    expires_at: Mapped[Optional[datetime]] = _ts(nullable=True)

    def live(self) -> bool:
        """Whether this grant is currently in force."""
        return self.enabled and (self.expires_at is None or self.expires_at > now())


class AvailabilityWindow(Base):
    """When a grant may be used, in the *owning account's* wall clock.

    Stored as weekday + minutes-past-midnight rather than as instants, and resolved through the
    owner's IANA zone when evaluated. That is what keeps a midnight-to-6am window at the owner's
    midnight through daylight-saving changes, and lets the UI render it in the viewer's zone.
    A window that wraps past midnight (start > end) is normalized into two rows at save time.
    """
    __tablename__ = "availability_windows"

    id: Mapped[str] = mapped_column(String(40), primary_key=True, default=lambda: new_id("w_"))
    grant_id: Mapped[str] = mapped_column(ForeignKey("access_grants.id", ondelete="CASCADE"), index=True)
    weekday: Mapped[int] = mapped_column(Integer)        # 0 = Monday, matching datetime.weekday()
    start_min: Mapped[int] = mapped_column(Integer)      # inclusive, 0..1439
    end_min: Mapped[int] = mapped_column(Integer)        # exclusive, 1..1440


BUDGET_PERIODS = ("day", "week", "month", "total")


BUDGET_UNITS = ("credits",)


class Budget(Base):
    """A spend cap on a grant, in credits, the compute ledger's unit (P3.4.6). Accounting and admission control
    only — no money moves anywhere.

    Spend is what the ledger (`compute_events`, package 3-10) settles against the budget's entries, so a budget is
    counted in the same unit as everything a member gives and uses.
    """
    __tablename__ = "budgets"

    id: Mapped[str] = mapped_column(String(40), primary_key=True, default=lambda: new_id("b_"))
    grant_id: Mapped[str] = mapped_column(ForeignKey("access_grants.id", ondelete="CASCADE"), index=True)
    period: Mapped[str] = mapped_column(String(12), default="month")
    amount: Mapped[float] = mapped_column(Float, default=0.0)
    unit: Mapped[str] = mapped_column(String(8), default="credits")      # BUDGET_UNITS
    #: hard refuses to start work that would exceed the cap and pauses work that crosses it;
    #: soft warns and keeps going.
    behavior: Mapped[str] = mapped_column(String(8), default="hard")
    #: Period boundaries are computed in the *owner's* zone, so a "month" ends at their midnight.
    created_at: Mapped[datetime] = _ts(default=now)


class BudgetEntry(Base):
    """One activity's claim on a budget, in credits: reserved when work starts, settled as the ledger lands.

    Without the reservation a long game started with $2 of headroom could quietly spend $40 before
    anything noticed, because cost is only known after the fact.
    """
    __tablename__ = "budget_entries"

    id: Mapped[str] = mapped_column(String(40), primary_key=True, default=lambda: new_id("be_"))
    budget_id: Mapped[str] = mapped_column(ForeignKey("budgets.id", ondelete="CASCADE"), index=True)
    #: Usage-ledger activity id (usage.py), so the estimate can be replaced by the real figure.
    activity_id: Mapped[str] = mapped_column(String(64), index=True)
    user_id: Mapped[Optional[str]] = mapped_column(ForeignKey("users.id", ondelete="SET NULL"))
    reserved: Mapped[float] = mapped_column(Float, default=0.0)
    actual: Mapped[Optional[float]] = mapped_column(Float)
    #: The owner-local period this entry counts against, e.g. "2026-09" — computed once at open
    #: time so a game running across a month boundary does not jump budgets mid-flight.
    period_key: Mapped[str] = mapped_column(String(16), index=True)
    opened_at: Mapped[datetime] = _ts(default=now)
    closed_at: Mapped[Optional[datetime]] = _ts(nullable=True)


Index("ix_budget_entries_budget_period", BudgetEntry.budget_id, BudgetEntry.period_key)


# ============================================================================ shared objects

#: The kinds of owned item (P3.2.4): one `owned_items` row per file-backed job, its files staying where they are.
ITEM_KINDS = ("benchmark_suite", "benchmark_run", "probe", "probe_run", "lab_experiment", "scenario", "map",
              "bot_profile", "report")

#: What an ACL entry may name: a game, a server group, a server, or an owned item by its kind.
OBJECT_TYPES = ("game", "server_group", "server") + ITEM_KINDS
PERMISSIONS = ("view", "play", "manage")

#: A game's index row (P3.2.5): `setup` before its first turn, `playing` while it is played or loaded, `finished`
#: once it is over, `closed` when its session stopped before the end. Only lobby games (`kind = game`) in `setup` or
#: `playing` count against an account's game limit.
GAME_STATUSES = ("setup", "playing", "finished", "closed")


class Game(Base):
    """Index row for a game. The state itself stays in saves/ as it does today; this row carries
    who owns it, who may see it, and whether its data feeds the public averages."""
    __tablename__ = "games"

    id: Mapped[str] = mapped_column(String(40), primary_key=True)     # matches the session id
    owner_id: Mapped[Optional[str]] = mapped_column(ForeignKey("users.id", ondelete="SET NULL"), index=True)
    name: Mapped[str] = mapped_column(String(160), default="")
    kind: Mapped[str] = mapped_column(String(16), default="game")     # game | benchmark | probe | lab | scenario

    #: Who may *watch*. Play access is always the seat holders plus `play` ACL entries, so a private
    #: game between two people can still carry a public spectator link.
    visibility: Mapped[str] = mapped_column(String(16), default="private")
    slug: Mapped[str] = mapped_column(String(48), unique=True, index=True,
                                      default=lambda: secrets.token_urlsafe(16))
    #: pool | private, defaulting from the owner. Filtered at query time, so flipping it
    #: retroactively adds or withdraws this game from every aggregate.
    data_sharing: Mapped[str] = mapped_column(String(16), default="pool")

    status: Mapped[str] = mapped_column(String(16), default="setup")  # GAME_STATUSES
    turn: Mapped[int] = mapped_column(Integer, default=0)
    save_path: Mapped[Optional[str]] = mapped_column(String(512))
    meta: Mapped[dict] = mapped_column(JSON, default=dict)

    created_at: Mapped[datetime] = _ts(default=now)
    updated_at: Mapped[datetime] = _ts(default=now, onupdate=now)
    finished_at: Mapped[Optional[datetime]] = _ts(nullable=True)
    deleted_at: Mapped[Optional[datetime]] = _ts(nullable=True)


class OwnedItem(Base):
    """One file-backed job and its owner (P3.2.4): a benchmark suite or run, a probe or probe run, a lab experiment,
    a scenario, a map, a bot profile or a report. The files stay where they are (`path`, relative to the state
    directory); this row carries who owns the item, who may see it and how it is shared, on the same ladder as a
    game. A report's `scope` (own | pooled | both) and `spec` are in `meta`. `owner_id` is null for the shipped
    items (built-in bot profiles and the like), which only admins manage. `citar/items.py` (package 3-02) is the
    only writer."""
    __tablename__ = "owned_items"
    __table_args__ = (UniqueConstraint("kind", "ref", name="uq_owned_item_kind_ref"),)

    id: Mapped[str] = mapped_column(String(40), primary_key=True, default=lambda: new_id("oi_"))
    kind: Mapped[str] = mapped_column(String(24), index=True)          # ITEM_KINDS
    #: The item's id on disk: a suite or run id, a profile id, an experiment name.
    ref: Mapped[str] = mapped_column(String(120))
    owner_id: Mapped[Optional[str]] = mapped_column(ForeignKey("users.id", ondelete="SET NULL"), index=True)
    name: Mapped[str] = mapped_column(String(160), default="")
    visibility: Mapped[str] = mapped_column(String(16), default="private")
    slug: Mapped[str] = mapped_column(String(48), unique=True, index=True,
                                      default=lambda: secrets.token_urlsafe(16))
    path: Mapped[Optional[str]] = mapped_column(String(512))
    meta: Mapped[dict] = mapped_column(JSON, default=dict)
    #: The server's own data collection (only admins set it): its compute counts as given by whoever's machine
    #: served it, and used by nobody (P3.4.2).
    collection: Mapped[bool] = mapped_column(Boolean, default=False)
    created_at: Mapped[datetime] = _ts(default=now)
    updated_at: Mapped[datetime] = _ts(default=now, onupdate=now)
    deleted_at: Mapped[Optional[datetime]] = _ts(nullable=True)


class AclEntry(Base):
    """`allowlist` membership: one permission for one account on one object."""
    __tablename__ = "acl_entries"
    __table_args__ = (UniqueConstraint("object_type", "object_id", "user_id", "permission",
                                       name="uq_acl_subject_permission"),)

    id: Mapped[str] = mapped_column(String(40), primary_key=True, default=lambda: new_id("acl_"))
    object_type: Mapped[str] = mapped_column(String(24), index=True)
    object_id: Mapped[str] = mapped_column(String(40), index=True)
    user_id: Mapped[str] = mapped_column(ForeignKey("users.id", ondelete="CASCADE"), index=True)
    permission: Mapped[str] = mapped_column(String(16), default="view")
    granted_by: Mapped[Optional[str]] = mapped_column(String(40))
    created_at: Mapped[datetime] = _ts(default=now)


# ============================================================================ the compute ledger

#: What compute an event measures, and which work it was (P3.4.2).
COMPUTE_KINDS = ("gpu", "cpu", "api")
TASK_CLASSES = ("full", "partial", "cpu")
ACTIVITY_KINDS = ("game", "benchmark", "probe", "report", "lab", "test")
#: `pending` until a CPU job settles; `rejected` for a run whose result was refused or withdrawn (P4.5.7): its
#: credits are 0 and any budget settlement it made is reversed.
VERIFICATION = ("unverified", "verified", "pending", "rejected")


class ComputeEvent(Base):
    """One unit of compute, given by a machine's owner and used by an activity's owner (P3.4.2). Written by the
    ledger's writer thread (package 3-10) from the scheduler's model steps and the host's measured CPU; never
    updated except by an admin's reprice.

    `consumer_id` is null for the server's own data collection and `provider_id` for a machine the site provides;
    `self_use` marks the events counted on neither side. The machine is kept by id, not by a foreign key, so an
    event outlives the machine it ran on.
    """
    __tablename__ = "compute_events"

    id: Mapped[int] = mapped_column(Integer, primary_key=True, autoincrement=True)
    #: When the work ended.
    at: Mapped[datetime] = _ts(default=now, index=True)
    #: game:<id>, benchmark_run:<id>, probe_run:<id>, report:<id>, lab:<name> or test:<id>.
    activity: Mapped[str] = mapped_column(String(160), index=True)
    activity_kind: Mapped[str] = mapped_column(String(16))              # ACTIVITY_KINDS
    task_class: Mapped[str] = mapped_column(String(8))                  # TASK_CLASSES
    consumer_id: Mapped[Optional[str]] = mapped_column(ForeignKey("users.id", ondelete="SET NULL"), index=True)
    provider_id: Mapped[Optional[str]] = mapped_column(ForeignKey("users.id", ondelete="SET NULL"), index=True)
    machine_id: Mapped[str] = mapped_column(String(40), index=True)
    kind: Mapped[str] = mapped_column(String(8))                        # COMPUTE_KINDS
    model: Mapped[Optional[str]] = mapped_column(String(200))
    #: Generation seconds, bounded by what the server observed (P3.3.4), and CPU core-seconds.
    gen_s: Mapped[float] = mapped_column(Float, default=0.0)
    cpu_core_s: Mapped[float] = mapped_column(Float, default=0.0)
    #: For a helper's model step: the helper's `Done.duration` and the server's own wall time from request to Done.
    reported_s: Mapped[Optional[float]] = mapped_column(Float)
    observed_s: Mapped[Optional[float]] = mapped_column(Float)
    tokens_in: Mapped[int] = mapped_column(Integer, default=0)
    tokens_out: Mapped[int] = mapped_column(Integer, default=0)
    tokens_reasoning: Mapped[int] = mapped_column(Integer, default=0)
    #: Input tokens the provider read from its prompt cache, and wrote to it (revision 0016b): an online API prices
    #: them apart from the rest of the input, so costing needs them.
    tokens_cache_read: Mapped[int] = mapped_column(Integer, default=0, server_default="0")
    tokens_cache_write: Mapped[int] = mapped_column(Integer, default=0, server_default="0")
    #: Priced when written, from the machine's hardware class of this kind.
    credits: Mapped[float] = mapped_column(Float, default=0.0)
    #: {"overreported", "capped_tokens", "unpriced"}.
    flags: Mapped[dict] = mapped_column(JSON, default=dict)
    self_use: Mapped[bool] = mapped_column(Boolean, default=False)
    verification: Mapped[str] = mapped_column(String(16), default="unverified")   # VERIFICATION
    request_id: Mapped[Optional[str]] = mapped_column(String(64))


ROLLUP_SIDES = ("given", "used")


class ComputeRollup(Base):
    """An hour of one member's given or used compute of one kind (P3.4.5): the sums of `compute_events` for that
    hour, recomputed whole whenever the hour is touched, so a read of complete hours never scans the events."""
    __tablename__ = "compute_rollups"

    user_id: Mapped[str] = mapped_column(ForeignKey("users.id", ondelete="CASCADE"), primary_key=True)
    #: The hour's start, UTC.
    hour: Mapped[datetime] = mapped_column(UTCDateTime(), primary_key=True)
    side: Mapped[str] = mapped_column(String(8), primary_key=True)     # ROLLUP_SIDES
    kind: Mapped[str] = mapped_column(String(8), primary_key=True)     # COMPUTE_KINDS
    gen_s: Mapped[float] = mapped_column(Float, default=0.0)
    cpu_core_s: Mapped[float] = mapped_column(Float, default=0.0)
    credits: Mapped[float] = mapped_column(Float, default=0.0)
    tokens_in: Mapped[int] = mapped_column(Integer, default=0)
    tokens_out: Mapped[int] = mapped_column(Integer, default=0)
    events: Mapped[int] = mapped_column(Integer, default=0)


# ============================================================================ CPU jobs (Phase 4)

#: The kinds of CPU job (design/phase4-helper.md P4.4.1): bot-only work, a closed vocabulary. The same kinds a machine
#: may lend its CPU to (`machine_settings.CPU_WORK`; tests/test_migrations.py holds the two equal).
CPU_JOB_KINDS = ("lab_game", "ranking_game", "reference_game")
#: A job's states (P4.5.4): `settling` while its replicas run, shown only to administrators (P4.5.8 rule 2); a crash
#: outcome is a result (P4.4.2), accepted once verified like any other.
CPU_JOB_STATES = ("queued", "running", "settling", "accepted", "crashed", "failed", "withdrawn", "cancelled")
#: Who ran a run: the server's own job host, or a connected helper (P4.5.6).
CPU_RUN_EXECUTORS = ("local", "remote")
#: Why a run exists: the job's first run, a sampled or required check of it, or the local executor deciding between
#: two that disagree (P4.5.8).
CPU_RUN_ROLES = ("primary", "replica", "tiebreak")
CPU_RUN_STATES = ("offered", "running", "done", "failed", "aborted", "expired")
#: What verification made of a finished run: the local executor's own (`reference`), equal to its replica's, not
#: equal, accepted without a replica, or refused (a bad signature, claim or chain; P4.5.8 rule 5).
CPU_RUN_VERDICTS = ("reference", "match", "mismatch", "unchecked", "rejected")


class CpuJob(Base):
    """One game of a lab, ranking or reference experiment to be played by an executor (P4.5.4): the job the dispatcher
    (package 4-09, its only writer) makes from the lab's queue, offers, and settles from its runs. `spec` is the typed
    spec as data (no path, URL or code; P4.4.1) and `spec_hash` the hash every executor recomputes; `ref` is the
    ledger's activity id (`lab:<exp>:<i>`), `item_kind`/`item_ref` the owned experiment its access follows, and
    `owner_id` the consumer (null for the server's own `collection` experiments). `mem_mb` starts from the cost table
    and is raised to the peak a run reached when memory ended it."""
    __tablename__ = "cpu_jobs"

    id: Mapped[str] = mapped_column(String(40), primary_key=True, default=lambda: new_id("cj_"))
    kind: Mapped[str] = mapped_column(String(16))                       # CPU_JOB_KINDS
    ref: Mapped[str] = mapped_column(String(160), index=True)
    item_kind: Mapped[Optional[str]] = mapped_column(String(24))
    item_ref: Mapped[Optional[str]] = mapped_column(String(120))
    owner_id: Mapped[Optional[str]] = mapped_column(ForeignKey("users.id", ondelete="SET NULL"), index=True)
    spec: Mapped[dict] = mapped_column(JSON, default=dict)
    spec_hash: Mapped[str] = mapped_column(String(64))
    #: The jobs build the job is for (P4.4.5): offered only to executors of that build, compared only within it.
    jobs_build: Mapped[str] = mapped_column(String(16))
    priority: Mapped[int] = mapped_column(Integer, default=0)
    mem_mb: Mapped[int] = mapped_column(Integer, default=0)
    state: Mapped[str] = mapped_column(String(16), default="queued")   # CPU_JOB_STATES
    #: The run whose result the lab took, and that result's hash, once the job settles.
    accepted_run: Mapped[Optional[str]] = mapped_column(String(40))
    result_hash: Mapped[Optional[str]] = mapped_column(String(64))
    attempts: Mapped[int] = mapped_column(Integer, default=0)
    created_at: Mapped[datetime] = _ts(default=now)
    updated_at: Mapped[datetime] = _ts(default=now, onupdate=now)


#: The dispatcher's queue order: queued jobs by priority, then age.
Index("ix_cpu_jobs_state_priority_created", CpuJob.state, CpuJob.priority, CpuJob.created_at)


class CpuJobRun(Base):
    """One run of a job on one executor (P4.5.4): offered, played, and settled with its verdict. The machine is kept by
    id rather than by a foreign key, as the ledger keeps it, so a run stays the evidence of what a machine returned
    after the machine is gone. `result` is pruned once the job settles and `digests` once matched (kept as evidence
    on a mismatch); `claim` and `signature` are the helper's signed claim (P4.4.4), absent for the local executor's
    runs; `error` holds a failure's `{code, message}`, or a mismatch's evidence."""
    __tablename__ = "cpu_job_runs"

    id: Mapped[str] = mapped_column(String(40), primary_key=True, default=lambda: new_id("cr_"))
    job_id: Mapped[str] = mapped_column(ForeignKey("cpu_jobs.id", ondelete="CASCADE"), index=True)
    machine_id: Mapped[str] = mapped_column(String(40), index=True)
    executor: Mapped[str] = mapped_column(String(8), default="remote")     # CPU_RUN_EXECUTORS
    role: Mapped[str] = mapped_column(String(16), default="primary")       # CPU_RUN_ROLES
    state: Mapped[str] = mapped_column(String(16), default="offered", index=True)   # CPU_RUN_STATES
    offered_at: Mapped[datetime] = _ts(default=now)
    started_at: Mapped[Optional[datetime]] = _ts(nullable=True)
    #: The run's heartbeat: a running run silent for 5 minutes while its helper is connected expires (P4.2.5).
    last_progress_at: Mapped[Optional[datetime]] = _ts(nullable=True)
    ended_at: Mapped[Optional[datetime]] = _ts(nullable=True)
    deadline_at: Mapped[Optional[datetime]] = _ts(nullable=True)
    #: The executor's own figures: CPU and wall seconds, seconds paused (which extend the deadline), counted peak MB.
    cpu_s: Mapped[float] = mapped_column(Float, default=0.0)
    wall_s: Mapped[float] = mapped_column(Float, default=0.0)
    paused_s: Mapped[float] = mapped_column(Float, default=0.0)
    peak_mb: Mapped[float] = mapped_column(Float, default=0.0)
    result: Mapped[Optional[dict]] = mapped_column(JSON)
    result_hash: Mapped[Optional[str]] = mapped_column(String(64))
    chain_head: Mapped[Optional[str]] = mapped_column(String(64))
    rounds: Mapped[Optional[int]] = mapped_column(Integer)
    digests: Mapped[Optional[list]] = mapped_column(JSON)
    claim: Mapped[Optional[dict]] = mapped_column(JSON)
    signature: Mapped[Optional[str]] = mapped_column(String(128))
    #: When the server acknowledged the result (`job_result_ack`), so a second copy of it is acknowledged again.
    acked_at: Mapped[Optional[datetime]] = _ts(nullable=True)
    verdict: Mapped[Optional[str]] = mapped_column(String(16))           # CPU_RUN_VERDICTS
    error: Mapped[Optional[dict]] = mapped_column(JSON)


class JobCost(Base):
    """What a kind of job costs (P4.5.4), per kind, map size, seat count and bucket of 50 rounds (`rounds` is the
    bucket's first round): `cpu_s` the median CPU seconds of the local executor's and the verifiers' runs (the
    reference cost every credit is bounded by, P4.5.7) and `peak_mb` the 95th percentile of every run's counted peak
    (the memory a job is offered with). Seeded from a committed table measured on the golden and benchmark jobs
    (package 4-09), so the first jobs already have an estimate."""
    __tablename__ = "job_costs"

    kind: Mapped[str] = mapped_column(String(16), primary_key=True)     # CPU_JOB_KINDS
    size: Mapped[str] = mapped_column(String(24), primary_key=True)
    seats: Mapped[int] = mapped_column(Integer, primary_key=True)
    rounds: Mapped[int] = mapped_column(Integer, primary_key=True)
    cpu_s: Mapped[float] = mapped_column(Float, default=0.0)
    peak_mb: Mapped[float] = mapped_column(Float, default=0.0)
    samples: Mapped[int] = mapped_column(Integer, default=0)
    updated_at: Mapped[datetime] = _ts(default=now, onupdate=now)


# ============================================================================ operations

class AuditLog(Base):
    """Append-only record of anything consequential: logins, role changes, grants, suspensions,
    publishes, deletions. Administrators read it; nobody can edit it through the application."""
    __tablename__ = "audit_log"

    id: Mapped[int] = mapped_column(Integer, primary_key=True, autoincrement=True)
    at: Mapped[datetime] = _ts(default=now, index=True)
    actor_id: Mapped[Optional[str]] = mapped_column(String(40), index=True)
    actor_label: Mapped[str] = mapped_column(String(80), default="")   # survives the account's deletion
    action: Mapped[str] = mapped_column(String(64), index=True)
    object_type: Mapped[Optional[str]] = mapped_column(String(24))
    object_id: Mapped[Optional[str]] = mapped_column(String(64))
    ip: Mapped[Optional[str]] = mapped_column(String(64))
    detail: Mapped[dict] = mapped_column(JSON, default=dict)


class RateLimit(Base):
    """Persistent counters for the unauthenticated routes, so a restart is not a way to reset a
    lockout. In-process counters handle the hot path; this table is the durable backstop."""
    __tablename__ = "rate_limits"

    key: Mapped[str] = mapped_column(String(160), primary_key=True)
    window_start: Mapped[datetime] = _ts(default=now)
    count: Mapped[int] = mapped_column(Integer, default=0)
    blocked_until: Mapped[Optional[datetime]] = _ts(nullable=True)


class Setting(Base):
    """Runtime server settings an admin changes without a restart: registration mode, probation
    policy, per-role capability defaults, the welcome text. Boot defaults come from citar/settings.py."""
    __tablename__ = "settings_kv"

    key: Mapped[str] = mapped_column(String(64), primary_key=True)
    value: Mapped[dict] = mapped_column(JSON, default=dict)
    updated_at: Mapped[datetime] = _ts(default=now, onupdate=now)
    updated_by: Mapped[Optional[str]] = mapped_column(String(40))
