"""Prices the usage ledger (usage.py) with the server registry (servers.py).

Method, per server and per minute:
  * Fixed costs are charged on a calendar basis: a server's hourly fixed rate is its depreciation (straight-line per
    component over its lifespan, less resale) + fixed monthly costs (lease, maintenance, API subscription, a share of
    the electricity bill's fixed fee) spread over the hours of that month. An activity pays that rate for the time it
    held the server; activities holding it at the same time split the minute. Time nobody held is "unallocated".
  * Rented machines' hourly usage rate is charged the same way, but only for time in use.
  * Energy: the machine's power for the minute is measured (GPU watts sampled with nvidia-smi plus CPU utilisation
    times the server's CPU watts) or, where there are no samples, estimated from what CITAR had it doing (model
    generation seconds for the GPU, CPU seconds for the CPU). Idle power is split like fixed costs (by time held);
    the extra "dynamic" power above idle goes to the activities that did work in that minute, by their share of it.
    The energy price comes from the electricity plan in force (flat, time-of-use or tiered) plus, if chosen, the
    plan's fixed fee spread over the household's monthly kWh.
  * API servers: tokens x the model's per-million prices (input, output, cache reads, cache writes).
Everything is computed with the configuration in force at the time of use (effective-dated cost periods).
"""
from __future__ import annotations

import calendar
from collections import defaultdict
from datetime import datetime, timedelta
from typing import Optional

from . import servers as S
from . import usage as U

HOURS_PER_YEAR = 8766.0
BUCKET = 60


def _dt(ts: float) -> datetime:
    """A timestamp as a datetime."""
    return datetime.fromtimestamp(ts)


def _parse_date(s: str) -> Optional[datetime]:
    """Parse a ``YYYY-MM-DD`` date, or None."""
    try:
        return datetime.strptime(s, "%Y-%m-%d") if s else None
    except ValueError:
        return None


def hours_in_month(when: datetime) -> float:
    """Hours in the month containing a moment, for spreading fixed costs."""
    return calendar.monthrange(when.year, when.month)[1] * 24.0


def depreciation_rate(server: dict, when: datetime, lifespan_override: Optional[float] = None) -> float:
    """Currency per hour of calendar time for the server's owned components at `when`."""
    rate = 0.0
    for c in server.get("components") or []:
        price = c.get("price")
        if not price:
            continue
        years = float(lifespan_override or c.get("lifespan_years") or 4)
        bought = _parse_date(c.get("purchased"))
        retired = _parse_date(c.get("retired") or "")
        if retired and when >= retired:
            continue
        if bought:
            if when < bought or when >= bought + timedelta(days=365.25 * years):
                continue
        rate += max(0.0, price - (c.get("resale") or 0.0)) / (years * HOURS_PER_YEAR)
    return rate


def energy_price(plan: Optional[dict], when: datetime) -> tuple[Optional[float], float]:
    """(price per kWh incl. any fixed-fee adder, fee per hour charged by share) for an electricity plan at `when`."""
    if not plan:
        return None, 0.0
    p = S.period_at(plan["periods"], when)
    if not p:
        return None, 0.0
    rate = p.get("rate_kwh")
    if p["type"] == "tou" and p.get("tou"):
        hit = S._window_hit(p["tou"], when)
        if hit:
            rate = hit[0].get("rate_kwh", rate)
    elif p["type"] == "tiered" and p.get("tiers"):
        usage = p.get("household_kwh_month") or 0
        tiers = sorted(p["tiers"], key=lambda t: (t.get("up_to_kwh") is None, t.get("up_to_kwh") or 0))
        rate = tiers[-1]["rate_kwh"]
        for t in tiers:
            if t.get("up_to_kwh") is None or usage <= t["up_to_kwh"]:
                rate = t["rate_kwh"]
                break
    fee_hourly = 0.0
    if p.get("fixed_monthly"):
        if p["fee_allocation"] == "household_kwh" and p.get("household_kwh_month"):
            rate = (rate or 0.0) + p["fixed_monthly"] / p["household_kwh_month"]
        elif p["fee_allocation"] == "share":
            fee_hourly = p["fixed_monthly"] * (p.get("share_pct") or 0) / 100.0 / hours_in_month(when)
    return rate, fee_hourly


def fixed_rates(server: dict, reg: dict, when: datetime, lifespan_override: Optional[float] = None) -> dict:
    """Currency per hour: depreciation, other fixed (lease, maintenance, subscription, electricity fee share) and the
    rented-machine usage rate; plus the energy price per kWh."""
    per = S.period_at(server["costs"], when) or {}
    hm = hours_in_month(when)
    plan = next((p for p in reg["electricity_plans"] if p["id"] == per.get("electricity_plan_id")), None)
    kwh_price, fee_hourly = energy_price(plan, when)
    return {"depreciation": depreciation_rate(server, when, lifespan_override) if server["kind"] == "owned" else 0.0,
            "fixed": (per.get("fixed_monthly") or 0.0) / hm + (per.get("api_fixed_monthly") or 0.0) / hm + fee_hourly,
            "hourly": (per.get("hourly_rate") or 0.0) if server["kind"] == "leased" else 0.0,
            "kwh": kwh_price, "plan": plan["name"] if plan else None}


