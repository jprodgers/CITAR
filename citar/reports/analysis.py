"""Deterministic analysis for reports: statistics helpers and the findings (plain-language observations computed from
the data, each tagged with the section it belongs to)."""
from __future__ import annotations

import math
import statistics
from datetime import datetime
from typing import Optional

from .. import costing, servers as S


def mean_ci(xs: list[float]) -> tuple[Optional[float], Optional[float]]:
    """A mean with its 95% confidence interval.

    The interval is the point. A model comparison over four games has an interval wide enough to say
    so, and a table of bare means does not.
    """
    xs = [x for x in xs if x is not None]
    if not xs:
        return None, None
    m = statistics.mean(xs)
    if len(xs) < 2:
        return m, None
    return m, 1.96 * statistics.stdev(xs) / math.sqrt(len(xs))


def welch(a: list[float], b: list[float]) -> Optional[dict]:
    """Welch's t-test (two-sided, normal approximation for the p-value; fine for a screening note)."""
    if len(a) < 2 or len(b) < 2:
        return None
    ma, mb = statistics.mean(a), statistics.mean(b)
    va, vb = statistics.variance(a), statistics.variance(b)
    se = math.sqrt(va / len(a) + vb / len(b))
    if se == 0:
        return {"diff": ma - mb, "t": math.inf if ma != mb else 0.0, "p": 0.0 if ma != mb else 1.0}
    t = (ma - mb) / se
    p = math.erfc(abs(t) / math.sqrt(2))
    return {"diff": ma - mb, "t": t, "p": p}


def pareto(points: list[dict], x: str, y: str) -> list[str]:
    """Names on the cost/performance frontier: nobody else is both cheaper (lower x) and better (higher y)."""
    out = []
    for p in points:
        if p.get(x) is None or p.get(y) is None:
            continue
        dominated = any(q is not p and q.get(x) is not None and q.get(y) is not None and q[x] <= p[x] and q[y] >= p[y]
                        and (q[x] < p[x] or q[y] > p[y]) for q in points)
        if not dominated:
            out.append(p["name"])
    return out


def money(v: Optional[float], sym: str = "$") -> str:
    """A number as money, in the report's currency."""
    if v is None:
        return "–"
    a = abs(v)
    if a >= 100:
        return f"{sym}{v:,.0f}"
    if a >= 1:
        return f"{sym}{v:,.2f}"
    if a >= 0.01:
        return f"{sym}{v:.3f}"
    if a == 0:
        return f"{sym}0"
    digits = min(12, 1 - math.floor(math.log10(a)))       # two significant digits for tiny amounts
    return f"{sym}{v:.{digits}f}"


COMPONENTS = [("depreciation", "Depreciation"), ("fixed", "Fixed monthly"), ("hourly", "Usage rate"),
              ("energy_idle", "Energy (idle share)"), ("energy_dynamic", "Energy (work)"), ("tokens", "API tokens")]


