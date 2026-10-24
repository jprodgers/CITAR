"""Trust tiers' rules, the keyed sample and the claim's check (design/phase4-helper.md P4.5.8, P4.4.4; package 4-11's
gate 1).

- **The tier machine** (``citar.jobs.trust``), a table of cases: each transition (``new`` to ``provisional`` at
  ``new_streak`` matches, ``provisional`` to ``trusted`` at ``promote_verified`` verified matches with no mismatch and
  no over-reporting within ``overreport_days``, any tier to ``quarantined`` on a mismatch, release back to ``new`` with
  the counters reset and ``since`` now), each threshold at and below it, a match against a machine that is no
  verifier not counted, a quarantined verifier's matches taken back (a machine whose promotion to its present tier
  counted them back to ``new`` below that tier's threshold; one an administrator set that tier for, or whose lost
  matches all came after its promotion, keeping it), the settings' bounds; a key reset keeping the tier is
  ``tests/test_verification.py``'s, through its route.
- **Sampling** over 10,000 job ids, for each rate p of 0.02 and 0.10 and two secrets: the observed rate within
  4·sqrt(p(1−p)/n) of p, and the two secrets sampling different jobs.
- **Claims** through the binding (``engine_api.verify_claim``): an honest claim of the golden duel verifies; a changed
  byte in any claim field (not signed again: a bad signature; signed again: the claim names what the records do not),
  in the result, in a digest or in the signature fails with its verdict and its reason, and so does an outcome whose
  own result hash is not its result's (the honest hash beside an altered result, hashed into its signed claim).
"""
from __future__ import annotations

import tests  # noqa: F401  (temporary saves folder and registry; must be imported before citar)

import json
import math
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

from citar import engine_api
from citar.jobs import trust as T
from tests import has_test_ops

ROOT = Path(__file__).resolve().parent.parent
NOW = datetime(2026, 10, 16, 12, 0, tzinfo=timezone.utc)


def settings(**over) -> dict:
    return {**T.DEFAULTS, **over}


