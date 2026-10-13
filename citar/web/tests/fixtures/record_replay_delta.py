"""Records citar/web/tests/fixtures/replay-delta.json.gz: one game's replay in the `Delta` format beside the same
frames in the `Full` format, which replaydecode.test.mjs holds the browser's decoder to (design/phase3-features.md
P3.5.8, package 3-13).

The game: three bot civilizations, a city-state and barbarians on a 22 by 14 map, played by the compiled bot to its
turn limit of 100, and reloaded in place at turn 70 (the `reload` test operation: the whole history saved and loaded
again), so its frames hold a keyframe at the first round, one 64 rounds later and one after the reload, with deltas
between. The engine's `Full` format, which expanded every frame on the server, was retired by the same package once
the client decoded `Delta` itself, so this script ran once, on the build just before (`replay_data("full")` and
`replay_data("delta")` of one game, in one process). It needs the `test-ops` build (`cargo xtask develop`).

    python citar/web/tests/fixtures/record_replay_delta.py [--out PATH]

The output is gzip (mtime 0) of compact JSON, so the same build writes the same bytes: {"format", "recorded",
"width", "height", "improvement_ids", "feature_ids", "unit_ids", "delta": [{turn, keyframe, bytes}], "full": [frame in
Python's shape]}. Gzip because the full frames, the point of the comparison, are large (each repeats every layer) and
compress about ten to one.
"""
from __future__ import annotations

import argparse
import gzip
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

CONFIG = {"seed": 13013, "map_size": "duel", "map_type": "continents", "width": 22, "height": 14,
          "players": [{"controller": "bot"}] * 3, "city_states": 1, "barbarians": "normal", "turn_limit": 100}
RELOAD_AT = 70


def record() -> dict:
    from citar.engine_api import EngineGame, bot_instance

    g = EngineGame.new(dict(CONFIG))
    bots = {p: bot_instance("basic") for p in range(3)}
    reloaded = False
    while g.phase == "playing":
        g.drive(bots, 1)
        if not reloaded and g.turn == RELOAD_AT:
            g.test_ops([{"op": "reload"}])
            reloaded = True
    full = json.loads(g._g.replay_data("full"))
    delta = json.loads(g._g.replay_data("delta"))
    assert reloaded, "the game reached the reload"
    assert len(full["frames"]) == len(delta["frames"]), "every record decodes"
    keys = [f["turn"] for f in delta["frames"] if f["keyframe"]]
    assert len(keys) >= 3, f"keyframes at {keys}"
    for k in ("width", "height", "improvement_ids", "feature_ids", "unit_ids"):
        assert full[k] == delta[k], k
    return {
        "format": "citar-replay-fixture", "recorded": {"config": CONFIG, "reload_at": RELOAD_AT, "keyframes": keys,
                                                        "turn": full["turn"], "phase": full["phase"]},
        "width": delta["width"], "height": delta["height"], "improvement_ids": delta["improvement_ids"],
        "feature_ids": delta["feature_ids"], "unit_ids": delta["unit_ids"],
        "delta": delta["frames"], "full": full["frames"],
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--out", type=Path, default=HERE / "replay-delta.json.gz")
    a = ap.parse_args()
    doc = record()
    raw = json.dumps(doc, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    a.out.write_bytes(gzip.compress(raw, compresslevel=9, mtime=0))
    print(f"{a.out}: {len(doc['delta'])} frames (keyframes at {doc['recorded']['keyframes']}), "
          f"{len(raw)} bytes of JSON, {a.out.stat().st_size} gzipped")
    return 0


if __name__ == "__main__":
    sys.exit(main())
