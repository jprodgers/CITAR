"""Compares the golden reports of the determinism matrix (crates/citar-engine/DESIGN.md 2.6, 9.6).

Used by determinism.yml (the short sets on every pull request) and nightly.yml (the long set).

    golden_compare.py compare REPORTS --expect T1,T2,... [--prefix golden-] [--odd FILE]

Reads REPORTS/<prefix><target>.json, each written by `golden check --out` on one target, and
fails (exit 1) when

- a target sent no report;
- the targets disagree on a set: a determinism bug. The targets are grouped by what they
  computed; the reference group is the one that matches the committed file, or else the largest.
  For each other group, the first row of each list where it parts from the reference is named,
  with the committed file's row there (a game and its turn for the whole-game sets);
- the targets agree with each other but not with the committed file: a behaviour change, which
  is blessed (`cargo golden bless`) if it is deliberate.

With --odd, the targets outside the reference group are written to FILE as {target: [set, ...]},
and `diverged=true|false` goes to $GITHUB_OUTPUT when it is set.

    golden_compare.py states STATES ODD GOLDEN OUT

For each target ODD names, reads the states it wrote (`golden check --states`, uploaded as
STATES/golden-states-<target>/), plays each of the same games to the same round on this machine
with GOLDEN (`golden dump`), and compares the two (`golden diff`), writing everything to OUT:
the reference states and one diff per state. Exit 0 whatever the diffs say: the compare step has
already failed the run.
"""

import functools
import json
import os
import subprocess
import sys
from pathlib import Path

GOLDEN_DIR = Path("crates/citar-testkit/golden")


def committed_row(set_name, key, index):
    path = GOLDEN_DIR / f"{set_name}.json"
    try:
        rows = json.loads(path.read_text(encoding="utf-8")).get(key) or []
        return json.dumps(rows[index]) if index < len(rows) else "not in the committed file"
    except (OSError, ValueError):
        return "(no committed file)"


def first_parting(ref_rows, rows):
    """For each list, where two targets' rows first part: (key, index, differing, sizes)."""
    out = []
    for key, hashes in ref_rows.items():
        other = rows.get(key)
        if other is None:
            continue
        index = next((i for i, (a, b) in enumerate(zip(hashes, other)) if a != b), None)
        if index is None and len(hashes) != len(other):
            index = min(len(hashes), len(other))
        if index is not None:
            differing = sum(1 for a, b in zip(hashes, other) if a != b)
            out.append((key, index, differing, (len(hashes), len(other))))
    return out


def rank(sets, group):
    """The reference group first: the one that matches the committed file, then the largest."""
    computed, targets = group
    matches = any(sets[t].get("matches_committed") for t in targets)
    return (not matches, -len(targets), computed or "")


def compare(args):
    reports_dir = Path(args[0])
    expect, prefix, odd_file = [], "golden-", None
    rest = iter(args[1:])
    for a in rest:
        if a == "--expect":
            expect = next(rest).split(",")
        elif a == "--prefix":
            prefix = next(rest)
        elif a == "--odd":
            odd_file = next(rest)
        else:
            sys.exit(f"unknown argument {a}")
    reports = {}
    for path in sorted(reports_dir.glob(f"{prefix}*.json")):
        target = path.name[len(prefix):-len(".json")]
        reports[target] = json.loads(path.read_text(encoding="utf-8"))
    failed = False
    odd = {}
    missing = sorted(set(expect) - reports.keys())
    if missing:
        print(f"::error::no report from {', '.join(missing)}")
        failed = True
    names = sorted({name for r in reports.values() for name in r["sets"]})
    for name in names:
        sets = {t: r["sets"].get(name) or {} for t, r in reports.items()}
        groups = {}
        for t, s in sorted(sets.items()):
            groups.setdefault(s.get("computed"), []).append(t)
        if len(groups) == 1:
            if all(s.get("matches_committed") for s in sets.values()):
                print(f"{name}: identical on {len(sets)} targets and equal to the committed file")
            else:
                failed = True
                print(f"::error::{name}: every target agrees, but not with the committed file: a behaviour "
                      "change (bless it if it is deliberate)")
                some = next(iter(sets.values()))
                for p in some.get("problems") or []:
                    print(f"    {p}")
            continue
        failed = True
        (_, ref_targets), *others = sorted(groups.items(), key=functools.partial(rank, sets))
        ref = sets[ref_targets[0]]
        print(f"::error::{name}: the targets disagree, a determinism bug. Reference: {', '.join(ref_targets)}"
              f"{' (equal to the committed file)' if ref.get('matches_committed') else ''}")
        for _, targets in others:
            print(f"  {', '.join(targets)} part from it:")
            for t in targets:
                odd.setdefault(t, []).append(name)
            for key, index, differing, sizes in first_parting(ref.get("rows") or {}, sets[targets[0]].get("rows") or {}):
                print(f"    {key}: {differing} rows differ ({sizes[0]} against {sizes[1]}); the first is [{index}], "
                      f"committed {committed_row(name, key, index)}")
    if odd_file:
        Path(odd_file).write_text(json.dumps(odd, indent=1) + "\n", encoding="utf-8")
        out = os.environ.get("GITHUB_OUTPUT")
        if out:
            with open(out, "a", encoding="utf-8") as f:
                f.write(f"diverged={'true' if odd else 'false'}\n")
    return 1 if failed else 0


def states(args):
    states_dir, odd_file, golden, out = Path(args[0]), Path(args[1]), args[2], Path(args[3])
    odd = json.loads(odd_file.read_text(encoding="utf-8"))
    (out / "reference").mkdir(parents=True, exist_ok=True)
    runner = os.environ.get("GOLDEN_RUNNER_TARGET", "linux-x64")
    if runner in odd:
        print(f"note: {runner}, where these references are dumped, is itself among the targets that part; "
              "dump the references on a target that agrees with the committed file")
    for target, sets in sorted(odd.items()):
        listing = states_dir / f"golden-states-{target}" / "divergence.jsonl"
        if not listing.exists():
            print(f"{target}: wrote no states")
            continue
        for line in listing.read_text(encoding="utf-8").splitlines():
            entry = json.loads(line)
            if entry.get("agrees") or entry["set"] not in sets:
                continue
            name = f"{entry['set']}:{entry['game']}"
            reference = out / "reference" / entry["file"]
            dump = [golden, "dump", name] + ([str(entry["turn"])] if entry["turn"] is not None else [])
            done = subprocess.run([*dump, "--out", str(reference)], capture_output=True, text=True)
            if done.returncode != 0:
                print(f"{target}: {name}: the reference does not dump: {done.stderr.strip()}")
                continue
            diff = subprocess.run([golden, "diff", str(reference), str(listing.parent / entry["file"])],
                                  capture_output=True, text=True)
            report = out / f"diff-{target}-{entry['file'][:-len('.json')]}.txt"
            report.write_text(diff.stdout + diff.stderr, encoding="utf-8")
            lines = diff.stdout.splitlines()
            print(f"{target}: {name} round {entry['turn']}: {lines[-1] if lines else diff.stderr.strip()}")
            for shown in lines[:15]:
                print(f"    {shown}")
    return 0


if __name__ == "__main__":
    commands = {"compare": compare, "states": states}
    if len(sys.argv) < 3 or sys.argv[1] not in commands:
        sys.exit(__doc__)
    sys.exit(commands[sys.argv[1]](sys.argv[2:]))
