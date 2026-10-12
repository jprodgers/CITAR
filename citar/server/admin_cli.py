"""Command-line administration, for the things that cannot be done through the web UI.

Chiefly: creating the first administrator of a fresh server, and getting back in when nobody can.

    python -m citar.server.admin_cli create-admin --handle jimmie --email you@example.com
    python -m citar.server.admin_cli create-user --handle sam --email sam@example.com [--role moderator]
    python -m citar.server.admin_cli list-users
    python -m citar.server.admin_cli set-role jimmie admin
    python -m citar.server.admin_cli set-status sam suspended --reason "spam"
    python -m citar.server.admin_cli set-caps sam run_lab=on publish_global=default
    python -m citar.server.admin_cli set-limits sam --games 4 --seats 8 --invites 2
    python -m citar.server.admin_cli set-trust sam trusted
    python -m citar.server.admin_cli audit --actor jimmie --action 'user.*' --limit 20
    python -m citar.server.admin_cli reset-password jimmie
    python -m citar.server.admin_cli invite --role moderator --uses 1
    python -m citar.server.admin_cli policy registration invite

Passwords are always prompted for, never taken as an argument: a password on the command line ends
up in the shell history and in the process list where every other user on the box can read it. A
script gives ``create-user`` one on its standard input instead (``--password-stdin``).
"""
from __future__ import annotations

import argparse
import getpass
import sys

from sqlalchemy import select

from .. import db, settings
from ..auth import accounts, audit, invites, passwords, policy
from ..db.models import User


def _setup():
    """Load settings and open the database, before any command runs."""
    db.configure()
    from .boot import migrate
    migrate()


def _prompt_password(confirm: bool = True) -> str:
    """Ask for a password without echoing it, twice when it is being set.

    Always prompted, never taken as an argument: a password on a command line is in the shell history
    and in the process list, where every other account on the machine can read it.
    """
    while True:
        first = getpass.getpass("Password: ")
        try:
            passwords.check(first)
        except passwords.PasswordError as exc:
            print(f"  {exc}\n")
            continue
        if not confirm:
            return first
        if first != getpass.getpass("Repeat password: "):
            print("  Those did not match.\n")
            continue
        return first


def cmd_create_admin(args) -> int:
    """Create the first administrator."""
    with db.session() as s:
        existing = list(s.scalars(select(User).where(User.role == "admin", User.status == "active")))
        if existing and not args.force:
            print(f"This server already has {len(existing)} administrator(s): "
                  f"{', '.join(u.handle for u in existing)}")
            print("Use --force to add another.")
            return 1
        password = _prompt_password()
        try:
            user = accounts.create_user(s, handle=args.handle, email=args.email, password=password,
                                        role="admin", status="active", email_verified=True,
                                        display_name=args.name or args.handle)
        except accounts.AccountError as exc:
            print(f"error: {exc}")
            return 1
        audit.record(s, "user.bootstrap_admin", actor=user, object_type="user", object_id=user.id)
        accounts.on_first_admin(s, user)
        print(f"Created administrator '{user.handle}' <{user.email}>.")
        print(f"Sign in at {settings.get().url('/')}")
    return 0


def _read_password(args) -> str:
    """The new account's password: prompted (twice), or the first line of standard input with ``--password-stdin``
    (for a script, as ``docker login`` takes one: never an argument, which the process list would show). A password
    from standard input is checked like a typed one."""
    if not getattr(args, "password_stdin", False):
        return _prompt_password()
    password = sys.stdin.readline().rstrip("\r\n")
    passwords.check(password)
    return password


def cmd_create_user(args) -> int:
    """Create an account that is ready to use: active, its email address taken as verified, with the role given
    (design/phase3-features.md P3.2.7; the QA harness makes its accounts with it). An administrator made this way
    adopts the item files nobody owns, as every path that makes one does (``accounts.on_first_admin``)."""
    with db.session() as s:
        try:
            password = _read_password(args)
            user = accounts.create_user(s, handle=args.handle, email=args.email, password=password, role=args.role,
                                        status="active", email_verified=True, display_name=args.name or args.handle)
        except (accounts.AccountError, passwords.PasswordError) as exc:
            print(f"error: {exc}")
            return 1
        audit.record(s, "user.created", actor_label="admin_cli", object_type="user", object_id=user.id,
                     role=user.role)
        accounts.on_first_admin(s, user)
        print(f"Created {user.role} '{user.handle}' <{user.email}>.")
    return 0


