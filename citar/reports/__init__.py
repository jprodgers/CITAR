"""Reports: built on demand from a spec (the report builder's choices), written as self-contained HTML files.

    saves/reports/<id>/spec.json      what was asked for
    saves/reports/<id>/meta.json      status, progress, timings, headline numbers
    saves/reports/<id>/report.html    the report

A spec:
    {"title", "pooling": own|pooled|both, "range": {"preset": all|today|24h|7d|30d|90d|365d|custom, "from", "to"},
     "scope": {"servers": [ids], "models": [keys], "kinds": [game, benchmark, probe, lab, report],
               "items": [{"kind": game|benchmark_run|probe|probe_run|lab_experiment|suite|activity, "id", "name"}]},
     "sections": [...], "options": {"electricity_basis": full|marginal, "lifespan_range": [2, 6]},
     "narrative": {"enabled", "server_id", "model_id", "profile_id", "instructions"}}
"""
from __future__ import annotations

import json
import re
import secrets
import shutil
import threading
import time
import traceback
from datetime import datetime
from pathlib import Path
from typing import Optional
from .. import paths

REPORTS = paths.saves_path("reports")
ALL_SECTIONS = ["summary", "costs", "cost_per_unit", "efficiency", "servers", "server_trend", "models", "benchmarks", "probes", "behavior",
                "lab", "whatif", "depreciation", "hardware", "data_quality", "activities", "methodology"]

PRESETS = {
    "item_cost": {"name": "Cost of a game, run or experiment",
                  "description": "What one thing (or a few) cost, broken down by server and cost component.",
                  "sections": ["summary", "costs", "cost_per_unit", "efficiency", "benchmarks", "probes", "lab", "activities", "data_quality", "methodology"]},
    "model_comparison": {"name": "Model comparison",
                         "description": "Performance, speed, reliability and cost of each model side by side, with what-if pricing.",
                         "sections": ["summary", "models", "cost_per_unit", "efficiency", "costs", "benchmarks", "behavior", "whatif", "data_quality", "methodology"]},
    "server_comparison": {"name": "Server comparison over time",
                          "description": "What each server cost, how busy it was, idle vs allocated, and lifespan sensitivity.",
                          "sections": ["summary", "servers", "efficiency", "server_trend", "costs", "whatif", "depreciation", "hardware", "data_quality", "methodology"]},
    "behavior": {"name": "Model behavior in scenarios",
                 "description": "Probe outcomes per case and model, tool use, how turns ended and common errors.",
                 "sections": ["summary", "probes", "behavior", "models", "data_quality"]},
    "lab": {"name": "Bot lab experiments", "description": "Cost of bot-vs-bot experiments and their results per seat label.",
            "sections": ["summary", "lab", "costs", "servers", "data_quality", "methodology"]},
    "everything": {"name": "Everything", "description": "Every section.", "sections": ALL_SECTIONS},
}

#: What a report's figures come from (design/phase3-features.md P3.2.4): ``own``, the activities its owner may see
#: (their own, those shared with them, members' and published ones; everything for an administrator); ``pooled``,
#: only the averages other accounts pool, each withheld until enough accounts are behind it; ``both``.
POOLING = ("own", "pooled", "both")

#: The kind of work a report's narrative is for admission (``admission.PURPOSES``): a pooled prompt (P3.3.7). The route
#: admits the report's owner for it when the report is asked for, and the narrative's ticket again when it is made.
NARRATIVE_PURPOSE = "pooled"


def _now() -> float:
    """The current time."""
    return time.time()


def _part(d: dict, key: str, kind: type, what: str):
    """``d[key]`` when it is a ``kind`` (an empty one when it is missing or empty); a ValueError naming ``what``
    otherwise, so a request with a wrong shape is a 400, never a 500."""
    v = d.get(key)
    if not v:
        return kind()
    if not isinstance(v, kind):
        raise ValueError(f"A report's {key} is {what}.")
    return v


def _ref(v, key: str) -> Optional[str]:
    """A narrative's machine, model or profile: an id, or None."""
    if not v:
        return None
    if not isinstance(v, str):
        raise ValueError(f"A report's narrative names its {key} by its id.")
    return v


