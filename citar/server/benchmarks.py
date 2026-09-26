"""Benchmark suites, runs and the scheduler that plays them.

A *suite* is a saved configuration: servers from the server registry (each with the models to test on it, and a
load profile per model) and scenarios (map, opponents, turn limit). Starting a suite creates a *run*: one *job* per
scenario x model x repeat. Each job is an ordinary game (the model against scripted bots) that shows up in the lobby
and can be watched live.

The scheduler runs jobs sequentially, or in parallel across servers (up to each server's `max_parallel`). When a
server's restricted hours begin, its games finish the model turn in progress (up to the server's grace minutes), then
pause, and its models are unloaded if the server asks for it; they resume when the window ends. Runs are persisted, so
multi-day runs survive server restarts: games in progress are reloaded from their autosaves.
"""
from __future__ import annotations

import copy
import json
import re
import secrets
import threading
import time
import traceback
from datetime import datetime
from pathlib import Path
from typing import Optional

from .. import paths
from .. import servers as REG
from .session import SAVE_DIR, SessionManager, GameSession

def _bench_dir() -> Path:
    """Where benchmark suites and runs are stored.

    Writable state must not live inside the install tree. On a deployed server the code directory is
    mounted read-only (systemd ProtectSystem=strict), which is what we want — a bug in the web app
    should not be able to rewrite the application it is running from. So the directory follows
    CITAR_BENCH_DIR, then the save directory, and only falls back to the repository layout for a
    checkout being run in place.
    """
    return paths.bench_path()


BENCH_DIR = _bench_dir()

DEFAULT_SETTINGS = {
    "score_weights": {"benchmark": 0.6, "reliability": 0.25, "speed": 0.15},
}

ACTIVE = ("loading", "running", "resuming", "paused")      # job states that hold a slot on their server
FINISHED_SESSION_GRACE = 600                                 # seconds a finished benchmark game stays open for viewers


def _new_id(prefix: str = "") -> str:
    """A new opaque id with a prefix."""
    return prefix + secrets.token_hex(4)


def _now() -> float:
    """The current time, in one place so tests can see it."""
    return time.time()


# ----------------------------------------------------------------------------
# suites
# ----------------------------------------------------------------------------
BENCH_SPEED = "Quick"          # benchmarks play UnCiv's Quick speed...
BENCH_TURNS = 330              # ...up to its time-victory turn
BENCH_NATION = "BenchmarkCiv"  # every seat plays the civilization without special abilities (override per scenario)


def default_scenario(**kw) -> dict:
    """A scenario with defaults that produce a short, meaningful benchmark game."""
    sc = {"id": _new_id("sc_"), "name": "Duel — continents", "enabled": True, "map_size": "duel", "map_type": "continents",
          "speed": BENCH_SPEED, "difficulty": "Prince", "barbarians": "normal", "opponents": 1, "bot_aggression": 0.4,
          "seed": 7, "turn_limit": BENCH_TURNS, "max_turn_minutes": 30,
          "victories": {"Scientific": True, "Cultural": True, "Domination": True, "Diplomatic": True, "Time": True},
          "city_states": None, "religion": True, "espionage": True, "tech_trading": True, "ruins": True,
          "nations": BENCH_NATION}
    sc.update(kw)
    return sc


def _scenario_speed(sc: dict) -> str:
    """The game speed a scenario runs at, defaulting to Quick."""
    from .. import engine_api
    return engine_api.resolve_name("speed", sc.get("speed")) or BENCH_SPEED


def _seat_nations(sc: dict, n: int) -> list:
    """Nations for the seats (model first): 'BenchmarkCiv' (default), 'random', or a list of nation names."""
    v = sc.get("nations", BENCH_NATION)
    if isinstance(v, list):
        return [(v[i] if i < len(v) else None) for i in range(n)]
    if v in (None, "", "random", "Random"):
        return [None] * n
    return [v] * n


def default_model(model_id: str = "", **kw) -> dict:
    """A model in a suite: a registry model (model_id) with a load profile and optional per-suite overrides."""
    m = {"id": _new_id("sm_"), "model_id": model_id, "model": "", "profile_id": None, "enabled": True, "label": "",
         "persona": "", "reasoning_effort": "", "effort": "", "tool_mode": ""}
    m.update(kw)
    return m


def resolve_group(group: dict) -> dict:
    """A suite's server group joined with the registry: display fields and each model's key (so runs keep a readable
    record even if the registry changes later)."""
    sv = REG.find(group.get("server_id"))
    if sv is None:
        from ..pool import seats as pool_seats
        pooled = pool_seats.lookup(group.get("server_id"))
        if pooled is not None:
            return _resolve_pooled(group, pooled)
    out = {"id": group.get("server_id"), "server_id": group.get("server_id"), "missing": sv is None,
           "name": sv["name"] if sv else f"(deleted server {group.get('server_id')})",
           "provider": sv["connection"]["provider"] if sv else None, "base_url": sv["connection"].get("base_url") if sv else None,
           "max_parallel": sv["connection"].get("max_parallel", 1) if sv else 1, "models": []}
    for m in group.get("models") or []:
        mm = dict(m)
        entry = REG.model_entry(sv, m.get("model_id") or m.get("model")) if sv else None
        if entry:
            mm["model_id"], mm["model"] = entry["id"], entry["key"]
            prof = REG.profile(entry, m.get("profile_id"))
            mm["profile_id"] = prof["id"] if prof else None
            mm["profile_name"] = prof["name"] if prof else None
            mm["label"] = m.get("label") or entry.get("label") or entry["key"]
        else:
            mm["missing"] = True
        out["models"].append(mm)
    return out


def _resolve_pooled(group: dict, pooled: dict) -> dict:
    """A suite group on a machine from the Servers page, played through its helper.

    Models are named by key (what the helper reports); there are no load profiles, because CITAR does not
    load or unload models on somebody else's machine. How many games may run on it at once is its
    registration's max_parallel.
    """
    from ..pool import seats as pool_seats
    known = {m.get("key") for m in (pooled["config"].get("models") or [])} | \
        {m.get("key") for m in pool_seats.live_models(pooled["id"])}
    out = {"id": pooled["id"], "server_id": pooled["id"], "missing": False, "pooled": True, "name": pooled["name"],
           "provider": "worker", "base_url": None, "max_parallel": pool_seats.max_parallel(pooled), "models": []}
    for m in group.get("models") or []:
        mm = dict(m)
        key = m.get("model") or m.get("model_id")
        mm["model_id"] = mm["model"] = key
        mm["profile_id"] = mm["profile_name"] = None
        mm["label"] = m.get("label") or key
        if key not in known:
            mm["missing"] = True
        out["models"].append(mm)
    return out


