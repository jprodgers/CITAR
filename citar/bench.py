"""Benchmark language models head-to-head on an identical scenario.

Each model plays the same seeded map for N turns against a scripted bot (models run one after another so a single
local GPU isn't shared). Prints a comparison table and writes a JSON report; the games are also saved, so they show
up in the lobby's Model comparison and can be replayed.

    python -m citar.bench --base-url http://localhost:1234/v1 --model qwen/qwen3.8-27b --model prism-ml/bonsai-27b --turns 10

For long, resumable benchmarks with quiet hours, parallel servers and live viewing, use the Benchmarks page of the web
app instead (python -m citar.server, then open Benchmarks). Games from this script also count toward model scores.

LM Studio extras:
    --all-models          benchmark every LLM on the server
    --load-context 32768  before each run, unload everything and load that model alone with this context (via the
                          `lms` CLI), so load time, JIT defaults and GPU sharing don't skew the turn times
    --tool-mode auto      (default) models LM Studio doesn't mark as tool-capable use the JSON tool-call mode
"""
from __future__ import annotations

import argparse
import json
import time
from pathlib import Path

from .server import lmstudio
from .server.session import SessionManager, SAVE_DIR

REPORT_NAME = f"benchmark-{time.strftime('%Y%m%d-%H%M%S')}.json"


def run_model(manager: SessionManager, args, model: str) -> dict:
    """Play one benchmark game with a model and record what happened."""
    tool_mode = lmstudio.tool_mode_for(args.base_url, model, args.tool_mode)
    load_s = None
    if args.load_context:
        print(f"\nLoading {model} alone with {args.load_context:,} context...", flush=True)
        try:
            load_s = lmstudio.ensure_loaded(args.base_url, model, args.load_context, exclusive=True, gpu=args.gpu)
        except Exception as e:
            print(f"  {e}", flush=True)
            return {"model": model, "tool_mode": tool_mode, "error": str(e), "metrics": {}, "outcome": {}}
        print(f"  loaded in {load_s}s", flush=True)
    llm = {"provider": args.provider, "base_url": args.base_url, "model": model, "tool_mode": tool_mode,
           "reasoning_effort": args.reasoning_effort or None, "api_key_env": args.api_key_env or None,
           "max_turn_seconds": args.max_turn_seconds, "effort": args.effort or None}
    llm = {k: v for k, v in llm.items() if v is not None}
    from .server.benchmarks import BENCH_SPEED, BENCH_NATION
    seats = [{"type": "llm", "civ_name": None, "llm": llm, "nation": BENCH_NATION}]
    seats += [{"type": "bot", "civ_name": f"Benchmark Bot {i + 1}" if args.opponents > 1 else "Benchmark Bot",
               "nation": BENCH_NATION, "difficulty": args.bot_difficulty or None} for i in range(args.opponents)]
    s = manager.create({"map_size": args.map_size, "map_type": args.map_type, "seed": args.seed, "barbarians": args.barbarians,
                        "speed": BENCH_SPEED, "turn_limit": None, "difficulty": args.difficulty},
                       seats, name=f"Bench: {model}")
    s.ai_delay = 0
    s.benchmark = {"run_id": None, "job_id": None, "run_name": "CLI benchmark", "model": model, "server": args.base_url,
                   "scenario": f"{args.map_size} {args.map_type} seed {args.seed} (CLI)", "llm_player": 0, "turn_limit": 0}
    started = time.time()
    last_turn = 0
    print(f"\n=== {model} (game {s.id}, tool mode {tool_mode}) ===", flush=True)
    try:
        while s.game.turn <= args.turns and s.game.s.phase == "playing":
            if time.time() - started > args.max_total_minutes * 60:
                print("  stopping: total time limit reached", flush=True)
                break
            if s.game.turn != last_turn:
                last_turn = s.game.turn
                rep = s.metrics_report()["summary"].get(0, {})
                if rep.get("turns"):
                    print(f"  turn {last_turn}: avg turn {rep['avg_turn_s']}s, steps/turn {rep['avg_model_steps']}, "
                          f"repeats/turn {rep['avg_repeats']}, errors/turn {rep['avg_errors']}, ends {rep['end_reasons']}",
                          flush=True)
            time.sleep(1)
    except KeyboardInterrupt:
        print("  interrupted", flush=True)
    s.paused = True
    with s.lock:
        g = s.game
        report = s.metrics_report()
        summary = report["summary"].get(0, {})
        p = g.player(0)
        from .engine.victory import score
        bot_scores = [score(g, q.id)["total"] for q in g.majors() if q.id != 0]
        outcome = {
            "turns_played": g.turn - 1, "civ_name": p.name, "cities": len(g.player_cities(0)), "units": len(g.player_units(0)),
            "techs": len(p.techs), "score": score(g, 0)["total"], "bot_score": max(bot_scores or [0]),
            "bot_scores": bot_scores, "bot_techs": [len(q.techs) for q in g.majors() if q.id != 0],
            "bot_cities": [len(g.player_cities(q.id)) for q in g.majors() if q.id != 0],
            "winner": g.s.winner, "victory": g.s.victory,
            "population": sum(c.pop for c in g.player_cities(0)), "gold": int(p.gold),
        }
        errors = [e for e in s.errors if e.get("player") in (0, None)][-5:]
    s.save("benchmark")
    manager.delete(s.id)
    return {"model": model, "tool_mode": tool_mode, "load_seconds": load_s, "game_id": s.id,
            "wall_minutes": round((time.time() - started) / 60, 1), "metrics": summary, "outcome": outcome,
            "agent_errors": errors}


