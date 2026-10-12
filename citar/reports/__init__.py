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

#: The kind of work a report's narrative is for admission (``admission.PURPOSES``): one prompt on a machine, as a
#: game's turn is. The route admits it for this purpose and the runner's lease is capped by the grant for it.
#: Package 3-09 gives pooled prompts a purpose of their own.
NARRATIVE_PURPOSE = "game"


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


class ReportRunner:
    """Builds reports in the background, one at a time, and keeps the results."""
    def __init__(self, directory: Path = REPORTS):
        self.dir = Path(directory)
        self.dir.mkdir(parents=True, exist_ok=True)
        self.lock = threading.Lock()
        self.queue: list[str] = []
        self._thread: Optional[threading.Thread] = None
        requeue = []
        for m in self.list():
            if m["status"] == "running":
                self._set(m["id"], status="failed", error="The CITAR server restarted while this report was running; run it again.")
            elif m["status"] == "queued":
                requeue.append(m["id"])
        from ..pool import queue as work_queue
        work_queue.register("report", self.queue_items, self.running_items)
        self._current: Optional[str] = None
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

    def meta(self, rid: str) -> dict:
        """A report's metadata."""
        p = self._path(rid) / "meta.json"
        if not p.exists():
            raise KeyError(rid)
        for _ in range(20):
            try:
                return json.loads(p.read_text(encoding="utf-8"))
            except (PermissionError, ValueError):
                time.sleep(0.05)
        return json.loads(p.read_text(encoding="utf-8"))

    def _set(self, rid: str, **fields) -> dict:
        """Update a report's metadata."""
        from ..fsutil import write_text
        with self.lock:
            p = self._path(rid)
            try:
                m = self.meta(rid)
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
                out.append(json.loads(p.read_text(encoding="utf-8")))
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

    # ------------------------------------------------------------------ the shared queue
    def _machine(self, rid: str) -> Optional[str]:
        """The model machine a report needs (for its written analysis), or None."""
        try:
            n = self.spec(rid)["narrative"]
        except (KeyError, OSError, ValueError):
            return None
        return n.get("server_id") if n.get("enabled") else None

    def queue_items(self) -> list[dict]:
        """Queued reports that need a model machine, for the shared work queue (see citar.pool.queue)."""
        from .. import servers
        with self.lock:
            rids = list(self.queue)
        out = []
        for rid in rids:
            sid = self._machine(rid)
            if not sid:
                continue
            try:
                m = self.meta(rid)
            except KeyError:
                continue
            title = m.get("title") or "report"
            out.append({"kind": "report", "id": rid, "group": rid, "run": title, "label": f"report “{title}”",
                        "server_id": sid, "server": servers.server_name(sid), "created": m.get("created") or 0,
                        "waiting": m.get("waiting"), "waiting_note": m.get("waiting_note")})
        return out

    def running_items(self) -> list[dict]:
        """The report whose analysis a model is writing now, for the queue page."""
        rid = self._current
        sid = self._machine(rid) if rid else None
        if not sid:
            return []
        from .. import servers
        title = self.meta(rid).get("title") or "report"
        return [{"kind": "report", "id": rid, "group": rid, "run": title, "label": f"report “{title}”",
                 "server_id": sid, "server": servers.server_name(sid), "created": self.meta(rid).get("created") or 0}]

    def _blocked(self, rid: str) -> Optional[dict]:
        """Why a queued report cannot start yet, as a ``pool.seats.note`` (None when it can): its machine is in its
        quiet hours, its helper is offline, it is in use, or higher-priority work is waiting for it. Reports without a
        written analysis never wait. A note naming other work is told to each viewer as they may see it
        (``work_access.with_waiting``)."""
        sid = self._machine(rid)
        if not sid:
            return None
        from .. import servers
        from ..pool import queue as work_queue, seats as pool_seats
        name = servers.server_name(sid)
        end = servers.restricted_now(sid)
        if end:
            return pool_seats.note(f"{name} is in its quiet hours until {servers.restricted_text(sid, end)}", [])
        if servers.is_helper(sid) and not _helper_online(sid):
            # its narrative would fail at once (a lease's request on a machine of unknown capacity passes through to a
            # hub with no helper): the report waits for the helper, as for quiet hours, holding nothing
            return pool_seats.note(f"{name}'s helper is offline", [])
        held = pool_seats.holders(sid)
        if held:
            return pool_seats.note(f"{name} is in use by ", held)
        m = self.meta(rid)
        first = work_queue.ahead_of(sid, "report", (-work_queue.priority("report", rid), float(m.get("created") or 0)))
        if first is not None:
            return pool_seats.note("", [pool_seats.queue_holder(first)],
                                   f" goes first on {name} (priority {first['priority']})")
        return None

    def _next(self) -> Optional[str]:
        """The queued report to build now: the best-ranked one that is not waiting for its machine."""
        from ..pool import queue as work_queue
        with self.lock:
            rids = list(self.queue)
        ranked = []
        for rid in rids:
            try:
                created = float(self.meta(rid).get("created") or 0)
            except KeyError:
                with self.lock:
                    if rid in self.queue:
                        self.queue.remove(rid)
                continue
            ranked.append(((-work_queue.priority("report", rid), created), rid))
        from ..pool import seats as pool_seats
        for _, rid in sorted(ranked):
            note = self._blocked(rid)
            if note is None:
                return rid
            why = pool_seats.note_text(note)
            m = self.meta(rid)
            if m.get("waiting_note") != note or m.get("waiting") != why:
                self._set(rid, waiting=why, waiting_note=note, progress=f"Waiting: {why}")
        return None

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

    POLL_SECONDS = 5.0

    def _wake(self):
        """Start the worker thread if it is not running."""
        if self._thread is None or not self._thread.is_alive():
            self._thread = threading.Thread(target=self._loop, daemon=True, name="reports")
            self._thread.start()

    def _loop(self):
        """The worker thread: build queued reports, best first. One that needs a busy model machine waits for it
        (in the shared queue) while the others go ahead."""
        while True:
            with self.lock:
                if not self.queue:
                    self._thread = None
                    return
            try:
                rid = self._next()
            except Exception:
                traceback.print_exc()
                rid = None
            if rid is None:
                time.sleep(self.POLL_SECONDS)
                continue
            with self.lock:
                if rid in self.queue:
                    self.queue.remove(rid)
            self._set(rid, waiting=None, waiting_note=None)
            lease = None
            self._current = rid
            try:
                lease = self._lease(rid)
                self.run(rid, lease=lease)
            except Exception as e:
                self._set(rid, status="failed", error=f"{type(e).__name__}: {e}", trace=traceback.format_exc(limit=8),
                          finished=_now())
            finally:
                self._current = None
                if lease is not None:
                    lease.release()

    def _lease(self, rid: str):
        """The lease a report whose analysis a model writes holds on that machine while it is built
        (design/phase3-features.md P3.3.3): non-lending (``report``), so ``occupied()`` lists the report and queued
        benchmarks and probe runs wait for it, and the narrative's request is its. None for a report without a
        narrative. Package 3-09 makes the narrative a ``report`` ticket instead."""
        sid = self._machine(rid)
        if not sid:
            return None
        from ..pool import scheduler as prompt_scheduler, seats as pool_seats
        meta = self.meta(rid)
        owner = meta.get("owner_id")
        return prompt_scheduler.get().lease(
            sid, kind="report", activity=f"report:{rid}", consumer=owner, label=f"report “{meta.get('title') or rid}”",
            cap=pool_seats.consumer_cap(owner, sid, NARRATIVE_PURPOSE), ref=("report", rid))

    def run(self, rid: str, lease=None):
        """Build one report: gather (what its owner may see: ``reach.Reach``), cost, render, write. ``lease`` is the
        report's on its narrative's machine (``_lease``), which the narrative's request is made under."""
        from .data import gather
        from .reach import Reach
        from .render import render, narrative_brief
        spec = normalize_spec(self.spec(rid))
        t0 = _now()
        self._set(rid, status="running", started=t0, progress="Collecting data")
        data = gather(spec, progress=lambda msg: self._set(rid, progress=msg),
                      reach=Reach.of(self.meta(rid).get("owner_id")))
        narrative, note = "", ""
        if spec["narrative"]["enabled"]:
            self._set(rid, progress="Writing the analysis with a model")
            narrative, note = self._narrative(rid, spec, narrative_brief(data), lease)
        self._set(rid, progress="Rendering")
        page, summary = render(data, narrative, note)
        from ..fsutil import write_text
        write_text(self._path(rid) / "report.html", page)
        self._set(rid, status="done", finished=_now(), progress="Done", summary=summary, seconds=round(_now() - t0, 1),
                  size=len(page.encode("utf-8")))

    #: How long a narrative waits for a helper that dropped while the report was built (its request failed with a
    #: connection error worth waiting out), as the narrative's own polling of a helper waited before the scheduler
    RECONNECT_SECONDS = 1800.0
    #: The longest pause between two tries while it waits; a helper that comes back is tried at once, after at least
    #: ``RETRY_MIN_SECONDS`` (an error from a helper the hub still lists is never retried in a tight loop)
    RECONNECT_POLL_SECONDS = 15.0
    RETRY_MIN_SECONDS = 1.0

    def _complete_waiting(self, rid: str, sv: dict, cfg: dict, prompt: str, lease) -> tuple[str, float]:
        """``complete`` under ``lease``, riding out a helper that drops or reconnects while the report is built: a
        connection error worth waiting out (``WorkerConnectionError``: the helper not connected, gone mid-request) is
        tried again once the helper is back, for up to ``RECONNECT_SECONDS``; anything else fails at once. The
        runner's thread holds no lock while it waits."""
        from ..agents.providers.worker_provider import WorkerConnectionError
        deadline = time.perf_counter() + self.RECONNECT_SECONDS
        while True:
            try:
                return complete(cfg, NARRATIVE_SYSTEM, prompt, lease)
            except WorkerConnectionError as e:
                if time.perf_counter() >= deadline:
                    raise
                self._set(rid, progress=f"Waiting for {sv['name']}'s helper: {e}")
            now = time.perf_counter()
            soonest, latest = now + self.RETRY_MIN_SECONDS, min(deadline, now + self.RECONNECT_POLL_SECONDS)
            while time.perf_counter() < latest and not (time.perf_counter() >= soonest and _helper_online(sv["id"])):
                time.sleep(0.25)
            self._set(rid, progress="Writing the analysis with a model")

    def _narrative(self, rid: str, spec: dict, brief: str, lease=None) -> tuple[str, str]:
        """Ask the chosen model for the written analysis, under the report's ``lease`` (a scheduled request like every
        model call, P3.3.4: the usage ledger's listener meters it to the report's activity)."""
        from .. import servers, usage
        n = spec["narrative"]
        sv = servers.find(n["server_id"])
        if sv is None:
            return "", f"The narrative was skipped: no server '{n['server_id']}'."
        end = servers.restricted_now(sv["id"])
        if end:
            return "", (f"The narrative was skipped: {sv['name']} is in its restricted hours until "
                        f"{servers.restricted_text(sv['id'], end)}.")
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
            if not servers.is_helper(sv["id"]):        # CITAR does not load models on somebody else's machine
                try:
                    servers.ensure_model(sv, cfg["model"], cfg.get("load"))
                except Exception:
                    pass
            own = None
            if lease is None:           # built outside the runner's loop (a script): a lease for the narrative alone
                from ..pool import scheduler as prompt_scheduler
                lease = own = prompt_scheduler.get().lease(sv["id"], kind="report", activity=act,
                                                           label=f"report “{spec['title']}”", ref=("report", rid))
            try:
                lease.set_model(cfg["model"])
                text, secs = self._complete_waiting(rid, sv, cfg, prompt, lease)
            finally:
                if own is not None:
                    own.release()
            return text, f"Written by {cfg['model']} on {sv['name']} in {secs:.0f}s from the computed figures above."
        except Exception as e:
            return "", f"The narrative failed: {type(e).__name__}: {e}"
        finally:
            holding["on"] = False
            tr.update(act, ended=_now())
            tr.unregister(act)
            tr.flush()


def _helper_online(server_id: str) -> bool:
    """Whether a helper machine's helper is connected now (the hub's view; no database read)."""
    from ..server.workers import hub
    return hub().is_online(server_id)


def complete(cfg: dict, system: str, prompt: str, binding) -> tuple[str, float]:
    """One plain completion (no tools) under ``binding`` (a lease or a ticket of ``citar.pool.scheduler``): the one path
    every model call takes (``make_conversation``, P3.3.4), whatever the provider, so the request waits for its
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
