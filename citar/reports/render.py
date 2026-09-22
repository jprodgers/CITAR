"""Turns gathered report data into one self-contained HTML page (inline CSS and SVG, no scripts or network), readable
offline, printable, and themed for light and dark."""
from __future__ import annotations

import html
from collections import Counter, defaultdict
from datetime import datetime

from .. import costing, servers as S
from . import analysis as A, charts as C
from .data import config_rollup

SECTION_TITLES = {
    "summary": "Summary", "narrative": "Analysis", "costs": "Where the money went", "cost_per_unit": "Cost per unit of work",
    "efficiency": "Energy and efficiency", "servers": "Servers", "server_trend": "Costs over time", "models": "Model comparison", "benchmarks": "Benchmark runs",
    "probes": "Scenario probes", "behavior": "Model behavior", "lab": "Bot lab experiments", "whatif": "What if it ran elsewhere",
    "depreciation": "Hardware lifespan sensitivity", "hardware": "Hardware", "data_quality": "Data quality",
    "activities": "All activities", "methodology": "How costs are calculated",
}

CSS = """
:root{color-scheme:light;--surface:#fcfcfb;--page:#f9f9f7;--ink:#0b0b0b;--ink2:#52514e;--muted:#898781;--grid:#e1e0d9;
--axis:#c3c2b7;--ring:rgba(11,11,11,.10);--good:#006300;--warn:#9a5b00;--bad:#b42323;--accent:#2a78d6;
--s1:#2a78d6;--s2:#eb6834;--s3:#1baf7a;--s4:#eda100;--s5:#e87ba4;--s6:#008300;--s7:#4a3aa7;--s8:#e34948}
@media (prefers-color-scheme:dark){:root:where(:not([data-theme="light"])){color-scheme:dark;--surface:#1a1a19;--page:#0d0d0d;
--ink:#fff;--ink2:#c3c2b7;--muted:#898781;--grid:#2c2c2a;--axis:#383835;--ring:rgba(255,255,255,.10);--good:#0ca30c;
--warn:#fab219;--bad:#e66767;--accent:#3987e5;--s1:#3987e5;--s2:#d95926;--s3:#199e70;--s4:#c98500;--s5:#d55181;--s6:#008300;
--s7:#9085e9;--s8:#e66767}}
:root[data-theme="dark"]{color-scheme:dark;--surface:#1a1a19;--page:#0d0d0d;--ink:#fff;--ink2:#c3c2b7;--muted:#898781;
--grid:#2c2c2a;--axis:#383835;--ring:rgba(255,255,255,.10);--good:#0ca30c;--warn:#fab219;--bad:#e66767;--accent:#3987e5;
--s1:#3987e5;--s2:#d95926;--s3:#199e70;--s4:#c98500;--s5:#d55181;--s6:#008300;--s7:#9085e9;--s8:#e66767}
*{box-sizing:border-box}body{margin:0;background:var(--page);color:var(--ink);font:14px/1.5 system-ui,-apple-system,"Segoe UI",sans-serif}
main{max-width:1000px;margin:0 auto;padding:24px 16px 64px}
header.rh h1{font-size:26px;margin:0 0 4px}header.rh .meta{color:var(--ink2);font-size:13px}
nav.toc{margin:16px 0;display:flex;flex-wrap:wrap;gap:6px}nav.toc a{color:var(--ink2);text-decoration:none;border:1px solid var(--ring);
border-radius:999px;padding:2px 10px;font-size:12px}nav.toc a:hover{color:var(--ink);border-color:var(--axis)}
section{background:var(--surface);border:1px solid var(--ring);border-radius:10px;padding:18px 20px;margin:16px 0}
section h2{font-size:18px;margin:0 0 10px}section h3{font-size:15px;margin:18px 0 6px}
.lead{color:var(--ink2);margin:0 0 10px}
.findings{list-style:none;padding:0;margin:8px 0}.findings li{padding:6px 10px 6px 30px;border-radius:6px;margin:4px 0;position:relative}
.findings li::before{position:absolute;left:10px;top:6px;font-weight:700}
.findings li.info::before{content:"ℹ";color:var(--accent)}.findings li.good::before{content:"✓";color:var(--good)}
.findings li.warn::before{content:"!";color:var(--warn)}
.tiles{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:10px;margin:8px 0 12px}
.tile{border:1px solid var(--ring);border-radius:8px;padding:10px 12px}.tl{color:var(--ink2);font-size:12px}
.tv{font-size:22px;font-weight:600}.tn{color:var(--muted);font-size:12px}
figure.chart{margin:12px 0 18px}figcaption b{font-size:14px}.fig-sub{color:var(--ink2);font-size:12px}
.svg{width:100%;height:auto;display:block;margin-top:6px}
.svg text{font:12px system-ui,-apple-system,"Segoe UI",sans-serif}.svg .tick{fill:var(--muted);font-variant-numeric:tabular-nums}
.svg .lab{fill:var(--ink2)}.svg .val{fill:var(--ink)}.svg .grid{stroke:var(--grid);stroke-width:1}
.svg .axis{stroke:var(--axis);stroke-width:1}.svg .ln{stroke-width:2;stroke-linejoin:round;stroke-linecap:round}
.svg .dot{stroke:var(--surface);stroke-width:2}.svg .dim{opacity:.35}
.svg .frontier{fill:none;stroke:var(--muted);stroke-width:1;stroke-dasharray:4 3}
.svg path:hover,.svg circle:hover{opacity:.85}
.legend{display:flex;flex-wrap:wrap;gap:4px 14px;margin:6px 0 0;font-size:12px;color:var(--ink2)}
.legend i{display:inline-block;margin-right:5px;vertical-align:middle}.legend i.swatch{width:10px;height:10px;border-radius:2px}
.legend i.line{width:14px;height:2px}.legend i.dotk{width:9px;height:9px;border-radius:50%}
details.data{margin-top:4px;font-size:12px}details.data summary{color:var(--muted);cursor:pointer}
details.data table,table.rt{border-collapse:collapse;width:100%;font-size:12.5px}
.tbl-wrap{overflow-x:auto;margin:8px 0}
table.rt th,table.rt td,details.data th,details.data td{text-align:left;padding:5px 8px;border-bottom:1px solid var(--grid);
vertical-align:top;font-variant-numeric:tabular-nums}
table.rt th,details.data th{color:var(--ink2);font-weight:600;white-space:nowrap}
.meter{display:inline-block;width:70px;height:6px;border-radius:3px;background:var(--grid);vertical-align:middle;overflow:hidden}
.meter span{display:block;height:100%;background:var(--accent)}
.good{color:var(--good)}.warn{color:var(--warn)}.bad{color:var(--bad)}.muted{color:var(--muted)}
.narr p{margin:0 0 10px}.pill{display:inline-block;border:1px solid var(--ring);border-radius:999px;padding:0 8px;font-size:11px;color:var(--ink2)}
code{font-size:12px}footer{color:var(--muted);font-size:12px;text-align:center;margin-top:24px}
@media print{body{background:#fff}section{break-inside:avoid;border-color:#ddd}nav.toc{display:none}}
"""