def write_report(args, results) -> Path:
    """Write the benchmark results to ``saves/benchmark-*.json``."""
    SAVE_DIR.mkdir(parents=True, exist_ok=True)
    path = SAVE_DIR / REPORT_NAME
    path.write_text(json.dumps({"args": vars(args), "results": results}, indent=2, default=str), encoding="utf-8")
    return path


def main():
    """The ``citar bench`` command line."""
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--model", action="append", default=[], help="model id (repeat for several)")
    ap.add_argument("--all-models", action="store_true", help="LM Studio: benchmark every LLM on the server")
    ap.add_argument("--load-context", type=int, default=0, help="LM Studio: load each model alone with this context first")
    ap.add_argument("--gpu", default="", help='LM Studio GPU offload when loading: "off", "max" or a ratio (default: automatic)')
    ap.add_argument("--provider", default="openai_compatible", choices=["openai_compatible", "anthropic"])
    ap.add_argument("--base-url", default="http://localhost:1234/v1")
    ap.add_argument("--api-key-env", default="")
    ap.add_argument("--tool-mode", default="auto", choices=["auto", "native", "json"])
    ap.add_argument("--reasoning-effort", default="low", help="low|medium|high or '' for model default (OpenAI-compatible)")
    ap.add_argument("--effort", default="", help="Anthropic effort")
    ap.add_argument("--turns", type=int, default=10)
    ap.add_argument("--seed", type=int, default=7)
    ap.add_argument("--map-size", default="duel")
    ap.add_argument("--opponents", type=int, default=1, help="number of scripted bot opponents")
    ap.add_argument("--difficulty", default="Prince", help="game difficulty (the model's seat)")
    ap.add_argument("--bot-difficulty", default="", help="difficulty of the bot seats (default: game difficulty)")
    ap.add_argument("--map-type", default="continents")
    ap.add_argument("--barbarians", default="normal")
    ap.add_argument("--max-turn-seconds", type=float, default=900)
    ap.add_argument("--max-total-minutes", type=float, default=240)
    args = ap.parse_args()
    if args.all_models:
        args.model += [i for i, m in lmstudio.models(args.base_url).items()
                       if m.get("type") != "embeddings" and i not in args.model]
    if not args.model:
        ap.error("give --model (repeatable) or --all-models")

    manager = SessionManager()
    results = []
    for m in args.model:
        results.append(run_model(manager, args, m))
        write_report(args, results)  # keep partial results if a later model hangs or the run is stopped

    cols = [("model", 34), ("mode", 7), ("turns", 6), ("avg turn", 9), ("max", 7), ("steps", 6), ("calls", 6), ("errors", 7),
            ("repeats", 8), ("malformed", 10), ("clean ends", 11), ("tok/s", 6), ("cities", 7), ("techs", 6), ("score", 6)]
    print("\n" + "".join(name.ljust(w) for name, w in cols))
    for r in results:
        if r.get("error"):
            print(r["model"][:33].ljust(34) + "failed: " + r["error"][:120])
            continue
        m, o = r["metrics"], r["outcome"]
        turns = m.get("turns", 0)
        clean = m.get("end_reasons", {}).get("end_turn", 0)
        vals = [r["model"][:33], r["tool_mode"], turns, f"{m.get('avg_turn_s', '-')}s", f"{m.get('max_turn_s', '-')}s",
                m.get("avg_model_steps", "-"), m.get("avg_tool_calls", "-"), m.get("avg_errors", "-"), m.get("avg_repeats", "-"),
                m.get("malformed_calls", "-"), f"{round(100 * clean / turns) if turns else 0}%", m.get("output_tokens_per_s", "-"),
                o["cities"], o["techs"], f"{o['score']}/{o['bot_score']}"]
        print("".join(str(v).ljust(w) for v, (_, w) in zip(vals, cols)))
    print(f"\nReport written to {write_report(args, results)}")


if __name__ == "__main__":
    main()
