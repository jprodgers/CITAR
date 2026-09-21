"""Authorization: who may see, play and manage what.

These are the tests that matter most in the whole project. Everything else being wrong makes CITAR
annoying; this being wrong makes somebody's private game readable by the internet.
"""
from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path

import tests  # noqa: F401

_TMP = Path(tempfile.mkdtemp(prefix="citar_access_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from fastapi import HTTPException

from citar import db, settings
from citar.auth import access, accounts, policy
from citar.db.models import Base, Game as GameRow, Report

settings.reset()
db.configure(os.environ["CITAR_DB_URL"])
db.create_all()


def _reset():
    db.dispose()
    db.configure(os.environ["CITAR_DB_URL"])
    Base.metadata.drop_all(db.engine())
    db.create_all()
    policy.invalidate()


class AccessModel(unittest.TestCase):
    def setUp(self):
        _reset()
        with db.session() as s:
            self.owner = accounts.create_user(s, handle="owner", email="owner@example.com",
                                              password="a long enough phrase", status="active",
                                              email_verified=True)
            self.other = accounts.create_user(s, handle="other", email="other@example.com",
                                              password="a long enough phrase", status="active",
                                              email_verified=True)
            self.mod = accounts.create_user(s, handle="maggie", email="maggie@example.com",
                                            password="a long enough phrase", role="moderator",
                                            status="active", email_verified=True)
            self.admin = accounts.create_user(s, handle="zara", email="zara@example.com",
                                              password="a long enough phrase", role="admin",
                                              status="active", email_verified=True)
            self.owner_id = self.owner.id

    def _game(self, visibility="private", **kw):
        with db.session() as s:
            row = GameRow(id=kw.pop("id", "g" + os.urandom(4).hex()), owner_id=self.owner_id,
                          name="Test game", visibility=visibility, **kw)
            s.add(row)
            s.flush()
            s.expunge(row)
            return row

    def _perms(self, viewer, row, **kw):
        with db.session() as s:
            return access.on(s, viewer, s.get(GameRow, row.id), **kw)

    # ---------------------------------------------------------------- private
    def test_private_game_is_owner_only(self):
        game = self._game("private")
        self.assertIn(access.VIEW, self._perms(self.owner, game))
        self.assertIn(access.MANAGE, self._perms(self.owner, game))
        self.assertFalse(self._perms(self.other, game))
        self.assertFalse(self._perms(None, game))

    def test_stranger_gets_404_not_403_on_a_private_game(self):
        # 403 would confirm the game exists, turning id guessing into an enumeration oracle.
        game = self._game("private")
        with self.assertRaises(HTTPException) as caught:
            self._perms(self.other, game).require(access.VIEW)
        self.assertEqual(caught.exception.status_code, 404)

    # ---------------------------------------------------------------- public
    def test_public_game_is_viewable_by_anyone_but_not_playable(self):
        game = self._game("public")
        anon = self._perms(None, game)
        self.assertIn(access.VIEW, anon)
        # "Open to the internet if the link is good" must not mean strangers can take your cities.
        self.assertNotIn(access.PLAY, anon)
        self.assertNotIn(access.MANAGE, anon)

    def test_public_game_denies_play_with_403_once_viewable(self):
        game = self._game("public")
        with self.assertRaises(HTTPException) as caught:
            self._perms(self.other, game).require(access.PLAY)
        self.assertEqual(caught.exception.status_code, 403)

    # ---------------------------------------------------------------- link
    def test_link_game_needs_the_right_slug(self):
        game = self._game("link")
        self.assertFalse(self._perms(None, game))
        self.assertFalse(self._perms(None, game, slug="wrong-slug"))
        self.assertIn(access.VIEW, self._perms(None, game, slug=game.slug))

    def test_rotating_the_slug_kills_old_links(self):
        game = self._game("link")
        old = game.slug
        with db.session() as s:
            row = s.get(GameRow, game.id)
            access.rotate_slug(s, row, actor=self.owner)
            new = row.slug
        self.assertNotEqual(old, new)
        self.assertFalse(self._perms(None, game, slug=old))
        self.assertIn(access.VIEW, self._perms(None, game, slug=new))

    # ---------------------------------------------------------------- allowlist
    def test_allowlist_grants_exactly_what_was_given(self):
        game = self._game("allowlist")
        self.assertFalse(self._perms(self.other, game))
        with db.session() as s:
            access.grant(s, s.get(GameRow, game.id), s.get(type(self.other), self.other.id),
                         access.VIEW, actor=self.owner)
        perms = self._perms(self.other, game)
        self.assertIn(access.VIEW, perms)
        self.assertNotIn(access.PLAY, perms)

    def test_play_grant_implies_view(self):
        game = self._game("allowlist")
        with db.session() as s:
            access.grant(s, s.get(GameRow, game.id), s.get(type(self.other), self.other.id),
                         access.PLAY, actor=self.owner)
        perms = self._perms(self.other, game)
        self.assertIn(access.VIEW, perms)
        self.assertIn(access.PLAY, perms)
        self.assertNotIn(access.MANAGE, perms)

    def test_revoking_removes_access(self):
        game = self._game("allowlist")
        with db.session() as s:
            row, target = s.get(GameRow, game.id), s.get(type(self.other), self.other.id)
            access.grant(s, row, target, access.PLAY, actor=self.owner)
        self.assertIn(access.PLAY, self._perms(self.other, game))
        with db.session() as s:
            row, target = s.get(GameRow, game.id), s.get(type(self.other), self.other.id)
            access.revoke(s, row, target, actor=self.owner)
        self.assertFalse(self._perms(self.other, game))

    # ---------------------------------------------------------------- roles
    def test_moderator_can_reach_published_content_only(self):
        private_game = self._game("private")
        public_game = self._game("public")
        # A moderation tool must not become a surveillance tool.
        self.assertFalse(self._perms(self.mod, private_game))
        perms = self._perms(self.mod, public_game)
        self.assertIn(access.VIEW, perms)
        self.assertIn(access.MANAGE, perms)     # enough to unpublish

    def test_admin_reaches_everything(self):
        game = self._game("private")
        perms = self._perms(self.admin, game)
        self.assertIn(access.VIEW, perms)
        self.assertIn(access.MANAGE, perms)
        self.assertIn(access.ADMIN, perms)

    def test_suspended_owner_loses_access_to_their_own_game(self):
        game = self._game("private")
        with db.session() as s:
            s.get(type(self.owner), self.owner_id).status = "suspended"
        with db.session() as s:
            owner = s.get(type(self.owner), self.owner_id)
            self.assertFalse(access.on(s, owner, s.get(GameRow, game.id)))

    # ---------------------------------------------------------------- seats
    def test_seat_holder_plays_without_an_account(self):
        # An LLM or MCP player holds a seat token and has no account at all.
        game = self._game("private")
        with db.session() as s:
            perms = access.on(s, None, s.get(GameRow, game.id), seat_holder=True)
        self.assertIn(access.VIEW, perms)
        self.assertIn(access.PLAY, perms)
        self.assertNotIn(access.MANAGE, perms)

    # ---------------------------------------------------------------- deleted
    def test_deleted_game_is_gone_for_its_owner(self):
        from datetime import datetime, timezone
        game = self._game("private")
        with db.session() as s:
            s.get(GameRow, game.id).deleted_at = datetime.now(timezone.utc)
        self.assertFalse(self._perms(self.owner, game))

    # ---------------------------------------------------------------- publishing
    def test_publishing_requires_the_capability(self):
        game = self._game("private")
        with db.session() as s:
            row = s.get(GameRow, game.id)
            owner = s.get(type(self.owner), self.owner_id)
            # A plain user cannot publish to the whole internet by default.
            with self.assertRaises(HTTPException) as caught:
                access.set_visibility(s, row, "public", actor=owner)
            self.assertEqual(caught.exception.status_code, 403)
            # Link sharing is fine — it is not listed or indexed.
            access.set_visibility(s, row, "link", actor=owner)
            self.assertEqual(row.visibility, "link")

    def test_admin_can_publish(self):
        game = self._game("private")
        with db.session() as s:
            access.set_visibility(s, s.get(GameRow, game.id), "public",
                                  actor=s.get(type(self.admin), self.admin.id))
            self.assertEqual(s.get(GameRow, game.id).visibility, "public")

    # ---------------------------------------------------------------- listing
    def test_listing_excludes_other_peoples_and_unlisted_games(self):
        self._game("private")
        self._game("public")
        self._game("link")
        with db.session() as s:
            other = s.get(type(self.other), self.other.id)
            rows = access.visible(s, GameRow, other)
            visibilities = sorted(r.visibility for r in rows)
        # Only the public one. `link` is reachable by URL but never listed.
        self.assertEqual(visibilities, ["public"])

    def test_owner_lists_their_own_whatever_the_visibility(self):
        self._game("private")
        self._game("link")
        with db.session() as s:
            owner = s.get(type(self.owner), self.owner_id)
            self.assertEqual(len(access.visible(s, GameRow, owner)), 2)

    def test_access_client_does_not_clobber_the_rows_visibility(self):
        """A stale permission snapshot must not overwrite a visibility that was just changed.

        Regression: Access.client() used to carry `visibility`, so a sharing response built by
        merging it over the row reported the OLD value while also emitting a share link for the new
        one — the API contradicting itself in a single payload.
        """
        from citar.server import ownership
        game = self._game("private")
        with db.session() as s:
            row = s.get(GameRow, game.id)
            owner = s.get(type(self.owner), self.owner_id)
            perms = access.on(s, owner, row)          # snapshot taken while still private
            access.set_visibility(s, row, "link", actor=owner)
            payload = ownership.to_client(row, perms, base_url="https://citar.test")
        self.assertEqual(payload["visibility"], "link")
        self.assertIn("share_url", payload)

    # ---------------------------------------------------------------- reports
    def test_reports_use_the_same_ladder(self):
        with db.session() as s:
            report = Report(owner_id=self.owner_id, name="Analysis", visibility="private")
            s.add(report)
            s.flush()
            rid = report.id
        with db.session() as s:
            row = s.get(Report, rid)
            self.assertFalse(access.on(s, s.get(type(self.other), self.other.id), row))
            self.assertIn(access.MANAGE, access.on(s, s.get(type(self.owner), self.owner_id), row))


if __name__ == "__main__":
    unittest.main()