def esc(s) -> str:
    """Escape text for HTML. Everything that reaches a report goes through here."""
    return html.escape(str(s if s is not None else ""), quote=True)


def _date(ts) -> str:
    """A timestamp as a readable date."""
    return datetime.fromtimestamp(ts).strftime("%Y-%m-%d %H:%M") if ts else "–"


def _dur(h: float) -> str:
    """A number of hours as a readable duration."""
    if h is None:
        return "–"
    s = h * 3600
    if s < 90:
        return f"{s:.0f}s"
    if s < 5400:
        return f"{s / 60:.0f} min"
    return f"{h:.1f} h"


def _findings_html(items: list[dict]) -> str:
    """The computed findings as a list.

    These never need a model: they are derived from the data, and they are what makes a report without
    a narrative still worth reading.
    """
    if not items:
        return ""
    return '<ul class="findings">' + "".join(f'<li class="{f["level"]}">{esc(f["text"])}</li>' for f in items) + "</ul>"


class Ctx:
    """Everything a report section needs: the gathered data, the costs, and the formatting."""
    def __init__(self, data: dict):
        self.data = data
        self.sym = data["symbol"]
        self.roll = config_rollup(data)
        self.findings = A.findings(data, self.roll)
        self.config_order = sorted(self.roll, key=lambda k: -self.roll[k]["cost"])
        self.color_of = {k: i for i, k in enumerate(self.config_order)}

    def money(self, v):
        """A number as money, in the configured currency."""
        return A.money(v, self.sym)

    def mfmt(self, v):
        """Money formatted compactly, for chart labels and table cells."""
        return A.money(v, self.sym)

    def f(self, section):
        """The findings belonging to one section."""
        return _findings_html([x for x in self.findings if x["section"] == section])


# ---------------------------------------------------------------------------- sections
def s_summary(x: Ctx) -> str:
    """The summary section: what was measured, what it cost, and the key findings."""
    d = x.data
    acts = d["acts"]
    total = sum(a["total"] for a in acts)
    games = sum(c["games"] for c in x.roll.values())
    ai_games = sum(c["games"] for k, c in x.roll.items() if not k.startswith(("bots", "report writing")))
    turns = sum(c["model_turns"] for c in x.roll.values())
    toks = sum(c["in"] + c["out"] + c["cr"] + c["cw"] for c in x.roll.values())
    kwh = sum(c["kwh"] for c in x.roll.values())
    kinds = Counter(a["kind"] for a in acts)
    tiles = [{"label": "Total cost", "value": x.money(total), "note": "full energy basis" if d["basis"] == "full" else "work energy only"},
             {"label": "Activities", "value": f"{len(acts):,}", "note": ", ".join(f"{v} {k}" for k, v in kinds.most_common())},
             {"label": "Games with AI models", "value": f"{ai_games:,}", "note": f"{turns:,} model turns" + (f" · plus {games - ai_games} bot-only lab games" if games > ai_games else "")},
             {"label": "Tokens", "value": C.compact(toks), "note": "input + output + cache"},
             {"label": "Energy", "value": f"{kwh:,.2f} kWh" if kwh >= 1 else f"{kwh * 1000:,.1f} Wh", "note": None}]
    if turns:
        tiles.append({"label": "Cost per model turn", "value": x.money(total / turns)})
    return C.stat_tiles(tiles) + _findings_html(x.findings[:12] if len(x.findings) <= 12 else
                                                 [f for f in x.findings if f["section"] in ("summary", "models", "cost_per_unit")][:10])


