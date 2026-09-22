"""Probes: scripted experiments on one AI seat of a scenario. Every case starts from the scenario's saved state, applies
its setup, lets the subject respond once (a deal offer, a message, or a whole turn), records what it did, and throws
the game away — so each case sees exactly the same world.

A probe file (saves/probes/<id>.json):

    {"format": "citar-probe", "version": 1, "id": "trade-limits", "name": "...", "description": "...",
     "scenario": "industrial-duel",       # scenario id (saves/scenarios)
     "subject": 1,                        # the seat under test (played by the model chosen when the run starts)
     "counterparty": 0,                   # the scripted seat that makes the offers
     "setup": [ops...],                   # scenario operations applied before every case
     "repeats": 1,                        # default runs per case (sampling variation)
     "cases": [
       {"id": "oil-for-gold", "kind": "offer", "message": "300 gold for 2 Oil?",
        "give": [{"type": "gold", "amount": 300}],                      # what the counterparty gives the subject
        "receive": [{"type": "resource", "resource": "Oil", "amount": 2}], # what it asks for
        "on_counter": "reject",            # scripted answer to a counter-offer: reject | accept | leave
        "expect": "accept",                # optional: accept | reject | counter | reply (marks pass/fail)
        "setup": [ops...]},
       {"id": "open-turn", "kind": "turn", "expect_tools": ["declare_war"], "forbid_tools": ["make_peace"]},
       {"id": "ask-alliance", "kind": "message", "message": "Will you join me against Rome?"}
     ]}

Deal item types are the negotiation items (gold, gold_per_turn, resource, tech, city, open_borders, embassy,
peace_treaty, declaration_of_friendship, research_agreement, defensive_pact, declare_war).
Runs live in saves/probes/runs/<run id>/ (run.json, results.jsonl and one save per case for inspection).
"""
from __future__ import annotations

import json
import re
import secrets
import threading
import time
import traceback
from datetime import datetime
from pathlib import Path

from .fsutil import replace as _fs_replace
from typing import Optional

from .engine.game import ActionError
from . import paths

BASE = paths.saves_path("probes")
RUNS = BASE / "runs"
CASE_KINDS = ("offer", "message", "turn")


class ProbeError(ValueError):
    """A probe that cannot be run or saved as written."""
    pass


class _CaseInvalid(Exception):
    """One case is malformed; the rest of the probe can still run."""
    pass


# ----------------------------------------------------------------------------
# probe files
# ----------------------------------------------------------------------------
def slug(name: str) -> str:
    """A filesystem-safe identifier from a name."""
    s = re.sub(r"[^a-z0-9]+", "-", (name or "").lower()).strip("-")
    return s[:60] or f"probe-{int(time.time())}"


def _path(pid: str) -> Path:
    """The file for a probe id, rejecting anything that is not a plain slug."""
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,79}", pid or ""):
        raise ProbeError(f"Invalid probe id '{pid}'.")
    return BASE / f"{pid}.json"


