"""Profiles, the directory and avatars (design/phase3-features.md P3.2.6; package 3-04, gates 1 and 2).

Gate 1: each profile field at each level shows to exactly the viewers P3.2.6 names (a visitor, a member, a member on
probation, the account itself, an administrator); a hidden `profile` makes the page 404; `listed = false` takes the
account out of the directory only; `global` is refused on probation; the bio's and the links' limits, a `javascript:`
link refused; a private display name and avatar absent wherever the account appears (a `global` game's owner for a
visitor, a sharing list for another member: the handle shows); a directory search for a private display name finds
nothing; the callers of ``.public(`` are the fixed list. Gate 2: the avatar corpus in ``tests/data/avatars/``
accepted or refused as the rules say, served with the type its header proved, ``nosniff`` and the sandboxing policy,
404 when the field is hidden, the previous file removed on replace, nothing written outside ``<data dir>/avatars/``.
Every new route also gets P3.9.1's stranger, anonymous, owner and administrator tests (``Routes``).
"""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import os
import re
import shutil
import struct
import tempfile
import unittest
import zlib
from datetime import timedelta
from pathlib import Path

_TMP = Path(tempfile.mkdtemp(prefix="citar_profiles_"))
os.environ["CITAR_DATA_DIR"] = str(_TMP)
os.environ["CITAR_DB_URL"] = "sqlite:///" + (_TMP / "test.db").as_posix()
os.environ.setdefault("CITAR_MODE", "server")
os.environ.setdefault("CITAR_PUBLIC_ORIGIN", "https://citar.test")
os.environ.setdefault("CITAR_SECRET_KEY", "test-secret-key-that-is-long-enough-to-pass")
os.environ["CITAR_REQUIRE_HTTPS"] = "0"
os.environ["CITAR_BEHIND_PROXY"] = "0"

from citar import db, items
from citar.auth import access, accounts, images, policy, profiles, sessions
from citar.db.models import Base, FIELD_LEVELS, USER_FIELD_DEFAULTS, User

ROOT = Path(__file__).resolve().parent.parent
AVATARS = Path(__file__).resolve().parent / "data" / "avatars"
FIELDS = tuple(USER_FIELD_DEFAULTS)
#: Who sees a field at each level (P3.2.6): visitors `global`, members (probation too) `server` and `global`, the
#: account itself and administrators everything.
SEES = {"private": {"pat", "ada"}, "server": {"max", "newt", "pat", "ada"},
        "global": {None, "max", "newt", "pat", "ada"}}
VIEWERS = (None, "max", "newt", "pat", "ada")
SECRET_NAME = "Patricia Hidden-Name"


def _reset():
    db.dispose()
    db.configure(os.environ["CITAR_DB_URL"])
    Base.metadata.drop_all(db.engine())
    db.create_all()
    policy.invalidate()
    # the data directory as the server finds it (another test module may have set CITAR_DATA_DIR after this one)
    shutil.rmtree(profiles.avatars_dir(), ignore_errors=True)


class Base_(unittest.TestCase):
    """A database with an account (pat) with a display name and a bio, a member (max), a member on probation (newt),
    a moderator (mo) and an administrator (ada), each signed in."""

    @classmethod
    def setUpClass(cls):
        from fastapi.testclient import TestClient
        from citar.server.app import app
        cls.client = TestClient(app, base_url="https://citar.test")

    @classmethod
    def tearDownClass(cls):
        cls.client.close()

    def setUp(self):
        _reset()
        self.headers, self.ids = {None: {}}, {}
        with db.session() as s:
            for handle, role, status in (("pat", "user", "active"), ("max", "user", "active"),
                                         ("newt", "user", "probation"), ("moe", "moderator", "active"),
                                         ("ada", "admin", "active")):
                user = accounts.create_user(s, handle=handle, email=f"{handle}@example.com", role=role, status=status,
                                            email_verified=True, display_name=SECRET_NAME if handle == "pat" else "")
                row, raw = sessions.start(s, user)
                self.headers[handle] = {"cookie": f"{sessions.COOKIE_NAME}={raw}", sessions.CSRF_HEADER: row.csrf_token,
                                        "origin": "https://citar.test"}
                self.ids[handle] = user.id
            pat = s.get(User, self.ids["pat"])
            pat.bio = "Plays wide.\nSecond line."
            pat.links = [{"label": "site", "url": "https://example.com/pat"}]

    def set_levels(self, handle: str, levels: dict) -> None:
        with db.session() as s:
            s.get(User, self.ids[handle]).field_visibility = dict(levels)

    def get(self, path: str, who=None, **kw):
        return self.client.get(path, headers=self.headers[who], **kw)

    def put_me(self, who: str, body: dict):
        return self.client.put("/api/auth/me", json=body, headers=self.headers[who])

    def user(self, handle: str) -> User:
        with db.session() as s:
            return s.get(User, self.ids[handle])


# ---------------------------------------------------------------------------------------------------------- gate 1

