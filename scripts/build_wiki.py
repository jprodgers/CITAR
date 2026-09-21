"""Generate the GitHub wiki from docs/.

A GitHub wiki is a separate git repository with a flat namespace: no directories, no `.md` links
between pages, and a sidebar defined by one file. The documentation in `docs/` is none of those
things — it is a tree of files that link to each other with relative paths and is reviewed in pull
requests like any other change.

So the wiki is generated rather than maintained. This script flattens the tree, rewrites every
link, adds a sidebar and a footer, and writes the result to `wiki/`, which the docs workflow pushes.

    python scripts/build_wiki.py            # write wiki/ and stop
    python scripts/build_wiki.py --push     # write wiki/ and publish it

`--push` publishes with whatever git credentials the machine already has, which is the whole
point of it: pushing to a wiki from Actions needs a classic personal access token, because
GITHUB_TOKEN cannot write to a wiki and fine-grained tokens have no wiki permission at all. Where
that token does not exist, this is the way the wiki gets updated.

Anything edited directly in the wiki is overwritten by the next run. Every generated page says so
at the bottom, because somebody will try.
"""
from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOCS = ROOT / "docs"
OUT = ROOT / "wiki"
REPO = "https://github.com/jprodgers/CITAR"

#: Documentation source -> wiki page name. Order is the sidebar order.
#: Wiki page names become URLs, so they are written the way a reader would want to see them.
PAGES: list[tuple[str, str, str]] = [
    # (path under docs/ or the repository root, wiki page name, sidebar section)
    ("docs/index.md", "Home", ""),
    ("docs/QUICKSTART.md", "Quick-start", "Playing"),
    ("docs/INSTALL.md", "Installing", "Playing"),
    ("docs/PLAYING.md", "Playing-in-the-browser", "Playing"),
    ("docs/AI_PLAYERS.md", "AI-players", "Playing"),
    ("docs/BENCHMARKS.md", "Benchmarks", "Research"),
    ("docs/SCENARIOS.md", "Scenarios-and-probes", "Research"),
    ("docs/REPORTS.md", "Servers-costs-and-reports", "Research"),
    ("docs/BOTS.md", "Scripted-bots", "Research"),
    ("docs/research/BOT_TUNING.md", "Bot-tuning-log", "Research"),
    ("docs/server/DEPLOY.md", "Deploying-a-server", "Running a server"),
    ("docs/server/VPS.md", "Preparing-a-VPS", "Running a server"),
    ("docs/server/OAUTH.md", "Single-sign-on", "Running a server"),
    ("docs/server/WORKERS.md", "Workers", "Running a server"),
    ("docs/server/RUNBOOK.md", "Runbook", "Running a server"),
    ("docs/server/ACCOUNTS.md", "Design-accounts-and-sharing", "Running a server"),
    ("docs/ARCHITECTURE.md", "Architecture", "Building on it"),
    ("docs/API.md", "HTTP-and-tool-API", "Building on it"),
    ("docs/MODDING.md", "Modding", "Building on it"),
    ("docs/CONFIGURATION.md", "Configuration", "Building on it"),
    ("CONTRIBUTING.md", "Contributing", "Building on it"),
    ("docs/TROUBLESHOOTING.md", "Troubleshooting", "Help"),
    ("docs/FAQ.md", "Questions", "Help"),
    ("KNOWN_ISSUES.md", "Known-issues", "Help"),
    ("CHANGELOG.md", "Changelog", "Help"),
]

#: Source path (as written in a link, resolved from the repository root) -> wiki page.
LINK_MAP = {source: page for source, page, _ in PAGES}

FOOTER = (
    "\n\n---\n\n"
    "*This page is generated from [`{source}`]({repo}/blob/main/{source}) and any edit made here "
    "will be overwritten. Corrections are welcome as a pull request.*\n"
)


def resolve(link: str, source: Path) -> str:
    """Turn a relative Markdown link into a wiki link, or a repository URL when there is no page.

    Links are resolved against the file they appear in, exactly as GitHub resolves them, so
    ``../CONTRIBUTING.md`` from ``docs/FAQ.md`` and ``CONTRIBUTING.md`` from the root are the same
    target and map to the same page.
    """
    anchor = ""
    if "#" in link:
        link, anchor = link.split("#", 1)
        anchor = "#" + anchor
    if not link:
        return anchor or "#"

    target = (source.parent / link).resolve()
    try:
        relative = target.relative_to(ROOT).as_posix()
    except ValueError:
        return link + anchor                    # outside the repository; leave it alone

    if relative in LINK_MAP:
        return LINK_MAP[relative] + anchor
    # Not a wiki page: point at the file in the repository, so the link still works.
    return f"{REPO}/blob/main/{relative}{anchor}"