def normalize_spec(d: dict) -> dict:
    """Validate a report specification and fill in its defaults. A ValueError for one with a wrong shape."""
    d = d or {}
    if not isinstance(d, dict):
        raise ValueError("A report's specification is an object.")
    rng = _part(d, "range", dict, "an object")
    scope = _part(d, "scope", dict, "an object")
    opts = _part(d, "options", dict, "an object")
    narr = _part(d, "narrative", dict, "an object")
    try:
        lo, hi = (opts.get("lifespan_range") or [2, 6])[:2] if isinstance(opts.get("lifespan_range"), list) else (2, 6)
        lo, hi = max(1, min(20, int(lo))), max(1, min(30, int(hi)))
    except (TypeError, ValueError):
        raise ValueError("A report's lifespan range is two numbers of years.") from None
    preset = d.get("preset")
    pooling = d.get("pooling")
    return {
        "title": (str(d.get("title") or "").strip() or "CITAR report")[:120],
        "preset": preset if isinstance(preset, str) and preset in PRESETS else None,
        "pooling": pooling if isinstance(pooling, str) and pooling in POOLING else "own",
        "range": {"preset": rng.get("preset") if rng.get("preset") in ("all", "today", "24h", "7d", "30d", "90d", "365d", "custom") else "all",
                  "from": str(rng.get("from") or "")[:10], "to": str(rng.get("to") or "")[:10]},
        "scope": {"servers": [str(s) for s in _part(scope, "servers", list, "a list")][:50],
                  "models": [str(s) for s in _part(scope, "models", list, "a list")][:50],
                  "kinds": [k for k in _part(scope, "kinds", list, "a list") if k in ("game", "benchmark", "probe", "lab", "report")],
                  "items": [{"kind": str(i.get("kind")), "id": str(i.get("id")), "name": str(i.get("name") or "")[:120]}
                            for i in _part(scope, "items", list, "a list") if isinstance(i, dict) and i.get("id")][:200]},
        "sections": [s for s in _part(d, "sections", list, "a list") or PRESETS["model_comparison"]["sections"]
                     if s in ALL_SECTIONS] or ["summary"],
        "options": {"electricity_basis": "marginal" if opts.get("electricity_basis") == "marginal" else "full",
                    "lifespan_range": [min(lo, hi), max(lo, hi)]},
        "narrative": {"enabled": bool(narr.get("enabled")), "server_id": _ref(narr.get("server_id"), "machine"),
                      "model_id": _ref(narr.get("model_id"), "model"),
                      "profile_id": _ref(narr.get("profile_id"), "profile"),
                      "instructions": str(narr.get("instructions") or "")[:2000]},
    }


NARRATIVE_SYSTEM = (
    "You are the analyst for CITAR, a research tool that benchmarks AI models by having them play a Civilization-like "
    "strategy game against scripted bots, and that tracks what running them costs on each server. Write the analysis "
    "section of a report from the facts given. Use only those facts: do not invent numbers, and say when data is thin "
    "or estimated. Lead with the most decision-relevant conclusion (which model or server gives the best results for "
    "the money, and at what cost), then explain the main cost drivers, then caveats and concrete recommendations. "
    "Plain paragraphs, optionally short '- ' bullet lists; no tables; 250-500 words.")


class NarrativeRefused(RuntimeError):
    """The narrative's machine will not take its prompt: a refusal that does not clear (no grant, the model not loaded,
    the machine disabled), or one that does not clear before the narrative's start deadline (``START_SECONDS``)."""


def _ordinal(n: int) -> str:
    """1st, 2nd, 3rd, 4th..."""
    return f"{n}{'th' if 10 <= n % 100 <= 20 else {1: 'st', 2: 'nd', 3: 'rd'}.get(n % 10, 'th')}"