def s_costs(x: Ctx) -> str:
    """Where the money went, by configuration, server and activity type."""
    d = x.data
    comps = [c for c in A.COMPONENTS if d["basis"] == "full" or c[0] != "energy_idle"]
    keys = [k for k, _ in comps]
    names = [n for _, n in comps]
    used = [i for i, k in enumerate(keys) if any(a["cost"][k] > 0 for a in d["acts"])]
    keys, names = [keys[i] for i in used], [names[i] for i in used]
    out = [x.f("costs")]
    if not keys:
        return out[0] + '<p class="muted">No costs were recorded (check the Data quality section).</p>'
    cats = x.config_order
    vals = [[sum(a["cost"][k] for a in d["acts"] if a["config"] == c) for k in keys] for c in cats]
    out.append(C.stacked_hbar("Cost by configuration", cats, names, vals, fmt=x.mfmt,
                              subtitle="Each model on its server; bars split by cost component"))
    by_srv = defaultdict(lambda: [0.0] * len(keys))
    for a in d["acts"]:
        for srv, sc in a["servers"].items():
            sv = d["servers_by_id"].get(srv)
            name = sv["name"] if sv else srv
            for i, k in enumerate(keys):
                by_srv[name][i] += sc[k]
    srv_names = sorted(by_srv, key=lambda n: -sum(by_srv[n]))
    out.append(C.stacked_hbar("Cost by server", srv_names, names, [by_srv[n] for n in srv_names], fmt=x.mfmt,
                              subtitle="The host PC carries the game engine and scripted bots; model servers carry the AI"))
    by_kind = Counter()
    for a in d["acts"]:
        by_kind[a["kind"]] += a["total"]
    if len(by_kind) > 1:
        out.append(C.hbar("Cost by activity type", sorted(by_kind.items(), key=lambda kv: -kv[1]), fmt=x.mfmt))
    return "".join(out)


def s_cost_per_unit(x: Ctx) -> str:
    """Cost per unit of work: per game, per model turn, per million tokens, per win."""
    rows = []
    for k in x.config_order:
        c = x.roll[k]
        rows.append([k, x.money(c["cost"]), c["games"] or "–", x.money(c["per_game"]), c["model_turns"] or "–",
                     x.money(c["per_turn"]), x.money(c["per_mtok"]), x.money(c["per_point"]), x.money(c["per_win"]),
                     x.money(c["per_case"])])
    head = ["Configuration", "Total", "Games", "Per game", "Model turns", "Per model turn", "Per 1M tokens",
            "Per performance point", "Per win", "Per probe case"]
    items = [(k, x.roll[k]["per_turn"]) for k in x.config_order if x.roll[k].get("per_turn")]
    chart = C.hbar("Cost per model turn", sorted(items, key=lambda kv: kv[1]), fmt=x.mfmt,
                   color_index=x.color_of) if len(items) > 1 else ""
    return x.f("cost_per_unit") + C.table(head, rows) + chart


def s_efficiency(x: Ctx) -> str:
    """Energy per unit of work, per model on each machine: the compute-per-watt comparison."""
    def n(v, fmt):
        return fmt.format(v) if v is not None else "–"
    rows, items = [], []
    for k in x.config_order:
        c = x.roll[k]
        if not c["kwh"]:
            continue
        rows.append([k, f"{c['kwh']:.3f}", x.money(c["energy_cost"]), n(c["wh_per_game"], "{:,.0f}"), n(c["wh_per_turn"], "{:,.1f}"),
                     n(c["wh_per_case"], "{:,.1f}"), n(c["out_per_wh"], "{:,.0f}"), n(c["out_per_s"], "{:,.1f}"),
                     n(c["perf_per_kwh"], "{:,.1f}"), x.money(c["per_game"]), x.money(c["per_turn"]), x.money(c["per_case"]),
                     "measured" if (c.get("measured_share") or 0) > 0.5 else "estimated"])
        if c["out_per_wh"]:
            items.append((k, c["out_per_wh"]))
    if not rows:
        return (x.f("efficiency") + '<p class="lead">No energy recorded in scope: give the machines power figures '
                '(Servers page → Power &amp; costs) to see it.</p>')
    head = ["Configuration", "kWh", "Electricity", "Wh per game", "Wh per model turn", "Wh per probe case",
            "Output tokens per Wh", "Output tokens / s", "Performance per kWh", "Cost per game", "Cost per model turn",
            "Cost per probe case", "Energy is"]
    chart = C.hbar("Output tokens per watt-hour (higher is better)", sorted(items, key=lambda kv: -kv[1]),
                   fmt=lambda v: f"{v:,.0f}", color_index=x.color_of) if len(items) > 1 else ""
    return (x.f("efficiency") + '<p class="lead">Energy is the whole machine at the wall: idle power for the time the '
            'work held the machine, plus the extra drawn while the model generated. Cost columns include hardware wear '
            '(depreciation) and fixed charges as well as electricity.</p>' + C.table(head, rows) + chart)


