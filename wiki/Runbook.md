# Runbook

What to do when something needs doing. Written for the person who did not build it and is looking
at it at an awkward hour.

It assumes a deployment made the way [DEPLOY.md](Deploying-a-server) describes: CITAR at `/opt/citar`, state
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
sudo citar-admin list-users
sudo citar-admin set-role NAME moderator
sudo citar-admin set-status NAME suspended --reason "why"
sudo citar-admin reset-password NAME
sudo citar-admin invite --role user --uses 1
sudo citar-admin policy registration invite       # open | invite | closed
```

Passwords are always prompted for, never passed as arguments — an argument ends up in the shell
history and in the process list.

**Locked out entirely?** The CLI does not need the web app or a session. `create-admin --force`
adds another administrator.

---

## Servers and workers

```bash
sudo citar-admin add-server --name "Desktop" --group "Home GPUs"   # registers and prints a token
sudo citar-admin worker-token "Desktop"                            # another token
sudo citar-admin workers                                           # what is registered
```

On the machine with the model:

```bash
python -m citar.worker --server https://citar.example.com --token TOKEN --name "Desktop"
```

The worker dials out, so nothing at that end needs a port forward. Add `--quiet "22:00-06:00"` for
quiet hours **enforced on that machine**, which the server cannot override.

A worker that will not connect:

- `That worker token was refused` — the token is wrong or revoked. Issue another.
- `ssl=None is incompatible` — an old worker. Update it.
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


---

*This page is generated from [`docs/server/RUNBOOK.md`](https://github.com/jprodgers/CITAR/blob/main/docs/server/RUNBOOK.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*
