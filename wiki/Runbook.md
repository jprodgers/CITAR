# Runbook

What to do when something needs doing. Written for the person who did not build it and is looking
at it at an awkward hour.

It assumes a deployment made the way [DEPLOY.md](Deploying-a-server) describes: the release layout, with the release that runs
at `/opt/citar/current` (a link into `/opt/citar/releases/<version>`), state at `/var/lib/citar`, configuration at
`/etc/citar/citar.env`, running under systemd as the `citar` account behind nginx. Adjust the paths if yours differ;
`citar where` prints them.

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

`citar-admin` is `/usr/local/bin/citar-admin`, which runs `/opt/citar/current/venv/bin/citar-admin` as the service
runs: as `citar`, through `systemd-run`, with the unit's environment file, working directory and sandbox, so it reads
the configuration exactly as the server does and can write nothing the server cannot. `citar-admin --plan list-users`
prints the command it would run. `sudo citar-deploy status` says which release runs, what else is installed, the
database's revision, the health and the last deploys.

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
it runs under a 400 MB cap, its lab games included, is the first thing the kernel kills under memory pressure, and
yields the CPU to them under contention (`CPUWeight=20`).

---

## It will not start

The logs are the first thing to read and they are usually explicit:

```bash
sudo journalctl -u citar -n 60 --no-pager
```

Known causes, in the order they actually happen:

**A configuration error.** Server mode refuses to start without a secret key, a public origin and
HTTPS. The message names the variable. Check `/etc/citar/citar.env`; every comment there must be on a line of its
own, since systemd reads anything after `=` as the value.

**A failed migration.** The schema is applied at startup. If a migration fails the service will not
start, on purpose — running against a half-migrated schema is worse. Restore the database (below)
and report the migration.

**A database a newer release migrated.** A release refuses a database at a revision its migrations do not know. That
happens when `current` is pointed back by hand after an upgrade migrated: go back with `citar-deploy rollback
--restore-backup` instead ([below](#going-back)), or forward again.

**Out of memory.** `systemctl status citar` shows `oom-kill`. The cap is 400 MB for the server and its lab games
together; the server idles at about 90 MB and peaked at 148 MB in the staging rehearsal, and the lab's job host is
held to its machine settings' memory figure (128 MB on a 961 MB box), so this means a leak, a very large game or a
figure set too high. Check `MemoryCurrent` over time. There is 2 GB of swap, so this should be rare.

**Port 8765 in use.** `sudo ss -tulpn | grep 8765`. Something else grabbed it, or an old process
did not die: `sudo systemctl restart citar`.

---

## Backups

Nightly at 03:20 in the box's own time zone, up to 20 minutes later (`citar-backup.timer`'s
`RandomizedDelaySec=20m`), kept 14 days (`CITAR_BACKUP_KEEP_DAYS`), in
`/var/backups/citar/<UTC time>/`. Every deploy takes one more, with the service stopped, just before it switches.

```bash
systemctl list-timers citar-backup      # when it next runs
sudo systemctl start citar-backup       # run one now
sudo journalctl -u citar-backup -n 20   # what happened
```

Each backup holds:

| File | What |
|---|---|
| `citar.db` | the database: a proper `sqlite3 .backup` snapshot (not a copy of a file being written to), checked with `PRAGMA integrity_check` |
| `state.tar.gz` | the whole state directory (saves, config, benchmarks, everything CITAR writes there) without the live database files |
| `data.tar.gz`, `saves.tar.gz`, `config.tar.gz`, `benchmarks.tar.gz` | only for a directory the env file moves outside the state directory |
| `citar.env` | the configuration and secrets (0600) |
| `RELEASE` | what wrote them: the release `current` named, its version, the database's revision, where each directory was |
| `UNTRUSTWORTHY` | only when something failed, saying what. Such a backup prunes nothing |

Its exit status, which `systemctl start citar-backup` reports: 0 complete (its last line is `backup complete:
<folder>`); 1 it could not start; 2 not trustworthy (`UNTRUSTWORTHY` says why); 3 complete and trustworthy on this
disk, but the offsite copy failed. A database outside SQLite (PostgreSQL) is named in `RELEASE` and not dumped:
back it up with its own tools.

⚠️ **Backups are on the same disk as the thing they back up.** They protect against a bad migration
or a mistaken delete; they do not protect against losing the VPS. Set
`CITAR_BACKUP_RSYNC_TARGET` in the backup service's environment to copy each new one off-box.

### Restoring

The whole state from a backup, release and all, is `citar-deploy rollback --restore-backup DIR` (below). By hand,
the database alone:

```bash
LATEST=$(ls -1d /var/backups/citar/20* | tail -1)
cat "$LATEST/RELEASE"                                          # the release and revision it was taken at
test -e "$LATEST/UNTRUSTWORTHY" && cat "$LATEST/UNTRUSTWORTHY" # must not exist

sudo sqlite3 "$LATEST/citar.db" 'PRAGMA integrity_check;'     # expect: ok
sudo systemctl stop citar
sudo cp /var/lib/citar/citar.db /var/lib/citar/citar.db.before-restore
sudo install -o citar -g citar -m 640 "$LATEST/citar.db" /var/lib/citar/citar.db
# The WAL and shared-memory files belong to the OLD database and will corrupt the restored one.
sudo rm -f /var/lib/citar/citar.db-wal /var/lib/citar/citar.db-shm
sudo systemctl start citar
sudo citar-admin list-users
```

The rest of the state: stop the service, move `/var/lib/citar` aside, make it again (`citar:citar`, 0750), extract
`state.tar.gz` into it as root (`sudo tar -xzpf "$LATEST/state.tar.gz" -C /var/lib/citar`, which keeps the owners),
put `citar.db` back as above, and start. The release that runs must know the backup's revision (`RELEASE`'s
`revision`), or it refuses the database: restore onto the release `RELEASE` names.

