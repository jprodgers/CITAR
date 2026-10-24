#!/usr/bin/env bash
# Back up CITAR: the database, the whole state directory, the configuration, and the release that
# wrote them.
#
# Run from a systemd timer (deploy/citar-backup.timer), and by citar-deploy with the service
# stopped just before it switches releases. Safe to run at any time: it never stops the service
# and never blocks a game.
#
# What it protects against, in order of likelihood:
#   a bad migration or a mistaken delete   -> the nightly copies
#   the disk or the VPS going away         -> whatever you copy off-box (see OFFSITE below)
#
# Each backup is a folder under $BACKUP_DIR named by its UTC time:
#   citar.db         the database, by sqlite3 .backup (consistent while the server writes), then
#                    integrity-checked
#   state.tar.gz     the whole state directory (saves, config, benchmarks, everything 0.1.6 writes
#                    there) without the live database files, which citar.db replaces
#   data.tar.gz, saves.tar.gz, config.tar.gz, benchmarks.tar.gz
#                    only for a directory the env file moves outside the state directory
#                    (CITAR_DATA_DIR, CITAR_SAVE_DIR, CITAR_CONFIG_DIR, CITAR_BENCH_DIR), the same way
#   citar.env        the configuration and secrets (0600)
#   RELEASE          what wrote them: the release (/opt/citar/current), the database's revision,
#                    where each directory was, and which archives hold them
#   UNTRUSTWORTHY    only when something above failed, saying what; such a backup prunes nothing
#
# The exit status, which `systemctl start citar-backup` reports (citar-deploy and install.sh take
# their switch's backup that way):
#   0  the backup is complete; its last line is "backup complete: <folder>"
#   1  it could not start (no backup folder could be made)
#   2  the backup is not trustworthy (UNTRUSTWORTHY says why)
#   3  the backup is complete and trustworthy on this disk, but the offsite copy failed
#
# Restoring is in docs/server/RUNBOOK.md. A backup nobody has restored is a hope, not a backup, so
# the runbook has an actual drill.
#
# CITAR_DEPLOY_ROOT (tests only) prefixes every path the script reads or writes, so its logic runs
# without root against a temporary tree.

set -uo pipefail

ROOT="${CITAR_DEPLOY_ROOT:-}"
# The paths `citar setup --server` puts in when it installs this script (citar/wizard/units.py).
ENV_FILE="/etc/citar/citar.env"
STATE_DIR="/var/lib/citar"
CURRENT="/opt/citar/current"
BACKUP_DIR="$ROOT${CITAR_BACKUP_DIR:-/var/backups/citar}"
KEEP_DAYS="${CITAR_BACKUP_KEEP_DAYS:-14}"
STAMP=$(date -u +%Y%m%d-%H%M%S)

