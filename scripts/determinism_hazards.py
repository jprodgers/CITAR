"""Plants a determinism hazard in a scratch checkout of the Rust engine, to show the guards catch it.

    python scripts/determinism_hazards.py ROOT hashmap|powf|round [--allow]

Package 1e-02's gate 3 (crates/citar-engine/DESIGN.md 7.5): a branch with a hash map iterated
where its order decides something, a platform `powf`, or an `f64::round`, breaks clippy or the
determinism matrix. The plants:

- `hashmap`: the cities end their turn in a `std::collections::HashSet`'s order
  (`game::cities::lifecycle::end_turn_stage`), not by (razing, id);
- `powf`: `base::num::pow` calls the platform's `f64::powf` instead of `libm::pow`;
- `round`: `base::num::round_half_even` calls `f64::round`, which rounds halves away from zero.

Without --allow the plant is plain code, which `cargo clippy -p citar-engine --all-targets -- -D
warnings` must refuse. With --allow the function carries the attributes that silence clippy, so
the matrix must catch it instead: `cargo golden check` (the hash order moves the pass and random
sets, the platform `powf` moves the libm set on Windows, `f64::round` moves pyfmt and every game)
and `same_process_twice` (the hash order). Run it in a worktree you throw away, never on a branch
that is pushed for merging.
"""

import sys
from pathlib import Path

if len(sys.argv) < 3:
    sys.exit(__doc__)
root, what = Path(sys.argv[1]), sys.argv[2]
allow = "--allow" in sys.argv


def edit(rel, old, new):
    p = root / rel
    s = p.read_text(encoding="utf-8")
    if s.count(old) != 1:
        sys.exit(f"{rel}: the code to replace is not there once; the engine has moved on")
    p.write_text(s.replace(old, new), encoding="utf-8", newline="\n")


def attr(lints):
    return f'#[allow({lints}, reason = "a determinism hazard, planted")]\n' if allow else ""


if what == "hashmap":
    edit(
        "crates/citar-engine/src/game/cities/lifecycle.rs",
        """pub(crate) fn end_turn_stage(g: &mut Game, p: PlayerId) {
    let mut cities: Vec<(bool, CityId)> = g.player_cities(p).map(|x| (!x.razing, x.id())).collect();
    cities.sort();
    for (_, c) in cities {
        if g.city(c).is_some() {
            end_turn(g, c);
        }
    }
}""",
        attr("clippy::disallowed_types, clippy::iter_over_hash_type")
        + """pub(crate) fn end_turn_stage(g: &mut Game, p: PlayerId) {
    let cities: std::collections::HashSet<CityId> = g.player_cities(p).map(|x| x.id()).collect();
    for c in cities {
        if g.city(c).is_some() {
            end_turn(g, c);
        }
    }
}""",
    )
elif what == "powf":
    edit(
        "crates/citar-engine/src/base/num.rs",
        """pub fn pow(x: f64, y: f64) -> f64 {
    libm::pow(x, y)
}""",
        attr("clippy::disallowed_methods") + "pub fn pow(x: f64, y: f64) -> f64 {\n    x.powf(y)\n}",
    )
elif what == "round":
    edit(
        "crates/citar-engine/src/base/num.rs",
        """pub fn round_half_even(x: f64) -> f64 {
    x.round_ties_even()
}""",
        attr("clippy::disallowed_methods") + "pub fn round_half_even(x: f64) -> f64 {\n    x.round()\n}",
    )
else:
    sys.exit(__doc__)
print(f"planted {what}{' with the allows' if allow else ''}")
