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
        checked = write([game(0), {"i": 1, "seed": 5001, "size": "small", "map_type": "pangaea",
                                   "barbarians": "normal", "speed": "Quick", "turn_limit": None,
                                   "crash": "InvariantViolation: CITY-3", "trace": ""}],
                        self.dir.name, "checked.jsonl")
        code, out = self.gate([game(i, jitter=1.0) for i in range(40)], extra=["--checked", checked])
        self.assertEqual(code, 1)
        self.assertIn("crashed with the invariants on: InvariantViolation", out)

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

    def test_a_malformed_entry_is_refused(self):
        for bad in ('[[gap]]\nmetric = "towers"\ncheckpoint = "100"\nstratum = "small"\nreason = "r"\n',
                    '[[gap]]\nmetric = "cities"\ncheckpoint = "100"\nstratum = "small"\n',
                    '[[gap]]\nmetric = "cities"\ncheckpoint = "100"\nstratum = "small"\nreason = "r"\nwhy = 1\n',
                    '[[gap]]\nmetric = "rate:war"\ncheckpoint = "100"\nstratum = "small"\nreason = "r"\n',
                    '[[gap]]\nmetric = "cities"\ncheckpoint = "100"\nstratum = "small"\nreason = "r"\n'
                    '[[gap]]\nmetric = "cities"\ncheckpoint = "100"\nstratum = "small"\nreason = "again"\n'):
            code, _ = self.gate([game(i, jitter=1.0) for i in range(40)], bad)
            self.assertEqual(code, 2, bad)


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


if __name__ == "__main__":
    unittest.main()
