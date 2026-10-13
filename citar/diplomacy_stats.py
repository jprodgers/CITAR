"""The diplomacy leaderboard: what one seat did in a game's diplomacy, valued alike in every game, and the pairs of a
hybrid benchmark job and its bot-only control game summed up per model (design/phase3-features.md P3.6.9, P3.6.10).

**The recorder** (``DiplomacyRecorder``) follows one seat. After every drive, answer and tool call (the session's
``_after_action``, or the control game's loop after each ``EngineGame.drive``) it reads the negotiations the seat is in
that moved since it last looked, and writes one row per new entry: who made it, its action, and for an entry with a
proposal its worth in gold from the seat's side, ``EngineGame.bot_evaluate(pid, bot, other, give, receive)`` on the
entry's recorded terms against the game as it stands then, with the seat's own profile playing every category (the
hybrid seat's fallback bot, the control seat's bot). Many negotiations open and close inside one drive (bot to bot, and
every proposal a control seat makes, since its game drives all its bots together), so they never reach Python while
open, and ``bot_advice``'s value would be null for them: reading the history after the drive values both arms of a pair
the same way. An accepted deal keeps the value of the proposal that was accepted. The game's events give the rest
(``on_event``): wars declared by and on the seat, cities it captured and lost, peace, eliminations. Rows are
append-only (a session keeps them in its metrics, ``Metrics.data["diplomacy"]``, so they are saved with the game and
come back with it); what the recorder has read so far is worked out again from them (``_sync``). A record that fails
unexpectedly stops for good (``fail``: a ``broken`` row, kept with the others), since rows missed or valued late are not
the same measure as its control's; its job then pairs nothing.

**A pair's line** (``pair_line``): the score difference (the hybrid seat's minus the control seat's) at the end and at
turns 100, 200 and 300 where both games reached them, the deals made, their total and mean value, the acceptance rate of
the seat's own proposals, its wars and how they ended, its captures and losses, and the control arm's own figures beside
them. **The leaderboard** (``leaderboard``): per model and owners preset, the pairs, the mean score difference with a
90% bootstrap interval (``BOOTSTRAP_RESAMPLES`` seeded resamples), deals per game, mean deal value, acceptance rate,
wars declared per game, captures and losses, and the fallbacks: those the model caused (``MODEL_CAUSES``) counted as
its, the others (``OTHER_CAUSES``: a benchmark's queue, a refusal, a prompt that never started) listed beside the
line.

Replaces: nothing; before 3-16 hybrid seats had no benchmark and no leaderboard.
"""
from __future__ import annotations

import random
from typing import Iterable, Optional

from . import engine_api
from .engine_api import ActionError

#: The turns at which a pair's score difference is given besides the end, where both of its games reached them.
MILESTONES = (100, 200, 300)
#: How many resamples the bootstrap interval of a line's mean score difference draws, and the seed they are drawn
#: with, so a line is the same number every time it is read.
BOOTSTRAP_RESAMPLES = 2000
BOOTSTRAP_SEED = 316
#: The interval's coverage: 90%, from the 5th to the 95th percentile of the resampled means.
BOOTSTRAP_COVERAGE = 0.90
#: Fallback causes that are the model's doing (a model reachable but silent past its time once its prompt had started,
#: or one that failed), and those that are not (it waited behind another benchmark, the machine or its admission
#: refused it, or its prompt never started: ``chat.ChatClock._cause``): the leaderboard counts the first against the
#: model and lists the second beside its line (P3.6.10).
MODEL_CAUSES = ("timeout", "failed")
OTHER_CAUSES = ("queued_behind_benchmark", "refused", "not_started")
#: Event types the recorder keeps a row of, when they concern its seat (``on_event``).
EVENT_KINDS = ("war_declared", "peace", "city_captured", "eliminated")


def _other(head: dict, pid: int) -> int:
    """The other party of a negotiation ``pid`` is in."""
    return head["responder"] if head["initiator"] == pid else head["initiator"]