def findings(data: dict, roll: dict) -> list[dict]:
    """[{section, text, level}] - level: info | good | warn."""
    sym = data["symbol"]
    out = []
    acts = data["acts"]
    total = sum(a["total"] for a in acts)
    if not acts:
        return [{"section": "summary", "level": "warn",
                 "text": "Nothing was recorded in this scope. Costs are tracked from the moment the Servers feature was "
                         "installed; widen the date range or run something first."}]
    comp = {k: sum(a["cost"][k] for a in acts) for k, _ in COMPONENTS}
    if data["basis"] != "full":
        comp.pop("energy_idle")
    big = max(comp, key=comp.get)
    if total > 0:
        out.append({"section": "summary", "level": "info",
                    "text": f"{len(acts)} activities cost {money(total, sym)} in total; the largest part is "
                            f"{dict(COMPONENTS)[big].lower()} ({100 * comp[big] / total:.0f}%)."})
    # configurations whose cost is only a lower bound (energy or tokens not priced) are left out of cost rankings
    for k, c in roll.items():
        if c.get("incomplete") and not k.startswith(("bots", "report writing")):
            out.append({"section": "cost_per_unit", "level": "warn",
                        "text": f"{k}: costs are incomplete ({', '.join(c['incomplete'])} has no power figures or prices), so it is "
                                f"left out of cost comparisons until those are filled in on the Servers page."})
    # cheapest / dearest per model turn
    rows = [(k, c) for k, c in roll.items() if c.get("per_turn") is not None and c["cost"] > 0 and not k.startswith(("bots", "report writing"))
            and not c.get("incomplete")]
    if len(rows) >= 2:
        rows.sort(key=lambda kv: kv[1]["per_turn"])
        lo, hi = rows[0], rows[-1]
        ratio = hi[1]["per_turn"] / lo[1]["per_turn"] if lo[1]["per_turn"] else None
        out.append({"section": "cost_per_unit", "level": "info",
                    "text": f"Cheapest per model turn: {lo[0]} at {money(lo[1]['per_turn'], sym)}; most expensive: {hi[0]} at "
                            f"{money(hi[1]['per_turn'], sym)}" + (f" ({ratio:.1f}x)." if ratio and ratio < 1e6 else ".")})
    # performance
    perf = [(k, c) for k, c in roll.items() if c["perf"]]
    if perf:
        perf.sort(key=lambda kv: -(kv[1]["perf_mean"] or 0))
        k, c = perf[0]
        m, ci = mean_ci(c["perf"])
        n = len(c["perf"])
        out.append({"section": "models", "level": "good",
                    "text": f"Best game performance: {k}, {m:.0f}/100" + (f" ± {ci:.0f}" if ci else "") +
                            f" over {n} game{'s' if n != 1 else ''}" + (" (provisional: fewer than 3 games)." if n < 3 else ".")})
        if len(perf) >= 2:
            w = welch(perf[0][1]["perf"], perf[1][1]["perf"])
            if w:
                sig = w["p"] < 0.05
                out.append({"section": "models", "level": "info",
                            "text": f"{perf[0][0]} vs {perf[1][0]}: {w['diff']:+.1f} performance points "
                                    f"({'a significant difference' if sig else 'not significant'}, p≈{w['p']:.2f})."})
        pts = [{"name": k, "x": c["per_turn"], "y": c["perf_mean"]} for k, c in perf if c.get("per_turn") is not None
               and not c.get("incomplete")]
        front = set(pareto(pts, "x", "y"))
        for p in pts:
            if p["name"] not in front:
                better = [q for q in pts if q["name"] in front and q["x"] <= p["x"] and q["y"] >= p["y"]]
                if better:
                    out.append({"section": "models", "level": "warn",
                                "text": f"{p['name']} is outperformed on both counts: {better[0]['name']} scores higher and costs "
                                        f"less per turn."})
    # speed
    speed = [(k, c) for k, c in roll.items() if c.get("avg_turn") is not None]
    if len(speed) >= 2:
        speed.sort(key=lambda kv: kv[1]["avg_turn"])
        out.append({"section": "models", "level": "info",
                    "text": f"Fastest turns: {speed[0][0]} ({speed[0][1]['avg_turn']:.0f}s average); slowest: {speed[-1][0]} "
                            f"({speed[-1][1]['avg_turn']:.0f}s)."})
    # reliability
    for k, c in roll.items():
        if c.get("clean") is not None and c["clean"] < 0.8 and c["model_turns"] >= 5:
            out.append({"section": "behavior", "level": "warn",
                        "text": f"{k} ended only {100 * c['clean']:.0f}% of its turns cleanly (the rest hit limits, stalled or failed)."})
    # probes
    pr = [(k, c) for k, c in roll.items() if c["probe_checked"]]
    for k, c in sorted(pr, key=lambda kv: -(kv[1]["pass_rate"] or 0)):
        out.append({"section": "probes", "level": "info",
                    "text": f"{k} matched the expected answer in {c['probe_passed']} of {c['probe_checked']} checked probe cases "
                            f"({100 * c['pass_rate']:.0f}%)."})
    # servers
    for sid, sv_out in data["cost"]["servers"].items():
        sv = data["servers_by_id"].get(sid)
        cal = sv_out.get("calendar")
        if not sv or not cal or sv["kind"] not in ("owned", "leased"):
            continue
        fixed_cal = cal["depreciation"] + cal["fixed"]
        alloc = sv_out["allocated"]["depreciation"] + sv_out["allocated"]["fixed"]
        if fixed_cal > 0 and sv_out.get("utilization") is not None and alloc > 0:
            out.append({"section": "servers", "level": "info",
                        "text": f"{sv['name']} was in use {100 * sv_out['utilization']:.1f}% of the period. Of its "
                                f"{money(fixed_cal, sym)} fixed costs over the period, {money(alloc, sym)} went to recorded work "
                                f"and {money(max(0.0, fixed_cal - alloc), sym)} is unallocated idle time."})
    # energy data quality
    mh = sum(c["measured_h"] for c in roll.values())
    eh = sum(c["estimated_h"] for c in roll.values())
    if mh + eh > 0 and eh / (mh + eh) > 0.5:
        out.append({"section": "data_quality", "level": "warn",
                    "text": f"{100 * eh / (mh + eh):.0f}% of the energy figures are estimates from the servers' power settings "
                            f"rather than measurements; measured idle/load watts make them much better."})
    for n in data["cost"]["notes"]:
        out.append({"section": "data_quality", "level": "warn", "text": n})
    for sid in {s for a in acts for s in a["servers"]}:
        sv = data["servers_by_id"].get(sid)
        if sv:
            for issue in S.issues(sv, data["registry"]):
                out.append({"section": "data_quality", "level": "warn", "text": f"{sv['name']}: {issue}"})
    # depreciation sensitivity
    sens = data.get("sensitivity")
    if sens and len(sens) >= 2 and any(v["depreciation"] > 0 for v in sens.values()):
        ys = sorted(sens)
        a, b = sens[ys[0]]["allocated"], sens[ys[-1]]["allocated"]
        if a > 0:
            out.append({"section": "depreciation", "level": "info",
                        "text": f"Assuming the hardware lasts {ys[-1]} years instead of {ys[0]} changes these activities' cost "
                                f"from {money(a, sym)} to {money(b, sym)} ({100 * (b - a) / a:+.0f}%)."})
    # small samples
    for k, c in roll.items():
        if 0 < c["games"] < 3 and c["perf"]:
            out.append({"section": "data_quality", "level": "warn",
                        "text": f"{k}: only {c['games']} game{'s' if c['games'] != 1 else ''}; treat its averages as provisional."})
    return out


