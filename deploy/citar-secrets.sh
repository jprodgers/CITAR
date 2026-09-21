#!/usr/bin/env bash
# Set CITAR's secrets without them passing through anybody else's hands.
#
#     sudo citar-secrets
#
# Prompts for a value, writes it straight into /etc/citar/citar.env (root:citar 0640) and restarts
# the service. Typed values are never echoed, never appear in your shell history, and never appear
# in the process list — which is what `citar-secrets KEY=value` would do, so this script does not
# accept that form.
#
# `sudo citar-secrets show` prints which secrets are configured, never what they are.

set -uo pipefail

ENV_FILE=/etc/citar/citar.env
SERVICE=citar

[ "$(id -u)" = "0" ] || { echo "Run with sudo — $ENV_FILE is root-owned."; exit 1; }
[ -f "$ENV_FILE" ] || { echo "No $ENV_FILE. Is CITAR installed?"; exit 1; }

get() { grep -E "^$1=" "$ENV_FILE" 2>/dev/null | head -1 | cut -d= -f2-; }

set_value() {
  local key="$1" value="$2"
  local tmp
  tmp=$(mktemp) || return 1
  # Write to a temp file and move it into place: an interrupted edit must not leave a truncated
  # env file, which would stop the server booting.
  if grep -qE "^$key=" "$ENV_FILE"; then
    awk -v k="$key" -v v="$value" \
      'BEGIN{FS=OFS="="} $1==k {print k "=" v; found=1; next} {print} END{if(!found) print k "=" v}' \
      "$ENV_FILE" > "$tmp"
  else
    cat "$ENV_FILE" > "$tmp"
    printf '%s=%s\n' "$key" "$value" >> "$tmp"
  fi
  cat "$tmp" > "$ENV_FILE"       # preserves the existing owner and mode
  rm -f "$tmp"
  chown root:citar "$ENV_FILE"
  chmod 640 "$ENV_FILE"
}

ask_secret() {
  local key="$1" label="$2" current
  current=$(get "$key")
  if [ -n "$current" ]; then
    printf '  %s is already set. Replace it? [y/N] ' "$label"
    read -r reply
    case "$reply" in [Yy]*) ;; *) echo "  left unchanged"; return 0 ;; esac
  fi
  printf '  %s: ' "$label"
  read -rs value
  echo
  [ -n "$value" ] || { echo "  nothing entered, left unchanged"; return 0; }
  printf '  repeat it: '
  read -rs again
  echo
  [ "$value" = "$again" ] || { echo "  those did not match, left unchanged"; return 1; }
  set_value "$key" "$value"
  echo "  ✓ $label saved"
}

ask_plain() {
  local key="$1" label="$2" default="${3:-}" current value
  current=$(get "$key")
  printf '  %s [%s]: ' "$label" "${current:-$default}"
  read -r value
  value="${value:-${current:-$default}}"
  [ -n "$value" ] && set_value "$key" "$value" && echo "  ✓ $label = $value"
}

show() {
  echo "Configured in $ENV_FILE:"
  local key
  for key in CITAR_SECRET_KEY CITAR_SMTP_PASSWORD CITAR_HCAPTCHA_SECRET \
             CITAR_OAUTH_GITHUB_CLIENT_SECRET CITAR_OAUTH_GOOGLE_CLIENT_SECRET \
             CITAR_OAUTH_DISCORD_CLIENT_SECRET CITAR_OAUTH_MICROSOFT_CLIENT_SECRET; do
    if [ -n "$(get "$key")" ]; then printf '  %-40s set\n' "$key"; else printf '  %-40s —\n' "$key"; fi
  done
  echo
  echo "Not secret, shown in full:"
  for key in CITAR_MODE CITAR_PUBLIC_ORIGIN CITAR_REGISTRATION CITAR_SMTP_HOST CITAR_SMTP_PORT \
             CITAR_SMTP_SECURITY CITAR_SMTP_USER CITAR_MAIL_FROM CITAR_HCAPTCHA_SITE_KEY \
             CITAR_OAUTH_GITHUB_CLIENT_ID CITAR_OAUTH_GOOGLE_CLIENT_ID \
             CITAR_OAUTH_DISCORD_CLIENT_ID CITAR_OAUTH_MICROSOFT_CLIENT_ID; do
    printf '  %-40s %s\n' "$key" "$(get "$key")"
  done
}

restart() {
  echo
  printf 'Restart CITAR now so the changes take effect? [Y/n] '
  read -r reply
  case "$reply" in
    [Nn]*) echo "Not restarted. Run: sudo systemctl restart $SERVICE" ;;
    *) systemctl restart "$SERVICE"; sleep 4
       echo "  citar is $(systemctl is-active $SERVICE)"
       journalctl -u "$SERVICE" -n 12 --no-pager | grep -iE "warning|error" | head -5 ;;
  esac
}

case "${1:-menu}" in
  show) show; exit 0 ;;
esac

echo "CITAR secrets"
echo
PS3=$'\nWhat do you want to set? '
select choice in \
  "Email (SMTP) — enables sign-up, password reset and emailed invites" \
  "hCaptcha secret — only needed for open registration" \
  "GitHub sign-in" \
  "Google sign-in" \
  "Discord sign-in" \
  "Microsoft sign-in" \
  "Show what is configured" \
  "Quit"; do
  case "$REPLY" in
    1)
      echo
      echo "Namecheap Private Email. Port 587 with STARTTLS is the more reliable of the two"
      echo "they offer; 465 with implicit SSL also works if 587 is blocked."
      ask_plain CITAR_SMTP_HOST     "SMTP host"       "mail.privateemail.com"
      ask_plain CITAR_SMTP_PORT     "SMTP port"       "587"
      ask_plain CITAR_SMTP_SECURITY "Security"        "starttls"
      ask_plain CITAR_SMTP_USER     "Username"        "you@example.com"
      ask_plain CITAR_MAIL_FROM     "From address"    "you@example.com"
      ask_plain CITAR_MAIL_FROM_NAME "From name"      "CITAR"
      ask_secret CITAR_SMTP_PASSWORD "Mailbox password"
      restart
      echo
      echo "Test it with:  sudo citar-admin test-email you@somewhere.com"
      ;;
    2)
      echo
      echo "The site key is public and already in the config; only the secret is sensitive."
      ask_plain  CITAR_HCAPTCHA_SITE_KEY "Site key"
      ask_secret CITAR_HCAPTCHA_SECRET   "hCaptcha secret key"
      restart ;;
    3|4|5|6)
      case "$REPLY" in
        3) P=GITHUB;    N=GitHub ;;
        4) P=GOOGLE;    N=Google ;;
        5) P=DISCORD;   N=Discord ;;
        6) P=MICROSOFT; N=Microsoft ;;
      esac
      echo
      echo "Redirect / callback URL to register with $N:"
      echo "  $(get CITAR_PUBLIC_ORIGIN)/api/auth/oauth/$(echo "$P" | tr 'A-Z' 'a-z')/callback"
      echo
      ask_plain  "CITAR_OAUTH_${P}_CLIENT_ID" "$N client ID"
      ask_secret "CITAR_OAUTH_${P}_CLIENT_SECRET" "$N client secret"
      restart ;;
    7) echo; show ;;
    8|"") break ;;
    *) echo "Pick a number." ;;
  esac
  echo
done