**Drill it before you need it.** A backup nobody has restored is a hope. The staging rehearsal drills it on every run
(the newest backup opened, integrity-checked, and a second CITAR started on a copy of it, reading its accounts).

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
or removing a machine ([Workers](Workers#revocation)).

---

## Servers and workers

```bash
sudo citar-admin add-server --name "Desktop" --group "Home GPUs"   # registers and prints a token
sudo citar-admin worker-token "Desktop"                            # another token
sudo citar-admin workers                                           # what is registered
```

On the machine with the model, the CITAR helper (download it from the Servers page, or
[INSTALL.md](Installing#the-citar-helper)'s other routes):

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
  ([Workers](Workers#moving-from-015); a 0.1.5 system unit needs `systemctl disable --now` first).
- connects then drops — check `sudo journalctl -u citar -f` while it tries.

---

## Secrets

```bash
sudo citar-secrets          # menu: email, hCaptcha, each sign-in provider
sudo citar-secrets show     # which secrets are set — never what they are
```

Prompted, never echoed, never in the shell history or the process list. `citar-secrets KEY=value`
is deliberately not accepted for exactly that reason.

`citar-deploy` installs `citar-secrets`; a server the installer made (`install.sh --server`) has none. There, edit
the environment file as root and restart, which keeps the value out of the shell history and the process list too:

```bash
sudoedit /etc/citar/citar.env       # KEY=value lines; a comment on a line of its own
sudo systemctl restart citar
```

The file stays `root:citar` 0640 (`sudoedit` keeps its owner and mode). The release's `deploy/citar-secrets.sh` (in
its tag, or in a deploy bundle from `bundle.py`) also works there, run as `sudo bash citar-secrets.sh`, when the
environment file is the default `/etc/citar/citar.env`: it writes that file and offers to restart `citar`.

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

A release goes onto the server as a **deploy bundle**, made and checked on your own machine, then installed by
`citar-deploy` ([DEPLOY.md](Deploying-a-server#citar-deploy-a-scripted-deploy-for-a-shared-box) says what each step does). The
server never downloads a release itself.

```bash
# on your own machine, in a checkout with the tag fetched (git fetch --tags) and gh signed in
python scripts/deploy/bundle.py 0.1.7          # the wheel and constraints from the release, hashes and attestation
                                               # checked: citar-deploy-0.1.7.tar.gz and its .sha256
scp citar-deploy-0.1.7.tar.gz citar-deploy-0.1.7.tar.gz.sha256 you@citar.example.com:

# on the server, inside tmux or screen
sha256sum -c citar-deploy-0.1.7.tar.gz.sha256 && tar -xzf citar-deploy-0.1.7.tar.gz
sudo citar-deploy install --bundle citar-deploy-0.1.7 --plan    # every check, then each step; nothing changes
sudo citar-deploy install --bundle citar-deploy-0.1.7
```

The first deploy on a box runs the bundle's own copy, `sudo bash citar-deploy-0.1.7/deploy/citar-deploy.sh`; each
deploy installs it as `/usr/local/sbin/citar-deploy`. **Read the plan** before the run: it names every path, every
command, the files that will be replaced (with their differences) and the settings the staged release is verified
with.

`install` stages the release, verifies it as `citar` with temporary state, stops the service, takes a backup, switches
`current`, installs the bundle's units and scripts where they differ (never the nginx site, whose difference it shows),
starts the service, checks its health within 60 s (directly, through nginx with a WebSocket handshake, and `/api/meta`
naming the new version), checks the other sites again, and records the deploy. The staged release's venv is made
fresh: a release already staged is refused without `--reinstall` (a release a rollback left behind, say).

| Exit | Means |
|---|---|
| 0 | done |
| 1 | refused before anything changed (the message names why, and ends "Nothing was changed"), or failed and undone: the release that ran is back, with the database restored from the switch's backup if the new release had migrated it; the message says which |
| 2 | a usage error |
| 3 | deployed and healthy, but another site in `/etc/citar/deploy-checks.conf` does not answer since. Nothing is done about it automatically (the cause may not be CITAR); the message gives the way back |

**Its files.** `/var/log/citar-deploy.log` has one line a deploy (`install version= previous= result=ok|undone
revision_before= revision_after= backup= downtime=`). `/var/lib/citar-deploy` holds its lock (a second deploy stops
there), the staged release's verification folders (removed afterwards) and a cutover's record. Each file a deploy
replaces is kept in `/var/lib/citar-deploy/run.<stamp>.*` until the deploy is recorded or undone; one left after a hard
kill is the copy of what was replaced.

**Interrupted.** Run it inside tmux or screen, so that a dropped SSH session does not end it. From the moment the
service stops until the deploy is recorded, a hang-up is ignored (the command runs to its end), and Ctrl-C or a TERM
undoes the install or cutover as a failed release is undone (a second Ctrl-C does not cut the undo short).
`rollback`, `uncutover` and `prune` run to their end once begun.

**The release that ran before** stays installed beside the new one, so a rollback can go back to it: a running
release without its `.complete` mark (one installed before the mark existed) is marked when its own `citar --version`
names it, and the install is refused otherwise. After a deploy, releases beyond three are pruned, never the one that
runs or the one before it.

```bash
sudo citar-deploy status            # the release that runs, every release and its mark, the service, the revision,
                                    # the health, a cutover's record, the last five deploys
sudo citar-deploy prune --keep 3    # releases beyond N removed (N at least 2)
```

### Going back

```bash
sudo citar-deploy rollback --plan                 # to the newest complete release older than the running one
sudo citar-deploy rollback --to 0.1.6
sudo citar-deploy rollback --to 0.1.6 --restore-backup /var/backups/citar/<the deploy's backup>
```

The server migrates its database on every start and refuses one a newer release migrated, so a rollback first reads
the database's revision against the target release's migrations. When the target knows it, `current` goes back, the
service restarts and the health is checked. When it does not (the newer release migrated), the rollback is refused
without `--restore-backup`, and the refusal names the backup to give it: the one the migrating deploy took (from the
deploy log), else the newest usable backup at a revision the target knows. With it, the service is stopped and the
state restored first: each folder the backup's `RELEASE` names is moved aside as `<dir>.before-restore-<stamp>` (kept)
and extracted again from its tarball with its owner and mode, a database outside those folders is moved aside as
`<db>.before-restore-<stamp>`, and the backup's `citar.db` put in its place. **What was written since that backup is
in the moved-aside copies, not in the restored state.**

---

## The cutover from 0.1.5, and undoing it

The first 0.1.6 deploy on a box that runs 0.1.5 (its source tree in `/opt/citar`, `/opt/citar/.venv`, and
`citar-lab.service`) is a cutover, not an install:

```bash
sudo bash citar-deploy-0.1.6/deploy/citar-deploy.sh cutover --bundle citar-deploy-0.1.6 --plan
sudo bash citar-deploy-0.1.6/deploy/citar-deploy.sh cutover --bundle citar-deploy-0.1.6
```

It recognises 0.1.5's layout (and refuses a box that has `current` or an earlier cutover), stops `citar` and
`citar-lab`, takes 0.1.5's own backup, saves each unit and script it will replace or remove (in
`/var/lib/citar-deploy/cutover/files`, with the record `cutover/CUTOVER`), disables and removes `citar-lab.service`,
moves `/opt/citar` to `/opt/citar-0.1.5` and `/var/lib/citar` to `/var/lib/citar-archive-<stamp>`, makes a fresh
`/var/lib/citar` (0.1.6 starts its database afresh), and lists the env file's settings 0.1.6 no longer reads (shown,
not changed). Then it stages, verifies, switches, enables `citar` and `citar-backup.timer`, and checks as `install`
does. Any failure from the first move on undoes it by itself and exits 1. The downtime includes the staging (about half
a minute in the rehearsal; a few minutes on a slow box), since the old tree must move before the new venv exists.

Afterwards: the first administrator, `sudo citar-admin create-admin --handle NAME --email you@example.com` (it asks
for the password), and the host machine's memory figure set to 128 MB
([DEPLOY.md](Deploying-a-server#the-unit-and-limits-on-a-small-box)).

`sudo citar-deploy uncutover` undoes it: the service stopped, `/opt/citar` and `/var/lib/citar` moved aside as
`-<version>-undone-<stamp>` (kept) where the record's archive is there to take their place, 0.1.5's tree and state
put back, its saved units restored (`citar-lab` included) and the files the cutover added removed, both services
started, 0.1.5's health and the other sites checked. The record is kept as `cutover-undone-<stamp>`.

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

The lab plays inside `citar.service` too, one game at a time on its job host, at `nice` and `CPUWeight=20` beside
the other sites, held to the host machine's memory figure (128 MB here: the server's peak plus that figure stays under
the unit's `MemoryHigh=320M`, [DEPLOY.md](Deploying-a-server#the-unit-and-limits-on-a-small-box)). Pause it from the Lab page,
or lower the host machine's threads and share in its machine settings, when the box is struggling.

```bash
uptime                                        # load average
systemctl show citar -p MemoryCurrent -p MemoryPeak
free -h
```

Sustained load above ~1.5 means CITAR and PHP-FPM are fighting. More users means a bigger VPS, or
moving work onto helper machines: their models already serve model seats, and a helper whose owner lends its CPU
plays the lab's games ([WORKERS.md](Workers#lending-cpu-what-plays-on-it-and-for-whom)).


---

*This page is generated from [`docs/server/RUNBOOK.md`](https://github.com/jprodgers/CITAR/blob/main/docs/server/RUNBOOK.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*