def normalize_suite(d: dict) -> dict:
    """Fill defaults and ids so hand-written or older suite files keep working."""
    s = {"id": d.get("id") or _new_id("suite_"), "name": (d.get("name") or "Untitled suite").strip()[:80],
         "description": d.get("description") or "", "mode": d.get("mode") if d.get("mode") in ("sequential", "parallel") else "sequential",
         "repeats": max(1, min(20, int(d.get("repeats") or 1))), "created": d.get("created") or _now(), "updated": _now()}
    s["servers"] = []
    for sv in d.get("servers") or []:
        if not sv.get("server_id"):
            continue            # pre-registry suites (inline URLs) were archived with the upgrade
        group = {"server_id": sv["server_id"], "models": []}
        for m in sv.get("models") or []:
            if isinstance(m, str):
                m = {"model_id": m}
            mm = default_model()
            mm.update({k: v for k, v in m.items() if k in mm})
            mm["id"] = m.get("id") or _new_id("sm_")
            if mm["model_id"] or mm["model"]:
                group["models"].append(mm)
        s["servers"].append(group)
    s["scenarios"] = []
    for sc in d.get("scenarios") or []:
        base = default_scenario()
        base.update(sc)
        base["id"] = sc.get("id") or _new_id("sc_")
        base["opponents"] = max(1, min(23, int(base.get("opponents") or 1)))
        base["turn_limit"] = max(0, int(base.get("turn_limit") or 0))
        base["speed"] = _scenario_speed(base)        # older suites used CIGAR speeds ("normal", ...)
        base.pop("villages", None)
        s["scenarios"].append(base)
    if not s["scenarios"]:
        s["scenarios"].append(default_scenario())
    return s


