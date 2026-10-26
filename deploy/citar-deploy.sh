#!/usr/bin/env bash
# citar-deploy: put a CITAR release on this server from a deploy bundle, and take it back
# (design/phase5-release.md P5.13.3). Run as root on the server, from an unpacked bundle
# (`sudo bash deploy/citar-deploy.sh ...`) or from the copy it installs (/usr/local/sbin/citar-deploy).
#
#     sudo citar-deploy install --bundle DIR [--reinstall]    a newer release, switched with the service stopped
#     sudo citar-deploy cutover --bundle DIR                  the first deploy, from 0.1.5's layout
#     sudo citar-deploy rollback [--to VERSION] [--restore-backup DIR]
#     sudo citar-deploy uncutover                             the cutover undone: 0.1.5 as it was
#     sudo citar-deploy status
#     sudo citar-deploy prune [--keep N]                      releases beyond N (default 3) removed
#
# Every command takes --plan: every check runs, then each step is printed with its paths and commands, and nothing is
# changed (status changes nothing anyway). Read the plan, then run the same command without it.
#
#     --slice NAME      every systemd-run this starts, the pip install's scope included, runs in that slice (also
#                       CITAR_DEPLOY_SLICE; the staging run sets it, design/phase5-release.md P5.14; unset on a server)
#     --site-url URL    the site as visitors reach it, through the proxy (default: the env file's CITAR_PUBLIC_ORIGIN);
#                       it is always reached at this box's own 127.0.0.1, with the site's host name
#
# The layout (P5.13.1): each release a venv of its own in /opt/citar/releases/<version>, /opt/citar/current linking
# to the one that runs, the state in /var/lib/citar, the configuration in /etc/citar/citar.env. The other sites this
# box serves are listed in /etc/citar/deploy-checks.conf, one URL a line (# comments): each must answer 200 before a
# deploy changes anything, and is checked again after it. citar-deploy keeps its own files (its lock, a cutover's
# record) in /var/lib/citar-deploy and writes one line a deploy to /var/log/citar-deploy.log.
#
# The server migrates its database on every start and refuses one a newer release migrated. So `install` reads the
# database's revision, stops the service, takes a backup (citar-backup, the service stopped, so the snapshot is
# consistent and current) and only then switches; if the new release does not come up, it is stopped, the database is
# put back from that backup when the revision moved, and the previous release is started again. `rollback` refuses a
# database at a revision the release it goes back to does not know, unless it is given a backup to restore.
#
# Run it inside tmux (or screen), so that a dropped SSH session does not end it. From the moment it stops the service
# until the deploy is recorded, a hang-up is ignored (the command runs to its end); Ctrl-C or a TERM undoes an install
# or a cutover as a failed release is undone, and a rollback or an uncutover, once begun, runs to its end.
#
# Exit status: 0 done; 1 refused or failed (a failed or interrupted install or cutover is undone first, and says so);
# 2 a usage error; 3 deployed, but another site on this box does not answer since (nothing is done about it
# automatically).
#
# Tests only (tests/test_citar_deploy.py): CITAR_DEPLOY_ROOT prefixes every path, lifts the root check, skips the
# changes of owner, and requires stand-ins for systemctl and systemd-run first on PATH, so the logic runs without root;
# CITAR_DEPLOY_PYTHON names the Python (default python3.12); CITAR_DEPLOY_HEALTH_SECONDS shortens the 60 s wait.

set -uo pipefail
umask 022

ROOT="${CITAR_DEPLOY_ROOT:-}"
PY="${CITAR_DEPLOY_PYTHON:-python3.12}"
HEALTH_SECONDS="${CITAR_DEPLOY_HEALTH_SECONDS:-60}"

# The release layout (P5.13.1), as every path below names it; $ROOT comes in front only where a file is read or written.
OPT="/opt/citar"
RELEASES="$OPT/releases"
CURRENT="$OPT/current"
# citar-backup's names for these, so env_get and at below are its own lines (tests/test_citar_deploy.py holds them equal).
ENV_FILE="/etc/citar/citar.env"
STATE_DIR="/var/lib/citar"
CHECKS="/etc/citar/deploy-checks.conf"
WORK="/var/lib/citar-deploy"
CUTOVER="$WORK/cutover"
LOG_FILE="/var/log/citar-deploy.log"
BACKUPS="/var/backups/citar"
UNITS="/etc/systemd/system"
LAB_UNIT="$UNITS/citar-lab.service"
NGINX_SITE="/etc/nginx/sites-available/citar"
SERVICE_USER="citar"
NEED_MB=1536
KEEP_DEFAULT=3

# What the bundle installs, where, and its mode: units.TARGETS (citar/wizard/units.py) and the two scripts only this
# script installs. The box then never keeps an older release's backup, which left most of the state directory out.
FILES=(
  "deploy/citar.service|$UNITS/citar.service|0644"
  "deploy/citar-backup.service|$UNITS/citar-backup.service|0644"
  "deploy/citar-backup.timer|$UNITS/citar-backup.timer|0644"
  "deploy/citar-admin.sh|/usr/local/bin/citar-admin|0755"
  "deploy/citar-backup.sh|/usr/local/bin/citar-backup|0755"
  "deploy/citar-secrets.sh|/usr/local/bin/citar-secrets|0755"
  "deploy/citar-deploy.sh|/usr/local/sbin/citar-deploy|0755"
)
# The files a bundle must hold besides its wheel, constraints, BUNDLE and SHA256SUMS.
BUNDLE_NEEDS=(deploy/citar.service deploy/citar-backup.service deploy/citar-backup.timer deploy/citar-admin.sh
  deploy/citar-backup.sh deploy/citar-secrets.sh deploy/citar-deploy.sh deploy/nginx-citar.conf env.example)
# The settings that name state: the staged release's verification points every one of them at a temporary folder.
STATE_KEYS=(CITAR_STATE_DIR CITAR_DATA_DIR CITAR_DB_URL CITAR_SAVE_DIR CITAR_CONFIG_DIR CITAR_BENCH_DIR CITAR_KEYS_FILE)
# And the home: systemd gives a unit with User= that account's home, which `citar setup --server` makes the state
# directory (useradd --home), and CITAR's fallbacks read it (citar/paths.py, citar/keystore.py, keyring's folders).
HOME_KEYS=(HOME XDG_CONFIG_HOME XDG_DATA_HOME XDG_CACHE_HOME XDG_STATE_HOME)
# The verification's share of the box (P5.13.4): the pip scope's limits, and an OOM score above citar.service's 500,
# so under memory pressure the verification dies first, never CITAR or another site (0.1.5's lab unit had 800).
VERIFY_LIMITS=(-p "CPUWeight=20" -p "MemoryMax=500M" -p "OOMScoreAdjust=800")

# citar.service's account and sandbox (deploy/citar.service; citar-admin's list, tests/test_citar_deploy.py holds the
# two equal), for the staged release's verification. WorkingDirectory, the environment and ReadWritePaths are its own.
SANDBOX=(
  -p "ProtectSystem=strict"
  -p "ProtectHome=yes"
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
# What the staged release's engine says of itself (scripts/ci/smoke_wheel.py's EXTENSION_FACTS).
ENGINE_FACTS="import json, citar._engine as E; print(json.dumps([E.__file__, E.HAS_TEST_OPS]))"

# ----------------------------------------------------------------------------------------------------- output

say()  { printf '%s\n' "$*"; }
ok()   { printf '  ok  %s\n' "$*"; }
warn() { printf 'citar-deploy: warning: %s\n' "$*" >&2; }
die()  { printf 'citar-deploy: %s\n' "$*" >&2; exit 1; }
# A refusal before anything has changed.
refuse() { die "$* Nothing was changed."; }
usage_die() { printf 'citar-deploy: %s (citar-deploy --help)\n' "$*" >&2; exit 2; }

# A path or command as the server names it: without the tests' root.
show() {
  if [ -n "$ROOT" ]; then printf '%s' "${1//"$ROOT"/}"; else printf '%s' "$1"; fi
}

quote() {
  case "$1" in
    "" ) printf "''" ;;
    *[!A-Za-z0-9_./:=@%+,^-]*) printf "'%s'" "${1//\'/\'\\\'\'}" ;;
    *) printf '%s' "$1" ;;
  esac
}

# A step that changes something: printed, then (unless --plan) run. Its exit status is the command's.
run() {
  local word line=""
  for word in "$@"; do line+=" $(quote "$(show "$word")")"; done
  printf '   %s\n' "${line# }"
  [ "$PLAN" -eq 1 ] && return 0
  "$@"
}

# A step done another way (a redirection, a loop): printed; returns 0 when it is to be done (not --plan).
act() {
  printf '   %s\n' "$1"
  [ "$PLAN" -eq 0 ]
}

usage() {
  sed -n '2,/^$/s/^# \{0,1\}//p' "$0"
}

# Text with the tests' root taken out of every path in it.
unroot() {
  if [ -n "$ROOT" ]; then sed "s#$ROOT##g"; else cat; fi
}

# ----------------------------------------------------------------------------------------------------- arguments

PLAN=0
SLICE="${CITAR_DEPLOY_SLICE:-}"
SITE_URL=""
COMMAND=""
BUNDLE=""
REINSTALL=0
TO=""
RESTORE=""
KEEP=""
while [ $# -gt 0 ]; do
  case "$1" in
    --plan) PLAN=1 ;;
    --reinstall) REINSTALL=1 ;;
    --slice|--bundle|--to|--restore-backup|--keep|--site-url)
      [ $# -ge 2 ] || usage_die "$1 needs a value"
      case "$1" in
        --slice) SLICE="$2" ;;
        --bundle) BUNDLE="$2" ;;
        --to) TO="$2" ;;
        --restore-backup) RESTORE="$2" ;;
        --keep) KEEP="$2" ;;
        --site-url) SITE_URL="$2" ;;
      esac
      shift ;;
    --slice=*) SLICE="${1#*=}" ;;
    --bundle=*) BUNDLE="${1#*=}" ;;
    --to=*) TO="${1#*=}" ;;
    --restore-backup=*) RESTORE="${1#*=}" ;;
    --keep=*) KEEP="${1#*=}" ;;
    --site-url=*) SITE_URL="${1#*=}" ;;
    -h|--help) usage; exit 0 ;;
    install|cutover|rollback|uncutover|status|prune)
      [ -z "$COMMAND" ] || usage_die "one command at a time ($COMMAND and $1)"
      COMMAND="$1" ;;
    *) usage_die "unknown argument: $1" ;;
  esac
  shift
done
[ -n "$COMMAND" ] || usage_die "which command: install, cutover, rollback, uncutover, status or prune?"
case "$COMMAND" in
  install|cutover) [ -n "$BUNDLE" ] || usage_die "$COMMAND needs --bundle DIR (an unpacked deploy bundle)" ;;
  *) [ -z "$BUNDLE" ] || usage_die "--bundle is for install and cutover" ;;
esac
[ "$REINSTALL" -eq 0 ] || [ "$COMMAND" = install ] || usage_die "--reinstall is for install"
[ -z "$TO$RESTORE" ] || [ "$COMMAND" = rollback ] || usage_die "--to and --restore-backup are for rollback"
[ -z "$KEEP" ] || [ "$COMMAND" = prune ] || usage_die "--keep is for prune"
if [ -n "$SLICE" ]; then
  [[ "$SLICE" =~ ^[A-Za-z0-9_.:@-]+\.slice$ ]] || usage_die "--slice: '$SLICE' is not a slice's name (NAME.slice)"
