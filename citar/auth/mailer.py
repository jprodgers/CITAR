"""Outgoing mail: verification links, password resets, invites and security notices.

Plain smtplib on a worker thread rather than another async dependency — CITAR sends a handful of
messages a day, and the simplest thing that cannot block the event loop is enough.

When no SMTP server is configured the messages are written to the log instead of being dropped. That
is what makes the whole email flow testable on the laptop with no mail server: the verification link
appears in the terminal, you paste it into the browser, and the same code path runs that will run in
production. In server mode a missing mail configuration is reported at boot, because there an
unsent verification email looks to the person signing up like the site is broken.

Every message is sent as both plain text and HTML. The text part is not an afterthought: a link
that only exists inside an HTML part is invisible to anyone reading mail as text, and more likely to
be scored as spam.
"""
from __future__ import annotations

import asyncio
import logging
import smtplib
import ssl
from email.message import EmailMessage
from email.utils import formataddr, make_msgid
from typing import Optional

from .. import settings

log = logging.getLogger("citar.mail")


class MailError(RuntimeError):
    """Mail that could not be sent, with the server's own reason."""
    pass


def enabled() -> bool:
    """Whether mail can be sent at all."""
    return settings.get().email_enabled


def _send_now(message: EmailMessage) -> None:
    """Blocking send. Runs on a worker thread."""
    cfg = settings.get().smtp
    context = ssl.create_default_context()
    try:
        if cfg.security == "ssl":
            server = smtplib.SMTP_SSL(cfg.host, cfg.port, timeout=30, context=context)
        else:
            server = smtplib.SMTP(cfg.host, cfg.port, timeout=30)
        with server:
            server.ehlo()
            if cfg.security == "starttls":
                server.starttls(context=context)
                server.ehlo()
            if cfg.user:
                server.login(cfg.user, cfg.password)
            server.send_message(message)
    except Exception as exc:
        # The address is in the log already; never log the body, it contains a single-use token.
        raise MailError(f"Could not send mail via {cfg.host}: {exc}") from exc


async def send(to: str, subject: str, text: str, html: Optional[str] = None) -> bool:
    """Send one message. Returns whether it actually went out over SMTP.

    Raises MailError only when a configured mail server refuses it — a *missing* configuration logs
    the message and returns False, so callers do not have to special-case development.
    """
    cfg = settings.get()
    message = EmailMessage()
    message["Subject"] = subject
    message["From"] = formataddr((cfg.smtp.from_name, cfg.smtp.from_address or "citar@localhost"))
    message["To"] = to
    message["Message-ID"] = make_msgid(domain=(cfg.smtp.from_address.split("@")[-1] or "localhost"))
    # Mail clients must not turn a one-time verification link into a prefetched, already-used link.
    message["X-Auto-Response-Suppress"] = "All"
    message.set_content(text)
    if html:
        message.add_alternative(html, subtype="html")

    if cfg.smtp.transport == "log":
        # Development transport: the whole message, links included, goes to the terminal so the
        # verification and reset flows can be walked through without a mail server. Refused in
        # server mode by settings.load(), so it cannot reach production.
        log.warning("MAIL (not sent — CITAR_MAIL_TRANSPORT=log)\nTo: %s\nSubject: %s\n\n%s",
                    to, subject, text)
        return False
    if not cfg.email_enabled:
        log.warning("No SMTP configured — message to %s not sent. Subject: %s\n%s", to, subject, text)
        return False

    await asyncio.to_thread(_send_now, message)
    log.info("Sent %r to %s", subject, to)
    return True


# ---------------------------------------------------------------------------- templates

def _wrap(title: str, body_html: str) -> str:
    """One plain, inline-styled shell. No images, no external CSS, no tracking pixel."""
    return f"""<!doctype html>
<html><body style="margin:0;padding:24px;background:#f5f6f8;font-family:-apple-system,Segoe UI,Roboto,sans-serif;color:#1c1e21">
  <div style="max-width:520px;margin:0 auto;background:#fff;border-radius:10px;padding:28px 32px;border:1px solid #e3e5e8">
    <div style="font-weight:700;font-size:15px;letter-spacing:.06em;color:#3c78d8;margin-bottom:18px">CITAR</div>
    <h1 style="font-size:19px;margin:0 0 14px">{title}</h1>
    {body_html}
    <hr style="border:none;border-top:1px solid #e9ebee;margin:26px 0 14px">
    <p style="font-size:12px;color:#8a8d91;margin:0">
      Civ Inspired Tool for AI Research · {settings.get().public_origin}
    </p>
  </div>
</body></html>"""


