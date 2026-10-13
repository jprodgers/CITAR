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

A case's model calls go through the prompt scheduler (design/phase3-features.md P3.3.3, P3.3.5): each answer of an
offer or message case is a ``probe`` ticket (a pooled prompt, admitted for ``pooled`` when it is made), and a turn case
holds a ``probe_case`` lease for the case only, lending nothing. So a run holds its machine only while a turn case
plays, and its offer and message cases run beside a lobby game on the slots its seat lends between its turns; an answer
the machine cuts off (the seat needing its slot back, a benchmark taking the machine) is asked again, never scored as
the model's. A run waits at the job level (``pool.queue``, saying what for) for a benchmark that holds its machine
(before every case, and when one takes the machine mid-answer: an answer's ticket does not wait for benchmarks), for
work that ranks ahead of it there, and before a turn case for the leases that fill its machine's slots.
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

from .fsutil import newest_first as _fs_newest, read_text as _fs_read, replace as _fs_replace
from typing import Optional

from . import engine_api, paths
from .engine_api import ActionError

BASE = paths.saves_path("probes")
RUNS = BASE / "runs"
CASE_KINDS = ("offer", "message", "turn")
#: How long one answer of an offer or message case may wait to start (its ticket's deadline), the helper's coming back
#: after a temporary refusal (offline, quiet hours) included: two of a lobby seat's longest turns, on whose idle slot a
#: probe's prompt runs.
PROBE_START_SECONDS = 3600.0
#: The longest pause between two tries of a temporarily refused answer.
PROBE_RETRY_SECONDS = 15.0
#: How an answer's ticket can end by the machine's doing rather than the model's (the scheduler's codes): the lobby seat
#: whose idle slot it borrowed needed it back (``yielded``), the borrowed time ran out (``borrow_cap``), a benchmark took
#: the machine (``benchmark``). The answer is asked again on a fresh ticket while the negotiation still waits for it, at
#: most ``PROBE_ANSWER_TRIES`` times in all; a case whose answer never ran whole records why and is not scored.
INTERRUPTIONS = ("yielded", "borrow_cap", "benchmark")
PROBE_ANSWER_TRIES = 3
#: How the scheduler ends a ticket that did not run its answer: the case is not the model's to score.
NOT_RUN = ("cancelled", "expired", "disabled") + INTERRUPTIONS
# What the scripted counterparty says with each answer: every negotiation entry carries a message
SCRIPT_LINES = {"accept": "Agreed.", "reject": "No deal.", "counter": "How about this instead?", "reply": "Noted."}


class ProbeError(ValueError):
    """A probe that cannot be run or saved as written."""
    pass


class _CaseInvalid(Exception):
    """One case is malformed; the rest of the probe can still run."""
    pass


# ----------------------------------------------------------------------------
# probe files
# ----------------------------------------------------------------------------
def case_purposes(probe: dict, cases=None) -> tuple:
    """What a run of ``probe`` (its ``cases`` by id, every case when none are named) is admitted for (P3.3.7):
    ``probe`` for its turn cases, which hold a lease each, ``pooled`` for its offer and message cases, whose answers
    are pooled prompts."""
    chosen = [c for c in probe.get("cases") or [] if isinstance(c, dict) and (not cases or c.get("id") in cases)]
    kinds = {c.get("kind") or "offer" for c in chosen}
    out = (("probe",) if "turn" in kinds else ()) + (("pooled",) if kinds & {"offer", "message"} else ())
    return out or ("probe",)


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
    if not isinstance(data, dict):
        raise ProbeError("A probe must be a JSON object.")
    if not data.get("scenario"):
        raise ProbeError("A probe needs a scenario id.")
    try:
        scn = engine_api.load_scenario(data["scenario"])
    except ActionError as e:
        raise ProbeError(str(e))
    majors = [p["id"] for p in engine_api.state_summary(scn["state"])["majors"]]
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
    """Every saved probe, in summary, with what a run of all its cases is admitted for (``purposes``,
    ``case_purposes``), so the run dialog can show a machine's refusal before the run is asked."""
    BASE.mkdir(parents=True, exist_ok=True)
    out = []
    for p, st in _fs_newest(BASE.glob("*.json")):
        try:
            d = json.loads(_fs_read(p))
            out.append({"id": d.get("id"), "name": d.get("name"), "description": d.get("description", ""),
                        "scenario": d.get("scenario"), "cases": len(d.get("cases") or []), "repeats": d.get("repeats", 1),
                        "subject": d.get("subject"), "counterparty": d.get("counterparty"),
                        "purposes": list(case_purposes(d)),
                        "modified": datetime.fromtimestamp(st.st_mtime).isoformat(timespec="seconds")})
        except (OSError, ValueError):
            continue
    return out


def load_probe(pid: str) -> dict:
    """Load a probe from disk."""
    p = _path(pid)
    if not p.exists():
        raise ProbeError(f"No probe '{pid}'.")
    return json.loads(_fs_read(p))


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
    try:
        return g.describe_items(items or [])
    except Exception:
        return json.dumps(items)


def run_case(manager, scn: dict, probe: dict, case: dict, llm_cfg: dict, save_path: Optional[Path] = None,
             live: Optional[dict] = None, should_stop=lambda: False, usage_act: Optional[str] = None,
             owner_id: Optional[str] = None, run_id: Optional[str] = None, run_name: Optional[str] = None,
             on_ticket=None, wait_benchmark=None) -> dict:
    """Plays one case on a fresh copy of the scenario and returns its record. ``owner_id`` is the probe run's owner,
    the case game's owner (``GameSession.owner_id``) and the consumer of its prompts. A model subject's answers in an
    offer or message case are ``probe`` tickets, one per answer (``_answer``), and its turn in a turn case plays on a
    ``probe_case`` lease the case's seat holds for the case only (released when the case's game stops); both name the
    run (``run_id``, ``run_name``) on the Queue page. ``on_ticket(ticket)`` is told of each answer's ticket while it
    is asked, and ``on_ticket(None)`` after (the runner cancels it when the run is stopped); ``wait_benchmark()`` waits
    while a benchmark holds the machine (the runner's job-level wait, which says so) and returns whether it waited.
    An answer that was not asked or did not run whole (refused, cut off, stopped) is the case's ``error``: its outcome
    is ``error`` (``stopped`` for a stopped run) and it is not scored (``passed`` None)."""
    subject, cp = probe["subject"], probe.get("counterparty")
    seats = [None] * len(scn.get("seats") or [])
    seats = [{} for _ in range(max(len(seats), subject + 1, (cp or 0) + 1))]
    is_bot = llm_cfg.get("provider") == "bot"       # the scripted bot as the subject: the baseline
    seats[subject] = {"type": "bot", "bot": {"aggression": float(llm_cfg.get("aggression", 0.4)),
                                             "profile": llm_cfg.get("profile")}} if is_bot \
        else {"type": "llm", "llm": llm_cfg}
    if cp is not None:
        seats[cp] = {"type": "script"}
    s = manager.create_from_scenario(scn, seats, name=f"probe {probe['id']}/{case['id']}", register=False, start=False)
    s.owner_id = owner_id
    s.lease_kind = "probe_case"         # a turn case's model calls hold the machine for the case only, lending nothing
    if run_id:
        s.lease_ref, s.lease_label = ("probe", run_id), f"probe run “{run_name or run_id}”"
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
    thought_mark = g.thought_count()
    calls: list = []
    not_run = False                     # an answer the machine did not run: the case is not the model's to score
    orig_call = s.call_tool

    def recording_call(pid, name, args=None, *a, **kw):
        """Wrap the model's tool calls so each one is recorded as it happens."""
        res = orig_call(pid, name, args, *a, **kw)
        if pid == subject and len(calls) < 400:
            calls.append({"tool": name, "args": args, "ok": res.get("ok"),
                          "error": (res.get("error") or "")[:300] or None})
        return res
    s.call_tool = recording_call        # every call the model makes, queries and failures included
    if is_bot:
        # The bot's actions never pass through call_tool: its turn is a drive and its answers go through
        # EngineGame.answer. Each drive's counts for the subject are recorded as its calls instead.
        orig_drive = s.drive_bots

        def recording_drive(pid):
            """Record the subject's actions in each drive of its turn."""
            stop = orig_drive(pid)
            _record_bot_actions(calls, (stop.get("actions") or {}).get(subject))
            return stop
        s.drive_bots = recording_drive
    if live is not None:
        live.update({"case": case["id"], "session": s, "since": t0})
    try:
        with s.lock:
            g.apply_ops((probe.get("setup") or []) + (case.get("setup") or []))
        # a model's answers are pooled prompts with no lease (P3.3.3), its turn a lease's: the bot asks no machine
        prompts = case["kind"] in ("offer", "message") and not is_bot
        agent = s.prompt_agent(subject) if prompts else s.get_agent(subject)
        if case["kind"] in ("offer", "message"):
            with s.lock:
                if not g.has_met(cp, subject):
                    g.meet(cp, subject)
                give = case.get("give") if case["kind"] == "offer" else None
                receive = case.get("receive") if case["kind"] == "offer" else None
                if case["kind"] == "offer":
                    give, receive = give or [], receive or []
                r = g.open_negotiation_as(cp, subject, case["message"], give, receive)
                rec["offer_text"] = None
                nid = r["negotiation_id"]
                prop = g.negotiation(nid)["proposal"]
                if prop:
                    rec["offer_text"] = (f"{g.player_name(cp)} gives {_items_text(g, prop[str(cp)])}; "
                                         f"{g.player_name(subject)} gives {_items_text(g, prop[str(subject)])}")
                    try:
                        # the subject's side too, so an impossible request is reported rather than "no response"
                        g.validate_items(subject, cp, prop[str(subject)], prop)
                    except ActionError as e:
                        rec["outcome"] = "invalid_case"
                        rec["error"] = f"The subject cannot give what the case asks: {e}"
                        raise _CaseInvalid()
            followups = list(case.get("followups") or [])
            for _round in range(g.max_chat_messages() + 1):
                if should_stop():
                    rec["outcome"] = "stopped"
                    break
                if prompts:
                    refused = _answer(s, agent, subject, nid, on_ticket=on_ticket, should_stop=should_stop,
                                      usage_act=usage_act, owner_id=owner_id, run_id=run_id, run_name=run_name,
                                      wait_benchmark=wait_benchmark)
                    if refused:
                        rec["error"], not_run = refused, True
                        rec["outcome"] = "stopped" if should_stop() else "error"
                        break
                    answer = None
                else:
                    answer = agent.respond_negotiation(s, subject, nid)
                if is_bot and isinstance(answer, dict):
                    _record_bot_actions(calls, answer.get("actions"))
                with s.lock:
                    n = g.negotiation(nid)
                    last = n["history"][-1] if n["history"] else {}
                    if last.get("by") == subject:
                        if last.get("message"):
                            rec["subject_messages"].append(last["message"])
                        if last.get("action") == "counter" and last.get("proposal"):
                            prop = last["proposal"]
                            rec["counter_offers"].append({
                                "subject_gives": prop.get(str(subject), []), "subject_asks": prop.get(str(cp), []),
                                "text": f"{g.player_name(subject)} gives {_items_text(g, prop.get(str(subject)))}; "
                                        f"{g.player_name(cp)} gives {_items_text(g, prop.get(str(cp)))}"})
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
                        # every negotiation entry carries a message; a case written without one gets a plain line
                        g.execute(cp, "respond_negotiation",
                                  {"negotiation_id": nid, "message": SCRIPT_LINES.get(f.get("action"), "Noted."), **f})
                        continue
                    policy = case.get("on_counter", "leave" if case["kind"] == "message" else "reject")
                    if policy == "leave":
                        break
                    action = "accept" if policy == "accept" and n["proposal"] and n["proposal_by"] == subject else "reject"
                    try:
                        g.execute(cp, "respond_negotiation", {"negotiation_id": nid, "action": action,
                                                              "message": SCRIPT_LINES[action]})
                    except ActionError:
                        g.execute(cp, "respond_negotiation", {"negotiation_id": nid, "action": "reject",
                                                              "message": SCRIPT_LINES["reject"]})
                    if action == "accept":
                        rec["outcome"] = "counter_accepted_by_script"
                    break
            with s.lock:
                n = g.negotiation(nid)
                rec["negotiation"] = g.negotiation_view(nid, cp)
                if n.get("deal_id") is not None:
                    deal = g.deal(n["deal_id"])
                    rec["deal"] = deal.get("summary") if deal else None
        elif case["kind"] == "turn":
            with s.lock:
                g.force_turn(subject)
                s._track_turn()
                began = (g.turn, g.current)
            if hasattr(agent, "wait_cap"):
                # Nobody answers a chat in a turn case (the other seats are scripts, and the case's session runs no
                # chat clock): a chat the subject opens is closed at once rather than at its safety cap, and its turn
                # goes on to its end.
                agent.wait_cap = 0.0
            agent.play_turn(s, subject)
            with s.lock:
                if is_bot:
                    # the drive ends the bot's turn itself, with no end_turn call: the turn moving on is its end_turn
                    ended = not s.crashed and ((g.turn, g.current) != began or g.phase != "playing")
                    if ended:
                        calls.append({"tool": "end_turn", "args": None, "ok": True, "error": None, "count": 1})
                else:
                    ended = any(c["tool"] == "end_turn" and c["ok"] for c in calls)
                rec["end_reason"] = "end_turn" if ended else \
                    ((s.metrics.current(subject) or {}).get("end_reason") or "no_end_turn")
            rec["outcome"] = rec["end_reason"]
        with s.lock:
            rec["thoughts"] = [{"kind": t.get("kind"), "text": t.get("text")}
                               for t in g.thoughts(subject, since=thought_mark)][-40:]
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
                s.save_copy(save_path)      # beside it, its own journal: the case's game stands apart from the run's
                rec["save"] = str(save_path)
            except Exception:
                pass
        s.stop()
    if s.crashed:
        # the engine stopped mid-case (GameSession._crashed): whatever the subject did before it, the case did not run
        rec["outcome"] = "error"
        rec["error"] = f"The engine stopped on turn {s.crashed['turn']}: {str(s.crashed['message'])[:300]}"
        rec["passed"] = False
        return rec
    rec["passed"] = None if not_run else _check(case, rec)
    return rec


def _answer(s, agent, subject: int, nid: int, *, on_ticket, should_stop, usage_act: Optional[str],
            owner_id: Optional[str], run_id: Optional[str], run_name: Optional[str],
            wait_benchmark=None) -> Optional[str]:
    """One answer of a model subject to the case's negotiation, as a ``probe`` ticket on its machine (P3.3.3): admitted
    for ``pooled`` when it is made (``pool.seats.ticket``, with ``run_probes``), following the run's queue priority, at
    most ``PROBE_START_SECONDS`` to start (``_probe_ticket``). Stopping the run cancels the ticket (``on_ticket``; a stop
    that lands while the ticket is made is noticed here, since the runner found nothing to cancel then). An answer the
    machine cut off (``INTERRUPTIONS``: the lobby seat whose idle slot it borrowed took it back, its borrowed time ran
    out, a benchmark took the machine) is asked again on a fresh ticket while the negotiation still waits for it, up to
    ``PROBE_ANSWER_TRIES`` times: contention is not the model's failure. Returns why the answer was not asked or did not
    run whole (a refusal, an expiry, a cancel, the tries spent), else None: what the model did is the case's. A subject
    with no machine to play on (``NoMachineAgent``) answers nothing, as before."""
    if not hasattr(agent, "binding"):
        agent.respond_negotiation(s, subject, nid)
        return None
    tries = 0
    while True:
        ticket, why = _probe_ticket(s, agent, should_stop=should_stop, usage_act=usage_act, owner_id=owner_id,
                                    run_id=run_id, run_name=run_name, wait_benchmark=wait_benchmark)
        if ticket is None:
            return why
        if on_ticket is not None:
            on_ticket(ticket)
        if should_stop():
            ticket.cancel("stopped")
        agent.binding = ticket
        try:
            agent.respond_negotiation(s, subject, nid)
        finally:
            agent.binding = None
            if on_ticket is not None:
                on_ticket(None)
            ticket.close()
        code = ticket.error.code if ticket.error is not None else None
        if code not in NOT_RUN or not _awaits(s, nid, subject):
            return None                 # the model answered, or chose not to: that is the case's result
        if code in INTERRUPTIONS and not should_stop():
            tries += 1
            if tries < PROBE_ANSWER_TRIES:
                continue
            return f"The answer was cut off {tries} times by the machine's other work: {ticket.reason}"
        return f"The prompt did not run: {ticket.reason}"


def _probe_ticket(s, agent, *, should_stop, usage_act: Optional[str], owner_id: Optional[str],
                  run_id: Optional[str], run_name: Optional[str], wait_benchmark=None) -> tuple:
    """The ticket one answer is asked on (``_answer``): (the ticket, None) once one is queued, else (None, why not).
    A refusal that says "not yet" (the helper offline, the machine's quiet hours, a grant's window) is tried again
    within ``PROBE_START_SECONDS``. The ticket does not wait for benchmarks: one refused because a benchmark holds the
    machine waits for it at the job level (``wait_benchmark``: the run says what it waits for, as long as the benchmark
    lasts, and the answer's window starts again after), rather than as a prompt that waits silently for an hour and
    expires; without the runner's wait it is tried again within the window."""
    from .pool import queue as work_queue, seats as pool_seats
    cfg = getattr(agent, "cfg", None) or {}
    sid = cfg.get("server_id") or None
    deadline = time.perf_counter() + PROBE_START_SECONDS
    while True:
        if should_stop():
            return None, "The prompt was not asked: the run was stopped."
        ticket = pool_seats.ticket(
            sid, cfg.get("model") or "", cls="probe", consumer=owner_id, capability="run_probes",
            activity=usage_act, label=f"probe run “{run_name or run_id or s.name}”",
            ref=("probe", run_id) if run_id else None,
            priority=work_queue.priority("probe", run_id) if run_id else 0,
            start_within=max(1.0, deadline - time.perf_counter()),
            steps=getattr(agent, "max_negotiation_calls", 20), wait_for_benchmark=False)
        if ticket.state == "queued":
            return ticket, None
        error = ticket.error
        benchmark = error is not None and error.code == "benchmark"
        if benchmark and wait_benchmark is not None and wait_benchmark():
            deadline = max(deadline, time.perf_counter() + PROBE_START_SECONDS)
            continue
        clears = (error.retry_at - datetime.now()).total_seconds() if error is not None and error.retry_at else None
        now = time.perf_counter()
        if error is None or not (error.temporary or benchmark) or should_stop() or now >= deadline or (
                clears is not None and now + clears > deadline):
            return None, f"The prompt was not asked: {ticket.reason}"
        # a benchmark's last request still draining (the runner saw no benchmark to wait for) clears within seconds
        wake = now + (1.0 if benchmark and wait_benchmark is not None else PROBE_RETRY_SECONDS)
        if clears is not None:
            wake = min(wake, now + max(1.0, clears))
        while time.perf_counter() < min(wake, deadline) and not should_stop():
            time.sleep(0.25)


def _awaits(s, nid: int, subject: int) -> bool:
    """Whether the case's negotiation still waits for the subject's answer."""
    with s.lock:
        n = s.game.negotiation_head(nid)
        return n["status"] == "open" and n["awaiting"] == subject


def _record_bot_actions(calls: list, counts) -> None:
    """Add the bot subject's actions from one drive or one answer (``{tool: [taken, refused]}``) to a case's calls:
    one entry per tool taken (``ok``, with its ``count``) and one per tool refused."""
    for tool, pair in (counts or {}).items():
        taken, refused = (list(pair) + [0, 0])[:2]
        for ok, n in ((True, taken), (False, refused)):
            if n and len(calls) < 400:
                calls.append({"tool": tool, "args": None, "ok": ok, "error": None if ok else "refused by the engine",
                              "count": int(n)})


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


def _set_waiting(run: dict, note: Optional[dict]) -> None:
    """What a queued run waits for (a ``pool.seats.note``, or None): the note, which the run's route tells each viewer
    with the names they may see (``work_access.with_waiting``), and its text naming no holder."""
    from .pool import seats as pool_seats
    run["waiting_note"], run["waiting"] = note, pool_seats.note_text(note)


# ----------------------------------------------------------------------------
# the runner (one run at a time: runs usually share a single GPU)
# ----------------------------------------------------------------------------
class ProbeRunner:
    """Runs probes in the background and records the results.

    Each model machine gets its own worker thread, one run at a time on it, so a run waiting for its machine
    (a game using it, its quiet hours, higher-priority work) never holds up runs for other machines. A
    dispatcher thread starts the best-ranked queued run for every machine that is free.
    """
    POLL_SECONDS = 5.0

    def __init__(self, manager):
        self.manager = manager
        self.lock = threading.Lock()
        self.queue: list[str] = []
        self.lives: dict = {}                       # run id -> what it is doing now (shown on the Probes page)
        self.asking: dict = {}                      # run id -> the ticket its case's answer is asked on now
        self._workers: dict = {}                    # machine -> the thread running a run on it
        self._loaded: dict = {}                     # server id -> model this runner loaded there
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
        return json.loads(_fs_read(p))

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
        for p, _ in _fs_newest(RUNS.glob("*/run.json")):
            try:
                out.append(json.loads(_fs_read(p)))
            except (OSError, ValueError):
                pass
        return out

    def start(self, probe_id: str, llm: dict, repeats: Optional[int] = None, name: str = "",
              cases: Optional[list] = None, *, owner_id: Optional[str] = None, session=None) -> dict:
        """Start a probe run against a model. ``owner_id`` is the account starting it (design/phase3-features.md
        P3.2.4): the run is theirs, its file records them, and the games its cases play are theirs. With ``session``
        (a request's) the run's row in ``owned_items`` is written and committed there before the run is queued; a run
        started without one (a script, a test) has no row until adoption gives it one, to the owner its file
        records."""
        probe = validate(load_probe(probe_id))
        n_rep = max(1, min(50, int(repeats or probe.get("repeats") or 1)))
        chosen = [c for c in probe["cases"] if not cases or c["id"] in cases]
        if not chosen:
            raise ProbeError("No cases selected.")
        rid = datetime.now().strftime("%Y%m%d-%H%M%S-") + secrets.token_hex(2)
        model = f"scripted bot ({llm.get('profile') or 'standard'})" if llm.get("provider") == "bot" else (llm.get("model") or llm.get("provider") or "model")
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
               "summary": {}, "error": None, "owner_id": owner_id}
        if session is not None:
            from . import items
            items.register(session, "probe_run", rid, owner_id, run["name"], path=self._run_dir(rid))
            session.commit()
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
        ticket = self.asking.get(rid)
        if ticket is not None:
            ticket.cancel("stopped")        # an answer waiting for its machine stops waiting
        return run

    def delete(self, rid: str):
        """Delete a run and its results."""
        import shutil
        run = self.get_run(rid)
        if run["status"] in ("queued", "running"):
            raise ProbeError("Stop the run first.")
        shutil.rmtree(self._run_dir(rid), ignore_errors=True)

    def _kick(self):
        """Make sure the dispatcher thread is running."""
        if self._thread is None or not self._thread.is_alive():
            self._thread = threading.Thread(target=self._loop, daemon=True, name="probe-runner")
            self._thread.start()

    @property
    def live(self) -> dict:
        """What the most recently started run is doing (the Probes page's live panel)."""
        lives = [v for v in list(self.lives.values()) if v]
        return max(lives, key=lambda v: v.get("started", 0)) if lives else {}

    def _set_live(self, rid: str, value: Optional[dict]):
        """Record what a run is doing now (None when it is not running)."""
        if value is None:
            self.lives.pop(rid, None)
        else:
            value.setdefault("started", (self.lives.get(rid) or {}).get("started") or time.time())
            self.lives[rid] = value

    def _machine_of(self, rid: str) -> str:
        """The machine a run plays on ("" when it names none)."""
        try:
            return self.get_run(rid)["llm"].get("server_id") or ""
        except (ProbeError, OSError, ValueError):
            return ""

    def _loop(self):
        """The dispatcher: start the best-ranked queued run on every machine that is free, each on its own thread."""
        while True:
            with self.lock:
                active = {m for m, t in self._workers.items() if t.is_alive()}
            for rid in self._ordered_queue():
                machine = self._machine_of(rid)
                if machine in active or self._machine_busy(rid):
                    continue
                with self.lock:
                    if rid not in self.queue:
                        continue            # stopped meanwhile
                    self.queue.remove(rid)
                    worker = threading.Thread(target=self._work, args=(rid,), daemon=True, name=f"probe-{rid}")
                    self._workers[machine] = worker
                active.add(machine)
                worker.start()
            with self.lock:
                idle = not self.queue and not any(t.is_alive() for t in self._workers.values())
                if idle:
                    self._thread = None
            if idle:
                self._unload_all()          # nothing left: free the GPUs of whatever this runner loaded
                return
            time.sleep(self.POLL_SECONDS)

    def _work(self, rid: str):
        """One machine's worker: execute a run, recording a failure instead of losing it."""
        try:
            self._run(rid)
        except Exception:
            try:
                run = self.get_run(rid)
                run["status"], run["error"] = "failed", traceback.format_exc(limit=6)
                self._write(run)
            except Exception:
                pass
        finally:
            self._set_live(rid, None)

    def _unload_all(self):
        """Unload every model this runner loaded."""
        from . import servers
        for sid, model in list(self._loaded.items()):
            sv = servers.find(sid)
            if sv:
                try:
                    servers.unload_models(sv, [model])
                except Exception:
                    pass
            self._loaded.pop(sid, None)

    def _run(self, rid: str):
        """Execute one run: every case, in order, with the configured repeats."""
        run = self.get_run(rid)
        probe = run["probe"]
        scn = engine_api.load_scenario(probe["scenario"])
        done = {(r["case"], r.get("rep", 0)) for r in self.results(rid)}
        run["status"], run["started"] = "running", run.get("started") or time.time()
        self._write(run)
        cases = {c["id"]: c for c in probe["cases"]}
        act = f"probe:{rid}"
        from . import usage
        usage.tracker().activity(act, "probe", run["name"], parent={"kind": "probe", "id": probe["id"], "name": probe["name"]},
                                 ref={"probe_run": rid, "scenario": probe.get("scenario")},
                                 server_id=run["llm"].get("server_id"), model=run["llm"].get("model"))
        self._run_cases(rid, run, probe, scn, done, cases, act)

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
                        "created": run.get("created") or 0, "waiting": run.get("waiting"),
                        "waiting_note": run.get("waiting_note")})
        return out

    def running_items(self) -> list[dict]:
        """The runs using their machines now, for the queue page."""
        out = []
        for rid in list(self.lives):
            try:
                run = self.get_run(rid)
            except (ProbeError, OSError, ValueError):
                continue
            if run["status"] != "running":
                continue
            out.append({"kind": "probe", "id": rid, "group": rid, "run": run["name"], "label": run["name"],
                        "server_id": run["llm"].get("server_id"), "server": run["llm"].get("server"),
                        "created": run.get("created") or 0, "waiting": None})
        return out

    def _make_way(self, rid: str, run: dict) -> bool:
        """Between cases: if higher-priority work is waiting for this run's machine, put the run back in the queue
        (it keeps its rank and carries on from the next case). Returns True if it did."""
        from .pool import queue as work_queue, seats as pool_seats
        first = work_queue.preempting(run["llm"].get("server_id"), "probe", rid, work_queue.priority("probe", rid))
        if first is None:
            return False
        run = self.get_run(rid)
        run["status"] = "queued"
        _set_waiting(run, pool_seats.note("paused for ", [pool_seats.queue_holder(first)],
                                          f" (priority {first['priority']})"))
        self._write(run)
        with self.lock:
            if rid not in self.queue:
                self.queue.append(rid)
        self._set_live(rid, None)
        return True

    def _ordered_queue(self) -> list[str]:
        """The queued runs, highest priority first, then oldest."""
        from .pool import queue as work_queue
        items = self.queue_items()
        for it in items:
            it["priority"] = work_queue.priority("probe", it["group"])
        return [it["id"] for it in sorted(items, key=work_queue.rank)]

    @staticmethod
    def _benchmarks(server_id) -> list:
        """The benchmark runs holding a machine (a live benchmark lease), each once: a probe run waits for them before
        every case (no pooled prompt starts beside a benchmark, and a turn case's lease would share the machine with
        its steps). Lobby games and pooled prompts do not hold an offer or message case up: its prompts run on the
        slots a lobby seat lends, as the scheduler's rules allow (P3.3.3, P3.3.5)."""
        from .pool import seats as pool_seats
        return [h for h in pool_seats.holders(server_id) if h.get("kind") == "benchmark"]

    @staticmethod
    def _slots_held(server_id) -> list:
        """What fills a machine's slots with leases (a lobby game's seat, another run's turn case), each once, or
        nothing while a slot is left: a turn case's own lease, which never lends, waits for one, as a benchmark job
        does (the job-level rule: fewer leases than the machine's slots, P3.3.5). A helper machine whose helper is
        offline is taken at its row's number."""
        from . import servers
        from .pool import scheduler as prompt_scheduler, seats as pool_seats
        occs = pool_seats.occupants(server_id)
        if not occs:
            return []
        machine = prompt_scheduler.get().machine(server_id)
        slots = machine.capacity() if machine is not None else None
        if slots is None:
            slots = int((servers.machine_settings(server_id) or {}).get("slots") or 1)
        return pool_seats.distinct(occs) if len(occs) >= max(1, slots) else []

    def _machine_busy(self, rid: str) -> bool:
        """Whether a queued run cannot start yet - a benchmark holds its machine, or higher-priority work is waiting
        for it; if so, the run says what it is waiting for."""
        from .pool import seats as pool_seats
        from .pool import queue as work_queue
        try:
            run = self.get_run(rid)
        except ProbeError:
            return False
        server_id = run["llm"].get("server_id")
        held = self._benchmarks(server_id)
        note = pool_seats.note("waiting for ", held) if held else None
        if not held and server_id:
            first = work_queue.ahead_of(server_id, "probe", (-work_queue.priority("probe", rid), run.get("created") or 0))
            if first is not None:
                held = [pool_seats.queue_holder(first)]
                note = pool_seats.note("waiting: ", held, f" goes first (priority {first['priority']})")
        status = "waiting (machine busy)" if held else "queued"
        if run.get("waiting_note") != note or run.get("waiting") != pool_seats.note_text(note) \
                or run["status"] != status:
            _set_waiting(run, note)
            run["status"] = status
            self._write(run)
        return bool(held)

    #: How often a run waiting for its machine looks again (it notices a stop at once).
    BENCHMARK_POLL_SECONDS = 15.0

    def _wait_benchmark(self, rid: str, server_id) -> bool:
        """Wait while a benchmark holds the run's machine: before every case, and when one took the machine from an
        answer (``run_case``'s ``wait_benchmark``). Returns whether it waited."""
        return self._wait_machine(rid, server_id, full=False)

    def _wait_machine(self, rid: str, server_id, *, full: bool) -> bool:
        """Wait, saying so, while a benchmark holds the run's machine (``_benchmarks``) and, before a turn case
        (``full``: a lease of its own that never lends), while leases fill its slots (``_slots_held``), so the case
        never plays its turn beside a benchmark's steps or step by step against a lobby game's seat (P3.3.5). The run's
        status and waiting note say what it waits for; the live panel's case is put back after. Returns whether it
        waited; a stopped run stops waiting at once."""
        from .pool import seats as pool_seats
        waited, before = False, self.lives.get(rid)
        while server_id and rid not in self._stop_run:
            held = self._benchmarks(server_id) or (self._slots_held(server_id) if full else [])
            if not held:
                break
            note = pool_seats.note("waiting for ", held)
            run = self.get_run(rid)
            if not waited or run.get("waiting_note") != note:
                run["status"] = "waiting (machine busy)"
                _set_waiting(run, note)
                self._write(run)
            if not waited:
                # the live panel is shown to whoever may see the run: what holds the machine is the run's waiting
                # note, which its route tells each viewer as they may see it
                self._set_live(rid, {"run": rid, "case": "(waiting: the machine is in use)", "since": time.time()})
            waited = True
            wake = time.perf_counter() + self.BENCHMARK_POLL_SECONDS
            while time.perf_counter() < wake and rid not in self._stop_run:
                time.sleep(0.25)
        if waited:
            run = self.get_run(rid)
            run["status"] = "running"
            _set_waiting(run, None)
            self._write(run)
            if before is not None:
                self.lives[rid] = before
        return waited

    def _run_cases(self, rid, run, probe, scn, done, cases, act):
        """Every case of a run, in order, with the configured repeats, each once no benchmark holds the machine
        (``_wait_machine``): a turn case also once a slot is free of leases, then on a lease for the case; an offer or
        message case on tickets."""
        from . import usage
        self._prepare_model(run)
        server_id = run["llm"].get("server_id")
        model_subject = run["llm"].get("provider") != "bot"
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
            if model_subject:
                self._wait_machine(rid, server_id, full=cases[job["case"]].get("kind") == "turn")
                if rid in self._stop_run:
                    break
            self._set_live(rid, {"run": rid, "case": job["case"], "rep": job["rep"], "since": time.time()})
            from . import ledger
            # the case's own CPU on this thread is the run's (P3.4.3); its game's drives are units of their own
            with ledger.cpu_unit(act):
                rec = run_case(self.manager, scn, probe, cases[job["case"]], run["llm"],
                               save_path=self._run_dir(rid) / f"{job['case']}-{job['rep']}.citar",
                               live=self.lives[rid], should_stop=lambda: rid in self._stop_run, usage_act=act,
                               owner_id=run.get("owner_id"), run_id=rid, run_name=run.get("name"),
                               on_ticket=lambda t: self.asking.__setitem__(rid, t) if t is not None
                               else self.asking.pop(rid, None),
                               wait_benchmark=lambda: self._wait_benchmark(rid, server_id))
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
        self._set_live(rid, None)

    # ---------------------------------------------------------------- models
    def _prepare_model(self, run: dict) -> bool:
        """Loads the run's model with its load profile on a server CITAR manages (LM Studio), swapping out a model this
        runner loaded earlier but never one the user loaded. The prompt scheduler is told at once (``models_changed``),
        so an answer's ticket made next reads the model now loaded rather than the list it cached a moment ago."""
        from . import servers
        llm = run["llm"]
        sv = servers.find(llm.get("server_id"))
        if sv is None or not sv["connection"].get("manage_loading"):
            return False
        model = llm.get("model")
        entry = servers.model_entry(sv, llm.get("model_id") or model)
        loaded = self._loaded.get(sv["id"])
        if loaded and loaded != model:
            servers.unload_models(sv, [loaded])
            self._loaded.pop(sv["id"], None)
        self._set_live(run["id"], {"run": run["id"], "case": "(loading the model)", "since": time.time()})
        try:
            if servers.ensure_model(sv, model, servers.profile(entry, llm.get("profile_id"))) > 0:
                self._loaded[sv["id"]] = model
        except Exception as e:      # still try: LM Studio may load it just in time
            run = self.get_run(run["id"])
            run["error"] = f"Could not load {model}: {e}"
            self._write(run)
        finally:
            from .pool import scheduler as prompt_scheduler
            prompt_scheduler.get().models_changed(sv["id"])
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
                if sv and self._loaded.get(sv["id"]) and sv["restricted_hours"].get("unload_models"):
                    servers.unload_models(sv, [self._loaded.pop(sv["id"])])
                self._set_live(rid, {"run": rid, "case": f"(paused: {servers.server_name(llm.get('server_id'))} is in its restricted hours)",
                                     "since": time.time()})
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
                    # the subject: the seat a model plays (P3.6.7: an LLM seat, or a hybrid seat's model)
                    subj = next((seat.player for seat in s.seats if seat.model_seat), None)
                    live["thoughts"] = [{"kind": t.get("kind"), "text": (t.get("text") or "")[:1500]}
                                        for t in (s.game.thoughts(subj) if subj is not None else [])][-6:]
            except Exception:
                pass
        if live.get("since"):
            live["seconds"] = round(time.time() - live["since"], 1)
        running = [{"run": v.get("run"), "case": v.get("case"), "rep": v.get("rep"),
                    "seconds": round(time.time() - v["since"], 1) if v.get("since") else None}
                   for v in list(self.lives.values()) if v]
        return {"queue": list(self.queue), "live": live, "running": running}


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