def validate(data: dict) -> dict:
    """Checks a probe's structure (not its deal items; those are checked by the game when a case runs)."""
    from .engine import scenario as S
    if not isinstance(data, dict):
        raise ProbeError("A probe must be a JSON object.")
    if not data.get("scenario"):
        raise ProbeError("A probe needs a scenario id.")
    try:
        scn = S.load_scenario(data["scenario"])
    except ActionError as e:
        raise ProbeError(str(e))
    majors = [p["id"] for p in scn["state"]["players"] if p.get("kind") == "major"]
    for k in ("subject", "counterparty"):
        if k in data and data[k] is not None and int(data[k]) not in majors:
            raise ProbeError(f"{k} must be a major civilization of the scenario ({majors}).")
    if data.get("subject") is None:
        raise ProbeError("A probe needs a subject (the seat under test).")
    if data.get("counterparty") is not None and int(data["counterparty"]) == int(data["subject"]):
        raise ProbeError("The counterparty must differ from the subject.")
    cases = data.get("cases") or []
    if not cases:
        raise ProbeError("A probe needs at least one case.")
    seen = set()
    out_cases = []
    for n, c in enumerate(cases):
        if not isinstance(c, dict):
            raise ProbeError(f"Case {n + 1} is not an object.")
        c = dict(c)
        c.setdefault("id", f"case-{n + 1}")
        c.setdefault("kind", "offer")
        if c["kind"] not in CASE_KINDS:
            raise ProbeError(f"Case {c['id']}: kind must be one of {', '.join(CASE_KINDS)}.")
        if c["id"] in seen:
            raise ProbeError(f"Duplicate case id '{c['id']}'.")
        seen.add(c["id"])
        if c["kind"] in ("offer", "message") and data.get("counterparty") is None:
            raise ProbeError(f"Case {c['id']}: offers and messages need a counterparty.")
        if c["kind"] == "offer" and not (c.get("give") or c.get("receive")):
            raise ProbeError(f"Case {c['id']}: an offer needs give and/or receive items.")
        if c["kind"] in ("offer", "message") and not c.get("message"):
            c["message"] = "I have a proposal for you." if c["kind"] == "offer" else "Hello."
        if not isinstance(c.get("setup") or [], list):
            raise ProbeError(f"Case {c['id']}: setup must be a list of operations.")
        out_cases.append(c)
    clean = {"format": "citar-probe", "version": 1, "id": data.get("id") or slug(data.get("name", "")),
             "name": str(data.get("name") or data.get("id") or "Probe")[:100],
             "description": str(data.get("description") or "")[:4000], "scenario": data["scenario"],
             "subject": int(data["subject"]),
             "counterparty": int(data["counterparty"]) if data.get("counterparty") is not None else None,
             "setup": data.get("setup") or [], "repeats": max(1, min(50, int(data.get("repeats") or 1))),
             "cases": out_cases}
    return clean


def list_probes() -> list[dict]:
    """Every saved probe, in summary."""
    BASE.mkdir(parents=True, exist_ok=True)
    out = []
    for p in sorted(BASE.glob("*.json"), key=lambda p: p.stat().st_mtime, reverse=True):
        try:
            d = json.loads(p.read_text(encoding="utf-8"))
            out.append({"id": d.get("id"), "name": d.get("name"), "description": d.get("description", ""),
                        "scenario": d.get("scenario"), "cases": len(d.get("cases") or []), "repeats": d.get("repeats", 1),
                        "subject": d.get("subject"), "counterparty": d.get("counterparty"),
                        "modified": datetime.fromtimestamp(p.stat().st_mtime).isoformat(timespec="seconds")})
        except (OSError, ValueError):
            continue
    return out


def load_probe(pid: str) -> dict:
    """Load a probe from disk."""
    p = _path(pid)
    if not p.exists():
        raise ProbeError(f"No probe '{pid}'.")
    return json.loads(p.read_text(encoding="utf-8"))


def save_probe(data: dict) -> dict:
    """Validate and save a probe."""
    clean = validate(data)
    clean["id"] = slug(clean["id"] or clean["name"])
    BASE.mkdir(parents=True, exist_ok=True)
    p = _path(clean["id"])
    tmp = p.with_suffix(".tmp")
    tmp.write_text(json.dumps(clean, indent=1), encoding="utf-8")
    _fs_replace(tmp, p)
    return clean


def delete_probe(pid: str):
    """Delete a probe."""
    p = _path(pid)
    if p.exists():
        p.unlink()


def example_probe(scenario_id: str, subject: int = 1, counterparty: int = 0) -> dict:
    """An example probe against a scenario, as a starting point."""
    return {"id": "", "name": "New probe", "description": "What the subject will and won't accept.",
            "scenario": scenario_id, "subject": subject, "counterparty": counterparty, "repeats": 1, "setup": [],
            "cases": [
                {"id": "fair-gold-trade", "kind": "offer", "message": "I'll pay 200 gold for your friendship.",
                 "give": [{"type": "gold", "amount": 200}], "receive": [{"type": "declaration_of_friendship"}],
                 "on_counter": "reject", "expect": "accept"},
                {"id": "lopsided", "kind": "offer", "message": "Give me 500 gold, as a gesture of goodwill.",
                 "give": [], "receive": [{"type": "gold", "amount": 500}], "expect": "reject"},
                {"id": "ask-war", "kind": "message", "message": "Our neighbour grows too strong. Will you stand with me?"},
                {"id": "one-turn", "kind": "turn"}]}


