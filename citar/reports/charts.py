"""Inline SVG charts for reports: no JavaScript or network needed, themed through CSS custom properties so they follow
the report's light/dark theme. Every mark carries a <title> (hover tooltip) and every chart comes with a data table.

Conventions (see the report stylesheet): series colors are --s1..--s8 in a fixed order (color follows the entity,
never its rank); text uses ink tokens, never series colors; bars are <= 24px thick with 4px rounded data ends; lines
are 2px; markers r=4 with a 2px surface ring; gridlines are 1px hairlines; one y-axis only.
"""
from __future__ import annotations

import html
import math
from typing import Callable, Optional

MAX_SERIES = 8


def esc(s) -> str:
    """Escape text for inclusion in SVG."""
    return html.escape(str(s), quote=True)


def color(i: int) -> str:
    """The nth colour of the chart palette, chosen to stay distinguishable in both themes."""
    return f"var(--s{(i % MAX_SERIES) + 1})"


def nice_ticks(lo: float, hi: float, n: int = 5) -> list[float]:
    """Round axis ticks spanning a range."""
    if hi <= lo:
        hi = lo + 1
    span = hi - lo
    raw = span / max(1, n)
    mag = 10 ** math.floor(math.log10(raw))
    step = next(m * mag for m in (1, 2, 2.5, 5, 10) if m * mag >= raw)
    start = math.floor(lo / step) * step
    ticks = []
    v = start
    while v <= hi + step * 0.5:
        ticks.append(round(v, 10))
        v += step
    return ticks


def compact(v: float, unit: str = "") -> str:
    """A number formatted compactly for an axis or label."""
    if v is None:
        return "–"
    a = abs(v)
    if unit == "$":
        if a >= 1000:
            return f"${v / 1000:,.1f}K"
        if a >= 1:
            return f"${v:,.2f}"
        if a >= 0.01:
            return f"${v:.3f}"
        return f"${v:.4f}" if a else "$0"
    if a >= 1e6:
        return f"{v / 1e6:,.1f}M{unit}"
    if a >= 1e4:
        return f"{v / 1e3:,.1f}K{unit}"
    if a >= 100:
        return f"{v:,.0f}{unit}"
    if a >= 1:
        return f"{v:,.2f}".rstrip("0").rstrip(".") + unit
    return f"{v:.3g}{unit}"


CHAR_PX = 6.7          # average width of a 12px system-ui character