fi
if [ -n "$KEEP" ]; then
  { [[ "$KEEP" =~ ^[0-9]+$ ]] && [ "$KEEP" -ge 2 ]; } \
    || usage_die "--keep: at least 2 (the release that runs and the one before it are never removed)"
fi
KEEP="${KEEP:-$KEEP_DEFAULT}"
SLICE_FLAG=()
[ -z "$SLICE" ] || SLICE_FLAG=("--slice=$SLICE")

# ----------------------------------------------------------------------------------------------------- helpers in Python

# HTTP checks, wheel tags and version order: Python's standard library, the interpreter the releases are made with.
probe() {
  "$PY" -I - "$@" <<'PY'
import base64
import hashlib
import http.client
import json
import os
import platform
import re
import socket
import ssl
import sys
import time
from urllib.parse import urlsplit

VERSION = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:(a|b|rc)(0|[1-9][0-9]*))?")
WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


def key(version):
    m = VERSION.fullmatch(version)
    if not m:
        raise ValueError(version)
    return (int(m[1]), int(m[2]), int(m[3]), {"a": 0, "b": 1, "rc": 2}.get(m[4], 3), int(m[5] or 0))


def pep440(version):
    return str(version).replace("-alpha.", "a").replace("-beta.", "b").replace("-rc.", "rc")


def connection(url, via):
    """An HTTP(S) connection for url, made to ``via`` (this box's own address) when given, with the URL's host name
    for the Host header and the certificate; and that Host header."""
    parts = urlsplit(url)
    if parts.scheme not in ("http", "https") or not parts.hostname:
        raise ValueError(f"{url} is not an http or https URL")
    port = parts.port or (443 if parts.scheme == "https" else 80)
    target = via or parts.hostname
    host = parts.netloc.rsplit("@", 1)[-1]
    if parts.scheme == "http":
        return http.client.HTTPConnection(target, port, timeout=10), host
    context = ssl.create_default_context()
    name = parts.hostname

    class Connection(http.client.HTTPSConnection):
        def connect(self):
            self.sock = context.wrap_socket(socket.create_connection((target, port), self.timeout),
                                            server_hostname=name)

    return Connection(name, port, timeout=10, context=context), host


def get(url, path, via=None):
    conn, host = connection(url, via)
    try:
        conn.request("GET", path, headers={"Host": host, "User-Agent": "citar-deploy", "Connection": "close"})
        response = conn.getresponse()
        return response.status, response.read(1 << 16)
    finally:
        conn.close()


def handshake(url, via):
    """A WebSocket handshake on /ws/worker: answered by the app (101, its accept key) only when the proxy passes the
    Upgrade; a proxy that drops it hands the app a plain GET, which it answers 404."""
    nonce = base64.b64encode(os.urandom(16)).decode()
    conn, host = connection(url, via)
    try:
        conn.putrequest("GET", "/ws/worker", skip_host=True, skip_accept_encoding=True)
        for name, value in (("Host", host), ("Upgrade", "websocket"), ("Connection", "Upgrade"),
                            ("Sec-WebSocket-Key", nonce), ("Sec-WebSocket-Version", "13"),
                            ("User-Agent", "citar-deploy")):
            conn.putheader(name, value)
        conn.endheaders()
        response = conn.getresponse()
        want = base64.b64encode(hashlib.sha1((nonce + WS_GUID).encode()).digest()).decode()
        if response.status != 101:
            return f"answers {response.status}, not 101 (does the proxy pass the Upgrade?)"
        if response.getheader("Sec-WebSocket-Accept") != want:
            return "answers 101 without the accept key for this handshake"
        return None
    finally:
        conn.close()


def health_once(direct, site, want):
    try:
        status, _ = get(direct, "/api/auth/config")
        if status != 200:
            return f"{direct}/api/auth/config answers {status}"
        if want != "-":
            status, body = get(direct, "/api/meta")
            got = json.loads(body).get("version") if status == 200 else None
            if pep440(got) != want:
                return f"{direct}/api/meta answers {status}, as CITAR {got}, not {want}"
    except Exception as exc:
        return f"{direct}: {type(exc).__name__}: {exc}"
    try:
        status, _ = get(site, "/api/auth/config", via="127.0.0.1")
        if status != 200:
            return f"{site}/api/auth/config (at 127.0.0.1) answers {status}"
        if want != "-":
            problem = handshake(site, "127.0.0.1")
            if problem:
                return f"{site}/ws/worker (at 127.0.0.1): the WebSocket handshake {problem}"
    except Exception as exc:
        return f"{site} (at 127.0.0.1): {type(exc).__name__}: {exc}"
    return None


def health(direct, site, want, seconds):
    deadline = time.monotonic() + float(seconds)
    while True:
        problem = health_once(direct, site, want)
        if problem is None:
            what = "/api/auth/config 200" + (f", /api/meta CITAR {want}" if want != "-" else "")
            through = "/api/auth/config 200" + (", the WebSocket handshake answered" if want != "-" else "")
            print(f"{direct}: {what}; {site} at 127.0.0.1: {through}")
            return 0
        if time.monotonic() >= deadline:
            print(problem)
            return 1
        time.sleep(1)


def sites(path):
    urls = []
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if line and not line.startswith("#"):
                urls.append(line)
    bad = []
    for url in urls:
        parts = urlsplit(url)
        try:
            status, _ = get(url, (parts.path or "/") + (f"?{parts.query}" if parts.query else ""))
            if status != 200:
                bad.append(f"{url} answers {status}")
        except Exception as exc:
            bad.append(f"{url}: {type(exc).__name__}: {exc}")
    for problem in bad:
        print(problem)
    if not bad:
        print(len(urls))
    return 1 if bad else 0


def wheel(name, version):
    """Whether wheel ``name`` is CITAR ``version`` for this machine: a manylinux tag of its architecture no newer than
    its glibc, for this Python."""
    m = re.fullmatch(r"citar-([^-]+)-([^-]+)-([^-]+)-([^-]+)\.whl", name)
    if not m:
        print(f"{name} is not a wheel's name")
        return 1
    got, pys, abis, plats = m.groups()
    if got != version:
        print(f"{name} is CITAR {got}, and the bundle says {version}")
        return 1
    machine = platform.machine()
    libc = os.confstr("CS_GNU_LIBC_VERSION") if hasattr(os, "confstr") else None
    glibc = tuple(int(n) for n in libc.split()[1].split(".")[:2]) if libc and libc.startswith("glibc ") else None
    here = f"{machine}, {libc or 'no glibc'}, Python {sys.version_info[0]}.{sys.version_info[1]}"
    legacy = {"manylinux1": (2, 5), "manylinux2010": (2, 12), "manylinux2014": (2, 17)}
    fits = False
    for plat in plats.split("."):
        m = re.fullmatch(r"manylinux_(\d+)_(\d+)_(.+)", plat)
        if m:
            need, arch = (int(m[1]), int(m[2])), m[3]
        else:
            base, _, arch = plat.partition("_")
            if base not in legacy:
                continue
            need = legacy[base]
        if arch == machine and glibc is not None and need <= glibc:
            fits = True
    if not fits:
        print(f"{name} is not for this machine ({here}): its platform tags are {plats}")
        return 1
    ours = sys.version_info[:2]
    for py in pys.split("."):
        m = re.fullmatch(r"cp3(\d+)", py)
        if m and (("abi3" in abis.split(".") and (3, int(m[1])) <= ours) or (3, int(m[1])) == ours):
            print(f"{name}: CITAR {version} for this machine ({here})")
            return 0
    print(f"{name} is not for this machine ({here}): its Python tags are {pys}-{abis}")
    return 1


def named(wheel_path, example):
    """Every CITAR_ setting env.example names (commented out or not) or the wheel's Python code mentions."""
    import zipfile

    found = set()
    with open(example, encoding="utf-8") as handle:
        for line in handle:
            m = re.match(r"#?\s*(CITAR_[A-Z0-9_]+)=", line)
            if m:
                found.add(m[1])
    with zipfile.ZipFile(wheel_path) as wheel:
        for name in wheel.namelist():
            if name.endswith(".py"):
                found.update(re.findall(r"CITAR_[A-Z0-9_]+", wheel.read(name).decode("utf-8", "replace")))
    for name in sorted(found):
        print(name)
    return 0


def older(a, b):
    try:
        return 0 if key(a) < key(b) else 1
    except ValueError:
        return 2


def ordered(versions):
    for version in sorted((v for v in versions if VERSION.fullmatch(v)), key=key):
        print(version)
    return 0


command, args = sys.argv[1], sys.argv[2:]
sys.exit({"health": lambda: health(*args), "sites": lambda: sites(*args), "wheel": lambda: wheel(*args),
          "older": lambda: older(*args), "ordered": lambda: ordered(args), "named": lambda: named(*args)}[command]())
PY
}

# ----------------------------------------------------------------------------------------------------- the env file
# These two are citar-backup's own lines (deploy/citar-backup.sh), so both read a setting and a path exactly alike.

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

# The live database as the service finds it (citar-backup's rules: CITAR_DB_URL, else citar.db in the data directory,
# else in the state directory), with the tests' root; nothing when it is not SQLite.
live_db() {
  local state data url path
  state="$(at "$(env_get CITAR_STATE_DIR)")"
  state="${state:-$(at "$STATE_DIR")}"
  data="$(at "$(env_get CITAR_DATA_DIR)")"
  data="${data:-$state}"
  url="$(env_get CITAR_DB_URL)"
  case "$url" in
    "") printf '%s' "$data/citar.db" ;;
    sqlite*:///*) path="${url#*:///}"; at "${path%%\?*}" ;;
  esac
}

# Every live state path as the service finds it (citar-backup's rules), as the server names it: the state, data, saves,
# configuration and benchmarks directories, and the database's folder.
live_paths() {
  local state data saves bench db
  state="$(at "$(env_get CITAR_STATE_DIR)")"
  state="${state:-$(at "$STATE_DIR")}"
  data="$(at "$(env_get CITAR_DATA_DIR)")"
  saves="$(at "$(env_get CITAR_SAVE_DIR)")"
  bench="$(at "$(env_get CITAR_BENCH_DIR)")"
  if [ -z "$bench" ] && [ -n "$saves" ]; then bench="$(dirname "$saves")/benchmarks"; fi
  db="$(live_db)"
  show "$(printf '%s\n' "$state" "${data:-$state}" "${saves:-$state/saves}" \
    "$(at "$(env_get CITAR_CONFIG_DIR)")" "${bench:-$state/benchmarks}" "${db:+$(dirname "$db")}" | sed '/^$/d')"
  printf '\n'
}

# The env file's keys, one a line (comments and `export` lines left out).
env_keys() {
  [ -f "$ROOT$ENV_FILE" ] || return 0
  awk '{ sub(/^[ \t\r]+/, "") } $0 == "" || /^[#;]/ { next }
       { eq = index($0, "="); if (eq == 0) next; name = substr($0, 1, eq - 1); sub(/[ \t]+$/, "", name)
         if (name ~ /^[A-Za-z_][A-Za-z0-9_]*$/) print name }' "$ROOT$ENV_FILE" | sort -u
}

# Where the service answers itself: the env file's CITAR_HOST and CITAR_PORT (the server's defaults otherwise).
direct_url() {
  local host port
  host="$(env_get CITAR_HOST)"
  port="$(env_get CITAR_PORT)"
  case "${host:-127.0.0.1}" in
    0.0.0.0) host="127.0.0.1" ;;
    ::) host="[::1]" ;;
    *:*) host="[$host]" ;;
    *) host="${host:-127.0.0.1}" ;;
  esac
  printf 'http://%s:%s' "$host" "${port:-8765}"
}