def s_servers(x: Ctx) -> str:
    """Per-server costs, utilisation and metered energy, including unallocated fixed costs."""
    d = x.data
    rows = []
    for sv in d["registry"]["servers"]:
        o = d["cost"]["servers"].get(sv["id"]) or {}
        alloc = o.get("allocated") or costing._new_cost()
        cal = o.get("calendar") or {}
        fixed_cal = (cal.get("depreciation") or 0) + (cal.get("fixed") or 0)
        alloc_fixed = alloc["depreciation"] + alloc["fixed"]
        used = costing.total(alloc, d["basis"])
        if not used and not fixed_cal and not alloc["held_h"]:
            continue
        util = o.get("utilization")
        met = o.get("metered")
        rows.append([sv["name"], sv["kind"], x.money(used), _dur(alloc["held_h"]) if alloc["held_h"] else "–",
                     x.money(used / alloc["held_h"]) if alloc["held_h"] else "–", x.money(fixed_cal) if fixed_cal else "–",
                     x.money(max(0.0, fixed_cal - alloc_fixed)) if fixed_cal else "–",
                     C.meter(util, f"{100 * util:.1f}%") if util is not None else "–",
                     f"{alloc['kwh_idle'] + alloc['kwh_dynamic']:.2f}",
                     f"{met['kwh']:.2f} kWh / {x.money(met['cost'])}" if met else "–"])
    head = ["Server", "Kind", "Allocated to activities", "Time held", "Per hour held", "Fixed costs over the period", "Unallocated fixed",
            "In use", "kWh to activities", "Metered machine total"]
    return (x.f("servers") + '<p class="lead">Fixed costs accrue by the calendar; activities pay for the time they held a '
            'server, and the rest is unallocated idle time. "Metered" covers minutes with live power samples.</p>'
            + C.table(head, rows))


def s_server_trend(x: Ctx) -> str:
    """How each server's costs moved over the period."""
    d = x.data
    days = d["cost"]["days"]
    if not days:
        return '<p class="muted">No costs in this period.</p>'
    unit = "Daily"
    if len(days) < 3 and d["cost"].get("hours"):
        # a short period: hour by hour, with the empty hours in between shown as zero
        from datetime import datetime as _d, timedelta as _td
        hrs = d["cost"]["hours"]
        first, last = _d.strptime(min(hrs), "%Y-%m-%d %H:%M"), _d.strptime(max(hrs), "%Y-%m-%d %H:%M")
        full = {}
        t = first
        while t <= last:
            k = t.strftime("%Y-%m-%d %H:00")
            full[k] = hrs.get(k, {})
            t += _td(hours=1)
        days = {k[5:]: v for k, v in full.items()}
        unit = "Hourly"
    dates = sorted(days)
    srvs = sorted({s for v in days.values() for s in v}, key=lambda s: -sum(days[dd].get(s, 0) for dd in dates))
    names = {s: (d["servers_by_id"].get(s) or {}).get("name", s) for s in srvs}
    series = {names[s]: [days[dd].get(s, 0.0) for dd in dates] for s in srvs[:8]}
    cum = {}
    for n, vals in series.items():
        run, acc = 0.0, []
        for v in vals:
            run += v
            acc.append(run)
        cum[n] = acc
    out = (C.line(f"{unit} cost per server", dates, series, fmt=x.mfmt, subtitle="Allocated costs (fixed share, usage rate, energy, tokens)")
           + C.line("Cumulative cost per server", dates, cum, fmt=x.mfmt, area=len(cum) == 1))
    months = sorted({dd[:7] for dd in dates})
    if unit == "Daily" and (len(months) > 1 or len(dates) > 31):
        rows = [[names[s]] + [x.money(sum(v for dd, day in days.items() if dd[:7] == mo for k, v in day.items() if k == s)) for mo in months]
                for s in srvs]
        out += "<h3>By month</h3>" + C.table(["Server"] + months, rows)
    return out


def s_models(x: Ctx) -> str:
    """Model comparison: performance with confidence intervals, speed, reliability and cost.

    The section people read first, and the one most likely to be over-read. It shows confidence
    intervals and flags small samples because two games is not a result, however clean the table looks.
    """
    rows, pts = [], []
    for k in x.config_order:
        c = x.roll[k]
        if k.startswith(("bots", "report writing")):
            continue
        m, ci = A.mean_ci(c["perf"])
        rows.append([k, c["games"], f"{m:.0f}" + (f" ± {ci:.0f}" if ci else "") if m is not None else "–", c["wins"] or "–",
                     f"{c['avg_turn']:.0f}s" if c["avg_turn"] is not None else "–",
                     f"{100 * c['err']:.1f}%" if c["err"] is not None else "–",
                     f"{100 * c['clean']:.0f}%" if c["clean"] is not None else "–",
                     C.compact((c["in"] + c["cr"] + c["cw"]) / c["model_turns"]) if c["model_turns"] else "–",
                     C.compact(c["out"] / c["model_turns"]) if c["model_turns"] else "–",
                     x.money(c["per_turn"])])
        if m is not None and c.get("per_turn") is not None and not c.get("incomplete"):
            pts.append({"name": k, "x": c["per_turn"], "y": m})
    if not rows:
        return '<p class="muted">No AI model activity in this scope.</p>'
    head = ["Configuration", "Games", "Performance (0–100)", "Wins", "Avg turn", "Tool error rate", "Clean turn ends",
            "Prompt tokens / turn", "Output tokens / turn", "Cost / model turn"]
    out = [x.f("models"), '<p class="lead">Performance is the benchmark measure: 100 for a win, 0 if eliminated, otherwise the '
           'model\'s share of the score against the strongest bot (50 = level). ± is a 95% confidence interval.</p>',
           C.table(head, rows)]
    if len(pts) >= 2:
        front = A.pareto(pts, "x", "y")
        skipped = [k for k in x.config_order if x.roll[k].get("incomplete") and x.roll[k]["perf"]]
        out.append(C.scatter("Performance vs cost per model turn", pts, "Cost per model turn", "Performance", x.mfmt,
                             lambda v: f"{v:.0f}", subtitle="Up and to the left is better; the dashed line joins the "
                             "configurations nobody beats on both" + (f". Not shown (costs incomplete): {', '.join(skipped)}" if skipped else ""),
                             frontier=front))
    perf = [(k, x.roll[k]["perf_mean"]) for k in x.config_order if x.roll[k]["perf_mean"] is not None and not k.startswith("bots")]
    if len(perf) >= 2:
        out.append(C.hbar("Mean game performance", sorted(perf, key=lambda kv: -kv[1]), fmt=lambda v: f"{v:.0f}",
                          color_index=x.color_of))
    return "".join(out)


