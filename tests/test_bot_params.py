"""The parameter schema of the bot version basic-1 (crates/citar-bot/params/basic-1.json) against the live bot's
PARAM_GROUPS, and the table of what ``profiles.clean_params`` makes of a profile's overrides, which the Rust bot's
``clean()`` is tested against (crates/citar-engine DESIGN.md P2.3.2, packages 2-00b and 2-01a).

The schema file was written by scripts/bots/export_params.py and is the source of truth from then on; this module
holds it equal to PARAM_GROUPS, apart from the two cache parameters basic-1 drops, for as long as the Python bot exists.

tests/data/clean_params_cases.json is recorded from the inputs in ``CASES``:

    python -m tests.test_bot_params --record

A case marked ``fix`` is one where basic-1 refuses on purpose what Python accepted (``FIXES``).
"""
import tests  # noqa: F401  (temporary saves folder and server registry; must be imported before citar)
import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCHEMA = ROOT / "crates" / "citar-bot" / "params" / "basic-1.json"
TABLE = ROOT / "tests" / "data" / "clean_params_cases.json"
DROPPED = ("site_cache_turns", "bv_cache_turns")

#: Python accepted these, and basic-1's clean() refuses them (DESIGN.md P2.3.2, the two laxities fixed)
FIXES = {
    "int-integral": "An int must be integral: 2, 2.0 and \"2\" are 2, and 2.5 is refused. Python kept a fractional "
                    "value as a float, which turned the bot's // into float floor division.",
    "names-known": "The names in an order or a list must be among the parameter's options (or a preset, \"default\" "
                   "or null). Python accepted any string.",
}

