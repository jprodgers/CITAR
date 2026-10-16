"""The server's job host (design/phase4-helper.md P4.4.3, P4.5.6; package 4-08): ``python -m citar.jobs host``, the
jobs crate's host behind the extension, started and read by :mod:`citar.jobs.host` as the helper starts and reads its
own (``citar_jobs::host::parent``). The limits themselves, per OS, are the jobs crate's tests (``tests/host.rs``);
here the server's side of them:

- **Ready:** the host's jobs build is the extension's, its memory is counted, and it says how hard its limits hold as
  P4.3.6's table says for this OS (on Linux under the scope, as far as the user's manager delegates the controllers),
  and how much memory each run may use, its own left out.
- **A lab job** through the host gives exactly what ``engine_api.run_job`` gives in this process.
- **A memory death:** a run past the host's memory ends it; the death is classified ``memory`` from what showed it,
  every run in flight on it fails ``memory``, the parent knows nearly the peak it reached, and a new host plays the
  next job.
- **Memory per run:** every thread using ``memory_per_run_mb`` at once finishes, and an offer above it is refused
  ``memory`` with no death (on Linux, with the scope and without it).
- **Pause, resume, abort** from this side: a paused run reports its pause and finishes with its paused time; an
  abort ends a run within a second and leaves nothing in flight.
- **Builds:** a host told another jobs build refuses to start, and a ``ready`` of another build is refused.
- **Ending:** a host whose input closes is gone within two seconds, its run in flight with it.
- **Overhead** (P4.7.9): a 30-round duel through the host against ``run_job`` in this process, the same run,
  alternating, five each, medians: at most 10% plus 0.2 s more (three times that on CI).
- **The parent's rules** (the scope's arguments, the CPU list, the death's classification), which are
  ``citar_jobs::host::parent``'s in Python.

Every host is held to at most two cores' worth and 512 MB (P4.7.1).
"""
from __future__ import annotations

import json
import os
import statistics
import sys
import time
import unittest
from pathlib import Path

from citar import engine_api, lab
from citar.jobs import host as H
from tests import has_test_ops

ON_CI = bool(os.environ.get("CI"))
#: The frames that end a run.
ENDS = ("job_reject", "job_result", "job_failed", "job_aborted")


