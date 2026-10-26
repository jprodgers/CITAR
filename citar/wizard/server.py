"""Set CITAR up as a public server.

The difference between this and the local flow is not the number of questions — it is that a wrong
answer here produces a site that looks like it works and does not. A sign-in link built from the
wrong origin bounces people to a page that refuses them; a proxy-hop count that is one too high lets
anyone forge their IP past the rate limiter; a missing secret key logs everyone out on restart. So
this flow states what each answer is *for*, checks what it can, and refuses to write a file that
would start a server nobody can sign in to.

What it produces
----------------
An environment file (``citar.env``), and optionally the system integration that runs CITAR from it:

* a **systemd unit** with the memory limit, the writable state directory and the sandboxing already
  set, so that CITAR is the process the kernel kills under pressure rather than whatever else is on
  the box; with it the admin wrapper (``citar-admin``, which runs each command as the service runs)
  and the nightly backup. They are ``deploy/``'s files, written with this server's paths
  (:mod:`citar.wizard.units`), so a server installed this way runs exactly what citar-deploy installs;
* an **nginx site** that proxies to CITAR and passes the WebSocket upgrade through, written to sit
  alongside existing sites rather than replacing them;
* a **TLS certificate** through certbot, using the nginx plugin so renewal keeps working.

Each of those is optional and each is reported before it happens. Running as a non-root user, or on
a platform without systemd, simply skips them and prints the file to install by hand.

Nothing here runs at import: everything reads its state from the environment at call time, so the
wizard can be run against a machine it is not itself deployed on.
"""
from __future__ import annotations

import os
import re
import secrets
import shutil
import stat
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

from . import prompts, units

DOMAIN_RE = re.compile(r"^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$", re.I)

#: Where an environment file goes on a system-wide install, and who may read it.
SYSTEM_ENV_PATH = Path("/etc/citar/citar.env")
SYSTEM_STATE_DIR = Path("/var/lib/citar")
SERVICE_USER = "citar"


@dataclass
class ServerPlan:
    """Everything the server flow decided, before a byte is written."""

    domain: str = ""
    public_origin: str = ""
    host: str = "127.0.0.1"
    port: int = 8765
    behind_proxy: bool = True
    proxy_hops: int = 1
    secret_key: str = ""
    db_url: str = ""
    state_dir: Path = SYSTEM_STATE_DIR
    registration: str = "invite"
    env_path: Path = SYSTEM_ENV_PATH
    smtp: dict = field(default_factory=dict)
    oauth: dict = field(default_factory=dict)
    hcaptcha: dict = field(default_factory=dict)
    admin: dict = field(default_factory=dict)
    install_systemd: bool = False
    install_nginx: bool = False
    run_certbot: bool = False

    def summary(self) -> list[str]:
        """Everything this plan will do, as the lines shown before anything is written."""
        lines = [
            f"Serve {self.public_origin} from {self.host}:{self.port}",
            f"Write {self.env_path} (mode 0600 - it holds the secret key and any passwords)",
            f"Keep state in {self.state_dir}",
            f"Registration: {self.registration}",
        ]
        lines.append("E-mail: " + (f"via {self.smtp['host']}:{self.smtp['port']}" if self.smtp else
                                   "not configured (no sign-up, verification or password reset)"))
        lines.append("Single sign-on: " + (", ".join(sorted(self.oauth)) if self.oauth else "none"))
        if self.hcaptcha:
            lines.append("hCaptcha: configured")
        if self.admin:
            lines.append(f"Create the first administrator: {self.admin['handle']}")
        if self.install_systemd:
            lines.append("Install and enable the citar systemd service, citar-admin, and the nightly backup "
                         "(citar-backup.timer)")
        if self.install_nginx:
            lines.append(f"Add an nginx site for {self.domain} (existing sites untouched)")
        if self.run_certbot:
            lines.append(f"Ask certbot for a certificate for {self.domain}")
        return lines


