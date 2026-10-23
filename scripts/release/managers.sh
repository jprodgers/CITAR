#!/usr/bin/env bash
#
# A release's Homebrew tap and Scoop bucket, updated from its manifests (design/phase5-release.md P5.15 step 8;
# packaging/RELEASING.md).
#
#   scripts/release/managers.sh V [--plan | --push] [--manifests DIR] [--tap URL] [--bucket URL] [--keep]
#
# It clones the tap (jprodgers/homebrew-citar) and the bucket (jprodgers/scoop-citar) into a temporary folder of its
# own, copies the release's manifests in, and shows the diff:
#
#   the tap      Formula/citar-helper.rb added (or replaced), Formula/citar.rb removed (0.1.5's game formula: the
#                game installs from its wheels now, packaging/README.md "Homebrew")
#   the bucket   bucket/citar.json and bucket/citar-helper.json
#
# Nothing leaves the temporary folder unless --push is given: then each repository with a change gets one commit
# ("CITAR V") and is pushed to its own origin, with this machine's git credentials. --plan (the default) shows the
# diff and pushes nothing. winget is not a repository of ours: its manifests go to Microsoft by wingetcreate, whose
# commands it prints at the end.
#
# The manifests are packaging/'s layout (homebrew/citar-helper.rb, scoop/citar.json, scoop/citar-helper.json): by
# default packaging/ itself, where `python scripts/release_checksums.py V --write` writes them from the published
# release; or --manifests DIR, the managers/ folder of package.yml's pkg-release-preview artifact. Each must be V's
# (its URLs and version name V) and hold no template placeholder.
#
#   --plan          show the diff, push nothing (the default)
#   --push          commit and push each repository that changed
#   --manifests DIR where the manifests are (default: packaging/ of this checkout)
#   --tap URL       the tap to clone (default: https://github.com/jprodgers/homebrew-citar.git)
#   --bucket URL    the bucket to clone (default: https://github.com/jprodgers/scoop-citar.git)
#   --keep          keep the temporary folder (its path is printed)
#
# Exit status: 0 done, 1 something did not hold (each named), 2 a usage error.
set -euo pipefail

TAP_URL=https://github.com/jprodgers/homebrew-citar.git
BUCKET_URL=https://github.com/jprodgers/scoop-citar.git

say() { printf 'managers.sh: %s\n' "$*"; }
fail() { printf 'managers.sh: %s\n' "$*" >&2; exit 1; }
usage() { printf 'managers.sh: %s\n' "$*" >&2; printf 'usage: managers.sh V [--plan | --push] [--manifests DIR] [--tap URL] [--bucket URL] [--keep]\n' >&2; exit 2; }

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
root=$(cd "$here/../.." && pwd -P)

version=
mode=
manifests="$root/packaging"
keep=
while [ $# -gt 0 ]; do
  case "$1" in
    --plan|--push)
      [ -z "$mode" ] || [ "$mode" = "${1#--}" ] || usage "--plan and --push: one or the other"
      mode=${1#--} ;;
    --manifests|--tap|--bucket)
      [ $# -ge 2 ] && [ -n "$2" ] || usage "$1 needs a value"
      case "$1" in
        --manifests) manifests=$2 ;;
        --tap) TAP_URL=$2 ;;
        --bucket) BUCKET_URL=$2 ;;
      esac
      shift ;;
    --keep) keep=1 ;;
    -h|--help) sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
    -*) usage "unknown option $1" ;;
    *)
      [ -z "$version" ] || usage "one version, not $version and $1"
      version=$1 ;;
  esac
  shift
