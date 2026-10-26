#!/usr/bin/env bash
# staging.sh: the staging rehearsal of a CITAR deploy, inside WSL, held to the VPS's limits (design/phase5-release.md
# P5.14). Run as root inside WSL only, from a copy on WSL's own disk (never under /mnt/c), never on a real server:
#
#     staging.sh setup --ballast-mb N --repo CLONE [--cpu-quota 60%] [--cpu-period 10] [--cpu N] [--work-ms 25]
#     staging.sh probes [--without-cpu-quota]
#     staging.sh run --bundle DIR --rc1 DIR --rc2 DIR --rc3 DIR [--weights "50 35 20"] [--load-games N] [--lab-turns N]
#                    [--host-mb N]
#     staging.sh cleanup [--keep-logs DIR]
#     staging.sh status
#
# setup: the apt packages a fresh VPS lacks here (python3-venv, sqlite3, nginx); vm.swappiness 10 (the VPS's; the
#   value it had is recorded and cleanup puts it back); citar-staging.slice with the VPS's memory (MemoryMax=961M,
#   MemorySwapMax=2G) and its one vCPU: one CPU of this machine (AllowedCPUs=, --cpu, default the last), so the
#   slice's units share one run queue as the box's do and CPUWeight= decides between them (with several CPUs they
#   would run side by side and the weights would decide nothing), scaled by CPUQuota= (--cpu-quota, default 60%: a
#   laptop core is taken to be about 1.6 times the VPS's) over a short period (--cpu-period, default 10 ms: the share
#   of a slower core in slices too short to see, rather than the whole core taken away for 40 ms in every 100, which
#   no box does); the probes (below), which must pass; the other sites' load in the slice: a ballast
#   holding --ballast-mb of their resident memory (the private log's figure, kept in ops/p5-notes.md and never in a
#   tracked file) and two stand-in sites, an nginx server block and a Python unit doing --work-ms of CPU a request
#   (php-fpm's stand-in), listed in /etc/citar/deploy-checks.conf; and CITAR 0.1.5 as the VPS has it: `git archive
#   v0.1.5` of CLONE in /opt/citar with its .venv from its requirements.txt, its citar, citar-lab and backup units,
#   its env file, a hand-made citar-admin, and an nginx site on a loopback port with the connection_upgrade map,
#   started once to make its state. Every staged unit joins the slice through a drop-in (Slice=).
# probes: the slice's memory.max, memory.swap.max, cpu.max and cpuset.cpus.effective read back as asked, and a
#   process started in the slice may run on that one CPU alone; a unit in the slice with a small
#   MemoryMax and no swap allocates past it and is OOM-killed, the slice's oom_kill count rising; a busy loop in the
#   slice for 5 s uses the quota's share of a CPU (cpu.stat's usage_usec within 10% of quota times wall time).
#   --without-cpu-quota is the gate that can fail: it takes the quota away, runs the CPU probe alone (which must then
#   use more than the quota's share), puts the quota back and reads the slice's files again. Like every command it
#   exits 0 only when all of that held: the probe failed without its quota, and the quota is back.
# run: first reads the slice again as setup left it (its files, the ballast, the loaded stand-in, vm.swappiness), and
#   runs nothing on a slice that differs (a quota left off would put every figure on an unthrottled core); then,
#   with CITAR_DEPLOY_SLICE=citar-staging.slice exported, so citar-deploy's pip scope and every systemd-run of
#   citar-deploy and citar-admin land in the slice: citar-deploy cutover with --bundle; the first administrator, the
#   host machine's settings for one CPU and 961 MB, idle, a 4-bot game of 40 rounds, a lab of three duel games, and
#   at each --weights CPUWeight= a longer lab (--load-games duels, each to --lab-turns rounds, 0 for the speed's last)
#   while the stand-in's response times are taken (staging_check.py measure, the stand-in loaded throughout; a duel
#   plays in under a second, so three of them alone are too short to time anything against), the bound held at the
#   installed unit's own CPUWeight= and the other weights measured beside it; with --host-mb, three duels again at
#   that host memory figure (the one recommended for the box); install of --rc1 (it
#   migrates); rollback refused without --restore-backup, then with the backup it names; install --reinstall of
#   --rc1; install of --rc2 (planted: its server exits after migrating), undone with the database restored; install
#   of --rc3 (planted: its server exits before migrating), undone without a restore; --rc2 again, interrupted by a
#   TERM during its health wait; citar-backup and the restore drill (a second CITAR on a copy, its accounts read);
#   uncutover, 0.1.5 answering again. The stand-in sites answer 200 before and after every step. A timeline
#   (staging_check.py watch) records the memory, the OOM kills, every transient unit and CITAR's answers.
# cleanup: the units, drop-ins, the slice, the ballast, the stand-ins, the citar account, /opt/citar*,
#   /var/lib/citar*, /etc/citar, the backups, the deploy log, the nginx sites and the CITAR site's nginx logs
#   (/var/log/nginx/citar.*) removed; vm.swappiness restored; nginx stopped and disabled (the packages stay); then
#   each of these checked gone.
#
# The logs are in /var/lib/citar-staging/logs (cleanup --keep-logs copies them out first). Exit status: 0 when every
# check passed, 1 when one failed or something was refused, 2 for a usage error.

set -uo pipefail
umask 022
cd / || exit 1

SLICE="citar-staging.slice"
STAGE="/var/lib/citar-staging"
RECORD="$STAGE/setup.env"
TOOLS="/opt/citar-staging"
UNITS="/etc/systemd/system"
CHECKS="/etc/citar/deploy-checks.conf"
SITE_NAME="citar-staging.test"
SITE_PORT=8088
SITE_URL="http://$SITE_NAME:$SITE_PORT"
DIRECT_URL="http://127.0.0.1:8765"
STATIC_PORT=8081
STANDIN_PORT=8082
STANDIN_URL="http://127.0.0.1:$STANDIN_PORT/"
DRILL_PORT=8766
PY="/usr/bin/python3"
APT_NEEDS=(python3-venv python3.12-venv sqlite3 nginx)
STAGED_UNITS=(citar.service citar-lab.service citar-backup.service)
OWN_UNITS=(citar-staging-ballast.service citar-staging-standin.service)
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"

FAILED=0
say()  { printf '%s\n' "$*"; }
ok()   { printf '  ok  %s\n' "$*"; }
fail() { printf 'FAILED: %s\n' "$*"; FAILED=$((FAILED + 1)); }
die()  { printf 'staging: %s\n' "$*" >&2; exit 1; }
usage_die() { printf 'staging: %s (staging.sh --help)\n' "$*" >&2; exit 2; }
usage() { sed -n '2,/^$/s/^# \{0,1\}//p' "$0"; }
check() {   # $1 a command's status or a test's (0 passes), $2 what it shows
  if [ "$1" -eq 0 ]; then ok "$2"; else fail "$2"; fi
}
step() { say ""; say "== $*"; }

