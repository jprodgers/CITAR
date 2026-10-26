"""scripts/refcheck/summarize.py: the per-game values, the bootstrap, and the gates G1-G4 (DESIGN.md P2.4.4).

The gates decide whether the Rust bot may replace the Python one, so each rule is tested on games built to meet or
break it, and the committed Python baselines are read as the gate reads them.
"""
import contextlib
import importlib.util
import io
import json
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PYTHON = ROOT / "refcheck" / "baseline" / "python"
EXPLAINED = ROOT / "refcheck" / "baseline" / "explained.toml"


def _load():
    spec = importlib.util.spec_from_file_location("refcheck_summarize", ROOT / "scripts" / "refcheck" / "summarize.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


S = _load()


def civ(pid, scale=1.0, wars=0, captures=0, alive=True, turns=(100, 200, 300)):
    """A civilization whose state metrics grow with the checkpoint, times `scale`."""
    at = {}
    for t in turns:
        k = t / 100
        at[str(t)] = {"alive": alive, "cities": 3 * k * scale, "population": 18 * k * scale,
                      "techs": 19 * k * scale, "score": 230 * k * scale, "military": 43 * k * scale,
                      "era": k, "policies": 8 * k, "land": 45 * k * scale,
                      "wars_declared": wars, "wars_declared_on_majors": wars, "wars_declared_on_others": 0,
                      "cities_captured": captures, "cities_lost": 0}
    at["end"] = dict(at[str(turns[-1])], turn=330)
    return {"pid": pid, "nation": "X", "aggression": 0.5, "alive": alive, "eliminated_turn": None,
            "final_score": 0, "at": at}


def game(i, scale=1.0, wars=1, captures=0, victory="Time", size="small", jitter=0.0):
    """A finished 4-civilization game; `jitter` spreads the games around `scale` (up to 10% either way)."""
    per_game = scale * (1 + jitter * ((i % 5) - 2) / 20)
    civs = [civ(p, per_game, wars=wars, captures=captures) for p in range(4)]
    return {"i": i, "seed": 5000 + i, "size": size, "map_type": "continents", "barbarians": "normal",
            "speed": "Quick", "turn_limit": None, "players": 4, "turns": 330, "winner": 0, "victory": victory,
            "civs": civs, "bot_errors": 0, "seconds": 1.0}


def write(lines, folder, name):
    p = Path(folder) / name
    p.write_text("".join(json.dumps(x) + "\n" for x in lines), encoding="utf-8")
    return p


def run(argv):
    """The command line's exit code and printout."""
    out = io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
        code = S.main([str(a) for a in argv])
    return code, out.getvalue()


class PerGameTests(unittest.TestCase):
    def test_a_state_metric_is_the_mean_over_the_living_and_an_event_metric_the_games_total(self):
        g = game(0, wars=2, captures=1)
        g["civs"][3]["at"]["100"] = {"alive": False, "score": 0, "wars_declared": 5, "cities_captured": 0}
        self.assertAlmostEqual(S.game_value(g, "cities", "100"), 3.0)        # three living civilizations of 3
        self.assertEqual(S.game_value(g, "wars_declared", "100"), 2 + 2 + 2 + 5)   # the dead one's wars count
        self.assertEqual(S.game_value(g, "cities_captured", "end"), 4)
        self.assertIsNone(S.game_value(g, "cities", "250"))                  # a checkpoint never reached

    def test_rates_count_games(self):
        gs = [game(0, wars=0), game(1, captures=1), game(2, victory="Scientific")]
        gs[2]["civs"][0]["alive"] = False
        r = S.rates(gs)
        self.assertEqual(r["war"], [False, True, True])
        self.assertEqual(r["capture"], [False, True, False])
        self.assertEqual(r["early_end"], [False, False, True])
        self.assertEqual(r["elimination"], [False, False, True])
        self.assertEqual(r["victory_Scientific"], [False, False, True])

    def test_the_bootstrap_is_seeded_by_its_cell_and_brackets_a_shift(self):
        a = [float(x % 7) for x in range(60)]
        b = [x + 2.0 for x in a]
        lo, hi = S.bootstrap(a, b, "cell")
        self.assertEqual((lo, hi), S.bootstrap(a, b, "cell"))
        self.assertNotEqual((lo, hi), S.bootstrap(a, b, "another cell"))
        self.assertTrue(lo < 2.0 < hi and lo > 0, (lo, hi))

    def test_a_split_half_keeps_the_game_order_alternating(self):
        gs = [game(i) for i in (5, 0, 3, 1, 2, 4)]
        h1, h2 = S.split_half(gs)
        self.assertEqual([g["i"] for g in h1], [0, 2, 4])
        self.assertEqual([g["i"] for g in h2], [1, 3, 5])


class GateTests(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.py = write([game(i, jitter=1.0) for i in range(40)], self.dir.name, "py.jsonl")
        self.empty = Path(self.dir.name) / "empty.toml"
        self.empty.write_text("", encoding="utf-8")

    def gate(self, rust_games, explained="", extra=()):
        rust = write(rust_games, self.dir.name, "rust.jsonl")
        f = Path(self.dir.name) / "explained.toml"
        f.write_text(explained, encoding="utf-8")
        return run([self.py, rust, "--gate", f, *extra])

    def test_the_same_games_pass(self):
        code, out = self.gate([game(i, jitter=1.0) for i in range(40)])
        self.assertEqual(code, 0, out)
        self.assertIn("gate: passed on small", out)

    def test_g1_a_crash_line_fails_even_when_a_later_line_finished_the_game(self):
        crash = {"i": 3, "seed": 5003, "size": "small", "map_type": "continents", "barbarians": "normal",
                 "speed": "Quick", "turn_limit": None, "crash": "T4 P1 basic-1: panic: boom", "trace": ""}
        code, out = self.gate([game(i, jitter=1.0) for i in range(40)] + [crash])
        self.assertEqual(code, 1)
        self.assertIn("G1: game 3", out)

    def test_g1_reads_the_runs_with_the_invariants_on(self):
        checked = write([dict(game(0), checks=True),
                         {"i": 1, "seed": 5001, "size": "small", "map_type": "pangaea", "barbarians": "normal",
                          "speed": "Quick", "turn_limit": None, "checks": True,
                          "crash": "InvariantViolation: CITY-3", "trace": ""}],
                        self.dir.name, "checked.jsonl")
        code, out = self.gate([game(i, jitter=1.0) for i in range(40)], extra=["--checked", checked])
        self.assertEqual(code, 1)
        self.assertIn("crashed with the invariants on: InvariantViolation", out)
        self.assertNotIn("do not say", out)
        clean = write([dict(game(i), checks=True) for i in range(3)], self.dir.name, "clean.jsonl")
        code, out = self.gate([game(i, jitter=1.0) for i in range(40)], extra=["--checked", clean])
        self.assertEqual(code, 0, out)
        self.assertIn("3 games finished with the invariants on (small 3), 0 crash lines", out)

    def test_g1_a_checked_run_must_say_its_lines_were_checked_and_hold_games(self):
        # A crash-free run played without --checks has the same lines but for the flag, so it cannot stand in.
        unchecked = write([game(i) for i in range(3)] + [dict(game(3), checks=True)], self.dir.name, "plain.jsonl")
        code, out = self.gate([game(i, jitter=1.0) for i in range(40)], extra=["--checked", unchecked])
        self.assertEqual(code, 1)
        self.assertIn('3 lines do not say "checks": true (games 0, 1, 2)', out)
        self.assertIn("4 games finished (small 4), 0 crash lines, 3 lines not marked as played with the "
                      "invariants on", out)
        empty = write([], self.dir.name, "none.jsonl")
        code, out = self.gate([game(i, jitter=1.0) for i in range(40)], extra=["--checked", empty])
        self.assertEqual(code, 1)
        self.assertIn("no finished game, so it shows nothing about the invariants", out)

    def test_g1_every_stratum_of_a_needs_games_in_b(self):
        py = write([game(i, jitter=1.0) for i in range(40)] + [game(40 + i, size="large", jitter=1.0)
                                                                for i in range(10)], self.dir.name, "py2.jsonl")
        rust = write([game(i, jitter=1.0) for i in range(40)], self.dir.name, "rust2.jsonl")
        code, out = run([py, rust, "--gate", self.empty])
        self.assertEqual(code, 1, out)
        self.assertIn("G1: large: B has no finished game on large maps (A has 10), so large was not compared", out)
        # B's map sizes that A lacks are only a warning: A is the reference.
        code, out = run([rust, py, "--gate", self.empty])
        self.assertEqual(code, 0, out)
        self.assertIn("warning: B's 10 games on large maps have none in A", out)

    def test_g1_a_run_cut_short_fails(self):
        # Ten games of forty: every interval widens and nothing is material, so without a count it would pass.
        code, out = self.gate([game(i, jitter=1.0) for i in range(10)])
        self.assertEqual(code, 1, out)
        self.assertIn("G1: small: B has 10 finished games on small maps, fewer than 40", out)
        code, out = self.gate([game(i, jitter=1.0) for i in range(10)], extra=["--min-games", 10])
        self.assertEqual(code, 0, out)
        code, out = self.gate([game(i, jitter=1.0) for i in range(40)], extra=["--min-games", 41])
        self.assertEqual(code, 1, out)
        self.assertIn("fewer than 41 (--min-games)", out)

    def test_g2_a_gross_gap_fails_and_cannot_be_explained(self):
        gross = "".join(f'[[gap]]\nmetric = "{m}"\ncheckpoint = "{t}"\nstratum = "small"\nreason = "r"\n\n'
                        for m in S.METRICS for t in ("100", "200", "300", "end"))
        code, out = self.gate([game(i, scale=0.7, jitter=1.0) for i in range(40)], gross)
        self.assertEqual(code, 1)
        self.assertIn("G2: small: cities at 100", out)

    def test_g2_means_must_rise(self):
        flat = []
        for i in range(40):
            g = game(i, jitter=1.0)
            for c in g["civs"]:
                c["at"]["300"]["cities"] = c["at"]["200"]["cities"]
            flat.append(g)
        code, out = self.gate(flat, "".join(
            f'[[gap]]\nmetric = "cities"\ncheckpoint = "{t}"\nstratum = "small"\nreason = "r"\n\n'
            for t in ("300", "end")))
        self.assertEqual(code, 1)
        self.assertIn("do not rise", out)

    def test_g3_a_material_gap_needs_an_entry_and_an_entry_answers_it(self):
        rust = []
        for i in range(40):
            g = game(i, jitter=1.0)
            for c in g["civs"]:
                for t in c["at"]:
                    c["at"][t]["military"] *= 1.2
            rust.append(g)
        code, out = self.gate(rust)
        self.assertEqual(code, 1)
        self.assertIn("G3: small: military at 100: 1.20x", out)
        entries = "".join(f'[[gap]]\nmetric = "military"\ncheckpoint = "{t}"\nstratum = "small"\nreason = "r"\n\n'
                          for t in ("100", "200", "300", "end"))
        code, out = self.gate(rust, entries)
        self.assertEqual(code, 0, out)
        self.assertIn("4 explained", out)

    def test_g3_a_small_gap_needs_nothing_and_its_entry_is_only_stale(self):
        rust = []
        for i in range(40):
            g = game(i, jitter=1.0)
            for c in g["civs"]:
                for t in c["at"]:
                    c["at"][t]["military"] *= 1.05
            rust.append(g)
        code, out = self.gate(rust, '[[gap]]\nmetric = "military"\ncheckpoint = "100"\nstratum = "small"\n'
                                    'reason = "an old gap"\n')
        self.assertEqual(code, 0, out)
        self.assertIn("warning: stale: military at 100 on small", out)

    def test_g3_skips_event_metrics_python_saw_less_than_once_a_game(self):
        # Captures 0 -> 4 a game, but Python's mean is 0 (its rate is G4's: one game in five).
        code, out = self.gate([game(i, jitter=1.0, captures=int(i % 5 == 0)) for i in range(40)])
        self.assertEqual(code, 0, out)
        self.assertNotIn("FAIL G3", out)

    def test_g4_a_rate_off_by_more_than_its_bound_fails_unless_explained(self):
        rust = [game(i, jitter=1.0, wars=0 if i % 2 else 1) for i in range(40)]
        entry = '[[gap]]\nmetric = "rate:war"\ncheckpoint = "game"\nstratum = "small"\nreason = "r"\n'
        wars = "".join(f'[[gap]]\nmetric = "wars_declared"\ncheckpoint = "{t}"\nstratum = "small"\nreason = "r"\n\n'
                       for t in ("100", "200", "300", "end"))
        code, out = self.gate(rust, wars)
        self.assertEqual(code, 1)
        self.assertIn("G4: small: rate war: 100% -> 50%", out)
        code, out = self.gate(rust, wars + entry)
        self.assertEqual(code, 0, out)

    def test_pending_entries_pass_until_no_pending_refuses_them(self):
        rust = []
        for i in range(40):
            g = game(i, jitter=1.0)
            for c in g["civs"]:
                c["at"]["100"]["military"] *= 1.3
            rust.append(g)
        entry = ('[[gap]]\nmetric = "military"\ncheckpoint = "100"\nstratum = "small"\nreason = "r"\n'
                 'pending = "2-07f"\n')
        code, out = self.gate(rust, entry)
        self.assertEqual(code, 0, out)
        self.assertIn("PENDING 2-07f", out)
        code, out = self.gate(rust, entry, extra=["--no-pending"])
        self.assertEqual(code, 1)
        self.assertIn("pending: small: military at 100", out)

    def test_no_pending_refuses_a_pending_entry_whose_gap_is_under_its_rule(self):
        # Noise, a short run or a partial fix can take the gap under G3's rule: the fix is still owed.
        entry = ('[[gap]]\nmetric = "military"\ncheckpoint = "100"\nstratum = "small"\nreason = "r"\n'
                 'pending = "2-07f"\n')
        same = [game(i, jitter=1.0) for i in range(40)]
        code, out = self.gate(same, entry)
        self.assertEqual(code, 0, out)
        self.assertIn("warning: stale: military at 100 on small", out)
        code, out = self.gate(same, entry, extra=["--no-pending"])
        self.assertEqual(code, 1, out)
        self.assertIn("pending: military at 100 on small is still owed by 2-07f (its gap is under its rule in "
                      "this run), which --no-pending refuses", out)

    def test_no_pending_refuses_a_pending_entry_of_a_map_size_not_compared(self):
        entry = ('[[gap]]\nmetric = "wars_declared"\ncheckpoint = "end"\nstratum = "large"\nreason = "r"\n'
                 'pending = "2-07f"\n')
        code, out = self.gate([game(i, jitter=1.0) for i in range(40)], entry, extra=["--no-pending"])
        self.assertEqual(code, 1, out)
        self.assertIn("(large is not compared by this command)", out)
        code, out = self.gate([game(i, jitter=1.0) for i in range(40)], entry)
        self.assertEqual(code, 0, out)
        self.assertNotIn("stale", out)

    def test_an_entry_answers_only_the_gap_in_its_band(self):
        def scaled(k):
            rust = []
            for i in range(40):
                g = game(i, jitter=1.0)
                for c in g["civs"]:
                    c["at"]["100"]["military"] *= k
                rust.append(g)
            return rust
        entry = '[[gap]]\nmetric = "military"\ncheckpoint = "100"\nstratum = "small"\nreason = "r"\nratio = {}\n'
        code, out = self.gate(scaled(1.2), entry.format("[1.1, 1.3]"))
        self.assertEqual(code, 0, out)
        self.assertIn("1 explained", out)
        # Twice as large, or the other way, is another gap than the one the reason measured.
        for k, shown in ((1.6, "1.60x"), (0.8, "0.80x")):
            code, out = self.gate(scaled(k), entry.format("[1.1, 1.3]"))
            self.assertEqual(code, 1, out)
            self.assertIn(f"G3: small: military at 100: {shown}", out)
            self.assertIn(f"the explained gap moved: {shown} is outside the entry's ratio [1.1, 1.3]", out)

    def test_a_rate_entry_answers_only_the_gap_in_its_points(self):
        rust = [game(i, jitter=1.0, wars=0 if i % 2 else 1) for i in range(40)]
        wars = "".join(f'[[gap]]\nmetric = "wars_declared"\ncheckpoint = "{t}"\nstratum = "small"\nreason = "r"\n\n'
                       for t in ("100", "200", "300", "end"))
        entry = '[[gap]]\nmetric = "rate:war"\ncheckpoint = "game"\nstratum = "small"\nreason = "r"\npoints = {}\n'
        code, out = self.gate(rust, wars + entry.format("[-60, -40]"))
        self.assertEqual(code, 0, out)
        code, out = self.gate(rust, wars + entry.format("[-30, -26]"))
        self.assertEqual(code, 1, out)
        self.assertIn("G4: small: rate war: 100% -> 50% (-50 points): the explained gap moved: -50.00 points is "
                      "outside the entry's points [-30, -26]", out)

    def test_a_malformed_entry_is_refused(self):
        head = '[[gap]]\nmetric = "cities"\ncheckpoint = "100"\nstratum = "small"\nreason = "r"\n'
        rate = '[[gap]]\nmetric = "rate:war"\ncheckpoint = "game"\nstratum = "small"\nreason = "r"\n'
        for bad in ('[[gap]]\nmetric = "towers"\ncheckpoint = "100"\nstratum = "small"\nreason = "r"\n',
                    '[[gap]]\nmetric = "cities"\ncheckpoint = "100"\nstratum = "small"\n',
                    head + 'why = 1\n',
                    '[[gap]]\nmetric = "rate:war"\ncheckpoint = "100"\nstratum = "small"\nreason = "r"\n',
                    head + '[[gap]]\nmetric = "cities"\ncheckpoint = "100"\nstratum = "small"\nreason = "again"\n',
                    head + 'ratio = [1.3, 1.1]\n', head + 'ratio = [0, 1.1]\n', head + 'ratio = [1.1]\n',
                    head + 'ratio = "1.1-1.3"\n', head + 'ratio = [1, true]\n', head + 'points = [-10, 10]\n',
                    rate + 'ratio = [0.8, 1.2]\n', rate + 'points = [-120, 0]\n', rate + 'points = [5, 5]\n'):
            code, _ = self.gate([game(i, jitter=1.0) for i in range(40)], bad)
            self.assertEqual(code, 2, bad)
        for good in (head + 'ratio = [1, 2]\n', rate + 'points = [-100, 100]\n'):
            code, out = self.gate([game(i, jitter=1.0) for i in range(40)], good)
            self.assertEqual(code, 0, f"{good}\n{out}")


class CommittedBaselineTests(unittest.TestCase):
    def test_each_python_baseline_passes_against_itself(self):
        for name in ("small", "std-large"):
            code, out = run([PYTHON / f"{name}.jsonl", PYTHON / f"{name}.jsonl", "--gate", EXPLAINED])
            self.assertEqual(code, 0, out)

    def test_the_strata_are_the_sizes(self):
        code, out = run([PYTHON / "std-large.jsonl", PYTHON / "std-large.jsonl", "--json"])
        self.assertEqual(code, 0)
        doc = json.loads(out)
        self.assertEqual([c["stratum"] for c in doc["compare"]], ["standard", "large"])
        self.assertEqual([(c["games_a"], c["games_b"]) for c in doc["compare"]], [(14, 14), (10, 10)])

    def test_the_explained_file_reads(self):
        entries = S.read_explained(EXPLAINED)
        self.assertTrue(all(e["reason"].strip() for e in entries))
        # Each committed entry records the band its gap was measured in, so a later gap in its cell that the
        # reason does not describe fails rather than passing as explained.
        self.assertTrue(all("ratio" in e or "points" in e for e in entries), entries)

    def test_no_pending_refuses_the_committed_pending_entries_even_against_itself(self):
        # Python against itself has no material gap, so every entry is stale; one still owed fails all the same.
        owed = [e for e in S.read_explained(EXPLAINED) if e.get("pending")]
        for name in ("small", "std-large"):
            code, out = run([PYTHON / f"{name}.jsonl", PYTHON / f"{name}.jsonl", "--gate", EXPLAINED,
                             "--no-pending"])
            self.assertEqual(code, 1 if owed else 0, out)
            for e in owed:
                self.assertIn(f"pending: {e['metric']} at {e['checkpoint']} on {e['stratum']} is still owed by "
                              f"{e['pending']}", out)


if __name__ == "__main__":
    unittest.main()