#: (what the case shows, the overrides, the fix it meets or None)
CASES = [
    ("no overrides", {}, None),
    ("null overrides", None, None),
    ("an int", {"counter_rounds": 2}, None),
    ("an int from an integral float", {"counter_rounds": 2.0}, None),
    ("an int from a numeric string", {"counter_rounds": "2"}, None),
    ("an int from a numeric string with spaces", {"counter_rounds": " 3 "}, None),
    ("an int from an exponent string", {"war_min_turn": "1e2"}, None),
    ("a negative int", {"war_min_turn": -1}, None),
    ("an int that is fractional", {"counter_rounds": 2.5}, "int-integral"),
    ("an int from a fractional string", {"counter_rounds": "2.5"}, "int-integral"),
    ("an int equal to its default is dropped", {"counter_rounds": 4}, None),
    ("an int equal to its default as a float is dropped", {"counter_rounds": 4.0}, None),
    ("an int refuses a word", {"counter_rounds": "x"}, None),
    ("an int refuses a boolean", {"counter_rounds": True}, None),
    ("an int refuses null", {"counter_rounds": None}, None),
    ("an int refuses a list", {"counter_rounds": [1]}, None),
    ("a float", {"tech_noise": 0.25}, None),
    ("a float from an int", {"tech_noise": 1}, None),
    ("a float from a numeric string", {"tech_noise": "0.3"}, None),
    ("a float equal to its default is dropped", {"tech_noise": 0.1}, None),
    ("a float zero", {"tech_noise": 0}, None),
    ("a float refuses a boolean", {"tech_noise": False}, None),
    ("a float refuses a word", {"tech_noise": "abc"}, None),
    ("a bool true", {"lux_buy": True}, None),
    ("a bool from 1", {"lux_buy": 1}, None),
    ("a bool from \"1\"", {"lux_buy": "1"}, None),
    ("a bool from \"true\"", {"lux_buy": "true"}, None),
    ("a bool from \"TRUE\"", {"lux_buy": "TRUE"}, None),
    ("a bool from \"yes\"", {"lux_buy": "yes"}, None),
    ("a bool from \"on\"", {"lux_buy": "on"}, None),
    ("a bool from \"On\"", {"lux_buy": "On"}, None),
    ("a bool false is the default and is dropped", {"lux_buy": False}, None),
    ("a bool from \"false\"", {"faith_buildings": "false"}, None),
    ("a bool from \"0\"", {"faith_buildings": "0"}, None),
    ("a bool from \"no\"", {"faith_buildings": "no"}, None),
    ("a bool from \"off\"", {"faith_buildings": "off"}, None),
    ("a bool from 0", {"faith_buildings": 0}, None),
    ("a bool from any other word is false", {"faith_buildings": "maybe"}, None),
    ("a bool from 1.0 is false: its text is not \"1\"", {"faith_buildings": 1.0}, None),
    ("a bool from null is false", {"faith_buildings": None}, None),
    ("a bool true equal to its default is dropped", {"faith_buildings": True}, None),
    ("a choice", {"tech_mode": "potential"}, None),
    ("a choice equal to its default is dropped", {"tech_mode": "classic"}, None),
    ("a choice refuses a name it does not list", {"tech_mode": "random"}, None),
    ("a choice refuses null when it does not list it", {"tech_mode": None}, None),
    ("a choice of null, its default, is dropped", {"small_city_focus": None}, None),
    ("a choice that lists null takes a name", {"small_city_focus": "food"}, None),
    ("a choice refuses the text \"none\"", {"small_city_focus": "none"}, None),
    ("a choice of a great person", {"free_gp_early": "Great Engineer"}, None),
    ("an order's preset", {"policy_order_peaceful": "liberty_first"}, None),
    ("an order's preset equal to its default is dropped", {"policy_order_peaceful": "rationalism_early"}, None),
    ("an order's \"default\"", {"policy_order_peaceful": "default"}, None),
    ("an order of null whose default is a preset", {"policy_order_peaceful": None}, None),
    ("an order of null, its default, is dropped", {"policy_order_aggressive": None}, None),
    ("an order's \"default\" where the default is null is kept", {"policy_order_aggressive": "default"}, None),
    ("an order's preset where the default is null", {"policy_order_aggressive": "rationalism_early"}, None),
    ("an order refuses a preset it does not have", {"policy_order_aggressive": "bogus"}, None),
    ("an order of names", {"policy_order_aggressive": ["Honor", "Tradition", "Liberty"]}, None),
    ("an order with a name it does not list", {"policy_order_peaceful": ["Tradition", "Bogus"]}, "names-known"),
    ("an order refuses a name that is no string", {"policy_order_peaceful": ["Tradition", 3]}, None),
    ("an order refuses a number", {"policy_order_peaceful": 5}, None),
    ("an order equal to its default is dropped",
     {"beliefs_founder": ["Ceremonial Burial", "Tithe", "Church Property", "Peace Loving", "Pilgrimage",
                          "Initiation Rites", "Interfaith Dialogue", "World Church", "Papal Primacy"]}, None),
    ("an order of fewer names", {"beliefs_founder": ["Tithe"]}, None),
    ("an empty order", {"beliefs_pantheon": []}, None),
    ("a list of names", {"promo_lines": ["Drill", "Shock"]}, None),
    ("a list with a name it does not list", {"promo_in_city": ["Cover", "Bogus"]}, "names-known"),
    ("a list has no presets", {"promo_in_city": "default"}, None),
    ("a list of null", {"promo_in_city": None}, None),
    ("an unknown key", {"no_such_parameter": 1}, None),
    ("a cache parameter basic-1 drops is known to Python", {"site_cache_turns": 8}, "dropped"),
    ("an unknown key among known ones", {"counter_rounds": 2, "bogus": 1}, None),
    ("several overrides, defaults dropped",
     {"counter_rounds": "6", "tech_noise": 0.1, "lux_buy": "yes", "tech_mode": "potential",
      "policy_order_aggressive": "liberty_first"}, None),
]


