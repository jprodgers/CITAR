import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import math
import random
import unittest

from citar.engine import mapgen
from citar.engine import unique_types as U
from citar.engine.rules import get_rules


class ResourceVarietyTest(unittest.TestCase):
    """Maps hold every strategic type, and more luxury types the bigger they are."""

    @classmethod
    def setUpClass(cls):
        cls.R = get_rules()
        cls.lux = {r for r, d in cls.R.resources.items() if d["resourceType"] == "Luxury"
                   and not mapgen._never_generates(d) and not d["_umap"].has_tag(U.CityStateOnlyResource)}
        cls.strat = {r for r, d in cls.R.resources.items() if d["resourceType"] == "Strategic"
                     and not mapgen._never_generates(d)}

    def gen(self, size, seed, players=None, city_states=None, options=None):
        """The set of resources on a generated map of a lobby size."""
        sz = self.R.const["map_sizes"][size]
        tiles, *_ = mapgen.generate_map(
            self.R, sz["width"], sz["height"], "continents",
            sz["players"] if players is None else players,
            sz["city_states"] if city_states is None else city_states,
            random.Random(seed), options=options)
        return {t.resource for t in tiles if t.resource}

    def test_variety_curve(self):
        def v(k):
            return mapgen.luxury_variety(self.R, self.R.const["map_sizes"][k]["width"]
                                         * self.R.const["map_sizes"][k]["height"])

        self.assertAlmostEqual(v("small"), 0.5)
        self.assertAlmostEqual(v("standard"), 0.75)
        self.assertAlmostEqual(v("large"), 0.9)
        self.assertAlmostEqual(v("huge"), 1.0)
        self.assertAlmostEqual(v("gargantuan"), 1.0)
        self.assertAlmostEqual(mapgen.luxury_variety(self.R, 100), 0.5)
        self.assertLess(v("standard"), mapgen.luxury_variety(self.R, 4500), v("large"))

    def test_big_maps_have_every_type(self):
        # few players leave the most luxury types to chance, so they are the hard case
        for size, seed in (("huge", 1), ("huge", 7), ("gargantuan", 3)):
            present = self.gen(size, seed, players=2, city_states=2)
            self.assertEqual(self.lux - present, set(), f"{size} seed {seed}")
            self.assertEqual(self.strat - present, set(), f"{size} seed {seed}")

    def test_standard_and_small_maps(self):
        for seed in (1, 2, 5):
            present = self.gen("standard", seed, players=2, city_states=0)
            self.assertGreaterEqual(len(self.lux & present), math.ceil(0.7 * len(self.lux)), f"seed {seed}")
            self.assertEqual(self.strat - present, set())
            for size in ("duel", "small"):
                present = self.gen(size, seed, players=2, city_states=0)
                self.assertGreaterEqual(len(self.lux & present), len(self.lux) // 2, f"{size} seed {seed}")
                self.assertEqual(self.strat - present, set(), f"{size} seed {seed}")

    def test_sparse_settings_still_have_every_strategic(self):
        opts = {"resources": {"strategic": {"density": 0.05}, "luxury": {"each": {"Silk": {"mode": "off"}}}}}
        present = self.gen("duel", 4, options=opts)
        self.assertEqual(self.strat - present, set())
        self.assertNotIn("Silk", present)
        # a kind turned off stays off
        present = self.gen("duel", 4, options={"resources": {"strategic": {"density": 0}}})
        self.assertEqual(self.strat & present, set())

    def test_deterministic(self):
        a = mapgen.generate_map(self.R, 44, 28, "continents", 2, 2, random.Random(9))[0]
        b = mapgen.generate_map(self.R, 44, 28, "continents", 2, 2, random.Random(9))[0]
        self.assertEqual([(t.resource, t.resource_amount) for t in a], [(t.resource, t.resource_amount) for t in b])


if __name__ == "__main__":
    unittest.main()