def cmd_list_users(args) -> int:
    """List every account with its role and status."""
    with db.session() as s:
        rows = list(s.scalars(select(User).order_by(User.created_at)))
        if not rows:
            print("No accounts yet.")
            return 0
        width = max(len(u.handle) for u in rows)
        print(f"{'handle'.ljust(width)}  {'role':<10} {'status':<10} {'email':<32} created")
        for u in rows:
            created = u.created_at.strftime("%Y-%m-%d") if u.created_at else "?"
            print(f"{u.handle.ljust(width)}  {u.role:<10} {u.status:<10} "
                  f"{(u.email or '—'):<32} {created}")
        print(f"\n{len(rows)} account(s).")
    return 0


def _admin_change(handle: str, changes: dict, reason=None) -> int:
    """Change an account as the admin users page does (``user_admin.apply``: everything checked first, one audit row
    naming the fields, never the last active administrator demoted or suspended), as ``admin_cli``."""
    from ..auth import user_admin
    with db.session() as s:
        user = accounts.by_handle(s, handle)
        if user is None:
            print(f"error: no account '{handle}'")
            return 1
        try:
            changed = user_admin.apply(s, user, changes, actor_label="admin_cli", reason=reason)
        except user_admin.UserAdminError as exc:
            print(f"error: {exc}")
            return 1
        if not changed:
            print(f"{user.handle}: nothing to change")
            return 0
        for field in changed:
            value = getattr(user, field)
            if field == "caps":
                value = ", ".join(f"{k}={'on' if v else 'off'}" for k, v in sorted((value or {}).items())) or "(role defaults)"
            print(f"{user.handle}: {field} -> {value}")
    return 0


def cmd_set_role(args) -> int:
    """Change an account's role."""
    return _admin_change(args.handle, {"role": args.role})


def cmd_set_status(args) -> int:
    """Activate, put on probation or suspend an account (a suspension needs --reason: its owner is told it)."""
    return _admin_change(args.handle, {"status": args.status}, reason=args.reason)


#: How ``set-caps`` reads a setting: on, off, or back to the role's default.
_CAP_WORDS = {"on": True, "true": True, "yes": True, "1": True, "off": False, "false": False, "no": False, "0": False,
              "default": None, "null": None, "none": None}


def cmd_set_caps(args) -> int:
    """Override an account's capabilities (``cap=on``, ``cap=off``), or drop an override (``cap=default``)."""
    caps = {}
    for item in args.caps:
        cap, sep, word = item.partition("=")
        if not sep or word.strip().lower() not in _CAP_WORDS:
            print(f"error: {item!r}: write capability=on, capability=off or capability=default")
            return 1
        caps[cap.strip()] = _CAP_WORDS[word.strip().lower()]
    return _admin_change(args.handle, {"caps": caps})


def cmd_set_limits(args) -> int:
    """Change an account's limits: invitations, games at once, seats per game (0: the ruleset's maximum)."""
    changes = {field: value for field, value in (("invite_quota", args.invites), ("max_concurrent_games", args.games),
                                                 ("max_seats_per_game", args.seats)) if value is not None}
    if not changes:
        print("error: name at least one of --invites, --games, --seats")
        return 1
    return _admin_change(args.handle, changes)


def cmd_set_trust(args) -> int:
    """Set an account's trust tier."""
    return _admin_change(args.handle, {"trust_tier": args.tier})


def cmd_audit(args) -> int:
    """Read the audit log, newest first (the admin page's filters)."""
    import json as _json
    from ..auth import user_admin
    with db.session() as s:
        page = user_admin.audit_page(s, actor=args.actor, action=args.action, object_type=args.object_type,
                                     object_id=args.object_id, before=args.before, limit=args.limit)
    if args.json:
        print(_json.dumps(page, indent=2))
        return 0
    if not page["entries"]:
        print("No entries.")
        return 0
    for e in page["entries"]:
        obj = f"{e['object_type']}/{e['object_id']}" if e["object_type"] else "-"
        detail = _json.dumps(e["detail"], separators=(",", ":")) if e["detail"] else ""
        print(f"{e['id']:>7}  {(e['at'] or '')[:19]}  {e['actor']:<28.28}  {e['action']:<26}  {obj}  {detail}")
    if page["next_before"] is not None:
        print()
        print(f"More: --before {page['next_before']}")
    return 0