def two_cores() -> int:
    """The share of the machine that is at most two cores' worth, in percent."""
    return max(1, min(100, 200 // H.machine_cpus()))


def duel_spec(turns: int = 30, seed: int = 9400) -> dict:
    e = lab.normalize({"name": "host", "games": 1, "seed": seed, "size": "duel", "maps": ["pangaea"],
                       "turns": turns, "seats": [{"label": "a", "bot": "basic"}, {"label": "b", "bot": "basic"}]})
    return lab.job_spec(lab.game_spec(e, 0))


def test_spec(kind: str, spec: dict) -> dict:
    from citar import _engine
    return json.loads(_engine.jobs.test_job_spec(kind, json.dumps(spec).encode()))


def user_manager_delegates(controller: str) -> bool:
    """Whether the user's service manager can hold ``controller`` in the scopes it makes (Linux)."""
    if not hasattr(os, "getuid"):
        return False
    uid = os.getuid()
    try:
        text = Path(f"/sys/fs/cgroup/user.slice/user-{uid}.slice/user@{uid}.service/cgroup.controllers").read_text()
    except OSError:
        return False
    return controller in text.split()


def expected_held(scope: bool) -> tuple:
    """P4.3.6's table for this OS; on Linux under the scope, the CPU is hard where the user's manager delegates the
    cpu controller, and the memory hard either way (the scope's ``MemoryMax``, else ``RLIMIT_DATA``)."""
    if os.name == "nt":
        return ("hard", "hard")
    if sys.platform.startswith("linux"):
        return ("hard", "hard") if scope and user_manager_delegates("cpu") else ("soft", "hard")
    return ("soft", "soft")


def until(host: H.JobHost, run: str, want, timeout: float = 60.0) -> dict:
    """The first frame about ``run`` that ``want`` takes, skipping the rest."""
    deadline = time.monotonic() + timeout
    while True:
        frame = host.next(max(0.0, deadline - time.monotonic()))
        if frame is None:
            raise TimeoutError(f"run {run}: nothing wanted within {timeout} s")
        if frame["t"] == "gone":
            raise AssertionError(f"the host ended: {host.death()}")
        if frame.get("run") == run and want(frame):
            return frame


class HostCase(unittest.TestCase):
    def start(self, threads: int = 1, memory_mb: int = 400, **kw) -> H.JobHost:
        host = H.JobHost(threads, two_cores(), memory_mb, **kw)
        host.start()
        self.addCleanup(host.close)
        return host


class Ready(HostCase):
    def test_a_host_is_ready_with_the_extensions_jobs_build_and_says_how_its_limits_hold(self):
        host = self.start()
        r = host.ready
        self.assertEqual(r["jobs_build"], engine_api.jobs_build())
        self.assertTrue(r["verifiable"], r)
        self.assertTrue(r["counting"], "the extension counts each run's memory")
        self.assertEqual((r["limits"]["cpu"], r["limits"]["memory"]), expected_held(host.scope), r)
        self.assertTrue(r["notes"], "how the limits are held, for the log")
        # Its own memory is measured and left out of what a run may use.
        self.assertGreater(r["own_mb"], 0, r)
        self.assertTrue(0 < host.memory_per_run_mb < 400, r)
        self.assertEqual(host.host_pid, r["pid"])
        if os.name != "nt" and not host.scope:
            self.assertEqual(r["pid"], host.pid, "the host is the child itself (a venv's launcher only on Windows)")


class LabJob(HostCase):
    def test_a_lab_job_through_the_host_is_run_jobs_result_exactly(self):
        spec = duel_spec(40)
        checked = engine_api.check_job_spec("lab_game", spec)
        here = engine_api.run_job("lab_game", spec)
        host = self.start()
        host.offer("cr_lab", "lab_game", checked["spec"], checked["spec_hash"], 600, job="cj_lab")
        frames = H.run_until(host, "cr_lab")
        self.assertEqual(frames[0]["t"], "job_accept", frames)
        self.assertEqual(frames[-1]["t"], "job_result", frames[-1])
        out = frames[-1]["outcome"]
        self.assertEqual(out["result"], here["result"])
        self.assertEqual(out["result_hash"], here["result_hash"])
        self.assertEqual(out["chain"], here["chain"])
        self.assertGreater(out["peak_mb"], 0)
        self.assertEqual(out["paused_s"], 0)
        self.assertEqual(engine_api.job_result_hash("lab_game", out["result"]), out["result_hash"])
        self.assertEqual(host.in_flight, set())


@unittest.skipUnless(has_test_ops(), "the test jobs need a build with the test operations")
class Memory(HostCase):
    def test_a_memory_death_is_classified_and_the_next_host_plays(self):
        host = self.start(memory_mb=300)
        if host.ready["limits"]["memory"] != "hard" and sys.platform != "darwin":
            self.skipTest(f"memory is not held here: {host.ready['limits']}")
        # Held, so that macOS's watchdog (every half second) sees it.
        big = test_spec("alloc", {"mb": 500, "hold_s": 3.0})
        host.offer("cr_big", "alloc", big["spec"], big["spec_hash"], 60)
        with self.assertRaises(H.HostError) as caught:
            H.run_until(host, "cr_big", timeout=60)
        death = caught.exception.death
        self.assertEqual(death.kind, "memory", death)
        if sys.platform == "darwin":
            wanted = "watchdog"
        elif sys.platform.startswith("linux") and host.scope and user_manager_delegates("memory"):
            wanted = "oom_kill"
        else:
            wanted = "allocation failed"
        self.assertIn(wanted, death.why)
        failures = host.failures(death)
        self.assertEqual([(f["run"], f["code"], f["retryable"]) for f in failures], [("cr_big", "memory", True)])
        # The run reported its peak as it grew, so this side knows nearly how much it reached.
        self.assertGreaterEqual(host.peak_mb("cr_big") or 0, host.memory_per_run_mb / 2, host.ready)
        self.next_host_plays()

    @unittest.skipUnless(sys.platform.startswith("linux"), "Linux's fallback without the scope")
    def test_without_the_scope_linux_holds_memory_by_rlimit(self):
        host = self.start(memory_mb=300, scope=False)
        self.assertEqual((host.ready["limits"]["cpu"], host.ready["limits"]["memory"]), ("soft", "hard"))
        big = test_spec("alloc", {"mb": 500})
        host.offer("cr_big", "alloc", big["spec"], big["spec_hash"], 60)
        with self.assertRaises(H.HostError) as caught:
            H.run_until(host, "cr_big", timeout=60)
        death = caught.exception.death
        self.assertEqual(death.kind, "memory", death)
        self.assertIn("allocation failed", death.why)
        self.next_host_plays(scope=False)

    def next_host_plays(self, **kw):
        again = self.start(memory_mb=300, **kw)
        small = test_spec("alloc", {"mb": 50})
        again.offer("cr_small", "alloc", small["spec"], small["spec_hash"], 60)
        out = H.run_until(again, "cr_small")[-1]
        self.assertEqual(out["t"], "job_result", out)
        self.assertAlmostEqual(out["outcome"]["peak_mb"], 50, delta=5)


@unittest.skipUnless(has_test_ops(), "the test jobs need a build with the test operations")
class Share(HostCase):
    def test_every_run_gets_its_share_at_once_and_an_offer_above_it_is_refused(self):
        scopes = [None]
        if sys.platform.startswith("linux") and H.scope_available():
            scopes.append(False)    # RLIMIT_DATA too, which counts each run thread's whole stack
        for scope in scopes:
            for threads, memory_mb in ((1, 200), (3, 300)):
                with self.subTest(scope=scope, threads=threads, memory_mb=memory_mb):
                    self.share_holds(threads, memory_mb, scope)

    def share_holds(self, threads: int, memory_mb: int, scope) -> None:
        host = self.start(threads=threads, memory_mb=memory_mb, scope=scope)
        per_run = host.memory_per_run_mb
        self.assertGreater(host.ready["own_mb"], 0, host.ready)
        self.assertTrue(8 < per_run < memory_mb // threads, host.ready)
        mb = per_run - 8
        # Held a second, so the runs overlap (and macOS's watchdog sees them).
        spec = test_spec("alloc", {"mb": mb, "hold_s": 1.0})
        runs = {f"cr_share_{i}" for i in range(threads)}
        for run in sorted(runs):
            host.offer(run, "alloc", spec["spec"], spec["spec_hash"], 60, mem_mb=mb)
        ends = {}
        deadline = time.monotonic() + 60
        while len(ends) < threads:
            frame = host.next(max(0.0, deadline - time.monotonic()))
            self.assertIsNotNone(frame, f"the runs did not end: {ends}")
            if frame["t"] == "gone":
                self.fail(f"the host ended: {host.death()}")
            if frame.get("run") in runs and frame["t"] in ENDS:
                ends[frame["run"]] = frame
        for end in ends.values():
            self.assertEqual(end["t"], "job_result", end)
            self.assertAlmostEqual(end["outcome"]["peak_mb"], mb, delta=mb * 0.1 + 1)
        over = test_spec("alloc", {"mb": 1})
        host.offer("cr_over", "alloc", over["spec"], over["spec_hash"], 60, mem_mb=per_run + 1)
        refused = H.run_until(host, "cr_over")[-1]
        self.assertEqual((refused["t"], refused.get("reason")), ("job_reject", "memory"), refused)
        last = test_spec("alloc", {"mb": mb})
        host.offer("cr_last", "alloc", last["spec"], last["spec_hash"], 60, mem_mb=mb)
        self.assertEqual(H.run_until(host, "cr_last")[-1]["t"], "job_result")
        self.assertEqual(host.in_flight, set())


@unittest.skipUnless(has_test_ops(), "the test jobs need a build with the test operations")
class PauseAndAbort(HostCase):
    def test_a_paused_run_reports_its_pause_and_finishes_with_its_paused_time(self):
        host = self.start()
        burn = test_spec("burn", {"seconds": 2.0})
        host.offer("cr_burn", "burn", burn["spec"], burn["spec_hash"], 60)
        until(host, "cr_burn", lambda f: f["t"] == "job_accept")
        host.pause()
        held = until(host, "cr_burn", lambda f: f["t"] == "job_progress" and f["paused"])
        self.assertLess(held["round"], 2, held)
        time.sleep(1.0)     # the pause's length, which paused_s must then report
        host.resume()
        frames = H.run_until(host, "cr_burn")
        self.assertTrue([f for f in frames if f["t"] == "job_progress" and not f["paused"]], frames)
        end = frames[-1]
        self.assertEqual(end["t"], "job_result", end)
        self.assertGreaterEqual(end["outcome"]["paused_s"], 0.9, end)
        self.assertGreaterEqual(end["outcome"]["wall_s"], 2.9, end)
        self.assertEqual(host.in_flight, set())

    def test_an_abort_ends_a_run_within_a_second(self):
        host = self.start()
        held = test_spec("alloc", {"mb": 8, "hold_s": 30.0})
        host.offer("cr_hold", "alloc", held["spec"], held["spec_hash"], 60)
        until(host, "cr_hold", lambda f: f["t"] == "job_accept")
        self.assertEqual(host.in_flight, {"cr_hold"})
        asked = time.perf_counter()
        host.abort("cr_hold", "test")
        end = H.run_until(host, "cr_hold", timeout=10)[-1]
        took = time.perf_counter() - asked
        self.assertEqual(end["t"], "job_aborted", end)
        self.assertLess(took, 1.0 * (3 if ON_CI else 1))
        self.assertEqual(host.in_flight, set())
        self.assertIsNone(host.peak_mb("cr_hold"))


class Builds(HostCase):
    def test_another_jobs_build_is_refused_on_both_sides(self):
        host = H.JobHost(1, two_cores(), 300, jobs_build="000000000000")
        with self.assertRaises(H.HostError) as caught:
            host.start()
        self.assertEqual((caught.exception.code, caught.exception.fatal), ("refused", "build"))
        self.assertIn("000000000000", str(caught.exception))
        # A parent that expects another build than the host's refuses its ready.
        fake = [sys.executable, "-c", "import sys; sys.stdin.readline(); print('{\"t\": \"ready\", "
                "\"limits\": {\"cpu\": \"hard\", \"memory\": \"hard\"}, \"verifiable\": true, \"jobs_build\": "
                "\"ffffffffffff\", \"threads\": 1, \"cpu_s\": 0, \"pid\": 1, \"counting\": true}', flush=True); "
                "sys.stdin.read()"]
        host = H.JobHost(1, two_cores(), 300, argv=fake, scope=False)
        with self.assertRaises(H.HostError) as caught:
            host.start()
        self.assertEqual(caught.exception.code, "build")
        self.assertIn("ffffffffffff", str(caught.exception))


    @unittest.skipUnless(has_test_ops(), "the self-check's test switch needs a build with the test operations")
    def test_the_servers_own_host_plays_when_its_self_check_fails_and_says_so(self):
        """The local executor's host (4-09) is told it plays its parent's own work: a failed self-check (a server on
        a ruleset of its own) is reported, and its jobs still play, to the result this process computes; a helper's
        host is not told so, and refuses them ``unverifiable``."""
        spec = duel_spec(30, seed=9600)
        checked = engine_api.check_job_spec("lab_game", spec)
        want = engine_api.run_job("lab_game", spec)["result_hash"]
        env = {"CITAR_JOBS_TEST_SELF_CHECK": "fail"}
        own = self.start(env=env, play_unverifiable=True)
        self.assertFalse(own.ready["verifiable"])
        self.assertIn("self-check", own.ready.get("reason") or "")
        own.offer("cr_own", "lab_game", checked["spec"], checked["spec_hash"], 600)
        end = H.run_until(own, "cr_own", 120)[-1]
        self.assertEqual((end["t"], end["outcome"]["result_hash"]), ("job_result", want))
        helper = self.start(env=env)
        helper.offer("cr_theirs", "lab_game", checked["spec"], checked["spec_hash"], 600)
        end = H.run_until(helper, "cr_theirs", 60)[-1]
        self.assertEqual((end["t"], end.get("reason")), ("job_reject", "unverifiable"))


class Ending(HostCase):
    def test_a_host_whose_input_closes_is_gone_within_two_seconds(self):
        host = self.start()
        spec = duel_spec(330)
        checked = engine_api.check_job_spec("lab_game", spec)
        host.offer("cr_long", "lab_game", checked["spec"], checked["spec_hash"], 600)
        frame = host.next(60)
        while frame and frame.get("run") != "cr_long":
            frame = host.next(60)
        self.assertEqual(frame["t"], "job_accept", frame)
        t = time.perf_counter()
        code = host.close(wait=5)
        took = time.perf_counter() - t
        self.assertEqual(code, 0)
        self.assertLess(took, 2.0 * (3 if ON_CI else 1))


class Overhead(HostCase):
    def test_a_duel_through_the_host_costs_at_most_ten_percent_and_a_fifth_of_a_second_more(self):
        """P4.7.9: the same run, alternating, five times each, medians compared; CI gets three times the bound."""
        spec = duel_spec(30, seed=9500)
        checked = engine_api.check_job_spec("lab_game", spec)
        host = self.start(memory_mb=512)
        here, there = [], []
        for n in range(5):
            t = time.perf_counter()
            engine_api.run_job("lab_game", spec)
            here.append(time.perf_counter() - t)
            run = f"cr_speed_{n}"
            t = time.perf_counter()
            host.offer(run, "lab_game", checked["spec"], checked["spec_hash"], 600)
            end = H.run_until(host, run)[-1]
            there.append(time.perf_counter() - t)
            self.assertEqual(end["t"], "job_result", end)
        a, b = statistics.median(here), statistics.median(there)
        slack = 3 if ON_CI else 1
        self.assertLessEqual(b, (a * 1.1 + 0.2) * slack, f"through the host {b:.3f} s, in this process {a:.3f} s")


class ParentRules(unittest.TestCase):
    def test_the_scope_holds_the_limits_as_properties(self):
        args = H.scope_args(2, 10, 200, 12)
        for p in ("--user", "--scope", "--collect", "--quiet", "CPUQuota=120%", "MemoryMax=200M", "MemorySwapMax=0",
                  "TasksMax=18"):
            self.assertIn(p, args)
        self.assertEqual(args[-1], "--")
        # The trial scope is made as a host's is; there is none off Linux.
        for flag in ("--user", "--scope", "--quiet", "--collect", "--"):
            self.assertIn(flag, H.SCOPE_TRIAL)
        if not sys.platform.startswith("linux"):
            self.assertFalse(H.scope_available())

    def test_cpu_lists_count_as_the_kernel_writes_them(self):
        self.assertEqual(H.parse_cpu_list("0-11\n"), 12)
        self.assertEqual(H.parse_cpu_list("0,2-5"), 5)
        self.assertEqual(H.parse_cpu_list("x"), 0)
        self.assertGreaterEqual(H.machine_cpus(), 1)

    def test_deaths_are_classified_from_what_shows_them(self):
        self.assertEqual(H.classify(None, ["ready", "memory allocation of 8388608 bytes failed"]).kind, "memory")
        oom = H.classify(-9, [], oom=(2, 3))
        self.assertEqual(oom.kind, "memory")
        self.assertIn("oom_kill", oom.why)
        self.assertEqual(H.classify(-9, [], oom=(3, 3)).kind, "other")
        # A neighbour's kill in the same parent cgroup is no evidence for a host that ended otherwise.
        self.assertEqual(H.classify(-6, [], oom=(2, 3)).kind, "other")
        self.assertEqual(H.classify(1, [], oom=(2, 3)).kind, "other")
        self.assertEqual(H.classify(None, [], oom=(2, 3)).kind, "other")
        self.assertEqual(H.classify(None, [f"{H.WATCHDOG_LINE}: 300 MB of 200 MB"]).kind, "memory")
        self.assertEqual(H.classify(H.EXIT_MEMORY, []).kind, "memory")
        quoted = ["thread panicked: memory allocation of 8 bytes failed, said the bot"]
        self.assertEqual(H.classify(1, quoted).kind, "other")
        refused = H.classify(2, [], fatal={"code": "build", "message": "another build"})
        self.assertEqual((refused.kind, refused.code), ("refused", "build"))
        other = H.classify(1, ["one", "two"])
        self.assertTrue(other.why.endswith("two"), other)
        self.assertTrue(H.HostError("refused", "x", fatal="limits").limits)
        self.assertTrue(H.HostError("died", "x", death=H.Death("memory", "y")).limits)
        self.assertFalse(H.HostError("build", "x").limits)

    def test_the_module_starts_only_as_the_host(self):
        from citar.jobs import __main__ as M
        self.assertEqual(M.main([]), 2)
        self.assertEqual(M.main(["other"]), 2)


if __name__ == "__main__":
    unittest.main()