def s_benchmarks(x: Ctx) -> str:
    """Benchmark runs and their outcomes."""
    d = x.data
    if not d["benchmark_runs"]:
        return '<p class="muted">No benchmark runs in this scope.</p>'
    cost_by_job = {a["ref"].get("job_id"): a for a in d["acts"] if a["ref"].get("job_id")}
    out = []
    for run in d["benchmark_runs"]:
        rows = []
        for j in run["jobs"]:
            p = j.get("result") or j.get("progress") or {}
            a = cost_by_job.get(j["id"])
            rows.append([j.get("label") or j["model"], j.get("server_name") or "", j.get("scenario_name"),
                         j["status"] + (f" · {p['outcome']}" if p.get("outcome") else ""),
                         f"{max(0, (p.get('turn') or 1) - 1)}" + (f" / {p['turn_limit']}" if p.get("turn_limit") else ""),
                         f"{p['performance']:.0f}" if p.get("performance") is not None else "–",
                         f"{p['avg_turn_s']:.0f}s" if p.get("avg_turn_s") else "–",
                         x.money(a["total"]) if a else "–"])
        sm = run["summary"]
        out.append(f'<h3>{esc(run["name"])} <span class="pill">{esc(run["status"])}</span></h3>'
                   f'<p class="lead">Started {_date(run.get("created"))} · {sm["turns_done"]:,} of {sm["turns_total"]:,} turns · '
                   f'{len(run["jobs"])} games</p>')
        out.append(C.table(["Model", "Server", "Scenario", "Status", "Turns", "Performance", "Avg turn", "Cost"], rows))
    return "".join(out)


def s_probes(x: Ctx) -> str:
    """Probe runs: outcome per case and model, and pass rates."""
    d = x.data
    if not d["probe_runs"]:
        return '<p class="muted">No probe runs in this scope.</p>'
    out = [x.f("probes")]
    by_probe = defaultdict(list)
    for r in d["probe_runs"]:
        by_probe[(r["probe"]["id"], r["probe"]["name"])].append(r)
    cost_by_run = {a["ref"].get("probe_run"): a for a in d["acts"] if a["ref"].get("probe_run")}
    for (pid, pname), runs in by_probe.items():
        cases = [c["id"] for c in runs[0]["probe"]["cases"]]
        expect = {c["id"]: c.get("expect") for c in runs[0]["probe"]["cases"]}
        cols = [r["name"].split(" · ", 1)[-1] for r in runs]
        rows = []
        for cid in cases:
            row = [cid, expect.get(cid) or "–"]
            for r in runs:
                res = [z for z in r["results"] if z["case"] == cid]
                if not res:
                    row.append("–")
                    continue
                outs = Counter(z.get("outcome") or "?" for z in res)
                txt = ", ".join(f"{k}×{v}" if v > 1 else k for k, v in outs.most_common())
                passed = [z.get("passed") for z in res if z.get("passed") is not None]
                mark = "" if not passed else (" ✓" if all(passed) else " ✗" if not any(passed) else " ~")
                row.append(txt + mark)
            rows.append(row)
        out.append(f"<h3>{esc(pname)}</h3>")
        out.append(C.table(["Case", "Expected"] + cols, rows))
        rates = []
        for r, col in zip(runs, cols):
            sm = r.get("summary") or {}
            a = cost_by_run.get(r["id"])
            rates.append([col, len(r["results"]), f"{100 * sm['pass_rate']:.0f}%" if sm.get("pass_rate") is not None else "–",
                          f"{sum(z.get('seconds') or 0 for z in r['results']) / max(1, len(r['results'])):.0f}s",
                          x.money(a["total"]) if a else "–",
                          x.money(a["total"] / len(r["results"])) if a and r["results"] else "–"])
        out.append(C.table(["Run", "Cases run", "Pass rate", "Avg time / case", "Cost", "Cost / case"], rates))
    return "".join(out)


