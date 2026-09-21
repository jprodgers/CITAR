"""Usage ledger: what every activity (game, benchmark game, probe run, lab game, report) used on which server, over
time. Costs are *not* stored here: reports price the ledger with the server configs in force at the time, so fixing a
wrong electricity rate later fixes every report.

Files (saves/usage/, JSON lines, one writer per file so processes never interleave):
    server-YYYY-MM.jsonl        written by the CITAR server (games, benchmarks, probes, reports)
    lab-YYYY-MM.jsonl           written by the lab runner (bot-vs-bot games)
    power-<server>-YYYY-MM.jsonl  one-minute power samples of a machine (GPU watts from nvidia-smi, CPU utilisation)

Row kinds:
    {"k": "act", "id", "kind", "name", "parent": {kind, id, name}, "ref": {...}, "t", ...}   an activity; later rows
                                                                                          for the same id update it
    {"k": "span", "act", "srv", "model", "t0", "t1", "held", "busy", "cpu", "in", "out", "rsn", "cr", "cw", "req"}
        one flush window (about a minute): seconds the activity held the server, seconds the model was generating,
        CPU seconds on the host, tokens (input, output, reasoning, cache read, cache write) and model requests
    {"k": "pw", "srv", "t0", "t1", "n", "gpu_w", "cpu_util"}    one minute of power samples
"""
from __future__ import annotations

import json
import os
import threading
import time
import traceback
from collections import defaultdict
from datetime import datetime
from pathlib import Path
from typing import Callable, Optional
from . import paths

USAGE_DIR = paths.saves_path("usage")
SAMPLE_SECONDS = 5.0
FLUSH_SECONDS = 60.0
TOKEN_FIELDS = ("in", "out", "rsn", "cr", "cw", "req")


def _month(ts: float) -> str:
    """The month a timestamp belongs to, which is how ledger files are split."""
    return datetime.fromtimestamp(ts).strftime("%Y-%m")


def append(writer: str, rows: list[dict]):
    """Append rows to this writer's ledger file for the current month."""
    if not rows:
        return
    USAGE_DIR.mkdir(parents=True, exist_ok=True)
    path = USAGE_DIR / f"{writer}-{_month(time.time())}.jsonl"
    text = "".join(json.dumps(r, separators=(",", ":"), default=str) + "\n" for r in rows)
    for k in range(40):
        try:
            with open(path, "a", encoding="utf-8") as f:
                f.write(text)
            return
        except PermissionError:          # a sync client briefly holding the file
            time.sleep(0.1)


def read(since: Optional[float] = None, until: Optional[float] = None) -> dict:
    """All ledger rows between two timestamps: {"acts": {id: merged}, "spans": [...], "power": {srv: [...]}}."""
    acts: dict = {}
    spans: list = []
    power: dict = defaultdict(list)
    if not USAGE_DIR.exists():
        return {"acts": acts, "spans": spans, "power": power}
    lo = _month(since) if since else "0000-00"
    hi = _month(until) if until else "9999-99"
    for p in sorted(USAGE_DIR.glob("*.jsonl")):
        month = p.stem[-7:]
        if not (lo <= month <= hi):
            continue
        try:
            lines = p.read_text(encoding="utf-8").splitlines()
        except OSError:
            continue
        for line in lines:
            try:
                r = json.loads(line)
            except ValueError:
                continue
            k = r.get("k")
            if k == "act":
                cur = acts.setdefault(r["id"], {})
                for key, v in r.items():
                    if isinstance(v, dict) and isinstance(cur.get(key), dict):
                        cur[key] = {**cur[key], **v}
                    elif v is not None:
                        cur[key] = v
                cur.setdefault("first_t", r.get("t"))
            elif k == "span":
                if (since and r["t1"] < since) or (until and r["t0"] > until):
                    continue
                spans.append(r)
            elif k == "pw":
                if (since and r["t1"] < since) or (until and r["t0"] > until):
                    continue
                power[r["srv"]].append(r)
    return {"acts": acts, "spans": spans, "power": dict(power)}


