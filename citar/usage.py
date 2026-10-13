"""Usage ledger: what every activity (game, benchmark game, probe run, lab game, report) held on which server, over
time. Costs are *not* stored here: reports price the ledger with the server configs in force at the time, so fixing a
wrong electricity rate later fixes every report.

The work itself (a model's generation seconds and tokens, the host's CPU seconds) is in the compute ledger
(``compute_events``, ``citar/ledger.py``; design/phase3-features.md P3.4.7), which costing reads beside this one
(``read``'s ``work``): this ledger keeps the activities, the time each held a machine, and the power samples. The
tracker measures the host's CPU per unit of work (``ledger.cpu_unit`` around a drive, an engine call, a chat answer, a
report's build, a probe case) and, for a game's driver thread, its time outside those units by sampling; at each flush
it hands the compute ledger one ``cpu`` event per activity.

Files (saves/usage/, JSON lines, one writer per file so processes never interleave):
    server-YYYY-MM.jsonl        written by the CITAR server (games, benchmarks, probes, reports)
    lab-YYYY-MM.jsonl           written by the lab runner (bot-vs-bot games)
    power-<server>-YYYY-MM.jsonl  one-minute power samples of a machine (GPU watts from nvidia-smi, CPU utilisation)

Row kinds:
    {"k": "act", "id", "kind", "name", "parent": {kind, id, name}, "ref": {...}, "t", ...}   an activity; later rows
                                                                                          for the same id update it
    {"k": "span", "act", "srv", "t0", "t1", "held"}
        one flush window (about a minute): seconds the activity held the server. Spans written before package 3-10
        also carry "model", "busy" (generation seconds), "cpu" (host CPU seconds) and tokens ("in", "out", "rsn",
        "cr", "cw", "req"), which costing still reads; the compute ledger holds them now.
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


def read(since: Optional[float] = None, until: Optional[float] = None, *, work: bool = False) -> dict:
    """All ledger rows between two timestamps: {"acts": {id: merged}, "spans": [...], "power": {srv: [...]},
    "work": [...]}. With ``work`` (what costing asks for: ``costing.compute``, a report's ``gather``), ``"work"`` is the
    compute ledger's events in the range, in a span's shape (``work_rows``): the generation seconds, CPU seconds and
    tokens costing prices; empty without a database, and without ``work`` (the activities alone are wanted, and every
    model step is an event)."""
    acts: dict = {}
    spans: list = []
    power: dict = defaultdict(list)
    done = work_rows(since, until) if work else []
    if not USAGE_DIR.exists():
        return {"acts": acts, "spans": spans, "power": power, "work": done}
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
    return {"acts": acts, "spans": spans, "power": dict(power), "work": done}


#: The window a CPU event's seconds are spread over by costing when it names none (its ``observed_s``: the tracker's
#: flush window, about a minute; a lab game's wall time), and the longest window a CPU event is looked for over.
CPU_WINDOW_SECONDS = 60.0
MAX_CPU_WINDOW_SECONDS = 7 * 86400.0


def work_rows(since: Optional[float] = None, until: Optional[float] = None) -> list:
    """The compute ledger's events between two timestamps as costing reads work (P3.4.7): one row per event,
    ``{"act", "srv", "model", "t0", "t1", "busy", "cpu", "in", "out", "rsn", "cr", "cw", "req"}``, a model step's
    generation seconds ending when it did, a CPU event's seconds over the window it was measured in (its
    ``observed_s``: the tracker's flush window, a lab game's whole run; a minute when it names none). Empty in a
    process without the server's database."""
    from . import db
    if not db.configured():
        return []
    try:
        from datetime import timezone
        from sqlalchemy import and_, or_, select
        from .db.models import ComputeEvent
        q = select(ComputeEvent.activity, ComputeEvent.machine_id, ComputeEvent.model, ComputeEvent.at,
                   ComputeEvent.kind, ComputeEvent.gen_s, ComputeEvent.cpu_core_s, ComputeEvent.tokens_in,
                   ComputeEvent.tokens_out, ComputeEvent.tokens_reasoning, ComputeEvent.tokens_cache_read,
                   ComputeEvent.tokens_cache_write, ComputeEvent.observed_s)
        if since is not None:
            q = q.where(ComputeEvent.at >= datetime.fromtimestamp(since, timezone.utc))
        if until is not None:
            # an event ends at ``at``; one ending after ``until`` still counts when its window began before it (a lab
            # game's CPU over its whole run)
            q = q.where(or_(ComputeEvent.at <= datetime.fromtimestamp(until + CPU_WINDOW_SECONDS, timezone.utc),
                            and_(ComputeEvent.kind == "cpu", ComputeEvent.observed_s > CPU_WINDOW_SECONDS,
                                 ComputeEvent.at <= datetime.fromtimestamp(until + MAX_CPU_WINDOW_SECONDS,
                                                                           timezone.utc))))
        out = []
        with db.session() as s:
            for act, srv, model, at, kind, gen, cpu, tin, tout, rsn, cr, cw, observed in s.execute(q):
                t1 = at.timestamp()
                if kind == "cpu":
                    window = float(observed) if observed and observed > 0 else CPU_WINDOW_SECONDS
                    t0 = t1 - min(window, MAX_CPU_WINDOW_SECONDS)
                    if until is not None and t0 > until:
                        continue
                    out.append({"act": act, "srv": srv, "t0": t0, "t1": t1, "busy": 0.0, "cpu": float(cpu or 0.0)})
                else:
                    out.append({"act": act, "srv": srv, "model": model or "?", "t0": t1 - max(float(gen or 0.0), 1e-3),
                                "t1": t1, "busy": float(gen or 0.0), "cpu": 0.0, "in": int(tin or 0),
                                "out": int(tout or 0), "rsn": int(rsn or 0), "cr": int(cr or 0), "cw": int(cw or 0),
                                "req": 1})
        return out
    except Exception:
        import logging
        logging.getLogger("citar.usage").warning("could not read the compute ledger's events", exc_info=True)
        return []