def fit(text: str, px: float) -> str:
    """A label that fits in `px`: drop a model's publisher prefix ("google/"), then cut the middle with an ellipsis."""
    import re
    t = str(text)
    if len(t) * CHAR_PX <= px:
        return t
    t = re.sub(r"(^|[·+] ?)[\w.-]+/", r"\1", t)
    n = int(px / CHAR_PX)
    if len(t) <= n:
        return t
    keep = max(4, n - 1)
    return t[: keep // 2] + "…" + t[len(t) - (keep - keep // 2):]


def _legend(names: list[str], kind: str = "swatch") -> str:
    """A chart legend."""
    if len(names) < 2:
        return ""
    items = "".join(f'<span class="lg"><i class="{kind}" style="background:{color(i)}"></i>{esc(n)}</span>'
                    for i, n in enumerate(names))
    return f'<div class="legend">{items}</div>'


def _table(headers: list[str], rows: list[list]) -> str:
    """The data table shown under a chart.

    Every chart has one. A picture of a number is not the number, and a report that cannot be checked
    is a report that has to be believed.
    """
    th = "".join(f"<th>{esc(h)}</th>" for h in headers)
    trs = "".join("<tr>" + "".join(f"<td>{esc(c)}</td>" for c in r) + "</tr>" for r in rows)
    return f'<details class="data"><summary>Data table</summary><table><tr>{th}</tr>{trs}</table></details>'


def _figure(title: str, subtitle: str, svg: str, legend: str, table: str) -> str:
    """One complete figure: title, chart, legend and table."""
    sub = f'<div class="fig-sub">{esc(subtitle)}</div>' if subtitle else ""
    return f'<figure class="chart"><figcaption><b>{esc(title)}</b>{sub}</figcaption>{legend}{svg}{table}</figure>'


def _bar_path(x: float, y: float, w: float, h: float, r: float = 4, horizontal: bool = True) -> str:
    """A bar with a rounded data end (right for horizontal bars, top for columns) and a square baseline."""
    if w <= 0 or h <= 0:
        return ""
    if horizontal:
        r = min(r, w, h / 2)
        return (f"M{x:.1f},{y:.1f}h{w - r:.1f}a{r},{r} 0 0 1 {r},{r}v{h - 2 * r:.1f}a{r},{r} 0 0 1 {-r},{r}"
                f"h{-(w - r):.1f}z")
    r = min(r, h, w / 2)
    return (f"M{x:.1f},{y + h:.1f}v{-(h - r):.1f}a{r},{r} 0 0 1 {r},{-r}h{w - 2 * r:.1f}a{r},{r} 0 0 1 {r},{r}"
            f"v{h - r:.1f}z")


def hbar(title: str, items: list[tuple[str, float]], unit: str = "", subtitle: str = "", fmt: Optional[Callable] = None,
         highlight: Optional[set] = None, color_index: Optional[dict] = None) -> str:
    """Horizontal bars (one measure, many categories), sorted as given. Value at the tip."""
    if not items:
        return ""
    fmt = fmt or (lambda v: compact(v, unit))
    label_w = min(300, max(80, CHAR_PX * max(len(str(k)) for k, _ in items) + 12))
    W, row = 720, 30
    plot_w = W - label_w - 90
    vmax = max((v for _, v in items), default=1) or 1
    H = row * len(items) + 24
    parts = [f'<svg viewBox="0 0 {W} {H}" class="svg" role="img" aria-label="{esc(title)}">']
    for t in nice_ticks(0, vmax, 4):
        if t > vmax * 1.001:
            break
        x = label_w + plot_w * t / vmax
        parts.append(f'<line x1="{x:.1f}" y1="0" x2="{x:.1f}" y2="{H - 20}" class="grid"/>'
                     f'<text x="{x:.1f}" y="{H - 6}" class="tick" text-anchor="middle">{esc(fmt(t))}</text>')
    for i, (k, v) in enumerate(items):
        y = i * row + 6
        w = plot_w * max(0.0, v) / vmax
        ci = (color_index or {}).get(k, 0)
        cls = "bar" + (" dim" if highlight and k not in highlight else "")
        parts.append(f'<text x="{label_w - 8}" y="{y + 13}" class="lab" text-anchor="end"><title>{esc(k)}</title>{esc(fit(k, label_w - 12))}</text>')
        parts.append(f'<path d="{_bar_path(label_w, y, max(w, 0.5), 18)}" fill="{color(ci)}" class="{cls}">'
                     f'<title>{esc(k)}: {esc(fmt(v))}</title></path>')
        parts.append(f'<text x="{label_w + w + 6:.1f}" y="{y + 13}" class="val">{esc(fmt(v))}</text>')
    parts.append(f'<line x1="{label_w}" y1="0" x2="{label_w}" y2="{H - 20}" class="axis"/></svg>')
    return _figure(title, subtitle, "".join(parts), "", _table(["Item", title], [[k, fmt(v)] for k, v in items]))


def stacked_hbar(title: str, cats: list[str], series: list[str], values: list[list[float]], unit: str = "",
                 subtitle: str = "", fmt: Optional[Callable] = None) -> str:
    """Horizontal stacked bars: values[i][j] = category i, series j. Total at the tip; 2px surface gaps."""
    if not cats or not series:
        return ""
    fmt = fmt or (lambda v: compact(v, unit))
    label_w = min(300, max(80, CHAR_PX * max(len(c) for c in cats) + 12))
    W, row = 720, 30
    plot_w = W - label_w - 90
    totals = [sum(max(0, x) for x in r) for r in values]
    vmax = max(totals, default=1) or 1
    H = row * len(cats) + 24
    parts = [f'<svg viewBox="0 0 {W} {H}" class="svg" role="img" aria-label="{esc(title)}">']
    for t in nice_ticks(0, vmax, 4):
        if t > vmax * 1.001:
            break
        x = label_w + plot_w * t / vmax
        parts.append(f'<line x1="{x:.1f}" y1="0" x2="{x:.1f}" y2="{H - 20}" class="grid"/>'
                     f'<text x="{x:.1f}" y="{H - 6}" class="tick" text-anchor="middle">{esc(fmt(t))}</text>')
    for i, c in enumerate(cats):
        y = i * row + 6
        x = label_w
        parts.append(f'<text x="{label_w - 8}" y="{y + 13}" class="lab" text-anchor="end"><title>{esc(c)}</title>{esc(fit(c, label_w - 12))}</text>')
        nonzero = [j for j, v in enumerate(values[i]) if v > 0]
        for j in nonzero:
            v = values[i][j]
            w = plot_w * v / vmax
            last = j == nonzero[-1]
            gap = 0 if last else 2
            d = _bar_path(x, y, max(0.5, w - gap), 18) if last else f"M{x:.1f},{y}h{max(0.5, w - gap):.1f}v18h{-max(0.5, w - gap):.1f}z"
            parts.append(f'<path d="{d}" fill="{color(j)}"><title>{esc(c)} · {esc(series[j])}: {esc(fmt(v))}</title></path>')
            x += w
        parts.append(f'<text x="{x + 6:.1f}" y="{y + 13}" class="val">{esc(fmt(totals[i]))}</text>')
    parts.append(f'<line x1="{label_w}" y1="0" x2="{label_w}" y2="{H - 20}" class="axis"/></svg>')
    rows = [[c] + [fmt(v) for v in values[i]] + [fmt(totals[i])] for i, c in enumerate(cats)]
    return _figure(title, subtitle, "".join(parts), _legend(series), _table(["Item"] + series + ["Total"], rows))


def line(title: str, x_labels: list[str], series: dict[str, list[Optional[float]]], unit: str = "", subtitle: str = "",
         fmt: Optional[Callable] = None, area: bool = False) -> str:
    """Lines over an ordered x (dates). One y-axis; the last value of each series is labeled at the end."""
    if not x_labels or not series:
        return ""
    fmt = fmt or (lambda v: compact(v, unit))
    names = list(series)[:MAX_SERIES]
    vals = [v for n in names for v in series[n] if v is not None]
    if not vals:
        return ""
    vmax = max(vals) or 1
    vmin = min(0.0, min(vals))
    W, H, L, R, T, B = 720, 260, 64, 190, 10, 28
    pw, ph = W - L - R, H - T - B
    n = len(x_labels)

    def X(i):
        """The x position for a data index."""
        return L + (pw * i / (n - 1) if n > 1 else pw / 2)

    ticks = nice_ticks(vmin, vmax, 4)
    top = max(ticks[-1], vmax)

    def Y(v):
        """The y position for a value."""
        return T + ph - ph * (v - vmin) / ((top - vmin) or 1)

    parts = [f'<svg viewBox="0 0 {W} {H}" class="svg" role="img" aria-label="{esc(title)}">']
    for t in ticks:
        parts.append(f'<line x1="{L}" y1="{Y(t):.1f}" x2="{W - R}" y2="{Y(t):.1f}" class="grid"/>'
                     f'<text x="{L - 6}" y="{Y(t) + 4:.1f}" class="tick" text-anchor="end">{esc(fmt(t))}</text>')
    step = max(1, math.ceil(n / 8))
    for i in range(0, n, step):
        parts.append(f'<text x="{X(i):.1f}" y="{H - 8}" class="tick" text-anchor="middle">{esc(x_labels[i])}</text>')
    ends = []
    for si, name in enumerate(names):
        pts = [(X(i), Y(v), v, x_labels[i]) for i, v in enumerate(series[name]) if v is not None]
        if not pts:
            continue
        d = "M" + " L".join(f"{x:.1f},{y:.1f}" for x, y, _, _ in pts)
        if area and len(pts) > 1:
            parts.append(f'<path d="{d} L{pts[-1][0]:.1f},{Y(max(0, vmin)):.1f} L{pts[0][0]:.1f},{Y(max(0, vmin)):.1f}z" '
                         f'fill="{color(si)}" opacity="0.1"/>')
        parts.append(f'<path d="{d}" fill="none" stroke="{color(si)}" class="ln"/>')
        for x, y, v, lab in pts if len(pts) <= 60 else [pts[-1]]:
            parts.append(f'<circle cx="{x:.1f}" cy="{y:.1f}" r="4" fill="{color(si)}" class="dot">'
                         f'<title>{esc(name)} · {esc(lab)}: {esc(fmt(v))}</title></circle>')
        ends.append((pts[-1][1], name, pts[-1][2], pts[-1][0]))
    ends.sort()
    last_y = -99
    if len(ends) <= 4:
        for y, name, v, x in ends:
            if y - last_y < 13:        # converging ends: leave identity to the legend and tooltips
                continue
            last_y = y
            parts.append(f'<text x="{x + 8:.1f}" y="{y + 4:.1f}" class="val">{esc(fmt(v))}{" · " + esc(fit(name, R - 70)) if len(names) > 1 else ""}</text>')
    parts.append(f'<line x1="{L}" y1="{T + ph}" x2="{W - R}" y2="{T + ph}" class="axis"/></svg>')
    rows = [[x_labels[i]] + [fmt(series[nm][i]) if series[nm][i] is not None else "–" for nm in names] for i in range(n)]
    return _figure(title, subtitle, "".join(parts), _legend(names, "line"), _table(["", *names], rows))


def scatter(title: str, points: list[dict], x_label: str, y_label: str, x_fmt: Callable, y_fmt: Callable,
            subtitle: str = "", frontier: Optional[list[str]] = None) -> str:
    """Points {name, x, y, group}: one dot per model, directly labeled. Groups (<= 3 colors) share a color."""
    pts = [p for p in points if p.get("x") is not None and p.get("y") is not None]
    if len(pts) < 2:
        return ""
    groups = []
    for p in pts:
        if p.get("group") not in groups:
            groups.append(p.get("group"))
    if len(groups) > 3:          # all-pairs color separation holds for three slots only
        groups = [None]
        for p in pts:
            p["group"] = None
    W, H, L, R, T, B = 720, 320, 70, 150, 12, 40
    pw, ph = W - L - R, H - T - B
    xs, ys = [p["x"] for p in pts], [p["y"] for p in pts]
    xt, yt = nice_ticks(min(0, min(xs)), max(xs), 5), nice_ticks(min(0, min(ys)), max(ys), 4)
    x0, x1, y0, y1 = xt[0], max(xt[-1], max(xs)), yt[0], max(yt[-1], max(ys))

    def X(v):
        """The x position for a value."""
        return L + pw * (v - x0) / ((x1 - x0) or 1)

    def Y(v):
        """The y position for a value."""
        return T + ph - ph * (v - y0) / ((y1 - y0) or 1)

    parts = [f'<svg viewBox="0 0 {W} {H}" class="svg" role="img" aria-label="{esc(title)}">']
    for t in yt:
        parts.append(f'<line x1="{L}" y1="{Y(t):.1f}" x2="{W - R}" y2="{Y(t):.1f}" class="grid"/>'
                     f'<text x="{L - 6}" y="{Y(t) + 4:.1f}" class="tick" text-anchor="end">{esc(y_fmt(t))}</text>')
    for t in xt:
        parts.append(f'<text x="{X(t):.1f}" y="{T + ph + 16}" class="tick" text-anchor="middle">{esc(x_fmt(t))}</text>')
    parts.append(f'<text x="{L + pw / 2:.1f}" y="{H - 4}" class="lab" text-anchor="middle">{esc(x_label)}</text>'
                 f'<text x="14" y="{T + ph / 2:.1f}" class="lab" text-anchor="middle" transform="rotate(-90 14 {T + ph / 2:.1f})">{esc(y_label)}</text>')
    if frontier:
        fp = sorted([p for p in pts if p["name"] in frontier], key=lambda p: p["x"])
        if len(fp) > 1:
            parts.append('<path d="M' + " L".join(f"{X(p['x']):.1f},{Y(p['y']):.1f}" for p in fp) + '" class="frontier"/>')
    for p in pts:
        gi = groups.index(p.get("group")) if p.get("group") in groups else 0
        parts.append(f'<circle cx="{X(p["x"]):.1f}" cy="{Y(p["y"]):.1f}" r="5" fill="{color(gi)}" class="dot">'
                     f'<title>{esc(p["name"])}: {esc(x_label)} {esc(x_fmt(p["x"]))}, {esc(y_label)} {esc(y_fmt(p["y"]))}</title></circle>')
        if len(pts) <= 12:
            parts.append(f'<text x="{X(p["x"]) + 8:.1f}" y="{Y(p["y"]) + 4:.1f}" class="val">{esc(fit(p["name"], R + (W - R - X(p["x"])) - 14))}</text>')
    parts.append(f'<line x1="{L}" y1="{T + ph}" x2="{W - R}" y2="{T + ph}" class="axis"/></svg>')
    rows = [[p["name"], x_fmt(p["x"]), y_fmt(p["y"])] + ([p.get("group") or ""] if len(groups) > 1 else []) for p in pts]
    legend = _legend([str(g) for g in groups], "dotk") if len(groups) > 1 else ""
    return _figure(title, subtitle, "".join(parts), legend,
                   _table(["Model", x_label, y_label] + (["Group"] if len(groups) > 1 else []), rows))


def columns(title: str, cats: list[str], values: list[float], unit: str = "", subtitle: str = "",
            fmt: Optional[Callable] = None) -> str:
    """Vertical columns for an ordered category (e.g. a lifespan sweep). Value on each cap."""
    if not cats:
        return ""
    fmt = fmt or (lambda v: compact(v, unit))
    W, H, L, T, B = 720, 220, 64, 18, 28
    pw, ph = W - L - 20, H - T - B
    vmax = max(values, default=1) or 1
    ticks = nice_ticks(0, vmax, 4)
    top = max(ticks[-1], vmax)
    band = pw / len(cats)
    bw = min(24.0, band * 0.6)
    parts = [f'<svg viewBox="0 0 {W} {H}" class="svg" role="img" aria-label="{esc(title)}">']
    for t in ticks:
        y = T + ph - ph * t / top
        parts.append(f'<line x1="{L}" y1="{y:.1f}" x2="{W - 20}" y2="{y:.1f}" class="grid"/>'
                     f'<text x="{L - 6}" y="{y + 4:.1f}" class="tick" text-anchor="end">{esc(fmt(t))}</text>')
    for i, (c, v) in enumerate(zip(cats, values)):
        x = L + band * i + (band - bw) / 2
        h = ph * max(0, v) / top
        parts.append(f'<path d="{_bar_path(x, T + ph - h, bw, max(h, 0.5), horizontal=False)}" fill="{color(0)}">'
                     f'<title>{esc(c)}: {esc(fmt(v))}</title></path>'
                     f'<text x="{x + bw / 2:.1f}" y="{T + ph - h - 5:.1f}" class="val" text-anchor="middle">{esc(fmt(v))}</text>'
                     f'<text x="{x + bw / 2:.1f}" y="{H - 8}" class="tick" text-anchor="middle">{esc(c)}</text>')
    parts.append(f'<line x1="{L}" y1="{T + ph}" x2="{W - 20}" y2="{T + ph}" class="axis"/></svg>')
    return _figure(title, subtitle, "".join(parts), "", _table(["", title], [[c, fmt(v)] for c, v in zip(cats, values)]))


def stat_tiles(tiles: list[dict]) -> str:
    """[{label, value, note}] -> a row of stat tiles."""
    out = "".join(f'<div class="tile"><div class="tl">{esc(t["label"])}</div><div class="tv">{esc(t["value"])}</div>'
                  + (f'<div class="tn">{esc(t["note"])}</div>' if t.get("note") else "") + "</div>" for t in tiles)
    return f'<div class="tiles">{out}</div>'


def table(headers: list[str], rows: list[list], cls: str = "") -> str:
    """A plain report table (cells may be pre-escaped HTML when wrapped in Raw)."""
    th = "".join(f"<th>{esc(h)}</th>" for h in headers)
    body = "".join("<tr>" + "".join(f"<td>{c.html if isinstance(c, Raw) else esc(c)}</td>" for c in r) + "</tr>" for r in rows)
    return f'<div class="tbl-wrap"><table class="rt {cls}"><tr>{th}</tr>{body}</table></div>'


class Raw:
    """HTML that is already safe and must not be escaped again."""
    def __init__(self, html_text: str):
        self.html = html_text


def meter(frac: float, label: str) -> Raw:
    """A small horizontal bar, for proportions inside a table cell."""
    pct = max(0.0, min(1.0, frac or 0)) * 100
    return Raw(f'<span class="meter"><span style="width:{pct:.0f}%"></span></span> {esc(label)}')
