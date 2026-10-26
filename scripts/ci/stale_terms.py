"""Refuse the phrases of CITAR's retired install and deploy routes in the docs, the units and the manifests
(design/phase5-release.md P5.16, "Checks"; run by test.yml's lint job).

0.1.6 installs from wheels and deploys a release layout with ``citar-deploy``. The routes 0.1.5 documented are gone,
and a page that still gives one of their commands sends its reader somewhere that no longer works:

* the ``git archive`` deploy: the whole tree copied onto the server and run in place from ``/opt/citar`` with its
  ``/opt/citar/.venv`` (``citar-deploy`` installs a release's wheel instead; ``bundle.py``'s ``git archive`` of
  ``deploy/`` is another thing, and is allowed on a line that names ``deploy/``);
* ``requirements.txt`` installs (``pip install -r requirements.txt``) and the script that kept the file in step;
* the game's Homebrew formula (the tap ships only ``citar-helper``; ``brew uninstall citar`` removes 0.1.5's, and
  is allowed);
* ``citar-lab``, 0.1.5's lab service (the lab runs inside ``citar.service``);
* the server started as a module, ``python -m citar.server``, in a unit or shown as the command (``citar serve``),
  and a unit run from the source tree (``WorkingDirectory=/opt/citar``);
* the PyPI sdist as a manifest's download (``files.pythonhosted.org``).

Where it looks (``KINDS``): the Markdown at the checkout's top, under ``docs/`` and under ``packaging/``; the systemd
units under ``deploy/`` and the package's copies under ``citar/wizard/deploy/``; and the package managers'
templates, ``packaging/**/*.in``. Each phrase names the kinds it applies to. ``design/`` (the record of how things
were), ``wiki/`` (generated from ``docs/``, S5) and the crates' own notes are not read.

What is exempt, by section (``EXEMPT``): the CHANGELOG's history (every released version's section) and the
Unreleased section's Compatibility notes, which say what changed for someone coming from 0.1.5; and the few sections
that describe 0.1.5's layout because a tool acts on it (the cutover from 0.1.5, the staging rehearsal that stages
0.1.5). A section is its heading's lines down to the next heading of the same or a higher level; a ``#`` line inside
a fenced code block is no heading.

    python scripts/ci/stale_terms.py [--root DIR] [--list]

Each finding is ``path:line: name: "the text" (what to say instead)``. Exit 0 when nothing is found, 1 when something
is, 2 for a usage error (a root that is not a checkout). Standard library only.
"""
from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path

CHECKOUT = Path(__file__).resolve().parents[2]

DOCS = "docs"
UNITS = "units"
MANIFESTS = "manifests"

#: Where each kind of file is, as globs from the checkout's root.
KINDS: dict[str, tuple[str, ...]] = {
    DOCS: ("*.md", "docs/**/*.md", "packaging/**/*.md"),
    UNITS: ("deploy/*.service", "deploy/*.timer", "citar/wizard/deploy/*.service", "citar/wizard/deploy/*.timer"),
    MANIFESTS: ("packaging/**/*.in",),
}


@dataclass(frozen=True)
class Phrase:
    name: str
    pattern: re.Pattern
    kinds: tuple[str, ...]
    instead: str
    #: A line the phrase must find, for the tests' plants (tests/test_stale_terms.py).
    sample: str


def _phrase(name: str, pattern: str, kinds: tuple[str, ...], instead: str, sample: str) -> Phrase:
    return Phrase(name, re.compile(pattern), kinds, instead, sample)