# ----------------------------------------------------------------------------- the tracker (server process)
class Tracker:
    """Collects usage in the CITAR server process and writes it to the ledger about once a minute: the time each
    activity held a machine (sampled), and the host's CPU each spent (per unit of work, ``ledger.cpu_unit``, and a
    game driver's time outside its units, sampled), which goes to the compute ledger."""

    def __init__(self, writer: str = "server"):
        self.writer = writer
        self.lock = threading.RLock()
        self.holders: dict[str, tuple] = {}               # key -> (act, fn() -> {srv: {"threads": [...]}} or None)
        self.acc: dict = defaultdict(lambda: {"held": 0.0})           # (act, srv) -> seconds held this window
        self.cpu_acc: dict = defaultdict(float)                         # act -> CPU seconds this window
        self.window_start = time.time()
        self._last_sample = time.time()
        #: Per sampled thread: its CPU total and its units' total when it was last charged.
        self._thread_cpu: dict = {}
        #: Per thread: the CPU seconds of its units of work that have ended, and whether one runs on it now.
        self._unit_cpu: dict = defaultdict(float)
        self._open_units: dict = {}
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

    # ---- the host's CPU, per unit of work (``ledger.cpu_unit``)
    def unit_started(self, tid: int, t0: float) -> None:
        """A unit of work began on thread ``tid``: until it ends, sampling leaves that thread's time to it."""
        with self.lock:
            self._open_units[tid] = t0

    def unit_ended(self, tid: int, act: str, seconds: float) -> None:
        """A unit of work on thread ``tid`` used ``seconds`` of its CPU, for ``act``."""
        with self.lock:
            self._open_units.pop(tid, None)
            self._unit_cpu[tid] += seconds
            self.cpu_acc[act] += seconds
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
                    self.acc[(act, srv)]["held"] += dt
                    for tid in (info or {}).get("threads") or []:
                        cur = times.get(tid)
                        if cur is not None:
                            self._charge_thread(act, tid, cur)
        with self.lock:
            for act in done:
                self.holders.pop(act, None)

    def _charge_thread(self, act: str, tid: int, cur: float) -> None:
        """Charge a game driver's thread's CPU outside its units of work (save writes and the like) to ``act``: its
        CPU since it was last charged, less what its units measured meanwhile. While a unit runs on it the thread is
        left for later, so a unit straddling a sample is never counted twice. Lock held."""
        st = self._thread_cpu.get(tid)
        units = self._unit_cpu[tid]
        if st is None:
            self._thread_cpu[tid] = (cur, units)
            return
        if tid in self._open_units:
            return
        other = (cur - st[0]) - (units - st[1])
        self._thread_cpu[tid] = (cur, units)
        if other > 0:
            self.cpu_acc[act] += other

    def flush(self):
        """Write pending ledger entries to disk (the activities and the time each held a machine), and hand the
        compute ledger each activity's CPU seconds of the window as one ``cpu`` event on the host."""
        now = time.time()
        with self.lock:
            rows, self._rows = self._rows, []
            t0 = self.window_start
            self.window_start = now
            acc, self.acc = self.acc, defaultdict(lambda: {"held": 0.0})
            cpu, self.cpu_acc = self.cpu_acc, defaultdict(float)
            # only a sampled thread needs its units' total (against its own baseline): a responder's thread, which
            # nothing samples, would otherwise stay here for good
            for tid in [tid for tid in self._unit_cpu if tid not in self._thread_cpu and tid not in self._open_units]:
                del self._unit_cpu[tid]
        for (act, srv), a in acc.items():
            if round(a["held"], 1):
                rows.append({"k": "span", "act": act, "srv": srv, "t0": round(t0, 1), "t1": round(now, 1),
                             "held": round(a["held"], 1)})
        append(self.writer, rows)
        if cpu:
            from . import ledger
            led = ledger.get()
            for act, seconds in cpu.items():
                led.cpu(act, seconds, at=now, window=now - t0)


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
    """What a running game session holds right now: the host (its driver thread's CPU) and each machine one of its
    leases is live on (design/phase3-features.md P3.3.5: a seat changed away, eliminated or made way holds nothing).
    Nothing while it is paused or over; None when the session is finished (the tracker then drops it)."""
    if s.stopped:
        return None
    g = s.game
    if s.paused or g.phase != "playing":
        return {}
    from . import servers as S
    out: dict = {}
    reg = S.load()
    host = reg.get("host_server_id")
    if host:
        drv = getattr(s, "_driver", None)
        out[host] = {"threads": [drv.native_id] if drv is not None and drv.native_id else []}
    for server_id in s.leased_machines():
        out.setdefault(server_id, {"threads": []})
    return out


