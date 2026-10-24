"""The 0.1.6 baseline: every table and column Phase 3 needs, in one revision.

Revision ID: 0016a_baseline
Revises:
Created: 2026-10-11

Replaces the two revisions of 0.1.5 and Phases 0-2 (f064316a6b34, the initial schema, and 767c824c2fcb, the
per-account seat limit). Written by hand from `citar/db/models.py` (design/phase3-features.md P3.2.2), and checked
against it by tests/test_migrations.py: an upgrade to head followed by alembic's `compare_metadata` finds nothing.
It holds every table and column Phase 3's packages fill in, so they add behaviour rather than migrations: the
profile columns on `users`, `owned_items` (which absorbs `reports`), the games' statuses, the machine columns on
`servers` (whose owner, like a group's, is nullable and set to null when the account goes), `hardware_classes` with
its three defaults, `compute_events` and `compute_rollups`, and budgets in credits.

A database at one of the old revisions is not migrated: the boot renames it aside and starts on a fresh one
(`citar.server.boot.migrate`). A later change to the schema is a revision on top of this one, with an upgrade test
from it; nothing re-squashes.
"""
from __future__ import annotations

from datetime import datetime, timezone
from typing import Sequence, Union

from alembic import context, op
import sqlalchemy as sa


revision: str = "0016a_baseline"
down_revision: Union[str, None] = None
branch_labels: Union[str, Sequence[str], None] = None
depends_on: Union[str, Sequence[str], None] = None

#: The default hardware classes (P3.4.3), written out rather than imported: a revision must keep creating what it
#: created when it was written. tests/test_migrations.py checks them against `models.HARDWARE_CLASS_SEEDS`.
HARDWARE_CLASSES = (
    {"id": "hc_gpu", "kind": "gpu", "name": "GPU (unclassified)", "rate": 1.0},
    {"id": "hc_cpu", "kind": "cpu", "name": "CPU core (unclassified)", "rate": 0.05},
    {"id": "hc_api", "kind": "api", "name": "Online API", "rate": 1000.0},
)


