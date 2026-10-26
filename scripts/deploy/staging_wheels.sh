#!/usr/bin/env bash
# staging_wheels.sh: the releases a staging run deploys (design/phase5-release.md P5.14), built as the release builds
# its Linux x64 wheel (maturin in a manylinux container under rootless podman), each made into a deploy bundle by
# scripts/deploy/bundle.py --from-dir. Run inside WSL as an ordinary user, never as root and never under /mnt/c:
#
#     staging_wheels.sh --clone CLONE --out DIR [--rc-base X.Y.Z] [--cutover-from DIR] [--bundles-only]
#
# CLONE is a git clone of the head being staged (its deploy/ goes into every bundle). DIR, outside any git working
# tree, receives wheels/<version>/ and bundles/citar-deploy-<version>/ (unpacked, ready for staging.sh run):
#
#   - the cutover's release: CLONE's own wheel, built here; or, with --cutover-from, the wheel and its constraints
#     taken from a folder (the release dry run's linux-x64 wheel, as Phase 5's exit stages it);
#   - X.Y.Z-rc.1: a scratch clone of CLONE bumped to it by scripts/release.py, with one empty migration added (an
#     upgrade that migrates);
#   - X.Y.Z-rc.2: bumped, rc.1's migration and a second empty one, and its server planted to exit right after it has
#     migrated (citar/server/boot.py's startup()): the failed upgrade citar-deploy must undo with the database
#     restored;
#   - X.Y.Z-rc.3: as rc.2, but its server exits before it migrates: undone without a restore.
#
# --bundles-only builds nothing: each release's bundle is made again from the wheel and constraints DIR holds already
# (after a change to deploy/, say). --rc-base defaults to the patch release after CLONE's version. Each scratch clone's changes are committed in it
# (never pushed) and the clone is removed once its wheel is built. The cargo target and the container's registry are
# ~/target/citar-p5's (CARGO_TARGET_DIR for the container: its manylinux folder). Exit status 0 when every bundle is
# written, 1 when something failed, 2 for a usage error.

set -euo pipefail
umask 022
cd "$HOME"

IMAGE="ghcr.io/pyo3/maturin:v1.15.0"
TARGET="$HOME/target/citar-p5"
PY="python3.12"

die() { printf 'staging_wheels: %s\n' "$*" >&2; exit 1; }
usage_die() { printf 'staging_wheels: %s (staging_wheels.sh --help)\n' "$*" >&2; exit 2; }
say() { printf '%s\n' "$*"; }

CLONE=""
OUT=""
RC_BASE=""
CUTOVER_FROM=""
BUNDLES_ONLY=0
while [ $# -gt 0 ]; do
  case "$1" in
    --clone|--out|--rc-base|--cutover-from)
      [ $# -ge 2 ] || usage_die "$1 needs a value"
      case "$1" in
        --clone) CLONE="$2" ;;
        --out) OUT="$2" ;;
        --rc-base) RC_BASE="$2" ;;
        --cutover-from) CUTOVER_FROM="$2" ;;
      esac
      shift ;;
    --bundles-only) BUNDLES_ONLY=1 ;;
    -h|--help) sed -n '2,/^$/s/^# \{0,1\}//p' "$0"; exit 0 ;;
    *) usage_die "unknown argument: $1" ;;
  esac
  shift
done
{ [ -n "$CLONE" ] && [ -n "$OUT" ]; } || usage_die "--clone and --out are needed"
[ "$(id -u)" != 0 ] || die "run it as an ordinary user (podman rootless), not as root"
case "$CLONE$OUT" in /mnt/*) die "never under /mnt: build on WSL's own disk" ;; esac
CLONE="$(cd "$CLONE" && pwd -P)"
# P5.18.1 rule 8: name every checkout explicitly, and check the clone is the one meant before anything is written.
[ "$(git -C "$CLONE" rev-parse --show-toplevel)" = "$CLONE" ] || die "$CLONE is not the top of a git clone"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd -P)"
if git -C "$OUT" rev-parse --show-toplevel >/dev/null 2>&1; then
  die "$OUT is inside a git working tree: the wheels and bundles go outside every checkout"
fi
[ "$(pwd -P)" = "$HOME" ] || die "not in \$HOME"

# Cargo's [workspace.package] version is the source of every other (scripts/release.py).
HEAD_VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' "$CLONE/Cargo.toml" | head -n 1)"
[ -n "$HEAD_VERSION" ] || die "no [workspace.package] version in $CLONE/Cargo.toml"
if [ -z "$RC_BASE" ]; then
  RC_BASE="$(printf '%s' "$HEAD_VERSION" | awk -F. '{ sub(/-.*/, "", $3); printf "%d.%d.%d", $1, $2, $3 + 1 }')"