done
mode=${mode:-plan}
[ -n "$version" ] || usage "which version? (managers.sh 0.1.6)"
if ! [[ "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-(alpha|beta|rc)\.(0|[1-9][0-9]*))?$ ]]; then
  usage "$version is not a release version (X.Y.Z or X.Y.Z-rc.N)"
fi
[ -d "$manifests" ] || usage "--manifests $manifests: no such folder"
manifests=$(cd "$manifests" && pwd -P)

# ---- the manifests: present, V's, and filled in
formula="$manifests/homebrew/citar-helper.rb"
scoop_game="$manifests/scoop/citar.json"
scoop_helper="$manifests/scoop/citar-helper.json"
problems=0
for file in "$formula" "$scoop_game" "$scoop_helper"; do
  if [ ! -f "$file" ]; then
    printf 'managers.sh: %s: missing (python scripts/release_checksums.py %s --write writes it)\n' "$file" "$version" >&2
    problems=$((problems + 1))
    continue
  fi
  if grep -n '{{' "$file" >/dev/null; then
    printf 'managers.sh: %s: a template placeholder is left in it\n' "$file" >&2
    problems=$((problems + 1))
  fi
done
if [ -f "$formula" ] && ! grep -F "/releases/download/v$version/citar-helper-$version-" "$formula" >/dev/null; then
  printf 'managers.sh: %s: its URLs are not %s'"'"'s release (/releases/download/v%s/citar-helper-%s-...)\n' \
    "$formula" "$version" "$version" "$version" >&2
  problems=$((problems + 1))
fi
for file in "$scoop_game" "$scoop_helper"; do
  if [ -f "$file" ] && ! grep -E "\"version\": *\"$version\"" "$file" >/dev/null; then
    printf 'managers.sh: %s: its "version" is not %s\n' "$file" "$version" >&2
    problems=$((problems + 1))
  fi
done
[ "$problems" -eq 0 ] || fail "$problems problem(s) with the manifests in $manifests: nothing was cloned"

# ---- the clones, in a folder of this run's
work=$(mktemp -d "${TMPDIR:-/tmp}/citar-managers-XXXXXX")
cleanup() {
  if [ -n "$keep" ]; then say "kept $work"; else rm -rf "$work"; fi
}
trap cleanup EXIT
say "$version: $mode, the manifests from $manifests, the clones in $work"

# The diff a person reads: whole, uncoloured, never paged.
show() {
  git -C "$1" --no-pager diff --cached --stat
  git -C "$1" --no-pager diff --cached --no-color
}

update() {  # update NAME URL: clone, then the files the caller's function copies, then the diff (and the push)
  local name=$1 url=$2 copy=$3 dir="$work/$1"
  say "$name: cloning $url"
  git clone --quiet "$url" "$dir" || fail "$name: could not clone $url"
  "$copy" "$dir"
  git -C "$dir" add -A
  if git -C "$dir" diff --cached --quiet; then
    say "$name: already $version's, nothing to change"
    return 0
  fi
  say "$name: the change"
  show "$dir"
  if [ "$mode" = push ]; then
    git -C "$dir" commit --quiet -m "CITAR $version" -m "From the release's manifests (scripts/release/managers.sh in jprodgers/CITAR)."
    git -C "$dir" push --quiet origin HEAD || fail "$name: the push to $url failed (the commit is in $dir; --keep keeps it)"
    say "$name: pushed $(git -C "$dir" rev-parse --short HEAD) to $url"
  else
    say "$name: not pushed (--plan); run again with --push to commit and push this"
  fi
}

copy_tap() {
  mkdir -p "$1/Formula"
  cp "$formula" "$1/Formula/citar-helper.rb"
  if [ -e "$1/Formula/citar.rb" ]; then
    git -C "$1" rm --quiet Formula/citar.rb
  fi
}

copy_bucket() {
  mkdir -p "$1/bucket"
  cp "$scoop_game" "$1/bucket/citar.json"
  cp "$scoop_helper" "$1/bucket/citar-helper.json"
}

update tap "$TAP_URL" copy_tap
update bucket "$BUCKET_URL" copy_bucket

cat <<EOF
managers.sh: winget is Microsoft's repository: validate and submit each package from a Windows machine (packaging/README.md, "winget"):
  winget validate --manifest packaging\\winget\\game
  wingetcreate submit --prtitle "New package: JimmieRodgers.CITAR $version" packaging\\winget\\game
  winget validate --manifest packaging\\winget\\helper
  wingetcreate submit --prtitle "New package: JimmieRodgers.CITAR.Helper $version" packaging\\winget\\helper
EOF