def upgrade() -> None:
    """Create every table, then seed the default hardware classes."""
    op.create_table('audit_log',
        sa.Column('id', sa.Integer(), autoincrement=True, nullable=False),
        sa.Column('at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('actor_id', sa.String(length=40), nullable=True),
        sa.Column('actor_label', sa.String(length=80), nullable=False),
        sa.Column('action', sa.String(length=64), nullable=False),
        sa.Column('object_type', sa.String(length=24), nullable=True),
        sa.Column('object_id', sa.String(length=64), nullable=True),
        sa.Column('ip', sa.String(length=64), nullable=True),
        sa.Column('detail', sa.JSON(), nullable=False),
        sa.PrimaryKeyConstraint('id')
    )
    op.create_index(op.f('ix_audit_log_action'), 'audit_log', ['action'], unique=False)
    op.create_index(op.f('ix_audit_log_actor_id'), 'audit_log', ['actor_id'], unique=False)
    op.create_index(op.f('ix_audit_log_at'), 'audit_log', ['at'], unique=False)
    op.create_table('hardware_classes',
        sa.Column('id', sa.String(length=40), nullable=False),
        sa.Column('kind', sa.String(length=8), nullable=False),
        sa.Column('name', sa.String(length=80), nullable=False),
        sa.Column('rate', sa.Float(), nullable=False),
        sa.Column('match', sa.JSON(), nullable=False),
        sa.Column('priority', sa.Integer(), nullable=False),
        sa.Column('is_default', sa.Boolean(), nullable=False),
        sa.Column('updated_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('updated_by', sa.String(length=40), nullable=True),
        sa.PrimaryKeyConstraint('id')
    )
    op.create_table('rate_limits',
        sa.Column('key', sa.String(length=160), nullable=False),
        sa.Column('window_start', sa.DateTime(timezone=True), nullable=False),
        sa.Column('count', sa.Integer(), nullable=False),
        sa.Column('blocked_until', sa.DateTime(timezone=True), nullable=True),
        sa.PrimaryKeyConstraint('key')
    )
    op.create_table('settings_kv',
        sa.Column('key', sa.String(length=64), nullable=False),
        sa.Column('value', sa.JSON(), nullable=False),
        sa.Column('updated_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('updated_by', sa.String(length=40), nullable=True),
        sa.PrimaryKeyConstraint('key')
    )
    op.create_table('users',
        sa.Column('id', sa.String(length=40), nullable=False),
        sa.Column('handle', sa.String(length=40), nullable=False),
        sa.Column('handle_lower', sa.String(length=40), nullable=False),
        sa.Column('display_name', sa.String(length=80), nullable=False),
        sa.Column('email', sa.String(length=320), nullable=True),
        sa.Column('email_lower', sa.String(length=320), nullable=True),
        sa.Column('email_verified', sa.Boolean(), nullable=False),
        sa.Column('password_hash', sa.String(length=255), nullable=True),
        sa.Column('role', sa.String(length=16), nullable=False),
        sa.Column('status', sa.String(length=16), nullable=False),
        sa.Column('tz', sa.String(length=64), nullable=False),
        sa.Column('locale', sa.String(length=16), nullable=False),
        sa.Column('avatar_url', sa.String(length=512), nullable=True),
        sa.Column('data_sharing', sa.String(length=16), nullable=False),
        sa.Column('caps', sa.JSON(), nullable=False),
        sa.Column('invite_quota', sa.Integer(), nullable=False),
        sa.Column('max_concurrent_games', sa.Integer(), nullable=False),
        sa.Column('max_seats_per_game', sa.Integer(), nullable=False),
        sa.Column('is_local_owner', sa.Boolean(), nullable=False),
        sa.Column('created_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('last_seen_at', sa.DateTime(timezone=True), nullable=True),
        sa.Column('suspended_at', sa.DateTime(timezone=True), nullable=True),
        sa.Column('suspended_by', sa.String(length=40), nullable=True),
        sa.Column('suspended_reason', sa.Text(), nullable=True),
        sa.Column('invited_by', sa.String(length=40), nullable=True),
        sa.Column('bio', sa.Text(), nullable=False),
        sa.Column('links', sa.JSON(), nullable=False),
        sa.Column('avatar_path', sa.String(length=200), nullable=True),
        sa.Column('field_visibility', sa.JSON(), nullable=False),
        sa.Column('listed', sa.Boolean(), nullable=False),
        sa.Column('trust_tier', sa.String(length=16), nullable=False),
        sa.PrimaryKeyConstraint('id')
    )
    op.create_index(op.f('ix_users_email_lower'), 'users', ['email_lower'], unique=True)
    op.create_index(op.f('ix_users_handle_lower'), 'users', ['handle_lower'], unique=True)
    op.create_table('acl_entries',
        sa.Column('id', sa.String(length=40), nullable=False),
        sa.Column('object_type', sa.String(length=24), nullable=False),
        sa.Column('object_id', sa.String(length=40), nullable=False),
        sa.Column('user_id', sa.String(length=40), nullable=False),
        sa.Column('permission', sa.String(length=16), nullable=False),
        sa.Column('granted_by', sa.String(length=40), nullable=True),
        sa.Column('created_at', sa.DateTime(timezone=True), nullable=False),
        sa.ForeignKeyConstraint(['user_id'], ['users.id'], ondelete='CASCADE'),
        sa.PrimaryKeyConstraint('id'),
        sa.UniqueConstraint('object_type', 'object_id', 'user_id', 'permission', name='uq_acl_subject_permission')
    )
    op.create_index(op.f('ix_acl_entries_object_id'), 'acl_entries', ['object_id'], unique=False)
    op.create_index(op.f('ix_acl_entries_object_type'), 'acl_entries', ['object_type'], unique=False)
    op.create_index(op.f('ix_acl_entries_user_id'), 'acl_entries', ['user_id'], unique=False)
    op.create_table('auth_sessions',
        sa.Column('id', sa.String(length=40), nullable=False),
        sa.Column('user_id', sa.String(length=40), nullable=False),
        sa.Column('token_hash', sa.String(length=64), nullable=False),
        sa.Column('csrf_token', sa.String(length=64), nullable=False),
        sa.Column('created_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('last_seen_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('expires_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('revoked_at', sa.DateTime(timezone=True), nullable=True),
        sa.Column('ip', sa.String(length=64), nullable=True),
        sa.Column('user_agent', sa.String(length=300), nullable=True),
        sa.ForeignKeyConstraint(['user_id'], ['users.id'], ondelete='CASCADE'),
        sa.PrimaryKeyConstraint('id')
    )
    op.create_index(op.f('ix_auth_sessions_token_hash'), 'auth_sessions', ['token_hash'], unique=True)
    op.create_index(op.f('ix_auth_sessions_user_id'), 'auth_sessions', ['user_id'], unique=False)
    op.create_table('compute_events',
        sa.Column('id', sa.Integer(), autoincrement=True, nullable=False),
        sa.Column('at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('activity', sa.String(length=160), nullable=False),
        sa.Column('activity_kind', sa.String(length=16), nullable=False),
        sa.Column('task_class', sa.String(length=8), nullable=False),
        sa.Column('consumer_id', sa.String(length=40), nullable=True),
        sa.Column('provider_id', sa.String(length=40), nullable=True),
        sa.Column('machine_id', sa.String(length=40), nullable=False),
        sa.Column('kind', sa.String(length=8), nullable=False),
        sa.Column('model', sa.String(length=200), nullable=True),
        sa.Column('gen_s', sa.Float(), nullable=False),
        sa.Column('cpu_core_s', sa.Float(), nullable=False),
        sa.Column('reported_s', sa.Float(), nullable=True),
        sa.Column('observed_s', sa.Float(), nullable=True),
        sa.Column('tokens_in', sa.Integer(), nullable=False),
        sa.Column('tokens_out', sa.Integer(), nullable=False),
        sa.Column('tokens_reasoning', sa.Integer(), nullable=False),
        sa.Column('credits', sa.Float(), nullable=False),
        sa.Column('flags', sa.JSON(), nullable=False),
        sa.Column('self_use', sa.Boolean(), nullable=False),
        sa.Column('verification', sa.String(length=16), nullable=False),
        sa.Column('request_id', sa.String(length=64), nullable=True),
        sa.ForeignKeyConstraint(['consumer_id'], ['users.id'], ondelete='SET NULL'),
        sa.ForeignKeyConstraint(['provider_id'], ['users.id'], ondelete='SET NULL'),
        sa.PrimaryKeyConstraint('id')
    )
    op.create_index(op.f('ix_compute_events_activity'), 'compute_events', ['activity'], unique=False)
    op.create_index(op.f('ix_compute_events_at'), 'compute_events', ['at'], unique=False)
    op.create_index(op.f('ix_compute_events_consumer_id'), 'compute_events', ['consumer_id'], unique=False)
    op.create_index(op.f('ix_compute_events_machine_id'), 'compute_events', ['machine_id'], unique=False)
    op.create_index(op.f('ix_compute_events_provider_id'), 'compute_events', ['provider_id'], unique=False)
    op.create_table('compute_rollups',
        sa.Column('user_id', sa.String(length=40), nullable=False),
        sa.Column('hour', sa.DateTime(timezone=True), nullable=False),
        sa.Column('side', sa.String(length=8), nullable=False),
        sa.Column('kind', sa.String(length=8), nullable=False),
        sa.Column('gen_s', sa.Float(), nullable=False),
        sa.Column('cpu_core_s', sa.Float(), nullable=False),
        sa.Column('credits', sa.Float(), nullable=False),
        sa.Column('tokens_in', sa.Integer(), nullable=False),
        sa.Column('tokens_out', sa.Integer(), nullable=False),
        sa.Column('events', sa.Integer(), nullable=False),
        sa.ForeignKeyConstraint(['user_id'], ['users.id'], ondelete='CASCADE'),
        sa.PrimaryKeyConstraint('user_id', 'hour', 'side', 'kind')
    )
    op.create_table('email_tokens',
        sa.Column('id', sa.String(length=40), nullable=False),
        sa.Column('user_id', sa.String(length=40), nullable=False),
        sa.Column('purpose', sa.String(length=24), nullable=False),
        sa.Column('token_hash', sa.String(length=64), nullable=False),
        sa.Column('new_email', sa.String(length=320), nullable=True),
        sa.Column('created_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('expires_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('used_at', sa.DateTime(timezone=True), nullable=True),
        sa.ForeignKeyConstraint(['user_id'], ['users.id'], ondelete='CASCADE'),
        sa.PrimaryKeyConstraint('id')
    )
    op.create_index(op.f('ix_email_tokens_token_hash'), 'email_tokens', ['token_hash'], unique=True)
    op.create_index(op.f('ix_email_tokens_user_id'), 'email_tokens', ['user_id'], unique=False)
    op.create_table('games',
        sa.Column('id', sa.String(length=40), nullable=False),
        sa.Column('owner_id', sa.String(length=40), nullable=True),
        sa.Column('name', sa.String(length=160), nullable=False),
        sa.Column('kind', sa.String(length=16), nullable=False),
        sa.Column('visibility', sa.String(length=16), nullable=False),
        sa.Column('slug', sa.String(length=48), nullable=False),
        sa.Column('data_sharing', sa.String(length=16), nullable=False),
        sa.Column('status', sa.String(length=16), nullable=False),
        sa.Column('turn', sa.Integer(), nullable=False),
        sa.Column('save_path', sa.String(length=512), nullable=True),
        sa.Column('meta', sa.JSON(), nullable=False),
        sa.Column('created_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('updated_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('finished_at', sa.DateTime(timezone=True), nullable=True),
        sa.Column('deleted_at', sa.DateTime(timezone=True), nullable=True),
        sa.ForeignKeyConstraint(['owner_id'], ['users.id'], ondelete='SET NULL'),
        sa.PrimaryKeyConstraint('id')
    )
    op.create_index(op.f('ix_games_owner_id'), 'games', ['owner_id'], unique=False)
    op.create_index(op.f('ix_games_slug'), 'games', ['slug'], unique=True)
    op.create_table('identities',
        sa.Column('id', sa.String(length=40), nullable=False),
        sa.Column('user_id', sa.String(length=40), nullable=False),
        sa.Column('provider', sa.String(length=32), nullable=False),
        sa.Column('provider_user_id', sa.String(length=255), nullable=False),
        sa.Column('email', sa.String(length=320), nullable=True),
        sa.Column('email_verified', sa.Boolean(), nullable=False),
        sa.Column('display_name', sa.String(length=120), nullable=True),
        sa.Column('avatar_url', sa.String(length=512), nullable=True),
        sa.Column('created_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('last_login_at', sa.DateTime(timezone=True), nullable=True),
        sa.ForeignKeyConstraint(['user_id'], ['users.id'], ondelete='CASCADE'),
        sa.PrimaryKeyConstraint('id'),
        sa.UniqueConstraint('provider', 'provider_user_id', name='uq_identity_provider_subject')
    )
    op.create_index(op.f('ix_identities_user_id'), 'identities', ['user_id'], unique=False)
    op.create_table('invites',
        sa.Column('id', sa.String(length=40), nullable=False),
        sa.Column('code', sa.String(length=64), nullable=False),
        sa.Column('created_by', sa.String(length=40), nullable=True),
        sa.Column('role', sa.String(length=16), nullable=False),
        sa.Column('email', sa.String(length=320), nullable=True),
        sa.Column('max_uses', sa.Integer(), nullable=False),
        sa.Column('uses', sa.Integer(), nullable=False),
        sa.Column('skip_probation', sa.Boolean(), nullable=False),
        sa.Column('note', sa.Text(), nullable=True),
        sa.Column('created_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('expires_at', sa.DateTime(timezone=True), nullable=True),
        sa.Column('revoked_at', sa.DateTime(timezone=True), nullable=True),
        sa.ForeignKeyConstraint(['created_by'], ['users.id'], ondelete='SET NULL'),
        sa.PrimaryKeyConstraint('id')
    )
    op.create_index(op.f('ix_invites_code'), 'invites', ['code'], unique=True)
    op.create_table('owned_items',
        sa.Column('id', sa.String(length=40), nullable=False),
        sa.Column('kind', sa.String(length=24), nullable=False),
        sa.Column('ref', sa.String(length=120), nullable=False),
        sa.Column('owner_id', sa.String(length=40), nullable=True),
        sa.Column('name', sa.String(length=160), nullable=False),
        sa.Column('visibility', sa.String(length=16), nullable=False),
        sa.Column('slug', sa.String(length=48), nullable=False),
        sa.Column('path', sa.String(length=512), nullable=True),
        sa.Column('meta', sa.JSON(), nullable=False),
        sa.Column('collection', sa.Boolean(), nullable=False),
        sa.Column('created_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('updated_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('deleted_at', sa.DateTime(timezone=True), nullable=True),
        sa.ForeignKeyConstraint(['owner_id'], ['users.id'], ondelete='SET NULL'),
        sa.PrimaryKeyConstraint('id'),
        sa.UniqueConstraint('kind', 'ref', name='uq_owned_item_kind_ref')
    )
    op.create_index(op.f('ix_owned_items_kind'), 'owned_items', ['kind'], unique=False)
    op.create_index(op.f('ix_owned_items_owner_id'), 'owned_items', ['owner_id'], unique=False)
    op.create_index(op.f('ix_owned_items_slug'), 'owned_items', ['slug'], unique=True)
    op.create_table('server_groups',
        sa.Column('id', sa.String(length=40), nullable=False),
        sa.Column('owner_id', sa.String(length=40), nullable=True),
        sa.Column('name', sa.String(length=120), nullable=False),
        sa.Column('description', sa.Text(), nullable=False),
        sa.Column('visibility', sa.String(length=16), nullable=False),
        sa.Column('slug', sa.String(length=48), nullable=False),
        sa.Column('accepts_requests', sa.Boolean(), nullable=False),
        sa.Column('created_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('archived_at', sa.DateTime(timezone=True), nullable=True),
        sa.ForeignKeyConstraint(['owner_id'], ['users.id'], ondelete='SET NULL'),
        sa.PrimaryKeyConstraint('id')
    )
    op.create_index(op.f('ix_server_groups_owner_id'), 'server_groups', ['owner_id'], unique=False)
    op.create_index(op.f('ix_server_groups_slug'), 'server_groups', ['slug'], unique=True)
    op.create_table('access_grants',
        sa.Column('id', sa.String(length=40), nullable=False),
        sa.Column('group_id', sa.String(length=40), nullable=False),
        sa.Column('subject_type', sa.String(length=16), nullable=False),
        sa.Column('subject_user_id', sa.String(length=40), nullable=True),
        sa.Column('purposes', sa.JSON(), nullable=False),
        sa.Column('concurrency', sa.Integer(), nullable=False),
        sa.Column('priority', sa.Integer(), nullable=False),
        sa.Column('enabled', sa.Boolean(), nullable=False),
        sa.Column('note', sa.Text(), nullable=True),
        sa.Column('created_by', sa.String(length=40), nullable=True),
        sa.Column('created_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('expires_at', sa.DateTime(timezone=True), nullable=True),
        sa.ForeignKeyConstraint(['group_id'], ['server_groups.id'], ondelete='CASCADE'),
        sa.ForeignKeyConstraint(['subject_user_id'], ['users.id'], ondelete='CASCADE'),
        sa.PrimaryKeyConstraint('id')
    )
    op.create_index(op.f('ix_access_grants_group_id'), 'access_grants', ['group_id'], unique=False)
    op.create_index(op.f('ix_access_grants_subject_user_id'), 'access_grants', ['subject_user_id'], unique=False)
    op.create_table('servers',
        sa.Column('id', sa.String(length=40), nullable=False),
        sa.Column('owner_id', sa.String(length=40), nullable=True),
        sa.Column('group_id', sa.String(length=40), nullable=True),
        sa.Column('name', sa.String(length=120), nullable=False),
        sa.Column('kind', sa.String(length=16), nullable=False),
        sa.Column('provider', sa.String(length=32), nullable=False),
        sa.Column('config', sa.JSON(), nullable=False),
        sa.Column('reach', sa.String(length=16), nullable=False),
        sa.Column('enabled', sa.Boolean(), nullable=False),
        sa.Column('max_concurrent', sa.Integer(), nullable=False),
        sa.Column('is_host', sa.Boolean(), nullable=False),
        sa.Column('server_resource', sa.Boolean(), nullable=False),
        sa.Column('capabilities', sa.JSON(), nullable=False),
        sa.Column('hardware_mode', sa.String(length=8), nullable=False),
        sa.Column('models_mode', sa.String(length=8), nullable=False),
        sa.Column('power_mode', sa.String(length=8), nullable=False),
        sa.Column('hardware_detected', sa.JSON(), nullable=True),
        sa.Column('detected_at', sa.DateTime(timezone=True), nullable=True),
        sa.Column('gpu_class_id', sa.String(length=40), nullable=True),
        sa.Column('cpu_class_id', sa.String(length=40), nullable=True),
        sa.Column('class_override', sa.Boolean(), nullable=False),
        sa.Column('trust_tier', sa.String(length=16), nullable=False),
        sa.Column('flags', sa.JSON(), nullable=False),
        sa.Column('created_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('updated_at', sa.DateTime(timezone=True), nullable=False),
        sa.ForeignKeyConstraint(['cpu_class_id'], ['hardware_classes.id'], ondelete='SET NULL'),
        sa.ForeignKeyConstraint(['gpu_class_id'], ['hardware_classes.id'], ondelete='SET NULL'),
        sa.ForeignKeyConstraint(['group_id'], ['server_groups.id'], ondelete='SET NULL'),
        sa.ForeignKeyConstraint(['owner_id'], ['users.id'], ondelete='SET NULL'),
        sa.PrimaryKeyConstraint('id')
    )
    op.create_index(op.f('ix_servers_group_id'), 'servers', ['group_id'], unique=False)
    op.create_index(op.f('ix_servers_owner_id'), 'servers', ['owner_id'], unique=False)
    op.create_table('availability_windows',
        sa.Column('id', sa.String(length=40), nullable=False),
        sa.Column('grant_id', sa.String(length=40), nullable=False),
        sa.Column('weekday', sa.Integer(), nullable=False),
        sa.Column('start_min', sa.Integer(), nullable=False),
        sa.Column('end_min', sa.Integer(), nullable=False),
        sa.ForeignKeyConstraint(['grant_id'], ['access_grants.id'], ondelete='CASCADE'),
        sa.PrimaryKeyConstraint('id')
    )
    op.create_index(op.f('ix_availability_windows_grant_id'), 'availability_windows', ['grant_id'], unique=False)
    op.create_table('budgets',
        sa.Column('id', sa.String(length=40), nullable=False),
        sa.Column('grant_id', sa.String(length=40), nullable=False),
        sa.Column('period', sa.String(length=12), nullable=False),
        sa.Column('amount', sa.Float(), nullable=False),
        sa.Column('unit', sa.String(length=8), nullable=False),
        sa.Column('behavior', sa.String(length=8), nullable=False),
        sa.Column('created_at', sa.DateTime(timezone=True), nullable=False),
        sa.ForeignKeyConstraint(['grant_id'], ['access_grants.id'], ondelete='CASCADE'),
        sa.PrimaryKeyConstraint('id')
    )
    op.create_index(op.f('ix_budgets_grant_id'), 'budgets', ['grant_id'], unique=False)
    op.create_table('worker_tokens',
        sa.Column('id', sa.String(length=40), nullable=False),
        sa.Column('server_id', sa.String(length=40), nullable=False),
        sa.Column('token_hash', sa.String(length=64), nullable=False),
        sa.Column('prefix', sa.String(length=12), nullable=False),
        sa.Column('label', sa.String(length=80), nullable=False),
        sa.Column('created_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('created_by', sa.String(length=40), nullable=True),
        sa.Column('last_seen_at', sa.DateTime(timezone=True), nullable=True),
        sa.Column('last_ip', sa.String(length=64), nullable=True),
        sa.Column('revoked_at', sa.DateTime(timezone=True), nullable=True),
        sa.ForeignKeyConstraint(['server_id'], ['servers.id'], ondelete='CASCADE'),
        sa.PrimaryKeyConstraint('id')
    )
    op.create_index(op.f('ix_worker_tokens_server_id'), 'worker_tokens', ['server_id'], unique=False)
    op.create_index(op.f('ix_worker_tokens_token_hash'), 'worker_tokens', ['token_hash'], unique=True)
    op.create_table('budget_entries',
        sa.Column('id', sa.String(length=40), nullable=False),
        sa.Column('budget_id', sa.String(length=40), nullable=False),
        sa.Column('activity_id', sa.String(length=64), nullable=False),
        sa.Column('user_id', sa.String(length=40), nullable=True),
        sa.Column('reserved', sa.Float(), nullable=False),
        sa.Column('actual', sa.Float(), nullable=True),
        sa.Column('period_key', sa.String(length=16), nullable=False),
        sa.Column('opened_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('closed_at', sa.DateTime(timezone=True), nullable=True),
        sa.ForeignKeyConstraint(['budget_id'], ['budgets.id'], ondelete='CASCADE'),
        sa.ForeignKeyConstraint(['user_id'], ['users.id'], ondelete='SET NULL'),
        sa.PrimaryKeyConstraint('id')
    )
    op.create_index(op.f('ix_budget_entries_activity_id'), 'budget_entries', ['activity_id'], unique=False)
    op.create_index(op.f('ix_budget_entries_budget_id'), 'budget_entries', ['budget_id'], unique=False)
    op.create_index('ix_budget_entries_budget_period', 'budget_entries', ['budget_id', 'period_key'], unique=False)
    op.create_index(op.f('ix_budget_entries_period_key'), 'budget_entries', ['period_key'], unique=False)

    if context.is_offline_mode():
        # SQL to review (`alembic upgrade head --sql`): a JSON value has no literal form, so the rows are written
        # out as SQL both SQLite and Postgres take
        for c in HARDWARE_CLASSES:
            op.execute(f"INSERT INTO hardware_classes (id, kind, name, rate, match, priority, is_default, updated_at, "
                       f"updated_by) VALUES ('{c['id']}', '{c['kind']}', '{c['name']}', {c['rate']!r}, '{{}}', 0, "
                       f"true, CURRENT_TIMESTAMP, NULL)")
        return
    classes = sa.table(
        "hardware_classes", sa.column("id", sa.String), sa.column("kind", sa.String), sa.column("name", sa.String),
        sa.column("rate", sa.Float), sa.column("match", sa.JSON), sa.column("priority", sa.Integer),
        sa.column("is_default", sa.Boolean), sa.column("updated_at", sa.DateTime(timezone=True)),
        sa.column("updated_by", sa.String))
    stamp = datetime.now(timezone.utc)
    op.bulk_insert(classes, [{**c, "match": {}, "priority": 0, "is_default": True, "updated_at": stamp,
                              "updated_by": None} for c in HARDWARE_CLASSES])


def downgrade() -> None:
    """Drop everything, in the reverse order of the upgrade."""
    op.drop_index(op.f('ix_budget_entries_period_key'), table_name='budget_entries')
    op.drop_index('ix_budget_entries_budget_period', table_name='budget_entries')
    op.drop_index(op.f('ix_budget_entries_budget_id'), table_name='budget_entries')
    op.drop_index(op.f('ix_budget_entries_activity_id'), table_name='budget_entries')
    op.drop_table('budget_entries')
    op.drop_index(op.f('ix_worker_tokens_token_hash'), table_name='worker_tokens')
    op.drop_index(op.f('ix_worker_tokens_server_id'), table_name='worker_tokens')
    op.drop_table('worker_tokens')
    op.drop_index(op.f('ix_budgets_grant_id'), table_name='budgets')
    op.drop_table('budgets')
    op.drop_index(op.f('ix_availability_windows_grant_id'), table_name='availability_windows')
    op.drop_table('availability_windows')
    op.drop_index(op.f('ix_servers_owner_id'), table_name='servers')
    op.drop_index(op.f('ix_servers_group_id'), table_name='servers')
    op.drop_table('servers')
    op.drop_index(op.f('ix_access_grants_subject_user_id'), table_name='access_grants')
    op.drop_index(op.f('ix_access_grants_group_id'), table_name='access_grants')
    op.drop_table('access_grants')
    op.drop_index(op.f('ix_server_groups_slug'), table_name='server_groups')
    op.drop_index(op.f('ix_server_groups_owner_id'), table_name='server_groups')
    op.drop_table('server_groups')
    op.drop_index(op.f('ix_owned_items_slug'), table_name='owned_items')
    op.drop_index(op.f('ix_owned_items_owner_id'), table_name='owned_items')
    op.drop_index(op.f('ix_owned_items_kind'), table_name='owned_items')
    op.drop_table('owned_items')
    op.drop_index(op.f('ix_invites_code'), table_name='invites')
    op.drop_table('invites')
    op.drop_index(op.f('ix_identities_user_id'), table_name='identities')
    op.drop_table('identities')
    op.drop_index(op.f('ix_games_slug'), table_name='games')
    op.drop_index(op.f('ix_games_owner_id'), table_name='games')
    op.drop_table('games')
    op.drop_index(op.f('ix_email_tokens_user_id'), table_name='email_tokens')
    op.drop_index(op.f('ix_email_tokens_token_hash'), table_name='email_tokens')
    op.drop_table('email_tokens')
    op.drop_table('compute_rollups')
    op.drop_index(op.f('ix_compute_events_provider_id'), table_name='compute_events')
    op.drop_index(op.f('ix_compute_events_machine_id'), table_name='compute_events')
    op.drop_index(op.f('ix_compute_events_consumer_id'), table_name='compute_events')
    op.drop_index(op.f('ix_compute_events_at'), table_name='compute_events')
    op.drop_index(op.f('ix_compute_events_activity'), table_name='compute_events')
    op.drop_table('compute_events')
    op.drop_index(op.f('ix_auth_sessions_user_id'), table_name='auth_sessions')
    op.drop_index(op.f('ix_auth_sessions_token_hash'), table_name='auth_sessions')
    op.drop_table('auth_sessions')
    op.drop_index(op.f('ix_acl_entries_user_id'), table_name='acl_entries')
    op.drop_index(op.f('ix_acl_entries_object_type'), table_name='acl_entries')
    op.drop_index(op.f('ix_acl_entries_object_id'), table_name='acl_entries')
    op.drop_table('acl_entries')
    op.drop_index(op.f('ix_users_handle_lower'), table_name='users')
    op.drop_index(op.f('ix_users_email_lower'), table_name='users')
    op.drop_table('users')
    op.drop_table('settings_kv')
    op.drop_table('rate_limits')
    op.drop_table('hardware_classes')
    op.drop_index(op.f('ix_audit_log_at'), table_name='audit_log')
    op.drop_index(op.f('ix_audit_log_actor_id'), table_name='audit_log')
    op.drop_index(op.f('ix_audit_log_action'), table_name='audit_log')
    op.drop_table('audit_log')
