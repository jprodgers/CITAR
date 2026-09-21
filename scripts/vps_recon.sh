#!/usr/bin/env bash
# CITAR VPS reconnaissance.
#
# Run this on the VPS and send back the output. It answers every question I have about the machine
# before I touch it: what it is, what is already running on it, what is listening on which port, and
# which of the things CITAR needs are already there.
#
#     curl -fsSL -o vps_recon.sh <however you get this file>   # or just paste it into a file
#     bash vps_recon.sh > citar-recon.txt 2>&1
#     # then send me citar-recon.txt
#
# It is READ-ONLY. It installs nothing, changes nothing, starts and stops nothing. Every command is
# a query. Run it as a normal user; it will note the few things it could not see without sudo, and
# you can re-run with `sudo bash vps_recon.sh` if you want those filled in too.
#
# It deliberately does NOT print: private keys, passwords, certificate keys, mail credentials or
# the contents of any config file that typically holds a secret. It prints filenames and whether
# things exist, not what is in them.

set -uo pipefail

section() { printf '\n\n========== %s ==========\n' "$1"; }
have()    { command -v "$1" >/dev/null 2>&1; }
try()     { if have "${1%% *}"; then eval "$@" 2>&1 | head -60; else echo "(not installed: ${1%% *})"; fi; }

echo "CITAR VPS recon — $(date -u '+%Y-%m-%d %H:%M:%S UTC')"
echo "run as: $(id -un)  (uid $(id -u))"
[ "$(id -u)" -ne 0 ] && echo "NOTE: not root; a few sections will say 'permission denied'. That is fine."

section "IDENTITY"
echo "hostname : $(hostname -f 2>/dev/null || hostname)"
echo "uptime   : $(uptime -p 2>/dev/null || uptime)"
try "cat /etc/os-release"
echo "kernel   : $(uname -srmo)"
have systemd-detect-virt && echo "virt     : $(systemd-detect-virt 2>/dev/null)"

section "CPU / MEMORY / DISK"
try "lscpu | grep -Ei 'model name|^cpu\(s\)|core|thread|mhz|vendor'"
echo "--- memory ---"
free -h 2>/dev/null || vmstat -s | head -5
echo "--- swap ---"
swapon --show 2>/dev/null || echo "(none)"
echo "--- disk ---"
df -hT -x tmpfs -x devtmpfs 2>/dev/null
echo "--- block devices ---"
try "lsblk -o NAME,SIZE,TYPE,MOUNTPOINT,ROTA"
echo "--- load ---"
cat /proc/loadavg

section "NETWORK"
echo "--- public IPv4 ---"
curl -fsS --max-time 8 https://api.ipify.org 2>/dev/null || echo "(could not determine)"
echo
echo "--- public IPv6 ---"
curl -fsS --max-time 8 https://api6.ipify.org 2>/dev/null || echo "(no IPv6 egress, or none configured)"
echo
echo "--- interfaces ---"
try "ip -brief addr"
echo "--- does this host have a routable IPv6 address? ---"
ip -6 addr show scope global 2>/dev/null | grep -q inet6 && echo "YES — an AAAA record is possible" || echo "NO — IPv4 only, skip the AAAA record"
echo "--- reverse DNS of the public IP (matters for mail) ---"
PUB4=$(curl -fsS --max-time 8 https://api.ipify.org 2>/dev/null)
if [ -n "${PUB4:-}" ]; then
  if have dig; then dig +short -x "$PUB4" 2>/dev/null || true
  elif have host; then host "$PUB4" 2>/dev/null || true
  else echo "(install dnsutils to check)"; fi
fi

section "WHAT IS LISTENING"
echo "--- listening sockets (this is the important one) ---"
if have ss; then ss -tulpn 2>/dev/null || ss -tuln
elif have netstat; then netstat -tulpn 2>/dev/null || netstat -tuln
else echo "(neither ss nor netstat)"; fi

section "WEB SERVER"
for s in nginx apache2 httpd caddy traefik lighttpd; do
  if have $s || systemctl list-unit-files 2>/dev/null | grep -q "^$s"; then
    echo "--- $s ---"
    systemctl is-active $s 2>/dev/null || echo "(not managed by systemd)"
    case $s in
      nginx)   nginx -v 2>&1; echo "sites:"; ls -1 /etc/nginx/sites-enabled/ 2>/dev/null; \
               ls -1 /etc/nginx/conf.d/ 2>/dev/null ;;
      apache2|httpd) $s -v 2>&1 | head -2; echo "sites:"; ls -1 /etc/apache2/sites-enabled/ 2>/dev/null ;;
      caddy)   caddy version 2>&1 ;;
    esac
  fi
done
echo "--- server_name / ServerName entries (which domains are already served) ---"
grep -rhoE 'server_name[[:space:]]+[^;]+;' /etc/nginx/ 2>/dev/null | sort -u | head -30
grep -rhoE 'ServerName[[:space:]]+[^[:space:]]+' /etc/apache2/ 2>/dev/null | sort -u | head -30

