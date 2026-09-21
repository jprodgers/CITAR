#!/usr/bin/env bash
# The paste-in-one-go version of vps_recon.sh.
#
# No file to transfer, no chmod, no output redirection to lose track of. Paste the whole thing into
# an SSH session; it prints to the screen AND saves a copy at a known absolute path, which it tells
# you at the end.
#
# Still read-only: every command is a query. Nothing is installed, started, stopped or changed, and
# no password, key or secret is printed.
#
# It works as a normal user. Lines it cannot read without root say so and are skipped; that is fine
# and does not stop the rest.

exec > >(tee /tmp/citar-recon.txt) 2>&1

h() { printf '\n===== %s =====\n' "$1"; }
have() { command -v "$1" >/dev/null 2>&1; }

echo "CITAR recon — $(date -u '+%F %T UTC')"

h "WHO AM I / CAN I SUDO"
echo "user      : $(id -un)  (uid $(id -u))"
echo "groups    : $(id -Gn)"
if [ "$(id -u)" = "0" ]; then
  echo "sudo      : not needed, already root"
elif sudo -n true 2>/dev/null; then
  echo "sudo      : yes, without a password"
elif have sudo; then
  echo "sudo      : sudo exists but needs a password (or this account is not a sudoer)"
else
  echo "sudo      : SUDO IS NOT INSTALLED on this box"
fi

h "SYSTEM"
have hostnamectl && hostnamectl 2>/dev/null | head -8 || { echo "host: $(hostname -f 2>/dev/null || hostname)"; cat /etc/os-release 2>/dev/null | head -4; }
echo "kernel    : $(uname -srm)"
have systemd-detect-virt && echo "virt      : $(systemd-detect-virt 2>/dev/null)"
echo "uptime    : $(uptime -p 2>/dev/null || uptime)"

h "CPU / RAM / DISK"
grep -m1 'model name' /proc/cpuinfo 2>/dev/null | cut -d: -f2-
echo "cores     : $(nproc 2>/dev/null)"
free -h 2>/dev/null | head -3
swapon --show 2>/dev/null | head -3 || echo "swap: none"
df -hT -x tmpfs -x devtmpfs 2>/dev/null | head -8
echo "load      : $(cat /proc/loadavg)"

h "NETWORK / IP ADDRESSES"
echo "public IPv4: $(curl -fsS --max-time 8 https://api.ipify.org 2>/dev/null || echo unknown)"
echo "public IPv6: $(curl -fsS --max-time 8 https://api6.ipify.org 2>/dev/null || echo 'none')"
if ip -6 addr show scope global 2>/dev/null | grep -q inet6; then
  echo "IPv6 ON THIS HOST: YES  -> an AAAA record is worth adding"
  ip -6 addr show scope global 2>/dev/null | grep inet6 | head -3
else
  echo "IPv6 ON THIS HOST: NO   -> skip the AAAA record entirely"
fi

h "LISTENING PORTS  (what is already using 80/443)"
if have ss; then ss -tulpn 2>/dev/null || ss -tuln; else netstat -tulpn 2>/dev/null || netstat -tuln; fi

h "WEB SERVER"
for s in nginx apache2 httpd caddy traefik; do
  if have $s || systemctl list-unit-files 2>/dev/null | grep -q "^$s\."; then
    echo "-- $s: $(systemctl is-active $s 2>/dev/null || echo '?')"
  fi
done
echo "-- nginx sites:";  ls -1 /etc/nginx/sites-enabled/ /etc/nginx/conf.d/ 2>/dev/null
echo "-- apache sites:"; ls -1 /etc/apache2/sites-enabled/ 2>/dev/null
echo "-- domains already served:"
grep -rhoE 'server_name[[:space:]]+[^;]+;' /etc/nginx/ 2>/dev/null | sort -u | head -20
grep -rhoE 'ServerName[[:space:]]+[^[:space:]]+' /etc/apache2/ 2>/dev/null | sort -u | head -20
echo "-- apps behind the web server (proxy targets):"
grep -rhoE 'proxy_pass[[:space:]]+[^;]+;' /etc/nginx/ 2>/dev/null | sort -u | head -15
echo "-- web roots:"
grep -rhoE '(root|DocumentRoot)[[:space:]]+[^;[:space:]]+' /etc/nginx/ /etc/apache2/ 2>/dev/null | sort -u | head -15