# ----------------------------------------------------------------------------
# running cases
# ----------------------------------------------------------------------------
def _items_text(g, items) -> str:
    """A deal's items as text, for the record of what was offered."""
    from .engine.diplomacy import describe_items
    try:
        return describe_items(g, items or [])
    except Exception:
        return json.dumps(items)


def run_case(manager, scn: dict, probe: dict, case: dict, llm_cfg: dict, save_path: Optional[Path] = None,
             live: Optional[dict] = None, should_stop=lambda: False, usage_act: Optional[str] = None) -> dict:
    """Plays one case on a fresh copy of the scenario and returns its record."""
    from .engine import scenario as S, diplomacy as D, tools
    subject, cp = probe["subject"], probe.get("counterparty")
    seats = [None] * len(scn.get("seats") or [])
    seats = [{} for _ in range(max(len(seats), subject + 1, (cp or 0) + 1))]
    is_bot = llm_cfg.get("provider") == "bot"       # the scripted bot as the subject: the baseline
    seats[subject] = {"type": "bot", "bot": {"aggression": float(llm_cfg.get("aggression", 0.4))}} if is_bot \
        else {"type": "llm", "llm": llm_cfg}
    if cp is not None:
        seats[cp] = {"type": "script"}
    s = manager.create_from_scenario(scn, seats, name=f"probe {probe['id']}/{case['id']}", register=False, start=False)
    g = s.game
    if usage_act:
        # the case's model calls and the time it holds the servers go to the probe run's usage record
        from . import usage as ledger, servers
        s.usage_act = usage_act
        held = {sid: {"threads": []} for sid in {servers.load().get("host_server_id"), llm_cfg.get("server_id")} if sid}
        ledger.tracker().register(usage_act, lambda: None if s.stopped else held, key=f"{usage_act}#{case['id']}#{time.time()}")
    rec = {"case": case["id"], "kind": case["kind"], "started": datetime.now().isoformat(timespec="seconds"),
           "outcome": None, "passed": None, "subject_messages": [], "counter_offers": [], "tool_calls": [],
           "thoughts": [], "error": None}
    t0 = time.time()
    thought_mark = len(g.s.thoughts)
    calls: list = []
    orig_call = s.call_tool

    def recording_call(pid, name, args=None, *a, **kw):
        """Wrap the model's tool calls so each one is recorded as it happens."""
        res = orig_call(pid, name, args, *a, **kw)
        if pid == subject and len(calls) < 400:
            calls.append({"tool": name, "args": args, "ok": res.get("ok"),
                          "error": (res.get("error") or "")[:300] or None})
        return res
    s.call_tool = recording_call        # every call the model makes, queries and failures included
    if live is not None:
        live.update({"case": case["id"], "session": s, "since": t0})
    try:
        with s.lock:
            S.apply_ops(g, (probe.get("setup") or []) + (case.get("setup") or []))
        agent = s.get_agent(subject)
        if case["kind"] in ("offer", "message"):
            with s.lock:
                if not g.has_met(cp, subject):
                    g.meet(cp, subject)
                saved_current = g.s.current
                g.s.current = cp            # negotiations are opened on the opener's turn
                try:
                    give = case.get("give") if case["kind"] == "offer" else None
                    receive = case.get("receive") if case["kind"] == "offer" else None
                    if case["kind"] == "offer":
                        give, receive = give or [], receive or []
                    r = D.open_negotiation(g, cp, subject, case["message"], give, receive)
                finally:
                    g.s.current = saved_current
                rec["offer_text"] = None
                nid = r["negotiation_id"]
                prop = D.get_negotiation(g, nid)["proposal"]
                if prop:
                    rec["offer_text"] = (f"{g.player(cp).name} gives {_items_text(g, prop[str(cp)])}; "
                                         f"{g.player(subject).name} gives {_items_text(g, prop[str(subject)])}")
                    try:
                        # the subject's side too, so an impossible request is reported rather than "no response"
                        D.validate_items(g, subject, cp, prop[str(subject)], prop)
                    except ActionError as e:
                        rec["outcome"] = "invalid_case"
                        rec["error"] = f"The subject cannot give what the case asks: {e}"
                        raise _CaseInvalid()
            followups = list(case.get("followups") or [])
            for _round in range(g.rules.const["diplomacy"]["max_negotiation_exchanges"] + 1):
                if should_stop():
                    rec["outcome"] = "stopped"
                    break
                agent.respond_negotiation(s, subject, nid)
                with s.lock:
                    n = D.get_negotiation(g, nid)
                    last = n["history"][-1] if n["history"] else {}
                    if last.get("by") == subject:
                        if last.get("message"):
                            rec["subject_messages"].append(last["message"])
                        if last.get("action") == "counter" and last.get("proposal"):
                            prop = last["proposal"]
                            rec["counter_offers"].append({
                                "subject_gives": prop.get(str(subject), []), "subject_asks": prop.get(str(cp), []),
                                "text": f"{g.player(subject).name} gives {_items_text(g, prop.get(str(subject)))}; "
                                        f"{g.player(cp).name} gives {_items_text(g, prop.get(str(cp)))}"})
                    if n["status"] != "open":
                        rec["outcome"] = {"accepted": "accept"}.get(n["status"], n["status"])
                        if n["status"] == "rejected":
                            rec["outcome"] = "reject" if last.get("by") == subject else "rejected_by_script"
                        if n["status"] == "accepted" and last.get("by") != subject:
                            rec["outcome"] = "counter_accepted_by_script"
                        break
                    if n["awaiting"] == subject:
                        rec["outcome"] = "no_response"
                        break
                    # the subject countered or replied: answer from the script
                    rec["outcome"] = last.get("action") or "reply"
                    if followups:
                        f = followups.pop(0)
                        tools.execute(g, cp, "respond_negotiation", {"negotiation_id": nid, **f})
                        continue
                    policy = case.get("on_counter", "leave" if case["kind"] == "message" else "reject")
                    if policy == "leave":
                        break
                    action = "accept" if policy == "accept" and n["proposal"] and n["proposal_by"] == subject else "reject"
                    try:
                        tools.execute(g, cp, "respond_negotiation", {"negotiation_id": nid, "action": action})
                    except ActionError:
                        tools.execute(g, cp, "respond_negotiation", {"negotiation_id": nid, "action": "reject"})
                    if action == "accept":
                        rec["outcome"] = "counter_accepted_by_script"
                    break
            with s.lock:
                n = D.get_negotiation(g, nid)
                rec["negotiation"] = D.negotiation_view(g, n, cp)
                if n.get("deal_id") is not None:
                    deal = next((d for d in g.s.deals if d.get("id") == n["deal_id"]), None)
                    rec["deal"] = deal.get("summary") if deal else None
        elif case["kind"] == "turn":
            with s.lock:
                if g.s.current != subject:
                    g.s.current = subject
                    g.s.turn_started = False
                    g.begin_turn()
                s._track_turn()
            agent.play_turn(s, subject)
            with s.lock:
                ended = any(c["tool"] == "end_turn" and c["ok"] for c in calls)
                rec["end_reason"] = "end_turn" if ended or is_bot else \
                    ((s.metrics.current(subject) or {}).get("end_reason") or "no_end_turn")
            rec["outcome"] = rec["end_reason"]
        with s.lock:
            rec["thoughts"] = [{"kind": t.get("kind"), "text": t.get("text")} for t in g.s.thoughts[thought_mark:]
                               if t.get("player") == subject][-40:]
            rec["tool_calls"] = calls
    except _CaseInvalid:
        pass
    except Exception as e:
        rec["error"] = f"{type(e).__name__}: {e}"
        rec["trace"] = traceback.format_exc(limit=6)
        rec["outcome"] = rec["outcome"] or "error"
    finally:
        rec["tool_calls"] = calls
        rec["seconds"] = round(time.time() - t0, 1)
        usage = getattr(s.agents.get(subject), "usage_total", {}) or {}
        rec["tokens"] = {k: usage.get(k, 0) for k in ("input_tokens", "output_tokens", "reasoning_tokens",
                                                       "cache_read_input_tokens", "cache_creation_input_tokens") if k in usage}
        if save_path is not None:
            try:
                data = s.to_save()
                import gzip
                save_path.parent.mkdir(parents=True, exist_ok=True)
                with gzip.open(save_path, "wt", encoding="utf-8") as f:
                    json.dump(data, f)
                rec["save"] = str(save_path)
            except Exception:
                pass
        s.stop()
    rec["passed"] = _check(case, rec)
    return rec