section "TLS CERTIFICATES"
echo "--- certbot / lets encrypt ---"
have certbot && certbot certificates 2>/dev/null | grep -E 'Certificate Name|Domains|Expiry' || echo "(certbot not installed or no certs)"
ls -1 /etc/letsencrypt/live/ 2>/dev/null || echo "(no /etc/letsencrypt/live)"

section "DATABASES ALREADY PRESENT"
for s in mysql mariadb postgresql redis-server mongod; do
  systemctl is-active $s >/dev/null 2>&1 && echo "$s: ACTIVE"
done
have mysql      && mysql --version
have psql       && psql --version
have redis-cli  && redis-cli --version

section "RUNTIMES"
for c in python3 python3.11 python3.12 python3.13 pip3 git docker docker-compose node npm; do
  if have $c; then printf '%-16s %s\n' "$c" "$($c --version 2>&1 | head -1)"; else printf '%-16s %s\n' "$c" "NOT INSTALLED"; fi
done
have docker && { echo "--- docker containers ---"; docker ps --format '{{.Names}}\t{{.Image}}\t{{.Ports}}' 2>/dev/null || echo "(need sudo/docker group)"; }

section "THE FORUM AND THE WEBSITE"
echo "Looking for what serves example.com and forum.example.com."
echo "--- web roots referenced in configs ---"
grep -rhoE '(root|DocumentRoot)[[:space:]]+[^;[:space:]]+' /etc/nginx/ /etc/apache2/ 2>/dev/null | sort -u | head -20
echo "--- proxy_pass targets (apps behind the web server) ---"
grep -rhoE 'proxy_pass[[:space:]]+[^;]+;' /etc/nginx/ 2>/dev/null | sort -u | head -20
echo "--- php ---"
have php && php --version | head -1 || echo "(no php)"
systemctl list-units --type=service --state=running 2>/dev/null | grep -Ei 'php|fpm|discourse|flarum|nodebb|phpbb' | head

section "MAIL — is self-hosting even possible here?"
echo "--- is outbound port 25 blocked? (most VPS providers block it by default) ---"
if have nc; then
  timeout 8 nc -zv gmail-smtp-in.l.google.com 25 2>&1 || echo "PORT 25 OUTBOUND APPEARS BLOCKED"
elif have timeout && have bash; then
  timeout 8 bash -c 'cat < /dev/null > /dev/tcp/gmail-smtp-in.l.google.com/25' 2>&1 \
    && echo "port 25 outbound OPEN" || echo "PORT 25 OUTBOUND APPEARS BLOCKED"
else echo "(install netcat to test)"; fi
echo "--- is anything already doing mail? ---"
for s in postfix dovecot exim4 opendkim mailcow docker-mailserver; do
  systemctl is-active $s >/dev/null 2>&1 && echo "$s: ACTIVE"
done
have postconf && postconf -n 2>/dev/null | grep -E 'myhostname|relayhost|mydomain' || true
echo "--- existing DNS records for the domain (run from anywhere, shown here for convenience) ---"
if have dig; then
  for r in A AAAA MX TXT; do echo "example.com $r: $(dig +short $r example.com | tr '\n' ' ')"; done
  echo "forum A: $(dig +short A forum.example.com | tr '\n' ' ')"
  echo "citar A: $(dig +short A citar.example.com | tr '\n' ' ')"
  echo "SPF    : $(dig +short TXT example.com | grep -i spf | tr '\n' ' ')"
  echo "DMARC  : $(dig +short TXT _dmarc.example.com | tr '\n' ' ')"
else echo "(install dnsutils: sudo apt install -y dnsutils)"; fi

section "FIREWALL"
have ufw && (ufw status verbose 2>&1 | head -25 || echo "(need sudo)")
have firewall-cmd && firewall-cmd --list-all 2>&1 | head -20
if have iptables; then echo "--- iptables INPUT ---"; iptables -L INPUT -n 2>&1 | head -20; fi
have nft && nft list ruleset 2>/dev/null | head -20

section "SSH CONFIGURATION"
echo "--- effective settings that matter for adding a deploy user ---"
grep -EiH '^[[:space:]]*(Port|PermitRootLogin|PasswordAuthentication|PubkeyAuthentication|AllowUsers|AllowGroups|AuthorizedKeysFile)' \
  /etc/ssh/sshd_config /etc/ssh/sshd_config.d/*.conf 2>/dev/null || echo "(need sudo to read sshd_config)"

section "EXISTING USERS AND SUDO"
echo "--- human users (uid >= 1000) ---"
awk -F: '$3>=1000 && $3<65534 {print $1"  uid="$3"  shell="$7}' /etc/passwd
echo "--- sudo group members ---"
getent group sudo wheel 2>/dev/null

section "AUTOMATIC UPDATES / SECURITY"
systemctl is-active unattended-upgrades >/dev/null 2>&1 && echo "unattended-upgrades: ACTIVE" || echo "unattended-upgrades: not active"
systemctl is-active fail2ban >/dev/null 2>&1 && echo "fail2ban: ACTIVE" || echo "fail2ban: not active"

section "DONE"
echo "Send this whole file back. Nothing in it is secret, but read it over first if you like —"
echo "it lists hostnames, IPs and which services you run, and no passwords or keys."