h "TLS CERTIFICATES"
ls -1 /etc/letsencrypt/live/ 2>/dev/null || echo "no /etc/letsencrypt/live (or not readable without root)"

h "RUNTIMES AND SERVICES"
for c in python3 pip3 git docker node npm php mysql psql redis-cli certbot; do
  if have $c; then printf '%-10s %s\n' "$c" "$($c --version 2>&1 | head -1)"; else printf '%-10s %s\n' "$c" "-"; fi
done
echo "-- running services of interest:"
systemctl list-units --type=service --state=running 2>/dev/null \
  | grep -Ei 'nginx|apache|php|fpm|mysql|maria|postgres|redis|docker|discourse|flarum|nodebb|postfix|dovecot' | head -15
have docker && (docker ps --format '{{.Names}}  {{.Image}}  {{.Ports}}' 2>/dev/null | head -10 || echo "(docker needs sudo here)")

h "MAIL FEASIBILITY"
if have nc; then
  timeout 8 nc -z gmail-smtp-in.l.google.com 25 2>/dev/null \
    && echo "outbound port 25: OPEN" || echo "outbound port 25: BLOCKED (normal; provider must unblock)"
else
  timeout 8 bash -c 'cat </dev/null >/dev/tcp/gmail-smtp-in.l.google.com/25' 2>/dev/null \
    && echo "outbound port 25: OPEN" || echo "outbound port 25: BLOCKED (normal; provider must unblock)"
fi
PUB=$(curl -fsS --max-time 8 https://api.ipify.org 2>/dev/null)
if [ -n "$PUB" ] && have dig; then echo "reverse DNS of $PUB: $(dig +short -x "$PUB" | tr '\n' ' ')"; fi
for s in postfix dovecot exim4 opendkim; do systemctl is-active $s >/dev/null 2>&1 && echo "$s: ACTIVE"; done

h "DNS FOR THE DOMAIN"
if have dig; then
  for r in A AAAA MX; do echo "example.com $r : $(dig +short $r example.com | tr '\n' ' ')"; done
  echo "forum.example.com A : $(dig +short A forum.example.com | tr '\n' ' ')"
  echo "citar.example.com A : $(dig +short A citar.example.com | tr '\n' ' ')"
  echo "SPF   : $(dig +short TXT example.com | grep -i spf | tr '\n' ' ')"
  echo "DMARC : $(dig +short TXT _dmarc.example.com | tr '\n' ' ')"
  echo "NS    : $(dig +short NS example.com | tr '\n' ' ')"
else
  echo "(dig not installed — skip, I can check DNS from my end)"
fi

h "FIREWALL"
have ufw && (ufw status 2>/dev/null || echo "(ufw needs root)")
have firewall-cmd && firewall-cmd --list-all 2>/dev/null | head -12
have nft && (nft list ruleset 2>/dev/null | head -12 || echo "(nft needs root)")

h "SSH SETTINGS"
grep -EihH '^[[:space:]]*(Port|PermitRootLogin|PasswordAuthentication|PubkeyAuthentication|AllowUsers|AllowGroups)' \
  /etc/ssh/sshd_config /etc/ssh/sshd_config.d/*.conf 2>/dev/null || echo "(not readable without root — fine)"

h "USERS"
awk -F: '$3>=1000 && $3<65534 {print $1"  uid="$3"  shell="$7}' /etc/passwd 2>/dev/null
echo "sudo group: $(getent group sudo 2>/dev/null)"
echo "wheel group: $(getent group wheel 2>/dev/null)"

h "DONE"
echo "A copy was saved to /tmp/citar-recon.txt"
echo "To see it again:   cat /tmp/citar-recon.txt"