class DiplomacyRecorder:
    """Follows seat ``pid`` through a game's diplomacy (the module docstring). ``bot`` values the proposals: the seat's
    own profile with every category the bot's. ``rows`` is the list the rows go to (a session's
    ``metrics.data["diplomacy"]``, shared by its seats' recorders, each row naming its ``player``); rows already in it
    (a loaded game's) are what the recorder has read so far."""

    def __init__(self, pid: int, bot, rows: Optional[list] = None):
        self.pid = int(pid)
        self.bot = bot
        self.rows: list = rows if rows is not None else []
        self._seen: dict = {}       # negotiation id -> its entries recorded
        self._table: dict = {}      # negotiation id -> the value of the proposal on the table, from the seat's side
        self._watch: set = set()    # the seat's negotiations still open when last looked at
        self._next = 1              # the next negotiation id never looked at
        self._synced = False
        #: why the record stopped (``fail``), or None: a loaded game's rows say so with a ``broken`` row. A broken record
        #: reads nothing more
        self.broken: Optional[str] = next((r.get("error") or "failed" for r in self.rows
                                           if r.get("player") == self.pid and r.get("kind") == "broken"), None)

    def fail(self, turn, why: str) -> None:
        """The record failed unexpectedly (``why``: the error's kind, never its words, which the game's save keeps):
        it stops here, a ``broken`` row saying so (once)."""
        if self.broken is None:
            self.broken = str(why) or "failed"
            self.rows.append({"player": self.pid, "kind": "broken", "turn": turn, "error": self.broken})

    # ------------------------------------------------------------------ reading the game
    def on_event(self, ev: dict) -> None:
        """A game event (the game's subscriber, after the engine call that made it): a row for a war declared by or on
        the seat, peace it made, a city it captured or lost, and an elimination (its own, or a civilization's it was at
        war with: a war's outcome)."""
        kind = ev.get("type")
        if kind not in EVENT_KINDS or self.broken is not None:
            return
        d = ev.get("data") or {}
        pid = self.pid
        row = None
        if kind == "war_declared" and pid in (d.get("attacker"), d.get("defender")):
            ours = d.get("attacker") == pid
            row = {"kind": "war", "by": "us" if ours else "them",
                   "with": d.get("defender") if ours else d.get("attacker")}
        elif kind == "peace" and pid in (d.get("a"), d.get("b")):
            row = {"kind": "peace", "with": d.get("b") if d.get("a") == pid else d.get("a")}
        elif kind == "city_captured" and pid in (d.get("new_owner"), d.get("old_owner")):
            ours = d.get("new_owner") == pid
            row = {"kind": "capture" if ours else "loss", "with": d.get("old_owner") if ours else d.get("new_owner"),
                   "city": d.get("city")}
        elif kind == "eliminated" and isinstance(d.get("player"), int):
            row = {"kind": "eliminated", "with": d.get("player")}
        if row is not None:
            self.rows.append({"player": pid, "turn": ev.get("turn"), **row})

    def observe(self, g) -> None:
        """Read what moved since the last look: every negotiation of the seat with entries not yet recorded, each new
        entry a row (``_entry``), its proposal valued against the game as it stands now. Cheap: a negotiation's head is
        read without copying its history, and only those that moved are read whole. A broken record reads nothing."""
        if self.broken is not None:
            return
        if not self._synced:
            self._sync()
        pid = self.pid
        while True:
            try:
                head = g.negotiation_head(self._next)
            except ActionError:
                break
            if pid in (head["initiator"], head["responder"]):
                self._watch.add(self._next)
            self._next += 1
        for nid in sorted(self._watch):
            try:
                head = g.negotiation_head(nid)
            except ActionError:
                self._watch.discard(nid)
                continue
            if head["entries"] > self._seen.get(nid, 0):
                record = g.negotiation(nid)
                other = _other(head, pid)
                for i, entry in enumerate(record.get("history") or [], 1):
                    if i > self._seen.get(nid, 0):
                        self.rows.append(self._entry(g, nid, i, entry, other))
                        self._seen[nid] = i
            if head["status"] != "open":
                self._watch.discard(nid)

    def _entry(self, g, nid: int, i: int, entry: dict, other: int) -> dict:
        """One entry's row: ``value`` is a proposal's worth from the seat's side, an accept's the value of the proposal
        it accepted (the one on the table), None for any other entry and for terms that no longer read."""
        pid = self.pid
        proposal = entry.get("proposal")
        row = {"player": pid, "kind": "entry", "nid": nid, "i": i, "turn": entry.get("turn"),
               "by": entry.get("by"), "with": other, "action": entry.get("action"), "proposal": bool(proposal),
               "value": None}
        if proposal:
            try:
                value = g.bot_evaluate(pid, self.bot, other, proposal.get(str(pid)) or [],
                                       proposal.get(str(other)) or [])
                row["value"] = round(float(value), 2)
            except (ActionError, ValueError, TypeError):
                row["value"] = None
            try:
                row["categories"] = sorted(engine_api.proposal_categories(proposal))
            except (ActionError, ValueError, TypeError):
                row["categories"] = []
            self._table[nid] = row["value"]
        elif entry.get("action") == "accept":
            row["value"] = self._table.get(nid)
        return row

    def _sync(self) -> None:
        """What the rows already hold (a loaded game's): how many entries of each negotiation are recorded, and the
        value of the proposal on each table. The negotiations are looked at again from the first, so one that moved
        while nobody recorded is caught up at the next look."""
        for row in self.rows:
            if row.get("player") != self.pid or row.get("kind") != "entry":
                continue
            nid = row["nid"]
            self._seen[nid] = max(self._seen.get(nid, 0), int(row.get("i") or 0))
            if row.get("proposal"):
                self._table[nid] = row.get("value")
        self._synced = True

    def mine(self) -> list:
        """The rows of this recorder's seat."""
        return [r for r in self.rows if r.get("player") == self.pid]