# ----------------------------------------------------------------------------------------------------- the database

# The alembic revision of database $1 (with the root; read only): the revision, "none" (no file, table or row),
# "external" (not SQLite) or "unknown" (it could not be read).
revision() {
  local out
  [ -n "$1" ] || { printf 'external'; return 0; }
  [ -f "$1" ] || { printf 'none'; return 0; }
  if out="$(sqlite3 -readonly "$1" 'SELECT version_num FROM alembic_version;' 2>&1)"; then
    out="$(printf '%s\n' "$out" | head -n 1)"
    printf '%s' "${out:-none}"
  else
    case "$out" in *"no such table"*) printf 'none' ;; *) printf 'unknown' ;; esac
  fi
}

# Whether release $1's migrations (its venv's citar/migrations) know revision $2.
knows() {
  local escaped
  case "$2" in none) return 0 ;; unknown|external|"") return 1 ;; esac
  escaped="$(printf '%s' "$2" | sed 's/[][\.*^$/]/\\&/g')"
  grep -Eqs "^revision(: *str)? *= *['\"]${escaped}['\"]" \
    "$ROOT$RELEASES/$1"/venv/lib/python3*/site-packages/citar/migrations/versions/*.py
}

# Puts snapshot $2 back as database $1 (with the root) in one rename, with the owner and mode $1 had (or $3's, the
# database moved aside before it); its write-ahead log and journal go (they belong to the database being replaced).
# Without either, the snapshot's owner is its folder's.
restore_db() {
  local db="$1" snapshot="$2" staged="$1.citar-restore" reference="${3:-$1}"
  [ -e "$reference" ] || reference="$(dirname "$db")"
  cp "$snapshot" "$staged" || return 1
  if [ -z "$ROOT" ]; then
    chown --reference="$reference" "$staged" || return 1
  fi
  if [ -f "$reference" ]; then chmod --reference="$reference" "$staged" || return 1; else chmod 0640 "$staged"; fi
  rm -f "$db-wal" "$db-shm" "$db-journal"
  mv -f "$staged" "$db"
}

# ----------------------------------------------------------------------------------------------------- the layout

running_release() {
  local target
  [ -L "$ROOT$CURRENT" ] || return 0
  target="$(readlink "$ROOT$CURRENT")"
  target="${target%/}"
  printf '%s' "${target##*/}"
}

# Every release folder's version, oldest first.
release_versions() {
  local names=() dir
  for dir in "$ROOT$RELEASES"/*/; do
    [ -d "$dir" ] || continue
    dir="${dir%/}"
    names+=("${dir##*/}")
  done
  [ "${#names[@]}" -gt 0 ] || return 0
  probe ordered "${names[@]}"
}

# The newest complete release older than $1, or nothing.
release_before() {
  local version found=""
  while IFS= read -r version; do
    [ -f "$ROOT$RELEASES/$version/.complete" ] || continue
    if [ "$(probe older "$version" "$1"; echo $?)" = 0 ]; then found="$version"; fi
  done < <(release_versions)
  printf '%s' "$found"
}

# Points current at releases/$1 in one rename, so there is never a moment without a current release.
switch_current() {
  run ln -sfn "releases/$1" "$ROOT$CURRENT.switching" && run mv -T "$ROOT$CURRENT.switching" "$ROOT$CURRENT"
}

stamp() {   # a UTC time for a folder's name; in a plan, its placeholder
  if [ "$PLAN" -eq 1 ]; then printf '<stamp>'; else date -u +%Y%m%d-%H%M%S; fi
}

own() {   # $1 a path: the service account's (not under the tests' root)
  [ -n "$ROOT" ] || chown "$SERVICE_USER:$SERVICE_USER" "$1"
}

# A file put in place whole: written beside its target, given its mode and root's ownership, then renamed over it (so
# a running script, this one included, keeps the file it is reading).
put() {   # $1 source, $2 target (with the root), $3 mode
  local staged="$2.citar-deploy-new"
  mkdir -p "$(dirname "$2")" && cp "$1" "$staged" && chmod "$3" "$staged" || return 1
  if [ -z "$ROOT" ]; then chown root:root "$staged" || return 1; fi
  mv -f "$staged" "$2"
}

record() {   # one line in the deploy log
  local line
  line="$(date -u '+%Y-%m-%dT%H:%M:%SZ') $*"
  if act "append to $LOG_FILE: <time> $*"; then
    { mkdir -p "$(dirname "$ROOT$LOG_FILE")" && printf '%s\n' "$line" >> "$ROOT$LOG_FILE"; } \
      || warn "could not write $LOG_FILE: $line"
  fi
}

# ----------------------------------------------------------------------------------------------------- preflight

pf_root() {
  if [ -n "$ROOT" ]; then
    local tool where
    for tool in systemctl systemd-run; do
      where="$(command -v "$tool" || true)"
      case "$where" in
        ""|/bin/*|/sbin/*|/usr/bin/*|/usr/sbin/*|/usr/local/bin/*|/usr/local/sbin/*)
          die "CITAR_DEPLOY_ROOT is for tests: put stand-ins for systemctl and systemd-run first on PATH (found '$where')" ;;
      esac
    done
    ok "root: not needed under CITAR_DEPLOY_ROOT (tests)"
    return 0
  fi
  [ "$(id -u)" = 0 ] || refuse "Run it as root (sudo): it stops and starts services and writes /opt, /etc and /var."
  ok "root"
}

pf_tools() {
  local tool missing=()
  for tool in systemctl systemd-run sqlite3 sha256sum flock; do
    command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
  done
  [ "${#missing[@]}" -eq 0 ] || refuse "Missing here: ${missing[*]} (apt install sqlite3 for sqlite3)."
  command -v "$PY" >/dev/null 2>&1 || refuse "No $PY here: every release's venv is made with it."
  "$PY" -c 'import venv, ensurepip' >/dev/null 2>&1 \
    || refuse "$PY cannot make a venv with pip (apt install python3.12-venv)."
  ok "tools: systemctl, systemd-run, sqlite3, sha256sum, flock, and $PY with venv"
}

pf_user() {
  id -u "$SERVICE_USER" >/dev/null 2>&1 || refuse "No $SERVICE_USER account here: the server runs as it."
  ok "account: $SERVICE_USER"
}

pf_env() {
  [ -f "$ROOT$ENV_FILE" ] || refuse "No $ENV_FILE: the server's configuration (env.example names its settings)."
  if [ -z "$SITE_URL" ]; then
    SITE_URL="$(env_get CITAR_PUBLIC_ORIGIN)"
    [ -n "$SITE_URL" ] || refuse "$ENV_FILE names no CITAR_PUBLIC_ORIGIN, and no --site-url was given: the health
check goes through the site as visitors reach it."
  fi
  SITE_URL="${SITE_URL%/}"
  DIRECT_URL="$(direct_url)"
  ok "configuration: $ENV_FILE; CITAR answers at $DIRECT_URL, and through $SITE_URL"
}

# The bundle's files against its SHA256SUMS, every one listed and nothing more; its BUNDLE; the files it must hold.
pf_bundle() {
  local dir listed actual line sum name got f
  [ -d "$BUNDLE" ] || refuse "No bundle at $BUNDLE."
  BUNDLE="$(cd "$BUNDLE" && pwd -P)"
  dir="$BUNDLE"
  [ -f "$dir/SHA256SUMS" ] || refuse "$(show "$dir") has no SHA256SUMS: not a deploy bundle (scripts/deploy/bundle.py)."
  listed="$(mktemp)"
  actual="$(mktemp)"
  while IFS= read -r line || [ -n "$line" ]; do
    [ -n "$line" ] || continue
    sum="${line%% *}"
    name="${line#* }"
    name="${name# }"
    name="${name#\*}"
    if [[ ! "$sum" =~ ^[0-9a-f]{64}$ ]] || [ -z "$name" ] || [[ "$name" == /* || "$name" == *..* ]]; then
      rm -f "$listed" "$actual"
      refuse "$(show "$dir")/SHA256SUMS has a line that is not '<sha256>  <file>': $line"
    fi
    printf '%s\n' "$name" >> "$listed"
    if [ ! -f "$dir/$name" ]; then
      rm -f "$listed" "$actual"
      refuse "The bundle does not match its SHA256SUMS: $name is missing."
    fi
    got="$(sha256sum "$dir/$name" | cut -d' ' -f1)"
    if [ "$got" != "$sum" ]; then
      rm -f "$listed" "$actual"
      refuse "The bundle does not match its SHA256SUMS: $name: its SHA-256 is $got, and SHA256SUMS says $sum."
    fi
  done < "$dir/SHA256SUMS"
  (cd "$dir" && find . -type f ! -name SHA256SUMS -printf '%P\n') | sort > "$actual"
  sort -o "$listed" "$listed"
  f="$(comm -13 "$listed" "$actual" | head -n 1)"
  rm -f "$listed" "$actual"
  [ -z "$f" ] || refuse "The bundle does not match its SHA256SUMS: $f is in the bundle and not in SHA256SUMS."
  [ -f "$dir/BUNDLE" ] || refuse "$(show "$dir") has no BUNDLE file: not a deploy bundle (scripts/deploy/bundle.py)."
  V="$(sed -n 's/^version=//p' "$dir/BUNDLE" | head -n 1)"
  P="$(sed -n 's/^pep440=//p' "$dir/BUNDLE" | head -n 1)"
  { [[ "$P" =~ ^[0-9]+\.[0-9]+\.[0-9]+((a|b|rc)[0-9]+)?$ ]] && [ -n "$V" ]; } \
    || refuse "$(show "$dir")/BUNDLE names no version (version= and pep440=)."
  for f in "${BUNDLE_NEEDS[@]}" "citar-$P-server-constraints.txt"; do
    [ -f "$dir/$f" ] || refuse "The bundle has no $f."
  done
  WHEELS=("$dir"/citar-*.whl)
  { [ "${#WHEELS[@]}" -eq 1 ] && [ -f "${WHEELS[0]}" ]; } || refuse "The bundle must hold exactly one CITAR wheel."
  WHEEL="${WHEELS[0]##*/}"
  CONSTRAINTS="citar-$P-server-constraints.txt"
  ok "bundle: $(show "$dir"), CITAR $V: every file as its SHA256SUMS says"
}

pf_wheel() {
  local said
  said="$(probe wheel "$WHEEL" "$P")" || refuse "$said."
  ok "wheel: $WHEEL, for this machine"
}

# The nearest existing folder of $1 (with the root).
existing() {
  local path="$1"
  while [ ! -e "$path" ] && [ "$path" != / ] && [ -n "$path" ]; do path="$(dirname "$path")"; done
  printf '%s' "${path:-/}"
}

pf_disk() {
  local where free
  for where in "$OPT" /var; do
    free="$(df -Pk "$(existing "$ROOT$where")" 2>/dev/null | awk 'NR == 2 { print int($4 / 1024) }')"
    [ -n "$free" ] || refuse "df could not read the free space under $where."
    [ "$free" -ge "$NEED_MB" ] || refuse "Too little disk under $where: $free MB free, and a deploy needs $NEED_MB MB
(a release's venv, pip's downloads and the backup)."
  done
  ok "disk: at least $NEED_MB MB free under $OPT and /var"
}

pf_sites() {   # $1 "before" or "after"
  local said
  [ -f "$ROOT$CHECKS" ] || refuse "No $CHECKS: the other sites this box serves, one URL a line (an empty file if it
serves none). Make it from the server's own records."
  if ! said="$(probe sites "$ROOT$CHECKS" 2>&1)"; then
    refuse "Another site on this box does not answer 200, and a box already in trouble is not deployed to:
$said"
  fi
  ok "other sites: $said URL(s) in $CHECKS, each answering 200"
}

pf_citar() {   # $1 the version that runs (or - when its /api/meta names none)
  local said
  said="$(probe health "$DIRECT_URL" "$SITE_URL" "$1" 10)" \
    || refuse "CITAR does not answer as it should before the deploy ($said). Look at it first (journalctl -u citar)."
  ok "CITAR $( [ "$1" = - ] && printf '%s' "$OLD" || printf '%s' "$1") answers: $(show "$said")"
}

# The env file's settings that neither the bundle's env.example nor the release's own code names (0.1.5's, which
# 0.1.6 ignores): shown, and left as they are.
pf_env_keys() {
  local named unknown
  named="$(probe named "$BUNDLE/$WHEEL" "$BUNDLE/env.example")" || refuse "The wheel's settings could not be read."
  unknown="$(env_keys | grep -vxF -f <(printf '%s\n' "$named") || true)"
  if [ -n "$unknown" ]; then
    ok "settings: $ENV_FILE sets $(printf '%s' "$unknown" | tr '\n' ' ' | sed 's/ $//'), which neither env.example
      nor CITAR $V's code names: left as they are (CITAR $V ignores them)"
  else
    ok "settings: every setting in $ENV_FILE is one CITAR $V reads"
  fi
}

# The installed unit names the layout's paths, which this script deploys and backs up.
pf_unit() {
  local unit="$ROOT$UNITS/citar.service" value
  [ -f "$unit" ] || refuse "No $UNITS/citar.service: set this server up with install.sh --server first."
  for value in "ExecStart=$CURRENT/venv/bin/citar serve" "WorkingDirectory=$STATE_DIR" "EnvironmentFile=$ENV_FILE"; do
    grep -qxF "$value" "$unit" || refuse "The installed citar.service has no '$value': it was not written for the
release layout this script deploys (design/phase5-release.md P5.13.1)."
  done
  ok "service: $UNITS/citar.service runs $CURRENT, with $STATE_DIR and $ENV_FILE"
}

pf_database() {
  DB="$(live_db)"
  BEFORE_REV="$(revision "$DB")"
  if [ -z "$DB" ]; then
    ok "database: not SQLite (CITAR_DB_URL): back it up with its own tools first; a failed release cannot be undone here"
  else
    ok "database: $(show "$DB"), at revision $BEFORE_REV"
  fi
}

# One citar-deploy at a time. The folder's mode is set only once the lock is held, so a second run that stops at the
# lock changes nothing under a first one's verification (0711: root lists it, citar only passes through to the
# verification's own folder).
lock() {
  [ "$PLAN" -eq 0 ] || return 0
  mkdir -p "$ROOT$WORK" || die "cannot make $WORK"
  exec 9>>"$ROOT$WORK/lock" || die "cannot open $WORK/lock"
  flock -n 9 || die "another citar-deploy is running (it holds $WORK/lock)."
  chmod 0711 "$ROOT$WORK" || die "cannot set $WORK's mode"
}

# Signals once something that runs is changed. A hang-up (a dropped SSH session) and a closed output are ignored to the
# end of the command, so it finishes or undoes itself instead of stopping half way; Ctrl-C or a TERM undoes an install
# or a cutover (UNDO says how far it has gone) as a failed release is undone; a rollback or an uncutover, once begun,
# runs to its end. A signal is acted on once the command it arrives during has ended (Ctrl-C ends that one too).
UNDO=""
SWITCH_FROM=""
SWITCH_REV=""
on_signal() {   # $1 the signal's name
  trap '' INT TERM HUP PIPE
  case "$UNDO" in
    install) undo_install "$SWITCH_FROM" "$SWITCH_REV" "interrupted (SIG$1)" ;;
    cutover-stopped) cutover_abort "interrupted (SIG$1)" ;;
    cutover) cutover_failed "interrupted (SIG$1)" ;;
  esac
  die "interrupted (SIG$1)"
}
guard() {   # $1 what an INT or TERM undoes from here: install, cutover-stopped, cutover; or "" for nothing (runs on)
  [ "$PLAN" -eq 0 ] || return 0
  UNDO="$1"
  trap '' HUP PIPE
  if [ -n "$1" ]; then
    trap 'on_signal INT' INT
    trap 'on_signal TERM' TERM
  else
    trap '' INT TERM
  fi
}
unguard() {   # the switch is recorded: Ctrl-C and TERM act as usual again (a hang-up stays ignored to the end)
  [ "$PLAN" -eq 0 ] || return 0
  UNDO=""
  trap - INT TERM
}

STEP=0
step() {   # the next numbered step's heading, after a blank line
  STEP=$((STEP + 1))
  say ""
  say "$STEP. $*"
}

header() {
  if [ "$PLAN" -eq 1 ]; then
    say "citar-deploy $COMMAND: the plan. Every check runs; nothing is changed (run it without --plan to do it)."
  else
    say "citar-deploy $COMMAND"
  fi
  say ""
  say "Before anything changes:"
}

# ----------------------------------------------------------------------------------------------------- the steps

# RUN_DIR holds the copies of the units and scripts a switch replaces, the only ones an undo can put back: it is
# /var/lib/citar-deploy/run.<stamp> (root's, 0700), made when the first file is replaced and removed only once the
# deploy is recorded or its undo has put every file back, never on another exit (it is what a hand undo starts from).
RUN_DIR=""
VERIFY_DIR=""
cleanup() {
  [ -z "$VERIFY_DIR" ] || rm -rf "$VERIFY_DIR"
}
trap cleanup EXIT

# Stage releases/$P: a venv of its own, the wheel and constraints beside it, pip held to a scope that cannot starve
# the other sites, its freeze. Returns 1 when pip fails.
stage() {
  local rel="$RELEASES/$P"
  if [ "$REINSTALL" -eq 1 ] && [ -e "$ROOT$rel" ]; then
    run rm -rf "$ROOT$rel" || return 1
  fi
  run mkdir -p "$ROOT$rel" || return 1
  run cp "$BUNDLE/$WHEEL" "$BUNDLE/$CONSTRAINTS" "$ROOT$rel/" || return 1
  run "$PY" -m venv "$ROOT$rel/venv" || return 1
  run systemd-run --scope --quiet ${SLICE_FLAG[@]+"${SLICE_FLAG[@]}"} -p MemoryMax=500M -p CPUWeight=20 -- \
    "$ROOT$rel/venv/bin/pip" install --disable-pip-version-check --no-cache-dir --only-binary=:all: \
    --constraint "$ROOT$rel/$CONSTRAINTS" "$ROOT$rel/${WHEEL}[server]" || return 1
  if act "$rel/venv/bin/pip freeze > $rel/freeze.txt"; then
    "$ROOT$rel/venv/bin/pip" freeze --disable-pip-version-check > "$ROOT$rel/freeze.txt" || return 1
  fi
  if act "sha256sum $WHEEL > $rel/SHA256SUMS"; then
    (cd "$ROOT$rel" && sha256sum "$WHEEL" > SHA256SUMS) || return 1
  fi
}

# The live state paths a verification must not reach (with the root), for InaccessiblePaths=: each of live_paths but
# "/", any that holds citar-deploy's own folder or the releases (hiding those would hide the verification itself), and
# any inside another (hidden with it).
hidden_paths() {
  local path above hidden=()
  while IFS= read -r path; do
    case "$path" in ""|/) continue ;; esac
    case "$WORK/" in "$path"/*) continue ;; esac
    case "$OPT/" in "$path"/*) continue ;; esac
    for above in ${hidden[@]+"${hidden[@]}"}; do
      case "$path" in "$above"/*) continue 2 ;; esac
    done
    hidden+=("$path")
    printf '%s\n' "$ROOT$path"
  done < <(live_paths | LC_ALL=C sort -u)
}

# The verification's own properties (VPROPS), for folder $1: as citar, there, its state and home inside it, the live
# state out of its reach, held to the deploy's share of the box. citar.service's sandbox comes after them.
verify_props() {
  local v="$1" path
  VPROPS=(-p "User=$SERVICE_USER" -p "Group=$SERVICE_USER" -p "WorkingDirectory=$v"
    -p "Environment=CITAR_STATE_DIR=$v/state" -p "Environment=HOME=$v/state" -p "EnvironmentFile=$v/citar.env"
    -p "ReadWritePaths=$v")
  while IFS= read -r path; do VPROPS+=(-p "InaccessiblePaths=-$path"); done < <(hidden_paths)
  VPROPS+=("${VERIFY_LIMITS[@]}")
}

# The staged release's commands as the service runs them (systemd-run as citar, with citar.service's sandbox), with
# every state setting and the home in the verification's own folder.
as_service() {
  systemd-run --quiet --wait --collect --pipe --service-type=exec ${SLICE_FLAG[@]+"${SLICE_FLAG[@]}"} \
    "${VPROPS[@]}" "${SANDBOX[@]}" -- "$@" < /dev/null
}

# Verify the staged release before anything that runs changes: as citar, with the env file's settings but every state
# setting pointed at a fresh folder (removed afterwards), so the new code never opens the live database. citar
# --version is V; its engine loads from its venv without the test operations; citar doctor passes, its job host
# verifiable. Returns 1 (having said why) when it does not.
verify() {
  local rel="$RELEASES/$P" v="$WORK/verify.<random>" out status facts line word shown=""
  if act "mkdir $v ($SERVICE_USER's, 0700) and $v/state; $v/citar.env: $ENV_FILE without ${STATE_KEYS[*]} ${HOME_KEYS[*]} (each set inside $v) or any other setting naming a live state path"; then
    # In the lock's folder (0711, lock): a folder citar can enter, under one only root may list.
    VERIFY_DIR="$(mktemp -d "$ROOT$WORK/verify.XXXXXX")" || return 1
    v="$VERIFY_DIR"
    mkdir "$v/state" && own "$v" && own "$v/state" && chmod 0700 "$v" "$v/state" || return 1
    {
      # Every state setting and the home go, and so does any other setting whose value names a live state path (a
      # file kept in the state directory, say): the new code sees none of them.
      awk -v keys=" ${STATE_KEYS[*]} ${HOME_KEYS[*]} " -v live="$(live_paths)" -v dropped="$v/dropped" '
        BEGIN { n = split(live, dirs, "\n") }
        { line = $0; sub(/^[ \t\r]+/, "", line) }
        line != "" && line !~ /^[#;]/ && index(line, "=") > 0 {
          name = substr(line, 1, index(line, "=") - 1); sub(/[ \t]+$/, "", name)
          if (index(keys, " " name " ") > 0) next
          value = substr(line, index(line, "=") + 1)
          for (i = 1; i <= n; i++) {
            if (dirs[i] == "" || dirs[i] == "/") continue
            at = index(value, dirs[i])
            after = substr(value, at + length(dirs[i]), 1)
            if (at > 0 && (after == "" || after == "/" || after ~ /["\047 \t\r]/)) {
              print name >> dropped
              next
            }
          }
        }
        { print }' "$ROOT$ENV_FILE"
      printf '\n# citar-deploy: the verification'"'"'s own state\n'
      printf 'CITAR_STATE_DIR=%s\nCITAR_DATA_DIR=%s\nCITAR_DB_URL=sqlite:///%s\n' "$v/state" "$v/state" "$v/state/citar.db"
      printf 'CITAR_SAVE_DIR=%s\nCITAR_CONFIG_DIR=%s\nCITAR_BENCH_DIR=%s\nCITAR_KEYS_FILE=%s\n' \
        "$v/state/saves" "$v/state/config" "$v/state/benchmarks" "$v/state/config/keys.enc"
      # The home last: an environment file's settings win over the home systemd gives citar (systemd.exec(5)).
      printf 'HOME=%s\nXDG_CONFIG_HOME=%s\nXDG_DATA_HOME=%s\nXDG_CACHE_HOME=%s\nXDG_STATE_HOME=%s\n' \
        "$v/state" "$v/state/.config" "$v/state/.local/share" "$v/state/.cache" "$v/state/.local/state"
    } > "$v/citar.env" && chmod 0600 "$v/citar.env" || return 1
    if [ -f "$v/dropped" ]; then
      say "      left out, as they name a live state path: $(sort -u "$v/dropped" | tr '\n' ' ' | sed 's/ $//')"
      rm -f "$v/dropped"
    fi
  fi
  verify_props "$v"
  for word in "${VPROPS[@]}"; do shown+=" $(show "$word")"; done
  say "   each command below runs as: systemd-run --quiet --wait --collect --pipe --service-type=exec${SLICE:+ --slice=$SLICE}$shown, and citar.service's sandbox (${#SANDBOX[@]} more arguments: ProtectSystem=strict ... SystemCallErrorNumber=EPERM)"
  if act "$rel/venv/bin/citar --version: CITAR $V"; then
    out="$(as_service "$ROOT$rel/venv/bin/citar" --version 2>&1)"
    status=$?
    line="$(printf '%s\n' "$out" | tail -n 1)"
    { [ "$status" -eq 0 ] && [ "$(probe_pep440 "${line#CITAR }")" = "$P" ]; } \
      || { say "   FAILED: citar --version said '$line' (exit $status), not CITAR $V"; return 1; }
    ok "$line"
  fi
  if act "$rel/venv/bin/python -P -c '$ENGINE_FACTS': the engine from $rel/venv, without the test operations"; then
    out="$(as_service "$ROOT$rel/venv/bin/python" -P -c "$ENGINE_FACTS" 2>&1)"
    status=$?
    facts="$(printf '%s\n' "$out" | tail -n 1)"
    case "$status:$facts" in
      0:\[\""$ROOT$rel"/venv/lib/*\",\ false\]) ok "the engine: $(show "$facts")" ;;
      *) say "   FAILED: the engine said '$(show "$facts")' (exit $status): not $rel/venv's, or with the test operations"
         return 1 ;;
    esac
  fi
  if act "$rel/venv/bin/citar doctor: exits 0 (no FAIL line), and its jobs line says verifiable"; then
    out="$(as_service "$ROOT$rel/venv/bin/citar" doctor 2>&1)"
    status=$?
    line="$(printf '%s\n' "$out" | grep -E '^\[.{6}\] jobs: ' | head -n 1)"
    if [ "$status" -ne 0 ] || [[ "$line" != "[  ok  ] jobs: verifiable"* ]]; then
      say "   FAILED: citar doctor (exit $status):"
      printf '%s\n' "$out" | grep -E '^\[ (FAIL|warn) \]|jobs:|-> ' | sed 's/^/      /'
      return 1
    fi
    ok "citar doctor: no problem; $line"
  fi
  if act "rm -rf $v"; then
    rm -rf "$VERIFY_DIR"
    VERIFY_DIR=""
  fi
  if act "$rel/.complete: verified by citar-deploy: CITAR $V"; then
    printf 'verified by citar-deploy: CITAR %s\n' "$V" > "$ROOT$rel/.complete" || return 1
  fi
}

probe_pep440() {
  printf '%s' "$1" | sed -e 's/-alpha\./a/' -e 's/-beta\./b/' -e 's/-rc\./rc/'
}

# The backup folders' names (citar-backup's <UTC stamp>[-N]), one a line.
backup_names() {
  local dir
  for dir in "$ROOT$BACKUPS"/*/; do
    dir="${dir%/}"
    dir="${dir##*/}"
    [[ ! "$dir" =~ ^[0-9]{8}-[0-9]{6}(-[0-9]+)?$ ]] || printf '%s\n' "$dir"
  done
}