def _button(url: str, label: str) -> str:
    """A call-to-action button for an HTML e-mail."""
    return (f'<p style="margin:22px 0"><a href="{url}" style="background:#3c78d8;color:#fff;'
            f'text-decoration:none;padding:11px 20px;border-radius:7px;display:inline-block;'
            f'font-weight:600">{label}</a></p>'
            f'<p style="font-size:12px;color:#65676b;margin:0">If the button does not work, paste this '
            f'into your browser:<br><span style="word-break:break-all">{url}</span></p>')


async def send_verification(to: str, name: str, url: str, hours: int = 24) -> bool:
    """Send an address-verification link."""
    return await send(
        to, "Confirm your CITAR email address",
        f"""Hello {name},

Confirm this address to finish setting up your CITAR account:

  {url}

The link works once and expires in {hours} hours.

If you did not sign up for CITAR, ignore this message — the account cannot be used
until somebody confirms the address, and it will be cleaned up on its own.
""",
        _wrap("Confirm your email address",
              f"<p>Hello {name}, confirm this address to finish setting up your CITAR account.</p>"
              + _button(url, "Confirm email address")
              + f'<p style="font-size:13px;color:#65676b;margin-top:18px">The link works once and expires in '
                f'{hours} hours. If you did not sign up, you can ignore this message.</p>'))


async def send_password_reset(to: str, name: str, url: str, hours: int = 1) -> bool:
    """Send a password-reset link."""
    return await send(
        to, "Reset your CITAR password",
        f"""Hello {name},

Somebody asked to reset the password on your CITAR account. To choose a new one:

  {url}

The link works once and expires in {hours} hour(s).

If this was not you, no action is needed: your password has not changed, and whoever
asked cannot see this message.
""",
        _wrap("Reset your password",
              f"<p>Hello {name}, somebody asked to reset the password on your CITAR account.</p>"
              + _button(url, "Choose a new password")
              + f'<p style="font-size:13px;color:#65676b;margin-top:18px">The link works once and expires in '
                f'{hours} hour(s). If this was not you, nothing has changed and you can ignore it.</p>'))


async def send_invite(to: str, inviter: str, url: str, note: str = "") -> bool:
    """Send an invitation link."""
    extra = f"\n\nThey added: {note}\n" if note else ""
    extra_html = (f'<p style="background:#f0f2f5;border-radius:7px;padding:12px 14px;font-size:14px">'
                  f'{note}</p>') if note else ""
    return await send(
        to, f"{inviter} invited you to CITAR",
        f"""{inviter} has invited you to CITAR, a tool for running Civilization-style games
with AI players and measuring how they do.{extra}

Accept the invitation:

  {url}
""",
        _wrap(f"{inviter} invited you to CITAR",
              "<p>CITAR is a tool for running Civilization-style games with AI players and measuring "
              "how they do.</p>" + extra_html + _button(url, "Accept the invitation")))


async def send_security_notice(to: str, name: str, what: str, detail: str = "") -> bool:
    """Told-you-so mail for changes somebody would want to know about: password changed, email
    changed, a new sign-in method linked. Sent to the *old* address as well on a change, which is
    what makes a silent account takeover visible to the person losing the account."""
    body = f"""Hello {name},

{what}

{detail}

If this was you, nothing further is needed. If it was not, change your password
immediately and review the sessions and linked accounts on your CITAR account page:

  {settings.get().url('/#/account')}
"""
    return await send(
        to, f"CITAR security: {what}", body,
        _wrap("Security notice",
              f"<p>Hello {name},</p><p><strong>{what}</strong></p>"
              + (f"<p>{detail}</p>" if detail else "")
              + '<p style="font-size:13px;color:#65676b">If this was not you, change your password '
                f'immediately and review your sessions at '
                f'<a href="{settings.get().url("/#/account")}">your account page</a>.</p>'))
