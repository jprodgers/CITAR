"""Generate the GitHub wiki from docs/.

A GitHub wiki is a separate git repository with a flat namespace: no directories, no `.md` links
between pages, and a sidebar defined by one file. The documentation in `docs/` is none of those
things — it is a tree of files that link to each other with relative paths and is reviewed in pull
requests like any other change.

So the wiki is generated rather than maintained. This script flattens the tree, rewrites every
link, adds a sidebar and a footer, and writes the result to `wiki/`, which the docs workflow pushes.

    python scripts/build_wiki.py

Anything edited directly in the wiki is overwritten by the next run. Every generated page says so
at the bottom, because somebody will try.
"""
from __future__ import annotations

import re
import shutil
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


def main() -> int:
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
    print("\nTo publish by hand:")
    print(f"  git clone {REPO}.wiki.git /tmp/citar-wiki")
    print("  cp wiki/*.md /tmp/citar-wiki/ && cd /tmp/citar-wiki")
    print("  git add -A && git commit -m 'docs: sync' && git push")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
