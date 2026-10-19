"""CPU jobs through the bindings (design/phase4-helper.md P4.4; package 4-07's gates): the lab game's result built in
Rust (``engine_api.run_job``, which ``lab.play`` now calls) against the Python result building it replaced, the specs'
refusals and their fuzzing, the result hash, the chain and whether a result answers its spec, the lab's queue and
runner on jobs, and the speed of a lab duel.

- **Parity** (gate 1): the seven games recorded from the Python result building before it was deleted
  (``tests/data/lab_results_*.json``, recorded by ``tests/lab_results.py``: six of 40 rounds, duels and small games,
  and a duel of 130) give the same result on every deterministic key, ``errors`` as messages, the keys that name the
  build held to this build's values (``job_result_rebased``, so a release's version bump, which moves the build id,
  changes nothing here) for as long as this build plays those games as the recording one did; and the Python
  building kept as an oracle (``tests/lab_play_oracle.py``) agrees with
  ``lab.play`` on those games and on fresh ones, on any build: longer games, and seats whose overrides and levels are
  lists and orders of names.
- **No clock in a result** (gate 3): a hashed result holds no ``seconds``, ``cpu_s``, ``started_ts``, ``finished`` or
  trace, and one that carries any is refused; a reference spec with a path, budget or workers key is refused.
- **Refusals** (gate 5): an unknown key, a value out of bounds, an unknown bot version and a hash that does not match,
  each with its reason; 2,000 mutated lab and reference specs raise nothing but ``bad_spec``, JSON that does not read
  (``NaN``, ``Infinity``, nesting past 128) among them.
- **Tampering shows** (gate 6): a result altered in one score no longer has the result hash, a digest list altered in
  one round fails the chain check, and another game's result does not answer a spec (``job_result_fits``).
- **The queue and the runner**: an experiment whose games a job would refuse is refused when queued, saying why; one
  already queued is shown as one that cannot be played, once, and none of its games starts; a crash is final.
- **Speed** (gate 8): a lab duel through ``run_job`` is no slower than through the old ``lab.play``, in the same run.
"""
from __future__ import annotations

import contextlib
import copy
import io
import json
import os
import random
import shutil
import statistics
import tempfile
import time
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock

from citar import engine_api, lab
from tests import has_test_ops, lab_play_oracle, lab_results

DATA = Path(__file__).resolve().parent / "data"
#: What a lab result line holds outside the hash: measurements the writer adds.
TIMES = ("seconds", "started_ts", "cpu_s", "finished")
ON_CI = bool(os.environ.get("CI"))


def recorded() -> list[tuple[str, dict]]:
    """The recorded games: (name, {"spec", "result", ...})."""
    out = []
    for p in sorted(DATA.glob("lab_results_*.json")):
        out.append((p.stem[len("lab_results_"):], json.loads(p.read_text(encoding="utf-8"))))
    return out


def deterministic(line: dict) -> dict:
    """A result line as its file holds it (through JSON, so the old building's integer keys are text) without its
    times, ``errors`` as messages only (the first line of each)."""
    out = {k: v for k, v in json.loads(json.dumps(line)).items() if k not in TIMES}
    out["errors"] = [str(e).splitlines()[0] for e in out.get("errors") or []]
    return out


def quiet(fn, *args):
    """``fn(*args)`` with its PROGRESS lines swallowed."""
    with redirect_stdout(io.StringIO()):
        return fn(*args)