fi
[[ "$RC_BASE" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || usage_die "--rc-base: X.Y.Z ($RC_BASE)"
pep440() { printf '%s' "$1" | sed -e 's/-alpha\./a/' -e 's/-beta\./b/' -e 's/-rc\./rc/'; }
say "staging_wheels: the cutover's release is $( [ -n "$CUTOVER_FROM" ] && printf 'from %s' "$CUTOVER_FROM" || printf "CLONE's %s" "$HEAD_VERSION"); the planted ones $RC_BASE-rc.1 to -rc.3"
mkdir -p "$TARGET/manylinux" "$TARGET/maturin-registry" "$OUT/wheels" "$OUT/bundles"

# The wheel of source tree $1 into folder $2, as package.yml's Linux x64 leg builds it (manylinux_2_28, --locked,
# --release), labelled $3.
build_wheel() {
  local src="$1" into="$2" label="$3" t0
  rm -rf "$src/dist"
  t0="$(date +%s)"
  podman run --rm -v "$src":/io -v "$TARGET/manylinux":/target -v "$TARGET/maturin-registry":/root/.cargo/registry \
    -e CARGO_TARGET_DIR=/target -e CITAR_BUILD_ID="$label" "$IMAGE" \
    build --release --locked --compatibility manylinux_2_28 --out /io/dist
  mkdir -p "$into"
  cp "$src"/dist/citar-*-manylinux_2_28_x86_64.whl "$into/"
  say "staging_wheels: $(basename "$into"/citar-*.whl) in $(( $(date +%s) - t0 )) s"
}

# The [server] extra's tested set for the wheel in folder $1 (scripts/ci/smoke_wheel.py constraints, as package.yml's
# constraints job writes it).
constraints() {
  local into="$1" p="$2" work
  work="$(mktemp -d "$OUT/constraints.XXXXXX")"
  "$PY" "$CLONE/scripts/ci/smoke_wheel.py" constraints --wheel "$into" --python "$PY" --work "$work" \
    --out "$into/citar-$p-server-constraints.txt" > "$work.log" 2>&1 || { cat "$work.log"; die "constraints failed"; }
  rm -rf "$work" "$work.log"
  say "staging_wheels: citar-$p-server-constraints.txt ($(grep -c '==' "$into/citar-$p-server-constraints.txt") pins)"
}

bundle() {   # $1 the version, $2 the folder with its wheel and constraints
  local version="$1" from="$2"
  rm -rf "$OUT/bundles/citar-deploy-$version" "$OUT/bundles/citar-deploy-$version.tar.gz"*
  "$PY" "$CLONE/scripts/deploy/bundle.py" "$version" --from-dir "$from" --ref HEAD --out "$OUT/bundles"
  tar -xzf "$OUT/bundles/citar-deploy-$version.tar.gz" -C "$OUT/bundles"
  (cd "$OUT/bundles/citar-deploy-$version" && sha256sum -c --quiet SHA256SUMS) || die "the bundle of $version does not check"
}

# A migration added to scratch clone $1: revision $2, after the clone's head, with nothing in it.
add_migration() {
  "$PY" -I - "$1" "$2" <<'PY'
import re
import sys
from pathlib import Path

root, rev = Path(sys.argv[1]), sys.argv[2]
folder = root / "citar" / "migrations" / "versions"
revs, downs = set(), set()
for f in folder.glob("*.py"):
    text = f.read_text(encoding="utf-8")
    m = re.search(r'^revision(?:: *str)? *= *["\']([^"\']+)["\']', text, re.M)
    d = re.search(r'^down_revision(?:: *[^=]+)? *= *["\']([^"\']+)["\']', text, re.M)
    if m:
        revs.add(m[1])
    if d:
        downs.add(d[1])
heads = sorted(revs - downs)
if len(heads) != 1:
    sys.exit(f"not one head in {folder}: {heads}")
(folder / f"{rev}.py").write_text(f'''"""The staging run's empty migration (scripts/deploy/staging_wheels.sh): a release that moves the revision."""
from typing import Sequence, Union

revision: str = "{rev}"
down_revision: Union[str, None] = "{heads[0]}"
branch_labels: Union[str, Sequence[str], None] = None
depends_on: Union[str, Sequence[str], None] = None


def upgrade() -> None:
    pass


def downgrade() -> None:
    pass
''', encoding="utf-8")
print(f"migration {rev} after {heads[0]}")
PY
}

# Plants a server that exits after (or before) it migrates: citar/server/boot.py's startup() of scratch clone $1.
plant() {   # $1 the clone, $2 after|before
  "$PY" -I - "$1" "$2" <<'PY'
import sys
from pathlib import Path

boot = Path(sys.argv[1]) / "citar" / "server" / "boot.py"
text = boot.read_text(encoding="utf-8")
anchor = "    notice = migrate()\n"
if text.count(anchor) != 1:
    sys.exit(f"{boot}: startup()'s migrate() call is not where the plant expects it")
raise_line = ('    raise BootError("staging plant: this release\'s server exits {} it migrates '
              '(scripts/deploy/staging_wheels.sh)")\n').format(sys.argv[2])
text = text.replace(anchor, anchor + raise_line if sys.argv[2] == "after" else raise_line + anchor)
boot.write_text(text, encoding="utf-8")
print(f"planted: the server exits {sys.argv[2]} it migrates")
PY
}

# Scratch clone $1 of CLONE at version $2 with migrations $3 (space-separated) and plant $4 (after, before or none):
# its wheel, constraints and bundle.
planted_release() {
  local n="$1" version="$2" migrations="$3" how="$4" src p rev sha
  src="$OUT/clone-rc$n"
  rm -rf "$src"
  git clone --quiet --no-hardlinks "$CLONE" "$src"
  [ "$(git -C "$src" rev-parse --show-toplevel)" = "$src" ] || die "$src is not its own clone"
  "$PY" "$src/scripts/release.py" bump "$version" --root "$src" --date "$(date +%F)" > /dev/null
  for rev in $migrations; do add_migration "$src" "$rev"; done
  [ "$how" = none ] || plant "$src" "$how"
  git -C "$src" add -A
  git -C "$src" -c user.name="staging" -c user.email="staging@localhost" commit --quiet \
    -m "staging: $version (scripts/deploy/staging_wheels.sh)"
  sha="$(git -C "$src" rev-parse HEAD)"
  p="$(pep440 "$version")"
  build_wheel "$src" "$OUT/wheels/$p" "ci+${sha:0:7}"
  constraints "$OUT/wheels/$p" "$p"
  bundle "$version" "$OUT/wheels/$p"
  rm -rf "$src"
}

# A wheel folder's version as its tag names it (bundle.py's V): 0.1.6rc1 -> 0.1.6-rc.1.
tagged() { printf '%s' "$1" | sed -e 's/rc\([0-9]*\)$/-rc.\1/' -e 's/a\([0-9]*\)$/-alpha.\1/' -e 's/b\([0-9]*\)$/-beta.\1/'; }

if [ "$BUNDLES_ONLY" -eq 1 ]; then
  for from in "$OUT"/wheels/*/; do
    from="${from%/}"
    bundle "$(tagged "${from##*/}")" "$from"
  done
  say "staging_wheels: every bundle made again from $OUT/wheels"
  exit 0
fi

if [ -n "$CUTOVER_FROM" ]; then
  CUT_VERSION="$(basename "$(ls "$CUTOVER_FROM"/citar-*-cp311-abi3-manylinux_2_28_x86_64.whl)" | sed 's/^citar-\([^-]*\)-.*/\1/')"
  bundle "$CUT_VERSION" "$CUTOVER_FROM"
else
  CUT_VERSION="$HEAD_VERSION"
  p="$(pep440 "$HEAD_VERSION")"
  build_wheel "$CLONE" "$OUT/wheels/$p" "ci+$(git -C "$CLONE" rev-parse --short=7 HEAD)"
  rm -rf "$CLONE/dist"
  constraints "$OUT/wheels/$p" "$p"
  bundle "$HEAD_VERSION" "$OUT/wheels/$p"
fi
M1="9000a_staging_upgrade"
M2="9000b_staging_failure"
planted_release 1 "$RC_BASE-rc.1" "$M1" none
planted_release 2 "$RC_BASE-rc.2" "$M1 $M2" after
planted_release 3 "$RC_BASE-rc.3" "$M1 $M2" before
say ""
say "staging_wheels: the bundles, for staging.sh run:"
say "  --bundle $OUT/bundles/citar-deploy-$CUT_VERSION"
say "  --rc1 $OUT/bundles/citar-deploy-$RC_BASE-rc.1 --rc2 $OUT/bundles/citar-deploy-$RC_BASE-rc.2 --rc3 $OUT/bundles/citar-deploy-$RC_BASE-rc.3"