def record_case(name: str, params, fix) -> dict:
    """What ``clean_params("basic", params)`` gives for one case: its output, or its error's message."""
    from citar.bots.profiles import ProfileError, clean_params
    case = {"name": name, "params": params}
    try:
        case["out"] = clean_params("basic", params)
    except ProfileError as e:
        case["error"] = str(e)
    if fix:
        case["fix"] = fix
    return case


def table() -> dict:
    """The table as recorded: every case of CASES with Python's answer."""
    return {
        "format": 1,
        "about": "profiles.clean_params(\"basic\", params) for each case: \"out\", the cleaned overrides, or "
                 "\"error\", ProfileError's message. Python's live bot is named \"basic\" in its messages; it is the "
                 "version basic-1. A case marked \"fix\" is one basic-1's clean() answers differently on purpose: "
                 "\"int-integral\" and \"names-known\" are refused (see \"fixes\"), and \"dropped\" names a "
                 "parameter basic-1 does not have, refused as an unknown key. Recorded by "
                 "python -m tests.test_bot_params --record.",
        "engine": "basic",
        "version": "basic-1",
        "fixes": dict(FIXES, dropped="site_cache_turns and bv_cache_turns are not parameters of basic-1 "
                                     "(DESIGN.md P2.3.9, fixes 5 and 6)."),
        "cases": [record_case(*c) for c in CASES],
    }


def table_text() -> str:
    """The table's file: two-space JSON, a final newline."""
    return json.dumps(table(), indent=2, ensure_ascii=False) + "\n"


def exact(v) -> str:
    """A value as JSON text, so that 2 and 2.0 differ, as they do to the Rust test that reads the table."""
    return json.dumps(v, sort_keys=True)


class ParameterSchema(unittest.TestCase):
    """basic-1.json is PARAM_GROUPS without the two cache parameters, spec for spec."""

    @classmethod
    def setUpClass(cls):
        cls.doc = json.loads(SCHEMA.read_text(encoding="utf-8"))

    def test_the_file_has_the_bots_page_shape(self):
        self.assertEqual(list(self.doc), ["engine", "groups"])
        self.assertEqual(self.doc["engine"], "basic-1")
        for group in self.doc["groups"]:
            self.assertEqual(list(group), ["name", "help", "params"])

    def test_373_parameters_in_17_groups(self):
        specs = [s for g in self.doc["groups"] for s in g["params"]]
        self.assertEqual(len(self.doc["groups"]), 17)
        self.assertEqual(len(specs), 373)
        self.assertEqual(len({s["key"] for s in specs}), 373)
        counts = {}
        for s in specs:
            counts[s["type"]] = counts.get(s["type"], 0) + 1
        self.assertEqual(counts, {"int": 220, "float": 127, "choice": 9, "bool": 8, "order": 7, "list": 2})

    def test_every_spec_is_param_groups_apart_from_the_dropped_keys(self):
        from citar.bots.basic import PARAM_GROUPS
        self.assertEqual(len(self.doc["groups"]), len(PARAM_GROUPS))
        for group, (name, help_text, specs) in zip(self.doc["groups"], PARAM_GROUPS):
            self.assertEqual((group["name"], group["help"]), (name, help_text))
            kept = [s for s in specs if s["key"] not in DROPPED]
            self.assertEqual([s["key"] for s in group["params"]], [s["key"] for s in kept], name)
            for mine, theirs in zip(group["params"], kept):
                # every key, type, default, label, help, range, unit, choice, option and preset, as JSON has it
                self.assertEqual(exact(mine), exact(theirs), mine["key"])
                self.assertEqual(list(mine), list(theirs), mine["key"])
        dropped = [s["key"] for _, _, specs in PARAM_GROUPS for s in specs if s["key"] in DROPPED]
        self.assertEqual(sorted(dropped), sorted(DROPPED))

    def test_the_specs_keys_by_type(self):
        number = {"key", "default", "type", "label", "help", "min", "max", "unit"}
        for s in (s for g in self.doc["groups"] for s in g["params"]):
            want = {"int": number, "float": number, "bool": {"key", "default", "type", "label", "help"},
                    "choice": {"key", "default", "type", "label", "help", "choices"},
                    "order": {"key", "default", "type", "label", "help", "options", "presets"},
                    "list": {"key", "default", "type", "label", "help", "options", "presets"}}[s["type"]]
            self.assertEqual(set(s), want, s["key"])

    def test_the_exporter_writes_this_file(self):
        import contextlib
        import io
        sys.path.insert(0, str(ROOT / "scripts" / "bots"))
        try:
            import export_params
        finally:
            sys.path.remove(str(ROOT / "scripts" / "bots"))
        with contextlib.redirect_stdout(io.StringIO()) as out:
            status = export_params.main(["--check"])
        self.assertEqual(status, 0, out.getvalue())