def cmd_reset_password(args) -> int:
    """Set an account's password."""
    with db.session() as s:
        user = accounts.by_handle(s, args.handle)
        if user is None:
            print(f"error: no account '{args.handle}'")
            return 1
        password = _prompt_password()
        try:
            accounts.set_password(s, user, password)
        except passwords.PasswordError as exc:
            print(f"error: {exc}")
            return 1
        print(f"Password set for {user.handle}. Other sessions were signed out.")
    return 0


def cmd_invite(args) -> int:
    """Mint an invitation code."""
    with db.session() as s:
        creator = accounts.by_handle(s, args.by) if args.by else s.scalar(
            select(User).where(User.role == "admin", User.status == "active"))
        if creator is None:
            print("error: no administrator to issue the invitation as. Create one first.")
            return 1
        try:
            invite = invites.create(s, creator, role=args.role, email=args.email,
                                    max_uses=args.uses, expires_days=args.days, note=args.note or "")
        except invites.InviteError as exc:
            print(f"error: {exc}")
            return 1
        payload = invites.to_client(s, invite)
        print(f"Invitation code : {payload['code']}")
        print(f"Link            : {payload['url']}")
        print(f"Role            : {payload['role']}   uses: {payload['max_uses']}   "
              f"expires: {payload['expires_at'] or 'never'}")
    return 0


def cmd_add_server(args) -> int:
    """Register a machine and issue its first worker token.

    Exists because the Servers page has no UI for this yet, and because issuing a token from a
    terminal on the box is the most direct route when setting a server up for the first time.
    """
    from .. import pool
    from ..auth import tokens as token_util
    from ..db.models import WorkerToken

    with db.session() as s:
        owner = accounts.by_handle(s, args.owner) if args.owner else s.scalar(
            select(User).where(User.role == "admin", User.status == "active"))
        if owner is None:
            print("error: no owner account. Create an administrator first.")
            return 1
        group = None
        if args.group:
            from ..db.models import ServerGroup
            group = s.scalar(select(ServerGroup).where(ServerGroup.owner_id == owner.id,
                                                       ServerGroup.name == args.group))
            if group is None:
                group = pool.create_group(s, owner, name=args.group)
                print(f"Created server group '{group.name}'.")
        try:
            server = pool.create_server(
                s, owner,
                {"name": args.name, "kind": args.kind,
                 "connection": {"provider": args.provider, "max_parallel": args.max_concurrent}},
                group_id=group.id if group else None, reach="worker")
        except pool.PoolError as exc:
            print(f"error: {exc}")
            return 1

        raw, hashed = token_util.new_pair()
        s.add(WorkerToken(server_id=server.id, token_hash=hashed,
                          prefix=token_util.prefix(raw), label="created by admin_cli",
                          created_by=owner.id))
        origin = settings.get().public_origin
        print(f"Server '{server.name}' registered to {owner.handle}"
              + (f" in group '{group.name}'." if group else "."))
        print(f"  server id : {server.id}")
        print()
        print("Run this on the machine with the model:")
        print(f"  python -m citar.worker --server {origin} \\")
        print(f"      --token {raw} \\")
        print(f"      --name \"{server.name}\"")
        print()
        print("  This token is shown once. Only its hash is stored.")
    return 0


def cmd_worker_token(args) -> int:
    """Issue another worker token for an existing server."""
    from ..auth import tokens as token_util
    from ..db.models import Server, WorkerToken

    with db.session() as s:
        server = s.scalar(select(Server).where(Server.name == args.server))
        if server is None:
            server = s.get(Server, args.server)
        if server is None:
            print(f"error: no server called '{args.server}'")
            print("known servers:", ", ".join(x.name for x in s.scalars(select(Server))) or "(none)")
            return 1
        raw, hashed = token_util.new_pair()
        s.add(WorkerToken(server_id=server.id, token_hash=hashed,
                          prefix=token_util.prefix(raw), label=args.label or "admin_cli"))
        print(f"New worker token for '{server.name}':")
        print(f"  python -m citar.worker --server {settings.get().public_origin} "
              f"--token {raw} --name \"{server.name}\"")
        print("  Shown once; only the hash is stored.")
    return 0