class Tiers(unittest.TestCase):
    def matches(self, tier: str, trust: dict, n: int, s: dict, *, counts: bool = True, flags=None) -> tuple:
        promotions = []
        for _ in range(n):
            tier, trust, promoted = T.on_match(tier, trust, s, counts=counts, flags=flags, now=NOW)
            if promoted:
                promotions.append(promoted)
        return tier, trust, promotions

    def test_each_transition_at_its_threshold(self):
        s = settings(new_streak=5, promote_verified=50)
        cases = [
            # (tier, trust, matches, the tier after, promotions)
            ("new", {}, 4, "new", []),
            ("new", {}, 5, "provisional", ["provisional"]),
            ("new", {"streak": 4, "verified": 4}, 1, "provisional", ["provisional"]),
            ("provisional", {"streak": 5, "verified": 48}, 1, "provisional", []),
            ("provisional", {"streak": 5, "verified": 49}, 1, "trusted", ["trusted"]),
            ("provisional", {"streak": 5, "verified": 49, "mismatches": 1}, 3, "provisional", []),
            ("trusted", {"streak": 60, "verified": 60}, 2, "trusted", []),
            ("quarantined", {"streak": 0, "verified": 3}, 9, "quarantined", []),
        ]
        for tier, trust, n, after, promotions in cases:
            got, out, promoted = self.matches(tier, dict(trust), n, s)
            self.assertEqual((got, promoted), (after, promotions), (tier, trust, n))
            if tier != "quarantined":
                self.assertEqual(out["verified"], int(trust.get("verified", 0)) + n)
                self.assertEqual(out["last_verified_at"], NOW.isoformat(timespec="seconds"))
            if promotions:
                # a promotion keeps its tier and when the match that made it was counted, to the microsecond
                self.assertEqual(out["promoted"], {"tier": after, "at": NOW.isoformat(timespec="microseconds")})
                self.assertEqual(T.promoted_at(after, out), NOW)
            else:
                self.assertIsNone(T.promoted_at(got, out))

    def test_one_step_a_match_and_the_lowered_thresholds(self):
        """Gate 6's thresholds (3 and 6): three matches make a new machine provisional, three more trusted, each
        step once."""
        s = settings(new_streak=3, promote_verified=6)
        tier, trust, promotions = self.matches("new", {}, 6, s)
        self.assertEqual((tier, promotions), ("trusted", ["provisional", "trusted"]))
        self.assertEqual((trust["verified"], trust["streak"]), (6, 6))

    def test_a_match_against_a_machine_that_is_no_verifier_counts_nothing(self):
        tier, trust, promotions = self.matches("new", {"streak": 4, "verified": 4}, 3, settings(), counts=False)
        self.assertEqual((tier, trust, promotions), ("new", {"streak": 4, "verified": 4}, []))

    def test_over_reporting_within_its_days_blocks_promotion_to_trusted(self):
        s = settings(promote_verified=10, overreport_days=30)
        base = {"streak": 9, "verified": 9}
        for flag in ("overreports", "overreported_cpu"):
            recent = {flag: {"count": 3, "last": (NOW - timedelta(days=29)).isoformat()}}
            self.assertEqual(self.matches("provisional", dict(base), 1, s, flags=recent)[0], "provisional", flag)
            old = {flag: {"count": 3, "last": (NOW - timedelta(days=31)).isoformat()}}
            self.assertEqual(self.matches("provisional", dict(base), 1, s, flags=old)[0], "trusted", flag)
        # it never blocks the first step, and never quarantines
        recent = {"overreports": {"last": NOW.isoformat()}}
        self.assertEqual(self.matches("new", {"streak": 4}, 1, settings(), flags=recent)[0], "provisional")

    def test_a_mismatch_quarantines_and_a_release_starts_again(self):
        ev = T.evidence("chain", run="cr_x", job="cj_x", round=12, now=NOW)
        self.assertEqual(ev["words"], "the chains parted at round 12: faulty hardware or an incompatible build")
        unknown = T.evidence("chain", run="cr_x", job="cj_x", now=NOW)
        self.assertEqual(unknown["words"], "the chains differ (the round where they parted is not known): faulty "
                                           "hardware or an incompatible build")
        self.assertNotIn("round", unknown)
        tier, trust = T.on_mismatch({"streak": 7, "verified": 7, "verifier": True}, ev)
        self.assertEqual(tier, "quarantined")
        self.assertEqual((trust["mismatches"], trust["streak"], trust["verifier"]), (1, 0, False))
        self.assertEqual(trust["last_mismatch"], ev)
        self.assertEqual(T.evidence("result")["words"],
                         "same chain, different result: tampering or an incompatible build")
        self.assertIn("a bad signature", T.evidence("signature")["words"])
        self.assertEqual([v for v in T.VERDICTS if T.tampering(v)],
                         ["result", "signature", "claim", "chain_inconsistent"])
        # an administrator's quarantine is no mismatch of the machine's
        _t, by_hand = T.on_mismatch({}, T.evidence("administrator"))
        self.assertEqual(by_hand.get("mismatches", 0), 0)
        tier, released = T.release({**trust, "abandons": 2, "claim_failures": 1, "replicate": 3}, NOW)
        self.assertEqual(tier, "new")
        self.assertEqual({k: released[k] for k in T.COUNTERS}, dict.fromkeys(T.COUNTERS, 0))
        self.assertEqual((released["since"], released["verifier"], released["replicate"]),
                         (NOW.isoformat(timespec="seconds"), False, 0))
        self.assertEqual(released["last_mismatch"], ev, "its evidence stays")
        self.assertEqual(T.since(released), NOW)
        self.assertIsNone(T.since({}), "no release: every result since its registration")

    def test_a_quarantined_verifiers_matches_taken_back(self):
        s = settings(new_streak=5, promote_verified=50)
        cases = [
            # (tier, trust, matches lost, of them counted toward its present tier's promotion, the tier after)
            ("provisional", {"streak": 7, "verified": 7}, 2, 2, "provisional"),
            ("provisional", {"streak": 6, "verified": 6}, 2, 2, "new"),
            ("trusted", {"streak": 60, "verified": 60}, 10, 10, "trusted"),
            ("trusted", {"streak": 60, "verified": 55, "verifier": True}, 10, 3, "new"),
            ("new", {"streak": 2, "verified": 2}, 5, 0, "new"),
            # an administrator set its tier (no promotion counted them): its counters fall, its tier and mark stay
            ("trusted", {"streak": 1, "verified": 1, "verifier": True}, 1, 0, "trusted"),
            ("provisional", {"streak": 2, "verified": 2}, 2, 0, "provisional"),
            # every lost match came after its promotion: they did not earn its tier
            ("trusted", {"streak": 55, "verified": 55, "verifier": True}, 10, 0, "trusted"),
        ]
        for tier, trust, n, counted, after in cases:
            got, out, demoted = T.lose_matches(tier, dict(trust), n, s, counted=counted)
            self.assertEqual(got, after, (tier, trust, n, counted))
            self.assertEqual(demoted, after == "new" and tier != "new")
            self.assertEqual(out["verified"], max(0, trust["verified"] - n))
            if demoted:
                self.assertFalse(out["verifier"])
                self.assertNotIn("promoted", out)
            else:
                self.assertEqual(out.get("verifier"), trust.get("verifier"))

    def test_a_promotion_is_kept_until_the_tier_moves_otherwise(self):
        s = settings(new_streak=1, promote_verified=2)
        tier, trust, _p = self.matches("new", {}, 1, s)
        self.assertEqual((tier, T.promoted_at("provisional", trust)), ("provisional", NOW))
        self.assertIsNone(T.promoted_at("trusted", trust), "a record of another tier is no promotion to this one")
        later = NOW + timedelta(minutes=5)
        tier, trust, promoted = T.on_match(tier, trust, s, counts=True, now=later)
        self.assertEqual((tier, promoted, T.promoted_at("trusted", trust)), ("trusted", "trusted", later))
        _t, quarantined = T.on_mismatch(trust, T.evidence("chain", round=3))
        self.assertNotIn("promoted", quarantined)
        _t, released = T.release(trust)
        self.assertNotIn("promoted", released)
        self.assertIsNone(T.promoted_at("trusted", {"promoted": {"tier": "trusted", "at": "not a time"}}))

    def test_replication_by_tier_and_an_administrators_request(self):
        s = settings(provisional_rate=0.0, trusted_rate=0.0)
        self.assertEqual(T.replicate("new", "cj_1", s, "secret"), "new")
        self.assertIsNone(T.replicate("provisional", "cj_1", s, "secret"))
        self.assertIsNone(T.replicate("trusted", "cj_1", s, "secret"))
        self.assertEqual(T.replicate("trusted", "cj_1", s, "secret", asked=2), "asked")
        always = settings(provisional_rate=1.0, trusted_rate=1.0)
        self.assertEqual(T.replicate("provisional", "cj_1", always, "secret"), "sampled")
        self.assertEqual(T.rate("new", s), 1.0)

    def test_the_settings_bounds(self):
        self.assertEqual(T.clean({"provisional_rate": 1, "new_streak": 3}), {"provisional_rate": 1, "new_streak": 3})
        for bad in ({"trusted_rate": 1.5}, {"new_streak": 0}, {"new_streak": 10_001}, {"new_streak": 2.5},
                    {"new_verify_share": -0.1}, {"trusted_rate": True}, {"rate": 0.1}, {"trusted_rate": "0.1"}):
            with self.assertRaises(T.SettingsError, msg=str(bad)):
                T.clean(bad)

    def test_a_verifier_is_trusted_and_marked(self):
        self.assertTrue(T.is_verifier("trusted", {"verifier": True}))
        self.assertFalse(T.is_verifier("trusted", {}))
        self.assertFalse(T.is_verifier("provisional", {"verifier": True}))