# ---------------------------------------------------------------------------- one seat's game
def final_of(g, pid: int) -> dict:
    """How the seat's game ended (or stands): its score (0 once eliminated), whether it is alive and won, the game's
    turn, phase, winner and victory, and its score at each of ``MILESTONES`` the game reached (the turn statistics)."""
    summ = g.summary()
    alive = bool(g.is_alive(pid))
    score = (g.standing(pid).get("score") or 0) if alive else 0
    at = {}
    wanted = set(MILESTONES)
    for row in g.stats() or []:
        t = row.get("turn")
        if t in wanted:
            p = (row.get("players") or {}).get(str(pid)) or {}
            at[str(t)] = (p.get("score") or 0) if p.get("alive", True) else 0
    return {"turn": summ["turn"], "phase": summ["phase"], "winner": summ["winner"], "victory": summ["victory"],
            "score": score, "alive": alive, "won": summ["winner"] == pid, "scores_at": at}


def summarize(rows: Iterable[dict], pid: int, final: Optional[dict] = None) -> dict:
    """What a seat's rows say (one arm of a pair): proposals made and received, how many of its own were accepted and
    the rate, the deals made (each with the value of the proposal accepted) with their total and mean, the wars it
    declared and had declared on it with how each ended (``peace`` on a turn, ``eliminated them``, ``eliminated``, or
    ``ongoing``), its captures and losses, peace made; and ``final`` (``final_of``) when given."""
    rows = [r for r in rows if r.get("player") == pid]
    entries = [r for r in rows if r.get("kind") == "entry"]
    made = [r for r in entries if r.get("by") == pid and r.get("proposal")]
    received = [r for r in entries if r.get("by") != pid and r.get("proposal")]
    accepted_own = 0
    deals = []
    proposer: dict = {}
    for r in sorted(entries, key=lambda r: (r["nid"], r["i"])):
        if r.get("proposal"):
            proposer[r["nid"]] = r.get("by")
        elif r.get("action") == "accept":
            mine = proposer.get(r["nid"]) == pid
            if mine and r.get("by") != pid:
                accepted_own += 1
            deals.append({"nid": r["nid"], "turn": r.get("turn"), "with": r.get("with"),
                          "proposed_by": "us" if mine else "them", "value": r.get("value")})
    values = [d["value"] for d in deals if d["value"] is not None]
    events = sorted((r for r in rows if r.get("kind") != "entry"), key=lambda r: (r.get("turn") or 0))
    wars = []
    for r in events:
        if r["kind"] == "war":
            wars.append({"turn": r.get("turn"), "with": r.get("with"), "by": r.get("by"), "ended": None,
                         "outcome": "ongoing", "captured": 0, "lost": 0})
        elif r["kind"] == "eliminated" and r.get("with") == pid:
            for w in wars:
                if w["ended"] is None:
                    w.update(ended=r.get("turn"), outcome="eliminated")
        elif r["kind"] in ("peace", "eliminated"):
            war = next((w for w in reversed(wars) if w["with"] == r.get("with") and w["ended"] is None), None)
            if war is not None:
                war.update(ended=r.get("turn"), outcome="peace" if r["kind"] == "peace" else "eliminated them")
    for r in events:
        if r["kind"] in ("capture", "loss"):
            # the war with that civilization the city changed hands in (an elimination comes the same turn as the
            # capture of the last city, before it among the events)
            t = r.get("turn") or 0
            war = next((w for w in reversed(wars) if w["with"] == r.get("with") and (w["turn"] or 0) <= t
                        and (w["ended"] is None or w["ended"] >= t)), None)
            if war is not None:
                war["captured" if r["kind"] == "capture" else "lost"] += 1
    out = {"proposals_made": len(made), "proposals_received": len(received), "accepted_own": accepted_own,
           "acceptance_rate": round(accepted_own / len(made), 4) if made else None,
           "deals": len(deals), "deals_valued": len(values),
           "deal_value_total": round(sum(values), 2) if values else 0.0,
           "deal_value_mean": round(sum(values) / len(values), 2) if values else None,
           "deal_list": deals,
           "wars_declared": sum(1 for w in wars if w["by"] == "us"),
           "wars_declared_on": sum(1 for w in wars if w["by"] == "them"), "wars": wars,
           "captures": sum(1 for r in events if r["kind"] == "capture"),
           "losses": sum(1 for r in events if r["kind"] == "loss"),
           "peace": sum(1 for r in events if r["kind"] == "peace")}
    if final is not None:
        out["final"] = final
    return out