class FieldVisibility(Base_):
    """Each field at each level, by each kind of viewer, through the profile route and ``User.public(viewer)``."""

    def test_each_field_at_each_level_shows_to_exactly_its_viewers(self):
        for field in FIELDS:
            for level in FIELD_LEVELS:
                # the field at the level, everything else (the page itself included) open to everyone
                self.set_levels("pat", {**dict.fromkeys(FIELDS, "global"), field: level})
                for who in VIEWERS:
                    with self.subTest(field=field, level=level, viewer=who or "visitor"):
                        r = self.get("/api/users/pat", who)
                        sees = who in SEES[level]
                        if field == "profile":
                            self.assertEqual(r.status_code, 200 if sees else 404, r.text)
                            continue
                        self.assertEqual(r.status_code, 200, r.text)
                        body = r.json()
                        self.assertEqual(field in body["fields"], sees)
                        if field in ("bio", "links", "items"):
                            self.assertEqual(field in body, sees)
                        if field == "bio":
                            self.assertEqual("Plays wide." in r.text, sees)
                        if field == "links":
                            self.assertEqual("example.com/pat" in r.text, sees)
                        if field == "display_name":
                            self.assertEqual(SECRET_NAME in r.text, sees)
                            self.assertEqual(body["display_name"], SECRET_NAME if sees else "pat")

    def test_public_applies_the_display_name_and_avatar_and_the_profile_gate(self):
        with db.session() as s:
            pat = s.get(User, self.ids["pat"])
            pat.avatar_url = "https://images.example/pat.png"
            viewers = {who: (s.get(User, self.ids[who]) if who else None) for who in VIEWERS}
            for level in FIELD_LEVELS:
                for gate in FIELD_LEVELS:
                    pat.field_visibility = {"display_name": level, "avatar": level, "profile": gate}
                    for who, viewer in viewers.items():
                        with self.subTest(level=level, profile=gate, viewer=who or "visitor"):
                            shown = who in SEES[level] and who in SEES[gate]
                            public = pat.public(viewer)
                            self.assertEqual(set(public), {"id", "handle", "display_name", "avatar"})
                            self.assertEqual(public["handle"], "pat")
                            self.assertEqual(public["display_name"], SECRET_NAME if shown else "pat")
                            self.assertEqual(public["avatar"], "https://images.example/pat.png" if shown else None)

    def test_defaults_are_members_only_and_the_ratio_private(self):
        r = self.get("/api/users/pat", "max")
        self.assertEqual(set(r.json()["fields"]), set(FIELDS) - {"ratio"})
        self.assertEqual(self.get("/api/users/pat").status_code, 404, "a visitor: the profile is `server`")
        self.assertIn("ratio", self.get("/api/users/pat", "pat").json()["fields"])

    def test_a_hidden_profile_is_404_alike_for_a_missing_account(self):
        self.set_levels("pat", {"profile": "private"})
        for who in (None, "max", "newt", "moe"):
            r = self.get("/api/users/pat", who)
            self.assertEqual(r.status_code, 404, who)
            self.assertEqual(r.json(), self.get("/api/users/nobody-here", who).json())
        self.assertEqual(self.get("/api/users/pat", "pat").status_code, 200)
        self.assertEqual(self.get("/api/users/pat", "ada").status_code, 200)

    def test_an_account_out_of_good_standing_shows_nothing(self):
        self.set_levels("pat", dict.fromkeys(FIELDS, "global"))
        for status in ("suspended", "pending", "deleted"):
            with db.session() as s:
                s.get(User, self.ids["pat"]).status = status
            with self.subTest(status=status):
                self.assertEqual(self.get("/api/users/pat").status_code, 404)
                self.assertEqual(self.get("/api/users/pat", "max").status_code, 404)
                self.assertEqual(self.get("/api/users/pat", "ada").status_code, 200)

    def test_probation_holds_global_fields_to_members(self):
        """Set while active, then put on probation: a visitor sees nothing more than members-only."""
        self.set_levels("pat", dict.fromkeys(FIELDS, "global"))
        self.assertEqual(self.get("/api/users/pat").status_code, 200)
        with db.session() as s:
            s.get(User, self.ids["pat"]).status = "probation"
        self.assertEqual(self.get("/api/users/pat").status_code, 404)
        self.assertEqual(self.get("/api/users/pat", "max").status_code, 200)

    def test_view_as_narrows_and_never_widens(self):
        self.set_levels("pat", {"profile": "global", "bio": "server", "display_name": "private"})
        mine = self.get("/api/users/pat", "pat").json()
        self.assertEqual((mine["seen_as"], mine["display_name"]), ("you", SECRET_NAME))
        as_member = self.get("/api/users/pat?as=member", "pat").json()
        self.assertEqual((as_member["seen_as"], as_member["display_name"]), ("member", "pat"))
        self.assertIn("bio", as_member)
        as_visitor = self.get("/api/users/pat?as=visitor", "pat").json()
        self.assertEqual(as_visitor["seen_as"], "visitor")
        self.assertNotIn("bio", as_visitor)
        # a visitor asking to be a member is still a visitor
        self.assertNotIn("bio", self.get("/api/users/pat?as=member").json())
        self.assertEqual(self.get("/api/users/pat?as=king", "pat").status_code, 400)
        self.set_levels("pat", {"profile": "server"})
        self.assertEqual(self.get("/api/users/pat?as=visitor", "pat").status_code, 404,
                         "seen as a visitor, a members-only page is not found")

    def test_items_list_only_what_the_viewer_may_see(self):
        from citar.db.models import Game as GameRow
        with db.session() as s:
            pat = s.get(User, self.ids["pat"])
            for gid, vis in (("g-private", "private"), ("g-members", "server"), ("g-world", "global"),
                             ("g-link", "link")):
                s.add(GameRow(id=gid, owner_id=pat.id, name=gid, kind="game", visibility=vis, status="finished"))
            s.add(GameRow(id="g-bench", owner_id=pat.id, name="g-bench", kind="benchmark", visibility="global"))
            items.register(s, "map", "m-world", pat, "World map")
            items.get(s, "map", "m-world").visibility = "global"
            items.register(s, "map", "m-mine", pat, "My map")
            shared = items.register(s, "report", "r-shared", pat, "Shared report")
            access.grant(s, shared, s.get(User, self.ids["max"]), access.VIEW, actor=pat)
        self.set_levels("pat", {"profile": "global", "items": "global"})

        def ids(who, query=""):
            return {i["id"] for i in self.get(f"/api/users/pat{query}", who).json()["items"]}
        self.assertEqual(ids(None), {"g-world", "m-world"})
        self.assertEqual(ids("newt"), {"g-world", "g-members", "m-world"})
        self.assertEqual(ids("max"), {"g-world", "g-members", "m-world", "r-shared"}, "its ACL entry counts")
        everything = {"g-private", "g-members", "g-world", "g-link", "m-world", "m-mine", "r-shared"}
        self.assertEqual(ids("pat"), everything, "benchmark games are their runs', not listed")
        self.assertEqual(ids("ada"), everything)
        self.assertEqual(ids("pat", "?as=member"), {"g-world", "g-members", "m-world"})
        self.assertEqual(ids("pat", "?as=visitor"), {"g-world", "m-world"})