# The citar-backup a switch takes with the service stopped (a oneshot, so starting it waits for it). Sets BACKUP to its
# folder (the one that was not there before it ran: a folder dated ahead, or a clock set back, cannot pass for it), or
# returns 1 when there is no trustworthy new one. Exit 3 is a whole backup whose offsite copy failed.
BACKUP=""
take_backup() {   # $1 "release" to require a RELEASE file (0.1.6's backup), "any" for 0.1.5's
  local before status=0 newest
  before="$(backup_names)"
  systemctl start citar-backup || status=$?
  if [ "$status" -ne 0 ]; then
    [ "$(systemctl show -p ExecMainStatus --value citar-backup 2>/dev/null)" = 3 ] || return 1
    warn "the backup's offsite copy failed (journalctl -u citar-backup); the backup on this disk is whole"
  fi
  newest="$(backup_names | grep -vxF -f <(printf '%s\n' "$before") | sort | tail -n 1)"
  [ -n "$newest" ] || return 1
  BACKUP="$BACKUPS/$newest"
  [ ! -e "$ROOT$BACKUP/UNTRUSTWORTHY" ] || return 1
  [ "$1" != release ] || [ -f "$ROOT$BACKUP/RELEASE" ]
}

# The bundle's units and scripts installed where they differ, each replaced one kept in the run's folder so a failed
# release can be undone; the nginx site compared and never written.
REPLACED=()
ADDED=()
install_files() {
  local entry src dest mode rest
  for entry in "${FILES[@]}"; do
    src="${entry%%|*}"
    rest="${entry#*|}"
    dest="${rest%%|*}"
    mode="${rest#*|}"
    if [ -f "$ROOT$dest" ] && cmp -s "$BUNDLE/$src" "$ROOT$dest"; then
      say "   $dest: the same as the bundle's $src"
      continue
    fi
    if [ -f "$ROOT$dest" ]; then
      if act "$dest <- the bundle's $src ($mode): it differs (the difference is shown as it is replaced)"; then
        # Shown only: diff's 1 (they differ) is the expected answer here, never this step's failure.
        { diff -u "$ROOT$dest" "$BUNDLE/$src" || true; } | unroot | sed 's/^/      /'
        if [ -z "$RUN_DIR" ]; then
          RUN_DIR="$(mktemp -d "$ROOT$WORK/run.$(date -u +%Y%m%d-%H%M%S).XXXXXX")" || return 1
          say "      (each file replaced is kept in $(show "$RUN_DIR") until the deploy is recorded or undone)"
        fi
        mkdir -p "$RUN_DIR/replaced$(dirname "$dest")" && cp -p "$ROOT$dest" "$RUN_DIR/replaced$dest" || return 1
        REPLACED+=("$dest|$mode")
      fi
    elif act "$dest <- the bundle's $src ($mode): new"; then
      ADDED+=("$dest")
    fi
    if [ "$PLAN" -eq 0 ]; then
      put "$BUNDLE/$src" "$ROOT$dest" "$mode" || return 1
    fi
  done
  if [ ! -f "$ROOT$NGINX_SITE" ]; then
    say "   $NGINX_SITE: none here (citar-deploy never writes an nginx site; docs/server/DEPLOY.md)"
  elif cmp -s "$BUNDLE/deploy/nginx-citar.conf" "$ROOT$NGINX_SITE"; then
    say "   $NGINX_SITE: the same as the bundle's deploy/nginx-citar.conf"
  elif act "$NGINX_SITE: never rewritten (certbot edited it); its difference from the bundle's deploy/nginx-citar.conf is shown"; then
    # Under pipefail diff's 1 (they differ, as a site certbot edited always does) would be this function's status, and
    # the switch would be undone for a file it never writes (found by the staging run, design/phase5-release.md P5.14).
    { diff -u "$ROOT$NGINX_SITE" "$BUNDLE/deploy/nginx-citar.conf" || true; } | unroot | sed 's/^/      /'
  fi
}

# The files install_files replaced put back, and the ones it added removed; RUN_DIR removed once all are back (else
# kept, and said).
undo_files() {
  local entry dest failed=0
  for entry in ${REPLACED[@]+"${REPLACED[@]}"}; do
    dest="${entry%%|*}"
    put "$RUN_DIR/replaced$dest" "$ROOT$dest" "${entry#*|}" || { warn "could not put $dest back"; failed=1; }
  done
  for dest in ${ADDED[@]+"${ADDED[@]}"}; do
    rm -f "$ROOT$dest" || { warn "could not remove $dest"; failed=1; }
  done
  if [ "$failed" -eq 0 ]; then
    drop_run_dir
  else
    warn "the files the switch replaced are kept in $(show "$RUN_DIR")"
  fi
}

drop_run_dir() {
  [ -z "$RUN_DIR" ] || rm -rf "$RUN_DIR"
  RUN_DIR=""
}

# /api/auth/config, /api/meta's version and the WebSocket handshake, directly and through the site, within 60 s.
healthy() {   # $1 the version (- for a release whose /api/meta names none); sets SAID
  SAID="$(probe health "$DIRECT_URL" "$SITE_URL" "$1" "$HEALTH_SECONDS")"
}

health_plan() {   # $1 the version
  say "   within ${HEALTH_SECONDS} s: $(show "$DIRECT_URL")/api/auth/config answers 200$( [ "$1" = - ] || printf ' and /api/meta CITAR %s' "$1");"
  say "   $SITE_URL at 127.0.0.1 (the site's own Host): /api/auth/config answers 200$( [ "$1" = - ] || printf ', and a WebSocket handshake on /ws/worker is answered by CITAR (101)')"
}

sites_after() {   # the other sites once more; a failure is loud, and left to the operator (exit 3)
  local said
  step "The other sites again:"
  if act "each URL in $CHECKS answers 200"; then
    if ! said="$(probe sites "$ROOT$CHECKS" 2>&1)"; then
      printf '\n%s\n%s\n%s\n\n' "!!! ANOTHER SITE ON THIS BOX DOES NOT ANSWER since the deploy:" "$said" \
        "!!! CITAR $1 runs. Nothing was done about it: the cause may not be CITAR's. To go back: $2" >&2
      SITES_FAILED=1
      return 1
    fi
    ok "the $said other site(s) answer 200"
  fi
}

# ----------------------------------------------------------------------------------------------------- install