def s_behavior(x: Ctx) -> str:
    """How models behaved: tool mix, how turns ended, and the most common errors."""
    d = x.data
    tools = defaultdict(Counter)
    reasons = defaultdict(Counter)
    for a in d["acts"]:
        g = a.get("game") or {}
        for seat in g.get("seats") or []:
            m = seat.get("metrics") or {}
            for t, v in (m.get("tools") or {}).items():
                tools[a["config"]][t] += v.get("count", 0)
            for r, n in (m.get("end_reasons") or {}).items():
                reasons[a["config"]][r] += n
    for r in d["probe_runs"]:
        cfg = next((a["config"] for a in d["acts"] if a["ref"].get("probe_run") == r["id"]), r["name"])
        for z in r["results"]:
            for c in z.get("tool_calls") or []:
                tools[cfg][c["tool"]] += 1
    out = [x.f("behavior")]
    if not tools and not reasons:
        return out[0] + '<p class="muted">No model behavior recorded in this scope.</p>'
    all_tools = Counter()
    for c in tools.values():
        all_tools.update(c)
    top = [t for t, _ in all_tools.most_common(8)]
    cfgs = [k for k in x.config_order if k in tools]
    if cfgs and top:
        vals = [[tools[k][t] / max(1, sum(tools[k].values())) * 100 for t in top] for k in cfgs]
        out.append(C.stacked_hbar("Tool mix (share of calls)", cfgs, top, vals, fmt=lambda v: f"{v:.0f}%",
                                  subtitle="The 8 most used tools across all configurations"))
    if reasons:
        rs = sorted({r for c in reasons.values() for r in c})
        rows = [[k] + [f"{100 * reasons[k][r] / max(1, sum(reasons[k].values())):.0f}%" for r in rs] for k in x.config_order if k in reasons]
        out.append("<h3>How turns ended</h3>" + C.table(["Configuration"] + rs, rows))
    errs = []
    for a in d["acts"]:
        for seat in (a.get("game") or {}).get("seats") or []:
            for e in (seat.get("metrics") or {}).get("top_errors") or []:
                errs.append([a["config"], e["error"], e["count"]])
    if errs:
        errs.sort(key=lambda r: -r[2])
        out.append("<h3>Most common tool errors</h3>" + C.table(["Configuration", "Error", "Count"], errs[:15]))
    return "".join(out)


def s_lab(x: Ctx) -> str:
    """Bot lab experiments and their results."""
    d = x.data
    if not d["lab"]:
        return '<p class="muted">No lab experiments in this scope.</p>'
    out = []
    cost_by_exp = defaultdict(float)
    games_by_exp = Counter()
    for a in d["acts"]:
        if a["kind"] == "lab":
            cost_by_exp[a["parent"].get("id")] += a["total"]
            games_by_exp[a["parent"].get("id")] += 1
    rows = [[e, v.get("games", 0), games_by_exp[e], x.money(cost_by_exp[e]),
             x.money(cost_by_exp[e] / games_by_exp[e]) if games_by_exp[e] else "–",
             f"{(v.get('summary') or {}).get('avg_minutes') or '–'} min"] for e, v in sorted(d["lab"].items())]
    out.append(C.table(["Experiment", "Games in results", "Games costed", "Cost", "Cost / game", "Avg game length"], rows))
    for e, v in sorted(d["lab"].items()):
        labels = (v.get("summary") or {}).get("labels") or {}
        if not labels:
            continue
        lrows = [[lab, s.get("seats"), f"{s.get('win_share', 0):.2f}",
                  f"{s.get('score_share', 0):.3f}" + (f" ± {s['score_share_ci']:.3f}" if s.get("score_share_ci") else ""),
                  s.get("mean_rank"), s.get("final_cities"), s.get("final_techs")] for lab, s in labels.items()]
        out.append(f"<h3>{esc(e)}</h3>" + C.table(["Seat label", "Seats", "Win share", "Score share", "Mean rank", "Cities", "Techs"], lrows))
    return "".join(out)


def s_whatif(x: Ctx) -> str:
    """What the same work would have cost elsewhere: other APIs' prices, other machines' hours."""
    out = ['<p class="lead">The same work priced elsewhere. Token repricing is exact for the tokens used (different models '
           'tokenize differently, so treat it as indicative). Hour repricing assumes the other machine runs at the same '
           'speed, which it won\'t: it bounds the cost of the time, not the work.</p>']
    tok = A.whatif_tokens(x.data, x.roll)
    if tok:
        alts = list(tok[0]["alt"])
        rows = [[r["config"], x.money(r["actual"])] + [x.money(r["alt"][a]) for a in alts] for r in tok]
        out.append("<h3>Tokens at API list prices</h3>" + C.table(["Configuration", "Actual cost"] + alts, rows))
    hrs = A.whatif_hours(x.data, x.roll)
    if hrs:
        alts = list(hrs[0]["alt"])
        rows = [[r["config"], _dur(r["hours"]), x.money(r["actual"])] + [x.money(r["alt"][a]) for a in alts] for r in hrs]
        out.append("<h3>Server time at each machine's full hourly cost</h3>" +
                   C.table(["Configuration", "Hours held", "Actual cost"] + alts, rows))
    if len(out) == 1:
        out.append('<p class="muted">Nothing to reprice in this scope.</p>')
    return "".join(out)


def s_depreciation(x: Ctx) -> str:
    """How sensitive the costs are to the hardware lifespans assumed."""
    sens = x.data.get("sensitivity")
    if not sens:
        return '<p class="muted">No sensitivity data.</p>'
    ys = sorted(sens)
    return (x.f("depreciation") + C.columns("Cost of these activities by assumed hardware lifespan", [f"{y} years" for y in ys],
                                           [sens[y]["allocated"] for y in ys], fmt=x.mfmt,
                                           subtitle="Every owned component depreciated over the given lifespan instead of its own setting")
            + C.table(["Lifespan", "Total cost", "Of which depreciation"],
                      [[f"{y} years", x.money(sens[y]["allocated"]), x.money(sens[y]["depreciation"])] for y in ys]))