class ProfileEdits(Base_):
    """PUT /api/auth/me: the bio, the links, the levels, the directory switch."""

    def test_the_bio_is_cleaned_and_limited(self):
        r = self.put_me("max", {"bio": "  Hello\x07 there\u202e!\r\nLine two\n\n\n\nfar  "})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(self.user("max").bio, "Hello there!\nLine two\n\nfar")
        self.assertEqual(self.put_me("max", {"bio": "x" * 1000}).status_code, 200)
        r = self.put_me("max", {"bio": "x" * 1001})
        self.assertEqual(r.status_code, 400)
        self.assertIn("1,000", r.json()["detail"])
        self.assertEqual(self.user("max").bio, "x" * 1000)
        self.assertEqual(self.put_me("max", {"bio": ["not", "text"]}).status_code, 400)

    def test_a_new_accounts_display_name_is_cleaned_too(self):
        with db.session() as s:
            user = accounts.create_user(s, handle="spoof", display_name="admin\u202e\x00 team ", status="active")
            self.assertEqual(user.display_name, "admin team")

    def test_links_take_three_web_addresses(self):
        good = [{"label": "a", "url": "https://example.com/a"}, {"label": "", "url": "http://example.org"},
                {"label": "L" * 40, "url": "https://example.net/" + "p" * 270}]
        self.assertEqual(self.put_me("max", {"links": good}).status_code, 200)
        self.assertEqual(self.user("max").links, good)
        bad = {
            "a fourth link": good + [{"label": "d", "url": "https://example.com/d"}],
            "a label of 41": [{"label": "L" * 41, "url": "https://example.com"}],
            "an address of 301": [{"label": "", "url": "https://example.com/" + "p" * 281}],
            "javascript:": [{"label": "x", "url": "javascript:alert(1)"}],
            "JavaScript: in capitals": [{"label": "x", "url": "JavaScript:alert(1)"}],
            "data:": [{"label": "x", "url": "data:text/html,<script>alert(1)</script>"}],
            "ftp:": [{"label": "x", "url": "ftp://example.com/file"}],
            "no host": [{"label": "x", "url": "https:///path"}],
            "a password in it": [{"label": "x", "url": "https://user:pw@example.com"}],
            "a space in it": [{"label": "x", "url": "https://example.com/a b"}],
            "an unknown key": [{"label": "x", "url": "https://example.com", "rel": "me"}],
            "not a list": "https://example.com",
        }
        for why, links in bad.items():
            with self.subTest(why=why):
                self.assertEqual(self.put_me("max", {"links": links}).status_code, 400)
        self.assertEqual(self.user("max").links, good, "nothing refused changed anything")

    def test_levels_known_keys_and_values_and_global_refused_on_probation(self):
        self.assertEqual(self.put_me("max", {"field_visibility": {"bio": "global"}}).status_code, 200)
        self.assertEqual(self.user("max").chosen_levels()["bio"], "global")
        for bad in ({"shoe_size": "server"}, {"bio": "public"}, {"bio": "link"}, "global"):
            with self.subTest(bad=bad):
                self.assertEqual(self.put_me("max", {"field_visibility": bad}).status_code, 400)
        r = self.put_me("newt", {"field_visibility": {"bio": "global"}})
        self.assertEqual(r.status_code, 403)
        self.assertEqual(r.json()["detail"], profiles.PROBATION_GLOBAL)
        self.assertEqual(self.put_me("newt", {"field_visibility": {"bio": "private", "profile": "server"}}).status_code,
                         200, "members-only and private are a new account's too")

    def test_a_field_made_global_before_probation_may_stay_while_the_rest_saves(self):
        """The Account page sends every field: an account put on probation after it made one `global` still saves its
        profile, the field held to members meanwhile; making another field `global` is still refused."""
        self.assertEqual(self.put_me("max", {"field_visibility": {"profile": "global", "bio": "global"}}).status_code, 200)
        with db.session() as s:
            s.get(User, self.ids["max"]).status = "probation"
        everything = {**self.user("max").chosen_levels(), "display_name": "private"}
        r = self.put_me("max", {"bio": "still mine", "field_visibility": everything})
        self.assertEqual(r.status_code, 200, r.text)
        self.assertEqual(self.user("max").bio, "still mine")
        self.assertEqual(self.get("/api/users/max").status_code, 404, "held to members on probation")
        self.assertEqual(self.put_me("max", {"field_visibility": {**everything, "links": "global"}}).status_code, 403)

    def test_a_refused_part_changes_nothing(self):
        r = self.put_me("max", {"bio": "new bio", "display_name": "New Name",
                                "links": [{"label": "x", "url": "javascript:alert(1)"}]})
        self.assertEqual(r.status_code, 400)
        user = self.user("max")
        self.assertEqual((user.bio, user.display_name), ("", ""))

    def test_one_audit_row_names_what_changed(self):
        from citar.db.models import AuditLog
        from sqlalchemy import select
        self.put_me("max", {"bio": "hi", "field_visibility": {"bio": "private"}, "listed": False})
        with db.session() as s:
            rows = list(s.scalars(select(AuditLog).where(AuditLog.action == "user.profile_updated")))
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0].detail["fields"], ["bio", "field_visibility", "listed"])
        self.assertEqual(rows[0].detail["levels"], {"bio": {"from": "server", "to": "private"}})
        self.assertNotIn("hi", str(rows[0].detail), "the text itself is never logged")

    def test_the_account_sees_its_own_profile_settings(self):
        self.put_me("max", {"bio": "b", "field_visibility": {"ratio": "server"}, "listed": False})
        me = self.get("/api/auth/me", "max").json()["user"]
        self.assertEqual((me["bio"], me["listed"], me["field_visibility"]["ratio"]), ("b", False, "server"))
        self.assertEqual(me["field_visibility"]["bio"], "server", "unchosen fields show their default")