# ----------------------------------------------------------------------------- the tracker (server process)
class Tracker:
    """Collects usage in the CITAR server process and writes it to the ledger about once a minute."""

    def __init__(self, writer: str = "server"):
        self.writer = writer
        self.lock = threading.RLock()
        self.holders: dict[str, tuple] = {}               # key -> (act, fn() -> {srv: {"threads": [...]}} or None)
        self.acc: dict = defaultdict(lambda: {"held": 0.0, "busy": 0.0, "cpu": 0.0, "models": defaultdict(lambda: defaultdict(float))})
        self.window_start = time.time()
        self._last_sample = time.time()
        self._thread_cpu: dict = {}                        # native thread id -> last cpu seconds
        self._rows: list = []
        self._thread: Optional[threading.Thread] = None
        self._stop = threading.Event()

    # ---- activities
    def activity(self, act_id: str, kind: str, name: str = "", parent: Optional[dict] = None, ref: Optional[dict] = None,
                 **fields):
        """Begin recording one activity: a game, a benchmark, a probe run, a lab game."""
        row = {"k": "act", "id": act_id, "kind": kind, "name": name, "t": time.time()}
        if parent:
            row["parent"] = parent
        if ref:
            row["ref"] = ref
        row.update({k: v for k, v in fields.items() if v is not None})
        with self.lock:
            self._rows.append(row)

    def update(self, act_id: str, **fields):
        """Add or change fields on an activity in progress."""
        row = {"k": "act", "id": act_id, "t": time.time()}
        row.update({k: v for k, v in fields.items() if v is not None})
        with self.lock:
            self._rows.append(row)

    def register(self, act_id: str, holder: Callable, key: Optional[str] = None):
        """Count the servers `holder()` reports as held toward `act_id` (several holders may feed one activity, e.g.
        the cases of a probe run: give each its own key)."""
        with self.lock:
            self.holders[key or act_id] = (act_id, holder)
        self.start()

    def unregister(self, key: str):
        """Stop tracking something that has ended."""
        with self.lock:
            fn = self.holders.pop(key, None)
        if fn is not None:
            self._sample()

    # ---- model calls
    def llm(self, act_id: Optional[str], server_id: Optional[str], model: Optional[str], seconds: float,
            input_tokens: int = 0, output_tokens: int = 0, reasoning_tokens: int = 0, cache_read: int = 0,
            cache_write: int = 0, requests: int = 1):
        """Record one model call: which server and model, how long, and how many tokens."""
        if not act_id:
            return
        with self.lock:
            a = self.acc[(act_id, server_id or "unassigned")]
            a["busy"] += max(0.0, seconds)
            m = a["models"][model or "?"]
            m["busy"] += max(0.0, seconds)
            m["in"] += input_tokens or 0
            m["out"] += output_tokens or 0
            m["rsn"] += reasoning_tokens or 0
            m["cr"] += cache_read or 0
            m["cw"] += cache_write or 0
            m["req"] += requests
        self.start()

    # ---- background
    def start(self):
        """Start the sampling thread."""
        if self._thread is None or not self._thread.is_alive():
            self._thread = threading.Thread(target=self._loop, daemon=True, name=f"usage-{self.writer}")
            self._thread.start()

    def stop(self):
        """Stop sampling and flush what has not been written."""
        self._stop.set()
        self.flush()

    def _loop(self):
        """The sampling thread: take a sample, write what is due, repeat."""
        while not self._stop.wait(SAMPLE_SECONDS):
            try:
                self._sample()
                if time.time() - self.window_start >= FLUSH_SECONDS:
                    self.flush()
            except Exception:
                traceback.print_exc()

    def _thread_times(self) -> dict:
        """CPU time per thread, for attributing the host's work to the right activity."""
        try:
            import psutil
            return {t.id: t.user_time + t.system_time for t in psutil.Process().threads()}
        except Exception:
            return {}

    def _sample(self):
        """Take one sample of CPU and power."""
        now = time.time()
        with self.lock:
            dt = min(now - self._last_sample, SAMPLE_SECONDS * 4)
            self._last_sample = now
            holders = dict(self.holders)
        if dt <= 0:
            return
        times = self._thread_times()
        done = []
        for key, (act, fn) in holders.items():
            try:
                held = fn()
            except Exception:
                held = {}
            if held is None:
                done.append(key)
                continue
            for srv, info in held.items():
                with self.lock:
                    a = self.acc[(act, srv)]
                    a["held"] += dt
                    for tid in (info or {}).get("threads") or []:
                        cur = times.get(tid)
                        if cur is None:
                            continue
                        prev = self._thread_cpu.get(tid)
                        self._thread_cpu[tid] = cur
                        if prev is not None and cur >= prev:
                            a["cpu"] += cur - prev
        with self.lock:
            for act in done:
                self.holders.pop(act, None)

    def flush(self):
        """Write pending ledger entries to disk."""
        now = time.time()
        with self.lock:
            rows, self._rows = self._rows, []
            t0 = self.window_start
            self.window_start = now
            acc, self.acc = self.acc, defaultdict(lambda: {"held": 0.0, "busy": 0.0, "cpu": 0.0,
                                                           "models": defaultdict(lambda: defaultdict(float))})
        for (act, srv), a in acc.items():
            base = {"k": "span", "act": act, "srv": srv, "t0": round(t0, 1), "t1": round(now, 1),
                    "held": round(a["held"], 1), "cpu": round(a["cpu"], 2)}
            if not a["models"]:
                if base["held"] or base["cpu"]:
                    rows.append({**base, "busy": 0.0})
                continue
            first = True
            for model, m in a["models"].items():
                row = {**base, "model": model, "busy": round(m["busy"], 2),
                       **{f: int(m[f]) for f in TOKEN_FIELDS if m.get(f)}}
                if not first:
                    row["held"] = 0.0          # the hold belongs to the server once, not to each model on it
                    row["cpu"] = 0.0
                first = False
                rows.append(row)
        append(self.writer, rows)