log() { printf '%s %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$*"; }

# A setting as systemd 255 reads the env file (EnvironmentFile=, probed in WSL): the LAST
# assignment of the key wins; lines starting with # or ; are comments; whitespace around the key,
# the = and the value is dropped; one pair of quotes around the value is taken off. `export KEY=`
# is not an assignment to systemd, and a # after a value is part of it. (A value continued on the
# next line with a backslash, or with escapes inside its quotes, is read as written: no CITAR
# setting needs either.)
env_get() {
  local value
  [ -f "$ROOT$ENV_FILE" ] || return 0
  value=$(awk -v key="$1" '
    { sub(/^[ \t\r]+/, ""); sub(/[ \t\r]+$/, "") }
    $0 == "" || /^[#;]/ { next }
    {
      eq = index($0, "=")
      if (eq == 0) next
      name = substr($0, 1, eq - 1)
      sub(/[ \t]+$/, "", name)
      if (name != key) next
      found = substr($0, eq + 1)
      sub(/^[ \t]+/, "", found)
    }
    END { printf "%s", found }' "$ROOT$ENV_FILE" 2>/dev/null)
  case "$value" in
    \"*\") value="${value#\"}"; value="${value%\"}" ;;
    \'*\') value="${value#\'}"; value="${value%\'}" ;;
  esac
  printf '%s' "$value"
}

# A directory setting as the service resolves it: absolute, or relative to its working directory
# (the unit's state directory), without a trailing slash; nothing for an empty setting.
at() {
  local path="$1"
  case "$path" in
    "") return 0 ;;
    /*) path="$ROOT$path" ;;
    *)  path="$ROOT$STATE_DIR/$path" ;;
  esac
  while [ "${#path}" -gt 1 ] && [ "${path%/}" != "$path" ]; do path="${path%/}"; done
  printf '%s' "$path"
}

# Whether directory $1 is $2 or inside it.
inside() { case "$1" in "$2"|"$2"/*) return 0 ;; esac; return 1; }

UNTRUSTWORTHY=""
distrust() {
  log "UNTRUSTWORTHY: $*"
  UNTRUSTWORTHY="${UNTRUSTWORTHY}$*"$'\n'
}

# --- where things are ------------------------------------------------------
# As the service sees them (citar/paths.py): the unit's state directory unless the env file names
# another; the data directory (the database and the secret key), the saves, the configuration and
# the benchmarks inside it unless the env file moves them; and the database where the settings put
# it (citar/settings.py: CITAR_DB_URL, else citar.db in the data directory).
STATE="$(at "$(env_get CITAR_STATE_DIR)")"
STATE="${STATE:-$(at "$STATE_DIR")}"
DATA="$(at "$(env_get CITAR_DATA_DIR)")"
DATA="${DATA:-$STATE}"
saves="$(env_get CITAR_SAVE_DIR)"
SAVES="$(at "$saves")"
SAVES="${SAVES:-$STATE/saves}"
CONFIG="$(at "$(env_get CITAR_CONFIG_DIR)")"
CONFIG="${CONFIG:-$STATE/config}"
BENCH="$(at "$(env_get CITAR_BENCH_DIR)")"
if [ -z "$BENCH" ] && [ -n "$saves" ]; then
  BENCH="$(dirname "$SAVES")/benchmarks"     # paths.bench_dir: beside moved saves
fi
BENCH="${BENCH:-$STATE/benchmarks}"
url="$(env_get CITAR_DB_URL)"
case "$url" in
  "")          DB="$DATA/citar.db" ;;
  sqlite*:///*) path="${url#*:///}"; DB="$(at "${path%%\?*}")" ;;
  *)           DB="" ;;
esac

case "$BACKUP_DIR" in
  *\'*|*\"*) log "CITAR_BACKUP_DIR must not hold a quote: $BACKUP_DIR"; exit 1 ;;
esac
OUT="$BACKUP_DIR/$STAMP"
n=1
while [ -e "$OUT" ]; do OUT="$BACKUP_DIR/$STAMP-$n"; n=$((n + 1)); done
if ! { mkdir -p "$BACKUP_DIR" && chmod 700 "$BACKUP_DIR" && mkdir "$OUT"; }; then
  log "cannot create $OUT"
  exit 1
fi
log "backing up $STATE into $OUT"

# --- database ---------------------------------------------------------------
# sqlite3 .backup, not cp. A live SQLite database has a write-ahead log, and copying the file while
# the server is mid-transaction yields something that may or may not open. .backup takes a proper
# consistent snapshot of a database that is being written to.
revision="unknown"
if [ -z "$DB" ]; then
  log "the database is not SQLite (${url%%:*}): back it up with its own tools; the rest is backed up here"
  database="external (${url%%:*})"
elif [ ! -f "$DB" ]; then
  log "no database at $DB"
  database="none"
else
  database="${DB#"$ROOT"}"
  if ! command -v sqlite3 >/dev/null 2>&1; then
    cp -a "$DB" "$OUT/citar.db"
    distrust "sqlite3 is not installed (apt install sqlite3), so the database was copied, not snapshotted, and is unchecked"
  else
    if sqlite3 "$DB" ".backup '$OUT/citar.db'" 2>"$OUT/sqlite3.err"; then
      log "database snapshot ok ($(du -h "$OUT/citar.db" | cut -f1))"
    else
      cp -a "$DB" "$OUT/citar.db" 2>/dev/null
      distrust "sqlite3 .backup failed: $(head -c 300 "$OUT/sqlite3.err"); the database was copied instead"
    fi
    rm -f "$OUT/sqlite3.err"
    # Run as root with the service stopped, sqlite3 may have made the write-ahead log and its index
    # beside the database itself: give them to the database's owner, or the service cannot open it.
    if [ "$(id -u)" = "0" ]; then
      for extra in "$DB-wal" "$DB-shm"; do
        [ -e "$extra" ] && chown --reference="$DB" "$extra" 2>/dev/null
      done
    fi
    # Prove it opens and passes its own integrity check before calling this a backup.
    check="$(sqlite3 "$OUT/citar.db" 'PRAGMA integrity_check;' 2>&1 | head -5)"
    if [ "$check" = "ok" ]; then
      log "integrity check ok"
      revision="$(sqlite3 "$OUT/citar.db" 'SELECT version_num FROM alembic_version;' 2>/dev/null | head -1)"
      revision="${revision:-none}"
    else
      distrust "the snapshot fails its integrity check: $(printf '%s' "$check" | tr '\n' ' ' | head -c 300)"
    fi
  fi
fi

# --- the state directory ----------------------------------------------------
# Everything the server writes: games, scenarios, maps, probes, reports, the usage ledger, lab
# results, benchmarks, configuration. The live database files are left out (the snapshot above is
# the consistent copy), and so is this backup folder if it was put inside the directory.
archive() {   # $1 the tarball's name in the backup, $2 the directory
  local name="$1" dir="$2" rel status suffix
  local -a excludes=()
  case "$DB" in
    "$dir"/*)
      rel="${DB#"$dir"/}"
      for suffix in "" -wal -shm -journal; do excludes+=("--exclude=./$rel$suffix"); done ;;
  esac
  case "$BACKUP_DIR" in
    "$dir"/*) excludes+=("--exclude=./${BACKUP_DIR#"$dir"/}") ;;
  esac
  tar --create --gzip --file "$OUT/$name" --directory "$dir" --anchored ${excludes[@]+"${excludes[@]}"} . 2>"$OUT/tar.err"
  status=$?
  if [ "$status" -eq 0 ]; then
    log "${dir#"$ROOT"} archived as $name ($(du -h "$OUT/$name" | cut -f1))"
  elif [ "$status" -eq 1 ]; then
    # GNU tar's 1: a file changed while it was read (the server was running). The archive is whole;
    # that file is as it was part way through being written.
    log "${dir#"$ROOT"} archived as $name; files changed while being read: $(head -c 300 "$OUT/tar.err" | tr '\n' ' ')"
  else
    distrust "the archive of ${dir#"$ROOT"} failed (tar exit $status): $(head -c 300 "$OUT/tar.err" | tr '\n' ' ')"
  fi
  rm -f "$OUT/tar.err"
  ARCHIVES="${ARCHIVES:+$ARCHIVES }$name"
}

ARCHIVES=""
archived=()
if [ -d "$STATE" ]; then
  archive state.tar.gz "$STATE"
  archived+=("$STATE")
else
  distrust "no state directory at ${STATE#"$ROOT"}"
fi
# A directory the env file moved outside the state directory is archived on its own; one that does
# not exist yet holds nothing (the server makes each when it first writes there).
for pair in "data.tar.gz|$DATA" "saves.tar.gz|$SAVES" "config.tar.gz|$CONFIG" "benchmarks.tar.gz|$BENCH"; do
  name="${pair%%|*}"
  dir="${pair#*|}"
  covered=0
  for done_dir in ${archived[@]+"${archived[@]}"}; do
    if inside "$dir" "$done_dir"; then covered=1; break; fi
  done
  [ "$covered" -eq 0 ] || continue
  if [ -d "$dir" ]; then
    archive "$name" "$dir"
    archived+=("$dir")
  else
    log "no ${name%.tar.gz} directory at ${dir#"$ROOT"} yet: nothing to archive there"
  fi
done

# --- configuration ----------------------------------------------------------
# Contains the secret key: losing it logs everyone out and invalidates outstanding email links.
if [ -f "$ROOT$ENV_FILE" ]; then
  if cp -a "$ROOT$ENV_FILE" "$OUT/citar.env" && chmod 600 "$OUT/citar.env"; then
    log "configuration saved"
  else
    distrust "the configuration could not be copied"
  fi
else
  log "no configuration at $ROOT$ENV_FILE"
fi

# --- what wrote it ----------------------------------------------------------
# The release that runs (or ran, when citar-deploy stopped it to take this): a database migrated by
# a newer release cannot be opened by an older one, so a rollback needs to know both.
if [ -L "$ROOT$CURRENT" ]; then
  release="$(readlink "$ROOT$CURRENT")"
  version="${release%/}"
  version="${version##*/}"
else
  release="none (no $CURRENT)"
  version="unknown"
fi
{
  printf 'made=%s\n' "$STAMP"
  printf 'release=%s\n' "$release"
  printf 'version=%s\n' "$version"
  printf 'state=%s\n' "${STATE#"$ROOT"}"
  printf 'data=%s\n' "${DATA#"$ROOT"}"
  printf 'saves=%s\n' "${SAVES#"$ROOT"}"
  printf 'config=%s\n' "${CONFIG#"$ROOT"}"
  printf 'benchmarks=%s\n' "${BENCH#"$ROOT"}"
  printf 'archives=%s\n' "$ARCHIVES"
  printf 'database=%s\n' "$database"
  printf 'revision=%s\n' "$revision"
} > "$OUT/RELEASE"
log "release $release (version $version), database revision $revision"

if [ -n "$UNTRUSTWORTHY" ]; then
  printf '%s' "$UNTRUSTWORTHY" > "$OUT/UNTRUSTWORTHY"
  log "THIS BACKUP IS NOT TRUSTWORTHY ($OUT/UNTRUSTWORTHY); older backups are kept, none pruned"
  exit 2
fi

# --- prune ------------------------------------------------------------------
# Deliberately AFTER the new backup is written and verified: pruning first would mean a failure
# leaves you with fewer backups than you started with.
find "$BACKUP_DIR" -mindepth 1 -maxdepth 1 -type d -name '20*' -mtime "+$KEEP_DAYS" -exec rm -rf {} + 2>/dev/null
log "kept $(find "$BACKUP_DIR" -mindepth 1 -maxdepth 1 -type d -name '20*' | wc -l) backup(s), pruning older than ${KEEP_DAYS}d"

# --- OFFSITE ----------------------------------------------------------------
# Everything above is on the same disk as the thing it is backing up, so it survives a mistake but
# not a dead VPS. If CITAR_BACKUP_RSYNC_TARGET is set, the newest backup is copied there. A failed
# copy is its own exit status (3): the backup itself is whole and trustworthy, and a deploy may
# switch on it, but the unit fails so the failure is seen.
#   CITAR_BACKUP_RSYNC_TARGET="user@host:/path/citar-backups/"
if [ -n "${CITAR_BACKUP_RSYNC_TARGET:-}" ]; then
  if rsync -az --delete-after "$OUT/" "$CITAR_BACKUP_RSYNC_TARGET${OUT##*/}/" 2>&1; then
    log "copied offsite to $CITAR_BACKUP_RSYNC_TARGET${OUT##*/}/"
  else
    log "OFFSITE COPY FAILED: $OUT is complete and checked, but on this disk only"
    exit 3
  fi
else
  log "no offsite target configured (set CITAR_BACKUP_RSYNC_TARGET); backups are on this disk only"
fi
log "backup complete: $OUT"