class CleanParams(unittest.TestCase):
    """tests/data/clean_params_cases.json is what Python's clean_params gives today."""

    @classmethod
    def setUpClass(cls):
        cls.doc = json.loads(TABLE.read_text(encoding="utf-8"))

    def test_the_recorded_table_is_current(self):
        self.assertEqual(exact(self.doc), exact(table()), "re-record with python -m tests.test_bot_params --record")

    def test_the_file_is_as_the_recorder_writes_it(self):
        self.assertEqual(TABLE.read_text(encoding="utf-8"), table_text())

    def test_the_table_covers_every_listed_input(self):
        from citar.bots.basic import PARAM_SPECS
        specs = {s["key"]: s for s in PARAM_SPECS}
        cases = self.doc["cases"]
        self.assertGreaterEqual(len(cases), 40)

        def one(kind, value, default=None, fix=None):
            """Whether some case gives a parameter of this type this value, alone."""
            for c in cases:
                p = c["params"] or {}
                if len(p) != 1:
                    continue
                (k, v), = p.items()
                s = specs.get(k)
                if s is None or s["type"] not in kind or exact(v) != exact(value):
                    continue
                if default is not None and exact(s["default"]) != exact(default):
                    continue
                if fix is not None and c.get("fix") != fix:
                    continue
                return True
            return False
        self.assertTrue(one(("order",), None, default=None), "a null order whose default is null")
        self.assertTrue(one(("choice",), None), "a null choice")
        for v in (2, 2.0, "2"):
            self.assertTrue(one(("int",), v), f"an int given as {v!r}")
        self.assertTrue(one(("int",), 2.5, fix="int-integral"), "an int given as 2.5")
        self.assertTrue(any(c.get("error", "").endswith("is not a parameter of basic.") for c in cases), "unknown key")
        self.assertTrue(any(c.get("fix") == "names-known" and specs[next(iter(c["params"]))]["type"] == t
                            for c in cases for t in ("order", "list")), "an unknown list name")
        self.assertTrue(any(c.get("fix") == "names-known" and specs[next(iter(c["params"]))]["type"] == "list"
                            for c in cases), "an unknown name in a list")
        self.assertTrue(one(("order",), "liberty_first"), "a preset")
        self.assertTrue(one(("order",), "default"), "'default'")
        for v in (True, 1, "1", "true", "yes", "on", False, 0, "0", "false", "no", "off"):
            self.assertTrue(one(("bool",), v), f"the bool spelling {v!r}")
        self.assertTrue(all(("out" in c) != ("error" in c) for c in cases))
        self.assertTrue(all(c.get("fix") in (None, *self.doc["fixes"]) for c in cases))


if __name__ == "__main__":
    if sys.argv[1:] == ["--record"]:
        TABLE.parent.mkdir(parents=True, exist_ok=True)
        TABLE.write_text(table_text(), encoding="utf-8", newline="\n")
        print(f"wrote {TABLE.relative_to(ROOT)}: {len(CASES)} cases")
    else:
        unittest.main()