def s_hardware(x: Ctx) -> str:
    """The machines involved, and what each is."""
    d = x.data
    used = {s for a in d["acts"] for s in a["servers"]} or {s["id"] for s in d["registry"]["servers"]}
    rows = []
    for sv in d["registry"]["servers"]:
        if sv["id"] not in used:
            continue
        hw = sv.get("hardware") or {}
        pw = sv["power"]
        comps = "; ".join(f"{c['name']} {x.money(c['price']) if c.get('price') is not None else '(no price)'}"
                          f"{' bought ' + c['purchased'] if c.get('purchased') else ''}, {c['lifespan_years']:g} y"
                          for c in sv.get("components") or []) or "–"
        rows.append([sv["name"], sv["kind"], S.hardware_summary(hw) if sv["kind"] != "api" else "API service",
                     f"idle {pw['idle_w']:g} W, CPU +{pw.get('cpu_max_w') or 0:g} W, GPU +{pw.get('gpu_max_w') or 0:g} W ({pw['source']})"
                     if pw.get("idle_w") is not None else "–", comps])
    return C.table(["Server", "Kind", "Hardware", "Power", "Components"], rows)


def s_data_quality(x: Ctx) -> str:
    """What in this report is measured and what is estimated.

    Not decoration. It names the energy figures that were estimated rather than metered, the cost
    periods with no price set, and the comparisons resting on too few games - so a report that looks
    precise and rests on a guessed electricity rate says so.
    """
    d = x.data
    items = [f for f in x.findings if f["section"] == "data_quality"]
    extra = [f"The ledger has {d['ledger_rows']:,} usage rows in this period."]
    ms = [c["measured_share"] for c in x.roll.values() if c.get("measured_share") is not None]
    if ms:
        extra.append(f"Energy measured live for {100 * sum(ms) / len(ms):.0f}% of server time on average (the rest estimated).")
    return _findings_html(items) + "".join(f'<p class="muted">{esc(e)}</p>' for e in extra)


def s_activities(x: Ctx) -> str:
    """Every activity in scope, itemised."""
    rows = []
    for a in sorted(x.data["acts"], key=lambda a: -(a["start"] or 0))[:500]:
        held = sum(sc.get("held_h", 0) for sc in a["servers"].values())
        busy = sum(sc.get("busy_h", 0) for sc in a["servers"].values())
        g = a.get("game") or {}
        outcome = g.get("phase") if g else a["info"].get("status") or ""
        rows.append([_date(a["start"]), a["kind"], a["name"], a["config"], _dur(held), _dur(busy), outcome or "",
                     x.money(a["total"])])
    note = '<p class="muted">Newest first; the first 500 shown.</p>' if len(x.data["acts"]) > 500 else ""
    return C.table(["Started", "Type", "Name", "Configuration", "Server time", "Model busy", "State", "Cost"], rows) + note


def s_methodology(x: Ctx) -> str:
    """How each number was arrived at."""
    return ('<div class="narr">'
            '<p><b>Fixed costs</b> are charged by the calendar. A server\'s hourly fixed rate is its depreciation (each owned '
            'component\'s price less resale value, straight-line over its lifespan from its purchase date) plus its fixed '
            'monthly costs (lease, maintenance, API subscription, and a share of the electricity bill\'s fixed fee if you '
            'chose to allocate it that way) divided by the hours in that month. An activity pays that rate for the time it '
            'held the server; activities holding it at the same moment split it. Time nobody held it is reported as '
            'unallocated rather than spread onto the work.</p>'
            '<p><b>Rented machines</b>\' hourly usage rate is charged the same way, for the time in use.</p>'
            '<p><b>Energy</b> is worked out minute by minute. Where CITAR sampled the machine (GPU watts from nvidia-smi and '
            'CPU utilisation), the draw is measured; otherwise it is estimated from what CITAR had the machine doing (seconds '
            'the model spent generating, CPU seconds of the game engine and bots) and the server\'s idle / CPU / GPU watt '
            'settings. Idle power is split like fixed costs; the power above idle goes to whichever activities did work that '
            'minute, in proportion to it. The price per kWh is the electricity plan\'s rate in force at that time (flat, '
            'time-of-use or tiered), plus the fixed fee spread over the household\'s monthly kWh if chosen.</p>'
            '<p><b>API tokens</b> are priced at the model\'s per-million rates for input, output, cache reads and cache writes '
            'in force when they were used. With server-side fallbacks the model that actually served a request is billed.</p>'
            f'<p>Every figure uses the configuration in force at the time (cost periods are effective-dated), so correcting a '
            f'rate later corrects every report. Energy basis for this report: <b>{"full draw" if x.data["basis"] == "full" else "work above idle only"}</b>.</p>'
            '</div>')


SECTIONS = {"summary": s_summary, "costs": s_costs, "cost_per_unit": s_cost_per_unit, "efficiency": s_efficiency, "servers": s_servers,
            "server_trend": s_server_trend, "models": s_models, "benchmarks": s_benchmarks, "probes": s_probes,
            "behavior": s_behavior, "lab": s_lab, "whatif": s_whatif, "depreciation": s_depreciation, "hardware": s_hardware,
            "data_quality": s_data_quality, "activities": s_activities, "methodology": s_methodology}


def _inline(text: str) -> str:
    """Escaped text with the light markdown models use: **bold**, *italic*, `code`."""
    import re
    t = esc(text)
    t = re.sub(r"\*\*(.+?)\*\*", r"<b>\1</b>", t)
    t = re.sub(r"(?<![\w*])\*(?!\s)(.+?)(?<!\s)\*(?![\w*])", r"<i>\1</i>", t)
    return re.sub(r"`([^`]+)`", r"<code>\1</code>", t)


