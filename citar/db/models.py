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

#: Per-account permission switches, on top of the coarse role. The defaults for each role live in
#: the runtime settings table so an admin sets policy once rather than per user; a value here is an
#: explicit override for one account.
CAPABILITIES = (
    "register_servers",     # may add inference servers and share them
    "run_reports",          # may build reports — required for private analysis to be useful
    "publish_public",       # may make a game or report readable by the whole internet
    "create_games",         # may start games at all
    "invite",               # may mint invite codes, up to invite_quota
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

    def public(self, *, full: bool = False) -> dict:
        """What the browser sees. `full` is the account's own view or a moderator's."""
        d = {"id": self.id, "handle": self.handle, "display_name": self.name, "role": self.role,
             "avatar_url": self.avatar_url, "status": self.status}
        if full:
            d.update(email=self.email, email_verified=self.email_verified, tz=self.tz,
                     data_sharing=self.data_sharing, caps=dict(self.caps or {}),
                     invite_quota=self.invite_quota, max_concurrent_games=self.max_concurrent_games,
                     max_seats_per_game=self.max_seats_per_game,
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
VISIBILITY = ("private", "allowlist", "link", "public")


class ServerGroup(Base):
    """The unit of sharing. Servers go in groups; grants are made against groups."""
    __tablename__ = "server_groups"

    id: Mapped[str] = mapped_column(String(40), primary_key=True, default=lambda: new_id("sg_"))
    owner_id: Mapped[str] = mapped_column(ForeignKey("users.id", ondelete="CASCADE"), index=True)
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


class Server(Base):
    """One machine or API endpoint that can run a model.

    `config` is the validated dict from citar/servers.py — connection, hardware, power, components,
    cost periods, restricted hours and the model catalog. Keeping it whole means the existing cost
    engine, hardware collector and load-profile code keep working untouched.
    """
    __tablename__ = "servers"

    id: Mapped[str] = mapped_column(String(40), primary_key=True, default=lambda: new_id("sv_"))
    owner_id: Mapped[str] = mapped_column(ForeignKey("users.id", ondelete="CASCADE"), index=True)
    group_id: Mapped[Optional[str]] = mapped_column(ForeignKey("server_groups.id", ondelete="SET NULL"), index=True)
    name: Mapped[str] = mapped_column(String(120))
    kind: Mapped[str] = mapped_column(String(16), default="owned")      # owned | leased | api | test
    provider: Mapped[str] = mapped_column(String(32), default="none")
    config: Mapped[dict] = mapped_column(JSON, default=dict)

    #: How this server is reached. `worker` means an agent dials out to us and we never connect in;
    #: `direct` means CITAR opens a connection to base_url itself (only sane on a trusted network).
    reach: Mapped[str] = mapped_column(String(16), default="worker")
    enabled: Mapped[bool] = mapped_column(Boolean, default=True)
    max_concurrent: Mapped[int] = mapped_column(Integer, default=1)

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


class Budget(Base):
    """A spend cap on a grant. Accounting and admission control only — no money moves anywhere.

    Spend is computed from the usage ledger priced by citar/costing.py, so correcting an electricity
    rate or a token price later corrects every budget that was measured with it.
    """
    __tablename__ = "budgets"

    id: Mapped[str] = mapped_column(String(40), primary_key=True, default=lambda: new_id("b_"))
    grant_id: Mapped[str] = mapped_column(ForeignKey("access_grants.id", ondelete="CASCADE"), index=True)
    period: Mapped[str] = mapped_column(String(12), default="month")
    amount: Mapped[float] = mapped_column(Float, default=0.0)
    currency: Mapped[str] = mapped_column(String(8), default="USD")
    #: hard refuses to start work that would exceed the cap and pauses work that crosses it;
    #: soft warns and keeps going.
    behavior: Mapped[str] = mapped_column(String(8), default="hard")
    #: Period boundaries are computed in the *owner's* zone, so a "month" ends at their midnight.
    created_at: Mapped[datetime] = _ts(default=now)


class BudgetEntry(Base):
    """One activity's claim on a budget: reserved at launch, trued up as the usage ledger lands.

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

OBJECT_TYPES = ("game", "report", "dataset", "server_group", "scenario", "map", "probe")
PERMISSIONS = ("view", "play", "manage")


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

    status: Mapped[str] = mapped_column(String(16), default="setup")  # setup | playing | finished | abandoned
    turn: Mapped[int] = mapped_column(Integer, default=0)
    save_path: Mapped[Optional[str]] = mapped_column(String(512))
    meta: Mapped[dict] = mapped_column(JSON, default=dict)

    created_at: Mapped[datetime] = _ts(default=now)
    updated_at: Mapped[datetime] = _ts(default=now, onupdate=now)
    finished_at: Mapped[Optional[datetime]] = _ts(nullable=True)
    deleted_at: Mapped[Optional[datetime]] = _ts(nullable=True)


class Report(Base):
    """A generated report. Same visibility ladder as games, set independently of the games it
    draws on, so an analysis can be published while its source games stay private."""
    __tablename__ = "reports"

    id: Mapped[str] = mapped_column(String(40), primary_key=True, default=lambda: new_id("r_"))
    owner_id: Mapped[Optional[str]] = mapped_column(ForeignKey("users.id", ondelete="SET NULL"), index=True)
    name: Mapped[str] = mapped_column(String(160), default="")
    visibility: Mapped[str] = mapped_column(String(16), default="private")
    slug: Mapped[str] = mapped_column(String(48), unique=True, index=True,
                                      default=lambda: secrets.token_urlsafe(16))
    #: Whether this report was built from pooled data only, or includes the owner's private games.
    scope: Mapped[str] = mapped_column(String(16), default="own")     # own | pooled | both
    spec: Mapped[dict] = mapped_column(JSON, default=dict)
    path: Mapped[Optional[str]] = mapped_column(String(512))
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


# ============================================================================ operations

class AuditLog(Base):
    """Append-only record of anything consequential: logins, role changes, grants, suspensions,
    publishes, deletions. Moderators can read it; nobody can edit it through the application."""
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