cmd_install() {
  local p0 before_rev after_rev t0 downtime rel mark
  header
  pf_root
  pf_tools
  pf_user
  pf_env
  pf_bundle
  pf_wheel
  p0="$(running_release)"
  if [ -z "$p0" ]; then
    if [ -f "$ROOT$OPT/citar/__init__.py" ]; then
      refuse "This server runs 0.1.5's layout ($OPT/citar): its first deploy is citar-deploy cutover."
    fi
    refuse "No release runs here ($CURRENT): a new server is installed with install.sh --server."
  fi
  [ "$P" != "$p0" ] || refuse "CITAR $P is the release that runs: there is nothing to install."
  if [ "$(probe older "$P" "$p0"; echo $?)" = 0 ]; then
    refuse "CITAR $P is older than the release that runs ($p0): going back is citar-deploy rollback --to $P."
  fi
  # The release that runs is what a rollback goes back to, so it must be whole: without its .complete (a release made
  # by hand, or before 0.1.6), its own citar --version must name it, as install.sh asks, and it is marked below.
  mark=""
  if [ ! -f "$ROOT$RELEASES/$p0/.complete" ]; then
    mark="$("$ROOT$RELEASES/$p0/venv/bin/citar" --version 2>/dev/null | tail -n 1)"
    [ "$(probe_pep440 "${mark#CITAR }")" = "$p0" ] || refuse "$RELEASES/$p0 is the release that runs, but it has no
.complete and its citar --version says '${mark:-nothing}', not CITAR $p0: it may not be whole, and a rollback goes
back to it. Point current at a whole release first (citar-deploy rollback --to VERSION)."
    ok "the release that runs: $RELEASES/$p0 has no .complete, and its citar --version says $mark (marked below)"
  fi
  rel="$RELEASES/$P"
  if [ -e "$ROOT$rel" ] && [ "$REINSTALL" -eq 0 ]; then
    refuse "$rel is here already (an earlier install of it, or one that failed): --reinstall removes it and makes it
again."
  fi
  ok "releases: $CURRENT -> releases/$p0 runs; CITAR $P goes beside it$( [ ! -e "$ROOT$rel" ] || printf ' (its folder made again: --reinstall)')"
  pf_unit
  pf_disk
  pf_database
  pf_sites
  pf_citar "$p0"
  lock
  step "Stage CITAR $V in $rel (nothing that runs changes):"
  if [ -n "$mark" ] && act "$RELEASES/$p0/.complete: marked by citar-deploy: $mark, the release that runs"; then
    printf 'marked by citar-deploy: %s, the release that runs\n' "$mark" > "$ROOT$RELEASES/$p0/.complete" \
      || die "could not mark $RELEASES/$p0 complete; nothing that runs was changed."
  fi
  if ! stage; then
    die "pip could not stage CITAR $P (above). $rel is left without .complete for a look; nothing that runs was
changed. Run again with --reinstall."
  fi
  step "Verify CITAR $V as $SERVICE_USER, its state in a temporary folder (the live state untouched):"
  if ! verify; then
    die "CITAR $P failed its verification (above), so nothing that runs was changed. $rel is left without .complete
for a look; run again with --reinstall once it is fixed."
  fi
  step "Switch, with the service stopped (the downtime starts):"
  before_rev="$BEFORE_REV"
  if act "the database's revision, read now (sqlite3, read only): $before_rev"; then
    before_rev="$(revision "$DB")"
    say "      $before_rev"
  fi
  t0="$(date +%s)"
  # From here until the record, an interruption undoes the switch (guard) and a hang-up is ignored.
  SWITCH_FROM="$p0"
  SWITCH_REV="$before_rev"
  guard install
  if ! run systemctl stop citar; then
    systemctl start citar || true
    die "systemctl stop citar failed, so nothing was switched (citar was started again on $p0). $rel is staged and
verified: run this again once the service stops."
  fi
  if act "systemctl start citar-backup: a new backup in $BACKUPS without UNTRUSTWORTHY (else citar starts again on $p0 and nothing is switched)"; then
    if ! take_backup release; then
      systemctl start citar || true
      die "The backup before the switch failed or is not trustworthy (journalctl -u citar-backup), so nothing was
switched and citar was started again on $p0. $rel is staged and verified: run this again once the backup works."
    fi
    ok "backup: $BACKUP"
  fi
  switch_current "$P" || { undo_install "$p0" "$before_rev" "the switch of current failed"; }
  install_files || { undo_install "$p0" "$before_rev" "the units and scripts could not be installed"; }
  run systemctl daemon-reload || { undo_install "$p0" "$before_rev" "systemctl daemon-reload failed"; }
  run systemctl start citar || { undo_install "$p0" "$before_rev" "systemctl start citar failed"; }
  step "Health of CITAR $P (else it is undone: stopped, the database put back from step 3's backup if its revision"
  say "   moved, current back to $p0 with its units and scripts, started, and checked):"
  health_plan "$P"
  if [ "$PLAN" -eq 0 ]; then
    if ! healthy "$P"; then
      undo_install "$p0" "$before_rev" "CITAR $P did not answer as it should: $SAID"
    fi
    downtime=$(( $(date +%s) - t0 ))
    ok "$(show "$SAID")"
    ok "downtime: ${downtime} s (the backup and the start)"
    after_rev="$(revision "$DB")"
  else
    downtime="<n>"
    after_rev="<revision>"
  fi
  SITES_FAILED=0
  sites_after "$P" "sudo citar-deploy rollback --to $p0$( [ "$after_rev" = "$before_rev" ] || printf ' (the database moved from %s to %s: add --restore-backup %s)' "$before_rev" "$after_rev" "${BACKUP:-<backup>}")" || true
  step "Record and prune:"
  record "install version=$P previous=$p0 result=ok revision_before=$before_rev revision_after=$after_rev backup=${BACKUP:-<backup>} downtime=${downtime}s"
  unguard
  drop_run_dir
  prune_releases "$KEEP" "$p0"
  say ""
  if [ "$PLAN" -eq 1 ]; then
    say "End of the plan: nothing was changed."
    exit 0
  fi
  [ "$SITES_FAILED" -eq 0 ] || exit 3
  say "CITAR $P runs. $p0 stays in $RELEASES/$p0; the backup taken before the switch is $BACKUP."
}

# A failed install undone: stopped, the database put back when the release moved its revision, current back to the
# previous release with its units and scripts, started, checked; then exit 1, saying what happened.
undo_install() {   # $1 the previous release, $2 the revision before, $3 what failed
  local p0="$1" before_rev="$2" after_rev restored="no" back moved word
  # The undo runs to its end: a second Ctrl-C, a TERM or a hang-up does not cut it short.
  trap '' INT TERM HUP PIPE
  warn "$3: undoing the switch"
  systemctl stop citar || true
  after_rev="$(revision "$DB")"
  if [ -z "$DB" ]; then
    restored="no (the database is not SQLite: restore it with its own tools if CITAR $P migrated it)"
  elif [ "$after_rev" = "$before_rev" ]; then
    restored="no (its revision is still $before_rev)"
  elif [ "$before_rev" = none ]; then
    moved="$DB.made-by-$P"
    if mv -f "$DB" "$moved" && rm -f "$DB-wal" "$DB-shm" "$DB-journal"; then
      restored="yes: there was none before, and the one CITAR $P made (at $after_rev) is now $(show "$moved")"
    else
      restored="NO: CITAR $P made a database at $after_rev and it could not be moved aside"
    fi
  elif [ "$before_rev" = unknown ]; then
    restored="no (its revision could not be read before the switch; it is now $after_rev)"
  elif [ -n "$BACKUP" ] && [ -f "$ROOT$BACKUP/citar.db" ] && restore_db "$DB" "$ROOT$BACKUP/citar.db"; then
    restored="yes, from $BACKUP/citar.db (CITAR $P had moved its revision from $before_rev to $after_rev)"
  else
    restored="NO: CITAR $P moved its revision from $before_rev to $after_rev and the backup could not be put back"
  fi
  { ln -sfn "releases/$p0" "$ROOT$CURRENT.switching" && mv -T "$ROOT$CURRENT.switching" "$ROOT$CURRENT"; } \
    || warn "current could not be pointed back at releases/$p0"
  undo_files
  systemctl daemon-reload || true
  systemctl start citar || true
  if healthy "$p0"; then
    back="citar runs $p0 again ($(show "$SAID"))"
  else
    back="citar did not come back on $p0 either ($(show "$SAID")): see journalctl -u citar"
  fi
  case "$restored" in yes*) word=yes ;; NO*) word=failed ;; *) word=no ;; esac
  record "install version=$P previous=$p0 result=undone revision_before=$before_rev revision_after=$after_rev backup=${BACKUP:-none} restored=$word"
  die "CITAR $P was not switched to: $3.
$CURRENT -> releases/$p0 again, and $back.
The database was restored: $restored.
The backup is ${BACKUP:-none}; CITAR $P stays in $RELEASES/$P."
}

# ----------------------------------------------------------------------------------------------------- prune

# Releases beyond $1 removed, never the one that runs or $2, the one before it (a rollback's target: after an install,
# the release that ran until the switch, whatever its mark), so the plan and the run keep the same ones.
prune_releases() {   # $1 how many to keep, $2 the release before the one that runs
  local p0 before="$2" version kept=0 all=() keep=() name i
  if [ "$COMMAND" = install ]; then p0="$P"; else p0="$(running_release)"; fi
  [ -n "$p0" ] || { say "   no release runs: nothing to prune"; return 0; }
  while IFS= read -r version; do all+=("$version"); done < <(release_versions)
  if [ "$PLAN" -eq 1 ] && [ "$COMMAND" = install ]; then
    case " ${all[*]-} " in *" $P "*) ;; *) all+=("$P") ;; esac
    mapfile -t all < <(probe ordered "${all[@]}")
  fi
  keep=("$p0")
  [ -z "$before" ] || [ "$before" = "$p0" ] || keep+=("$before")
  kept="${#keep[@]}"
  for (( i = ${#all[@]} - 1; i >= 0; i-- )); do
    version="${all[$i]}"
    case " ${keep[*]} " in *" $version "*) continue ;; esac
    if [ "$kept" -lt "$1" ] && { [ -f "$ROOT$RELEASES/$version/.complete" ] || [ "$version" = "$P" ]; }; then
      keep+=("$version")
      kept=$((kept + 1))
    fi
  done
  for name in ${all[@]+"${all[@]}"}; do
    case " ${keep[*]} " in *" $name "*) continue ;; esac
    run rm -rf "$ROOT$RELEASES/$name" || warn "could not remove $RELEASES/$name"
  done
  say "   kept: ${keep[*]} (the release that runs, the one before it, and the newest up to $1)"
}

cmd_prune() {
  local p0
  header
  pf_root
  pf_tools
  p0="$(running_release)"
  [ -n "$p0" ] || refuse "No release runs here ($CURRENT)."
  ok "releases: $(release_versions | tr '\n' ' ' | sed 's/ $//'); $CURRENT -> releases/$p0"
  lock
  guard ""
  step "Prune the releases beyond $KEEP:"
  prune_releases "$KEEP" "$(release_before "$p0")"
  [ "$PLAN" -eq 0 ] || { say ""; say "End of the plan: nothing was changed."; }
}

# ----------------------------------------------------------------------------------------------------- rollback

# Whether backup folder $1 (as the server names it) is one release $2 can be restored from: a citar-backup folder,
# trustworthy, its database at a revision $2 knows.
usable_backup() {
  local rev
  { [ -f "$ROOT$1/RELEASE" ] && [ -f "$ROOT$1/citar.db" ] && [ ! -e "$ROOT$1/UNTRUSTWORTHY" ]; } || return 1
  rev="$(sed -n 's/^revision=//p' "$ROOT$1/RELEASE" | head -n 1)"
  knows "$2" "$rev"
}