def fallbacks_of(metrics_data: dict, pid: int) -> dict:
    """A hybrid seat's fallbacks in a game, by cause (P3.6.5, P3.6.11): its chats its fallback bot answered (the
    metrics' ``fallback`` rows) and its diplomat phases that did not run (``skipped``, ``refused``, ``failed``), each
    with its cause; ``model`` counts the ones the model caused (``MODEL_CAUSES``: a chat it was too slow for once its
    prompt had started, a call that failed), ``other`` the rest by cause. A chat whose time ran out before its prompt
    started (``not_started``, the chat clock's cause in a benchmark game) and a phase that never started (``skipped``:
    its prompt waited past its 30 s, which a benchmark game's never does) are the machine's, ``not_started``."""
    chat: dict = {}
    for x in (metrics_data or {}).get("negotiations") or []:
        if x.get("player") == pid and x.get("outcome") == "fallback":
            cause = x.get("cause") or "failed"
            chat[cause] = chat.get(cause, 0) + 1
    phases: dict = {}
    for x in (metrics_data or {}).get("diplomat") or []:
        if x.get("player") != pid or x.get("outcome") not in ("skipped", "refused", "failed"):
            continue
        if x.get("outcome") == "failed":
            cause = "failed"
        else:
            cause = x.get("cause") if x.get("cause") in OTHER_CAUSES else "not_started"
        phases[cause] = phases.get(cause, 0) + 1
    total: dict = {}
    for part in (chat, phases):
        for k, v in part.items():
            total[k] = total.get(k, 0) + v
    return {"chats": chat, "phases": phases,
            "model": sum(v for k, v in total.items() if k in MODEL_CAUSES),
            "other": {k: v for k, v in sorted(total.items()) if k not in MODEL_CAUSES}}