# ----------------------------------------------------------------------------
# scheduler
# ----------------------------------------------------------------------------
class BenchmarkScheduler:
    """Runs benchmark suites: which game starts where, and what happens when things stop.

    The hard part is not starting games, it is surviving everything that interrupts them. A run may
    last days, and in that time the process will be restarted, a GPU will enter its restricted hours,
    a model will fail to load, and somebody will pause a job to look at it.

    So a run is a document on disk rather than state in memory. Every job records its game id, and on
    startup the games that were in progress are reloaded from their autosaves and continue. Pausing a
    job keeps its slot - the model stays loaded and assigned - because releasing it would mean
    reloading a 20 GB model to answer one turn.

    Progress is written at most once a minute: it changes every few seconds, and the project folder may
    be synced to a cloud drive that would otherwise be copying a run file continuously.
    """
    def __init__(self, manager: SessionManager, directory: Path = BENCH_DIR, autostart: bool = True):
        self.manager = manager
        self.dir = Path(directory)
        self.suites_dir = self.dir / "suites"
        self.runs_dir = self.dir / "runs"
        self.lock = threading.RLock()
        self.runs: dict[str, dict] = {}
        self.settings = copy.deepcopy(DEFAULT_SETTINGS)
        self._restricted: dict = {}     # server id -> {since, unloaded} while in restricted hours
        self.log: list[dict] = []
        self._dirty: set[str] = set()
        self._stop = threading.Event()
        self._thread: Optional[threading.Thread] = None
        self._progress_at: dict[str, float] = {}
        self._progress_dirty: set[str] = set()
        self._progress_flushed = 0.0
        self._clock = datetime.now     # overridable in tests
        self._load()
        from ..pool import queue as work_queue
        work_queue.register("benchmark", self.queue_items, self.running_items)
        if autostart:
            self.start()

    def queue_items(self) -> list[dict]:
        """Queued jobs - and jobs that paused to make way for higher-priority work - for the shared work queue."""
        with self.lock:
            return [self._item(run, job) for run in self.runs.values() if run["status"] == "running"
                    for job in run["jobs"] if job["status"] == "queued" or self._preempted(job)]

    def running_items(self) -> list[dict]:
        """Jobs using their machine now, for the queue page."""
        with self.lock:
            return [dict(self._item(run, job), state=self._state(job)) for run in self.runs.values()
                    for job in run["jobs"] if job["status"] in ACTIVE and not self._preempted(job)]

    @staticmethod
    def _state(job: dict) -> str:
        """What a job holding its machine is doing, in words for the queue page."""
        if job["status"] != "paused":
            return job["status"]
        return {"restricted": "paused (quiet hours)", "quiet": "paused (quiet hours)", "user": "paused by a person",
                "disconnect": "paused (model server unreachable)"}.get(job.get("pause_reason"), "paused")

    @staticmethod
    def _preempted(job: dict) -> bool:
        """Whether a job paused to make way for higher-priority work on its machine."""
        return job["status"] == "paused" and job.get("pause_reason") == "preempted"

    def _item(self, run: dict, job: dict) -> dict:
        """A job as the shared queue lists it."""
        return {"kind": "benchmark", "id": job["id"], "group": run["id"], "run": run["name"],
                "label": f"{job['label']} · {job['scenario_name']} #{job['repeat']}",
                "server_id": self._machine(job) if job.get("game_id") else job["server_key"],
                "server": job.get("server_name"), "created": job["created"],
                "waiting": job.get("waiting") or ("paused for higher-priority work" if self._preempted(job) else None)}

    # ------------------------------------------------------------------ persistence
    def _load(self):
        """Read suites and runs from disk at startup."""
        self.suites_dir.mkdir(parents=True, exist_ok=True)
        self.runs_dir.mkdir(parents=True, exist_ok=True)
        sp = self.dir / "settings.json"
        if sp.exists():
            try:
                saved = json.loads(sp.read_text(encoding="utf-8"))
                for k, v in saved.items():
                    if isinstance(v, dict) and isinstance(self.settings.get(k), dict):
                        self.settings[k].update(v)
            except Exception:
                self._log("Could not read benchmark settings; using defaults.")
        for p in sorted(self.runs_dir.glob("*.json")):
            try:
                run = json.loads(p.read_text(encoding="utf-8"))
                self.runs[run["id"]] = run
            except Exception:
                self._log(f"Could not read run file {p.name}.")

    def _write(self, path: Path, data):
        """Write a JSON file atomically."""
        tmp = path.with_suffix(".tmp")
        tmp.write_text(json.dumps(data, indent=2, default=str), encoding="utf-8")
        tmp.replace(path)

    def _save_run(self, run: dict):
        """Write one run to disk."""
        self._write(self.runs_dir / f"{run['id']}.json", run)

    def _flush(self):
        # live progress changes every few seconds; write it at most once a minute (the project may live in a synced folder)
        """Write the runs that have changed, at most once a minute."""
        if self._progress_dirty and _now() - self._progress_flushed > 60:
            self._dirty |= self._progress_dirty
            self._progress_dirty.clear()
            self._progress_flushed = _now()
        for rid in list(self._dirty):
            run = self.runs.get(rid)
            if run is not None:
                try:
                    self._save_run(run)
                except Exception:
                    self._log(f"Could not save run {rid}: {traceback.format_exc(limit=1)}")
            self._dirty.discard(rid)

    def _touch(self, run: dict):
        """Mark a run as needing to be written."""
        self._dirty.add(run["id"])

    def _log(self, msg: str):
        """Append to the scheduler's log, which the Benchmarks page shows."""
        self.log.append({"t": _now(), "msg": msg})
        del self.log[:-200]

    # ------------------------------------------------------------------ settings
    def get_settings(self) -> dict:
        """The scoring weights."""
        return copy.deepcopy(self.settings)

    def update_settings(self, patch: dict) -> dict:
        """Change the scoring weights and save them."""
        with self.lock:
            w = patch.get("score_weights") or {}
            for k in ("benchmark", "reliability", "speed"):
                if k in w:
                    self.settings["score_weights"][k] = max(0.0, float(w[k]))
            self._write(self.dir / "settings.json", self.settings)
        self.tick()
        return self.get_settings()

    def restricted_until(self, server_id: Optional[str]) -> Optional[datetime]:
        """End of the restricted window the server is in now (None when it may be used)."""
        return REG.restricted_at(server_id, self._clock())

    # ------------------------------------------------------------------ suites
    def list_suites(self) -> list[dict]:
        """Every saved suite, in summary."""
        out = []
        for p in sorted(self.suites_dir.glob("*.json"), key=lambda p: -p.stat().st_mtime):
            try:
                s = json.loads(p.read_text(encoding="utf-8"))
                out.append({"id": s["id"], "name": s["name"], "description": s.get("description", ""),
                            "updated": s.get("updated"), "mode": s.get("mode"),
                            "servers": [{"name": g["name"], "server_id": g["server_id"], "missing": g["missing"],
                                         "models": [m.get("model") or m.get("model_id") for m in g["models"] if m.get("enabled", True)]}
                                        for g in (resolve_group(x) for x in s.get("servers", []))],
                            "scenarios": [sc["name"] for sc in s.get("scenarios", []) if sc.get("enabled", True)],
                            "jobs": self._job_count(s)})
            except Exception:
                continue
        return out

    def _suite_path(self, suite_id: str) -> Path:
        """The file for a suite id, rejecting anything that is not a plain identifier.

        The id reaches this from a URL, so it is validated rather than trusted - a path separator here
        would be a way to read or write files outside the benchmark directory.
        """
        if not re.fullmatch(r"[A-Za-z0-9_\-]+", suite_id or ""):
            raise KeyError(suite_id)
        return self.suites_dir / f"{suite_id}.json"

    def get_suite(self, suite_id: str) -> dict:
        """One suite."""
        p = self._suite_path(suite_id)
        if not p.exists():
            raise KeyError(suite_id)
        return json.loads(p.read_text(encoding="utf-8"))

    def save_suite(self, data: dict) -> dict:
        """Create or replace a suite, after validating it."""
        suite = normalize_suite(data)
        self._write(self._suite_path(suite["id"]), suite)
        return suite

    def delete_suite(self, suite_id: str):
        """Delete a suite, leaving its finished runs alone."""
        p = self._suite_path(suite_id)
        if p.exists():
            p.unlink()

    @staticmethod
    def _job_count(suite: dict) -> int:
        """How many games a suite will produce: models times scenarios times repeats."""
        models = sum(1 for sv in suite.get("servers", []) for m in sv.get("models", []) if m.get("enabled", True))
        scen = sum(1 for sc in suite.get("scenarios", []) if sc.get("enabled", True))
        return models * scen * max(1, int(suite.get("repeats") or 1))

    # ------------------------------------------------------------------ runs
    def create_run(self, suite: dict, name: Optional[str] = None) -> dict:
        """Expand a suite into a run with one job per game."""
        suite = normalize_suite(suite)
        suite["servers"] = [resolve_group(g) for g in suite["servers"]]
        missing = [f"{g['name']}: {m.get('model') or m.get('model_id')}" for g in suite["servers"]
                   for m in g["models"] if m.get("enabled", True) and (g["missing"] or m.get("missing"))]
        if missing:
            raise ValueError("These models aren't in the server registry any more: " + "; ".join(missing))
        jobs = []
        for sc in [x for x in suite["scenarios"] if x.get("enabled", True)]:
            for rep in range(suite["repeats"]):
                for sv in suite["servers"]:
                    for m in [x for x in sv["models"] if x.get("enabled", True)]:
                        seed = sc.get("seed")
                        jobs.append({
                            "id": _new_id("job_"), "server_id": sv["id"], "server_name": sv["name"],
                            "server_key": sv["id"], "model_id": m["id"], "model": m["model"],
                            "profile": m.get("profile_name"),
                            "label": m.get("label") or m["model"], "scenario_id": sc["id"], "scenario_name": sc["name"],
                            "repeat": rep + 1, "seed": (int(seed) + rep) if seed not in (None, "") else None,
                            "status": "queued", "pause_reason": None, "game_id": None, "created": _now(),
                            "started": None, "finished": None, "error": None, "result": None, "progress": None,
                        })
        if not jobs:
            raise ValueError("This suite has no enabled models and scenarios to run.")
        run = {"id": _new_id("run_"), "name": name or suite["name"], "suite_id": suite["id"], "suite": suite,
               "status": "running", "created": _now(), "started": _now(), "finished": None, "jobs": jobs}
        with self.lock:
            self.runs[run["id"]] = run
            self._touch(run)
            self._log(f"Started run '{run['name']}' with {len(jobs)} jobs.")
            self._flush()
        self.tick()
        return run

    def _machine(self, job: dict) -> str:
        """The machine a job's model actually plays on, for restricted hours.

        Normally the suite's server. But a game's AI seat can be moved to another machine (a machine removed
        from the Servers page and registered again gets a new id), and what must pause is the machine the
        game is really using - so a job with a game takes it from the game's seat, and remembers it for
        while the game is paused and unloaded.
        """
        s = self.manager.get(job["game_id"]) if job.get("game_id") else None
        if s is not None:
            pid = (s.benchmark or {}).get("llm_player", 0)
            seat = s.seats[pid] if pid < len(s.seats) else None
            sid = ((seat.llm if seat else None) or {}).get("server_id")
            if sid:
                job["machine_id"] = sid
        return job.get("machine_id") or job["server_id"]

    def _server(self, run: dict, job: dict) -> dict:
        """The server configuration a job belongs to."""
        return next(sv for sv in run["suite"]["servers"] if sv["id"] == job["server_id"])

    def _scenario(self, run: dict, job: dict) -> dict:
        """The scenario a job plays."""
        return next(sc for sc in run["suite"]["scenarios"] if sc["id"] == job["scenario_id"])

    def _model_cfg(self, run: dict, job: dict) -> dict:
        """The model configuration a job uses."""
        return next(m for m in self._server(run, job)["models"] if m["id"] == job["model_id"])

    def _job(self, run_id: str, job_id: str) -> tuple[dict, dict]:
        """A run and one of its jobs, by id."""
        run = self.runs.get(run_id)
        if run is None:
            raise KeyError(run_id)
        job = next((j for j in run["jobs"] if j["id"] == job_id), None)
        if job is None:
            raise KeyError(job_id)
        return run, job

    # ------------------------------------------------------------------ controls
    def control_run(self, run_id: str, action: str) -> dict:
        """Pause, resume or cancel a whole run."""
        with self.lock:
            run = self.runs.get(run_id)
            if run is None:
                raise KeyError(run_id)
            if action == "pause":
                if run["status"] in ("running", "queued"):
                    run["status"] = "paused"
                    for j in run["jobs"]:
                        self._pause_job(j, "user")
            elif action == "resume":
                if run["status"] == "paused":
                    run["status"] = "running"
                    for j in run["jobs"]:
                        if j["status"] == "paused" and j["pause_reason"] == "user":
                            self._request_resume(run, j)
            elif action == "cancel":
                for j in run["jobs"]:
                    if j["status"] in ACTIVE or j["status"] == "queued":
                        self._cancel_job(run, j, "Run cancelled.")
                run["status"] = "cancelled"
                run["finished"] = _now()
            elif action == "remove":
                if run["status"] in ("running", "paused"):
                    raise ValueError("Cancel the run before removing it.")
                self.runs.pop(run_id, None)
                p = self.runs_dir / f"{run_id}.json"
                if p.exists():
                    p.unlink()
                return {"removed": run_id}
            else:
                raise ValueError(f"Unknown action '{action}'.")
            self._touch(run)
            self._flush()
        self.tick()
        return run

    def control_job(self, run_id: str, job_id: str, action: str) -> dict:
        """Pause, resume, skip or retry one job."""
        with self.lock:
            run, job = self._job(run_id, job_id)
            if action == "pause" and job["status"] in ("running", "resuming"):
                self._pause_job(job, "user")
            elif action == "resume" and job["status"] == "paused":
                self._request_resume(run, job)
            elif action == "skip" and (job["status"] in ACTIVE or job["status"] == "queued"):
                self._cancel_job(run, job, "Skipped.")
            elif action == "retry" and job["status"] in ("failed", "cancelled"):
                job.update({"status": "queued", "pause_reason": None, "game_id": None, "started": None, "finished": None,
                            "error": None, "result": None, "progress": None})
                if run["status"] in ("done", "cancelled"):
                    run["status"], run["finished"] = "running", None
            else:
                raise ValueError(f"Can't {action} a job that is {job['status']}.")
            self._touch(run)
            self._flush()
        self.tick()
        return job

    def _pause_job(self, job: dict, reason: str, first: Optional[dict] = None):
        """Pause a job and record why, so the page can say whether it was a person or a window."""
        if job["status"] in ("running", "resuming"):
            s = self.manager.get(job["game_id"]) if job["game_id"] else None
            if s is not None:
                s.suspend({"kind": "queue", "message": f"making way for {first['label']} (priority {first['priority']})"}
                          if reason == "preempted" and first else None)
            job["status"], job["pause_reason"] = "paused", reason
        elif job["status"] == "paused" and reason == "user":
            job["pause_reason"] = "user"

    def _request_resume(self, run: dict, job: dict):
        """Mark a paused job to be resumed on the next tick."""
        job["status"], job["pause_reason"] = "resuming", None

    def _cancel_job(self, run: dict, job: dict, why: str):
        """Cancel a job and release its game."""
        s = self.manager.get(job["game_id"]) if job["game_id"] else None
        if s is not None:
            self._record_progress(run, job, s)
            try:
                s.save("benchmark")
            except Exception:
                pass
            self.manager.delete(s.id)
        job.update({"status": "cancelled", "finished": _now(), "error": why, "pause_reason": None})

    def open_job_game(self, run_id: str, job_id: str) -> GameSession:
        """Return the job's game session, loading it from disk if it isn't running (finished or cancelled jobs)."""
        with self.lock:
            run, job = self._job(run_id, job_id)
            if not job["game_id"]:
                raise ValueError("This job hasn't started yet.")
            s = self.manager.get(job["game_id"])
            if s is not None:
                return s
            folder = SAVE_DIR / job["game_id"]
            for name in ("benchmark.citar", "autosave.citar"):
                if (folder / name).exists():
                    s = self.manager.load(folder / name)
                    if job["status"] in ("done", "failed", "cancelled"):
                        job["closed_view_at"] = _now()
                    return s
            raise ValueError("The game's save file is missing.")

    # ------------------------------------------------------------------ loop
    def start(self):
        """Start the scheduler thread."""
        if self._thread is not None:
            return
        self._recover()
        self._thread = threading.Thread(target=self._loop, daemon=True, name="benchmark-scheduler")
        self._thread.start()

    def stop(self):
        """Stop the scheduler thread and let running games finish their current turn."""
        self._stop.set()

    def _loop(self):
        """The scheduler thread: tick, sleep, repeat."""
        while not self._stop.is_set():
            try:
                self.tick()
            except Exception:
                self._log("Scheduler error: " + traceback.format_exc(limit=3))
            self._stop.wait(2.0)

    def _recover(self):
        """After a server restart: reload the games of jobs that were in progress from their autosaves."""
        with self.lock:
            for run in self.runs.values():
                for job in run["jobs"]:
                    if job["status"] == "loading":
                        job["status"] = "queued"
                    if job["status"] not in ("running", "resuming", "paused"):
                        continue
                    path = SAVE_DIR / (job["game_id"] or "-") / "autosave.citar"
                    if not job["game_id"] or not path.exists():
                        job.update({"status": "failed", "error": "Server restarted and the game's autosave is missing.",
                                    "finished": _now()})
                    elif self.manager.get(job["game_id"]) is None:
                        try:
                            self.manager.load(path)   # loads paused
                        except Exception as e:
                            job.update({"status": "failed", "error": f"Could not reload the game: {e}", "finished": _now()})
                            continue
                        if job["status"] != "paused" or job["pause_reason"] != "user":
                            job["status"], job["pause_reason"] = "resuming", None
                        self._log(f"Reloaded {job['label']} / {job['scenario_name']} after restart.")
                    self._touch(run)
            self._flush()

    def tick(self):
        """One pass: check running jobs, start what can start, write what changed."""
        with self.lock:
            self._check_restrictions()
            for run in sorted(self.runs.values(), key=lambda r: r["created"]):
                if run["status"] in ("running", "paused"):
                    self._check_jobs(run)
                else:
                    for job in run["jobs"]:
                        self._close_finished_view(job)
            self._start_jobs()
            self._flush()

    # restricted hours, per server --------------------------------------------------------------
    def _check_restrictions(self):
        """Enter/leave each server's restricted hours: games on it finish the model's turn in progress (up to the
        server's grace minutes), then pause; when the window ends they resume."""
        server_ids = {self._machine(j) for r in self.runs.values() for j in r["jobs"]}
        for sid in server_ids:
            end = self.restricted_until(sid)
            was = sid in self._restricted
            if end and not was:
                self._restricted[sid] = {"since": _now(), "unloaded": False}
                self._log(f"Restricted hours started on {REG.server_name(sid)} (until {REG.restricted_text(sid, end)}); "
                          f"its games pause after the current model turn.")
            elif not end and was:
                self._restricted.pop(sid, None)
                self._log(f"Restricted hours are over on {REG.server_name(sid)}; resuming its games.")
                for run in self.runs.values():
                    for job in run["jobs"]:
                        if self._machine(job) == sid and job["status"] == "paused" and job["pause_reason"] in ("restricted", "quiet"):
                            self._request_resume(run, job)
                            self._touch(run)
        for sid, st in list(self._restricted.items()):
            sv = REG.find(sid)     # None for a Servers-page machine: its helper keeps its models loaded
            grace = float(REG.restriction_config(sid).get("grace_minutes", 15)) * 60
            pending = False
            for run in self.runs.values():
                for job in run["jobs"]:
                    if self._machine(job) != sid or job["status"] not in ("running", "resuming", "loading"):
                        continue
                    if job["status"] == "resuming":
                        job["status"], job["pause_reason"] = "paused", "restricted"
                        self._touch(run)
                        continue
                    if job["status"] == "loading":
                        pending = True
                        continue
                    s = self.manager.get(job["game_id"]) if job["game_id"] else None
                    mid_turn = bool(s and s.game.current == (s.benchmark or {}).get("llm_player", 0)
                                    and s.agent_status.get((s.benchmark or {}).get("llm_player", 0)) == "thinking")
                    if mid_turn and _now() - st["since"] < grace:
                        pending = True
                        job["pause_pending"] = "restricted"
                        self._touch(run)
                        continue
                    job.pop("pause_pending", None)
                    self._pause_job(job, "restricted")
                    self._touch(run)
            if not pending and not st["unloaded"] and sv and sv["restricted_hours"].get("unload_models"):
                st["unloaded"] = True
                threading.Thread(target=self._safe, args=(REG.unload_models, sv), daemon=True).start()

    def _safe(self, fn, *args):
        """Run part of a tick, logging a failure rather than killing the thread.

        A scheduler that dies on one bad job takes a multi-day run with it, so no single failure is allowed
        to end the loop.
        """
        try:
            fn(*args)
        except Exception as e:
            self._log(f"{getattr(fn, '__name__', 'task')} failed: {e}")

    def _check_jobs(self, run: dict):
        """Update every running job: progress, completion, and restricted hours."""
        for job in run["jobs"]:
            if job["status"] not in ("running", "resuming", "paused"):
                if job["status"] in ("done", "failed", "cancelled"):
                    self._close_finished_view(job)
                continue
            s = self.manager.get(job["game_id"]) if job["game_id"] else None
            if s is None:
                job.update({"status": "cancelled", "error": "The game was closed.", "finished": _now()})
                self._touch(run)
                continue
            if s.game.phase != "playing" or not s.game.is_alive((s.benchmark or {}).get("llm_player", 0)):
                # once the model is eliminated the result is settled (performance 0), and the bots playing
                # on would only hold the machine and the CPU for nothing
                self._finish_job(run, job, s)
                continue
            if job["status"] == "resuming" and self._machine(job) not in self._restricted and run["status"] == "running":
                job["status"] = "loading"
                threading.Thread(target=self._in_job_thread,
                                 args=(self._resume_job, run["id"], job["id"]), daemon=True).start()
            elif job["status"] == "running" and s.paused and not s.stopped:
                # paused from the game screen, or by the game itself when a model server went away
                job["status"], job["pause_reason"] = "paused", (s.pause_reason or {}).get("kind") or "user"
                self._touch(run)
            elif job["status"] == "paused" and job["pause_reason"] in ("user", "disconnect") and not s.paused:
                job["status"], job["pause_reason"] = "running", None     # resumed from the game screen
                self._touch(run)
            self._queue_turn(run, job, s)
            if _now() - self._progress_at.get(job["id"], 0) > 5:
                self._record_progress(run, job, s)
        states = [j["status"] for j in run["jobs"]]
        if run["status"] == "running" and all(st in ("done", "failed", "cancelled") for st in states):
            run["status"], run["finished"] = "done", _now()
            self._log(f"Run '{run['name']}' finished.")
            self._touch(run)

    PREEMPT_GRACE = 900        # seconds a model's turn in progress may run on before higher-priority work takes over

    def _queue_turn(self, run: dict, job: dict, s: GameSession):
        """Make way for higher-priority work on this job's machine, or take the machine back once it has gone.

        A running job pauses after the model's turn in progress (or after PREEMPT_GRACE) when something waiting
        for its machine has a higher priority than its run. A job paused that way resumes when nothing waiting
        ranks ahead of it and the machine is free again.
        """
        from ..pool import queue as work_queue, seats as pool_seats
        prio = work_queue.priority("benchmark", run["id"])
        machine = self._machine(job)
        if job["status"] == "running" and not s.paused:
            first = work_queue.preempting(machine, "benchmark", job["id"], prio)
            if first is None:
                if job.get("pause_pending") == "preempted":
                    job.pop("pause_pending", None)
                    job.pop("preempt_since", None)
                    self._touch(run)
                return
            pid = (s.benchmark or {}).get("llm_player", 0)
            mid_turn = s.game.current == pid and s.agent_status.get(pid) == "thinking"
            since = job.setdefault("preempt_since", _now())
            if mid_turn and _now() - since < self.PREEMPT_GRACE:
                if job.get("pause_pending") != "preempted":
                    job["pause_pending"] = "preempted"
                    self._touch(run)
                return
            job.pop("pause_pending", None)
            job.pop("preempt_since", None)
            self._pause_job(job, "preempted", first)
            self._log(f"{job['label']} · {job['scenario_name']} #{job['repeat']} paused on {job['server_name']} "
                      f"for {first['label']} (priority {first['priority']}).")
            self._touch(run)
        elif self._preempted(job) and run["status"] == "running" and machine not in self._restricted:
            ours = frozenset(j["game_id"] for r in self.runs.values() for j in r["jobs"] if j.get("game_id"))
            limit = self._server(run, job).get("max_parallel", 1)
            in_use = sum(1 for r in self.runs.values() for j in r["jobs"]
                         if j["status"] in ACTIVE and not self._preempted(j) and self._machine(j) == machine)
            if in_use + len(pool_seats.occupied(machine, exclude=ours)) >= limit:
                return
            if work_queue.first_ahead(machine, (-prio, job["created"]), exclude=("benchmark", job["id"])) is None:
                self._request_resume(run, job)
                self._touch(run)

    def _close_finished_view(self, job: dict):
        """Release a finished job's game once nobody is watching it."""
        s = self.manager.get(job["game_id"]) if job.get("game_id") else None
        if s is None:
            return
        since = job.get("closed_view_at") or job.get("finished") or 0
        if _now() - since > FINISHED_SESSION_GRACE and not s.subscribers:
            self.manager.delete(s.id)

    def _start_jobs(self):
        # paused jobs keep their slot: their model stays assigned to the server and resumes where it left off
        """Start as many queued jobs as the servers' limits allow."""
        from ..pool import seats as pool_seats
        busy: dict[str, int] = {}
        ours = set()
        for run in self.runs.values():
            for job in run["jobs"]:
                if job["status"] in ACTIVE and not self._preempted(job):
                    busy[job["server_key"]] = busy.get(job["server_key"], 0) + 1
                if job.get("game_id"):
                    ours.add(job["game_id"])
        # Games that are not ours - somebody's lobby game, a probe - hold their machine too. A queued job
        # waits for them rather than fighting a running game for the machine's slot.
        others: dict[str, list] = {}
        for sid in {j["server_key"] for r in self.runs.values() for j in r["jobs"] if j["status"] == "queued"}:
            others[sid] = pool_seats.occupied(sid, exclude=frozenset(ours))
        from ..pool import queue as work_queue
        for run in sorted(self.runs.values(), key=lambda r: (-work_queue.priority("benchmark", r["id"]), r["created"])):
            if run["status"] != "running":
                continue
            prio = work_queue.priority("benchmark", run["id"])
            run_busy = sum(1 for j in run["jobs"] if j["status"] in ACTIVE)
            for job in run["jobs"]:
                if job["status"] != "queued":
                    continue
                if run["suite"]["mode"] == "sequential" and run_busy >= 1:
                    break
                if job["server_id"] in self._restricted:
                    continue
                limit = self._server(run, job).get("max_parallel", 1)
                held = others.get(job["server_key"]) or []
                if busy.get(job["server_key"], 0) + len(held) >= limit:
                    note = f"waiting: {job['server_name']} is in use by {', '.join(held)}" if held else None
                    if job.get("waiting") != note:
                        job["waiting"] = note
                        self._touch(run)
                    continue
                first = work_queue.ahead_of(job["server_key"], "benchmark", (-prio, job["created"]))
                if first is not None:
                    note = f"waiting: {first['label']} goes first on {job['server_name']} (priority {first['priority']})"
                    if job.get("waiting") != note:
                        job["waiting"] = note
                        self._touch(run)
                    continue
                job["waiting"] = None
                busy[job["server_key"]] = busy.get(job["server_key"], 0) + 1
                run_busy += 1
                job["status"] = "loading"
                job["started"] = _now()
                self._touch(run)
                threading.Thread(target=self._in_job_thread,
                                 args=(self._launch_job, run["id"], job["id"]), daemon=True).start()

    # ------------------------------------------------------------------ job threads
    def _in_job_thread(self, what, run_id: str, job_id: str):
        """Run a job thread so that a failure is reported instead of disappearing.

        `_launch_job` and `_resume_job` run on daemon threads. An exception on one of those has
        nowhere to go: Python prints it to stderr, the thread ends, and the job is left in
        `loading` or `resuming` for ever. A run would then sit at "starting" indefinitely with
        nothing anywhere saying why - which is exactly what it did, on one platform, until a test
        timed out and said only that the job never started.

        So every path out of a job thread ends here, and a failure becomes a failed job with the
        traceback in the log.
        """
        try:
            what(run_id, job_id)
        except Exception as exc:                            # deliberately everything: the point is that nothing escapes
            detail = f"{type(exc).__name__}: {exc}"
            with self.lock:
                try:
                    run, job = self._job(run_id, job_id)
                except KeyError:
                    self._log(f"job thread failed after the run went away: {detail}")
                    return
                job.update({"status": "failed", "error": detail, "finished": _now()})
                self._log(f"{job.get('label', job_id)}: {detail}")
                self.log.append({"t": _now(), "msg": traceback.format_exc()})
                self._touch(run)
                self._flush()

    def _ensure_model(self, run: dict, job: dict) -> float:
        """Load the model a job needs, if CITAR manages loading on that server."""
        if self._server(run, job).get("pooled"):
            return 0.0              # somebody else's machine: its owner decides what is loaded
        sv = REG.get(job["server_id"])
        m = self._model_cfg(run, job)
        entry = REG.model_entry(sv, m.get("model_id") or m["model"])
        return REG.ensure_model(sv, m["model"], REG.profile(entry, m.get("profile_id")))

    def _llm_config(self, run: dict, job: dict) -> dict:
        """The seat's llm block: a reference into the registry plus suite overrides (resolved when the agent starts)."""
        sc, m = self._scenario(run, job), self._model_cfg(run, job)
        extra = {"max_turn_seconds": float(sc.get("max_turn_minutes") or 30) * 60}
        for k in ("persona", "reasoning_effort", "effort", "tool_mode"):
            if m.get(k):
                extra[k] = m[k]
        ref = REG.seat_ref(job["server_id"], m.get("model_id") or m["model"], m.get("profile_id"), **extra)
        resolved = REG.resolve_llm(ref, with_key=False)
        if resolved.get("tool_mode"):
            ref["tool_mode"] = resolved["tool_mode"]
        return ref

    def _launch_job(self, run_id: str, job_id: str):
        """Create the game for a job and begin playing it."""
        with self.lock:
            run, job = self._job(run_id, job_id)
            sc = self._scenario(run, job)
        try:
            load_s = self._ensure_model(run, job)
            llm = self._llm_config(run, job)
        except Exception as e:
            with self.lock:
                job.update({"status": "failed", "error": str(e), "finished": _now()})
                self._log(f"{job['label']}: {e}")
                self._touch(run)
                self._flush()
            return
        with self.lock:
            if job["status"] != "loading":      # cancelled while loading
                return
            if self._machine(job) in self._restricted or run["status"] != "running":
                job["status"], job["started"] = "queued", None
                self._touch(run)
                return
            config = {"map_size": sc["map_size"], "map_type": sc["map_type"], "speed": _scenario_speed(sc),
                      "difficulty": sc.get("difficulty") or "Prince", "barbarians": sc.get("barbarians", "normal"),
                      "barbarian_difficulty": sc.get("barbarian_difficulty") or None,
                      "turn_limit": int(sc.get("turn_limit") or 0) or None, "seed": job["seed"],
                      "victories": sc.get("victories") or {}, "city_states": sc.get("city_states"),
                      "religion": sc.get("religion", True), "espionage": sc.get("espionage", True),
                      "tech_trading": sc.get("tech_trading", True), "ruins": sc.get("ruins", True),
                      # map generation: edges, rivers and resources (see mapgen.MapOptions); absent = defaults
                      **{k: sc[k] for k in ("map_edges", "river_density", "resources") if sc.get(k) is not None}}
            n_opp = int(sc.get("opponents") or 1)
            nations = _seat_nations(sc, n_opp + 1)
            seats = [{"type": "llm", "civ_name": None, "llm": llm, "nation": nations[0]}]
            seats += [{"type": "bot", "civ_name": None, "nation": nations[i + 1],
                       "difficulty": sc.get("bot_difficulty") or None,
                       "bot": {"aggression": float(sc.get("bot_aggression", 0.4)),
                               "profile": sc.get("bot_profile") or None}} for i in range(n_opp)]
            try:
                s = self.manager.create(config, seats, name=f"Bench · {job['label']} · {sc['name']}"
                                                            + (f" #{job['repeat']}" if run["suite"]["repeats"] > 1 else ""),
                                        track=False, start=False)
            except Exception as e:
                job.update({"status": "failed", "error": f"Could not create the game: {e}", "finished": _now()})
                self._touch(run)
                self._flush()
                return
            s.ai_delay = 0
            s.benchmark = {"run_id": run["id"], "job_id": job["id"], "run_name": run["name"], "suite_id": run["suite_id"],
                           "scenario": sc["name"], "scenario_id": sc["id"], "model": job["model"], "server": job["server_name"],
                           "repeat": job["repeat"], "llm_player": 0, "turn_limit": s.game.turn_limit,
                           "server_id": job["server_id"], "profile": job.get("profile")}
            self.manager.track(s)
            s.autosave(force=True)
            s.start()       # only now: a game started before `benchmark` is set plays its first turn as a lobby game
            job.update({"status": "running", "game_id": s.id, "load_seconds": load_s, "tool_mode": llm.get("tool_mode")})
            self._log(f"Started {job['label']} on {sc['name']} (game {s.id}).")
            self._touch(run)
            self._flush()

    def _resume_job(self, run_id: str, job_id: str):
        """Reload a paused or interrupted job's game from its autosave and continue."""
        with self.lock:
            run, job = self._job(run_id, job_id)
        try:
            self._ensure_model(run, job)
        except Exception as e:
            with self.lock:
                job.update({"status": "paused", "pause_reason": "user", "error": f"Could not load the model: {e}"})
                self._touch(run)
                self._flush()
            return
        with self.lock:
            if job["status"] != "loading":
                return
            s = self.manager.get(job["game_id"])
            if s is None:
                job.update({"status": "cancelled", "error": "The game was closed.", "finished": _now()})
            elif self._machine(job) in self._restricted or run["status"] != "running":
                job["status"], job["pause_reason"] = "paused", "restricted" if self._machine(job) in self._restricted else "user"
            else:
                s.resume()
                job["status"], job["pause_reason"] = "running", None
            self._touch(run)
            self._flush()

    # ------------------------------------------------------------------ results
    def _record_progress(self, run: dict, job: dict, s: GameSession):
        """Copy a running game's progress and score into the job record."""
        with s.lock:
            job["progress"] = game_progress(s)
        self._progress_at[job["id"]] = _now()
        self._progress_dirty.add(run["id"])

    def _finish_job(self, run: dict, job: dict, s: GameSession):
        """Finish a job: final score, timing, and release the game."""
        self._record_progress(run, job, s)
        job["result"] = job["progress"]
        job.update({"status": "done", "finished": _now(), "pause_reason": None})
        if s.game.phase == "playing":
            s.stop()                # the model was eliminated: stop the bots playing out a settled game
        try:
            s.save("benchmark")
        except Exception as e:
            job["error"] = f"Could not save the finished game: {e}"
        self._log(f"Finished {job['label']} on {job['scenario_name']}: {job['result'].get('outcome')}.")
        self._touch(run)

    # ------------------------------------------------------------------ views
    def status(self) -> dict:
        """What the scheduler is doing now, for the page header and the badge."""
        with self.lock:
            active = [j for r in self.runs.values() for j in r["jobs"] if j["status"] in ("running", "loading", "resuming")]
            return {"restricted": REG.restriction_status(), "active_jobs": len(active),
                    "queued_jobs": sum(1 for r in self.runs.values() if r["status"] == "running"
                                       for j in r["jobs"] if j["status"] == "queued"),
                    "log": self.log[-30:]}

    def list_runs(self) -> list[dict]:
        """Every run with its jobs, for the Runs view."""
        with self.lock:
            out = []
            for run in sorted(self.runs.values(), key=lambda r: -r["created"]):
                r = {k: v for k, v in run.items() if k != "suite"}
                r["mode"] = run["suite"]["mode"]
                r["servers"] = [{"id": sv["id"], "name": sv["name"], "base_url": sv.get("base_url"), "provider": sv.get("provider"),
                                 "max_parallel": sv.get("max_parallel", 1), "restricted_until":
                                 (lambda e: e.strftime("%H:%M") if e else None)(self.restricted_until(sv["id"]))}
                                for sv in run["suite"]["servers"]]
                r["scenarios"] = {sc["id"]: {"name": sc["name"], "turn_limit": sc.get("turn_limit"), "map_size": sc["map_size"],
                                             "map_type": sc["map_type"], "opponents": sc.get("opponents")}
                                  for sc in run["suite"]["scenarios"]}
                r["summary"] = run_summary(run)
                out.append(r)
            return out