class ReportRunner:
    """Builds reports in the background and keeps the results.

    Each queued report is built on a thread of its own, up to ``MAX_BUILDS`` at once, highest queue priority first, then
    oldest; gathering and rendering (the server's own disk and CPU) go one at a time. A report's written analysis is a
    pooled prompt (design/phase3-features.md P3.3.3, P3.3.5): a ``report`` ticket on the machine its specification
    names, admitted when it is made (``pool.seats.ticket``: the owner's grant for ``pooled``), waiting for a benchmark
    that holds the machine rather than refused by it, and at most ``START_SECONDS`` to start. So a report holds no
    lease, never occupies its machine, and is not in the shared job queue: its prompt waits in the machine's queue
    with every other pooled prompt, by priority (the report's queue priority, which its ticket follows), then
    round-robin across members, then arrival."""

    #: Reports built at once: most of a build is waiting for its narrative's machine.
    MAX_BUILDS = 4
    #: How long a narrative may wait to start (its ticket's deadline), a refusal that says "not yet" (the helper
    #: offline, the machine's quiet hours, the grant's window) included; one whose end is known to come later is given
    #: up at once.
    START_SECONDS = 1800.0
    #: How long a narrative waits for a helper that dropped while it was written (its request failed with a connection
    #: error worth waiting out), as the narrative's own polling of a helper waited before the scheduler.
    RECONNECT_SECONDS = 1800.0
    #: The longest pause between two looks while a narrative waits for a refusal to clear or a helper to come back; a
    #: helper that comes back is tried at once, after at least ``RETRY_MIN_SECONDS`` (never in a tight loop).
    RECONNECT_POLL_SECONDS = 15.0
    RETRY_MIN_SECONDS = 1.0
    #: How often the dispatcher looks at the queue when nothing wakes it.
    POLL_SECONDS = 5.0

    def __init__(self, directory: Path = REPORTS):
        self.dir = Path(directory)
        self.dir.mkdir(parents=True, exist_ok=True)
        self.lock = threading.Lock()
        self.queue: list[str] = []
        self._builds: dict = {}                 # report id -> the thread building it
        self._thread: Optional[threading.Thread] = None
        self._kick = threading.Event()
        self._cpu = threading.Lock()            # gathering and rendering, one report at a time
        #: where a building report's narrative is in its machine's line (the ticket's status), shown as its progress
        self._places: dict = {}
        requeue = []
        for m in self.list():
            if m["status"] == "running":
                self._set(m["id"], status="failed", error="The CITAR server restarted while this report was running; run it again.")
            elif m["status"] == "queued":
                requeue.append(m["id"])
        for rid in reversed(requeue):          # list() is newest first
            self.queue.append(rid)
        if self.queue:
            self._wake()

    # ------------------------------------------------------------------ storage
    def _path(self, rid: str) -> Path:
        """The directory for a report, rejecting an id that is not a plain slug."""
        if not re.fullmatch(r"[a-z0-9-]{6,40}", rid or ""):
            raise KeyError(rid)
        return self.dir / rid

    def _stored(self, rid: str) -> dict:
        """A report's metadata as its file holds it."""
        p = self._path(rid) / "meta.json"
        if not p.exists():
            raise KeyError(rid)
        for _ in range(20):
            try:
                return json.loads(p.read_text(encoding="utf-8"))
            except (PermissionError, ValueError):
                time.sleep(0.05)
        return json.loads(p.read_text(encoding="utf-8"))

    def _live(self, m: dict) -> dict:
        """``m`` with where its narrative waits in its machine's line, while it waits (``_places``)."""
        place = self._places.get(m.get("id"))
        return dict(m, progress=place) if place and m.get("status") == "running" else m

    def meta(self, rid: str) -> dict:
        """A report's metadata."""
        return self._live(self._stored(rid))

    def _set(self, rid: str, **fields) -> dict:
        """Update a report's metadata."""
        from ..fsutil import write_text
        with self.lock:
            p = self._path(rid)
            try:
                m = self._stored(rid)
            except KeyError:
                m = {"id": rid}
            m.update(fields)
            p.mkdir(parents=True, exist_ok=True)
            write_text(p / "meta.json", json.dumps(m, indent=1, default=str))
            return m

    def list(self) -> list[dict]:
        """Every report, newest first."""
        out = []
        for p in sorted(self.dir.glob("*/meta.json"), key=lambda p: p.stat().st_mtime, reverse=True):
            try:
                out.append(self._live(json.loads(p.read_text(encoding="utf-8"))))
            except (OSError, ValueError):
                pass
        return out

    def spec(self, rid: str) -> dict:
        """The specification a report was built from."""
        return json.loads((self._path(rid) / "spec.json").read_text(encoding="utf-8"))

    def html_path(self, rid: str) -> Path:
        """Where a report's HTML lives."""
        p = self._path(rid) / "report.html"
        if not p.exists():
            raise KeyError(rid)
        return p

    def delete(self, rid: str):
        """Delete a report and everything it produced. A queued report is taken out of the queue."""
        m = self.meta(rid)
        if m["status"] == "running":
            raise ValueError("The report is still running.")
        with self.lock:
            if rid in self.queue:
                self.queue.remove(rid)
        shutil.rmtree(self._path(rid), ignore_errors=True)

    # ------------------------------------------------------------------ running
    def start(self, spec: dict, *, owner_id: Optional[str] = None, session=None) -> dict:
        """Queue a report to be built. ``owner_id`` is the account asking for it: the report is theirs
        (design/phase3-features.md P3.2.4), its meta records them, and the runner writes its row in ``owned_items``
        (kind ``report``, its specification in the row's meta): in ``session`` when given (a request's, committed
        before the report is queued), else in a session of its own when the process has its database. A report
        made where there is none (a script) has no row until adoption gives it one, to the owner it records."""
        spec = normalize_spec(spec)
        rid = datetime.now().strftime("%Y%m%d-%H%M%S-") + secrets.token_hex(2)
        p = self._path(rid)
        self._register(rid, spec, owner_id, session)
        p.mkdir(parents=True, exist_ok=True)
        (p / "spec.json").write_text(json.dumps(spec, indent=1), encoding="utf-8")
        m = self._set(rid, title=spec["title"], preset=spec.get("preset"), status="queued", created=_now(), started=None,
                      finished=None, progress="Waiting", error=None, summary=None, narrative=spec["narrative"]["enabled"],
                      owner_id=owner_id)
        with self.lock:
            self.queue.append(rid)
        self._wake()
        return m

    def _register(self, rid: str, spec: dict, owner_id: Optional[str], session) -> None:
        """The report's row (``start``)."""
        from .. import db, items
        path = self._path(rid) / "report.html"
        if session is not None:
            items.register(session, "report", rid, owner_id, spec["title"], path=path, meta={"spec": spec})
            session.commit()
        elif db.configured():
            with db.session() as s:
                items.register(s, "report", rid, owner_id, spec["title"], path=path, meta={"spec": spec})

    def _wake(self):
        """Start the dispatcher if it is not running, and have it look at the queue now."""
        self._kick.set()
        with self.lock:
            if self._thread is None or not self._thread.is_alive():
                self._thread = threading.Thread(target=self._loop, daemon=True, name="reports")
                self._thread.start()

    def _ranked(self) -> list:
        """The queued reports, highest queue priority first, then oldest. Lock held."""
        from ..pool import queue as work_queue
        ranked = []
        for rid in list(self.queue):
            try:
                created = float(self._stored(rid).get("created") or 0)
            except KeyError:
                self.queue.remove(rid)
                continue
            ranked.append(((-work_queue.priority("report", rid), created), rid))
        return [rid for _, rid in sorted(ranked)]

    def _loop(self):
        """The dispatcher: start the best-ranked queued reports, each on a thread of its own, up to ``MAX_BUILDS``."""
        while True:
            self._kick.clear()                  # a wake from here on is seen by the next look
            with self.lock:
                self._builds = {rid: t for rid, t in self._builds.items() if t.is_alive()}
                if not self.queue and not self._builds:
                    self._thread = None
                    return
                starting = []
                for rid in self._ranked():
                    if len(self._builds) >= self.MAX_BUILDS:
                        break
                    self.queue.remove(rid)
                    t = threading.Thread(target=self._build, args=(rid,), daemon=True, name=f"report-{rid}")
                    self._builds[rid] = t
                    starting.append(t)
            for t in starting:
                t.start()
            self._kick.wait(self.POLL_SECONDS)

    def _build(self, rid: str):
        """One report's thread: build it, recording a failure instead of losing it; then let the dispatcher start the
        next."""
        try:
            self.run(rid)
        except Exception as e:
            self._set(rid, status="failed", error=f"{type(e).__name__}: {e}", trace=traceback.format_exc(limit=8),
                      finished=_now())
        finally:
            self._places.pop(rid, None)
            self._kick.set()

    def run(self, rid: str):
        """Build one report: gather (what its owner may see: ``reach.Reach``), cost, render, write; its narrative, when
        it has one, is a ``report`` ticket on the machine it names (``_narrative``), waited for with nothing else of
        the build held."""
        from .data import gather
        from .reach import Reach
        from .render import render, narrative_brief
        spec = normalize_spec(self.spec(rid))
        t0 = _now()
        self._set(rid, status="running", started=t0, progress="Collecting data", waiting=None, waiting_note=None)
        with self._cpu:
            data = gather(spec, progress=lambda msg: self._set(rid, progress=msg),
                          reach=Reach.of(self.meta(rid).get("owner_id")))
        narrative, note = "", ""
        if spec["narrative"]["enabled"]:
            self._set(rid, progress="Writing the analysis with a model")
            narrative, note = self._narrative(rid, spec, narrative_brief(data))
        self._set(rid, progress="Rendering")
        with self._cpu:
            page, summary = render(data, narrative, note)
        from ..fsutil import write_text
        write_text(self._path(rid) / "report.html", page)
        self._set(rid, status="done", finished=_now(), progress="Done", summary=summary, seconds=round(_now() - t0, 1),
                  size=len(page.encode("utf-8")))

    def _status(self, rid: str, name: str, payload: dict) -> None:
        """The narrative's ticket's news (the scheduler's notifier thread, no lock held): where it is in its machine's
        line while it waits, shown as the report's progress (``_places``: nothing is written to the disk here)."""
        state = payload.get("state")
        if state == "queued" and payload.get("position"):
            eta = payload.get("eta_s")
            self._places[rid] = (f"Waiting for {name}: {_ordinal(int(payload['position']))} in line"
                                 + (f", about {eta:.0f} s" if isinstance(eta, (int, float)) else ""))
        else:
            self._places.pop(rid, None)

    def _ticket(self, rid: str, sv: dict, cfg: dict, deadline: float):
        """The narrative's ``report`` ticket on its machine (P3.3.3): the report's owner its consumer, admitted for
        ``pooled`` with ``run_reports`` when it is made (``pool.seats.ticket``), following the report's queue priority,
        waiting for a benchmark that holds the machine, one step, to start by ``deadline`` (on ``time.perf_counter``).
        A refusal that says "not yet" (the helper offline, the machine's quiet hours, the grant's window or budget)
        is waited out while it may clear before the deadline; anything else raises ``NarrativeRefused``."""
        from ..pool import queue as work_queue, seats as pool_seats
        meta = self._stored(rid)
        title = meta.get("title") or rid
        while True:
            left = deadline - time.perf_counter()
            ticket = pool_seats.ticket(
                sv["id"], cfg.get("model") or "", cls="report", consumer=meta.get("owner_id"),
                capability="run_reports", activity=f"report:{rid}", label=f"report “{title}”", ref=("report", rid),
                priority=work_queue.priority("report", rid), start_within=max(1.0, left), steps=1,
                wait_for_benchmark=True, on_status=lambda payload: self._status(rid, sv["name"], payload))
            if ticket.state == "queued":
                return ticket
            error = ticket.error
            clears = error.retry_at if error is not None else None
            if error is None or not error.temporary or left <= 0 or (
                    clears is not None and (clears - datetime.now()).total_seconds() > left):
                raise NarrativeRefused(ticket.reason or "the machine refused the prompt")
            self._set(rid, progress=f"Waiting: {ticket.reason}")
            self._pause(sv["id"], deadline, clears)
            self._set(rid, progress="Writing the analysis with a model")

    def _pause(self, server_id: str, deadline: float, clears: Optional[datetime] = None) -> None:
        """Wait before trying a refused or dropped narrative again: at least ``RETRY_MIN_SECONDS``, at most
        ``RECONNECT_POLL_SECONDS``, sooner when a helper machine's helper is back, never past ``deadline`` or the moment
        the refusal clears (``clears``). The build's thread holds nothing while it waits."""
        now = time.perf_counter()
        latest = min(deadline, now + self.RECONNECT_POLL_SECONDS)
        if clears is not None:
            latest = min(latest, now + max(self.RETRY_MIN_SECONDS, (clears - datetime.now()).total_seconds()))
        soonest = now + self.RETRY_MIN_SECONDS
        from .. import servers
        helper = servers.is_helper(server_id)
        while time.perf_counter() < latest:
            if time.perf_counter() >= soonest and helper and _helper_online(server_id):
                return
            time.sleep(0.25)

    def _complete_waiting(self, rid: str, sv: dict, cfg: dict, prompt: str) -> tuple[str, float]:
        """``complete`` on the narrative's ticket (``_ticket``), riding out a helper that drops or reconnects while
        the analysis is written: a connection error worth waiting out (``WorkerConnectionError``: the helper not
        connected, gone mid-request) is tried again on a new ticket once the helper is back, for up to
        ``RECONNECT_SECONDS`` from the first drop; anything else fails at once."""
        from ..agents.providers.worker_provider import WorkerConnectionError
        start_by = time.perf_counter() + self.START_SECONDS
        dropped_by: Optional[float] = None      # set at the first drop: how long the helper is waited for
        while True:
            ticket = self._ticket(rid, sv, cfg, start_by if dropped_by is None else dropped_by)
            try:
                return complete(cfg, NARRATIVE_SYSTEM, prompt, ticket)
            except WorkerConnectionError as e:
                if dropped_by is None:
                    dropped_by = time.perf_counter() + self.RECONNECT_SECONDS
                if time.perf_counter() >= dropped_by:
                    raise
                self._set(rid, progress=f"Waiting for {sv['name']}'s helper: {e}")
            finally:
                ticket.close()
            self._pause(sv["id"], dropped_by)
            self._set(rid, progress="Writing the analysis with a model")

    def _narrative(self, rid: str, spec: dict, brief: str) -> tuple[str, str]:
        """Ask the chosen model for the written analysis, as a ``report`` ticket on its machine (a scheduled request
        like every model call, P3.3.4: the usage ledger's listener meters it to the report's activity)."""
        from .. import servers, usage
        n = spec["narrative"]
        sv = servers.find(n["server_id"])
        if sv is None:
            return "", f"The narrative was skipped: no server '{n['server_id']}'."
        try:
            ref = servers.seat_ref(sv["id"], n["model_id"], n["profile_id"])
            cfg = servers.resolve_llm(ref)
        except servers.ServerError as e:
            return "", f"The narrative was skipped: {e}"
        act = f"report:{rid}"
        tr = usage.tracker()
        tr.activity(act, "report", spec["title"], parent={"kind": "report", "id": rid, "name": spec["title"]},
                    server_id=sv["id"], model=cfg["model"])
        holding = {"on": True}
        host = servers.load().get("host_server_id")
        tr.register(act, lambda: {sid: {"threads": []} for sid in {sv["id"], host} if sid} if holding["on"] else None)
        prompt = brief + ("\n\nExtra instructions from the reader:\n" + n["instructions"] if n.get("instructions") else "")
        try:
            self._load_model(sv, cfg)
            text, secs = self._complete_waiting(rid, sv, cfg, prompt)
            return text, f"Written by {cfg['model']} on {sv['name']} in {secs:.0f}s from the computed figures above."
        except NarrativeRefused as e:
            return "", f"The narrative was skipped: {e}"
        except Exception as e:
            return "", f"The narrative failed: {type(e).__name__}: {e}"
        finally:
            holding["on"] = False
            tr.update(act, ended=_now())
            tr.unregister(act)
            tr.flush()

    @staticmethod
    def _load_model(sv: dict, cfg: dict) -> None:
        """Load the narrative's model, with its load profile, on a machine CITAR manages, when its owner lets pooled
        prompts swap models (P3.3.3: a ticket runs on a loaded model, unless the machine allows swaps); CITAR loads
        nothing on a helper's machine. The scheduler is told, so the ticket's loaded-model rule reads the model now
        loaded."""
        from .. import servers
        if servers.is_helper(sv["id"]) or not (servers.machine_settings(sv["id"]) or {}).get("allow_swaps"):
            return
        try:
            if servers.ensure_model(sv, cfg["model"], cfg.get("load")):
                from ..pool import scheduler as prompt_scheduler
                prompt_scheduler.get().models_changed(sv["id"])
        except Exception:
            pass