checker() { "$PY" -I "$TOOLS/staging_check.py" "$@"; }

# ----------------------------------------------------------------------------------------------------- arguments

COMMAND=""
BALLAST_MB=""
CPU_QUOTA="60%"
CPU_PERIOD=10
CPU_ONE=""
WORK_MS=25
REPO=""
WITHOUT_QUOTA=0
BUNDLE=""
RC1=""
RC2=""
RC3=""
WEIGHTS="50 35 20"
HOST_MB=0
LAB_TURNS=0
LOAD_GAMES=40
KEEP_LOGS=""
while [ $# -gt 0 ]; do
  case "$1" in
    setup|probes|run|cleanup|status)
      [ -z "$COMMAND" ] || usage_die "one command at a time ($COMMAND and $1)"
      COMMAND="$1" ;;
    --without-cpu-quota) WITHOUT_QUOTA=1 ;;
    --ballast-mb|--cpu-quota|--cpu-period|--cpu|--work-ms|--repo|--bundle|--rc1|--rc2|--rc3|--weights|--load-games|--lab-turns|--host-mb|--keep-logs)
      [ $# -ge 2 ] || usage_die "$1 needs a value"
      case "$1" in
        --ballast-mb) BALLAST_MB="$2" ;;
        --cpu-quota) CPU_QUOTA="$2" ;;
        --cpu-period) CPU_PERIOD="$2" ;;
        --cpu) CPU_ONE="$2" ;;
        --work-ms) WORK_MS="$2" ;;
        --repo) REPO="$2" ;;
        --bundle) BUNDLE="$2" ;;
        --rc1) RC1="$2" ;;
        --rc2) RC2="$2" ;;
        --rc3) RC3="$2" ;;
        --weights) WEIGHTS="$2" ;;
        --load-games) LOAD_GAMES="$2" ;;
        --host-mb) HOST_MB="$2" ;;
        --lab-turns) LAB_TURNS="$2" ;;
        --keep-logs) KEEP_LOGS="$2" ;;
      esac
      shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage_die "unknown argument: $1" ;;
  esac
  shift
done
[ -n "$COMMAND" ] || usage_die "which command: setup, probes, run, cleanup or status?"
[ "$WITHOUT_QUOTA" -eq 0 ] || [ "$COMMAND" = probes ] || usage_die "--without-cpu-quota is probes' alone"
[[ "$CPU_QUOTA" =~ ^([1-9][0-9]?)%$ ]] || usage_die "--cpu-quota: a percentage of one CPU from 1% to 99% ($CPU_QUOTA)"
{ [[ "$CPU_PERIOD" =~ ^[1-9][0-9]{0,2}$ ]] && [ "$CPU_PERIOD" -le 100 ]; } \
  || usage_die "--cpu-period: milliseconds, from 1 to 100 ($CPU_PERIOD)"
if [ -n "$CPU_ONE" ]; then
  { [[ "$CPU_ONE" =~ ^[0-9]{1,3}$ ]] && [ "$CPU_ONE" -lt "$(nproc --all)" ]; } \
    || usage_die "--cpu: one of this machine's CPUs, 0 to $(( $(nproc --all) - 1 )) ($CPU_ONE)"
