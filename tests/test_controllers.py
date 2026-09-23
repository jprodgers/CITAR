"""Who drives a civilization's turns (Player.controller) is separate from which difficulty numbers it gets
(Player.handicap) and which decisions the engine takes for it (Player.auto: UN votes, conquered cities, free picks)."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import unittest
from unittest import mock

from citar.engine import cities, conquest, research, triggers, victory
from citar.engine.game import Game
from citar.engine.state import GameState, Player, seat_overrides
from citar.engine.uniques import Unique, civ_matches

ALL_ON = {"un_vote": True, "conquest": True, "free_picks": True}
ALL_OFF = {"un_vote": False, "conquest": False, "free_picks": False}


def game(*players, **kw):
    cfg = {"map_size": "duel", "seed": 21, "barbarians": "normal", "city_states": 1,
           "players": [dict(p) for p in players] or [{"controller": "human"}, {"controller": "bot"}]}
    cfg.update(kw)
    return Game.new(cfg)


class DefaultTests(unittest.TestCase):
    def test_defaults_follow_the_controller(self):
        g = game(*({"controller": c} for c in ("human", "llm", "mcp", "bot", "hybrid")), map_size="small")
        got = {p.controller: (p.handicap, p.auto) for p in g.s.players}
        for c in ("human", "llm", "mcp"):
            self.assertEqual(got[c], ("human", ALL_OFF), c)
        for c in ("bot", "hybrid", "minor", "barbarian"):
            self.assertEqual(got[c], ("ai", ALL_ON), c)

    def test_difficulty_and_unique_filters_follow_the_handicap(self):
        g = game({"controller": "hybrid"}, {"controller": "hybrid", "handicap": "human"}, difficulty="Settler")
        self.assertEqual([g.is_humanlike(0), g.is_humanlike(1)], [False, True])
        self.assertEqual([civ_matches(g, 0, "AI player"), civ_matches(g, 1, "Human player")], [True, True])
        self.assertNotEqual(research.tech_cost(g, 0, "Pottery"), research.tech_cost(g, 1, "Pottery"))

    def test_overrides_are_kept_and_checked(self):
        g = game({"controller": "hybrid", "handicap": "human", "auto": {"un_vote": False, "conquest": False}},
                 {"controller": "llm", "auto": {"free_picks": True}})
        a, b = g.player(0), g.player(1)
        self.assertEqual((a.handicap, a.auto), ("human", {"un_vote": False, "conquest": False, "free_picks": True}))
        self.assertEqual((b.handicap, b.auto), ("human", {"un_vote": False, "conquest": False, "free_picks": True}))
        for bad in ({"controller": "bot", "handicap": "deity"}, {"controller": "bot", "auto": {"trades": True}},
                    {"controller": "bot", "auto": {"un_vote": "false"}}, {"controller": "bot", "auto": {"conquest": 0}}):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                game(bad, {"controller": "bot"})

    def test_auto_values_must_be_true_or_false(self):
        """The string "false" is truthy: coercing it would hand the seat the very decision it declined."""
        self.assertEqual(seat_overrides(None, {"un_vote": False}), {"auto": {"un_vote": False}})
        with self.assertRaises(ValueError) as cm:
            seat_overrides(None, {"un_vote": "false", "conquest": True})
        self.assertIn("un_vote", str(cm.exception))
        self.assertNotIn("conquest", str(cm.exception))

    def test_a_new_controller_rederives_what_was_not_set_explicitly(self):
        p = Player(id=0, name="A", color="#aa0000", controller="bot", overrides={"auto": {"un_vote": False}})
        self.assertEqual((p.handicap, p.auto), ("ai", {**ALL_ON, "un_vote": False}))
        p.set_controller("human")
        self.assertEqual((p.handicap, p.auto), ("human", {**ALL_OFF, "un_vote": False}))
        p.set_controller("bot", handicap="human")
        self.assertEqual((p.handicap, p.auto), ("human", {**ALL_ON, "un_vote": False}))


class SaveTests(unittest.TestCase):
    def test_round_trip_keeps_the_settings(self):
        g = game({"controller": "hybrid", "handicap": "human", "auto": {"un_vote": False}}, {"controller": "bot"})
        g.player(1).auto["conquest"] = False               # set during play, not as an override
        s = GameState.from_dict(g.s.to_dict())
        self.assertEqual((s.players[0].handicap, s.players[0].auto), ("human", {**ALL_ON, "un_vote": False}))
        self.assertEqual(s.players[0].overrides, {"handicap": "human", "auto": {"un_vote": False}})
        self.assertEqual(s.players[1].auto, {**ALL_ON, "conquest": False})

    def test_saves_from_before_the_split_derive_them_from_the_controller(self):
        d = game({"controller": "llm"}, {"controller": "bot"}).s.to_dict()
        for pd in d["players"]:
            for k in ("handicap", "auto", "overrides"):
                pd.pop(k)
        s = GameState.from_dict(d)
        self.assertEqual([(p.handicap, p.auto) for p in s.players[:2]], [("human", ALL_OFF), ("ai", ALL_ON)])


class AutoDecisionTests(unittest.TestCase):
    def test_un_votes_are_cast_only_for_civs_whose_votes_are_automatic(self):
        g = game({"controller": "human"}, {"controller": "bot"}, {"controller": "bot", "auto": {"un_vote": False}},
                 city_states=0, map_size="small")
        victory._un(g)["next_vote"] = g.turn
        with mock.patch.object(victory, "_ai_vote", return_value=None) as ai_vote:
            victory.hold_vote(g)
        self.assertEqual([c.args[1].id for c in ai_vote.call_args_list], [1])
        # and asked directly, it has no vote to give for a civ that decides for itself, whoever it knows
        g.meet(2, 0)
        g.meet(2, 1)
        self.assertIsNone(victory._ai_vote(g, g.player(2)))

    def test_conquered_cities_are_decided_only_when_conquest_is_automatic(self):
        for auto, expected in ((True, 1), (False, 0)):
            with self.subTest(auto=auto):
                g = game({"controller": "human", "auto": {"conquest": auto}}, {"controller": "human"})
                settler = next(u for u in g.player_units(1) if u.type == "Settler")
                city = cities.found_city(g, 1, settler.idx, unit=settler)
                unit = next(u for u in g.player_units(0) if g.udef(u)["_military"])
                with mock.patch.object(conquest, "_auto_conquer") as auto_conquer:
                    conquest.conquer(g, city, unit)
                self.assertEqual(auto_conquer.call_count, expected)
                self.assertEqual(city.owner, 0)

    def test_free_picks_are_made_only_when_they_are_automatic(self):
        for auto, left in ((True, 0), (False, 1)):
            with self.subTest(auto=auto):
                g = game({"controller": "bot", "auto": {"free_picks": auto}}, {"controller": "bot"})
                p = g.player(0)
                p.free_techs = 0
                triggers.trigger(g, Unique("Free Technology"), 0)
                self.assertEqual(p.free_techs, left)

    def test_free_great_people_are_chosen_only_when_picks_are_automatic(self):
        from citar.engine import great_people
        for auto in (True, False):
            with self.subTest(auto=auto):
                g = game({"controller": "bot", "auto": {"free_picks": auto}}, {"controller": "bot"})
                p = g.player(0)
                p.free_great_people = 0
                with mock.patch.object(great_people, "ai_choose_free") as choose:
                    triggers.trigger(g, Unique("Free Great Person"), 0)
                self.assertEqual(p.free_great_people, 1)          # the pick is owed either way...
                self.assertEqual(choose.call_count, int(auto))    # ...and made for the civ only when it is automatic


class SeatTests(unittest.TestCase):
    def test_changing_a_seat_changes_the_engine_controller(self):
        from citar.server.session import SessionManager, SAVE_DIR
        import shutil
        m = SessionManager()
        s = m.create({"map_size": "duel", "seed": 3, "barbarians": "off"},
                     [{"type": "human"}, {"type": "bot", "handicap": "human"}], track=False, start=False)
        try:
            p0, p1 = s.game.player(0), s.game.player(1)
            self.assertEqual((p1.controller, p1.handicap, p1.auto), ("bot", "human", ALL_ON))
            s.update_seat(0, type="bot")
            self.assertEqual((s.seats[0].type, p0.controller, p0.handicap, p0.auto), ("bot", "bot", "ai", ALL_ON))
            s.update_seat(1, type="llm")
            self.assertEqual((p1.controller, p1.handicap, p1.auto), ("llm", "human", ALL_OFF))   # handicap was set
            with self.assertRaises(ValueError):
                s.update_seat(0, type="hybrid")           # no hybrid agent until the hybrid seat exists
        finally:
            m.delete(s.id)
            shutil.rmtree(SAVE_DIR / s.id, ignore_errors=True)

    def test_scenarios_accept_hybrid_seats_and_their_settings(self):
        from citar.engine import scenario
        g = game({"controller": "human"}, {"controller": "bot"})
        seats = scenario.normalize_seats(g, [{"type": "hybrid", "handicap": "human", "auto": {"un_vote": False}}])
        self.assertEqual(seats[0]["type"], "hybrid")
        self.assertEqual((seats[0]["handicap"], seats[0]["auto"]), ("human", {"un_vote": False}))
        over = scenario.overview(g)["players"][0]
        self.assertEqual((over["handicap"], over["auto"]), ("human", ALL_OFF))


if __name__ == "__main__":
    unittest.main()
