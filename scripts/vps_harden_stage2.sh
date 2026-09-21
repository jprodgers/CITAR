#!/usr/bin/env bash
# Stage 2 hardening: turn off SSH password authentication.
#
# Run ONLY after the operator has a working key. This is the step that can lock somebody out of
# their own server, so it refuses to proceed unless it can prove there is another way in:
#
#   * an authorized_keys file exists for a human account
#   * it contains at least one syntactically valid key
#   * that account can actually use sudo
#
# If any check fails it changes nothing and says why.
#
# Undo, if run from a session that is still open (or from the provider's web console):
#   sudo rm -f /etc/ssh/sshd_config.d/01-citar-keys-only.conf && sudo systemctl reload ssh

set -uo pipefail

ok()   { printf '    [ok] %s\n' "$1"; }
warn() { printf '    [!!] %s\n' "$1"; }
say()  { printf '\n\033[1m--- %s\033[0m\n' "$1"; }

say "checking there is a key-based way in before removing the password one"

FOUND=0
for home in /home/*; do
  user=$(basename "$home")
  ak="$home/.ssh/authorized_keys"
  sudo test -f "$ak" || continue
  # ssh-keygen -l fails on a malformed file, which is exactly the case we must not trust.
  n=$(sudo ssh-keygen -l -f "$ak" 2>/dev/null | wc -l)
  [ "$n" -gt 0 ] || { warn "$user: authorized_keys present but contains no valid key"; continue; }
  sudo -l -U "$user" 2>/dev/null | grep -qE '\(ALL|NOPASSWD' && s="sudo" || s="no sudo"
  ok "$user: $n valid key(s), $s"
  FOUND=$((FOUND + 1))
done

if [ "$FOUND" -eq 0 ]; then
  warn "No account has a usable SSH key."
  warn "Disabling password login now would lock everyone out. Nothing changed."
  exit 1
fi

say "disabling password authentication"
# The filename prefix is load-bearing. OpenSSH uses the FIRST value it reads for a keyword, and
# drop-ins are read in lexical order, so Ubuntu's 50-cloud-init.conf ("PasswordAuthentication yes")
# beats anything numbered above it. A 70- file here was silently ignored: sshd -T still reported
# "passwordauthentication yes" while looking like it had been applied.
sudo tee /etc/ssh/sshd_config.d/01-citar-keys-only.conf >/dev/null <<'CONF'
# CITAR stage 2: keys only.
#
# The box was taking thousands of password guesses a day. With this in place a guess cannot succeed
# no matter how many are made, which is a stronger guarantee than any rate limit.
#
# Delete this file and reload ssh to put password login back.
PasswordAuthentication no
KbdInteractiveAuthentication no
ChallengeResponseAuthentication no
PubkeyAuthentication yes
AuthenticationMethods publickey
CONF

if sudo sshd -t; then
  sudo systemctl reload ssh 2>/dev/null || sudo systemctl reload sshd
  ok "applied and reloaded — open sessions are unaffected"
else
  sudo rm -f /etc/ssh/sshd_config.d/01-citar-keys-only.conf
  warn "sshd config INVALID — reverted, nothing changed"
  exit 1
fi

# cloud-init rewrites 50-cloud-init.conf with "yes" whenever it runs, so tell it not to.
printf 'ssh_pwauth: false
' | sudo tee /etc/cloud/cloud.cfg.d/99-citar-no-pwauth.cfg >/dev/null
ok "cloud-init told not to re-enable password auth"

# Prove it rather than trusting the file: sshd -T is the only authority on what is in effect.
if sudo sshd -T 2>/dev/null | grep -qi '^passwordauthentication no'; then
  ok "verified: passwordauthentication no"
else
  warn "PASSWORD AUTH IS STILL ON despite the config — check drop-in ordering in /etc/ssh/sshd_config.d/"
fi

say "effective settings"
sudo sshd -T 2>/dev/null | grep -Ei '^(passwordauthentication|pubkeyauthentication|permitrootlogin|authenticationmethods|maxauthtries)' | sed 's/^/        /'

say "done"
echo "        Password login is OFF. Keep this session open until you have confirmed"
echo "        a NEW terminal can still connect with your key."
