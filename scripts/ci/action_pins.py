"""Pin every action a workflow uses to a commit, and prove each pin (design/phase5-release.md P5.17, rule 11).

A tag such as ``actions/checkout@v7`` moves: whoever controls the action's repository can point it at other code,
which then runs in a job of ours, beside its token and, in the release, beside PyPI's and the registry's. So every
remote ``uses:`` names a commit, with the version it was in a comment that Dependabot's ``github-actions`` updates
read and move (``.github/dependabot.yml``)::

    - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1

``scripts/ci/check_workflows.py`` holds that form on every pull request, offline. This tool makes it and proves it
against GitHub, read-only (``gh api`` GETs, through the ``gh`` the machine is signed in with):

    python scripts/ci/action_pins.py list               # every remote uses:, with its line and comment
    python scripts/ci/action_pins.py pin [--write]      # resolve each tag or branch to its commit
    python scripts/ci/action_pins.py verify [--out F]   # each pin: a commit of that repository, its tag on it

``pin`` resolves a reference that is not yet a commit: a tag (an annotated one dereferenced to its commit) or else a
branch, and writes the commit with the most precise version tag that points at it (``v7`` becomes the commit of
``v7`` with ``# v7.0.1``, the full release ``v7`` stands for). Without ``--write`` it prints what it would change.
``verify`` asks GitHub, for every pinned reference, that the SHA is a commit of the named repository and that the
commented tag, dereferenced, is that commit; a commit only a fork holds fails the second test, since a repository's
tags are its own. Exit status 0 when every reference is pinned (``pin``) or proven (``verify``), 1 when not, 2 for a
usage error or when ``gh`` fails.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = Path(".github") / "workflows"

#: A ``uses:`` line: the key (in a list item or not), the reference (quoted or not) and its comment.
USES = re.compile(r"""^(?P<lead>\s*(?:-\s+)?uses:\s*)(?P<q>["']?)(?P<ref>[^\s"'#]+)(?P=q)(?P<rest>\s*(?:#.*)?)$""")
SHA = re.compile(r"[0-9a-f]{40}")
#: A version a comment names: ``v7``, ``v7.0.1``, ``1.15.0``, ``v2.62.47-rc.1``.
VERSION = re.compile(r"v?\d+(?:\.\d+)*(?:[-+][0-9A-Za-z.-]+)?")


class Failed(Exception):
    """``gh`` failed, or GitHub does not know what was asked."""


@dataclass
class Use:
    """One remote ``uses:`` line."""

    path: Path
    line: int  # 1-based
    ref: str  # owner/repo[/path]@reference
    comment: str  # the comment's text, without its '#'

    @property
    def repo(self) -> str:
        return "/".join(self.ref.split("@", 1)[0].split("/")[:2])

    @property
    def target(self) -> str:
        return self.ref.split("@", 1)[1] if "@" in self.ref else ""

    @property
    def pinned(self) -> bool:
        return bool(SHA.fullmatch(self.target))

    @property
    def version(self) -> str | None:
        words = self.comment.split()
        return words[0] if words and VERSION.fullmatch(words[0]) else None

    def where(self, root: Path) -> str:
        try:
            shown = self.path.relative_to(root).as_posix()
        except ValueError:
            shown = self.path.as_posix()
        return f"{shown}:{self.line}"


def workflow_files(root: Path) -> list[Path]:
    folder = root / WORKFLOWS
    return sorted([*folder.glob("*.yml"), *folder.glob("*.yaml")])


def remote(ref: str) -> bool:
    """A reference to another repository's action or workflow; ``./`` ones are this checkout's, ``docker://`` an
    image (check_workflows.py holds those to a digest)."""
    return not ref.startswith("./") and not ref.startswith("docker://")


def uses_in(path: Path) -> list[Use]:
    out = []
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if line.lstrip().startswith("#"):
            continue
        match = USES.match(line)
        if match and remote(match["ref"]):
            comment = match["rest"].strip()
            out.append(Use(path, number, match["ref"], comment[1:].strip() if comment.startswith("#") else ""))
    return out


def all_uses(root: Path) -> list[Use]:
    return [use for path in workflow_files(root) for use in uses_in(path)]


# ------------------------------------------------------------------------------------------------ GitHub, read-only


def gh_get(endpoint: str, missing_ok: bool = False):
    """``gh api ENDPOINT`` (a GET), parsed; None for a 404 when ``missing_ok``."""
    proc = subprocess.run(["gh", "api", "-X", "GET", endpoint], capture_output=True, text=True, encoding="utf-8")
    if proc.returncode != 0:
        if missing_ok and ("404" in proc.stderr or "Not Found" in proc.stdout):
            return None
        raise Failed(f"gh api {endpoint}: {proc.stderr.strip() or proc.stdout.strip()}")
    return json.loads(proc.stdout)


def commit_of(repo: str, obj: dict, get=gh_get) -> str:
    """The commit a ref's object names: an annotated tag is followed to what it tags, to any depth."""
    seen = 0
    while obj["type"] == "tag":
        seen += 1
        if seen > 10:
            raise Failed(f"{repo}: a tag chain longer than ten")
        obj = get(f"repos/{repo}/git/tags/{obj['sha']}")["object"]
    if obj["type"] != "commit":
        raise Failed(f"{repo}: the reference names a {obj['type']}, not a commit")
    return obj["sha"]


def resolve(repo: str, name: str, get=gh_get) -> tuple[str, str]:
    """(commit, kind) for a tag or, failing that, a branch of ``repo``."""
    for kind in ("tags", "heads"):
        ref = get(f"repos/{repo}/git/ref/{kind}/{name}", missing_ok=True)
        if ref is not None and isinstance(ref, dict) and "object" in ref:
            return commit_of(repo, ref["object"], get), kind[:-1] if kind == "tags" else "branch"
    raise Failed(f"{repo} has no tag or branch {name!r}")


def version_key(name: str) -> tuple:
    """Sorts versions by their numbers, a release after its prereleases, the more precise after the less."""
    match = re.fullmatch(r"v?(\d+(?:\.\d+)*)(?:-(.*))?", name)
    if not match:
        return ((), 0, name)
    numbers = tuple(int(n) for n in match[1].split("."))
    return (numbers, 0 if match[2] else 1, len(numbers), name)


def best_version(repo: str, name: str, commit: str, get=gh_get) -> str:
    """The most precise version tag on ``commit``: for ``v7``, the newest ``v7.x.y`` there; ``name`` itself when it
    is already as precise as the repository's tags are, or when no other tag names that commit."""
    stem = name if re.fullmatch(r"v?\d+(?:\.\d+)*", name) else ""
    prefix = f"{stem}." if stem else "v"
    refs = get(f"repos/{repo}/git/matching-refs/tags/{prefix}") or []
    candidates = []
    for ref in refs:
        tag = ref["ref"].removeprefix("refs/tags/")
        if not VERSION.fullmatch(tag):
            continue
        if commit_of(repo, ref["object"], get) == commit:
            candidates.append(tag)
    if VERSION.fullmatch(name):
        candidates.append(name)
    if not candidates:
        raise Failed(f"{repo}: no version tag names the commit {commit} of {name!r}")
    return max(candidates, key=version_key)


# ------------------------------------------------------------------------------------------------ commands


def pin(root: Path, write: bool, out=print, get=gh_get) -> int:
    changes: dict[Path, dict[int, str]] = {}
    cache: dict[tuple[str, str], tuple[str, str]] = {}
    for use in all_uses(root):
        if use.pinned:
            continue
        key = (use.repo, use.target)
        if key not in cache:
            commit, kind = resolve(use.repo, use.target, get)
            cache[key] = (commit, best_version(use.repo, use.target, commit, get))
            out(f"{use.repo}@{use.target} ({kind}) -> {commit} # {cache[key][1]}")
        commit, version = cache[key]
        path_part = use.ref.split("@", 1)[0]
        lines = changes.setdefault(use.path, {})
        lines[use.line] = f"{path_part}@{commit} # {version}"
    for path, lines in changes.items():
        text = path.read_text(encoding="utf-8")
        newline = "\r\n" if "\r\n" in text else "\n"
        rows = text.splitlines()
        for number, value in lines.items():
            match = USES.match(rows[number - 1])
            rows[number - 1] = f"{match['lead']}{value}"
            out(f"{Use(path, number, value, '').where(root)}: {rows[number - 1].strip()}")
        if write:
            path.write_text(newline.join(rows) + newline, encoding="utf-8", newline="")
    total = sum(len(lines) for lines in changes.values())
    out(f"action_pins pin: {total} reference(s) {'pinned' if write else 'to pin (run again with --write)'}")
    return 0 if write or total == 0 else 1


def verify(root: Path, out=print, get=gh_get) -> tuple[int, list[dict]]:
    rows = []
    problems = 0
    proven: dict[tuple[str, str, str | None], str | None] = {}
    for use in all_uses(root):
        row = {"where": use.where(root), "ref": use.ref, "comment": use.comment, "ok": False, "why": ""}
        rows.append(row)
        if not use.pinned:
            row["why"] = "not pinned to a commit"
        elif use.version is None:
            row["why"] = "no version in its comment"
        else:
            key = (use.repo, use.target, use.version)
            if key not in proven:
                proven[key] = prove(use.repo, use.target, use.version, get)
            row["why"] = proven[key] or ""
            row["ok"] = proven[key] is None
        problems += not row["ok"]
        out(f"{'ok  ' if row['ok'] else 'FAIL'} {row['where']}: {use.ref} # {use.comment}"
            + (f"  ({row['why']})" if row["why"] else ""))
    repos = len({u.repo for u in all_uses(root)})
    out(f"action_pins verify: {len(rows)} reference(s) to {len(proven)} pin(s) in {repos} repositories, "
        f"{problems} problem(s)")
    return (1 if problems else 0), rows


def prove(repo: str, sha: str, version: str, get=gh_get) -> str | None:
    """None when ``sha`` is a commit of ``repo`` and its tag ``version`` is that commit; else why not."""
    commit = get(f"repos/{repo}/commits/{sha}", missing_ok=True)
    if commit is None or commit.get("sha") != sha:
        return f"{sha} is not a commit of {repo}"
    ref = get(f"repos/{repo}/git/ref/tags/{version}", missing_ok=True)
    if ref is None or "object" not in ref:
        return f"{repo} has no tag {version}"
    tagged = commit_of(repo, ref["object"], get)
    if tagged != sha:
        return f"{repo}'s tag {version} is the commit {tagged}, not {sha}"
    return None


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=ROOT, help="the checkout (default: this script's)")
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("list", help="every remote uses:, with its line and comment")
    p = sub.add_parser("pin", help="resolve each tag or branch reference to its commit")
    p.add_argument("--write", action="store_true", help="rewrite the workflows (otherwise only print)")
    p = sub.add_parser("verify", help="prove each pin against GitHub, read-only")
    p.add_argument("--out", type=Path, help="also write the rows as JSON to this file")
    args = parser.parse_args(argv)
    root = args.root.resolve()
    try:
        if args.command == "list":
            for use in all_uses(root):
                print(f"{use.where(root)}: {use.ref}" + (f" # {use.comment}" if use.comment else ""))
            return 0
        if args.command == "pin":
            return pin(root, args.write)
        status, rows = verify(root)
        if args.out:
            args.out.write_text(json.dumps(rows, indent=2) + "\n", encoding="utf-8")
        return status
    except Failed as e:
        print(f"action_pins: {e}", file=sys.stderr)
        return 2
    except FileNotFoundError as e:
        print(f"action_pins: {e} (is gh installed?)", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
