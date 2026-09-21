"""Reports: built on demand from a spec (the report builder's choices), written as self-contained HTML files.

    saves/reports/<id>/spec.json      what was asked for
    saves/reports/<id>/meta.json      status, progress, timings, headline numbers
    saves/reports/<id>/report.html    the report

A spec:
    {"title", "range": {"preset": all|today|24h|7d|30d|90d|365d|custom, "from", "to"},
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
ALL_SECTIONS = ["summary", "costs", "cost_per_unit", "servers", "server_trend", "models", "benchmarks", "probes", "behavior",
                "lab", "whatif", "depreciation", "hardware", "data_quality", "activities", "methodology"]

PRESETS = {
    "item_cost": {"name": "Cost of a game, run or experiment",
                  "description": "What one thing (or a few) cost, broken down by server and cost component.",
                  "sections": ["summary", "costs", "cost_per_unit", "benchmarks", "probes", "lab", "activities", "data_quality", "methodology"]},
    "model_comparison": {"name": "Model comparison",
                         "description": "Performance, speed, reliability and cost of each model side by side, with what-if pricing.",
                         "sections": ["summary", "models", "cost_per_unit", "costs", "benchmarks", "behavior", "whatif", "data_quality", "methodology"]},
    "server_comparison": {"name": "Server comparison over time",
                          "description": "What each server cost, how busy it was, idle vs allocated, and lifespan sensitivity.",
                          "sections": ["summary", "servers", "server_trend", "costs", "whatif", "depreciation", "hardware", "data_quality", "methodology"]},
    "behavior": {"name": "Model behavior in scenarios",
                 "description": "Probe outcomes per case and model, tool use, how turns ended and common errors.",
                 "sections": ["summary", "probes", "behavior", "models", "data_quality"]},
    "lab": {"name": "Bot lab experiments", "description": "Cost of bot-vs-bot experiments and their results per seat label.",
            "sections": ["summary", "lab", "costs", "servers", "data_quality", "methodology"]},
    "everything": {"name": "Everything", "description": "Every section.", "sections": ALL_SECTIONS},
}


def _now() -> float:
    """The current time."""
    return time.time()


def normalize_spec(d: dict) -> dict:
    """Validate a report specification and fill in its defaults."""
    d = d or {}
    rng = d.get("range") or {}
    scope = d.get("scope") or {}
    opts = d.get("options") or {}
    narr = d.get("narrative") or {}
    lo, hi = (opts.get("lifespan_range") or [2, 6])[:2] if isinstance(opts.get("lifespan_range"), list) else (2, 6)
    lo, hi = max(1, min(20, int(lo))), max(1, min(30, int(hi)))
    return {
        "title": (str(d.get("title") or "").strip() or "CITAR report")[:120],
        "preset": d.get("preset") if d.get("preset") in PRESETS else None,
        "range": {"preset": rng.get("preset") if rng.get("preset") in ("all", "today", "24h", "7d", "30d", "90d", "365d", "custom") else "all",
                  "from": str(rng.get("from") or "")[:10], "to": str(rng.get("to") or "")[:10]},
        "scope": {"servers": [str(s) for s in scope.get("servers") or []][:50],
                  "models": [str(s) for s in scope.get("models") or []][:50],
                  "kinds": [k for k in scope.get("kinds") or [] if k in ("game", "benchmark", "probe", "lab", "report")],
                  "items": [{"kind": str(i.get("kind")), "id": str(i.get("id")), "name": str(i.get("name") or "")[:120]}
                            for i in scope.get("items") or [] if isinstance(i, dict) and i.get("id")][:200]},
        "sections": [s for s in d.get("sections") or PRESETS["model_comparison"]["sections"] if s in ALL_SECTIONS] or ["summary"],
        "options": {"electricity_basis": "marginal" if opts.get("electricity_basis") == "marginal" else "full",
                    "lifespan_range": [min(lo, hi), max(lo, hi)]},
        "narrative": {"enabled": bool(narr.get("enabled")), "server_id": narr.get("server_id") or None,
                      "model_id": narr.get("model_id") or None, "profile_id": narr.get("profile_id") or None,
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
        for m in self.list():
            if m["status"] in ("queued", "running"):
                self._set(m["id"], status="failed", error="The CITAR server restarted while this report was running; run it again.")

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
        """Delete a report and everything it produced."""
        m = self.meta(rid)
        if m["status"] in ("queued", "running"):
            raise ValueError("The report is still running.")
        shutil.rmtree(self._path(rid), ignore_errors=True)

    # ------------------------------------------------------------------ running
    def start(self, spec: dict) -> dict:
        """Queue a report to be built."""
        spec = normalize_spec(spec)
        rid = datetime.now().strftime("%Y%m%d-%H%M%S-") + secrets.token_hex(2)
        p = self._path(rid)
        p.mkdir(parents=True, exist_ok=True)
        (p / "spec.json").write_text(json.dumps(spec, indent=1), encoding="utf-8")
        m = self._set(rid, title=spec["title"], preset=spec.get("preset"), status="queued", created=_now(), started=None,
                      finished=None, progress="Waiting", error=None, summary=None, narrative=spec["narrative"]["enabled"])
        with self.lock:
            self.queue.append(rid)
        if self._thread is None or not self._thread.is_alive():
            self._thread = threading.Thread(target=self._loop, daemon=True, name="reports")
            self._thread.start()
        return m

    def _loop(self):
        """The worker thread: build queued reports in order."""
        while True:
            with self.lock:
                rid = self.queue.pop(0) if self.queue else None
            if rid is None:
                return
            try:
                self.run(rid)
            except Exception as e:
                self._set(rid, status="failed", error=f"{type(e).__name__}: {e}", trace=traceback.format_exc(limit=8),
                          finished=_now())

    def run(self, rid: str):
        """Build one report: gather, cost, render, write."""
        from .data import gather
        from .render import render, narrative_brief
        spec = self.spec(rid)
        t0 = _now()
        self._set(rid, status="running", started=t0, progress="Collecting data")
        data = gather(spec, progress=lambda msg: self._set(rid, progress=msg))
        narrative, note = "", ""
        if spec["narrative"]["enabled"]:
            self._set(rid, progress="Writing the analysis with a model")
            narrative, note = self._narrative(rid, spec, narrative_brief(data))
        self._set(rid, progress="Rendering")
        page, summary = render(data, narrative, note)
        from ..fsutil import write_text
        write_text(self._path(rid) / "report.html", page)
        self._set(rid, status="done", finished=_now(), progress="Done", summary=summary, seconds=round(_now() - t0, 1),
                  size=len(page.encode("utf-8")))

    def _narrative(self, rid: str, spec: dict, brief: str) -> tuple[str, str]:
        """Ask the chosen model for the written analysis; its usage is recorded as a 'report' activity."""
        from .. import servers, usage
        n = spec["narrative"]
        try:
            sv = servers.get(n["server_id"])
        except servers.ServerError as e:
            return "", f"The narrative was skipped: {e}"
        end = servers.restricted(sv)
        if end:
            return "", f"The narrative was skipped: {sv['name']} is in its restricted hours until {end:%H:%M}."
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
            try:
                servers.ensure_model(sv, cfg["model"], cfg.get("load"))
            except Exception:
                pass
            t0 = time.perf_counter()
            text, tokens, served = complete(cfg, NARRATIVE_SYSTEM, prompt)
            secs = time.perf_counter() - t0
            tr.llm(act, sv["id"], served or cfg["model"], secs, **tokens)
            return text, f"Written by {cfg['model']} on {sv['name']} in {secs:.0f}s from the computed figures above."
        except Exception as e:
            return "", f"The narrative failed: {type(e).__name__}: {e}"
        finally:
            holding["on"] = False
            tr.update(act, ended=_now())
            tr.unregister(act)
            tr.flush()


def complete(cfg: dict, system: str, prompt: str) -> tuple[str, dict, Optional[str]]:
    """One plain completion (no tools). Returns (text, token counts for the ledger, model that served it)."""
    provider = cfg.get("provider")
    if provider == "dryrun":
        time.sleep(float(cfg.get("dry_run_delay") or 0))
        text = ("(Dry run narrative.) This is where a model's written analysis of the figures would appear. The computed "
                "findings and charts below are produced without a model.")
        return text, {"input_tokens": len(prompt) // 4, "output_tokens": len(text) // 4}, cfg.get("model")
    if provider == "anthropic":
        import anthropic
        client = anthropic.Anthropic(api_key=cfg.get("api_key"), base_url=cfg.get("base_url") or None, max_retries=3)
        params = {"model": cfg["model"], "max_tokens": int(cfg.get("max_tokens") or 8000), "system": system,
                      "messages": [{"role": "user", "content": prompt}]}
        if cfg.get("effort"):
            params["output_config"] = {"effort": cfg["effort"]}
        r = client.messages.create(**params)
        if r.stop_reason == "refusal":
            raise RuntimeError("the model declined to write the analysis")
        text = "\n".join(b.text for b in r.content if b.type == "text")
        u = r.usage
        return text, {"input_tokens": u.input_tokens or 0, "output_tokens": u.output_tokens or 0,
                      "cache_read": getattr(u, "cache_read_input_tokens", 0) or 0,
                      "cache_write": getattr(u, "cache_creation_input_tokens", 0) or 0}, getattr(r, "model", None)
    from openai import OpenAI
    client = OpenAI(base_url=cfg.get("base_url") or None, api_key=cfg.get("api_key") or "not-needed",
                    timeout=float(cfg.get("timeout") or 1800), max_retries=1)
    params = {"model": cfg["model"], "messages": [{"role": "system", "content": system}, {"role": "user", "content": prompt}]}
    if cfg.get("reasoning_effort"):
        params["extra_body"] = {"reasoning_effort": cfg["reasoning_effort"]}
    if cfg.get("max_tokens"):
        params["max_tokens"] = int(cfg["max_tokens"])
    r = client.chat.completions.create(**params)
    text = r.choices[0].message.content or ""
    text = re.sub(r"<think>.*?</think>", "", text, flags=re.S).strip()
    toks = {}
    if r.usage:
        toks = {"input_tokens": r.usage.prompt_tokens or 0, "output_tokens": r.usage.completion_tokens or 0}
    return text, toks, cfg.get("model")


def scope_options() -> dict:
    """What the report builder can pick from: servers, models, and recent items (runs, experiments, games)."""
    from .. import servers, usage
    reg = servers.load()
    ledger = usage.read()
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
