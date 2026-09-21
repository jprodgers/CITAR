# Preparing a VPS

Everything on this page is about the machine, not about CITAR. If you already run servers, skip it
— [DEPLOY.md](Deploying-a-server) is the part that is CITAR-specific.

## What to rent

| | |
|---|---|
| **CPU** | The thing that matters. Games are CPU-bound and single-threaded per game, so cores ≈ concurrent games. One core runs one game |
| **RAM** | 1 GB is enough: about 95 MB for CITAR plus 2–7 MB per loaded game. Add **2 GB of swap** if the machine has none |
| **Disk** | 2 GB for the program; saved games are a few hundred kilobytes each |
| **OS** | Any current Linux. Debian and Ubuntu are the best-tested |
| **Network** | IPv4. IPv6 too if you can, but nothing requires it |

Per-core speed matters less than you would think: a mid-range VPS core is well under twice as slow
as a laptop core. The constraint is *how many at once*, not how fast.

## Before CITAR

### A non-root account with sudo

```bash
adduser --disabled-password --gecos "" deploy
usermod -aG sudo deploy          # wheel on RHEL-family systems
mkdir -p /home/deploy/.ssh && chmod 700 /home/deploy/.ssh
# put your public key in /home/deploy/.ssh/authorized_keys
chmod 600 /home/deploy/.ssh/authorized_keys
chown -R deploy:deploy /home/deploy/.ssh
```

### Keys only, no passwords

```bash
sudo tee /etc/ssh/sshd_config.d/01-keys-only.conf >/dev/null <<'EOF'
PasswordAuthentication no
AuthenticationMethods publickey
PermitRootLogin prohibit-password
MaxAuthTries 3
LoginGraceTime 30
EOF
sudo sshd -t && sudo systemctl reload ssh
```

**Test a new session before closing the one you have.** Locking yourself out of a remote machine is
a slow problem to fix.

Two gotchas that cost people afternoons:

- **OpenSSH takes the *first* value it reads.** Ubuntu ships `/etc/ssh/sshd_config.d/50-cloud-init.conf`
  containing `PasswordAuthentication yes`. A drop-in numbered `01-` wins; one numbered `60-` does
  not, however correct it looks.
- **cloud-init rewrites it on reboot** unless you also set `ssh_pwauth: false` in
  `/etc/cloud/cloud.cfg.d/99-no-pwauth.cfg`.

### fail2ban

If the machine has a public SSH port, it is being brute-forced right now.

```bash
sudo apt install fail2ban
sudo tee /etc/fail2ban/jail.d/sshd.local >/dev/null <<'EOF'
[sshd]
enabled = true
backend = systemd
maxretry = 3
bantime = 1h
EOF
sudo systemctl restart fail2ban
sudo fail2ban-client status sshd
```

`backend = systemd` matters on Ubuntu 24.04 and similar: sshd logs to the journal, and the stock
file-based jail watches `/var/log/auth.log`, which does not exist — so it matches nothing while
looking perfectly healthy.

### A firewall

```bash
sudo ufw allow OpenSSH
sudo ufw allow 80,443/tcp
sudo ufw enable
```

CITAR itself listens on loopback. Only the proxy is exposed.

### Swap, if there is none

```bash
sudo fallocate -l 2G /swapfile && sudo chmod 600 /swapfile
sudo mkswap /swapfile && sudo swapon /swapfile
echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
```

With zero swap, one memory spike makes the kernel kill whatever it likes — which on a shared box
may be your database rather than CITAR.

### Unattended security updates

```bash
sudo apt install unattended-upgrades
sudo dpkg-reconfigure -plow unattended-upgrades
```

## DNS

An `A` record for your domain pointing at the machine's IPv4 address, and `AAAA` if it has IPv6.
Do this first: certificate issuance checks it, and propagation is the one part you cannot hurry.

```bash
dig +short citar.example.com
```

## If the machine already runs other sites

Normal, and fine. CITAR adds a site to the existing nginx rather than replacing anything, and
listens on loopback so it does not contend for port 443.

Two things to think about when sharing:

- **Memory.** Set `MemoryMax=` on the CITAR unit so that CITAR is what dies under pressure, not the
  other site's database.
- **CPU.** A busy game will use a whole core. If something latency-sensitive shares the machine,
  cap CITAR with `CPUQuota=` and keep `default_max_concurrent_games` low.

## Then

[DEPLOY.md](Deploying-a-server).


---

*This page is generated from [`docs/server/VPS.md`](https://github.com/jprodgers/CITAR/blob/main/docs/server/VPS.md) and any edit made here will be overwritten. Corrections are welcome as a pull request.*