def track_session(s, kind: str = "game", parent: Optional[dict] = None, ref: Optional[dict] = None, name: str = ""):
    """Start (or continue, for a reloaded save) the usage record of a game session."""
    act = getattr(s, "usage_act", None) or f"game:{s.id}"
    s.usage_act = act
    t = tracker()
    # every seat a model plays, with its task class (P3.6.7): its machine is held only while a lease of the game's is
    # live on it (``session_servers``), and a hybrid seat's prompts are tickets, which hold none
    seats = [{"player": seat.player, "type": seat.type,
              "server_id": seat.llm.get("server_id") if seat.model_seat else None,
              "model": seat.llm.get("model") if seat.model_seat else None,
              "class": seat.task_class} for seat in s.seats]
    t.activity(act, kind, name or s.name, parent=parent, ref={"game_id": s.id, **(ref or {})}, seats=seats)
    t.register(act, lambda: session_servers(s))
    return act


def finish_session(s, **fields):
    """Close out a game session's ledger entry."""
    act = getattr(s, "usage_act", None)
    if not act:
        return
    summ = s.game.summary()
    majors = [p for p in summ["players"] if p["kind"] == "major"]
    t = tracker()
    t.update(act, turn=summ["turn"], phase=summ["phase"], winner=summ["winner"], victory=summ["victory"],
             ended=time.time() if summ["phase"] != "playing" else None,
             alive={str(p["id"]): p["alive"] for p in majors}, **fields)


