#!/usr/bin/env bash
# Create the two CITAR accounts on the VPS and authorize the deploy key.
#
# Paste this whole thing into an SSH session on the VPS. It is safe to run more than once — every
# step checks first, so re-running fixes a half-finished attempt rather than breaking it.
#
# It creates:
#   deploy   the account Claude connects as. Key-only login, passwordless sudo.
#   citar    a service account with no shell, no password and no sudo. CITAR runs as this.
#
# It touches nothing belonging to the website or the forum.
#
# Revoking all of it, at any time:
#   sudo rm -f /etc/sudoers.d/90-citar-deploy && sudo deluser --remove-home deploy

set -u

KEY='ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAII64+4u7VMLS+z8jKjvnFyEfw9LNxJlyzICwtgVqiVJb citar-deploy@jimmie-laptop'

say() { printf '\n--- %s\n' "$1"; }
ok()  { printf '    ok: %s\n' "$1"; }
bad() { printf '    !! %s\n' "$1"; }

if [ "$(id -u)" != "0" ] && ! sudo -n true 2>/dev/null; then
  echo "This needs sudo (or root). Stopping before changing anything."
  exit 1
fi
SUDO=""; [ "$(id -u)" != "0" ] && SUDO="sudo"

# ---------------------------------------------------------------- deploy account
say "deploy account"
if id deploy >/dev/null 2>&1; then
  ok "already exists"
else
  if command -v adduser >/dev/null 2>&1; then
    $SUDO adduser --disabled-password --gecos "" deploy >/dev/null 2>&1
  else
    $SUDO useradd -m -s /bin/bash deploy
  fi
  id deploy >/dev/null 2>&1 && ok "created" || bad "could not create deploy"
fi

# The sudo group is Debian/Ubuntu; wheel is RHEL/Rocky/Alma/Fedora. Add to whichever exists.
for g in sudo wheel; do
  if getent group "$g" >/dev/null 2>&1; then
    $SUDO usermod -aG "$g" deploy && ok "added to group '$g'"
  fi
done

# ---------------------------------------------------------------- the key
say "authorized key"
$SUDO install -d -m 700 -o deploy -g deploy /home/deploy/.ssh
if $SUDO grep -qF "${KEY##* }" /home/deploy/.ssh/authorized_keys 2>/dev/null; then
  ok "key already installed"
else
  printf '%s\n' "$KEY" | $SUDO tee -a /home/deploy/.ssh/authorized_keys >/dev/null
  ok "key installed"
fi
$SUDO chown deploy:deploy /home/deploy/.ssh/authorized_keys
$SUDO chmod 600 /home/deploy/.ssh/authorized_keys
ok "permissions set (sshd silently ignores a key if these are too open)"

# ---------------------------------------------------------------- passwordless sudo
# deploy has no password at all, so without this every sudo would prompt and fail. Validated before
# it is left in place: a malformed sudoers file can lock EVERYONE out of sudo, so if the check fails
# the file is removed immediately.
say "passwordless sudo for deploy"
printf 'deploy ALL=(ALL) NOPASSWD:ALL\n' | $SUDO tee /etc/sudoers.d/90-citar-deploy >/dev/null
$SUDO chmod 0440 /etc/sudoers.d/90-citar-deploy
if $SUDO visudo -c >/dev/null 2>&1; then
  ok "granted and sudoers validated"
else
  $SUDO rm -f /etc/sudoers.d/90-citar-deploy
  bad "sudoers validation FAILED — change reverted, nothing broken"
fi

# ---------------------------------------------------------------- service account
say "citar service account (no shell, no sudo)"
if id citar >/dev/null 2>&1; then
  ok "already exists"
else
  NOLOGIN=$(command -v nologin || echo /usr/sbin/nologin)
  if command -v adduser >/dev/null 2>&1 && adduser --help 2>&1 | grep -q -- --system; then
    $SUDO adduser --system --group --home /var/lib/citar --shell "$NOLOGIN" citar >/dev/null 2>&1
  else
    $SUDO useradd --system --create-home --home-dir /var/lib/citar --shell "$NOLOGIN" citar
  fi
  id citar >/dev/null 2>&1 && ok "created" || bad "could not create citar"
fi

# ---------------------------------------------------------------- checks
say "does sshd restrict who may log in?"
RESTRICT=$($SUDO grep -EihR '^[[:space:]]*(AllowUsers|AllowGroups|DenyUsers)' \
  /etc/ssh/sshd_config /etc/ssh/sshd_config.d/ 2>/dev/null)
if [ -n "$RESTRICT" ]; then
  bad "sshd restricts logins. 'deploy' must be added or it cannot connect:"
  printf '       %s\n' "$RESTRICT"
else
  ok "no AllowUsers/AllowGroups restriction — deploy can connect"
fi

say "connection details Claude needs"
echo "    public IPv4 : $(curl -fsS --max-time 8 https://api.ipify.org 2>/dev/null || echo '(unknown)')"
echo "    public IPv6 : $(curl -fsS --max-time 8 https://api6.ipify.org 2>/dev/null || echo '(none)')"
echo "    hostname    : $(hostname -f 2>/dev/null || hostname)"
echo "    ssh port    : $($SUDO grep -ihR '^[[:space:]]*Port' /etc/ssh/sshd_config /etc/ssh/sshd_config.d/ 2>/dev/null | awk '{print $2}' | head -1 || echo 22)"

say "result"
echo "    deploy groups : $(id -Gn deploy 2>/dev/null)"
echo "    citar  groups : $(id -Gn citar 2>/dev/null)"
if $SUDO -u deploy sudo -n true 2>/dev/null; then
  echo "    deploy sudo   : working"
else
  echo "    deploy sudo   : NOT working — tell Claude"
fi
echo
echo "Send Claude the public IP (or hostname) and the ssh port above."