class Parity(unittest.TestCase):
    def test_the_recorded_python_results_are_the_rust_results(self):
        games = recorded()
        self.assertEqual(len(games), 7)
        self.assertEqual(sum(1 for _, d in games if d["spec"]["turns"] == 40), 6, "the gate's six of 40 rounds")
        build = engine_api.build_info()["build_id"]
        for name, doc in games:
            with self.subTest(game=name):
                line = quiet(lab.play, doc["spec"])
                self.assertNotIn("crash", line)
                for k in TIMES:
                    self.assertIn(k, line)
                # The old building, live, on the same game: parity on whatever build this is.
                self.assertEqual(deterministic(line), deterministic(lab_play_oracle.play(doc["spec"])))
                out = engine_api.run_job("lab_game", lab.job_spec(doc["spec"]))
                self.assertEqual(out["result"], {k: v for k, v in line.items() if k not in TIMES})
                self.assertEqual(engine_api.job_result_hash("lab_game", out["result"]), out["result_hash"])
                engine_api.job_chain_check(out["spec_hash"], out["chain"])
                engine_api.job_result_fits("lab_game", lab.job_spec(doc["spec"]), out["result"], out["chain"])
                self.assertEqual(out["chain"]["rounds"], out["result"]["turns"])
                # The recorded line, exactly: what the Python building gave before it was deleted. A result names
                # the build that played it, and the build id moves with every release's version (DESIGN.md P2.2.1),
                # so those keys are held to this build's values, as the jobs golden set holds its results.
                recorded_result = {k: v for k, v in doc["result"].items() if k not in TIMES}
                spec = lab.job_spec(doc["spec"])
                rebased = engine_api.job_result_rebased("lab_game", spec, recorded_result)
                if doc["build"]["build_id"] == build:
                    self.assertEqual(rebased, recorded_result, "rebasing under the recording build changes nothing")
                elif out["result"] != rebased:
                    self.skipTest(f"recorded under build {doc['build']['build_id']}, and build {build} plays this "
                                  "game otherwise: only the live comparison above holds; `python -m "
                                  "tests.lab_results --force` records them again")
                self.assertEqual(deterministic(line), deterministic({**doc["result"], **rebased}))
                self.assertEqual(out["result"], rebased)

    def test_the_recorder_in_the_repository_made_these_files(self):
        """``tests/lab_results.py`` names each recorded game, so they can be recorded again."""
        self.assertEqual({f"lab_results_{n}.json" for n, _, _ in lab_results.EXPERIMENTS},
                         {p.name for p in DATA.glob("lab_results_*.json")})
        for name, doc in recorded():
            self.assertEqual(set(doc), {"about", "recorded_at", "build", "spec", "result"}, name)
            self.assertRegex(doc["build"]["build_id"], r"^[0-9a-f]{12}$")
        lines = []
        self.assertEqual(lab_results.record(out=lines.append), [], "without --force a file is kept")
        self.assertEqual(len(lines), 7)

    def test_the_old_python_building_agrees_on_fresh_games(self):
        exps = [
            {"name": "fresh-duel", "games": 2, "seed": 9100, "size": "duel", "maps": ["fractal"], "turns": 110,
             "barbarians": "raging", "seats": [{"label": "x", "bot": "basic", "aggression": 0.9},
                                                {"profile": "classic-production"}]},
            {"name": "fresh-small", "games": 4, "seed": 9200, "size": "small", "maps": ["pangaea"], "turns": 60,
             "city_states": 3, "seats": [{"label": s, "bot": "basic"} for s in "abc"] + [{"label": "i", "bot": "idle"}]},
        ]
        for exp in exps:
            e = lab.normalize(exp)
            for i in range(2):
                spec = lab.game_spec(e, i)
                with self.subTest(exp=exp["name"], i=i):
                    old = lab_play_oracle.play(spec)
                    new = quiet(lab.play, spec)
                    self.assertEqual(deterministic(new), deterministic(old))

    def test_list_and_order_overrides_play_as_they_did_in_python(self):
        """basic-1's ``order`` and ``list`` parameters take lists of names, which the Bots page edits and a profile
        keeps: a seat that overrides them, an order's preset by name, and a factorial experiment whose levels are
        lists, through the old building and the new."""
        exps = [
            {"name": "lists-duel", "games": 1, "seed": 9400, "size": "duel", "maps": ["continents"], "turns": 40,
             "seats": [{"label": "ordered", "bot": "basic",
                        "params": {"policy_order_peaceful": ["Liberty", "Tradition", "Honor"],
                                   "promo_lines": ["Drill", "Shock"],
                                   "beliefs_pantheon": ["Fertility Rites", "Goddess of Love"]}},
                       {"label": "preset", "bot": "basic", "params": {"policy_order_peaceful": "liberty_first"}}]},
            {"name": "lists-factorial", "games": 1, "seed": 9500, "size": "small", "maps": ["pangaea"], "turns": 30,
             "players": 4, "bot": "basic", "factors": {"promo_in_city": [["Cover"], ["Drill", "Shock"]],
                                                        "tech_noise": [0.0, 0.3]}},
        ]
        for exp in exps:
            spec = lab.game_spec(lab.normalize(exp), 0)
            with self.subTest(exp=exp["name"]):
                old = lab_play_oracle.play(spec)
                new = quiet(lab.play, spec)
                self.assertNotIn("crash", new)
                self.assertEqual(deterministic(new), deterministic(old))
                lists = [v for p in new["players"].values() for v in {**p["params"], **(p["levels"] or {})}.values()
                         if isinstance(v, list)]
                self.assertTrue(lists, "the result records the lists")
        duel = quiet(lab.play, lab.game_spec(lab.normalize(exps[0]), 0))
        self.assertEqual(duel["players"]["0"]["params"]["policy_order_peaceful"], ["Liberty", "Tradition", "Honor"])

    def test_a_crash_is_written_as_a_crash_line_reads_it(self):
        """The runner's progress lines went with it (4-09: the job host reports a run's rounds); a crash's line
        keeps its shape."""
        spec = recorded()[0][1]["spec"]
        buf = io.StringIO()
        with redirect_stdout(buf):
            lab.play(spec)
        self.assertNotIn("PROGRESS ", buf.getvalue())
        self.assertEqual(lab.crash_text({"turn": 4, "player": 1, "label": "b", "message": "boom"}),
                         "T4 P1 b: panic: boom")
        self.assertEqual(lab.crash_text({"turn": 0, "player": None, "label": None, "message": "boom"}),
                         "T0: panic: boom")

    @unittest.skipUnless(has_test_ops(), "needs a build with the test operations")
    def test_a_crashed_game_is_a_final_crash_line_with_where_it_happened(self):
        spec = recorded()[0][1]["spec"]
        real = engine_api.run_job

        def panicking(kind, job, **kw):
            return real(kind, job, test_panic={"player": 1, "turn": 6}, **kw)

        with mock.patch.object(engine_api, "run_job", panicking):
            line = quiet(lab.play, spec)
        self.assertEqual(sorted(line), ["crash", "exp", "final", "finished", "i"])
        self.assertIs(line["final"], True)
        self.assertTrue(line["crash"].startswith("T6 P1 beta: panic: the test bot panics on turn 6"), line["crash"])
        out = engine_api.run_job("lab_game", lab.job_spec(spec), test_panic={"player": 1, "turn": 6})
        self.assertEqual(out["result"], {"crash": {"turn": 6, "player": 1, "label": "beta",
                                                   "message": "the test bot panics on turn 6, as it was asked to"}})
        self.assertEqual(out["chain"]["rounds"], 5)
        engine_api.job_chain_check(out["spec_hash"], out["chain"])
        engine_api.job_result_fits("lab_game", lab.job_spec(spec), out["result"], out["chain"])
        again = engine_api.run_job("lab_game", lab.job_spec(spec), test_panic={"player": 1, "turn": 6})
        self.assertEqual(again["result_hash"], out["result_hash"], "a crash is reproduced")

    def test_a_seat_carries_only_the_keys_a_job_reads_and_its_aggression_held(self):
        spec = copy.deepcopy(recorded()[0][1]["spec"])
        spec["seats"][0]["note"] = "a key of the experiment's own"
        spec["seats"][1]["aggression"] = 1.7
        job = lab.job_spec(spec)
        self.assertNotIn("note", job["seats"][0])
        self.assertEqual(job["seats"][1]["aggression"], 1.0)
        engine_api.check_job_spec("lab_game", job)