# ----------------------------------------------------------------------------- power sampling
class PowerSampler:
    """Samples this machine's power draw every few seconds and writes one row a minute: GPU watts (nvidia-smi, all
    GPUs) and CPU utilisation. Only one process on the machine samples at a time (a heartbeat lock file), so the
    CITAR server and the lab runner can both start one.

    ``server_id`` None follows the host: whichever machine is the host is looked up at each sample, so a host that
    appears after the start (a fresh server-mode database's, imported when its first administrator is made) or one an
    administrator chooses later is sampled from then on, without a restart."""

    def __init__(self, server_id: Optional[str] = None, writer_lock: Optional[Path] = None):
        self.server_id = server_id
        self._writer_lock = writer_lock
        self._thread: Optional[threading.Thread] = None
        self._stop = threading.Event()
        self._beat = 0.0
        self.nvsmi = None
        import shutil
        self.nvsmi = shutil.which("nvidia-smi")

    def lock_path(self, server_id: str) -> Path:
        """The heartbeat file of whoever samples ``server_id``."""
        return self._writer_lock or USAGE_DIR / f"power-{server_id}.lock"

    def target(self) -> Optional[str]:
        """The machine sampled now: the one given, or the host (None while there is none)."""
        if self.server_id:
            return self.server_id
        from . import servers as S
        h = S.host()
        return h["id"] if h else None

    def start(self):
        """Start sampling."""
        if self._thread is None:
            self._thread = threading.Thread(target=self._loop, daemon=True, name="power-sampler")
            self._thread.start()

    def stop(self):
        """Stop sampling."""
        self._stop.set()

    def _own_lock(self, server_id: str) -> bool:
        """Whether this process is the one sampling, so two CITARs do not both measure."""
        me = f"{os.getpid()}"
        path = self.lock_path(server_id)
        try:
            if path.exists():
                txt = path.read_text(encoding="utf-8").split()
                if txt and txt[0] != me and time.time() - float(txt[1]) < 45:
                    return False
            # refresh the heartbeat every 20 s, not every sample (the folder may sync to the cloud)
            if time.time() - self._beat > 20:
                USAGE_DIR.mkdir(parents=True, exist_ok=True)
                path.write_text(f"{me} {time.time():.0f}", encoding="utf-8")
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
        current = None
        if psutil:
            psutil.cpu_percent(None)
        while not self._stop.wait(SAMPLE_SECONDS):
            try:
                from . import servers as S
                sid = self.target()
                if sid != current:
                    # another machine (or the first): its own minute, its own heartbeat file
                    gpu, cpu, n, start, current, self._beat = [], [], 0, time.time(), sid, 0.0
                sv = S.find(sid) if sid else None
                if sv is None or sv["power"].get("sampling") == "off" or not self._own_lock(sid):
                    gpu, cpu, n, start = [], [], 0, time.time()
                    continue
                w = self._gpu_watts()
                if w is not None:
                    gpu.append(w)
                if psutil:
                    cpu.append(psutil.cpu_percent(None) / 100.0)
                n += 1
                if time.time() - start >= 60:
                    row = {"k": "pw", "srv": sid, "t0": round(start, 1), "t1": round(time.time(), 1), "n": n,
                           "cpu_util": round(sum(cpu) / len(cpu), 4) if cpu else None,
                           "gpu_w": round(sum(gpu) / len(gpu), 2) if gpu else None}
                    append(f"power-{sid}", [row])
                    gpu, cpu, n, start = [], [], 0, time.time()
            except Exception:
                traceback.print_exc()


_sampler: Optional[PowerSampler] = None


def start_power_sampler(follow_host: bool = False):
    """Sample the host machine's power. ``follow_host`` (the CITAR server): start now whether or not there is a host
    yet, sampling whichever machine is the host at each sample (``PowerSampler``); otherwise (the lab runner, a
    process of its own) only when there is a host now, and that one."""
    global _sampler
    if _sampler is not None:
        return _sampler
    if follow_host:
        _sampler = PowerSampler(None)
        _sampler.start()
        return _sampler
    try:
        from . import servers as S
        h = S.host()
    except Exception:
        return None
    if h is None:
        return None
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
        # its CPU seconds are the compute ledger's (the server reads the lab's results: ``ledger.ingest_lab``)
        rows.append({"k": "span", "act": act, "srv": host_id, "t0": round(t0, 1), "t1": round(t1, 1),
                     "held": round(secs, 1)})
    return rows