# The backup to go back to release $1 with, from a database at a revision it does not know: the one the migrating
# deploy took (P5.13.3), from the deploy log: the last install recorded as ok that moved the revision, whose backup is
# one $1 can be restored from (so over two migrating deploys, the first one's, taken at a revision $1 knows); else the
# newest backup folder $1 can be restored from.
backup_for() {
  local line word words found="" result before after backup dir i folders=()
  if [ -f "$ROOT$LOG_FILE" ]; then
    while IFS= read -r line; do
      read -r -a words <<< "$line"
      [ "${words[1]-}" = install ] || continue
      result="" before="" after="" backup=""
      for word in "${words[@]}"; do
        case "$word" in
          result=*) result="${word#result=}" ;;
          revision_before=*) before="${word#revision_before=}" ;;
          revision_after=*) after="${word#revision_after=}" ;;
          backup=*) backup="${word#backup=}" ;;
        esac
      done
      { [ "$result" = ok ] && [ -n "$before" ] && [ "$before" != "$after" ]; } || continue
      case "$backup" in /*) ;; *) continue ;; esac
      if usable_backup "$backup" "$1"; then found="$backup"; fi
    done < "$ROOT$LOG_FILE"
  fi
  if [ -z "$found" ]; then
    for dir in "$ROOT$BACKUPS"/*/; do
      [ -d "$dir" ] && folders+=("$(show "${dir%/}")")
    done
    for (( i = ${#folders[@]} - 1; i >= 0; i-- )); do
      if usable_backup "${folders[$i]}" "$1"; then found="${folders[$i]}"; break; fi
    done
  fi
  printf '%s' "$found"
}

cmd_rollback() {
  local p0 t rev backup_rev info archive name dir stamp target folder aside inside suffix moved=()
  header
  pf_root
  pf_tools
  pf_env
  p0="$(running_release)"
  [ -n "$p0" ] || refuse "No release runs here ($CURRENT): there is nothing to roll back."
  t="${TO:-$(release_before "$p0")}"
  [ -n "$t" ] || refuse "No complete release older than $p0 is here to go back to (--to names one)."
  [ "$t" != "$p0" ] || refuse "CITAR $t is the release that runs."
  [ -f "$ROOT$RELEASES/$t/.complete" ] || refuse "$RELEASES/$t is not a complete release (no .complete)."
  ok "releases: $CURRENT -> releases/$p0 runs; back to $t"
  pf_database
  rev="$BEFORE_REV"
  if [ -n "$RESTORE" ]; then
    RESTORE="${RESTORE%/}"
    { [ -f "$ROOT$RESTORE/RELEASE" ] && [ -f "$ROOT$RESTORE/citar.db" ] && [ -f "$ROOT$RESTORE/state.tar.gz" ]; } \
      || refuse "$RESTORE is not a citar-backup folder (RELEASE, citar.db and state.tar.gz)."
    [ ! -e "$ROOT$RESTORE/UNTRUSTWORTHY" ] || refuse "$RESTORE is marked UNTRUSTWORTHY: $(head -c 300 "$ROOT$RESTORE/UNTRUSTWORTHY")"
    backup_rev="$(sed -n 's/^revision=//p' "$ROOT$RESTORE/RELEASE" | head -n 1)"
    knows "$t" "$backup_rev" || refuse "The backup $RESTORE holds a database at revision $backup_rev, which CITAR $t does
not know either."
    [ -n "$DB" ] || refuse "The database is not SQLite: restore it with its own tools, then roll back without
--restore-backup."
    ok "backup: $RESTORE, its database at revision $backup_rev, which CITAR $t knows"
  elif ! knows "$t" "$rev"; then
    if [ "$rev" = unknown ] || [ "$rev" = external ]; then
      refuse "The database's revision cannot be read ($rev), so whether CITAR $t can open it is not known. Roll back
with --restore-backup DIR (a citar-backup folder), or look at the database first."
    fi
    backup="$(backup_for "$t")"
    refuse "The database is at revision $rev, which CITAR $t does not know: CITAR $p0 migrated it, and $t would refuse
to start on it. Roll back with the backup taken before that migration:
    sudo citar-deploy rollback --to $t --restore-backup ${backup:-<a backup folder in $BACKUPS whose RELEASE names a revision $t knows>}"
  else
    ok "revision: $rev, which CITAR $t knows"
  fi
  lock
  # Once the service is stopped, the rollback runs to its end (the way back is itself the recovery).
  guard ""
  step "Stop CITAR $p0:"
  if ! run systemctl stop citar; then
    systemctl start citar || true
    die "systemctl stop citar failed, so nothing was rolled back."
  fi
  if [ -n "$RESTORE" ]; then
    step "Restore the state and the database from $RESTORE (each folder it names moved aside first, kept):"
    info="$(cat "$ROOT$RESTORE/RELEASE")"
    stamp="$(stamp)"
    moved=()
    for archive in $(printf '%s\n' "$info" | sed -n 's/^archives=//p' | head -n 1); do
      name="${archive%.tar.gz}"
      dir="$(printf '%s\n' "$info" | sed -n "s/^$name=//p" | head -n 1)"
      { [ -n "$dir" ] && [ -f "$ROOT$RESTORE/$archive" ]; } || { warn "$archive: no folder named for it; not restored"; continue; }
      if [ -e "$ROOT$dir" ]; then
        run mv -T "$ROOT$dir" "$ROOT$dir.before-restore-$stamp" || die "could not move $dir aside; nothing restored"
        moved+=("$dir")
      fi
      if act "mkdir $dir, with the owner and mode it had; tar -xzpf $RESTORE/$archive -C $dir"; then
        mkdir -p "$ROOT$dir" || die "could not make $dir"
        if [ -e "$ROOT$dir.before-restore-$stamp" ]; then
          chmod --reference="$ROOT$dir.before-restore-$stamp" "$ROOT$dir"
          [ -n "$ROOT" ] || chown --reference="$ROOT$dir.before-restore-$stamp" "$ROOT$dir"
        fi
        tar -xzpf "$ROOT$RESTORE/$archive" -C "$ROOT$dir" || die "could not extract $RESTORE/$archive into $dir"
      fi
    done
    target="$DB"
    # A database outside every folder moved aside (CITAR_DB_URL elsewhere) is moved aside itself, so what was written
    # since the backup is kept there too, never overwritten.
    aside=""
    inside=0
    for dir in ${moved[@]+"${moved[@]}"}; do
      case "$(show "$target")" in "$dir"/*) inside=1 ;; esac
    done
    if [ "$inside" -eq 0 ] && [ -e "$target" ]; then
      aside="$target.before-restore-$stamp"
      if act "mv -T $(show "$target") $(show "$aside") (with its -wal, -shm and -journal, where there)"; then
        mv -T "$target" "$aside" || die "could not move $(show "$target") aside; the database was not restored"
        for suffix in -wal -shm -journal; do
          if [ -e "$target$suffix" ]; then
            mv -T "$target$suffix" "$aside$suffix" \
              || die "could not move $(show "$target")$suffix aside; the database was not restored"
          fi
        done
      fi
    fi
    if act "$(show "$target") <- $RESTORE/citar.db (its write-ahead log and journal removed)"; then
      folder="$(dirname "$target")"
      mkdir -p "$folder"
      restore_db "$target" "$ROOT$RESTORE/citar.db" "$aside" \
        || die "could not put $RESTORE/citar.db back as $(show "$target")"
    fi
  fi
  step "Switch to CITAR $t and start it (its units are the installed ones):"
  switch_current "$t" || die "current could not be switched to releases/$t"
  run systemctl start citar || warn "systemctl start citar failed"
  health_plan "$t"
  if [ "$PLAN" -eq 0 ]; then
    healthy "$t" || die "CITAR $t did not answer as it should after the rollback: $SAID. See journalctl -u citar."
    ok "$(show "$SAID")"
  fi
  SITES_FAILED=0
  sites_after "$t" "look at the other sites first" || true
  step "Record:"
  record "rollback version=$t previous=$p0 result=ok revision_before=$rev restored=${RESTORE:-no}"
  if [ "$PLAN" -eq 1 ]; then
    say ""
    say "End of the plan: nothing was changed."
    exit 0
  fi
  [ "$SITES_FAILED" -eq 0 ] || exit 3
  say "CITAR $t runs; $p0 stays in $RELEASES/$p0."
}

# ----------------------------------------------------------------------------------------------------- cutover

# Recognises 0.1.5's layout: its source tree and venv in /opt/citar, and no release layout.
pf_old_layout() {
  [ ! -L "$ROOT$CURRENT" ] || refuse "This server already runs the release layout ($CURRENT): a release is installed
with citar-deploy install."
  [ ! -e "$ROOT$CUTOVER" ] || refuse "A cutover was made already ($CUTOVER): citar-deploy uncutover undoes it."
  { [ -f "$ROOT$OPT/citar/__init__.py" ] && [ -d "$ROOT$OPT/.venv" ]; } \
    || refuse "$OPT is not 0.1.5's layout (its citar/__init__.py and .venv): nothing to cut over from."
  OLD="$(sed -n 's/^__version__ *= *["'"'"']\([^"'"'"']*\)["'"'"'].*/\1/p' "$ROOT$OPT/citar/__init__.py" | head -n 1)"
  [ -n "$OLD" ] || refuse "$OPT/citar/__init__.py names no __version__."
  OLD_TREE="/opt/citar-$OLD"
  [ ! -e "$ROOT$OLD_TREE" ] || refuse "$OLD_TREE is here already: the archive of the old tree goes there."
  LAB=0
  [ ! -f "$ROOT$LAB_UNIT" ] || LAB=1
  ok "0.1.5's layout: CITAR $OLD from $OPT ($OPT/.venv)$( [ "$LAB" -eq 0 ] || printf ', with citar-lab.service')"
}

# The files a cutover replaces or removes, saved with the record so uncutover can put each back.
cutover_targets() {
  local entry rest
  for entry in "${FILES[@]}"; do
    rest="${entry#*|}"
    printf '%s\n' "${rest%%|*}"
  done
  printf '%s\n' "$LAB_UNIT"
}

cmd_cutover() {
  local stamp archive path saved=() absent=()
  header
  pf_root
  pf_tools
  pf_user
  pf_env
  pf_bundle
  pf_wheel
  pf_old_layout
  pf_disk
  pf_env_keys
  pf_sites
  pf_citar -
  lock
  stamp="$(stamp)"
  archive="/var/lib/citar-archive-$stamp"
  # From the stop until the record is written, an interruption or a failure starts CITAR 0.1.5 again (nothing else
  # has changed); from the record on, it undoes the cutover. A hang-up is ignored throughout.
  guard cutover-stopped
  step "Stop CITAR $OLD:"
  if ! run systemctl stop citar || { [ "$LAB" -eq 1 ] && ! run systemctl stop citar-lab; }; then
    cutover_abort "CITAR $OLD's services did not stop"
  fi
  step "CITAR $OLD's own backup:"
  if act "systemctl start citar-backup: a new backup in $BACKUPS without UNTRUSTWORTHY (else CITAR $OLD starts again and nothing changes)"; then
    take_backup any || cutover_abort "CITAR $OLD's backup failed or is not trustworthy (journalctl -u citar-backup)"
    ok "backup: $BACKUP"
  fi
  step "Archive CITAR $OLD (uncutover puts all of it back):"
  if act "mkdir -p $CUTOVER/files (root's, 0700); each of these saved there if it is here: $(cutover_targets | tr '\n' ' ' | sed 's/ $//')"; then
    { mkdir -p "$ROOT$CUTOVER/files" && chmod 0700 "$ROOT$CUTOVER"; } || cutover_abort "cannot make $CUTOVER"
    while IFS= read -r path; do
      if [ -e "$ROOT$path" ]; then
        { mkdir -p "$ROOT$CUTOVER/files$(dirname "$path")" && cp -p "$ROOT$path" "$ROOT$CUTOVER/files$path"; } \
          || cutover_abort "could not save $path"
        saved+=("$path")
      else
        absent+=("$path")
      fi
    done < <(cutover_targets)
    # Written whole, then renamed: uncutover never reads half a record.
    if ! {
      printf 'made=%s\nold_version=%s\nold_tree=%s\nstate_archive=%s\nbackup=%s\nlab=%s\nrelease=%s\n' \
        "$stamp" "$OLD" "$OLD_TREE" "$archive" "$BACKUP" "$LAB" "$P"
      printf 'saved=%s\nabsent=%s\n' "${saved[*]-}" "${absent[*]-}"
    } > "$ROOT$CUTOVER/CUTOVER.new"; then
      cutover_abort "could not write $CUTOVER/CUTOVER"
    fi
    mv -T "$ROOT$CUTOVER/CUTOVER.new" "$ROOT$CUTOVER/CUTOVER" || cutover_abort "could not write $CUTOVER/CUTOVER"
  fi
  guard cutover
  if [ "$LAB" -eq 1 ]; then
    run systemctl disable citar-lab || cutover_failed "systemctl disable citar-lab failed"
    run rm -f "$ROOT$LAB_UNIT" || cutover_failed "could not remove $LAB_UNIT"
  fi
  run mv -T "$ROOT$OPT" "$ROOT$OLD_TREE" || cutover_failed "could not move $OPT to $OLD_TREE"
  run mv -T "$ROOT$STATE_DIR" "$ROOT$archive" || cutover_failed "could not move $STATE_DIR to $archive"
  if act "mkdir $STATE_DIR ($SERVICE_USER:$SERVICE_USER 0750): CITAR $V makes its database there on its first start"; then
    { mkdir "$ROOT$STATE_DIR" && own "$ROOT$STATE_DIR" && chmod 0750 "$ROOT$STATE_DIR"; } \
      || cutover_failed "could not make a fresh $STATE_DIR"
  fi
  run mkdir -p "$ROOT$RELEASES" || cutover_failed "could not make $RELEASES"
  say "   From here on, a failure undoes the cutover (citar-deploy uncutover) and exits 1."
  step "Stage CITAR $V in $RELEASES/$P:"
  stage || cutover_failed "pip could not stage CITAR $P (above)"
  step "Verify CITAR $V as $SERVICE_USER, its state in a temporary folder:"
  verify || cutover_failed "CITAR $P failed its verification (above)"
  step "Switch (no snapshot: the archived state is the backup):"
  switch_current "$P" || cutover_failed "current could not be made"
  install_files || cutover_failed "the units and scripts could not be installed"
  run systemctl daemon-reload || cutover_failed "systemctl daemon-reload failed"
  run systemctl enable citar citar-backup.timer || cutover_failed "systemctl enable failed"
  run systemctl start citar || cutover_failed "systemctl start citar failed"
  step "Health of CITAR $P (else the cutover is undone):"
  health_plan "$P"
  if [ "$PLAN" -eq 0 ]; then
    healthy "$P" || cutover_failed "CITAR $P did not answer as it should: $SAID"
    ok "$(show "$SAID")"
  fi
  SITES_FAILED=0
  sites_after "$P" "sudo citar-deploy uncutover" || true
  step "Record:"
  record "cutover version=$P previous=$OLD result=ok archive=$OLD_TREE state_archive=$archive backup=${BACKUP:-<backup>}"
  unguard
  drop_run_dir
  say ""
  if [ "$PLAN" -eq 1 ]; then
    say "End of the plan: nothing was changed."
    exit 0
  fi
  say "CITAR $P runs, on a fresh $STATE_DIR. CITAR $OLD is archived in $OLD_TREE and $archive; citar-deploy uncutover"
  say "puts it back. Next (packaging/RELEASING.md, step 10): the first administrator (sudo citar-admin create-user ...),"
  say "the host machine's settings from the staging measurement, and the helpers registered again."
  [ "$SITES_FAILED" -eq 0 ] || exit 3
}

# A cutover stopped before its record, so before anything but the stop: CITAR 0.1.5 started again, and the partial
# record removed (an earlier cutover's record would refuse the next one). Exits 1.
cutover_abort() {
  trap '' INT TERM HUP PIPE
  warn "$1: starting CITAR $OLD again"
  systemctl start citar || warn "systemctl start citar failed"
  [ "$LAB" -eq 0 ] || systemctl start citar-lab || warn "systemctl start citar-lab failed"
  [ ! -e "$ROOT$CUTOVER" ] || rm -rf "$ROOT$CUTOVER"
  die "The cutover to CITAR $P stopped before it changed anything ($1): CITAR $OLD was started again."
}

# A cutover that failed (or was interrupted) once its record was written: undone by itself (uncutover's steps). Exits 1.
cutover_failed() {
  trap '' INT TERM HUP PIPE
  warn "$1: undoing the cutover"
  drop_run_dir
  if undo_cutover "$P"; then
    record "cutover version=$P previous=$OLD result=undone reason=$(printf '%s' "$1" | tr ' ' '_' | cut -c1-80)"
    die "The cutover to CITAR $P failed ($1), and was undone: CITAR $OLD runs again from $OPT."
  fi
  die "The cutover to CITAR $P failed ($1), and undoing it failed too (above). Look at $CUTOVER/CUTOVER."
}

# ----------------------------------------------------------------------------------------------------- uncutover

# Puts 0.1.5 back as the cutover's record says, the new layout and state moved aside. $1 is the release that ran (for
# the folders' names). Returns 1 when something could not be put back.
undo_cutover() {
  local info old old_tree archive lab stamp saved absent path failed=0 rel="$1"
  info="$(cat "$ROOT$CUTOVER/CUTOVER" 2>/dev/null)" || { warn "no $CUTOVER/CUTOVER"; return 1; }
  old="$(printf '%s\n' "$info" | sed -n 's/^old_version=//p')"
  old_tree="$(printf '%s\n' "$info" | sed -n 's/^old_tree=//p')"
  archive="$(printf '%s\n' "$info" | sed -n 's/^state_archive=//p')"
  lab="$(printf '%s\n' "$info" | sed -n 's/^lab=//p')"
  saved="$(printf '%s\n' "$info" | sed -n 's/^saved=//p')"
  absent="$(printf '%s\n' "$info" | sed -n 's/^absent=//p')"
  stamp="$(stamp)"
  run systemctl stop citar || true
  # Only what the cutover moved is moved back: with the old tree archived, what is at /opt/citar is the new layout;
  # with the state archived, what is at /var/lib/citar is the new state. Each is moved aside and kept.
  if [ -e "$ROOT$old_tree" ]; then
    if [ -e "$ROOT$OPT" ]; then run mv -T "$ROOT$OPT" "$ROOT$OPT-$rel-undone-$stamp" || failed=1; fi
    [ "$failed" -eq 1 ] || run mv -T "$ROOT$old_tree" "$ROOT$OPT" || failed=1
  fi
  if [ -e "$ROOT$archive" ]; then
    if [ -e "$ROOT$STATE_DIR" ]; then
      run mv -T "$ROOT$STATE_DIR" "$ROOT$STATE_DIR-$rel-undone-$stamp" || failed=1
    fi
    [ "$failed" -eq 1 ] || run mv -T "$ROOT$archive" "$ROOT$STATE_DIR" || failed=1
  fi
  [ "$failed" -eq 0 ] || { warn "CITAR $old's tree and state could not be put back"; return 1; }
  for path in $saved; do
    if act "$path <- its copy saved at the cutover"; then
      { mkdir -p "$(dirname "$ROOT$path")" && cp -p "$ROOT$CUTOVER/files$path" "$ROOT$path.citar-deploy-new" \
        && mv -f "$ROOT$path.citar-deploy-new" "$ROOT$path"; } || { warn "could not put $path back"; failed=1; }
    fi
  done
  for path in $absent; do
    if [ "$PLAN" -eq 1 ]; then
      say "   rm -f $path (not here before the cutover)"
    elif [ -e "$ROOT$path" ]; then
      run rm -f "$ROOT$path" || failed=1
    fi
  done
  run systemctl daemon-reload || failed=1
  if [ "$lab" = 1 ]; then run systemctl enable citar-lab || failed=1; fi
  run systemctl start citar || failed=1
  if [ "$lab" = 1 ]; then run systemctl start citar-lab || failed=1; fi
  health_plan -
  if [ "$PLAN" -eq 0 ]; then
    if healthy -; then ok "CITAR $old answers: $(show "$SAID")"; else warn "CITAR $old does not answer: $SAID"; failed=1; fi
  fi
  if act "$CUTOVER -> $CUTOVER-undone-$stamp (the record kept)"; then
    mv -T "$ROOT$CUTOVER" "$ROOT$CUTOVER-undone-$stamp" || failed=1
  fi
  [ "$failed" -eq 0 ]
}

cmd_uncutover() {
  local info old rel
  header
  pf_root
  pf_tools
  pf_env
  [ -f "$ROOT$CUTOVER/CUTOVER" ] || refuse "No cutover to undo here ($CUTOVER/CUTOVER)."
  info="$(cat "$ROOT$CUTOVER/CUTOVER")"
  old="$(printf '%s\n' "$info" | sed -n 's/^old_version=//p')"
  OLD="$old"
  [ -d "$ROOT$(printf '%s\n' "$info" | sed -n 's/^old_tree=//p')" ] \
    || refuse "CITAR $old's tree ($(printf '%s\n' "$info" | sed -n 's/^old_tree=//p')) is not here."
  [ -d "$ROOT$(printf '%s\n' "$info" | sed -n 's/^state_archive=//p')" ] \
    || refuse "CITAR $old's state ($(printf '%s\n' "$info" | sed -n 's/^state_archive=//p')) is not here."
  rel="$(running_release)"
  ok "cutover: from CITAR $old, made $(printf '%s\n' "$info" | sed -n 's/^made=//p'); CITAR ${rel:-none} runs now"
  lock
  # Once begun, the uncutover runs to its end (it is itself the way back).
  guard ""
  step "Put CITAR $old back (the new layout and state moved aside and kept):"
  undo_cutover "${rel:-none}" || die "Undoing the cutover did not complete (above)."
  SITES_FAILED=0
  sites_after "$old" "look at the other sites first" || true
  step "Record:"
  record "uncutover version=$old previous=${rel:-none} result=ok"
  if [ "$PLAN" -eq 1 ]; then
    say ""
    say "End of the plan: nothing was changed."
    exit 0
  fi
  [ "$SITES_FAILED" -eq 0 ] || exit 3
  say "CITAR $old runs again from $OPT."
}

# ----------------------------------------------------------------------------------------------------- status

cmd_status() {
  local p0 version state said
  [ "$PLAN" -eq 0 ] || say "status changes nothing: its plan is what it reads."
  p0="$(running_release)"
  say "current     ${p0:+$CURRENT -> releases/$p0}${p0:-none ($CURRENT)}"
  while IFS= read -r version; do
    state="no .complete"
    [ ! -f "$ROOT$RELEASES/$version/.complete" ] || state="complete"
    [ "$version" != "$p0" ] || state="$state, runs"
    say "release     $version ($state)"
  done < <(release_versions)
  say "service     citar: $(systemctl is-active citar 2>/dev/null || true)"
  if [ -f "$ROOT$ENV_FILE" ]; then
    DB="$(live_db)"
    say "database    ${DB:+$(show "$DB"), at revision $(revision "$DB")}${DB:-not SQLite}"
    said="$(probe health "$(direct_url)" "$(direct_url)" "${p0:--}" 0 2>&1)" || true
    say "answers     $(show "$said")"
  fi
  if [ -f "$ROOT$CUTOVER/CUTOVER" ]; then
    say "cutover     $(tr '\n' ' ' < "$ROOT$CUTOVER/CUTOVER")"
  fi
  if [ -f "$ROOT$LOG_FILE" ]; then
    say "deploys     the last five, from $LOG_FILE:"
    tail -n 5 "$ROOT$LOG_FILE" | sed 's/^/  /'
  fi
}

case "$COMMAND" in
  install) cmd_install ;;
  cutover) cmd_cutover ;;
  rollback) cmd_rollback ;;
  uncutover) cmd_uncutover ;;
  status) cmd_status ;;
  prune) cmd_prune ;;
esac
