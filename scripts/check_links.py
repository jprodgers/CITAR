"""Check that every relative link in the documentation points at something that exists.

Broken links are the most common documentation bug and the least likely to be noticed by the
person who wrote them, because they know where the page is. This walks every Markdown file that
ships, resolves every relative link and anchor, and reports the ones that go nowhere.

    python scripts/check_links.py
    python scripts/check_links.py --strict     # non-zero exit if anything is broken

External URLs are listed but not fetched: a link checker that makes network requests fails in CI
for reasons that have nothing to do with the change being tested.
"""
from __future__ import annotations

import re
import sys
import unicodedata
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

#: Markdown that is part of the published documentation. Generated output is skipped, as is
#: anything in the ignored operator-notes directory.
INCLUDE = ["README.md", "CONTRIBUTING.md", "CODE_OF_CONDUCT.md", "SECURITY.md", "CHANGELOG.md",
           "KNOWN_ISSUES.md", "NOTICE.md", "DESIGN.md"]
INCLUDE_DIRS = ["docs", "packaging"]
SKIP_DIRS = {"wiki", "site", "ops", "node_modules", ".git", "saves", "dist", "build"}

LINK = re.compile(r"\[([^\]]*)\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")


def anchors_of(text: str) -> set[str]:
    """Every anchor a Markdown file defines, as GitHub would generate them.

    GitHub's rule: lower-case, punctuation removed, spaces to hyphens. Reimplemented rather than
    guessed at, because the failure it catches — a link to a heading that was later reworded — is
    otherwise invisible until a reader clicks it.
    """
    found = set()
    in_code = False
    for line in text.splitlines():
        if line.lstrip().startswith("```"):
            in_code = not in_code
            continue
        if in_code or not line.startswith("#"):
            continue
        heading = line.lstrip("#").strip()
        slug = unicodedata.normalize("NFKD", heading).lower()
        slug = re.sub(r"[^\w\s-]", "", slug).strip()
        slug = re.sub(r"[\s_]+", "-", slug)
        found.add(slug)
    # Explicit anchors, e.g. <a id="something">
    found.update(re.findall(r'<a\s+(?:id|name)="([^"]+)"', text))
    return found


def files() -> list[Path]:
    out = [ROOT / name for name in INCLUDE if (ROOT / name).exists()]
    for directory in INCLUDE_DIRS:
        base = ROOT / directory
        if not base.is_dir():
            continue
        for path in sorted(base.rglob("*.md")):
            if any(part in SKIP_DIRS for part in path.parts):
                continue
            out.append(path)
    return out


def main() -> int:
    strict = "--strict" in sys.argv
    paths = files()
    anchors = {p: anchors_of(p.read_text(encoding="utf-8")) for p in paths}

    broken: list[str] = []
    external = 0
    checked = 0

    for path in paths:
        text = path.read_text(encoding="utf-8")
        for label, link in LINK.findall(text):
            if link.startswith(("http://", "https://", "mailto:")):
                external += 1
                continue
            checked += 1
            anchor = ""
            target_link = link
            if "#" in link:
                target_link, anchor = link.split("#", 1)

            if not target_link:                     # a link within this file
                if anchor and anchor not in anchors[path]:
                    broken.append(f"{path.relative_to(ROOT)}: [{label}](#{anchor}) - no such heading")
                continue

            target = (path.parent / target_link).resolve()
            if not target.exists():
                broken.append(f"{path.relative_to(ROOT)}: [{label}]({link}) - no such file")
                continue
            if anchor and target.suffix == ".md":
                known = anchors.get(target)
                if known is None:
                    known = anchors_of(target.read_text(encoding="utf-8"))
                    anchors[target] = known
                if anchor not in known:
                    broken.append(
                        f"{path.relative_to(ROOT)}: [{label}]({link}) - no such heading in "
                        f"{target.relative_to(ROOT)}")

    print(f"{len(paths)} files, {checked} internal links, {external} external (not fetched)\n")
    if broken:
        print(f"{len(broken)} broken:\n")
        for item in broken:
            print(f"  {item}")
        return 1 if strict else 0
    print("Every internal link resolves.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