_tracker: Optional[Tracker] = None
_tlock = threading.Lock()


def tracker() -> Tracker:
    """The usage tracker, created on first use."""
    global _tracker
    with _tlock:
        if _tracker is None:
            _tracker = Tracker("server")
        return _tracker


# ----------------------------------------------------------------------------- game sessions
def session_servers(s) -> dict:
    """What a running game session holds right now: the host (its driver thread's CPU) and each LLM seat's server.
    Returns None when the session is finished (the tracker then drops it)."""
    if s.stopped:
        return None
    g = s.game
    if s.paused or g.s.phase != "playing":
        return {}
    from . import servers as S
    out: dict = {}
    reg = S.load()
    host = reg.get("host_server_id")
    if host:
        drv = getattr(s, "_driver", None)
        out[host] = {"threads": [drv.native_id] if drv is not None and drv.native_id else []}
    for seat in s.seats:
        if seat.type == "llm" and seat.llm.get("server_id") and g.player(seat.player).alive:
            out.setdefault(seat.llm["server_id"], {"threads": []})
    return out


def track_session(s, kind: str = "game", parent: Optional[dict] = None, ref: Optional[dict] = None, name: str = ""):
    """Start (or continue, for a reloaded save) the usage record of a game session."""
    act = getattr(s, "usage_act", None) or f"game:{s.id}"
    s.usage_act = act
    t = tracker()
    seats = [{"player": seat.player, "type": seat.type, "server_id": seat.llm.get("server_id") if seat.type == "llm" else None,
              "model": seat.llm.get("model") if seat.type == "llm" else None} for seat in s.seats]
    t.activity(act, kind, name or s.name, parent=parent, ref={"game_id": s.id, **(ref or {})}, seats=seats)
    t.register(act, lambda: session_servers(s))
    return act


def finish_session(s, **fields):
    """Close out a game session's ledger entry."""
    act = getattr(s, "usage_act", None)
    if not act:
        return
    g = s.game
    majors = [p for p in g.s.players if p.kind == "major"]
    t = tracker()
    t.update(act, turn=g.turn, phase=g.s.phase, winner=g.s.winner, victory=g.s.victory,
             ended=time.time() if g.s.phase != "playing" else None,
             alive={str(p.id): p.alive for p in majors}, **fields)