def narrative_html(text: str) -> str:
    """The model's narrative (plain text / light markdown) as paragraphs, headings and bullet lists."""
    import re
    out, para, items = [], [], []

    def flush():
        """Emit the accumulated narrative paragraph."""
        if para:
            out.append(f"<p>{_inline(' '.join(para))}</p>")
            para.clear()
        if items:
            out.append("<ul>" + "".join(f"<li>{_inline(i)}</li>" for i in items) + "</ul>")
            items.clear()
    for raw in (text or "").splitlines():
        ln = raw.strip()
        bullet = re.match(r"^(?:[-*•]|\d+[.)])\s+(.*)", ln)
        if not ln:
            flush()
        elif ln.startswith("#"):
            flush()
            out.append(f"<h3>{_inline(ln.lstrip('# '))}</h3>")
        elif bullet:
            if para:
                out.append(f"<p>{_inline(' '.join(para))}</p>")
                para.clear()
            items.append(bullet.group(1))
        else:
            if items:
                flush()
            para.append(ln)
    flush()
    return '<div class="narr">' + "".join(out) + "</div>"


def scope_text(data: dict) -> str:
    """A sentence describing what this report covers."""
    spec = data["spec"]
    sc = spec.get("scope") or {}
    parts = [f"{_date(data['since'])} → {_date(data['until'])}"]
    if sc.get("servers"):
        parts.append("servers: " + ", ".join((data["servers_by_id"].get(s) or {}).get("name", s) for s in sc["servers"]))
    if sc.get("models"):
        parts.append("models: " + ", ".join(sc["models"]))
    if sc.get("kinds"):
        parts.append("types: " + ", ".join(sc["kinds"]))
    if sc.get("items"):
        parts.append(f"{len(sc['items'])} selected item(s)")
    return " · ".join(parts)


def render(data: dict, narrative: str = "", narrative_note: str = "") -> tuple[str, dict]:
    """Returns (html, summary) - summary holds headline numbers for the Reports list."""
    x = Ctx(data)
    spec = data["spec"]
    sections = [s for s in spec.get("sections") or ["summary"] if s in SECTIONS or s == "narrative"]
    if narrative and "narrative" not in sections:
        sections.insert(1 if sections and sections[0] == "summary" else 0, "narrative")
    body, toc = [], []
    for key in sections:
        title = SECTION_TITLES.get(key, key)
        if key == "narrative":
            if not narrative:
                continue
            inner = narrative_html(narrative) + (f'<p class="muted">{esc(narrative_note)}</p>' if narrative_note else "")
        else:
            try:
                inner = SECTIONS[key](x)
            except Exception as e:           # one broken section shouldn't sink the report
                import traceback
                inner = f'<p class="bad">This section failed: {esc(type(e).__name__)}: {esc(e)}</p><pre>{esc(traceback.format_exc(limit=4))}</pre>'
        toc.append(f'<a href="#{key}">{esc(title)}</a>')
        body.append(f'<section id="{key}"><h2>{esc(title)}</h2>{inner}</section>')
    title = spec.get("title") or "CITAR report"
    page = (f'<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">'
            f'<title>{esc(title)}</title><style>{CSS}</style></head><body><main>'
            f'<header class="rh"><h1>{esc(title)}</h1><div class="meta">{esc(scope_text(data))}<br>Generated '
            f'{_date(data["generated"])} by CITAR · currency {esc(data["currency"])}</div></header>'
            f'<nav class="toc">{"".join(toc)}</nav>{"".join(body)}'
            f'<footer>CITAR — Civ Inspired Tool for AI Research · costs from the usage ledger priced with the server registry</footer>'
            f'</main></body></html>')
    total = sum(a["total"] for a in data["acts"])
    summary = {"total": total, "activities": len(data["acts"]), "configs": len(x.roll),
               "games": sum(c["games"] for c in x.roll.values()), "symbol": data["symbol"],
               "findings": [f["text"] for f in x.findings[:6]]}
    return page, summary


def narrative_brief(data: dict) -> str:
    """Compact facts for the model that writes the narrative."""
    x = Ctx(data)
    lines = [f"Report: {data['spec'].get('title') or 'CITAR report'}", f"Scope: {scope_text(data)}",
             f"Currency: {data['currency']} ({data['symbol']}); energy basis: {data['basis']}",
             f"Total cost: {x.money(sum(a['total'] for a in data['acts']))} over {len(data['acts'])} activities.", "",
             "Per configuration (model @ server):"]
    for k in x.config_order:
        c = x.roll[k]
        m, ci = A.mean_ci(c["perf"])
        lines.append(f"- {k}: cost {x.money(c['cost'])}; games {c['games']}; model turns {c['model_turns']}; "
                     f"cost/turn {x.money(c['per_turn'])}; cost/1M tokens {x.money(c['per_mtok'])}; "
                     f"performance {'%.0f' % m if m is not None else 'n/a'}{' ± %.0f' % ci if ci else ''}; wins {c['wins']}; "
                     f"avg turn {('%.0fs' % c['avg_turn']) if c['avg_turn'] else 'n/a'}; tool error rate "
                     f"{('%.1f%%' % (100 * c['err'])) if c['err'] is not None else 'n/a'}; probe pass rate "
                     f"{('%.0f%%' % (100 * c['pass_rate'])) if c['pass_rate'] is not None else 'n/a'}; kWh {c['kwh']:.3f}"
                     + (f"; COSTS INCOMPLETE (no power figures or prices for {', '.join(c['incomplete'])}: its cost is a "
                        f"lower bound and must not be called cheaper)" if c.get("incomplete") else ""))
    lines += ["", "Computed findings:"] + [f"- [{f['section']}] {f['text']}" for f in x.findings]
    return "\n".join(lines)
