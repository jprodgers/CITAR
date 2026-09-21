#!/usr/bin/env bash
# Back up CITAR: the database, the configuration, and the game saves.
#
# Run from a systemd timer (deploy/citar-backup.timer). Safe to run at any time — it never stops
# the service and never blocks a game.
#
# What it protects against, in order of likelihood:
#   a bad migration or a mistaken delete   -> the nightly copies
#   the disk or the VPS going away         -> whatever you copy off-box (see OFFSITE below)
#
# Restoring is in docs/RUNBOOK.md. A backup nobody has restored is a hope, not a backup, so the
# runbook has an actual drill.

set -uo pipefail

DATA_DIR="${CITAR_DATA_DIR:-/var/lib/citar}"
BACKUP_DIR="${CITAR_BACKUP_DIR:-/var/backups/citar}"
KEEP_DAYS="${CITAR_BACKUP_KEEP_DAYS:-14}"
STAMP=$(date -u +%Y%m%d-%H%M%S)
OUT="$BACKUP_DIR/$STAMP"

log() { printf '%s %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$*"; }

mkdir -p "$OUT" || { log "cannot create $OUT"; exit 1; }

# --- database ---------------------------------------------------------------
# sqlite3 .backup, not cp. A live SQLite database has a write-ahead log, and copying the file while
# the server is mid-transaction yields something that may or may not open. .backup takes a proper
# consistent snapshot of a database that is being written to.
DB="$DATA_DIR/citar.db"
if [ -f "$DB" ]; then
  if command -v sqlite3 >/dev/null 2>&1; then
    if sqlite3 "$DB" ".backup '$OUT/citar.db'" 2>/dev/null; then
      log "database snapshot ok ($(du -h "$OUT/citar.db" | cut -f1))"
      # Prove it opens and passes its own integrity check before we call this a backup.
      if [ "$(sqlite3 "$OUT/citar.db" 'PRAGMA integrity_check;' 2>/dev/null)" = "ok" ]; then
        log "integrity check ok"
      else
        log "INTEGRITY CHECK FAILED — this backup is not trustworthy"
        touch "$OUT/UNTRUSTWORTHY"
      fi
    else
      log "sqlite3 .backup failed; falling back to a copy (may be inconsistent)"
      cp -a "$DB" "$OUT/citar.db"
    fi
  else
    log "sqlite3 not installed — install it (apt install sqlite3) for consistent snapshots"
    cp -a "$DB" "$OUT/citar.db"
  fi
else
  log "no database at $DB"
fi

# --- configuration ----------------------------------------------------------
# Contains the secret key: losing it logs everyone out and invalidates outstanding email links.
if [ -f /etc/citar/citar.env ]; then
  cp -a /etc/citar/citar.env "$OUT/citar.env"
  chmod 600 "$OUT/citar.env"
  log "configuration saved"
fi

# --- saves ------------------------------------------------------------------
# Games, scenarios, maps, probes, reports and the usage ledger. Compressed because the ledger is
# JSON lines and compresses to almost nothing.
if [ -d "$DATA_DIR/saves" ]; then
  tar czf "$OUT/saves.tar.gz" -C "$DATA_DIR" saves 2>/dev/null \
    && log "saves archived ($(du -h "$OUT/saves.tar.gz" | cut -f1))" \
    || log "saves archive failed"
fi

# --- prune ------------------------------------------------------------------
# Deliberately AFTER the new backup is written and verified: pruning first would mean a failure
# leaves you with fewer backups than you started with.
find "$BACKUP_DIR" -maxdepth 1 -type d -name '20*' -mtime "+$KEEP_DAYS" -exec rm -rf {} + 2>/dev/null
log "kept $(find "$BACKUP_DIR" -maxdepth 1 -type d -name '20*' | wc -l) backup(s), pruning older than ${KEEP_DAYS}d"

chmod 700 "$BACKUP_DIR"
log "backup complete: $OUT"

# --- OFFSITE ----------------------------------------------------------------
# Everything above is on the same disk as the thing it is backing up, so it survives a mistake but
# not a dead VPS. If CITAR_BACKUP_RSYNC_TARGET is set, the newest backup is copied there.
#   CITAR_BACKUP_RSYNC_TARGET="user@host:/path/citar-backups/"
if [ -n "${CITAR_BACKUP_RSYNC_TARGET:-}" ]; then
  if rsync -az --delete-after "$OUT/" "$CITAR_BACKUP_RSYNC_TARGET$STAMP/" 2>&1; then
    log "copied offsite to $CITAR_BACKUP_RSYNC_TARGET$STAMP/"
  else
    log "OFFSITE COPY FAILED"
    exit 1
  fi
else
  log "no offsite target configured (set CITAR_BACKUP_RSYNC_TARGET); backups are on this disk only"
fi