# ---------------------------------------------------------------------------- pairs and the leaderboard
def pair_line(hybrid: dict, control: dict) -> dict:
    """A pair's line (P3.6.10) from its two arms' summaries (``summarize`` with ``final``): the score difference,
    hybrid minus control, at the end and at each of ``MILESTONES`` both games reached; the hybrid arm's deals, their
    total and mean value, its acceptance rate, wars with their outcomes, captures and losses; and the control arm's
    figures beside them."""
    hf, cf = hybrid.get("final") or {}, control.get("final") or {}
    diff = {"end": (hf.get("score") or 0) - (cf.get("score") or 0)}
    for t in MILESTONES:
        a, b = (hf.get("scores_at") or {}).get(str(t)), (cf.get("scores_at") or {}).get(str(t))
        if a is not None and b is not None:
            diff[str(t)] = a - b
    keep = ("proposals_made", "proposals_received", "accepted_own", "acceptance_rate", "deals", "deals_valued",
            "deal_value_total", "deal_value_mean", "wars_declared", "wars_declared_on", "captures", "losses", "peace")
    return {"score_diff": diff, **{k: hybrid.get(k) for k in keep}, "wars": hybrid.get("wars") or [],
            "won": bool(hf.get("won")), "alive": hf.get("alive"),
            "control": {**{k: control.get(k) for k in keep}, "won": bool(cf.get("won")), "alive": cf.get("alive"),
                        "wars": control.get("wars") or []}}


def bootstrap_interval(values: list, *, resamples: int = BOOTSTRAP_RESAMPLES, seed: int = BOOTSTRAP_SEED,
                       coverage: float = BOOTSTRAP_COVERAGE) -> Optional[list]:
    """The percentile bootstrap interval of the mean of ``values``: ``resamples`` resamples with replacement, drawn by
    ``random.Random(seed)`` (each resample ``len(values)`` draws of ``randrange``), their means sorted, and the means at
    the positions ``round(q * (resamples - 1))`` for q the lower and upper tail (0.05 and 0.95 for 90%). None for no
    values."""
    n = len(values)
    if not n:
        return None
    rng = random.Random(seed)
    means = sorted(sum(values[rng.randrange(n)] for _ in range(n)) / n for _ in range(resamples))
    tail = (1.0 - coverage) / 2.0
    lo, hi = means[int(round(tail * (resamples - 1)))], means[int(round((1.0 - tail) * (resamples - 1)))]
    return [round(lo, 2), round(hi, 2)]


def owners_label(owners) -> str:
    """How a pair's owners are named on the board: the preset's name, or "custom" with the model's categories."""
    from .agents import hybrid
    try:
        full = hybrid.owners_map(owners if owners is not None else "trade_only")
    except ValueError:
        return "custom"
    preset = hybrid.preset_of(full)
    return preset or "custom: " + ", ".join(hybrid.model_categories(full))


def leaderboard(pairs: list) -> list:
    """The lines of the diplomacy leaderboard from pairs (each ``{"model", "owners", "line", "fallbacks", ...}``, its
    ``line`` a ``pair_line``), one per model and owners preset: how many pairs, the mean score difference and its 90%
    bootstrap interval, deals per game, the mean value of the deals valued, the acceptance rate of the seat's own
    proposals (over all its proposals), wars declared per game, captures and losses per game, the control arm's deals
    per game and mean deal value beside them, and the fallbacks: ``fallbacks_per_game`` counts only those the model
    caused, ``other_fallbacks`` lists the rest by cause. Sorted by mean score difference, best first."""
    groups: dict = {}
    for p in pairs:
        groups.setdefault(_line_key(p), []).append(p)
    out = []
    for (model, preset), ps in groups.items():
        n = len(ps)
        lines = [p["line"] for p in ps]
        diffs = [float(ln["score_diff"]["end"]) for ln in lines]
        mean_value = _mean_value(lines)
        control_value = _mean_value([ln.get("control") or {} for ln in lines])
        made = sum(ln.get("proposals_made") or 0 for ln in lines)
        accepted = sum(ln.get("accepted_own") or 0 for ln in lines)
        other: dict = {}
        model_fb = 0
        for p in ps:
            fb = p.get("fallbacks") or {}
            model_fb += int(fb.get("model") or 0)
            for k, v in (fb.get("other") or {}).items():
                other[k] = other.get(k, 0) + int(v)
        out.append({
            "model": model, "owners": preset, "pairs": n,
            "mean_score_diff": round(sum(diffs) / n, 2), "ci90": bootstrap_interval(diffs),
            "deals_per_game": round(sum(ln.get("deals") or 0 for ln in lines) / n, 2),
            "mean_deal_value": mean_value,
            "acceptance_rate": round(accepted / made, 4) if made else None,
            "wars_declared_per_game": round(sum(ln.get("wars_declared") or 0 for ln in lines) / n, 2),
            "captures_per_game": round(sum(ln.get("captures") or 0 for ln in lines) / n, 2),
            "losses_per_game": round(sum(ln.get("losses") or 0 for ln in lines) / n, 2),
            "control_deals_per_game": round(sum((ln.get("control") or {}).get("deals") or 0 for ln in lines) / n, 2),
            "control_mean_deal_value": control_value,
            "fallbacks_per_game": round(model_fb / n, 2), "other_fallbacks": other,
        })
    out.sort(key=lambda r: (-r["mean_score_diff"], r["model"], r["owners"]))
    return out