class Directory(Base_):
    """GET /api/users: listed accounts in good standing, members only, a search that never reveals a hidden name."""

    def names(self, who="max", q=""):
        r = self.get("/api/users", who, params={"q": q} if q else None)
        self.assertEqual(r.status_code, 200, r.text)
        return [u["handle"] for u in r.json()["users"]]

    def test_members_only(self):
        self.assertEqual(self.get("/api/users").status_code, 401)
        self.assertIn("pat", self.names("newt"))

    def test_listed_false_takes_the_account_out_of_the_directory_only(self):
        self.assertIn("pat", self.names())
        self.assertEqual(self.put_me("pat", {"listed": False}).status_code, 200)
        self.assertNotIn("pat", self.names())
        self.assertNotIn("pat", self.names("ada"))
        self.assertEqual(self.get("/api/users/pat", "max").status_code, 200, "its page still opens")

    def test_accounts_out_of_good_standing_are_not_listed(self):
        with db.session() as s:
            s.get(User, self.ids["pat"]).status = "suspended"
        self.assertNotIn("pat", self.names())
        self.assertIn("newt", self.names(), "probation is good standing")

    def test_a_search_for_a_hidden_name_finds_nothing(self):
        self.set_levels("pat", {"display_name": "private"})
        self.assertEqual(self.names(q="Hidden-Name"), [])
        self.assertNotIn(SECRET_NAME, self.get("/api/users", "max").text)
        self.assertEqual(self.names("pat", q="Hidden-Name"), ["pat"], "the account finds itself")
        self.assertEqual(self.names("ada", q="hidden-name"), ["pat"], "an administrator sees every name")
        self.assertEqual(self.names(q="pat"), ["pat"], "the handle always matches")
        self.set_levels("pat", {"display_name": "server"})
        self.assertEqual(self.names(q="hidden-name"), ["pat"])
        self.set_levels("pat", {"display_name": "server", "profile": "private"})
        self.assertEqual(self.names(q="hidden-name"), [], "a hidden profile hides its name too")
        self.assertEqual(self.names(q="%"), [], "a wildcard is a character")

    def test_cards_carry_what_shows_and_whether_the_page_opens(self):
        self.set_levels("pat", {"bio": "private"})
        card = next(u for u in self.get("/api/users", "max").json()["users"] if u["handle"] == "pat")
        self.assertEqual(card["display_name"], SECRET_NAME)
        self.assertNotIn("bio", card)
        self.assertTrue(card["profile"])
        self.set_levels("pat", {"profile": "private"})
        card = next(u for u in self.get("/api/users", "max").json()["users"] if u["handle"] == "pat")
        self.assertEqual((card["display_name"], card["profile"]), ("pat", False))
        self.set_levels("pat", {})
        card = next(u for u in self.get("/api/users", "max").json()["users"] if u["handle"] == "pat")
        self.assertEqual(card["bio"], "Plays wide.", "the first line")

    def test_newest_first_and_paged_by_fifty(self):
        with db.session() as s:
            made = [accounts.create_user(s, handle=f"page{i:02d}", status="active") for i in range(55)]
            s.flush()
            # a second apart: Windows' clock can give accounts made in one loop the same moment, which the directory
            # orders by id (stable paging, but not this test's "newest")
            base = made[0].created_at
            for i, u in enumerate(made):
                u.created_at = base + timedelta(seconds=i)
        first = self.get("/api/users", "max").json()
        self.assertEqual((len(first["users"]), first["next"]), (50, 50))
        self.assertEqual(first["users"][0]["handle"], "page54")
        rest = self.get("/api/users", "max", params={"offset": 50}).json()
        self.assertIsNone(rest["next"])
        handles = [u["handle"] for u in first["users"] + rest["users"]]
        self.assertEqual(len(handles), len(set(handles)))
        self.assertEqual(set(handles), {f"page{i:02d}" for i in range(55)} | {"pat", "max", "newt", "moe", "ada"})

    def test_an_offset_out_of_range_is_refused_never_handed_to_the_database(self):
        """SQLite takes a 64-bit integer: a larger offset raised OverflowError (a 500) for any member. Now 422, and
        ``profiles.directory`` holds its offset in range itself."""
        for offset in ("99999999999999999999999", str(2 ** 63), "-1", str(profiles.OFFSET_MAX + 1)):
            with self.subTest(offset=offset):
                r = self.get("/api/users", "max", params={"offset": offset})
                self.assertEqual(r.status_code, 422, r.text)
                self.assertEqual(self.get("/api/users", params={"offset": offset}).status_code, 401)
        r = self.get("/api/users", "max", params={"offset": profiles.OFFSET_MAX})
        self.assertEqual((r.status_code, r.json()["users"], r.json()["next"]), (200, [], None))
        with db.session() as s:
            viewer = s.get(User, self.ids["max"])
            self.assertEqual(profiles.directory(s, viewer, offset=2 ** 70)["offset"], profiles.OFFSET_MAX)
            self.assertEqual(len(profiles.directory(s, viewer, offset=-(2 ** 70))["users"]), 5)