def _helper_online(server_id: str) -> bool:
    """Whether a helper machine's helper is connected now (the hub's view; no database read)."""
    from ..server.workers import hub
    return hub().is_online(server_id)


def complete(cfg: dict, system: str, prompt: str, binding) -> tuple[str, float]:
    """One plain completion (no tools) under ``binding`` (a ticket of ``citar.pool.scheduler``, the narrative's): the one
    path every model call takes (``make_conversation``, P3.3.4), whatever the provider, so the request waits for its
    machine's slot rather than polling a busy helper, and the usage ledger's listener meters it. Returns the text,
    without a model's ``<think>`` block, and the generation seconds the scheduler measured."""
    from ..agents.providers import make_conversation
    conv = make_conversation(dict(cfg, tool_mode="native"), system, [], binding=binding)
    conv.request_timeout = float(cfg.get("timeout") or 1800)
    try:
        conv.add_user_text(prompt)
        step = conv.step()
    finally:
        conv.close()
    if step.stop_reason == "refusal":
        raise RuntimeError("the model declined to write the analysis")
    text = re.sub(r"<think>.*?</think>", "", step.text or "", flags=re.S).strip()
    return text, float(getattr(conv, "last_gen_s", 0.0) or 0.0)


def scope_options(reach=None) -> dict:
    """What the report builder can pick from: servers, models, and recent items (runs, experiments, games), as far as
    ``reach`` goes (``reach.Reach``, the asker's: a member is offered what their report could gather, and the machines
    they may see; everything by default)."""
    from .. import servers, usage
    from .reach import Reach
    reach = reach or Reach()
    reg = servers.with_pooled()
    ledger = reach.ledger(usage.read())
    if not reach.everything:
        reg = {**reg, "servers": [sv for sv in reg["servers"] if sv["id"] in reach.names]}
    items: dict = {}
    models = set()
    for a in ledger["acts"].values():
        parent = a.get("parent") or {}
        ref = a.get("ref") or {}
        t = a.get("t") or 0
        if parent.get("kind") in ("benchmark_run", "probe", "lab_experiment") and parent.get("id"):
            key = (parent["kind"], parent["id"])
            cur = items.setdefault(key, {"kind": parent["kind"], "id": parent["id"], "name": parent.get("name") or parent["id"],
                                         "t": t, "count": 0})
            cur["count"] += 1
            cur["t"] = max(cur["t"], t)
        if a.get("kind") == "game" and ref.get("game_id"):
            items[("game", ref["game_id"])] = {"kind": "game", "id": ref["game_id"], "name": a.get("name") or ref["game_id"],
                                                "t": t, "count": 1}
        if ref.get("probe_run"):
            items[("probe_run", ref["probe_run"])] = {"kind": "probe_run", "id": ref["probe_run"], "name": a.get("name"),
                                                       "t": t, "count": 1}
        for s in a.get("seats") or []:
            if s.get("model"):
                models.add(s["model"])
        if a.get("model"):
            models.add(a["model"])
    return {"servers": [{"id": s["id"], "name": s["name"], "kind": s["kind"]} for s in reg["servers"]],
            "models": sorted(models | {m["key"] for s in reg["servers"] for m in s["models"]}),
            "items": sorted(items.values(), key=lambda i: -i["t"])[:300],
            "presets": [{"id": k, **v} for k, v in PRESETS.items()], "sections": ALL_SECTIONS}