PHRASES: tuple[Phrase, ...] = (
    _phrase("git-archive-deploy", r"\bgit archive\b(?!.*\bdeploy/)", (DOCS,),
            "a server installs a release's wheel with citar-deploy (docs/server/DEPLOY.md); bundle.py's "
            "`git archive` of deploy/ names deploy/ on its line",
            "git archive --format=tar HEAD | gzip -9 > /tmp/citar.tar.gz"),
    _phrase("source-tree-venv", r"/opt/citar/\.venv\b", (DOCS, UNITS),
            "a release's venv is /opt/citar/releases/<version>/venv, run through /opt/citar/current",
            "sudo -u citar /opt/citar/.venv/bin/pip install citar"),
    _phrase("requirements-install", r"(?<![\w-])-r\s+(?:\S*/)?requirements\.txt\b", (DOCS,),
            "requirements.txt is gone: pip installs the wheel with the release's constraints file",
            "pip install --no-cache-dir -r /opt/citar/requirements.txt"),
    _phrase("requirements-sync", r"\bsync_requirements\b", (DOCS,),
            "scripts/sync_requirements.py went with requirements.txt",
            "python scripts/sync_requirements.py --check"),
    _phrase("homebrew-game", r"\bbrew\s+(?:install|upgrade|reinstall|info)\s+(?:--?\S+\s+)*(?:jprodgers/citar/)?citar(?![\w-])",
            (DOCS, MANIFESTS),
            "the tap ships only the helper (brew install jprodgers/citar/citar-helper); the game installs with "
            "pipx or install.sh, and `brew uninstall citar` removes 0.1.5's formula",
            "brew install jprodgers/citar/citar"),
    _phrase("homebrew-game-formula", r"\bpackaging/homebrew/citar\.rb\b", (DOCS,),
            "the game's formula is gone; packaging/homebrew/citar-helper.rb.in is the tap's one formula",
            "the formula is packaging/homebrew/citar.rb"),
    _phrase("citar-lab", r"\bcitar-lab\b", (DOCS,),
            "the lab runs inside citar.service (0.1.6 has no citar-lab); only the cutover from 0.1.5 names it",
            "sudo systemctl restart citar citar-lab"),
    _phrase("server-module", r"-m\s+citar\.server(?![.\w])", (DOCS, UNITS),
            "the server is `citar serve` (a unit: ExecStart=/opt/citar/current/venv/bin/citar serve)",
            "ExecStart=/opt/citar/.venv/bin/python -m citar.server"),
    _phrase("unit-in-source-tree", r"^\s*WorkingDirectory=/opt/citar/?\s*$", (DOCS, UNITS),
            "a unit's working directory is the state directory, /var/lib/citar",
            "WorkingDirectory=/opt/citar"),
    _phrase("pypi-sdist", r"\bfiles\.pythonhosted\.org\b|\bpypi\.(?:org|io)/packages/", (DOCS, MANIFESTS),
            "a manifest installs the release's own files (packaging/README.md); the sdist builds the engine with Rust",
            'url "https://files.pythonhosted.org/packages/source/c/citar/citar-0.1.5.tar.gz"'),
)


@dataclass(frozen=True)
class Exempt:
    path: str
    #: The section's heading text (without its #s), matched whole.
    heading: str
    why: str


#: Sections that describe 0.1.5's layout because a tool acts on it. The CHANGELOG's history and Compatibility notes
#: are exempt by changelog_exempt() below.
EXEMPT: tuple[Exempt, ...] = (
    Exempt("docs/server/DEPLOY.md", "Moving from 0.1.5",
           "the cutover from 0.1.5 stops and removes its citar-lab and archives its source tree"),
    Exempt("docs/server/RUNBOOK.md", "The cutover from 0.1.5, and undoing it",
           "citar-deploy cutover and uncutover act on 0.1.5's layout, citar-lab included"),
    Exempt("docs/server/RUNBOOK.md", "The staging rehearsal",
           "the rehearsal stages 0.1.5 as the box has it, citar-lab and its source tree included"),
)

FENCE = re.compile(r"^\s{0,3}(```|~~~)")
HEADING = re.compile(r"^(#{1,6})\s+(.*?)\s*#*\s*$")
RELEASED = re.compile(r"^\[\d+\.\d+\.\d+[^\]]*\]")


@dataclass(frozen=True)
class Finding:
    path: str
    line: int
    phrase: Phrase
    text: str

    def __str__(self) -> str:
        return f'{self.path}:{self.line}: {self.phrase.name}: "{self.text}" ({self.phrase.instead})'


