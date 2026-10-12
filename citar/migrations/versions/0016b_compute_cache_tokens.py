"""The compute ledger keeps a model step's cached input tokens.

Revision ID: 0016b_compute_cache_tokens
Revises: 0016a_baseline
Created: 2026-10-12

Package 3-10 moves costing onto `compute_events` (design/phase3-features.md P3.4.7): a report prices an online API's
work from the tokens the ledger kept. An Anthropic conversation reads most of its prompt from the provider's cache, at
a tenth of the input price, and writes it there at a premium, so pricing every input token at the full price (or
leaving the cached ones out) would misprice the work. The baseline kept input, output and reasoning tokens only; this
adds the cache's reads and writes, 0 for every event written before.
"""
from __future__ import annotations

from typing import Sequence, Union

from alembic import op
import sqlalchemy as sa


revision: str = "0016b_compute_cache_tokens"
down_revision: Union[str, None] = "0016a_baseline"
branch_labels: Union[str, Sequence[str], None] = None
depends_on: Union[str, Sequence[str], None] = None


def upgrade() -> None:
    """Add the two counts, 0 for the events already written."""
    op.add_column("compute_events", sa.Column("tokens_cache_read", sa.Integer(), nullable=False, server_default="0"))
    op.add_column("compute_events", sa.Column("tokens_cache_write", sa.Integer(), nullable=False, server_default="0"))


def downgrade() -> None:
    """Drop them again."""
    with op.batch_alter_table("compute_events") as batch:
        batch.drop_column("tokens_cache_write")
        batch.drop_column("tokens_cache_read")