def board(pairs: list, scope=None) -> dict:
    """``GET /api/models/diplomacy``'s answer (P3.6.10) from the pairs of the runs the caller may see
    (``BenchmarkScheduler.pairs``): for a member (``scope``, ``aggregate.scope_for``), only the pairs of their own games
    and of games other members pool, another member's part of a line only when enough accounts are behind it
    (``aggregate.publishable``, per model and owners preset), and only their own pairs named (``games``; the others
    counted in ``pooled_pairs``); for an administrator (``scope`` None) every pair, each named. ``withheld`` names the
    lines whose pooled part was held back, with how many accounts it had."""
    from . import aggregate
    withheld: dict = {}
    if scope is not None:
        pairs = [p for p in pairs if p.get("game_id") in scope.game_ids]
        by_line: dict = {}
        for p in pairs:
            by_line.setdefault(_line_key(p), set()).add(p["game_id"])
        allowed, withheld = aggregate.publishable(scope, by_line)
        pairs = [p for p in pairs if p["game_id"] in allowed.get(_line_key(p), ())]
    lines = leaderboard(pairs)
    for line in lines:
        mine = [p for p in pairs if _line_key(p) == (line["model"], line["owners"])
                and (scope is None or p["game_id"] in scope.own_ids)]
        line["games"] = [{"run": p.get("run"), "run_id": p.get("run_id"), "job_id": p.get("job_id"),
                          "scenario": p.get("scenario"), "seed": p.get("seed"),
                          "score_diff": p["line"]["score_diff"], "deals": p["line"].get("deals"),
                          "control": p.get("control")} for p in mine]
        line["pooled_pairs"] = line["pairs"] - len(mine)
    out = {"lines": lines, "bootstrap": {"resamples": BOOTSTRAP_RESAMPLES, "seed": BOOTSTRAP_SEED,
                                         "coverage": BOOTSTRAP_COVERAGE},
           "milestones": list(MILESTONES), "model_causes": list(MODEL_CAUSES), "other_causes": list(OTHER_CAUSES),
           "withheld": [{"model": k[0], "owners": k[1], "contributors": n,
                         "reason": f"Pooled pairs from {n} other account{'s' if n != 1 else ''}: at least "
                                   f"{aggregate.min_contributors()} are needed before they are shown."}
                        for k, n in sorted(withheld.items())]}
    if scope is not None:
        out["scope"] = scope.client()
    return out


def _line_key(pair: dict) -> tuple:
    """The line a pair belongs to: its model and its owners as the board names them."""
    return (pair.get("model") or "?", owners_label(pair.get("owners")))


def _mean_value(arms: list) -> Optional[float]:
    """The mean value of every valued deal of ``arms`` (lines, or their control parts): their totals over their counts,
    so each deal weighs the same whichever game it was made in. None when no deal was valued."""
    n = sum(int(a.get("deals_valued") or 0) for a in arms)
    if not n:
        return None
    return round(sum(float(a.get("deal_value_total") or 0.0) for a in arms) / n, 2)