# ----------------------------------------------------------------------------
# progress & scoring helpers (shared with scoring.py)
# ----------------------------------------------------------------------------
def performance(scores: dict, llm: int, phase: str, winner, alive: dict) -> Optional[float]:
    """Game performance 0-100 for the model seat: 100 for a win, 0 if eliminated, otherwise its share of the score
    against the best opponent (50 = level with the strongest bot)."""
    if phase != "playing" and winner is not None:
        if winner == llm:
            return 100.0
    if alive.get(llm) is False:
        return 0.0
    mine = scores.get(llm)
    others = [v for k, v in scores.items() if k != llm]
    if mine is None or not others:
        return None
    best = max(others)
    if mine + best <= 0:
        return 50.0
    share = 100.0 * mine / (mine + best)
    return round(min(share, 95.0) if phase != "playing" else share, 1)


def game_progress(s: GameSession) -> dict:
    """Live progress of a benchmark game (called with the session lock held)."""
    g = s.game
    llm = (s.benchmark or {}).get("llm_player", 0)
    summ = g.summary()
    majors = [p for p in summ["players"] if p["kind"] == "major"]
    standings = g.standings()
    scores = {p["id"]: (standings[p["id"]]["score"] if p["alive"] else 0) for p in majors}
    alive = {p["id"]: p["alive"] for p in majors}
    mine = standings[llm]
    phase, winner, victory = summ["phase"], summ["winner"], summ["victory"]
    rep = s.metrics.summary({llm: {"name": g.player_name(llm), "controller": "llm", "model": (s.benchmark or {}).get("model")}})[llm]
    best_opp = max((scores[p["id"]] for p in majors if p["id"] != llm), default=0)
    outcome = "eliminated" if not alive.get(llm) else None
    if phase != "playing":
        outcome = "won" if winner == llm else ("eliminated" if not alive.get(llm) else f"lost ({victory or 'game over'})")
    series = [{"turn": e["turn"], "llm": (e["players"].get(str(llm)) or {}).get("score", 0),
               "best_bot": max([(v or {}).get("score", 0) for k, v in e["players"].items() if k != str(llm)] or [0])}
              for e in g.stats(last=400)]
    status = s.agent_status.get(llm)
    last_thought = next((t for t in reversed(g.thoughts(llm)) if t.get("kind") in (None, "reasoning", "system")), None)
    return {
        "turn": summ["turn"], "turn_limit": summ["turn_limit"], "phase": phase, "current_player": summ["current"],
        "llm_to_move": summ["current"] == llm and phase == "playing", "agent_status": status, "paused": s.paused,
        "civ": g.player_name(llm), "score": scores.get(llm), "best_bot_score": best_opp, "alive": alive.get(llm),
        "cities": mine["cities"], "techs": mine["techs"],
        "population": mine["population"], "winner": winner, "victory": victory,
        "outcome": outcome, "performance": performance(scores, llm, phase, winner, alive),
        "turns_played": rep.get("turns", 0), "avg_turn_s": rep.get("avg_turn_s"), "max_turn_s": rep.get("max_turn_s"),
        "avg_errors": rep.get("avg_errors"), "avg_repeats": rep.get("avg_repeats"), "avg_tool_calls": rep.get("avg_tool_calls"),
        "malformed_calls": rep.get("malformed_calls"), "end_reasons": rep.get("end_reasons", {}),
        "output_tokens_per_s": rep.get("output_tokens_per_s"), "series": series,
        "last_thought": (last_thought["text"][:300] if last_thought else None),
        "updated": _now(),
    }