REFERENCE = {"i": 0, "seed": 5000, "size": "duel", "map_type": "continents", "barbarians": "normal",
             "speed": "Quick", "turn_limit": 20}


def _job(kind: str, seed: int | None = None) -> tuple[dict, dict]:
    """A spec of ``kind`` and its run."""
    if kind == "reference_game":
        spec = dict(REFERENCE)
    else:
        spec = lab.job_spec(recorded()[0][1]["spec"])
        spec["turns"] = 20
    if seed is not None:
        spec["seed"] = seed
    return spec, engine_api.run_job(kind, spec)


class Results(unittest.TestCase):
    def test_one_job_twice_gives_one_result_hash_and_one_chain(self):
        for kind in engine_api.JOB_KINDS:
            with self.subTest(kind=kind):
                spec, a = _job(kind)
                b = engine_api.run_job(kind, spec)
                self.assertEqual((a["result_hash"], a["chain"]), (b["result_hash"], b["chain"]))
                self.assertEqual(a["spec_hash"], engine_api.check_job_spec(kind, spec)["spec_hash"])
                self.assertEqual(a["chain"]["rounds"], 20)
                self.assertEqual(len(a["chain"]["digests"]), 20)
                self.assertGreater(a["wall_s"], 0)
        _, lab_run = _job("lab_game")
        _, ranked = _job("ranking_game")
        self.assertEqual(lab_run["result"], ranked["result"], "one game")
        self.assertNotEqual(lab_run["result_hash"], ranked["result_hash"], "the kind is hashed")

    def test_no_hashed_result_holds_a_time_or_a_trace(self):
        for kind in ("lab_game", "reference_game"):
            _, out = _job(kind)

            def keys(v):
                if isinstance(v, dict):
                    for k, x in v.items():
                        yield k
                        yield from keys(x)
                elif isinstance(v, list):
                    for x in v:
                        yield from keys(x)

            found = set(keys(out["result"])) & {"seconds", "cpu_s", "started_ts", "finished", "trace", "wall_s"}
            self.assertEqual(found, set(), kind)
            for k in ("seconds", "cpu_s", "started_ts", "finished", "trace"):
                timed = {**out["result"], k: 1.5}
                with self.assertRaises(engine_api.JobRefused) as cm:
                    engine_api.job_result_hash(kind, timed)
                self.assertEqual(cm.exception.code, "bad_result")
                self.assertIn("unknown field", str(cm.exception))

    def test_an_altered_score_or_digest_shows(self):
        spec, out = _job("lab_game")
        result = copy.deepcopy(out["result"])
        result["players"]["0"]["score"] += 1
        self.assertNotEqual(engine_api.job_result_hash("lab_game", result), out["result_hash"])
        self.assertEqual(engine_api.job_result_hash("lab_game", out["result"]), out["result_hash"])
        chain = copy.deepcopy(out["chain"])
        engine_api.job_chain_check(out["spec_hash"], chain)
        chain["digests"][7] = "ab" * 32
        with self.assertRaises(engine_api.JobRefused) as cm:
            engine_api.job_chain_check(out["spec_hash"], chain)
        self.assertEqual(cm.exception.code, "bad_chain")
        self.assertIn("do not fold to the head", str(cm.exception))
        short = {**out["chain"], "digests": out["chain"]["digests"][:-1]}
        with self.assertRaises(engine_api.JobRefused):
            engine_api.job_chain_check(out["spec_hash"], short)
        with self.assertRaises(engine_api.JobRefused):
            engine_api.job_chain_check("not a hash", out["chain"])

    def test_a_result_answers_its_spec_and_no_other(self):
        """``job_result_fits``: a result hash and a chain that folds say nothing of which spec a result answers. The
        spec decides the experiment, game, seed and map, who sat where and what played them (fingerprints included)
        and the build; a result naming another, or turns its chain did not play, is refused."""
        spec, out = _job("lab_game")
        engine_api.job_result_fits("lab_game", spec, out["result"], out["chain"])
        engine_api.job_result_fits("lab_game", spec, out["result"], out["chain"], spec_hash=out["spec_hash"])
        other_spec, other = _job("lab_game", seed=spec["seed"] + 1)
        engine_api.job_result_fits("lab_game", other_spec, other["result"], other["chain"])

        def refused(code, *args, **kw):
            with self.assertRaises(engine_api.JobRefused) as cm:
                engine_api.job_result_fits(*args, **kw)
            self.assertEqual(cm.exception.code, code, str(cm.exception))
            return str(cm.exception)

        # Another game's result beside this game's chain; beside its own chain, which folds from another chain_0.
        self.assertIn("seed", refused("bad_result", "lab_game", spec, other["result"], out["chain"]))
        refused("bad_chain", "lab_game", spec, other["result"], other["chain"])
        # A seat's fingerprint, label or overrides, or the build, not the spec's.
        for path, value in ((("players", "1", "fingerprint"), "000000000000"), (("players", "0", "label"), "x"),
                            (("players", "0", "params"), {"tech_noise": 0.3}), (("engine",), "000000000000")):
            forged = copy.deepcopy(out["result"])
            at = forged
            for k in path[:-1]:
                at = at[k]
            at[path[-1]] = value
            self.assertIn(path[-1], refused("bad_result", "lab_game", spec, forged, out["chain"]))
        # Turns the chain did not play.
        refused("bad_result", "lab_game", spec, {**out["result"], "turns": 12}, out["chain"])
        # The spec as another kind, a spec hash that is not its own, a reference game's line for a lab spec.
        refused("bad_chain", "ranking_game", spec, out["result"], out["chain"])
        refused("bad_spec", "lab_game", spec, out["result"], out["chain"], spec_hash="0" * 64)
        ref_spec, ref = _job("reference_game")
        engine_api.job_result_fits("reference_game", ref_spec, ref["result"], ref["chain"])
        refused("bad_result", "lab_game", spec, ref["result"], out["chain"])
        self.assertIn("seed", refused("bad_result", "reference_game", ref_spec, {**ref["result"], "seed": 1},
                                      ref["chain"]))

    def test_a_result_of_another_build_takes_this_builds_names(self):
        """``job_result_rebased``: a result names the build that played it (the engine's build id; each seat's build
        and fingerprint; a reference line's bot), and a release's version bump moves the build id. Rebased, a result
        recorded under another build id is this build's: it answers its spec here, and the golden jobs' are the
        results this build plays. Nothing a game decides is touched; a crash comes back as it is."""
        golden = json.loads((Path(__file__).resolve().parent.parent / "crates" / "citar-jobs" / "golden"
                             / "jobs.json").read_text(encoding="utf-8"))["jobs"]
        for name, job in golden.items():
            with self.subTest(job=name):
                rebased = engine_api.job_result_rebased(job["kind"], job["spec"], job["result"])
                chain = {"head": job["chain_head"], "rounds": job["rounds"], "digests": job["digests"]}
                engine_api.job_result_fits(job["kind"], job["spec"], rebased, chain, job["spec_hash"])
                self.assertEqual(engine_api.job_result_rebased(job["kind"], job["spec"], rebased), rebased)
        duel = golden["duel-a"]
        played = engine_api.run_job(duel["kind"], duel["spec"], spec_hash=duel["spec_hash"])
        self.assertEqual(engine_api.job_result_rebased(duel["kind"], duel["spec"], duel["result"]), played["result"])
        # Another build's names, the engine's and a seat's, are this build's again; a score is not touched.
        other = copy.deepcopy(played["result"])
        other["engine"] = "000000000000"
        other["players"]["1"]["build"] = "000000000000"
        other["players"]["1"]["fingerprint"] = "111111111111"
        self.assertEqual(engine_api.job_result_rebased("lab_game", duel["spec"], other), played["result"])
        other["players"]["0"]["score"] += 1
        self.assertEqual(engine_api.job_result_rebased("lab_game", duel["spec"], other)["players"]["0"]["score"],
                         played["result"]["players"]["0"]["score"] + 1)
        crash = {"crash": {"label": "alpha", "turn": 3, "message": "x"}}
        self.assertEqual(engine_api.job_result_rebased("lab_game", duel["spec"], crash), crash)
        for code, args in (("bad_spec", ("lab_game", {**duel["spec"], "turns": -1}, played["result"])),
                           ("bad_spec", ("bogus_game", duel["spec"], played["result"])),
                           ("bad_result", ("lab_game", duel["spec"], {"engine": float("nan")}))):
            with self.subTest(code=code, args=args[0]):
                with self.assertRaises(engine_api.JobRefused) as cm:
                    engine_api.job_result_rebased(*args)
                self.assertEqual(cm.exception.code, code)

    def test_a_reference_game_takes_list_overrides(self):
        spec = {**REFERENCE, "turn_limit": 10, "params": {"beliefs_pantheon": ["Fertility Rites", "Goddess of Love"],
                                                            "promo_lines": ["Drill", "Shock"]}}
        checked = engine_api.check_job_spec("reference_game", spec)
        self.assertEqual(checked["spec"]["params"]["promo_lines"], ["Drill", "Shock"])
        out = engine_api.run_job("reference_game", spec)
        self.assertNotIn("crash", out["result"])
        self.assertRegex(out["result"]["bot"], r"^basic-1\+[0-9a-f]{12}$")
        self.assertEqual(out["spec_hash"], checked["spec_hash"])
        engine_api.job_result_fits("reference_game", spec, out["result"], out["chain"])

    def test_json_that_does_not_read_is_refused_with_its_code(self):
        """Python's json writes NaN and Infinity, which JSON has not (a queue file may hold them), and a value nested
        past 128 levels does not read: each is the call's own refusal, never a bare ValueError."""
        spec, out = _job("lab_game")
        deep = []
        for _ in range(200):
            deep = [deep]
        cases = [
            ("bad_spec", engine_api.check_job_spec, ("lab_game", {**spec, "river_density": float("nan")})),
            ("bad_spec", engine_api.check_job_spec, ("lab_game", {**spec, "turns": float("inf")})),
            ("bad_spec", engine_api.check_job_spec, ("lab_game", {**spec, "seats": deep})),
            ("bad_spec", engine_api.run_job, ("lab_game", {**spec, "river_density": float("-inf")})),
            ("bad_result", engine_api.job_result_hash, ("lab_game", {**out["result"], "turns": float("nan")})),
            ("bad_result", engine_api.job_result_hash, ("lab_game", {**out["result"], "errors": deep})),
            ("bad_chain", engine_api.job_chain_check, (out["spec_hash"], {**out["chain"], "rounds": float("nan")})),
            ("bad_result", engine_api.job_result_fits, ("lab_game", spec, {"crash": float("nan")}, out["chain"])),
            ("bad_chain", engine_api.job_result_fits, ("lab_game", spec, out["result"], {"head": float("inf")})),
        ]
        for code, fn, args in cases:
            with self.subTest(fn=fn.__name__, code=code):
                with self.assertRaises(engine_api.JobRefused) as cm:
                    fn(*args)
                self.assertEqual(cm.exception.code, code)
                self.assertIn("is not JSON", str(cm.exception))

    def test_the_jobs_build_is_twelve_hex_digits(self):
        jb = engine_api.jobs_build()
        self.assertRegex(jb, r"^[0-9a-f]{12}$")
        self.assertEqual(engine_api.jobs_build(), jb)
        self.assertNotEqual(jb, engine_api.build_info()["build_id"], "the build id and more")


