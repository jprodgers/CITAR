# Runbook

What to do when something needs doing. Written for the person who did not build it and is looking
at it at an awkward hour.

It assumes a deployment made the way [DEPLOY.md](DEPLOY.md) describes: CITAR at `/opt/citar`, state
at `/var/lib/citar`, configuration at `/etc/citar/citar.env`, running under systemd as the `citar`
account behind nginx. Adjust the paths if yours differ; `citar where` prints them.

Throughout, `citar.example.com` stands for your domain.

---

## The five commands

```bash
sudo systemctl status citar          # is it running
sudo journalctl -u citar -f          # what is it doing
sudo systemctl restart citar         # turn it off and on again
sudo citar-admin config              # what does it think its configuration is
sudo citar-admin list-users          # who has an account
```

---

## Is it broken?

```bash
curl -s -o /dev/null -w '%{http_code}\n' https://citar.example.com/api/auth/config
```

`200` means the whole chain works: DNS, nginx, TLS, the service and the database.

If it is not 200, work outwards from the app:

| Check | Command | If it fails |
| --- | --- | --- |
| app | `curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8765/` | the service — see below |
| service | `systemctl status citar` | `journalctl -u citar -n 50` |
| nginx | `sudo nginx -t && systemctl status nginx` | fix the config it names, then `systemctl reload nginx` |
| DNS | `dig +short citar.example.com` | should be `203.0.113.10` |
| certificate | `sudo certbot certificates` | `sudo certbot renew` |

**The forum and the resume site are separate.** If `forum.example.com` is down but CITAR is
up, the problem is nginx, PHP-FPM or MariaDB — not CITAR. CITAR cannot take them down by design:
it runs under a 400 MB cap and is the first thing the kernel kills under memory pressure.

---

## It will not start

The logs are the first thing to read and they are usually explicit:

```bash
sudo journalctl -u citar -n 60 --no-pager
```

Known causes, in the order they actually happen:

**A configuration error.** Server mode refuses to start without a secret key, a public origin and
HTTPS. The message names the variable. Check `/etc/citar/citar.env`.

**A failed migration.** The schema is applied at startup. If a migration fails the service will not
start, on purpose — running against a half-migrated schema is worse. Restore the database (below)
and report the migration.

**Out of memory.** `systemctl status citar` shows `oom-kill`. The cap is 400 MB and normal use is
about 70 MB, so this means a leak or a very large game. Check `MemoryCurrent` over time. There is
2 GB of swap, so this should be rare.

**Port 8765 in use.** `sudo ss -tulpn | grep 8765`. Something else grabbed it, or an old process
did not die: `sudo systemctl restart citar`.

---

## Backups

Nightly at 03:20 UTC, kept 14 days, in `/var/backups/citar/<timestamp>/`.

```bash
systemctl list-timers citar-backup      # when it next runs
sudo systemctl start citar-backup       # run one now
sudo journalctl -u citar-backup -n 20   # what happened
```

Each backup holds `citar.db` (a proper `sqlite3 .backup` snapshot, not a copy of a file being
written to), `citar.env` and `saves.tar.gz`. Every run verifies the snapshot with
`PRAGMA integrity_check` and marks the directory `UNTRUSTWORTHY` if it fails.

⚠️ **Backups are on the same disk as the thing they back up.** They protect against a bad migration
or a mistaken delete; they do not protect against losing the VPS. Set
`CITAR_BACKUP_RSYNC_TARGET` in the backup service's environment to copy them off-box.

### Restoring

```bash
LATEST=$(ls -1d /var/backups/citar/20* | tail -1)

sudo sqlite3 "$LATEST/citar.db" 'PRAGMA integrity_check;'     # expect: ok
sudo systemctl stop citar
sudo cp /var/lib/citar/citar.db /var/lib/citar/citar.db.before-restore
sudo install -o citar -g citar -m 640 "$LATEST/citar.db" /var/lib/citar/citar.db
# The WAL and shared-memory files belong to the OLD database and will corrupt the restored one.
sudo rm -f /var/lib/citar/citar.db-wal /var/lib/citar/citar.db-shm
sudo systemctl start citar
sudo citar-admin list-users
```

Saves: `sudo tar xzf "$LATEST/saves.tar.gz" -C /var/lib/citar`.

**Drill it before you need it.** A backup nobody has restored is a hope. This exact procedure was
run on 2026-09-20 and CITAR opened the restored database and read its accounts.