def _threads(server: dict) -> int:
    """How many threads a machine has, for dividing its cost between concurrent work."""
    return int(((server.get("hardware") or {}).get("cpu") or {}).get("threads") or 8)


def _new_cost() -> dict:
    """A blank cost breakdown."""
    return {"depreciation": 0.0, "fixed": 0.0, "hourly": 0.0, "energy_idle": 0.0, "energy_dynamic": 0.0, "tokens": 0.0,
            "kwh_idle": 0.0, "kwh_dynamic": 0.0, "held_h": 0.0, "busy_h": 0.0, "cpu_h": 0.0,
            "in": 0, "out": 0, "rsn": 0, "cr": 0, "cw": 0, "req": 0, "energy_measured_h": 0.0, "energy_estimated_h": 0.0,
            "unpriced_kwh": 0.0, "unpriced_tokens": 0}


def total(c: dict, basis: str = "full") -> float:
    """The total of a cost breakdown, on the chosen energy basis.

    ``full`` counts all the energy the machine drew; ``marginal`` counts only the extra caused by the
    work. Both are defensible and they differ by a lot, which is why it is a choice rather than a
    constant.
    """
    t = c["depreciation"] + c["fixed"] + c["hourly"] + c["energy_dynamic"] + c["tokens"]
    if basis == "full":
        t += c["energy_idle"]
    return t