class Refusals(unittest.TestCase):
    def refused(self, kind: str, spec, reason: str, spec_hash=None):
        with self.assertRaises(engine_api.JobRefused) as cm:
            engine_api.check_job_spec(kind, spec, spec_hash)
        self.assertEqual(cm.exception.code, "bad_spec")
        self.assertIsInstance(cm.exception, ValueError)
        self.assertIn(reason, str(cm.exception))
        with self.assertRaises(engine_api.JobRefused):
            engine_api.run_job(kind, spec, spec_hash=spec_hash)

    def test_each_kind_of_bad_spec_is_refused_with_its_reason(self):
        good = lab.job_spec(recorded()[0][1]["spec"])
        checked = engine_api.check_job_spec("lab_game", good)
        self.assertEqual(checked["bots"], ["basic-1"])
        self.assertEqual(engine_api.check_job_spec("lab_game", checked["spec"], checked["spec_hash"]), checked)
        self.refused("lab_game", {**good, "path": "/etc"}, "unknown field `path`")
        self.refused("lab_game", {**good, "seats": [{**good["seats"][0], "code": "x"}]}, "unknown field `code`")
        self.refused("lab_game", {**good, "turns": 5000}, "past the speed's last turn")
        self.refused("lab_game", {**good, "i": 1_000_000}, "not below 1,000,000")
        self.refused("lab_game", {**good, "size": "planet"}, "Unknown map_size 'planet'")
        self.refused("lab_game", {**good, "seats": [{"bot": "basic-9"}]}, "unknown bot version")
        self.refused("lab_game", {**good, "seats": [{"bot": "basic", "params": {"no_such": 1}}]}, "params")
        self.refused("lab_game", {**good, "seats": [{"bot": "basic", "params": {"promo_lines": [["Drill"]]}}]},
                     "params.promo_lines holds a list or an object")
        self.refused("lab_game", {**good, "seats": [{"bot": "basic", "params": {"promo_lines": ["Teleport"]}}]},
                     "params")
        self.refused("lab_game", good, "is not the hash of the spec", spec_hash="0" * 64)
        self.refused("burn", good, "unknown job kind")
        ref = {**REFERENCE, "turn_limit": 40}
        engine_api.check_job_spec("reference_game", ref)
        for k, v in (("dir", "/tmp"), ("path", "/tmp/x.jsonl"), ("budget", 60), ("max_minutes", 5), ("workers", 4)):
            self.refused("reference_game", {**ref, k: v}, f"unknown field `{k}`")

    def test_two_thousand_mutated_specs_raise_nothing_but_bad_spec(self):
        rng = random.Random(4070)
        lab_base = lab.job_spec(recorded()[2][1]["spec"])
        lab_base["seats"][0]["params"] = {**(lab_base["seats"][0].get("params") or {}),
                                          "promo_lines": ["Shock", "Drill"]}
        ref_base = {**REFERENCE, "turn_limit": 40,
                    "params": {"tech_noise": 0.1, "policy_order_peaceful": ["Liberty", "Tradition"]}}
        deep: list = []
        for _ in range(200):
            deep = [deep]
        junk = [None, True, -1, 2**64, 1e308, -0.0, "NaN", "x" * 300, [1, "y"], {"path": "/etc/passwd"},
                {"d": {"d": {"d": 1}}}, "basic-99", 0.5, "", {}, float("nan"), float("inf"), float("-inf"), deep,
                [["a"]], ["a"] * 300]

        def paths(v, here=()):
            yield here
            if isinstance(v, dict):
                for k, x in v.items():
                    yield from paths(x, here + (k,))
            elif isinstance(v, list):
                for i, x in enumerate(v):
                    yield from paths(x, here + (i,))

        def mutate(base):
            v = copy.deepcopy(base)
            path = rng.choice(list(paths(v)))
            parent, last = None, None
            cur = v
            for step in path:
                parent, last, cur = cur, step, cur[step]
            op = rng.randrange(3)
            if op == 0 and parent is not None:
                parent[last] = rng.choice(junk)
            elif op == 1 and parent is not None:
                del parent[last]
            elif isinstance(cur, dict):
                cur[f"unknown_{rng.randrange(99)}"] = rng.choice(junk)
            else:
                v = rng.choice(junk) if parent is None else v
            return v

        outcomes = {"accepted": 0, "bad_spec": 0}
        for n in range(2000):
            kind, base = (("lab_game", lab_base), ("ranking_game", lab_base), ("reference_game", ref_base))[n % 3]
            spec = mutate(base)
            try:
                engine_api.check_job_spec(kind, spec)
                outcomes["accepted"] += 1
            except engine_api.JobRefused as e:
                self.assertEqual(e.code, "bad_spec")
                outcomes["bad_spec"] += 1
        self.assertGreater(outcomes["bad_spec"], 1000, outcomes)
        self.assertGreater(outcomes["accepted"], 10, outcomes)


