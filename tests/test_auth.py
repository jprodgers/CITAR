"""Accounts, sessions and permissions.

Runs against the real FastAPI app through TestClient, so the dependency wiring, cookies, CSRF and
status codes are exercised the way a browser exercises them — not just the functions underneath.
"""
from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path

import tests  # noqa: F401  — sets CITAR_SAVE_DIR and the throwaway server registry

_TMP = Path(tempfile.mkdtemp(prefix="citar_auth_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ["CITAR_MODE"] = "server"
os.environ["CITAR_PUBLIC_ORIGIN"] = "https://citar.test"
os.environ["CITAR_SECRET_KEY"] = "test-secret-key-that-is-long-enough-to-pass"
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"
os.environ["CITAR_REGISTRATION"] = "open"
os.environ["CITAR_SMTP_HOST"] = "localhost"      # enables the email paths; mailer logs instead
os.environ["CITAR_MAIL_FROM"] = "citar@citar.test"

from citar import db, settings
from citar.auth import accounts, invites, passwords, policy, ratelimit, sessions

settings.reset()
db.configure(os.environ["CITAR_DB_URL"])
db.create_all()


class _Outbox(list):
    """Captures mail instead of sending it, so the email flows can be tested end to end."""

    async def __call__(self, to, subject, text, html=None):
        self.append({"to": to, "subject": subject, "text": text})
        return True


def _capture_mail():
    from citar.auth import mailer
    outbox = _Outbox()
    mailer.send = outbox
    return outbox


def _client():
    from fastapi.testclient import TestClient
    from citar.server.app import app
    client = TestClient(app, base_url="https://citar.test")
    return client


def _reset_db():
    from citar.db.models import Base
    db.dispose()
    db.configure(os.environ["CITAR_DB_URL"])
    Base.metadata.drop_all(db.engine())
    db.create_all()
    policy.invalidate()
    ratelimit._hits.clear()
    ratelimit._blocks.clear()


class PasswordRules(unittest.TestCase):
    def test_rejects_weak_passwords(self):
        for bad in ["", "short", "password123", "aaaaaaaaaaaa", "abcdefghijkl", "Password1"]:
            with self.assertRaises(passwords.PasswordError, msg=bad):
                passwords.check(bad)

    def test_rejects_password_containing_identity(self):
        with self.assertRaises(passwords.PasswordError):
            passwords.check("ada-is-great", handle="ada")
        with self.assertRaises(passwords.PasswordError):
            passwords.check("alice-rules-ok!", email="alice@example.com")

    def test_accepts_a_passphrase(self):
        passwords.check("correct horse battery staple")

    def test_hash_round_trip(self):
        h = passwords.hash_password("correct horse battery staple")
        self.assertTrue(passwords.verify(h, "correct horse battery staple"))
        self.assertFalse(passwords.verify(h, "correct horse battery stapl"))

    def test_no_password_never_authenticates(self):
        # SSO-only accounts have no hash; an empty password must not slip through.
        self.assertFalse(passwords.verify(None, ""))
        self.assertFalse(passwords.verify("", ""))


class Handles(unittest.TestCase):
    def test_reserved_and_malformed_rejected(self):
        for bad in ["ab", "admin", "root", "-lead", "trail-", "has space", "a" * 40]:
            with self.assertRaises(accounts.AccountError, msg=bad):
                accounts.validate_handle(bad)

    def test_unicode_lookalikes_fold_together(self):
        # Without NFKC folding these register as distinct handles and can impersonate each other.
        self.assertEqual(accounts.normalize_handle("ＡＤＭＩＮ"), "admin")
        self.assertEqual(accounts.normalize_handle("  Ada_L "), "ada_l")


class AccountCreation(unittest.TestCase):
    def setUp(self):
        _reset_db()

    def test_duplicate_handle_and_email_blocked_case_insensitively(self):
        with db.session() as s:
            accounts.create_user(s, handle="alice", email="Alice@Example.com",
                                 password="a long enough phrase")
        with db.session() as s:
            with self.assertRaises(accounts.AccountError):
                accounts.create_user(s, handle="ALICE", email="other@example.com",
                                     password="a long enough phrase")
            with self.assertRaises(accounts.AccountError):
                accounts.create_user(s, handle="bob", email="ALICE@example.COM",
                                     password="a long enough phrase")

    def test_disposable_addresses_blocked(self):
        with db.session() as s:
            with self.assertRaises(accounts.AccountError):
                accounts.create_user(s, handle="throw", email="x@mailinator.com",
                                     password="a long enough phrase")
            with self.assertRaises(accounts.AccountError):
                accounts.create_user(s, handle="throw", email="x@sub.mailinator.com",
                                     password="a long enough phrase")

    def test_unverified_account_starts_pending_and_cannot_log_in(self):
        with db.session() as s:
            user = accounts.create_user(s, handle="carol", email="carol@example.com",
                                        password="a long enough phrase")
            self.assertEqual(user.status, "pending")
            with self.assertRaises(accounts.AccountError):
                accounts.authenticate(s, "carol", "a long enough phrase")

    def test_login_failures_are_indistinguishable(self):
        with db.session() as s:
            accounts.create_user(s, handle="dave", email="dave@example.com",
                                 password="a long enough phrase", email_verified=True)
        with db.session() as s:
            try:
                accounts.authenticate(s, "dave", "wrong password here")
                self.fail("expected failure")
            except accounts.AccountError as exc:
                wrong_password = str(exc)
            try:
                accounts.authenticate(s, "nobody-at-all", "wrong password here")
                self.fail("expected failure")
            except accounts.AccountError as exc:
                no_account = str(exc)
        # Same message, or the login form becomes a way to enumerate who is registered.
        self.assertEqual(wrong_password, no_account)


class EmailTokens(unittest.TestCase):
    def setUp(self):
        _reset_db()

    def test_token_is_single_use(self):
        with db.session() as s:
            user = accounts.create_user(s, handle="erin", email="erin@example.com",
                                        password="a long enough phrase")
            raw, url = accounts.issue_email_token(s, user, "verify")
        with db.session() as s:
            found, _row = accounts.consume_email_token(s, raw, "verify")
            self.assertEqual(found.handle, "erin")
        with db.session() as s:
            with self.assertRaises(accounts.AccountError):
                accounts.consume_email_token(s, raw, "verify")

    def test_token_purposes_are_not_interchangeable(self):
        with db.session() as s:
            user = accounts.create_user(s, handle="frank", email="frank@example.com",
                                        password="a long enough phrase")
            raw, _url = accounts.issue_email_token(s, user, "verify")
        with db.session() as s:
            # A verification link must not double as a password reset.
            with self.assertRaises(accounts.AccountError):
                accounts.consume_email_token(s, raw, "reset")

    def test_issuing_invalidates_the_previous_token(self):
        with db.session() as s:
            user = accounts.create_user(s, handle="gina", email="gina@example.com",
                                        password="a long enough phrase")
            first, _ = accounts.issue_email_token(s, user, "reset")
            second, _ = accounts.issue_email_token(s, user, "reset")
        with db.session() as s:
            with self.assertRaises(accounts.AccountError):
                accounts.consume_email_token(s, first, "reset")
            accounts.consume_email_token(s, second, "reset")


class Capabilities(unittest.TestCase):
    def setUp(self):
        _reset_db()

    def _user(self, **kw):
        with db.session() as s:
            return accounts.create_user(s, handle=kw.pop("handle", "user1"),
                                        email=kw.pop("email", "u1@example.com"),
                                        password="a long enough phrase", email_verified=True, **kw)

    def test_probation_blocks_the_risky_capabilities(self):
        user = self._user(status="probation")
        self.assertTrue(policy.can(user, "create_games"))
        self.assertFalse(policy.can(user, "register_servers"))
        self.assertFalse(policy.can(user, "publish_public"))
        self.assertFalse(policy.can(user, "invite"))

    def test_suspension_overrides_every_grant(self):
        user = self._user(status="active", role="admin")
        self.assertTrue(policy.can(user, "publish_public"))
        user.status = "suspended"
        # An explicit override must not resurrect a suspended account.
        user.caps = {"publish_public": True}
        self.assertFalse(policy.can(user, "publish_public"))

    def test_explicit_override_beats_the_role_default(self):
        user = self._user(status="active")
        self.assertFalse(policy.can(user, "publish_public"))
        user.caps = {"publish_public": True}
        self.assertTrue(policy.can(user, "publish_public"))


class Invitations(unittest.TestCase):
    def setUp(self):
        _reset_db()

    def test_only_admins_can_invite_elevated_roles(self):
        with db.session() as s:
            admin = accounts.create_user(s, handle="root1", email="root@example.com",
                                         password="a long enough phrase", role="admin",
                                         status="active", email_verified=True)
            mod = accounts.create_user(s, handle="mod1", email="mod@example.com",
                                       password="a long enough phrase", role="moderator",
                                       status="active", email_verified=True)
            mod.invite_quota = 10
            invites.create(s, admin, role="admin")
            with self.assertRaises(invites.InviteError):
                invites.create(s, mod, role="admin")
            invites.create(s, mod, role="user")

    def test_quota_counts_outstanding_uses(self):
        with db.session() as s:
            user = accounts.create_user(s, handle="host1", email="host@example.com",
                                        password="a long enough phrase", status="active",
                                        email_verified=True)
            user.invite_quota = 3
            user.caps = {"invite": True}
            invites.create(s, user, max_uses=3)
            with self.assertRaises(invites.InviteError):
                invites.create(s, user, max_uses=1)

    def test_sloppy_code_formatting_is_accepted(self):
        with db.session() as s:
            admin = accounts.create_user(s, handle="root2", email="root2@example.com",
                                         password="a long enough phrase", role="admin",
                                         status="active", email_verified=True)
            invite = invites.create(s, admin)
            code = invite.code
        with db.session() as s:
            for variant in (code, code.lower(), code.replace("-", ""), f"  {code.lower()}  "):
                self.assertEqual(invites.check(s, variant).code, code)

    def test_pinned_email_is_enforced(self):
        with db.session() as s:
            admin = accounts.create_user(s, handle="root3", email="root3@example.com",
                                         password="a long enough phrase", role="admin",
                                         status="active", email_verified=True)
            invite = invites.create(s, admin, email="wanted@example.com")
            invites.check(s, invite.code, email="wanted@example.com")
            with self.assertRaises(invites.InviteError):
                invites.check(s, invite.code, email="someone.else@example.com")


class HttpFlows(unittest.TestCase):
    """The API as a browser sees it."""

    def setUp(self):
        _reset_db()
        self.outbox = _capture_mail()
        self.client = _client()

    def tearDown(self):
        self.client.close()

    def _signup(self, handle="newbie", email="newbie@example.com", password="a long enough phrase",
                **kw):
        return self.client.post("/api/auth/signup",
                                json={"handle": handle, "email": email, "password": password, **kw})

    def _verify_latest(self, handle):
        """Redeem the verification token directly — the mailer only logs in tests."""
        with db.session() as s:
            user = accounts.by_handle(s, handle)
            raw, _url = accounts.issue_email_token(s, user, "verify")
        return self.client.get("/auth/verify", params={"token": raw}, follow_redirects=False)

    def test_config_endpoint_leaks_no_secrets(self):
        body = self.client.get("/api/auth/config").json()
        self.assertIn("registration", body)
        self.assertIn("providers", body)
        text = str(body)
        self.assertNotIn("test-secret-key", text)

    def test_signup_verify_login_logout(self):
        response = self._signup()
        self.assertEqual(response.status_code, 200, response.text)

        # Not confirmed yet, so no sign-in.
        denied = self.client.post("/api/auth/login",
                                  json={"identifier": "newbie", "password": "a long enough phrase"})
        self.assertEqual(denied.status_code, 401)

        self.assertEqual(self._verify_latest("newbie").status_code, 303)

        ok = self.client.post("/api/auth/login",
                              json={"identifier": "newbie", "password": "a long enough phrase"})
        self.assertEqual(ok.status_code, 200, ok.text)
        csrf = ok.json()["csrf_token"]
        self.assertIn(sessions.COOKIE_NAME, self.client.cookies)

        me = self.client.get("/api/auth/me").json()
        self.assertTrue(me["authenticated"])
        self.assertEqual(me["user"]["handle"], "newbie")

        out = self.client.post("/api/auth/logout", headers={sessions.CSRF_HEADER: csrf})
        self.assertEqual(out.status_code, 200)
        self.assertFalse(self.client.get("/api/auth/me").json()["authenticated"])

    def test_state_changing_request_needs_csrf_token(self):
        self._signup(handle="csrfuser", email="csrf@example.com")
        self._verify_latest("csrfuser")
        self.client.post("/api/auth/login",
                         json={"identifier": "csrfuser", "password": "a long enough phrase"})
        # Cookie present, CSRF header missing: must be refused.
        blocked = self.client.put("/api/auth/me", json={"display_name": "Hacked"})
        self.assertEqual(blocked.status_code, 403)

    def test_invite_only_mode_blocks_open_signup(self):
        policy.set("registration", "invite")
        try:
            response = self._signup(handle="uninvited", email="uninvited@example.com")
            self.assertEqual(response.status_code, 403)
        finally:
            policy.set("registration", "open")

    def test_signup_with_invite_works_in_invite_only_mode(self):
        with db.session() as s:
            admin = accounts.create_user(s, handle="root9", email="root9@example.com",
                                         password="a long enough phrase", role="admin",
                                         status="active", email_verified=True)
            code = invites.create(s, admin, role="moderator").code
        policy.set("registration", "invite")
        try:
            response = self._signup(handle="invited", email="invited@example.com", invite=code)
            self.assertEqual(response.status_code, 200, response.text)
            with db.session() as s:
                self.assertEqual(accounts.by_handle(s, "invited").role, "moderator")
        finally:
            policy.set("registration", "open")

    def test_password_reset_round_trip_and_session_revocation(self):
        self._signup(handle="forgetful", email="forgetful@example.com")
        self._verify_latest("forgetful")
        self.client.post("/api/auth/login",
                         json={"identifier": "forgetful", "password": "a long enough phrase"})
        self.assertTrue(self.client.get("/api/auth/me").json()["authenticated"])

        with db.session() as s:
            user = accounts.by_handle(s, "forgetful")
            raw, _url = accounts.issue_email_token(s, user, "reset")
        done = self.client.post("/api/auth/password/reset",
                                json={"token": raw, "password": "a different long phrase"})
        self.assertEqual(done.status_code, 200, done.text)

        # The reset signed every device out, including this one.
        self.assertFalse(self.client.get("/api/auth/me").json()["authenticated"])
        again = self.client.post("/api/auth/login",
                                 json={"identifier": "forgetful", "password": "a different long phrase"})
        self.assertEqual(again.status_code, 200)

    def test_forgot_password_does_not_reveal_registration(self):
        self._signup(handle="known", email="known@example.com")
        known = self.client.post("/api/auth/password/forgot", json={"email": "known@example.com"})
        unknown = self.client.post("/api/auth/password/forgot", json={"email": "nobody@example.com"})
        self.assertEqual(known.status_code, unknown.status_code)
        self.assertEqual(known.json()["message"], unknown.json()["message"])

    def test_suspended_account_is_locked_out_immediately(self):
        self._signup(handle="badactor", email="bad@example.com")
        self._verify_latest("badactor")
        self.client.post("/api/auth/login",
                         json={"identifier": "badactor", "password": "a long enough phrase"})
        self.assertTrue(self.client.get("/api/auth/me").json()["authenticated"])

        with db.session() as s:
            accounts.by_handle(s, "badactor").status = "suspended"
        # The live cookie must stop working now, not when it expires.
        self.assertFalse(self.client.get("/api/auth/me").json()["authenticated"])

    def test_login_rate_limit_locks_out_brute_force(self):
        self._signup(handle="target", email="target@example.com")
        self._verify_latest("target")
        statuses = []
        for _ in range(14):
            r = self.client.post("/api/auth/login",
                                 json={"identifier": "target", "password": "definitely wrong here"})
            statuses.append(r.status_code)
        self.assertIn(429, statuses, "brute force was never rate limited")


class LocalMode(unittest.TestCase):
    """The laptop workflow: no login form, but still a real account and a real session."""

    def setUp(self):
        _reset_db()
        settings.set_for_test(mode="local", db_url=os.environ["CITAR_DB_URL"],
                              secret_key="test-secret-key-that-is-long-enough-to-pass")

    def tearDown(self):
        settings.reset()

    def test_local_mode_signs_in_automatically_as_an_admin(self):
        client = _client()
        try:
            body = client.get("/api/auth/me").json()
            self.assertTrue(body["authenticated"])
            self.assertEqual(body["user"]["role"], "admin")
            self.assertTrue(body["local_mode"])
        finally:
            client.close()

    def test_local_owner_is_created_once(self):
        with db.session() as s:
            first = accounts.ensure_local_owner(s)
        with db.session() as s:
            self.assertEqual(accounts.ensure_local_owner(s).id, first.id)


if __name__ == "__main__":
    unittest.main()
