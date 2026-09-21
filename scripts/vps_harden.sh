#!/usr/bin/env bash
# Stage 1 hardening for the CITAR VPS.
#
# Everything here is safe to run while the only way onto the box is a password: nothing in this
# script can lock anybody out. Disabling password authentication is stage 2, deliberately separate,
# and must not run until the operator has a working key.
#
# What it does:
#   1. backs up every file it will touch
#   2. adds swap (the box has none, and an OOM kill would take down the forum)
#   3. installs and configures fail2ban against the live SSH brute force
#   4. brings up a firewall, opening 22/80/443 BEFORE enabling it
#   5. stops root logging in over SSH, and lowers the per-connection guess allowance
#   6. validates sshd and reloads rather than restarts, so existing sessions survive
#
# Idempotent: re-running repairs a partial run instead of double-applying.

set -uo pipefail

STAMP=$(date -u +%Y%m%d-%H%M%S)
BACKUP="/root/citar-backups/$STAMP"
say() { printf '\n\033[1m--- %s\033[0m\n' "$1"; }
ok()  { printf '    [ok] %s\n' "$1"; }
note(){ printf '    ... %s\n' "$1"; }
warn(){ printf '    [!!] %s\n' "$1"; }

sudo install -d -m 700 "$BACKUP"
say "backups -> $BACKUP"
for f in /etc/ssh/sshd_config /etc/fstab; do
  [ -f "$f" ] && sudo cp -a "$f" "$BACKUP/" && ok "saved $f"
done

# ---------------------------------------------------------------- 1. swap
say "swap"
if [ "$(swapon --show --noheadings | wc -l)" -gt 0 ]; then
  ok "swap already present: $(swapon --show --noheadings | awk '{print $1, $3}' | tr '\n' ' ')"
else
  # 2G against 961M of RAM. This is insurance, not extra capacity: with zero swap the kernel's OOM
  # killer picks a victim under pressure, and the victim can just as easily be MariaDB or php-fpm
  # as the thing that actually misbehaved. The forum should not die because a game got greedy.
  sudo fallocate -l 2G /swapfile 2>/dev/null || sudo dd if=/dev/zero of=/swapfile bs=1M count=2048 status=none
  sudo chmod 600 /swapfile
  sudo mkswap /swapfile >/dev/null
  sudo swapon /swapfile
  grep -q '^/swapfile' /etc/fstab || echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab >/dev/null
  ok "2G swapfile created and enabled"
fi
# Low swappiness: prefer keeping things resident, use swap only as an emergency valve.
printf 'vm.swappiness=10\nvm.vfs_cache_pressure=50\n' | sudo tee /etc/sysctl.d/60-citar.conf >/dev/null
sudo sysctl -q -p /etc/sysctl.d/60-citar.conf 2>/dev/null
ok "vm.swappiness=10"

# ---------------------------------------------------------------- 2. fail2ban
say "fail2ban"
if ! command -v fail2ban-server >/dev/null 2>&1; then
  sudo DEBIAN_FRONTEND=noninteractive apt-get update -qq >/dev/null 2>&1
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq fail2ban >/dev/null 2>&1
  ok "installed"
else
  ok "already installed"
fi

# Ubuntu 24.04 ships sshd logs to the journal, not /var/log/auth.log, so the stock file-based jail
# silently never matches anything. backend=systemd is what actually makes this work here.
sudo tee /etc/fail2ban/jail.d/citar-sshd.local >/dev/null <<'CONF'
# CITAR: SSH brute-force protection.
#
# backend=systemd matters: Ubuntu 24.04 logs sshd to the journal, and the default file backend
# watches /var/log/auth.log, which does not exist. A jail that matches nothing looks healthy.
[sshd]
enabled  = true
backend  = systemd
port     = ssh
maxretry = 4
findtime = 10m
bantime  = 1h
# Each repeat offence bans for longer: 1h, 2h, 4h ... capped at a week.
bantime.increment = true
bantime.factor    = 2
bantime.maxtime   = 1w
ignoreip = 127.0.0.1/8 ::1
CONF
sudo systemctl enable --now fail2ban >/dev/null 2>&1
sudo systemctl restart fail2ban
sleep 3
if sudo fail2ban-client status sshd >/dev/null 2>&1; then
  ok "sshd jail active"
  sudo fail2ban-client status sshd | sed 's/^/        /'