fi
[[ "$WORK_MS" =~ ^[1-9][0-9]{0,3}$ ]] || usage_die "--work-ms: whole milliseconds ($WORK_MS)"
[[ "$LAB_TURNS" =~ ^[0-9]{1,4}$ ]] || usage_die "--lab-turns: a number of rounds, 0 for the speed's last ($LAB_TURNS)"
[[ "$LOAD_GAMES" =~ ^[1-9][0-9]{0,2}$ ]] || usage_die "--load-games: a number of duels ($LOAD_GAMES)"
[[ "$HOST_MB" =~ ^[0-9]{1,4}$ ]] || usage_die "--host-mb: MB, 0 for none ($HOST_MB)"
[[ "$WEIGHTS" =~ ^[0-9]+([ ,][0-9]+)*$ ]] || usage_die "--weights: CPUWeight= values, such as \"50 20\""
for w in ${WEIGHTS//,/ }; do
  { [ "$w" -ge 20 ] && [ "$w" -le 50 ]; } || usage_die "--weights: each from 20 to 50, the unit's range ($w)"
done
if [ -n "$BALLAST_MB" ]; then
  { [[ "$BALLAST_MB" =~ ^[1-9][0-9]*$ ]] && [ "$BALLAST_MB" -lt 961 ]; } \
    || usage_die "--ballast-mb: the other sites' resident memory in MB, below the slice's 961 ($BALLAST_MB)"
fi

# ----------------------------------------------------------------------------------------------------- where it runs

# Root, inside WSL, systemd running, and never from a folder under /mnt (Windows' disks).
where_ok() {
  [ "$(id -u)" = 0 ] || die "run it as root inside WSL (wsl -d Ubuntu-24.04 -u root -- bash /root/.../staging.sh ...)"
  grep -qi microsoft /proc/sys/kernel/osrelease 2>/dev/null \
    || die "this is not WSL: staging changes the machine it runs on (a slice, swappiness, nginx, a citar account),
and must never run on a real server"
  [ -d /run/systemd/system ] || die "systemd is not running here"
  case "$HERE" in /mnt/*) die "run it from a copy on WSL's own disk, not $HERE" ;; esac
}

# The record of what setup changed, read by run and cleanup.
recorded() { [ -f "$RECORD" ] && sed -n "s/^$1=//p" "$RECORD" | tail -n 1; }
remember() { mkdir -p "$STAGE" && printf '%s=%s\n' "$1" "$2" >> "$RECORD"; }

# The slice's cgroup. A dash in a slice's name nests it (systemd.slice(5)): citar-staging.slice is
# /citar.slice/citar-staging.slice, inside an implicit citar.slice that sets no limit.
# The quota setup used (its record; else the arguments): QUOTA_PCT, PERIOD_US and CPU_MAX, cpu.max as it must read.
quota_figures() {
  local q p
  local c
  q="$(recorded CPU_QUOTA)"
  p="$(recorded CPU_PERIOD)"
  c="$(recorded CPU_ONE)"
  CPU_QUOTA="${q:-$CPU_QUOTA}"
  CPU_PERIOD="${p:-$CPU_PERIOD}"
  CPU_ONE="${c:-${CPU_ONE:-$(( $(nproc --all) - 1 ))}}"
  QUOTA_PCT="${CPU_QUOTA%\%}"
  PERIOD_US=$((CPU_PERIOD * 1000))
  CPU_MAX="$((QUOTA_PCT * PERIOD_US / 100)) $PERIOD_US"
}

slice_dir() {
  local cg
  cg="$(systemctl show -p ControlGroup --value "$SLICE" 2>/dev/null)"
  printf '/sys/fs/cgroup%s' "${cg:-/citar.slice/$SLICE}"
}
oom_kills() { awk '$1 == "oom_kill" { print $2 }' "$(slice_dir)/memory.events" 2>/dev/null || echo 0; }

# ----------------------------------------------------------------------------------------------------- setup

apt_step() {
  local pkg missing=()
  for pkg in "${APT_NEEDS[@]}"; do
    dpkg-query -W -f='${Status}' "$pkg" 2>/dev/null | grep -q "install ok installed" || missing+=("$pkg")
  done
  if [ "${#missing[@]}" -eq 0 ]; then
    ok "apt: ${APT_NEEDS[*]} are installed already"
    remember APT_INSTALLED ""
    return 0
  fi
  say "apt-get install -y ${missing[*]}"
  DEBIAN_FRONTEND=noninteractive apt-get install -y "${missing[@]}" || die "apt-get install ${missing[*]} failed"
  remember APT_INSTALLED "${missing[*]}"
  ok "apt: installed ${missing[*]} (recorded; the packages stay after cleanup)"
}

write_slice() {
  cat > "$UNITS/$SLICE" <<EOF
# The staging run's stand-in for the VPS (scripts/deploy/staging.sh; design/phase5-release.md P5.14): its memory and
# swap, and its one vCPU scaled to this machine's faster cores. Every staged unit joins it through a drop-in.
[Unit]
Description=CITAR staging: the VPS's limits

[Slice]
AllowedCPUs=$CPU_ONE
CPUQuota=$CPU_QUOTA
CPUQuotaPeriodSec=${CPU_PERIOD}ms
MemoryMax=961M
MemorySwapMax=2G
EOF
  { systemctl daemon-reload && systemctl start "$SLICE"; } || die "the slice could not be started"
}

probe_readback() {
  local dir mem swap cpu
  dir="$(slice_dir)"
  mem="$(cat "$dir/memory.max")"
  swap="$(cat "$dir/memory.swap.max")"
  cpu="$(cat "$dir/cpu.max")"
  [ "$mem" = $((961 * 1048576)) ]; check $? "memory.max reads $mem (961M)"
  [ "$swap" = $((2048 * 1048576)) ]; check $? "memory.swap.max reads $swap (2G)"
  [ "$cpu" = "$CPU_MAX" ]; check $? "cpu.max reads '$cpu' ($CPU_QUOTA of one CPU, in periods of $CPU_PERIOD ms)"
  [ "$(cat "$dir/cpuset.cpus.effective" 2>/dev/null)" = "$CPU_ONE" ]
  check $? "cpuset.cpus.effective reads '$(cat "$dir/cpuset.cpus.effective" 2>/dev/null)' (AllowedCPUs=$CPU_ONE: one CPU)"
}

probe_oom() {
  local before after out status
  before="$(oom_kills)"
  out="$(systemd-run --unit=citar-staging-probe-oom --slice="$SLICE" --wait --collect -p Type=exec \
    -p MemoryMax=64M -p MemorySwapMax=0 -- "$PY" -I "$TOOLS/staging_check.py" hog --mb 256 2>&1)"
  status=$?
  after="$(oom_kills)"
  printf '%s\n' "$out" | sed 's/^/      /'
  { [ "$status" -ne 0 ] && printf '%s' "$out" | grep -q 'oom-kill' && [ "$after" -gt "$before" ]; }
  check $? "the OOM probe (256 MiB under MemoryMax=64M, no swap) is killed by the kernel, and the slice's oom_kill rises ($before -> $after)"
}

# The CPU probe: a 5 s busy loop in the slice, and the CPUs it may run on (the slice's one alone, a check). Returns 0
# when its CPU use is the quota's share (within 10%), 1 when it is not, 2 when the probe printed no figures (counted
# as a failed check). With "held" (the default) the share is a check; with "unheld" (the quota taken away) the use is
# printed as a figure, and the caller holds the status to 1.
probe_cpu() {   # $1 the share expected, in percent of one CPU; $2 held or unheld
  local out usage wall allowed line status
  out="$(systemd-run --unit=citar-staging-probe-cpu --slice="$SLICE" --quiet --wait --collect --pipe -p Type=exec \
    -- "$PY" -I "$TOOLS/staging_check.py" burn --seconds 5 --cgroup "$(slice_dir)" 2>&1 < /dev/null | tail -n 1)"
  usage="$(printf '%s' "$out" | sed -n 's/.*"usage_usec": \([0-9]*\).*/\1/p')"
  allowed="$(printf '%s' "$out" | sed -n 's/.*"cpus_allowed": "\([^"]*\)".*/\1/p')"
  wall="$(printf '%s' "$out" | sed -n 's/.*"wall_usec": \([0-9]*\).*/\1/p')"
  if [ -z "$usage" ] || [ -z "$wall" ]; then
    fail "the CPU probe printed no figures: $out"
    return 2
  fi
  line="$(awk -v u="$usage" -v w="$wall" -v q="$1" 'BEGIN {
    used = 100 * u / w; lo = q * 0.9; hi = q * 1.1
    printf "the CPU probe used %.1f%% of a CPU over %.2f s (the quota %s%%: %.1f%% to %.1f%% is its share)\n", used, w / 1e6, q, lo, hi
    exit !(used >= lo && used <= hi) }')"
  status=$?
  if [ "${2:-held}" = held ]; then check "$status" "$line"; else say "  --  $line"; fi
  [ "$allowed" = "$CPU_ONE" ]; check $? "the probe may run on CPU $CPU_ONE alone (Cpus_allowed_list: ${allowed:-none})"
  return "$status"
}

cmd_probes() {
  local status now
  where_ok
  quota_figures
  [ -f "$UNITS/$SLICE" ] || die "no $SLICE: run setup first"
  if [ "$WITHOUT_QUOTA" -eq 1 ]; then
    # The gate that can fail. Its verdict is this command's own exit status, 0 only when the probe failed without its
    # quota and the quota is back: a refusal, a probe within its share, or a quota left off all exit 1.
    step "The CPU probe with the slice's CPUQuota taken away (a gate that can fail: the probe must fail)"
    systemctl set-property --runtime "$SLICE" CPUQuota= || die "the quota could not be taken away"
    now="$(cat "$(slice_dir)/cpu.max")"
    [ "${now%% *}" = max ]; check $? "the quota is taken away: cpu.max reads '$now'"
    probe_cpu "$QUOTA_PCT" unheld
    status=$?
    rm -f "/run/systemd/system.control/$SLICE.d/50-CPUQuota.conf"
    systemctl daemon-reload
    [ "$status" -eq 1 ]
    check $? "without its quota the CPU probe fails, as it must: the quota is what holds the slice to its share"
    step "The quota put back: the slice's files read again"
    probe_readback
    say ""
    if [ "$FAILED" -eq 0 ]; then
      say "The gate that can fail did: the CPU probe failed without the quota, and the quota is back."
      return 0
    fi
    say "$FAILED check(s) FAILED (above)."
    return 1
  fi
  step "The probes: each limit must bind before anything is measured"
  probe_readback
  probe_oom
  probe_cpu "$QUOTA_PCT"
  [ "$FAILED" -eq 0 ]
}

own_unit() {   # $1 the unit's name, $2 its description, then its command
  local name="$1" what="$2"
  shift 2
  cat > "$UNITS/$name" <<EOF
# The staging run's stand-in (scripts/deploy/staging.sh; design/phase5-release.md P5.14).
[Unit]
Description=CITAR staging: $what

[Service]
Type=notify
NotifyAccess=main
Slice=$SLICE
ExecStart=$*
EOF
}

other_sites() {
  own_unit citar-staging-ballast.service "the other sites' resident memory ($BALLAST_MB MB)" \
    "$PY -I $TOOLS/staging_check.py ballast --mb $BALLAST_MB"
  own_unit citar-staging-standin.service "a loaded stand-in site ($WORK_MS ms of CPU a request)" \
    "$PY -I $TOOLS/staging_check.py standin --port $STANDIN_PORT --work-ms $WORK_MS"
  cat > /etc/nginx/sites-available/citar-staging-static <<EOF
# The staging run's static stand-in site (scripts/deploy/staging.sh).
server {
    listen 127.0.0.1:$STATIC_PORT;
    server_name _;
    access_log off;
    default_type text/plain;
    location / { return 200 "stand-in site\n"; }
}
EOF
  ln -sfn ../sites-available/citar-staging-static /etc/nginx/sites-enabled/citar-staging-static
  systemctl daemon-reload
  systemctl start citar-staging-ballast.service citar-staging-standin.service || die "the stand-ins did not start"
  local current
  current="$(cat "$(slice_dir)/citar-staging-ballast.service/memory.current")"
  [ "$current" -ge $((BALLAST_MB * 1048576 * 95 / 100)) ]
  check $? "the ballast holds $((current / 1048576)) MB in the slice (--ballast-mb $BALLAST_MB)"
}

# 0.1.5's hand-made admin wrapper, as the VPS had one (/usr/local/bin/citar-admin): cutover keeps it with the record.
write_old_admin() {
  cat > /usr/local/bin/citar-admin <<'EOF'
#!/bin/sh
# CITAR 0.1.5's administration commands as the service account (the staging run's stand-in for the VPS's hand-made
# wrapper; citar-deploy cutover replaces it with deploy/citar-admin.sh).
exec systemd-run --quiet --wait --collect --pipe -p User=citar -p Group=citar -p WorkingDirectory=/opt/citar \
  -p EnvironmentFile=/etc/citar/citar.env -- /opt/citar/.venv/bin/python -m citar.server.admin_cli "$@"
EOF
  chmod 0755 /usr/local/bin/citar-admin
}

write_env() {
  local key
  key="$("$PY" -c 'import secrets; print(secrets.token_urlsafe(48))')"
  install -d -m 0755 /etc/citar
  {
    printf '# CITAR 0.1.5 on the staging box (scripts/deploy/staging.sh): the VPS'"'"'s settings, with a staging origin.\n'
    printf 'CITAR_MODE=server\nCITAR_PUBLIC_ORIGIN=https://%s\nCITAR_SECRET_KEY=%s\n' "$SITE_NAME" "$key"
    printf 'CITAR_HOST=127.0.0.1\nCITAR_PORT=8765\nCITAR_BEHIND_PROXY=1\nCITAR_TRUSTED_PROXY_HOPS=1\n'
    printf 'CITAR_REGISTRATION=invite\nCITAR_SESSION_DAYS=30\nCITAR_SESSION_IDLE_HOURS=336\nCITAR_WORKER_PING_SECONDS=20\n'
    printf 'CITAR_DATA_DIR=/var/lib/citar\nCITAR_SAVE_DIR=/var/lib/citar/saves\nCITAR_CONFIG_DIR=/var/lib/citar/config\n'
    printf 'CITAR_BENCH_DIR=/var/lib/citar/benchmarks\n'
  } > /etc/citar/citar.env.new
  install -m 0640 -o root -g citar /etc/citar/citar.env.new /etc/citar/citar.env && rm -f /etc/citar/citar.env.new
  printf '# The other sites staging stands in for (scripts/deploy/staging.sh): each must answer 200.\n%s\n%s\n' \
    "http://127.0.0.1:$STATIC_PORT/" "$STANDIN_URL" > "$CHECKS"
}

# The CITAR site, on a loopback port, with the connection_upgrade map (deploy/nginx-citar.conf's locations).
write_citar_site() {
  cat > /etc/nginx/sites-available/citar <<EOF
# CITAR's nginx site on the staging box (scripts/deploy/staging.sh): a loopback port, plain HTTP.
map \$http_upgrade \$connection_upgrade {
    default upgrade;
    ''      close;
}

upstream citar_app {
    server 127.0.0.1:8765 fail_timeout=10s;
    keepalive 8;
}

server {
    listen 127.0.0.1:$SITE_PORT;
    server_name $SITE_NAME;
    client_max_body_size 32m;
    access_log /var/log/nginx/citar.access.log;
    error_log  /var/log/nginx/citar.error.log;

    location /ws/ {
        proxy_pass http://citar_app;
        proxy_http_version 1.1;
        proxy_set_header Upgrade \$http_upgrade;
        proxy_set_header Connection \$connection_upgrade;
        proxy_set_header Host \$host;
        proxy_set_header X-Real-IP \$remote_addr;
        proxy_set_header X-Forwarded-For \$proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto \$scheme;
        proxy_read_timeout 3600s;
        proxy_send_timeout 3600s;
        proxy_buffering off;
    }

    location / {
        proxy_pass http://citar_app;
        proxy_http_version 1.1;
        proxy_set_header Host \$host;
        proxy_set_header X-Real-IP \$remote_addr;
        proxy_set_header X-Forwarded-For \$proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto \$scheme;
        proxy_set_header Connection "";
        proxy_read_timeout 600s;
        proxy_send_timeout 600s;
        proxy_connect_timeout 10s;
        proxy_buffering off;
    }
}
EOF
  ln -sfn ../sites-available/citar /etc/nginx/sites-enabled/citar
}

deploy_old() {
  local version unit
  { [ -n "$REPO" ] && [ -d "$REPO/.git" ]; } || die "--repo: no git clone at '$REPO'"
  useradd --system --user-group --home-dir /var/lib/citar --no-create-home --shell /usr/sbin/nologin citar \
    || die "could not make the citar account"
  remember CITAR_USER 1
  install -d -o citar -g citar -m 0750 /var/lib/citar
  mkdir -p /opt/citar
  git -c safe.directory="$REPO" -C "$REPO" archive v0.1.5 | tar -x -C /opt/citar \
    || die "git archive v0.1.5 of $REPO failed (does the clone hold the tag?)"
  version="$(sed -n 's/^__version__ *= *"\(.*\)".*/\1/p' /opt/citar/citar/__init__.py)"
  [ "$version" = 0.1.5 ] || die "/opt/citar/citar/__init__.py says $version, not 0.1.5"
  ok "/opt/citar: git archive v0.1.5 ($(find /opt/citar -type f | wc -l) files)"
  python3.12 -m venv /opt/citar/.venv || die "the venv could not be made"
  /opt/citar/.venv/bin/pip install --disable-pip-version-check --no-cache-dir -q -r /opt/citar/requirements.txt \
    > "$STAGE/logs/pip-0.1.5.log" 2>&1 || die "pip install -r requirements.txt failed ($STAGE/logs/pip-0.1.5.log)"
  ok "/opt/citar/.venv: its requirements.txt ($(/opt/citar/.venv/bin/pip freeze 2>/dev/null | wc -l) distributions)"
  write_env
  for unit in citar.service citar-lab.service citar-backup.service citar-backup.timer; do
    install -m 0644 "/opt/citar/deploy/$unit" "$UNITS/$unit"
  done
  install -m 0755 /opt/citar/deploy/citar-backup.sh /usr/local/bin/citar-backup
  write_old_admin
  for unit in "${STAGED_UNITS[@]}"; do
    mkdir -p "$UNITS/$unit.d"
    printf '# The staging run (scripts/deploy/staging.sh): every staged unit runs in the slice.\n[Service]\nSlice=%s\n' \
      "$SLICE" > "$UNITS/$unit.d/staging-slice.conf"
  done
  write_citar_site
  nginx -t 2>&1 | sed 's/^/      /'
  systemctl reload nginx || systemctl restart nginx || die "nginx did not take the sites"
  systemctl daemon-reload
  systemctl enable --now citar.service citar-lab.service citar-backup.timer || die "0.1.5's services did not start"
  checker health "$DIRECT_URL" --site "$SITE_URL" --seconds 90
  check $? "CITAR 0.1.5 answers on 127.0.0.1:8765 and through its nginx site"
  systemctl is-active --quiet citar-lab.service
  check $? "citar-lab.service runs"
}

cmd_setup() {
  local before
  where_ok
  [ -n "$BALLAST_MB" ] || usage_die "setup needs --ballast-mb N: the other sites' resident memory, from the private
log's measurement (ops/p5-notes.md keeps the figure)"
  [ -n "$REPO" ] || usage_die "setup needs --repo CLONE: a clone holding the tag v0.1.5"
  [ ! -e "$RECORD" ] || die "staging is set up already ($RECORD): run cleanup first"
  for path in /opt/citar /var/lib/citar /etc/citar "$UNITS/citar.service" "$UNITS/$SLICE"; do
    [ ! -e "$path" ] || die "$path is here already: staging needs a WSL without CITAR (run cleanup)"
  done
  ! id citar >/dev/null 2>&1 || die "a citar account exists already: staging needs a WSL without CITAR (run cleanup)"
  mkdir -p "$STAGE/logs"
  remember CPU_QUOTA "$CPU_QUOTA"
  remember CPU_PERIOD "$CPU_PERIOD"
  remember CPU_ONE "${CPU_ONE:-$(( $(nproc --all) - 1 ))}"
  quota_figures
  remember BALLAST_MB "$BALLAST_MB"
  remember WORK_MS "$WORK_MS"
  install -d -m 0755 "$TOOLS"
  install -m 0644 "$HERE/staging_check.py" "$TOOLS/staging_check.py"

  step "Packages"
  apt_step
  if [ -e /etc/nginx/sites-enabled/default ]; then
    rm -f /etc/nginx/sites-enabled/default
    remember NGINX_DEFAULT 1
    ok "nginx's default site (port 80 on every address) disabled for the run; cleanup enables it again"
  fi

  step "vm.swappiness"
  before="$(cat /proc/sys/vm/swappiness)"
  remember SWAPPINESS "$before"
  sysctl -q -w vm.swappiness=10 || die "vm.swappiness could not be set"
  [ "$(cat /proc/sys/vm/swappiness)" = 10 ]; check $? "vm.swappiness 10, the VPS's (it was $before; cleanup restores it)"

  step "The slice: $SLICE (AllowedCPUs=$CPU_ONE, CPUQuota=$CPU_QUOTA in periods of $CPU_PERIOD ms, MemoryMax=961M, MemorySwapMax=2G)"
  write_slice
  remember SLICE 1
  cmd_probes || die "a probe failed: a limit that does not bind fails setup (cleanup undoes it)"

  step "The other sites: the ballast and the two stand-ins"
  other_sites

  step "CITAR 0.1.5 as the VPS has it"
  deploy_old
  checker sites "$CHECKS" --when "after setup"
  check $? "the stand-in sites answer"
  [ "$FAILED" -eq 0 ] || die "setup failed (above); cleanup undoes it"
  remember SETUP_DONE 1
  say ""
  say "Set up: CITAR 0.1.5 runs in $SLICE beside the other sites' stand-ins. Next: staging.sh run."
}

# ----------------------------------------------------------------------------------------------------- run

LOGS=""
phase() { printf '{"t": %s, "phase": "%s"}\n' "$(date +%s.%N)" "$1" >> "$LOGS/phases.jsonl"; }

bundle_version() { sed -n 's/^pep440=//p' "$1/BUNDLE" | head -n 1; }

meta_version() {
  local got
  got="$("$PY" -I -c 'import json, sys, urllib.request
print(json.load(urllib.request.urlopen(sys.argv[1] + "/api/meta", timeout=10)).get("version"))' "$DIRECT_URL" 2>/dev/null)"
  printf '%s' "$got" | sed -e 's/-alpha\./a/' -e 's/-beta\./b/' -e 's/-rc\./rc/'
}

live_revision() { sqlite3 -readonly /var/lib/citar/citar.db 'SELECT version_num FROM alembic_version;' 2>/dev/null; }

# A step: the stand-in sites before and after it, its output kept in $LOGS/<name>.log, and its exit status held to
# the one wanted. Sets OUT to its log.
OUT=""
expect() {   # $1 the exit status wanted, $2 the step's name, then its command
  local want="$1" name="$2" status
  shift 2
  step "$name"
  phase "$name"
  checker sites "$CHECKS" --when "before: $name" || FAILED=$((FAILED + 1))
  OUT="$LOGS/$name.log"
  "$@" > "$OUT" 2>&1
  status=$?
  sed 's/^/   | /' "$OUT"
  [ "$status" -eq "$want" ]; check $? "$name: exit $status (wanted $want)"
  checker sites "$CHECKS" --when "after: $name" || FAILED=$((FAILED + 1))
}

serving() {   # $1 the version wanted on /api/meta, $2 the revision wanted (or -)
  local got rev
  got="$(meta_version)"
  [ "$got" = "$1" ]; check $? "CITAR $got answers on /api/meta (wanted $1)"
  if [ "$2" != - ]; then
    rev="$(live_revision)"
    [ "$rev" = "$2" ]; check $? "the live database is at revision $rev (wanted $2)"
  fi
}

# The head a release's server migrates to: the one revision among its venv's migrations that none names as its
# down_revision.
head_revision() {   # $1 the release's version
  "$PY" -I - /opt/citar/releases/"$1"/venv/lib/python3*/site-packages/citar/migrations/versions <<'PY'
import re
import sys
from pathlib import Path

revs, downs = set(), set()
for f in Path(sys.argv[1]).glob("*.py"):
    text = f.read_text(encoding="utf-8")
    m = re.search(r'^revision(?:: *str)? *= *["\']([^"\']+)["\']', text, re.M)
    d = re.search(r'^down_revision(?:: *[^=]+)? *= *["\']([^"\']+)["\']', text, re.M)
    if m:
        revs.add(m[1])
    if d:
        downs.add(d[1])
heads = sorted(revs - downs)
print(heads[0] if len(heads) == 1 else "")
PY
}

interrupted_install() {   # --rc2 again, a TERM sent to citar-deploy during its health wait
  local pid deadline
  bash "$RC2/deploy/citar-deploy.sh" install --reinstall --bundle "$RC2" --site-url "$SITE_URL" &
  pid=$!
  deadline=$((SECONDS + 600))
  while kill -0 "$pid" 2>/dev/null && [ "$SECONDS" -lt "$deadline" ]; do
    if grep -q '^[0-9]*\. Health of CITAR' "$OUT" 2>/dev/null; then
      sleep 5
      printf 'staging: sending TERM to citar-deploy (pid %s) during its health wait\n' "$pid"
      kill -TERM "$pid"
      break
    fi
    sleep 0.5
  done
  wait "$pid"
}

drill() {
  local backup d status
  step "citar-backup and the restore drill"
  phase "drill"
  checker sites "$CHECKS" --when "before: the drill" || FAILED=$((FAILED + 1))
  systemctl start citar-backup.service
  status=$?
  { [ "$status" -eq 0 ] || [ "$(systemctl show -p ExecMainStatus --value citar-backup)" = 3 ]; }
  check $? "systemctl start citar-backup (exit $status)"
  backup="$(find /var/backups/citar -mindepth 1 -maxdepth 1 -type d -name '2*' | sort | tail -n 1)"
  { [ -f "$backup/RELEASE" ] && [ -f "$backup/citar.db" ] && [ -f "$backup/state.tar.gz" ] \
    && [ ! -e "$backup/UNTRUSTWORTHY" ]; }
  check $? "the backup $backup: RELEASE, citar.db, state.tar.gz, and no UNTRUSTWORTHY"
  sed 's/^/      /' "$backup/RELEASE"
  [ "$(sqlite3 -readonly "$backup/citar.db" 'PRAGMA integrity_check;')" = ok ]
  check $? "the snapshot opens and passes PRAGMA integrity_check"
  d="$STAGE/drill"
  rm -rf "$d"
  install -d -o citar -g citar -m 0700 "$d" "$d/state"
  tar -xzpf "$backup/state.tar.gz" -C "$d/state" && cp "$backup/citar.db" "$d/state/citar.db" \
    && chown -R citar:citar "$d/state"
  check $? "a copy of it: state.tar.gz and citar.db in $d/state"
  {
    grep -vE '^(CITAR_(STATE_DIR|DATA_DIR|DB_URL|SAVE_DIR|CONFIG_DIR|BENCH_DIR|KEYS_FILE|PORT|HOST)|HOME)=' \
      /etc/citar/citar.env
    printf 'CITAR_STATE_DIR=%s\nCITAR_DATA_DIR=%s\nCITAR_SAVE_DIR=%s\nCITAR_CONFIG_DIR=%s\nCITAR_BENCH_DIR=%s\n' \
      "$d/state" "$d/state" "$d/state/saves" "$d/state/config" "$d/state/benchmarks"
    printf 'CITAR_HOST=127.0.0.1\nCITAR_PORT=%s\nHOME=%s\n' "$DRILL_PORT" "$d/state"
  } > "$d/citar.env"
  chmod 0600 "$d/citar.env"
  systemd-run --unit=citar-staging-drill --slice="$SLICE" --collect --quiet -p Type=exec -p User=citar -p Group=citar \
    -p WorkingDirectory="$d/state" -p EnvironmentFile="$d/citar.env" -- /opt/citar/current/venv/bin/citar serve
  checker drill --url "http://127.0.0.1:$DRILL_PORT" --password-file "$STAGE/admin.pw" \
    | tee "$LOGS/drill.log" | sed 's/^/   | /'
  check "${PIPESTATUS[0]}" "the restore drill: a second CITAR on a copy of the backup reads its accounts"
  systemctl stop citar-staging-drill.service 2>/dev/null
  rm -rf "$d"
  checker sites "$CHECKS" --when "after: the drill" || FAILED=$((FAILED + 1))
}

# The slice read again as setup left it, before a run measures anything on it: its files (a quota left off by an
# interrupted `probes --without-cpu-quota`, say, would put every figure on an unthrottled core and fail nothing), the
# ballast still holding the other sites' memory (swapped or not), the loaded stand-in running, vm.swappiness 10.
slice_as_set() {
  local ballast held
  step "The slice as setup left it (read again before anything is measured)"
  quota_figures
  probe_readback
  ballast="$(recorded BALLAST_MB)"
  held=$(( $(cat "$(slice_dir)/citar-staging-ballast.service/memory.current" 2>/dev/null || echo 0) \
    + $(cat "$(slice_dir)/citar-staging-ballast.service/memory.swap.current" 2>/dev/null || echo 0) ))
  [ "$held" -ge $(( ${ballast:-0} * 1048576 * 95 / 100 )) ] && [ "${ballast:-0}" -gt 0 ]
  check $? "the ballast holds $((held / 1048576)) MB in the slice (--ballast-mb ${ballast:-none})"
  systemctl is-active --quiet citar-staging-standin.service
  check $? "the loaded stand-in runs (citar-staging-standin.service)"
  [ "$(cat /proc/sys/vm/swappiness)" = 10 ]; check $? "vm.swappiness reads $(cat /proc/sys/vm/swappiness) (10)"
  [ "$FAILED" -eq 0 ] || die "the slice is not as setup left it (above): nothing is run on it (cleanup, then setup)"
}

cmd_run() {
  local p0 p1 p2 p3 rev0 rev1 named stamp
  where_ok
  [ "$(recorded SETUP_DONE)" = 1 ] || die "staging is not set up (staging.sh setup)"
  for b in "$BUNDLE" "$RC1" "$RC2" "$RC3"; do
    { [ -n "$b" ] && [ -f "$b/BUNDLE" ]; } || usage_die "run needs --bundle, --rc1, --rc2 and --rc3: unpacked deploy bundles"
  done
  slice_as_set
  BUNDLE="$(cd "$BUNDLE" && pwd -P)"; RC1="$(cd "$RC1" && pwd -P)"; RC2="$(cd "$RC2" && pwd -P)"; RC3="$(cd "$RC3" && pwd -P)"
  p0="$(bundle_version "$BUNDLE")"; p1="$(bundle_version "$RC1")"; p2="$(bundle_version "$RC2")"; p3="$(bundle_version "$RC3")"
  stamp="$(date -u +%Y%m%d-%H%M%S)"
  LOGS="$STAGE/logs/run-$stamp"
  mkdir -p "$LOGS"
  export CITAR_DEPLOY_SLICE="$SLICE"
  say "staging run $stamp: cutover to $p0, then $p1, $p2 (planted: exits after migrating), $p3 (planted: exits before"
  say "migrating); CPU $(recorded CPU_ONE) alone at $(recorded CPU_QUOTA) (periods of $(recorded CPU_PERIOD) ms), ballast $(recorded BALLAST_MB) MB, stand-in $(recorded WORK_MS) ms a request;"
  say "logs in $LOGS"
  systemd-run --unit=citar-staging-watch --collect --quiet -p Type=exec -- "$PY" -I "$TOOLS/staging_check.py" watch \
    --out "$LOGS/watch.jsonl" --url "$DIRECT_URL" || die "the timeline could not be started"

  expect 0 "cutover" bash "$BUNDLE/deploy/citar-deploy.sh" cutover --bundle "$BUNDLE" --site-url "$SITE_URL"
  checker health "$DIRECT_URL" --site "$SITE_URL" --version "$p0" --handshake --seconds 30
  check $? "CITAR $p0 answers, and its WebSocket upgrade passes nginx"
  rev0="$(live_revision)"

  step "The measurements (staging_check.py measure)"
  phase "measure"
  checker measure --out "$LOGS/measure.json" --site "$SITE_URL" --standin "$STANDIN_URL" \
    --password-file "$STAGE/admin.pw" --weights "$WEIGHTS" --lab-turns "$LAB_TURNS" \
    --load-games "$LOAD_GAMES" --host-mb "$HOST_MB" 2>&1 | tee "$LOGS/measure.log" \
    | sed 's/^/   | /'
  check "${PIPESTATUS[0]}" "the measurements' checks"
  checker sites "$CHECKS" --when "after: the measurements" || FAILED=$((FAILED + 1))

  expect 0 "install-$p1" bash "$RC1/deploy/citar-deploy.sh" install --bundle "$RC1" --site-url "$SITE_URL"
  rev1="$(head_revision "$p1")"
  serving "$p1" "$rev1"
  [ "$rev1" != "$rev0" ]; check $? "$p1 migrated the database ($rev0 -> $rev1)"

  expect 1 "rollback-refused" /usr/local/sbin/citar-deploy rollback --to "$p0" --site-url "$SITE_URL"
  named="$(sed -n 's/.*--restore-backup \(\/var\/backups\/citar\/[^ ]*\).*/\1/p' "$OUT" | head -n 1)"
  [ -n "$named" ] && [ -d "$named" ]; check $? "the refusal names the backup the migrating install took: ${named:-none}"
  grep -q 'Nothing was changed' "$OUT"; check $? "and changes nothing"
  serving "$p1" "$rev1"

  expect 0 "rollback-restored" /usr/local/sbin/citar-deploy rollback --to "$p0" --restore-backup "$named" \
    --site-url "$SITE_URL"
  serving "$p0" "$rev0"

  expect 0 "install-$p1-again" bash "$RC1/deploy/citar-deploy.sh" install --reinstall --bundle "$RC1" \
    --site-url "$SITE_URL"
  serving "$p1" "$rev1"

  expect 1 "install-$p2-planted" bash "$RC2/deploy/citar-deploy.sh" install --bundle "$RC2" --site-url "$SITE_URL"
  grep -q 'The database was restored: yes' "$OUT"
  check $? "$p2 migrated, then its server exited: undone by itself, the database restored from the switch's backup"
  serving "$p1" "$rev1"

  expect 1 "install-$p3-planted" bash "$RC3/deploy/citar-deploy.sh" install --bundle "$RC3" --site-url "$SITE_URL"
  grep -q 'The database was restored: no (its revision is still' "$OUT"
  check $? "$p3's server exited before migrating: undone by itself, without a restore"
  serving "$p1" "$rev1"

  OUT="$LOGS/install-$p2-interrupted.log"
  step "install-$p2-interrupted"
  phase "install-$p2-interrupted"
  checker sites "$CHECKS" --when "before: the interrupted install" || FAILED=$((FAILED + 1))
  interrupted_install > "$OUT" 2>&1
  local status=$?
  sed 's/^/   | /' "$OUT"
  { [ "$status" -eq 1 ] && grep -q 'interrupted (SIGTERM)' "$OUT"; }
  check $? "a TERM during $p2's health wait undoes the install (exit $status, 'interrupted (SIGTERM)')"
  serving "$p1" "$rev1"
  [ -z "$(find /var/lib/citar-deploy -maxdepth 1 -name 'run.*' 2>/dev/null)" ]
  check $? "no /var/lib/citar-deploy/run.* folder is left"
  checker sites "$CHECKS" --when "after: the interrupted install" || FAILED=$((FAILED + 1))

  drill

  expect 0 "uncutover" /usr/local/sbin/citar-deploy uncutover --site-url "$SITE_URL"
  checker health "$DIRECT_URL" --site "$SITE_URL" --seconds 60
  check $? "CITAR 0.1.5 answers again, directly and through its site"
  [ -f /opt/citar/citar/__init__.py ] && systemctl is-active --quiet citar-lab.service
  check $? "0.1.5's tree is back in /opt/citar and citar-lab runs"

  phase "end"
  systemctl stop citar-staging-watch.service
  step "The run's figures (staging_check.py report)"
  checker report --logs "$LOGS" | sed 's/^/   | /'
  check "${PIPESTATUS[0]}" "the timeline's checks"
  journalctl -u citar -u citar-lab -u citar-backup --since "@$(sed -n '1s/.*"t": \([0-9]*\).*/\1/p' "$LOGS/phases.jsonl")" \
    --no-pager > "$LOGS/journal.log" 2>&1
  cp /var/log/citar-deploy.log "$LOGS/citar-deploy.log" 2>/dev/null
  say ""
  if [ "$FAILED" -eq 0 ]; then
    say "staging run $stamp: every check passed. Logs: $LOGS"
    return 0
  fi
  say "staging run $stamp: $FAILED check(s) FAILED (above). Logs: $LOGS"
  return 1
}

# ----------------------------------------------------------------------------------------------------- cleanup

cmd_cleanup() {
  local unit swap path left=()
  where_ok
  step "Units"
  for unit in citar-staging-watch.service citar-staging-drill.service citar-staging-probe-oom.service \
      citar-staging-probe-cpu.service citar.service citar-lab.service citar-backup.timer citar-backup.service \
      "${OWN_UNITS[@]}"; do
    systemctl stop "$unit" 2>/dev/null
    systemctl disable "$unit" 2>/dev/null
  done
  pkill -u citar 2>/dev/null
  rm -f "$UNITS"/citar.service "$UNITS"/citar-lab.service "$UNITS"/citar-backup.service "$UNITS"/citar-backup.timer
  rm -f "$UNITS"/*.wants/citar*
  rm -f "$UNITS/$SLICE" "${OWN_UNITS[@]/#/$UNITS/}"
  rm -rf "$UNITS"/citar*.service.d "$UNITS/$SLICE.d" /run/systemd/system.control/citar* \
    "/run/systemd/system.control/$SLICE.d"
  systemctl daemon-reload
  # The slice, and the citar.slice its name puts it in (a dash nests a slice).
  systemctl stop "$SLICE" citar.slice 2>/dev/null
  systemctl reset-failed 2>/dev/null
  ok "the citar, citar-lab and backup units, the stand-ins, their drop-ins and the slice removed"

  step "Files"
  rm -f /usr/local/bin/citar-admin /usr/local/bin/citar-backup /usr/local/bin/citar-secrets /usr/local/sbin/citar-deploy
  rm -f /etc/nginx/sites-enabled/citar /etc/nginx/sites-available/citar \
    /etc/nginx/sites-enabled/citar-staging-static /etc/nginx/sites-available/citar-staging-static
  if [ "$(recorded NGINX_DEFAULT)" = 1 ] && [ -f /etc/nginx/sites-available/default ]; then
    ln -sfn /etc/nginx/sites-available/default /etc/nginx/sites-enabled/default
    ok "nginx's default site enabled again, as the package installed it"
  fi
  if [ -n "$KEEP_LOGS" ] && [ -d "$STAGE/logs" ]; then
    mkdir -p "$KEEP_LOGS" && cp -a "$STAGE/logs/." "$KEEP_LOGS/" && ok "the logs copied to $KEEP_LOGS"
  fi
  swap="$(recorded SWAPPINESS)"
  shopt -s nullglob
  # Every name beginning citar: the releases' trees and states moved aside (-<version>-undone-<stamp>, a restore's
  # .before-restore-<stamp>), the archive, citar-deploy's and staging's own folders.
  rm -rf /opt/citar* /var/lib/citar* /etc/citar /var/backups/citar \
    /var/log/citar-deploy.log
  shopt -u nullglob
  ok "/opt/citar*, /var/lib/citar*, /etc/citar, /var/backups/citar and /var/log/citar-deploy.log removed"
  if id citar >/dev/null 2>&1; then
    userdel citar 2>/dev/null || userdel -f citar
  fi
  getent group citar >/dev/null && groupdel citar
  ok "the citar account removed"

  step "System settings"
  if [ -n "$swap" ]; then
    sysctl -q -w vm.swappiness="$swap"
  fi
  if command -v nginx >/dev/null 2>&1; then
    systemctl stop nginx 2>/dev/null
    systemctl disable nginx 2>/dev/null
  fi
  # The CITAR site's own logs (write_citar_site names them), rotated ones included, once nginx has let go of them;
  # nginx's own access.log and error.log stay with the package.
  rm -f /var/log/nginx/citar.access.log* /var/log/nginx/citar.error.log*
  ok "the CITAR site's nginx logs removed (/var/log/nginx/citar.*)"

  step "Checked gone (S10's WSL items)"
  ! id citar >/dev/null 2>&1 || left+=("the citar account")
  shopt -s nullglob
  for path in /opt/citar* /var/lib/citar* /etc/citar /var/backups/citar /var/log/citar-deploy.log "$UNITS"/citar* \
      "$UNITS"/*.wants/citar* /usr/local/bin/citar-* /usr/local/sbin/citar-deploy /etc/nginx/sites-*/citar* \
      /var/log/nginx/citar*; do
    if [ -e "$path" ] || [ -L "$path" ]; then left+=("$path"); fi
  done
  shopt -u nullglob
  [ -z "$(systemctl list-units --all --plain --no-legend 'citar*' "$SLICE" 2>/dev/null)" ] || left+=("a citar unit")
  ! pgrep -u citar >/dev/null 2>&1 || left+=("a process of citar")
  if command -v nginx >/dev/null 2>&1; then
    ! systemctl is-active --quiet nginx || left+=("nginx running")
    [ "$(systemctl is-enabled nginx 2>/dev/null)" != enabled ] || left+=("nginx enabled")
  fi
  if [ -n "$swap" ]; then
    [ "$(cat /proc/sys/vm/swappiness)" = "$swap" ] || left+=("vm.swappiness $(cat /proc/sys/vm/swappiness), not $swap")
  else
    say "   (no setup record, so vm.swappiness was left as it is: $(cat /proc/sys/vm/swappiness))"
  fi
  if [ "${#left[@]}" -eq 0 ]; then
    ok "no citar account, /opt/citar*, /var/lib/citar*, /etc/citar, citar unit, staging slice or CITAR nginx site or"
    say "      log; nginx stopped and disabled; vm.swappiness ${swap:-?}"
    return 0
  fi
  fail "left behind: ${left[*]}"
  return 1
}

cmd_status() {
  say "setup:       $( [ -f "$RECORD" ] && tr '\n' ' ' < "$RECORD" || printf 'not set up')"
  say "swappiness:  $(cat /proc/sys/vm/swappiness)"
  say "slice:       $(systemctl is-active "$SLICE" 2>/dev/null) $( [ -d "$(slice_dir)" ] && printf '(memory.max %s, cpu.max %s)' "$(cat "$(slice_dir)/memory.max")" "$(cat "$(slice_dir)/cpu.max")")"
  local unit
  for unit in citar.service citar-lab.service "${OWN_UNITS[@]}" nginx.service; do
    say "unit:        $unit $(systemctl is-active "$unit" 2>/dev/null)"
  done
}

case "$COMMAND" in
  setup) cmd_setup ;;
  probes) cmd_probes ;;
  run) cmd_run ;;
  cleanup) cmd_cleanup ;;
  status) cmd_status ;;
esac
