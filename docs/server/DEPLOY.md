# Running a CITAR server

A public CITAR gives people accounts, invitations, single sign-on, shared games, per-user budgets,
and a way to lend their own GPUs to the server without opening a port at home.

Three routes. They produce the same thing.

| | Best for |
|---|---|
| [The installer](#the-installer) | A fresh Linux box, or one already running other sites |
| [Docker](#docker) | A machine where you would rather not install Python |
| [By hand](#by-hand) | An existing platform with its own conventions |

**What it needs:** a machine with a domain name pointing at it, one core and 1 GB of RAM to start,
and about 2 GB of disk. Games are CPU-bound, so capacity is roughly one concurrent game per core.

---

## Before you start

**DNS.** An `A` record for your domain pointing at the machine's IPv4 address, and an `AAAA` record
if it has IPv6. Certificate issuance fails without it, so do this first and let it propagate.

**A plan for sign-in.** CITAR needs at least one way in besides the admin account you create:

| | Needs |
|---|---|
| Invitations | Nothing. This is the default, and a server can go live with only this |
| E-mail and password | An SMTP relay |
| Single sign-on | An OAuth app registered with Google, GitHub, Discord or Microsoft — [OAUTH.md](OAUTH.md) |

Invite-only needs neither e-mail nor a captcha, which is why it is the default.

---

## The installer

```bash
curl -fsSL https://raw.githubusercontent.com/jprodgers/CITAR/main/install.sh | bash -s -- --server
```

It installs CITAR to `/opt/citar` in its own virtual environment and then runs
`citar setup --server`, which asks for:

- your **domain**,
- where **state** goes (default `/var/lib/citar`),
- **SQLite or PostgreSQL**,
- who may **sign up**,
- optionally **SMTP**, **OAuth** and **hCaptcha**,
- the first **administrator**.

Then, if you let it, it writes the systemd unit, adds an nginx site, and runs certbot.

Nothing is written until it shows you the whole list and you say yes.

### What it creates

| | |
|---|---|
| `/opt/citar` | The program |
| `/var/lib/citar` | Everything it writes. **This is the backup** |
| `/etc/citar/citar.env` | Configuration and secrets, mode 0600 |
| `/etc/systemd/system/citar.service` | The service |
| `/etc/nginx/sites-available/citar` | The site, alongside any you already have |
| A `citar` system account | No shell, no sudo |

### If you already run nginx

The installer adds a new site and leaves the others alone — it does not touch `default`, and it
does not take over port 443 from anything. It checks `nginx -t` before reloading, and if the
configuration is rejected it says so and leaves it unloaded.

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
wrong. If you already run a proxy, delete the `caddy` service, publish CITAR on `127.0.0.1:8765`,
and point your own at it.

State lives in the `citar-state` volume. Back that up; everything else is rebuildable.

---

## By hand

```bash
pip install "citar[server]"
citar setup --server                 # writes the environment file and can install the service
```

or fully manual: copy [env.example](https://github.com/jprodgers/CITAR/blob/main/env.example), fill it in, and run

```bash
env $(grep -v '^#' citar.env | xargs) citar serve
```

Migrations run at startup, so deploying a new version is: install, restart. `deploy/citar.service`
is a unit file to adapt.

---

## After it is up

```bash
citar doctor                         # what is configured, what is missing, what each gap costs
```

Or open **Server** in the top navigation, which shows the same checks in the browser and lets an
administrator change runtime policy, send a test e-mail, and see where the files are.

### Invite the first people

```bash
citar admin invite --email them@example.com
citar admin invite --role moderator --uses 5
```

### Set the capacity limits

Games are CPU-bound. The lever is per-account:

```bash
citar admin policy default_max_concurrent_games 2
```

On a single-core VPS, one concurrent game is realistic before turns start to drag.

### Turn on backups

The installer sets up a nightly backup timer. Check that it has run, and **practise the restore** —
a backup you have not restored is a hypothesis. [RUNBOOK.md](RUNBOOK.md#backups) has the drill.

---

## Hardening

CITAR runs as a service account with no shell and no sudo, under a systemd unit with
`ProtectSystem=strict` and one writable directory. Beyond that, the machine is yours; the
[VPS guide](VPS.md) covers SSH keys, fail2ban and a firewall, and none of it is CITAR-specific.

The one CITAR-specific thing worth knowing: **`MemoryMax=` on the unit means CITAR is what the
kernel kills** when the machine runs short of memory, rather than whatever else is on the box. On a
shared machine that is the difference between one service restarting and all of them falling over.

---

## Lending GPUs to the server

A worker lets somebody's home machine serve models to the server without opening a port on their
router — the connection is outbound. [WORKERS.md](WORKERS.md).

---

## Keeping it running

[RUNBOOK.md](RUNBOOK.md) — health checks, what to do when it will not start, backups and restores,
accounts, workers, TLS, capacity, and deploying a new version.