def sections(lines: list[str]) -> list[tuple[str, ...]]:
    """For each line, the headings it is under, outermost first (its own heading's line included).

    A ``#`` line inside a fenced code block (a shell comment) is no heading.
    """
    stack: list[tuple[int, str]] = []
    out: list[tuple[str, ...]] = []
    fence = ""
    for line in lines:
        match = FENCE.match(line)
        if match:
            if not fence:
                fence = match.group(1)
            elif match.group(1) == fence:
                fence = ""
        elif not fence:
            heading = HEADING.match(line)
            if heading:
                level = len(heading.group(1))
                while stack and stack[-1][0] >= level:
                    stack.pop()
                stack.append((level, heading.group(2)))
        out.append(tuple(text for _, text in stack))
    return out


def changelog_exempt(path: str, headings: tuple[str, ...]) -> bool:
    """The CHANGELOG's history (a released version's section) and its Unreleased section's Compatibility notes."""
    if path != "CHANGELOG.md" or not headings:
        return False
    if len(headings) >= 2 and RELEASED.match(headings[1]):
        return True
    return len(headings) >= 3 and headings[1] == "[Unreleased]" and headings[2] == "Compatibility"


def exempt(path: str, headings: tuple[str, ...]) -> bool:
    if changelog_exempt(path, headings):
        return True
    return any(entry.path == path and entry.heading in headings for entry in EXEMPT)


def files(root: Path) -> list[tuple[str, str]]:
    """Every (relative path, kind) the scan reads, in a stable order."""
    seen: dict[str, str] = {}
    for kind, globs in KINDS.items():
        for pattern in globs:
            for path in sorted(root.glob(pattern)):
                if path.is_file():
                    seen.setdefault(path.relative_to(root).as_posix(), kind)
    return sorted(seen.items())


def scan_text(path: str, kind: str, text: str) -> list[Finding]:
    lines = text.splitlines()
    under = sections(lines) if kind == DOCS else [()] * len(lines)
    found: list[Finding] = []
    for number, (line, headings) in enumerate(zip(lines, under), start=1):
        if kind == DOCS and exempt(path, headings):
            continue
        for phrase in PHRASES:
            if kind not in phrase.kinds:
                continue
            match = phrase.pattern.search(line)
            if match:
                found.append(Finding(path, number, phrase, line.strip()))
    return found


def scan(root: Path) -> list[Finding]:
    found: list[Finding] = []
    for path, kind in files(root):
        found.extend(scan_text(path, kind, (root / path).read_text(encoding="utf-8")))
    return found


def missing_sections(root: Path) -> list[str]:
    """Each exempt section whose file or heading is gone: an exemption that no longer names anything is a mistake."""
    problems = []
    for entry in EXEMPT:
        target = root / entry.path
        if not target.is_file():
            problems.append(f"{entry.path}: the exempt section's file is gone ({entry.heading!r})")
            continue
        lines = target.read_text(encoding="utf-8").splitlines()
        if not any(headings and headings[-1] == entry.heading and HEADING.match(line)
                   for line, headings in zip(lines, sections(lines))):
            problems.append(f"{entry.path}: no heading {entry.heading!r}, which stale_terms.EXEMPT names")
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0], allow_abbrev=False)
    parser.add_argument("--root", type=Path, default=CHECKOUT, help="the checkout to scan (default: this one)")
    parser.add_argument("--list", action="store_true", help="print each phrase and where it applies, then exit")
    args = parser.parse_args(argv)
    if args.list:
        for phrase in PHRASES:
            print(f"{phrase.name} ({', '.join(phrase.kinds)}): {phrase.pattern.pattern}")
        for entry in EXEMPT:
            print(f"exempt: {entry.path} '{entry.heading}': {entry.why}")
        return 0
    root = args.root.resolve()
    if not (root / "CHANGELOG.md").is_file() or not (root / "docs").is_dir():
        print(f"stale_terms: {root} is not a CITAR checkout (no CHANGELOG.md and docs/)", file=sys.stderr)
        return 2
    problems = missing_sections(root)
    findings = scan(root)
    for problem in problems:
        print(f"stale_terms: {problem}")
    for finding in findings:
        print(finding)
    count = len(files(root))
    if problems or findings:
        print(f"stale_terms: {len(findings)} retired phrase(s) and {len(problems)} exemption problem(s) "
              f"in {count} files", file=sys.stderr)
        return 1
    print(f"stale_terms: {count} files, no retired route named")
    return 0


if __name__ == "__main__":
    sys.exit(main())