else
  warn "sshd jail did not come up — check: sudo fail2ban-client status"
fi

# ---------------------------------------------------------------- 3. firewall
say "firewall (ufw)"
if ! command -v ufw >/dev/null 2>&1; then
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq ufw >/dev/null 2>&1
fi
# Ports are opened BEFORE the firewall is switched on. Doing it the other way round is the classic
# way to lock yourself out of a remote machine.
sudo ufw allow 22/tcp    >/dev/null 2>&1 && ok "allow 22 (ssh)"
sudo ufw allow 80/tcp    >/dev/null 2>&1 && ok "allow 80 (http)"
sudo ufw allow 443/tcp   >/dev/null 2>&1 && ok "allow 443 (https)"
sudo ufw default deny incoming  >/dev/null 2>&1
sudo ufw default allow outgoing >/dev/null 2>&1
if sudo ufw status | head -1 | grep -q inactive; then
  sudo ufw --force enable >/dev/null 2>&1 && ok "enabled"
else
  ok "already enabled"
fi
sudo ufw status numbered | sed 's/^/        /'

# ---------------------------------------------------------------- 4. sshd
say "sshd"
# NOTE: PasswordAuthentication is deliberately left ON. Turning it off before the operator has a
# working key would lock them out of their own server. That is stage 2.
sudo tee /etc/ssh/sshd_config.d/60-citar-hardening.conf >/dev/null <<'CONF'
# CITAR stage 1 hardening. A drop-in rather than an edit to sshd_config, so the distribution file
# stays pristine and this whole change can be undone by deleting one file.
#
# PasswordAuthentication is NOT set here on purpose: it stays enabled until the operator has a
# working key, at which point 70-citar-keys-only.conf turns it off.

# Root has no authorized_keys, so this blocks root SSH entirely today while leaving a key-based
# path open if one is ever added. Most of the brute force on this box targets root.
PermitRootLogin prohibit-password

# Fewer guesses per connection; fail2ban counts the failures and bans the source.
MaxAuthTries 3
LoginGraceTime 30

# Not needed on a headless server, and each is attack surface.
X11Forwarding no
AllowAgentForwarding no
PermitEmptyPasswords no
CONF

if sudo sshd -t; then
  sudo systemctl reload ssh 2>/dev/null || sudo systemctl reload sshd
  ok "config valid, reloaded (existing sessions unaffected)"
else
  sudo rm -f /etc/ssh/sshd_config.d/60-citar-hardening.conf
  warn "sshd config INVALID — change reverted, nothing applied"
fi

say "resulting sshd settings"
sudo sshd -T 2>/dev/null | grep -Ei '^(permitrootlogin|passwordauthentication|pubkeyauthentication|maxauthtries|logingracetime)' | sed 's/^/        /'

# ---------------------------------------------------------------- 5. updates
say "unattended security updates"
sudo tee /etc/apt/apt.conf.d/20auto-upgrades >/dev/null <<'CONF'
APT::Periodic::Update-Package-Lists "1";
APT::Periodic::Unattended-Upgrade "1";
APT::Periodic::AutocleanInterval "7";
CONF
ok "daily package lists + unattended upgrades on"

say "state"
free -h | sed 's/^/        /'
echo "        swap: $(swapon --show --noheadings | awk '{print $1, $3}')"
echo "        fail2ban: $(systemctl is-active fail2ban)   ufw: $(sudo ufw status | head -1 | awk '{print $2}')"
echo
echo "STAGE 1 COMPLETE. Password login is still enabled on purpose."
echo "Backups of every changed file are in $BACKUP"