def run(args) -> int:
    """Ask the server-setup questions, then apply the answers."""
    if getattr(args, "refresh_units", False):
        return refresh_units()
    plan = ServerPlan()
    _ask_identity(plan, args)
    _ask_storage(plan, args)
    _ask_signin(plan, args)
    _ask_system_integration(plan, args)

    # Checked again here, not only at the prompt: an unattended run takes --state-dir unasked.
    problem = state_dir_problem(plan.state_dir) if _is_root() else None
    if problem:
        prompts.note(problem)
        prompts.say("Nothing was changed.")
        return 1
    if not prompts.confirm_plan("CITAR will:", plan.summary()):
        prompts.say("Nothing was changed.")
        return 1

    _write_env(plan)
    if plan.install_systemd:
        _install_systemd(plan)
    if plan.install_nginx:
        _install_nginx(plan)
    if plan.run_certbot:
        _run_certbot(plan)
    if plan.admin:
        _create_admin(plan)
    _finish(plan)
    return 0


# --------------------------------------------------------------------------- questions

def _ask_identity(plan: ServerPlan, args) -> None:
    """Ask for the domain, the bind address and the proxy arrangement."""
    prompts.heading("What is this server called?")
    prompts.note("The domain browsers will use. Sign-in links, OAuth callbacks and e-mails are all")
    prompts.note("built from it, so it has to match exactly - including whether it has 'www'.")

    def check(value: str) -> Optional[str]:
        """Validate a domain, rejecting anything that is not one."""
        host = value.replace("https://", "").replace("http://", "").strip("/")
        if not DOMAIN_RE.match(host):
            return "That does not look like a domain name. Example: citar.example.com"
        return None

    domain = prompts.ask("Domain", args.domain or "", flag="--domain", validate=check)
    plan.domain = domain.replace("https://", "").replace("http://", "").strip("/")
    plan.public_origin = f"https://{plan.domain}"

    prompts.say("")
    prompts.note("CITAR itself listens on loopback and a reverse proxy terminates TLS in front of")
    prompts.note("it. That is how the certificate, HTTP/2 and any other sites on this machine are")
    prompts.note("handled by one piece of software that is good at it.")
    plan.host = prompts.ask("Bind address", args.host or "127.0.0.1")
    plan.port = prompts.ask_port("Port", args.port or 8765)

    plan.behind_proxy = prompts.ask_yes_no("Is a reverse proxy in front of CITAR?", True)
    if plan.behind_proxy:
        prompts.note("How many proxies add an X-Forwarded-For entry. One nginx in front = 1.")
        prompts.note("Too high and a client can forge its own IP past the rate limiter; too low and")
        prompts.note("every request looks like it came from the proxy.")
        plan.proxy_hops = int(prompts.ask("Proxy hops", str(args.proxy_hops or 1)))

    plan.secret_key = secrets.token_urlsafe(48)


def _ask_storage(plan: ServerPlan, args) -> None:
    """Ask where state goes, which database, and where the environment file lives."""
    prompts.heading("Where does the data go?")
    default_state = Path(args.state_dir) if args.state_dir else (
        SYSTEM_STATE_DIR if _is_root() else Path.home() / ".local" / "share" / "citar")
    # A system install writes these paths into the unit and the scripts, which read a space, a quote, `$` or `%`
    # differently from a shell's or the user's idea of them; and it gives the state directory to the service account.
    check_path = units.path_problem if _is_root() else None
    check_state = state_dir_problem if _is_root() else None
    plan.state_dir = Path(prompts.ask("State directory", str(default_state), validate=check_state))
    prompts.note("Saved games, the database, benchmark runs and reports. Back this up.")

    choice = prompts.ask_choice(
        "Which database?",
        [("sqlite", "SQLite (recommended)",
          "One file in the state directory. Fine for this workload - the game engine is the\n"
          "bottleneck long before the database is."),
         ("postgres", "PostgreSQL",
          "For an existing Postgres, or if you expect to outgrow one machine.\n"
          "The schema is written to the intersection of both, so either works.")],
        default="sqlite")
    if choice == "postgres":
        plan.db_url = prompts.ask("Connection URL",
                                  "postgresql+psycopg://citar:PASSWORD@127.0.0.1:5432/citar")
    else:
        plan.db_url = ""                                        # settings derives it from the state dir

    default_env = SYSTEM_ENV_PATH if _is_root() else plan.state_dir / "citar.env"
    plan.env_path = Path(prompts.ask("Environment file", str(args.env_path or default_env), validate=check_path))


