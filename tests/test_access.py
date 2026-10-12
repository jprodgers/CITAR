"""Authorization: who may see, play and manage what.

These are the tests that matter most in the whole project. Everything else being wrong makes CITAR
annoying; this being wrong makes somebody's private game readable by the internet.

The visibility ladder of design/phase3-features.md P3.2.3 (private, allowlist, link, server, global),
the moderator's one permission (unpublish), and the scan that keeps every visibility string in the code
on the ladder (package 3-01's gate 2).
"""
from __future__ import annotations

import ast
import os
import tempfile
import unittest
from pathlib import Path

import tests  # noqa: F401
from tests import has_test_ops

_TMP = Path(tempfile.mkdtemp(prefix="citar_access_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from fastapi import HTTPException
from sqlalchemy import select

from citar import db, settings
from citar.auth import access, accounts, policy
from citar.db.models import AuditLog, Base, Game as GameRow, OwnedItem, User

settings.reset()
db.configure(os.environ["CITAR_DB_URL"])
db.create_all()

ROOT = Path(__file__).resolve().parent.parent
V, P, M, MOD = access.VIEW, access.PLAY, access.MANAGE, access.MODERATE


def _reset():
    db.dispose()
    db.configure(os.environ["CITAR_DB_URL"])
    Base.metadata.drop_all(db.engine())
    db.create_all()
    policy.invalidate()


def _user(s, handle, **kw):
    return accounts.create_user(s, handle=handle, email=f"{handle}@example.com", password="a long enough phrase",
                                email_verified=True, **{"status": "active", **kw})


class AccessModel(unittest.TestCase):
    def setUp(self):
        _reset()
        with db.session() as s:
            self.owner = _user(s, "owner")
            self.other = _user(s, "other")
            self.newbie = _user(s, "newbie", status="probation")
            self.banned = _user(s, "banned")
            self.reader = _user(s, "reader")
            self.mod = _user(s, "maggie", role="moderator")
            self.admin = _user(s, "zara", role="admin")
            self.owner_id = self.owner.id
        with db.session() as s:
            # suspended after creation: create_user takes no such status
            s.get(User, self.banned.id).status = "suspended"

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
            target = s.get(type(row), row.id)
            who = s.get(User, viewer.id) if viewer is not None else None
            return access.on(s, who, target, **kw)

    def _share(self, row, user, permission=access.VIEW):
        with db.session() as s:
            access.grant(s, s.get(type(row), row.id), s.get(User, user.id), permission,
                         actor=s.get(User, self.owner_id))

    # ---------------------------------------------------------------- the ladder table
    def test_the_ladder_by_level_viewer_and_permission(self):
        """Five levels by eight kinds of viewer by view, play, manage and moderate (P3.2.3). The ACL viewer
        holds a view entry at every level; nobody presents a slug here (see the link tests)."""
        everything = set(access.ALL)
        expected = {
            #           anonymous  member  probation  suspended  owner           ACL viewer  moderator  admin
            "private":   (set(),   set(),  set(),     set(),     {V, P, M},      {V},        set(),     everything),
            "allowlist": (set(),   set(),  set(),     set(),     {V, P, M},      {V},        set(),     everything),
            "link":      (set(),   set(),  set(),     set(),     {V, P, M},      {V},        set(),     everything),
            "server":    (set(),   {V},    {V},       set(),     {V, P, M},      {V},        {V},       everything),
            "global":    ({V},     {V},    {V},       set(),     {V, P, M},      {V},        {V, MOD},  everything),
        }
        viewers = (None, self.other, self.newbie, self.banned, self.owner, self.reader, self.mod, self.admin)
        names = ("anonymous", "member", "probation", "suspended", "owner", "acl viewer", "moderator", "admin")
        self.assertEqual(tuple(expected), access.VISIBILITY)
        for visibility, row_expect in expected.items():
            for kind in ("game", "item"):
                if kind == "game":
                    row = self._game(visibility)
                else:
                    with db.session() as s:
                        item = OwnedItem(kind="report", ref=os.urandom(4).hex(), owner_id=self.owner_id,
                                         visibility=visibility)
                        s.add(item)
                        s.flush()
                        s.expunge(item)
                    row = item
                self._share(row, self.reader)
                for viewer, name, want in zip(viewers, names, row_expect):
                    with self.subTest(visibility=visibility, kind=kind, viewer=name):
                        got = self._perms(viewer, row).permissions
                        self.assertEqual(got & {V, P, M, MOD, access.ADMIN}, want)

    def test_a_link_opens_with_its_slug_and_a_moderator_may_unpublish_it(self):
        game = self._game("link")
        self.assertEqual(self._perms(None, game, slug=game.slug).permissions, {V})
        self.assertEqual(self._perms(self.other, game, slug=game.slug).permissions, {V})
        self.assertEqual(self._perms(self.mod, game, slug=game.slug).permissions, {V, MOD})
        self.assertFalse(self._perms(self.mod, game, slug="wrong"))
        self.assertFalse(self._perms(None, game, slug="wrong-slug"))

    def test_on_a_server_object_a_moderator_has_nothing_a_member_lacks(self):
        game = self._game("server")
        self.assertEqual(self._perms(self.mod, game).permissions, self._perms(self.other, game).permissions)
        self.assertNotIn(MOD, self._perms(self.mod, game))

    def test_acl_entries_count_at_every_level(self):
        for visibility in access.VISIBILITY:
            game = self._game(visibility)
            self._share(game, self.other, access.PLAY)
            with self.subTest(visibility=visibility):
                self.assertTrue({V, P} <= self._perms(self.other, game).permissions)

    # ---------------------------------------------------------------- listing
    def test_visible_filter_per_viewer(self):
        rows = {v: self._game(v, id=f"list-{v}") for v in access.VISIBILITY}
        self._share(rows["private"], self.reader)
        self._share(rows["allowlist"], self.reader)
        expected = {
            "anonymous": {"global"},
            "member": {"server", "global"},
            "probation": {"server", "global"},
            "suspended": {"global"},
            "owner": set(access.VISIBILITY),
            "acl viewer": {"private", "allowlist", "server", "global"},
            "moderator": {"server", "global"},
            "admin": set(access.VISIBILITY),
        }
        viewers = {"anonymous": None, "member": self.other, "probation": self.newbie, "suspended": self.banned,
                   "owner": self.owner, "acl viewer": self.reader, "moderator": self.mod, "admin": self.admin}
        for name, want in expected.items():
            with self.subTest(viewer=name), db.session() as s:
                viewer = s.get(User, viewers[name].id) if viewers[name] is not None else None
                got = {r.visibility for r in access.visible(s, GameRow, viewer)}
                self.assertEqual(got, want)

    def test_link_rows_are_never_listed_and_deleted_rows_only_for_nobody(self):
        from datetime import datetime, timezone
        self._game("link")
        gone = self._game("global")
        with db.session() as s:
            s.get(GameRow, gone.id).deleted_at = datetime.now(timezone.utc)
        with db.session() as s:
            self.assertEqual(access.visible(s, GameRow, s.get(User, self.other.id)), [])
            self.assertEqual(len(access.visible(s, GameRow, s.get(User, self.owner_id))), 1)

    def test_owned_items_list_by_their_own_kind(self):
        """An owned item's ACL entries carry its kind, so an entry for a report never lists a map."""
        with db.session() as s:
            report = OwnedItem(kind="report", ref="r1", owner_id=self.owner_id, visibility="allowlist")
            amap = OwnedItem(kind="map", ref="m1", owner_id=self.owner_id, visibility="allowlist")
            s.add_all([report, amap])
            s.flush()
            access.grant(s, report, s.get(User, self.reader.id), access.VIEW, actor=s.get(User, self.owner_id))
        with db.session() as s:
            listed = access.visible(s, OwnedItem, s.get(User, self.reader.id))
            self.assertEqual([(r.kind, r.ref) for r in listed], [("report", "r1")])

    # ---------------------------------------------------------------- publishing
    def test_global_needs_publish_global_and_server_needs_nothing(self):
        game = self._game("private")
        with db.session() as s:
            row, owner = s.get(GameRow, game.id), s.get(User, self.owner_id)
            with self.assertRaises(HTTPException) as caught:
                access.set_visibility(s, row, "global", actor=owner)
            self.assertEqual(caught.exception.status_code, 403)
            access.set_visibility(s, row, "server", actor=owner)
            self.assertEqual(row.visibility, "server")
            access.set_visibility(s, row, "link", actor=owner)
            owner.caps = {"publish_global": True}
            access.set_visibility(s, row, "global", actor=owner)
            self.assertEqual(row.visibility, "global")

    def test_probation_never_publishes_globally(self):
        game = self._game("private")
        with db.session() as s:
            row = s.get(GameRow, game.id)
            row.owner_id = self.newbie.id
            newbie = s.get(User, self.newbie.id)
            newbie.caps = {"publish_global": True}
            with self.assertRaises(HTTPException) as caught:
                access.set_visibility(s, row, "global", actor=newbie)
            self.assertEqual(caught.exception.status_code, 403)
            access.set_visibility(s, row, "server", actor=newbie)

    def test_admin_can_publish(self):
        game = self._game("private")
        with db.session() as s:
            access.set_visibility(s, s.get(GameRow, game.id), "global", actor=s.get(User, self.admin.id))
            self.assertEqual(s.get(GameRow, game.id).visibility, "global")

    def test_check_visibility_refuses_public_and_anything_off_the_ladder(self):
        for bad in ("public", "Global", "", None, "everyone"):
            with self.subTest(value=bad):
                with self.assertRaises(access.BadVisibility) as caught:
                    access.check_visibility(bad)
                # both kinds of caller: an HTTP 400 and a ValueError
                self.assertIsInstance(caught.exception, ValueError)
                self.assertEqual(caught.exception.status_code, 400)
        for good in access.VISIBILITY:
            self.assertEqual(access.check_visibility(good), good)
        with db.session() as s:
            with self.assertRaises(access.BadVisibility):
                access.set_visibility(s, s.get(GameRow, self._game().id), "public", actor=s.get(User, self.admin.id))

    def test_share_url_carries_the_key_for_link_only(self):
        game = self._game("private")
        self.assertIsNone(access.share_url(game, "https://citar.test"))
        game.visibility = "allowlist"
        self.assertIsNone(access.share_url(game, "https://citar.test"))
        game.visibility = "link"
        self.assertEqual(access.share_url(game, "https://citar.test/"),
                         f"https://citar.test/#/game/{game.id}?k={game.slug}")
        for visibility in ("server", "global"):
            game.visibility = visibility
            self.assertEqual(access.share_url(game, "https://citar.test"), f"https://citar.test/#/game/{game.id}")

    # ---------------------------------------------------------------- roles and statuses
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

    def test_global_game_denies_play_with_403_once_viewable(self):
        game = self._game("global")
        with self.assertRaises(HTTPException) as caught:
            self._perms(self.other, game).require(access.PLAY)
        self.assertEqual(caught.exception.status_code, 403)

    def test_rotating_the_slug_kills_old_links(self):
        game = self._game("link")
        old = game.slug
        with db.session() as s:
            row = s.get(GameRow, game.id)
            access.rotate_slug(s, row, actor=s.get(User, self.owner_id))
            new = row.slug
        self.assertNotEqual(old, new)
        self.assertFalse(self._perms(None, game, slug=old))
        self.assertIn(access.VIEW, self._perms(None, game, slug=new))

    def test_revoking_removes_access(self):
        game = self._game("allowlist")
        self._share(game, self.other, access.PLAY)
        self.assertIn(access.PLAY, self._perms(self.other, game))
        with db.session() as s:
            access.revoke(s, s.get(GameRow, game.id), s.get(User, self.other.id), actor=s.get(User, self.owner_id))
        self.assertFalse(self._perms(self.other, game))

    def test_suspended_owner_loses_access_to_their_own_game(self):
        game = self._game("private")
        with db.session() as s:
            s.get(User, self.owner_id).status = "suspended"
        self.assertFalse(self._perms(self.owner, game))

    def test_seat_holder_plays_without_an_account(self):
        # An LLM or MCP player holds a seat token and has no account at all.
        game = self._game("private")
        with db.session() as s:
            perms = access.on(s, None, s.get(GameRow, game.id), seat_holder=True)
        self.assertEqual(perms.permissions, {V, P})

    def test_deleted_game_is_gone_for_its_owner(self):
        from datetime import datetime, timezone
        game = self._game("private")
        with db.session() as s:
            s.get(GameRow, game.id).deleted_at = datetime.now(timezone.utc)
        self.assertFalse(self._perms(self.owner, game))

    def test_unpublish_makes_it_private_rotates_the_link_and_keeps_the_reason(self):
        game = self._game("global")
        with db.session() as s:
            row = s.get(GameRow, game.id)
            access.unpublish(s, row, actor=s.get(User, self.mod.id), reason="spam")
        with db.session() as s:
            row = s.get(GameRow, game.id)
            self.assertEqual(row.visibility, "private")
            self.assertNotEqual(row.slug, game.slug)
            seen = access.last_unpublished(s, row)
            self.assertEqual((seen["by"], seen["reason"]), ("a moderator", "spam"))
            self.assertEqual(access.last_unpublished(s, row, self.mod.id)["by"], "you")
            # published again, the old unpublish is no longer the item's news
            access.set_visibility(s, row, "server", actor=s.get(User, self.owner_id))
            s.flush()
            self.assertIsNone(access.last_unpublished(s, row))

    def test_who_unpublished_is_told_as_a_role_never_a_handle(self):
        for actor, viewer, said in ((self.admin, self.owner_id, "an administrator"),
                                    (self.owner, self.admin.id, "its owner"),
                                    (self.owner, self.owner_id, "you"),
                                    (self.reader, self.owner_id, "someone who manages it")):
            with self.subTest(actor=actor.handle, viewer=viewer):
                game = self._game("global")
                with db.session() as s:
                    row = s.get(GameRow, game.id)
                    access.unpublish(s, row, actor=s.get(User, actor.id), reason="r")
                    s.flush()
                    seen = access.last_unpublished(s, row, viewer)
                self.assertEqual(seen["by"], said)
                self.assertEqual(set(seen), {"at", "by", "reason"})     # nothing else of the actor

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
            owner = s.get(User, self.owner_id)
            perms = access.on(s, owner, row)          # snapshot taken while still private
            access.set_visibility(s, row, "link", actor=owner)
            payload = ownership.to_client(row, perms, base_url="https://citar.test")
        self.assertEqual(payload["visibility"], "link")
        self.assertIn("share_url", payload)


class VisibilityScan(unittest.TestCase):
    """No visibility string outside the ladder anywhere in citar/ (P3.2.3).

    A grep for "public" would fail on `User.public()`, `public_server` and `INTENTIONALLY_PUBLIC`; this
    looks only where a visibility is set or compared: assignments to a `visibility` name or attribute,
    `visibility=` keywords and parameter defaults, comparisons with a visibility (`getattr(x, "visibility")`
    included), and dict entries under a "visibility" key."""

    @staticmethod
    def _is_visibility(node) -> bool:
        if isinstance(node, ast.Name):
            return node.id == "visibility"
        if isinstance(node, ast.Attribute):
            return node.attr == "visibility"
        return (isinstance(node, ast.Call) and isinstance(node.func, ast.Name) and node.func.id == "getattr"
                and len(node.args) >= 2 and isinstance(node.args[1], ast.Constant)
                and node.args[1].value == "visibility")

    @classmethod
    def _strings(cls, node):
        if isinstance(node, ast.Constant) and isinstance(node.value, str):
            yield node
        elif isinstance(node, (ast.Tuple, ast.List, ast.Set)):
            for elt in node.elts:
                yield from cls._strings(elt)

    def stray(self, source: str) -> list:
        """The visibility strings of ``source`` that are not on the ladder, with their lines."""
        out = []
        for node in ast.walk(ast.parse(source)):
            found = []
            if isinstance(node, ast.Assign) and any(self._is_visibility(t) for t in node.targets):
                found += self._strings(node.value)
            elif isinstance(node, ast.AnnAssign) and node.value is not None and self._is_visibility(node.target):
                found += self._strings(node.value)
            elif isinstance(node, ast.keyword) and node.arg == "visibility":
                found += self._strings(node.value)
            elif isinstance(node, ast.arguments):
                positional = node.posonlyargs + node.args
                pairs = list(zip(positional[len(positional) - len(node.defaults):], node.defaults))
                pairs += [(a, d) for a, d in zip(node.kwonlyargs, node.kw_defaults) if d is not None]
                for arg, default in pairs:
                    if arg.arg == "visibility":
                        found += self._strings(default)
            elif isinstance(node, ast.Compare):
                sides = [node.left, *node.comparators]
                if any(self._is_visibility(x) for x in sides):
                    for x in sides:
                        found += self._strings(x)
            elif isinstance(node, ast.Dict):
                for key, value in zip(node.keys, node.values):
                    if isinstance(key, ast.Constant) and key.value == "visibility" and value is not None:
                        found += self._strings(value)
            out += [(c.lineno, c.value) for c in found if c.value not in access.VISIBILITY]
        return out

    def test_the_scan_catches_each_kind_of_site(self):
        planted = ('row.visibility = "public"\n'
                   'register(s, g, visibility="public")\n'
                   'def f(visibility="public"): pass\n'
                   'class B:\n    visibility: str = "public"\n'
                   'ok = row.visibility in ("link", "public")\n'
                   'ok = getattr(o, "visibility", "") == "public"\n'
                   'd = {"visibility": "public"}\n'
                   'row.visibility = "server"\n')
        self.assertEqual(sorted(line for line, _ in self.stray(planted)), [1, 2, 3, 5, 6, 7, 8])

    def test_no_visibility_constant_outside_the_ladder(self):
        bad = []
        for path in sorted((ROOT / "citar").rglob("*.py")):
            bad += [f"{path.relative_to(ROOT)}:{line}: {value!r}"
                    for line, value in self.stray(path.read_text(encoding="utf-8"))]
        self.assertEqual(bad, [])


class UserPublic(unittest.TestCase):
    """Gate 4: how an account appears to others carries no role or status; its owner still sees both."""

    def setUp(self):
        _reset()

    def test_public_has_no_role_or_status_and_account_has_both(self):
        with db.session() as s:
            user = _user(s, "quiet", role="moderator")
            public = user.public()
            account = user.account()
        self.assertEqual(set(public), {"id", "handle", "display_name", "avatar_url"})
        self.assertEqual((account["role"], account["status"]), ("moderator", "active"))
        self.assertEqual(public, {k: v for k, v in account.items() if k in public})

    def test_auth_me_still_gives_the_owner_role_and_status(self):
        from fastapi.testclient import TestClient
        from citar.auth import sessions
        from citar.server.app import app
        with db.session() as s:
            user = _user(s, "itsme", role="moderator")
            _row, raw = sessions.start(s, user)
        client = TestClient(app, base_url="https://citar.test")       # no startup: the module's database as it is
        try:
            me = client.get("/api/auth/me", headers={"cookie": f"{sessions.COOKIE_NAME}={raw}"}).json()
        finally:
            client.close()
        self.assertEqual((me["user"]["role"], me["user"]["status"]), ("moderator", "active"))

    def test_sharing_lists_people_without_their_role(self):
        with db.session() as s:
            owner, mod = _user(s, "owner"), _user(s, "maggie", role="moderator")
            row = GameRow(id="g-shared", owner_id=owner.id, visibility="allowlist")
            s.add(row)
            s.flush()
            access.grant(s, row, mod, access.VIEW, actor=owner)
            entry = access.shared_with(s, row)[0]
        self.assertNotIn("role", entry["user"])
        self.assertNotIn("status", entry["user"])


def sess_dir():
    from citar.server import session
    return session.SAVE_DIR


class GameRoutes(unittest.TestCase):
    """The same rules at the HTTP layer, for routes that once skipped the gate, and the unpublish route.

    Both of the first answered anybody who knew the game id: the id is eight hex digits and appears in
    share links and URLs, so "knowing the id" is no protection at all.
    """

    def setUp(self):
        from fastapi.testclient import TestClient
        from citar.auth import sessions
        from citar.server import ownership
        from citar.server.app import app, manager

        _reset()
        self.manager = manager
        self.client = TestClient(app, base_url="https://citar.test")
        self.headers = {}
        with db.session() as s:
            for handle, role in (("owner", "user"), ("other", "user"), ("maggie", "moderator"), ("zara", "admin"),
                                 ("reader", "user")):
                user = _user(s, handle, role=role)
                row, raw = sessions.start(s, user)
                self.headers[handle] = {"cookie": f"{sessions.COOKIE_NAME}={raw}", sessions.CSRF_HEADER: row.csrf_token,
                                        "origin": "https://citar.test"}
                if handle == "owner":
                    owner = user
            # Two bots and not started: nothing runs, and with no humans god view is allowed.
            self.game = manager.create({"map_size": "duel", "seed": 3, "barbarians": "off"},
                                       [{"type": "bot"}, {"type": "bot"}], "Private game",
                                       track=False, start=False)
            self.row = ownership.register(s, self.game, owner, visibility="private")
            # an ACL viewer: may look, may change nothing
            access.grant(s, self.row, accounts.by_handle(s, "reader"), access.VIEW, actor=owner)

    def tearDown(self):
        self.manager.delete(self.game.id)
        self.client.close()

    # ---------------------------------------------------------------- the routes 3-01 added or changed (P3.9.1)
    def test_each_changed_route_by_stranger_acl_viewer_owner_admin_and_anonymous(self):
        """A private game: another member gets 404 for reads and writes, an anonymous caller 401 or 404, an ACL
        viewer reads (200) and may write nothing (403), the owner and an administrator 200. DELETE (close) runs
        last, on games of its own."""
        gid = self.game.id
        routes = [
            ("read", "GET", f"/api/games/{gid}/sharing", None),
            ("write", "POST", f"/api/games/{gid}/unpublish", {"reason": "test"}),
            ("write", "POST", f"/api/games/{gid}/seats/1", {"name": "renamed"}),
            ("write", "POST", "/api/scenario-editor", {"source": "game", "game_id": gid}),
            ("write", "POST", "/api/scenario-editor", {"source": "save", "save": f"{gid}/kept.citar"}),
            ("write", "POST", "/api/saves/delete", {"path": f"{gid}/kept.citar"}),
        ]
        expect = {"read": {None: (404,), "other": (404,), "reader": (200,), "owner": (200,), "zara": (200,)},
                  "write": {None: (401, 404), "other": (404,), "reader": (403,), "owner": (200,), "zara": (200,)}}
        for who in (None, "other", "reader", "zara", "owner"):
            self.game.save("kept")                       # the owner and the admin each delete their own copy
            for kind, method, path, body in routes:
                with self.subTest(who=who or "anonymous", route=f"{method} {path} {body}"):
                    r = self.client.request(method, path, json=body, headers=self.headers.get(who, {}))
                    self.assertIn(r.status_code, expect[kind][who], r.text)
        for who, want in ((None, (401, 404)), ("other", (404,)), ("reader", (403,)), ("owner", (200,)),
                          ("zara", (200,))):
            with self.subTest(close=who or "anonymous"):
                r = self.client.delete(f"/api/games/{gid}", headers=self.headers.get(who, {}))
                self.assertIn(r.status_code, want, r.text)
                if r.status_code == 200:          # closed: put it back for the next caller
                    self.manager.load(sess_dir() / gid / "autosave.citar")
                    self._set_visibility("private")

    def test_loading_a_save_by_stranger_acl_viewer_owner_admin_and_anonymous(self):
        """``/api/saves/load`` (3-01 made it give the session its owner): another member and an anonymous caller get
        404, an ACL viewer 403, the owner and an administrator 200; the game stays its owner's whoever loads it."""
        gid = self.game.id
        self.game.save("kept")
        owner_id = self.row.owner_id
        for who, want in ((None, (401, 404)), ("other", (404,)), ("reader", (403,)), ("owner", (200,)),
                          ("zara", (200,))):
            with self.subTest(load=who or "anonymous"):
                r = self.client.post("/api/saves/load", json={"path": f"{gid}/kept.citar"},
                                     headers=self.headers.get(who, {}))
                self.assertIn(r.status_code, want, r.text)
                s = self.manager.get(gid)
                self.assertEqual(s.owner_id if r.status_code == 200 else owner_id, owner_id)
                with db.session() as sdb:
                    row = sdb.get(GameRow, gid)
                    self.assertEqual((row.owner_id, row.status), (owner_id, "playing"))

    def test_launching_a_scenario_by_anonymous_member_owner_and_admin(self):
        """``/api/scenarios/{sid}/launch`` (3-01 made it a lobby game): anonymous 401; a member 200, owning the game it
        made, and 429 over their game limit; an administrator 200. The scenario is the administrator's (3-02), shared
        with members (`server`): while it is private a member's launch is a 404."""
        from citar import engine_api
        r = self.client.post("/api/scenario-editor", json={"source": "generate", "players": [{"type": "bot"}, {"type": "bot"}],
                                                            "config": {"map_size": "duel", "seed": 3, "barbarians": "off"}},
                             headers=self.headers["zara"])
        self.assertEqual(r.status_code, 200, r.text)
        r = self.client.post(f"/api/scenario-editor/{r.json()['editor_id']}/save", json={"name": "launch table"},
                             headers=self.headers["zara"])
        self.assertEqual(r.status_code, 200, r.text)
        sid = r.json()["id"]
        made = []
        try:
            self.assertEqual(self.client.post(f"/api/scenarios/{sid}/launch", json={}).status_code, 401)
            r = self.client.post(f"/api/scenarios/{sid}/launch", json={}, headers=self.headers["other"])
            self.assertEqual(r.status_code, 404, r.text)
            r = self.client.put(f"/api/items/scenario/{sid}/sharing", json={"visibility": "server"},
                                headers=self.headers["zara"])
            self.assertEqual(r.status_code, 200, r.text)
            with db.session() as s:
                accounts.by_handle(s, "other").max_concurrent_games = 1
            for who, want in (("other", 200), ("other", 429), ("owner", 200), ("zara", 200)):
                with self.subTest(launch=who):
                    r = self.client.post(f"/api/scenarios/{sid}/launch", json={"name": f"{who}'s"},
                                         headers=self.headers[who])
                    self.assertEqual(r.status_code, want, r.text)
                    if r.status_code == 200:
                        gid = r.json()["id"]
                        made.append(gid)
                        with db.session() as s:
                            row = s.get(GameRow, gid)
                            user = accounts.by_handle(s, who)
                            self.assertEqual((row.owner_id, row.kind, row.status), (user.id, "game", "playing"))
                        self.assertEqual(self.manager.get(gid).owner_id, user.id)
        finally:
            for gid in made:
                self.manager.delete(gid)
            engine_api.delete_scenario(sid)

    def test_watching_and_replaying_with_no_token(self):
        """3-03's view and replay routes (P3.9.1): an AI-only game is watched and replayed with no token by whoever
        the ladder lets see it, the view saying whether they manage it (``can_manage``, the screen's controls); a
        stranger and a visitor get 404, a member 404 too until it is shared with members, then 200, and a visitor
        holding a link's key 200."""
        gid = self.game.id
        for who, want, manages in ((None, 404, None), ("other", 404, None), ("maggie", 404, None),
                                   ("reader", 200, False), ("owner", 200, True), ("zara", 200, True)):
            with self.subTest(who=who or "anonymous"):
                view = self.client.get(f"/api/games/{gid}/view", headers=self.headers.get(who, {}))
                replay = self.client.get(f"/api/games/{gid}/replay", headers=self.headers.get(who, {}))
                self.assertEqual((view.status_code, replay.status_code), (want, want), view.text[:200])
                if want == 200:
                    self.assertIs(view.json()["can_manage"], manages)
        # a moderator on a `global` game (P3.9.1): reads 200, managing nothing, and control 403; a visitor reads too
        self._set_visibility("global")
        for who in ("maggie", None):
            with self.subTest(who=who or "anonymous", visibility="global"):
                view = self.client.get(f"/api/games/{gid}/view", headers=self.headers.get(who, {}))
                replay = self.client.get(f"/api/games/{gid}/replay", headers=self.headers.get(who, {}))
                self.assertEqual((view.status_code, replay.status_code), (200, 200), view.text[:200])
                self.assertIs(view.json()["can_manage"], False)
        self.assertEqual(self._post("/control", "maggie", {"paused": True}).status_code, 403)
        self.assertIn(self._post("/control", None, {"paused": True}).status_code, (401, 403))
        self._set_visibility("server")
        self.assertEqual(self.client.get(f"/api/games/{gid}/view", headers=self.headers["other"]).status_code, 200)
        self.assertEqual(self.client.get(f"/api/games/{gid}/replay", headers=self.headers["other"]).status_code, 200)
        self.assertEqual(self.client.get(f"/api/games/{gid}/view").status_code, 404)
        self._set_visibility("link")
        with db.session() as s:
            key = s.get(GameRow, gid).slug
        self.assertEqual(self.client.get(f"/api/games/{gid}/view", headers={"x-citar-share-key": key}).status_code,
                         200)
        self.assertEqual(self.client.get(f"/api/games/{gid}/replay", params={"k": key}).status_code, 200)

    def test_creating_and_launching_need_an_account(self):
        for path, body in (("/api/games", {"seats": [{"type": "bot"}]}),
                           ("/api/scenarios/none/launch", {}),
                           ("/api/scenario-editor/none/meta", {}), ("/api/scenario-editor/none/save", {})):
            with self.subTest(path=path):
                self.assertEqual(self.client.post(path, json=body).status_code, 401)

    def _get(self, path, who=None, **params):
        return self.client.get(f"/api/games/{self.game.id}{path}", params=params,
                               headers=self.headers.get(who, {}))

    def _post(self, path, who=None, body=None):
        return self.client.post(f"/api/games/{self.game.id}{path}", json=body or {}, headers=self.headers.get(who, {}))

    def _set_visibility(self, visibility):
        with db.session() as s:
            s.get(GameRow, self.game.id).visibility = visibility

    # ---------------------------------------------------------------- unpublish
    def test_a_moderator_unpublishes_a_global_game_and_controls_nothing(self):
        self._set_visibility("global")
        old_slug = self.row.slug
        # every control route is the owner's, not the moderator's
        controls = (("POST", "/control", {"paused": True}), ("POST", "/seats/0", {"type": "bot"}),
                    ("PUT", "/sharing", {"visibility": "private"}), ("DELETE", "", None))
        for method, path, body in controls:
            with self.subTest(route=method + " " + (path or "/")):
                r = self.client.request(method, f"/api/games/{self.game.id}{path}", json=body,
                                        headers=self.headers["maggie"])
                self.assertEqual(r.status_code, 403, r.text)
        self.assertEqual(self._get("/sharing", "maggie").status_code, 200)
        r = self._post("/unpublish", "maggie", {"reason": "spam in the game's name"})
        self.assertEqual(r.status_code, 200, r.text)
        with db.session() as s:
            row = s.get(GameRow, self.game.id)
            self.assertEqual(row.visibility, "private")
            self.assertNotEqual(row.slug, old_slug)
            audits = list(s.scalars(select(AuditLog).where(AuditLog.action == "game.unpublished")))
        self.assertEqual(len(audits), 1)
        self.assertEqual(audits[0].detail["reason"], "spam in the game's name")
        self.assertEqual(audits[0].actor_label, "maggie (moderator)")
        # private now: the moderator has no way back in, to anything
        for method, path, body in controls + (("POST", "/unpublish", {}),):
            with self.subTest(after=method + " " + (path or "/")):
                r = self.client.request(method, f"/api/games/{self.game.id}{path}", json=body,
                                        headers=self.headers["maggie"])
                self.assertEqual(r.status_code, 404, r.text)
        # the owner sees that a moderator took it down, and why; never which moderator
        sharing = self._get("/sharing", "owner").json()
        seen = sharing["unpublished"]
        self.assertEqual((seen["by"], seen["reason"]), ("a moderator", "spam in the game's name"))
        self.assertNotIn("maggie", repr(sharing))

    def test_on_a_server_game_a_moderator_is_a_member(self):
        self._set_visibility("server")
        for who in ("maggie", "other"):
            with self.subTest(who=who):
                self.assertEqual(self._get("", who).status_code, 200)
                self.assertEqual(self._post("/unpublish", who).status_code, 403)
                self.assertEqual(self._post("/control", who, {"paused": True}).status_code, 403)

    def test_unpublish_by_owner_admin_stranger_and_anonymous(self):
        self.assertEqual(self._post("/unpublish", "other").status_code, 404)
        self.assertIn(self._post("/unpublish").status_code, (401, 404))
        for who in ("owner", "zara"):
            with self.subTest(who=who):
                self._set_visibility("link")
                self.assertEqual(self._post("/unpublish", who).status_code, 200)

    def test_a_moderator_holding_the_link_may_unpublish_it(self):
        self._set_visibility("link")
        r = self.client.post(f"/api/games/{self.game.id}/unpublish", json={"reason": "leaked"},
                             headers={**self.headers["maggie"], "x-citar-share-key": self.row.slug})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(self._post("/unpublish", "maggie").status_code, 404)    # without it: nothing

    # ---------------------------------------------------------------- debug/errors
    def test_stranger_gets_404_on_debug_errors(self):
        for who in (None, "other"):
            with self.subTest(who=who or "anonymous"):
                self.assertEqual(self._get("/debug/errors", who).status_code, 404)

    def test_seat_and_spectator_tokens_do_not_open_debug_errors(self):
        """A token grants view, so this is a 403 rather than a 404 — but it is still a refusal."""
        for token in (self.game.seats[0].token, self.game.spectator_token):
            self.assertEqual(self._get("/debug/errors", token=token).status_code, 403)

    def test_viewer_of_a_global_game_cannot_read_debug_errors(self):
        """Viewing is not enough: the errors carry what each seat was trying to do."""
        self._set_visibility("global")
        self.assertEqual(self._get("/debug/errors", "other").status_code, 403)

    def test_owner_and_admin_read_debug_errors(self):
        for who in ("owner", "zara"):
            with self.subTest(who=who):
                response = self._get("/debug/errors", who)
                self.assertEqual(response.status_code, 200, response.text)
                self.assertIn("errors", response.json())
                self.assertEqual(set(response.json()["save_lock"]), {"saves", "total_s", "max_s", "last_s"})

    # ---------------------------------------------------------------- the listing
    def test_the_games_list_follows_the_ladder(self):
        def listed(who):
            r = self.client.get("/api/games", headers=self.headers.get(who, {}))
            return self.game.id in [g["id"] for g in r.json()]
        self.assertEqual([listed(w) for w in (None, "other", "owner")], [False, False, True])
        self._set_visibility("server")
        self.assertEqual([listed(w) for w in (None, "other", "owner")], [False, True, True])
        self._set_visibility("global")
        self.assertEqual([listed(w) for w in (None, "other", "owner")], [True, True, True])

    # ---------------------------------------------------------------- replay
    @unittest.skipUnless(has_test_ops(), "needs the engine's test operations (a test-ops build)")
    def test_stranger_gets_404_on_replay_of_a_finished_private_game(self):
        self.game.game.test_ops([{"op": "end_game"}])
        for who in (None, "other"):
            with self.subTest(who=who or "anonymous"):
                self.assertEqual(self._get("/replay", who).status_code, 404)

    @unittest.skipUnless(has_test_ops(), "needs the engine's test operations (a test-ops build)")
    def test_viewers_read_the_replay_of_a_finished_game(self):
        self.game.game.test_ops([{"op": "end_game"}])
        for who in ("owner", "zara"):
            with self.subTest(who=who):
                self.assertEqual(self._get("/replay", who).status_code, 200)
        self.assertEqual(self._get("/replay", token=self.game.seats[0].token).status_code, 200)
        self._set_visibility("global")
        self.assertEqual(self._get("/replay").status_code, 200)

    def test_spectator_token_still_opens_the_replay_of_a_live_ai_only_game(self):
        self.assertEqual(self.game.game.phase, "playing")
        response = self._get("/replay", token=self.game.spectator_token)
        self.assertEqual(response.status_code, 200, response.text)
        self.assertEqual(self._get("/replay").status_code, 404)

    # ---------------------------------------------------------------- view as_player
    def test_spectators_of_an_ai_only_game_may_look_as_any_player(self):
        for as_player in (0, 1):
            with self.subTest(as_player=as_player):
                response = self._get("/view", token=self.game.spectator_token, as_player=as_player)
                self.assertEqual(response.status_code, 200, response.text)
                self.assertTrue(response.json()["spectator"])

    def test_nobody_looks_through_another_civs_eyes_while_a_human_plays(self):
        """as_player used to skip the god-view rule: a spectator link, or the owner without a seat
        token, could read a human's private view, or an AI's scouted map, mid-game."""
        from citar.server import ownership
        with db.session() as s:
            owner = accounts.by_handle(s, "owner")
            game = self.manager.create({"map_size": "duel", "seed": 5, "barbarians": "off"},
                                       [{"type": "human"}, {"type": "bot"}], "Human game",
                                       track=False, start=False)
            ownership.register(s, game, owner, visibility="private")
        try:
            get = lambda who=None, **params: self.client.get(  # noqa: E731
                f"/api/games/{game.id}/view", params=params, headers=self.headers.get(who, {}))
            # The god view itself is closed mid-game, and as_player follows the same rule.
            self.assertEqual(get("owner").status_code, 403)
            self.assertEqual(get(token=game.spectator_token).status_code, 403)
            for as_player in (0, 1):
                with self.subTest(as_player=as_player):
                    self.assertEqual(get(token=game.spectator_token, as_player=as_player).status_code, 403)
                    self.assertEqual(get("owner", as_player=as_player).status_code, 403)
            # The human's own seat still sees its own view, and as_player cannot widen it.
            response = get(token=game.seats[0].token, as_player=1)
            self.assertEqual(response.status_code, 200, response.text)
            self.assertEqual(response.json()["seat"]["player"], 0)
        finally:
            self.manager.delete(game.id)


if __name__ == "__main__":
    unittest.main()