class WhereverTheAccountAppears(Base_):
    """A private display name and avatar never reach anyone the account does not show them to: the owner of a
    `global` game for a visitor, a sharing list for another member, an item's owner, the inviter. The handle shows."""

    def setUp(self):
        super().setUp()
        with db.session() as s:
            s.get(User, self.ids["pat"]).avatar_url = "https://images.example/secret-face.png"
        self.set_levels("pat", {"display_name": "private", "avatar": "private", "profile": "global"})

    def assert_hidden(self, text: str, entry: dict):
        self.assertNotIn(SECRET_NAME, text)
        self.assertNotIn("secret-face", text)
        self.assertEqual((entry["handle"], entry["display_name"], entry["avatar"]), ("pat", "pat", None))

    def test_the_owner_of_a_global_game_for_a_visitor(self):
        from citar.server import ownership
        from citar.server.app import manager
        with db.session() as s:
            game = manager.create({"map_size": "duel", "seed": 4, "barbarians": "off"},
                                  [{"type": "bot"}, {"type": "bot"}], "World game", track=False, start=False)
            ownership.register(s, game, s.get(User, self.ids["pat"]), visibility="global")
        try:
            for path in (f"/api/games/{game.id}", "/api/games"):
                with self.subTest(path=path):
                    r = self.get(path)
                    self.assertEqual(r.status_code, 200, r.text)
                    info = r.json() if path != "/api/games" else next(g for g in r.json() if g["id"] == game.id)
                    self.assert_hidden(r.text, info["sharing"]["owner"])
            mine = self.get(f"/api/games/{game.id}", "pat").json()["sharing"]["owner"]
            self.assertEqual(mine["display_name"], SECRET_NAME)
        finally:
            manager.delete(game.id)

    def test_a_sharing_list_for_another_member(self):
        from citar.server import ownership
        from citar.server.app import manager
        with db.session() as s:
            game = manager.create({"map_size": "duel", "seed": 5, "barbarians": "off"},
                                  [{"type": "bot"}, {"type": "bot"}], "Max's game", track=False, start=False)
            ownership.register(s, game, s.get(User, self.ids["max"]), visibility="private")
            report = items.register(s, "report", "r-max", s.get(User, self.ids["max"]), "Max's report")
            del report
        try:
            add = self.client.post(f"/api/games/{game.id}/sharing/users", json={"handle": "pat", "permission": "view"},
                                   headers=self.headers["max"])
            self.assertEqual(add.status_code, 200, add.text)
            self.assert_hidden(add.text, add.json()["shared_with"][0]["user"])
            got = self.get(f"/api/games/{game.id}/sharing", "max")
            self.assert_hidden(got.text, got.json()["shared_with"][0]["user"])
            add = self.client.post("/api/items/report/r-max/sharing/users", json={"handle": "pat", "permission": "view"},
                                   headers=self.headers["max"])
            self.assertEqual(add.status_code, 200, add.text)
            got = self.get("/api/items/report/r-max/sharing", "max")
            self.assert_hidden(got.text, got.json()["shared_with"][0]["user"])
            seen = self.get("/api/items/report/r-max/sharing", "ada").json()["shared_with"][0]["user"]
            self.assertEqual(seen["display_name"], SECRET_NAME, "an administrator sees every name")
        finally:
            manager.delete(game.id)

    def test_an_items_owner(self):
        with db.session() as s:
            row = items.register(s, "map", "m-pat", s.get(User, self.ids["pat"]), "Pat's map")
            row.visibility = "server"
        got = self.get("/api/items/map/m-pat/sharing", "pat").json()
        self.assertEqual(got["owner"]["display_name"], SECRET_NAME, "the owner sees their own name")
        with db.session() as s:
            row = items.get(s, "map", "m-pat")
            client = items.client(s, row, access.on(s, s.get(User, self.ids["max"]), row), s.get(User, self.ids["max"]))
        self.assertEqual((client["owner"]["display_name"], client["owner"]["avatar"]), ("pat", None))

    def test_the_inviter_named_to_whoever_holds_the_code(self):
        from citar.auth import invites
        with db.session() as s:
            pat = s.get(User, self.ids["pat"])
            pat.caps = {"invite": True}
            pat.invite_quota = 1
            code = invites.create(s, pat, role="user").code
        r = self.get("/api/invites/check", params={"code": code})
        self.assertEqual(r.json()["invited_by"], "pat")
        self.assertNotIn(SECRET_NAME, r.text)


class PublicCallers(unittest.TestCase):
    """Every ``.public(`` in ``citar/``: an account's names a viewer (``User.public(viewer)``, P3.2.6), and the rest
    are a seat's or a machine's own. A new line here fails until it is read and added, so a new place an account
    appears cannot forget its viewer."""

    FIXED = {
        ("citar/auth/access.py", 'entry = out.setdefault(user.id, {"user": user.public(viewer), "permissions": []})'),
        ("citar/db/models.py", "d = self.public(self, rank=_SELF)"),
        ("citar/auth/profiles.py", "entry = user.public(viewer, rank=rank)"),
        ("citar/auth/profiles.py", 'out = {**user.public(viewer, rank=rank), "fields": sorted(shown), "is_you": you,'),
        ("citar/items.py", '"owner": owner.public(viewer) if owner is not None else None, "system": row.owner_id is None,'),
        ("citar/pool/__init__.py", '"owner": owner.public(viewer) if owner else None,'),
        ("citar/pool/__init__.py", 'base["config"] = registry.public(config)'),
        ("citar/pool/__init__.py", 'log.debug("registry.public() failed for server %s", server.id, exc_info=True)'),
        ("citar/pool/__init__.py", '"subject": subject.public(viewer) if subject else None,'),
        ("citar/server/admin_api.py", '"host_server_id": reg.get("host_server_id"), "servers": [S.public(sv, reg) for sv in mine],'),
        ("citar/server/admin_api.py", 'return S.public(S.get(sv["id"]))'),
        ("citar/server/admin_api.py", 'return S.public(S.get(saved["id"]))'),
        ("citar/server/app.py", 'pid, extra = seat.player, {"seat": seat.public()}'),
        ("citar/server/app.py", "seats = [seat.public() for seat in s.seats]"),
        ("citar/server/auth_api.py", '"invited_by": creator.public(None)["display_name"] if creator else None}'),
        ("citar/server/ownership.py", '"owner": owner.public(viewer) if owner is not None else None,'),
        ("citar/server/pool_api.py", '"visibility": group.visibility, "owner": owner.public(me) if owner else None,'),
        ("citar/server/session.py", '"seats": [s.public(include_tokens) for s in self.seats],'),
    }

    def test_the_callers_are_the_fixed_list(self):
        found = set()
        for path in sorted((ROOT / "citar").rglob("*.py")):
            for line in path.read_text(encoding="utf-8").splitlines():
                if re.search(r"\.public\(", line):
                    found.add((path.relative_to(ROOT).as_posix(), line.strip()))
        self.assertEqual(sorted(found - self.FIXED), [], "new callers: does each name its viewer?")
        self.assertEqual(sorted(self.FIXED - found), [], "callers gone: update the list")

    def test_an_account_needs_a_viewer(self):
        with self.assertRaises(TypeError):
            User(handle="x", handle_lower="x").public()


# ---------------------------------------------------------------------------------------------------------- gate 2

#: The corpus: what each file is and whether it is taken (and served as what).
CORPUS = {
    "valid.png": "image/png", "valid.jpg": "image/jpeg", "valid.webp": "image/webp", "png-named.jpg": "image/png",
    "lossless.webp": "image/webp", "alpha.webp": "image/webp", "progressive.jpg": "image/jpeg",
    "exif-gps.jpg": "image/jpeg", "metadata.png": "image/png", "metadata.webp": "image/webp",
    "image.svg": None, "html-named.png": None, "truncated.png": None, "257kb.png": None, "2000x2000.png": None,
    "image.gif": None,
}
#: The pictures that carry metadata (made with Pillow, which CITAR does not use: each holds the text SECRET in every
#: kind of metadata its format has, and Exif with a GPS position), and the rest, which carry none.
WITH_METADATA = ("exif-gps.jpg", "metadata.png", "metadata.webp")