def _ask_signin(plan: ServerPlan, args) -> None:
    """Ask how people sign in: registration mode, e-mail, providers, captcha, first administrator."""
    prompts.heading("Who may sign in?")
    plan.registration = prompts.ask_choice(
        "Registration",
        [("invite", "Invitation only (recommended to start)",
          "Nobody signs up unprompted. You mint codes with `citar admin invite`.\n"
          "This needs no e-mail and no captcha, so a server can go live before either exists."),
         ("open", "Open registration",
          "Anyone can create an account. Wants e-mail verification and a captcha, or it will\n"
          "collect junk accounts."),
         ("closed", "Closed",
          "No new accounts at all. For a server whose users already exist.")],
        default="invite")

    if prompts.ask_yes_no("\nConfigure e-mail (sign-up, verification, password reset)?", False):
        plan.smtp = {
            "host": prompts.ask("SMTP host", "smtp.example.com"),
            "port": prompts.ask("SMTP port", "587"),
            "security": prompts.ask_choice(
                "Connection security",
                [("starttls", "STARTTLS (port 587)", "The usual choice."),
                 ("ssl", "Implicit TLS (port 465)", "Older, still common."),
                 ("none", "None", "Only for a relay on localhost.")],
                default="starttls"),
            "user": prompts.ask("Username", ""),
            "password": prompts.ask_secret("Password", allow_empty=True),
            "from": prompts.ask("From address", f"citar@{plan.domain}"),
            "from_name": prompts.ask("From name", "CITAR"),
        }
    else:
        prompts.note("Skipped. Invitations still work; password reset does not.")

    prompts.say("")
    prompts.note("Single sign-on needs an app registered with each provider, with the callback")
    prompts.note(f"   {plan.public_origin}/api/auth/oauth/<provider>/callback")
    prompts.note("You can add these later; docs/server/OAUTH.md has the steps for each provider.")
    if prompts.ask_yes_no("Configure a sign-in provider now?", False):
        for provider in ("google", "github", "discord", "microsoft"):
            if not prompts.ask_yes_no(f"  {provider.capitalize()}?", False):
                continue
            entry = {
                "client_id": prompts.ask(f"  {provider} client id", ""),
                "client_secret": prompts.ask_secret(f"  {provider} client secret", allow_empty=True),
            }
            if provider == "microsoft":
                entry["tenant"] = prompts.ask("  Microsoft tenant", "common")
            if entry["client_id"]:
                plan.oauth[provider] = entry

    if plan.registration == "open" and prompts.ask_yes_no("\nConfigure hCaptcha?", True):
        plan.hcaptcha = {
            "site_key": prompts.ask("hCaptcha site key", ""),
            "secret": prompts.ask_secret("hCaptcha secret", allow_empty=True),
        }

    prompts.say("")
    if prompts.ask_yes_no("Create the first administrator account now?", True):
        plan.admin = {
            "handle": prompts.ask("Handle", "operator"),
            "email": prompts.ask("E-mail", f"admin@{plan.domain}"),
            "password": prompts.ask_secret("Password", allow_empty=True),
        }


def _ask_system_integration(plan: ServerPlan, args) -> None:
    """Ask whether to install the service, the nginx site and the certificate."""
    if sys.platform == "win32":
        return
    prompts.heading("System integration")
    if not _is_root():
        prompts.note("Not running as root, so the service, the nginx site and the certificate are")
        prompts.note("skipped. Re-run with sudo, or install the files printed at the end by hand.")
        return

    plan.install_systemd = _has("systemctl") and prompts.ask_yes_no(
        "Install and enable a systemd service?", True)
    if _has("nginx"):
        plan.install_nginx = prompts.ask_yes_no(
            f"Add an nginx site for {plan.domain}? (existing sites are left alone)", True)
        if plan.install_nginx and _has("certbot"):
            plan.run_certbot = prompts.ask_yes_no(
                "Ask certbot for a TLS certificate afterwards?", True)
    else:
        prompts.note("nginx is not installed; put your own reverse proxy in front of "
                     f"{plan.host}:{plan.port}.")


# --------------------------------------------------------------------------- apply