---

## Accounts

```bash
sudo citar-admin create-admin --handle NAME --email you@example.com   # first admin
sudo citar-admin create-user --handle NAME --email them@example.com --role moderator   # active, verified
sudo citar-admin list-users
sudo citar-admin set-role NAME moderator
sudo citar-admin set-status NAME suspended --reason "why"     # the reason is required, and shown at sign-in
sudo citar-admin set-caps NAME run_lab=on publish_global=default   # on, off, or back to the role's default
sudo citar-admin set-limits NAME --games 4 --seats 8 --invites 2
sudo citar-admin set-trust NAME trusted              # new | provisional | trusted | quarantined
sudo citar-admin audit --actor NAME --action 'user.*' --limit 50   # --json for a script; --actor admin_cli: these commands' own
sudo citar-admin reset-password NAME
sudo citar-admin invite --role user --uses 1
sudo citar-admin policy registration invite       # open | invite | closed
```

Passwords are always prompted for, never passed as arguments — an argument ends up in the shell
history and in the process list. A script gives `create-user` its password on standard input instead
(`--password-stdin`, the first line).

**Locked out entirely?** The CLI does not need the web app or a session. `create-admin --force`
adds another administrator. The CLI changes accounts under the web page's rules (`set-role` will not demote
the last active administrator) and each change is an audit row of `admin_cli`'s; the same changes are on the
**Users** page (`#/admin/users`).