def _png_chunks(data: bytes) -> list:
    """A PNG's chunk types, in order."""
    pos, out = 8, []
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos:pos + 8])
        out.append(kind.decode("ascii"))
        pos += 12 + length
    return out


def _jpeg_segments(data: bytes) -> list:
    """A JPEG's marker segments before its first scan's data, as (marker, the segment's first 12 bytes after its
    length)."""
    pos, out = 2, []
    while data[pos + 1] != 0xDA:
        (length,) = struct.unpack(">H", data[pos + 2:pos + 4])
        out.append((data[pos + 1], data[pos + 4:pos + 16]))
        pos += 2 + length
    return out


def _webp_chunks(data: bytes) -> list:
    """A WebP's chunks, as (type, payload)."""
    pos, out = 12, []
    while pos < len(data):
        (length,) = struct.unpack("<I", data[pos + 4:pos + 8])
        out.append((data[pos:pos + 4].decode("ascii"), data[pos + 8:pos + 8 + length]))
        pos += 8 + length + (length & 1)
    return out


def _chunk(kind: bytes, data: bytes) -> bytes:
    """A PNG chunk with its checksum."""
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)


def _jpeg_at(data: bytes, marker: int) -> tuple:
    """Where a JPEG's first segment of ``marker`` starts and ends, walking the segments from SOI."""
    pos = 2
    while True:
        (length,) = struct.unpack(">H", data[pos + 2:pos + 4])
        if data[pos + 1] == marker:
            return pos, pos + 2 + length
        pos += 2 + length


def _riff(body: bytes) -> bytes:
    """A WebP of the chunks ``body``."""
    return b"RIFF" + struct.pack("<I", len(body) + 4) + b"WEBP" + body


def _tree(root: Path) -> dict:
    """Every file under ``root`` with its size and modification time, but the database's own files."""
    out = {}
    for path in root.rglob("*"):
        if path.is_file() and not re.search(r"\.db(-wal|-shm|-journal)?$", path.name):
            st = path.stat()
            out[path.relative_to(root).as_posix()] = (st.st_size, st.st_mtime_ns)
    return out