def cmd_workers(args) -> int:
    """Which workers are connected right now.

    Only meaningful inside the running server process, so from the CLI this reports what the
    database knows: which servers exist and when each token was last used.
    """
    from ..db.models import Server, WorkerToken

    with db.session() as s:
        rows = list(s.scalars(select(Server)))
        if not rows:
            print("No servers registered.")
            return 0
        for server in rows:
            owner = s.get(User, server.owner_id) if server.owner_id else None
            toks = list(s.scalars(select(WorkerToken).where(WorkerToken.server_id == server.id)))
            live = [t for t in toks if t.revoked_at is None]
            last = max((t.last_seen_at for t in live if t.last_seen_at), default=None)
            print(f"{server.name}")
            print(f"    owner      : {owner.handle if owner else '?'}")
            print(f"    reach      : {server.reach}   enabled: {server.enabled}")
            print(f"    tokens     : {len(live)} active, {len(toks) - len(live)} revoked")
            print(f"    last seen  : {last.strftime('%Y-%m-%d %H:%M UTC') if last else 'never'}")
            print(f"    models     : {len((server.config or {}).get('models') or [])}")
    return 0


def cmd_test_email(args) -> int:
    """Send a real message through the configured SMTP server.

    Worth having as a command rather than a note in a runbook: the alternative way to discover
    that mail is misconfigured is somebody failing to reset their password, and by then the
    failure is silent and days old.
    """
    import asyncio

    from ..auth import mailer

    cfg = settings.get()
    if not cfg.email_enabled:
        print("No SMTP configured. Set it with: sudo citar-secrets")
        return 1
    print(f"Sending via {cfg.smtp.host}:{cfg.smtp.port} ({cfg.smtp.security}) "
          f"as {cfg.smtp.user or '(no auth)'}")
    print(f"From: {cfg.smtp.from_address}")
    print(f"To:   {args.to}")
    try:
        sent = asyncio.run(mailer.send(
            args.to, "CITAR test message",
            "This is a test from your CITAR server.\n\n"
            "If you are reading it, verification emails, password resets and emailed "
            "invitations will work.\n\n"
            f"Server: {cfg.public_origin}\n"))
    except mailer.MailError as exc:
        print(f"\nFAILED: {exc}")
        print("\nCommon causes:")
        print("  - wrong password (Namecheap Private Email uses the mailbox password)")
        print("  - port 587 needs security=starttls; port 465 needs security=ssl")
        print("  - the From address must be a mailbox the account may send as")
        return 1
    if sent:
        print("\nSent. Check the inbox — and the spam folder, which is where mail from a")
        print("new subdomain often lands until SPF and DKIM are in place.")
    else:
        print("\nNot sent: mail is not enabled in this configuration.")
    return 0 if sent else 1


def cmd_policy(args) -> int:
    """Read or change a runtime policy setting."""
    if args.key is None:
        for key, value in sorted(policy.all_settings().items()):
            if key == "role_defaults":
                value = "{...}"
            print(f"{key:<32} {value}")
        return 0
    if args.value is None:
        print(policy.get(args.key))
        return 0
    raw = args.value
    value = {"true": True, "false": False}.get(raw.lower(), raw)
    if isinstance(value, str) and value.isdigit():
        value = int(value)
    try:
        policy.set(args.key, value)
    except KeyError as exc:
        print(f"error: {exc}")
        return 1
    print(f"{args.key} = {value}")
    return 0


def cmd_config(args) -> int:
    """Print the effective configuration, with no secrets in it."""
    for key, value in settings.get().describe().items():
        print(f"{key:<16} {value}")
    print(f"{'database ok':<16} {db.healthy()}")
    return 0