def _check(case: dict, rec: dict) -> Optional[bool]:
    """Whether a case's result matches what it expected, or None if it expected nothing."""
    exp = case.get("expect")
    ok = None
    if exp:
        got = rec.get("outcome")
        norm = {"accept": "accept", "accepted": "accept", "reject": "reject", "rejected": "reject",
                "counter": "counter", "counter_accepted_by_script": "counter", "rejected_by_script": "counter",
                "reply": "reply", "expired": "counter"}
        ok = norm.get(got, got) == exp
    tools_used = {c["tool"] for c in rec.get("tool_calls", []) if c.get("ok") is not False}
    if case.get("expect_tools"):
        ok = (ok is not False) and all(t in tools_used for t in case["expect_tools"])
    if case.get("forbid_tools"):
        ok = (ok is not False) and not any(t in tools_used for t in case["forbid_tools"])
    return ok


# ----------------------------------------------------------------------------
# the runner (one run at a time: runs usually share a single GPU)
# ----------------------------------------------------------------------------
class ProbeRunner:
    """Runs probes in the background, one case at a time, and records the results."""
    def __init__(self, manager):
        self.manager = manager
        self.lock = threading.Lock()
        self.queue: list[str] = []
        self.live: dict = {}
        self._thread: Optional[threading.Thread] = None
        self._stop_run: set = set()
        RUNS.mkdir(parents=True, exist_ok=True)
        from .pool import queue as work_queue
        work_queue.register("probe", self.queue_items, self.running_items)
        for run in self.list_runs():      # runs interrupted by a server restart go back in the queue
            if run["status"] in ("queued", "running", "waiting (quiet hours)", "waiting (restricted hours)",
                                 "waiting (machine busy)"):
                self.queue.append(run["id"])
        self._kick()

    def _run_dir(self, rid: str) -> Path:
        """The directory for a run, rejecting an id that is not a plain slug."""
        if not re.fullmatch(r"[a-z0-9-]{4,40}", rid or ""):
            raise ProbeError("Invalid run id.")
        return RUNS / rid

    def _write(self, run: dict):
        """Write a run's metadata."""
        d = self._run_dir(run["id"])
        d.mkdir(parents=True, exist_ok=True)
        tmp = d / "run.json.tmp"
        tmp.write_text(json.dumps(run, indent=1), encoding="utf-8")
        _fs_replace(tmp, d / "run.json")

    def get_run(self, rid: str) -> dict:
        """One run's metadata."""
        p = self._run_dir(rid) / "run.json"
        if not p.exists():
            raise ProbeError(f"No run '{rid}'.")
        return json.loads(p.read_text(encoding="utf-8"))

    def results(self, rid: str) -> list[dict]:
        """The per-case results of a run."""
        p = self._run_dir(rid) / "results.jsonl"
        if not p.exists():
            return []
        out = []
        for line in p.read_text(encoding="utf-8").splitlines():
            try:
                out.append(json.loads(line))
            except ValueError:
                pass
        return out

    def list_runs(self) -> list[dict]:
        """Every run, newest first."""
        out = []
        for p in sorted(RUNS.glob("*/run.json"), key=lambda p: p.stat().st_mtime, reverse=True):
            try:
                out.append(json.loads(p.read_text(encoding="utf-8")))
            except (OSError, ValueError):
                pass
        return out

    def start(self, probe_id: str, llm: dict, repeats: Optional[int] = None, name: str = "",
              cases: Optional[list] = None) -> dict:
        """Start a probe run against a model."""
        probe = validate(load_probe(probe_id))
        n_rep = max(1, min(50, int(repeats or probe.get("repeats") or 1)))
        chosen = [c for c in probe["cases"] if not cases or c["id"] in cases]
        if not chosen:
            raise ProbeError("No cases selected.")
        rid = datetime.now().strftime("%Y%m%d-%H%M%S-") + secrets.token_hex(2)
        model = "scripted bot" if llm.get("provider") == "bot" else (llm.get("model") or llm.get("provider") or "model")
        if llm.get("server_id"):
            from . import servers
            from .pool import seats as pool_seats
            sv = servers.find(llm["server_id"]) or pool_seats.lookup(llm["server_id"])
            if sv is None:
                raise ProbeError(f"No server '{llm['server_id']}'.")
            model = f"{model} @ {sv['name']}"
        run = {"id": rid, "name": name or f"{probe['name']} · {model}", "probe": probe, "llm": {k: v for k, v in llm.items() if k != "api_key"},
               "repeats": n_rep, "jobs": [{"case": c["id"], "rep": r} for c in chosen for r in range(n_rep)],
               "status": "queued", "done": 0, "created": time.time(), "started": None, "finished": None,
               "summary": {}, "error": None}
        self._write(run)
        with self.lock:
            self.queue.append(rid)
        self._kick()
        return run

    def stop(self, rid: str) -> dict:
        """Ask a run to stop after the current case."""
        run = self.get_run(rid)
        with self.lock:
            if rid in self.queue and (run["status"] == "queued" or run["status"].startswith("waiting")):
                self.queue.remove(rid)
                run["status"] = "cancelled"
                self._write(run)
            else:
                self._stop_run.add(rid)
        return run

    def delete(self, rid: str):
        """Delete a run and its results."""
        import shutil
        run = self.get_run(rid)
        if run["status"] in ("queued", "running"):
            raise ProbeError("Stop the run first.")
        shutil.rmtree(self._run_dir(rid), ignore_errors=True)

    def _kick(self):
        """Make sure the worker thread is running."""
        if self._thread is None or not self._thread.is_alive():
            self._thread = threading.Thread(target=self._loop, daemon=True, name="probe-runner")
            self._thread.start()

    def _loop(self):
        """The worker thread: take the next queued run and execute it."""
        while True:
            queue = self._ordered_queue()
            # the first run whose machine is free, so one busy machine does not hold up every other run
            rid = next((r for r in queue if not self._machine_busy(r)), None)
            if rid is None and queue:
                time.sleep(15)
                continue
            with self.lock:
                if rid is not None:
                    if rid not in self.queue:
                        continue            # stopped meanwhile
                    self.queue.remove(rid)
            if rid is None:
                # nothing left: free the GPU of whatever this runner loaded
                if self._loaded_by_us:
                    from . import servers
                    sv = servers.find(self._loaded_by_us[0])
                    if sv:
                        servers.unload_models(sv, [self._loaded_by_us[1]])
                    self._loaded_by_us = None
                return
            try:
                self._run(rid)
            except Exception:
                try:
                    run = self.get_run(rid)
                    run["status"], run["error"] = "failed", traceback.format_exc(limit=6)
                    self._write(run)
                except Exception:
                    pass

    def _run(self, rid: str):
        """Execute one run: every case, in order, with the configured repeats."""
        from .engine import scenario as S
        run = self.get_run(rid)
        probe = run["probe"]
        scn = S.load_scenario(probe["scenario"])
        done = {(r["case"], r.get("rep", 0)) for r in self.results(rid)}
        run["status"], run["started"] = "running", run.get("started") or time.time()
        self._write(run)
        cases = {c["id"]: c for c in probe["cases"]}
        act = f"probe:{rid}"
        from . import usage
        from .pool import seats as pool_seats
        usage.tracker().activity(act, "probe", run["name"], parent={"kind": "probe", "id": probe["id"], "name": probe["name"]},
                                 ref={"probe_run": rid, "scenario": probe.get("scenario")},
                                 server_id=run["llm"].get("server_id"), model=run["llm"].get("model"))
        server_id = run["llm"].get("server_id")
        self._wait_free(rid, server_id)
        pool_seats.claim(server_id, f"probe run “{run['name']}”")
        try:
            self._run_cases(rid, run, probe, scn, done, cases, act)
        finally:
            pool_seats.release(server_id, f"probe run “{run['name']}”")

    def queue_items(self) -> list[dict]:
        """Queued runs, for the shared work queue (see citar.pool.queue)."""
        with self.lock:
            rids = list(self.queue)
        out = []
        for rid in rids:
            try:
                run = self.get_run(rid)
            except (ProbeError, OSError, ValueError):
                continue
            out.append({"kind": "probe", "id": rid, "group": rid, "run": run["name"], "label": run["name"],
                        "server_id": run["llm"].get("server_id"), "server": run["llm"].get("server"),
                        "created": run.get("created") or 0, "waiting": run.get("waiting")})
        return out

    def running_items(self) -> list[dict]:
        """The run using its machine now, for the queue page."""
        rid = (self.live or {}).get("run")
        if not rid:
            return []
        try:
            run = self.get_run(rid)
        except (ProbeError, OSError, ValueError):
            return []
        if run["status"] != "running":
            return []
        return [{"kind": "probe", "id": rid, "group": rid, "run": run["name"], "label": run["name"],
                 "server_id": run["llm"].get("server_id"), "server": run["llm"].get("server"),
                 "created": run.get("created") or 0, "waiting": None}]

    def _make_way(self, rid: str, run: dict) -> bool:
        """Between cases: if higher-priority work is waiting for this run's machine, put the run back in the queue
        (it keeps its rank and carries on from the next case). Returns True if it did."""
        from .pool import queue as work_queue
        first = work_queue.preempting(run["llm"].get("server_id"), "probe", rid, work_queue.priority("probe", rid))
        if first is None:
            return False
        run = self.get_run(rid)
        run["status"] = "queued"
        run["waiting"] = f"paused for {first['label']} (priority {first['priority']})"
        self._write(run)
        with self.lock:
            if rid not in self.queue:
                self.queue.append(rid)
        self.live = {}
        return True

    def _ordered_queue(self) -> list[str]:
        """The queued runs, highest priority first, then oldest."""
        from .pool import queue as work_queue
        items = self.queue_items()
        for it in items:
            it["priority"] = work_queue.priority("probe", it["group"])
        return [it["id"] for it in sorted(items, key=work_queue.rank)]

    def _machine_busy(self, rid: str) -> bool:
        """Whether a queued run cannot start yet - its machine is in use, or higher-priority work is waiting for
        it; if so, the run says what it is waiting for."""
        from .pool import seats as pool_seats
        from .pool import queue as work_queue
        try:
            run = self.get_run(rid)
        except ProbeError:
            return False
        server_id = run["llm"].get("server_id")
        held = pool_seats.occupied(server_id)
        note = f"waiting for {', '.join(held)}" if held else None
        if not held and server_id:
            first = work_queue.ahead_of(server_id, "probe", (-work_queue.priority("probe", rid), run.get("created") or 0))
            if first is not None:
                held = [first["label"]]
                note = f"waiting: {first['label']} goes first (priority {first['priority']})"
        status = "waiting (machine busy)" if held else "queued"
        if run.get("waiting") != note or run["status"] != status:
            run["waiting"], run["status"] = note, status
            self._write(run)
        return bool(held)

    def _wait_free(self, rid: str, server_id):
        """Wait while the run's machine is being used by a game, so the run does not fight it for the slot."""
        from .pool import seats as pool_seats
        waited = False
        while server_id and rid not in self._stop_run:
            held = pool_seats.occupied(server_id)
            if not held:
                break
            if not waited:
                waited = True
                run = self.get_run(rid)
                run["status"] = "waiting (machine busy)"
                run["waiting"] = f"waiting for {', '.join(held)}"
                self._write(run)
                self.live = {"run": rid, "case": f"(waiting: the machine is in use by {', '.join(held)})",
                             "since": time.time()}
            time.sleep(15)
        if waited:
            run = self.get_run(rid)
            run["status"], run["waiting"] = "running", None
            self._write(run)

    def _run_cases(self, rid, run, probe, scn, done, cases, act):
        """Every case of a run, in order, with the configured repeats."""
        from . import usage
        self._prepare_model(run)
        for job in run["jobs"]:
            if rid in self._stop_run:
                break
            key = (job["case"], job["rep"])
            if key in done:
                continue
            if self._make_way(rid, run):
                return
            if self._wait_quiet(rid, run["llm"]):
                self._prepare_model(run)
            self.live = {"run": rid, "case": job["case"], "rep": job["rep"], "since": time.time()}
            rec = run_case(self.manager, scn, probe, cases[job["case"]], run["llm"],
                           save_path=self._run_dir(rid) / f"{job['case']}-{job['rep']}.citar", live=self.live,
                           should_stop=lambda: rid in self._stop_run, usage_act=act)
            rec["rep"] = job["rep"]
            with open(self._run_dir(rid) / "results.jsonl", "a", encoding="utf-8") as f:
                f.write(json.dumps(rec, default=str) + "\n")
            done.add(key)
            run = self.get_run(rid)
            run["done"] = len(done)
            run["summary"] = summarize(self.results(rid))
            self._write(run)
        run = self.get_run(rid)
        stopped = rid in self._stop_run
        self._stop_run.discard(rid)
        run["status"] = "stopped" if stopped else "finished"
        run["finished"] = time.time()
        run["summary"] = summarize(self.results(rid))
        self._write(run)
        usage.tracker().update(act, status=run["status"], ended=run["finished"], cases_run=run.get("done"),
                               pass_rate=run["summary"].get("pass_rate"))
        self.live = {}

    # ---------------------------------------------------------------- models
    _loaded_by_us: Optional[tuple] = None     # (server id, model key) this runner loaded

    def _prepare_model(self, run: dict) -> bool:
        """Loads the run's model with its load profile on a server CITAR manages (LM Studio), swapping out a model this
        runner loaded earlier but never one the user loaded."""
        from . import servers
        llm = run["llm"]
        sv = servers.find(llm.get("server_id"))
        if sv is None or not sv["connection"].get("manage_loading"):
            return False
        model = llm.get("model")
        entry = servers.model_entry(sv, llm.get("model_id") or model)
        if self._loaded_by_us and self._loaded_by_us[1] != model:
            servers.unload_models(servers.find(self._loaded_by_us[0]) or sv, [self._loaded_by_us[1]])
            self._loaded_by_us = None
        self.live = {"run": run["id"], "case": "(loading the model)", "since": time.time()}
        try:
            if servers.ensure_model(sv, model, servers.profile(entry, llm.get("profile_id"))) > 0:
                self._loaded_by_us = (sv["id"], model)
        except Exception as e:      # still try: LM Studio may load it just in time
            run = self.get_run(run["id"])
            run["error"] = f"Could not load {model}: {e}"
            self._write(run)
        return True

    def _wait_quiet(self, rid: str, llm: dict) -> bool:
        """During the restricted hours of the run's server, unload what this runner loaded (if the server asks for
        it) and wait. Returns True if it waited (so the model must be loaded again)."""
        from . import servers
        waited = False
        while servers.restricted_now(llm.get("server_id")) and rid not in self._stop_run:
            if not waited:
                waited = True
                sv = servers.find(llm.get("server_id"))
                run = self.get_run(rid)
                run["status"] = "waiting (restricted hours)"
                self._write(run)
                if self._loaded_by_us and sv and sv["restricted_hours"].get("unload_models"):
                    servers.unload_models(servers.find(self._loaded_by_us[0]) or sv, [self._loaded_by_us[1]])
                    self._loaded_by_us = None
                self.live = {"run": rid, "case": f"(paused: {servers.server_name(llm.get('server_id'))} is in its restricted hours)",
                             "since": time.time()}
            time.sleep(30)
        if waited:
            run = self.get_run(rid)
            run["status"] = "running"
            self._write(run)
        return waited

    def status(self) -> dict:
        """What the runner is doing now."""
        live = dict(self.live)
        s = live.pop("session", None)
        if s is not None:
            try:
                with s.lock:
                    subj = next((seat.player for seat in s.seats if seat.type == "llm"), None)
                    live["thoughts"] = [{"kind": t.get("kind"), "text": (t.get("text") or "")[:1500]}
                                        for t in s.game.s.thoughts if t.get("player") == subj][-6:]
            except Exception:
                pass
        if live.get("since"):
            live["seconds"] = round(time.time() - live["since"], 1)
        return {"queue": list(self.queue), "live": live}


def summarize(results: list[dict]) -> dict:
    """Summarise a run: pass rates per case, and what the model did."""
    by_case: dict = {}
    for r in results:
        c = by_case.setdefault(r["case"], {"runs": 0, "outcomes": {}, "passed": 0, "checked": 0, "seconds": 0.0})
        c["runs"] += 1
        c["outcomes"][r.get("outcome") or "?"] = c["outcomes"].get(r.get("outcome") or "?", 0) + 1
        c["seconds"] += r.get("seconds") or 0
        if r.get("passed") is not None:
            c["checked"] += 1
            c["passed"] += 1 if r["passed"] else 0
    for c in by_case.values():
        c["seconds"] = round(c["seconds"] / max(1, c["runs"]), 1)
    checked = sum(c["checked"] for c in by_case.values())
    return {"cases": by_case, "runs": len(results),
            "pass_rate": round(sum(c["passed"] for c in by_case.values()) / checked, 3) if checked else None}