def convert(source: Path, text: str) -> str:
    """Rewrite every relative link in *text* for the wiki."""
    def replace(match: re.Match) -> str:
        label, link = match.group(1), match.group(2)
        if link.startswith(("http://", "https://", "mailto:", "#")):
            return match.group(0)
        return f"[{label}]({resolve(link, source)})"

    return re.sub(r"\[([^\]]*)\]\(([^)\s]+)\)", replace, text)


def sidebar() -> str:
    lines = ["### CITAR", ""]
    section = None
    for _, page, group in PAGES:
        if group != section:
            section = group
            if group:
                lines += ["", f"**{group}**", ""]
        label = page.replace("-", " ")
        lines.append(f"- [[{label}|{page}]]")
    lines += ["", "---", "", f"[Repository]({REPO}) · [Releases]({REPO}/releases) · "
                            f"[Issues]({REPO}/issues)"]
    return "\n".join(lines) + "\n"


def run(*command: str, cwd: Path) -> str:
    """One git command, with its output, failing loudly."""
    done = subprocess.run(command, cwd=cwd, capture_output=True, text=True)
    if done.returncode != 0:
        raise SystemExit(f"{' '.join(command)} failed:\n{done.stderr.strip() or done.stdout.strip()}")
    return done.stdout.strip()


def push() -> int:
    """Replace the wiki repository's contents with `wiki/` and push it."""
    with tempfile.TemporaryDirectory(prefix="citar-wiki-") as tmp:
        clone = Path(tmp) / "wiki"
        print(f"cloning {REPO}.wiki.git")
        run("git", "clone", "--quiet", f"{REPO}.wiki.git", str(clone), cwd=ROOT)

        # Everything but .git goes, so a page dropped from PAGES disappears from the wiki instead
        # of lingering as a stale copy that nothing links to but search still finds.
        for item in clone.iterdir():
            if item.name != ".git":
                shutil.rmtree(item) if item.is_dir() else item.unlink()
        for page in sorted(OUT.glob("*.md")):
            shutil.copy2(page, clone / page.name)

        run("git", "add", "-A", cwd=clone)
        if not run("git", "status", "--porcelain", cwd=clone):
            print("the wiki is already up to date")
            return 0
        run("git", "commit", "--quiet", "-m", "docs: sync from docs/", cwd=clone)
        run("git", "push", "--quiet", cwd=clone)
    print(f"pushed. {REPO}/wiki")
    return 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description="Generate the GitHub wiki from docs/.")
    parser.add_argument("--push", action="store_true",
                        help="publish wiki/ to the repository's wiki using this machine's git credentials")
    args = parser.parse_args(sys.argv[1:] if argv is None else list(argv))

    if OUT.exists():
        # Rebuilt from scratch: a page removed from PAGES should disappear rather than linger as a
        # stale copy that nothing links to but search still finds.
        for item in OUT.iterdir():
            if item.name != ".git":
                shutil.rmtree(item) if item.is_dir() else item.unlink()
    OUT.mkdir(exist_ok=True)

    written = 0
    for relative, page, _ in PAGES:
        source = ROOT / relative
        if not source.exists():
            print(f"  missing: {relative}")
            continue
        text = convert(source, source.read_text(encoding="utf-8"))
        text += FOOTER.format(source=relative, repo=REPO)
        (OUT / f"{page}.md").write_text(text, encoding="utf-8", newline="\n")
        written += 1

    (OUT / "_Sidebar.md").write_text(sidebar(), encoding="utf-8", newline="\n")
    (OUT / "_Footer.md").write_text(
        f"[CITAR]({REPO}) · MPL-2.0 · rules derived from "
        "[UnCiv](https://github.com/yairm210/Unciv)\n", encoding="utf-8", newline="\n")

    print(f"wrote {written} pages + sidebar and footer to {OUT.relative_to(ROOT)}")
    if args.push:
        return push()
    print("\nTo publish it:  python scripts/build_wiki.py --push")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
