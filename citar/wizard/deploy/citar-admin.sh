#!/usr/bin/env bash
# citar-admin: CITAR's administration commands, run as the service runs. Installed as
# /usr/local/bin/citar-admin.
#
#     sudo citar-admin list-users
#     sudo citar-admin create-admin --handle you --email you@example.com
#     sudo citar-admin test-email you@example.com
#     citar-admin --plan list-users        # print the command it would run, and run nothing
#
# Every command runs in a transient unit (systemd-run) as the citar account, with citar.service's
# environment file, working directory and sandbox. So it reads the configuration exactly as the
# service does (systemd parses the env file, not a shell), opens the same database and state, and
# can write nothing the service cannot: run as root from a shell, the same command would read no
# configuration, open a database of its own under root's home, and leave root-owned files in the
# state directory that the service could no longer write.
#
# CITAR_DEPLOY_SLICE   when set, the transient unit runs in that slice (the staging run sets it,
#                      design/phase5-release.md P5.14; unset on a real server).
# CITAR_DEPLOY_ROOT    tests only: prefixes every path, and lifts the root check.

set -euo pipefail

ROOT="${CITAR_DEPLOY_ROOT:-}"
# The paths `citar setup --server` puts in when it installs this script (citar/wizard/units.py).
ENV_FILE="/etc/citar/citar.env"
STATE_DIR="/var/lib/citar"
PROGRAM="/opt/citar/current/venv/bin/citar-admin"
SERVICE_USER="citar"

die() { printf 'citar-admin: %s\n' "$*" >&2; exit 1; }

PLAN=0
if [ "${1:-}" = "--plan" ]; then
  PLAN=1
  shift
fi

# citar.service's own settings (deploy/citar.service; tests/test_deploy.py holds the two equal):
# who it runs as, its configuration, and its file-system and privilege sandbox. Not its memory and
# CPU limits, which are for a server that runs for weeks.
properties=(
  -p "User=$SERVICE_USER"
  -p "Group=$SERVICE_USER"
  -p "WorkingDirectory=$ROOT$STATE_DIR"
  -p "Environment=CITAR_STATE_DIR=$ROOT$STATE_DIR"
  -p "EnvironmentFile=$ROOT$ENV_FILE"
  -p "ProtectSystem=strict"
  -p "ProtectHome=yes"
  -p "ReadWritePaths=$ROOT$STATE_DIR"
  -p "PrivateTmp=yes"
  -p "NoNewPrivileges=yes"
  -p "PrivateDevices=yes"
  -p "ProtectKernelTunables=yes"
  -p "ProtectKernelModules=yes"
  -p "ProtectControlGroups=yes"
  -p "ProtectClock=yes"
  -p "ProtectHostname=yes"
  -p "ProtectKernelLogs=yes"
  -p "ProtectProc=invisible"
  -p "RestrictSUIDSGID=yes"
  -p "RestrictRealtime=yes"
  -p "RestrictNamespaces=yes"
  -p "LockPersonality=yes"
  -p "RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX"
  -p "SystemCallFilter=@system-service"
  -p "SystemCallErrorNumber=EPERM"
)

# A terminal gets one (a password prompt needs it); a pipe is passed through as it is, so a script
# can give `create-user --password-stdin` its password and read the output.
if [ -t 0 ] && [ -t 1 ]; then
  io="--pty"
else
  io="--pipe"
fi

command=(systemd-run --quiet --wait --collect "$io" --service-type=exec)
# systemd 254 and later expand ${VARIABLE} and $VARIABLE in the command's arguments unless told
# not to; a reason, a display name or a password could hold a dollar sign. Older ones cannot be
# told, so an argument with one is refused there rather than changed.
if [ "$PLAN" -eq 1 ] && ! command -v systemd-run >/dev/null 2>&1; then
  expand_flag=1
elif systemd-run --help 2>/dev/null | grep -q -- '--expand-environment'; then
  expand_flag=1
else
  expand_flag=0
fi
if [ "$expand_flag" -eq 1 ]; then
  command+=(--expand-environment=no)
else
  for argument in "$@"; do
    case "$argument" in
      *'$'*) die "this systemd (older than 254) would expand the \$ in \"$argument\": run the command another way" ;;
    esac
  done
fi
if [ -n "${CITAR_DEPLOY_SLICE:-}" ]; then
  command+=("--slice=$CITAR_DEPLOY_SLICE")
fi
command+=("${properties[@]}" -- "$ROOT$PROGRAM" "$@")

if [ "$PLAN" -eq 1 ]; then
  printf 'citar-admin would run, as root:\n  '
  printf '%q ' "${command[@]}"
  printf '\n'
  exit 0
fi

if [ -z "$ROOT" ] && [ "$(id -u)" != "0" ]; then
  die "run it with sudo: each command runs as the $SERVICE_USER account through systemd-run, which needs root"
fi
command -v systemd-run >/dev/null 2>&1 || die "systemd-run is not here: citar-admin needs systemd"
[ -f "$ROOT$ENV_FILE" ] || die "no $ROOT$ENV_FILE: is CITAR set up? (citar setup --server writes it)"
[ -x "$ROOT$PROGRAM" ] || die "no $ROOT$PROGRAM: is CITAR installed there?"
exec "${command[@]}"