class Speed(unittest.TestCase):
    """A machine's measured speed and the class it is compared with (P4.5.7), as 4-12's review left them: a helper's
    figures reach only so far, and the bar is set by machines that did not over-report, trusted ones first."""

    NOW = datetime(2026, 10, 17, tzinfo=timezone.utc)

    def machine(self, speed: float, tier: str = "new", overreported_days_ago: float | None = None):
        from types import SimpleNamespace
        flags = {}
        if overreported_days_ago is not None:
            flags["overreported_cpu"] = {"count": 1, "last": (self.NOW - timedelta(days=overreported_days_ago))
                                         .isoformat()}
        return SimpleNamespace(trust_tier=tier, trust={"speed": speed}, flags=flags)

    def test_a_sample_is_held_within_its_bounds(self):
        self.assertEqual(T.speed_sample({}, 1.0, 1000.0)["speed"], T.SPEED_MIN, "cpu:1000")
        self.assertEqual(T.speed_sample({}, 1000.0, 1.0)["speed"], T.SPEED_MAX)
        self.assertEqual((T.SPEED_MIN, T.SPEED_MAX), (0.05, 20.0))
        self.assertEqual(T.speed_sample({}, 1.0, 0.1), {}, "under the floor: nothing")

    def test_the_bar_leaves_out_over_reporters_and_prefers_trusted_machines(self):
        cheat = self.machine(0.05, overreported_days_ago=2)
        old = self.machine(0.9, overreported_days_ago=40)       # past overreport_days: counted again
        honest = self.machine(1.0)
        self.assertEqual(T.class_bar([cheat, old, honest], self.NOW, 30), [old, honest])
        fast = [self.machine(5.0), self.machine(5.0)]           # a group under-reporting its CPU
        trusted = self.machine(1.0, "trusted")
        self.assertEqual(T.class_bar(fast + [trusted], self.NOW, 30), [trusted])
        peers = [m.trust["speed"] for m in T.class_bar(fast + [trusted], self.NOW, 30)]
        self.assertIsNone(T.class_suspect(0.6, peers), "an honest machine at 0.6 is in line with the trusted one")
        self.assertEqual(T.class_suspect(0.6, [5.0, 5.0, 1.0]), 5.0, "as the under-reporters would have it")
        self.assertEqual(T.class_bar(fast, self.NOW, 30), fast, "no trusted machine: every one that did not "
                                                                 "over-report")
        self.assertEqual(T.class_bar([cheat], self.NOW, 30), [])