def compute(since: Optional[float] = None, until: Optional[float] = None, reg: Optional[dict] = None,
            ledger: Optional[dict] = None, lifespan_override: Optional[float] = None) -> dict:
    """Cost every activity in [since, until]. Returns {
        "acts": {act_id: {"info": act row, "servers": {srv: cost}, "models": {model: {...}}}},
        "servers": {srv: {"allocated": cost, "calendar": {...}, "power": {...}}},
        "days": {date: {srv: cost}}, "notes": [...]}"""
    reg = reg or S.load()
    ledger = ledger or U.read(since, until)
    servers = {s["id"]: s for s in reg["servers"]}
    acts_out: dict = {}
    notes: set = set()

    def act_entry(act_id):
        """The cost entry for one activity, created on first use."""
        if act_id not in acts_out:
            acts_out[act_id] = {"info": ledger["acts"].get(act_id) or {"id": act_id, "kind": "unknown"},
                                "servers": defaultdict(_new_cost), "models": defaultdict(_new_cost)}
        return acts_out[act_id]

    # ---- per server, per minute: who held it and what work they did
    days: dict = defaultdict(lambda: defaultdict(float))     # date -> srv -> cost (full basis)
    hours: dict = defaultdict(lambda: defaultdict(float))    # "YYYY-MM-DD HH:00" -> srv -> cost
    buckets: dict = defaultdict(lambda: defaultdict(lambda: defaultdict(lambda: [0.0, 0.0, 0.0])))   # srv -> m -> act -> [held, busy, cpu]
    for sp in ledger["spans"]:
        srv = sp.get("srv") or "unassigned"
        t0, t1 = float(sp["t0"]), float(sp["t1"])
        if since:
            t0c = max(t0, since)
        else:
            t0c = t0
        t1c = min(t1, until) if until else t1
        dur = max(1e-6, t1 - t0)
        frac_range = max(0.0, (t1c - t0c) / dur)
        if frac_range <= 0:
            continue
        a = act_entry(sp["act"])
        sc = a["servers"][srv]
        model = sp.get("model")
        mc = a["models"][model] if model and model != "?" else None
        # tokens are priced at the span's time on API servers
        toks = {f: (sp.get(f) or 0) * frac_range for f in U.TOKEN_FIELDS}
        for f, v in toks.items():
            sc[f] += v
            if mc is not None:
                mc[f] += v
        sv = servers.get(srv)
        busy = (sp.get("busy") or 0.0) * frac_range
        if mc is not None:
            mc["busy_h"] += busy / 3600
        if sv and sv["kind"] == "api" and model:
            price = S.model_price(sv, model, _dt(t1))
            if price:
                c = (toks["in"] * price["input"] + toks["out"] * price["output"] + toks["cr"] * price.get("cache_read", 0)
                     + toks["cw"] * price.get("cache_write", 0)) / 1e6
                sc["tokens"] += c
                if mc is not None:
                    mc["tokens"] += c
                days[_dt(t1).strftime("%Y-%m-%d")][srv] += c
                hours[_dt(t1).strftime("%Y-%m-%d %H:00")][srv] += c
            elif toks["in"] or toks["out"]:
                sc["unpriced_tokens"] += toks["in"] + toks["out"]
                notes.add(f"No token price for {model} on {sv['name']}; its tokens are uncosted.")
        if sv is None:
            if srv == "unassigned":
                notes.add("Some model usage has no server (a seat not chosen from the Servers list); it is uncosted.")
            else:
                notes.add(f"Usage on a deleted server ({srv}) is uncosted.")
        # spread held/busy/cpu over the minutes of the span
        held, cpu = (sp.get("held") or 0.0), (sp.get("cpu") or 0.0)
        if not (held or busy or cpu):
            continue
        m0, m1 = int(t0c // BUCKET), int(max(t0c, t1c - 1e-6) // BUCKET)
        for m in range(m0, m1 + 1):
            lo, hi = max(t0c, m * BUCKET), min(t1c, (m + 1) * BUCKET)
            if hi <= lo:
                continue
            f = (hi - lo) / dur
            cell = buckets[srv][m][sp["act"]]
            cell[0] += held * f
            cell[1] += busy * f
            cell[2] += cpu * f

    # ---- measured power, per server per minute
    power: dict = {}
    for srv, rows in (ledger.get("power") or {}).items():
        sv = servers.get(srv)
        if not sv:
            continue
        gpu_vals = sorted(r["gpu_w"] for r in rows if r.get("gpu_w") is not None)
        gpu_idle = gpu_vals[int(len(gpu_vals) * 0.05)] if gpu_vals else 0.0
        by_min = {}
        for r in rows:          # sample windows don't line up with clock minutes: cover every minute they overlap
            for m in range(int(r["t0"] // BUCKET), int((r["t1"] - 1e-6) // BUCKET) + 1):
                by_min.setdefault(m, r)
        power[srv] = {"rows": by_min, "raw": rows, "gpu_idle": gpu_idle}

    server_out: dict = {}
    rate_cache: dict = {}

    def rates(sv, m):
        """The rates applying to a server and model at this moment."""
        key = (sv["id"], m // 60)          # rates are hourly-stable
        if key not in rate_cache:
            rate_cache[key] = fixed_rates(sv, reg, _dt(m * BUCKET), lifespan_override)
        return rate_cache[key]

    for srv, mins in buckets.items():
        sv = servers.get(srv)
        if not sv:
            continue
        pw = sv["power"]
        idle_w = pw.get("idle_w")
        cpu_max = pw.get("cpu_max_w") or 0.0
        gpu_max = pw.get("gpu_max_w") or 0.0
        oh = 1 + (pw.get("measured_overhead_pct") or 0) / 100.0
        threads = _threads(sv)
        parallel = max(1, sv["connection"].get("max_parallel") or 1)
        agg = server_out.setdefault(srv, {"allocated": _new_cost(), "active_minutes": 0, "measured_minutes": 0})
        meas = power.get(srv)
        for m, cells in mins.items():
            r = rates(sv, m)
            H = sum(c[0] for c in cells.values())
            if H <= 0:
                continue
            agg["active_minutes"] += 1
            scale = min(1.0, BUCKET / H)          # concurrent holders split the minute
            day = _dt(m * BUCKET).strftime("%Y-%m-%d")
            # power for this minute
            energy_ok = idle_w is not None and sv["kind"] in ("owned", "leased")
            dyn_w = 0.0
            measured = False
            if energy_ok:
                row = meas["rows"].get(m) if meas else None
                if row is not None:
                    measured = True
                    agg["measured_minutes"] += 1
                    dyn_w = (row.get("cpu_util") or 0.0) * cpu_max
                    if row.get("gpu_w") is not None:
                        dyn_w += max(0.0, row["gpu_w"] - meas["gpu_idle"]) * oh
                else:
                    busy_total = sum(c[1] for c in cells.values())
                    cpu_total = sum(c[2] for c in cells.values())
                    dyn_w = min(1.0, busy_total / (BUCKET * parallel)) * gpu_max + min(1.0, cpu_total / (BUCKET * threads)) * cpu_max
            W = sum(c[1] + c[2] for c in cells.values())
            for act, (held, busy, cpu) in cells.items():
                share = held * scale                   # seconds of the minute this activity is charged for
                c = acts_out[act]["servers"][srv]
                c["held_h"] += held / 3600
                c["busy_h"] += busy / 3600
                c["cpu_h"] += cpu / 3600
                c["depreciation"] += r["depreciation"] * share / 3600
                c["fixed"] += r["fixed"] * share / 3600
                c["hourly"] += r["hourly"] * share / 3600
                if energy_ok:
                    kwh_idle = idle_w * share / 3600 / 1000
                    wshare = (busy + cpu) / W if W > 0 else held / H
                    kwh_dyn = dyn_w * BUCKET / 3600 / 1000 * wshare
                    c["kwh_idle"] += kwh_idle
                    c["kwh_dynamic"] += kwh_dyn
                    if r["kwh"] is None:
                        c["unpriced_kwh"] += kwh_idle + kwh_dyn
                    else:
                        c["energy_idle"] += kwh_idle * r["kwh"]
                        c["energy_dynamic"] += kwh_dyn * r["kwh"]
                    if measured:
                        c["energy_measured_h"] += share / 3600
                    else:
                        c["energy_estimated_h"] += share / 3600
                spent = (r["depreciation"] + r["fixed"] + r["hourly"]) * share / 3600
                if energy_ok and r["kwh"] is not None:
                    spent += (kwh_idle + kwh_dyn) * r["kwh"]
                days[day][srv] += spent
                hours[_dt(m * BUCKET).strftime("%Y-%m-%d %H:00")][srv] += spent

    # ---- roll up: per activity totals, per server allocated, per day energy/tokens
    for act, a in acts_out.items():
        a["servers"] = {k: dict(v) for k, v in a["servers"].items()}
        a["models"] = {k: dict(v) for k, v in a["models"].items()}
        tot = _new_cost()
        for srv, c in a["servers"].items():
            for k in tot:
                tot[k] += c[k]
            agg = server_out.setdefault(srv, {"allocated": _new_cost(), "active_minutes": 0, "measured_minutes": 0})
            for k in tot:
                agg["allocated"][k] += c[k]
        a["total"] = tot

    # ---- calendar view of each server over the range: what its fixed costs were, allocated or not
    if since and until:
        for sv in reg["servers"]:
            cal = {"depreciation": 0.0, "fixed": 0.0, "hours": (until - since) / 3600}
            t = since
            while t < until:
                step = min(3600.0, until - t)
                r = fixed_rates(sv, reg, _dt(t), lifespan_override)
                cal["depreciation"] += r["depreciation"] * step / 3600
                cal["fixed"] += r["fixed"] * step / 3600
                t += step
            out = server_out.setdefault(sv["id"], {"allocated": _new_cost(), "active_minutes": 0, "measured_minutes": 0})
            out["calendar"] = cal
            out["utilization"] = min(1.0, out["active_minutes"] / 60 / max(1e-9, cal["hours"]))
            meas = power.get(sv["id"])
            if meas and sv["power"].get("idle_w") is not None:
                kwh = 0.0
                cost = 0.0
                for row in meas["raw"]:
                    m = int(row["t0"] // BUCKET)
                    w = sv["power"]["idle_w"] + (row.get("cpu_util") or 0) * (sv["power"].get("cpu_max_w") or 0)
                    if row.get("gpu_w") is not None:
                        w += max(0.0, row["gpu_w"] - meas["gpu_idle"]) * (1 + (sv["power"].get("measured_overhead_pct") or 0) / 100)
                    e = w * (row["t1"] - row["t0"]) / 3600 / 1000
                    kwh += e
                    price = rates(sv, m)["kwh"]
                    cost += e * (price or 0)
                out["metered"] = {"kwh": kwh, "cost": cost, "minutes": len(meas["raw"]), "gpu_idle_w": meas["gpu_idle"]}
    return {"acts": acts_out, "servers": server_out, "days": {d: dict(v) for d, v in days.items()},
            "hours": {h: dict(v) for h, v in hours.items()},
            "notes": sorted(notes), "currency": reg.get("currency", "USD"), "symbol": reg.get("currency_symbol", "$")}


def hourly_profile(server: dict, reg: dict, when: Optional[datetime] = None, busy_fraction: float = 1.0) -> dict:
    """What an hour on this server costs at `when` with the model busy `busy_fraction` of the time (for what-if
    comparisons): fixed + depreciation + usage rate + estimated energy."""
    when = when or datetime.now()
    r = fixed_rates(server, reg, when)
    pw = server["power"]
    energy = None
    if pw.get("idle_w") is not None and server["kind"] in ("owned", "leased"):
        watts = pw["idle_w"] + busy_fraction * (pw.get("gpu_max_w") or 0) + 0.1 * (pw.get("cpu_max_w") or 0)
        energy = watts / 1000 * (r["kwh"] or 0)
    return {"depreciation": r["depreciation"], "fixed": r["fixed"], "hourly": r["hourly"], "energy": energy,
            "total": r["depreciation"] + r["fixed"] + r["hourly"] + (energy or 0), "kwh_price": r["kwh"]}


def price_tokens(price: dict, toks: dict) -> float:
    """What a number of tokens costs at a model's prices."""
    return (toks.get("in", 0) * price["input"] + toks.get("out", 0) * price["output"]
            + toks.get("cr", 0) * price.get("cache_read", 0) + toks.get("cw", 0) * price.get("cache_write", 0)) / 1e6