def run_summary(run: dict) -> dict:
    """A run's counts and progress, without its full job list."""
    jobs = run["jobs"]
    counts: dict[str, int] = {}
    for j in jobs:
        counts[j["status"]] = counts.get(j["status"], 0) + 1
    turns_done = turns_total = 0
    eta = {}
    # seconds per turn for each model so far, used to estimate jobs that haven't started yet
    pace: dict[str, list] = {}
    for j in jobs:
        p = j.get("result") or j.get("progress") or {}
        if p.get("avg_turn_s"):
            pace.setdefault(j["model"], []).append(p["avg_turn_s"])
    unknown = 0
    for j in jobs:
        sc = next((x for x in run["suite"]["scenarios"] if x["id"] == j["scenario_id"]), {})
        limit = int(sc.get("turn_limit") or 0)
        p = j.get("result") or j.get("progress") or {}
        played = min(limit, max(0, (p.get("turn") or 1) - 1)) if limit else max(0, (p.get("turn") or 1) - 1)
        if j["status"] in ("done",):
            played = limit or played
        turns_total += limit
        turns_done += played if limit else 0
        if j["status"] not in ("done", "failed", "cancelled") and limit:
            per_turn = p.get("avg_turn_s") or (sum(pace[j["model"]]) / len(pace[j["model"]]) if pace.get(j["model"]) else None)
            if per_turn:
                eta[j["server_key"]] = eta.get(j["server_key"], 0) + per_turn * max(0, limit - played)
            else:
                unknown += 1
    return {"counts": counts, "jobs": len(jobs), "turns_done": turns_done, "turns_total": turns_total,
            "eta_seconds": round(max(eta.values())) if eta and run["suite"]["mode"] == "parallel" else (round(sum(eta.values())) if eta else None),
            "eta_unknown_jobs": unknown}