def main(argv=None) -> int:
    """The ``citar admin`` command line."""
    parser = argparse.ArgumentParser(prog="citar-admin", description=__doc__.split("\n")[0])
    subparsers = parser.add_subparsers(dest="command", required=True)

    p = subparsers.add_parser("create-admin", help="create the first administrator")
    p.add_argument("--handle", required=True)
    p.add_argument("--email", required=True)
    p.add_argument("--name", default="")
    p.add_argument("--force", action="store_true", help="add another admin even if one exists")
    p.set_defaults(func=cmd_create_admin)

    p = subparsers.add_parser("create-user", help="create an active account with a verified email")
    p.add_argument("--handle", required=True)
    p.add_argument("--email", required=True)
    p.add_argument("--name", default="")
    p.add_argument("--role", default="user", choices=["user", "moderator", "admin"])
    p.add_argument("--password-stdin", dest="password_stdin", action="store_true",
                   help="read the password from the first line of standard input instead of prompting")
    p.set_defaults(func=cmd_create_user)

    p = subparsers.add_parser("list-users", help="list every account")
    p.set_defaults(func=cmd_list_users)

    p = subparsers.add_parser("set-role", help="change an account's role")
    p.add_argument("handle")
    p.add_argument("role", choices=["user", "moderator", "admin"])
    p.set_defaults(func=cmd_set_role)

    p = subparsers.add_parser("set-status", help="activate, put on probation or suspend an account")
    p.add_argument("handle")
    p.add_argument("status", choices=["active", "probation", "suspended"])
    p.add_argument("--reason", default=None, help="why (required for a suspension: its owner is told)")
    p.set_defaults(func=cmd_set_status)

    p = subparsers.add_parser("set-caps", help="override an account's capabilities (cap=on|off|default)")
    p.add_argument("handle")
    p.add_argument("caps", nargs="+", metavar="CAP=on|off|default")
    p.set_defaults(func=cmd_set_caps)

    p = subparsers.add_parser("set-limits", help="change an account's limits")
    p.add_argument("handle")
    p.add_argument("--invites", type=int, default=None, help="invitations it may have outstanding (0 to 1000; 0: none)")
    p.add_argument("--games", type=int, default=None, help="lobby games it may run at once (1 to 100)")
    p.add_argument("--seats", type=int, default=None, help="seats per game (0 to 64; 0: the ruleset's maximum)")
    p.set_defaults(func=cmd_set_limits)

    p = subparsers.add_parser("set-trust", help="set an account's trust tier")
    p.add_argument("handle")
    p.add_argument("tier", choices=["new", "provisional", "trusted", "quarantined"])
    p.set_defaults(func=cmd_set_trust)

    p = subparsers.add_parser("audit", help="read the audit log, newest first")
    p.add_argument("--actor", default=None,
                   help="an account's handle or id, or admin_cli for this command line's changes")
    p.add_argument("--action", default=None, help="an action, or a prefix ending in * (user.*)")
    p.add_argument("--object-type", dest="object_type", default=None)
    p.add_argument("--object-id", dest="object_id", default=None)
    p.add_argument("--before", type=int, default=None, help="entries before this id (the next page)")
    p.add_argument("--limit", type=int, default=50, help="at most 200")
    p.add_argument("--json", action="store_true", help="print the page as JSON")
    p.set_defaults(func=cmd_audit)

    p = subparsers.add_parser("reset-password", help="set an account's password")
    p.add_argument("handle")
    p.set_defaults(func=cmd_reset_password)

    p = subparsers.add_parser("invite", help="mint an invitation code")
    p.add_argument("--role", default="user", choices=["user", "moderator", "admin"])
    p.add_argument("--email", default=None, help="pin the invite to one address")
    p.add_argument("--uses", type=int, default=1)
    p.add_argument("--days", type=int, default=invites.DEFAULT_EXPIRY_DAYS)
    p.add_argument("--note", default="")
    p.add_argument("--by", default=None, help="issue as this account (default: an admin)")
    p.set_defaults(func=cmd_invite)

    p = subparsers.add_parser("add-server", help="register a machine and issue its worker token")
    p.add_argument("--name", required=True)
    p.add_argument("--owner", default=None, help="account that owns it (default: an admin)")
    p.add_argument("--group", default=None, help="server group to put it in (created if missing)")
    p.add_argument("--provider", default="lmstudio",
                   choices=["lmstudio", "ollama", "openai_compatible", "anthropic", "dryrun"])
    p.add_argument("--kind", default="owned", choices=["owned", "leased", "api", "test"])
    p.add_argument("--max-concurrent", dest="max_concurrent", type=int, default=1)
    p.set_defaults(func=cmd_add_server)

    p = subparsers.add_parser("worker-token", help="issue another worker token for a server")
    p.add_argument("server", help="server name or id")
    p.add_argument("--label", default="")
    p.set_defaults(func=cmd_worker_token)

    p = subparsers.add_parser("workers", help="list registered servers and their worker tokens")
    p.set_defaults(func=cmd_workers)

    p = subparsers.add_parser("test-email", help="send a test message through the configured SMTP")
    p.add_argument("to", help="where to send it")
    p.set_defaults(func=cmd_test_email)

    p = subparsers.add_parser("policy", help="read or change a runtime policy setting")
    p.add_argument("key", nargs="?", default=None)
    p.add_argument("value", nargs="?", default=None)
    p.set_defaults(func=cmd_policy)

    p = subparsers.add_parser("config", help="show the effective configuration")
    p.set_defaults(func=cmd_config)

    args = parser.parse_args(argv)
    _setup()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