def whatif_tokens(data: dict, roll: dict) -> list[dict]:
    """What each configuration's tokens would cost on each API model in the registry (list prices)."""
    apis = []
    for sv in data["registry"]["servers"]:
        if sv["kind"] != "api":
            continue
        for m in sv["models"]:
            price = S.model_price(sv, m["key"], datetime.now())
            if price:
                apis.append((f"{m['key']} ({sv['name']})", price))
    rows = []
    for k, c in roll.items():
        if not (c["in"] or c["out"]):
            continue
        toks = {"in": c["in"], "out": c["out"], "cr": c["cr"], "cw": c["cw"]}
        rows.append({"config": k, "actual": c["cost"], "tokens": toks,
                     "alt": {name: costing.price_tokens(price, toks) for name, price in apis}})
    return rows


def whatif_hours(data: dict, roll: dict) -> list[dict]:
    """What each configuration's model-busy hours would cost on each owned/leased server (fully loaded hourly rate,
    same speed assumed - a rough bound, since other hardware runs at a different speed)."""
    targets = [sv for sv in data["registry"]["servers"] if sv["kind"] in ("owned", "leased")]
    rows = []
    for k, c in roll.items():
        if c["held_h"] <= 0 or k.startswith("bots"):
            continue
        rows.append({"config": k, "actual": c["cost"], "hours": c["held_h"],
                     "alt": {sv["name"]: costing.hourly_profile(sv, data["registry"])["total"] * c["held_h"] for sv in targets}})
    return rows