# ----------------------------------------------------------------------------- power sampling
class PowerSampler:
    """Samples this machine's power draw every few seconds and writes one row a minute: GPU watts (nvidia-smi, all
    GPUs) and CPU utilisation. Only one process on the machine samples at a time (a heartbeat lock file), so the
    CITAR server and the lab runner can both start one."""

    def __init__(self, server_id: str, writer_lock: Optional[Path] = None):
        self.server_id = server_id
        self.lock_path = writer_lock or USAGE_DIR / f"power-{server_id}.lock"
        self._thread: Optional[threading.Thread] = None
        self._stop = threading.Event()
        self.nvsmi = None
        import shutil
        self.nvsmi = shutil.which("nvidia-smi")

    def start(self):
        """Start sampling."""
        if self._thread is None:
            self._thread = threading.Thread(target=self._loop, daemon=True, name="power-sampler")
            self._thread.start()

    def stop(self):
        """Stop sampling."""
        self._stop.set()

    def _own_lock(self) -> bool:
        """Whether this process is the one sampling, so two CITARs do not both measure."""
        me = f"{os.getpid()}"
        try:
            if self.lock_path.exists():
                txt = self.lock_path.read_text(encoding="utf-8").split()
                if txt and txt[0] != me and time.time() - float(txt[1]) < 45:
                    return False
            # refresh the heartbeat every 20 s, not every sample (the folder may sync to the cloud)
            if time.time() - getattr(self, "_beat", 0) > 20:
                USAGE_DIR.mkdir(parents=True, exist_ok=True)
                self.lock_path.write_text(f"{me} {time.time():.0f}", encoding="utf-8")
                self._beat = time.time()
            return True
        except (OSError, ValueError, IndexError):
            return False

    def _gpu_watts(self) -> Optional[float]:
        """GPU power from nvidia-smi, or None where it cannot be read."""
        if not self.nvsmi:
            return None
        import subprocess
        try:
            r = subprocess.run([self.nvsmi, "--query-gpu=power.draw", "--format=csv,noheader,nounits"], capture_output=True,
                               stdin=subprocess.DEVNULL, timeout=10)
            vals = [float(x) for x in r.stdout.decode().split() if x.replace(".", "", 1).isdigit()]
            return sum(vals) if vals else None
        except (OSError, ValueError, subprocess.SubprocessError):
            return None

    def _loop(self):
        """The sampling thread."""
        try:
            import psutil
        except ImportError:
            psutil = None
        gpu, cpu, n = [], [], 0
        start = time.time()
        if psutil:
            psutil.cpu_percent(None)
        while not self._stop.wait(SAMPLE_SECONDS):
            try:
                from . import servers as S
                sv = S.find(self.server_id)
                if sv is None or sv["power"].get("sampling") == "off" or not self._own_lock():
                    gpu, cpu, n, start = [], [], 0, time.time()
                    continue
                w = self._gpu_watts()
                if w is not None:
                    gpu.append(w)
                if psutil:
                    cpu.append(psutil.cpu_percent(None) / 100.0)
                n += 1
                if time.time() - start >= 60:
                    row = {"k": "pw", "srv": self.server_id, "t0": round(start, 1), "t1": round(time.time(), 1), "n": n,
                           "cpu_util": round(sum(cpu) / len(cpu), 4) if cpu else None,
                           "gpu_w": round(sum(gpu) / len(gpu), 2) if gpu else None}
                    append(f"power-{self.server_id}", [row])
                    gpu, cpu, n, start = [], [], 0, time.time()
            except Exception:
                traceback.print_exc()


_sampler: Optional[PowerSampler] = None


def start_power_sampler():
    """Sample the host machine's power if the registry has a host with sampling on."""
    global _sampler
    try:
        from . import servers as S
        h = S.host()
    except Exception:
        return None
    if h is None or _sampler is not None:
        return _sampler
    _sampler = PowerSampler(h["id"])
    _sampler.start()
    return _sampler


# ----------------------------------------------------------------------------- lab (separate process)
def lab_game_rows(exp: str, i: int, res: dict, host_id: Optional[str], labels: Optional[list] = None) -> list[dict]:
    """Ledger rows for one finished lab game (called by the lab runner)."""
    fin = res.get("finished")
    try:
        t1 = datetime.fromisoformat(fin).timestamp() if fin else time.time()
    except ValueError:
        t1 = time.time()
    secs = float(res.get("seconds") or 0)
    t0 = float(res.get("started_ts") or (t1 - secs))
    act = f"lab:{exp}:{i}"
    rows = [{"k": "act", "id": act, "kind": "lab", "name": f"{exp} #{i}", "t": t1,
             "parent": {"kind": "lab_experiment", "id": exp, "name": exp},
             "ref": {"exp": exp, "i": i, "seed": res.get("seed"), "map": res.get("map")},
             "turns": res.get("turns"), "winner_label": res.get("winner_label"), "victory": res.get("victory"),
             "ended": t1, "crash": bool(res.get("crash"))}]
    if host_id and secs > 0:
        rows.append({"k": "span", "act": act, "srv": host_id, "t0": round(t0, 1), "t1": round(t1, 1), "held": round(secs, 1),
                     "cpu": round(float(res.get("cpu_s") or secs), 2), "busy": 0.0, "est_cpu": res.get("cpu_s") is None})
    return rows
