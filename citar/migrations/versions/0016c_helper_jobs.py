"""Phase 4's schema: the CPU job store, the cost table, and the helper's columns on the machine row.

Revision ID: 0016c_helper_jobs
Revises: 0016b_compute_cache_tokens
Created: 2026-10-14

Package 4-02 lands every table and column Phase 4 needs in one revision, schema first (design/phase4-helper.md
P4.5.4, as Phase 3's baseline did), so the later packages add behaviour rather than migrations:
- `cpu_jobs`, one row per game of a lab, ranking or reference experiment the dispatcher (4-09) offers to executors,
  indexed for its queue (state, priority, age) and by the ledger's activity id;
- `cpu_job_runs`, one row per run of a job on an executor: its figures, its result, chain and signed claim, and the
  verdict verification (4-11) gives it;
- `job_costs`, the reference cost and memory of a kind of job per map size, seat count and bucket of 50 rounds (seeded
  by 4-09);
- on `servers`, the machine key its helper registered (`public_key`, `key_at`, trust on first use, P4.3.7), the last
  hello's helper block (`helper`), and proof of work's counters (`trust`, P4.5.8), `{}` for the machines already there.
`compute_events.verification` gains the value `rejected` and `settings_kv` the key `verification`; both are strings or
JSON already, so neither needs DDL.
"""
from __future__ import annotations

from typing import Sequence, Union

from alembic import op
import sqlalchemy as sa


revision: str = "0016c_helper_jobs"
down_revision: Union[str, None] = "0016b_compute_cache_tokens"
branch_labels: Union[str, Sequence[str], None] = None
depends_on: Union[str, Sequence[str], None] = None