**The first administrator imports the old server registry.** A server upgraded from before 0.1.6 still has
`config/servers.json`; it is imported into the database once, the moment the first administrator exists (at a start
that has one, or when `create-admin` makes one), under its own ids and owned by that administrator, then renamed
`servers.json.imported-<date>-<time>` (never deleted). Until then the start's log says that no machine is usable yet.
Suspending an account disconnects the helpers of its machines at once; so do revoking a worker token and disabling
or removing a machine ([Workers](WORKERS.md#revocation)).

---

## Servers and workers

```bash
sudo citar-admin add-server --name "Desktop" --group "Home GPUs"   # registers and prints a token
sudo citar-admin worker-token "Desktop"                            # another token
sudo citar-admin workers                                           # what is registered
```

On the machine with the model, the CITAR helper (download it from the Servers page):

```bash
citar-helper --server https://citar.example.com --token TOKEN --name "Desktop"
```

The helper dials out, so nothing at that end needs a port forward. Add `--quiet "22:00-06:00"` for
quiet hours **enforced on that machine**, which the server cannot override.

A helper that will not connect:

- `The server refused this helper's token` — the token is wrong or revoked. Issue another.
- `another helper's key` — the machine's key was registered by another helper: compare the fingerprint
  with `citar-helper --version`, then Reset key… in the machine's Helper dialog.
- a 0.1.5 worker gets "This server runs CITAR 0.1.6, which needs the new CITAR helper": replace it
  ([Workers](WORKERS.md#moving-from-015); a 0.1.5 system unit needs `systemctl disable --now` first).
- connects then drops — check `sudo journalctl -u citar -f` while it tries.

---

## Secrets

```bash
sudo citar-secrets          # menu: email, hCaptcha, each sign-in provider
sudo citar-secrets show     # which secrets are set — never what they are
```

Prompted, never echoed, never in the shell history or the process list. `citar-secrets KEY=value`
is deliberately not accepted for exactly that reason.

Mail is `mail.privateemail.com:587` STARTTLS as `you@example.com`. Test it:

```bash
sudo citar-admin test-email you@somewhere.com
```

⚠️ Mail from a new subdomain often lands in spam until SPF and DKIM cover it. The domain's SPF
(`v=spf1 include:spf.privateemail.com ~all`) already authorises Private Email's servers, and CITAR
sends *through* them, so this should be fine — but check the spam folder on the first test. There
is no DMARC record; adding one is a single TXT entry and worth doing.

## TLS

Certbot renews automatically. To check:

```bash
sudo certbot certificates
sudo systemctl list-timers | grep certbot
```

Manual renewal: `sudo certbot renew`. It touches all three certificates (the site, the forum and
CITAR); `--cert-name citar.example.com` limits it to CITAR's.

---

## Security

```bash
sudo fail2ban-client status sshd     # who is currently banned
sudo ufw status                      # 22, 80, 443 only
sudo sshd -T | grep -i password      # must say: passwordauthentication no
```

SSH is keys-only and root login is blocked. **If `passwordauthentication` ever says `yes` again**,
check the ordering in `/etc/ssh/sshd_config.d/` — OpenSSH takes the *first* value it reads, and
Ubuntu's `50-cloud-init.conf` sets it to `yes`. CITAR's file is `01-` so that it wins.

The audit log records logins, role changes, grants, suspensions, publishes and deletions:

```bash
sudo sqlite3 /var/lib/citar/citar.db \
  "SELECT datetime(at), actor_label, action, object_type FROM audit_log ORDER BY at DESC LIMIT 20;"
```

---

## Deploying a new version

```bash
# from the laptop, in the project folder
git archive --format=tar HEAD | gzip -9 > /tmp/citar.tar.gz
scp -i ~/.ssh/citar_vps_ed25519 /tmp/citar.tar.gz deploy@203.0.113.10:/tmp/

# on the server
sudo systemctl start citar-backup          # always back up before a deploy
sudo tar xzf /tmp/citar.tar.gz -C /opt/citar
sudo chown -R citar:citar /opt/citar
sudo -u citar /opt/citar/.venv/bin/pip install -q --no-cache-dir -r /opt/citar/requirements.txt
sudo systemctl restart citar
sleep 8 && systemctl is-active citar
curl -s -o /dev/null -w '%{http_code}\n' https://citar.example.com/api/auth/config
```

Migrations run automatically at startup. If the service does not come back, the logs say why and
the backup you just took is one command away.

---

## The staging rehearsal

Rehearse a deploy before it touches the server: `scripts/deploy/staging.sh` plays the whole of it
inside WSL (Ubuntu 24.04, as on the box), held to the box's limits rather than the laptop's. It
deploys 0.1.5 the way the box has it, cuts over to the release with `citar-deploy`, measures, upgrades,
rolls back, plants two failed upgrades, drills a restore and undoes the cutover, checking the other
sites' stand-ins answer before and after every step. It changes the machine it runs on (a `citar`
account, nginx, a slice, `vm.swappiness`), so it refuses anything but WSL; never run it on a server.

**What makes WSL look like the box.** A slice, `citar-staging.slice`, that every staged unit joins
(and every `systemd-run` of `citar-deploy` and `citar-admin`, through `CITAR_DEPLOY_SLICE`), with the
box's memory and swap (`MemoryMax=961M`, `MemorySwapMax=2G`) and its one vCPU: one CPU of the laptop
(`AllowedCPUs=`, `--cpu`), so the slice's units take turns on it as the box's do and their CPU
weights decide between them, slowed by `CPUQuota=` to the box's speed (`--cpu-quota`, default 60%: a
laptop core is about 1.6 times the box's) in 10 ms periods (`--cpu-period`), so it is a slower core
rather than a fast one switched off for 40 ms in every 100; `vm.swappiness` 10, as the box has it; a ballast in the slice holding the other sites' resident memory (`--ballast-mb`, the
operator's own measurement of the box, which the repository does not record); and two stand-in
sites, an nginx server block and a small Python site doing 25 ms of CPU a request (php-fpm's
stand-in), listed in `/etc/citar/deploy-checks.conf`. Before anything is measured, probes prove each
limit binds: the slice's files read back as asked, a process in it may run on its one CPU alone, a
unit with a small `MemoryMax` and no swap is killed by the kernel (the slice's `oom_kill` rises), and
a busy loop gets the quota's share of the CPU and no more. `probes --without-cpu-quota` is the probe
that must fail: it takes the quota away, checks the busy loop then gets more than the quota's share,
puts the quota back and reads the slice's files again, and exits 0 only when all of that held.
`IOWeight=` cannot be proven in WSL (its virtual disk weighs no one's IO); it is the box's alone.

**The releases.** As your own account in WSL (podman, rootless), from a clone of the commit being
deployed (never under `/mnt`, nor its output): the release's wheel and three planted ones, each made
into a deploy bundle.

```bash
bash scripts/deploy/staging_wheels.sh --clone ~/citar --out ~/citar-staging
#   the cutover's release: the clone's own wheel, built in maturin's container (pinned by digest)
#   with the release's flags, though CI builds the release's own in the manylinux_2_28 image; so
#   for a real release or a dry run, its linux-x64 wheel and constraints: --cutover-from <folder>
#   (an unpacked bundle.py bundle holds both)
#   X.Y.Z-rc.1 (the next patch release): one empty migration, an upgrade that migrates
#   X.Y.Z-rc.2: a second migration, and its server exits right after migrating
#   X.Y.Z-rc.3: the same, but its server exits before migrating
#   --bundles-only makes the bundles again from wheels already built (after a change to deploy/)
```

**The run.** As root, from copies of the two scripts on WSL's own disk (never under `/mnt/c`):

```bash
sudo bash staging.sh setup --ballast-mb N --repo ~/citar      # the limits, their probes, 0.1.5
sudo bash staging.sh probes --without-cpu-quota                # the probe fails without the quota (exit 0)
sudo bash staging.sh run --bundle ~/citar-staging/bundles/citar-deploy-V \
    --rc1 .../citar-deploy-X.Y.Z-rc.1 --rc2 .../citar-deploy-X.Y.Z-rc.2 --rc3 .../citar-deploy-X.Y.Z-rc.3 \
    --host-mb 128
sudo bash staging.sh cleanup --keep-logs /mnt/c/<somewhere>    # everything undone, then checked gone
```

`run` checks, each on a line of its own (`ok` or `FAILED`), and exits 0 only when all of them pass:

| Step | What must hold |
|---|---|
| before anything | the slice as setup left it: its files read back (memory, swap, the quota, the one CPU), the ballast held, the stand-in running, `vm.swappiness` 10; otherwise `run` stops there |
| `cutover` | 0.1.5 archived, the release staged, verified as `citar` in the slice, switched and healthy within 60 s, its WebSocket upgrade through nginx |
| the measurements | the first administrator made with `citar-admin create-user --password-stdin`; the host machine's settings set to what server mode computes for one CPU and 961 MB (`GET /api/servers` names the host machine; `PUT /api/machines/{id}/settings`); a 4-bot game of 40 rounds; three lab duels played by the job host inside `citar.service`, its `ready` at 240 MB held by `RLIMIT_DATA`; no OOM kill; with the lab running, the stand-in's p95 within twice its idle p95 plus 50 ms at the unit's own `CPUWeight=` (other weights are measured beside it); with `--host-mb`, the duels again at that memory figure |
| `install` of rc.1 | it migrates, answers as rc.1 |
| `rollback` | refused without `--restore-backup`, naming the backup the migrating install took, nothing changed; then accepted with it: the previous release serving at its own revision |
| `install --reinstall` of rc.1 | healthy again |
| `install` of rc.2 | undone by itself, the database restored from the switch's backup, rc.1 serving at its revision, exit 1 |
| `install` of rc.3 | undone by itself without a restore, exit 1 |
| rc.2 again, interrupted | a TERM during its health wait undoes it the same way, no `run.*` copies left |
| backup and drill | `citar-backup` whole and integrity-checked; a second CITAR on a copy of it, on another port, signs the administrator in and reads its accounts |
| `uncutover` | 0.1.5 back, answering directly and through its site, `citar-lab` running |

The logs are in `/var/lib/citar-staging/logs/run-<stamp>/`: each step's output, `measure.json`
(memory per phase: idle, the game, the lab and each weight's load, as `citar.service`'s own peak and
systemd's `MemoryCurrent`/`MemoryPeak`; the job host's resident peak and its runs' peaks; seconds
a round; the stand-in's and CITAR's response times), `watch.jsonl` (the timeline, sampled four times
a second) and `report.json` (each `pip install` scope's time and peak, the switches' downtime, the
slice's peak and what held it). The figures that settled `citar.service`'s `CPUWeight=`, `IOWeight=`,
`MemoryMax=` and `MemoryHigh=` are written beside them in `deploy/citar.service`; the host machine's
memory figure the box should use (128 MB) is the one `--host-mb` proves.

---

## Capacity

One vCPU and 961 MB. Games are CPU-bound, so roughly **one game at a time** before turns slow and
the forum starts feeling it. The per-account limits (`max_concurrent_games`, default 2) are the
lever; lower them with `citar-admin` if the box is struggling.

```bash
uptime                                        # load average
systemctl show citar -p MemoryCurrent
free -h
```

Sustained load above ~1.5 means CITAR and PHP-FPM are fighting. More users means a bigger VPS, or
moving game execution onto worker machines — the job-queue boundary is already in place for that.
