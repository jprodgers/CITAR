"""Who drives a civilization's turns (a player's controller) is separate from which difficulty numbers it gets (its
handicap) and which decisions the engine takes for it (its auto settings: UN votes, conquered cities, free picks).

The engine's side is tested in the rule scripts (tests/rules/controllers_*.toml and the ones they name); these are the
server's: the seats of a session and a scenario."""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import unittest

ALL_ON = {"un_vote": True, "conquest": True, "free_picks": True}
ALL_OFF = {"un_vote": False, "conquest": False, "free_picks": False}


class SeatTests(unittest.TestCase):
    """A session's seat changes reach the engine, and a scenario's seats take the controller's settings."""

    def test_changing_a_seat_changes_the_engine_controller(self):
        from citar.server.session import SessionManager, SAVE_DIR
        import shutil
        m = SessionManager()
        s = m.create({"map_size": "duel", "seed": 3, "barbarians": "off"},
                     [{"type": "human"}, {"type": "bot", "handicap": "human"}], track=False, start=False)
        try:
            def seat(pid):
                p = s.game.player(pid)
                return p["controller"], p["handicap"], p["auto"]
            self.assertEqual(seat(1), ("bot", "human", ALL_ON))
            s.update_seat(0, type="bot")
            self.assertEqual((s.seats[0].type, *seat(0)), ("bot", "bot", "ai", ALL_ON))
            s.update_seat(1, type="llm")
            self.assertEqual(seat(1), ("llm", "human", ALL_OFF))   # handicap was set
            with self.assertRaises(ValueError):
                s.update_seat(0, type="hybrid")           # no hybrid agent until the hybrid seat exists
        finally:
            m.delete(s.id)
            shutil.rmtree(SAVE_DIR / s.id, ignore_errors=True)

    def test_scenarios_accept_hybrid_seats_and_their_settings(self):
        from citar.engine_api import EngineGame
        g = EngineGame.new({"map_size": "duel", "seed": 21, "barbarians": "normal", "city_states": 1,
                            "players": [{"controller": "human"}, {"controller": "bot"}]})
        seats = g.normalize_seats([{"type": "hybrid", "handicap": "human", "auto": {"un_vote": False}}])
        self.assertEqual(seats[0]["type"], "hybrid")
        self.assertEqual((seats[0]["handicap"], seats[0]["auto"]), ("human", {"un_vote": False}))
        over = g.scenario_overview()["players"][0]
        self.assertEqual((over["handicap"], over["auto"]), ("human", ALL_OFF))


if __name__ == "__main__":
    unittest.main()