class Queue(unittest.TestCase):
    """The lab's queue on jobs: an experiment is checked game by game as jobs when it is queued, and a spec a job
    refuses played in this process is a final crash. The dispatcher's side of it (an experiment queued before its
    games were checked shown as unplayable and never started; a crash, a final result) is tests/test_dispatcher.py's."""

    def setUp(self):
        self.dir = Path(tempfile.mkdtemp(prefix="citar-jobs-lab-"))
        self.addCleanup(shutil.rmtree, self.dir, ignore_errors=True)
        for name, sub in (("LAB", ""), ("QUEUE", "queue"), ("DONE", "done"), ("RESULTS", "results"),
                          ("REFERENCE", "reference")):
            patcher = mock.patch.object(lab, name, self.dir / sub)
            patcher.start()
            self.addCleanup(patcher.stop)
        lab._dirs()

    def refused(self, exp: dict, reason: str):
        with self.assertRaises(ValueError) as cm:
            lab.submit(exp)
        self.assertIn(reason, str(cm.exception))
        self.assertFalse((lab.QUEUE / f"{exp['name']}.json").exists())
        return str(cm.exception)

    def test_an_experiment_a_job_would_refuse_is_refused_when_queued(self):
        duel = {"games": 2, "size": "duel", "maps": ["pangaea"], "seats": [{"bot": "basic"}, {"bot": "basic"}]}
        why = self.refused({**duel, "name": "long", "turns": 500}, "past the speed's last turn, 330")
        self.assertIn("Game 0 of experiment long cannot be played", why)
        lab.submit({**duel, "name": "full", "turns": 330})
        # the seed of the last game, not the first, is past what a job carries
        self.assertIn("Game 1", self.refused({**duel, "name": "seedy", "seed": 2**53 - 1}, "2^53 - 1"))
        lab.submit({**duel, "name": "seed-ok", "seed": 2**53 - 2})
        self.refused({**duel, "name": "label", "seats": [{"bot": "basic", "label": "x" * 201}, {"bot": "basic"}]},
                     "more than 200")
        # a factorial experiment's labels and overrides differ from game to game: every game is checked
        names = ["Shock", "Drill", "Accuracy", "Barrage", "Bombardment", "Targeting", "Interception", "Cover"]
        self.refused({"name": "wide", "games": 3, "size": "small", "players": 4, "bot": "basic",
                      "factors": {"promo_lines": [names], "promo_in_city": [names]}},
                     "label is")
        lab.submit({"name": "lists", "games": 3, "size": "small", "players": 4, "bot": "basic",
                    "factors": {"promo_lines": [names[:2], names[2:4]], "tech_noise": [0.0, 0.2]}})
        self.assertEqual(sorted(p.stem for p in lab.QUEUE.glob("*.json")), ["full", "lists", "seed-ok"])

    def test_the_command_line_says_why_and_exits_2(self):
        spec = self.dir / "long.json"
        spec.write_text(json.dumps({"name": "long", "size": "duel", "turns": 500, "seats": [{}, {}]}), "utf-8")
        err = io.StringIO()
        with contextlib.redirect_stderr(err), self.assertRaises(SystemExit) as cm:
            lab.main(["submit", str(spec)])
        self.assertEqual(cm.exception.code, 2)
        self.assertIn("citar lab submit:", err.getvalue())
        self.assertIn("past the speed's last turn", err.getvalue())

    def queue_as_before(self, name: str, **over) -> dict:
        """An experiment queued as an earlier build queued it, without its games checked as jobs."""
        with mock.patch.object(lab, "check_games"):
            exp = lab.submit({"name": name, "games": 2, "size": "duel", "maps": ["pangaea"],
                              "seats": [{"bot": "basic"}, {"bot": "basic"}], **over})
        return exp

    def test_a_refused_spec_played_is_a_final_crash(self):
        exp = self.queue_as_before("old-seat", seats=[{"bot": "basic", "label": "x" * 300}, {"bot": "basic"}])
        line = quiet(lab.play, lab.game_spec(exp, 0))
        self.assertEqual((sorted(line), line["final"]), (["crash", "exp", "final", "finished", "i"], True))
        self.assertTrue(line["crash"].startswith("JobRefused (bad_spec): seat 0: label is 300 characters"), line)

class Speed(unittest.TestCase):
    def test_a_lab_duel_through_run_job_is_no_slower_than_through_the_old_play(self):
        """P4.7.9: the same run, alternating, five times each, medians compared; CI gets three times the bound."""
        e = lab.normalize({"name": "speed", "games": 1, "seed": 9300, "size": "duel", "maps": ["pangaea"],
                           "turns": 100, "seats": [{"label": "a", "bot": "basic"}, {"label": "b", "bot": "basic"}]})
        spec = lab.game_spec(e, 0)
        old, new = [], []
        for _ in range(5):
            t = time.perf_counter()
            lab_play_oracle.play(spec)
            old.append(time.perf_counter() - t)
            t = time.perf_counter()
            quiet(lab.play, spec)
            new.append(time.perf_counter() - t)
        bound = 1.0 * (3 if ON_CI else 1)
        ratio = statistics.median(new) / statistics.median(old)
        self.assertLessEqual(ratio, bound, f"run_job {statistics.median(new):.3f} s, old play "
                                           f"{statistics.median(old):.3f} s")


if __name__ == "__main__":
    unittest.main()
