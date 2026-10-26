# Running a CITAR server

A public CITAR gives people accounts, invitations, single sign-on, shared games, per-user budgets,
and a way to lend their own GPUs to the server without opening a port at home.

Three routes, each installing the same release wheel. They produce the same server.

| | Best for |
|---|---|
| [The installer](#the-installer) | A fresh Linux box, or one already running other sites |
| [Docker](#docker) | A machine where you would rather not install Python |
| [By hand](#by-hand) | An existing platform with its own conventions |

A box that also serves other sites, and any box you upgrade with care, deploys each release with
[`citar-deploy`](#citar-deploy-a-scripted-deploy-for-a-shared-box): the same layout as the installer's, with every
step checked and a failed release undone by itself.

**What it needs:** a Linux machine (x64 or ARM64, glibc 2.28 or newer, Python 3.11 or newer; Ubuntu 24.04's 3.12 is
what the deploy is rehearsed on) with a domain name pointing at it, one core and 1 GB of RAM to start, and about 2 GB
of disk (three releases of about 200 MB each, the state and its backups). Games are CPU-bound, so capacity is roughly
one concurrent game per core.

---

## Before you start

**DNS.** An `A` record for your domain pointing at the machine's IPv4 address, and an `AAAA` record
if it has IPv6. Certificate issuance fails without it, so do this first and let it propagate.

**A plan for sign-in.** CITAR needs at least one way in besides the admin account you create:

| | Needs |
|---|---|
| Invitations | Nothing. This is the default, and a server can go live with only this |
| E-mail and password | An SMTP relay |
| Single sign-on | An OAuth app registered with Google, GitHub, Discord or Microsoft — [OAUTH.md](Single-sign-on) |

Invite-only needs neither e-mail nor a captcha, which is why it is the default.

---

## The release layout

The installer and `citar-deploy` install a server the same way: each release is a virtual environment of its own,
made from that release's wheel, and a link says which one runs. Nothing runs from a source tree, so nothing a release
did not ship can be imported, and switching releases (or back) is one rename.

| | |
|---|---|
| `/opt/citar/releases/<version>/venv` | A release: its own venv, root's, read-only to the service; beside it the wheel, its `pip freeze` and its `SHA256SUMS` line, and `.complete` once it was installed and checked |
| `/opt/citar/current` | A link to the release that runs, switched atomically |
| `/var/lib/citar` | Everything CITAR writes (`citar:citar`, 0750), and the service's working directory. **This is what the backup holds** |
| `/etc/citar/citar.env` | Configuration and secrets (`root:citar`, 0640) |
| `/etc/systemd/system/citar.service` | The service: `ExecStart=/opt/citar/current/venv/bin/citar serve` |
| `/usr/local/bin/citar-admin` | The administration commands, each run [as the service runs](Runbook#accounts) |
| `/usr/local/bin/citar-backup`, `citar-backup.service`, `citar-backup.timer` | The nightly backup ([RUNBOOK.md](Runbook#backups)) |
| `/usr/local/bin/citar-secrets` | Setting a secret without it reaching the shell history |
| `/etc/nginx/sites-available/citar` | The site, alongside any you already have |
| A `citar` system account | No shell, no sudo |

`citar-deploy` adds `/usr/local/sbin/citar-deploy`, `/etc/citar/deploy-checks.conf` (the box's other sites),
`/var/lib/citar-deploy` (its lock and records) and `/var/log/citar-deploy.log` (one line a deploy). The lab has no
unit of its own: it runs inside `citar.service` ([below](#the-lab-on-a-server)).

---

## The installer

```bash
curl -fsSL https://raw.githubusercontent.com/jprodgers/CITAR/main/install.sh | bash -s -- --server
```

It installs the release's wheel (the newest, or `--version X.Y.Z`; `--from` a wheel or a folder of wheels) into the
layout above, then runs `citar setup --server`, which asks for:

- your **domain**,
- where **state** goes (default `/var/lib/citar`; it must be a new or empty folder, or one already holding CITAR's
  state, never one overlapping the install),
- **SQLite or PostgreSQL**,
- who may **sign up**,
- optionally **SMTP**, **OAuth** and **hCaptcha**,
- the first **administrator**.

Then, if you let it, it writes the systemd unit, `citar-admin` and the nightly backup (enabling both the service and
the timer), adds an nginx site, and runs certbot. Nothing is written until it shows you the whole list and you say
yes, and `install.sh --server --plan` prints every step first with every path and changes nothing.

### Upgrading with the installer

Run it again with the newer release:

```bash
curl -fsSL https://raw.githubusercontent.com/jprodgers/CITAR/main/install.sh | bash -s -- --server --version 0.1.7
```

The new release is installed beside the running one, keeping the configuration. With the service installed, the
switch is made with it stopped: `systemctl stop citar`, a backup (`citar-backup`; one that is not trustworthy starts
the old release again and switches nothing), `current` switched, the unit, `citar-admin` and the backup written again
from the new release (`citar setup --server --refresh-units`), the service started, and `/api/meta` must answer as
the new version within 60 s. If it does not, the switch is undone: the service stopped, the database put back from
that backup if the new release migrated it, `current` back, the old release started; the run exits 1 saying so. An
older release than the running one is refused, and the release that runs is never removed. `--plan` prints the
switch step by step.

`citar-deploy` does the same switch with more checks around it (a staged verification, nginx and the box's other
sites); a box with other sites on it should use that.

### If you already run nginx

The installer adds a new site and leaves the others alone — it does not touch `default`, and it
does not take over port 443 from anything. It checks `nginx -t` before reloading, and if the
configuration is rejected it says so and leaves it unloaded. No deploy rewrites the site afterwards (certbot edits
it); `citar-deploy` shows how it differs from the release's `deploy/nginx-citar.conf` instead.

The two settings that matter, if you write your own site:

```nginx
# Live game state, benchmark progress and worker connections all ride on this.
proxy_set_header Upgrade $http_upgrade;
proxy_set_header Connection $connection_upgrade;

# A model turn is a long request, not a slow one. The default 60s cuts turns off mid-move.
proxy_read_timeout 1800s;
proxy_send_timeout 1800s;
```

`deploy/nginx-citar.conf` is a complete example.

---

## Docker

```bash
mkdir citar && cd citar
curl -O https://raw.githubusercontent.com/jprodgers/CITAR/main/docker-compose.yml
curl -o .env https://raw.githubusercontent.com/jprodgers/CITAR/main/.env.docker.example
mkdir -p deploy && curl -o deploy/Caddyfile \
  https://raw.githubusercontent.com/jprodgers/CITAR/main/deploy/Caddyfile

$EDITOR .env          # CITAR_DOMAIN, CITAR_SECRET_KEY, ACME_EMAIL
docker compose up -d
docker compose exec citar citar admin create-admin --handle you --email you@example.com
```

Caddy obtains and renews the certificate itself, which removes the step people most often get
wrong. If you already run a proxy, delete the `caddy` service, publish CITAR on `127.0.0.1` only (`ports:
["127.0.0.1:8765:8765"]`), and point your own at it.

The image, `ghcr.io/jprodgers/citar`, is built for `linux/amd64` and `linux/arm64` from the release's own wheels (on
Debian 13, Python 3.12), with the dependency versions the release was tested with. `:0.1.6` names a release, `:0.1`
the newest 0.1 release, `:latest` the newest. It runs as uid and gid 10001 with its state directory 0750; a volume
made by 0.1.5's image keeps working, since its files belong to uid 10001. Its health check asks the server directly,
so a container given `http_proxy` for its model APIs stays healthy.

State lives in the `citar-state` volume. Back that up; everything else is rebuildable. An upgrade is `docker compose
pull && docker compose up -d`: the server migrates its database at startup.

---

## By hand

```bash
python3 -m venv /opt/citar-venv
/opt/citar-venv/bin/pip install --only-binary=citar "citar[server]"
/opt/citar-venv/bin/citar setup --server   # writes the environment file and can install the service
```

`citar setup --server` writes `deploy/citar.service` with your paths put in (the release layout's own paths leave it
unchanged), so the unit the wizard installs and the one in the repository are one definition; `citar-admin` and the
backup come the same way. For the exact dependency versions a release was tested with, install with its
constraints file, a release asset: `pip install --constraint citar-0.1.6-server-constraints.txt
"citar[server]==0.1.6"`.

Or fully manual: copy [env.example](https://github.com/jprodgers/CITAR/blob/main/env.example) (every comment is on a
line of its own, since systemd reads anything after `=` as the value), fill it in, and run

```bash
env $(grep -v '^#' citar.env | xargs) citar serve
```

Migrations run at startup, so deploying a new version is: install, restart. A database a newer release has migrated
is refused by an older one, so take a backup before an upgrade you might want to undo.

---

## `citar-deploy`: a scripted deploy for a shared box

`citar-deploy` (`deploy/citar-deploy.sh`) puts a release on the server from a **deploy bundle** and takes it back. It
is written for a small box that also serves other sites, where a deploy must not disturb them and a failed one must
undo itself:

1. **On your own machine**, `python scripts/deploy/bundle.py 0.1.6` (from a checkout with the tag fetched, `gh`
   signed in) downloads the release's Linux wheel and its constraints file, checks both against the release's
   `SHA256SUMS` and their provenance (`gh attestation verify`: made by `release.yml` for the tag `v0.1.6`, on GitHub's
   runners, from the same commit as the deploy scripts), and packs them with the tag's `deploy/` and `env.example`
   into `citar-deploy-0.1.6.tar.gz`. The server never downloads anything.
2. **Copy it to the server** and unpack it.
3. **`sudo bash citar-deploy-0.1.6/deploy/citar-deploy.sh install --bundle citar-deploy-0.1.6 --plan`**, read the
   plan, then the same without `--plan`. Once installed, it is `/usr/local/sbin/citar-deploy`.

What an `install` does, in order: every check before anything changes (the bundle's hashes, the wheel's platform and
version, Python, disk, the running release, the box's other sites answering 200, CITAR's own health); the new release
staged in its own venv (pip in a scope of its own, `MemoryMax=500M` and `CPUWeight=20`, so it cannot starve the other
sites); the staged release verified as the `citar` account, under the service's sandbox, with every state setting
pointed at a temporary folder (`citar --version`, the engine, `citar doctor` with its job host verifiable), so the new
code never opens the live state while the old release serves; then the switch with the service stopped, just after a
backup; the health checked within 60 s, directly and through nginx (a WebSocket handshake included); the other sites
checked again; the deploy recorded. A release that does not come up is undone by itself, the database put back from
that backup if it was migrated. [RUNBOOK.md](Runbook#deploying-a-new-version) has every command (`install`,
`rollback`, `status`, `prune`), its exit codes and what each does when interrupted.

`/etc/citar/deploy-checks.conf` lists the box's other sites, one URL a line (`#` comments): each must answer 200
before a deploy changes anything, and again after it. Make it on the box; an empty file serves a box with no other
sites. `citar-deploy` also needs `python3.12` with its `venv` module, `sqlite3` and `flock` on the box (on Ubuntu
24.04, `sudo apt install python3-venv sqlite3`), and the release layout's unit: a server installed by the installer
has it, and a 0.1.5 box gets it from the cutover (below).

### Moving from 0.1.5

0.1.5 ran from a copy of the source tree in `/opt/citar` with a venv at `/opt/citar/.venv`, beside a separate lab
service, `citar-lab.service`. `sudo bash citar-deploy-0.1.6/deploy/citar-deploy.sh cutover --bundle
citar-deploy-0.1.6` (with `--plan` first) moves such a box to the release layout: it stops `citar` and `citar-lab`,
takes 0.1.5's own backup, saves every unit and script it will replace, disables and removes `citar-lab.service`, moves
`/opt/citar` to `/opt/citar-0.1.5` and `/var/lib/citar` to `/var/lib/citar-archive-<stamp>`, makes a fresh state
directory (0.1.6 starts its database afresh), lists the env file's settings 0.1.6 no longer reads, and then installs
the release as above. Any failure undoes it by itself; `sudo citar-deploy uncutover` undoes it later. After it:
`sudo citar-admin create-user` for the first administrator, and the host machine's memory figure set to 128 MB
([below](#the-unit-and-limits-on-a-small-box)).

A 0.1.5 box upgraded any other way keeps `citar-lab.service`, which 0.1.6 retires (`citar lab run` exits 2, so the
unit would restart into that every 30 s): `sudo systemctl disable --now citar-lab && sudo rm
/etc/systemd/system/citar-lab.service && sudo systemctl daemon-reload`.

---

## The unit, and limits on a small box

`deploy/citar.service` is the one definition of the service, and the release layout is in it: the release's console
script (`/opt/citar/current/venv/bin/citar serve`), the state directory as its working directory, the settings from
`/etc/citar/citar.env`, and a sandbox in which it can write to its state directory and nothing else. On a box of one
vCPU and 961 MB that also serves other sites, its limits are what keep them safe, and each figure was measured by the
[staging rehearsal](Runbook#the-staging-rehearsal) on a slice held to that box:

| Thing | How it is held |
|---|---|
| CITAR and its games | `MemoryMax=400M` and `MemoryHigh=320M`. The server's own peak was 148 MB (at its first start; 90 MB idle, 96 MB with a 4-bot game) |
| When memory runs out | `OOMScoreAdjust=500`: the kernel kills CITAR, never another site's database |
| CITAR against the other sites | `CPUWeight=20` and `IOWeight=20`, the weights 0.1.5's lab unit had: under contention the other sites win. With lab games playing without pause, a stand-in site's p95 response time was 58 ms (66 at 50); CITAR's own pages kept theirs |
| The lab | the host machine's CPU settings ([below](#the-lab-on-a-server)): one thread, half the machine, and a memory figure of **128 MB** |
| A deploy's pip install | a scope of its own, `MemoryMax=500M`, `CPUWeight=20` |
| Releases on disk | three kept (`citar-deploy prune`); never the one that runs or the one before it |

`MemoryHigh` slows the whole cgroup, web server included, so the rule is: the server's peak plus the job host's
memory figure stays under `MemoryHigh` with 40 MB to spare. Server mode's default figure, min(1,024 MB, a quarter of
the memory), is about 240 MB on a 961 MB box, which with the server's peak would pass 320 MB; 128 MB is what the lab
was measured playing at (148 + 128 + 40 = 316). Set it after the first start, once the host machine exists: the
**Servers** page's machine settings for the host, or `PUT /api/machines/<host>/settings` with
`capabilities.cpu.memory_mb`. On a bigger box, raise `MemoryMax`/`MemoryHigh` and the figure together by the same
rule.

### The lab on a server

The [lab](Scripted-bots) (bot-vs-bot experiments) runs inside the server: its job dispatcher plays queued experiments
on a job host, a child process of the server (`python -P -m citar.jobs host`, so inside `citar.service`'s cgroup and
`MemoryMax`), under the host machine's CPU settings. In server mode they default to one game at a time, half the
machine and the memory figure above, held by `RLIMIT_DATA` (a system service has no user manager to make a cgroup
scope, so the CPU share is soft: `nice` and the one thread, and the unit's `CPUWeight` against the other sites). An
administrator changes them in the host's machine settings. Queue experiments from the Lab page (the A/B form), or
with `citar lab submit SPEC.json` run as the service runs (below); they show on the Queue and Lab pages.

---

## After it is up

Open **Server** in the top navigation: what is configured, what is missing and what each gap costs, and an
administrator can change runtime policy, send a test e-mail, and see where the files are. On the box, `citar doctor`
says the same, run with the service's account and settings (as `citar-admin` runs its commands; any other `citar`
command, `citar lab submit` among them, runs the same way):

```bash
sudo systemd-run --quiet --wait --collect --pty -p User=citar -p Group=citar -p WorkingDirectory=/var/lib/citar \
  -p Environment=CITAR_STATE_DIR=/var/lib/citar -p EnvironmentFile=/etc/citar/citar.env \
  /opt/citar/current/venv/bin/citar doctor
```

### Invite the first people

```bash
sudo citar-admin invite --email them@example.com
sudo citar-admin invite --role moderator --uses 5
```

### Set the capacity limits

Games are CPU-bound. The lever is per-account:

```bash
sudo citar-admin policy default_max_concurrent_games 2
```

On a single-core VPS, one concurrent game is realistic before turns start to drag.

### Backups

The installer sets up a nightly backup timer (`citar-backup.timer`, 03:20 UTC): the database, the whole state
directory, the configuration and the release that wrote them. Check that it has run, and **practise the restore** —
a backup you have not restored is a hypothesis. [RUNBOOK.md](Runbook#backups) has the drill.

---

## Hardening

CITAR runs as a service account with no shell and no sudo, under a systemd unit with
`ProtectSystem=strict` and one writable directory, from a release its account cannot change. `citar-admin` runs every
administration command the same way (as `citar`, through `systemd-run`, with the unit's settings and sandbox), so an
administration command can write nothing the service cannot. Beyond that, the machine is yours; the
[VPS guide](Preparing-a-VPS) covers SSH keys, fail2ban and a firewall, and none of it is CITAR-specific.

The one CITAR-specific thing worth knowing: **`MemoryMax=` on the unit means CITAR is what the
kernel kills** when the machine runs short of memory, rather than whatever else is on the box. On a
shared machine that is the difference between one service restarting and all of them falling over.

---

## Lending GPUs to the server

The CITAR helper lets somebody's home machine serve models to the server without opening a port on their
router — the connection is outbound. [WORKERS.md](Workers).

---

## Keeping it running

[RUNBOOK.md](Runbook) — health checks, what to do when it will not start, backups and restores,
accounts, workers, TLS, capacity, and deploying a new version.


---

*This page is generated from [`docs/server/DEPLOY.md`](https://github.com/jprodgers/CITAR/blob/main/docs/server/DEPLOY.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*