def _write_env(plan: ServerPlan) -> None:
    """Write the environment file with mode 0600, before anything reads it."""
    lines = [
        "# CITAR server configuration, written by `citar setup`.",
        "# Everything here is read once at startup. Restart CITAR after changing it.",
        "",
        "CITAR_MODE=server",
        f"CITAR_PUBLIC_ORIGIN={plan.public_origin}",
        f"CITAR_SECRET_KEY={plan.secret_key}",
        f"CITAR_HOST={plan.host}",
        f"CITAR_PORT={plan.port}",
        f"CITAR_BEHIND_PROXY={1 if plan.behind_proxy else 0}",
        f"CITAR_TRUSTED_PROXY_HOPS={plan.proxy_hops}",
        "",
        f"CITAR_STATE_DIR={plan.state_dir}",
        f"CITAR_DATA_DIR={plan.state_dir}",
    ]
    if plan.db_url:
        lines.append(f"CITAR_DB_URL={plan.db_url}")
    lines += ["", f"CITAR_REGISTRATION={plan.registration}"]

    if plan.smtp:
        lines += [
            "",
            f"CITAR_SMTP_HOST={plan.smtp['host']}",
            f"CITAR_SMTP_PORT={plan.smtp['port']}",
            f"CITAR_SMTP_SECURITY={plan.smtp['security']}",
            f"CITAR_SMTP_USER={plan.smtp['user']}",
            f"CITAR_SMTP_PASSWORD={plan.smtp['password']}",
            f"CITAR_MAIL_FROM={plan.smtp['from']}",
            f"CITAR_MAIL_FROM_NAME={plan.smtp['from_name']}",
        ]
    for provider, entry in plan.oauth.items():
        key = provider.upper()
        lines += ["", f"CITAR_OAUTH_{key}_CLIENT_ID={entry['client_id']}",
                  f"CITAR_OAUTH_{key}_CLIENT_SECRET={entry['client_secret']}"]
        if provider == "microsoft":
            lines.append(f"CITAR_OAUTH_MICROSOFT_TENANT={entry.get('tenant', 'common')}")
    if plan.hcaptcha:
        lines += ["", f"CITAR_HCAPTCHA_SITE_KEY={plan.hcaptcha['site_key']}",
                  f"CITAR_HCAPTCHA_SECRET={plan.hcaptcha['secret']}"]
    lines.append("")

    plan.env_path.parent.mkdir(parents=True, exist_ok=True)
    # Created with the restrictive mode from the start: writing it world-readable and chmod-ing
    # afterwards leaves a window in which the secret key is readable by every user on the box.
    descriptor = os.open(plan.env_path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as handle:
        handle.write("\n".join(lines))
    prompts.note(f"Wrote {plan.env_path} (0600)")

    plan.state_dir.mkdir(parents=True, exist_ok=True)
    if _is_root() and _user_exists(SERVICE_USER):
        shutil.chown(plan.state_dir, SERVICE_USER, SERVICE_USER)
        os.chmod(plan.env_path, 0o640)
        shutil.chown(plan.env_path, "root", SERVICE_USER)


def _install_systemd(plan: ServerPlan) -> None:
    """Write citar.service, citar-admin and the backup from deploy/'s files, then enable and start them."""
    if not _user_exists(SERVICE_USER):
        subprocess.run(["useradd", "--system", "--home", str(plan.state_dir),
                        "--shell", "/usr/sbin/nologin", SERVICE_USER], check=False)
    plan.state_dir.mkdir(parents=True, exist_ok=True)
    shutil.chown(plan.state_dir, SERVICE_USER, SERVICE_USER)
    os.chmod(plan.state_dir, 0o750)

    install = units.this_install(plan.env_path, plan.state_dir, plan.public_origin)
    try:
        files = units.rendered(install)
    except ValueError as exc:
        prompts.note(f"Not installing the service: {exc}.")
        prompts.note("Choose a state directory and an environment file without such characters, and run this again.")
        return
    _write_units(files)
    subprocess.run(["systemctl", "daemon-reload"], check=False)
    subprocess.run(["systemctl", "enable", "--now", "citar"], check=False)
    subprocess.run(["systemctl", "enable", "--now", "citar-backup.timer"], check=False)
    prompts.note("Service enabled and started; the nightly backup is on (citar-backup.timer).")


def _write_units(files: dict[str, Optional[str]]) -> None:
    """Install :func:`units.rendered`'s files where :data:`units.TARGETS` puts them, each only when it differs."""
    for name, text in files.items():
        target = Path(units.TARGETS[name])
        if text is None:
            prompts.note(f"Not writing {target}: this copy of CITAR has no citar-admin command to run.")
            continue
        try:
            same = not target.is_symlink() and target.read_text(encoding="utf-8") == text
        except OSError:
            same = False
        if same:
            prompts.note(f"{target} is up to date")
            continue
        _install_file(target, text, 0o755 if name.endswith(".sh") else 0o644)
        prompts.note(f"Wrote {target}")


def refresh_units() -> int:
    """``citar setup --server --refresh-units``: write the unit, citar-admin and the backup again from this release,
    for a server whose unit is installed, keeping the paths that unit names (its environment file, its state
    directory and its documentation link). It asks nothing and starts nothing: install.sh's switch, which runs it
    from the release it has just made current, reloads systemd and starts the service itself. Returns 0 when the
    files are in place, 2 when there is no installed unit to take the paths from."""
    unit = Path(units.TARGETS["citar.service"])
    if not _is_root():
        prompts.note(f"--refresh-units writes {unit} and the scripts beside it: run it as root.")
        return 2
    try:
        installed = unit.read_text(encoding="utf-8")
    except OSError as exc:
        prompts.note(f"There is no unit to write again ({exc}): `citar setup --server` installs the service.")
        return 2
    service, about = dict(units.directives(installed, "Service")), dict(units.directives(installed, "Unit"))
    env_file, state_dir = service.get("EnvironmentFile", "").lstrip("-"), service.get("WorkingDirectory", "")
    if not env_file or not state_dir:
        prompts.note(f"{unit} names no EnvironmentFile= or WorkingDirectory=: it is not one `citar setup` wrote, so "
                     "it is left as it is.")
        return 2
    install = units.this_install(Path(env_file), Path(state_dir), about.get("Documentation") or units.DOCUMENTATION)
    try:
        files = units.rendered(install)
    except ValueError as exc:
        prompts.note(f"Not writing the unit again: {exc}.")
        return 2
    _write_units(files)
    return 0


def _install_file(path: Path, text: str, mode: int) -> None:
    """Write ``path`` whole and then rename it into place, so a crash leaves the old file, never half of one,
    and a symbolic link already there (0.1.5's install linked citar-admin to a venv's own command) is
    replaced rather than written through."""
    path.parent.mkdir(parents=True, exist_ok=True)
    staged = path.with_name(f".{path.name}.citar-setup")
    staged.write_text(text, encoding="utf-8", newline="\n")
    os.chmod(staged, mode)
    os.replace(staged, path)


def _install_nginx(plan: ServerPlan) -> None:
    """Add a site for this domain without touching the ones already there."""
    config = f"""# CITAR - {plan.domain}
# Added by `citar setup`. Other sites on this server are not affected.
server {{
    listen 80;
    listen [::]:80;
    server_name {plan.domain};

    # certbot replaces this block with a TLS one and a redirect.
    location /.well-known/acme-challenge/ {{ root /var/www/html; }}

    location / {{
        proxy_pass http://{plan.host}:{plan.port};
        proxy_http_version 1.1;

        # The WebSocket upgrade carries live game state, benchmark progress and the worker
        # connection. Without these two headers the site loads and then silently stops updating.
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection $connection_upgrade;

        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        # A model turn can take minutes. The default 60s read timeout cuts those off mid-turn.
        proxy_read_timeout 1800s;
        proxy_send_timeout 1800s;
        proxy_buffering off;
    }}

    client_max_body_size 32m;
}}
"""
    available = Path("/etc/nginx/sites-available")
    enabled = Path("/etc/nginx/sites-enabled")
    if available.is_dir():
        site = available / "citar"
        site.write_text(config, encoding="utf-8", newline="\n")
        link = enabled / "citar"
        if not link.exists():
            link.symlink_to(site)
    else:                                                       # RHEL-style layout
        site = Path("/etc/nginx/conf.d/citar.conf")
        site.write_text(config, encoding="utf-8", newline="\n")
    prompts.note(f"Wrote {site}")

    _ensure_connection_upgrade_map()
    if subprocess.run(["nginx", "-t"], capture_output=True).returncode == 0:
        subprocess.run(["systemctl", "reload", "nginx"], check=False)
        prompts.note("nginx reloaded.")
    else:
        prompts.note("nginx rejected the configuration; it was left in place but not loaded.")
        prompts.note("Run `nginx -t` to see why.")


def _ensure_connection_upgrade_map() -> None:
    """Define ``$connection_upgrade`` once, at http level, if the machine has no such map.

    Without it nginx fails to start with "unknown variable", which is a confusing way to learn that
    a WebSocket proxy needs a map. It is written as its own conf.d file so it cannot clash with an
    existing definition somewhere else.
    """
    conf_d = Path("/etc/nginx/conf.d")
    if not conf_d.is_dir():
        return
    existing = subprocess.run(["grep", "-rl", "connection_upgrade", "/etc/nginx"],
                              capture_output=True, text=True).stdout
    if any(line and "citar" not in line for line in existing.splitlines()):
        return
    (conf_d / "citar-websocket-map.conf").write_text(
        "# Required by CITAR's WebSocket proxying.\n"
        "map $http_upgrade $connection_upgrade {\n"
        "    default upgrade;\n"
        "    ''      close;\n"
        "}\n", encoding="utf-8", newline="\n")


def _run_certbot(plan: ServerPlan) -> None:
    """Ask certbot for a certificate, using the nginx plugin so renewal keeps working."""
    email = plan.admin.get("email") or plan.smtp.get("from") or ""
    command = ["certbot", "--nginx", "-d", plan.domain, "--redirect", "--agree-tos",
               "--non-interactive"]
    command += ["-m", email] if email else ["--register-unsafely-without-email"]
    prompts.note("Running certbot...")
    result = subprocess.run(command, capture_output=True, text=True)
    if result.returncode == 0:
        prompts.note(f"Certificate installed for {plan.domain}.")
    else:
        prompts.note("certbot failed. The site still works over plain HTTP; fix DNS and re-run:")
        prompts.note(f"  certbot --nginx -d {plan.domain}")
        for line in (result.stderr or "").strip().splitlines()[-4:]:
            prompts.note(f"  {line}")


def _create_admin(plan: ServerPlan) -> None:
    """Create the first administrator, through the same code path the CLI uses."""
    os.environ.setdefault("CITAR_STATE_DIR", str(plan.state_dir))
    os.environ.setdefault("CITAR_DATA_DIR", str(plan.state_dir))
    give = _is_root() and _user_exists(SERVICE_USER)
    before = _paths_under(plan.state_dir) if give else set()
    try:
        prompts.note(f"Created administrator '{_bootstrap_admin(plan)}'.")
    except Exception as exc:
        prompts.note(f"Could not create the account ({type(exc).__name__}: {exc}).")
        prompts.note("Create it once the service is running:")
        prompts.note(f"  {_admin_command(plan)} create-admin --handle {plan.admin['handle']} "
                     f"--email {plan.admin['email']}")
    finally:
        # Even after a failure: a migration that ran part way may have made the database as root.
        if give:
            _give_to_service_user(plan.state_dir, before, plan.env_path)


def _bootstrap_admin(plan: ServerPlan) -> str:
    """Migrate the database and create the first administrator in it; returns the new account's handle."""
    from .. import db
    from ..auth import accounts, audit
    from ..server import boot

    boot.migrate()
    with db.session() as session:
        user = accounts.create_user(
            session, handle=plan.admin["handle"], email=plan.admin["email"],
            password=plan.admin["password"] or None, role="admin", status="active",
            email_verified=True, display_name=plan.admin["handle"])
        audit.record(session, "user.bootstrap_admin", actor=user,
                     object_type="user", object_id=user.id)
        accounts.on_first_admin(session, user)
        return user.handle


def _admin_command(plan: ServerPlan) -> str:
    """How the operator runs an admin command on this server: the wrapper, which runs it as the service runs, when
    it was installed; else the command itself (a server without systemd, where nothing else runs it)."""
    if plan.install_systemd and Path(units.TARGETS["citar-admin.sh"]).is_file():
        return "sudo citar-admin"
    return "citar admin"


#: The database files the in-process admin creation writes as root (settings' default, citar.db in the state
#: directory) are the service's whatever made them first.
_DATABASE_FILES = ("citar.db", "citar.db-wal", "citar.db-shm", "citar.db-journal")


def _paths_under(state_dir: Path) -> set[str]:
    """Every path in ``state_dir`` (itself included), without following a link."""
    found = {str(state_dir)} if os.path.lexists(state_dir) else set()
    for folder, folders, files in os.walk(state_dir):
        found.update(os.path.join(folder, name) for name in folders + files)
    return found


def _give_to_service_user(state_dir: Path, before: set[str], env_path: Path) -> None:
    """Give the service account what root's in-process admin creation made in its state directory: the entries that
    were not there ``before`` it, and the database's files. Run as root, a new database, its write-ahead log or a
    folder is root's, and the service (citar) could open the database but never write it again. Nothing that was
    there already is touched, nor the environment file (root:citar 0640, whoever's folder it is in)."""
    database = {str(state_dir / name) for name in _DATABASE_FILES}
    try:
        keep = os.path.realpath(env_path)
    except OSError:
        keep = ""
    for path in sorted(_paths_under(state_dir)):
        if (path in before and path not in database) or os.path.realpath(path) == keep:
            continue
        try:
            info = os.lstat(path)
            if info.st_uid == 0 and not stat.S_ISLNK(info.st_mode):
                shutil.chown(path, SERVICE_USER, SERVICE_USER)
        except OSError:
            pass


#: What a state directory CITAR already uses holds (citar/paths.py), any one of which marks it as CITAR's own.
_STATE_ENTRIES = frozenset({"citar.db", "saves", "config", "benchmarks", "secret_key", "citar.env"})


def _install_roots() -> list[Path]:
    """Where CITAR itself is installed: this venv, its release layout's root when it runs from one, and the layout's
    default prefix (install.sh --server's)."""
    venv = Path(sys.prefix).resolve()
    roots = [venv, Path(str(units.LAYOUT))]
    if venv.parent.parent.name == "releases":
        roots.append(venv.parent.parent.parent)
    return roots


def state_dir_problem(state_dir: Path | str) -> Optional[str]:
    """Why a root install must not take ``state_dir`` as the service's state directory, or None when it may. The
    folder is given to the service account (citar:citar 0750) and backed up whole, so it must be CITAR's alone: new,
    empty, or already holding CITAR's state; never one holding, or held in, CITAR's own install (whose venvs are
    root's and read-only to the service, and which an uninstall removes whole)."""
    problem = units.path_problem(state_dir)
    if problem:
        return problem
    path = Path(state_dir).resolve()
    for root in _install_roots():
        root = root.resolve()
        if path == root or root in path.parents or path in root.parents:
            return (f"{path} overlaps CITAR's own install at {root}: choose a folder of its own for the state "
                    "(the default is /var/lib/citar)")
    if path.exists():
        if not path.is_dir():
            return f"{path} is not a folder"
        entries = {entry.name for entry in path.iterdir()} - {"lost+found"}
        if entries and not entries & _STATE_ENTRIES:
            return (f"{path} already holds files that are not CITAR's: choose a new or empty folder (the state "
                    f"directory is given to the {SERVICE_USER} account and backed up whole)")
    return None


def _finish(plan: ServerPlan) -> None:
    """Print what was done, and the commands for what comes next."""
    prompts.heading("Done")
    prompts.note(f"Configuration: {plan.env_path}")
    prompts.note(f"State:         {plan.state_dir}")
    prompts.say("")
    if plan.install_systemd:
        prompts.note("systemctl status citar          is it running")
        prompts.note("journalctl -u citar -f          what it is doing")
        prompts.note("sudo citar-admin list-users     an admin command, run as the service runs")
    else:
        prompts.note("Start it with:")
        prompts.note(f"  env $(grep -v '^#' {plan.env_path} | xargs) citar serve")
    prompts.say("")
    prompts.note(f"Check it: curl -s -o /dev/null -w '%{{http_code}}' {plan.public_origin}/api/auth/config")
    if plan.registration == "invite":
        prompts.note(f"Invite the first player:  {_admin_command(plan)} invite --email them@example.com")
    prompts.say("")
    prompts.note("Then run `citar doctor` to confirm every part of the configuration.")


# --------------------------------------------------------------------------- helpers

def _is_root() -> bool:
    """Whether this is running as root."""
    return hasattr(os, "geteuid") and os.geteuid() == 0


def _has(program: str) -> bool:
    """Whether a program is on the PATH."""
    return shutil.which(program) is not None


def _user_exists(name: str) -> bool:
    """Whether a system account exists."""
    try:
        import pwd

        pwd.getpwnam(name)
        return True
    except (ImportError, KeyError):
        return False