def upgrade() -> None:
    """Create the three tables, then add the machine row's four columns."""
    op.create_table('cpu_jobs',
        sa.Column('id', sa.String(length=40), nullable=False),
        sa.Column('kind', sa.String(length=16), nullable=False),
        sa.Column('ref', sa.String(length=160), nullable=False),
        sa.Column('item_kind', sa.String(length=24), nullable=True),
        sa.Column('item_ref', sa.String(length=120), nullable=True),
        sa.Column('owner_id', sa.String(length=40), nullable=True),
        sa.Column('spec', sa.JSON(), nullable=False),
        sa.Column('spec_hash', sa.String(length=64), nullable=False),
        sa.Column('jobs_build', sa.String(length=16), nullable=False),
        sa.Column('priority', sa.Integer(), nullable=False),
        sa.Column('mem_mb', sa.Integer(), nullable=False),
        sa.Column('state', sa.String(length=16), nullable=False),
        sa.Column('accepted_run', sa.String(length=40), nullable=True),
        sa.Column('result_hash', sa.String(length=64), nullable=True),
        sa.Column('attempts', sa.Integer(), nullable=False),
        sa.Column('created_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('updated_at', sa.DateTime(timezone=True), nullable=False),
        sa.ForeignKeyConstraint(['owner_id'], ['users.id'], ondelete='SET NULL'),
        sa.PrimaryKeyConstraint('id')
    )
    op.create_index(op.f('ix_cpu_jobs_owner_id'), 'cpu_jobs', ['owner_id'], unique=False)
    op.create_index(op.f('ix_cpu_jobs_ref'), 'cpu_jobs', ['ref'], unique=False)
    op.create_index('ix_cpu_jobs_state_priority_created', 'cpu_jobs', ['state', 'priority', 'created_at'],
                    unique=False)
    op.create_table('cpu_job_runs',
        sa.Column('id', sa.String(length=40), nullable=False),
        sa.Column('job_id', sa.String(length=40), nullable=False),
        sa.Column('machine_id', sa.String(length=40), nullable=False),
        sa.Column('executor', sa.String(length=8), nullable=False),
        sa.Column('role', sa.String(length=16), nullable=False),
        sa.Column('state', sa.String(length=16), nullable=False),
        sa.Column('offered_at', sa.DateTime(timezone=True), nullable=False),
        sa.Column('started_at', sa.DateTime(timezone=True), nullable=True),
        sa.Column('last_progress_at', sa.DateTime(timezone=True), nullable=True),
        sa.Column('ended_at', sa.DateTime(timezone=True), nullable=True),
        sa.Column('deadline_at', sa.DateTime(timezone=True), nullable=True),
        sa.Column('cpu_s', sa.Float(), nullable=False),
        sa.Column('wall_s', sa.Float(), nullable=False),
        sa.Column('paused_s', sa.Float(), nullable=False),
        sa.Column('peak_mb', sa.Float(), nullable=False),
        sa.Column('result', sa.JSON(), nullable=True),
        sa.Column('result_hash', sa.String(length=64), nullable=True),
        sa.Column('chain_head', sa.String(length=64), nullable=True),
        sa.Column('rounds', sa.Integer(), nullable=True),
        sa.Column('digests', sa.JSON(), nullable=True),
        sa.Column('claim', sa.JSON(), nullable=True),
        sa.Column('signature', sa.String(length=128), nullable=True),
        sa.Column('acked_at', sa.DateTime(timezone=True), nullable=True),
        sa.Column('verdict', sa.String(length=16), nullable=True),
        sa.Column('error', sa.JSON(), nullable=True),
        sa.ForeignKeyConstraint(['job_id'], ['cpu_jobs.id'], ondelete='CASCADE'),
        sa.PrimaryKeyConstraint('id')
    )
    op.create_index(op.f('ix_cpu_job_runs_job_id'), 'cpu_job_runs', ['job_id'], unique=False)
    op.create_index(op.f('ix_cpu_job_runs_machine_id'), 'cpu_job_runs', ['machine_id'], unique=False)
    op.create_index(op.f('ix_cpu_job_runs_state'), 'cpu_job_runs', ['state'], unique=False)
    op.create_table('job_costs',
        sa.Column('kind', sa.String(length=16), nullable=False),
        sa.Column('size', sa.String(length=24), nullable=False),
        sa.Column('seats', sa.Integer(), nullable=False),
        sa.Column('rounds', sa.Integer(), nullable=False),
        sa.Column('cpu_s', sa.Float(), nullable=False),
        sa.Column('peak_mb', sa.Float(), nullable=False),
        sa.Column('samples', sa.Integer(), nullable=False),
        sa.Column('updated_at', sa.DateTime(timezone=True), nullable=False),
        sa.PrimaryKeyConstraint('kind', 'size', 'seats', 'rounds')
    )
    # the machines already registered have no key, no helper block yet and empty counters
    op.add_column('servers', sa.Column('public_key', sa.String(length=64), nullable=True))
    op.add_column('servers', sa.Column('key_at', sa.DateTime(timezone=True), nullable=True))
    op.add_column('servers', sa.Column('helper', sa.JSON(), nullable=True))
    op.add_column('servers', sa.Column('trust', sa.JSON(), nullable=False, server_default='{}'))


def downgrade() -> None:
    """Drop the machine row's columns and the three tables."""
    with op.batch_alter_table('servers') as batch:
        batch.drop_column('trust')
        batch.drop_column('helper')
        batch.drop_column('key_at')
        batch.drop_column('public_key')
    op.drop_table('job_costs')
    op.drop_index(op.f('ix_cpu_job_runs_state'), table_name='cpu_job_runs')
    op.drop_index(op.f('ix_cpu_job_runs_machine_id'), table_name='cpu_job_runs')
    op.drop_index(op.f('ix_cpu_job_runs_job_id'), table_name='cpu_job_runs')
    op.drop_table('cpu_job_runs')
    op.drop_index('ix_cpu_jobs_state_priority_created', table_name='cpu_jobs')
    op.drop_index(op.f('ix_cpu_jobs_ref'), table_name='cpu_jobs')
    op.drop_index(op.f('ix_cpu_jobs_owner_id'), table_name='cpu_jobs')
    op.drop_table('cpu_jobs')
