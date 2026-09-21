"""Data pooling and public sharing.

The brief: "Users can (by default) allow their collected data to feed into the server averaging, or
they can lock their data and do private analysis. Reports and collected data can similarly be
locked to only allowed users, or open for the full internet to read."

The tests that matter here are the ones about *withdrawal* — that going private takes your history
out of everybody else's averages retroactively — and about the contributor threshold, because an
average computed over one person is that person's data with a label on it.
"""
from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path

import tests  # noqa: F401

_TMP = Path(tempfile.mkdtemp(prefix="citar_share_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from citar import aggregate, db, settings
from citar.auth import access, accounts, policy
from citar.db.models import Base, Game, Report, User

settings.reset()
db.configure(os.environ["CITAR_DB_URL"])
db.create_all()


def _reset():
    db.dispose()
    db.configure(os.environ["CITAR_DB_URL"])
    Base.metadata.drop_all(db.engine())
    db.create_all()
    policy.invalidate()


class Pooling(unittest.TestCase):
    def setUp(self):
        _reset()
        with db.session() as s:
            self.people = {}
            for handle in ("anna", "brett", "cara", "dilip"):
                u = accounts.create_user(s, handle=handle, email=f"{handle}@example.com",
                                         password="a long enough phrase", status="active",
                                         email_verified=True)
                self.people[handle] = u.id
            # Three pooled contributors, plus one who keeps everything private.
            for handle in ("anna", "brett", "cara"):
                for n in range(2):
                    s.add(Game(id=f"{handle}{n}", owner_id=self.people[handle],
                               name=f"{handle} {n}", data_sharing="pool", kind="game"))
            for n in range(3):
                s.add(Game(id=f"dilip{n}", owner_id=self.people["dilip"],
                           name=f"dilip {n}", data_sharing="private", kind="game"))

    def _user(self, s, handle):
        return s.get(User, self.people[handle])

    def test_pool_contains_only_pooled_games(self):
        with db.session() as s:
            ids = aggregate.pooled_game_ids(s)
        self.assertEqual(len(ids), 6)
        self.assertNotIn("dilip0", ids)

    def test_private_user_still_sees_their_own_data(self):
        """Going private keeps you out of other people's averages. It does not hide your results
        from you — that would make private mode useless for its stated purpose."""
        with db.session() as s:
            scope = aggregate.scope_for(s, self._user(s, "dilip"))
        self.assertEqual(len(scope.own_ids), 3)
        self.assertEqual(len(scope.pooled_ids), 6)
        self.assertEqual(len(scope.game_ids), 9)

    def test_private_games_are_invisible_to_everyone_else(self):
        with db.session() as s:
            scope = aggregate.scope_for(s, self._user(s, "anna"))
        self.assertFalse(any(i.startswith("dilip") for i in scope.pooled_ids))
        self.assertEqual(len(scope.own_ids), 2)

    def test_going_private_withdraws_history_retroactively(self):
        """The filter runs at query time, so changing your mind applies to everything you have
        already done — in both directions."""
        with db.session() as s:
            before = len(aggregate.pooled_game_ids(s))
        with db.session() as s:
            for g in s.query(Game).filter(Game.owner_id == self.people["anna"]):
                g.data_sharing = "private"
        with db.session() as s:
            after = len(aggregate.pooled_game_ids(s))
        self.assertEqual(before - after, 2)

        # ...and coming back restores it.
        with db.session() as s:
            for g in s.query(Game).filter(Game.owner_id == self.people["anna"]):
                g.data_sharing = "pool"
        with db.session() as s:
            self.assertEqual(len(aggregate.pooled_game_ids(s)), before)

    def test_own_only_scope_excludes_the_pool(self):
        with db.session() as s:
            scope = aggregate.scope_for(s, self._user(s, "dilip"), include_pooled=False)
        self.assertEqual(len(scope.pooled_ids), 0)
        self.assertEqual(len(scope.game_ids), 3)

    def test_anonymous_scope_is_pooled_only(self):
        with db.session() as s:
            scope = aggregate.scope_for(s, None)
        self.assertEqual(len(scope.own_ids), 0)
        self.assertEqual(len(scope.pooled_ids), 6)


class ContributorThreshold(unittest.TestCase):
    """A public average must never be readable as one person's numbers."""

    def setUp(self):
        _reset()

    def test_bucket_below_the_threshold_is_withheld(self):
        records = [{"owner": "u1"}, {"owner": "u1"}, {"owner": "u2"}]
        out = aggregate.withhold({"model-a": records}, contributor_of=lambda r: r["owner"],
                                 threshold=3)
        self.assertFalse(out["model-a"]["published"])
        self.assertEqual(out["model-a"]["contributors"], 2)
        self.assertIn("at least 3", out["model-a"]["reason"])

    def test_bucket_at_the_threshold_is_published(self):
        records = [{"owner": f"u{i}"} for i in range(3)]
        out = aggregate.withhold({"model-a": records}, contributor_of=lambda r: r["owner"],
                                 threshold=3)
        self.assertTrue(out["model-a"]["published"])

    def test_many_games_from_one_person_do_not_meet_the_threshold(self):
        # Twenty games from one account is still one contributor. Counting rows rather than
        # accounts is the obvious mistake, and it defeats the whole protection.
        records = [{"owner": "u1"} for _ in range(20)]
        out = aggregate.withhold({"model-a": records}, contributor_of=lambda r: r["owner"],
                                 threshold=3)
        self.assertFalse(out["model-a"]["published"])
        self.assertEqual(out["model-a"]["contributors"], 1)

    def test_withheld_buckets_are_marked_not_dropped(self):
        # The UI should say "not enough data yet", not imply the model was never tested.
        out = aggregate.withhold({"model-a": [{"owner": "u1"}]},
                                 contributor_of=lambda r: r["owner"], threshold=3)
        self.assertIn("model-a", out)
        self.assertIn("reason", out["model-a"])


class ReportSharing(unittest.TestCase):
    def setUp(self):
        _reset()
        with db.session() as s:
            self.owner = accounts.create_user(s, handle="author", email="a@example.com",
                                              password="a long enough phrase", status="active",
                                              email_verified=True).id
            self.other = accounts.create_user(s, handle="reader", email="r@example.com",
                                              password="a long enough phrase", status="active",
                                              email_verified=True).id
            self.admin = accounts.create_user(s, handle="zara", email="z@example.com",
                                              password="a long enough phrase", role="admin",
                                              status="active", email_verified=True).id
            r = Report(owner_id=self.owner, name="Model comparison", visibility="private")
            s.add(r)
            s.flush()
            self.report_id, self.slug = r.id, r.slug

    def _perms(self, handle_id, **kw):
        with db.session() as s:
            viewer = s.get(User, handle_id) if handle_id else None
            return access.on(s, viewer, s.get(Report, self.report_id), **kw)

    def test_private_report_is_owner_only(self):
        self.assertIn(access.MANAGE, self._perms(self.owner))
        self.assertFalse(self._perms(self.other))
        self.assertFalse(self._perms(None))

    def test_link_shared_report_needs_the_key(self):
        with db.session() as s:
            s.get(Report, self.report_id).visibility = "link"
        self.assertFalse(self._perms(None))
        self.assertIn(access.VIEW, self._perms(None, slug=self.slug))

    def test_public_report_is_readable_by_anyone(self):
        with db.session() as s:
            s.get(Report, self.report_id).visibility = "public"
        self.assertIn(access.VIEW, self._perms(None))
        self.assertNotIn(access.MANAGE, self._perms(None))

    def test_publishing_a_report_needs_the_capability(self):
        from fastapi import HTTPException
        with db.session() as s:
            row, owner = s.get(Report, self.report_id), s.get(User, self.owner)
            with self.assertRaises(HTTPException) as caught:
                access.set_visibility(s, row, "public", actor=owner)
            self.assertEqual(caught.exception.status_code, 403)
            # Link sharing is not publishing: it is not listed and not indexed.
            access.set_visibility(s, row, "link", actor=owner)
            self.assertEqual(row.visibility, "link")

    def test_report_visibility_is_independent_of_its_games(self):
        """An analysis can be published while the games behind it stay private — that is the
        normal case for showing a conclusion without handing over raw results."""
        with db.session() as s:
            s.add(Game(id="secret1", owner_id=self.owner, name="private game",
                       data_sharing="private", visibility="private", kind="game"))
            s.get(Report, self.report_id).visibility = "public"
        self.assertIn(access.VIEW, self._perms(None))
        with db.session() as s:
            game_perms = access.on(s, None, s.get(Game, "secret1"))
        self.assertFalse(game_perms)

    def test_rotating_the_key_kills_old_links(self):
        with db.session() as s:
            row = s.get(Report, self.report_id)
            row.visibility = "link"
            old = row.slug
            access.rotate_slug(s, row, actor=s.get(User, self.owner))
            new = row.slug
        self.assertNotEqual(old, new)
        self.assertFalse(self._perms(None, slug=old))
        self.assertIn(access.VIEW, self._perms(None, slug=new))


class SharingSummary(unittest.TestCase):
    def setUp(self):
        _reset()

    def test_summary_counts_accounts_and_games(self):
        with db.session() as s:
            a = accounts.create_user(s, handle="anna", email="a@example.com",
                                     password="a long enough phrase", status="active",
                                     email_verified=True)
            b = accounts.create_user(s, handle="brett", email="b@example.com",
                                     password="a long enough phrase", status="active",
                                     email_verified=True)
            b.data_sharing = "private"
            s.add(Game(id="g1", owner_id=a.id, data_sharing="pool", kind="game"))
            s.add(Game(id="g2", owner_id=b.id, data_sharing="private", kind="game"))
        with db.session() as s:
            summary = aggregate.summarize_sharing(s)
        self.assertEqual(summary["games_total"], 2)
        self.assertEqual(summary["games_pooled"], 1)
        self.assertEqual(summary["games_private"], 1)
        self.assertEqual(summary["accounts_pooling"], 1)
        self.assertEqual(summary["accounts_private"], 1)


if __name__ == "__main__":
    unittest.main()