class Sampling(unittest.TestCase):
    N = 10_000

    def test_the_rate_holds_and_two_secrets_sample_different_jobs(self):
        ids = [f"cj_{i:016x}" for i in range(self.N)]
        for p in (0.02, 0.10):
            picked = {}
            for secret in ("one server's secret-key-of-enough-length", "another server's secret-key-too"):
                chosen = {j for j in ids if T.sampled(secret, j, p)}
                observed = len(chosen) / self.N
                bound = 4 * math.sqrt(p * (1 - p) / self.N)
                self.assertLessEqual(abs(observed - p), bound, f"p {p}: observed {observed:.4f}, bound {bound:.4f}")
                picked[secret] = chosen
            a, b = picked.values()
            self.assertNotEqual(a, b)
            # independent samples share about p of each other's jobs, nowhere near all of them
            self.assertLess(len(a & b), 0.5 * min(len(a), len(b)), (p, len(a & b), len(a), len(b)))
        fractions = [engine_api.sample_fraction("s", j) for j in ids[:200]]
        self.assertTrue(all(0.0 <= x < 1.0 for x in fractions))
        self.assertEqual(fractions, [engine_api.sample_fraction("s", j) for j in ids[:200]], "the same each time")


@unittest.skipUnless(has_test_ops(), "needs the extension's test operations (the throwaway keys)")
class Claims(unittest.TestCase):
    """The binding's ``verify_claim`` on the golden duel's run, signed by a throwaway key."""

    @classmethod
    def setUpClass(cls):
        from tests import helperfake as H
        golden = json.loads((ROOT / "crates" / "citar-jobs" / "golden" / "jobs.json").read_text(encoding="utf-8"))
        job = golden["jobs"]["duel-a"]
        cls.spec, cls.spec_hash = job["spec"], job["spec_hash"]
        out = engine_api.run_job("lab_game", job["spec"], spec_hash=job["spec_hash"])
        cls.offer = {"job": "cj_0123456789abcdef", "run": "cr_0123456789abcdef", "kind": "lab_game",
                     "spec": job["spec"], "spec_hash": job["spec_hash"], "jobs_build": engine_api.jobs_build()}
        cls.frame = H.result_frame(cls.offer, out, "sv_claims")
        cls.key = H.test_key()
        cls.H = H

    def check(self, claim=None, signature=None, result=None, chain=None, key=None, result_hash=None):
        f = self.frame
        engine_api.verify_claim("lab_game", self.spec, self.spec_hash, job=self.offer["job"], run=self.offer["run"],
                                machine="sv_claims", jobs_build=self.offer["jobs_build"],
                                public_key=key or self.key, claim=claim or f["claim"],
                                signature=signature if signature is not None else f["signature"],
                                result=result or f["outcome"]["result"],
                                result_hash=result_hash if result_hash is not None else f["outcome"]["result_hash"],
                                chain=chain or f["outcome"]["chain"])

    def refused(self, verdict: str, words: str, **kw) -> None:
        with self.assertRaises(engine_api.ClaimRefused) as caught:
            self.check(**kw)
        self.assertEqual(caught.exception.verdict, verdict, str(caught.exception))
        self.assertIn(words, str(caught.exception))

    @staticmethod
    def one_byte(text: str) -> str:
        return text[:-1] + ("1" if text[-1] == "0" else "0")

    def test_an_honest_claim_verifies(self):
        self.check()

    def test_a_changed_byte_in_any_claim_field(self):
        named = {"job": "job", "run": "run", "machine": "machine", "spec_hash": "spec hash",
                 "jobs_build": "jobs build", "result_hash": "claim names result", "chain_head": "claim names a chain",
                 "key": "key"}
        for field, words in named.items():
            claim = dict(self.frame["claim"], **{field: self.one_byte(self.frame["claim"][field])})
            self.refused("signature", "bad signature", claim=claim)
            self.refused("claim", words, claim=claim, signature=self.H.sign_claim(claim))
        for field, value in (("kind", "ranking_game"), ("v", 2), ("rounds", 31), ("bots", ["basic1", "idle"])):
            claim = dict(self.frame["claim"], **{field: value})
            self.refused("signature", "bad signature", claim=claim)
            self.refused("claim", "", claim=claim, signature=self.H.sign_claim(claim))

    def test_a_changed_result_digest_or_signature(self):
        result = json.loads(json.dumps(self.frame["outcome"]["result"]))
        first = next(iter(result["players"].values()))
        first["score"] += 1
        self.refused("claim", "hashes to", result=result)
        chain = json.loads(json.dumps(self.frame["outcome"]["chain"]))
        chain["digests"][11] = self.one_byte(chain["digests"][11])
        self.refused("chain_inconsistent", "do not fold", chain=chain)
        import base64
        sig = bytearray(base64.b64decode(self.frame["signature"]))
        sig[3] ^= 1
        self.refused("signature", "bad signature", signature=base64.b64encode(bytes(sig)).decode())
        self.refused("signature", "bad signature",
                     signature=self.H.sign_claim(self.frame["claim"], seed=self.H.TEST_KEY_SEED + 1))
        self.refused("signature", "registered key", key="AAAA")
        self.refused("signature", "not text", signature=5)

    def test_an_outcome_whose_own_hash_is_not_its_results(self):
        """The outcome's ``result_hash`` is what the server keeps and compares: an altered result, hashed into a claim
        signed again with the right key, beside the honest hash in the outcome, is a ``claim`` verdict, not a result
        that a matching replica could settle as verified."""
        honest = self.frame["outcome"]["result_hash"]
        result = json.loads(json.dumps(self.frame["outcome"]["result"]))
        next(iter(result["players"].values()))["score"] += 1
        altered = engine_api.job_result_hash("lab_game", result)
        claim = dict(self.frame["claim"], result_hash=altered)
        signature = self.H.sign_claim(claim)
        self.check(claim=claim, signature=signature, result=result, result_hash=altered)    # consistent: it holds
        self.refused("claim", "the outcome names result", claim=claim, signature=signature, result=result,
                     result_hash=honest)
        self.refused("claim", "the outcome names result", result_hash="d" * 64)
        self.refused("claim", "not text", result_hash=7)


if __name__ == "__main__":
    unittest.main()