class Avatars(Base_):
    """Uploads checked by their headers, stored under the data directory alone, served safely."""

    def upload(self, who: str, name: str, data: bytes = None, **kw):
        data = (AVATARS / name).read_bytes() if data is None else data
        return self.client.post("/api/auth/me/avatar", files={"file": (name, data, "application/octet-stream")},
                                headers=self.headers[who], **kw)

    def mine(self, handle="pat") -> list:
        folder = profiles.avatars_dir()
        return sorted(p.name for p in folder.glob(f"{self.ids[handle]}-*")) if folder.exists() else []

    def test_the_corpus_is_what_the_gate_names(self):
        self.assertEqual({p.name for p in AVATARS.iterdir()}, set(CORPUS))
        self.assertEqual((AVATARS / "257kb.png").stat().st_size, 257 * 1024)

    def test_the_corpus_is_accepted_or_refused_and_served_with_its_proven_type(self):
        self.set_levels("pat", {"avatar": "global", "profile": "global"})
        for name, mime in CORPUS.items():
            with self.subTest(file=name):
                data = (AVATARS / name).read_bytes()
                r = self.upload("pat", name)
                if mime is None:
                    self.assertIn(r.status_code, (400, 413), r.text)
                    self.assertIn("PNG, JPEG or WebP", r.json()["detail"])
                    continue
                self.assertEqual(r.status_code, 200, r.text)
                avatar = r.json()["user"]["avatar"]
                self.assertTrue(avatar.startswith("/api/users/pat/avatar?v="), avatar)
                got = self.client.get(avatar)                      # a visitor: the field is global
                self.assertEqual(got.status_code, 200)
                # as stored: the upload without its metadata; a picture that carries none, byte for byte
                self.assertEqual(got.content, images.inspect(data).data)
                self.assertEqual(got.content == data, name not in WITH_METADATA)
                self.assertEqual(got.headers["content-type"], mime)
                self.assertEqual(got.headers["x-content-type-options"], "nosniff")
                self.assertEqual(got.headers["content-security-policy"], "default-src 'none'; sandbox")
                self.assertIn("max-age=", got.headers["cache-control"])
                self.assertEqual(len(self.mine()), 1, "the previous file was removed")

    def test_the_header_checks(self):
        png, jpg, webp = ((AVATARS / name).read_bytes() for name in ("valid.png", "valid.jpg", "valid.webp"))
        sof, sos = _jpeg_at(jpg, 0xC0), _jpeg_at(jpg, 0xDA)
        for why, data in {"a PNG with a bad checksum": png[:40] + bytes([png[40] ^ 1]) + png[41:],
                          "a PNG with bytes after IEND": png + b"x",
                          "a PNG without IDAT": png[:33] + png[-12:],
                          "a JPEG without its end": jpg[:-2],
                          "a WebP whose RIFF length lies": webp[:-1],
                          "a PNG with a critical chunk no decoder knows": png[:33] + _chunk(b"QQQQ", b"x") + png[33:],
                          "a PNG chunk whose type is not letters": png[:33] + _chunk(b"t3Xt", b"x") + png[33:],
                          "a JPEG cut inside its scan": jpg[:(sos[1] + len(jpg)) // 2],
                          "a JPEG with a scan and no frame": jpg[:sof[0]] + jpg[sof[1]:],
                          "a JPEG with its headers and no scan": jpg[:sos[0]] + b"\xff\xd9",
                          "a WebP whose second chunk runs past the end": _riff(webp[12:] + b"EXIF\xff\0\0\0abcd"),
                          "a WebP with a second VP8X": _riff(webp[12:] + b"VP8X\x0a\0\0\0" + bytes(10)),
                          "nothing": b""}.items():
            with self.subTest(why=why):
                with self.assertRaises(images.ImageError):
                    images.inspect(data)
        self.assertEqual(images.inspect(png).width, 64)
        # what follows a JPEG's end is not part of it (a second picture, appended bytes): left out, not refused
        self.assertEqual(images.inspect(jpg + b"<html>appended</html>").data, jpg)
        # a WebP whose last chunk is odd and unpadded, its RIFF length counting without the pad, is whole
        lossless = (AVATARS / "lossless.webp").read_bytes()
        odd = _riff(lossless[12:] + b"ALPH\x03\0\0\0abc")
        self.assertEqual(images.inspect(odd).data, _riff(lossless[12:] + b"ALPH\x03\0\0\0abc\0"))

    def test_metadata_is_left_out(self):
        """A picture goes to everyone its field reaches: Exif (a GPS position, the camera's serial number, when it was
        taken), XMP, text chunks and comments are left out before it is stored, and what changes how it looks is
        kept (the ICC profile, gamma, the pixel size). Nothing is decoded: the pixels are the upload's."""
        self.set_levels("pat", {"avatar": "global", "profile": "global"})
        for name in WITH_METADATA:
            with self.subTest(file=name):
                data = (AVATARS / name).read_bytes()
                self.assertIn(b"SECRET", data)
                r = self.upload("pat", name)
                self.assertEqual(r.status_code, 200, r.text)
                got = self.client.get(r.json()["user"]["avatar"]).content
                stored = (profiles.avatars_dir() / self.mine()[0]).read_bytes()
                self.assertEqual(got, stored)
                for leak in (b"SECRET", b"Exif\0", b"http://ns.adobe.com/xap", b"adobe:ns:meta", b"2026:10:11"):
                    self.assertNotIn(leak, got)
                info, again = images.inspect(data), images.inspect(got)
                self.assertEqual((again.width, again.height), (info.width, info.height))
                self.assertEqual(again.data, got, "a stored picture has nothing more to leave out")
                if name.endswith(".jpg"):
                    segments = _jpeg_segments(got)
                    self.assertFalse([m for m, _ in segments if m in (0xE1, 0xED, 0xFE)], "no APP1, APP13 or COM")
                    self.assertTrue(any(m == 0xE2 and head.startswith(b"ICC_PROFILE") for m, head in segments))
                    app0 = got[got.index(b"\xff\xe0"):][:18]
                    self.assertEqual(app0[2:4], b"\x00\x10", "JFIF without its thumbnail")
                    self.assertEqual(app0[16:18], b"\x00\x00")
                elif name.endswith(".png"):
                    chunks = _png_chunks(got)
                    self.assertFalse({"tEXt", "zTXt", "iTXt", "eXIf", "tIME"} & set(chunks), chunks)
                    self.assertTrue({"IHDR", "IDAT", "IEND", "iCCP", "pHYs", "gAMA"} <= set(chunks), chunks)
                else:
                    chunks = _webp_chunks(got)
                    kinds = [k for k, _ in chunks]
                    self.assertEqual(kinds, ["VP8X", "ICCP", "VP8 "])
                    self.assertEqual(chunks[0][1][0] & 0x0C, 0, "VP8X no longer says EXIF or XMP follow")
                    self.assertEqual(chunks[0][1][0] & 0x20, 0x20, "it still says ICC does")
                    self.assertEqual(struct.unpack("<I", got[4:8])[0] + 8, len(got))

    def test_404_when_the_field_is_hidden_and_for_no_upload(self):
        self.assertEqual(self.get("/api/users/pat/avatar", "max").status_code, 404, "no upload yet")
        self.assertEqual(self.upload("pat", "valid.png").status_code, 200)
        self.assertEqual(self.get("/api/users/pat/avatar", "max").status_code, 200, "members by default")
        self.assertEqual(self.get("/api/users/pat/avatar").status_code, 404, "not for a visitor")
        self.assertIn("private", self.get("/api/users/pat/avatar", "max").headers["cache-control"])
        self.set_levels("pat", {"avatar": "private"})
        for who in (None, "max", "newt", "moe"):
            self.assertEqual(self.get("/api/users/pat/avatar", who).status_code, 404, who)
        self.assertEqual(self.get("/api/users/pat/avatar", "pat").status_code, 200)
        self.assertEqual(self.get("/api/users/pat/avatar", "ada").status_code, 200)
        self.set_levels("pat", {"avatar": "global", "profile": "server"})
        self.assertEqual(self.get("/api/users/pat/avatar").status_code, 404, "the profile gates the avatar too")
        self.set_levels("pat", {"avatar": "global", "profile": "global"})
        self.assertIn("public", self.get("/api/users/pat/avatar").headers["cache-control"])

    def test_replace_removes_the_previous_file_and_delete_removes_it(self):
        self.assertEqual(self.upload("pat", "valid.png").status_code, 200)
        first = self.mine()
        self.assertEqual(len(first), 1)
        self.assertEqual(self.upload("pat", "valid.webp").status_code, 200)
        second = self.mine()
        self.assertEqual(len(second), 1)
        self.assertNotEqual(first, second)
        self.assertTrue(second[0].endswith(".webp"))
        self.assertEqual(self.upload("pat", "image.svg").status_code, 400)
        self.assertEqual(self.mine(), second, "a refused upload leaves the avatar as it was")
        r = self.client.delete("/api/auth/me/avatar", headers=self.headers["pat"])
        self.assertEqual(r.status_code, 200)
        self.assertIsNone(r.json()["user"]["avatar"])
        self.assertEqual(self.mine(), [])
        self.assertEqual(self.get("/api/users/pat/avatar", "pat").status_code, 404)

    def test_only_the_replaced_file_goes(self):
        """Two uploads of one account at the same moment: each removes only the file it replaced, never the other's
        new one, which the row may point at; nothing outside the avatars folder is ever removed."""
        from citar import paths
        folder = profiles.avatars_dir()
        uid = self.ids["pat"]
        for name in ("old", "theirs", "mine"):
            (folder / f"{uid}-{name}.png").write_bytes(b"x")
        self.assertTrue(profiles.remove_replaced(f"avatars/{uid}-old.png", f"avatars/{uid}-mine.png"))
        self.assertEqual(self.mine(), [f"{uid}-mine.png", f"{uid}-theirs.png"])
        self.assertFalse(profiles.remove_replaced(f"avatars/{uid}-mine.png", f"avatars/{uid}-mine.png"))
        outside = paths.data_dir() / "keep-me.txt"
        outside.write_text("x")
        try:
            self.assertFalse(profiles.remove_replaced("keep-me.txt", None))
            self.assertFalse(profiles.remove_replaced("avatars/../keep-me.txt", None))
            self.assertTrue(outside.exists())
        finally:
            outside.unlink()

    def test_nothing_is_written_outside_the_avatars_folder(self):
        from citar import paths
        roots = [paths.data_dir(), paths.save_dir(), paths.config_dir()]
        before = [_tree(r) for r in roots]
        for name in CORPUS:
            self.upload("pat", name)
        self.client.delete("/api/auth/me/avatar", headers=self.headers["pat"])
        self.upload("max", "valid.jpg")
        after = [_tree(r) for r in roots]
        for root, b, a in zip(roots, before, after):
            changed = {k for k in set(a) | set(b) if a.get(k) != b.get(k)}
            # the usage ledger is written by any game the process still plays (other tests' sessions), not by avatars
            outside = sorted(k for k in changed if not (root == paths.data_dir() and k.startswith("avatars/"))
                             and not k.startswith("usage/"))
            self.assertEqual(outside, [], f"written under {root}")
        self.assertEqual(sorted(p.name for p in profiles.avatars_dir().iterdir()), self.mine("max"),
                         "only max's file is left: no temporary file, nothing of pat's")

    def test_bad_uploads(self):
        self.assertEqual(self.client.post("/api/auth/me/avatar", content=b"x", headers={
            **self.headers["pat"], "content-type": "image/png"}).status_code, 400, "not a form")
        two = self.client.post("/api/auth/me/avatar", headers=self.headers["pat"], files=[
            ("a", ("a.png", (AVATARS / "valid.png").read_bytes(), "image/png")),
            ("b", ("b.png", (AVATARS / "valid.png").read_bytes(), "image/png"))])
        self.assertEqual(two.status_code, 400, "two files")
        big = self.upload("pat", "big.png", data=b"\x89PNG" + b"\0" * (300 * 1024))
        self.assertEqual(big.status_code, 413)
        self.assertEqual(self.mine(), [])
        # bodies the form parser underneath cannot read (its errors are ValueErrors, which were a 500): a boundary
        # that is not the header's, bytes that are no form, and a form cut short in a part's headers (no file in
        # it); each is a 400 with the rules, the picture stays as it was and nothing is written
        self.assertEqual(self.upload("pat", "valid.png").status_code, 200)
        kept, before = self.mine(), _tree(profiles.avatars_dir())
        part = (b'Content-Disposition: form-data; name="file"; filename="a.png"\r\nContent-Type: image/png\r\n\r\n'
                + (AVATARS / "valid.png").read_bytes() + b"\r\n")
        for why, body in (("a boundary that is not the header's", b"--x\r\n" + part + b"--x--\r\n"),
                          ("bytes that are no form", bytes(range(256)) * 4),
                          ("a part with no headers' end", b"--zz\r\nContent-Disposition: form-data; name=\"f\"")):
            with self.subTest(body=why):
                r = self.client.post("/api/auth/me/avatar", content=body, headers={
                    **self.headers["pat"], "content-type": "multipart/form-data; boundary=zz"})
                self.assertEqual(r.status_code, 400, r.text)
                self.assertIn("PNG, JPEG or WebP", r.json()["detail"])
                self.assertEqual(self.mine(), kept)
                self.assertEqual(_tree(profiles.avatars_dir()), before)


# ---------------------------------------------------------------------------------------------------------- P3.9.1

class Routes(Base_):
    """The new routes by a stranger, a visitor, the owner and an administrator (P3.9.1)."""

    def test_profile_routes(self):
        self.assertEqual(self.client.post("/api/auth/me/avatar", files={"f": ("a.png", (AVATARS / "valid.png")
                                                                              .read_bytes())}, headers=self.headers["pat"])
                         .status_code, 200)
        self.set_levels("pat", {"profile": "private"})
        expect = {None: 404, "max": 404, "moe": 404, "pat": 200, "ada": 200}
        for path in ("/api/users/pat", "/api/users/pat/avatar"):
            for who, want in expect.items():
                with self.subTest(path=path, who=who or "visitor"):
                    self.assertEqual(self.get(path, who).status_code, want)
        self.assertEqual(self.get("/api/users").status_code, 401)
        for who in ("max", "pat", "ada"):
            self.assertEqual(self.get("/api/users", who).status_code, 200)

    def test_my_avatar_is_mine_and_needs_csrf(self):
        anon = self.client.post("/api/auth/me/avatar", files={"f": ("a.png", (AVATARS / "valid.png").read_bytes())})
        self.assertEqual(anon.status_code, 401)
        self.assertEqual(self.client.delete("/api/auth/me/avatar").status_code, 401)
        no_csrf = {k: v for k, v in self.headers["pat"].items() if k != sessions.CSRF_HEADER}
        r = self.client.post("/api/auth/me/avatar", files={"f": ("a.png", (AVATARS / "valid.png").read_bytes())},
                             headers=no_csrf)
        self.assertEqual(r.status_code, 403)
        self.assertEqual(self.client.delete("/api/auth/me/avatar", headers=no_csrf).status_code, 403)
        self.assertEqual(self.client.put("/api/auth/me", json={"bio": "x"}, headers=no_csrf).status_code, 403)

    def test_admin_routes_are_not_there_for_anyone_else(self):
        uid = self.ids["pat"]
        routes = [("GET", "/api/admin/users", None), ("GET", f"/api/admin/users/{uid}", None),
                  ("PATCH", f"/api/admin/users/{uid}", {"trust_tier": "trusted"}), ("GET", "/api/admin/audit", None)]
        for method, path, body in routes:
            for who, want in ((None, 401), ("max", 404), ("newt", 404), ("moe", 404), ("pat", 404), ("ada", 200)):
                with self.subTest(route=f"{method} {path}", who=who or "visitor"):
                    r = self.client.request(method, path, json=body, headers=self.headers[who])
                    self.assertEqual(r.status_code, want, r.text)


def tearDownModule():
    db.dispose()
    shutil.rmtree(_TMP, ignore_errors=True)


if __name__ == "__main__":
    unittest.main()
